//! A valid timeline as OpenTimelineIO's object tree.
//!
//! Every object is written with its keys in the order OpenTimelineIO's own
//! writer uses: `OTIO_SCHEMA` first, then each base class's fields before
//! the derived class's.
//!
//! Every track is planned, then walked, then written. The plan counts every
//! child — each count written within ±2^53, the whole numbers an `f64` holds
//! exactly; a clip's source range in the first of its own rulers that writes
//! it, or else in the first ruler the timeline's operands are counted in
//! that does ([`source_plan`]); its medium's available range likewise, in the
//! first that holds it whole, its end with it ([`whole_plan`]), and each of
//! its markers the same way; the timeline's markers and a track's at the
//! edit rate — and the walk, [`mod@derive`], forms what OpenTimelineIO
//! derives from those counts, in its own arithmetic, holding each: a source
//! range's end among them, which the plan leaves to the walk. Where the
//! walk refuses a value, a search ([`settle`]) writes the clips it is formed
//! from in other rulers that hold their source ranges — every ruler the
//! timeline's own operands are counted in, and the finest and the coarsest
//! whole ones below a clip's plan's — one clip one ruler at a time, and
//! walks again; what no plan it tries holds is refused, with the last
//! walk's refusal and what the search tried.
//!
//! Last, every number the document would carry is spelled so that
//! OpenTimelineIO 0.17.0's reader reads it back as the value meant. That
//! reader, RapidJSON in its normal precision, reads a count written
//! `<digits>.0` exactly up to 1 801 439 850 948 198, ⌊2^54 / 10⌋, and past
//! it not always — the fraction's `0` joins the significand, which converts
//! to a double rounded past 2^53 before it is divided by ten — so a count
//! past it is written as its integer, which it hands over whole
//! ([`number`]). It reads the shortest spelling of the `f64` of most rates
//! exactly, but not of every one: 96000/1001's it reads as a neighbouring
//! `f64`, while every common rate reads exactly. Such a rate is written in
//! another spelling of the same `f64` that a build rounding its
//! multiply-adds twice and one fusing them both read exactly
//! ([`rate_spelling`]); a rate none of those spellings writes is refused,
//! with no search ([`spellings`], [`read_back`]).
//!
//! OpenTimelineIO keeps a rate as an `f64`, which names many rationals, and
//! a range as two counts in one ruler, which says nothing of the timebase
//! the model counts it in. So beside what OpenTimelineIO reads, the export
//! writes the exact words a reader needs to rebuild the timeline exactly,
//! under `metadata.mediaio` ([`OURS`]): the edit rate (`rate`); for each
//! media-side range — a clip's source range, its medium's available range,
//! a clip's marker — the exact ruler its counts are written in and the
//! timebase the model counts it in (`source`, `available`, `range`); and,
//! on a track whose transitions are kept out of their cuts' order, each
//! dissolve's place among them (`order`). OpenTimelineIO's own fields stay
//! the authority for every count; a reader holds each written rate to its
//! exact word. Beside those words, in the same `metadata`, it writes each
//! other reader's namespace the timeline, a track, a clip or a marker holds
//! ([`ReaderMetadata`]), whole ([`metadata`]).

use core::cmp::Reverse;
use std::{collections::BTreeSet, vec, vec::Vec};

use mediatime::{Duration, Rate, Rounding, TimeRange, Timebase};
use smol_bytes::{Utf8Bytes, format_utf8bytes};

use super::{
  Misread, MisreadNumber, NotRepresentable, OtioTarget, RulerSearch, Spot,
  derive::{self, Child, Ruler, Time},
  json::{self, Number, Value},
};
use crate::{
  Clip, ClipAt, Edge, EdgeAt, Fade, FadeShape, Item, Layout, Marker, MarkerAt, MediaRef, Metadata,
  ReaderMetadata, Timeline, Track, TrackKind, TrackLayout, Transition, TransitionAt,
};

pub(super) use crate::metadata::OURS;

/// The key a `Clip.2` keeps its one media reference under.
pub(super) const DEFAULT_MEDIA: &str = "DEFAULT_MEDIA";

/// 2^53: an `f64` holds every whole number of no greater magnitude, and past
/// it only some — a count there is read rounded.
const EXACT: u128 = 1 << 53;

pub(crate) fn timeline(
  timeline: &Timeline,
  laid: &Layout<'_>,
  target: OtioTarget,
) -> Result<Value, NotRepresentable> {
  let edit = Ruler::new(timeline.rate());
  let start = i128::from(timeline.start().pts());
  held(start, edit, Spot::Start)?;
  let stacked = timeline
    .markers()
    .iter()
    .enumerate()
    .map(|(marker, placed)| edit_marker(placed, edit, Spot::Marker(MarkerAt::Timeline { marker })))
    .collect::<Result<Vec<_>, _>>()?;
  let operands = operands(laid, edit);
  let planned = laid
    .tracks()
    .iter()
    .enumerate()
    .map(|(index, laid)| plan(index, laid, edit, &operands))
    .collect::<Result<Vec<_>, _>>()?;
  let marked = laid
    .tracks()
    .iter()
    .enumerate()
    .map(|(index, laid)| markers_planned(index, laid.track(), edit, &operands))
    .collect::<Result<Vec<_>, _>>()?;
  let settled = settle(planned, edit, &operands, Time::written(start, edit))?;
  spellings(start, edit, &stacked, &settled, &marked)?;
  // A valid timeline states its rate, and the reciprocal of a rate above
  // zero is a timebase.
  let ticks = timeline.rate().checked_to_timebase().unwrap_or_default();
  let tracks = laid
    .tracks()
    .iter()
    .zip(&settled)
    .zip(&marked)
    .map(|((laid, children), marked)| {
      track(laid.track(), children, marked, Edit { edit, ticks }, target)
    })
    .collect();
  let mut rate = vec![(
    "rate",
    Value::String(format_utf8bytes!("{}", timeline.rate())),
  )];
  rate.extend(spelled(timeline.start().timebase(), ticks).map(|start| ("start", start)));
  Ok(object(vec![
    ("OTIO_SCHEMA", text("Timeline.1")),
    (
      "metadata",
      metadata(rate, timeline.metadata(), timeline.readers()),
    ),
    ("name", text(timeline.name())),
    ("global_start_time", rational_time(edit, start)),
    (
      "tracks",
      object(vec![
        ("OTIO_SCHEMA", text("Stack.1")),
        ("metadata", Value::Object(Vec::new())),
        ("name", text("tracks")),
        ("source_range", Value::Null),
        ("effects", Value::Array(Vec::new())),
        (
          "markers",
          Value::Array(edit_markers(timeline.markers(), &stacked, ticks)),
        ),
        ("enabled", Value::Bool(true)),
        ("children", Value::Array(tracks)),
      ]),
    ),
  ]))
}

/// One child of a track as the export writes it, counted.
#[derive(Clone, Copy)]
enum Planned<'a> {
  /// A gap of so many edit-rate ticks.
  Gap(u64),
  /// A clip, its source range and its medium's available range counted
  /// each in one ruler.
  Clip {
    clip: &'a Clip,
    source: Counted,
    available: Option<Counted>,
  },
  /// A fade at one edge of a clip, as a dissolve against a gap.
  Fade(Fade, Edge),
  /// A transition between two clips, and its index among its track's.
  Dissolve(usize, &'a Transition),
}

/// A track's children in OpenTimelineIO's order — items end to end, each
/// transition between the two items it joins — each count written held within
/// ±2^53, a clip's source range planned among the timeline's `operands` where
/// none of its own rulers writes it ([`source_plan`]), and its medium's
/// available range where none holds it whole ([`whole_plan`]).
///
/// A fade is a dissolve against a gap: a fade-in after the gap before its
/// clip, a fade-out before the gap after it. Where the clip abuts another
/// clip or a track end, a gap of no length stands in, so the fade always has
/// black (or silence) on its other side and takes no time from a neighbour.
fn plan<'a>(
  track_index: usize,
  laid: &TrackLayout<'a>,
  edit: Ruler,
  operands: &BTreeSet<Rate>,
) -> Result<Vec<Planned<'a>>, NotRepresentable> {
  let track = laid.track();
  let items = laid.items();
  let mut out = Vec::with_capacity(items.len() * 2);
  let mut after_gap = false;
  // The clip the walk reaches next. The layout derives a gap only before a
  // clip, and a gap is named by the clip it leads to.
  let mut next = 0;
  for (index, &item) in items.iter().enumerate() {
    let at = ClipAt::new(track_index, next);
    match item {
      Item::Gap(range) => {
        let length = range.span().ticks();
        held(i128::from(length), edit, Spot::Record(at))?;
        out.push(Planned::Gap(length));
        after_gap = true;
      }
      Item::Clip(clip) => {
        next += 1;
        let fades = clip.fades();
        if let Some(fade) = fades.in_() {
          if !after_gap {
            out.push(Planned::Gap(0));
          }
          out.push(fade_planned(fade, EdgeAt::new(at, Edge::In), edit)?);
        }
        out.push(clip_planned(clip, at, operands)?);
        after_gap = false;
        if let Some(fade) = fades.out() {
          out.push(fade_planned(fade, EdgeAt::new(at, Edge::Out), edit)?);
          if !matches!(items.get(index + 1), Some(Item::Gap(_))) {
            out.push(Planned::Gap(0));
            after_gap = true;
          }
        } else if let Some((which, transition)) = leaving(track, clip) {
          let spot = Spot::Transition(TransitionAt::new(track_index, which));
          held(i128::from(transition.in_offset().ticks()), edit, spot)?;
          held(i128::from(transition.out_offset().ticks()), edit, spot)?;
          out.push(Planned::Dissolve(which, transition));
        }
      }
    }
  }
  Ok(out)
}

/// The transition at the cut where `clip`'s record ends, and its index.
fn leaving<'a>(track: &'a Track, clip: &Clip) -> Option<(usize, &'a Transition)> {
  let end = clip.record().end();
  track
    .transitions()
    .iter()
    .enumerate()
    .find(|(_, transition)| transition.at() == end)
}

fn fade_planned(fade: Fade, at: EdgeAt, edit: Ruler) -> Result<Planned<'static>, NotRepresentable> {
  held(i128::from(fade.duration().ticks()), edit, Spot::Fade(at))?;
  Ok(Planned::Fade(fade, at.edge()))
}

/// The source range whole, start and length in one ruler ([`source_plan`]),
/// so the end OpenTimelineIO derives from it — the start rescaled to the
/// duration's rate, plus the duration — is the stored end. Validation holds
/// the source's length to a whole number of edit-rate ticks, so the same
/// duration is the record's length: the clip fills exactly its record. The
/// medium's available range, where it is known, in the first of its own
/// rulers, else of the rulers the timeline's `operands` are counted in, that
/// holds it whole ([`whole_plan`]).
fn clip_planned<'a>(
  clip: &'a Clip,
  at: ClipAt,
  operands: &BTreeSet<Rate>,
) -> Result<Planned<'a>, NotRepresentable> {
  let media = clip.media();
  let source = source_plan(
    clip.source_range(),
    media.rate(),
    Spot::Source(at),
    operands,
  )?;
  let available = media
    .available_range()
    .map(|range| whole_plan(range, media.rate(), Spot::Available(at), operands))
    .transpose()?;
  Ok(Planned::Clip {
    clip,
    source,
    available,
  })
}

/// How many free rulers each of a clip's two free bands holds — whole rates
/// below its plan's that hold its source range and that no operand of the
/// timeline is counted in: the finest so many, just below its plan's, and
/// the coarsest, which shrink its counts the most. A ruler an operand is
/// counted in is never capped ([`rulers`]).
pub(super) const FREE: usize = 64;

/// A clip the search may write in another ruler: its track, its index among
/// the track's planned children — which is its index among the track's
/// OpenTimelineIO children, [`ChildAt::child`](super::ChildAt::child) — its
/// index among the track's clips ([`ClipAt::clip`]), the rulers it may be
/// written in, its plan's first ([`rulers`]), and the one it is written in.
struct Choice {
  track: usize,
  child: usize,
  clip: usize,
  rulers: Vec<Counted>,
  at: usize,
}

/// The tracks as planned, each clip's source range in the ruler the search
/// settles it in — or the refusal of the last walk the search makes, with
/// what the search tried ([`RulerSearch`]).
///
/// The search starts with every clip in its plan's ruler and walks the
/// tracks in order, then the stack ([`walk`]). Where a walk refuses a
/// value, one clip is written in the next ruler of its list and the walk is
/// made again: of the clips the refused value is formed from, the one whose
/// ruler is now the finest ([`step`]) — the finest rate among the times
/// OpenTimelineIO adds is the one it carries their sum in. A clip moves only
/// forward along its list and never back, and a track a walk has held is
/// walked again only once one of its clips moves. The search ends at the
/// first walk that holds every value, or refuses with the walk after which
/// no clip the refused value names can move — of its track; its own clip
/// alone, for a source range's end; of the timeline, for the stack's
/// duration; none, for the end of a timeline with no track: the last walk,
/// every one of those clips at the end of its list.
///
/// A clip's list ([`rulers`]) runs its plan's ruler, then three bands
/// ([`RulerBand`](super::RulerBand)): every ruler the timeline's operands
/// are counted in that holds the clip's source range — the edit rate, at
/// which the global start, the gaps and the transitions are written; the
/// rate 1 a track's duration is summed from; every clip's planned ruler —
/// however many, finest first; then, of the whole rates below its plan's
/// that hold the range and that no operand is counted in, the [`FREE`]
/// finest, finest first, and the [`FREE`] coarsest not among them, finest
/// first; and its plan's again, last, where every other is finer. So no
/// ruler the timeline's own operands are counted in is dropped from a clip
/// it holds — where OpenTimelineIO would rescale a clip into the edit rate,
/// or into a neighbour's planned ruler, the search can write the clip in
/// that ruler itself — and the whole rates just below a clip's plan's are
/// tried as well as the coarsest. The search is complete over these bands,
/// not over every ruler: a timeline that only a ruler outside them, or a
/// plan of rulers the moves do not reach, would hold is refused, by
/// contract, and the refusal says so.
///
/// It ends, and within a bound. Let `d` be the number of distinct rates the
/// timeline's operands are counted in ([`operands`]): the edit rate, 1, and
/// each clip's planned ruler — at most `n + 2` for `n` clips, and in
/// practice the few rates the media are counted in. A clip's list holds its
/// plan's ruler, at most `d − 1` named others and at most `2 · FREE` free
/// ones, a ruler of both free bands once; its plan's again, last, only where
/// every other is finer, and so with no free one: at most `d + 2 · FREE`
/// rulers. Each refused walk either moves one clip one place along its list
/// or ends the search. The clips' places, summed, start at zero, rise by one
/// at each move and never pass `n · (d + 2 · FREE − 1)`: at most that many
/// moves, so at most `1 + n · (d + 2 · FREE − 1)` walks — the count a
/// refusal reports ([`RulerSearch::walks`]). Within them a track is walked
/// once, and once more each time one of its clips moves — at most
/// `t + n · (d + 2 · FREE − 1)` walks of a track, for `t` tracks. The lists
/// are built at the first refusal: a timeline the first walk holds costs
/// that walk alone.
fn settle<'a>(
  mut tracks: Vec<Vec<Planned<'a>>>,
  edit: Ruler,
  operands: &BTreeSet<Rate>,
  global: Time,
) -> Result<Vec<Vec<Planned<'a>>>, NotRepresentable> {
  let mut durations = vec![None; tracks.len()];
  let mut lists = None;
  let mut walks = 0;
  loop {
    walks += 1;
    let refusal = match walk(&tracks, &mut durations, edit, global) {
      Ok(()) => return Ok(tracks),
      Err(refusal) => refusal,
    };
    let choices = lists.get_or_insert_with(|| self::choices(&tracks, operands));
    let Some(moved) = step(choices, refusal.at) else {
      return Err(NotRepresentable {
        searched: Some(RulerSearch { walks }),
        ..refusal
      });
    };
    let choice = &mut choices[moved];
    choice.at += 1;
    if let Planned::Clip { source, .. } = &mut tracks[choice.track][choice.child] {
      *source = choice.rulers[choice.at];
    }
    durations[choice.track] = None;
  }
}

/// Walks each track not yet walked in its present plan, in order — its
/// clips' source ranges ([`sources`]), then what OpenTimelineIO derives
/// from its children ([`derive::track`]) — keeping its duration as
/// OpenTimelineIO sums it in `durations`, then the stack, from the global
/// start where it has no track.
fn walk(
  tracks: &[Vec<Planned<'_>>],
  durations: &mut [Option<Time>],
  edit: Ruler,
  global: Time,
) -> Result<(), NotRepresentable> {
  for (index, (children, duration)) in tracks.iter().zip(durations.iter_mut()).enumerate() {
    if duration.is_none() {
      sources(index, children)?;
      *duration = Some(derive::track(index, &walked(children, edit), global)?);
    }
  }
  let durations: Vec<Time> = durations.iter().flatten().copied().collect();
  derive::stack(&durations, global)
}

/// Holds the end and the last tick of the source range of each clip of
/// track `index`, as planned in `children` ([`derive::range`]): its plan
/// writes the start and the length within ±2^53, and OpenTimelineIO
/// derives the end from them — past 2^53 in a ruler that writes the range
/// but cannot end it, which the search then moves the clip from.
fn sources(index: usize, children: &[Planned<'_>]) -> Result<(), NotRepresentable> {
  let clips = children.iter().filter_map(|child| match child {
    Planned::Clip { source, .. } => Some(*source),
    _ => None,
  });
  for (clip, source) in clips.enumerate() {
    derive::range(
      Time::written(source.start, source.ruler),
      Time::written(source.length, source.ruler),
      Spot::Source(ClipAt::new(index, clip)),
    )?;
  }
  Ok(())
}

/// The clip [`settle`] moves to the next ruler of its list after a walk
/// refused a value at `at`, among the clips that can still move: of the
/// clips the value is formed from, the one whose ruler is the finest, the
/// later on a tie — for a place on a track or from the global start, the
/// clips up to the child it names; for a visible range, the item's own; for
/// a track's duration or its end, the track's; for the stack's duration,
/// every clip. Where none of those can move, the finest of the track's
/// clips (of every clip, for the stack's). For a source range's end, its
/// own clip alone, which no other clip's ruler changes; for the end of a
/// timeline with no track, none. `None` once none of those can move.
fn step(choices: &[Choice], at: Spot) -> Option<usize> {
  match at {
    Spot::TrackPosition(child) | Spot::Absolute(child) => finest(choices, |choice| {
      choice.track == child.track() && choice.child <= child.child()
    })
    .or_else(|| finest(choices, |choice| choice.track == child.track())),
    Spot::Visible(child) => finest(choices, |choice| {
      choice.track == child.track() && choice.child == child.child()
    })
    .or_else(|| finest(choices, |choice| choice.track == child.track())),
    Spot::Source(clip) => finest(choices, |choice| {
      choice.track == clip.track() && choice.clip == clip.clip()
    }),
    Spot::TrackDuration(track) | Spot::AbsoluteEnd(track) => {
      finest(choices, |choice| choice.track == track)
    }
    // A timeline with no track has no clip to move.
    Spot::TimelineEnd => None,
    // The stack's pick weighs every track; the other spots are the plan's,
    // which a walk never refuses.
    _ => finest(choices, |_| true),
  }
}

/// Of the `choices` that can still move and that `reaches` names, the one
/// whose ruler is the finest, the later on a tie.
fn finest(choices: &[Choice], reaches: impl Fn(&Choice) -> bool) -> Option<usize> {
  choices
    .iter()
    .enumerate()
    .filter(|(_, choice)| choice.at + 1 < choice.rulers.len() && reaches(choice))
    .max_by_key(|&(index, choice)| (choice.rulers[choice.at].ruler.exact(), index))
    .map(|(index, _)| index)
}

/// The clips the search may write in another ruler, each with its list
/// ([`rulers`]) over the timeline's [`operands`]: those with a ruler
/// besides their plan's.
fn choices(tracks: &[Vec<Planned<'_>>], operands: &BTreeSet<Rate>) -> Vec<Choice> {
  let mut choices = Vec::new();
  for (track, children) in tracks.iter().enumerate() {
    let clips = children
      .iter()
      .enumerate()
      .filter_map(|(child, planned)| match planned {
        Planned::Clip { clip, source, .. } => Some((child, *clip, *source)),
        _ => None,
      });
    for (index, (child, clip, source)) in clips.enumerate() {
      let rulers = rulers(clip.source_range(), source, operands);
      if rulers.len() > 1 {
        choices.push(Choice {
          track,
          child,
          clip: index,
          rulers,
          at: 0,
        });
      }
    }
  }
  choices
}

/// The rates OpenTimelineIO's operands are counted in, each once: the edit
/// rate — the global start's, every gap's, transition's and fade's — the
/// rate 1 its sum of a track's duration starts from, and every clip's
/// planned ruler, counted before any clip is planned.
///
/// A clip is planned in one of its own rulers where one writes its source
/// range ([`own_plan`]), and that ruler is counted here. A clip none of
/// whose own rulers writes it has no ruler to give, so the set never
/// depends on it: it is planned in one of these rates ([`source_plan`]),
/// and the set is every clip's planned ruler, its too — planning it adds
/// none.
///
/// A medium's available range gives no rate, wherever it is planned
/// ([`whole_plan`]): OpenTimelineIO derives nothing from it but its own
/// two ends, so no operand is ever counted in its ruler.
fn operands(laid: &Layout<'_>, edit: Ruler) -> BTreeSet<Rate> {
  let clips = laid
    .tracks()
    .iter()
    .flat_map(TrackLayout::items)
    .filter_map(|item| match *item {
      Item::Clip(clip) => Some(clip),
      Item::Gap(_) => None,
    });
  let mut rates = BTreeSet::from([edit.exact(), Rate::hz(1)]);
  rates.extend(
    clips
      .filter_map(|clip| own_plan(clip.source_range(), clip.media().rate()))
      .map(|plan| plan.ruler.exact()),
  );
  rates
}

/// The rulers the search may write a clip's source range in — `range`,
/// planned in `planned` — in the order it tries them:
///
/// 1. the plan's;
/// 2. **named**: every one of the timeline's `operands` but the plan's that
///    holds the range, counting its start and its length in whole ticks and
///    the three of start, length and end within ±2^53 — however many there
///    are — finest first;
/// 3. **free**: the whole rates below the plan's that hold it so — the
///    multiples of the coarsest, [`TimeRange::coarsest_whole_rate`] — that
///    no operand is counted in: the [`FREE`] finest, then the [`FREE`]
///    coarsest not among them, each band finest first ([`free_bands`]).
///    Each counts the range in smaller counts than the plan's, so each holds
///    it where the plan's ends it within ±2^53; a plan that cannot end it
///    has none that can, its coarsest whole ruler being among the rulers it
///    was planned from. A plan from the operands has none at all: it writes
///    a range the coarsest whole ruler does not, so it is coarser than that
///    ruler, and every whole rate that lands on the range is a multiple of
///    it;
/// 4. the plan's again, where every other is finer: a clip with finer
///    rulers only ends the search in its plan's.
///
/// The available range keeps the ruler its plan gave it: OpenTimelineIO
/// derives nothing from it but its own two ends.
fn rulers(range: TimeRange, planned: Counted, operands: &BTreeSet<Rate>) -> Vec<Counted> {
  let length = range.span();
  let own = planned.ruler.exact();
  let mut named: Vec<Counted> = operands
    .iter()
    .filter(|&&rate| rate != own)
    .filter_map(|&rate| recount(range, length, rate))
    .filter(Counted::held)
    .collect();
  named.sort_unstable_by_key(|counted| Reverse(counted.ruler.exact()));
  let free = range
    .coarsest_whole_rate()
    .map_or_else(Vec::new, |coarsest| {
      free_bands(range, length, own, coarsest.num(), operands)
    });
  let finer = free.is_empty() && named.last().is_some_and(|last| last.ruler.exact() > own);
  let mut rulers = vec![planned];
  rulers.extend(named);
  rulers.extend(free);
  if finer {
    rulers.push(planned);
  }
  rulers
}

/// The free rulers of `range`, `length` long, planned at the rate `own`:
/// the multiples of `coarsest`, the coarsest whole rate that lands on it,
/// below `own`, none of the timeline's `operands`, holding it whole — the
/// [`FREE`] finest, then the [`FREE`] coarsest not among them, each band
/// finest first, so the list runs finest first throughout.
fn free_bands(
  range: TimeRange,
  length: Duration,
  own: Rate,
  coarsest: i32,
  operands: &BTreeSet<Rate>,
) -> Vec<Counted> {
  // The multiples m · coarsest below the plan's rate, num / den: those with
  // m · coarsest · den < num, each recounted, a ruler an operand is counted
  // in left out.
  let below = (i128::from(own.num()) - 1) / (i128::from(coarsest) * i128::from(own.den().get()));
  let ruler = |multiple: i128| {
    let rate = Rate::hz(i32::try_from(multiple * i128::from(coarsest)).ok()?);
    if operands.contains(&rate) {
      return None;
    }
    recount(range, length, rate)
      .filter(Counted::held)
      .map(|counted| (multiple, counted))
  };
  let finest: Vec<(i128, Counted)> = (1..=below).rev().filter_map(ruler).take(FREE).collect();
  // The coarsest band stops short of the finest's coarsest multiple, so a
  // ruler in both bands is listed once, in the finest.
  let floor = finest.last().map_or(below + 1, |&(multiple, _)| multiple);
  let mut coarse: Vec<(i128, Counted)> = (1..floor).filter_map(ruler).take(FREE).collect();
  coarse.reverse();
  finest
    .into_iter()
    .chain(coarse)
    .map(|(_, counted)| counted)
    .collect()
}

/// The planned children as the walk reads them.
fn walked(planned: &[Planned<'_>], edit: Ruler) -> Vec<Child> {
  let ticks = |count: u64| Time::written(i128::from(count), edit);
  planned
    .iter()
    .map(|child| match *child {
      Planned::Gap(length) => Child::Item {
        start: ticks(0),
        duration: ticks(length),
      },
      Planned::Clip { source, .. } => Child::Item {
        start: Time::written(source.start, source.ruler),
        duration: Time::written(source.length, source.ruler),
      },
      Planned::Fade(fade, edge) => {
        let (in_offset, out_offset) = fade_offsets(fade, edge);
        Child::Transition {
          in_offset: ticks(in_offset),
          out_offset: ticks(out_offset),
        }
      }
      Planned::Dissolve(_, transition) => Child::Transition {
        in_offset: ticks(transition.in_offset().ticks()),
        out_offset: ticks(transition.out_offset().ticks()),
      },
    })
    .collect()
}

/// A fade's offsets in edit-rate ticks: a fade-in runs its length after the
/// cut from its gap, a fade-out its length before the cut to its gap —
/// inside the clip either way, so it plays no handle.
fn fade_offsets(fade: Fade, edge: Edge) -> (u64, u64) {
  let length = fade.duration().ticks();
  match edge {
    Edge::In => (0, length),
    Edge::Out => (length, 0),
  }
}

/// The edit rate's ruler, and the timebase its reciprocal spells — the one
/// a reader counts an edit-side value in where no word spells another.
#[derive(Clone, Copy)]
struct Edit {
  edit: Ruler,
  ticks: Timebase,
}

/// The word an edit-side value's `timebase` is written with: its own
/// spelling, `num/den`, where it spells the edit rate's reciprocal `ticks`
/// otherwise — `1/24` beside a rate of `48/2` — and none where it does not,
/// so a document stays as compact as the timeline's spellings allow.
/// Validation holds every edit-side value to the edit rate by value, so
/// the counts are the same in either spelling.
fn spelled(timebase: Timebase, ticks: Timebase) -> Option<Value> {
  (timebase.num() != ticks.num() || timebase.den() != ticks.den())
    .then(|| Value::String(format_utf8bytes!("{timebase}")))
}

fn track(
  track: &Track,
  children: &[Planned<'_>],
  marked: &Marked,
  Edit { edit, ticks }: Edit,
  target: OtioTarget,
) -> Value {
  // Dissolves are written in their cuts' order; where the track keeps its
  // transitions in another, each says its place among them.
  let reordered = reordered(track);
  let mut clips = marked.clips.iter();
  let children = children
    .iter()
    .map(|child| match *child {
      Planned::Gap(length) => gap(length, edit),
      Planned::Clip {
        clip,
        source,
        available,
      } => {
        let markers = clips.next().map_or(&[][..], Vec::as_slice);
        self::clip(clip, source, available, markers, ticks, target)
      }
      Planned::Fade(fade, edge) => fade_transition(fade, edge, Edit { edit, ticks }),
      Planned::Dissolve(index, transition) => {
        dissolve(transition, Edit { edit, ticks }, reordered.then_some(index))
      }
    })
    .collect();
  let markers = edit_markers(track.markers(), &marked.track, ticks);
  object(vec![
    ("OTIO_SCHEMA", text("Track.1")),
    (
      "metadata",
      metadata(Vec::new(), &Metadata::new(), track.readers()),
    ),
    ("name", text(track.name())),
    ("source_range", Value::Null),
    ("effects", Value::Array(Vec::new())),
    ("markers", Value::Array(markers)),
    ("enabled", Value::Bool(track.enabled())),
    ("children", Value::Array(children)),
    (
      "kind",
      text(match track.kind() {
        TrackKind::Video => "Video",
        TrackKind::Audio => "Audio",
      }),
    ),
  ])
}

fn clip(
  clip: &Clip,
  source: Counted,
  available: Option<Counted>,
  markers: &[Counted],
  ticks: Timebase,
  target: OtioTarget,
) -> Value {
  let mut words = vec![("id", text(clip.id().as_str()))];
  words.extend(spelled(clip.record().timebase(), ticks).map(|record| ("record", record)));
  if let Some(gain) = clip.gain() {
    words.push((
      "gain_db",
      Value::Number(format_utf8bytes!("{:?}", gain.db())),
    ));
  }
  words.push((
    "source",
    exact_ruler(source.ruler, clip.source_range().timebase()),
  ));
  let markers = clip
    .markers()
    .iter()
    .zip(markers)
    .map(|(marker, &range)| {
      let exact = exact_ruler(range.ruler, marker.range().timebase());
      self::marker(marker, range, Some(("range", exact)))
    })
    .collect();
  let mut members = vec![
    (
      "OTIO_SCHEMA",
      text(match target {
        OtioTarget::V0_15Plus => "Clip.2",
        OtioTarget::Legacy => "Clip.1",
      }),
    ),
    ("metadata", metadata(words, clip.metadata(), clip.readers())),
    ("name", text(clip.name())),
    ("source_range", source.time_range()),
    ("effects", Value::Array(Vec::new())),
    ("markers", Value::Array(markers)),
    ("enabled", Value::Bool(clip.enabled())),
  ];
  let reference = external_reference(clip.media(), available, target);
  match target {
    OtioTarget::V0_15Plus => {
      members.push(("media_references", object(vec![(DEFAULT_MEDIA, reference)])));
      members.push(("active_media_reference_key", text(DEFAULT_MEDIA)));
    }
    OtioTarget::Legacy => members.push(("media_reference", reference)),
  }
  object(members)
}

fn external_reference(media: &MediaRef, available: Option<Counted>, target: OtioTarget) -> Value {
  let mut words = Vec::new();
  if let Some(rate) = media.rate() {
    words.push(("rate", Value::String(format_utf8bytes!("{rate}"))));
  }
  if let Some(reel) = media.reel() {
    words.push(("reel", text(reel)));
  }
  if let (Some(range), Some(counted)) = (media.available_range(), available) {
    words.push(("available", exact_ruler(counted.ruler, range.timebase())));
  }
  let mut members = vec![
    ("OTIO_SCHEMA", text("ExternalReference.1")),
    (
      "metadata",
      metadata(words, &Metadata::new(), &ReaderMetadata::new()),
    ),
    ("name", text("")),
    (
      "available_range",
      available.map_or(Value::Null, Counted::time_range),
    ),
  ];
  if let OtioTarget::V0_15Plus = target {
    members.push(("available_image_bounds", Value::Null));
  }
  members.push(("target_url", text(media.locator())));
  object(members)
}

/// A gap of `length` edit-rate ticks.
fn gap(length: u64, edit: Ruler) -> Value {
  object(vec![
    ("OTIO_SCHEMA", text("Gap.1")),
    ("metadata", Value::Object(Vec::new())),
    ("name", text("")),
    (
      "source_range",
      time_range(
        rational_time(edit, i128::from(length)),
        rational_time(edit, 0),
      ),
    ),
    ("effects", Value::Array(Vec::new())),
    ("markers", Value::Array(Vec::new())),
    ("enabled", Value::Bool(true)),
  ])
}

/// A fade as a dissolve against its gap, its duration's timebase spelled
/// where it is spelled otherwise than the edit rate's reciprocal.
fn fade_transition(fade: Fade, edge: Edge, Edit { edit, ticks }: Edit) -> Value {
  let (in_offset, out_offset) = fade_offsets(fade, edge);
  let word = match edge {
    Edge::In => "in",
    Edge::Out => "out",
  };
  let shape = match fade.shape() {
    FadeShape::Linear => "linear",
    FadeShape::EqualPower => "equal_power",
  };
  let mut words = vec![("fade", text(word)), ("shape", text(shape))];
  words.extend(spelled(fade.duration().timebase(), ticks).map(|timebase| ("timebase", timebase)));
  transition(
    metadata(words, &Metadata::new(), &ReaderMetadata::new()),
    rational_time(edit, i128::from(in_offset)),
    rational_time(edit, i128::from(out_offset)),
  )
}

/// A transition as a dissolve, with its index among its track's
/// transitions where the track keeps them out of their cuts' order, and
/// the timebases of its cut and its offsets spelled where each is spelled
/// otherwise than the edit rate's reciprocal.
fn dissolve(transition: &Transition, Edit { edit, ticks }: Edit, order: Option<usize>) -> Value {
  let mut words = order.map_or_else(Vec::new, |index| {
    vec![("order", Value::Number(format_utf8bytes!("{index}")))]
  });
  for (word, timebase) in [
    ("at", transition.at().timebase()),
    ("in_offset", transition.in_offset().timebase()),
    ("out_offset", transition.out_offset().timebase()),
  ] {
    words.extend(spelled(timebase, ticks).map(|spelled| (word, spelled)));
  }
  self::transition(
    metadata(words, &Metadata::new(), &ReaderMetadata::new()),
    rational_time(edit, i128::from(transition.in_offset().ticks())),
    rational_time(edit, i128::from(transition.out_offset().ticks())),
  )
}

/// Whether `track` keeps its transitions in an order other than their
/// cuts'. A valid track holds one transition at a cut at most, so in their
/// cuts' order each cut is later than the one before.
fn reordered(track: &Track) -> bool {
  track
    .transitions()
    .windows(2)
    .any(|pair| pair[1].at() <= pair[0].at())
}

/// The timeline's markers or a track's, each as [`marker`] writes one at
/// the edit rate, as `counted`, its timebase spelled where it is spelled
/// otherwise than the edit rate's reciprocal, `ticks` (`timebase`).
fn edit_markers(markers: &[Marker], counted: &[Counted], ticks: Timebase) -> Vec<Value> {
  markers
    .iter()
    .zip(counted)
    .map(|(marker, &range)| {
      let word = spelled(marker.range().timebase(), ticks).map(|timebase| ("timebase", timebase));
      self::marker(marker, range, word)
    })
    .collect()
}

/// A marker as `Marker.2`: its `range` counted in one ruler — the edit
/// rate's, for the timeline's marker or a track's, with its timebase
/// spelled where it is spelled otherwise than the edit rate's reciprocal
/// (`timebase`); for a clip's, the ruler its plan chose, with its exact
/// words (`range`) — and its readers' namespaces beside those words.
fn marker(marker: &Marker, range: Counted, word: Option<(&'static str, Value)>) -> Value {
  let words = word.into_iter().collect();
  object(vec![
    ("OTIO_SCHEMA", text("Marker.2")),
    (
      "metadata",
      metadata(words, &Metadata::new(), marker.readers()),
    ),
    ("name", text(marker.name())),
    ("color", text(marker.color().otio_name())),
    ("marked_range", range.time_range()),
    ("comment", text(marker.comment())),
  ])
}

/// The exact words for a media-side range written in `ruler`: the rate its
/// counts are ticks of, and the `timebase` the model counts the range in,
/// each as `num/den`.
fn exact_ruler(ruler: Ruler, timebase: Timebase) -> Value {
  object(vec![
    (
      "ruler",
      Value::String(format_utf8bytes!("{}", ruler.exact())),
    ),
    ("timebase", Value::String(format_utf8bytes!("{timebase}"))),
  ])
}

/// A track's markers, and each of its clips', counted: a track's at the
/// edit rate, held whole there; a clip's held whole in the first of its own
/// rulers that does, else the first the timeline's operands are counted in
/// ([`whole_plan`]). OpenTimelineIO derives nothing from a marker but its
/// own two ends, so none is ever an operand, and no search moves one.
struct Marked {
  track: Vec<Counted>,
  /// Per clip, in the track's order.
  clips: Vec<Vec<Counted>>,
}

/// The markers of `track`, the `index`-th, planned ([`Marked`]); a marker
/// none of its rulers holds whole is refused before any walk — a track's
/// with no search, at the edit rate, as any count the export writes there.
fn markers_planned(
  index: usize,
  track: &Track,
  edit: Ruler,
  operands: &BTreeSet<Rate>,
) -> Result<Marked, NotRepresentable> {
  let on_track = track
    .markers()
    .iter()
    .enumerate()
    .map(|(marker, placed)| {
      let at = Spot::Marker(MarkerAt::Track {
        track: index,
        marker,
      });
      edit_marker(placed, edit, at)
    })
    .collect::<Result<Vec<_>, _>>()?;
  let on_clips = track
    .clips()
    .iter()
    .enumerate()
    .map(|(clip_index, clip)| {
      clip
        .markers()
        .iter()
        .enumerate()
        .map(|(marker, placed)| {
          let at = Spot::Marker(MarkerAt::Clip {
            clip: ClipAt::new(index, clip_index),
            marker,
          });
          whole_plan(placed.range(), clip.media().rate(), at, operands)
        })
        .collect::<Result<Vec<_>, _>>()
    })
    .collect::<Result<Vec<_>, _>>()?;
  Ok(Marked {
    track: on_track,
    clips: on_clips,
  })
}

/// The timeline's marker or a track's, counted at the edit rate and held
/// whole there — its start, its length and its end within ±2^53 — or
/// refused at `at`, with no search, as any count the export writes there.
fn edit_marker(marker: &Marker, edit: Ruler, at: Spot) -> Result<Counted, NotRepresentable> {
  let range = marker.range();
  Counted::new(edit, range.start_pts(), range.span().ticks()).within(true, at, None)
}

fn transition(metadata: Value, in_offset: Value, out_offset: Value) -> Value {
  object(vec![
    ("OTIO_SCHEMA", text("Transition.1")),
    ("metadata", metadata),
    ("name", text("")),
    ("in_offset", in_offset),
    ("out_offset", out_offset),
    ("transition_type", text("SMPTE_Dissolve")),
  ])
}

/// A media-side range's own rulers — a source range's or an available
/// range's — in the order they are tried, each counting its start and its
/// length in whole ticks where it does:
///
/// 1. frames of the medium's stated rate, when both land on whole frames;
/// 2. ticks of the range's own timebase ([`own_ticks`]), which always do;
/// 3. the coarsest ruler of a whole number of ticks a second in which both
///    land on a tick ([`TimeRange::coarsest_whole_rate`]).
fn own_rulers(range: TimeRange, rate: Option<Rate>) -> [Option<Counted>; 3] {
  let length = range.span();
  [
    rate.and_then(|rate| recount(range, length, rate)),
    Some(own_ticks(range)),
    range
      .coarsest_whole_rate()
      .and_then(|coarsest| recount(range, length, coarsest)),
  ]
}

/// `range` in ticks of its own timebase.
fn own_ticks(range: TimeRange) -> Counted {
  let own = Rate::checked_from_timebase(range.timebase()).unwrap_or(Rate::hz(0));
  Counted::new(Ruler::new(own), range.start_pts(), range.span().ticks())
}

/// A source range in the first of its own rulers ([`own_rulers`]) that holds
/// it whole — its start, its length and the end they make within ±2^53 —
/// else in the first that writes its start and its length, its end left to
/// the walk; `None` where none of them writes it.
fn own_plan(range: TimeRange, rate: Option<Rate>) -> Option<Counted> {
  let rulers = own_rulers(range, rate);
  let first = |fits: fn(&Counted) -> bool| rulers.iter().flatten().copied().find(fits);
  first(Counted::held).or_else(|| first(Counted::written))
}

/// A clip's source range, `range`, planned: start and length in one ruler,
/// never two, each a whole count within ±2^53 — the ruler the search starts
/// the clip in ([`settle`]):
///
/// 1. the first of its own rulers that holds it whole, else the first that
///    writes it ([`own_plan`]);
/// 2. where none of them writes it, the first of the timeline's `operands`
///    that writes it, finest first, as the search runs
///    [`RulerBand::Operands`](super::RulerBand::Operands)
///    ([`operand_plan`]).
///
/// Its end is the walk's to hold ([`sources`]), and the search's to settle
/// by writing the clip in a ruler of its list that holds it ([`rulers`]).
///
/// Only an operand's ruler can write a range none of its own rulers writes.
/// Every whole rate that lands on a range is a multiple of the coarsest
/// that does — one of its own rulers — and counts the range's start and its
/// length in that ruler's counts times the multiple, no smaller: so no whole
/// rate writes a range its coarsest whole ruler does not, neither a free
/// ruler of the search's bands, a whole rate all, nor a whole operand rate.
/// What can is a ruler at a fractional rate — the edit rate at 1/2 fps or
/// at 30000/1001, a neighbour's frames at a fractional rate — and of the
/// bands only the operands' rulers are at one. A range none of those writes
/// either is refused before any walk, with its own count past 2^53 and with
/// the search's bands, no ruler of which writes it: the search has no plan
/// to walk ([`RulerSearch::walks`] 0).
fn source_plan(
  range: TimeRange,
  rate: Option<Rate>,
  at: Spot,
  operands: &BTreeSet<Rate>,
) -> Result<Counted, NotRepresentable> {
  if let Some(own) = own_plan(range, rate) {
    return Ok(own);
  }
  if let Some(written) = operand_plan(range, operands, Counted::written) {
    return Ok(written);
  }
  own_ticks(range).within(false, at, Some(RulerSearch { walks: 0 }))
}

/// A media-side range OpenTimelineIO derives only its own ends from — the
/// available range of a clip's medium, or a clip's marker — `range`,
/// planned: start and length in one ruler, the first that holds it whole —
/// its start, its length and the end they make within ±2^53:
///
/// 1. of its own rulers ([`own_rulers`]), frames of the medium's stated
///    `rate` first;
/// 2. where none of them does, of the timeline's `operands`, finest first
///    ([`operand_plan`]).
///
/// Its end is held here with its start and its length, not left to the
/// walk: OpenTimelineIO derives nothing from such a range but its own two
/// ends, and the search never writes it in another ruler, so no walk could
/// change that end. Its ruler is no operand ([`operands`]).
///
/// As for a source range ([`source_plan`]), only a ruler at a fractional
/// rate can hold a range none of its own rulers holds whole: every whole
/// rate that lands on it is a multiple of its coarsest whole ruler, one of
/// its own, and counts its start, its length and its end in that ruler's
/// counts times the multiple, no smaller. A range none of the operands'
/// rulers holds whole either is refused before any walk, with its own count
/// past 2^53 — its start, its length or its end — and with the search's
/// bands, no ruler of which holds it ([`RulerSearch::walks`] 0).
fn whole_plan(
  range: TimeRange,
  rate: Option<Rate>,
  at: Spot,
  operands: &BTreeSet<Rate>,
) -> Result<Counted, NotRepresentable> {
  let own = own_rulers(range, rate)
    .into_iter()
    .flatten()
    .find(Counted::held);
  if let Some(whole) = own {
    return Ok(whole);
  }
  if let Some(whole) = operand_plan(range, operands, Counted::held) {
    return Ok(whole);
  }
  own_ticks(range).within(true, at, Some(RulerSearch { walks: 0 }))
}

/// `range` in the first ruler the timeline's `operands` are counted in,
/// finest first, that counts it so it `fits` — the plan's pass over
/// [`RulerBand::Operands`](super::RulerBand::Operands) for a media-side range
/// none of its own rulers fits: a source range none writes, its end left to
/// the walk ([`Counted::written`]); an available range none holds whole, its
/// end held with it ([`Counted::held`]).
fn operand_plan(
  range: TimeRange,
  operands: &BTreeSet<Rate>,
  fits: fn(&Counted) -> bool,
) -> Option<Counted> {
  let length = range.span();
  operands
    .iter()
    .rev()
    .filter_map(|&rate| recount(range, length, rate))
    .find(fits)
}

/// A range counted in one ruler: the ruler, and the range's start and
/// length in it.
#[derive(Clone, Copy)]
struct Counted {
  ruler: Ruler,
  start: i128,
  length: i128,
}

impl Counted {
  fn new(ruler: Ruler, start: i64, length: u64) -> Self {
    Self {
      ruler,
      start: i128::from(start),
      length: i128::from(length),
    }
  }

  /// The first of the start and the length — the counts the document
  /// writes — and, `with_end`, the end OpenTimelineIO derives from them
  /// (the two added, in this one ruler), that lies past ±2^53.
  fn past(self, with_end: bool) -> Option<i128> {
    let counts = [self.start, self.length, self.start + self.length];
    counts[..if with_end { 3 } else { 2 }]
      .iter()
      .copied()
      .find(|&count| !exact(count))
  }

  /// Whether the range is held whole: its start, its length and its end
  /// within ±2^53. Its last tick, `end_time_inclusive` — the end less one
  /// tick, or the start for a range of one tick or none — lies between
  /// them, and OpenTimelineIO computes it exactly in one ruler
  /// ([`mod@derive`]): held with them.
  fn held(&self) -> bool {
    self.past(true).is_none()
  }

  /// Whether the counts the document writes, the start and the length, lie
  /// within ±2^53.
  fn written(&self) -> bool {
    self.past(false).is_none()
  }

  /// The range so counted, where its start and its length — and, `with_end`,
  /// its end — lie within ±2^53; else refused at `at` with the first of them
  /// that does not, counted in this ruler, and `searched`.
  fn within(
    self,
    with_end: bool,
    at: Spot,
    searched: Option<RulerSearch>,
  ) -> Result<Self, NotRepresentable> {
    match self.past(with_end) {
      None => Ok(self),
      Some(value) => Err(NotRepresentable {
        at,
        value,
        rate: self.ruler.exact(),
        searched,
        misread: None,
      }),
    }
  }

  fn time_range(self) -> Value {
    time_range(
      rational_time(self.ruler, self.length),
      rational_time(self.ruler, self.start),
    )
  }

  /// Refuses the range at `at` where OpenTimelineIO 0.17 reads a number
  /// [`time_range`](Self::time_range) writes as another value, in its
  /// order: the duration's, then the start's ([`read_back`]).
  fn read_back(self, at: Spot) -> Result<(), NotRepresentable> {
    read_back(self.length, self.ruler, at)?;
    read_back(self.start, self.ruler, at)
  }
}

/// `range`, `length` long, recounted in ticks of `rate` by `mediatime`'s
/// exact rescale, which answers only where both land on a tick.
fn recount(range: TimeRange, length: Duration, rate: Rate) -> Option<Counted> {
  let ruler = rate.checked_to_timebase()?;
  let start = range.start().checked_rescale_with(ruler, Rounding::Exact)?;
  let ticks = length.checked_rescale_with(ruler, Rounding::Exact)?;
  Some(Counted::new(Ruler::new(rate), start.pts(), ticks.ticks()))
}

/// The metadata object: this crate's own `words` and the model's `notes`
/// under [`OURS`], where there are either, and beside them each other
/// reader's namespace in `readers`, whole — `{}` for one holding no entry,
/// as the model holds it — every key in byte order, as OpenTimelineIO's own
/// writer orders a dictionary's; an empty object where there are none of
/// them, so an object with no namespace writes what it wrote before
/// namespaces existed.
///
/// The notes are an array of `[key, value]` pairs, never an object: a
/// note's key is anything the model was given, and OpenTimelineIO reads a
/// dictionary carrying `OTIO_SCHEMA` as an object of that schema
/// (`deserialization.cpp` 561–573 in 0.17.0) — a note keyed so would make
/// the export a document OpenTimelineIO refuses. As strings in an array,
/// keys are values, which no reader takes for a schema. A namespace is
/// written as its reader reads it, an object of strings: validation holds
/// `OTIO_SCHEMA` out of its keys, and `mediaio` and `OTIO_SCHEMA` out of
/// the namespaces' names, so no namespace is read as an object of a schema
/// or takes the place of this crate's words.
fn metadata(
  mut words: Vec<(&'static str, Value)>,
  notes: &Metadata,
  readers: &ReaderMetadata,
) -> Value {
  if !notes.is_empty() {
    let notes = notes
      .iter()
      .map(|(key, value)| Value::Array(vec![text(key), text(value)]))
      .collect();
    words.push(("metadata", Value::Array(notes)));
  }
  let mut members: Vec<(Utf8Bytes, Value)> = readers
    .iter()
    .map(|(namespace, entries)| {
      let entries = entries
        .iter()
        .map(|(key, value)| (Utf8Bytes::from(key), text(value)))
        .collect();
      (Utf8Bytes::from(namespace), Value::Object(entries))
    })
    .collect();
  if !words.is_empty() {
    let place = members.partition_point(|(namespace, _)| namespace.as_str() < OURS);
    members.insert(place, (Utf8Bytes::from_static(OURS), object(words)));
  }
  Value::Object(members)
}

/// `count` ticks of `ruler`: its rate as [`rate`] writes one, and the count
/// as [`number`] does.
fn rational_time(ruler: Ruler, count: i128) -> Value {
  object(vec![
    ("OTIO_SCHEMA", text("RationalTime.1")),
    ("rate", rate(ruler)),
    ("value", number(count)),
  ])
}

/// A ruler's rate: the `f64` nearest it, in the spelling
/// [`rate_spelling`] finds — its shortest wherever OpenTimelineIO 0.17's
/// reader reads that back exactly, as it reads every common rate's — or,
/// where none it tries reads back exactly, the shortest, which
/// [`read_back`] refuses.
fn rate(ruler: Ruler) -> Value {
  let value = ruler.otio();
  Value::Number(rate_spelling(value).unwrap_or_else(|| format_utf8bytes!("{value:?}")))
}

/// The powers of ten an `f64` holds exactly, 10^0 to 10^22: RapidJSON's
/// table holds these exactly too (`pow10.h` 28–50 at `06d58b9e`), so a
/// division by one is rounded once.
const TEN: [f64; 23] = [
  1e0, 1e1, 1e2, 1e3, 1e4, 1e5, 1e6, 1e7, 1e8, 1e9, 1e10, 1e11, 1e12, 1e13, 1e14, 1e15, 1e16, 1e17,
  1e18, 1e19, 1e20, 1e21, 1e22,
];

/// 2^64: RapidJSON reads an integer below it whole, into a `uint64_t`.
const TWO_64: f64 = 18_446_744_073_709_551_616.0;

/// The spelling of `value`, a ruler's rate, positive and finite, that
/// OpenTimelineIO 0.17's reader reads back as `value`, bit for bit, in a
/// build that rounds RapidJSON's multiply-adds twice and in one that fuses
/// them ([`json::readings`]): the first of
///
/// 1. its shortest spelling, `{:?}`'s — every common rate's;
/// 2. `{D}e-{K}`, for `K` from 1 to 22 and `D` an integer below 2^64 that
///    an `f64` holds: RapidJSON reads `D` whole into its 64-bit integer and
///    converts it exactly, and its table holds `10^K` exactly, so it reads
///    `D / 10^K` by one division, rounded once, as both kinds of build do,
///    and as every reader that rounds to nearest does;
/// 3. `{D}`, `z` zeros, then `e-{K + z}`, for `z` from 1 to 24: past 2^64
///    RapidJSON carries the integer on in a `double`, each zero a
///    multiplication by ten rounded once with nothing added, which no
///    build fuses, then one division by its table's power of ten;
///
/// the smallest `K` first, then the smallest `z`, then the `D` nearest
/// `value × 10^K`, the lower of two as near. `None` where none of them
/// reads back as `value`.
///
/// Of the rates `num/1001` for `num` from 1 to 200 000, the shortest
/// spelling of 26 050 reads as another `f64`; the second form spells
/// 25 750 of them and the third 297 of the other 300, each read exactly by
/// RapidJSON at `06d58b9e` compiled either way. Three have none:
/// 29195/1001, 31264/1001 and 31799/1001. Nor does any of the 15 626
/// spellings tried around each — every integer within 300 of `value ×
/// 10^E` over `10^E`, for `E` from 15 to 35, and as many written with a
/// point — read exactly in both builds: the export refuses these three.
fn rate_spelling(value: f64) -> Option<Utf8Bytes> {
  let exact = |text: &str| {
    json::readings(text)
      .iter()
      .all(|reading| reading.is_ok_and(|number| number.double().to_bits() == value.to_bits()))
  };
  let shortest = format_utf8bytes!("{value:?}");
  if exact(&shortest) {
    return Some(shortest);
  }
  // Each `K`'s integers near `value × 10^K` that an `f64` holds, while the
  // product stays below 2^64.
  let near: Vec<(usize, Vec<u64>)> = (1..TEN.len())
    .map_while(|k| near_integers(value * TEN[k]).map(|integers| (k, integers)))
    .collect();
  for (k, integers) in &near {
    for d in integers {
      let text = format_utf8bytes!("{d}e-{k}");
      if exact(&text) {
        return Some(text);
      }
    }
  }
  for (k, integers) in &near {
    for z in 1..=24 {
      for d in integers {
        let text = format_utf8bytes!("{d}{:0>z$}e-{}", "", k + z);
        if exact(&text) {
          return Some(text);
        }
      }
    }
  }
  None
}

/// The integers near `scaled` that an `f64` holds, below 2^64 and above
/// zero, the nearest first and the lower of two as near: within three of it
/// below 2^52, where every integer is an `f64`, and past it the four
/// `f64`s either side of it, which are integers there. `None` for a
/// `scaled` past 2^64.
fn near_integers(scaled: f64) -> Option<Vec<u64>> {
  if scaled.is_nan() || scaled >= TWO_64 {
    return None;
  }
  let mut near: Vec<f64> = if scaled < 4_503_599_627_370_496.0 {
    // `core` has no `f64::floor`; the walk's own floor is `std`'s.
    let below = derive::floor(scaled);
    (-2..=3).map(|step| below + f64::from(step)).collect()
  } else {
    let mut around = vec![scaled];
    let (mut up, mut down) = (scaled, scaled);
    for _ in 0..4 {
      up = up.next_up();
      down = down.next_down();
      around.extend([up, down]);
    }
    around
  };
  // A double is whole where it is its own floor.
  near.retain(|&d| d > 0.0 && d < TWO_64 && derive::floor(d) == d);
  near.sort_by(|a, b| {
    (a - scaled)
      .abs()
      .total_cmp(&(b - scaled).abs())
      .then(a.total_cmp(b))
  });
  near.dedup();
  // An integer-valued `f64` below 2^64 converts to `u64` exactly.
  Some(near.into_iter().map(|d| d as u64).collect())
}

/// Refuses the first rate the document would carry, in the order it writes
/// them, that no spelling [`rate_spelling`] tries makes OpenTimelineIO
/// 0.17's reader read exactly ([`read_back`]): the global start's; then per
/// track its markers', then its children's — a gap's length, a clip's
/// source range, its markers and its medium's available range, a fade's
/// length, a dissolve's two offsets — each named by the count written
/// beside it. Every count among them the plan holds within ±2^53 already,
/// where every one is spelled exactly ([`number`]). A gap's start and a
/// fade's offset of zero are zeros, and every rate written beside them is
/// the edit rate's, read with the start's.
fn spellings(
  start: i128,
  edit: Ruler,
  stacked: &[Counted],
  tracks: &[Vec<Planned<'_>>],
  marked: &[Marked],
) -> Result<(), NotRepresentable> {
  read_back(start, edit, Spot::Start)?;
  for (marker, range) in stacked.iter().enumerate() {
    range.read_back(Spot::Marker(MarkerAt::Timeline { marker }))?;
  }
  for (track, (children, marked)) in tracks.iter().zip(marked).enumerate() {
    for (marker, range) in marked.track.iter().enumerate() {
      range.read_back(Spot::Marker(MarkerAt::Track { track, marker }))?;
    }
    // The clip the walk reaches next: a gap is named by the clip it leads
    // to, as the plan names it, and a fade by the clip it fades.
    let mut next = 0;
    for child in children {
      match *child {
        Planned::Gap(length) => read_back(
          i128::from(length),
          edit,
          Spot::Record(ClipAt::new(track, next)),
        )?,
        Planned::Clip {
          source, available, ..
        } => {
          let at = ClipAt::new(track, next);
          source.read_back(Spot::Source(at))?;
          let markers = marked.clips.get(next).map_or(&[][..], Vec::as_slice);
          for (marker, range) in markers.iter().enumerate() {
            range.read_back(Spot::Marker(MarkerAt::Clip { clip: at, marker }))?;
          }
          if let Some(available) = available {
            available.read_back(Spot::Available(at))?;
          }
          next += 1;
        }
        Planned::Fade(fade, edge) => {
          // A fade-in is written before its clip, a fade-out after it.
          let clip = match edge {
            Edge::In => next,
            Edge::Out => next.saturating_sub(1),
          };
          read_back(
            i128::from(fade.duration().ticks()),
            edit,
            Spot::Fade(EdgeAt::new(ClipAt::new(track, clip), edge)),
          )?;
        }
        Planned::Dissolve(which, transition) => {
          let at = Spot::Transition(TransitionAt::new(track, which));
          read_back(i128::from(transition.in_offset().ticks()), edit, at)?;
          read_back(i128::from(transition.out_offset().ticks()), edit, at)?;
        }
      }
    }
  }
  Ok(())
}

/// Refuses `count` ticks of `ruler`, at `at`, where no spelling of the
/// ruler's rate [`rate_spelling`] tries makes OpenTimelineIO 0.17's reader
/// read it back exactly — in a build that rounds RapidJSON's multiply-adds
/// twice, or in one that fuses them ([`json::readings`]) — so that the
/// export would write its shortest, which a build reads as another value.
/// Named as [`NotRepresentable::misread`] with the value meant and what
/// each build reads of the shortest. The count itself is spelled exactly
/// ([`number`]).
fn read_back(count: i128, ruler: Ruler, at: Spot) -> Result<(), NotRepresentable> {
  let meant = ruler.otio();
  if rate_spelling(meant).is_some() {
    return Ok(());
  }
  // A refused reading is no value at all: NaN, which is no number meant.
  let read = json::readings(format_utf8bytes!("{meant:?}").as_str())
    .map(|reading| reading.map_or(f64::NAN, Number::double));
  Err(NotRepresentable {
    at,
    value: count,
    rate: ruler.exact(),
    searched: None,
    misread: Some(Misread::new(MisreadNumber::Rate, meant, read)),
  })
}

fn time_range(duration: Value, start_time: Value) -> Value {
  object(vec![
    ("OTIO_SCHEMA", text("TimeRange.1")),
    ("duration", duration),
    ("start_time", start_time),
  ])
}

/// Whether an `f64` holds `count` exactly: whether it lies within ±2^53.
fn exact(count: i128) -> bool {
  count.unsigned_abs() <= EXACT
}

/// Refuses `value` ticks of `ruler`, as sitting at `at`, where an `f64`
/// would hold the count only rounded.
fn held(value: i128, ruler: Ruler, at: Spot) -> Result<(), NotRepresentable> {
  if exact(value) {
    Ok(())
  } else {
    Err(NotRepresentable {
      at,
      value,
      rate: ruler.exact(),
      searched: None,
      misread: None,
    })
  }
}

/// ⌊2^54 / 10⌋: OpenTimelineIO 0.17's reader reads every count written
/// `<digits>.0` exactly up to it, either way from zero.
const POINT_ZERO_EXACT: u128 = 1_801_439_850_948_198;

/// A whole count, within ±2^53, which the plan holds every written count
/// to: `86400.0`, its digits and `.0`, as OpenTimelineIO's own writer spells
/// a `double`, up to ⌊2^54 / 10⌋ either way, where OpenTimelineIO 0.17's
/// reader reads that back exactly in every build; past it its digits alone,
/// `1801439850948199`, which RapidJSON hands over as an integer and a
/// `double` member takes exactly within ±2^53 — as every reader from 0.12
/// takes one (`deserialization.cpp` 418–454 in 0.17.0).
fn number(value: i128) -> Value {
  if value.unsigned_abs() <= POINT_ZERO_EXACT {
    Value::Number(format_utf8bytes!("{value}.0"))
  } else {
    Value::Number(format_utf8bytes!("{value}"))
  }
}

fn text(value: &str) -> Value {
  Value::String(Utf8Bytes::from(value))
}

fn object(members: Vec<(&'static str, Value)>) -> Value {
  Value::Object(
    members
      .into_iter()
      .map(|(key, value)| (Utf8Bytes::from(key), value))
      .collect(),
  )
}
