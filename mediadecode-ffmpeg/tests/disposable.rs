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
  DecodePath, DecoderLimits, DecoderOptions, Discard, FfmpegOwnedDemuxer as FfmpegDemuxer,
  FfmpegOwnedVideoStreamDecoder as FfmpegVideoStreamDecoder, OwnedVideoFrame as VideoFrame,
  OwnedVideoPacket as VideoPacket, VideoStreamDecoderOpen, empty_owned_video_frame,
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

/// Every picture the session opened by `request` delivers for `packets`,
/// by pts, each as `read` makes of it.
fn session_pictures<T>(
  request: VideoStreamDecoderOpen,
  timebase: Timebase,
  parameters: &ffmpeg_next::codec::Parameters,
  packets: &[&VideoPacket],
  mut read: impl FnMut(&VideoFrame) -> T,
) -> BTreeMap<i64, T> {
  let mut decoder = FfmpegVideoStreamDecoder::open_from(
    request,
    parameters.clone(),
    timebase,
    DecoderLimits::default(),
  )
  .expect("open the session");
  assert_eq!(decoder.options(), request.options());
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
/// as its [`picture_hash`]: the session opened with `options`.
fn decode(
  timebase: Timebase,
  parameters: &ffmpeg_next::codec::Parameters,
  packets: &[&VideoPacket],
  options: DecoderOptions,
) -> BTreeMap<i64, u64> {
  session_pictures(
    VideoStreamDecoderOpen::new()
      .with_path(DecodePath::Software)
      .with_options(options),
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
  let whole = decode(timebase, &parameters, &every, DecoderOptions::new());
  assert_eq!(whole.len(), 24, "every picture of the full decode");
  let references = decode(timebase, &parameters, &admitted, DecoderOptions::new());
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

#[test]
fn ffmpegs_non_reference_skip_keeps_the_same_eight_pictures() {
  let Some(path) = fixture("12_sony_fx3_xavc.mp4") else {
    return;
  };
  let (timebase, parameters, packets) = video_packets(&path);
  let every: Vec<&VideoPacket> = packets.iter().collect();
  let admitted: Vec<&VideoPacket> = packets
    .iter()
    .filter(|packet| !disposable(packet))
    .collect();
  let skipping = decode(
    timebase,
    &parameters,
    &every,
    DecoderOptions::new().with_skip_frame(Discard::NonReference),
  );
  let references = decode(timebase, &parameters, &admitted, DecoderOptions::new());
  assert_eq!(skipping, references);
}

/// The pts of every picture the standalone decoder `decoder` delivers for
/// `packets`, in order.
#[cfg(target_vendor = "apple")]
fn delivered(mut decoder: mediadecode_ffmpeg::VideoDecoder, packets: &[VideoPacket]) -> Vec<i64> {
  use mediadecode_ffmpeg::{Frame, VideoDecoder};

  let mut frame = Frame::empty().expect("a frame");
  let mut pictures = Vec::new();
  let mut take = |decoder: &mut VideoDecoder, frame: &mut Frame| {
    while let Received::Produced = decoder.receive_frame(frame).expect("decode cleanly") {
      pictures.push(frame.pts().expect("a picture's pts"));
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
  pictures.sort_unstable();
  pictures
}

/// The presentation times of the packets that hold a slice of a reference
/// picture — `nal_ref_idc` above 0 — sorted.
#[cfg(target_vendor = "apple")]
fn reference_pictures(packets: &[VideoPacket]) -> Vec<i64> {
  let mut pts: Vec<i64> = packets
    .iter()
    .filter(|packet| {
      nal_units(packet.data().as_ref())
        .into_iter()
        .any(|unit| matches!(unit[0] & 0x1F, 1 | 5) && unit[0] & 0x60 != 0)
    })
    .map(|packet| packet.pts().expect("a packet's pts").pts())
    .collect();
  pts.sort_unstable();
  pts
}

/// The standalone decoder takes the options too. Opened with
/// `skip_frame` = `noref` through its open request, on the probe and on a
/// pinned backend, it keeps exactly the reference pictures.
///
/// The standalone decoder is hardware-only, and FFmpeg asks VideoToolbox
/// for a hardware H.264 decoder (`libavcodec/videotoolbox.c` 848–854 in
/// 9.0.1). On the machines this was written on, that decoder fails every
/// picture of the Sony clips. So the clip is `01_pandas_muted.mp4`, Main
/// 4:2:0 with non-reference B pictures, and the target is an Apple one.
///
/// The bitstream marks none of this clip's packets: its sequence parameter
/// set sets no `bitstream_restriction_flag`, so FFmpeg grows the reorder
/// depth from the pictures it sees, and a picture dropped may cost a later
/// one its output.
#[cfg(target_vendor = "apple")]
#[test]
fn the_standalone_decoder_skipping_non_references_keeps_the_reference_pictures() {
  use mediadecode_ffmpeg::{Backend, VideoDecoder, VideoDecoderOpen};

  let Some(path) = fixture("01_pandas_muted.mp4") else {
    return;
  };
  let (timebase, parameters, packets) = video_packets(&path);
  assert_eq!(packets.len(), 300);
  assert_eq!(
    packets.iter().filter(|packet| disposable(packet)).count(),
    0,
    "the reorder depth is not fixed"
  );
  let admitted = reference_pictures(&packets);
  assert_eq!(admitted.len(), 155, "145 of the 300 non-reference");

  // libavcodec's defaults decode every picture: the decoder can decode
  // this clip.
  let whole = delivered(
    VideoDecoder::open_timed(parameters.clone(), timebase).expect("open the decoder"),
    &packets,
  );
  assert_eq!(whole.len(), 300);

  let skipping = VideoDecoderOpen::new()
    .with_timebase(Some(timebase))
    .with_options(DecoderOptions::new().with_skip_frame(Discard::NonReference));
  let probing =
    VideoDecoder::open_from(skipping, parameters.clone()).expect("open the probing decoder");
  assert_eq!(probing.options(), skipping.options());
  assert_eq!(delivered(probing, &packets), admitted, "the probe");
  let pinned = VideoDecoder::open_from(
    skipping.with_backend(Some(Backend::VideoToolbox)),
    parameters,
  )
  .expect("open the pinned decoder");
  assert_eq!(pinned.options(), skipping.options());
  assert_eq!(delivered(pinned, &packets), admitted, "the pinned backend");
}

/// The stream decoder takes the options on every path. Opened through
/// its request with `skip_frame` = `noref`, it keeps exactly the reference
/// pictures, on `Auto`, `AnyHardware`, the VideoToolbox pin, and software.
///
/// The target is an Apple one, on `01_pandas_muted.mp4`, for the reason
/// the standalone law above gives.
#[cfg(target_vendor = "apple")]
#[test]
fn the_session_skipping_non_references_keeps_the_reference_pictures_on_every_path() {
  use mediadecode_ffmpeg::Backend;

  let Some(path) = fixture("01_pandas_muted.mp4") else {
    return;
  };
  let (timebase, parameters, packets) = video_packets(&path);
  let every: Vec<&VideoPacket> = packets.iter().collect();
  let admitted = reference_pictures(&packets);
  assert_eq!(admitted.len(), 155, "145 of the 300 non-reference");
  let skipping = DecoderOptions::new().with_skip_frame(Discard::NonReference);
  for path in [
    DecodePath::Auto,
    DecodePath::AnyHardware,
    DecodePath::Hardware(Backend::VideoToolbox),
    DecodePath::Software,
  ] {
    let pictures = session_pictures(
      VideoStreamDecoderOpen::new()
        .with_path(path)
        .with_options(skipping),
      timebase,
      &parameters,
      &every,
      |_| (),
    );
    assert_eq!(
      pictures.keys().copied().collect::<Vec<_>>(),
      admitted,
      "{path:?}"
    );
  }
}

/// Under `skip_frame` = `all` no picture comes, so no probe could commit
/// on one: every road opens committed and drains cleanly to no picture —
/// the standalone decoder on its probe and on its pin, and the session on
/// `Auto` (software from the open), `AnyHardware`, the VideoToolbox pin and
/// software.
#[cfg(target_vendor = "apple")]
#[test]
fn skipping_every_picture_opens_committed_and_drains_cleanly_on_every_road() {
  use mediadecode_ffmpeg::{Backend, VideoDecoder, VideoDecoderOpen};

  let Some(path) = fixture("01_pandas_muted.mp4") else {
    return;
  };
  let (timebase, parameters, packets) = video_packets(&path);
  let every: Vec<&VideoPacket> = packets.iter().collect();
  let all = DecoderOptions::new().with_skip_frame(Discard::All);

  let request = VideoDecoderOpen::new()
    .with_timebase(Some(timebase))
    .with_options(all);
  let probing = VideoDecoder::open_from(request, parameters.clone()).expect("open on the probe");
  assert_eq!(delivered(probing, &packets), Vec::<i64>::new(), "the probe");
  let pinned = VideoDecoder::open_from(
    request.with_backend(Some(Backend::VideoToolbox)),
    parameters.clone(),
  )
  .expect("open on the pin");
  assert_eq!(delivered(pinned, &packets), Vec::<i64>::new(), "the pin");

  let auto = FfmpegVideoStreamDecoder::open_from(
    VideoStreamDecoderOpen::new().with_options(all),
    parameters.clone(),
    timebase,
    DecoderLimits::default(),
  )
  .expect("open on Auto");
  assert!(
    auto.is_software(),
    "Auto opens software under skip_frame all"
  );
  for path in [
    DecodePath::Auto,
    DecodePath::AnyHardware,
    DecodePath::Hardware(Backend::VideoToolbox),
    DecodePath::Software,
  ] {
    let pictures = session_pictures(
      VideoStreamDecoderOpen::new()
        .with_path(path)
        .with_options(all),
      timebase,
      &parameters,
      &every,
      |_| (),
    );
    assert!(pictures.is_empty(), "{path:?}: {} pictures", pictures.len());
  }
}

/// The NAL units of a packet of four-byte lengths, as the fixture's
/// `avcC` frames them.
#[cfg(target_vendor = "apple")]
fn nal_units(packet: &[u8]) -> Vec<&[u8]> {
  let mut units = Vec::new();
  let mut rest = packet;
  while !rest.is_empty() {
    let (length, tail) = rest.split_first_chunk::<4>().expect("a four-byte length");
    let length = u32::from_be_bytes(*length) as usize;
    assert!(
      length > 0 && length <= tail.len(),
      "a length of {length} in a packet of {}",
      packet.len()
    );
    let (unit, tail) = tail.split_at(length);
    units.push(unit);
    rest = tail;
  }
  units
}

/// A NAL unit's payload, every `03` after two zero bytes dropped.
#[cfg(target_vendor = "apple")]
fn rbsp(payload: &[u8]) -> Vec<u8> {
  let mut out = Vec::with_capacity(payload.len());
  let mut zeros = 0;
  for &byte in payload {
    if zeros >= 2 && byte == 3 {
      zeros = 0;
      continue;
    }
    zeros = if byte == 0 { zeros + 1 } else { 0 };
    out.push(byte);
  }
  out
}

/// An unsigned Exp-Golomb value at `*at`, a bit index into `bits`.
#[cfg(target_vendor = "apple")]
fn ue(bits: &[u8], at: &mut usize) -> u32 {
  let bit = |at: usize| (bits[at / 8] >> (7 - at % 8)) & 1;
  let mut zeros = 0;
  while bit(*at) == 0 {
    zeros += 1;
    *at += 1;
  }
  *at += 1;
  let mut value = 0_u32;
  for _ in 0..zeros {
    value = (value << 1) | u32::from(bit(*at));
    *at += 1;
  }
  (1 << zeros) - 1 + value
}

/// An H.264 slice NAL unit's `slice_type` (7.4.3).
#[cfg(target_vendor = "apple")]
fn slice_type(unit: &[u8]) -> u32 {
  let bits = rbsp(&unit[1..]);
  let mut at = 0;
  let _first_mb_in_slice = ue(&bits, &mut at);
  ue(&bits, &mut at)
}

/// Whether an H.264 slice NAL unit codes an I or SI slice (7.4.3).
#[cfg(target_vendor = "apple")]
fn intra_slice(unit: &[u8]) -> bool {
  matches!(slice_type(unit) % 5, 2 | 4)
}

/// Every slice in the packet of `nal_ref_idc` 0: a picture `noref` skips.
#[cfg(target_vendor = "apple")]
fn non_reference_alone(packet: &[u8]) -> bool {
  nal_units(packet)
    .into_iter()
    .all(|unit| !matches!(unit[0] & 0x1F, 1..=5) || unit[0] & 0x60 == 0)
}

/// Every slice in the packet a B slice: a picture `bidir` skips.
#[cfg(target_vendor = "apple")]
fn b_pictures_alone(packet: &[u8]) -> bool {
  nal_units(packet)
    .into_iter()
    .all(|unit| !matches!(unit[0] & 0x1F, 1 | 5) || slice_type(unit) % 5 == 1)
}

/// Whether an H.264 SEI NAL unit holds a recovery point (`payloadType` 6).
#[cfg(target_vendor = "apple")]
fn recovery_point(unit: &[u8]) -> bool {
  let bytes = rbsp(&unit[1..]);
  let coded = |at: &mut usize| {
    let mut value = 0_usize;
    while bytes[*at] == 0xFF {
      value += 255;
      *at += 1;
    }
    value += usize::from(bytes[*at]);
    *at += 1;
    value
  };
  // Every message, up to the trailing `0x80`.
  let mut at = 0;
  while at < bytes.len() && bytes[at] != 0x80 {
    let payload_type = coded(&mut at);
    let size = coded(&mut at);
    if payload_type == 6 {
      return true;
    }
    at += size;
  }
  false
}

/// No IDR slice and no recovery point SEI in the packet.
#[cfg(target_vendor = "apple")]
fn no_key_picture(packet: &[u8]) -> bool {
  nal_units(packet)
    .into_iter()
    .all(|unit| match unit[0] & 0x1F {
      5 => false,
      6 => !recovery_point(unit),
      _ => true,
    })
}

/// No I or SI slice in the packet.
#[cfg(target_vendor = "apple")]
fn no_intra_picture(packet: &[u8]) -> bool {
  nal_units(packet)
    .into_iter()
    .all(|unit| !matches!(unit[0] & 0x1F, 1 | 5) || !intra_slice(unit))
}

/// How much of the filtered fixture a law feeds.
#[cfg(target_vendor = "apple")]
#[derive(Clone, Copy, Debug)]
enum Feed {
  /// 120 packets at most: the probe is still on trial at the clean end.
  WithinTheProbe,
  /// Past the 256 packets the probe buffers — every packet kept, and a
  /// fragment shorter than that again, as often as it takes: the probe fills
  /// with no picture.
  PastTheProbe,
}

/// Under a skip policy that may leave a stream with no picture — every level
/// above `Default` — a clean end with none is the stream's end on every
/// unpinned road, and so is a probe that fills with none: the standalone
/// probe and `AnyHardware` commit the backend that opened, and `Auto`, which
/// probes hardware while pictures may come, ends on software. Under `All` no
/// probe runs at all, and `Auto` opens software.
#[cfg(target_vendor = "apple")]
fn a_stream_with_no_picture_to_give_ends_cleanly(
  skip: Discard,
  keep: fn(&[u8]) -> bool,
  feed: Feed,
) {
  use mediadecode_ffmpeg::{VideoDecoder, VideoDecoderOpen};

  let Some(path) = fixture("01_pandas_muted.mp4") else {
    return;
  };
  let (timebase, parameters, packets) = video_packets(&path);
  let mut kept: Vec<VideoPacket> = packets
    .into_iter()
    .filter(|packet| keep(packet.data().as_ref()))
    .collect();
  assert!(!kept.is_empty(), "{skip}: no packet kept");
  match feed {
    Feed::WithinTheProbe => kept.truncate(120),
    // The level skips every picture of the fragment however often it comes.
    Feed::PastTheProbe => {
      let fragment = kept.clone();
      while kept.len() <= 256 {
        kept.extend(fragment.iter().cloned());
      }
    }
  }
  let options = DecoderOptions::new().with_skip_frame(skip);

  let request = VideoDecoderOpen::new()
    .with_timebase(Some(timebase))
    .with_options(options);
  let probing = VideoDecoder::open_from(request, parameters.clone()).expect("open on the probe");
  assert_eq!(
    delivered(probing, &kept),
    Vec::<i64>::new(),
    "{skip}, {feed:?}: the probe"
  );

  let every: Vec<&VideoPacket> = kept.iter().collect();
  let pictures = session_pictures(
    VideoStreamDecoderOpen::new()
      .with_path(DecodePath::AnyHardware)
      .with_options(options),
    timebase,
    &parameters,
    &every,
    |_| (),
  );
  assert!(pictures.is_empty(), "{skip}, {feed:?}: AnyHardware");

  let mut auto = FfmpegVideoStreamDecoder::open_from(
    VideoStreamDecoderOpen::new().with_options(options),
    parameters.clone(),
    timebase,
    DecoderLimits::default(),
  )
  .expect("open on Auto");
  assert_eq!(
    auto.is_software(),
    skip == Discard::All,
    "{skip}, {feed:?}: Auto probes hardware while pictures may come, and opens software where \
     none can"
  );
  let mut frame = empty_owned_video_frame();
  let mut shown = 0;
  let mut take = |auto: &mut FfmpegVideoStreamDecoder, frame: &mut VideoFrame| {
    while let Received::Produced = auto.receive_frame(frame).expect("decode cleanly") {
      shown += 1;
    }
  };
  for packet in &kept {
    loop {
      match auto.send_packet(packet).expect("decode cleanly") {
        Sent::Accepted => break,
        Sent::MustDrain => take(&mut auto, &mut frame),
      }
    }
    take(&mut auto, &mut frame);
  }
  while let Sent::MustDrain = auto.send_eof().expect("decode cleanly") {
    take(&mut auto, &mut frame);
  }
  take(&mut auto, &mut frame);
  assert_eq!(shown, 0, "{skip}, {feed:?}: Auto");
  assert!(
    auto.is_software(),
    "{skip}, {feed:?}: Auto ends on software"
  );
}

/// `skip_frame = nonkey` over an H.264 stream with no IDR and no recovery
/// point SEI (the fixture without its key packets), ending while the probe
/// is on trial.
#[cfg(target_vendor = "apple")]
#[test]
fn a_stream_with_no_key_picture_ends_cleanly_under_nonkey() {
  a_stream_with_no_picture_to_give_ends_cleanly(
    Discard::NonKey,
    no_key_picture,
    Feed::WithinTheProbe,
  );
}

/// `skip_frame = nonintra` over an H.264 stream with no I slice (the fixture
/// without every packet that holds one), ending while the probe is on trial.
#[cfg(target_vendor = "apple")]
#[test]
fn a_stream_with_no_intra_picture_ends_cleanly_under_nonintra() {
  a_stream_with_no_picture_to_give_ends_cleanly(
    Discard::NonIntra,
    no_intra_picture,
    Feed::WithinTheProbe,
  );
}

/// The same streams whole, longer than the probe buffers: a probe that fills
/// with no picture under these policies commits too.
#[cfg(target_vendor = "apple")]
#[test]
fn a_probe_that_fills_with_no_picture_commits_under_nonkey_and_nonintra() {
  a_stream_with_no_picture_to_give_ends_cleanly(
    Discard::NonKey,
    no_key_picture,
    Feed::PastTheProbe,
  );
  a_stream_with_no_picture_to_give_ends_cleanly(
    Discard::NonIntra,
    no_intra_picture,
    Feed::PastTheProbe,
  );
}

/// `skip_frame = noref` over an H.264 fragment of non-reference pictures
/// alone (the fixture's 145 B pictures), every one of which FFmpeg skips
/// (`h264dec.c` 626–628): a clean end within the probe, and a probe that
/// fills, with no picture.
#[cfg(target_vendor = "apple")]
#[test]
fn a_stream_of_non_reference_pictures_alone_ends_cleanly_under_nonref() {
  for feed in [Feed::WithinTheProbe, Feed::PastTheProbe] {
    a_stream_with_no_picture_to_give_ends_cleanly(Discard::NonReference, non_reference_alone, feed);
  }
}

/// `skip_frame = bidir` over an H.264 fragment of B pictures alone, which
/// FFmpeg skips before it starts any (`h264_slice.c` 2139–2147; the
/// fixture's B pictures are non-reference too, `h264dec.c` 626–628): a clean
/// end within the probe, and a probe that fills, with no picture.
#[cfg(target_vendor = "apple")]
#[test]
fn a_stream_of_b_pictures_alone_ends_cleanly_under_bidir() {
  for feed in [Feed::WithinTheProbe, Feed::PastTheProbe] {
    a_stream_with_no_picture_to_give_ends_cleanly(Discard::Bidirectional, b_pictures_alone, feed);
  }
}

/// `skip_frame = all` over the whole fixture, within and past the probe's
/// size: no probe runs, and every road drains cleanly to no picture.
#[cfg(target_vendor = "apple")]
#[test]
fn every_picture_skipped_ends_cleanly_under_all_within_and_past_the_probe_size() {
  for feed in [Feed::WithinTheProbe, Feed::PastTheProbe] {
    a_stream_with_no_picture_to_give_ends_cleanly(Discard::All, |_| true, feed);
  }
}
