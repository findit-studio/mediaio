use std::{vec, vec::Vec};

use super::*;

/// An H.264 NAL unit: its header byte, then `payload`.
fn avc(nal_ref_idc: u8, nal_unit_type: u8, payload: &[u8]) -> Vec<u8> {
  let mut nal = vec![(nal_ref_idc << 5) | nal_unit_type];
  nal.extend_from_slice(payload);
  nal
}

/// A non-reference H.264 slice: `first_mb_in_slice` 0, `slice_type` 6, B.
fn b_slice() -> Vec<u8> {
  avc(0, 1, &[0x9C, 0x84, 0x21])
}

/// A non-reference H.264 slice of `slice_type` 7, I.
fn i_slice() -> Vec<u8> {
  avc(0, 1, &[0x88, 0x84, 0x21])
}

/// A reference H.264 slice.
fn p_slice() -> Vec<u8> {
  avc(2, 1, &[0x9A, 0x02, 0x10])
}

/// Bits, most significant first, for building a parameter set.
struct Bits {
  bytes: Vec<u8>,
  used: usize,
}

impl Bits {
  fn new() -> Self {
    Self {
      bytes: Vec::new(),
      used: 0,
    }
  }

  fn bit(&mut self, bit: u32) -> &mut Self {
    if self.used % 8 == 0 {
      self.bytes.push(0);
    }
    let shift = 7 - self.used % 8;
    *self.bytes.last_mut().expect("a byte") |= ((bit & 1) as u8) << shift;
    self.used += 1;
    self
  }

  fn bits(&mut self, value: u32, n: u32) -> &mut Self {
    for at in (0..n).rev() {
      self.bit(value >> at);
    }
    self
  }

  /// `ue(v)`.
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

  /// `se(v)`.
  fn se(&mut self, value: i32) -> &mut Self {
    let code = if value > 0 {
      2 * value.unsigned_abs() - 1
    } else {
      2 * value.unsigned_abs()
    };
    self.ue(code)
  }

  /// `rbsp_trailing_bits`, then the unit's bytes as an encoder escapes
  /// them.
  fn finish(&mut self) -> Vec<u8> {
    self.bit(1);
    escaped(&self.bytes)
  }
}

/// What an H.264 sequence parameter set says, up to its
/// `frame_mbs_only_flag` and a little past it.
#[derive(Clone, Copy)]
struct Sps {
  profile_idc: u32,
  frames_only: bool,
  chroma_format_idc: u32,
  separate_colour_planes: bool,
  luma_minus8: u32,
  chroma_minus8: u32,
  log2_max_frame_num_minus4: u32,
  pic_order_cnt_type: u32,
  max_num_ref_frames: u32,
  width_in_mbs_minus1: u32,
  height_in_map_units_minus1: u32,
  gaps: bool,
  vui: Vui,
}

/// What an [`Sps`] writes for its VUI.
#[derive(Clone, Copy)]
enum Vui {
  /// No VUI.
  Absent,
  /// A VUI that sets `bitstream_restriction_flag` and nothing else, with
  /// `max_num_reorder_frames` of the given count.
  Restricted(u32),
  /// The same, after a SAR, a colour description, chroma sites, timing and
  /// NAL HRD parameters of one CPB.
  RestrictedAfterAll,
  /// A VUI without the restriction.
  Unrestricted,
}

impl Sps {
  /// Baseline, 1080p, coding frames only.
  const BASELINE: Self = Self {
    profile_idc: 66,
    frames_only: true,
    chroma_format_idc: 1,
    separate_colour_planes: false,
    luma_minus8: 0,
    chroma_minus8: 0,
    log2_max_frame_num_minus4: 0,
    pic_order_cnt_type: 0,
    max_num_ref_frames: 1,
    width_in_mbs_minus1: 119,
    height_in_map_units_minus1: 67,
    gaps: false,
    vui: Vui::Restricted(1),
  };

  /// The same at High profile, with a scaling matrix written.
  const HIGH: Self = Self {
    profile_idc: 100,
    ..Self::BASELINE
  };

  /// The unit: its header, then the RBSP, escaped.
  fn unit(self) -> Vec<u8> {
    let mut bits = Bits::new();
    bits.bits(self.profile_idc, 8).bits(0, 8).bits(40, 8).ue(0);
    if matches!(
      self.profile_idc,
      100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 144
    ) {
      bits.ue(self.chroma_format_idc);
      if self.chroma_format_idc == 3 {
        bits.bit(u32::from(self.separate_colour_planes));
      }
      // The two bit depths, no transform bypass, and a scaling matrix: its
      // first list written, the rest not.
      bits
        .ue(self.luma_minus8)
        .ue(self.chroma_minus8)
        .bit(0)
        .bit(1)
        .bit(1);
      for delta in [1, -1, 2, 0, 0, 3, -2, 0, 0, 0, 0, 0, 0, 0, 0, 0] {
        bits.se(delta);
      }
      let lists = if self.chroma_format_idc == 3 { 12 } else { 8 };
      for _ in 1..lists {
        bits.bit(0);
      }
    }
    bits
      .ue(self.log2_max_frame_num_minus4)
      .ue(self.pic_order_cnt_type);
    if self.pic_order_cnt_type == 0 {
      bits.ue(0);
    }
    bits
      .ue(self.max_num_ref_frames)
      .bit(u32::from(self.gaps))
      .ue(self.width_in_mbs_minus1)
      .ue(self.height_in_map_units_minus1)
      .bit(u32::from(self.frames_only));
    if !self.frames_only {
      // `mb_adaptive_frame_field_flag`.
      bits.bit(1);
    }
    // `direct_8x8_inference_flag`, no cropping.
    bits.bit(1).bit(0);
    match self.vui {
      Vui::Absent => {
        bits.bit(0);
      }
      Vui::Restricted(reorder) => {
        // No SAR, overscan, signal type, chroma sites, timing or HRD, no
        // `pic_struct`; then the restriction.
        bits.bit(1).bits(0, 8);
        restriction(&mut bits, reorder);
      }
      Vui::RestrictedAfterAll => {
        // An extended SAR, overscan, a signal type with its colour
        // description, chroma sites, timing, NAL HRD of one CPB, no VCL
        // HRD, `low_delay_hrd_flag`, `pic_struct_present_flag`.
        bits.bit(1).bit(1).bits(255, 8).bits(4, 16).bits(3, 16);
        bits.bit(1).bit(0);
        bits
          .bit(1)
          .bits(5, 3)
          .bit(0)
          .bit(1)
          .bits(9, 8)
          .bits(16, 8)
          .bits(9, 8);
        bits.bit(1).ue(0).ue(0);
        bits.bit(1).bits(1001, 32).bits(60_000, 32).bit(1);
        bits
          .bit(1)
          .ue(0)
          .bits(4, 4)
          .bits(6, 4)
          .ue(62_499)
          .ue(124_999)
          .bit(0);
        bits.bits(23, 5).bits(23, 5).bits(23, 5).bits(24, 5);
        bits.bit(0).bit(0).bit(1);
        restriction(&mut bits, 2);
      }
      Vui::Unrestricted => {
        bits.bit(1).bits(0, 8).bit(0);
      }
    }
    let mut unit = vec![0x67];
    unit.extend_from_slice(&bits.finish());
    unit
  }
}

/// `bitstream_restriction_flag` 1 and its fields, `max_num_reorder_frames`
/// `reorder`.
fn restriction(bits: &mut Bits, reorder: u32) {
  bits
    .bit(1)
    .bit(1)
    .ue(2)
    .ue(1)
    .ue(16)
    .ue(16)
    .ue(reorder)
    .ue(4);
}

/// An H.264 sequence parameter set coding frames only.
fn avc_sps() -> Vec<u8> {
  Sps::BASELINE.unit()
}

/// An HEVC NAL unit: its two header bytes, then `payload`.
fn hevc(nal_unit_type: u8, layer: u8, temporal_id: u8, payload: &[u8]) -> Vec<u8> {
  let mut nal = vec![
    (nal_unit_type << 1) | (layer >> 5),
    ((layer & 0x1F) << 3) | (temporal_id + 1),
  ];
  nal.extend_from_slice(payload);
  nal
}

/// An HEVC SPS declaring `max_sub_layers_minus1`.
fn hevc_sps(max_sub_layers_minus1: u8) -> Vec<u8> {
  hevc(33, 0, 0, &[(max_sub_layers_minus1 << 1) | 1, 0x01, 0x60])
}

/// An HEVC VPS declaring `max_sub_layers_minus1`.
fn hevc_vps(max_sub_layers_minus1: u8) -> Vec<u8> {
  hevc(32, 0, 0, &[0x0C, 0x01 | (max_sub_layers_minus1 << 1), 0xFF])
}

/// `rbsp` with an emulation prevention byte before every byte of 3 or
/// less that follows two zero bytes, as an encoder writes it.
fn escaped(rbsp: &[u8]) -> Vec<u8> {
  let mut out = Vec::new();
  let mut zeros = 0;
  for &byte in rbsp {
    if zeros >= 2 && byte <= 3 {
      out.push(3);
      zeros = 0;
    }
    out.push(byte);
    zeros = if byte == 0 { zeros + 1 } else { 0 };
  }
  out
}

/// One SEI message: its `payloadType` and `payloadSize`, each coded in
/// runs of `0xFF`, then `payload`.
fn sei_message(payload_type: u32, payload: &[u8]) -> Vec<u8> {
  let mut out = Vec::new();
  for mut value in [payload_type as usize, payload.len()] {
    while value >= 255 {
      out.push(0xFF);
      value -= 255;
    }
    out.push(value as u8);
  }
  out.extend_from_slice(payload);
  out
}

/// The RBSP of an SEI unit holding `messages`, its trailing byte last.
fn sei_rbsp(messages: &[Vec<u8>]) -> Vec<u8> {
  let mut rbsp: Vec<u8> = messages.concat();
  rbsp.push(0x80);
  rbsp
}

/// An H.264 SEI unit holding `messages`.
fn avc_sei(messages: &[Vec<u8>]) -> Vec<u8> {
  avc(0, 6, &escaped(&sei_rbsp(messages)))
}

/// An HEVC SEI unit of `nal_unit_type`, prefix or suffix, holding
/// `messages`.
fn hevc_sei(nal_unit_type: u8, temporal_id: u8, messages: &[Vec<u8>]) -> Vec<u8> {
  hevc(nal_unit_type, 0, temporal_id, &escaped(&sei_rbsp(messages)))
}

/// A picture timing message, three bytes of it zero.
fn picture_timing() -> Vec<u8> {
  sei_message(1, &[0x00, 0x00, 0x00])
}

/// A filler payload message.
fn filler() -> Vec<u8> {
  sei_message(3, &[0xFF, 0xFF])
}

/// A decoded picture hash message: an MD5 of zeros.
fn picture_hash() -> Vec<u8> {
  sei_message(132, &[0; 17])
}

/// `nals` each after a big-endian length of `size` bytes.
fn lengths(size: usize, nals: &[Vec<u8>]) -> Vec<u8> {
  let mut out = Vec::new();
  for nal in nals {
    let length = nal.len().to_be_bytes();
    out.extend_from_slice(&length[length.len() - size..]);
    out.extend_from_slice(nal);
  }
  out
}

/// `nals` each after a start code: four bytes for the first, three after.
fn annex_b(nals: &[Vec<u8>]) -> Vec<u8> {
  let mut out = Vec::new();
  for (index, nal) in nals.iter().enumerate() {
    out.extend_from_slice(if index == 0 {
      &[0, 0, 0, 1]
    } else {
      &[0, 0, 1]
    });
    out.extend_from_slice(nal);
  }
  out
}

/// An `avcC` of `sps` and one PPS, its `lengthSizeMinusOne` given.
fn avcc_of(length_size_minus_one: u8, sps: &[u8]) -> Vec<u8> {
  let pps = avc(3, 8, &[0xEE, 0x3C, 0x80]);
  let mut record = vec![1, 0x64, 0x00, 0x28, 0xFC | length_size_minus_one, 0xE1];
  record.extend_from_slice(&(sps.len() as u16).to_be_bytes());
  record.extend_from_slice(sps);
  record.push(1);
  record.extend_from_slice(&(pps.len() as u16).to_be_bytes());
  record.extend_from_slice(&pps);
  record
}

/// An `avcC` of one SPS and one PPS, its `lengthSizeMinusOne` given.
fn avcc(length_size_minus_one: u8) -> Vec<u8> {
  avcc_of(length_size_minus_one, &avc_sps())
}

/// An `hvcC` of one array holding `nals`, its `numTemporalLayers` and
/// `lengthSizeMinusOne` given.
fn hvcc(num_temporal_layers: u8, length_size_minus_one: u8, nals: &[Vec<u8>]) -> Vec<u8> {
  let mut record = vec![0; 23];
  record[0] = 1;
  record[21] = (num_temporal_layers << 3) | length_size_minus_one;
  record[22] = 1;
  record.push(0x80 | 33);
  record.extend_from_slice(&(nals.len() as u16).to_be_bytes());
  for nal in nals {
    record.extend_from_slice(&(nal.len() as u16).to_be_bytes());
    record.extend_from_slice(nal);
  }
  record
}

fn kept(reason: KeptReason) -> Verdict {
  Verdict::Kept(reason)
}

/// An H.264 reading of `extradata` past a reference picture and an intra
/// slice, as a stream's reading is past its first IDR picture: the laws
/// below read the packets after it.
fn h264(extradata: &[u8]) -> NalStream {
  let mut stream = NalStream::new(NalCodec::H264, extradata);
  stream.reference_seen = true;
  stream.intra_seen = true;
  stream
}

/// An HEVC reading of `extradata` past its first packet, which takes up the
/// `eos` the decoder opens with: the laws below read the packets after it.
fn hevc_stream(extradata: &[u8]) -> NalStream {
  let mut stream = NalStream::new(NalCodec::Hevc, extradata);
  stream.sequence_start = false;
  stream
}

#[test]
fn an_avc_record_frames_lengths_of_one_to_four_bytes() {
  // ISO/IEC 14496-15 allows 1, 2 and 4; FFmpeg reads 3 as it reads the
  // others.
  for (minus_one, size) in [(0, 1), (1, 2), (2, 3), (3, 4)] {
    let mut stream = h264(&avcc(minus_one));
    let framing = match size {
      1 => Framing::Length(LengthSize::One),
      2 => Framing::Length(LengthSize::Two),
      3 => Framing::Length(LengthSize::Three),
      _ => Framing::Length(LengthSize::Four),
    };
    assert_eq!(stream.framing(), Some(framing), "{size}");
    assert_eq!(
      stream.read(&lengths(size, &[b_slice()])),
      Verdict::Disposable,
      "{size}"
    );
    assert_eq!(
      stream.read(&lengths(size, &[p_slice()])),
      kept(KeptReason::Referenced),
      "{size}"
    );
  }
  // A record that ends before its parameter sets do sets lengths and no
  // size: FFmpeg splits nothing.
  let record = avcc(3);
  let mut short = h264(&record[..record.len() - 1]);
  assert_eq!(short.framing(), None);
  assert_eq!(
    short.read(&lengths(4, &[b_slice()])),
    kept(KeptReason::Unframed)
  );
  assert_eq!(h264(&record[..6]).framing(), None);
}

#[test]
fn annex_b_reads_three_and_four_byte_start_codes_and_leading_and_trailing_zeros() {
  // No extradata, or extradata that is no `avcC`, is Annex B; without one
  // the sequence parameter set arrives in-band.
  for extradata in [&[][..], &annex_b(&[avc_sps()])[..]] {
    let mut stream = h264(extradata);
    assert_eq!(stream.framing(), Some(Framing::AnnexB));
    if extradata.is_empty() {
      assert_eq!(
        stream.read(&annex_b(&[avc_sps()])),
        kept(KeptReason::Companion(7))
      );
    }
    let aud = avc(0, 9, &[0xF0]);
    let mut packet = vec![0, 0];
    packet.extend_from_slice(&annex_b(&[aud, b_slice(), b_slice()]));
    packet.extend_from_slice(&[0, 0, 0]);
    assert_eq!(stream.read(&packet), Verdict::Disposable);
    assert_eq!(
      stream.read(&annex_b(&[b_slice(), p_slice()])),
      kept(KeptReason::Referenced)
    );
  }
  let mut stream = h264(&[]);
  // A byte other than zero before the first start code, no start code at
  // all, and a start code with nothing after it.
  assert_eq!(
    stream.read(&[7, 0, 0, 1, 0x01]),
    kept(KeptReason::Malformed)
  );
  assert_eq!(stream.read(&[0x01, 0x88]), kept(KeptReason::Malformed));
  assert_eq!(
    stream.read(&[0, 0, 1, 0x01, 0x88, 0, 0, 1]),
    kept(KeptReason::Malformed)
  );
}

#[test]
fn an_h264_packet_is_disposable_only_where_every_slice_is_non_reference_and_none_is_idr() {
  let mut stream = h264(&avcc(3));
  let read = |stream: &mut NalStream, nals: &[Vec<u8>]| stream.read(&lengths(4, nals));
  // An access unit delimiter, filler data and an SEI unit of picture
  // timing ride beside the slices.
  let sei = avc_sei(&[picture_timing()]);
  let aud = avc(0, 9, &[0xF0]);
  let filler_data = avc(0, 12, &[0xFF, 0x80]);
  assert_eq!(
    read(
      &mut stream,
      &[aud.clone(), sei.clone(), b_slice(), filler_data]
    ),
    Verdict::Disposable
  );
  // A field pair: both fields of `nal_ref_idc` 0, or one a reference.
  assert_eq!(
    read(&mut stream, &[b_slice(), b_slice()]),
    Verdict::Disposable
  );
  assert_eq!(
    read(&mut stream, &[b_slice(), p_slice()]),
    kept(KeptReason::Referenced)
  );
  // Slice data partitions are slices too.
  assert_eq!(
    read(&mut stream, &[avc(0, 2, &[0x9C]), avc(0, 3, &[0x88])]),
    Verdict::Disposable
  );
  assert_eq!(
    read(&mut stream, &[avc(0, 2, &[0x9C]), avc(1, 4, &[0x88])]),
    kept(KeptReason::Referenced)
  );
  // An IDR slice, whatever its `nal_ref_idc`.
  assert_eq!(
    read(&mut stream, &[avc(3, 5, &[0x88])]),
    kept(KeptReason::RandomAccess)
  );
  assert_eq!(
    read(&mut stream, &[avc(0, 5, &[0x88])]),
    kept(KeptReason::RandomAccess)
  );
  // No slice at all.
  assert_eq!(read(&mut stream, &[aud, sei]), kept(KeptReason::NoPicture));
}

#[test]
fn a_parameter_set_an_sei_unit_or_an_end_beside_a_non_reference_slice_keeps_the_packet() {
  let mut stream = h264(&avcc(3));
  for nal_unit_type in [7, 8, 6, 10, 11, 13, 15, 16, 19, 0, 23, 31] {
    assert_eq!(
      stream.read(&lengths(4, &[avc(3, nal_unit_type, &[0x42]), b_slice()])),
      kept(KeptReason::Companion(nal_unit_type)),
      "{nal_unit_type}"
    );
  }
  // The scalable, multiview and 3-D extensions.
  for nal_unit_type in [14, 20, 21] {
    assert_eq!(
      stream.read(&lengths(4, &[avc(0, nal_unit_type, &[0x42]), b_slice()])),
      kept(KeptReason::Extension),
      "{nal_unit_type}"
    );
  }
}

#[test]
fn an_h264_sei_unit_rides_only_where_every_message_is_picture_timing_or_filler() {
  let mut stream = h264(&avcc(3));
  let mut read = |sei: Vec<u8>| stream.read(&lengths(4, &[sei, b_slice()]));
  // The picture timing message holds `00 00 00`, so the unit holds an
  // emulation prevention byte the reading drops.
  assert!(
    avc_sei(&[picture_timing()])
      .windows(3)
      .any(|w| w == [0, 0, 3])
  );
  assert_eq!(read(avc_sei(&[picture_timing()])), Verdict::Disposable);
  assert_eq!(
    read(avc_sei(&[picture_timing(), filler()])),
    Verdict::Disposable
  );
  // Every other message keeps the packet: a buffering period, user data
  // registered and unregistered, a recovery point, film grain, frame
  // packing, mastering display, content light — and types past 255,
  // coded in runs of `0xFF`.
  for payload_type in [0, 2, 4, 5, 6, 19, 45, 137, 144, 255, 256, 300] {
    assert_eq!(
      read(avc_sei(&[
        picture_timing(),
        sei_message(payload_type, &[0x42])
      ])),
      kept(KeptReason::Companion(6)),
      "{payload_type}"
    );
  }
  // A unit that does not read as FFmpeg reads one: no message, a message
  // past the unit's end, no trailing byte.
  assert_eq!(read(avc(0, 6, &[0x80])), kept(KeptReason::Companion(6)));
  let whole = avc_sei(&[sei_message(1, &[0x42, 0x42])]);
  let mut short = whole.clone();
  short.remove(whole.len() - 2);
  assert_eq!(read(short), kept(KeptReason::Companion(6)));
  let mut untrailed = whole;
  untrailed.pop();
  assert_eq!(read(untrailed), kept(KeptReason::Companion(6)));
}

#[test]
fn a_packet_that_does_not_parse_is_kept() {
  let mut stream = h264(&avcc(3));
  // A length of 0, one past the end, and bytes too few for a length.
  assert_eq!(
    stream.read(&[0, 0, 0, 0, 0x01, 0x88]),
    kept(KeptReason::Malformed)
  );
  assert_eq!(
    stream.read(&[0, 0, 0, 9, 0x01, 0x88]),
    kept(KeptReason::Malformed)
  );
  let mut packet = lengths(4, &[b_slice()]);
  packet.extend_from_slice(&[0, 0]);
  assert_eq!(stream.read(&packet), kept(KeptReason::Malformed));
  // The forbidden bit.
  assert_eq!(
    stream.read(&lengths(4, &[vec![0x81, 0x88]])),
    kept(KeptReason::Malformed)
  );
  // A malformed unit after a keeping one is still malformed: the packet
  // does not parse.
  assert_eq!(
    stream.read(&lengths(4, &[p_slice(), vec![0x81, 0x88]])),
    kept(KeptReason::Malformed)
  );
  // A non-reference slice whose bytes hold `00 00 01 41`: FFmpeg's
  // splitter reads a reference slice after the start code.
  assert_eq!(
    stream.read(&lengths(4, &[avc(0, 1, &[0x88, 0, 0, 1, 0x41, 0x99])])),
    kept(KeptReason::Malformed)
  );
  // An empty packet holds no picture.
  assert_eq!(stream.read(&[]), kept(KeptReason::NoPicture));
}

#[test]
fn a_four_byte_length_track_switches_its_framing_where_ffmpeg_does_and_keeps_the_switch() {
  // `h264dec.c` 602–607: a packet that begins `00 00 00 01` whose next four
  // bytes are no length that fits is Annex B, and so are the packets after
  // it until one begins with a length that fits. A packet that switches
  // the framing is kept; the packets after it are read in the new one.
  let mut stream = h264(&avcc(3));
  let annex = annex_b(&[avc(0, 9, &[0xF0]), b_slice()]);
  assert_eq!(stream.read(&annex), kept(KeptReason::FramingChange));
  assert_eq!(stream.framing(), Some(Framing::AnnexB));
  assert_eq!(stream.read(&annex), Verdict::Disposable);
  assert_eq!(
    stream.read(&lengths(4, &[b_slice()])),
    kept(KeptReason::FramingChange)
  );
  assert_eq!(stream.framing(), Some(Framing::Length(LengthSize::Four)));
  assert_eq!(stream.read(&lengths(4, &[b_slice()])), Verdict::Disposable);
  // Two-byte lengths are read as declared.
  let mut two = h264(&avcc(1));
  assert_eq!(two.read(&annex), kept(KeptReason::Malformed));
}

#[test]
fn the_four_byte_size_outlives_an_annex_b_record_and_a_length_of_257_is_read_as_one() {
  // An Annex B record makes the stream one of start codes and keeps the
  // four-byte size (`h264_parse.c` 517–519), so the probe still runs.
  let mut stream = h264(&avcc(3));
  stream.reconfigure(&annex_b(&[avc_sps()]));
  assert_eq!(stream.framing(), Some(Framing::AnnexB));
  // `00 00 01 01 41 …`: as Annex B, a start code and a non-reference
  // slice; as FFmpeg reads it, a four-byte length of 257 and a reference
  // slice.
  let mut reference = avc(2, 1, &[]);
  reference.resize(257, 0x88);
  let packet = lengths(4, &[reference]);
  assert_eq!(&packet[..5], &[0, 0, 1, 1, 0x41]);
  assert_eq!(stream.read(&packet), kept(KeptReason::FramingChange));
  assert_eq!(stream.framing(), Some(Framing::Length(LengthSize::Four)));
  assert_eq!(stream.read(&packet), kept(KeptReason::Referenced));
  // The same bytes on a track no record gave a size: start codes, and a
  // non-reference slice — what the four-byte track must not read.
  let mut annex = h264(&annex_b(&[avc_sps()]));
  assert_eq!(annex.read(&packet), Verdict::Disposable);
}

#[test]
fn a_record_moves_only_what_ffmpeg_moves() {
  let mut stream = h264(&avcc(3));
  stream.reconfigure(&avcc(1));
  assert_eq!(stream.framing(), Some(Framing::Length(LengthSize::Two)));
  // An empty record moves nothing (`h264_parse.c` 472–473).
  stream.reconfigure(&[]);
  assert_eq!(stream.framing(), Some(Framing::Length(LengthSize::Two)));
  // An `avcC` that fails part way: lengths, the size kept.
  let record = avcc(3);
  stream.reconfigure(&record[..record.len() - 1]);
  assert_eq!(stream.framing(), Some(Framing::Length(LengthSize::Two)));
  // An Annex B record: start codes, the size kept — two, so no probe.
  stream.reconfigure(&annex_b(&[avc_sps()]));
  assert_eq!(stream.framing(), Some(Framing::AnnexB));
  assert_eq!(stream.read(&annex_b(&[b_slice()])), Verdict::Disposable);
  // A failing `avcC` after it reads lengths again, of the size kept.
  stream.reconfigure(&record[..6]);
  assert_eq!(stream.framing(), Some(Framing::Length(LengthSize::Two)));
  assert_eq!(stream.read(&lengths(2, &[b_slice()])), Verdict::Disposable);

  // HEVC: an empty record moves nothing; one of start codes keeps the
  // size; an `hvcC` that fails part way stops at two.
  let mut hevc_stream = hevc_stream(&hvcc(0, 3, &[hevc_sps(0)]));
  hevc_stream.reconfigure(&[]);
  assert_eq!(
    hevc_stream.framing(),
    Some(Framing::Length(LengthSize::Four))
  );
  hevc_stream.reconfigure(&annex_b(&[hevc_sps(0)]));
  assert_eq!(hevc_stream.framing(), Some(Framing::AnnexB));
  let whole = hvcc(0, 3, &[hevc_sps(0)]);
  hevc_stream.reconfigure(&whole[..whole.len() - 1]);
  assert_eq!(
    hevc_stream.framing(),
    Some(Framing::Length(LengthSize::Two))
  );
}

#[test]
fn a_packet_that_is_an_avcc_configures_the_track_and_is_kept() {
  // `h264dec.c` 1045–1050: while lengths are read, a packet that is an
  // `avcC` is read as one, and holds no picture.
  let mut stream = h264(&avcc(3));
  assert_eq!(stream.read(&avcc(1)), kept(KeptReason::Configuration));
  assert_eq!(stream.framing(), Some(Framing::Length(LengthSize::Two)));
  assert_eq!(stream.read(&lengths(2, &[b_slice()])), Verdict::Disposable);
  // While start codes are read, the same bytes are no record.
  let mut annex = h264(&[]);
  assert_eq!(annex.read(&avcc(1)), kept(KeptReason::Malformed));
  assert_eq!(annex.framing(), Some(Framing::AnnexB));
}

#[test]
fn a_record_on_a_packet_of_no_bytes_is_read_by_hevc_and_not_by_h264() {
  // FFmpeg's H.264 decoder reads a packet of no bytes as the stream's end,
  // before the record it carries (`h264dec.c` 1034–1036).
  let mut stream = h264(&avcc(3));
  assert_eq!(
    stream.read_with_record(&[], Some(&avcc(1))),
    kept(KeptReason::NoPicture)
  );
  assert_eq!(stream.framing(), Some(Framing::Length(LengthSize::Four)));
  // On a packet of bytes the record comes first.
  assert_eq!(
    stream.read_with_record(&lengths(2, &[b_slice()]), Some(&avcc(1))),
    Verdict::Disposable
  );
  assert_eq!(stream.framing(), Some(Framing::Length(LengthSize::Two)));
  // HEVC's reads the record whatever the packet (`hevcdec.c` 3854–3860).
  let mut hevc_stream = hevc_stream(&hvcc(0, 3, &[hevc_sps(0)]));
  let _ = hevc_stream.read_with_record(&[], Some(&annex_b(&[hevc_sps(0)])));
  assert_eq!(hevc_stream.framing(), Some(Framing::AnnexB));
}

#[test]
fn a_seek_after_the_framing_moved_marks_nothing_again() {
  // Nothing moved: the framing is the same after the seek, and the
  // reading awaits a reference picture again.
  let mut stream = h264(&avcc(3));
  stream.seeked();
  assert_eq!(
    stream.read(&lengths(4, &[b_slice()])),
    kept(KeptReason::AwaitingReference)
  );
  assert_eq!(
    stream.read(&lengths(4, &[p_slice()])),
    kept(KeptReason::Referenced)
  );
  assert_eq!(stream.read(&lengths(4, &[b_slice()])), Verdict::Disposable);
  // Moved, then sought: a decoder that kept its framing and one opened
  // afresh read the next packet otherwise.
  stream.reconfigure(&annex_b(&[avc_sps()]));
  stream.seeked();
  assert_eq!(stream.framing(), None);
  assert_eq!(
    stream.read(&lengths(4, &[b_slice()])),
    kept(KeptReason::FramingUnknown)
  );
  assert_eq!(
    stream.read(&annex_b(&[b_slice()])),
    kept(KeptReason::FramingUnknown)
  );
  // A record after it does not bring it back.
  stream.reconfigure(&avcc(3));
  assert_eq!(
    stream.read(&lengths(4, &[b_slice()])),
    kept(KeptReason::FramingUnknown)
  );
  // Moved and moved back before the seek: the framing is the one the
  // stream opened in, which both decoders read.
  let mut back = h264(&avcc(3));
  back.reconfigure(&annex_b(&[avc_sps()]));
  back.reconfigure(&avcc(3));
  back.seeked();
  assert_eq!(
    back.read(&lengths(4, &[p_slice()])),
    kept(KeptReason::Referenced)
  );
  assert_eq!(back.read(&lengths(4, &[b_slice()])), Verdict::Disposable);
}

#[test]
fn a_parameter_set_of_21802_bytes_or_more_leaves_the_size_unknown() {
  // `h264_parse.c` 436–437: such a parameter set that does not decode
  // ends the record with `ERANGE`, the size where it was.
  let big = |length: usize| {
    let mut sps = avc_sps();
    sps.resize(length, 0x11);
    sps
  };
  // 21,800 bytes and its two-byte length: 21,802.
  let mut stream = h264(&avcc_of(3, &big(21_800)));
  assert_eq!(stream.framing(), None);
  assert_eq!(
    stream.read(&lengths(4, &[b_slice()])),
    kept(KeptReason::FramingUnknown)
  );
  // A record read whole sets it again.
  stream.reconfigure(&avcc(3));
  assert_eq!(stream.read(&lengths(4, &[b_slice()])), Verdict::Disposable);
  // A size it would set and already is stays known.
  stream.reconfigure(&avcc_of(3, &big(21_800)));
  assert_eq!(stream.framing(), Some(Framing::Length(LengthSize::Four)));
  // One byte less is read whole.
  let whole = h264(&avcc_of(3, &big(21_799)));
  assert_eq!(whole.framing(), Some(Framing::Length(LengthSize::Four)));
}

#[test]
fn a_mid_stream_hvcc_of_other_than_two_byte_lengths_marks_nothing_again() {
  // At the opening an `hvcC` is read whole or leaves no decoder.
  let mut stream = hevc_stream(&hvcc(0, 3, &[hevc_sps(0)]));
  assert_eq!(stream.framing(), Some(Framing::Length(LengthSize::Four)));
  // Mid-stream, one of two-byte lengths is two whether or not its
  // parameter sets decode.
  stream.reconfigure(&hvcc(0, 1, &[hevc_sps(0)]));
  assert_eq!(stream.framing(), Some(Framing::Length(LengthSize::Two)));
  assert_eq!(
    stream.read(&lengths(2, &[hevc(0, 0, 0, &[0xAF])])),
    Verdict::Disposable
  );
  // One of four is two under `AV_EF_EXPLODE` if a parameter set does not
  // decode (`hevc/parse.c` 72–76, 121–129).
  stream.reconfigure(&hvcc(0, 3, &[hevc_sps(0)]));
  assert_eq!(stream.framing(), None);
  assert_eq!(
    stream.read(&lengths(4, &[hevc(0, 0, 0, &[0xAF])])),
    kept(KeptReason::FramingUnknown)
  );
  stream.reconfigure(&annex_b(&[hevc_sps(0)]));
  assert_eq!(stream.framing(), None);
}

#[test]
fn an_hevc_slnr_picture_is_disposable_only_at_the_highest_sub_layer() {
  // Three sub-layers, from the SPS in the record.
  let mut stream = hevc_stream(&hvcc(0, 3, &[hevc_sps(2)]));
  assert_eq!(stream.highest_sub_layer(), Some(2));
  let read = |stream: &mut NalStream, nals: &[Vec<u8>]| stream.read(&lengths(4, nals));
  let aud = hevc(35, 0, 2, &[0x50]);
  let sei = hevc_sei(39, 2, &[filler()]);
  for nal_unit_type in [0, 2, 4, 6, 8] {
    assert_eq!(
      read(
        &mut stream,
        &[aud.clone(), sei.clone(), hevc(nal_unit_type, 0, 2, &[0xAF])]
      ),
      Verdict::Disposable,
      "{nal_unit_type}"
    );
    // Below it, a higher sub-layer may reference the picture.
    for below in [0, 1] {
      assert_eq!(
        read(&mut stream, &[hevc(nal_unit_type, 0, below, &[0xAF])]),
        kept(KeptReason::BelowHighestSubLayer),
        "{nal_unit_type} at {below}"
      );
    }
  }
  // The sub-layer reference types, and IRAP.
  for nal_unit_type in [1, 3, 5, 7, 9, 11, 13, 15] {
    assert_eq!(
      read(&mut stream, &[hevc(nal_unit_type, 0, 2, &[0xAF])]),
      kept(KeptReason::Referenced),
      "{nal_unit_type}"
    );
  }
  for nal_unit_type in 16..=23 {
    assert_eq!(
      read(&mut stream, &[hevc(nal_unit_type, 0, 0, &[0xAF])]),
      kept(KeptReason::RandomAccess),
      "{nal_unit_type}"
    );
  }
  // A TemporalId of -1, and one past every declared sub-layer — which
  // raises the highest to it.
  assert_eq!(
    read(&mut stream, &[vec![0x00, 0x00, 0xAF]]),
    kept(KeptReason::Malformed)
  );
  assert_eq!(
    read(&mut stream, &[hevc(0, 0, 3, &[0xAF])]),
    kept(KeptReason::Malformed)
  );
  assert_eq!(stream.highest_sub_layer(), Some(3));
  assert_eq!(
    read(&mut stream, &[hevc(0, 0, 2, &[0xAF])]),
    kept(KeptReason::BelowHighestSubLayer)
  );
}

#[test]
fn a_unit_of_a_sub_layer_no_parameter_set_declared_raises_the_highest() {
  let mut stream = hevc_stream(&hvcc(0, 3, &[hevc_sps(0)]));
  assert_eq!(stream.highest_sub_layer(), Some(0));
  // A reference slice at TemporalId 2: kept as one, and learned from.
  assert_eq!(
    stream.read(&lengths(4, &[hevc(1, 0, 2, &[0xAF])])),
    kept(KeptReason::Referenced)
  );
  assert_eq!(stream.highest_sub_layer(), Some(2));
  assert_eq!(
    stream.read(&lengths(4, &[hevc(0, 0, 0, &[0xAF])])),
    kept(KeptReason::BelowHighestSubLayer)
  );
}

#[test]
fn a_single_sub_layer_stream_marks_its_slnr_pictures() {
  let mut stream = hevc_stream(&hvcc(1, 3, &[hevc_sps(0)]));
  assert_eq!(stream.highest_sub_layer(), Some(0));
  assert_eq!(
    stream.read(&lengths(4, &[hevc(0, 0, 0, &[0xAF])])),
    Verdict::Disposable
  );
}

#[test]
fn the_highest_sub_layer_is_learned_from_every_parameter_set_and_only_rises() {
  // No SPS yet: nothing is disposable, whatever the record's
  // `numTemporalLayers` says.
  let mut stream = hevc_stream(&hvcc(1, 3, &[]));
  assert_eq!(stream.highest_sub_layer(), None);
  assert_eq!(
    stream.read(&lengths(4, &[hevc(0, 0, 0, &[0xAF])])),
    kept(KeptReason::SubLayersUnknown)
  );
  // An in-band SPS of one sub-layer: the packet carrying it is kept, the
  // next is read.
  assert_eq!(
    stream.read(&lengths(4, &[hevc_sps(0), hevc(0, 0, 0, &[0xAF])])),
    kept(KeptReason::Companion(33))
  );
  assert_eq!(stream.highest_sub_layer(), Some(0));
  assert_eq!(
    stream.read(&lengths(4, &[hevc(0, 0, 0, &[0xAF])])),
    Verdict::Disposable
  );
  // A VPS of three raises it; a later SPS of one does not lower it.
  let _ = stream.read(&lengths(4, &[hevc_vps(2)]));
  assert_eq!(stream.highest_sub_layer(), Some(2));
  let _ = stream.read(&lengths(4, &[hevc_sps(0)]));
  assert_eq!(stream.highest_sub_layer(), Some(2));
  assert_eq!(
    stream.read(&lengths(4, &[hevc(0, 0, 0, &[0xAF])])),
    kept(KeptReason::BelowHighestSubLayer)
  );
  // The record's `numTemporalLayers` raises it too, once an SPS is known.
  let mut record = hevc_stream(&hvcc(4, 3, &[hevc_sps(0)]));
  assert_eq!(record.highest_sub_layer(), Some(3));
  assert_eq!(
    record.read(&lengths(4, &[hevc(0, 0, 0, &[0xAF])])),
    kept(KeptReason::BelowHighestSubLayer)
  );
  // A field past 6 does not parse, and teaches nothing.
  assert_eq!(
    stream.read(&lengths(4, &[hevc_sps(7)])),
    kept(KeptReason::Malformed)
  );
  assert_eq!(stream.highest_sub_layer(), Some(2));
}

#[test]
fn every_parameter_set_ffmpegs_splitter_may_read_is_learned() {
  // A malformed unit before a VPS of three: the packet does not parse, and
  // the VPS is read, as FFmpeg's splitter passes over the unit
  // (`h2645_parse.c` 646–660).
  let mut stream = hevc_stream(&hvcc(0, 3, &[hevc_sps(0)]));
  assert_eq!(
    stream.read(&lengths(4, &[vec![0x80, 0x01], hevc_vps(2)])),
    kept(KeptReason::Malformed)
  );
  assert_eq!(stream.highest_sub_layer(), Some(2));
  // A start code within a length-framed unit: the splitter reads the
  // bytes after it as a unit of its own (`h2645_parse.c` 44–52, 559–584).
  let mut within = hevc_stream(&hvcc(0, 3, &[hevc_sps(0)]));
  let mut unit = hevc(0, 0, 0, &[0xAF, 0, 0, 1]);
  unit.extend_from_slice(&hevc_sps(2));
  assert_eq!(
    within.read(&lengths(4, &[unit])),
    kept(KeptReason::Malformed)
  );
  assert_eq!(within.highest_sub_layer(), Some(2));
  // Annex B: bytes before the first start code, which the splitter passes
  // over to the units after it (`h2645_parse.c` 565–580).
  let mut annex = hevc_stream(&annex_b(&[hevc_sps(0)]));
  let mut packet = vec![7];
  packet.extend_from_slice(&annex_b(&[hevc_vps(2), hevc(0, 0, 0, &[0xAF])]));
  assert_eq!(annex.read(&packet), kept(KeptReason::Malformed));
  assert_eq!(annex.highest_sub_layer(), Some(2));
  // And a record of start codes holding bytes that frame nothing.
  let mut extradata = vec![9, 9];
  extradata.extend_from_slice(&annex_b(&[hevc_sps(1)]));
  let record = hevc_stream(&extradata);
  assert_eq!(record.highest_sub_layer(), Some(1));
}

#[test]
fn hevc_annex_b_extradata_and_in_band_sets_are_read() {
  let extradata = annex_b(&[hevc_vps(0), hevc_sps(1), hevc(34, 0, 0, &[0xC1])]);
  let mut stream = hevc_stream(&extradata);
  assert_eq!(stream.framing(), Some(Framing::AnnexB));
  assert_eq!(stream.highest_sub_layer(), Some(1));
  assert_eq!(
    stream.read(&annex_b(&[hevc(35, 0, 1, &[0x50]), hevc(0, 0, 1, &[0xAF])])),
    Verdict::Disposable
  );
}

#[test]
fn an_hevc_layer_above_0_or_a_companion_unit_keeps_the_packet() {
  // Each on a reading of its own: an end of sequence, an SEI unit or an
  // RPU moves what the next packet is held to.
  let fresh = || hevc_stream(&hvcc(0, 3, &[hevc_sps(0)]));
  let slnr = hevc(0, 0, 0, &[0xAF]);
  assert_eq!(
    fresh().read(&lengths(4, &[slnr.clone(), hevc(0, 1, 0, &[0xAF])])),
    kept(KeptReason::Extension)
  );
  for nal_unit_type in [32, 33, 34, 36, 37, 39, 40, 24, 31, 41, 47, 48, 62, 63] {
    let unit = match nal_unit_type {
      32 => hevc_vps(0),
      33 => hevc_sps(0),
      other => hevc(other, 0, 0, &[0x42, 0x42]),
    };
    assert_eq!(
      fresh().read(&lengths(4, &[slnr.clone(), unit])),
      kept(KeptReason::Companion(nal_unit_type)),
      "{nal_unit_type}"
    );
  }
  for beside in [35, 38] {
    assert_eq!(
      fresh().read(&lengths(4, &[hevc(beside, 0, 0, &[0x50]), slnr.clone()])),
      Verdict::Disposable,
      "{beside}"
    );
  }
}

#[test]
fn an_hevc_sei_unit_rides_only_where_every_message_is_filler() {
  let mut stream = hevc_stream(&hvcc(0, 3, &[hevc_sps(0)]));
  let slnr = hevc(0, 0, 0, &[0xAF]);
  // A prefix unit of filler before the picture and a suffix one after it.
  assert_eq!(
    stream.read(&lengths(
      4,
      &[
        hevc_sei(39, 0, &[filler()]),
        slnr.clone(),
        hevc_sei(40, 0, &[filler(), filler()])
      ]
    )),
    Verdict::Disposable
  );
  // The picture's hash keeps its packet: FFmpeg clears it only at a frame's
  // end, so beside a picture its decoder skips it carries over to the
  // next. Sixteen zero bytes, so emulation prevention bytes in it.
  assert!(
    hevc_sei(40, 0, &[picture_hash()])
      .windows(3)
      .any(|w| w == [0, 0, 3])
  );
  assert_eq!(
    stream.read(&lengths(
      4,
      &[slnr.clone(), hevc_sei(40, 0, &[picture_hash()])]
    )),
    kept(KeptReason::Companion(40))
  );
  // Every other message keeps the packet — picture timing among them, whose
  // field structure FFmpeg keeps for later pictures (`hevc/refs.c`
  // 183–187).
  // A prefix unit before its picture, a suffix one after it.
  for payload_type in [0, 1, 4, 5, 6, 129, 132, 136, 137, 144, 147] {
    for nal_unit_type in [39, 40] {
      let sei = hevc_sei(
        nal_unit_type,
        0,
        &[filler(), sei_message(payload_type, &[0x42])],
      );
      let nals = if nal_unit_type == 39 {
        [sei, slnr.clone()]
      } else {
        [slnr.clone(), sei]
      };
      let mut stream = hevc_stream(&hvcc(0, 3, &[hevc_sps(0)]));
      assert_eq!(
        stream.read(&lengths(4, &nals)),
        kept(KeptReason::Companion(nal_unit_type)),
        "{payload_type} in {nal_unit_type}"
      );
    }
  }
}

#[test]
fn an_hevc_record_is_detected_as_ffmpeg_detects_it() {
  // At least 23 bytes, its first byte 1 — or 0, with a nonzero second byte
  // or a third above 1 — else Annex B.
  let sps = [hevc_sps(0)];
  assert_eq!(
    hevc_stream(&hvcc(0, 1, &sps)).framing(),
    Some(Framing::Length(LengthSize::Two))
  );
  let mut legacy = hvcc(0, 0, &sps);
  legacy[0] = 0;
  legacy[1] = 1;
  assert_eq!(
    hevc_stream(&legacy).framing(),
    Some(Framing::Length(LengthSize::One))
  );
  legacy[1] = 0;
  legacy[2] = 1;
  assert_eq!(hevc_stream(&legacy).framing(), Some(Framing::AnnexB));
  // A length of three bytes, read as FFmpeg reads it; and a record that
  // ends inside an array, which stops at two (the decoder then does not
  // open, `hevcdec.c` 4169–4172).
  assert_eq!(
    hevc_stream(&hvcc(0, 2, &sps)).framing(),
    Some(Framing::Length(LengthSize::Three))
  );
  let whole = hvcc(0, 3, &sps);
  assert_eq!(
    hevc_stream(&whole[..whole.len() - 1]).framing(),
    Some(Framing::Length(LengthSize::Two))
  );
}

#[test]
fn an_h264_packet_is_marked_only_while_every_sps_codes_frames_only() {
  // Before any sequence parameter set, nothing.
  let mut stream = h264(&[]);
  assert_eq!(stream.frames_only(), None);
  assert_eq!(
    stream.read(&annex_b(&[b_slice()])),
    kept(KeptReason::MaybeField)
  );
  // One of frames only, in-band: its packet kept, the next marked.
  assert_eq!(
    stream.read(&annex_b(&[avc_sps(), b_slice()])),
    kept(KeptReason::Companion(7))
  );
  assert_eq!(stream.frames_only(), Some(true));
  assert_eq!(stream.read(&annex_b(&[b_slice()])), Verdict::Disposable);
  // One that lets fields in, mid-stream: nothing from that packet on, and a
  // later one of frames only does not bring marking back.
  let interlaced = Sps {
    frames_only: false,
    ..Sps::HIGH
  };
  let _ = stream.read(&annex_b(&[interlaced.unit()]));
  assert_eq!(stream.frames_only(), Some(false));
  assert_eq!(
    stream.read(&annex_b(&[b_slice()])),
    kept(KeptReason::MaybeField)
  );
  let _ = stream.read(&annex_b(&[avc_sps()]));
  assert_eq!(
    stream.read(&annex_b(&[b_slice()])),
    kept(KeptReason::MaybeField)
  );
  // A record's: an `avcC` of a PAFF/MBAFF-capable SPS marks nothing, and
  // reference and IDR slices keep their own reasons.
  let mut record = h264(&avcc_of(3, &interlaced.unit()));
  assert_eq!(record.frames_only(), Some(false));
  assert_eq!(
    record.read(&lengths(4, &[b_slice()])),
    kept(KeptReason::MaybeField)
  );
  assert_eq!(
    record.read(&lengths(4, &[p_slice()])),
    kept(KeptReason::Referenced)
  );
  assert_eq!(
    record.read(&lengths(4, &[avc(0, 5, &[0x88])])),
    kept(KeptReason::RandomAccess)
  );
  // An SPS in the record's picture parameter set array: FFmpeg reads each
  // unit of either array as its own type says (`h264_parse.c` 501–513).
  let progressive = avc_sps();
  let mut mixed = vec![1, 0x64, 0x00, 0x28, 0xFF, 0xE1];
  mixed.extend_from_slice(&(progressive.len() as u16).to_be_bytes());
  mixed.extend_from_slice(&progressive);
  mixed.push(1);
  mixed.extend_from_slice(&(interlaced.unit().len() as u16).to_be_bytes());
  mixed.extend_from_slice(&interlaced.unit());
  assert_eq!(h264(&mixed).frames_only(), Some(false));
  assert_eq!(h264(&avcc_of(3, &progressive)).frames_only(), Some(true));
}

#[test]
fn an_sps_codes_frames_only_only_where_every_reading_ffmpeg_makes_proves_it() {
  let reads = |unit: &[u8]| h264(&avcc_of(3, unit)).frames_only();
  // Baseline and Main, and High through its scaling matrix, at 4:2:0 and
  // 4:4:4.
  assert_eq!(reads(&Sps::BASELINE.unit()), Some(true));
  assert_eq!(
    reads(
      &Sps {
        profile_idc: 77,
        ..Sps::BASELINE
      }
      .unit()
    ),
    Some(true)
  );
  assert_eq!(reads(&Sps::HIGH.unit()), Some(true));
  assert_eq!(
    reads(
      &Sps {
        chroma_format_idc: 3,
        ..Sps::HIGH
      }
      .unit()
    ),
    Some(true)
  );
  // Fields, and an SPS cut before its flag.
  assert_eq!(
    reads(
      &Sps {
        frames_only: false,
        ..Sps::BASELINE
      }
      .unit()
    ),
    Some(false)
  );
  let whole = Sps::BASELINE.unit();
  assert_eq!(reads(&whole[..5]), Some(false));
  // What FFmpeg refuses (`h264_ps.c` 319–445): separate colour planes,
  // unequal or out-of-range bit depths, a frame number of 17 bits, a
  // fourth order-count type, 17 reference frames.
  for refused in [
    Sps {
      chroma_format_idc: 3,
      separate_colour_planes: true,
      ..Sps::HIGH
    },
    Sps {
      chroma_minus8: 2,
      ..Sps::HIGH
    },
    Sps {
      luma_minus8: 7,
      chroma_minus8: 7,
      ..Sps::HIGH
    },
    Sps {
      log2_max_frame_num_minus4: 13,
      ..Sps::BASELINE
    },
    Sps {
      pic_order_cnt_type: 3,
      ..Sps::BASELINE
    },
    Sps {
      max_num_ref_frames: 17,
      ..Sps::BASELINE
    },
  ] {
    assert_eq!(reads(&refused.unit()), Some(false));
  }
  // An emulation prevention byte before the flag: a width whose code has 22
  // leading zeros puts `00 00` before a byte of 3 or less. Its RBSP codes
  // frames only; its bytes as they stand, which FFmpeg reads where the
  // RBSP fails, read otherwise, so it is no proof.
  let wide = Sps {
    width_in_mbs_minus1: 0x3F_FFFF,
    ..Sps::BASELINE
  }
  .unit();
  assert!(wide.windows(3).any(|w| w == [0, 0, 3]), "{wide:x?}");
  assert_eq!(
    sps_facts(BitReader::new(&wide[1..], Reading::Rbsp)).frames_only,
    Some(true)
  );
  assert_ne!(
    sps_facts(BitReader::new(&wide[1..], Reading::Bytes)).frames_only,
    Some(true)
  );
  assert_eq!(reads(&wide), Some(false));
}

#[test]
fn the_reescaped_reading_writes_what_decode_extradata_ps_mp4_writes() {
  // `h264_parse.c` 446–453: where three bytes are left and they read
  // `00 00 0x`, x at most 3, `00 00 03` is written for the two zeros.
  let read = |bytes: &[u8]| {
    let mut bits = BitReader::whole(bytes, Reading::Reescaped);
    let mut out = Vec::new();
    while let Some(byte) = bits.bits(8) {
      out.push(byte as u8);
    }
    out
  };
  let cases: [(&[u8], &[u8]); 7] = [
    (&[0, 0, 3, 0x80], &[0, 0, 3, 3, 0x80]),
    (&[0, 0, 0, 1], &[0, 0, 3, 0, 1]),
    (&[0, 0, 0, 0, 1], &[0, 0, 3, 0, 0, 3, 1]),
    (&[5, 0, 0, 2], &[5, 0, 0, 3, 2]),
    (&[0, 0, 4], &[0, 0, 4]),
    (&[0, 0], &[0, 0]),
    (&[], &[]),
  ];
  for (bytes, reescaped) in cases {
    assert_eq!(read(bytes), reescaped, "{bytes:x?}");
    // Its RBSP is the bytes again, as FFmpeg means it to be.
    let mut rbsp = BitReader::whole(reescaped, Reading::Rbsp);
    let mut back = Vec::new();
    while let Some(byte) = rbsp.bits(8) {
      back.push(byte as u8);
    }
    assert_eq!(back, bytes, "{bytes:x?}");
  }
}

#[test]
fn an_sps_is_no_proof_where_the_bytes_ffmpeg_escapes_it_to_read_otherwise() {
  // `decode_extradata_ps_mp4` escapes an `avcC` entry it cannot read and
  // reads it again (`h264_parse.c` 421–464): the RBSP of that is the
  // bytes, and its bytes are a third reading. A height whose code has 22
  // leading zeros puts `00 00 03` in the RBSP, escaped `00 00 03 03`: as
  // the RBSP and as the bytes the flag reads 1, as the bytes FFmpeg
  // escapes it to, 0.
  let unit = Sps {
    pic_order_cnt_type: 2,
    max_num_ref_frames: 0,
    width_in_mbs_minus1: 0,
    height_in_map_units_minus1: 8_355_838,
    vui: Vui::Absent,
    ..Sps::BASELINE
  }
  .unit();
  assert_eq!(
    unit,
    [
      0x67, 0x42, 0, 0x28, 0xDD, 0, 0, 3, 3, 0xFB, 0xFF, 0xFE, 0x40
    ]
  );
  let payload = &unit[1..];
  let reads = |reading: Reading| sps_facts(BitReader::new(payload, reading)).frames_only;
  assert_eq!(reads(Reading::Rbsp), Some(true));
  assert_eq!(reads(Reading::Bytes), Some(true));
  assert_eq!(reads(Reading::Reescaped), Some(false));
  assert!(!sps_proof(payload).frames_only);
  let stream = h264(&avcc_of(3, &unit));
  assert_eq!(stream.frames_only(), Some(false));
}

#[test]
fn a_packet_the_reading_cannot_frame_still_teaches_its_sequence_parameter_sets() {
  let interlaced = Sps {
    frames_only: false,
    ..Sps::HIGH
  }
  .unit();
  // A packet that switches the framing: the decoder reads it in the one it
  // switches to.
  let mut switched = h264(&avcc(3));
  assert_eq!(
    switched.read(&annex_b(&[interlaced.clone(), b_slice()])),
    kept(KeptReason::FramingChange)
  );
  assert_eq!(switched.frames_only(), Some(false));
  assert_eq!(
    switched.read(&annex_b(&[b_slice()])),
    kept(KeptReason::MaybeField)
  );
  // While the size is unknown (a parameter set of 21,802 bytes, `ERANGE`),
  // in every framing a decoder may read the packet in; a record read whole
  // after it does not unlearn it.
  let mut big = avc_sps();
  big.resize(21_800, 0x11);
  for packet in [
    annex_b(&[interlaced.clone()]),
    lengths(1, &[interlaced.clone()]),
    lengths(2, &[interlaced.clone()]),
    lengths(3, &[interlaced.clone()]),
    lengths(4, &[interlaced.clone()]),
  ] {
    let mut unknown = h264(&avcc_of(3, &big));
    assert_eq!(unknown.framing(), None);
    assert_eq!(unknown.frames_only(), Some(true));
    assert_eq!(unknown.read(&packet), kept(KeptReason::FramingUnknown));
    assert_eq!(unknown.frames_only(), Some(false), "{packet:x?}");
    unknown.reconfigure(&avcc(3));
    assert_eq!(
      unknown.read(&lengths(4, &[b_slice()])),
      kept(KeptReason::MaybeField)
    );
  }
  // Unframed: a record that ends before its parameter sets do.
  let record = avcc(3);
  let mut short = h264(&record[..record.len() - 1]);
  assert_eq!(
    short.read(&lengths(4, &[interlaced.clone()])),
    kept(KeptReason::Unframed)
  );
  assert_eq!(short.frames_only(), Some(false));
  short.reconfigure(&avcc(3));
  assert_eq!(
    short.read(&lengths(4, &[b_slice()])),
    kept(KeptReason::MaybeField)
  );
}

#[test]
fn a_reserved_slnr_type_keeps_its_packet() {
  // FFmpeg's decoder skips RSV_VCL_N10, N12 and N14 without a picture.
  let mut stream = hevc_stream(&hvcc(0, 3, &[hevc_sps(0)]));
  for reserved in [10, 12, 14] {
    assert_eq!(
      stream.read(&lengths(4, &[hevc(reserved, 0, 0, &[0xAF])])),
      kept(KeptReason::Companion(reserved)),
      "{reserved}"
    );
    assert_eq!(
      stream.read(&lengths(
        4,
        &[hevc(0, 0, 0, &[0xAF]), hevc(reserved, 0, 0, &[0xAF])]
      )),
      kept(KeptReason::Companion(reserved)),
      "{reserved} beside a picture"
    );
  }
}

#[test]
fn an_h264_picture_of_intra_slices_alone_is_kept() {
  // FFmpeg's decoder, under frame threading, resets the `poc_offset` later
  // pictures' film grain is seeded from at a picture of intra slices alone
  // (`h264dec.c` 501–551, `h264_slice.c` 1427–1430).
  let mut stream = h264(&avcc(3));
  let mut read = |nals: &[Vec<u8>]| stream.read(&lengths(4, nals));
  let slice = |payload: &[u8]| avc(0, 1, payload);
  // I and SI, of either range: `slice_type` 2, 7, 4 and 9.
  for payload in [
    &[0xB4, 0x84, 0x21][..],
    &[0x88, 0x84, 0x21][..],
    &[0x94, 0x84, 0x21][..],
    &[0x8A, 0x84, 0x21][..],
  ] {
    assert_eq!(
      read(&[slice(payload)]),
      kept(KeptReason::IntraOnly),
      "{payload:x?}"
    );
  }
  // P, B, SP of either range, and past 9, which FFmpeg reads as P: 0, 1,
  // 3, 8, 10 and 30.
  for payload in [
    &[0xC0, 0x84][..],
    &[0xA0, 0x84][..],
    &[0x90, 0x84][..],
    &[0x89, 0x84, 0x21][..],
    &[0x8B, 0x84, 0x21][..],
    &[0x87, 0xE0][..],
  ] {
    assert_eq!(read(&[slice(payload)]), Verdict::Disposable, "{payload:x?}");
  }
  // A picture of intra and inter slices.
  assert_eq!(read(&[i_slice(), b_slice()]), Verdict::Disposable);
  // A header that does not read counts as intra.
  assert_eq!(read(&[slice(&[])]), kept(KeptReason::IntraOnly));
  assert_eq!(read(&[slice(&[]), b_slice()]), Verdict::Disposable);
  // An intra slice data partition A counts as FFmpeg counts it; beside no
  // slice the decoder decodes, the decoder starts no picture to reset.
  assert_eq!(
    read(&[avc(0, 2, &[0x88]), i_slice()]),
    kept(KeptReason::IntraOnly)
  );
  assert_eq!(
    read(&[avc(0, 2, &[0x88]), avc(0, 3, &[0x42])]),
    Verdict::Disposable
  );
}

#[test]
fn an_h264_sps_that_lets_frame_num_jump_marks_nothing() {
  // FFmpeg fills a gap in `frame_num` counting from the picture before,
  // reference or not (`h264_slice.c` 1432–1530, `h264_picture.c` 202–203).
  let gaps = Sps {
    gaps: true,
    ..Sps::BASELINE
  }
  .unit();
  let mut record = h264(&avcc_of(3, &gaps));
  assert_eq!(
    record.read(&lengths(4, &[b_slice()])),
    kept(KeptReason::FrameNumGaps)
  );
  // In-band, for good.
  let mut stream = h264(&avcc(3));
  assert_eq!(stream.read(&lengths(4, &[b_slice()])), Verdict::Disposable);
  assert_eq!(
    stream.read(&lengths(4, &[gaps])),
    kept(KeptReason::Companion(7))
  );
  assert_eq!(
    stream.read(&lengths(4, &[b_slice()])),
    kept(KeptReason::FrameNumGaps)
  );
  let _ = stream.read(&lengths(4, &[avc_sps()]));
  assert_eq!(
    stream.read(&lengths(4, &[b_slice()])),
    kept(KeptReason::FrameNumGaps)
  );
}

#[test]
fn an_h264_packet_is_marked_only_where_every_sps_fixes_the_reorder_depth_at_15_or_less() {
  let reads = |vui: Vui| {
    let unit = Sps {
      vui,
      ..Sps::BASELINE
    }
    .unit();
    h264(&avcc_of(3, &unit)).read(&lengths(4, &[b_slice()]))
  };
  // No VUI, a VUI without the restriction, a depth of 16 — where a picture
  // may follow 16 it precedes in output order and FFmpeg resets its record
  // of them (`h264_slice.c` 1310–1328) — and one of 17, which FFmpeg
  // refuses.
  for vui in [
    Vui::Absent,
    Vui::Unrestricted,
    Vui::Restricted(16),
    Vui::Restricted(17),
  ] {
    assert_eq!(reads(vui), kept(KeptReason::ReorderUnfixed));
  }
  // Depths of 0 and 15, and a restriction past every field before it.
  for vui in [
    Vui::Restricted(0),
    Vui::Restricted(15),
    Vui::RestrictedAfterAll,
  ] {
    assert_eq!(reads(vui), Verdict::Disposable);
  }
  // A restriction the unit ends inside.
  let whole = Sps::BASELINE.unit();
  let cut = &whole[..whole.len() - 2];
  assert_eq!(
    h264(&avcc_of(3, cut)).read(&lengths(4, &[b_slice()])),
    kept(KeptReason::ReorderUnfixed)
  );
  // In-band, for good.
  let mut stream = h264(&avcc(3));
  let unrestricted = Sps {
    vui: Vui::Unrestricted,
    ..Sps::BASELINE
  }
  .unit();
  let _ = stream.read(&lengths(4, &[unrestricted]));
  let _ = stream.read(&lengths(4, &[avc_sps()]));
  assert_eq!(
    stream.read(&lengths(4, &[b_slice()])),
    kept(KeptReason::ReorderUnfixed)
  );
}

#[test]
fn an_h264_reading_awaits_a_reference_picture_after_its_opening_and_every_seek() {
  // FFmpeg's decoder takes its picture order count up from the first
  // picture it decodes after it opens or is flushed (`h264_parse.c`
  // 293–294).
  let mut stream = NalStream::new(NalCodec::H264, &avcc(3));
  let read = |stream: &mut NalStream, nals: &[Vec<u8>]| stream.read(&lengths(4, nals));
  assert_eq!(
    read(&mut stream, &[b_slice()]),
    kept(KeptReason::AwaitingReference)
  );
  // A slice data partition of a reference picture does not end the wait:
  // the decoder passes over partitions (`h264dec.c` 682–686).
  assert_eq!(
    read(&mut stream, &[avc(2, 2, &[0x9A])]),
    kept(KeptReason::Referenced)
  );
  assert_eq!(
    read(&mut stream, &[b_slice()]),
    kept(KeptReason::AwaitingReference)
  );
  // A reference slice does.
  assert_eq!(
    read(&mut stream, &[p_slice()]),
    kept(KeptReason::Referenced)
  );
  assert_eq!(read(&mut stream, &[b_slice()]), Verdict::Disposable);
  // A seek starts the wait again, and an IDR slice ends it.
  stream.seeked();
  assert_eq!(
    read(&mut stream, &[b_slice()]),
    kept(KeptReason::AwaitingReference)
  );
  assert_eq!(
    read(&mut stream, &[avc(3, 5, &[0x88])]),
    kept(KeptReason::RandomAccess)
  );
  assert_eq!(read(&mut stream, &[b_slice()]), Verdict::Disposable);
}

#[test]
fn an_intra_slice_waits_for_one_in_a_packet_kept_since_the_opening_or_a_seek() {
  // FFmpeg's first intra slice after a flush stops later pictures
  // referencing the gray frames it fills gaps with (`h264_slice.c`
  // 2032–2034, `h264_refs.c` 410–419, `h264dec.c` 492).
  let mut stream = NalStream::new(NalCodec::H264, &avcc(3));
  let read = |stream: &mut NalStream, nals: &[Vec<u8>]| stream.read(&lengths(4, nals));
  // A reference picture of P slices: the wait for a reference ends, the
  // one for an intra slice does not.
  assert_eq!(
    read(&mut stream, &[p_slice()]),
    kept(KeptReason::Referenced)
  );
  assert_eq!(read(&mut stream, &[b_slice()]), Verdict::Disposable);
  assert_eq!(
    read(&mut stream, &[i_slice(), b_slice()]),
    kept(KeptReason::AwaitingIntraSlice)
  );
  // That packet, kept, held one.
  assert_eq!(
    read(&mut stream, &[i_slice(), b_slice()]),
    Verdict::Disposable
  );
  // After a seek: a header that does not read may be intra, and is no
  // intra slice kept.
  stream.seeked();
  assert_eq!(
    read(&mut stream, &[p_slice()]),
    kept(KeptReason::Referenced)
  );
  assert_eq!(
    read(&mut stream, &[avc(0, 1, &[]), b_slice()]),
    kept(KeptReason::AwaitingIntraSlice)
  );
  assert_eq!(
    read(&mut stream, &[avc(0, 1, &[])]),
    kept(KeptReason::IntraOnly)
  );
  assert_eq!(
    read(&mut stream, &[i_slice(), b_slice()]),
    kept(KeptReason::AwaitingIntraSlice)
  );
  assert_eq!(
    read(&mut stream, &[i_slice(), b_slice()]),
    Verdict::Disposable
  );
  // An IDR picture's slices are intra.
  stream.seeked();
  assert_eq!(
    read(&mut stream, &[avc(3, 5, &[0x88])]),
    kept(KeptReason::RandomAccess)
  );
  assert_eq!(
    read(&mut stream, &[i_slice(), b_slice()]),
    Verdict::Disposable
  );
}

#[test]
fn the_first_hevc_packet_after_the_opening_a_seek_or_an_end_of_sequence_is_kept() {
  // FFmpeg's HEVC decoder takes its `eos` flag up with whatever packet it
  // reads next, and the flag decides whether the next CRA picture starts a
  // sequence (`hevc/hevcdec.c` 3687–3720, 3283–3287).
  let slnr = || lengths(4, &[hevc(0, 0, 0, &[0xAF])]);
  let mut stream = NalStream::new(NalCodec::Hevc, &hvcc(0, 3, &[hevc_sps(0)]));
  // A packet of no bytes is none the decoder reads.
  assert_eq!(stream.read(&[]), kept(KeptReason::NoPicture));
  assert_eq!(stream.read(&slnr()), kept(KeptReason::SequenceStart));
  assert_eq!(stream.read(&slnr()), Verdict::Disposable);
  // A flush sets it again.
  stream.seeked();
  assert_eq!(stream.read(&slnr()), kept(KeptReason::SequenceStart));
  assert_eq!(stream.read(&slnr()), Verdict::Disposable);
  // So does an end of sequence or of bitstream, for the packet after it.
  for end in [36, 37] {
    assert_eq!(
      stream.read(&lengths(4, &[hevc(0, 0, 0, &[0xAF]), hevc(end, 0, 0, &[])])),
      kept(KeptReason::Companion(end))
    );
    assert_eq!(
      stream.read(&slnr()),
      kept(KeptReason::SequenceStart),
      "{end}"
    );
    assert_eq!(stream.read(&slnr()), Verdict::Disposable, "{end}");
  }
  // And so does a packet that does not parse, which may hold one.
  assert_eq!(
    stream.read(&lengths(4, &[vec![0x80, 0x01]])),
    kept(KeptReason::Malformed)
  );
  assert_eq!(stream.read(&slnr()), kept(KeptReason::SequenceStart));
  assert_eq!(stream.read(&slnr()), Verdict::Disposable);
}

#[test]
fn an_hevc_recovery_point_keeps_every_slnr_picture_until_an_irap_picture_the_decoder_decodes() {
  // FFmpeg ends a recovery at the first picture of its count and holds back
  // the pictures before it (`hevc/hevcdec.c` 3289–3294, `hevc/refs.c`
  // 239–242); an IRAP picture ends it (3283–3287).
  let slnr = || hevc(0, 0, 0, &[0xAF]);
  let recovery = || hevc_sei(39, 0, &[sei_message(6, &[0x80])]);
  let read = |stream: &mut NalStream, nals: &[Vec<u8>]| stream.read(&lengths(4, nals));
  let mut stream = hevc_stream(&hvcc(0, 3, &[hevc_sps(0)]));
  assert_eq!(
    read(&mut stream, &[recovery(), hevc(1, 0, 0, &[0xAF])]),
    kept(KeptReason::Companion(39))
  );
  assert_eq!(read(&mut stream, &[slnr()]), kept(KeptReason::Recovering));
  assert_eq!(read(&mut stream, &[slnr()]), kept(KeptReason::Recovering));
  // Past an IRAP type the decoder skips, and an IRAP picture of a layer
  // above 0.
  assert_eq!(
    read(&mut stream, &[hevc(22, 0, 0, &[0xAF])]),
    kept(KeptReason::RandomAccess)
  );
  assert_eq!(read(&mut stream, &[slnr()]), kept(KeptReason::Recovering));
  assert_eq!(
    read(&mut stream, &[hevc(19, 1, 0, &[0xAF])]),
    kept(KeptReason::Extension)
  );
  assert_eq!(read(&mut stream, &[slnr()]), kept(KeptReason::Recovering));
  // Up to an IRAP picture it decodes.
  assert_eq!(
    read(&mut stream, &[hevc(21, 0, 0, &[0xAF])]),
    kept(KeptReason::RandomAccess)
  );
  assert_eq!(read(&mut stream, &[slnr()]), Verdict::Disposable);
  // A recovery point in an IRAP picture's own packet starts none.
  assert_eq!(
    read(&mut stream, &[recovery(), hevc(19, 0, 0, &[0xAF])]),
    kept(KeptReason::Companion(39))
  );
  assert_eq!(read(&mut stream, &[slnr()]), Verdict::Disposable);
}

#[test]
fn an_hevc_picture_that_may_take_up_what_an_earlier_packet_left_is_kept() {
  // FFmpeg hands SEI state and a Dolby Vision RPU to the next picture it
  // decodes (`hevc/hevcdec.c` 3068–3151, 3505–3512, 3723–3760).
  let slnr = || hevc(0, 0, 0, &[0xAF]);
  let user_data = || hevc_sei(39, 0, &[sei_message(5, &[0x42; 16])]);
  let read = |stream: &mut NalStream, nals: &[Vec<u8>]| stream.read(&lengths(4, nals));
  let fresh = || hevc_stream(&hvcc(0, 3, &[hevc_sps(0)]));
  // A prefix SEI unit in a packet whose picture the decoder may skip — a
  // RASL picture, a reserved type, a reserved IRAP type — or that holds
  // none.
  for picture in [
    vec![hevc(9, 0, 0, &[0xAF])],
    vec![hevc(8, 0, 0, &[0xAF])],
    vec![hevc(11, 0, 0, &[0xAF])],
    vec![hevc(22, 0, 0, &[0xAF])],
    vec![],
  ] {
    let mut stream = fresh();
    let mut nals = vec![user_data()];
    nals.extend(picture.iter().cloned());
    assert!(!read(&mut stream, &nals).is_disposable(), "{picture:x?}");
    assert_eq!(
      read(&mut stream, &[slnr()]),
      kept(KeptReason::CarriedOver),
      "{picture:x?}"
    );
    // That picture, kept, took it up.
    assert_eq!(
      read(&mut stream, &[slnr()]),
      Verdict::Disposable,
      "{picture:x?}"
    );
  }
  // A suffix SEI unit, a picture hash, and an RPU: only a picture of their
  // own packet takes them up.
  for tail in [
    hevc_sei(40, 0, &[picture_hash()]),
    hevc(62, 0, 0, &[0x01, 0x02, 0x03]),
  ] {
    let mut stream = fresh();
    assert!(!read(&mut stream, &[hevc(9, 0, 0, &[0xAF]), tail.clone()]).is_disposable());
    assert_eq!(read(&mut stream, &[slnr()]), kept(KeptReason::CarriedOver));
    let mut stream = fresh();
    assert!(!read(&mut stream, &[hevc(1, 0, 0, &[0xAF]), tail.clone()]).is_disposable());
    assert_eq!(read(&mut stream, &[slnr()]), Verdict::Disposable);
  }
  // A prefix SEI unit after its packet's picture waits for the next one;
  // one before it does not.
  let mut stream = fresh();
  assert!(!read(&mut stream, &[hevc(1, 0, 0, &[0xAF]), user_data()]).is_disposable());
  assert_eq!(read(&mut stream, &[slnr()]), kept(KeptReason::CarriedOver));
  let mut stream = fresh();
  assert!(!read(&mut stream, &[user_data(), hevc(1, 0, 0, &[0xAF])]).is_disposable());
  assert_eq!(read(&mut stream, &[slnr()]), Verdict::Disposable);
  // Only a picture's first slice segment starts it.
  let mut stream = fresh();
  assert!(!read(&mut stream, &[user_data(), hevc(1, 0, 0, &[0x2F])]).is_disposable());
  assert_eq!(read(&mut stream, &[slnr()]), kept(KeptReason::CarriedOver));
  // Filler payload leaves nothing.
  let mut stream = fresh();
  assert_eq!(
    read(
      &mut stream,
      &[hevc_sei(39, 0, &[filler()]), hevc(9, 0, 0, &[0xAF])]
    ),
    kept(KeptReason::Referenced)
  );
  assert_eq!(read(&mut stream, &[slnr()]), Verdict::Disposable);
}

/// SplitMix64, for the property below: fixed seeds, the same draws on
/// every platform.
struct Seeded(u64);

impl Seeded {
  fn next(&mut self) -> u64 {
    self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = self.0;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
  }

  fn below(&mut self, bound: u64) -> u64 {
    self.next() % bound
  }
}

/// A draw that stands for an SEI unit of messages its picture alone takes
/// up, rather than a NAL unit type.
const RIDING_SEI: u8 = u8::MAX;

#[test]
fn a_packet_holding_anything_that_may_be_needed_is_never_marked() {
  // Random H.264 and HEVC packets, length-framed and Annex B: whenever one
  // holds a reference slice, a random-access slice, a parameter set or any
  // unit but a non-reference slice at the highest sub-layer, a delimiter,
  // filler or an SEI unit of messages its picture alone takes up — or no
  // slice — it is kept; else it is disposable.
  let mut marked = 0;
  let mut kept_ = 0;
  for seed in 0..512 {
    let mut draw = Seeded(seed);
    let hevc_track = draw.below(2) == 1;
    let annex = draw.below(2) == 1;
    let mut nals = Vec::new();
    let mut needed = false;
    let mut pictures = 0;
    for _ in 0..1 + draw.below(5) {
      // Most units are drawn from those that may ride in a disposable
      // packet, so both sides are drawn often; the rest from every type,
      // an SEI unit among them, of a message that keeps it.
      let friendly = draw.below(5) != 0;
      if hevc_track {
        const RIDE: [u8; 8] = [0, 2, 4, 6, 8, 35, 38, RIDING_SEI];
        let nal_unit_type = if friendly {
          RIDE[draw.below(RIDE.len() as u64) as usize]
        } else {
          draw.below(64) as u8
        };
        let temporal_id = if friendly { 2 } else { draw.below(3) as u8 };
        let layer = if !friendly && draw.below(4) == 0 {
          1
        } else {
          0
        };
        let slnr = matches!(nal_unit_type, 0 | 2 | 4 | 6 | 8);
        let beside = matches!(nal_unit_type, 35 | 38 | RIDING_SEI);
        if layer != 0 || !(slnr && temporal_id == 2 || beside) {
          needed = true;
        }
        if slnr {
          pictures += 1;
        }
        // The units read here never include a parameter set that lowers
        // or raises the stream's three sub-layers.
        let nal = match nal_unit_type {
          RIDING_SEI => hevc_sei(39 + draw.below(2) as u8, 2, &[filler()]),
          32 => hevc_vps(2),
          33 => hevc_sps(2),
          other => hevc(other, layer, temporal_id, &[0xAF, 0x13]),
        };
        nals.push(nal);
      } else {
        const RIDE: [u8; 7] = [1, 2, 3, 4, 9, 12, RIDING_SEI];
        let nal_unit_type = if friendly {
          RIDE[draw.below(RIDE.len() as u64) as usize]
        } else {
          draw.below(32) as u8
        };
        let nal_ref_idc = if friendly { 0 } else { draw.below(4) as u8 };
        let slice = matches!(nal_unit_type, 1..=4);
        let beside = matches!(nal_unit_type, 9 | 12 | RIDING_SEI);
        if !(slice && nal_ref_idc == 0 || beside) {
          needed = true;
        }
        if slice {
          pictures += 1;
        }
        let nal = match nal_unit_type {
          RIDING_SEI => avc_sei(&[picture_timing()]),
          other => avc(nal_ref_idc, other, &[0x9C, 0x13]),
        };
        nals.push(nal);
      }
    }
    if pictures == 0 {
      needed = true;
    }
    let (extradata, packet) = match (hevc_track, annex) {
      (false, false) => (avcc(3), lengths(4, &nals)),
      (false, true) => (annex_b(&[avc_sps()]), annex_b(&nals)),
      (true, false) => (hvcc(0, 3, &[hevc_sps(2)]), lengths(4, &nals)),
      (true, true) => (annex_b(&[hevc_sps(2)]), annex_b(&nals)),
    };
    let codec = if hevc_track {
      NalCodec::Hevc
    } else {
      NalCodec::H264
    };
    let mut stream = NalStream::new(codec, &extradata);
    stream.reference_seen = true;
    stream.intra_seen = true;
    stream.sequence_start = false;
    let verdict = stream.read(&packet);
    assert_eq!(
      verdict.is_disposable(),
      !needed,
      "seed {seed}: {nals:?} read {verdict:?}"
    );
    if verdict.is_disposable() {
      marked += 1;
    } else {
      kept_ += 1;
    }
  }
  // Both sides drawn.
  assert!(marked > 16 && kept_ > 16, "{marked} marked, {kept_} kept");
}

#[test]
fn a_kept_reason_says_why() {
  let shown = |reason: KeptReason| std::string::ToString::to_string(&reason);
  assert_eq!(
    shown(KeptReason::Companion(8)),
    "a NAL unit of type 8 a later picture may need"
  );
  assert_eq!(
    shown(KeptReason::BelowHighestSubLayer),
    "a picture below the highest temporal sub-layer"
  );
  assert_eq!(
    shown(KeptReason::FramingUnknown),
    "a framing a decoder may read otherwise"
  );
  assert_eq!(
    shown(KeptReason::MaybeField),
    "a picture that may be one field of a pair"
  );
  assert_eq!(
    shown(KeptReason::AwaitingIntraSlice),
    "an intra slice before the first one kept"
  );
  assert_eq!(
    shown(KeptReason::SequenceStart),
    "the first packet a sequence may start at"
  );
  assert_eq!(
    shown(KeptReason::CarriedOver),
    "a picture that may take up what an earlier packet left"
  );
}
