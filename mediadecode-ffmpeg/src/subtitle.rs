//! `mediadecode::SubtitleDecoder` impl backed by
//! `ffmpeg::decoder::Subtitle`.
//!
//! Subtitles use FFmpeg's legacy synchronous `decode()` API rather
//! than `send_packet`/`receive_frame`. We bridge the difference by
//! converting the produced `AVSubtitle` into a
//! [`mediadecode::SubtitleFrame`] inside [`SubtitleDecoder::send_packet`]
//! and stashing it in `pending` for the next [`SubtitleDecoder::receive_frame`]
//! call. This matches the trait's contract: `send_packet` enqueues
//! work — answering [`Sent::MustDrain`] while the seat is still full,
//! since the inline API cannot queue a second cue — and `receive_frame`
//! drains one decoded frame at a time, answering
//! [`Received::NeedsInput`] when the seat is empty and
//! [`Received::Ended`] once [`SubtitleDecoder::send_eof`] has been
//! signalled.
//!
//! **A decoder with no tail still has an end.** `avcodec_decode_subtitle2`
//! produces its cue inline, so nothing is buffered and `send_eof` has
//! nothing to flush — but a session the caller has declared over is a
//! different state from one still waiting for packets, and only one of
//! them lets a drain loop stop. The latch that tells them apart is the
//! whole of this backend's end-of-stream machinery.

use derive_more::{IsVariant, TryUnwrap, Unwrap};
use ffmpeg_next::{codec::Parameters, ffi::avsubtitle_free};
use mediadecode::{
  Received, Sent, Timebase, decoder::SubtitleDecoder, frame::SubtitleFrame, packet::SubtitlePacket,
};

use crate::{
  DecoderLimits, Error, Ffmpeg, boundary,
  convert::{self, ConvertError},
  decoder::build_codec_context,
  extras::{SubtitleFrameExtra, SubtitlePacketExtra},
};

/// RAII wrapper that owns an `ffmpeg_next::Subtitle` scratch slot and
/// frees the FFmpeg-side rect allocations on drop / explicit `clear`.
///
/// `ffmpeg::Subtitle::new()` zero-initializes; `decoder.decode()` may
/// allocate per-rect storage (`AVSubtitleRect.text` / `.ass` /
/// `.data[0]` / `.data[1]`) which only `avsubtitle_free` releases.
/// Without this wrapper, every successful decode leaks until the
/// decoder drops.
struct ScratchSubtitle {
  inner: ffmpeg_next::Subtitle,
}

impl ScratchSubtitle {
  fn new() -> Self {
    Self {
      inner: ffmpeg_next::Subtitle::new(),
    }
  }

  fn clear(&mut self) {
    // SAFETY: `inner` holds a valid AVSubtitle (zero-initialized or
    // populated by `decode`). `avsubtitle_free` frees the rect array
    // and per-rect allocations, then leaves the struct in a state
    // suitable for reuse by the next decode call.
    unsafe { avsubtitle_free(self.inner.as_mut_ptr()) };
  }
}

impl Drop for ScratchSubtitle {
  fn drop(&mut self) {
    self.clear();
  }
}

/// `mediadecode::SubtitleDecoder` impl wrapping `ffmpeg::decoder::Subtitle`.
///
/// Subtitle decoders are stateless from FFmpeg's perspective — each
/// `decode()` call consumes one packet and produces zero-or-one
/// `AVSubtitle`. The pending-frame buffer here is a one-slot queue
/// so the trait's `send_packet` / `receive_frame` split works.
pub struct CarrierSubtitleStreamDecoder<C: crate::FfmpegCarrier> {
  decoder: ffmpeg_next::decoder::Subtitle,
  scratch: ScratchSubtitle,
  /// `true` when [`Self::scratch`] holds a decoded `AVSubtitle` that
  /// has not been converted and delivered.
  ///
  /// **The conversion happens on the receive side, and that is the
  /// point.** `avcodec_decode_subtitle2` consumes the packet: once it
  /// has answered, the cue exists only in this scratch, and nothing
  /// re-offers it. Converting inside `send_packet` meant an allocation
  /// that failed took the cue with it — the scratch was freed, the
  /// error returned, and the caller's next packet decoded the *next*
  /// cue. Deferring the conversion to `receive_frame` gives it a seat
  /// to fail into: the scratch is cleared when a carrier exists for it,
  /// not before.
  ///
  /// The same shape the audio and video decoders keep for a decoded
  /// `AVFrame`, and the same discipline `flush` clears.
  scratch_pending: bool,
  /// `true` once [`Self::send_eof_impl`] has been called and no
  /// [`Self::flush_impl`] has reset the session.
  ///
  /// The legacy `decode()` API buffers nothing, so this latch is not a
  /// drain cursor — it is the only thing that distinguishes "no cue
  /// yet, send another packet" from "there will be no more cues". Both
  /// used to answer with the same error arm, which meant a caller
  /// draining to the end of a subtitle track had no terminating
  /// condition to look for at all.
  eof: bool,
  time_base: Timebase,
  /// Retained, not discarded at open: the send path judges
  /// [`DecoderLimits::max_packet_bytes`] against every packet it
  /// rebuilds into an `AVPacket`.
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
  _carrier: core::marker::PhantomData<C>,
}

impl<C: crate::FfmpegCarrier + crate::CarrierOps> CarrierSubtitleStreamDecoder<C> {
  /// Opens a subtitle decoder for the given codec parameters.
  ///
  /// `limits` reaches the `AVCodecContext` this call opens. A subtitle
  /// decoder produces no pixels, so the pixel half never fires here —
  /// it is passed for one reason: every decoder this crate opens gets
  /// the same ceiling written into it, and a seam that skipped one
  /// would be a seam somebody has to remember.
  pub(crate) fn open_impl(
    parameters: Parameters,
    time_base: Timebase,
    limits: DecoderLimits,
  ) -> Result<Self, SubtitleDecodeError> {
    // Use the checked codec-context builder — `Context::from_parameters`
    // is OOM-UB-prone (see `crate::decoder::build_codec_context`).
    let (ctx, callback_state) = build_codec_context(
      &parameters,
      limits,
      crate::DecoderOptions::new(),
      Some(time_base),
    )
    .map_err(SubtitleDecodeError::Decode)?;
    // Opened without forming a bindgen enum from FFmpeg memory: the codec
    // is resolved off a raw `codec_id`, and the medium is proved off a raw
    // `codec_type`. See `crate::decoder::ensure_codec_type`.
    let codec = crate::decoder::find_decoder(&parameters).map_err(SubtitleDecodeError::Decode)?;
    let opened = ctx
      .decoder()
      .open_as(codec)
      .map_err(|e| SubtitleDecodeError::Decode(Error::Ffmpeg(e)))?;
    crate::decoder::ensure_codec_type(
      &opened,
      ffmpeg_next::ffi::AVMediaType::AVMEDIA_TYPE_SUBTITLE,
    )
    .map_err(SubtitleDecodeError::Decode)?;
    let decoder = ffmpeg_next::decoder::Subtitle(opened);
    Ok(Self {
      decoder,
      scratch: ScratchSubtitle::new(),
      scratch_pending: false,
      eof: false,
      time_base,
      limits,
      _callback_state: callback_state,
      _carrier: core::marker::PhantomData,
    })
  }

  /// Returns the time base associated with the source stream.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub(crate) const fn time_base_impl(&self) -> Timebase {
    self.time_base
  }

  /// The ceilings this decoder was opened with.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub(crate) const fn limits_impl(&self) -> DecoderLimits {
    self.limits
  }

  /// Borrow the wrapped `ffmpeg::decoder::Subtitle`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub(crate) const fn inner_impl(&self) -> &ffmpeg_next::decoder::Subtitle {
    &self.decoder
  }
}

impl<C: crate::FfmpegCarrier + crate::CarrierOps> CarrierSubtitleStreamDecoder<C> {
  pub(crate) fn send_packet_impl(
    &mut self,
    packet: &SubtitlePacket<SubtitlePacketExtra, C::Buffer>,
  ) -> Result<Sent, SubtitleDecodeError> {
    // **Nothing is sent after the stream has been declared over, and
    // this gate is FIRST for a reason.**
    //
    // Every other decoder in the family gets this refusal from its
    // substrate: `avcodec_send_packet` answers `AVERROR_EOF` to a packet
    // that follows a flush packet, and the WebCodecs decoder tracks its
    // own resolved flush. `avcodec_decode_subtitle2` has no send/receive
    // state machine at all — it is a synchronous call that decodes
    // whatever it is handed — so this session's `eof` latch is the only
    // thing that knows, and a latch that gates only the receive side is
    // not a latch. Without this, a valid packet after `send_eof` decoded
    // normally, set the seat, and made the *next* `receive_frame` answer
    // `Frame`: a terminal `Ended` reversed with no `flush` anywhere.
    //
    // **Before the held-cue check, not after.** A cue in the seat would
    // otherwise turn a usage fault into `Sent::MustDrain` — an
    // instruction to drain and re-offer, which is precisely the one
    // thing that must not happen: the drained retry would then be
    // accepted and the reversal would happen one call later.
    if self.eof {
      return Err(SubtitleDecodeError::AfterEof);
    }
    // **Nothing is sent while a cue is held.** The legacy `decode()`
    // API produces a frame inline, so a second send would silently drop
    // the first. The discipline is unchanged; only its spelling moved.
    // It was `FramePending`, a fault-shaped value that made a caller
    // choose between giving up and guessing — and the guess that
    // survived was to offer the packet twice. It is back pressure, and
    // it says so: nothing was consumed, drain and offer again.
    if self.scratch_pending {
      return Ok(Sent::MustDrain);
    }
    // Free any allocations from a previous decode before reusing the
    // scratch — avoids leaking when the previous packet produced no
    // frame (got == false, which still mutates the struct).
    self.scratch.clear();
    // Scoped submission — see `boundary::with_ffmpeg_subtitle_packet`.
    let state: *const crate::ffi::CallbackState = self._callback_state.as_ptr();
    let decoder = &mut self.decoder;
    let scratch = &mut self.scratch.inner;
    let submit = |av_pkt: &ffmpeg_next::Packet| {
      decoder.decode(av_pkt, scratch).map_err(|e| {
        // SAFETY: `state` is the pointer the codec context holds, owned by
        // `self._callback_state` until after the context is freed.
        SubtitleDecodeError::Decode(unsafe { crate::decoder::software_exit(state, e) })
      })
    };
    // SAFETY: `submit` hands the packet to `avcodec_decode_subtitle2`
    // (through `decode`), which reads it and does not write through it;
    // the closure neither clones nor keeps it, so a shared body never
    // outlives this call.
    let got = unsafe {
      boundary::with_ffmpeg_subtitle_packet::<C, _>(
        packet,
        self.limits.packet_limits(),
        // Nothing on this road records what it is sent, so the packet
        // really does die inside the call and its body may be shared.
        crate::BodyRoute::Submission,
        submit,
      )
    }
    .map_err(|e| SubtitleDecodeError::Decode(Error::PacketBuild(e)))??;
    // The cue stays in the scratch until a carrier exists for it — see
    // [`Self::scratch_pending`]. Nothing is converted here.
    self.scratch_pending = got;
    Ok(Sent::Accepted)
  }

  pub(crate) fn receive_frame_impl(
    &mut self,
    dst: &mut SubtitleFrame<SubtitleFrameExtra, C::Buffer>,
  ) -> Result<Received, SubtitleDecodeError> {
    if !self.scratch_pending {
      // A held cue is delivered even after EOF — the latch ends the
      // session, it does not discard what the session already made.
      return Ok(if self.eof {
        Received::Ended
      } else {
        Received::NeedsInput
      });
    }
    // SAFETY: `scratch.inner` is a live `AVSubtitle` filled by the
    // decode whose result the scratch holds — `avcodec_decode_subtitle2` wrote its
    // rects, their strings and their planes, each plane for its own
    // `linesize[0] × h`. Conversion copies every rect it takes —
    // `AVSubtitleRect` has no refcounted buffer, so both lanes copy — and
    // the FFmpeg-side allocations are released below, once there is
    // something to release them in favour of.
    let converted =
      unsafe { convert::av_subtitle_to_subtitle_frame_as::<C>(self.scratch.inner.as_ptr()) };
    match converted {
      Ok(frame) => {
        self.scratch.clear();
        self.scratch_pending = false;
        *dst = frame;
        Ok(Received::Produced)
      }
      Err(e) if crate::decoder::parks_in_decode(&e) => {
        // Kept: another attempt could carry this cue, and there is no
        // other copy of it anywhere.
        Err(SubtitleDecodeError::Convert(e))
      }
      Err(e) => {
        // A cue nothing can carry is let go — freed immediately, so a
        // caller that ignores the error cannot leave the scratch
        // holding FFmpeg allocations, and re-offering it forever would
        // stall the session.
        self.scratch.clear();
        self.scratch_pending = false;
        Err(SubtitleDecodeError::Convert(e))
      }
    }
  }

  pub(crate) fn send_eof_impl(&mut self) -> Result<Sent, SubtitleDecodeError> {
    // Subtitle decoders have no tail to drain — the legacy decode() API
    // produces a cue inline with each packet — so nothing is forwarded
    // to libavcodec here. What EOF does mean is that no further packet
    // is coming, which is what `receive_frame` needs in order to answer
    // `Ended` instead of asking for input that will never arrive.
    //
    // **Always `Accepted`, including under a held cue and including a
    // second time.** The family's line is *sending data after the end
    // is a fault; re-declaring the end is not* — a packet after EOF is
    // input the caller believes will be decoded and will not be, while
    // a second `send_eof` restates a fact that is already true and
    // costs nothing. The held cue is still delivered by the next
    // `receive_frame`, and only then does the seat answer `Ended`;
    // refusing here would be back pressure with nothing behind it.
    //
    // The `swresample` seam one tier along is idempotent for the same
    // reason. The raw FFmpeg decoders are the documented exception:
    // libavcodec refuses a second flush packet with `AVERROR_EOF`, and
    // that is the substrate's word, reported rather than papered over.
    self.eof = true;
    Ok(Sent::Accepted)
  }

  pub(crate) fn flush_impl(&mut self) -> Result<(), SubtitleDecodeError> {
    self.decoder.flush();
    // A held cue belongs to the position being abandoned.
    self.scratch_pending = false;
    self.scratch.clear();
    // And the session is open again: flush is how a caller reuses this
    // decoder for another stream, so the end it declared is retracted
    // with the rest of the position.
    self.eof = false;
    Ok(())
  }
}

macro_rules! subtitle_lane_face {
  ($($lane:ty),+ $(,)?) => { $(
    impl CarrierSubtitleStreamDecoder<$lane> {
      /// Opens a subtitle decoder for `parameters`.
      pub fn open(
        parameters: Parameters,
        time_base: Timebase,
        limits: DecoderLimits,
      ) -> Result<Self, SubtitleDecodeError> {
        Self::open_impl(parameters, time_base, limits)
      }

      /// The time base associated with the source stream.
      pub const fn time_base(&self) -> Timebase {
        self.time_base_impl()
      }

      /// The budgets this decoder was opened with.
      pub const fn limits(&self) -> DecoderLimits {
        self.limits_impl()
      }

      /// The wrapped decoder context.
      pub const fn inner(&self) -> &ffmpeg_next::decoder::Subtitle {
        self.inner_impl()
      }
    }

    impl SubtitleDecoder for CarrierSubtitleStreamDecoder<$lane> {
      type Adapter = Ffmpeg;
      type Buffer = <$lane as crate::FfmpegCarrier>::Buffer;
      type Error = SubtitleDecodeError;

      fn send_packet(
        &mut self,
        packet: &SubtitlePacket<SubtitlePacketExtra, Self::Buffer>,
      ) -> Result<Sent, Self::Error> {
        self.send_packet_impl(packet)
      }

      fn receive_frame(
        &mut self,
        dst: &mut SubtitleFrame<SubtitleFrameExtra, Self::Buffer>,
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

subtitle_lane_face!(crate::View, crate::Owned);

/// Errors from [`FfmpegSubtitleStreamDecoder`] — **faults and the
/// send-side refusal**.
///
/// `NoFrameReady` used to be here, and it was the crate's worst
/// conflation: `send_eof` on this backend is a no-op, so "no cue yet"
/// and "there will be no more cues" were the same value, and a caller
/// draining to the end of a subtitle track had nothing to stop on. Both
/// are [`Received`] states now — [`Received::NeedsInput`] and
/// [`Received::Ended`] — told apart by the session's own EOF latch.
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
pub enum SubtitleDecodeError {
  /// The wrapped `ffmpeg::decoder::Subtitle` reported an error.
  #[error(transparent)]
  Decode(#[from] Error),
  /// Conversion from FFmpeg's `AVSubtitle` to mediadecode's
  /// `SubtitleFrame` failed.
  #[error(transparent)]
  Convert(#[from] ConvertError),

  /// [`send_packet`](SubtitleDecoder::send_packet) was called after
  /// [`send_eof`](SubtitleDecoder::send_eof). Call
  /// [`flush`](SubtitleDecoder::flush) first to reuse the decoder for
  /// another stream.
  ///
  /// **A caller usage fault, not back pressure**, which is the line
  /// that keeps it here while the held-cue refusal became
  /// [`Sent::MustDrain`]. Draining changes nothing about it: this
  /// session will refuse every packet until `flush`, so answering
  /// `MustDrain` would send the caller into a loop with no exit — and,
  /// worse, the loop's next offer would be *accepted*, reversing a
  /// terminal [`Received::Ended`].
  ///
  /// Named `AfterEof` rather than `AtEof` because that is what this
  /// crate already calls the condition one seam over
  /// ([`ResampleError::AfterEof`](crate::ResampleError::AfterEof)), and
  /// one condition deserves one word.
  #[error("send_packet after send_eof; flush() first to start another stream")]
  AfterEof,
}
