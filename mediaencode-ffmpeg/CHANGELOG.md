# Changelog

All notable changes to the [`mediaencode-ffmpeg`](https://crates.io/crates/mediaencode-ffmpeg)
crate are documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this crate adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
The FFmpeg-dependent crates — `mediaio-ffmpeg`, `mediadecode-ffmpeg`,
`mediaencode-ffmpeg` and `mediaio` — release in lockstep; every other
crate moves only when it changes.

## [0.21.1] - unreleased

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
  passed through. Refused, the packet handed back untouched: a
  subtitle's negative duration outside Matroska and WebM, which
  libavformat hands the container as it is; MP4's duration, decode gap
  and composition offset past `i32::MAX` ticks, the gap judged in
  `i128`; a reordering stream's packet without a presentation time; a
  time libavformat's first write leaves before the earliest the container
  writes, and every such time where the muxer does not mirror that
  write's move of every stream's times; an FLV packet decoded before the
  first one FLV wrote (`FfmpegMuxRefusal::BeforeFirstDts`); an MP4 `mov_text` cue before zero, or before the
  last cue's end where another track's packet can make MP4 mark that end
  first; and a
  packet's bytes or side data the container would refuse or write
  otherwise — an empty packet it drops (a configuration's packet — no
  payload, new extradata — MP4 takes for MPEG-4 ALS, AAC, AV1 and FLAC,
  Matroska for AAC and FLAC: taken, no sample counted), AAC in an ADTS header on a
  configured track no filter converts, partial PCM,
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
  durations, where the samples it writes do not abut, and where a
  `mov_text` cue starting before the last one's end shortens it; a
  negative duration libavformat or the container writes as zero; a
  packet not marked a key frame that libavformat or the container writes
  as one (`Rewrite::KeyFramesMarked`) — libavformat for a codec whose
  descriptor counts every frame one, by the library's own question — and
  libavformat's move of every stream's times at its first write, where a
  packet lies before the earliest the container writes
  (`Uncarried::TimesShifted`).
- **The automatic filters, on by default, each reported.** libavformat's
  automatic bitstream filters run as `ffmpeg -c copy` runs them —
  `aac_adtstoasc` for MP4's and Matroska's AAC framed in ADTS,
  `vp9_superframe` for VP9 in MP4, Matroska and WebM, `pgs_frame_merge`
  for Matroska's PGS, and every other filter a muxer inserts —
  `pcm_rechunk`, `av1_metadata`, `eia608_to_smpte436m`,
  `extract_extradata` and the Annex B conversions — each named for its
  track (`Uncarried::Filtered`, `AutomaticFilter`): at open where open
  decides it, else at the write of the first packet, which inserts it. The
  first packet decides: libavformat inserts a track's filter at its first
  packet, and the muxer admits on an AAC track with no configuration only
  what the filter converts, refusing by name what it would refuse after
  taking it (`PayloadRule::AdtsFrame`, `Superframe`, `PgsSegments`).
  Every mirror of libavformat's timing and the container's moves on what
  a filter puts out now, in its times, never on what it takes: a packet
  the filter holds meets no rule of its times until a packet put out
  carries them. PGS's open display sets the muxer holds itself and hands
  to libavformat whole at the packet that completes them; one whose
  completion is refused stays held, and `CarrierMuxer::take_held` hands
  it back. A caller turns the filters off with
  `MuxOptions::set_automatic_filters`, and the refusals by name apply
  instead.
- **MP4 is judged by the samples it writes.** `check_pkt` counts a
  packet's gap from the last sample MP4 wrote on the track, which an
  E-AC-3 frame MP4 holds to join into a sample of six blocks does not
  move; the joined sample's bytes are held within `av_grow_packet`'s bound
  (`PayloadRule::Ac3SampleSize`), and a last joined sample past `i32::MAX`
  ticks, which MP4's trailer asserts on, is refused by `finish`
  (`FfmpegMuxRefusal::LastSampleTooLong`). A `mov_text` cue starting
  before the last one's end is written as MP4 writes it.
- **Packet side data, by the side-data table.** What each container
  writes of a packet's side data, from its source: a kind it has no place
  for, or keeps only in part, is reported for its track by name the first
  time a packet carrying it is written (`Uncarried::SideData`,
  `SideDataFate::Dropped`) — MP4's new extradata among them where the
  codec's sample entry reads none of it: AC-3's, E-AC-3's, APV's, PCM's,
  TTML's and GoPro metadata's — and a kind whose fate the source does not
  settle as possibly dropped (`SideDataFate::PossiblyDropped`).
- **The report table.** What a road keeps of a track's filename, MIME
  type, language and each disposition flag, per container, is one
  table the report and the stream's disposition both read: Matroska's
  text descriptions on a subtitle track alone; a language Matroska or
  WebM writes as `und` or reads back as none, and an MP4 language whose
  packed code does not unpack to itself, reported; no flag the report
  names as dropped handed to libavformat, so MP4 makes no cover of a
  timed track.
- **Each container's checks of each codec are made first** (table C of
  the census). Into MP4: an AC-3 track's first frame — the one MP4 writes
  `dac3` from at the trailer — of a bit stream id past 8, refused at that
  frame (`PayloadRule::Ac3BitStreamId`); an AC-3 or E-AC-3 track that took
  no frame, refused by `finish` before the trailer would fail the file
  (`FfmpegMuxRefusal::NoFrame`, carried by the fault's new
  `MuxFault::refusal`; `MuxFault::error` now answers an `Option`); AV1,
  FLAC, Opus, ALAC and MPEG-4 ALS with no configuration, which MP4 writes
  empty or refuses at the trailer (`MuxOpenError::ConfigurationMissing`);
  FLAC's configuration not one 34-byte STREAMINFO, Opus's not an OpusHead
  `dOps` takes, and the header strings of libavcodec's TTML encoder cut
  short (`MuxOpenError::ConfigurationInvalid`); frame cropping that leaves
  no picture (`MuxOpenError::Cropping`); new extradata MP4 would write as a
  sample description its trailer refuses, or of another form than the
  track's — Annex B or not — which turns the rewrite on or off
  (`PayloadRule::NewExtradata`; FLAC's of another length,
  `PayloadRule::FlacStreamInfo`); an E-AC-3 first packet of more than 15
  dependent frames. Into Matroska: AAC's new extradata that libavcodec
  does not parse, or larger than the space its header reserved, or any
  where the header reserved no output sampling frequency, which the
  update would write over the track entry (`PayloadRule::NewExtradata`).
- **What a container adds, reinterprets or drops of the table is
  reported** (table E). The flags a track reads back with that it did not
  declare (`Uncarried::DispositionAdded`): the default MP4 sets on the
  first video, audio and subtitle track of each type where none declares
  it; the partner of each DASH role pair MP4 writes as one — hearing
  impaired and captions, visual impaired and descriptions; the attached
  picture an MP4 cover, and a Matroska attachment of an image MIME type,
  reads back as. A chapter's id a container does not keep
  (`Uncarried::ChapterId`) and its span where an instant moves
  (`Uncarried::ChapterSpan`); an MP4 chapter without a title, or with one
  past its 16-bit length, refused at open (`MuxOpenError::ChapterWithoutTitle`,
  `ChapterTitleTooLong`). An attachment of no bytes, which Matroska's
  reader drops and MP4 skips, is not written, and reported.
- **The sizes libavformat counts in 32 bits are counted first, wide.**
  VP9's superframe — the hidden frames the automatic filter holds and the
  frame shown that joins them, which it sums in 32 bits and allocates by
  — is refused at the frame that decides it would pass libavcodec's
  largest packet (`PayloadRule::SuperframeSize`; past 4 GiB the filter's
  sum wrapped and its copy ran past the allocation); in another container,
  where the filter may run, a VP9 frame larger than a seventh of it. A PGS
  display set the filter would grow past it (`DisplaySetSize`); a packet
  whose Annex B rewrite would pass it (`RewrittenSize`); a Matroska block
  larger than half of what a cluster's buffer holds beside 5 MiB, which
  Matroska would write cut (`ClusterSize`); an MP4 TTML paragraph that
  would take the document MP4 joins at the trailer past it
  (`DocumentSize`).
- **HDR metadata side data is checked exactly before Matroska casts it.**
  On a Matroska or WebM video track, an HDR10+ or SMPTE ST 2094 APP5
  entry is admitted only as the linked libavutil's struct, whole, that its
  serializer writes and its reader reads back as itself
  (`mediaio_ffmpeg::hdr_metadata`); any other is refused by name, the
  packet handed back (`PayloadRule::DynamicHdrPlus`, `DynamicHdrApp5`).
  The writer cast an undersized entry to the struct and read past it.
  Two side-data entries of one kind, which libavcodec folds into one, are
  refused in every container (`PayloadRule::SideDataRepeated`).
- **AAC's program config element is bound-parsed.** On MP4's and
  Matroska's AAC with no configuration, the first ADTS frame whose
  channels are its program config element's is admitted only where the
  whole element, its comment included, ends before its payload does: the
  automatic filter consumes it with no check, leaving a negative or empty
  packet (`PayloadRule::AdtsFrame`).
- **A track's kind is judged against its media type** before any stream
  is added (`MuxOpenError::MediaTypeMismatch`): a timed track's against
  its ticket's media type and its codec's own descriptor; an attachment's
  against an attachment stream's or an attached picture's.
- **The muxing context's vocabulary is this crate's public face.**
  `OutputCall`, `OutputError`, `OutputPhase` and `OutputText` are
  re-exported from `mediaio-ffmpeg`'s `output` module, which that crate
  hides from its documentation; the FFmpeg crates move in lockstep, and
  the hidden marking there licenses no change this crate's face would
  carry unmarked — a change to them is a change to this crate's API.
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
