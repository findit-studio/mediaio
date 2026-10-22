//! OpenTimelineIO: export, the structural self-check, and reading back.
//!
//! [`to_otio`] writes a valid timeline as OpenTimelineIO JSON for the
//! readers an [`OtioTarget`] names:
//!
//! | model | OpenTimelineIO |
//! |---|---|
//! | [`Timeline`] | `Timeline.1`; `global_start_time` is always written, from [`Timeline::start`], at the edit rate |
//! | its tracks | one `Stack.1` named `tracks`, bottom track first |
//! | [`Track`](crate::Track) | `Track.1`, `kind` `Video` or `Audio`, `enabled` |
//! | [`Clip`](crate::Clip) | `Clip.2` under [`OtioTarget::V0_15Plus`], `Clip.1` under [`OtioTarget::Legacy`]; `enabled` |
//! | its trim | `source_range`: the source range whole, `start_time` and `duration` in the medium's one ruler — by validation, a duration as long as the record |
//! | [`MediaRef`](crate::MediaRef) | `ExternalReference.1` (under `DEFAULT_MEDIA` in `Clip.2`): `target_url` is the locator, `available_range` the available range |
//! | a gap between records | `Gap.1` — derived by [`layout`](fn@crate::layout), never stored |
//! | [`Transition`](crate::Transition) | `Transition.1`, `SMPTE_Dissolve`, its offsets at the edit rate |
//! | a [`Fade`](crate::Fade) | `Transition.1`, `SMPTE_Dissolve` against a gap — a gap of no length where the clip abuts a clip or a track's end |
//! | a clip's [`id`](crate::Clip::id), gain, reel, the medium's rate, a fade's curve, [`Metadata`](crate::Metadata) | `metadata.mediaio`: `id`, `gain_db`, `reel`, `rate`, `fade` and `shape`, and `metadata` — the notes as an array of `[key, value]` pairs, never an object, so no key of the model's is a key a reader takes for a schema word (`OTIO_SCHEMA`) |
//! | a [`Marker`](crate::Marker), on a track or a clip | `Marker.2` among the item's `markers`: `name`, `color` (OpenTimelineIO's capitals), `marked_range`, `comment` — a track's at the edit rate, a clip's in its medium's time |
//! | other readers' [`ReaderMetadata`](crate::ReaderMetadata), on the timeline, a track, a clip or a marker | the object's `metadata`, beside `mediaio`: each namespace under its own key, an object of strings, written whole — `{}` for one holding no entry — every key in byte order, as OpenTimelineIO's own writer orders a dictionary's; an object with no namespace writes none |
//! | the exact words a reader needs | `metadata.mediaio`: the edit rate (`rate`, `num/den`) on the timeline; on a clip, its reference and a clip's marker, the exact ruler a media-side range's counts are written in and the timebase the model counts it in (`source`, `available`, `range`); on a dissolve, its place among its track's transitions where the track keeps them out of their cuts' order (`order`); and the timebase of an edit-side value spelled otherwise than the edit rate's reciprocal — the timeline's `start`, a clip's `record`, a fade's or a track marker's `timebase`, a dissolve's `at`, `in_offset`, `out_offset` — so `1/24` beside a rate of `48/2` reads back as `1/24` |
//!
//! Every position on the record side is a whole count at the edit rate. A
//! media-side range (a source range, an available range) is written whole in
//! one ruler: frames of the medium's stated rate when its start and length
//! both land on one, and otherwise ticks of its own timebase.
//!
//! **Every count OpenTimelineIO reads or derives is one it holds exactly.**
//! A `RationalTime` keeps its value in an `f64`, which holds every whole
//! number up to 2^53 and only some past it, so a larger count may be read
//! rounded — a clip placed early, a different stretch of media played. And
//! OpenTimelineIO derives a document's positions in that arithmetic: a sum
//! of two times is carried in the higher of their rates, the other rescaled
//! into it, two rounded operations. So the export holds three things:
//!
//! - every count it writes lies within ±2^53, and a rate is written as the
//!   `f64` nearest it, in its shortest spelling; a
//!   media-side range whose counts are past 2^53 in both rulers above is
//!   written in the coarsest ruler of a whole number of ticks a second that
//!   holds its start and its length exactly, when its counts there are
//!   within 2^53. Of those three rulers, a range is written in the first
//!   whose counts end it within ±2^53 too, where one does; where none does,
//!   a source range is written in the first that writes its start and its
//!   length, its end left to the walk below and to the search. A source
//!   range none of the three writes is written in the first ruler the
//!   timeline's operands are counted in that does, finest first, its end
//!   left to the walk likewise; an available range none of the three ends
//!   within ±2^53, in the first ruler the timeline's operands are counted in
//!   whose counts do, finest first — the walk never reads an available
//!   range, so its end is held here. Each such ruler is at a fractional
//!   rate, as every whole rate that lands on a range is a multiple of the
//!   coarsest, which counts it in the smallest counts; and a range none of
//!   them writes — or, an available range, ends within ±2^53 — is refused;
//! - every number it writes reads back, in OpenTimelineIO 0.17.0's reader,
//!   as the value meant, in a build that rounds its multiply-adds twice and
//!   in one that fuses them. That reader is RapidJSON in its normal
//!   precision, which does not always round to the nearest double. It reads
//!   a count written `<digits>.0` exactly up to 1 801 439 850 948 198,
//!   ⌊2^54 / 10⌋, but past it may read another — `1801439850948199.0` as
//!   1801439850948199.25, `9007199254740991.0` as 9007199254740990 — so a
//!   count past it is written as its integer, `1801439850948199`, which it
//!   hands over whole. It reads the shortest spelling of the `f64` of every
//!   common rate exactly — 24, 25, 30, 48, 50 and 60, the NTSC rates over
//!   1001, the audio rates, 90 kHz — but of some others a neighbouring
//!   `f64`, 96000/1001's among them, so such a rate is written in another
//!   spelling of the same `f64` that both builds read exactly:
//!   `95904095904095904e-15`, an integer an `f64` holds over a power of ten
//!   its table holds, which the reader divides once. A rate no such
//!   spelling writes is refused ([`NotRepresentable::misread`]): of the
//!   rates `num/1001` up to 200 000/1001, three — 29195/1001, 31264/1001
//!   and 31799/1001;
//! - every count OpenTimelineIO derives from the document — the end of each
//!   clip's source range, its start plus its length; each child's place on
//!   its track, from zero in the child's own rate with every item before it
//!   added, and in the timeline; the running end of one walk over a whole
//!   track; each item's visible range, widened by the handles of the
//!   transitions beside it; each track's duration and the stack's, the
//!   longest of them as OpenTimelineIO picks it; the global start added to
//!   each place and each track's end; each child's range from the global
//!   start, the moved start and the child's own duration, and its end; for
//!   a timeline with no track, the global start counted at rate 1, where
//!   the empty stack's duration of zero carries it; and the last tick of
//!   every range among them, `end_time_inclusive`, which floors the range's
//!   end or takes a tick off it by a branch OpenTimelineIO takes on its own
//!   doubles — is computed as OpenTimelineIO computes it, operation for
//!   operation and branch for branch, beside its exact value, and must lie
//!   within ±2^53 in the ruler OpenTimelineIO carries it in, with
//!   OpenTimelineIO's double less than half a tick from the exact count. A
//!   range written in one ruler ends inclusively where it exactly does: its
//!   counts whole, OpenTimelineIO's arithmetic on them is exact.
//!
//! **Exact, or refused.** Where OpenTimelineIO's arithmetic would round a
//! value, the export writes the clips the value is formed from in other
//! rulers that hold their source ranges exactly and walks the timeline
//! again: a search, bounded by contract ([`RulerSearch`]). Besides its
//! plan's ruler, a clip may be written in a ruler of three bands
//! ([`RulerBand`]): every ruler the timeline's operands are counted in — the
//! edit rate, at which the global start, the gaps and the transitions are
//! written, the rate 1 a track's duration is summed from, and every clip's
//! planned ruler — all of them; the 64 finest whole numbers of ticks a
//! second below its plan's ruler that no operand is counted in; and the 64
//! coarsest of those. One clip moves one ruler along its list at a time,
//! the one with the finest ruler first. The guarantee is soundness: what the
//! export writes, OpenTimelineIO reads back exactly. A timeline no plan the
//! search walks holds, [`to_otio`] refuses ([`Refused::NotRepresentable`]),
//! naming the value the last plan could not hold and what the search tried —
//! its bands and its walks, none where no ruler of the bands writes a clip's
//! source range or holds its medium's available range whole — so a refusal
//! says the search was bounded: a timeline only a ruler outside the bands
//! would hold is refused by this contract, and never written to be read
//! rounded. Where the bound has been met, counts lie near 2^53 ticks of a
//! clip's ruler — at the rates media run at, positions thousands of years in:
//! 2^53 ticks are some 1 500 years even at 192 kHz. The rounding a real
//! timeline meets is a rate or a rescale no `f64` holds exactly — NTSC's
//! 30000/1001, or frames of one rate counted in another — and the half-tick
//! hold settles it: written where OpenTimelineIO's double lands within half a
//! tick of the exact count, refused by name where it does not.
//!
//! A source range's length is a whole number of edit-rate ticks (validation
//! refuses one that is not), so laying a track's items end to end puts every
//! clip on its record. Where a medium's ruler is not the edit rate, one
//! track's items are counted in more than one rate, and OpenTimelineIO sums
//! those in floating point: a position derived that way is read to the
//! nearest frame, not truncated — held, as above, under half a tick of the
//! exact count.
//!
//! OpenTimelineIO has no word for a clip's id, its gain or a reel, so all
//! three ride in `metadata` — the id so that a reader can tell each clip
//! again, its name and medium being free to repeat. An application that does
//! not read it — DaVinci Resolve among them — applies neither gain nor reel.
//! Available ranges and the start are written as the
//! timeline holds them: a medium read through `mediadecode` starts at zero
//! until the read side exposes the container's timecode, so its available
//! range does not yet carry the camera's timecode.
//!
//! [`validate_json`] reads a document back with the crate's own JSON reader
//! and checks OpenTimelineIO's schema shape — no Python, no OpenTimelineIO
//! install.
//!
//! [`from_otio`] reads a document back into a [`Timeline`]: the exact inverse
//! of [`to_otio`]. For every timeline `to_otio` writes, for either target,
//! `from_otio` answers that very timeline — every count, timebase and rate
//! as the model held it — because the export writes, beside what
//! OpenTimelineIO reads, the exact words a reader needs (the table above).
//! A document from another writer is read where it uses only what the
//! timeline models and carries those words; anything else is refused by
//! name, never dropped ([`Unreadable`]).

use core::fmt;
use std::{boxed::Box, vec::Vec};

use mediatime::Rate;
use smol_bytes::{Utf8Bytes, Utf8BytesMut};

use crate::{
  ClipAt, EdgeAt, MarkerAt, Refusal, Timeline, TransitionAt, layout::layout_valid, validate,
};

mod check;
mod derive;
mod export;
mod import;
mod json;

/// Which OpenTimelineIO readers a document is written for.
///
/// Marked `#[non_exhaustive]`: a later target joins as a variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum OtioTarget {
  /// OpenTimelineIO 0.15 and later: a clip is `Clip.2`, its medium under
  /// `media_references`.
  V0_15Plus,
  /// Readers from before OpenTimelineIO 0.15: a clip is `Clip.1`, with one
  /// `media_reference`.
  Legacy,
}

/// Writes `timeline` as OpenTimelineIO JSON for `target`, indented four
/// spaces a level as OpenTimelineIO's own writer indents, ending in a
/// newline.
///
/// # Errors
///
/// - [`Refused::Validation`], with its refusals, for a timeline that does
///   not [`validate`](fn@validate): its records could not be laid end to
///   end, which is how OpenTimelineIO places items.
/// - [`Refused::NotRepresentable`] for a count OpenTimelineIO would read
///   or derive rounded in every plan of rulers the export's bounded search
///   tries ([`RulerSearch`]): one past 2^53, where its `f64` no longer holds
///   every whole number, or one its arithmetic would land half a tick off
///   or more (see [the module's docs](self)); and for a rate no spelling of
///   which the export tries OpenTimelineIO 0.17's reader reads back exactly
///   ([`NotRepresentable::misread`]).
pub fn to_otio(timeline: &Timeline, target: OtioTarget) -> Result<Utf8Bytes, Refused> {
  validate(timeline).map_err(Refused::Validation)?;
  let tree = export::timeline(timeline, &layout_valid(timeline), target)
    .map_err(Refused::NotRepresentable)?;
  let mut out = Utf8BytesMut::new();
  json::write_pretty(&tree, &mut out);
  out.push('\n');
  Ok(out.freeze_shared())
}

/// Why [`to_otio`] wrote nothing.
///
/// Marked `#[non_exhaustive]`: a later reason joins as a variant.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Refused {
  /// The timeline does not [`validate`](fn@validate): its refusals, as
  /// `validate` answers them.
  Validation(Vec<Refusal>),
  /// A count OpenTimelineIO would read or derive rounded in every plan of
  /// rulers the export's bounded search tries — past 2^53, where its `f64`
  /// no longer holds every whole number, or landed half a tick off or more
  /// by its arithmetic: the count the last plan tried could not hold, and,
  /// for a count OpenTimelineIO derives, a source range no ruler of the
  /// search's bands writes or an available range none holds whole, what the
  /// search tried ([`NotRepresentable::searched`]); or a rate no spelling
  /// of which the export tries OpenTimelineIO 0.17's reader reads back
  /// exactly ([`NotRepresentable::misread`]).
  NotRepresentable(NotRepresentable),
}

/// Writes `the timeline does not validate: …` with each refusal, or the
/// count that is not representable.
impl fmt::Display for Refused {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Validation(refusals) => {
        f.write_str("the timeline does not validate")?;
        for (index, refusal) in refusals.iter().enumerate() {
          f.write_str(if index == 0 { ": " } else { "; " })?;
          fmt::Display::fmt(refusal, f)?;
        }
        Ok(())
      }
      Self::NotRepresentable(count) => fmt::Display::fmt(count, f),
    }
  }
}

impl core::error::Error for Refused {}

/// A count OpenTimelineIO would not hold exactly: what
/// [`Refused::NotRepresentable`] carries.
///
/// The count is one the export would write, or one OpenTimelineIO derives
/// from them in its own `f64` arithmetic — a range's end or its last tick,
/// a child's place on its track or in the timeline, a visible range, a
/// track's or the stack's duration, a place counted from the global start.
/// Past 2^53 an `f64` no longer holds every whole number; a derived count
/// can also come out of OpenTimelineIO's rescaling, or its branch on a
/// rescaled double, half a tick off or more. Either way a reader would get,
/// or add up from it, a rounded time.
///
/// A count OpenTimelineIO derives is refused after the export's search, in
/// the last plan of rulers it walked, and the refusal says what the search
/// tried ([`searched`](Self::searched)) — a clip's source range's end
/// among them. Refused as the timeline holds it, before any walk: a count
/// the export would write at the edit rate, which no ruler the search tries
/// changes; a source range no ruler of the search's bands writes — its
/// start or its length past 2^53 in each of its own rulers and in each
/// ruler the timeline's operands are counted in; and an available range no
/// ruler of the bands holds whole — its start, its length or its end past
/// 2^53 in each of those, its end held with its start and its length, as
/// the search never moves it — and a clip's marker likewise. The operands'
/// rulers are the only rulers of the bands that can write or hold a range
/// its own cannot: every whole rate that lands on a range is a multiple of
/// its coarsest whole ruler, one of its own, which counts it in the smallest
/// counts. Those refusals say what the search tried too: its bands, and no
/// walk. A track's marker is counted at the edit rate, as a count the
/// export writes there, and refused with no search.
///
/// Last, a ruler's rate none of whose spellings the export tries
/// OpenTimelineIO 0.17 reads back exactly is refused, with no search
/// ([`misread`](Self::misread)). OpenTimelineIO 0.17.0 reads numbers with
/// RapidJSON in its normal precision, which does not always round to the
/// nearest double: it reads the shortest spelling of the `f64` of some
/// rates as a neighbouring `f64`, 96000/1001's among them, while every
/// common rate reads exactly; and a build that fuses one of RapidJSON's
/// multiply-adds may read a long number otherwise than one that does not.
/// The export writes such a rate in another spelling of the same `f64` that
/// both kinds of build read exactly, where one of those it tries does — an
/// integer an `f64` holds over a power of ten, which the reader divides
/// once — and refuses the first rate, in the order the document writes
/// them, that none does: of the rates `num/1001` up to 200 000/1001,
/// 29195/1001, 31264/1001 and 31799/1001. Every count it writes reads back
/// exactly: `<digits>.0` up to ⌊2^54 / 10⌋, which the reader reads exactly,
/// and its integer past that.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NotRepresentable {
  at: Spot,
  value: i128,
  rate: Rate,
  searched: Option<RulerSearch>,
  misread: Option<Misread>,
}

impl NotRepresentable {
  /// Where the count sits.
  pub const fn at(&self) -> Spot {
    self.at
  }

  /// The count, in ticks of [`rate`](Self::rate).
  ///
  /// A count refused before any walk is the count as the timeline holds it:
  /// on the record side ticks of the edit rate; on the media side ticks of
  /// the range's own timebase, the first past 2^53 — a source range's start
  /// or its length, an available range's start, its length or its end.
  /// A count OpenTimelineIO derives is counted, exactly and to the nearest
  /// tick, in the ruler OpenTimelineIO derives it in: the highest rate among
  /// the times it adds up. A last tick is the exact one — the range's exact
  /// end floored, or less one tick, as the exact duration is fractional or
  /// whole.
  pub const fn value(&self) -> i128 {
    self.value
  }

  /// The rate [`value`](Self::value) counts ticks of.
  pub const fn rate(&self) -> Rate {
    self.rate
  }

  /// What the export's search tried before it refused the count — its
  /// bands and its walks: where OpenTimelineIO derives the count, a source
  /// range's end among them, the plans it walked; for a source range no
  /// ruler of the bands writes, or an available range none of them holds
  /// whole, none ([`RulerSearch::walks`] 0), the range refused as the
  /// timeline holds it. `None` for a count the export writes at the edit
  /// rate, refused before any walk — the search tries no ruler for it — and
  /// for a number OpenTimelineIO 0.17 reads as another value
  /// ([`misread`](Self::misread)).
  pub const fn searched(&self) -> Option<RulerSearch> {
    self.searched
  }

  /// The rate of [`rate`](Self::rate)'s ruler, where none of its spellings
  /// the export tries reads back exactly in OpenTimelineIO 0.17, and what
  /// that reader reads of its shortest. `None` for a count past 2^53, or
  /// one OpenTimelineIO derives rounded.
  pub const fn misread(&self) -> Option<Misread> {
    self.misread
  }
}

/// A number the export would write that OpenTimelineIO 0.17 reads as
/// another value, whatever spelling of it the export tries: what
/// [`NotRepresentable::misread`] carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Misread {
  number: MisreadNumber,
  meant: u64,
  read: [u64; 2],
}

impl Misread {
  pub(crate) const fn new(number: MisreadNumber, meant: f64, read: [f64; 2]) -> Self {
    Self {
      number,
      meant: meant.to_bits(),
      read: [read[0].to_bits(), read[1].to_bits()],
    }
  }

  /// Which of the `RationalTime`'s numbers it is.
  pub const fn number(&self) -> MisreadNumber {
    self.number
  }

  /// The value meant: the `f64` nearest the exact rate.
  pub const fn meant(&self) -> f64 {
    f64::from_bits(self.meant)
  }

  /// What OpenTimelineIO 0.17 reads the shortest spelling as: in a build
  /// that rounds RapidJSON's `d * 10 + digit` twice, then in one that fuses
  /// it — one value twice where the two agree.
  pub const fn read(&self) -> [f64; 2] {
    [f64::from_bits(self.read[0]), f64::from_bits(self.read[1])]
  }
}

/// Writes `its rate written 29.165834165834166: OpenTimelineIO 0.17 reads
/// this spelling as another value, and every other the export tries,
/// 29.16583416583417` — the read value in its shortest spelling — with both
/// readings where two builds differ.
impl fmt::Display for Misread {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let meant = self.meant();
    match self.number {
      MisreadNumber::Rate => write!(f, "its rate written {meant:?}")?,
    }
    f.write_str(
      ": OpenTimelineIO 0.17 reads this spelling as another value, and every other the export \
       tries, ",
    )?;
    let [twice, fused] = self.read();
    if twice.to_bits() == fused.to_bits() {
      write!(f, "{twice:?}")
    } else {
      write!(
        f,
        "{twice:?} in a build that rounds its multiply-add twice and {fused:?} in one that \
         fuses it"
      )
    }
  }
}

/// Which number of a `RationalTime` OpenTimelineIO 0.17 would read as
/// another value ([`Misread::number`]). Its count it never does: the export
/// writes every count in a spelling that reader reads exactly.
///
/// Marked `#[non_exhaustive]`: a later number joins as a variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum MisreadNumber {
  /// Its rate: the `f64` nearest the exact rate, which OpenTimelineIO 0.17
  /// reads as another value in its shortest spelling and in every other
  /// the export tries.
  Rate,
}

/// Writes `the gap before track 0, clip 0 counts 9007199254740993 ticks of
/// 1/1 s, which OpenTimelineIO's f64 does not hold exactly`, and for a count
/// OpenTimelineIO derives, what the search tried after it:
/// `…, in the last of 3 plans the bounded search walked, …` — or, for a
/// source range no ruler of the bands writes or an available range none
/// holds whole, `…, in any of the range's own rulers or of the bounded
/// search's bands, so the search walked no plan`. For a rate
/// OpenTimelineIO 0.17 reads as another value whatever spelling the export
/// tries, the shortest and what it reads instead ([`Misread`]): `the start
/// counts 0 ticks of 1001/29195 s, its rate written 29.165834165834166:
/// OpenTimelineIO 0.17 reads this spelling as another value, and every
/// other the export tries, 29.16583416583417`.
impl fmt::Display for NotRepresentable {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "{} counts {} ticks", self.at, self.value)?;
    if let Some(tick) = self.rate.checked_to_timebase() {
      write!(f, " of {tick} s")?;
    }
    if let Some(misread) = self.misread {
      return write!(f, ", {misread}");
    }
    f.write_str(", which OpenTimelineIO's f64 does not hold exactly")?;
    if let Some(searched) = self.searched {
      write!(f, ", in {searched}")?;
    }
    Ok(())
  }
}

/// What the export's search tried before [`to_otio`] refused a count
/// OpenTimelineIO derives, or a clip's media-side range before any walk:
/// the bands of rulers it may write a clip in, and how many plans of rulers
/// it walked.
///
/// The search is bounded, by contract. Where OpenTimelineIO's arithmetic
/// would round a value, the export writes one of the clips the value is
/// formed from in the next ruler of its list — the one whose ruler is the
/// finest — and walks the timeline again. A clip's list runs:
///
/// 1. its plan's ruler: one of its own — the first that holds its source
///    range whole, else the first that writes its start and its length —
///    or, where none of its own writes it, the first ruler the timeline's
///    operands are counted in that does, finest first;
/// 2. [`RulerBand::Operands`], finest first;
/// 3. [`RulerBand::Finest`], finest first;
/// 4. [`RulerBand::Coarsest`], finest first, less any ruler already in the
///    finest band;
/// 5. its plan's ruler again, where every other ruler of the list is finer.
///
/// A clip never moves back along its list, so the walks are bounded: at
/// most `1 + n · (d + 127)` for `n` clips and `d` distinct rates the
/// timeline's operands are counted in. What the export writes,
/// OpenTimelineIO reads back exactly; a timeline no plan the search walks
/// holds is refused with the last plan's value, and with this record of
/// what the search tried. A source range no ruler of the bands writes has
/// no plan to walk; nor has an available range none of them holds whole —
/// planned as a source range is, in its own rulers and then the operands',
/// but held whole there, its end with its start and its length, as the
/// search never moves it. Either is refused as the timeline holds it, with
/// this record and no walk. A ruler outside the bands, or a plan of rulers
/// the moves do not reach, is never tried: a timeline only such a plan
/// would hold is refused by this contract, not misread.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RulerSearch {
  walks: usize,
}

/// The bands every search tries a clip in, in its list's order.
const BANDS: [RulerBand; 3] = [
  RulerBand::Operands,
  RulerBand::Finest(export::FREE),
  RulerBand::Coarsest(export::FREE),
];

impl RulerSearch {
  /// The bands of rulers, besides its plan's, the search may write a clip
  /// in, in the order a clip's list runs them.
  pub const fn bands(&self) -> &'static [RulerBand] {
    &BANDS
  }

  /// How many plans of rulers the search walked, the plan the export first
  /// counted among them and the refused one last; 0 where no ruler of the
  /// bands writes a clip's source range, or holds its medium's available
  /// range whole, which leaves no plan to walk.
  pub const fn walks(&self) -> usize {
    self.walks
  }
}

/// Writes `the last of 129 plans the bounded search walked, trying each clip
/// in its plan's ruler, every ruler …, the 64 finest …, and the 64 coarsest
/// …`; with no walk, `any of the range's own rulers or of the bounded
/// search's bands, so the search walked no plan`.
impl fmt::Display for RulerSearch {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self.walks {
      0 => {
        return f.write_str(
          "any of the range's own rulers or of the bounded search's bands, so the search walked \
           no plan",
        );
      }
      1 => f.write_str("the one plan the bounded search walked")?,
      walks => write!(f, "the last of {walks} plans the bounded search walked")?,
    }
    f.write_str(", trying each clip in its plan's ruler")?;
    let bands = self.bands();
    for (index, band) in bands.iter().enumerate() {
      f.write_str(if index + 1 == bands.len() {
        ", and "
      } else {
        ", "
      })?;
      fmt::Display::fmt(band, f)?;
    }
    Ok(())
  }
}

/// A band of the rulers the export's search may write a clip's source range
/// in, besides its plan's ([`RulerSearch`]). Every ruler of a band holds the
/// range exactly: its start and its length whole ticks of it, and those and
/// the end they make each within ±2^53.
///
/// Marked `#[non_exhaustive]`: a later band joins as a variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum RulerBand {
  /// Every ruler the timeline's operands are counted in — the edit rate, at
  /// which the global start, the gaps and the transitions are written, the
  /// rate 1 OpenTimelineIO sums a track's duration from, and every clip's
  /// planned ruler — that holds the range, finer or coarser than the clip's
  /// own: all of them.
  Operands,
  /// The finest so many whole numbers of ticks a second strictly below the
  /// clip's plan's rate that hold the range — multiples of the coarsest
  /// whole ruler that does — and that no operand is counted in.
  Finest(usize),
  /// The coarsest so many whole numbers of ticks a second below the clip's
  /// plan's rate that hold the range and that no operand is counted in.
  Coarsest(usize),
}

/// Writes `every ruler the timeline's operands are counted in that holds
/// it`, `the 64 finest whole rates below its plan's that hold it`, `the 64
/// coarsest whole rates below its plan's that hold it`.
impl fmt::Display for RulerBand {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Operands => {
        f.write_str("every ruler the timeline's operands are counted in that holds it")
      }
      Self::Finest(count) => write!(
        f,
        "the {count} finest whole rates below its plan's that hold it"
      ),
      Self::Coarsest(count) => write!(
        f,
        "the {count} coarsest whole rates below its plan's that hold it"
      ),
    }
  }
}

/// Where in a timeline a count [`to_otio`] would write, or OpenTimelineIO
/// would derive, sits: what [`NotRepresentable`] names.
///
/// Marked `#[non_exhaustive]`: a later counted word joins as a variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Spot {
  /// The timeline's start: `global_start_time`.
  Start,
  /// The gap before a clip: its length, written at the edit rate.
  Record(ClipAt),
  /// A clip's source range: its start or its length, which the export
  /// writes, or its end or its last tick, which OpenTimelineIO derives from
  /// them.
  Source(ClipAt),
  /// The available range of a clip's medium: its start, its length or its
  /// end.
  Available(ClipAt),
  /// A transition's offsets.
  Transition(TransitionAt),
  /// A clip's fade at one edge.
  Fade(EdgeAt),
  /// A child's place as OpenTimelineIO derives it: where it starts or ends
  /// on its track — exclusively, or at its last tick, `end_time_inclusive`
  /// — a sum of the items before it on the way there, the same in the
  /// timeline, or where it starts or ends in one walk over the whole track.
  TrackPosition(ChildAt),
  /// A track's duration, as OpenTimelineIO sums it, or the last tick of the
  /// track's range, which runs that duration from zero; the stack's
  /// duration is the longest of them, and its range the longest track's.
  TrackDuration(usize),
  /// The stack's duration, which OpenTimelineIO picks by comparing the
  /// tracks' durations in `f64`: a shorter track picked where two compare
  /// equal. [`NotRepresentable::value`] is the longest track's duration.
  StackDuration,
  /// An item's visible range: its source range widened by the handles of
  /// the transitions beside it — its start, its duration, its end or its
  /// last tick.
  Visible(ChildAt),
  /// A child's place in the timeline counted from the global start: its
  /// start, its end, or the end or the last tick of its range from there —
  /// the moved start and the child's own duration, as OpenTimelineIO moves
  /// a child's range into its parent's.
  Absolute(ChildAt),
  /// A track's end counted from the global start — exclusively or at its
  /// last tick; the timeline's end, for the longest track.
  AbsoluteEnd(usize),
  /// The end of a timeline with no track, counted from the global start —
  /// exclusively or at its last tick. OpenTimelineIO gives a stack with no
  /// track, and the timeline with it, a duration of zero at rate 1, so the
  /// timeline's range from the global start ends at the global start
  /// counted at rate 1. A timeline with a track ends with its longest
  /// track ([`AbsoluteEnd`](Self::AbsoluteEnd)).
  TimelineEnd,
  /// A marker's range — its start, its length or its end — refused as the
  /// timeline holds it, before any walk: a track's marker counted at the
  /// edit rate; a clip's in none of its own rulers, nor of the rulers the
  /// timeline's operands are counted in, held whole.
  Marker(MarkerAt),
}

/// Writes `the start`, `the place of track 0, child 2`, and so on.
impl fmt::Display for Spot {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Start => f.write_str("the start"),
      Self::Record(at) => write!(f, "the gap before {at}"),
      Self::Source(at) => write!(f, "the source range of {at}"),
      Self::Available(at) => write!(f, "the available range of {at}"),
      Self::Transition(at) => write!(f, "{at}"),
      Self::Fade(at) => write!(f, "the fade at the {} of {}", at.edge().side(), at.clip()),
      Self::TrackPosition(at) => write!(f, "the place of {at}"),
      Self::TrackDuration(track) => write!(f, "the duration of track {track}"),
      Self::StackDuration => f.write_str("the stack's duration"),
      Self::Visible(at) => write!(f, "the visible range of {at}"),
      Self::Absolute(at) => write!(f, "the place of {at} from the global start"),
      Self::AbsoluteEnd(track) => write!(f, "the end of track {track} from the global start"),
      Self::TimelineEnd => {
        f.write_str("the end of the timeline, which has no track, from the global start")
      }
      Self::Marker(at) => write!(f, "the marked range of {at}"),
    }
  }
}

/// One child of an exported track, by the index of its track in the
/// timeline and its index among the track's OpenTimelineIO children —
/// items and transitions, in the order they are written: gaps, clips, and
/// a dissolve or a fade's transition between two items.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ChildAt {
  track: usize,
  child: usize,
}

impl ChildAt {
  /// Child `child` of track `track`.
  pub const fn new(track: usize, child: usize) -> Self {
    Self { track, child }
  }

  /// The track's index in the timeline.
  pub const fn track(&self) -> usize {
    self.track
  }

  /// The child's index among the track's OpenTimelineIO children.
  pub const fn child(&self) -> usize {
    self.child
  }
}

/// Writes `track 0, child 2`.
impl fmt::Display for ChildAt {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "track {}, child {}", self.track, self.child)
  }
}

/// Checks that `json` is an OpenTimelineIO timeline `target`'s readers
/// read: well-formed JSON, every object of a schema OpenTimelineIO defines,
/// with exactly the keys those readers require — audited against each
/// schema's `read_from`, per target — the types they read for the keys the
/// export writes, and every transition between two items.
///
/// Every dictionary is walked, metadata included: a reader decodes one
/// carrying `OTIO_SCHEMA` as an object of that schema wherever it sits, so
/// the check holds it to that schema's rules, and refuses by name a schema
/// whose rules it does not know.
///
/// This is the structural self-check the export is held to — the shape, not
/// the meaning: it does not recompute positions.
pub fn validate_json(json: &str, target: OtioTarget) -> Result<(), Invalid> {
  let root = json::parse(json).map_err(Invalid::Syntax)?;
  check::timeline(&root, target).map_err(Invalid::Shape)
}

/// Why [`validate_json`] refused a document.
///
/// Marked `#[non_exhaustive]`: a later kind of refusal joins as a variant.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Invalid {
  /// The text is not one well-formed JSON value.
  Syntax(Syntax),
  /// The JSON is not the shape OpenTimelineIO reads.
  Shape(Shape),
}

impl fmt::Display for Invalid {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Syntax(syntax) => fmt::Display::fmt(syntax, f),
      Self::Shape(shape) => fmt::Display::fmt(shape, f),
    }
  }
}

impl core::error::Error for Invalid {}

/// Where the JSON grammar was broken, and how.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Syntax {
  offset: usize,
  reason: &'static str,
}

impl Syntax {
  /// The byte offset at which the text stopped being JSON.
  pub const fn offset(&self) -> usize {
    self.offset
  }

  /// What was wrong there.
  pub const fn reason(&self) -> &'static str {
    self.reason
  }
}

/// Writes `byte 12: expected a value`.
impl fmt::Display for Syntax {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "byte {}: {}", self.offset, self.reason)
  }
}

/// Where the document left OpenTimelineIO's shape, and what was expected
/// there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shape {
  path: Utf8Bytes,
  expected: &'static str,
}

impl Shape {
  /// Where, as a path from the document's root: `$.tracks.children[0]`.
  pub fn path(&self) -> &str {
    &self.path
  }

  /// What OpenTimelineIO reads there.
  pub const fn expected(&self) -> &'static str {
    self.expected
  }
}

/// Writes `$.tracks.children[0].kind: expected a string`.
impl fmt::Display for Shape {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "{}: expected {}", self.path, self.expected)
  }
}

impl core::error::Error for Shape {}

/// Reads OpenTimelineIO JSON back into a [`Timeline`]: the exact inverse of
/// [`to_otio`].
///
/// For every timeline [`to_otio`] writes, for either target,
/// `from_otio(&to_otio(&timeline, target)?)` is `Ok` of that very timeline.
/// The reader takes the edit rate, and for each media-side range the exact
/// ruler its counts are written in and the timebase the model counts it in,
/// from the words the export writes under `metadata.mediaio`, and holds
/// every `RationalTime` to them: its rate the `f64` of its exact ruler, bit
/// for bit, and its value a whole count within ±2^53. Every number is read
/// as OpenTimelineIO 0.17.0's parser reads it — RapidJSON at the commit the
/// release pins, in its normal precision, which does not always round to
/// the nearest double: a count as the whole number the `double` it reads
/// is, a rate as that `double`, a gain as the `f32` whose shortest spelling
/// reads as the same `double` — so `2.4e1` reads as `24.0` does, and so does
/// `24.0000000000000001`, whose digits past the 17th RapidJSON drops. A
/// number RapidJSON refuses (`0e309`, `2e308`) is refused as syntax, and so
/// is one OpenTimelineIO's builds read as different values, as a build that
/// fuses one of RapidJSON's multiply-adds reads some long numbers; NaN and
/// an infinity, which it reads, are refused by name in a time or a rate, and
/// so is a count of `-0.0`, whose sign no whole count keeps. What OpenTimelineIO
/// derives from those counts in its own `f64` arithmetic is held as
/// [`to_otio`] holds it before writing: both ends of every range — its end
/// and its last tick — and every place, visible range and duration of the
/// walk of each track and of the stack, so a document whose times
/// OpenTimelineIO would read rounded is refused. It lays each track's
/// items end to end from zero, as OpenTimelineIO places them — a clip as
/// long as its source range at the edit rate — and puts each fade on the
/// clip beside its gap and each dissolve on the cut it straddles. A track
/// ends with its last clip: a gap of some length after it, which
/// OpenTimelineIO counts in the track's duration, is refused
/// ([`Feature::TrailingGap`]), and so is a gap of no length anywhere but
/// where the export writes one, on a fade's gap side
/// ([`Feature::StandaloneGap`]). A gap states its range, at the edit rate,
/// as the export writes it. Both clip schemas are read: `Clip.2` and
/// `Clip.1`.
///
/// A document from another writer is read where it uses only what the
/// timeline models and carries the crate's words. A schema the timeline
/// does not model — an effect, a time warp, a nested stack, a reference
/// other than an `ExternalReference.1`, a marker of another version — or
/// one it models elsewhere is refused with its name and where it sits
/// ([`Unreadable::Schema`]); a key, value or note of a modelled schema the
/// timeline has no word for is refused by name too
/// ([`Unreadable::Feature`]). On the timeline, a track, a clip and a
/// marker, each key of the `metadata` beside `mediaio` is another reader's
/// namespace, read whole into the object's
/// [`ReaderMetadata`](crate::ReaderMetadata) — `{}` as an empty one — and
/// refused by name where it is no object ([`Feature::Metadata`]) or holds a
/// value that is no string ([`Feature::NamespaceValue`]); on every other
/// object such a key is refused by name ([`Feature::Metadata`]). A member is read only where the timeline keeps
/// it, or where OpenTimelineIO 0.17.0 holds it as it holds its absence:
/// nothing is dropped — a `color` on an item and an `enabled` on a
/// transition, which OpenTimelineIO keeps as dynamic fields, are refused by
/// name, `null` and `true` among them. A marker's color is one of the eleven
/// names exactly, in capitals. A member OpenTimelineIO's schema lets a document
/// leave out reads as OpenTimelineIO 0.17.0's reader reads it — a name as
/// empty, `enabled` as `true`, a marker's color as GREEN — or, where that
/// is no value and the timeline needs one (the timeline's
/// `global_start_time`, a clip's `source_range`), or a value it does not
/// keep (a gap's `source_range`, nothing at rate 1), is refused by name.
///
/// **What it reads, the export writes back.** Last, the export writes the
/// timeline read, and every value it writes must be the one the document
/// states at the same path, as OpenTimelineIO 0.17.0's reader holds it: a
/// string byte for byte, a boolean exactly, a `RationalTime`'s rate and value
/// as doubles — `24` and `24.0` alike — and a number in a dictionary as the
/// integer or the double RapidJSON reads it as; two doubles bit for bit, so
/// `-0.0` is not `0.0`. An object's keys may come in any order. A member the export writes may be missing only where
/// OpenTimelineIO reads its absence as that very value — an empty name, no
/// metadata, effects or markers, `enabled`, no ranges, a GREEN marker with
/// no comment — and one the document states may go unwritten only where
/// OpenTimelineIO holds it as it holds its absence: a reference's image
/// bounds of `null` in a document for `Legacy`. The first value that
/// differs is refused as [`Unreadable::Inexact`] there, naming both. The
/// checks before it refuse by name what they know; this one closes the
/// rest of the class — a value read, then written back otherwise — so the
/// documents read are exactly those the export reproduces.
///
/// # Errors
///
/// - [`Unreadable::Syntax`] for text that is not JSON as OpenTimelineIO
///   0.17.0's parser reads it — a number it refuses among it — or a number
///   its builds read as different values;
/// - [`Unreadable::Shape`] for a value missing or not of the type read
///   there, a word of the crate's among them;
/// - [`Unreadable::Schema`] and [`Unreadable::Feature`] for what the
///   timeline does not model;
/// - [`Unreadable::Inexact`] for a number the model holds only rounded, or
///   not at all — NaN, an infinity, a count of `-0.0` — a count or rate that
///   does not land exactly where the timeline counts it, a time
///   OpenTimelineIO's `f64` arithmetic rounds, or a value the export does
///   not write back as the document states it;
/// - [`Unreadable::Validation`] for a timeline read whole that does not
///   [`validate`](fn@validate).
pub fn from_otio(json: &str) -> Result<Timeline, Unreadable> {
  let root = json::parse(json).map_err(Unreadable::Syntax)?;
  let timeline = import::timeline(&root)?;
  validate(&timeline).map_err(Unreadable::Validation)?;
  import::reproduced(&root, &timeline)?;
  Ok(timeline)
}

/// Why [`from_otio`] read no timeline.
///
/// Marked `#[non_exhaustive]`: a later kind of refusal joins as a variant.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Unreadable {
  /// The text is not one well-formed JSON value.
  Syntax(Syntax),
  /// A value is missing, or not of the type read there.
  Shape(Shape),
  /// An object of a schema the timeline does not model, or models in
  /// another place.
  Schema(UnmodelledSchema),
  /// A key, a value or a note of a modelled schema the timeline has no
  /// word for.
  Feature(UnmodelledFeature),
  /// A count or a rate that does not land exactly where the timeline
  /// counts it, a time OpenTimelineIO derives from the document that its
  /// `f64` arithmetic rounds, or a value the export would not write back as
  /// the document states it.
  Inexact(Inexact),
  /// The timeline read does not [`validate`](fn@validate): its refusals.
  Validation(Vec<Refusal>),
}

impl fmt::Display for Unreadable {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Syntax(syntax) => fmt::Display::fmt(syntax, f),
      Self::Shape(shape) => fmt::Display::fmt(shape, f),
      Self::Schema(schema) => fmt::Display::fmt(schema, f),
      Self::Feature(feature) => fmt::Display::fmt(feature, f),
      Self::Inexact(inexact) => fmt::Display::fmt(inexact, f),
      Self::Validation(refusals) => {
        f.write_str("the timeline read does not validate")?;
        for (index, refusal) in refusals.iter().enumerate() {
          f.write_str(if index == 0 { ": " } else { "; " })?;
          fmt::Display::fmt(refusal, f)?;
        }
        Ok(())
      }
    }
  }
}

impl core::error::Error for Unreadable {}

/// An object of a schema the timeline does not model there, and where it
/// sits: what [`Unreadable::Schema`] carries.
///
/// Held boxed, so a refusal stays small to return.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnmodelledSchema(Box<SchemaAt>);

#[derive(Debug, Clone, PartialEq, Eq)]
struct SchemaAt {
  name: Utf8Bytes,
  path: Utf8Bytes,
}

impl UnmodelledSchema {
  pub(crate) fn new(name: Utf8Bytes, path: Utf8Bytes) -> Self {
    Self(Box::new(SchemaAt { name, path }))
  }

  /// The schema's name and version: `Effect.1`.
  pub fn name(&self) -> &str {
    &self.0.name
  }

  /// Where the object sits, as a path from the document's root:
  /// `$.tracks.children[0].children[1].effects[0]`.
  pub fn path(&self) -> &str {
    &self.0.path
  }
}

/// Writes `$.tracks.children[0].children[1].effects[0]: the schema
/// Effect.1, which the timeline does not model there`.
impl fmt::Display for UnmodelledSchema {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(
      f,
      "{}: the schema {}, which the timeline does not model there",
      self.0.path, self.0.name
    )
  }
}

impl core::error::Error for UnmodelledSchema {}

/// A key, a value or a note of a modelled schema the timeline has no word
/// for, and where it sits: what [`Unreadable::Feature`] carries.
///
/// Held boxed, so a refusal stays small to return.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnmodelledFeature(Box<FeatureAt>);

#[derive(Debug, Clone, PartialEq, Eq)]
struct FeatureAt {
  feature: Feature,
  path: Utf8Bytes,
}

impl UnmodelledFeature {
  pub(crate) fn new(feature: Feature, path: Utf8Bytes) -> Self {
    Self(Box::new(FeatureAt { feature, path }))
  }

  /// What the document holds that the timeline does not model.
  pub fn feature(&self) -> &Feature {
    &self.0.feature
  }

  /// Where it sits, as a path from the document's root.
  pub fn path(&self) -> &str {
    &self.0.path
  }
}

/// Writes `$.tracks.children[0].kind: the track kind Data, where the
/// timeline knows Video and Audio`.
impl fmt::Display for UnmodelledFeature {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "{}: {}", self.0.path, self.0.feature)
  }
}

impl core::error::Error for UnmodelledFeature {}

/// What a document holds, in a schema the timeline models, that the
/// timeline has no word for.
///
/// Marked `#[non_exhaustive]`: a later feature joins as a variant.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Feature {
  /// A key the reader does not know on its object.
  Key(Utf8Bytes),
  /// A note in an object's `metadata` under a key other than this crate's
  /// `mediaio`: another writer's, which the timeline does not keep — on
  /// an object that keeps no reader's namespace (the stack, a gap, a
  /// transition, a media reference), or, on one that does (the timeline, a
  /// track, a clip, a marker), one whose value is no object, which no
  /// namespace is.
  Metadata(Utf8Bytes),
  /// An entry of a reader's namespace whose value is no string: a
  /// namespace holds strings alone ([`ReaderMetadata`](crate::ReaderMetadata)),
  /// and the timeline keeps no other value there.
  NamespaceValue(Utf8Bytes),
  /// A word under `mediaio` the reader does not know there.
  Word(Utf8Bytes),
  /// A name where the timeline keeps none: the stack's, other than
  /// `tracks`, a gap's, a transition's or a media reference's.
  Name(Utf8Bytes),
  /// A track kind other than `Video` and `Audio`.
  TrackKind(Utf8Bytes),
  /// A transition type other than `SMPTE_Dissolve`.
  TransitionType(Utf8Bytes),
  /// A trim — a `source_range` — on the stack or a track.
  Trim,
  /// The stack or a gap disabled.
  Disabled,
  /// A `color` on an item, `null` among them: OpenTimelineIO 0.18's item
  /// color (`item.cpp` 181 in 0.18.1), not modelled yet. OpenTimelineIO
  /// 0.17.0 keeps it as a dynamic field, which the timeline would drop, and
  /// 0.18 writes one on every item, so a document 0.18 writes is refused
  /// here until the item color is modelled.
  ItemColor,
  /// A marker color outside OpenTimelineIO's eleven.
  MarkerColor(Utf8Bytes),
  /// Image bounds on a media reference.
  ImageBounds,
  /// A clip's media references other than one, under `DEFAULT_MEDIA`, the
  /// active one.
  MediaReferences,
  /// A clip with no media reference.
  NoMedia,
  /// A gap whose source range starts anywhere but zero.
  GapStart,
  /// A gap of some length after a track's last clip — any gap of a track
  /// with no clip among them. OpenTimelineIO counts it in the track's
  /// duration, and so in the timeline's, while the timeline's track ends
  /// with its last clip.
  TrailingGap,
  /// A gap of no length anywhere but where the export writes one — on a
  /// fade's gap side, before a fade-in or after a fade-out, beside no other
  /// gap — a gap of a track with no clip among them included.
  /// OpenTimelineIO counts it all the same: its range is the item's
  /// duration (`item.cpp` 43–46 in 0.17.0), and its rate joins the track's
  /// sum, which starts at rate 1 and takes the higher of two rates
  /// (`track.cpp` 111–141; `rationalTime.h` 283–294), while the timeline
  /// has nothing there.
  StandaloneGap,
  /// A fade not between a gap and the clip it fades, or playing anything
  /// of the gap's side.
  Fade,
  /// A dissolve beside a gap: OpenTimelineIO's fade from black or silence,
  /// whose curve the document does not name.
  DissolveBesideGap,
}

/// Writes `the track kind Data, where the timeline knows Video and Audio`,
/// and so on.
impl fmt::Display for Feature {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Key(key) => write!(f, "the key {key}, which this reader does not know there"),
      Self::Metadata(key) => write!(
        f,
        "the note {key}, another writer's, which the timeline does not keep"
      ),
      Self::NamespaceValue(key) => write!(
        f,
        "the entry {key}, which is no string: a reader's namespace holds strings alone"
      ),
      Self::Word(word) => write!(f, "the word {word}, which this reader does not know there"),
      Self::Name(name) => write!(f, "the name {name:?}, where the timeline keeps none"),
      Self::TrackKind(kind) => write!(
        f,
        "the track kind {kind}, where the timeline knows Video and Audio"
      ),
      Self::TransitionType(kind) => write!(
        f,
        "the transition type {kind}, where the timeline knows SMPTE_Dissolve"
      ),
      Self::Trim => f.write_str("a trim, which the timeline keeps on clips alone"),
      Self::Disabled => f.write_str("disabled, which the timeline keeps on tracks and clips alone"),
      Self::ItemColor => f.write_str(
        "a color on an item, null among them: OpenTimelineIO 0.18's item color, not modelled yet",
      ),
      Self::MarkerColor(color) => write!(
        f,
        "the marker color {color}, outside OpenTimelineIO's eleven"
      ),
      Self::ImageBounds => f.write_str("image bounds, which the timeline does not keep"),
      Self::MediaReferences => f.write_str(
        "media references other than one, under DEFAULT_MEDIA and active, which a clip reads",
      ),
      Self::NoMedia => f.write_str("a clip with no media reference"),
      Self::GapStart => f.write_str("a gap starting anywhere but zero"),
      Self::TrailingGap => {
        f.write_str("a gap of some length after the track's last clip, where its track has ended")
      }
      Self::StandaloneGap => f.write_str(
        "a gap of no length where no fade stands beside it, which OpenTimelineIO still counts",
      ),
      Self::Fade => {
        f.write_str("a fade not between a gap and the clip it fades, or playing the gap's side")
      }
      Self::DissolveBesideGap => {
        f.write_str("a dissolve beside a gap: a fade whose curve the document does not name")
      }
    }
  }
}

/// A count or a rate that does not land exactly where the timeline counts
/// it, a time OpenTimelineIO's `f64` arithmetic rounds, or a value the
/// export would not write back as stated, and where: what
/// [`Unreadable::Inexact`] carries. A time OpenTimelineIO derives is refused
/// where it is derived from: a range at the range, a place or a visible
/// range at its child, a duration or an end at its track, the stack's pick
/// of its longest track at the stack. A value the export would write
/// otherwise is refused at the first that differs, with both values
/// ([`Inexact::written`], [`Inexact::stated`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inexact {
  path: Utf8Bytes,
  reason: &'static str,
  /// Where the export would write back another value than the document
  /// states: the value it writes, and the document's.
  values: Option<Box<(Utf8Bytes, Utf8Bytes)>>,
}

impl Inexact {
  /// A value that does not land exactly at `path`, for `reason`.
  pub(crate) const fn new(path: Utf8Bytes, reason: &'static str) -> Self {
    Self {
      path,
      reason,
      values: None,
    }
  }

  /// A value the export would write back as `written` where the document
  /// states `stated`, at `path` — each as JSON, or `nothing` for a member
  /// left out.
  pub(crate) fn with_values(
    path: Utf8Bytes,
    reason: &'static str,
    written: Utf8Bytes,
    stated: Utf8Bytes,
  ) -> Self {
    Self {
      path,
      reason,
      values: Some(Box::new((written, stated))),
    }
  }

  /// Where, as a path from the document's root.
  pub fn path(&self) -> &str {
    &self.path
  }

  /// Why the value does not land exactly.
  pub const fn reason(&self) -> &'static str {
    self.reason
  }

  /// Where the export would write back another value than the document
  /// states, the value it writes there, as JSON — `nothing` where it writes
  /// none.
  pub fn written(&self) -> Option<&str> {
    self.values.as_deref().map(|(written, _)| written.as_str())
  }

  /// Where the export would write back another value than the document
  /// states, the document's value there, as JSON — `nothing` where it states
  /// none.
  pub fn stated(&self) -> Option<&str> {
    self.values.as_deref().map(|(_, stated)| stated.as_str())
  }
}

/// Writes `$.global_start_time.value: a count that is no whole number`, and
/// where the export would write another value back, both:
/// `$.tracks.children[0].markers[0].color: a value the export does not write
/// back as the document states it: the export writes "GREEN", the document
/// states "Green"`.
impl fmt::Display for Inexact {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "{}: {}", self.path, self.reason)?;
    match self.values.as_deref() {
      Some((written, stated)) => write!(
        f,
        ": the export writes {written}, the document states {stated}"
      ),
      None => Ok(()),
    }
  }
}

impl core::error::Error for Inexact {}

#[cfg(test)]
mod tests;
