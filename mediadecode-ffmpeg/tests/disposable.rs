//! `PacketFlags::DISPOSABLE` from the bitstream, on real camera files:
//! Sony's XAVC originals carry no `sdtp`, so the container marks nothing,
//! and their H.264 marks two pictures in three `nal_ref_idc` 0.
//!
//! The fixtures are `findit-studio/video-fixtures`' `h264/` clips. Point
//! `VIDEO_FIXTURES` at a checkout of that repository; without it the
//! laws look at the default checkout path and skip, naming the fixture,
//! where it is absent.

use std::{
  collections::BTreeMap,
  hash::{DefaultHasher, Hash, Hasher},
  path::PathBuf,
};

use mediadecode::{
  PixelFormat, Received, Sent, Timebase,
  decoder::VideoStreamDecoder,
  demuxer::{DemuxedPacket, Demuxer, TrackKind},
  packet::PacketFlags,
};
use mediadecode_ffmpeg::{
  DecodePath, DecoderLimits, FfmpegOwnedDemuxer as FfmpegDemuxer,
  FfmpegOwnedVideoStreamDecoder as FfmpegVideoStreamDecoder, OwnedVideoFrame as VideoFrame,
  OwnedVideoPacket as VideoPacket, empty_owned_video_frame,
};

const FIXTURES_ENV: &str = "VIDEO_FIXTURES";
const DEFAULT_FIXTURES: &str = "/Volumes/rollroll/fixtures/video-fixtures";

/// The `h264/` fixture `name`, or `None` — and the skip, named — where it
/// is absent.
fn fixture(name: &str) -> Option<PathBuf> {
  let root =
    std::env::var_os(FIXTURES_ENV).map_or_else(|| PathBuf::from(DEFAULT_FIXTURES), PathBuf::from);
  let path = root.join("h264").join(name);
  if path.is_file() {
    Some(path)
  } else {
    eprintln!(
      "skip: the fixture h264/{name} is absent at {} — set {FIXTURES_ENV} to a checkout of \
       findit-studio/video-fixtures",
      path.display()
    );
    None
  }
}

/// The video track of the file at `path`: its index, timebase and codec
/// parameters, and every packet it delivers, in order.
fn video_packets(path: &PathBuf) -> (Timebase, ffmpeg_next::codec::Parameters, Vec<VideoPacket>) {
  let mut demuxer = FfmpegDemuxer::open(path).expect("open the fixture");
  let index = demuxer
    .tracks()
    .iter()
    .position(|track| track.kind() == TrackKind::Video)
    .expect("a video track");
  let track = &demuxer.tracks()[index];
  let timebase = track.timebase();
  let parameters = track.extra().clone_parameters().expect("parameters");
  let mut packets = Vec::new();
  while let Some(packet) = demuxer.next_packet().expect("read") {
    if let DemuxedPacket::Video(video) = packet
      && video.track().get() == index
    {
      packets.push(video.into_packet());
    }
  }
  (timebase, parameters, packets)
}

fn disposable(packet: &VideoPacket) -> bool {
  packet.flags().contains(PacketFlags::DISPOSABLE)
}

#[test]
fn the_fx3_clip_marks_sixteen_of_its_twenty_four_pictures_disposable() {
  let Some(path) = fixture("12_sony_fx3_xavc.mp4") else {
    return;
  };
  let (_, _, packets) = video_packets(&path);
  assert_eq!(packets.len(), 24);
  let marked = packets.iter().filter(|packet| disposable(packet)).count();
  assert_eq!(marked, 16, "the bitstream's two in three");
  // A keyframe is a reference: never marked.
  for packet in packets
    .iter()
    .filter(|packet| packet.flags().contains(PacketFlags::KEY))
  {
    assert!(!disposable(packet), "a keyframe marked disposable");
  }
}

#[test]
fn the_hlg_clip_marks_one_hundred_and_eighty_of_its_two_hundred_and_seventy_one() {
  let Some(path) = fixture("13_sony_hlg_xavc.mp4") else {
    return;
  };
  let (_, _, packets) = video_packets(&path);
  // Every packet is delivered, the 20 its edit list hides among them,
  // flagged for discard.
  assert_eq!(packets.len(), 271);
  assert_eq!(
    packets
      .iter()
      .filter(|packet| packet.flags().contains(PacketFlags::DISCARD))
      .count(),
    20
  );
  assert_eq!(
    packets.iter().filter(|packet| disposable(packet)).count(),
    180
  );
}

/// A hash of `frame`'s visible samples: each plane's rows, as many bytes
/// of each as a 4:2:2 10-bit picture of the frame's width holds — the
/// fixture's format, checked first — so a decoder's padding is never
/// compared.
fn picture_hash(frame: &VideoFrame) -> u64 {
  assert_eq!(
    *frame.pixel_format(),
    PixelFormat::Yuv422p10Le,
    "the fixture's format"
  );
  let (width, height) = (frame.width() as usize, frame.height() as usize);
  let mut hasher = DefaultHasher::new();
  for (index, plane) in frame.planes().iter().enumerate() {
    // Two bytes a sample; the chroma planes half as wide, as tall.
    let row = if index == 0 { width * 2 } else { width };
    let stride = plane.stride() as usize;
    let data: &[u8] = plane.data_ref().as_ref();
    for line in 0..height {
      data[line * stride..line * stride + row].hash(&mut hasher);
    }
  }
  hasher.finish()
}

/// Every picture the session on `path` delivers for `packets`, by pts,
/// each as `read` makes of it.
fn session_pictures<T>(
  path: DecodePath,
  timebase: Timebase,
  parameters: &ffmpeg_next::codec::Parameters,
  packets: &[&VideoPacket],
  mut read: impl FnMut(&VideoFrame) -> T,
) -> BTreeMap<i64, T> {
  let mut decoder =
    FfmpegVideoStreamDecoder::open_as(parameters.clone(), timebase, DecoderLimits::default(), path)
      .expect("open the session");
  let mut frame = empty_owned_video_frame();
  let mut pictures = BTreeMap::new();
  let mut take = |decoder: &mut FfmpegVideoStreamDecoder, frame: &mut VideoFrame| {
    while let Received::Produced = decoder.receive_frame(frame).expect("decode cleanly") {
      let pts = frame.pts().expect("a picture's pts").pts();
      assert!(
        pictures.insert(pts, read(frame)).is_none(),
        "two pictures at {pts}"
      );
    }
  };
  for packet in packets {
    loop {
      match decoder.send_packet(packet).expect("decode cleanly") {
        Sent::Accepted => break,
        Sent::MustDrain => take(&mut decoder, &mut frame),
      }
    }
    take(&mut decoder, &mut frame);
  }
  while let Sent::MustDrain = decoder.send_eof().expect("decode cleanly") {
    take(&mut decoder, &mut frame);
  }
  take(&mut decoder, &mut frame);
  pictures
}

/// Every picture a software session delivers for `packets`, by pts, each
/// as its [`picture_hash`].
fn decode(
  timebase: Timebase,
  parameters: &ffmpeg_next::codec::Parameters,
  packets: &[&VideoPacket],
) -> BTreeMap<i64, u64> {
  session_pictures(
    DecodePath::Software,
    timebase,
    parameters,
    packets,
    picture_hash,
  )
}

#[test]
fn the_admitted_packets_alone_decode_exactly_the_reference_pictures() {
  let Some(path) = fixture("12_sony_fx3_xavc.mp4") else {
    return;
  };
  let (timebase, parameters, packets) = video_packets(&path);
  let every: Vec<&VideoPacket> = packets.iter().collect();
  let admitted: Vec<&VideoPacket> = packets
    .iter()
    .filter(|packet| !disposable(packet))
    .collect();
  assert_eq!(admitted.len(), 8);
  let whole = decode(timebase, &parameters, &every);
  assert_eq!(whole.len(), 24, "every picture of the full decode");
  let references = decode(timebase, &parameters, &admitted);
  // Exactly the admitted packets' pictures, each the very picture the
  // full decode made.
  let expected: Vec<i64> = {
    let mut pts: Vec<i64> = admitted
      .iter()
      .map(|packet| packet.pts().expect("a packet's pts").pts())
      .collect();
    pts.sort_unstable();
    pts
  };
  assert_eq!(references.keys().copied().collect::<Vec<_>>(), expected);
  for (pts, hash) in &references {
    assert_eq!(
      whole.get(pts),
      Some(hash),
      "the picture at {pts} decoded otherwise"
    );
  }
}
