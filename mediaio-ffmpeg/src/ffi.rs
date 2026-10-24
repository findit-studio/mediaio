//! FFI shims used by the crossing and by the decoders built on it. Kept in
//! one place so the unsafe surface is easy to audit.
//!
//! All reads of `AVPixelFormat` / `AVHWDeviceType` values returned by FFmpeg
//! at runtime go through `ptr::read::<i32>` after a pointer cast, never
//! through the bindgen-generated Rust enum. The enums are `#[repr(i32)]`
//! and constructing them from a value not in the listed discriminants is
//! undefined behavior — exactly the situation header/library skew creates.
//! See the doc comments on individual functions for what is read as raw
//! integer vs. constructed from a known constant.

use std::{
  ffi::{c_char, c_int, c_uint},
  ptr,
};

use ffmpeg_next::ffi::{
  AV_CODEC_HW_CONFIG_METHOD_HW_DEVICE_CTX, AVCodec, AVCodecContext, AVHWDeviceType, AVPacket,
  AVPacketSideDataType, AVPixelFormat, avcodec_get_hw_config,
};

unsafe extern "C" {
  /// `av_get_pix_fmt_name`, redeclared with a plain `c_int` parameter
  /// instead of `AVPixelFormat`.
  ///
  /// The binding `ffmpeg-sys-next` generates takes the bindgen enum,
  /// and the whole point of calling this function is to name an integer
  /// we could *not* map — i.e. one that may well not be in our build's
  /// discriminant set. Constructing `AVPixelFormat` from such a value to
  /// pass it in would be immediate UB, the exact hazard this module
  /// exists to keep out of the crate. `AVPixelFormat` is `#[repr(i32)]`
  /// and C passes the enum as an `int`, so the redeclared signature is
  /// ABI-identical; only the Rust-side validity obligation is dropped.
  ///
  /// libavutil answers *every* integer, in range or not, with either a
  /// pointer into its static descriptor table or null — it bounds-checks
  /// before indexing. That is the property [`pix_fmt_name`] relies on,
  /// and `pix_fmt_name_refuses_what_ffmpeg_cannot_name` pins it against
  /// the linked library rather than assuming it.
  fn av_get_pix_fmt_name(pix_fmt: c_int) -> *const c_char;
}

unsafe extern "C" {
  /// `av_packet_new_side_data`, redeclared with a plain integer type
  /// parameter instead of `AVPacketSideDataType`.
  ///
  /// Same reason as [`av_get_pix_fmt_name`] above: the value being
  /// passed is a side-data type this crate carries as the raw integer
  /// it is on the wire, and constructing the bindgen enum from it would
  /// be undefined behaviour for any value outside this build's
  /// discriminant set. `AVPacketSideDataType` is `#[repr(u32)]` and C
  /// passes the enum in a register as an `unsigned int`, so the
  /// redeclared signature is ABI-identical; only the Rust-side validity
  /// obligation is dropped.
  ///
  /// Callers must still hand it a type the linked library names —
  /// [`packet_new_side_data`] enforces that, through
  /// [`runtime_names_side_data`] — because FFmpeg's own code compares the
  /// field against its named constants.
  #[link_name = "av_packet_new_side_data"]
  fn av_packet_new_side_data_raw(pkt: *mut AVPacket, kind: c_uint, size: usize) -> *mut u8;

  /// `av_packet_side_data_name`, redeclared with a plain `c_int`
  /// parameter instead of `AVPacketSideDataType`.
  ///
  /// The question it answers is whether the **linked** libavcodec names
  /// a side-data type — which may be one the bindings this crate was
  /// built against do not, when the library it runs against is newer.
  /// Forming `AVPacketSideDataType` from such an integer would be
  /// undefined behaviour, so the integer crosses as the integer it is;
  /// C passes the enum in an `int`-sized register, so the redeclaration
  /// is ABI-identical. libavcodec answers every integer: a `switch` over
  /// the types it names, and null for anything else
  /// (`libavcodec/packet.c` 269–317 in FFmpeg 9.0.1).
  #[link_name = "av_packet_side_data_name"]
  fn av_packet_side_data_name_raw(kind: c_int) -> *const c_char;
}

/// Whether the **linked** libavcodec names side-data type `kind` — asked
/// of the library this process runs against, as the integer the type is,
/// never through the bindings' enum.
///
/// The admission test for a side-data entry this crate hands to C. The
/// read side carries every type as the integer it is on the wire,
/// including one newer than the bindings this crate was built against;
/// what may go back is exactly what the library it runs against knows.
/// A count from the bindings (`AV_PKT_DATA_NB`) is the wrong authority
/// for that: it is the build's, and refused a type the runtime names.
pub(crate) fn runtime_names_side_data(kind: i32) -> bool {
  #[cfg(test)]
  if side_data_namer_stub::names(kind) {
    return true;
  }
  // SAFETY: `av_packet_side_data_name` takes any integer and answers null
  // or a pointer into libavcodec's static strings; only whether the
  // pointer is null is read.
  !unsafe { av_packet_side_data_name_raw(kind) }.is_null()
}

/// A linked library newer than the bindings, for the laws: while a
/// [`side_data_namer_stub::name_also`] guard lives, the namer on its
/// thread names one extra type. The linked FFmpeg 9.0.1 names exactly
/// the types its own bindings do, so without it there is no type above
/// the bindings' count for a law to round-trip.
#[cfg(test)]
pub(crate) mod side_data_namer_stub {
  use core::cell::Cell;

  std::thread_local! {
    static EXTRA: Cell<Option<i32>> = const { Cell::new(None) };
  }

  /// Names `kind` on this thread until the guard drops.
  pub(crate) fn name_also(kind: i32) -> Guard {
    EXTRA.with(|extra| extra.set(Some(kind)));
    Guard(())
  }

  /// Ends [`name_also`]'s extra name.
  pub(crate) struct Guard(());

  impl Drop for Guard {
    fn drop(&mut self) {
      EXTRA.with(|extra| extra.set(None));
    }
  }

  /// Whether the stub names `kind` on this thread.
  pub(crate) fn names(kind: i32) -> bool {
    EXTRA.with(|extra| extra.get() == Some(kind))
  }
}

/// Attaches a side-data buffer of `size` bytes to `pkt`, returning a
/// pointer to it, or `None` when the type is not one the linked FFmpeg
/// names or the allocation failed.
///
/// [`runtime_names_side_data`] is what keeps the raw type integer
/// honest: only a type the library this process runs against names is
/// handed to C, whatever the bindings' own enum says. A type
/// `av_packet_add_side_data` keeps is any integer — it compares
/// entries' types for equality and counts them against its own
/// `AV_PKT_DATA_NB` (`libavcodec/packet.c` 197–228 in FFmpeg 9.0.1) —
/// so an entry newer than the bindings attaches and reads back as the
/// integer it is.
///
/// # Safety
///
/// `pkt` must be a live `*mut AVPacket` for the duration of the call.
pub(crate) unsafe fn packet_new_side_data(
  pkt: *mut AVPacket,
  kind: i32,
  size: usize,
) -> Option<*mut u8> {
  if !runtime_names_side_data(kind) {
    return None;
  }
  // SAFETY: the caller keeps `pkt` live, and `kind` was just proved to
  // be a type the linked library names — so it is not negative, and
  // `as c_uint` keeps its value.
  let ptr = unsafe { av_packet_new_side_data_raw(pkt, kind as c_uint, size) };
  (!ptr.is_null()).then_some(ptr)
}

/// How many side-data types the bindings this crate was built against
/// name — the floor of the side-data entry cap, never an admission test
/// (see [`runtime_names_side_data`]).
#[cfg_attr(not(tarpaulin), inline(always))]
pub(crate) const fn side_data_type_count() -> i32 {
  AVPacketSideDataType::AV_PKT_DATA_NB as i32
}

/// Upper bound on the NUL search in [`pix_fmt_name`]. FFmpeg's longest
/// pixel-format name is around fourteen bytes, so this is generous; the
/// cap exists only so that a corrupt or version-skewed descriptor table
/// cannot turn the walk into an unbounded read, matching the discipline
/// the rest of the crate's FFI text handling follows.
const PIX_FMT_NAME_MAX_BYTES: usize = 64;

/// One of FFmpeg's own static-table strings, copied into text this crate
/// owns — or `None` for a null pointer, a walk that found no terminator
/// within `max_bytes`, or bytes that are not UTF-8.
///
/// **The one bounded C-string reader for FFmpeg's static tables**, and
/// the three roads onto it are the pixel-format namer below, the codec
/// descriptor behind [`crate::CodecId::name`] and the container word
/// behind [`crate::ContainerFormat`]. A bounded search rather than
/// `CStr::from_ptr`: a missing terminator violates that function's
/// precondition outright, and this crate does not hand FFmpeg's word on
/// string lengths to a function that cannot survive being wrong.
///
/// **Copied, not borrowed.** Every one of these pointers really does
/// name a `static const` table entry that outlives any session, so a
/// `&'static str` would be sound today — and it would rest the whole
/// surface on a provenance argument about FFmpeg's internals rather
/// than on this crate's own memory. [`Utf8Bytes`] keeps a codec name or a
/// format word *inline* (up to 22 bytes, which every one of these is
/// bar the longest comma list), so the safe answer is also the free
/// one.
///
/// **Refused, not replaced.** These are FFmpeg's own compiled-in
/// identifiers — ASCII by construction — so a non-UTF-8 read means the
/// pointer was not what it claimed, and `None` says so. That is the
/// opposite of the container *metadata* road, where invalid bytes are
/// replaced (`U+FFFD`) because the bytes there are the *file's* and a
/// legacy codepage is still a fact worth surfacing. Two roads, two
/// answers, because the byte's author is different.
///
/// # Safety
///
/// `ptr` must be null, or point at a NUL-terminated byte string that is
/// **valid for the whole process** — not merely for this call.
///
/// That is a stronger contract than the one this helper used to carry,
/// and it is the one every caller already satisfied: libavformat's
/// `AVInputFormat` entries and libavcodec's `codec_descriptors[]` are
/// `static const` tables compiled into those libraries, and the two
/// `name`/`long_name` fields of each are string literals inside them.
/// Nothing in this crate calls it with a pointer of any other kind.
///
/// # Why the return type changed
///
/// It used to hand back an owned `Utf8Bytes` built from the borrowed
/// bytes, which **allocates** past `smol_bytes::INLINE_CAP` — and
/// FFmpeg's own tables reach past it: the SER demuxer's long name is
/// sixty-five bytes. So opening an ordinary container could allocate,
/// infallibly, to copy a string the process already owns for its whole
/// life. Borrowing is both cheaper and truer; a caller that wants an
/// owned carrier builds one with `Utf8Bytes::from_static`, which stores
/// a static slice rather than copying it.
pub(crate) unsafe fn table_text(ptr: *const c_char, max_bytes: usize) -> Option<&'static str> {
  if ptr.is_null() {
    return None;
  }
  for len in 0..max_bytes {
    // SAFETY: `ptr` is non-null and NUL-terminated per the contract, so
    // the walk reads at most one byte past the last value byte and
    // stops at the terminator.
    if unsafe { *ptr.add(len).cast::<u8>() } == 0 {
      // SAFETY: the `len` bytes below the terminator were just walked,
      // so the slice is in bounds and initialised — and it lives in a
      // table valid for the process, which is what makes the `'static`
      // borrow sound rather than merely convenient.
      let bytes = unsafe { std::slice::from_raw_parts(ptr.cast::<u8>(), len) };
      return std::str::from_utf8(bytes).ok();
    }
  }
  None
}

/// FFmpeg's own name for a raw `AVFrame.format` integer — `"yuv420p"`,
/// `"vaapi"` — or `None` when libavutil has no descriptor for it,
/// **borrowed rather than copied**.
///
/// Diagnostic only. It never feeds a mapping decision: the raw integer
/// is turned into a [`mediaspine::PixelFormat`] elsewhere, and this is
/// what an error message says so a reader can recognise the format
/// without decoding the integer themselves.
///
/// # Why it borrows
///
/// libavutil's format names are string literals in its static
/// `av_pix_fmt_descriptors` table: they live for the process, so there
/// is nothing to own. A copying version stood here and its only
/// consumer was the unsupported-format error — which made an
/// **allocation a precondition of reporting a refusal**, so a
/// container-selected unsupported format could abort the process
/// precisely while the converter was trying to name it. An error that
/// says "this is unsupported" must not depend on the allocator
/// agreeing, and the string it needs was never the allocator's to give.
pub(crate) fn pix_fmt_name_static(raw: i32) -> Option<&'static str> {
  // SAFETY: the redeclaration above takes a plain `c_int`, so no
  // `AVPixelFormat` is constructed from `raw` and no invalid enum value
  // is ever formed. libavutil bounds-checks the index itself and
  // returns null for anything outside its table, so every `i32` —
  // negative, `AV_PIX_FMT_NONE`, or past the end — is a defined call.
  let ptr = unsafe { av_get_pix_fmt_name(raw) };
  if ptr.is_null() {
    return None;
  }
  for len in 0..PIX_FMT_NAME_MAX_BYTES {
    // SAFETY: the table's entries are NUL-terminated literals; the walk
    // reads at most one byte past the last and stops at the terminator.
    if unsafe { *ptr.add(len).cast::<u8>() } == 0 {
      // SAFETY: the `len` bytes below the terminator were just walked,
      // and the table they live in is valid for the process lifetime —
      // which is what makes the `'static` borrow sound rather than
      // merely convenient.
      let bytes = unsafe { std::slice::from_raw_parts(ptr.cast::<u8>(), len) };
      return std::str::from_utf8(bytes).ok();
    }
  }
  None
}

/// State pointed to by `AVCodecContext::opaque` so the `get_format`
/// callback can pick the correct hardware pixel format without globals,
/// and the allocator judge ([`judge_buffer`]) can read its byte budget
/// and leave its refusals. One instance per decoder, installed by
/// [`install_allocator_judge`]: the decoder owns it through the pointer
/// the context holds, and frees it only after that context is freed.
///
/// Its fields are this crate's: a decoder builds one with
/// [`CallbackState::new`], names the hardware format it wants with
/// [`CallbackState::set_wanted`], and collects the refusals through
/// [`take_ceiling_declination`] and [`take_frame_budget_declination`].
///
/// `wanted` is set from a hardcoded `AVPixelFormat` constant in our bindings
/// (via `Backend::hw_pixel_format`), so it is always a valid enum value. We
/// also store its raw `i32` so the callback can compare against the offered
/// list without going through enum reads.
#[repr(C)]
pub struct CallbackState {
  /// Hardware pixel format we want the decoder to produce. Constructed
  /// from a known constant; safe to use as the callback's return value.
  pub(crate) wanted: AVPixelFormat,
  /// Same value as `wanted` cast to `i32`, cached so the callback's
  /// pix_fmts walk doesn't have to convert per iteration.
  pub(crate) wanted_int: i32,
  /// Set by [`get_hw_format`] when it declines this backend because the
  /// **coded** surface is over the frame ceiling.
  ///
  /// A `get_format` callback has no way to return a reason: declining
  /// means returning `AV_PIX_FMT_NONE`, which libavcodec reports as
  /// `Invalid data found when processing input`. That is a true
  /// statement about what libavcodec saw and a false one about what
  /// happened — the data was fine and this crate declined it — so the
  /// reason is left here instead, where the probe machinery can pick it
  /// up and give the caller the name the callback could not.
  pub(crate) ceiling_declined: core::sync::atomic::AtomicBool,
  /// The coded pixel count that was refused, for the message.
  pub(crate) declined_pixels: core::sync::atomic::AtomicI64,
  /// The ceiling it was refused against.
  pub(crate) declined_limit: core::sync::atomic::AtomicI64,
  /// The caller's [`FrameLimits::max_frame_bytes`](crate::FrameLimits::max_frame_bytes),
  /// verbatim.
  ///
  /// **The single source of truth for the allocator judge.** It used to
  /// be recovered from `AVCodecContext.max_pixels`, which is set to
  /// `min(pixel ceiling, byte ceiling / worst-bytes-per-pixel)` — so
  /// when the *pixel* seat was the tighter of the two, `max_pixels`
  /// stopped encoding the byte ceiling at all and the recovery invented
  /// a smaller one. A 256x256 frame at 16 bytes a pixel under
  /// `max_pixels = 65536` and a 2 MiB byte budget satisfies both of the
  /// caller's limits and costs 1,050,624 bytes — and was judged against
  /// 1,048,576 and refused. The recovery also omitted the footprint's
  /// own alignment and slack, which is where those extra 2,048 bytes
  /// come from.
  ///
  /// Carried here instead, so the judge reads the number the caller
  /// actually set.
  pub(crate) max_frame_bytes: u64,
  /// Set by `judge_buffer` when it refuses a **software** allocation
  /// over [`Self::max_frame_bytes`].
  ///
  /// A `get_buffer2` callback can only answer with an errno, and
  /// `AVERROR(EINVAL)` is what libavcodec also reports for corrupt
  /// input — so a caller could not tell a budget refusal this crate
  /// made from a broken file. The reason is left here and collected by
  /// the decoder funnels, exactly as the `get_format` declination is.
  pub(crate) frame_budget_declined: core::sync::atomic::AtomicBool,
  /// What the refused frame would have cost.
  pub(crate) declined_frame_bytes: core::sync::atomic::AtomicU64,
  /// Whether the refused frame was audio (`true`) or a picture.
  pub(crate) declined_frame_audio: core::sync::atomic::AtomicBool,
}

impl CallbackState {
  /// A state with no hardware format wanted yet and `max_frame_bytes` as
  /// the allocator judge's byte budget — the caller's
  /// [`FrameLimits::max_frame_bytes`](crate::FrameLimits::max_frame_bytes),
  /// verbatim.
  pub fn new(max_frame_bytes: u64) -> Self {
    Self {
      wanted: AVPixelFormat::AV_PIX_FMT_NONE,
      wanted_int: AVPixelFormat::AV_PIX_FMT_NONE as i32,
      ceiling_declined: core::sync::atomic::AtomicBool::new(false),
      declined_pixels: core::sync::atomic::AtomicI64::new(0),
      declined_limit: core::sync::atomic::AtomicI64::new(0),
      max_frame_bytes,
      frame_budget_declined: core::sync::atomic::AtomicBool::new(false),
      declined_frame_bytes: core::sync::atomic::AtomicU64::new(0),
      declined_frame_audio: core::sync::atomic::AtomicBool::new(false),
    }
  }

  /// Names the hardware pixel format the `get_format` callback should
  /// pick, keeping the raw integer the callback compares against in
  /// step with it.
  ///
  /// `wanted` is a constant of this build's bindings — a `Backend`'s
  /// hardware format — so it is a valid enum value by construction.
  pub fn set_wanted(&mut self, wanted: AVPixelFormat) {
    self.wanted = wanted;
    self.wanted_int = wanted as i32;
  }
}

/// Reads and clears a software frame-budget refusal, if one was left.
///
/// Clear-on-read for the same reason the `get_format` declination is: a
/// refusal that latched would be reported again against the next frame,
/// which never declined anything.
///
/// # Safety
///
/// `state` must be null — answered `None` — or point at a live, aligned
/// [`CallbackState`](crate::contracts#callbackstate) for the duration of
/// the call. Its frame-budget atomics are read and the latch cleared
/// through shared access, and its `max_frame_bytes` is read. In practice
/// it is the state a decoder owns and frees only after the codec context
/// that points at it.
pub unsafe fn take_frame_budget_declination(
  state: *const CallbackState,
) -> Option<(u64, u64, bool)> {
  use core::sync::atomic::Ordering;
  if state.is_null() {
    return None;
  }
  // SAFETY: `state` is non-null, so by the contract above it points at a
  // `CallbackState` live for this call; every field read is an atomic or
  // a plain integer.
  let (declined, bytes, limit, audio) = unsafe {
    (
      (*state)
        .frame_budget_declined
        .swap(false, Ordering::Acquire),
      (*state).declined_frame_bytes.load(Ordering::Relaxed),
      (*state).max_frame_bytes,
      (*state).declined_frame_audio.load(Ordering::Relaxed),
    )
  };
  declined.then_some((bytes, limit, audio))
}

/// Reads and clears a coded-surface refusal the `get_format` callback
/// left, if it left one: the coded pixel count it refused and the
/// ceiling it was refused against.
///
/// Clear-on-read for the reason [`take_frame_budget_declination`] is: the
/// state outlives one hardware candidate, so a refusal that latched would
/// be reported again against the next backend.
///
/// # Safety
///
/// `state` must be null — answered `None` — or point at a live, aligned
/// [`CallbackState`](crate::contracts#callbackstate) for the duration of
/// the call. Its ceiling atomics are read and the latch cleared through
/// shared access. In practice it is the state a decoder owns and frees
/// only after the codec context that points at it.
pub unsafe fn take_ceiling_declination(state: *const CallbackState) -> Option<(i64, i64)> {
  use core::sync::atomic::Ordering;
  if state.is_null() {
    return None;
  }
  // SAFETY: `state` is non-null, so by the contract above it points at a
  // `CallbackState` live for this call; every field read is an atomic.
  let (declined, pixels, limit) = unsafe {
    (
      (*state).ceiling_declined.swap(false, Ordering::Acquire),
      (*state).declined_pixels.load(Ordering::Relaxed),
      (*state).declined_limit.load(Ordering::Relaxed),
    )
  };
  declined.then_some((pixels, limit))
}

/// Latches an allocator-judge frame-budget refusal, as `judge_buffer`
/// does when a decoded frame would cost more than the caller's ceiling.
///
/// # Safety
///
/// `state` must be null — ignored — or point at a live, aligned
/// [`CallbackState`](crate::contracts#callbackstate) for the duration of
/// the call. Only its atomics are written, through shared access, exactly
/// as the callback writes them.
#[cfg(any(test, feature = "test-support"))]
pub unsafe fn declare_frame_budget_declined_for_test(state: *mut CallbackState, bytes: u64) {
  use core::sync::atomic::Ordering;
  if state.is_null() {
    return;
  }
  // SAFETY: `state` is non-null, so by the contract above it is live for
  // the call; only its atomics are written.
  unsafe {
    (*state)
      .declined_frame_bytes
      .store(bytes, Ordering::Relaxed);
    (*state)
      .declined_frame_audio
      .store(false, Ordering::Relaxed);
    (*state)
      .frame_budget_declined
      .store(true, Ordering::Release);
  }
}

/// `AVCodecContext::get_format` callback. FFmpeg invokes it with the list of
/// pixel formats the codec is willing to output for the current stream.
///
/// The offered list is walked as `*const i32` (cast from `*const AVPixelFormat`)
/// to avoid constructing the bindgen enum from values that may not be in our
/// build's discriminant set. The return value is either `wanted` (a known
/// constant) or `AV_PIX_FMT_NONE` (also a known constant) — both safe to
/// produce as `AVPixelFormat`.
///
/// # A missing hardware format says nothing about why
///
/// When `wanted` is not on offer the callback declines and records
/// nothing, because the list holds no cause to record. `ff_get_format`
/// withdraws a hardware format after **any** failed hwaccel setup and
/// asks again without it (`libavcodec/decode.c` 1341–1343 and 1348–1357
/// in FFmpeg 9.0.1): a failed allocation of the hwaccel's private data
/// (1194–1198) and the backend's own setup error (1202–1211) arrive as
/// the same shorter list. The codec then fails the picture in its own
/// words, and that error is routed as any other — a failed candidate
/// while the probe runs, the picture's own error after the first
/// picture.
///
/// # Safety
///
/// This is libavcodec's to call, installed as a context's `get_format`. For
/// the duration of the call, `ctx` must point at a live, aligned
/// `AVCodecContext` that meets [the callbacks' `AVCodecContext`
/// contract](crate::contracts#avcodeccontext-for-the-callbacks): its
/// `opaque` is null or the [`CallbackState`](crate::contracts#callbackstate)
/// [`install_allocator_judge`] installed — `wanted`, `wanted_int` and
/// `max_frame_bytes` read, the ceiling atomics written — and its
/// `hw_device_ctx` is null or a live device reference, handed to
/// `avcodec_get_hw_frames_parameters`, whose frames context is read
/// (`width`, `height`, `sw_format`) and released inside the call.
/// `pix_fmts` must point at a list of `AVPixelFormat` values, read as
/// integers, terminated by `AV_PIX_FMT_NONE` — what libavcodec passes.
/// A caller other than libavcodec owes the same.
pub unsafe extern "C" fn get_hw_format(
  ctx: *mut AVCodecContext,
  pix_fmts: *const AVPixelFormat,
) -> AVPixelFormat {
  debug_assert!(!ctx.is_null());
  debug_assert!(!pix_fmts.is_null());

  // SAFETY: opaque is null or the `CallbackState` `install_allocator_judge`
  // installed, which its owner frees only after the codec context (the
  // contract above). When opaque is null we treat the call as strict — a
  // stray invocation cannot silently downgrade.
  let state = unsafe { (*ctx).opaque as *const CallbackState };
  let (wanted, wanted_int) = if state.is_null() {
    (
      AVPixelFormat::AV_PIX_FMT_NONE,
      AVPixelFormat::AV_PIX_FMT_NONE as i32,
    )
  } else {
    unsafe { ((*state).wanted, (*state).wanted_int) }
  };

  // Walk the offered list as i32. The pointer cast is sound because
  // `AVPixelFormat` is `#[repr(i32)]` (same size and alignment as i32).
  // Reading as i32 cannot be UB regardless of the value FFmpeg wrote.
  let mut p = pix_fmts as *const i32;
  let none_int = AVPixelFormat::AV_PIX_FMT_NONE as i32;
  loop {
    // SAFETY: FFmpeg guarantees the list is terminated by AV_PIX_FMT_NONE.
    // We bail at the sentinel; reads up to and including it are in-bounds.
    let v = unsafe { ptr::read(p) };
    if v == none_int {
      // The hardware format is not on offer, and the list cannot say
      // why: a withdrawn format means only that the hwaccel's setup
      // failed. Nothing is recorded — see the function's documentation.
      return AVPixelFormat::AV_PIX_FMT_NONE;
    }
    if v == wanted_int {
      // **The coded-dimension seat.** This is the last moment before
      // the hardware frames pool is built, and the first at which the
      // *coded* extent is known — which is the extent that gets
      // allocated, and not the one `max_pixels` was checked against.
      //
      // `ff_set_dimensions` applies `av_image_check_size2` to the dims
      // the decoder passes it, and for a cropped stream those are the
      // **display** dims. Measured on this build with an h264 stream
      // carrying SPS cropping of 32x32 out of a 1920x1088 macroblock
      // grid — a 2040x divergence: `max_pixels = 5000` admits it,
      // because 32x32 is 1024 pixels. The surface behind it is
      // 2,088,960.
      //
      // On the software road that gap is already closed, and this was
      // measured too: `get_buffer2` receives the frame at **coded**
      // dims (1920x1088, aligned to 1920x1090, a 2 MiB allocation), so
      // `judge_buffer` bounds the real extent. The hardware road never
      // reaches `get_buffer2` at all — `ff_get_buffer` goes to
      // `hwaccel->alloc_frame` — which leaves this callback as the only
      // place the same number can be applied to the same extent.
      //
      // Refused by returning `AV_PIX_FMT_NONE`: that is already this
      // callback's vocabulary for "not this backend", and the machinery
      // around it already answers by falling back to software — where
      // `judge_buffer` applies the very same ceiling to the very same
      // coded dimensions. One number, both roads, and no refusal mode
      // invented for the occasion.
      // SAFETY: `state` is the live `CallbackState` this crate put in
      // `opaque`; it outlives the codec context. A null one means a
      // context this crate did not build, and gets no budget to judge
      // against.
      let budget = if state.is_null() {
        u64::MAX
      } else {
        unsafe { (*state).max_frame_bytes }
      };
      if let Some((cost, limit)) = unsafe { coded_extent_over_ceiling(ctx, wanted, budget) } {
        if !state.is_null() {
          use core::sync::atomic::Ordering;
          // SAFETY: as above.
          unsafe {
            (*state)
              .declined_pixels
              .store(i64::try_from(cost).unwrap_or(i64::MAX), Ordering::Relaxed);
            (*state)
              .declined_limit
              .store(i64::try_from(limit).unwrap_or(i64::MAX), Ordering::Relaxed);
            (*state).ceiling_declined.store(true, Ordering::Release);
          }
        }
        return AVPixelFormat::AV_PIX_FMT_NONE;
      }
      return wanted;
    }
    p = unsafe { p.add(1) };
  }
}

/// Whether this context's **coded** extent, aligned as the allocator
/// will align it, exceeds the pixel ceiling the context carries.
///
/// Reads `max_pixels` off the context — the field the crate set itself,
/// already carrying the byte ceiling converted at the worst per-pixel
/// rate — so there is no state to thread and no second number to keep
/// in step. The same read `judge_buffer` does on the software road.
///
/// Conservative when the context declares no coded extent: a zero or
/// negative dimension is nothing to judge, and refusing it here would
/// reject streams whose dimensions arrive later.
///
/// # Safety
///
/// `ctx` must be the live `AVCodecContext` FFmpeg passed to the
/// callback.
unsafe fn coded_extent_over_ceiling(
  ctx: *mut AVCodecContext,
  wanted: AVPixelFormat,
  budget: u64,
) -> Option<(u64, u64)> {
  // **Ask the pool what it will be, and refuse it if it will not say.**
  // `avcodec_get_hw_frames_parameters` builds the `AVHWFramesContext`
  // the decoder is about to initialise — fully populated and *not yet*
  // allocated — so its `width`/`height` are the pool's own declaration
  // of its extent and its `sw_format` is the layout that extent is
  // stored in.
  //
  // There used to be a fallback here: when the query failed, the judge
  // computed the codec's own aligned dimensions instead. That fallback
  // was unsound on its face, and the comment beside it said so — a
  // hardware pool aligns by its own API's rules, and D3D11 HEVC and AV1
  // round both dimensions to 128, so codec alignment can answer
  // *smaller* than the pool. A conservative fallback that can
  // under-state is not conservative; it is a hole with a comment on it.
  //
  // So this seat takes the rule the transfer judge already keeps: **a
  // pool that will not declare itself is a pool that cannot be judged,
  // and an unprovable extent is not a small one.** Refusing here
  // declines the hardware format, which falls the decode back to
  // software — where `judge_buffer` applies the same budget to the
  // frame it can see.
  //
  // SAFETY: `ctx` is the live context; the out-parameter is a fresh
  // null pointer FFmpeg fills with a reference we unref before
  // returning, and the device reference is borrowed, not consumed.
  let device_ref = unsafe { (*ctx).hw_device_ctx };
  if device_ref.is_null() {
    // No device means no hardware frames pool will be built at all, so
    // there is nothing for this seat to guard. That is a different
    // thing from a pool declining to describe itself, which is refused
    // below — this crate's hardware road always attaches a device
    // before opening, so reaching here means the caller is not on it.
    return None;
  }
  let mut frames_ref: *mut ffmpeg_next::ffi::AVBufferRef = core::ptr::null_mut();
  // **The format being negotiated, not `ctx->pix_fmt`.** At
  // `get_format` time the context still carries the software format;
  // asking about that one answers `ENOENT`. Measured: with the
  // hardware format the query returns the pool's own 32x32
  // declaration; with `ctx->pix_fmt` it returns -2.
  let rc = unsafe {
    ffmpeg_next::ffi::avcodec_get_hw_frames_parameters(ctx, device_ref, wanted, &mut frames_ref)
  };
  if rc < 0 || frames_ref.is_null() {
    return Some((u64::MAX, budget));
  }
  // SAFETY: on success `frames_ref` holds a live, uninitialised
  // `AVHWFramesContext`. `width`/`height` are plain `c_int`s;
  // `sw_format` is read as the integer it is, never as an enum.
  let (pool_w, pool_h, sw_format) = unsafe {
    let fc = (*frames_ref).data as *const ffmpeg_next::ffi::AVHWFramesContext;
    (
      (*fc).width,
      (*fc).height,
      core::ptr::read(core::ptr::addr_of!((*fc).sw_format) as *const libc::c_int),
    )
  };
  // SAFETY: the reference is ours to release and is released once.
  unsafe { ffmpeg_next::ffi::av_buffer_unref(&mut frames_ref) };

  let Some(cost) = pool_bytes(sw_format, pool_w, pool_h) else {
    // Dimensions that are not a picture: nothing here can be priced,
    // and nothing here will guess.
    return Some((u64::MAX, budget));
  };
  if cost > budget {
    tracing::warn!(
      pool_width = pool_w,
      pool_height = pool_h,
      cost,
      budget,
      "hwdecode: the hardware surface pool exceeds the frame ceiling; declining the \
       hardware format so the decode falls back to software, where the same ceiling applies"
    );
    return Some((cost, budget));
  }
  None
}

/// What a pool of `w` x `h` in `sw_format` costs.
///
/// Priced through the allocator-parity footprint when libavutil can
/// size the layout, and through
/// [`crate::footprint::video_frame_bytes_upper_bound`] when it cannot —
/// which applies the same dimension alignment and per-plane overhead at
/// the widest per-pixel rate, so it dominates every layout the build
/// could have priced. The bare `w * h * 16` this replaced omitted both
/// and could land *below* the accurate path.
///
/// `None` when the dimensions are not a picture, which callers treat as
/// unjudgeable rather than free.
fn pool_bytes(sw_format: libc::c_int, w: libc::c_int, h: libc::c_int) -> Option<u64> {
  crate::footprint::video_frame_bytes(sw_format, w, h)
    .or_else(|| crate::footprint::video_frame_bytes_upper_bound(w, h))
    .map(|bytes| bytes as u64)
}

/// Walk the codec's `AVCodecHWConfig` table and return whether the codec
/// advertises support for `device_type` **with** `wanted_pix_fmt` via the
/// `HW_DEVICE_CTX` setup method.
///
/// FFmpeg's HW config table is keyed per (device_type, pix_fmt) pair: a
/// codec can advertise the same device with several different hardware
/// pixel formats (e.g. VAAPI codecs that offer both `AV_PIX_FMT_VAAPI`
/// and `AV_PIX_FMT_DRM_PRIME`). Matching only on `device_type` would let
/// us proceed to install a strict `get_format` callback for a format the
/// codec never advertises, and the failure would surface deep inside the
/// probe / decode path instead of up front. Requiring the codec to
/// advertise the **exact** pix_fmt our `Backend` uses keeps the strict
/// `get_format` honest and gives `open_with` a clean rejection signal.
///
/// All reads from the FFmpeg-supplied `AVCodecHWConfig` are performed as
/// raw integers via `addr_of!` + `ptr::read::<i32>` to avoid copying or
/// interpreting enum-typed fields whose runtime values might not match
/// our build's discriminant set.
///
/// # Safety
///
/// `codec` must point at a live `AVCodec` — one libavcodec handed out,
/// such as `avcodec_find_decoder`'s answer, which names an entry in its
/// static codec table and stays valid for the process ([the `AVCodec`
/// contract](crate::contracts#avcodec)). Its hardware configurations are
/// read through `avcodec_get_hw_config`, each one's `methods`,
/// `device_type` and `pix_fmt` as integers.
pub unsafe fn codec_supports_hwaccel(
  codec: *const AVCodec,
  device_type: AVHWDeviceType,
  wanted_pix_fmt: i32,
) -> bool {
  debug_assert!(!codec.is_null());
  let device_type_int = device_type as i32;
  let mut i = 0;
  loop {
    // SAFETY: `codec` is a live `AVCodec` per the contract above, and
    // `avcodec_get_hw_config` returns null past the end of its table; we
    // stop then.
    let cfg = unsafe { avcodec_get_hw_config(codec, i) };
    if cfg.is_null() {
      return false;
    }
    // Read each field as raw integer rather than copying the whole struct
    // (which would interpret `pix_fmt` and `device_type` as their enum types).
    // SAFETY: `cfg` is non-null and points to a valid `AVCodecHWConfig` for
    // the lifetime of the call; `addr_of!` projects to a sized field; the
    // `*const i32` cast is sound because `methods` is `c_int` (i32),
    // `device_type` is `AVHWDeviceType` (`#[repr(u32)]`, but FFmpeg's
    // assigned values fit in i32 and the runtime layout is i32-sized),
    // and `pix_fmt` is `AVPixelFormat` (`#[repr(i32)]`).
    let methods: i32 = unsafe { ptr::read(ptr::addr_of!((*cfg).methods)) };
    let cfg_device_type_int: i32 =
      unsafe { ptr::read(ptr::addr_of!((*cfg).device_type) as *const i32) };
    let cfg_pix_fmt_int: i32 = unsafe { ptr::read(ptr::addr_of!((*cfg).pix_fmt) as *const i32) };

    if methods & (AV_CODEC_HW_CONFIG_METHOD_HW_DEVICE_CTX as i32) != 0
      && cfg_device_type_int == device_type_int
      && cfg_pix_fmt_int == wanted_pix_fmt
    {
      return true;
    }
    i += 1;
  }
}

/// Latches a coded-surface refusal exactly as the `get_format` callback
/// does, so a lane can exercise the funnels that collect it without
/// needing a hardware backend to decline a real surface.
///
/// # Safety
///
/// `state` must be null — ignored — or point at a live, aligned
/// [`CallbackState`](crate::contracts#callbackstate) for the duration of
/// the call. Only its atomics are written, through shared access, exactly
/// as the callback writes them.
#[cfg(any(test, feature = "test-support"))]
pub unsafe fn declare_ceiling_declined_for_test(
  state: *mut CallbackState,
  pixels: i64,
  limit: i64,
) {
  use core::sync::atomic::Ordering;
  if state.is_null() {
    return;
  }
  // SAFETY: `state` is non-null, so by the contract above it is live for
  // the call; only its atomics are written.
  unsafe {
    (*state).declined_pixels.store(pixels, Ordering::Relaxed);
    (*state).declined_limit.store(limit, Ordering::Relaxed);
    (*state).ceiling_declined.store(true, Ordering::Release);
  }
}

/// Local FFI shims: FFmpeg entry points re-declared with `c_int` where
/// the generated bindings use a closed Rust enum.
///
/// Constructing `AVCodecID` / `AVPixelFormat` / `AVSampleFormat` from a
/// runtime integer that is not in this build's discriminant set is UB —
/// and these are open C enums that FFmpeg extends in ABI-compatible
/// releases. Declaring the same C symbol with `c_int` sidesteps the
/// boundary entirely: both Rust declarations resolve to the same symbol
/// at link time, and the integer never becomes an enum on the Rust
/// side.
///
/// **This is the enum class inside this crate's own code.** The
/// dependency-API sweep closed every place *ffmpeg-next* formed an enum
/// out of FFmpeg memory; these three are places the crate's own new
/// code did the same thing. The census that walks the pixel-format
/// table to price the worst format is the sharpest instance: it exists
/// precisely to be correct about formats this build does not name, and
/// the binding it called returned those very ids as a closed
/// `AVPixelFormat`. Every id would have become an invalid enum value on
/// the way *into* the pricing that was supposed to handle it.
///
/// **Crate-private.** A raw declaration has no contract a caller could
/// meet by reading it; what `mediadecode-ffmpeg` needs from here goes
/// through the documented doors below — [`find_decoder_raw`] and
/// [`visit_hwframe_download_formats`].
pub(crate) mod c_shims {
  use libc::c_int;

  use super::AVCodec;

  unsafe extern "C" {
    /// `AVCodecID` as `c_int`.
    pub(crate) fn avcodec_find_decoder(id: c_int) -> *const AVCodec;

    /// `AVCodecID` as `c_int`, answering the descriptor libavcodec keeps
    /// for that id — or null where this build names no codec for it.
    ///
    /// The pointer is into `codec_descriptors[]`, a `static const` table
    /// compiled into libavcodec, so the strings it names are live and
    /// unwritten for the process — the contract `crate::ffi`'s bounded
    /// reader is given. It is left **raw** at every call site: the
    /// struct embeds an `AVCodecID` and an `AVMediaType`, and forming a
    /// reference to it would assert two bindgen enums valid on a table
    /// that belongs to the linked library rather than to the bindings.
    /// See `crate::CodecId::descriptor`.
    ///
    /// Preferred over `avcodec_get_name`, which never answers null: for
    /// an id it cannot place it returns the string `"unknown_codec"`,
    /// a sentinel wearing a name's clothes. A descriptor that is absent
    /// is `None` here, and a consumer can tell the two apart.
    pub(crate) fn avcodec_descriptor_get(id: c_int) -> *const ffmpeg_next::ffi::AVCodecDescriptor;

    /// Returns `AVPixelFormat` as `c_int` — the id of a descriptor that
    /// may well name a format this build's bindings do not.
    pub(crate) fn av_pix_fmt_desc_get_id(
      desc: *const ffmpeg_next::ffi::AVPixFmtDescriptor,
    ) -> c_int;

    /// Takes `AVPixelFormat` as `c_int`, so an id straight out of
    /// [`av_pix_fmt_desc_get_id`] can be priced without ever being an
    /// enum.
    pub(crate) fn av_image_get_buffer_size(
      pix_fmt: c_int,
      width: c_int,
      height: c_int,
      align: c_int,
    ) -> c_int;

    /// Takes `AVSampleFormat` as `c_int`. Kept for the footprint
    /// sweep, which walks the format table to decide which cells
    /// exist; production pricing goes through
    /// `av_samples_get_buffer_size`, the allocator's own ruler.
    #[cfg(test)]
    pub(crate) fn av_get_bytes_per_sample(sample_fmt: c_int) -> c_int;

    /// The allocator's own audio ruler, with `AVSampleFormat` as
    /// `c_int`. `align = 0` asks for the alignment
    /// `av_frame_get_buffer` itself uses.
    pub(crate) fn av_samples_get_buffer_size(
      linesize: *mut c_int,
      nb_channels: c_int,
      nb_samples: c_int,
      sample_fmt: c_int,
      align: c_int,
    ) -> c_int;

    /// Writes an `AV_PIX_FMT_NONE`-terminated list of destination
    /// formats a transfer may produce. Declared `*mut *mut c_int` so
    /// the list is walked as integers — a driver may well offer a
    /// format this build's bindings do not name.
    pub(crate) fn av_hwframe_transfer_get_formats(
      hwframe_ctx: *mut ffmpeg_next::ffi::AVBufferRef,
      dir: c_int,
      formats: *mut *mut c_int,
      flags: c_int,
    ) -> c_int;

    /// `AVCodecID` as `c_int`: whether a muxer can store a codec — `1`
    /// yes, `0` no, negative when the muxer cannot tell. libavformat
    /// answers every integer (its tag tables are searched, never
    /// indexed).
    pub(crate) fn avformat_query_codec(
      ofmt: *const ffmpeg_next::ffi::AVOutputFormat,
      codec_id: c_int,
      std_compliance: c_int,
    ) -> c_int;

    /// `AVCodecID` as `c_int`: the tag a muxer's table gives a codec,
    /// written to `tag`; answers `0` when the table names none.
    pub(crate) fn av_codec_get_tag2(
      tags: *const *const ffmpeg_next::ffi::AVCodecTag,
      id: c_int,
      tag: *mut libc::c_uint,
    ) -> c_int;

    /// Returns `AVCodecID` as `c_int`: the codec a muxer's table names
    /// for a tag — `AV_CODEC_ID_NONE` where none — so an id this build's
    /// bindings do not name is never formed into the enum.
    pub(crate) fn av_codec_get_id(
      tags: *const *const ffmpeg_next::ffi::AVCodecTag,
      tag: libc::c_uint,
    ) -> c_int;

    /// `AVCodecID` as `c_int`: the bits one sample of the codec takes
    /// where its samples have a fixed size, else `0` — libavcodec answers
    /// every integer from a `switch` over the ids it names.
    pub(crate) fn av_get_bits_per_sample(codec_id: c_int) -> c_int;

    /// `AVPacketSideDataType` as `c_int`: libavcodec's name for a packet
    /// side-data type, or null for a type it does not name — answered for
    /// every integer from a `switch` over the types it names
    /// (`libavcodec/packet.c` 269–317 in FFmpeg 9.0.1). Each name is a
    /// string literal of libavcodec's, live and unwritten for the process.
    pub(crate) fn av_packet_side_data_name(kind: c_int) -> *const libc::c_char;

    /// `libavutil/hdr_dynamic_metadata.h`, which ffmpeg-sys-next 9.0.1
    /// binds no part of: a zeroed `AVDynamicHDRPlus` of the linked
    /// libavutil's own size, which it writes to `size` — 0 with a failed
    /// allocation, answered as null (`hdr_dynamic_metadata.c` 36–44 in
    /// FFmpeg 9.0.1). The struct stays opaque here: the pointer is read as
    /// bytes and freed with `av_free`, never formed into a Rust type.
    pub(crate) fn av_dynamic_hdr_plus_alloc(size: *mut usize) -> *mut libc::c_void;

    /// As [`av_dynamic_hdr_plus_alloc`], for `AVDynamicHDRSmpte2094App5`
    /// (`hdr_dynamic_metadata.c` 397–405).
    pub(crate) fn av_dynamic_hdr_smpte2094_app5_alloc(size: *mut usize) -> *mut libc::c_void;

    /// Parses an ITU-T T.35 HDR10+ payload of `size` bytes into the struct
    /// `s` points at, answering 0 or a negative code
    /// (`hdr_dynamic_metadata.c` 59–235): it refuses a payload past
    /// `AV_HDR_PLUS_MAX_PAYLOAD_SIZE` bytes, copies the rest into a padded
    /// buffer of its own, and writes only the struct's fields.
    pub(crate) fn av_dynamic_hdr_plus_from_t35(
      s: *mut libc::c_void,
      data: *const u8,
      size: usize,
    ) -> c_int;

    /// As [`av_dynamic_hdr_plus_from_t35`], for SMPTE ST 2094 application
    /// 5 (`hdr_dynamic_metadata.c` 429–561), which copies the payload into
    /// a padded buffer of its own first.
    pub(crate) fn av_dynamic_hdr_smpte2094_app5_from_t35(
      s: *mut libc::c_void,
      data: *const u8,
      size: usize,
    ) -> c_int;
  }
}

/// The decoder libavcodec registers for a codec id, asked as the
/// integer the id is on the wire — never formed into `AVCodecID`, whose
/// discriminant set is this build's and may not name it.
///
/// `None` where libavcodec names no decoder for the id. Any integer is
/// a valid question: `avcodec_find_decoder` compares ids against its own
/// codec table and reads nothing else. A pointer that comes back names
/// an entry of that static table, live and unwritten for the process —
/// what [`codec_supports_hwaccel`] and `ffmpeg_next::Codec::wrap` ask
/// for.
pub fn find_decoder_raw(id: c_int) -> Option<ptr::NonNull<AVCodec>> {
  // SAFETY: `avcodec_find_decoder` takes any integer and answers null or
  // a pointer into libavcodec's static codec table.
  ptr::NonNull::new(unsafe { c_shims::avcodec_find_decoder(id) }.cast_mut())
}

/// Visits the formats a hardware frame can be downloaded to — the
/// `AV_HWFRAME_TRANSFER_DIRECTION_FROM` list of its frames context —
/// each as the integer it is, since a driver may offer a format this
/// build's bindings do not name, until `visit` breaks.
///
/// `Ok(Some(_))` carries what `visit` broke with, `Ok(None)` means the
/// list was walked to its end (or libavutil listed nothing), and `Err`
/// carries libavutil's own error code. The list libavutil allocates is
/// freed on every road out — `visit` panicking included: a guard owns it
/// from the moment it exists, and frees it as the stack unwinds. On
/// `Err` there is no list (libavutil stores one only on success), and
/// nothing is freed.
///
/// # Safety
///
/// For the duration of the call, `frames_ctx` must point at a live
/// `AVBufferRef` whose `data` is an `AVHWFramesContext` — the reference
/// a hardware frame's `hw_frames_ctx` holds. libavutil reads the frames
/// context, and the device behind it, to build the list; the list is
/// `AV_PIX_FMT_NONE`-terminated by libavutil's contract, and this reads
/// it up to and including the terminator.
pub unsafe fn visit_hwframe_download_formats<B>(
  frames_ctx: *mut ffmpeg_next::ffi::AVBufferRef,
  mut visit: impl FnMut(c_int) -> core::ops::ControlFlow<B>,
) -> Result<Option<B>, c_int> {
  let mut list: *mut c_int = ptr::null_mut();
  // `AV_HWFRAME_TRANSFER_DIRECTION_FROM` is 0 — passed as the integer it
  // is, like every other open C enum on this road.
  //
  // SAFETY: `frames_ctx` per the contract above; on success libavutil
  // stores a freshly allocated, terminated list in `list`.
  let rc = unsafe { c_shims::av_hwframe_transfer_get_formats(frames_ctx, 0, &mut list, 0) };
  if rc < 0 {
    // libavutil stores a list only on success, so there is none to free.
    return Err(rc);
  }
  if list.is_null() {
    return Ok(None);
  }
  // **Owned before anything can unwind.** `visit` is the caller's code
  // and may panic; a free placed after the loop would be skipped by the
  // unwinding stack, where this guard's `Drop` is not.
  let list = FormatList(list);
  let none = AVPixelFormat::AV_PIX_FMT_NONE as c_int;
  let mut cursor = list.0.cast_const();
  loop {
    // SAFETY: the list is terminated by `AV_PIX_FMT_NONE`, and `cursor`
    // has not moved past the terminator: it advances only after a
    // non-terminator element. The guard keeps the list alive until the
    // function returns.
    let candidate = unsafe { cursor.read() };
    if candidate == none {
      return Ok(None);
    }
    if let core::ops::ControlFlow::Break(value) = visit(candidate) {
      return Ok(Some(value));
    }
    // SAFETY: `candidate` was not the terminator, so the list holds at
    // least one more element.
    cursor = unsafe { cursor.add(1) };
  }
}

/// The list `av_hwframe_transfer_get_formats` allocated, owned: freed
/// when this drops, on every road out of
/// [`visit_hwframe_download_formats`], a panic unwinding through it
/// included.
struct FormatList(*mut c_int);

impl Drop for FormatList {
  fn drop(&mut self) {
    #[cfg(test)]
    format_list_probe::freed();
    // SAFETY: the list was allocated by `av_hwframe_transfer_get_formats`
    // (with `av_malloc`), this guard is its only owner, and it is freed
    // exactly once, here.
    unsafe { ffmpeg_next::ffi::av_freep(ptr::addr_of_mut!(self.0).cast()) };
  }
}

/// Counts the download-format lists freed on this thread, so a law can
/// see the guard free one while a panic unwinds. FFmpeg keeps no count
/// of live allocations a law could read, and the allocation cap
/// (`test_support::cap_ffmpeg_allocations`) bounds one allocation's size
/// rather than counting any, so the guard reports its own frees.
#[cfg(test)]
pub(crate) mod format_list_probe {
  use core::cell::Cell;

  std::thread_local! {
    static FREED: Cell<u32> = const { Cell::new(0) };
  }

  /// Records one free.
  pub(crate) fn freed() {
    FREED.with(|freed| freed.set(freed.get() + 1));
  }

  /// The frees recorded on this thread so far.
  pub(crate) fn frees() -> u32 {
    FREED.with(Cell::get)
  }
}

/// `AVCodecContext.get_buffer2`: the same pixel ceiling, applied where
/// the **aligned** dimensions are knowable.
///
/// # The hole this closes
///
/// `max_pixels` is checked by libavcodec against the frame's *raw*
/// `width * height`. What it then allocates is the **aligned** shape —
/// `avcodec_align_dimensions2` rounds both dimensions up to whatever
/// the codec and the CPU want — and for degenerate aspect ratios those
/// are not the same number at all. Measured on this build:
///
/// | shape | raw | aligned | inflation |
/// |---|---|---|---|
/// | `gray8` 65536x1 | 65,536 px / 64 KiB | 65536x32 = 2,097,152 px / 2 MiB | **32x** |
/// | `gray8` 1x65536 | 65,536 px / 64 KiB | 16x65536 = 1,048,576 px / 2 MiB | 16x |
/// | `yuv420p` 7680x4320 | 33,177,600 px | 7680x4320 | 1.00x |
/// | `gray8` 1024x1024 | 1,048,576 px | 1024x1024 | 1.00x |
///
/// So a one-pixel-tall frame slips 32 times its declared cost past a
/// scalar compared against `w * h`, and no value of that scalar can fix
/// it: bounding the product cannot bound a product whose factors are
/// then rounded up independently. Real pictures inflate by nothing at
/// all, which is why the ceiling looked sound.
///
/// # Why this hook and not `get_format`
///
/// `get_format` was measured first, because it needs no allocation
/// decision and receives the context. It **does not fire on every
/// road**: on this build a one-shot `mjpeg` decode calls it once and a
/// `png` decode calls it *zero* times. Cover art is overwhelmingly
/// mjpeg or png, so half the road this ceiling exists to guard would
/// have been unguarded.
///
/// `get_buffer2` fired on both — it is the allocator, so every frame
/// libavcodec hands back comes through it, and it sees the frame's
/// *real* format rather than a negotiated candidate.
///
/// # Its state
///
/// The byte budget is not recoverable from any field of the context, so
/// the callback reads it from the [`CallbackState`] behind `opaque` —
/// the caller's own `max_frame_bytes`, verbatim — and records a refusal
/// there for the decoder to name. The callback and its state are
/// installed together, by [`install_allocator_judge`], and only there:
/// the callback is this crate's, so no context can carry it without the
/// state it reads.
///
/// Panic discipline is likewise structural rather than asserted: the
/// body allocates nothing, indexes nothing, unwraps nothing, and calls
/// exactly three FFmpeg functions. There is no Rust operation in it
/// that can panic, and an `extern "C"` function aborts rather than
/// unwinding into C in any case.
///
/// # Safety
///
/// This is libavcodec's to call, as a context's `get_buffer2`, once
/// [`install_allocator_judge`] has installed it:
///
/// * `ctx` must be a live `AVCodecContext`, and its `opaque` must be null
///   or point at a live, aligned [`CallbackState`] for the whole call —
///   the state installed with the callback;
/// * `frame` must be a live `AVFrame` carrying what libavcodec sets
///   before it asks for buffers: `format`, and `width` and `height` for a
///   picture or `nb_samples` and `ch_layout` for audio — which is also
///   what `avcodec_default_get_buffer2`, to which every accepted frame is
///   delegated, requires.
pub(crate) unsafe extern "C" fn judge_buffer(
  ctx: *mut ffmpeg_next::ffi::AVCodecContext,
  frame: *mut ffmpeg_next::ffi::AVFrame,
  flags: libc::c_int,
) -> libc::c_int {
  // SAFETY: libavcodec passes a live context and frame; both fields are
  // plain integers.
  let (width, height) = unsafe { ((*frame).width, (*frame).height) };

  // **This callback judges cost, and only cost.**
  //
  // `max_pixels` is a *logical* limit on a picture's extent, and
  // libavcodec already enforces it — against the **raw** dimensions, in
  // `ff_set_dimensions` via `av_image_check_size2`, before any frame
  // exists. That is the semantics the caller asked for and the
  // semantics FFmpeg documents, and this callback does not restate it.
  //
  // It used to. R11 added an *aligned*-dimension comparison here
  // against `max_pixels`, because at the time the callback had no
  // accurate byte check and a degenerate shape could slip its real cost
  // past a raw-pixel gate — 65536x1 aligns to 65536x32, thirty-two
  // times the pixels. That instrument is now both **redundant** and
  // **wrong**:
  //
  // * redundant, because since the byte ceiling was threaded in the
  //   footprint below prices the aligned dimensions itself, so the
  //   degenerate shape is refused on its actual cost; and
  // * wrong, because `max_pixels` is `min(the caller's pixel limit,
  //   byte ceiling / worst-bytes-per-pixel)` — so when the caller's
  //   pixel limit was the tighter of the two, alignment inflation alone
  //   refused frames satisfying *both* requested limits. A 65536x1
  //   `gray8` frame under `max_pixels = 65536` and a generous byte
  //   budget fits the pixel limit exactly and costs 2 MiB, and was
  //   refused anyway — for arithmetic the caller never asked about.
  //
  // Logical extent is libavcodec's gate on raw dimensions; allocation
  // cost is this one, against the caller's own `max_frame_bytes`. One
  // question each.
  //
  // Audio reaches here too, and used to pass unpriced entirely:
  // `max_samples` bounds the sample *count*, so one sample across eight
  // packed `f64` channels is 64 valid bytes under a 64-byte ceiling and
  // a 2,080-byte allocation — delivered, because the copy-out only ever
  // rechecks the valid bytes.
  //
  // SAFETY: `ctx` and `frame` are live; every field read is a plain
  // integer, and `format` stays an integer throughout.
  // SAFETY: `frame` is live; the field is a plain pointer.
  let hw_frames = unsafe { (*frame).hw_frames_ctx };

  // A hardware frame carries no CPU bytes for this callback to price — its
  // pool is judged where it is declared, in the `get_format` callback —
  // so it is delegated rather than failed closed on an unpriceable
  // format.
  if hw_frames.is_null() {
    // **The caller's own number, read from the field that carries it.**
    // This used to recover a byte ceiling from `AVCodecContext.max_pixels`,
    // and the recovery was wrong in both directions:
    //
    // * `max_pixels` is `min(pixel ceiling, byte ceiling / worst)`, so
    //   when the *pixel* ceiling was the tighter of the two it stopped
    //   encoding the byte ceiling at all — and the recovery invented a
    //   smaller one. A 256x256 frame at 16 bytes a pixel under
    //   `max_pixels = 65536` with a 2 MiB byte budget satisfies both of
    //   the caller's limits, costs 1,050,624 bytes, and was judged
    //   against 1,048,576 and refused. The claim that the conflation
    //   was harmless in one direction was simply wrong: it omitted the
    //   footprint's own alignment and slack, which is exactly where
    //   those extra 2,048 bytes live.
    // * and for audio a pixel ceiling has no business being consulted
    //   at all.
    //
    // The audio road briefly recovered from `max_samples` instead,
    // which *is* exact — but two sources of truth for one number is how
    // the first one went wrong. Both media read the field now.
    //
    // SAFETY: `opaque` holds the `CallbackState` that
    // `install_allocator_judge` installed with this callback, and whose
    // owner keeps it live for as long as the context can call back (the
    // contract above). A null one means a context the door did not
    // equip, and is refused rather than assumed generous.
    let state = unsafe { (*ctx).opaque } as *const CallbackState;
    if state.is_null() {
      return -(libc::EINVAL);
    }
    // SAFETY: non-null per the check above; the field is a plain `u64`.
    let byte_ceiling = u128::from(unsafe { (*state).max_frame_bytes });

    // SAFETY: `frame` is live; both are plain integer fields.
    let (format_raw, nb_samples) = unsafe { ((*frame).format, (*frame).nb_samples) };
    let priced = if width > 0 && height > 0 {
      crate::footprint::video_frame_bytes(format_raw, width, height)
    } else if nb_samples > 0 {
      // **The frame's layout, not the context's.** FFmpeg's
      // `get_buffer2` contract says the callback reads the values on
      // the *frame*, and `avcodec_default_get_buffer2` sizes from them
      // — the context's layout is whatever was last negotiated and can
      // differ outright. A context claiming mono against a frame
      // carrying 255 `dblp` channels at 130,000 samples prices about a
      // megabyte and allocates about 265 MB.
      //
      // Read raw and signed, per the house discipline, and refused
      // rather than floored: a negative count is malformed, and
      // flooring it to zero would price an allocation that is about to
      // happen at nothing.
      // SAFETY: `frame` is live; `ch_layout.nb_channels` is a plain
      // `c_int`.
      let channels = unsafe { (*frame).ch_layout.nb_channels };
      if channels <= 0 {
        return -(libc::EINVAL);
      }
      crate::footprint::audio_frame_bytes(format_raw, nb_samples as usize, channels as usize)
    } else {
      // Neither geometry nor samples: nothing is being allocated that
      // this callback can price, and nothing is claimed.
      Some(0)
    };

    // **The refusal leaves its reason behind.** A `get_buffer2`
    // callback can only answer libavcodec with an errno, and
    // `AVERROR(EINVAL)` is also what libavcodec reports for corrupt
    // input — so a bare refusal here was indistinguishable from a
    // broken file, and only one of those is worth retrying with a
    // larger ceiling. The decoder funnels collect this the same way
    // they collect the `get_format` declination.
    let record = |bytes: u64| {
      use core::sync::atomic::Ordering;
      // SAFETY: `state` was proved non-null above.
      unsafe {
        (*state)
          .declined_frame_bytes
          .store(bytes, Ordering::Relaxed);
        (*state)
          .declined_frame_audio
          .store(width <= 0 && height <= 0, Ordering::Relaxed);
        (*state)
          .frame_budget_declined
          .store(true, Ordering::Release);
      }
      -(libc::EINVAL)
    };
    match priced {
      // Fail closed. An allocation whose size cannot be established is
      // not a small one — the same stance every other judge here takes.
      // Reported as an unbounded cost, which is what an unprovable one
      // is.
      None => return record(u64::MAX),
      // Nothing to buy, so nothing to refuse.
      Some(0) => {}
      // A budget of zero admits nothing, and this is the arm that used
      // to be a skipped guard.
      Some(bytes) if byte_ceiling == 0 => return record(bytes as u64),
      Some(bytes) if bytes as u128 > byte_ceiling => return record(bytes as u64),
      Some(_) => {}
    }
  }

  // SAFETY: delegating to the allocator libavcodec would have used.
  unsafe { ffmpeg_next::ffi::avcodec_default_get_buffer2(ctx, frame, flags) }
}

/// Installs the allocator judge on a codec context: the context's
/// `get_buffer2` becomes the judge, and its `opaque` the
/// [`CallbackState`] the judge reads its byte budget from and records
/// its refusals in — the same state the `get_format` callback,
/// [`get_hw_format`], reads the wanted hardware format from.
///
/// This is the one way to install the judge. The two fields are written
/// together because the judge cannot run without the state, and a
/// context the door did not equip carries none.
///
/// # Safety
///
/// * `ctx` must point at a live `AVCodecContext` the caller owns that has
///   not been opened. No callback runs on a context before
///   `avcodec_open2`, so none is running while the two fields are
///   written.
/// * `state` must point at a live, aligned [`CallbackState`] ([its
///   contract](crate::contracts#callbackstate)) and stay valid for the
///   callbacks' accesses — reads of its plain fields,
///   atomic reads and writes of the rest — **until the context has been
///   freed**, the free included. That is one address kept live, and it
///   is also provenance kept intact: nothing may invalidate `state` in
///   that span. A `Box` asserts uniqueness over what it holds, so a box
///   the state was taken from must not be moved, written through or
///   borrowed as `&mut` after this call — each of the three invalidates
///   `state` — and no `&mut CallbackState` may be made from any other
///   pointer. Owning the state through the pointer `Box::into_raw`
///   returns, and freeing it with `Box::from_raw` once the context is
///   freed, satisfies this.
/// * **The never-opened exception.** A context that is freed without
///   `avcodec_open2` ever having been called on it makes no callback at
///   all, so on that road, and only there, the state may be freed first
///   — provided nothing but the context's free follows. A caller's
///   returns between this call and the open take this road.
/// * From the context's opening until it has been freed, libavcodec may
///   call back at any time: within the caller's own libavcodec calls,
///   and — should the decoder run threads — from its own threads, which
///   FFmpeg documents for both callbacks. The callbacks read the plain
///   fields and record through the atomic ones, so in that span nothing
///   may write a plain field or hold a `&mut CallbackState`. Before the
///   opening the caller may still write the state through `state`
///   itself — [`CallbackState::set_wanted`] called through this pointer
///   does not invalidate it.
/// * From this call on, `opaque` is the judge's: nothing may read it as
///   anything but this state, and nothing but this door may replace it.
pub unsafe fn install_allocator_judge(
  ctx: *mut ffmpeg_next::ffi::AVCodecContext,
  state: *mut CallbackState,
) {
  // SAFETY: `ctx` is live and not yet opened per the contract, so no
  // callback reads either field while they are written; both are plain
  // fields of it.
  unsafe {
    (*ctx).opaque = state.cast();
    (*ctx).get_buffer2 = Some(judge_buffer);
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn pix_fmt_name_reads_the_linked_librarys_own_table() {
    assert_eq!(
      pix_fmt_name_static(AVPixelFormat::AV_PIX_FMT_YUV420P as i32),
      Some("yuv420p")
    );
    assert_eq!(
      pix_fmt_name_static(AVPixelFormat::AV_PIX_FMT_NV12 as i32),
      Some("nv12")
    );
    // A hardware surface: no CPU pixel data, so `from_av_pixel_format`
    // answers `PixelFormat::None` — and this is what puts a name on the
    // integer behind that `None` in the error message.
    assert_eq!(
      pix_fmt_name_static(AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX as i32),
      Some("videotoolbox_vld")
    );
  }

  #[test]
  fn pix_fmt_name_refuses_what_ffmpeg_cannot_name() {
    // The property the redeclared signature depends on, checked against
    // the linked library rather than assumed: libavutil bounds-checks
    // the index and answers out-of-range integers with null, so passing
    // a value outside the enum's discriminant set is defined.
    assert_eq!(
      pix_fmt_name_static(AVPixelFormat::AV_PIX_FMT_NONE as i32),
      None
    );
    assert_eq!(pix_fmt_name_static(-99_999), None);
    assert_eq!(pix_fmt_name_static(i32::MIN), None);
    assert_eq!(pix_fmt_name_static(i32::MAX), None);
    assert_eq!(
      pix_fmt_name_static(AVPixelFormat::AV_PIX_FMT_NB as i32),
      None
    );
  }

  // The callback derefs `(*ctx).opaque`, so we need a real-looking
  // AVCodecContext. We construct a zeroed one (the callback only reads opaque).
  struct FakeCtx(*mut AVCodecContext);
  impl FakeCtx {
    fn new(state: *mut CallbackState) -> Self {
      let boxed: Box<AVCodecContext> = unsafe { Box::new(std::mem::zeroed()) };
      let raw = Box::into_raw(boxed);
      unsafe { (*raw).opaque = state.cast() };
      Self(raw)
    }
  }
  impl Drop for FakeCtx {
    fn drop(&mut self) {
      unsafe { drop(Box::from_raw(self.0)) };
    }
  }

  fn make_state(wanted: AVPixelFormat) -> CallbackState {
    CallbackState {
      wanted,
      wanted_int: wanted as i32,
      ceiling_declined: core::sync::atomic::AtomicBool::new(false),
      declined_pixels: core::sync::atomic::AtomicI64::new(0),
      declined_limit: core::sync::atomic::AtomicI64::new(0),
      max_frame_bytes: u64::MAX,
      frame_budget_declined: core::sync::atomic::AtomicBool::new(false),
      declined_frame_bytes: core::sync::atomic::AtomicU64::new(0),
      declined_frame_audio: core::sync::atomic::AtomicBool::new(false),
    }
  }

  fn run(state: &CallbackState, mut offered: Vec<i32>) -> AVPixelFormat {
    // Build the offered list as raw i32, terminated by AV_PIX_FMT_NONE.
    offered.push(AVPixelFormat::AV_PIX_FMT_NONE as i32);
    let ctx = FakeCtx::new(state as *const _ as *mut _);
    // SAFETY: we cast the i32 buffer pointer to *const AVPixelFormat
    // because that's the function's declared signature. The callback only
    // ever reads through *const i32 internally, so this transit through
    // *const AVPixelFormat is purely a type system formality.
    unsafe { get_hw_format(ctx.0, offered.as_ptr() as *const AVPixelFormat) }
  }

  #[test]
  fn returns_wanted_hw_format_when_offered() {
    let state = make_state(AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX);
    let got = run(
      &state,
      vec![
        AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX as i32,
        AVPixelFormat::AV_PIX_FMT_NV12 as i32,
      ],
    );
    assert_eq!(got, AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX);
  }

  #[test]
  fn returns_none_when_wanted_absent() {
    let state = make_state(AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX);
    let got = run(
      &state,
      vec![
        AVPixelFormat::AV_PIX_FMT_NV12 as i32,
        AVPixelFormat::AV_PIX_FMT_YUV420P as i32,
      ],
    );
    assert_eq!(got, AVPixelFormat::AV_PIX_FMT_NONE);
  }

  #[test]
  fn null_opaque_is_treated_as_strict() {
    let boxed: Box<AVCodecContext> = unsafe { Box::new(std::mem::zeroed()) };
    let ctx_raw = Box::into_raw(boxed);
    unsafe { (*ctx_raw).opaque = ptr::null_mut() };
    let offered = [
      AVPixelFormat::AV_PIX_FMT_NV12 as i32,
      AVPixelFormat::AV_PIX_FMT_NONE as i32,
    ];
    let got = unsafe { get_hw_format(ctx_raw, offered.as_ptr() as *const AVPixelFormat) };
    assert_eq!(got, AVPixelFormat::AV_PIX_FMT_NONE);
    unsafe { drop(Box::from_raw(ctx_raw)) };
  }

  #[test]
  fn unknown_offered_value_is_skipped_without_ub() {
    // Simulate a header-skewed FFmpeg that offers a pixel-format value we
    // don't have a binding constant for (e.g. some future format). The
    // callback walks the list as i32 — no enum is constructed from that
    // value, so this read is sound.
    let state = make_state(AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX);
    let got = run(
      &state,
      vec![
        99_999_i32, // imaginary unknown
        AVPixelFormat::AV_PIX_FMT_NV12 as i32,
      ],
    );
    assert_eq!(got, AVPixelFormat::AV_PIX_FMT_NONE);
  }

  /// `codec_supports_hwaccel` must reject a (device_type, pix_fmt) pair
  /// that the codec does not advertise — even if the device alone is
  /// listed. Without this check, the strict `get_format` callback would
  /// be wired up for a HW pix_fmt the codec never offers and the failure
  /// would surface deep inside the probe / decode path instead of at
  /// `open_with` / probe-build time.
  ///
  /// macOS-only: the test relies on FFmpeg's H.264 decoder advertising
  /// `(AV_HWDEVICE_TYPE_VIDEOTOOLBOX, AV_PIX_FMT_VIDEOTOOLBOX)`, which is
  /// only present in builds with VideoToolbox compiled in.
  #[cfg(target_os = "macos")]
  #[test]
  fn codec_supports_hwaccel_requires_matching_pix_fmt() {
    use ffmpeg_next::ffi::{AVCodecID, AVHWDeviceType, AVPixelFormat, avcodec_find_decoder};

    // SAFETY: AV_CODEC_ID_H264 is a known constant in our build's
    // `AVCodecID` discriminant set; constructing it does not invoke the
    // bindgen-enum UB we worry about for runtime-derived ids.
    let codec_ptr = unsafe { avcodec_find_decoder(AVCodecID::AV_CODEC_ID_H264) };
    assert!(!codec_ptr.is_null(), "H.264 decoder must be present");

    let device = AVHWDeviceType::AV_HWDEVICE_TYPE_VIDEOTOOLBOX;
    let videotoolbox = AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX as i32;
    let nv12 = AVPixelFormat::AV_PIX_FMT_NV12 as i32;

    // SAFETY: `codec_ptr` is libavcodec's own H.264 decoder entry, just
    // checked non-null; its static codec table lives for the process.
    assert!(
      unsafe { codec_supports_hwaccel(codec_ptr, device, videotoolbox) },
      "VideoToolbox + AV_PIX_FMT_VIDEOTOOLBOX must be advertised by FFmpeg's H.264 decoder"
    );
    // SAFETY: as above.
    assert!(
      !unsafe { codec_supports_hwaccel(codec_ptr, device, nv12) },
      "VideoToolbox + AV_PIX_FMT_NV12 must NOT match the codec's HW config — \
       the strict get_format would have no offered HW format to return"
    );
  }

  // ---------------------------------------------------------------------------
  //  R16: the allocator callback, driven at the seam.
  // ---------------------------------------------------------------------------

  /// Drives [`judge_buffer`] the way libavcodec does, with the
  /// context and the frame described independently.
  ///
  /// **Why at the seam and not in the pricer's matrix.** The footprint
  /// matrix is exhaustive over formats and shapes and still could not
  /// have caught the bug this exercises: it feeds one channel count into
  /// both the estimate and the measurement, so a callback reading the
  /// *context's* layout where the allocator reads the *frame's* is
  /// invisible to it. A matrix proves a formula. Only a lane that lets
  /// the two sides disagree proves the seam that joins them.
  struct JudgeCase {
    ctx_channels: i32,
    max_pixels: i64,
    max_frame_bytes: u64,
    format_raw: i32,
    width: i32,
    height: i32,
    nb_samples: i32,
    frame_channels: i32,
  }

  impl JudgeCase {
    fn audio(format_raw: i32, nb_samples: i32, frame_channels: i32) -> Self {
      Self {
        ctx_channels: 1,
        max_pixels: i64::MAX,
        max_frame_bytes: u64::MAX,
        format_raw,
        width: 0,
        height: 0,
        nb_samples,
        frame_channels,
      }
    }
    fn video(format_raw: i32, width: i32, height: i32) -> Self {
      Self {
        ctx_channels: 0,
        max_pixels: i64::MAX,
        max_frame_bytes: u64::MAX,
        format_raw,
        width,
        height,
        nb_samples: 0,
        frame_channels: 0,
      }
    }
    fn with_max_pixels(mut self, v: i64) -> Self {
      self.max_pixels = v;
      self
    }
    fn with_max_frame_bytes(mut self, v: u64) -> Self {
      self.max_frame_bytes = v;
      self
    }

    /// Returns the callback's verdict: `Ok(())` when it delegated (and
    /// the allocation succeeded), `Err(rc)` when it refused.
    fn run(&self) -> std::result::Result<(), i32> {
      use ffmpeg_next::ffi;
      // SAFETY: a context and a frame are allocated, described, driven
      // through the callback and freed here; every field written is a
      // plain integer or a layout FFmpeg's own helper fills.
      unsafe {
        // The context has to be *opened*: `avcodec_default_get_buffer2`
        // reads the codec's own descriptor, so driving the callback
        // against a bare allocation would fault inside FFmpeg rather than
        // test anything. A raw codec per medium keeps the harness honest
        // — the allocator is the real one.
        let audio = self.width <= 0 && self.height <= 0;
        let codec = ffi::avcodec_find_decoder(if audio {
          ffi::AVCodecID::AV_CODEC_ID_PCM_S16LE
        } else {
          ffi::AVCodecID::AV_CODEC_ID_RAWVIDEO
        });
        let ctx = ffi::avcodec_alloc_context3(codec);
        assert!(!ctx.is_null());
        if audio {
          (*ctx).sample_rate = 48_000;
          (*ctx).sample_fmt = ffi::AVSampleFormat::AV_SAMPLE_FMT_S16;
          ffi::av_channel_layout_default(
            core::ptr::addr_of_mut!((*ctx).ch_layout),
            self.ctx_channels.max(1),
          );
        } else {
          (*ctx).width = self.width.max(1);
          (*ctx).height = self.height.max(1);
          (*ctx).pix_fmt = core::mem::transmute::<i32, ffi::AVPixelFormat>(self.format_raw);
        }
        // The judge reads its budget from the callback state, and a
        // context the door did not equip has none and is refused — so the
        // harness installs both through the door before the open, as the
        // read adapter's `build_codec_context` does. The box is not
        // touched again until the context is freed.
        let mut state = Box::new(CallbackState::new(self.max_frame_bytes));
        install_allocator_judge(ctx, &raw mut *state);
        assert_eq!(
          ffi::avcodec_open2(ctx, codec, core::ptr::null_mut()),
          0,
          "the harness codec must open",
        );
        // Set after the open, so nothing resets it.
        (*ctx).max_pixels = self.max_pixels;

        let frame = ffi::av_frame_alloc();
        assert!(!frame.is_null());
        (*frame).format = self.format_raw;
        (*frame).width = self.width;
        (*frame).height = self.height;
        (*frame).nb_samples = self.nb_samples;
        if self.frame_channels > 0 {
          ffi::av_channel_layout_default(
            core::ptr::addr_of_mut!((*frame).ch_layout),
            self.frame_channels,
          );
        }

        // Through the pointer the door installed, as libavcodec calls it.
        let get_buffer2 = (*ctx).get_buffer2.expect("the door installs the judge");
        let rc = get_buffer2(ctx, frame, 0);
        // The state outlives the context, as the door requires.
        ffi::av_frame_free(&mut (frame as *mut _));
        ffi::avcodec_free_context(&mut (ctx as *mut _));
        drop(state);
        if rc < 0 { Err(rc) } else { Ok(()) }
      }
    }
  }

  /// **The raw decoder door answers every integer.** A codec id this
  /// build names finds its decoder; an integer libavcodec has no decoder
  /// for — negative, or far past every id — answers `None`, and none of
  /// them ever becomes an `AVCodecID`.
  #[test]
  fn the_raw_decoder_door_answers_every_integer() {
    let h264 = ffmpeg_next::ffi::AVCodecID::AV_CODEC_ID_H264 as c_int;
    assert!(
      find_decoder_raw(h264).is_some(),
      "H.264 is decodable in this build"
    );
    for id in [-1, i32::MAX, i32::MIN] {
      assert!(find_decoder_raw(id).is_none(), "no decoder for {id}");
    }
  }

  /// **A panicking visitor leaves no list behind.** The download-format
  /// list is libavutil's allocation, and the visitor is the caller's
  /// code: when it panics, the guard that owns the list frees it as the
  /// panic unwinds, where the old free after the loop was skipped and
  /// the list leaked.
  ///
  /// Run against a real frames context — VideoToolbox's, whose
  /// `transfer_get_formats` needs no hardware, only the frames context's
  /// `sw_format`, so it answers on any macOS host. The frees are counted
  /// by the guard itself ([`format_list_probe`]): FFmpeg keeps no count of
  /// live allocations, and the allocation cap bounds a single
  /// allocation's size rather than counting any.
  #[test]
  #[cfg(target_os = "macos")]
  fn a_panicking_visitor_leaves_no_download_list_behind() {
    use core::ops::ControlFlow;
    use ffmpeg_next::ffi::{
      AVBufferRef, AVHWDeviceType, AVHWFramesContext, av_buffer_unref, av_hwdevice_ctx_create,
      av_hwframe_ctx_alloc,
    };

    let mut device: *mut AVBufferRef = ptr::null_mut();
    // SAFETY: `device` is a valid out-pointer; VideoToolbox's device
    // takes no device string and no options.
    let rc = unsafe {
      av_hwdevice_ctx_create(
        &mut device,
        AVHWDeviceType::AV_HWDEVICE_TYPE_VIDEOTOOLBOX,
        ptr::null(),
        ptr::null_mut(),
        0,
      )
    };
    assert_eq!(rc, 0, "a VideoToolbox device context");
    // SAFETY: `device` is the live device reference just created.
    let mut frames = unsafe { av_hwframe_ctx_alloc(device) };
    assert!(!frames.is_null(), "a frames context");
    // SAFETY: `frames` is the live reference just allocated; its `data`
    // is the `AVHWFramesContext`, whose plain fields are set before
    // anything reads them.
    unsafe {
      let ctx = (*frames).data.cast::<AVHWFramesContext>();
      (*ctx).format = AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX;
      (*ctx).sw_format = AVPixelFormat::AV_PIX_FMT_NV12;
      (*ctx).width = 64;
      (*ctx).height = 64;
    }

    // The ordinary road: the list is walked whole and freed once.
    let before = format_list_probe::frees();
    let mut seen = Vec::new();
    // SAFETY: `frames` is a live reference to a frames context.
    let walked = unsafe {
      visit_hwframe_download_formats::<()>(frames, |format| {
        seen.push(format);
        ControlFlow::Continue(())
      })
    };
    assert_eq!(walked, Ok(None));
    assert_eq!(seen, [AVPixelFormat::AV_PIX_FMT_NV12 as c_int]);
    assert_eq!(
      format_list_probe::frees() - before,
      1,
      "freed on the way out"
    );

    // The visitor panics: the panic reaches the caller, and the list is
    // freed on its way past.
    let before = format_list_probe::frees();
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
      // SAFETY: as above.
      unsafe { visit_hwframe_download_formats::<()>(frames, |_| panic!("a visitor that panics")) }
    }));
    assert!(caught.is_err(), "the visitor's panic reaches the caller");
    assert_eq!(
      format_list_probe::frees() - before,
      1,
      "the guard freed the list while the panic unwound",
    );

    // SAFETY: both references are this law's own, unreffed once each.
    unsafe {
      av_buffer_unref(&mut frames);
      av_buffer_unref(&mut device);
    }
  }

  /// **The door installs the judge and its state together.** Afterwards
  /// the context's `get_buffer2` is the judge and its `opaque` the state
  /// — the only pairing the judge can run on.
  #[test]
  fn the_door_installs_the_judge_with_its_state() {
    // SAFETY: a bare context is allocated, given the judge and its
    // state, read and freed; nothing decodes on it, and the state
    // outlives it.
    unsafe {
      let mut ctx = ffmpeg_next::ffi::avcodec_alloc_context3(core::ptr::null());
      assert!(!ctx.is_null());
      let mut state = Box::new(CallbackState::new(1 << 20));
      let state_ptr = &raw mut *state;
      install_allocator_judge(ctx, state_ptr);
      assert_eq!((*ctx).opaque, state_ptr.cast::<core::ffi::c_void>());
      assert_eq!(
        (*ctx)
          .get_buffer2
          .map(|callback| callback as *const () as usize),
        Some(judge_buffer as *const () as usize),
        "the door installs the judge itself",
      );
      ffmpeg_next::ffi::avcodec_free_context(&mut ctx);
      drop(state);
    }
  }

  #[test]
  fn the_callback_prices_the_frames_layout_not_the_contexts() {
    ffmpeg_next::init().expect("ffmpeg init");
    const DBLP: i32 = ffmpeg_next::ffi::AVSampleFormat::AV_SAMPLE_FMT_DBLP as i32;

    // The reported shape: a context claiming mono against a frame
    // carrying 255 `dblp` channels at 130,000 samples. Priced from the
    // context that is about a megabyte; the allocator takes about 265 MB.
    //
    // A 16 MiB ceiling therefore separates the two readings exactly: it
    // admits the context's story and must refuse the frame's.
    assert!(
      JudgeCase::audio(DBLP, 130_000, 255)
        .with_max_frame_bytes(16 * 1024 * 1024)
        .run()
        .is_err(),
      "the callback priced the context's channel count, not the frame's",
    );

    // And the same frame under a ceiling that genuinely covers it is
    // delegated — the judge refuses cost, not multichannel audio.
    JudgeCase::audio(DBLP, 130_000, 255)
      .with_max_frame_bytes(u64::MAX)
      .run()
      .expect("an affordable frame must still be allocated");

    // A frame declaring no channels at all is malformed, and refused
    // rather than priced at nothing.
    assert!(JudgeCase::audio(DBLP, 1024, 0).run().is_err());
  }

  #[test]
  fn the_callback_recovers_each_mediums_ceiling_independently() {
    ffmpeg_next::init().expect("ffmpeg init");
    const S16: i32 = ffmpeg_next::ffi::AVSampleFormat::AV_SAMPLE_FMT_S16 as i32;
    const NV12: i32 = ffmpeg_next::ffi::AVPixelFormat::AV_PIX_FMT_NV12 as i32;

    // **A pixel ceiling has no business bounding audio.** The first cut
    // derived one byte ceiling for both media from `max_pixels`, so a
    // caller who set `max_pixels = 1` gave every audio frame a 16-byte
    // budget and ordinary sound was refused. Both media read the caller's
    // own `max_frame_bytes` from the callback state now.
    JudgeCase::audio(S16, 1024, 2)
      .with_max_pixels(1)
      .with_max_frame_bytes(u64::MAX)
      .run()
      .expect("a pixel ceiling must not refuse audio");

    // **And a budget of zero must refuse, not disappear.** The guard used
    // to be written as `max_pixels > 0`, so the tightest ceiling a caller
    // can ask for turned the judge off entirely — the one direction a
    // ceiling must never fail in.
    assert!(
      JudgeCase::video(NV12, 16, 16)
        .with_max_frame_bytes(0)
        .run()
        .is_err(),
      "a zero byte budget admitted a picture",
    );
    assert!(
      JudgeCase::audio(S16, 1024, 2)
        .with_max_frame_bytes(0)
        .run()
        .is_err(),
      "a zero byte budget admitted an audio frame",
    );

    // A generous pixel limit does not rescue a starved byte budget.
    assert!(
      JudgeCase::audio(S16, 65_535, 8)
        .with_max_pixels(i64::MAX)
        .with_max_frame_bytes(16)
        .run()
        .is_err(),
    );

    // **The shape that disproved the old recovery.** A 256x256 frame at
    // 16 bytes a pixel under a tight *pixel* limit and a generous *byte*
    // budget satisfies both of the caller's limits — 65,536 pixels, and
    // 1,050,624 bytes against 2 MiB — while the recovered ceiling was
    // `max_pixels * 16 = 1,048,576`. It was refused by exactly the 2,048
    // bytes of alignment and slack the recovery could not see, which is
    // why the recovery had to go rather than be adjusted.
    const RGBAF32: i32 = ffmpeg_next::ffi::AVPixelFormat::AV_PIX_FMT_RGBAF32LE as i32;
    JudgeCase::video(RGBAF32, 256, 256)
      .with_max_pixels(65_536)
      .with_max_frame_bytes(2 * 1024 * 1024)
      .run()
      .expect("a frame inside both of the caller's limits must be allocated");

    // And the other direction still refuses: a generous pixel limit buys
    // nothing past the byte budget.
    assert!(
      JudgeCase::video(RGBAF32, 256, 256)
        .with_max_pixels(i64::MAX)
        .with_max_frame_bytes(1024 * 1024)
        .run()
        .is_err(),
      "a generous pixel ceiling admitted a frame past the byte ceiling",
    );
  }

  #[test]
  fn the_callback_judges_cost_and_leaves_logical_extent_to_libavcodec() {
    ffmpeg_next::init().expect("ffmpeg init");
    const GRAY8: i32 = ffmpeg_next::ffi::AVPixelFormat::AV_PIX_FMT_GRAY8 as i32;

    // A 65536x1 `gray8` frame: 65,536 raw pixels, and 65536x32 once the
    // allocator has aligned it — thirty-two times the pixels, about 2 MiB.
    //
    // **(a) It satisfies both of the caller's limits, so it is
    // allocated.** The pixel limit is met exactly on the raw dimensions,
    // which is the semantics `max_pixels` has and that libavcodec already
    // enforces in `ff_set_dimensions`; the byte budget is generous enough
    // for the real cost. Nothing here is over any line the caller drew.
    //
    // This is what the removed gate got wrong: it compared the *aligned*
    // dimensions against `max_pixels`, which is
    // `min(pixel limit, byte ceiling / worst)` — so when the pixel limit
    // was the tighter of the two, alignment inflation alone refused a frame
    // that was inside both requested limits, for arithmetic the caller
    // never asked about.
    JudgeCase::video(GRAY8, 65_536, 1)
      .with_max_pixels(65_536)
      .with_max_frame_bytes(8 * 1024 * 1024)
      .run()
      .expect("a frame inside both of the caller's limits must be allocated");

    // **(b) And the degenerate shape is still refused when it is
    // genuinely too expensive** — which is the point the aligned gate was
    // introduced for in the first place. The footprint prices the aligned
    // dimensions itself, so it carries that defense alone: same frame,
    // same generous pixel limit, a byte budget below its real cost.
    assert!(
      JudgeCase::video(GRAY8, 65_536, 1)
        .with_max_pixels(i64::MAX)
        .with_max_frame_bytes(1024 * 1024)
        .run()
        .is_err(),
      "the degenerate shape slipped its real cost past the byte ceiling",
    );

    // The two together are the whole argument for the removal: the gate
    // was redundant against (b) and wrong against (a).
  }
}
