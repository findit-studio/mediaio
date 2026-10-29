//! The HDR colour/metadata faces, proved through a real decode.
//!
//! `convert::tests` (in the library's own `src/`) proves the byte-
//! level parsers against hand-built payloads matching FFmpeg's structs
//! exactly. This file closes the other half: that a *real* HEVC decode
//! — `libx265`'s own HDR10 bitstream writer, not an approximation —
//! actually reaches `VideoFrame::color()` and `VideoFrameExtra::
//! mastering_display()` / `content_light_level()` with the numbers a
//! downstream linear-light consumer would read.
//!
//! Two fixtures, [`support::Corpus::hdr10_hevc`] and
//! [`support::Corpus::hlg_hevc`], carry two *different* real transfer
//! characteristics (PQ and HLG) so this is not "the same clip twice" —
//! and only the first carries static HDR metadata, so the second
//! proves the "absent metadata answers absent" half of the same face
//! rather than merely asserting it in isolation.

mod support;

use mediadecode::{
  Received, Sent, Timebase,
  color::{ColorPrimaries, ColorTransfer},
  decoder::VideoStreamDecoder,
};
use mediadecode_ffmpeg::{
  DecoderLimits, OwnedVideoFrame, PacketLimits, empty_owned_video_frame, extras::ContentLightLevel,
  owned_video_packet_from_ffmpeg_in,
};

/// Demuxes `path` with a fresh FFmpeg input, opens the auto-probed
/// video decoder on its first video stream, and returns the first
/// decoded [`OwnedVideoFrame`].
///
/// # Panics
/// If the file has no video stream, opening fails, or no frame comes
/// back within the first 64 packets — any of which means the fixture
/// itself is broken, not that the field under test refused correctly.
fn first_video_frame(path: &std::path::Path) -> OwnedVideoFrame {
  let mut input = ffmpeg_next::format::input(path).expect("open the minted clip");
  let stream = input
    .streams()
    .best(ffmpeg_next::media::Type::Video)
    .expect("the fixture has a video stream");
  let stream_index = stream.index();
  let tb = stream.time_base();
  let time_base = Timebase::new(
    tb.numerator(),
    core::num::NonZeroI32::new(tb.denominator().max(1)).expect("a non-zero denominator"),
  );

  let mut decoder = mediadecode_ffmpeg::FfmpegOwnedVideoStreamDecoder::open(
    stream.parameters(),
    time_base,
    DecoderLimits::default(),
  )
  .expect("open the video decoder");

  let mut frame: OwnedVideoFrame = empty_owned_video_frame();
  for (index, (s, av_packet)) in input.packets().enumerate() {
    if s.index() != stream_index {
      continue;
    }
    let Some(packet) =
      // SAFETY: `packets()` reads each packet with `av_read_frame`, so libavformat wrote its
      // payload, and it is live for the call.
      unsafe { owned_video_packet_from_ffmpeg_in(&av_packet, time_base, PacketLimits::default()) }
        .expect("a wrappable video payload")
    else {
      continue;
    };
    if decoder.send_packet(&packet).expect("send packet") != Sent::Accepted {
      continue;
    }
    if let Received::Produced = decoder.receive_frame(&mut frame).expect("receive frame") {
      return frame;
    }
    assert!(index < 64, "no frame within the first 64 packets");
  }
  panic!("the input ended before a frame was produced");
}

/// PQ transfer, BT.2020 primaries, and both static HDR fields populated
/// with the exact numbers `libx265` was asked to write — the same
/// numbers `convert::tests::parse_mastering_display_reads_a_real_
/// hdr10_payload` pins as a unit fixture, now reached through an
/// actual demux + decode.
#[test]
fn hdr10_hevc_decodes_pq_bt2020_and_static_hdr_metadata() {
  let Some(corpus) = support::Corpus::new() else {
    return;
  };
  let path = corpus.hdr10_hevc();
  let frame = first_video_frame(&path);

  let color = frame.color();
  assert_eq!(color.transfer(), ColorTransfer::SmpteSt2084Pq);
  assert_eq!(color.primaries(), ColorPrimaries::Bt2020);

  let mastering_display = frame
    .extra()
    .mastering_display()
    .expect("libx265 wrote a mastering-display SEI");
  assert_eq!(
    mastering_display.display_primaries(),
    [(34_000, 16_000), (13_250, 34_500), (7_500, 3_000)]
  );
  assert_eq!(mastering_display.white_point(), (15_635, 16_450));
  assert_eq!(mastering_display.max_luminance(), (10_000_000, 10_000));
  assert_eq!(mastering_display.min_luminance(), (1, 10_000));

  assert_eq!(
    frame.extra().content_light_level(),
    Some(ContentLightLevel::new(1000, 400)),
  );
}

/// A real, different transfer characteristic (HLG, not PQ) with no
/// static HDR side data at all — both HDR10-only fields read `None`,
/// not a leftover default from some other clip.
#[test]
fn hlg_hevc_decodes_hlg_transfer_with_absent_static_hdr_metadata() {
  let Some(corpus) = support::Corpus::new() else {
    return;
  };
  let path = corpus.hlg_hevc();
  let frame = first_video_frame(&path);

  assert_eq!(frame.color().transfer(), ColorTransfer::AribStdB67Hlg);
  assert_eq!(frame.color().primaries(), ColorPrimaries::Bt2020);
  assert!(
    frame.extra().mastering_display().is_none(),
    "the HLG fixture carries no mastering-display SEI"
  );
  assert!(
    frame.extra().content_light_level().is_none(),
    "the HLG fixture carries no content-light-level SEI"
  );
}
