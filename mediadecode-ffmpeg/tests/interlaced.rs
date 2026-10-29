//! `PacketFlags::DISPOSABLE` from the bitstream on interlaced H.264: a
//! field pair whose fields ride in one packet marked as one picture, a pair
//! split across packets kept whole, MBAFF and PicAFF frames marked as
//! progressive ones — and on every fixture, dropping the marked packets
//! changes no other picture's pixels.
//!
//! The fixtures are made at run time. libx264 writes MBAFF frames (`-flags
//! +ildct`) and frames under a sequence parameter set that lets fields in
//! (`fake-interlaced`), through the `ffmpeg` CLI, as MP4 and as a raw
//! stream. No encoder FFmpeg drives writes field pictures, so the PAFF
//! stream is written here, field by field, of I_PCM and P_Skip
//! macroblocks, which FFmpeg decodes exactly; FFmpeg's demuxer and muxer
//! package it: the raw stream, which FFmpeg's parser splits one field to a
//! packet; an MP4 the CLI copies it into, one field to a sample; and an MP4
//! FFmpeg's muxer writes with each pair in one sample. Without the CLI the
//! laws skip, saying why.

use std::{
  collections::BTreeSet,
  hash::{DefaultHasher, Hash, Hasher},
  path::{Path, PathBuf},
  process::Command,
};

use mediadecode::{
  PixelFormat, Received, Sent, Timebase, Timestamp,
  decoder::VideoStreamDecoder,
  demuxer::{DemuxedPacket, Demuxer, TrackKind},
  packet::PacketFlags,
};
use mediadecode_ffmpeg::{
  DecodePath, DecoderLimits, DemuxerName, DemuxerOpen, FfmpegOwnedDemuxer as FfmpegDemuxer,
  FfmpegOwnedVideoStreamDecoder as FfmpegVideoStreamDecoder, OwnedVideoFrame as VideoFrame,
  OwnedVideoPacket as VideoPacket, VideoStreamDecoderOpen, empty_owned_video_frame,
};

/// A directory for the fixtures, and the `ffmpeg` CLI to make them; `None`,
/// and the skip, named, where the CLI is absent.
fn corpus() -> Option<tempfile::TempDir> {
  let available = Command::new("ffmpeg")
    .arg("-version")
    .output()
    .is_ok_and(|output| output.status.success());
  if !available {
    eprintln!(
      "skip: the `ffmpeg` CLI is not on PATH — these laws make their interlaced fixtures with it"
    );
    return None;
  }
  ffmpeg_next::init().expect("ffmpeg init");
  Some(tempfile::tempdir().expect("a temp dir"))
}

fn ffmpeg(args: &[&str]) {
  let output = Command::new("ffmpeg")
    .args(["-hide_banner", "-loglevel", "error", "-y"])
    .args(args)
    .output()
    .expect("run ffmpeg");
  assert!(
    output.status.success(),
    "ffmpeg {args:?}: {}",
    String::from_utf8_lossy(&output.stderr)
  );
}

/// libx264's interlaced output of two seconds of `testsrc2`, at `path` (MP4,
/// or a raw stream for `.264`): two non-reference B pictures between
/// references, every picture a frame.
fn x264(path: &Path, interlaced: &[&str], params: &str) {
  let format = if path.extension().is_some_and(|extension| extension == "264") {
    "h264"
  } else {
    "mp4"
  };
  let params = format!("keyint=25:b-pyramid=none:{params}");
  let mut args = vec![
    "-f",
    "lavfi",
    "-i",
    "testsrc2=size=320x240:rate=25:duration=2",
    "-c:v",
    "libx264",
    "-preset",
    "ultrafast",
    "-bf",
    "2",
  ];
  args.extend_from_slice(interlaced);
  args.extend_from_slice(&["-x264-params", &params, "-pix_fmt", "yuv420p", "-f", format]);
  args.push(path.to_str().expect("a UTF-8 path"));
  ffmpeg(&args);
}

/// The open request an MP4 is read with: the `mov` demuxer's inputs
/// declared plaintext.
fn declared() -> DemuxerOpen {
  DemuxerOpen::new().with_plaintext_demuxers(vec![
    DemuxerName::registered("mov,mp4,m4a,3gp,3g2,mj2").expect("the ISOBMFF demuxer"),
  ])
}

/// A fixture's video track, read by `mediadecode-ffmpeg`'s demuxer: its
/// timebase and codec parameters, and every packet, in order. A packet with
/// no presentation time — a raw stream's — is given its index for one, so
/// that each picture decoded is known by its packet's.
struct Track {
  timebase: Timebase,
  parameters: ffmpeg_next::codec::Parameters,
  packets: Vec<VideoPacket>,
}

fn track(path: &Path) -> Track {
  let request = if path.extension().is_some_and(|extension| extension == "mp4") {
    declared()
  } else {
    DemuxerOpen::new()
  };
  let mut demuxer = FfmpegDemuxer::open_from(path, request).expect("open the fixture");
  let index = demuxer
    .tracks()
    .iter()
    .position(|track| track.kind() == TrackKind::Video)
    .expect("a video track");
  let timebase = demuxer.tracks()[index].timebase();
  let parameters = demuxer.tracks()[index]
    .extra()
    .clone_parameters()
    .expect("parameters");
  let mut packets = Vec::new();
  while let Some(packet) = demuxer.next_packet().expect("read") {
    if let DemuxedPacket::Video(video) = packet
      && video.track().get() == index
    {
      let packet = video.into_packet();
      let packet = if packet.pts().is_none() {
        let at = Some(Timestamp::new(packets.len() as i64, timebase));
        packet.with_pts(at).with_dts(at)
      } else {
        packet
      };
      packets.push(packet);
    }
  }
  Track {
    timebase,
    parameters,
    packets,
  }
}

fn disposable(packet: &VideoPacket) -> bool {
  packet.flags().contains(PacketFlags::DISPOSABLE)
}

/// The NAL units of `data`: after each start code in a raw stream
/// (`annex_b`), else after four-byte lengths, as an `avcC` of four-byte
/// lengths frames them.
fn nal_units(data: &[u8], annex_b: bool) -> Vec<&[u8]> {
  if annex_b {
    let starts: Vec<usize> = (0..data.len().saturating_sub(2))
      .filter(|&at| data[at..at + 3] == [0, 0, 1])
      .collect();
    return starts
      .iter()
      .enumerate()
      .map(|(index, &start)| {
        let end = starts.get(index + 1).copied().unwrap_or(data.len());
        let mut unit = &data[start + 3..end];
        while let [rest @ .., 0] = unit {
          unit = rest;
        }
        unit
      })
      .collect();
  }
  let mut units = Vec::new();
  let mut rest = data;
  while let Some((length, tail)) = rest.split_first_chunk::<4>() {
    let (unit, tail) = tail.split_at(u32::from_be_bytes(*length) as usize);
    units.push(unit);
    rest = tail;
  }
  units
}

/// Whether every slice of `packet` (NAL unit types 1–5) has `nal_ref_idc`
/// 0, and it holds one: a picture nothing references.
fn non_reference(packet: &VideoPacket, annex_b: bool) -> bool {
  let slices: Vec<u8> = nal_units(packet.data().as_ref(), annex_b)
    .iter()
    .filter(|unit| matches!(unit[0] & 0x1F, 1..=5))
    .map(|unit| unit[0])
    .collect();
  !slices.is_empty() && slices.iter().all(|&header| header & 0x60 == 0)
}

/// A hash of `frame`'s visible samples, as many bytes of each row as a
/// 4:2:0 8-bit picture of its width holds — the fixtures' format, checked
/// first — so a decoder's padding is never compared.
fn picture_hash(frame: &VideoFrame) -> u64 {
  assert_eq!(
    *frame.pixel_format(),
    PixelFormat::Yuv420p,
    "the fixtures' format"
  );
  let (width, height) = (frame.width() as usize, frame.height() as usize);
  let mut hasher = DefaultHasher::new();
  for (index, plane) in frame.planes().iter().enumerate() {
    let (row, rows) = if index == 0 {
      (width, height)
    } else {
      (width.div_ceil(2), height.div_ceil(2))
    };
    let stride = plane.stride() as usize;
    let data: &[u8] = plane.data_ref().as_ref();
    for line in 0..rows {
      data[line * stride..line * stride + row].hash(&mut hasher);
    }
  }
  hasher.finish()
}

/// Every picture a software session delivers for `packets`, each as its pts
/// and its [`picture_hash`], sorted.
fn decode(track: &Track, packets: &[&VideoPacket]) -> Vec<(i64, u64)> {
  let mut decoder = FfmpegVideoStreamDecoder::open_from(
    VideoStreamDecoderOpen::new().with_path(DecodePath::Software),
    track.parameters.clone(),
    track.timebase,
    DecoderLimits::default(),
  )
  .expect("open the session");
  let mut frame = empty_owned_video_frame();
  let mut pictures = Vec::new();
  let mut take = |decoder: &mut FfmpegVideoStreamDecoder, frame: &mut VideoFrame| {
    while let Received::Produced = decoder.receive_frame(frame).expect("decode cleanly") {
      let pts = frame.pts().expect("a picture's pts").pts();
      pictures.push((pts, picture_hash(frame)));
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

/// The law every fixture keeps: dropping the marked packets — all of them,
/// and each half of them, every other one — changes no other picture's
/// pixels, and loses no other picture. Each marked packet holds a whole
/// picture, known by its pts.
fn dropping_the_marked_packets_changes_no_other_picture(name: &str, track: &Track) {
  let every: Vec<&VideoPacket> = track.packets.iter().collect();
  let whole = decode(track, &every);
  let mut seen = BTreeSet::new();
  assert!(
    whole.iter().all(|&(pts, _)| seen.insert(pts)),
    "{name}: two pictures at one pts in the whole decode"
  );
  let marked: Vec<usize> = (0..track.packets.len())
    .filter(|&index| disposable(&track.packets[index]))
    .collect();
  assert!(!marked.is_empty(), "{name}: nothing marked");
  let drops: [Vec<usize>; 3] = [
    marked.clone(),
    marked.iter().copied().step_by(2).collect(),
    marked.iter().copied().skip(1).step_by(2).collect(),
  ];
  for dropped in drops {
    let gone: BTreeSet<usize> = dropped.iter().copied().collect();
    let kept: Vec<&VideoPacket> = every
      .iter()
      .enumerate()
      .filter(|(index, _)| !gone.contains(index))
      .map(|(_, packet)| *packet)
      .collect();
    let rest = decode(track, &kept);
    let lost: BTreeSet<i64> = dropped
      .iter()
      .map(|&index| track.packets[index].pts().expect("a pts").pts())
      .collect();
    let expected: Vec<(i64, u64)> = whole
      .iter()
      .filter(|(pts, _)| !lost.contains(pts))
      .copied()
      .collect();
    assert_eq!(
      rest,
      expected,
      "{name}: dropping {} of {} marked packets",
      dropped.len(),
      marked.len()
    );
  }
}

#[test]
fn mbaff_and_picaff_frames_are_marked_as_progressive_ones() {
  let Some(dir) = corpus() else {
    return;
  };
  // MBAFF frames (`frame_mbs_only_flag` 0, `mb_adaptive_frame_field_flag`
  // 1), and frames under an SPS that lets fields in (`fake-interlaced`:
  // `mb_adaptive_frame_field_flag` 0), each slice reading `field_pic_flag` 0.
  let fixtures = [
    ("mbaff.mp4", &["-flags", "+ildct"][..], "tff=1"),
    ("mbaff.264", &["-flags", "+ildct"][..], "tff=1"),
    ("picaff.mp4", &[][..], "fake-interlaced=1"),
    ("picaff.264", &[][..], "fake-interlaced=1"),
  ];
  for (name, interlaced, params) in fixtures {
    let path = dir.path().join(name);
    x264(&path, interlaced, params);
    let track = track(&path);
    assert_eq!(track.packets.len(), 50, "{name}");
    // The non-reference pictures are marked, every one, and nothing else:
    // a reference picture never is.
    let marked: Vec<usize> = (0..50)
      .filter(|&index| disposable(&track.packets[index]))
      .collect();
    let annex_b = name.ends_with(".264");
    let non_reference: Vec<usize> = (0..50)
      .filter(|&index| non_reference(&track.packets[index], annex_b))
      .collect();
    assert!(non_reference.len() > 20, "{name}: {non_reference:?}");
    assert_eq!(marked, non_reference, "{name}");
    dropping_the_marked_packets_changes_no_other_picture(name, &track);
  }
}

/// The hand-written PAFF stream ([`paff`]) at `path`, and the same stream
/// in an MP4 FFmpeg's CLI copies it into, and in one FFmpeg's muxer writes
/// with each pair in one sample.
struct Paff {
  raw: PathBuf,
  fields: PathBuf,
  pairs: PathBuf,
}

fn paff(dir: &Path) -> Paff {
  let raw = dir.join("paff.264");
  std::fs::write(&raw, paff::stream()).expect("write the stream");
  let fields = dir.join("paff_fields.mp4");
  ffmpeg(&[
    "-i",
    raw.to_str().expect("a UTF-8 path"),
    "-c",
    "copy",
    fields.to_str().expect("a UTF-8 path"),
  ]);
  let pairs = dir.join("paff_pairs.mp4");
  paff::mux_pairs(&raw, &pairs);
  Paff { raw, fields, pairs }
}

#[test]
fn paff_pairs_in_one_packet_are_marked_and_pairs_split_across_packets_kept_whole() {
  let Some(dir) = corpus() else {
    return;
  };
  let paff = paff(dir.path());
  let plan = paff::plan();
  // Split: FFmpeg's parser ends a picture at each field (`h264_parser.c`
  // 140–157), and its MP4 muxer writes each packet as a sample.
  for path in [&paff.raw, &paff.fields] {
    let track = track(path);
    let shapes: Vec<(paff::Shape, bool)> = plan
      .iter()
      .flat_map(|picture| {
        let packets = match picture.shape {
          paff::Shape::Pair => 2,
          paff::Shape::Frame => 1,
        };
        std::iter::repeat_n((picture.shape, picture.reference), packets)
      })
      .collect();
    assert_eq!(track.packets.len(), shapes.len(), "{path:?}");
    dropping_the_marked_packets_changes_no_other_picture(&format!("{path:?}"), &track);
    for (index, (packet, (shape, reference))) in track.packets.iter().zip(&shapes).enumerate() {
      // A field alone is never marked, its pair's other half in the packet
      // beside it: the pair is judged together. A non-reference frame is.
      let expected = *shape == paff::Shape::Frame && !reference;
      assert_eq!(
        disposable(packet),
        expected,
        "{path:?}: packet {index}, {shape:?}, reference {reference}"
      );
    }
  }
  // Joined: a pair of non-reference fields in one sample is marked as one
  // picture; a reference one never is.
  let track = track(&paff.pairs);
  assert_eq!(track.packets.len(), plan.len());
  for (index, (packet, picture)) in track.packets.iter().zip(&plan).enumerate() {
    assert_eq!(
      disposable(packet),
      !picture.reference,
      "pairs: packet {index}, {picture:?}"
    );
  }
  assert!(
    plan
      .iter()
      .any(|picture| picture.shape == paff::Shape::Pair && !picture.reference)
  );
  dropping_the_marked_packets_changes_no_other_picture("pairs", &track);
}

/// An H.264 PAFF stream FFmpeg decodes exactly, written whole: Main profile,
/// CAVLC, 64×64, `frame_mbs_only_flag` 0 and `mb_adaptive_frame_field_flag`
/// 0, so that each picture is a field or a frame of its own (PicAFF across
/// them); every macroblock I_PCM, or P_Skip, which copies the first
/// reference of its list; deblocking off; a `frame_num` of four bits that
/// steps after each reference picture; picture order counts of eight bits
/// that only rise, and a reorder depth of 0, so that decoding order is
/// output order. Each field and frame is an access unit of its own, after an
/// access unit delimiter.
mod paff {
  use std::path::Path;

  /// Bits, most significant first.
  struct Bits {
    bits: Vec<bool>,
  }

  impl Bits {
    fn new() -> Self {
      Self { bits: Vec::new() }
    }

    fn bit(&mut self, bit: u32) -> &mut Self {
      self.bits.push(bit & 1 == 1);
      self
    }

    fn bits(&mut self, value: u32, n: u32) -> &mut Self {
      for at in (0..n).rev() {
        self.bit(value >> at);
      }
      self
    }

    fn ue(&mut self, value: u32) -> &mut Self {
      let code = u64::from(value) + 1;
      let length = 64 - code.leading_zeros();
      for _ in 1..length {
        self.bit(0);
      }
      for at in (0..length).rev() {
        self.bit((code >> at) as u32);
      }
      self
    }

    fn se(&mut self, value: i32) -> &mut Self {
      let code = if value > 0 {
        2 * value.unsigned_abs() - 1
      } else {
        2 * value.unsigned_abs()
      };
      self.ue(code)
    }

    /// Zero bits to the next byte: `pcm_alignment_zero_bit`.
    fn align(&mut self) -> &mut Self {
      while self.bits.len() % 8 != 0 {
        self.bit(0);
      }
      self
    }

    /// `rbsp_trailing_bits`, and the NAL unit of `header`, its bytes
    /// escaped as an encoder escapes them.
    fn nal(&mut self, header: u8) -> Vec<u8> {
      self.bit(1).align();
      let mut unit = vec![header];
      let mut zeros = 0;
      for byte in self.bits.chunks(8) {
        let byte = byte
          .iter()
          .fold(0_u8, |byte, &bit| (byte << 1) | u8::from(bit));
        if zeros >= 2 && byte <= 3 {
          unit.push(3);
          zeros = 0;
        }
        unit.push(byte);
        zeros = if byte == 0 { zeros + 1 } else { 0 };
      }
      unit
    }
  }

  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub enum Shape {
    /// Two fields, top first.
    Pair,
    /// A frame.
    Frame,
  }

  #[derive(Debug, Clone, Copy)]
  pub struct Picture {
    pub shape: Shape,
    pub reference: bool,
  }

  /// The pictures, in order: an IDR pair, then four times over a
  /// non-reference pair, a reference pair, a non-reference pair, a
  /// non-reference frame, a reference frame and a reference pair.
  pub fn plan() -> Vec<Picture> {
    let picture = |shape, reference| Picture { shape, reference };
    let mut plan = vec![picture(Shape::Pair, true)];
    for _ in 0..4 {
      plan.extend([
        picture(Shape::Pair, false),
        picture(Shape::Pair, true),
        picture(Shape::Pair, false),
        picture(Shape::Frame, false),
        picture(Shape::Frame, true),
        picture(Shape::Pair, true),
      ]);
    }
    plan
  }

  fn sps() -> Vec<u8> {
    let mut bits = Bits::new();
    // Main, level 3.0, SPS 0; `log2_max_frame_num_minus4` 0, POC type 0 of
    // eight bits, two reference frames, no gaps; 4×2 map units of macroblock
    // pairs; fields and frames, no MBAFF, `direct_8x8_inference_flag`.
    bits
      .bits(77, 8)
      .bits(0, 8)
      .bits(30, 8)
      .ue(0)
      .ue(0)
      .ue(0)
      .ue(4)
      .ue(2)
      .bit(0)
      .ue(3)
      .ue(1)
      .bit(0)
      .bit(0)
      .bit(1)
      .bit(0);
    // A VUI: 25 frames a second, and the bitstream restriction, a reorder
    // depth of 0.
    bits.bit(1).bit(0).bit(0).bit(0).bit(0);
    bits.bit(1).bits(1, 32).bits(50, 32).bit(1);
    bits.bit(0).bit(0).bit(0);
    bits.bit(1).bit(1).ue(0).ue(0).ue(16).ue(16).ue(0).ue(2);
    bits.nal(0x67)
  }

  fn pps() -> Vec<u8> {
    let mut bits = Bits::new();
    bits
      .ue(0)
      .ue(0)
      .bit(0)
      .bit(0)
      .ue(0)
      .ue(0)
      .ue(0)
      .bit(0)
      .bits(0, 2)
      .se(0)
      .se(0)
      .se(0)
      .bit(1)
      .bit(0)
      .bit(0);
    bits.nal(0x68)
  }

  fn aud() -> Vec<u8> {
    Bits::new().bits(7, 3).nal(0x09)
  }

  /// One I_PCM macroblock's samples, 4:2:0, every picture's, field's and
  /// macroblock's its own.
  fn samples(picture: usize, parity: usize, macroblock: usize) -> impl Iterator<Item = u8> {
    let luma =
      (0..256).map(move |i| 16 + ((picture * 7 + parity * 50 + macroblock * 13 + i) % 200));
    let cb = (0..64).map(move |i| 64 + ((picture * 3 + parity * 17 + macroblock + i) % 128));
    let cr = (0..64).map(move |i| 64 + ((picture * 5 + parity * 29 + macroblock * 2 + i) % 128));
    luma.chain(cb).chain(cr).map(|sample| sample as u8)
  }

  /// One slice: picture `index`'s field — top for `Some(false)`, bottom
  /// for `Some(true)` — or frame for `None`.
  fn slice(
    index: usize,
    field: Option<bool>,
    frame_num: u32,
    reference: bool,
    idr: bool,
  ) -> Vec<u8> {
    let intra = index == 0;
    let macroblocks = if field.is_some() { 8 } else { 16 };
    let mut bits = Bits::new();
    // `first_mb_in_slice`, `slice_type` (I or P, every slice alike), the
    // PPS, `frame_num`, the field flags.
    bits
      .ue(0)
      .ue(if intra { 7 } else { 5 })
      .ue(0)
      .bits(frame_num, 4);
    match field {
      None => {
        bits.bit(0);
      }
      Some(bottom) => {
        bits.bit(1).bit(u32::from(bottom));
      }
    }
    if idr {
      bits.ue(0);
    }
    let parity = field.map_or(0, usize::from);
    bits.bits(((2 * index + parity) % 256) as u32, 8);
    if !intra {
      // No reference count override, no list modification.
      bits.bit(0).bit(0);
    }
    if reference {
      // The sliding window; an IDR's prior pictures output, no long-term one.
      if idr {
        bits.bit(0).bit(0);
      } else {
        bits.bit(0);
      }
    }
    // `slice_qp_delta`, the deblocking filter off.
    bits.se(0).ue(1);
    let mut skipped = 0;
    for macroblock in 0..macroblocks {
      if !intra && (macroblock + index) % 2 == 1 {
        skipped += 1;
        continue;
      }
      if !intra {
        bits.ue(skipped);
        skipped = 0;
      }
      // I_PCM: 25 in an I slice, 5 + 25 in a P slice.
      bits.ue(if intra { 25 } else { 30 }).align();
      for sample in samples(index, parity + usize::from(field.is_some()), macroblock) {
        bits.bits(u32::from(sample), 8);
      }
    }
    if skipped > 0 {
      bits.ue(skipped);
    }
    let nal_unit_type = if idr { 5 } else { 1 };
    let nal_ref_idc = if reference { 3 } else { 0 };
    bits.nal((nal_ref_idc << 5) | nal_unit_type)
  }

  /// The stream of [`plan`], in Annex B.
  pub fn stream() -> Vec<u8> {
    let mut out = Vec::new();
    let mut unit = |nal: Vec<u8>| {
      out.extend_from_slice(&[0, 0, 0, 1]);
      out.extend_from_slice(&nal);
    };
    let mut reference_frame_num: Option<u32> = None;
    for (index, picture) in plan().into_iter().enumerate() {
      let frame_num = reference_frame_num.map_or(0, |frame_num| (frame_num + 1) % 16);
      if picture.reference {
        reference_frame_num = Some(frame_num);
      }
      let fields: &[Option<bool>] = match picture.shape {
        Shape::Pair => &[Some(false), Some(true)],
        Shape::Frame => &[None],
      };
      for &field in fields {
        let idr = index == 0 && field == Some(false);
        unit(aud());
        if idr {
          unit(sps());
          unit(pps());
        }
        unit(slice(index, field, frame_num, picture.reference, idr));
      }
    }
    out
  }

  /// The stream at `raw` into an MP4 at `out`, through FFmpeg's demuxer and
  /// muxer, each pair of fields in one packet: the packets FFmpeg's parser
  /// splits, one to a field, joined again by [`plan`].
  pub fn mux_pairs(raw: &Path, out: &Path) {
    let mut input = ffmpeg_next::format::input(raw).expect("open the raw stream");
    let parameters = input
      .streams()
      .best(ffmpeg_next::media::Type::Video)
      .expect("a video stream")
      .parameters();
    let mut packets: Vec<Vec<u8>> = input
      .packets()
      .map(|(_, packet)| packet.data().expect("a packet's bytes").to_vec())
      .collect();
    packets.reverse();
    let mut output = ffmpeg_next::format::output(out).expect("open the MP4");
    {
      let mut stream = output
        .add_stream(None::<ffmpeg_next::Codec>)
        .expect("a stream");
      stream.set_parameters(parameters);
      stream.set_time_base((1, 25));
    }
    output.write_header().expect("the header");
    let time_base = output.stream(0).expect("the stream").time_base();
    for (index, picture) in plan().into_iter().enumerate() {
      let mut data = packets.pop().expect("a field or frame");
      if picture.shape == Shape::Pair {
        data.extend_from_slice(&packets.pop().expect("its second field"));
      }
      let mut packet = ffmpeg_next::Packet::copy(&data);
      packet.set_stream(0);
      packet.set_pts(Some(index as i64));
      packet.set_dts(Some(index as i64));
      packet.set_duration(1);
      if index == 0 {
        packet.set_flags(ffmpeg_next::packet::Flags::KEY);
      }
      packet.rescale_ts((1, 25), time_base);
      packet.write_interleaved(&mut output).expect("a packet");
    }
    assert!(packets.is_empty(), "every packet joined");
    output.write_trailer().expect("the trailer");
  }
}
