//! The stage's other body: the same seat, answering that it cannot
//! fill it.
//!
//! Reached on every target [`crate::vtscale`]'s gate turns away — a
//! non-Apple target, or an Apple one whose availability window Rust
//! cannot express (iOS, tvOS, watchOS) — and on any build that asked
//! for the stage to be compiled out.
//!
//! `VTPixelTransferSession` is VideoToolbox, so this stage exists only
//! on Apple targets. The other three backends this crate wires —
//! [`crate::Backend::Vaapi`], [`crate::Backend::Cuda`],
//! [`crate::Backend::D3d11va`] — each have a native scaling seam of
//! their own (VAAPI's VPP,
//! [#57](https://github.com/findit-studio/mediadecode/issues/57);
//! NVDEC/CUVID in-decode scaling,
//! [#56](https://github.com/findit-studio/mediadecode/issues/56); the
//! D3D11 Video Processor,
//! [#58](https://github.com/findit-studio/mediadecode/issues/58)), and
//! each is filed rather than fabricated here. Until one of them lands,
//! this body keeps the answer honest: `Unsupported`, on a road that
//! delivers full-size pictures and says so.

use ffmpeg_next::frame;
use mediadecode::decoder::ScaledOutputCapability;

/// The scaled-output stage on a target with no pixel-transfer session.
/// Holds nothing, because there is nothing to hold.
pub(crate) struct ScaledOutput;

impl ScaledOutput {
  /// A stage that will refuse everything.
  pub(crate) const fn new() -> Self {
    Self
  }

  /// Whether this build can honor a request at all.
  pub(crate) const fn supported() -> bool {
    false
  }

  /// Always `None` — nothing is ever accepted here.
  #[cfg(test)]
  pub(crate) const fn requested(&self) -> Option<(u32, u32)> {
    None
  }

  /// Nothing was ever promised here, so nothing can be broken.
  pub(crate) const fn promise_stands(&self) -> bool {
    true
  }

  /// Nothing is ever standing here, so there is nothing to cancel.
  pub(crate) fn cancel(&mut self) {}

  /// Nothing is built here, so nothing is retired.
  pub(crate) fn retire(&mut self) {}

  /// Refuses. Nothing was ever standing, so a refusal returning the
  /// session to full size is what it has always been doing.
  pub(crate) fn request(&mut self, size: (u32, u32), source: (u32, u32)) -> ScaledOutputCapability {
    let _ = (size, source);
    ScaledOutputCapability::Unsupported
  }

  /// Always stands down: the caller downloads the full-size frame.
  pub(crate) fn stage(&mut self, src: &frame::Video) -> Option<&frame::Video> {
    let _ = src;
    None
  }
}
