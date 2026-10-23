use derive_more::{IsVariant, TryUnwrap, Unwrap};
use ffmpeg_next::Packet;

use crate::backend::Backend;

/// Crate result alias.
pub type Result<T> = std::result::Result<T, Error>;

/// Errors returned from [`crate::VideoDecoder`].
///
/// `Debug` is derived; the variants that wrap a payload struct
/// (`HwDeviceInitFailed`, `AllBackendsFailed`, `FallbackFailed`)
/// delegate their `Debug` to the payload, which is hand-written
/// where needed because [`ffmpeg_next::Packet`] (carried by
/// `AllBackendsFailed::unconsumed_packets` /
/// `FallbackFailed::unconsumed_packets`) does not derive
/// `Debug`. Those payloads summarize the packet count rather
/// than dumping each packet's fields, which would be both noisy
/// and useless for triage.
///
/// **Open fault taxonomy, so it is `#[non_exhaustive]`.** New ways to
/// fail are discovered — a backend, a ceiling, a corruption a codec
/// learns to report — and a consumer that meets one it has never heard
/// of should take its generic-fault path. That is exactly what the
/// wildcard arm this attribute forces is for. The two status
/// vocabularies opposite it,
/// [`Sent`](mediadecode::Sent) and [`Received`](mediadecode::Received),
/// are exhaustive for the mirror-image reason: their arms are the
/// substrate's fixed state set, and there the wildcard would be dead
/// weight hiding a state a consumer forgot.
#[derive(Debug, thiserror::Error, IsVariant, Unwrap, TryUnwrap)]
#[unwrap(ref, ref_mut)]
#[try_unwrap(ref, ref_mut)]
#[non_exhaustive]
pub enum Error {
  /// An underlying FFmpeg error.
  #[error("ffmpeg error: {0}")]
  Ffmpeg(#[from] ffmpeg_next::Error),

  /// A portable packet could not be rebuilt as an `AVPacket` on its
  /// way into a decoder — see [`crate::boundary::PacketBuildError`].
  #[error(transparent)]
  PacketBuild(#[from] crate::boundary::PacketBuildError),

  /// A stream's codec parameters hold more heap bytes than the
  /// decoder tier will copy — see
  /// [`crate::DEFAULT_MAX_CODEC_PARAMETER_BYTES`].
  ///
  /// The decoder tier has no options object of its own for this, so it
  /// applies the default ceiling. A caller that needs a larger one
  /// opens the parameters through the demux tier, where
  /// [`DemuxLimits`](crate::DemuxLimits) carries the seat.
  #[error(transparent)]
  ParametersTooLarge(#[from] mediaio_ffmpeg::ParametersTooLarge),

  /// A stream's channel layout is not a shape FFmpeg's own helpers can
  /// be given, so the decoder was not opened over it.
  ///
  /// **Structural and permanent, never an allocation failure.**
  /// `avcodec_parameters_to_context` reaches `av_channel_layout_copy`,
  /// whose `memcpy` reads the custom map with no null check of its own,
  /// and FFmpeg's describe and compare helpers compute
  /// `nb_channels - popcount(mask)` — and take an integer square root
  /// of it — without checking either. None of that is a shortage of
  /// memory and none of it will be different on the next attempt, which
  /// is why it does not share the allocation arm.
  ///
  /// See
  /// [`layout_preflight`](crate::channel_layout::layout_preflight) for
  /// the rule, which the demux admission pass and the outbound clone
  /// apply too.
  #[error(transparent)]
  MalformedChannelLayout(#[from] mediaio_ffmpeg::ParametersLayoutShape),

  /// A stream's channel layout declares a custom order without the map
  /// that order requires — the shape `av_channel_layout_copy` would
  /// `memcpy` from null. Structural and permanent, as above.
  #[error(transparent)]
  ChannelMapMissing(#[from] mediaio_ffmpeg::ParametersChannelMap),

  /// `avcodec_find_decoder` returned null for the input codec id. The id
  /// is reported as the raw integer (`AVCodecID` discriminant) — we do not
  /// construct the bindgen `AVCodecID` enum from a runtime value, since
  /// values outside our build's discriminant set would invoke UB.
  #[error("no decoder for codec id {0}")]
  NoCodec(u32),

  /// The CPU frame a hardware->CPU transfer would allocate is larger
  /// than [`FrameLimits::max_frame_bytes`](crate::FrameLimits::max_frame_bytes).
  ///
  /// The hardware road's own seat. `judge_buffer` — the allocator hook
  /// that applies the byte ceiling to aligned dimensions — is **not** a
  /// universal choke point: `ff_get_buffer` calls `hwaccel->alloc_frame`
  /// directly for VideoToolbox h264/hevc/vp9 and never reaches
  /// `get_buffer2` at all, and `av_hwframe_transfer_data` allocates its
  /// CPU destination outside both. This is the seat for that second
  /// road, judged before the transfer rather than after it.
  #[error(transparent)]
  HwTransferTooLarge(#[from] HwTransferTooLarge),

  /// A frame's allocation would have cost more than
  /// [`FrameLimits::max_frame_bytes`](crate::FrameLimits::max_frame_bytes),
  /// so it was refused in the allocator, before the allocation.
  #[error(transparent)]
  FrameBudgetExceeded(#[from] FrameBudgetExceeded),

  /// The stream's **coded** surface is over the frame ceiling, so the
  /// hardware format was declined before its pool could be built.
  ///
  /// The two dimension vocabularies: `max_pixels` is applied by
  /// `ff_set_dimensions` to a stream's *display* dims, and a cropped
  /// stream can display 32x32 out of a 1920x1088 coded surface. What
  /// gets allocated is the coded figure, so it is the one judged here —
  /// and it is judged in **bytes**, priced through the allocator-parity
  /// footprint against the caller's `max_frame_bytes`, because
  /// `max_pixels` carries the caller's logical pixel limit and nothing
  /// about cost.
  #[error(transparent)]
  HwSurfaceTooLarge(#[from] HwSurfaceTooLarge),

  /// The codec does not advertise a hardware configuration matching the
  /// requested backend (via `avcodec_get_hw_config`).
  #[error("codec does not support backend {0:?}")]
  BackendUnsupportedByCodec(Backend),

  /// `av_hwdevice_ctx_create` failed for the requested backend. See
  /// [`HwDeviceInitFailed`] for the payload details. `#[from]` gives
  /// a free `impl From<HwDeviceInitFailed> for Error`, so inner
  /// helpers that return `Result<_, HwDeviceInitFailed>` can be
  /// `?`-propagated into `Error` directly.
  #[error(transparent)]
  HwDeviceInitFailed(#[from] HwDeviceInitFailed),

  /// Auto-probe exhausted every backend in the platform's order. See
  /// [`AllBackendsFailed`] for the payload details (in particular the
  /// `unconsumed_packets` history that callers should replay through
  /// their own software decoder for non-seekable inputs). `#[from]`
  /// gives a free `impl From<AllBackendsFailed> for Error`.
  #[error(transparent)]
  AllBackendsFailed(#[from] AllBackendsFailed),

  /// Surfaced by [`crate::FfmpegVideoStreamDecoder`] when a HW->SW
  /// fallback attempt itself fails. See [`FallbackFailed`] for the
  /// payload details (in particular the rescued `unconsumed_packets`
  /// the HW path had already consumed from the caller). `#[from]`
  /// gives a free `impl From<FallbackFailed> for Error`.
  #[error(transparent)]
  FallbackFailed(#[from] FallbackFailed),
}

/// The crossing's own failure, reported as this error has always
/// reported it: an FFmpeg error in the `Ffmpeg` arm.
///
/// `mediaio-ffmpeg`'s frame allocators and `Frame::empty` answer with a
/// [`CrossingError`](mediaio_ffmpeg::CrossingError); the decoders built
/// here pass it on unchanged in meaning.
impl From<mediaio_ffmpeg::CrossingError> for Error {
  fn from(e: mediaio_ffmpeg::CrossingError) -> Self {
    match e {
      mediaio_ffmpeg::CrossingError::Ffmpeg(e) => Self::Ffmpeg(e),
    }
  }
}

/// Payload for [`Error::HwDeviceInitFailed`].
///
/// `av_hwdevice_ctx_create` failed for the requested backend.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("hardware device init failed for {backend:?}: {source}")]
pub struct HwDeviceInitFailed {
  /// Backend that failed to initialise.
  backend: Backend,
  /// Underlying FFmpeg error.
  source: ffmpeg_next::Error,
}

impl HwDeviceInitFailed {
  /// Constructs a new [`HwDeviceInitFailed`] payload.
  #[inline]
  pub const fn new(backend: Backend, source: ffmpeg_next::Error) -> Self {
    Self { backend, source }
  }
  /// Backend that failed to initialise.
  #[inline]
  pub const fn backend(&self) -> Backend {
    self.backend
  }
  /// Underlying FFmpeg error.
  #[inline]
  pub const fn source(&self) -> &ffmpeg_next::Error {
    &self.source
  }
  /// Consume the payload, returning the backend identifier and the
  /// moved FFmpeg error so callers can take ownership without
  /// cloning.
  #[inline]
  pub fn into_parts(self) -> (Backend, ffmpeg_next::Error) {
    (self.backend, self.source)
  }
}

/// Where in a session's life an [`AllBackendsFailed`] was raised: the
/// probe, before the first picture, always.
///
/// It is the only era a hardware failure is collected in. After the
/// first picture a failure is that picture's own error, reported as it
/// was minted, and nothing is collected or replayed — see
/// [`crate::VideoDecoder`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, IsVariant)]
pub enum FallbackOrigin {
  /// Before the first picture, while backends are on trial. Every failure
  /// advances the probe, and when none is left
  /// [`Error::AllBackendsFailed`] hands back the probe's buffered history
  /// — possibly empty, when the failure landed on the first packet.
  /// [`DecodePath::Auto`](crate::DecodePath::Auto) replays that history
  /// into a software decoder and routes the still-unconsumed current
  /// packet to it; the other paths report it.
  Probe,
}

/// Payload for [`Error::AllBackendsFailed`].
///
/// Auto-probe exhausted every backend in the platform's order. Empty
/// `attempts` means the platform has no hardware backends listed in
/// [`crate::Backend`] for the current `target_os` — callers must
/// fall back to a software decoder of their choice.
///
/// `unconsumed_packets` holds the packets the decoder accepted from
/// the caller before the probe exhausted: copies the probe made of the
/// packets fed via `send_packet`, each payload referenced by its packet
/// alone, so the caller's own packets and these never share bytes. For
/// non-seekable inputs (live streams, pipes, network sources) the
/// caller cannot re-demux from start, so this crate surfaces the
/// buffered history here so the caller can feed those packets directly
/// into a software decoder of their choice. When `AllBackendsFailed` comes
/// from [`crate::VideoDecoder::open`] (no packets were ever sent),
/// this vec is empty.
///
/// `origin` is always [`FallbackOrigin::Probe`]: this is the probe's
/// exhaustion, before the first picture. After it a hardware failure is
/// that picture's own error.
///
/// `Debug` is hand-written: [`ffmpeg_next::Packet`] does not derive
/// `Debug`, so we print `[N packets]` instead of dumping per-packet
/// bytes, which would be both noisy and useless for triage.
#[derive(thiserror::Error)]
#[error("all hardware backends failed; attempts: {attempts:?}")]
pub struct AllBackendsFailed {
  /// Per-backend errors collected during probing, in the order tried.
  attempts: Vec<(Backend, Box<Error>)>,
  /// Packets the decoder consumed from the caller before exhaustion.
  /// Replay them through a software decoder for non-seekable inputs.
  unconsumed_packets: Vec<Packet>,
  /// Where this was raised — the probe, always. See [`FallbackOrigin`].
  origin: FallbackOrigin,
}

impl AllBackendsFailed {
  /// Constructs a probe-era [`AllBackendsFailed`] payload — raised while the
  /// inner decoder's probe is still active. `unconsumed_packets` is the probe's
  /// buffered history (possibly empty if the failure landed on the first
  /// packet). See [`FallbackOrigin::Probe`].
  ///
  /// Not `const fn`: the `Vec` arguments may carry destructors and
  /// the const evaluator can't prove their drop safe for arbitrary
  /// allocator state.
  #[inline]
  pub fn new(attempts: Vec<(Backend, Box<Error>)>, unconsumed_packets: Vec<Packet>) -> Self {
    Self {
      attempts,
      unconsumed_packets,
      origin: FallbackOrigin::Probe,
    }
  }
  /// Per-backend errors collected during probing, in the order tried.
  #[inline]
  pub fn attempts(&self) -> &[(Backend, Box<Error>)] {
    &self.attempts
  }
  /// Where this failure was raised: [`FallbackOrigin::Probe`], always —
  /// see the type's documentation.
  #[inline]
  pub const fn origin(&self) -> FallbackOrigin {
    self.origin
  }
  /// Packets the decoder consumed from the caller before exhaustion.
  /// Replay them through a software decoder for non-seekable inputs.
  #[inline]
  pub fn unconsumed_packets(&self) -> &[Packet] {
    &self.unconsumed_packets
  }
  /// Consume the payload, returning the moved unconsumed packets so
  /// non-seekable callers can replay them through a software decoder
  /// without cloning.
  #[inline]
  pub fn into_unconsumed_packets(self) -> Vec<Packet> {
    self.unconsumed_packets
  }
  /// Consume the payload, returning the moved attempts log and
  /// unconsumed packets.
  #[inline]
  pub fn into_parts(self) -> (Vec<(Backend, Box<Error>)>, Vec<Packet>) {
    (self.attempts, self.unconsumed_packets)
  }
}

impl std::fmt::Debug for AllBackendsFailed {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("AllBackendsFailed")
      .field("attempts", &self.attempts)
      // `Packet` is not `Debug`; print just the count so the error is
      // still useful for triage without dumping per-packet bytes.
      .field(
        "unconsumed_packets",
        &format_args!("[{} packets]", self.unconsumed_packets.len()),
      )
      .field("origin", &self.origin)
      .finish()
  }
}

/// Payload for [`Error::FallbackFailed`].
///
/// Surfaced by [`crate::FfmpegVideoStreamDecoder`] when a HW->SW
/// fallback attempt itself fails — e.g. the SW decoder failed to
/// open, EOF replay returned EAGAIN past the bounded retry, or the
/// per-frame replay queue exceeded its cap. The HW decoder has
/// already consumed `unconsumed_packets` from the caller; we
/// surface them here so non-seekable inputs (pipes, live streams)
/// can drive their own decoder of last resort.
///
/// `Debug` is hand-written for the same reason as
/// [`AllBackendsFailed`]: [`ffmpeg_next::Packet`] does not derive
/// `Debug`.
#[derive(thiserror::Error)]
#[error("HW->SW fallback failed: {source}")]
pub struct FallbackFailed {
  /// Underlying error that aborted the fallback transition.
  source: Box<Error>,
  /// Packets that the HW path had consumed but had not yet decoded
  /// at fallback time. The caller can replay them through a
  /// software decoder of their choice.
  unconsumed_packets: Vec<Packet>,
}

impl FallbackFailed {
  /// Constructs a new [`FallbackFailed`] payload.
  ///
  /// Not `const fn`: the `Vec` argument may carry destructors.
  #[inline]
  pub fn new(source: Box<Error>, unconsumed_packets: Vec<Packet>) -> Self {
    Self {
      source,
      unconsumed_packets,
    }
  }
  /// Underlying error that aborted the fallback transition.
  #[inline]
  pub fn source(&self) -> &Error {
    &self.source
  }
  /// Packets that the HW path had consumed but had not yet decoded
  /// at fallback time.
  #[inline]
  pub fn unconsumed_packets(&self) -> &[Packet] {
    &self.unconsumed_packets
  }
  /// Consume the payload, returning the moved unconsumed packets so
  /// non-seekable callers can replay them through a software decoder
  /// without cloning.
  #[inline]
  pub fn into_unconsumed_packets(self) -> Vec<Packet> {
    self.unconsumed_packets
  }
  /// Consume the payload, returning the moved source error and
  /// unconsumed packets.
  #[inline]
  pub fn into_parts(self) -> (Box<Error>, Vec<Packet>) {
    (self.source, self.unconsumed_packets)
  }
}

impl std::fmt::Debug for FallbackFailed {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("FallbackFailed")
      .field("source", &self.source)
      .field(
        "unconsumed_packets",
        &format_args!("[{} packets]", self.unconsumed_packets.len()),
      )
      .finish()
  }
}

/// Payload for [`Error::HwTransferTooLarge`].
///
/// The CPU-side cost of a hardware->CPU download, priced before it
/// happens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("hw->cpu transfer would allocate {bytes} bytes for one frame, over a ceiling of {limit}")]
pub struct HwTransferTooLarge {
  bytes: usize,
  limit: usize,
}

impl HwTransferTooLarge {
  /// Constructs a `HwTransferTooLarge` payload.
  #[inline]
  pub const fn new(bytes: usize, limit: usize) -> Self {
    Self { bytes, limit }
  }
  /// Bytes the destination frame would have cost.
  #[inline]
  pub const fn bytes(&self) -> usize {
    self.bytes
  }
  /// The ceiling in force.
  #[inline]
  pub const fn limit(&self) -> usize {
    self.limit
  }
}

/// Payload for [`Error::HwSurfaceTooLarge`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error(
  "the hardware surface pool would cost {bytes} bytes, over a ceiling of {limit}; \
   the hardware format was declined before the pool was built"
)]
pub struct HwSurfaceTooLarge {
  bytes: i64,
  limit: i64,
}

impl HwSurfaceTooLarge {
  /// Constructs a `HwSurfaceTooLarge` payload.
  #[inline]
  pub const fn new(bytes: i64, limit: i64) -> Self {
    Self { bytes, limit }
  }
  /// What the pool would have cost, priced through the same
  /// allocator-parity footprint every other judge uses.
  #[inline]
  pub const fn bytes(&self) -> i64 {
    self.bytes
  }
  /// The ceiling in force.
  #[inline]
  pub const fn limit(&self) -> i64 {
    self.limit
  }
}

/// Which kind of frame a [`FrameBudgetExceeded`] refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameMedium {
  /// A picture.
  Video,
  /// An audio frame.
  Audio,
}

impl core::fmt::Display for FrameMedium {
  fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
    match self {
      Self::Video => f.write_str("picture"),
      Self::Audio => f.write_str("audio frame"),
    }
  }
}

/// Payload for [`Error::FrameBudgetExceeded`].
///
/// The allocator judge refused a frame whose real cost — priced through
/// the allocator-parity footprint, before `avcodec_default_get_buffer2`
/// ran — exceeds the caller's ceiling.
///
/// # Why this has a name
///
/// A `get_buffer2` callback can only answer libavcodec with an errno,
/// and `AVERROR(EINVAL)` is what libavcodec itself reports for corrupt
/// input. Without a name, a caller could not tell "this file is broken"
/// from "your budget refused this frame" — and only one of those is
/// worth retrying with a larger ceiling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the {medium} would allocate {bytes} bytes, over a ceiling of {limit}")]
pub struct FrameBudgetExceeded {
  bytes: u64,
  limit: u64,
  medium: FrameMedium,
}

impl FrameBudgetExceeded {
  /// Constructs a `FrameBudgetExceeded` payload.
  #[inline]
  pub const fn new(bytes: u64, limit: u64, medium: FrameMedium) -> Self {
    Self {
      bytes,
      limit,
      medium,
    }
  }
  /// What the frame would have cost.
  #[inline]
  pub const fn bytes(&self) -> u64 {
    self.bytes
  }
  /// The ceiling in force.
  #[inline]
  pub const fn limit(&self) -> u64 {
    self.limit
  }
  /// Whether the frame was a picture or audio.
  #[inline]
  pub const fn medium(&self) -> FrameMedium {
    self.medium
  }
}
