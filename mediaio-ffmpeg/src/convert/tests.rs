use super::*;

use ffmpeg_next::ffi::AVColorRange;

/// Writes every byte of every buffer `frame` holds — what a decoder does
/// to the frames it hands out, and what `frame::Video::new` and
/// `frame::Audio::new` leave undone: `av_frame_get_buffer` allocates
/// their planes without writing them, and every conversion reads only
/// [written bytes](crate::contracts#every-exported-byte-initialised).
fn write_every_plane(frame: &mut ffmpeg_next::Frame) {
  // SAFETY: the frame is live and uniquely borrowed. Each non-null
  // `buf[i]` is a live `AVBufferRef` whose `data` is valid for `size`
  // bytes, written through the raw pointer without forming a reference
  // to the unwritten bytes.
  unsafe {
    for buf in (*frame.as_mut_ptr()).buf {
      if !buf.is_null() {
        core::ptr::write_bytes((*buf).data, 0, (*buf).size);
      }
    }
  }
}

/// The `yuvj*` family is JPEG full-range *by definition*. `map_range_for`
/// must report [`ColorRange::Full`] for every one of the five variants —
/// including when the frame's `color_range` field is
/// `AVCOL_RANGE_UNSPECIFIED` (the common MJPEG/JPEG case), which is the
/// exact regression: deriving the range from the field alone would yield
/// `Unspecified`, silently demoting a full-swing frame.
#[test]
fn map_range_for_forces_full_for_every_yuvj_variant() {
  let yuvj = [
    PixelFormat::Yuvj411p,
    PixelFormat::Yuvj420p,
    PixelFormat::Yuvj422p,
    PixelFormat::Yuvj440p,
    PixelFormat::Yuvj444p,
  ];
  let unspecified = AVColorRange::AVCOL_RANGE_UNSPECIFIED as i32;
  let mpeg = AVColorRange::AVCOL_RANGE_MPEG as i32;
  let jpeg = AVColorRange::AVCOL_RANGE_JPEG as i32;
  for pf in &yuvj {
    // Unspecified field — the regression scenario.
    assert_eq!(
      map_range_for(pf, unspecified),
      ColorRange::Full,
      "{pf:?} with UNSPECIFIED color_range must deliver Full"
    );
    // Even a (spurious) explicit MPEG/limited tag is overridden: a yuvj
    // frame is full-range regardless of what the field claims.
    assert_eq!(
      map_range_for(pf, mpeg),
      ColorRange::Full,
      "{pf:?} is full-range by definition even if color_range says MPEG"
    );
    // An explicit JPEG tag agrees.
    assert_eq!(map_range_for(pf, jpeg), ColorRange::Full);
  }
}

/// Non-`yuvj` formats defer entirely to the frame's `color_range` field —
/// the override is scoped to the `yuvj*` family and does **not** generalize
/// to RGB. A `ColorRange` describes the swing of a YUV luma/chroma signal;
/// RGB has no such swing, so an RGB (or plain YUV) frame with an
/// unspecified range stays `Unspecified` rather than being speculatively
/// relabeled `Full`.
#[test]
fn map_range_for_defers_for_non_yuvj() {
  let unspecified = AVColorRange::AVCOL_RANGE_UNSPECIFIED as i32;
  let jpeg = AVColorRange::AVCOL_RANGE_JPEG as i32;
  let mpeg = AVColorRange::AVCOL_RANGE_MPEG as i32;

  // Plain limited-range YUV: unspecified stays unspecified, explicit tags
  // pass through untouched.
  assert_eq!(
    map_range_for(&PixelFormat::Yuv420p, unspecified),
    ColorRange::Unspecified
  );
  assert_eq!(map_range_for(&PixelFormat::Yuv420p, jpeg), ColorRange::Full);
  assert_eq!(
    map_range_for(&PixelFormat::Yuv420p, mpeg),
    ColorRange::Limited
  );

  // RGB is deliberately NOT forced to Full: range is a YUV property, and
  // mediaframe makes no full-range claim about RGB formats.
  assert_eq!(
    map_range_for(&PixelFormat::Rgb24, unspecified),
    ColorRange::Unspecified
  );
  assert_eq!(
    map_range_for(&PixelFormat::Rgba, unspecified),
    ColorRange::Unspecified
  );
}

/// `is_yuvj` recognizes exactly the five JPEG-range planar YUV formats and
/// nothing else — a guard so a newly-added `yuvj*` variant (or an
/// accidental inclusion of a non-`yuvj` format) is caught here.
#[test]
fn is_yuvj_covers_exactly_the_five_variants() {
  for pf in &[
    PixelFormat::Yuvj411p,
    PixelFormat::Yuvj420p,
    PixelFormat::Yuvj422p,
    PixelFormat::Yuvj440p,
    PixelFormat::Yuvj444p,
  ] {
    assert!(is_yuvj(pf), "{pf:?} should be recognized as yuvj");
  }
  // Their non-JPEG siblings and unrelated families are not yuvj.
  for pf in &[
    PixelFormat::Yuv411p,
    PixelFormat::Yuv420p,
    PixelFormat::Yuv422p,
    PixelFormat::Yuv440p,
    PixelFormat::Yuv444p,
    PixelFormat::Nv12,
    PixelFormat::Rgb24,
    PixelFormat::Gray8,
  ] {
    assert!(!is_yuvj(pf), "{pf:?} must not be classified yuvj");
  }
}

/// End-to-end regression: a real `AV_PIX_FMT_YUVJ420P` frame whose
/// `color_range` is left `AVCOL_RANGE_UNSPECIFIED` (exactly what FFmpeg's
/// MJPEG/JPEG decode paths and `av_frame_get_buffer` produce) must be
/// delivered with [`ColorRange::Full`], not `Unspecified`. Before the fix
/// `map_range` derived the range from the field alone and this returned
/// `Unspecified` — a silent full-range mislabel.
#[test]
fn yuvj420p_unspecified_range_delivers_full() {
  // `Video::new` allocates real plane buffers via `av_frame_get_buffer`
  // and leaves `color_range` at its zero default (== UNSPECIFIED). The
  // planes it leaves unwritten; a decoder would write them, and so does
  // this law.
  let mut frame = ffmpeg_next::frame::Video::new(ffmpeg_next::format::Pixel::YUVJ420P, 64, 48);
  write_every_plane(&mut frame);

  // Pin the regression precondition explicitly: color_range is UNSPECIFIED.
  // SAFETY: `frame` is a live, uniquely-owned AVFrame; we only write the
  // `color_range` scalar field through the raw pointer.
  unsafe {
    (*frame.as_mut_ptr()).color_range = AVColorRange::AVCOL_RANGE_UNSPECIFIED;
  }

  // SAFETY: the frame is live for the call, its geometry is
  // `av_frame_get_buffer`'s own, and every plane was written above.
  let out = unsafe { video_frame_from(&frame, Timebase::default(), FrameLimits::default()) }
    .expect("YUVJ420P frame should convert to a VideoFrame");

  assert_eq!(*out.pixel_format(), PixelFormat::Yuvj420p);
  assert_eq!(
    out.color().range(),
    ColorRange::Full,
    "YUVJ420P with UNSPECIFIED color_range must be delivered as Full"
  );
}

/// Builds an unallocated `AVFrame` carrying `raw` in its `format` field.
///
/// The point is to reach the convert path with an integer no CPU layout
/// can be derived from — including integers outside our bindgen's
/// `AVPixelFormat` discriminant set, which is precisely the case the
/// restored diagnostic exists for. `AVFrame.format` is a plain C `int`,
/// so writing one is not a detour around the enum; there is no enum
/// here to begin with, which is why a raw id was always available at
/// this boundary and only the error had stopped carrying it.
fn frame_with_raw_format(raw: i32) -> ffmpeg_next::frame::Video {
  let mut frame = ffmpeg_next::frame::Video::empty();
  // SAFETY: `frame` is a live, uniquely-owned AVFrame; these are three
  // scalar field writes through the raw pointer.
  unsafe {
    let frame_ptr = frame.as_mut_ptr();
    (*frame_ptr).format = raw;
    (*frame_ptr).width = 64;
    (*frame_ptr).height = 48;
  }
  frame
}

/// A hardware surface has no CPU pixel data, so the unified vocabulary's
/// answer for it is `PixelFormat::None` — and before this the whole error
/// message was that `None`, naming nothing. The raw id and FFmpeg's own
/// name for it now ride along.
#[test]
fn an_unsupported_format_is_named_in_the_error() {
  use ffmpeg_next::ffi::AVPixelFormat;

  let raw = AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX as i32;
  let frame = frame_with_raw_format(raw);

  // `VideoFrame` has no `Debug`, so `expect_err` is unavailable.
  //
  // SAFETY: the frame is live for the call and holds no planes — its
  // `data[]` and `buf[]` are null — so it exports no byte to be
  // unwritten; the format is refused before anything else is read.
  let Err(err) = (unsafe { video_frame_from(&frame, Timebase::default(), FrameLimits::default()) })
  else {
    panic!("a hardware surface carries no deliverable CPU layout");
  };

  let ConvertError::UnsupportedPixelFormat(p) = &err else {
    panic!("expected UnsupportedPixelFormat, got {err:?}");
  };
  // The *value* is unchanged — this restores the diagnostic, not the
  // struck `Unknown(u32)` variant.
  // The payload is a tag now: the raw id and libavutil's borrowed
  // name are the identity, and no `PixelFormat` is carried.
  assert!(p.name().is_none() || p.raw() != 0);
  assert_eq!(p.raw(), raw);
  assert_eq!(p.name(), Some("videotoolbox_vld"));

  let rendered = err.to_string();
  assert!(
    rendered.contains(&raw.to_string()),
    "the raw id is missing from {rendered:?}"
  );
  assert!(
    rendered.contains("videotoolbox_vld"),
    "the FFmpeg name is missing from {rendered:?}"
  );
}

/// An integer libavutil cannot describe — a corrupt read, or a format
/// from a newer library than the one linked. The name is absent and the
/// message says so, but the raw id is still there, which is the whole
/// point of carrying it separately.
#[test]
fn an_unnameable_format_still_reports_its_raw_id() {
  let raw = 99_999;
  let frame = frame_with_raw_format(raw);

  // SAFETY: as in the law above — no planes, so no exported byte; the
  // format is refused first.
  let Err(err) = (unsafe { video_frame_from(&frame, Timebase::default(), FrameLimits::default()) })
  else {
    panic!("an unmappable format integer has no deliverable layout");
  };

  let ConvertError::UnsupportedPixelFormat(p) = &err else {
    panic!("expected UnsupportedPixelFormat, got {err:?}");
  };
  // The payload is a tag now: the raw id and libavutil's borrowed
  // name are the identity, and no `PixelFormat` is carried.
  assert!(p.name().is_none() || p.raw() != 0);
  assert_eq!(p.raw(), raw);
  assert_eq!(p.name(), None);

  let rendered = err.to_string();
  assert!(
    rendered.contains("99999"),
    "the raw id is missing from {rendered:?}"
  );
  assert!(
    rendered.contains("unnamed by libavutil"),
    "the absent-name case is not spelled out in {rendered:?}"
  );
}

// ---------------------------------------------------------------------------
//  Static HDR metadata: `AV_FRAME_DATA_MASTERING_DISPLAY_METADATA` /
//  `AV_FRAME_DATA_CONTENT_LIGHT_LEVEL`, parsed off the raw side-data bytes.
// ---------------------------------------------------------------------------

/// Packs an `AVMasteringDisplayMetadata`-shaped payload: ten
/// `AVRational`s (native-endian `i32` num, `i32` den) — R's x then y,
/// G's x then y, B's x then y, white point's x then y, min luminance,
/// max luminance — then two `int` presence flags, matching
/// `libavutil/mastering_display_metadata.h` field by field. Each `x`/
/// `y` is its own independent rational; a primary's chromaticity is
/// two of them, not one.
#[allow(clippy::too_many_arguments)]
fn mastering_display_bytes(
  r_x: (i32, i32),
  r_y: (i32, i32),
  g_x: (i32, i32),
  g_y: (i32, i32),
  b_x: (i32, i32),
  b_y: (i32, i32),
  wp_x: (i32, i32),
  wp_y: (i32, i32),
  min_luminance: (i32, i32),
  max_luminance: (i32, i32),
  has_primaries: i32,
  has_luminance: i32,
) -> Vec<u8> {
  let mut out = Vec::with_capacity(MASTERING_DISPLAY_METADATA_BYTES);
  for (num, den) in [
    r_x,
    r_y,
    g_x,
    g_y,
    b_x,
    b_y,
    wp_x,
    wp_y,
    min_luminance,
    max_luminance,
  ] {
    out.extend_from_slice(&num.to_ne_bytes());
    out.extend_from_slice(&den.to_ne_bytes());
  }
  out.extend_from_slice(&has_primaries.to_ne_bytes());
  out.extend_from_slice(&has_luminance.to_ne_bytes());
  out
}

/// A zeroed `(0, 1)` rational — the filler this file's negative/short-
/// payload tests use for every field that is not the one under test.
const ZERO_RATIONAL: (i32, i32) = (0, 1);

/// The exact numbers a real HDR10 mastering-display SEI decodes to on
/// this host — cross-checked with `ffprobe -show_frames` against an
/// `libx265`-encoded clip (`master-display=G(13250,34500)B(7500,3000)
/// R(34000,16000)WP(15635,16450)L(10000000,1)`):
///
/// ```text
/// red_x=34000/50000    red_y=16000/50000
/// green_x=13250/50000  green_y=34500/50000
/// blue_x=7500/50000    blue_y=3000/50000
/// white_point_x=15635/50000  white_point_y=16450/50000
/// min_luminance=1/10000      max_luminance=10000000/10000
/// ```
///
/// So this is not an invented fixture: it is what FFmpeg's own SEI
/// parser actually produces, transcribed.
#[test]
fn parse_mastering_display_reads_a_real_hdr10_payload() {
  let bytes = mastering_display_bytes(
    (34_000, 50_000),     // red_x
    (16_000, 50_000),     // red_y
    (13_250, 50_000),     // green_x
    (34_500, 50_000),     // green_y
    (7_500, 50_000),      // blue_x
    (3_000, 50_000),      // blue_y
    (15_635, 50_000),     // white_point_x
    (16_450, 50_000),     // white_point_y
    (1, 10_000),          // min_luminance
    (10_000_000, 10_000), // max_luminance
    1,
    1,
  );
  // The x265 recipe above interleaves G/B/R; the struct's own order is
  // R, G, B — `ffprobe`'s `red_x`/`green_x`/`blue_x` naming is the
  // struct order, and what `mastering_display_bytes` is called with.
  let md = parse_mastering_display(&bytes).expect("a full 88-byte payload parses");
  assert_eq!(
    md.display_primaries(),
    [(34_000, 16_000), (13_250, 34_500), (7_500, 3_000)],
    "R, G, B chromaticities in ST 2086 fixed-point units"
  );
  assert_eq!(md.white_point(), (15_635, 16_450));
  assert_eq!(md.min_luminance(), (1, 10_000));
  assert_eq!(md.max_luminance(), (10_000_000, 10_000));
}

/// A denominator other than 50000 is rescaled exactly, not truncated —
/// `17/200` (0.085) becomes `4250/50000`.
#[test]
fn parse_mastering_display_rescales_a_non_standard_chroma_denominator() {
  let bytes = mastering_display_bytes(
    (17, 200), // red_x — the one non-standard field under test
    ZERO_RATIONAL,
    ZERO_RATIONAL,
    ZERO_RATIONAL,
    ZERO_RATIONAL,
    ZERO_RATIONAL,
    ZERO_RATIONAL,
    ZERO_RATIONAL,
    ZERO_RATIONAL,
    ZERO_RATIONAL,
    1,
    1,
  );
  let md = parse_mastering_display(&bytes).expect("parses");
  assert_eq!(md.display_primaries()[0].0, 4_250, "17/200 == 4250/50000");
}

/// Shorter than the fixed 88-byte struct — a version-skew or corrupt
/// entry — answers `None` rather than reading past the end.
#[test]
fn parse_mastering_display_refuses_a_short_payload() {
  let bytes = mastering_display_bytes(
    ZERO_RATIONAL,
    ZERO_RATIONAL,
    ZERO_RATIONAL,
    ZERO_RATIONAL,
    ZERO_RATIONAL,
    ZERO_RATIONAL,
    ZERO_RATIONAL,
    ZERO_RATIONAL,
    ZERO_RATIONAL,
    ZERO_RATIONAL,
    1,
    1,
  );
  assert_eq!(bytes.len(), MASTERING_DISPLAY_METADATA_BYTES);
  assert!(parse_mastering_display(&bytes[..bytes.len() - 1]).is_none());
  assert!(parse_mastering_display(&[]).is_none());
}

/// A negative chromaticity or luminance component is unrepresentable
/// in the physical quantity it names — refused rather than silently
/// cast into a huge `u32`. Every other field is otherwise-valid (`ZERO_
/// RATIONAL` or the real HDR10 numbers), so the refusal is provably
/// about the one negative field and not an incidental short payload.
#[test]
fn parse_mastering_display_refuses_a_negative_component() {
  let negative_primary = mastering_display_bytes(
    (-1, 50_000), // red_x — negative
    ZERO_RATIONAL,
    ZERO_RATIONAL,
    ZERO_RATIONAL,
    ZERO_RATIONAL,
    ZERO_RATIONAL,
    ZERO_RATIONAL,
    ZERO_RATIONAL,
    ZERO_RATIONAL,
    ZERO_RATIONAL,
    1,
    1,
  );
  assert_eq!(negative_primary.len(), MASTERING_DISPLAY_METADATA_BYTES);
  assert!(parse_mastering_display(&negative_primary).is_none());

  let negative_luminance = mastering_display_bytes(
    ZERO_RATIONAL,
    ZERO_RATIONAL,
    ZERO_RATIONAL,
    ZERO_RATIONAL,
    ZERO_RATIONAL,
    ZERO_RATIONAL,
    ZERO_RATIONAL,
    ZERO_RATIONAL,
    (-1, 10_000), // min_luminance — negative
    ZERO_RATIONAL,
    1,
    1,
  );
  assert_eq!(negative_luminance.len(), MASTERING_DISPLAY_METADATA_BYTES);
  assert!(parse_mastering_display(&negative_luminance).is_none());
}

/// `av_mastering_display_metadata_alloc`'s own default-initialized
/// record: ten zeroed `AVRational`s with both `has_primaries` and
/// `has_luminance` at `0`. Every numeric component is otherwise
/// "valid" (zero is a fine `u32`), so only the flags distinguish this
/// from a real all-zero-chromaticity record — and this is the
/// regression a flags-blind parser would silently accept as `Some`.
#[test]
fn parse_mastering_display_refuses_the_flags_unset_default_record() {
  let bytes = mastering_display_bytes(
    ZERO_RATIONAL,
    ZERO_RATIONAL,
    ZERO_RATIONAL,
    ZERO_RATIONAL,
    ZERO_RATIONAL,
    ZERO_RATIONAL,
    ZERO_RATIONAL,
    ZERO_RATIONAL,
    ZERO_RATIONAL,
    ZERO_RATIONAL,
    0, // has_primaries
    0, // has_luminance
  );
  assert_eq!(bytes.len(), MASTERING_DISPLAY_METADATA_BYTES);
  assert!(
    parse_mastering_display(&bytes).is_none(),
    "an unset-flags record must not be reported as real HDR metadata"
  );
}

/// Every other combination of the two flags — real numbers behind
/// both, but only one declared present — is refused too:
/// [`MasteringDisplay`] cannot represent "primaries but no luminance"
/// or vice versa, so a partial record is `None` rather than a value
/// with a fabricated half.
#[test]
fn parse_mastering_display_refuses_every_partial_flag_combination() {
  let real = (
    (34_000, 50_000),
    (16_000, 50_000),
    (13_250, 50_000),
    (34_500, 50_000),
    (7_500, 50_000),
    (3_000, 50_000),
    (15_635, 50_000),
    (16_450, 50_000),
    (1, 10_000),
    (10_000_000, 10_000),
  );
  for (has_primaries, has_luminance) in [(1, 0), (0, 1)] {
    let bytes = mastering_display_bytes(
      real.0,
      real.1,
      real.2,
      real.3,
      real.4,
      real.5,
      real.6,
      real.7,
      real.8,
      real.9,
      has_primaries,
      has_luminance,
    );
    assert!(
      parse_mastering_display(&bytes).is_none(),
      "has_primaries={has_primaries} has_luminance={has_luminance} must refuse, \
       real numbers behind an unset flag notwithstanding"
    );
  }
  // The control: identical numbers with both flags set parse fine, so
  // the refusals above are about the flags and not the fixture itself.
  let bytes = mastering_display_bytes(
    real.0, real.1, real.2, real.3, real.4, real.5, real.6, real.7, real.8, real.9, 1, 1,
  );
  assert!(parse_mastering_display(&bytes).is_some());
}

/// A zero (or negative) luminance denominator is a degenerate rational
/// no real encoder emits — refused rather than stored as a ratio that
/// means nothing to a caller who evaluates it.
#[test]
fn parse_mastering_display_refuses_a_non_positive_luminance_denominator() {
  let zero_min_den = mastering_display_bytes(
    ZERO_RATIONAL,
    ZERO_RATIONAL,
    ZERO_RATIONAL,
    ZERO_RATIONAL,
    ZERO_RATIONAL,
    ZERO_RATIONAL,
    ZERO_RATIONAL,
    ZERO_RATIONAL,
    (1, 0), // min_luminance — zero denominator
    ZERO_RATIONAL,
    1,
    1,
  );
  assert!(parse_mastering_display(&zero_min_den).is_none());

  let negative_max_den = mastering_display_bytes(
    ZERO_RATIONAL,
    ZERO_RATIONAL,
    ZERO_RATIONAL,
    ZERO_RATIONAL,
    ZERO_RATIONAL,
    ZERO_RATIONAL,
    ZERO_RATIONAL,
    ZERO_RATIONAL,
    ZERO_RATIONAL,
    (1, -1), // max_luminance — negative denominator
    1,
    1,
  );
  assert!(parse_mastering_display(&negative_max_den).is_none());
}

/// `AVContentLightMetadata` is two `unsigned` fields — `MaxCLL` then
/// `MaxFALL` — read verbatim. `1000, 400` is what the same real
/// `libx265 -x265-params max-cll=1000,400` clip decodes to.
#[test]
fn parse_content_light_level_reads_max_cll_and_max_fall() {
  let mut bytes = Vec::with_capacity(CONTENT_LIGHT_METADATA_BYTES);
  bytes.extend_from_slice(&1000u32.to_ne_bytes());
  bytes.extend_from_slice(&400u32.to_ne_bytes());
  let cll = parse_content_light_level(&bytes).expect("an 8-byte payload parses");
  assert_eq!(cll.max_cll(), 1000);
  assert_eq!(cll.max_fall(), 400);
}

/// Shorter than the fixed 8-byte struct answers `None`.
#[test]
fn parse_content_light_level_refuses_a_short_payload() {
  assert!(parse_content_light_level(&[0u8; 7]).is_none());
  assert!(parse_content_light_level(&[]).is_none());
}

/// [`find_mastering_display`] / [`find_content_light_level`] answer
/// `None` on an empty side-data list — the "absent metadata answers
/// absent" contract, at the field a caller actually reads.
#[test]
fn absent_side_data_answers_none_for_both_hdr_fields() {
  let side_data: Vec<SideDataEntry> = Vec::new();
  assert!(find_mastering_display(&side_data).is_none());
  assert!(find_content_light_level(&side_data).is_none());
}

/// The two entries are found by kind among unrelated side data, not by
/// position — an SEI/timecode entry ahead of them does not hide them.
#[test]
fn hdr_fields_are_found_by_kind_among_unrelated_side_data() {
  let cll_bytes = {
    let mut b = Vec::with_capacity(CONTENT_LIGHT_METADATA_BYTES);
    b.extend_from_slice(&500u32.to_ne_bytes());
    b.extend_from_slice(&100u32.to_ne_bytes());
    b
  };
  let side_data = vec![
    SideDataEntry::new(
      0xDEAD_BEEFu32 as i32,
      FfmpegBytes::copy_from_slice(&[1, 2, 3]),
    ),
    SideDataEntry::new(
      AVFrameSideDataType::AV_FRAME_DATA_CONTENT_LIGHT_LEVEL as i32,
      FfmpegBytes::copy_from_slice(&cll_bytes),
    ),
  ];
  assert!(find_mastering_display(&side_data).is_none());
  let cll = find_content_light_level(&side_data).expect("the CLL entry is present");
  assert_eq!(cll.max_cll(), 500);
  assert_eq!(cll.max_fall(), 100);
}

/// **A custom channel map reaching the borrowed conversion is refused
/// unread.** Safe `ffmpeg_next` code can give a frame a `CUSTOM` layout
/// whose map points at nothing, and the borrowed conversion's contract
/// does not ask its caller to vouch for the map. The refusal is by name,
/// and the worker — the only code that walks a map — is never entered:
/// the probe's tripwire would stop it at the door before it touched the
/// frame.
#[test]
fn a_dangling_custom_map_is_refused_by_the_borrowed_conversion_unread() {
  use ffmpeg_next::ffi::{AVChannelCustom, AVChannelOrder};

  let mut frame = ffmpeg_next::frame::Audio::empty();
  // SAFETY: reading the pointer of a live frame.
  let raw = unsafe { frame.as_mut_ptr() };
  // SAFETY: `raw` is the live frame. Its empty layout owns nothing, so
  // overwriting the fields leaks nothing; the map is a dangling,
  // well-aligned address that nothing here reads.
  unsafe {
    (*raw).ch_layout.order = AVChannelOrder::AV_CHANNEL_ORDER_CUSTOM;
    (*raw).ch_layout.nb_channels = 1;
    (*raw).ch_layout.u.map = core::ptr::NonNull::<AVChannelCustom>::dangling().as_ptr();
  }

  let before = worker_probe::entries();
  worker_probe::arm(true);
  // SAFETY: the frame is live for the call and holds no planes, so it
  // exports no byte to be unwritten; its map is not the contract's to
  // vouch for on this road, which refuses it unread — the law.
  let result = unsafe { audio_frame_from(&frame, Timebase::default(), FrameLimits::default()) };
  worker_probe::arm(false);
  let entered = worker_probe::entries() - before;

  // SAFETY: as above. The frame goes back to an empty layout before it
  // drops, or `av_frame_free` would free the dangling map.
  unsafe {
    (*raw).ch_layout.order = AVChannelOrder::AV_CHANNEL_ORDER_UNSPEC;
    (*raw).ch_layout.nb_channels = 0;
    (*raw).ch_layout.u.mask = 0;
  }

  assert_eq!(entered, 0, "the borrowed road must not enter the worker");
  assert!(
    matches!(result, Err(ConvertError::UnverifiableChannelMap(p)) if p.channels() == 1),
    "a custom map is refused by name, got {:?}",
    result.as_ref().err(),
  );
}

/// **A null-backed frame is refused before any field is read.**
/// ffmpeg-next's safe `Audio::empty()` stores whatever `av_frame_alloc`
/// answered — null, when the allocation fails — so the borrowed
/// conversion can be handed a wrapper around nothing. `Audio::wrap(null)`
/// builds that same wrapper without an allocation failure.
#[test]
fn a_null_backed_audio_frame_is_refused_before_any_read() {
  // SAFETY: the state `Audio::empty()` leaves after a failed
  // `av_frame_alloc`; dropping it calls `av_frame_free` on null, which
  // does nothing.
  let frame = unsafe { ffmpeg_next::frame::Audio::wrap(core::ptr::null_mut()) };
  assert!(matches!(
    // SAFETY: a wrapper around null is the contract's one admitted
    // exception: it is refused before anything is read.
    unsafe { audio_frame_from(&frame, Timebase::default(), FrameLimits::default()) },
    Err(ConvertError::NullFrame)
  ));
}

/// **An impossible count is refused as one, whatever the layout's
/// order.** The borrowed road judges the sample and channel counts exactly
/// as the worker does before it asks the map question, so a custom
/// layout declaring `-1`, `256`, or no channels under samples — or a
/// negative sample count — is refused for the count, and the worker is
/// never entered. Only a count the worker would take reaches
/// [`ConvertError::UnverifiableChannelMap`].
#[test]
fn a_custom_layout_with_an_impossible_count_is_refused_for_the_count() {
  use ffmpeg_next::ffi::{AVChannelCustom, AVChannelOrder};

  let mut frame = ffmpeg_next::frame::Audio::empty();
  // SAFETY: reading the pointer of a live frame.
  let raw = unsafe { frame.as_mut_ptr() };
  let refuse = |channels: i32, samples: i32| {
    // SAFETY: `raw` is the live frame. Its empty layout owns nothing,
    // so overwriting the fields leaks nothing; the map is a dangling,
    // well-aligned address that nothing here reads.
    unsafe {
      (*raw).nb_samples = samples;
      (*raw).ch_layout.order = AVChannelOrder::AV_CHANNEL_ORDER_CUSTOM;
      (*raw).ch_layout.nb_channels = channels;
      (*raw).ch_layout.u.map = core::ptr::NonNull::<AVChannelCustom>::dangling().as_ptr();
    }
    let before = worker_probe::entries();
    worker_probe::arm(true);
    // SAFETY: the frame is live for the call and holds no planes; the
    // counts are refused before anything else is read — the law.
    let result = unsafe { audio_frame_from(&frame, Timebase::default(), FrameLimits::default()) };
    worker_probe::arm(false);
    assert_eq!(
      worker_probe::entries() - before,
      0,
      "the borrowed road must not enter the worker"
    );
    // SAFETY: as above. The frame goes back to an empty layout before it
    // drops, or `av_frame_free` would free the dangling map.
    unsafe {
      (*raw).nb_samples = 0;
      (*raw).ch_layout.order = AVChannelOrder::AV_CHANNEL_ORDER_UNSPEC;
      (*raw).ch_layout.nb_channels = 0;
      (*raw).ch_layout.u.mask = 0;
    }
    result.err()
  };

  for (channels, samples) in [(-1, 0), (256, 0), (i32::MAX, 0), (0, 64)] {
    assert!(
      matches!(
        refuse(channels, samples),
        Some(ConvertError::UnsupportedChannelCount(p)) if p.channels() == channels
      ),
      "{channels} channels under {samples} samples is refused for the count",
    );
  }
  assert!(matches!(
    refuse(2, -1),
    Some(ConvertError::InvalidSampleCount(p)) if p.count() == -1
  ));
}

/// **A custom map libavcodec allocated still converts — on the road
/// whose caller vouches for it.** `av_channel_layout_custom_init` sizes
/// the map for `nb_channels` entries, which is the shape every
/// decoder's frame carries. The borrowed road refuses this frame too: it
/// cannot tell an honest map from a forged one.
#[test]
fn a_libavcodec_owned_custom_layout_converts_through_the_unsafe_entry() {
  use ffmpeg_next::{
    ChannelLayout,
    ffi::{AVChannel, av_channel_layout_custom_init, av_channel_layout_uninit},
    format::{Sample, sample::Type},
  };

  let mut frame =
    ffmpeg_next::frame::Audio::new(Sample::F32(Type::Planar), 64, ChannelLayout::STEREO);
  // `Audio::new` leaves the samples unwritten; a decoder writes them, and
  // so does this law, before anything reads them.
  write_every_plane(&mut frame);
  // SAFETY: reading the pointer of a live frame.
  let raw = unsafe { frame.as_mut_ptr() };
  // SAFETY: `raw` is the live frame. Its stereo layout is released by
  // FFmpeg's own uninit, and the custom one is allocated by FFmpeg for
  // exactly two entries, both written in bounds.
  unsafe {
    av_channel_layout_uninit(&raw mut (*raw).ch_layout);
    assert_eq!(
      av_channel_layout_custom_init(&raw mut (*raw).ch_layout, 2),
      0
    );
    (*(*raw).ch_layout.u.map).id = AVChannel::AV_CHAN_FRONT_LEFT;
    (*(*raw).ch_layout.u.map.add(1)).id = AVChannel::AV_CHAN_FRONT_RIGHT;
  }

  assert!(matches!(
    // SAFETY: the frame is live for the call and every sample was written
    // above; its map is refused unread on this road.
    unsafe { audio_frame_from(&frame, Timebase::default(), FrameLimits::default()) },
    Err(ConvertError::UnverifiableChannelMap(p)) if p.channels() == 2
  ));

  // SAFETY: the frame is live for the call, every sample was written
  // above, its custom map holds the two entries FFmpeg allocated for it,
  // and the owned lane copies.
  let converted = unsafe {
    av_frame_to_audio_frame_as::<crate::Owned>(
      frame.as_ptr(),
      Timebase::default(),
      FrameLimits::default(),
    )
  }
  .expect("a custom map FFmpeg sized converts");
  assert_eq!(converted.channel_count(), 2);
  assert_eq!(converted.channel_layout().channels(), 2);
}

/// **A bitmap rect is refused by the borrowed conversion, unread.** A
/// decoded bitmap rect owns a plane of `linesize[0] × h` bytes, and
/// `ffmpeg_next`'s safe `BitmapMut::set_height` moves `h` alone — here
/// to a million rows over a four-byte plane. The borrowed road refuses
/// the rect by name, and the worker, the only code that reads a plane,
/// is never entered: the probe's tripwire would stop it at the door.
#[test]
fn a_bitmap_rect_is_refused_by_the_borrowed_subtitle_conversion_unread() {
  use ffmpeg_next::subtitle::{RectMut, Type};

  let mut subtitle = ffmpeg_next::Subtitle::new();
  // A text rect first: the refusal names the bitmap's own index.
  if let RectMut::Text(mut text) = subtitle.add_rect(Type::Text) {
    text.set("before the bitmap");
  }
  let RectMut::Bitmap(mut bitmap) = subtitle.add_rect(Type::Bitmap) else {
    panic!("a bitmap rect was asked for");
  };
  // SAFETY: `bitmap` is the rect `add_rect` just allocated. The plane is
  // a four-byte `av_malloc` allocation, freed with the subtitle by
  // `avsubtitle_free`; two rows of two bytes is its honest shape.
  unsafe {
    let rect = bitmap.as_mut_ptr();
    let plane = ffmpeg_next::ffi::av_malloc(4).cast::<u8>();
    assert!(!plane.is_null());
    core::ptr::write_bytes(plane, 0, 4);
    (*rect).data[0] = plane;
    (*rect).linesize[0] = 2;
    (*rect).w = 2;
    (*rect).h = 2;
  }
  // The safe setter that makes the plane's extent a lie.
  bitmap.set_height(1_000_000);

  let before = worker_probe::entries();
  worker_probe::arm(true);
  // SAFETY: the subtitle is live for the call; its text rect's string is
  // `ffmpeg_next`'s NUL-terminated copy, and the bitmap's plane is not
  // this road's to read — it is refused unread, the law.
  let result = unsafe { subtitle_frame_from(&subtitle) };
  worker_probe::arm(false);

  assert_eq!(
    worker_probe::entries() - before,
    0,
    "the borrowed road must not enter the worker"
  );
  assert!(
    matches!(result, Err(ConvertError::UnverifiableBitmap(p)) if p.rect() == 1),
    "a bitmap rect is refused by name, got {:?}",
    result.as_ref().err(),
  );
}

/// **Text still converts on the borrowed road.** The refusal is the
/// bitmap's alone: a subtitle of text rects, whose strings `ffmpeg_next`
/// duplicates NUL-terminated, comes through the borrowed conversion.
#[test]
fn a_text_subtitle_converts_on_the_borrowed_road() {
  use ffmpeg_next::subtitle::{RectMut, Type};

  let mut subtitle = ffmpeg_next::Subtitle::new();
  if let RectMut::Text(mut text) = subtitle.add_rect(Type::Text) {
    text.set("hello");
  }
  // SAFETY: the subtitle is live for the call, and its one rect's string
  // is `ffmpeg_next`'s NUL-terminated copy.
  let frame = unsafe { subtitle_frame_from(&subtitle) }.expect("a text subtitle converts");
  match frame.payload() {
    mediaspine::subtitle::SubtitlePayload::Text(text) => {
      assert_eq!(text.text().as_ref(), b"hello");
    }
    _ => panic!("a text payload was expected"),
  }
}

/// The text a converted subtitle carries, on either lane.
fn subtitle_text<B: AsRef<[u8]>>(frame: &SubtitleFrame<SubtitleFrameExtra, B>) -> Vec<u8> {
  match frame.payload() {
    mediaspine::subtitle::SubtitlePayload::Text(text) => {
      let bytes: &[u8] = text.text().as_ref();
      bytes.to_vec()
    }
    _ => panic!("a text payload was expected"),
  }
}

/// **A rect's fields are read only under its own kind.** A text rect and
/// an ASS rect whose bitmap fields hold garbage — plane and palette
/// pointers that point at nothing, a stride and height whose product the
/// bitmap arm would refuse, a negative width, positions out of range —
/// convert to their text, on the borrowed road and on both raw lanes.
///
/// The garbage is stored, never read. It is ordinary written data, so
/// the law itself stays defined whatever the conversion does; a
/// conversion that dereferenced the planes would fault, and one that
/// judged their geometry before dispatching on `type` would refuse. A
/// decoder that allocates a rect without zeroing it leaves exactly these
/// fields unwritten on a text rect, which is why the worker reads `type`
/// first and every other field only under the arm that owns it.
#[test]
fn a_text_rect_converts_whatever_its_bitmap_fields_hold() {
  use ffmpeg_next::subtitle::{RectMut, Type};

  let mut subtitle = ffmpeg_next::Subtitle::new();
  let mut rects = Vec::new();
  if let RectMut::Text(mut text) = subtitle.add_rect(Type::Text) {
    text.set("the text");
    // SAFETY: reading the pointer of the rect `add_rect` just allocated.
    rects.push(unsafe { text.as_mut_ptr() });
  }
  if let RectMut::Ass(mut ass) = subtitle.add_rect(Type::Ass) {
    ass.set("the ass");
    // SAFETY: as above.
    rects.push(unsafe { ass.as_mut_ptr() });
  }
  assert_eq!(rects.len(), 2, "a text rect and an ASS rect were asked for");
  let nowhere = core::ptr::NonNull::<u8>::dangling().as_ptr();
  for &rect in &rects {
    // SAFETY: each rect is live, owned by `subtitle`, and nothing else
    // holds it; these are plain stores of initialised values, and the
    // two pointers go back to null below.
    unsafe {
      (*rect).data[0] = nowhere;
      (*rect).data[1] = nowhere;
      (*rect).linesize[0] = i32::MAX;
      (*rect).w = -7;
      (*rect).h = i32::MAX;
      (*rect).x = i32::MIN;
      (*rect).y = -1;
    }
  }

  // SAFETY (all three): the subtitle is live for each call; its two
  // strings are `ffmpeg_next`'s NUL-terminated copies, and the garbage
  // sits in fields of a kind neither rect is — fields the contract does
  // not ask for, and the conversion does not read.
  let borrowed = unsafe { subtitle_frame_from(&subtitle) };
  let viewed = unsafe { av_subtitle_to_subtitle_frame(subtitle.as_ptr()) };
  let owned = unsafe { av_subtitle_to_owned_subtitle_frame(subtitle.as_ptr()) };

  for &rect in &rects {
    // SAFETY: as above; the planes go back to the null `add_rect` left,
    // so nothing that frees the subtitle's rects is handed the garbage.
    unsafe {
      (*rect).data[0] = core::ptr::null_mut();
      (*rect).data[1] = core::ptr::null_mut();
    }
  }

  let expected: &[u8] = b"the text\nthe ass";
  assert_eq!(
    subtitle_text(&borrowed.expect("the borrowed road converts text rects")),
    expected
  );
  assert_eq!(
    subtitle_text(&viewed.expect("the view lane converts text rects")),
    expected
  );
  assert_eq!(
    subtitle_text(&owned.expect("the owned lane converts text rects")),
    expected
  );
}
