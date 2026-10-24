//! [`VideoStreamDecoderOpen`]: everything a video stream decoder's
//! `open_from` takes beside the stream's codec parameters, its timebase
//! and its limits.

use crate::DecoderOptions;

use super::DecodePath;

#[cfg(doc)]
use super::CarrierVideoStreamDecoder;

/// How to open a video stream decoder: every optional of a
/// [`CarrierVideoStreamDecoder`]'s construction in one value.
/// [`CarrierVideoStreamDecoder::open_from`] takes it beside the three
/// things every session is opened with: the stream's codec parameters,
/// its timebase, and the [`DecoderLimits`](crate::limits::DecoderLimits).
///
/// [`Self::new`] is [`CarrierVideoStreamDecoder::open`]'s request:
///
/// - [`Self::path`]: [`DecodePath::Auto`] here, which probes the
///   platform's hardware backends and falls back to software. The other
///   arms keep the session to hardware, to one backend, or to software;
///   see [`DecodePath`].
/// - [`Self::options`]: how the session decodes (libavcodec's defaults
///   here). They are written into every `AVCodecContext` the session
///   opens: each hardware candidate, the software decoder, and a
///   fallback's.
///
/// Each optional has a getter, a consuming `with_*` and an in-place
/// `set_*`. [`CarrierVideoStreamDecoder::open_as`]`(parameters, time_base,
/// limits, path)` opens `VideoStreamDecoderOpen::new().with_path(path)`.
///
/// This is a sibling of [`VideoDecoderOpen`](crate::VideoDecoderOpen), not
/// that type. A session can fall back to software where the standalone
/// decoder cannot, and a session always declares its packet timebase.
///
/// Marked `#[non_exhaustive]`: a later optional joins as a field with
/// its own methods, not as another constructor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct VideoStreamDecoderOpen {
  path: DecodePath,
  options: DecoderOptions,
}

impl VideoStreamDecoderOpen {
  /// [`CarrierVideoStreamDecoder::open`]'s request: [`DecodePath::Auto`]
  /// and libavcodec's options.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new() -> Self {
    Self {
      path: DecodePath::Auto,
      options: DecoderOptions::new(),
    }
  }

  /// Which decoder the session may run on.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn path(&self) -> DecodePath {
    self.path
  }

  /// How the session decodes.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn options(&self) -> DecoderOptions {
    self.options
  }

  /// Sets [`Self::path`] (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_path(mut self, value: DecodePath) -> Self {
    self.path = value;
    self
  }

  /// Sets [`Self::options`] (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_options(mut self, value: DecoderOptions) -> Self {
    self.options = value;
    self
  }

  /// Sets [`Self::path`] in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_path(&mut self, value: DecodePath) -> &mut Self {
    self.path = value;
    self
  }

  /// Sets [`Self::options`] in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_options(&mut self, value: DecoderOptions) -> &mut Self {
    self.options = value;
    self
  }
}

impl Default for VideoStreamDecoderOpen {
  /// [`Self::new`].
  #[inline]
  fn default() -> Self {
    Self::new()
  }
}
