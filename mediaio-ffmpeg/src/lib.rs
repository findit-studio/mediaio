#![doc = include_str!("../README.md")]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![cfg_attr(docsrs, allow(unused_attributes))]
#![deny(missing_docs)]
// A signature here reads `VideoPacket<VideoPacketExtra, FfmpegBytes>`
// because every one of those names is load-bearing — the household, the
// backend's extras, and the carrier. `clippy::type_complexity` counts
// nesting, and the alias it asks for would hide exactly the word that
// says which carrier a value is on.
#![allow(clippy::type_complexity)]

mod adapter;
pub mod boundary;
// The crossing's plumbing: public because the machines in
// `mediadecode-ffmpeg` drive it, hidden because nothing else should.
// Every operation here whose soundness rests on its caller is an
// `unsafe fn` with a `# Safety` section.
#[doc(hidden)]
pub mod buffer;
mod carrier;
pub mod channel_layout;
mod codec_id;
mod container;
pub mod contracts;
pub mod convert;
pub mod error;
pub mod extras;
#[cfg(any(test, feature = "test-support"))]
mod fault_subprocess;
#[doc(hidden)]
pub mod ffi;
#[doc(hidden)]
pub mod footprint;
#[doc(hidden)]
pub mod frame;
pub mod limits;
#[doc(hidden)]
pub mod measured;
mod pixdesc;
#[doc(hidden)]
pub mod reader_guard;
#[cfg(feature = "resample")]
mod resampler;
mod sample_format;
#[doc(hidden)]
pub mod text;
pub mod ticket;
#[doc(hidden)]
pub mod view;

pub use adapter::Ffmpeg;
pub use boundary::{
  MediaKind, PacketBuildError, SendPayloadTooLarge, SendSideDataTooLarge,
  attachment_packet_from_ffmpeg, audio_packet_from_ffmpeg, audio_packet_from_ffmpeg_in,
  data_packet_from_ffmpeg_in, empty_audio_frame, empty_owned_audio_frame,
  empty_owned_subtitle_frame, empty_owned_video_frame, empty_subtitle_frame, empty_video_frame,
  ffmpeg_packet_from_audio_packet, ffmpeg_packet_from_owned_audio_packet,
  ffmpeg_packet_from_owned_subtitle_packet, ffmpeg_packet_from_owned_video_packet,
  ffmpeg_packet_from_subtitle_packet, ffmpeg_packet_from_video_packet, from_av_pixel_format,
  is_hardware_pix_fmt, owned_attachment_packet_from_ffmpeg, owned_audio_packet_from_ffmpeg_in,
  owned_data_packet_from_ffmpeg_in, owned_subtitle_packet_from_ffmpeg_in,
  owned_video_packet_from_ffmpeg_in, subtitle_packet_from_ffmpeg, subtitle_packet_from_ffmpeg_in,
  try_clone_packet, video_packet_from_ffmpeg, video_packet_from_ffmpeg_in,
};

pub use buffer::{FfmpegBytes, PacketBufferError, TrustedPayload};
// The lane operations: public because the machines in
// `mediadecode-ffmpeg` bound on them, hidden because a consumer has no
// use for them. Every operation whose invariants live in its caller is
// an `unsafe fn`; the safe ones check what they are given.
#[doc(hidden)]
pub use carrier::{BodyRoute, CarrierOps};
pub use carrier::{FfmpegCarrier, Owned, View};
pub use channel_layout::{
  ChannelLayoutFault, channel_layout_description_from_ffmpeg, channel_layout_from_ffmpeg,
  channel_order_from_ffmpeg,
};
pub use codec_id::CodecId;
pub use container::ContainerFormat;
pub use error::{
  CrossingError, ParametersAlloc, ParametersChannelMap, ParametersCopy, ParametersError,
  ParametersLayoutShape, ParametersMissing, ParametersOpaque, ParametersTooLarge,
};
pub use frame::Frame;
pub use limits::{
  DEFAULT_MAX_ATTACHMENT_BYTES, DEFAULT_MAX_CHAPTERS, DEFAULT_MAX_CODEC_PARAMETER_BYTES,
  DEFAULT_MAX_FRAME_BYTES, DEFAULT_MAX_IMAGE_INPUT_BYTES, DEFAULT_MAX_IMAGE_SIDE_DATA_BYTES,
  DEFAULT_MAX_PACKET_BYTES, DEFAULT_MAX_PIXELS, DEFAULT_MAX_PROBE_BYTES, DEFAULT_MAX_STREAMS,
  DEFAULT_MAX_TOTAL_ATTACHMENT_BYTES, DEFAULT_MAX_TOTAL_CHAPTER_TITLE_BYTES,
  DEFAULT_MAX_TOTAL_CODEC_PARAMETER_BYTES, DEFAULT_MAX_TOTAL_STREAM_METADATA_BYTES, DecoderLimits,
  DemuxLimits, FrameLimits, PacketLimits,
};
#[cfg(feature = "resample")]
#[cfg_attr(docsrs, doc(cfg(feature = "resample")))]
pub use resampler::{
  CarrierResampler, OutputTooLarge, ResampleError, ResampleSpec, SpecEnd,
  UnsupportedChannelCount as UnsupportedSpecChannelCount,
};
pub use sample_format::SampleFormat;
pub use ticket::{ChannelLayoutTicket, CodecTicket, CustomChannel, DolbyVisionConfig, Ratio};
pub use view::FfmpegBuffer;

/// The resampler, on the **view** lane — planes shared out of its own
/// output frame.
///
/// Its output frames are the resampler's, not a decoder's: it allocates
/// one per conversion and never writes into a buffer it has handed out,
/// so the pool-hostage warning that applies to decoder frames does not
/// apply here. What does apply is `!Sync` and the amputation contract —
/// use [`FfmpegOwnedResampler`] for a frame that has to travel.
#[cfg(feature = "resample")]
#[cfg_attr(docsrs, doc(cfg(feature = "resample")))]
pub type FfmpegResampler = resampler::CarrierResampler<View>;

/// The resampler, on the **owned** lane — every produced plane copied
/// out of the output frame.
#[cfg(feature = "resample")]
#[cfg_attr(docsrs, doc(cfg(feature = "resample")))]
pub type FfmpegOwnedResampler = resampler::CarrierResampler<Owned>;

/// Test machinery — compiled only with the `test-support` feature.
///
/// Nothing here decodes anything. It is the harness this crate's own
/// lanes drive their hardest roads with, exported so that a crate built
/// on this one can drive the same roads in its lanes: a child process
/// for faults that cannot be injected beside other tests, the global
/// FFmpeg allocation cap such a child may set, and the hooks that latch
/// the callback state's refusals exactly as the `get_format` and
/// `get_buffer2` callbacks do.
///
/// The cap and the hooks are `unsafe fn`: one breaks every unchecked
/// allocation in the process, the other writes through a raw pointer.
/// Their `# Safety` sections say what a caller proves.
#[cfg(feature = "test-support")]
#[cfg_attr(docsrs, doc(cfg(feature = "test-support")))]
#[doc(hidden)]
pub mod test_support {
  pub use crate::{
    fault_subprocess::{cap_ffmpeg_allocations, in_subprocess, uncap_ffmpeg_allocations},
    ffi::{
      CallbackState, declare_ceiling_declined_for_test, declare_frame_budget_declined_for_test,
    },
  };
}

// Every bare alias below binds [`FfmpegBuffer`] as the `D` carrier — the
// view lane, the ordinary road: a decoder's output read where it lands
// and dropped. The `Owned*` family binds [`FfmpegBytes`] and is what a
// payload takes when it has to **travel** — outlive the decoder, cross
// into a graph, be shared across threads.
//
// Each spells its carrier out rather than hiding it behind a neutral
// name, so whether holding a value holds libavcodec's memory open is
// answerable at the use site.

/// Compressed video packet pre-parameterized with this crate's extras
/// and view carrier — the type `mediadecode-ffmpeg`'s
/// `FfmpegVideoStreamDecoder` consumes via
/// `VideoStreamDecoder::send_packet` (the read face in `mediadecode`).
pub type VideoPacket = mediaspine::packet::VideoPacket<extras::VideoPacketExtra, FfmpegBuffer>;

/// Compressed audio packet pre-parameterized with this crate's extras
/// and view carrier.
pub type AudioPacket = mediaspine::packet::AudioPacket<extras::AudioPacketExtra, FfmpegBuffer>;

/// Compressed subtitle packet pre-parameterized with this crate's
/// extras and view carrier.
pub type SubtitlePacket =
  mediaspine::packet::SubtitlePacket<extras::SubtitlePacketExtra, FfmpegBuffer>;

/// Decoded video frame pre-parameterized with this crate's pixel
/// format / extras / view carrier.
///
/// Its planes are windows into the decoder's own frame buffer wherever
/// the geometry proves they may be — see the frame row of the lane
/// table in [`mediaspine::adapter`]. **A frame held is a pool slot
/// held**: on a hardware or fixed-pool decoder, retaining these past
/// the next `receive_frame` starves the decoder. Use [`OwnedVideoFrame`]
/// when a frame has to outlive the decode loop.
pub type VideoFrame =
  mediaspine::frame::VideoFrame<mediaspine::PixelFormat, extras::VideoFrameExtra, FfmpegBuffer>;

/// Decoded audio frame pre-parameterized with this crate's sample
/// format / channel layout / extras / view carrier.
///
/// Each plane is a window over exactly the samples the decoder wrote —
/// never the allocator's alignment padding past them.
pub type AudioFrame = mediaspine::frame::AudioFrame<
  SampleFormat,
  mediaframe::audio::ChannelLayoutDescription,
  extras::AudioFrameExtra,
  FfmpegBuffer,
>;

/// Decoded subtitle frame pre-parameterized with this crate's
/// extras / view carrier.
pub type SubtitleFrame = mediaspine::frame::SubtitleFrame<extras::SubtitleFrameExtra, FfmpegBuffer>;

/// Decoded still image pre-parameterized with this crate's pixel
/// format / extras / view carrier — what `mediadecode-ffmpeg`'s
/// `FfmpegImageDecoder` produces.
pub type ImageFrame =
  mediaspine::frame::ImageFrame<mediaspine::PixelFormat, extras::ImageFrameExtra, FfmpegBuffer>;

/// Timed opaque-data packet pre-parameterized with this crate's extras
/// and view carrier.
pub type DataPacket = mediaspine::demuxer::DataPacket<extras::DataPacketExtra, FfmpegBuffer>;

/// Attachment payload pre-parameterized with this crate's extras and
/// view carrier — a font, or the cover art `mediadecode-ffmpeg`'s
/// `FfmpegImageDecoder` decodes.
pub type AttachmentPacket =
  mediaspine::demuxer::AttachmentPacket<extras::AttachmentPacketExtra, FfmpegBuffer>;

/// The five-arm demux envelope `mediadecode-ffmpeg`'s `FfmpegDemuxer`
/// delivers.
pub type DemuxedPacket = mediaspine::demuxer::DemuxedPacket<Ffmpeg, FfmpegBuffer>;

// --- The owned lane's alias family -----------------------------------
//
// The same shapes on the copying carrier, named explicitly because the
// bare names mean the view lane — the ordinary road for a direct
// consumer. Reach for these when a packet has to travel.
//
// Note what is **not** doubled: the `*Extra` types and `SideDataEntry`
// stay monomorphic on both lanes. Side data has no `AVBufferRef` to
// share — `AVPacketSideData` and `AVFrameSideData` payloads are plain
// allocations — so both lanes copy it, and a second family of extras
// would have been two names for one representation. The carrier
// parameter reaches the **payload**, not the annotations.

/// [`VideoPacket`] on the owned lane.
pub type OwnedVideoPacket = mediaspine::packet::VideoPacket<extras::VideoPacketExtra, FfmpegBytes>;

/// [`AudioPacket`] on the owned lane.
pub type OwnedAudioPacket = mediaspine::packet::AudioPacket<extras::AudioPacketExtra, FfmpegBytes>;

/// [`SubtitlePacket`] on the owned lane.
pub type OwnedSubtitlePacket =
  mediaspine::packet::SubtitlePacket<extras::SubtitlePacketExtra, FfmpegBytes>;

/// [`DataPacket`] on the owned lane.
pub type OwnedDataPacket = mediaspine::demuxer::DataPacket<extras::DataPacketExtra, FfmpegBytes>;

/// [`AttachmentPacket`] on the owned lane.
pub type OwnedAttachmentPacket =
  mediaspine::demuxer::AttachmentPacket<extras::AttachmentPacketExtra, FfmpegBytes>;

/// [`DemuxedPacket`] on the owned lane.
pub type OwnedDemuxedPacket = mediaspine::demuxer::DemuxedPacket<Ffmpeg, FfmpegBytes>;

/// [`VideoFrame`] on the owned lane — planes copied out of the
/// decoder's buffer, so the frame outlives it and the pool slot goes
/// straight back.
pub type OwnedVideoFrame =
  mediaspine::frame::VideoFrame<mediaspine::PixelFormat, extras::VideoFrameExtra, FfmpegBytes>;

/// [`AudioFrame`] on the owned lane.
pub type OwnedAudioFrame = mediaspine::frame::AudioFrame<
  SampleFormat,
  mediaframe::audio::ChannelLayoutDescription,
  extras::AudioFrameExtra,
  FfmpegBytes,
>;

/// [`SubtitleFrame`] on the owned lane.
pub type OwnedSubtitleFrame =
  mediaspine::frame::SubtitleFrame<extras::SubtitleFrameExtra, FfmpegBytes>;

/// [`ImageFrame`] on the owned lane — what `mediadecode-ffmpeg`'s
/// `FfmpegOwnedImageDecoder` produces.
pub type OwnedImageFrame =
  mediaspine::frame::ImageFrame<mediaspine::PixelFormat, extras::ImageFrameExtra, FfmpegBytes>;

/// One row of the track table a demuxer's `tracks` returns.
pub type TrackInfo = mediaspine::demuxer::TrackInfo<Ffmpeg>;

/// A track's per-kind codec parameters, as [`TrackInfo`] carries them.
pub type TrackParams = mediaspine::demuxer::TrackParams<Ffmpeg>;

/// One row of the chapter table a demuxer's `chapters` returns.
pub type Chapter = mediaspine::demuxer::Chapter<Ffmpeg>;

/// Asserts a submission was taken, and answers nothing.
///
/// The `#[must_use]` on [`mediaspine::Sent`] is deliberate teeth: a
/// test that submits and ignores the answer is a test that would not
/// notice a session quietly asking to be drained.
///
/// The resampler's tests are its only users here, so it is compiled
/// where they are.
#[cfg(all(test, feature = "resample"))]
#[track_caller]
pub(crate) fn accepted<E: core::fmt::Debug>(
  status: core::result::Result<mediaspine::Sent, E>,
  what: &str,
) {
  assert_eq!(
    status.unwrap_or_else(|e| panic!("{what}: {e:?}")),
    mediaspine::Sent::Accepted,
    "{what}: the session asked to be drained where the test expected room",
  );
}
