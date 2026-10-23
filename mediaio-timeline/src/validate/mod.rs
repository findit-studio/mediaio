//! Judging a timeline: every refusal by name, with where it is.

use core::{cmp::Ordering, fmt};
use std::{
  collections::{BTreeMap, btree_map::Entry},
  vec::Vec,
};

use mediatime::{Duration, ExactSeconds, Rounding, Timebase, Timestamp};

use crate::{
  Clip, Marker, ReaderMetadata, Timeline, Track, Transition,
  metadata::{OURS, SCHEMA_WORD},
  time::span,
};

/// Judges `timeline`, answering every refusal it earns, or `Ok` when there
/// is none.
///
/// A timeline is valid when:
///
/// - its edit rate is stated — not zero ([`Refusal::RateUnstated`]);
/// - every position on it — the start, each record, each fade, each
///   transition — is counted at the edit rate ([`Refusal::OffEditRate`]);
/// - each clip's medium has a real ruler ([`Refusal::MediaRateUnstated`]);
/// - no range of a clip runs longer than `i64::MAX` ticks of its timebase
///   ([`Refusal::RangeTooLong`]) — no timeline needs one, and its length has
///   no exact count here;
/// - each record starts at or after the timeline's zero
///   ([`Refusal::RecordBeforeZero`]) and covers some time
///   ([`Refusal::EmptyRecord`]);
/// - each track's clips are in record order ([`Refusal::OutOfOrder`]) and no
///   two of their records overlap ([`Refusal::Overlap`]) — a transition
///   blends across a cut with media outside the records, so it is never an
///   overlap;
/// - each clip carries an id ([`Refusal::EmptyClipId`]) that no other clip
///   of the timeline carries ([`Refusal::DuplicateClipId`]) — the id is a
///   clip's identity, which [`diff`](fn@crate::diff) matches by. Names and
///   media may repeat: a cut back to one shot places it twice, under two
///   ids;
/// - each source range runs a whole number of edit-rate ticks
///   ([`Refusal::SourceOffEditRate`]) and its record exactly as long
///   ([`Refusal::DurationMismatch`]) — there is no time-warp in schema 1, so
///   nothing may round a source onto the edit rate;
/// - each source range lies inside its medium's available range when that
///   is known ([`Refusal::OutsideAvailable`]);
/// - each transition sits on a cut two of its track's records share
///   ([`Refusal::OffBoundary`]), alone ([`Refusal::DuplicateTransition`]),
///   with both handles inside the media where that is known
///   ([`Refusal::HandleMissing`]);
/// - no fade sits at a cut a transition already blends
///   ([`Refusal::FadeMeetsTransition`]), and a clip's blends — a fade, or the
///   part of a transition inside it — fit inside it
///   ([`Refusal::BlendsOverrunClip`]): in particular, no fade runs longer
///   than its clip;
/// - each marker is counted where its owner counts time — the timeline's
///   and a track's at the edit rate ([`Refusal::OffEditRate`]), a clip's in
///   a real ruler ([`Refusal::MarkerRateUnstated`]) — over a range no
///   longer than `i64::MAX` ticks ([`Refusal::MarkerTooLong`]);
/// - each reader's namespace — on the timeline, a track, a clip or a
///   marker ([`ReaderMetadata`]) — is named neither `mediaio`, the key the
///   export writes the crate's own words under, nor `OTIO_SCHEMA`
///   ([`Refusal::ReservedNamespace`]), and holds no key `OTIO_SCHEMA`
///   ([`Refusal::SchemaKeyInNamespace`]): OpenTimelineIO reads a dictionary
///   carrying that key as an object of the schema it names, so either would
///   turn the object's metadata, or the namespace, into another object.
///
/// Refusals come back in a fixed order: the timeline's own, then its
/// markers', then its namespaces', then track by track — each clip's, its
/// namespaces' and its markers' among them, then each transition's, then
/// each clip's blends, then the track's markers', then the track's
/// namespaces'. Each marker's namespaces' follow its own.
/// The checks that need the edit rate are skipped while it is unstated,
/// those that read a clip's media ruler are skipped for a clip whose medium
/// has none, and those that measure a range are skipped for a range too long
/// to measure, so those defects are not reported again by every check that
/// would read them.
///
/// The time arithmetic is `mediatime`'s, compared exactly across timebases.
pub fn validate(timeline: &Timeline) -> Result<(), Vec<Refusal>> {
  let mut refusals = Vec::new();
  // Each id's first clip, across the timeline.
  let mut ids = BTreeMap::new();
  let edit = timeline.edit_timebase();
  match edit {
    None => refusals.push(Refusal::RateUnstated),
    Some(edit) => {
      if timeline.start().timebase() != edit {
        refusals.push(Refusal::OffEditRate(Place::Start));
      }
    }
  }
  for (index, marker) in timeline.markers().iter().enumerate() {
    judge_edit_marker(
      marker,
      MarkerAt::Timeline { marker: index },
      edit,
      &mut refusals,
    );
  }
  judge_readers(timeline.readers(), MetadataAt::Timeline, &mut refusals);
  for (index, track) in timeline.tracks().iter().enumerate() {
    judge_track(index, track, edit, &mut ids, &mut refusals);
  }
  if refusals.is_empty() {
    Ok(())
  } else {
    Err(refusals)
  }
}

fn judge_track<'a>(
  track_index: usize,
  track: &'a Track,
  edit: Option<Timebase>,
  ids: &mut BTreeMap<&'a str, ClipAt>,
  out: &mut Vec<Refusal>,
) {
  let clips = track.clips();
  // The record reaching furthest so far, by index and end: a later record
  // starting before that end overlaps it.
  let mut furthest: Option<(usize, Timestamp)> = None;
  for (index, clip) in clips.iter().enumerate() {
    let at = ClipAt::new(track_index, index);
    let record = clip.record();
    let on_rate = edit.is_none_or(|edit| record.timebase() == edit);
    if !on_rate {
      out.push(Refusal::OffEditRate(Place::Record(at)));
    }
    let media_stated = media_stated(clip);
    if !media_stated {
      out.push(Refusal::MediaRateUnstated(at));
    }
    let record_span = span(record);
    let source_span = span(clip.source_range());
    let available_span = clip.media().available_range().map(span);
    for (range, measured) in [
      (ClipRange::Record, record_span.is_some()),
      (ClipRange::Source, source_span.is_some()),
      (
        ClipRange::Available,
        available_span.is_none_or(|span| span.is_some()),
      ),
    ] {
      if !measured {
        out.push(Refusal::RangeTooLong(RangeAt::new(at, range)));
      }
    }
    if record.start() < Timestamp::new(0, record.timebase()) {
      out.push(Refusal::RecordBeforeZero(at));
    }
    if record.start() == record.end() {
      out.push(Refusal::EmptyRecord(at));
    }
    if let Some(previous) = index.checked_sub(1) {
      let earlier = clips[previous].record();
      if record.start() < earlier.start() {
        out.push(Refusal::OutOfOrder(ClipPair::new(
          track_index,
          previous,
          index,
        )));
      } else if let Some((reach, end)) = furthest
        && record.start() < end
        && record.start() < record.end()
      {
        out.push(Refusal::Overlap(ClipPair::new(track_index, reach, index)));
      }
    }
    if furthest.is_none_or(|(_, end)| record.end() > end) {
      furthest = Some((index, record.end()));
    }
    // An empty id is refused once, and is no id the others could share.
    let id = clip.id().as_str();
    if id.is_empty() {
      out.push(Refusal::EmptyClipId(at));
    } else {
      match ids.entry(id) {
        Entry::Vacant(slot) => {
          slot.insert(at);
        }
        Entry::Occupied(slot) => {
          out.push(Refusal::DuplicateClipId(IdClash::new(*slot.get(), at)));
        }
      }
    }
    if let Some(edit) = edit
      && media_stated
      && let Some(length) = source_span
    {
      // The source's length at the edit rate, exactly: with no time-warp in
      // schema 1 a record cannot absorb a remainder. A length between ticks
      // is the source's own defect; one too long to count at the edit rate
      // matches no record.
      let expected = length.checked_rescale_with(edit, Rounding::Exact);
      if expected.is_none() && length.checked_rescale_to(edit).is_some() {
        out.push(Refusal::SourceOffEditRate(at));
      } else if on_rate
        && let Some(found) = record_span
        && expected.is_none_or(|expected| expected.ticks() != found.ticks())
      {
        out.push(Refusal::DurationMismatch(Mismatch {
          clip: at,
          expected,
          found,
        }));
      }
    }
    if media_stated
      && let Some(available) = clip.media().available_range()
      && !available.contains(&clip.source_range())
    {
      out.push(Refusal::OutsideAvailable(at));
    }
    if let Some(edit) = edit {
      let fades = clip.fades();
      for (edge, fade) in [(Edge::In, fades.in_()), (Edge::Out, fades.out())] {
        if let Some(fade) = fade
          && fade.duration().timebase() != edit
        {
          out.push(Refusal::OffEditRate(Place::Fade(EdgeAt::new(at, edge))));
        }
      }
    }
    judge_readers(clip.readers(), MetadataAt::Clip(at), out);
    for (index, marker) in clip.markers().iter().enumerate() {
      let at = MarkerAt::Clip {
        clip: at,
        marker: index,
      };
      if marker.range().timebase().num() == 0 {
        out.push(Refusal::MarkerRateUnstated(at));
      }
      judge_marker_length(marker, at, out);
      judge_readers(marker.readers(), MetadataAt::Marker(at), out);
    }
  }

  let transitions = track.transitions();
  for (index, transition) in transitions.iter().enumerate() {
    let at = TransitionAt::new(track_index, index);
    if let Some(edit) = edit
      && (transition.at().timebase() != edit
        || transition.in_offset().timebase() != edit
        || transition.out_offset().timebase() != edit)
    {
      out.push(Refusal::OffEditRate(Place::Transition(at)));
    }
    if transitions[..index]
      .iter()
      .any(|earlier| earlier.at() == transition.at())
    {
      out.push(Refusal::DuplicateTransition(at));
    }
    let (Some(outgoing), Some(incoming)) = (
      outgoing_at(clips, transition.at()),
      incoming_at(clips, transition.at()),
    ) else {
      out.push(Refusal::OffBoundary(at));
      continue;
    };
    if !tail_handle_held(&clips[outgoing], transition.out_offset()) {
      out.push(Refusal::HandleMissing(EdgeAt::new(
        ClipAt::new(track_index, outgoing),
        Edge::Out,
      )));
    }
    if !head_handle_held(&clips[incoming], transition.in_offset()) {
      out.push(Refusal::HandleMissing(EdgeAt::new(
        ClipAt::new(track_index, incoming),
        Edge::In,
      )));
    }
  }

  for (index, clip) in clips.iter().enumerate() {
    let at = ClipAt::new(track_index, index);
    let record = clip.record();
    let fades = clip.fades();
    let entering = transition_at(transitions, record.start());
    let leaving = transition_at(transitions, record.end());
    if fades.in_().is_some() && entering.is_some() {
      out.push(Refusal::FadeMeetsTransition(EdgeAt::new(at, Edge::In)));
    }
    if fades.out().is_some() && leaving.is_some() {
      out.push(Refusal::FadeMeetsTransition(EdgeAt::new(at, Edge::Out)));
    }
    // What blends inside the record at each edge: the fade, or the part of a
    // transition on this side of its cut — whichever reaches further.
    let head = longest(
      fades.in_().map(|fade| fade.duration()),
      entering.map(Transition::out_offset),
    );
    let tail = longest(
      fades.out().map(|fade| fade.duration()),
      leaving.map(Transition::in_offset),
    );
    let blends = ExactSeconds::from_duration(head).checked_add(ExactSeconds::from_duration(tail));
    if let Some(length) = span(record)
      && blends.is_none_or(|blends| blends > ExactSeconds::from_duration(length))
    {
      out.push(Refusal::BlendsOverrunClip(at));
    }
  }

  for (index, marker) in track.markers().iter().enumerate() {
    let at = MarkerAt::Track {
      track: track_index,
      marker: index,
    };
    judge_edit_marker(marker, at, edit, out);
  }
  judge_readers(track.readers(), MetadataAt::Track(track_index), out);
}

/// Refuses the timeline's marker or a track's, at `at`: counted otherwise
/// than at the edit rate, where that is stated; a range too long to
/// measure; and its namespaces, as [`judge_readers`] does.
fn judge_edit_marker(
  marker: &Marker,
  at: MarkerAt,
  edit: Option<Timebase>,
  out: &mut Vec<Refusal>,
) {
  if let Some(edit) = edit
    && marker.range().timebase() != edit
  {
    out.push(Refusal::OffEditRate(Place::Marker(at)));
  }
  judge_marker_length(marker, at, out);
  judge_readers(marker.readers(), MetadataAt::Marker(at), out);
}

/// Refuses `marker`, at `at`, for a range too long to measure.
fn judge_marker_length(marker: &Marker, at: MarkerAt, out: &mut Vec<Refusal>) {
  if span(marker.range()).is_none() {
    out.push(Refusal::MarkerTooLong(at));
  }
}

/// Refuses each namespace of `readers`, on `owner`, named as one the export
/// keeps, and each holding OpenTimelineIO's schema word as a key.
fn judge_readers(readers: &ReaderMetadata, owner: MetadataAt, out: &mut Vec<Refusal>) {
  for (index, (namespace, words)) in readers.iter().enumerate() {
    let at = NamespaceAt::new(owner, index);
    if namespace == OURS || namespace == SCHEMA_WORD {
      out.push(Refusal::ReservedNamespace(at));
    }
    if words.get(SCHEMA_WORD).is_some() {
      out.push(Refusal::SchemaKeyInNamespace(at));
    }
  }
}

/// Whether the clip's medium has a real ruler: no stated rate of zero, and
/// neither its source range nor its available range counted in a
/// degenerate timebase.
fn media_stated(clip: &Clip) -> bool {
  let media = clip.media();
  media.rate().is_none_or(|rate| rate.num() != 0)
    && clip.source_range().timebase().num() != 0
    && media
      .available_range()
      .is_none_or(|available| available.timebase().num() != 0)
}

/// The clip whose record ends at the cut `at`.
fn outgoing_at(clips: &[Clip], at: Timestamp) -> Option<usize> {
  clips.iter().position(|clip| {
    let record = clip.record();
    record.end() == at && record.start() < record.end()
  })
}

/// The clip whose record starts at the cut `at`.
fn incoming_at(clips: &[Clip], at: Timestamp) -> Option<usize> {
  clips.iter().position(|clip| {
    let record = clip.record();
    record.start() == at && record.start() < record.end()
  })
}

/// The first transition whose cut is at `at`.
fn transition_at(transitions: &[Transition], at: Timestamp) -> Option<&Transition> {
  transitions.iter().find(|transition| transition.at() == at)
}

/// Whether the outgoing clip's medium holds `out_offset` past its source
/// range's end. A medium whose available range is unknown is taken at its
/// word, as its source range is; a sum past `i128` holds nothing.
fn tail_handle_held(clip: &Clip, out_offset: Duration) -> bool {
  if !media_stated(clip) {
    return true;
  }
  let Some(available) = clip.media().available_range() else {
    return true;
  };
  ExactSeconds::from_timestamp(clip.source_range().end())
    .checked_add(ExactSeconds::from_duration(out_offset))
    .is_some_and(|needed| needed <= ExactSeconds::from_timestamp(available.end()))
}

/// Whether the incoming clip's medium holds `in_offset` before its source
/// range's start.
fn head_handle_held(clip: &Clip, in_offset: Duration) -> bool {
  if !media_stated(clip) {
    return true;
  }
  let Some(available) = clip.media().available_range() else {
    return true;
  };
  ExactSeconds::from_timestamp(clip.source_range().start())
    .checked_sub(ExactSeconds::from_duration(in_offset))
    .is_some_and(|needed| ExactSeconds::from_timestamp(available.start()) <= needed)
}

/// The longer of two optional lengths, or a zero length when both are
/// absent.
fn longest(a: Option<Duration>, b: Option<Duration>) -> Duration {
  match (a, b) {
    (Some(a), Some(b)) => match a.cmp_semantic(&b) {
      Ordering::Less => b,
      _ => a,
    },
    (Some(one), None) | (None, Some(one)) => one,
    (None, None) => Duration::new(0, Timebase::default()),
  }
}

/// One defect [`validate`] found, named, with where it is.
///
/// Marked `#[non_exhaustive]`: a later check joins as a variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Refusal {
  /// The edit rate is zero: a timeline needs a real ruler to count its
  /// positions in.
  RateUnstated,
  /// A position on the timeline is not counted at the edit rate.
  OffEditRate(Place),
  /// A clip's medium has no real ruler: a stated rate of zero, or a source
  /// or available range counted in a degenerate (`0/den`) timebase.
  MediaRateUnstated(ClipAt),
  /// One of a clip's ranges runs longer than `i64::MAX` ticks of its
  /// timebase. No timeline needs one, and its length has no exact count
  /// here: measured by `mediatime`'s `total_pts`, which saturates, two such
  /// ranges of different lengths would compare equal.
  RangeTooLong(RangeAt),
  /// A record starts before the timeline's zero.
  RecordBeforeZero(ClipAt),
  /// A record covers no time.
  EmptyRecord(ClipAt),
  /// A clip's record starts before an earlier clip's: a track's clips are
  /// kept in record order.
  OutOfOrder(ClipPair),
  /// Two records of one track overlap.
  Overlap(ClipPair),
  /// A clip's id is empty: a clip carries the id its creator minted.
  EmptyClipId(ClipAt),
  /// A clip carries the id of an earlier clip of the timeline, on its track
  /// or another. An id is a clip's identity, which
  /// [`diff`](fn@crate::diff) matches by, so it is unique in the timeline.
  DuplicateClipId(IdClash),
  /// A source range's length is no whole number of edit-rate ticks. A
  /// record runs exactly as long as its source (schema 1 has no time-warp),
  /// so a source must rescale onto the edit rate exactly.
  SourceOffEditRate(ClipAt),
  /// A record's length is not its source range's, counted at the edit rate.
  DurationMismatch(Mismatch),
  /// A source range runs outside its medium's available range.
  OutsideAvailable(ClipAt),
  /// A transition's cut is not where one of its track's records ends and
  /// another begins.
  OffBoundary(TransitionAt),
  /// A transition sits at a cut an earlier transition already holds.
  DuplicateTransition(TransitionAt),
  /// A clip's medium does not hold the handle a transition plays at that
  /// edge.
  HandleMissing(EdgeAt),
  /// A fade sits at a cut a transition already blends.
  FadeMeetsTransition(EdgeAt),
  /// A clip's blends — its fades, and the parts of transitions inside it —
  /// together run longer than its record.
  BlendsOverrunClip(ClipAt),
  /// A clip's marker is counted in a degenerate (`0/den`) timebase: it
  /// marks no time in the clip's medium.
  MarkerRateUnstated(MarkerAt),
  /// A marker's range runs longer than `i64::MAX` ticks of its timebase,
  /// as [`Refusal::RangeTooLong`] refuses a clip's range.
  MarkerTooLong(MarkerAt),
  /// A reader's namespace named `mediaio` — the key the export writes the
  /// crate's own words under — or `OTIO_SCHEMA`, which OpenTimelineIO
  /// reads as a schema word: the export would write it over its own words,
  /// or make the object's metadata an object of a schema.
  ReservedNamespace(NamespaceAt),
  /// A key `OTIO_SCHEMA` in a reader's namespace: OpenTimelineIO reads a
  /// dictionary carrying it as an object of the schema it names
  /// (`deserialization.cpp` 561–573 in 0.17.0), which the namespace is not.
  SchemaKeyInNamespace(NamespaceAt),
}

impl fmt::Display for Refusal {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::RateUnstated => f.write_str("the edit rate is zero"),
      Self::OffEditRate(place) => write!(f, "{place} is not counted at the edit rate"),
      Self::MediaRateUnstated(at) => write!(f, "{at}: the medium has no real ruler"),
      Self::RangeTooLong(at) => write!(f, "{at} runs longer than i64::MAX ticks"),
      Self::RecordBeforeZero(at) => {
        write!(f, "{at}: the record starts before the timeline's zero")
      }
      Self::EmptyRecord(at) => write!(f, "{at}: the record covers no time"),
      Self::OutOfOrder(pair) => write!(
        f,
        "track {}: clip {} starts before clip {}",
        pair.track, pair.later, pair.earlier
      ),
      Self::Overlap(pair) => write!(
        f,
        "track {}: clip {}'s record overlaps clip {}'s",
        pair.track, pair.later, pair.earlier
      ),
      Self::EmptyClipId(at) => write!(f, "{at}: the clip has no id"),
      Self::DuplicateClipId(clash) => {
        write!(f, "{} has the id of {}", clash.later, clash.first)
      }
      Self::SourceOffEditRate(at) => write!(
        f,
        "{at}: the source range's length is no whole number of edit-rate ticks"
      ),
      Self::DurationMismatch(mismatch) => {
        write!(
          f,
          "{}: the record runs {} ticks, the source range ",
          mismatch.clip,
          mismatch.found.ticks()
        )?;
        match mismatch.expected {
          Some(expected) => write!(f, "{} at the edit rate", expected.ticks()),
          None => f.write_str("has no count at the edit rate"),
        }
      }
      Self::OutsideAvailable(at) => {
        write!(f, "{at}: the source range runs outside the available range")
      }
      Self::OffBoundary(at) => write!(f, "{at}: the cut is not where two records meet"),
      Self::DuplicateTransition(at) => {
        write!(f, "{at}: an earlier transition holds this cut")
      }
      Self::HandleMissing(at) => write!(
        f,
        "{}: the medium holds no handle for the transition at its {}",
        at.clip,
        at.edge.side()
      ),
      Self::FadeMeetsTransition(at) => write!(
        f,
        "{}: a fade at its {} where a transition blends",
        at.clip,
        at.edge.side()
      ),
      Self::BlendsOverrunClip(at) => {
        write!(
          f,
          "{at}: its fades and transitions run longer than its record"
        )
      }
      Self::MarkerRateUnstated(at) => {
        write!(f, "{at}: the marked range has no real ruler")
      }
      Self::MarkerTooLong(at) => {
        write!(f, "{at}: the marked range runs longer than i64::MAX ticks")
      }
      Self::ReservedNamespace(at) => write!(
        f,
        "{at}: a reader's namespace named mediaio or OTIO_SCHEMA, which the export keeps"
      ),
      Self::SchemaKeyInNamespace(at) => write!(
        f,
        "{at}: the key OTIO_SCHEMA, which OpenTimelineIO reads as a schema word"
      ),
    }
  }
}

impl core::error::Error for Refusal {}

/// A clip, by the index of its track in the timeline and its index in the
/// track.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ClipAt {
  track: usize,
  clip: usize,
}

impl ClipAt {
  /// Clip `clip` of track `track`.
  pub const fn new(track: usize, clip: usize) -> Self {
    Self { track, clip }
  }

  /// The track's index in the timeline.
  pub const fn track(&self) -> usize {
    self.track
  }

  /// The clip's index in its track.
  pub const fn clip(&self) -> usize {
    self.clip
  }
}

/// Writes `track 0, clip 2`.
impl fmt::Display for ClipAt {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "track {}, clip {}", self.track, self.clip)
  }
}

/// Two clips of one track, by index: the earlier and the later in the
/// track's order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ClipPair {
  track: usize,
  earlier: usize,
  later: usize,
}

impl ClipPair {
  /// Clips `earlier` and `later` of track `track`.
  pub const fn new(track: usize, earlier: usize, later: usize) -> Self {
    Self {
      track,
      earlier,
      later,
    }
  }

  /// The track's index in the timeline.
  pub const fn track(&self) -> usize {
    self.track
  }

  /// The earlier clip's index in its track.
  pub const fn earlier(&self) -> usize {
    self.earlier
  }

  /// The later clip's index in its track.
  pub const fn later(&self) -> usize {
    self.later
  }
}

/// Two clips of one timeline carrying one id: the first to carry it, in the
/// timeline's order — track by track, clip by clip — and a later one. What
/// [`Refusal::DuplicateClipId`] and [`Ambiguous`](crate::Ambiguous) name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct IdClash {
  first: ClipAt,
  later: ClipAt,
}

impl IdClash {
  /// `later` carrying the id `first` carries.
  pub const fn new(first: ClipAt, later: ClipAt) -> Self {
    Self { first, later }
  }

  /// The first clip to carry the id.
  pub const fn first(&self) -> ClipAt {
    self.first
  }

  /// A later clip carrying it.
  pub const fn later(&self) -> ClipAt {
    self.later
  }
}

/// A transition, by the index of its track in the timeline and its index in
/// the track's transitions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TransitionAt {
  track: usize,
  transition: usize,
}

impl TransitionAt {
  /// Transition `transition` of track `track`.
  pub const fn new(track: usize, transition: usize) -> Self {
    Self { track, transition }
  }

  /// The track's index in the timeline.
  pub const fn track(&self) -> usize {
    self.track
  }

  /// The transition's index in its track.
  pub const fn transition(&self) -> usize {
    self.transition
  }
}

/// Writes `track 0, transition 1`.
impl fmt::Display for TransitionAt {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "track {}, transition {}", self.track, self.transition)
  }
}

/// One of a clip's two edges.
#[derive(
  Debug,
  Clone,
  Copy,
  PartialEq,
  Eq,
  Hash,
  PartialOrd,
  Ord,
  derive_more::Display,
  derive_more::IsVariant,
)]
pub enum Edge {
  /// The clip's start: where a fade-in runs, and where a transition into the
  /// clip plays the media before its source range.
  #[display("in")]
  In,
  /// The clip's end: where a fade-out runs, and where a transition out of
  /// the clip plays the media after its source range.
  #[display("out")]
  Out,
}

impl Edge {
  pub(crate) const fn side(self) -> &'static str {
    match self {
      Self::In => "start",
      Self::Out => "end",
    }
  }
}

/// One edge of one clip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EdgeAt {
  clip: ClipAt,
  edge: Edge,
}

impl EdgeAt {
  /// The `edge` of `clip`.
  pub const fn new(clip: ClipAt, edge: Edge) -> Self {
    Self { clip, edge }
  }

  /// The clip.
  pub const fn clip(&self) -> ClipAt {
    self.clip
  }

  /// Which edge.
  pub const fn edge(&self) -> Edge {
    self.edge
  }
}

/// One of the three ranges a clip carries.
///
/// Marked `#[non_exhaustive]`: a later range joins as a variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum ClipRange {
  /// Its record, on the timeline.
  Record,
  /// Its source range, in its medium.
  Source,
  /// Its medium's available range.
  Available,
}

impl ClipRange {
  const fn noun(self) -> &'static str {
    match self {
      Self::Record => "the record",
      Self::Source => "the source range",
      Self::Available => "the available range",
    }
  }
}

/// One range of one clip: what [`Refusal::RangeTooLong`] names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RangeAt {
  clip: ClipAt,
  range: ClipRange,
}

impl RangeAt {
  /// The `range` of `clip`.
  pub const fn new(clip: ClipAt, range: ClipRange) -> Self {
    Self { clip, range }
  }

  /// The clip.
  pub const fn clip(&self) -> ClipAt {
    self.clip
  }

  /// Which of its ranges.
  pub const fn range(&self) -> ClipRange {
    self.range
  }
}

/// Writes `the source range of track 0, clip 2`.
impl fmt::Display for RangeAt {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "{} of {}", self.range.noun(), self.clip)
  }
}

/// A position on the timeline that [`Refusal::OffEditRate`] names.
///
/// Marked `#[non_exhaustive]`: a later positioned word joins as a variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Place {
  /// The timeline's start.
  Start,
  /// A clip's record.
  Record(ClipAt),
  /// A clip's fade at one edge.
  Fade(EdgeAt),
  /// A transition's cut or one of its offsets.
  Transition(TransitionAt),
  /// The timeline's marker or a track's: its marked range.
  Marker(MarkerAt),
}

/// Writes `the start`, `the record of track 0, clip 2`, and so on.
impl fmt::Display for Place {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Start => f.write_str("the start"),
      Self::Record(at) => write!(f, "the record of {at}"),
      Self::Fade(at) => write!(f, "the fade at the {} of {}", at.edge.side(), at.clip),
      Self::Transition(at) => write!(f, "{at}"),
      Self::Marker(at) => write!(f, "the marked range of {at}"),
    }
  }
}

/// A marker, by its owner and its index among the owner's markers.
///
/// Marked `#[non_exhaustive]`: a later owner joins as a variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum MarkerAt {
  /// The timeline's own marker, which OpenTimelineIO writes on its stack.
  Timeline {
    /// The marker's index among the timeline's markers.
    marker: usize,
  },
  /// A track's marker.
  Track {
    /// The track's index in the timeline.
    track: usize,
    /// The marker's index among the track's markers.
    marker: usize,
  },
  /// A clip's marker.
  Clip {
    /// The clip.
    clip: ClipAt,
    /// The marker's index among the clip's markers.
    marker: usize,
  },
}

/// Writes `the timeline, marker 0`, `track 0, marker 1`, or `track 0,
/// clip 2, marker 1`.
impl fmt::Display for MarkerAt {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Timeline { marker } => write!(f, "the timeline, marker {marker}"),
      Self::Track { track, marker } => write!(f, "track {track}, marker {marker}"),
      Self::Clip { clip, marker } => write!(f, "{clip}, marker {marker}"),
    }
  }
}

/// An object that carries other readers' metadata ([`ReaderMetadata`]):
/// the timeline, a track, a clip or a marker.
///
/// Marked `#[non_exhaustive]`: a later owner joins as a variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum MetadataAt {
  /// The timeline.
  Timeline,
  /// A track, by its index in the timeline.
  Track(usize),
  /// A clip.
  Clip(ClipAt),
  /// A marker.
  Marker(MarkerAt),
}

/// Writes `the timeline`, `track 0`, `track 0, clip 2`, or a marker as
/// [`MarkerAt`] writes one.
impl fmt::Display for MetadataAt {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Timeline => f.write_str("the timeline"),
      Self::Track(track) => write!(f, "track {track}"),
      Self::Clip(at) => write!(f, "{at}"),
      Self::Marker(at) => write!(f, "{at}"),
    }
  }
}

/// One reader's namespace on an object, by the object and the namespace's
/// index among the object's, in key order — the order
/// [`ReaderMetadata::iter`] yields them in: what
/// [`Refusal::ReservedNamespace`] and [`Refusal::SchemaKeyInNamespace`]
/// name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NamespaceAt {
  owner: MetadataAt,
  namespace: usize,
}

impl NamespaceAt {
  /// The `namespace`-th of `owner`'s namespaces, in key order.
  pub const fn new(owner: MetadataAt, namespace: usize) -> Self {
    Self { owner, namespace }
  }

  /// The object carrying the namespace.
  pub const fn owner(&self) -> MetadataAt {
    self.owner
  }

  /// The namespace's index among the object's, in key order.
  pub const fn namespace(&self) -> usize {
    self.namespace
  }
}

/// Writes `track 0, clip 2, namespace 1`.
impl fmt::Display for NamespaceAt {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "{}, namespace {}", self.owner, self.namespace)
  }
}

/// A record whose length is not its source range's: what
/// [`Refusal::DurationMismatch`] carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Mismatch {
  clip: ClipAt,
  expected: Option<Duration>,
  found: Duration,
}

impl Mismatch {
  /// The clip.
  pub const fn clip(&self) -> ClipAt {
    self.clip
  }

  /// The source range's length counted at the edit rate — the record's
  /// length the source asks for — or `None` when that count does not fit a
  /// `u64`.
  pub const fn expected(&self) -> Option<Duration> {
    self.expected
  }

  /// The record's length.
  pub const fn found(&self) -> Duration {
    self.found
  }
}

#[cfg(test)]
mod tests;
