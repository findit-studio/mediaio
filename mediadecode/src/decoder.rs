//! Decoder traits — push-style streams (FFmpeg / WebCodecs / ProRes
//! RAW via VTDecompressionSession), pull-style frame sources
//! (R3D / BRAW / ARRIRAW / X-OCN / Canon RAW Light), and the one-shot
//! [`ImageDecoder`].
//!
//! # What the names say
//!
//! `Stream` in a name means the decoder has a *rhythm*: packets go in
//! over time, frames come out over time, and the two are not in step —
//! hence `send_packet` / `receive_frame` / `send_eof` / `flush`.
//! [`VideoStreamDecoder`] and [`AudioStreamDecoder`] carry it.
//! [`SubtitleDecoder`] and [`ImageDecoder`] do not, and their names say
//! so: a subtitle cue and a still image each come out of exactly the
//! packet that went in.
//!
//! All four are mirrored under [`crate::future`] with `async fn`
//! methods, in both the `!Send` and the `Send`-bounded variant.
//!
//! # What the two calls answer
//!
//! Every `send_packet` / `send_eof` here answers [`Sent`] — *accepted*
//! or *must drain* — and every `receive_frame` answers [`Received`] —
//! *produced* (a frame, on these faces), *needs input*, or *ended*.
//! `Err` is reserved for faults on both faces. The rationale, and the
//! reason those two vocabularies are exhaustive while the backends'
//! error types are not, is on the [`rhythm`](crate::rhythm) module.
//!
//! ## The feeder loop
//!
//! ```
//! use mediadecode::{Received, Sent, decoder::AudioStreamDecoder};
//! # use mediadecode::{adapter::AudioAdapter, frame::AudioFrame, packet::AudioPacket};
//! # type Frame<D> = AudioFrame<
//! #   <<D as AudioStreamDecoder>::Adapter as AudioAdapter>::SampleFormat,
//! #   <<D as AudioStreamDecoder>::Adapter as AudioAdapter>::ChannelLayout,
//! #   <<D as AudioStreamDecoder>::Adapter as AudioAdapter>::FrameExtra,
//! #   <D as AudioStreamDecoder>::Buffer,
//! # >;
//! # type Packet<D> = AudioPacket<
//! #   <<D as AudioStreamDecoder>::Adapter as AudioAdapter>::PacketExtra,
//! #   <D as AudioStreamDecoder>::Buffer,
//! # >;
//! /// Feeds one packet, draining as required, and delivers every frame
//! /// it makes ready. Answers `true` once the stream is over.
//! fn feed<D: AudioStreamDecoder>(
//!   decoder: &mut D,
//!   packet: &Packet<D>,
//!   dst: &mut Frame<D>,
//!   mut on_frame: impl FnMut(&Frame<D>),
//! ) -> Result<bool, D::Error> {
//!   loop {
//!     // `?` means what it says on both faces: only a fault leaves.
//!     match decoder.send_packet(packet)? {
//!       Sent::Accepted => break,
//!       // Not a failed submission — a full decoder. Drain and
//!       // re-offer *this same packet*; nothing consumed it.
//!       Sent::MustDrain => loop {
//!         match decoder.receive_frame(dst)? {
//!           Received::Produced => on_frame(dst),
//!           Received::NeedsInput | Received::Ended => break,
//!         }
//!       },
//!     }
//!   }
//!   loop {
//!     match decoder.receive_frame(dst)? {
//!       Received::Produced => on_frame(dst),
//!       Received::NeedsInput => return Ok(false),
//!       Received::Ended => return Ok(true),
//!     }
//!   }
//! }
//! ```
//!
//! That loop is the point. Under the older convention it could not be
//! written against the trait at all: "drain me first" and "this packet
//! is damaged" were both `Err`, indistinguishable to a generic
//! consumer, so the idiom that survived was to **offer the packet
//! twice** — submit, drain on any failure, submit again, and treat the
//! second failure as real. Twice, because once meant nothing.
//!
//! ## The drain loop
//!
//! ```
//! use mediadecode::{Received, decoder::AudioStreamDecoder};
//! # use mediadecode::{adapter::AudioAdapter, frame::AudioFrame};
//! # type Frame<D> = AudioFrame<
//! #   <<D as AudioStreamDecoder>::Adapter as AudioAdapter>::SampleFormat,
//! #   <<D as AudioStreamDecoder>::Adapter as AudioAdapter>::ChannelLayout,
//! #   <<D as AudioStreamDecoder>::Adapter as AudioAdapter>::FrameExtra,
//! #   <D as AudioStreamDecoder>::Buffer,
//! # >;
//! /// Drains everything ready right now. Answers `true` once the
//! /// stream is over, so the caller knows not to feed it again.
//! fn drain<D: AudioStreamDecoder>(
//!   decoder: &mut D,
//!   dst: &mut Frame<D>,
//!   mut on_frame: impl FnMut(&Frame<D>),
//! ) -> Result<bool, D::Error> {
//!   loop {
//!     // `?` means what it says again: only a fault leaves here.
//!     match decoder.receive_frame(dst)? {
//!       Received::Produced => on_frame(dst),
//!       Received::NeedsInput => return Ok(false),
//!       Received::Ended => return Ok(true),
//!     }
//!   }
//! }
//! ```
//!
//! The loop terminates by construction: the two non-frame arms both
//! return, and neither can be reached without the callee having decided
//! which one it is. Under the older convention — both conditions
//! arriving as unnamed `Err` variants — a caller that had already sent
//! end-of-stream could not tell "send me more" from "there is no more",
//! and a drain that answered the first forever was an ordinary,
//! silent infinite loop.
//!
//! # The buffer slot
//!
//! Every trait here carries a `Buffer` associated type bounded only by
//! `AsRef<[u8]>`, and none of them names a concrete carrier. What a
//! backend may bind there is the
//! [carrier amputation contract](crate::adapter#the-carrier-amputation-contract):
//! owned, `Send + Sync`, clone-is-a-refcount-bump, with no
//! backend-internal lifetime crossing the seam.
//!
//! # Construction is not on these traits
//!
//! Opening a decoder is each backend's own business and each
//! backend's is different — codec parameters, a WebCodecs config
//! dictionary, a clip handle. Putting a constructor on the trait would
//! force one of those spellings onto all of them, which is the same
//! stance [`crate::demuxer::Demuxer`] takes for the same reason.

use crate::{
  Received, Sent, Timebase, Timestamp,
  adapter::{AudioAdapter, ImageAdapter, SubtitleAdapter, VideoAdapter},
  demuxer::AttachmentPacket,
  frame::{AudioFrame, ImageFrame, SubtitleFrame, VideoFrame},
  packet::{AudioPacket, SubtitlePacket, VideoPacket},
};

/// Whether a decode session can emit decoded pictures at a caller-
/// requested output size instead of full coded size.
///
/// A platform-neutral **capability word** on the video decode session
/// family: every [`VideoStreamDecoder`] answers it, and a backend that
/// cannot honor a smaller output size answers
/// [`Self::Unsupported`] — never an error. See
/// [`VideoStreamDecoder::scaled_output_capability`] and
/// [`VideoStreamDecoder::request_scaled_output`].
///
/// # The determinism trade, verbatim
///
/// A backend's scaler is not pixon's — enabling scaled output trades
/// cross-backend byte-determinism for bandwidth; recommended together
/// with a pinned backend (`#50`'s face — [`DecodePath::Hardware`] /
/// [`DecodePath::Software`] in `mediadecode-ffmpeg`, or the equivalent
/// pin on any other backend). A caller that resamples with the ordinary
/// area-resample kernel elsewhere in this ecosystem, and needs the same
/// picture bytes regardless of which decode path served the stream,
/// should leave scaled output unrequested and pin the backend instead —
/// two different decoders' hardware scalers do not agree bit-for-bit,
/// where two runs of the same pinned software path do.
///
/// [`DecodePath::Hardware`]: https://docs.rs/mediadecode-ffmpeg/latest/mediadecode_ffmpeg/enum.DecodePath.html
/// [`DecodePath::Software`]: https://docs.rs/mediadecode-ffmpeg/latest/mediadecode_ffmpeg/enum.DecodePath.html
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ScaledOutputCapability {
  /// This backend has no decode-time output-size seam. Every picture
  /// comes back at full coded size regardless of what was requested —
  /// the caller decides whether to resample it afterwards.
  Unsupported,
  /// This backend can decode directly to the size most recently
  /// accepted by [`VideoStreamDecoder::request_scaled_output`].
  Supported,
}

impl ScaledOutputCapability {
  /// `true` for [`Self::Supported`].
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn is_supported(&self) -> bool {
    matches!(self, Self::Supported)
  }
}

/// Push-style video decoder. Caller submits compressed packets and
/// drains decoded frames.
///
/// Backends: FFmpeg, WebCodecs, ProRes RAW (VideoToolbox).
pub trait VideoStreamDecoder {
  /// Backend-specific vocabulary.
  type Adapter: VideoAdapter;
  /// Buffer type held by the packets and frames this decoder
  /// produces or accepts.
  type Buffer: AsRef<[u8]>;
  /// Decoder-specific error type.
  type Error;

  /// Submits one compressed packet.
  ///
  /// [`Sent::Accepted`] means the packet was consumed.
  /// [`Sent::MustDrain`] means it was **not** — the session's output
  /// must be drained through [`receive_frame`](Self::receive_frame)
  /// and the same packet offered again. That is back pressure, not a
  /// refusal, and it is why `Err` here can be read as a fault without
  /// a second attempt.
  fn send_packet(
    &mut self,
    packet: &VideoPacket<<Self::Adapter as VideoAdapter>::PacketExtra, Self::Buffer>,
  ) -> Result<Sent, Self::Error>;

  /// Drains one decoded frame into `dst`.
  ///
  /// [`Received::Produced`] means `dst` was written.
  /// [`Received::NeedsInput`] and [`Received::Ended`] are the protocol's
  /// other two answers, and neither is an error — `Err` is a fault.
  fn receive_frame(
    &mut self,
    dst: &mut VideoFrame<
      <Self::Adapter as VideoAdapter>::PixelFormat,
      <Self::Adapter as VideoAdapter>::FrameExtra,
      Self::Buffer,
    >,
  ) -> Result<Received, Self::Error>;

  /// Signals end-of-stream.
  ///
  /// Answers [`Sent`] for the same reason
  /// [`send_packet`](Self::send_packet) does: a session with undrained
  /// output can be unable to take the signal yet, and
  /// [`Sent::MustDrain`] means the end-of-stream was **not** recorded.
  /// Drain and signal again.
  fn send_eof(&mut self) -> Result<Sent, Self::Error>;

  /// Flushes internal state. Not a submission — nothing is offered, so
  /// there is nothing to be back-pressured.
  fn flush(&mut self) -> Result<(), Self::Error>;

  /// Whether this session can currently emit pictures at a caller-
  /// requested output size instead of full coded size. See
  /// [`ScaledOutputCapability`] for what a `Self::Supported` answer
  /// costs a caller who acts on it.
  ///
  /// A pure query — it does not itself request anything, and calling
  /// it has no effect on what [`receive_frame`](Self::receive_frame)
  /// delivers. Defaults to [`ScaledOutputCapability::Unsupported`] so
  /// every existing implementor of this trait keeps compiling; a
  /// backend that can honor a request overrides both this and
  /// [`request_scaled_output`](Self::request_scaled_output) to say so.
  fn scaled_output_capability(&self) -> ScaledOutputCapability {
    ScaledOutputCapability::Unsupported
  }

  /// Requests that this session emit decoded pictures at `size`
  /// (width, height) from here on, and reports whether the request was
  /// honored.
  ///
  /// Refusal is **not an error**: a backend that cannot honor the
  /// request answers [`ScaledOutputCapability::Unsupported`] and
  /// leaves the session decoding at full coded size — nothing about
  /// [`send_packet`](Self::send_packet) or
  /// [`receive_frame`](Self::receive_frame) can fail because a
  /// requested size was refused. The caller reads the return value (or
  /// calls [`scaled_output_capability`](Self::scaled_output_capability)
  /// again) to learn which happened, and falls back to resampling the
  /// full-size picture itself when it did not take effect.
  ///
  /// Defaults to a no-op that always answers
  /// [`ScaledOutputCapability::Unsupported`], matching
  /// [`scaled_output_capability`](Self::scaled_output_capability)'s
  /// default — every existing implementor keeps compiling.
  fn request_scaled_output(&mut self, size: (u32, u32)) -> ScaledOutputCapability {
    let _ = size;
    ScaledOutputCapability::Unsupported
  }
}

/// Pull-style video frame source. Caller requests frames by integer
/// index. Clip-level metadata accessible via `clip_meta()`.
///
/// Backends: R3D, BRAW, ARRIRAW, Sony X-OCN, Canon Cinema RAW Light.
pub trait VideoFrameSource {
  /// Backend-specific vocabulary.
  type Adapter: VideoAdapter;
  /// Buffer type for the produced frames.
  type Buffer: AsRef<[u8]>;
  /// Backend-specific clip-level metadata bag (e.g. `R3dClipMeta`,
  /// `ArriClipMeta`). Backends without clip metadata set this to `()`.
  type ClipMeta;
  /// Decoder-specific error type.
  type Error;

  /// Total frame count in the clip.
  fn frame_count(&self) -> u64;
  /// Video frame rate (frames per second as a `Timebase`).
  fn frame_rate(&self) -> Timebase;
  /// Total clip duration.
  fn duration(&self) -> Timestamp;
  /// Backend-specific clip-level metadata.
  fn clip_meta(&self) -> &Self::ClipMeta;

  /// Decodes one frame at `index` into `dst`.
  fn decode_frame(
    &mut self,
    index: u64,
    dst: &mut VideoFrame<
      <Self::Adapter as VideoAdapter>::PixelFormat,
      <Self::Adapter as VideoAdapter>::FrameExtra,
      Self::Buffer,
    >,
  ) -> Result<(), Self::Error>;
}

/// Push-style audio decoder.
pub trait AudioStreamDecoder {
  /// Backend vocabulary.
  type Adapter: AudioAdapter;
  /// Buffer type.
  type Buffer: AsRef<[u8]>;
  /// Decoder-specific error.
  type Error;
  /// Submits a compressed audio packet. See
  /// [`VideoStreamDecoder::send_packet`] — the two answers are the same
  /// on every push face here.
  fn send_packet(
    &mut self,
    packet: &AudioPacket<<Self::Adapter as AudioAdapter>::PacketExtra, Self::Buffer>,
  ) -> Result<Sent, Self::Error>;
  /// Drains a decoded frame into `dst`. See
  /// [`VideoStreamDecoder::receive_frame`] — the three answers are the
  /// same on every push face here.
  fn receive_frame(
    &mut self,
    dst: &mut AudioFrame<
      <Self::Adapter as AudioAdapter>::SampleFormat,
      <Self::Adapter as AudioAdapter>::ChannelLayout,
      <Self::Adapter as AudioAdapter>::FrameExtra,
      Self::Buffer,
    >,
  ) -> Result<Received, Self::Error>;
  /// Signals EOF.
  fn send_eof(&mut self) -> Result<Sent, Self::Error>;
  /// Flushes internal state.
  fn flush(&mut self) -> Result<(), Self::Error>;
}

/// Pull-style audio frame source. Caller requests blocks by sample
/// offset.
///
/// Backends: R3D, BRAW (audio in companion track of the same clip).
pub trait AudioFrameSource {
  /// Backend vocabulary.
  type Adapter: AudioAdapter;
  /// Buffer type.
  type Buffer: AsRef<[u8]>;
  /// Backend-specific clip-level metadata.
  type ClipMeta;
  /// Decoder-specific error.
  type Error;
  /// Total sample count across all channels.
  fn sample_count(&self) -> u64;
  /// Sample rate (Hz).
  fn sample_rate(&self) -> u32;
  /// Channel count.
  fn channel_count(&self) -> u8;
  /// Backend-specific clip metadata.
  fn clip_meta(&self) -> &Self::ClipMeta;
  /// Decodes a block starting at `sample_offset`, of `sample_count` samples.
  fn decode_block(
    &mut self,
    sample_offset: u64,
    sample_count: u32,
    dst: &mut AudioFrame<
      <Self::Adapter as AudioAdapter>::SampleFormat,
      <Self::Adapter as AudioAdapter>::ChannelLayout,
      <Self::Adapter as AudioAdapter>::FrameExtra,
      Self::Buffer,
    >,
  ) -> Result<(), Self::Error>;
}

/// Push-style subtitle decoder. (No pull-style subtitle decoders
/// exist in the wild — subtitle streams are linear and small.)
pub trait SubtitleDecoder {
  /// Backend vocabulary.
  type Adapter: SubtitleAdapter;
  /// Buffer type.
  type Buffer: AsRef<[u8]>;
  /// Decoder-specific error.
  type Error;
  /// Submits a compressed subtitle packet. See
  /// [`VideoStreamDecoder::send_packet`].
  fn send_packet(
    &mut self,
    packet: &SubtitlePacket<<Self::Adapter as SubtitleAdapter>::PacketExtra, Self::Buffer>,
  ) -> Result<Sent, Self::Error>;
  /// Drains a decoded subtitle frame into `dst`.
  ///
  /// A backend whose underlying API produces a cue inline with the
  /// packet still answers all three: [`Received::NeedsInput`] until a
  /// packet has produced one, and [`Received::Ended`] once
  /// [`send_eof`](Self::send_eof) has been signalled and no cue is
  /// held. A subtitle decoder with no tail to drain is still a session
  /// with an end, and saying so is what lets a generic drain loop stop.
  fn receive_frame(
    &mut self,
    dst: &mut SubtitleFrame<<Self::Adapter as SubtitleAdapter>::FrameExtra, Self::Buffer>,
  ) -> Result<Received, Self::Error>;
  /// Signals EOF.
  fn send_eof(&mut self) -> Result<Sent, Self::Error>;
  /// Flushes internal state.
  fn flush(&mut self) -> Result<(), Self::Error>;
}

/// One-shot still-image decoder — cover art, an embedded thumbnail, a
/// poster frame.
///
/// **No `Stream` in the name, and no rhythm to go with it.** An
/// attachment track delivers exactly one packet (see
/// [`Demuxer`](crate::demuxer::Demuxer)'s attachment contract), that
/// packet is a whole file, and decoding it produces exactly one
/// picture. There is nothing to queue, nothing to drain, and no
/// end-of-stream to signal — so [`decode`](Self::decode) takes the
/// packet and returns the frame, instead of the
/// `send_packet` / `receive_frame` split the two `*StreamDecoder`
/// traits need. [`SubtitleDecoder`] keeps that split only because
/// FFmpeg's own subtitle API is push-shaped; nothing forces it here.
///
/// **The frame has no timestamps**, because
/// [`ImageFrame`] has no fields for them.
///
/// # Where the input comes from
///
/// A container's still images arrive as
/// [`TrackKind::Attachment`](crate::demuxer::TrackKind::Attachment)
/// tracks: the track row says what codec the picture is in, and the
/// track's one [`AttachmentPacket`] carries its bytes. So the row
/// opens the decoder — off-trait, per the module docs — and the packet
/// is what `decode` is handed.
///
/// # `&mut self`
///
/// Decoding one image needs no state across calls; the exclusive
/// borrow is here because a backend's decoder handle is a mutable
/// resource (FFmpeg's `AVCodecContext` is), and because it lets a
/// backend reuse one open decoder across several attachments of the
/// same codec rather than reopening per picture.
pub trait ImageDecoder {
  /// Backend-specific vocabulary.
  type Adapter: ImageAdapter;
  /// Buffer type held by the packet this decoder accepts and the frame
  /// it produces. See the module docs for what may be bound here.
  type Buffer: AsRef<[u8]>;
  /// Decoder-specific error type.
  type Error;

  /// Decodes one attachment payload — a whole image file — into a
  /// still.
  ///
  /// Backends signal "these bytes are not a picture this decoder can
  /// read" through a backend-specific `Error` variant, never by
  /// returning an empty frame. That is **not** the receive-path
  /// convention wearing different clothes: [`Received`] names the
  /// states of a *session* — nothing yet, over — and a one-shot decode
  /// has neither. "These bytes are not a picture" is a fact about the
  /// payload the caller handed over, which is what an error is for.
  fn decode(
    &mut self,
    packet: &AttachmentPacket<<Self::Adapter as ImageAdapter>::PacketExtra, Self::Buffer>,
  ) -> Result<
    ImageFrame<
      <Self::Adapter as ImageAdapter>::PixelFormat,
      <Self::Adapter as ImageAdapter>::FrameExtra,
      Self::Buffer,
    >,
    Self::Error,
  >;
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::Timebase;
  use core::num::NonZeroI32;

  pub(crate) struct VLoop;
  impl VideoAdapter for VLoop {
    type CodecId = u32;
    type PixelFormat = u32;
    type PacketExtra = ();
    type FrameExtra = ();
  }

  /// Trivial loopback impl — confirms the trait can be implemented.
  pub(crate) struct LoopVideoStream;

  #[derive(Debug)]
  pub(crate) struct LoopError;

  impl VideoStreamDecoder for LoopVideoStream {
    type Adapter = VLoop;
    type Buffer = &'static [u8];
    type Error = LoopError;

    fn send_packet(&mut self, _: &VideoPacket<(), &'static [u8]>) -> Result<Sent, LoopError> {
      Ok(Sent::Accepted)
    }
    fn receive_frame(
      &mut self,
      _: &mut VideoFrame<u32, (), &'static [u8]>,
    ) -> Result<Received, LoopError> {
      Ok(Received::NeedsInput)
    }
    fn send_eof(&mut self) -> Result<Sent, LoopError> {
      Ok(Sent::Accepted)
    }
    fn flush(&mut self) -> Result<(), LoopError> {
      Ok(())
    }
  }

  pub(crate) struct LoopVideoSource;

  impl VideoFrameSource for LoopVideoSource {
    type Adapter = VLoop;
    type Buffer = &'static [u8];
    type ClipMeta = ();
    type Error = LoopError;

    fn frame_count(&self) -> u64 {
      0
    }
    fn frame_rate(&self) -> Timebase {
      Timebase::new(30, NonZeroI32::new(1).unwrap())
    }
    fn duration(&self) -> Timestamp {
      Timestamp::new(0, self.frame_rate())
    }
    fn clip_meta(&self) -> &() {
      &()
    }
    fn decode_frame(
      &mut self,
      _: u64,
      _: &mut VideoFrame<u32, (), &'static [u8]>,
    ) -> Result<(), LoopError> {
      Err(LoopError)
    }
  }

  #[test]
  fn video_traits_are_implementable() {
    fn _stream<D: VideoStreamDecoder>() {}
    fn _source<D: VideoFrameSource>() {}
    _stream::<LoopVideoStream>();
    _source::<LoopVideoSource>();
  }

  /// An implementor that overrides neither new method keeps compiling
  /// (the whole point of a default) and answers the honest "no seam"
  /// value — never an error, and requesting a size has no effect on
  /// what [`VideoStreamDecoder::receive_frame`] would deliver.
  #[test]
  fn scaled_output_defaults_to_unsupported_and_never_errors() {
    let mut decoder = LoopVideoStream;
    assert_eq!(
      decoder.scaled_output_capability(),
      ScaledOutputCapability::Unsupported
    );
    assert_eq!(
      decoder.request_scaled_output((64, 64)),
      ScaledOutputCapability::Unsupported
    );
    // The query is unaffected by the request that just ran.
    assert_eq!(
      decoder.scaled_output_capability(),
      ScaledOutputCapability::Unsupported
    );
  }

  #[test]
  fn scaled_output_capability_is_supported_predicate_agrees_with_the_variant() {
    assert!(ScaledOutputCapability::Supported.is_supported());
    assert!(!ScaledOutputCapability::Unsupported.is_supported());
  }

  pub(crate) struct ALoop;
  impl AudioAdapter for ALoop {
    type CodecId = u32;
    type SampleFormat = u32;
    type ChannelLayout = u32;
    type PacketExtra = ();
    type FrameExtra = ();
  }

  pub(crate) struct LoopAudioStream;

  impl AudioStreamDecoder for LoopAudioStream {
    type Adapter = ALoop;
    type Buffer = &'static [u8];
    type Error = LoopError;
    fn send_packet(&mut self, _: &AudioPacket<(), &'static [u8]>) -> Result<Sent, LoopError> {
      Ok(Sent::Accepted)
    }
    fn receive_frame(
      &mut self,
      _: &mut AudioFrame<u32, u32, (), &'static [u8]>,
    ) -> Result<Received, LoopError> {
      Ok(Received::NeedsInput)
    }
    fn send_eof(&mut self) -> Result<Sent, LoopError> {
      Ok(Sent::Accepted)
    }
    fn flush(&mut self) -> Result<(), LoopError> {
      Ok(())
    }
  }

  pub(crate) struct LoopAudioSource;

  impl AudioFrameSource for LoopAudioSource {
    type Adapter = ALoop;
    type Buffer = &'static [u8];
    type ClipMeta = ();
    type Error = LoopError;
    fn sample_count(&self) -> u64 {
      0
    }
    fn sample_rate(&self) -> u32 {
      48_000
    }
    fn channel_count(&self) -> u8 {
      2
    }
    fn clip_meta(&self) -> &() {
      &()
    }
    fn decode_block(
      &mut self,
      _: u64,
      _: u32,
      _: &mut AudioFrame<u32, u32, (), &'static [u8]>,
    ) -> Result<(), LoopError> {
      Err(LoopError)
    }
  }

  #[test]
  fn audio_traits_are_implementable() {
    fn _stream<D: AudioStreamDecoder>() {}
    fn _source<D: AudioFrameSource>() {}
    _stream::<LoopAudioStream>();
    _source::<LoopAudioSource>();
  }

  pub(crate) struct SLoop;
  impl SubtitleAdapter for SLoop {
    type CodecId = u32;
    type PacketExtra = ();
    type FrameExtra = ();
  }

  pub(crate) struct LoopSubtitleStream;

  impl SubtitleDecoder for LoopSubtitleStream {
    type Adapter = SLoop;
    type Buffer = &'static [u8];
    type Error = LoopError;
    fn send_packet(&mut self, _: &SubtitlePacket<(), &'static [u8]>) -> Result<Sent, LoopError> {
      Ok(Sent::Accepted)
    }
    fn receive_frame(
      &mut self,
      _: &mut SubtitleFrame<(), &'static [u8]>,
    ) -> Result<Received, LoopError> {
      Ok(Received::NeedsInput)
    }
    fn send_eof(&mut self) -> Result<Sent, LoopError> {
      Ok(Sent::Accepted)
    }
    fn flush(&mut self) -> Result<(), LoopError> {
      Ok(())
    }
  }

  #[test]
  fn subtitle_decoder_is_implementable() {
    fn _decoder<D: SubtitleDecoder>() {}
    _decoder::<LoopSubtitleStream>();
  }

  pub(crate) struct ILoop;
  impl ImageAdapter for ILoop {
    type CodecId = u32;
    type PixelFormat = u32;
    type PacketExtra = ();
    type FrameExtra = ();
  }

  pub(crate) struct LoopImage;

  impl ImageDecoder for LoopImage {
    type Adapter = ILoop;
    type Buffer = &'static [u8];
    type Error = LoopError;

    fn decode(
      &mut self,
      _: &AttachmentPacket<(), &'static [u8]>,
    ) -> Result<ImageFrame<u32, (), &'static [u8]>, LoopError> {
      Err(LoopError)
    }
  }

  #[test]
  fn image_decoder_is_implementable() {
    fn _decoder<D: ImageDecoder>() {}
    _decoder::<LoopImage>();
  }

  #[test]
  fn the_one_shot_seam_takes_a_packet_and_answers_a_frame() {
    // The shape the register turns on: no `send_*`, no `receive_*`, no
    // `flush`. One call in, one picture out.
    let mut decoder = LoopImage;
    let packet: AttachmentPacket<(), &'static [u8]> = AttachmentPacket::new(&[][..], ());
    assert!(decoder.decode(&packet).is_err());
  }

  /// The push rhythm against a decoder face: the tests that implement
  /// `AudioStreamDecoder` to prove `Sent` and `Received` make the
  /// two-offer rule writable against the trait alone.
  mod rhythm {
    use crate::{
      Received, Sent,
      adapter::AudioAdapter,
      decoder::AudioStreamDecoder,
      frame::{AudioFrame, Plane},
      packet::AudioPacket,
    };

    struct ALoop;
    impl AudioAdapter for ALoop {
      type CodecId = u32;
      type SampleFormat = u32;
      type ChannelLayout = u32;
      type PacketExtra = ();
      type FrameExtra = ();
    }

    /// **Faults only**, and with the two protocol states gone there is
    /// nothing this decoder can fail at — which is the shape the reform
    /// produces at the small end.
    #[derive(Debug, PartialEq, Eq)]
    struct Fault;

    /// A decoder with a **one-frame output queue**: the smallest thing
    /// that can answer [`Sent::MustDrain`] and mean it.
    struct Squeeze {
      queued: bool,
      eof: bool,
      /// Counts submissions the session actually consumed, so a test can
      /// prove a back-pressured packet was not silently eaten.
      accepted: u32,
    }

    impl AudioStreamDecoder for Squeeze {
      type Adapter = ALoop;
      type Buffer = &'static [u8];
      type Error = Fault;

      fn send_packet(&mut self, _: &AudioPacket<(), &'static [u8]>) -> Result<Sent, Fault> {
        // **The terminal state is checked first, and a fixture that
        // models the face has to model this too.** `MustDrain` promises
        // that draining makes the same offer acceptable; past the end it
        // never does, so answering it under a held frame would send a
        // caller round a loop that cannot finish — and the drained retry
        // would then be *accepted*, un-ending a stream that ended.
        if self.eof {
          return Err(Fault);
        }
        if self.queued {
          // Nothing consumed. The caller still owns this packet.
          return Ok(Sent::MustDrain);
        }
        self.queued = true;
        self.accepted += 1;
        Ok(Sent::Accepted)
      }

      fn receive_frame(
        &mut self,
        _: &mut AudioFrame<u32, u32, (), &'static [u8]>,
      ) -> Result<Received, Fault> {
        if self.queued {
          self.queued = false;
          return Ok(Received::Produced);
        }
        Ok(if self.eof {
          Received::Ended
        } else {
          Received::NeedsInput
        })
      }

      fn send_eof(&mut self) -> Result<Sent, Fault> {
        // A held frame is output too: the queue has to clear before the
        // end can be recorded, and saying so is the same back pressure.
        if self.queued {
          return Ok(Sent::MustDrain);
        }
        self.eof = true;
        Ok(Sent::Accepted)
      }

      fn flush(&mut self) -> Result<(), Fault> {
        self.queued = false;
        self.eof = false;
        Ok(())
      }
    }

    fn packet() -> AudioPacket<(), &'static [u8]> {
      AudioPacket::new(&b"compressed"[..], ())
    }

    fn frame() -> AudioFrame<u32, u32, (), &'static [u8]> {
      const EMPTY: &[u8] = &[];
      AudioFrame::new(48_000, 1024, 2, 0, 0, [Plane::new(EMPTY, 0); 8], 1, ())
    }

    /// **The two-offer rule, written once and generically.**
    ///
    /// Bounded on the trait alone — no backend type, no predicate, no
    /// error inspection. That is the property under test: before the
    /// reform this function could not exist, because "drain me first" and
    /// "this packet is damaged" were both `Err` on a `Self::Error` the
    /// bound says nothing about. The idiom that replaced it was to submit
    /// the packet **twice** and treat the second failure as real — a
    /// guess that is wrong whenever one drain is not enough.
    ///
    /// Here the loop offers until the answer is `Accepted`, and `?` is
    /// honest: only a fault leaves.
    fn feed<D: AudioStreamDecoder>(
      decoder: &mut D,
      packet: &AudioPacket<<D::Adapter as AudioAdapter>::PacketExtra, D::Buffer>,
      dst: &mut AudioFrame<
        <D::Adapter as AudioAdapter>::SampleFormat,
        <D::Adapter as AudioAdapter>::ChannelLayout,
        <D::Adapter as AudioAdapter>::FrameExtra,
        D::Buffer,
      >,
      frames: &mut u32,
      offers: &mut u32,
    ) -> Result<(), D::Error> {
      loop {
        *offers += 1;
        match decoder.send_packet(packet)? {
          Sent::Accepted => return Ok(()),
          Sent::MustDrain => {
            while let Received::Produced = decoder.receive_frame(dst)? {
              *frames += 1;
            }
          }
        }
      }
    }

    #[test]
    fn the_two_offer_rule_is_writable_against_the_trait_alone() {
      let mut decoder = Squeeze {
        queued: false,
        eof: false,
        accepted: 0,
      };
      let packet = packet();
      let mut dst = frame();
      let (mut frames, mut offers) = (0, 0);

      // First packet: the queue is empty, so it goes straight in.
      feed(&mut decoder, &packet, &mut dst, &mut frames, &mut offers).expect("no fault");
      assert_eq!((offers, frames), (1, 0));

      // Second packet: the queue is occupied. The loop is told so, drains
      // the one frame, and re-offers the SAME packet — which is only
      // sound because `MustDrain` promises nothing was consumed.
      feed(&mut decoder, &packet, &mut dst, &mut frames, &mut offers).expect("no fault");
      assert_eq!(
        (offers, frames),
        (3, 1),
        "one refused offer, one drained frame, one accepted offer",
      );
      assert_eq!(
        decoder.accepted, 2,
        "a back-pressured submission must not be counted as consumed",
      );
    }

    #[test]
    fn end_of_stream_is_back_pressured_the_same_way() {
      let mut decoder = Squeeze {
        queued: false,
        eof: false,
        accepted: 0,
      };
      let mut dst = frame();
      decoder
        .send_packet(&packet())
        .expect("no fault")
        .is_accepted()
        .then_some(())
        .expect("the empty queue takes it");

      // The queue is full, so the end cannot be recorded yet — and the
      // caller is told which of the two it is.
      assert_eq!(decoder.send_eof(), Ok(Sent::MustDrain));
      assert_eq!(decoder.receive_frame(&mut dst), Ok(Received::Produced));
      assert_eq!(decoder.send_eof(), Ok(Sent::Accepted));
      assert_eq!(decoder.receive_frame(&mut dst), Ok(Received::Ended));
    }
  }
}
