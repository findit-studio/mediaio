//! The decoder configuration records MP4 and FLV write through
//! libavformat's helpers whose answers they ignore (`movenc.c` 1573–1738,
//! 1102–1201, 1599–1626; `flvenc.c` 886–899 in FFmpeg 9.0.1): what each
//! helper does with a configuration, mirrored from its source, so that a
//! configuration it would refuse, or write otherwise than it is, is refused
//! by name before anything is handed over (table C′′′ of the census, see
//! [the module](super)).
//!
//! **The trust model's boundary.** A condition a configuration of a stream
//! the standards allow can reach is mirrored exactly. A condition only a
//! parameter set the standard does not allow reaches — a value past the
//! range the standard caps, syntax that runs out or does not parse — rests
//! on the trust model ("a stream is one the standards allow") and is not
//! refused; where a mirrored condition's parse passes one, the parse stops
//! there and the configuration is taken. Those conditions, by writer:
//! HEVC's SPS of more than 64 short-term reference picture sets, or of a
//! set whose pictures' bits run past the SPS (`hevc.c` 739–740, 608–611);
//! VVC's VPS or SPS of eight sub-layers, which the standard caps at seven,
//! its arrays indexed past their end and its 3-bit count wrapped (`vvc.c`
//! 146–150, 208–216, 706); EVC's SPS of an id past 15 or a chroma
//! format past 3 (`evc.c` 95–96, 112–113); LCEVC's block of a reserved size type, a
//! size past `INT_MAX`, or running to its unit's end, and its global
//! configuration of a reserved resolution type or of no dimensions
//! (`lcevc.c` 64–74, 147–148); VC-1's sequence header of another profile
//! than the advanced one (`movenc.c` 1137–1141).

use std::borrow::Cow;

use mediaio_ffmpeg::ffi::pix_fmt_sampling;

/// The most bytes `init_get_bits8` takes: its bits must stay below
/// `INT_MAX - AV_INPUT_BUFFER_PADDING_SIZE * 8` (`get_bits.h` 517–550 in
/// FFmpeg 9.0.1).
pub(super) const GET_BITS_BYTES_MAX: usize = (i32::MAX as usize - 64 * 8 - 1) / 8;

/// `AVERROR_INVALIDDATA`, which `get_ue_golomb` answers for a code longer
/// than its short reader takes.
const INVALIDDATA: i32 = -0x4144_4E49;

/// `AV_PROFILE_UNKNOWN`.
const PROFILE_UNKNOWN: i32 = -99;

/// What a configuration writer does with a configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Wrote {
  /// Written as the configuration holds it.
  Whole,
  /// Its failure answered: nothing written, or the record cut where the
  /// writer stopped.
  Failed,
  /// Success answered, the record written otherwise than the
  /// configuration holds it: empty, a unit's length cut to its 16 bits, or
  /// a count wrapped in them.
  Corrupt,
}

impl Wrote {
  /// Whether the record is written as the configuration holds it.
  pub(super) const fn whole(self) -> bool {
    matches!(self, Self::Whole)
  }
}

/// Bits read as libavcodec's checked reader reads them (`get_bits.h`
/// 107–700 in FFmpeg 9.0.1, its cached reader off): `bits` bits over
/// `bytes`, which go on past them — a configuration's next bytes, or the
/// zeroed padding past its end — and the place held at 8 bits past them.
#[derive(Clone, Copy)]
struct Reader<'a> {
  bytes: &'a [u8],
  bits: usize,
  index: usize,
}

impl<'a> Reader<'a> {
  /// The reader `init_get_bits8` makes of `bytes`, whole.
  fn new(bytes: &'a [u8]) -> Self {
    Self {
      bytes,
      bits: bytes.len() * 8,
      index: 0,
    }
  }

  /// `size_in_bits_plus8`.
  fn bound(&self) -> usize {
    self.bits + 8
  }

  fn byte(&self, at: usize) -> u8 {
    self.bytes.get(at).copied().unwrap_or(0)
  }

  /// `get_bits_left`, below zero up to 8 bits past the end.
  fn left(&self) -> i64 {
    self.bits as i64 - self.index as i64
  }

  /// `get_bits_count`.
  fn count(&self) -> usize {
    self.index
  }

  /// Whether the place is held: every read from it answers zeros and
  /// moves nothing.
  fn exhausted(&self) -> bool {
    self.index >= self.bound()
  }

  /// `UPDATE_CACHE`: the 32 bits at the place, those before it in its
  /// byte shifted out and zeros shifted in.
  fn cache(&self) -> u32 {
    let at = self.index / 8;
    let word = u32::from_be_bytes([
      self.byte(at),
      self.byte(at + 1),
      self.byte(at + 2),
      self.byte(at + 3),
    ]);
    word << (self.index % 8)
  }

  /// `UPDATE_CACHE_32` of a 64-bit build: 32 bits at the place, whole.
  fn cache32(&self) -> u32 {
    let at = self.index / 8;
    let word = u64::from_be_bytes(core::array::from_fn(|offset| self.byte(at + offset)));
    ((word << (self.index % 8)) >> 32) as u32
  }

  /// `SKIP_COUNTER`.
  fn skip(&mut self, count: usize) {
    self.index = self.index.saturating_add(count).min(self.bound());
  }

  /// `skip_bits_long`: either way, held within the buffer.
  fn skip_long(&mut self, count: i64) {
    let index = self.index as i64;
    self.index = (index + count.clamp(-index, self.bound() as i64 - index)) as usize;
  }

  /// `get_bits`, 1 to 25 bits.
  fn read(&mut self, count: u32) -> u32 {
    let value = self.cache() >> (32 - count);
    self.skip(count as usize);
    value
  }

  /// `get_bits1`.
  fn read1(&mut self) -> u32 {
    let value = u32::from((self.byte(self.index / 8) << (self.index % 8)) >> 7);
    if self.index < self.bound() {
      self.index += 1;
    }
    value
  }

  /// `get_bits_long`, 0 to 32 bits.
  fn read_long(&mut self, count: u32) -> u32 {
    match count {
      0 => 0,
      1..=25 => self.read(count),
      _ => {
        let value = self.cache32() >> (32 - count);
        self.skip(count as usize);
        value
      }
    }
  }

  /// `get_ue_golomb_long`: of no one bit in its 32, `UINT_MAX`.
  fn ue_long(&mut self) -> u32 {
    let mut peek = *self;
    let buf = peek.read_long(32);
    let log = 31 - log2(buf);
    self.skip_long(i64::from(log));
    self.read_long(log + 1).wrapping_sub(1)
  }

  /// `get_se_golomb_long`: its bits `ue_long`'s.
  fn se_long(&mut self) -> i32 {
    let buf = self.ue_long();
    let sign = (buf & 1) as i32 - 1;
    (((buf >> 1) as i32) ^ sign).wrapping_add(1)
  }

  /// `get_ue_golomb`, its short reader (`golomb.h` 53–102): a code of up to
  /// nine bits from its table, a longer one from its cache — its bits read
  /// either way, one past 25 answering `AVERROR_INVALIDDATA`.
  fn ue(&mut self) -> i32 {
    let buf = self.cache();
    if buf >= 1 << 27 {
      let prefix = buf >> 23;
      let len = 2 * (prefix.leading_zeros() - 23) + 1;
      self.skip(len as usize);
      (prefix >> (9 - len)) as i32 - 1
    } else {
      let log = 2 * log2(buf) as i32 - 31;
      self.skip((32 - log) as usize);
      if log < 7 {
        return INVALIDDATA;
      }
      ((buf >> log) - 1) as i32
    }
  }

  /// `align_get_bits`.
  fn align(&mut self) {
    self.skip(self.index.wrapping_neg() & 7);
  }
}

/// `av_log2`: of zero, zero.
fn log2(value: u32) -> u32 {
  31 - (value | 1).leading_zeros()
}

/// Whether `bytes` opens on a 3- or 4-byte start code, as the writers
/// test it (`AV_RB24(data) == 1 || AV_RB32(data) == 1`).
fn annex_b(bytes: &[u8]) -> bool {
  bytes.starts_with(&[0, 0, 1]) || bytes.starts_with(&[0, 0, 0, 1])
}

/// The NAL units libavformat's Annex B splitter finds in `bytes`, each as
/// it writes it — the bytes after a start code to the next, less their
/// trailing zeros, those before the first dropped (`nal_parse_units`,
/// `nal.c` 74–111 in FFmpeg 9.0.1; `ff_nal_find_startcode`, 31–72): what
/// the Annex B rewrite writes behind lengths, and the configuration
/// writers walk.
pub(super) fn nal_units(bytes: &[u8]) -> Vec<&[u8]> {
  // `nal_find_startcode_internal` looks no further than four bytes before
  // the end: a start code in the last three is no start code (31–66).
  let find = |from: usize| -> usize {
    let found = (from..bytes.len().saturating_sub(3))
      .find(|&at| bytes[at] == 0 && bytes[at + 1] == 0 && bytes[at + 2] == 1)
      .unwrap_or(bytes.len());
    if from < found && found < bytes.len() && bytes[found - 1] == 0 {
      found - 1
    } else {
      found
    }
  };
  let mut units = Vec::new();
  let mut start = find(0);
  loop {
    // Past the zeros and the 1 of the start code.
    while start < bytes.len() {
      let byte = bytes[start];
      start += 1;
      if byte != 0 {
        break;
      }
    }
    if start >= bytes.len() {
      break;
    }
    let end = find(start);
    let mut size = end - start;
    while size > 0 && bytes[start + size - 1] == 0 {
      size -= 1;
    }
    units.push(&bytes[start..start + size]);
    start = end;
  }
  units
}

/// Each unit the configuration writers' walk reads of the buffer
/// `ff_nal_parse_units_buf` makes of `bytes` — every unit behind its
/// 4-byte length, walked while more than four bytes are left (`avc.c`
/// 67–101; `hevc.c` 1380–1390; `vvc.c` 907–925) — with the two bytes after
/// its length, which they read its type from: past a unit shorter than
/// two, the next unit's length, and past the buffer its padding's zeros.
/// Each length is the unit's own, so a step is a unit; a last unit of none
/// leaves four bytes, and is not walked.
fn walked(bytes: &[u8]) -> Vec<(&[u8], [u8; 2])> {
  let units = nal_units(bytes);
  let mut out = Vec::with_capacity(units.len());
  for (index, unit) in units.iter().enumerate() {
    let next = units
      .get(index + 1)
      .map(|next| (next.len() as u32).to_be_bytes());
    if unit.is_empty() && next.is_none() {
      break;
    }
    // The bytes after this unit's length: its own, then the next length's.
    let after = |at: usize| -> u8 {
      match unit.get(at) {
        Some(&byte) => byte,
        None => next.map_or(0, |length| length[at - unit.len()]),
      }
    };
    out.push((*unit, [after(0), after(1)]));
  }
  out
}

/// The bytes of the RBSP `ff_nal_unit_extract_rbsp` makes of the NAL unit
/// `nal`: its own, less each emulation prevention byte after its first
/// `header` (`nal.c` 163–193).
fn nal_rbsp_len(nal: &[u8], header: usize) -> usize {
  let mut dropped = 0;
  let mut at = header.min(nal.len());
  while at + 2 < nal.len() {
    if nal[at] == 0 && nal[at + 1] == 0 && nal[at + 2] == 3 {
      dropped += 1;
      at += 3;
    } else {
      at += 1;
    }
  }
  nal.len() - dropped
}

/// The RBSP `ff_nal_unit_extract_rbsp` makes of the NAL unit `nal`
/// (`nal.c` 163–193): its first `header` bytes as they are, then every
/// emulation prevention byte — a 3 after two zeros — dropped.
fn nal_rbsp(nal: &[u8], header: usize) -> Vec<u8> {
  let mut out = Vec::with_capacity(nal.len());
  let mut at = 0;
  while at < header && at < nal.len() {
    out.push(nal[at]);
    at += 1;
  }
  while at + 2 < nal.len() {
    if nal[at] == 0 && nal[at + 1] == 0 && nal[at + 2] == 3 {
      out.extend_from_slice(&[0, 0]);
      at += 3;
    } else {
      out.push(nal[at]);
      at += 1;
    }
  }
  out.extend_from_slice(&nal[at..]);
  out
}

/// What `ff_isom_write_avcc` does with an H.264 configuration (`avc.c`
/// 32–143 in FFmpeg 9.0.1): fewer than 7 bytes it refuses; one opening on
/// no start code — an `avcC` — it writes as it is; of an Annex B one, its
/// sequence parameter sets, picture parameter sets and SPS extensions (NAL
/// unit types 7, 8 and 13), each at most 65 535 bytes, its 16-bit length,
/// and fewer than 32 SPSs, 256 PPSs and 256 extensions, the one that
/// reaches a count refused; the SPSs' lengths and bytes at least 6, a PPS
/// at least one. `ff_avc_decode_sps` fails only on an allocation: its
/// reader is the checked one, over at most 31 sets of 65 535 bytes.
pub(super) fn avcc(config: &[u8]) -> Wrote {
  if config.len() <= 6 {
    return Wrote::Failed;
  }
  if !annex_b(config) {
    return Wrote::Whole;
  }
  let (mut sps, mut pps, mut extensions) = (0_u32, 0_u32, 0_u32);
  let (mut sps_bytes, mut pps_bytes) = (0_usize, 0_usize);
  for (unit, [first, _]) in walked(config) {
    let (count, cap, bytes) = match first & 0x1F {
      7 => (&mut sps, 32, Some(&mut sps_bytes)),
      8 => (&mut pps, 256, Some(&mut pps_bytes)),
      13 => (&mut extensions, 256, None),
      _ => continue,
    };
    *count += 1;
    if unit.len() > usize::from(u16::MAX) || *count >= cap {
      return Wrote::Failed;
    }
    if let Some(bytes) = bytes {
      *bytes += 2 + unit.len();
    }
  }
  if sps_bytes < 6 || pps_bytes == 0 {
    Wrote::Failed
  } else {
    Wrote::Whole
  }
}

/// The HEVC NAL unit types `ff_isom_write_hvcc` keeps, in its arrays'
/// order: VPS, SPS, PPS, prefix SEI, suffix SEI (`hevc.c` 1278–1280).
const HEVC_ARRAYS: [u8; 5] = [32, 33, 34, 39, 40];

/// What `write_configuration_record` holds of an HEVC configuration as it
/// goes (`hevc.c` 44–88, 840–933).
#[derive(Default)]
struct Hvcc {
  /// Each array's count of NAL units, held to 65 535 (847–848).
  counts: [u32; 5],
  /// Each VPS parsed, in order: its id and its sub-layers less one, which
  /// a multilayer SPS of it reads (642–652).
  vps: Vec<(u8, u8)>,
  /// The auxiliary alpha layer a VPS extension names, whose parameter sets
  /// the record keeps beside the base layer's (431, 452, 888).
  alpha: u8,
  /// A unit kept past 65 535 bytes, its 16-bit length cut (856, 1194).
  cut: bool,
}

/// How a parse of a parameter set ends, as far as the record needs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Parse {
  /// Read to where nothing after it fails.
  Done,
  /// A failure a set the standards allow reaches: the record refused.
  Failed,
  /// A failure only a set the standard does not allow reaches: the trust
  /// model's, not refused.
  Trusted,
}

/// What `ff_isom_write_hvcc` does with an HEVC configuration (`hevc.c`
/// 1293–1406 in FFmpeg 9.0.1), `complete` its parameter set arrays'
/// completeness — MP4's under the `hvc1` tag (`movenc.c` 1639–1652), not
/// FLV's (`flvenc.c` 889) nor Matroska's (`matroskaenc.c` 1176). Fewer than
/// 6 bytes refused. An `hvcC` — of version 1 — of fewer than 23 bytes, or
/// of more than `init_get_bits8` takes, refused; its arrays walked, a unit
/// whose length runs past the record answering success with nothing
/// written (1352–1353). Else one opening on no start code refused; its
/// Annex B units of the five kept types each read whole as an RBSP, which
/// `init_get_bits8` must take, its header's layer kept where it is the base
/// layer or the alpha layer a VPS extension names, a sixth array count of
/// 65 536 refused, a unit of more than 65 535 bytes written with its length
/// cut (847–856), each VPS, SPS and PPS parsed: an SPS of a multilayer
/// extension whose VPS the record holds no earlier refused, and one of more
/// than 31 long-term reference pictures, which the standard allows up to 32
/// (`hevc/ps.c` 1524), refused (642–652, 750–751). Complete, it
/// refuses arrays of no VPS, SPS or PPS, or of more than 16, 16 and 64
/// (1087–1093). `ff_isom_write_lhvc` is never reached here: MP4 writes
/// `lhvC` for a stream flagged multilayer alone (2893–2898), and the
/// stream is handed only the flags the report keeps, of which MP4 keeps no
/// multilayer one (`kept`, see [the module](super)).
pub(super) fn hvcc(config: &[u8], complete: bool) -> Wrote {
  if config.len() < 6 {
    return Wrote::Failed;
  }
  let mut record = Hvcc::default();
  if config[0] == 1 {
    if config.len() < 23 || config.len() > GET_BITS_BYTES_MAX {
      return Wrote::Failed;
    }
    let mut bits = Reader::new(config);
    // The record's fields before its arrays (1317–1338).
    bits.skip(176);
    let arrays = bits.read(8);
    for _ in 0..arrays {
      bits.skip(2);
      let kind = bits.read(6) as u8;
      let nalus = bits.read(16);
      for _ in 0..nalus {
        let len = bits.read(16) as usize;
        if len as i64 > bits.left() / 8 {
          return Wrote::Corrupt;
        }
        let start = bits.count() / 8;
        let nal = config.get(start..start + len).unwrap_or(&[]);
        if let Some(array) = HEVC_ARRAYS.iter().position(|&kept| kept == kind)
          && hvcc_add(&mut record, nal, array, false) == Parse::Failed
        {
          return Wrote::Failed;
        }
        bits.skip_long(len as i64 * 8);
      }
    }
  } else if !annex_b(config) {
    return Wrote::Failed;
  } else {
    for (unit, [first, _]) in walked(config) {
      let kind = (first >> 1) & 0x3F;
      if let Some(array) = HEVC_ARRAYS.iter().position(|&kept| kept == kind)
        && hvcc_add(&mut record, unit, array, true) == Parse::Failed
      {
        return Wrote::Failed;
      }
    }
  }
  let [vps, sps, pps, ..] = record.counts;
  if complete && (vps == 0 || vps > 16 || sps == 0 || sps > 16 || pps == 0 || pps > 64) {
    Wrote::Failed
  } else if record.cut {
    Wrote::Corrupt
  } else {
    Wrote::Whole
  }
}

/// `hvcc_add_nal_unit` (`hevc.c` 862–933) of the unit `nal` into the array
/// `array`, `parsed` where the record is made of Annex B units, whose
/// parameter sets it parses.
fn hvcc_add(record: &mut Hvcc, nal: &[u8], array: usize, parsed: bool) -> Parse {
  if nal_rbsp_len(nal, 2) > GET_BITS_BYTES_MAX {
    return Parse::Failed;
  }
  // The header's two bytes, which no emulation prevention byte precedes.
  let mut header = Reader::new(nal.get(..2).unwrap_or(nal));
  header.skip(1);
  let kind = header.read(6) as u8;
  let layer = header.read(6) as u8;
  if layer > 0 && layer != record.alpha {
    return Parse::Done;
  }
  if record.counts[array] >= u32::from(u16::MAX) {
    return Parse::Failed;
  }
  record.counts[array] += 1;
  record.cut |= nal.len() > usize::from(u16::MAX);
  if !parsed || !(32..=33).contains(&kind) {
    return Parse::Done;
  }
  let rbsp = nal_rbsp(nal, 2);
  let mut bits = Reader::new(&rbsp);
  bits.skip(16);
  if kind == 32 {
    hevc_vps(&mut bits, record);
    Parse::Done
  } else {
    hevc_sps(&mut bits, layer, record)
  }
}

/// `hvcc_parse_ptl` (`hevc.c` 152–203): the bits a profile, tier and level
/// of `sub_layers` sub-layers less one take.
fn hevc_ptl(bits: &mut Reader<'_>, profile_present: bool, sub_layers: u32) {
  // The general profile space, tier, profile, compatibility flags,
  // constraint flags and level: 96 bits; the level alone, 8.
  bits.skip(if profile_present { 96 } else { 8 });
  let flags: Vec<(bool, bool)> = (0..sub_layers)
    .map(|_| (bits.read1() == 1, bits.read1() == 1))
    .collect();
  if sub_layers > 0 {
    for _ in sub_layers..8 {
      bits.skip(2);
    }
  }
  for (profile, level) in flags {
    if profile {
      bits.skip(88);
    }
    if level {
      bits.skip(8);
    }
  }
}

/// `skip_hrd_parameters` (`hevc.c` 224–297): its bits, its refusal of a
/// count of coded picture buffers past 32, which the VPS's parse ignores.
/// A fixed rate in general reads the low-delay flag, not the elemental
/// duration the standard infers (271–279).
fn hevc_hrd(bits: &mut Reader<'_>, common: bool, sub_layers: u32) -> Result<(), ()> {
  let (mut nal, mut vcl, mut sub_picture) = (false, false, false);
  if common {
    nal = bits.read1() == 1;
    vcl = bits.read1() == 1;
    if nal || vcl {
      sub_picture = bits.read1() == 1;
      if sub_picture {
        bits.skip(19);
      }
      bits.skip(8);
      if sub_picture {
        bits.skip(4);
      }
      bits.skip(15);
    }
  }
  for _ in 0..=sub_layers {
    let (mut buffers, mut low_delay, mut within) = (0, false, false);
    if bits.read1() == 0 {
      within = bits.read1() == 1;
    }
    if within {
      bits.ue_long();
    } else {
      low_delay = bits.read1() == 1;
    }
    if !low_delay {
      buffers = bits.ue_long();
      if buffers > 31 {
        return Err(());
      }
    }
    for present in [nal, vcl] {
      if present {
        for _ in 0..=buffers {
          bits.ue_long();
          bits.ue_long();
          if sub_picture {
            bits.ue_long();
            bits.ue_long();
          }
          bits.skip(1);
        }
      }
    }
  }
  Ok(())
}

/// `hvcc_parse_vps` (`hevc.c` 460–540), the reader past the unit's header:
/// its id and sub-layers kept, and its extension's alpha layer.
fn hevc_vps(bits: &mut Reader<'_>, record: &mut Hvcc) {
  let id = bits.read(4) as u8;
  let base_internal = bits.read(1) == 1;
  bits.skip(1);
  let max_layers = bits.read(6);
  let sub_layers = bits.read(3);
  record.vps.push((id, sub_layers as u8));
  bits.skip(17);
  hevc_ptl(bits, true, sub_layers);
  let first = if bits.read(1) == 1 { 0 } else { sub_layers };
  for _ in first..=sub_layers {
    bits.ue();
    bits.ue();
    bits.ue();
  }
  let max_layer_id = bits.read(6) as i32;
  let layer_sets = bits.ue();
  bits.skip_long(i64::from((max_layer_id + 1).wrapping_mul(layer_sets)));
  if bits.read(1) == 1 {
    bits.skip_long(64);
    if bits.read(1) == 1 {
      bits.ue();
    }
    let hrds = bits.ue();
    let mut index = 0;
    // Past the end every further one reads zeros and moves nothing.
    while index < hrds && !bits.exhausted() {
      bits.ue();
      let common = index == 0 || bits.read(1) == 1;
      let _ = hevc_hrd(bits, common, sub_layers);
      index += 1;
    }
  }
  if bits.read(1) == 1 {
    bits.align();
    hevc_vps_extension(bits, record, max_layers, base_internal, sub_layers);
  }
}

/// `hvcc_parse_vps_extension` (`hevc.c` 393–458): the layer an auxiliary
/// alpha dimension names.
fn hevc_vps_extension(
  bits: &mut Reader<'_>,
  record: &mut Hvcc,
  max_layers: u32,
  base_internal: bool,
  sub_layers: u32,
) {
  if max_layers > 0 && base_internal {
    hevc_ptl(bits, false, sub_layers);
  }
  let splitting = bits.read(1) as usize;
  let mut mask = [0_u8; 16];
  let mut types = 0;
  for dimension in 0..16 {
    if bits.read(1) == 1 {
      mask[types] = dimension;
      types += 1;
    }
  }
  let mut lens = [0_u32; 16];
  for len in lens.iter_mut().take(types.saturating_sub(splitting)) {
    *len = bits.read(3) + 1;
  }
  let present = bits.read(1) == 1;
  let layers = max_layers.min(62) as usize;
  let mut ids = [0_u8; 64];
  for (layer, layer_id) in ids.iter_mut().enumerate().take(layers + 1).skip(1) {
    *layer_id = if present {
      bits.read(6) as u8
    } else {
      layer as u8
    };
    if splitting == 0 {
      for dimension in 0..types {
        if bits.read(lens[dimension]) == 1 && mask[dimension] == 3 {
          record.alpha = *layer_id;
        }
      }
    }
  }
  if splitting == 1 {
    let mut offsets = [0_u8; 17];
    for dimension in 1..types {
      offsets[dimension] = offsets[dimension - 1].wrapping_add(lens[dimension - 1] as u8);
    }
    offsets[types] = 6;
    if types > 0 && offsets[types - 1] >= 6 {
      return;
    }
    for &layer_id in &ids[1..=layers] {
      for dimension in 0..types {
        let id = (u32::from(layer_id) & ((1 << offsets[dimension + 1]) - 1)) >> offsets[dimension];
        if id == 1 && mask[dimension] == 3 {
          record.alpha = layer_id;
        }
      }
    }
  }
}

/// `hvcc_parse_sps` (`hevc.c` 628–767) of a unit of `layer`, the reader past
/// its header, to its count of long-term reference pictures — after it
/// nothing fails.
fn hevc_sps(bits: &mut Reader<'_>, layer: u8, record: &Hvcc) -> Parse {
  let vps_id = bits.read(4) as u8;
  let (extension, sub_layers) = if layer == 0 {
    (0, bits.read(3))
  } else {
    let extension = bits.read(3);
    if extension == 7 {
      match record.vps.iter().find(|(id, _)| *id == vps_id) {
        Some(&(_, sub_layers)) => (7, u32::from(sub_layers)),
        None => return Parse::Failed,
      }
    } else {
      (extension, extension)
    }
  };
  let multilayer = layer != 0 && extension == 7;
  if !multilayer {
    bits.skip(1);
    hevc_ptl(bits, true, sub_layers);
  }
  bits.ue_long();
  if multilayer {
    if bits.read1() == 1 {
      bits.skip(8);
    }
  } else {
    // `chromaFormat` is held in 8 bits (`hevc.c` 683–686).
    if bits.ue_long() as u8 == 3 {
      bits.skip(1);
    }
    bits.ue_long();
    bits.ue_long();
    if bits.read1() == 1 {
      for _ in 0..4 {
        bits.ue_long();
      }
    }
    bits.ue_long();
    bits.ue_long();
  }
  bits.ue_long();
  if !multilayer {
    let first = if bits.read1() == 1 { 0 } else { sub_layers };
    for _ in first..=sub_layers {
      bits.ue_long();
      bits.ue_long();
      bits.ue_long();
    }
  }
  for _ in 0..6 {
    bits.ue_long();
  }
  if bits.read1() == 1 {
    let inferred = multilayer && bits.read1() == 1;
    if inferred {
      bits.skip(6);
    } else if bits.read1() == 1 {
      hevc_scaling_lists(bits);
    }
  }
  bits.skip(2);
  if bits.read1() == 1 {
    bits.skip(8);
    bits.ue_long();
    bits.ue_long();
    bits.skip(1);
  }
  let sets = bits.ue_long();
  if sets > 64 {
    return Parse::Trusted;
  }
  let mut deltas = [0_u32; 64];
  for index in 0..sets as usize {
    if hevc_rps(bits, index, &mut deltas).is_err() {
      return Parse::Trusted;
    }
  }
  if bits.read1() == 1 && bits.ue_long() > 31 {
    return Parse::Failed;
  }
  Parse::Done
}

/// `skip_scaling_list_data` (`hevc.c` 542–559).
fn hevc_scaling_lists(bits: &mut Reader<'_>) {
  for size in 0..4_u32 {
    for _ in 0..if size == 3 { 2 } else { 6 } {
      if bits.read1() == 0 {
        bits.ue_long();
      } else {
        if size > 1 {
          bits.se_long();
        }
        for _ in 0..64.min(1 << (4 + (size << 1))) {
          bits.se_long();
        }
      }
    }
  }
}

/// `parse_rps` (`hevc.c` 561–626) of the short-term reference picture set
/// `index`: its refusal of pictures whose bits run past the SPS, the
/// trust model's — a set it predicts from another is never past the count,
/// read in an SPS. Past the end its loops read zeros and move nothing.
fn hevc_rps(bits: &mut Reader<'_>, index: usize, deltas: &mut [u32; 64]) -> Result<(), ()> {
  if index > 0 && bits.read1() == 1 {
    bits.skip(1);
    bits.ue_long();
    deltas[index] = 0;
    let mut picture = 0_u64;
    while picture <= u64::from(deltas[index - 1]) && !bits.exhausted() {
      let used = bits.read1() == 1;
      if used || bits.read1() == 1 {
        deltas[index] = deltas[index].wrapping_add(1);
      }
      picture += 1;
    }
  } else {
    let negative = bits.ue_long();
    let positive = bits.ue_long();
    // An `int` compared with an unsigned 64-bit sum: one below zero never
    // refuses.
    if (u64::from(positive) + u64::from(negative)) * 2 > bits.left() as u64 {
      return Err(());
    }
    deltas[index] = negative.wrapping_add(positive);
    for _ in 0..u64::from(negative) + u64::from(positive) {
      if bits.exhausted() {
        break;
      }
      bits.ue_long();
      bits.skip(1);
    }
  }
  Ok(())
}

/// The VVC NAL unit types `ff_isom_write_vvcc` keeps, in its arrays'
/// order: OPI, VPS, SPS, PPS, prefix SEI, suffix SEI (`vvc.c` 914–916).
const VVC_ARRAYS: [u8; 6] = [12, 14, 15, 16, 23, 24];

/// What `ff_isom_write_vvcc` does with a VVC configuration (`vvc.c`
/// 879–936 in FFmpeg 9.0.1) — its arrays' completeness, MP4's under `vvc1`
/// and FLV's, writes a bit and refuses nothing. Fewer than 6 bytes
/// refused; a `vvcC` — its first five bits set — written as it is; else one
/// opening on no start code refused; its Annex B units of the six kept
/// types each read whole as an RBSP, which `init_get_bits8` must take, a
/// count of 65 536 or a unit of more than 65 535 bytes written with its 16
/// bits wrapped or cut (464–466), its VPSs and SPSs parsed, refusing
/// nothing; more than 16 VPSs, no SPS or more than 16, no PPS or more than
/// 64, refused (683–688).
pub(super) fn vvcc(config: &[u8]) -> Wrote {
  if config.len() < 6 {
    return Wrote::Failed;
  }
  if config[0] & 0xF8 == 0xF8 {
    return Wrote::Whole;
  }
  if !annex_b(config) {
    return Wrote::Failed;
  }
  let mut counts = [0_u16; 6];
  let mut cut = false;
  for (unit, [_, second]) in walked(config) {
    let Some(array) = VVC_ARRAYS.iter().position(|&kept| kept == second >> 3) else {
      continue;
    };
    if nal_rbsp_len(unit, 2) > GET_BITS_BYTES_MAX {
      return Wrote::Failed;
    }
    let (count, wrapped) = counts[array].overflowing_add(1);
    counts[array] = count;
    cut |= wrapped || unit.len() > usize::from(u16::MAX);
  }
  let [_, vps, sps, pps, ..] = counts;
  if vps > 16 || sps == 0 || sps > 16 || pps == 0 || pps > 64 {
    Wrote::Failed
  } else if cut {
    Wrote::Corrupt
  } else {
    Wrote::Whole
  }
}

/// The EVC NAL unit types `ff_isom_write_evcc` keeps, in its arrays'
/// order: SPS, PPS, APS, SEI (`evc.c` 339–354; `libavcodec/evc.h` 58–62).
const EVC_ARRAYS: [i32; 4] = [24, 25, 26, 28];

/// What `ff_isom_write_evcc` does with an EVC configuration (`evc.c`
/// 298–385 in FFmpeg 9.0.1) — its completeness writes a bit. Fewer than 8
/// bytes refused; an `evcC` — of version 1 — written as it is; else units
/// behind 4-byte lengths, walked while more than four bytes are left and
/// stopped at a length of zero or one past what is left: a unit whose
/// header has the forbidden bit or no type refused (`evc_get_nalu_type`,
/// `evc.h` 32–46, read of what is left, not of the unit); its SPSs, PPSs,
/// APSs and SEIs kept, a count of 65 536 or a unit of more than 65 535
/// bytes written with its 16 bits wrapped or cut; an SPS read past its
/// header as it is, unescaped by nothing (`evcc_parse_sps`, 80–125): one
/// too short to read refused, a bit depth past 14 refused — the record
/// holds three bits of it; no SPS, or more than 16, refused (226–227).
pub(super) fn evcc(config: &[u8]) -> Wrote {
  if config.len() < 8 {
    return Wrote::Failed;
  }
  if config[0] == 1 {
    return Wrote::Whole;
  }
  let mut counts = [0_u16; 4];
  let mut cut = false;
  let (mut at, mut left) = (0, config.len());
  while left > 4 {
    let size =
      u32::from_be_bytes([config[at], config[at + 1], config[at + 2], config[at + 3]]) as usize;
    if size == 0 {
      break;
    }
    at += 4;
    left -= 4;
    if left < size {
      break;
    }
    let first = config[at];
    let kind = if left >= 2 && first & 0x80 == 0 {
      i32::from((first >> 1) & 0x3F) - 1
    } else {
      -1
    };
    if !(0..=62).contains(&kind) {
      return Wrote::Failed;
    }
    if let Some(array) = EVC_ARRAYS.iter().position(|&kept| kept == kind) {
      let (count, wrapped) = counts[array].overflowing_add(1);
      counts[array] = count;
      cut |= wrapped || size > usize::from(u16::MAX);
      if kind == 24 && evc_sps(&config[at..], size) == Parse::Failed {
        return Wrote::Failed;
      }
    }
    at += size;
    left -= size;
  }
  if counts[0] == 0 || counts[0] > 16 {
    Wrote::Failed
  } else if cut {
    Wrote::Corrupt
  } else {
    Wrote::Whole
  }
}

/// `evcc_parse_sps` (`evc.c` 80–125) of the SPS of `size` bytes opening
/// `rest`, the configuration from it on: its reader past the unit's 2-byte
/// header over the `int` its size less it comes to — reading on into the
/// next bytes — refused where `init_get_bits8` refuses it; an id past 15
/// or a chroma format past 3, the trust model's; a bit depth past 14,
/// refused. Its fields are held in 8 bits.
fn evc_sps(rest: &[u8], size: usize) -> Parse {
  let bytes = (size as u32 as i32).wrapping_sub(2);
  if bytes < 0 || bytes as usize > GET_BITS_BYTES_MAX {
    return Parse::Failed;
  }
  let mut bits = Reader {
    bytes: rest.get(2..).unwrap_or(&[]),
    bits: bytes as usize * 8,
    index: 0,
  };
  if bits.ue_long() >= 16 {
    return Parse::Trusted;
  }
  // The profile, the level, the two toolset words.
  bits.skip(80);
  if bits.ue_long() as u8 > 3 {
    return Parse::Trusted;
  }
  bits.ue_long();
  bits.ue_long();
  let luma = bits.ue_long() as u8;
  let chroma = bits.ue_long() as u8;
  if luma > 6 || chroma > 6 {
    Parse::Failed
  } else {
    Parse::Done
  }
}

/// The codecs whose Annex B units `ff_h2645_packet_split` splits, as their
/// headers read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Units {
  H264,
  Hevc,
  Vvc,
  Lcevc,
}

/// A unit `ff_h2645_packet_split` splits off (`h2645_parse.c` 527–665 in
/// FFmpeg 9.0.1).
struct Split<'a> {
  /// `raw_data`, `raw_size`: its bytes, its emulation prevention in.
  raw: &'a [u8],
  /// `data`, `size`: its RBSP.
  rbsp: Cow<'a, [u8]>,
  /// `skipped_bytes_pos`: where in the RBSP each emulation prevention
  /// byte was dropped, at the second zero before it.
  skipped: Vec<usize>,
  /// Its header's unit type.
  kind: u8,
}

/// `ff_h2645_packet_split` over the Annex B units of `bytes`, of
/// `small_padding` or not (`h2645_parse.c` 527–665 in FFmpeg 9.0.1): each
/// unit from past a start code to the next — or to the next `00 00 02` once
/// an emulation prevention byte is met — its RBSP's trailing zeros and stop
/// bit counted off; one of no bits, or whose header its codec refuses,
/// dropped. `None` where it fails: a search reaching the end with no unit
/// kept — of no start code at all — or a unit of more bits than
/// `init_get_bits` takes.
fn h2645_split(bytes: &[u8], units: Units, small_padding: bool) -> Option<Vec<Split<'_>>> {
  let length = bytes.len();
  let mut at = 0;
  let mut split = Vec::new();
  while length - at >= 4 {
    // `find_next_start_code`: past the first start code with a byte after
    // it, or to the end (`h2645_parse.c` 465–478).
    let rest = length - at;
    at += (0..rest - 3)
      .find(|&i| bytes[at + i] == 0 && bytes[at + i + 1] == 0 && bytes[at + i + 2] == 1)
      .map_or(rest, |i| i + 3);
    if at == length {
      return (!split.is_empty()).then_some(split);
    }
    let (rbsp, raw, skipped) = extract_rbsp(&bytes[at..], small_padding);
    let start = at;
    at += raw;
    let skip_trailing_zeros = bytes.get(at..at + 4) != Some(&[0, 0, 1, 0xE0][..]);
    let bits = bit_length(
      &rbsp,
      if units == Units::Hevc { 2 } else { 1 },
      skip_trailing_zeros,
    );
    if rbsp.is_empty() || bits <= 0 {
      continue;
    }
    if bits >= i64::from(i32::MAX - 512) {
      return None;
    }
    let mut header = Reader {
      bytes: &rbsp,
      bits: bits as usize,
      index: 0,
    };
    let kind = match units {
      Units::H264 => {
        if header.read1() != 0 {
          continue;
        }
        header.skip(2);
        header.read(5)
      }
      Units::Hevc => {
        if header.read1() != 0 {
          continue;
        }
        let kind = header.read(6);
        let layer = header.read(6);
        if layer == 63 || header.read(3) == 0 {
          continue;
        }
        kind
      }
      Units::Vvc => {
        if header.read1() != 0 {
          continue;
        }
        header.skip(7);
        let kind = header.read(5);
        let temporal = header.read(3);
        if temporal == 0 || ((7..=11).contains(&kind) && temporal != 1) {
          continue;
        }
        kind
      }
      Units::Lcevc => {
        if header.read1() != 0 || header.read1() != 1 {
          continue;
        }
        header.read(5)
      }
    };
    split.push(Split {
      raw: &bytes[start..at],
      rbsp,
      skipped,
      kind: kind as u8,
    });
  }
  Some(split)
}

/// `ff_h2645_extract_rbsp` (`h2645_parse.c` 37–149) of the unit opening
/// `src`, which runs to the end at most: its RBSP, its raw size and where
/// its emulation prevention bytes were dropped. It ends at the first start
/// code before an emulation prevention byte, at the first `00 00 01` or
/// `00 00 02` after one; of small padding and none met, it is its own RBSP.
fn extract_rbsp(src: &[u8], small_padding: bool) -> (Cow<'_, [u8]>, usize, Vec<usize>) {
  let mut length = src.len();
  let first = (0..length.saturating_sub(2))
    .find(|&i| src[i] == 0 && src[i + 1] == 0 && (src[i + 2] == 1 || src[i + 2] == 3));
  let found = match first {
    Some(i) => {
      if src[i + 2] == 1 {
        length = i;
      }
      i
    }
    None => length,
  };
  if found + 1 >= length && small_padding {
    return (Cow::Borrowed(&src[..length]), length, Vec::new());
  }
  let found = found.min(length);
  let mut dst = src[..found].to_vec();
  let mut skipped = Vec::new();
  let mut si = found;
  while si + 2 < length {
    if src[si + 2] > 3 {
      dst.extend_from_slice(&src[si..si + 2]);
      si += 2;
    } else if src[si] == 0 && src[si + 1] == 0 && src[si + 2] != 0 {
      if src[si + 2] == 3 {
        dst.extend_from_slice(&[0, 0]);
        si += 3;
        skipped.push(dst.len() - 1);
        continue;
      }
      return (Cow::Owned(dst), si, skipped);
    }
    dst.push(src[si]);
    si += 1;
  }
  dst.extend_from_slice(&src[si..length]);
  (Cow::Owned(dst), length, skipped)
}

/// `get_bit_length` (`h2645_parse.c` 348–376): the bits of `rbsp` but its
/// trailing zeros — where they are skipped — and its stop bit; one of no
/// byte answering zero, one shorter than `min_size` or past `INT_MAX / 8`
/// bytes below zero.
fn bit_length(rbsp: &[u8], min_size: usize, skip_trailing_zeros: bool) -> i64 {
  let mut size = rbsp.len();
  while skip_trailing_zeros && size > 0 && rbsp[size - 1] == 0 {
    size -= 1;
  }
  if size == 0 {
    return 0;
  }
  let mut padding = 0;
  if size <= min_size {
    if rbsp.len() < min_size {
      return -1;
    }
    size = min_size;
  } else if rbsp[size - 1] != 0 {
    padding = i64::from(rbsp[size - 1].trailing_zeros()) + 1;
  }
  if size > i32::MAX as usize / 8 {
    return -1;
  }
  size as i64 * 8 - padding
}

/// The configuration FLV's `extract_extradata` makes of the H.264, HEVC or
/// VVC packet `bytes`, which replaces the packet's own new extradata —
/// `Ok(None)` where it makes none — or `Err` where the filter refuses the
/// packet, which libavformat then drops (`extract_extradata_h2645`,
/// `extract_extradata.c` 166–275 in FFmpeg 9.0.1, over
/// `ff_h2645_packet_split` of small padding; `mux.c` 1132–1140): the
/// packet's parameter sets in their order — H.264's SPSs and PPSs, HEVC's
/// and VVC's VPSs, SPSs and PPSs — each its raw bytes behind a 4-byte
/// start code, where an SPS is among them, and for HEVC a VPS.
pub(super) fn h2645_extracted(units: Units, bytes: &[u8]) -> Result<Option<Vec<u8>>, ()> {
  let split = h2645_split(bytes, units, true).ok_or(())?;
  let (kept, sps, vps): (&[u8], u8, Option<u8>) = match units {
    Units::Vvc => (&[14, 15, 16], 15, None),
    Units::Hevc => (&[32, 33, 34], 33, Some(32)),
    Units::H264 => (&[7, 8], 7, None),
    // LCEVC's configuration is another of the filter's roads, which FLV,
    // writing no LCEVC, never reaches.
    Units::Lcevc => return Ok(None),
  };
  let sets = || split.iter().filter(|unit| kept.contains(&unit.kind));
  let present = |kind: u8| sets().any(|unit| unit.kind == kind);
  if sets().next().is_none() || !present(sps) || vps.is_some_and(|vps| !present(vps)) {
    return Ok(None);
  }
  let mut config = Vec::new();
  for unit in sets() {
    config.extend_from_slice(&[0, 0, 0, 1]);
    config.extend_from_slice(unit.raw);
  }
  Ok(Some(config))
}

/// The pictures `ff_lcevc_resolution_type` names, by resolution type
/// (`lcevctab.c` 22–36): types 1 to 45; 0 and 46 to 62 name none.
const LCEVC_RESOLUTIONS: core::ops::RangeInclusive<u32> = 1..=45;

/// `get_mb` (`lcevc_parse.h` 26–40): a multi-byte count of seven bits a
/// byte, ten bytes at most.
fn lcevc_count(bits: &mut Reader<'_>) -> u64 {
  let mut count = 0_u64;
  for byte in 1..=10 {
    let value = bits.read(8);
    count = (count << 7) | u64::from(value & 0x7F);
    if value & 0x80 == 0 || byte == 10 {
      break;
    }
  }
  count
}

/// How `write_nalu` (`lcevc.c` 39–178) ends over an IDR or non-IDR unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LcevcUnit {
  /// Written, the 16-bit length it writes of it: its header, its first
  /// sequence configuration block, its first global configuration block
  /// of a picture, and each additional information block until both are
  /// met, at its raw size.
  Length(usize),
  /// Refused: a unit of no sequence or no global configuration block.
  Failed,
  /// Refused where only a unit the standard does not allow reaches — a
  /// block of a reserved size type, of a size past `INT_MAX`, or running to
  /// the unit's end or past it, a global configuration of a picture of a
  /// reserved resolution type or of no dimensions: the trust model's.
  Trusted,
}

/// `write_nalu` (`lcevc.c` 39–178) over the IDR or non-IDR unit `unit`.
fn lcevc_unit(unit: &Split<'_>) -> LcevcUnit {
  let data = &unit.rbsp[..];
  let mut at = data.len().min(2);
  let mut length = 3;
  let (mut sequence, mut global, mut unnamed) = (false, false, false);
  let mut skipped = 0;
  while data.len() - at > 1 && !(sequence && global) {
    let left = data.len() - at;
    let mut bits = Reader::new(&data[at..]);
    let size_type = bits.read(3);
    let kind = bits.read(5);
    if size_type == 6 {
      return LcevcUnit::Trusted;
    }
    let size = if size_type == 7 {
      lcevc_count(&mut bits)
    } else {
      u64::from(size_type)
    };
    let header = (bits.count() >> 3) as u64;
    if size > i32::MAX as u64 - header {
      return LcevcUnit::Trusted;
    }
    let block = (size + header) as usize;
    if block >= left {
      return LcevcUnit::Trusted;
    }
    let end = at + block;
    let mut raw_block = block;
    while skipped < unit.skipped.len() && unit.skipped[skipped] < end {
      raw_block += 1;
      skipped += 1;
    }
    match kind {
      0 if !sequence => {
        length += raw_block;
        sequence = true;
      }
      1 if !global => {
        if lcevc_picture(&mut bits) {
          length += raw_block;
          global = true;
        } else {
          unnamed = true;
        }
      }
      5 => length += raw_block,
      _ => {}
    }
    at = end;
  }
  if sequence && global {
    LcevcUnit::Length(length)
  } else if sequence && unnamed {
    LcevcUnit::Trusted
  } else {
    LcevcUnit::Failed
  }
}

/// Whether a global configuration block names a picture (`lcevc.c` 104–148):
/// its resolution type one its table holds, or its own dimensions, neither
/// zero.
fn lcevc_picture(bits: &mut Reader<'_>) -> bool {
  let planes = bits.read1() == 1;
  let resolution = bits.read(6);
  // A reserved bit, the chroma sampling, two reserved bits, the depth.
  bits.skip(7);
  if resolution < 63 {
    return LCEVC_RESOLUTIONS.contains(&resolution);
  }
  let temporal = bits.read1() == 1;
  bits.skip(3);
  let upsample = bits.read(3);
  let level1 = bits.read1() == 1;
  bits.skip(4);
  let tiles = bits.read(2);
  bits.skip(4);
  if planes {
    bits.skip(4);
  }
  if temporal {
    bits.skip(8);
  }
  if upsample != 0 {
    bits.skip_long(64);
  }
  if level1 {
    bits.skip(8);
  }
  if tiles != 0 {
    if tiles == 3 {
      bits.skip_long(32);
    }
    bits.skip(8);
  }
  bits.read(16) != 0 && bits.read(16) != 0
}

/// What `ff_isom_write_lvcc` does with an LCEVC configuration (`lcevc.c`
/// 251–339 in FFmpeg 9.0.1): at most 6 bytes refused; one opening on no
/// start code — an `lvcC` — written as it is; else its units split as
/// libavcodec splits them, of no small padding, refused where that fails;
/// every IDR and non-IDR unit written through `write_nalu`
/// ([`lcevc_unit`]), refused where it refuses one of no sequence or global
/// configuration, its 16-bit length cut past 65 535; none at all refused;
/// the counts of each, 16 bits, wrapped past 65 535.
pub(super) fn lvcc(config: &[u8]) -> Wrote {
  if config.len() <= 6 {
    return Wrote::Failed;
  }
  if !annex_b(config) {
    return Wrote::Whole;
  }
  let Some(split) = h2645_split(config, Units::Lcevc, false) else {
    return Wrote::Failed;
  };
  let (mut idr, mut other, mut cut) = (0_usize, 0_usize, false);
  for unit in &split {
    let count = match unit.kind {
      29 => &mut idr,
      28 => &mut other,
      _ => continue,
    };
    *count += 1;
    match lcevc_unit(unit) {
      LcevcUnit::Length(length) => cut |= length > usize::from(u16::MAX),
      LcevcUnit::Failed => return Wrote::Failed,
      LcevcUnit::Trusted => {}
    }
  }
  if idr == 0 && other == 0 {
    Wrote::Failed
  } else if cut || idr > usize::from(u16::MAX) || other > usize::from(u16::MAX) {
    Wrote::Corrupt
  } else {
    Wrote::Whole
  }
}

/// What `ff_isom_write_vpcc` does with a VP9 track whose stream's pixel
/// format is `format` and profile `profile`, its configuration — MP4's
/// first sample where it states none (`movenc.c` 7024–7041) — of `len`
/// bytes (`vpcc.c` 38–69, 157–227 in FFmpeg 9.0.1): a format of no
/// descriptor, or of chroma subsampled otherwise than 4:2:0, 4:2:2 or
/// 4:4:4 — VP9's 4:4:0 among them — refused; where the profile is unknown
/// or the format's depth is none, the configuration read for them, refused
/// past what `init_get_bits8` takes.
pub(super) fn vpcc(len: usize, format: i32, profile: i32) -> Wrote {
  let Some(sampling) = pix_fmt_sampling(format) else {
    return Wrote::Failed;
  };
  if !matches!(
    (sampling.log2_chroma_w(), sampling.log2_chroma_h()),
    (1, 1) | (1, 0) | (0, 0)
  ) {
    return Wrote::Failed;
  }
  if len > GET_BITS_BYTES_MAX && (profile == PROFILE_UNKNOWN || sampling.depth() == 0) {
    return Wrote::Failed;
  }
  Wrote::Whole
}

/// What `mov_write_av3c` does with an AVS3 configuration (`movenc.c`
/// 1599–1616 in FFmpeg 9.0.1), MP4 ignoring its answer (1618–1626): fewer
/// than 4 bytes refused, the box left empty; an
/// `Avs3DecoderConfigurationRecord`, of version 1, written as it is; else
/// the sequence header behind its 16-bit length, cut past 65 535.
pub(super) fn av3c(config: &[u8]) -> Wrote {
  if config.len() < 4 {
    Wrote::Failed
  } else if config[0] != 1 && config.len() > usize::from(u16::MAX) {
    Wrote::Corrupt
  } else {
    Wrote::Whole
  }
}

/// `VC1_CODE_SEQHDR` (`vc1_common.h` 40).
const VC1_SEQUENCE_HEADER: u32 = 0x0000_010F;

/// What `mov_write_dvc1_tag` does with a VC-1 configuration (`movenc.c`
/// 1102–1201 in FFmpeg 9.0.1), MP4 writing no `dvc1` where it fails, its
/// answer ignored (2915–2916): among the configuration's markers — each
/// start code with a byte after it, searched from four bytes past the last
/// (`find_next_marker`, `vc1_common.h` 59–68) — a sequence header with a
/// byte of its own, or refused: VC-1's simple and main profiles, which
/// carry none, among them. A sequence header of another profile than the
/// advanced one, which alone writes one, it refuses too: the trust model's.
pub(super) fn dvc1(config: &[u8]) -> Wrote {
  let len = config.len();
  let marker = |from: usize| -> usize {
    if len < from.saturating_add(4) {
      return len;
    }
    (from..=len - 4)
      .find(|&at| config[at] == 0 && config[at + 1] == 0 && config[at + 2] == 1)
      .unwrap_or(len)
  };
  let mut found = false;
  let mut start = marker(0);
  while start < len {
    let next = marker(start + 4);
    if next > start + 4
      && u32::from_be_bytes([
        config[start],
        config[start + 1],
        config[start + 2],
        config[start + 3],
      ]) == VC1_SEQUENCE_HEADER
    {
      if config[start + 4] >> 6 != 3 {
        return Wrote::Whole;
      }
      found = true;
    }
    start = next;
  }
  if found { Wrote::Whole } else { Wrote::Failed }
}

/// A configuration writer the MOV family or FLV calls and ignores the
/// answer of, with what it reads beside the configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Writer {
  /// `ff_isom_write_avcc`.
  Avcc,
  /// `ff_isom_write_hvcc`, its arrays complete or not.
  Hvcc { complete: bool },
  /// `ff_isom_write_vvcc`.
  Vvcc,
  /// `ff_isom_write_evcc`.
  Evcc,
  /// `ff_isom_write_lvcc`.
  Lvcc,
  /// `ff_isom_write_vpcc`, of the stream's pixel format and profile.
  Vpcc { format: i32, profile: i32 },
  /// `ff_isom_write_av1c`.
  Av1c,
  /// `mov_write_av3c`.
  Av3c,
  /// `mov_write_dvc1_tag`.
  Dvc1,
}

impl Writer {
  /// What the writer does with `config`.
  pub(super) fn writes(self, config: &[u8]) -> Wrote {
    match self {
      Self::Avcc => avcc(config),
      Self::Hvcc { complete } => hvcc(config, complete),
      Self::Vvcc => vvcc(config),
      Self::Evcc => evcc(config),
      Self::Lvcc => lvcc(config),
      Self::Vpcc { format, profile } => vpcc(config.len(), format, profile),
      Self::Av1c => {
        if super::av1c_written(config) {
          Wrote::Whole
        } else {
          Wrote::Failed
        }
      }
      Self::Av3c => av3c(config),
      Self::Dvc1 => dvc1(config),
    }
  }
}
