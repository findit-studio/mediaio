# Changelog

All notable changes to the [`mediadecode`](https://crates.io/crates/mediadecode)
crate are documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this crate adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

The sibling FFmpeg adapter has its own log at
[`mediadecode-ffmpeg/CHANGELOG.md`](../mediadecode-ffmpeg/CHANGELOG.md).

## [Unreleased]

## [0.19.1] - unreleased

### Added

- **`nal`**, re-exported from `mediaspine` 0.1.1 beside the rest of its
  vocabulary: `NalStream` reads whether an H.264 or HEVC packet's NAL
  units prove that nothing references its picture, for
  `PacketFlags::DISPOSABLE` in any container.

### Changed

- **`PacketFlags::DISPOSABLE` is the container's mark or the
  bitstream's**, and the bitstream's verdict decides wherever a demuxer
  reads a track. Its contract is unchanged — absent means unknown — but a
  demuxer that reads the bitstream, as `mediadecode-ffmpeg`'s now does,
  marks more H.264 and HEVC packets than the container does, so a
  consumer that drops them decodes fewer pictures. Its promise is pixels:
  no later picture's pixels change, and nothing is promised of a later
  frame's best-effort timestamp; a frame's own pts is the exact one.

## [0.19.0] - 2026-10-16

The vocabulary moves to its own crate,
[`mediaspine`](../mediaspine/CHANGELOG.md), and this crate keeps the
read faces. The family releases 0.19.0 in lockstep with the new write
faces (`mediaencode`), the FFmpeg base (`mediaio-ffmpeg`), the FFmpeg
write adapter (`mediaencode-ffmpeg`) and the umbrella (`mediaio`).

### Changed (BREAKING)

- **`Received::Frame` is `Received::Produced`.** The answer that
  delivers output is the one an encoder answers with a packet, so it is
  named for what every push session does: a decoder or a resampler
  produces a frame, an encoder a packet. The derived `is_frame` is
  `is_produced`. A `match` on the old name no longer compiles, which is
  how every site is found.
- **The vocabulary is `mediaspine`'s.** `adapter`, `cfa`, `color`,
  `frame`, `packet`, `pixel_format`, `resampler`, `rhythm` and
  `subtitle`, and the demuxer's types, live in `mediaspine` and are
  re-exported here under the same paths. `mediadecode::frame::VideoFrame`
  *is* `mediaspine::frame::VideoFrame`, so code that mixes the two paths
  type-checks. The `Demuxer` trait stays here, beside a re-export of the
  demux types, and `mediaspine` itself is re-exported as
  `mediadecode::mediaspine`.
- **Every feature is forwarded.** `alloc`, `std`, `serde`, `arbitrary`
  and `quickcheck` enable the same features of `mediaspine`, where the
  types they apply to now live; `future` stays this crate's.

## [0.18.0] - 2026-10-10

### Added

- **`PacketFlags::DISPOSABLE` (`0b1_0000`)**: the demuxer states that
  nothing references this packet (FFmpeg `AV_PKT_FLAG_DISPOSABLE`).
  Dropping such a packet loses its own picture and no other, so a consumer
  that samples pictures can skip the B pictures nothing references. In
  libavformat 9.0.1 one demuxer sets it: MP4/MOV, from `sdtp`
  (`libavformat/mov.c` 11719–11723). Matroska's discardable bit is NOT
  mapped (`libavformat/matroskadec.c` 4319–4320 reads the keyframe bit
  alone), so a Matroska packet reads absent. The bit's ABSENCE means
  unknown, never "referenced": that is the rule a consumer must honour.

### Changed (BREAKING)

- **`PacketFlags` names a fourth bit.** No packet's serialized number moves:
  `0b1_0000` is the bit FFmpeg's demuxer already set, and the set already
  carried it as an unnamed bit (`from_bits_retain` on both serde legs), so a
  stored 0.17 number reads back the same set, the bit now named. What changes
  is everything that reads the named set: `PacketFlags::all()` is
  `0b1_0111` (was `0b111`), `complement()` / `!` and the strict
  `from_bits` include the bit, `from_bits_truncate` keeps it, and `Debug`
  prints `DISPOSABLE` where it printed `0x10`.

## [0.17.0] - 2026-10-10

Both public dependencies cross a breaking minor at once: `mediatime`
0.4 → 0.5 and `mediaframe` 0.11 → 0.12, so a consumer holding a
`mediatime 0.4` or `mediaframe 0.11` value no longer type-checks against
this release. The version skips 0.16 so that the workspace's three crates
release in lockstep again: `mediadecode-ffmpeg` 0.16.0 was released
alone. The upstream notes are the authority
([mediatime](https://github.com/findit-studio/mediatime/blob/main/CHANGELOG.md),
[mediaframe](https://github.com/findit-studio/mediaframe/blob/main/CHANGELOG.md));
what follows is only what changes here.

### Added

- **`InvertedRange`, re-exported beside `Timebase`, `Timestamp` and
  `TimeRange`.** In `mediatime` 0.5 a range moves one end only by a
  checked method, which refuses an end before the start by this name. A
  consumer that moves a range's ends through this crate's re-exports can
  name the refusal without a `mediatime` dependency of its own.
- **`mediadecode::mediatime`: the whole of `mediatime`, re-exported
  beside the four flat names.** The re-exported primitives' own methods
  take or answer types the flat re-exports do not name: a directed
  rescale (`checked_rescale_with`) takes a `Rounding`, which inference
  cannot supply; `Timestamp::parse_seconds` refuses with a
  `ParseSecondsError`; `TimeRange::span` answers a `Duration`, and
  `coarsest_whole_rate` a `Rate`, whose `checked_count` takes
  `ExactSeconds`; `Timestamp`'s signed arithmetic takes and answers
  `SignedDuration`. Each now has a path through this crate, as will
  whatever `mediatime` adds; the four flat re-exports are unchanged.

### Changed (BREAKING)

- **`mediatime` 0.4 → 0.5**, at the same pin as before
  (`default-features = false`), resolving to 0.5.1. `Timebase`,
  `Timestamp` and `TimeRange` are this crate's own public surface, the
  reasoning of the 0.11.0 crossing. What reaches a consumer through the
  re-exports:

  - `TimeRange::{with_start, with_end, set_start, set_end}` are removed:
    they assigned without checking, so a safe call could build a range
    whose end precedes its start. `try_with_start` / `try_with_end` /
    `try_set_start` / `try_set_end` answer `Err(InvertedRange)` and leave
    the range as it was; `with_bounds` / `set_bounds` move both ends at
    once and panic on an inverted pair, as `TimeRange::new` does.
  - The three types no longer implement buffa's `Message`. This crate
    never enables `mediatime/buffa`; a consumer that does maps
    `.mediatime.v1` onto `mediatime::wire` and converts at the edge.
  - The rest is additive: `TimeRange`'s comparison algebra, `span` and
    `coarsest_whole_rate`, `Timestamp::parse_seconds`, and the directed
    and exact rescales. The types those take or answer (`Rounding`,
    `ParseSecondsError`, `Duration`, `Rate`) are reached through
    `mediadecode::mediatime` (above). The serde shapes, and the message
    an inverted range is refused with, are unchanged.

- **`mediaframe` 0.11 → 0.12**, at the same pin as before
  (`default-features = false`, `features = ["frame"]`). Upstream 0.12.0
  is the `mediatime` crossing above and nothing else: its source is
  0.11.0's but for comments. The color, pixel-format and frame
  vocabulary this crate re-exports is `mediaframe`'s, so the crossing is
  Breaking here as well. With both crossed the graph holds one
  `mediatime`, and a `mediaframe::frame::TimestampedFrame` takes this
  crate's `Timestamp` as it is.

No source line moved but the re-exports: the workspace compiles clean on
the new pins, both adapters included. `tests/time_primitives.rs` holds
the three laws: a move that would invert a range is refused by a name
this crate exports; every type the primitives' own methods take or
answer has a path through this crate; and this crate's `Timestamp` is
the one `mediaframe`'s frames carry.

### Changed

- `adapter`'s send-direction rule now says what a decoder that hands
  back what it is sent owes: copies of its own, so the scoped send
  stays zero-copy on every road. It said the body is copied wherever a
  history is being recorded, which `mediadecode-ffmpeg` 0.16.0 no
  longer does: its hardware probe records copies.

## [0.15.1] - 2026-10-05

### Added

- **The container-level duration crosses the demux seam, with the
  library's account of it: a *provided* `Demuxer::duration` answering
  `Option<ContainerDuration>`.**

  The figures that bear on a file's duration come in layers, and each
  layer is answered by its own method. Two are lengths — the
  container-level figure and a track's — and one is an endpoint, the
  walk's measured end. This method is the first: the figure the library
  reports for the file as a whole, `ContainerDuration::value`, while
  `TrackInfo::duration` stays the library's figure for one track. A
  track the library reported none for stays `None`: Matroska reports a
  length for the file and none for any track, and the container's
  figure is never copied onto a row by this tier.

  The figure carries how the library says it came by it,
  `ContainerDuration::source`, in libavformat's own three words and no
  others: `DurationSource::FromPts` ("Duration accurately estimated from
  PTSes"), `FromStream` ("Duration estimated from a stream with a known
  duration") and `FromBitrate` ("Duration estimated from bitrate (less
  accurate)"), plus `Unknown` for an account the build cannot name,
  which is never folded into an estimate. None of them says that a
  header declares the figure, and the type does not rank them. The
  figure is a *length*, and the walk's measured end is an endpoint on a
  track's timeline — different quantities that this tier neither ranks
  nor converts. `TrackInfo::duration`'s doc now says it is the
  library's figure from before probing and no claim about the file, and
  points at `measured_end` for what the packets themselves show.

  Provided, answering `None`, so every implementor written before the
  method existed compiles unchanged and answers correctly. Like the two
  tables it is fixed for the session's life and callable at any point.

- **A walk's measured end crosses the demux seam: `demuxer::MeasuredEnd`,
  and a *provided* `Demuxer::measured_end`.**

  `measured_end(track)` answers where a track's packets ended — the
  greatest `pts + duration` over the timed packets the session read on
  it, an endpoint on the track's own timeline — with two independent
  qualifications. `MeasuredEnd::exact` is `false`, for good, from the
  first timed packet read with a duration that is not positive
  (libavformat writes zero for one it does not know) or the first packet
  read with no usable timestamp: the end then advances to that packet's
  `pts` only, or not at all, and is a lower bound. The mark is made when
  the packet is read, so it does not depend on whether the packet is
  then delivered, refused or parked. A measured end promises only that
  it is exact when `exact`, covering what `walk_complete` says.

  It is an endpoint, not a length. The container's duration is a
  length, and a track that starts at ten seconds and runs for two ends
  at twelve against a container duration of two. Turning an end into a
  length means subtracting the track's start and honouring presentation
  edits, which is the composer's job and not this crate's.

  `MeasuredEnd::walk_complete` is `false` for a figure *so far* and
  turns `true` only once every packet the library delivered to the
  session was observed, from the first to end of file, with no skip. A
  complete walk is frozen: its figures are final, and a later read or
  seek neither moves them nor takes the flag back. A walk broken before
  it completes — by a seek, by any packet that was read and not
  observed (refused, skipped as corrupt, or on a stream the session
  never described, which is what a container that adds streams
  mid-read produces), or by an end too large to represent, whose track
  answers `None` — can never complete in the session, and its figure
  keeps rising as a "so far" maximum. A timed packet with no payload is
  observed: it is a real endpoint.

  What a complete walk does not cover is the library's own probing,
  before the session's first read. libavformat buffers and replays the
  packets it reads then, but a read error it swallows is not exposed
  anywhere, so data skipped there is invisible.

  The figure rides the pulls the caller already makes, so nothing is
  read for it, nothing is sought, and there is no second pass. Provided,
  answering `None` for every track, so every implementor written before
  the method existed compiles unchanged.

## [0.15.0] - 2026-09-11

### Added

- **The container's table of contents crosses the demux seam:
  `demuxer::Chapter`, and a *provided* `Demuxer::chapters`.**

  A chapter is a named stretch of the **file's** timeline the container
  itself declares — a DVD-style scene marker, a podcast segment, a
  Matroska `ChapterAtom`. It belongs to no track, so it has no seat on
  `TrackInfo`, and until now the only road to one was whatever escape
  hatch a backend happened to expose.

  `Chapter<E: DemuxAdapter>` carries exactly what a container writes:
  `id` — the **container's** id, not the row's position — a `timebase`
  of the chapter's own, `start` and `end` expressed in it, and an
  optional `title` on the adapter's `Text` carrier. House shape
  throughout: private fields, `new`, `const` getters, `with_title` /
  `set_title`. `Debug` unconditionally; `Clone` where `E::Text: Clone`,
  `PartialEq` where `E::Text: PartialEq` — each hand-written, so the
  bound lands on `E::Text` rather than on a flat `E` that no field
  needs.

  Two shape decisions, both argued in the type's own docs:

  - **`Clone`, unlike `TrackInfo`.** The message-carrier law that keeps
    `Clone` off a track row is about cost — a row carries backend
    metadata down to codec parameters. A chapter is four scalars and a
    title, so there is no deep copy here for the law to forbid, and no
    `TrackHandle`-style carrier seat worth minting to avoid one.
  - **Two instants, not one `TimeRange`.** `TimeRange`'s constructor
    refuses `end < start`, which is precisely the row a container can
    write and this tier has to be able to report. The span is
    reported, never repaired: deciding what an inverted span *means*
    takes a policy a demux tier does not have, and a clamp applied
    here would be indistinguishable downstream from a file that was
    well formed all along.

  **`Demuxer::chapters(&self) -> &[Chapter<Self::Adapter>]` is
  provided**, answering an empty slice — so every implementor written
  before the seat existed compiles unchanged *and* answers correctly:
  most files declare no chapters, and a format that cannot carry them
  has nothing to override. Rows come back by borrow rather than through
  a handle seat, for the same reason `Chapter` is `Clone`. The table is
  read under the track table's rules: held for the session's whole
  life, non-destructive, no ordering rule at all.

## [0.14.0] - 2026-09-02

### Changed (BREAKING)

- **`mediaframe` 0.9 → 0.10**, at the same pin as before
  (`default-features = false`, `features = ["frame"]` — the no-alloc
  tier). A public dependency crossing an incompatible 0.x minor is
  Breaking regardless of how much of its surface actually moved — same
  reasoning as the 0.5.0, 0.6.0, 0.11.0 and 0.7 → 0.9 crossings.

  **No `mediadecode` source line changed.** Upstream 0.10.0 is two
  changes, swept against what this crate names:

  - **A case-sensitivity axis for every vocabulary's parse table** is
    `pub(crate)` — upstream's own note is no public API changes, and
    all 22 `Other(SmolStr)` households (`pixel_format::PixelFormat` and
    the `color` household among them) declare `Insensitive`. Zero
    behaviour change, no site here regardless of visibility.
  - **`Type::other(slug)` now runs the ignore-case `FromStr` lookup
    first, and a genuine stranger's spelling is preserved verbatim in
    `Other` rather than ASCII-folded** — across all 22 households. This
    crate's own `mediaframe` pin is the no-alloc `frame` tier;
    `Other(SmolStr)` lives behind mediaframe's `alloc` feature and is
    not reachable through it (see `pixel_format.rs`'s own note). No
    `.other(...)` call and no `FromStr` onto a `mediaframe` vocabulary
    type appears anywhere in this crate's source.

  The consumed surface, swept item by item, is unchanged in 0.10:
  `color::{ChromaLocation, DynamicRange, Info, Matrix, Primaries,
  Transfer}` (re-exported here under the disambiguated `Color*`
  aliases), `pixel_format::PixelFormat`, and `frame::{BayerPattern,
  Dimensions, Plane, Rect}`. Verified empirically, not just argued:
  `cargo hack clippy / build / test -p mediadecode --feature-powerset
  --exclude-no-default-features` and `cargo fmt --check`, stable
  toolchain, all clean with zero source change.

### Removed (BREAKING)

- **The optional `ingraph` feature is gone, whole** — the feature
  itself, its `dep:ingraph` row, the `mediadecode::ingraph` module it
  gated, and that module's tests. Deleted, not deprecated: this is the
  0.x line, and no alias survives.

  The feature self-granted [`packet::PacketFlags`] citizenship in the
  indexing framework ([#49]), which put the dependency edge backwards —
  a vocabulary crate depending on the framework that reads it, rather
  than the framework depending on the vocabulary crate the way it
  already does for `mediaframe` and `mediatime`. The direction is
  reversed now: `PacketFlags`' citizenship lives behind **`ingraph`'s
  own `mediadecode` feature** instead ([ingraph#524]), carrying six of
  this module's seven rows — `FlagsValue`, `FlagsFilterMarker`,
  `DefaultMarker`, `DefaultVecMarker`, `ColumnKind`, `ColumnEq`. A
  consumer reading, defaulting or comparing a `PacketFlags` column
  turns this crate's `ingraph` feature off and `ingraph`'s own
  `mediadecode` feature on instead — nothing about `PacketFlags` itself
  changed shape.

  **`CursorValue` does not cross, and that is not an oversight.**
  `ingraph`'s bare-seat citizenship deliberately withholds the
  keyset-cursor row: a flags column's cursor domain is the declared bit
  mask, which is a storage-tier precondition the read-side citizenship
  this feature (and its replacement) grants does not carry — the
  module's own note argues this at length, and a dedicated test on that
  side asserts the absence rather than leaving it implicit. A consumer
  that paged a `PacketFlags` column by keyset cursor loses that
  capability here; it returns only if and when `ingraph` grows the
  storage half for this citizen.

  [#49]: https://github.com/findit-studio/mediadecode/issues/49
  [ingraph#524]: https://github.com/findit-studio/ingraph/pull/524

## [0.13.0] - 2026-09-01

### Added

- **`ScaledOutputCapability`: a platform-neutral capability word for
  decode-time output scaling, on `VideoStreamDecoder`.**

  ```rust
  pub enum ScaledOutputCapability { Unsupported, Supported }

  pub trait VideoStreamDecoder {
    // ...existing seats...
    fn scaled_output_capability(&self) -> ScaledOutputCapability { .. }
    fn request_scaled_output(&mut self, size: (u32, u32)) -> ScaledOutputCapability { .. }
  }
  ```

  A backend answers whether it can emit decoded pictures at a
  caller-requested size, and a caller requests one and reads back
  whether it took effect. Both default to `Unsupported` / a no-op, so
  every existing implementor of `VideoStreamDecoder` keeps compiling —
  additive. Refusal is never an error: a backend that cannot honor the
  request leaves the session decoding at full coded size, and the
  caller falls back to resampling it.

  **The determinism trade.** A backend's own scaler is not the ordinary
  area-resample kernel used elsewhere in this ecosystem — enabling
  scaled output trades cross-backend byte-determinism for bandwidth.
  Recommended together with a pinned backend
  (`mediadecode-ffmpeg`'s `DecodePath`, [#50]) rather than the auto
  probe, for a caller that needs the same picture bytes run to run.

  `mediadecode-ffmpeg`'s `CarrierVideoStreamDecoder` implements both
  seats and answers `Unsupported` on every path it opens today — see
  its own `CHANGELOG` entry and doc comments for the census, and the
  per-backend follow-ups: VideoToolbox ([#55]), NVDEC/CUVID ([#56]),
  VAAPI ([#57]), D3D11 Video Processor ([#58]).

  [#50]: https://github.com/findit-studio/mediadecode/issues/50
  [#55]: https://github.com/findit-studio/mediadecode/issues/55
  [#56]: https://github.com/findit-studio/mediadecode/issues/56
  [#57]: https://github.com/findit-studio/mediadecode/issues/57
  [#58]: https://github.com/findit-studio/mediadecode/issues/58

- **A track row carries the language its container declares**
  ([#44](https://github.com/findit-studio/mediadecode/issues/44)).

  ```rust
  impl<E: DemuxAdapter> TrackInfo<E> {
    pub const fn language(&self) -> Option<&E::Text>;
    pub fn with_language(self, v: Option<E::Text>) -> Self;
    pub fn set_language(&mut self, v: Option<E::Text>) -> &mut Self;
  }
  ```

  The seat `filename` and `mime_type` already have, for the third piece
  of identity a container writes about a track. Additive: a row built
  by an existing backend defaults it to `None`, as `new` does for the
  other two.

  **It carries the declaration and nothing else.** An MKV writes ISO
  639-2/B `ger`, an MP4 writes 639-2/T `deu`, a decades-old muxer
  writes `iw` for a language the registry renamed `he`, and Matroska
  can write a BCP 47 `zh-Hans`. Each is a different string for
  something a vocabulary may well call one language, and none of them
  is folded here. Folding them takes two published registries — the
  IANA subtag registry, plus ISO 639-2's own for the alpha-3 space BCP
  47 does not register — and the crate that owns one of those is where
  the fold belongs. A demux tier that folded early would be a *second*
  authority on the question, disagreeing with the first in exactly the
  cases the registries exist for.

  A narrower seat was considered and refused for the same reason: a
  three-letter code has nowhere to put `zh-Hans`, and dropping a tag
  because it does not fit is a guess wearing an absence's clothes.

  `Option`, and both answers are real. Matroska omits the element for
  an untagged track, so the row says nothing; an ISOBMFF `mdhd` has a
  language field it must fill, so an untagged MP4 track says `und` —
  *undetermined*, which the file really does declare. Neither is folded
  into the other.

- **An optional `ingraph` feature: this crate's types grant themselves
  citizenship in the indexing framework**
  ([#49](https://github.com/findit-studio/mediadecode/issues/49)).

  Off by default and purely additive. With it,
  [`packet::PacketFlags`] carries the faces `ingraph` reads a flags
  column through — `FlagsValue`, `FlagsFilterMarker`, `DefaultMarker`,
  `DefaultVecMarker`, `CursorValue`, `ColumnKind` and `ColumnEq` — so a
  declaration downstream can hold a column of it directly.

  The roster is one type because a census says so: `PacketFlags` is
  the only type of this crate's that a consumer mirrors today
  (`mediagraph`'s `types::packet::PacketFlags`, declared
  `#[ingraph::flags(u8, remote = "mediadecode::packet::PacketFlags")]`).
  Everything else here is packets, frames and sessions — values a graph
  moves *through* a node rather than values a row stores.

  What it replaces is a **restatement**. A mirror is a local enum
  repeating this crate's bits with a crossing in each direction and a
  drift pin over the pair, edited every time the upstream grows a bit,
  in a crate with no reason to know the bit exists. These rows sit on
  `bitflags::Flags` — the table `PacketFlags` already has — so a bit
  added here reaches the framework with nothing to edit anywhere.

  What the feature does **not** carry, deliberately: the per-backend
  storage bind (`sqlx`) and the two GraphQL wire seats. Both ride
  features of `ingraph`'s that name a backend or a wire library, and a
  media decoder that pulled a SQL driver and a GraphQL runtime into its
  dependency graph to describe three bits would be paying for a build
  it never runs. `ingraph` publishes `if_sqlite!` / `if_postgres!` /
  `if_mysql!` / `if_mongo!` for exactly that half, so the rows are
  reachable when a consumer asks for them.

  The feature implies `std` — `ingraph` is a `std` framework — and
  nothing in the default build changes. See `mediadecode::ingraph`.

### Added (BREAKING)

- **`Demuxer::TrackHandle`** — a new associated type, and the carrier a
  session's track rows are handed out over:

  ```rust
  type TrackHandle: Clone + Deref<Target = TrackInfo<Self::Adapter>>;
  ```

  A backend picks it the same way it already picks `Buffer`: a
  heap-backed, thread-crossing session binds `Arc<TrackInfo<..>>`, a
  single-threaded one binds `Rc`, and one whose rows live in memory it
  already borrows binds `&TrackInfo<..>`. That last binding is why the
  seat exists rather than an `Arc` written into the trait — **the demux
  tier stays `core`-only**, with no item in the module owning an
  allocation, which an `Arc` in the signature would have ended for
  every implementor including the ones that never allocate.

  `Clone` on the handle is required to be a refcount bump or a copied
  borrow, never a deep copy of the row — the message-carrier law, which
  the implementor upholds. The bound is what makes upholding it the
  path of least resistance rather than a discipline: `TrackInfo` has no
  `Clone`, so `Box<TrackInfo<..>>` has none either and no derive
  reaches a carrier that owns a row outright. A deep copy would have to
  be hand-written against the law.

  Breaking for implementors, who must name the type; source-compatible
  for callers that only *read* rows, since the handle derefs to the row.

### Changed (BREAKING)

- **`Demuxer::tracks` hands out handles, and the session keeps its
  table for life.**

  ```rust
  // before
  fn tracks(&self) -> &[TrackInfo<Self::Adapter>];
  // after
  fn tracks(&self) -> &[Self::TrackHandle];
  ```

  There is no ordering rule left on this face: read the table before
  the first pull, between two packets, after end of file, twice, or
  never. A caller that needs a row beyond a borrow of the session — to
  fan it out, or simply to hold it across the `&mut self` of the pull
  loop — clones a handle.

### Removed (BREAKING)

- **`Demuxer::take_tracks` is gone**, root and branch, with no
  deprecation alias
  ([#51](https://github.com/findit-studio/mediadecode/issues/51)).

  It was a *destructive read of state the session needs for its whole
  life*. The track table is what a demuxer classifies packets against,
  and this method moved it out — so a caller who followed the method's
  own documented order ("takes the table exactly once, right after
  opening a session and before pulling any packet") left the session
  unable to place a single packet. At the FFmpeg backend, which
  classified against the very `Vec` the `mem::take` emptied, that made
  every packet in a healthy file take the "no row describes this
  stream" arm: the documented order demuxed a full file to `Ok(None)`,
  with no error, no assertion and no log. The failing order was the
  documented one.

  The three obvious patches — keep a shadow classification table,
  classify off the `AVFormatContext` instead, or invert the
  documentation and make the violation loud — all leave a face whose
  contract is "reading this costs the session the state it runs on".
  So the shape went instead of the symptom. Nothing takes the table
  away any more, and nothing can: the door that removed it does not
  exist, and the accessor that replaced it borrows.

  **Migration.** `let rows = demuxer.take_tracks();` becomes
  `let rows: Vec<_> = demuxer.tracks().to_vec();` — a vector of
  handles, each one a refcount bump, addressing the session's own rows
  rather than rows moved out of it. Callers that were already wrapping
  each taken row in an `Arc` for fan-out can drop that step: the
  backend does it once, at open.

## [0.12.0] - 2026-08-30

### Changed (BREAKING)

- **`mediaframe` 0.7 → 0.9**, at the same pin as before
  (`default-features = false`, `features = ["frame"]` — the no-alloc
  tier). Two breaking minors crossed at once, and `mediaframe` is a
  **public** dependency: `color`, `pixel_format` and the structural
  primitives in `frame` are re-export modules, so a consumer still
  holding a `mediaframe 0.7` value no longer type-checks against this
  release. Same reasoning as the 0.5.0, 0.6.0 and 0.11.0 crossings —
  a public dependency crossing an incompatible 0.x minor is Breaking
  regardless of how much of its surface actually moved.

  **No `mediadecode` source line changed**, and that is a census result
  rather than a hope. What upstream moved, against what this crate
  names:

  - **0.8.0 is additive to rosters this crate does not name.** It adds
    the `image::Format` household, a plural `extensions()` face on
    `container::Format` and `audio::ContainerFormat`, four promoted
    variants (`container::Format::{M2ts, Threeg2}`,
    `audio::ContainerFormat::Aifc`, `image::Format::Heic`) and a
    widened, ignore-case `FromStr` on the two pre-existing container
    rosters — a real, observable parsing-result change upstream flags
    in its own log. None of the three roster households is named
    anywhere in this workspace: container/audio-container/image formats
    are a *directory-walk* vocabulary, and `mediadecode` starts one
    tier past the file, at a demuxer that has already been handed one.
    The `FromStr` change therefore has no site here.
  - **0.9.0 retires the lossy `lang::Language` triple** — the wrapper
    over `icu_locale_core` that kept only language/script/region and
    discarded every variant, extension and private-use subtag — and
    replaces it with a four-type `lang` family (`LanguageId`,
    `Language`, `ScriptSubtag`, `Region`) over a vendored BCP 47
    registry, moving `audio::Tags`'s language seat and `LanguageError`
    with it. **Neither the retired type, nor its successors, nor
    `audio::Tags` is named anywhere in this workspace.** The language
    tag this crate does carry is its own and always was:
    `subtitle::SubtitlePayload::Text`'s
    `Option<[u8; 3]>` ISO 639-2/T seat, a raw three-byte array minted
    here, which `mediaframe` never typed and 0.9 does not reach.
    Whether that seat should become a `lang::LanguageId` is a design
    question this bump does not force and does not answer.

  The consumed surface, swept item by item, is unchanged in 0.9:
  `color::{ChromaLocation, DynamicRange, Info, Matrix, Primaries,
  Transfer}` (re-exported here under the disambiguated `Color*`
  aliases), `pixel_format::PixelFormat`, and
  `frame::{BayerPattern, Dimensions, Plane, Rect}` — plus the `serde`
  feature's `mediaframe/serde` forwarding entry, which the `packet`
  module's own forwarding tests still prove end to end. Verified
  empirically, not just argued: `cargo check` / `cargo test` /
  `cargo clippy -- -D warnings` (`--workspace --all-features`) are all
  clean with zero source change.

  One dependency-graph note, upstream's and inherited rather than
  chosen here: `mediaframe` 0.9 drops `icu_locale_core` (and with it
  `tinystr`, `zerovec`, `writeable`, `litemap`, `potential_utf`) and
  takes on `smol-bytes` and `simdutf8` at its **`alloc` tier**. This
  crate's own pin is the no-alloc `frame` tier, where neither new crate
  is reachable; they enter the graph only through the adapters, which
  pin `mediaframe` with `alloc`. Net, the graph shrinks.

## [0.11.0] - 2026-08-28

Both public dependencies cross a breaking minor at once: `mediatime`
0.3 → 0.4 and `mediaframe` 0.6 → 0.7. `mediatime::{Timebase, Timestamp,
TimeRange}` and the re-exported `mediaframe` vocabulary (`color`,
`pixel_format`, `frame`) are this crate's own public surface — the same
reasoning as the 0.4.0 and 0.5.0 crossings — so a consumer holding a
`mediatime 0.3` or `mediaframe 0.6` value no longer type-checks against
this release. `mediaframe` 0.7.0 is itself the family's third instance
of this exact maneuver, after `mediatime` 0.1 → 0.2 and 0.2 → 0.3: a
public dependency crossing an incompatible 0.x minor is Breaking
regardless of how much of its surface actually moved. The upstream
notes are the authority
([mediatime](https://github.com/findit-studio/mediatime/blob/main/CHANGELOG.md),
[mediaframe](https://github.com/findit-studio/mediaframe/blob/main/CHANGELOG.md));
what follows is only what changes **here**.

No `mediadecode` source line changed: the whole diff is the two pins,
the version and this note.

### Changed (BREAKING)

- **`mediatime` 0.3 → 0.4.** Upstream is additive only — one commit,
  adding `Duration` (the unsigned counterpart to `SignedDuration`) plus
  its `core::time::Duration` and `SignedDuration` conversions.
  `Timebase`'s public surface is unchanged, and `Duration` does not
  appear in any `mediadecode` signature, so nothing here moves for it.
- **`mediaframe` 0.6 → 0.7.** Upstream's 0.7.0 is entirely the
  `mediatime` 0.3 → 0.4 crossing above — no `mediaframe` API moved.
  Same pin as before (`default-features = false`, `features =
  ["frame"]` — the no-alloc tier).

Verified empirically, not just argued: `cargo check` (default
features, `--all-features`, `--no-default-features`), `cargo test`
(default features and `--all-features`), and `cargo clippy
--all-features -- -D warnings` all pass — zero fallout in this crate's
own source.

## [0.10.0] - 2026-08-27

### Added

- **`Sent` and `Received` — the push rhythm's vocabulary**, in a new
  `rhythm` module and re-exported at the crate root. `Sent` is
  `Accepted` / `MustDrain`; `Received` is `Frame` / `NeedsInput` /
  `Ended`. Both are carried in the `Ok` arm, on every submission and
  every drain in the crate.

  The module is named `rhythm` because that is
  [already this crate's word][rhythm] for the thing both enums describe
  — packets in over time, frames out over time, the two not in step —
  and because one law with two faces belongs in one place rather than
  in a `send` module and a `receive` module that would each carry half
  the rationale and drift.

  [rhythm]: https://docs.rs/mediadecode/latest/mediadecode/decoder/#what-the-names-say

  The facade published **no** vocabulary for any of the three non-frame
  conditions before this, and said so in prose: "backends signal 'no
  frame ready' via a backend-specific `Error` variant". Three things
  followed from that, and all three are why this is worth a breaking
  release:

  - **`?` did not mean what it says.** A function that propagated a
    drain error propagated end-of-stream as a failure, and one that
    propagated a send error propagated *back pressure* as a failure — so
    every correct caller had to *avoid* `?` and hand-write a classifier
    instead.
  - **The classifier could not be written generically.** The conditions
    lived in `Self::Error`, a type the traits leave entirely to the
    backend. A consumer bounded on the trait alone had no way to ask.
    On the receive side what survived was
    `while d.receive_frame(&mut f).is_ok()` — which cannot tell
    *drained* from *broken*, and therefore swallows every receive-side
    failure it meets; that idiom appeared **24 times** in this
    repository's own tests, examples and benches. On the send side what
    survived was the **two-offer rule**: submit, and if that fails drain
    and submit again, treating the second failure as real — because
    "drain me first" and "this packet is damaged" arrived as the same
    `Err` and only a second attempt could separate them. Twice, because
    once meant nothing. It is a guess, and it is wrong whenever one
    drain is not enough.
  - **Backends of the same trait disagreed completely.** WebCodecs named
    all four conditions; the FFmpeg video and audio decoders named none,
    passing `Other { errno: EAGAIN }` and `Eof` straight through. A
    caller generic over `VideoStreamDecoder` observed a different
    protocol depending on which backend it was instantiated with — the
    exact thing a trait exists to prevent.

  **`Sent` and `Received` are deliberately not `#[non_exhaustive]`, and
  every backend error type in the family deliberately now is.** The two
  decisions are the same decision seen from its two ends. A status enum
  is a *closed protocol vocabulary*: its arms are the state set of the
  substrate every push decoder is built on, there is no further answer
  to discover, so a permanent wildcard arm would be dead weight hiding a
  state a consumer forgot. An error type is an *open fault taxonomy*:
  new ways to fail really are discovered, and a consumer that meets one
  it has never heard of should take its generic-fault path — which is
  exactly what a wildcard arm is for.

### Changed

- **BREAKING: `receive_frame` returns `Received`, and
  `send_packet` / `send_frame` / `send_eof` return `Sent`** on
  `decoder::{VideoStreamDecoder, AudioStreamDecoder, SubtitleDecoder}`,
  on `resampler::AudioResampler`, and on the `future::local` /
  `future::send` mirrors of all three decoder faces. `Err` is now
  **fault only** on every one of them, and the `#[must_use]` on both
  enums is what stops an answer being dropped. `flush` is unchanged:
  nothing is offered to it, so there is nothing to be back-pressured.

- **`send_eof` answers `Sent` too, and that is not symmetry for its own
  sake.** A session with undrained output can be unable to record the
  end-of-stream yet, and `Sent::MustDrain` there means it was **not**
  recorded — a distinction `is_ok()` cannot make, and one this release
  had to act on inside the FFmpeg backend (see its log).

- **The async mirrors keep `Sent::MustDrain`**, which is the one kind of
  pressure awaiting cannot resolve: only the caller's own
  `receive_frame` relieves it, and both methods hold `&mut self`, so a
  `send_packet` that parked instead of answering would deadlock. Host
  pressure the browser drains by itself is still awaited; pressure that
  needs the caller is reported.

- **`AudioResampler::Error`'s doc no longer claims to carry "nothing
  ready yet"**, and the trait states that the tail's end arrives as
  `Received::Ended` and never as `Received::NeedsInput` — the
  distinction that lets a drain loop be written without the caller
  remembering whether it called `send_eof`.

- **`SubtitleDecoder::receive_frame` documents that a backend with no
  tail still has an end.** A decoder whose underlying API produces its
  cue inline still answers all three states: `NeedsInput` until a packet
  has produced one, `Ended` once `send_eof` has been signalled.

### Unchanged, and deliberately

- **`Demuxer::next_packet` keeps `Result<Option<_>, E>`.** It has no
  needs-input state — a demuxer is pulled, not fed — so a three-state
  word here would declare an arm no backend can produce and every
  consumer would still have to match. It also carries its packet in the
  `Ok` arm rather than into a `dst`, so folding it onto `Received` would
  need either a generic payload (making every decoder write
  `Received<()>`) or a second enum. The two faces share the law, not the
  type: **a protocol state never travels in `Err`.** The trait docs now
  say so.

- **`ImageDecoder::decode` is untouched.** A one-shot decode has no
  session states: "these bytes are not a picture" is a fact about the
  payload the caller handed over, which is what an error is for. Its doc
  now draws that line explicitly.

- **`VideoFrameSource` / `AudioFrameSource` are untouched.** They are
  index-addressed pull faces; the caller names the frame, so there is no
  rhythm to report.

### Fixed

- **The `serde` feature now forwards `mediaframe/serde`**, so re-exported
  mediaframe vocabulary types serialize too.


## [0.9.0] - 2026-08-26

### Added

- **The resource governance contract** (user-ruled 2026-08-25), written
  into the `adapter` module's docs beside the amputation contract. It
  states, in three tiers, what a decoding backend owes a caller who asks
  "how much can this cost?":

  - **Tier one — what the backend allocates itself.** Every byte a
    backend copies or allocates is bounded, by a named seat or by a
    format; there is no third kind. Provable, and proved by enumeration
    rather than assertion — a backend keeps an accounting of its own
    allocation sites and what bounds each. Two rules folded in that this
    release paid for: *a judge must dominate the allocator's arithmetic,
    not the payload's*, and *everything a conversion can refuse is
    refused before anything it can allocate is allocated*.
  - **Tier two — the substrate's own knobs.** A backend sets every
    resource knob its substrate offers, at every interposition point the
    substrate exposes. Explicitly **defense in depth, not a proof**: the
    union of those knobs is whatever the substrate's authors chose to
    make interruptible.
  - **Tier three — the boundary.** Allocations internal to a substrate,
    past its knob surface, are the substrate's territory. A deployment
    needing a hard memory bound puts the decode behind an OS-level
    instrument — an rlimit, a cgroup, a memory-limited worker — and the
    seats compose with it rather than replacing it. Stated without
    hedging: **this crate does not promise to be a hypervisor for its
    substrates.**

  The principle is homed here because it binds every backend, not only
  the FFmpeg one; the concrete knob enumeration belongs to whichever
  crate owns the substrate.


## [0.9.0] - 2026-08-24

### Added

- **The two-carrier-lanes contract** (user-ruled 2026-08-25), written
  into the `adapter` module's docs beside the amputation contract. A
  backend may offer more than one carrier, and the contract states what
  each owes:

  - the **view** lane hands out a refcounted handle onto the
    allocation the substrate already made — nothing copied — and is the
    **default**, because the ordinary consumer decodes in place and
    paying to copy bytes it will discard is a cost with nothing on the
    other side of it;
  - the **owned** lane is the amputation contract as written: one copy
    at the boundary, `Send + Sync`, a lifetime answerable to nobody.

  With a tradeoff table, the **pool-hostage warning** — a frame held is
  a pool slot held, so a consumer that queues view frames stalls its own
  decoder — and the rule that follows from it: read in place, drop,
  decode on. Graph traffic, fan-out and anything needing `Sync` is
  steered to the owned lane explicitly; `mediagraph` belongs there.


### Added (BREAKING)

- **The D-seat amputation contract**, written into the `adapter`
  module's docs as the one law a backend's buffer type `B` must obey:
  *owned, `Send + Sync`, cheap to clone (a refcount bump), with its
  lifetime fully decoupled from the backend's internal buffers at the
  exit — no FFI pointer, pooled buffer or JavaScript handle crosses
  the seam.*

  It is a law about backends, not a change to this crate's signatures.
  `VideoFrame<P, E, D>`, `AudioFrame<S, C, E, D>`,
  `SubtitleFrame<E, D>`, the five `*Packet<E, D>` and `Plane<B>` keep
  every parameter they had, `AsRef<[u8]>` remains the only bound, and
  no concrete carrier is named anywhere in the crate. What the law
  states is what a *consumer* may rely on when a backend hands it a
  frame: that holding one does not hold a decoder open, that it can
  cross a channel, and that fanning it out to two consumers costs a
  refcount rather than a copy of the picture. The rationale — including
  why one copy at the boundary is cheaper than the copy each consumer
  would otherwise have to make later — is on the module.

- **`ImageFrame<P, E, D>` — a fourth frame household.** A decoded
  still: cover art, an embedded thumbnail, a poster frame. It carries
  `dimensions`, `visible_rect`, `pixel_format`, up to four `Plane<D>`,
  `ColorInfo` and backend `extra` — and **no `pts`, no `duration`**.
  Not `None`-valued seats: absent ones. A still is not on the
  timeline, which is the same fact `AttachmentPacket` has always
  stated on the packet side, and a field that can only ever be empty
  is an invitation to a consumer to sort by it.

  `visible_rect` earns its place here more than anywhere: a JPEG's
  coded dimensions are rounded up to its MCU grid (8 or 16 pixels), so
  a 30×30 photograph is coded 32×32 and only the visible rect says
  which of those pixels are the picture. Construction mirrors the
  video household exactly — a panicking `new`, a fallible `try_new`,
  and a new `FrameError::TooManyImagePlanes` arm with a
  `TooManyImagePlanes` payload, over the same 4-slot cap (packed RGB
  = 1, MJPEG's YUV = 3, either plus alpha = 4).

- **`ImageAdapter`** — the fourth per-kind vocabulary: `CodecId`,
  `PixelFormat`, `PacketExtra`, `FrameExtra`. Minted rather than
  folded into `VideoAdapter` because the two disagree about the one
  thing an adapter exists to name: a still's extras are EXIF, an ICC
  profile, an orientation — not a picture type, a field order or a
  best-effort timestamp. Its `PacketExtra` is the *attachment's*
  extras, so a backend that also implements `DemuxAdapter` normally
  binds one type in both seats and a cover-art payload goes from
  `next_packet` into `decode` with nothing to convert.

- **`ImageDecoder`** — a one-shot decoder seam. `decode(&packet)`
  takes an `AttachmentPacket` and answers an `ImageFrame`; there is no
  `send_packet` / `receive_frame` split and no `send_eof`, because an
  attachment track's contract is exactly one packet and a still
  codec's answer to it is exactly one picture.

  **No `Stream` in the name, and the register now says something.**
  `VideoStreamDecoder` and `AudioStreamDecoder` carry it because they
  have a rhythm — packets in over time, frames out over time, the two
  not in step. `SubtitleDecoder` and `ImageDecoder` do not.

  Mirrored under `future::local::ImageDecoder` and
  `future::send::ImageDecoder`, on the same `trait_variant` machinery
  as the other four faces there, so the register is complete rather
  than four-fifths complete. One `async fn`, because the sync trait
  has one method: what the `async` buys is a backend whose *decode* is
  asynchronous — a browser's `createImageBitmap`, a GPU submission —
  not a rhythm the sync trait was hiding. It is also the face the
  WebCodecs adapter will implement when it implements one at all.

- **`Clone` on `VideoFrame` and `SubtitleFrame`** (`AudioFrame` had it
  already), derived, so the per-parameter bound is exactly what
  cloning the fields requires. All four households are now cloneable —
  the consumer-side half of the amputation contract, whose backend-side
  half is what makes the clone a refcount bump.

### Changed

- `decoder`'s module docs now state what the trait names mean, what
  may be bound in the `Buffer` seat, and why construction is off the
  traits. `adapter`'s carry the contract itself.

## [0.8.0] - 2026-08-24

### Added

- **`Debug` across the demux tier; `Clone` where it stays cheap.**
  `TrackParams` and `TrackInfo` gain `Debug` (previously absent),
  hand-written and bounded on what their fields actually need
  (`E::TrackExtra`, `E::Text`, and each per-kind payload struct's own
  associated types) rather than on `E` itself, which `#[derive(Debug)]`
  cannot see past. `VideoPacket`, `AudioPacket`, `SubtitlePacket`,
  `DataPacket`, and `AttachmentPacket` derive `Clone` + `Debug` directly
  — their fields are the generic parameters themselves, so the derive's
  bound is already precise — and `DemuxedPacket` gets the same pair
  hand-written, for the same associated-type reason, since its five
  arms carry exactly those packet types.

  `TrackParams` and `TrackInfo` do **not** get `Clone`. An interim
  version of this entry gave them one — a real allocation-and-copy per
  clone, minted to satisfy a channel bound — and it violated the
  message-carrier law recovered from the frozen desktop tree: messages
  may be `Clone`, but `Clone` is always a refcount bump, never a deep
  copy. That impl is gone. In its place, `Demuxer` gains
  **`take_tracks`**, the owned-tracks door: the first call moves the
  whole track table out of the session; `tracks()` answers the empty
  slice afterward. The intended caller takes the table once, right
  after opening a session and before pulling any packet, and wraps
  each row in `Arc` for fan-out — one allocation per track, ever,
  rather than a deep copy per consumer.

### Changed (BREAKING)

- **Every struct-shaped enum variant is now a tuple variant wrapping a
  named payload struct** — `TrackParams`'s six arms, `DemuxedPacket`'s
  five arms, and `SubtitlePayload`'s two arms. The house rule this
  crate otherwise already followed everywhere else (see `FrameError`,
  which has always shaped its variants this way): a struct variant has
  no nameable type of its own, so it cannot answer
  `is_<variant>`/`unwrap_<variant>`/`try_unwrap_<variant>`, and its
  fields are trapped instead of being a reusable, documented,
  accessor-bearing type.

  - `TrackParams::Video { codec, width, height, pixel_format,
    frame_rate }` → `TrackParams::Video(VideoTrackParams<E>)`, and the
    same shape for `Audio` → `AudioTrackParams`, `Subtitle` →
    `SubtitleTrackParams`, `Data` → `DataTrackParams`, `Attachment` →
    `AttachmentTrackParams`, `Unknown` → `UnknownTrackParams`. Each
    payload has private fields, a positional `new(...)`, and a `const`
    accessor per field (`codec()`, `width()`, `pixel_format()`, …),
    matching whichever of `Copy`/`Clone`-only the field's own type is.

  - `DemuxedPacket::Video { track, packet }` →
    `DemuxedPacket::Video(VideoTrackPacket<E, D>)`, and the same shape
    for `Audio` → `AudioTrackPacket`, `Subtitle` →
    `SubtitleTrackPacket`, `Data` → `DataTrackPacket`, `Attachment` →
    `AttachmentTrackPacket`. Named `<Kind>TrackPacket` rather than
    reusing `VideoPacket` etc. — those names are already taken by the
    packet type nested *inside* the envelope. Each payload carries
    `track()`, `packet()` (borrowed), `into_packet()` and
    `into_parts()` (owned) — the enum's own `track()` / `kind()`
    accessors are unchanged and now read through these.

  - `SubtitlePayload::Text { text, language }` →
    `SubtitlePayload::Text(subtitle::Text<B>)`, and `Bitmap { regions
    }` → `SubtitlePayload::Bitmap(subtitle::Bitmap<B>)`.

  A match that used to destructure the struct fields now binds the
  payload and reads it through accessors:

  ```rust
  // Before
  match params { TrackParams::Video { width, height, .. } => .. }
  // After
  match params { TrackParams::Video(p) => (p.width(), p.height()) }
  ```

  **The accessor face rides the reshape.** `TrackParams`,
  `DemuxedPacket`, and `SubtitlePayload` now derive
  `derive_more::{IsVariant, Unwrap, TryUnwrap}`, with
  `#[unwrap(ref, ref_mut)]` and `#[try_unwrap(ref, ref_mut)]` on each
  enum — every arm answers `is_<variant>()`,
  `unwrap_<variant>()` / `_ref()` / `_mut()`, and
  `try_unwrap_<variant>()` / `_ref()` / `_mut()`. This is exactly what
  the reshape above was for: a struct variant has no nameable payload
  type to return from `unwrap_<variant>`, so the face could not exist
  until the fields moved into named structs. `TrackKind` gains
  `IsVariant` too. The `derive_more` dependency row picks up the
  `unwrap` / `try_unwrap` features alongside the existing `display` /
  `is_variant`.

## [0.7.0] - 2026-08-23

### Added

- **The demux tier — `mediadecode::demuxer`.** Until now the spine
  started at the decoder: something else had to open the container, name
  its tracks and hand over packets. That something is now a trait.

  - **`Demuxer`** — the opened-session face: `tracks()`, `next_packet()`,
    `seek(target)`. The session is **pull-style**; the caller owns the
    loop, holds one packet at a time whatever the file's length, and
    pulls only when it is ready. `Ok(None)` is end of file.

    Opening is deliberately **not** on the trait. FFmpeg opens from a
    path or a `Read + Seek` reader, a WebAssembly container parser opens
    from bytes — the same reason the decoder traits carry no
    constructor. And not every backend is a demuxer at all: R3D and BRAW
    are clip-oriented SDKs with no packets to hand out, so they keep
    joining a pipeline one tier up through `VideoFrameSource` /
    `AudioFrameSource`.

  - **`DemuxedPacket<E, D>`** — five arms (`Video` / `Audio` /
    `Subtitle` / `Data` / `Attachment`), each `{ track, packet }`. The
    packet types stay exactly what a decoder already accepts; the
    envelope carries the track coordinate, so nothing has to be stripped
    before `send_packet`.

  - **`DataPacket<E, D>`** and **`AttachmentPacket<E, D>`** — the two
    packet kinds that had no type because they have no decoder. A data
    packet is timed and never reordered, so like `SubtitlePacket` it has
    no DTS seat. An attachment carries **no timestamps at all**: a font
    or a cover image is not on the timeline.

  - **`TrackKind`** — the closed roster `{ Video, Audio, Subtitle, Data,
    Attachment, Unknown }`, minted here because `mediaframe` has a track
    *disposition* vocabulary but no kind vocabulary and the dependency
    direction forbids reaching the other way. **Cover art is
    `Attachment`, not `Video`** — one sample, no timeline, no motion —
    so the `Video` arm carries true motion video and nothing else.

  - **`TrackInfo<E>` / `TrackParams<E>` / `TrackIndex`** — the track
    table. `TrackIndex(i)` *is* the position of `tracks()[i]`, so a
    packet's coordinate needs no side map. `TrackInfo::kind()` is read
    off the `TrackParams` arm rather than stored beside it: there is no
    way to build a row whose advertised kind disagrees with its payload.
    Attachment identity — the filename it was attached under, its MIME
    type — lives on the row, not repeated on every packet.

  - **`DemuxAdapter`** — the demux tier's vocabulary, bundling the three
    existing adapter families and adding what only a demuxer needs:
    `DataExtra`, `AttachmentExtra`, `TrackExtra`, and a `Text` carrier
    for identity metadata (a seat, not a fixed string type, so the core
    stays allocator-free). The three families are bound to share the
    bundle's `CodecId`: a demuxer reads one container, and a container's
    track table has one codec-identifier column.

  Two contracts are stated at the face and binding on every
  implementation. **An attachment track delivers exactly one packet,
  before any timed packet** — synthesised when the container keeps the
  payload outside the packet stream (fonts, whose bytes live in codec
  extradata), hoisted when it is a real packet (cover art). And **`seek`
  obeys three laws**: it flushes session state; it lands on the nearest
  keyframe *at or before* the target, never after, because a decoder
  started past the target has no reference frame; and attachments are
  never replayed, however many times the session seeks.

- **The resample seam — `mediadecode::resampler`.** `AudioResampler`:
  `send_frame` / `receive_frame` / `send_eof` / `flush`, the
  `AudioStreamDecoder` push pair one tier along, with "nothing ready
  yet" signalled the same way — a backend-specific `Error` variant out
  of `receive_frame`.

  Three contract lines ride the face. **Both specs are explicit at
  construction**: the source is read off the track's `TrackInfo`, the
  target is the caller's — 16 kHz mono for a speech model, 48 kHz for an
  audio-event one, from the same file at the same time. The target is
  options, never a constant. **A mid-stream format change is a named
  refusal**: the face does not silently reconfigure, because that would
  resample the two halves of a stream on different terms and hand back
  one unbroken timeline built out of them. And **EOF drains the
  conversion tail** — a rate converter holds tens of milliseconds inside
  its filter, and skipping the drain loses the end of every file. Output
  timestamps are kept by delay-compensated accounting, so the drained
  tail continues the same timeline.

  No FFmpeg in the shape: `swresample` is merely the first
  implementation, and a pure-Rust polyphase resampler fits the same
  face.

## [0.6.0] - 2026-08-21

### Removed (BREAKING)

- **The `channel` module is gone, whole.** `ChannelLayoutKind`,
  `AudioChannelOrderKind`, `AudioChannelSpec`, `AudioChannelLayout`,
  their two parse errors (`ParseChannelLayoutKindError`,
  `ParseAudioChannelOrderKindError`) and the `serde` / `arbitrary` /
  `quickcheck` matrices that covered them are deleted, not moved and
  not deprecated. The vocabulary's house is `mediaframe::audio`, which
  now carries all four:

  | was (`mediadecode::channel`) | is (`mediaframe::audio`) |
  | ---------------------------- | ------------------------ |
  | `ChannelLayoutKind` (39 named + `Unknown`) | `ChannelLayout` (43 named + `Other(SmolStr)`) |
  | `AudioChannelOrderKind`      | `ChannelOrder`           |
  | `AudioChannelSpec`           | `ChannelSpec`            |
  | `AudioChannelLayout`         | `ChannelLayoutDescription` |

  **Nothing is re-exported.** `AudioFrame`'s channel-layout parameter
  `C` is generic and always was, so this crate never named a channel
  type in a signature and needs none now; a consumer that wants one
  adds `mediaframe` and spells `mediaframe::audio::*`. Both adapters in
  this workspace did exactly that.

  Renamings ride along inside the moved types. `ChannelLayout`'s slugs
  are FFmpeg's own, taken from `channel_layout_map[]` rather than from
  the constant names, so several spellings change: `"5.0"` / `"5.1"` /
  `"7.1(wide)"` are the **back**-speaker layouts and the side ones are
  qualified `"5.0(side)"` / `"5.1(side)"` / `"7.1(wide-side)"`;
  `"5.1.4"`, `"7.1.4"` and `"9.1.4"` carry no `(back)` suffix even
  though their constants do; `StereoDownmix` is `"downmix"`.
  `ChannelLayoutDescription` renames the free-text field `description`
  to `text` (`text()` / `with_text()` / `set_text()`), and its
  `known_kind()` returns `&ChannelLayout` — borrowed, because the
  escape arm costs the type `Copy` — with `ChannelLayout::default()`
  (the `Other("")` sentinel) where `ChannelLayoutKind::Unknown` used to
  sit.

  Four defects leave with the table that held them rather than being
  fixed in place:

  - `Ch7_1Wide` and `Ch7_1WideBack` carried each other's slugs. FFmpeg
    gives the unqualified `"7.1(wide)"` to the **back** layout and
    qualifies the side one; this crate had it the other way round.
  - `Ch7_1TopBack` named nothing. `AV_CH_LAYOUT_7POINT1_TOP_BACK` is a
    `#define` alias of `AV_CH_LAYOUT_5POINT1POINT2_BACK`, matched
    earlier in this workspace's own FFmpeg adapter, so the variant was
    unreachable — a value writable through `as_str` / `to_u32` and
    produced by nothing.
  - `Ch2_2`'s doc promised "left, right, subwoofer, and an additional
    channel"; `AV_CH_LAYOUT_2_2`'s mask is FL+FR+SL+SR — no LFE and no
    centre. Upstream spells it `QuadSide`, after its `"quad(side)"`
    slug, for that reason.
  - `Ch5_1`'s doc described rear speakers while the adapter mapped it
    from `AV_CH_LAYOUT_5POINT1`, which is the side layout. The two
    sentences could not both be true.

  Also gone: the runtime roster check that could not see any of this.
  It walked `from_u32`, so a variant with an `as_str` and a `to_u32`
  but no `from_u32` arm stayed invisible to it — which is exactly the
  shape `Ch7_1TopBack` had. Upstream's `roster!` pins its list with a
  compile-time exhaustive match instead.

- **`smol_str` is no longer a dependency.** It was reachable only from
  the two deleted records, so the optional dep and its four feature
  edges (`alloc`'s `smol_str`, `std`'s `smol_str/default`, `serde`'s
  `smol_str?/serde`, `arbitrary`'s `smol_str?/arbitrary`) go with them.
  A build that turned on `mediadecode/alloc` to get `smol_str` into the
  graph now names it directly.

### Changed (BREAKING)

- **`mediaframe` 0.5 → 0.6**, at the same pin as before
  (`default-features = false`, `frame` — the no-alloc tier). This is
  the release that carries the channel household, and `mediaframe` is a
  public dependency here (`color`, `pixel_format`, `cfa` and `frame`
  are re-export modules), so a downstream still spelling `mediaframe`
  0.5 gets two versions of it in one graph. Bump in lockstep.

  Note the tier: `mediaframe::audio` is compiled only at `mediaframe`'s
  alloc-or-std tier, and this crate's pin does not enable it. A
  consumer naming `mediaframe::audio::*` turns on `mediaframe/alloc`
  (or `std`) on its own dependency edge, as both adapters in this
  workspace do.

## [0.5.0]

Tracks `mediaframe` 0.4 → 0.5, a breaking minor. No `mediadecode`
source line changed: the whole diff is the pin, the version and this
note. It is still breaking, because `mediaframe` is a **public**
dependency — `color`, `pixel_format`, `cfa` and `frame` are re-export
modules — so a downstream that still spells `mediaframe` 0.4 gets
`expected ColorMatrix, found Matrix`, with the compiler adding "there
are multiple different versions of crate `mediaframe` in the
dependency graph". Bump `mediaframe` in lockstep and that error goes
away.

### Changed (BREAKING)

- **`mediaframe` 0.4 → 0.5**, at the same pin as before
  (`default-features = false`, `frame` — the no-alloc tier). Upstream
  breaks on three counts; here is where each one lands.

  - **`FromStr::Err` is `Infallible` at the alloc / std tier** for ten
    vocabularies, six of which this crate re-exports
    (`pixel_format::PixelFormat` and `color`'s `Matrix`, `Primaries`,
    `Transfer`, `DynamicRange`, `ChromaLocation`). At *this* crate's
    pin nothing moves — the parse still returns
    `ParsePixelFormatError` / `ParseMatrixError`, because the escape
    arm those errors exist for is `alloc`-gated and this crate does
    not enable `alloc` on `mediaframe`. But the tier is not this
    crate's to fix: any dependency anywhere in the graph that turns on
    `mediaframe/alloc` unifies the feature and flips the re-exported
    associated type to `core::convert::Infallible`. Code that names
    the old error through a `mediadecode::` path — a `match` arm, a
    `From` impl, an annotated binding — moves the way upstream
    describes; code that only propagated it needs nothing.
  - **`subtitle::TrackOrigin` opened** (no longer `Copy`, `as_str` no
    longer `const fn -> &'static str`, `to_u32` returns `Option<u32>`,
    both wire forms changed) — **not reachable from here**.
    `mediaframe`'s `subtitle` module is compiled only at the `alloc`
    tier, and this crate neither enables it nor re-exports it. The
    `to_u32` and `Parse*Error` names that do appear in this crate are
    its own — `channel::ChannelLayoutKind`, `channel::AudioChannelOrderKind`
    — and are untouched.
  - **`subtitle::Format::PgsSub` merged into `HdmvPgs`** — likewise
    not reachable; this crate names neither.

### Added

- **`ROSTER` on the re-exported open vocabularies.**
  `mediadecode::pixel_format::PixelFormat` and the five re-exported
  `color` enums gain `pub const ROSTER: &'static [Self]`, upstream's
  declaration-order list of the named variants, excluding the open
  escape. It is available at this crate's no-alloc pin. Consumers that
  were hand-copying one of these vocabularies can read the list
  instead.

### Changed

- **`cfa::BayerPattern` is closed.** Upstream removed
  `#[non_exhaustive]`, so a downstream matching
  `mediadecode::cfa::BayerPattern` may now drop its wildcard arm and
  get a completeness proof from the compiler. Existing matches keep
  compiling; this only removes a restriction.

## [0.4.0] - 2026-08-19

### Added

- **The three optional matrices reach every type that owns a wire
  shape.** `channel::AudioChannelSpec`, `channel::AudioChannelLayout`
  and `packet::PacketFlags` gain `serde`, `arbitrary` and `quickcheck`
  impls; the matrices had covered only the two `channel` vocabularies.
  The two records travel as a map of their accessor names (with the
  vocabularies inside them still as slugs); `PacketFlags` travels as
  its raw bits, because a bit set has no name to spell and
  `from_bits_retain` has to carry bits this build has no constant for
  (FFmpeg's `AV_PKT_FLAG_TRUSTED` / `_DISPOSABLE`). Same reasoning, and
  the same wire, as `mediaframe::TrackDisposition`.
- **Feature wiring**: `serde` now enables `smol_str?/serde`, `alloc`
  enables `serde?/alloc` and `std` enables `serde?/std` — the records'
  `Vec` and `SmolStr` fields need them. `arbitrary`'s existing
  `smol_str?/arbitrary` is live for the first time.

### Removed

- **`serde` no longer enables `bitflags/serde`.** It was inert
  (bitflags 2 routes serde through a derive placed inside the
  `bitflags!` body, which `PacketFlags` does not carry), and its wire
  shape is a flag grammar — `"KEY | CORRUPT"` — in any human-readable
  format, which is not the shape `PacketFlags` takes.

Both public dependencies cross **two** breaking minors at once:
`mediatime` 0.1 → 0.3 and `mediaframe` 0.1 → 0.3. Neither is an
internal detail — `mediatime::{Timebase, Timestamp, TimeRange}` and
eleven `mediaframe` types are re-exported as mediadecode's own public
surface and appear in its signatures — so a consumer holding a
`mediatime 0.1` / `mediaframe 0.1` value no longer type-checks against
this release. The upstream notes are the authority
([mediatime](https://github.com/findit-studio/mediatime/blob/main/CHANGELOG.md),
[mediaframe](https://github.com/findit-studio/mediaframe/blob/main/CHANGELOG.md));
what follows is only what changes **here**.

### Changed (BREAKING)

- **`mediatime` 0.1 → 0.3.** `Timebase`'s numerator and denominator
  are now signed (`u32 → i32`, `NonZeroU32 → NonZeroI32`), matching
  FFmpeg's `AVRational`; `Timebase::new` panics on a negative
  numerator or denominator, with `try_new` as the fallible form.
  Every `Timebase::new(n, NonZeroU32::new(d).unwrap())` call site
  becomes `Timebase::new(n, NonZeroI32::new(d).unwrap())`. mediatime
  0.2 → 0.3 additionally deleted the bare rescale ladder
  (`rescale_pts` / `rescale` / `duration_to_pts` → `checked_*` /
  `saturating_*`), corrected the rounding to `AV_ROUND_NEAR_INF` and
  moved `frames_to_duration` onto the new `Rate` type — **mediadecode
  calls none of those**, so nothing here moves for them; consumers
  that call them through mediadecode's re-export do.
- **`mediaframe` 0.1 → 0.3.** `PixelFormat::Unknown(u32)` is struck —
  the same numeric escape goes from eleven coded vocabularies in all.
  `PixelFormat::None` — a **named** member (FFmpeg's own
  `AV_PIX_FMT_NONE`, and the `Default`) — is what the adapter crates
  now produce where they used to produce `Unknown(raw as u32)`; the
  raw integer no longer rides along. `from_u32` returns
  `Option<Self>` and `to_u32` returns `Option<u32>`. The open
  extension arm mediaframe offers instead is `Other(SmolStr)`, which
  lives behind mediaframe's `alloc` feature; mediadecode pins
  mediaframe at the no-alloc tier (`default-features = false`,
  `features = ["frame"]` — unchanged from 0.1), so the re-exported
  vocabularies are **closed** here.
- **`VideoAdapter::PixelFormat` is now `Clone + Eq + Debug`**, was
  `Copy + Eq + Debug`. mediaframe 0.3 dropped `Copy` from the ten
  coded enums (the `Other` arm is heap-capable), so a backend binding
  `mediaframe::PixelFormat` could not satisfy the old bound. Relaxing
  a bound is free for implementors; consumers that relied on
  `A::PixelFormat: Copy` need a `.clone()` or a borrow.
  `AudioAdapter::ChannelLayout` was already `Clone` for the same
  reason (`AudioChannelLayout` carries an owned description) — this
  brings the two into line.
- **`VideoFrame::color` is no longer `const`** and returns a clone.
  `mediaframe::color::Info` lost `Copy` in 0.3. The signature is
  unchanged (`fn color(&self) -> ColorInfo`), matching what
  mediaframe's own `Info` accessors did with the same problem.
- **`VideoFrame::with_color` and `VideoFrame::set_color` are no longer
  `const`.** Assigning the field drops the previous `ColorInfo`, and
  mediaframe 0.3's `Info` acquires a destructor as soon as
  mediaframe's `alloc` feature is on — a const destructor is not
  evaluable (`E0493`). This is **not** conditional on how mediadecode
  pins mediaframe: Cargo unifies features across the whole graph, so
  any other crate depending on `mediaframe` with its defaults turns
  `alloc` on for this build too. The `const` therefore cannot be kept
  at either tier. Signatures are otherwise unchanged; only `const`
  evaluation of these two setters is lost.

### Changed

- Version bumped to 0.4.0. The sibling adapters move to 0.4.0 with it.
- **Fifteen `channel::ChannelLayoutKind` renderings move**
  ([#19](https://github.com/findit-studio/mediadecode/pull/19)). The
  multi-word names are hyphenated rather than spaced, so `Display` (and
  the new `as_str`) now print `"stereo-downmix"`, `"5.1-back"`,
  `"7.1-wide-back"` where they printed `"stereo downmix"`, `"5.1 back"`,
  `"7.1 wide back"`. Affected: `StereoDownmix`, `Ch2_1Alt`, `Ch5_0Back`,
  `Ch5_1Back`, `Ch5_1_2Back`, `Ch5_1_4Back`, `Ch6_0Front`, `Ch6_1Back`,
  `Ch6_1Front`, `Ch7_0Front`, `Ch7_1Wide`, `Ch7_1WideBack`,
  `Ch7_1TopBack`, `Ch7_1_4Back`, `Ch9_1_4Back`. The other 24 slugs are
  unchanged. A slug has to survive a CLI argument, a filename and an
  environment variable without quoting, and the sibling crates spell
  every multi-word slug this way. **No alias was kept**: the old spaced
  spelling is now a parse error, because one value has one name.

### Added

- **A text form for the two `channel` vocabularies**
  ([#19](https://github.com/findit-studio/mediadecode/pull/19)).
  `ChannelLayoutKind` and `AudioChannelOrderKind` gain a `const fn
  as_str` returning a canonical lowercase slug, `FromStr` reading it
  back, and one error type each — `ParseChannelLayoutKindError` /
  `ParseAudioChannelOrderKindError`, both `#[non_exhaustive]` unit
  structs that deliberately do not retain the rejected input.
  `AudioChannelOrderKind` also gains `Display`, which it did not have.
  The door folds ASCII case and nothing else (`"5.1-BACK"` parses,
  `"5.1-back "` does not) and allocates nothing, so it works at the
  no-`alloc` tier where both enums live. The numeric doors (`to_u32` /
  `as_u32` / `from_u32`) are unchanged and stay the compact form.
- **`serde` / `arbitrary` / `quickcheck` for those two vocabularies**
  ([#19](https://github.com/findit-studio/mediadecode/pull/19)). Before
  this, all three features compiled and no type in the crate
  implemented anything. serde carries them as their slug rather than
  their `u32` code: an unrecognised name is a deserialization error,
  where an unrecognised code would decode to `Unknown` / `Unspecified`
  and invent a value. Both generators choose uniformly from the
  variant roster rather than decoding an arbitrary `u32`, which would
  have spent the whole budget on the fall-through variant.

### Not affected

- `mediaframe::frame::Rational` widening to `i64` / `NonZeroI64`
  (mediaframe 0.2) has **no** site here: mediadecode names no
  `Rational`, `SampleAspectRatio` or `FrameRate`, and
  `mediadecode-ffmpeg`'s `Rational` is `ffmpeg_next::Rational`.
- The retired shared `mediaframe::parse::ParseError`, the new
  `KernelMatrix` / `KernelGamut` kernel selectors, and mediaframe's
  serde/buffa number → slug wire move have no site here either:
  mediadecode parses none of those vocabularies, calls no conversion
  kernel, and its `serde` feature does not reach mediaframe.

[0.4.0]: https://github.com/findit-studio/mediadecode/releases/tag/mediadecode-v0.4.0

## [0.3.1] - 2026-06-14

Additive release ([#10](https://github.com/findit-studio/mediadecode/pull/10)).

### Added

- **`Clone` on `AudioFrame`.** The decode → resample pipelines that
  consume this crate fan one decoded audio frame out to several
  renditions (a 16 kHz and a 48 kHz resampler, say), which needs the
  frame itself to be clonable. Nothing about the clone is expensive by
  construction: an `AudioFrame`'s planes are the adapter's buffer type,
  and for the FFmpeg adapter that clone is an `av_buffer_ref` refcount
  bump. The sibling adapter's error types gain `Clone` in the same
  release — see
  [`mediadecode-ffmpeg` 0.3.1](../mediadecode-ffmpeg/CHANGELOG.md#031---2026-06-14).

[0.3.1]: https://github.com/findit-studio/mediadecode/releases/tag/mediadecode-v0.3.1

## [0.3.0] - 2026-06-07

The shared vocabulary crate is renamed: `videoframe` 0.2 became
`mediaframe` 0.1 when its charter broadened from pixel/frame to all
media-stream vocabulary, and the old `videoframe` 0.x line is yanked.
mediadecode flips in lockstep
([#7](https://github.com/findit-studio/mediadecode/pull/7),
[#8](https://github.com/findit-studio/mediadecode/pull/8)). Because the
re-exported types *are* mediadecode's public surface, a rename upstream
is a break here even where this crate's own spellings do not move.

### Changed (BREAKING)

- **`videoframe` 0.2 → `mediaframe` 0.1.** Every re-export in
  `color`, `cfa`, `pixel_format` and `frame` now resolves to a
  `mediaframe` type. The import paths callers write are unchanged
  (`mediadecode::color::ColorMatrix`, `mediadecode::PixelFormat`, …),
  but the **type identity** behind them is a different crate, so a
  value obtained from `videoframe` 0.2 no longer type-checks here.
- **`frame::Plane::data()` is renamed `data_ref()`**, tracking
  mediaframe's `_ref` getter-suffix convention.
- **The `Color*` names are now aliases.** Upstream renamed
  `Color{Matrix,Primaries,Transfer,Range,Info}` to
  `{Matrix,Primaries,Transfer,DynamicRange,Info}`; mediadecode keeps
  the disambiguated spellings as re-export aliases
  (`DynamicRange as ColorRange`, `Info as ColorInfo`, …) so its own
  surface and its consumers stay source-compatible. Callers naming the
  upstream types directly see the new names.
- **`ColorMatrix::default()` is `Unspecified`**, was `Bt709` — an
  upstream default that this crate re-exports rather than defines.
- **`ColorTransfer::Bt470M` / `Bt470Bg` are renamed `Gamma22` /
  `Gamma28`**, again upstream. The wire mapping is untouched: the same
  H.273 codes, and the FFmpeg adapter still maps `AVCOL_TRC_GAMMA22` /
  `AVCOL_TRC_GAMMA28` to them.

### Changed

- Version bumped to 0.3.0 — pre-1.0 SemVer puts a breaking change in
  the minor. All three workspace members move together.

[0.3.0]: https://github.com/findit-studio/mediadecode/releases/tag/mediadecode-v0.3.0

## [0.2.0] - 2026-05-15

The shared pixel-vocabulary layer (`color`, `cfa`, `pixel_format`,
frame primitives) now lives in the dedicated
[`videoframe`](https://crates.io/crates/videoframe) crate, so colconv,
mediadecode, and scenesdetect share a single canonical definition of
these types. mediadecode keeps the decoder-output story (timestamped
frames + per-backend extras) — pixel and color vocabulary are
re-exports.

### Changed (BREAKING)

- **`PixelFormat::Unknown` shape**: now `Unknown(u32)` (tuple variant
  carrying the raw wire identifier) instead of the prior unit
  variant. Lossless round-trip via `from_u32` / `to_u32`. Callers
  matching the variant must switch from `PixelFormat::Unknown` to
  `PixelFormat::Unknown(_)` (or `Unknown(raw)` if the raw value is
  useful). Boundary adapters (`mediadecode-ffmpeg`,
  `mediadecode-webcodecs`) have been updated to preserve the raw
  FFmpeg / WebCodecs identifier through the cast.
- **`FrameError` variants** are now newtype-tuple form wrapping
  payload structs (matches the convention in
  [`videoframe`](https://crates.io/crates/videoframe)). Affected
  variants: `TooManyVideoPlanes`, `TooManyAudioPlanes`. Callers
  destructuring `Err(FrameError::TooManyVideoPlanes { plane_count })`
  must switch to `Err(FrameError::TooManyVideoPlanes(p))` and call
  `p.plane_count()`. The payload structs
  ([`frame::TooManyVideoPlanes`](https://docs.rs/mediadecode/0.2/mediadecode/frame/struct.TooManyVideoPlanes.html),
  [`frame::TooManyAudioPlanes`](https://docs.rs/mediadecode/0.2/mediadecode/frame/struct.TooManyAudioPlanes.html))
  carry the same `plane_count: u8` and expose it via a
  `pub const fn plane_count(&self) -> u8` accessor. Both variants
  also carry `#[from]`, so `impl From<TooManyVideoPlanes> for FrameError`
  / `impl From<TooManyAudioPlanes> for FrameError` are auto-generated
  — inner helpers returning `Result<_, TooManyVideoPlanes>` can be
  `?`-propagated directly into `FrameError`.
- **`PixelFormat` enum body**: now sourced from
  [`videoframe::pixel_format::PixelFormat`](https://docs.rs/videoframe/0.2/videoframe/pixel_format/enum.PixelFormat.html)
  and covers **every** FFmpeg `n8.1` `AVPixelFormat` slug (~270 variants,
  closed against FFmpeg's vendored slug list via `cargo xtask check`)
  plus cinema-RAW additions. The previously-shipped subset (NV12, P010
  / P012 / P016, P210 / P212 / P216, P410 / P412 / P416, YUV420P, RGB24,
  …) is a strict subset of the new set, so most existing match arms
  still resolve; matches that relied on the enum being closed at the
  prior list will need updating (FFmpeg-derived sources now feed
  variants like `Yuv411p`, `Yuv410p`, `Yuv440p`, `Y210`, `V210`,
  `Xv36`, `Vuya`, `Bayer*`, `Xyz12`, etc.).

### Changed

- **`mediadecode::color::*`** (`ColorMatrix`, `ColorPrimaries`,
  `ColorTransfer`, `ColorRange`, `ChromaLocation`, `ColorInfo`,
  `DcpTargetGamut`) now re-export from `videoframe::color::*`. Public
  import paths (`mediadecode::color::ColorMatrix`, etc.) keep
  resolving — no source-level break for consumers.
- **`mediadecode::cfa::BayerPattern`** re-exports from
  `videoframe::frame::BayerPattern` (videoframe 0.2 dropped its
  separate `cfa` module; the type lives under `frame::bayer` and is
  re-exported via `frame::*`).
- **`mediadecode::frame::{Dimensions, Rect, Plane}`** re-export from
  `videoframe::frame::*`. The structural primitives are now the
  canonical videoframe definitions; the type identity is
  cross-crate-equal so values can flow without conversion.
- **Decoder-output types unchanged.** `VideoFrame<P, E, D>`,
  `AudioFrame<S, C, E, D>`, `SubtitleFrame<E, D>` remain in
  mediadecode — they carry timestamp + backend-extras, which sit
  above the pure pixel-vocabulary layer.

### Added

- **`videoframe`** as a new required dep (`videoframe = "0.2"`).
  Enabled with `features = ["frame"]` so every per-family pixel-format
  borrow type is available to downstream consumers.
- **`#[must_use]`** on every consuming `with_*` builder method
  across frame / packet / subtitle types. Catches accidental
  discards of the returned value at compile time.
- **`VideoFrame::try_new`** / **`AudioFrame::try_new`** —
  panic-free constructors returning `Result<Self, FrameError>`.
  The existing `new` constructors keep their panicking behavior
  for `const fn` / statically-known call sites; `try_new` is for
  runtime-checked callers (e.g. backend adapters validating
  decoder output). Pairs the `new` / `try_new` convention the
  rest of the crate already follows
  (`Plane::new` / `Plane::try_new`, `*_empty` / `try_*_empty`,
  …).
- **`mediadecode::frame::FrameError`** — enum capturing the
  validation failures the `try_new` constructors can surface
  (`TooManyVideoPlanes` / `TooManyAudioPlanes`). `non_exhaustive`,
  `IsVariant`, `thiserror::Error`.

### Fixed

- **`plane_count` validated against the fixed plane-array
  capacity.** `VideoFrame::new` asserts `plane_count <= 4`,
  `AudioFrame::new` asserts `plane_count <= 8`. Previously,
  out-of-range values would panic later inside `planes()` /
  `samples()`; now they fail-fast at construction.
  Closes [issue #4 — finding 1](https://github.com/findit-studio/mediadecode/issues/4).

[0.2.0]: https://github.com/findit-studio/mediadecode/releases/tag/mediadecode-v0.2.0

## [0.1.0] - 2026-05-09

Initial public release.

### Added

- **Core enums.** `PixelFormat` (closed enum covering CPU and HW-tile
  formats: NV12, P010 / P012 / P016, P210 / P212 / P216, P410 / P412 /
  P416, YUV420P, RGB24, …), `SampleFormat`, `AudioChannelLayout`, and
  `BayerPattern` for RAW.
- **Color metadata.** H.273-aligned `ColorMatrix`, `ColorPrimaries`,
  `ColorTransfer`, `ColorRange`, `ChromaLocation`, plus the bundled
  `ColorInfo` type with `const fn` getters / `with_*` builders /
  `set_*` mutators.
- **Generic packet types.** `VideoPacket<A, B>`, `AudioPacket<A, B>`,
  `SubtitlePacket<A, B>` with the `PacketFlags` bitflags
  (`KEY` / `CORRUPT` / `DISCARD`).
- **Generic frame types.** `VideoFrame<A, B>`, `AudioFrame<A, B>`,
  `SubtitleFrame<A, B>`, alongside the `Plane<B>` plane carrier, the
  `Rect` rectangle, and the alloc-gated `SubtitlePayload<B>::Bitmap`
  variant.
- **Adapter traits.** `VideoAdapter`, `AudioAdapter`,
  `SubtitleAdapter` — fix the `extras` and `buffer` types for a
  whole pipeline once.
- **Decoder traits.** `VideoStreamDecoder`, `AudioStreamDecoder`,
  `SubtitleStreamDecoder` (push-style `send_packet` / `receive_frame`
  / `send_eof` / `flush` shape) plus `VideoFrameSource` /
  `AudioFrameSource`.
- **Time primitives.** `Timebase`, `Timestamp`, `TimeRange` re-exported
  from [`mediatime`](https://crates.io/crates/mediatime) so consumers
  don't need a separate dependency.
- **API style.** All public fields private; access via `field()`
  getters, consuming `with_field(value)` builders, and `set_field`
  mutators returning `&mut Self`. `const fn` everywhere the type
  allows. Panicking constructors paired with fallible `try_*`
  counterparts.
- **`no_std` core.** Builds without `std` or `alloc`; opt-in `alloc` /
  `std` features. Errors via `thiserror` over the stable
  `core::error::Error`, so `Error` impls survive
  `--no-default-features`.
- **Optional features.** `serde`, `arbitrary`, `quickcheck` (each
  forwards to `mediatime`'s matching feature).

[0.1.0]: https://github.com/findit-studio/mediadecode/releases/tag/mediadecode-v0.1.0
