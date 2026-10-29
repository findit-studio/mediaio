//! `Ffmpeg` adapter — implements [`mediaspine::VideoAdapter`],
//! [`mediaspine::AudioAdapter`], [`mediaspine::SubtitleAdapter`],
//! [`mediaspine::adapter::ImageAdapter`] and
//! [`mediaspine::demuxer::DemuxAdapter`] for this crate.
//!
//! The adapter is a zero-sized type whose sole purpose is to bind the
//! associated types together so the rest of the API (Packet / Frame /
//! Decoder) reads cleanly: `VideoPacket<Ffmpeg, FfmpegBytes>` etc.
//!
//! `DemuxAdapter` bundles the other three, and `Ffmpeg` fills all four
//! slots with itself — the demux tier's `CodecId` bound (one
//! codec-identifier namespace across a container's whole track table) is
//! trivially satisfied when every family already binds
//! [`crate::CodecId`].

use mediaframe::audio::ChannelLayoutDescription;
use mediaspine::{
  PixelFormat,
  adapter::{AudioAdapter, ImageAdapter, SubtitleAdapter, VideoAdapter},
  demuxer::DemuxAdapter,
};
use smol_bytes::Utf8Bytes;

use crate::{
  codec_id::CodecId,
  extras::{
    AttachmentPacketExtra, AudioFrameExtra, AudioPacketExtra, DataPacketExtra, ImageFrameExtra,
    SubtitleFrameExtra, SubtitlePacketExtra, TrackExtra, VideoFrameExtra, VideoPacketExtra,
  },
  sample_format::SampleFormat,
};

/// Zero-sized type carrying the FFmpeg adapter's vocabulary.
///
/// Used as the `A` parameter on `mediaspine::VideoPacket<A, B>` /
/// `Frame<A, B>` (and audio / subtitle counterparts) when this crate's
/// decoders are in play. Construction is `Ffmpeg` (unit struct);
/// nothing about the adapter is stateful.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct Ffmpeg;

impl VideoAdapter for Ffmpeg {
  type CodecId = CodecId;
  type PixelFormat = PixelFormat;
  type PacketExtra = VideoPacketExtra;
  type FrameExtra = VideoFrameExtra;
}

impl AudioAdapter for Ffmpeg {
  type CodecId = CodecId;
  type SampleFormat = SampleFormat;
  type ChannelLayout = ChannelLayoutDescription;
  type PacketExtra = AudioPacketExtra;
  type FrameExtra = AudioFrameExtra;
}

impl SubtitleAdapter for Ffmpeg {
  type CodecId = CodecId;
  type PacketExtra = SubtitlePacketExtra;
  type FrameExtra = SubtitleFrameExtra;
}

impl ImageAdapter for Ffmpeg {
  type CodecId = CodecId;
  type PixelFormat = PixelFormat;
  // The packet an image decoder is fed is the attachment packet the
  // demuxer hands out — one type, both slots, so a cover-art payload
  // goes straight from `next_packet` into `decode` with nothing to
  // convert in between.
  type PacketExtra = AttachmentPacketExtra;
  type FrameExtra = ImageFrameExtra;
}

impl DemuxAdapter for Ffmpeg {
  type CodecId = CodecId;
  type Video = Ffmpeg;
  type Audio = Ffmpeg;
  type Subtitle = Ffmpeg;
  type DataExtra = DataPacketExtra;
  type AttachmentExtra = AttachmentPacketExtra;
  type TrackExtra = TrackExtra;
  // `AVStream.metadata` hands out borrowed `&str` that dies with the
  // format context, so a track row has to own its identity strings.
  // **`Utf8Bytes`, and the reason is allocation failure rather than
  // size.** A metadata value is read out of a container, so its length
  // is attacker-controlled; the demuxer therefore measures it, charges
  // it against a budget, and builds it in a `String` reserved with
  // `try_reserve_exact`, so exhaustion is a named error rather than an
  // abort.
  //
  // That whole chain is only worth anything if the last step — handing
  // the buffer to this carrier — allocates nothing more. `SmolStr` sat
  // here before and could not: its constructor takes a `&str` and
  // copies into a fresh `Arc<str>` for anything past 23 bytes, so a
  // second, infallible allocation of an attacker-sized value happened
  // *after* the fallible one, with the first still live. Failing there
  // aborted the process, which is exactly what the budget existed to
  // prevent.
  //
  // `Utf8Bytes::from(String)` **moves** the buffer instead: short
  // values go inline (`smol_bytes::INLINE_CAP`, no allocation at all)
  // and longer ones reach `bytes::Bytes::from(Vec<u8>)`, which takes
  // the vector's own allocation over rather than copying it. See
  // `demuxer::lossy_text` for the one residue that remains and why it
  // is not attacker-scaled.
  //
  // It is also the carrier every text slot in this household is
  // supposed to be on.
  type Text = Utf8Bytes;
}

#[cfg(test)]
mod tests {
  use super::*;

  /// Compile-time proof that the three trait impls' associated types
  /// resolve correctly when the `Ffmpeg` adapter parameterizes
  /// mediaspine's generic types.
  #[test]
  fn adapter_parameterizes_mediadecode_types() {
    use crate::FfmpegBytes;

    use mediaspine::{
      adapter::{AudioAdapter, SubtitleAdapter, VideoAdapter},
      packet::{AudioPacket, SubtitlePacket, VideoPacket},
    };

    fn _video_packet_resolves(
      _: &VideoPacket<Ffmpeg, FfmpegBytes>,
      _: <Ffmpeg as VideoAdapter>::CodecId,
      _: <Ffmpeg as VideoAdapter>::PixelFormat,
      _: &<Ffmpeg as VideoAdapter>::PacketExtra,
      _: &<Ffmpeg as VideoAdapter>::FrameExtra,
    ) {
    }

    fn _audio_packet_resolves(
      _: &AudioPacket<Ffmpeg, FfmpegBytes>,
      _: <Ffmpeg as AudioAdapter>::CodecId,
      _: <Ffmpeg as AudioAdapter>::SampleFormat,
      _: &<Ffmpeg as AudioAdapter>::ChannelLayout,
      _: &<Ffmpeg as AudioAdapter>::PacketExtra,
      _: &<Ffmpeg as AudioAdapter>::FrameExtra,
    ) {
    }

    fn _subtitle_packet_resolves(
      _: &SubtitlePacket<Ffmpeg, FfmpegBytes>,
      _: <Ffmpeg as SubtitleAdapter>::CodecId,
      _: &<Ffmpeg as SubtitleAdapter>::PacketExtra,
      _: &<Ffmpeg as SubtitleAdapter>::FrameExtra,
    ) {
    }
  }

  #[test]
  fn ffmpeg_is_zero_sized() {
    use core::mem::size_of;
    assert_eq!(size_of::<Ffmpeg>(), 0);
  }
}
