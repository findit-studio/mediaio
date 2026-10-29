//! The request hook's answer, on whichever target these run on.
//!
//! Deliberately the *only* thing tested here: the stage's arithmetic
//! moved into `videotoolbox/tests.rs` alongside the body that owns it,
//! so a build without a stage carries neither the code nor a test for
//! it. What every build does carry is this hook, and what it must
//! always do is answer honestly about itself.

use mediadecode::decoder::ScaledOutputCapability;

use super::ScaledOutput;

#[test]
fn the_request_hook_answers_what_this_target_can_do() {
  let mut stage = ScaledOutput::new();
  let accepted = stage.request((512, 288), (1920, 1080));
  if ScaledOutput::supported() {
    assert_eq!(accepted, ScaledOutputCapability::Supported);
    assert_eq!(stage.requested(), Some((512, 288)));
  } else {
    assert_eq!(accepted, ScaledOutputCapability::Unsupported);
    assert_eq!(stage.requested(), None);
  }
}

#[test]
fn a_refused_request_returns_the_session_to_full_size() {
  let mut stage = ScaledOutput::new();
  let first = stage.request((512, 288), (1920, 1080));
  // Zero and upscale are refused on every target.
  assert_eq!(
    stage.request((0, 288), (1920, 1080)),
    ScaledOutputCapability::Unsupported
  );
  if ScaledOutput::supported() {
    assert_eq!(first, ScaledOutputCapability::Supported);
  }
  // **And the refusal is not a no-op.** The trait says an `Unsupported`
  // answer from this hook leaves the session decoding at full coded
  // size, and a caller acting on it resamples for itself — so a session
  // that went on fitting to the older request would have it resample an
  // already-fitted picture.
  assert_eq!(
    stage.requested(),
    None,
    "a refused request returns the session to full size"
  );
  // A refusal with nothing standing is simply the same answer again.
  assert_eq!(
    stage.request((3840, 2160), (1920, 1080)),
    ScaledOutputCapability::Unsupported
  );
  assert_eq!(stage.requested(), None);
}
