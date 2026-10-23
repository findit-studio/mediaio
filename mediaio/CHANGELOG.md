# Changelog

All notable changes to the [`mediaio`](https://crates.io/crates/mediaio)
crate are documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this crate adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
The family releases in lockstep.

## [Unreleased]

## [0.21.0] - unreleased

### Changed

- **BREAKING: the `ffmpeg` feature's adapters are `mediaio-ffmpeg`,
  `mediadecode-ffmpeg` and `mediaencode-ffmpeg` 0.21.** `ffmpeg::decode`
  is `mediadecode-ffmpeg` 0.21: its hardware probe ends in
  `ProbeExhausted` (it was `AllBackendsFailed`), a backend whose download
  chooses a layout a `Frame` cannot carry is unfit for the stream rather
  than failed, and the video decoders take `DecoderOptions` through open
  requests. See its changelog. The default, pure-Rust build is untouched.

## [0.20.0] - unreleased

### Changed

- **BREAKING: the `ffmpeg` feature needs FFmpeg 9 or newer.** Its adapters
  are `mediaio-ffmpeg`, `mediadecode-ffmpeg` and `mediaencode-ffmpeg`
  0.20, whose base refuses an older FFmpeg by name in its build script. The
  default, pure-Rust build is untouched.
- **BREAKING: `timeline` is `mediaio-timeline` 0.3** (`mediaio::timeline`):
  markers, `otio::from_otio`, and an OpenTimelineIO export that now carries
  the exact-readback metadata.

## [0.19.0] - 2026-10-16

### Added

- **The family in one dependency.** Default features: `core`
  (`mediaspine`, as `mediaio::spine`), `decode` (`mediadecode`),
  `encode` (`mediaencode`), `timeline` (`mediaio-timeline`) — pure Rust,
  no FFmpeg. Opt-in: `ffmpeg` (the base and both FFmpeg adapters, under
  `mediaio::ffmpeg`), `webcodecs`, `time` (`mediatime`) and `frame`
  (`mediaframe`); `render` is reserved.
