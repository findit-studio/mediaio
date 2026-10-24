//! HDR dynamic metadata in packet side data — what a muxer owes
//! libavformat before it hands over an entry whose bytes are not bytes but
//! a struct of libavutil's.
//!
//! Two kinds of packet side data carry a libavutil struct rather than a
//! byte payload: HDR10+ (`AV_PKT_DATA_DYNAMIC_HDR10_PLUS`, an
//! `AVDynamicHDRPlus`, SMPTE ST 2094-40 application 4) and SMPTE ST 2094
//! application 5 (`AV_PKT_DATA_DYNAMIC_HDR_SMPTE_2094_APP5`, an
//! `AVDynamicHDRSmpte2094App5`). Matroska's writer — the one writer in
//! libavformat that reads either (`mkv_write_block`, `matroskaenc.c`
//! 2947–2990 in FFmpeg 9.0.1) — casts the entry's bytes to the struct
//! without asking their size, and hands it to libavutil's serializer
//! (`av_dynamic_hdr_plus_to_t35`, `hdr_dynamic_metadata.c` 237–395;
//! `av_dynamic_hdr_smpte2094_app5_to_t35`, 563–706), which trusts every
//! count and divides by every denominator. An entry shorter than the
//! struct is read past its end; a count past its array, past the array; a
//! zero denominator divides by zero; and a count past what the serializer
//! sizes for fails `av_assert0`, which aborts the process.
//! [`check_hdr10_plus`] and [`check_smpte2094_app5`] hold an entry to all
//! of it, and to what reading it back takes, before any muxer is handed
//! it.
//!
//! # The layouts
//!
//! ffmpeg-sys-next 9.0.1 binds no part of
//! `libavutil/hdr_dynamic_metadata.h`, so the two structs are mirrored
//! here, `repr(C)`, field for field from the header. The header warns that
//! a struct's size is not part of libavutil's ABI: a later minor release
//! may add fields at its end. So the size a check requires is the linked
//! library's own, asked once of its allocator (`av_dynamic_hdr_plus_alloc`
//! reports it, `hdr_dynamic_metadata.c` 36–44), and an entry is judged only
//! where that size is the mirror's: under a libavutil whose struct differs,
//! every such entry is refused by name ([`HdrMetadataError::Layout`]). The
//! laws read libavutil's own parse of a payload through the mirrors'
//! offsets, field by field.
//!
//! Nothing here forms a mirror from bytes: an entry is read through the
//! mirrors' offsets (`offset_of!`) as the integers its fields are, so no
//! `unsafe` reads it. The `unsafe` is the allocator's size and the parse
//! of a payload into libavutil's own struct.
//!
//! # What is checked
//!
//! An entry is admitted only where its bytes are the struct's whole size
//! and, of every field the ITU-T T.35 payload carries:
//!
//! - every count names no element past its array, nor one past what the
//!   reader takes (`av_dynamic_hdr_plus_from_t35`, 59–235;
//!   `av_dynamic_hdr_smpte2094_app5_from_t35`, 429–561) — or the reader
//!   refuses the block, and Matroska's reader fails the packet with it
//!   (`matroskadec.c` 4057–4101);
//! - every denominator is nonzero, and the serializer's arithmetic in C's
//!   `int` overflows nowhere;
//! - every value fits the bits it is written in, and every rational is a
//!   whole number of its field's unit, so that it reads back as that
//!   number over the unit — the same value;
//! - every flag is 0 or 1;
//! - what the writer writes the reader reads in the same place: HDR10+'s
//!   writer writes 1 for the application version whatever the struct
//!   holds, and writes a window's saturation flag inside its tone
//!   mapping, where the reader reads it after; APP5's writer writes each
//!   image's curves by its own count, where the reader copies the first
//!   image's;
//! - the payload is of a size its writer takes.
//!
//! A field the payload does not carry — the T.35 country code, the first
//! window's geometry, an element past its count, a field its flag leaves
//! out — is not judged: it is not written, and reads back zero.

use core::{
  ffi::c_void,
  mem::{offset_of, size_of},
};
use std::sync::OnceLock;

use crate::ffi::c_shims;

/// `AV_HDR_PLUS_MAX_PAYLOAD_SIZE`: the most bytes HDR10+'s serializer
/// writes, which it asserts (`av_assert0`, `hdr_dynamic_metadata.c` 298).
const HDR_PLUS_MAX_PAYLOAD: usize = 907;

/// The bytes Matroska's writer gives APP5's serializer
/// (`SMPTE_2094_APP5_MAX_SIZE`, `matroskaenc.c` 2872), which refuses a
/// payload past it (`hdr_dynamic_metadata.c` 625–626).
const MATROSKA_APP5_MAX_PAYLOAD: usize = 855;

/// `AVRational`.
#[allow(dead_code)]
#[repr(C)]
struct Rational {
  num: i32,
  den: i32,
}

/// `AVHDRPlusPercentile`.
#[allow(dead_code)]
#[repr(C)]
struct HdrPlusPercentile {
  percentage: u8,
  percentile: Rational,
}

/// `AVHDRPlusColorTransformParams`.
#[allow(dead_code)]
#[repr(C)]
struct HdrPlusColorTransformParams {
  window_upper_left_corner_x: Rational,
  window_upper_left_corner_y: Rational,
  window_lower_right_corner_x: Rational,
  window_lower_right_corner_y: Rational,
  center_of_ellipse_x: u16,
  center_of_ellipse_y: u16,
  rotation_angle: u8,
  semimajor_axis_internal_ellipse: u16,
  semimajor_axis_external_ellipse: u16,
  semiminor_axis_external_ellipse: u16,
  /// `enum AVHDRPlusOverlapProcessOption`, an `int`.
  overlap_process_option: i32,
  maxscl: [Rational; 3],
  average_maxrgb: Rational,
  num_distribution_maxrgb_percentiles: u8,
  distribution_maxrgb: [HdrPlusPercentile; 15],
  fraction_bright_pixels: Rational,
  tone_mapping_flag: u8,
  knee_point_x: Rational,
  knee_point_y: Rational,
  num_bezier_curve_anchors: u8,
  bezier_curve_anchors: [Rational; 15],
  color_saturation_mapping_flag: u8,
  color_saturation_weight: Rational,
}

/// `AVDynamicHDRPlus`.
#[allow(dead_code)]
#[repr(C)]
struct DynamicHdrPlus {
  itu_t_t35_country_code: u8,
  application_version: u8,
  num_windows: u8,
  params: [HdrPlusColorTransformParams; 3],
  targeted_system_display_maximum_luminance: Rational,
  targeted_system_display_actual_peak_luminance_flag: u8,
  num_rows_targeted_system_display_actual_peak_luminance: u8,
  num_cols_targeted_system_display_actual_peak_luminance: u8,
  targeted_system_display_actual_peak_luminance: [[Rational; 25]; 25],
  mastering_display_actual_peak_luminance_flag: u8,
  num_rows_mastering_display_actual_peak_luminance: u8,
  num_cols_mastering_display_actual_peak_luminance: u8,
  mastering_display_actual_peak_luminance: [[Rational; 25]; 25],
}

/// `AVDynamicHDRSmpte2094App5`.
#[allow(dead_code)]
#[repr(C)]
struct DynamicHdrSmpte2094App5 {
  application_version: u8,
  minimum_application_version: u8,
  has_custom_hdr_reference_white_flag: u8,
  has_adaptive_tone_map_flag: u8,
  hdr_reference_white: u16,
  baseline_hdr_headroom: u16,
  use_reference_white_tone_mapping_flag: u8,
  num_alternate_images: u8,
  gain_application_space_chromaticities_flag: u8,
  has_common_component_mix_params_flag: u8,
  has_common_curve_params_flag: u8,
  gain_application_space_chromaticities: [u16; 8],
  alternate_hdr_headrooms: [u16; 4],
  component_mixing_type: [u8; 4],
  has_component_mixing_coefficient_flag: [[u8; 6]; 4],
  component_mixing_coefficient: [[u16; 6]; 4],
  gain_curve_num_control_points_minus_1: [u8; 4],
  gain_curve_use_pchip_slope_flag: [u8; 4],
  gain_curve_control_points_x: [[u16; 32]; 4],
  gain_curve_control_points_y: [[u16; 32]; 4],
  gain_curve_control_points_theta: [[u16; 32]; 4],
}

/// Why an HDR metadata entry is not one the writers take whole.
///
/// Marked `#[non_exhaustive]`: a reason a later release tells apart joins
/// as a variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum HdrMetadataError {
  /// libavutil failed the allocation it reports its struct's size by.
  #[error("libavutil could not allocate its struct, which is how it says the struct's size")]
  Allocation,
  /// The linked libavutil's struct is not the one this build mirrors, so
  /// no entry can be judged against it.
  #[error("the linked libavutil's struct is {linked} bytes, not the {mirrored} this build mirrors")]
  Layout {
    /// The linked library's size.
    linked: usize,
    /// The mirror's.
    mirrored: usize,
  },
  /// The entry is not the struct's size: a shorter one is read past its
  /// end.
  #[error("the entry is {found} bytes, not the struct's {expected}")]
  Size {
    /// The struct's size.
    expected: usize,
    /// The entry's.
    found: usize,
  },
  /// A count names elements past its array, or past what the reader
  /// takes.
  #[error("a count names elements past its array, or past what the reader takes")]
  Count,
  /// A denominator is zero, or a division the serializer makes in C's
  /// `int` overflows.
  #[error("a denominator is zero, or a division overflows C's int")]
  Divisor,
  /// A product the serializer makes in C's `int` overflows.
  #[error("a product the serializer makes in C's int overflows")]
  Overflow,
  /// A value does not fit the bits it is written in, or is not a whole
  /// number of its field's unit; or a flag is neither 0 nor 1.
  #[error("a value does not fit its bits or its unit, or a flag is neither 0 nor 1")]
  Value,
  /// An application version the writer refuses, or writes over.
  #[error("an application version the writer refuses, or writes over")]
  Version,
  /// Fields the writer writes where the reader reads others.
  #[error("the writer writes fields where the reader reads others")]
  Syntax,
  /// A payload of a size its writer refuses.
  #[error("the payload is of a size its writer refuses")]
  Unwritable,
  /// libavutil refused the T.35 payload, with this code.
  #[error("libavutil refused the payload ({0})")]
  Parse(i32),
}

/// An entry read through a mirror's offsets: its fields as the integers
/// they are, in the platform's byte order, as libavutil wrote them.
///
/// Every offset read is inside the mirror, and the entry is the mirror's
/// size (checked first), so no read is out of bounds.
struct Fields<'a>(&'a [u8]);

impl Fields<'_> {
  fn u8(&self, at: usize) -> u8 {
    self.0[at]
  }

  fn u16(&self, at: usize) -> u16 {
    u16::from_ne_bytes([self.0[at], self.0[at + 1]])
  }

  fn i32(&self, at: usize) -> i32 {
    i32::from_ne_bytes([self.0[at], self.0[at + 1], self.0[at + 2], self.0[at + 3]])
  }

  /// An `AVRational`: its numerator and denominator.
  fn rational(&self, at: usize) -> (i32, i32) {
    (
      self.i32(at + offset_of!(Rational, num)),
      self.i32(at + offset_of!(Rational, den)),
    )
  }
}

/// How the serializer scales a rational into the integer it writes:
/// `num * unit / den`, the product in C's `int` (a `static const int32_t`
/// unit) or in `int64_t` (an `int64_t` one) — `hdr_dynamic_metadata.c`
/// 28–34.
#[derive(Clone, Copy)]
enum Unit {
  Int(i32),
  Wide(i64),
}

/// Whether `(num, den)` scaled by `unit` is written as itself in `bits`:
/// a nonzero denominator, no overflow where C makes one, a whole number of
/// the unit, from 0 below `2^bits`.
fn scaled((num, den): (i32, i32), unit: Unit, bits: u32) -> Result<(), HdrMetadataError> {
  if den == 0 {
    return Err(HdrMetadataError::Divisor);
  }
  let quotient = match unit {
    Unit::Int(unit) => {
      let product = num.checked_mul(unit).ok_or(HdrMetadataError::Overflow)?;
      let quotient = product.checked_div(den).ok_or(HdrMetadataError::Divisor)?;
      if product.checked_rem(den) != Some(0) {
        return Err(HdrMetadataError::Value);
      }
      i64::from(quotient)
    }
    // `num` promoted to `int64_t`: no product of an `int` and the units
    // here overflows it, nor its division by an `int`.
    Unit::Wide(unit) => {
      let product = i64::from(num) * unit;
      let den = i64::from(den);
      if product % den != 0 {
        return Err(HdrMetadataError::Value);
      }
      product / den
    }
  };
  if (0..1i64 << bits).contains(&quotient) {
    Ok(())
  } else {
    Err(HdrMetadataError::Value)
  }
}

/// A flag the writer writes in one bit.
fn flag(value: u8) -> Result<bool, HdrMetadataError> {
  match value {
    0 => Ok(false),
    1 => Ok(true),
    _ => Err(HdrMetadataError::Value),
  }
}

/// The linked libavutil's size for a struct, asked of `alloc` once (and
/// again after a failed allocation, which is not remembered).
fn linked_size(
  cell: &OnceLock<usize>,
  alloc: unsafe extern "C" fn(*mut usize) -> *mut c_void,
) -> Result<usize, HdrMetadataError> {
  if let Some(&size) = cell.get() {
    return Ok(size);
  }
  let mut size = 0usize;
  // SAFETY: `alloc` is one of libavutil's two allocators redeclared in
  // `c_shims`: it answers a zeroed struct of its own size, which it writes
  // to the valid out-pointer, or null; the struct is freed at once.
  let ptr = unsafe { alloc(&mut size) };
  if ptr.is_null() {
    return Err(HdrMetadataError::Allocation);
  }
  // SAFETY: `ptr` is libavutil's own allocation, freed exactly once.
  unsafe { ffmpeg_next::ffi::av_free(ptr) };
  Ok(*cell.get_or_init(|| size))
}

/// The linked libavutil's `sizeof(AVDynamicHDRPlus)`.
fn linked_hdr_plus_size() -> Result<usize, HdrMetadataError> {
  static SIZE: OnceLock<usize> = OnceLock::new();
  linked_size(&SIZE, c_shims::av_dynamic_hdr_plus_alloc)
}

/// The linked libavutil's `sizeof(AVDynamicHDRSmpte2094App5)`.
fn linked_app5_size() -> Result<usize, HdrMetadataError> {
  static SIZE: OnceLock<usize> = OnceLock::new();
  linked_size(&SIZE, c_shims::av_dynamic_hdr_smpte2094_app5_alloc)
}

/// Whether `bytes` is a whole struct of `mirrored` bytes, as the linked
/// libavutil sizes it.
fn whole(bytes: &[u8], linked: usize, mirrored: usize) -> Result<(), HdrMetadataError> {
  if linked != mirrored {
    return Err(HdrMetadataError::Layout { linked, mirrored });
  }
  if bytes.len() != mirrored {
    return Err(HdrMetadataError::Size {
      expected: mirrored,
      found: bytes.len(),
    });
  }
  Ok(())
}

/// The offset of `field` of HDR10+'s window `w`.
macro_rules! window {
  ($w:expr, $field:ident) => {
    offset_of!(DynamicHdrPlus, params)
      + $w * size_of::<HdrPlusColorTransformParams>()
      + offset_of!(HdrPlusColorTransformParams, $field)
  };
}

/// An HDR10+ peak luminance table — its rows, its columns, and the entries
/// they name — as the serializer writes it and the reader reads it: from 2
/// to 25 rows and columns (`hdr_dynamic_metadata.c` 128, 181), each
/// entry a whole number of fifteenths in 4 bits.
fn peak_table(
  fields: &Fields<'_>,
  rows: usize,
  cols: usize,
  table: usize,
) -> Result<(), HdrMetadataError> {
  let (rows, cols) = (usize::from(fields.u8(rows)), usize::from(fields.u8(cols)));
  if !(2..=25).contains(&rows) || !(2..=25).contains(&cols) {
    return Err(HdrMetadataError::Count);
  }
  for row in 0..rows {
    for col in 0..cols {
      let entry = table + (row * 25 + col) * size_of::<Rational>();
      scaled(fields.rational(entry), Unit::Int(15), 4)?;
    }
  }
  Ok(())
}

/// Whether `bytes` is an `AVDynamicHDRPlus` the HDR10+ serializer writes
/// whole and its reader reads back as itself — see [the module](self) for
/// what that takes, `av_dynamic_hdr_plus_to_t35` (`hdr_dynamic_metadata.c`
/// 237–395) and `av_dynamic_hdr_plus_from_t35` (59–235) for each rule.
pub fn check_hdr10_plus(bytes: &[u8]) -> Result<(), HdrMetadataError> {
  whole(bytes, linked_hdr_plus_size()?, size_of::<DynamicHdrPlus>())?;
  let fields = Fields(bytes);
  // The serializer writes application version 1 whatever the struct
  // holds (315–316); the reader keeps what it reads (83).
  if fields.u8(offset_of!(DynamicHdrPlus, application_version)) != 1 {
    return Err(HdrMetadataError::Version);
  }
  // Three windows in the array; the reader takes from one to three
  // (84–88).
  let windows = usize::from(fields.u8(offset_of!(DynamicHdrPlus, num_windows)));
  if !(1..=3).contains(&windows) {
    return Err(HdrMetadataError::Count);
  }
  // The geometry of each window past the first (321–333): four corners in
  // 16 bits each as `num / den`, read back over 1; the ellipse's fields
  // fit their bits by their types; the overlap option in 1 bit.
  for w in 1..windows {
    for corner in [
      window!(w, window_upper_left_corner_x),
      window!(w, window_upper_left_corner_y),
      window!(w, window_lower_right_corner_x),
      window!(w, window_lower_right_corner_y),
    ] {
      scaled(fields.rational(corner), Unit::Int(1), 16)?;
    }
    if !(0..=1).contains(&fields.i32(window!(w, overlap_process_option))) {
      return Err(HdrMetadataError::Value);
    }
  }
  // The targeted display's luminance in 27 bits, its unit 1 in `int64_t`
  // (335–336), and its peak table where flagged (337–346).
  scaled(
    fields.rational(offset_of!(
      DynamicHdrPlus,
      targeted_system_display_maximum_luminance
    )),
    Unit::Wide(1),
    27,
  )?;
  if flag(fields.u8(offset_of!(
    DynamicHdrPlus,
    targeted_system_display_actual_peak_luminance_flag
  )))? {
    peak_table(
      &fields,
      offset_of!(
        DynamicHdrPlus,
        num_rows_targeted_system_display_actual_peak_luminance
      ),
      offset_of!(
        DynamicHdrPlus,
        num_cols_targeted_system_display_actual_peak_luminance
      ),
      offset_of!(
        DynamicHdrPlus,
        targeted_system_display_actual_peak_luminance
      ),
    )?;
  }
  // Each window's color statistics (348–360): maxscl and the average in
  // 17 bits over 100000 in `int64_t`; up to 15 percentiles, each a 7-bit
  // percentage and a 17-bit value; the bright pixels in 10 bits over 1000,
  // in `int`.
  for w in 0..windows {
    for i in 0..3 {
      scaled(
        fields.rational(window!(w, maxscl) + i * size_of::<Rational>()),
        Unit::Wide(100_000),
        17,
      )?;
    }
    scaled(
      fields.rational(window!(w, average_maxrgb)),
      Unit::Wide(100_000),
      17,
    )?;
    let percentiles = usize::from(fields.u8(window!(w, num_distribution_maxrgb_percentiles)));
    if percentiles > 15 {
      return Err(HdrMetadataError::Count);
    }
    for i in 0..percentiles {
      let entry = window!(w, distribution_maxrgb) + i * size_of::<HdrPlusPercentile>();
      if fields.u8(entry + offset_of!(HdrPlusPercentile, percentage)) >= 1 << 7 {
        return Err(HdrMetadataError::Value);
      }
      scaled(
        fields.rational(entry + offset_of!(HdrPlusPercentile, percentile)),
        Unit::Wide(100_000),
        17,
      )?;
    }
    scaled(
      fields.rational(window!(w, fraction_bright_pixels)),
      Unit::Int(1000),
      10,
    )?;
  }
  // The mastering display's peak table where flagged (362–371).
  if flag(fields.u8(offset_of!(
    DynamicHdrPlus,
    mastering_display_actual_peak_luminance_flag
  )))? {
    peak_table(
      &fields,
      offset_of!(
        DynamicHdrPlus,
        num_rows_mastering_display_actual_peak_luminance
      ),
      offset_of!(
        DynamicHdrPlus,
        num_cols_mastering_display_actual_peak_luminance
      ),
      offset_of!(DynamicHdrPlus, mastering_display_actual_peak_luminance),
    )?;
  }
  // Each window's tone mapping (373–388): the knee point in 12 bits over
  // 4095, up to 15 anchors in 10 bits over 1023, and the saturation weight
  // in 6 bits over 8, each in `int`. The serializer writes the saturation
  // flag inside the tone mapping; the reader reads it after (224–231). So
  // a window without tone mapping reads back as itself only where it is
  // the last — the reader then reads its saturation flag from the zero
  // bits the serializer pads with — and has no saturation mapping.
  for w in 0..windows {
    let tone = flag(fields.u8(window!(w, tone_mapping_flag)))?;
    let saturation = flag(fields.u8(window!(w, color_saturation_mapping_flag)))?;
    if tone {
      for knee in [window!(w, knee_point_x), window!(w, knee_point_y)] {
        scaled(fields.rational(knee), Unit::Int(4095), 12)?;
      }
      let anchors = usize::from(fields.u8(window!(w, num_bezier_curve_anchors)));
      if anchors > 15 {
        return Err(HdrMetadataError::Count);
      }
      for i in 0..anchors {
        scaled(
          fields.rational(window!(w, bezier_curve_anchors) + i * size_of::<Rational>()),
          Unit::Int(1023),
          10,
        )?;
      }
      if saturation {
        scaled(
          fields.rational(window!(w, color_saturation_weight)),
          Unit::Int(8),
          6,
        )?;
      }
    } else if w + 1 < windows || saturation {
      return Err(HdrMetadataError::Syntax);
    }
  }
  if hdr10_plus_payload_bytes(&fields, windows) > HDR_PLUS_MAX_PAYLOAD {
    return Err(HdrMetadataError::Unwritable);
  }
  Ok(())
}

/// The bytes HDR10+'s serializer sizes its payload at, by its own sum
/// (`hdr_dynamic_metadata.c` 262–296) — within the counts
/// [`check_hdr10_plus`] admits, never past what it asserts.
fn hdr10_plus_payload_bytes(fields: &Fields<'_>, windows: usize) -> usize {
  let table = |flag: usize, rows: usize, cols: usize| {
    if fields.u8(flag) == 0 {
      0
    } else {
      10 + usize::from(fields.u8(rows)) * usize::from(fields.u8(cols)) * 4
    }
  };
  let mut bits = 8 + 2 + 153 * (windows - 1) + 27 + 1;
  bits += table(
    offset_of!(
      DynamicHdrPlus,
      targeted_system_display_actual_peak_luminance_flag
    ),
    offset_of!(
      DynamicHdrPlus,
      num_rows_targeted_system_display_actual_peak_luminance
    ),
    offset_of!(
      DynamicHdrPlus,
      num_cols_targeted_system_display_actual_peak_luminance
    ),
  );
  for w in 0..windows {
    bits += 72 + usize::from(fields.u8(window!(w, num_distribution_maxrgb_percentiles))) * 24 + 10;
  }
  bits += 1
    + table(
      offset_of!(DynamicHdrPlus, mastering_display_actual_peak_luminance_flag),
      offset_of!(
        DynamicHdrPlus,
        num_rows_mastering_display_actual_peak_luminance
      ),
      offset_of!(
        DynamicHdrPlus,
        num_cols_mastering_display_actual_peak_luminance
      ),
    );
  for w in 0..windows {
    bits += 1;
    if fields.u8(window!(w, tone_mapping_flag)) != 0 {
      bits += 28 + usize::from(fields.u8(window!(w, num_bezier_curve_anchors))) * 10;
    }
    bits += 1;
    if fields.u8(window!(w, color_saturation_mapping_flag)) != 0 {
      bits += 6;
    }
  }
  bits.div_ceil(8)
}

/// The offset of APP5's `field`, at `[a]` or `[a][c]` where given.
macro_rules! app5 {
  ($field:ident) => {
    offset_of!(DynamicHdrSmpte2094App5, $field)
  };
  ($field:ident [$a:expr], $element:ty) => {
    offset_of!(DynamicHdrSmpte2094App5, $field) + $a * size_of::<$element>()
  };
  ($field:ident [$a:expr][$c:expr], $row:expr, $element:ty) => {
    offset_of!(DynamicHdrSmpte2094App5, $field) + ($a * $row + $c) * size_of::<$element>()
  };
}

/// Whether `bytes` is an `AVDynamicHDRSmpte2094App5` APP5's serializer
/// writes whole into Matroska's buffer and its reader reads back as itself
/// — see [the module](self) for what that takes,
/// `av_dynamic_hdr_smpte2094_app5_to_t35` (`hdr_dynamic_metadata.c`
/// 563–706) and `av_dynamic_hdr_smpte2094_app5_from_t35` (429–561) for
/// each rule.
pub fn check_smpte2094_app5(bytes: &[u8]) -> Result<(), HdrMetadataError> {
  whole(
    bytes,
    linked_app5_size()?,
    size_of::<DynamicHdrSmpte2094App5>(),
  )?;
  let fields = Fields(bytes);
  // The serializer refuses these versions (575–576); each is 3 bits.
  if fields.u8(app5!(application_version)) >= 8
    || fields.u8(app5!(minimum_application_version)) >= 3
  {
    return Err(HdrMetadataError::Version);
  }
  flag(fields.u8(app5!(has_custom_hdr_reference_white_flag)))?;
  if !flag(fields.u8(app5!(has_adaptive_tone_map_flag)))? {
    return app5_sized(&fields);
  }
  if flag(fields.u8(app5!(use_reference_white_tone_mapping_flag)))? {
    return app5_sized(&fields);
  }
  // Up to four images, as both the serializer (590–591) and the reader
  // (481–484) take; the chromaticities flag in 2 bits.
  let images = usize::from(fields.u8(app5!(num_alternate_images)));
  if images > 4 {
    return Err(HdrMetadataError::Count);
  }
  if fields.u8(app5!(gain_application_space_chromaticities_flag)) > 3 {
    return Err(HdrMetadataError::Value);
  }
  let common_mix = flag(fields.u8(app5!(has_common_component_mix_params_flag)))?;
  let common_curve = flag(fields.u8(app5!(has_common_curve_params_flag)))?;
  let mixing_type = |a: usize| fields.u8(app5!(component_mixing_type[a], u8));
  let coefficient_flag =
    |a: usize, k: usize| fields.u8(app5!(has_component_mixing_coefficient_flag[a][k], 6, u8));
  let coefficient =
    |a: usize, k: usize| fields.u16(app5!(component_mixing_coefficient[a][k], 6, u16));
  let points = |a: usize| fields.u8(app5!(gain_curve_num_control_points_minus_1[a], u8));
  let pchip = |a: usize| fields.u8(app5!(gain_curve_use_pchip_slope_flag[a], u8));
  let x = |a: usize, c: usize| fields.u16(app5!(gain_curve_control_points_x[a][c], 32, u16));
  for a in 0..images {
    // Every image's curves are written by its own count, read past
    // `[a][31]` were it more (683–695): the bound holds for each image,
    // whether its curves are its own or the first one's.
    let count = points(a);
    if count > 31 {
      return Err(HdrMetadataError::Count);
    }
    if a == 0 || !common_mix {
      // The mixing type in 2 bits; its six coefficient flags where it is 3
      // (667–678).
      let kind = mixing_type(a);
      if kind > 3 {
        return Err(HdrMetadataError::Value);
      }
      if kind == 3 {
        for k in 0..6 {
          flag(coefficient_flag(a, k))?;
        }
      }
    } else if mixing_type(a) != mixing_type(0)
      || (mixing_type(0) == 3
        && (0..6).any(|k| {
          coefficient_flag(a, k) != coefficient_flag(0, k) || coefficient(a, k) != coefficient(0, k)
        }))
    {
      // The reader copies the first image's mixing for every later one
      // (515–523), which the serializer does not write.
      return Err(HdrMetadataError::Syntax);
    }
    if a == 0 || !common_curve {
      flag(pchip(a))?;
    } else if count != points(0)
      || pchip(a) != pchip(0)
      || (0..=usize::from(count)).any(|c| x(a, c) != x(0, c))
    {
      // The reader copies the first image's count, slope flag and x
      // points (541–549); the serializer writes this image's y and theta
      // points by its own count and flag (690–695).
      return Err(HdrMetadataError::Syntax);
    }
  }
  app5_sized(&fields)
}

/// Whether APP5's serializer, by its own sum (`hdr_dynamic_metadata.c`
/// 577–618), sizes a payload of whole bytes that fits Matroska's buffer.
/// Within the counts [`check_smpte2094_app5`] admits, every term is whole
/// bytes and the most is exactly Matroska's 855.
fn app5_sized(fields: &Fields<'_>) -> Result<(), HdrMetadataError> {
  let set = |at: usize| fields.u8(at) != 0;
  let mut bits = 3 + 3 + 2 + 1 + 1 + 6;
  if set(app5!(has_custom_hdr_reference_white_flag)) {
    bits += 16;
  }
  if set(app5!(has_adaptive_tone_map_flag)) {
    bits += 16 + 1;
    if set(app5!(use_reference_white_tone_mapping_flag)) {
      bits += 7;
    } else {
      bits += 3 + 2 + 1 + 1;
      if fields.u8(app5!(gain_application_space_chromaticities_flag)) == 3 {
        bits += 16 * 8;
      }
      let common_mix = set(app5!(has_common_component_mix_params_flag));
      let common_curve = set(app5!(has_common_curve_params_flag));
      for a in 0..usize::from(fields.u8(app5!(num_alternate_images))) {
        bits += 16;
        if a == 0 || !common_mix {
          bits += 2 + 6;
          if fields.u8(app5!(component_mixing_type[a], u8)) == 3 {
            bits += 16
              * (0..6)
                .filter(|&k| set(app5!(has_component_mixing_coefficient_flag[a][k], 6, u8)))
                .count();
          }
        }
        let points =
          usize::from(fields.u8(app5!(gain_curve_num_control_points_minus_1[a], u8))) + 1;
        if a == 0 || !common_curve {
          bits += 5 + 1 + 2 + 16 * points;
        }
        bits += 16 * points;
        if !set(app5!(gain_curve_use_pchip_slope_flag[a], u8)) {
          bits += 16 * points;
        }
      }
    }
  }
  if bits % 8 != 0 || bits / 8 > MATROSKA_APP5_MAX_PAYLOAD {
    return Err(HdrMetadataError::Unwritable);
  }
  Ok(())
}

/// The `AVDynamicHDRPlus` libavutil parses from the ITU-T T.35 HDR10+
/// payload `payload` — the bytes after the T.35 header, as Matroska's
/// reader hands them (`matroskadec.c` 4057–4077) — as the bytes of the
/// struct, the linked library's own size; or the code it refused it with.
///
/// The side data a demuxer delivers for HDR10+ is exactly this, and so is
/// what [`check_hdr10_plus`] reads back through the mirrors.
pub fn hdr10_plus_from_t35(payload: &[u8]) -> Result<Vec<u8>, HdrMetadataError> {
  from_t35(
    payload,
    c_shims::av_dynamic_hdr_plus_alloc,
    c_shims::av_dynamic_hdr_plus_from_t35,
  )
}

/// As [`hdr10_plus_from_t35`], for an SMPTE ST 2094 application 5 payload
/// (`matroskadec.c` 4078–4101).
pub fn smpte2094_app5_from_t35(payload: &[u8]) -> Result<Vec<u8>, HdrMetadataError> {
  from_t35(
    payload,
    c_shims::av_dynamic_hdr_smpte2094_app5_alloc,
    c_shims::av_dynamic_hdr_smpte2094_app5_from_t35,
  )
}

/// A struct `alloc` makes, parsed from `payload` by `parse`, as its bytes.
fn from_t35(
  payload: &[u8],
  alloc: unsafe extern "C" fn(*mut usize) -> *mut c_void,
  parse: unsafe extern "C" fn(*mut c_void, *const u8, usize) -> i32,
) -> Result<Vec<u8>, HdrMetadataError> {
  let mut size = 0usize;
  // SAFETY: `alloc` is one of libavutil's two allocators: a zeroed struct
  // of `size` bytes, or null.
  let ptr = unsafe { alloc(&mut size) };
  if ptr.is_null() {
    return Err(HdrMetadataError::Allocation);
  }
  // SAFETY: `ptr` is the live struct just allocated; `payload` is live for
  // the call and its length passed with it; libavutil copies it into a
  // padded buffer of its own and writes only the struct's fields.
  let status = unsafe { parse(ptr, payload.as_ptr(), payload.len()) };
  let bytes = (status >= 0).then(|| {
    // SAFETY: `ptr` holds `size` bytes, every one initialized — zeroed by
    // the allocator, then fields written — and is not freed until below.
    unsafe { core::slice::from_raw_parts(ptr.cast::<u8>(), size) }.to_vec()
  });
  // SAFETY: libavutil's own allocation, freed exactly once.
  unsafe { ffmpeg_next::ffi::av_free(ptr) };
  bytes.ok_or(HdrMetadataError::Parse(status))
}

#[cfg(test)]
mod tests;
