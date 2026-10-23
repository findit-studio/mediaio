#![doc = include_str!("../README.md")]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![cfg_attr(docsrs, allow(unused_attributes))]
#![deny(missing_docs)]
// The core crate carries this same allow, for this same reason, and 0.9
// is where this crate joined it: a signature here reads
// `VideoPacket<VideoPacketExtra, FfmpegBytes>` because every one of those
// three names is load-bearing — the household, the backend's extras,
// and the owned carrier the carrier amputation contract requires.
// `clippy::type_complexity` counts nesting, and the fix it asks for is
// an alias that hides exactly the word this release exists to make
// visible. 0.8 had that alias; it was called `FfmpegBuffer`.
#![allow(clippy::type_complexity)]

mod audio;
mod backend;
mod decoder;
mod demuxer;
mod error;
mod image;
mod subtitle;
mod video;
mod vtscale;

// The crossing lives in `mediaio-ffmpeg`. Its modules are named here at
// the crate root so the machines below reach them by the paths they
// always used; the public ones are re-exported under the same names.
#[cfg(test)]
use mediaio_ffmpeg::view;
pub use mediaio_ffmpeg::{boundary, channel_layout, convert, extras, limits, ticket};
use mediaio_ffmpeg::{buffer, ffi, footprint, frame, measured, reader_guard, text};

pub use mediaio_ffmpeg::{
  AttachmentPacket, AudioFrame, AudioPacket, ChannelLayoutFault, ChannelLayoutTicket, Chapter,
  CodecId, CodecTicket, ContainerFormat, CrossingError, CustomChannel,
  DEFAULT_MAX_ATTACHMENT_BYTES, DEFAULT_MAX_CHAPTERS, DEFAULT_MAX_CODEC_PARAMETER_BYTES,
  DEFAULT_MAX_FRAME_BYTES, DEFAULT_MAX_IMAGE_INPUT_BYTES, DEFAULT_MAX_IMAGE_SIDE_DATA_BYTES,
  DEFAULT_MAX_PACKET_BYTES, DEFAULT_MAX_PIXELS, DEFAULT_MAX_PROBE_BYTES, DEFAULT_MAX_STREAMS,
  DEFAULT_MAX_TOTAL_ATTACHMENT_BYTES, DEFAULT_MAX_TOTAL_CHAPTER_TITLE_BYTES,
  DEFAULT_MAX_TOTAL_CODEC_PARAMETER_BYTES, DEFAULT_MAX_TOTAL_STREAM_METADATA_BYTES, DataPacket,
  DecoderLimits, DemuxLimits, DemuxedPacket, DemuxerName, DolbyVisionConfig, Ffmpeg, FfmpegBuffer,
  FfmpegBytes, FfmpegCarrier, Frame, FrameLimits, ImageFrame, MediaKind, Owned,
  OwnedAttachmentPacket, OwnedAudioFrame, OwnedAudioPacket, OwnedDataPacket, OwnedDemuxedPacket,
  OwnedImageFrame, OwnedSubtitleFrame, OwnedSubtitlePacket, OwnedVideoFrame, OwnedVideoPacket,
  PacketBufferError, PacketBuildError, PacketLimits, ParametersChannelMap, ParametersError,
  ParametersLayoutShape, Ratio, SampleFormat, SendPayloadTooLarge, SendSideDataTooLarge,
  SubtitleFrame, SubtitlePacket, TrackInfo, TrackParams, TrustedPayload, UnregisteredDemuxer,
  VideoFrame, VideoPacket, View, attachment_packet_from_ffmpeg, audio_packet_from_ffmpeg,
  audio_packet_from_ffmpeg_in, channel_layout_description_from_ffmpeg, channel_layout_from_ffmpeg,
  channel_order_from_ffmpeg, data_packet_from_ffmpeg_in, empty_audio_frame,
  empty_owned_audio_frame, empty_owned_subtitle_frame, empty_owned_video_frame,
  empty_subtitle_frame, empty_video_frame, ffmpeg_packet_from_audio_packet,
  ffmpeg_packet_from_owned_audio_packet, ffmpeg_packet_from_owned_subtitle_packet,
  ffmpeg_packet_from_owned_video_packet, ffmpeg_packet_from_subtitle_packet,
  ffmpeg_packet_from_video_packet, from_av_pixel_format, is_hardware_pix_fmt,
  owned_attachment_packet_from_ffmpeg, owned_audio_packet_from_ffmpeg_in,
  owned_data_packet_from_ffmpeg_in, owned_subtitle_packet_from_ffmpeg_in,
  owned_video_packet_from_ffmpeg_in, subtitle_packet_from_ffmpeg, subtitle_packet_from_ffmpeg_in,
  video_packet_from_ffmpeg, video_packet_from_ffmpeg_in,
};
use mediaio_ffmpeg::{BodyRoute, CarrierOps};
#[cfg(feature = "resample")]
#[cfg_attr(docsrs, doc(cfg(feature = "resample")))]
pub use mediaio_ffmpeg::{
  CarrierResampler, FfmpegOwnedResampler, FfmpegResampler, OutputTooLarge, ResampleError,
  ResampleSpec, SpecEnd, UnsupportedSpecChannelCount,
};

/// The test machinery of `mediaio-ffmpeg`, forwarded — compiled only
/// with the `test-support` feature.
#[cfg(feature = "test-support")]
#[cfg_attr(docsrs, doc(cfg(feature = "test-support")))]
#[doc(hidden)]
pub use mediaio_ffmpeg::test_support;

/// The demuxer, on the **view** lane — the ordinary road.
///
/// Packets carry [`FfmpegBuffer`] views onto libavformat's own
/// allocations: nothing is copied, and a consumer that reads a packet
/// and drops it never pays for bytes it did not keep. This is what a
/// direct consumer wants, which is why it is what the bare name means.
///
/// Reach for [`FfmpegOwnedDemuxer`] when a packet has to **travel** —
/// across a graph, between threads that both read it, into a cache
/// that outlives the decoder. See [the carrier lanes][lanes] for the
/// tradeoff table.
///
/// [lanes]: mediadecode::adapter#the-two-carrier-lanes
pub type FfmpegDemuxer = demuxer::CarrierDemuxer<View>;

/// The demuxer on the **owned** lane: every byte copied once at the
/// boundary into memory Rust owns.
///
/// The same type as [`FfmpegDemuxer`] on the other carrier, with the
/// same constructors — `FfmpegOwnedDemuxer::open(&path)`. Packets are
/// `Send + Sync + 'static` and owe nothing to the session that produced
/// them, which is what a graph needs and what a view cannot give.
pub type FfmpegOwnedDemuxer = demuxer::CarrierDemuxer<Owned>;

/// The audio decoder on the **view** lane.
pub type FfmpegAudioStreamDecoder = audio::CarrierAudioStreamDecoder<View>;
/// The audio decoder on the **owned** lane.
pub type FfmpegOwnedAudioStreamDecoder = audio::CarrierAudioStreamDecoder<Owned>;

/// The subtitle decoder on the **view** lane.
pub type FfmpegSubtitleStreamDecoder = subtitle::CarrierSubtitleStreamDecoder<View>;
/// The subtitle decoder on the **owned** lane.
pub type FfmpegOwnedSubtitleStreamDecoder = subtitle::CarrierSubtitleStreamDecoder<Owned>;

/// The still-image decoder on the **view** lane.
pub type FfmpegImageDecoder = image::CarrierImageDecoder<View>;
/// The still-image decoder on the **owned** lane.
pub type FfmpegOwnedImageDecoder = image::CarrierImageDecoder<Owned>;

/// The video stream decoder on the **view** lane.
pub type FfmpegVideoStreamDecoder = video::CarrierVideoStreamDecoder<View>;
/// The video stream decoder on the **owned** lane.
pub type FfmpegOwnedVideoStreamDecoder = video::CarrierVideoStreamDecoder<Owned>;
pub use audio::{AudioDecodeError, CarrierAudioStreamDecoder};
pub use backend::Backend;
pub use decoder::VideoDecoder;
pub use demuxer::{
  CarrierDemuxer, ChapterAlloc, ChapterTimebaseInvalid, ChapterTitleAlloc,
  ChapterTitleBudgetExhausted, ChapterTitleTooLong, DemuxError, DemuxerOpen, ProbeBudgetExhausted,
  TooManyChapters, TrackMetadataAlloc, TrackMetadataBudgetExhausted, TrackMetadataTooLong,
  TrackTableAlloc, TrackTimebaseInvalid,
};
pub use error::{
  Error, FrameBudgetExceeded, FrameMedium, HwSurfaceTooLarge, HwTransferTooLarge, Result,
};
pub use image::{CarrierImageDecoder, Corrupt, CorruptSource, ImageDecodeError, InputTooLarge};
pub use subtitle::{CarrierSubtitleStreamDecoder, SubtitleDecodeError};
pub use video::{CarrierVideoStreamDecoder, DecodePath, VideoDecodeError};

/// Asserts a submission was taken, and answers nothing.
///
/// The `#[must_use]` on [`mediadecode::Sent`] is deliberate teeth: a
/// test that submits and ignores the answer is a test that would not
/// notice a decoder quietly asking to be drained. Most of this crate's
/// tests submit into a session they have just emptied, where
/// [`Sent::MustDrain`](mediadecode::Sent::MustDrain) is a real
/// surprise — so they say so here rather than dropping it.
#[cfg(test)]
#[track_caller]
pub(crate) fn accepted<E: core::fmt::Debug>(
  status: core::result::Result<mediadecode::Sent, E>,
  what: &str,
) {
  assert_eq!(
    status.unwrap_or_else(|e| panic!("{what}: {e:?}")),
    mediadecode::Sent::Accepted,
    "{what}: the session asked to be drained where the test expected room",
  );
}
