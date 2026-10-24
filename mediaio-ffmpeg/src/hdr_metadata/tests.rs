use super::*;

/// A bit writer, most significant bit first, as ITU-T T.35 payloads are
/// written.
#[derive(Default)]
struct Bits(Vec<bool>);

impl Bits {
  fn put(&mut self, value: u64, width: u32) -> &mut Self {
    for bit in (0..width).rev() {
      self.0.push(value >> bit & 1 == 1);
    }
    self
  }

  fn bytes(&self) -> Vec<u8> {
    self
      .0
      .chunks(8)
      .map(|byte| {
        byte
          .iter()
          .chain(core::iter::repeat(&false))
          .take(8)
          .fold(0u8, |acc, &bit| acc << 1 | u8::from(bit))
      })
      .collect()
  }
}

/// An HDR10+ T.35 payload of one window, every field the syntax carries
/// set to a value of its own: application version 1; the targeted
/// display's 400; maxscl 1000, 2000, 3000 and an average of 500 (over
/// 100000); two percentiles, (1, 100) and (50, 5000); bright pixels 10 (over
/// 1000); a knee point at 100 and 200 (over 4095); anchors 10, 20, 30 (over
/// 1023); a saturation weight of 5 (over 8).
fn hdr10_plus_payload() -> Vec<u8> {
  let mut bits = Bits::default();
  bits.put(1, 8).put(1, 2).put(400, 27).put(0, 1);
  bits
    .put(1000, 17)
    .put(2000, 17)
    .put(3000, 17)
    .put(500, 17)
    .put(2, 4);
  bits
    .put(1, 7)
    .put(100, 17)
    .put(50, 7)
    .put(5000, 17)
    .put(10, 10);
  bits.put(0, 1);
  bits.put(1, 1).put(100, 12).put(200, 12).put(3, 4);
  bits.put(10, 10).put(20, 10).put(30, 10).put(1, 1).put(5, 6);
  bits.bytes()
}

/// An APP5 T.35 payload: versions 0, a custom reference white of 203, a
/// baseline of 1000, one alternate image at 2000 with its own mixing (type
/// 0) and curve — two points, (100, 300) and (200, 400), slopes by PCHIP.
fn app5_payload() -> Vec<u8> {
  let mut bits = Bits::default();
  bits.put(0, 3).put(0, 3).put(0, 2);
  bits.put(1, 1).put(1, 1).put(0, 6);
  bits.put(203, 16).put(1000, 16).put(0, 1);
  bits.put(1, 3).put(0, 2).put(0, 1).put(0, 1);
  bits.put(2000, 16).put(0, 2).put(0, 6);
  bits.put(1, 5).put(1, 1).put(0, 2).put(100, 16).put(200, 16);
  bits.put(300, 16).put(400, 16);
  bits.bytes()
}

fn set_u8(bytes: &mut [u8], at: usize, value: u8) {
  bytes[at] = value;
}

fn set_i32(bytes: &mut [u8], at: usize, value: i32) {
  bytes[at..at + 4].copy_from_slice(&value.to_ne_bytes());
}

fn set_rational(bytes: &mut [u8], at: usize, (num, den): (i32, i32)) {
  set_i32(bytes, at + offset_of!(Rational, num), num);
  set_i32(bytes, at + offset_of!(Rational, den), den);
}

fn set_u16(bytes: &mut [u8], at: usize, value: u16) {
  bytes[at..at + 2].copy_from_slice(&value.to_ne_bytes());
}

/// **The mirrors are the linked libavutil's layouts**: their sizes are the
/// ones its allocators report, and its own parse of a payload reads back
/// through their offsets, field by field.
#[test]
fn the_mirrors_are_libavutils_layouts() {
  assert_eq!(
    linked_hdr_plus_size(),
    Ok(size_of::<DynamicHdrPlus>()),
    "AVDynamicHDRPlus"
  );
  assert_eq!(
    linked_app5_size(),
    Ok(size_of::<DynamicHdrSmpte2094App5>()),
    "AVDynamicHDRSmpte2094App5"
  );

  let plus = hdr10_plus_from_t35(&hdr10_plus_payload()).expect("libavutil parses it");
  let fields = Fields(&plus);
  assert_eq!(
    fields.u8(offset_of!(DynamicHdrPlus, application_version)),
    1
  );
  assert_eq!(fields.u8(offset_of!(DynamicHdrPlus, num_windows)), 1);
  assert_eq!(
    fields.rational(offset_of!(
      DynamicHdrPlus,
      targeted_system_display_maximum_luminance
    )),
    (400, 1)
  );
  for (i, value) in [1000, 2000, 3000].into_iter().enumerate() {
    assert_eq!(
      fields.rational(window!(0, maxscl) + i * size_of::<Rational>()),
      (value, 100_000)
    );
  }
  assert_eq!(fields.rational(window!(0, average_maxrgb)), (500, 100_000));
  assert_eq!(
    fields.u8(window!(0, num_distribution_maxrgb_percentiles)),
    2
  );
  let second = window!(0, distribution_maxrgb) + size_of::<HdrPlusPercentile>();
  assert_eq!(
    fields.u8(second + offset_of!(HdrPlusPercentile, percentage)),
    50
  );
  assert_eq!(
    fields.rational(second + offset_of!(HdrPlusPercentile, percentile)),
    (5000, 100_000)
  );
  assert_eq!(
    fields.rational(window!(0, fraction_bright_pixels)),
    (10, 1000)
  );
  assert_eq!(fields.u8(window!(0, tone_mapping_flag)), 1);
  assert_eq!(fields.rational(window!(0, knee_point_y)), (200, 4095));
  assert_eq!(fields.u8(window!(0, num_bezier_curve_anchors)), 3);
  assert_eq!(
    fields.rational(window!(0, bezier_curve_anchors) + 2 * size_of::<Rational>()),
    (30, 1023)
  );
  assert_eq!(fields.u8(window!(0, color_saturation_mapping_flag)), 1);
  assert_eq!(fields.rational(window!(0, color_saturation_weight)), (5, 8));

  let app5 = smpte2094_app5_from_t35(&app5_payload()).expect("libavutil parses it");
  let fields = Fields(&app5);
  assert_eq!(fields.u8(app5!(has_custom_hdr_reference_white_flag)), 1);
  assert_eq!(fields.u16(app5!(hdr_reference_white)), 203);
  assert_eq!(fields.u16(app5!(baseline_hdr_headroom)), 1000);
  assert_eq!(fields.u8(app5!(num_alternate_images)), 1);
  assert_eq!(fields.u16(app5!(alternate_hdr_headrooms[0], u16)), 2000);
  assert_eq!(
    fields.u8(app5!(gain_curve_num_control_points_minus_1[0], u8)),
    1
  );
  assert_eq!(fields.u8(app5!(gain_curve_use_pchip_slope_flag[0], u8)), 1);
  assert_eq!(
    fields.u16(app5!(gain_curve_control_points_x[0][1], 32, u16)),
    200
  );
  assert_eq!(
    fields.u16(app5!(gain_curve_control_points_y[0][1], 32, u16)),
    400
  );
}

/// **An HDR10+ struct libavutil parsed is admitted, and one the writer
/// would read out of bounds, divide by zero, overflow, or write otherwise
/// than it reads back is refused by what it breaks.**
///
/// PLANT: dropping the size check admits the cut entry, and the window
/// bound the fourth window.
#[test]
fn an_hdr10_plus_entry_is_refused_by_what_it_breaks() {
  let canonical = hdr10_plus_from_t35(&hdr10_plus_payload()).expect("libavutil parses it");
  assert_eq!(check_hdr10_plus(&canonical), Ok(()));
  let changed = |change: &dyn Fn(&mut Vec<u8>)| {
    let mut bytes = canonical.clone();
    change(&mut bytes);
    check_hdr10_plus(&bytes)
  };
  assert_eq!(
    check_hdr10_plus(&canonical[..canonical.len() - 1]),
    Err(HdrMetadataError::Size {
      expected: canonical.len(),
      found: canonical.len() - 1
    }),
    "an entry shorter than the struct"
  );
  assert_eq!(
    check_hdr10_plus(&[0; 16]),
    Err(HdrMetadataError::Size {
      expected: canonical.len(),
      found: 16
    })
  );
  assert_eq!(
    changed(&|b| set_u8(b, offset_of!(DynamicHdrPlus, num_windows), 4)),
    Err(HdrMetadataError::Count),
    "a fourth window, past the array"
  );
  assert_eq!(
    changed(&|b| set_u8(b, offset_of!(DynamicHdrPlus, num_windows), 0)),
    Err(HdrMetadataError::Count),
    "no window, which the reader refuses"
  );
  assert_eq!(
    changed(&|b| set_u8(b, window!(0, num_distribution_maxrgb_percentiles), 16)),
    Err(HdrMetadataError::Count)
  );
  assert_eq!(
    changed(&|b| set_u8(b, window!(0, num_bezier_curve_anchors), 16)),
    Err(HdrMetadataError::Count)
  );
  assert_eq!(
    changed(&|b| set_rational(b, window!(0, average_maxrgb), (500, 0))),
    Err(HdrMetadataError::Divisor),
    "a zero denominator"
  );
  assert_eq!(
    changed(&|b| set_rational(b, window!(0, knee_point_x), (i32::MAX / 2, 1))),
    Err(HdrMetadataError::Overflow),
    "a product past C's int"
  );
  assert_eq!(
    changed(&|b| set_rational(b, window!(0, maxscl), (1 << 17, 100_000))),
    Err(HdrMetadataError::Value),
    "past its 17 bits"
  );
  assert_eq!(
    changed(&|b| set_rational(b, window!(0, maxscl), (1, 3))),
    Err(HdrMetadataError::Value),
    "not a whole number of its unit"
  );
  assert_eq!(
    changed(&|b| set_rational(b, window!(0, maxscl), (1, 2))),
    Ok(()),
    "a whole number of its unit, over another denominator"
  );
  assert_eq!(
    changed(&|b| set_u8(b, window!(0, tone_mapping_flag), 2)),
    Err(HdrMetadataError::Value),
    "a flag of 2"
  );
  assert_eq!(
    changed(&|b| set_u8(b, offset_of!(DynamicHdrPlus, application_version), 0)),
    Err(HdrMetadataError::Version),
    "a version the writer writes over"
  );
  assert_eq!(
    changed(&|b| set_u8(b, window!(0, tone_mapping_flag), 0)),
    Err(HdrMetadataError::Syntax),
    "the last window's saturation flag, written inside its tone mapping"
  );
  assert_eq!(
    changed(&|b| {
      set_u8(b, window!(0, tone_mapping_flag), 0);
      set_u8(b, window!(0, color_saturation_mapping_flag), 0);
    }),
    Ok(()),
    "the last window, neither"
  );
  // Two windows: the second's geometry and statistics, canonical.
  let two = |b: &mut Vec<u8>| {
    set_u8(b, offset_of!(DynamicHdrPlus, num_windows), 2);
    for corner in [
      window!(1, window_upper_left_corner_x),
      window!(1, window_upper_left_corner_y),
      window!(1, window_lower_right_corner_x),
      window!(1, window_lower_right_corner_y),
    ] {
      set_rational(b, corner, (7, 1));
    }
    for i in 0..3 {
      set_rational(
        b,
        window!(1, maxscl) + i * size_of::<Rational>(),
        (0, 100_000),
      );
    }
    set_rational(b, window!(1, average_maxrgb), (0, 100_000));
    set_rational(b, window!(1, fraction_bright_pixels), (0, 1000));
  };
  assert_eq!(
    changed(&two),
    Ok(()),
    "two windows, the second without tone mapping"
  );
  assert_eq!(
    changed(&|b| {
      two(b);
      set_u8(b, window!(0, tone_mapping_flag), 0);
      set_u8(b, window!(0, color_saturation_mapping_flag), 0);
    }),
    Err(HdrMetadataError::Syntax),
    "a window without tone mapping before another: read apart"
  );
  assert_eq!(
    changed(&|b| {
      two(b);
      set_i32(b, window!(1, overlap_process_option), 2);
    }),
    Err(HdrMetadataError::Value)
  );
  assert_eq!(
    changed(&|b| {
      two(b);
      set_rational(b, window!(1, window_lower_right_corner_x), (i32::MIN, -1));
    }),
    Err(HdrMetadataError::Divisor),
    "INT_MIN / -1"
  );
  // A peak table of 26 rows, past the array.
  assert_eq!(
    changed(&|b| {
      set_u8(
        b,
        offset_of!(
          DynamicHdrPlus,
          targeted_system_display_actual_peak_luminance_flag
        ),
        1,
      );
      set_u8(
        b,
        offset_of!(
          DynamicHdrPlus,
          num_rows_targeted_system_display_actual_peak_luminance
        ),
        26,
      );
      set_u8(
        b,
        offset_of!(
          DynamicHdrPlus,
          num_cols_targeted_system_display_actual_peak_luminance
        ),
        2,
      );
    }),
    Err(HdrMetadataError::Count)
  );
  // A field the payload does not carry is not judged: the first window's
  // geometry, an anchor past the count.
  assert_eq!(
    changed(&|b| {
      set_rational(b, window!(0, window_upper_left_corner_x), (1, 0));
      set_rational(
        b,
        window!(0, bezier_curve_anchors) + 5 * size_of::<Rational>(),
        (9, 0),
      );
    }),
    Ok(())
  );
}

/// **An APP5 struct libavutil parsed is admitted, and one the writer would
/// read past its arrays, refuse, or write apart from what the reader reads
/// is refused by what it breaks.**
///
/// PLANT: bounding the curve count only where the image's curves are its
/// own admits the second image's 32.
#[test]
fn an_app5_entry_is_refused_by_what_it_breaks() {
  let canonical = smpte2094_app5_from_t35(&app5_payload()).expect("libavutil parses it");
  assert_eq!(check_smpte2094_app5(&canonical), Ok(()));
  let changed = |change: &dyn Fn(&mut Vec<u8>)| {
    let mut bytes = canonical.clone();
    change(&mut bytes);
    check_smpte2094_app5(&bytes)
  };
  assert!(matches!(
    check_smpte2094_app5(&canonical[..10]),
    Err(HdrMetadataError::Size { found: 10, .. })
  ));
  assert_eq!(
    changed(&|b| set_u8(b, app5!(application_version), 8)),
    Err(HdrMetadataError::Version)
  );
  assert_eq!(
    changed(&|b| set_u8(b, app5!(minimum_application_version), 3)),
    Err(HdrMetadataError::Version)
  );
  assert_eq!(
    changed(&|b| set_u8(b, app5!(num_alternate_images), 5)),
    Err(HdrMetadataError::Count)
  );
  assert_eq!(
    changed(&|b| set_u8(b, app5!(component_mixing_type[0], u8), 4)),
    Err(HdrMetadataError::Value)
  );
  assert_eq!(
    changed(&|b| set_u8(b, app5!(has_adaptive_tone_map_flag), 2)),
    Err(HdrMetadataError::Value)
  );
  // A second image sharing the first one's curves.
  let shared = |b: &mut Vec<u8>| {
    set_u8(b, app5!(num_alternate_images), 2);
    set_u8(b, app5!(has_common_curve_params_flag), 1);
    set_u8(b, app5!(gain_curve_num_control_points_minus_1[1], u8), 1);
    set_u8(b, app5!(gain_curve_use_pchip_slope_flag[1], u8), 1);
    for c in 0..2 {
      let x = Fields(&canonical).u16(app5!(gain_curve_control_points_x[0][c], 32, u16));
      set_u16(b, app5!(gain_curve_control_points_x[1][c], 32, u16), x);
    }
  };
  assert_eq!(changed(&shared), Ok(()), "the first image's curves, copied");
  assert_eq!(
    changed(&|b| {
      shared(b);
      set_u8(b, app5!(gain_curve_num_control_points_minus_1[1], u8), 32);
    }),
    Err(HdrMetadataError::Count),
    "a count past [1][31], which the writer reads its points by"
  );
  assert_eq!(
    changed(&|b| {
      shared(b);
      set_u8(b, app5!(gain_curve_num_control_points_minus_1[1], u8), 3);
    }),
    Err(HdrMetadataError::Syntax),
    "a count the reader replaces with the first image's"
  );
  assert_eq!(
    changed(&|b| {
      shared(b);
      set_u16(b, app5!(gain_curve_control_points_x[1][0], 32, u16), 1);
    }),
    Err(HdrMetadataError::Syntax),
    "an x point the reader replaces"
  );
  // Fields the payload does not carry are not judged: a curve point past
  // the count.
  assert_eq!(
    changed(&|b| set_u16(b, app5!(gain_curve_control_points_y[0][7], 32, u16), 9)),
    Ok(())
  );
}

/// **A payload libavutil cannot parse is named by its code.**
#[test]
fn a_payload_libavutil_refuses_is_named() {
  assert!(matches!(
    hdr10_plus_from_t35(&[0; 2]),
    Err(HdrMetadataError::Parse(code)) if code < 0
  ));
  assert!(matches!(
    smpte2094_app5_from_t35(&[0xFF]),
    Err(HdrMetadataError::Parse(code)) if code < 0
  ));
}
