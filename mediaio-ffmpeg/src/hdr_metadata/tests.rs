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

/// **A check of the sizes resolved before judges as the check that asks
/// libavutil, and asks it nothing** (R18, Codex R17): the sizes
/// [`LinkedSizes::resolve`] asks are the ones the checks ask; every entry —
/// the canonical one, one a byte short, one of 16 bytes — answers the same
/// of both checks; and of sizes another layout's, the sized check answers
/// [`HdrMetadataError::Layout`] of what it was given, so that what it reads
/// is the sizes, not the library.
///
/// PLANT: the sized check asking the library again, the other layout
/// judged as the linked one's.
#[test]
fn a_check_of_resolved_sizes_judges_as_the_check_that_asks() {
  let sizes = LinkedSizes::resolve().expect("the sizes");
  assert_eq!(
    sizes,
    LinkedSizes::of(
      linked_hdr_plus_size().expect("the HDR10+ size"),
      linked_app5_size().expect("the APP5 size")
    ),
    "resolved as the checks ask"
  );
  let plus = hdr10_plus_from_t35(&hdr10_plus_payload()).expect("libavutil parses it");
  let app5 = smpte2094_app5_from_t35(&app5_payload()).expect("libavutil parses it");
  for entry in [plus.clone(), plus[..plus.len() - 1].to_vec(), vec![0; 16]] {
    assert_eq!(
      check_hdr10_plus_sized(&entry, sizes),
      check_hdr10_plus(&entry),
      "HDR10+ of {} bytes",
      entry.len()
    );
  }
  for entry in [app5.clone(), app5[..app5.len() - 1].to_vec(), vec![0; 16]] {
    assert_eq!(
      check_smpte2094_app5_sized(&entry, sizes),
      check_smpte2094_app5(&entry),
      "APP5 of {} bytes",
      entry.len()
    );
  }
  let (plus_size, app5_size) = (
    size_of::<DynamicHdrPlus>(),
    size_of::<DynamicHdrSmpte2094App5>(),
  );
  let other = LinkedSizes::of(plus_size + 8, app5_size + 8);
  assert_eq!(
    check_hdr10_plus_sized(&plus, other),
    Err(HdrMetadataError::Layout {
      linked: plus_size + 8,
      mirrored: plus_size
    }),
    "HDR10+ judged of the sizes given"
  );
  assert_eq!(
    check_smpte2094_app5_sized(&app5, other),
    Err(HdrMetadataError::Layout {
      linked: app5_size + 8,
      mirrored: app5_size
    }),
    "APP5 judged of the sizes given"
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
    Err(HdrMetadataError::Uninitialized),
    "the last window's saturation flag and weight, sized but written inside its tone mapping: \
     178 bits sized, 171 written"
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
    Ok(()),
    "a window without tone mapping before another of none: read apart, as zeros, back as itself"
  );
  assert_eq!(
    changed(&|b| {
      two(b);
      set_u8(b, window!(0, tone_mapping_flag), 0);
      set_u8(b, window!(0, color_saturation_mapping_flag), 0);
      set_u8(b, window!(1, tone_mapping_flag), 1);
      set_rational(b, window!(1, knee_point_x), (0, 4095));
      set_rational(b, window!(1, knee_point_y), (0, 4095));
    }),
    Err(HdrMetadataError::Syntax),
    "the second window's tone mapping, read as the first's saturation flag"
  );
  assert_eq!(
    changed(&|b| {
      two(b);
      set_u8(b, window!(1, tone_mapping_flag), 1);
    }),
    Err(HdrMetadataError::Divisor),
    "a knee point over zero, which the writer divides by"
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

/// A zeroed `AVDynamicHDRPlus` of the linked library's own size.
fn zeroed_hdr10_plus() -> Vec<u8> {
  vec![0; linked_hdr_plus_size().expect("the linked size")]
}

/// The bytes libavutil's HDR10+ serializer puts out of `entry`, into a
/// buffer filled with `fill` first: the payload it declares, or its code.
fn hdr10_plus_to_t35(entry: &[u8], fill: u8) -> Result<Vec<u8>, i32> {
  let mut size = 0usize;
  // SAFETY: libavutil's allocator answers a zeroed struct of `size` bytes,
  // its own alignment, or null.
  let s = unsafe { c_shims::av_dynamic_hdr_plus_alloc(&mut size) };
  assert!(!s.is_null() && size == entry.len(), "the struct, allocated");
  let mut buffer = vec![fill; HDR_PLUS_MAX_PAYLOAD + 64];
  let mut data = buffer.as_mut_ptr();
  let mut capacity = buffer.len();
  // SAFETY: `s` holds `size` bytes, `entry`'s length, and the two do not
  // overlap; the struct is one the laws build with every count within its
  // array and every denominator its unit, so the serializer reads inside it
  // and divides soundly; `data` points at `capacity` bytes it may write.
  let status = unsafe {
    core::ptr::copy_nonoverlapping(entry.as_ptr(), s.cast::<u8>(), size);
    c_shims::av_dynamic_hdr_plus_to_t35(s.cast_const(), &mut data, &mut capacity)
  };
  // SAFETY: libavutil's own allocation, freed once.
  unsafe { ffmpeg_next::ffi::av_free(s) };
  if status < 0 {
    return Err(status);
  }
  Ok(buffer[..capacity].to_vec())
}

/// Every field of HDR10+'s `fields` its payload carries, by its flags and
/// counts, as the integers they are — rationals as their values over their
/// units — in the reader's order.
fn hdr10_plus_carried(fields: &Fields<'_>) -> Vec<(i64, i64)> {
  let mut out: Vec<(i64, i64)> = Vec::new();
  let mut int = |value: i64| out.push((value, 1));
  let windows = usize::from(fields.u8(offset_of!(DynamicHdrPlus, num_windows)));
  int(i64::from(
    fields.u8(offset_of!(DynamicHdrPlus, application_version)),
  ));
  int(windows as i64);
  let mut rationals = Vec::new();
  let mut rational = |at: usize| {
    let (num, den) = fields.rational(at);
    rationals.push((i64::from(num), i64::from(den)));
  };
  for w in 1..windows.min(3) {
    for corner in [
      window!(w, window_upper_left_corner_x),
      window!(w, window_upper_left_corner_y),
      window!(w, window_lower_right_corner_x),
      window!(w, window_lower_right_corner_y),
    ] {
      rational(corner);
    }
  }
  rational(offset_of!(
    DynamicHdrPlus,
    targeted_system_display_maximum_luminance
  ));
  let tables = [
    (
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
      offset_of!(
        DynamicHdrPlus,
        targeted_system_display_actual_peak_luminance
      ),
    ),
    (
      offset_of!(DynamicHdrPlus, mastering_display_actual_peak_luminance_flag),
      offset_of!(
        DynamicHdrPlus,
        num_rows_mastering_display_actual_peak_luminance
      ),
      offset_of!(
        DynamicHdrPlus,
        num_cols_mastering_display_actual_peak_luminance
      ),
      offset_of!(DynamicHdrPlus, mastering_display_actual_peak_luminance),
    ),
  ];
  let mut flags = Vec::new();
  for (index, (flag, rows, cols, table)) in tables.into_iter().enumerate() {
    let set = fields.u8(flag);
    flags.push(i64::from(set));
    if set != 0 {
      let (rows, cols) = (usize::from(fields.u8(rows)), usize::from(fields.u8(cols)));
      flags.push(rows as i64);
      flags.push(cols as i64);
      for i in 0..rows.min(25) {
        for j in 0..cols.min(25) {
          rational(table + (i * 25 + j) * size_of::<Rational>());
        }
      }
    }
    if index == 0 {
      for w in 0..windows.min(3) {
        for i in 0..3 {
          rational(window!(w, maxscl) + i * size_of::<Rational>());
        }
        rational(window!(w, average_maxrgb));
        let percentiles = usize::from(fields.u8(window!(w, num_distribution_maxrgb_percentiles)));
        flags.push(percentiles as i64);
        for i in 0..percentiles.min(15) {
          let entry = window!(w, distribution_maxrgb) + i * size_of::<HdrPlusPercentile>();
          flags.push(i64::from(
            fields.u8(entry + offset_of!(HdrPlusPercentile, percentage)),
          ));
          rational(entry + offset_of!(HdrPlusPercentile, percentile));
        }
        rational(window!(w, fraction_bright_pixels));
      }
    }
  }
  for w in 0..windows.min(3) {
    let tone = fields.u8(window!(w, tone_mapping_flag));
    flags.push(i64::from(tone));
    if tone != 0 {
      rational(window!(w, knee_point_x));
      rational(window!(w, knee_point_y));
      let anchors = usize::from(fields.u8(window!(w, num_bezier_curve_anchors)));
      flags.push(anchors as i64);
      for i in 0..anchors.min(15) {
        rational(window!(w, bezier_curve_anchors) + i * size_of::<Rational>());
      }
    }
    let saturation = fields.u8(window!(w, color_saturation_mapping_flag));
    flags.push(i64::from(saturation));
    if saturation != 0 {
      rational(window!(w, color_saturation_weight));
    }
  }
  out.extend(flags.into_iter().map(|flag| (flag, 1)));
  // Each rational by its value: its numerator over one, its denominator
  // its unit's — compared cross-multiplied.
  out.extend(rationals);
  out
}

/// Whether two carried lists hold the same values, rationals compared by
/// value.
fn same_values(a: &[(i64, i64)], b: &[(i64, i64)]) -> bool {
  a.len() == b.len()
    && a
      .iter()
      .zip(b)
      .all(|(&(an, ad), &(bn, bd))| ad != 0 && bd != 0 && an * bd == bn * ad)
}

/// libavutil's own answer for `entry`: whether its serializer writes every
/// byte it declares — the payload is the same into a buffer of zeros and
/// one of `0xFF` — and its reader reads back every field the entry
/// carries.
fn libavutil_round_trips_hdr10_plus(entry: &[u8]) -> bool {
  let (Ok(zeros), Ok(ones)) = (hdr10_plus_to_t35(entry, 0), hdr10_plus_to_t35(entry, 0xFF)) else {
    return false;
  };
  if zeros != ones {
    return false;
  }
  match hdr10_plus_from_t35(&zeros) {
    Ok(back) => same_values(
      &hdr10_plus_carried(&Fields(&back)),
      &hdr10_plus_carried(&Fields(entry)),
    ),
    Err(_) => false,
  }
}

/// An HDR10+ struct of `windows`, its targeted and mastering peak tables of
/// `tables` rows and columns where given, every window's tone mapping and
/// saturation `(tone, anchors, saturation)` — its knee, anchors and weight
/// all `fill`'s, each over its unit — and every other value 0 over its unit.
fn hdr10_plus_entry(
  windows: usize,
  tables: [Option<(u8, u8)>; 2],
  maps: &[(bool, u8, bool)],
  fill: u32,
) -> Vec<u8> {
  let mut b = zeroed_hdr10_plus();
  set_u8(&mut b, offset_of!(DynamicHdrPlus, application_version), 1);
  set_u8(
    &mut b,
    offset_of!(DynamicHdrPlus, num_windows),
    windows as u8,
  );
  for w in 1..windows {
    for corner in [
      window!(w, window_upper_left_corner_x),
      window!(w, window_upper_left_corner_y),
      window!(w, window_lower_right_corner_x),
      window!(w, window_lower_right_corner_y),
    ] {
      set_rational(&mut b, corner, (0, 1));
    }
  }
  set_rational(
    &mut b,
    offset_of!(DynamicHdrPlus, targeted_system_display_maximum_luminance),
    (0, 1),
  );
  let table_fields = [
    (
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
      offset_of!(
        DynamicHdrPlus,
        targeted_system_display_actual_peak_luminance
      ),
    ),
    (
      offset_of!(DynamicHdrPlus, mastering_display_actual_peak_luminance_flag),
      offset_of!(
        DynamicHdrPlus,
        num_rows_mastering_display_actual_peak_luminance
      ),
      offset_of!(
        DynamicHdrPlus,
        num_cols_mastering_display_actual_peak_luminance
      ),
      offset_of!(DynamicHdrPlus, mastering_display_actual_peak_luminance),
    ),
  ];
  for (table, (flag, rows, cols, entries)) in tables.into_iter().zip(table_fields) {
    if let Some((r, c)) = table {
      set_u8(&mut b, flag, 1);
      set_u8(&mut b, rows, r);
      set_u8(&mut b, cols, c);
      for i in 0..usize::from(r) {
        for j in 0..usize::from(c) {
          set_rational(
            &mut b,
            entries + (i * 25 + j) * size_of::<Rational>(),
            (0, 15),
          );
        }
      }
    }
  }
  for (w, &(tone, anchors, saturation)) in maps.iter().enumerate() {
    for i in 0..3 {
      set_rational(
        &mut b,
        window!(w, maxscl) + i * size_of::<Rational>(),
        (0, 100_000),
      );
    }
    set_rational(&mut b, window!(w, average_maxrgb), (0, 100_000));
    set_rational(&mut b, window!(w, fraction_bright_pixels), (0, 1000));
    set_u8(&mut b, window!(w, tone_mapping_flag), u8::from(tone));
    set_u8(&mut b, window!(w, num_bezier_curve_anchors), anchors);
    let at = |most: u32| (fill.min(most)) as i32;
    set_rational(&mut b, window!(w, knee_point_x), (at(4095), 4095));
    set_rational(&mut b, window!(w, knee_point_y), (at(4095), 4095));
    for i in 0..usize::from(anchors) {
      set_rational(
        &mut b,
        window!(w, bezier_curve_anchors) + i * size_of::<Rational>(),
        (at(1023), 1023),
      );
    }
    set_u8(
      &mut b,
      window!(w, color_saturation_mapping_flag),
      u8::from(saturation),
    );
    set_rational(&mut b, window!(w, color_saturation_weight), (at(63), 8));
  }
  b
}

/// **The HDR10+ check is libavutil's own answer, over every small shape of
/// flags and counts**: one to three windows; no peak table, one of 2 × 2
/// and one of 3 × 3 entries, targeted and mastering; each window's tone
/// mapping with no anchor or one, or none, its saturation flag set or not;
/// the knee, anchors and weight all zeros, or all their units' largest. In
/// every case the check admits the entry exactly where libavutil's
/// serializer writes every byte it declares — the same payload into a
/// buffer of zeros as into one of `0xFF` — and its reader reads back every
/// field the entry carries; nothing stricter, nothing laxer.
///
/// PLANT: dropping the written-bytes rule admits the cases whose last byte
/// goes out unwritten — the 169/168 one among them.
#[test]
fn the_hdr10_plus_check_is_libavutils_own_answer() {
  let tables = [None, Some((2, 2)), Some((3, 3))];
  let maps = [
    (false, 0, false),
    (false, 0, true),
    (true, 0, false),
    (true, 0, true),
    (true, 1, false),
    (true, 1, true),
  ];
  let (mut cases, mut admitted) = (0, 0);
  for windows in 1..=3usize {
    let shapes = maps.len().pow(windows as u32);
    for targeted in tables {
      for mastering in tables {
        for shape in 0..shapes {
          let chosen: Vec<_> = (0..windows)
            .map(|w| maps[shape / maps.len().pow(w as u32) % maps.len()])
            .collect();
          for fill in [0, u32::MAX] {
            let entry = hdr10_plus_entry(windows, [targeted, mastering], &chosen, fill);
            let ours = check_hdr10_plus(&entry);
            let theirs = libavutil_round_trips_hdr10_plus(&entry);
            assert_eq!(
              ours.is_ok(),
              theirs,
              "{windows} windows, tables {targeted:?} {mastering:?}, maps {chosen:?}, fill {fill}: \
               ours {ours:?}"
            );
            cases += 1;
            admitted += usize::from(theirs);
          }
        }
      }
    }
  }
  assert_eq!(cases, 2 * 9 * (6 + 36 + 216));
  assert!(admitted > 0 && admitted < cases, "{admitted} of {cases}");
}

/// **Codex's two cases**: one window with a 3 × 3 targeted peak table, no
/// percentiles, no mastering table and neither tone mapping nor saturation
/// — 169 bits declared, 168 written, the 22nd byte left unwritten — is
/// refused; two windows with no table, no percentiles and every tone and
/// saturation flag zero — 360 declared, 358 written, in 45 bytes either
/// way, read back as four zeros — is admitted, and libavutil round-trips
/// it.
#[test]
fn hdr10_pluss_unwritten_byte_is_refused_and_its_zero_windows_admitted() {
  let unwritten = hdr10_plus_entry(1, [Some((3, 3)), None], &[(false, 0, false)], 0);
  assert_eq!(
    check_hdr10_plus(&unwritten),
    Err(HdrMetadataError::Uninitialized)
  );
  let zeros = hdr10_plus_to_t35(&unwritten, 0).expect("written");
  let ones = hdr10_plus_to_t35(&unwritten, 0xFF).expect("written");
  assert_eq!(zeros.len(), 22, "169 bits declared");
  assert_ne!(zeros[21], ones[21], "the 22nd byte, never written");
  let two = hdr10_plus_entry(2, [None, None], &[(false, 0, false); 2], 0);
  assert_eq!(check_hdr10_plus(&two), Ok(()));
  assert!(libavutil_round_trips_hdr10_plus(&two));
  assert_eq!(hdr10_plus_to_t35(&two, 0).expect("written").len(), 45);
}

/// The bytes libavutil's APP5 serializer puts out of `entry` into
/// Matroska's 855 bytes, filled with `fill` first: the payload it declares,
/// or its code.
fn app5_to_t35(entry: &[u8], fill: u8) -> Result<Vec<u8>, i32> {
  let mut size = 0usize;
  // SAFETY: libavutil's allocator answers a zeroed struct of `size` bytes,
  // its own alignment, or null.
  let s = unsafe { c_shims::av_dynamic_hdr_smpte2094_app5_alloc(&mut size) };
  assert!(!s.is_null() && size == entry.len(), "the struct, allocated");
  let mut buffer = vec![fill; MATROSKA_APP5_MAX_PAYLOAD];
  let mut data = buffer.as_mut_ptr();
  let mut capacity = buffer.len();
  // SAFETY: as `hdr10_plus_to_t35`: the laws' structs keep every count
  // within its array, which is all this serializer trusts.
  let status = unsafe {
    core::ptr::copy_nonoverlapping(entry.as_ptr(), s.cast::<u8>(), size);
    c_shims::av_dynamic_hdr_smpte2094_app5_to_t35(s.cast_const(), &mut data, &mut capacity)
  };
  // SAFETY: libavutil's own allocation, freed once.
  unsafe { ffmpeg_next::ffi::av_free(s) };
  if status < 0 {
    return Err(status);
  }
  Ok(buffer[..capacity].to_vec())
}

/// Every field of APP5's `fields` its reader sets from a payload — read,
/// or copied from the first image where the common flags say — by the
/// struct's own flags and counts, in the reader's order; a field a flag
/// leaves out reads back zero and is not listed.
fn app5_carried(fields: &Fields<'_>) -> Vec<u32> {
  let mut out = Vec::new();
  let u8_at = |at: usize| u32::from(fields.u8(at));
  let u16_at = |at: usize| u32::from(fields.u16(at));
  out.extend([
    u8_at(app5!(application_version)),
    u8_at(app5!(minimum_application_version)),
    u8_at(app5!(has_custom_hdr_reference_white_flag)),
    u8_at(app5!(has_adaptive_tone_map_flag)),
  ]);
  if fields.u8(app5!(has_custom_hdr_reference_white_flag)) != 0 {
    out.push(u16_at(app5!(hdr_reference_white)));
  }
  if fields.u8(app5!(has_adaptive_tone_map_flag)) == 0 {
    return out;
  }
  out.push(u16_at(app5!(baseline_hdr_headroom)));
  out.push(u8_at(app5!(use_reference_white_tone_mapping_flag)));
  if fields.u8(app5!(use_reference_white_tone_mapping_flag)) != 0 {
    return out;
  }
  let images = usize::from(fields.u8(app5!(num_alternate_images))).min(4);
  out.extend([
    images as u32,
    u8_at(app5!(gain_application_space_chromaticities_flag)),
    u8_at(app5!(has_common_component_mix_params_flag)),
    u8_at(app5!(has_common_curve_params_flag)),
  ]);
  if fields.u8(app5!(gain_application_space_chromaticities_flag)) == 3 {
    for r in 0..8 {
      out.push(u16_at(app5!(gain_application_space_chromaticities[r], u16)));
    }
  }
  for a in 0..images {
    out.push(u16_at(app5!(alternate_hdr_headrooms[a], u16)));
    let kind = fields.u8(app5!(component_mixing_type[a], u8));
    out.push(u32::from(kind));
    if kind == 3 {
      for k in 0..6 {
        let flag = fields.u8(app5!(has_component_mixing_coefficient_flag[a][k], 6, u8));
        out.push(u32::from(flag));
        if flag != 0 {
          out.push(u16_at(app5!(component_mixing_coefficient[a][k], 6, u16)));
        }
      }
    }
    let points =
      usize::from(fields.u8(app5!(gain_curve_num_control_points_minus_1[a], u8))).min(31);
    let pchip = fields.u8(app5!(gain_curve_use_pchip_slope_flag[a], u8));
    out.extend([points as u32, u32::from(pchip)]);
    for c in 0..=points {
      out.push(u16_at(app5!(gain_curve_control_points_x[a][c], 32, u16)));
      out.push(u16_at(app5!(gain_curve_control_points_y[a][c], 32, u16)));
      if pchip == 0 {
        out.push(u16_at(app5!(
          gain_curve_control_points_theta[a][c],
          32,
          u16
        )));
      }
    }
  }
  out
}

/// libavutil's own answer for an APP5 `entry`, as for HDR10+'s.
fn libavutil_round_trips_app5(entry: &[u8]) -> bool {
  let (Ok(zeros), Ok(ones)) = (app5_to_t35(entry, 0), app5_to_t35(entry, 0xFF)) else {
    return false;
  };
  if zeros != ones {
    return false;
  }
  match smpte2094_app5_from_t35(&zeros) {
    Ok(back) => app5_carried(&Fields(&back)) == app5_carried(&Fields(entry)),
    Err(_) => false,
  }
}

/// **The APP5 check is libavutil's own answer, over every small shape of
/// its flags and counts**: the adaptive tone map or none, by reference
/// white or by images; one or two images, sharing the first one's mixing
/// and curves or not; each image's mixing of type 0 or 3, its coefficient
/// flags none or every other; one curve point or two, slopes by PCHIP or
/// stated; the second image's fields the first's, or one of them — its
/// mixing type, a coefficient its flag names, one its flag leaves out, its
/// curve count, an x point — another. In every case the check admits the
/// entry exactly where libavutil writes every byte it declares into
/// Matroska's buffer and reads back every field its reader sets.
///
/// PLANT: comparing a coefficient a flag leaves out refuses the cases where
/// only that coefficient differs, which libavutil round-trips.
#[test]
fn the_app5_check_is_libavutils_own_answer() {
  let (mut cases, mut admitted) = (0, 0);
  for (adaptive, by_white) in [(false, false), (true, true), (true, false)] {
    for images in [1, 2usize] {
      for (common_mix, common_curve) in [(false, false), (true, false), (false, true), (true, true)]
      {
        for kind in [0, 3u8] {
          for points in [0, 1u8] {
            for pchip in [0, 1u8] {
              // The second image: the first's own, or one field apart.
              for apart in 0..6 {
                let mut b = canonical_app5();
                set_u8(
                  &mut b,
                  app5!(has_adaptive_tone_map_flag),
                  u8::from(adaptive),
                );
                set_u8(
                  &mut b,
                  app5!(use_reference_white_tone_mapping_flag),
                  u8::from(by_white),
                );
                set_u8(&mut b, app5!(num_alternate_images), images as u8);
                set_u8(
                  &mut b,
                  app5!(has_common_component_mix_params_flag),
                  u8::from(common_mix),
                );
                set_u8(
                  &mut b,
                  app5!(has_common_curve_params_flag),
                  u8::from(common_curve),
                );
                for a in 0..images {
                  set_u16(
                    &mut b,
                    app5!(alternate_hdr_headrooms[a], u16),
                    1000 + a as u16,
                  );
                  set_u8(&mut b, app5!(component_mixing_type[a], u8), kind);
                  for k in 0..6 {
                    set_u8(
                      &mut b,
                      app5!(has_component_mixing_coefficient_flag[a][k], 6, u8),
                      u8::from(k % 2 == 0),
                    );
                    set_u16(
                      &mut b,
                      app5!(component_mixing_coefficient[a][k], 6, u16),
                      if k % 2 == 0 { 100 + k as u16 } else { 0 },
                    );
                  }
                  set_u8(
                    &mut b,
                    app5!(gain_curve_num_control_points_minus_1[a], u8),
                    points,
                  );
                  set_u8(&mut b, app5!(gain_curve_use_pchip_slope_flag[a], u8), pchip);
                  for c in 0..=usize::from(points) {
                    set_u16(
                      &mut b,
                      app5!(gain_curve_control_points_x[a][c], 32, u16),
                      10 + c as u16,
                    );
                    set_u16(
                      &mut b,
                      app5!(gain_curve_control_points_y[a][c], 32, u16),
                      20 + c as u16 + a as u16,
                    );
                    set_u16(
                      &mut b,
                      app5!(gain_curve_control_points_theta[a][c], 32, u16),
                      30 + c as u16,
                    );
                  }
                }
                if images == 2 {
                  match apart {
                    1 => set_u8(&mut b, app5!(component_mixing_type[1], u8), 3 - kind),
                    2 => set_u16(&mut b, app5!(component_mixing_coefficient[1][0], 6, u16), 7),
                    3 => set_u16(&mut b, app5!(component_mixing_coefficient[1][1], 6, u16), 7),
                    4 => set_u8(
                      &mut b,
                      app5!(gain_curve_num_control_points_minus_1[1], u8),
                      1 - points,
                    ),
                    5 => set_u16(
                      &mut b,
                      app5!(gain_curve_control_points_x[1][0], 32, u16),
                      99,
                    ),
                    _ => {}
                  }
                }
                let ours = check_smpte2094_app5(&b);
                let theirs = libavutil_round_trips_app5(&b);
                assert_eq!(
                  ours.is_ok(),
                  theirs,
                  "adaptive {adaptive}, by white {by_white}, {images} images, common {common_mix} \
                   {common_curve}, type {kind}, points {points}, pchip {pchip}, apart {apart}: \
                   ours {ours:?}"
                );
                cases += 1;
                admitted += usize::from(theirs);
              }
            }
          }
        }
      }
    }
  }
  assert_eq!(cases, 3 * 2 * 4 * 2 * 2 * 2 * 6);
  assert!(admitted > 0 && admitted < cases, "{admitted} of {cases}");
}

/// A zeroed APP5 struct of the linked size, versions 0, a custom reference
/// white of 203, a baseline of 1000.
fn canonical_app5() -> Vec<u8> {
  let mut b = vec![0; linked_app5_size().expect("the linked size")];
  set_u8(&mut b, app5!(has_custom_hdr_reference_white_flag), 1);
  set_u16(&mut b, app5!(hdr_reference_white), 203);
  set_u16(&mut b, app5!(baseline_hdr_headroom), 1000);
  b
}
