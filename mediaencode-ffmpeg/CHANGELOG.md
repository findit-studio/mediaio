# Changelog

All notable changes to the [`mediaencode-ffmpeg`](https://crates.io/crates/mediaencode-ffmpeg)
crate are documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this crate adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
The FFmpeg-dependent crates — `mediaio-ffmpeg`, `mediadecode-ffmpeg`,
`mediaencode-ffmpeg` and `mediaio` — release in lockstep; every other
crate moves only when it changes.

## [Unreleased]

### Added

- **The muxer**: `FfmpegMuxer` (view lane) and `FfmpegOwnedMuxer` (owned
  lane) implement `Muxer` over libavformat for any container it names
  (`ContainerName`). `open` takes the sink, the container, the track
  table with each track's `CodecTicket`, the chapters and the packet
  limits, refuses by name what the container cannot take
  (`MuxOpenError`) and reports what it drops (`MuxReport`, `Uncarried`);
  `open_with` takes `MuxOptions` — the packet limits and whether
  libavformat's automatic bitstream filters run — and the muxer's own
  `report` grows with what it learns writing.
  Matroska keeps attachments with their filename and MIME type, MP4 keeps
  cover art; the report follows each track's road, naming the flags an
  attachment loses in Matroska and the filename, MIME type, language and
  flags a cover loses in MP4. Writes judge presentation
  order on the counts libavformat is handed, and refuse a strict
  container's repeated decode time, and a time the stream cannot count —
  one that lands on `i64::MIN`, libavformat's no-timestamp value, among
  them — by the muxer's own name (`FfmpegMuxRefusal`, through
  `MuxError::Backend`), handing the packet back; faults name their stage
  (`MuxFault`), a sink's panic included.
- **The packet-rule table.** Every rule libavformat applies on the
  packet path in Matroska, WebM and MP4, at FFmpeg 9.0.1, is refused by
  name before libavformat is handed the packet, reported at open, or
  passed through. Refused, the packet handed back untouched: a negative
  duration; MP4's duration, decode gap and composition offset past
  `i32::MAX` ticks, the gap judged in `i128`; a reordering stream's
  packet without a presentation time; a time before the earliest the
  container writes; an MP4 `mov_text` cue out of order; an unmarked key
  frame of a codec of key frames alone (`FfmpegMuxRefusal`); and a
  packet's bytes or side data the container would refuse or write
  otherwise — an empty packet it drops, AAC in an ADTS header on a track
  whose configuration is stated, partial PCM,
  AV1 OBUs or WavPack blocks that do not parse, MP4 skip-samples below
  nothing, Matroska's new FLAC STREAMINFO of another length, a ProRes
  packet off its atom, a WebVTT cue that would not read back, an AC-3
  frame MP4 would drop or refuse (`FfmpegMuxRefusal::Payload`,
  `PayloadRule`). Refused at open: a track whose configuration the
  container needs and the track lacks (`MuxOpenError::ConfigurationMissing`),
  AAC's only with the automatic filters off; TrueHD into MP4, and ATRAC3,
  Cook, RealAudio 2.0, SIPR and RealVideo 1.0, 2.0 and 3.0 into Matroska,
  whose headers refuse them (`MuxOpenError::CodecNotCarried`); MP4 PCM of
  a channel layout its `chnl` box cannot write, an unspecified one among
  them, which would fail the trailer (`MuxOpenError::ChannelLayout`).
  Reported at open (`Uncarried::Rewritten`, `Rewrite`): length prefixes
  for Annex B, filtered OBUs, MP4's joined E-AC-3, `mov_text` cue ends and
  joined TTML paragraphs, sync from the bitstream, Matroska's and WebM's
  durations without a default duration, Matroska's stripped WavPack
  headers. Reported while writing (`CarrierMuxer::report`): MP4's
  durations, where a track's packets do not abut.
- **The automatic filters, on by default, each reported.** libavformat's
  automatic bitstream filters run as `ffmpeg -c copy` runs them —
  `aac_adtstoasc` for MP4's and Matroska's AAC framed in ADTS with no
  configuration, `vp9_superframe` for VP9 in MP4, Matroska and WebM,
  `pgs_frame_merge` for Matroska's PGS — each named for its track at open
  (`Uncarried::Filtered`, `AutomaticFilter`). The first packet decides:
  libavformat inserts a track's filter at its first packet, and the muxer
  admits there only what the filter converts, refusing by name what it
  would refuse after taking it (`PayloadRule::AdtsFrame`, `Superframe`,
  `PgsSegments`). A packet the filter holds meets no rule of its times. A
  caller turns them off with `MuxOptions::set_automatic_filters`, and the
  refusals by name apply instead.
- **Packet side data, by the side-data table.** What each container
  writes of a packet's side data, from its source: a kind it has no place
  for, or keeps only in part, is reported for its track by name the first
  time a packet carrying it is written (`Uncarried::SideData`,
  `SideDataFate::Dropped`), and a kind whose fate the source does not
  settle as possibly dropped (`SideDataFate::PossiblyDropped`).
- **The report table.** What a road keeps of a track's filename, MIME
  type, language and each disposition flag, per container, is one
  table the report and the stream's disposition both read: Matroska's
  text descriptions on a subtitle track alone; a language Matroska or
  WebM writes as `und` or reads back as none, and an MP4 language whose
  packed code does not unpack to itself, reported; no flag the report
  names as dropped handed to libavformat, so MP4 makes no cover of a
  timed track.
- The encoders arrive in the following releases.

## [0.21.0] - 2026-10-24

### Changed

- Released in lockstep with the FFmpeg crates at 0.21.0, on
  `mediaio-ffmpeg` 0.21.0. Nothing in this crate changed.

## [0.20.0] - 2026-10-23

### Changed

- **BREAKING: FFmpeg 9 is the floor.** This crate builds against FFmpeg 9
  or newer and no older release: `mediaio-ffmpeg` 0.20.0, the base it is
  built on, refuses an older FFmpeg by name in its build script, from the
  release `ffmpeg-sys-next` reads off the installed headers.

## [0.19.0] - 2026-10-16

### Added

- **The encoder-by-name catalogue**, the gate every FFmpeg encoder will
  be opened through: `lookup` checks a closed catalogue and the license
  gate without asking FFmpeg anything, and `encoder_by_name` then asks
  the linked libavcodec, answering an `Admitted` encoder or an
  `EncoderRefusal` by name.
- **The `gpl` feature, off by default.** Without it the GPL encoders
  (`libx264`, `libx264rgb`, `libx265`) are refused by name before
  libavcodec is asked for them. Linking a GPL FFmpeg makes a binary GPL
  whatever this feature says; the README states it.
- The muxer and the encoders arrive in the following releases.
