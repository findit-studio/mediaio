//! Matroska's and WebM's walk: the EBML header, the Segment and its level
//! 1 elements, and the TrackEntries Tracks holds — as the sink takes them
//! (`matroskaenc.c` in FFmpeg 9.0.1).
//!
//! **What it follows.** Each element its ID, of 1 to 4 bytes, and its size,
//! of 1 to 8: the EBML header, stepped over by its size; the Segment, its
//! size written unknown in 8 bytes (`mkv_write_header`, 2687–2689) — a
//! field the trailer rewrites with the Segment's length (3402–3408),
//! registered as the header is read, accepted at any value and judged at
//! the verdict; then the Segment's elements, each stepped over by its size
//! — Clusters, Cues, Chapters, Attachments, Voids — but four, which the
//! trailer writes again whole, each of its own size, from the buffer the
//! header wrote it from (`end_ebml_master_crc32_tentatively`, 848–869;
//! 3410–3492): the Void the header reserves for the SeekHead at the
//! Segment's first byte (`mkv_start_seekhead`, 901–909; `mkv_write_seekhead`,
//! 921–960), Info, Tracks and Tags. Each is a window registered as its
//! header is read: its rewrite must run through it in order, once, and
//! close at its end; any other write below the frontier is a rewrite of no
//! field.
//!
//! **What it reads.** Tracks' TrackEntries, in order — one a stream but an
//! attachment's (`mkv_write_track`, 1956–1957) — each its CodecPrivate's
//! length and the fold of its bytes from the offset the muxer states;
//! read as the header writes Tracks, then again from the trailer's
//! rewrite, whose reading replaces the first: the CodecPrivate a packet's
//! new configuration rewrote in Tracks' buffer (`mkv_update_codecprivate`,
//! 1334–1369; `mkv_check_new_extra_data`, 3052–3122) is the one judged.

use super::{FOLD_SEED, Unfollowed, Why, fold};

/// The EBML header's ID.
const EBML: u32 = 0x1A45_DFA3;
/// The Segment's.
const SEGMENT: u32 = 0x1853_8067;
/// A Void's.
const VOID: u32 = 0xEC;
/// Info's.
const INFO: u32 = 0x1549_A966;
/// Tracks'.
const TRACKS: u32 = 0x1654_AE6B;
/// Tags'.
const TAGS: u32 = 0x1254_C367;
/// A TrackEntry's.
const TRACK_ENTRY: u32 = 0xAE;
/// A CodecPrivate's.
const CODEC_PRIVATE: u32 = 0x63A2;

/// The regions the trailer writes again: the Segment's size, then the
/// SeekHead's Void, Info, Tracks and Tags.
const SEGMENT_SIZE: usize = 0;
const SEEK_HEAD: usize = 1;
const WINDOW_INFO: usize = 2;
const WINDOW_TRACKS: usize = 3;
const WINDOW_TAGS: usize = 4;

/// A track as the muxer states it at open, in TrackEntry order: the offset
/// in its CodecPrivate from which the walk folds its bytes, `None` of a
/// fold of none.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::mux) struct MkvTrack {
  pub(in crate::mux) fold_from: Option<u64>,
}

/// What the walk read of one track's TrackEntry: its CodecPrivate's length
/// and the fold of its bytes from the stated offset, `None` of no
/// CodecPrivate.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(in crate::mux) struct MkvSeen {
  pub(in crate::mux) codec_private: Option<(u64, u64)>,
}

/// An element's header being read.
#[derive(Debug, Clone, Copy, Default)]
struct Head {
  bytes: [u8; 12],
  have: usize,
}

/// An element's header, whole: its ID, its size — `None` of an unknown one
/// — and the header's bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Element {
  id: u32,
  size: Option<u64>,
  head: u64,
}

impl Head {
  /// One more byte of the header: the element, once whole; an ID or a size
  /// of a length no EBML writer makes, `Err`.
  fn byte(&mut self, byte: u8) -> Result<Option<Element>, ()> {
    self.bytes[self.have] = byte;
    self.have += 1;
    let id_len = self.bytes[0].leading_zeros() as usize + 1;
    if id_len > 4 {
      return Err(());
    }
    if self.have <= id_len {
      return Ok(None);
    }
    let first = self.bytes[id_len];
    let size_len = first.leading_zeros() as usize + 1;
    if size_len > 8 {
      return Err(());
    }
    if self.have < id_len + size_len {
      return Ok(None);
    }
    let id = self.bytes[..id_len]
      .iter()
      .fold(0_u32, |id, &byte| id << 8 | u32::from(byte));
    let size = self.bytes[id_len + 1..id_len + size_len]
      .iter()
      .fold(u64::from(first) & (0xFF >> size_len), |size, &byte| {
        size << 8 | u64::from(byte)
      });
    let unknown = size == (1_u64 << (7 * size_len)) - 1;
    let head = self.have as u64;
    self.have = 0;
    Ok(Some(Element {
      id,
      size: (!unknown).then_some(size),
      head,
    }))
  }
}

/// A region the trailer writes again: its first byte, its length, and the
/// bytes of it written again so far, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Region {
  at: u64,
  len: u64,
  done: u64,
}

/// Where the walk stands at the top of the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
  /// An element's header before the Segment: the EBML header's, then the
  /// Segment's.
  Top { ebml: bool },
  /// The EBML header's payload, stepped over to its end.
  Ebml { end: u64 },
  /// A level 1 element's header, inside the Segment.
  Level1,
  /// A level 1 element's payload, stepped over to its end.
  Skip { end: u64 },
  /// Tracks, read to its end.
  Tracks { end: u64 },
}

/// Tracks being read, from its own first byte: at the header, or from the
/// trailer's rewrite.
#[derive(Debug, Clone, Copy)]
struct TracksRead {
  head: Head,
  /// Where the element whose header is being read begins.
  head_at: u64,
  /// Tracks' end, once its header is read; the open TrackEntry's.
  tracks_end: Option<u64>,
  entry_end: Option<u64>,
  /// A child stepped over, to its end.
  skip_end: Option<u64>,
  /// The CodecPrivate being read: its first byte, its end, and its fold.
  private: Option<(u64, u64, u64)>,
  /// The TrackEntries begun.
  entries: u64,
}

impl TracksRead {
  const fn new(at: u64) -> Self {
    Self {
      head: Head {
        bytes: [0; 12],
        have: 0,
      },
      head_at: at,
      tracks_end: None,
      entry_end: None,
      skip_end: None,
      private: None,
      entries: 0,
    }
  }

  /// Whether Tracks is read to its end.
  fn closed(&self, pos: u64) -> bool {
    self.tracks_end == Some(pos) && self.head.have == 0
  }

  /// `bytes`, at `pos`, read into `tracks`.
  fn read(
    &mut self,
    mut pos: u64,
    mut bytes: &[u8],
    tracks: &mut [(MkvTrack, MkvSeen)],
  ) -> Result<(), Unfollowed> {
    while !bytes.is_empty() {
      if self.tracks_end == Some(pos) && self.head.have == 0 {
        // Bytes past Tracks' end.
        return Err(Unfollowed::new(pos, Why::Structure));
      }
      let taken = if let Some(end) = self.skip_end {
        let taken = usize::try_from(end - pos)
          .unwrap_or(usize::MAX)
          .min(bytes.len());
        if pos + taken as u64 == end {
          self.skip_end = None;
        }
        taken
      } else if let Some((start, end, state)) = self.private {
        let taken = usize::try_from(end - pos)
          .unwrap_or(usize::MAX)
          .min(bytes.len());
        let entry = usize::try_from(self.entries - 1).unwrap_or(usize::MAX);
        let from = tracks
          .get(entry)
          .and_then(|(track, _)| track.fold_from)
          .unwrap_or(u64::MAX);
        let mut state = state;
        for (offset, &byte) in bytes[..taken].iter().enumerate() {
          if pos + offset as u64 - start >= from {
            state = fold(state, u64::from(byte));
          }
        }
        let end_reached = pos + taken as u64 == end;
        self.private = (!end_reached).then_some((start, end, state));
        if end_reached && let Some((_, seen)) = tracks.get_mut(entry) {
          seen.codec_private = Some((end - start, state));
        }
        taken
      } else {
        // An element's header, a byte at a time.
        let element = self
          .head
          .byte(bytes[0])
          .map_err(|()| Unfollowed::new(self.head_at, Why::Structure))?;
        if let Some(element) = element {
          self.element(element, tracks)?;
        }
        1
      };
      pos += taken as u64;
      bytes = &bytes[taken..];
      self.close(pos);
      if self.skip_end.is_none() && self.private.is_none() && self.head.have == 0 {
        self.head_at = pos;
      }
    }
    Ok(())
  }

  /// An element's header read whole at `head_at`.
  fn element(
    &mut self,
    element: Element,
    tracks: &mut [(MkvTrack, MkvSeen)],
  ) -> Result<(), Unfollowed> {
    let structure = Unfollowed::new(self.head_at, Why::Structure);
    let size = element.size.ok_or(structure)?;
    let payload = self.head_at + element.head;
    let end = payload.checked_add(size).ok_or(structure)?;
    let Some(tracks_end) = self.tracks_end else {
      if element.id != TRACKS {
        return Err(structure);
      }
      self.tracks_end = Some(end);
      return Ok(());
    };
    match self.entry_end {
      None if end > tracks_end => Err(structure),
      None if element.id == TRACK_ENTRY => {
        self.entry_end = Some(end);
        self.entries += 1;
        if let Some((_, seen)) = usize::try_from(self.entries - 1)
          .ok()
          .and_then(|entry| tracks.get_mut(entry))
        {
          *seen = MkvSeen::default();
        }
        Ok(())
      }
      None => {
        self.skip_end = (size > 0).then_some(end);
        Ok(())
      }
      Some(entry_end) if end > entry_end => Err(structure),
      Some(_) if element.id == CODEC_PRIVATE => {
        if size == 0 {
          if let Some((_, seen)) = usize::try_from(self.entries - 1)
            .ok()
            .and_then(|entry| tracks.get_mut(entry))
          {
            seen.codec_private = Some((0, FOLD_SEED));
          }
        } else {
          self.private = Some((payload, end, FOLD_SEED));
        }
        Ok(())
      }
      Some(_) => {
        self.skip_end = (size > 0).then_some(end);
        Ok(())
      }
    }
  }

  /// The elements that end at `pos`, closed: a TrackEntry.
  fn close(&mut self, pos: u64) {
    if self.skip_end.is_none() && self.private.is_none() && self.entry_end == Some(pos) {
      self.entry_end = None;
    }
  }
}

/// Matroska's and WebM's walk (see [the module](self)).
pub(in crate::mux) struct MkvWalk {
  state: State,
  head: Head,
  /// Where the element whose header is being read begins.
  head_at: u64,
  /// The Segment's payload's first byte, once its header is read.
  segment: Option<u64>,
  /// The Segment's size as last written: the header's unknown size, or the
  /// trailer's rewrite of it.
  segment_size: [u8; 8],
  /// The regions the trailer writes again, as their headers are read.
  regions: [Option<Region>; 5],
  /// Tracks being read: at the header, or from the trailer's rewrite.
  tracks_read: Option<TracksRead>,
  /// The TrackEntries the last whole reading of Tracks held.
  entries: u64,
  /// The muxer's tracks, in TrackEntry order, and what was read of each.
  tracks: Box<[(MkvTrack, MkvSeen)]>,
}

impl MkvWalk {
  /// The walk of a file of `tracks`, in TrackEntry order: its table
  /// allocated here, at open.
  pub(in crate::mux) fn new(tracks: &[MkvTrack]) -> Self {
    Self {
      state: State::Top { ebml: false },
      head: Head::default(),
      head_at: 0,
      segment: None,
      segment_size: [0xFF; 8],
      regions: [None; 5],
      tracks_read: None,
      entries: 0,
      tracks: tracks
        .iter()
        .map(|track| (*track, MkvSeen::default()))
        .collect(),
    }
  }

  /// What was read of each track, in TrackEntry order.
  pub(in crate::mux) fn tracks(&self) -> impl Iterator<Item = &MkvSeen> {
    self.tracks.iter().map(|(_, seen)| seen)
  }

  /// The TrackEntries the last whole reading of Tracks held.
  pub(in crate::mux) const fn entries(&self) -> u64 {
    self.entries
  }

  /// The Segment's payload's first byte and its size as written — the
  /// unknown size's `2^56 − 1` where the trailer did not write it — or
  /// `None` of a file of no Segment read.
  pub(in crate::mux) fn segment(&self) -> Option<(u64, u64)> {
    let at = self.segment?;
    let size = u64::from_be_bytes(self.segment_size) & ((1 << 56) - 1);
    Some((at, size))
  }

  /// Why a file the walk ended at `frontier` is not whole, or `None`: a
  /// region part written again; the Segment never read; an element open,
  /// named at its first byte.
  pub(in crate::mux) fn unfinished(&self, frontier: u64) -> Option<Unfollowed> {
    if let Some(region) = self
      .regions
      .iter()
      .flatten()
      .find(|region| region.done > 0 && region.done < region.len)
    {
      return Some(Unfollowed::new(region.at, Why::Unresolved));
    }
    match self.state {
      State::Level1 if self.head.have == 0 => None,
      State::Top { .. } | State::Ebml { .. } => Some(Unfollowed::new(frontier, Why::Truncated)),
      _ => Some(Unfollowed::new(self.head_at, Why::Truncated)),
    }
  }

  /// `bytes`, appended at `at`.
  pub(in crate::mux) fn append(&mut self, at: u64, bytes: &[u8]) -> Result<(), Unfollowed> {
    let (mut pos, mut bytes) = (at, bytes);
    while !bytes.is_empty() {
      let taken = match self.state {
        State::Ebml { end } | State::Skip { end } => {
          let taken = usize::try_from(end - pos)
            .unwrap_or(usize::MAX)
            .min(bytes.len());
          if pos + taken as u64 == end {
            self.state = if matches!(self.state, State::Ebml { .. }) {
              State::Top { ebml: true }
            } else {
              State::Level1
            };
          }
          taken
        }
        State::Tracks { end } => {
          let taken = usize::try_from(end - pos)
            .unwrap_or(usize::MAX)
            .min(bytes.len());
          let read = self
            .tracks_read
            .as_mut()
            .ok_or(Unfollowed::new(pos, Why::Structure))?;
          read.read(pos, &bytes[..taken], &mut self.tracks)?;
          if pos + taken as u64 == end {
            self.tracks_closed(end)?;
            self.state = State::Level1;
          }
          taken
        }
        State::Top { .. } | State::Level1 => {
          if self.head.have == 0 {
            self.head_at = pos;
          }
          let element = self
            .head
            .byte(bytes[0])
            .map_err(|()| Unfollowed::new(self.head_at, Why::Structure))?;
          if let Some(element) = element {
            self.element(element)?;
          }
          1
        }
      };
      pos += taken as u64;
      bytes = &bytes[taken..];
    }
    Ok(())
  }

  /// An element's header read whole at `head_at`, its header's bytes still
  /// held.
  fn element(&mut self, element: Element) -> Result<(), Unfollowed> {
    let at = self.head_at;
    let structure = Unfollowed::new(at, Why::Structure);
    let payload = at + element.head;
    match self.state {
      State::Top { ebml: false } => {
        // The EBML header, of a known size (`ebml_write_header`, 2587–2601).
        let size = element
          .size
          .filter(|_| element.id == EBML)
          .ok_or(structure)?;
        self.state = if size == 0 {
          State::Top { ebml: true }
        } else {
          State::Ebml {
            end: payload + size,
          }
        };
        Ok(())
      }
      State::Top { ebml: true } => {
        // The Segment, of a size written unknown in 8 bytes (2687–2689).
        if element.id != SEGMENT || element.size.is_some() || element.head != 12 {
          return Err(structure);
        }
        self.segment = Some(payload);
        self.segment_size = [0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF];
        self.regions[SEGMENT_SIZE] = Some(Region {
          at: at + 4,
          len: 8,
          done: 0,
        });
        self.state = State::Level1;
        Ok(())
      }
      State::Level1 => {
        let size = element.size.ok_or(structure)?;
        let end = payload.checked_add(size).ok_or(structure)?;
        let window = match element.id {
          VOID if Some(at) == self.segment => Some(SEEK_HEAD),
          INFO => Some(WINDOW_INFO),
          TRACKS => Some(WINDOW_TRACKS),
          TAGS => Some(WINDOW_TAGS),
          _ => None,
        }
        .filter(|&window| self.regions[window].is_none());
        if let Some(window) = window {
          self.regions[window] = Some(Region {
            at,
            len: end - at,
            done: 0,
          });
        }
        if window == Some(WINDOW_TRACKS) {
          // Tracks, read from its own first byte: its header's bytes, then
          // its payload as it comes.
          let mut read = TracksRead::new(at);
          let head = self.head.bytes;
          read.read(
            at,
            &head[..usize::try_from(element.head).unwrap_or(0)],
            &mut self.tracks,
          )?;
          self.tracks_read = Some(read);
          if size == 0 {
            self.tracks_closed(end)?;
          } else {
            self.state = State::Tracks { end };
          }
        } else if size > 0 {
          self.state = State::Skip { end };
        }
        Ok(())
      }
      _ => Err(structure),
    }
  }

  /// Tracks read whole to `end`: its TrackEntries counted.
  fn tracks_closed(&mut self, end: u64) -> Result<(), Unfollowed> {
    let read = self
      .tracks_read
      .take()
      .ok_or(Unfollowed::new(end, Why::Structure))?;
    if !read.closed(end) {
      return Err(Unfollowed::new(read.head_at, Why::Structure));
    }
    self.entries = read.entries;
    Ok(())
  }

  /// `bytes`, written at `at` below `frontier`: the regions the trailer
  /// writes again, each in order and once.
  pub(in crate::mux) fn patch(
    &mut self,
    _frontier: u64,
    at: u64,
    bytes: &[u8],
  ) -> Result<(), Unfollowed> {
    let (mut pos, mut bytes) = (at, bytes);
    while !bytes.is_empty() {
      let rewrite = Unfollowed::new(pos, Why::Rewrite);
      let slot = self
        .regions
        .iter()
        .position(|region| {
          region.is_some_and(|region| (region.at..region.at + region.len).contains(&pos))
        })
        .ok_or(rewrite)?;
      let Some(region) = self.regions[slot].as_mut() else {
        return Err(rewrite);
      };
      if pos != region.at + region.done {
        return Err(rewrite);
      }
      let taken = usize::try_from(region.len - region.done)
        .unwrap_or(usize::MAX)
        .min(bytes.len());
      let part = &bytes[..taken];
      let first = region.done == 0;
      region.done += taken as u64;
      let whole = region.done == region.len;
      let region = *region;
      match slot {
        SEGMENT_SIZE => {
          let from = usize::try_from(pos - region.at).unwrap_or(0);
          self.segment_size[from..from + taken].copy_from_slice(part);
          // An 8-byte length (`put_ebml_length`, 379–392).
          if whole && self.segment_size[0] != 0x01 {
            return Err(Unfollowed::new(region.at, Why::Structure));
          }
        }
        WINDOW_TRACKS => {
          if first {
            self.tracks_read = Some(TracksRead::new(region.at));
          }
          let read = self.tracks_read.as_mut().ok_or(rewrite)?;
          read.read(pos, part, &mut self.tracks)?;
          if whole {
            self.tracks_closed(region.at + region.len)?;
          }
        }
        _ => {}
      }
      pos += taken as u64;
      bytes = &bytes[taken..];
    }
    Ok(())
  }
}
