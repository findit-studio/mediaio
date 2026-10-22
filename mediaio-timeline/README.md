<div align="center">
<h1>mediaio-timeline</h1>
</div>
<div align="center">

The editing timeline as data: tracks, clips at explicit record ranges, gain,
fades, dissolves and markers — with validation that refuses by name, a diff,
and OpenTimelineIO export with its exact read-back. Pure Rust, `no_std` +
`alloc`, no FFmpeg.

[<img alt="github" src="https://img.shields.io/badge/github-findit--studio/mediadecode-8da0cb?style=for-the-badge&logo=Github" height="22">][Github-url]
<img alt="LoC" src="https://img.shields.io/endpoint?url=https%3A%2F%2Fgist.githubusercontent.com%2Fal8n%2F327b2a8aef9003246e45c6e47fe63937%2Fraw%2Fmediaio-timeline" height="22">
[<img alt="Build" src="https://img.shields.io/github/actions/workflow/status/findit-studio/mediadecode/ci-core.yml?logo=Github-Actions&style=for-the-badge" height="22">][CI-url]
[<img alt="docs.rs" src="https://img.shields.io/badge/docs.rs-mediaio--timeline-66c2a5?style=for-the-badge&labelColor=555555" height="20">][doc-url]
[<img alt="crates.io" src="https://img.shields.io/crates/v/mediaio-timeline?style=for-the-badge" height="22">][crates-url]
<img alt="license" src="https://img.shields.io/badge/License-Apache%202.0/MIT-blue.svg?style=for-the-badge" height="22">

</div>

## The model

| type | what it is |
|---|---|
| `Timeline` | an edit: `name`, the edit `rate`, the timecode `start` its zero carries, `metadata`, `tracks` stacked bottom first, and its `readers` — a `schema 1` document |
| `Track` | one layer: `kind` (`Video` or `Audio`), `name`, `enabled`, `clips` in record order, the `transitions` between them, its `markers` and its `readers` |
| `Clip` | a stretch of one medium at an explicit position: its `id`, `name`, `media`, `source_range` (in the medium's timebase), `record` (at the edit rate), `enabled`, `gain`, `fades`, `metadata`, `markers`, `readers` |
| `ClipId` | a clip's identity: a string its creator mints, once per placement, unique in the timeline |
| `MediaRef` | the medium: its `locator`, and when known its `available_range`, `rate` and `reel` |
| `Transition` | a `Dissolve` across the cut `at`, reaching `in_offset` before it and `out_offset` after it — OpenTimelineIO's offsets |
| `Fades`, `Fade` | a fade at a clip's start (`in`) and end (`out`): a `duration` at the edit rate and a `shape`, `Linear` or `EqualPower` |
| `Gain` | decibels, finite by construction |
| `Marker`, `MarkerColor` | a named point or stretch of time: a `name`, the `range` it marks, a `color` (OpenTimelineIO's eleven, green by default), a `comment` and its `readers` — on a track at the edit rate from the timeline's zero, on a clip in its medium's time |
| `Metadata` | an ordered map of strings — notes the model carries and never reads |
| `ReaderMetadata` | other readers' own metadata on the timeline, a track, a clip or a marker: under each reader's namespace — `Resolve_OTIO`, say — a `Metadata` of that reader's words, which the export writes where that reader reads them and the model never reads |

The time words — `Rate`, `Timebase`, `Timestamp`, `Duration`, `TimeRange` —
are [`mediatime`](https://crates.io/crates/mediatime)'s, re-exported.

### Positions are explicit

A clip's `record` is stored, not derived from the clips before it: trimming
one clip moves no other, and a diff calls a clip moved only when its own
record changed. Every position on the record side — a record, the start, a
transition's cut and offsets, a fade's length — is counted at the edit rate,
from the timeline's zero. `start` is the timecode that zero carries
(`01:00:00:00` is 86 400 frames at 24 fps); it labels the timeline and moves
nothing. With no time-warp in schema 1, a source range runs a whole number of
edit-rate ticks and its record exactly as long: nothing rounds a source onto
the edit rate.

Gaps are never stored. `layout` derives them — from the zero to the first
record, and between records — for formats that place items end to end.

### A clip is its id

A clip is identified by its `id`, which its creator mints — findit's edit
plan mints one per placement — and which no other clip of the timeline
carries. Its name and its medium may repeat: a cut back to a shot, or a
loop of it, places one medium twice, under two ids, and a medium on a video
and an audio track is two clips with two ids. `diff` matches clips by the
id, so a clip renamed or pointed at another medium is still the same clip.

### Text is `Utf8Bytes`

Every text the model holds — a timeline's, a track's, a clip's and a
marker's name, a clip's id, a medium's locator and reel, a marker's comment,
a note's key and value — and the OpenTimelineIO document `to_otio` writes is
a
[`smol_bytes::Utf8Bytes`](https://docs.rs/smol-bytes): immutable UTF-8, held
inline when short and shared by reference count when long, so a clone is
cheap. Constructors and setters take `impl Into<Utf8Bytes>` — a `&str`, a
`String` or a `Utf8Bytes` — and a reel, being optional, an
`Option<Utf8Bytes>`; getters lend a `&str`. `smol_bytes::format_utf8bytes!`
formats straight into one.

## Validation

`validate(&Timeline) -> Result<(), Vec<Refusal>>` answers every defect at
once, in a fixed order, each naming the clip, pair of clips, range, edge,
transition, marker or reader's namespace it is about:

| refusal | the defect |
|---|---|
| `RateUnstated` | the edit rate is zero |
| `OffEditRate` | the start, a record, a fade, a transition or a track's marker not counted at the edit rate |
| `MediaRateUnstated` | a stated media rate of zero, or a source or available range in a degenerate timebase |
| `RangeTooLong` | a record, source range or available range longer than `i64::MAX` ticks — no timeline needs one, and its length has no exact count |
| `RecordBeforeZero` | a record starting before the timeline's zero |
| `EmptyRecord` | a record covering no time |
| `OutOfOrder` | a track's clips out of record order |
| `Overlap` | two records of one track overlapping — a transition blends with media outside the records, so it is never one |
| `EmptyClipId` | a clip whose id is empty |
| `DuplicateClipId` | a clip carrying the id of an earlier clip of the timeline, on its track or another — the identity `diff` matches by |
| `SourceOffEditRate` | a source range whose length is no whole number of edit-rate ticks |
| `DurationMismatch` | a record not exactly its source's length at the edit rate |
| `OutsideAvailable` | a source range outside its medium's available range, when that is known |
| `OffBoundary` | a transition not on a cut where one record ends and the next begins |
| `DuplicateTransition` | a second transition on one cut |
| `HandleMissing` | the outgoing clip's medium lacks `out_offset` after its source range, or the incoming clip's lacks `in_offset` before it |
| `FadeMeetsTransition` | a fade where a transition already blends |
| `BlendsOverrunClip` | a clip's fades and the parts of transitions inside it running longer than the clip — a fade longer than its clip among them |
| `MarkerRateUnstated` | a clip's marker in a degenerate timebase |
| `MarkerTooLong` | a marker's range longer than `i64::MAX` ticks |
| `ReservedNamespace` | a reader's namespace named `mediaio`, the key the export writes the crate's own words under, or `OTIO_SCHEMA` |
| `SchemaKeyInNamespace` | a key `OTIO_SCHEMA` in a reader's namespace, which OpenTimelineIO would read as a schema word |

Nothing holds a marker inside its owner's time: a point past a track's last
record, or a stretch of a medium its clip does not play, is a note the
timeline may carry.

Building a timeline checks nothing: the value is plain data, and `validate`
is where it is judged.

## Diff

`diff(&before, &after) -> Result<Delta, Ambiguous>` reports, per track in
record order, each clip `Added`, `Removed`, `Moved` (its record changed),
`Retimed` (its source range changed), `Regained` (its gain or fades
changed), `EnabledFlipped`, `Renamed` or `Relinked` (its medium's locator
changed). Tracks match by index, and a clip by its `id` alone: a clip that
changes track is removed from one and added to the other. A side carrying
one id on two clips — a timeline `validate` would refuse — is refused as
`Ambiguous`, naming the side and the two clips, rather than matched by
occurrence. Ranges compare by the time they cover. The order is fixed, so
`diff(a, a)` is empty for every `a` it answers. Only clips are compared: the
timeline's and a track's own words, transitions, markers, a clip's notes,
and other readers' metadata are not.

## OpenTimelineIO

`otio::to_otio(&Timeline, OtioTarget) -> Result<Utf8Bytes, otio::Refused>`
writes a valid timeline as OpenTimelineIO JSON, keys in OpenTimelineIO's own
writer order. A timeline that does not validate is refused with its
refusals (`Refused::Validation`); a count OpenTimelineIO cannot hold exactly
is refused by name (`Refused::NotRepresentable`, below).

| model | OpenTimelineIO |
|---|---|
| `Timeline` | `Timeline.1`; `global_start_time` always written, from `start`, at the edit rate |
| its tracks | one `Stack.1` named `tracks` |
| `Track` | `Track.1`: `kind` `Video` or `Audio`, `enabled` |
| `Clip` | `Clip.2` for `OtioTarget::V0_15Plus` (OpenTimelineIO 0.15 and later), `Clip.1` for `OtioTarget::Legacy`; `enabled` |
| a clip's trim | `source_range`: the source range whole, `start_time` and `duration` in the medium's one ruler — by the exactness rule, a duration as long as the record |
| `MediaRef` | `ExternalReference.1` (under `DEFAULT_MEDIA` in a `Clip.2`): `target_url` is the locator, `available_range` the available range |
| a gap between records | `Gap.1`, derived by `layout` |
| `Transition` | `Transition.1`, `SMPTE_Dissolve`, offsets at the edit rate |
| a fade | `Transition.1`, `SMPTE_Dissolve` against a gap — of no length where the clip abuts a clip or the track's end |
| `Marker` | `Marker.2` in its track's or clip's `markers`: `name`, `color` in OpenTimelineIO's capitals, `marked_range`, `comment` — a track's at the edit rate, a clip's in its medium's time |
| a clip's `id`, gain, reel, the medium's rate, a fade's curve, `Metadata` | `metadata.mediaio`: `id`, `gain_db`, `reel`, `rate`, `fade` and `shape`, `metadata` |
| `ReaderMetadata`, on the timeline, a track, a clip or a marker | the object's `metadata`, beside `mediaio`: each namespace under its own key, an object of strings, written whole — `{}` for one holding no entry — every key in byte order; an object with no namespace writes none |

Record-side times are whole counts at the edit rate. A media-side range — a
source range, an available range — is written whole in one ruler: frames of
the medium's stated rate where its start and length both land on one, else
ticks of its own timebase. The end OpenTimelineIO derives from a range (its
start rescaled to its duration's rate, plus the duration) is the range's own
end, held within ±2^53 as every count it derives is (below). Because a
source range runs a whole number of edit-rate ticks, walking the exported
items end to end lands every clip on its record. Where a medium's ruler is
not the edit rate, one track's items are counted in more than one rate, and
OpenTimelineIO sums those in floating point: a position derived that way is
read to the nearest frame, not truncated — the export holds it under half a
tick of the exact count (below).

**Every count OpenTimelineIO reads or derives is one it holds exactly.** A
`RationalTime` keeps its value in an `f64`, which holds every whole number
up to 2^53 and only some past it, and OpenTimelineIO derives a document's
positions in that arithmetic: a sum of two times is carried in the higher
of their two rates, the other rescaled into it — two rounded operations. The
export holds both:

- **what it writes.** Every count lies within ±2^53, and a rate is the `f64`
  nearest it, in its shortest spelling. A media-side
  range its own rulers count past 2^53 — a medium stamped in nanoseconds
  since 1970 — is written in the coarsest ruler of a whole number of ticks a
  second that holds its start and its length, whole seconds or
  milliseconds, when its counts there are within 2^53. Of those rulers a
  range is written in the first whose counts end it within ±2^53 too; where
  none does, a source range is written in the first that writes its start
  and its length, its end left to the walk and to the search below. Past
  its own rulers, a range is written in the first ruler the timeline's
  operands are counted in, finest first, that does what its own could not:
  a source range none of them writes, in the first that writes its start
  and its length, its end left to the walk likewise; an available range
  none of them ends within ±2^53, in the first whose counts end it there
  too — no walk reads an available range, so its end is held as it is
  planned. Such a ruler is at a fractional rate, such as the edit rate at
  one frame every two seconds, as every whole rate that lands on a range is
  a multiple of the coarsest, which counts it in the smallest counts; a
  range none of those rulers writes — or, an available range, ends — is
  refused.
- **how it reads back.** OpenTimelineIO 0.17.0 reads numbers with RapidJSON
  in its normal precision, which does not always round to the nearest
  double. It reads every count written `<digits>.0` up to
  1 801 439 850 948 198 (⌊2^54 / 10⌋) exactly, but past it not every one —
  `9007199254740991.0` as 9007199254740990 — so a count past it is written
  as its integer, which it hands over whole. It reads the shortest spelling
  of every common rate exactly (24, 25, 30, 48, 50, 60, the NTSC rates over
  1001, the audio rates, 90 kHz), but of some others a neighbouring `f64`
  (96000/1001's), so such a rate is written in another spelling of the same
  `f64` that it reads exactly, in a build that fuses one of its
  multiply-adds and in one that does not: an integer an `f64` holds over a
  power of ten (`95904095904095904e-15`), or the same with zeros the reader
  multiplies in. A rate no such spelling writes is refused by name
  (`NotRepresentable::misread`); of the rates num/1001 up to
  200 000/1001, three: 29195/1001, 31264/1001 and 31799/1001.
- **what OpenTimelineIO derives from it**, computed operation for operation
  and branch for branch as OpenTimelineIO computes it, beside its exact
  value: the end of each clip's source range; each child's place on its
  track — from zero in that child's own rate, every item before it added,
  as `range_of_child_at_index` sums it, and as one walk over the whole
  track carries it — and in the timeline; each item's visible range, its
  source range widened by the handles of the transitions beside it; each
  track's duration, and the stack's, the longest of them as OpenTimelineIO
  picks it; the global start added to each place and to each track's end,
  as OpenTimelineIO's own tools add it; each child's range from the global
  start — the moved start and the child's own duration, as OpenTimelineIO
  moves a child's range into its parent's, so its end rescales the moved
  start into the duration's rate, a rounding of its own; for a timeline
  with no track, its range from the global start, which OpenTimelineIO
  gives no duration at rate 1, so it ends at the global start counted in
  seconds; and the last tick of every one of those ranges —
  `end_time_inclusive`, which floors a range's end where the duration's
  double has a fraction and takes a tick off it where it has none, so a
  duration whose double rounds onto a whole number, or off one, can move
  the last tick by up to a tick.
  Each must lie within ±2^53 in the ruler OpenTimelineIO carries it in, and
  OpenTimelineIO's double less than half a tick from the exact count — read
  to the nearest tick, it is the exact count. On one ruler nothing is
  rescaled, and the double is the count itself: a range the export writes
  in one ruler ends inclusively where it exactly does.

**Exact, or refused.** The export's guarantee is soundness: what it writes,
OpenTimelineIO reads back exactly, and a timeline it cannot so write is
refused by name. Where OpenTimelineIO would round a value, the export
writes the clips the value is formed from in other rulers that hold their
source ranges exactly and walks the timeline again — a search, bounded by
contract. A clip's rulers, in the order the search tries them:

1. its plan's: one of its own — frames of its medium's stated rate, ticks
   of its source's timebase, or the coarsest whole rate that holds the
   range — or, where none of those writes the range, the first ruler the
   timeline's operands are counted in that does;
2. every ruler the timeline's operands are counted in that holds the range
   — the edit rate, at which the global start, the gaps and the transitions
   are written, the rate 1 OpenTimelineIO sums a track's duration from, and
   every clip's planned ruler — finer or coarser than its plan's, all of
   them, finest first;
3. the 64 finest whole numbers of ticks a second below its plan's that
   hold the range and that no operand is counted in, finest first;
4. the 64 coarsest of those, finest first, less any already listed;
5. its plan's again, where every other is finer.

So where OpenTimelineIO would rescale a clip into the edit rate or into a
neighbour's ruler, the search can write the clip in that ruler itself. It
moves one clip to the next ruler of its list at a time — of the clips the
refused value is formed from, the one with the finest ruler — and walks
again, so clips can meet on a ruler they share, or one move while its
neighbour keeps its own. A clip never moves back, so for `n` clips and `d`
distinct operand rulers the search ends within `1 + n · (d + 127)` walks.

What no plan the search walks holds is refused,
`Refused::NotRepresentable`, naming where (`otio::Spot`; a child of an
exported track is an `otio::ChildAt`), the count the last plan could not
hold and the ruler it is counted in — and, for a count OpenTimelineIO
derives, a source range's end among them, what the search tried:
`otio::RulerSearch`, its bands (`otio::RulerBand`) and its walks. So a
refusal says the search was bounded. A source range no ruler of the bands
writes — its start or its length past 2^53 in each of its own rulers and
in each ruler the timeline's operands are counted in, the only rulers of
the bands that can write a range its own cannot — leaves the search no
plan to walk, and so does an available range none of them holds whole,
its end with its start and its length, as the search never moves it: each
is refused before any walk, and says so, naming the bands and no walk
(`RulerSearch::walks` 0). Refused before any walk, naming no search: a
count written at the edit rate, which no ruler of the search changes. A
timeline that only a ruler outside the bands, or a plan of rulers the
moves do not reach, would hold is refused by this contract, never written
to be read rounded: a complete search would try every
holding ruler of every clip together, a product space with no closed form
for OpenTimelineIO's double rounding. Where the bound has been met, counts
lie near 2^53 ticks of a clip's ruler — at the rates media run at,
positions thousands of years in: 2^53 ticks are some 1 500 years even at
192 kHz. The rounding a real timeline meets is a rate or a rescale no `f64`
holds exactly — NTSC's 30000/1001, or frames of one rate counted in
another — and the half-tick hold settles it: written where
OpenTimelineIO's double lands within half a tick of the exact count,
refused by name where it does not.

OpenTimelineIO has no word for a clip's id, its gain or a reel: all three
ride in `metadata` — the id so a reader can tell each clip again — and an
application that does not read it — DaVinci Resolve among them — applies
neither gain nor reel. Beside them ride the exact words a reader needs to
rebuild the timeline exactly, since OpenTimelineIO keeps a rate as an `f64`
that names many rationals and a range as two counts that say nothing of the
model's timebase: the edit rate (`rate`, as `num/den`); for each media-side
range — a source range, an available range, a clip's marker — the exact
ruler its counts are written in and the timebase the model counts it in
(`source`, `available`, `range`); for a track that keeps its
transitions out of their cuts' order, each dissolve's place among them
(`order`); and where an edit-side value spells its timebase otherwise than
the edit rate's reciprocal — `1/24` beside a rate of `48/2` — that
spelling (`start`, `record`, `timebase`, `at`, `in_offset`,
`out_offset`), so every value reads back as it was written. The model's
notes ride beside the clip's words as an array of `[key, value]` pairs,
never as an object: a note's key is anything, and OpenTimelineIO reads an
object carrying `OTIO_SCHEMA` as an object of that schema. Available
ranges and the start are written as the timeline holds them: a medium read
through `mediadecode` starts at zero until the read side exposes the
container's timecode, so its available range does not yet carry the
camera's.

Other readers' own metadata rides in the same `metadata`, each namespace
under its own key, where its reader reads it: DaVinci Resolve 21.1 reads a
marker's note from `Resolve_OTIO`'s `Note` alone, not from OpenTimelineIO's
`comment` (findit-studio/application#265), so a writer that wants the note
in Resolve puts it there — the crate interprets no namespace. A namespace
holds strings alone, and `validate` refuses one named `mediaio` or
`OTIO_SCHEMA`, and a key `OTIO_SCHEMA` inside one, which OpenTimelineIO
would read as a schema word.

`otio::validate_json(&str, OtioTarget)` is the structural self-check the
export is held to: the crate's own JSON reader — which reads every number as
OpenTimelineIO 0.17.0's parser does, `NaN` and the infinities among them,
and refuses as syntax one it refuses (`0e309`, `2e308`) or one
OpenTimelineIO's builds read as different values — then OpenTimelineIO's
schema shape — exactly the keys the target's readers require (audited
against each schema's `read_from`, per target: `Track.1` needs its `kind`,
a `Marker.2` its `marked_range`, and a reader before 0.15 a `Clip.1`'s
`media_reference`), the type each reader reads every member as, media
references, the target's clip schema, every marker a `Marker.2`, every
transition between two items. It walks every dictionary, metadata
included: OpenTimelineIO decodes one carrying `OTIO_SCHEMA` as an object
of that schema wherever it sits, so the check holds it to that schema's
rules, and refuses by name a schema whose rules it does not know; an
object's `OTIO_REF_ID` must be a string no other object carries. No
Python and no OpenTimelineIO install; CI runs it on the goldens.

### Reading back

`otio::from_otio(&str) -> Result<Timeline, otio::Unreadable>` is the exact
inverse of `to_otio`: for every timeline `to_otio` writes, for either
target, it reads that very timeline back — every count, timebase and rate
as the model held it. It holds each `RationalTime` to the exact words: its
rate the `f64` of its exact ruler, bit for bit, and its value a whole count
within ±2^53. Every number is read as OpenTimelineIO 0.17.0's parser reads
it — RapidJSON at the commit the release pins, in its normal precision, the
readings of a build that fuses one of its multiply-adds and of one that
does not alike, or refused — and held to the model exactly: a count as the
whole number its double is, a rate as that double, a clip's gain as the
`f32` whose shortest spelling reads back as the same double. So
`24.0000000000000001` reads as 24, as OpenTimelineIO reads it; a gain of
`16777217` dB, which no `f32` holds, is refused; and so, by name, is a
count of NaN, of an infinity or of `-0.0`, whose sign no whole count
keeps. What OpenTimelineIO derives from those
counts in its own `f64` arithmetic is held as the export holds it: both
ends of every range — a marker one tick long at 2^53, whose end
OpenTimelineIO's sum rounds back to 2^53, is refused — and every place,
visible range and duration of each track's walk and the stack's. It lays
each track's items end to end from zero, as OpenTimelineIO places them,
and validates what it read. A track ends with its last clip, so a gap of
some length after it — which OpenTimelineIO counts in the track's
duration — is refused.

A document from another writer is read where it uses only what the
timeline models and carries those words. Nothing is dropped: everything
else is refused by name, with where it sits — a schema the timeline does
not model there (`Unreadable::Schema`: an effect, a time warp, a nested
stack, a reference other than an `ExternalReference.1`, a marker on a gap),
a key, value or note it has no word for (`Unreadable::Feature`: another
writer's metadata on an object that keeps no reader's namespace, a value
other than a string in a reader's namespace, a track kind or transition
type it does not know, a
trim on a track, a marker color outside the eleven, a gap of some length
after a track's last clip, …), and a count or rate that does not land
exactly, or a time OpenTimelineIO would read rounded
(`Unreadable::Inexact`). A member OpenTimelineIO lets a document leave out
reads as OpenTimelineIO's own reader reads it — an empty name, `enabled`,
a GREEN marker — or, where that is no value and the timeline needs one
(`global_start_time`, a clip's `source_range`), or a value the timeline
does not keep (a gap's `source_range`, whose default is nothing at rate 1,
a rate its track's duration would take), is refused by name. A member is
read only where the timeline keeps it, or where OpenTimelineIO holds it as
it holds its absence: a `color` on an item and an `enabled` on a
transition, which OpenTimelineIO 0.17.0 keeps as fields of their own, are
refused by name, and a marker's color is one of the eleven names exactly,
in capitals. OpenTimelineIO 0.18 writes a `color` on every item — its item
color, `null` where unset — so a document 0.18 writes is refused by name
until the item color is modelled. Last, the export writes the timeline read, and every value it
writes must be the one the document states at the same path, as
OpenTimelineIO holds it — a string byte for byte, a time's numbers as
doubles, two doubles bit for bit — or the read is refused as inexact there,
naming both: the reader takes exactly the documents the export reproduces.

## The stored document

With the `serde` feature, a timeline is a document whose first field is
`schema` (`1`).
A reader refuses by name an unknown schema, a field it does not know — inside
a time value too, a record's or a rate's — a required field left out (a
clip's `id` among them), a metadata key or a reader's namespace named twice,
a gain that is not finite and a range longer than `i64::MAX` ticks. Time
values are written under `mediatime`'s own field names
and read through the crate's strict shapes of them. An optional word left
out reads as absent. A track's and a clip's `markers` are written only
where there are some, so a timeline without markers writes the document
0.2.0 wrote, and a reader before markers refuses one with them by name. So
are the timeline's, a track's, a clip's and a marker's `readers`: a
timeline without them writes the document 0.3.0 wrote.

## Time

The crate does no time arithmetic of its own: every comparison, sum,
measure and recount is `mediatime`'s, exact across timebases unless it
names a rounding. A range's exact length is `TimeRange::span` — and one
longer than `i64::MAX` ticks is refused — the coarsest whole-rate ruler
holding a range is `TimeRange::coarsest_whole_rate`, and an exact number
of seconds counted at a rate, as an exact fraction, is
`Rate::checked_count`. The export also computes OpenTimelineIO's own
floating-point arithmetic, operation for operation, to find a count
OpenTimelineIO would round: that arithmetic is OpenTimelineIO's, not the
model's.

## Not here

- **Time-warp.** `speed` is reserved for a later schema, and a schema 1
  reader refuses it by name.
- **Rendering.** The compositor and the contact sheet are `mediaio-render`'s,
  the mixer `mediaencode`'s.
- **The container's timecode** in available ranges — a `mediadecode` row.

## Example

```rust
use core::num::NonZeroI32;

use mediaio_timeline::{
  Clip, ClipId, ClipPair, Duration, Fade, FadeShape, Fades, MediaRef, Rate, Refusal, TimeRange,
  Timebase, Timeline, Timestamp, Track, TrackKind, Transition, TransitionAt,
  otio::{OtioTarget, from_otio, to_otio, validate_json},
  validate,
};
use smol_bytes::format_utf8bytes;

let edit = Timebase::new(1, NonZeroI32::new(25).unwrap());
let frames = |n| Duration::new(n, edit);
let shot = |name: &str, start: i64| {
  Clip::new(
    ClipId::new(format_utf8bytes!("{name}-1")),
    name,
    MediaRef::new(format_utf8bytes!("file:///media/{name}.mov"))
      .with_available_range(Some(TimeRange::new(0, 250, edit))),
    TimeRange::new(50, 100, edit),
    TimeRange::new(start, start + 50, edit),
  )
};

// Two shots with a ten-frame dissolve between them; the second fades out.
let timeline = Timeline::new("cut", Rate::FPS_25).with_track(
  Track::new(TrackKind::Video, "V1")
    .with_clip(shot("a", 0))
    .with_clip(shot("b", 50).with_fades(
      Fades::new().with_out(Some(Fade::new(frames(10), FadeShape::Linear))),
    ))
    .with_transition(Transition::dissolve(
      Timestamp::new(50, edit),
      frames(5),
      frames(5),
    )),
);
assert_eq!(validate(&timeline), Ok(()));

let otio = to_otio(&timeline, OtioTarget::V0_15Plus).unwrap();
assert_eq!(validate_json(&otio, OtioTarget::V0_15Plus), Ok(()));
// And read back, exactly.
assert_eq!(from_otio(&otio), Ok(timeline.clone()));

// Pulled ten frames early, `b` overlaps `a` and leaves the dissolve off
// any cut: both refused, by name.
let mut early = timeline.clone();
early.tracks_mut()[0].clips_mut()[1].set_record(TimeRange::new(40, 90, edit));
assert_eq!(
  validate(&early),
  Err(vec![
    Refusal::Overlap(ClipPair::new(0, 0, 1)),
    Refusal::OffBoundary(TransitionAt::new(0, 0)),
  ])
);
```

## Features

| feature | default | what it gives |
|---|---|---|
| `std` | yes | what `alloc` gives, on the `std` builds of `smol-bytes`, `mediatime` and `serde`; the crate itself uses nothing of `std` |
| `alloc` | | the model, `validate`, `layout`, `diff` and `otio`, on `no_std` + `alloc` |
| `serde` | | the stored document: `Serialize` and `Deserialize` for `Timeline` and every type in it, with `std` or `alloc` |

The crate is `#![no_std]` in every tier. With neither `std` nor `alloc` it
builds on `core` alone and exports only the time words it re-exports from
`mediatime` (`Duration`, `Rate`, `TimeRange`, `Timebase`, `Timestamp`):
every model type owns its names, lists and maps, so all of the model needs
`alloc`, and `serde` adds nothing there. CI builds the `no_std` tiers for
`thumbv7em-none-eabihf`, a target with no operating system, and runs clippy
and the tests on every combination of the three features.

Without `std`:

```toml
mediaio-timeline = { version = "0.4", default-features = false, features = ["alloc"] }
```

## Requirements

- Rust ≥ **1.95**, edition 2024.
- `mediatime` 0.5.1 or later.
- Nothing links FFmpeg.

## License

`mediaio-timeline` is under the terms of both the MIT license and the
Apache License (Version 2.0).

See [LICENSE-APACHE](https://github.com/findit-studio/mediadecode/blob/main/LICENSE-APACHE),
[LICENSE-MIT](https://github.com/findit-studio/mediadecode/blob/main/LICENSE-MIT)
for details.

Copyright (c) 2026 FinDIT Studio authors.

[Github-url]: https://github.com/findit-studio/mediadecode
[CI-url]: https://github.com/findit-studio/mediadecode/actions/workflows/ci-core.yml
[doc-url]: https://docs.rs/mediaio-timeline
[crates-url]: https://crates.io/crates/mediaio-timeline
