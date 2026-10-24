use derive_more::{IsVariant, TryUnwrap, Unwrap};
use ffmpeg_next::Packet;

use crate::backend::Backend;

/// Crate result alias.
pub type Result<T> = std::result::Result<T, Error>;

/// Errors returned from [`crate::VideoDecoder`].
///
/// `Debug` is derived; the variants that wrap a payload struct
/// (`HwDeviceInitFailed`, `ProbeExhausted`, `FallbackFailed`)
/// delegate their `Debug` to the payload, which is hand-written
/// where needed because [`ffmpeg_next::Packet`] (carried by
/// `ProbeExhausted::unconsumed_packets` /
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
  /// A picture's download would cost more than the caller's frame limit:
  /// the hardware road's own ceiling. `judge_buffer` — the allocator hook
  /// that applies the byte ceiling to aligned dimensions — is **not** a
  /// universal choke point: `ff_get_buffer` calls `hwaccel->alloc_frame`
  /// directly for VideoToolbox h264/hevc/vp9 and never reaches
  /// `get_buffer2` at all, and the CPU destination of a download is
  /// allocated outside both. So that second road is judged here, before the
  /// destination is allocated. The picture is dropped, named by its pts, and
  /// nothing is charged.
  #[error(transparent)]
  HwTransferTooLarge(#[from] HwTransferTooLarge),

  /// A frame's allocation would have cost more than
  /// [`FrameLimits::max_frame_bytes`](crate::FrameLimits::max_frame_bytes),
  /// so it was refused in the allocator, before the allocation.
  #[error(transparent)]
  FrameBudgetExceeded(#[from] FrameBudgetExceeded),

  /// A committed decoder's picture whose download would choose a CPU layout
  /// a [`Frame`](crate::Frame) cannot carry, or for which the backend lists
  /// no layout at all: a decoder past its probe, or a backend pinned alone.
  /// On trial, either is the candidate's unfitness for the stream, and the
  /// probe leaves it. See [`DownloadFormatUnsupported`].
  #[error(transparent)]
  DownloadFormatUnsupported(#[from] DownloadFormatUnsupported),

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

  /// **The hardware probe has no candidate left**: each backend it tried
  /// failed by its own answer or was unfit for the stream, and none
  /// delivered a picture. See [`ProbeExhausted`] for the payload details
  /// (in particular the `unconsumed_packets` history that callers should
  /// replay through their own software decoder for non-seekable inputs).
  /// `#[from]` gives a free `impl From<ProbeExhausted> for Error`.
  ///
  /// It was `AllBackendsFailed` before 0.21.0, renamed because an attempt
  /// may have been unfit rather than failed.
  #[error(transparent)]
  ProbeExhausted(#[from] ProbeExhausted),

  /// **A decoder ended, because libavcodec left unknown whether it took a
  /// packet.** `avcodec_send_packet` answers `AVERROR(ENOMEM)` both when it
  /// cannot reference a packet, which it then has not taken, and when the
  /// decode it runs after taking one fails for memory
  /// (`libavcodec/decode.c` 748–750 and 754–757 in FFmpeg 9.0.1), and
  /// nothing public tells the two apart. Where the packet was one only this
  /// decoder held, it can go on neither as if it had been taken nor as if it
  /// had not without losing it or feeding it twice, so it ends. See
  /// [`AmbiguousSend`] for the payload: every packet it may not have taken,
  /// in order, for a decoder that starts from them. `#[from]` gives a free
  /// `impl From<AmbiguousSend> for Error`.
  #[error(transparent)]
  AmbiguousSend(#[from] AmbiguousSend),

  /// **The decoder is spent, and takes nothing more**: it answered
  /// [`Error::ProbeExhausted`] or [`Error::AmbiguousSend`] before, and no
  /// decoder lives behind it. Every send, end and receive after answers this,
  /// naming which, and a flush does not revive it: going on means building a
  /// new decoder, from what that error handed back. See [`Spent`].
  #[error("{0}")]
  Spent(Spent),

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

/// Where in a session's life an [`ProbeExhausted`] was raised: the
/// probe, before the first picture, always.
///
/// It is the only era a hardware failure is collected in. After the
/// first picture a failure is that picture's own error, reported as it
/// was minted, and nothing is collected or replayed — see
/// [`crate::VideoDecoder`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, IsVariant)]
pub enum FallbackOrigin {
  /// Before the first picture, while backends are on trial. Every failure
  /// that is a backend's own advances the probe, and when none is left
  /// [`Error::ProbeExhausted`] hands back the probe's buffered history
  /// — possibly empty, when the failure landed on the first packet.
  /// [`DecodePath::Auto`](crate::DecodePath::Auto) replays that history
  /// into a software decoder and routes the still-unconsumed current
  /// packet to it; the other paths report it.
  Probe,
}

/// Payload for [`Error::ProbeExhausted`].
///
/// The probe tried every backend in its order, and none delivered a
/// picture. **Each attempt is a failure or an unfitness**, told apart in
/// [`Self::attempts`]:
///
/// - **A failure** is the backend's own answer, to its open or to one of its
///   own calls (a send, a receive, a download), as the error it was.
/// - **An unfitness** is a backend that decoded a picture whose download
///   would choose a CPU layout a [`Frame`](crate::Frame) cannot carry, or
///   for which it lists no layout at all:
///   [`Error::DownloadFormatUnsupported`], naming the layout, or
///   `AV_PIX_FMT_NONE` where it lists none. The backend failed nothing, and
///   no errno is made for it.
///
/// The probe ends in this error whatever its last attempt was, with every
/// attempt and every packet of its history, on every road it runs on: the
/// standalone [`VideoDecoder`](crate::VideoDecoder)'s, whose caller can
/// replay the packets into a software decoder of its own, and the stream
/// decoder's. There [`DecodePath::Auto`](crate::DecodePath::Auto) replays
/// them into software, and [`DecodePath::AnyHardware`](crate::DecodePath::AnyHardware)
/// hands them back.
///
/// Empty `attempts` means the platform has no hardware backends listed in
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
/// into a software decoder of their choice. When `ProbeExhausted` comes
/// from [`crate::VideoDecoder::open`] (no packets were ever sent),
/// this vec is empty.
///
/// `origin` is always [`FallbackOrigin::Probe`]: this is the probe's
/// exhaustion, before the first picture. After it a hardware failure is
/// that picture's own error.
///
/// **Raised at run time, it leaves the decoder spent**, terminally: the
/// candidate it left was torn down before it was raised, no decoder lives
/// behind it, and every send, end and receive after answers
/// [`Error::Spent`] with [`Spent::ProbeExhausted`], a flush included. The
/// stream decoder under [`DecodePath::Auto`](crate::DecodePath::Auto) goes
/// on in software from the packets it hands back; any other caller opens a
/// new decoder.
///
/// **Only backends' answers and unfitness raise it, never a failure of this
/// crate's own.** An allocation or a copy this crate makes — the parameters
/// a backend is built from, its codec context, a picture's properties — and
/// the caller's limits on what a picture costs are reported as the errors
/// they are, and charge no backend. A backend commits at its first
/// delivered picture: downloaded in a layout a `Frame` carries, its
/// properties copied. A picture one of those refuses is no delivery, and
/// the backend stays on trial. A packet the probe's history cannot keep — past its
/// ceiling of 256 packets or 64 MiB, of more side-data entries than it
/// copies, or one whose copy cannot be made — ends the trial on the
/// candidate's answer:
/// the candidate that takes it is committed, and one that refuses it fails
/// on that refusal as on any packet. After such a commit a failure is the
/// committed decoder's own error, reported as it was minted, and comes with
/// no replay history: the history could not keep the packet, so none could
/// have been replayed whole.
///
/// It was `AllBackendsFailed` before 0.21.0, renamed because an attempt
/// may have been unfit rather than failed.
///
/// `Debug` is hand-written: [`ffmpeg_next::Packet`] does not derive
/// `Debug`, so we print `[N packets]` instead of dumping per-packet
/// bytes, which would be both noisy and useless for triage.
#[derive(thiserror::Error)]
#[error(
  "no hardware backend delivered a picture, each one failed or unfit for the stream; \
   attempts: {attempts:?}"
)]
pub struct ProbeExhausted {
  /// Each backend's attempt, in the order tried: its failure, or its
  /// unfitness for the stream.
  attempts: Vec<(Backend, Box<Error>)>,
  /// Packets the decoder consumed from the caller before exhaustion.
  /// Replay them through a software decoder for non-seekable inputs.
  unconsumed_packets: Vec<Packet>,
  /// Where this was raised — the probe, always. See [`FallbackOrigin`].
  origin: FallbackOrigin,
}

impl ProbeExhausted {
  /// Constructs a probe-era [`ProbeExhausted`] payload — raised while the
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
  /// Each backend's attempt, in the order tried: its own answer, as the
  /// error it was, or its unfitness for the stream,
  /// [`Error::DownloadFormatUnsupported`] naming the layout the download of
  /// the picture it decoded would choose, or `AV_PIX_FMT_NONE` where it lists
  /// none. An attempt may have been unfit rather than failed.
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

impl std::fmt::Debug for ProbeExhausted {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("ProbeExhausted")
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

/// Payload for [`Error::AmbiguousSend`]: a hardware decoder that ended
/// because libavcodec left unknown whether it took a packet, and what it
/// hands back.
///
/// # When
///
/// - **After the commit, feeding the backlog**: the packets a probe's replay
///   had not yet given the backend it committed, at its first picture, fed
///   before anything new. They are this decoder's only copies, so the one
///   whose send answered `ENOMEM` can be neither dropped, which would lose
///   it if it was not taken, nor sent again, which would feed it twice if it
///   was.
/// - **On trial**, where a candidate's send answered `ENOMEM` with a
///   refusal of this crate's own latched beside it, the frame budget: the
///   candidate can be neither kept, not knowing what it holds, nor charged
///   with this crate's refusal and left for the next backend. Where nothing
///   of this crate's is latched, the probe leaves the candidate instead, its
///   answer the `ENOMEM` libavcodec returned, and the next backend is
///   replayed the history whole.
///
/// # What comes back
///
/// [`Self::residual`] is every packet the decoder may not have taken, in
/// the order sent: on trial the whole history, since no picture was
/// delivered and a decoder that starts again starts from the first;
/// committed, the backlog, its first the packet whose send was ambiguous.
/// Each is a copy the probe made, its payload referenced by its packet
/// alone: the decoder let every reference go before it ended. Where the
/// caller had already signalled the end, [`Self::end_follows`] says so, and
/// a decoder that starts from the residual is sent the end after it. **A
/// packet the caller was sending when the decoder ended is not in it**: it
/// was not taken, and is the caller's to send after the residual.
///
/// A committed decoder's residual follows pictures already delivered, so a
/// decoder that starts from it starts mid-stream, and the pictures the
/// ended decoder still held from packets it took went with it. Whether to
/// go on, and from where, is the caller's: a seekable input is better
/// sought back to a key frame.
///
/// **After it the decoder is spent**, terminally: no decoder lives behind
/// it, and every send, end and receive answers [`Error::Spent`] with
/// [`Spent::AmbiguousSend`], a flush included. Going on means a new
/// decoder.
///
/// `Debug` is hand-written, as for [`ProbeExhausted`].
#[derive(thiserror::Error)]
#[error(
  "libavcodec left unknown whether the {backend:?} decoder took a packet ({reason}); it ended, \
   handing back {} packets",
  .residual.len()
)]
pub struct AmbiguousSend {
  /// The backend whose decoder ended.
  backend: Backend,
  /// The send's verdict: `ENOMEM` as libavcodec returned it, or the refusal
  /// this crate latched beside it.
  reason: Box<Error>,
  /// Every packet the decoder may not have taken, in order.
  residual: Vec<Packet>,
  /// Whether the caller's end follows the residual.
  end_follows: bool,
}

impl AmbiguousSend {
  /// Constructs an [`AmbiguousSend`] payload.
  ///
  /// Not `const fn`: the `Vec` argument may carry destructors.
  #[inline]
  pub fn new(
    backend: Backend,
    reason: Box<Error>,
    residual: Vec<Packet>,
    end_follows: bool,
  ) -> Self {
    Self {
      backend,
      reason,
      residual,
      end_follows,
    }
  }
  /// The backend whose decoder ended.
  #[inline]
  pub const fn backend(&self) -> Backend {
    self.backend
  }
  /// The send's verdict: `ENOMEM` as libavcodec returned it, or the refusal
  /// this crate latched beside it, by name.
  #[inline]
  pub fn reason(&self) -> &Error {
    &self.reason
  }
  /// Every packet the decoder may not have taken, in the order sent.
  #[inline]
  pub fn residual(&self) -> &[Packet] {
    &self.residual
  }
  /// Whether the caller had signalled the end, which follows the residual.
  #[inline]
  pub const fn end_follows(&self) -> bool {
    self.end_follows
  }
  /// Consume the payload, returning the moved residual.
  #[inline]
  pub fn into_residual(self) -> Vec<Packet> {
    self.residual
  }
  /// Consume the payload, returning the backend, the verdict, the residual
  /// and whether the end follows it.
  #[inline]
  pub fn into_parts(self) -> (Backend, Box<Error>, Vec<Packet>, bool) {
    (self.backend, self.reason, self.residual, self.end_follows)
  }
}

impl std::fmt::Debug for AmbiguousSend {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("AmbiguousSend")
      .field("backend", &self.backend)
      .field("reason", &self.reason)
      // `Packet` is not `Debug`; the count is what triage needs.
      .field(
        "residual",
        &format_args!("[{} packets]", self.residual.len()),
      )
      .field("end_follows", &self.end_follows)
      .finish()
  }
}

/// Payload for [`Error::Spent`]: why a hardware decoder takes nothing more.
///
/// A decoder is spent at the error that ended it, which handed back what it
/// held; no decoder lives behind it after. So every entry — a send, the end,
/// a receive — answers [`Error::Spent`] naming that error, and a flush,
/// which has no decoder to flush, leaves it spent. Reuse means a new
/// decoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, IsVariant)]
pub enum Spent {
  /// The probe ran out of backends: it answered [`Error::ProbeExhausted`],
  /// with every attempt and every packet of its history.
  ProbeExhausted,
  /// libavcodec left unknown whether the decoder took a packet only it
  /// held: it answered [`Error::AmbiguousSend`], with every packet it may
  /// not have taken.
  AmbiguousSend,
  /// The stream decoder's hardware probe was exhausted, and the software
  /// decoder [`DecodePath::Auto`](crate::DecodePath::Auto) falls back to
  /// could not take the stream: it answered [`Error::FallbackFailed`], with
  /// every packet it rescued.
  FallbackFailed,
}

impl core::fmt::Display for Spent {
  fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
    match self {
      Self::ProbeExhausted => f.write_str(
        "the decoder is spent: its probe was exhausted, and it takes nothing more; open a new one",
      ),
      Self::AmbiguousSend => f.write_str(
        "the decoder is spent: it ended on an ambiguous send, and takes nothing more; open a new one",
      ),
      Self::FallbackFailed => f.write_str(
        "the decoder is spent: its hardware probe was exhausted and the software fallback failed, \
         and it takes nothing more; open a new one",
      ),
    }
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
/// [`ProbeExhausted`]: [`ffmpeg_next::Packet`] does not derive
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
/// happens. **The picture is dropped**, deliberately: the limit is the
/// caller's, so no retry can deliver it, and nothing is charged to the
/// backend. It is named by its presentation timestamp ([`Self::pts`]), so it
/// is never lost unseen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HwTransferTooLarge {
  bytes: usize,
  limit: usize,
  pts: Option<i64>,
}

impl HwTransferTooLarge {
  /// Constructs a `HwTransferTooLarge` payload, naming no picture.
  #[inline]
  pub const fn new(bytes: usize, limit: usize) -> Self {
    Self {
      bytes,
      limit,
      pts: None,
    }
  }
  /// The payload, naming the dropped picture by its presentation timestamp,
  /// in the stream's time base: `None` where the picture carried none.
  #[inline]
  #[must_use]
  pub const fn with_pts(mut self, pts: Option<i64>) -> Self {
    self.pts = pts;
    self
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
  /// The dropped picture's presentation timestamp, in the stream's time
  /// base: `None` where it carried none.
  #[inline]
  pub const fn pts(&self) -> Option<i64> {
    self.pts
  }
}

impl core::fmt::Display for HwTransferTooLarge {
  fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
    write!(
      f,
      "hw->cpu transfer would allocate {} bytes for one frame, over a ceiling of {}; the \
       picture is dropped",
      self.bytes, self.limit
    )?;
    match self.pts {
      Some(pts) => write!(f, " (pts {pts})"),
      None => f.write_str(" (no pts)"),
    }
  }
}

impl std::error::Error for HwTransferTooLarge {}

/// Payload for [`Error::DownloadFormatUnsupported`]: a picture's hardware
/// download would choose a CPU layout [`Frame`](crate::Frame) cannot carry,
/// and this is the raw `AVPixelFormat` it would choose — or the backend lists
/// no layout at all to download into, and this is `AV_PIX_FMT_NONE`.
///
/// The layout is the first of the formats the backend's frames context
/// offers (`av_hwframe_transfer_get_formats`), as libavutil's own
/// allocating road picks it, and this crate keeps that choice, reading it
/// before any transfer and allocating the destination in it itself. A
/// conversion a driver makes to another layout can lose samples (4:2:2 to
/// 4:2:0), and some backends offer their surface's own layout alone.
///
/// **A backend that lists nothing** — VAAPI where its device lists no
/// format, D3D11VA for an opaque surface — offers no CPU layout to download
/// into. That is its unfitness for this caller, no failure: it is this
/// payload with [`Self::format`] `AV_PIX_FMT_NONE` (`-1`), and no errno is
/// made for it.
///
/// **Such a picture is no delivery**, and a candidate commits at its first
/// delivered picture. So the layout means one of two things:
///
/// - **On trial**, the candidate is unfit for the stream. The probe records
///   the unfitness against it — this payload, naming the layout, never an
///   errno — and leaves it as it leaves a failed candidate, the history
///   replayed whole into the next one. With no candidate left, the probe
///   ends in [`Error::ProbeExhausted`], every attempt and every packet with
///   it, and under [`DecodePath::Auto`](crate::DecodePath::Auto) software
///   takes the stream.
/// - **From a committed decoder**, it is that decoder's error, for each such
///   picture: a mid-stream reconfiguration to a layout a `Frame` cannot
///   carry, a candidate committed on a packet the probe could not keep, or a
///   backend pinned alone, which no probe ever held.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DownloadFormatUnsupported {
  format: i32,
}

impl DownloadFormatUnsupported {
  /// Constructs a `DownloadFormatUnsupported` payload for the raw
  /// `AVPixelFormat` the download would choose, `AV_PIX_FMT_NONE` where the
  /// backend lists none.
  #[inline]
  pub const fn new(format: i32) -> Self {
    Self { format }
  }
  /// The raw `AVPixelFormat` the download would choose, as the integer
  /// FFmpeg listed: never formed into the bindings' enum, which a value they
  /// do not name would make undefined behaviour. `AV_PIX_FMT_NONE` (`-1`)
  /// where the backend lists no layout at all.
  #[inline]
  pub const fn format(&self) -> i32 {
    self.format
  }
}

impl core::fmt::Display for DownloadFormatUnsupported {
  fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
    if self.format == ffmpeg_next::ffi::AVPixelFormat::AV_PIX_FMT_NONE as i32 {
      f.write_str(
        "the hardware backend lists no CPU pixel format to download into (AV_PIX_FMT_NONE)",
      )
    } else {
      write!(
        f,
        "the hardware download would choose pixel format {}, which a Frame cannot carry",
        self.format
      )
    }
  }
}

impl std::error::Error for DownloadFormatUnsupported {}

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
