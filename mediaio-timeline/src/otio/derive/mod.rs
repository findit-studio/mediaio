//! What OpenTimelineIO derives from the document the export writes, in
//! OpenTimelineIO's own arithmetic beside the exact counts, and the hold on
//! each value it derives.
//!
//! OpenTimelineIO keeps a time as a `RationalTime`: a value and a rate, both
//! `f64`. A sum or a difference of two is carried in the higher of their
//! rates — the operand at the lower rate is rescaled to it, `(value ·
//! new_rate) / rate`, two rounded operations, and the values are added
//! (`opentime/rationalTime.h` 70–75, 286–339). A range ends at its duration
//! plus its start rescaled to the duration's rate (`timeRange.h` 105–108),
//! and its last tick — `end_time_inclusive`, the time of the last frame with
//! data — is that end floored or less one tick, by a branch OpenTimelineIO
//! takes on its own doubles (`timeRange.h` 88–102; [`Time::end_inclusive`]).
//! Every range below is held at both ends. On that arithmetic:
//!
//! - a child's place on its track starts at zero **in that child's own
//!   rate** and adds the duration of every item before it
//!   (`opentimelineio/track.cpp` 51–92); a transition starts its
//!   `in_offset` earlier;
//! - walking a whole track carries one running end, rescaled into each
//!   item's rate in turn (`track.cpp` 221–271);
//! - a track's duration starts at zero at rate 1 and adds every item
//!   (`track.cpp` 117–148), and the track's range runs it from zero in its
//!   rate — the range the stack gives the track too (`stack.cpp` 41–62); the
//!   stack's is the longest track's, picked by comparing `value / rate` in
//!   `f64`, the first of two that compare equal (`stack.cpp` 118–134,
//!   `rationalTime.h` 348–371), so the stack's range is the picked track's;
//! - a child's place in the timeline adds the stack's start of its track:
//!   zero, in the track's duration rate (`composition.cpp` 320–368,
//!   `stack.cpp` 41–62, `timeline.h` 69–74);
//! - an item beside a transition is visible over its source range widened by
//!   the transition's handles (`item.cpp` 60–85, `track.cpp` 150–165);
//! - an absolute time adds the global start to a place, with `operator+`,
//!   or runs a range of the timeline's duration from it — how
//!   OpenTimelineIO's own tools read one: toucan (`TimelineWrapper.cpp`
//!   319–324), raven (`app.cpp` 1315–1318), the FCP XML adapter
//!   (`fcp_xml.py` 2029–2032) and the Unreal plugin (`util.py` 93–106).
//!   OpenTimelineIO's core adds the global start to nothing itself
//!   (`timeline.h` 50–66);
//! - a child's range from the global start is its range in the timeline
//!   moved by the global start, its own duration kept — as OpenTimelineIO
//!   moves a child's range into its parent's, the parent's start added to
//!   the child's and the child's duration unchanged (`composition.cpp`
//!   357–359) — so it ends at that duration plus the moved start rescaled
//!   to the duration's rate, a rescale that neither `operator+` of the
//!   global start to an end of the child's place makes;
//! - a stack with no track has the range `TimeRange()` (`stack.cpp`
//!   121–123), its duration `RationalTime()` (`timeRange.h` 34–37), zero at
//!   rate 1 (`rationalTime.h` 28), and so the timeline (`timeline.h`
//!   63–66): its range from the global start ends at the global start
//!   rescaled to rate 1.
//!
//! [`Time`] is a `RationalTime` computed operation for operation as
//! OpenTimelineIO computes it, beside the time it stands for, exactly
//! (`mediatime`'s [`ExactSeconds`]). Every value the walk forms is held
//! ([`Time::hold`]): within ±2^53 in the ruler OpenTimelineIO carries it in,
//! where an `f64` holds every whole count, and OpenTimelineIO's own double
//! less than half a tick from the exact count — read to the nearest tick, it
//! is the exact count. On one ruler, where nothing is rescaled, the sums are
//! of whole counts and the doubles are the counts themselves — so a range the
//! document writes in one ruler, its start and duration whole and its end
//! within ±2^53, also ends inclusively where it exactly does: its duration is
//! whole, it takes the less-one branch exactly when it is longer than one
//! tick, and every sum on the way is exact. The available and gap ranges the
//! export writes are such ranges, held whole as they are planned; a clip's
//! source range is planned with its start and its length within ±2^53, and
//! its end and last tick are held by the walk ([`range`]), so the search can
//! write the clip in a ruler that holds them. The reader holds what it reads
//! to the same arithmetic: every range by both its ends ([`range_held`]),
//! and each track and the stack by the walk the export makes of them
//! ([`track`], [`stack`]).
//!
//! The lines cited are OpenTimelineIO's `main` at `00c22fa` (2026-10-09),
//! and the tools' heads of the same day.

use core::cmp::Ordering;
use std::vec::Vec;

use mediatime::{ExactSeconds, Rate, Timestamp};

use super::{ChildAt, NotRepresentable, Spot};

/// 2^53: an `f64` holds every whole number of no greater magnitude.
const EXACT: i128 = 1 << 53;

/// How far short of half a tick [`near`] holds a double: more than the
/// three roundings of its own arithmetic add up to (each under 2^-51 of a
/// tick), so it never holds a double half a tick off.
const MARGIN: f64 = 1.0 / 281_474_976_710_656.0;

/// 2^52: from it on an `f64` has no fraction, and is its own floor.
const WHOLE: f64 = 4_503_599_627_370_496.0;

/// A ruler: the rate OpenTimelineIO reads — the `f64` nearest the exact rate
/// — and the exact rate it stands for.
#[derive(Debug, Clone, Copy)]
pub(super) struct Ruler {
  otio: f64,
  exact: Rate,
}

impl Ruler {
  pub(super) const fn new(exact: Rate) -> Self {
    Self {
      otio: exact.as_f64(),
      exact,
    }
  }

  /// The rate OpenTimelineIO reads.
  pub(super) const fn otio(self) -> f64 {
    self.otio
  }

  /// The exact rate.
  pub(super) const fn exact(self) -> Rate {
    self.exact
  }
}

/// A `RationalTime` as OpenTimelineIO computes it, beside the time it stands
/// for.
#[derive(Debug, Clone, Copy)]
pub(super) struct Time {
  /// OpenTimelineIO's value.
  value: f64,
  /// OpenTimelineIO's rate, and the exact rate it stands for.
  ruler: Ruler,
  /// The time, exactly; `None` once an exact sum leaves `i128`, which the
  /// hold then refuses.
  seconds: Option<ExactSeconds>,
}

impl Time {
  /// `count` ticks of `ruler`, as the document writes them: a count within
  /// ±2^53, which an `f64` reads exactly.
  pub(super) fn written(count: i128, ruler: Ruler) -> Self {
    let seconds = i64::try_from(count).ok().and_then(|pts| {
      let timebase = ruler.exact.checked_to_timebase()?;
      Some(ExactSeconds::from_timestamp(Timestamp::new(pts, timebase)))
    });
    Self {
      value: count as f64,
      ruler,
      seconds,
    }
  }

  /// No time, in `ruler`: `RationalTime(0, rate)`.
  pub(super) fn zero(ruler: Ruler) -> Self {
    Self::written(0, ruler)
  }

  /// `value_rescaled_to` (`rationalTime.h` 70–75): the value unchanged in an
  /// equal rate, else multiplied by the new rate and divided by the old.
  fn value_rescaled_to(self, rate: f64) -> f64 {
    if rate == self.ruler.otio {
      self.value
    } else if self.ruler.otio > 0.0 {
      (self.value * rate) / self.ruler.otio
    } else {
      0.0
    }
  }

  /// `rescaled_to` (`rationalTime.h` 58–67): the same time in `ruler`.
  fn rescaled_to(self, ruler: Ruler) -> Self {
    Self {
      value: self.value_rescaled_to(ruler.otio),
      ruler,
      seconds: self.seconds,
    }
  }

  /// `operator+` (`rationalTime.h` 316–326; `+=`, 286–298, is the same
  /// arithmetic): carried in the higher rate, the left one's on a tie.
  pub(super) fn add(self, other: Self) -> Self {
    let seconds = match (self.seconds, other.seconds) {
      (Some(a), Some(b)) => a.checked_add(b),
      _ => None,
    };
    if self.ruler.otio < other.ruler.otio {
      Self {
        value: self.value_rescaled_to(other.ruler.otio) + other.value,
        ruler: other.ruler,
        seconds,
      }
    } else {
      Self {
        value: other.value_rescaled_to(self.ruler.otio) + self.value,
        ruler: self.ruler,
        seconds,
      }
    }
  }

  /// `operator-` (`rationalTime.h` 329–339; `-=`, 301–313, is the same
  /// arithmetic).
  pub(super) fn sub(self, other: Self) -> Self {
    let seconds = match (self.seconds, other.seconds) {
      (Some(a), Some(b)) => a.checked_sub(b),
      _ => None,
    };
    if self.ruler.otio < other.ruler.otio {
      Self {
        value: self.value_rescaled_to(other.ruler.otio) - other.value,
        ruler: other.ruler,
        seconds,
      }
    } else {
      Self {
        value: self.value - other.value_rescaled_to(self.ruler.otio),
        ruler: self.ruler,
        seconds,
      }
    }
  }

  /// `TimeRange::end_time_exclusive` (`timeRange.h` 105–108): the duration
  /// plus the start rescaled to the duration's rate.
  pub(super) fn end(start: Self, duration: Self) -> Self {
    duration.add(start.rescaled_to(duration.ruler))
  }

  /// `TimeRange::end_time_inclusive` (`timeRange.h` 88–102) of the range
  /// from `start` running `duration`: the range's end, in the duration's
  /// rate, then — where the end less the start, rescaled there, is more than
  /// one tick — the end floored if the duration's value is not whole, else
  /// the end less one tick (`RationalTime(1, duration.rate())`); a range of
  /// one tick or none ends inclusively at its start, in the start's rate.
  ///
  /// OpenTimelineIO takes each branch on its own doubles; the exact time
  /// takes the same branches on the exact counts — a duration more than one
  /// tick long, whole or not — which is where the range's last tick exactly
  /// is. A double that rounds onto the other branch is held against that.
  pub(super) fn end_inclusive(start: Self, duration: Self) -> Self {
    let end = Self::end(start, duration);
    let floored = end.floor();
    let less_one = end.sub(Self::written(1, duration.ruler));
    let otio = if end.sub(start.rescaled_to(duration.ruler)).value > 1.0 {
      if duration.value != floor(duration.value) {
        floored
      } else {
        less_one
      }
    } else {
      start
    };
    let seconds = match duration.exact_count() {
      Some((num, den)) if num > den => {
        if num.rem_euclid(den) == 0 {
          less_one.seconds
        } else {
          floored.seconds
        }
      }
      Some(_) => start.seconds,
      None => None,
    };
    Self {
      value: otio.value,
      ruler: otio.ruler,
      seconds,
    }
  }

  /// `RationalTime::floor` (`rationalTime.h` 104–107): the value floored,
  /// in the same rate — and the exact count floored beside it.
  fn floor(self) -> Self {
    let seconds = self.exact_count().and_then(|(num, den)| {
      let whole = i64::try_from(num.div_euclid(den)).ok()?;
      let tick = self.ruler.exact.checked_to_timebase()?;
      Some(ExactSeconds::from_timestamp(Timestamp::new(whole, tick)))
    });
    Self {
      value: floor(self.value),
      ruler: self.ruler,
      seconds,
    }
  }

  /// `operator<` (`rationalTime.h` 353–364): `!(a >= b)`, `value / rate`
  /// compared in `f64` — so also where either is not a number.
  fn otio_lt(self, other: Self) -> bool {
    let (seconds, other_seconds) = (self.value / self.ruler.otio, other.value / other.ruler.otio);
    !matches!(
      seconds.partial_cmp(&other_seconds),
      Some(Ordering::Greater | Ordering::Equal)
    )
  }

  /// The exact count in this ruler — the time times the exact rate — as a
  /// numerator and a positive denominator in lowest terms, or `None` past
  /// `i128`: `mediatime`'s `Rate::checked_count`.
  fn exact_count(self) -> Option<(i128, i128)> {
    let (num, den) = self.ruler.exact.checked_count(self.seconds?)?;
    Some((num, den.get()))
  }

  /// Holds the value at `at`: its exact count within ±2^53, and
  /// OpenTimelineIO's double less than half a tick from it. Refused with
  /// the exact count to the nearest tick — or, past `i128`, the double.
  pub(super) fn hold(self, at: Spot) -> Result<(), NotRepresentable> {
    if self.held() {
      Ok(())
    } else {
      Err(self.refused(at))
    }
  }

  /// Whether [`hold`](Self::hold) holds the value.
  fn held(self) -> bool {
    self.exact_count().is_some_and(|(num, den)| {
      let (whole, part) = (num.div_euclid(den), num.rem_euclid(den));
      let within = (-EXACT..EXACT).contains(&whole) || (whole == EXACT && part == 0);
      within && near(self.value, whole, part, den)
    })
  }

  /// The refusal of this value at `at`: its exact count to the nearest tick
  /// (half a tick rounds up), or the double where it has no exact count.
  fn refused(self, at: Spot) -> NotRepresentable {
    let value = match self.exact_count() {
      Some((num, den)) => {
        let (whole, part) = (num.div_euclid(den), num.rem_euclid(den));
        // `part` against half of `den`, in magnitudes that cannot overflow.
        if part.unsigned_abs() >= den.unsigned_abs() - part.unsigned_abs() {
          whole.saturating_add(1)
        } else {
          whole
        }
      }
      None => self.value as i128,
    };
    NotRepresentable {
      at,
      value,
      rate: self.ruler.exact,
      searched: None,
      misread: None,
    }
  }
}

/// Whether the double `value` lies less than half a tick from `whole +
/// part / den` (`whole` within ±2^53, `0 ≤ part < den`).
///
/// In `f64`, with a margin for the check's own rounding: `whole` is an `f64`
/// exactly, so `value - whole` rounds once (under 2^-51 while it is under 4),
/// `part / den` at most three times in all (under 2^-51, being under 1), and
/// their difference once more (under 2^-51) — together under 2^-49, inside
/// [`MARGIN`]. A double half a tick off is never held; one closer than half
/// a tick less 2^-47 always is.
fn near(value: f64, whole: i128, part: i128, den: i128) -> bool {
  let from_whole = value - whole as f64;
  if !(from_whole > -4.0 && from_whole < 4.0) {
    return false;
  }
  let off = from_whole - part as f64 / den as f64;
  let bound = 0.5 - MARGIN;
  off > -bound && off < bound
}

/// `std::floor`, which `core` lacks: the greatest whole `f64` not above
/// `value`. Past ±2^52 an `f64` is whole already, and an infinity or a NaN
/// is its own floor, as `std::floor` answers them.
pub(super) fn floor(value: f64) -> f64 {
  if !(value > -WHOLE && value < WHOLE) {
    return value;
  }
  // Within ±2^52 the cast truncates toward zero, exactly.
  let truncated = value as i64 as f64;
  if truncated > value {
    truncated - 1.0
  } else {
    truncated
  }
}

/// One child of a track, as the walk reads it.
#[derive(Debug, Clone, Copy)]
pub(super) enum Child {
  /// An item — a gap or a clip: its source range's start and duration.
  Item { start: Time, duration: Time },
  /// A transition: how far it reaches before its cut and after it.
  Transition { in_offset: Time, out_offset: Time },
}

impl Child {
  /// `Composable::duration`: an item's source range's duration
  /// (`item.cpp` 44–48), a transition's two offsets added
  /// (`transition.cpp` 52–56).
  fn duration(self) -> Time {
    match self {
      Self::Item { duration, .. } => duration,
      Self::Transition {
        in_offset,
        out_offset,
      } => in_offset.add(out_offset),
    }
  }

  /// Whether OpenTimelineIO lays the child over its neighbours rather than
  /// after them: a transition (`transition.cpp` 26–30, `item.cpp` 38–42).
  const fn overlapping(self) -> bool {
    matches!(self, Self::Transition { .. })
  }
}

/// Walks one track — `children` as the export writes them, `index` its
/// place in the stack, `start` the global start — holding every value
/// OpenTimelineIO derives from it: each child's place on the track and in
/// the timeline, the running end of a walk over the whole track, each
/// item's visible range, the place of each from the global start and its
/// range from there — the moved start, the child's own duration — the
/// track's duration and its end from the global start, and the last tick of
/// every range among them ([`Time::end_inclusive`]). Answers the track's
/// duration.
pub(super) fn track(
  index: usize,
  children: &[Child],
  start: Time,
) -> Result<Time, NotRepresentable> {
  let at = |child| ChildAt::new(index, child);
  let duration = track_duration(children, |_| Ok(()))?;
  // The stack places the track at zero in its duration's rate
  // (`stack.cpp` 41–62): a child's place in the timeline adds that zero
  // (`composition.cpp` 357–359).
  let placed = Time::zero(duration.ruler);
  // One running sum per rate a child's place starts in, advanced as the
  // children are reached: `range_of_child_at_index` adds the items before a
  // child from zero in that child's own rate.
  let mut sums: Vec<(Time, usize)> = Vec::new();
  // `range_of_all_children` starts at zero in the first child's rate.
  let mut last_end = match children.first() {
    Some(Child::Transition { in_offset, .. }) => Time::zero(in_offset.ruler),
    Some(Child::Item { duration, .. }) => Time::zero(duration.ruler),
    None => Time::zero(Ruler::new(Rate::hz(1))),
  };
  for (k, &child) in children.iter().enumerate() {
    let spot = Spot::TrackPosition(at(k));
    let length = child.duration();
    length.hold(spot)?;
    // `range_of_child_at_index` (`track.cpp` 51–92).
    let ruler = length.ruler;
    let slot = match sums.iter().position(|(sum, _)| {
      sum.ruler.otio.to_bits() == ruler.otio.to_bits() && sum.ruler.exact == ruler.exact
    }) {
      Some(slot) => slot,
      None => {
        sums.push((Time::zero(ruler), 0));
        sums.len() - 1
      }
    };
    let (mut sum, from) = sums[slot];
    for earlier in &children[from..k] {
      if !earlier.overlapping() {
        sum = sum.add(earlier.duration());
        sum.hold(spot)?;
      }
    }
    sums[slot] = (sum, k);
    let begins = match child {
      Child::Transition { in_offset, .. } => sum.sub(in_offset),
      Child::Item { .. } => sum,
    };
    let ends = Time::end(begins, length);
    begins.hold(spot)?;
    ends.hold(spot)?;
    Time::end_inclusive(begins, length).hold(spot)?;
    // The same in the timeline, and from the global start.
    let begins = begins.add(placed);
    let ends = Time::end(begins, length);
    begins.hold(spot)?;
    ends.hold(spot)?;
    Time::end_inclusive(begins, length).hold(spot)?;
    let absolute = Spot::Absolute(at(k));
    let from = start.add(begins);
    from.hold(absolute)?;
    start.add(ends).hold(absolute)?;
    // The child's range from the global start: the moved start and the
    // child's own duration (`composition.cpp` 357–359), its end the
    // duration plus that start rescaled to the duration's rate.
    Time::end(from, length).hold(absolute)?;
    Time::end_inclusive(from, length).hold(absolute)?;
    // `range_of_all_children` (`track.cpp` 221–271).
    match child {
      Child::Transition {
        in_offset,
        out_offset,
      } => {
        let (begins, length) = (last_end.sub(in_offset), out_offset.add(in_offset));
        begins.hold(spot)?;
        length.hold(spot)?;
        Time::end(begins, length).hold(spot)?;
        Time::end_inclusive(begins, length).hold(spot)?;
      }
      Child::Item { .. } => {
        Time::end_inclusive(last_end, length).hold(spot)?;
        last_end = Time::end(last_end, length);
        last_end.hold(spot)?;
      }
    }
    // `visible_range` (`item.cpp` 60–85): the handles are the `in_offset` of
    // a transition before the item and the `out_offset` of one after it
    // (`track.cpp` 150–165).
    if let Child::Item {
      start: from,
      duration: length,
    } = child
    {
      let head = match k.checked_sub(1).map(|before| children[before]) {
        Some(Child::Transition { in_offset, .. }) => Some(in_offset),
        _ => None,
      };
      let tail = match children.get(k + 1) {
        Some(Child::Transition { out_offset, .. }) => Some(*out_offset),
        _ => None,
      };
      if head.is_some() || tail.is_some() {
        visible(from, length, head, tail, Spot::Visible(at(k)))?;
      }
    }
  }
  let spot = Spot::TrackDuration(index);
  let duration = track_duration(children, |sum| sum.hold(spot))?;
  // The track's range, from zero in its duration's rate (`track.cpp`
  // 147): the stack's range of the track too (`stack.cpp` 61).
  Time::end_inclusive(placed, duration).hold(spot)?;
  from_the_global_start(start, duration, Spot::AbsoluteEnd(index))?;
  Ok(duration)
}

/// The end of a range `duration` long from the global start `start` — a
/// track's, or a timeline's with no track — each value held at `spot`: the
/// start rescaled to the duration's rate, the range's end and its last
/// tick; the same of the range from the stack's start of it, zero in the
/// duration's rate, with the global start added; and the global start plus
/// the duration, by `operator+`.
fn from_the_global_start(start: Time, duration: Time, spot: Spot) -> Result<(), NotRepresentable> {
  start.rescaled_to(duration.ruler).hold(spot)?;
  Time::end(start, duration).hold(spot)?;
  Time::end_inclusive(start, duration).hold(spot)?;
  let from = Time::zero(duration.ruler).add(start);
  from.hold(spot)?;
  Time::end(from, duration).hold(spot)?;
  Time::end_inclusive(from, duration).hold(spot)?;
  start.add(duration).hold(spot)
}

/// A range the document writes in one ruler — a clip's source range, its
/// start and its duration whole counts within ±2^53 — held at `spot` where
/// OpenTimelineIO derives from it: its end, the duration plus the start
/// (`timeRange.h` 105–108), and its last tick (88–102). In one ruler each
/// is the exact count wherever it lies within ±2^53.
pub(super) fn range(start: Time, duration: Time, spot: Spot) -> Result<(), NotRepresentable> {
  ends(start, duration)
    .into_iter()
    .try_for_each(|end| end.hold(spot))
}

/// Whether [`range`] holds the range from `start` running `duration`: for
/// the reader, which names a range by where the document writes it.
pub(super) fn range_held(start: Time, duration: Time) -> bool {
  ends(start, duration).into_iter().all(Time::held)
}

/// The two ends OpenTimelineIO derives from the range from `start` running
/// `duration`: exclusive, then its last tick.
fn ends(start: Time, duration: Time) -> [Time; 2] {
  [
    Time::end(start, duration),
    Time::end_inclusive(start, duration),
  ]
}

/// `Track::available_range` (`track.cpp` 117–148): from zero at rate 1,
/// every item's duration added, and the offsets of a transition at either
/// end — each sum handed to `hold`.
fn track_duration(
  children: &[Child],
  mut hold: impl FnMut(Time) -> Result<(), NotRepresentable>,
) -> Result<Time, NotRepresentable> {
  let mut sum = Time::zero(Ruler::new(Rate::hz(1)));
  for child in children {
    if let Child::Item { duration, .. } = child {
      sum = sum.add(*duration);
      hold(sum)?;
    }
  }
  if let Some(Child::Transition { in_offset, .. }) = children.first() {
    sum = sum.add(*in_offset);
    hold(sum)?;
  }
  if let Some(Child::Transition { out_offset, .. }) = children.last() {
    sum = sum.add(*out_offset);
    hold(sum)?;
  }
  Ok(sum)
}

/// `visible_range` of an item whose source range starts at `start` and runs
/// `duration`, with the `head` before it and the `tail` after it.
fn visible(
  start: Time,
  duration: Time,
  head: Option<Time>,
  tail: Option<Time>,
  spot: Spot,
) -> Result<(), NotRepresentable> {
  let (mut start, mut duration) = (start, duration);
  if let Some(head) = head {
    start = start.sub(head);
    duration = duration.add(head);
    start.hold(spot)?;
    duration.hold(spot)?;
  }
  if let Some(tail) = tail {
    duration = duration.add(tail);
    duration.hold(spot)?;
  }
  Time::end(start, duration).hold(spot)?;
  Time::end_inclusive(start, duration).hold(spot)
}

/// `Stack::available_range` (`stack.cpp` 118–134): the longest of the
/// tracks' `durations`, picked as OpenTimelineIO picks it —
/// `std::max` by `operator<`, the first of two that compare equal. Refused
/// where that pick is shorter than the longest track, exactly: the stack,
/// and the timeline with it, would end early. The stack's range runs the
/// picked duration from zero in its rate: the picked track's own range,
/// whose ends [`track`] has held, from zero and from the global start.
///
/// A stack with no track has the range `TimeRange()` (`stack.cpp`
/// 121–123): no duration, at rate 1. Its range from the global start
/// `start` — the timeline's — is held as a track's end is, at
/// [`Spot::TimelineEnd`].
pub(super) fn stack(durations: &[Time], start: Time) -> Result<(), NotRepresentable> {
  let Some((&first, rest)) = durations.split_first() else {
    let none = Time::zero(Ruler::new(Rate::hz(1)));
    return from_the_global_start(start, none, Spot::TimelineEnd);
  };
  let (mut picked, mut longest) = (first, first);
  for &duration in rest {
    if picked.otio_lt(duration) {
      picked = duration;
    }
    if exact_cmp(longest, duration) == Some(Ordering::Less) {
      longest = duration;
    }
  }
  if exact_cmp(picked, longest) == Some(Ordering::Equal) {
    Ok(())
  } else {
    Err(longest.refused(Spot::StackDuration))
  }
}

/// The two times compared exactly, or `None` where one has no exact value.
fn exact_cmp(a: Time, b: Time) -> Option<Ordering> {
  Some(a.seconds?.cmp(&b.seconds?))
}

#[cfg(test)]
mod tests;
