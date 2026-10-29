//! `FfmpegBuffer` — the **view** lane's carrier.
//!
//! A refcounted handle onto an `AVBufferRef` that FFmpeg already owns.
//! Cloning bumps the refcount; dropping releases one reference; the
//! bytes are never copied. This is the carrier 0.8 shipped, resurrected
//! by ruling as the second of two first-class lanes — see
//! [the carrier lanes][lanes] for what each one is for.
//!
//! # What the amputation round taught this type
//!
//! The view lane is not 0.8 restored unchanged. Every lesson from the
//! 0.9 review loop that lands on *this* type has been re-applied:
//!
//! * **The extent is proved before a view exists.** 0.8 formed a view
//!   over a packet's claimed range and let a malformed `size` hand out
//!   a slice nobody had checked. The proof now runs first, in
//!   [`crate::buffer::payload_of`], which both lanes share — so the
//!   view lane cannot regress it independently.
//! * **`AV_PKT_FLAG_TRUSTED` is refused on both legs**, and for a
//!   sharper reason here than on the owned lane: a payload that is a
//!   structure of pointers is uncarriable when copied, and *equally*
//!   uncarriable when viewed. Sharing the allocation does not make its
//!   pointers own what they name.
//! * **Budgets judge sizes, not copies.** A view costs no bytes, but a
//!   ceiling on frame and packet size is a ceiling on what a caller
//!   will be handed and asked to hold — so every limit that fires on
//!   the owned lane fires identically here.
//! * **Every constructor that takes an extent is crate-private**, and
//!   the one that takes row geometry checks it rather than asserting
//!   it. An invariant that lives in the caller is an invariant the
//!   caller has to be inside this crate to be trusted with.
//! * **A shared buffer never reaches a caller as something mutable.**
//!   The zero-copy send exists, but it is scoped to a decoder
//!   submission inside this crate; a packet a caller *holds* owns its
//!   bytes. See [the boundary's `with_ffmpeg_video_packet`][scoped].
//! * **What may be read past a carrier is recorded, not inferred.**
//!   Trailing capacity is not padding — see [`Origin`].
//!
//! [scoped]: crate::boundary
//!
//! [lanes]: mediaspine::adapter#the-two-carrier-lanes

use core::{fmt, slice};

use ffmpeg_next::ffi::{AVBufferRef, av_buffer_ref, av_buffer_unref};

/// Where a carrier's bytes came from, and therefore what may be assumed
/// about the bytes **after** them.
///
/// The send leg needs one specific guarantee: libavcodec reads
/// `AV_INPUT_BUFFER_PADDING_SIZE` bytes past a packet's payload, and
/// libavformat delivers every packet it produces with that much padding
/// of its own writing behind the payload. Trailing *capacity* is not that
/// guarantee — a video plane has more pixels after it, a resampler's
/// output frame has more samples, a caller's packet has whatever its
/// caller left there, and a bitstream reader running off the end of a
/// payload into any of them would eat them as though they were bitstream.
///
/// So provenance is recorded where it is known — at capture, from who
/// delivered the packet (`buffer::PayloadProvenance`) — rather than
/// inferred later from a size comparison that cannot tell padding from a
/// neighbour, or from a buffer's contents that cannot tell padding from
/// bytes nobody wrote.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
  /// A payload captured out of the buffer of a packet **libavformat
  /// delivered** — a demuxed packet, or a stream's parked picture —
  /// whose trailing padding is libavformat's. The only carriers a
  /// decoder may be handed by reference.
  PacketPayload,
  /// Anything else: a frame plane, a resampler's output, a copy this
  /// crate allocated, a caller's packet. Nothing may be assumed about
  /// what follows, and a decoder is handed a padded copy.
  Foreign,
}

/// Refcounted view onto a contiguous byte range inside an
/// `AVBufferRef`.
///
/// Holds one reference to the buffer. The view (offset + length) carves
/// out a sub-region, which is what lets several planes of one
/// allocation — NV12's `data[1] == data[0] + y_size` — each be their
/// own carrier at their own offset, every one bumping the same
/// refcount.
///
/// # Lifetime, stated plainly
///
/// This carrier keeps FFmpeg's buffer alive, which is the point and
/// also the catch: a frame held is a **pool slot held**. A decoder
/// whose pool is exhausted blocks or fails, so a consumer that parks
/// view frames in a queue is a consumer that stalls its own decoder.
/// Read in place, drop, decode on. See the contract for the tradeoff
/// table and for why graph traffic belongs on the owned lane.
pub struct FfmpegBuffer {
  /// The reference this carrier owns, or null for the empty carrier.
  ///
  /// Null is the *only* shape with no buffer behind it, and it exists so
  /// that a placeholder plane slot costs nothing and cannot fail: an
  /// empty carrier that allocated would put an out-of-memory road under
  /// `[Plane; 8]`, which is a lot of failure to buy a zero-length span.
  /// `len == 0` whenever this is null, and every read consults `len`
  /// first.
  inner: *mut AVBufferRef,
  /// Offset from `inner.data` where this view starts.
  offset: usize,
  /// Byte length of this view. Always `<= inner.size - offset`.
  len: usize,
  /// What may be assumed about the bytes after this view. See
  /// [`Origin`].
  origin: Origin,
}

// SAFETY: `AVBufferRef`'s refcount is managed atomically by FFmpeg, and
// `Drop` — the only operation that mutates it — goes through
// `av_buffer_unref`. Moving a carrier between threads is therefore
// sound.
//
// `Sync` is deliberately **not** implemented, and this is a contract
// decision rather than an oversight. The bytes behind the view belong
// to FFmpeg, which is entitled to hand the same allocation to an API
// that writes through it; nothing in this type's contract forbids a
// caller from doing exactly that via `as_av_buffer_ref`. Shared access
// from two threads would then race. The owned lane is `Sync` because
// its bytes are nobody's but ours.
unsafe impl Send for FfmpegBuffer {}

impl FfmpegBuffer {
  /// Takes a view over `len` bytes at `offset` inside `buf`,
  /// incrementing its refcount.
  ///
  /// The caller keeps its own reference and must release it
  /// independently.
  ///
  /// `None` when `buf` is null, when the view would run past the
  /// buffer's `size`, or when `av_buffer_ref` fails.
  ///
  /// **Crate-private on purpose.** Every argument here is an invariant
  /// the unsafe layer trusts, and a caller outside this crate has no
  /// way to establish them. The lanes are reached through the carrier
  /// seam, whose operations are equally unreachable from outside; what
  /// a consumer gets is the finished carrier.
  ///
  /// # Safety
  ///
  /// `buf` must be null or a live `AVBufferRef` for the duration of
  /// this call.
  pub(crate) unsafe fn view_of(
    buf: *mut AVBufferRef,
    offset: usize,
    len: usize,
    origin: Origin,
  ) -> Option<Self> {
    if buf.is_null() {
      return None;
    }
    // SAFETY: `buf` is live per the contract; `size` is a public field.
    let size = unsafe { (*buf).size };
    // The extent proof, kept here as well as at the call sites: this is
    // the constructor, and a constructor that trusts its arguments is
    // one bad caller away from a view over somebody else's memory.
    if offset.checked_add(len)? > size {
      return None;
    }
    // SAFETY: as above; `av_buffer_ref` is atomic and returns null only
    // on allocation failure.
    let new_ref = unsafe { av_buffer_ref(buf) };
    if new_ref.is_null() {
      return None;
    }
    Some(Self {
      inner: new_ref,
      offset,
      len,
      origin,
    })
  }

  /// What may be assumed about the bytes after this view.
  #[inline]
  #[doc(hidden)]
  pub const fn origin(&self) -> Origin {
    self.origin
  }

  /// Allocates a fresh refcounted buffer and copies `bytes` into it.
  ///
  /// **The view lane copies here, and it has to.** Not every byte
  /// FFmpeg hands over lives in an `AVBufferRef`: subtitle rect text,
  /// `AVFrameSideData` payloads and a palette plane are plain
  /// allocations with no refcount to share. A carrier over them has to
  /// own something, so it owns a copy — and the lane stays honest by
  /// saying so rather than pretending the whole road is zero-copy.
  ///
  /// `None` when the allocation fails.
  pub fn copy_from_slice(bytes: &[u8]) -> Option<Self> {
    use ffmpeg_next::ffi::av_buffer_alloc;
    let len = bytes.len();
    if len == 0 {
      // `av_buffer_alloc(0)` is not portable, and there is nothing to
      // hold: the empty carrier is the answer.
      return Some(Self::empty());
    }
    // SAFETY: a plain allocation, checked for null before any write.
    let raw = unsafe { av_buffer_alloc(len as _) };
    if raw.is_null() {
      return None;
    }
    // SAFETY: `raw` is a fresh allocation of `len` bytes and `bytes` is
    // valid for `len` reads; the two cannot overlap.
    unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), (*raw).data, len) };
    Some(Self {
      inner: raw,
      offset: 0,
      len,
      // A copy this crate allocated: exactly `len` bytes, nothing
      // behind them, and so never shareable into a decoder.
      origin: Origin::Foreign,
    })
  }

  /// Allocates a refcounted buffer and gathers `rows` runs of
  /// `row_bytes` into it, tightly.
  ///
  /// The padded-plane road, which this lane copies like the other one —
  /// see [`FfmpegCarrier::from_rows`](crate::FfmpegCarrier::from_rows)
  /// for why sharing a padded span is not available to anybody.
  ///
  /// `None` when the allocation fails, or when a row is not exactly
  /// `row_bytes` wide.
  ///
  /// **The row length is checked, not asserted.** It was a
  /// `debug_assert` once, which meant a release build copied
  /// `row_bytes` out of whatever slice the closure returned — an
  /// out-of-bounds read whose only guard vanished under `--release`.
  /// A length that arrives from a caller is an input, and an input is
  /// checked.
  pub(crate) fn from_rows<'a>(
    rows: usize,
    row_bytes: usize,
    mut row: impl FnMut(usize) -> &'a [u8],
  ) -> Option<Self> {
    use ffmpeg_next::ffi::av_buffer_alloc;

    let len = rows.checked_mul(row_bytes)?;
    if len == 0 {
      return Some(Self::empty());
    }
    // SAFETY: a plain allocation, checked for null before any write.
    let raw = unsafe { av_buffer_alloc(len as _) };
    if raw.is_null() {
      return None;
    }
    // The allocation is owned from here on, so a refusal below releases
    // it instead of leaking it. `Self` is that guard: it is a complete
    // carrier the moment it exists, and dropping it unrefs the buffer.
    let out = Self {
      inner: raw,
      offset: 0,
      len,
      origin: Origin::Foreign,
    };
    for index in 0..rows {
      let src = row(index);
      if src.len() != row_bytes {
        // `out` drops here and releases the allocation.
        return None;
      }
      // SAFETY: `raw` holds `rows * row_bytes` bytes; this write lands
      // at `index * row_bytes` for `row_bytes`, inside it. `src` was
      // just checked to be exactly that wide, and is a distinct
      // allocation.
      unsafe {
        core::ptr::copy_nonoverlapping(src.as_ptr(), (*raw).data.add(index * row_bytes), row_bytes);
      }
    }
    Some(out)
  }

  /// The empty carrier: no buffer, no bytes, no allocation.
  ///
  /// Infallible and `const`, which is the point — it is what an
  /// unpopulated plane slot holds, and eight of them per frame is not a
  /// place to put an allocator.
  #[must_use]
  pub const fn empty() -> Self {
    Self {
      inner: core::ptr::null_mut(),
      offset: 0,
      len: 0,
      origin: Origin::Foreign,
    }
  }

  /// Narrows this view to its first `len` bytes.
  ///
  /// Only ever shrinks: a length past the current one is clamped, since
  /// growing would extend the span past what the constructor proved.
  /// The buffer, the reference and the offset are untouched — this
  /// moves no bytes and cannot fail, which is what lets a producer take
  /// its reference **before** a conversion runs and settle the exact
  /// length after it.
  /// Narrowing **clears provenance**: whatever now sits between the new
  /// end and the old one is the carrier's own former contents, not the
  /// zeroed slack a decoder is entitled to read.
  pub(crate) const fn shrink_to(&mut self, len: usize) {
    if len < self.len {
      self.len = len;
      self.origin = Origin::Foreign;
    }
  }

  /// Bytes visible through this view.
  #[inline]
  pub const fn len(&self) -> usize {
    self.len
  }

  /// Whether the view is zero bytes long.
  #[inline]
  pub const fn is_empty(&self) -> bool {
    self.len == 0
  }

  /// Byte offset of this view's start inside the underlying buffer.
  #[inline]
  pub const fn offset(&self) -> usize {
    self.offset
  }

  /// Start of the view. Valid for [`Self::len`] bytes while `self`
  /// lives.
  ///
  /// A dangling-but-aligned pointer when the view is empty, so a caller
  /// must consult `len` before reading — the same contract
  /// `NonNull::dangling` keeps.
  pub fn as_ptr(&self) -> *const u8 {
    if self.inner.is_null() {
      return core::ptr::NonNull::<u8>::dangling().as_ptr();
    }
    // SAFETY: `inner` is non-null on this road. `data` can still be
    // null for a zero-sized buffer, and `null.add(n)` is undefined even
    // before a read, so the null case answers with the sentinel.
    unsafe {
      let data = (*self.inner).data;
      if data.is_null() {
        return core::ptr::NonNull::<u8>::dangling().as_ptr();
      }
      (data as *const u8).add(self.offset)
    }
  }

  /// The underlying `AVBufferRef`, borrowed — null for the empty
  /// carrier.
  ///
  /// Points at the **whole** buffer, not this view's sub-region, and is
  /// `*const` on purpose: a shared borrow must not become an aliased
  /// write. Do not `av_buffer_unref` it — `self` still owns that
  /// reference.
  #[inline]
  pub const fn as_av_buffer_ref(&self) -> *const AVBufferRef {
    self.inner.cast_const()
  }

  /// Whether two carriers view the same underlying allocation.
  ///
  /// The proof a clone shared rather than copied, and the proof two
  /// planes of one frame really do share one buffer.
  ///
  /// Compares the `AVBuffer` behind the reference, not the reference
  /// itself: `av_buffer_ref` mints a **new** `AVBufferRef` around the
  /// same shared object, so two carriers that genuinely share have
  /// different `AVBufferRef` pointers and the same `buffer`.
  pub fn ptr_eq(&self, other: &Self) -> bool {
    if self.inner.is_null() || other.inner.is_null() {
      // Two empty carriers share the same nothing; an empty one shares
      // nothing with a real buffer.
      return self.inner == other.inner;
    }
    // SAFETY: both `inner` pointers are non-null on this road; `buffer`
    // is a public field naming the shared allocation.
    unsafe { (*self.inner).buffer == (*other.inner).buffer }
  }

  /// Fallible [`Clone::clone`]: `None` on allocation failure instead of
  /// a panic.
  pub fn try_clone(&self) -> Option<Self> {
    if self.inner.is_null() {
      return Some(Self::empty());
    }
    // SAFETY: `inner` is non-null on this road; `av_buffer_ref` is
    // atomic and returns null only on allocation failure.
    let new_ref = unsafe { av_buffer_ref(self.inner) };
    if new_ref.is_null() {
      return None;
    }
    Some(Self {
      inner: new_ref,
      offset: self.offset,
      len: self.len,
      // Provenance is a property of the bytes, not of the handle: a
      // clone views the same range of the same allocation.
      origin: self.origin,
    })
  }
}

impl Clone for FfmpegBuffer {
  /// One refcount bump. **Panics** on allocation failure; see
  /// [`FfmpegBuffer::try_clone`] for the fallible road.
  fn clone(&self) -> Self {
    self
      .try_clone()
      .expect("FfmpegBuffer::clone: av_buffer_ref returned null (OOM)")
  }
}

impl Drop for FfmpegBuffer {
  fn drop(&mut self) {
    if self.inner.is_null() {
      return;
    }
    // SAFETY: `inner` is non-null on this road and this carrier owns
    // exactly one reference to it, released exactly once here.
    unsafe { av_buffer_unref(&mut self.inner) };
  }
}

impl AsRef<[u8]> for FfmpegBuffer {
  fn as_ref(&self) -> &[u8] {
    if self.len == 0 {
      return &[];
    }
    // SAFETY: the constructors prove `offset + len <= size` against the
    // buffer's own extent, so the range lies inside an allocation this
    // carrier holds a reference to and therefore keeps alive.
    unsafe { slice::from_raw_parts(self.as_ptr(), self.len) }
  }
}

impl fmt::Debug for FfmpegBuffer {
  /// Shape, not contents: a carrier can be megabytes, and a `Debug`
  /// that prints them is a `Debug` nobody can use.
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("FfmpegBuffer")
      .field("offset", &self.offset)
      .field("len", &self.len)
      .finish_non_exhaustive()
  }
}

#[cfg(test)]
mod tests;
