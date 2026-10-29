//! The Apple-target body of [`crate::vtscale`]: a real
//! `VTPixelTransferSession` between the decoded hardware frame and the
//! CPU download. See the parent module for the design and for the
//! stand-down discipline every refusal here follows.

use core::ptr::{self, NonNull, addr_of, addr_of_mut, read_unaligned, write_unaligned};

use ffmpeg_next::{
  ffi::{
    AVBufferRef, AVFrame, AVHWFramesContext, AVPixelFormat, av_buffer_unref, av_frame_unref,
    av_hwframe_ctx_alloc, av_hwframe_ctx_init, av_hwframe_get_buffer,
  },
  frame,
};
use libc::{c_int, c_void};
use mediadecode::decoder::ScaledOutputCapability;

/// The VideoToolbox / CoreVideo / CoreFoundation entry points this
/// stage calls.
///
/// **Every one of them is an Apple system framework symbol**, present
/// on every macOS and visionOS this crate can be built for, and none of
/// them is FFmpeg's. That is deliberate and it is what lets this module
/// be gated on the target alone: the stage names no symbol whose
/// presence depends on how the linked FFmpeg was configured, so there
/// is nothing about the FFmpeg build for a build script to predict and
/// nothing it can predict wrongly. An FFmpeg without VideoToolbox
/// simply never opens a VideoToolbox device, and this stage never sees
/// a frame to work on.
///
/// `CVPixelBufferRef` is spelled as an opaque `*mut c_void` rather than
/// borrowed from the FFmpeg bindings' `__CVBuffer` typedef: it is an
/// opaque Core Foundation handle either way, the two spellings are
/// ABI-identical, and keeping it local means this module names exactly
/// the surface it calls.
#[allow(non_snake_case)]
mod ffi {
  use libc::c_void;

  /// `OSStatus`: `noErr` (0) on success, a negative Core Foundation /
  /// VideoToolbox error code otherwise.
  pub(super) type OsStatus = i32;
  /// An opaque `CVPixelBufferRef`.
  pub(super) type CVPixelBufferRef = *mut c_void;
  /// An opaque `CFTypeRef` — only ever a pixel-transfer session here.
  pub(super) type CFTypeRef = *const c_void;
  /// An opaque `CFAllocatorRef`; this stage always passes null (the
  /// default allocator).
  pub(super) type CFAllocatorRef = *const c_void;

  /// `struct OpaqueVTPixelTransferSession`, never dereferenced.
  #[repr(C)]
  pub(super) struct OpaqueVtPixelTransferSession {
    _private: [u8; 0],
  }

  /// `VTPixelTransferSessionRef`.
  pub(super) type VTPixelTransferSessionRef = *mut OpaqueVtPixelTransferSession;

  unsafe extern "C" {
    /// Creates a pixel-transfer session. The out parameter is returned
    /// **retained** — the caller owns one reference and must
    /// `VTPixelTransferSessionInvalidate` then `CFRelease` it.
    pub(super) fn VTPixelTransferSessionCreate(
      allocator: CFAllocatorRef,
      session_out: *mut VTPixelTransferSessionRef,
    ) -> OsStatus;

    /// Tears a session down deterministically. Paired with `CFRelease`
    /// per the framework header's own instruction.
    pub(super) fn VTPixelTransferSessionInvalidate(session: VTPixelTransferSessionRef);

    /// Scales `source` into `destination`. With no properties set —
    /// which is this stage's configuration — "the full width and
    /// height of sourceBuffer are scaled to the full width and height
    /// of destinationBuffer", and the destination's attachments are
    /// replaced with ones describing the transferred image.
    pub(super) fn VTPixelTransferSessionTransferImage(
      session: VTPixelTransferSessionRef,
      source: CVPixelBufferRef,
      destination: CVPixelBufferRef,
    ) -> OsStatus;

    /// Releases one Core Foundation reference.
    pub(super) fn CFRelease(cf: CFTypeRef);

    /// The buffer's `OSType` pixel format (`'420v'`, `'420f'`, `'x420'`
    /// …).
    pub(super) fn CVPixelBufferGetPixelFormatType(buffer: CVPixelBufferRef) -> u32;
    /// The buffer's own width, which is what the transfer scales from.
    pub(super) fn CVPixelBufferGetWidth(buffer: CVPixelBufferRef) -> usize;
    /// The buffer's own height, which is what the transfer scales from.
    pub(super) fn CVPixelBufferGetHeight(buffer: CVPixelBufferRef) -> usize;
  }
}

/// The thread a resource was created on.
///
/// **[`std::thread::ThreadId`], and the choice is the whole point.** A
/// `pthread_t` is the obvious handle and the wrong one: Darwin recycles
/// them, so a thread created after the owner exits can be handed the
/// same value and `pthread_equal` will say the two are one — which
/// would let a session built on a dead thread be reused on a live
/// stranger. POSIX also leaves reading a `pthread_t` after its thread
/// ends undefined. `ThreadId` is documented never to be reused for the
/// lifetime of the process, so an owner that has exited can only ever
/// compare *unequal* to whatever is running now, which is exactly the
/// answer this guard needs.
///
/// Safe to read from a `Drop` on any Rust ≥ 1.83 — `thread::current`
/// no longer panics during thread-local destruction — and this crate's
/// floor is well above that.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct ThreadOwner(std::thread::ThreadId);

impl ThreadOwner {
  /// The calling thread.
  fn current() -> Self {
    Self(std::thread::current().id())
  }

  /// Whether the calling thread is the one this was taken on.
  fn is_current(self) -> bool {
    self == Self::current()
  }
}

/// Greatest common divisor, for reducing the rational
/// [`scaled_sample_aspect_ratio`] forms. Euclid's, on `i64` because the
/// numerator and denominator are built from an `i32` ratio and two
/// `u32` extents before they are reduced back into the pair
/// `AVRational` holds.
#[cfg_attr(not(tarpaulin), inline)]
const fn gcd(mut a: i128, mut b: i128) -> i128 {
  while b != 0 {
    let t = b;
    b = a % b;
    a = t;
  }
  a
}

/// The sample aspect ratio a picture scaled from `src` to `dst` must
/// carry to keep the **display** aspect ratio the source declared.
///
/// A sample aspect ratio says how non-square the stored pixels are, so
/// it is a function of the storage grid: resizing that grid without
/// touching the ratio silently restates the picture's shape. Scaling
/// `(w, h)` to `(dw, dh)` multiplies the ratio by `(w * dh) / (dw * h)`,
/// which is `1:1` exactly when the scale is uniform — the ordinary case,
/// where a caller fits a picture inside a pixel budget without changing
/// its shape — and corrects the display geometry when it is not.
///
/// The answer is always **reduced**, the way `av_reduce` reduces one:
/// a uniform scale of an already-reduced ratio (which is what FFmpeg's
/// decoders emit) therefore answers the identical pair, and one that was
/// not reduced answers the same ratio in its reduced form.
///
/// Answers `Some(sar)` unchanged when there is nothing to correct:
/// either term is non-positive — `0/1` is FFmpeg's "unspecified", and
/// an unspecified ratio stays unspecified rather than becoming a
/// fabricated one — or either extent is zero, which the stage refuses
/// upstream anyway.
///
/// Answers **`None`** when the corrected ratio, reduced, does not fit
/// in the `c_int` pair `AVRational` holds. That is not a fallback to
/// the source's ratio: after the grid moved, the source's ratio is a
/// *wrong* answer, not a conservative one, and publishing it would
/// restate the picture's shape. `None` means the stage stands down and
/// the caller delivers the unscaled frame, which carries the source
/// ratio because it still has the source grid.
pub(crate) const fn scaled_sample_aspect_ratio(
  sar: (i32, i32),
  src: (u32, u32),
  dst: (u32, u32),
) -> Option<(i32, i32)> {
  let (num, den) = sar;
  if num <= 0 || den <= 0 || src.0 == 0 || src.1 == 0 || dst.0 == 0 || dst.1 == 0 {
    return Some(sar);
  }
  // **`i128`, and the width is load-bearing rather than cautious.** The
  // widest intermediate here is `i32::MAX * u32::MAX * u32::MAX`, a
  // shade under 2^95: it does not fit in `i64`, and an `i64` product
  // would panic under `overflow-checks` and wrap without them. Real
  // frame extents are bounded by `FrameLimits` long before they get
  // near that, but this is a pure function over a caller-supplied
  // request and a decoder-supplied extent, and it may not have a size
  // at which it stops being right. Nothing below can overflow.
  let numerator = (num as i128) * (src.0 as i128) * (dst.1 as i128);
  let denominator = (den as i128) * (dst.0 as i128) * (src.1 as i128);
  // Both terms are strictly positive (every zero and negative input
  // returned above), so the divisor is too.
  let divisor = gcd(numerator, denominator);
  let numerator = numerator / divisor;
  let denominator = denominator / divisor;
  if numerator > i32::MAX as i128 || denominator > i32::MAX as i128 {
    return None;
  }
  Some((numerator as i32, denominator as i32))
}

/// Whether `size` is a size this stage may accept for a source of
/// `source` pixels, independent of any platform.
///
/// Two refusals, both from the budget law this capability exists to
/// serve:
///
/// - **Zero.** A zero-extent picture is not a smaller picture.
/// - **Upscale.** The stage exists to move fewer bytes across the
///   GPU→CPU bus; enlarging a picture moves more, and inventing detail
///   a decoder did not produce is the caller's business, not a decode
///   session's. Equal is not an upscale and is accepted — the stage
///   simply has nothing to do, and says so by standing down per frame.
#[cfg_attr(not(tarpaulin), inline)]
pub(crate) const fn is_acceptable_request(size: (u32, u32), source: (u32, u32)) -> bool {
  size.0 != 0 && size.1 != 0 && size.0 <= source.0 && size.1 <= source.1
}

/// An owned `VTPixelTransferSessionRef`, and the thread it belongs to.
///
/// The **only** Core Foundation object this crate owns on this road:
/// the pixel buffers on both ends of the transfer belong to FFmpeg. One
/// reference in, one release out, at the one place a session can go out
/// of scope.
///
/// # Why the session records its thread
///
/// `VideoToolbox.framework`'s own header marks `VTPixelTransferSessionRef`
/// `CM_SWIFT_NONSENDABLE`, and Apple publishes no guarantee that a
/// session may be created on one thread and used or torn down on
/// another. `VideoDecoder` is `Send`, so a decoder — and with it this
/// stage — really can cross threads. Rather than assume a guarantee the
/// vendor declines to give, the stage makes the question moot: a
/// session is only ever *used* on the thread that created it (the cache
/// retires and rebuilds when the decoder has moved, see
/// [`ScaledOutput::stage`]), and the one call it cannot avoid making
/// off-thread — the release in `Drop` — is narrowed to the half the
/// framework documents as sufficient on its own.
struct Session {
  handle: NonNull<ffi::OpaqueVtPixelTransferSession>,
  owner: ThreadOwner,
}

impl Drop for Session {
  fn drop(&mut self) {
    // **Invalidate only on the owning thread.**
    // `VTPixelTransferSession.h`: "When a pixel transfer session's
    // retain count reaches zero, it is automatically invalidated, but
    // since sessions may be retained by multiple parties, it can be
    // hard to predict when this will happen. Calling
    // VTPixelTransferSessionInvalidate ensures a deterministic, orderly
    // teardown." So the release is the teardown and the invalidate only
    // makes its timing deterministic — which is worth having on the
    // thread that owns the session, and not worth reaching for on a
    // thread the framework never promised it could be called from. This
    // stage is the session's sole retainer, so the release always drops
    // the count to zero either way.
    //
    // Core Foundation reference counting is atomic and callable from
    // any thread; that is what makes the unconditional `CFRelease`
    // sound where the conditional `Invalidate` is the careful half.
    let owned_here = self.owner.is_current();
    // SAFETY: `self.handle` is the retained session
    // `VTPixelTransferSessionCreate` handed back, never released before
    // now — this is its sole owner and the type is neither `Clone` nor
    // `Copy`.
    unsafe {
      if owned_here {
        ffi::VTPixelTransferSessionInvalidate(self.handle.as_ptr());
      }
      ffi::CFRelease(self.handle.as_ptr().cast::<c_void>().cast_const());
    }
    if !owned_here {
      tracing::debug!(
        "mediadecode-ffmpeg: scaled-output session released from a thread other than the one          that created it; skipped the optional invalidate"
      );
    }
  }
}

/// An owned reference to the fitted-size `AVHWFramesContext` the
/// destination buffers are drawn from.
struct FittedPool(*mut AVBufferRef);

impl Drop for FittedPool {
  fn drop(&mut self) {
    // SAFETY: `self.0` is the reference `av_hwframe_ctx_alloc` returned
    // and this is its sole owner. `av_buffer_unref` tolerates a null
    // slot and nulls the pointer it is given.
    unsafe { av_buffer_unref(&mut self.0) };
  }
}

/// What a built session and pool were built **for**. Any change — the
/// stream's format or extent, or the caller's requested size — retires
/// them and builds again.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct StageKey {
  /// The source `CVPixelBuffer`'s `OSType`, which decides the
  /// destination's: matching it exactly is what keeps the transfer a
  /// pure resize with no colour conversion, and keeps the pixel format
  /// the CPU download reports identical to the unscaled road's.
  source_cv_format: u32,
  /// The source frame's extent.
  source: (u32, u32),
  /// The accepted request.
  fitted: (u32, u32),
  /// The source frames context's `sw_format`, widened — the format the
  /// download will produce on both roads.
  sw_format: c_int,
}

/// A built stage: the session, the pool it draws destinations from, and
/// the reusable frame those destinations arrive in.
struct Built {
  /// Declared first so the frame releases its pooled buffer before the
  /// pool reference goes. Reference counting makes the order
  /// immaterial; stating it keeps it that way.
  fitted: frame::Video,
  key: StageKey,
  session: Session,
  pool: FittedPool,
}

/// The session cache: either a stage is built for a key, or nothing is.
///
/// **Two states, because a third would be unreachable.** An earlier
/// shape carried a `Refused` latch so a key that failed to build was
/// not retried every frame. The one-way promise on [`ScaledOutput`]
/// subsumes it and more: a build failure stands the stage down, and a
/// stood-down stage does not run again until an explicit request
/// renews it — which retires everything anyway.
enum Cache {
  Empty,
  Built(Built),
}

impl Cache {
  /// The key a built stage was built for.
  fn built_key(&self) -> Option<StageKey> {
    match self {
      Self::Built(built) => Some(built.key),
      Self::Empty => None,
    }
  }
}

/// What the cache can do for a frame, decided before anything is
/// built.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum CachePlan {
  /// The standing session and pool were built for exactly this key.
  Reuse,
  /// Nothing usable is cached — build, replacing whatever is there.
  Build,
}

/// The cache's whole decision, as arithmetic over keys rather than over
/// GPU objects — which is what makes it testable without one.
///
/// **One session per key, and the key is the stream's format and extent
/// paired with the caller's requested size.** A steady stream reuses one
/// session and one pool for its whole life; a mid-stream resolution or
/// format change, or a second `request_scaled_output` naming a different
/// size, retires both and builds once more.
fn cache_plan(built: Option<StageKey>, want: StageKey) -> CachePlan {
  if built == Some(want) {
    CachePlan::Reuse
  } else {
    CachePlan::Build
  }
}

/// Whether state built for `key` is stale in front of a source of
/// `cv_format`, `extent` and `sw_format`.
///
/// Split out as arithmetic so the retirement rule is testable without a
/// GPU to build state on. `None` — nothing cached — is never stale.
fn is_stale(key: Option<StageKey>, cv_format: u32, extent: (u32, u32), sw_format: c_int) -> bool {
  key.is_some_and(|key| {
    key.source_cv_format != cv_format || key.source != extent || key.sw_format != sw_format
  })
}

/// The scaled-output stage on the VideoToolbox road.
pub(crate) struct ScaledOutput {
  /// The accepted request, if one is standing.
  ///
  /// A **refused** request clears it, because the trait says what an
  /// `Unsupported` answer means and it is not "your old request still
  /// stands": a backend that cannot honor a request "answers
  /// `Unsupported` and leaves the session decoding at full coded size".
  /// A two-valued word cannot say "rejected, and the previous one is
  /// still in force", so the honest reading is the one the caller can
  /// act on — after any refusal, this session is returning to full
  /// size, and the caller resamples for itself.
  request: Option<(u32, u32)>,
  /// `true` once a frame went out at something other than the standing
  /// request's extent.
  ///
  /// **This is what keeps the capability word from lying.** The trait's
  /// contract is that a `Supported` answer lets a caller skip its own
  /// resampler, and this stage can stand down per frame — a stream
  /// whose pixel buffer is padded, a crop rectangle, side data a resize
  /// would strand, a resolution change that turns the request into an
  /// upscale, a transfer that fails. Delivering a full-size picture
  /// under a `Supported` answer would hand that caller mixed extents
  /// with nothing to notice them by. So the first time it happens the
  /// stage records it here, [`Self::capability`] answers `Unsupported`
  /// from that moment, and the caller's next query — the very one the
  /// trait tells it to make — learns that it is back in charge of
  /// resampling. A new [`Self::request`] clears it and buys a fresh
  /// chance.
  ///
  /// A request the source already satisfies (`fitted` equal to the
  /// source extent) is **not** a failure to honor: the picture comes
  /// back at exactly the requested size. That path does not set this.
  unhonored: bool,
  cache: Cache,
}

/// The facts one source frame carries that the stage needs, read once
/// per frame before anything is borrowed mutably.
struct Source {
  pixbuf: ffi::CVPixelBufferRef,
  device_ref: *mut AVBufferRef,
  cv_format: u32,
  sw_format: c_int,
  /// The frame's display extent, which — gated below — is also the
  /// pixel buffer's.
  extent: (u32, u32),
  sar: (i32, i32),
}

impl ScaledOutput {
  /// A stage with nothing requested and nothing built.
  pub(crate) const fn new() -> Self {
    Self {
      request: None,
      unhonored: false,
      cache: Cache::Empty,
    }
  }

  /// Whether this build can honor a request at all. `true` here; the
  /// [`super`] module's other body answers `false`.
  pub(crate) const fn supported() -> bool {
    true
  }

  /// Whether the stage will attempt to fit the next frame at all.
  ///
  /// The behavioural half of [`Self::promise_stands`]: the two move
  /// together by construction, so the capability word a caller reads
  /// and what the next frame actually does can never disagree.
  #[cfg(test)]
  pub(crate) const fn staging_armed(&self) -> bool {
    !self.unhonored
  }

  /// Whether this stage's promise still stands.
  ///
  /// `false` once a frame has gone out at anything other than a
  /// standing request's extent — see [`Self::unhonored`]. A caller that
  /// acted on a `Supported` answer learns from one query that it is
  /// resampling again.
  pub(crate) const fn promise_stands(&self) -> bool {
    !self.unhonored
  }

  /// Record that this frame could not be honored, so the capability
  /// word stops promising what the stage stopped delivering — and stop
  /// staging until somebody asks again.
  ///
  /// **One-way, and that is the whole contract.** A caller told
  /// `Unsupported` resumes resampling for itself; if the stage then
  /// quietly fitted a later frame, that caller would resample an
  /// already-fitted picture, or collect mixed extents from a session
  /// that had said it was done. So the flag gates [`Self::stage`] as
  /// well as [`Self::promise_stands`], and only an accepted
  /// [`Self::request`] lifts it. Whatever was built goes with it: a
  /// stage that will not run has no business holding a pooled surface,
  /// a frames context and a transfer session.
  fn stand_down(&mut self) {
    self.unhonored = true;
    self.retire();
  }

  /// The standing request, for the tests and the observability that
  /// want to see it.
  #[cfg(test)]
  pub(crate) const fn requested(&self) -> Option<(u32, u32)> {
    self.request
  }

  /// Drop the standing request and everything built for it, returning
  /// the session to full coded size from the next picture.
  ///
  /// What a **refused** request does, reached from the one road that
  /// refuses without going through [`Self::request`]: a request placed
  /// while a decoded picture is parked. That road cannot accept —
  /// the parked picture is already decided — but it must not leave the
  /// old request armed either, or a caller told `Unsupported` would go
  /// on receiving pictures fitted to it and resample them a second
  /// time. Same clearing, same retirement, one meaning for the word.
  pub(crate) fn cancel(&mut self) {
    self.request = None;
    self.retire();
  }

  /// Drops everything built over a hardware device, keeping the
  /// caller's standing request.
  ///
  /// Called when the decoder swaps the device out from under the stage
  /// — a probe advance to another backend. The request is the caller's
  /// and survives; the session and pool are the old device's and do
  /// not.
  pub(crate) fn retire(&mut self) {
    self.cache = Cache::Empty;
  }

  /// Retire anything cached for a **different** stream shape than the
  /// one in front of us.
  ///
  /// Called as soon as a frame's source tuple is known and before any
  /// of the per-frame stand-downs, so a session built for a 4K stream
  /// does not sit retained through a run of 720p frames that turned the
  /// standing request into an upscale. Matching state is left alone —
  /// that is the steady-state path, and it must stay a comparison
  /// rather than a rebuild.
  fn retire_unless_source_is(&mut self, cv_format: u32, extent: (u32, u32), sw_format: c_int) {
    if is_stale(self.cache.built_key(), cv_format, extent, sw_format) {
      self.retire();
    }
  }

  /// Records `size` as the size frames should come out at, measured
  /// against the session's `source` extent.
  ///
  /// Answers [`ScaledOutputCapability::Supported`] when it was
  /// recorded and [`ScaledOutputCapability::Unsupported`] when
  /// [`is_acceptable_request`] refused it. Never an error.
  ///
  /// **A refusal returns the session to full coded size**, which is
  /// what the trait says an `Unsupported` answer from this hook means,
  /// and the only reading a caller can safely act on: told
  /// `Unsupported`, it resamples for itself, and a session that went on
  /// quietly fitting to an older request would have it resample an
  /// already-fitted picture. So a refused request drops whatever was
  /// standing and retires what was built for it.
  ///
  /// Either way the change takes effect from the next picture the
  /// decoder produces. One already decoded keeps the extent it was
  /// decoded at — the same rule the accepting direction has always
  /// carried, applied symmetrically.
  pub(crate) fn request(&mut self, size: (u32, u32), source: (u32, u32)) -> ScaledOutputCapability {
    if !is_acceptable_request(size, source) {
      tracing::debug!(
        requested_width = size.0,
        requested_height = size.1,
        source_width = source.0,
        source_height = source.1,
        "mediadecode-ffmpeg: scaled-output request refused (zero or upscale); \
         the session returns to full size"
      );
      self.request = None;
      self.retire();
      return ScaledOutputCapability::Unsupported;
    }
    // **An accepted request always retires what is cached — including
    // when the size has not changed.** Two reasons, and the second is
    // the sharper one.
    //
    // A *changed* size makes the cache dead weight: the key carries the
    // requested extent, so a later frame would rebuild anyway — but
    // "later" can be never, because a request for the source's own
    // extent stands down before the cache is ever consulted, and an 8K
    // P010 surface, its frames context and its session would sit
    // retained until the decoder itself dropped.
    //
    // An *unchanged* size is the case that would otherwise hand back a
    // promise already known to be false. A build or transfer failure
    // latches `Cache::Refused(key)`; keeping that latch across an
    // explicit re-request would clear the broken promise below, answer
    // `Supported`, and then stand down deterministically on the very
    // next frame. Asking again is a caller saying "try again", and the
    // honest reading of that is to try.
    self.retire();
    self.request = Some(size);
    // A fresh request buys a fresh promise.
    self.unhonored = false;
    ScaledOutputCapability::Supported
  }

  /// Scale `src` on the GPU, and answer the frame the CPU download
  /// should read.
  ///
  /// `Some` is the fitted surface; `None` means the stage stood down
  /// and the caller should download `src` itself, unchanged. Never
  /// errors — see the parent module.
  pub(crate) fn stage(&mut self, src: &frame::Video) -> Option<&frame::Video> {
    let fitted = self.request?;
    if self.unhonored {
      // The promise is already broken and the caller has been told, so
      // it is resampling for itself. Fitting a frame now would hand it
      // a picture to resample twice. Only an accepted request restarts
      // this.
      return None;
    }
    // **The extent first, before any gate that could refuse this
    // frame.** A frame the stage cannot scale but whose extent already
    // *is* the requested one has been honored to the letter, so it must
    // not break the promise — and reading the two `c_int`s that settle
    // that costs nothing and cannot fail.
    let Some(extent) = frame_extent(src) else {
      self.stand_down();
      return None;
    };
    if fitted == extent {
      // Already the requested size: the picture goes out at exactly
      // what was asked for, so the promise holds and nothing is scaled.
      // Anything built for an earlier, smaller request is dead weight
      // from here — a pooled surface, its frames context and a session
      // — and this is the last point at which it can be let go.
      self.retire();
      return None;
    }
    let Some(source) = read_source(src) else {
      // A frame this stage cannot read is a frame whose stream it can
      // no longer vouch for, so nothing built for the old one may
      // outlive it.
      self.retire();
      self.stand_down();
      return None;
    };
    // Now that the stream's own shape is known, anything cached for a
    // different one goes — **before** the stand-downs below, each of
    // which returns early and would otherwise leave it retained for the
    // whole run.
    self.retire_unless_source_is(source.cv_format, source.extent, source.sw_format);
    if !is_acceptable_request(fitted, source.extent) {
      self.stand_down();
      // A standing request that a mid-stream resolution change turned
      // into an upscale. The request stays standing — a later
      // resolution can make it honorable again — and this frame goes
      // out full size.
      tracing::debug!(
        requested_width = fitted.0,
        requested_height = fitted.1,
        source_width = source.extent.0,
        source_height = source.extent.1,
        "mediadecode-ffmpeg: standing scaled-output request is not a downscale of this \
         frame; delivering it full size"
      );
      return None;
    }
    // **Settled before anything is built or drawn.** A sample aspect
    // ratio describes the storage grid, so the scale moves it; when the
    // corrected ratio will not fit in `AVRational`'s `c_int` pair there
    // is no honest value to publish — the source's would restate the
    // picture's shape — and the frame goes out full size, still
    // carrying the grid its ratio describes. Deciding it here rather
    // than after the transfer keeps it a stand-down like every other
    // one, with no destination drawn and nothing to latch.
    let Some(sar) = scaled_sample_aspect_ratio(source.sar, source.extent, fitted) else {
      self.stand_down();
      tracing::debug!(
        sar_num = source.sar.0,
        sar_den = source.sar.1,
        "mediadecode-ffmpeg: the scale-corrected sample aspect ratio is not representable; \
         delivering this frame full size"
      );
      return None;
    };
    let key = StageKey {
      source_cv_format: source.cv_format,
      source: source.extent,
      fitted,
      sw_format: source.sw_format,
    };
    // **A session never crosses a thread.** `VideoDecoder` is `Send`, so
    // a decoder can be built on one thread and driven on another; the
    // stage answers that by retiring anything built elsewhere and
    // building again here, rather than by assuming a mobility guarantee
    // VideoToolbox does not publish. See [`Session`]. Costs one
    // `pthread_equal` per scaled frame, and a rebuild only on a real
    // move — which for a decode session is once, if ever.
    if let Cache::Built(built) = &self.cache
      && !built.session.owner.is_current()
    {
      tracing::debug!(
        "mediadecode-ffmpeg: the decoder moved threads; rebuilding the scaled-output session          on this one"
      );
      self.retire();
    }
    match cache_plan(self.cache.built_key(), key) {
      CachePlan::Reuse => {}
      // SAFETY: `source.device_ref` was read a few lines up out of `src`'s
      // own frames context, and `src` — which holds that context — is
      // borrowed for the whole of this call.
      CachePlan::Build => match unsafe { Built::create(key, source.device_ref) } {
        Some(built) => self.cache = Cache::Built(built),
        None => {
          self.stand_down();
          return None;
        }
      },
    }
    let Cache::Built(built) = &mut self.cache else {
      self.stand_down();
      return None;
    };
    if built.transfer(src, &source, sar).is_err() {
      self.stand_down();
      return None;
    }
    // Not `let Cache::Built(built) = &self.cache else { self.stand_down(); ... };
    // Some(&built.fitted)`: that shares one borrow of `self.cache` across
    // both the returning arm and `stand_down`'s `&mut self`, and because
    // the returning arm ties the borrow to this function's output
    // lifetime, 1.95's NLL treats it as live across the whole `let else`
    // — including the arm that never returns it — and rejects the
    // mutation (E0502; NLL "Problem Case #3", rust-lang/rfcs#2094, the
    // same shape as that RFC's `map.get_mut` / `map.insert` example).
    // `matches!` here borrows only long enough to answer a `bool`, never
    // touching the output lifetime, so by the time `stand_down` can run
    // no borrow of `self.cache` is outstanding; the `match` below then
    // borrows fresh, only on the path that returns it. `self.cache`
    // cannot actually be anything but `Built` here — nothing since the
    // `&mut` match above replaces it — so `Cache::Empty => None` is
    // defensive, not reachable in practice, same as the arm it replaces.
    if !matches!(self.cache, Cache::Built(_)) {
      self.stand_down();
      return None;
    }
    match &self.cache {
      Cache::Built(built) => Some(&built.fitted),
      Cache::Empty => None,
    }
  }
}

impl Built {
  /// Build the fitted frames context and the transfer session for
  /// `key`. `None` on any failure, with the reason logged — the caller
  /// latches it so the failure is reported once, not per frame.
  ///
  /// # Safety
  ///
  /// `device_ref` must be null or a live `AVBufferRef` to an
  /// `AVHWDeviceContext` for the duration of the call;
  /// `av_hwframe_ctx_alloc` takes a reference of its own.
  unsafe fn create(key: StageKey, device_ref: *mut AVBufferRef) -> Option<Self> {
    if device_ref.is_null() {
      return None;
    }
    let fitted = frame::Video::empty();
    // SAFETY: reading the freshly allocated frame's pointer to check
    // `av_frame_alloc` did not return null under memory pressure.
    if unsafe { fitted.as_ptr() }.is_null() {
      tracing::warn!("mediadecode-ffmpeg: scaled output could not allocate its destination frame");
      return None;
    }

    // SAFETY: `device_ref` is non-null, so by the contract above it is a
    // live device reference for this call — in practice the decode
    // session's VideoToolbox device, read out of the source frame's frames
    // context; `av_hwframe_ctx_alloc` takes its own reference to it.
    let pool = FittedPool(unsafe { av_hwframe_ctx_alloc(device_ref) });
    if pool.0.is_null() {
      tracing::warn!("mediadecode-ffmpeg: scaled output could not allocate a frames context");
      return None;
    }
    // SAFETY: `pool.0` is a freshly allocated `AVHWFramesContext`
    // reference; `data` points at the context, which is ours to fill
    // until `av_hwframe_ctx_init`. `sw_format` is written through an
    // `i32` projection rather than as the bindgen enum: the value came
    // out of FFmpeg's own context and may name a format these bindings
    // do not.
    unsafe {
      let ctx = (*pool.0).data.cast::<AVHWFramesContext>();
      (*ctx).format = AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX;
      write_unaligned(
        addr_of_mut!((*ctx).sw_format).cast::<c_int>(),
        key.sw_format,
      );
      (*ctx).width = key.fitted.0 as c_int;
      (*ctx).height = key.fitted.1 as c_int;
    }
    // SAFETY: the context is filled per its documented contract above.
    let rc = unsafe { av_hwframe_ctx_init(pool.0) };
    if rc < 0 {
      tracing::warn!(
        rc,
        width = key.fitted.0,
        height = key.fitted.1,
        "mediadecode-ffmpeg: scaled output could not initialise its fitted frames context; \
         frames stay full size"
      );
      return None;
    }

    let mut raw: ffi::VTPixelTransferSessionRef = ptr::null_mut();
    // SAFETY: `raw` is a live out-parameter slot; a null allocator asks
    // for the default one.
    let status = unsafe { ffi::VTPixelTransferSessionCreate(ptr::null(), &mut raw) };
    let session = match NonNull::new(raw) {
      Some(handle) if status == 0 => Session {
        handle,
        owner: ThreadOwner::current(),
      },
      other => {
        // A non-zero status with a non-null out-parameter is not a
        // shape the framework documents, but releasing what we were
        // handed costs one branch and closes the leak either way.
        if let Some(handle) = other {
          drop(Session {
            handle,
            owner: ThreadOwner::current(),
          });
        }
        tracing::warn!(
          status,
          "mediadecode-ffmpeg: VTPixelTransferSessionCreate failed; frames stay full size"
        );
        return None;
      }
    };
    Some(Self {
      fitted,
      key,
      session,
      pool,
    })
  }

  /// Draw one destination buffer from the pool and scale `src` into it.
  ///
  /// `Err(())` leaves `self.fitted` unreferenced (no pooled buffer
  /// retained) and tells the caller to latch this key as refused.
  fn transfer(&mut self, src: &frame::Video, source: &Source, sar: (i32, i32)) -> Result<(), ()> {
    // SAFETY: `self.fitted` is this stage's own frame. Unreferencing it
    // releases the previous frame's pooled buffer and its side data,
    // which is also `av_hwframe_get_buffer`'s documented precondition
    // ("an empty (freshly allocated or unreffed) frame").
    unsafe { av_frame_unref(self.fitted.as_mut_ptr()) };
    // SAFETY: `self.pool.0` is an initialised VideoToolbox frames
    // context and `self.fitted` was just unreferenced.
    let rc = unsafe { av_hwframe_get_buffer(self.pool.0, self.fitted.as_mut_ptr(), 0) };
    if rc < 0 {
      tracing::warn!(
        rc,
        "mediadecode-ffmpeg: scaled output could not draw a fitted surface from its pool; \
         frames stay full size"
      );
      return Err(());
    }
    // SAFETY: the frame was just filled by `av_hwframe_get_buffer`,
    // which sets `data[3]` to the pooled `CVPixelBufferRef`.
    let destination = unsafe { (*self.fitted.as_ptr()).data[3] }.cast::<c_void>();
    if !destination.is_null() {
      // **The transfer must be a resize and nothing else, and this is
      // where that is checked rather than arranged.**
      //
      // `VTPixelTransferSessionTransferImage` will happily convert
      // between pixel formats, and a destination whose `OSType`
      // differed from the source's would mean a colour or range
      // conversion this stage never advertised — with metadata that
      // did not move with it. So the destination's format is *asked*,
      // and a mismatch stands the stage down.
      //
      // Asked rather than steered on purpose. FFmpeg's pool derives the
      // destination `OSType` from the frames context's `sw_format` and
      // an `AVVTFramesContext.color_range`, and writing that field
      // would make this module name an FFmpeg symbol whose presence
      // depends on how the linked FFmpeg was configured — the one thing
      // that would force a build script to predict `bindgen`'s output,
      // and be wrong about it on some cross or overlay configuration.
      // Verifying costs one call per frame, names only Apple's own
      // symbols, and gives a *stronger* guarantee than steering did: it
      // is checked on the buffer actually being written, every time.
      //
      // The cost is a narrower stage. A full-range source
      // (`420f`-class) meets a video-range pool and stands down, so it
      // is delivered full size. VideoToolbox's H.264 and HEVC decode
      // paths produce video-range buffers, which is why the road this
      // serves is unaffected in practice.
      // SAFETY: `destination` is the live pooled `CVPixelBufferRef`.
      let produced = unsafe { ffi::CVPixelBufferGetPixelFormatType(destination) };
      if produced != source.cv_format {
        // SAFETY: releasing a destination this stage will not use.
        unsafe { av_frame_unref(self.fitted.as_mut_ptr()) };
        tracing::debug!(
          source_format = source.cv_format,
          destination_format = produced,
          "mediadecode-ffmpeg: the fitted pool's pixel format is not the source's, so a \
           transfer would convert rather than resize; frames stay full size"
        );
        return Err(());
      }
    }
    if destination.is_null() {
      // SAFETY: as above; releasing what the pool just handed over.
      unsafe { av_frame_unref(self.fitted.as_mut_ptr()) };
      tracing::warn!(
        "mediadecode-ffmpeg: the fitted frames context produced no pixel buffer; \
         frames stay full size"
      );
      return Err(());
    }
    // SAFETY: both handles are live `CVPixelBufferRef`s — the source
    // owned by `src`'s `AVFrame`, the destination by `self.fitted`'s —
    // and the session is this stage's own. The call is synchronous, so
    // both outlive it.
    let status = unsafe {
      ffi::VTPixelTransferSessionTransferImage(
        self.session.handle.as_ptr(),
        source.pixbuf,
        destination,
      )
    };
    if status != 0 {
      // SAFETY: releasing the destination we drew but did not fill.
      unsafe { av_frame_unref(self.fitted.as_mut_ptr()) };
      tracing::warn!(
        status,
        "mediadecode-ffmpeg: VTPixelTransferSessionTransferImage failed; \
         frames stay full size"
      );
      return Err(());
    }
    // SAFETY: both are live `AVFrame`s and `self.fitted` was unreffed
    // above, which is the helper's documented precondition. Its cost is
    // paid twice per scaled frame — once here and once again when the
    // CPU download copies from this frame — and both copies are held to
    // the same entry and byte caps.
    if unsafe { crate::decoder::copy_frame_props_minimal(self.fitted.as_mut_ptr(), src.as_ptr()) }
      .is_err()
    {
      // SAFETY: releasing a destination whose metadata is incomplete.
      unsafe { av_frame_unref(self.fitted.as_mut_ptr()) };
      tracing::warn!(
        "mediadecode-ffmpeg: scaled output could not carry this frame's metadata across; \
         frames stay full size"
      );
      return Err(());
    }
    // The one property the copy above cannot carry unchanged: a sample
    // aspect ratio describes the storage grid, and the grid just moved.
    // `sar` was computed — and its representability settled — before
    // this frame's destination was ever drawn, so there is nothing to
    // release here.
    // SAFETY: writing two `c_int`s into this stage's own frame.
    unsafe {
      let raw = self.fitted.as_mut_ptr();
      (*raw).sample_aspect_ratio.num = sar.0;
      (*raw).sample_aspect_ratio.den = sar.1;
    }
    Ok(())
  }
}

/// The frame's display extent, or `None` when it does not have a
/// positive one.
///
/// Split out of [`read_source`] because it is the one fact the stage
/// needs *before* it decides whether a frame it cannot scale broke a
/// promise: a picture whose extent already equals the request was
/// honored exactly, whatever else about it makes it unscalable.
///
/// # Safety
/// `src` is a live `frame::Video`; this reads two `c_int` fields of its
/// `AVFrame` and forms no reference to it.
fn frame_extent(src: &frame::Video) -> Option<(u32, u32)> {
  // SAFETY: as documented above.
  unsafe {
    let raw = src.as_ptr();
    if raw.is_null() {
      return None;
    }
    let (width, height) = ((*raw).width, (*raw).height);
    if width <= 0 || height <= 0 {
      return None;
    }
    Some((width as u32, height as u32))
  }
}

/// Read what one source frame can tell the stage, or `None` when it is
/// not a frame this stage may touch.
///
/// Every `None` here is a stand-down, and each has its own reason:
///
/// - **Not a VideoToolbox surface.** Nothing to transfer.
/// - **No frames context, or no pixel buffer.** The frame is not the
///   shape the hwaccel road produces; scaling it would be guesswork.
/// - **A non-empty crop rectangle.** The rectangle is expressed in the
///   source grid, and a resize invalidates it: the transfer scales the
///   whole buffer, so the padding the crop was there to remove would be
///   scaled into the picture. libavcodec applies cropping itself by
///   default (`AVCodecContext.apply_cropping`), so this is a guard
///   against a configuration this crate does not set rather than a
///   common path.
/// - **A pixel buffer whose extent is not the frame's.** The transfer
///   scales the buffer's full extent; if that is larger than the
///   picture, the surplus is padding and scaling it in would be wrong.
/// - **Side data a resize would strand, or side-data bookkeeping that
///   cannot be walked.** See [`side_data_forbids_scaling`].
fn read_source(src: &frame::Video) -> Option<Source> {
  // SAFETY: `src` is a live `frame::Video`; every read below is of a
  // scalar or pointer field of its `AVFrame`, and `format` is read as
  // the `c_int` the binding declares rather than as a pixel-format
  // enum.
  unsafe {
    let raw = src.as_ptr();
    if raw.is_null() {
      return None;
    }
    let format = read_unaligned(addr_of!((*raw).format));
    if format != AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX as c_int {
      return None;
    }
    if (*raw).crop_left != 0
      || (*raw).crop_top != 0
      || (*raw).crop_right != 0
      || (*raw).crop_bottom != 0
    {
      tracing::debug!("mediadecode-ffmpeg: scaled output stands down on a cropped frame");
      return None;
    }
    if side_data_forbids_scaling(raw) {
      return None;
    }
    let frames_ref = (*raw).hw_frames_ctx;
    if frames_ref.is_null() {
      return None;
    }
    let frames_ctx = (*frames_ref).data.cast::<AVHWFramesContext>();
    if frames_ctx.is_null() {
      return None;
    }
    let device_ref = (*frames_ctx).device_ref;
    let sw_format = read_unaligned(addr_of!((*frames_ctx).sw_format).cast::<c_int>());
    let pixbuf = (*raw).data[3].cast::<c_void>();
    if pixbuf.is_null() {
      return None;
    }
    let (width, height) = ((*raw).width, (*raw).height);
    if width <= 0 || height <= 0 {
      return None;
    }
    let extent = (width as u32, height as u32);
    let buffer_extent = (
      ffi::CVPixelBufferGetWidth(pixbuf),
      ffi::CVPixelBufferGetHeight(pixbuf),
    );
    if buffer_extent != (extent.0 as usize, extent.1 as usize) {
      tracing::debug!(
        frame_width = extent.0,
        frame_height = extent.1,
        buffer_width = buffer_extent.0,
        buffer_height = buffer_extent.1,
        "mediadecode-ffmpeg: scaled output stands down — the pixel buffer's extent is not \
         the picture's, so a full-buffer transfer would scale padding into it"
      );
      return None;
    }
    Some(Source {
      pixbuf,
      device_ref,
      cv_format: ffi::CVPixelBufferGetPixelFormatType(pixbuf),
      sw_format,
      extent,
      sar: (
        (*raw).sample_aspect_ratio.num,
        (*raw).sample_aspect_ratio.den,
      ),
    })
  }
}

/// Whether `frame` carries side data that forbids scaling it.
///
/// **A stand-down rather than a strip, and that is the crate's stated
/// stance.** `copy_frame_props_minimal` — the helper the fitted frame
/// borrows to carry this frame's metadata across — byte-copies side
/// data verbatim. For most kinds that is exactly right; for the ones
/// whose payload is expressed in the picture's own grid it is not.
/// After a resize such a payload describes a grid that no longer
/// exists, and a consumer trusting it would crop, orient or weight the
/// wrong part of the picture.
///
/// The alternative — copy everything except those entries — publishes a
/// fitted picture with metadata quietly missing, which is the shape
/// [`crate::decoder::copy_frame_props_minimal`]'s own out-of-memory arm
/// already rejects in as many words: a picture that comes back with
/// something quietly missing is worse than one that does not come back,
/// because nothing downstream can tell. So the stage stands down and
/// the caller delivers the unscaled frame, metadata and grid agreeing
/// as they always did.
///
/// # Which kinds, and why they are named rather than asked for
///
/// FFmpeg 8 added `av_frame_side_data_desc`, whose descriptor carries
/// an `AV_SIDE_DATA_PROP_SIZE_DEPENDENT` bit — the future-proof way to
/// ask this question. It is deliberately **not** used: `ffmpeg-sys-next`
/// binds whatever headers the host has, this crate builds against
/// FFmpeg releases older than 8, and a build there would meet either a
/// missing `AVSideDataDescriptor` type or a descriptor whose props
/// never carry the bit — an unfixed gate wearing a fix's clothes, and
/// the same strong-import class the platform gate above exists to
/// avoid.
///
/// So the kinds are named. The list is not guesswork: it is FFmpeg 9's
/// own `AV_SIDE_DATA_PROP_SIZE_DEPENDENT` table, **intersected with
/// [`crate::decoder::whitelisted_side_data_kind`]** — the only entries
/// this crate copies onto a frame at all. FFmpeg 9 marks seven kinds
/// size-dependent; the four this crate never copies (motion vectors,
/// detection bounding boxes, video hint, LCEVC payload) are dropped by
/// that whitelist on the scaled and unscaled roads alike, so a resize
/// strands nothing of theirs. The three that remain are the three
/// below. A later FFmpeg marking a *whitelisted* kind size-dependent
/// would need this list extended, which is the price of not depending
/// on a symbol half the supported releases do not have.
///
/// # What else counts as forbidding, and what this walk trusts
///
/// Three shapes of side-data bookkeeping are refused before the array
/// is ever indexed, because each is a value the `c_int` field can hold
/// that a real array provably cannot back: a **negative** count, a
/// **positive count with a null array**, and a count past
/// [`crate::decoder::HW_COPY_SIDE_DATA_MAX_ENTRIES`] — the cap the copy
/// path itself truncates at, past which this walk could not tell
/// whether a size-dependent entry sat in the part that gets dropped. A
/// **null entry** found inside the array is refused too, on the same
/// reasoning, from inside the walk.
///
/// What is left is trusted, and the trust is worth naming: within those
/// bounds this reads `nb_side_data` as the true length of the
/// `side_data` array. **There is no second source for that length** —
/// `AVFrame` publishes the count and the pointer and nothing that could
/// corroborate them — so a count that is positive, within the cap, and
/// larger than the array FFmpeg actually allocated cannot be detected
/// by any amount of care here, and this walk would read past the end.
/// That is exactly the trust
/// [`crate::decoder::copy_frame_props_minimal`] and its byte-summing
/// neighbour already place in the same two fields on every frame this
/// crate transfers, and it is libavutil's own invariant to keep: the
/// pair is written together by `av_frame_new_side_data` and cleared
/// together by `av_frame_unref`. This stage does not widen that
/// boundary; it walks a **shorter** prefix of the same array than the
/// copy that follows it.
///
/// # Safety
/// `frame` must be a live `*const AVFrame`. Reads `nb_side_data`, the
/// `side_data` pointer array, and each entry's `type_` as the `c_int`
/// it is — no `AVFrameSideDataType` is ever materialised from FFmpeg
/// memory, which is the same enum-from-integer law the rest of this
/// crate keeps.
unsafe fn side_data_forbids_scaling(frame: *const AVFrame) -> bool {
  use ffmpeg_next::ffi::AVFrameSideDataType;

  unsafe {
    let count = (*frame).nb_side_data;
    if count == 0 {
      return false;
    }
    let entries = (*frame).side_data;
    if count < 0
      || entries.is_null()
      || count as usize > crate::decoder::HW_COPY_SIDE_DATA_MAX_ENTRIES
    {
      tracing::debug!(
        count,
        cap = crate::decoder::HW_COPY_SIDE_DATA_MAX_ENTRIES,
        "mediadecode-ffmpeg: scaled output stands down — this frame's side-data bookkeeping \
         cannot be walked, so whether a resize would strand any of it is unknowable"
      );
      return true;
    }
    for index in 0..count as usize {
      let entry = *entries.add(index);
      if entry.is_null() {
        tracing::debug!(
          index,
          "mediadecode-ffmpeg: scaled output stands down — a null side-data entry inside a \
           non-empty array is malformed bookkeeping"
        );
        return true;
      }
      let kind_raw = read_unaligned(addr_of!((*entry).type_).cast::<c_int>());
      // Not copied onto the fitted frame at all, so a resize cannot
      // strand it. The whitelist gate runs first for exactly that
      // reason.
      let Some(kind) = crate::decoder::whitelisted_side_data_kind(kind_raw) else {
        continue;
      };
      if matches!(
        kind,
        AVFrameSideDataType::AV_FRAME_DATA_PANSCAN
          | AVFrameSideDataType::AV_FRAME_DATA_SPHERICAL
          | AVFrameSideDataType::AV_FRAME_DATA_REGIONS_OF_INTEREST
      ) {
        tracing::debug!(
          kind_raw,
          "mediadecode-ffmpeg: scaled output stands down — this frame carries side data whose \
           meaning is tied to the picture's dimensions, and a resize would strand it"
        );
        return true;
      }
    }
    false
  }
}

// `#[path]`, because this module is itself reached through one: a
// bare `mod tests;` here resolves against `src/vtscale/` — the
// directory of the file that declared *this* module — and would pick
// up the parent's test file instead of its own.
#[cfg(test)]
#[path = "videotoolbox/tests.rs"]
mod tests;
