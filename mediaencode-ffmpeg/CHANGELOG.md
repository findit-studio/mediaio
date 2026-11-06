# Changelog

All notable changes to the [`mediaencode-ffmpeg`](https://crates.io/crates/mediaencode-ffmpeg)
crate are documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this crate adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
The FFmpeg-dependent crates — `mediaio-ffmpeg`, `mediadecode-ffmpeg`,
`mediaencode-ffmpeg` and `mediaio` — release in lockstep; every other
crate moves only when it changes.

## [0.22.0] - unreleased

### Added

- **The write audit** (findit-studio/mediaio#95): what FFmpeg wrote, read
  as the caller's sink takes it — the base's tap on the writer guard,
  walking the file in constant memory, keeping no byte and allocating
  nothing per packet — and judged at `finish` against what the muxer
  handed it. A difference — an omission or a misreport the census names,
  which FFmpeg writes where an allocation fails and the failure is
  swallowed — fails the file by name: the output is abandoned and the sink
  not handed back (`MuxFault::written_otherwise`, `WrittenOtherwise` of a
  track, a `WrittenPart` and what was `Found` there — `Counts`, or where
  the walk could not follow the writer, `Unfollowed` and its `Why`). The
  MOV family's MP4, MOV and iPod first: each `trak`, its id, its `stts`
  and `stsz`; movenc's own chapter track; the `mdat`'s size against the
  payload the sink took; each sample description's configuration record
  — `avcC`, `hvcC` and `lhvC`, `vvcC`, `evcC`, `lvcC`, `av1C`, `dvc1`,
  `apvC` — present and as long as its writer makes it, E-AC-3's `dec3` in
  MOV's `wave`, and DVD subtitles' palette, each named by its index
  (`WrittenPart::Configuration`, `WrittenPart::Palette`). A `mov_text`
  track's tables are read by one rule on both sides, which the end samples
  libavformat's interleaving makes movenc write leave unchanged (the
  coordinator's ruling R5). With the new `test-support` feature,
  forwarding to the base's: `CarrierMuxer::open_hooked` and `OpenHooks`,
  what a law runs inside the open — around the header, or a plant over
  the bytes the sink takes; hidden: `MuxOptions::with_audit`,
  `CarrierMuxer::audited_bytes`, `expected_records`, `expected_palettes`
  and `expected_chapter_samples`, the laws' seams.
- **The muxer**: `FfmpegMuxer` (view lane) and `FfmpegOwnedMuxer` (owned
  lane) implement `Muxer` over libavformat for the containers this
  release audited and keeps (`ContainerName`): the MOV family's MP4, MOV,
  iPod and AVIF, Matroska and WebM, MPEG-TS, FLV, IVF, WAV and Wave64,
  and the raw streams of ADTS AAC, AC-3, E-AC-3, DTS, FLAC, H.264, HEVC
  and AV1 — Ogg, MP3 and raw MP2, audited, withheld by name and the
  reason until the project's own Ogg and MPEG audio writers land
  (`MuxOpenError::ContainerWithheld`, `Withheld`), and any other
  libavformat names refused at open by the muxer's name
  (`MuxOpenError::ContainerNotSupported`). `open` takes the sink, the container, the track
  table with each track's `CodecTicket`, the chapters and the packet
  limits, refuses by name what the container cannot take
  (`MuxOpenError`) and reports what it drops (`MuxReport`, `Uncarried`);
  `open_with` takes `MuxOptions` — the packet limits, whether
  libavformat's automatic bitstream filters run, and the MOV family's
  `write_btrt` option — and the muxer's own
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
  `av1_metadata`, `extract_extradata` and the Annex B conversions — each named for its
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
  (`FfmpegMuxRefusal::LastSampleTooLong`); the frames MP4 still joins at
  the end, which it frees unwritten, are counted at `finish`
  (`Uncarried::Unjoined`). A `mov_text` cue starting
  before the last one's end is written as MP4 writes it.
- **The audited containers, by their census (table H).** What each
  writes of a packet beside its bytes is reported where it leaves
  something out: no time at all (`Rewrite::Timeline`), no duration
  (`Rewrite::Durations`), the presentation time alone
  (`Rewrite::DecodeTimes`) or the decode time alone
  (`Rewrite::PresentationTimes`), no key flag for a codec that does not
  count every frame one (`Rewrite::SyncFromBitstream`), ADTS's headers in
  place of the configuration (`Rewrite::AdtsHeaders`), a chapter it writes
  nowhere (`Uncarried::Chapter`), and the side data, language and flags
  it drops. Refused: an empty packet the container would write as nothing
  (`PayloadRule::Empty`); H.264 or HEVC into a raw stream not opening on a
  start code that no filter converts (`PayloadRule::StartCode`); a raw
  stream's first packet without the configuration the track states
  (`PayloadRule::InBandConfiguration`); ADTS's configurations its header
  cannot write, a second header, a frame past its 13-bit length, a
  headless frame (`MuxOpenError::ConfigurationInvalid`,
  `PayloadRule::AdtsHeader`, `AdtsFrameLength`, `AdtsFrame`); FLAC's
  video tracks, FLAC's missing or `fLaC`-marked configuration; IVF's
  dimensions past 16 bits (`MuxOpenError::Dimensions`); a WAV file past
  4 GiB (`PayloadRule::FileSize`). FLV: a reordering packet with no
  presentation time (`FfmpegMuxRefusal::MissingPts`), a decode time past
  its 32 bits of milliseconds (`FfmpegMuxRefusal::TimeWraps`), a
  composition time past its 24 (`PresentationOffset`), a tag past its
  24-bit size (`PayloadRule::TagSize`), ADTS AAC no filter converts
  (`AdtsHeader`), AAC with no configuration whose first packet brings none
  (`PayloadRule::ConfigurationAbsent`), and at open Opus or FLAC with no
  configuration, which its header asserts, and H.264, HEVC, VVC, AV1,
  MPEG-4 or AAC with none and the filters off. WAV and Wave64: an audio
  configuration past the WAVEFORMATEX's 16-bit `cbSize`
  (`MuxOpenError::ConfigurationInvalid`), a configured AAC track's ADTS
  packet (`AdtsHeader`). A subtitle's negative duration is refused only
  where the container writes a duration. MPEG-TS:
  a video, subtitle or data packet's times kept, audio gathered into PES
  packets of a few (`Rewrite::Timeline`), an access unit delimiter put
  before each H.264, HEVC and VVC picture
  (`Rewrite::AccessUnitDelimiters`, new), a configured AAC track's frames
  behind ADTS headers (`Rewrite::AdtsHeaders`); refused at open a codec
  it writes as private data its reader may not name, AAC of a
  configuration ADTS cannot write, Opus of a channel mapping its PMT
  cannot describe, ARIB captions of a profile but A and C, and a stream
  its PMT section has no room for (`MuxOpenError::StreamIds`, new);
  refused in packets a
  reordering packet with no presentation time (`MissingPts`), H.264,
  HEVC or VVC off a start code no filter converts (`StartCode`), AAC or
  Opus of fewer than 2 bytes (`PayloadRule::TooShort`, new), raw AAC on
  a track with no configuration or past its ADTS header's 13 bits, an
  empty packet, and a time past its clock reference's room (`TimeWraps`);
  an audio stream's language of three bytes and the first of its
  clean-effects, hearing-impaired and visual-impaired flags kept, a DVB
  subtitle's and teletext's language. The MOV family's modes beside MP4 —
  `mov`, `ipod`, `avif` — meet MP4's rules, one writer, and their own:
  each admits the codecs of its tag table its paths write as MP4's —
  MOV's H.264, HEVC, MPEG-4, AAC, ALAC, AC-3, E-AC-3 and `mov_text`;
  iPod's and AVIF's of those and AV1 as each table holds — the rest
  refused at open and booked (`CodecNotCarried`); in every mode a picture
  past 65535 is refused (`Dimensions`), and MP3 below 16 kHz outside MOV
  and TTML's `dfxp` tag outside ISMV (`ConfigurationInvalid`) — MP4's own
  too; AVIF's table of more than one track (`MuxOpenError::TrackLayout`,
  new); MOV writes a language through the Macintosh table, every other
  mode packs it; only MP4's mode writes the DASH roles; MP4 and iPod
  write a cover; MOV's and iPod's chapter track is MP4's, AVIF's none;
  AVIF has libavformat move negative times (`TimesShifted`); AVIF holds
  its track's samples until the trailer, refused past the buffer
  (`PayloadRule::FragmentSize`, new). A chapter's end is judged where the
  next chapter starts as written, in MP4 too.
- **Each mirrored rule carries FFmpeg's whole predicate, and each size
  its loops' exact arithmetic (R9).** Every gate on a codec, a media
  type, a codec tag, a flag, a disposition or the compliance level, every
  loop and split of a text into a list, and every length or count written
  in 8, 16 or 24 bits, in every audited container's writer and the
  helpers it calls, is placed against the rules (greps R13–R15 of #82).
  FLV: big-endian PCM, which its reader takes as little-endian on a
  little-endian host, refused (`CodecNotCarried`); an audio codec of no
  extended header — PCM, Swiff ADPCM, Nellymoser, G.711, Speex — of a
  layout not exactly mono or stereo, whose multichannel header FFmpeg
  writes with a fourcc it has none of, asserting, refused
  (`MuxOpenError::ChannelLayout`, its message now any container's); PCM's
  and Swiff ADPCM's 8 and 16 kHz, written as the 5.5 kHz code, and G.711
  at another rate than 8 kHz, read back at 8 kHz, refused
  (`ConfigurationInvalid`); the Annex B rewrite of H.264, HEVC and VVC to
  length prefixes reported (`Rewrite::LengthPrefixed`) where the
  configuration as of the packet makes it — at open, or at the packet
  whose new extradata, the caller's or `extract_extradata`'s, turns it
  on — and the tag's size judged by it. WAV: of every codec tag but
  integer PCM's, a packet that would take its fact chunk's count past
  2^32 samples refused (`PayloadRule::FileSize`, which now names this
  too). `MuxOpenError::ChapterWithoutTitle`'s message is now any container's.
  MPEG-TS writes a language as the codes its descriptors' loops write: an
  audio stream's ISO 639 descriptor each run of three bytes between
  commas, 63 at most — `eng,fra` read back whole, not reported, a 64th
  code reported dropped — and the DVB subtitle and teletext descriptors
  three bytes and one skipped; the PMT counted at each descriptor's exact
  size, a table past its section refused (`StreamIds`) where FFmpeg
  writes no PMT and answers success; a DVB subtitle or teletext language
  past its descriptor's 8-bit length refused
  (`MuxOpenError::LanguageTooLong`, new); a DVB subtitle's or teletext's
  configuration of another size than its codes' entries refused
  (`ConfigurationInvalid`), none refused (`ConfigurationMissing`), as each
  reads back otherwise; a visually impaired audio stream's descriptions
  and a DVB subtitle's hearing-impaired subtitling type reported added
  (`Uncarried::DispositionAdded`). A track of no
  configuration MP4 or Matroska takes its first packet's new extradata for
  — MP4's AAC, AV1, FLAC and MPEG-4 ALS, AVIF's excepted, and
  Matroska's AAC — opens, its first packet to bring the configuration, a
  configuration's packet among them, or an ADTS frame AAC's filter makes
  one of, the track reading back configured; a first packet bringing none
  refused (`PayloadRule::ConfigurationAbsent`, now these tracks' too), and
  AAC's filter on such a track named at the packet that inserts it. AV1's
  configuration `ff_isom_write_av1c` would fail on — an `av1C` record not
  of version 1 or under four bytes; OBUs whose headers do not parse, of no
  sequence header or two, of an empty metadata OBU, or of a sequence
  header its parse does not read to the last bit — which MP4 and FLV write
  as an empty box or sequence header, refused at open
  (`ConfigurationInvalid`) and as a packet's new extradata
  (`PayloadRule::NewExtradata`); in FLV of no configuration, the one
  `extract_extradata` makes of a unit's sequence headers too, and a unit
  whose OBUs do not parse, which the filter refuses (`PayloadRule::Obus`,
  now FLV's too). The configuration `extract_extradata` makes replaces the
  packet's own, and FLV's rewrite is decided by it.
- **The coordinator's R8 letters, built (R10).**
  MP4's DVD subtitles: the configuration's text palette, which MP4
  writes as sixteen YUV colours and its reader writes back as a palette
  text of its own, reported (`Rewrite::Configuration`, new), or
  `Rewrite::PaletteMoved` (new) where the conversion moves a colour —
  white reads back `fefefe`; a palette line of fewer than sixteen
  colours, the rest written unset, refused (`ConfigurationInvalid`); new
  extradata, which a new sample description holds unconverted, refused
  (`PayloadRule::NewExtradata`).
  Matroska's and WebM's AV1 of no configuration opens, its first packet's
  new extradata written as its CodecPrivate, as `ff_isom_write_av1c`
  writes it, into the room the header reserved; one past that room, of
  which Matroska would write the first four bytes alone, its OBUs lost,
  refused (`PayloadRule::NewExtradata`), as is a first packet bringing none
  (`PayloadRule::ConfigurationAbsent`).
  The configuration writers MP4 and FLV call and ignore the answer of —
  `avcC`, `hvcC`, `vvcC`, `evcC`, `lvcC`, `vpcC`, `av3c` and `dvc1` —
  mirrored from their source: a configuration a stream the standards
  allow can bring that the writer refuses, cuts or writes empty, refused
  at open (`ConfigurationInvalid`) and as a packet's new extradata
  (`PayloadRule::NewExtradata`); a unit's length or an array's count its
  16 bits cut or wrap, which Matroska writes too and answers success,
  refused in Matroska as well; MP4's VP9 of no configuration judged at its
  first sample, which its `vpcC` reads. Under FLV's `extract_extradata`,
  the configuration the filter makes of each H.264, HEVC or VVC packet is
  judged by the writer, and a packet of no unit the filter splits off is
  refused (`PayloadRule::NalUnits`, new). A condition only a parameter
  set the standard does not allow reaches — a value past the range it
  caps, syntax that does not parse — rests on the trust model, and is not
  refused.
  Every admitted container's channel layout, by the round trip — what it
  writes of the layout, its reader's reading, and the decoder
  `avformat_find_stream_info` opens: where the file reads back another,
  reported at open (`Uncarried::ChannelLayout`, new, and
  `LayoutReadBack`, new) — RIFF's mono and stereo of no order, its mask of
  `0x40000` and up written as 0, and its custom orders; Matroska's count
  alone, and MP4's of mono PCM and of FLAC; FLAC's own
  order, which its decoder fills in where the container states none;
  FLV's multichannel configuration's dropped channels; a custom channel's
  name, which no container writes.
- **What a container's start refuses, by name before anything is written
  (R11).** FLV, every refusal of its start (`flv_init`,
  `get_audio_flags`): an audio track at a rate its flags' table names no
  code for — MP3 but at 48, 44.1, 22.05 and 11.025 kHz, its other codecs
  of no extended header but at 44.1, 22.05, 11.025, 16, 8 and 5.512 kHz,
  Speex but at 16 kHz (`MuxOpenError::ConfigurationInvalid`) — and Speex
  of more than one channel (`MuxOpenError::ChannelLayout`); MPEG-4 Part 2
  and H.263, no part of FLV's specification at the normal compliance
  (`MuxOpenError::CodecNotCarried`); a video track past the first of
  another codec than VP9, AV1, H.264, HEVC and VVC, and an audio track
  past the first of another than AAC, MP3, Opus, FLAC, AC-3 and E-AC-3
  (`MuxOpenError::TrackLayout`). FLV's native channel layout of no channel
  its multichannel configuration keeps — downmix's stereo, the wide, the
  surround direct, the side and top surround and the binaural pairs —
  written as a mask of zero, which its reader refuses, the stream read
  back of no channels (`MuxOpenError::ChannelLayout`), the one reader's
  refusal of a written layout among every audited container's.
  MOV's track aperture of a video whose aspect ratio is stated and not 1,
  its widths and height shifted left 16 in an `int`, past 32767
  (`MuxOpenError::Dimensions`): every MOV video codec, H.264, HEVC and
  MPEG-4 among them.
- **MP4's PCM, by its sample table (R11).** MP4 counts every PCM sample
  one tick from the track's first, so that a packet's time is the samples
  before it: a gap or an overlap moves the packet and every later one,
  now reported as the timeline it moves (`Rewrite::Timeline`) where it was
  reported as `Rewrite::Durations`; a packet's duration not its samples is
  still `Durations`. Its samples, summed in an `int`, refused past
  `INT_MAX` (`PayloadRule::SampleCount`, new): 12.4 hours at 48 kHz.
- **MP4's sample tables, by their sizes (R12).** Every 32-bit box size
  movenc writes from a per-sample count, by mechanism: none of one track's
  passes 32 bits within the table movenc keeps of its samples, which it
  grows 1024 entries at a time through an allocation libavutil refuses
  past `INT_MAX` bytes — after writing the packet's bytes, the write
  failed. The packet whose sample would pass it is refused
  (`PayloadRule::SampleTable`, new): 29 826 048 entries of 72 bytes, each
  sample MP4 writes one — a packet's, an E-AC-3 sample of the frames
  joined, a `mov_text` cue and the empty samples around it. A chapter
  table past it, which MP4's
  chapter track writes a sample a chapter, refused at open
  (`MuxOpenError::ChapterCount`, new). The chunk offsets and the `mdat` widen
  past 32 bits.
- **MP4's sample descriptions, counted as movenc makes them (R13).** One
  of each configuration a packet brings that none of the track's holds,
  byte for byte — the stream's the first, MPEG-4 ALS's, AAC's, AV1's and
  FLAC's rewritten in place by a configuration's packet — and the 1025th,
  which FFmpeg's reader refuses with the whole file, refused
  (`PayloadRule::SampleDescriptions`, new). Each
  packet is judged on borrowed bytes, its refusals made before anything is
  allocated, and only the configuration it adds or changes is kept (R14);
  VP9's first description of no configuration is the superframe its
  filter makes of the frames it held, byte for byte.
- **MP4's `moov`, by an upper bound (R14).** The `moov`
  the trailer writes sums every track's boxes in a 32-bit size, and a
  track's chunks rest on libavformat's interleaving, which this release
  does not mirror: each track's per-sample tables are counted at their
  worst for the samples taken — a chunk a sample, its offset in 64 bits,
  a run of every table a sample — beside its sample descriptions, the
  chapter track, the covers, the chapters, the streams' `pssh` boxes and a
  derived slack, and the packet after which the bound would pass
  `UINT32_MAX` is refused (`PayloadRule::MovieBox`, new): stricter than
  FFmpeg past the bound alone, until an exact mirror replaces it. The
  laws' seams, hidden: `MuxOptions::with_lowered_movie_bound` and
  `CarrierMuxer::movie_box_bound` (`MovieBoxBound`).
- **MP4's covers, within the movie metadata's buffer (R14b, R15).** The
  movie's `udta` holds every cover and is made in a dynamic buffer of
  `INT_MAX` bytes, which writes the `covr` cut past it — two covers of the
  default packet budget pass it — and its size and 8-byte header are
  summed in an `int`, which overflows past `INT_MAX - 8`. Its size is
  counted exactly at open — the iTunes metadata, libavformat's name and
  version among it, the covers and the Nero list — and the table whose
  covers would take it past `INT_MAX - 8` refused, nothing written
  (`MuxOpenError::CoversTooLarge`, new; the laws' seam
  `MuxOptions::with_lowered_user_data_bound`, hidden).
- **MP4's `moov` of no sample, and before its trailer (R15).** A table
  whose `moov` would pass its 32 bits before any sample — covers, `pssh`
  boxes, sample descriptions — is refused at open
  (`MuxOpenError::MovieBoxTooLarge`, new); and the bound is checked once
  more before the trailer runs (`FfmpegMuxRefusal::MovieBox`, new). A description rewritten in
  place counts its new bytes instead of its old: the bound keeps the
  descriptions' exact sum. The laws' seam, hidden:
  `CarrierMuxer::lower_movie_bound`. The bound charges the tracks movenc
  writes alone (R16): outside AVIF, a track of no sample is left out of the `moov` but an empty
  `mov_text` track, whose trailer writes it an end sample, and TTML of
  libavcodec's encoder of a paragraph, whose trailer joins its document.
- **A packet refused allocates nothing; a write's allocation refused
  hands the packet back (R15).** Every rule a packet meets — the face's,
  libavformat's and the container's — is judged before anything is
  allocated for it, over its borrowed bytes and counted lengths; what
  recording the write needs is then allocated fallibly, and an allocation
  the allocator refuses refuses the packet by name, handed back with
  nothing recorded (`FfmpegMuxRefusal::OutOfMemory`, new). What
  libavformat allocates as it writes the packet is its own.
- **HDR side data judged against sizes resolved at open (R18).** The
  linked libavutil's HDR10+ and APP5 struct sizes are asked at the open,
  before anything else — their allocation's failure the open's allocation
  fault — and a packet's HDR side data is judged against them, so that
  the judgment of a packet calls into no library and allocates nothing,
  and no allocation's failure is taken for a malformed entry.
- **What FFmpeg's writers do with an allocation's failure, censused (R19).**
  The MOV family writes a stream's encryption initialization as its
  `pssh` boxes, counted in the `moov`'s bound — an allocation's failure
  where movenc parses it again, which drops the boxes unsaid, rests on the
  premise the census states (R21). AVIF's `moov` is
  bounded at `INT_MAX`, the null buffer its trailer measures it in
  (`MovieBoxBound::limit`, new, hidden). MP4 and FLV refuse a VP9 track
  of an unknown level whose picture's samples pass an `int`
  (`MuxOpenError::Dimensions`). The module's census names every other
  allocation failure FFmpeg swallows, by mechanism.
- **Every finding classed by its source; sizes under budgets the caller
  raises (R20).** A length that wraps is refused wherever a budget the
  caller raises — `PacketLimits`, `CodecTicket::mirror_with` — lets one
  reach it: the configuration writers' Annex B rewrite, held in a dynamic
  buffer cut past `INT_MAX` with its padding, refused where the cut takes
  a unit the record keeps (R22); the MOV family's `esds` descriptor in its
  28 bits; MP4's `dvc1` and `lvcC` (`ConfigurationInvalid`); FLV's
  sequence header in its tag's 24 bits, at open, as a packet's new
  extradata and as `extract_extradata` makes it of a packet
  (`ConfigurationInvalid`, `PayloadRule::NewExtradata`), and that filter's
  split of a packet, held to what libavcodec allocates
  (`PayloadRule::SplitSize`, new); MPEG-TS's PES payloads, summed in an
  `int` (`PayloadRule::PesSize`, new), and an Opus initial padding its
  `int` counts at 48 kHz (`MuxOpenError::InitialPaddingTooLong`, new).
  MPEG-TS's raw AAC is framed in ADTS here, as FFmpeg's ADTS writer frames
  it, and MPEG-TS takes the frame as it is: its nested ADTS muxer, which
  copied a null buffer where its own close failed, is never fed. The laws'
  seams, hidden: `MuxOptions::with_lowered_descriptor_bound`,
  `with_lowered_obu_bound`, `with_lowered_pes_bound`,
  `with_lowered_reader_bound`, `with_lowered_rewrite_bound`,
  `with_lowered_sequence_bound` and `with_lowered_split_bounds`.
- **A write that failed is not gone on from (R20–R22).** A libavformat
  call that failed ends the output failed: every later write and the
  trailer are refused by name (`FfmpegMuxRefusal::OutputFailed`, new), and
  the sink is dropped, never handed back as a whole file. A trailer this
  muxer refuses by name before it runs — no frame, a last sample too
  long, the `moov` past its bound, a bit rate, an edit list or a fact
  chunk its field would cut — abandons the output the same way, nothing
  more reaching the sink.
- **FLV's tracks of a kind, and the durations libavformat guesses
  (R21).** FLV numbers each track of a kind in 8 bits: a kind's 257th
  track is refused at open (`MuxOpenError::TrackLayout`). A packet that
  states no duration is counted as libavformat guesses it
  (`guess_pkt_duration`) wherever a container writes one — WAV's fact
  chunk, MP4's samples — and Matroska's default duration is judged as
  Matroska judges it.
- **The narrow writes (R21).** Every fixed-width write of the kept
  muxers and the helpers they call is classed by the range of what it
  writes, and each a stream the standards allow can wrap is refused by
  name: IVF's frame count (`PayloadRule::FrameCount`, new; the laws' seam
  `MuxOptions::with_lowered_ivf_frame_bound`, hidden); WAV's and Wave64's
  format fields; Matroska's JPEG 2000 bitmap header; MP4's display width;
  the `hvcC` and `vvcC` bit depths and VVC's picture
  (`ConfigurationInvalid`, `Dimensions`, `PayloadRule::NewExtradata`);
  FLV's 31-bit tag time (`TimeWraps`); and, at `finish`, of the whole
  track (R22), the MOV family's edit list and bit rates.
- **The formats kept, and those withheld (R21, R22).** The muxers of
  formats no longer in use are refused at open by the muxer's name
  (`MuxOpenError::ContainerNotSupported`, renamed from
  `ContainerNotAudited`), their rules, refusals and seams gone with them;
  Ogg, MP3 and raw MP2, audited, are withheld by name and the reason
  (`MuxOpenError::ContainerWithheld`; `Withheld`, new —
  `OggPageBuffering` until findit-studio/oggoat, `Mp3XingFrame` and
  `Mp2AwaitsMpegoat` until findit-studio/mpegoat, mediaio#98). The MOV
  family writes a stream's encryption initialization as its `pssh` boxes
  again, a key id of no bytes refused at open, before libavutil parses
  it (`MuxOpenError::KeyIdWithoutBytes`, new).
- **The MOV family's bit rates, as its trailer writes them (R22).**
  `calculate_mpeg4_bit_rates` is mirrored whole: the average counted of
  the samples as movenc writes them — each of its rewrites counted
  exactly — its product in 128 bits; where it counts none, the CPB
  properties' average, the stated rate, then their maximum; the maximum
  and the buffer — each in the field its `btrt` or `esds` writes it,
  refused at `finish`, before the trailer runs, the box named
  (`FfmpegMuxRefusal::BitRates`, `BitRateBox`, new); CPB properties
  shorter than the fields movenc reads, refused so
  (`FfmpegMuxRefusal::CpbProperties`, new); MP4's chapter track's at open
  (`MuxOpenError::ChapterBitRate`, new). FFmpeg's `write_btrt` option is
  the caller's: `MuxOptions::with_write_btrt`, `set_write_btrt` and
  `write_btrt`, of `WriteBtrt` (new) — `Auto`, MP4's mode alone, FFmpeg's
  default; `On`; `Off`, the remedy for an MP4 track refused by its
  `btrt`. The laws' seam, hidden: `CarrierMuxer::bit_rate_counts`.
- **Exact both ways (R22).** Each predicate that mirrors an FFmpeg
  computation is held neither looser nor stricter than it: the MOV
  family's edit list judged at `finish` of the whole track — its media
  time where its version is 0 (`TimeWraps`), and a span written below
  zero, of a track that ends before zero or whose samples are all marked
  discarded (`FfmpegMuxRefusal::NegativeSpan`, new); WAV's fact chunk of
  the last packet's duration at `finish`, the span alone at the packet; a
  subtitle track's header dimensions in the MOV family as a video's, a DVD
  subtitle's of its configuration's size line (`Dimensions`); a packet's
  Annex B rewrite counted exactly, MP4's HEVC under `hvc1` refused only
  where its buffer's cut takes a unit its walk keeps (`RewrittenSize`; the
  laws' seam `MuxOptions::with_lowered_sum_bound`, hidden).
- **The duration libavformat hands (R23).** Every judgment that reads a
  packet's duration reads the one libavformat hands the container — the
  packet's own, else its guess (`guess_pkt_duration`): MP4's limit of
  `INT_MAX` ticks, a guess past it refused by name with the guessed
  duration in the stream's time base and the packet handed back, where
  `check_pkt` failed the write (`DurationTooLong`), and MP4's skip samples
  (`SkipSamples`), which counted a packet of no duration as one frame.
- **Every signed sum of a time (R24).** Each sum a packet's times meet in
  `int64_t` — libavformat's count of the next decode time and its shift's
  move; Matroska's and WebM's time, codec delay and end, of each track's
  VFW or QuickTime mode; FLV's end and its delay; the MOV family's sample
  end — is held to it exactly at the packet, by name, the packet handed
  back (`TimeWraps`, `DurationTooLong`), where the sum was a signed
  overflow. A packet before libavformat's first write whose time moves its
  shift, or FLV's first decode time, so that a packet held before it passes
  one of those bounds, FLV's 31 bits or MPEG-TS's clock, is refused by that
  time. MP4's joined TTML document is held to the largest packet of its
  exact bytes (`PayloadRule::DocumentSize`), no longer of an upper bound of
  each paragraph.
- **The interleaving's sums, bounded (R25).** Two signed sums whose
  exactness would need libavformat's interleaving mirrored are held to
  named bounds (`TimeWraps`): Matroska's cluster time, a block's time at
  `INT64_MAX + 2` less the file's largest codec delay refused; and the
  interleaving's delay, a decode time outside ±2^62 microseconds refused
  where two streams or more interleave. Matroska's move of early times in
  a file of a video codec of no native id is mirrored, of the codec's VFW
  or QuickTime mode, where such times were refused.
- **MPEG-TS's clock, of FFmpeg's own expressions (R25).** The clock
  reference counts the decode time alone, 300 times over, of a `max_delay`
  the muxer leaves at zero: a decode time, moved by libavformat's shift,
  past `i64::MAX / 300` ticks is refused (`TimeWraps`, the decode time
  named), and the presentation time is held to `int64_t` alone, where both
  were refused past `i64::MAX / 600`.
- **MPEG-TS's clock reference, of its period (R26).** On the stream MPEG-TS
  writes its clock reference on — the first video stream, else the first —
  the first decode time is held to `(i64::MAX − period) / 300`, of the
  period FFmpeg chooses of the stream's frame (`TimeWraps`), where FFmpeg's
  first sum against the reference a period before zero overflowed.
- **Matroska's DURATION tag, and the MOV family's XDCAM name (R27).**
  Matroska's trailer writes each track's latest end in seconds cut to an
  `int`: a packet whose end passes `INT_MAX` seconds is refused
  (`DurationTooLong`, or `TimeWraps` by its time). The MOV family's
  interlaced XDCAM picture of a frame rate whose double passes an `int` is
  refused at open (`ConfigurationInvalid`). An MP4 AC-3 or E-AC-3 track of
  no sample, which MP4 leaves out of its `moov`, no longer fails `finish`
  (`NoFrame`).
- **WAV's fact chunk of no packet (R28, R29).** A WAV track of any codec
  but integer PCM that took no packet is refused at `finish` (`NoFrame`):
  FFmpeg counts it as `−INT64_MAX` samples, written cut. Wave64, whose
  header starts its extrema at zero, counts such a track as zero and
  finishes; its fact sum is held to `int64_t` at `finish`
  (`DurationTooLong`) and its count to `av_rescale`'s (`FileSize`).
- **MP4's APV `apvC`, by its parse of the packets (R11).** MP4 records each
  distinct frame information of a PBU type it parses out of the packets
  and counts them in 8 bits, its parse failing at the 256th, its answer
  ignored: the packet whose frame would bring one is refused
  (`PayloadRule::ApvFrameInfo`, new).
- **HEVC's layers, where its configuration records lose them (R11).**
  `ff_isom_write_hvcc` keeps the base layer's units and the alpha layer's
  a VPS extension names — of an `hvcC`, the base layer's alone: a
  configuration's units of another layer, MV-HEVC's second view among
  them, are refused (`MuxOpenError::ConfigurationInvalid`,
  `PayloadRule::NewExtradata`) in Matroska, FLV and MP4 — but where MP4
  writes them into `lhvC`: an MP4 HEVC track's multilayer flag
  (`AV_DISPOSITION_MULTILAYER`) is now handed to the stream, `lhvC`'s
  every condition mirrored; an `lhvC` of no such unit, which FFmpeg's
  reader refuses with the whole file, refused; the flag reported lost
  where `lhvC` is not written.
- **What a reader would read back falsely, refused (R11).** FLV's HEVC,
  AV1, VP9 and VVC track of no matrix coefficients, which FLV's metadata
  packet states not and its reader reads back as RGB
  (`MuxOpenError::ColourDescription`, new).
  A coded side data value past the field a container writes it in, or its
  reader reads it back into (`MuxOpenError::CodedSideData`, new): MP4's
  and MOV's content light level past 65 535 — 70 000 read back as 4464 —
  mastering chromaticities and ambient light past 65 535
  fifty-thousandths, luminances and illuminance past `INT_MAX`
  ten-thousandths, read back negative; MOV's stereo field of view and
  disparity; FLV's light level past 65 535.
  WebM's stereo 3D of checkerboard, lines or columns, which its check,
  wanting its mask, writes as modes WebM defines not, and of frame
  sequence, which fails its header (`MuxOpenError::StereoMode`, new).
- **A coded side data entry FLV's reader rebuilds otherwise, reported
  (R12, Codex R11).** FLV writes an HEVC, AV1, VP9 or VVC track's content
  light level and mastering display into its metadata packet whole, and
  its reader rebuilds the light level only of MaxCLL and MaxFALL both
  nonzero, the display's primaries only of every coordinate above zero and
  its luminance only of both bounds above zero: the entry, or the part of
  it, the file reads back without is reported at open
  (`Uncarried::CodedSideData` and `CodedSideDataReadBack`, new) — the file
  true, the packets as written — and so is a display whose values its
  reader, keeping each in a float and making it a rational again with
  `av_d2q`, reads back as other rationals, however near (R13): the line
  carries what reads back. The report's first line of a track's coded
  side data.
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
  whose Annex B rewrite would pass the `int` its rewriter counts it in
  (`RewrittenSize`); a Matroska block
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

### Changed

- **BREAKING: smol-bytes 0.2 / mediaframe 0.13.** A container's name is
  `smol-bytes` 0.2's `Utf8Bytes` (`ContainerName`), and the write faces this
  crate implements are `mediaencode` 0.20's, on `mediaspine` 0.2 and
  `mediaframe` 0.13. Released in lockstep with `mediaio-ffmpeg`,
  `mediadecode-ffmpeg` and `mediaio` 0.22.0.

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
