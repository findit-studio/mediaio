//! The write audit: what FFmpeg wrote, read as the caller's sink takes it,
//! and judged at `finish` against what the muxer handed it
//! (findit-studio/mediaio#95).
//!
//! The census's premise rows (see [the module](super)) are what FFmpeg's
//! writers put in a file when an allocation fails and the failure is
//! swallowed: a box left out, a size written otherwise, success answered.
//! The muxer cannot see a swallowed failure, but it can see the file: the
//! writer guard hands every run of bytes the sink takes to a tap
//! ([`SinkTap`]), and the tap here walks them in constant memory, keeping
//! no byte — each format's boxes, tags or elements by their sizes, the
//! tables it judges folded as they stream ([`fold`]). The muxer keeps what
//! it expects beside its own mirrors, at each packet, allocating nothing,
//! and once the trailer is written it compares the two
//! ([`CarrierMuxer::finish_into_sink`](super::CarrierMuxer::finish_into_sink)):
//! a difference is a named failure ([`WrittenOtherwise`]), the output is
//! abandoned, and the sink is not handed back.
//!
//! # The byte stream the sink sees
//!
//! The tap is handed each run at libavformat's offset. A run at the
//! frontier — every byte appended before it — is an append, which the walk
//! steps over; a run wholly below the frontier is a patch, admitted only
//! where it rewrites a field the walk registered when it read the bytes it
//! covers; a run past the frontier is a gap. A patch reaches the sink
//! either inside the bytes it patches, as their final value, or after every
//! byte of the structure it closes and before any byte after it
//! (`aviobuf.c` 168–182, 272–280, 302–317 in FFmpeg 9.0.1: a patch made
//! while its target is buffered moves the buffer's pointer alone; one made
//! after the target was flushed flushes, seeks and writes into the emptied
//! buffer, which the writer's next seek flushes before any later byte) —
//! as a write of its own, or at the head of a run that goes on past the
//! frontier, where the writer's seek back to its end stays inside the
//! buffer under `avio_seek`'s inclusive bound (272–280) and the bytes after
//! it leave with it: movenc's 64-bit `mdat` size over an empty payload
//! (R7). Such a run is split at the frontier, the part below judged as a
//! patch and the rest walked as an append. It is the same family as
//! `lhvC`'s rewind, which the buffer absorbs before any flush (the design
//! gate's finding 3): the buffer decides how a write reaches the sink, and
//! the walk follows what it flushes. So a size read as a placeholder is
//! resolved by the patch that follows its structure's last byte, a size
//! read as final is final, and the walks need no look-ahead.
//!
//! # What it cannot see
//!
//! - **Payload content.** The samples' bytes in the `mdat` are counted,
//!   never read: no census row changes a payload byte without changing a
//!   size the audit counts.
//! - **A configuration record's bytes.** Each sample description's record
//!   is judged by its presence and its length, as its writer makes it of
//!   the description's configuration ([`entry_expected`]): every road of
//!   the census's A4, A5 and A8 leaves a record out, empty or cut. `vvcC`'s
//!   and `evcC`'s length are not mirrored — each is judged longer than its
//!   own header, as its writer writes all or nothing — and `dec3`'s against
//!   its own fields. DVD subtitles' palette alone is judged by its content,
//!   the fold of its sixteen colours.
//! - **A record whose configuration rests on the trust model** (R8): where
//!   its writer's mirror stopped at a limit only a stream the standards do
//!   not allow passes, the record is judged not at all, neither its
//!   presence nor its length, as the muxer refused nothing there — `hvcC`
//!   and `lhvC` of an SPS of more than 64 short-term reference picture sets
//!   or of a set whose bits run past it, written empty and left out;
//!   `evcC` of an SPS of an id past 15 or a chroma format past 3, written
//!   empty; `vvcC` of an SPS of eight sub-layers, of what FFmpeg writes
//!   undefined, its parse indexing its arrays past their end; `dvc1` of a
//!   sequence header of another profile than the advanced one, left out;
//!   `lvcC` of a block or a picture the standard does not allow, written
//!   empty ([`records::rests_on_trust`]). FLV folds a track's sequence
//!   headers' records together: a track one of whose configurations rests
//!   there has its records judged by their count alone, which its header
//!   writes whatever the record ([`records::rests_on_trust_of`]).
//! - **VC-1's and TrueHD's sync flags.** movenc derives VC-1's from the
//!   bitstream after the fact, its scan of each packet able to clear every
//!   earlier sample's flag (`mov_parse_vc1_frame`, `movenc.c` 6475–6532):
//!   a VC-1 track's `stss` and `sdtp` are judged not at all. TrueHD's it
//!   reads from the bitstream too (7344–7348), and the MOV family refuses
//!   TrueHD at open. Every other track's flags are judged, each sample's as
//!   its packet reaches movenc. MOV's MPEG-2 is refused at open
//!   ([`mov_admitted`](super::mov_admitted)); its scan, `stps` and report
//!   row arrive with its own admission audit.
//! - **The bytes of the `udta`s and the `pssh` boxes.** Each is judged by
//!   its presence and its payload's length — a `pssh` box's in order,
//!   folded — as the census's roads leave each out whole (A1, A6, A7).
//! - **libavformat's own name in MOV's movie `udta`** — `©swr`, the
//!   context's `encoder` (`movenc.c` 5093): present, of exactly
//!   `LIBAVFORMAT_IDENT`, or absent, the `udta` judged either way (the
//!   owner's ruling on letter 1, (b): the tag stays outside the contract).
//! - **What the muxer does not mirror, and no census row writes:** chunk
//!   offsets and `stsc`, which libavformat's interleaving decides; `ctts`,
//!   edit lists — judged before the trailer, not read back — and the
//!   fields of `mvhd`, `tkhd` but its id, `mdhd` and `hdlr` but its type.
//! - **The count and the places of a `mov_text` track's end samples before
//!   its last** (R5): movenc writes one at a cue's end where another
//!   track's packet passes that end first, which libavformat's
//!   interleaving decides ([`CueTables`]). The rule that leaves the tables
//!   unchanged by them leaves two more things unseen, one in each table: a
//!   real cue of exactly 2 bytes — an empty cue — is folded out of the
//!   sample sizes, but kept by the time-to-sample table where its duration
//!   is not zero; and a real cue of zero duration is folded out of the
//!   time-to-sample table, but kept by the sample sizes. The trailer's end
//!   sample, the one whose loss movenc does not read, stays judged.
//! - **Deterministic behaviours the mirrors share.** The expectations are
//!   FFmpeg's success path: what FFmpeg does on every run alike, the
//!   muxer refuses by name before the packet, or expects.
//! - **FFmpeg's undefined arithmetic.** On the census's roads a null
//!   pointer and `-64` are summed before the omission (Q6): the audit names
//!   the outcome at `finish`; it cannot remove the arithmetic.
//! - **Memory faults**, which the muxer refuses or avoids as before.
//!
//! # What a finding names
//!
//! The first difference in a fixed order: the tap's own state — a rewrite
//! it could not follow, a file that ends inside a structure, a panicked
//! tap; then each track's own box, in the order the file holds them; then
//! each track's sample descriptions — the first that differs — its
//! tables, its sync samples and their dependencies and, of an animated
//! AVIF, its first chunk's offset, then its roll groups and its user data,
//! in track order; then AVIF's items'
//! records; then the file's parts: the media data, the movie's user data,
//! the protection headers, and AVIF's first item's location. A track's
//! omission is named before the file's sizes it shortens, so that the most
//! specific name wins.

use core::fmt;

use mediaencode::mediaspine::demuxer::TrackIndex;
use mediaio_ffmpeg::writer_guard::SinkTap;

use super::{
  MovMode, av1c_written, av1c_written_len,
  records::{self, ApvFrameInfo, Writer},
};

mod flac;
mod flv;
mod mkv;
mod mov;
#[cfg(test)]
mod tests;

use flac::FlacWalk;
pub(super) use flv::FlvTrack;
use flv::FlvWalk;
pub(super) use mkv::MkvTrack;
use mkv::MkvWalk;
use mov::{Described, MovWalk};

/// What FFmpeg wrote otherwise than it was handed, answering success: an
/// omission or a misreport (the census's premise rows), found by the write
/// audit once the trailer is written (findit-studio/mediaio#95).
///
/// Its parts say what is wrong, and where: the track, where the part is a
/// track's ([`Self::track`]), the part of the file ([`Self::part`]), and
/// what was found there against what was handed ([`Self::found`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WrittenOtherwise {
  track: Option<TrackIndex>,
  part: WrittenPart,
  found: Found,
}

impl WrittenOtherwise {
  pub(super) const fn new(track: Option<TrackIndex>, part: WrittenPart, found: Found) -> Self {
    Self { track, part, found }
  }

  /// The track the part is of, or `None` where it is the file's own: its
  /// structure, its media data, a track the writer makes itself.
  pub const fn track(&self) -> Option<TrackIndex> {
    self.track
  }

  /// The part of the file.
  pub const fn part(&self) -> WrittenPart {
    self.part
  }

  /// What was found there, against what was handed.
  pub const fn found(&self) -> Found {
    self.found
  }
}

impl fmt::Display for WrittenOtherwise {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    if let Some(track) = self.track {
      write!(f, "track {}: ", track.get())?;
    }
    write!(f, "{}: {}", self.part, self.found)
  }
}

impl std::error::Error for WrittenOtherwise {}

/// The part of a file a [`WrittenOtherwise`] names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum WrittenPart {
  /// The file's own structure: its boxes, as far as the walk could follow
  /// them, and how many tracks it holds.
  Structure,
  /// The media data: the MOV family's `mdat`, whose size counts the
  /// samples' bytes.
  MediaData,
  /// A track's own box: the MOV family's `trak`, and the track header's
  /// id in it.
  Track,
  /// The MOV family's chapter track, which movenc makes of the chapters
  /// itself and which has no track index of the muxer's.
  ChapterTrack,
  /// A track's sample description, the `index`-th its `stsd` holds, and
  /// the configuration it carries: the MOV family's record box — `avcC`,
  /// `hvcC` and `lhvC`, `vvcC`, `evcC`, `lvcC`, `av1C`, `dvc1`, `apvC` — or
  /// E-AC-3's `dec3` in MOV's `wave`; where the descriptions are counted
  /// otherwise than handed, the first one past the shorter count. FLV's: a
  /// track's sequence headers' records, the `index`-th — their lengths
  /// summed and folded, a difference named at the first, a count's at the
  /// first past the shorter.
  Configuration(u32),
  /// A DVD subtitle track's sample description, the `index`-th, and the
  /// palette its `esds` carries as its decoder-specific information.
  Palette(u32),
  /// AVIF's item of this number, from 1, as its `iloc` locates it: its
  /// extent's offset, against the payload's place.
  ItemLocation(u8),
  /// An animated AVIF track's chunk offsets: its first, against the
  /// payload's place.
  ChunkOffsets,
  /// A track's time-to-sample table: the MOV family's `stts`.
  TimeToSample,
  /// A track's sample sizes: the MOV family's `stsz`.
  SampleSizes,
  /// A track's sync samples: the MOV family's `stss`.
  SyncSamples,
  /// A track's samples' dependencies: the MOV family's `sdtp`.
  Dependencies,
  /// A track's frames: FLV's tags of coded frames, each its data size.
  Frames,
  /// A track's channel mask: FLAC's `WAVEFORMATEXTENSIBLE_CHANNEL_MASK`
  /// comment, which Matroska writes in the track's CodecPrivate and raw
  /// FLAC in its VORBIS_COMMENT.
  ChannelMask,
  /// A track's roll groups: the MOV family's `sgpd` and `sbgp`, AAC's one
  /// group of every sample or Opus's roll distances.
  RollGroups,
  /// A track's user data: MP4's track `udta`, the `kind` box of each DASH
  /// role its disposition names.
  TrackUserData,
  /// The movie's user data: the MOV family's `udta` of its `moov` — MP4's
  /// and iPod's iTunes metadata, their covers and the Nero chapter list;
  /// MOV's Nero list.
  MovieUserData,
  /// The protection headers: the MOV family's `pssh` boxes, each an
  /// encryption initialization of a stream's.
  ProtectionHeaders,
}

impl fmt::Display for WrittenPart {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let name = match self {
      Self::Structure => "the file's structure",
      Self::MediaData => "the media data",
      Self::Track => "its track box",
      Self::ChapterTrack => "the chapter track",
      Self::Configuration(index) => {
        return write!(f, "its configuration {index}");
      }
      Self::Palette(index) => return write!(f, "its sample description {index}'s palette"),
      Self::ItemLocation(item) => return write!(f, "item {item}'s location"),
      Self::ChunkOffsets => "its chunk offsets",
      Self::TimeToSample => "its time-to-sample table",
      Self::SampleSizes => "its sample sizes",
      Self::Frames => "its frames",
      Self::ChannelMask => "its channel mask",
      Self::SyncSamples => "its sync samples",
      Self::Dependencies => "its samples' dependencies",
      Self::RollGroups => "its roll groups",
      Self::TrackUserData => "its user data",
      Self::MovieUserData => "the movie's user data",
      Self::ProtectionHeaders => "the protection headers",
    };
    f.write_str(name)
  }
}

/// What a [`WrittenOtherwise`] found, against what was handed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Found {
  /// The part is not in the file.
  Omitted,
  /// A length in bytes: the part's, or the bytes a size counts.
  Length(Counts),
  /// A count: of samples, of runs, of tracks.
  Count(Counts),
  /// A value: an id, a field.
  Value(Counts),
  /// Lengths and counts agree, and the content does not: the folds of
  /// what was written and of what was handed differ.
  Content,
  /// The writer's bytes could not be followed: a rewrite of no field, a
  /// gap, a structure the walk does not know, a file that ends inside a
  /// structure, a position lost, a panicked tap ([`Unfollowed`]).
  Unfollowed(Unfollowed),
}

impl fmt::Display for Found {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Omitted => f.write_str("left out"),
      Self::Length(counts) => write!(
        f,
        "{} bytes written, {} handed",
        counts.written, counts.expected
      ),
      Self::Count(counts) | Self::Value(counts) => {
        write!(f, "{} written, {} handed", counts.written, counts.expected)
      }
      Self::Content => f.write_str("written otherwise than handed"),
      Self::Unfollowed(unfollowed) => unfollowed.fmt(f),
    }
  }
}

/// What was written, and what was handed: the two sides of a [`Found`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Counts {
  written: u64,
  expected: u64,
}

impl Counts {
  pub(super) const fn new(written: u64, expected: u64) -> Self {
    Self { written, expected }
  }

  /// What the file holds.
  pub const fn written(&self) -> u64 {
    self.written
  }

  /// What the muxer handed, and FFmpeg's success path writes of it.
  pub const fn expected(&self) -> u64 {
    self.expected
  }
}

/// Where the walk could not follow the writer's bytes, and why.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unfollowed {
  at: u64,
  why: Why,
}

impl Unfollowed {
  pub(super) const fn new(at: u64, why: Why) -> Self {
    Self { at, why }
  }

  /// The offset in the file: the run's, or the frontier's where the run
  /// lay past it or the file ended.
  pub const fn at(&self) -> u64 {
    self.at
  }

  /// Why.
  pub const fn why(&self) -> Why {
    self.why
  }
}

impl fmt::Display for Unfollowed {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "{} at {}", self.why, self.at)
  }
}

/// Why the walk could not follow the writer's bytes ([`Unfollowed`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Why {
  /// The writer went back over bytes it had written, where no field it
  /// patches lies.
  Rewrite,
  /// The writer skipped forward past the end of what it had written.
  Gap,
  /// A structure no writer of the format makes: a size past its parent's
  /// end, a size patch that closes no open structure, nesting too deep.
  Structure,
  /// The file ends inside a structure.
  Truncated,
  /// A field the writer patches was left part written at the end.
  Unresolved,
  /// The sink's position left libavformat's offsets, so the bytes after
  /// it could not be placed (`SinkTap::lost`).
  Position,
  /// The tap panicked, and saw no byte after it.
  Panicked,
}

impl fmt::Display for Why {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(match self {
      Self::Rewrite => "the writer went back over its own bytes",
      Self::Gap => "the writer skipped past its end",
      Self::Structure => "a structure no writer makes",
      Self::Truncated => "the file ends inside a structure",
      Self::Unresolved => "a field left part written",
      Self::Position => "the position was lost",
      Self::Panicked => "the audit's tap panicked",
    })
  }
}

/// The fold the walk and the muxer make of a sequence, the same function
/// on both sides: the state xored with the value, then splitmix64's
/// finalizer. Order-sensitive and not cryptographic — no input is chosen
/// to collide (the trust model) — it holds the content exact where the
/// counts and lengths compared before it already agree.
pub(super) const fn fold(state: u64, value: u64) -> u64 {
  let mut z = state ^ value;
  z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
  z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
  z ^ (z >> 31)
}

/// The fold of an empty sequence: not zero, so that zeros folded from it
/// move it.
const FOLD_SEED: u64 = 0x6A09_E667_F3BC_C908;

/// The fold of a palette's colours, each a word: the muxer's of the colours
/// movenc converts, the walk's — the same — of the words the `esds`'s
/// decoder-specific information holds.
pub(super) fn palette_fold(colours: impl IntoIterator<Item = u32>) -> u64 {
  colours
    .into_iter()
    .fold(FOLD_SEED, |state, colour| fold(state, u64::from(colour)))
}

/// What [`CueTables`] folds for the trailer's end sample of a `mov_text`
/// track, where the track's last sample is one.
const END_MARK: u64 = 0x2;

/// A sequence of values, as runs of equal ones: their count, their sum, and
/// the fold of the runs — each value, then its run's length — closed as a
/// value of another follows. The walk feeds it a table's entries as it
/// reads them, a run at a time; the muxer each sample as movenc writes it.
/// A run of `count` values costs one step, so that a table written as one
/// run — constant-size audio's sample size, its time-to-sample run — is
/// never expanded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Runs {
  fold: u64,
  open: Option<(u64, u64)>,
  count: u64,
  sum: u128,
}

impl Default for Runs {
  fn default() -> Self {
    Self {
      fold: FOLD_SEED,
      open: None,
      count: 0,
      sum: 0,
    }
  }
}

impl Runs {
  /// `count` values of `value`, after those pushed before.
  pub(super) fn push(&mut self, value: u64, count: u64) {
    if count == 0 {
      return;
    }
    self.count = self.count.saturating_add(count);
    self.sum = self
      .sum
      .saturating_add(u128::from(value) * u128::from(count));
    match &mut self.open {
      Some((open, length)) if *open == value => *length = length.saturating_add(count),
      open => {
        if let Some((closed, length)) = open.replace((value, count)) {
          self.fold = fold(fold(self.fold, closed), length);
        }
      }
    }
  }

  /// The values' count, their sum, and the fold of their runs, the open
  /// one closed.
  pub(super) fn summary(&self) -> Summary {
    Summary {
      count: self.count,
      sum: self.sum,
      fold: match self.open {
        Some((value, length)) => fold(fold(self.fold, value), length),
        None => self.fold,
      },
    }
  }
}

/// What a table comes to, compared field by field: its values' count and
/// sum, then the fold of their runs ([`Runs`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Summary {
  count: u64,
  sum: u128,
  fold: u64,
}

impl Summary {
  /// The values' count: the laws' seam.
  #[cfg(feature = "test-support")]
  pub(super) const fn count(self) -> u64 {
    self.count
  }

  /// The values' sum, as a length: saturated where no file's could reach.
  pub(super) fn sum(self) -> u64 {
    u64::try_from(self.sum).unwrap_or(u64::MAX)
  }
}

/// A `mov_text` track's tables as R5 reads them (the coordinator's ruling
/// on the end samples, findit-studio/mediaio#95).
///
/// movenc writes a 2-byte, zero-duration sample at a cue's end before any
/// packet of any track whose decode time, counted in its own ticks, passes
/// that end, where the track's last sample is not one already (`movenc.c`
/// 7646–7653 in FFmpeg 9.0.1). Before a cue that starts exactly at the last
/// one's end, the ordinary case, whether one is written turns on which
/// packet libavformat's interleaving hands movenc first, which this muxer
/// does not mirror: beside a video track the end samples come at each such
/// boundary but the first, alone at none. So both sides fold a cue track's
/// tables by one rule, which leaves them unchanged by those samples:
///
/// - **the time-to-sample table:** runs of zero duration dropped, and the
///   runs around them merged ([`Self::time`]);
/// - **the sample sizes:** those other than 2 counted, summed and folded
///   ([`Self::size`]);
/// - **and the trailer's end:** the last sample is the trailer's end sample
///   — its duration zero, its size 2 — counted once in each table.
///
/// The walk calls the two functions on the tables it reads; the muxer on
/// each sample it expects. The trailer's end sample, the one end sample
/// whose write movenc does not read the answer of (9025–9031), stays judged
/// exactly: where it is lost, the last cue is the last sample, and the
/// counted samples fall short by one (A10). What the rule leaves unseen
/// is named in the module's census: the count and the places of the end
/// samples before the last, a cue of exactly 2 bytes in the sizes, and a
/// cue of zero duration in the time-to-sample table.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct CueTables {
  times: Runs,
  sizes: Runs,
  last_duration: Option<u64>,
  last_size: Option<u64>,
}

impl CueTables {
  /// R5's rule for the time-to-sample table: `count` samples of `duration`
  /// ticks — a run of zero duration dropped, so that the runs around it
  /// merge.
  pub(super) fn time(&mut self, count: u64, duration: u64) {
    if count == 0 {
      return;
    }
    self.last_duration = Some(duration);
    if duration != 0 {
      self.times.push(duration, count);
    }
  }

  /// R5's rule for the sample sizes: `count` samples of `size` bytes — a
  /// size of 2 dropped.
  pub(super) fn size(&mut self, size: u64, count: u64) {
    if count == 0 {
      return;
    }
    self.last_size = Some(size);
    if size != 2 {
      self.sizes.push(size, count);
    }
  }

  /// The time-to-sample table as judged: the runs of nonzero duration, and
  /// the last sample where its duration is zero — the trailer's end.
  pub(super) fn times(&self) -> Summary {
    Self::ended(self.times.summary(), self.last_duration == Some(0), 0)
  }

  /// The sample sizes as judged: those other than 2, and the last sample
  /// where it is of 2 bytes — the trailer's end.
  pub(super) fn sizes(&self) -> Summary {
    Self::ended(self.sizes.summary(), self.last_size == Some(2), 2)
  }

  /// `summary`, the trailer's end sample counted once where `ends`.
  fn ended(summary: Summary, ends: bool, value: u64) -> Summary {
    if !ends {
      return summary;
    }
    Summary {
      count: summary.count.saturating_add(1),
      sum: summary.sum.saturating_add(u128::from(value)),
      fold: fold(summary.fold, END_MARK),
    }
  }
}

/// The tap the muxer installs over a written file: the walk of its format,
/// fed every run the sink takes ([`SinkTap`]), which stops at the first
/// run it cannot follow.
pub(super) struct Audit {
  /// The bytes appended so far, in libavformat's offsets.
  frontier: u64,
  /// The first run the walk could not follow; nothing is walked after it.
  unfollowed: Option<Unfollowed>,
  walk: Walk,
}

/// The walk of a format.
enum Walk {
  /// The MOV family's boxes.
  Mov(MovWalk),
  /// FLV's tags.
  Flv(FlvWalk),
  /// Matroska's and WebM's elements.
  Mkv(MkvWalk),
  /// Raw FLAC's metadata blocks.
  Flac(FlacWalk),
}

impl Audit {
  /// A tap over a file of the MOV family in `mode`, of `tracks` tracks at
  /// most beside movenc's own: its tables are allocated here, at open, and
  /// never grow.
  pub(super) fn mov(mode: MovMode, tracks: usize) -> Self {
    Self {
      frontier: 0,
      unfollowed: None,
      walk: Walk::Mov(MovWalk::new(mode, tracks)),
    }
  }

  /// A tap over an FLV file of `tracks` and of `pre_tags` VP6A pre-tags
  /// (R11): its table allocated here, at open.
  pub(super) fn flv(tracks: &[FlvTrack], pre_tags: usize) -> Self {
    Self {
      frontier: 0,
      unfollowed: None,
      walk: Walk::Flv(FlvWalk::new(tracks, pre_tags)),
    }
  }

  /// A tap over a Matroska or WebM file of `tracks`, in TrackEntry order:
  /// its table allocated here, at open.
  pub(super) fn mkv(tracks: &[MkvTrack]) -> Self {
    Self {
      frontier: 0,
      unfollowed: None,
      walk: Walk::Mkv(MkvWalk::new(tracks)),
    }
  }

  /// A tap over a raw FLAC file.
  pub(super) const fn flac() -> Self {
    Self {
      frontier: 0,
      unfollowed: None,
      walk: Walk::Flac(FlacWalk::new()),
    }
  }

  /// The bytes appended so far.
  pub(super) const fn frontier(&self) -> u64 {
    self.frontier
  }

  /// The walk's own state as a finding, where it is one: a run it could not
  /// follow, or a file that does not end where its format's walk ends.
  pub(super) fn unfollowed(&self) -> Option<Unfollowed> {
    if let Some(unfollowed) = self.unfollowed {
      return Some(unfollowed);
    }
    match &self.walk {
      Walk::Mov(walk) => walk.unfinished(self.frontier),
      Walk::Flv(walk) => walk.unfinished(self.frontier),
      Walk::Mkv(walk) => walk.unfinished(self.frontier),
      Walk::Flac(walk) => walk.unfinished(self.frontier),
    }
  }

  /// The MOV walk, where this tap walks the MOV family.
  pub(super) fn mov_walk(&self) -> Option<&MovWalk> {
    match &self.walk {
      Walk::Mov(walk) => Some(walk),
      Walk::Flv(_) | Walk::Mkv(_) | Walk::Flac(_) => None,
    }
  }

  /// The FLV walk, where this tap walks FLV.
  pub(super) fn flv_walk(&self) -> Option<&FlvWalk> {
    match &self.walk {
      Walk::Flv(walk) => Some(walk),
      Walk::Mov(_) | Walk::Mkv(_) | Walk::Flac(_) => None,
    }
  }

  /// The Matroska walk, where this tap walks Matroska or WebM.
  pub(super) fn mkv_walk(&self) -> Option<&MkvWalk> {
    match &self.walk {
      Walk::Mkv(walk) => Some(walk),
      Walk::Mov(_) | Walk::Flv(_) | Walk::Flac(_) => None,
    }
  }

  /// The raw FLAC walk, where this tap walks raw FLAC.
  pub(super) fn flac_walk(&self) -> Option<&FlacWalk> {
    match &self.walk {
      Walk::Flac(walk) => Some(walk),
      Walk::Mov(_) | Walk::Flv(_) | Walk::Mkv(_) => None,
    }
  }

  /// Takes the sample descriptions of the `place`-th `trak` the file
  /// holds, which the muxer moves here before the trailer writes them (R6).
  pub(super) fn hand(&mut self, place: usize, handed: Handed) {
    if let Walk::Mov(walk) = &mut self.walk {
      walk.hand(place, handed);
    }
  }
}

impl SinkTap for Audit {
  fn took(&mut self, at: u64, bytes: &[u8]) {
    if self.unfollowed.is_some() {
      return;
    }
    let end = at.saturating_add(bytes.len() as u64);
    let walked = if at == self.frontier {
      let appended = match &mut self.walk {
        Walk::Mov(walk) => walk.append(at, bytes),
        Walk::Flv(walk) => walk.append(at, bytes),
        Walk::Mkv(walk) => walk.append(at, bytes),
        Walk::Flac(walk) => walk.append(at, bytes),
      };
      self.frontier = end;
      appended
    } else if end <= self.frontier {
      match &mut self.walk {
        Walk::Mov(walk) => walk.patch(self.frontier, at, bytes),
        Walk::Flv(walk) => walk.patch(self.frontier, at, bytes),
        Walk::Mkv(walk) => walk.patch(self.frontier, at, bytes),
        Walk::Flac(walk) => walk.patch(self.frontier, at, bytes),
      }
    } else if at > self.frontier {
      Err(Unfollowed::new(self.frontier, Why::Gap))
    } else {
      // A patch at the head of a run that goes on past the frontier (R7):
      // the part below, a patch; the rest, an append.
      let below = usize::try_from(self.frontier - at).unwrap_or(bytes.len());
      let (patched, appended) = bytes.split_at(below.min(bytes.len()));
      let frontier = self.frontier;
      let walked = match &mut self.walk {
        Walk::Mov(walk) => walk
          .patch(frontier, at, patched)
          .and_then(|()| walk.append(frontier, appended)),
        Walk::Flv(walk) => walk
          .patch(frontier, at, patched)
          .and_then(|()| walk.append(frontier, appended)),
        Walk::Mkv(walk) => walk
          .patch(frontier, at, patched)
          .and_then(|()| walk.append(frontier, appended)),
        Walk::Flac(walk) => walk
          .patch(frontier, at, patched)
          .and_then(|()| walk.append(frontier, appended)),
      };
      self.frontier = end;
      walked
    };
    if let Err(unfollowed) = walked {
      self.unfollowed = Some(unfollowed);
    }
  }

  fn lost(&mut self) {
    self
      .unfollowed
      .get_or_insert(Unfollowed::new(self.frontier, Why::Position));
  }
}

/// What the muxer expects of one track's sample tables in the MOV family,
/// kept at each sample movenc writes — into fields of the track's own,
/// allocating nothing (note §2.7).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct SampleMirror {
  times: Runs,
  sizes: Runs,
  cues: CueTables,
  /// The decode time of the last sample, whose duration the next one's
  /// decode time sets (`get_cluster_duration`, `movenc.c` 1240–1258).
  last: Option<i64>,
  /// Whether the track's tables are read by R5's rule ([`CueTables`]).
  cue_track: bool,
}

impl SampleMirror {
  /// The mirror of a `mov_text` track, whose tables R5's rule reads.
  pub(super) fn cue_track() -> Self {
    Self {
      cue_track: true,
      ..Self::default()
    }
  }

  /// A sample movenc writes at `dts`, of `bytes`: the last sample's
  /// duration is now known, the distance to it.
  pub(super) fn sample(&mut self, dts: i64, bytes: u64) {
    if let Some(last) = self.last.replace(dts) {
      // movenc keeps each distance in an `int`, checked within
      // `[0, INT_MAX]` (1252–1254), and writes it in 32 bits; the muxer
      // refuses a packet past either before it is written.
      self.time(1, u64::from(dts.wrapping_sub(last) as u32));
    }
    if self.cue_track {
      self.cues.size(bytes, 1);
    } else {
      self.sizes.push(bytes, 1);
    }
  }

  /// The track's end: the last sample's duration, in ticks —
  /// `track_duration` and the track's start less its decode time
  /// (1248–1249), an `int` written in 32 bits.
  pub(super) fn end(&mut self, duration: i64) {
    if self.last.is_some() {
      self.time(1, u64::from(duration as u32));
    }
  }

  /// Constant-size audio's tables, each one run (`mov_write_stts_tag`,
  /// 3248–3256; `mov_write_stsz_tag`, 225–259): `samples` samples of
  /// `size` bytes, each of one tick.
  pub(super) fn constant(&mut self, size: u64, samples: u64) {
    self.times.push(1, samples);
    self.sizes.push(size, samples);
  }

  /// The decode time of the last sample, if any.
  pub(super) const fn last(&self) -> Option<i64> {
    self.last
  }

  fn time(&mut self, count: u64, duration: u64) {
    if self.cue_track {
      self.cues.time(count, duration);
    } else {
      self.times.push(duration, count);
    }
  }

  /// The time-to-sample table, as the verdict compares it.
  pub(super) fn times(&self) -> Summary {
    if self.cue_track {
      self.cues.times()
    } else {
      self.times.summary()
    }
  }

  /// The sample sizes, as the verdict compares them.
  pub(super) fn sizes(&self) -> Summary {
    if self.cue_track {
      self.cues.sizes()
    } else {
      self.sizes.summary()
    }
  }

  /// Whether the track's tables are read by R5's rule.
  pub(super) const fn reads_cues(&self) -> bool {
    self.cue_track
  }

  /// The bytes of the samples movenc writes, as the sample sizes count
  /// them: AVIF's payload.
  pub(super) fn bytes(&self) -> u64 {
    self.sizes().sum()
  }

  /// The samples movenc writes, as the sample sizes count them.
  pub(super) fn count(&self) -> u64 {
    self.sizes().count
  }
}

/// A track's sync and dependency flags as compared, each table `None`
/// where movenc writes none: `stss`'s sync samples, each its index from 1,
/// and `sdtp`'s byte of each sample (`mov_write_stss_tag`,
/// `mov_write_sdtp_tag`, `movenc.c` 292–336 in FFmpeg 9.0.1), each folded
/// ([`Runs`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct FlagTables {
  sync: Option<Summary>,
  dependencies: Option<Summary>,
}

/// What the muxer counts of a track's flags, sample by sample, as movenc
/// sets them ([`FlagTables`]). Fields of the track's own, allocating
/// nothing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct FlagsMirror {
  samples: u64,
  sync: Runs,
  dependencies: Runs,
  disposable: u64,
}

impl FlagsMirror {
  /// A sample movenc writes, a sync sample where `sync` — its packet
  /// reaching movenc marked a key frame (7344–7359) — and disposable where
  /// `disposable` (7360–7363): its index listed in `stss` where a sync
  /// sample, and its `sdtp` byte — depending on others but where it is a
  /// sync sample, referenced by none where it is disposable (316–336).
  pub(super) fn sample(&mut self, sync: bool, disposable: bool) {
    self.samples += 1;
    if sync {
      self.sync.push(self.samples, 1);
    }
    let dependent: u64 = if sync { 2 } else { 1 };
    let referenced: u64 = if disposable { 2 } else { 0 };
    self.dependencies.push(dependent << 4 | referenced << 2, 1);
    self.disposable += u64::from(disposable);
  }

  /// The tables movenc writes of the samples (3412–3420): `stss` of a track
  /// whose kind writes one (`syncs`: video, MPEG-H 3D Audio, USAC AAC)
  /// where some but not all samples are sync samples; `sdtp` of `video`
  /// where any sample is disposable.
  pub(super) fn written(&self, syncs: bool, video: bool) -> FlagTables {
    let sync = self.sync.summary();
    FlagTables {
      sync: (syncs && sync.count > 0 && sync.count < self.samples).then_some(sync),
      dependencies: (video && self.disposable > 0).then(|| self.dependencies.summary()),
    }
  }
}

/// The first way a flag table as read differs from `expected`, `None` where
/// movenc writes none: written where none is, left out, its entries'
/// count, then their content.
fn flags_found(expected: Option<Summary>, written: Option<Summary>) -> Option<Found> {
  match (expected, written) {
    (None, None) => None,
    (None, Some(written)) => Some(Found::Count(Counts::new(written.count, 0))),
    (Some(_), None) => Some(Found::Omitted),
    (Some(wanted), Some(written)) if written.count != wanted.count => {
      Some(Found::Count(Counts::new(written.count, wanted.count)))
    }
    (Some(wanted), Some(written)) => (written != wanted).then_some(Found::Content),
  }
}

/// A track's roll groups as compared (`mov_preroll_write_stbl_atoms`,
/// `movenc.c` 3305–3400 in FFmpeg 9.0.1): the groups `sgpd` holds — one a
/// run of samples of a roll distance other than none, its distance
/// written negated in 16 bits — and the runs `sbgp` holds, each its
/// samples and its group's index, none of a run of no distance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct RollGroups {
  /// `sgpd`'s entries, each a value ([`Runs`]).
  distances: Summary,
  /// `sbgp`'s entries.
  runs: u64,
  /// `sbgp`'s samples, by their group's index ([`Runs`]).
  samples: Summary,
}

impl RollGroups {
  /// What the walk read: `sgpd`'s entries, and `sbgp`'s count and samples.
  pub(super) const fn read(distances: Summary, runs: u64, samples: Summary) -> Self {
    Self {
      distances,
      runs,
      samples,
    }
  }

  /// The groups `sgpd` holds: the laws' seam.
  #[cfg(feature = "test-support")]
  pub(super) const fn groups(&self) -> u64 {
    self.distances.count
  }

  /// The runs `sbgp` holds: the laws' seam.
  #[cfg(feature = "test-support")]
  pub(super) const fn runs(&self) -> u64 {
    self.runs
  }
}

/// What the muxer counts of a track's roll groups, sample by sample, as
/// movenc counts them ([`RollGroups`]): a run of samples of one distance
/// each, a new run of a distance other than none taking the next group's
/// index — of a distance seen before as well (3352–3358) — and one of none
/// the index 0. Fields of the track's own, allocating nothing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct RollMirror {
  distances: Runs,
  groups: u64,
  runs: u64,
  samples: Runs,
  /// The open run's distance.
  open: Option<u16>,
}

impl RollMirror {
  /// `count` samples of a roll distance of `distance`, after those before:
  /// AAC's every sample, of a distance of 1 (3359–3366); Opus's each.
  pub(super) fn samples(&mut self, distance: u16, count: u64) {
    if count == 0 {
      return;
    }
    if self.open != Some(distance) {
      self.open = Some(distance);
      self.runs += 1;
      if distance != 0 {
        self.groups += 1;
        // `avio_wb16` of the distance negated (3383).
        self.distances.push(u64::from(distance.wrapping_neg()), 1);
      }
    }
    let index = if distance == 0 { 0 } else { self.groups };
    self.samples.push(index, count);
  }

  /// The groups as movenc writes them: `None` where it writes neither box —
  /// every distance none, or no sample (3323, 3369–3372).
  pub(super) fn written(&self) -> Option<RollGroups> {
    (self.groups > 0).then(|| RollGroups {
      distances: self.distances.summary(),
      runs: self.runs,
      samples: self.samples.summary(),
    })
  }
}

/// What a track's sample descriptions carry that the audit judges, by its
/// codec and the configuration writer movenc calls for it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum Carried {
  /// Nothing but their count.
  #[default]
  Count,
  /// The record box the writer makes of each description's configuration
  /// (`movenc.c` 1573–1738, 1102–1201; [`Writer`]).
  Record(Writer),
  /// APV's `apvC`, of the track's frame informations, in each description
  /// alike (1725–1738; `apv.c` 145–203).
  Apv,
  /// E-AC-3's `dec3`, in MOV's `wave` (1059–1100; 612–666).
  Dec3,
  /// DVD subtitles' palette: its sixteen colours, as movenc converts them
  /// (`mov_create_dvd_sub_decoder_specific_info`, 8000–8048).
  Palette([u32; 16]),
  /// AVIF's AV1, whose `av1C` — of its item, and of an animated image's
  /// sample entry — `ff_isom_write_av1c` writes without the sequence
  /// header OBU (`mov_write_av1c_tag`, 1572–1581).
  ItemAv1c,
}

/// A track's sample descriptions as the muxer hands them to the tap before
/// the trailer writes them (R6): moved, nothing copied and nothing
/// allocated, and read by nothing else after it.
#[derive(Debug, Default)]
pub(super) struct Handed {
  carried: Carried,
  /// Each description's configuration as movenc keeps it, `None` of the
  /// one it makes itself: DVD subtitles' first.
  entries: Vec<Option<Vec<u8>>>,
  /// APV's frame informations, by PBU type.
  apv: Vec<(u8, Vec<ApvFrameInfo>)>,
  /// How many descriptions movenc writes.
  count: usize,
}

impl Handed {
  /// A track's descriptions, as the muxer kept them.
  pub(super) fn new(
    carried: Carried,
    entries: Vec<Option<Vec<u8>>>,
    apv: Vec<(u8, Vec<ApvFrameInfo>)>,
  ) -> Self {
    Self {
      carried,
      count: entries.len(),
      entries,
      apv,
    }
  }

  /// The chapter track's: its one text description, judged by its count.
  pub(super) fn chapter_track() -> Self {
    Self {
      count: 1,
      ..Self::default()
    }
  }

  /// How many descriptions movenc writes.
  pub(super) const fn count(&self) -> usize {
    self.count
  }

  /// What the `index`-th description is expected to carry.
  pub(super) fn entry(&self, index: usize) -> EntryExpected {
    entry_expected(
      self.carried,
      self.entries.get(index).and_then(Option::as_deref),
      &self.apv,
    )
  }
}

/// What a sample description is expected to carry ([`entry_expected`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct EntryExpected {
  /// Its record box, where one is judged.
  record: Option<RecordExpected>,
  /// `lhvC` beside its `hvcC`, judged of every HEVC description: its
  /// payload's bytes where movenc writes one — a stream flagged multilayer,
  /// of a configuration `ff_isom_write_lhvc` writes — `None` where none.
  layers: Option<Option<u64>>,
  /// DVD subtitles' palette, its fold.
  palette: Option<u64>,
}

impl EntryExpected {
  /// Its record box's FourCC and payload's bytes, where they are mirrored
  /// exactly; and `lhvC`'s beside it: the laws' seam.
  #[cfg(feature = "test-support")]
  pub(super) fn records(self) -> impl Iterator<Item = ([u8; 4], u64)> {
    let record = match self.record {
      Some(RecordExpected::Exact(fourcc, len)) => Some((fourcc, len)),
      _ => None,
    };
    record
      .into_iter()
      .chain(self.layers.flatten().map(|len| (*b"lhvC", len)))
  }
}

/// A record box, as a sample description is expected to carry it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RecordExpected {
  /// A box `fourcc` whose payload is `len` bytes.
  Exact([u8; 4], u64),
  /// A box `fourcc` whose payload is longer than `len` bytes, its own
  /// header's: of a writer that writes its record whole or not at all, its
  /// length not mirrored (`vvc.c` 879–936, `evc.c` 298–385; the design
  /// gate's condition 1).
  Longer([u8; 4], u64),
  /// E-AC-3's `dec3`, as long as its own fields say.
  Dec3,
}

/// What a sample description of a track whose descriptions carry
/// `carried`, of the configuration `config` — `None` of the one movenc
/// makes itself — is expected to carry, of a track of the APV frame
/// informations `apv`: its record as the writer writes the configuration,
/// which the muxer took only where the writer writes it whole (table C′′′
/// of the census, [`Writer::writes`]) — `avcC`, `hvcC` and `lhvC` beside
/// it, `lvcC`, `av1C` as their writers make them, `dvc1` of its seven
/// bytes and the configuration (1186–1201), of a configuration at all;
/// `vvcC` and `evcC` longer than their header; `apvC` of its header's four
/// bytes and the frame informations' record; E-AC-3's `dec3`; DVD
/// subtitles' palette — and nothing of a record whose writer's mirror
/// stopped at the trust model's limit ([`records::rests_on_trust`]; R8). The walk calls it of each entry as the entry's last
/// byte is read, the laws' seam of each description the muxer holds: one
/// function, so that no mirror is copied (R6).
pub(super) fn entry_expected(
  carried: Carried,
  config: Option<&[u8]>,
  apv: &[(u8, Vec<ApvFrameInfo>)],
) -> EntryExpected {
  let exact = |fourcc: &[u8; 4], len: u64| Some(RecordExpected::Exact(*fourcc, len));
  let mut expected = EntryExpected::default();
  match (carried, config) {
    (Carried::Count, _) | (Carried::Record(_), None) => {}
    // A record that rests on the trust model: judged not at all (R8).
    (Carried::Record(writer), Some(config)) if records::rests_on_trust(writer, config) => {}
    (Carried::Apv, _) => expected.record = exact(b"apvC", 4 + records::apvc_written_len(apv)),
    (Carried::Dec3, _) => expected.record = Some(RecordExpected::Dec3),
    (Carried::Palette(colours), _) => expected.palette = Some(palette_fold(colours)),
    (Carried::ItemAv1c, None) => {}
    (Carried::ItemAv1c, Some(config)) => {
      expected.record = exact(b"av1C", av1c_record_len(config, false));
    }
    (Carried::Record(writer), Some(config)) => {
      expected.record = match writer {
        Writer::Avcc => exact(b"avcC", records::avcc_written_len(config)),
        Writer::Hvcc { complete, lhvc } => {
          expected.layers = Some(
            (lhvc && records::lhvc(config, complete).whole())
              .then(|| records::lhvc_written_len(config)),
          );
          exact(b"hvcC", records::hvcc_written_len(config))
        }
        Writer::Vvcc => Some(RecordExpected::Longer(*b"vvcC", 4)),
        Writer::Evcc => Some(RecordExpected::Longer(*b"evcC", 0)),
        // A configuration whose units rest on the trust model, whose
        // length is none of the mirror's: judged by nothing.
        Writer::Lvcc => records::lvcc_written_len(config).and_then(|len| exact(b"lvcC", len)),
        Writer::Av1c => exact(b"av1C", av1c_record_len(config, true)),
        Writer::Dvc1 if config.is_empty() => None,
        Writer::Dvc1 => exact(b"dvc1", 7 + config.len() as u64),
        Writer::Vpcc { .. } | Writer::Av3c => None,
      };
    }
  }
  expected
}

/// The bytes `ff_isom_write_av1c` writes of `config`, its sequence header
/// OBU among them where `sequence` (`av1.c` 398–502): none of one it fails
/// on ([`av1c_written`]).
fn av1c_record_len(config: &[u8], sequence: bool) -> u64 {
  if av1c_written(config) {
    av1c_written_len(config, sequence) as u64
  } else {
    0
  }
}

/// A track's `trak` as the muxer expects it, in the order movenc writes
/// them: a track of the muxer's, or movenc's chapter track.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct ExpectedTrak {
  /// The muxer's track, or `None` for the chapter track.
  pub(super) track: Option<TrackIndex>,
  /// The track header's id: the stream's index and one; the chapter
  /// track's, the streams' count and one (`mov_setup_track_ids`, `movenc.c`
  /// 5270–5279).
  pub(super) id: u32,
  /// Its sample tables.
  pub(super) samples: SampleMirror,
  /// Its flag tables; `None` where they are unjudged, VC-1's.
  pub(super) flags: Option<FlagTables>,
  /// Its roll groups, where movenc writes them (A3).
  pub(super) rolls: Option<RollGroups>,
  /// Its `udta`'s payload bytes, 0 where movenc writes none (A6).
  pub(super) user_data: u64,
}

/// The movie's `udta` as the muxer expects it, by the mode
/// (`mov_write_udta_tag`, `movenc.c` 5066–5122): its payload's bytes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum MovieUserData {
  /// None: AVIF's `moov` holds none (5446–5447), a still AVIF no `moov`.
  #[default]
  None,
  /// MP4's and iPod's: exactly these bytes — the iTunes metadata, the
  /// covers and the Nero list.
  Exact(u64),
  /// MOV's: the Nero list's `len` bytes, and beside them libavformat's
  /// name in `©swr`, its `name` bytes, or not — the owner's ruling on
  /// letter 1, (b) — none at all where both are none.
  NameOptional { len: u64, name: u64 },
}

/// The movie's parts as the muxer expects them ([`mov_verdict`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct ExpectedMovie {
  /// AVIF's payload: the bytes movenc writes of its track's samples;
  /// `None` of MP4, MOV and iPod, whose `mdat`'s size is judged against
  /// the payload the sink took.
  pub(super) media: Option<u64>,
  /// The movie's `udta` (A7).
  pub(super) user_data: MovieUserData,
  /// The `pssh` boxes, each its payload's bytes, in the order movenc
  /// writes them (`mov_write_pssh_tag`, 5175–5210; 5449–5451; A1).
  pub(super) protection: Runs,
}

/// The first way a track's roll groups as read — `sgpd`'s entries, and
/// `sbgp`'s count and samples — differ from `expected`: their count, the
/// runs', the samples' the runs cover, then their content; one box of the
/// two, the other left out.
fn rolls_found(
  expected: Option<RollGroups>,
  (groups, runs): (Option<Summary>, Option<(u64, Summary)>),
) -> Option<Found> {
  let written = match (groups, runs) {
    (None, None) => None,
    (Some(distances), Some((runs, samples))) => Some(RollGroups::read(distances, runs, samples)),
    _ if expected.is_none() => return Some(Found::Count(Counts::new(1, 0))),
    _ => return Some(Found::Omitted),
  };
  let count = |written: u64, wanted: u64| Found::Count(Counts::new(written, wanted));
  match (expected, written) {
    (None, None) => None,
    (None, Some(written)) => Some(count(written.distances.count, 0)),
    (Some(_), None) => Some(Found::Omitted),
    (Some(wanted), Some(written)) if written.distances.count != wanted.distances.count => {
      Some(count(written.distances.count, wanted.distances.count))
    }
    (Some(wanted), Some(written)) if written.runs != wanted.runs => {
      Some(count(written.runs, wanted.runs))
    }
    (Some(wanted), Some(written)) if written.samples.count != wanted.samples.count => {
      Some(count(written.samples.count, wanted.samples.count))
    }
    (Some(wanted), Some(written)) => (written != wanted).then_some(Found::Content),
  }
}

/// The first way a `udta` as read, its payload's bytes, differs from
/// `expected`, 0 where none is: written where none is expected, left out,
/// or of another length.
fn user_data_found(expected: u64, written: Option<u64>) -> Option<Found> {
  match (expected, written) {
    (0, None) => None,
    (0, Some(_)) => Some(Found::Count(Counts::new(1, 0))),
    (_, None) => Some(Found::Omitted),
    (expected, Some(written)) => {
      (written != expected).then(|| Found::Length(Counts::new(written, expected)))
    }
  }
}

/// The first way the movie's `udta` as read differs from `expected`
/// ([`MovieUserData`]).
fn movie_user_data_found(expected: MovieUserData, written: Option<u64>) -> Option<Found> {
  match (expected, written) {
    (MovieUserData::None, written) => user_data_found(0, written),
    (MovieUserData::Exact(len), written) => user_data_found(len, written),
    (MovieUserData::NameOptional { len, .. }, None) => (len > 0).then_some(Found::Omitted),
    (MovieUserData::NameOptional { len, name }, Some(written)) => (written != len
      && written != len + name)
      .then(|| Found::Length(Counts::new(written, len + name))),
  }
}

/// The first difference between what the MOV walk read and what the muxer
/// expects, in the verdict's order (see [the module](self)).
pub(super) fn mov_verdict(
  audit: &Audit,
  expected: impl Iterator<Item = ExpectedTrak> + Clone,
  movie: &ExpectedMovie,
) -> Option<WrittenOtherwise> {
  let media = movie.media;
  if let Some(unfollowed) = audit.unfollowed() {
    return Some(WrittenOtherwise::new(
      None,
      WrittenPart::Structure,
      Found::Unfollowed(unfollowed),
    ));
  }
  let walk = audit.mov_walk()?;
  let traks = walk.traks();
  // Each track's own box, in the order the file holds them.
  let mut wanted = 0;
  for (index, trak) in expected.clone().enumerate() {
    wanted += 1;
    let part = if trak.track.is_some() {
      WrittenPart::Track
    } else {
      WrittenPart::ChapterTrack
    };
    let Some(seen) = traks.get(index) else {
      return Some(WrittenOtherwise::new(trak.track, part, Found::Omitted));
    };
    if seen.id() != Some(trak.id) {
      return Some(WrittenOtherwise::new(
        trak.track,
        part,
        Found::Value(Counts::new(
          u64::from(seen.id().unwrap_or(0)),
          u64::from(trak.id),
        )),
      ));
    }
  }
  if walk.traks_seen() != wanted {
    return Some(WrittenOtherwise::new(
      None,
      WrittenPart::Structure,
      Found::Count(Counts::new(walk.traks_seen() as u64, wanted as u64)),
    ));
  }
  // Each track's descriptions, then its tables, in track order.
  for (trak, seen) in expected.zip(traks) {
    if let Some((index, described, found)) = seen.descriptions() {
      let part = match (trak.track, described) {
        (None, _) => WrittenPart::ChapterTrack,
        (Some(_), Described::Configuration) => WrittenPart::Configuration(index),
        (Some(_), Described::Palette) => WrittenPart::Palette(index),
      };
      return Some(WrittenOtherwise::new(trak.track, part, found));
    }
    let cues = trak.samples.reads_cues();
    let table = |part, written: Option<Summary>, wanted: Summary| {
      let Some(written) = written else {
        return Some(Found::Omitted);
      };
      if written.count != wanted.count {
        return Some(Found::Count(Counts::new(written.count, wanted.count)));
      }
      if part == WrittenPart::SampleSizes && written.sum != wanted.sum {
        return Some(Found::Length(Counts::new(written.sum(), wanted.sum())));
      }
      (written.fold != wanted.fold).then_some(Found::Content)
    };
    let tables = [
      (
        WrittenPart::TimeToSample,
        seen.times(cues),
        trak.samples.times(),
      ),
      (
        WrittenPart::SampleSizes,
        seen.sizes(cues),
        trak.samples.sizes(),
      ),
    ];
    for (part, written, wanted) in tables {
      if let Some(found) = table(part, written, wanted) {
        let part = match (trak.track, part) {
          (None, _) => WrittenPart::ChapterTrack,
          (Some(_), part) => part,
        };
        return Some(WrittenOtherwise::new(trak.track, part, found));
      }
    }
    // Its sync samples and their dependencies, unjudged of VC-1's.
    if let Some(flags) = trak.flags {
      let (sync, dependencies) = seen.flags();
      let tables = [
        (WrittenPart::SyncSamples, flags.sync, sync),
        (WrittenPart::Dependencies, flags.dependencies, dependencies),
      ];
      for (part, wanted, written) in tables {
        if let Some(found) = flags_found(wanted, written) {
          let part = if trak.track.is_some() {
            part
          } else {
            WrittenPart::ChapterTrack
          };
          return Some(WrittenOtherwise::new(trak.track, part, found));
        }
      }
    }
    // An animated AVIF's first chunk: its samples' first byte, the
    // payload's (`avif_write_trailer`, 9179–9181).
    if media.is_some()
      && let (Some(first), Some(start)) = (seen.first_chunk(), walk.payload_start())
      && first != start
    {
      return Some(WrittenOtherwise::new(
        trak.track,
        WrittenPart::ChunkOffsets,
        Found::Value(Counts::new(first, start)),
      ));
    }
    // Its roll groups (A3), then its user data (A6).
    let found = rolls_found(trak.rolls, seen.rolls())
      .map(|found| (WrittenPart::RollGroups, found))
      .or_else(|| {
        user_data_found(trak.user_data, seen.user_data())
          .map(|found| (WrittenPart::TrackUserData, found))
      });
    if let Some((part, found)) = found {
      let part = if trak.track.is_some() {
        part
      } else {
        WrittenPart::ChapterTrack
      };
      return Some(WrittenOtherwise::new(trak.track, part, found));
    }
  }
  // AVIF's items' records, each its track's description.
  if let Some((item, found)) = walk.item_found() {
    return Some(WrittenOtherwise::new(
      Some(TrackIndex::new(item)),
      WrittenPart::Configuration(0),
      found,
    ));
  }
  // The file's parts: the media data's size, against the bytes the sink
  // took between its header and that size's patch — AVIF's, written of
  // its size, against the bytes its track's samples were written of.
  if let Some((declared, taken)) = walk.media_data() {
    let wanted = media.unwrap_or(taken);
    if declared != wanted {
      return Some(WrittenOtherwise::new(
        None,
        WrittenPart::MediaData,
        Found::Length(Counts::new(declared, wanted)),
      ));
    }
  }
  // The movie's user data (A7), then its protection headers (A1).
  if let Some(found) = movie_user_data_found(movie.user_data, walk.user_data()) {
    return Some(WrittenOtherwise::new(
      None,
      WrittenPart::MovieUserData,
      found,
    ));
  }
  let (written, wanted) = (walk.protection(), movie.protection.summary());
  let found = if written.count != wanted.count {
    Some(Found::Count(Counts::new(written.count, wanted.count)))
  } else if written.sum != wanted.sum {
    Some(Found::Length(Counts::new(written.sum(), wanted.sum())))
  } else {
    (written.fold != wanted.fold).then_some(Found::Content)
  };
  if let Some(found) = found {
    return Some(WrittenOtherwise::new(
      None,
      WrittenPart::ProtectionHeaders,
      found,
    ));
  }
  // AVIF's first item, its extent at the payload's first byte (9192–9195).
  if media.is_some()
    && let (Some((offset, _)), Some(start)) = (walk.items().next(), walk.payload_start())
    && offset != start
  {
    return Some(WrittenOtherwise::new(
      None,
      WrittenPart::ItemLocation(1),
      Found::Value(Counts::new(offset, start)),
    ));
  }
  None
}

/// A track of FLV's as the muxer expects it: its sequence headers' records
/// — VVC's of their emptiness alone — and its coded frames' data sizes,
/// each folded ([`Runs`]), the muxer's track among FLV's audio and video
/// tracks in the order the walk is told them; `records` false where a
/// configuration of the track's rests on the trust model, its records then
/// judged by their count alone (R8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ExpectedFlv {
  pub(super) track: TrackIndex,
  pub(super) sequences: Summary,
  pub(super) records: bool,
  pub(super) frames: Summary,
}

/// The first difference between what the FLV walk read and what the muxer
/// expects: the tap's own state; then each track's sequence headers — their
/// count, named at the first past the shorter; their records' lengths
/// summed, then folded, named at the first — then its frames: their count,
/// their data sizes summed, then folded.
pub(super) fn flv_verdict(
  audit: &Audit,
  expected: impl Iterator<Item = ExpectedFlv>,
) -> Option<WrittenOtherwise> {
  if let Some(unfollowed) = audit.unfollowed() {
    return Some(WrittenOtherwise::new(
      None,
      WrittenPart::Structure,
      Found::Unfollowed(unfollowed),
    ));
  }
  let walk = audit.flv_walk()?;
  for (wanted, seen) in expected.zip(walk.tracks()) {
    let track = Some(wanted.track);
    let (written, handed) = (seen.sequences(), wanted.sequences);
    let first = u32::try_from(written.count.min(handed.count)).unwrap_or(u32::MAX);
    let found = if written.count != handed.count {
      Some((
        WrittenPart::Configuration(first),
        Found::Count(Counts::new(written.count, handed.count)),
      ))
    } else if wanted.records && written.sum != handed.sum {
      Some((
        WrittenPart::Configuration(0),
        Found::Length(Counts::new(written.sum(), handed.sum())),
      ))
    } else if wanted.records && written.fold != handed.fold {
      Some((WrittenPart::Configuration(0), Found::Content))
    } else {
      let (written, handed) = (seen.frames(), wanted.frames);
      if written.count != handed.count {
        Some((
          WrittenPart::Frames,
          Found::Count(Counts::new(written.count, handed.count)),
        ))
      } else if written.sum != handed.sum {
        Some((
          WrittenPart::Frames,
          Found::Length(Counts::new(written.sum(), handed.sum())),
        ))
      } else {
        (written.fold != handed.fold).then_some((WrittenPart::Frames, Found::Content))
      }
    };
    if let Some((part, found)) = found {
      return Some(WrittenOtherwise::new(track, part, found));
    }
  }
  None
}

/// The fold of a configuration's bytes, a byte at a time — the same over
/// one buffer as over the pieces it is made of: the muxer's of FLV's
/// current and new configurations, which FLV compares by size and bytes
/// (`flvenc.c` 1250–1264; R12).
pub(super) fn config_fold(state: u64, bytes: &[u8]) -> u64 {
  bytes
    .iter()
    .fold(state, |state, &byte| fold(state, u64::from(byte)))
}

/// The fold of an empty configuration.
pub(super) const CONFIG_SEED: u64 = FOLD_SEED;

/// What the muxer expects of a Matroska or WebM track's CodecPrivate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MkvPrivate {
  /// Nothing judged.
  Unjudged,
  /// Its length: AV1's `av1C` as `ff_isom_write_av1c` writes it (A4).
  Length(u64),
  /// FLAC's: its length, and the fold of its bytes past `fLaC` and the
  /// STREAMINFO block — the VORBIS_COMMENT block of the channel mask's one
  /// comment, its header among them (A21).
  Comment { len: u64, fold: u64 },
}

/// The bytes of a FLAC CodecPrivate before its comment block: `fLaC`, and
/// the STREAMINFO block's header and its 34 bytes (`ff_flac_write_header`,
/// `flacenc_header.c` 29–48).
pub(super) const FLAC_HEADER: u64 = 42;

/// The first difference between what the Matroska walk read and what the
/// muxer expects: the tap's own state; the TrackEntries' count; then, per
/// track in TrackEntry order, its CodecPrivate where judged — AV1's length
/// (`Configuration(0)`), FLAC's channel mask comment's (`ChannelMask`:
/// left out, its CodecPrivate's length, then the comment's fold); then the
/// Segment's size against the bytes after its header (`Structure`).
pub(super) fn mkv_verdict(
  audit: &Audit,
  expected: impl Iterator<Item = (TrackIndex, MkvPrivate)>,
  entries: u64,
) -> Option<WrittenOtherwise> {
  if let Some(unfollowed) = audit.unfollowed() {
    return Some(WrittenOtherwise::new(
      None,
      WrittenPart::Structure,
      Found::Unfollowed(unfollowed),
    ));
  }
  let walk = audit.mkv_walk()?;
  if walk.entries() != entries {
    return Some(WrittenOtherwise::new(
      None,
      WrittenPart::Structure,
      Found::Count(Counts::new(walk.entries(), entries)),
    ));
  }
  for ((track, wanted), seen) in expected.zip(walk.tracks()) {
    let written = seen.codec_private;
    let found = match wanted {
      MkvPrivate::Unjudged => None,
      MkvPrivate::Length(len) => match written {
        None => Some((WrittenPart::Configuration(0), Found::Omitted)),
        Some((written, _)) if written != len => Some((
          WrittenPart::Configuration(0),
          Found::Length(Counts::new(written, len)),
        )),
        Some(_) => None,
      },
      MkvPrivate::Comment { len, fold } => match written {
        None => Some((WrittenPart::ChannelMask, Found::Omitted)),
        Some((written, _)) if written <= FLAC_HEADER => {
          Some((WrittenPart::ChannelMask, Found::Omitted))
        }
        Some((written, _)) if written != len => Some((
          WrittenPart::ChannelMask,
          Found::Length(Counts::new(written, len)),
        )),
        Some((_, written)) => {
          (written != fold).then_some((WrittenPart::ChannelMask, Found::Content))
        }
      },
    };
    if let Some((part, found)) = found {
      return Some(WrittenOtherwise::new(Some(track), part, found));
    }
  }
  let (at, size) = walk.segment()?;
  let taken = audit.frontier().saturating_sub(at);
  (size != taken).then(|| {
    WrittenOtherwise::new(
      None,
      WrittenPart::Structure,
      Found::Length(Counts::new(size, taken)),
    )
  })
}

/// The first difference between what the raw FLAC walk read and what the
/// muxer expects of its `track`: the tap's own state; then the channel mask
/// entries of its VORBIS_COMMENT — `expected`, their count and the fold of
/// the last one's bytes — named by their absence, their count, then their
/// content (`ChannelMask`).
pub(super) fn flac_verdict(
  audit: &Audit,
  track: TrackIndex,
  expected: (u64, u64),
) -> Option<WrittenOtherwise> {
  if let Some(unfollowed) = audit.unfollowed() {
    return Some(WrittenOtherwise::new(
      None,
      WrittenPart::Structure,
      Found::Unfollowed(unfollowed),
    ));
  }
  let (count, fold) = audit.flac_walk()?.masks();
  let found = if count != expected.0 {
    if count == 0 {
      Found::Omitted
    } else {
      Found::Count(Counts::new(count, expected.0))
    }
  } else if fold != expected.1 {
    Found::Content
  } else {
    return None;
  };
  Some(WrittenOtherwise::new(
    Some(track),
    WrittenPart::ChannelMask,
    found,
  ))
}
