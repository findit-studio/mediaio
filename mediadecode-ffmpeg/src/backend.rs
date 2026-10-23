use derive_more::IsVariant;
use ffmpeg_next::ffi::{AVHWDeviceType, AVPixelFormat};

/// Hardware decoding backend.
///
/// `hwdecode` only manages **hardware** decoders — software fallback is
/// out of scope. If no backend in [`probe_order`] for the current platform
/// can decode a stream, [`crate::VideoDecoder::open`] returns
/// [`crate::Error::ProbeExhausted`] and the caller decides how to fall
/// back (e.g. by opening an `ffmpeg::decoder::Video` directly).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, IsVariant)]
pub enum Backend {
  /// Apple VideoToolbox (macOS, iOS, iPadOS, tvOS, visionOS).
  VideoToolbox,
  /// Linux Video Acceleration API (Intel / AMD GPUs).
  Vaapi,
  /// NVIDIA NVDEC via CUDA (Linux / Windows on NVIDIA hardware).
  Cuda,
  /// Microsoft Direct3D 11 Video Acceleration (Windows).
  D3d11va,
}

impl Backend {
  /// `AVHWDeviceType` corresponding to this backend.
  pub(crate) fn av_hwdevice_type(self) -> AVHWDeviceType {
    match self {
      Self::VideoToolbox => AVHWDeviceType::AV_HWDEVICE_TYPE_VIDEOTOOLBOX,
      Self::Vaapi => AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI,
      Self::Cuda => AVHWDeviceType::AV_HWDEVICE_TYPE_CUDA,
      Self::D3d11va => AVHWDeviceType::AV_HWDEVICE_TYPE_D3D11VA,
    }
  }

  /// Hardware pixel format the codec is expected to produce when this
  /// backend is in use. (The post-`av_hwframe_transfer_data` CPU format is
  /// typically `NV12` or `P010LE`; this is the *pre-transfer* sentinel.)
  ///
  /// Returns a `AVPixelFormat` value constructed from a hardcoded constant
  /// in our bindings — never reads an enum value supplied by FFmpeg, so
  /// no enum-discriminant UB risk.
  pub(crate) fn hw_pixel_format(self) -> AVPixelFormat {
    match self {
      Self::VideoToolbox => AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX,
      Self::Vaapi => AVPixelFormat::AV_PIX_FMT_VAAPI,
      Self::Cuda => AVPixelFormat::AV_PIX_FMT_CUDA,
      Self::D3d11va => AVPixelFormat::AV_PIX_FMT_D3D11,
    }
  }
}

/// Probe order for `VideoDecoder::open` on the current target. Hardware
/// backends only, in preference order. Empty for platforms with no known
/// HW backend; on those `open()` returns `ProbeExhausted` immediately.
pub(crate) fn probe_order() -> &'static [Backend] {
  #[cfg(target_vendor = "apple")]
  {
    &[Backend::VideoToolbox]
  }
  #[cfg(target_os = "linux")]
  {
    &[Backend::Vaapi, Backend::Cuda]
  }
  #[cfg(target_os = "windows")]
  {
    &[Backend::D3d11va, Backend::Cuda]
  }
  #[cfg(not(any(target_vendor = "apple", target_os = "linux", target_os = "windows",)))]
  {
    &[]
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn all_backends_have_hwdevice_type_and_pix_fmt() {
    for b in [
      Backend::VideoToolbox,
      Backend::Vaapi,
      Backend::Cuda,
      Backend::D3d11va,
    ] {
      let _ = b.av_hwdevice_type();
      let _ = b.hw_pixel_format();
    }
  }

  #[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "tvos",
    target_os = "visionos",
  ))]
  #[test]
  fn apple_probe_order() {
    assert_eq!(probe_order(), &[Backend::VideoToolbox]);
  }

  #[cfg(target_os = "linux")]
  #[test]
  fn linux_probe_order() {
    assert_eq!(probe_order(), &[Backend::Vaapi, Backend::Cuda]);
  }

  #[cfg(target_os = "windows")]
  #[test]
  fn windows_probe_order() {
    assert_eq!(probe_order(), &[Backend::D3d11va, Backend::Cuda]);
  }
}
