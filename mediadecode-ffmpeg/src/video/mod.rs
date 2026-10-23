//! `mediadecode::VideoStreamDecoder` impl over FFmpeg's hardware and
//! software decoders.
//!
//! [`FfmpegVideoStreamDecoder`] opens on the [`DecodePath`] it is given:
//!
//! * **`Auto`** — what `open` does — probes the platform's hardware
//!   backends in order through [`crate::VideoDecoder`]. When none of them
//!   takes the stream before its first picture, it falls back to a
//!   **software** `ffmpeg::decoder::Video` opened from the same
//!   `Parameters`, and that fallback is lossless: the probe keeps every
//!   packet it consumed and hands them back in
//!   [`ProbeExhausted`](crate::Error::ProbeExhausted), and they are
//!   replayed into the software decoder — then the packet the probe
//!   refused — before anything else, so a non-seekable input loses
//!   nothing. A probe that exhausts at `open` opens software at once.
//! * **`AnyHardware`** is the same probe with software taken away: when no
//!   backend takes the stream before its first picture, the session
//!   reports [`ProbeExhausted`](crate::Error::ProbeExhausted) with
//!   the packets the probe took (none, when no backend opens at all), and
//!   the caller decides what to replay them into.
//! * **`Hardware(b)`** is that one backend or nothing, committed at open.
//! * **`Software`** is libavcodec's own decoder, with no probe.
//!
//! # After the first picture, nothing changes the road
//!
//! Once a backend has committed — at its first picture, or at open when
//! it was named — the session stays on it, and nothing is classified.
//! Every decoder failure ([`VideoDecodeError::Decode`]) is that picture's
//! own error, reported as it was minted, and nothing of it is remembered,
//! so the next call reaches libavcodec. Whether a hardware session
//! recovers is FFmpeg's: after a VideoToolbox restart that fails, every
//! picture fails the same way until a new parameter set re-arms the
//! restart, and `flush` rebuilds no hardware session (see
//! [`crate::VideoDecoder`] for the lines). This wrapper neither waits for
//! a recovery nor rules one out.
//!
//! # Three answers end the session
//!
//! [`Error::AmbiguousSend`](crate::Error::AmbiguousSend), where libavcodec's
//! `ENOMEM` left unknown whether the hardware decoder took a packet only it
//! held, hands back every packet the decoder may not have taken.
//! [`Error::ProbeExhausted`](crate::Error::ProbeExhausted) on a path that
//! excludes software hands back every packet the probe took, and
//! [`Error::FallbackFailed`](crate::Error::FallbackFailed), where the
//! software decoder `Auto` falls back to could not take the stream, every
//! packet it rescued. After each the session is **spent**: no decoder is
//! left in it, hardware or software, and every send, end and receive
//! answers [`Error::Spent`](crate::Error::Spent) naming which, a `flush`
//! included. Going on means a new session, from what that error handed
//! back. No path takes an ambiguous send as the cue for software: after the
//! first picture nothing changes the road, and before it the decoder ends
//! so only for a refusal of this crate's own, which software meets the same
//! way.
//!
//! A [`VideoDecodeError::Convert`] is this wrapper's own failure to
//! convert a decoded picture into a frame, not the decoder's. One that
//! failed on an allocation parks the picture, and until a `receive_frame`
//! delivers it, `send_packet` and `send_eof` answer `Sent::MustDrain`
//! without reaching libavcodec: back pressure, not a failure.
//!
//! **`Auto` does not go on in software at that point.** A software
//! decoder that starts mid-stream holds no reference pictures, and after
//! the first picture this session keeps no packets to give it, so it
//! cannot switch without a gap. Whether to switch, and what gap to
//! accept, is the caller's: it sees the failures and what it has
//! delivered, and it decides what to keep. [`DecodePath`] describes the
//! simplest policy.
//!
//! Frames produced by either decoder are converted via
//! [`crate::convert::av_frame_to_video_frame`] so the consumer sees the
//! same `mediadecode::VideoFrame<PixelFormat, VideoFrameExtra,
//! FfmpegBytes>` shape regardless of which backend produced it.

use std::collections::VecDeque;

/// Maximum number of frames the SW fallback replay path will buffer
/// while draining the new SW decoder during packet/EOF replay.
/// Replaying many compressed packets through SW can produce hundreds
/// of decoded frames before the fallback commits; with no cap the
/// resident memory grows unbounded (e.g. 4K frames at ~12 MB each ×
/// 100s of frames). 64 frames is enough room to absorb every
/// realistic codec's reorder/lookahead window without becoming a
/// resource sink.
const SW_REPLAY_FRAME_CAP: usize = 64;

use derive_more::{IsVariant, TryUnwrap, Unwrap};
use ffmpeg_next::{Packet, codec::Parameters, frame};
use mediadecode::{
  Received, Sent, Timebase,
  decoder::{ScaledOutputCapability, VideoStreamDecoder},
  frame::VideoFrame,
  packet::VideoPacket,
};

use crate::{
  Backend, DecoderLimits, Error, Ffmpeg, Frame, VideoDecoder, boundary,
  convert::{self, ConvertError},
  decoder::{build_codec_context, try_clone_parameters},
  error::{FallbackFailed, Spent},
  extras::{VideoFrameExtra, VideoPacketExtra},
  frame::alloc_av_video_frame,
};

mod open;

pub use open::VideoStreamDecoderOpen;

/// Which decode path a video session takes — the choice
/// [`CarrierVideoStreamDecoder::open_as`] is given.
///
/// # Three words and a pin, and what each permits
///
/// - [`Auto`](Self::Auto): before the first picture, the platform's
///   hardware backends in probe order, the packets taken so far replayed
///   across them; when none takes the stream, software, the same packets
///   replayed into it. After it, the decoder that produced it. Only the
///   backends' own answers and their unfitness for the stream — a picture
///   downloaded in a layout a [`Frame`](crate::Frame) cannot carry — are its
///   cue for software, never the probe's own limits: a packet the probe
///   cannot keep for a replay commits the backend that takes it, picture or
///   none (see [`VideoDecoder`](crate::VideoDecoder)).
/// - [`AnyHardware`](Self::AnyHardware): the same probe, and never
///   software — when no backend takes the stream,
///   [`Error::ProbeExhausted`] hands back the packets the probe took.
///   After the first picture, the backend that produced it.
/// - [`Software`](Self::Software): libavcodec's own decoder, with no
///   probe, before and after.
/// - [`Hardware(b)`](Self::Hardware), the pin: backend `b`, committed at
///   open; nothing else is tried, before or after.
///
/// **After the first picture nothing changes the road, on any path, and
/// nothing is classified.** A decoder failure
/// ([`VideoDecodeError::Decode`]) is that picture's own error, reported as
/// it was minted, and nothing of it is remembered, so the next call
/// reaches libavcodec. Whether a hardware session recovers is FFmpeg's:
/// after a VideoToolbox restart that fails, every picture fails the same
/// way until a new parameter set re-arms the restart, and `flush` rebuilds
/// no hardware session. See [`VideoDecoder`](crate::VideoDecoder) for the
/// lines.
///
/// # When to stop trusting a hardware session
///
/// A caller that sees a hardware session's decoder failures persist
/// rebuilds: it opens a session on [`Software`](Self::Software) from the
/// same parameters and feeds it forward. When to do so is the caller's
/// policy, not this crate's, because it is the caller that sees the
/// failures on all three roads, the packets' key flags and what it has
/// delivered. What the new session can be fed depends on the road the
/// failure came from:
///
/// - **`send_packet`**: the failure names the packet in hand, and the new
///   session is given that packet.
/// - **`receive_frame`**: the failure may concern a packet accepted
///   earlier. FFmpeg decouples input from output and may hold several
///   pictures (`libavcodec/avcodec.h` 90–139 in FFmpeg 9.0.1), so no
///   packet is named: the new session is given the next packet, and the
///   caller accepts the gap.
/// - **`send_eof`**: the end has no packet to give a new session. What
///   the hardware session still held is recoverable only from packets
///   kept from before, or by a seek.
///
/// What a policy counts is the hardware session's decoder failures,
/// [`VideoDecodeError::Decode`]. A [`VideoDecodeError::Convert`] is this
/// wrapper's own: a decoded picture it could not convert into a frame (a
/// frame ceiling, a pixel format or plane layout it cannot carry, an
/// allocation). It is reported, not counted. One that failed on an
/// allocation parks the picture: the next `receive_frame` converts it
/// again, and until one delivers it `send_packet` and `send_eof` answer
/// [`Sent::MustDrain`] without reaching libavcodec. That is the wrapper's
/// back pressure, not a failure.
///
/// The example in the [crate documentation](crate) is the simplest such
/// policy:
///
/// - It counts the hardware session's decoder failures from all three
///   roads, and only a delivered picture ends the count.
/// - At a threshold of its own it opens [`Software`](Self::Software) from
///   the same parameters and feeds it forward, road by road as above.
///   Every picture the new session delivers is delivered as it is.
/// - On software its failures are reported and the stream goes on:
///   software is where the policy ends.
///
/// It keeps no packets, so the pictures from a failure to the next
/// keyframe are lost: the new session holds no reference pictures, and
/// libavcodec drops or conceals what comes before one. A caller that
/// cannot afford that gap keeps packets and replays them with a picture
/// identity of its own; that bookkeeping is the caller's design, and the
/// example does not show it.
///
/// The words differ in what they **permit**, not only where they start.
/// That is the difference the consumers of this door need. A determinism
/// comparison decodes *one stream* both ways and compares the pixels; a
/// run that silently swapped paths halfway would compare nothing and say
/// it had. An operator turning hardware off for a lane over a driver that
/// produces wrong pixels needs it to stay off. And a node that sends
/// hardware and software sessions to separate pools needs a hardware
/// session that never quietly becomes a software one — which is what
/// [`AnyHardware`](Self::AnyHardware) is for.
///
/// # Observability
///
/// [`is_hardware`](CarrierVideoStreamDecoder::is_hardware) and
/// [`is_software`](CarrierVideoStreamDecoder::is_software) read where a
/// session **is**, which stays a live reading — under
/// [`Auto`](Self::Auto) it can change once, during the probe, and under
/// the others it answers what was chosen because nothing can move it.
///
/// This type deliberately grows **no** `is_*` predicates of its own,
/// where most vocabularies in this crate do. They would spell the
/// decoder's two questions a second time with a different meaning —
/// `path.is_software()` is *what was asked for* and
/// `decoder.is_software()` is *where it ended up*, and under
/// [`Auto`](Self::Auto) those genuinely differ. A caller that needs to
/// branch on the choice it made already holds the value and can
/// `match` it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DecodePath {
  /// Probe the platform's hardware backends in order and fall back to
  /// software when none takes the stream before its first picture, the
  /// packets the probe consumed replayed into it so nothing is lost.
  /// After the first picture the session stays on the decoder that
  /// produced it.
  ///
  /// What [`CarrierVideoStreamDecoder::open`] does.
  Auto,
  /// **Any of the platform's hardware backends, and never software.**
  ///
  /// The same probe as [`Auto`](Self::Auto): the backends in the
  /// platform's probe order, the packets taken before the first picture
  /// replayed across them as each one is tried. Where `Auto` would then
  /// open software, this arm refuses — at
  /// [`open_as`](CarrierVideoStreamDecoder::open_as) when no backend
  /// opens (on a platform with no hardware backend at all, at once), and
  /// on the road that met it when the probe exhausts later — with
  /// [`Error::ProbeExhausted`] carrying every backend's attempt and the
  /// packets the probe took, so the caller can replay them into a
  /// software session of its own. After the first picture it is the
  /// backend that produced it, as on every path.
  ///
  /// [`is_hardware`](CarrierVideoStreamDecoder::is_hardware) answers
  /// `true` for the session's whole life.
  AnyHardware,
  /// **This hardware backend, or nothing.** No other backend is probed
  /// and software is never opened.
  ///
  /// A backend that cannot be opened for the stream fails the
  /// [`open_as`](CarrierVideoStreamDecoder::open_as) call. The backend is
  /// committed from open, so every failure after it is that picture's
  /// own error, as on every path.
  Hardware(Backend),
  /// **Software, with no probe at all.**
  ///
  /// Opens `libavcodec`'s own decoder for the stream directly. There is
  /// no hardware in this session to fail, so there is nothing for it to
  /// fall back from — the state [`Auto`](Self::Auto) reaches when its
  /// probe exhausts, entered on purpose.
  Software,
}

/// `mediadecode::VideoStreamDecoder` impl over FFmpeg's hardware and
/// software decoders, on the [`DecodePath`] it was opened on — under
/// [`DecodePath::Auto`], hardware with a lossless fallback to software
/// before the first picture.
pub struct CarrierVideoStreamDecoder<C: crate::FfmpegCarrier> {
  state: DecodeState,
  /// The path this session was opened on — see [`DecodePath`].
  ///
  /// Read for exactly one question, [`Self::may_open_software`]: whether
  /// a probe's exhaustion is this session's cue to open software or its
  /// cue to report. Kept as the whole choice rather than reduced to that
  /// bit so a session can say what it *is*, not only what it allows.
  path: DecodePath,
  /// Codec parameters retained so we can open a software
  /// `ffmpeg::decoder::Video` if the HW probe exhausts.
  parameters: Parameters,
  /// HW-side scratch frame (filled by [`VideoDecoder::receive_frame`]).
  hw_scratch: Frame,
  /// SW-side scratch frame (filled by `ffmpeg::decoder::Video::receive_frame`).
  sw_scratch: frame::Video,
  /// Frames produced while draining the SW decoder during fallback
  /// replay (see [`Self::fall_back_to_sw`]). The trait's
  /// `receive_frame` delivers from this queue before pulling new
  /// frames from the SW decoder. Empty in steady-state operation.
  sw_replay_frames: VecDeque<frame::Video>,
  /// Resource ceilings for the frames this decoder exports, and for the
  /// `AVCodecContext`s it opens — HW candidates, the SW fallback, and
  /// any decoder a later probe advance builds all get the same number.
  limits: DecoderLimits,
  /// How every `AVCodecContext` this session opens decodes — HW
  /// candidates, the SW fallback and a later probe advance's alike
  /// ([`crate::DecoderOptions`]).
  options: crate::DecoderOptions,
  /// `true` once `send_eof` has been called on the active decoder.
  /// Used to propagate EOF to the SW decoder when fallback fires
  /// during the drain phase — without this, codecs that hold tail
  /// frames at EOF would hang waiting for an EOF they already saw on
  /// the HW path.
  eof_sent: bool,
  /// Source-stream time base, used to label produced frames.
  time_base: Timebase,
  /// The lane this decoder captures into. A marker: the carrier
  /// appears in the frames it produces, not in its own state.
  /// `true` when the scratch frame holds a decoded frame whose
  /// conversion has **not committed** — see
  /// [`CarrierAudioStreamDecoder::scratch_pending`](crate::audio::CarrierAudioStreamDecoder)
  /// for the reasoning, which is the same on both roads.
  ///
  /// **This decoder has two scratches and can change which one is
  /// current, so the seat is enforced rather than merely recorded.**
  /// While it is set, `send_packet` and `send_eof` answer
  /// [`Sent::MustDrain`]: both are the roads that commit a
  /// hardware-to-software fallback, and a fallback under a parked frame
  /// would leave the retry reading the *other* scratch — delivering a
  /// stale frame, or refusing permanently and stranding a decoded one.
  /// Refusing makes the retry's state the state that parked it **by
  /// construction**, which is a stronger guarantee than remembering
  /// which road produced it.
  ///
  /// **The discipline is unchanged; only its spelling moved.** It was
  /// `VideoDecodeError::FramePending`, and the escape was already
  /// documented as "call `receive_frame`, or `flush` to abandon it" —
  /// which is to say it was back pressure wearing an error's clothes.
  /// Now it says so, and a caller can act on it without inspecting a
  /// backend-specific error type. The subtitle decoder keeps the same
  /// seat one road over, spelled the same way.
  scratch_pending: bool,
  _carrier: core::marker::PhantomData<C>,
}

/// Hardware-decode seam behind [`DecodeState::Hw`]. In production this is
/// the real [`VideoDecoder`]; tests substitute a fake to drive the
/// probe-era fallback and the committed road without a live GPU. Mirrors
/// the subset of `VideoDecoder`'s surface the wrapper drives on the HW
/// path.
pub(crate) trait HwInner: Send {
  /// See [`VideoDecoder::send_av_packet`].
  ///
  /// # Safety
  ///
  /// [`VideoDecoder::send_av_packet`]'s contract: `packet` meets the
  /// `AVPacket` contract with every exported byte written, the padding
  /// after its payload is zeroed or libavformat's own, and nothing writes
  /// through its payload or that padding while the decoder may still read
  /// them.
  unsafe fn send_packet(&mut self, packet: &Packet) -> Result<Sent, Error>;
  /// See [`VideoDecoder::receive_frame`].
  fn receive_frame(&mut self, frame: &mut Frame) -> Result<Received, Error>;
  /// See [`VideoDecoder::send_eof`].
  fn send_eof(&mut self) -> Result<Sent, Error>;
  /// See [`VideoDecoder::flush`]. Returns `Result` for a uniform seam even
  /// though the inherent method is infallible.
  fn flush(&mut self) -> Result<(), Error>;
  /// Downcast to the concrete [`VideoDecoder`] when this seam is the real
  /// HW decoder, so [`FfmpegVideoStreamDecoder::hardware_inner`] can keep
  /// exposing it. Returns `None` for a test fake.
  fn as_video_decoder(&self) -> Option<&VideoDecoder>;

  /// See [`VideoDecoder::scaled_output_capability`].
  ///
  /// Defaulted to the refusal so a test fake — which has no
  /// VideoToolbox road behind it, and therefore no stage — answers
  /// honestly without having to say so.
  fn scaled_output_capability(&self) -> ScaledOutputCapability {
    ScaledOutputCapability::Unsupported
  }

  /// See [`VideoDecoder::request_scaled_output`]. Defaulted to the
  /// refusal, for the same reason as above.
  fn request_scaled_output(&mut self, size: (u32, u32)) -> ScaledOutputCapability {
    let _ = size;
    ScaledOutputCapability::Unsupported
  }

  /// See [`VideoDecoder::cancel_scaled_output`]. Defaulted to nothing,
  /// because a seat that never accepts a request has none to withdraw.
  fn cancel_scaled_output(&mut self) {}
}

impl HwInner for VideoDecoder {
  #[inline]
  unsafe fn send_packet(&mut self, packet: &Packet) -> Result<Sent, Error> {
    // SAFETY: the seam's contract is `send_av_packet`'s, forwarded.
    unsafe { VideoDecoder::send_av_packet(self, packet) }
  }
  #[inline]
  fn receive_frame(&mut self, frame: &mut Frame) -> Result<Received, Error> {
    VideoDecoder::receive_frame(self, frame)
  }
  #[inline]
  fn send_eof(&mut self) -> Result<Sent, Error> {
    VideoDecoder::send_eof(self)
  }
  #[inline]
  fn flush(&mut self) -> Result<(), Error> {
    VideoDecoder::flush(self);
    Ok(())
  }
  #[inline]
  fn as_video_decoder(&self) -> Option<&VideoDecoder> {
    Some(self)
  }
  #[inline]
  fn scaled_output_capability(&self) -> ScaledOutputCapability {
    VideoDecoder::scaled_output_capability(self)
  }
  #[inline]
  fn request_scaled_output(&mut self, size: (u32, u32)) -> ScaledOutputCapability {
    VideoDecoder::request_scaled_output(self, size)
  }
  #[inline]
  fn cancel_scaled_output(&mut self) {
    VideoDecoder::cancel_scaled_output(self);
  }
}

/// Internal: which backend is currently driving the decode — every entry
/// matches it first.
enum DecodeState {
  /// Hardware-backed decoder. Under [`DecodePath::Auto`] it may
  /// transition to `Sw` on its probe's `ProbeExhausted`, before the
  /// first picture, and at no other time; to `Spent` where that fallback
  /// fails, where the probe exhausts on a path that excludes software, and
  /// where it ends on an ambiguous send. Boxed behind [`HwInner`] so tests
  /// can inject a fake HW decoder.
  Hw(Box<dyn HwInner>),
  /// Software decoder. Terminal for the road: nothing leaves it.
  Sw(SwDecoder),
  /// **No decoder is left**: the session answered the error this names,
  /// which handed back what it held. Terminal: every send, end and receive
  /// answers [`Error::Spent`], and a flush leaves it so. The hardware
  /// decoder that answered it was dropped at the transition, so nothing
  /// exhausted or ended stays installed.
  Spent(Spent),
}

/// A software decoder and the callback state its codec context points
/// at.
///
/// The state carries the allocator judge's byte budget and the
/// `get_format` declination; it has to outlive the `AVCodecContext`
/// that references it, which is why it is a field here rather than a
/// value dropped at the end of `open_sw_decoder`.
///
/// `Deref` so that every call site keeps talking to the decoder and
/// only the construction changed — this pairing is a lifetime fact, not
/// a new abstraction.
pub(crate) struct SwDecoder {
  decoder: ffmpeg_next::decoder::Video,
  /// Declared **after** the decoder: fields drop in declaration order,
  /// so the codec context is freed before the state it points at, which
  /// this owns through the context's own pointer — see
  /// [`InstalledCallbackState`](crate::decoder::InstalledCallbackState).
  _callback_state: crate::decoder::InstalledCallbackState,
}

impl SwDecoder {
  /// The callback state this decoder's codec context points at.
  ///
  /// Handed out as a raw pointer so an error closure can consult it
  /// while the decoder itself is mutably borrowed — every software send
  /// / receive / EOF failure on this road goes through
  /// [`crate::decoder::software_exit`] with it, so a frame the
  /// allocator judge refused surfaces named instead of as the `EINVAL`
  /// libavcodec also uses for corrupt input.
  ///
  /// `Deref` alone was not enough: it exposes the decoder and hides the
  /// state, so every call site kept wrapping raw and the budget refusal
  /// had no way out on the whole software road — including the replay
  /// helper, which drops the state when it finishes.
  pub(crate) fn state(&self) -> *const crate::ffi::CallbackState {
    self._callback_state.as_ptr()
  }
}

impl core::ops::Deref for SwDecoder {
  type Target = ffmpeg_next::decoder::Video;
  fn deref(&self) -> &Self::Target {
    &self.decoder
  }
}

impl core::ops::DerefMut for SwDecoder {
  fn deref_mut(&mut self) -> &mut Self::Target {
    &mut self.decoder
  }
}

impl<C: crate::FfmpegCarrier + crate::CarrierOps> CarrierVideoStreamDecoder<C> {
  /// Opens a decoder for the given codec parameters with the default
  /// HW backend probe order. If the HW probe can't open any backend,
  /// falls back to a software `ffmpeg::decoder::Video` immediately —
  /// `open` only returns `Err` when both paths fail.
  ///
  /// A probe that exhausts later, before the first picture, triggers the
  /// same software fallback with its rescued packets replayed. After the
  /// first picture nothing changes the road — see [`DecodePath`].
  ///
  /// `limits` bounds what one decoded frame may cost. It is taken here
  /// rather than through a builder because half of it —
  /// [`DecoderLimits::max_pixels`] — is written into every
  /// `AVCodecContext` this decoder opens, and a context's ceiling
  /// cannot be moved after `avcodec_open2`. That includes the contexts
  /// opened later, by a probe-era fallback or a probe advance: the
  /// limits are retained for exactly that reason.
  pub(crate) fn open_impl(
    parameters: Parameters,
    time_base: Timebase,
    limits: DecoderLimits,
  ) -> Result<Self, Error> {
    Self::open_from_impl(VideoStreamDecoderOpen::new(), parameters, time_base, limits)
  }

  /// [`Self::open_impl`], with the decode path chosen rather than
  /// probed. `DecodePath::Auto` is the constructor above, verbatim.
  pub(crate) fn open_as_impl(
    parameters: Parameters,
    time_base: Timebase,
    limits: DecoderLimits,
    path: DecodePath,
  ) -> Result<Self, Error> {
    Self::open_from_impl(
      VideoStreamDecoderOpen::new().with_path(path),
      parameters,
      time_base,
      limits,
    )
  }

  /// The session `request` names: the one road `open` and `open_as`
  /// take.
  pub(crate) fn open_from_impl(
    request: VideoStreamDecoderOpen,
    parameters: Parameters,
    time_base: Timebase,
    limits: DecoderLimits,
  ) -> Result<Self, Error> {
    Self::open_from_in(
      request,
      parameters,
      time_base,
      limits,
      crate::backend::probe_order(),
    )
  }

  /// [`Self::open_from_impl`] with the probe order named — the
  /// platform's own for every public constructor, and an empty one for
  /// the lane that stands on a platform with no hardware backend at all.
  pub(crate) fn open_from_in(
    request: VideoStreamDecoderOpen,
    parameters: Parameters,
    time_base: Timebase,
    limits: DecoderLimits,
    order: &[Backend],
  ) -> Result<Self, Error> {
    let (path, options) = (request.path(), request.options());
    // ffmpeg-next's `Parameters` carries an optional `owner: Rc<dyn Any>`
    // (when constructed from `stream.parameters()` it points back at
    // the demuxer's `AVStream`). Upstream marks the type `Send`
    // anyway, which is unsound the moment a non-`None` owner is in
    // play — moving such a value across threads moves the `Rc`. We
    // sidestep this by always storing a deep-cloned `Parameters`
    // (`avcodec_parameters_copy` produces an owner-free copy), so
    // the `FfmpegVideoStreamDecoder`'s `Send` reachability never
    // depends on the caller's owner discipline.
    //
    // Use `try_clone_parameters` instead of `Parameters::clone` —
    // ffmpeg-next's `clone` calls `Parameters::new()` which can
    // return a `Parameters` whose inner pointer is null on OOM
    // (`avcodec_parameters_alloc` returns null without indication);
    // the subsequent `avcodec_parameters_copy` against that null
    // destination is C UB. Our checked helper surfaces the OOM as
    // an error instead.
    let owned_parameters = try_clone_parameters(&parameters, limits.max_codec_parameter_bytes())?;
    let hw_scratch = Frame::empty()?;
    let sw_scratch = alloc_av_video_frame()?;
    let state = match path {
      // The probe, under every skip policy: it weighs its candidates'
      // answers alone, and a clean end with no picture completes one
      // (see `VideoDecoder`).
      DecodePath::Auto => match VideoDecoder::open_probing(
        try_clone_parameters(&owned_parameters, limits.max_codec_parameter_bytes())?,
        limits,
        options,
        Some(time_base),
        order,
      ) {
        Ok(hw) => DecodeState::Hw(Box::new(hw)),
        Err(Error::ProbeExhausted(_)) => {
          // Open-time HW exhaustion: no rescued packets (open didn't
          // see any). Just open SW directly from our owned copy.
          let sw = open_sw_decoder(&owned_parameters, limits, options, Some(time_base))?;
          DecodeState::Sw(sw)
        }
        Err(other) => return Err(other),
      },
      // **The same probe, and no software behind it.** An exhaustion at
      // open is the answer — `ProbeExhausted` with every backend's
      // attempt and no packet, since none was sent — where `Auto` would
      // have read it as its cue to open software.
      DecodePath::AnyHardware => DecodeState::Hw(Box::new(VideoDecoder::open_probing(
        try_clone_parameters(&owned_parameters, limits.max_codec_parameter_bytes())?,
        limits,
        options,
        Some(time_base),
        order,
      )?)),
      // **The named backend, and no probe order at all.** Nothing is
      // tried before it and nothing after it, which is what makes the
      // arm a pin: an open that fails is the answer, where `Auto` would
      // have read the same failure as a reason to look elsewhere.
      DecodePath::Hardware(backend) => {
        DecodeState::Hw(Box::new(VideoDecoder::open_with_limits_timed_in(
          try_clone_parameters(&owned_parameters, limits.max_codec_parameter_bytes())?,
          backend,
          limits,
          options,
          Some(time_base),
        )?))
      }
      // The software decoder, opened on purpose rather than reached by
      // degrading. `DecodeState::Sw` is terminal, so this session has
      // nothing to keep it on its path but the shape of the state
      // machine itself.
      DecodePath::Software => DecodeState::Sw(open_sw_decoder(
        &owned_parameters,
        limits,
        options,
        Some(time_base),
      )?),
    };
    Ok(Self {
      state,
      path,
      parameters: owned_parameters,
      hw_scratch,
      sw_scratch,
      sw_replay_frames: VecDeque::new(),
      eof_sent: false,
      time_base,
      limits,
      options,
      scratch_pending: false,
      _carrier: core::marker::PhantomData,
    })
  }

  /// Returns `true` when this decoder has fallen back to the software
  /// path. `false` while still on the HW probe (the initial state), and once
  /// the session is spent.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub(crate) const fn is_software_impl(&self) -> bool {
    matches!(self.state, DecodeState::Sw(_))
  }

  /// Returns `true` while a hardware decoder drives the session: on trial,
  /// or committed. `false` once the session is spent.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub(crate) const fn is_hardware_impl(&self) -> bool {
    matches!(self.state, DecodeState::Hw(_))
  }

  /// Whether this session can currently honor a
  /// [`Self::request_scaled_output_impl`] request. See
  /// [`ScaledOutputCapability`] for the determinism trade a caller
  /// takes on by requesting one.
  ///
  /// **[`ScaledOutputCapability::Supported`] on exactly one road: a
  /// live VideoToolbox session on an Apple target.** There, a
  /// `VTPixelTransferSession` sits between the decoded hardware frame
  /// and `av_hwframe_transfer_data` and resizes the `CVPixelBuffer` on
  /// the GPU, so the fitted picture is what crosses to the CPU — see
  /// [`crate::vtscale`] for the design, and
  /// [mediadecode#55](https://github.com/findit-studio/mediadecode/issues/55)
  /// for the ruling that chose it. Everything else answers
  /// `Unsupported`, and each refusal has its own reason rather than a
  /// shared shrug:
  ///
  /// - **A session that fell back to software during its probe.** The
  ///   stage is the hardware road's; this answer follows the session, so
  ///   it flips to `Unsupported` the moment the fallback commits, and a
  ///   caller that asks again learns it.
  /// - **The other hardware backends.** [`Backend::Vaapi`],
  ///   [`Backend::Cuda`] and [`Backend::D3d11va`] are wired in source
  ///   (`Backend::av_hwdevice_type`, `probe_order`) but cannot be
  ///   compiled, run or verified on a non-Linux, non-Windows host, and
  ///   each has a native scaling seam of its own that this crate has
  ///   not built: NVDEC/CUVID in-decode scaling
  ///   ([#56](https://github.com/findit-studio/mediadecode/issues/56)),
  ///   VAAPI VPP
  ///   ([#57](https://github.com/findit-studio/mediadecode/issues/57)),
  ///   the D3D11 Video Processor
  ///   ([#58](https://github.com/findit-studio/mediadecode/issues/58)).
  ///   Filed rather than fabricated.
  /// - **Software.** See [`Self::request_scaled_output_impl`] for the
  ///   software road's own, separate refusal.
  ///
  /// What the VideoToolbox road did **not** get is decode-time
  /// scaling, and the distinction is worth keeping: inter prediction
  /// needs full-resolution reference frames, so every road decodes full
  /// size internally. What this seam saves is the GPU→CPU crossing and
  /// the CPU frame at the end of it — roughly thirtyfold on a 4K stream
  /// fitted to a 512-class box. A caller-owned `VTDecompressionSession`
  /// would save the same crossing and no more, which is why it stays
  /// #55's standing future enhancement rather than this release's work.
  ///
  /// A pure query: calling it requests nothing and changes nothing
  /// about what [`Self::receive_frame`] delivers.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub(crate) fn scaled_output_capability_impl(&self) -> ScaledOutputCapability {
    match &self.state {
      DecodeState::Hw(hw) => hw.scaled_output_capability(),
      DecodeState::Sw(_) | DecodeState::Spent(_) => ScaledOutputCapability::Unsupported,
    }
  }

  /// Requests that this session emit pictures at `size` from the next
  /// frame on. See [`Self::scaled_output_capability_impl`] for which
  /// road can honor it at all.
  ///
  /// # A parked frame refuses
  ///
  /// While [`Self::scratch_pending`] holds a decoded picture whose
  /// conversion did not commit, this refuses. That frame is already
  /// decided — the retry delivers it from the scratch without
  /// consulting the stage — so accepting a new size would promise an
  /// extent the very next frame cannot have. Drain it and ask again;
  /// the same escape every other seat guarded by that flag offers.
  ///
  /// The refusal carries the same meaning as every other: the session
  /// returns to full coded size, so any request already standing is
  /// withdrawn. The parked picture keeps the extent it was decoded at.
  ///
  /// # When a mid-stream request takes effect
  ///
  /// On the **next** picture [`Self::receive_frame`] produces. The
  /// stage is consulted per frame, on the way out of the hardware
  /// decoder and before the GPU→CPU download, so a request never
  /// reaches back to a picture already decoded and never waits longer
  /// than the one being decoded now.
  ///
  /// # The two refusals this seat mints itself
  ///
  /// Neither is an error, and each **returns the session to full coded
  /// size**, dropping any request already standing — what the trait
  /// says this answer means, and the only reading a caller can act on
  /// without risking a second resample of an already-fitted picture:
  ///
  /// - **A zero extent.** A zero-extent picture is not a smaller
  ///   picture.
  /// - **An upscale.** The stage exists to move fewer bytes across the
  ///   GPU→CPU bus; enlarging moves more, and inventing detail the
  ///   decoder did not produce is the caller's business rather than a
  ///   decode session's. An *equal* size is not an upscale: it is
  ///   accepted, and the stage simply has nothing to do.
  ///
  /// # The software road's refusal has its own, different shape
  ///
  /// Worth naming rather than folding into "no backend does this yet":
  /// FFmpeg's software decoders have no *general* decode-time scaling
  /// seam. The one option that comes close — `AVCodecContext.lowres`
  /// (the CLI's `-lowres`) — falls short on three separate counts, any
  /// one of which would disqualify it as this seam's software answer:
  ///
  /// 1. **Narrow codec coverage.** `lowres` is wired only into the
  ///    legacy MPEG-family decoders (MPEG-1/2/4 part 2, H.263) that
  ///    still carry the low-resolution IDCT machinery it depends on.
  ///    HEVC, AV1 and VP9 — the codecs a modern HDR pipeline actually
  ///    decodes — implement no `lowres` support at all.
  /// 2. **The one codec that is wired is broken.** `lowres` on H.264
  ///    (also nominally covered) has been non-functional for years —
  ///    the decoder does not honor it correctly — so even the "old
  ///    family" half of the promise does not hold across the board.
  /// 3. **It is not a resize, it is reduced reconstruction.** Where it
  ///    does work, `lowres` decodes at a coarser IDCT precision
  ///    (`1<<lowres`), skipping reconstruction detail rather than
  ///    decoding in full and scaling the result — later inter frames
  ///    drift from a reference the decoder itself degraded, which is a
  ///    different (and worse) contract than "the same picture, smaller".
  ///
  /// So the software road's answer is not "unimplemented" the way the
  /// other hardware backends' is — it is "full-size decode, then the
  /// fused conform walk downstream", by design, on every codec this
  /// crate decodes in software.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub(crate) fn request_scaled_output_impl(&mut self, size: (u32, u32)) -> ScaledOutputCapability {
    // **A parked frame is already decided, so it may not be
    // re-promised.** [`Self::scratch_pending`] means a picture came out
    // of the decoder and its conversion did not commit; the retry
    // delivers *that* frame from the scratch without going back through
    // the stage, so it will arrive at whatever extent it already has.
    // Accepting a new size here would answer `Supported` and then hand
    // the caller a frame the new request never touched — the exact
    // silent mismatch [`Self::scaled_output_capability_impl`]'s promise
    // exists to rule out. Refusing changes nothing, which is the
    // contract for a refusal, and the caller's escape is the one this
    // seat already documents everywhere else: drain the frame, then ask
    // again.
    if self.scratch_pending {
      tracing::debug!(
        requested_width = size.0,
        requested_height = size.1,
        "mediadecode-ffmpeg: scaled-output request refused while a decoded frame is parked; \
         the session returns to full size — drain it and ask again"
      );
      // **A refusal means the same thing here as anywhere else.**
      // Returning `Unsupported` while an earlier request stayed armed
      // would leave the caller resampling pictures this session went on
      // fitting — the very double-scale the word exists to prevent. So
      // the standing request goes, and with it what was built for it.
      // The parked picture keeps the extent it was decoded at, which is
      // the same rule an *acceptance* has always carried.
      if let DecodeState::Hw(hw) = &mut self.state {
        hw.cancel_scaled_output();
      }
      return ScaledOutputCapability::Unsupported;
    }
    match &mut self.state {
      DecodeState::Hw(hw) => hw.request_scaled_output(size),
      DecodeState::Sw(_) | DecodeState::Spent(_) => ScaledOutputCapability::Unsupported,
    }
  }

  /// Borrow the inner [`VideoDecoder`] when this decoder is still on the
  /// real HW path. Returns `None` after the SW fallback has fired (or, in
  /// tests, when the HW seam is a fake rather than a real decoder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub(crate) fn hardware_inner_impl(&self) -> Option<&VideoDecoder> {
    match &self.state {
      DecodeState::Hw(hw) => hw.as_video_decoder(),
      DecodeState::Sw(_) | DecodeState::Spent(_) => None,
    }
  }

  /// Returns the time base associated with the source stream.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub(crate) const fn time_base_impl(&self) -> Timebase {
    self.time_base
  }

  /// Whether this session may open a software decoder in answer to its
  /// probe's exhaustion.
  ///
  /// **The one place a path that excludes software is enforced**,
  /// consulted by all three roads that can meet a probe-era
  /// [`Error::ProbeExhausted`] — the two send arms and the receive
  /// arm. It is one predicate rather than three conditions because the
  /// promise is one: a session opened on [`DecodePath::AnyHardware`] or
  /// [`DecodePath::Hardware`] ends on hardware or ends in an error, and
  /// a road that forgot to ask would break that promise silently, which
  /// is the failure mode a caller cannot see.
  ///
  /// It is a question for the probe era only. After the first picture no
  /// path opens software: a failure there is that picture's own error, on
  /// every path.
  ///
  /// [`DecodePath::Software`] answers `true` and it costs nothing:
  /// `DecodeState::Sw` is terminal, so no hardware exhaustion can
  /// reach a road that asks. Answering for it by state rather than by
  /// path would make the predicate say something it does not mean.
  #[cfg_attr(not(tarpaulin), inline(always))]
  const fn may_open_software(&self) -> bool {
    matches!(self.path, DecodePath::Auto | DecodePath::Software)
  }

  /// Internal: **probe-era** transition from HW to SW. Replays the rescued
  /// packets (the inner decoder's buffered history, already accepted by the HW
  /// probe but not yet decoded) through the new SW decoder so the stream resumes
  /// seamlessly. No frame was delivered on the HW path yet, so replaying the
  /// history is lossless.
  ///
  /// It is the only transition from hardware to software, and only
  /// [`DecodePath::Auto`] takes it: after the first picture nothing
  /// changes the road.
  ///
  /// **Transactional**: drained replay frames accumulate in a local
  /// queue; we only commit them to `self.sw_replay_frames` and switch
  /// `self.state` to `Sw` after the replay (and EOF re-forwarding, if
  /// needed) succeed. On failure, the SW decoder and the local frame
  /// queue are dropped, every rescued packet goes back with
  /// [`Error::FallbackFailed`], and **the session is spent**
  /// ([`DecodeState::Spent`]): the exhausted hardware decoder is dropped
  /// with it, so no decoder is left installed that could take a packet
  /// (Codex R2, finding 3), and every entry after answers
  /// [`Error::Spent`] with [`Spent::FallbackFailed`].
  ///
  /// **EOF-aware**: when EOF was already accepted on the HW path
  /// (`self.eof_sent`), the new SW decoder also receives `send_eof()`
  /// after replay. Without this, codecs that delay tail frames hang
  /// forever in the drain phase.
  ///
  /// **EAGAIN-aware**: if SW's `send_packet` returns EAGAIN during
  /// replay, drain produced frames into the local queue and retry.
  ///
  /// `eof_pending` is passed as a **local** argument rather than read from
  /// `self.eof_sent`: callers must not mutate `self.eof_sent` before this
  /// transaction commits (see [`VideoStreamDecoder::send_eof`]), so the
  /// in-transaction SW EOF re-forward keys off the local flag and `self`'s
  /// EOF state is updated only after a clean commit.
  fn fall_back_to_sw(
    &mut self,
    unconsumed_packets: std::vec::Vec<ffmpeg_next::Packet>,
    eof_pending: bool,
  ) -> Result<(), Error> {
    tracing::info!(
      packets_replayed = unconsumed_packets.len(),
      eof_pending,
      "mediadecode-ffmpeg: HW probe exhausted, falling back to software decode",
    );
    // Wrap the internal worker so any failure path returns the
    // rescued packets to the caller via `Error::FallbackFailed`.
    // Without this, non-seekable streams (live feeds, pipes) would
    // lose every compressed byte the HW path had consumed when a
    // fallback transition fails partway.
    match self.fall_back_to_sw_inner(&unconsumed_packets, eof_pending) {
      Ok(()) => Ok(()),
      Err(source) => {
        self.state = DecodeState::Spent(Spent::FallbackFailed);
        Err(Error::FallbackFailed(FallbackFailed::new(
          Box::new(source),
          unconsumed_packets,
        )))
      }
    }
  }

  /// Worker for [`Self::fall_back_to_sw`]. Returns the rescued packets
  /// untouched on the borrowed slice; the wrapper takes ownership of
  /// them and surfaces them in `FallbackFailed` if this returns Err.
  fn fall_back_to_sw_inner(
    &mut self,
    unconsumed_packets: &[ffmpeg_next::Packet],
    eof_pending: bool,
  ) -> Result<(), Error> {
    let mut sw = open_sw_decoder(
      &self.parameters,
      self.limits,
      self.options,
      Some(self.time_base),
    )?;
    // Bound before the decoder is mutably borrowed, so the error
    // closures below can still consult it.
    let sw_state = sw.state();
    let mut local_replay: VecDeque<frame::Video> = VecDeque::new();
    // Helper: drain SW into the local replay queue, capped at
    // `SW_REPLAY_FRAME_CAP`.
    //
    // Error discipline: stop the drain **only** on the transient
    // backpressure signals EAGAIN / EOF (the decoder has no more output for
    // now). Every other `ffmpeg_next::Error` — e.g. `InvalidData` from a
    // corrupt replayed packet — is a real decode failure and is propagated,
    // so a non-recoverable error surfaces as `FallbackFailed` (carrying the
    // replay packets) instead of being silently swallowed and the fallback
    // committed over corruption.
    //
    // # Safety
    //
    // `state` must be null or point at a `CallbackState` that stays live
    // for the duration of the call — in practice `sw`'s own.
    unsafe fn drain_into(
      sw: &mut ffmpeg_next::decoder::Video,
      state: *const crate::ffi::CallbackState,
      local_replay: &mut VecDeque<frame::Video>,
    ) -> std::result::Result<(), Error> {
      loop {
        let mut tmp = alloc_av_video_frame()?;
        match sw.receive_frame(&mut tmp) {
          Ok(()) => {
            if local_replay.len() >= SW_REPLAY_FRAME_CAP {
              tracing::error!(
                cap = SW_REPLAY_FRAME_CAP,
                "mediadecode-ffmpeg: SW fallback replay produced more frames than the \
                 replay cap allows; aborting fallback (no frames dropped — they're \
                 still in the SW decoder's internal queue and will be released when \
                 it drops)",
              );
              return Err(Error::Ffmpeg(ffmpeg_next::Error::Other {
                errno: libc::ENOMEM,
              }));
            }
            local_replay.push_back(tmp);
          }
          // EAGAIN / EOF: no more output for now — stop draining, success.
          Err(ffmpeg_next::Error::Other { errno }) if errno == ffmpeg_next::error::EAGAIN => {
            break;
          }
          Err(ffmpeg_next::Error::Eof) => break,
          // Any other error is a genuine decode failure on a replayed
          // packet — surface it so it is not masked as a clean fallback.
          //
          // SAFETY: this function's own contract, forwarded.
          Err(other) => return Err(unsafe { crate::decoder::software_exit(state, other) }),
        }
      }
      Ok(())
    }

    // **Through the one door, and transactional by class**
    // ([`crate::decoder::Acceptance`]): a packet taken goes on, and back
    // pressure drains and offers it again. Any other answer — taken with the
    // decoder's error, `ENOMEM` and so perhaps taken, or not taken — ends the
    // transaction: the software decoder is dropped, and every rescued packet
    // goes back with `FallbackFailed`, so nothing goes on past a packet this
    // decoder may or may not hold.
    for pkt in unconsumed_packets {
      let mut attempts: u32 = 0;
      loop {
        match crate::decoder::send(&mut sw, crate::decoder::Sending::Packet(pkt)) {
          crate::decoder::Acceptance::Accepted => break,
          crate::decoder::Acceptance::NotAccepted(ffmpeg_next::Error::Other { errno })
            if errno == ffmpeg_next::error::EAGAIN =>
          {
            // SAFETY: `sw_state` is `sw`'s own callback state, and `sw`
            // lives until the commit at the end of this function.
            unsafe { drain_into(&mut sw, sw_state, &mut local_replay) }?;
            attempts += 1;
            if attempts > 16 {
              return Err(Error::Ffmpeg(ffmpeg_next::Error::Other {
                errno: ffmpeg_next::error::EAGAIN,
              }));
            }
          }
          crate::decoder::Acceptance::NotAccepted(other)
          | crate::decoder::Acceptance::Ambiguous(other)
          | crate::decoder::Acceptance::AcceptedWithError(other) => {
            // SAFETY: `sw_state` is `sw`'s own callback state, live here.
            return Err(unsafe { crate::decoder::software_exit(sw_state, other) });
          }
        }
      }
    }
    // Re-forward EOF if the HW path already saw it. SW EOF can also
    // return EAGAIN until prior output is drained — mirror the
    // packet-replay loop.
    if eof_pending {
      let mut attempts: u32 = 0;
      loop {
        match crate::decoder::send(&mut sw, crate::decoder::Sending::End) {
          crate::decoder::Acceptance::Accepted => break,
          crate::decoder::Acceptance::NotAccepted(ffmpeg_next::Error::Other { errno })
            if errno == ffmpeg_next::error::EAGAIN =>
          {
            // SAFETY: `sw_state` is `sw`'s own callback state, and `sw`
            // lives until the commit at the end of this function.
            unsafe { drain_into(&mut sw, sw_state, &mut local_replay) }?;
            attempts += 1;
            if attempts > 16 {
              return Err(Error::Ffmpeg(ffmpeg_next::Error::Other {
                errno: ffmpeg_next::error::EAGAIN,
              }));
            }
          }
          crate::decoder::Acceptance::NotAccepted(other)
          | crate::decoder::Acceptance::Ambiguous(other)
          | crate::decoder::Acceptance::AcceptedWithError(other) => {
            // SAFETY: `sw_state` is `sw`'s own callback state, live here.
            return Err(unsafe { crate::decoder::software_exit(sw_state, other) });
          }
        }
      }
    }
    // Final drain BEFORE commit — the transactional commit boundary. The
    // EAGAIN-triggered drains above only fire when SW exerts backpressure mid
    // replay; a SW decoder that ACCEPTS every replayed packet (and the EOF)
    // without one then surfaces a non-transient error — `InvalidData` from a
    // corrupt replayed packet, or any other decode failure — only on the *next*
    // `receive_frame`. Without this drain that error would land after the
    // commit (frames appended, `state` flipped to `Sw`, rescued packets
    // dropped) and reach the caller as a plain decode failure, not
    // `FallbackFailed` — breaking probe-era recovery on non-seekable input.
    // Draining to EAGAIN/EOF here forces any such error to surface now, so it is
    // wrapped as `FallbackFailed` (retaining the rescued packets), nothing is
    // committed, and the session is spent.
    // SAFETY: `sw_state` is `sw`'s own callback state, and `sw` is still
    // here — it moves into `self.state` only below.
    unsafe { drain_into(&mut sw, sw_state, &mut local_replay) }?;
    // Commit: only after replay, any EOF forwarding, AND the final drain
    // succeeded do we move the new SW decoder and queue into `self`.
    self.sw_replay_frames.append(&mut local_replay);
    self.state = DecodeState::Sw(sw);
    Ok(())
  }

  /// The one place a delivered frame is committed.
  ///
  /// Every road that hands a frame to the caller passes through here —
  /// the hardware scratch, the software scratch, both replay-queue
  /// entries, and the retry of a parked frame — so the bookkeeping a
  /// delivery owes cannot be attached to some of them and forgotten on
  /// others.
  fn commit_delivery(
    &mut self,
    frame: VideoFrame<mediadecode::PixelFormat, VideoFrameExtra, C::Buffer>,
    dst: &mut VideoFrame<mediadecode::PixelFormat, VideoFrameExtra, C::Buffer>,
  ) {
    // The scratch is free once a carrier exists for what it held.
    self.scratch_pending = false;
    *dst = frame;
  }

  /// Where this session is. See
  /// [`SessionPhase`](crate::decoder::SessionPhase).
  ///
  /// The wrapper never sees a probe — that lives inside the hardware
  /// seam, which derives its own — so only the committed pair is
  /// reachable from here.
  const fn phase(&self) -> crate::decoder::SessionPhase {
    if self.eof_sent {
      crate::decoder::SessionPhase::Draining
    } else {
      crate::decoder::SessionPhase::Streaming
    }
  }

  /// Internal: convert the active scratch frame into a
  /// `mediadecode::VideoFrame` and write into `dst`.
  fn deliver_frame(
    &mut self,
    dst: &mut VideoFrame<mediadecode::PixelFormat, VideoFrameExtra, C::Buffer>,
  ) -> Result<Received, VideoDecodeError> {
    let av_frame = match &mut self.state {
      // SAFETY: only a const pointer is read out of the frame; nothing
      // `as_inner_mut`'s contract guards is touched.
      DecodeState::Hw(_) => unsafe { self.hw_scratch.as_inner_mut().as_ptr() },
      DecodeState::Sw(_) => unsafe { self.sw_scratch.as_ptr() },
      // Unreachable: a spent session parks nothing, since it is spent only
      // at an error, never at a picture.
      DecodeState::Spent(spent) => return Err(VideoDecodeError::Decode(Error::Spent(*spent))),
    };
    // SAFETY: the scratch frame is live — either just filled by the
    // inner decoder's `receive_frame` (libavcodec's decode, or
    // `av_hwframe_transfer_data` out of a hardware surface, wrote every
    // byte it exports), or left holding such a frame whose conversion
    // did not commit. Convert takes what it needs out of it, so the
    // scratch can be reused once this has committed.
    let converted = unsafe {
      convert::av_frame_to_video_frame_as::<C>(av_frame, self.time_base, self.limits.frame())
    };
    match converted {
      Ok(new_frame) => {
        self.commit_delivery(new_frame, dst);
        Ok(Received::Produced)
      }
      Err(e) => {
        // Park only what another attempt could survive.
        self.scratch_pending = crate::decoder::parks_in_decode(&e);
        Err(VideoDecodeError::Convert(e))
      }
    }
  }
}

#[cfg(test)]
impl<C: crate::FfmpegCarrier + crate::CarrierOps> CarrierVideoStreamDecoder<C> {
  /// Build a decoder around an injected HW seam, bypassing the real probe.
  /// Lets tests drive the probe-era fallback and the committed road with a
  /// [`HwInner`] fake instead of a live GPU. The SW fallback still opens
  /// the **real** `ffmpeg::decoder::Video` from `parameters`, so a fallback
  /// in these tests genuinely decodes.
  pub(crate) fn from_hw_inner_for_test(
    hw: Box<dyn HwInner>,
    parameters: Parameters,
    time_base: Timebase,
  ) -> Result<Self, Error> {
    Self::from_hw_inner_for_test_as(hw, parameters, time_base, DecodePath::Auto)
  }

  /// [`Self::from_hw_inner_for_test`], with the session's
  /// [`DecodePath`] named.
  ///
  /// The seam a **pinned** session's mid-stream behaviour is driven
  /// through: a pin's promise is about what happens when the hardware
  /// fails after opening, and the only way to reach that on a machine
  /// whose GPU works is to inject a seam that fails on demand. See
  /// `a_hardware_pin_reports_its_probes_exhaustion_instead_of_falling_back`.
  pub(crate) fn from_hw_inner_for_test_as(
    hw: Box<dyn HwInner>,
    parameters: Parameters,
    time_base: Timebase,
    path: DecodePath,
  ) -> Result<Self, Error> {
    Self::from_hw_inner_for_test_from(
      hw,
      VideoStreamDecoderOpen::new().with_path(path),
      parameters,
      time_base,
    )
  }

  /// [`Self::from_hw_inner_for_test_as`], with the session's whole open
  /// request named: the seam through which a probe-era fallback's
  /// options are proved.
  pub(crate) fn from_hw_inner_for_test_from(
    hw: Box<dyn HwInner>,
    request: VideoStreamDecoderOpen,
    parameters: Parameters,
    time_base: Timebase,
  ) -> Result<Self, Error> {
    let limits = DecoderLimits::default();
    let owned_parameters = try_clone_parameters(&parameters, limits.max_codec_parameter_bytes())?;
    Ok(Self {
      state: DecodeState::Hw(hw),
      path: request.path(),
      parameters: owned_parameters,
      hw_scratch: Frame::empty()?,
      sw_scratch: alloc_av_video_frame()?,
      sw_replay_frames: VecDeque::new(),
      eof_sent: false,
      time_base,
      limits,
      options: request.options(),
      scratch_pending: false,
      _carrier: core::marker::PhantomData,
    })
  }

  /// Whether `send_eof` has been committed on the active decoder. Lets the
  /// rollback tests assert that a failed EOF fallback restores (never
  /// half-mutates) `eof_sent`.
  pub(crate) const fn eof_sent_for_test(&self) -> bool {
    self.eof_sent
  }

  /// Whether the probe-era replay queue is empty — so a lane can tell a
  /// frame parked in the scratch from one still waiting in the queue.
  pub(crate) fn sw_replay_frames_is_empty_for_test(&self) -> bool {
    self.sw_replay_frames.is_empty()
  }
}

impl<C: crate::FfmpegCarrier + crate::CarrierOps> CarrierVideoStreamDecoder<C> {
  /// The fault a submission after end-of-stream earns on this face.
  ///
  /// **Censused from the empty-seat road rather than invented.** With
  /// the seat free, a post-EOF `send_packet` or a repeated `send_eof`
  /// reaches libavcodec, which answers `AVERROR_EOF`, and all four
  /// roads through this wrapper — hardware and software, packet and
  /// EOF — surface it as exactly this value. The gates below short
  /// out to the same one so a parked seat cannot change *which* answer
  /// a caller gets, only how quickly. `the_post_eof_fault_is_the_one_the_substrate_gives`
  /// pins the two against each other.
  ///
  /// Deliberately **not** a new `VideoDecodeError` arm. The subtitle
  /// seam had to mint `AfterEof` because `avcodec_decode_subtitle2` has
  /// no state machine to refuse for it; this face already has an answer
  /// for the condition, and a second spelling for one fault on one
  /// surface is the disease this release is curing.
  fn after_eof() -> VideoDecodeError {
    VideoDecodeError::Decode(Error::Ffmpeg(ffmpeg_next::Error::Eof))
  }

  pub(crate) fn send_packet_impl(
    &mut self,
    packet: &VideoPacket<VideoPacketExtra, C::Buffer>,
  ) -> Result<Sent, VideoDecodeError> {
    // **The state first**: a spent session answers by name, before any
    // other gate, since none of them can be satisfied by anything.
    if let DecodeState::Spent(spent) = self.state {
      return Err(VideoDecodeError::Decode(Error::Spent(spent)));
    }
    // **The end of the stream outranks the parked seat, and the order
    // is the whole point.**
    //
    // `Sent::MustDrain` is a promise: drain the output and this same
    // offer becomes acceptable. Past end-of-stream that promise is
    // false — draining empties the seat and the retry still faults,
    // until `flush`. Checking the seat first made the wrapper answer
    // `MustDrain` for a submission nothing could ever accept, which is
    // the same fault-under-back-pressure inversion the subtitle seam
    // carried: a caller obeying the contract loops, drains, re-offers,
    // and is refused anyway.
    //
    // It is reachable: `send_eof` is accepted and sets `eof_sent`, a
    // delayed tail frame comes out of the decoder, its carrier
    // allocation fails parkably, and the seat is taken on a session
    // that is already over.
    if !self.phase().accepts_input() {
      return Err(Self::after_eof());
    }
    // **Nothing is sent while a frame is parked.** Both send roads can
    // commit a hardware-to-software fallback, and a fallback under a
    // parked frame would leave the retry reading the other scratch. See
    // [`Self::scratch_pending`]. Nothing was consumed, so this is back
    // pressure and the packet is still the caller's to re-offer — which
    // is true precisely because the stream is not over, checked above.
    if self.scratch_pending {
      return Ok(Sent::MustDrain);
    }
    let phase = self.phase();
    // Scoped submission: the rebuilt `AVPacket` never leaves this call,
    // which is what lets the view lane share its buffer with libavcodec
    // rather than copy into it. See `boundary::with_ffmpeg_video_packet`.
    let limits = self.limits.packet_limits();
    // **One route, on every road.** The submission may share its
    // carrier's buffer — wherever `boundary::share_or_copy` can prove
    // the padding — in the probe window as after it, hardware or
    // software. libavcodec may keep a reference past the call and does
    // not write through it (`libavcodec/avcodec.h` 2333–2335 in FFmpeg
    // 9.0.1). The one thing that keeps what it is sent *and hands it
    // back* is the hardware probe, whose rescue history goes to the
    // caller as owned, mutable `Packet`s
    // (`ProbeExhausted::into_unconsumed_packets`); it records copies
    // of its own (`boundary::try_clone_packet`), so nothing it hands back
    // addresses a carrier.
    let route = crate::BodyRoute::Submission;
    let submit = |av_pkt: &Packet| {
      match &mut self.state {
        // SAFETY: `av_pkt` is the submission `with_ffmpeg_video_packet`
        // built from this call's carrier — its payload and side data are
        // this crate's writes, and its padding is either the zeroed
        // padding of a fresh copy or, where the body is shared, the
        // padding of a packet libavformat delivered (`share_or_copy`
        // shares nothing else) — lent for this call only, and a shared
        // body is reachable by no caller, so nothing writes through it
        // while the hardware session, which never writes through what it
        // is sent, may hold a reference; the probe keeps deep copies.
        DecodeState::Hw(hw) => match unsafe { hw.send_packet(av_pkt) } {
          // The seam already classified libavcodec's back pressure, so
          // both states travel on unchanged.
          Ok(status) => Ok(status),
          Err(Error::ProbeExhausted(p)) => {
            // **A pinned hardware session reports rather than falls back.**
            // See [`Self::may_open_software`]: this is the exhaustion
            // `DecodePath::Auto` reads as its cue to open software, and
            // the pin's whole content is that it is not that cue here.
            // Reported with the payload intact, so the caller keeps the
            // backend, its error, and any rescued packets.
            if !self.may_open_software() {
              // **And the session is spent**: the exhausted hardware
              // decoder is dropped, so nothing is left installed.
              self.state = DecodeState::Spent(Spent::ProbeExhausted);
              return Err(VideoDecodeError::Decode(Error::ProbeExhausted(p)));
            }
            // The probe exhausted before the first picture: replay the
            // inner decoder's buffered history (lossless — no frame was
            // delivered yet), then forward the still-unconsumed current
            // packet to SW.
            let rescued = p.into_unconsumed_packets();
            // `eof_pending` is the committed EOF state — never pre-mutated here.
            let eof_pending = self.eof_sent;
            self
              .fall_back_to_sw(rescued, eof_pending)
              .map_err(VideoDecodeError::Decode)?;
            // Forward the new (still-unconsumed) current packet to the
            // freshly-opened SW decoder — the HW decoder REFUSED it, so it was not
            // in the replay set. A failure here surfaces (it is not silently
            // dropped), and back pressure from the fresh decoder is reported as
            // such rather than mistaken for one: the fallback committed either
            // way, and the caller re-offers the packet.
            if let DecodeState::Sw(sw) = &mut self.state {
              let st = sw.state();
              match crate::decoder::send(sw, crate::decoder::Sending::Packet(av_pkt)) {
                crate::decoder::Acceptance::Accepted => {}
                crate::decoder::Acceptance::NotAccepted(e)
                | crate::decoder::Acceptance::Ambiguous(e)
                | crate::decoder::Acceptance::AcceptedWithError(e) => {
                  // SAFETY: `st` is `sw`'s own callback state, live while
                  // `sw` is.
                  return unsafe { crate::decoder::software_send(st, e, phase) }
                    .map_err(VideoDecodeError::Decode);
                }
              }
            }
            Ok(Sent::Accepted)
          }
          // **The hardware decoder ended**, terminally: dropped, and the
          // session spent with it.
          Err(e @ Error::AmbiguousSend(_)) => {
            self.state = DecodeState::Spent(Spent::AmbiguousSend);
            Err(VideoDecodeError::Decode(e))
          }
          Err(other) => Err(VideoDecodeError::Decode(other)),
        },
        // Checked before the submission was built.
        DecodeState::Spent(spent) => Err(VideoDecodeError::Decode(Error::Spent(*spent))),
        DecodeState::Sw(sw) => {
          let st = sw.state();
          // **Through the one door** ([`crate::decoder::send`]). The packet
          // is the caller's, and nothing of this wrapper's is at stake, so
          // libavcodec's own contract is the answer, by class: taken, back
          // pressure, or the funnel's verdict — a packet's own error, a
          // refusal for good, or `ENOMEM`, which leaves the caller unable to
          // know whether it was taken (`avcodec.h` 2358).
          match crate::decoder::send(sw, crate::decoder::Sending::Packet(av_pkt)) {
            crate::decoder::Acceptance::Accepted => Ok(Sent::Accepted),
            // Funnel, then gate: a refusal the allocator judge latched
            // comes back named, and back pressure is `MustDrain`.
            //
            // SAFETY: `st` is `sw`'s own callback state, live while `sw`
            // is.
            crate::decoder::Acceptance::NotAccepted(e)
            | crate::decoder::Acceptance::Ambiguous(e)
            | crate::decoder::Acceptance::AcceptedWithError(e) => {
              unsafe { crate::decoder::software_send(st, e, phase) }
                .map_err(VideoDecodeError::Decode)
            }
          }
        }
      }
    };
    // SAFETY: `submit` hands the packet to libavcodec's send (software, or
    // the hardware session's) and to the hardware probe, whose history
    // keeps deep copies (`boundary::try_clone_packet`); it neither clones
    // the packet shallowly nor keeps it, so a shared body never outlives
    // this call.
    unsafe { boundary::with_ffmpeg_video_packet::<C, _>(packet, limits, route, submit) }
      .map_err(|e| VideoDecodeError::Decode(Error::PacketBuild(e)))?
  }

  pub(crate) fn receive_frame_impl(
    &mut self,
    dst: &mut VideoFrame<mediadecode::PixelFormat, VideoFrameExtra, C::Buffer>,
  ) -> Result<Received, VideoDecodeError> {
    // **The state first**, as on the send roads. A spent session holds no
    // replayed frame and parks none: it is spent only at an error.
    if let DecodeState::Spent(spent) = self.state {
      return Err(VideoDecodeError::Decode(Error::Spent(spent)));
    }
    // Deliver any frames produced during SW fallback replay before
    // pulling new ones from the SW decoder. This is the queue
    // populated by `fall_back_to_sw` when SW returned EAGAIN during
    // packet replay.
    // **Peeked, not popped.** A replayed frame is the rescue history's
    // only copy: popping it before the conversion committed lost it to
    // any allocation failure, which is the one thing this queue exists
    // to prevent. It leaves the queue when a carrier exists for it.
    if let Some(replayed) = self.sw_replay_frames.front() {
      // SAFETY: `replayed` is a live AVFrame owned by this queue, which
      // the software decoder filled during replay, writing every byte it
      // exports; convert takes what it needs out of it.
      let converted = unsafe {
        convert::av_frame_to_video_frame_as::<C>(
          replayed.as_ptr(),
          self.time_base,
          self.limits.frame(),
        )
      };
      let new_frame = match converted {
        Ok(new_frame) => new_frame,
        Err(e) if crate::decoder::parks_in_decode(&e) => return Err(VideoDecodeError::Convert(e)),
        // A frame nothing can carry is dropped rather than re-offered
        // forever — the same rule the scratch seat follows.
        Err(e) => {
          self.sw_replay_frames.pop_front();
          return Err(VideoDecodeError::Convert(e));
        }
      };
      self.sw_replay_frames.pop_front();
      self.commit_delivery(new_frame, dst);
      return Ok(Received::Produced);
    }
    // A frame whose conversion did not commit is converted again before
    // the decoder is asked for another — see [`Self::scratch_pending`].
    // The scratch still holds it, and `deliver_frame` reads whichever
    // scratch the current state uses.
    if self.scratch_pending {
      return self.deliver_frame(dst);
    }
    let phase = self.phase();
    loop {
      match &mut self.state {
        DecodeState::Hw(hw) => match hw.receive_frame(&mut self.hw_scratch) {
          Ok(Received::Produced) => {
            // The frame is out of the decoder's queue from here; the
            // seat is what keeps it if the conversion cannot commit.
            self.scratch_pending = true;
            return self.deliver_frame(dst);
          }
          // The hardware seam already classified the two flow signals.
          Ok(status) => return Ok(status),
          Err(Error::ProbeExhausted(p)) => {
            // The same gate, on the receive road — see
            // [`Self::may_open_software`] and the identical gate on the
            // two send roads.
            if !self.may_open_software() {
              // Spent, as on the send road.
              self.state = DecodeState::Spent(Spent::ProbeExhausted);
              return Err(VideoDecodeError::Decode(Error::ProbeExhausted(p)));
            }
            // The probe exhausted at frame time, before the first picture:
            // replay the buffered history (lossless). There is no current
            // packet here.
            let rescued = p.into_unconsumed_packets();
            // `eof_pending` is the committed EOF state — never pre-mutated here.
            let eof_pending = self.eof_sent;
            self
              .fall_back_to_sw(rescued, eof_pending)
              .map_err(VideoDecodeError::Decode)?;
            // If the replay produced any drained frames, return one
            // immediately — preserves stream order vs. whatever the
            // SW decoder will produce next.
            // **Peeked, not popped** — the second delivery path onto
            // this queue, and it owes the same discipline as the first
            // (see the head of `receive_frame_impl`). The replay queue
            // is the rescue history's only copy of these frames, so a
            // conversion that cannot commit must leave the head where
            // it is rather than advance past it.
            if let Some(replayed) = self.sw_replay_frames.front() {
              // SAFETY: `replayed` is a live AVFrame owned by this
              // queue, which the software decoder filled during replay,
              // writing every byte it exports; convert takes what it
              // needs out of it.
              let converted = unsafe {
                convert::av_frame_to_video_frame_as::<C>(
                  replayed.as_ptr(),
                  self.time_base,
                  self.limits.frame(),
                )
              };
              let new_frame = match converted {
                Ok(new_frame) => new_frame,
                Err(e) if crate::decoder::parks_in_decode(&e) => {
                  return Err(VideoDecodeError::Convert(e));
                }
                // A frame nothing can carry is dropped rather than
                // re-offered forever.
                Err(e) => {
                  self.sw_replay_frames.pop_front();
                  return Err(VideoDecodeError::Convert(e));
                }
              };
              self.sw_replay_frames.pop_front();
              self.commit_delivery(new_frame, dst);
              return Ok(Received::Produced);
            }
            // Fall through to the loop; next iteration takes the Sw arm.
          }
          // Ended, as on the send road.
          Err(e @ Error::AmbiguousSend(_)) => {
            self.state = DecodeState::Spent(Spent::AmbiguousSend);
            return Err(VideoDecodeError::Decode(e));
          }
          Err(other) => return Err(VideoDecodeError::Decode(other)),
        },
        // Checked at the top.
        DecodeState::Spent(spent) => return Err(VideoDecodeError::Decode(Error::Spent(*spent))),
        DecodeState::Sw(sw) => {
          // Convert inline (rather than via `deliver_frame`, which borrows all
          // of `self`) so only the disjoint fields `sw_scratch` / `time_base`
          // are touched alongside the `self.state` borrow `sw` holds.
          let st = sw.state();
          match sw.receive_frame(&mut self.sw_scratch) {
            Ok(()) => {
              // The frame is out of the decoder's queue from here; the
              // seat is what keeps it if the conversion cannot commit.
              self.scratch_pending = true;
              // SAFETY: the scratch frame is live, just filled by
              // `receive_frame`, so libavcodec wrote every byte it
              // exports; convert takes what it needs out of it, so the
              // scratch can be reused once this commits.
              let converted = unsafe {
                convert::av_frame_to_video_frame_as::<C>(
                  self.sw_scratch.as_ptr(),
                  self.time_base,
                  self.limits.frame(),
                )
              };
              let new_frame = match converted {
                Ok(new_frame) => new_frame,
                Err(e) => {
                  self.scratch_pending = crate::decoder::parks_in_decode(&e);
                  return Err(VideoDecodeError::Convert(e));
                }
              };
              self.commit_delivery(new_frame, dst);
              return Ok(Received::Produced);
            }
            // Funnel first — so a recorded budget refusal is named
            // rather than laundered — and read as a status second:
            // `EAGAIN` is `NeedsInput`, `Eof` is `Ended`, and the errno
            // stops inside this crate either way.
            Err(e) => {
              // SAFETY: `st` is `sw`'s own callback state, live while
              // `sw` is.
              return unsafe { crate::decoder::software_receive(st, e, phase) }
                .map_err(VideoDecodeError::Decode);
            }
          }
        }
      }
    }
  }

  pub(crate) fn send_eof_impl(&mut self) -> Result<Sent, VideoDecodeError> {
    // **The state first**, as on `send_packet`.
    if let DecodeState::Spent(spent) = self.state {
      return Err(VideoDecodeError::Decode(Error::Spent(spent)));
    }
    // The same two gates in the same order, for the same reason: a
    // repeated end-of-stream past a committed one is refused however
    // much is drained, so answering back pressure would be a promise
    // this face cannot keep. See [`Self::after_eof`].
    if !self.phase().accepts_input() {
      return Err(Self::after_eof());
    }
    // As `send_packet`: EOF can commit a fallback too. Nothing was
    // recorded, so drain and signal again.
    if self.scratch_pending {
      return Ok(Sent::MustDrain);
    }
    let phase = self.phase();
    let outcome = match &mut self.state {
      DecodeState::Hw(hw) => match hw.send_eof() {
        // The seam classified libavcodec's back pressure already.
        Ok(status) => Ok(status),
        Err(Error::ProbeExhausted(p)) => {
          // The same gate, on the EOF road — see
          // [`Self::may_open_software`]. Returned rather than folded into
          // `outcome`: the commit below fires only on `Ok(Sent::Accepted)`,
          // so the two roads agree.
          if !self.may_open_software() {
            // Spent, as on the send road.
            self.state = DecodeState::Spent(Spent::ProbeExhausted);
            return Err(VideoDecodeError::Decode(Error::ProbeExhausted(p)));
          }
          // EOF is pending for this transaction, so the SW decoder must also
          // receive `send_eof` (codecs that delay tail frames hang otherwise).
          // We pass that intent locally rather than pre-setting `self.eof_sent`:
          // `self.eof_sent` is committed only after the whole operation
          // succeeds (the `outcome` check below), keeping the fallback
          // all-or-nothing. A fallback that fails returns `FallbackFailed`
          // and leaves the session spent, so no later fallback can follow.
          //
          // Replay the buffered history (lossless), re-forwarding EOF inside
          // the transaction.
          let rescued = p.into_unconsumed_packets();
          self
            .fall_back_to_sw(rescued, true)
            .map(|()| Sent::Accepted)
            .map_err(VideoDecodeError::Decode)
        }
        // Ended, as on the send road.
        Err(e @ Error::AmbiguousSend(_)) => {
          self.state = DecodeState::Spent(Spent::AmbiguousSend);
          Err(VideoDecodeError::Decode(e))
        }
        Err(other) => Err(VideoDecodeError::Decode(other)),
      },
      // Checked at the top.
      DecodeState::Spent(spent) => Err(VideoDecodeError::Decode(Error::Spent(*spent))),
      DecodeState::Sw(sw) => {
        let st = sw.state();
        match crate::decoder::send(sw, crate::decoder::Sending::End) {
          crate::decoder::Acceptance::Accepted => Ok(Sent::Accepted),
          // SAFETY: `st` is `sw`'s own callback state, live while `sw` is.
          crate::decoder::Acceptance::NotAccepted(e)
          | crate::decoder::Acceptance::Ambiguous(e)
          | crate::decoder::Acceptance::AcceptedWithError(e) => {
            unsafe { crate::decoder::software_send(st, e, phase) }.map_err(VideoDecodeError::Decode)
          }
        }
      }
    };
    // Commit EOF state only when the EOF was actually **taken** — a failed
    // fallback left `self.eof_sent` untouched (restored-by-construction: we
    // never mutated it), and left the session spent.
    //
    // **`is_ok()` is not the test any more, and that is not a stylistic
    // change.** `Ok(Sent::MustDrain)` means the decoder did not take the
    // end-of-stream; recording `eof_sent` there would make a later fallback
    // inject an EOF into the software decoder for a signal that was never
    // accepted — the exact half-mutation the local `eof_pending` argument
    // exists to prevent on the failure road.
    if matches!(outcome, Ok(Sent::Accepted)) {
      self.eof_sent = true;
    }
    outcome
  }

  pub(crate) fn flush_impl(&mut self) -> Result<(), VideoDecodeError> {
    // Drop any frames buffered during SW fallback replay before
    // flushing the inner decoder — otherwise a seek/reset would
    // surface stale pre-flush frames on the next `receive_frame`.
    self.sw_replay_frames.clear();
    // And a parked frame belongs to the position being abandoned.
    self.scratch_pending = false;
    // Flush ends the drain phase; the decoder accepts new packets
    // after this, so reset EOF tracking.
    self.eof_sent = false;
    match &mut self.state {
      // The HW seam's `flush` returns `Result` for a uniform trait; the
      // real `VideoDecoder::flush` is infallible (always `Ok`).
      DecodeState::Hw(hw) => hw.flush().map_err(VideoDecodeError::Decode)?,
      DecodeState::Sw(sw) => sw.flush(),
      // **Nothing to flush, and nothing revived**: a spent session stays
      // spent.
      DecodeState::Spent(_) => {}
    }
    Ok(())
  }
}

macro_rules! video_lane_face {
  ($($lane:ty),+ $(,)?) => { $(
    impl CarrierVideoStreamDecoder<$lane> {
      /// Opens a video decoder for `parameters`, probing hardware
      /// backends in order and falling back to software.
      ///
      /// [`open_as`](Self::open_as)`(.., DecodePath::Auto)`.
      pub fn open(
        parameters: Parameters,
        time_base: Timebase,
        limits: DecoderLimits,
      ) -> Result<Self, Error> {
        Self::open_impl(parameters, time_base, limits)
      }

      /// Opens a video decoder on a **named decode path**.
      ///
      /// [`DecodePath::Auto`] is [`open`](Self::open) exactly;
      /// [`DecodePath::AnyHardware`] keeps the session to the platform's
      /// hardware backends and the other two arms pin it to one backend or
      /// to software, each for its whole life. See [`DecodePath`] for what
      /// each permits and what it costs.
      ///
      /// Everything else about the session is unchanged — the same
      /// [`VideoStreamDecoder`] face, the same frames, the same
      /// [`is_hardware`](Self::is_hardware) / [`is_software`](Self::is_software)
      /// readings. The choice is *which decoder is behind them*, which
      /// is what a determinism comparison and a deployment policy each
      /// need and neither could reach.
      ///
      /// # Errors
      ///
      /// [`DecodePath::AnyHardware`] fails here with
      /// [`Error::ProbeExhausted`] when no hardware backend opens for
      /// the stream — at once on a platform with none — carrying every
      /// backend's attempt and no packet, since none was sent.
      /// [`DecodePath::Hardware`] fails here when the named backend
      /// cannot be opened for the stream. [`DecodePath::Auto`] would have
      /// gone on to software in both cases. [`DecodePath::Software`] fails
      /// only where libavcodec has no decoder for the stream, or the
      /// context cannot be built.
      ///
      /// # Examples
      ///
      /// ```no_run
      /// use mediadecode_ffmpeg::{DecodePath, DecoderLimits, FfmpegVideoStreamDecoder};
      /// # fn f(parameters: ffmpeg_next::codec::Parameters, time_base: mediadecode::Timebase)
      /// # -> Result<(), Box<dyn std::error::Error>> {
      /// // The same stream, decoded without a GPU anywhere in the story.
      /// let decoder = FfmpegVideoStreamDecoder::open_as(
      ///   parameters,
      ///   time_base,
      ///   DecoderLimits::default(),
      ///   DecodePath::Software,
      /// )?;
      /// assert!(decoder.is_software());
      /// # Ok(())
      /// # }
      /// ```
      pub fn open_as(
        parameters: Parameters,
        time_base: Timebase,
        limits: DecoderLimits,
        path: DecodePath,
      ) -> Result<Self, Error> {
        Self::open_as_impl(parameters, time_base, limits, path)
      }

      /// Opens the session `request` names for `parameters`. This is the
      /// one road [`open`](Self::open) and [`open_as`](Self::open_as)
      /// take.
      ///
      /// The request's [`DecodePath`] chooses the decoder, as
      /// [`open_as`](Self::open_as) describes. Its
      /// [`DecoderOptions`](crate::DecoderOptions) (libavcodec's
      /// `skip_frame`, `skip_loop_filter` and `skip_idct`) are written
      /// into every video `AVCodecContext` the session opens: each
      /// hardware candidate the probe builds, the software decoder, and a
      /// fallback's.
      ///
      /// **The options are not the road for a picture cadence.** A packet
      /// the demuxer marks
      /// [`DISPOSABLE`](mediadecode::packet::PacketFlags::DISPOSABLE) is
      /// dropped before it is sent. On a temporally scalable HEVC stream,
      /// [`Discard::NonReference`](crate::Discard::NonReference) also
      /// skips pictures a higher sub-layer references, which the mark
      /// never does. See [`crate::options`].
      ///
      /// # Errors
      ///
      /// As [`open_as`](Self::open_as), on the request's path.
      pub fn open_from(
        request: VideoStreamDecoderOpen,
        parameters: Parameters,
        time_base: Timebase,
        limits: DecoderLimits,
      ) -> Result<Self, Error> {
        Self::open_from_impl(request, parameters, time_base, limits)
      }

      /// How this session decodes: the options it was opened with
      /// ([`VideoStreamDecoderOpen::options`]), libavcodec's defaults for
      /// [`open`](Self::open) and [`open_as`](Self::open_as).
      pub const fn options(&self) -> crate::DecoderOptions {
        self.options
      }

      /// Whether this decoder is currently running on software.
      ///
      /// Neither this nor [`is_hardware`](Self::is_hardware) once the
      /// session is spent — after an `AmbiguousSend`, a `FallbackFailed`, or
      /// a probe's exhaustion on a path that excludes software: no decoder is
      /// left, and every entry answers [`Error::Spent`].
      pub const fn is_software(&self) -> bool {
        self.is_software_impl()
      }

      /// Whether this decoder is currently running on hardware. See
      /// [`is_software`](Self::is_software) for a spent session, which is
      /// neither.
      pub const fn is_hardware(&self) -> bool {
        self.is_hardware_impl()
      }

      /// Whether this session can currently emit pictures at a
      /// caller-requested output size. See
      /// [`ScaledOutputCapability`] and
      /// [`Self::request_scaled_output`].
      ///
      /// `Supported` on a live VideoToolbox session on an Apple
      /// target, `Unsupported` everywhere else — including on a
      /// session that fell back to software during its probe, which is
      /// why this reads the session's live state rather than a fact
      /// recorded once at open.
      pub fn scaled_output_capability(&self) -> ScaledOutputCapability {
        self.scaled_output_capability_impl()
      }

      /// Requests that this session emit pictures at `size` (width,
      /// height) from the next frame on, and reports whether the
      /// request was recorded. See
      /// [`VideoStreamDecoder::request_scaled_output`] for the full
      /// contract (never an error) and
      /// [`Self::scaled_output_capability`]'s documentation for which
      /// road can honor one, what a mid-stream request means, and the
      /// zero / upscale refusals this seat mints itself.
      pub fn request_scaled_output(&mut self, size: (u32, u32)) -> ScaledOutputCapability {
        self.request_scaled_output_impl(size)
      }

      /// The hardware wrapper, when one is in use.
      pub fn hardware_inner(&self) -> Option<&VideoDecoder> {
        self.hardware_inner_impl()
      }

      /// The stream timebase every produced timestamp is stamped with.
      pub const fn time_base(&self) -> Timebase {
        self.time_base_impl()
      }
    }

    impl VideoStreamDecoder for CarrierVideoStreamDecoder<$lane> {
      type Adapter = Ffmpeg;
      type Buffer = <$lane as crate::FfmpegCarrier>::Buffer;
      type Error = VideoDecodeError;

      fn send_packet(
        &mut self,
        packet: &VideoPacket<VideoPacketExtra, Self::Buffer>,
      ) -> Result<Sent, Self::Error> {
        self.send_packet_impl(packet)
      }

      fn receive_frame(
        &mut self,
        dst: &mut VideoFrame<mediadecode::PixelFormat, VideoFrameExtra, Self::Buffer>,
      ) -> Result<Received, Self::Error> {
        self.receive_frame_impl(dst)
      }

      fn send_eof(&mut self) -> Result<Sent, Self::Error> {
        self.send_eof_impl()
      }

      fn flush(&mut self) -> Result<(), Self::Error> {
        self.flush_impl()
      }

      fn scaled_output_capability(&self) -> ScaledOutputCapability {
        self.scaled_output_capability_impl()
      }

      fn request_scaled_output(&mut self, size: (u32, u32)) -> ScaledOutputCapability {
        self.request_scaled_output_impl(size)
      }
    }
  )+ };
}

video_lane_face!(crate::View, crate::Owned);

fn open_sw_decoder(
  parameters: &Parameters,
  limits: DecoderLimits,
  options: crate::DecoderOptions,
  pkt_timebase: Option<Timebase>,
) -> Result<SwDecoder, Error> {
  // Use the checked codec-context builder — ffmpeg-next's
  // `Context::from_parameters` calls `Context::new()` which doesn't
  // null-check `avcodec_alloc_context3`'s return value before
  // running `avcodec_parameters_to_context` against it. Under
  // memory pressure that's C-level UB; `build_codec_context`
  // surfaces the OOM as an error instead.
  let (ctx, callback_state) = build_codec_context(parameters, limits, options, pkt_timebase)?;
  // Opened without forming a bindgen enum from FFmpeg memory: the codec
  // is resolved off a raw `codec_id`, and the medium is proved off a raw
  // `codec_type`. See `crate::decoder::ensure_codec_type`.
  let codec = crate::decoder::find_decoder(parameters)?;
  let opened = ctx.decoder().open_as(codec).map_err(Error::Ffmpeg)?;
  crate::decoder::ensure_video_codec_type(&opened)?;
  Ok(SwDecoder {
    decoder: ffmpeg_next::decoder::Video(opened),
    _callback_state: callback_state,
  })
}

/// Error type for [`FfmpegVideoStreamDecoder`] — **faults and the
/// send-side refusal**.
///
/// Every arm here is something that went wrong or something the push
/// face declined. The drain's *needs input* and *ended* are
/// [`Received`] states out of `receive_frame`; they used to arrive as
/// `Decode(Ffmpeg(Other { errno: EAGAIN }))` and `Decode(Ffmpeg(Eof))`,
/// which is to say they had no name at this tier at all.
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
#[derive(thiserror::Error, Debug, IsVariant, Unwrap, TryUnwrap)]
#[unwrap(ref, ref_mut)]
#[try_unwrap(ref, ref_mut)]
#[non_exhaustive]
pub enum VideoDecodeError {
  /// The wrapped decoder (HW or SW) reported an error.
  #[error(transparent)]
  Decode(#[from] Error),
  /// Frame conversion from FFmpeg's native types to mediadecode's
  /// types failed.
  #[error(transparent)]
  Convert(#[from] ConvertError),
}

#[cfg(test)]
mod tests;
