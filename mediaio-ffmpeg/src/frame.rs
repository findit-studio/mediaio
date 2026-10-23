//! CPU-side decoded video frame.
//!
//! Wraps `ffmpeg_next::frame::Video`. All accessors read from raw `AVFrame`
//! fields (`format`, `linesize`, `data`, `width`, `height`, `pts`) directly
//! and never go through ffmpeg-next's `Video::format()` / `plane_height()`
//! / `plane_width()` / `data()` — those construct `AVPixelFormat` from the
//! frame's raw `format` integer via `transmute`, which is undefined behavior
//! when the value isn't in the build's bindgen-generated discriminant set
//! (the exact failure mode this crate is designed to survive).
//!
//! Per-row sizes for [`Frame::row`] / [`Frame::rows`] are computed from
//! hardcoded chroma-subsampling and bit-depth tables keyed on the safe
//! `pix_fmt()` integer, covering only the formats `hwdecode` produces (the
//! NV* and P0xx/P2xx/P4xx families after `av_hwframe_transfer_data`). For
//! any other format, the row accessors return `None` rather than guessing
//! at a slice length.
//!
//! Why per-row, not whole-plane: FFmpeg allocates each row at
//! `linesize[plane]` ([`Frame::stride`]) bytes for SIMD alignment, but
//! hardware transfer paths only initialize the first
//! [`Frame::row_bytes`]`(plane)` of every row. Exposing a stride-inclusive
//! `&[u8]` over an entire plane would let safe code observe those
//! uninitialized padding bytes, which violates `slice::from_raw_parts`.
//! Per-row slices are tightly clipped to the visible byte width so the
//! safe API never hands out an uninitialized byte. Callers that need a
//! single base pointer (e.g. SIMD pixel converters keyed off stride) can
//! reach for [`Frame::as_ptr`] and consume `stride * plane_h` bytes
//! themselves under their own `unsafe` contract.
//!
//! Compare formats against the variants of
//! [`mediaspine::PixelFormat`].

use std::slice;

use ffmpeg_next::frame;
use mediaspine::PixelFormat;

use crate::{boundary, error::CrossingError};

/// Checked allocator for `ffmpeg_next::frame::Video`. ffmpeg-next's
/// `Video::empty` is built on `av_frame_alloc()` and ignores its
/// NULL-on-OOM return; the resulting `Video` would have a null inner
/// `*mut AVFrame` and the next FFmpeg call against it would be UB.
/// Use this helper anywhere a SW video scratch frame is constructed
/// in production code.
pub fn alloc_av_video_frame() -> Result<frame::Video, CrossingError> {
  let f = frame::Video::empty();
  // SAFETY: `as_ptr()` reads the inner pointer without dereferencing.
  if unsafe { f.as_ptr() }.is_null() {
    return Err(CrossingError::Ffmpeg(ffmpeg_next::Error::Other {
      errno: libc::ENOMEM,
    }));
  }
  Ok(f)
}

/// Checked allocator for `ffmpeg_next::frame::Audio`. Same rationale
/// as [`alloc_av_video_frame`].
pub fn alloc_av_audio_frame() -> Result<frame::Audio, CrossingError> {
  let f = frame::Audio::empty();
  // SAFETY: `as_ptr()` reads the inner pointer without dereferencing.
  if unsafe { f.as_ptr() }.is_null() {
    return Err(CrossingError::Ffmpeg(ffmpeg_next::Error::Other {
      errno: libc::ENOMEM,
    }));
  }
  Ok(f)
}

/// CPU-side decoded video frame produced by `mediadecode-ffmpeg`'s
/// `VideoDecoder`.
pub struct Frame {
  inner: frame::Video,
}

impl core::fmt::Debug for Frame {
  /// `frame::Video` (from `ffmpeg_next`) doesn't itself implement
  /// `Debug`, so route through the public accessors. Shows the
  /// dimensions, pixel format, plane count, and PTS — enough to
  /// distinguish frames at debug-print sites without surfacing
  /// raw FFI internals.
  fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
    f.debug_struct("Frame")
      .field("width", &self.width())
      .field("height", &self.height())
      .field("pix_fmt", &self.pix_fmt())
      .field("planes", &self.planes())
      .field("pts", &self.pts())
      .finish()
  }
}

impl Frame {
  /// Construct an empty frame, suitable as the destination passed to
  /// `mediadecode-ffmpeg`'s `VideoDecoder::receive_frame`.
  ///
  /// Returns `Err(CrossingError::Ffmpeg(Other { errno: ENOMEM }))` when
  /// the underlying `av_frame_alloc()` returns NULL — `ffmpeg_next` does
  /// not surface that failure, so we check it here rather than letting a
  /// null pointer flow into the safe accessors and become UB on first
  /// read.
  pub fn empty() -> Result<Self, CrossingError> {
    // SAFETY: as_ptr() is safe; we just inspect the value (potentially null).
    let inner = frame::Video::empty();
    if unsafe { inner.as_ptr() }.is_null() {
      return Err(CrossingError::Ffmpeg(ffmpeg_next::Error::Other {
        errno: libc::ENOMEM,
      }));
    }
    Ok(Self { inner })
  }

  /// Width in pixels.
  pub fn width(&self) -> u32 {
    // SAFETY: AVFrame.width is c_int; safe to read regardless of value.
    unsafe { (*self.inner.as_ptr()).width as u32 }
  }

  /// Height in pixels.
  pub fn height(&self) -> u32 {
    // SAFETY: AVFrame.height is c_int.
    unsafe { (*self.inner.as_ptr()).height as u32 }
  }

  /// Pixel format, returned as a [`PixelFormat`] (the unified
  /// mediaspine enum). The mapping is via [`boundary::from_av_pixel_format`]
  /// — sound regardless of the linked FFmpeg version, no
  /// `AVPixelFormat` enum is constructed from a runtime integer.
  pub fn pix_fmt(&self) -> PixelFormat {
    // SAFETY: AVFrame.format is bound as c_int.
    boundary::from_av_pixel_format(unsafe { (*self.inner.as_ptr()).format })
  }

  /// Presentation timestamp in stream time base, or `None` for
  /// `AV_NOPTS_VALUE`.
  pub fn pts(&self) -> Option<i64> {
    // ffmpeg-next's Frame::pts performs no enum conversion; safe to use.
    self.inner.pts()
  }

  /// Number of populated planes (1 for packed formats, 2 for NV12/P010,
  /// 3 for planar YUV, etc.). Computed by scanning `linesize` for the
  /// first zero entry — no enum reads.
  pub fn planes(&self) -> usize {
    // SAFETY: AVFrame.linesize is `[c_int; 8]`; reads are sound.
    unsafe {
      let linesize = &(*self.inner.as_ptr()).linesize;
      for (i, ls) in linesize.iter().enumerate() {
        if *ls == 0 {
          return i;
        }
      }
      linesize.len()
    }
  }

  /// Bytes per row for `plane`. Reads `AVFrame.linesize[plane]` directly.
  ///
  /// # Panics
  ///
  /// Panics if `plane >= planes()` or the linesize is non-positive
  /// (FFmpeg allows negative linesize for vertically-flipped formats;
  /// this crate does not surface those). Callers who need to handle
  /// either case without panicking should use [`Self::try_stride`],
  /// or the non-panicking pixel accessors [`Self::row`] / [`Self::rows`]
  /// / [`Self::row_bytes`] / [`Self::as_ptr`].
  pub fn stride(&self, plane: usize) -> usize {
    let n = self.planes();
    assert!(
      plane < n,
      "stride: plane {plane} out of bounds (planes={n})"
    );
    // SAFETY: bounds-checked above; linesize is `[c_int; 8]`.
    let linesize: i32 = unsafe { (*self.inner.as_ptr()).linesize[plane] };
    assert!(
      linesize > 0,
      "stride: non-positive linesize {linesize} for plane {plane} \
       (negative linesize means vertically-flipped — not supported)"
    );
    linesize as usize
  }

  /// Fallible counterpart to [`Self::stride`]. Returns `None` when
  /// `plane` is out of bounds *or* the linesize is non-positive (the
  /// two conditions [`Self::stride`] panics on). Use this when the
  /// frame's plane count or layout is caller-controlled / data-driven
  /// and either case should be handled rather than aborting.
  pub fn try_stride(&self, plane: usize) -> Option<usize> {
    if plane >= self.planes() {
      return None;
    }
    // SAFETY: bounds-checked above; linesize is `[c_int; 8]`.
    let linesize: i32 = unsafe { (*self.inner.as_ptr()).linesize[plane] };
    if linesize <= 0 {
      return None;
    }
    Some(linesize as usize)
  }

  /// Visible byte width of `plane` — the number of initialized bytes at
  /// the start of every row in that plane.
  ///
  /// Distinct from [`Self::stride`], which returns the FFmpeg `linesize`.
  /// `linesize` is `>= row_bytes` and may include trailing alignment
  /// padding bytes that FFmpeg's hardware transfer paths do not
  /// initialize. `row_bytes` is what `slice::from_raw_parts` can safely
  /// see.
  ///
  /// Returns `None` when the format is not in the supported HW-output set
  /// (see crate `pix_fmt`) or the plane is out of range.
  pub fn row_bytes(&self, plane: usize) -> Option<usize> {
    if plane >= self.planes() {
      return None;
    }
    plane_row_bytes_for(&self.pix_fmt(), plane, self.width() as usize)
  }

  /// Pixel data for one row of `plane`, tightly clipped to the visible
  /// byte width ([`Self::row_bytes`]).
  ///
  /// Excludes the trailing alignment padding that [`Self::stride`]
  /// includes — those bytes are not guaranteed to be initialized by
  /// FFmpeg's hardware transfer paths and must not be exposed through a
  /// safe `&[u8]`.
  ///
  /// Returns `None` for any of the following — never panics:
  /// - The frame's pixel format is not one of the supported hardware-
  ///   output formats listed in [`crate::pix_fmt`].
  /// - The plane index is out of range.
  /// - `y` is past the plane's row count.
  /// - `AVFrame.linesize[plane]` is `<= 0` or `AVFrame.height` is `<= 0`.
  /// - The plane's data pointer is null.
  /// - The plane size would overflow `isize::MAX`.
  pub fn row(&self, plane: usize, y: usize) -> Option<&[u8]> {
    let info = self.plane_info(plane)?;
    if y >= info.plane_h {
      return None;
    }
    // y < plane_h and plane_h * stride ≤ isize::MAX (verified in plane_info),
    // so y * stride is bounded by (plane_h - 1) * stride ≤ isize::MAX.
    let offset = y * info.stride;
    // SAFETY:
    // - `info.plane_ptr` is non-null (verified in plane_info).
    // - `offset + row_bytes ≤ plane_h * stride`, which is the size of the
    //   FFmpeg allocation for this plane.
    // - Bytes 0..row_bytes of every row are written by FFmpeg's HW
    //   transfer; the slice is fully initialized.
    // - `row_bytes ≤ stride ≤ isize::MAX` per plane_info.
    unsafe {
      let row_ptr = info.plane_ptr.add(offset);
      Some(slice::from_raw_parts(row_ptr, info.row_bytes))
    }
  }

  /// Iterator over every row of `plane`. Each yielded slice has length
  /// [`Self::row_bytes`]`(plane)` — never includes the trailing alignment
  /// padding that lives within [`Self::stride`].
  ///
  /// Returns `None` under the same conditions as [`Self::row`].
  pub fn rows(&self, plane: usize) -> Option<impl Iterator<Item = &[u8]> + '_> {
    let info = self.plane_info(plane)?;
    Some((0..info.plane_h).map(move |y| {
      // Same bounds argument as `row()`.
      let offset = y * info.stride;
      // SAFETY: see `row()` — the same invariants hold here, and the
      // iterator's lifetime is tied to `&self` so the pointer remains
      // valid for every yielded slice.
      unsafe { slice::from_raw_parts(info.plane_ptr.add(offset), info.row_bytes) }
    }))
  }

  /// Raw base pointer to `plane`'s allocation, or `None` if the plane
  /// fails the same layout validation [`Self::row`] applies.
  ///
  /// Returns `None` whenever any of the following is true:
  /// - The plane index is out of range (`plane >= planes()`).
  /// - The frame's pixel format is not in the supported HW-output set.
  /// - `linesize[plane] <= 0`. **In particular, FFmpeg permits negative
  ///   linesizes for vertically-flipped frames with `data[n]` pointing
  ///   at the *end* of the image. Returning that pointer with the
  ///   advertised "valid for `stride * plane_h` bytes forward" contract
  ///   would let a downstream converter walk past the buffer.** This
  ///   accessor refuses the layout instead of handing back a pointer the
  ///   caller cannot safely interpret as forward-addressable.
  /// - `height <= 0`, the data pointer is null, `row_bytes > stride`, or
  ///   the total plane size would overflow `isize::MAX`.
  ///
  /// On `Some(ptr)` the pointer is valid for
  /// `stride(plane) * plane_height` *forward-addressable* bytes, and
  /// only the first [`Self::row_bytes`]`(plane)` bytes of each row are
  /// guaranteed to be initialized. The trailing per-row alignment padding
  /// is uninitialized; callers performing wide SIMD loads that read past
  /// `row_bytes` must mask the result and never surface those bytes
  /// through a safe `&[u8]`.
  ///
  /// This accessor exists for downstream pixel-format converters
  /// (`colconv`) that work in `(ptr, stride, width, height)` quadruples;
  /// safe code should prefer [`Self::row`] / [`Self::rows`].
  pub fn as_ptr(&self, plane: usize) -> Option<*const u8> {
    // Share the full plane-layout validation so the unsafe escape hatch
    // never escapes a layout that `row()` / `rows()` reject. Returning a
    // pointer for a negative-stride frame (FFmpeg's vertical-flip
    // convention, where `data[n]` points at the *end* of the image)
    // would invite forward-walking out-of-bounds reads from a caller
    // that trusts the documented "valid for stride × plane_h bytes"
    // contract.
    self.plane_info(plane).map(|info| info.plane_ptr)
  }

  /// Read every per-plane field needed by the row accessors with the
  /// safety preconditions enforced once.
  fn plane_info(&self, plane: usize) -> Option<PlaneInfo> {
    if plane >= self.planes() {
      return None;
    }
    // SAFETY: bounds-checked plane index; linesize/height/data are raw
    // c_int / pointer reads that cannot themselves be UB.
    let (stride_int, height_int, plane_ptr) = unsafe {
      let raw = self.inner.as_ptr();
      ((*raw).linesize[plane], (*raw).height, (*raw).data[plane])
    };
    if stride_int <= 0 || height_int <= 0 || plane_ptr.is_null() {
      return None;
    }
    let stride = stride_int as usize;
    let plane_h = plane_height_for(&self.pix_fmt(), plane, height_int as usize)?;
    let row_bytes = plane_row_bytes_for(&self.pix_fmt(), plane, self.width() as usize)?;
    if row_bytes > stride {
      return None;
    }
    // Bound the entire plane allocation to isize::MAX so any byte offset
    // computed as `y * stride` (y < plane_h) stays representable, satisfying
    // the safety contract of `pointer::add` and `slice::from_raw_parts`.
    let plane_size = stride.checked_mul(plane_h)?;
    if plane_size > isize::MAX as usize {
      return None;
    }
    Some(PlaneInfo {
      plane_ptr,
      stride,
      plane_h,
      row_bytes,
    })
  }

  /// Crate-internal: hand the wrapped frame to FFmpeg / our decoder code.
  ///
  /// # Safety
  ///
  /// [`Self::row`], [`Self::rows`] and [`Self::as_ptr`] trust the wrapped
  /// `AVFrame`'s `format`, `width`, `height`, `linesize` and `data` to
  /// describe the buffers it holds, and form slices from them. The caller
  /// must leave those fields describing each other: change the frame only
  /// as a whole — `av_frame_unref`, `av_frame_move_ref`, a decoder's
  /// `receive_frame`, `av_hwframe_transfer_data`, or replacing it with a
  /// whole `frame::Video` — and never through the setters
  /// `ffmpeg_next::frame::Video` offers as safe (`set_width`,
  /// `set_height`, `set_format`, ...), which move the geometry and leave
  /// the buffers where they were.
  ///
  /// The frame left behind must also meet [every exported byte
  /// initialised](crate::contracts#every-exported-byte-initialised): the
  /// accessors lend each row as `&[u8]`, so every row of every plane must
  /// have been written. A whole `frame::Video::new` does not meet it until
  /// its caller writes its planes — `av_frame_get_buffer` leaves them
  /// unwritten.
  pub unsafe fn as_inner_mut(&mut self) -> &mut frame::Video {
    &mut self.inner
  }
}

#[derive(Clone, Copy)]
struct PlaneInfo {
  plane_ptr: *const u8,
  stride: usize,
  plane_h: usize,
  row_bytes: usize,
}

// `Default` intentionally omitted: constructing a frame can fail (OOM
// in `av_frame_alloc`), and a panicking `default()` would defeat the
// safety stance of [`Frame::empty`]. Use `Frame::empty()?` directly.

/// Whether `pix_fmt_int` is a CPU pixel format the safe `Frame::row` /
/// `Frame::rows` / `Frame::row_bytes` / `Frame::as_ptr` accessors
/// support — i.e. one of the NV*/P0xx/P2xx/P4xx semi-planar families
/// this crate expects HW backends to produce after
/// `av_hwframe_transfer_data`.
///
/// Single source of truth for "supported CPU pix_fmt." Used by:
/// - the safe `Frame::*` row accessors (via `plane_row_bytes_for` /
///   `plane_height_for`, which agree with this helper — every format
///   that returns `Some` from those functions is also accepted here).
/// - `mediadecode-ffmpeg`'s `transfer_hw_frame` post-transfer
///   validation — if FFmpeg's auto-pick produces a format outside this
///   set, treat as a backend failure so probe advances rather than
///   collapsing on an unusable frame.
pub fn is_supported_cpu_pix_fmt(pix_fmt: &PixelFormat) -> bool {
  matches!(
    pix_fmt,
    // --- HW download outputs (NV* + P0xx/P2xx/P4xx) ---
    PixelFormat::Nv12
      | PixelFormat::Nv21
      | PixelFormat::Nv16
      | PixelFormat::Nv24
      | PixelFormat::P010Le
      | PixelFormat::P012Le
      | PixelFormat::P016Le
      | PixelFormat::P210Le
      | PixelFormat::P212Le
      | PixelFormat::P216Le
      | PixelFormat::P410Le
      | PixelFormat::P412Le
      | PixelFormat::P416Le
      // --- SW decoder outputs: planar YUV ---
      | PixelFormat::Yuv420p
      | PixelFormat::Yuv422p
      | PixelFormat::Yuv444p
      | PixelFormat::Yuv420p10Le
      | PixelFormat::Yuv420p12Le
      | PixelFormat::Yuv420p16Le
      | PixelFormat::Yuv422p10Le
      | PixelFormat::Yuv422p12Le
      | PixelFormat::Yuv422p16Le
      | PixelFormat::Yuv444p10Le
      | PixelFormat::Yuv444p12Le
      | PixelFormat::Yuv444p16Le
      // --- SW decoder outputs: packed RGB ---
      | PixelFormat::Rgb24
      | PixelFormat::Bgr24
      | PixelFormat::Rgba
      | PixelFormat::Bgra
      | PixelFormat::Argb
      | PixelFormat::Abgr
      // --- SW decoder outputs: greyscale ---
      | PixelFormat::Gray8
      | PixelFormat::Gray16Le
  )
}

/// Visible byte width of `plane`'s rows for a frame of `frame_width` and
/// the given pixel format. `None` for formats not in the supported HW-
/// output set.
///
/// Distinct from `linesize` (FFmpeg's per-row stride, which may include
/// alignment padding). HW transfer paths only initialize bytes
/// `0..plane_row_bytes_for(...)` of each row; everything from there to
/// `stride` is uninitialized padding and must not be exposed via
/// `slice::from_raw_parts`.
pub(crate) fn plane_row_bytes_for(
  pix_fmt: &PixelFormat,
  plane: usize,
  frame_width: usize,
) -> Option<usize> {
  match pix_fmt {
    // 8-bit semi-planar 4:2:0 / 4:2:2: Y at full width (1 byte/sample);
    // UV interleaved at horizontally-subsampled chroma with `ceil(W/2)`
    // U+V pairs at 2 bytes per pair. For even W the chroma row equals
    // `W` bytes (the simple case); for odd W it must round *up* to the
    // next even byte so the trailing chroma sample is not silently
    // dropped on width = 2k+1 frames.
    PixelFormat::Nv12 | PixelFormat::Nv21 | PixelFormat::Nv16 => match plane {
      0 => Some(frame_width),
      1 => Some(frame_width.div_ceil(2).checked_mul(2)?),
      _ => None,
    },
    // 8-bit 4:4:4 semi-planar: chroma at full horizontal resolution,
    // 2 bytes per pixel (1 byte U + 1 byte V) — no rounding required.
    PixelFormat::Nv24 => match plane {
      0 => Some(frame_width),
      1 => Some(frame_width.checked_mul(2)?),
      _ => None,
    },
    // 10/12/16-bit semi-planar 4:2:0 / 4:2:2: Y is 2 bytes/sample
    // (high-bit-depth packed in 16-bit). UV interleaved at horizontally-
    // subsampled chroma with `ceil(W/2)` U+V pairs at 4 bytes per pair
    // (2 bytes U + 2 bytes V). Same odd-width rounding as the 8-bit
    // chroma path, scaled by 2 bytes per sample.
    PixelFormat::P010Le
    | PixelFormat::P012Le
    | PixelFormat::P016Le
    | PixelFormat::P210Le
    | PixelFormat::P212Le
    | PixelFormat::P216Le => match plane {
      0 => Some(frame_width.checked_mul(2)?),
      1 => Some(frame_width.div_ceil(2).checked_mul(4)?),
      _ => None,
    },
    // 10/12/16-bit 4:4:4 semi-planar: Y is 2 bytes/sample; UV at full
    // horizontal resolution with 4 bytes per pixel (2 bytes U + 2 bytes V).
    PixelFormat::P410Le | PixelFormat::P412Le | PixelFormat::P416Le => match plane {
      0 => Some(frame_width.checked_mul(2)?),
      1 => Some(frame_width.checked_mul(4)?),
      _ => None,
    },
    // --- SW planar YUV 4:2:0 8-bit ---
    PixelFormat::Yuv420p => match plane {
      0 => Some(frame_width),
      1 | 2 => Some(frame_width.div_ceil(2)),
      _ => None,
    },
    // --- SW planar YUV 4:2:2 8-bit ---
    PixelFormat::Yuv422p => match plane {
      0 => Some(frame_width),
      1 | 2 => Some(frame_width.div_ceil(2)),
      _ => None,
    },
    // --- SW planar YUV 4:4:4 8-bit ---
    PixelFormat::Yuv444p => match plane {
      0..=2 => Some(frame_width),
      _ => None,
    },
    // --- SW planar YUV 4:2:0 10/12/16-bit (low-packed in u16) ---
    PixelFormat::Yuv420p10Le | PixelFormat::Yuv420p12Le | PixelFormat::Yuv420p16Le => match plane {
      0 => Some(frame_width.checked_mul(2)?),
      1 | 2 => Some(frame_width.div_ceil(2).checked_mul(2)?),
      _ => None,
    },
    // --- SW planar YUV 4:2:2 10/12/16-bit ---
    PixelFormat::Yuv422p10Le | PixelFormat::Yuv422p12Le | PixelFormat::Yuv422p16Le => match plane {
      0 => Some(frame_width.checked_mul(2)?),
      1 | 2 => Some(frame_width.div_ceil(2).checked_mul(2)?),
      _ => None,
    },
    // --- SW planar YUV 4:4:4 10/12/16-bit ---
    PixelFormat::Yuv444p10Le | PixelFormat::Yuv444p12Le | PixelFormat::Yuv444p16Le => match plane {
      0..=2 => Some(frame_width.checked_mul(2)?),
      _ => None,
    },
    // --- SW packed RGB 8-bit (3 bytes/pixel for RGB24/BGR24,
    //     4 bytes/pixel for RGBA/BGRA/ARGB/ABGR). Single plane. ---
    PixelFormat::Rgb24 | PixelFormat::Bgr24 => match plane {
      0 => Some(frame_width.checked_mul(3)?),
      _ => None,
    },
    PixelFormat::Rgba | PixelFormat::Bgra | PixelFormat::Argb | PixelFormat::Abgr => match plane {
      0 => Some(frame_width.checked_mul(4)?),
      _ => None,
    },
    // --- SW greyscale ---
    PixelFormat::Gray8 => match plane {
      0 => Some(frame_width),
      _ => None,
    },
    PixelFormat::Gray16Le => match plane {
      0 => Some(frame_width.checked_mul(2)?),
      _ => None,
    },
    _ => None,
  }
}

/// Number of rows in `plane` for a frame of `frame_height` and the given
/// pixel format. `None` for formats not in the supported HW-output set.
///
/// Crate-internal: the row accessors read it, so they and
/// [`is_supported_cpu_pix_fmt`] keep one chroma-subsampling table.
pub(crate) fn plane_height_for(
  pix_fmt: &PixelFormat,
  plane: usize,
  frame_height: usize,
) -> Option<usize> {
  match pix_fmt {
    // 4:2:0 semi-planar — Y full height, chroma half height.
    PixelFormat::Nv12
    | PixelFormat::Nv21
    | PixelFormat::P010Le
    | PixelFormat::P012Le
    | PixelFormat::P016Le => match plane {
      0 => Some(frame_height),
      1 => Some(frame_height.div_ceil(2)),
      _ => None,
    },
    // 4:2:2 / 4:4:4 semi-planar — both planes full height.
    PixelFormat::Nv16
    | PixelFormat::Nv24
    | PixelFormat::P210Le
    | PixelFormat::P212Le
    | PixelFormat::P216Le
    | PixelFormat::P410Le
    | PixelFormat::P412Le
    | PixelFormat::P416Le => match plane {
      0 | 1 => Some(frame_height),
      _ => None,
    },
    // --- SW planar YUV 4:2:0: Y full, U/V half-height ---
    PixelFormat::Yuv420p
    | PixelFormat::Yuv420p10Le
    | PixelFormat::Yuv420p12Le
    | PixelFormat::Yuv420p16Le => match plane {
      0 => Some(frame_height),
      1 | 2 => Some(frame_height.div_ceil(2)),
      _ => None,
    },
    // --- SW planar YUV 4:2:2 / 4:4:4: all planes full height ---
    PixelFormat::Yuv422p
    | PixelFormat::Yuv422p10Le
    | PixelFormat::Yuv422p12Le
    | PixelFormat::Yuv422p16Le
    | PixelFormat::Yuv444p
    | PixelFormat::Yuv444p10Le
    | PixelFormat::Yuv444p12Le
    | PixelFormat::Yuv444p16Le => match plane {
      0..=2 => Some(frame_height),
      _ => None,
    },
    // --- SW packed RGB / greyscale: single plane, full height ---
    PixelFormat::Rgb24
    | PixelFormat::Bgr24
    | PixelFormat::Rgba
    | PixelFormat::Bgra
    | PixelFormat::Argb
    | PixelFormat::Abgr
    | PixelFormat::Gray8
    | PixelFormat::Gray16Le => match plane {
      0 => Some(frame_height),
      _ => None,
    },
    _ => None,
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use ffmpeg_next::ffi::AVPixelFormat;

  #[test]
  fn empty_frame_has_zero_dimensions_and_no_pts() {
    let f = Frame::empty().expect("alloc");
    assert_eq!(f.width(), 0);
    assert_eq!(f.height(), 0);
    assert_eq!(f.pts(), None);
    // AVFrame.format defaults to -1 (AV_PIX_FMT_NONE) for an empty frame.
    assert_eq!(f.pix_fmt(), PixelFormat::None);
    // No active planes for an empty frame (all linesize entries are 0).
    assert_eq!(f.planes(), 0);
  }

  #[test]
  fn row_returns_none_for_unknown_format() {
    let f = Frame::empty().expect("alloc");
    // pix_fmt is NONE (-1), not in the supported set.
    assert!(f.row(0, 0).is_none());
    assert!(f.rows(0).is_none());
    assert!(f.row_bytes(0).is_none());
  }

  /// Synthesize a frame with a negative linesize (FFmpeg's vertical-flip
  /// convention) and assert the row accessors refuse to construct a slice.
  /// Without the linesize > 0 check, the negative `i32 as usize` would
  /// produce a huge positive length and `from_raw_parts` would be UB.
  ///
  /// `as_ptr` shares the same validation — handing back the data pointer
  /// for a negative-stride frame would let a downstream converter
  /// following the "valid for stride × plane_h bytes forward" contract
  /// walk past the buffer.
  #[test]
  fn row_returns_none_for_negative_linesize() {
    let mut f = Frame::empty().expect("alloc");
    unsafe {
      let raw = f.inner.as_mut_ptr();
      (*raw).format = AVPixelFormat::AV_PIX_FMT_NV12 as i32;
      (*raw).width = 1920;
      (*raw).height = 1080;
      (*raw).linesize[0] = -1920; // vertically-flipped
      (*raw).linesize[1] = -1920;
      // data pointers stay null; the accessors would also reject on null,
      // but should bail earlier on the linesize sign.
    }
    assert!(f.row(0, 0).is_none());
    assert!(f.row(1, 0).is_none());
    assert!(f.rows(0).is_none());
    assert!(
      f.as_ptr(0).is_none(),
      "as_ptr must share row()/rows() validation — a negative-stride \
       frame must not leak a forward-readable plane pointer"
    );
    assert!(f.as_ptr(1).is_none());
  }

  #[test]
  fn row_returns_none_for_non_positive_height() {
    let mut f = Frame::empty().expect("alloc");
    unsafe {
      let raw = f.inner.as_mut_ptr();
      (*raw).format = AVPixelFormat::AV_PIX_FMT_NV12 as i32;
      (*raw).width = 1920;
      (*raw).height = 0;
      (*raw).linesize[0] = 1920;
      (*raw).linesize[1] = 1920;
    }
    assert!(f.row(0, 0).is_none());
  }

  /// Synthesize a frame backed by a manually-allocated buffer with stride
  /// strictly larger than visible row bytes (the exact case where
  /// FFmpeg's HW transfer leaves trailing padding uninitialized) and
  /// confirm the safe row accessor returns slices clipped to the visible
  /// width.
  #[test]
  fn row_clips_to_visible_width_not_stride() {
    use std::alloc::{Layout, alloc, dealloc};
    let width = 64usize;
    let height = 4usize;
    // Stride > width: 16 bytes of padding per row in the Y plane.
    let stride = 80usize;
    let plane_size = stride * height;
    // Allocate ourselves so we can fully control initialization. Fill
    // bytes 0..width with 0xAA per row (the "valid pixel" range) and
    // bytes width..stride with 0xFF (the simulated alignment padding —
    // FFmpeg would leave these uninitialized; we set them to a sentinel
    // that the test can detect if the safe slice ever exposes them).
    let layout = Layout::from_size_align(plane_size, 32).unwrap();
    let buf = unsafe { alloc(layout) };
    assert!(!buf.is_null());
    for y in 0..height {
      let row = unsafe { buf.add(y * stride) };
      for x in 0..width {
        unsafe { *row.add(x) = 0xAA };
      }
      for x in width..stride {
        unsafe { *row.add(x) = 0xFF };
      }
    }

    let mut f = Frame::empty().expect("alloc");
    unsafe {
      let raw = f.inner.as_mut_ptr();
      (*raw).format = AVPixelFormat::AV_PIX_FMT_NV12 as i32;
      (*raw).width = width as i32;
      (*raw).height = height as i32;
      (*raw).linesize[0] = stride as i32;
      // linesize[1] = 0 keeps planes() at 1 so the test stays focused on
      // plane 0 without owning a second allocation.
      (*raw).data[0] = buf;
    }

    assert_eq!(f.row_bytes(0), Some(width));
    assert_eq!(f.stride(0), stride);
    let row0 = f.row(0, 0).expect("row 0");
    assert_eq!(
      row0.len(),
      width,
      "safe row must be clipped to visible width"
    );
    assert!(
      row0.iter().all(|&b| b == 0xAA),
      "row must not include padding sentinel 0xFF"
    );

    let collected: Vec<&[u8]> = f.rows(0).expect("rows iterator").collect();
    assert_eq!(collected.len(), height);
    for r in &collected {
      assert_eq!(r.len(), width);
      assert!(r.iter().all(|&b| b == 0xAA));
    }

    // `as_ptr` accepts the valid layout and returns the same base pointer
    // FFmpeg wrote into `data[0]`, so SIMD callers can reach the plane
    // through the documented unsafe contract.
    assert_eq!(
      f.as_ptr(0),
      Some(buf as *const u8),
      "as_ptr must surface the plane base for a valid forward-stride frame"
    );

    // Out-of-range row index returns None instead of panicking.
    assert!(f.row(0, height).is_none());

    // Detach the buffer before drop so AVFrame's own free path doesn't
    // touch our manual allocation.
    unsafe {
      (*f.inner.as_mut_ptr()).data[0] = std::ptr::null_mut();
      dealloc(buf, layout);
    }
  }

  #[test]
  #[should_panic(expected = "non-positive linesize")]
  fn stride_panics_on_negative_linesize() {
    let mut f = Frame::empty().expect("alloc");
    unsafe {
      let raw = f.inner.as_mut_ptr();
      (*raw).linesize[0] = -1920;
    }
    let _ = f.stride(0);
  }

  #[test]
  fn frame_is_send() {
    fn check<T: Send>() {}
    check::<Frame>();
  }

  #[test]
  fn plane_height_table_covers_supported_formats() {
    // Spot-check the chroma subsampling table.
    assert_eq!(plane_height_for(&PixelFormat::Nv12, 0, 1080), Some(1080));
    assert_eq!(plane_height_for(&PixelFormat::Nv12, 1, 1080), Some(540));
    assert_eq!(plane_height_for(&PixelFormat::Nv12, 1, 1081), Some(541));
    assert_eq!(plane_height_for(&PixelFormat::P010Le, 1, 1080), Some(540));
    assert_eq!(plane_height_for(&PixelFormat::Nv16, 1, 1080), Some(1080));
    assert_eq!(plane_height_for(&PixelFormat::Nv24, 1, 1080), Some(1080));
    assert_eq!(plane_height_for(&PixelFormat::P416Le, 1, 1080), Some(1080));
    assert_eq!(plane_height_for(&PixelFormat::None, 0, 1080), None);
    assert_eq!(plane_height_for(&PixelFormat::Nv12, 2, 1080), None);
  }

  /// 4:2:0 / 4:2:2 chroma planes carry `ceil(W/2)` U+V pairs per row.
  /// For odd `W`, dropping the round-up silently truncates the last chroma
  /// sample — and the safe row slice would expose a buffer one byte (8-bit)
  /// or two bytes (high-bit-depth) shorter than the data FFmpeg actually
  /// wrote. Y planes and 4:4:4 chroma planes are unaffected because their
  /// row count is just `W` or a fixed multiple of `W`.
  #[test]
  fn plane_row_bytes_rounds_up_chroma_for_odd_widths() {
    // 8-bit subsampled chroma — odd W gains one byte (the missing sample
    // pair).
    assert_eq!(plane_row_bytes_for(&PixelFormat::Nv12, 1, 1921), Some(1922));
    assert_eq!(plane_row_bytes_for(&PixelFormat::Nv21, 1, 1921), Some(1922));
    assert_eq!(plane_row_bytes_for(&PixelFormat::Nv16, 1, 1921), Some(1922));
    // High-bit-depth subsampled chroma — odd W gains two bytes.
    assert_eq!(
      plane_row_bytes_for(&PixelFormat::P010Le, 1, 1921),
      Some(3844)
    );
    assert_eq!(
      plane_row_bytes_for(&PixelFormat::P010Le, 1, 1921),
      Some(3844)
    );
    assert_eq!(
      plane_row_bytes_for(&PixelFormat::P012Le, 1, 1921),
      Some(3844)
    );
    assert_eq!(
      plane_row_bytes_for(&PixelFormat::P016Le, 1, 1921),
      Some(3844)
    );
    assert_eq!(
      plane_row_bytes_for(&PixelFormat::P210Le, 1, 1921),
      Some(3844)
    );
    assert_eq!(
      plane_row_bytes_for(&PixelFormat::P212Le, 1, 1921),
      Some(3844)
    );
    assert_eq!(
      plane_row_bytes_for(&PixelFormat::P216Le, 1, 1921),
      Some(3844)
    );
    // Y planes always at full width regardless of subsampling.
    assert_eq!(plane_row_bytes_for(&PixelFormat::Nv12, 0, 1921), Some(1921));
    assert_eq!(
      plane_row_bytes_for(&PixelFormat::P010Le, 0, 1921),
      Some(3842)
    );
    // 4:4:4 chroma is at full horizontal resolution — no rounding.
    assert_eq!(plane_row_bytes_for(&PixelFormat::Nv24, 1, 1921), Some(3842));
    assert_eq!(
      plane_row_bytes_for(&PixelFormat::P410Le, 1, 1921),
      Some(7684)
    );
    // Even widths must still match the original (pre-fix) values so the
    // change is purely additive on the dominant code path.
    assert_eq!(plane_row_bytes_for(&PixelFormat::Nv12, 1, 1920), Some(1920));
    assert_eq!(
      plane_row_bytes_for(&PixelFormat::P010Le, 1, 1920),
      Some(3840)
    );
  }

  #[test]
  fn plane_row_bytes_table_covers_supported_formats() {
    // 8-bit 4:2:0 / 4:2:2 — both planes at width.
    assert_eq!(plane_row_bytes_for(&PixelFormat::Nv12, 0, 1920), Some(1920));
    assert_eq!(plane_row_bytes_for(&PixelFormat::Nv12, 1, 1920), Some(1920));
    assert_eq!(plane_row_bytes_for(&PixelFormat::Nv21, 1, 1920), Some(1920));
    assert_eq!(plane_row_bytes_for(&PixelFormat::Nv16, 1, 1920), Some(1920));
    // 8-bit 4:4:4 — chroma plane is 2 * width.
    assert_eq!(plane_row_bytes_for(&PixelFormat::Nv24, 0, 1920), Some(1920));
    assert_eq!(plane_row_bytes_for(&PixelFormat::Nv24, 1, 1920), Some(3840));
    // 10/12/16-bit 4:2:0 / 4:2:2 — both planes at 2 * width.
    assert_eq!(
      plane_row_bytes_for(&PixelFormat::P010Le, 0, 1920),
      Some(3840)
    );
    assert_eq!(
      plane_row_bytes_for(&PixelFormat::P010Le, 1, 1920),
      Some(3840)
    );
    assert_eq!(
      plane_row_bytes_for(&PixelFormat::P210Le, 1, 1920),
      Some(3840)
    );
    // 10/12/16-bit 4:4:4 — Y is 2 * width, chroma is 4 * width.
    assert_eq!(
      plane_row_bytes_for(&PixelFormat::P410Le, 0, 1920),
      Some(3840)
    );
    assert_eq!(
      plane_row_bytes_for(&PixelFormat::P410Le, 1, 1920),
      Some(7680)
    );
    assert_eq!(
      plane_row_bytes_for(&PixelFormat::P416Le, 1, 1920),
      Some(7680)
    );
    // Unsupported / out-of-range.
    assert_eq!(plane_row_bytes_for(&PixelFormat::None, 0, 1920), None);
    assert_eq!(plane_row_bytes_for(&PixelFormat::Nv12, 2, 1920), None);
  }

  /// Every format `is_supported_cpu_pix_fmt` accepts must also have a
  /// row-byte table entry (otherwise `Frame::row_bytes` would return
  /// `None` for a "supported" format), and every format the table
  /// accepts must be in `is_supported_cpu_pix_fmt`. The
  /// post-transfer validator and the safe row accessor must agree
  /// on what's usable.
  #[test]
  fn is_supported_cpu_pix_fmt_agrees_with_row_byte_table() {
    let supported = [
      PixelFormat::Nv12,
      PixelFormat::Nv21,
      PixelFormat::Nv16,
      PixelFormat::Nv24,
      PixelFormat::P010Le,
      PixelFormat::P010Le,
      PixelFormat::P012Le,
      PixelFormat::P016Le,
      PixelFormat::P210Le,
      PixelFormat::P212Le,
      PixelFormat::P216Le,
      PixelFormat::P410Le,
      PixelFormat::P412Le,
      PixelFormat::P416Le,
    ];
    for fmt in &supported {
      assert!(
        is_supported_cpu_pix_fmt(fmt),
        "is_supported_cpu_pix_fmt rejected pix_fmt {fmt:?}, but the row-byte \
         table accepts it — the two are out of sync"
      );
      assert!(
        plane_row_bytes_for(fmt, 0, 1920).is_some(),
        "plane_row_bytes_for rejected pix_fmt {fmt:?}, but \
         is_supported_cpu_pix_fmt accepts it — out of sync"
      );
      assert!(
        plane_height_for(fmt, 0, 1080).is_some(),
        "plane_height_for rejected pix_fmt {fmt:?} — out of sync"
      );
    }
  }

  /// Common CPU formats outside the supported HW-output set must be
  /// rejected. These are the formats a misbehaving driver might pick
  /// for `av_hwframe_transfer_data`'s auto-format selection that the
  /// safe `Frame` accessors would silently fail on.
  #[test]
  fn is_supported_cpu_pix_fmt_rejects_common_unsupported_formats() {
    use ffmpeg_next::ffi::AVPixelFormat;

    // AV_PIX_FMT_NONE sentinel and HW pix_fmts (those should never
    // surface post-transfer).
    assert!(!is_supported_cpu_pix_fmt(&PixelFormat::None));
    assert!(!is_supported_cpu_pix_fmt(&boundary::from_av_pixel_format(
      AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX as i32
    )));
    assert!(!is_supported_cpu_pix_fmt(&boundary::from_av_pixel_format(
      AVPixelFormat::AV_PIX_FMT_VAAPI as i32
    )));
    assert!(!is_supported_cpu_pix_fmt(&boundary::from_av_pixel_format(
      AVPixelFormat::AV_PIX_FMT_CUDA as i32
    )));
    assert!(!is_supported_cpu_pix_fmt(&boundary::from_av_pixel_format(
      AVPixelFormat::AV_PIX_FMT_D3D11 as i32
    )));

    // YUVJ420P (deprecated full-range marker) maps to its own
    // `PixelFormat::Yuvj420p`, which is outside this narrow HW-output
    // gate (the range itself is carried on `ColorInfo::range`). The
    // descriptor-driven convert path still handles it; this gate just
    // doesn't list the J family among the formats the safe `Frame::row`
    // accessors support.
    assert!(!is_supported_cpu_pix_fmt(&boundary::from_av_pixel_format(
      AVPixelFormat::AV_PIX_FMT_YUVJ420P as i32
    )));

    // Note: YUV420P / YUV422P / YUV444P / RGB24 / BGR24 / RGBA / BGRA
    // are now intentionally **supported** (added when SW fallback
    // landed in the FfmpegVideoStreamDecoder). They previously appeared
    // here as "unsupported" when this crate was HW-only.

    // A future / unknown format value FFmpeg might invent — the helper
    // is closed-set so unknown integers are always rejected without
    // constructing the bindgen enum.
    assert!(!is_supported_cpu_pix_fmt(&boundary::from_av_pixel_format(
      99_999_999
    )));
  }
}
