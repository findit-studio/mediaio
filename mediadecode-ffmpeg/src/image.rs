//! [`mediadecode::decoder::ImageDecoder`] impl backed by
//! `libavcodec` — the cover-art road.
//!
//! A container's still images are not video, and this crate's demux
//! tier already says so: a video-shaped stream carrying
//! `AV_DISPOSITION_ATTACHED_PIC` is reclassified to
//! [`TrackKind::Attachment`](mediadecode::demuxer::TrackKind::Attachment),
//! and its one packet — the picture, whole — is queued at open. What
//! was missing was the other half: something to turn those bytes back
//! into pixels.
//!
//! # The codec identity survives the reclassification
//!
//! Nothing about the reclassification loses the picture's codec.
//! [`TrackParams::Attachment`](mediadecode::demuxer::TrackParams::Attachment)
//! carries the `AVCodecParameters.codec_id` the stream declared
//! (`mjpeg`, `png`, `bmp`, …), and
//! [`TrackExtra`](crate::extras::TrackExtra) carries a deep,
//! checked copy of the whole `AVCodecParameters` — width, height,
//! pixel format and extradata included. Those parameters are exactly
//! what [`FfmpegImageDecoder::open`] wants, so the road from a track
//! row to a decoded picture has no gap in it:
//!
//! ```no_run
//! use mediadecode::{
//!   decoder::ImageDecoder,
//!   demuxer::{Demuxer, DemuxedPacket, TrackKind},
//! };
//! use mediadecode_ffmpeg::{FfmpegOwnedDemuxer, FfmpegOwnedImageDecoder, DecoderLimits};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! // The owned lane: this decoder takes owned attachments, and a
//! // cover is a picture a caller usually keeps.
//! let mut demuxer = FfmpegOwnedDemuxer::open("song.mp3")?;
//! let cover_track = demuxer
//!   .tracks()
//!   .iter()
//!   .position(|t| t.kind() == TrackKind::Attachment)
//!   .expect("this file has cover art");
//! let parameters = demuxer.tracks()[cover_track].extra().clone_parameters()?;
//!
//! while let Some(packet) = demuxer.next_packet()? {
//!   if let DemuxedPacket::Attachment(attachment) = packet {
//!     let mut decoder = FfmpegOwnedImageDecoder::open(parameters, DecoderLimits::default())?;
//!     let image = decoder.decode(attachment.packet())?;
//!     println!("{}x{}", image.width(), image.height());
//!     break;
//!   }
//! }
//! # Ok(())
//! # }
//! ```
//!
//! # One shot, and the decoder stays open
//!
//! [`ImageDecoder::decode`] takes a packet and answers a picture; there
//! is no `send` / `receive` split, because an attachment track's
//! contract is exactly one packet and a still codec's answer to it is
//! exactly one frame. The `AVCodecContext` is nonetheless kept and
//! reset between calls rather than reopened: a container with a dozen
//! cover images (Matroska attachments, timed thumbnails exported by
//! hand) decodes them through one decoder.
//!
//! # No `DecoderOptions`
//!
//! The context opens with libavcodec's own `skip_frame`,
//! `skip_loop_filter` and `skip_idct`, and this decoder takes no
//! [`DecoderOptions`](crate::DecoderOptions). A still is one packet in
//! and one picture out, so `skip_frame` could only make it answer
//! nothing.

use derive_more::{IsVariant, TryUnwrap, Unwrap};
use ffmpeg_next::{codec::Parameters, frame};
use mediadecode::{
  PixelFormat, decoder::ImageDecoder, demuxer::AttachmentPacket, frame::ImageFrame,
  packet::PacketFlags as MdPacketFlags,
};

use crate::{
  DecoderLimits, Error, Ffmpeg, boundary,
  convert::{self, ConvertError},
  decoder::{build_codec_context, ensure_video_codec_type, find_decoder},
  extras::{AttachmentPacketExtra, ImageFrameExtra},
  frame::alloc_av_video_frame,
};

/// `mediadecode::ImageDecoder` impl wrapping `ffmpeg::decoder::Video`.
///
/// Opened from the codec parameters of an
/// [`Attachment`](mediadecode::demuxer::TrackKind::Attachment) track —
/// see the [module docs](self) for the whole road.
pub struct CarrierImageDecoder<C: crate::FfmpegCarrier> {
  decoder: ffmpeg_next::decoder::Video,
  scratch: frame::Video,
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

impl<C: crate::FfmpegCarrier + crate::CarrierOps> CarrierImageDecoder<C> {
  /// Opens a still-image decoder for the given codec parameters.
  ///
  /// The parameters come from an attachment track's
  /// [`TrackExtra::clone_parameters`](crate::extras::TrackExtra::clone_parameters),
  /// which is a checked deep copy with no tie back to the format
  /// context — so the decoder outlives the demuxer that named it.
  ///
  /// A **still** codec is what this is for, but nothing here refuses a
  /// motion one: `mjpeg`, `png` and `h264` open through the same call,
  /// and a decoder opened on a motion codec will answer with that
  /// codec's first picture. Refusing by codec id would mean minting a
  /// roster of "image codecs" this crate has no business owning — the
  /// container already said the track is an attachment, and that is
  /// the judgement that matters.
  ///
  /// `limits` bounds what one decoded picture may cost — **a cover-art
  /// bomb is the same attack as a video bomb**, and this seam is the
  /// more exposed of the two: an attachment is decoded from a payload a
  /// container handed over eagerly, often by a thumbnailer that never
  /// asked for video at all. [`DecoderLimits::max_pixels`] is written
  /// into the `AVCodecContext` opened here, so libavcodec refuses an
  /// oversized still before allocating it; the byte half is checked
  /// against what the planes would export, before this crate allocates
  /// anything.
  pub(crate) fn open_impl(
    parameters: Parameters,
    limits: DecoderLimits,
  ) -> Result<Self, ImageDecodeError> {
    // Use the checked codec-context builder — `Context::from_parameters`
    // is OOM-UB-prone (see `crate::decoder::build_codec_context`).
    // libavcodec's own skips: see the module docs' "No `DecoderOptions`".
    let (ctx, callback_state) =
      build_codec_context(&parameters, limits, crate::DecoderOptions::new(), None)
        .map_err(ImageDecodeError::Decode)?;
    // **Opened without ever forming a bindgen enum from FFmpeg memory.**
    // `Context::decoder().video()` looks cheap and is not: it resolves
    // the codec by reading `AVCodecParameters.codec_id` as the bindgen
    // `AVCodecID`, and then `Opened::video()` reads
    // `AVCodecContext.codec_type` as `AVMediaType`. Both are open C
    // enums — FFmpeg adds members in ABI-compatible releases — and
    // forming a Rust enum from a value outside this build's discriminant
    // set is UB before any comparison can run. The hardware path has
    // bypassed this API since it was written; the image road was still
    // going through it, which is the one road a *file* chooses the
    // codec id on.
    //
    // `find_decoder` does the lookup off a raw `u32`, and
    // `ensure_video_codec_type` does the check off a raw `i32`; the
    // `decoder::Video` wrapper is then constructed through its public
    // tuple field, exactly as `Opened::video()` would have on success.
    let codec = find_decoder(&parameters).map_err(ImageDecodeError::Decode)?;
    let opened = ctx
      .decoder()
      .open_as(codec)
      .map_err(|e| ImageDecodeError::Decode(Error::Ffmpeg(e)))?;
    ensure_video_codec_type(&opened).map_err(ImageDecodeError::Decode)?;
    let decoder = ffmpeg_next::decoder::Video(opened);
    let scratch = alloc_av_video_frame().map_err(|e| ImageDecodeError::Decode(e.into()))?;
    Ok(Self {
      decoder,
      scratch,
      limits,
      _callback_state: callback_state,
      _carrier: core::marker::PhantomData,
    })
  }

  /// The frame ceilings this decoder was opened with.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub(crate) const fn limits_impl(&self) -> DecoderLimits {
    self.limits
  }

  /// Borrow the wrapped `ffmpeg::decoder::Video` (e.g. to query
  /// `width()` / `height()` / `format()` before decoding).
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub(crate) const fn inner_impl(&self) -> &ffmpeg_next::decoder::Video {
    &self.decoder
  }
}

impl<C: crate::FfmpegCarrier + crate::CarrierOps> CarrierImageDecoder<C> {
  pub(crate) fn decode_impl(
    &mut self,
    packet: &AttachmentPacket<AttachmentPacketExtra, C::Buffer>,
  ) -> Result<ImageFrame<PixelFormat, ImageFrameExtra, C::Buffer>, ImageDecodeError> {
    let bytes: &[u8] = packet.data().as_ref();
    // An attachment with no bytes is no picture. The demuxer really
    // does produce these — a cover-art stream that parks no payload,
    // or an `AVMEDIA_TYPE_ATTACHMENT` track with empty extradata, both
    // arrive as a `synthesized` packet with an empty body — and
    // handing that to `avcodec_send_packet` would not be a decode but
    // a *drain* signal, which is a different thing wearing the same
    // shape.
    if bytes.is_empty() {
      return Err(ImageDecodeError::EmptyPayload);
    }
    // **The compressed-input ceiling, before the copy.** `decode` hands
    // these bytes to `try_packet_copy`, which duplicates them into an
    // `AVPacket` — a second full copy of whatever the caller holds,
    // capped by nothing but `c_int::MAX`.
    //
    // When the payload arrives through the demuxer it was already
    // charged against the attachment budget; this check is what keeps
    // the same ceiling in force on the road that skips the demux tier,
    // where a caller builds the packet itself. Without it the direct
    // road was a gigabyte more permissive than the demuxed one for the
    // same bytes.
    if bytes.len() > self.limits.max_image_input_bytes() {
      return Err(ImageDecodeError::InputTooLarge(InputTooLarge::new(
        bytes.len(),
        self.limits.max_image_input_bytes(),
      )));
    }
    // **The corruption refusal, before the ceiling and before the
    // copy.** See [`Corrupt`] for the census behind it: forwarding this
    // flag to libavcodec is measurably a no-op on the image road, and
    // an `ImageFrame` cannot carry the fact, so refusing is the only
    // handling that does not silently delete the caller's own warning.
    // It costs one bit test, which is why it runs ahead of the budget.
    // The fourth rebuild road, and it obeys the same refusal as the
    // three stream families: see `crate::buffer::TrustedPayload`. This
    // seam writes the flags onto a fresh `AVPacket` and hands it to a
    // decoder, so a `TRUSTED` bit arriving here is a decoder being told
    // it may dereference a body this crate copied by value.
    if packet.flags().bits() & crate::buffer::TRUSTED_BIT != 0 {
      return Err(ImageDecodeError::TrustedPayload(
        crate::buffer::TrustedPayload::new(bytes.len()),
      ));
    }
    if packet.flags().contains(MdPacketFlags::CORRUPT) {
      return Err(ImageDecodeError::Corrupt(Corrupt::new(
        CorruptSource::Packet,
      )));
    }
    // The decoder may still hold state from a previous picture (or
    // from a `decode` that failed part way). Reset before feeding, so
    // one call's failure cannot become the next call's frame.
    self.decoder.flush();

    let mut av_pkt =
      boundary::try_packet_copy(bytes).map_err(|e| ImageDecodeError::Decode(Error::Ffmpeg(e)))?;
    // **The flags, through the crate's one flag writer.** This road
    // rebuilt the `AVPacket` from bytes alone and left `flags` zeroed,
    // so everything the portable packet said about itself stopped here:
    // `DISCARD` — which libavcodec really does obey, measurably, by
    // producing no frame — was silently ignored, and the attachment was
    // decoded anyway. The three stream families have always gone
    // through `write_md_flags`; the image road now does too rather than
    // growing a second copy of the same three lines.
    boundary::write_md_flags(&mut av_pkt, packet.flags());
    // Through the software funnel: a frame the allocator judge refused
    // surfaces named, not as the `EINVAL` a corrupt file also produces.
    let state: *const crate::ffi::CallbackState = self._callback_state.as_ptr();
    // Through the one door ([`crate::decoder::send`]): one packet of the
    // caller's, and its answer is the payload's, whatever its class.
    //
    // SAFETY: `state` is the pointer the codec context holds, owned by
    // `self._callback_state` until after the context is freed.
    let refused = |e| ImageDecodeError::Decode(unsafe { crate::decoder::software_exit(state, e) });
    match crate::decoder::send(&mut self.decoder, crate::decoder::Sending::Packet(&av_pkt)) {
      crate::decoder::Acceptance::Accepted => {}
      crate::decoder::Acceptance::NotAccepted(e)
      | crate::decoder::Acceptance::Ambiguous(e)
      | crate::decoder::Acceptance::AcceptedWithError(e) => return Err(refused(e)),
    }
    // Every still codec answers its one packet with its one frame, but
    // it is entitled to want the end of the stream first — so the EOF
    // goes in before the drain rather than after a speculative
    // `receive_frame` that would have to interpret `EAGAIN`.
    match crate::decoder::send(&mut self.decoder, crate::decoder::Sending::End) {
      crate::decoder::Acceptance::Accepted => {}
      crate::decoder::Acceptance::NotAccepted(e)
      | crate::decoder::Acceptance::Ambiguous(e)
      | crate::decoder::Acceptance::AcceptedWithError(e) => return Err(refused(e)),
    }
    if let Err(e) = self.decoder.receive_frame(&mut self.scratch) {
      self.decoder.flush();
      // Funnelled, then classified — the same two steps every receive
      // road in this crate takes, so a recorded budget refusal is named
      // rather than mistaken for a payload that held no picture.
      //
      // **And then the classification collapses, which is the one thing
      // this road does differently.** A one-shot decode has no session
      // states to report: there is no more input the caller could send
      // and no stream to be at the end of. `EOF` from a decoder that
      // never produced a picture, and `EAGAIN` from one that has already
      // been told the stream ended, are therefore the same fact about
      // the *payload* — it is not an image this codec reads — and that
      // fact is an error, not a rhythm. (`EAGAIN` after `send_eof` is
      // not a state `avcodec` documents; it is caught here so a codec
      // that does it cannot surface as a raw errno the caller has to
      // decode.)
      return Err(
        // `Draining` is the literal truth of this road: `send_eof` ran
        // three lines up, and a one-shot decoder has no probe. Both
        // flow signals therefore read as the end — which is exactly the
        // collapse this road has always made by hand.
        //
        // SAFETY: as above — this decoder's own callback state.
        match unsafe {
          crate::decoder::software_receive(state, e, crate::decoder::SessionPhase::Draining)
        } {
          Ok(_) => ImageDecodeError::NoImage,
          Err(fault) => ImageDecodeError::Decode(fault),
        },
      );
    }
    // **The other road the same fact arrives on.** A decoder is entitled
    // to hand back a picture and mark it corrupt — that is libavcodec
    // saying it concealed errors rather than read the image — and
    // `ImageFrame` has no more room for that fact than it had for the
    // packet's. Closing one road and leaving its sibling open is the
    // shape this release has already been caught by twice.
    //
    // SAFETY: `scratch` owns a live `AVFrame` just filled by
    // `receive_frame`; `flags` is a plain `c_int` field.
    let frame_flags = unsafe { (*self.scratch.as_ptr()).flags };
    // **Two fields, one fact.** `AV_FRAME_FLAG_CORRUPT` is the loud one;
    // `decode_error_flags` is the quiet one, and it is the one h264
    // actually writes — `FF_DECODE_ERROR_INVALID_BITSTREAM` /
    // `..._MISSING_REFERENCE` / `..._CONCEALMENT_ACTIVE` /
    // `..._DECODE_SLICES` are set on a frame the decoder *concealed its
    // way through*, with the frame flag left clear. A gate that reads
    // only the flag passes exactly the frames a real decoder marks.
    //
    // Any nonzero value counts. The field is a bit set FFmpeg extends,
    // so enumerating the members this build names would re-open the
    // same door on the next release; "the decoder recorded an error"
    // is the fact, and its spelling is FFmpeg's business.
    //
    // SAFETY: same live `AVFrame`; `decode_error_flags` is a public
    // `c_int` field.
    let decode_error_flags = unsafe { (*self.scratch.as_ptr()).decode_error_flags };
    if frame_is_corrupt(frame_flags, decode_error_flags) {
      self.decoder.flush();
      return Err(ImageDecodeError::Corrupt(Corrupt::new(
        CorruptSource::DecodedFrame,
      )));
    }
    // **This road needs no parked-frame slot, and the reason is the
    // signature.** The timed decoders advance libavcodec and hand the
    // caller nothing to retry with, which is why they park a frame
    // whose conversion could not commit. Here the caller still holds
    // the attachment packet — it was *borrowed* — so a failure leaves
    // them everything needed to call again, and the decoder is flushed
    // on the way out so the next attempt starts from the same place
    // this one did.
    //
    // SAFETY: `scratch` was just filled by `receive_frame`, so libavcodec
    // wrote every byte it exports; the conversion copies every byte it
    // takes, so the scratch frame can be reused on the next call and the
    // produced `ImageFrame` outlives this decoder.
    let image = unsafe {
      convert::av_frame_to_image_frame_as::<C>(self.scratch.as_ptr(), self.limits.frame())
    }
    .map_err(|e| {
      self.decoder.flush();
      ImageDecodeError::Convert(e)
    })?;
    // Leave the decoder ready for the next attachment rather than
    // drained-and-latched at EOF.
    self.decoder.flush();
    Ok(image)
  }
}

macro_rules! image_lane_face {
  ($($lane:ty),+ $(,)?) => { $(
    impl CarrierImageDecoder<$lane> {
      /// Opens a still-image decoder for `parameters`.
      pub fn open(
        parameters: Parameters,
        limits: DecoderLimits,
      ) -> Result<Self, ImageDecodeError> {
        Self::open_impl(parameters, limits)
      }

      /// The budgets this decoder was opened with.
      pub const fn limits(&self) -> DecoderLimits {
        self.limits_impl()
      }

      /// The wrapped decoder context.
      pub const fn inner(&self) -> &ffmpeg_next::decoder::Video {
        self.inner_impl()
      }
    }

    impl ImageDecoder for CarrierImageDecoder<$lane> {
      type Adapter = Ffmpeg;
      type Buffer = <$lane as crate::FfmpegCarrier>::Buffer;
      type Error = ImageDecodeError;

      fn decode(
        &mut self,
        packet: &AttachmentPacket<AttachmentPacketExtra, Self::Buffer>,
      ) -> Result<ImageFrame<PixelFormat, ImageFrameExtra, Self::Buffer>, Self::Error> {
        self.decode_impl(packet)
      }
    }
  )+ };
}

image_lane_face!(crate::View, crate::Owned);

/// Payload for [`ImageDecodeError::InputTooLarge`].
///
/// The compressed bytes handed to one decode exceed
/// [`DecoderLimits::max_image_input_bytes`].
#[derive(thiserror::Error, Debug, Clone, Copy, PartialEq, Eq)]
#[error("a {bytes}-byte image payload exceeds the {limit}-byte input ceiling")]
pub struct InputTooLarge {
  bytes: usize,
  limit: usize,
}

impl InputTooLarge {
  /// Constructs an `InputTooLarge` payload.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(bytes: usize, limit: usize) -> Self {
    Self { bytes, limit }
  }
  /// The payload length the caller handed over.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn bytes(&self) -> usize {
    self.bytes
  }
  /// The ceiling in force.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn limit(&self) -> usize {
    self.limit
  }
}

/// Errors from [`FfmpegImageDecoder`].
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
pub enum ImageDecodeError {
  /// The compressed payload is larger than the ceiling allows. Refused
  /// **before** it is copied into an `AVPacket`.
  #[error(transparent)]
  InputTooLarge(#[from] InputTooLarge),

  /// The wrapped `ffmpeg::decoder::Video` reported an error, or the
  /// codec context could not be built.
  #[error(transparent)]
  Decode(#[from] Error),
  /// Conversion from FFmpeg's `AVFrame` to mediadecode's `ImageFrame`
  /// failed — an undeliverable pixel format, or a plane layout the
  /// safe accessors refuse.
  #[error(transparent)]
  Convert(#[from] ConvertError),
  /// The attachment carried no bytes, so there is nothing to decode.
  ///
  /// Distinct from [`Self::NoImage`]: this is an empty payload, which
  /// the demuxer synthesizes for an attachment track whose picture the
  /// container never stored.
  #[error("the attachment carries no bytes; there is no image to decode")]
  EmptyPayload,
  /// The payload was accepted and produced no picture — it is not an
  /// image this codec reads.
  #[error("the attachment's bytes decoded to no image")]
  NoImage,
  /// Something on this road is marked corrupt, and an [`ImageFrame`]
  /// has nowhere to say so.
  #[error(transparent)]
  Corrupt(#[from] Corrupt),
  /// The attachment is marked `AV_PKT_FLAG_TRUSTED`, so its body may
  /// hold pointers rather than bytes. See
  /// [`crate::buffer::TrustedPayload`].
  #[error(transparent)]
  TrustedPayload(#[from] crate::buffer::TrustedPayload),
}

/// Whether libavcodec is telling us the picture it returned is damaged.
///
/// A named predicate rather than an inline condition, because it is the
/// part of the corruption gate that can be exercised without a decoder
/// that produces the input — see the note on `decode_error_flags`
/// below.
///
/// Two fields, one fact:
///
/// * `AV_FRAME_FLAG_CORRUPT` is the loud one, and the only one the
///   original gate read.
/// * `decode_error_flags` is the quiet one, and it is the one that is
///   actually written in practice: h264 records
///   `FF_DECODE_ERROR_INVALID_BITSTREAM`, `..._MISSING_REFERENCE`,
///   `..._CONCEALMENT_ACTIVE` and `..._DECODE_SLICES` there for a frame
///   it *concealed its way through*, leaving the frame flag clear. A
///   gate reading only the flag passes exactly the frames a real
///   decoder marks.
///
/// Any nonzero value counts. The field is a bit set FFmpeg extends, so
/// enumerating the members this build names would re-open the same door
/// on the next release: "the decoder recorded an error" is the fact,
/// and its spelling is FFmpeg's business.
///
/// **Coverage, stated honestly.** No codec reachable from this crate's
/// test corpus produces a frame with `decode_error_flags` set — mjpeg,
/// which is what cover art overwhelmingly is, either refuses a damaged
/// payload outright at `send_packet` or decodes it cleanly, and the
/// corpus has no h264 attachment to exercise concealment with. This
/// predicate is unit-tested against both fields directly; the road from
/// a real damaged h264 still to a nonzero `decode_error_flags` is
/// FFmpeg's, not this crate's, and is not pinned here.
#[inline]
const fn frame_is_corrupt(frame_flags: i32, decode_error_flags: i32) -> bool {
  frame_flags & ffmpeg_next::ffi::AV_FRAME_FLAG_CORRUPT != 0 || decode_error_flags != 0
}

/// Which side of the decode declared the corruption.
///
/// Two roads, one fact. A closed vocabulary owned by this crate, so it
/// is an enum rather than a string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, IsVariant)]
pub enum CorruptSource {
  /// The caller's packet arrived with `CORRUPT` set — the demuxer that
  /// produced it found the payload damaged.
  Packet,
  /// libavcodec returned a picture and marked the frame itself corrupt
  /// (`AV_FRAME_FLAG_CORRUPT`), which is a decoder saying it concealed
  /// errors rather than read the image.
  DecodedFrame,
}

impl core::fmt::Display for CorruptSource {
  fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
    match self {
      Self::Packet => f.write_str("the attachment packet"),
      Self::DecodedFrame => f.write_str("the decoded frame"),
    }
  }
}

/// Payload for [`ImageDecodeError::Corrupt`].
///
/// # Why this is a refusal and not a forwarded flag
///
/// Measured, on this build, with a real cover-art payload:
///
/// * `AV_PKT_FLAG_CORRUPT` reaches libavcodec and libavcodec **does
///   nothing with it** — the mjpeg decoder returns a full picture and
///   leaves `AV_FRAME_FLAG_CORRUPT` clear. So forwarding the flag does
///   not preserve the fact; it only moves where it is dropped.
/// * `AV_PKT_FLAG_DISCARD`, by contrast, **is** honoured: the decoder
///   produces no frame at all. That flag is therefore forwarded and
///   nothing is decided here.
///
/// `ImageFrame` has no flag field — deliberately, it is a picture and
/// not a packet — so a corruption signal that survives the decode has
/// nowhere left to go. Returning the picture anyway would hand a caller
/// a possibly-garbage image with the one warning about it deleted en
/// route. Named refusal, both roads. A caller who wants the bytes
/// decoded regardless can clear the flag it set.
// The field is `declared_by`, not `source`: `thiserror` reads a field
// of that name as the error's `Error::source()` chain, and an inherent
// accessor spelled `source` would shadow the trait method besides.
#[derive(thiserror::Error, Debug, Clone, Copy, PartialEq, Eq)]
#[error("{declared_by} is marked corrupt; this decoder will not return a picture from it")]
pub struct Corrupt {
  declared_by: CorruptSource,
}

impl Corrupt {
  /// Constructs a `Corrupt` payload.
  #[inline]
  pub const fn new(declared_by: CorruptSource) -> Self {
    Self { declared_by }
  }
  /// Which side declared the corruption.
  #[inline]
  pub const fn declared_by(&self) -> CorruptSource {
    self.declared_by
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn the_corruption_gate_reads_both_fields() {
    const CORRUPT: i32 = ffmpeg_next::ffi::AV_FRAME_FLAG_CORRUPT;
    const KEY: i32 = 1 << 1;

    // Clean is clean.
    assert!(!frame_is_corrupt(0, 0));
    assert!(!frame_is_corrupt(KEY, 0));

    // The loud field.
    assert!(frame_is_corrupt(CORRUPT, 0));
    assert!(frame_is_corrupt(KEY | CORRUPT, 0));

    // The quiet one, which is the one h264 actually writes — this is
    // the case the gate used to pass.
    assert!(frame_is_corrupt(
      KEY,
      ffmpeg_next::ffi::FF_DECODE_ERROR_CONCEALMENT_ACTIVE
    ));
    assert!(frame_is_corrupt(
      KEY,
      ffmpeg_next::ffi::FF_DECODE_ERROR_INVALID_BITSTREAM
    ));
    assert!(frame_is_corrupt(
      KEY,
      ffmpeg_next::ffi::FF_DECODE_ERROR_MISSING_REFERENCE
    ));
    assert!(frame_is_corrupt(
      KEY,
      ffmpeg_next::ffi::FF_DECODE_ERROR_DECODE_SLICES
    ));

    // And any bit a future FFmpeg adds, without this crate learning its
    // name — which is the whole reason the test is `!= 0`.
    assert!(frame_is_corrupt(0, 1 << 30));
  }
}
