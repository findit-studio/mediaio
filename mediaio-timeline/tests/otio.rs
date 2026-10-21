//! The law timeline's OpenTimelineIO, against goldens predicted by hand
//! before the export existed — and read back.
#![cfg(any(feature = "std", feature = "alloc"))]

mod common;

use mediaio_timeline::otio::{Invalid, OtioTarget, from_otio, to_otio, validate_json};

const GOLDEN_V0_15: &str = include_str!("golden/law.v0_15.otio");
const GOLDEN_LEGACY: &str = include_str!("golden/law.legacy.otio");

/// Byte-for-byte equality, failing at the first line that differs.
fn same_text(actual: &str, golden: &str, name: &str) {
  if actual == golden {
    return;
  }
  for (index, (a, g)) in actual.lines().zip(golden.lines()).enumerate() {
    assert_eq!(a, g, "{name}: first difference at line {}", index + 1);
  }
  assert_eq!(
    actual.lines().count(),
    golden.lines().count(),
    "{name}: line count"
  );
  assert_eq!(actual, golden, "{name}: line endings");
}

#[test]
fn the_law_timeline_exports_its_golden_byte_for_byte() {
  let law = common::law();
  same_text(
    &to_otio(&law, OtioTarget::V0_15Plus).unwrap(),
    GOLDEN_V0_15,
    "law.v0_15.otio",
  );
  same_text(
    &to_otio(&law, OtioTarget::Legacy).unwrap(),
    GOLDEN_LEGACY,
    "law.legacy.otio",
  );
}

#[test]
fn the_goldens_pass_the_self_check_for_their_target() {
  assert_eq!(validate_json(GOLDEN_V0_15, OtioTarget::V0_15Plus), Ok(()));
  assert_eq!(validate_json(GOLDEN_LEGACY, OtioTarget::Legacy), Ok(()));
  // Readers before 0.15 know no Clip.2.
  assert!(matches!(
    validate_json(GOLDEN_V0_15, OtioTarget::Legacy),
    Err(Invalid::Shape(_))
  ));
}

/// **The goldens read back as the law timeline**, for both targets: every
/// count, timebase and rate as the model held it, the markers among them.
#[test]
fn the_goldens_read_back_as_the_law_timeline() {
  let law = common::law();
  assert_eq!(from_otio(GOLDEN_V0_15), Ok(law.clone()));
  assert_eq!(from_otio(GOLDEN_LEGACY), Ok(law));
}
