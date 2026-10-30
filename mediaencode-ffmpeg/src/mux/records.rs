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
//! than the advanced one (`movenc.c` 1137–1141); APV's access unit of no
//! signature, of a PBU that does not split, or of a frame header or tiles
//! that do not parse, and its frame of an odd bit depth (`apv.c` 257–281;
//! `cbs_apv.c` 162–244).
//!
//! APV's `apvC` is MP4's parse of the packets, not of a configuration
//! (`ff_isom_parse_apvc`, `apv.c` 253–318): each frame's information,
//! recorded by its PBU type, its 256th distinct one past the record's
//! 8-bit count ([`apv_frame_infos`]).
//!
//! **Nothing allocated** (R15, Codex R14 3): these mirrors run in the
//! muxer's judgment of a packet, before any of its refusals, so each reads
//! its configuration in place — a unit's RBSP through [`Bytes`], its units
//! through iterators, the configuration FLV's `extract_extradata` makes of
//! a packet through [`Extracted`] — and holds no buffer of its own. The
//! cost, accepted (R15b): a byte of an RBSP view of a unit that holds an
//! emulation prevention byte is found by a walk from the unit's start, so
//! that a reader of such a unit runs in time quadratic in its length.
//! Only the configuration writers read a unit through a reader, the
//! parameter sets alone, which the standards keep small; FLV's split
//! reads every unit of a packet, slices among them, once, linearly — its
//! RBSP's length, its trailing bits and its header's first bytes.

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

/// The bytes a reader reads, in place: a buffer as it is, or the RBSP of a
/// unit — each emulation prevention byte, a `3` after two zeros at its
/// `header`th byte or later, dropped, as `ff_nal_unit_extract_rbsp` and
/// `ff_h2645_extract_rbsp` drop it (`nal.c` 163–193; `h2645_parse.c`
/// 37–149) — from its `from`th byte to its `len`th (R15).
#[derive(Clone, Copy)]
enum Bytes<'a> {
  Plain(&'a [u8]),
  Rbsp {
    raw: &'a [u8],
    header: usize,
    from: usize,
    len: usize,
  },
}

impl<'a> Bytes<'a> {
  /// The RBSP of `raw`, its first `header` bytes as they are: `raw` itself
  /// where it holds no emulation prevention byte.
  fn rbsp(raw: &'a [u8], header: usize) -> Self {
    let len = nal_rbsp_len(raw, header);
    if len == raw.len() {
      Self::Plain(raw)
    } else {
      Self::Rbsp {
        raw,
        header,
        from: 0,
        len,
      }
    }
  }

  fn len(self) -> usize {
    match self {
      Self::Plain(bytes) => bytes.len(),
      Self::Rbsp { from, len, .. } => len - from,
    }
  }

  fn is_empty(self) -> bool {
    self.len() == 0
  }

  /// The byte at `at`, or `None` past the end.
  fn get(self, at: usize) -> Option<u8> {
    match self {
      Self::Plain(bytes) => bytes.get(at).copied(),
      Self::Rbsp {
        raw,
        header,
        from,
        len,
      } => {
        let at = from.checked_add(at).filter(|&at| at < len)?;
        RbspBytes::new(raw, header).nth(at)
      }
    }
  }

  /// The bytes from the `at`th on.
  fn from(self, at: usize) -> Self {
    match self {
      Self::Plain(bytes) => Self::Plain(bytes.get(at..).unwrap_or(&[])),
      Self::Rbsp {
        raw,
        header,
        from,
        len,
      } => Self::Rbsp {
        raw,
        header,
        from: from.saturating_add(at).min(len),
        len,
      },
    }
  }

  /// Each byte, in order.
  fn iter(self) -> impl Iterator<Item = u8> + 'a {
    let (plain, rbsp) = match self {
      Self::Plain(bytes) => (Some(bytes.iter().copied()), None),
      Self::Rbsp {
        raw,
        header,
        from,
        len,
      } => (
        None,
        Some(RbspBytes::new(raw, header).skip(from).take(len - from)),
      ),
    };
    plain
      .into_iter()
      .flatten()
      .chain(rbsp.into_iter().flatten())
  }
}

/// The RBSP of `raw`, byte by byte, its first `header` bytes as they are
/// (`nal.c` 163–193): each `00 00 03` read as `00 00`, past it the bytes
/// after the `03`; its last two bytes as they are.
#[derive(Clone)]
struct RbspBytes<'a> {
  raw: &'a [u8],
  header: usize,
  at: usize,
  zero: bool,
}

impl<'a> RbspBytes<'a> {
  fn new(raw: &'a [u8], header: usize) -> Self {
    Self {
      raw,
      header,
      at: 0,
      zero: false,
    }
  }
}

impl Iterator for RbspBytes<'_> {
  type Item = u8;

  fn next(&mut self) -> Option<u8> {
    if self.zero {
      self.zero = false;
      return Some(0);
    }
    let (raw, at) = (self.raw, self.at);
    let byte = *raw.get(at)?;
    if at >= self.header && at + 2 < raw.len() && byte == 0 && raw[at + 1] == 0 && raw[at + 2] == 3
    {
      self.at += 3;
      self.zero = true;
      return Some(0);
    }
    self.at += 1;
    Some(byte)
  }
}

/// Where in the RBSP of `raw` — of no header — each emulation prevention
/// byte was dropped, at the second zero before it, in order:
/// `ff_h2645_extract_rbsp`'s `skipped_bytes_pos` (`h2645_parse.c` 37–149).
fn skipped(raw: &[u8]) -> impl Iterator<Item = usize> + '_ {
  let (mut at, mut dropped) = (0, 0);
  core::iter::from_fn(move || {
    while at + 2 < raw.len() {
      if raw[at] == 0 && raw[at + 1] == 0 && raw[at + 2] == 3 {
        let position = at + 1 - dropped;
        dropped += 1;
        at += 3;
        return Some(position);
      }
      at += 1;
    }
    None
  })
}

/// Bits read as libavcodec's checked reader reads them (`get_bits.h`
/// 107–700 in FFmpeg 9.0.1, its cached reader off): `bits` bits over
/// `bytes`, which go on past them — a configuration's next bytes, or the
/// zeroed padding past its end — and the place held at 8 bits past them.
#[derive(Clone, Copy)]
struct Reader<'a> {
  bytes: Bytes<'a>,
  bits: usize,
  index: usize,
}

impl<'a> Reader<'a> {
  /// The reader `init_get_bits8` makes of `bytes`, whole.
  fn new(bytes: &'a [u8]) -> Self {
    Self::of(Bytes::Plain(bytes))
  }

  /// The reader `init_get_bits8` makes of `bytes`, whole.
  fn of(bytes: Bytes<'a>) -> Self {
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
    self.bytes.get(at).unwrap_or(0)
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
pub(super) fn nal_units(bytes: &[u8]) -> NalUnits<'_> {
  NalUnits {
    bytes,
    start: start_code(bytes, 0),
  }
}

/// `nal_find_startcode_internal`'s search from `from` (`nal.c` 31–66): no
/// further than four bytes before the end — a start code in the last three
/// is no start code — its zero before it taken with it.
fn start_code(bytes: &[u8], from: usize) -> usize {
  let found = (from..bytes.len().saturating_sub(3))
    .find(|&at| bytes[at] == 0 && bytes[at + 1] == 0 && bytes[at + 2] == 1)
    .unwrap_or(bytes.len());
  if from < found && found < bytes.len() && bytes[found - 1] == 0 {
    found - 1
  } else {
    found
  }
}

/// The units [`nal_units`] finds, in order, read in place.
#[derive(Clone)]
pub(super) struct NalUnits<'a> {
  bytes: &'a [u8],
  start: usize,
}

impl<'a> Iterator for NalUnits<'a> {
  type Item = &'a [u8];

  fn next(&mut self) -> Option<&'a [u8]> {
    let bytes = self.bytes;
    // Past the zeros and the 1 of the start code.
    while self.start < bytes.len() {
      let byte = bytes[self.start];
      self.start += 1;
      if byte != 0 {
        break;
      }
    }
    if self.start >= bytes.len() {
      return None;
    }
    let end = start_code(bytes, self.start);
    let mut size = end - self.start;
    while size > 0 && bytes[self.start + size - 1] == 0 {
      size -= 1;
    }
    let unit = &bytes[self.start..self.start + size];
    self.start = end;
    Some(unit)
  }
}

/// Each unit the configuration writers' walk reads of the buffer
/// `ff_nal_parse_units_buf` makes of `bytes` — every unit behind its
/// 4-byte length, walked while more than four bytes are left (`avc.c`
/// 67–101; `hevc.c` 1380–1390; `vvc.c` 907–925) — with the two bytes after
/// its length, which they read its type from: past a unit shorter than
/// two, the next unit's length, and past the buffer its padding's zeros.
/// Each length is the unit's own, so a step is a unit; a last unit of none
/// leaves four bytes, and is not walked.
fn walked<'a>(units: impl Iterator<Item = &'a [u8]>) -> impl Iterator<Item = (&'a [u8], [u8; 2])> {
  let mut units = units.peekable();
  core::iter::from_fn(move || {
    let unit = units.next()?;
    let next = units.peek().map(|next| (next.len() as u32).to_be_bytes());
    if unit.is_empty() && next.is_none() {
      return None;
    }
    // The bytes after this unit's length: its own, then the next length's.
    let after = |at: usize| -> u8 {
      match unit.get(at) {
        Some(&byte) => byte,
        None => next.map_or(0, |length| length[at - unit.len()]),
      }
    };
    Some((unit, [after(0), after(1)]))
  })
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

/// A configuration the writers read: its bytes, or the one FLV's
/// `extract_extradata` makes of a packet, read in place ([`Extracted`]).
#[derive(Clone, Copy)]
pub(super) enum Config<'a> {
  Bytes(&'a [u8]),
  Extracted(Extracted<'a>),
}

impl<'a> Config<'a> {
  fn len(self) -> usize {
    match self {
      Self::Bytes(bytes) => bytes.len(),
      Self::Extracted(extracted) => extracted.len,
    }
  }

  /// Its first byte: an extracted one's start code's.
  fn first(self) -> Option<u8> {
    match self {
      Self::Bytes(bytes) => bytes.first().copied(),
      Self::Extracted(_) => Some(0),
    }
  }

  fn annex_b(self) -> bool {
    match self {
      Self::Bytes(bytes) => annex_b(bytes),
      Self::Extracted(_) => true,
    }
  }

  /// Its units as libavformat's Annex B splitter finds them ([`nal_units`]).
  fn units(self) -> impl Iterator<Item = &'a [u8]> {
    let (bytes, extracted) = match self {
      Self::Bytes(bytes) => (Some(nal_units(bytes)), None),
      Self::Extracted(extracted) => (None, Some(extracted.units())),
    };
    bytes
      .into_iter()
      .flatten()
      .chain(extracted.into_iter().flatten())
  }
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
  avcc_config(Config::Bytes(config))
}

/// [`avcc`] of a configuration read in place.
fn avcc_config(config: Config<'_>) -> Wrote {
  if config.len() <= 6 {
    return Wrote::Failed;
  }
  if !config.annex_b() {
    return Wrote::Whole;
  }
  let (mut sps, mut pps, mut extensions) = (0_u32, 0_u32, 0_u32);
  let (mut sps_bytes, mut pps_bytes) = (0_usize, 0_usize);
  for (unit, [first, _]) in walked(config.units()) {
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

/// The bytes `ff_isom_write_avcc` writes of `config`, a configuration it
/// writes whole ([`avcc`]; `avc.c` 32–143): an `avcC` as it is; of Annex
/// B units, the record's six bytes, each SPS behind its 16-bit length, a
/// byte of their count and each PPS so, and — of a profile, the first
/// SPS's second byte, other than 66, 77 and 88 — four bytes and each SPS
/// extension so (111–134).
pub(super) fn avcc_written_len(config: &[u8]) -> u64 {
  if config.len() <= 6 {
    return 0;
  }
  if !annex_b(config) {
    return config.len() as u64;
  }
  let (mut sps, mut pps, mut extensions) = (0_u64, 0_u64, 0_u64);
  // The bytes of the SPSs' buffer up to its fourth, the profile it reads:
  // each SPS's 16-bit length, then the SPS (80–81, 112).
  let mut head = [0_u8; 4];
  let mut filled = 0;
  for (unit, [first, _]) in walked(nal_units(config)) {
    let bytes = 2 + unit.len() as u64;
    match first & 0x1F {
      7 => {
        let length = (unit.len() as u16).to_be_bytes();
        for &byte in length.iter().chain(unit) {
          if filled == head.len() {
            break;
          }
          head[filled] = byte;
          filled += 1;
        }
        sps += bytes;
      }
      8 => pps += bytes,
      13 => extensions += bytes,
      _ => {}
    }
  }
  let profile = head[3];
  6 + sps
    + 1
    + pps
    + if [66, 77, 88].contains(&profile) {
      0
    } else {
      4 + extensions
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
  /// Each VPS parsed, by its 4-bit id, the first of each: its sub-layers
  /// less one, which a multilayer SPS of it reads (642–652).
  vps: [Option<u8>; 16],
  /// The auxiliary alpha layer a VPS extension names, whose parameter sets
  /// the record keeps beside the base layer's (431, 452, 888).
  alpha: u8,
  /// A unit kept past 65 535 bytes, its 16-bit length cut (856, 1194).
  cut: bool,
  /// `lhvC`'s: the units of a layer past the base's, by array, which
  /// `ff_isom_write_lhvc` alone writes and counts (1004–1013, 1186–1189).
  upper: [u32; 5],
  /// `lhvC`'s: such a unit past 65 535 bytes, its length cut.
  upper_cut: bool,
  /// `lhvC`'s: the bytes of its arrays, each array's three and each unit's
  /// length and bytes, as written (1170–1192).
  upper_bytes: u64,
  /// A unit of a kept type `hvcC` skips for its layer — neither the base
  /// layer's nor the alpha layer's a VPS extension names before it — lost
  /// to the record (888–889; R11, ruling 1).
  dropped: bool,
  /// The bytes of the units the record keeps, each written behind its
  /// 16-bit length (`hvcc_write`, 1187–1198).
  unit_bytes: u64,
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
/// (1087–1093). A unit of a layer the record skips is lost to it
/// ([`hvcc_drops_layers`]); MP4 writes those of a stream flagged
/// multilayer into `lhvC` beside it ([`lhvc`]).
pub(super) fn hvcc(config: &[u8], complete: bool) -> Wrote {
  hvcc_config(Config::Bytes(config), complete)
}

/// [`hvcc`] of a configuration read in place.
fn hvcc_config(config: Config<'_>, complete: bool) -> Wrote {
  let record = match hvcc_walk(config, false) {
    Ok(record) => record,
    Err(wrote) => return wrote,
  };
  let [vps, sps, pps, ..] = record.counts;
  if complete && (vps == 0 || vps > 16 || sps == 0 || sps > 16 || pps == 0 || pps > 64) {
    Wrote::Failed
  } else if record.cut {
    Wrote::Corrupt
  } else {
    Wrote::Whole
  }
}

/// The bytes `ff_isom_write_hvcc` writes of `config`, a configuration it
/// writes whole ([`hvcc`]; `hvcc_write`, `hevc.c` 964–1201): the record's
/// 23 bytes, and each array of a unit, three and each unit behind its
/// 16-bit length.
pub(super) fn hvcc_written_len(config: &[u8]) -> u64 {
  hvcc_walk(Config::Bytes(config), false).map_or(0, |record| {
    23 + 3 * record.counts.iter().filter(|&&count| count > 0).count() as u64 + record.unit_bytes
  })
}

/// Whether `ff_isom_write_hvcc` skips a unit of `config` for its layer, the
/// record written without it (`hevc.c` 888–889): of an `hvcC`, every unit
/// of a layer past the base's — no VPS parsed there, no alpha layer named;
/// of Annex B units, one of neither the base layer nor the alpha layer a
/// VPS extension named before it (R11, ruling 1). `false` where the record
/// itself is refused ([`hvcc`]).
pub(super) fn hvcc_drops_layers(config: &[u8]) -> bool {
  hvcc_config_drops_layers(Config::Bytes(config))
}

/// [`hvcc_drops_layers`] of a configuration read in place.
fn hvcc_config_drops_layers(config: Config<'_>) -> bool {
  hvcc_walk(config, false).is_ok_and(|record| record.dropped)
}

/// What `ff_isom_write_lhvc` does with an HEVC configuration (`hevc.c`
/// 1408–1413): the walk `hvcc`'s, every layer's units kept and Annex B's
/// parsed — a layer's SPS that does not parse failing it — and the record
/// of the units of a layer past the base's alone, written by MP4 for a
/// stream flagged multilayer (`movenc.c` 2893–2898, 1654–1674), its failure
/// logged and the box not written. Complete, it refuses arrays of no such
/// SPS or PPS, or of more than 16 and 64, its VPSs unjudged (1087–1093); a
/// written unit of more than 65 535 bytes is written with its length cut.
/// FFmpeg's reader merges its arrays into the configuration and flags the
/// stream multilayer (`mov.c` 8878–8925) — but refuses, the file with it, a
/// record whose arrays take fewer than 8 bytes, its size less 14 negative
/// in `mov_realloc_extradata`'s unsigned test (8907–8914, 2248–2253): one
/// of no unit of a layer past the base's, written of no array, `Corrupt`.
pub(super) fn lhvc(config: &[u8], complete: bool) -> Wrote {
  lhvc_config(Config::Bytes(config), complete)
}

/// [`lhvc`] of a configuration read in place.
fn lhvc_config(config: Config<'_>, complete: bool) -> Wrote {
  let record = match hvcc_walk(config, true) {
    Ok(record) => record,
    Err(wrote) => return wrote,
  };
  let [_, sps, pps, ..] = record.upper;
  if complete && (sps == 0 || sps > 16 || pps == 0 || pps > 64) {
    Wrote::Failed
  } else if record.upper_cut || record.upper_bytes < 8 {
    Wrote::Corrupt
  } else {
    Wrote::Whole
  }
}

/// `write_configuration_record`'s walk of `config` (`hevc.c` 1293–1399),
/// `lhvc` its `FLAG_IS_LHVC`: the record as it stands before it is
/// written, or what the writer answers where the walk stops.
fn hvcc_walk(config: Config<'_>, lhvc: bool) -> Result<Hvcc, Wrote> {
  if config.len() < 6 {
    return Err(Wrote::Failed);
  }
  let mut record = Hvcc::default();
  if let Config::Bytes(config) = config
    && config[0] == 1
  {
    if config.len() < 23 || config.len() > GET_BITS_BYTES_MAX {
      return Err(Wrote::Failed);
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
          return Err(Wrote::Corrupt);
        }
        let start = bits.count() / 8;
        let nal = config.get(start..start + len).unwrap_or(&[]);
        if let Some(array) = HEVC_ARRAYS.iter().position(|&kept| kept == kind)
          && hvcc_add(&mut record, nal, array, (false, lhvc)) == Parse::Failed
        {
          return Err(Wrote::Failed);
        }
        bits.skip_long(len as i64 * 8);
      }
    }
  } else if !config.annex_b() {
    return Err(Wrote::Failed);
  } else {
    for (unit, [first, _]) in walked(config.units()) {
      let kind = (first >> 1) & 0x3F;
      if let Some(array) = HEVC_ARRAYS.iter().position(|&kept| kept == kind)
        && hvcc_add(&mut record, unit, array, (true, lhvc)) == Parse::Failed
      {
        return Err(Wrote::Failed);
      }
    }
  }
  Ok(record)
}

/// `hvcc_add_nal_unit` (`hevc.c` 862–933) of the unit `nal` into the array
/// `array`, `parsed` where the record is made of Annex B units, whose
/// parameter sets it parses, and `lhvc` where every layer's unit is kept.
fn hvcc_add(record: &mut Hvcc, nal: &[u8], array: usize, (parsed, lhvc): (bool, bool)) -> Parse {
  if nal_rbsp_len(nal, 2) > GET_BITS_BYTES_MAX {
    return Parse::Failed;
  }
  // The header's two bytes, which no emulation prevention byte precedes.
  let mut header = Reader::new(nal.get(..2).unwrap_or(nal));
  header.skip(1);
  let kind = header.read(6) as u8;
  let layer = header.read(6) as u8;
  if !lhvc && layer > 0 && layer != record.alpha {
    record.dropped = true;
    return Parse::Done;
  }
  if record.counts[array] >= u32::from(u16::MAX) {
    return Parse::Failed;
  }
  record.counts[array] += 1;
  record.cut |= nal.len() > usize::from(u16::MAX);
  record.unit_bytes += 2 + u64::from(nal.len() as u16);
  if layer > 0 {
    if record.upper[array] == 0 {
      record.upper_bytes += 3;
    }
    record.upper[array] += 1;
    record.upper_cut |= nal.len() > usize::from(u16::MAX);
    record.upper_bytes += 2 + u64::from(nal.len() as u16);
  }
  if !parsed || !(32..=33).contains(&kind) {
    return Parse::Done;
  }
  let mut bits = Reader::of(Bytes::rbsp(nal, 2));
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
  // Of at most seven sub-layers, a 3-bit count.
  let mut flags = [(false, false); 8];
  for flag in flags.iter_mut().take(sub_layers as usize) {
    *flag = (bits.read1() == 1, bits.read1() == 1);
  }
  if sub_layers > 0 {
    for _ in sub_layers..8 {
      bits.skip(2);
    }
  }
  for &(profile, level) in flags.iter().take(sub_layers as usize) {
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
  let first = &mut record.vps[usize::from(id)];
  if first.is_none() {
    *first = Some(sub_layers as u8);
  }
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
      match record.vps[usize::from(vps_id)] {
        Some(sub_layers) => (7, u32::from(sub_layers)),
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
  vvcc_config(Config::Bytes(config))
}

/// [`vvcc`] of a configuration read in place.
fn vvcc_config(config: Config<'_>) -> Wrote {
  if config.len() < 6 {
    return Wrote::Failed;
  }
  if config.first().is_some_and(|first| first & 0xF8 == 0xF8) {
    return Wrote::Whole;
  }
  if !config.annex_b() {
    return Wrote::Failed;
  }
  let mut counts = [0_u16; 6];
  let mut cut = false;
  for (unit, [_, second]) in walked(config.units()) {
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

/// The most bytes `ff_isom_write_vvcc` writes of `config`, a configuration
/// it writes whole ([`vvcc`]; `vvcc_write`, `vvc.c` 569–808): a `vvcC` as
/// it is; of Annex B units, a byte, the profile, tier and level at their
/// most — 1 049 bytes, of nine bytes of constraint information, six
/// sub-layers' levels and 255 sub-profiles (693–776), which its VPS and SPS
/// set and this release does not parse — a byte of the arrays' count, and
/// each array of a unit, a byte, two of its count but an OPI array's, and
/// each unit behind its 16-bit length (779–805).
pub(super) fn vvcc_written_bound(config: &[u8]) -> u64 {
  if config.len() < 6 {
    return 0;
  }
  if config[0] & 0xF8 == 0xF8 {
    return config.len() as u64;
  }
  if !annex_b(config) {
    return 0;
  }
  let mut counts = [0_u64; 6];
  let mut units = 0_u64;
  for (unit, [_, second]) in walked(nal_units(config)) {
    let Some(array) = VVC_ARRAYS.iter().position(|&kept| kept == second >> 3) else {
      continue;
    };
    counts[array] += 1;
    units += 2 + u64::from(unit.len() as u16);
  }
  let arrays: u64 = counts
    .iter()
    .zip(VVC_ARRAYS)
    .filter(|(count, _)| **count > 0)
    // The OPI array's, the first, holds no count (794–796).
    .map(|(_, kind)| if kind == VVC_ARRAYS[0] { 1 } else { 3 })
    .sum();
  1 + 1049 + 1 + arrays + units
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
    bytes: Bytes::Plain(rest.get(2..).unwrap_or(&[])),
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
/// FFmpeg 9.0.1), read in place.
#[derive(Clone, Copy)]
struct Split<'a> {
  /// `raw_data`, `raw_size`: its bytes, its emulation prevention in.
  raw: &'a [u8],
  /// `data`, `size`: its RBSP; `skipped_bytes_pos` is [`skipped`]'s of
  /// `raw`.
  rbsp: Bytes<'a>,
  /// Its header's unit type.
  kind: u8,
}

/// `ff_h2645_packet_split` over the Annex B units of `bytes` (`h2645_parse.c`
/// 527–665 in FFmpeg 9.0.1): each unit from past a start code to the next —
/// or to the next `00 00 02` once an emulation prevention byte is met — its
/// RBSP's trailing zeros and stop bit counted off; one of no bits, or whose
/// header its codec refuses, dropped. `None` where it fails: a search
/// reaching the end with no unit kept — of no start code at all — or a unit
/// of more bits than `init_get_bits` takes. Its padding, small or not,
/// changes no unit it splits, only whether libavcodec copies one that holds
/// no emulation prevention byte. Walked once to judge it, then again for
/// its units, nothing held. Where `limits` are given — the split of no
/// small padding `ff_isom_write_lvcc` asks for — what it allocates by its
/// input's sizes fails it past them ([`SplitLimits`]).
fn h2645_split(bytes: &[u8], units: Units, limits: Option<SplitLimits>) -> Option<H2645Split<'_>> {
  if limits.is_some_and(|limits| bytes.len() > limits.bytes) {
    return None;
  }
  let split = H2645Split {
    bytes,
    units,
    at: 0,
    kept: false,
    limits,
    kept_units: 0,
    slots: 0,
    positions: 0,
  };
  let mut judged = split;
  loop {
    match judged.step() {
      Ok(Some(_)) => {}
      Ok(None) => return Some(split),
      Err(()) => return None,
    }
  }
}

/// The units [`h2645_split`] splits off, in order.
#[derive(Clone, Copy)]
struct H2645Split<'a> {
  bytes: &'a [u8],
  units: Units,
  at: usize,
  /// Whether a unit is kept so far.
  kept: bool,
  /// What the split allocates is held to, where mirrored.
  limits: Option<SplitLimits>,
  /// `nb_nals`: the units kept so far.
  kept_units: u64,
  /// `nals_allocated`: the slots of the unit list so far.
  slots: u64,
  /// `skipped_bytes_pos_size` of the slot the next unit is split into.
  positions: u64,
}

/// `MAX_MBPAIR_SIZE` (`h2645_parse.h` 32): the padding a split of no small
/// padding allocates its RBSP buffer with.
const MAX_MBPAIR_SIZE: usize = 256 * 1024;

/// `GetBitContext`, libavcodec's uncached bit reader (`get_bits.h`
/// 109–114 in FFmpeg 9.0.1), mirrored for its size alone.
#[repr(C)]
#[allow(dead_code)]
struct GetBitContext {
  buffer: *const u8,
  index: i32,
  size_in_bits: i32,
  size_in_bits_plus8: i32,
}

/// `H2645NAL`, a split's unit (`h2645_parse.h` 34–72 in FFmpeg 9.0.1),
/// mirrored for its size alone, which the target's C layout sets: 88
/// bytes of a 64-bit target.
#[repr(C)]
#[allow(dead_code)]
struct H2645Nal {
  data: *const u8,
  size: i32,
  size_bits: i32,
  raw_size: i32,
  raw_data: *const u8,
  gb: GetBitContext,
  kind: i32,
  ref_idc: i32,
  temporal_id: i32,
  nuh_layer_id: i32,
  skipped_bytes: i32,
  skipped_bytes_pos_size: i32,
  skipped_bytes_pos: *mut i32,
}

/// What `ff_h2645_packet_split` of no small padding allocates by its
/// input's sizes, each refused past libavutil's `INT_MAX` and the split
/// failed (R20, r2: `ff_isom_write_lvcc`'s split, `lcevc.c` 270, whose
/// failure MP4 discards, the `lvcC` written empty): its RBSP buffer, the
/// configuration's bytes, `MAX_MBPAIR_SIZE` and the padding
/// (`alloc_rbsp_buffer`, `h2645_parse.c` 480–486; 534–540); its unit
/// list, a slot a unit, the next one asked for before each unit after a
/// kept one and the first (590–607); and each slot's emulation prevention
/// positions, `int`s, at first `min(1024, b / 3 + 1)` of the `b` bytes left
/// from the unit that asks for the slot, doubled as a unit's emulation
/// prevention bytes outrun them (`ff_h2645_extract_rbsp`, 112–125). Each
/// exact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SplitLimits {
  /// The most bytes split: `INT_MAX` less the padding and
  /// `MAX_MBPAIR_SIZE`.
  pub(super) bytes: usize,
  /// The slot count asked for that is refused, and every one past it:
  /// `INT_MAX` over an `H2645NAL`'s bytes (594).
  pub(super) slots: u64,
  /// The most bytes a slot's positions take: libavutil's `INT_MAX`
  /// (`av_reallocp_array`).
  pub(super) positions: u64,
}

impl SplitLimits {
  /// libavcodec's and libavutil's own.
  pub(super) const REAL: Self = Self {
    bytes: i32::MAX as usize - 64 - MAX_MBPAIR_SIZE,
    slots: (i32::MAX as usize / core::mem::size_of::<H2645Nal>()) as u64,
    positions: i32::MAX as u64,
  };
}

impl<'a> H2645Split<'a> {
  /// The next unit, `None` at the end, or `Err` where the split fails.
  fn step(&mut self) -> Result<Option<Split<'a>>, ()> {
    let bytes = self.bytes;
    let length = bytes.len();
    while length - self.at >= 4 {
      // `find_next_start_code`: past the first start code with a byte after
      // it, or to the end (`h2645_parse.c` 465–478).
      let at = self.at;
      let rest = length - at;
      self.at += (0..rest - 3)
        .find(|&i| bytes[at + i] == 0 && bytes[at + i + 1] == 0 && bytes[at + i + 2] == 1)
        .map_or(rest, |i| i + 3);
      if self.at == length {
        return if self.kept { Ok(None) } else { Err(()) };
      }
      let start = self.at;
      // The unit's slot, where the list asks for a new one (590–607).
      if let Some(limits) = self.limits
        && self.slots == self.kept_units
      {
        if self.kept_units + 1 >= limits.slots {
          return Err(());
        }
        self.slots = self.kept_units + 1;
        self.positions = 1024.min((length - start) as u64 / 3 + 1);
      }
      self.at += extract_rbsp_len(&bytes[start..]);
      let raw = &bytes[start..self.at];
      // Its emulation prevention bytes' positions, each doubling of their
      // room asked for of libavutil (112–125).
      if let Some(limits) = self.limits {
        let count = skipped(raw).count() as u64;
        while count > self.positions {
          self.positions *= 2;
          if self.positions * 4 > limits.positions {
            return Err(());
          }
        }
      }
      let rbsp = Bytes::rbsp(raw, 0);
      let skip_trailing_zeros = bytes.get(self.at..self.at + 4) != Some(&[0, 0, 1, 0xE0][..]);
      let bits = bit_length(
        rbsp,
        if self.units == Units::Hevc { 2 } else { 1 },
        skip_trailing_zeros,
      );
      if rbsp.is_empty() || bits <= 0 {
        continue;
      }
      if bits >= i64::from(i32::MAX - 512) {
        return Err(());
      }
      let mut header = Reader {
        bytes: rbsp,
        bits: bits as usize,
        index: 0,
      };
      let kind = match self.units {
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
      self.kept = true;
      self.kept_units += 1;
      return Ok(Some(Split {
        raw,
        rbsp,
        kind: kind as u8,
      }));
    }
    Ok(None)
  }
}

impl<'a> Iterator for H2645Split<'a> {
  type Item = Split<'a>;

  /// The next unit — of a split [`h2645_split`] judged whole.
  fn next(&mut self) -> Option<Split<'a>> {
    self.step().ok().flatten()
  }
}

/// `ff_h2645_extract_rbsp` (`h2645_parse.c` 37–149) of the unit opening
/// `src`, which runs to the end at most: its raw size, its RBSP the bytes
/// of it each emulation prevention byte dropped ([`Bytes::rbsp`]). It ends
/// at the first start code before an emulation prevention byte, at the
/// first `00 00 01` or `00 00 02` after one.
fn extract_rbsp_len(src: &[u8]) -> usize {
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
  let mut si = found.min(length);
  while si + 2 < length {
    if src[si + 2] > 3 {
      // The two bytes copied, and the one after them below.
      si += 2;
    } else if src[si] == 0 && src[si + 1] == 0 && src[si + 2] != 0 {
      if src[si + 2] == 3 {
        si += 3;
        continue;
      }
      return si;
    }
    si += 1;
  }
  length
}

/// `get_bit_length` (`h2645_parse.c` 348–376): the bits of `rbsp` but its
/// trailing zeros — where they are skipped — and its stop bit; one of no
/// byte answering zero, one shorter than `min_size` or past `INT_MAX / 8`
/// bytes below zero.
fn bit_length(rbsp: Bytes<'_>, min_size: usize, skip_trailing_zeros: bool) -> i64 {
  let len = rbsp.len();
  let mut size = if skip_trailing_zeros {
    rbsp
      .iter()
      .enumerate()
      .filter(|&(_, byte)| byte != 0)
      .last()
      .map_or(0, |(at, _)| at + 1)
  } else {
    len
  };
  if size == 0 {
    return 0;
  }
  let mut padding = 0;
  if size <= min_size {
    if len < min_size {
      return -1;
    }
    size = min_size;
  } else if let Some(last) = rbsp.get(size - 1).filter(|&last| last != 0) {
    padding = i64::from(last.trailing_zeros()) + 1;
  }
  if size > i32::MAX as usize / 8 {
    return -1;
  }
  size as i64 * 8 - padding
}

/// The configuration FLV's `extract_extradata` makes of an H.264, HEVC or
/// VVC packet, read in place ([`h2645_extracted`]): the packet's parameter
/// sets in their order, each its raw bytes behind a 4-byte start code.
#[derive(Clone, Copy)]
pub(super) struct Extracted<'a> {
  bytes: &'a [u8],
  units: Units,
  kept: &'static [u8],
  /// Its bytes: each set's and its start code's.
  len: usize,
}

impl<'a> Extracted<'a> {
  /// Its bytes, made: the laws' comparison.
  #[cfg(test)]
  pub(super) fn bytes(self) -> Vec<u8> {
    let kept = self.kept;
    h2645_split(self.bytes, self.units, None)
      .into_iter()
      .flatten()
      .filter(|unit| kept.contains(&unit.kind))
      .flat_map(|unit| [&[0_u8, 0, 0, 1][..], unit.raw].concat())
      .collect()
  }

  /// Its units, as libavformat's Annex B splitter finds them in it: each
  /// set's raw bytes, its trailing zeros dropped — none holds a start code,
  /// which the split ends a unit before, so each start code it is put
  /// behind opens it, and its zeros before the next are the splitter's to
  /// drop.
  fn units(self) -> impl Iterator<Item = &'a [u8]> {
    let kept = self.kept;
    h2645_split(self.bytes, self.units, None)
      .into_iter()
      .flatten()
      .filter(move |unit| kept.contains(&unit.kind))
      .map(|unit| {
        let mut size = unit.raw.len();
        while size > 0 && unit.raw[size - 1] == 0 {
          size -= 1;
        }
        &unit.raw[..size]
      })
  }
}

/// The configuration FLV's `extract_extradata` makes of the H.264, HEVC or
/// VVC packet `bytes`, which replaces the packet's own new extradata —
/// `Ok(None)` where it makes none — or `Err` where the filter refuses the
/// packet, which libavformat then drops (`extract_extradata_h2645`,
/// `extract_extradata.c` 166–275 in FFmpeg 9.0.1, over
/// `ff_h2645_packet_split` of small padding; `mux.c` 1132–1140): the
/// packet's parameter sets in their order — H.264's SPSs and PPSs, HEVC's
/// and VVC's VPSs, SPSs and PPSs — each its raw bytes behind a 4-byte
/// start code, where an SPS is among them, and for HEVC a VPS; read in
/// place, never made (R15).
pub(super) fn h2645_extracted(units: Units, bytes: &[u8]) -> Result<Option<Extracted<'_>>, ()> {
  let split = h2645_split(bytes, units, None).ok_or(())?;
  let (kept, sps, vps): (&'static [u8], u8, Option<u8>) = match units {
    Units::Vvc => (&[14, 15, 16], 15, None),
    Units::Hevc => (&[32, 33, 34], 33, Some(32)),
    Units::H264 => (&[7, 8], 7, None),
    // LCEVC's configuration is another of the filter's roads, which FLV,
    // writing no LCEVC, never reaches.
    Units::Lcevc => return Ok(None),
  };
  let sets = || split.filter(|unit| kept.contains(&unit.kind));
  let present = |kind: u8| sets().any(|unit| unit.kind == kind);
  if sets().next().is_none() || !present(sps) || vps.is_some_and(|vps| !present(vps)) {
    return Ok(None);
  }
  Ok(Some(Extracted {
    bytes,
    units,
    kept,
    len: sets().map(|unit| 4 + unit.raw.len()).sum(),
  }))
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
fn lcevc_unit(unit: Split<'_>) -> LcevcUnit {
  let data = unit.rbsp;
  let mut at = data.len().min(2);
  let mut length = 3;
  let (mut sequence, mut global, mut unnamed) = (false, false, false);
  let mut skipped = skipped(unit.raw).peekable();
  while data.len() - at > 1 && !(sequence && global) {
    let left = data.len() - at;
    let mut bits = Reader::of(data.from(at));
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
    while skipped.next_if(|&position| position < end).is_some() {
      raw_block += 1;
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
/// libavcodec splits them, of no small padding, refused where that fails —
/// what the split allocates by the configuration's sizes among it
/// ([`SplitLimits`]; R20, r2); every IDR and non-IDR unit written through
/// `write_nalu` ([`lcevc_unit`]), refused where it refuses one of no
/// sequence or global configuration, its 16-bit length cut past 65 535;
/// none at all refused; the counts of each, 16 bits, wrapped past 65 535.
pub(super) fn lvcc(config: &[u8]) -> Wrote {
  lvcc_within(config, SplitLimits::REAL)
}

/// [`lvcc`], its split held to `limits`: the laws' seam.
pub(super) fn lvcc_within(config: &[u8], limits: SplitLimits) -> Wrote {
  if config.len() <= 6 {
    return Wrote::Failed;
  }
  if !annex_b(config) {
    return Wrote::Whole;
  }
  let Some(split) = h2645_split(config, Units::Lcevc, Some(limits)) else {
    return Wrote::Failed;
  };
  let (mut idr, mut other, mut cut) = (0_usize, 0_usize, false);
  for unit in split {
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

/// The bits past which `init_get_bits`'s `int` count of a VC-1 unit's
/// bytes, eight a byte, wraps: `2^28` bytes (`movenc.c` 1134–1135).
pub(super) const VC1_UNIT_BYTES_WRAP: usize = 1 << 28;

/// Whether `mov_write_dvc1_structs` reads a unit of the VC-1
/// configuration `config` past what its reader holds (R20, r2; `movenc.c`
/// 1102–1184): between its markers — as [`dvc1`] finds them — each unit is
/// unescaped into a buffer of the configuration's size
/// (`vc1_unescape_buffer`, `vc1_common.h` 70–88) and its bits counted
/// `8 * size` in an `int` for `init_get_bits` (`get_bits.h` 517–534): a
/// unit of `wrap` bytes or more — [`VC1_UNIT_BYTES_WRAP`] — its count
/// wrapped, the length undefined; a sequence header of more than `reader`
/// bytes — [`GET_BITS_BYTES_MAX`], past which `init_get_bits` answers a
/// reader of no buffer, unchecked — its fields read through that null
/// pointer (1137–1149). Each unit's bytes counted unescaped, exactly.
pub(super) fn dvc1_reads_past(config: &[u8], reader: usize, wrap: usize) -> bool {
  let len = config.len();
  let marker = |from: usize| -> usize {
    if len < from.saturating_add(4) {
      return len;
    }
    (from..=len - 4)
      .find(|&at| config[at] == 0 && config[at + 1] == 0 && config[at + 2] == 1)
      .unwrap_or(len)
  };
  let mut start = marker(0);
  while start < len {
    let next = marker(start + 4);
    let size = next.saturating_sub(start + 4);
    if size > 0 {
      let sequence = u32::from_be_bytes([
        config[start],
        config[start + 1],
        config[start + 2],
        config[start + 3],
      ]) == VC1_SEQUENCE_HEADER;
      // Fewer bytes than either bound are fewer unescaped: counted only
      // past.
      if size >= reader.min(wrap) {
        let unescaped = vc1_unescaped_len(&config[start + 4..next]);
        if unescaped >= wrap || (sequence && unescaped > reader) {
          return true;
        }
      }
      // A sequence header of another profile than the advanced one ends
      // the walk, its first byte its own (1137–1141).
      if sequence && config[start + 4] >> 6 != 3 {
        return false;
      }
    }
    start = next;
  }
  false
}

/// The bytes `vc1_unescape_buffer` makes of `unit` (`vc1_common.h`
/// 70–88): a `3` after two zeros and before a byte below 4 dropped, from
/// its third byte to its last but one; a unit of fewer than 4 bytes as it
/// is.
fn vc1_unescaped_len(unit: &[u8]) -> usize {
  let size = unit.len();
  if size < 4 {
    return size;
  }
  let (mut i, mut dropped) = (0, 0);
  while i < size {
    if unit[i] == 3
      && i >= 2
      && unit[i - 1] == 0
      && unit[i - 2] == 0
      && i < size - 1
      && unit[i + 1] < 4
    {
      dropped += 1;
      i += 2;
    } else {
      i += 1;
    }
  }
  size - dropped
}

/// The PBU types of a frame, each its own entry of APV's `apvC`
/// (`decompose_unit_types`, `apv.c` 208–211): primary, non-primary,
/// preview, depth and alpha.
const APV_FRAMES: [u8; 5] = [1, 2, 25, 26, 27];

/// One frame's information as MP4's parse of an APV access unit records it
/// in `apvC`, and compares it with those recorded, field by field
/// (`APVDecoderFrameInfo`, `apv.c` 34–62; `ff_isom_parse_apvc`, 253–318):
/// the frame information's fields, the colour description's where the
/// frame header states one — zeros where it does not — and
/// `capture_time_distance_ignored` set, which every record shares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ApvFrameInfo {
  profile: u8,
  level: u8,
  band: u8,
  width: u32,
  height: u32,
  chroma: u8,
  depth_minus8: u8,
  distance: u8,
  /// The colour primaries, transfer characteristics, matrix coefficients
  /// and full range flag, where the frame header states them.
  color: Option<[u8; 4]>,
}

/// The frame informations MP4's parse of the APV access unit `au` records,
/// each with its PBU type, in order (`ff_isom_parse_apvc`, `apv.c`
/// 253–318): none where the parse fails — a packet of fewer than 8 bytes
/// or of no signature, or a split `cbs_apv_split_fragment` refuses, a PBU
/// shorter than 8 bytes or past what is left, a PBU header's reserved
/// byte set (`cbs_apv.c` 162–244; `cbs_apv_syntax_template.c` 19–29) — and
/// none past a frame of a bit depth past 16 or odd, where the parse stops.
/// A frame's header is read for its frame information and its colour
/// description (`cbs_apv_syntax_template.c` 63–91, 142–163), each range the
/// syntax checks there checked — a frame of a reserved field set, of
/// 4:2:0 or a chroma format past 4, of a bit depth below 10 or past 16, or
/// too short for them, failing the parse; its tiles, which the parse reads
/// too, rest on the trust model: a stream the standards allow, they parse.
pub(super) fn apv_frame_infos(au: &[u8]) -> ApvFrameInfos<'_> {
  const SIGNATURE: &[u8; 4] = b"aPv1";
  let none = ApvFrameInfos { units: &[] };
  if au.len() < 8 || &au[..4] != SIGNATURE {
    return none;
  }
  let units = ApvFrameInfos { units: &au[4..] };
  // Every PBU split whole, and every frame's header parsed, or none read.
  let mut rest = units.units;
  while !rest.is_empty() {
    let Some((unit, after)) = apv_unit(rest) else {
      return none;
    };
    if APV_FRAMES.contains(&unit[0]) && apv_frame_info(&unit[4..]).is_none() {
      return none;
    }
    rest = after;
  }
  units
}

/// The PBU opening `rest` — its 4-byte size, at least 8 and within what is
/// left, its header's reserved byte clear — and what follows it.
fn apv_unit(rest: &[u8]) -> Option<(&[u8], &[u8])> {
  if rest.len() < 8 {
    return None;
  }
  let size = u32::from_be_bytes([rest[0], rest[1], rest[2], rest[3]]) as usize;
  let rest = &rest[4..];
  if size < 8 || size > rest.len() || rest[3] != 0 {
    return None;
  }
  Some(rest.split_at(size))
}

/// The frame informations [`apv_frame_infos`] reads of an access unit, in
/// order, read in place.
#[derive(Clone, Copy)]
pub(super) struct ApvFrameInfos<'a> {
  units: &'a [u8],
}

impl Iterator for ApvFrameInfos<'_> {
  type Item = (u8, ApvFrameInfo);

  fn next(&mut self) -> Option<(u8, ApvFrameInfo)> {
    while let Some((unit, after)) = apv_unit(self.units) {
      self.units = after;
      if !APV_FRAMES.contains(&unit[0]) {
        continue;
      }
      let info = apv_frame_info(&unit[4..])?;
      let depth = info.depth_minus8 + 8;
      if !(8..=16).contains(&depth) || depth % 2 != 0 {
        self.units = &[];
        return None;
      }
      return Some((unit[0], info));
    }
    None
  }
}

/// A frame header's frame information and colour description, its ranges
/// checked as the syntax checks them, or `None` where it does not parse.
fn apv_frame_info(header: &[u8]) -> Option<ApvFrameInfo> {
  if header.len() < 14 {
    return None;
  }
  let mut bits = Reader::new(header);
  let profile = bits.read(8) as u8;
  let level = bits.read(8) as u8;
  let band = bits.read(3) as u8;
  let reserved = bits.read(5);
  let width = bits.read(24);
  let height = bits.read(24);
  let chroma = bits.read(4) as u8;
  let depth_minus8 = bits.read(4) as u8;
  let distance = bits.read(8) as u8;
  let reserved_8 = bits.read(8);
  let reserved_header = bits.read(8);
  if reserved != 0
    || reserved_8 != 0
    || reserved_header != 0
    || chroma > 4
    || chroma == 1
    || !(2..=8).contains(&depth_minus8)
  {
    return None;
  }
  let color = if bits.read1() == 1 {
    if header.len() < 18 {
      return None;
    }
    Some([
      bits.read(8) as u8,
      bits.read(8) as u8,
      bits.read(8) as u8,
      bits.read1() as u8,
    ])
  } else {
    None
  };
  Some(ApvFrameInfo {
    profile,
    level,
    band,
    width,
    height,
    chroma,
    depth_minus8,
    distance,
    color,
  })
}

/// A configuration writer the MOV family or FLV calls and ignores the
/// answer of, with what it reads beside the configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Writer {
  /// `ff_isom_write_avcc`.
  Avcc,
  /// `ff_isom_write_hvcc`, its arrays complete or not, and MP4's
  /// `ff_isom_write_lhvc` beside it where the stream is flagged multilayer.
  Hvcc { complete: bool, lhvc: bool },
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
      Self::Vpcc { format, profile } => vpcc(config.len(), format, profile),
      Self::Av1c => {
        if super::av1c_written(config) {
          Wrote::Whole
        } else {
          Wrote::Failed
        }
      }
      Self::Evcc => evcc(config),
      Self::Lvcc => lvcc(config),
      Self::Av3c => av3c(config),
      Self::Dvc1 => dvc1(config),
      _ => self.writes_config(Config::Bytes(config)),
    }
  }

  /// What the writer does with `config`, read in place: the H.264, HEVC
  /// and VVC writers', which FLV hands what `extract_extradata` makes.
  pub(super) fn writes_config(self, config: Config<'_>) -> Wrote {
    match self {
      Self::Avcc => avcc_config(config),
      // A layer's units `hvcC` skips are lost, but where `lhvC` writes
      // them; a stream flagged multilayer whose `lhvC` fails of no such
      // unit loses its flag alone, which the report names (R11, ruling 1).
      Self::Hvcc {
        complete,
        lhvc: beside,
      } => match hvcc_config(config, complete) {
        Wrote::Whole if beside => match lhvc_config(config, complete) {
          Wrote::Corrupt => Wrote::Corrupt,
          Wrote::Failed if hvcc_config_drops_layers(config) => Wrote::Failed,
          _ => Wrote::Whole,
        },
        Wrote::Whole if hvcc_config_drops_layers(config) => Wrote::Failed,
        wrote => wrote,
      },
      Self::Vvcc => vvcc_config(config),
      // A configuration of their own bytes.
      Self::Evcc | Self::Lvcc | Self::Vpcc { .. } | Self::Av1c | Self::Av3c | Self::Dvc1 => {
        match config {
          Config::Bytes(bytes) => self.writes(bytes),
          Config::Extracted(_) => Wrote::Failed,
        }
      }
    }
  }
}
