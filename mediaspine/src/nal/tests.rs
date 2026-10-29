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

/// An H.264 IDR slice of PPS 0, `slice_type` 7, I, its header read to its
/// `frame_num`: one FFmpeg starts a picture at.
fn idr_slice() -> Vec<u8> {
  avc(3, 5, &[0x88, 0x84, 0x21])
}

/// A non-reference H.264 slice of PPS 0, B, its header read to its
/// `frame_num`, under an SPS of frames only: [`b_slice`] names PPS 3, which
/// the laws' records do not hold, and FFmpeg refuses.
fn b_slice_0() -> Vec<u8> {
  slice_of(0, 0, 0, 0, None)
}

/// PPS 0 on SPS 0 of two slice groups, FMO, which FFmpeg refuses
/// (`h264_ps.c` 757–762): under `AV_EF_EXPLODE` it reads no more of the
/// packet after it (`h264dec.c` 741–745).
fn refused_pps() -> Vec<u8> {
  let mut fmo = Bits::new();
  fmo.ue(0).ue(0).bit(0).bit(0).ue(1).ue(0);
  avc(3, 8, &fmo.finish())
}

/// An H.264 picture parameter set, `pps_id` on `sps_id`: CAVLC, one slice
/// group, one reference by default, no weights, QP 26, offsets 0, the
/// deblocking control present, no redundant counts, and nothing after.
fn pps(pps_id: u32, sps_id: u32) -> Vec<u8> {
  let mut bits = Bits::new();
  bits
    .ue(pps_id)
    .ue(sps_id)
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
  let mut unit = vec![0x68];
  unit.extend_from_slice(&bits.finish());
  unit
}

/// [`pps`] of `redundant_pic_cnt_present_flag` 1: its slices carry a
/// `redundant_pic_cnt`, and FFmpeg discards one of a count above 0
/// (`h264_slice.c` 2081–2085).
fn redundant_pps(pps_id: u32, sps_id: u32) -> Vec<u8> {
  let mut bits = Bits::new();
  bits
    .ue(pps_id)
    .ue(sps_id)
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
    .bit(1);
  let mut unit = vec![0x68];
  unit.extend_from_slice(&bits.finish());
  unit
}

/// A slice of a picture parameter set of [`redundant_pps`], under an SPS of
/// `pic_order_cnt_type` 0 and four bits of it, as [`Sps::BASELINE`] and
/// [`Sps::HIGH`] write: [`slice_of`]'s first fields, then
/// `pic_order_cnt_lsb` and `redundant_pic_cnt` `count`. An I slice where
/// `intra`, a P or B slice else.
fn counted_slice(
  nal_ref_idc: u8,
  pps_id: u32,
  frame_num: u32,
  structure: Option<Structure>,
  intra: bool,
  count: u32,
) -> Vec<u8> {
  let slice_type = match (intra, nal_ref_idc) {
    (true, _) => 7,
    (false, 0) => 6,
    (false, _) => 5,
  };
  let mut bits = Bits::new();
  bits.ue(0).ue(slice_type).ue(pps_id).bits(frame_num, 4);
  match structure {
    None => {}
    Some(Structure::Frame) => {
      bits.bit(0);
    }
    Some(Structure::Top) => {
      bits.bit(1).bit(0);
    }
    Some(Structure::Bottom) => {
      bits.bit(1).bit(1);
    }
  }
  bits.bits(2 * frame_num, 4).ue(count).ue(1).se(0);
  avc(nal_ref_idc, 1, &bits.finish())
}

/// An IDR slice of a picture parameter set of [`redundant_pps`], `pps_id`,
/// under an SPS of frames only and of [`counted_slice`]'s four bits of
/// `frame_num` and of picture order count: I, `frame_num` 0, `idr_pic_id`
/// 0, `pic_order_cnt_lsb` 0, then `redundant_pic_cnt` `count` and the rest
/// of an IDR header.
fn counted_idr(pps_id: u32, count: u32) -> Vec<u8> {
  let mut bits = Bits::new();
  bits
    .ue(0)
    .ue(7)
    .ue(pps_id)
    .bits(0, 4)
    .ue(0)
    .bits(0, 4)
    .ue(count)
    .bit(0)
    .bit(0)
    .se(0);
  avc(3, 5, &bits.finish())
}

/// [`pps`], then the tail FFmpeg reads where bits are left and the SPS's
/// profile lets it (`h264_ps.c` 794–810): `transform_8x8_mode_flag`, no
/// scaling matrix, a second chroma offset of 0.
fn pps_tail(pps_id: u32, sps_id: u32, transform_8x8: bool) -> Vec<u8> {
  let mut bits = Bits::new();
  bits
    .ue(pps_id)
    .ue(sps_id)
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
    .bit(0)
    .bit(u32::from(transform_8x8))
    .bit(0)
    .se(0);
  let mut unit = vec![0x68];
  unit.extend_from_slice(&bits.finish());
  unit
}

/// An H.264 slice — B of `nal_ref_idc` 0, P else — its header's first
/// fields as FFmpeg reads them before it starts a picture:
/// `first_mb_in_slice`, `slice_type`, `pic_parameter_set_id` and a four-bit
/// `frame_num`, the test SPS's; for `Some`, the `field_pic_flag` and
/// `bottom_field_flag` an SPS that lets fields in reads; then a few bits of
/// the rest.
fn slice_of(
  nal_ref_idc: u8,
  first_mb: u32,
  pps_id: u32,
  frame_num: u32,
  structure: Option<Structure>,
) -> Vec<u8> {
  slice_bits(nal_ref_idc, first_mb, pps_id, (frame_num, 4), structure)
}

/// [`slice_of`], its `frame_num` of the given bits.
fn slice_bits(
  nal_ref_idc: u8,
  first_mb: u32,
  pps_id: u32,
  (frame_num, frame_num_bits): (u32, u32),
  structure: Option<Structure>,
) -> Vec<u8> {
  let mut bits = Bits::new();
  bits
    .ue(first_mb)
    .ue(if nal_ref_idc == 0 { 6 } else { 5 })
    .ue(pps_id)
    .bits(frame_num, frame_num_bits);
  match structure {
    None => {}
    Some(Structure::Frame) => {
      bits.bit(0);
    }
    Some(Structure::Top) => {
      bits.bit(1).bit(0);
    }
    Some(Structure::Bottom) => {
      bits.bit(1).bit(1);
    }
  }
  bits.bits(0b0110, 4).ue(1).se(0);
  avc(nal_ref_idc, 1, &bits.finish())
}

/// A frame's slice, of PPS 0, under an SPS that lets fields in.
fn frame_slice(nal_ref_idc: u8, frame_num: u32) -> Vec<u8> {
  slice_of(nal_ref_idc, 0, 0, frame_num, Some(Structure::Frame))
}

/// A field's first slice, of PPS 0, its parity `bottom`.
fn field_slice(nal_ref_idc: u8, frame_num: u32, bottom: bool) -> Vec<u8> {
  let structure = if bottom {
    Structure::Bottom
  } else {
    Structure::Top
  };
  slice_of(nal_ref_idc, 0, 0, frame_num, Some(structure))
}

/// An SPS that lets fields in, PAFF and MBAFF: High, `frame_mbs_only_flag`
/// 0, `mb_adaptive_frame_field_flag` 1.
fn interlaced_sps() -> Vec<u8> {
  Sps {
    frames_only: false,
    ..Sps::HIGH
  }
  .unit()
}

/// An `avcC` of four-byte lengths of [`interlaced_sps`] and PPS 0 on it.
fn interlaced_avcc() -> Vec<u8> {
  avcc_of(3, &interlaced_sps())
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
  /// `seq_parameter_set_id`.
  id: u32,
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
  /// The restriction, of a reorder depth of 1, after an extended sample
  /// aspect ratio of these two, and a colour description of this matrix
  /// where it is given.
  Described {
    sar: (u32, u32),
    matrix: Option<u32>,
  },
}

impl Sps {
  /// Baseline, 1080p, coding frames only.
  const BASELINE: Self = Self {
    profile_idc: 66,
    id: 0,
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
    bits
      .bits(self.profile_idc, 8)
      .bits(0, 8)
      .bits(40, 8)
      .ue(self.id);
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
      Vui::Described { sar, matrix } => {
        // An extended SAR, no overscan, the signal type and its colour
        // description where a matrix is given, no chroma sites, timing, HRD
        // or `pic_struct`; then the restriction.
        bits
          .bit(1)
          .bit(1)
          .bits(255, 8)
          .bits(sar.0, 16)
          .bits(sar.1, 16);
        bits.bit(0);
        match matrix {
          Some(matrix) => {
            bits
              .bit(1)
              .bits(5, 3)
              .bit(0)
              .bit(1)
              .bits(1, 8)
              .bits(1, 8)
              .bits(matrix, 8);
          }
          None => {
            bits.bit(0);
          }
        }
        bits.bit(0).bit(0).bit(0).bit(0).bit(0);
        restriction(&mut bits, 1);
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

/// A film grain characteristics message that does not persist: no
/// component, `film_grain_characteristics_persistence_flag` 0, then the
/// payload's alignment bits (H.265 D.2.21, D.2.1).
fn film_grain() -> Vec<u8> {
  sei_message(19, &[0x00, 0x02])
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
  stream.pending.reference_seen = true;
  stream.pending.intra_seen = true;
  stream
}

/// An HEVC reading of `extradata` past its first packet, which takes up the
/// `eos` the decoder opens with: the laws below read the packets after it.
fn hevc_stream(extradata: &[u8]) -> NalStream {
  let mut stream = NalStream::new(NalCodec::Hevc, extradata);
  stream.pending.sequence_start = false;
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
fn an_h264_slice_is_read_for_its_field_wherever_an_sps_lets_fields_in() {
  // Before any sequence parameter set, nothing.
  let mut stream = h264(&[]);
  assert_eq!(stream.frames_only(), None);
  assert_eq!(
    stream.read(&annex_b(&[b_slice()])),
    kept(KeptReason::MaybeField)
  );
  // One of frames only, in-band: its packet kept, the next marked, whatever
  // picture parameter set its slices name.
  assert_eq!(
    stream.read(&annex_b(&[avc_sps(), b_slice()])),
    kept(KeptReason::Companion(7))
  );
  assert_eq!(stream.frames_only(), Some(true));
  assert_eq!(stream.read(&annex_b(&[b_slice()])), Verdict::Disposable);
  // One that lets fields in, mid-stream, and a picture parameter set read on
  // it: from that packet on, each slice is read for its field.
  let interlaced = Sps {
    frames_only: false,
    ..Sps::HIGH
  };
  let _ = stream.read(&annex_b(&[interlaced.unit(), pps(0, 0)]));
  assert_eq!(stream.frames_only(), Some(false));
  assert_eq!(
    stream.read(&annex_b(&[frame_slice(0, 1)])),
    Verdict::Disposable
  );
  assert_eq!(
    stream.read(&annex_b(&[
      field_slice(0, 1, false),
      field_slice(0, 1, true)
    ])),
    Verdict::Disposable
  );
  assert_eq!(
    stream.read(&annex_b(&[field_slice(0, 1, false)])),
    kept(KeptReason::UnpairedField)
  );
  assert_eq!(
    stream.read(&annex_b(&[field_slice(0, 1, true)])),
    kept(KeptReason::UnpairedField)
  );
  // A slice naming a picture parameter set FFmpeg does not hold starts no
  // picture (`h264_slice.c` 1740–1746): nothing of the pairing moves.
  assert_eq!(stream.read(&annex_b(&[b_slice()])), Verdict::Disposable);
  // A later one of frames only does not make every slice a frame again, but
  // the slices of a picture parameter set read on it are frames. Its size
  // another, FFmpeg sets its context up again at its first picture
  // (`h264_slice.c` 1066–1092, 1136–1165), a flush: that picture awaits a
  // reference picture, as after a seek.
  let _ = stream.read(&annex_b(&[avc_sps(), pps(0, 0)]));
  assert_eq!(stream.frames_only(), Some(false));
  assert_eq!(
    stream.read(&annex_b(&[slice_of(0, 0, 0, 1, None)])),
    kept(KeptReason::AwaitingReference)
  );
  assert_eq!(
    stream.read(&annex_b(&[slice_of(2, 0, 0, 1, None)])),
    kept(KeptReason::Referenced)
  );
  assert_eq!(
    stream.read(&annex_b(&[slice_of(0, 0, 0, 2, None)])),
    Verdict::Disposable
  );
  // A record's: an `avcC` of a PAFF/MBAFF-capable SPS, and reference and
  // IDR slices keep their own reasons.
  let mut record = h264(&avcc_of(3, &interlaced.unit()));
  assert_eq!(record.frames_only(), Some(false));
  assert_eq!(
    record.read(&lengths(4, &[frame_slice(0, 1)])),
    Verdict::Disposable
  );
  assert_eq!(
    record.read(&lengths(4, &[field_slice(0, 1, true)])),
    kept(KeptReason::UnpairedField)
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
  // switches to — and the reading cannot tell whether FFmpeg took up its
  // parameter sets, so the slices of the picture parameter set in it are
  // read by neither list.
  let mut switched = h264(&avcc(3));
  assert_eq!(
    switched.read(&annex_b(&[interlaced.clone(), pps(0, 0), b_slice()])),
    kept(KeptReason::FramingChange)
  );
  assert_eq!(switched.frames_only(), Some(false));
  assert_eq!(
    switched.read(&annex_b(&[frame_slice(0, 1)])),
    kept(KeptReason::MaybeField)
  );
  // While the size is unknown (a parameter set of 21,802 bytes, `ERANGE`),
  // in every framing a decoder may read the packet in. A record read whole
  // after it does not unlearn it, and sets the lists again.
  let mut big = avc_sps();
  big.resize(21_800, 0x11);
  for packet in [
    annex_b(&[interlaced.clone(), pps(0, 0)]),
    lengths(1, &[interlaced.clone(), pps(0, 0)]),
    lengths(2, &[interlaced.clone(), pps(0, 0)]),
    lengths(3, &[interlaced.clone(), pps(0, 0)]),
    lengths(4, &[interlaced.clone(), pps(0, 0)]),
  ] {
    let mut unknown = h264(&avcc_of(3, &big));
    assert_eq!(unknown.framing(), None);
    assert_eq!(unknown.frames_only(), Some(true));
    assert_eq!(unknown.read(&packet), kept(KeptReason::FramingUnknown));
    assert_eq!(unknown.frames_only(), Some(false), "{packet:x?}");
    assert_eq!(unknown.sets.pps[0], Slot::Unknown, "{packet:x?}");
    unknown.reconfigure(&avcc(3));
    assert_eq!(unknown.frames_only(), Some(false));
    assert!(matches!(unknown.sets.pps[0], Slot::Holds(_)), "{packet:x?}");
    // What the packet's every framing taught otherwise stays: some framings
    // read units no parameter set proves anything of.
    assert_ne!(
      unknown.read(&lengths(4, &[slice_of(0, 0, 0, 1, None)])),
      kept(KeptReason::MaybeField),
      "{packet:x?}"
    );
  }
  // Unframed: a record that ends before its parameter sets do.
  let record = avcc(3);
  let mut short = h264(&record[..record.len() - 1]);
  assert_eq!(
    short.read(&lengths(4, &[interlaced.clone(), pps(0, 0)])),
    kept(KeptReason::Unframed)
  );
  assert_eq!(short.frames_only(), Some(false));
  assert_eq!(short.sets.pps[0], Slot::Unknown);
  short.reconfigure(&avcc(3));
  assert!(matches!(short.sets.pps[0], Slot::Holds(_)));
  assert_ne!(
    short.read(&lengths(4, &[slice_of(0, 0, 0, 1, None)])),
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
  // A seek starts the wait again, and an IDR slice ends it. One FFmpeg
  // refuses ends nothing: a unit of one byte past its header, whose
  // `pic_parameter_set_id` FFmpeg reads past its end and refuses
  // (`h264_slice.c` 1735–1739).
  stream.seeked();
  assert_eq!(
    read(&mut stream, &[b_slice()]),
    kept(KeptReason::AwaitingReference)
  );
  assert_eq!(
    read(&mut stream, &[avc(3, 5, &[0x88])]),
    kept(KeptReason::RandomAccess)
  );
  assert_eq!(
    read(&mut stream, &[b_slice()]),
    kept(KeptReason::AwaitingReference)
  );
  assert_eq!(
    read(&mut stream, &[idr_slice()]),
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
    read(&mut stream, &[idr_slice()]),
    kept(KeptReason::RandomAccess)
  );
  assert_eq!(
    read(&mut stream, &[i_slice(), b_slice()]),
    Verdict::Disposable
  );
}

#[test]
fn an_intra_slice_past_a_unit_ffmpeg_may_stop_at_ends_no_wait() {
  // A PPS FFmpeg refuses, then an intra slice: under `AV_EF_EXPLODE` FFmpeg
  // reads nothing of the packet past the refusal (`h264dec.c` 741–745), and
  // the slice may go undecoded, `non_gray` unset (`h264_slice.c` 2032–2034).
  // Predicted, before this law: the census counted the slice, and the next
  // intra slice was marked; planted, a start past a possible stop followed,
  // red.
  let mut stream = NalStream::new(NalCodec::H264, &avcc(3));
  let read = |stream: &mut NalStream, nals: &[Vec<u8>]| stream.read(&lengths(4, nals));
  assert_eq!(
    read(&mut stream, &[p_slice()]),
    kept(KeptReason::Referenced)
  );
  assert_eq!(
    read(&mut stream, &[refused_pps(), i_slice()]),
    kept(KeptReason::Companion(8))
  );
  assert_eq!(
    read(&mut stream, &[i_slice(), b_slice_0()]),
    kept(KeptReason::AwaitingIntraSlice)
  );
  assert_eq!(
    read(&mut stream, &[i_slice(), b_slice_0()]),
    Verdict::Disposable
  );
}

#[test]
fn a_reference_picture_past_a_possible_stop_or_refused_ends_no_wait() {
  // The same for the reference picture a reading awaits after its opening
  // or a seek (`h264_parse.c` 293–294): one past a PPS FFmpeg refuses may go
  // undecoded. Predicted, before this law: counted, and the next
  // non-reference picture marked; planted, a start past a possible stop
  // followed, red.
  let mut stream = NalStream::new(NalCodec::H264, &avcc(3));
  stream.pending.intra_seen = true;
  let read = |stream: &mut NalStream, nals: &[Vec<u8>]| stream.read(&lengths(4, nals));
  assert_eq!(
    read(&mut stream, &[refused_pps(), p_slice()]),
    kept(KeptReason::Companion(8))
  );
  assert_eq!(
    read(&mut stream, &[b_slice_0()]),
    kept(KeptReason::AwaitingReference)
  );
  // And a reference slice FFmpeg refuses for a PPS it does not hold
  // (`h264_slice.c` 1740–1746) is none it decodes. Planted, a refused
  // slice counted a reference picture: red.
  assert_eq!(
    read(&mut stream, &[slice_of(2, 0, 3, 1, None)]),
    kept(KeptReason::Referenced)
  );
  assert_eq!(
    read(&mut stream, &[b_slice_0()]),
    kept(KeptReason::AwaitingReference)
  );
  assert_eq!(
    read(&mut stream, &[p_slice()]),
    kept(KeptReason::Referenced)
  );
  assert_eq!(read(&mut stream, &[b_slice_0()]), Verdict::Disposable);
}

#[test]
fn a_frame_at_which_the_context_is_set_up_again_awaits_a_reference() {
  // Every SPS of frames only, SPS 0 read again of another width, and PPS 0
  // on it: at the next picture FFmpeg sets its context up again
  // (`h264_slice.c` 1066–1092, 1136–1165), flushing it — its picture order
  // count and `frame_num` taken up afresh from that picture (`h264dec.c`
  // 438–474), so that dropping it moves them to the next. Predicted, before
  // this law: marked; planted, no set-up again, red.
  let mut stream = h264(&avcc(3));
  let read = |stream: &mut NalStream, nals: &[Vec<u8>]| stream.read(&lengths(4, nals));
  assert_eq!(read(&mut stream, &[b_slice_0()]), Verdict::Disposable);
  let narrow = Sps {
    width_in_mbs_minus1: 79,
    ..Sps::BASELINE
  };
  let _ = read(&mut stream, &[narrow.unit(), pps(0, 0)]);
  assert_eq!(stream.frames_only(), Some(true));
  assert_eq!(
    read(&mut stream, &[b_slice_0()]),
    kept(KeptReason::AwaitingReference)
  );
  assert_eq!(
    read(&mut stream, &[b_slice_0()]),
    kept(KeptReason::AwaitingReference)
  );
  assert_eq!(
    read(&mut stream, &[p_slice()]),
    kept(KeptReason::Referenced)
  );
  assert_eq!(read(&mut stream, &[b_slice_0()]), Verdict::Disposable);
  // SPS 0 read again byte for byte, and PPS 0 on it, set nothing up.
  let _ = read(&mut stream, &[narrow.unit(), pps(0, 0)]);
  assert_eq!(read(&mut stream, &[b_slice_0()]), Verdict::Disposable);
}

#[test]
fn a_primary_picture_of_a_pps_of_redundant_counts_ends_the_waits() {
  // A Baseline stream whose PPS carries redundant counts: FFmpeg reads each
  // slice to its `redundant_pic_cnt` (`h264_slice.c` 1786–1812), and decodes
  // one of a count of 0. Predicted, before this law: every slice of it
  // unreadable, nothing counted, and the waits for a reference picture and
  // an intra slice never over; planted, the count unread again, red.
  let mut stream = NalStream::new(NalCodec::H264, &avcc(3));
  let read = |stream: &mut NalStream, nals: &[Vec<u8>]| stream.read(&lengths(4, nals));
  assert_eq!(
    read(&mut stream, &[redundant_pps(0, 0)]),
    kept(KeptReason::Companion(8))
  );
  assert_eq!(
    read(&mut stream, &[counted_slice(2, 0, 0, None, true, 0)]),
    kept(KeptReason::Referenced)
  );
  assert_eq!(
    read(
      &mut stream,
      &[
        counted_slice(0, 0, 1, None, true, 0),
        counted_slice(0, 0, 1, None, false, 0)
      ]
    ),
    Verdict::Disposable
  );
}

#[test]
fn a_redundant_slice_ends_no_wait_and_moves_no_pairing() {
  // A slice of a count above 0 FFmpeg reads and discards (`h264_slice.c`
  // 2081–2085): no picture it decodes, and none it joins. Predicted, before
  // this law: unreadable, and so kept; planted, a discarded slice taken for
  // one FFmpeg decodes, red.
  let mut stream = NalStream::new(NalCodec::H264, &avcc(3));
  let read = |stream: &mut NalStream, nals: &[Vec<u8>]| stream.read(&lengths(4, nals));
  let _ = read(&mut stream, &[redundant_pps(0, 0)]);
  let _ = read(&mut stream, &[counted_slice(2, 0, 0, None, true, 1)]);
  assert!(!stream.pending.reference_seen);
  assert!(!stream.pending.intra_seen);
  assert_eq!(
    read(&mut stream, &[counted_slice(0, 0, 0, None, false, 0)]),
    kept(KeptReason::AwaitingReference)
  );
  // Fields: a top field waits, and a redundant bottom one joins nothing.
  let mut fields = h264(&interlaced_avcc());
  let _ = read4(&mut fields, &[redundant_pps(0, 0)]);
  assert_eq!(
    read4(
      &mut fields,
      &[counted_slice(0, 0, 1, Some(Structure::Top), false, 0)]
    ),
    kept(KeptReason::UnpairedField)
  );
  let _ = read4(
    &mut fields,
    &[counted_slice(0, 0, 1, Some(Structure::Bottom), false, 1)],
  );
  assert!(matches!(
    fields.pairing,
    Pairing::Waiting(Waiting { bottom: false, .. })
  ));
  assert_eq!(
    read4(
      &mut fields,
      &[counted_slice(0, 0, 1, Some(Structure::Bottom), false, 0)]
    ),
    kept(KeptReason::UnpairedField)
  );
  assert_eq!(fields.pairing, Pairing::Settled);
}

#[test]
fn a_packet_of_redundant_slices_alone_is_disposable_where_no_field_waits() {
  // FFmpeg discards each (`h264_slice.c` 2081–2085): the packet makes no
  // picture, and moves none of what one moves — whatever the slices'
  // `nal_ref_idc`, whatever a picture would await. Predicted, before this
  // law: kept, its slices unreadable; planted, the packet held to a
  // picture's rules, red.
  let mut stream = NalStream::new(NalCodec::H264, &avcc(3));
  let read = |stream: &mut NalStream, nals: &[Vec<u8>]| stream.read(&lengths(4, nals));
  let _ = read(&mut stream, &[redundant_pps(0, 0)]);
  assert_eq!(
    read(&mut stream, &[counted_slice(2, 0, 0, None, true, 1)]),
    Verdict::Disposable
  );
  assert_eq!(
    read(
      &mut stream,
      &[
        counted_slice(0, 0, 0, None, true, 2),
        counted_slice(0, 0, 0, None, false, 1)
      ]
    ),
    Verdict::Disposable
  );
  // One slice not proven discarded: the packet's picture is FFmpeg's, and
  // its rules hold.
  assert_eq!(
    read(
      &mut stream,
      &[
        counted_slice(0, 0, 0, None, false, 1),
        counted_slice(0, 0, 0, None, false, 0)
      ]
    ),
    kept(KeptReason::AwaitingReference)
  );
  // A first field waiting, FFmpeg ends it again at a packet of slices that
  // starts no picture (`h264dec.c` 650, 1061–1076): kept. Planted, the
  // waiting field passed over, red.
  let mut fields = h264(&interlaced_avcc());
  let _ = read4(&mut fields, &[redundant_pps(0, 0)]);
  let _ = read4(
    &mut fields,
    &[counted_slice(2, 0, 1, Some(Structure::Top), false, 0)],
  );
  assert_eq!(
    read4(
      &mut fields,
      &[counted_slice(2, 0, 1, Some(Structure::Bottom), false, 1)]
    ),
    kept(KeptReason::PendingField)
  );
}

#[test]
fn a_packet_of_slices_that_start_no_picture_is_kept_while_a_field_may_wait() {
  // A reference top field waits; a packet whose one slice FFmpeg refuses —
  // it names PPS 3, which the list does not hold — starts no picture, yet
  // FFmpeg ends the waiting field again at it (`has_slice`,
  // `ff_h264_field_end`, `h264dec.c` 650, 1061–1076), repeating its
  // reference marking. Predicted, before this law: marked; planted, the
  // waiting field passed over, red.
  let mut stream = h264(&interlaced_avcc());
  assert_eq!(
    read4(&mut stream, &[field_slice(2, 1, false)]),
    kept(KeptReason::Referenced)
  );
  assert_eq!(
    read4(&mut stream, &[b_slice()]),
    kept(KeptReason::PendingField)
  );
  assert!(matches!(stream.pairing, Pairing::Waiting(_)));
  // With nothing waiting, it moves nothing.
  let mut settled = h264(&interlaced_avcc());
  assert_eq!(read4(&mut settled, &[b_slice()]), Verdict::Disposable);
}

#[test]
fn a_redundant_slice_beside_a_primary_picture_keeps_nothing_of_its_own() {
  // FFmpeg discards the redundant slice after its header (`h264_slice.c`
  // 2081–2085): the packet's picture is the primary one, and the redundant
  // slice's `nal_ref_idc` marks nothing, nor its intra slice `non_gray`
  // (2032–2034). FFmpeg's `picture_intra_only` reads every slice, the
  // redundant one among them (`h264dec.c` 501–551). Predicted, before this
  // law: kept, every slice unreadable and no reference picture counted;
  // planted, the redundant slice's reference or its intra slice counted,
  // red.
  let mut stream = NalStream::new(NalCodec::H264, &avcc(3));
  let read = |stream: &mut NalStream, nals: &[Vec<u8>]| stream.read(&lengths(4, nals));
  let _ = read(&mut stream, &[redundant_pps(0, 0)]);
  assert_eq!(
    read(&mut stream, &[counted_slice(2, 0, 0, None, false, 0)]),
    kept(KeptReason::Referenced)
  );
  assert!(stream.pending.reference_seen);
  assert!(!stream.pending.intra_seen);
  assert_eq!(
    read(
      &mut stream,
      &[
        counted_slice(0, 0, 1, None, false, 0),
        counted_slice(2, 0, 1, None, true, 1)
      ]
    ),
    Verdict::Disposable
  );
  // A primary picture of intra slices, a redundant P one beside it: not
  // intra only to FFmpeg, but an intra slice it decodes.
  assert_eq!(
    read(
      &mut stream,
      &[
        counted_slice(0, 0, 1, None, true, 0),
        counted_slice(0, 0, 1, None, false, 1)
      ]
    ),
    kept(KeptReason::AwaitingIntraSlice)
  );
}

#[test]
fn a_redundant_slice_of_another_frame_num_after_a_first_is_refused_and_ffmpeg_may_stop() {
  // A slice after a picture's first is held to its `frame_num` in its header
  // (`h264_slice.c` 1750–1756), before FFmpeg reads its count: "Frame num
  // change", refused, and past it under `AV_EF_EXPLODE` FFmpeg reads no
  // more of the packet (`h264dec.c` 740–745). Predicted, before this law:
  // every slice unreadable, the pairing unknown after either packet;
  // planted, the refusal passed over, the first packet's pair joined, red.
  let mut stream = h264(&interlaced_avcc());
  let _ = read4(&mut stream, &[redundant_pps(0, 0)]);
  assert_eq!(
    read4(
      &mut stream,
      &[
        counted_slice(2, 0, 1, Some(Structure::Top), false, 0),
        counted_slice(2, 0, 2, Some(Structure::Top), false, 1),
        counted_slice(2, 0, 1, Some(Structure::Bottom), false, 0)
      ]
    ),
    kept(KeptReason::Referenced)
  );
  assert_eq!(stream.pairing, Pairing::Unknown);
  // Of its picture's `frame_num`, it is discarded, and the pair joined.
  let mut joined = h264(&interlaced_avcc());
  let _ = read4(&mut joined, &[redundant_pps(0, 0)]);
  let _ = read4(
    &mut joined,
    &[
      counted_slice(2, 0, 1, Some(Structure::Top), false, 0),
      counted_slice(2, 0, 1, Some(Structure::Top), false, 1),
      counted_slice(2, 0, 1, Some(Structure::Bottom), false, 0),
    ],
  );
  assert_eq!(joined.pairing, Pairing::Settled);
}

#[test]
fn an_idr_unit_whose_slice_ffmpeg_discards_awaits_a_reference_again() {
  // At a packet's first IDR unit FFmpeg removes every reference and resets
  // the picture order count's take-up and `frame_num` (`idr`, `h264dec.c`
  // 636–648, 438–448) before it reads the slice — here a redundant one it
  // then discards, as FFmpeg's parser hands it on in a packet of its own,
  // its first slice of `first_mb_in_slice` 0 (`h264_parser.c` 147–152).
  // Predicted, before this law: the packet's slices unreadable, the reset
  // unseen, and the picture after it marked; planted, an IDR unit passed
  // over, red; planted, every IDR unit of a packet resetting, the
  // primary's redundant picture beside it awaiting a reference again, red.
  let mut stream = h264(&avcc(3));
  let read = |stream: &mut NalStream, nals: &[Vec<u8>]| stream.read(&lengths(4, nals));
  let _ = read(&mut stream, &[redundant_pps(0, 0)]);
  // A primary IDR picture, and a redundant one in its packet: FFmpeg resets
  // at the first IDR unit alone (`idr_cleared`), and the primary picture is
  // a reference.
  assert_eq!(
    read(&mut stream, &[counted_idr(0, 0), counted_idr(0, 1)]),
    kept(KeptReason::RandomAccess)
  );
  assert_eq!(
    read(&mut stream, &[counted_slice(0, 0, 1, None, false, 0)]),
    Verdict::Disposable
  );
  // The redundant one alone.
  assert_eq!(
    read(&mut stream, &[counted_idr(0, 1)]),
    kept(KeptReason::RandomAccess)
  );
  assert!(!stream.pending.reference_seen);
  assert_eq!(
    read(&mut stream, &[counted_slice(0, 0, 1, None, false, 0)]),
    kept(KeptReason::AwaitingReference)
  );
  assert_eq!(
    read(&mut stream, &[counted_slice(2, 0, 1, None, false, 0)]),
    kept(KeptReason::Referenced)
  );
  assert_eq!(
    read(&mut stream, &[counted_slice(0, 0, 2, None, false, 0)]),
    Verdict::Disposable
  );
}

#[test]
fn a_packet_not_read_whole_that_may_hold_an_idr_unit_awaits_a_reference() {
  // A packet the reading does not walk — one that switches the framing,
  // which FFmpeg reads all the same (`h264dec.c` 602–607) — holding an IDR
  // unit, at which FFmpeg resets whatever it makes of the slice (`idr`,
  // 636–648): here a redundant one it discards. Predicted, before this law:
  // marked; planted, an IDR unit there passed over, red.
  let mut stream = h264(&avcc(3));
  let annex = |nals: &[Vec<u8>]| {
    let mut units = vec![avc(0, 9, &[0xF0])];
    units.extend_from_slice(nals);
    annex_b(&units)
  };
  let _ = stream.read(&lengths(4, &[redundant_pps(1, 0)]));
  assert_eq!(
    stream.read(&annex(&[counted_idr(1, 1)])),
    kept(KeptReason::FramingChange)
  );
  assert_eq!(
    stream.read(&annex(&[b_slice_0()])),
    kept(KeptReason::AwaitingReference)
  );
  assert_eq!(
    stream.read(&annex(&[p_slice()])),
    kept(KeptReason::Referenced)
  );
  assert_eq!(stream.read(&annex(&[b_slice_0()])), Verdict::Disposable);
  // A slice of another kind there resets nothing.
  assert_eq!(
    stream.read(&lengths(4, &[b_slice_0()])),
    kept(KeptReason::FramingChange)
  );
  assert_eq!(
    stream.read(&lengths(4, &[b_slice_0()])),
    Verdict::Disposable
  );
}

#[test]
fn a_packet_not_read_whole_that_may_set_the_context_up_again_awaits_a_reference() {
  // SPS 0 declares one colour matrix and SPS 1 another: a start on SPS 1
  // sets FFmpeg's context up again (`h264_slice.c` 1117–1123, 1136–1165),
  // flushing it. A packet the reading does not walk may hold one; after
  // it, a picture on SPS 2, which declares none and so sets nothing up
  // itself, may be the first FFmpeg takes its picture order count up from
  // again (`h264dec.c` 438–474). Predicted, before this law: marked;
  // planted, the set-up there passed over, red.
  let described = |id, matrix| {
    Sps {
      id,
      vui: Vui::Described {
        sar: (1, 1),
        matrix,
      },
      ..Sps::BASELINE
    }
    .unit()
  };
  let mut stream = h264(&avcc_of(3, &described(0, Some(1))));
  let read = |stream: &mut NalStream, nals: &[Vec<u8>]| stream.read(&lengths(4, nals));
  assert_eq!(
    read(&mut stream, &[slice_of(2, 0, 0, 0, None)]),
    kept(KeptReason::Referenced)
  );
  let _ = read(
    &mut stream,
    &[
      described(1, Some(6)),
      pps(1, 1),
      described(2, None),
      pps(2, 2),
    ],
  );
  // On SPS 2, nothing set up.
  assert_eq!(
    read(&mut stream, &[slice_of(0, 0, 2, 1, None)]),
    Verdict::Disposable
  );
  let mut malformed = lengths(4, &[slice_of(0, 0, 1, 1, None)]);
  malformed.extend_from_slice(&[0, 0]);
  assert_eq!(stream.read(&malformed), kept(KeptReason::Malformed));
  assert_eq!(
    read(&mut stream, &[slice_of(0, 0, 2, 1, None)]),
    kept(KeptReason::AwaitingReference)
  );
  assert_eq!(
    read(&mut stream, &[slice_of(2, 0, 2, 1, None)]),
    kept(KeptReason::Referenced)
  );
  assert_eq!(
    read(&mut stream, &[slice_of(0, 0, 2, 2, None)]),
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

/// HEVC NAL units of two-byte lengths, as the records of the laws below
/// frame them.
fn read2(stream: &mut NalStream, nals: &[Vec<u8>]) -> Verdict {
  stream.read(&lengths(2, nals))
}

/// The first slice segment of an HEVC picture of `nal_unit_type`, layer 0,
/// TemporalId 0.
fn hevc_slice(nal_unit_type: u8) -> Vec<u8> {
  hevc(nal_unit_type, 0, 0, &[0xAF])
}

#[test]
fn an_hevc_record_mid_stream_leaves_its_sei_for_the_next_picture_decoded() {
  // FFmpeg reads a packet's `AV_PKT_DATA_NEW_EXTRADATA` before the packet,
  // its SEI units into the state the next picture it decodes takes up
  // (`hevc/hevcdec.c` 3855–3860, `hevc/parse.c` 59–64). After a CRA that
  // starts a sequence its RASL pictures are skipped (`hevc/hevcdec.c`
  // 3542–3547), so a record riding on one leaves its film grain for the
  // picture after: that picture is not disposable.
  for sei in [
    hevc_sei(39, 0, &[film_grain()]),
    hevc_sei(40, 0, &[picture_hash()]),
    // User data, of layer 1.
    hevc(
      39,
      1,
      0,
      &escaped(&sei_rbsp(&[sei_message(5, &[0x42; 16])])),
    ),
  ] {
    let record = hvcc(0, 1, &[hevc_sps(0), sei.clone()]);
    let mut stream = hevc_stream(&hvcc(0, 1, &[hevc_sps(0)]));
    assert_eq!(
      read2(&mut stream, &[hevc_slice(21)]),
      kept(KeptReason::RandomAccess)
    );
    assert_eq!(
      stream.read_with_record(&lengths(2, &[hevc_slice(9)]), Some(&record)),
      kept(KeptReason::Referenced)
    );
    assert_eq!(
      read2(&mut stream, &[hevc_slice(0)]),
      kept(KeptReason::CarriedOver),
      "{sei:x?}"
    );
    // That picture, kept, took it up.
    assert_eq!(read2(&mut stream, &[hevc_slice(0)]), Verdict::Disposable);
    // A record read apart from its packet is pending for the packet after.
    let mut stream = hevc_stream(&hvcc(0, 1, &[hevc_sps(0)]));
    stream.reconfigure(&record);
    assert_eq!(
      read2(&mut stream, &[hevc_slice(9)]),
      kept(KeptReason::Referenced)
    );
    assert_eq!(
      read2(&mut stream, &[hevc_slice(0)]),
      kept(KeptReason::CarriedOver),
      "{sei:x?}"
    );
  }
  // A record of start codes, too.
  let mut stream = hevc_stream(&annex_b(&[hevc_sps(0)]));
  stream.reconfigure(&annex_b(&[hevc_sps(0), hevc_sei(39, 0, &[film_grain()])]));
  assert_eq!(
    stream.read(&annex_b(&[hevc_slice(9)])),
    kept(KeptReason::Referenced)
  );
  assert_eq!(
    stream.read(&annex_b(&[hevc_slice(0)])),
    kept(KeptReason::CarriedOver)
  );
  // A record whose SEI is filler payload alone leaves nothing.
  let mut stream = hevc_stream(&hvcc(0, 1, &[hevc_sps(0)]));
  let filled = hvcc(0, 1, &[hevc_sps(0), hevc_sei(39, 0, &[filler()])]);
  assert_eq!(
    stream.read_with_record(&lengths(2, &[hevc_slice(9)]), Some(&filled)),
    kept(KeptReason::Referenced)
  );
  assert_eq!(read2(&mut stream, &[hevc_slice(0)]), Verdict::Disposable);
}

#[test]
fn an_hevc_opening_record_leaves_its_sei_for_the_first_picture_decoded() {
  // FFmpeg's decoder reads the record it opens with, its SEI units into the
  // state the first picture it decodes takes up (`hevc/hevcdec.c`
  // 4168–4174, `hevc/parse.c` 59–64).
  for sei in [
    hevc_sei(39, 0, &[film_grain()]),
    hevc_sei(40, 0, &[picture_hash()]),
  ] {
    let record = hvcc(0, 1, &[hevc_sps(0), sei.clone()]);
    // A first packet that holds no picture leaves it for the next.
    let mut stream = NalStream::new(NalCodec::Hevc, &record);
    assert_eq!(
      read2(&mut stream, &[hevc(35, 0, 0, &[0x50])]),
      kept(KeptReason::NoPicture)
    );
    assert_eq!(
      read2(&mut stream, &[hevc_slice(0)]),
      kept(KeptReason::CarriedOver),
      "{sei:x?}"
    );
    assert_eq!(read2(&mut stream, &[hevc_slice(0)]), Verdict::Disposable);
    // A first picture takes it up.
    let mut stream = NalStream::new(NalCodec::Hevc, &record);
    assert_eq!(
      read2(&mut stream, &[hevc_slice(21)]),
      kept(KeptReason::RandomAccess)
    );
    assert_eq!(read2(&mut stream, &[hevc_slice(0)]), Verdict::Disposable);
  }
  // A record with no SEI leaves nothing.
  let mut stream = NalStream::new(NalCodec::Hevc, &hvcc(0, 1, &[hevc_sps(0)]));
  assert_eq!(
    read2(&mut stream, &[hevc(35, 0, 0, &[0x50])]),
    kept(KeptReason::NoPicture)
  );
  assert_eq!(read2(&mut stream, &[hevc_slice(0)]), Verdict::Disposable);
}

#[test]
fn a_decoder_opened_afresh_at_a_seek_reads_the_opening_record_s_sei_again() {
  let record = hvcc(0, 1, &[hevc_sps(0), hevc_sei(39, 0, &[film_grain()])]);
  let mut stream = NalStream::new(NalCodec::Hevc, &record);
  assert_eq!(
    read2(&mut stream, &[hevc_slice(21)]),
    kept(KeptReason::RandomAccess)
  );
  assert_eq!(read2(&mut stream, &[hevc_slice(0)]), Verdict::Disposable);
  stream.seeked();
  assert_eq!(
    read2(&mut stream, &[hevc(35, 0, 0, &[0x50])]),
    kept(KeptReason::NoPicture)
  );
  assert_eq!(
    read2(&mut stream, &[hevc_slice(0)]),
    kept(KeptReason::CarriedOver)
  );
  assert_eq!(read2(&mut stream, &[hevc_slice(0)]), Verdict::Disposable);
  // A stream that opened with none has none pending after a seek.
  let mut stream = NalStream::new(NalCodec::Hevc, &hvcc(0, 1, &[hevc_sps(0)]));
  assert_eq!(
    read2(&mut stream, &[hevc_slice(21)]),
    kept(KeptReason::RandomAccess)
  );
  stream.seeked();
  assert_eq!(
    read2(&mut stream, &[hevc(35, 0, 0, &[0x50])]),
    kept(KeptReason::NoPicture)
  );
  assert_eq!(read2(&mut stream, &[hevc_slice(0)]), Verdict::Disposable);
}

#[test]
fn the_packet_after_an_hevc_record_may_go_unread() {
  // A record the HEVC decoder reads mid-stream may fail — one whose array
  // runs past its end always (`hevc/parse.c` 115–118), and under
  // `AV_EF_EXPLODE` one a parameter set or SEI unit of which does not
  // decode (71–74) — and it then returns before the packet
  // (`hevc/hevcdec.c` 3855–3860). Whatever is pending before the packet
  // stays pending after it.
  let plain = hvcc(0, 1, &[hevc_sps(0)]);
  let short = &plain[..plain.len() - 1];
  let user_data = hevc_sei(39, 0, &[sei_message(5, &[0x42; 16])]);
  for record in [&plain[..], short] {
    // SEI state a RASL picture the decoder skips leaves, which a picture the
    // decoder may not read cannot take up.
    let mut stream = hevc_stream(&plain);
    assert_eq!(
      read2(&mut stream, &[user_data.clone(), hevc_slice(9)]),
      kept(KeptReason::Companion(39))
    );
    assert_eq!(
      stream.read_with_record(&lengths(2, &[hevc_slice(1)]), Some(record)),
      kept(KeptReason::Referenced)
    );
    assert_eq!(
      read2(&mut stream, &[hevc_slice(0)]),
      kept(KeptReason::CarriedOver),
      "{record:x?}"
    );
    assert_eq!(read2(&mut stream, &[hevc_slice(0)]), Verdict::Disposable);
  }
  // Nor can it take up the record's own SEI.
  let grain = hvcc(0, 1, &[hevc_sps(0), hevc_sei(39, 0, &[film_grain()])]);
  let mut stream = hevc_stream(&plain);
  assert_eq!(
    stream.read_with_record(&lengths(2, &[hevc_slice(1)]), Some(&grain)),
    kept(KeptReason::Referenced)
  );
  assert_eq!(
    read2(&mut stream, &[hevc_slice(0)]),
    kept(KeptReason::CarriedOver)
  );
  assert_eq!(read2(&mut stream, &[hevc_slice(0)]), Verdict::Disposable);
  // The `eos` an end of sequence left waits past a packet that may go
  // unread.
  let mut stream = hevc_stream(&plain);
  assert_eq!(
    read2(&mut stream, &[hevc_slice(0), hevc(36, 0, 0, &[])]),
    kept(KeptReason::Companion(36))
  );
  assert_eq!(
    stream.read_with_record(&lengths(2, &[hevc_slice(0)]), Some(&plain)),
    kept(KeptReason::SequenceStart)
  );
  assert_eq!(
    read2(&mut stream, &[hevc_slice(0)]),
    kept(KeptReason::SequenceStart)
  );
  assert_eq!(read2(&mut stream, &[hevc_slice(0)]), Verdict::Disposable);
  // So does a recovery, past an IRAP picture that may go unread.
  let recovery = hevc_sei(39, 0, &[sei_message(6, &[0x80])]);
  let mut stream = hevc_stream(&plain);
  assert_eq!(
    read2(&mut stream, &[recovery.clone(), hevc_slice(1)]),
    kept(KeptReason::Companion(39))
  );
  assert_eq!(
    stream.read_with_record(&lengths(2, &[hevc_slice(21)]), Some(&plain)),
    kept(KeptReason::RandomAccess)
  );
  assert_eq!(
    read2(&mut stream, &[hevc_slice(0)]),
    kept(KeptReason::Recovering)
  );
  assert_eq!(
    read2(&mut stream, &[hevc_slice(21)]),
    kept(KeptReason::RandomAccess)
  );
  assert_eq!(read2(&mut stream, &[hevc_slice(0)]), Verdict::Disposable);
  // A record of no bytes the decoder does not read (`hevc/hevcdec.c`
  // 3856): its packet is read.
  let mut stream = hevc_stream(&plain);
  assert_eq!(
    read2(&mut stream, &[recovery, hevc_slice(1)]),
    kept(KeptReason::Companion(39))
  );
  assert_eq!(
    stream.read_with_record(&lengths(2, &[hevc_slice(21)]), Some(&[])),
    kept(KeptReason::RandomAccess)
  );
  assert_eq!(read2(&mut stream, &[hevc_slice(0)]), Verdict::Disposable);
}

#[test]
fn a_recovery_point_of_any_layer_starts_a_recovery() {
  // FFmpeg's decoder reads an SEI unit of every layer into its SEI state
  // (`hevc/hevcdec.c` 3587–3631), a recovery point of a layer above 0
  // among them.
  let recovery = hevc(39, 1, 0, &escaped(&sei_rbsp(&[sei_message(6, &[0x80])])));
  let mut stream = hevc_stream(&hvcc(0, 1, &[hevc_sps(0)]));
  assert_eq!(
    read2(&mut stream, &[recovery.clone(), hevc_slice(1)]),
    kept(KeptReason::Extension)
  );
  assert_eq!(
    read2(&mut stream, &[hevc_slice(0)]),
    kept(KeptReason::Recovering)
  );
  assert_eq!(
    read2(&mut stream, &[hevc_slice(21)]),
    kept(KeptReason::RandomAccess)
  );
  assert_eq!(read2(&mut stream, &[hevc_slice(0)]), Verdict::Disposable);
  // A record's, of any layer.
  let mut stream = hevc_stream(&hvcc(0, 1, &[hevc_sps(0)]));
  stream.reconfigure(&hvcc(0, 1, &[hevc_sps(0), recovery]));
  assert_eq!(
    read2(&mut stream, &[hevc_slice(1)]),
    kept(KeptReason::Referenced)
  );
  assert_eq!(
    read2(&mut stream, &[hevc_slice(0)]),
    kept(KeptReason::Recovering)
  );
}

#[test]
fn an_h264_record_s_sei_is_read_by_no_decoder() {
  // FFmpeg's H.264 decoder reads a record's parameter sets alone
  // (`h264_parse.c` 380–409) — the one it opens with, a packet's
  // `AV_PKT_DATA_NEW_EXTRADATA` and a packet that is an `avcC` alike
  // (`h264dec.c` 399–412, 1038–1050) — and its SEI state lasts one access
  // unit (593–598): an SEI unit in a record leaves nothing pending.
  let record = annex_b(&[
    avc_sps(),
    avc(3, 8, &[0xEE, 0x3C, 0x80]),
    avc_sei(&[sei_message(5, &[0x42; 16])]),
  ]);
  let mut stream = h264(&record);
  assert_eq!(stream.read(&annex_b(&[b_slice()])), Verdict::Disposable);
  assert_eq!(
    stream.read_with_record(&annex_b(&[p_slice()]), Some(&record)),
    kept(KeptReason::Referenced)
  );
  assert_eq!(stream.read(&annex_b(&[b_slice()])), Verdict::Disposable);
  stream.reconfigure(&record);
  assert_eq!(stream.read(&annex_b(&[b_slice()])), Verdict::Disposable);
}

/// Reads `nals` as one packet of four-byte lengths.
fn read4(stream: &mut NalStream, nals: &[Vec<u8>]) -> Verdict {
  stream.read(&lengths(4, nals))
}

#[test]
fn a_frame_under_an_sps_that_lets_fields_in_is_marked_as_a_progressive_one() {
  // An MBAFF frame or a PicAFF frame codes `field_pic_flag` 0
  // (`h264_slice.c` 1767–1773): no field waits before it or after it, so it
  // is marked where a progressive frame is.
  let mut stream = h264(&interlaced_avcc());
  assert_eq!(
    read4(&mut stream, &[frame_slice(0, 1)]),
    Verdict::Disposable
  );
  // Two slices of it; and a slice that would start a second frame, which
  // FFmpeg refuses after a frame (2124–2125).
  assert_eq!(
    read4(
      &mut stream,
      &[
        frame_slice(0, 1),
        slice_of(0, 8, 0, 1, Some(Structure::Frame))
      ]
    ),
    Verdict::Disposable
  );
  assert_eq!(
    read4(&mut stream, &[frame_slice(0, 1), frame_slice(0, 1)]),
    Verdict::Disposable
  );
  assert_eq!(
    read4(&mut stream, &[frame_slice(2, 1)]),
    kept(KeptReason::Referenced)
  );
  assert_eq!(
    read4(&mut stream, &[frame_slice(0, 2)]),
    Verdict::Disposable
  );
  assert_eq!(stream.pairing, Pairing::Settled);
  assert_eq!(stream.unmatched, 0);
}

#[test]
fn a_field_pair_is_marked_only_where_both_fields_ride_in_one_packet() {
  let mut stream = h264(&interlaced_avcc());
  let top = |frame_num| field_slice(0, frame_num, false);
  let bottom = |frame_num| field_slice(0, frame_num, true);
  // Top field first, and bottom field first.
  assert_eq!(
    read4(&mut stream, &[top(1), bottom(1)]),
    Verdict::Disposable
  );
  assert_eq!(
    read4(&mut stream, &[bottom(1), top(1)]),
    Verdict::Disposable
  );
  // Each field of more than one slice; an access unit delimiter and an SEI
  // unit of picture timing before each field.
  let more = |frame_num, bottom| {
    let structure = if bottom {
      Structure::Bottom
    } else {
      Structure::Top
    };
    slice_of(0, 8, 0, frame_num, Some(structure))
  };
  let aud = avc(0, 9, &[0xF0]);
  let sei = avc_sei(&[picture_timing()]);
  assert_eq!(
    read4(
      &mut stream,
      &[
        aud.clone(),
        sei.clone(),
        top(1),
        more(1, false),
        aud,
        sei,
        bottom(1),
        more(1, true)
      ]
    ),
    Verdict::Disposable
  );
  // Two fields FFmpeg does not join: of one parity — the second finds the
  // first unmatched (1583–1587) — and of another `frame_num`, which FFmpeg
  // refuses within a packet ("Frame num change", 1752–1758).
  let mut fresh = h264(&interlaced_avcc());
  assert_eq!(
    read4(&mut fresh, &[top(1), top(1)]),
    kept(KeptReason::UnpairedField)
  );
  let mut fresh = h264(&interlaced_avcc());
  assert_eq!(
    read4(&mut fresh, &[top(1), bottom(2)]),
    kept(KeptReason::UnpairedField)
  );
  // A field alone: the pair split across packets is judged together, each
  // half kept, since dropping one moves the other's pairing; after the
  // second, nothing waits.
  assert_eq!(
    read4(&mut stream, &[top(1)]),
    kept(KeptReason::UnpairedField)
  );
  assert!(matches!(stream.pairing, Pairing::Waiting(_)));
  assert_eq!(
    read4(&mut stream, &[bottom(1)]),
    kept(KeptReason::UnpairedField)
  );
  assert_eq!(stream.pairing, Pairing::Settled);
  assert_eq!(
    read4(&mut stream, &[top(2), bottom(2)]),
    Verdict::Disposable
  );
  assert_eq!(stream.unmatched, 0);
}

#[test]
fn a_picture_while_a_first_field_may_wait_is_kept() {
  let mut stream = h264(&interlaced_avcc());
  // A reference field alone waits for its pair (1610–1611).
  assert_eq!(
    read4(&mut stream, &[field_slice(2, 1, false)]),
    kept(KeptReason::Referenced)
  );
  // A frame while it waits finds it unmatched (1583–1587).
  assert_eq!(
    read4(&mut stream, &[frame_slice(0, 2)]),
    kept(KeptReason::PendingField)
  );
  assert_eq!(stream.pairing, Pairing::Settled);
  assert!(stream.unmatched > 0);
  // From then on FFmpeg's `missing_fields` may be raised, in any frame
  // thread, and a pair dropped would leave it raised: every pair is kept, for
  // good. A frame is marked still.
  assert_eq!(
    read4(
      &mut stream,
      &[field_slice(0, 2, false), field_slice(0, 2, true)]
    ),
    kept(KeptReason::UnmatchedFields)
  );
  assert_eq!(
    read4(&mut stream, &[frame_slice(0, 2)]),
    Verdict::Disposable
  );
  stream.seeked();
  let _ = read4(&mut stream, &[frame_slice(2, 2)]);
  assert_eq!(
    read4(
      &mut stream,
      &[field_slice(0, 3, false), field_slice(0, 3, true)]
    ),
    kept(KeptReason::UnmatchedFields)
  );
  assert_eq!(
    read4(&mut stream, &[frame_slice(0, 3)]),
    Verdict::Disposable
  );
}

#[test]
fn a_reference_field_and_a_non_reference_one_are_never_joined() {
  // "Found reference and non-reference fields in the same frame"
  // (1494–1501): FFmpeg refuses the second, and the first waits on.
  let mut stream = h264(&interlaced_avcc());
  assert_eq!(
    read4(&mut stream, &[field_slice(2, 1, false)]),
    kept(KeptReason::Referenced)
  );
  assert_eq!(
    read4(&mut stream, &[field_slice(0, 1, true)]),
    kept(KeptReason::UnpairedField)
  );
  assert!(matches!(
    stream.pairing,
    Pairing::Waiting(Waiting {
      bottom: false,
      reference: true,
      ..
    })
  ));
  assert_eq!(
    read4(&mut stream, &[frame_slice(0, 2)]),
    kept(KeptReason::PendingField)
  );
  // In one packet: the first field keeps it, unpaired; the refused one is
  // no reference picture FFmpeg decodes.
  let mut fresh = NalStream::new(NalCodec::H264, &interlaced_avcc());
  assert_eq!(
    read4(
      &mut fresh,
      &[field_slice(0, 1, false), field_slice(2, 1, true)]
    ),
    kept(KeptReason::UnpairedField)
  );
  assert!(!fresh.pending.reference_seen);
  assert_eq!(
    read4(&mut fresh, &[field_slice(0, 1, true)]),
    kept(KeptReason::UnpairedField)
  );
  assert_eq!(fresh.pairing, Pairing::Settled);
  assert_eq!(
    read4(&mut fresh, &[frame_slice(0, 2)]),
    kept(KeptReason::AwaitingReference)
  );
  assert_eq!(
    read4(
      &mut fresh,
      &[field_slice(2, 2, false), field_slice(2, 2, true)]
    ),
    kept(KeptReason::Referenced)
  );
  assert!(fresh.pending.reference_seen);
  assert_eq!(read4(&mut fresh, &[frame_slice(0, 3)]), Verdict::Disposable);
}

#[test]
fn each_slice_is_read_by_the_sps_its_pps_was_read_on() {
  // SPS 0 codes frames only, its `frame_num` of four bits; SPS 1 lets fields
  // in, its `frame_num` of six, of the same size in frame macroblocks, so
  // that FFmpeg passes from one to the other without setting its context up
  // again (`h264_slice.c` 1066–1123). PPS 0 is read on SPS 0, PPS 1 on SPS 1.
  let wide = Sps {
    id: 1,
    frames_only: false,
    log2_max_frame_num_minus4: 2,
    height_in_map_units_minus1: 33,
    ..Sps::HIGH
  };
  let mut stream = h264(&[]);
  assert_eq!(
    stream.read(&annex_b(&[avc_sps(), wide.unit(), pps(0, 0), pps(1, 1)])),
    kept(KeptReason::Companion(7))
  );
  assert_eq!(stream.frames_only(), Some(false));
  // A slice of PPS 0: four bits of `frame_num`, and no `field_pic_flag`
  // after them, though the bits there read 1 0, a top field, to a reading
  // that took SPS 1's.
  let frames = slice_bits(0, 0, 0, (1, 4), Some(Structure::Top));
  assert_eq!(stream.read(&annex_b(&[frames])), Verdict::Disposable);
  // A slice of PPS 1: six bits, then the flags.
  let field = |bottom| {
    let structure = if bottom {
      Structure::Bottom
    } else {
      Structure::Top
    };
    slice_bits(0, 0, 1, (1, 6), Some(structure))
  };
  assert_eq!(
    stream.read(&annex_b(&[field(false)])),
    kept(KeptReason::UnpairedField)
  );
  assert_eq!(
    stream.read(&annex_b(&[field(true)])),
    kept(KeptReason::UnpairedField)
  );
  assert_eq!(
    stream.read(&annex_b(&[field(false), field(true)])),
    Verdict::Disposable
  );
  assert_eq!(
    stream.read(&annex_b(&[slice_bits(
      0,
      0,
      1,
      (1, 6),
      Some(Structure::Frame)
    )])),
    Verdict::Disposable
  );
  // FFmpeg keeps a picture parameter set on the SPS it was read with
  // (`remove_sps`, `h264_ps.c` 92–104): a new SPS 0 that lets fields in
  // leaves the slices of PPS 0 frames, until PPS 0 is read again.
  let _ = stream.read(&annex_b(&[interlaced_sps()]));
  assert_eq!(
    stream.read(&annex_b(&[slice_bits(
      0,
      0,
      0,
      (1, 4),
      Some(Structure::Top)
    )])),
    Verdict::Disposable
  );
  let _ = stream.read(&annex_b(&[pps(0, 0)]));
  assert_eq!(
    stream.read(&annex_b(&[slice_bits(
      0,
      0,
      0,
      (1, 4),
      Some(Structure::Top)
    )])),
    kept(KeptReason::UnpairedField)
  );
}

/// A non-reference pair whose second field FFmpeg refuses: `between` read
/// after the first field, in the packet. The first field waits on, so the
/// next packet's pair, which would be marked were the pair joined, finds it
/// unmatched (`h264_slice.c` 1583–1587) and is kept for it.
fn parted_by(stream: &mut NalStream, between: &[Vec<u8>]) {
  let mut packet = vec![field_slice(0, 1, false)];
  packet.extend_from_slice(between);
  packet.push(field_slice(0, 1, true));
  assert!(!read4(stream, &packet).is_disposable());
  assert!(
    matches!(
      stream.pairing,
      Pairing::Waiting(Waiting {
        bottom: false,
        frame_num: 1,
        reference: false,
        ..
      })
    ),
    "{:?}",
    stream.pairing
  );
  assert_eq!(
    read4(stream, &[field_slice(0, 1, false), field_slice(0, 1, true)]),
    kept(KeptReason::PendingField)
  );
  assert_eq!(stream.pairing, Pairing::Settled);
  assert!(stream.unmatched > 0);
}

#[test]
fn a_picture_parameter_set_replaced_between_two_fields_parts_them() {
  // A second field in the packet of its first is no first slice
  // (`h264_slice.c` 2072, before the first field ends at 2111–2114), so
  // FFmpeg holds its picture parameter set to the active one by what it
  // compares (2150–2163), never by its id. PPS 0 read again between the
  // fields, on the same SPS, `transform_8x8_mode` 1: "PPS changed between
  // slices". Predicted, before this law: the pair joined, and the next
  // pair marked; planted, the id compared again, red.
  let mut stream = h264(&interlaced_avcc());
  parted_by(&mut stream, &[pps_tail(0, 0, true)]);
}

#[test]
fn a_picture_parameter_set_read_on_another_sps_between_two_fields_parts_them() {
  // SPS 1, SPS 0 but its id; PPS 0 read again on it between the fields:
  // another `sps_id` (2153). The planted id comparison: red.
  let mut stream = h264(&interlaced_avcc());
  let one = Sps {
    id: 1,
    frames_only: false,
    ..Sps::HIGH
  };
  let _ = read4(&mut stream, &[one.unit()]);
  parted_by(&mut stream, &[pps(0, 1)]);
}

#[test]
fn an_sps_read_again_between_two_fields_parts_them_where_ffmpeg_makes_another_object() {
  // SPS 0 again, its `nal_ref_idc` 2 — the same set, but FFmpeg's copy of
  // its unit, `data`, another (`h264_ps.c` 297–306), so FFmpeg makes a new
  // object (581–587) — and PPS 0 read on it, between the fields: "SPS
  // changed in the middle of the frame" (`h264_slice.c` 2159–2163). Planted,
  // the SPS told by its id: red.
  let mut stream = h264(&interlaced_avcc());
  let mut renamed = interlaced_sps();
  renamed[0] = 0x47;
  parted_by(&mut stream, &[renamed, pps(0, 0)]);
}

#[test]
fn an_sps_and_a_pps_read_again_alike_between_two_fields_keep_the_pair() {
  // The same SPS again, byte for byte: FFmpeg keeps its object (`h264_ps.c`
  // 581–584), and PPS 0 read again on it compares alike, its other content
  // aside. The pair joins, and the next one is marked. Planted, every SPS
  // read again a new object: red.
  let mut stream = h264(&interlaced_avcc());
  let _ = read4(
    &mut stream,
    &[
      field_slice(0, 1, false),
      interlaced_sps(),
      pps(0, 0),
      field_slice(0, 1, true),
    ],
  );
  assert_eq!(stream.pairing, Pairing::Settled);
  assert_eq!(
    read4(
      &mut stream,
      &[field_slice(0, 1, false), field_slice(0, 1, true)]
    ),
    Verdict::Disposable
  );
  // Two picture parameter sets alike in what FFmpeg compares join one pair.
  let mut two = h264(&interlaced_avcc());
  let _ = read4(&mut two, &[pps(1, 0)]);
  assert_eq!(
    read4(
      &mut two,
      &[
        field_slice(0, 1, false),
        slice_of(0, 0, 1, 1, Some(Structure::Bottom))
      ]
    ),
    Verdict::Disposable
  );
  // And a slice of the picture naming the other: one picture still.
  assert_eq!(
    read4(
      &mut two,
      &[
        field_slice(0, 1, false),
        slice_of(0, 8, 1, 1, Some(Structure::Top)),
        field_slice(0, 1, true)
      ]
    ),
    Verdict::Disposable
  );
}

#[test]
fn ffmpeg_keeps_an_sps_object_only_for_the_same_copy_of_its_unit() {
  let unit = interlaced_sps();
  let canon = Canon::of(&unit);
  assert!(matches!(canon, Canon::Known { .. }));
  assert_eq!(canon.same(Canon::of(&unit)), Some(true));
  // Another `nal_ref_idc`: another first byte of `data`.
  let mut renamed = unit.clone();
  renamed[0] = 0x47;
  assert_eq!(canon.same(Canon::of(&renamed)), Some(false));
  // Not told: a unit FFmpeg's splitter may end or read past at `00 00 00`,
  // one whose RBSP ends in a zero byte, and one of more than 128 bytes —
  // which is told apart from a shorter one.
  assert_eq!(Canon::of(&[0x67, 0x64, 0, 0, 0, 0x80]), Canon::Unknown);
  assert_eq!(Canon::of(&[0x67, 0x64, 0, 0, 3]), Canon::Unknown);
  let long = [vec![0x67], vec![0x5A; 200]].concat();
  assert_eq!(Canon::of(&long), Canon::Long);
  assert_eq!(canon.same(Canon::Long), Some(false));
  assert_eq!(Canon::Long.same(Canon::Long), None);
  // FFmpeg's copy is of the RBSP: a `03` after two zero bytes dropped, the
  // byte after it whatever it is (`h2645_parse.c` 101–118).
  assert_eq!(
    Canon::of(&[0x67, 0, 0, 3, 5, 0x80]).same(Canon::of(&[0x67, 0, 0, 5, 0x80])),
    Some(true)
  );
  // In the list: the same unit again keeps the entry's object, another
  // makes a fresh one, which a PPS read after it is read on.
  let mut stream = h264(&interlaced_avcc());
  let Slot::Holds(first) = stream.sets.sps[0] else {
    panic!("SPS 0 held");
  };
  let _ = read4(&mut stream, &[unit.clone()]);
  assert_eq!(stream.sets.sps[0], Slot::Holds(first));
  let _ = read4(&mut stream, &[renamed]);
  let Slot::Holds(second) = stream.sets.sps[0] else {
    panic!("SPS 0 held");
  };
  assert_eq!(first.object.same(second.object), Some(false));
  let Slot::Holds(pps_0) = stream.sets.pps[0] else {
    panic!("PPS 0 held");
  };
  assert_eq!(pps_0.compared.sps, first.object);
  let _ = read4(&mut stream, &[pps(0, 0)]);
  let Slot::Holds(pps_0) = stream.sets.pps[0] else {
    panic!("PPS 0 held");
  };
  assert_eq!(pps_0.compared.sps, second.object);
}

#[test]
fn a_new_geometry_between_two_fields_sets_the_context_up_again() {
  // A top field alone, then SPS 0 of another width, PPS 0 read on it, and
  // the bottom field. At its first slice FFmpeg finds another size and sets
  // its context up again (`h264_slice.c` 1066–1078, 1136–1165), flushing it:
  // `first_field` 0 (`h264dec.c` 469, `h264_slice.c` 989). The bottom field
  // is a first field, and waits; the top one is dropped, found unmatched by
  // nothing. Predicted, before this law: the two joined, and the next pair
  // marked; planted, no set-up again, red.
  let mut stream = h264(&interlaced_avcc());
  assert_eq!(
    read4(&mut stream, &[field_slice(0, 1, false)]),
    kept(KeptReason::UnpairedField)
  );
  let narrow = Sps {
    frames_only: false,
    width_in_mbs_minus1: 79,
    ..Sps::HIGH
  };
  let _ = read4(
    &mut stream,
    &[narrow.unit(), pps(0, 0), field_slice(0, 1, true)],
  );
  assert!(
    matches!(
      stream.pairing,
      Pairing::Waiting(Waiting { bottom: true, .. })
    ),
    "{:?}",
    stream.pairing
  );
  assert_eq!(stream.unmatched, 0);
  // What a flush unsets is awaited again.
  assert!(!stream.pending.reference_seen);
  // The next pair's top field joins the waiting bottom one (1604–1606), and
  // its bottom field is refused (2124–2125): FFmpeg's state, which the
  // packet moves.
  assert_eq!(
    read4(
      &mut stream,
      &[field_slice(0, 1, false), field_slice(0, 1, true)]
    ),
    kept(KeptReason::PendingField)
  );
  assert_eq!(stream.pairing, Pairing::Settled);
  // A picture at which the context may be set up again is the first after a
  // flush: of the same size it is not.
  assert_eq!(
    read4(&mut stream, &[frame_slice(2, 2)]),
    kept(KeptReason::Referenced)
  );
  assert_eq!(
    read4(&mut stream, &[frame_slice(0, 3)]),
    Verdict::Disposable
  );
  let _ = read4(
    &mut stream,
    &[
      Sps {
        frames_only: false,
        ..Sps::HIGH
      }
      .unit(),
      pps(0, 0),
    ],
  );
  assert_eq!(
    read4(&mut stream, &[frame_slice(0, 3)]),
    kept(KeptReason::AwaitingReference)
  );
}

#[test]
fn a_sample_aspect_ratio_or_a_colour_matrix_ffmpeg_finds_changed_sets_the_context_up_again() {
  let described = |sar, matrix| {
    Sps {
      frames_only: false,
      vui: Vui::Described { sar, matrix },
      ..Sps::HIGH
    }
    .unit()
  };
  // From one SPS to another of the same size, ratio and matrix FFmpeg sets
  // nothing up again: the same object or not, it compares what they hold.
  let mut stream = h264(&avcc_of(3, &described((4, 3), Some(1))));
  assert_eq!(
    read4(&mut stream, &[frame_slice(0, 1)]),
    Verdict::Disposable
  );
  let _ = read4(&mut stream, &[described((8, 6), Some(1)), pps(0, 0)]);
  assert_eq!(
    read4(&mut stream, &[frame_slice(0, 1)]),
    Verdict::Disposable
  );
  // Another ratio (1094), or another matrix (1117–1123): set up again.
  let _ = read4(&mut stream, &[described((16, 11), Some(1)), pps(0, 0)]);
  assert_eq!(
    read4(&mut stream, &[frame_slice(0, 1)]),
    kept(KeptReason::AwaitingReference)
  );
  let _ = read4(&mut stream, &[frame_slice(2, 1)]);
  let _ = read4(&mut stream, &[described((16, 11), Some(6)), pps(0, 0)]);
  assert_eq!(
    read4(&mut stream, &[frame_slice(0, 2)]),
    kept(KeptReason::AwaitingReference)
  );
  // A ratio `av_image_check_sar` refuses at the picture's size, which
  // `ff_set_sar` replaces with 0:1 (`utils.c` 106–119): FFmpeg finds the two
  // apart at every first slice, and the reading may never say it does not.
  assert_eq!(
    sar_holds((65_535, 1), (1_073, 1_073), (1_920, 1_088)),
    Some(false)
  );
  assert_eq!(
    sar_holds((4, 3), (1_073, 1_073), (1_920, 1_088)),
    Some(true)
  );
  assert_eq!(sar_holds((0, 1), (1, 1), (16, 16)), Some(true));
  assert_eq!(sar_holds((1, 20), (1, 1), (32, 32)), None);
  let mut refused = h264(&avcc_of(3, &described((65_535, 1), None)));
  let _ = read4(&mut refused, &[frame_slice(2, 1)]);
  for frame_num in 2..4 {
    let _ = read4(&mut refused, &[frame_slice(2, frame_num)]);
    assert_eq!(
      read4(&mut refused, &[frame_slice(0, frame_num + 1)]),
      kept(KeptReason::AwaitingReference)
    );
  }
}

#[test]
fn a_slice_the_reading_cannot_read_for_its_field_keeps_its_packet() {
  let mut stream = h264(&interlaced_avcc());
  // A header that ends before `field_pic_flag`: two bits of its four-bit
  // `frame_num`, then the stop bit.
  let mut cut = Bits::new();
  cut.ue(0).ue(6).ue(0).bits(1, 2);
  assert_eq!(
    read4(&mut stream, &[avc(0, 1, &cut.finish())]),
    kept(KeptReason::MaybeField)
  );
  // A slice FFmpeg refuses before a field: under `AV_EF_EXPLODE` it reads
  // nothing after the refusal (`h264dec.c` 740–745).
  assert_eq!(
    read4(
      &mut stream,
      &[b_slice(), field_slice(0, 1, false), field_slice(0, 1, true)]
    ),
    kept(KeptReason::MaybeField)
  );
  // A picture parameter set of redundant counts: a slice whose header ends
  // before its `redundant_pic_cnt`, which FFmpeg may discard or not
  // (2081–2085).
  let _ = read4(&mut stream, &[redundant_pps(2, 0)]);
  let mut short = Bits::new();
  short.ue(0).ue(6).ue(2).bits(1, 4).bit(0).bits(6, 4);
  assert_eq!(
    read4(&mut stream, &[avc(0, 1, &short.finish())]),
    kept(KeptReason::MaybeField)
  );
}

#[test]
fn a_packet_not_read_whole_leaves_the_pairing_unknown() {
  let mut stream = h264(&interlaced_avcc());
  // A malformed packet holding a slice: FFmpeg may have started a field of
  // it, or found one unmatched.
  let mut packet = lengths(4, &[field_slice(2, 1, false)]);
  packet.extend_from_slice(&[0, 0]);
  assert_eq!(stream.read(&packet), kept(KeptReason::Malformed));
  assert_eq!(stream.pairing, Pairing::Unknown);
  assert!(stream.unmatched > 0);
  assert_eq!(
    read4(&mut stream, &[frame_slice(0, 2)]),
    kept(KeptReason::PendingField)
  );
  // A frame settles it.
  assert_eq!(stream.pairing, Pairing::Settled);
  assert_eq!(
    read4(&mut stream, &[frame_slice(0, 2)]),
    Verdict::Disposable
  );
  // A packet of no bytes may be the stream's end, after which nothing waits.
  let mut ended = h264(&interlaced_avcc());
  let _ = read4(&mut ended, &[field_slice(2, 1, false)]);
  assert_eq!(ended.read(&[]), kept(KeptReason::NoPicture));
  assert_eq!(ended.pairing, Pairing::Unknown);
  let mut settled = h264(&interlaced_avcc());
  assert_eq!(settled.read(&[]), kept(KeptReason::NoPicture));
  assert_eq!(settled.pairing, Pairing::Settled);
}

#[test]
fn a_seek_leaves_no_field_waiting_and_forgets_lists_that_moved() {
  // FFmpeg's flush holds no first field (`h264dec.c` 469).
  let mut stream = h264(&interlaced_avcc());
  let _ = read4(&mut stream, &[field_slice(2, 1, false)]);
  stream.seeked();
  assert_eq!(stream.pairing, Pairing::Settled);
  let _ = read4(&mut stream, &[frame_slice(2, 1)]);
  assert_eq!(
    read4(&mut stream, &[frame_slice(0, 2)]),
    Verdict::Disposable
  );
  // In-band parameter sets the record did not hold: a decoder opened afresh
  // at the seek holds the record's alone, and the reading neither.
  let mut raw = h264(&[]);
  let _ = raw.read(&annex_b(&[interlaced_sps(), pps(0, 0), frame_slice(2, 1)]));
  assert_eq!(
    raw.read(&annex_b(&[frame_slice(0, 2)])),
    Verdict::Disposable
  );
  raw.seeked();
  assert_eq!(raw.sets.pps[0], Slot::Unknown);
  // An entry empty since the opening stays empty.
  assert_eq!(raw.sets.pps[1], Slot::Empty);
  let _ = raw.read(&annex_b(&[frame_slice(2, 2)]));
  assert_eq!(
    raw.read(&annex_b(&[frame_slice(0, 3)])),
    kept(KeptReason::MaybeField)
  );
  let _ = raw.read(&annex_b(&[interlaced_sps(), pps(0, 0), frame_slice(2, 3)]));
  assert_eq!(
    raw.read(&annex_b(&[frame_slice(0, 4)])),
    Verdict::Disposable
  );
  // In-band ones the record holds already move nothing.
  let mut record = h264(&interlaced_avcc());
  let _ = read4(
    &mut record,
    &[interlaced_sps(), pps(0, 0), frame_slice(2, 1)],
  );
  record.seeked();
  let _ = read4(&mut record, &[frame_slice(2, 2)]);
  assert_eq!(
    read4(&mut record, &[frame_slice(0, 3)]),
    Verdict::Disposable
  );
}

#[test]
fn a_record_s_parameter_sets_are_read_as_ffmpeg_reads_them() {
  // An Annex B record, read whole.
  let mut annex = h264(&annex_b(&[interlaced_sps(), pps(0, 0)]));
  assert_eq!(
    annex.read(&annex_b(&[frame_slice(0, 1)])),
    Verdict::Disposable
  );
  // A picture parameter set FFmpeg refuses — two slice groups, FMO, which
  // it reads none of (`h264_ps.c` 757–762) — leaves its entry empty.
  let mut fmo = Bits::new();
  fmo.ue(0).ue(0).bit(0).bit(0).ue(1).ue(0);
  let refused = avc(3, 8, &fmo.finish());
  let mut groups = h264(&annex_b(&[interlaced_sps(), refused]));
  assert_eq!(groups.sets.pps[0], Slot::Empty);
  // A slice of it starts no picture.
  assert_eq!(
    groups.read(&annex_b(&[frame_slice(0, 1)])),
    Verdict::Disposable
  );
  // In an Annex B record, a set that may not decode may end the record
  // (`h264_parse.c` 380–409): the sets after it may go unread.
  let cut = &interlaced_sps()[..4];
  let after = h264(&annex_b(&[cut.to_vec(), interlaced_sps(), pps(0, 0)]));
  assert_eq!(after.sets.pps[0], Slot::Unknown);
  // An `avcC` entry of two units: FFmpeg splits it otherwise than the
  // reading can tell.
  let both = [interlaced_sps(), vec![0, 0, 1], pps(0, 0)].concat();
  let mut record = vec![1, 0x64, 0x00, 0x28, 0xFF, 0xE1];
  record.extend_from_slice(&(both.len() as u16).to_be_bytes());
  record.extend_from_slice(&both);
  record.push(0);
  let split = h264(&record);
  assert_eq!(split.sets.pps[0], Slot::Unknown);
}

#[test]
fn a_picture_parameter_set_is_read_as_ffmpeg_reads_it() {
  let mut sets = ParameterSets::EMPTY;
  let header = |sps: &[u8]| {
    sps_facts(BitReader::new(&sps[1..], Reading::Rbsp))
      .header
      .expect("a header")
  };
  let object = Object::Known {
    sps_id: 0,
    number: 1,
    fresh: true,
  };
  let entry = |sps: &[u8]| SpsEntry {
    header: header(sps),
    object,
    canon: Canon::of(sps),
  };
  sets.sps[0] = Slot::Holds(entry(&interlaced_sps()));
  let read = |bits: &mut Bits, sets: &ParameterSets| {
    let unit = bits.finish();
    pps_read(&unit, Reading::Rbsp, &sets.sps)
  };
  // The base one, and its parts FFmpeg refuses: an SPS it does not hold,
  // slice groups, 33 references, a chroma offset of 13.
  let base = |pps_id: u32, sps_id: u32, groups: u32, refs: u32, offset: i32| {
    let mut bits = Bits::new();
    bits
      .ue(pps_id)
      .ue(sps_id)
      .bit(0)
      .bit(0)
      .ue(groups)
      .ue(refs)
      .ue(0)
      .bit(0)
      .bits(0, 2)
      .se(0)
      .se(0)
      .se(offset)
      .bit(1)
      .bit(0)
      .bit(0);
    bits
  };
  assert!(matches!(
    read(&mut base(0, 0, 0, 0, 0), &sets),
    PpsRead::Stored(0, _)
  ));
  assert_eq!(
    read(&mut base(255, 0, 0, 31, -12), &sets),
    PpsRead::Stored(
      255,
      PpsHeader {
        sps: header(&interlaced_sps()),
        redundant: false,
        pic_order_present: false,
        compared: Compared {
          sps_id: Some(0),
          transform_8x8: Some(false),
          sps: object,
        },
      }
    )
  );
  for mut refused in [
    base(256, 0, 0, 0, 0),
    base(0, 1, 0, 0, 0),
    base(0, 32, 0, 0, 0),
    base(0, 0, 1, 0, 0),
    base(0, 0, 0, 32, 0),
    base(0, 0, 0, 0, 13),
  ] {
    assert_eq!(read(&mut refused, &sets), PpsRead::Refused);
  }
  // An SPS of 11 bits, on which FFmpeg reads no PPS.
  let eleven = Sps {
    luma_minus8: 3,
    chroma_minus8: 3,
    ..Sps::HIGH
  }
  .unit();
  let mut deep = ParameterSets::EMPTY;
  deep.sps[0] = Slot::Holds(entry(&eleven));
  assert_eq!(read(&mut base(0, 0, 0, 0, 0), &deep), PpsRead::Refused);
  // The tail: `transform_8x8_mode_flag`, a scaling matrix of one list
  // written, and a second offset; a delta of 128, and an offset of 13.
  let tail = |delta: i32, offset: i32| {
    let mut bits = base(0, 0, 0, 0, 0);
    bits.bit(1).bit(1).bit(1).se(delta);
    for _ in 1..16 {
      bits.se(0);
    }
    for _ in 1..8 {
      bits.bit(0);
    }
    bits.se(offset);
    bits
  };
  assert!(matches!(
    read(&mut tail(3, 2), &sets),
    PpsRead::Stored(
      0,
      PpsHeader {
        compared: Compared {
          transform_8x8: Some(true),
          ..
        },
        ..
      }
    )
  ));
  assert_eq!(read(&mut tail(128, 2), &sets), PpsRead::Refused);
  assert_eq!(read(&mut tail(3, 13), &sets), PpsRead::Refused);
  // A Baseline SPS of a constraint flag: FFmpeg reads no tail.
  let mut baseline = ParameterSets::EMPTY;
  let constrained_sps = {
    let mut unit = Sps::BASELINE.unit();
    unit[2] = 0xC0;
    unit
  };
  baseline.sps[0] = Slot::Holds(entry(&constrained_sps));
  assert!(!header(&constrained_sps).pps_tail);
  assert!(matches!(
    read(&mut tail(128, 13), &baseline),
    PpsRead::Stored(
      0,
      PpsHeader {
        compared: Compared {
          transform_8x8: Some(false),
          ..
        },
        ..
      }
    )
  ));
  // Cut short, it is undecided; unread to its id, the more so.
  let mut short = base(0, 0, 0, 0, 0).finish();
  short.truncate(1);
  assert!(matches!(
    pps_read(&short, Reading::Rbsp, &sets.sps),
    PpsRead::Undecided(_)
  ));
  assert_eq!(
    pps_read(&[], Reading::Rbsp, &sets.sps),
    PpsRead::Undecided(None)
  );
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
    stream.pending.reference_seen = true;
    stream.pending.intra_seen = true;
    stream.pending.sequence_start = false;
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
