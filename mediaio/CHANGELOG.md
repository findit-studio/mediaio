# Changelog

All notable changes to the [`mediaio`](https://crates.io/crates/mediaio)
crate are documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this crate adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
The family releases in lockstep.

## [Unreleased]

### Changed

- **BREAKING: `timeline` is `mediaio-timeline` 0.4** (`mediaio::timeline`):
  markers, `otio::from_otio`, other readers' own metadata
  (`ReaderMetadata`), and an OpenTimelineIO export that now carries the
  exact-readback metadata and spells every number so OpenTimelineIO 0.17's
  reader reads it exactly.

## [0.19.0] - 2026-10-16

### Added

- **The family in one dependency.** Default features: `core`
  (`mediaspine`, as `mediaio::spine`), `decode` (`mediadecode`),
  `encode` (`mediaencode`), `timeline` (`mediaio-timeline`) — pure Rust,
  no FFmpeg. Opt-in: `ffmpeg` (the base and both FFmpeg adapters, under
  `mediaio::ffmpeg`), `webcodecs`, `time` (`mediatime`) and `frame`
  (`mediaframe`); `render` is reserved.
