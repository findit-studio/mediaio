//! What changed between two timelines, clip by clip.

use core::{cmp::Ordering, fmt};
use std::{
  collections::{BTreeMap, btree_map::Entry},
  vec::Vec,
};

use mediatime::{TimeRange, Timestamp};

use crate::{Clip, ClipAt, Fade, Fades, IdClash, Timeline};

/// What changed from `before` to `after`, clip by clip, per track.
///
/// Tracks are matched by index, and within a track a clip by its
/// [`id`](Clip::id) alone: a clip renamed, or pointed at another medium,
/// keeps its id and is the same clip, while two placements of one medium —
/// a cut back to a shot — are two clips. A clip that changes track is
/// removed from one and added to the other.
///
/// An id names one clip of a timeline. A side that carries one twice is
/// refused with [`Ambiguous`] rather than matched by occurrence, which would
/// report a clip that stayed put as moved and one that stayed as removed;
/// [`validate`](fn@crate::validate) refuses such a timeline too
/// ([`Refusal::DuplicateClipId`](crate::Refusal::DuplicateClipId)).
///
/// A clip only in `after` is [`Added`](ChangeKind::Added); one only in
/// `before` is [`Removed`](ChangeKind::Removed). A clip in both is reported
/// once for each way it changed:
///
/// - [`Moved`](ChangeKind::Moved) — its record covers a different stretch
///   of the timeline. Positions are explicit, so a clip moves only when its
///   own record changed, never because a clip before it was trimmed;
/// - [`Retimed`](ChangeKind::Retimed) — its source range covers a different
///   stretch of its medium;
/// - [`Regained`](ChangeKind::Regained) — its gain or its fades changed;
/// - [`EnabledFlipped`](ChangeKind::EnabledFlipped) — it was enabled and is
///   disabled, or the other way round;
/// - [`Renamed`](ChangeKind::Renamed) — its name changed;
/// - [`Relinked`](ChangeKind::Relinked) — its medium's locator changed: it
///   reads another medium.
///
/// Ranges and lengths compare by the time they cover, so a range recounted
/// in another timebase over the same instants is not a change.
///
/// The changes come in a fixed order: by track, then by where the clip sits
/// — its record in `after`, or in `before` for a removed clip — then by kind
/// in the order above, then by index. So `diff(a, a)` is empty for every `a`
/// it answers, and every run over the same pair answers the same list.
///
/// Only clips are compared. The timeline's own words (name, rate, start,
/// notes, markers), a track's words (kind, name, enabled, markers),
/// transitions, a clip's notes and markers, its medium's words other than
/// the locator, and other readers' metadata on any of them
/// ([`ReaderMetadata`](crate::ReaderMetadata)) are not.
///
/// ```
/// use core::num::NonZeroI32;
///
/// use mediaio_timeline::{ChangeKind, Clip, ClipId, MediaRef, Rate, TimeRange, Timebase, Timeline, Track, TrackKind, diff};
///
/// let edit = Timebase::new(1, NonZeroI32::new(25).unwrap());
/// let at = |name: &str, start: i64| {
///   Clip::new(ClipId::new(name), name, MediaRef::new(name), TimeRange::new(0, 10, edit), TimeRange::new(start, start + 10, edit))
/// };
/// let before = Timeline::new("t", Rate::FPS_25).with_track(
///   Track::new(TrackKind::Video, "V").with_clip(at("a", 0)).with_clip(at("b", 10)),
/// );
/// let mut after = before.clone();
/// after.tracks_mut()[0].clips_mut()[1].set_record(TimeRange::new(20, 30, edit));
/// let delta = diff(&before, &after).unwrap();
/// assert_eq!(delta.changes().len(), 1);
/// assert_eq!(delta.changes()[0].kind(), ChangeKind::Moved);
/// assert_eq!(delta.changes()[0].after(), Some(1));
/// assert!(diff(&after, &after).unwrap().is_empty());
/// ```
pub fn diff(before: &Timeline, after: &Timeline) -> Result<Delta, Ambiguous> {
  for (side, timeline) in [(Side::Before, before), (Side::After, after)] {
    if let Some(clips) = first_clash(timeline) {
      return Err(Ambiguous { side, clips });
    }
  }
  let mut changes = Vec::new();
  let tracks = before.tracks().len().max(after.tracks().len());
  for track in 0..tracks {
    let clips_before = before
      .tracks()
      .get(track)
      .map_or(&[][..], |track| track.clips());
    let clips_after = after
      .tracks()
      .get(track)
      .map_or(&[][..], |track| track.clips());
    diff_track(track, clips_before, clips_after, &mut changes);
  }
  changes.sort_by_cached_key(|change| {
    (
      change.track,
      change.starts_at(before, after),
      rank(change.kind),
      change.before,
      change.after,
    )
  });
  Ok(Delta { changes })
}

/// The first clip of `timeline` carrying an earlier clip's id, in the
/// timeline's order, with the first clip to carry it.
fn first_clash(timeline: &Timeline) -> Option<IdClash> {
  let mut first = BTreeMap::new();
  for (track_index, track) in timeline.tracks().iter().enumerate() {
    for (index, clip) in track.clips().iter().enumerate() {
      let at = ClipAt::new(track_index, index);
      match first.entry(clip.id().as_str()) {
        Entry::Vacant(slot) => {
          slot.insert(at);
        }
        Entry::Occupied(slot) => return Some(IdClash::new(*slot.get(), at)),
      }
    }
  }
  None
}

/// Matches `before`'s clips with `after`'s by id, unique on each side.
fn diff_track(track: usize, before: &[Clip], after: &[Clip], out: &mut Vec<Change>) {
  // Each id's clip in `after`, waiting to be matched.
  let mut waiting: BTreeMap<&str, usize> = after
    .iter()
    .enumerate()
    .map(|(index, clip)| (clip.id().as_str(), index))
    .collect();
  let mut matched = std::vec![false; after.len()];
  for (index, clip) in before.iter().enumerate() {
    let Some(partner) = waiting.remove(clip.id().as_str()) else {
      out.push(Change::new(track, ChangeKind::Removed, Some(index), None));
      continue;
    };
    matched[partner] = true;
    let then = clip;
    let now = &after[partner];
    let pair = |kind| Change::new(track, kind, Some(index), Some(partner));
    if !same_span(then.record(), now.record()) {
      out.push(pair(ChangeKind::Moved));
    }
    if !same_span(then.source_range(), now.source_range()) {
      out.push(pair(ChangeKind::Retimed));
    }
    if then.gain() != now.gain() || !same_fades(then.fades(), now.fades()) {
      out.push(pair(ChangeKind::Regained));
    }
    if then.enabled() != now.enabled() {
      out.push(pair(ChangeKind::EnabledFlipped));
    }
    if then.name() != now.name() {
      out.push(pair(ChangeKind::Renamed));
    }
    if then.media().locator() != now.media().locator() {
      out.push(pair(ChangeKind::Relinked));
    }
  }
  for (index, matched) in matched.into_iter().enumerate() {
    if !matched {
      out.push(Change::new(track, ChangeKind::Added, None, Some(index)));
    }
  }
}

/// Whether two ranges cover the same stretch of time, whatever they are
/// counted in.
fn same_span(a: TimeRange, b: TimeRange) -> bool {
  a.start() == b.start() && a.end() == b.end()
}

fn same_fades(a: Fades, b: Fades) -> bool {
  same_fade(a.in_(), b.in_()) && same_fade(a.out(), b.out())
}

fn same_fade(a: Option<Fade>, b: Option<Fade>) -> bool {
  match (a, b) {
    (None, None) => true,
    (Some(a), Some(b)) => {
      a.shape() == b.shape() && a.duration().cmp_semantic(&b.duration()) == Ordering::Equal
    }
    _ => false,
  }
}

const fn rank(kind: ChangeKind) -> u8 {
  match kind {
    ChangeKind::Added => 0,
    ChangeKind::Removed => 1,
    ChangeKind::Moved => 2,
    ChangeKind::Retimed => 3,
    ChangeKind::Regained => 4,
    ChangeKind::EnabledFlipped => 5,
    ChangeKind::Renamed => 6,
    ChangeKind::Relinked => 7,
  }
}

/// Why [`diff`] refused: one side carries one clip id on two clips, so a
/// clip of the other side could be matched with either.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Ambiguous {
  side: Side,
  clips: IdClash,
}

impl Ambiguous {
  /// The timeline that carries the id twice.
  pub const fn side(&self) -> Side {
    self.side
  }

  /// Its first two clips carrying the id.
  pub const fn clips(&self) -> IdClash {
    self.clips
  }
}

/// Writes `before: track 1, clip 0 has the id of track 0, clip 2`.
impl fmt::Display for Ambiguous {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(
      f,
      "{}: {} has the id of {}",
      match self.side {
        Side::Before => "before",
        Side::After => "after",
      },
      self.clips.later(),
      self.clips.first()
    )
  }
}

impl core::error::Error for Ambiguous {}

/// One of the two timelines [`diff`] compares.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Side {
  /// The timeline before the change.
  Before,
  /// The timeline after it.
  After,
}

/// The changes [`diff`] found, in its fixed order.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct Delta {
  changes: Vec<Change>,
}

impl Delta {
  /// The changes, by track, then by where the clip sits, then by kind.
  pub const fn changes(&self) -> &[Change] {
    self.changes.as_slice()
  }

  /// Whether nothing [`diff`] compares changed.
  pub const fn is_empty(&self) -> bool {
    self.changes.is_empty()
  }
}

/// One way one clip changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Change {
  track: usize,
  kind: ChangeKind,
  before: Option<usize>,
  after: Option<usize>,
}

impl Change {
  const fn new(
    track: usize,
    kind: ChangeKind,
    before: Option<usize>,
    after: Option<usize>,
  ) -> Self {
    Self {
      track,
      kind,
      before,
      after,
    }
  }

  /// The track's index, in both timelines.
  pub const fn track(&self) -> usize {
    self.track
  }

  /// How the clip changed.
  pub const fn kind(&self) -> ChangeKind {
    self.kind
  }

  /// The clip's index in the track before, or `None` for an added clip.
  pub const fn before(&self) -> Option<usize> {
    self.before
  }

  /// The clip's index in the track after, or `None` for a removed clip.
  pub const fn after(&self) -> Option<usize> {
    self.after
  }

  /// The instant the clip starts at: after the change, or before it for a
  /// removed clip.
  fn starts_at(&self, before: &Timeline, after: &Timeline) -> Option<Timestamp> {
    let (timeline, index) = match self.after {
      Some(index) => (after, index),
      None => (before, self.before?),
    };
    let clip = timeline.tracks().get(self.track)?.clips().get(index)?;
    Some(clip.record().start())
  }
}

/// How a clip changed between two timelines.
///
/// Marked `#[non_exhaustive]`: a later kind of change joins as a variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, derive_more::IsVariant, derive_more::Display)]
#[non_exhaustive]
pub enum ChangeKind {
  /// The clip is new.
  #[display("added")]
  Added,
  /// The clip is gone.
  #[display("removed")]
  Removed,
  /// The clip's record changed: it sits elsewhere on the timeline, or covers
  /// more or less of it.
  #[display("moved")]
  Moved,
  /// The clip's source range changed: it plays a different stretch of its
  /// medium.
  #[display("retimed")]
  Retimed,
  /// The clip's gain or fades changed.
  #[display("regained")]
  Regained,
  /// The clip was enabled and is disabled, or the other way round.
  #[display("enabled_flipped")]
  EnabledFlipped,
  /// The clip's name changed.
  #[display("renamed")]
  Renamed,
  /// The clip's medium's locator changed: it reads another medium.
  #[display("relinked")]
  Relinked,
}

#[cfg(test)]
mod tests;
