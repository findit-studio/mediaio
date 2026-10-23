use std::{
  collections::VecDeque,
  mem::ManuallyDrop,
  ptr::{self, NonNull},
};

use ffmpeg_next::{
  Codec, Packet, Rational,
  codec::{
    self,
    Context,
    // Bring the `Ref` trait into scope so `Packet::as_ptr` resolves. It
    // is aliased to avoid shadowing any future `Ref` type we might add.
    // The `Mut` twin left with the packet copier, which now lives in
    // `mediaio-ffmpeg`'s `boundary`.
    packet::Ref as PacketRef,
  },
  ffi::{
    AVBufferRef, AVFrame, AVHWFramesContext, AVMediaType, av_buffer_ref, av_buffer_unref,
    av_frame_unref, av_hwdevice_ctx_create, av_hwframe_transfer_data, avcodec_alloc_context3,
    avcodec_free_context, avcodec_parameters_to_context,
  },
  frame,
};

use mediadecode::{Received, Sent, decoder::ScaledOutputCapability};

use crate::{
  backend::{self, Backend},
  error::{AllBackendsFailed, Error, HwDeviceInitFailed, Result},
  ffi::{CallbackState, codec_supports_hwaccel, get_hw_format},
  frame::Frame,
};

mod open;

pub use open::VideoDecoderOpen;

/// Hardware-accelerated video decoder.
///
/// Hardware-only — there is no software fallback inside this crate. If
/// every hardware backend in the platform's probe order fails to open,
/// `open` returns [`Error::AllBackendsFailed`] and the caller is
/// responsible for falling back to a software decoder of their choice
/// (e.g. `ffmpeg::decoder::Video`).
///
/// Mirrors `ffmpeg::decoder::Video`'s `send_packet`/`receive_frame` interface.
/// Decoded frames are returned through [`crate::Frame`], a CPU-side wrapper
/// whose accessors avoid the `AVPixelFormat`-enum UB that an unvalidated read
/// of FFmpeg's raw integer pixel formats can trigger.
///
/// `open` does a true probe: each backend opens with a strict `get_format`
/// callback. On a backend's own failure (see below) the decoder is torn down
/// and the next backend in probe order is tried with the history of packets
/// seen so far. **The history stays the probe's own until a candidate
/// commits.** A candidate is sent the history's packets themselves, each by
/// reference, as far as it takes them before it has output for the caller,
/// and the rest as the caller drains it. So a failing backend in the middle of
/// the order does not strand the caller without history, no packet is copied
/// a second time, and no frame is held back. Once the first frame is
/// delivered the probe collapses and subsequent calls go straight to the
/// active (committed) backend.
///
/// # What the probe weighs
///
/// **A backend on trial is judged by its own answers, and by nothing
/// else.** No skip level, codec, packet flag or payload is read to guess
/// what it should have given:
///
/// | the candidate's answer | the probe |
/// |---|---|
/// | a picture | commits it |
/// | its own error: its opening, a send, a receive or a download failing, or its `get_format` declining the surface pool it declared ([`Error::HwSurfaceTooLarge`]) | fails it, and the next backend is tried with the whole history |
/// | the end taken, then libavcodec's `AVERROR_EOF` with no refusal latched | a successful empty completion: commits it, and the stream has ended |
/// | `EAGAIN` after the end | fails it: libavcodec answers a decoder drained to its end with `AVERROR_EOF` (`libavcodec/decode.c` 446–451, 489–505, 655–656 in FFmpeg 9.0.1), so a candidate that asks for input there has broken the drain |
/// | a packet the history cannot keep: past the probe's ceiling (256 packets or 64 MiB), of more side-data entries than the probe copies (64), or one whose copy cannot be made | the trial's last: sent with the history whole, it commits the candidate once taken, and fails it as above if refused |
///
/// The last row's are **the wrapper's own limits, never a backend's
/// answer**: none ends a trial without the candidate's word, and none
/// answers [`Error::AllBackendsFailed`], so none is the stream decoder's
/// cue for software. Once the candidate that took such a packet is
/// committed, a failure is that decoder's own error, reported as it was
/// minted, with no history to replay: the history could not keep the
/// packet, so no replay could have been whole.
///
/// **Nothing of this crate's own is a backend's answer, by type.** The
/// probe records and advances on the candidate's answers alone, and only
/// the candidate's own calls make one. Every failure of this crate's own is
/// reported to the caller as the error it is: an allocation or a copy it
/// makes (the parameters a candidate is built from, a codec context, a
/// picture's properties), and the caller's limits on what a picture costs
/// ([`Error::FrameBudgetExceeded`], [`Error::HwTransferTooLarge`]). None
/// ends a trial, advances to another backend or enters
/// [`Error::AllBackendsFailed`]: the backends still to try stay in the
/// order, and the history stays whole. **A picture commits its candidate
/// before any limit of this crate's is applied to it**, so a limit that
/// refuses that picture is the committed decoder's error, never the
/// candidate's.
///
/// A skip level is the codec's to honour, not the probe's to assume.
/// FFmpeg 9.0.1's VP9 decoder reads no `skip_frame` at all, the native AV1
/// decoder reads `NonIntra`, `NonKey` and `All` alone (`av1dec.c`
/// 1166–1171), and `libdav1d` decodes key pictures under `All`
/// (`libdav1d.c` 236–243). A packet that decodes to no picture is no
/// failure either: a VP9 or AV1 hidden frame (`vp9.c` 1818–1821), or
/// edit-list pre-roll flagged `AV_PKT_FLAG_DISCARD`, whose frame libavcodec
/// drops (`decode.c` 464–466). A stream of them ends in a clean, empty
/// completion. This decoder drops nothing itself, so no shortcut stands on
/// what it would drop: every skip is libavcodec's, enforced by the codec.
///
/// # After commit
///
/// A committed backend reports per picture, and nothing is classified or
/// remembered. Every failure is that picture's own error, reported as it
/// was minted — a refusal this crate made by its name, anything else as
/// libavcodec's errno — and the next call reaches libavcodec. That is all
/// this decoder promises about the next call: whether the hardware
/// session behind it recovers is FFmpeg's.
///
/// FFmpeg has no reliable signal that a hardware session is gone, so this
/// decoder never decides that one is. In FFmpeg 9.0.1 `AVERROR_EXTERNAL`
/// also answers a single picture: VideoToolbox's frame post-processing
/// and buffer creation (`libavcodec/videotoolbox.c` 126–129 and 556–559),
/// and every failed CUDA call under NVDEC (`libavutil/cuda_check.h` 52),
/// the decode of one picture included (`nvdec.c` 662). `ENOSYS` is
/// NVDEC's answer to one HEVC picture its tables cannot describe
/// (`nvdec_hevc.c` 200–227). A hardware format `ff_get_format` withdraws
/// says only that the hwaccel's setup failed, not why (`decode.c`
/// 1341–1343 and 1348–1357).
///
/// Nor does FFmpeg always recover a session. A malfunction or an
/// invalidated session marks a VideoToolbox session for a restart
/// (`videotoolbox.c` 1076–1077), as does an H.264 SPS whose profile/level
/// bytes differ (446–450), and the next picture stops the session and
/// starts a new one (1062–1068). A restart that fails answers
/// `AVERROR_EXTERNAL` (1066–1067) and leaves no session behind, because
/// the stop released it (518–522): VideoToolbox answers every picture
/// after it with `AVERROR_INVALIDDATA` (1071–1072) until a new parameter
/// set re-arms the restart (446–450). [`Self::flush`] does not rebuild
/// one: `avcodec_flush_buffers` reaches only the codec's own flush
/// (`libavcodec/avcodec.c` 417–418).
///
/// So a caller that sees failures persist rebuilds: it opens a software
/// decoder from the same parameters and feeds it forward. When to do so
/// is the caller's policy — the caller sees the errors, the packets' key
/// flags and what it has delivered — and [`DecodePath`](crate::DecodePath)
/// describes one.
pub struct VideoDecoder {
  /// Live FFmpeg state for the currently active backend.
  state: DecoderState,
  /// Reusable frame buffer used for hw-side decoding before transfer / move.
  /// Internal use only — never handed to callers.
  hw_frame: frame::Video,
  /// Probe state: present until the first frame is received from the active
  /// backend, then `None`. While `Some`, packets are buffered for replay and
  /// the candidate's own answers advance to the next backend
  /// ([`BackendAnswer`]).
  probe: Option<ProbeState>,
  /// The probe's history a candidate was committed before being fed, in
  /// order: what its replay still owed when it gave its first picture
  /// ([`Self::commit`]). Fed before anything new ([`Self::feed`]), each
  /// packet dropped once the decoder takes it.
  backlog: VecDeque<Packet>,
  /// Whether the end the caller already gave ([`Self::eof_sent`]) is still
  /// owed to the decoder: a candidate replayed a history that ends stops
  /// where it first has output for the caller, before the end
  /// ([`Self::feed`]). Implies `eof_sent`.
  end_owed: bool,
  /// Resource ceilings for the frames this decoder produces. Fixed at
  /// open, because [`FrameLimits::max_pixels`] is written into every
  /// `AVCodecContext` this decoder builds — including the ones a probe
  /// advance builds later — and a context's ceiling cannot be moved
  /// after `avcodec_open2`.
  frame_limits: crate::limits::DecoderLimits,
  /// The stream's packet timebase, written into every
  /// `AVCodecContext` this decoder opens — including the ones a probe
  /// advance opens later, which is why it is held rather than passed.
  ///
  /// `None` where the opener declares none: an untimed request
  /// ([`VideoDecoderOpen::timebase`]). The stream decoder one tier up
  /// always declares one.
  pkt_timebase: Option<mediadecode::Timebase>,
  /// How the decoder decodes ([`crate::DecoderOptions`]), written into
  /// every `AVCodecContext` this decoder opens — including the ones a
  /// probe advance opens later, which is why it is held. The request's
  /// ([`VideoDecoderOpen::options`]), or the stream decoder's one tier
  /// up.
  options: crate::DecoderOptions,
  /// `true` once [`Self::send_eof`] has been accepted, until
  /// [`Self::flush`].
  ///
  /// **It lives here rather than in [`ProbeState`], and that move is
  /// the point.** It used to be a probe field, so it vanished the
  /// moment the probe collapsed — a committed decoder could not tell
  /// whether the caller had signalled the end, and therefore could not
  /// answer the one question [`SessionPhase`] exists to answer without
  /// guessing. The probe machinery still reads it for replay; it simply
  /// no longer owns it.
  eof_sent: bool,
  /// The GPU-side scaled-output stage — see [`crate::vtscale`]. Carries
  /// the caller's standing [`Self::request_scaled_output`] and, on the
  /// VideoToolbox road, the cached pixel-transfer session and fitted
  /// frames context the CPU download reads from.
  ///
  /// **It rides the decoder rather than the wrapper** because the seam
  /// it inserts itself into is here: between the decoded hardware frame
  /// and `av_hwframe_transfer_data`, the one point at which a picture is
  /// still a GPU surface and can still be resized without a
  /// full-resolution round-trip through main memory.
  ///
  /// Declared last, so it drops **after** the [`DecoderState`] holding
  /// the VideoToolbox device its fitted frames context was built over —
  /// and that order is safe by the FFmpeg contract rather than by luck:
  /// `av_hwframe_ctx_alloc` "will make a new reference for internal
  /// use", so the device outlives the decoder that opened it for exactly
  /// as long as this stage still holds a context over it.
  scaled_output: crate::vtscale::ScaledOutput,
}

/// Owned FFmpeg state for one open codec context. Has its own `Drop` so we
/// can swap it out cleanly during a probe advance via `mem::replace`.
struct DecoderState {
  /// Wrapped FFmpeg decoder. `ManuallyDrop` so we can sequence its drop
  /// before freeing the callback state.
  inner: ManuallyDrop<ffmpeg_next::decoder::Video>,
  /// Backend driving this state.
  backend: Backend,
  /// Owned reference produced by `av_hwdevice_ctx_create`.
  hw_device_ref: *mut AVBufferRef,
  /// The installed state, owned by the raw pointer
  /// `AVCodecContext::opaque` aliases (from
  /// [`InstalledCallbackState::into_raw`]); freed in `Drop`, after the
  /// context.
  callback_state: *mut CallbackState,
}

/// The most packets the probe holds for a replay: its ceiling, with
/// [`MAX_PROBE_PACKET_BYTES`]. Set high enough to absorb long B-frame GOPs
/// and codec setup latency, low enough to bound memory against streams
/// that never produce a first frame.
///
/// **At the ceiling the trial ends on the next packet's answer.** The
/// history cannot take that packet, so it is sent to the candidate on trial
/// with the history whole ([`Staged::Last`]): taken, the candidate is
/// committed, with no failure claimed for a backend that has not failed;
/// refused, the candidate fails, and the next backend is tried with the
/// history as it stands. The ceiling counts the packets the history holds,
/// every one alike, from the open or the last [`VideoDecoder::flush`].
const MAX_PROBE_PACKETS: usize = 256;

/// Maximum total compressed-byte size of buffered probe packets. Each
/// buffered packet holds its own copy of the payload (see
/// [`try_clone_packet`](crate::boundary::try_clone_packet)), so this bounds bytes the probe itself holds.
/// 64 MiB is generous for normal video and gives untrusted media a hard
/// ceiling, which a packet that would carry the history past it reaches as
/// [`MAX_PROBE_PACKETS`] describes.
const MAX_PROBE_PACKET_BYTES: usize = 64 * 1024 * 1024;

/// Hard cap on the number of side-data entries we tolerate per buffered
/// packet. `av_packet_ref` allocates an `AVPacketSideData` descriptor and
/// an `AVBufferRef` per entry, so a packet stuffed with many tiny or
/// zero-sized entries can consume significant memory in descriptor /
/// allocator overhead even after [`packet_side_data_bytes`] charges
/// [`SIDE_DATA_ENTRY_OVERHEAD`] bytes per entry. Such a packet is never
/// copied, which short-circuits the descriptor explosion path: it is the
/// trial's last ([`Unkept::SideData`]), sent with the history whole, and
/// the candidate that takes it is committed. FFmpeg's own API holds a
/// packet to one entry per kind (`av_packet_add_side_data`,
/// `libavcodec/packet.c` 197–228), so only a packet built past it is.
///
/// Sized for legitimate streams (typical video packets carry 0-5 side-
/// data entries; SEI-heavy HEVC/AV1 maybe a dozen) while comfortably
/// rejecting weaponised input.
///
/// Shared with the [`crate::FfmpegVideoStreamDecoder`] rolling GOP buffer,
/// which charges the same side-data budget so its byte cap is a true upper
/// bound on retained memory rather than counting bare payloads.
pub(crate) const MAX_PROBE_PACKET_SIDE_DATA_ENTRIES: usize = 64;

/// Conservative per-side-data-entry overhead estimate used by both
/// [`packet_side_data_bytes`] and the budget accounting in
/// [`VideoDecoder::send_packet`]. Counts the `AVPacketSideData`
/// descriptor (24 bytes per the FFmpeg 9.x bindings), the `AVBufferRef`
/// FFmpeg allocates per entry, and a margin for malloc bookkeeping
/// (header bytes, alignment slack). Setting it on the high side keeps
/// the byte cap a true upper bound on retained memory; under-charging
/// would let many tiny entries slip past the cap.
const SIDE_DATA_ENTRY_OVERHEAD: usize = 80;

/// Where a decoding session is in its life — **the one derived fact the
/// classifiers read, and the only place the latches are interpreted.**
///
/// Every road that must decide what an errno *means* needs the same two
/// questions answered, and answering them ad hoc at each road is what
/// let them disagree. `EAGAIN` means "send me more" only where more can
/// come; `AVERROR_EOF` means "the stream is over" only where a backend
/// has committed to producing it. Read those wrong and a caller is
/// handed a state with no satisfying operation, or a candidate that
/// will never produce a frame is mistaken for a finished stream.
///
/// The two questions are exactly the two dimensions the machinery
/// already keeps latches for — whether an end has been recorded, and
/// whether a backend is still on trial — so this enum is a census of
/// those latches rather than a new idea. Deriving it lives in
/// [`VideoDecoder::phase`] and its siblings, one per session type;
/// nothing else reads a latch to answer a classification question.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum SessionPhase {
  /// A committed backend, and no end-of-stream recorded. Both flow
  /// signals mean what they say.
  Streaming,
  /// A committed backend draining its tail after a recorded end. "Send
  /// me more" is no longer satisfiable here — the caller has nothing
  /// left to send and the send gates refuse — so it reads as the end.
  Draining,
  /// A candidate backend on trial, no end recorded. It may legitimately
  /// want more input; what it may not do is quietly end the stream on
  /// the caller's behalf, which is the committed backend's privilege.
  Auditioning,
  /// A candidate on trial that has already been handed the whole
  /// history, **end-of-stream included**.
  ///
  /// The arm that had no name. A candidate here has been given
  /// everything there is and answers about a stream that is already
  /// over, so its answer settles the trial. libavcodec's own
  /// `AVERROR_EOF`, no refusal latched, is a successful empty completion:
  /// the receive road commits the candidate, and the stream has ended.
  /// `EAGAIN` is neither "send me more" (nothing is left to send) nor the
  /// end (a drained decoder answers `AVERROR_EOF`): it is the candidate
  /// failing, the probe's business.
  AuditioningPastEnd,
}

impl SessionPhase {
  /// Whether more input can still reach this session.
  ///
  /// The satisfiability question: [`Received::NeedsInput`] and
  /// [`Sent::MustDrain`] are both instructions, and both are honest
  /// only where the caller can carry them out.
  pub(crate) const fn accepts_input(self) -> bool {
    matches!(self, Self::Streaming | Self::Auditioning)
  }

  /// Whether a backend has committed, and so may speak for the stream.
  ///
  /// Only a committed backend's `AVERROR_EOF` is the stream's end. A
  /// candidate's is its own: it drained to nothing without ever proving
  /// it could decode this content.
  pub(crate) const fn is_committed(self) -> bool {
    matches!(self, Self::Streaming | Self::Draining)
  }
}

/// How a funnel verdict routes. See [`VideoDecoder::verdict_routing`].
enum VerdictRouting {
  /// The name says this backend cannot take this stream: while it is on
  /// trial, the candidate fails. After commit the verdict is reported as
  /// it was minted, as every verdict is.
  CandidateFailed,
  /// The name says retrying a backend cannot help — report it as it is.
  Direct,
  /// Nothing was named; the road's own reading decides.
  Unnamed,
}

/// What an **unnamed** verdict means on the road that produced it — the
/// one thing the shared routing policy cannot know for itself.
enum BareVerdict {
  /// A flow signal's own fault: `avcodec_send_packet` answering
  /// `AVERROR_EOF` to a submission past the end. The probe must not
  /// advance on it — the candidate did nothing wrong, the caller did.
  Reported,
  /// A real failure. While a candidate is on trial, that is the
  /// candidate failing.
  CandidateFailure,
}

/// What the caller should do with a routed hardware failure.
enum HwRoute {
  /// Hand this to the caller.
  Report(Error),
  /// The active candidate failed: advance the probe and retry.
  Advance(BackendAnswer),
}

/// **A candidate's own answer, and the only thing the probe records
/// against a backend or advances on.**
///
/// It is made at the roads a candidate answers by, and nowhere else: its
/// opening ([`Self::of_open`]), and its own FFmpeg calls — a send, a
/// receive, a download — read through the funnel, which names a refusal its
/// `get_format` latched ([`Self::of_call`]). A failure of this crate's own —
/// an allocation, a copy, a cap, the caller's limits on what a picture costs
/// — is an [`Error`], surfaced as it is. The probe's record ([`ProbeState::attempts`]) and its
/// advance ([`VideoDecoder::advance_probe_with`], [`HwRoute::Advance`]) take
/// this type alone, so no such failure can enter the one or start the
/// other: what is charged to a backend is, by type, something the backend
/// said.
#[derive(Debug)]
pub(crate) struct BackendAnswer(Error);

impl BackendAnswer {
  /// What one of a candidate's own calls answered: `reason`, the funnel's
  /// verdict on the call's errno ([`VideoDecoder::hw_route`]).
  const fn of_call(reason: Error) -> Self {
    Self(reason)
  }

  /// What a backend answered when it was opened for the stream: the codec
  /// offers it no hardware configuration, its device would not be created,
  /// or the codec would not open on it ([`VideoDecoder::build_state`]).
  const fn of_open(reason: Error) -> Self {
    Self(reason)
  }

  /// The answer, as the error a caller reads.
  fn into_error(self) -> Error {
    self.0
  }
}

/// The record a probe hands the caller: each backend's answer, as the error
/// it was.
fn answered(attempts: Vec<(Backend, BackendAnswer)>) -> Vec<(Backend, Box<Error>)> {
  attempts
    .into_iter()
    .map(|(backend, answer)| (backend, Box::new(answer.into_error())))
    .collect()
}

/// Why a candidate could not be built ([`BuildCandidate`]).
enum BuildFailure {
  /// The backend's own answer: it cannot take the stream.
  Refused(BackendAnswer),
  /// A failure of this crate's own, or the stream's — the same for every
  /// backend: surfaced as it is, and no backend's answer.
  Local(Error),
}

impl From<BuildFailure> for Error {
  fn from(failure: BuildFailure) -> Self {
    match failure {
      BuildFailure::Refused(answer) => answer.into_error(),
      BuildFailure::Local(error) => error,
    }
  }
}

/// How [`VideoDecoder::advance_probe_with`] builds a candidate: the
/// signature of [`VideoDecoder::build_state`], which every production
/// road passes.
type BuildCandidate = fn(
  codec::Parameters,
  Codec,
  Backend,
  crate::limits::DecoderLimits,
  crate::DecoderOptions,
  Option<mediadecode::Timebase>,
) -> std::result::Result<DecoderState, BuildFailure>;

/// How far [`VideoDecoder::feed`] got.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Fed {
  /// What the decoder took: packets, and the end.
  taken: usize,
  /// Whether it took everything it was owed; where not, it answered
  /// `EAGAIN` first, with output for the caller.
  all: bool,
}

/// What feeding the decoder what it is owed came to, routed as a send is
/// ([`VideoDecoder::feed_owed`]).
enum Owed {
  /// Everything owed was taken.
  Taken,
  /// The decoder answered `EAGAIN` first: it has output for the caller.
  Full,
  /// A committed decoder refused it: that packet's own error.
  Report(Error),
  /// The candidate on trial refused it: its own answer.
  Advance(BackendAnswer),
}

/// How far [`feed_into`] got with a replay.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Taken {
  /// How many of the history's packets the candidate took, in order.
  packets: usize,
  /// Whether it took the end.
  end: bool,
}

/// What [`VideoDecoder::send_av_packet`] does with a packet once the
/// candidate on trial takes it.
enum Staged {
  /// The history records the probe's copy, and holds the second field's
  /// bytes with it.
  Recorded(Packet, usize),
  /// The history cannot keep the packet, so it is the trial's last: taken,
  /// the candidate is committed.
  Last(Unkept),
}

/// Why the probe's history cannot keep a packet for a replay — **the
/// wrapper's own limits, and never a backend's answer.** None ends the
/// trial without the candidate's word: the packet is sent with the history
/// whole, the candidate that takes it is committed, and one that refuses it
/// fails on its own refusal ([`VideoDecoder::send_av_packet`]).
#[derive(Debug)]
enum Unkept {
  /// It would carry the history past the probe's ceiling, to `packets`
  /// packets or `bytes` bytes ([`MAX_PROBE_PACKETS`],
  /// [`MAX_PROBE_PACKET_BYTES`]).
  Ceiling { packets: usize, bytes: usize },
  /// It carries `entries` side-data entries, more than the probe copies
  /// ([`MAX_PROBE_PACKET_SIDE_DATA_ENTRIES`]).
  SideData { entries: usize },
  /// The probe's copy of it could not be made.
  Copy(ffmpeg_next::Error),
}

impl core::fmt::Display for Unkept {
  fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
    match self {
      Self::Ceiling { packets, bytes } => write!(
        f,
        "past the probe's ceiling, at {packets} packets and {bytes} bytes"
      ),
      Self::SideData { entries } => {
        write!(f, "{entries} side-data entries, more than the probe copies")
      }
      Self::Copy(e) => write!(f, "the probe's copy could not be made: {e}"),
    }
  }
}

/// State carried only during the probe window (before the first successful
/// frame). Holds enough information to tear down the current decoder and
/// retry with the next backend.
struct ProbeState {
  parameters: codec::Parameters,
  codec: Codec,
  /// Backends still to try, in order. Empty means "no more options after
  /// the active one fails" — `advance_probe` then surfaces
  /// [`Error::AllBackendsFailed`] so the contract is the same on
  /// single-backend platforms (e.g. macOS) as on multi-backend ones.
  remaining_backends: Vec<Backend>,
  /// How the probe builds each candidate it advances to:
  /// [`VideoDecoder::build_state`] on every production road. A lane opens
  /// its candidates in software through this, so that the probe's own
  /// roads advance, replay and commit on any machine.
  build: BuildCandidate,
  /// Packets sent so far, kept for replay through any candidate backend.
  /// Preserved across failed candidates — cleared when the probe
  /// collapses, and forgotten at a flush ([`Self::forget_history`]).
  buffered_packets: Vec<Packet>,
  /// Cumulative size (in compressed bytes) of `buffered_packets`. Tracked
  /// incrementally so we don't have to re-sum on every send.
  buffered_bytes: usize,
  /// How many of `buffered_packets` the candidate on trial has been fed:
  /// all of them, but after an advance whose replay stopped where the
  /// candidate first had output for the caller ([`feed_into`]). The rest
  /// is owed, and fed before anything new ([`VideoDecoder::feed`]).
  fed: usize,
  /// Each backend's answer since the probe window opened, in the order
  /// tried: a backend that failed to open, the active one when it fails,
  /// and a candidate that refused its replay. A [`BackendAnswer`] alone, so
  /// nothing of this crate's own is ever charged here. Drained into
  /// [`Error::AllBackendsFailed`] when the probe exhausts every option.
  attempts: Vec<(Backend, BackendAnswer)>,
}

impl ProbeState {
  /// Forgets what the history recorded at a position the caller has left
  /// ([`VideoDecoder::flush`]): the packets, and every count kept of them,
  /// so the ceiling counts from the flush.
  ///
  /// **Exhaustive on purpose.** Every field is named here, kept or
  /// forgotten, so a field added to the probe does not compile until it is
  /// sorted into one or the other, and no count can outlive a flush unseen.
  /// What is kept is the probe's, not the position's: the stream's
  /// parameters and codec, the backends still to try and how they are
  /// built, and the failures already seen.
  fn forget_history(&mut self) {
    let Self {
      parameters: _,
      codec: _,
      remaining_backends: _,
      build: _,
      attempts: _,
      buffered_packets,
      buffered_bytes,
      fed,
    } = self;
    buffered_packets.clear();
    *buffered_bytes = 0;
    *fed = 0;
  }
}

// SAFETY: All raw pointers are exclusively owned by `DecoderState` and never
// shared. `ffmpeg::decoder::Video` is itself `Send` (its `Context` carries an
// `unsafe impl Send`). The decoder is not safe for concurrent use, hence not
// `Sync`.
//
// `VideoDecoder`'s claim additionally covers what `crate::vtscale` holds on
// Apple targets: a `VTPixelTransferSessionRef` and an `AVBufferRef` to a
// fitted `AVHWFramesContext`. Both are exclusively owned by the stage — no
// `Clone`, no handing out — and every use of the session goes through
// `&mut ScaledOutput`, so the one-transfer-at-a-time protocol
// `VTPixelTransferSessionTransferImage` expects is serialized by the borrow
// rather than by a lock. VideoToolbox sessions are Core Foundation objects
// with no thread affinity (unlike the UI-framework types that have one), so
// moving one between threads is exactly as sound as moving the codec context
// beside it. Still not `Sync`, for the same reason nothing else here is.
unsafe impl Send for DecoderState {}
unsafe impl Send for VideoDecoder {}

impl Drop for DecoderState {
  fn drop(&mut self) {
    // Order matters:
    //  1. Drop the codec context first. While it lives, FFmpeg may invoke
    //     `get_format`, which dereferences `callback_state` via `opaque`.
    //  2. Free the callback state heap allocation.
    //  3. Release our hw device reference (FFmpeg released its own when
    //     the codec context was freed in step 1).
    unsafe {
      ManuallyDrop::drop(&mut self.inner);
      if !self.callback_state.is_null() {
        drop(Box::from_raw(self.callback_state));
        self.callback_state = ptr::null_mut();
      }
      if !self.hw_device_ref.is_null() {
        av_buffer_unref(&mut self.hw_device_ref);
      }
    }
  }
}

impl VideoDecoder {
  /// Auto-probe hardware backends in the platform's default order.
  ///
  /// Each backend opens with a strict `get_format` callback. The first
  /// backend whose `avcodec_open2` succeeds becomes active; if its first
  /// frame is unusable (decode error, transfer failure, or a CPU-format
  /// frame from a HW context) the decoder is torn down and the next backend
  /// is tried — packets sent so far are replayed through the new decoder
  /// transparently. The probe advance is transactional: the next backend
  /// must build *and* take the history, as far as it takes it before it has
  /// output for the caller, before it replaces the one that failed; the
  /// history stays the probe's own, so a misbehaving middle backend cannot
  /// strand the caller.
  ///
  /// [`Self::backend`] reflects whichever backend ultimately produced the
  /// first frame.
  ///
  /// [`Error::AllBackendsFailed`] surfaces in two places, with the same
  /// meaning ("no hardware backend can decode this stream — fall back to
  /// software yourself"):
  /// - From `open` itself, when no backend even opens.
  /// - From [`Self::send_packet`] / [`Self::send_eof`] /
  ///   [`Self::receive_frame`], when the initially-opened backend fails
  ///   at decode time and every remaining backend in the probe order
  ///   either also fails or doesn't exist. On single-backend platforms
  ///   (e.g. macOS, where the order is `[VideoToolbox]`), this is the
  ///   only place a HW-only failure surfaces.
  ///
  /// In both cases, `attempts` carries the per-backend error log. When
  /// the runtime path fires, `unconsumed_packets` also contains the
  /// packets the decoder consumed from the caller before the probe
  /// exhausted (the probe's own copies of them); for non-seekable inputs
  /// (live streams, pipes) the caller can replay these directly into
  /// a software decoder of their choice without re-demuxing. From the
  /// open-time path the vec is empty since no packets have been sent.
  ///
  /// On `Ok`, the returned decoder **always** has an active probe
  /// rescue safety net. If a parameters clone fails under memory
  /// pressure before the probe state can be set up, `open` returns
  /// `Err(Error::Ffmpeg(Other { errno: ENOMEM }))` rather than handing
  /// back a live decoder with no fallback contract. No packets have
  /// been sent yet, so the caller can retry or fall back to software
  /// with the original `parameters` directly.
  pub fn open(parameters: codec::Parameters) -> Result<Self> {
    Self::open_from(VideoDecoderOpen::new(), parameters)
  }

  /// Opens the decoder `request` names over `parameters`. This is the one
  /// road the eight `open*` constructors take.
  ///
  /// With no backend pinned it probes the platform's backends in order,
  /// as [`Self::open`] describes. With one pinned it opens that one alone,
  /// as [`Self::open_with`] describes. The request's limits, packet
  /// timebase and [`DecoderOptions`](crate::DecoderOptions) are written
  /// into every `AVCodecContext` the decoder opens, including the ones a
  /// later probe advance opens.
  ///
  /// **The options are not the road for a picture cadence.** A packet the
  /// demuxer marks
  /// [`DISPOSABLE`](mediadecode::packet::PacketFlags::DISPOSABLE) is
  /// dropped before it is sent. On a temporally scalable HEVC stream,
  /// [`Discard::NonReference`](crate::Discard::NonReference) also skips
  /// pictures a higher sub-layer references, which the mark never does.
  /// See [`crate::options`].
  pub fn open_from(request: VideoDecoderOpen, parameters: codec::Parameters) -> Result<Self> {
    match request.backend() {
      None => Self::open_probing(
        parameters,
        request.limits(),
        request.options(),
        request.timebase(),
        backend::probe_order(),
      ),
      Some(backend) => Self::open_with_limits_timed_in(
        parameters,
        backend,
        request.limits(),
        request.options(),
        request.timebase(),
      ),
    }
  }

  /// [`Self::open`], with the stream's packet timebase declared.
  ///
  /// **Prefer this one whenever the caller knows the timebase**, which
  /// is nearly always: the packets fed to a decoder carry timestamps in
  /// their stream's units, and `AVCodecContext.pkt_timebase` is how
  /// libavcodec is told what those units are. Not owning the
  /// `AVStream` does not mean not knowing its ruler.
  ///
  /// The value reaches every `AVCodecContext` this decoder opens,
  /// including the ones a later hardware probe advance opens.
  pub fn open_timed(
    parameters: codec::Parameters,
    timebase: mediadecode::Timebase,
  ) -> Result<Self> {
    Self::open_from(
      VideoDecoderOpen::new().with_timebase(Some(timebase)),
      parameters,
    )
  }

  /// [`Self::open`], with the frame ceilings named.
  ///
  /// Taken at open for the reason [`Self::open_with_limits`] gives:
  /// [`FrameLimits::max_pixels`] is written into every `AVCodecContext`
  /// this decoder opens — including the ones a later probe advance
  /// opens — and a context's ceiling cannot be moved after
  /// `avcodec_open2`.
  pub fn open_with_frame_limits(
    parameters: codec::Parameters,
    limits: crate::limits::DecoderLimits,
  ) -> Result<Self> {
    // **Explicitly untimed.** No packet timebase is declared, so
    // `AVCodecContext.pkt_timebase` is left at libavcodec's own
    // default and anything it derives from that field — a subtitle
    // cue's PTS, a packet-duration fallback — is unavailable. Reach
    // for [`Self::open_with_frame_limits_timed`] when the stream's
    // ruler is known, which is nearly always.
    Self::open_from(VideoDecoderOpen::new().with_limits(limits), parameters)
  }

  /// [`Self::open_with_frame_limits`], with the stream's packet
  /// timebase declared — see [`Self::open_timed`].
  pub fn open_with_frame_limits_timed(
    parameters: codec::Parameters,
    limits: crate::limits::DecoderLimits,
    timebase: mediadecode::Timebase,
  ) -> Result<Self> {
    Self::open_from(
      VideoDecoderOpen::new()
        .with_limits(limits)
        .with_timebase(Some(timebase)),
      parameters,
    )
  }

  /// The probe itself, across `order` — [`backend::probe_order`] for
  /// [`Self::open_from`] and the stream decoder.
  ///
  /// The probe runs under every skip policy, `All` among them: what a
  /// codec skips is the codec's to say, and the candidate's answers say it
  /// (see the type's documentation).
  ///
  /// Taken as an argument so a lane can stand on a platform with no
  /// hardware backend at all, which no machine that runs the suite is: an
  /// empty `order` is exactly that platform's probe, and it answers
  /// [`Error::AllBackendsFailed`] with no attempt and no packet.
  pub(crate) fn open_probing(
    parameters: codec::Parameters,
    limits: crate::limits::DecoderLimits,
    options: crate::DecoderOptions,
    pkt_timebase: Option<mediadecode::Timebase>,
    order: &[Backend],
  ) -> Result<Self> {
    let codec = find_decoder(&parameters)?;

    let mut attempts: Vec<(Backend, BackendAnswer)> = Vec::new();
    for (i, &backend) in order.iter().enumerate() {
      // Use the checked clone — ffmpeg-next's `Parameters::clone` does
      // `avcodec_parameters_alloc` without a null check and ignores the
      // return of `avcodec_parameters_copy`. Under OOM that path silently
      // produces a Parameters with a null inner pointer.
      //
      // **The copy is this crate's own, and no backend's answer**: it is
      // surfaced as it is, and the next backend would need the same copy.
      let cloned_for_build = try_clone_parameters(&parameters, limits.max_codec_parameter_bytes())?;
      match Self::build_state(
        cloned_for_build,
        codec,
        backend,
        limits,
        options,
        pkt_timebase,
      ) {
        Ok(state) => {
          tracing::info!(?backend, "hwdecode: opened video decoder (probing)");
          let remaining = order[(i + 1)..].to_vec();
          // Deep-copy the caller's `parameters` before storing in ProbeState.
          // `codec::Parameters` from `stream.parameters()` carries an Rc
          // owner pointing at the demuxer; moving that Rc to a worker
          // thread (when VideoDecoder is sent) would race with the demuxer's
          // Rc on the original thread. The checked clone copies the bytes
          // into a fresh allocation with `owner: None`, severing the link.
          //
          // We always create ProbeState — even when `remaining` is empty
          // (single-backend platforms like macOS) — so that a first-frame
          // failure on the only backend surfaces as
          // `Error::AllBackendsFailed` from `receive_frame` /
          // `send_packet` rather than as a raw FFmpeg error. That keeps
          // the API contract the same regardless of how many HW backends
          // the platform exposes.
          //
          // If the clone fails (ENOMEM), fail the **whole open call**
          // rather than returning a live decoder with `probe: None`.
          // Returning Ok here would let the caller send packets that the
          // active backend consumes, and a subsequent backend failure
          // would then surface as a raw FFmpeg error with no
          // `unconsumed_packets` — silently breaking the rescue contract
          // for non-seekable inputs (live streams, pipes). Dropping the
          // already-built `state` here runs its FFmpeg cleanup, and the
          // caller can retry / fall back to software with the original
          // parameters in their hand (no packets were consumed yet).
          // Seed the probe's attempt log with any backends that failed
          // to open earlier in this loop (including
          // `BackendUnsupportedByCodec` and parameters-clone errors).
          // Without this, a runtime exhaustion on the active backend
          // would surface an `AllBackendsFailed` containing only the
          // active backend's runtime failure — losing the original
          // open-time causes that, on multi-backend platforms (Linux,
          // Windows), are usually the more diagnostic signal. E.g. a
          // VAAPI-then-CUDA host where VAAPI fails to open and CUDA
          // later fails at first-frame must report both failures in
          // probe order, not just CUDA.
          let probe = match try_clone_parameters(&parameters, limits.max_codec_parameter_bytes()) {
            Ok(probe_params) => ProbeState {
              parameters: probe_params,
              codec,
              remaining_backends: remaining,
              build: Self::build_state,
              buffered_packets: Vec::new(),
              buffered_bytes: 0,
              fed: 0,
              attempts: std::mem::take(&mut attempts),
            },
            Err(e) => {
              tracing::warn!(
                error = %e,
                "hwdecode: parameters clone failed for probe state at open; \
                 failing closed instead of returning a decoder without rescue"
              );
              return Err(e);
            }
          };
          return Ok(Self {
            state,
            hw_frame: alloc_av_frame().map_err(Error::Ffmpeg)?,
            probe: Some(probe),
            backlog: VecDeque::new(),
            end_owed: false,
            frame_limits: limits,
            pkt_timebase,
            options,
            eof_sent: false,
            scaled_output: crate::vtscale::ScaledOutput::new(),
          });
        }
        Err(BuildFailure::Refused(answer)) => {
          tracing::warn!(?backend, error = ?answer, "hwdecode: backend open failed");
          attempts.push((backend, answer));
        }
        // This crate's own failure, or the stream's: the same for every
        // backend, so surfaced as it is rather than charged to this one.
        Err(BuildFailure::Local(e)) => return Err(e),
      }
    }
    // No packets have been consumed at open time.
    Err(Error::AllBackendsFailed(AllBackendsFailed::new(
      answered(attempts),
      Vec::new(),
    )))
  }

  /// Open the decoder with a specific backend. No probe, no fallback.
  ///
  /// If `backend` cannot actually decode this stream, the failure surfaces
  /// from [`Self::receive_frame`] (the strict `get_format` callback returns
  /// `AV_PIX_FMT_NONE`, the decoder errors out). The caller is responsible
  /// for retrying with another hardware backend or falling back to a
  /// software decoder of their choice (e.g. `ffmpeg::decoder::Video`).
  pub fn open_with(parameters: codec::Parameters, backend: Backend) -> Result<Self> {
    Self::open_from(
      VideoDecoderOpen::new().with_backend(Some(backend)),
      parameters,
    )
  }

  /// [`Self::open_with`], with the stream's packet timebase declared —
  /// see [`Self::open_timed`].
  pub fn open_with_timed(
    parameters: codec::Parameters,
    backend: Backend,
    timebase: mediadecode::Timebase,
  ) -> Result<Self> {
    Self::open_from(
      VideoDecoderOpen::new()
        .with_backend(Some(backend))
        .with_timebase(Some(timebase)),
      parameters,
    )
  }

  /// [`Self::open_with`], with the frame ceilings named.
  ///
  /// The limits are taken **at open**, not through a `with_*` builder,
  /// because [`FrameLimits::max_pixels`] is written straight into the
  /// `AVCodecContext` this call opens — that is the layer that makes
  /// libavcodec refuse an oversized picture before allocating it, and a
  /// context's ceiling cannot be moved after `avcodec_open2`. A builder
  /// would have silently applied to only half the enforcement.
  pub fn open_with_limits(
    parameters: codec::Parameters,
    backend: Backend,
    limits: crate::limits::DecoderLimits,
  ) -> Result<Self> {
    // Explicitly untimed — see [`Self::open_with_frame_limits`].
    Self::open_from(
      VideoDecoderOpen::new()
        .with_backend(Some(backend))
        .with_limits(limits),
      parameters,
    )
  }

  /// [`Self::open_with_limits`], with the stream's packet timebase
  /// declared — see [`Self::open_timed`].
  pub fn open_with_limits_timed(
    parameters: codec::Parameters,
    backend: Backend,
    limits: crate::limits::DecoderLimits,
    timebase: mediadecode::Timebase,
  ) -> Result<Self> {
    Self::open_from(
      VideoDecoderOpen::new()
        .with_backend(Some(backend))
        .with_limits(limits)
        .with_timebase(Some(timebase)),
      parameters,
    )
  }

  /// The road onto one pinned backend: [`Self::open_from`]'s with a
  /// backend pinned, and the stream decoder's.
  pub(crate) fn open_with_limits_timed_in(
    parameters: codec::Parameters,
    backend: Backend,
    limits: crate::limits::DecoderLimits,
    options: crate::DecoderOptions,
    pkt_timebase: Option<mediadecode::Timebase>,
  ) -> Result<Self> {
    let codec = find_decoder(&parameters)?;
    let state = Self::build_state(parameters, codec, backend, limits, options, pkt_timebase)?;
    Ok(Self {
      state,
      hw_frame: alloc_av_frame().map_err(Error::Ffmpeg)?,
      probe: None,
      backlog: VecDeque::new(),
      end_owed: false,
      frame_limits: limits,
      pkt_timebase,
      options,
      eof_sent: false,
      scaled_output: crate::vtscale::ScaledOutput::new(),
    })
  }

  /// Builds a decoder around a **software** `ffmpeg::decoder::Video`,
  /// for tests that need [`VideoDecoder`]'s own send/receive arms driven
  /// against real libavcodec.
  ///
  /// **Why this exists.** Those arms classify libavcodec's flow control
  /// themselves, and the only other way to reach them is
  /// [`VideoDecoder::open`], which needs a working hardware backend and
  /// a sample file — so every existing lane through them is
  /// `#[ignore]`-gated and runs nowhere. A regression that never runs is
  /// a claim, not a check. This keeps the arms, the probe state and the
  /// funnels exactly as production builds them and swaps only the
  /// backend behind `state.inner`, which is the one thing a test cannot
  /// otherwise supply.
  ///
  /// `auditioning` opens the probe window with **no backends left to
  /// try**, which is what makes the candidate-failure road observable:
  /// `advance_probe` has nowhere to advance to, so it surfaces
  /// [`Error::AllBackendsFailed`] and a lane can tell "the probe road
  /// was taken" from "a status was answered". Passing `false` leaves the
  /// probe collapsed, for lanes that mean to exercise a committed
  /// backend.
  ///
  /// The `backend` label is cosmetic here — it is read only for the
  /// attempt log and [`Self::backend`], neither of which a software
  /// decoder reaches — and `hw_device_ref` is null, which
  /// [`DecoderState`]'s `Drop` already handles.
  #[cfg(test)]
  pub(crate) fn from_software_for_test(
    parameters: codec::Parameters,
    limits: crate::limits::DecoderLimits,
    auditioning: bool,
  ) -> Result<Self> {
    let codec = find_decoder(&parameters)?;
    Self::from_software_for_test_as(
      parameters,
      codec,
      limits,
      crate::DecoderOptions::new(),
      auditioning,
    )
  }

  /// [`Self::from_software_for_test`], over the decoder `codec` names —
  /// one by name, where FFmpeg registers more than one for a codec — and
  /// with `options` written into its context, as every road writes them.
  #[cfg(test)]
  pub(crate) fn from_software_for_test_as(
    parameters: codec::Parameters,
    codec: Codec,
    limits: crate::limits::DecoderLimits,
    options: crate::DecoderOptions,
    auditioning: bool,
  ) -> Result<Self> {
    let probe = auditioning.then(|| ProbeState {
      parameters: try_clone_parameters(&parameters, limits.max_codec_parameter_bytes())
        .expect("a clonable parameter set"),
      codec,
      remaining_backends: Vec::new(),
      build: Self::build_state,
      buffered_packets: Vec::new(),
      buffered_bytes: 0,
      fed: 0,
      attempts: Vec::new(),
    });
    let backend = backend::probe_order()
      .first()
      .copied()
      .unwrap_or(Backend::VideoToolbox);
    let state = Self::software_state_for_test(parameters, codec, backend, limits, options, None)?;
    Ok(Self {
      state,
      hw_frame: alloc_av_frame().map_err(Error::Ffmpeg)?,
      probe,
      backlog: VecDeque::new(),
      end_owed: false,
      frame_limits: limits,
      pkt_timebase: None,
      options,
      eof_sent: false,
      scaled_output: crate::vtscale::ScaledOutput::new(),
    })
  }

  /// A [`BuildCandidate`] that opens the decoder in software: the state
  /// [`Self::from_software_for_test`] stands on, labelled `backend`.
  ///
  /// Passed to [`Self::advance_probe_with`], it is a candidate the probe
  /// can build, replay into and commit on any machine.
  #[cfg(test)]
  fn software_state_for_test(
    parameters: codec::Parameters,
    codec: Codec,
    backend: Backend,
    limits: crate::limits::DecoderLimits,
    options: crate::DecoderOptions,
    pkt_timebase: Option<mediadecode::Timebase>,
  ) -> std::result::Result<DecoderState, BuildFailure> {
    let (ctx, callback_state) = build_codec_context(&parameters, limits, options, pkt_timebase)
      .map_err(BuildFailure::Local)?;
    let opened = ctx
      .decoder()
      .open_as(codec)
      .map_err(|e| BuildFailure::Refused(BackendAnswer::of_open(Error::Ffmpeg(e))))?;
    ensure_video_codec_type(&opened).map_err(BuildFailure::Local)?;
    Ok(DecoderState {
      inner: ManuallyDrop::new(ffmpeg_next::decoder::Video(opened)),
      backend,
      hw_device_ref: ptr::null_mut(),
      callback_state: callback_state.into_raw(),
    })
  }

  /// The backend currently producing frames. While the probe is still in
  /// progress (no frame received yet) this returns the optimistically
  /// selected backend; after the first frame, it is the backend that
  /// actually produced it. Once stable, never changes again.
  pub fn backend(&self) -> Backend {
    self.state.backend
  }

  /// How this decoder decodes: the options it was opened with
  /// ([`VideoDecoderOpen::options`]), written into every `AVCodecContext`
  /// it opens. They are libavcodec's defaults for the eight `open*`
  /// constructors.
  pub const fn options(&self) -> crate::DecoderOptions {
    self.options
  }

  /// Whether this decoder can emit pictures at a caller-requested size
  /// instead of full coded size.
  ///
  /// [`ScaledOutputCapability::Supported`] on exactly one road: the
  /// VideoToolbox backend on an Apple target, where
  /// [`crate::vtscale`]'s `VTPixelTransferSession` sits between the
  /// decoded hardware frame and the CPU download. Every other backend
  /// this crate wires — [`Backend::Vaapi`], [`Backend::Cuda`],
  /// [`Backend::D3d11va`] — answers `Unsupported`, each with its own
  /// filed native scaling seam.
  ///
  /// # And it stops saying `Supported` the moment the promise breaks
  ///
  /// The trait's contract is that a `Supported` answer lets a caller
  /// skip its own resampler, so a session that quietly went back to
  /// full-size pictures under that answer would hand the caller mixed
  /// extents with nothing to notice them by. The stage can stand down
  /// per frame — a padded pixel buffer, a crop rectangle, side data a
  /// resize would strand, a resolution change that turns the standing
  /// request into an upscale, a transfer that fails — and the **first**
  /// frame that goes out at anything other than the requested extent
  /// flips this answer to [`ScaledOutputCapability::Unsupported`]. That
  /// is the explicit transition a caller is told to look for: query
  /// again and learn that resampling is theirs once more. A fresh
  /// [`Self::request_scaled_output`] buys a fresh promise.
  ///
  /// A request the stream already satisfies is not a broken promise:
  /// the picture arrives at exactly the size asked for.
  ///
  /// A pure query: it neither requests anything nor changes what
  /// [`Self::receive_frame`] delivers.
  pub fn scaled_output_capability(&self) -> ScaledOutputCapability {
    if crate::vtscale::ScaledOutput::supported()
      && self.state.backend.is_video_toolbox()
      && self.scaled_output.promise_stands()
    {
      ScaledOutputCapability::Supported
    } else {
      ScaledOutputCapability::Unsupported
    }
  }

  /// Asks this decoder to emit pictures at `size` from the next frame
  /// on, and reports whether the request was recorded.
  ///
  /// # What "from the next frame on" means, exactly
  ///
  /// The stage is consulted per frame, on the way out of the decoder
  /// and before the GPU→CPU download. So a request placed mid-stream
  /// takes effect on the next picture `receive_frame` produces — never
  /// retroactively on one it has already produced, and never later than
  /// that.
  ///
  /// # Refusal is not an error
  ///
  /// [`ScaledOutputCapability::Unsupported`] comes back when this road
  /// has no stage at all (see [`Self::scaled_output_capability`]), when
  /// either requested extent is zero, or when the request is an
  /// **upscale** of the stream's coded size — the stage exists to move
  /// fewer bytes across the GPU→CPU bus, and enlarging a picture moves
  /// more. Nothing about [`Self::send_packet`] or
  /// [`Self::receive_frame`] can fail because of it.
  ///
  /// **A refusal returns the session to full coded size**, dropping any
  /// request already standing. That is what the trait says this answer
  /// means, and the only reading a caller can act on: told
  /// `Unsupported` it resamples for itself, and a session that went on
  /// quietly fitting to an older request would have it resample an
  /// already-fitted picture. Like an acceptance, it takes effect from
  /// the next picture; one already decoded keeps the extent it was
  /// decoded at.
  ///
  /// See [`ScaledOutputCapability`] for the determinism trade a caller
  /// takes on by acting on a `Supported` answer.
  pub fn request_scaled_output(&mut self, size: (u32, u32)) -> ScaledOutputCapability {
    // **The road, not the standing promise.** A session whose promise
    // broke on an earlier request answers `Unsupported` from
    // [`Self::scaled_output_capability`] until somebody asks again —
    // and asking again is exactly this, so gating it on that answer
    // would make the break permanent and unrecoverable. What is checked
    // here is what cannot change: whether this build and this backend
    // have a stage at all.
    if !crate::vtscale::ScaledOutput::supported() || !self.state.backend.is_video_toolbox() {
      return ScaledOutputCapability::Unsupported;
    }
    let coded = (self.width(), self.height());
    self.scaled_output.request(size, coded)
  }

  /// Withdraws any standing scaled-output request, returning this
  /// decoder to full coded size from the next frame.
  ///
  /// The refusal road that does not go through
  /// [`Self::request_scaled_output`]: the wrapper refuses a request
  /// placed while a decoded picture is parked, and a refusal has to
  /// mean the same thing there as everywhere else — the session is
  /// returning to full coded size. Without this the old request would
  /// stay armed behind an `Unsupported` answer, and a caller acting on
  /// that answer would resample pictures that were already fitted.
  pub fn cancel_scaled_output(&mut self) {
    self.scaled_output.cancel();
  }

  /// Decoder width in pixels.
  pub fn width(&self) -> u32 {
    self.state.inner.width()
  }

  /// Decoder height in pixels.
  pub fn height(&self) -> u32 {
    self.state.inner.height()
  }

  /// Codec context time base.
  pub fn time_base(&self) -> Rational {
    self.state.inner.time_base()
  }

  /// Frame rate from the codec context, if known.
  pub fn frame_rate(&self) -> Option<Rational> {
    self.state.inner.frame_rate()
  }

  /// Mints a verdict for a road that holds none yet, then routes it.
  ///
  /// The funnel runs **exactly once** here, which is the law the doors
  /// carry: see the invariant on [`software_receive`]. Roads that have
  /// already minted (the receive arm) call [`Self::hw_route`] straight,
  /// and the route never funnels again — a funnel **consumes** the latch
  /// it reads, so a second one would find nothing and report
  /// libavcodec's errno over a refusal already collected.
  fn hw_failure(&self, e: ffmpeg_next::Error, bare: BareVerdict) -> HwRoute {
    self.hw_route(self.hw_exit(Error::Ffmpeg(e)), bare)
  }

  /// How a funnel verdict routes, before the road's own reading of an
  /// unnamed one applies.
  ///
  /// **Exhaustive on purpose, and with no wildcard.** A `_ => false`
  /// stood here and was a hazard rather than a convenience: a future
  /// named verdict that *did* say a backend cannot take the stream would
  /// inherit the silence and be reported plain, which is a bug that
  /// compiles. The match is total over this crate's own error
  /// vocabulary, so adding an arm forces whoever adds it to say how it
  /// routes.
  fn verdict_routing(reason: &Error) -> VerdictRouting {
    match reason {
      // The hardware pool declined the coded surface. Software is not
      // subject to that ceiling, and neither is the next backend.
      Error::HwSurfaceTooLarge(_) => VerdictRouting::CandidateFailed,
      // Software would decode the same oversized frame and be refused
      // by the same ceiling; so would the next backend. A fallback here
      // invites an action that cannot succeed.
      Error::FrameBudgetExceeded(_) => VerdictRouting::Direct,
      // The funnel handed its fallback straight back: nothing was named,
      // so the errno is all there is and the road decides.
      Error::Ffmpeg(_) => VerdictRouting::Unnamed,
      // None of these can leave a funnel — `hw_exit` mints only the two
      // refusals above or returns its argument — and each is already a
      // decided fact that did not ask for a backend to be retried. They
      // are listed rather than swept up so a new arm cannot join them
      // silently.
      Error::PacketBuild(_)
      | Error::ParametersTooLarge(_)
      // A malformed channel layout is a fact about the *stream*, not
      // about a backend: every backend would be handed the same
      // parameters and refuse them the same way, and the refusal
      // happens before any backend is chosen at all.
      | Error::MalformedChannelLayout(_)
      | Error::ChannelMapMissing(_)
      | Error::NoCodec(_)
      | Error::HwTransferTooLarge(_)
      | Error::BackendUnsupportedByCodec(_)
      | Error::HwDeviceInitFailed(_)
      | Error::AllBackendsFailed(_)
      | Error::FallbackFailed(_) => VerdictRouting::Direct,
    }
  }

  /// **One policy for what a funnelled hardware failure means, shared by
  /// every road that can produce one.**
  ///
  /// The send roads used to return their funnel's result the moment they
  /// had it. That was right for a flow signal and wrong for anything
  /// else: `hw_send` can mint [`Error::HwSurfaceTooLarge`], and returning
  /// it plain meant a probe still auditioning never advanced past the
  /// candidate that had just declined the surface.
  ///
  /// So minting and routing are one move now, and the receive road's
  /// policy is the policy. What differs between roads is only what an
  /// *unnamed* verdict means, which is why [`BareVerdict`] is a
  /// parameter rather than an assumption.
  ///
  /// Only a running probe acts on what a verdict says. While a candidate
  /// is on trial, a verdict that fails it advances the probe. Once a
  /// backend has committed, every verdict is that picture's own error,
  /// reported as it was minted: nothing is classified, nothing is
  /// remembered, and the next call reaches libavcodec. FFmpeg gives no
  /// reliable signal that a hardware session is gone — see the type's
  /// documentation.
  fn hw_route(&self, reason: Error, bare: BareVerdict) -> HwRoute {
    let candidate_failed = match Self::verdict_routing(&reason) {
      VerdictRouting::CandidateFailed => true,
      VerdictRouting::Direct => false,
      VerdictRouting::Unnamed => matches!(bare, BareVerdict::CandidateFailure),
    };
    if candidate_failed && self.probe.is_some() {
      // **Where a backend's answer is made, on every road a call answers
      // by**: `reason` is the funnel's verdict on the candidate's own call.
      return HwRoute::Advance(BackendAnswer::of_call(reason));
    }
    HwRoute::Report(reason)
  }

  /// **Where this session is, derived here and nowhere else.**
  ///
  /// The two latches this reads — whether a backend is still on trial,
  /// and whether an end has been recorded — are the only inputs any
  /// classification question has ever needed. Reading them at the point
  /// of a decision is what let the roads disagree; reading them once,
  /// here, is what stops it.
  pub(crate) const fn phase(&self) -> SessionPhase {
    match (self.probe.is_some(), self.eof_sent) {
      (false, false) => SessionPhase::Streaming,
      (false, true) => SessionPhase::Draining,
      (true, false) => SessionPhase::Auditioning,
      (true, true) => SessionPhase::AuditioningPastEnd,
    }
  }

  /// Submits a packet to the decoder — the **safe** road, over the
  /// crate's own owned packet carrier.
  ///
  /// The packet's bytes are this crate's: they reach libavcodec as an
  /// `AVPacket` built from the carrier on the scoped submission road
  /// ([`boundary::with_ffmpeg_video_packet`](crate::boundary::with_ffmpeg_video_packet)),
  /// lent for this call alone, so nothing a caller holds can write
  /// through what the decoder reads. Everything else — back pressure,
  /// the probe's rescue history, the verdicts — is
  /// [`Self::send_av_packet`]'s, which this hands the built packet to.
  ///
  /// A packet of `ffmpeg_next`'s goes through [`Self::send_av_packet`],
  /// which is `unsafe`: a borrow of one cannot vouch that its payload
  /// was ever written.
  ///
  /// A packet whose payload cannot be built — over the packet limit, or
  /// side data that cannot be attached — is refused as
  /// [`Error::PacketBuild`] before anything is sent.
  pub fn send_packet(&mut self, packet: &crate::OwnedVideoPacket) -> Result<Sent> {
    let limits = self.frame_limits.packet_limits();
    let send = |av_packet: &Packet| {
      // SAFETY: `av_packet` was built by this crate's own builder from
      // `packet`'s owned carrier, so its payload and side data are
      // written, and the owned lane always copies the body into a fresh
      // `av_new_packet` allocation, whose padding is zeroed; it is lent
      // for this call only and reachable by no caller, so nothing writes
      // through it while libavcodec, which never writes through a packet
      // it is sent, may hold a reference.
      unsafe { self.send_av_packet(av_packet) }
    };
    // SAFETY: `send` hands the packet to `send_av_packet`, which gives it
    // to libavcodec and to the probe, whose history keeps deep copies
    // (`boundary::try_clone_packet`); neither clones it shallowly nor
    // keeps it, so a shared body never outlives this call.
    unsafe {
      crate::boundary::with_ffmpeg_video_packet::<crate::Owned, _>(
        packet,
        limits,
        crate::BodyRoute::Submission,
        send,
      )
    }
    .map_err(Error::PacketBuild)?
  }

  /// Submits an `ffmpeg_next` packet to the decoder — the raw road.
  ///
  /// **`unsafe`, because a borrow of an `ffmpeg_next` packet cannot vouch
  /// for its bytes**: `Packet::new` leaves the payload unwritten, and
  /// both libavcodec and the probe's copy read every byte of it. A call
  /// without `unsafe` does not compile:
  ///
  /// ```compile_fail,E0133
  /// fn send(decoder: &mut mediadecode_ffmpeg::VideoDecoder, packet: &ffmpeg_next::Packet) {
  ///   let _ = decoder.send_av_packet(packet);
  /// }
  /// ```
  ///
  /// On success — and only on success — the packet is buffered for potential
  /// replay through a fallback backend while the probe is active. `EAGAIN`
  /// (the decoder needs `receive_frame` to drain output first) is
  /// [`Sent::MustDrain`]: nothing was consumed, so the caller drains and
  /// offers the same packet again. `AVERROR_EOF` is **not** back pressure
  /// on this face — it means this decoder was already told the stream
  /// ended — so it stays a fault. See [`send_status`].
  ///
  /// While the probe is active, a non-transient error (e.g. the active HW
  /// backend rejecting this stream's geometry on first packet) advances the
  /// probe to the next candidate and retries the packet there. The caller
  /// observes only the eventual success or, if the probe is exhausted, the
  /// final error.
  ///
  /// **Atomic probe rescue.** While the probe is active, the rescue
  /// invariant is that everything FFmpeg has consumed since open is
  /// reflected in `buffered_packets` (so a future
  /// [`Error::AllBackendsFailed`] can hand a complete replay history
  /// back to the caller for software fallback on a non-seekable input).
  /// The post-probe path (after the first frame, when `self.probe` is
  /// `None`) skips this pre-flight entirely.
  ///
  /// **A packet the history cannot keep is the trial's last**, and no
  /// refusal: one past the probe's ceiling ([`MAX_PROBE_PACKETS`],
  /// [`MAX_PROBE_PACKET_BYTES`]), one of more side-data entries than the
  /// probe copies ([`MAX_PROBE_PACKET_SIDE_DATA_ENTRIES`]), or one whose
  /// copy cannot be made (`av_packet_ref` or `av_packet_make_writable`
  /// failing). These are the wrapper's own limits, never the backend's
  /// answer ([`Unkept`]), so the packet is sent to the candidate on trial
  /// with the history whole. Taken, the candidate is committed — the
  /// invariant holds, since no probe outlives a packet it does not hold —
  /// and a failure after it is that decoder's own error, with no history
  /// to replay. Refused, the candidate fails as on any packet, and the next
  /// backend is tried with the history whole and offered this packet in its
  /// turn.
  ///
  /// After commit a failure is this packet's own error, reported as it
  /// was minted, and the next packet reaches libavcodec — nothing is
  /// remembered from one call to the next. See the type's documentation.
  ///
  /// # Safety
  ///
  /// `packet` must meet [the `AVPacket`
  /// contract](crate::contracts#avpacket) and, with it, [every exported
  /// byte initialised](crate::contracts#every-exported-byte-initialised):
  /// its payload and each side-data entry's bytes were written — true of
  /// a packet `av_read_frame`, an encoder or `Packet::copy` filled, and
  /// not of a `Packet::new` or `Packet::grow` its caller has not written
  /// over.
  ///
  /// The `AV_INPUT_BUFFER_PADDING_SIZE` bytes after its payload must lie
  /// inside its buffer and be initialised to zero — or be the padding
  /// libavformat delivered it with, unmodified — because libavcodec's
  /// bitstream readers read past the end of what they are given. A packet
  /// `av_new_packet` allocated (`Packet::new`, `Packet::copy`) has zeroed
  /// padding, and one `av_read_frame` delivered has libavformat's; see
  /// [the `AVPacket` contract](crate::contracts#avpacket) for both.
  ///
  /// And nothing may write through its payload or that padding while this
  /// decoder may still read them: `avcodec_send_packet` keeps a reference
  /// to a refcounted packet's buffer past the call (`libavcodec/avcodec.h`
  /// 2333–2335 in FFmpeg 9.0.1), and a frame-threaded decoder reads it
  /// on another thread. A caller that keeps the packet does not write
  /// through it — no `Packet::data_mut`, no in-place write — until this
  /// decoder is flushed or dropped.
  pub unsafe fn send_av_packet(&mut self, packet: &Packet) -> Result<Sent> {
    // **A submission after the end is the caller's fault**, whether or not
    // the decoder has yet been handed that end: one still owed behind a
    // replay is the caller's all the same.
    if self.end_owed {
      return Err(Error::Ffmpeg(ffmpeg_next::Error::Eof));
    }
    loop {
      // Re-read each iteration: a probe advance moves this session from
      // one phase to another underneath the loop.
      let phase = self.phase();
      // **What the decoder is owed goes first**, in order: a replay that
      // stopped at the candidate's first output, or a backlog a commit
      // left. While it cannot all be taken, this packet waits.
      if let Some(route) = self.feed_owed() {
        match route {
          Owed::Taken => {}
          Owed::Full => return Ok(Sent::MustDrain),
          Owed::Report(err) => return Err(err),
          Owed::Advance(answer) => {
            self.advance_probe(answer)?;
            continue;
          }
        }
      }
      // Pre-flight while probe is active: prove we can record this
      // packet for replay BEFORE the active decoder consumes it.
      // `staged` carries what the probe does with the packet through the
      // send below — records its copy, or ends the trial on it — and is
      // acted on only if FFmpeg accepts the packet.
      let staged: Option<Staged> = if let Some(probe) = self.probe.as_ref() {
        // **Where the history cannot keep the packet, the trial ends on
        // this packet's answer, and never before it** ([`Unkept`]: the
        // wrapper's own limits, never a backend's answer). The candidate
        // is sent it with the history whole: taken, it is committed below,
        // no failure claimed for a backend that has failed nothing;
        // refused, it fails as on any packet, and the next backend is tried
        // with the history as it stands and offered the packet in its turn.
        //
        // Step 1: side-data entry count cap. Read just `side_data_elems`
        // (no array walk yet) so a corrupt or weaponised value cannot
        // drive an unbounded loop from the safe entry point; such a packet
        // is never copied.
        let side_count = packet_side_data_count(packet);
        if side_count > MAX_PROBE_PACKET_SIDE_DATA_ENTRIES {
          Some(Staged::Last(Unkept::SideData {
            entries: side_count,
          }))
        } else {
          // Step 2: the probe's ceiling, in packets and in bytes.
          // `packet_side_data_bytes` clamps its walk to
          // MAX_PROBE_PACKET_SIDE_DATA_ENTRIES as defense-in-depth even
          // though the count check above already bounded the array length.
          let pkt_size = packet.size().saturating_add(packet_side_data_bytes(
            packet,
            MAX_PROBE_PACKET_SIDE_DATA_ENTRIES,
          ));
          let new_count = probe.buffered_packets.len() + 1;
          let new_bytes = probe.buffered_bytes.saturating_add(pkt_size);
          if new_count > MAX_PROBE_PACKETS || new_bytes > MAX_PROBE_PACKET_BYTES {
            Some(Staged::Last(Unkept::Ceiling {
              packets: new_count,
              bytes: new_bytes,
            }))
          } else {
            // Step 3: copy before consuming. The history goes to the
            // caller as owned, mutable `Packet`s, so it keeps a payload of
            // its own: a reference to this one would carry the caller's
            // later writes into the history, and `data_mut` on the two
            // would alias. The copy is the size step 2 charged — payload
            // and side data — so `new_bytes` stands.
            // SAFETY: `packet` meets the `AVPacket` contract with every
            // byte written — this function's own contract, forwarded.
            match unsafe { crate::boundary::try_clone_packet(packet) } {
              Ok(c) => Some(Staged::Recorded(c, new_bytes)),
              Err(e) => Some(Staged::Last(Unkept::Copy(e))),
            }
          }
        }
      } else {
        None
      };

      match self.state.inner.send_packet(packet) {
        Ok(()) => {
          // Probe is still Some here: the only paths that take it are the
          // bailouts above (which return) and `advance_probe`'s exhaustion
          // (which would have propagated via `?`).
          match staged {
            // Record the clone now that FFmpeg has accepted the packet:
            // the candidate on trial has been fed the whole history, this
            // packet included — what it was owed went first, above.
            Some(Staged::Recorded(cloned, new_bytes)) => {
              if let Some(probe) = self.probe.as_mut() {
                probe.buffered_packets.push(cloned);
                probe.fed = probe.buffered_packets.len();
                probe.buffered_bytes = new_bytes;
              }
            }
            // The trial's last packet, taken: the candidate is committed,
            // and a failure after it is that decoder's own — there is no
            // history to replay, since the history could not keep this.
            Some(Staged::Last(unkept)) => {
              tracing::info!(
                backend = ?self.state.backend,
                reason = %unkept,
                "hwdecode: the candidate took a packet the probe could not keep; committed"
              );
              self.commit();
            }
            None => {}
          }
          return Ok(Sent::Accepted);
        }
        // **libavcodec's send-side flow control, guarded here and read
        // through the funnel — the same door the software road uses.**
        //
        // The guard and the classification are two different questions
        // and they get two different answers. `is_transient` decides
        // *whether the probe may advance*: neither `EAGAIN` nor
        // `AVERROR_EOF` is a candidate failing, so both are taken here,
        // ahead of the failure road, exactly as this one guard always
        // took them. `send_status` then decides *which* of the two this
        // is — back pressure or the double-EOF fault — and that
        // decision is not written here at all, so this arm cannot drift
        // away from the road the software decoders take. The staged
        // clone drops; the caller drains and re-offers, and we re-clone
        // at the top of the loop.
        //
        // It reads the errno through [`Self::hw_send`] rather than
        // classifying it raw: a refusal this crate latched during the
        // submission — `get_format` declining a coded surface as the
        // decoder configures on its first packet — must be what the
        // caller is told, not the `EAGAIN` libavcodec reported over the
        // top of it.
        // **Mint, then route — not mint and return.** A flow signal
        // leaves immediately; anything else is a verdict, and a verdict
        // that names a declined surface has to reach a running probe,
        // which advances on it. `BareVerdict::Reported`
        // is the road's own reading of an *unnamed* verdict here: the
        // double-EOF is the caller's fault, not the candidate's, so the
        // probe must not advance on it.
        Err(e) if is_transient(&e) => match self.hw_send(e, phase) {
          Ok(status) => return Ok(status),
          Err(reason) => match self.hw_route(reason, BareVerdict::Reported) {
            HwRoute::Report(err) => return Err(err),
            HwRoute::Advance(err) => {
              self.advance_probe(err)?;
              continue;
            }
          },
        },
        // A real failure. Minted once and routed by the shared policy:
        // while a candidate is on trial this is that candidate failing,
        // so `advance_probe` consumes the reason into `attempts` and
        // either installs the next candidate or surfaces
        // `AllBackendsFailed`. Any staged clone drops without entering
        // history; the next iteration clones afresh.
        Err(e) => match self.hw_failure(e, BareVerdict::CandidateFailure) {
          HwRoute::Report(err) => return Err(err),
          HwRoute::Advance(err) => {
            self.advance_probe(err)?;
            continue;
          }
        },
      }
    }
  }

  /// Signal end-of-stream to the decoder.
  ///
  /// Recorded for replay only if the underlying `send_eof` succeeds. While
  /// the probe is active, non-transient errors trigger probe advance and
  /// retry, matching `send_packet`'s behaviour.
  ///
  /// Answers [`Sent::MustDrain`] on `EAGAIN` — the end-of-stream was
  /// **not** recorded, so drain and signal again. A second EOF is a
  /// caller fault and stays one; see [`send_status`].
  ///
  /// After commit a failure here is reported as it was minted, as on
  /// [`Self::send_packet`], and nothing is remembered.
  pub fn send_eof(&mut self) -> Result<Sent> {
    // A second end, though the decoder is still owed the first: the
    // caller's fault, as libavcodec answers a second flush packet.
    if self.end_owed {
      return Err(Error::Ffmpeg(ffmpeg_next::Error::Eof));
    }
    loop {
      // Re-read each iteration: a probe advance moves this session from
      // one phase to another underneath the loop.
      let phase = self.phase();
      // What the decoder is owed goes first, as on `send_packet`.
      if let Some(route) = self.feed_owed() {
        match route {
          Owed::Taken => {}
          Owed::Full => return Ok(Sent::MustDrain),
          Owed::Report(err) => return Err(err),
          Owed::Advance(answer) => {
            self.advance_probe(answer)?;
            continue;
          }
        }
      }
      match self.state.inner.send_eof() {
        Ok(()) => {
          self.eof_sent = true;
          return Ok(Sent::Accepted);
        }
        // The same guard, the same door and the same routing as
        // `send_packet`; see the note there.
        Err(e) if is_transient(&e) => match self.hw_send(e, phase) {
          Ok(status) => return Ok(status),
          Err(reason) => match self.hw_route(reason, BareVerdict::Reported) {
            HwRoute::Report(err) => return Err(err),
            HwRoute::Advance(err) => {
              self.advance_probe(err)?;
              continue;
            }
          },
        },
        // The same shared policy; see `send_packet`.
        Err(e) => match self.hw_failure(e, BareVerdict::CandidateFailure) {
          HwRoute::Report(err) => return Err(err),
          HwRoute::Advance(err) => {
            self.advance_probe(err)?;
            continue;
          }
        },
      }
    }
  }

  /// Receive a CPU-side decoded frame.
  ///
  /// The frame is downloaded with `av_hwframe_transfer_data` and metadata
  /// is copied via `av_frame_copy_props`. The caller's frame is always
  /// unref'd first, so reuse across resolution changes or different
  /// decoders is safe.
  ///
  /// While the probe window is open, the candidate's own failure (a decode
  /// error, a download error, or a CPU-format frame from a HW-opened
  /// context) tears down the current decoder and advances to the next
  /// hardware backend in probe order, replaying buffered packets through
  /// it — up to the candidate's first output, the rest fed as the caller
  /// drains it, so the caller never loses initial frames after a fallback
  /// and no frame is queued meanwhile.
  ///
  /// **The first picture commits the candidate, before any limit of this
  /// crate's is applied to it.** A picture whose download would cost more
  /// than the caller's frame limit ([`Error::HwTransferTooLarge`]), or one
  /// whose properties this crate cannot copy, is reported as a committed
  /// decoder's error: the limit and the copy are this crate's, never the
  /// backend's answer, so neither advances the probe nor enters its
  /// record.
  ///
  /// This crate is hardware-only: there is no software fallback inside the
  /// decoder. When every backend in the probe order has been exhausted —
  /// including the case of a single-backend platform whose only backend
  /// failed — this returns [`Error::AllBackendsFailed`] with the per-
  /// backend attempt log so the caller can branch into a software
  /// decoder of their choice.
  ///
  /// After commit a failure is that picture's own error, reported as it
  /// was minted, and nothing is remembered: the next call reaches
  /// libavcodec. See the type's documentation.
  ///
  /// Answers the same three states `ffmpeg::decoder::Video` does, in
  /// the shape the trait tier publishes: [`Received::NeedsInput`] where
  /// libavcodec says `EAGAIN`, [`Received::Ended`] where it says `EOF`,
  /// and [`Received::Produced`] when `frame` was written. **The errno
  /// stops here** — the two flow signals never leave this crate as
  /// `Error::Ffmpeg`, so a caller has nothing to decode.
  pub fn receive_frame(&mut self, frame: &mut Frame) -> Result<Received> {
    loop {
      // Re-read each iteration: a probe advance moves this session from
      // one phase to another underneath the loop.
      let phase = self.phase();
      let res = self.state.inner.receive_frame(&mut self.hw_frame);
      match res {
        // **A decoder that wants input is fed what it is owed first**: a
        // replay that stopped at a candidate's first output, a backlog a
        // commit left, an end still owed. Only once nothing is owed, or the
        // decoder takes none of it, does its `EAGAIN` mean what the phase
        // says it means.
        Err(e) if is_eagain(&e) && self.owes() => match self.feed() {
          Ok(Fed { taken, .. }) if taken > 0 => continue,
          Ok(_) => match self.hw_receive(e, phase) {
            Ok(status) => return Ok(status),
            Err(reason) => match self.hw_route(reason, BareVerdict::CandidateFailure) {
              HwRoute::Report(err) => return Err(err),
              HwRoute::Advance(answer) => {
                self.advance_probe(answer)?;
                continue;
              }
            },
          },
          Err(refused) => match self.hw_failure(refused, BareVerdict::CandidateFailure) {
            HwRoute::Report(err) => return Err(err),
            HwRoute::Advance(answer) => {
              self.advance_probe(answer)?;
              continue;
            }
          },
        },
        Err(e) => {
          // **The phase decides whether this errno is a protocol state
          // at all, and this arm holds no opinion of its own.**
          //
          // `EAGAIN` used to short-circuit here unconditionally, which
          // was right for three of the four phases and quietly wrong for
          // the fourth: a candidate that has been replayed the whole
          // history *including the end* and still answers "nothing yet"
          // has broken the drain, since libavcodec answers a decoder
          // drained to its end with `AVERROR_EOF`. Answering the caller
          // `NeedsInput` there asked for input nothing could supply;
          // answering `Ended` would have taken a broken drain for a
          // finished one. It is a candidate failing, and the classifier
          // says so by handing it back — straight into the probe road
          // below, which is where candidate failures have always gone.
          //
          // **And it reads the errno through the funnel, which is the
          // law this road lost and got back.** A `get_format`
          // declination or an allocator-judge refusal sits in the
          // callback state waiting to be collected; classifying the raw
          // errno first answers `Ended` or `NeedsInput` for a frame this
          // crate itself declined, and the reason dies unread. The
          // funnel is no longer a step to remember — [`Self::hw_receive`]
          // is the only way in, and the classifiers are private to this
          // module so no road can take a shortcut past it.
          let reason = match self.hw_receive(e, phase) {
            Ok(status) => return Ok(status),
            // The funnel's verdict: the latched refusal when there was
            // one, the original error when there was not. It travels
            // onward as it is — rebuilding `Error::Ffmpeg(e)` here would
            // throw away the collection that just happened.
            Err(reason) => reason,
          };
          // **A clean end is a successful empty completion**, at every
          // skip level and for every codec. The candidate took the end and
          // drained to libavcodec's own `AVERROR_EOF`, no refusal latched
          // (a refusal would be the funnel's verdict instead): it did all
          // it was asked and failed nothing, so it is committed, and the
          // stream has ended. No skip level, flag or byte is read to doubt
          // it — a stream may give no picture for reasons none of them
          // foretells: hidden frames, pre-roll, a level the codec honours.
          if phase == SessionPhase::AuditioningPastEnd
            && matches!(reason, Error::Ffmpeg(ffmpeg_next::Error::Eof))
          {
            tracing::info!(
              backend = ?self.state.backend,
              "hwdecode: a clean end with no picture; committed"
            );
            self.commit();
            return Ok(Received::Ended);
          }
          // **The same shared policy every hardware road uses.** This
          // road mints its own verdict (above), so it routes rather than
          // minting again. `CandidateFailure` is its reading of an
          // unnamed verdict: what reaches here unnamed past the clean end
          // above — `EAGAIN` after the end, a decode error, or an
          // `AVERROR_EOF` before the end was taken, which libavcodec does
          // not give — is the candidate failing.
          match self.hw_route(reason, BareVerdict::CandidateFailure) {
            HwRoute::Report(err) => return Err(err),
            HwRoute::Advance(answer) => {
              self.advance_probe(answer)?;
              continue;
            }
          }
        }
        Ok(()) => {
          // Always attempt the HW→CPU transfer. With strict `get_format`,
          // libavcodec can only deliver frames in the wired-up HW format
          // (or fail). If a misbehaving codec ever hands us a CPU-side
          // frame anyway, `av_hwframe_transfer_data` returns AVERROR(EINVAL)
          // (neither src nor dst has an AVHWFramesContext attached) and we
          // route through the same error path below.
          // **The transfer is priced before it is paid, and a refusal
          // here is final.** See [`judge_hw_transfer`]: neither ceiling
          // hook reaches this allocation — `hwaccel->alloc_frame`
          // bypasses `get_buffer2` entirely, and the CPU destination is
          // allocated by `av_hwframe_transfer_data` outside both — so
          // this judge is what bounds what the hardware road hands back.
          //
          // Judged out here rather than inside `transfer_hw_frame`
          // deliberately. Errors from that function are FFmpeg's, and
          // the arms below route them as hardware failures — a candidate
          // failing while the probe runs, and after commit the picture's
          // own error. A byte ceiling is not a hardware failure: software
          // would decode the same oversized frame and be refused again,
          // so routing it as one is exactly the wrong answer. The named
          // refusal returns straight to the caller.
          //
          // **And a picture came, so the candidate is committed first.**
          // The limit is this crate's, never the backend's answer: a
          // candidate whose picture the ceiling refuses has decoded it,
          // and is the session's decoder from here, the refusal reported
          // as a committed decoder's.
          if let Err(e) =
            unsafe { judge_hw_transfer(self.hw_frame.as_ptr(), self.frame_limits.frame()) }
          {
            self.commit();
            return Err(Error::HwTransferTooLarge(e));
          }
          // **The scaled-output stage, and the one place it sits.** A
          // standing request turns this into a GPU resize followed by a
          // download of the *fitted* surface; with no request, or on any
          // condition the stage cannot honor, `stage` answers `None` and
          // the full-size hardware frame is downloaded exactly as
          // before. It cannot fail — see [`crate::vtscale`].
          //
          // The byte ceiling above is deliberately **not** re-priced
          // against the fitted size. `judge_hw_transfer` bounds what
          // this road may allocate, and pricing the full-size frame is
          // the conservative reading of that bound: a stream refused
          // without scaled output is refused identically with it, so
          // turning the stage on can never widen what the ceiling lets
          // through. What it saves is the bus traffic and the CPU
          // allocation actually paid, which is the trade #55 is about.
          //
          // **And a fitted surface that will not download is the
          // stage's failure, not the backend's.** The scale can succeed
          // and the crossing still fail — an unsupported destination
          // pixel format, a metadata copy that runs out of memory — and
          // routing that into the arms below would let an optional
          // bandwidth optimisation reject a VideoToolbox decode that
          // was working, or cost the caller a picture the hardware
          // decoded. So the fitted attempt is made first and
          // separately: if it fails, the stage latches the key off, the
          // destination is reset, and the original full-size frame —
          // still live in `hw_frame`, still the path this crate took
          // before any of this existed — is downloaded instead. Only
          // *that* failing is a hardware failure.
          let scaled = self
            .scaled_output
            .stage(&self.hw_frame)
            .map(|fitted| unsafe { transfer_hw_frame(frame, fitted) });
          match scaled {
            Some(Ok(())) => {
              self.commit();
              return Ok(Received::Produced);
            }
            Some(Err(e)) => {
              tracing::warn!(
                error = %e,
                "hwdecode: the fitted surface would not download; retiring scaled output for \
                 this size and delivering the full-size frame instead"
              );
              self.scaled_output.latch_failure();
              // SAFETY: `frame` is the caller's slot; unreferencing it
              // discards whatever the failed transfer left behind
              // before the retry writes it again, and leaves an empty
              // frame whose geometry describes no buffer — the shape
              // `as_inner_mut`'s contract allows.
              unsafe { av_frame_unref(frame.as_inner_mut().as_mut_ptr()) };
            }
            None => {}
          }
          match unsafe { transfer_hw_frame(frame, &self.hw_frame) } {
            Ok(()) => {
              self.commit();
              return Ok(Received::Produced);
            }
            Err(TransferFailure::Download(e)) => {
              // The same shared policy. A download failure is the
              // backend's own answer — an unsupported CPU pix_fmt surfaces
              // as `AVERROR(EINVAL)`, a context loss as Bug/Bug2/Unknown —
              // never input corruption, so while a candidate is on trial
              // it is that candidate failing.
              match self.hw_failure(e, BareVerdict::CandidateFailure) {
                HwRoute::Report(err) => return Err(err),
                HwRoute::Advance(answer) => {
                  self.advance_probe(answer)?;
                  // SAFETY: as above — the caller's slot, cleared whole.
                  unsafe { av_frame_unref(frame.as_inner_mut().as_mut_ptr()) };
                  continue;
                }
              }
            }
            // **The picture came and downloaded; what failed is this
            // crate's own copy of its properties.** No backend's answer:
            // the candidate is committed, and the failure surfaced as a
            // committed decoder's.
            Err(TransferFailure::Properties(e)) => {
              self.commit();
              return Err(Error::Ffmpeg(e));
            }
          }
        }
      }
    }
  }

  /// **Commits the candidate on trial**: the probe ends, and what its
  /// history still owed the candidate — the packets a replay stopped short
  /// of, at the candidate's first output — moves to the backlog, fed before
  /// anything new ([`Self::feed`]). A no-op for a committed decoder.
  fn commit(&mut self) {
    if let Some(mut probe) = self.probe.take() {
      let fed = probe.fed.min(probe.buffered_packets.len());
      self.backlog.extend(probe.buffered_packets.drain(fed..));
    }
  }

  /// Whether the decoder is owed anything: the history's unfed packets
  /// while a candidate is on trial, the backlog once committed, or the end.
  fn owes(&self) -> bool {
    self
      .probe
      .as_ref()
      .is_some_and(|probe| probe.fed < probe.buffered_packets.len())
      || !self.backlog.is_empty()
      || self.end_owed
  }

  /// Feeds the active decoder, in order, what it is owed: the history's
  /// unfed packets while it is on trial — the history keeps them for a
  /// later advance, and the decoder takes a reference to each, no copy —
  /// then the backlog once it is committed, each packet dropped once
  /// taken; then the end, where the caller's was not yet passed on. Stops
  /// where the decoder first answers `EAGAIN`, having output for the
  /// caller first.
  ///
  /// A refusal is the decoder's own answer, returned raw for the road to
  /// read through its funnel, and that packet is not sent to it again:
  /// libavcodec queues a packet before decoding it (`avcodec_send_packet`,
  /// `libavcodec/decode.c` 745–758 in FFmpeg 9.0.1), so an error out of the
  /// decode answers a packet the decoder took. A candidate's stays in the
  /// history, whole for the next candidate, which is replayed it from the
  /// start; a committed decoder's is that packet's own error, and is
  /// dropped.
  fn feed(&mut self) -> std::result::Result<Fed, ffmpeg_next::Error> {
    let mut taken = 0;
    if let Some(probe) = self.probe.as_mut() {
      while let Some(packet) = probe.buffered_packets.get(probe.fed) {
        match self.state.inner.send_packet(packet) {
          Ok(()) => {
            probe.fed += 1;
            taken += 1;
          }
          Err(e) if is_eagain(&e) => return Ok(Fed { taken, all: false }),
          Err(e) => {
            probe.fed += 1;
            return Err(e);
          }
        }
      }
    }
    while let Some(packet) = self.backlog.front() {
      match self.state.inner.send_packet(packet) {
        Ok(()) => {
          self.backlog.pop_front();
          taken += 1;
        }
        Err(e) if is_eagain(&e) => return Ok(Fed { taken, all: false }),
        Err(e) => {
          self.backlog.pop_front();
          return Err(e);
        }
      }
    }
    if self.end_owed {
      match self.state.inner.send_eof() {
        Ok(()) => {
          self.end_owed = false;
          taken += 1;
        }
        Err(e) if is_eagain(&e) => return Ok(Fed { taken, all: false }),
        Err(e) => {
          if self.probe.is_none() {
            self.end_owed = false;
          }
          return Err(e);
        }
      }
    }
    Ok(Fed { taken, all: true })
  }

  /// [`Self::feed`], where anything is owed, with a refusal routed as any
  /// send's: `None` where nothing is owed.
  fn feed_owed(&mut self) -> Option<Owed> {
    if !self.owes() {
      return None;
    }
    Some(match self.feed() {
      Ok(Fed { all: true, .. }) => Owed::Taken,
      Ok(Fed { all: false, .. }) => Owed::Full,
      Err(e) => match self.hw_failure(e, BareVerdict::CandidateFailure) {
        HwRoute::Report(err) => Owed::Report(err),
        HwRoute::Advance(answer) => Owed::Advance(answer),
      },
    })
  }

  /// Releases every reference the session holds to the probe's history,
  /// before the history goes to the caller as owned, mutable packets
  /// ([`Error::AllBackendsFailed`]): the active decoder, which a replay fed
  /// the history's own packets, is flushed, and the frame scratch cleared.
  fn release_history(&mut self) {
    self.state.inner.flush();
    // SAFETY: `hw_frame` is a valid AVFrame we own; `av_frame_unref` is a
    // no-op for an already-empty frame.
    unsafe { av_frame_unref(self.hw_frame.as_mut_ptr()) };
  }

  /// Flush internal buffers (e.g. after a seek).
  ///
  /// Discards every frame buffered by the decoder, the residual `hw_frame`
  /// scratch buffer, and whatever the decoder was still owed (the backlog,
  /// an end). Probe-time replay state (buffered packets, the count fed, the
  /// EOF marker) is also cleared since post-seek packets do not align with
  /// the previously captured history. After a flush, the next
  /// `receive_frame` waits for new post-seek input.
  ///
  /// It rebuilds no hardware session. `avcodec_flush_buffers` reaches only
  /// the codec's own flush (`libavcodec/avcodec.c` 417–418 in FFmpeg
  /// 9.0.1), which drops the pictures and references the codec holds. See
  /// the type's documentation for a session FFmpeg leaves without one.
  pub fn flush(&mut self) {
    self.state.inner.flush();
    // SAFETY: hw_frame is a valid AVFrame we own; av_frame_unref is a no-op
    // for an already-empty frame.
    unsafe { av_frame_unref(self.hw_frame.as_mut_ptr()) };
    // The end belongs to the position being abandoned, and so does the
    // history, and what the decoder was owed of it: the probe forgets it
    // whole, every count with it.
    self.backlog.clear();
    self.end_owed = false;
    self.eof_sent = false;
    if let Some(probe) = self.probe.as_mut() {
      probe.forget_history();
    }
  }

  /// Takes the coded-surface refusal the `get_format` callback left
  /// behind, if it left one, clearing it for the next candidate.
  fn take_ceiling_declination(&self) -> Option<Error> {
    // SAFETY: the state is this decoder's own, freed by `DecoderState`'s
    // drop only after the codec context that points at it.
    unsafe { ceiling_declination_of(self.state.callback_state) }
  }

  /// **The single hardware-exit funnel.** Every road that turns a
  /// hardware failure — or an end-of-stream that is really a refusal —
  /// into an `Error` goes through here, and it reads the callback's
  /// declination *before* anything wraps or tears down state.
  ///
  /// The reason there is a funnel at all: a `get_format` callback
  /// cannot return a reason, so it leaves one behind, and every exit
  /// that forgets to collect it hands the caller libavcodec's
  /// `Invalid data found when processing input` for a refusal this
  /// crate made — or, on the explicit-backend road, a stream that
  /// simply drains to EOF with nothing said at all.
  ///
  /// The lesson this encodes: R14 claimed four consumers of the
  /// declination and production had exactly one. Consumers added
  /// helper-by-helper are lost the next time the surrounding code is
  /// restructured; a single funnel that every exit *must* call is the
  /// only version of this that stays true. The per-road table in
  /// `decoder/tests.rs` is what checks that it did.
  /// The hardware road's funnel-and-classify entry — [`software_receive`]'s
  /// twin, and the same law: what a caller reads is what the funnel
  /// found, never the errno that reached it.
  ///
  /// Answers `Err` with the funnel's verdict, which is the latched
  /// refusal when there was one. Callers route *that* value onward
  /// rather than rebuilding the raw error, or the collection is undone
  /// the moment it is used.
  fn hw_receive(&self, e: ffmpeg_next::Error, phase: SessionPhase) -> Result<Received> {
    receive_status(self.hw_exit(Error::Ffmpeg(e)), phase)
  }

  /// The send road's half of [`Self::hw_receive`].
  fn hw_send(&self, e: ffmpeg_next::Error, phase: SessionPhase) -> Result<Sent> {
    send_status(self.hw_exit(Error::Ffmpeg(e)), phase)
  }

  fn hw_exit(&self, fallback: Error) -> Error {
    self
      .take_ceiling_declination()
      // SAFETY: as `take_ceiling_declination` — this decoder's own state,
      // live while `self` is.
      .or_else(|| unsafe { frame_budget_declination_of(self.state.callback_state) })
      .unwrap_or(fallback)
  }

  /// Try the next backend in `remaining_backends`. Transactional: a
  /// candidate must build and take its replay of the history before any
  /// probe state is consumed. A backend that refuses — it will not open, or
  /// it refuses a replayed packet — is recorded into `probe.attempts` with
  /// its own answer, and the loop continues to the next one.
  ///
  /// `answer` is the active candidate's own answer that triggered this
  /// advance ([`BackendAnswer`]) — a send, a receive or a download it
  /// failed. It is recorded against the active backend once the probe
  /// leaves it — a candidate installed, or every backend exhausted — in
  /// the place it was tried, ahead of the candidates that refused after
  /// it, so a final `AllBackendsFailed` carries the full attempt log in
  /// probe order, the initially-opened backend's runtime failure
  /// included.
  ///
  /// **The replay is the history itself.** Each candidate is sent the
  /// history's own packets, by reference ([`feed_into`]): no copy is made,
  /// so nothing of this crate's own can fail a replay, and the history is
  /// released from every decoder before it is handed back
  /// ([`Self::release_history`]). It stops where the candidate first has
  /// output for the caller — it answers `EAGAIN` — and the candidate is
  /// installed with the rest of the history owed, fed before anything new
  /// as the caller drains it ([`Self::feed`]). So no frame is queued, and
  /// no queue can be exhausted.
  ///
  /// Returns:
  /// - `Ok(())` when a candidate is installed — caller should retry the
  ///   operation.
  /// - `Err(Error::AllBackendsFailed(p))` when every remaining backend has
  ///   been exhausted (including the just-failed active one), with
  ///   `p.attempts()` the per-backend answers. On a single-backend
  ///   platform (e.g. macOS) this fires after the only backend's
  ///   first-frame failure; on multi-backend platforms after the last
  ///   candidate's.
  /// - `Err(_)` for a failure of this crate's own — the parameters copy a
  ///   candidate is built from, the context it is built on — or the
  ///   stream's: surfaced as it is, **no backend charged**. The next
  ///   backend is kept in the order and the history whole; the failed
  ///   candidate stays the active one, its answer unrecorded, and what it
  ///   answers next is its own.
  fn advance_probe(&mut self, answer: BackendAnswer) -> Result<()> {
    let build = self
      .probe
      .as_ref()
      .map_or(Self::build_state as BuildCandidate, |probe| probe.build);
    self.advance_probe_with(answer, build)
  }

  /// [`Self::advance_probe`], with the candidate builder named: the
  /// probe's own ([`ProbeState::build`]) on every road that advances.
  ///
  /// [`Self::build_state`] on every production road. A lane passes a
  /// builder that opens the candidate in software, which is how the
  /// replay and the commit below run on a machine with one hardware
  /// backend or none.
  fn advance_probe_with(&mut self, answer: BackendAnswer, build: BuildCandidate) -> Result<()> {
    if self.probe.is_none() {
      // Unreachable: every road advances only while a candidate is on
      // trial (`hw_route`). The answer is reported as it came.
      return Err(answer.into_error());
    }
    // The failed candidate, recorded once the probe leaves it, where it
    // was tried: before any candidate that refuses below.
    let failed = (self.state.backend, answer);
    let tried_at = self.probe.as_ref().map_or(0, |probe| probe.attempts.len());
    // Read before any `probe` borrow: the end is the *decoder's* fact
    // now, not the probe's, and a candidate must be handed it along
    // with the replayed history or it will sit at `EAGAIN` forever on a
    // stream that is already over.
    let eof_sent = self.eof_sent;

    loop {
      let probe = self.probe.as_mut().expect("probe state present");
      let Some(&next_backend) = probe.remaining_backends.first() else {
        // **No more candidates** — surface the attempt log as
        // `AllBackendsFailed`, so single- and multi-backend platforms
        // have the same contract for "every HW backend failed", and hand
        // the history back with it: those packets were consumed from the
        // caller's demuxer, and for a non-seekable input the caller
        // cannot re-demux them. Every decoder that holds a reference to
        // them lets it go first, so the caller's owned, mutable packets
        // share their bytes with nothing.
        let mut probe = self.probe.take().expect("probe state present");
        probe.attempts.insert(tried_at, failed);
        self.release_history();
        return Err(Error::AllBackendsFailed(AllBackendsFailed::new(
          answered(probe.attempts),
          probe.buffered_packets,
        )));
      };
      // **This crate's own copy, and no backend's answer**: a copy that
      // cannot be made is surfaced as it is, `next_backend` kept.
      let parameters = try_clone_parameters(
        &probe.parameters,
        self.frame_limits.max_codec_parameter_bytes(),
      )?;
      let codec = probe.codec;

      let prev_backend = self.state.backend;
      tracing::warn!(from = ?prev_backend, to = ?next_backend, "hwdecode: advancing probe");

      let mut candidate = match build(
        parameters,
        codec,
        next_backend,
        self.frame_limits,
        self.options,
        self.pkt_timebase,
      ) {
        Ok(state) => state,
        Err(BuildFailure::Refused(refusal)) => {
          tracing::warn!(?next_backend, error = ?refusal, "hwdecode: candidate build failed");
          let probe = self.probe.as_mut().expect("probe state present");
          probe.remaining_backends.remove(0);
          probe.attempts.push((next_backend, refusal));
          continue;
        }
        // This crate's own failure, or the stream's: surfaced as it is.
        Err(BuildFailure::Local(e)) => return Err(e),
      };

      // Replay the history through the candidate WITHOUT installing it:
      // the history's own packets, borrowed. If the candidate refuses one,
      // its Drop releases the FFmpeg state, and with it every reference it
      // took, and the history is whole for the next attempt.
      let replay = {
        let probe = self.probe.as_ref().expect("probe state present");
        feed_into(&mut candidate.inner, &probe.buffered_packets, eof_sent)
      };
      match replay {
        Ok(taken) => {
          // Install: the candidate is the active one, still on trial, owed
          // what its replay stopped short of.
          self.state = candidate;
          // **The scaled-output stage's session belongs to the device
          // that just went away.** Its fitted frames context was built over
          // the outgoing backend's hardware device; the caller's standing
          // request outlives the advance, but nothing built for the old
          // device may. Unreachable on the one platform where the stage
          // has a body — `probe_order` names a single backend on Apple
          // targets, so this method never reaches an install there — and
          // written all the same, because "unreachable today" is not a
          // property a resource-owning cache should depend on.
          self.scaled_output.retire();
          // SAFETY: `hw_frame` is a valid AVFrame we own.
          unsafe { av_frame_unref(self.hw_frame.as_mut_ptr()) };
          self.end_owed = eof_sent && !taken.end;
          let probe = self.probe.as_mut().expect("probe state present");
          probe.fed = taken.packets;
          probe.remaining_backends.remove(0);
          probe.attempts.insert(tried_at, failed);
          return Ok(());
        }
        Err(e) => {
          tracing::warn!(?next_backend, error = %e, "hwdecode: candidate replay failed");
          // **The candidate's own refusal, read before the candidate
          // dies**, through its own funnel — not the active backend's,
          // whose callback state is not the one that may have declined. A
          // budget refusal is this crate's own, and surfaced as it is.
          //
          // SAFETY: `candidate` owns this state and is dropped only
          // below, after the reads.
          if let Some(refusal) = unsafe { frame_budget_declination_of(candidate.callback_state) } {
            return Err(refusal);
          }
          // SAFETY: as above.
          let reason =
            unsafe { ceiling_declination_of(candidate.callback_state) }.unwrap_or(Error::Ffmpeg(e));
          drop(candidate);
          let probe = self.probe.as_mut().expect("probe state present");
          probe.remaining_backends.remove(0);
          probe
            .attempts
            .push((next_backend, BackendAnswer::of_call(reason)));
          continue;
        }
      }
    }
  }

  /// Build raw FFmpeg state for one hardware backend. Strict `get_format`
  /// (NONE on missing HW format); cross-backend fallback is the caller's job.
  fn build_state(
    parameters: codec::Parameters,
    codec: Codec,
    backend: Backend,
    limits: crate::limits::DecoderLimits,
    options: crate::DecoderOptions,
    pkt_timebase: Option<mediadecode::Timebase>,
  ) -> std::result::Result<DecoderState, BuildFailure> {
    // Use our checked allocator instead of Context::from_parameters, which
    // does not null-check avcodec_alloc_context3 and would feed a null
    // AVCodecContext into FFmpeg under OOM. Whatever refuses here is this
    // crate's own, or the stream's — the same for every backend.
    let (mut ctx, state) = build_codec_context(&parameters, limits, options, pkt_timebase)
      .map_err(BuildFailure::Local)?;
    let av_type = backend.av_hwdevice_type();

    // Verify the codec advertises this hwaccel **with the exact HW pix_fmt
    // we're about to wire up in `get_format`**. FFmpeg's HW config table
    // is keyed per (device_type, pix_fmt); a codec can advertise the same
    // device with several HW pix_fmts, so matching only on device_type
    // would let probing succeed for a backend whose pix_fmt the codec
    // never offers — the failure would then surface deep inside the
    // probe/decode loop. Matching the exact pix_fmt keeps the strict
    // `get_format` honest and gives `open_with` a clean rejection.
    let hw_pix_fmt = backend.hw_pixel_format();
    // SAFETY: `codec` is the decoder `find_decoder` answered — an entry
    // in libavcodec's static codec table, live for the process.
    if !unsafe { codec_supports_hwaccel(codec.as_ptr(), av_type, hw_pix_fmt as i32) } {
      return Err(BuildFailure::Refused(BackendAnswer::of_open(
        Error::BackendUnsupportedByCodec(backend),
      )));
    }

    // Create the device context.
    let mut hw_device_ref: *mut AVBufferRef = ptr::null_mut();
    // SAFETY: `hw_device_ref` is a stack ptr we hand FFmpeg to fill.
    let ret = unsafe {
      av_hwdevice_ctx_create(&mut hw_device_ref, av_type, ptr::null(), ptr::null_mut(), 0)
    };
    if ret < 0 {
      return Err(BuildFailure::Refused(BackendAnswer::of_open(
        Error::HwDeviceInitFailed(HwDeviceInitFailed::new(
          backend,
          ffmpeg_next::Error::from(ret),
        )),
      )));
    }

    // The state `build_codec_context` already installed in `opaque`,
    // told which format this backend wants. It is written through the
    // very pointer the context holds, before the open, so nothing needs
    // installing again. One allocation serves both callbacks — the
    // budget the judge reads and the declination the funnel reads are
    // the same object — and `into_raw` hands its ownership to the guard
    // below as that same pointer, so the pointer the context holds stays
    // the one that is freed.
    //
    // SAFETY: `state.as_ptr()` is the live state `opaque` holds. The
    // context has not been opened, so no callback reads the state while
    // `set_wanted` writes it, and the `&mut` that call makes is derived
    // from the installed pointer itself, so it does not invalidate it.
    unsafe { (*state.as_ptr()).set_wanted(hw_pix_fmt) };
    let callback_state = state.into_raw();
    // RAII guard: from now until the end-of-function `into_owned()`, every
    // early return — `av_buffer_ref` failure, `open_as` failure, codec_type
    // mismatch, or any future error path added between here and the
    // `DecoderState` construction — frees `hw_device_ref` and
    // `callback_state` via the guard's Drop. Without it, each error site
    // had to remember to clean up these two FFI-owned resources by hand;
    // the codec_type-mismatch branch was missed and silently leaked one
    // device ref + one heap allocation per bad input.
    let guard = PartialBuildState {
      hw_device_ref,
      callback_state,
    };

    // SAFETY: ctx is a freshly-constructed AVCodecContext we own;
    // av_buffer_ref bumps the refcount of the device buffer for FFmpeg's
    // use (we keep our own ref in `hw_device_ref` for cleanup).
    // av_buffer_ref returns NULL on allocation failure; we must check it
    // before assigning, otherwise the codec context would be opened with a
    // HW-flagged setup but no actual device reference.
    let device_ref_for_ctx = unsafe { av_buffer_ref(hw_device_ref) };
    if device_ref_for_ctx.is_null() {
      // guard's Drop frees hw_device_ref (the first ref) and callback_state.
      // This crate's own reference that could not be made: no backend's
      // answer.
      return Err(BuildFailure::Local(Error::Ffmpeg(
        ffmpeg_next::Error::Other {
          errno: libc::ENOMEM,
        },
      )));
    }
    // SAFETY: device_ref_for_ctx is a valid AVBufferRef* from av_buffer_ref;
    // ctx is freshly built, not yet opened, and owned by us. It has
    // aliased `callback_state` through `opaque` since `build_codec_context`
    // installed it (FFmpeg never frees opaque, so ownership stays with the
    // guard), and from here it also aliases `device_ref_for_ctx` (the
    // second ref) via `hw_device_ctx` (FFmpeg unrefs that on codec context
    // drop, independent of the guard's first ref). `get_hw_format` reads
    // the same state, so it is installed before the open as well.
    unsafe {
      let raw = ctx.as_mut_ptr();
      (*raw).hw_device_ctx = device_ref_for_ctx;
      (*raw).get_format = Some(get_hw_format);
    }

    // Open the decoder. On failure `ctx`/`opened` Drop releases the codec
    // context (and via that the second device ref); the guard releases the
    // first device ref and the callback state.
    //
    // We deliberately bypass `Opened::video()` because it calls
    // `Context::medium()`, which reads `AVCodecContext.codec_type` as the
    // bindgen `AVMediaType` enum — the same UB hazard we've been
    // systematically removing. Instead: validate `codec_type` as a raw
    // `c_int` ourselves, then construct the `decoder::Video` wrapper
    // directly via its public tuple field.
    // Through the funnel's free-standing half — there is no decoder yet
    // to ask, and the guard frees the callback state on the way out, so
    // the reason has to be collected here or not at all.
    let opened = match ctx.decoder().open_as(codec) {
      Ok(opened) => opened,
      // SAFETY: `callback_state` is the guard's live state, freed only
      // when the guard drops on this return, after the read.
      // The codec would not open on this backend: its own answer.
      Err(e) => {
        return Err(BuildFailure::Refused(BackendAnswer::of_open(
          unsafe { ceiling_declination_of(callback_state) }.unwrap_or(Error::Ffmpeg(e)),
        )));
      }
    };

    // Validate codec_type as a raw integer — never construct AVMediaType
    // from an unvalidated runtime value. On failure `opened`'s Drop
    // releases the codec context; the guard releases the first
    // hw_device_ref and the callback state.
    if let Err(e) = ensure_video_codec_type(&opened) {
      // Same exit, same collection: a declined format can leave the
      // context looking like the wrong medium, and is the backend's
      // refusal; a context that opened as another medium is the stream's.
      //
      // SAFETY: as above — the guard's live state, read before it drops.
      return Err(match unsafe { ceiling_declination_of(callback_state) } {
        Some(refusal) => BuildFailure::Refused(BackendAnswer::of_open(refusal)),
        None => BuildFailure::Local(e),
      });
    }
    // SAFETY of construction: `decoder::Video` is `pub struct Video(pub Opened)`.
    // We construct via the public field; this is the same wrapping
    // `Opened::video()` does on success, just without the enum read.
    let opened = ffmpeg_next::decoder::Video(opened);

    // Disarm the guard and transfer ownership of both resources into the
    // returned DecoderState (whose own Drop handles their lifetime).
    let (hw_device_ref, callback_state) = guard.into_owned();
    Ok(DecoderState {
      inner: ManuallyDrop::new(opened),
      backend,
      hw_device_ref,
      callback_state,
    })
  }
}

/// RAII guard for the partially-owned FFmpeg state that
/// [`VideoDecoder::build_state`] holds between the
/// `av_hwdevice_ctx_create` allocation, the callback state's hand-off
/// ([`InstalledCallbackState::into_raw`]) and the final `DecoderState`
/// construction.
///
/// If `build_state` returns `Err` for any reason in that window
/// (`av_buffer_ref` ENOMEM, `open_as` failure, codec_type mismatch, or
/// any future error path), this guard's `Drop` releases
/// `hw_device_ref` — the first ref returned by `av_hwdevice_ctx_create`,
/// distinct from the second ref FFmpeg unrefs when the codec context
/// drops — and the `CallbackState`, which FFmpeg never frees because
/// `AVCodecContext::opaque` is purely user-owned.
///
/// The state is freed after the context on every return past the open:
/// `open_as` frees the context itself when it fails, and `opened` is
/// declared after the guard, so it drops first. The `av_buffer_ref`
/// return comes before the open, and there the guard frees the state
/// while the context, never opened, still waits to be freed — the
/// door's never-opened exception (see
/// [`install_allocator_judge`](crate::ffi::install_allocator_judge)).
///
/// Successful construction calls [`Self::into_owned`] to disarm the
/// guard and hand both pointers to the new `DecoderState`.
struct PartialBuildState {
  hw_device_ref: *mut AVBufferRef,
  callback_state: *mut CallbackState,
}

impl PartialBuildState {
  /// Disarm the guard: return the owned pointers and replace the guard's
  /// fields with null so its Drop is a no-op.
  fn into_owned(mut self) -> (*mut AVBufferRef, *mut CallbackState) {
    let hw = std::mem::replace(&mut self.hw_device_ref, ptr::null_mut());
    let cb = std::mem::replace(&mut self.callback_state, ptr::null_mut());
    (hw, cb)
  }
}

impl Drop for PartialBuildState {
  fn drop(&mut self) {
    // SAFETY: pointers are either freshly allocated by `build_state` (via
    // `av_hwdevice_ctx_create`, and `InstalledCallbackState::into_raw`,
    // whose pointer is a `Box::into_raw`'s) or null after
    // `into_owned`. Both `av_buffer_unref` and `Box::from_raw` need the
    // null check we apply here; both are otherwise sound on resources we
    // own.
    unsafe {
      if !self.hw_device_ref.is_null() {
        let mut hw = self.hw_device_ref;
        av_buffer_unref(&mut hw);
      }
      if !self.callback_state.is_null() {
        drop(Box::from_raw(self.callback_state));
      }
    }
  }
}

/// Download a HW frame into a CPU [`Frame`]. Always unrefs the destination
/// first so reuse across resolution changes is safe.
///
/// `src` is whichever surface the scaled-output stage nominated: the
/// decoder's own hardware frame in the ordinary case, or the fitted
/// surface [`crate::vtscale`] scaled it into when a request is standing.
/// Both are VideoToolbox frames carrying their own `AVHWFramesContext`,
/// and the fitted one already carries this frame's metadata — so this
/// function's contract is unchanged either way, and the CPU frame's
/// extent is the nominated surface's.
///
/// Deliberately does **not** call `av_frame_copy_props`. That FFmpeg
/// helper deep-copies AVFrame side data (SEI, mastering display, ICC
/// profiles, dynamic HDR, etc.), the metadata dict, and bumps both
/// `opaque_ref` and `private_ref` on every receive — none of which
/// `Frame` exposes via its public accessors. On a crafted stream with
/// megabytes of per-frame metadata that would mean an unbounded
/// allocation per receive, with no caller-visible benefit. We instead
/// copy only the scalar fields the public API can read (today: `pts`);
/// pixel layout (`width`, `height`, `format`, `linesize`, `data`) is
/// already set by `av_hwframe_transfer_data`. If `Frame` ever grows
/// accessors for timing extras (`duration`, `time_base`, `pkt_dts`) or
/// color metadata, add those to `copy_frame_props_minimal` at the same
/// time.
///
/// Its failures are two, and they are told apart ([`TransferFailure`]):
/// the download — the backend's own call, or the layout it chose — and
/// this crate's copy of the frame's properties after it.
unsafe fn transfer_hw_frame(
  dst: &mut Frame,
  src: &frame::Video,
) -> std::result::Result<(), TransferFailure> {
  // SAFETY (`as_inner_mut`, every use below): `dst` is only cleared
  // whole (`av_frame_unref`), filled whole by `av_hwframe_transfer_data`
  // (which sets its format, dimensions, line sizes and buffers together),
  // read, or given `src`'s timing, colour and crop properties by
  // `copy_frame_props_minimal` — none of them a field `Frame`'s
  // accessors trust.
  unsafe {
    av_frame_unref(dst.as_inner_mut().as_mut_ptr());
    let ret = av_hwframe_transfer_data(dst.as_inner_mut().as_mut_ptr(), src.as_ptr(), 0);
    if ret < 0 {
      return Err(TransferFailure::Download(ffmpeg_next::Error::from(ret)));
    }
    // Validate the post-transfer CPU pix_fmt against the safe `Frame`
    // accessor's supported set. FFmpeg picks the destination format
    // when `dst.format == AV_PIX_FMT_NONE` on entry (which it always is
    // here — `av_frame_unref` clears it) by walking the result of
    // `av_hwframe_transfer_get_formats`. Driver/version ordering can
    // pick a layout outside our NV*/P0xx/P2xx/P4xx set; the call would
    // return success while the resulting frame is unreadable through
    // `Frame::row` / `Frame::as_ptr` (those return `None` for
    // unsupported formats). Surface the unsupported result as a
    // transfer failure so `receive_frame`'s probe-active path advances
    // to the next backend rather than collapsing on an unusable frame;
    // post-probe, the caller gets that picture's error.
    let dst_raw_fmt: i32 = (*dst.as_inner_mut().as_ptr()).format;
    let dst_pix_fmt = crate::boundary::from_av_pixel_format(dst_raw_fmt);
    if !crate::frame::is_supported_cpu_pix_fmt(&dst_pix_fmt) {
      tracing::warn!(
        pix_fmt = dst_raw_fmt,
        "hwdecode: hw->cpu transfer produced unsupported pix_fmt; \
         treating as backend failure"
      );
      av_frame_unref(dst.as_inner_mut().as_mut_ptr());
      return Err(TransferFailure::Download(ffmpeg_next::Error::Other {
        errno: libc::EINVAL,
      }));
    }
    if let Err(e) = copy_frame_props_minimal(dst.as_inner_mut().as_mut_ptr(), src.as_ptr()) {
      // Failed to propagate metadata. Reset the destination so the
      // partial frame doesn't leak (its pixel buffers were attached by
      // `av_hwframe_transfer_data` above) and surface it as this crate's
      // own failure, after a picture: no backend's answer, the caller gets
      // that picture's error from a committed decoder.
      av_frame_unref(dst.as_inner_mut().as_mut_ptr());
      return Err(TransferFailure::Properties(e));
    }
  }
  Ok(())
}

/// Hard cap on the number of `AVFrameSideData` entries we copy from
/// HW source frame to CPU destination frame on the HW transfer
/// path. Mirrors `convert::SIDE_DATA_MAX_ENTRIES`; the public
/// converter re-enforces the same cap so this is defense in depth.
pub(crate) const HW_COPY_SIDE_DATA_MAX_ENTRIES: usize = 64;
/// Hard cap on the total side-data byte budget per HW transfer.
/// Mirrors `convert::SIDE_DATA_MAX_TOTAL_BYTES`.
const HW_COPY_SIDE_DATA_MAX_TOTAL_BYTES: usize = 256 * 1024;

/// Maps a raw `AV_FRAME_DATA_*` integer to the matching bindgen
/// `AVFrameSideDataType` enum value when (and only when) the integer
/// is a known discriminant in the linked FFmpeg's bindgen output.
/// Returns `None` for unknown / version-skew / corrupt values —
/// the caller drops those entries instead of `transmute`-ing an
/// arbitrary integer back into the enum (which would be immediate
/// UB if the discriminant isn't in the enum's set).
///
/// The whitelist covers the entries safe to preserve across HW
/// transfer:
/// - HDR10 / HDR10+ / Dolby Vision / Vivid / ambient HDR metadata
/// - SMPTE / GOP timecodes
/// - ICC color profile
/// - A53 closed captions
/// - Spherical / display matrix orientation
/// - Stereo3D layout
///
/// Other AV_FRAME_DATA_* constants exist (motion vectors, encoder
/// params, RPU buffers, …) but are either decoder-internal or
/// rarely useful through the public mediadecode API; dropping them
/// is the safe default.
pub(crate) fn whitelisted_side_data_kind(
  kind_raw: i32,
) -> Option<ffmpeg_next::ffi::AVFrameSideDataType> {
  use ffmpeg_next::ffi::AVFrameSideDataType;
  // Each match arm compares `kind_raw` against the i32 cast of a
  // known constant, then returns the constant itself — we never
  // construct the enum from arbitrary integer bytes.
  let kind = match kind_raw {
    x if x == AVFrameSideDataType::AV_FRAME_DATA_PANSCAN as i32 => {
      AVFrameSideDataType::AV_FRAME_DATA_PANSCAN
    }
    x if x == AVFrameSideDataType::AV_FRAME_DATA_A53_CC as i32 => {
      AVFrameSideDataType::AV_FRAME_DATA_A53_CC
    }
    x if x == AVFrameSideDataType::AV_FRAME_DATA_STEREO3D as i32 => {
      AVFrameSideDataType::AV_FRAME_DATA_STEREO3D
    }
    x if x == AVFrameSideDataType::AV_FRAME_DATA_DISPLAYMATRIX as i32 => {
      AVFrameSideDataType::AV_FRAME_DATA_DISPLAYMATRIX
    }
    x if x == AVFrameSideDataType::AV_FRAME_DATA_AFD as i32 => {
      AVFrameSideDataType::AV_FRAME_DATA_AFD
    }
    x if x == AVFrameSideDataType::AV_FRAME_DATA_MASTERING_DISPLAY_METADATA as i32 => {
      AVFrameSideDataType::AV_FRAME_DATA_MASTERING_DISPLAY_METADATA
    }
    x if x == AVFrameSideDataType::AV_FRAME_DATA_GOP_TIMECODE as i32 => {
      AVFrameSideDataType::AV_FRAME_DATA_GOP_TIMECODE
    }
    x if x == AVFrameSideDataType::AV_FRAME_DATA_SPHERICAL as i32 => {
      AVFrameSideDataType::AV_FRAME_DATA_SPHERICAL
    }
    x if x == AVFrameSideDataType::AV_FRAME_DATA_CONTENT_LIGHT_LEVEL as i32 => {
      AVFrameSideDataType::AV_FRAME_DATA_CONTENT_LIGHT_LEVEL
    }
    x if x == AVFrameSideDataType::AV_FRAME_DATA_ICC_PROFILE as i32 => {
      AVFrameSideDataType::AV_FRAME_DATA_ICC_PROFILE
    }
    x if x == AVFrameSideDataType::AV_FRAME_DATA_S12M_TIMECODE as i32 => {
      AVFrameSideDataType::AV_FRAME_DATA_S12M_TIMECODE
    }
    x if x == AVFrameSideDataType::AV_FRAME_DATA_DYNAMIC_HDR_PLUS as i32 => {
      AVFrameSideDataType::AV_FRAME_DATA_DYNAMIC_HDR_PLUS
    }
    x if x == AVFrameSideDataType::AV_FRAME_DATA_REGIONS_OF_INTEREST as i32 => {
      AVFrameSideDataType::AV_FRAME_DATA_REGIONS_OF_INTEREST
    }
    x if x == AVFrameSideDataType::AV_FRAME_DATA_SEI_UNREGISTERED as i32 => {
      AVFrameSideDataType::AV_FRAME_DATA_SEI_UNREGISTERED
    }
    x if x == AVFrameSideDataType::AV_FRAME_DATA_FILM_GRAIN_PARAMS as i32 => {
      AVFrameSideDataType::AV_FRAME_DATA_FILM_GRAIN_PARAMS
    }
    x if x == AVFrameSideDataType::AV_FRAME_DATA_DOVI_RPU_BUFFER as i32 => {
      AVFrameSideDataType::AV_FRAME_DATA_DOVI_RPU_BUFFER
    }
    x if x == AVFrameSideDataType::AV_FRAME_DATA_DOVI_METADATA as i32 => {
      AVFrameSideDataType::AV_FRAME_DATA_DOVI_METADATA
    }
    x if x == AVFrameSideDataType::AV_FRAME_DATA_DYNAMIC_HDR_VIVID as i32 => {
      AVFrameSideDataType::AV_FRAME_DATA_DYNAMIC_HDR_VIVID
    }
    x if x == AVFrameSideDataType::AV_FRAME_DATA_AMBIENT_VIEWING_ENVIRONMENT as i32 => {
      AVFrameSideDataType::AV_FRAME_DATA_AMBIENT_VIEWING_ENVIRONMENT
    }
    _ => return None,
  };
  Some(kind)
}

/// Why [`transfer_hw_frame`] failed.
enum TransferFailure {
  /// The download: `av_hwframe_transfer_data` failing, or the CPU layout it
  /// chose being one [`Frame`] cannot carry — the backend's own answer.
  Download(ffmpeg_next::Error),
  /// This crate's copy of the frame's properties, after the download: no
  /// backend's answer.
  Properties(ffmpeg_next::Error),
}

impl core::fmt::Display for TransferFailure {
  fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
    match self {
      Self::Download(e) => write!(f, "the download failed: {e}"),
      Self::Properties(e) => write!(f, "the frame's properties could not be copied: {e}"),
    }
  }
}

/// Copies AVFrame metadata (timestamps, color metadata, crop rect,
/// flags, side data, etc.) from the source HW frame to the destination
/// CPU frame so the post-transfer frame surfaces the same metadata a
/// SW-decoded frame would.
///
/// Defers to FFmpeg's `av_frame_copy_props`, which handles the per-
/// `side_data[i]` allocation, dict copy, and refcounted buffer
/// replacements internally. The cost is bounded by what the source
/// frame attaches — typical HDR streams carry 1–3 side-data entries
/// (mastering display, content light level, dolby/HDR10+ dynamic
/// metadata) totalling a few hundred bytes, so per-frame allocation
/// overhead stays negligible relative to the pixel data already
/// transferred via `av_hwframe_transfer_data`.
///
/// # Safety
/// Both pointers must be valid `AVFrame` pointers we own. We do not
/// form `&AVFrame` — `av_frame_copy_props` operates on raw pointers
/// directly.
pub(crate) unsafe fn copy_frame_props_minimal(
  dst: *mut AVFrame,
  src: *const AVFrame,
) -> std::result::Result<(), ffmpeg_next::Error> {
  // We deliberately do NOT use `av_frame_copy_props` here, despite
  // its convenience. Upstream `av_frame_copy_props` deep-copies
  // *every* `AVFrameSideData` entry, the metadata `AVDictionary`,
  // and refcounted `opaque_ref` / `private_ref` buffers — all from
  // attacker-controlled decoder output. A crafted stream with many
  // multi-MiB side-data entries could drive the per-frame
  // allocation cost arbitrarily high (one alloc per entry, with the
  // entry's bytes copied via `memcpy`). The downstream
  // `convert::collect_side_data` cap helps the *Rust* side but the
  // FFmpeg-side allocations have already happened.
  //
  // Instead we copy scalar fields manually (timestamps, color
  // metadata, picture type, flags) and copy side-data with a hard
  // cap matching the converter's. Metadata dict and opaque_ref /
  // private_ref are intentionally NOT copied — they're rarely
  // populated on decoded frames and represent unbounded surfaces.
  use core::ptr::{addr_of, addr_of_mut, read_unaligned, write_unaligned};
  use ffmpeg_next::ffi::av_frame_new_side_data;
  unsafe {
    // Scalar timestamps / flags / color / SAR / crop. None of
    // these allocate.
    (*dst).pts = (*src).pts;
    (*dst).pkt_dts = (*src).pkt_dts;
    (*dst).duration = (*src).duration;
    (*dst).best_effort_timestamp = (*src).best_effort_timestamp;
    (*dst).quality = (*src).quality;
    (*dst).repeat_pict = (*src).repeat_pict;
    (*dst).flags = (*src).flags;
    (*dst).sample_aspect_ratio = (*src).sample_aspect_ratio;
    (*dst).crop_left = (*src).crop_left;
    (*dst).crop_top = (*src).crop_top;
    (*dst).crop_right = (*src).crop_right;
    (*dst).crop_bottom = (*src).crop_bottom;
    (*dst).time_base = (*src).time_base;

    // Enum-typed fields: bit-copy raw to avoid materializing an
    // invalid `AVColorPrimaries` etc. on either side. `read_unaligned`
    // / `write_unaligned` on `i32` projections sidestep the bindgen
    // enum's discriminant-validity invariant.
    let pict_type_raw = read_unaligned(addr_of!((*src).pict_type) as *const i32);
    write_unaligned(addr_of_mut!((*dst).pict_type) as *mut i32, pict_type_raw);
    let cp_raw = read_unaligned(addr_of!((*src).color_primaries) as *const i32);
    write_unaligned(addr_of_mut!((*dst).color_primaries) as *mut i32, cp_raw);
    let trc_raw = read_unaligned(addr_of!((*src).color_trc) as *const i32);
    write_unaligned(addr_of_mut!((*dst).color_trc) as *mut i32, trc_raw);
    let cs_raw = read_unaligned(addr_of!((*src).colorspace) as *const i32);
    write_unaligned(addr_of_mut!((*dst).colorspace) as *mut i32, cs_raw);
    let cr_raw = read_unaligned(addr_of!((*src).color_range) as *const i32);
    write_unaligned(addr_of_mut!((*dst).color_range) as *mut i32, cr_raw);
    let cl_raw = read_unaligned(addr_of!((*src).chroma_location) as *const i32);
    write_unaligned(addr_of_mut!((*dst).chroma_location) as *mut i32, cl_raw);

    // Side-data: bounded copy. `av_frame_new_side_data(dst, type,
    // size)` allocates the entry and returns a pointer to write
    // the payload bytes into; a null return is the OOM signal.
    // The caller (`transfer_hw_frame`) hands us a
    // freshly-unref'd `dst` frame, so any prior side-data has
    // already been freed by `av_frame_unref` — we don't need to
    // strip dst's existing side-data here.
    // Read `nb_side_data` as the bindgen `c_int` and clamp non-
    // positive values BEFORE casting to `usize`. A negative value
    // (corrupt / version-skew decoder output) cast directly to
    // `usize` becomes a huge positive count and would walk OOB
    // memory below; pinning to zero up front collapses that to a
    // no-op.
    let nb_side_data_raw = (*src).nb_side_data;
    let src_arr = (*src).side_data;
    if nb_side_data_raw > 0 && !src_arr.is_null() {
      let count_raw = nb_side_data_raw as usize;
      let count = count_raw.min(HW_COPY_SIDE_DATA_MAX_ENTRIES);
      if count_raw > HW_COPY_SIDE_DATA_MAX_ENTRIES {
        tracing::warn!(
          cap = HW_COPY_SIDE_DATA_MAX_ENTRIES,
          requested = count_raw,
          "mediadecode-ffmpeg: HW->CPU transfer side-data entry cap reached; truncating",
        );
      }
      let mut total_bytes: usize = 0;
      for i in 0..count {
        let entry = *src_arr.add(i);
        if entry.is_null() {
          continue;
        }
        let kind_raw = read_unaligned(addr_of!((*entry).type_) as *const i32);
        let size = (*entry).size;
        let data_ptr = (*entry).data;
        if size == 0 || data_ptr.is_null() {
          continue;
        }
        // Whitelist gate: only proceed when `kind_raw` matches a
        // known `AV_FRAME_DATA_*` constant the linked FFmpeg's
        // bindgen output knows about. Without this gate, a
        // version-skew or hostile decoder could write a side-data
        // type integer outside our bindgen's discriminant set, and
        // constructing the `AVFrameSideDataType` enum value (so
        // we could pass it to `av_frame_new_side_data`) would be
        // immediate UB before the call. Unknown types are dropped
        // with a debug-level log — the public converter's
        // `collect_side_data` walks the destination raw and would
        // also surface them as bare integers in `SideDataEntry.kind`.
        let Some(kind_enum) = whitelisted_side_data_kind(kind_raw) else {
          tracing::debug!(
            kind_raw,
            "mediadecode-ffmpeg: unknown AV_FRAME_DATA type during HW->CPU transfer; dropping",
          );
          continue;
        };
        let projected = total_bytes.saturating_add(size);
        if projected > HW_COPY_SIDE_DATA_MAX_TOTAL_BYTES {
          tracing::warn!(
            cap = HW_COPY_SIDE_DATA_MAX_TOTAL_BYTES,
            projected,
            "mediadecode-ffmpeg: HW->CPU transfer side-data byte cap reached; dropping rest",
          );
          break;
        }
        let new_entry = av_frame_new_side_data(dst, kind_enum, size);
        if new_entry.is_null() {
          // **OOM is reported, not absorbed.** This used to `break` and
          // return `Ok(())`, which published a frame carrying whatever
          // side data happened to fit before the allocator gave out —
          // silently dropping the entries behind it. Those entries are
          // the HDR mastering metadata, the ICC profile and the display
          // matrix: a picture that comes back with its colours or its
          // orientation quietly missing is worse than one that does not
          // come back, because nothing downstream can tell.
          //
          // The caller already knows what to do with an error here: it
          // unrefs the partial destination and reports this picture's
          // error from a committed decoder. The copy is this crate's own,
          // no backend's answer, so the probe never advances on it.
          tracing::warn!("mediadecode-ffmpeg: av_frame_new_side_data OOM during HW->CPU transfer",);
          return Err(ffmpeg_next::Error::Other {
            errno: libc::ENOMEM,
          });
        }
        // SAFETY: `(*new_entry).data` is allocated for `size` bytes
        // per av_frame_new_side_data's contract; `data_ptr` is
        // valid for `size` reads per AVFrameSideData's contract.
        core::ptr::copy_nonoverlapping(data_ptr, (*new_entry).data, size);
        total_bytes = projected;
      }
    }
  }
  Ok(())
}

/// Feeds a candidate the probe's history in order — the packets
/// themselves, the candidate taking a reference to each — and then the end
/// if `end`: until one is refused, or the candidate answers `EAGAIN`,
/// having output for the caller first. Answers how far it got.
///
/// **A replay makes no copy**, so nothing of this crate's own can fail it:
/// what can stop it is the candidate's own answer, and its output. And it
/// drains nothing, so no frame is queued while the probe runs and no queue
/// can fill: the candidate is installed with what it has yet to take
/// owed, fed as the caller drains it ([`VideoDecoder::feed`]). The history
/// is shared with the candidate this way, never with the caller: it is
/// released from every decoder before it is handed back
/// ([`VideoDecoder::release_history`]).
fn feed_into(
  decoder: &mut ffmpeg_next::decoder::Video,
  history: &[Packet],
  end: bool,
) -> std::result::Result<Taken, ffmpeg_next::Error> {
  for (index, packet) in history.iter().enumerate() {
    match decoder.send_packet(packet) {
      Ok(()) => {}
      Err(e) if is_eagain(&e) => {
        return Ok(Taken {
          packets: index,
          end: false,
        });
      }
      Err(e) => return Err(e),
    }
  }
  let packets = history.len();
  if !end {
    return Ok(Taken {
      packets,
      end: false,
    });
  }
  match decoder.send_eof() {
    Ok(()) => Ok(Taken { packets, end: true }),
    Err(e) if is_eagain(&e) => Ok(Taken {
      packets,
      end: false,
    }),
    Err(e) => Err(e),
  }
}

/// `EAGAIN` and `EOF` together: "this decoder has no more output for
/// now", either because it wants input or because it is finished.
///
/// **What is left of a predicate that used to guard both roads.** Both
/// public faces classify at their boundary now — [`receive_status`] and
/// [`send_status`] — and the send face had to stop treating the two
/// alike, since `AVERROR_EOF` there is a caller fault rather than a
/// state. What still wants them together is the send roads' guard: neither
/// is the candidate failing, so the probe never advances on one, and
/// [`send_status`] then says which of the two it is.
fn is_transient(e: &ffmpeg_next::Error) -> bool {
  is_eagain(e) || matches!(e, ffmpeg_next::Error::Eof)
}

/// **The receive road's single errno gate, and it cannot be spent
/// without saying where the session is.**
///
/// Turns what a funnel (`software_exit` / `hw_exit`) handed back into
/// the trait's status vocabulary, keeping the two flow signals inside
/// this crate.
///
/// Takes the funnel's *output*, never libavcodec's raw error, and that
/// ordering is the point: the funnels collect a `get_format` or
/// allocator-judge refusal that the callback state is holding, and a
/// classifier placed in front of them would answer "needs input" for a
/// road that had a named refusal waiting. So every receive site funnels
/// first and gates second.
///
/// # Why the phase is a parameter and not a guess
///
/// The same errno means different things at different points in a
/// session's life, and every road that guessed guessed differently:
///
/// * `EAGAIN` is [`Received::NeedsInput`] only where more input can
///   arrive. Past a recorded end it is an instruction the caller cannot
///   carry out — the send gates refuse — so it is the end instead. And
///   on a candidate that has already been handed the whole history
///   including the end, it is neither: a drained decoder answers
///   `AVERROR_EOF`, so that candidate has broken the drain, which is a
///   candidate failing, and it goes back as an error for the probe
///   machinery to act on.
/// * `AVERROR_EOF` is [`Received::Ended`] from a backend that has
///   committed. A candidate's goes back as an error: past the end it is
///   the candidate's successful empty completion, which the receive road
///   commits ([`VideoDecoder::receive_frame`]) — a state change no
///   classifier makes — and before the end libavcodec does not give it.
///
/// Making the phase an argument is what stops a road from having an
/// opinion about this. A classification without it does not compile.
fn receive_status(e: Error, phase: SessionPhase) -> Result<Received> {
  match &e {
    Error::Ffmpeg(f) if is_eagain(f) => {
      if phase.accepts_input() {
        Ok(Received::NeedsInput)
      } else if phase.is_committed() {
        // Draining. A committed backend with nothing more to give has
        // ended, whichever errno it chose — libavcodec is not supposed
        // to answer `EAGAIN` after a flush packet, but this crate has
        // met a codec that does (see `ImageDecodeError::NoImage`), and
        // the alternative is handing back a state nothing can satisfy.
        Ok(Received::Ended)
      } else {
        // `AuditioningPastEnd`: a candidate that has been given
        // everything and produced nothing. The probe road owns it.
        Err(e)
      }
    }
    Error::Ffmpeg(ffmpeg_next::Error::Eof) if phase.is_committed() => Ok(Received::Ended),
    _ => Err(e),
  }
}

/// **The send road's gate, and it is deliberately narrower than its
/// sibling.** Only `EAGAIN` is back pressure here.
///
/// `avcodec_send_packet` answers `AVERROR_EOF` for a different fact than
/// `avcodec_receive_frame` does: not "the stream is over" but *"this
/// decoder has already been told the stream is over, and you sent
/// something anyway"* — a caller usage fault rather than a session
/// state, so it stays in `Err`. Reading it as `Accepted` would silently
/// drop the submission; reading it as `MustDrain` would send the caller
/// into a drain loop that can never make the next offer succeed.
///
/// The same line puts [`crate::ResampleError::AfterEof`] and the
/// WebCodecs adapter's `AfterEof` on the error side.
///
/// # The phase, here too
///
/// [`Sent::MustDrain`] is a promise — *drain, and this same offer
/// becomes acceptable* — and past a recorded end it is one no session
/// can keep. The send gates refuse there first, so this is the second
/// lock rather than the first; what it buys is that the classifier
/// itself becomes incapable of making the promise, which is the whole
/// point of moving the phase into the signature.
fn send_status(e: Error, phase: SessionPhase) -> Result<Sent> {
  match &e {
    Error::Ffmpeg(f) if is_eagain(f) && phase.accepts_input() => Ok(Sent::MustDrain),
    _ => Err(e),
  }
}

/// Reject a `codec::Parameters` whose inner `*mut AVCodecParameters` is
/// null. This guards the public trust boundary: ffmpeg-next can produce
/// such a `Parameters` under OOM (`Parameters::new()` does not check
/// `avcodec_parameters_alloc`), and a safe caller can legally hand one
/// in. Without this check, the very next `(*p.as_ptr()).field` read
/// would be a null deref.
fn ensure_parameters_non_null(parameters: &codec::Parameters) -> Result<()> {
  // SAFETY: as_ptr() returns the inner *const AVCodecParameters; we just
  // inspect the pointer value (no deref).
  if unsafe { parameters.as_ptr() }.is_null() {
    return Err(Error::Ffmpeg(ffmpeg_next::Error::Other {
      errno: libc::ENOMEM,
    }));
  }
  Ok(())
}

/// Allocate a fresh `frame::Video`, checking that `av_frame_alloc` did not
/// return NULL. ffmpeg-next's `frame::Video::empty()` does not surface that
/// failure and the resulting null pointer would be UB on the next field
/// access; this wrapper catches it and surfaces it as `ENOMEM`.
fn alloc_av_frame() -> std::result::Result<frame::Video, ffmpeg_next::Error> {
  let inner = frame::Video::empty();
  // SAFETY: as_ptr() just exposes the inner pointer for inspection.
  if unsafe { inner.as_ptr() }.is_null() {
    return Err(ffmpeg_next::Error::Other {
      errno: libc::ENOMEM,
    });
  }
  Ok(inner)
}

/// The [`CallbackState`] a codec context's callbacks read, owned through
/// the very pointer the context holds.
///
/// [`build_codec_context`] makes it with `Box::into_raw` and hands that
/// pointer to the one door, [`crate::ffi::install_allocator_judge`];
/// from then on the state is reached only through it — by libavcodec's
/// callbacks, and by the decoder's own reads through [`Self::as_ptr`].
/// A `Box` cannot hold it: a box asserts uniqueness over what it holds,
/// so moving one with its decoder would invalidate the pointer the
/// context keeps. This owner moves freely and frees the state when
/// dropped, which must come after the codec context is freed: a decoder
/// that keeps the owner declares it after its context, so field order
/// drops it second, and `DecoderState` takes the raw pointer instead
/// ([`Self::into_raw`]) and frees it after the context itself.
pub(crate) struct InstalledCallbackState(NonNull<CallbackState>);

// The `Send` below rests on `CallbackState: Send + Sync`; this stops
// compiling the day that stops being true.
const _: fn() = || {
  fn send_and_sync<T: Send + Sync>() {}
  send_and_sync::<CallbackState>();
};

// SAFETY: as for `Box<CallbackState>`, which is `Send` because
// `CallbackState` is: this is the unique owner of a heap `CallbackState`,
// and nothing else frees it. Sending the owner sends the decoder that
// holds it, together with the codec context whose callbacks reach the
// state through their own copy of the pointer; they run on whichever
// thread drives that context. Decoding is single-threaded today —
// nothing in this crate sets `thread_count` or `thread_type`, FFmpeg's
// default is one thread, and a file is decoded on one thread — so the
// callbacks run on the decoding thread. Were threads enabled later, the
// state would be reached from several at once, which `CallbackState:
// Sync` covers: every field the callbacks write is an atomic, and the
// plain ones are written only before the context is opened. No `Sync`:
// the decoders holding this are not `Sync`, and nothing reaches the state
// through a shared reference to the owner.
unsafe impl Send for InstalledCallbackState {}

impl InstalledCallbackState {
  /// Moves `state` to the heap and takes ownership of it by raw pointer.
  fn new(state: CallbackState) -> Self {
    let raw = Box::into_raw(Box::new(state));
    // SAFETY: `Box::into_raw` returns the box's own pointer, which is
    // aligned and non-null.
    Self(unsafe { NonNull::new_unchecked(raw) })
  }

  /// The pointer the codec context holds — the one every access to the
  /// state goes through.
  pub(crate) fn as_ptr(&self) -> *mut CallbackState {
    self.0.as_ptr()
  }

  /// Gives up ownership without freeing: the caller frees the state with
  /// `Box::from_raw`, after the codec context — as `DecoderState`'s
  /// `Drop` and the hardware build's guard do.
  pub(crate) fn into_raw(self) -> *mut CallbackState {
    let raw = self.0.as_ptr();
    core::mem::forget(self);
    raw
  }
}

impl Drop for InstalledCallbackState {
  fn drop(&mut self) {
    // SAFETY: the pointer came from `Box::into_raw` in `new`, and this is
    // its only free (`into_raw` forgets the owner instead of dropping
    // it). The owner is dropped after the codec context is freed, or
    // before a context that was never opened, which makes no callback —
    // either way nothing reaches the state again.
    drop(unsafe { Box::from_raw(self.0.as_ptr()) });
  }
}

/// Build a fresh `Context` from `parameters`, checking the underlying
/// `avcodec_alloc_context3` for NULL before passing it to
/// `avcodec_parameters_to_context`. ffmpeg-next's `Context::from_parameters`
/// skips that check and would feed a null pointer into FFmpeg under OOM —
/// undefined behavior. This helper surfaces the failure as `ENOMEM` and
/// frees the context if `parameters_to_context` itself errors.
pub(crate) fn build_codec_context(
  parameters: &codec::Parameters,
  limits: crate::limits::DecoderLimits,
  options: crate::DecoderOptions,
  pkt_timebase: Option<mediadecode::Timebase>,
) -> Result<(Context, InstalledCallbackState)> {
  ensure_parameters_non_null(parameters)?;
  // **The choke point.** `avcodec_parameters_to_context` below is a
  // wholesale copy *into* FFmpeg — it duplicates `extradata`, every
  // `coded_side_data` entry and the channel map into the context, at
  // whatever size the caller's parameters declare. Every road that
  // opens a decoder in this crate arrives here, so measuring and
  // admitting once, right here, is what stops a caller handing
  // libavcodec parameters nobody budgeted: the four session `open`s,
  // the HW probe's `build_state`, its per-backend advances, and the
  // software fallback all pass through this function and none of them
  // can reach `avcodec_parameters_to_context` any other way.
  //
  // The outbound clone (`extras::bounded_clone_parameters`) closed the
  // Rust-side copy; this closes the FFmpeg-side one. They are the same
  // budget.
  //
  // **Structure before size, and before FFmpeg is handed anything.**
  //
  // `measure_parameters` below prices a custom channel map from
  // `nb_channels` alone and never looks at `u.map`, so a layout with a
  // positive count and a null map passed this budget and went on to
  // `avcodec_parameters_to_context`, which reaches
  // `av_channel_layout_copy` — a `memcpy` from that null. The
  // non-custom orders were no better: nothing bounded `nb_channels` for
  // a `NATIVE` or `AMBISONIC` layout, and FFmpeg's own arithmetic over
  // it assumes invariants it does not check.
  //
  // Every decoder this crate opens arrives here, and the refusal is
  // structural rather than an allocation failure, so it carries its own
  // error rather than the ceiling's or the allocator's.
  //
  // SAFETY: `ensure_parameters_non_null` just proved the pointer is
  // live; for a custom order its map is FFmpeg's own allocation of
  // `nb_channels` entries, and the preflight allocates nothing.
  unsafe {
    crate::channel_layout::layout_preflight(ptr::addr_of!((*parameters.as_ptr()).ch_layout))
  }
  .map_err(
    |fault| match mediaio_ffmpeg::ParametersError::from_layout_fault(0, fault) {
      mediaio_ffmpeg::ParametersError::LayoutShape(shape) => Error::MalformedChannelLayout(shape),
      mediaio_ffmpeg::ParametersError::ChannelMap(map) => Error::ChannelMapMissing(map),
      // The preflight allocates nothing, so no other arm is reachable;
      // reporting the allocator keeps the match total without an
      // `unreachable!`.
      _ => Error::Ffmpeg(ffmpeg_next::Error::Other {
        errno: libc::ENOMEM,
      }),
    },
  )?;

  // SAFETY: `ensure_parameters_non_null` just proved the pointer is
  // live; the measurement allocates nothing.
  let footprint = unsafe { crate::extras::measure_parameters(parameters.as_ptr()) };
  let declared = footprint.and_then(|f| f.total()).unwrap_or(usize::MAX);
  if declared > limits.max_codec_parameter_bytes() {
    return Err(Error::ParametersTooLarge(
      mediaio_ffmpeg::ParametersTooLarge::new(0, declared, limits.max_codec_parameter_bytes()),
    ));
  }
  // SAFETY: avcodec_alloc_context3(NULL) returns a fresh AVCodecContext
  // or NULL on allocation failure.
  let ctx_ptr = unsafe { avcodec_alloc_context3(ptr::null()) };
  if ctx_ptr.is_null() {
    return Err(Error::Ffmpeg(ffmpeg_next::Error::Other {
      errno: libc::ENOMEM,
    }));
  }
  // SAFETY: ctx_ptr is non-null and freshly allocated; parameters.as_ptr()
  // returns a valid AVCodecParameters pointer; the function copies bytes
  // out of parameters into the context.
  let ret = unsafe { avcodec_parameters_to_context(ctx_ptr, parameters.as_ptr()) };
  if ret < 0 {
    // SAFETY: ctx_ptr was allocated by us and never handed to anyone else.
    let mut p = ctx_ptr;
    unsafe { avcodec_free_context(&mut p) };
    return Err(Error::Ffmpeg(ffmpeg_next::Error::from(ret)));
  }
  // **The push-down.** The same pixel ceiling this crate checks against
  // a decoded frame is written into the decoder itself, so libavcodec
  // refuses an oversized picture *before allocating it*. Checking only
  // on our side would mean FFmpeg had already paid for the frame by the
  // time we declined to copy it — two layers, one number, and this is
  // the layer that matters.
  //
  // FFmpeg's own default here is `INT_MAX`, i.e. no ceiling worth the
  // name. `max_pixels` is a plain `int64_t` field on `AVCodecContext`
  // (and has been since FFmpeg 4.0), so it is set directly rather than
  // through `av_opt_set_int` and a stringly-typed option name.
  //
  // **And the byte ceiling, pushed down through the same field.**
  //
  // The pixel ceiling alone does not bound bytes, because a pixel is not
  // a fixed price: 10000x10000 is 100 Mpx — comfortably under the 256
  // Mpx default — and in `rgba64` it is 800 MB, well over the 512 MiB
  // byte ceiling. A highly compressible frame of that shape is a few KB
  // on disk, so nothing upstream sees it coming.
  //
  // **`max_pixels` carries the caller's number, verbatim.** It used to
  // carry `min(that, max_frame_bytes / worst-bytes-per-pixel)`, so the
  // byte ceiling could be enforced before libavcodec allocated — and
  // that translation charged every stream the widest format in
  // existence, 16 bytes a pixel. A 1920x1080 `yuv420p` frame costs
  // 3.14 MiB and was refused under a 4 MiB budget, at
  // `ff_set_dimensions`, before anything accurate had a chance to look
  // at it. Over-refusing ordinary video is not a conservative failure;
  // it is a broken decoder.
  //
  // The translation is gone because it is no longer needed: the byte
  // ceiling is enforced by `judge_buffer`, which is *also* a
  // pre-allocation seat — `get_buffer2` is the allocator, so it runs
  // before the allocation and prices the frame's real format at its
  // real aligned dimensions. Nothing is lost on the software road by
  // stating the pixel limit as what it is.
  //
  // SAFETY: `ctx_ptr` is the non-null context just allocated and
  // populated above; `max_pixels` is a public field.
  unsafe {
    (*ctx_ptr).max_pixels = i64::try_from(limits.frame().max_pixels()).unwrap_or(i64::MAX);
  }

  // **The byte ceiling's own seat, in the allocator itself.**
  // `max_pixels` bounds an extent; what an extent costs depends on its
  // format and on how the allocator aligns it — a `gray8` frame of
  // 65536x1 is 64 KiB by `w * h` and 2 MiB once its single row is
  // rounded up. No scalar compared against a pixel product can bound
  // that, so the byte question is asked where the answer is knowable:
  // in `get_buffer2`, which *is* the allocation, against the caller's
  // own `max_frame_bytes`.
  //
  // See `judge_buffer` for why this hook rather than `get_format`
  // (measured: `get_format` never fires for a one-shot `png` decode).
  // It is installed below, together with the state it reads.

  // **The packet timebase, for every decoder opened against a stream.**
  //
  // `AVCodecContext.pkt_timebase` is documented as caller-supplied, and
  // libavcodec reads it: its generic subtitle path derives
  // `AVSubtitle.pts` and the packet-duration fallback *only* when the
  // field is set, so a subtitle decoder opened without it returns
  // timestamped cues carrying neither. It was never written here, on
  // any road.
  //
  // **A zero numerator is deliberately not written.** Left alone the
  // field is `0/1`, and `0/1` is also exactly what a container that
  // declared no timebase hands this crate — the two are the same
  // statement, so writing it would be ceremony, and treating it as a
  // real ruler would claim one the file never gave. See
  // [`crate::demuxer`]'s own note on why that value is passed through
  // rather than refused on the track road.
  if let Some(timebase) = pkt_timebase.filter(|timebase| timebase.num() > 0) {
    // SAFETY: `ctx_ptr` is the non-null context allocated above and
    // `pkt_timebase` is a public field; the value is built from a
    // `Timebase`, which cannot be negative or zero-denominatored.
    unsafe {
      (*ctx_ptr).pkt_timebase =
        ffmpeg_next::Rational::new(timebase.num(), timebase.den().get()).into();
    }
  }

  // **How the decoder decodes** ([`crate::DecoderOptions`]): the three
  // `AVDiscard` settings, "decoding: Set by user" (`libavcodec/avcodec.h`
  // 1653–1667 in 9.0.1), written into every context this function builds.
  // A road that takes no options passes the defaults, which are the very
  // values `avcodec_alloc_context3` has just written, so its context is
  // unchanged.
  //
  // SAFETY: `ctx_ptr` is the non-null context allocated above; the three
  // are public fields of type `AVDiscard`, and each value written is one
  // of its named discriminants.
  unsafe {
    (*ctx_ptr).skip_frame = options.skip_frame().as_av();
    (*ctx_ptr).skip_loop_filter = options.skip_loop_filter().as_av();
    (*ctx_ptr).skip_idct = options.skip_idct().as_av();
  }

  // **`max_samples` is deliberately left alone.**
  //
  // It bounds `nb_samples * channels`, so bounding *bytes* with it
  // means dividing by a per-channel-sample cost — and the only sound
  // divisor is the widest sample format the build can emit, 8 bytes.
  // That charged every stream `f64` rates: a 6-channel `s16` frame
  // fitting a 64 KiB budget was refused, because the translation
  // priced it at four times its real cost.
  //
  // The audio pre-allocation story is now the same as the video one,
  // and it is stronger than the translation was: `judge_buffer` runs
  // in `get_buffer2`, before the planes are allocated, and prices the
  // frame's real sample format at its real channel count through
  // [`crate::footprint`] — which asks `av_samples_get_buffer_size`, the
  // allocator's own ruler. An exact judge at the allocation beats an
  // approximate one before it.

  // **The judge's budget seat.** `judge_buffer` runs as a C callback
  // with nothing but the context to read, and the byte ceiling is not
  // recoverable from any field on it — see `CallbackState`'s
  // `max_frame_bytes` in `mediaio-ffmpeg`. So the state that already
  // carries the `get_format` declination carries the budget too, and
  // every road gets one: this is the single point every decoder in the
  // crate is built through.
  //
  // Ownership goes back to the caller, by raw pointer — see
  // [`InstalledCallbackState`] for why not by box.
  let state =
    InstalledCallbackState::new(CallbackState::new(limits.frame().max_frame_bytes() as u64));
  // SAFETY: the door's contract, clause by clause.
  //
  // * `ctx_ptr` is the live context allocated above, and it has not been
  //   opened.
  // * `state.as_ptr()` comes from `Box::into_raw`, so no box holds the
  //   state, and from here on it is reached only through that pointer —
  //   by libavcodec's callbacks and by the decoder's own reads through
  //   `InstalledCallbackState::as_ptr` — so nothing invalidates it. The
  //   owner goes back to the caller, which frees it
  //   after the context: the audio, image, subtitle and video decoders
  //   declare it after their context; `DecoderState` holds it by raw
  //   pointer and its `Drop` frees the context first; and the hardware
  //   build's guard is dropped after the opened decoder.
  // * The never-opened exception: a caller's return between this call
  //   and the open (a missing decoder, an unsupported hardware backend, a
  //   device that fails to initialise or whose reference cannot be taken)
  //   may drop the owner before the context, which is then only freed.
  // * Its plain fields are written only before the open — the hardware
  //   build's `set_wanted`, through this same pointer — and no
  //   `&mut CallbackState` is held after it. Decoding is single-threaded
  //   today (nothing in this crate sets `thread_count` or `thread_type`,
  //   FFmpeg's default is one thread, and a file is decoded on one
  //   thread), so the callbacks run on the decoding thread; were threads
  //   enabled, `CallbackState: Sync` covers the state being reached from
  //   several.
  // * Nothing else uses `opaque`.
  unsafe { crate::ffi::install_allocator_judge(ctx_ptr, state.as_ptr()) };

  // SAFETY: ctx_ptr is valid; passing `owner: None` means our wrapper owns
  // the allocation and `Context::drop` will run `avcodec_free_context`.
  Ok((unsafe { Context::wrap(ctx_ptr, None) }, state))
}

/// Checked deep-clone of `codec::Parameters`. ffmpeg-next's
/// `Parameters::clone` allocates via `avcodec_parameters_alloc` without
/// checking for NULL and runs `avcodec_parameters_copy` without checking
/// the return code. On `ENOMEM` the result is a `Parameters` with a null
/// inner pointer, which becomes UB when later passed to FFmpeg.
///
/// This helper performs both calls explicitly, frees a partial allocation
/// on failure, and surfaces the AVERROR. The returned `Parameters` has
/// `owner: None`, severing any Rc link to the caller's demuxer (the
/// reason we deep-clone in the first place — see Send safety in
/// `VideoDecoder::open`).
pub(crate) fn try_clone_parameters(
  src: &codec::Parameters,
  budget: usize,
) -> std::result::Result<codec::Parameters, Error> {
  // Through the bounded clone, like every other parameter copy in this
  // crate — see [`crate::extras::bounded_clone_parameters`] for the
  // rule and why the wholesale `avcodec_parameters_copy` this used to
  // call is gone. This path is attacker-facing: `VideoDecoder::open`
  // takes whatever `stream.parameters()` hands it, straight off a
  // container.
  //
  // `budget` is the **active** ceiling, threaded from the session's own
  // `DecoderLimits` — through the initial ownership clone, the probe
  // state's copy, every probe advance and the software fallback. It
  // used to be the crate default, so a lowered ceiling did not bind
  // here (the clone admitted 16 MiB whatever the caller configured,
  // and only `build_codec_context` downstream refused) and a raised one
  // could not be used at all.
  //
  // The stream index is reported as 0: this helper is handed
  // parameters, not a stream, and inventing a coordinate it cannot
  // know would be worse than admitting it has none.
  crate::extras::bounded_clone_parameters(src, 0, budget).map_err(|e| match e {
    mediaio_ffmpeg::ParametersError::TooLarge(p) => Error::ParametersTooLarge(p),
    mediaio_ffmpeg::ParametersError::Copy(p) => Error::Ffmpeg(*p.source()),
    // A missing or unallocatable destination is the out-of-memory this
    // helper has always reported.
    _ => Error::Ffmpeg(ffmpeg_next::Error::Other {
      errno: libc::ENOMEM,
    }),
  })
}

/// Sum of `AVPacket.side_data[i].size` across every entry, plus
/// `nb_entries * SIDE_DATA_ENTRY_OVERHEAD` (descriptor + AVBufferRef +
/// allocator bookkeeping per entry). `av_packet_ref` performs a deep
/// copy of side data via `av_packet_copy_props`, so each probe-buffered
/// clone retains every one of these bytes. Charging both keeps
/// `MAX_PROBE_PACKET_BYTES` a true upper bound — without the overhead,
/// many zero-size entries slip past the cap on pure descriptor cost.
///
/// Walks at most `max_entries` entries even when `side_data_elems`
/// reports a larger count. Defense-in-depth against a corrupt or hostile
/// packet whose `side_data_elems` lies about the actual array length:
/// the caller is expected to also reject any packet whose count exceeds
/// the cap (so the inflated clone is never created), but bounding the
/// walk here means a stale or weaponised value can never trigger an
/// unbounded raw-pointer scan from the safe API.
///
/// Reads only the `size` field of each `AVPacketSideData` entry — never
/// touches the bindgen `AVPacketSideDataType` enum, so no UB even if a
/// future FFmpeg adds a side-data type discriminant our build doesn't
/// know.
pub(crate) fn packet_side_data_bytes(packet: &Packet, max_entries: usize) -> usize {
  // SAFETY: AVPacket.side_data is `*mut AVPacketSideData` and
  // side_data_elems is `c_int`; both are raw struct fields safe to read.
  // Field projection (`.size`) does not reconstruct the enum-typed `type_`
  // field, so the bindgen-enum UB hazard does not apply here.
  unsafe {
    let raw = packet.as_ptr();
    let nel = (*raw).side_data_elems;
    let arr = (*raw).side_data;
    if arr.is_null() || nel <= 0 || max_entries == 0 {
      return 0;
    }
    let count = (nel as usize).min(max_entries);
    let mut total = count.saturating_mul(SIDE_DATA_ENTRY_OVERHEAD);
    for i in 0..count {
      let entry = arr.add(i);
      total = total.saturating_add((*entry).size);
    }
    total
  }
}

/// Number of `AVPacketSideData` entries on `packet`. The probe buffer
/// uses this to enforce [`MAX_PROBE_PACKET_SIDE_DATA_ENTRIES`] before
/// cloning, so a packet whose entry count alone would dominate retained
/// memory is rejected up front.
pub(crate) fn packet_side_data_count(packet: &Packet) -> usize {
  // SAFETY: side_data_elems is `c_int`, safe to read; clamp negatives to 0.
  let nel = unsafe { (*packet.as_ptr()).side_data_elems };
  if nel <= 0 { 0 } else { nel as usize }
}

/// Whether a decode session should **park** the frame a conversion
/// refusal came from and re-attempt it before receiving another.
///
/// The same shape as the demuxer's parking, for the same reason: a
/// decoder's `receive_frame` advances libavcodec, so a conversion that
/// then fails on an allocation would lose a frame nothing can ask for
/// again. Only an allocation qualifies — every other arm is a fact about
/// the frame (a format nothing can carry, a layout that does not add up,
/// a plane outside its own buffers), and re-offering one of those would
/// answer every later receive with the same error.
///
/// Decided here, by the sessions that hold the frame, rather than on
/// `ConvertError`: whether to retry depends on what the caller still
/// holds, which the error cannot know.
pub(crate) fn parks_in_decode(e: &crate::convert::ConvertError) -> bool {
  matches!(e, crate::convert::ConvertError::CarrierAllocFailed(_))
}

/// Just `EAGAIN` (separate from EOF — the FFmpeg send/receive state machine
/// distinguishes "drain output and retry" from "stream over").
fn is_eagain(e: &ffmpeg_next::Error) -> bool {
  matches!(e, ffmpeg_next::Error::Other { errno } if *errno == ffmpeg_next::error::EAGAIN)
}

/// Prices the CPU frame `av_hwframe_transfer_data` would allocate, and
/// refuses it if it is over the ceiling — **before** the transfer runs.
///
/// # Why the hardware road needs its own seat
///
/// `judge_buffer` is not a universal choke point, and the census says
/// so on this machine. `ff_get_buffer` calls `hwaccel->alloc_frame`
/// directly and never reaches `get_buffer2` at all: a VideoToolbox
/// h264 decode of a 160x120 clip records **zero** `get_buffer2` calls
/// while producing a hardware frame. And the CPU destination of a
/// download is allocated by `av_hwframe_transfer_data` itself, outside
/// both hooks.
///
/// # What the census settled about the surface itself
///
/// `max_pixels` **does** bite before `alloc_frame`, and this was
/// measured rather than assumed: with `max_pixels = 100`, a 160x120
/// VideoToolbox h264 decode fails at `avcodec_open2` with
/// `Picture size 160x120 exceeds specified max pixel count 100` from
/// `av_image_check_size2`, zero `get_buffer2` calls and no frame. The
/// check lives in `ff_set_dimensions`, which every decoder runs when it
/// learns its dimensions and before any surface pool exists — so the
/// seat `max_pixels` already occupies covers the hardware surface too.
///
/// The residual on that road is the aligned-dimensions gap
/// `judge_buffer` closes for software frames, and it applies to
/// **driver-owned GPU memory** rather than to anything this crate
/// carries. What this crate does carry off the hardware road is the CPU
/// frame downloaded here, and that is bounded exactly, by this
/// function.
///
/// # How the price is taken
///
/// The destination format is not chosen by this crate: `dst.format` is
/// `AV_PIX_FMT_NONE` on entry and FFmpeg picks from
/// `av_hwframe_transfer_get_formats`. So the whole candidate list is
/// priced and the **worst** taken — walked as `*const c_int` through
/// the shim, because a driver may offer a format this build's bindings
/// do not name, which is the same discipline the pixel census keeps.
///
/// When the list cannot be obtained the global worst rate stands in;
/// over-refusing is the safe direction for a ceiling.
///
/// # Safety
///
/// `hw_frame` must be a live `*const AVFrame`.
unsafe fn judge_hw_transfer(
  hw_frame: *const ffmpeg_next::ffi::AVFrame,
  limits: crate::FrameLimits,
) -> std::result::Result<(), crate::error::HwTransferTooLarge> {
  // SAFETY: `hw_frame` is live per the contract; the field is a plain
  // pointer.
  let frames_ctx = unsafe { (*hw_frame).hw_frames_ctx };

  // **The allocated extent, not the displayed one.** `AVFrame.width` /
  // `.height` are the *display* dims; what
  // `av_hwframe_transfer_data` allocates is sized from the frames
  // context, and on a cropped stream the two diverge by orders of
  // magnitude — measured on this build, an h264 stream with SPS
  // cropping shows 32x32 display over a 1920x1088 coded surface, a
  // 2040x gap. This crate already had a helper that reads the pool
  // dims, with a doc comment naming this exact trap; the first version
  // of this judge reached past it for `AVFrame.width` anyway.
  //
  // **Fail closed.** No context, no dims, or no priceable candidate
  // means the allocation extent cannot be proved — and an unprovable
  // extent is not a small one: falling back to display dims here would
  // restore precisely the hole this judge exists to close.
  if frames_ctx.is_null() {
    // Not a hardware frame at all. `av_hwframe_transfer_data` refuses
    // such a source with `EINVAL` and allocates nothing, so there is no
    // extent to bound here — and answering "too large" would put a
    // ceiling's name on a completely different fault. The existing path
    // reports it accurately.
    return Ok(());
  }
  let Some((width, height)) = (unsafe { hw_frames_ctx_dimensions_raw(hw_frame) }) else {
    // A hardware frame whose pool extent cannot be read. The transfer
    // may well allocate; nothing here can say how much. Charged as
    // unbounded, which is what an unprovable extent is.
    return Err(crate::error::HwTransferTooLarge::new(
      usize::MAX,
      limits.max_frame_bytes(),
    ));
  };

  // **Every candidate folded in, priceable or not.**
  //
  // FFmpeg picks the destination format from this list; this crate does
  // not get to choose. So the bound has to be the maximum over the
  // *whole* list — and the fold used to skip the members libavutil
  // would not size, updating `worst` only on priceable ones and
  // reaching for a fallback only when *nothing* priced. A list holding
  // one cheap priceable format beside one unpriceable format was
  // therefore judged at the cheap price, while FFmpeg remained free to
  // select the one that was ignored.
  //
  // An unpriceable candidate is charged
  // [`crate::footprint::video_frame_bytes_upper_bound`] instead: the
  // same dimension alignment and per-plane overhead at the widest rate,
  // so it dominates whatever that layout would have cost had it been
  // priceable.
  let mut worst: usize = 0;
  let mut judged_any = false;
  if !frames_ctx.is_null() {
    // SAFETY: `frames_ctx` is the frame's live `AVHWFramesContext`
    // reference, held for this whole call. The base walks the list
    // libavutil allocates and frees it on every road out.
    let walked = unsafe {
      crate::ffi::visit_hwframe_download_formats(frames_ctx, |candidate| {
        // **The allocator's arithmetic, not the payload's.** Pricing
        // `av_image_get_buffer_size` at a fixed alignment is what the
        // pixels weigh laid out tightly — for a 16x16 NV12 destination
        // that is 768 bytes against the 1,792 `av_frame_get_buffer`
        // really takes. See [`crate::footprint`].
        let cost = crate::footprint::video_frame_bytes(candidate, width, height)
          .or_else(|| crate::footprint::video_frame_bytes_upper_bound(width, height));
        match cost {
          Some(size) => {
            worst = worst.max(size);
            judged_any = true;
            std::ops::ControlFlow::Continue(())
          }
          // Not even the dimension-only bound could be formed, so the
          // extent itself is not a picture. Nothing here will guess.
          None => std::ops::ControlFlow::Break(()),
        }
      })
    };
    // libavutil declining to list (`Err`) leaves `judged_any` false, and
    // the fallback below prices the frame — as before.
    if let Ok(Some(())) = walked {
      return Err(crate::error::HwTransferTooLarge::new(
        usize::MAX,
        limits.max_frame_bytes(),
      ));
    }
  }

  if !judged_any {
    // An empty list, or a query that failed: no candidate was seen at
    // all. Charge the dimension-only bound over the pool extent, which
    // is the most any format this build can emit could cost there.
    let Some(bound) = crate::footprint::video_frame_bytes_upper_bound(width, height) else {
      return Err(crate::error::HwTransferTooLarge::new(
        usize::MAX,
        limits.max_frame_bytes(),
      ));
    };
    worst = bound;
  }

  if worst > limits.max_frame_bytes() {
    return Err(crate::error::HwTransferTooLarge::new(
      worst,
      limits.max_frame_bytes(),
    ));
  }
  Ok(())
}
/// Reads and clears the coded-surface refusal a `get_format` callback
/// left in its state, if it left one.
///
/// Free-standing rather than a method because the reason has to survive
/// on **every** hardware exit, and one of them — the open-time failure
/// path — runs before a decoder exists to ask.
///
/// # Safety
///
/// `state` must be null or point at a [`CallbackState`] that stays live
/// for the duration of the call — in practice the one a decoder owns and
/// frees only after the codec context that points at it.
unsafe fn ceiling_declination_of(state: *const CallbackState) -> Option<Error> {
  // SAFETY: this function's own contract, forwarded.
  unsafe { crate::ffi::take_ceiling_declination(state) }.map(|(pixels, limit)| {
    Error::HwSurfaceTooLarge(crate::error::HwSurfaceTooLarge::new(pixels, limit))
  })
}
/// The software decoders' error funnel.
///
/// Every road that turns a libavcodec decode failure into an `Error`
/// goes through here, so a frame the allocator judge refused comes back
/// named instead of as the `EINVAL` libavcodec also uses for corrupt
/// input. The hardware roads have their own funnel (`hw_exit`); this is
/// its software twin, and the discipline is the same one: **a consumer
/// added helper-by-helper is lost the next time the surrounding code is
/// restructured, so every exit calls one function.**
///
/// # Safety
///
/// `state` must be null or point at a [`CallbackState`] that stays live
/// for the duration of the call.
pub(crate) unsafe fn software_exit(state: *const CallbackState, e: ffmpeg_next::Error) -> Error {
  // SAFETY: this function's own contract, forwarded.
  unsafe { frame_budget_declination_of(state) }.unwrap_or(Error::Ffmpeg(e))
}

/// **The software road's only way to read an errno — funnel and
/// classify in one call, because the order between them is a law and
/// laws that depend on remembering get broken.**
///
/// Every receive site used to write the two steps out: funnel, then
/// classify. The R1 report called that ordering load-bearing and
/// explained why — a `get_format` declination or an allocator-judge
/// refusal sits in the callback state waiting to be collected, and a
/// classifier that runs first reads the errno libavcodec reported
/// instead of the refusal this crate made, answering `Ended` or
/// `NeedsInput` for a frame that was declined. Then a restructure
/// reordered one road and the law was simply gone, silently, because
/// nothing enforced it.
///
/// So the classifiers are private to this module now and this is the
/// door. A caller cannot classify a raw error because it cannot reach a
/// classifier; the funnel is not something to remember to call first,
/// it is the only thing there is to call.
///
/// # The verdict is minted once and threaded
///
/// **A funnel consumes what it collects.** `take_ceiling_declination`
/// and `take_frame_budget_declination` both *clear* the latch they
/// read, because a refusal reported twice would be a refusal invented
/// once. That makes the verdict a one-shot value, and the rule that
/// follows is the whole of this invariant:
///
/// > The first funnel on a road mints the verdict. Every later step on
/// > that road **threads it**. A site that re-funnels, or that rebuilds
/// > `Error::Ffmpeg(raw)` after a funnel has run, is the bug class —
/// > the second call finds the latch empty and reports the errno the
/// > substrate happened to give over the refusal this crate made.
///
/// Nothing downstream of a funnel reads the raw errno to decide a route:
/// the hardware roads route on the verdict and on the road's own reading
/// of an unnamed one. What the caller is told is always the verdict.
///
/// # Safety
///
/// `state` must be null or point at a [`CallbackState`] that stays live
/// for the duration of the call.
pub(crate) unsafe fn software_receive(
  state: *const CallbackState,
  e: ffmpeg_next::Error,
  phase: SessionPhase,
) -> Result<Received> {
  // SAFETY: this function's own contract, forwarded.
  receive_status(unsafe { software_exit(state, e) }, phase)
}

/// The send road's half of [`software_receive`]. Same law, same door.
///
/// # Safety
///
/// `state` must be null or point at a [`CallbackState`] that stays live
/// for the duration of the call.
pub(crate) unsafe fn software_send(
  state: *const CallbackState,
  e: ffmpeg_next::Error,
  phase: SessionPhase,
) -> Result<Sent> {
  // SAFETY: this function's own contract, forwarded.
  send_status(unsafe { software_exit(state, e) }, phase)
}

/// Reads and clears a software frame-budget refusal left by
/// `judge_buffer`, as the named error it deserves.
///
/// The software twin of [`ceiling_declination_of`]: the allocator judge
/// can only answer libavcodec with an errno, so the reason lives in the
/// callback state and every decoder funnel collects it.
///
/// # Safety
///
/// `state` must be null or point at a [`CallbackState`] that stays live
/// for the duration of the call — in practice the one a decoder owns and
/// frees only after the codec context that points at it.
pub(crate) unsafe fn frame_budget_declination_of(state: *const CallbackState) -> Option<Error> {
  // SAFETY: this function's own contract, forwarded.
  unsafe { crate::ffi::take_frame_budget_declination(state) }.map(|(bytes, limit, audio)| {
    Error::FrameBudgetExceeded(crate::error::FrameBudgetExceeded::new(
      bytes,
      limit,
      if audio {
        crate::error::FrameMedium::Audio
      } else {
        crate::error::FrameMedium::Video
      },
    ))
  })
}

/// Proves an opened codec context is a **video** one without going
/// through `Opened::video()`.
///
/// `Opened::video()` calls `Context::medium()`, which reads
/// `AVCodecContext.codec_type` as the bindgen `AVMediaType` enum — a
/// value outside this build's discriminant set is UB the moment it is
/// formed, before any comparison can run. The hardware path has always
/// bypassed that API for this reason; this is that bypass, extracted so
/// the second caller reuses it instead of restating it.
///
/// The caller keeps ownership of `opened` on failure, so its `Drop`
/// still releases the codec context.
pub(crate) fn ensure_video_codec_type(opened: &codec::decoder::Opened) -> Result<()> {
  ensure_codec_type(opened, AVMediaType::AVMEDIA_TYPE_VIDEO)
}

/// The general form: proves an opened context has the medium expected,
/// reading `codec_type` as the integer it is.
///
/// `Opened::{video,audio,subtitle}()` all go through
/// `Context::medium()`, so all three carried the same hazard and all
/// three now come through here.
pub(crate) fn ensure_codec_type(
  opened: &codec::decoder::Opened,
  expected: AVMediaType,
) -> Result<()> {
  // SAFETY: `codec_type` is bound as `AVMediaType` (`#[repr(i32)]`),
  // the same size and alignment as `i32`; reading the bytes as `i32`
  // cannot be UB whatever FFmpeg wrote there.
  let codec_type_int: i32 =
    unsafe { ptr::read(ptr::addr_of!((*opened.as_ptr()).codec_type) as *const i32) };
  if codec_type_int != expected as i32 {
    // The same error `Opened::video()` would have produced, without the
    // enum construction.
    return Err(Error::Ffmpeg(ffmpeg_next::Error::InvalidData));
  }
  Ok(())
}

/// Look up the decoder for `parameters` without going through the bindgen
/// `AVCodecID` Rust enum. Reads the codec_id field as raw `u32` via
/// `addr_of!` + `ptr::read` so a value not in our build's discriminant
/// set never invokes UB.
pub(crate) fn find_decoder(parameters: &codec::Parameters) -> Result<Codec> {
  ensure_parameters_non_null(parameters)?;
  // SAFETY: parameters' inner pointer is non-null (checked above);
  // addr_of! projects to the codec_id field; the *const u32 cast is sound
  // because AVCodecID is `#[repr(u32)]` (same size and alignment as u32).
  // Reading as u32 cannot be UB regardless of the value FFmpeg wrote.
  let raw_id: u32 =
    unsafe { ptr::read(ptr::addr_of!((*parameters.as_ptr()).codec_id) as *const u32) };

  // Through the base's `c_int`-typed door — no `AVCodecID` enum is ever
  // formed from `raw_id`. An id libavcodec names no decoder for is
  // `Error::NoCodec`.
  let Some(codec_ptr) = crate::ffi::find_decoder_raw(raw_id as libc::c_int) else {
    return Err(Error::NoCodec(raw_id));
  };
  // SAFETY: the pointer names an entry of libavcodec's static codec
  // table, live for the duration of the program.
  Ok(unsafe { Codec::wrap(codec_ptr.as_ptr().cast_const()) })
}

/// Allocated frame dimensions according to `raw`'s `hw_frames_ctx`.
///
/// Per FFmpeg's `libavutil/hwcontext.c::transfer_data_alloc`, the CPU
/// destination of `av_hwframe_transfer_data` is allocated using
/// `AVHWFramesContext.width / .height` (the *allocated* surface size of
/// the HW pool); only afterwards is `dst->width / dst->height` reset to
/// `src->width / src->height` (the *display* size). For cropped or
/// heavily aligned streams the allocated dims can be much larger than
/// the display dims (e.g. coded 8192×8192 surface with a 100×100
/// display crop), so any byte-cap accounting that uses display dims
/// undercounts by `allocated_height / display_height` and lets the
/// real allocation slip past the cap.
///
/// Returns `None` when no `hw_frames_ctx` is attached or the dimensions
/// are non-positive — [`judge_hw_transfer`] charges a frame it cannot
/// size as unbounded.
///
/// # Safety
///
/// `raw` must be a live `*const AVFrame`.
unsafe fn hw_frames_ctx_dimensions_raw(raw: *const AVFrame) -> Option<(i32, i32)> {
  // SAFETY: AVFrame.hw_frames_ctx is `*mut AVBufferRef`. When non-null,
  // its `data` field points to an `AVHWFramesContext`. We read `.width`
  // and `.height` (both `c_int`) via field projection — neither field is
  // enum-typed, so no bindgen-enum UB hazard.
  unsafe {
    let hw_ctx_ref = (*raw).hw_frames_ctx;
    if hw_ctx_ref.is_null() {
      return None;
    }
    let data = (*hw_ctx_ref).data;
    if data.is_null() {
      return None;
    }
    let frames_ctx = data as *const AVHWFramesContext;
    let w: i32 = ptr::read(ptr::addr_of!((*frames_ctx).width));
    let h: i32 = ptr::read(ptr::addr_of!((*frames_ctx).height));
    if w <= 0 || h <= 0 {
      return None;
    }
    Some((w, h))
  }
}

#[allow(dead_code)]
fn _assert_send() {
  fn check<T: Send>() {}
  check::<VideoDecoder>();
}

/// The three skips `ctx` holds — `skip_frame`, `skip_loop_filter` and
/// `skip_idct` — as the raw integers they are, for the laws that prove a
/// decoder's options reached its context.
///
/// # Safety
///
/// `ctx` is a live `AVCodecContext` for the call.
#[cfg(test)]
pub(crate) unsafe fn skips_of(ctx: *const ffmpeg_next::ffi::AVCodecContext) -> (i32, i32, i32) {
  // SAFETY: the caller's contract; the three are `AVDiscard` fields, read
  // as integers so no enum is formed from FFmpeg memory.
  unsafe {
    (
      ptr::addr_of!((*ctx).skip_frame).cast::<i32>().read(),
      ptr::addr_of!((*ctx).skip_loop_filter).cast::<i32>().read(),
      ptr::addr_of!((*ctx).skip_idct).cast::<i32>().read(),
    )
  }
}

#[cfg(test)]
impl VideoDecoder {
  /// [`skips_of`] this decoder's active context.
  pub(crate) fn skips_for_test(&self) -> (i32, i32, i32) {
    // SAFETY: `state.inner` owns a live, opened context for this borrow.
    unsafe { skips_of(self.state.inner.as_ptr()) }
  }
}

#[cfg(test)]
mod tests;
