//! [`VideoDecoderOpen`]: everything [`VideoDecoder::open_from`] takes
//! beside the stream's codec parameters.

use crate::{Backend, DecoderOptions, limits::DecoderLimits};

#[cfg(doc)]
use super::VideoDecoder;

/// How to open a standalone [`VideoDecoder`]: every optional of its
/// construction in one value, which [`VideoDecoder::open_from`] takes
/// beside the stream's codec parameters.
///
/// [`Self::new`] is [`VideoDecoder::open`]'s request:
///
/// - [`Self::backend`]: `None`, which probes the platform's hardware
///   backends in order. `Some` pins one backend, with no probe and no
///   fallback, as [`VideoDecoder::open_with`] does.
/// - [`Self::limits`]: what the decoder may spend
///   ([`DecoderLimits::new`] here). It is fixed at open;
///   [`VideoDecoder::open_with_frame_limits`] says why.
/// - [`Self::timebase`]: the stream's packet timebase. `None` here
///   declares none; see [`VideoDecoder::open_timed`].
/// - [`Self::options`]: how the decoder decodes (libavcodec's defaults
///   here). They are written into every `AVCodecContext` the decoder
///   opens, including a later probe advance's.
///
/// Each optional has a getter, a consuming `with_*` and an in-place
/// `set_*`. Each of the eight `open*` constructors opens one such request.
/// For example, [`VideoDecoder::open_with_limits_timed`]`(parameters,
/// backend, limits, timebase)` opens
/// `VideoDecoderOpen::new().with_backend(Some(backend)).with_limits(limits).with_timebase(Some(timebase))`.
///
/// Marked `#[non_exhaustive]`: a later optional joins as a field with
/// its own methods, not as another constructor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct VideoDecoderOpen {
  backend: Option<Backend>,
  limits: DecoderLimits,
  timebase: Option<mediadecode::Timebase>,
  options: DecoderOptions,
}

impl VideoDecoderOpen {
  /// [`VideoDecoder::open`]'s request: the probe, the default limits, no
  /// packet timebase, and libavcodec's options.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new() -> Self {
    Self {
      backend: None,
      limits: DecoderLimits::new(),
      timebase: None,
      options: DecoderOptions::new(),
    }
  }

  /// The pinned backend, or `None` for the probe across the platform's
  /// hardware backends.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn backend(&self) -> Option<Backend> {
    self.backend
  }

  /// What the decoder may spend.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn limits(&self) -> DecoderLimits {
    self.limits
  }

  /// The stream's packet timebase, or `None` for an untimed decoder.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn timebase(&self) -> Option<mediadecode::Timebase> {
    self.timebase
  }

  /// How the decoder decodes.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn options(&self) -> DecoderOptions {
    self.options
  }

  /// Sets [`Self::backend`] (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_backend(mut self, value: Option<Backend>) -> Self {
    self.backend = value;
    self
  }

  /// Sets [`Self::limits`] (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_limits(mut self, value: DecoderLimits) -> Self {
    self.limits = value;
    self
  }

  /// Sets [`Self::timebase`] (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_timebase(mut self, value: Option<mediadecode::Timebase>) -> Self {
    self.timebase = value;
    self
  }

  /// Sets [`Self::options`] (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_options(mut self, value: DecoderOptions) -> Self {
    self.options = value;
    self
  }

  /// Sets [`Self::backend`] in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_backend(&mut self, value: Option<Backend>) -> &mut Self {
    self.backend = value;
    self
  }

  /// Sets [`Self::limits`] in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_limits(&mut self, value: DecoderLimits) -> &mut Self {
    self.limits = value;
    self
  }

  /// Sets [`Self::timebase`] in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_timebase(&mut self, value: Option<mediadecode::Timebase>) -> &mut Self {
    self.timebase = value;
    self
  }

  /// Sets [`Self::options`] in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_options(&mut self, value: DecoderOptions) -> &mut Self {
    self.options = value;
    self
  }
}

impl Default for VideoDecoderOpen {
  /// [`Self::new`].
  #[inline]
  fn default() -> Self {
    Self::new()
  }
}
