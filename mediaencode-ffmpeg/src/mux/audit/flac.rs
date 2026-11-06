//! Raw FLAC's walk: `fLaC`, its metadata blocks, then its frames, as the
//! sink takes them (`flacenc.c` in FFmpeg 9.0.1).
//!
//! **What it follows.** `fLaC`; then metadata blocks, each a byte of its
//! last-block flag and its type, a 24-bit length and its data: STREAMINFO
//! first (`ff_flac_write_header`, `flacenc_header.c` 29–48), its 34 bytes a
//! window the trailer writes again, once, with the encoder's final
//! STREAMINFO (`flac_write_trailer`, `flacenc.c` 307–336); then the
//! pictures, the VORBIS_COMMENT (`flac_write_block_comment`, 63–80) and
//! PADDING (54–61), the last of them flagged (`flac_finish_header`,
//! 170–203). Then frames, counted and not read: the file may end anywhere
//! among them. Any write below the frontier but STREAMINFO's rewrite is a
//! rewrite of no field.
//!
//! **What it reads.** The VORBIS_COMMENT, as `ff_vorbiscomment_write`
//! writes it (`vorbiscomment.c` 62–100): its vendor, after its length; its
//! count — none where the dictionary is empty — and its entries, each after
//! its length. Those whose key is `WAVEFORMATEXTENSIBLE_CHANNEL_MASK` are
//! counted, each folded whole; libavformat's own `encoder` and any other
//! entry walked and not judged (the owner's ruling on letter 1 (b)).

use super::{FOLD_SEED, Unfollowed, Why, fold};

/// FLAC's stream marker.
const MARKER: [u8; 4] = *b"fLaC";

/// The STREAMINFO block's data: its offset and length.
const STREAMINFO: (u64, u64) = (8, 34);

/// A metadata block's type: VORBIS_COMMENT.
const VORBIS_COMMENT: u8 = 4;

/// The key, and its `=`, of FLAC's channel mask comment.
const MASK_KEY: &[u8] = b"WAVEFORMATEXTENSIBLE_CHANNEL_MASK=";

/// Where the walk stands within the VORBIS_COMMENT's data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Comment {
  /// A 32-bit length or count, its bytes read: the vendor's length, the
  /// count, an entry's length.
  Word { what: Word, have: usize, value: u32 },
  /// The vendor's bytes, to its end.
  Vendor { end: u64 },
  /// An entry's bytes, to its end: the key's bytes matched so far — `None`
  /// once one differs — and the entry's fold.
  Entry {
    end: u64,
    matched: Option<usize>,
    fold: u64,
  },
  /// Every entry the count names read.
  Done,
}

/// The 32-bit words of a VORBIS_COMMENT.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Word {
  VendorLength,
  Count,
  EntryLength,
}

/// Where the walk stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
  /// `fLaC`, its bytes read.
  Marker(usize),
  /// A block's header, its bytes read.
  Header {
    at: u64,
    have: usize,
    bytes: [u8; 4],
  },
  /// A block's data stepped over, to its end; `last` of the last block.
  Skip { at: u64, end: u64, last: bool },
  /// The VORBIS_COMMENT's data, to its end.
  Comment { at: u64, end: u64, last: bool },
  /// The frames.
  Frames,
}

/// Raw FLAC's walk (see [the module](self)).
pub(in crate::mux) struct FlacWalk {
  state: State,
  comment: Comment,
  /// The VORBIS_COMMENT's entries left to read.
  entries: u32,
  /// The channel mask entries read, and the fold of the last.
  masks: (u64, u64),
  /// STREAMINFO's bytes written again so far, in order.
  rewritten: u64,
}

impl FlacWalk {
  pub(in crate::mux) const fn new() -> Self {
    Self {
      state: State::Marker(0),
      comment: Self::word(Word::VendorLength),
      entries: 0,
      masks: (0, FOLD_SEED),
      rewritten: 0,
    }
  }

  /// The channel mask entries the VORBIS_COMMENT holds, and the fold of
  /// the last one's bytes.
  pub(in crate::mux) const fn masks(&self) -> (u64, u64) {
    self.masks
  }

  /// Why a file the walk ended at `frontier` is not whole, or `None`:
  /// STREAMINFO part written again; the marker or a block's header part
  /// read; a block open, named at its header.
  pub(in crate::mux) fn unfinished(&self, frontier: u64) -> Option<Unfollowed> {
    if self.rewritten > 0 && self.rewritten < STREAMINFO.1 {
      return Some(Unfollowed::new(STREAMINFO.0, Why::Unresolved));
    }
    match self.state {
      State::Frames => None,
      State::Marker(_) => Some(Unfollowed::new(frontier, Why::Truncated)),
      State::Header { at, .. } | State::Skip { at, .. } | State::Comment { at, .. } => {
        Some(Unfollowed::new(at, Why::Truncated))
      }
    }
  }

  /// `bytes`, appended at `at`.
  pub(in crate::mux) fn append(&mut self, at: u64, bytes: &[u8]) -> Result<(), Unfollowed> {
    let (mut pos, mut bytes) = (at, bytes);
    while !bytes.is_empty() {
      let taken = match self.state {
        State::Frames => bytes.len(),
        State::Marker(have) => {
          if bytes[0] != MARKER[have] {
            return Err(Unfollowed::new(pos, Why::Structure));
          }
          self.state = if have + 1 == MARKER.len() {
            State::Header {
              at: pos + 1,
              have: 0,
              bytes: [0; 4],
            }
          } else {
            State::Marker(have + 1)
          };
          1
        }
        State::Header {
          at: start,
          have,
          bytes: mut header,
        } => {
          header[have] = bytes[0];
          self.state = if have + 1 < 4 {
            State::Header {
              at: start,
              have: have + 1,
              bytes: header,
            }
          } else {
            self.block(start, header)?
          };
          1
        }
        State::Skip { end, last, .. } => {
          let taken = usize::try_from(end - pos)
            .unwrap_or(usize::MAX)
            .min(bytes.len());
          if pos + taken as u64 == end {
            self.state = Self::after(end, last);
          }
          taken
        }
        State::Comment {
          at: start,
          end,
          last,
        } => {
          let taken = self.comment_byte(start, pos, end, bytes[0])?;
          if pos + 1 == end {
            self.comment_closed(start)?;
            self.state = Self::after(end, last);
          }
          taken
        }
      };
      pos += taken as u64;
      bytes = &bytes[taken..];
    }
    Ok(())
  }

  /// The state after a block that ends at `end`.
  const fn after(end: u64, last: bool) -> State {
    if last {
      State::Frames
    } else {
      State::Header {
        at: end,
        have: 0,
        bytes: [0; 4],
      }
    }
  }

  /// A block's header whole, at `at`: STREAMINFO first, of its 34 bytes;
  /// the VORBIS_COMMENT read; any other stepped over.
  fn block(&mut self, at: u64, header: [u8; 4]) -> Result<State, Unfollowed> {
    let last = header[0] & 0x80 != 0;
    let kind = header[0] & 0x7F;
    let len = u64::from(u32::from_be_bytes([0, header[1], header[2], header[3]]));
    let data = at + 4;
    let end = data + len;
    if (at == STREAMINFO.0 - 4) != (kind == 0) || (kind == 0 && len != STREAMINFO.1) {
      return Err(Unfollowed::new(at, Why::Structure));
    }
    if len == 0 {
      return Ok(Self::after(end, last));
    }
    Ok(if kind == VORBIS_COMMENT {
      self.comment = Self::word(Word::VendorLength);
      self.entries = 0;
      self.masks = (0, FOLD_SEED);
      State::Comment { at, end, last }
    } else {
      State::Skip { at, end, last }
    })
  }

  /// One byte of the VORBIS_COMMENT's data, at `pos`, the block at `at`
  /// ending at `end`.
  fn comment_byte(&mut self, at: u64, pos: u64, end: u64, byte: u8) -> Result<usize, Unfollowed> {
    let structure = Unfollowed::new(at, Why::Structure);
    let after = pos + 1;
    self.comment = match self.comment {
      Comment::Word { what, have, value } if have < 3 => Comment::Word {
        what,
        have: have + 1,
        value: value | u32::from(byte) << (8 * have),
      },
      Comment::Word { what, value, .. } => {
        let value = u64::from(value | u32::from(byte) << 24);
        match what {
          Word::VendorLength if value == 0 => Self::word(Word::Count),
          Word::VendorLength => Comment::Vendor { end: after + value },
          Word::Count => {
            self.entries = u32::try_from(value).unwrap_or(u32::MAX);
            self.next_entry()
          }
          Word::EntryLength if value == 0 => {
            self.entries -= 1;
            self.next_entry()
          }
          Word::EntryLength => Comment::Entry {
            end: after + value,
            matched: Some(0),
            fold: FOLD_SEED,
          },
        }
      }
      Comment::Vendor { end: vendor } if after == vendor => Self::word(Word::Count),
      vendor @ Comment::Vendor { .. } => vendor,
      Comment::Entry {
        end: entry,
        matched,
        fold: state,
      } => {
        let matched = matched.and_then(|matched| match MASK_KEY.get(matched) {
          Some(&key) if key == byte => Some(matched + 1),
          Some(_) => None,
          None => Some(matched),
        });
        let state = fold(state, u64::from(byte));
        if after == entry {
          if matched == Some(MASK_KEY.len()) {
            self.masks = (self.masks.0 + 1, state);
          }
          self.entries -= 1;
          self.next_entry()
        } else {
          Comment::Entry {
            end: entry,
            matched,
            fold: state,
          }
        }
      }
      // A byte past the entries the count names.
      Comment::Done => return Err(structure),
    };
    // A length past the block's end is no writer's.
    match self.comment {
      Comment::Vendor { end: inner } | Comment::Entry { end: inner, .. } if inner > end => {
        Err(structure)
      }
      _ => Ok(1),
    }
  }

  /// A 32-bit word of the VORBIS_COMMENT's, none of its bytes read.
  const fn word(what: Word) -> Comment {
    Comment::Word {
      what,
      have: 0,
      value: 0,
    }
  }

  /// The next entry's length, or every entry read.
  const fn next_entry(&self) -> Comment {
    if self.entries > 0 {
      Self::word(Word::EntryLength)
    } else {
      Comment::Done
    }
  }

  /// The VORBIS_COMMENT's data read to its end, at the block `at`: whole —
  /// its vendor alone, of an empty dictionary (`ff_vorbiscomment_write`,
  /// `vorbiscomment.c` 78), or every entry its count names — else no
  /// writer's.
  fn comment_closed(&self, at: u64) -> Result<(), Unfollowed> {
    match self.comment {
      Comment::Done
      | Comment::Word {
        what: Word::Count,
        have: 0,
        ..
      } => Ok(()),
      _ => Err(Unfollowed::new(at, Why::Structure)),
    }
  }

  /// `bytes`, written at `at` below the frontier: STREAMINFO's data, in
  /// order and once.
  pub(in crate::mux) fn patch(
    &mut self,
    _frontier: u64,
    at: u64,
    bytes: &[u8],
  ) -> Result<(), Unfollowed> {
    let end = at.saturating_add(bytes.len() as u64);
    if at != STREAMINFO.0 + self.rewritten || end > STREAMINFO.0 + STREAMINFO.1 {
      return Err(Unfollowed::new(at, Why::Rewrite));
    }
    self.rewritten += bytes.len() as u64;
    Ok(())
  }
}
