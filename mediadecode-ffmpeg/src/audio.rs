//! `mediadecode::AudioStreamDecoder` impl backed by
//! `ffmpeg::decoder::Audio`.
//!
//! Mirrors the shape of [`crate::FfmpegVideoStreamDecoder`] without
//! the HW-fallback wrinkle — audio decoders never go through a
//! hardware backend in the FFmpeg world, so there's no probe, no
//! state machine, just `send_packet` / `receive_frame` over the
//! software decoder.
//!
//! Frames produced via [`crate::convert::av_frame_to_audio_frame`]
//! carry `FfmpegBytes` planes copied out of the source `AVFrame` — the
//! [carrier amputation contract][law]. The consumer can hold the frame
//! across decoder calls, send it to another thread, and outlive the
//! decoder that made it.
//!
//! [law]: mediadecode::adapter#the-carrier-amputation-contract

use derive_more::{IsVariant, TryUnwrap, Unwrap};
use ffmpeg_next::{codec::Parameters, frame};
use mediadecode::{
  Received, Sent, Timebase, decoder::AudioStreamDecoder, frame::AudioFrame, packet::AudioPacket,
};
use mediaframe::audio::ChannelLayoutDescription;

use crate::{
  DecoderLimits, Error, Ffmpeg, SampleFormat, boundary,
  convert::{self, ConvertError},
  decoder::build_codec_context,
  extras::{AudioFrameExtra, AudioPacketExtra},
  frame::alloc_av_audio_frame,
};

/// `mediadecode::AudioStreamDecoder` impl wrapping `ffmpeg::decoder::Audio`.
pub struct CarrierAudioStreamDecoder<C: crate::FfmpegCarrier> {
  decoder: ffmpeg_next::decoder::Audio,
  scratch: frame::Audio,
  time_base: Timebase,
  limits: DecoderLimits,
  /// Owns the [`CallbackState`](crate::ffi::CallbackState) the codec
  /// context points at, through that same pointer — see
  /// [`InstalledCallbackState`](crate::decoder::InstalledCallbackState).
  ///
  /// Declared **after** the decoder on purpose: struct fields drop in
  /// declaration order, so the `AVCodecContext` is freed first and the
  /// state it references outlives it.
  _callback_state: crate::decoder::InstalledCallbackState,
  /// The lane this decoder captures into. A marker: the carrier
  /// appears in the frames it produces, not in its own state.
  /// `true` when [`Self::scratch`] holds a decoded frame whose
  /// conversion has **not committed**.
  ///
  /// `receive_frame` advances libavcodec: the frame it fills the
  /// scratch with is out of the codec's queue and nothing re-offers it.
  /// A conversion that then failed on an *allocation* used to leave
  /// that frame in a scratch the next call overwrites — a decoded frame
  /// lost to memory pressure, silently. So the receive and the
  /// conversion are one transaction: while this is set, the next call
  /// converts the scratch it already has instead of asking libavcodec
  /// for another.
  ///
  /// The same seat the demux session keeps for a packet, and the same
  /// discipline `flush` clears.
  scratch_pending: bool,
  /// `true` once [`Self::send_eof_impl`] has been accepted, until
  /// [`Self::flush_impl`].
  ///
  /// **This road kept no end of its own before, and that was the gap.**
  /// It leans on libavcodec to refuse a post-EOF submission — which
  /// libavcodec does — but leaning is not knowing: asked "where is this
  /// session", it could only answer `Streaming`, which is false the
  /// moment a caller signals the end. A phase that can be wrong is the
  /// thing [`SessionPhase`](crate::decoder::SessionPhase) exists to
  /// abolish, so the latch is here to make the answer true rather than
  /// to add a gate. The substrate is still the one that refuses.
  eof: bool,
  _carrier: core::marker::PhantomData<C>,
}

impl<C: crate::FfmpegCarrier + crate::CarrierOps> CarrierAudioStreamDecoder<C> {
  /// Opens an audio decoder for the given codec parameters.
  ///
  /// `limits` bounds what one decoded frame may cost and is taken here
  /// rather than through a builder: half of it is written straight into
  /// the `AVCodecContext` this call opens, and a context's ceiling
  /// cannot be moved after `avcodec_open2`. See [`DecoderLimits`].
  pub(crate) fn open_impl(
    parameters: Parameters,
    time_base: Timebase,
    limits: DecoderLimits,
  ) -> Result<Self, AudioDecodeError> {
    // Use the checked codec-context builder — `Context::from_parameters`
    // is OOM-UB-prone (see `crate::decoder::build_codec_context`).
    let (ctx, callback_state) = build_codec_context(
      &parameters,
      limits,
      crate::DecoderOptions::new(),
      Some(time_base),
    )
    .map_err(AudioDecodeError::Decode)?;
    // Opened without forming a bindgen enum from FFmpeg memory: the codec
    // is resolved off a raw `codec_id`, and the medium is proved off a raw
    // `codec_type`. See `crate::decoder::ensure_codec_type`.
    let codec = crate::decoder::find_decoder(&parameters).map_err(AudioDecodeError::Decode)?;
    let opened = ctx
      .decoder()
      .open_as(codec)
      .map_err(|e| AudioDecodeError::Decode(Error::Ffmpeg(e)))?;
    crate::decoder::ensure_codec_type(&opened, ffmpeg_next::ffi::AVMediaType::AVMEDIA_TYPE_AUDIO)
      .map_err(AudioDecodeError::Decode)?;
    let decoder = ffmpeg_next::decoder::Audio(opened);
    let scratch = alloc_av_audio_frame().map_err(|e| AudioDecodeError::Decode(e.into()))?;
    Ok(Self {
      decoder,
      scratch,
      time_base,
      limits,
      _callback_state: callback_state,
      scratch_pending: false,
      eof: false,
      _carrier: core::marker::PhantomData,
    })
  }

  /// Returns the time base associated with the source stream.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub(crate) const fn time_base_impl(&self) -> Timebase {
    self.time_base
  }

  /// The frame ceilings this decoder was opened with.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub(crate) const fn limits_impl(&self) -> DecoderLimits {
    self.limits
  }

  /// Borrow the wrapped `ffmpeg::decoder::Audio` (e.g. to query
  /// `channels()` / `rate()` / `format()`).
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub(crate) const fn inner_impl(&self) -> &ffmpeg_next::decoder::Audio {
    &self.decoder
  }
}

impl<C: crate::FfmpegCarrier + crate::CarrierOps> CarrierAudioStreamDecoder<C> {
  /// Where this session is. See
  /// [`SessionPhase`](crate::decoder::SessionPhase) — one derivation,
  /// no road reading the latch for itself. There is no probe on this
  /// road, so only the committed pair is reachable.
  const fn phase(&self) -> crate::decoder::SessionPhase {
    if self.eof {
      crate::decoder::SessionPhase::Draining
    } else {
      crate::decoder::SessionPhase::Streaming
    }
  }

  /// **This road never refuses for a parked frame, and that asymmetry
  /// is kept rather than papered over.** The video and subtitle
  /// decoders answer [`Sent::MustDrain`] while their scratch holds an
  /// undelivered frame; this one has a single scratch and cannot change
  /// which is current, so a submission under a park loses nothing and
  /// is simply taken. See
  /// [`CarrierVideoStreamDecoder::scratch_pending`](crate::video::CarrierVideoStreamDecoder)
  /// for the property that makes the refusal necessary over there.
  ///
  /// What it *does* answer `MustDrain` to is libavcodec's own back
  /// pressure: `avcodec_send_packet` returning `EAGAIN` because its
  /// output queue is full. That used to arrive as
  /// `Decode(Ffmpeg(Other { errno: EAGAIN }))` — a fault-shaped value a
  /// generic consumer could not recognise, which is what made the
  /// two-offer rule necessary in the first place.
  pub(crate) fn send_packet_impl(
    &mut self,
    packet: &AudioPacket<AudioPacketExtra, C::Buffer>,
  ) -> Result<Sent, AudioDecodeError> {
    // Scoped submission: the rebuilt `AVPacket` lives only inside this
    // call, which is what lets the view lane hand libavcodec its own
    // buffer instead of a copy. See `boundary::with_ffmpeg_audio_packet`.
    let state: *const crate::ffi::CallbackState = self._callback_state.as_ptr();
    let phase = self.phase();
    let decoder = &mut self.decoder;
    // Funnel, then gate — the same two steps the receive road takes. A
    // frame the allocator judge refused surfaces named, not as the
    // `EINVAL` a corrupt file also produces; whatever survives the
    // funnel is read for back pressure.
    // Through the one door ([`crate::decoder::send`]): the packet is the
    // caller's, and this decoder keeps none, so libavcodec's own contract
    // is the answer, by class.
    let submit = |av_pkt: &ffmpeg_next::Packet| match crate::decoder::send(
      decoder,
      crate::decoder::Sending::Packet(av_pkt),
    ) {
      crate::decoder::Acceptance::Accepted => Ok(Sent::Accepted),
      // SAFETY: `state` is the pointer the codec context holds, owned by
      // `self._callback_state` until after the context is freed.
      crate::decoder::Acceptance::NotAccepted(e)
      | crate::decoder::Acceptance::Ambiguous(e)
      | crate::decoder::Acceptance::AcceptedWithError(e) => {
        unsafe { crate::decoder::software_send(state, e, phase) }.map_err(AudioDecodeError::Decode)
      }
    };
    // SAFETY: `submit` hands the packet to `avcodec_send_packet` (through
    // `send_packet`) and returns; it neither clones nor keeps it, so a
    // shared body never outlives this call.
    unsafe {
      boundary::with_ffmpeg_audio_packet::<C, _>(
        packet,
        self.limits.packet_limits(),
        // Nothing on this road records what it is sent, so the
        // packet really does die inside the call and its body may
        // be shared.
        crate::BodyRoute::Submission,
        submit,
      )
    }
    .map_err(|e| AudioDecodeError::Decode(Error::PacketBuild(e)))?
  }

  pub(crate) fn receive_frame_impl(
    &mut self,
    dst: &mut AudioFrame<SampleFormat, ChannelLayoutDescription, AudioFrameExtra, C::Buffer>,
  ) -> Result<Received, AudioDecodeError> {
    let state: *const crate::ffi::CallbackState = self._callback_state.as_ptr();
    let phase = self.phase();
    // A frame whose conversion did not commit is converted again before
    // another is asked for — see [`Self::scratch_pending`].
    if !self.scratch_pending {
      // Funnel first, classify second: the funnel is what turns a
      // recorded budget refusal into its own name, and only what
      // survives it is read as a drain state. `EAGAIN` and `Eof` are
      // the protocol talking, so they leave through the `Ok` arm and
      // the errno never reaches the caller.
      if let Err(e) = self.decoder.receive_frame(&mut self.scratch) {
        // SAFETY: `state` is the pointer the codec context holds, owned by
        // `self._callback_state` until after the context is freed.
        return unsafe { crate::decoder::software_receive(state, e, phase) }
          .map_err(AudioDecodeError::Decode);
      }
    }
    // SAFETY: the scratch holds a frame — either one `receive_frame`
    // just filled it with, or one it was left holding by a conversion
    // that did not commit. Convert takes what it needs out of it, so
    // the scratch can be reused once this has committed. Every byte it
    // exports was written by libavcodec: `avcodec_receive_frame` decodes
    // into its own zero-allocated pools. Its channel layout is
    // libavcodec's too: `avcodec_receive_frame` fills it with
    // `av_channel_layout_copy`, so a custom map holds exactly
    // `nb_channels` entries, which is the worker's map clause.
    let converted = unsafe {
      convert::av_frame_to_audio_frame_as::<C>(
        self.scratch.as_ptr(),
        self.time_base,
        self.limits.frame(),
      )
    };
    match converted {
      Ok(new_frame) => {
        self.scratch_pending = false;
        *dst = new_frame;
        Ok(Received::Produced)
      }
      Err(e) => {
        // Park only what another attempt could survive; a frame nothing
        // can carry is let go, or every later receive answers with the
        // same error.
        self.scratch_pending = crate::decoder::parks_in_decode(&e);
        Err(AudioDecodeError::Convert(e))
      }
    }
  }

  pub(crate) fn send_eof_impl(&mut self) -> Result<Sent, AudioDecodeError> {
    let state: *const crate::ffi::CallbackState = self._callback_state.as_ptr();
    let phase = self.phase();
    match crate::decoder::send(&mut self.decoder, crate::decoder::Sending::End) {
      crate::decoder::Acceptance::Accepted => {
        self.eof = true;
        Ok(Sent::Accepted)
      }
      // SAFETY: `state` is the pointer the codec context holds, owned by
      // `self._callback_state` until after the context is freed.
      crate::decoder::Acceptance::NotAccepted(e)
      | crate::decoder::Acceptance::Ambiguous(e)
      | crate::decoder::Acceptance::AcceptedWithError(e) => {
        unsafe { crate::decoder::software_send(state, e, phase) }.map_err(AudioDecodeError::Decode)
      }
    }
  }

  pub(crate) fn flush_impl(&mut self) -> Result<(), AudioDecodeError> {
    // A parked frame belongs to the stream position being abandoned.
    self.scratch_pending = false;
    // And so does the end it was told about.
    self.eof = false;
    self.decoder.flush();
    Ok(())
  }
}

macro_rules! audio_lane_face {
  ($($lane:ty),+ $(,)?) => { $(
    impl CarrierAudioStreamDecoder<$lane> {
      /// Opens an audio decoder for `parameters`.
      pub fn open(
        parameters: Parameters,
        time_base: Timebase,
        limits: DecoderLimits,
      ) -> Result<Self, AudioDecodeError> {
        Self::open_impl(parameters, time_base, limits)
      }

      /// The stream timebase every produced timestamp is stamped with.
      pub const fn time_base(&self) -> Timebase {
        self.time_base_impl()
      }

      /// The budgets this decoder was opened with.
      pub const fn limits(&self) -> DecoderLimits {
        self.limits_impl()
      }

      /// The wrapped decoder context.
      pub const fn inner(&self) -> &ffmpeg_next::decoder::Audio {
        self.inner_impl()
      }
    }

    impl AudioStreamDecoder for CarrierAudioStreamDecoder<$lane> {
      type Adapter = Ffmpeg;
      type Buffer = <$lane as crate::FfmpegCarrier>::Buffer;
      type Error = AudioDecodeError;

      fn send_packet(
        &mut self,
        packet: &AudioPacket<AudioPacketExtra, Self::Buffer>,
      ) -> Result<Sent, Self::Error> {
        self.send_packet_impl(packet)
      }

      fn receive_frame(
        &mut self,
        dst: &mut AudioFrame<
          SampleFormat,
          ChannelLayoutDescription,
          AudioFrameExtra,
          Self::Buffer,
        >,
      ) -> Result<Received, Self::Error> {
        self.receive_frame_impl(dst)
      }

      fn send_eof(&mut self) -> Result<Sent, Self::Error> {
        self.send_eof_impl()
      }

      fn flush(&mut self) -> Result<(), Self::Error> {
        self.flush_impl()
      }
    }
  )+ };
}

audio_lane_face!(crate::View, crate::Owned);

/// Errors from [`FfmpegAudioStreamDecoder`] — **faults only**.
///
/// Both arms are real failures. The drain's other two answers never
/// reached this type even before they had names: they rode in as
/// `Decode(Ffmpeg(Other { errno: EAGAIN }))` and
/// `Decode(Ffmpeg(Eof))`, which a generic consumer had no way to
/// recognise. They are [`Received`] states now.
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
pub enum AudioDecodeError {
  /// The wrapped `ffmpeg::decoder::Audio` reported an error.
  #[error(transparent)]
  Decode(#[from] Error),
  /// Conversion from FFmpeg's `AVFrame` to mediadecode's `AudioFrame`
  /// failed.
  #[error(transparent)]
  Convert(#[from] ConvertError),
}
