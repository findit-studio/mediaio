# Changelog

All notable changes to the [`mediadecode-webcodecs`](https://crates.io/crates/mediadecode-webcodecs)
crate are documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this crate adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

The backend-agnostic core it adapts has its own log at
[`mediadecode/CHANGELOG.md`](../mediadecode/CHANGELOG.md).

## [Unreleased]

## [0.19.0] - 2026-10-16

Tracks `mediadecode` 0.19.0 (see
[its entry](../mediadecode/CHANGELOG.md#0190)). The family releases in
lockstep.

### Changed (BREAKING)

- **`Received::Frame` is `Received::Produced`**, with `mediadecode`: the
  async faces answer the vocabulary's own rhythm.

## [0.18.0] - 2026-10-10

Tracks `mediadecode` 0.18.0, whose `PacketFlags` names `DISPOSABLE` (see
[`mediadecode` 0.18.0](../mediadecode/CHANGELOG.md#0180)). The three crates
release in lockstep.

### Changed (BREAKING)

- **`mediadecode` 0.17 → 0.18.** WebCodecs has no counterpart to
  `PacketFlags::DISPOSABLE`: an `EncodedVideoChunk` is `key` or `delta` and
  nothing more. The flag is therefore not passed into a chunk, and this
  backend, which demuxes nothing, never sets it. (On the FFmpeg backend,
  libavformat 9.0.1 sets it from MP4/MOV `sdtp` alone; a Matroska packet
  reads absent, which means unknown.)

## [0.17.0] - 2026-10-10

Tracks `mediadecode` 0.17.0, which crosses `mediatime` 0.4 → 0.5 and
`mediaframe` 0.11 → 0.12 (see
[`mediadecode` 0.17.0](../mediadecode/CHANGELOG.md#0170)). The
workspace's three crates release in lockstep again; this crate skips
0.16, which `mediadecode-ffmpeg` released alone.

### Changed (BREAKING)

- **`mediatime` 0.4 → 0.5, through `mediadecode`.** The `Timebase` and
  `Timestamp` this adapter's API carries (`pts`, `duration`, `time_base`,
  the caller's own pts) are `mediadecode`'s re-exports, so a consumer
  holding a `mediatime 0.4` value no longer type-checks against this
  release. No adapter source line moved.
- **`mediaframe` 0.11 → 0.12**, a direct dependency here (pinned with
  `alloc`) as well as the core's. The channel layout descriptions this
  adapter hands out are `mediaframe`'s, so a consumer holding a
  `mediaframe 0.11` value no longer type-checks against this release.
  0.12's source is 0.11's but for comments, and no adapter source line
  moved.

## [0.15.1] - 2026-10-05

### Changed

- Version alignment with the workspace release 0.15.1 (`mediadecode` 0.15.1); no change of its own.

## [0.15.0] - 2026-09-11

**No changes; released in lockstep.** Tracks `mediadecode` 0.15.0 and
`mediadecode-ffmpeg` 0.15.0, which carry a container's chapter table
across the demux seam — a `demuxer::Chapter` row and a **provided**
`Demuxer::chapters` answering an empty slice — and repair a set of
open-time and timestamp defects a product-wide audit turned up in the
FFmpeg adapter (see
[`mediadecode` 0.15.0](../mediadecode/CHANGELOG.md#0150) and
[`mediadecode-ffmpeg` 0.15.0](../mediadecode-ffmpeg/CHANGELOG.md#0150)).

Nothing reaches this adapter. It implements no `Demuxer` at all — a
WebCodecs `VideoDecoder` / `AudioDecoder` is fed packets a caller has
already demuxed elsewhere — so the new door's provided body is the
whole of this crate's relationship to the release, and it is a door
nothing here can be asked through. Everything else the FFmpeg release
carries lives behind an FFI this crate does not have: container
rationals, `AVDictionary` metadata, `AVCodecContext.pkt_timebase`,
`AVSubtitle`, `AVPacket`.

No adapter source line moved.

## [0.14.0] - 2026-09-02

Tracks `mediadecode` 0.14.0 and `mediadecode-ffmpeg` 0.14.0. Neither
touches a surface this adapter uses. The core release drops the
optional `ingraph` feature whole (see
[`mediadecode` 0.14.0](../mediadecode/CHANGELOG.md#0140)) — this
crate's own `mediadecode` pin requests only `features = ["alloc",
"future"]` and never enabled it. The FFmpeg release fills in
`ScaledOutputCapability`'s VideoToolbox implementation (a
`VTPixelTransferSession`) and a companion MSRV-driven refactor with no
behaviour change (see
[`mediadecode-ffmpeg` 0.14.0](../mediadecode-ffmpeg/CHANGELOG.md#0140));
the capability word itself was already added to
`decoder::VideoStreamDecoder` — the **sync** trait — back in 0.13.0,
and this crate implements only the `future::local` async mirror, which
carries exactly the four methods it always has (`send_packet` /
`receive_frame` / `send_eof` / `flush`). Both changes live one crate
over, on surfaces this adapter does not touch.

No adapter source line moved.

**`mediaframe` 0.9 → 0.10 crosses too**, tracking the core's own pin —
a public dependency whose `audio`, `color`, `frame` and
`pixel_format` types this adapter re-exports and names in its own
signatures. Upstream 0.10.0 is a `pub(crate)` case-sensitivity axis (no
public API moved) and an `other()` fix — every escape constructor now
runs the ignore-case `FromStr` lookup first, and a genuine stranger's
spelling is preserved verbatim rather than ASCII-folded — reachable
only where `Other(SmolStr)` is constructed from an unrecognised
string, which this crate never does: it reads WebCodecs' own typed
enums (`VideoPixelFormat`, `VideoColorPrimaries`,
`VideoTransferCharacteristics`, `VideoMatrixCoefficients`, …) through
hand-written match arms onto the named vocabulary variants — an
unrecognised WebCodecs value is refused with an error, never carried
into `Other`.
No `.other(...)` call and no `FromStr` onto a `mediaframe` vocabulary
type appears anywhere in this crate's source.

Verified on the real dependency graph (native builds this crate
empty): `cargo check` / `cargo clippy --all-features -- -D warnings`
on `--target wasm32-unknown-unknown`, both clean with zero source
change. The browser `wasm-bindgen-test` lane needing headless Chromium
is CI-only and was not run locally.

## [0.13.0] - 2026-09-01

Tracks `mediadecode` 0.13.0 and `mediadecode-ffmpeg` 0.13.0. Neither
touches a surface this adapter uses. The core release removes
`Demuxer::take_tracks` and replaces `Demuxer::tracks`'s return with a
handle (see
[`mediadecode` 0.13.0](../mediadecode/CHANGELOG.md#0130)) and adds
`ScaledOutputCapability` to `decoder::VideoStreamDecoder` — the
**sync** trait. This crate implements neither `Demuxer` nor any
demux-tier type — `WebCodecsVideoStreamDecoder` /
`WebCodecsAudioStreamDecoder` are fed packets by something else — and
it implements only the `future::local` async mirror of the decoder
traits, which carries exactly the four methods it always has
(`send_packet` / `receive_frame` / `send_eof` / `flush`); the new
capability word was not added there. The FFmpeg-only additions
(`DecodePath` / `open_as`, `ContainerFormat`, `CodecId::name` /
`::long_name`, `DolbyVisionConfig`, `mediadecode-ffmpeg`'s own
`scaled_output_capability` impl) live one crate over, on a dependency
this crate does not carry.

No adapter source line moved.

Verified on the real dependency graph (native builds this crate
empty): `cargo check` / `cargo clippy --all-features -- -D warnings`
on `--target wasm32-unknown-unknown`, both clean with zero source
change. The browser `wasm-bindgen-test` lane needing headless Chromium
is CI-only and was not run locally.

## [0.12.0] - 2026-08-30

### Changed (BREAKING)

- **`mediaframe` 0.7 → 0.9**, tracking the core's own crossing (see
  [`mediadecode`](../mediadecode/CHANGELOG.md#unreleased)) — two
  breaking minors in a public dependency whose `PixelFormat`, `color`
  and `audio::ChannelLayoutDescription` types this adapter re-exports
  and names in its own signatures.

  **No adapter source line moved and no behaviour changes.** Upstream
  0.8.0 is additive to the container / audio-container / image roster
  households, which this adapter never names, and 0.9.0 is the `lang`
  family — the retirement of the lossy `lang::Language` triple, its
  `LanguageError` and `audio::Tags`'s language field, none of which
  appears anywhere in this crate.

  Verified on the real dependency graph (native builds this crate
  empty): `cargo check` and `cargo clippy --all-features -- -D
  warnings` on `--target wasm32-unknown-unknown`, both clean with zero
  source change. The browser `wasm-bindgen-test` lane needing headless
  Chromium is CI-only and was not run locally.

## [0.11.0] - 2026-08-28

Tracks `mediadecode` 0.11.0, which crosses `mediatime` 0.3 → 0.4 and
`mediaframe` 0.6 → 0.7 — two breaking minors in public dependencies
whose `PixelFormat` and `color` types this adapter re-exports. See
[`mediadecode` 0.11.0](../mediadecode/CHANGELOG.md#0110). No adapter
source line moved and no adapter behaviour changes — `mediatime` 0.4 is
additive only, and `mediaframe` 0.7 is itself entirely the `mediatime`
crossing.

Verified on the real dependency graph (native builds this crate
empty): `cargo check` / `cargo clippy -- -D warnings --all-features`
on `--target wasm32-unknown-unknown`, both clean. The browser
`wasm-bindgen-test` lane needing headless Chromium is CI-only and was
not run locally.

## [0.10.0] - 2026-08-27

### Changed

- **BREAKING: `receive_frame` returns `mediadecode::Received` and
  `send_packet` / `send_eof` return `mediadecode::Sent`** on both async
  decoder faces. `VideoDecodeError` and `AudioDecodeError` lose their
  `NoFrameReady`, `Eof` and `OutputFull` arms — three of the four
  conditions they named were never faults.

  This adapter had the family's only complete four-arm vocabulary — and
  that was the problem, not the fix: the FFmpeg backend's video and
  audio decoders named none of the four, so a consumer generic over
  `mediadecode`'s traits observed a different protocol depending on
  which backend it held. The vocabulary now lives one tier up, where
  both backends answer it.

- **`wait_for_frame` no longer takes `eof`**, and answers
  `Ok(None)` for "no frame, and nothing in flight that could produce one
  without the caller acting". Which of the two protocol answers that is
  belongs to the session's own EOF latch, which `receive_frame` owns —
  so there is exactly one place in the adapter that decides between
  "send another packet" and "there will be no more", and it is the place
  that holds the fact.

- **`await_decode_room` answers `Sent`**, and the two kinds of pressure
  part company there. The decoder-side cap drains by itself as the
  browser works, so it is still awaited; the *output* cap drains only
  when the caller calls `receive_frame`, which needs the `&mut self`
  that `send_packet` is holding — so awaiting it would deadlock, and it
  is reported as `Sent::MustDrain` instead. That distinction is why the
  arm survives the move to `async` at all.

- **BREAKING: `AtEof` is renamed `AfterEof`.** It **stays** an error —
  a caller usage fault, not back pressure: draining changes nothing, and
  this decoder refuses every packet until `flush()`, so `MustDrain`
  would be a loop with no exit. What changed is only the word. The
  FFmpeg adapter names the identical condition `AfterEof` on its
  resampler and (as of this release) its subtitle seam, and one
  condition wearing two words across two backends of one trait is the
  disease this release is curing.

- **`VideoDecodeError` and `AudioDecodeError` gain `#[non_exhaustive]`**,
  with the family's other seven. This is what the breaking window buys:
  adding the attribute is itself breaking, so it can only ride a release
  that already is, and afterwards every new fault arm is additive
  forever. The status enums stay exhaustive for the opposite reason —
  closed protocol vocabulary against open fault taxonomy.


## [0.9.0] - 2026-08-26

## [0.9.0] - 2026-08-24

### Changed

- Version bump to track `mediadecode` 0.9. **No adapter source
  changed**, and that is the finding rather than an absence of one:
  0.9's [D-slot amputation contract](../mediadecode/CHANGELOG.md) is
  the rule that `WebCodecsBuffer` was already built to — an
  `Arc<Vec<u8>>` view over bytes `copyTo` has already handed to Rust,
  owned, `Send + Sync`, cloning by refcount, with nothing reaching back
  into a JS handle. The FFmpeg adapter had to be rebuilt for that
  release; this one had to be checked, and it passed unchanged.

### Notes

- **Still images: a door, not a gap.** `mediadecode` 0.9 added a
  fourth frame household (`ImageFrame`) and a one-shot `ImageDecoder`
  seam. This crate does not implement them yet, and the absence is a
  schedule rather than a judgement: the browser's answer to "decode
  these bytes into a picture" is `createImageBitmap`, which is not
  part of the WebCodecs surface this crate wraps and carries its own
  async shape and its own pixel-readback problem — an `ImageBitmap`
  has no `copyTo`, so the bytes come back through a canvas or through
  `VideoFrame::new(ImageBitmap)`. Recorded on the crate's module docs
  so the next reader finds the reason where they find the absence.

  The seam this crate will implement when that work happens is
  `mediadecode::future::local::ImageDecoder`, the `async` mirror that
  0.9 also mints — `createImageBitmap` returns a `Promise`, so the
  sync face was never the one on offer here.

## [0.8.0] - 2026-08-24

## [0.7.0] - 2026-08-23

## [0.6.0] - 2026-08-21

### Changed (BREAKING)

- **`AudioAdapter::ChannelLayout` binds
  `mediaframe::audio::ChannelLayoutDescription`.** `mediadecode::channel`
  is gone (see
  [`mediadecode`](../mediadecode/CHANGELOG.md#unreleased)) and the
  channel vocabulary lives in `mediaframe::audio` now. The adapter's
  conduct is unchanged: WebCodecs reports a channel count and no layout
  name, so `boundary::empty_audio_frame` and the decode path still
  build `ChannelLayoutDescription::new(N)` — the count with an
  `Unspecified` order — and `default()` is still the zero-channel
  placeholder.

- **`mediaframe` is a direct `wasm32` dependency**, pinned with
  `alloc`: the audio household is compiled only at the alloc-or-std
  tier.

## [0.5.0]

Tracks `mediadecode` 0.5.0, which crosses `mediaframe` 0.4 → 0.5 — a
breaking minor in a public dependency whose `PixelFormat` and `color`
types this adapter re-exports. See
[`mediadecode` 0.5.0](../mediadecode/CHANGELOG.md#050). No adapter
source line moved and no adapter behaviour changes.

## [0.4.0] - 2026-08-19

Tracks `mediadecode` 0.4.0, which crosses `mediatime` 0.1 → 0.3 and
`mediaframe` 0.1 → 0.3 — two breaking minors each. See
[`mediadecode` 0.4.0](../mediadecode/CHANGELOG.md#040). No adapter
behaviour changes.

### Changed (BREAKING)

- **The empty destination frame's pixel format is now
  `PixelFormat::None`.** `boundary::empty_video_frame` filled the
  placeholder with `PixelFormat::Unknown(0)`; mediaframe 0.3 struck
  that variant, and `None` is its named "no format yet" member (and
  the `Default`) — which is what the placeholder always meant. The
  adapter overwrites it on every successful decode, as before.
- **`WebCodecsVideoFrame::format` and `::color` are no longer
  `const`** and return clones. `mediaframe::PixelFormat` and
  `mediaframe::color::Info` lost `Copy` in 0.3. Both signatures are
  otherwise unchanged.
- **`Timebase` construction is signed.** `mediatime` 0.2 made
  `Timebase`'s numerator and denominator `i32` / `NonZeroI32`; the
  crate's two `MICROS` constants and the wasm integration tests move
  from `NonZeroU32` to `NonZeroI32`.

### Changed

- **`mediadecode` dep**: bumped to `0.4`.
- `video::expected_plane_layout` (private) takes `&PixelFormat`.
- README: the install snippet said the crate was unpublished and
  pinned `"0.0"`; it has been on crates.io since 0.3.1. Now `"0.4"`.

[0.4.0]: https://github.com/findit-studio/mediadecode/releases/tag/mediadecode-webcodecs-v0.4.0

## [0.3.1] - 2026-06-14

Version-tracking release
([#10](https://github.com/findit-studio/mediadecode/pull/10)). No adapter
source changed.

### Changed

- **`mediadecode` dep**: bumped to `0.3.1`, which adds `Clone` to
  `AudioFrame` — see
  [`mediadecode` 0.3.1](../mediadecode/CHANGELOG.md#031---2026-06-14).

[0.3.1]: https://github.com/findit-studio/mediadecode/releases/tag/mediadecode-webcodecs-v0.3.1

## [0.3.0] - 2026-06-07

Tracks `mediadecode` 0.3.0, which flips the shared vocabulary crate from
`videoframe` 0.2 to `mediaframe` 0.1
([#7](https://github.com/findit-studio/mediadecode/pull/7),
[#8](https://github.com/findit-studio/mediadecode/pull/8)). No adapter source
changed — but the vocabulary this crate re-exports through its frame
types is a different crate's now, so the release is breaking for its
consumers all the same. See
[`mediadecode` 0.3.0](../mediadecode/CHANGELOG.md#030---2026-06-07).

### Changed (BREAKING)

- **`mediadecode` dep**: bumped to `0.3`.

### Changed

- Version bumped to 0.3.0 with the rest of the workspace.

[0.3.0]: https://github.com/findit-studio/mediadecode/releases/tag/mediadecode-webcodecs-v0.3.0

## [0.2.0] - 2026-05-15

Tracks the `mediadecode` 0.2.0 / `videoframe` 0.2 cutover
([#5](https://github.com/findit-studio/mediadecode/pull/5)).

### Changed

- The `PixelFormat::Unknown` boundary fallback in
  `webcodecs_pixel_format_to_mediadecode` preserves the raw
  WebCodecs identifier via `PixelFormat::Unknown(raw as u32)`
  instead of collapsing to a unit variant.

### Added

- `#[must_use]` added to every `with_*` consuming builder method.
- New `tests/native_stub.rs` — verifies the crate compiles to an
  empty stub on non-wasm32 targets and that no wasm-only names
  leak through. Closes
  [issue #4 — finding 4](https://github.com/findit-studio/mediadecode/issues/4).

[0.2.0]: https://github.com/findit-studio/mediadecode/releases/tag/mediadecode-webcodecs-v0.2.0

## [0.0.0] - 2026-05-15

Placeholder publish — the name is reserved and the shape is scaffolded;
there is no public API yet
([#3](https://github.com/findit-studio/mediadecode/pull/3)).

### Added

- Crate scaffolded: workspace member, `wasm32`-gated `web-sys`
  dependency surface, design spec captured in
  `docs/superpowers/specs/2026-05-09-webcodecs-design.md`.
  Public API lands in a subsequent release.

[0.0.0]: https://github.com/findit-studio/mediadecode/releases/tag/mediadecode-webcodecs-v0.0.0
