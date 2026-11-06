//! The MOV family's walk: its boxes, followed by their sizes as the sink
//! takes them, the tables the audit judges folded as they stream.
//!
//! **What it descends into.** A box movenc writes with a placeholder size
//! — `avio_wb32(pb, 0)`, patched by `update_size` once its last byte is
//! written (`movenc.c` 155–175 in FFmpeg 9.0.1) — reaches the sink with its
//! final size where its header was still buffered when it closed, and with
//! the placeholder, the patch a write of its own after its last byte,
//! where the header was flushed first: at any 32 KiB boundary of the I/O
//! context's buffer. A child's patch then lands inside its parent, so the
//! walk descends into every box movenc writes children of placeholder size
//! into — `moov`, `trak`, `mdia`, `minf`, `stbl`, `stsd`, each sample
//! entry, and the entries' `wave`, `sv3d` and `vexu` — and steps over any
//! other by its size, or, while its size is a placeholder, takes every
//! byte into it until its patch: each of those holds no child of a
//! placeholder size of its own (`udta` is written whole from a dynamic
//! buffer, 5066–5122; `dinf`, `gmhd`, `tapt`, `tref`, `proj` and `eyes`
//! hold children of fixed sizes). A raw subtitle entry — `tx3g`, `text`,
//! `stpp` — holds its configuration's bytes, then a `btrt` of 20 bytes
//! whose size is a placeholder of its own (1279–1294, 2282–2286): that one
//! patch is admitted where it falls, at the frontier less 20.
//!
//! **What it parses:** each `trak`'s `tkhd` id (`mov_write_tkhd_tag`, at
//! payload offset 12 of version 0 and 20 of version 1) and `mdia`'s `hdlr`
//! handler type (payload offset 8), which says how its sample entries are
//! laid out; each `stts` and `stsz` ([`Runs`], and R5's [`CueTables`]); the
//! `mdat`'s size and the payload the sink took after its header.
//!
//! **Each sample description,** as its entry's last byte is read, against
//! what the muxer handed the tap of it before the trailer ([`Handed`]; R6):
//! its configuration record's presence and payload length — the record
//! boxes `avcC`, `hvcC`, `lhvC`, `vvcC`, `evcC`, `lvcC`, `av1C`, `dvc1` and
//! `apvC`, counted as they stream and never kept — E-AC-3's `dec3` in MOV's
//! `wave`, against its own fields, and DVD subtitles' palette, the `esds`'s
//! decoder-specific information, whose tag `put_descr` writes at payload
//! offset 30, its length in four bytes of seven bits (694–701, 765–816),
//! its bytes folded a word at a time ([`palette_fold`]); and, as the `stsd`
//! closes, their count. Each `trak` keeps its first difference, of fixed
//! size, so that the walk's memory stays the table's at open.
//!
//! **AVIF** (`avif_write_trailer`, 9154–9213) writes its whole file at the
//! trailer: `ftyp`; `meta`, a container after its version and flags, whose
//! `iloc` the walk parses — its items' extents, each offset a field
//! patched once the payload is written (3660–3681, 9200–9210) — and whose
//! `iprp`'s `ipco` holds each item's `av1C`, counted as a record; an
//! animated image's `moov`, walked as MP4's, its first `trak`'s first
//! chunk offset read; then the `mdat`, its size known as it opens, its
//! payload counted to its end. No `moov` but an animated image's.
//!
//! **The fields patched beside the boxes' sizes:** `stsc`'s, `stss`'s and
//! `stps`'s entry counts at box offset 12, rewritten before their boxes'
//! sizes (283–286, 308–311); `stsd`'s version at box offset 8, rewritten
//! after its size (`update_size_and_version`, 165–175); the `mdat`'s size,
//! of 32 bits at its header, or of 64 over the `free` or `wide` box before
//! it, `00000001 'mdat' u64` (`mov_write_mdat_size`, 8991–9012), accepted
//! at any value and judged by the verdict. Each is optional — a patch made
//! while its target was buffered never reaches the sink on its own — and
//! each may reach it in parts, where the sink takes a write in parts.
//! AVIF's extent offsets, each accepted at any value and judged by the
//! verdict, like the `mdat`'s.

use super::{
  Counts, CueTables, EntryExpected, FOLD_SEED, Found, Handed, RecordExpected, Runs, Summary,
  Unfollowed, Why, fold,
};
use crate::mux::MovMode;

/// The record boxes a sample entry carries its configuration in, which the
/// walk counts (`movenc.c` 1573–1738, 1102–1201).
const RECORDS: [[u8; 4]; 9] = [
  *b"avcC", *b"hvcC", *b"lhvC", *b"vvcC", *b"evcC", *b"lvcC", *b"av1C", *b"dvc1", *b"apvC",
];

/// The bytes of an `esds`'s payload before its decoder-specific
/// information's tag: its version and flags, the ES descriptor's header of
/// five, the track id and flags, the decoder configuration descriptor's
/// header of five and its thirteen bytes (`mov_write_esds_tag`, 765–816).
const DSI_TAG_AT: u64 = 30;

/// DVD subtitles' palette: sixteen colours of four bytes (8037–8043).
const PALETTE_BYTES: u64 = 64;

/// The items AVIF writes, its color plane's and its alpha plane's
/// (3660–3681): one a stream, at most two.
const ITEMS: usize = 2;

/// The deepest the walk descends: `moov`, `trak`, `mdia`, `minf`, `stbl`,
/// `stsd`, an entry, its `wave`, and a box in that; FFmpeg writes no
/// deeper path the walk descends.
const DEPTH: usize = 16;

/// The fields the walk keeps patching at once: a size or count in parts,
/// and the optional ones registered.
const FIELDS: usize = 8;

/// What the walk does with a box's payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
  /// Boxes, after `prefix` bytes of the box's own: `stsd`'s version, flags
  /// and count.
  Boxes { prefix: u64 },
  /// A sample entry: its fixed part, then boxes. `None` while an audio
  /// entry of MOV's mode has not shown its version, which sets it.
  Entry { fixed: Option<u64> },
  /// A raw subtitle entry: its own bytes, and perhaps a trailing `btrt`.
  RawEntry,
  /// The media data, counted.
  MediaData,
  /// A sample entry's configuration record, counted ([`RECORDS`]).
  Record,
  /// AVIF's `av1C` of an item, in its `ipco`, counted.
  ItemRecord,
  /// A box the walk parses.
  Leaf(Leaf),
  /// A box the walk steps over.
  Opaque,
}

/// The boxes the walk parses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Leaf {
  /// A track header: its version, and its track id.
  Tkhd,
  /// `mdia`'s handler: its type.
  Hdlr,
  /// A time-to-sample table.
  Stts,
  /// A sample-size table.
  Stsz,
  /// An `mp4s` entry's `esds`: its decoder-specific information.
  Esds,
  /// E-AC-3's `dec3` in MOV's `wave`.
  Dec3,
  /// AVIF's item locations.
  Iloc,
  /// An animated AVIF's chunk offsets, of 32 bits, or of 64.
  Stco,
  Co64,
}

/// An open box.
#[derive(Debug, Clone, Copy)]
struct Frame {
  fourcc: [u8; 4],
  /// Its header's offset.
  at: u64,
  /// Where it ends, or `None` while its size is a placeholder.
  end: Option<u64>,
  role: Role,
  /// The payload's bytes read so far.
  read: u64,
  /// An audio entry's version, at payload offsets 8 and 9.
  version: [u8; 2],
}

const EMPTY_FRAME: Frame = Frame {
  fourcc: [0; 4],
  at: 0,
  end: None,
  role: Role::Opaque,
  read: 0,
  version: [0; 2],
};

/// A box header being read: its offset and its bytes, 8 or 16.
#[derive(Debug, Clone, Copy, Default)]
struct Header {
  at: u64,
  bytes: [u8; 16],
  have: usize,
}

/// A rewrite the walk admits, being filled.
#[derive(Debug, Clone, Copy)]
struct Field {
  at: u64,
  len: usize,
  filled: usize,
  bytes: [u8; 16],
  kind: FieldKind,
  /// Whether its patch may come now: `stsd`'s version only once its box
  /// closed at its size's patch, and no later than the next append.
  armed: bool,
}

/// What a [`Field`] rewrites.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FieldKind {
  /// The innermost open box's placeholder size.
  Size,
  /// The `mdat`'s size, of 32 bits.
  Mdat32,
  /// The `mdat`'s size, of 64 bits over the box before it.
  Mdat64,
  /// An `stsc`'s, `stss`'s or `stps`'s entry count, while its box is open.
  Count,
  /// The `stsd`'s version, from its box's close at its size's patch to the
  /// next append.
  Version,
  /// A raw subtitle entry's trailing `btrt`'s size.
  Btrt,
  /// AVIF's extent offset of the item at this index, written 0, then
  /// patched once its payload is placed.
  Extent(usize),
}

/// How a sample entry of a track is laid out, by its `mdia`'s handler.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Class {
  /// No handler read yet.
  #[default]
  Unknown,
  /// `vide`: 78 bytes of its own, then boxes.
  Video,
  /// `soun`: 28 bytes of its own, then boxes; in MOV's mode 44 or 64 of
  /// version 1 or 2 (`mov_write_audio_tag`, 1385–1403, 1416–1485). MP4's
  /// version 1 of `ipcm` and `fpcm` past 65 535 Hz keeps the 28 (1476).
  Audio,
  /// Any other: `mp4s` 8 bytes of its own then boxes; any other entry
  /// raw (`mov_write_subtitle_tag`, 2252–2290).
  Other,
}

/// What the walk read of one `trak`, in the order the file holds them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(in crate::mux) struct TrakSeen {
  id: Option<u32>,
  /// Its `stts`, read plainly and by R5's rule.
  times: Option<(Summary, Summary)>,
  /// Its `stsz`, read plainly and by R5's rule.
  sizes: Option<(Summary, Summary)>,
  /// The first of its sample descriptions read otherwise than handed: its
  /// index, what it carries, and what was found.
  descriptions: Option<(u32, Described, Found)>,
  /// An animated AVIF's: its first chunk's offset.
  first_chunk: Option<u64>,
}

/// What of a sample description a difference is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::mux) enum Described {
  /// Its configuration record, or its count.
  Configuration,
  /// DVD subtitles' palette.
  Palette,
}

impl TrakSeen {
  /// The track header's id.
  pub(in crate::mux) const fn id(&self) -> Option<u32> {
    self.id
  }

  /// The first sample description read otherwise than handed, its index,
  /// and what was found; `None` where every one was read as handed, or the
  /// walk was handed none of the track's.
  pub(in crate::mux) const fn descriptions(&self) -> Option<(u32, Described, Found)> {
    self.descriptions
  }

  /// Its first chunk's offset, where read: an animated AVIF's.
  pub(in crate::mux) const fn first_chunk(&self) -> Option<u64> {
    self.first_chunk
  }

  /// The time-to-sample table, read by R5's rule where `cues`.
  pub(in crate::mux) fn times(&self, cues: bool) -> Option<Summary> {
    self
      .times
      .map(|(plain, cue)| if cues { cue } else { plain })
  }

  /// The sample sizes, read by R5's rule where `cues`.
  pub(in crate::mux) fn sizes(&self, cues: bool) -> Option<Summary> {
    self
      .sizes
      .map(|(plain, cue)| if cues { cue } else { plain })
  }
}

/// A table's reading: its entries stated and read, the bytes of the entry
/// being read, and what it folds into.
#[derive(Debug, Clone, Copy, Default)]
struct Table {
  /// Bytes of the current entry, or of the fields before the entries.
  entry: [u8; 8],
  have: usize,
  /// The entries the table states.
  stated: u64,
  /// The entries read.
  read: u64,
  /// `stsz`'s uniform size, or 0 of a list.
  uniform: u64,
  plain: Runs,
  cues: CueTables,
  /// A `dec3`'s fields, as they are read.
  dec3: Dec3Fields,
  /// An `esds`'s decoder-specific information, as it is read.
  dsi: DsiFields,
}

/// A `dec3`'s payload, read against its own fields (`mov_write_eac3_tag`,
/// 612–666): two bytes — the data rate and the independent substreams
/// less one — then each substream's three bytes, four where it names
/// dependent substreams (its third byte's bits 1 to 4), then, where the
/// complexity index is written, a byte of its flag, 1, and the index.
#[derive(Debug, Clone, Copy, Default)]
struct Dec3Fields {
  /// The substreams still to read, the current among them.
  left: u8,
  /// Where the current substream starts, and its bytes.
  start: u64,
  len: u64,
  /// The bytes the fields before the complexity index say, once all read.
  implied: Option<u64>,
  /// The first byte past them.
  tail: Option<u8>,
}

/// An `esds`'s decoder-specific information: its tag at payload offset 30,
/// its length in the four bytes after it, and its bytes, folded a word at a
/// time.
#[derive(Debug, Clone, Copy)]
struct DsiFields {
  tag: u8,
  len: u64,
  read: u64,
  word: u32,
  fold: u64,
}

impl Default for DsiFields {
  fn default() -> Self {
    Self {
      tag: 0,
      len: 0,
      read: 0,
      word: 0,
      fold: FOLD_SEED,
    }
  }
}

/// What the walk read of the sample entry being read.
#[derive(Debug, Clone, Copy, Default)]
struct EntrySeen {
  /// Its first record box, of [`RECORDS`] but `lhvC`: its FourCC and its
  /// payload's bytes.
  record: Option<([u8; 4], u64)>,
  /// Its `lhvC`'s payload's bytes.
  layers: Option<u64>,
  /// Its `dec3`'s payload's bytes, those its fields say, and whether it is
  /// as long as they say.
  dec3: Option<(u64, u64, bool)>,
  /// Its `esds`'s decoder-specific information: its length, the bytes of
  /// it read and their fold; `None` where its tag is not at offset 30.
  dsi: Option<(u64, u64, u64)>,
}

/// The MOV family's walk (see [the module](self)).
pub(in crate::mux) struct MovWalk {
  mode: MovMode,
  stack: [Frame; DEPTH],
  depth: usize,
  header: Header,
  fields: [Option<Field>; FIELDS],
  /// The leaf being parsed.
  table: Table,
  /// The `trak`s read, as many as the table holds; the count beside.
  traks: Box<[TrakSeen]>,
  traks_seen: usize,
  /// The open `trak`'s place in `traks`, and its entries' layout.
  trak: Option<usize>,
  class: Class,
  /// The descriptions the muxer handed, by the place of their `trak`: the
  /// table allocated at open, its slots filled by moves (R6).
  handed: Box<[Option<Handed>]>,
  /// The open `stsd`'s entries read, and what the walk read of the entry
  /// being read.
  entries: usize,
  entry: EntrySeen,
  /// The last top-level box, where an 8-byte `free` or `wide`: its offset.
  wide: Option<u64>,
  /// That box, where it lay right before the `mdat`: what the `mdat`'s
  /// 64-bit size overwrites.
  mdat_wide: Option<u64>,
  /// The `mdat`'s payload's offset, and the size its patch declared and
  /// the payload the sink took before it.
  mdat: Option<u64>,
  media_data: Option<(u64, u64)>,
  /// Whether the `moov` closed.
  moov: bool,
  /// AVIF's items, as `iloc` holds them: each extent's offset, as read or
  /// patched, and its length; and each item's `av1C` against its track's
  /// description, the first difference.
  items: [Option<(u64, u64)>; ITEMS],
  items_seen: usize,
  item_records: usize,
  item_found: Option<(usize, Found)>,
}

impl MovWalk {
  /// The walk of a file in `mode`, of `tracks` tracks at most, the
  /// chapter track among them: its table allocated here.
  pub(in crate::mux) fn new(mode: MovMode, tracks: usize) -> Self {
    Self {
      mode,
      stack: [EMPTY_FRAME; DEPTH],
      depth: 0,
      header: Header::default(),
      fields: [None; FIELDS],
      table: Table::default(),
      traks: vec![TrakSeen::default(); tracks].into_boxed_slice(),
      traks_seen: 0,
      trak: None,
      class: Class::Unknown,
      handed: (0..tracks).map(|_| None).collect(),
      entries: 0,
      entry: EntrySeen::default(),
      wide: None,
      mdat_wide: None,
      mdat: None,
      media_data: None,
      moov: false,
      items: [None; ITEMS],
      items_seen: 0,
      item_records: 0,
      item_found: None,
    }
  }

  /// Takes the descriptions of the `place`-th `trak` the file holds,
  /// handed before the trailer writes them; a place past the table's is
  /// none movenc writes.
  pub(in crate::mux) fn hand(&mut self, place: usize, handed: Handed) {
    if let Some(slot) = self.handed.get_mut(place) {
      *slot = Some(handed);
    }
  }

  /// The `trak`s read, in the order the file holds them.
  pub(in crate::mux) fn traks(&self) -> &[TrakSeen] {
    &self.traks[..self.traks_seen.min(self.traks.len())]
  }

  /// How many `trak`s the file holds.
  pub(in crate::mux) const fn traks_seen(&self) -> usize {
    self.traks_seen
  }

  /// The payload the `mdat`'s size declared, and the payload the sink
  /// took between its header and that size's patch.
  pub(in crate::mux) const fn media_data(&self) -> Option<(u64, u64)> {
    self.media_data
  }

  /// AVIF's items, as its `iloc` holds them — each its extent's offset,
  /// as read or patched, and its length — in order.
  pub(in crate::mux) fn items(&self) -> impl Iterator<Item = (u64, u64)> + '_ {
    self.items[..self.items_seen.min(ITEMS)]
      .iter()
      .flatten()
      .copied()
  }

  /// AVIF's first item whose `av1C` differs from its track's description,
  /// and what was found.
  pub(in crate::mux) const fn item_found(&self) -> Option<(usize, Found)> {
    self.item_found
  }

  /// The payload's offset in the file: the `mdat`'s first byte past its
  /// header.
  pub(in crate::mux) const fn payload_start(&self) -> Option<u64> {
    self.mdat
  }

  /// Why a file the walk ended at `frontier` is not whole, or `None`: a
  /// box open, a header part read, a field part written, or no `moov` —
  /// of AVIF, no `mdat`, which it writes last.
  pub(in crate::mux) fn unfinished(&self, frontier: u64) -> Option<Unfollowed> {
    if self.fields.iter().flatten().any(|field| field.filled > 0) {
      return Some(Unfollowed::new(frontier, Why::Unresolved));
    }
    let ended = if self.mode == MovMode::Avif {
      self.media_data.is_some()
    } else {
      self.moov
    };
    (self.depth > 0 || self.header.have > 0 || !ended)
      .then(|| Unfollowed::new(frontier, Why::Truncated))
  }

  /// `bytes`, appended at `at`.
  pub(in crate::mux) fn append(&mut self, at: u64, bytes: &[u8]) -> Result<(), Unfollowed> {
    // A field patched in part is whole before any byte after it: the
    // parts of one write reach the sink one after another.
    if self.fields.iter().flatten().any(|field| field.filled > 0) {
      return Err(Unfollowed::new(at, Why::Unresolved));
    }
    // `stsd`'s version is patched right after its size, before any later
    // byte; after one, it is not.
    self.drop_fields(|field| field.kind == FieldKind::Version && field.armed);
    let mut pos = at;
    let mut rest = bytes;
    while !rest.is_empty() {
      self.close_ended(pos)?;
      let taken = self.step(pos, rest)?;
      pos = pos.saturating_add(taken as u64);
      rest = &rest[taken..];
    }
    self.close_ended(pos)
  }

  /// The innermost open box, if any.
  fn top(&self) -> Option<&Frame> {
    self.depth.checked_sub(1).map(|index| &self.stack[index])
  }

  /// Closes every box whose known end is `pos`.
  fn close_ended(&mut self, pos: u64) -> Result<(), Unfollowed> {
    while let Some(top) = self.top() {
      match top.end {
        Some(end) if end == pos => self.close(pos, false)?,
        _ => return Ok(()),
      }
    }
    Ok(())
  }

  /// Takes what lies at `pos` of `bytes`, and answers how many it took.
  fn step(&mut self, pos: u64, bytes: &[u8]) -> Result<usize, Unfollowed> {
    let room = self
      .top()
      .and_then(|top| top.end)
      .map_or(usize::MAX, |end| {
        usize::try_from(end.saturating_sub(pos)).unwrap_or(usize::MAX)
      });
    let bytes = &bytes[..bytes.len().min(room)];
    if self.header.have > 0 {
      return self.header_bytes(pos, bytes);
    }
    let Some(top) = self.top().copied() else {
      return self.header_bytes(pos, bytes);
    };
    match top.role {
      Role::Boxes { prefix } if top.read < prefix => Ok(self.own_bytes(bytes, prefix - top.read)),
      Role::Boxes { .. } => self.header_bytes(pos, bytes),
      Role::Entry { fixed: None } => {
        // MOV's audio entry: its version, at payload offset 8, sets its
        // fixed part (1385–1403).
        let taken = self.own_bytes(bytes, 10u64.saturating_sub(top.read));
        let index = self.depth - 1;
        if self.stack[index].read == 10 {
          let fixed = match self.stack[index].version {
            [0, 0] => 28,
            [0, 1] => 44,
            [0, 2] => 64,
            _ => return Err(Unfollowed::new(pos, Why::Structure)),
          };
          self.stack[index].role = Role::Entry { fixed: Some(fixed) };
        }
        Ok(taken)
      }
      Role::Entry { fixed: Some(fixed) } if top.read < fixed => {
        Ok(self.own_bytes(bytes, fixed - top.read))
      }
      Role::Entry { .. } => self.header_bytes(pos, bytes),
      Role::RawEntry | Role::Opaque | Role::MediaData | Role::Record | Role::ItemRecord => {
        let index = self.depth - 1;
        self.stack[index].read = self.stack[index].read.saturating_add(bytes.len() as u64);
        Ok(bytes.len())
      }
      Role::Leaf(leaf) => {
        // Each byte at its own offset: AVIF's extent offset registers there.
        for (offset, &byte) in bytes.iter().enumerate() {
          self.leaf_byte(leaf, pos.saturating_add(offset as u64), byte)?;
        }
        Ok(bytes.len())
      }
    }
  }

  /// Takes at most `want` of `bytes` as the innermost box's own, keeping
  /// the bytes at payload offsets 8 and 9 — an audio entry's version — and
  /// answers how many it took.
  fn own_bytes(&mut self, bytes: &[u8], want: u64) -> usize {
    let frame = &mut self.stack[self.depth - 1];
    let taken = bytes.len().min(usize::try_from(want).unwrap_or(usize::MAX));
    for &byte in &bytes[..taken] {
      if (8..10).contains(&frame.read) {
        frame.version[(frame.read - 8) as usize] = byte;
      }
      frame.read += 1;
    }
    taken
  }

  /// Reads a box header's bytes at `pos`, opening the box once whole.
  fn header_bytes(&mut self, pos: u64, bytes: &[u8]) -> Result<usize, Unfollowed> {
    if self.header.have == 0 {
      self.header.at = pos;
      // A header must fit what is left of its parent.
      if let Some(end) = self.top().and_then(|top| top.end)
        && end.saturating_sub(pos) < 8
      {
        return Err(Unfollowed::new(pos, Why::Structure));
      }
    }
    let need = if self.header.have >= 4 && self.header.bytes[0..4] == [0, 0, 0, 1] {
      16
    } else {
      8
    };
    let taken = bytes.len().min(need - self.header.have);
    self.header.bytes[self.header.have..self.header.have + taken].copy_from_slice(&bytes[..taken]);
    self.header.have += taken;
    if self.header.have == 8 && self.header.bytes[0..4] == [0, 0, 0, 1] {
      return Ok(taken);
    }
    if self.header.have == need {
      self.open()?;
    }
    Ok(taken)
  }

  /// Opens the box whose header is whole.
  fn open(&mut self) -> Result<(), Unfollowed> {
    let header = core::mem::take(&mut self.header);
    let size = u64::from(u32::from_be_bytes([
      header.bytes[0],
      header.bytes[1],
      header.bytes[2],
      header.bytes[3],
    ]));
    let fourcc = [
      header.bytes[4],
      header.bytes[5],
      header.bytes[6],
      header.bytes[7],
    ];
    let (len, size) = if size == 1 {
      (
        16,
        u64::from_be_bytes([
          header.bytes[8],
          header.bytes[9],
          header.bytes[10],
          header.bytes[11],
          header.bytes[12],
          header.bytes[13],
          header.bytes[14],
          header.bytes[15],
        ]),
      )
    } else {
      (8, size)
    };
    let at = header.at;
    let structure = || Unfollowed::new(at, Why::Structure);
    // movenc writes 0 as a placeholder alone, never as a size.
    let end = match size {
      0 => None,
      size if size < len => return Err(structure()),
      size => Some(at.checked_add(size).ok_or_else(structure)?),
    };
    if let (Some(end), Some(parent)) = (end, self.top().and_then(|top| top.end))
      && end > parent
    {
      return Err(structure());
    }
    if self.depth == DEPTH {
      return Err(structure());
    }
    let role = self.role(fourcc);
    match (role, &fourcc) {
      (Role::Boxes { .. }, b"trak") => {
        self.trak = Some(self.traks_seen);
        self.class = Class::Unknown;
      }
      (Role::Boxes { .. }, b"stsd") => self.entries = 0,
      (Role::Leaf(_), _) => {
        self.table = Table::default();
      }
      _ => {}
    }
    if matches!(role, Role::Entry { .. } | Role::RawEntry) {
      self.entry = EntrySeen::default();
    }
    match &fourcc {
      b"stsd" if self.parent_is(b"stbl") => {
        self.register(at.saturating_add(8), 1, FieldKind::Version)?
      }
      b"stsc" | b"stss" | b"stps" if self.parent_is(b"stbl") => {
        self.register(at.saturating_add(12), 4, FieldKind::Count)?;
      }
      _ => {}
    }
    if self.depth == 0 {
      if role == Role::MediaData {
        self.mdat = Some(at.saturating_add(len));
        self.mdat_wide = self.wide.filter(|wide| wide.checked_add(8) == Some(at));
      }
      self.wide = (matches!(&fourcc, b"free" | b"wide") && end == at.checked_add(8)).then_some(at);
    }
    self.stack[self.depth] = Frame {
      fourcc,
      at,
      end,
      role,
      read: 0,
      version: [0; 2],
    };
    self.depth += 1;
    // A box of no payload closes where it opened.
    if end == at.checked_add(len) {
      self.close(at.saturating_add(len), false)?;
    }
    Ok(())
  }

  /// Whether the innermost open box is a `fourcc`.
  fn parent_is(&self, fourcc: &[u8; 4]) -> bool {
    self.top().is_some_and(|top| &top.fourcc == fourcc)
  }

  /// Whether the box at `index` of the stack lies in a `fourcc`.
  fn parent_of(&self, index: usize, fourcc: &[u8; 4]) -> bool {
    index
      .checked_sub(1)
      .is_some_and(|parent| &self.stack[parent].fourcc == fourcc)
  }

  /// Keeps the open `trak`'s first sample description read otherwise than
  /// handed.
  fn noted(&mut self, index: usize, described: Described, found: Found) {
    if let Some(trak) = self.trak.and_then(|trak| self.traks.get_mut(trak)) {
      trak
        .descriptions
        .get_or_insert((u32::try_from(index).unwrap_or(u32::MAX), described, found));
    }
  }

  /// What the walk does with a box `fourcc`, where it opens.
  fn role(&self, fourcc: [u8; 4]) -> Role {
    let Some(parent) = self.top() else {
      return match &fourcc {
        b"moov" => Role::Boxes { prefix: 0 },
        b"mdat" => Role::MediaData,
        b"meta" if self.mode == MovMode::Avif => Role::Boxes { prefix: 4 },
        _ => Role::Opaque,
      };
    };
    match (&parent.fourcc, &fourcc) {
      (b"moov", b"trak") | (b"trak", b"mdia") | (b"mdia", b"minf") | (b"minf", b"stbl") => {
        Role::Boxes { prefix: 0 }
      }
      (b"trak", b"tkhd") => Role::Leaf(Leaf::Tkhd),
      (b"mdia", b"hdlr") => Role::Leaf(Leaf::Hdlr),
      (b"stbl", b"stsd") => Role::Boxes { prefix: 8 },
      (b"stbl", b"stts") => Role::Leaf(Leaf::Stts),
      (b"stbl", b"stsz") => Role::Leaf(Leaf::Stsz),
      (b"stbl", b"stco") if self.mode == MovMode::Avif => Role::Leaf(Leaf::Stco),
      (b"stbl", b"co64") if self.mode == MovMode::Avif => Role::Leaf(Leaf::Co64),
      (b"meta", b"iloc") if self.depth == 1 => Role::Leaf(Leaf::Iloc),
      (b"meta", b"iprp") | (b"iprp", b"ipco") if self.mode == MovMode::Avif => {
        Role::Boxes { prefix: 0 }
      }
      (b"ipco", b"av1C") => Role::ItemRecord,
      (b"stsd", entry) => match self.class {
        Class::Video => Role::Entry { fixed: Some(78) },
        Class::Audio if self.mode == MovMode::Mov => Role::Entry { fixed: None },
        Class::Audio => Role::Entry { fixed: Some(28) },
        Class::Other | Class::Unknown if entry == b"mp4s" => Role::Entry { fixed: Some(8) },
        Class::Other | Class::Unknown => Role::RawEntry,
      },
      (_, b"wave" | b"sv3d" | b"vexu") if matches!(parent.role, Role::Entry { .. }) => {
        Role::Boxes { prefix: 0 }
      }
      (_, record) if matches!(parent.role, Role::Entry { .. }) && RECORDS.contains(record) => {
        Role::Record
      }
      (b"mp4s", b"esds") if matches!(parent.role, Role::Entry { .. }) => Role::Leaf(Leaf::Esds),
      (b"wave", b"dec3")
        if self
          .depth
          .checked_sub(2)
          .is_some_and(|entry| matches!(self.stack[entry].role, Role::Entry { .. })) =>
      {
        Role::Leaf(Leaf::Dec3)
      }
      _ => Role::Opaque,
    }
  }

  /// Registers an optional field of `len` bytes at `at`.
  fn register(&mut self, at: u64, len: usize, kind: FieldKind) -> Result<(), Unfollowed> {
    let slot = self
      .fields
      .iter_mut()
      .find(|slot| slot.is_none())
      .ok_or(Unfollowed::new(at, Why::Structure))?;
    *slot = Some(Field {
      at,
      len,
      filled: 0,
      bytes: [0; 16],
      kind,
      armed: kind != FieldKind::Version,
    });
    Ok(())
  }

  /// Drops every registered field `which` names.
  fn drop_fields(&mut self, which: impl Fn(&Field) -> bool) {
    for slot in &mut self.fields {
      if slot.is_some_and(|field| which(&field)) {
        *slot = None;
      }
    }
  }

  /// One byte of a parsed leaf, at `pos`.
  fn leaf_byte(&mut self, leaf: Leaf, pos: u64, byte: u8) -> Result<(), Unfollowed> {
    let index = self.depth - 1;
    let read = self.stack[index].read;
    self.stack[index].read += 1;
    let table = &mut self.table;
    match leaf {
      Leaf::Tkhd => {
        // The version, then the id at 12 of version 0 and 20 of 1.
        if read == 0 {
          table.entry[7] = byte;
        }
        let at = if table.entry[7] == 1 { 20 } else { 12 };
        if (at..at + 4).contains(&read) {
          table.entry[(read - at) as usize] = byte;
          if read == at + 3
            && let Some(trak) = self.trak.and_then(|trak| self.traks.get_mut(trak))
          {
            trak.id = Some(u32::from_be_bytes([
              table.entry[0],
              table.entry[1],
              table.entry[2],
              table.entry[3],
            ]));
          }
        }
      }
      Leaf::Hdlr => {
        if (8..12).contains(&read) {
          table.entry[(read - 8) as usize] = byte;
          if read == 11 {
            self.class = match &table.entry[0..4] {
              // AVIF's image and its alpha plane (`mov_write_hdlr_tag`,
              // `movenc.c` 3579–3580): visual sample entries.
              b"vide" | b"pict" | b"auxv" => Class::Video,
              b"soun" => Class::Audio,
              _ => Class::Other,
            };
          }
        }
      }
      Leaf::Iloc => {
        // Version 0 of 4-byte offsets and lengths and no base offset, its
        // items' count, then each item's id, data reference and one extent
        // — its offset, then its length (3660–3681). Read through
        // `self.table`, so that the field registers as its byte is read.
        match read {
          0..=3 => {}
          4 | 5 => {
            if byte != [0x44, 0][(read - 4) as usize] {
              return Err(Unfollowed::new(pos, Why::Structure));
            }
          }
          6 | 7 => self.table.stated = (self.table.stated << 8) | u64::from(byte),
          _ => {
            let (item, at) = ((read - 8) / 14, (read - 8) % 14);
            let item = usize::try_from(item).unwrap_or(usize::MAX);
            let items = usize::try_from(self.table.stated).unwrap_or(usize::MAX);
            if item >= items.min(ITEMS) {
              return Err(Unfollowed::new(pos, Why::Structure));
            }
            if at >= 6 {
              self.table.entry[((at - 6) % 4) as usize] = byte;
            }
            if at == 6 {
              // The extent offset's field, patched once the payload is
              // placed: registered as its first byte is read.
              self.register(pos, 4, FieldKind::Extent(item))?;
            }
            if at == 9 || at == 13 {
              let entry = self.table.entry;
              let value = u64::from(u32::from_be_bytes([entry[0], entry[1], entry[2], entry[3]]));
              let slot = self.items[item].get_or_insert((0, 0));
              if at == 9 {
                slot.0 = value;
              } else {
                slot.1 = value;
                self.items_seen = item + 1;
              }
            }
          }
        }
      }
      Leaf::Stco | Leaf::Co64 => {
        // Version and flags, the count, then the first entry, of 4 bytes
        // or 8.
        let width = if leaf == Leaf::Stco { 4 } else { 8 };
        if (8..8 + width).contains(&read) {
          self.table.entry[(read - 8) as usize] = byte;
          if read + 1 == 8 + width {
            let mut word = [0; 8];
            word[(8 - width) as usize..].copy_from_slice(&self.table.entry[..width as usize]);
            if let Some(trak) = self.trak.and_then(|trak| self.traks.get_mut(trak)) {
              trak.first_chunk = Some(u64::from_be_bytes(word));
            }
          }
        }
      }
      Leaf::Esds => {
        let dsi = &mut table.dsi;
        match read {
          DSI_TAG_AT => dsi.tag = byte,
          // `put_descr`'s length: four bytes of seven bits (694–701).
          31..=34 => dsi.len = (dsi.len << 7) | u64::from(byte & 0x7F),
          _ if dsi.tag == 5 && read >= 35 && read - 35 < dsi.len => {
            dsi.word = (dsi.word << 8) | u32::from(byte);
            dsi.read += 1;
            if dsi.read.is_multiple_of(4) {
              dsi.fold = fold(dsi.fold, u64::from(dsi.word));
            }
          }
          _ => {}
        }
      }
      Leaf::Dec3 => {
        let fields = &mut table.dec3;
        if read == 1 {
          // The independent substreams less one, in the low 3 bits.
          (fields.left, fields.start, fields.len) = ((byte & 7) + 1, 2, 3);
        } else if read >= 2 && fields.left > 0 {
          if read == fields.start + 2 && (byte >> 1) & 0xF != 0 {
            // Dependent substreams named: their locations, 9 bits.
            fields.len = 4;
          }
          if read + 1 == fields.start + fields.len {
            fields.left -= 1;
            fields.start += fields.len;
            fields.len = 3;
            if fields.left == 0 {
              fields.implied = Some(fields.start);
            }
          }
        } else if fields.implied == Some(read) {
          fields.tail = Some(byte);
        }
      }
      Leaf::Stts | Leaf::Stsz => {
        // Version and flags; `stsz`'s uniform size; the count; then the
        // entries.
        let fields = if leaf == Leaf::Stts { 8 } else { 12 };
        if read < fields {
          if read >= 4 {
            table.entry[(read - 4) as usize] = byte;
          }
          if read + 1 == fields {
            let word = |at: usize| {
              u64::from(u32::from_be_bytes([
                table.entry[at],
                table.entry[at + 1],
                table.entry[at + 2],
                table.entry[at + 3],
              ]))
            };
            if leaf == Leaf::Stts {
              table.stated = word(0);
            } else {
              table.uniform = word(0);
              table.stated = word(4);
              if table.uniform != 0 {
                table.plain.push(table.uniform, table.stated);
                table.cues.size(table.uniform, table.stated);
              }
            }
          }
          return Ok(());
        }
        let width = if leaf == Leaf::Stts { 8 } else { 4 };
        if (leaf == Leaf::Stsz && table.uniform != 0) || table.read == table.stated {
          // Past the entries the table states: no writer's.
          return Err(Unfollowed::new(pos, Why::Structure));
        }
        table.entry[table.have] = byte;
        table.have += 1;
        if table.have == width {
          table.have = 0;
          table.read += 1;
          let word = |at: usize| {
            u64::from(u32::from_be_bytes([
              table.entry[at],
              table.entry[at + 1],
              table.entry[at + 2],
              table.entry[at + 3],
            ]))
          };
          if leaf == Leaf::Stts {
            let (count, duration) = (word(0), word(4));
            table.plain.push(duration, count);
            table.cues.time(count, duration);
          } else {
            let size = word(0);
            table.plain.push(size, 1);
            table.cues.size(size, 1);
          }
        }
      }
    }
    Ok(())
  }

  /// Closes the innermost box at `pos` — by its patch, where `patched`.
  fn close(&mut self, pos: u64, patched: bool) -> Result<(), Unfollowed> {
    let Some(index) = self.depth.checked_sub(1) else {
      return Err(Unfollowed::new(pos, Why::Structure));
    };
    let frame = self.stack[index];
    let structure = Unfollowed::new(frame.at, Why::Structure);
    match frame.role {
      // A box's own bytes before its children, whole.
      Role::Boxes { prefix } if frame.read < prefix => return Err(structure),
      Role::Entry { fixed: None } => return Err(structure),
      Role::Entry { fixed: Some(fixed) } if frame.read < fixed => return Err(structure),
      Role::Leaf(leaf @ (Leaf::Stts | Leaf::Stsz)) => {
        let table = &self.table;
        let fields = if leaf == Leaf::Stts { 8 } else { 12 };
        let whole = frame.read >= fields
          && table.have == 0
          && (table.read == table.stated || leaf == Leaf::Stsz && table.uniform != 0);
        if !whole {
          return Err(structure);
        }
        let summaries = match leaf {
          Leaf::Stts => (table.plain.summary(), table.cues.times()),
          _ => (table.plain.summary(), table.cues.sizes()),
        };
        if let Some(trak) = self.trak.and_then(|trak| self.traks.get_mut(trak)) {
          if leaf == Leaf::Stts {
            trak.times = Some(summaries);
          } else {
            trak.sizes = Some(summaries);
          }
        }
      }
      // MP4's `mdat` is closed by its patch alone, which records it; AVIF's
      // is written of its size, read as it closes.
      Role::MediaData => {
        if let (Some(end), Some(start)) = (frame.end, self.mdat) {
          self.media_data = Some((end.saturating_sub(start), frame.read));
        }
      }
      Role::ItemRecord => {
        let item = self.item_records;
        self.item_records += 1;
        let found = self
          .handed
          .get(item)
          .and_then(Option::as_ref)
          .and_then(|handed| {
            let seen = EntrySeen {
              record: Some((frame.fourcc, frame.read)),
              ..EntrySeen::default()
            };
            judged(handed.entry(0), &seen)
          });
        if let Some((_, found)) = found {
          self.item_found.get_or_insert((item, found));
        }
      }
      Role::Record if &frame.fourcc == b"lhvC" => self.entry.layers = Some(frame.read),
      Role::Record => {
        self.entry.record.get_or_insert((frame.fourcc, frame.read));
      }
      Role::Leaf(Leaf::Esds) => {
        let dsi = self.table.dsi;
        self.entry.dsi =
          (dsi.tag == 5 && frame.read > DSI_TAG_AT + 4).then_some((dsi.len, dsi.read, dsi.fold));
      }
      Role::Leaf(Leaf::Dec3) => {
        let fields = self.table.dec3;
        let written = frame.read;
        // What its fields say, at least, where they run past its end: its
        // two bytes and a substream's three where it ends before them.
        let implied = fields.implied.unwrap_or(match fields.left {
          0 => 5,
          left => fields.start + fields.len + 3 * u64::from(left - 1),
        });
        let whole = fields.implied.is_some()
          && (written == implied || (written == implied + 2 && fields.tail == Some(1)));
        self.entry.dec3 = Some((written, implied, whole));
      }
      _ => {}
    }
    // A sample entry read whole: judged against its description as handed.
    if matches!(frame.role, Role::Entry { .. } | Role::RawEntry) && self.parent_of(index, b"stsd") {
      let index = self.entries;
      self.entries += 1;
      let found = self
        .trak
        .and_then(|trak| self.handed.get(trak))
        .and_then(Option::as_ref)
        .filter(|handed| index < handed.count())
        .and_then(|handed| judged(handed.entry(index), &self.entry));
      if let Some((described, found)) = found {
        self.noted(index, described, found);
      }
    }
    // The descriptions' count, as the `stsd` closes.
    if &frame.fourcc == b"stsd" && matches!(frame.role, Role::Boxes { .. }) {
      let entries = self.entries;
      if let Some(count) = self
        .trak
        .and_then(|trak| self.handed.get(trak))
        .and_then(Option::as_ref)
        .map(Handed::count)
        .filter(|&count| count != entries)
      {
        self.noted(
          entries.min(count),
          Described::Configuration,
          Found::Count(Counts::new(entries as u64, count as u64)),
        );
      }
    }
    match &frame.fourcc {
      b"trak" if self.depth == 2 => {
        self.traks_seen += 1;
        self.trak = None;
      }
      b"moov" if self.depth == 1 => self.moov = true,
      // Its version's patch follows its size's alone; a size read final was
      // patched in the buffer, the version with it.
      b"stsd" if patched => {
        for field in self.fields.iter_mut().flatten() {
          if field.kind == FieldKind::Version {
            field.armed = true;
          }
        }
      }
      b"stsd" => self.drop_fields(|field| field.kind == FieldKind::Version),
      b"stsc" | b"stss" | b"stps" => self.drop_fields(|field| field.kind == FieldKind::Count),
      _ => {}
    }
    self.depth -= 1;
    Ok(())
  }

  /// `bytes`, written at `at` below `frontier`: each byte a field's the
  /// walk admits there, or the walk stops.
  pub(in crate::mux) fn patch(
    &mut self,
    frontier: u64,
    at: u64,
    bytes: &[u8],
  ) -> Result<(), Unfollowed> {
    let mut at = at;
    let mut rest = bytes;
    while !rest.is_empty() {
      let slot = self.field_for(frontier, at)?;
      let Some(field) = self.fields[slot].as_mut() else {
        return Err(Unfollowed::new(at, Why::Rewrite));
      };
      let taken = rest.len().min(field.len - field.filled);
      field.bytes[field.filled..field.filled + taken].copy_from_slice(&rest[..taken]);
      field.filled += taken;
      at = at.saturating_add(taken as u64);
      rest = &rest[taken..];
      if field.filled == field.len {
        let field = *field;
        self.fields[slot] = None;
        self.resolve(frontier, field)?;
      }
    }
    Ok(())
  }

  /// The slot of the field a patch at `at` continues or starts.
  fn field_for(&mut self, frontier: u64, at: u64) -> Result<usize, Unfollowed> {
    // A field being filled, continued where it stands; a registered one
    // started at its first byte.
    if let Some(slot) = self.fields.iter().position(|slot| {
      slot.is_some_and(|field| field.armed && at == field.at.saturating_add(field.filled as u64))
    }) {
      return Ok(slot);
    }
    let rewrite = Unfollowed::new(at, Why::Rewrite);
    let top = self.top().copied().ok_or(rewrite)?;
    let kind = if top.end.is_none() && at == top.at {
      if top.role == Role::MediaData {
        FieldKind::Mdat32
      } else {
        FieldKind::Size
      }
    } else if top.role == Role::MediaData && top.end.is_none() && self.mdat_wide == Some(at) {
      FieldKind::Mdat64
    } else if top.role == Role::RawEntry && top.end.is_none() && at == frontier.saturating_sub(20) {
      FieldKind::Btrt
    } else if self.stack[..self.depth]
      .iter()
      .any(|frame| frame.end.is_none() && frame.at == at)
    {
      // An open box's size, but not the innermost's: patches close boxes
      // innermost first (`update_size`, 155–163).
      return Err(Unfollowed::new(at, Why::Structure));
    } else {
      return Err(rewrite);
    };
    let len = if kind == FieldKind::Mdat64 { 16 } else { 4 };
    self.register(at, len, kind)?;
    self
      .fields
      .iter()
      .position(|slot| slot.is_some_and(|field| field.at == at && field.filled == 0))
      .ok_or(rewrite)
  }

  /// A field whole: judged, and the box it closes closed.
  fn resolve(&mut self, frontier: u64, field: Field) -> Result<(), Unfollowed> {
    let word = u64::from(u32::from_be_bytes([
      field.bytes[0],
      field.bytes[1],
      field.bytes[2],
      field.bytes[3],
    ]));
    let rewrite = Unfollowed::new(field.at, Why::Rewrite);
    match field.kind {
      FieldKind::Size => {
        // The innermost box, ended at the frontier, its content whole: no
        // child's header part read.
        if word != frontier.saturating_sub(field.at) || self.header.have > 0 {
          return Err(Unfollowed::new(field.at, Why::Structure));
        }
        self.close(frontier, true)
      }
      FieldKind::Mdat32 | FieldKind::Mdat64 => {
        let declared = if field.kind == FieldKind::Mdat32 {
          word.saturating_sub(8)
        } else {
          if field.bytes[0..8] != [0, 0, 0, 1, b'm', b'd', b'a', b't'] {
            return Err(rewrite);
          }
          u64::from_be_bytes([
            field.bytes[8],
            field.bytes[9],
            field.bytes[10],
            field.bytes[11],
            field.bytes[12],
            field.bytes[13],
            field.bytes[14],
            field.bytes[15],
          ])
          .saturating_sub(16)
        };
        let taken = frontier.saturating_sub(self.mdat.unwrap_or(frontier));
        self.media_data = Some((declared, taken));
        self.close(frontier, true)
      }
      FieldKind::Count => Ok(()),
      FieldKind::Version => {
        if field.bytes[0] > 1 {
          return Err(rewrite);
        }
        Ok(())
      }
      FieldKind::Btrt => {
        if word != 20 {
          return Err(rewrite);
        }
        Ok(())
      }
      FieldKind::Extent(item) => {
        if let Some(slot) = self.items.get_mut(item).and_then(Option::as_mut) {
          slot.0 = word;
        }
        Ok(())
      }
    }
  }
}

/// The first way `seen`, a sample entry as read, differs from `expected`,
/// what the muxer's description of it says ([`EntryExpected`]): its record
/// box left out, written of another length — no longer than its header, of
/// the writers whose length is not mirrored — or, of `dec3`, of another
/// length than its fields say; `lhvC` beside it; DVD subtitles' palette
/// left out, of another length, or of other colours.
fn judged(expected: EntryExpected, seen: &EntrySeen) -> Option<(Described, Found)> {
  let length = |written: u64, len: u64| Found::Length(Counts::new(written, len));
  let record = expected.record.and_then(|record| match record {
    RecordExpected::Exact(fourcc, len) => match seen.record {
      Some((read, written)) if read == fourcc => (written != len).then(|| length(written, len)),
      _ => Some(Found::Omitted),
    },
    RecordExpected::Longer(fourcc, len) => match seen.record {
      Some((read, written)) if read == fourcc && written > len => None,
      _ => Some(Found::Omitted),
    },
    RecordExpected::Dec3 => match seen.dec3 {
      None => Some(Found::Omitted),
      Some((written, implied, whole)) => (!whole).then(|| length(written, implied)),
    },
  });
  let layers = expected
    .layers
    .and_then(|layers| match (seen.layers, layers) {
      (None, None) => None,
      (Some(written), Some(len)) => (written != len).then(|| length(written, len)),
      (None, Some(_)) => Some(Found::Omitted),
      (Some(_), None) => Some(Found::Count(Counts::new(1, 0))),
    });
  let palette = expected.palette.and_then(|colours| match seen.dsi {
    None => Some(Found::Omitted),
    Some((len, _, _)) if len != PALETTE_BYTES => Some(length(len, PALETTE_BYTES)),
    Some((_, read, _)) if read != PALETTE_BYTES => Some(length(read, PALETTE_BYTES)),
    Some((_, _, folded)) => (folded != colours).then_some(Found::Content),
  });
  record
    .or(layers)
    .map(|found| (Described::Configuration, found))
    .or(palette.map(|found| (Described::Palette, found)))
}
