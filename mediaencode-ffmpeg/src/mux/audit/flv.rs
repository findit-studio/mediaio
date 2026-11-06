//! FLV's walk: its file header, VP6A's pre-tags, then its tags — each its
//! header, its data and its PreviousTagSize — as the sink takes them
//! (`flvenc.c` in FFmpeg 9.0.1).
//!
//! **What it follows.** The file header — `FLV`, version 1, its flags, its
//! size of 9 — and the first PreviousTagSize, 0 (`flv_write_header`,
//! 1083–1095); then, for each stream whose codec tag libavformat's init
//! leaves at 5 — VP6A's — an empty audio tag of a final size of 0 and a
//! PreviousTagSize of 11 (1096–1104), exactly as many as the muxer states
//! at open (R11); then tags: an 11-byte header — its type, 8 audio, 9
//! video, 18 script; its data size in 24 bits; its time; its stream id,
//! read and not judged — its data, and its PreviousTagSize, which must read
//! the data size and 11.
//!
//! **Sizes FLV rewrites.** A tag's data size reaches the sink final where
//! its header was still buffered when FLV rewrote it, and as first written
//! where the header was flushed before: the rewrite is then a write of its
//! own after the tag's last data byte and before its PreviousTagSize (the
//! patch lemma), which must close the tag at the frontier. FLV writes such
//! a size as:
//! - **0**, a placeholder: each codec header (`flv_write_codec_header`, 825,
//!   903–907) and `onMetaData` (`write_metadata`, 310, 493–499) — each
//!   opening on its writer's first byte: an audio codec header's 0xAF or
//!   0x9_, a video one's 0x17, 0x19 or 0x9_, `onMetaData`'s 0x02 (141–148,
//!   318, 825–883); a tag of size 0 opening on another, R11's pre-tag the
//!   walk was not told of among them, is no writer's;
//! - **5, of a video tag whose first data byte is 0xD4** —
//!   `FLV_IS_EX_HEADER | FLV_FRAME_VIDEO_INFO_CMD | PacketTypeMetadata` —
//!   HDR metadata's (`flv_write_metadata_packet`, 557–578, 666–670): its
//!   colorInfo object always written, its true size is never 5 (R10);
//! - **a script tag's provisional size**, any: a TEXT cue's tag holds its
//!   payload's size and one, then the size of the AMF it writes (1346–1386);
//!   a data stream's its payload's and one, then its payload's. A script
//!   tag's size is taken as final only where the 4 bytes after its data
//!   read it and 11; until 4 bytes or the rewrite arrive, the walk holds
//!   them pending (the design gate's finding 6).
//!
//! **The window.** `onMetaData`'s data — the first script tag, written at
//! the header — FLV rewrites while it is open (its array's count, 490–491)
//! and after it closes (the trailer's duration and file size, 1190–1206):
//! any write inside it is admitted, its content not judged. Any other
//! write below the frontier is a rewrite of no field.
//!
//! **What it reads of the audio and video tags:** each tag's track — its
//! type, and the multitrack header's index, 0 where it has none
//! (1395–1426) — and its packet: a sequence start, whose record — its data
//! less the header the muxer states of the track at open — is folded per
//! track, VVC's by whether it is empty alone; coded frames, whose data size
//! is folded per track; anything else — metadata, the multichannel
//! configuration, the end of a sequence — walked and not judged.

use super::{Runs, Summary, Unfollowed, Why};

/// The file header and the first PreviousTagSize (1089–1094); its flags,
/// at 4, any.
const HEADER: [u8; 13] = [b'F', b'L', b'V', 1, 0, 0, 0, 0, 9, 0, 0, 0, 0];

/// VP6A's pre-tag: an audio tag of a final data size of 0, its time 0, a
/// zero word, then a PreviousTagSize of 11 (1096–1104; R11).
const PRE_TAG: [u8; 15] = [8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 11];

/// A tag's header: its type, data size, time, extended time and stream id.
const TAG_HEADER: u64 = 11;

/// HDR metadata's first data byte (R10).
const METADATA: u8 = 0xD4;

/// The bytes of a tag's data the walk reads to name its track and packet:
/// a flags byte, the multitrack byte, a FourCC and the track's index.
const CLASS: usize = 7;

/// A tag's type.
const AUDIO: u8 = 8;
const VIDEO: u8 = 9;
const SCRIPT: u8 = 18;

/// A track as the muxer states it at open: its tags' type and its index
/// among its kind's — the multitrack header's — the bytes of its sequence
/// header before its record, and whether its records are folded by their
/// emptiness alone, VVC's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::mux) struct FlvTrack {
  pub(in crate::mux) kind: u8,
  pub(in crate::mux) index: u8,
  pub(in crate::mux) header: u64,
  pub(in crate::mux) emptiness: bool,
}

/// What the walk read of one track.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(in crate::mux) struct FlvSeen {
  /// Its sequence starts' records, each its length — VVC's, 1 of a record
  /// and 0 of none ([`Runs`]).
  sequences: Runs,
  /// Its coded frames' tags, each its data size ([`Runs`]).
  frames: Runs,
}

impl FlvSeen {
  /// Its sequence starts' records.
  pub(in crate::mux) fn sequences(&self) -> Summary {
    self.sequences.summary()
  }

  /// Its coded frames' data sizes.
  pub(in crate::mux) fn frames(&self) -> Summary {
    self.frames.summary()
  }
}

/// How a tag's data size stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Size {
  /// Final: the data ends there.
  Final(u64),
  /// A placeholder, rewritten at the tag's end.
  Placeholder,
  /// Read as 5 of a video tag: a placeholder where its first data byte is
  /// HDR metadata's, else final (R10).
  Five,
  /// A script tag's provisional size: final where the 4 bytes after it
  /// read it and 11.
  Provisional(u64),
}

/// Where the walk stands within the tag being read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
  /// Its header.
  Header,
  /// Its data.
  Data,
  /// The 4 bytes after a script tag's provisional end.
  Lookahead,
  /// Its PreviousTagSize.
  Trailer,
}

/// The tag being read.
#[derive(Debug, Clone, Copy)]
struct Tag {
  at: u64,
  kind: u8,
  size: Size,
  phase: Phase,
  /// Its data's bytes read.
  read: u64,
  /// Its first data bytes, for its track and packet.
  class: [u8; CLASS],
  /// The bytes of the header, lookahead or trailer being read.
  bytes: [u8; 11],
  have: usize,
  /// Its size's rewrite, in parts.
  patch: [u8; 3],
  patched: usize,
}

impl Tag {
  const fn new(at: u64) -> Self {
    Self {
      at,
      kind: 0,
      size: Size::Final(0),
      phase: Phase::Header,
      read: 0,
      class: [0; CLASS],
      bytes: [0; 11],
      have: 0,
      patch: [0; 3],
      patched: 0,
    }
  }

  /// Whether its size is one FLV rewrites, its data open.
  fn rewritten(&self) -> bool {
    matches!(self.phase, Phase::Data | Phase::Lookahead)
      && match self.size {
        Size::Final(_) => false,
        Size::Placeholder | Size::Provisional(_) => true,
        Size::Five => self.read > 0 && self.class[0] == METADATA,
      }
  }
}

/// FLV's walk (see [the module](self)).
pub(in crate::mux) struct FlvWalk {
  /// The file header's bytes read.
  head: usize,
  /// VP6A's pre-tags still to read, and the bytes of the one being read.
  pre_tags: usize,
  pre_read: usize,
  /// The tag being read, past the header.
  tag: Tag,
  /// `onMetaData`'s data: its first byte, and its end once known.
  window: Option<(u64, Option<u64>)>,
  /// The tracks the muxer states, and what was read of each.
  tracks: Box<[(FlvTrack, FlvSeen)]>,
}

impl FlvWalk {
  /// The walk of a file of `tracks` and of `pre_tags` VP6A pre-tags (R11):
  /// its table allocated here, at open.
  pub(in crate::mux) fn new(tracks: &[FlvTrack], pre_tags: usize) -> Self {
    Self {
      head: 0,
      pre_tags,
      pre_read: 0,
      tag: Tag::new(HEADER.len() as u64 + PRE_TAG.len() as u64 * pre_tags as u64),
      window: None,
      tracks: tracks
        .iter()
        .map(|track| (*track, FlvSeen::default()))
        .collect(),
    }
  }

  /// What was read of each track, in the order the muxer stated them.
  pub(in crate::mux) fn tracks(&self) -> impl Iterator<Item = &FlvSeen> {
    self.tracks.iter().map(|(_, seen)| seen)
  }

  /// The tracks as the muxer stated them, in its order: the laws' seam.
  #[cfg(feature = "test-support")]
  pub(in crate::mux) fn table(&self) -> impl Iterator<Item = &FlvTrack> {
    self.tracks.iter().map(|(track, _)| track)
  }

  /// Why a file the walk ended at `frontier` is not whole, or `None`: a size
  /// rewrite part written; the header or a pre-tag part read; a tag open,
  /// named at its offset.
  pub(in crate::mux) fn unfinished(&self, frontier: u64) -> Option<Unfollowed> {
    if self.tag.patched > 0 {
      return Some(Unfollowed::new(frontier, Why::Unresolved));
    }
    if self.head < HEADER.len() || self.pre_tags > 0 {
      return Some(Unfollowed::new(frontier, Why::Truncated));
    }
    (self.tag.phase != Phase::Header || self.tag.have > 0)
      .then(|| Unfollowed::new(self.tag.at, Why::Truncated))
  }

  /// `bytes`, appended at `at`.
  pub(in crate::mux) fn append(&mut self, at: u64, bytes: &[u8]) -> Result<(), Unfollowed> {
    // A size rewrite in parts is whole before any byte after it.
    if self.tag.patched > 0 {
      return Err(Unfollowed::new(at, Why::Unresolved));
    }
    for (offset, &byte) in bytes.iter().enumerate() {
      self.byte(at.saturating_add(offset as u64), byte)?;
    }
    Ok(())
  }

  /// One byte, appended at `pos`.
  fn byte(&mut self, pos: u64, byte: u8) -> Result<(), Unfollowed> {
    if self.head < HEADER.len() {
      if self.head != 4 && byte != HEADER[self.head] {
        return Err(Unfollowed::new(pos, Why::Structure));
      }
      self.head += 1;
      return Ok(());
    }
    if self.pre_tags > 0 {
      if byte != PRE_TAG[self.pre_read] {
        return Err(Unfollowed::new(pos - self.pre_read as u64, Why::Structure));
      }
      self.pre_read += 1;
      if self.pre_read == PRE_TAG.len() {
        self.pre_read = 0;
        self.pre_tags -= 1;
      }
      return Ok(());
    }
    match self.tag.phase {
      Phase::Header => self.header_byte(byte),
      Phase::Data => {
        let tag = &mut self.tag;
        if let Some(slot) = tag
          .class
          .get_mut(usize::try_from(tag.read).unwrap_or(CLASS))
        {
          *slot = byte;
        }
        tag.read += 1;
        if tag.read == 1 && !first_byte(tag.kind, tag.size, byte) {
          return Err(Unfollowed::new(tag.at, Why::Structure));
        }
        self.data_end(pos + 1)
      }
      Phase::Lookahead => self.lookahead_byte(pos, byte),
      Phase::Trailer => {
        let tag = &mut self.tag;
        tag.bytes[tag.have] = byte;
        tag.have += 1;
        if tag.have < 4 {
          return Ok(());
        }
        let word = u32::from_be_bytes([tag.bytes[0], tag.bytes[1], tag.bytes[2], tag.bytes[3]]);
        let Size::Final(size) = tag.size else {
          return Err(Unfollowed::new(pos, Why::Structure));
        };
        if u64::from(word) != size + TAG_HEADER {
          // A PreviousTagSize other than its tag's.
          return Err(Unfollowed::new(pos - 3, Why::Structure));
        }
        self.tag = Tag::new(pos + 1);
        Ok(())
      }
    }
  }

  /// One byte of a tag's header: its type and its data size, once whole.
  fn header_byte(&mut self, byte: u8) -> Result<(), Unfollowed> {
    let tag = &mut self.tag;
    tag.bytes[tag.have] = byte;
    tag.have += 1;
    if tag.have < TAG_HEADER as usize {
      return Ok(());
    }
    tag.have = 0;
    tag.kind = tag.bytes[0];
    if ![AUDIO, VIDEO, SCRIPT].contains(&tag.kind) {
      return Err(Unfollowed::new(tag.at, Why::Structure));
    }
    let size = u64::from(u32::from_be_bytes([
      0,
      tag.bytes[1],
      tag.bytes[2],
      tag.bytes[3],
    ]));
    tag.size = match (tag.kind, size) {
      (_, 0) => Size::Placeholder,
      (SCRIPT, size) => Size::Provisional(size),
      (VIDEO, 5) => Size::Five,
      (_, size) => Size::Final(size),
    };
    tag.phase = Phase::Data;
    let at = tag.at;
    if tag.kind == SCRIPT && self.window.is_none() {
      // `onMetaData`: the first script tag, written at the header.
      self.window = Some((at + TAG_HEADER, None));
    }
    self.data_end(at + TAG_HEADER)
  }

  /// One of the 4 bytes after a script tag's provisional end: its
  /// PreviousTagSize where they read the size and 11, else data, the tag
  /// running on until its size's rewrite closes it.
  fn lookahead_byte(&mut self, pos: u64, byte: u8) -> Result<(), Unfollowed> {
    let tag = &mut self.tag;
    tag.bytes[tag.have] = byte;
    tag.have += 1;
    if tag.have < 4 {
      return Ok(());
    }
    let Size::Provisional(size) = tag.size else {
      return Err(Unfollowed::new(pos, Why::Structure));
    };
    let held = [tag.bytes[0], tag.bytes[1], tag.bytes[2], tag.bytes[3]];
    tag.have = 0;
    if u64::from(u32::from_be_bytes(held)) == size + TAG_HEADER {
      // Its PreviousTagSize: the provisional size was final.
      tag.size = Size::Final(size);
      self.closed(pos - 3);
      self.tag = Tag::new(pos + 1);
      return Ok(());
    }
    for (offset, data) in held.into_iter().enumerate() {
      let at = usize::try_from(tag.read).unwrap_or(CLASS) + offset;
      if let Some(slot) = tag.class.get_mut(at) {
        *slot = data;
      }
    }
    tag.read += 4;
    tag.size = Size::Placeholder;
    tag.phase = Phase::Data;
    Ok(())
  }

  /// Whether the tag's data ends at `pos`, `read` bytes of it read: of a
  /// final size, at it; of a provisional one, its lookahead begins there;
  /// R10's size of 5 final where the first byte is not HDR metadata's.
  fn data_end(&mut self, pos: u64) -> Result<(), Unfollowed> {
    let tag = &mut self.tag;
    if tag.size == Size::Five && tag.read == 1 && tag.class[0] != METADATA {
      tag.size = Size::Final(5);
    }
    match tag.size {
      Size::Final(size) if tag.read == size => {
        self.closed(pos);
        Ok(())
      }
      Size::Provisional(size) if tag.read == size => {
        tag.phase = Phase::Lookahead;
        tag.have = 0;
        Ok(())
      }
      _ => Ok(()),
    }
  }

  /// The tag's data ended at `pos`: `onMetaData`'s window closed, its
  /// track's reading counted, and its PreviousTagSize next.
  fn closed(&mut self, pos: u64) {
    let tag = self.tag;
    if let Some((start, end @ None)) = &mut self.window
      && tag.kind == SCRIPT
      && *start == tag.at + TAG_HEADER
    {
      *end = Some(pos);
    }
    if let Size::Final(size) = tag.size {
      let read = usize::try_from(size).unwrap_or(CLASS).min(CLASS);
      if let Some((index, packet)) = classify(tag.kind, &tag.class[..read])
        && let Some((track, seen)) = self
          .tracks
          .iter_mut()
          .find(|(track, _)| track.kind == tag.kind && track.index == index)
      {
        match packet {
          Packet::Sequence => {
            let record = size.saturating_sub(track.header);
            let record = if track.emptiness {
              u64::from(record > 0)
            } else {
              record
            };
            seen.sequences.push(record, 1);
          }
          Packet::Frames => seen.frames.push(size, 1),
          Packet::Other => {}
        }
      }
    }
    self.tag.phase = Phase::Trailer;
    self.tag.have = 0;
  }

  /// `bytes`, written at `at` below `frontier`: inside `onMetaData`'s
  /// window, or the open tag's size, rewritten.
  pub(in crate::mux) fn patch(
    &mut self,
    frontier: u64,
    at: u64,
    bytes: &[u8],
  ) -> Result<(), Unfollowed> {
    let end = at.saturating_add(bytes.len() as u64);
    if let Some((start, window_end)) = self.window
      && at >= start
      && end <= window_end.unwrap_or(frontier)
    {
      return Ok(());
    }
    let tag = &mut self.tag;
    let field = tag.at + 1;
    if !tag.rewritten() || at != field + tag.patched as u64 || end > field + 3 {
      return Err(Unfollowed::new(at, Why::Rewrite));
    }
    tag.patch[tag.patched..tag.patched + bytes.len()].copy_from_slice(bytes);
    tag.patched += bytes.len();
    if tag.patched < 3 {
      return Ok(());
    }
    tag.patched = 0;
    let size = u64::from(u32::from_be_bytes([
      0,
      tag.patch[0],
      tag.patch[1],
      tag.patch[2],
    ]));
    // The rewrite closes the tag at the frontier: every byte after its
    // header its data, a lookahead's held bytes among them.
    if size != frontier.saturating_sub(tag.at + TAG_HEADER) {
      return Err(Unfollowed::new(field, Why::Structure));
    }
    tag.read = size;
    tag.size = Size::Final(size);
    self.closed(frontier);
    Ok(())
  }
}

/// Whether `byte`, the first of a tag's data of `kind` and `size`, is its
/// writer's: a placeholder's — a codec header's or `onMetaData`'s — opens
/// on its own (see [the module](self)); any other tag on any.
fn first_byte(kind: u8, size: Size, byte: u8) -> bool {
  if size != Size::Placeholder {
    return true;
  }
  match kind {
    AUDIO => byte == 0xAF || byte >> 4 == 9,
    VIDEO => byte == 0x17 || byte == 0x19 || byte >> 4 == 9,
    _ => byte == 2,
  }
}

/// What a tag's data is, of its track.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Packet {
  /// A sequence start: a configuration's record.
  Sequence,
  /// Coded frames.
  Frames,
  /// Anything else, walked and not judged.
  Other,
}

/// The track index and the packet of an audio or video tag of the first
/// data bytes `class` (`flvenc.c` 807–899, 1395–1452), or `None` of a
/// script tag's.
fn classify(kind: u8, class: &[u8]) -> Option<(u8, Packet)> {
  let first = *class.first()?;
  let at = |index: usize| class.get(index).copied().unwrap_or(0);
  match kind {
    VIDEO if first & 0x80 != 0 => {
      // The extended header: the packet's type in the low four bits, a
      // multitrack's in the next byte, then the FourCC and the index.
      let (packet, index) = if first & 0x0F == 6 {
        (at(1) & 0x0F, at(6))
      } else {
        (first & 0x0F, 0)
      };
      Some((
        index,
        match packet {
          0 => Packet::Sequence,
          1 | 3 => Packet::Frames,
          _ => Packet::Other,
        },
      ))
    }
    VIDEO => Some((
      0,
      // H.264's and MPEG-4's packet type after the flags; another codec's
      // tag holds coded frames alone.
      match (first & 0x0F, at(1)) {
        (7 | 9, 0) => Packet::Sequence,
        (7 | 9, 1) => Packet::Frames,
        (7 | 9, _) => Packet::Other,
        _ => Packet::Frames,
      },
    )),
    AUDIO if first >> 4 == 9 => {
      let (packet, index) = if first & 0x0F == 5 {
        (at(1) & 0x0F, at(6))
      } else {
        (first & 0x0F, 0)
      };
      Some((
        index,
        match packet {
          0 => Packet::Sequence,
          1 => Packet::Frames,
          _ => Packet::Other,
        },
      ))
    }
    AUDIO => Some((
      0,
      match (first >> 4, at(1)) {
        (10, 0) => Packet::Sequence,
        _ => Packet::Frames,
      },
    )),
    _ => None,
  }
}
