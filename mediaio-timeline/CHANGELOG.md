# Changelog

All notable changes to the [`mediaio-timeline`](https://crates.io/crates/mediaio-timeline)
crate are documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this crate adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
Its versions are its own: they do not move with `mediadecode`'s.

## [0.4.0] - unreleased

### Added

- **Other readers' own metadata: `ReaderMetadata`**, on the timeline, a
  track, a clip and a marker (`readers`, `readers_mut`, `with_readers`).
  Under each reader's namespace — `Resolve_OTIO`, say — it holds a
  `Metadata` of that reader's words, which `to_otio` writes in the object's
  OpenTimelineIO `metadata` beside the crate's own `mediaio` — whole, `{}`
  for a namespace with no entry, every key in byte order — and `from_otio`
  reads back exactly. DaVinci Resolve 21.1 reads a marker's note from
  `Resolve_OTIO`'s `Note` alone, not from `comment`
  (findit-studio/application#265); the crate interprets no namespace. A
  namespace holds strings alone: `from_otio` refuses any other value inside
  one by name (`Feature::NamespaceValue`). `validate` refuses a namespace
  named `mediaio` or `OTIO_SCHEMA` (`Refusal::ReservedNamespace`) and a key
  `OTIO_SCHEMA` inside one (`Refusal::SchemaKeyInNamespace`), which
  OpenTimelineIO reads as a schema word, each at a `NamespaceAt` of a
  `MetadataAt`. An object with no namespace writes the document 0.3.0
  wrote, and with `serde` the `readers` are written only where there are
  some.

### Changed

- **`from_otio` reads a reader's namespace** where 0.3.0 refused it: a key
  of the timeline's, a track's, a clip's or a marker's `metadata` beside
  `mediaio` whose value is an object. It still refuses such a key by name
  (`Feature::Metadata`) on every other object, and where its value is no
  object.

- **BREAKING: `MisreadNumber::Count` is gone.** `to_otio` writes every
  count within ±2^53 in a spelling OpenTimelineIO 0.17's reader reads
  exactly: `<digits>.0` up to ⌊2^54 / 10⌋ as before, and past it the
  integer alone, which RapidJSON hands over whole and a `double` member
  takes exactly — as every reader from 0.12 takes one. No count is refused
  as misread any more; 0.3.0 refused one past ⌊2^54 / 10⌋ whose
  `<digits>.0` the reader read as another.
- **`to_otio` writes a rate whose shortest spelling OpenTimelineIO 0.17
  misreads in another spelling of the same `f64`** that a build fusing one
  of RapidJSON's multiply-adds and one that does not both read exactly: an
  integer an `f64` holds over a power of ten, which the reader converts
  exactly and divides once (`95904095904095904e-15` for 96000/1001), or,
  where none is, the same with zeros the reader multiplies in, each rounded
  once with nothing added. 0.3.0 refused every such rate — about 13 % of
  the rates num/1001. Of the rates num/1001 up to 200 000/1001, three have
  no such spelling, nor any of the 15 626 tried around each, and are still
  refused by name: 29195/1001, 31264/1001 and 31799/1001. Every common rate
  keeps its shortest spelling, so the goldens are unchanged.
- **BREAKING: `validate_json` for `OtioTarget::V0_15Plus` holds three
  members of the newer readers to their types**: an item's `color` to a
  `Color.1` or `null`, as OpenTimelineIO 0.18 reads an optional `Color`
  (0.18.1 `item.cpp` 181); an effect's `enabled` to a boolean, as 0.18 reads
  it (`effect.cpp` 26); and a transition's `enabled` to a boolean, as
  `main` reads it (`transition.cpp` 38 at `00c22fa`). A boolean member's
  reader takes no `null`. 0.3.0 walked all three untyped, so it passed a
  document those readers refuse. `OtioTarget::Legacy` still walks them
  untyped: its readers keep them as keys they do not know.

## [0.3.0] - 2026-10-22

### Changed

- **BREAKING: the OpenTimelineIO export now carries the exact-readback
  metadata.** Beside OpenTimelineIO's fields, every document `to_otio`
  writes carries under `metadata.mediaio` the words `from_otio` reads the
  timeline back by: the edit rate (`rate`), each media-side range's exact
  ruler and timebase (`source`, `available`, `range`), a dissolve's place
  among its track's transitions where the track keeps them out of their
  cuts' order (`order`), and the timebase of an edit-side value spelled
  otherwise than the edit rate's reciprocal (`start`, `record`,
  `timebase`, `at`, `in_offset`, `out_offset`). They are always written,
  as the read-back needs them: no document is 0.2.0's byte for byte, with
  markers or without. OpenTimelineIO's readers keep them as metadata.
- **BREAKING: the model's notes are an array of `[key, value]` pairs**
  under `metadata.mediaio.metadata`, never an object: a note's key is
  anything, and OpenTimelineIO 0.17 reads an object carrying `OTIO_SCHEMA`
  as an object of that schema, so a note keyed so made the export a
  document it refuses.
- **BREAKING: `to_otio` refuses a count or a rate OpenTimelineIO 0.17's
  reader would read as another value.** OpenTimelineIO 0.17.0 reads
  numbers with RapidJSON in its normal precision, which does not always
  round to the nearest double: a count written `<digits>.0` past
  1 801 439 850 948 198 (⌊2^54 / 10⌋) can read as another —
  `9007199254740991.0` as 9007199254740990 — and so can the shortest
  spelling of some rates' `f64`s — 96000/1001's — while every common rate
  reads exactly. The export reads every count and rate it writes as both
  kinds of build read it — one that fuses a multiply-add of RapidJSON's
  and one that does not — and refuses one either misreads, naming the
  place and both values (`NotRepresentable::misread`). 0.2.0 wrote them.
- **BREAKING: `validate_json` reads every number as OpenTimelineIO 0.17's
  parser does** — RapidJSON at the commit the release pins, step for step:
  `NaN`, `Inf` and `Infinity` are numbers, read where a reader reads a
  number; `0e309`, `2e308` and an integer of 400 digits, which RapidJSON
  refuses, refuse the document as syntax, and so does a number
  OpenTimelineIO's builds read as different values; an `int64_t` member
  takes every integer RapidJSON reads as one — `-0`, and up to 2^64 − 1,
  which OpenTimelineIO keeps cut to its low 63 bits — and nothing written
  with a fraction or an exponent.

### Added

- **Markers.** `Marker` — a name, the range it marks, a `MarkerColor`
  (OpenTimelineIO's eleven, green by default) and a comment — on tracks
  (`Track::markers`, at the edit rate from the timeline's zero) and clips
  (`Clip::markers`, in the medium's time), with `markers_mut` and
  `with_marker`. `validate` refuses a track's marker off the edit rate
  (`OffEditRate` with the new `Place::Marker`), a clip's in a degenerate
  timebase (`MarkerRateUnstated`) and either longer than `i64::MAX` ticks
  (`MarkerTooLong`), naming it by `MarkerAt`. The stored document writes
  markers only where there are some: a timeline without them writes 0.2.0's
  document byte for byte.
- **`otio::from_otio`, the exact inverse of `to_otio`**: for every timeline
  `to_otio` writes, for either target, it reads that very timeline back,
  every count, timebase and rate as the model held it. What the timeline
  does not model is refused by name with where it sits — `Unreadable`, with
  `UnmodelledSchema`, `UnmodelledFeature` and its `Feature`, `Inexact`, and
  the self-check's `Shape` and `Syntax` — never dropped: a gap of some
  length after a track's last clip, which OpenTimelineIO counts in the
  track's duration, among them (`Feature::TrailingGap`), and a gap of no
  length anywhere but where the export writes one, on a fade's gap side,
  which OpenTimelineIO counts all the same (`Feature::StandaloneGap`).
  Every number is
  read as OpenTimelineIO 0.17.0's parser reads it — RapidJSON in its
  normal precision, the readings of a build that fuses one of its
  multiply-adds and of one that does not alike, or refused — and held to
  the model exactly: a count of NaN, of an infinity or of `-0.0` is
  refused by name. What OpenTimelineIO derives from the document in its own
  `f64` arithmetic — both ends of every range, and each track's walk and
  the stack's — is held as `to_otio` holds it; each is refused as
  `Inexact` where it rounds. A member OpenTimelineIO lets a document leave
  out reads as OpenTimelineIO 0.17.0's reader reads it — an empty name, a
  GREEN marker — or is refused by name where that is no value and the
  timeline needs one, or a value it does not keep: a gap's absent range,
  OpenTimelineIO's `TimeRange()`, is nothing at rate 1, a rate the track's
  duration takes and the timeline loses, so a gap states its range at the
  edit rate, as the export writes it. Last, the export writes the timeline
  read, and every value it writes must be the one the document states at
  the same path, as OpenTimelineIO 0.17.0's reader holds it — strings byte
  for byte, booleans exactly, a `RationalTime`'s numbers as doubles, a
  dictionary's as the integers or doubles RapidJSON reads them as, two
  doubles bit for bit, so `-0.0` is not `0.0` — or the read
  is refused as `Inexact` there, naming both values (`Inexact::written`,
  `Inexact::stated`): a medium's range counted in a ruler the export would
  not choose, a marker's color in another case, among them. A member may
  be left out only where OpenTimelineIO reads its absence as the value the
  export writes, and stated unwritten only where OpenTimelineIO holds it as
  its absence; each is a row of one of two tables, cited by source. The
  reader takes exactly the documents the export reproduces. A marker's
  color is one of the eleven names exactly, in capitals; a `color` on an
  item and an `enabled` on a transition, which OpenTimelineIO 0.17.0 keeps
  as dynamic fields, are refused by name, `null` and `true` among them. The
  item `color` is OpenTimelineIO 0.18's item color, which 0.18 writes on
  every item: a document 0.18 writes is refused by name until it is
  modelled.
- `otio::NotRepresentable::misread`, with `otio::Misread` and
  `otio::MisreadNumber`: the count or rate OpenTimelineIO 0.17's reader
  would read as another value, what was meant and what each kind of build
  reads.
- The export writes markers as `Marker.2`, refusing one no ruler holds
  whole as `Spot::Marker`.
- `validate_json` holds every marker to `Marker.2`, with the
  `marked_range` every reader requires.
- `validate_json` walks every dictionary, metadata included, as
  OpenTimelineIO's readers decode them: one carrying `OTIO_SCHEMA` —
  in metadata, under a key no reader reads, or nested in either — is held
  to that schema's rules, and a schema whose rules the check does not know
  is refused by name. Every schema it accepts is held to its 0.17.0
  reader's required and optional members and their types — a generator's
  kind and parameters, an image sequence's eight members, an effect's
  name and a time warp's scalar among them. Metadata, a clip's media
  references and a generator's parameters must each be a plain
  dictionary; every effect one of the four effect schemas; every object's
  `OTIO_REF_ID` a string, and a nonempty one unique in the document; and,
  for `V0_15Plus`, a media reference's image bounds a `Box2d.1`.

## [0.2.0] - 2026-10-15

### Changed

- **BREAKING: text is `smol_bytes::Utf8Bytes`, not `String`, in every
  public signature.** `Timeline::new`, `Track::new`, `Clip::new`,
  `ClipId::new` and `MediaRef::new`, and the `set_name` and `set_locator`
  setters, take `impl Into<Utf8Bytes>` — a `&str` or a `String` still
  converts; `MediaRef::with_reel` and `MediaRef::set_reel` take an
  `Option<Utf8Bytes>`; `Metadata::insert`, `Metadata::with` and
  `Metadata`'s `FromIterator` take `impl Into<Utf8Bytes>` keys and values,
  and `Metadata::insert` and `Metadata::remove` return the
  `Option<Utf8Bytes>` they displaced; `otio::to_otio` returns the document
  as a `Utf8Bytes`. The getters still lend a `&str`, but `Timeline::name`,
  `Track::name`, `Clip::name`, `ClipId::as_str`, `MediaRef::locator` and
  `MediaRef::reel` are no longer `const fn`.
- **BREAKING: `serde` is an opt-in feature.** Without it the model has no
  `Serialize` or `Deserialize`. With it the document is 0.1.1's, byte for
  byte: the same timeline writes the same JSON, and each version reads the
  other's documents.
- **BREAKING: the crate has features** — `std` (the default), `alloc` and
  `serde` — where 0.1 had none and was always `no_std` + `alloc`. It is
  `#![no_std]` in every tier: `default-features = false, features =
  ["alloc"]` is 0.1's build, and on `core` alone (neither `std` nor
  `alloc`) it exports only the time words it re-exports from `mediatime`.
  `std` gives what `alloc` gives, on the `std` builds of its dependencies.

### Added

- `Display` and `IsVariant` (`is_*` predicates), through `derive_more`, on
  `FadeShape` (`linear`, `equal_power`), `TrackKind` (`video`, `audio`),
  `ChangeKind` (`added`, `removed`, `moved`, `retimed`, `regained`,
  `enabled_flipped`, `renamed`, `relinked`) and `Edge` (`in`, `out`).
- `Gain: PartialOrd`; `Gain` is `#[repr(transparent)]` over its `f32`.
- `Timeline::tracks_mut_slice`, `Timeline::with_tracks` and
  `Timeline::set_tracks`; `Delta::changes` and `Delta::is_empty` are
  `const fn`.

## [0.1.1] - 2026-10-10

### Changed

- **Time is measured and counted by `mediatime` 0.5.1**, which this version
  requires. A range's length is `TimeRange::span`, and a range longer than
  `i64::MAX` ticks is refused by name as before; the coarsest whole-rate
  ruler a media-side range lands on is `TimeRange::coarsest_whole_rate`;
  and the exact count of a time in a ruler, which the export's walk holds
  every derived value by, is `Rate::checked_count`. The crate's own copies
  of the three, and its greatest-common-divisor routines, are gone.

  Every document, refusal and export is the one 0.1.0 gives, but at one
  edge: a count is formed in lowest terms whatever the spelling of its
  ruler's rate, so over a rate written in other than lowest terms
  (`60000/2002`) a count whose halves 0.1.0 formed past `i128`, and so
  refused, can be formed and held.

## [0.1.0] - 2026-10-10

The crate's first release, as 0.1.0.

### Added

- **The editing timeline as data, as a schema 1 document.** `Timeline`
  (`name`, the edit `rate`, the timecode `start` its zero carries,
  `metadata`, `tracks`), `Track` (`kind`: `Video` or `Audio`, `name`,
  `enabled`, `clips`, `transitions`), `Clip` (its `id`, `name`, `media`,
  `source_range` in the medium's timebase, `record` at the edit rate,
  `enabled`, `gain`, `fades`, `metadata`), `ClipId` (a clip's identity: a
  string its creator mints once per placement, unique in the timeline;
  names and media may repeat), `MediaRef` (`locator`, and when known
  `available_range`, `rate`, `reel`), `Transition` (a `Dissolve` at a cut,
  with OpenTimelineIO's `in_offset` and `out_offset`), `Fades` and `Fade`
  (`Linear` or `EqualPower`), `Gain` (finite decibels) and `Metadata` (an
  ordered string map). Pure Rust on `mediatime` 0.5 and `serde`; `no_std` +
  `alloc`; no FFmpeg.

  Positions are explicit: a clip's record is stored, never derived from the
  clips before it, so trimming one clip moves no other. With no time-warp in
  schema 1 a source range runs a whole number of edit-rate ticks and its
  record exactly as long; `speed` is reserved for a later schema.

  On the wire the document's first field is `schema`; an unknown schema, a
  field this reader does not know (inside a time value too), a required
  field left out (a clip's `id` among them), a metadata key named twice, a
  gain that is not finite and a range longer than `i64::MAX` ticks are
  refused by name. Time values are written under `mediatime`'s field names
  and read through the crate's own strict shapes of them.

- **`validate`**, refusing by name: `RateUnstated`, `OffEditRate`,
  `MediaRateUnstated`, `RangeTooLong`, `RecordBeforeZero`, `EmptyRecord`,
  `OutOfOrder`, `Overlap`, `EmptyClipId`, `DuplicateClipId`,
  `SourceOffEditRate`, `DurationMismatch`, `OutsideAvailable`,
  `OffBoundary`, `DuplicateTransition`, `HandleMissing`,
  `FadeMeetsTransition` and `BlendsOverrunClip`, each naming where
  (`ClipAt`, `ClipPair`, `IdClash`, `RangeAt` with `ClipRange`, `EdgeAt`,
  `TransitionAt`, `Place`, `Mismatch`). All at once, in a fixed order.

- **`layout`**: a valid timeline's clips at their records and the gaps
  between them, derived for export.

- **`diff`**: per track in record order, each clip `Added`, `Removed`,
  `Moved`, `Retimed`, `Regained`, `EnabledFlipped`, `Renamed` or
  `Relinked`, a clip matched by its id alone — a side carrying one id on two
  clips is refused as `Ambiguous` (its `Side` and `IdClash`) rather than
  matched by occurrence; `diff(a, a)` is empty.

- **`otio::to_otio`**: OpenTimelineIO JSON for `OtioTarget::V0_15Plus`
  (`Clip.2`) or `OtioTarget::Legacy` (`Clip.1`) — `global_start_time`
  always written, gaps from the layout, a clip's source range whole in its
  medium's one ruler, a dissolve as `SMPTE_Dissolve`, a fade as a dissolve
  against a gap, a clip's id, gain and reel in `metadata` (DaVinci Resolve
  applies neither gain nor reel). Every count written lies within ±2^53,
  where OpenTimelineIO's `f64` holds every whole number; a media-side range
  its own rulers count past that is written in the coarsest whole-rate
  ruler that holds it, and of its own rulers in the first that ends it
  within ±2^53 too — a source range none ends is written in one that
  writes its start and its length, its end left to the walk and the
  search; a source range none of them writes, in the first ruler the
  timeline's operands are counted in that does, finest first; and an
  available range none of them ends, in the first of those rulers that
  ends it too, its end held as it is planned (each one at a fractional
  rate: every whole rate that lands on a range is a multiple of the
  coarsest). Every count OpenTimelineIO derives from the document —
  the end of each clip's source range; each child's place on its track, from zero
  in the child's own rate, and in the timeline; each item's visible range
  with its neighbouring transitions' handles; each track's duration and the
  stack's, the longest as OpenTimelineIO picks it; the global start added
  to each place and each track's end; each child's range from the global
  start, the moved start and the child's own duration, as OpenTimelineIO
  moves a range into its parent's; a timeline with no track's range from
  the global start, which OpenTimelineIO gives no duration at rate 1; the
  last tick of every one of those ranges, `end_time_inclusive`, branch for
  branch — is computed in OpenTimelineIO's own arithmetic beside its exact
  value, and held within ±2^53 in the ruler OpenTimelineIO carries it in,
  its double less than half a tick from the exact count. Exact, or
  refused: where a derived value would round, a
  search bounded by contract writes the clips it is formed from in other
  rulers that hold their source ranges — a clip's plan's, then every ruler
  the timeline's operands are counted in (the edit rate, 1, every clip's
  planned ruler), never capped, then the 64 finest and the 64 coarsest
  whole rates below its plan's — one clip one ruler at a time, the finest
  first, within `1 + n · (d + 127)` walks for `n` clips and `d` operand
  rulers. Every range ends where it is stored, and the exported items, laid
  end to end, put every clip on its record. Refuses with `otio::Refused`:
  `Validation` with `validate`'s refusals, or `NotRepresentable` — an
  `otio::Spot` (`otio::ChildAt` for a child of an exported track,
  `TimelineEnd` for the end of a timeline with no track), the count and
  the rate it is counted at, and for a count OpenTimelineIO derives, a
  source range's end among them, what the search tried,
  `otio::RulerSearch`: its bands (`otio::RulerBand`) and its walks — where
  no plan the search tries holds a count, naming the last plan's. A count
  written at the edit rate is refused before any walk, naming no search; a
  source range no ruler of the bands writes, and an available range none
  of them holds whole — none of its own, none the timeline's operands are
  counted in — are refused before any walk too, naming the bands and no
  walk. A timeline only a ruler outside the bands would hold is refused by
  that contract, not misread.

- **`otio::validate_json`**: the structural self-check — a strict JSON
  reader and OpenTimelineIO's schema shape, requiring exactly the keys each
  target's readers require — with no Python dependency.
