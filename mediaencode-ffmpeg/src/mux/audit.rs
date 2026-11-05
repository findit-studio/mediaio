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
//! each track's tables, in track order; then the file's parts. A track's
//! omission is named before the file's sizes it shortens, so that the most
//! specific name wins.

use core::fmt;

use mediaencode::mediaspine::demuxer::TrackIndex;
use mediaio_ffmpeg::writer_guard::SinkTap;

use super::MovMode;

mod mov;
#[cfg(test)]
mod tests;

use mov::MovWalk;

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
  /// A track's time-to-sample table: the MOV family's `stts`.
  TimeToSample,
  /// A track's sample sizes: the MOV family's `stsz`.
  SampleSizes,
}

impl fmt::Display for WrittenPart {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(match self {
      Self::Structure => "the file's structure",
      Self::MediaData => "the media data",
      Self::Track => "its track box",
      Self::ChapterTrack => "the chapter track",
      Self::TimeToSample => "its time-to-sample table",
      Self::SampleSizes => "its sample sizes",
    })
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
  /// The values' sum, as a length: saturated where no file's could reach.
  fn sum(self) -> u64 {
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
    }
  }

  /// The MOV walk, where this tap walks the MOV family.
  pub(super) fn mov_walk(&self) -> Option<&MovWalk> {
    match &self.walk {
      Walk::Mov(walk) => Some(walk),
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
      };
      self.frontier = end;
      appended
    } else if end <= self.frontier {
      match &mut self.walk {
        Walk::Mov(walk) => walk.patch(self.frontier, at, bytes),
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
}

/// A track's `trak` as the muxer expects it, in the order movenc writes
/// them: a track of the muxer's, or movenc's chapter track.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ExpectedTrak {
  /// The muxer's track, or `None` for the chapter track.
  pub(super) track: Option<TrackIndex>,
  /// The track header's id: the stream's index and one; the chapter
  /// track's, the streams' count and one (`mov_setup_track_ids`, `movenc.c`
  /// 5270–5279).
  pub(super) id: u32,
  /// Its sample tables.
  pub(super) samples: SampleMirror,
}

/// The first difference between what the MOV walk read and what the muxer
/// expects, in the verdict's order (see [the module](self)).
pub(super) fn mov_verdict(
  audit: &Audit,
  expected: impl Iterator<Item = ExpectedTrak> + Clone,
) -> Option<WrittenOtherwise> {
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
  // Each track's tables, in track order.
  for (trak, seen) in expected.zip(traks) {
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
  }
  // The file's parts: the media data's size, against the bytes the sink
  // took between its header and that size's patch.
  if let Some((declared, taken)) = walk.media_data()
    && declared != taken
  {
    return Some(WrittenOtherwise::new(
      None,
      WrittenPart::MediaData,
      Found::Length(Counts::new(declared, taken)),
    ));
  }
  None
}
