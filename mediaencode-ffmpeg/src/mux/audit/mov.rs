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
//! **The fields patched beside the boxes' sizes:** `stsc`'s, `stss`'s and
//! `stps`'s entry counts at box offset 12, rewritten before their boxes'
//! sizes (283–286, 308–311); `stsd`'s version at box offset 8, rewritten
//! after its size (`update_size_and_version`, 165–175); the `mdat`'s size,
//! of 32 bits at its header, or of 64 over the `free` or `wide` box before
//! it, `00000001 'mdat' u64` (`mov_write_mdat_size`, 8991–9012), accepted
//! at any value and judged by the verdict. Each is optional — a patch made
//! while its target was buffered never reaches the sink on its own — and
//! each may reach it in parts, where the sink takes a write in parts.

use super::{CueTables, Runs, Summary, Unfollowed, Why};
use crate::mux::MovMode;

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
}

impl TrakSeen {
  /// The track header's id.
  pub(in crate::mux) const fn id(&self) -> Option<u32> {
    self.id
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
      wide: None,
      mdat_wide: None,
      mdat: None,
      media_data: None,
      moov: false,
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

  /// Why a file the walk ended at `frontier` is not whole, or `None`: a
  /// box open, a header part read, a field part written, or no `moov`.
  pub(in crate::mux) fn unfinished(&self, frontier: u64) -> Option<Unfollowed> {
    if self.fields.iter().flatten().any(|field| field.filled > 0) {
      return Some(Unfollowed::new(frontier, Why::Unresolved));
    }
    (self.depth > 0 || self.header.have > 0 || !self.moov)
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
      Role::RawEntry | Role::Opaque | Role::MediaData => {
        let index = self.depth - 1;
        self.stack[index].read = self.stack[index].read.saturating_add(bytes.len() as u64);
        Ok(bytes.len())
      }
      Role::Leaf(leaf) => {
        for &byte in bytes {
          self.leaf_byte(leaf, pos, byte)?;
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
      (Role::Leaf(_), _) => {
        self.table = Table::default();
      }
      _ => {}
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

  /// What the walk does with a box `fourcc`, where it opens.
  fn role(&self, fourcc: [u8; 4]) -> Role {
    let Some(parent) = self.top() else {
      return match &fourcc {
        b"moov" => Role::Boxes { prefix: 0 },
        b"mdat" => Role::MediaData,
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
              b"vide" => Class::Video,
              b"soun" => Class::Audio,
              _ => Class::Other,
            };
          }
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
      Role::MediaData => {
        // Closed by its patch alone, which records it.
      }
      _ => {}
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
    }
  }
}
