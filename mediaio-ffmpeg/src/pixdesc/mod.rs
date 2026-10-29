//! Descriptor-driven CPU pixel-format geometry.
//!
//! The safe plane extraction in [`crate::convert::av_frame_to_video_frame`]
//! and the safe row accessors on [`crate::Frame`] need three facts about a
//! decoded frame's pixel format: how many planes it has, how many rows each
//! plane carries, and how many *initialised* bytes there are at the start of
//! every row (the "visible" byte width, distinct from `AVFrame.linesize`,
//! which is padded for SIMD alignment). Getting any of those wrong is a
//! memory-safety bug — a too-large per-plane size makes the unsafe slice
//! construction read past the codec's allocation (UB), and a too-small one
//! silently truncates pixel data.
//!
//! ## Scope — which formats are deliverable
//!
//! This covers every FFmpeg CPU pixel format whose planes are **plain,
//! byte-row-addressable memory** — i.e. all planar / semi-planar / packed
//! YUV, RGB, GBR, greyscale, alpha, float, and XYZ at every bit depth and
//! endianness. Four descriptor-flagged families are deliberately **excluded**
//! ([`is_deliverable`] rejects them by flag, and [`crate::convert`] returns
//! `UnsupportedPixelFormat`), because they are not directly byte-row
//! addressable and each needs dedicated handling out of scope here:
//! **Bayer** mosaics (need demosaicing — colconv roadmap), **hardware-surface**
//! formats (the HW path transfers to a CPU format before delivery),
//! **paletted** (`PAL8` — index plane plus a side palette), and **sub-byte
//! bitstream** packings (`MONOWHITE`/`MONOBLACK`/`RGB4`/`BGR4` — component step
//! measured in bits). So "all CPU formats" means all *byte-addressable* ones;
//! the four families above are explicit, tested exclusions, not gaps.
//!
//! Rather than hand-maintain a per-format geometry table (correct only for a
//! closed set, and a fresh UB hazard for every format added by hand), this
//! module derives the geometry from FFmpeg's own [`AVPixFmtDescriptor`] via
//! the exact functions the rest of libavutil uses to size image buffers:
//!
//! * [`av_image_fill_linesizes`] computes the *tight* linesize (row bytes)
//!   per plane from the format and width. This is the visible byte width —
//!   for packed/sub-sampled/bitstream formats it already accounts for
//!   component step, bit depth, and chroma sub-sampling.
//! * [`av_image_fill_plane_sizes`] computes each plane's total byte size
//!   from the (tight) linesizes and height. Dividing the plane size by the
//!   tight linesize yields the plane's row count exactly — including the
//!   `AV_CEIL_RSHIFT(height, log2_chroma_h)` sub-sampling for chroma planes.
//!
//! Because production geometry and the test oracle both come from these same
//! authoritative libavutil functions, the per-plane height / row-bytes are
//! correct *by construction* for every CPU format FFmpeg can describe.
//!
//! ## Safety stance
//!
//! The crate never constructs an `AVPixelFormat` from a runtime integer
//! (forming an out-of-range enum value is instantaneous UB, and FFmpeg
//! header/library skew can put unknown values in `AVFrame.format`). This
//! module preserves that: the only bridge from a runtime value to an
//! `AVPixelFormat` is [`to_av_pixel_format`], which maps a *recognised*
//! [`PixelFormat`] onto a compile-time `AVPixelFormat::AV_PIX_FMT_*`
//! **constant** — never `transmute`/`as`-casts an integer into the enum.
//! `from_av_pixel_format` (the boundary) maps the raw frame integer to
//! `PixelFormat`; `to_av_pixel_format` maps it back to a known constant; the
//! two round-trip to the same raw value for every deliverable format (proven
//! in tests), so the descriptor we fetch geometry from is genuinely the
//! descriptor of the frame's own format.

use ffmpeg_next::ffi::{
  AV_PIX_FMT_FLAG_BAYER, AV_PIX_FMT_FLAG_BITSTREAM, AV_PIX_FMT_FLAG_HWACCEL, AV_PIX_FMT_FLAG_PAL,
  AVPixelFormat, av_image_fill_linesizes, av_image_fill_plane_sizes, av_pix_fmt_count_planes,
  av_pix_fmt_desc_get,
};
use mediaspine::PixelFormat;

/// Maximum planes a `VideoFrame` (and libavutil's image-fill helpers)
/// represent. `av_image_fill_linesizes` / `av_image_fill_plane_sizes`
/// fill `[_; 4]`.
pub(crate) const MAX_PLANES: usize = 4;

/// Per-plane geometry derived from libavutil for a concrete `(format,
/// width, height)`.
///
/// `count` planes are populated; for `i < count`, `row_bytes[i]` is the
/// tight (visible) byte width of a row and `height[i]` is the plane's row
/// count. Entries at `i >= count` are zero.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PlaneGeometry {
  /// Number of populated planes (`1..=MAX_PLANES`).
  pub(crate) count: usize,
  /// Tight (visible) byte width per row, per plane.
  pub(crate) row_bytes: [usize; MAX_PLANES],
  /// Row count per plane.
  pub(crate) height: [usize; MAX_PLANES],
  /// The index of the palette plane, when this format has one.
  ///
  /// A palette is **not a strided plane**: FFmpeg leaves its
  /// `linesize` at zero and points `data[1]` at a flat
  /// `AVPALETTE_SIZE` run. A copier that treated it like an image
  /// plane would read a zero row width and refuse the frame, which is
  /// exactly what happened first.
  pub(crate) palette_plane: Option<usize>,
}

/// Whether `pix_fmt` is a CPU pixel format whose planes this crate can
/// safely extract — i.e. libavutil has a descriptor for it and that
/// descriptor describes a plain CPU memory layout.
///
/// Rejected (returns `false`):
/// * [`PixelFormat::None`] — no format, so no descriptor.
/// * Hardware-surface formats (`AV_PIX_FMT_FLAG_HWACCEL`) — these never
///   describe CPU-side pixel bytes; the HW path transfers to a CPU format
///   before delivery.
/// * Bayer mosaics (`AV_PIX_FMT_FLAG_BAYER`) — deferred to colconv's
///   demosaic path; their "planes" aren't directly consumable as image
///   rows here.
/// * Paletted (`AV_PIX_FMT_FLAG_PAL`) — the data plane is palette indices
///   plus a side palette plane libavutil sizes specially; out of scope.
/// * Sub-byte bitstream packings (`AV_PIX_FMT_FLAG_BITSTREAM`, e.g.
///   `MONOBLACK`/`MONOWHITE`/`RGB4`/`BGR4`) — component step is measured in
///   *bits*, so a byte-granular row slice can't represent them losslessly.
///
/// Everything else FFmpeg can describe (planar/semi-planar/packed YUV, RGB,
/// GBR, greyscale, alpha, float, XYZ, at every endianness/bit-depth) is
/// accepted: its geometry is fully determined by the descriptor.
pub(crate) fn is_deliverable(pix_fmt: &PixelFormat) -> bool {
  geometry_descriptor(pix_fmt).is_some()
}

/// Resolve the format to a known `AVPixelFormat` constant and confirm its
/// descriptor describes a plain CPU layout; returns the constant on
/// success so callers don't repeat the mapping.
fn geometry_descriptor(pix_fmt: &PixelFormat) -> Option<AVPixelFormat> {
  descriptor_for(
    pix_fmt,
    AV_PIX_FMT_FLAG_HWACCEL
      | AV_PIX_FMT_FLAG_BAYER
      | AV_PIX_FMT_FLAG_PAL
      | AV_PIX_FMT_FLAG_BITSTREAM,
  )
}

/// [`geometry_descriptor`], for the **still** road, which accepts two
/// layouts the video road refuses.
///
/// A cover image is routinely an indexed PNG (`pal8`) or a 1-bit one
/// (`monob`), and refusing those left a real picture undecodable. The
/// video road keeps refusing them: motion video in these formats
/// essentially does not exist, and widening it would change what every
/// existing video consumer can be handed.
///
/// **Nothing is converted.** mediadecode delivers what FFmpeg decoded —
/// pixel conversion is colconv's job downstream — so a `pal8` still
/// arrives as its indices plus its palette, and a `monob` still arrives
/// as packed bits. Both are *format*-bounded, not budget-bounded: the
/// palette is structurally 1024 bytes and a 1-bit row is
/// `ceil(width / 8)`, and libavutil computes both.
fn still_descriptor(pix_fmt: &PixelFormat) -> Option<AVPixelFormat> {
  descriptor_for(pix_fmt, AV_PIX_FMT_FLAG_HWACCEL | AV_PIX_FMT_FLAG_BAYER)
}

fn descriptor_for(pix_fmt: &PixelFormat, rejected: i32) -> Option<AVPixelFormat> {
  let av = to_av_pixel_format(pix_fmt)?;
  // SAFETY: `av` is a known `AV_PIX_FMT_*` constant (never an integer cast
  // into the enum). `av_pix_fmt_desc_get` returns a pointer to a static
  // descriptor or null; we only read it through the returned pointer.
  let desc = unsafe { av_pix_fmt_desc_get(av) };
  if desc.is_null() {
    return None;
  }
  // SAFETY: non-null per the check above; the descriptor is a
  // `'static` libavutil table entry. `flags`/`nb_components` are plain
  // integer fields.
  let flags = unsafe { (*desc).flags };
  let nb_components = unsafe { (*desc).nb_components };
  if flags & (rejected as u64) != 0 {
    return None;
  }
  // A descriptor with zero components carries no image data we can size.
  if nb_components == 0 {
    return None;
  }
  Some(av)
}

/// Compute per-plane geometry for `pix_fmt` at `width` × `height` using
/// libavutil's authoritative image-fill helpers.
///
/// Returns `None` when:
/// * the format isn't deliverable (see [`is_deliverable`]),
/// * `width`/`height` exceed `c_int::MAX` (libavutil's parameters are
///   `int`),
/// * libavutil reports an error or a non-positive plane count, or
/// * any plane has a zero/garbage tight linesize while reporting a
///   non-zero size (which would make the row-count division ill-defined).
///
/// `width` and `height` are the frame's coded dimensions
/// (`AVFrame.width`/`.height`).
pub(crate) fn plane_geometry(
  pix_fmt: &PixelFormat,
  width: usize,
  height: usize,
) -> Option<PlaneGeometry> {
  geometry_from(geometry_descriptor(pix_fmt)?, width, height, false)
}

/// Whether the **still** road can deliver `pix_fmt` — everything
/// [`is_deliverable`] accepts, plus paletted and sub-byte bitstream
/// layouts. See [`still_descriptor`].
pub(crate) fn is_still_deliverable(pix_fmt: &PixelFormat) -> bool {
  still_descriptor(pix_fmt).is_some()
}

/// [`plane_geometry`] for the still road.
///
/// One extra fact over the shared road: a paletted format carries its
/// palette in `data[1]`, which `av_pix_fmt_count_planes` does not count
/// as a plane but `av_image_fill_plane_sizes` does size — at
/// `AVPALETTE_SIZE`, a fixed 1024 bytes, always. Measured on this
/// build: `PAL8` at 30×20 reports `count_planes = 1`,
/// `linesize = [30, …]`, `sizes = [600, 1024, …]`.
pub(crate) fn still_plane_geometry(
  pix_fmt: &PixelFormat,
  width: usize,
  height: usize,
) -> Option<PlaneGeometry> {
  let av = still_descriptor(pix_fmt)?;
  // SAFETY: `av` is a known `AV_PIX_FMT_*` constant.
  let desc = unsafe { av_pix_fmt_desc_get(av) };
  if desc.is_null() {
    return None;
  }
  // SAFETY: non-null per the check above; `flags` is a plain field.
  let paletted = unsafe { (*desc).flags } & (AV_PIX_FMT_FLAG_PAL as u64) != 0;
  geometry_from(av, width, height, paletted)
}

fn geometry_from(
  av: AVPixelFormat,
  width: usize,
  height: usize,
  paletted: bool,
) -> Option<PlaneGeometry> {
  let width_i = i32::try_from(width).ok()?;
  let height_i = i32::try_from(height).ok()?;
  if width_i <= 0 || height_i <= 0 {
    return None;
  }

  // Plane count from libavutil. `av_pix_fmt_count_planes` returns the
  // number of distinct `comp[].plane` indices used; for the formats we
  // accept it's `1..=MAX_PLANES`.
  // SAFETY: `av` is a known constant.
  let count_raw = unsafe { av_pix_fmt_count_planes(av) };
  if count_raw <= 0 || count_raw as usize > MAX_PLANES {
    return None;
  }
  let mut count = count_raw as usize;

  // Tight linesizes (visible row bytes) per plane. `av_image_fill_linesizes`
  // fills `[c_int; 4]` and returns < 0 on error.
  let mut linesizes_i: [core::ffi::c_int; MAX_PLANES] = [0; MAX_PLANES];
  // SAFETY: `linesizes_i` is a live `[c_int; 4]`; the function writes up to
  // 4 entries. `av` is a known constant; `width_i > 0`.
  let ret = unsafe { av_image_fill_linesizes(linesizes_i.as_mut_ptr(), av, width_i) };
  if ret < 0 {
    return None;
  }

  // Plane sizes need the linesizes as `ptrdiff_t` (isize).
  let mut linesizes_isize: [isize; MAX_PLANES] = [0; MAX_PLANES];
  for i in 0..MAX_PLANES {
    if linesizes_i[i] < 0 {
      // A negative tight linesize is never valid output here.
      return None;
    }
    linesizes_isize[i] = linesizes_i[i] as isize;
  }

  let mut sizes: [usize; MAX_PLANES] = [0; MAX_PLANES];
  // SAFETY: `sizes` is a live `[usize; 4]`; `linesizes_isize` is a live
  // `[isize; 4]` read-only input; `av` is a known constant; `height_i > 0`.
  let ret = unsafe {
    av_image_fill_plane_sizes(sizes.as_mut_ptr(), av, height_i, linesizes_isize.as_ptr())
  };
  if ret < 0 {
    return None;
  }

  let mut row_bytes = [0usize; MAX_PLANES];
  let mut plane_height = [0usize; MAX_PLANES];
  for i in 0..count {
    let ls = linesizes_i[i] as usize;
    let size = sizes[i];
    if ls == 0 {
      // A populated plane must have a positive tight linesize; without
      // it the row count is undefined.
      return None;
    }
    // `av_image_fill_plane_sizes` sets `size = linesize * rows` exactly,
    // so the division is exact. Guard against a non-multiple anyway —
    // it would signal a layout we don't model and must refuse rather
    // than truncate.
    if !size.is_multiple_of(ls) {
      return None;
    }
    row_bytes[i] = ls;
    plane_height[i] = size / ls;
    if plane_height[i] == 0 {
      return None;
    }
  }

  let mut palette_plane = None;
  if paletted {
    // The palette rides `data[1]` as a flat `AVPALETTE_SIZE` run: 256
    // entries of `AV_PIX_FMT_RGB32`, one row, never any other size.
    // A format bound, not a budget limit — there is no number here a
    // file gets to choose.
    if count >= MAX_PLANES {
      return None;
    }
    row_bytes[count] = PALETTE_BYTES;
    plane_height[count] = 1;
    palette_plane = Some(count);
    count += 1;
  }

  Some(PlaneGeometry {
    count,
    row_bytes,
    height: plane_height,
    palette_plane,
  })
}

/// `AVPALETTE_SIZE` — 256 `AV_PIX_FMT_RGB32` entries. Fixed by the
/// format, which is why the palette plane needs no budget of its own.
pub(crate) const PALETTE_BYTES: usize = 256 * 4;

/// Maps a recognised [`PixelFormat`] onto the matching compile-time
/// `AVPixelFormat::AV_PIX_FMT_*` constant.
///
/// Returns `None` for [`PixelFormat::None`] and for any variant with no
/// corresponding FFmpeg pixel format in the linked build. **Only literal
/// constants are produced** — never an integer cast into the enum — so this
/// is sound regardless of which discriminant set the linked FFmpeg exposes
/// (a constant absent from the build is a compile error, never UB).
///
/// This is the inverse of [`crate::boundary::from_av_pixel_format`] for the
/// formats that crate maps; the round-trip is asserted in tests.
pub(crate) const fn to_av_pixel_format(pix_fmt: &PixelFormat) -> Option<AVPixelFormat> {
  use AVPixelFormat as F;
  Some(match pix_fmt {
    // --- Planar YUV 8-bit ---
    PixelFormat::Yuv420p => F::AV_PIX_FMT_YUV420P,
    PixelFormat::Yuv422p => F::AV_PIX_FMT_YUV422P,
    PixelFormat::Yuv440p => F::AV_PIX_FMT_YUV440P,
    PixelFormat::Yuv444p => F::AV_PIX_FMT_YUV444P,
    PixelFormat::Yuv411p => F::AV_PIX_FMT_YUV411P,
    PixelFormat::Yuv410p => F::AV_PIX_FMT_YUV410P,
    // --- Deprecated JPEG-range planar YUV (yuvj-family) ---
    PixelFormat::Yuvj411p => F::AV_PIX_FMT_YUVJ411P,
    PixelFormat::Yuvj420p => F::AV_PIX_FMT_YUVJ420P,
    PixelFormat::Yuvj422p => F::AV_PIX_FMT_YUVJ422P,
    PixelFormat::Yuvj440p => F::AV_PIX_FMT_YUVJ440P,
    PixelFormat::Yuvj444p => F::AV_PIX_FMT_YUVJ444P,
    // --- Planar YUV 4:2:0 high-bit ---
    PixelFormat::Yuv420p9Le => F::AV_PIX_FMT_YUV420P9LE,
    PixelFormat::Yuv420p9Be => F::AV_PIX_FMT_YUV420P9BE,
    PixelFormat::Yuv420p10Le => F::AV_PIX_FMT_YUV420P10LE,
    PixelFormat::Yuv420p10Be => F::AV_PIX_FMT_YUV420P10BE,
    PixelFormat::Yuv420p12Le => F::AV_PIX_FMT_YUV420P12LE,
    PixelFormat::Yuv420p12Be => F::AV_PIX_FMT_YUV420P12BE,
    PixelFormat::Yuv420p14Le => F::AV_PIX_FMT_YUV420P14LE,
    PixelFormat::Yuv420p14Be => F::AV_PIX_FMT_YUV420P14BE,
    PixelFormat::Yuv420p16Le => F::AV_PIX_FMT_YUV420P16LE,
    PixelFormat::Yuv420p16Be => F::AV_PIX_FMT_YUV420P16BE,
    // --- Planar YUV 4:2:2 high-bit ---
    PixelFormat::Yuv422p9Le => F::AV_PIX_FMT_YUV422P9LE,
    PixelFormat::Yuv422p9Be => F::AV_PIX_FMT_YUV422P9BE,
    PixelFormat::Yuv422p10Le => F::AV_PIX_FMT_YUV422P10LE,
    PixelFormat::Yuv422p10Be => F::AV_PIX_FMT_YUV422P10BE,
    PixelFormat::Yuv422p12Le => F::AV_PIX_FMT_YUV422P12LE,
    PixelFormat::Yuv422p12Be => F::AV_PIX_FMT_YUV422P12BE,
    PixelFormat::Yuv422p14Le => F::AV_PIX_FMT_YUV422P14LE,
    PixelFormat::Yuv422p14Be => F::AV_PIX_FMT_YUV422P14BE,
    PixelFormat::Yuv422p16Le => F::AV_PIX_FMT_YUV422P16LE,
    PixelFormat::Yuv422p16Be => F::AV_PIX_FMT_YUV422P16BE,
    // --- Planar YUV 4:4:0 high-bit ---
    PixelFormat::Yuv440p10Le => F::AV_PIX_FMT_YUV440P10LE,
    PixelFormat::Yuv440p10Be => F::AV_PIX_FMT_YUV440P10BE,
    PixelFormat::Yuv440p12Le => F::AV_PIX_FMT_YUV440P12LE,
    PixelFormat::Yuv440p12Be => F::AV_PIX_FMT_YUV440P12BE,
    // --- Planar YUV 4:4:4 high-bit ---
    PixelFormat::Yuv444p9Le => F::AV_PIX_FMT_YUV444P9LE,
    PixelFormat::Yuv444p9Be => F::AV_PIX_FMT_YUV444P9BE,
    PixelFormat::Yuv444p10Le => F::AV_PIX_FMT_YUV444P10LE,
    PixelFormat::Yuv444p10Be => F::AV_PIX_FMT_YUV444P10BE,
    PixelFormat::Yuv444p12Le => F::AV_PIX_FMT_YUV444P12LE,
    PixelFormat::Yuv444p12Be => F::AV_PIX_FMT_YUV444P12BE,
    PixelFormat::Yuv444p14Le => F::AV_PIX_FMT_YUV444P14LE,
    PixelFormat::Yuv444p14Be => F::AV_PIX_FMT_YUV444P14BE,
    PixelFormat::Yuv444p16Le => F::AV_PIX_FMT_YUV444P16LE,
    PixelFormat::Yuv444p16Be => F::AV_PIX_FMT_YUV444P16BE,
    // --- MSB-packed YUV 4:4:4 ---
    PixelFormat::Yuv444p10MsbLe => F::AV_PIX_FMT_YUV444P10MSBLE,
    PixelFormat::Yuv444p10MsbBe => F::AV_PIX_FMT_YUV444P10MSBBE,
    PixelFormat::Yuv444p12MsbLe => F::AV_PIX_FMT_YUV444P12MSBLE,
    PixelFormat::Yuv444p12MsbBe => F::AV_PIX_FMT_YUV444P12MSBBE,
    // --- Planar YUVA ---
    PixelFormat::Yuva420p => F::AV_PIX_FMT_YUVA420P,
    PixelFormat::Yuva422p => F::AV_PIX_FMT_YUVA422P,
    PixelFormat::Yuva444p => F::AV_PIX_FMT_YUVA444P,
    PixelFormat::Yuva420p9Le => F::AV_PIX_FMT_YUVA420P9LE,
    PixelFormat::Yuva420p9Be => F::AV_PIX_FMT_YUVA420P9BE,
    PixelFormat::Yuva422p9Le => F::AV_PIX_FMT_YUVA422P9LE,
    PixelFormat::Yuva422p9Be => F::AV_PIX_FMT_YUVA422P9BE,
    PixelFormat::Yuva444p9Le => F::AV_PIX_FMT_YUVA444P9LE,
    PixelFormat::Yuva444p9Be => F::AV_PIX_FMT_YUVA444P9BE,
    PixelFormat::Yuva420p10Le => F::AV_PIX_FMT_YUVA420P10LE,
    PixelFormat::Yuva420p10Be => F::AV_PIX_FMT_YUVA420P10BE,
    PixelFormat::Yuva422p10Le => F::AV_PIX_FMT_YUVA422P10LE,
    PixelFormat::Yuva422p10Be => F::AV_PIX_FMT_YUVA422P10BE,
    PixelFormat::Yuva444p10Le => F::AV_PIX_FMT_YUVA444P10LE,
    PixelFormat::Yuva444p10Be => F::AV_PIX_FMT_YUVA444P10BE,
    // `Yuva420p12Le` / `Yuva444p14Le` have no `AV_PIX_FMT_*` constant in
    // the linked FFmpeg build (ffmpeg-sys-next 9.0) — they fall through
    // to `None` below.
    PixelFormat::Yuva422p12Le => F::AV_PIX_FMT_YUVA422P12LE,
    PixelFormat::Yuva422p12Be => F::AV_PIX_FMT_YUVA422P12BE,
    PixelFormat::Yuva444p12Le => F::AV_PIX_FMT_YUVA444P12LE,
    PixelFormat::Yuva444p12Be => F::AV_PIX_FMT_YUVA444P12BE,
    PixelFormat::Yuva420p16Le => F::AV_PIX_FMT_YUVA420P16LE,
    PixelFormat::Yuva420p16Be => F::AV_PIX_FMT_YUVA420P16BE,
    PixelFormat::Yuva422p16Le => F::AV_PIX_FMT_YUVA422P16LE,
    PixelFormat::Yuva422p16Be => F::AV_PIX_FMT_YUVA422P16BE,
    PixelFormat::Yuva444p16Le => F::AV_PIX_FMT_YUVA444P16LE,
    PixelFormat::Yuva444p16Be => F::AV_PIX_FMT_YUVA444P16BE,
    // --- Semi-planar YUV 8-bit ---
    PixelFormat::Nv12 => F::AV_PIX_FMT_NV12,
    PixelFormat::Nv21 => F::AV_PIX_FMT_NV21,
    PixelFormat::Nv16 => F::AV_PIX_FMT_NV16,
    PixelFormat::Nv24 => F::AV_PIX_FMT_NV24,
    PixelFormat::Nv42 => F::AV_PIX_FMT_NV42,
    PixelFormat::Nv20Le => F::AV_PIX_FMT_NV20LE,
    PixelFormat::Nv20Be => F::AV_PIX_FMT_NV20BE,
    // --- Semi-planar YUV high-bit ---
    PixelFormat::P010Le => F::AV_PIX_FMT_P010LE,
    PixelFormat::P010Be => F::AV_PIX_FMT_P010BE,
    PixelFormat::P012Le => F::AV_PIX_FMT_P012LE,
    PixelFormat::P012Be => F::AV_PIX_FMT_P012BE,
    PixelFormat::P016Le => F::AV_PIX_FMT_P016LE,
    PixelFormat::P016Be => F::AV_PIX_FMT_P016BE,
    PixelFormat::P210Le => F::AV_PIX_FMT_P210LE,
    PixelFormat::P210Be => F::AV_PIX_FMT_P210BE,
    PixelFormat::P212Le => F::AV_PIX_FMT_P212LE,
    PixelFormat::P212Be => F::AV_PIX_FMT_P212BE,
    PixelFormat::P216Le => F::AV_PIX_FMT_P216LE,
    PixelFormat::P216Be => F::AV_PIX_FMT_P216BE,
    PixelFormat::P410Le => F::AV_PIX_FMT_P410LE,
    PixelFormat::P410Be => F::AV_PIX_FMT_P410BE,
    PixelFormat::P412Le => F::AV_PIX_FMT_P412LE,
    PixelFormat::P412Be => F::AV_PIX_FMT_P412BE,
    PixelFormat::P416Le => F::AV_PIX_FMT_P416LE,
    PixelFormat::P416Be => F::AV_PIX_FMT_P416BE,
    // --- Packed YUV 8-bit ---
    PixelFormat::Yuyv422 => F::AV_PIX_FMT_YUYV422,
    PixelFormat::Uyvy422 => F::AV_PIX_FMT_UYVY422,
    PixelFormat::Yvyu422 => F::AV_PIX_FMT_YVYU422,
    PixelFormat::Uyyvyy411 => F::AV_PIX_FMT_UYYVYY411,
    // --- Packed YUV high-bit ---
    PixelFormat::Y210Le => F::AV_PIX_FMT_Y210LE,
    PixelFormat::Y210Be => F::AV_PIX_FMT_Y210BE,
    PixelFormat::Y212Le => F::AV_PIX_FMT_Y212LE,
    PixelFormat::Y212Be => F::AV_PIX_FMT_Y212BE,
    PixelFormat::Y216Le => F::AV_PIX_FMT_Y216LE,
    PixelFormat::Y216Be => F::AV_PIX_FMT_Y216BE,
    // `V210` / `V410Le` have no `AV_PIX_FMT_*` constant in the linked
    // FFmpeg build (ffmpeg-sys-next 9.0) — they fall through to `None`.
    PixelFormat::Xv30Le => F::AV_PIX_FMT_XV30LE,
    PixelFormat::Xv30Be => F::AV_PIX_FMT_XV30BE,
    PixelFormat::V30xLe => F::AV_PIX_FMT_V30XLE,
    PixelFormat::V30xBe => F::AV_PIX_FMT_V30XBE,
    PixelFormat::Xv36Le => F::AV_PIX_FMT_XV36LE,
    PixelFormat::Xv36Be => F::AV_PIX_FMT_XV36BE,
    PixelFormat::Xv48Le => F::AV_PIX_FMT_XV48LE,
    PixelFormat::Xv48Be => F::AV_PIX_FMT_XV48BE,
    PixelFormat::Vuya => F::AV_PIX_FMT_VUYA,
    PixelFormat::Vuyx => F::AV_PIX_FMT_VUYX,
    PixelFormat::Ayuv => F::AV_PIX_FMT_AYUV,
    PixelFormat::Ayuv64Le => F::AV_PIX_FMT_AYUV64LE,
    PixelFormat::Ayuv64Be => F::AV_PIX_FMT_AYUV64BE,
    PixelFormat::Uyva => F::AV_PIX_FMT_UYVA,
    PixelFormat::Vyu444 => F::AV_PIX_FMT_VYU444,
    // --- XYZ ---
    PixelFormat::Xyz12Le => F::AV_PIX_FMT_XYZ12LE,
    PixelFormat::Xyz12Be => F::AV_PIX_FMT_XYZ12BE,
    // --- Packed RGB 8-bit ---
    PixelFormat::Rgb24 => F::AV_PIX_FMT_RGB24,
    PixelFormat::Bgr24 => F::AV_PIX_FMT_BGR24,
    PixelFormat::Rgba => F::AV_PIX_FMT_RGBA,
    PixelFormat::Bgra => F::AV_PIX_FMT_BGRA,
    PixelFormat::Argb => F::AV_PIX_FMT_ARGB,
    PixelFormat::Abgr => F::AV_PIX_FMT_ABGR,
    PixelFormat::Rgbx => F::AV_PIX_FMT_RGB0,
    PixelFormat::Bgrx => F::AV_PIX_FMT_BGR0,
    PixelFormat::Xrgb => F::AV_PIX_FMT_0RGB,
    PixelFormat::Xbgr => F::AV_PIX_FMT_0BGR,
    PixelFormat::X2Rgb10Le => F::AV_PIX_FMT_X2RGB10LE,
    PixelFormat::X2Rgb10Be => F::AV_PIX_FMT_X2RGB10BE,
    PixelFormat::X2Bgr10Le => F::AV_PIX_FMT_X2BGR10LE,
    PixelFormat::X2Bgr10Be => F::AV_PIX_FMT_X2BGR10BE,
    PixelFormat::Gbr24p => F::AV_PIX_FMT_GBR24P,
    // --- Packed RGB high-bit ---
    PixelFormat::Rgb48Le => F::AV_PIX_FMT_RGB48LE,
    PixelFormat::Rgb48Be => F::AV_PIX_FMT_RGB48BE,
    PixelFormat::Bgr48Le => F::AV_PIX_FMT_BGR48LE,
    PixelFormat::Bgr48Be => F::AV_PIX_FMT_BGR48BE,
    PixelFormat::Rgba64Le => F::AV_PIX_FMT_RGBA64LE,
    PixelFormat::Rgba64Be => F::AV_PIX_FMT_RGBA64BE,
    PixelFormat::Bgra64Le => F::AV_PIX_FMT_BGRA64LE,
    PixelFormat::Bgra64Be => F::AV_PIX_FMT_BGRA64BE,
    PixelFormat::Rgb96Le => F::AV_PIX_FMT_RGB96LE,
    PixelFormat::Rgb96Be => F::AV_PIX_FMT_RGB96BE,
    PixelFormat::Rgba128Le => F::AV_PIX_FMT_RGBA128LE,
    PixelFormat::Rgba128Be => F::AV_PIX_FMT_RGBA128BE,
    // --- Packed RGB float / half-float ---
    PixelFormat::Rgbf16Le => F::AV_PIX_FMT_RGBF16LE,
    PixelFormat::Rgbf16Be => F::AV_PIX_FMT_RGBF16BE,
    PixelFormat::Rgbf32Le => F::AV_PIX_FMT_RGBF32LE,
    PixelFormat::Rgbf32Be => F::AV_PIX_FMT_RGBF32BE,
    PixelFormat::Rgbaf16Le => F::AV_PIX_FMT_RGBAF16LE,
    PixelFormat::Rgbaf16Be => F::AV_PIX_FMT_RGBAF16BE,
    PixelFormat::Rgbaf32Le => F::AV_PIX_FMT_RGBAF32LE,
    PixelFormat::Rgbaf32Be => F::AV_PIX_FMT_RGBAF32BE,
    // --- Planar GBR ---
    PixelFormat::Gbrp => F::AV_PIX_FMT_GBRP,
    PixelFormat::Gbrp9Le => F::AV_PIX_FMT_GBRP9LE,
    PixelFormat::Gbrp9Be => F::AV_PIX_FMT_GBRP9BE,
    PixelFormat::Gbrp10Le => F::AV_PIX_FMT_GBRP10LE,
    PixelFormat::Gbrp10Be => F::AV_PIX_FMT_GBRP10BE,
    PixelFormat::Gbrp10MsbLe => F::AV_PIX_FMT_GBRP10MSBLE,
    PixelFormat::Gbrp10MsbBe => F::AV_PIX_FMT_GBRP10MSBBE,
    PixelFormat::Gbrp12Le => F::AV_PIX_FMT_GBRP12LE,
    PixelFormat::Gbrp12Be => F::AV_PIX_FMT_GBRP12BE,
    PixelFormat::Gbrp12MsbLe => F::AV_PIX_FMT_GBRP12MSBLE,
    PixelFormat::Gbrp12MsbBe => F::AV_PIX_FMT_GBRP12MSBBE,
    PixelFormat::Gbrp14Le => F::AV_PIX_FMT_GBRP14LE,
    PixelFormat::Gbrp14Be => F::AV_PIX_FMT_GBRP14BE,
    PixelFormat::Gbrp16Le => F::AV_PIX_FMT_GBRP16LE,
    PixelFormat::Gbrp16Be => F::AV_PIX_FMT_GBRP16BE,
    PixelFormat::Gbrpf16Le => F::AV_PIX_FMT_GBRPF16LE,
    PixelFormat::Gbrpf16Be => F::AV_PIX_FMT_GBRPF16BE,
    PixelFormat::Gbrpf32Le => F::AV_PIX_FMT_GBRPF32LE,
    PixelFormat::Gbrpf32Be => F::AV_PIX_FMT_GBRPF32BE,
    // --- Planar GBRA ---
    PixelFormat::Gbrap => F::AV_PIX_FMT_GBRAP,
    PixelFormat::Gbrap10Le => F::AV_PIX_FMT_GBRAP10LE,
    PixelFormat::Gbrap10Be => F::AV_PIX_FMT_GBRAP10BE,
    PixelFormat::Gbrap12Le => F::AV_PIX_FMT_GBRAP12LE,
    PixelFormat::Gbrap12Be => F::AV_PIX_FMT_GBRAP12BE,
    PixelFormat::Gbrap14Le => F::AV_PIX_FMT_GBRAP14LE,
    PixelFormat::Gbrap14Be => F::AV_PIX_FMT_GBRAP14BE,
    PixelFormat::Gbrap16Le => F::AV_PIX_FMT_GBRAP16LE,
    PixelFormat::Gbrap16Be => F::AV_PIX_FMT_GBRAP16BE,
    PixelFormat::Gbrap32Le => F::AV_PIX_FMT_GBRAP32LE,
    PixelFormat::Gbrap32Be => F::AV_PIX_FMT_GBRAP32BE,
    PixelFormat::Gbrapf16Le => F::AV_PIX_FMT_GBRAPF16LE,
    PixelFormat::Gbrapf16Be => F::AV_PIX_FMT_GBRAPF16BE,
    PixelFormat::Gbrapf32Le => F::AV_PIX_FMT_GBRAPF32LE,
    PixelFormat::Gbrapf32Be => F::AV_PIX_FMT_GBRAPF32BE,
    // --- Greyscale ---
    PixelFormat::Gray8 => F::AV_PIX_FMT_GRAY8,
    PixelFormat::Gray9Le => F::AV_PIX_FMT_GRAY9LE,
    PixelFormat::Gray9Be => F::AV_PIX_FMT_GRAY9BE,
    PixelFormat::Gray10Le => F::AV_PIX_FMT_GRAY10LE,
    PixelFormat::Gray10Be => F::AV_PIX_FMT_GRAY10BE,
    PixelFormat::Gray12Le => F::AV_PIX_FMT_GRAY12LE,
    PixelFormat::Gray12Be => F::AV_PIX_FMT_GRAY12BE,
    PixelFormat::Gray14Le => F::AV_PIX_FMT_GRAY14LE,
    PixelFormat::Gray14Be => F::AV_PIX_FMT_GRAY14BE,
    PixelFormat::Gray16Le => F::AV_PIX_FMT_GRAY16LE,
    PixelFormat::Gray16Be => F::AV_PIX_FMT_GRAY16BE,
    PixelFormat::Gray32Le => F::AV_PIX_FMT_GRAY32LE,
    PixelFormat::Gray32Be => F::AV_PIX_FMT_GRAY32BE,
    PixelFormat::Grayf16Le => F::AV_PIX_FMT_GRAYF16LE,
    PixelFormat::Grayf16Be => F::AV_PIX_FMT_GRAYF16BE,
    PixelFormat::Grayf32Le => F::AV_PIX_FMT_GRAYF32LE,
    PixelFormat::Grayf32Be => F::AV_PIX_FMT_GRAYF32BE,
    // `Gray8a` and `Y400a` are mediaframe aliases of `Ya8`; FFmpeg's
    // `GRAY8A`/`Y400A` are themselves aliases of `YA8` (same discriminant).
    // We only need a constant to fetch the descriptor, so route them to
    // the canonical `YA8`. (`from_av_pixel_format` maps the raw value to
    // `Ya8`, so these alias variants are never produced from a frame; the
    // mapping exists purely for geometry completeness.)
    PixelFormat::Ya8 | PixelFormat::Gray8a | PixelFormat::Y400a => F::AV_PIX_FMT_YA8,
    PixelFormat::Ya16Le => F::AV_PIX_FMT_YA16LE,
    PixelFormat::Ya16Be => F::AV_PIX_FMT_YA16BE,
    PixelFormat::Yaf16Le => F::AV_PIX_FMT_YAF16LE,
    PixelFormat::Yaf16Be => F::AV_PIX_FMT_YAF16BE,
    PixelFormat::Yaf32Le => F::AV_PIX_FMT_YAF32LE,
    PixelFormat::Yaf32Be => F::AV_PIX_FMT_YAF32BE,
    // --- Formats with no plain-CPU geometry (descriptor still resolved,
    //     but `geometry_descriptor` rejects them by flag). Mapped so the
    //     constant exists for completeness / future use. ---
    PixelFormat::Monowhite => F::AV_PIX_FMT_MONOWHITE,
    PixelFormat::Monoblack => F::AV_PIX_FMT_MONOBLACK,
    PixelFormat::Pal8 => F::AV_PIX_FMT_PAL8,
    PixelFormat::Rgb4 => F::AV_PIX_FMT_RGB4,
    PixelFormat::Rgb4Byte => F::AV_PIX_FMT_RGB4_BYTE,
    PixelFormat::Rgb8 => F::AV_PIX_FMT_RGB8,
    PixelFormat::Bgr4 => F::AV_PIX_FMT_BGR4,
    PixelFormat::Bgr4Byte => F::AV_PIX_FMT_BGR4_BYTE,
    PixelFormat::Bgr8 => F::AV_PIX_FMT_BGR8,
    PixelFormat::Rgb444Le => F::AV_PIX_FMT_RGB444LE,
    PixelFormat::Rgb444Be => F::AV_PIX_FMT_RGB444BE,
    PixelFormat::Bgr444Le => F::AV_PIX_FMT_BGR444LE,
    PixelFormat::Bgr444Be => F::AV_PIX_FMT_BGR444BE,
    PixelFormat::Rgb555Le => F::AV_PIX_FMT_RGB555LE,
    PixelFormat::Rgb555Be => F::AV_PIX_FMT_RGB555BE,
    PixelFormat::Bgr555Le => F::AV_PIX_FMT_BGR555LE,
    PixelFormat::Bgr555Be => F::AV_PIX_FMT_BGR555BE,
    PixelFormat::Rgb565Le => F::AV_PIX_FMT_RGB565LE,
    PixelFormat::Rgb565Be => F::AV_PIX_FMT_RGB565BE,
    PixelFormat::Bgr565Le => F::AV_PIX_FMT_BGR565LE,
    PixelFormat::Bgr565Be => F::AV_PIX_FMT_BGR565BE,
    // --- Bayer: descriptor exists but is rejected by the BAYER flag.
    //     Deferred to colconv demosaic (#112). Mapped for completeness. ---
    PixelFormat::BayerBggr8 => F::AV_PIX_FMT_BAYER_BGGR8,
    PixelFormat::BayerRggb8 => F::AV_PIX_FMT_BAYER_RGGB8,
    PixelFormat::BayerGbrg8 => F::AV_PIX_FMT_BAYER_GBRG8,
    PixelFormat::BayerGrbg8 => F::AV_PIX_FMT_BAYER_GRBG8,
    PixelFormat::BayerBggr16Le => F::AV_PIX_FMT_BAYER_BGGR16LE,
    PixelFormat::BayerBggr16Be => F::AV_PIX_FMT_BAYER_BGGR16BE,
    PixelFormat::BayerRggb16Le => F::AV_PIX_FMT_BAYER_RGGB16LE,
    PixelFormat::BayerRggb16Be => F::AV_PIX_FMT_BAYER_RGGB16BE,
    PixelFormat::BayerGbrg16Le => F::AV_PIX_FMT_BAYER_GBRG16LE,
    PixelFormat::BayerGbrg16Be => F::AV_PIX_FMT_BAYER_GBRG16BE,
    PixelFormat::BayerGrbg16Le => F::AV_PIX_FMT_BAYER_GRBG16LE,
    PixelFormat::BayerGrbg16Be => F::AV_PIX_FMT_BAYER_GRBG16BE,
    // No FFmpeg pixel-format constant for these mediaframe variants in the
    // linked build (10/12/14-bit Bayer have no stable FFmpeg enum), or the
    // variant is `None` / an unnamed `Other`. Return `None`.
    _ => return None,
  })
}

#[cfg(test)]
mod tests;
