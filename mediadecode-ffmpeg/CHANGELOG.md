# Changelog

All notable changes to the [`mediadecode-ffmpeg`](https://crates.io/crates/mediadecode-ffmpeg)
crate are documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this crate adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

The backend-agnostic core it adapts has its own log at
[`mediadecode/CHANGELOG.md`](../mediadecode/CHANGELOG.md).

## [0.20.0] - unreleased

### Changed

- **The demuxer marks `PacketFlags::DISPOSABLE` from the bitstream.**
  A video packet of an H.264 or HEVC track is marked where its NAL units
  prove that nothing references its picture (`mediadecode::nal`): a
  non-reference H.264 picture, or an HEVC sub-layer non-reference picture
  at the highest temporal sub-layer, with nothing beside it a later
  picture needs, read with the `AV_PKT_DATA_NEW_EXTRADATA` it carries as
  the decoder takes the two, and told of every seek. **Where a track is
  read, its reading decides: the container's own mark (MP4's `sdtp`) is
  cleared on every other verdict**, since a picture nothing references
  may still take up SEI state an earlier packet left — film grain a
  skipped RASL picture's SEI left, for one. A track not read keeps the
  container's mark. Every packet is read, a corrupt one too, as the
  decoder reads it; a corrupt one is never marked. **A packet carrying side data that persists past it — a new
  configuration, a parameter change, a palette, captions, and any type
  this build does not name — is never `DISPOSABLE`: the container's mark
  is cleared there too.** **An encrypted payload is never read:** a
  packet carrying `AV_PKT_DATA_ENCRYPTION_INFO` or
  `AV_PKT_DATA_ENCRYPTION_INIT_INFO` gets no bitstream mark, and its
  track's reading stops there for the rest of the stream, a seek
  included; and a track FFmpeg names encrypted at the open — a stream's
  `ENCRYPTION_INIT_INFO` from `pssh`, or its `enc_key_id` — is never read
  at all. **No Matroska or WebM track is read**: FFmpeg 9 names a
  Matroska track's encryption only by its key id and passes one without
  a key id through undecrypted with nothing public to say so, so nothing
  establishes a Matroska track's plaintext; and with no discardable bit
  mapped, a Matroska or WebM packet is never `DISPOSABLE`. **A named
  limit:** an MXF file whose essence is encrypted and read without a key
  is outside the reading's guarantee, since libavformat returns its
  ciphertext with nothing that says so.
  **More packets are marked DISPOSABLE on H.264
  and HEVC, so a consumer that drops them decodes fewer pictures**: two in
  three of a Sony XAVC original, which carries no `sdtp` and was marked
  nowhere before. An H.264 stream whose sequence parameter set codes
  fields, lets `frame_num` jump, or leaves the reorder depth to FFmpeg's
  guess (no `bitstream_restriction_flag`) is marked nowhere. The flag's
  contract is unchanged — absent means unknown — and its promise is
  pixels: dropping a marked packet changes no later picture's pixels, but
  may move a later frame's best-effort timestamp
  (`VideoFrameExtra::best_effort_timestamp`, FFmpeg's guess from every
  frame decoded before), as dropping a packet `sdtp` marks may. A
  consumer reads the frame's `pts`, its packet's, for the exact one.
- **BREAKING: FFmpeg 9 is the floor.** This crate builds against FFmpeg
  9 or newer and no older release: the build script of `mediaio-ffmpeg`
  0.20.0, the base it is built on, reads the linked release from
  `ffmpeg-sys-next`'s `links` metadata (`DEP_FFMPEG_FFMPEG_9_0`, set from
  libavcodec 63.1, FFmpeg 9.0's) and refuses an older FFmpeg by name. Every
  FFmpeg 9 packet side-data kind is named as its headers name it: no
  version gates, and a kind a later FFmpeg adds persists, so its packet is
  never `DISPOSABLE`.

## [0.19.0] - 2026-10-16

The FFmpeg crossings move to the new base crate,
[`mediaio-ffmpeg`](../mediaio-ffmpeg/CHANGELOG.md); this crate keeps the
machines — the decoders, the demuxer, the hardware backends — and
re-exports every name it exported before. Tracks `mediadecode` 0.19.0
(see [its entry](../mediadecode/CHANGELOG.md#0190)). The family releases
in lockstep.

### Changed (BREAKING)

- **`Received::Frame` is `Received::Produced`**, with `mediadecode`.
- **Parameter copies answer `ParametersError`.** `TrackExtra::clone_parameters`,
  `CodecTicket::mirror` and `CodecTicket::rebuild` return the base's
  `ParametersError` — seven arms, the payloads of `DemuxError`'s
  `Parameters*` arms — instead of `DemuxError`. `DemuxError:
  From<ParametersError>` maps each arm onto its same-named `DemuxError`
  arm, so `e.into()` restores the old type.
- **`Frame::empty` answers `CrossingError`**, and
  `ResampleError::Resample` carries it, instead of the decoder `Error`;
  `Error: From<CrossingError>` maps it onto `Error::Ffmpeg`.

### Changed

- **The crossings live in `mediaio-ffmpeg`**: the carriers, packet and
  frame conversion, codec tickets and identifiers, limits, the AVFrame
  crossing, the allocator judge and the FFmpeg resampler, with `Ffmpeg`
  and its adapter impls. Every path through this crate still resolves,
  by re-export.
- **Every function whose soundness rests on something its caller proves
  is an `unsafe fn` with a written `# Safety` section**, and every call
  site states its proof — the precondition for the crossings becoming
  callable from another crate. Each re-exported `unsafe fn` names every
  pointer it dereferences, against `mediaio-ffmpeg`'s new `contracts`
  module.
- `boundary::write_md_flags` is a safe fn.

### Added

- `ParametersError` and `CrossingError`, re-exported from the base.
- A `test-support` feature (off by default), forwarded to the base: the
  fault-subprocess harness and the callback-state hooks, for the lanes
  of crates built on this one.

### Fixed

- **The callback state is owned through the pointer the codec context
  holds.** The audio, image, subtitle and video decoders kept it in a
  `Box` and moved that box after the context took its pointer — and a
  `Box` asserts uniqueness over what it holds, so the move invalidated
  the pointer the callbacks use. They now hold a raw-pointer owner that
  frees the state after the context, and the hardware probe names its
  wanted format through that same pointer before the open, instead of
  installing a second one.
- **The re-exported conversions from `ffmpeg_next` objects are
  `unsafe`.** `ffmpeg_next`'s safe API builds frames and packets whose
  bytes were never written (`frame::Video::new`, `Packet::new`), so a
  borrow of one cannot vouch for them and every conversion from one —
  the four `convert::*_frame_from` and the thirteen packet conversions —
  is an `unsafe fn` (see `mediaio-ffmpeg`). This crate's decoders and
  demuxer state where their bytes were written — `avcodec_receive_frame`,
  `av_hwframe_transfer_data`, `av_read_frame` — beside each conversion.
- **`VideoDecoder::send_packet` takes the crate's own packet carrier.**
  The hardware-probe decoder's safe input is an `OwnedVideoPacket`, built
  into an `AVPacket` on the scoped submission road; an `ffmpeg_next`
  packet goes through `VideoDecoder::send_av_packet`, which is `unsafe`:
  its caller vouches that the payload was written, that the
  `AV_INPUT_BUFFER_PADDING_SIZE` bytes after it are zeroed or the padding
  libavformat delivered it with, and that nothing writes through the
  payload or the padding while the decoder may still read them
  (libavcodec keeps a reference past the call). The stream decoders'
  hardware seam takes the same raw road only for the packets they build
  themselves.
- **A custom channel map is not walked on the borrowed road.**
  `convert::audio_frame_from` refuses it by name
  (`ConvertError::UnverifiableChannelMap`); this crate's audio decoder
  converts the frames libavcodec gives it through the raw entry, whose
  map extent libavcodec guarantees.
- **The borrowed conversions read nothing they cannot bound.**
  `convert::audio_frame_from` refuses a null-backed frame before
  reading a field, and `convert::subtitle_frame_from` refuses a bitmap
  rect carrying a plane, whose size nothing records
  (`ConvertError::UnverifiableBitmap`). This crate's subtitle decoder
  converts through the raw entry, whose bitmap extents libavcodec
  guarantees.

## [0.18.0] - 2026-10-10

Tracks `mediadecode` 0.18.0, whose `PacketFlags` names `DISPOSABLE` (see
[`mediadecode` 0.18.0](../mediadecode/CHANGELOG.md#0180)). The three crates
release in lockstep.

### Changed (BREAKING)

- **`mediadecode` 0.17 → 0.18**, and with it `AV_PKT_FLAG_DISPOSABLE`
  arrives as `PacketFlags::DISPOSABLE`. The raw flags byte already crossed
  the boundary bit for bit on every packet road: the demuxer's packets, the
  copies the probe and a replay make (`try_clone_packet` keeps the flags),
  and the `AVPacket` rebuilt for a decoder. The hint therefore arrives
  under its name with no mapping in between, and a compile-time assertion
  pins the two bits equal. A packet the demuxer did not mark arrives
  without it: unknown, not referenced. In libavformat 9.0.1 the MP4/MOV
  demuxer is the one that sets it, from `sdtp` (`mov.c` 11719–11723); the
  Matroska demuxer does not map its discardable bit (`matroskadec.c`
  4319–4320), so a Matroska packet arrives without it.

## [0.17.0] - 2026-10-10

Tracks `mediadecode` 0.17.0, which crosses `mediatime` 0.4 → 0.5 and
`mediaframe` 0.11 → 0.12 (see
[`mediadecode` 0.17.0](../mediadecode/CHANGELOG.md#0170)). The
workspace's three crates release in lockstep again: 0.16.0 was this
crate's alone, released ahead of the core and the WebCodecs adapter,
which both skip 0.16.

### Changed (BREAKING)

- **`mediatime` 0.4 → 0.5, through `mediadecode`.** Every `Timebase`,
  `Timestamp` and `TimeRange` in this crate's API is `mediadecode`'s
  re-export, so a consumer holding a `mediatime 0.4` value no longer
  type-checks against this release. No source line here moved: this
  crate calls none of the `TimeRange` setters 0.5 removed and never
  enables `mediatime/buffa`, and no `Timebase` or `Timestamp` method
  changed its code.
- **`mediaframe` 0.11 → 0.12**, a direct dependency here (pinned with
  `alloc`) as well as the core's. This crate's API carries `mediaframe`
  values — the channel layout descriptions, `frame::Rotation`, and the
  pixel-format and color vocabulary — so a consumer holding a
  `mediaframe 0.11` value no longer type-checks against this release.
  0.12's source is 0.11's but for comments, on `mediatime` 0.5, so the
  graph holds one `mediatime`. No source line here moved.

## [0.16.0] - 2026-10-10

A committed hardware session reports every decoder failure as that
picture's own and changes nothing after its first picture; when to stop
trusting it is the caller's policy. `DecodePath::AnyHardware` is a
session that is hardware or nothing.

### Changed (BREAKING)

- **`DecodePath::Auto` no longer degrades after the first picture.**
  `Auto` probes at open — the platform's hardware backends in order,
  the packets taken before the first picture replayed across them, then
  software with the same packets replayed, losslessly — and at no other
  time. The in-session degrade is gone: the cold software decoder opened
  mid-stream, its one-GOP gap, and the keyframe-anchored resync guard
  that watched over it.

- **After the first picture every decoder failure is that picture's own
  error, on every path, and nothing is classified or remembered.** A
  committed hardware session reports each `VideoDecodeError::Decode` as
  it was minted — a refusal this crate made by its name
  (`HwSurfaceTooLarge`, `HwTransferTooLarge`, `FrameBudgetExceeded`),
  anything else as `Error::Ffmpeg` with libavcodec's errno — and nothing
  of it is remembered, so the next call reaches libavcodec. Whether a
  hardware session recovers is FFmpeg's, not this crate's. Until now
  `AVERROR_EXTERNAL`, `AVERROR_BUG`, `AVERROR_BUG2`, `AVERROR_UNKNOWN`,
  `AVERROR_INVALIDDATA` and `EINVAL` from a committed backend, and a
  coded surface the `get_format` callback declined over the caller's
  ceiling, became a post-commit `AllBackendsFailed`, which `Auto`
  answered by degrading to software.

  FFmpeg has no reliable signal that a hardware session is gone, which
  is why none is read. In FFmpeg 9.0.1, `AVERROR_EXTERNAL` also answers
  a single picture: VideoToolbox's frame post-processing and buffer
  creation (`libavcodec/videotoolbox.c` 126–129 and 556–559), and every
  failed CUDA call under NVDEC (`libavutil/cuda_check.h` 52), the decode
  of one picture included (`nvdec.c` 662). `ENOSYS` is NVDEC's answer to
  one HEVC picture its tables cannot describe (`nvdec_hevc.c` 200–227).
  A hardware format `ff_get_format` withdraws says only that the
  hwaccel's setup failed, not why (`decode.c` 1341–1343 and 1348–1357).

  Nor does FFmpeg always recover a session. A malfunction or an
  invalidated session marks a VideoToolbox session for a restart
  (`videotoolbox.c` 1076–1077), as does an H.264 SPS whose profile/level
  bytes differ (446–450), and the next picture stops the session and
  starts a new one (1062–1068). A restart that fails answers
  `AVERROR_EXTERNAL` (1066–1067) and leaves no session behind, because
  the stop released it (518–522): VideoToolbox answers every picture
  after it with `AVERROR_INVALIDDATA` (1071–1072) until a new parameter
  set re-arms the restart (446–450). `flush` does not rebuild one:
  `avcodec_flush_buffers` reaches only the codec's own flush
  (`libavcodec/avcodec.c` 417–418), which drops the pictures and
  references the codec holds.

  **When to stop trusting a hardware session is the caller's policy.**
  A caller that sees a hardware session's decoder failures persist
  rebuilds: it opens a session on `DecodePath::Software` from the same
  parameters and feeds it forward. The caller sees the failures on all
  three roads, the packets' key flags and what it has delivered, and
  what it can feed the new session depends on the road:

  - a `send_packet` failure names the packet in hand, and the new
    session is given that packet;
  - a `receive_frame` failure may concern a packet accepted earlier,
    because FFmpeg decouples input from output and may hold several
    pictures (`libavcodec/avcodec.h` 90–139), so the new session is
    given the next packet and the caller accepts the gap;
  - `send_eof` has no packet: what the hardware session still held at
    the end is recoverable only from packets kept from before, or by a
    seek.

  The policy counts `VideoDecodeError::Decode`. A
  `VideoDecodeError::Convert` is the wrapper's own failure to convert a
  decoded picture into a frame, and it is reported, not counted. One
  that failed on an allocation parks the picture: until a
  `receive_frame` delivers it, `send_packet` and `send_eof` answer
  `Sent::MustDrain` without reaching libavcodec, which is the wrapper's
  back pressure and not a failure.

  The README's usage example is the simplest policy. It counts the
  hardware session's decoder failures from all three roads, and only a
  delivered picture ends the count; at a threshold of its own it opens
  `Software` from the same parameters and feeds it forward, and every
  picture the new session delivers is delivered as it is. It keeps no
  packets, so the pictures from the failure to the next keyframe are
  lost: libavcodec drops or conceals what comes before one. A lossless
  replay is the caller's own design and is not shown.

- **`AllBackendsFailed` is the probe's alone.** Its `origin()` is always
  `FallbackOrigin::Probe`.

- **`DecodePath` gains an arm,** `AnyHardware`, so an exhaustive `match`
  on it must name the new arm.

### Changed

- **The view lane's probe-window copy is retired.** A scoped submission
  shares its carrier's buffer with libavcodec on every road, before the
  first picture as after it, wherever the padding behind the payload is
  provable. From 0.9.0 the body was copied while the hardware probe
  recorded, because the probe kept its rescue history by reference. It
  now records copies of its own (see **Fixed**), which left each
  probe-era packet on the view lane copied twice; the probe's copy is
  the one copy now.

### Added

- **`DecodePath::AnyHardware`**: the platform's hardware backends in
  probe order, the probe-era packets replayed across them, and never
  software. When no backend takes the stream it refuses with
  `Error::AllBackendsFailed` — at `open_as`, with every backend's
  attempt and no packet (at once on a platform with no hardware
  backend), or on the road that met the probe's exhaustion, with the
  packets the probe took. `is_hardware()` answers `true` for the
  session's whole life.

### Removed (BREAKING)

- **`VideoDecodeError::PostCommitNeverResynced`**, its payload
  `PostCommitNeverResynced` (`new`, `packets_lost`), and the variant's
  generated `is_` / `unwrap_` / `try_unwrap_` accessors: the post-commit
  degrade whose lost tail it reported is gone.
- **`AllBackendsFailed::new_post_commit`**: no `AllBackendsFailed` is
  raised after the first picture.
- **`FallbackOrigin::PostCommit`** and `FallbackOrigin::is_post_commit`:
  nothing is collected after the first picture, so no era is left for
  it to name.

### Fixed

- **The probe's rescued packets are its own copies.** It recorded each
  packet it took by reference — `av_packet_ref` shares the payload
  buffer (`libavcodec/packet.c` 461–468 in FFmpeg 9.0.1) — so the
  packets `AllBackendsFailed` hands back shared the caller's payloads:
  the caller's writes after a send reached them, and `data_mut` on the
  two lent aliasing `&mut [u8]` from safe code. The probe now copies
  each payload as it records it (`av_packet_ref`, then
  `av_packet_make_writable`, both checked) and charges the copy what
  it charged the reference; a copy that cannot be made refuses the
  send with `AllBackendsFailed` before libavcodec sees the packet. A
  backend on trial is replayed copies, so none of what is handed back
  is shared with a decoder.

## [0.15.1] - 2026-10-05

### Added

- **The backend reads `AVFormatContext.duration` and its
  `duration_estimation_method`**, answering the `Demuxer::duration` that
  `mediadecode` adds, with libavformat's own account of the figure.

  The figure is read once at open, after probing, and carried in
  `AV_TIME_BASE` microseconds rather than rescaled; `None` where
  libavformat holds no positive figure. The method is carried word for
  word — `FROM_PTS`, `FROM_STREAM` and `FROM_BITRATE` become `FromPts`,
  `FromStream` and `FromBitrate` — and a method this build does not name
  becomes `Unknown`, not an estimate. It is read as the raw integer it is
  on the wire, never as the bindgen enum. None of the three says that
  the file declares the figure: `FROM_STREAM` is what libavformat
  records for a duration taken from a stream or from the container's own
  field (Matroska's), and the label does not say which.

  Pinned against real containers: Matroska and MP4 answer `FromStream`,
  an MP3 with no Xing header answers `FromBitrate`, an MPEG transport
  stream answers `FromPts`; a subtitled Matroska reports a length for
  the file while no track has a figure before probing; and an MP4's
  container and tracks report the same length to within a frame.

- **The backend measures each track's end as the walk reads packets**,
  answering `Demuxer::measured_end`.

  One running figure per track: the greatest `pts + duration` over the
  timed packets read, `pts` alone where a packet carries no duration
  (libavformat derives one for most packets whose demuxer wrote none),
  and a packet with no `pts` passed over. It is folded in before any
  payload filter, so a timed packet with no payload counts as the real
  endpoint it is. It rides the packet loop — no read, no seek — and is
  held beside the track table, reserved fallibly with it
  (`TrackTableAlloc` names a refusal). An attachment track, which is off
  the timeline, answers `None`.

  `MeasuredEnd::exact` turns `false`, for good, the first time a timed
  packet on the track is read with a duration that is not positive
  (libavformat writes zero for one it does not know) or a packet on it
  is read with no usable `pts`: the end advances to that packet's `pts`
  only, or not at all, and the figure is a lower bound. The mark is made
  when the packet is read, before the payload conversion, so it does not
  depend on whether the packet is then delivered, refused or parked, nor
  on whether it carries a payload. A duration libavformat derived from
  the stream's frame rate or frame size counts as a duration, and is
  only as exact as that derivation.

  The figure is an endpoint on the track's own timeline, not a length:
  the container's duration is a length, and turning an end into one
  means subtracting the track's start and honouring presentation edits,
  which is the composer's job and not this crate's.

  `MeasuredEnd::walk_complete` holds once a walk that skipped nothing
  has answered `Ok(None)`, and a complete walk is frozen: a later read
  or seek neither moves its figures nor takes the flag back. Before it
  completes, the walk breaks for good at a `seek` and at every packet
  `av_read_frame` produced that the session did not observe: refused and
  dropped, skipped as corrupt (`AVERROR_INVALIDDATA`), on a stream the
  track table never described (a container that adds streams after
  open, as MPEG-TS and RTP do), or on a track of unknown kind. The
  figure then keeps rising as a "so far" maximum and stays a lower
  bound. Accounting for streams that appear after open is not built; a
  session that read a packet on one never answers complete.

  An end that does not fit in an `i64` is no end: the track answers
  `None` and the walk stops being complete, where saturating would have
  recorded `i64::MAX` as an exact end over a complete walk.

  What it does not cover: libavformat probes while the container opens,
  before the session's first read, and a read error it swallows then is
  not exposed anywhere, so data skipped there is invisible to a walk
  that otherwise reads as complete.

  Pinned against real containers and against a hand-built WAV: every
  timed track of a subtitled Matroska and of an MP4 measures exactly
  what a bare `av_read_frame` loop sees; on those fixtures, whose tracks
  start at zero, an endpoint and the container's length name the same
  instant, and the two agree to within a frame; a walk stopped early
  answers a figure marked as not complete; and a seek, a dropped
  packet, a skipped corrupt read, a stream the table never described
  (an MPEG transport stream that adds audio mid-file), a track of
  unknown kind and an unrepresentable end each keep a walk from being
  called complete, while a timed empty packet at the end moves the
  measured end, a last packet with no duration leaves the end a lower
  bound, a packet read with no `pts` leaves it a lower bound even when
  its conversion is refused, and a complete walk stays frozen through a
  later seek.

### Fixed

- **A track's row no longer carries a duration libavformat filled in
  while probing.** Once probing ends, libavformat gives a stream that
  supplied no timing the container's own start and length. Every font
  and cover picture received it, and so did any timed stream whose first
  packet lies past the probe — a sparse subtitle track — so those rows
  presented the file's length as the track's own. `TrackInfo::duration`
  is now libavformat's figure for the stream from before probing, read
  between `avformat_open_input` and `avformat_find_stream_info`, and
  a figure that appears only afterwards is dropped. It is no claim about
  the file: a format's reader may compute the figure itself (WAV derives
  it from the data chunk's size) and a stream libavformat creates while
  probing has none. The per-track figure measured from packets is
  `measured_end`.

- **A path that is not valid UTF-8, or that holds a NUL byte, fails the
  open with an error** (`EINVAL`) where `ffmpeg-next`'s `input_*`
  functions panicked. The open is now this crate's own sequence of the
  two calls those functions make, `avformat_open_input` and
  `avformat_find_stream_info`, with the pre-probe read between them;
  both entrypoints (path and `Read + Seek`) take it.

## [0.15.0] - 2026-09-11

### Added

- **The backend reads `AVFormatContext.chapters`**, filling the
  `Demuxer::chapters` seat `mediadecode` 0.15.0 opens.

  The table is mirrored once at open — after
  `avformat_find_stream_info`, beside the track table — and held for
  the life of the session, so asking for it costs no packet and may
  happen at any point. A container that declares none allocates
  nothing.

  **Read raw, for the two reasons already standing in this file.**
  `ffmpeg_next`'s own chapter wrapper reads the metadata dictionary
  through `DictionaryRef`, whose `&str` is built with
  `from_utf8_unchecked` over bytes no container validates — so `title`
  takes the `metadata_text` road a track's `language` and `filename`
  already do, replacing invalid bytes rather than forming a `&str` that
  is not UTF-8 — and that wrapper dereferences an array entry without
  checking it, where this walk answers a null one by skipping it.
  Nothing read is a bindgen enum: an id, an `AVRational`, two tick
  counts and a dictionary pointer.

  **Nothing is repaired.** A chapter whose `end` precedes its `start`,
  and one whose end libavformat left at `AV_NOPTS_VALUE` because the
  file declared none, are both mirrored exactly as written — this layer
  reports what the container says.

  `mediadecode_ffmpeg::Chapter` joins `TrackInfo` and `TrackParams` as
  the `Ffmpeg`-bound alias of the core row.

  Pinned against real containers: `tests/support` grows a Matroska
  fixture whose two titled chapters are mixed in from an FFMETADATA
  sidecar, and the lanes assert the container's own ids (Matroska's
  UIDs, offset off the zero the format forbids — not the rows'
  positions), its own nanosecond timebase, both spans and both titles;
  a chapterless Matroska answers an empty table.

- **Ceilings for the chapter table: `DemuxLimits::max_chapters` and
  `DemuxLimits::max_total_chapter_title_bytes`**, with
  `DEFAULT_MAX_CHAPTERS` (4096) and
  `DEFAULT_MAX_TOTAL_CHAPTER_TITLE_BYTES` (1 MiB), both finite like
  every other seat there.

  `nb_chapters` is file-controlled and libavformat has **no**
  `max_chapters` knob, so nothing upstream bounds it: an `AVChapter` is
  four scalars and a dictionary pointer, cheap enough that a header can
  declare an enormous table for very few bytes, and the probe budget
  does not reach a count. This crate's mirror is the larger of the two —
  a row owns a title as well — so a table that parsed successfully is
  not thereby a table this process should pay for.

  The count is judged **before** the table is reserved, the reservation
  is **fallible** (`try_reserve_exact`), and titles are charged as they
  are read. Four new named faults, all refused at open:
  `TooManyChapters`, `ChapterTitleBudgetExhausted`, `ChapterAlloc` and
  `ChapterTimebaseInvalid`. (The per-title half was already bounded one
  layer down: the metadata reader refuses any single dictionary value
  past 64 KiB. What the aggregate seat adds is turning *count × that*
  into a small number.)


- **A whole-file budget for stream metadata:
  `DemuxLimits::max_total_stream_metadata_bytes`, with
  `DEFAULT_MAX_TOTAL_STREAM_METADATA_BYTES` (4 MiB).**

  A track row mirrors three values off each stream's metadata
  dictionary — `filename`, `mimetype`, `language` — eagerly, for every
  admitted stream, before a packet has been asked for. `max_streams`
  bounds how many streams a header may declare and says nothing about
  what each may carry: at that ceiling, three values of the 64 KiB a
  single metadata value may reach, in bytes that are not UTF-8 and so
  triple through lossy decoding, is roughly **562 MiB** retained during
  an open — and the path entrypoint has no hard read meter to fall back
  on. Named faults: `TrackMetadataBudgetExhausted`,
  `TrackMetadataTooLong`, `TrackMetadataAlloc`, each naming the stream
  and which of the three values it was reading.
### Changed

- **A timebase this backend cannot represent is now a named refusal
  rather than a clamp — and, in one case, rather than a panic.**

  `AVStream.time_base` and `AVChapter.time_base` are both
  file-controlled. The conversion behind them clamped a non-positive
  *denominator* up to 1 and handed the numerator to `Timebase::new`
  unexamined. Two faults in one line: the clamp fabricated a ruler a
  consumer could not tell from one the file declared, and
  `Timebase::new` **asserts** a non-negative numerator. libavformat's
  FFMETADATA parser stores `TIMEBASE=-1/1000` verbatim — measured with
  `ffprobe`, which reports `time_base=-1/1000` — so sixty bytes of text
  were enough to panic a safe `open`/`open_reader`, or to kill the
  process under `panic=abort`.

  The conversion is fallible now and nothing is invented. A stream
  whose declared timebase is not one fails the open with
  `TrackTimebaseInvalid`; a chapter's with `ChapterTimebaseInvalid`.
  Both name the stream or chapter and the rational exactly as the
  container wrote it.

  **The chapter rule is the stricter of the two, by the zero
  numerator.** `0/1` is libavformat's own default for a stream whose
  demuxer never set one, so a track seat reads it as the container
  declaring nothing and passes it through — as it always has. A
  chapter's ruler is written by whatever wrote the chapter
  (`avpriv_new_chapter` takes it as an argument), so the same value
  there is not an absence but a claim that every boundary in the table
  falls on one instant.

  **This is a behaviour change** for a file whose stream declares a
  malformed rational: 0.14.0 opened it with a `1/1` invented here, and
  0.15.0 refuses it by name. That file's timestamps were never
  meaningful; what changes is that it now says so instead of looking
  well-formed.

- **Metadata is measured, charged and only then materialised — in that
  order.**

  A chapter title was built before the budget was consulted: a lossy
  decode and a `SmolStr`, both infallible, so a 65,535-byte title did
  that heap work under a budget of **zero**, and an allocator failure
  aborted a safe `open` rather than answering. The reader now hands
  back a *borrow* of libavutil's buffer, so a value's size is known
  before any owning conversion exists and a refused value costs no heap
  at all. What is then built is built through a fallible reservation
  (`String::try_reserve_exact`), with `ChapterTitleAlloc` /
  `TrackMetadataAlloc` naming an exhaustion.

  The charge is the **decoded** size, not the raw one: lossy decoding
  replaces each invalid sequence with a three-byte `U+FFFD`, so the raw
  length would under-charge exactly the hostile input. It is computed
  without decoding, and its agreement with `String::from_utf8_lossy` is
  asserted rather than argued.

  `SmolStr::new` itself remains infallible — `smol_str` offers no
  constructor that is not, and its inline storage covers 23 bytes. What
  that leaves is one allocation of a size already charged against a
  budget *and* already served once by the allocator, which is stated
  here rather than papered over.

- **A metadata value with no terminator inside the 64 KiB per-value cap
  is refused by name, where it used to read as absent.**

  The reader answered `None` for an over-long value exactly as for a
  missing one. A declared chapter title of 65,536 bytes therefore
  reached a consumer as an *untitled* chapter — the mirrored table
  silently disagreeing with the container — and, retaining nothing,
  escaped every budget charged against it, including a zero one.
  Chapters answer `ChapterTitleTooLong`; a stream's three values answer
  `TrackMetadataTooLong`.

  `ChapterTitleTooLong` is deliberately **not** folded into
  `ChapterTitleBudgetExhausted`: the budget error is answered by
  raising `max_total_chapter_title_bytes`, and this one names a
  structural cap no seat can move — folding them would offer a knob
  that cannot fix it.

- **(BREAKING) The bare packet converters take the stream's timebase.**
  `video_packet_from_ffmpeg`, `audio_packet_from_ffmpeg` and
  `subtitle_packet_from_ffmpeg` stamped `Timebase::default()` — 1/1 —
  onto raw stream ticks. An `AVPacket` does not carry its stream's
  timebase, so PTS 90,000 from a 1/90,000 stream became a
  *self-describing* timestamp reading 90,000 seconds, with nothing in
  the value marking 1/1 as a placeholder rather than a reading. They
  now take a `Timebase` and stamp what the caller declares. Callers
  pass the track row's `timebase()`; the `*_in` variants, which already
  required one, are unchanged.

- **(BREAKING) The subtitle frame converters no longer take a
  timebase.** `av_subtitle_to_subtitle_frame`,
  `av_subtitle_to_owned_subtitle_frame` and `subtitle_frame_from` took
  a stream `Timebase` that could not affect the answer — see the PTS
  fix below — and a parameter that cannot affect the answer is how that
  bug survived review. Drop the argument at the call site.


- **(BREAKING) The `Ffmpeg` adapter's text carrier is `Utf8Bytes`, not
  `SmolStr`** — so `TrackInfo::filename`, `mime_type`, `language` and
  `Chapter::title` hand back `&Utf8Bytes`. `as_str()` reads the same as
  before; a caller that named `SmolStr` renames the type.

  Not a preference. A metadata value's length is container-controlled,
  so the demuxer measures it, charges it against a budget and builds it
  in a `String` reserved with `try_reserve_exact` — and that chain is
  worth nothing if the last step allocates again. `SmolStr::new` takes
  a `&str` and copies it into a fresh `Arc<str>` past 23 bytes: a
  **second, infallible** allocation of an attacker-sized value, made
  while the first was still live, so failing it aborted the process the
  budget existed to keep alive. `Utf8Bytes::from(String)` **moves** the
  buffer instead — inline under `smol_bytes::INLINE_CAP`, and otherwise
  through `bytes::Bytes::from(Vec<u8>)`, which takes the vector's own
  allocation over rather than copying it.

  The one residue, stated rather than glossed: `bytes` moves the buffer
  outright when length equals capacity, which is what
  `try_reserve_exact` plus exactly that many bytes produces; if an
  allocator returns more capacity than was asked for, it allocates a
  fixed-size 32-byte refcount header — the same for a ten-byte title
  and a 64 KiB one. What is gone is the part an attacker could scale.


- **(BREAKING) The text carrier across this crate is
  `smol_bytes::Utf8Bytes`; the `smol_str` dependency is gone** — so
  `TrackInfo::filename`, `mime_type`, `language` and `Chapter::title`
  hand back `&Utf8Bytes`. `as_str()` reads the same as before; a caller
  that named `SmolStr` renames the type.

  Not a preference. A metadata value's length is container-controlled,
  so the demuxer measures it, charges it against a budget and builds it
  in a `String` reserved with `try_reserve_exact` — and that chain is
  worth nothing if the last step allocates again. `SmolStr::new` takes
  a `&str` and copies it into a fresh `Arc<str>` past 23 bytes: a
  **second, infallible** allocation of an attacker-sized value, made
  while the first was still live, so failing it aborted the process the
  budget existed to keep alive. `Utf8Bytes::from(String)` **moves** the
  buffer instead — inline under `smol_bytes::INLINE_CAP` (62 bytes, no
  allocation at all) and otherwise through `bytes::Bytes::from(Vec<u8>)`,
  which takes the vector's own allocation over rather than copying it.

  Measured, not asserted: `tests/metadata_allocation.rs` installs a
  counting global allocator and watches the conversion itself. A 65,535
  byte value — the largest single metadata value the reader admits —
  reaches the carrier with **zero** allocations, as does the first
  value past the inline window; the `Arc<str>` copy the old carrier
  made shows up in the same file as one allocation of the full 65,535
  bytes.

  The swap reaches every text seat **this crate owns**:
  `ContainerFormat`'s names, `CodecId`'s and the pixel-format namer's,
  the reader-panic message, and the metadata seats above.

  `smol_str` remains a dependency for exactly one file,
  `channel_layout.rs`, and the reason is a boundary rather than an
  oversight: those labels are handed to **`mediaframe`**'s own
  vocabulary — `ChannelSpec::with_label`, `ChannelLayoutDescription::
  with_text` — whose seats are `SmolStr`, and that carrier is
  mediaframe's to choose. Nothing on that road is charged against a
  budget or sized by a container, so the allocation-failure argument
  above does not reach it.

- **The standalone `VideoDecoder` can declare a packet timebase.**

  This release's first cut wrote `AVCodecContext.pkt_timebase` only
  from the crate-private constructors the stream decoder uses; every
  public `VideoDecoder::open*` routed `None`. A direct caller feeding
  timestamped packets therefore had no way to tell libavcodec what
  their ticks meant — and not owning the `AVStream` does not mean not
  knowing its ruler. `open_timed`, `open_with_frame_limits_timed`,
  `open_with_timed` and `open_with_limits_timed` declare it, and it
  reaches every backend state including later hardware probe advances.
  The four existing constructors remain as **explicitly untimed**
  conveniences and say so in their docs.


- **Every allocation between admission and a successful open is
  fallible.**

  The budgets decide what a container may spend before a byte is
  copied — and then the copies themselves were infallible, so a file
  whose footprint the caller's own ceilings had *admitted* could still
  abort the process when the allocator declined. A budget that ends in
  an abort is not a budget.

  `FfmpegBytes` now holds `Arc<Vec<u8>>` rather than `Arc<[u8]>`: the
  attacker-scaled half goes through `Vec::try_reserve_exact` and the
  `Arc` is left holding three words, because stable has no fallible
  `Arc<[u8]>` and `Arc::new_uninit_slice` aborts like every other
  infallible allocator call. An inline-capable carrier was considered
  and rejected — inline storage makes `Clone` copy small payloads, and
  `FfmpegBytes::ptr_eq`, the amputation contract's own instrument, asks
  whether a clone was a refcount bump.

  On that footing: `FfmpegBytes::try_copy_from_slice` is the road every
  budgeted copy takes, `from_rows` reserves fallibly (and loses its
  `MaybeUninit` gather, and the written-every-slot argument with it),
  the codec-ticket mirror returns `Result` — extradata, each
  coded-side-data payload, the descriptor table and the custom channel
  map — and the track table, the attachment queue and the handle
  vector are all `try_reserve`d. `TrackTableAlloc` joins
  `ParametersAlloc`, `ChapterAlloc`, `ChapterTitleAlloc` and
  `TrackMetadataAlloc` as a named open failure.

  **What stays infallible, and why it cannot scale.** One `Arc::new`
  per track row: a fixed-size header, the row's own variable weight
  already sitting in carriers allocated fallibly above, and the count
  bounded by the `max_streams` ceiling. One `Arc` header per carrier,
  likewise fixed. Stable Rust has no fallible `Arc`, and neither of
  these grows with anything a file says.

  Watched rather than asserted: `tests/metadata_allocation.rs` opens a
  real container under a counting allocator and pins that no single
  allocation approaches the admitted per-stream ceiling — which is what
  a staged-then-copied payload would look like — and that the total
  stays in proportion to the file rather than to the budget.


- **The refcount is `triomphe::Arc`, so the last aborting allocations
  on the open path are gone.**

  The previous round moved the attacker-scaled bytes into a
  `try_reserve_exact`ed `Vec` and handed that to `std::sync::Arc` — and
  the `Arc` still aborted, once per coded-side-data entry, a count the
  file controls. `std::sync::Arc` has no fallible constructor on stable
  at all. `triomphe::Arc` does: `UniqueArc::try_new_uninit_slice` puts
  the payload in the allocation directly, with no intermediate `Vec`
  and no second copy, and `Arc::try_new` covers the per-row handle.
  `FfmpegBytes` also gains an `Empty` variant, so the empty carrier —
  which a video frame mints three times per decoded picture — costs no
  allocation at all instead of sharing a `OnceLock` singleton.

  Pointer identity survives, which is what ruled out the inline-capable
  carrier the text seats use: `ptr_eq` is the amputation contract's
  instrument for asking whether a clone was a refcount bump, and a
  carrier that copies small payloads cannot answer it.

  Swept with it: `CarrierOps::commit` returns `Option` (the resampler's
  owned lane copies there), the side-data road on the packet boundary
  lost its staged `Vec` and the second full-size copy that followed it,
  the track handle is `triomphe::Arc::try_new`, and the subtitle
  converter reserves its rect table and each text increment before
  writing — mapped to the error `parks_in_decode` already recognises,
  so a refusal keeps the pending cue for another attempt rather than
  taking the process.

- **A custom channel layout is validated before FFmpeg is allowed to
  describe it.**

  `av_channel_layout_describe` renders a `CUSTOM` layout by walking
  `u.map[i]` for each of `nb_channels`. The description helper formed
  the layout reference and called it *before* anything looked at the
  map, so a live layout declaring channels with a null map read from
  null inside FFmpeg — and the codec ticket's own null-map refusal
  happens later and could not reach it. The helper's contract asks its
  caller for a live pointer and nothing more, so such a layout is an
  input, not a caller error.

  The order is now: fold the raw `order`, read `nb_channels`, and for a
  custom order check `u.map` — all before the reference exists.
  `ChannelLayoutFault::MalformedCustomMap` names the refusal, and the
  demuxer maps it to `ParametersChannelMap`.

- **Channel-layout materialisation is fallible, and its labels no
  longer allocate.** The channel table and FFmpeg's rendering buffer
  are `try_reserve`d; each sixteen-byte label is decoded on the stack
  instead of through a `String`, so the only allocation left on that
  road is `SmolStr`'s own, bounded at forty-eight bytes by the source
  array's width.


- **The owned resampler allocates before the conversion, so a refusal
  cannot cost a converted sample.**

  Making `commit` fallible was not enough — it was the wrong place. The
  owned lane allocated its carrier there, *after* `swr` had consumed
  the input, so a refusal became `None` while `finish_output` had
  already advanced `next_pts`, the send arm still answered `Accepted`,
  and the samples were gone with no error and no retry. That is worse
  than the abort it replaced.

  `CarrierOps::reserve` takes the allocation now, before `swr` runs,
  where the existing `OutputBuffer` error already reports it; `commit`
  returns a carrier rather than an `Option` and cannot fail at all.
  `FfmpegBytes` grew a length distinct from its allocation so a
  reservation can be named at its true size without a second copy.
  This restores an invariant the resampler's own docs already claimed
  — "nothing below can fail at all".

- **A custom channel map is read on one road only, and that road is
  `unsafe`** — `channel_layout_from_ffmpeg` and
  `channel_layout_description_from_ffmpeg` refuse a `CUSTOM` order with
  `ChannelLayoutFault::UnverifiableCustomMap` rather than dereferencing
  it. (BREAKING for a caller who passed one; both already return
  `Result` in this release.)

  Validating harder was the wrong answer and this release tried it
  first. `ffmpeg_next::ChannelLayout` is a public newtype over a public
  `AVChannelLayout`, so safe Rust can write `nb_channels = 2` beside a
  `u.map` that points at one entry, or at a dangling address, or at
  something misaligned — and **the loop that would check those entries
  is itself the out-of-bounds read**. Neither provenance nor extent is
  observable from a pointer; every FFmpeg helper here
  (`av_channel_layout_describe`, `av_channel_layout_compare`) indexes
  the same way. No check placed behind a safe signature can establish
  what that signature failed to demand.

  So the extent is demanded of the caller instead.
  `channel_layout_description_from_raw_ptr` is the single road that
  reads a map, and its contract now requires a live, aligned array of
  exactly `nb_channels` entries. Inside this crate the demux and convert
  roads satisfy it from FFmpeg's own `AVCodecParameters` and `AVFrame`,
  where `av_channel_layout_copy` allocated the map and sized it, and
  each call argues exactly that in its `SAFETY` comment. What the
  pointer road still *checks* are content faults rather than extent
  ones — a null map beside a positive count, a non-positive count, and
  a sixteen-byte name with no NUL inside it (FFmpeg hands that fixed
  array to a `%s` conversion) — all refused as `MalformedCustomMap`
  before any helper sees the layout.

- **mediaframe 0.11: the text seats take `Utf8Bytes`, and a channel
  label or a layout rendering is carried WHOLE.** (Floor
  `mediaframe = "0.11"`; `smol_str` leaves the dependency graph.)

  The workaround this replaces was honest and lossy. mediaframe's seats
  took `SmolStr`, whose constructor is infallible and allocates past a
  twenty-three-byte inline window — on a road that runs once per channel
  of a file-declared count, that is an abort this crate could not
  report. So anything longer was reported **absent**: truthful about
  what the crate did, silent about what the container said.

  **`UnsupportedPixelFormat` is a compact tag now** (BREAKING: the
  `format()` accessor is gone and `new` takes `(raw, name)`). Two
  rounds landed on this payload. mediaframe 0.11 widened
  `PixelFormat`'s text arm, which made it the biggest arm of
  `ConvertError` at 144 bytes — a cost every `Result` on the convert
  road pays on its *success* path, and what `clippy::result_large_err`
  reports. Boxing the format fixed the size and introduced something
  worse: an **infallible allocation on the refusal path**, so a
  container-selected unsupported format could abort the process
  precisely while the converter was trying to report it.

  The payload carries the raw `AVPixelFormat` integer and libavutil's
  own name for it, **borrowed** from the static descriptor table that
  outlives the process. Twenty-four bytes, `const fn new`, and no field
  that can allocate — `ConvertError` is **32 bytes**, smaller than
  before mediaframe 0.11. The vocabulary's `PixelFormat` is gone from it
  rather than boxed, and that field's own documentation is why it costs
  nothing: it already said `raw` and `name` are where the identity
  survives. The copying `pix_fmt_name` reader went with its last caller.

- **`table_text` borrows FFmpeg's static tables instead of copying
  them.** It built an owned `Utf8Bytes` from the borrowed bytes, which
  **allocates** past `smol_bytes::INLINE_CAP` — and FFmpeg's own tables
  reach past it: the SER demuxer's long name is sixty-five bytes. So
  opening an ordinary container, or asking a codec for its description,
  allocated infallibly to duplicate a string the process already owns
  for its whole life.

  The reader returns `&'static str` now, and its contract says why that
  is sound rather than merely convenient: every caller reads
  libavformat's `AVInputFormat` entries or libavcodec's
  `codec_descriptors[]`, both `static const` tables compiled into those
  libraries. `ContainerFormat` and `CodecId` store the borrow with
  `Utf8Bytes::from_static`, which keeps a static slice rather than
  copying it, at any length. A lane walks a wide span of codec ids under
  the counting allocator, asserts the scan costs **nothing**, and
  asserts it really met a name past the inline window — so it cannot
  pass by never reaching the case it is about.

  Swept with it: a `panic!("literal")` payload is already `'static` and
  is stored rather than copied. The one borrowed-`&str` conversion left
  in the crate is the `String` arm of that same panic reader, where the
  text is a caller's own message rather than anything a container
  chooses, and where a clone-then-move would cost exactly what the copy
  costs.

- **A rendering too long to be a layout slug is no longer parsed.**
  `ChannelLayout::from_str` is total: what it does not recognise it
  wraps in `Other`, and because the input is *borrowed* that constructor
  copies — infallibly — for anything past `smol_bytes::INLINE_CAP`, only
  for the caller to discard it. An unnamed layout whose rendering the
  container controls therefore paid for an allocation this crate had no
  use for and could not refuse. Renderings past that window skip the
  parse; the longest slug the vocabulary recognises is fourteen bytes
  against a window of sixty-two, and a lane walks FFmpeg's own
  `av_channel_layout_standard` roster to prove no named layout renders
  longer than the bypass allows. (A `parse_known` that never constructs
  `Other` is filed against mediaframe for 0.11.1; this is the surgical
  fix on this side of the seam.)

  The road is fallible end to end now: measure the lossy decoding
  without producing it, `try_reserve_exact` that much, build it, and
  **move** the buffer into the carrier — the same two helpers
  (`lossy_len`, `lossy_text`) the metadata road already uses, shared
  rather than restated. Nothing is truncated, nothing is dropped, and an
  allocator refusal is `ChannelLayoutFault::Alloc` by name. The
  counting-allocator lanes cover the new road.

- **~~Nothing on a file-scaled path builds a heap `SmolStr`~~** — the
  gates below were the workaround, and they are gone with the seat that
  forced them. Kept as a record of what the constraint was:

  A channel label and a layout rendering are decoded into a fixed stack
  buffer and reported *absent* when the decoding will not fit
  `SmolStr`'s twenty-three-byte inline window. The rendering's guard
  compared the **raw** length and then called
  `String::from_utf8_lossy`, which is two allocations the check did not
  cover: the decoder allocates whenever its input is not valid UTF-8,
  and twenty-three raw bytes expand to sixty-nine, so `SmolStr::new`
  allocated as well. FFmpeg renders a `CUSTOM` layout by concatenating
  names the file wrote, so "not valid UTF-8" is an input rather than an
  accident.

  One helper now serves both seats, with a raw-length gate that is
  sound because lossy decoding never shrinks — a valid byte stays one
  byte, an invalid one becomes three — so an input already past the
  window cannot decode to something that fits, and what survives is
  bounded by a constant rather than by the file. Both mediaframe seats
  document an empty value as "none", which is a truer answer than an
  allocation this crate cannot refuse.

- **The frame and image side-data collectors no longer panic on OOM.**
  Both used `FfmpegBytes::copy_from_slice`, whose body is now an
  `expect` — a panic instead of an abort is not an improvement. Both
  use the fallible constructor and return `Result`, propagating as
  `CarrierAllocFailed` so a timed decoder parks its scratch frame. The
  infallible constructor is `#[cfg(test)]` now, which makes "no product
  path reaches it" a compile-time fact rather than a grep: **a fallible
  staging allocation does not protect a subsequent infallible copy.**

- **A malformed channel layout is permanent, not retryable.** The audio
  converter mapped both `MalformedCustomMap` and `Alloc` to
  `CarrierAllocFailed`, which `parks_in_decode` treats as transient — so
  a decoder held a structurally broken frame and returned the same
  error forever. `ConvertError::MalformedChannelLayout` is the
  permanent class; only `Alloc` stays transient.

- **Frame side data is never silently dropped.** `collect_side_data`
  still reserved a staging `Vec` after the carrier had been made
  fallible, and `continue`d past the entry when that reservation
  failed — while the function went on to return `Ok`. A
  mastering-display, content-light or caption annotation could
  therefore disappear under momentary memory pressure, and nothing
  downstream could tell that frame from one whose file never carried
  any. The dead staging is gone; the carrier's own refusal propagates
  as `CarrierAllocFailed`, which parks the frame for a retry. Entries
  are still left out for `SIDE_DATA_MAX_ENTRIES` and
  `SIDE_DATA_MAX_TOTAL_BYTES`, which are properties of the file rather
  than of the machine.

- **The decoder error family no longer derives `Clone`.** (BREAKING for
  a caller that cloned one.) `Error::AllBackendsFailed` and
  `Error::FallbackFailed` carry `Vec<ffmpeg_next::Packet>`, and
  ffmpeg-next 9.0.0's `Packet::clone` calls `av_packet_ref` and
  `av_packet_make_writable` while **discarding both return codes** — so
  a safe `err.clone()` under memory pressure yields empty or incomplete
  packets with nothing said. Those packets are the rescued replay
  history a non-seekable input has no other copy of, which makes a
  silent partial clone the difference between a recoverable failure and
  irreversible loss.

  `Clone` is gone from `Error` and from both payloads, and — transitively
  — from `AudioDecodeError`, `ImageDecodeError` and `ResampleError`,
  which wrap it. No product code cloned any of them; the census found
  the only `.clone()` calls in that family were on a pixel format and in
  tests. Where a duplicate really is needed there is a checked road
  already: `FfmpegBuffer::try_clone` reports `av_buffer_ref`'s failure
  rather than swallowing it.

- **One dispatcher decides which arm of a channel layout's union is
  readable, and every read in the crate goes through it.**
  `channel_layout::LayoutArm` names what each order defines — `u.mask`
  for `NATIVE` and `AMBISONIC`, `u.map` for `CUSTOM`, **nothing** for
  `UNSPEC` and for any order this build has never heard of.

  The rule had to be found twice to be written once. It was applied to
  the resampler's equality and `Debug`, and the codec-ticket mirror was
  still reading `u.mask` for an unspecified layout — importing storage
  FFmpeg declares undefined into `ChannelLayoutTicket::mask`, where the
  public `mask()` hands it out and the derived `Debug`, `PartialEq` and
  `Hash` expose and compare it. Two equivalent unspecified layouts could
  mirror to tickets that were not equal, and a log line could print
  bytes nobody wrote. The rebuild road had the mirror image of the
  defect: it wrote a mask back for an order that defines none.

  Two dead helpers went with it — `custom_channels` and
  `custom_channel_label`, superseded by the raw walk — because both
  formed `&[AVChannelCustom]` over FFmpeg's map, which is the undefined
  behaviour that raw walk exists to avoid: each entry carries an
  open-enum `id`, and a container writing a value outside this build's
  discriminant set makes the *reference* unsound before any `match` on
  it can run. Unreachable code that would be unsound if revived is a
  landmine rather than documentation.

  `ChannelLayoutTicket::is_custom` is gone, and its removal is the point
  rather than tidying: every caller asked "is this custom?" and treated
  `false` as "mask-backed", which is exactly how the undefined read got
  in. The question has three answers, not two. The resampler's staged
  layout also stopped reading the mask for `NATIVE` *alone*, which had
  quietly dropped an ambisonic layout's non-diegetic channels.

- **`ResampleSpec`'s `Debug` is hand-written, and its equality is
  order-aware.** `AVChannelLayout`'s union is **undefined** for
  `AV_CHANNEL_ORDER_UNSPEC` per FFmpeg's own header, and this crate read
  `u.mask` for every order that was not `CUSTOM` — so two valid
  unspecified layouts of the same width compared *unequal* whenever the
  bytes in storage neither of them owns happened to differ, and merely
  logging a spec read that storage too (the derived `Debug` delegates to
  `ffmpeg_next`'s formatter, which prints the mask unconditionally).

  A private `LayoutArm` now names which arm an order actually defines —
  `mask` for `NATIVE` and `AMBISONIC`, `map` for `CUSTOM`, **nothing**
  for `UNSPEC` and for any order this build has never heard of — and
  both traits consult it. An unspecified layout is its order and its
  width; a future order gets the same treatment until this crate is
  taught which arm it defines.

- **`ResampleSpec` no longer derives equality**, because the derive was
  a safe `==` with a raw pointer dereference behind it. (BREAKING only
  in the sense that two custom layouts now compare by identity; the type
  is still `PartialEq + Eq`.)

  `ffmpeg_next::ChannelLayout` implements `PartialEq` as
  `av_channel_layout_compare`, which walks `u.map[i]` for a `CUSTOM`
  order. A safe caller could therefore forge a layout declaring channels
  it has no map for, wrap it in two specs, write `a == b`, and reach a
  dereference inside FFmpeg **without constructing a resampler or
  running a line of this crate's code**. No preflight can dominate an
  operator, which is the lesson: the census that found every helper call
  had not looked at the traits a `derive` writes.

  Equality is hand-written and reads nothing through a pointer: the raw
  `order`, then the channel count, then the union's `mask` for every
  order that uses it. A `CUSTOM` order compares by **map identity**,
  never contents — the extent a safe function cannot establish is the
  extent it does not read — so two custom layouts describing identical
  channels through different allocations compare unequal. That is
  conservative in the only direction that matters: the single product
  use of layout equality is `check_pair`'s short-circuit, where
  "unequal" means the rematrix check *runs*.

- **The preflight is `av_channel_layout_check`, line by line.** Two of
  its rules were missing. A `CUSTOM` map entry whose `id` is
  `AV_CHAN_NONE` is a **hole in the map**, which `check` walks the map
  to refuse — admitted, it reached a codec ticket and was published to a
  consumer as `u32::MAX`. And `check` opens with `if (nb_channels <= 0)
  return 0;`, so a `NATIVE` layout declaring no channels with an empty
  mask is invalid even though that order's own rule
  (`popcount == nb_channels`) is satisfied by it.

  Both are refused now. The one deliberate divergence is narrow and
  documented: the all-zero `AVChannelLayout` — `UNSPEC` with no channels
  — is how a container says it declared none, no FFmpeg helper is ever
  called for it, and refusing it would turn away every stream that
  simply has no layout.

- **One preflight decides every channel-layout order, and every FFmpeg
  layout helper in this crate is behind it.** Ten rounds closed the
  custom-map road and left the rest on the argument that an order
  describing its channels through a `uint64_t` mask cannot be malformed.
  The mask is not the only field: `nb_channels` is an `int` a caller
  writes into a public struct, and `av_channel_layout_describe` computes
  `nb_channels - popcount(mask)` and takes an integer square root of it
  without checking either — so a safely constructed `AMBISONIC` layout
  with an extreme count reached FFmpeg's arithmetic through a **safe**
  API.

  `channel_layout::layout_preflight` is now the single rule: a count
  outside the range every downstream calculation assumes, a `NATIVE`
  mask whose population disagrees with its count, an `AMBISONIC` layout
  whose mask leaves no channel for the ambisonic part
  (`popcount(mask) < nb_channels`, which is `av_channel_layout_check`'s
  own rule verbatim), and the existing custom-map rule.

  **The ambisonic arm is `check`'s rule and only it.** A first cut also
  demanded that the remainder be a perfect square. That property is
  real, but it is `av_channel_layout_ambisonic_order`'s question — the
  function whose own comment calls the failing case "incomplete order -
  some harmonics are missing" and which answers `AVERROR(EINVAL)` for
  it. `check` never consults it, FFmpeg describes and converts such a
  layout, and refusing one here turned a valid file away. Asking a
  stricter question than the library asks is not caution; it is a
  different answer to a question nobody posed. The count bound stays —
  it guards FFmpeg's own `int` arithmetic, which is a separate concern
  from validity. It allocates nothing, and it dominates every
  `av_channel_layout_*`, `avcodec_parameters_*` and `swr_*` call that
  takes a layout: the safe conversions, the demux admission pass, the
  codec ticket, `bounded_clone_parameters`, `build_codec_context` and
  the resampler's construction. New named refusals:
  `DemuxError::ParametersLayoutShape`, `Error::MalformedChannelLayout`
  and `Error::ChannelMapMissing` — the last two **structural and
  permanent**, deliberately not the allocation arm.

- **Opening a decoder validates the layout before it measures or
  copies.** `build_codec_context` checked only a non-null `Parameters`
  and the heap footprint, and `measure_parameters` prices a custom map
  from `nb_channels` without looking at `u.map` — so a positive count
  with a null map passed the budget and reached
  `avcodec_parameters_to_context`, whose `av_channel_layout_copy`
  `memcpy`s from that null. Public decoder opens could therefore crash
  on malformed or version-skewed stream parameters.
  `bounded_clone_parameters` had the same bypass before its own direct
  copy. Both run the shared preflight first.

- **The resampler refuses a `CUSTOM` layout at construction, and is
  `Send`.** `swr_alloc_set_opts2` and `swr_build_matrix2` take an
  `AVChannelLayout` and assume every invariant
  `av_channel_layout_check` states; both specs are preflighted before
  `swr` is handed either. A custom order is refused outright, because
  its `u.map` extent is not something any signature here can establish —
  and that refusal is what makes every layout a live resampler holds a
  plain mask value rather than a struct with a bare pointer in it, which
  is the invariant behind the new `unsafe impl Send for
  CarrierResampler<C>`. **`Send` only, never `Sync`**: the `SwrContext`
  is owned by the value and reached solely through `&mut self`, which is
  the serialisation; `&self` would not provide it. A static lane asserts
  `Send` on both carrier lanes.

- **A parked attachment's structure is judged during admission.**
  `PacketBuffer` is not one fault — a `TRUSTED` payload, a `data`/`size`
  pair outside the buffer it claims, a buffer somebody else references,
  flags outside the portable set, **and** the allocator declining the
  carrier — and only the last is unforeseeable. `payload_of` is split
  into `preflight_payload`, which decides all the deterministic ones
  without reading a byte or allocating, and the capture that acts on its
  plan; `admit_streams` runs the preflight for every parked attachment.
  A bad final attachment no longer costs the whole table first.

- **One rule decides a custom channel map, and it decides it before any
  copy.** The structural validator the admission pass relies on checked
  each entry's `opaque` but not that its sixteen-byte `name` carries a
  NUL — a rule the *describe* road applied and this one did not, so a
  malformed final audio stream was admitted and then refused during
  materialisation, after every earlier stream had been paid for. Two
  rules for one fact is how that happens, so there is one now:
  `channel_layout::custom_map_fault` decides null map, non-positive
  count and unterminated name together, allocates nothing, and is what
  both the describe road and the codec ticket's validator call.

  One visible consequence, recorded rather than papered over: a custom
  layout declaring a *negative* channel count used to be refused as
  `ParametersTooLarge`, because the footprint
  (`nb_channels * size_of::<AVChannelCustom>()`) could not be computed
  and that gate was asked first. It is `ParametersChannelMap` now,
  which is the truer of the two statements about that file — a
  malformed map, not an oversized one.

  `CodecTicket::from_raw` also had the ordering defect on its own
  account — it copied `extradata` and every coded-side-data payload and
  only then reached the layout — so the complete validator is now its
  first statement, before the footprint is even measured. A lane pins
  that as an ordering rather than an outcome: a parameter set that is
  *both* malformed and over budget reports the structure, which only
  the new order can do.

- **The judge-before-pay rule now covers the refusals that are not
  budgets.** A malformed `AVStream.time_base`, a non-null
  `ch_layout.opaque`, and a `CUSTOM` channel order with no map or a
  non-positive count are all permanent, deterministic facts about the
  container — none of them depends on how much memory the machine has —
  yet each was discovered during materialisation, so one on the *last*
  stream was refused only after every earlier stream had been paid for.

  All three are decided in `admit_streams` now. The stream ruler goes
  through a new `stream_timebase`, and the layout's structure through a
  new `validate_channel_layout` split out of the ticket builder; both
  are called again where the values are actually used, so there is one
  conversion and one error construction with two call sites rather than
  two rules. Neither allocates: the layout check is a pointer walk.

  *On reachability, stated rather than implied:* unlike a chapter's
  timebase — which libavformat stores exactly as an FFMETADATA sidecar
  wrote it, `TIMEBASE=-1/1000` included — a stream's normally arrives
  through `avpriv_set_pts_info`, which refuses a non-positive value
  itself, and no container was found that reaches the refusal. It is
  kept because demuxers may assign `st->time_base` directly and one
  comparison is not worth trading for an assumption about every demuxer
  in libavformat.

- **Stream metadata is charged in the admission pass, not during
  materialisation.** The third instance of one defect: a value crossing
  the whole-file metadata budget on the *last* stream was refused only
  after every earlier stream's parameter clone and attachment carrier
  had been retained and that stream's own codec ticket copied — so a
  file certain to be refused could be made to cost the whole aggregate,
  on every open. `admit_streams` now measures `filename`, `mimetype`
  and `language` across every stream, allocating nothing, before
  `build_tracks` reserves a row; the materialisation-time charge stays
  as defence in depth and is what reports an allocator refusal, which
  no measurement can foresee. Both passes call one `charge_metadata`,
  so they cannot become two rules.

  With it — and with the structural refusals above — every refusal an
  open can raise from the container's declared facts is now raised
  before the process has spent anything on the file: probe bytes
  (metered during the read), stream count (libavformat's own option,
  set before the header is parsed), per-stream and whole-file codec
  parameters — extradata, every coded-side-data payload and a custom
  channel map, all through `measure_parameters` — per-attachment and
  whole-file attachment payloads, whole-file stream metadata, chapter
  count and chapter titles. `from_input` carries that list in a comment
  so the class can be checked rather than rediscovered.

- **The chapter table is judged before the track table is built.** Both
  checks were correct and their order was the defect: `from_input`
  materialised every track — attachment carriers up to 256 MiB by
  default, codec-parameter mirrors, retained metadata, one `Arc` per
  row — and only then ran `build_chapters`'s cheap count check. A file
  certain to be refused for its chapter count could first be made to
  cost hundreds of megabytes, and repeating the open repeated the bill.

  A new allocation-free admission pass runs beside stream admission,
  before either table exists: the declared count against
  `max_chapters`, every chapter's timebase, and every title's decoded
  length against the aggregate budget — the title measurement borrows
  libavutil's own buffer, so an over-budget table is refused having
  touched no heap at all.

- **`FfmpegBytes` equality and hashing read the span, not the
  allocation.** The derived implementations compared and hashed the
  whole backing buffer, and a carrier built from a reservation
  deliberately keeps capacity past its span — so it compared *unequal*
  to an exact copy of the very same bytes, and hashed differently.
  Anything keyed on a payload missed. `PartialEq`, `Eq` and `Hash` are
  written by hand over `as_slice()` now; `ptr_eq` remains the
  instrument for the other question, and is deliberately not what `==`
  answers.

### Fixed

- **Every timed decoder now writes `AVCodecContext.pkt_timebase`.**

  The shared codec-context builder never set it, on any road. FFmpeg
  documents the field as caller-supplied and libavcodec reads it: its
  generic subtitle path derives `AVSubtitle.pts` and the
  packet-duration fallback **only** when it is set, so a subtitle
  decoder returned timestamped cues carrying neither. The audio and
  subtitle sessions already held a validated stream `Timebase` and
  merely kept it Rust-side; the video stream decoder now carries one
  into every context it opens, hardware probe advances included.

  Audited across every road into the builder: audio and subtitle pass
  the stream's timebase; the video *stream* decoder passes its own on
  both the software and hardware paths and holds it for later probe
  advances; the standalone `VideoDecoder` and the still-image decoder
  pass none, because neither is opened against a stream and inventing
  one would be the very defect above.

  A zero-numerator timebase is deliberately **not** written: unset, the
  field is `0/1`, which is exactly what a container that declared no
  timebase gives this crate — the two are the same statement, and
  writing it would claim a ruler the file never did.

- **A decoded subtitle's PTS is labelled in microseconds, as FFmpeg
  defines it.**

  `AVSubtitle.pts` is in `AV_TIME_BASE` units whatever the stream's own
  timebase is; the converter attached the *stream* timebase without
  rescaling, so a cue five seconds into a 1/1000 stream reported itself
  as 5,000 seconds. It is now labelled `Timebase::MICROS`.

  **Labelled, not rescaled.** A `Timestamp` carries its own timebase
  and compares by the instant it names, so a microsecond label is
  already directly comparable with a packet timestamp in any other
  ruler — and it is exact, where a rescale into a coarser stream
  timebase would round a cue boundary for nobody's benefit. A consumer
  that wants the stream's ruler asks by name, with
  `Timestamp::rescale_to`.

  The two fixes are pinned by one lane, because the first hid the
  second: with `pkt_timebase` unset libavcodec left `AVSubtitle.pts`
  absent, so the mislabelling was unreachable through ordinary
  decoding and only the direct converter could show it.

- **The standalone `VideoDecoder` can declare a packet timebase.**

  This release's first cut wrote `AVCodecContext.pkt_timebase` only
  from the crate-private constructors the stream decoder uses; every
  public `VideoDecoder::open*` routed `None`. A direct caller feeding
  timestamped packets therefore had no way to tell libavcodec what
  their ticks meant — and not owning the `AVStream` does not mean not
  knowing its ruler. `open_timed`, `open_with_frame_limits_timed`,
  `open_with_timed` and `open_with_limits_timed` declare it, and it
  reaches every backend state including later hardware probe advances.
  The four existing constructors remain as **explicitly untimed**
  conveniences and say so in their docs.

## [0.14.0] - 2026-09-02

### Added — the seat 0.13 built gets its first occupant

- **Scaled output on the VideoToolbox road: a `VTPixelTransferSession`
  resizes the picture on the GPU, before it crosses to the CPU**
  ([#55](https://github.com/findit-studio/mediadecode/issues/55)).

  `scaled_output_capability()` answers
  `ScaledOutputCapability::Supported` on a live VideoToolbox session on
  an Apple target, and `request_scaled_output(size)` is honored there:
  pictures come back at the requested extent from the next frame on.
  0.13 shipped both methods answering `Unsupported` everywhere with a
  census explaining why; this fills the seat on the one backend this
  crate can compile, run and verify on the host that builds it.

  **FFmpeg's hwaccel decode is untouched.** The decode still negotiates
  through `av_hwdevice_ctx_create` + the strict `get_format` callback
  and still produces a full-size `CVPixelBuffer` — inter prediction
  needs full-resolution reference frames, so every road decodes full
  size internally and no design changes that. What changed is the
  **crossing**: between the decoded hardware frame and
  `av_hwframe_transfer_data`, a `VTPixelTransferSession` resizes the
  `CVPixelBuffer` into a fitted one, and *that* is what moves to the
  CPU. A 4K stream fitted to a 512-class box moves roughly thirty times
  fewer bytes over the bus, and allocates a CPU frame of the same
  reduced size.

  A caller-owned `VTDecompressionSession` with its own
  `destinationImageBufferAttributes` — decode and scale in one session
  — saves the same bandwidth and no more, and costs a second, parallel
  VideoToolbox integration path plus an async output-callback
  trampoline. It stays #55's standing future enhancement; the reopen
  trigger is a measurement showing a gap this stage cannot close.

  **The destination buffers are FFmpeg's, not this crate's.** A second
  `AVHWFramesContext` is built over the decode session's own
  VideoToolbox device at the fitted size, and every destination comes
  out of it through `av_hwframe_get_buffer` — so each fitted
  `CVPixelBuffer` arrives owned by an `AVFrame`, recycled through
  libavutil's pool, and released by `av_frame_unref` exactly as the
  decoder's own frames are. The only Core Foundation object this crate
  owns on the road is the transfer session itself: one per stream and
  requested size, cached, and retired when either changes.

  **Nothing about it can fail a decode.** Every condition the stage
  cannot honor makes it *stand down* for that frame — the full-size
  picture goes to the CPU download exactly as before. That covers a
  source that is not a VideoToolbox surface, a pixel buffer whose
  extent is not the picture's, a standing request a mid-stream
  resolution change turned into an upscale, a frame carrying a crop
  rectangle a resize would invalidate, and a session, pool or transfer
  that fails to build or run. It covers the case *after* the stage's own
  work too: a fitted surface that is produced perfectly and then will
  not cross to the CPU is the stage's failure, not the backend's, so the
  decoder latches that size off, resets the destination and downloads
  the original full-size frame — the path it took before any of this
  existed — rather than reporting a hardware failure and degrading a
  session that was working. No new error variant, no new failure mode
  on the decode road.

  **And the capability word stops promising the moment the stage stops
  delivering.** The trait's contract is that a `Supported` answer lets a
  caller skip its own resampler, so a session that quietly reverted to
  full-size pictures under that answer would hand it mixed extents with
  nothing to notice them by. The first frame that goes out at anything
  other than a standing request's extent therefore flips
  `scaled_output_capability` to `Unsupported` — the explicit transition
  a caller is told to look for, one query away — **and stops the stage
  with it**. That second half is what makes the word usable: a caller
  told `Unsupported` resumes resampling for itself, so a session that
  quietly fitted a later frame would have it resample an already-fitted
  picture. The answer and the behaviour move together, and only an
  accepted `request_scaled_output` restarts either — which also retires
  what was built, since a stage that will not run has no business
  holding a pooled surface and a session. A request the stream
  already satisfies is not a broken promise: the picture arrives at
  exactly the size asked for — and that is settled from the frame's own
  extent *before* any of the stage's other refusals, so a frame that
  merely cannot be scaled but is already the requested size does not
  break anything either.

  Three more places the promise and the resources are kept honest. A
  request placed while a decoded frame is **parked** in the scratch (a
  conversion that did not commit) is refused: that frame is delivered
  from the scratch without consulting the stage, so accepting a new size
  would promise an extent the very next picture cannot have — drain it
  and ask again, the same escape every seat guarded by that flag offers.
  And the refusal carries the same meaning there as everywhere, through
  the same clearing operation: the session returns to full coded size,
  so any request already standing is withdrawn and what was built for it
  retired. The parked picture keeps the extent it was decoded at. An accepted request **always retires**
  what is cached, including at an unchanged size, because a latched
  build or transfer failure would otherwise survive an explicit
  re-request and turn a transient failure permanent behind a renewed
  promise. And the moment a frame's stream shape is known, anything
  cached for a *different* one is retired — before the upscale and
  aspect-ratio stand-downs, each of which returns early and would
  otherwise leave a 4K surface, its frames context and its session
  retained for a whole lower-resolution run.

  **What it refuses, and what it keeps honest.** A zero extent and an
  upscale are refused — downscale-or-equal only, per the pixel-budget
  law this serves. A refusal is not an error, and it **returns the
  session to full coded size**, dropping any request already standing:
  that is what the trait says an `Unsupported` answer from this seat
  means, and the only reading a caller can act on, since one told
  `Unsupported` resamples for itself and a session still quietly fitting
  to an older request would have it resample an already-fitted picture.
  Like an acceptance, a refusal takes effect from the next picture; one
  already decoded keeps the extent it was decoded at. The destination `CVPixelBuffer`'s format
  is derived from the source's, so the transfer is a resize and not a
  colour conversion, and the pixel format the CPU download reports is
  the one the unscaled road reports. The sample aspect ratio is
  corrected for the scale, so the display geometry a stream declared
  survives a non-uniform fit. `served()` / acceleration observability
  is unchanged, and a session that degrades to software answers
  `Unsupported` again from that moment.

  **The determinism trade is unchanged and still applies**: a backend's
  scaler is not pixon's, so enabling scaled output trades cross-backend
  byte-determinism for bandwidth. Pair it with the `DecodePath` pin
  ([#50](https://github.com/findit-studio/mediadecode/issues/50)) when
  a caller needs the same picture bytes run to run — the paired shape
  is exercised end to end in `tests/scaled_output.rs`.

  The other three backends still answer `Unsupported`, each with its
  own filed native scaling seam: NVDEC/CUVID
  ([#56](https://github.com/findit-studio/mediadecode/issues/56)),
  VAAPI VPP
  ([#57](https://github.com/findit-studio/mediadecode/issues/57)), the
  D3D11 Video Processor
  ([#58](https://github.com/findit-studio/mediadecode/issues/58)). So
  does every software session, for the separately documented `lowres`
  reasons.

  **macOS and visionOS only, and the boundary is an availability
  window rather than a preference.** `VTPixelTransferSessionCreate` and
  its two companions are `API_AVAILABLE(macos(10.8), ios(16.0),
  tvos(16.0), visionos(1.0))` and `API_UNAVAILABLE(watchos)`. Rust has
  no availability attribute and no weak-import spelling for a plain
  `extern "C"` declaration, so an iOS or tvOS binary with a deployment
  target below 16.0 would carry a *strong* import of a symbol its
  runtime does not export and fail in the dynamic loader at launch —
  before any of the stage's stand-down logic could answer
  `Unsupported`. macOS 10.8 and visionOS 1.0 sit below every deployment
  target Rust supports there, so on those two the symbols are always
  present. iOS, tvOS and watchOS answer `Unsupported`; reaching iOS/tvOS
  16 and above would need a `dlopen`/`dlsym` resolution path, which is
  not this change.

  **And nothing about the FFmpeg build, deliberately.** The stage names
  no FFmpeg symbol whose presence depends on how FFmpeg was configured:
  the `AVHWFramesContext` it builds and the frames it draws are generic
  libavutil, and the VideoToolbox calls are Apple's own. In particular
  it does **not** steer the destination pool's pixel format through
  `AVVTFramesContext.color_range`, which FFmpeg compiles only under
  `CONFIG_VIDEOTOOLBOX` — depending on it would put a build script in
  the business of predicting what `bindgen` will generate from headers
  it has not seen, across include overlays, cross sysroots and
  `BINDGEN_EXTRA_CLANG_ARGS`, and being wrong there is a crate that
  will not compile. Instead the stage **verifies** the pool's `OSType`
  on the very buffer it is about to write, every frame: a mismatch means
  the transfer would convert rather than resize, and it stands down.
  That is a stronger guarantee than steering was, and it needs nothing
  from the build.

  So an FFmpeg compiled without VideoToolbox is handled where it
  belongs, at run time — no VideoToolbox device opens, the session takes
  another backend or software, and the stage never sees a frame. The
  narrowing this buys is one case: a full-range source (`420f`-class)
  meets a video-range pool and is delivered full size instead of fitted.
  VideoToolbox's H.264 and HEVC decode paths produce video-range
  buffers, which is why the road this serves is unaffected in practice.

  The two targets that pass the gate link `VideoToolbox`, `CoreVideo`
  and `CoreFoundation` (`build.rs`), and
  `MEDIADECODE_FFMPEG_NO_VIDEOTOOLBOX` compiles the stage out by
  request. No new Cargo feature and no new dependency: the runtime
  opt-in is `request_scaled_output` itself.

  **A session never crosses a thread.** `VideoToolbox.framework` marks
  `VTPixelTransferSessionRef` `CM_SWIFT_NONSENDABLE` and publishes no
  cross-thread mobility guarantee, and `VideoDecoder` is `Send` — so
  rather than assume one, the stage records the `std::thread::ThreadId`
  each session was created on and retires and rebuilds when the decoder
  has moved. A `ThreadId` rather than a `pthread_t` on purpose: Darwin
  recycles POSIX handles, so a thread created after the owner exited can
  be handed the same one and `pthread_equal` would call the two
  identical — the exact off-owner reuse the guard exists to stop.
  `ThreadId` is documented never to be reused for the process's
  lifetime. The one call the stage cannot avoid making off-thread, the
  release in `Drop`, is narrowed to the half the framework documents as
  sufficient by itself: "when a pixel transfer session's retain count
  reaches zero, it is automatically invalidated", so the optional
  `VTPixelTransferSessionInvalidate` is made only on the owning thread
  and the atomic `CFRelease` is what actually tears the session down.

  **Two more stand-downs the metadata demands**, alongside the crop
  rectangle. A frame carrying side data whose payload is expressed in
  the picture's own grid — a pan-scan window in 1/16-pel units, a
  spherical mapping, regions of interest in source pixel coordinates —
  is delivered full size rather than fitted: a resize strands that
  payload on a grid that no longer exists, and quietly dropping it is
  the failure mode `copy_frame_props_minimal`'s own OOM arm already
  rejects. The kinds are named rather than asked of FFmpeg 8's
  `av_frame_side_data_desc`, which half the FFmpeg releases this crate
  builds against do not have; the list is FFmpeg 9's own
  size-dependent table intersected with the crate's copy whitelist, so
  the four size-dependent kinds it never copies need no entry. Side-data
  bookkeeping the walk cannot trust — a negative count, a positive count
  with no array, a null entry, a count past the copy path's own
  64-entry cap — stands down for the same reason. And a scale whose
  corrected sample aspect ratio will not fit in `AVRational`'s `c_int`
  pair stands down too: after the grid moves, republishing the source's
  ratio is a wrong answer rather than a conservative one.

  `stage()`'s final projection of `Cache::Built` into the returned
  frame is a bool pre-check followed by a fresh match, not one borrow
  shared with the `&mut self` stand-down path — the shape the crate's
  MSRV floor (1.95) needs from the borrow checker (NLL "Problem Case
  #3", rust-lang/rfcs#2094). No change to what `stage()` answers.

### Changed

- **`CarrierVideoStreamDecoder::scaled_output_capability` and
  `::request_scaled_output` are no longer `const fn`.** Both now read
  the session's live state — which road it is on, and what the stream's
  coded extent is — rather than answering a compile-time constant, and
  that is what lets the capability follow a session which degrades to
  software instead of describing the machine it runs on. The
  `mediadecode::VideoStreamDecoder` trait methods they implement were
  never `const`, and neither is callable in a const context (both need
  a live decoder), so no working caller can observe the change.

### Changed (BREAKING)

- **`mediaframe` 0.9 → 0.10**, tracking the core's own crossing (see
  [`mediadecode`](../mediadecode/CHANGELOG.md#0140)). `mediaframe` is a
  public dependency of the core *and* a direct one here (pinned with
  `alloc`), and its `audio` and `frame` types appear in this adapter's
  own signatures, so this adapter's surface moves with it.

  **No adapter source line moved and no behaviour changes.** Upstream
  0.10.0 is two changes. The case-sensitivity axis is `pub(crate)` — no
  public API moved, and all 22 households (`audio::ChannelLayout` and
  its siblings included) declare `Insensitive`, zero behaviour change
  regardless. The `other()` fix — every escape constructor now runs the
  ignore-case `FromStr` lookup first, and a genuine stranger's spelling
  is preserved verbatim rather than ASCII-folded — has exactly one
  reachable site in this crate:
  [`channel_layout_from_describe`](src/channel_layout.rs), which turns
  FFmpeg's own rendering of an unrecognised layout into `ChannelLayout`
  via `FromStr` and then **explicitly filters out any `Other(_)`
  result**, collapsing it to the absent sentinel before any caller
  sees it — `ChannelLayoutDescription::text` already carries the
  verbatim FFmpeg rendering, separately. Whatever bytes `FromStr` would
  now put in that discarded `Other` never reach a caller either way: no
  behaviour change at the one site that could have shown one. No other
  `.other(...)` call and no other `FromStr` site onto a `mediaframe`
  vocabulary type exists in this crate.

  The surface this adapter actually consumes is unchanged in 0.10:
  `audio::{ChannelLayout, ChannelLayoutDescription, ChannelOrder,
  ChannelSpec}` and `frame::Rotation`. Verified, not just argued:
  `cargo hack clippy / build / test -p mediadecode-ffmpeg
  --feature-powerset --exclude-no-default-features` and `cargo fmt
  --check`, stable toolchain, all clean with zero source change.

## [0.13.0] - 2026-09-01

### Added — three producers a consumer had no door onto

- **`DecodePath` and `open_as`: a video session's decode path can be
  pinned** ([#50](https://github.com/findit-studio/mediadecode/issues/50)).

  ```rust
  pub enum DecodePath { Auto, Hardware(Backend), Software }

  impl CarrierVideoStreamDecoder<Lane> {
    pub fn open_as(
      parameters: Parameters,
      time_base: Timebase,
      limits: DecoderLimits,
      path: DecodePath,
    ) -> Result<Self, Error>;
  }
  ```

  On both lane faces. `open` is unchanged and is now
  `open_as(.., DecodePath::Auto)` — the same probe, the same fallback,
  the same behaviour down to the arm it takes.

  Before this there was **no door at any name** onto the software
  decoder as a session: `open_sw_decoder` was private and
  `VideoDecoder::from_software_for_test` was `#[cfg(test)]`. The only
  way to reach software was to pick a codec no hardware backend
  carries — which changes the stream, and therefore compares nothing.
  `VideoDecoder::open_with` reached a named backend but not as a
  session: it does not implement `mediadecode::decoder::VideoStreamDecoder`,
  and wrapping it in a consumer would mean a second copy of the
  probe/fallback state machine this crate already owns.

  **The two named arms are pins, not preferences**, and that is the
  half of this that is behaviour rather than surface. `Auto` may end on
  software, at open or mid-stream. `Hardware(backend)` may not: a
  backend that will not open fails the `open_as` call where `Auto`
  would have gone on to software, and a backend that opens and then
  cannot decode surfaces `Error::AllBackendsFailed` from the road that
  met it — on all three of the send, receive and EOF roads — instead of
  degrading behind the caller's back. A pin a mid-stream failure could
  quietly undo would not be one, and both consumers this door exists
  for need the promise rather than the preference: a determinism
  comparison that silently got software twice would report that the
  two paths agree.

  Observability is untouched. `is_hardware()` / `is_software()` remain
  live readings.

- **`ContainerFormat` and `CarrierDemuxer::format`: libavformat's own
  identification of the container reaches a door**
  ([#42](https://github.com/findit-studio/mediadecode/issues/42)).

  ```rust
  impl CarrierDemuxer<Lane> {
    pub const fn format(&self) -> Option<&ContainerFormat>;
  }

  impl ContainerFormat {
    pub fn name(&self) -> &str;                        // "mov,mp4,m4a,3gp,3g2,mj2"
    pub fn long_name(&self) -> Option<&str>;           // "QuickTime / MOV"
    pub fn names(&self) -> impl Iterator<Item = &str>; // the six words
  }
  ```

  Read once from `AVFormatContext.iformat` while the session opens and
  kept for its life — libavformat picks the demuxer during
  `avformat_open_input` and never changes it.

  **It is a reading of the bytes, not of a path**, which is what makes
  it usable on a content-addressed row where an extension guess is
  incoherent: the same content under `.mov` and `.mp4` answers
  identically, and a Matroska file copied to an `.mp4` path still
  answers `matroska,webm`.

  The name is a **list** because FFmpeg registers one demuxer per
  family, and the door hands over the whole list rather than picking
  one of the words. Which brand inside a family a file is — an `.mp4`
  against an `.m4a` — is a question libavformat did not answer, and a
  door that returned a single word would have had to guess it. An open
  vocabulary, never an enum: a container FFmpeg learns to demux in its
  next release names itself here with nothing to change on this side.

- **`CodecId::name` and `CodecId::long_name`: a track's codec has a
  word, not only a number**
  ([#43](https://github.com/findit-studio/mediadecode/issues/43)).

  ```rust
  impl CodecId {
    pub fn name(self) -> Option<SmolStr>;      // "h264", "aac", "subrip"
    pub fn long_name(self) -> Option<SmolStr>; // FFmpeg's description
  }
  ```

  Read off libavcodec's own `codec_descriptors[]`, so the vocabulary is
  open the way `ContainerFormat`'s is: a codec FFmpeg learns names
  itself here with nothing to change. `CodecId::raw` stays the
  identity — the name is a rendering, and it is what lets a stored
  track row cross into a typed codec vocabulary instead of holding a
  bare integer.

  **`None` is a real answer.** The descriptor table is read rather than
  `avcodec_get_name`, which never fails: for an id it cannot place that
  function returns the string `"unknown_codec"` — a sentinel a consumer
  would store as though it were a codec's word. `AV_CODEC_ID_NONE` has
  no descriptor either, so the "no codec" row reads as absent rather
  than as a track coded with something called `none`.

- **A track row carries its container's language declaration**
  ([#44](https://github.com/findit-studio/mediadecode/issues/44)).

  The adapter fills the core's new `TrackInfo::language` seat from the
  `AVStream` metadata `language` entry, where libavformat lands every
  container's tag: Matroska's `Language` element, an MP4 `mdhd`
  language code, an ASF descriptor, an ID3 `TLAN` frame. The value is
  what the file wrote — see the core's changelog for why nothing here
  folds it.

### Added — the HDR/colour faces, and a scaled-output capability word

- **HDR10 static metadata now actually crosses.** `VideoFrameExtra`
  has carried `mastering_display` / `content_light_level` seats
  (`Option<MasteringDisplay>` / `Option<ContentLightLevel>`) since
  before this release, but nothing populated them from a real decoded
  frame — every session answered `None` regardless of what the stream
  carried. Wired at the one place a `VideoFrameExtra` is built: an
  `AV_FRAME_DATA_MASTERING_DISPLAY_METADATA` / `AV_FRAME_DATA_CONTENT_
  LIGHT_LEVEL` side-data entry on the decoded `AVFrame`, when present,
  is now parsed onto the matching seat; absent, the seat reads `None`,
  same as before. Cross-checked against a real `libx265` HDR10 encode
  on this host — `ffprobe -show_frames` and this crate's own decode
  agree on every number.

  Color transfer characteristic and primaries (`ColorTransfer` /
  `ColorPrimaries`, both open-vocabulary with an `Other` escape) and
  the rest of `ColorInfo` were already fully wired end to end before
  this release, via the shared `mediaframe::color` vocabulary — this
  release adds no new words there, only proves PQ and HLG each decode
  correctly (see the `hdr_metadata` integration test) alongside the
  now-wired static HDR seats.

- **`CodecTicket::dolby_vision_config`: the Dolby Vision configuration
  record's two routing numbers.**

  ```rust
  pub struct DolbyVisionConfig { .. }
  impl DolbyVisionConfig {
    pub const fn profile(&self) -> u8;
    pub const fn compatibility_id(&self) -> u8;
  }
  impl CodecTicket {
    pub fn dolby_vision_config(&self) -> Option<DolbyVisionConfig>;
  }
  ```

  Reads the `dv_profile` / `dv_bl_signal_compatibility_id` seats of the
  container's `dvcC` / `dvvC` / `dwvC` box, when
  `AV_PKT_DATA_DOVI_CONF` side data is present on the stream's codec
  parameters — `None` otherwise. Numbers only, no interpretation: this
  crate does not map a profile number onto a named Dolby Vision profile
  or a compatibility id onto "HDR10-compatible" / "SDR-compatible" —
  that table belongs to the consumer that already routes
  base-layer-vs-refuse on it.

  The per-frame half of Dolby Vision — the RPU buffer
  (`AV_FRAME_DATA_DOVI_RPU_BUFFER`) and parsed dynamic metadata
  (`AV_FRAME_DATA_DOVI_METADATA`), plus HDR10+ dynamic metadata
  (`AV_FRAME_DATA_DYNAMIC_HDR_PLUS`) — is not exposed by this release;
  filed as [#54](https://github.com/findit-studio/mediadecode/issues/54).

- **`scaled_output_capability` / `request_scaled_output` implemented**
  on `CarrierVideoStreamDecoder` (both lanes), answering the core's new
  `VideoStreamDecoder` capability word. **Both answer `Unsupported` on
  every path this crate opens today** — hardware included. That is a
  documented census finding, not an oversight: every hardware backend
  here negotiates through FFmpeg's generic hwaccel machinery, which
  hands the destination size to libavcodec unconditionally, and
  `av_hwframe_transfer_data`'s own contract requires the source and
  destination frames to share their allocated dimensions — so even a
  downscale at the CPU-transfer step is out of reach. Software decoders
  have no general decode-time scaling seam either; the legacy `lowres`
  option covers only the old MPEG family (and is broken even there for
  H.264), skipping reconstruction detail rather than decoding in full
  and resizing. See `CarrierVideoStreamDecoder::scaled_output_
  capability`'s doc comment for the full census and the per-backend
  enhancement issues: VideoToolbox
  ([#55](https://github.com/findit-studio/mediadecode/issues/55)),
  NVDEC/CUVID
  ([#56](https://github.com/findit-studio/mediadecode/issues/56)),
  VAAPI
  ([#57](https://github.com/findit-studio/mediadecode/issues/57)),
  D3D11 Video Processor
  ([#58](https://github.com/findit-studio/mediadecode/issues/58)).

### Fixed

- **`tests/audio_pcm_fixtures.rs` skipped on a fetched corpus and
  panicked on an empty one**
  ([#34](https://github.com/findit-studio/mediadecode/issues/34)).

  The sweep guarded availability with `root.exists()`. A `git clone`
  without `--recursive` leaves the submodule's mount point as an
  *empty directory*, so the guard answered "present" and the lane
  panicked opening a file that was never fetched — on exactly the fresh
  worktree the skip exists to welcome. The guard now asks about a
  fixture file, taken from the first row of the sweep's own table so
  the file it asks about is by construction one the sweep opens.

### Fixed — the documented order demuxed a healthy file to nothing

- **`take_tracks()` before `next_packet()` yielded zero packets**
  ([#51](https://github.com/findit-studio/mediadecode/issues/51)).

  `take_tracks_impl` was a `mem::take` of `self.tracks` — the same
  `Vec` `next_packet_impl` classified every packet against. After the
  take, `self.tracks.get(index)` answered `None` for every packet in
  the file, each one took the "no row describes this stream" arm, the
  loop read to EOF and the session reported a clean end of file. No
  error, no assertion, no log. And the order that triggered it was the
  order the trait's own documentation prescribed.

  Measured on a one-second `testsrc2` H.264 clip: 25 packets when the
  table was read *after* the pull loop, 0 when it was read before.

  The fix is the core's, and it is structural rather than local: the
  destructive door is gone from the trait (see
  [`mediadecode`'s CHANGELOG](../mediadecode/CHANGELOG.md)), so this
  session can no longer be asked to give its table away. Nothing here
  guards against the state; the state cannot occur.

### Changed (BREAKING)

- **The track table is `Arc`-wrapped at open and kept for the life of
  the session.** `CarrierDemuxer` now holds
  `Vec<Arc<TrackInfo<Ffmpeg>>>`, binds
  `Demuxer::TrackHandle = Arc<TrackInfo<Ffmpeg>>`, and
  `tracks()` returns `&[Arc<TrackInfo<Ffmpeg>>]`. `take_tracks()` is
  gone with the trait method it implemented.

  `Arc` rather than `Rc` because these rows were *made* shareable
  across tasks: `ticket::CodecTicket` exists precisely so a track row
  holds no raw `AVCodecParameters` pointer and is therefore
  `Send + Sync` by construction. One allocation per track at open is
  now the whole cost of every fan-out afterwards, and it is paid at
  the door instead of by each consumer.

  Reading a row is source-compatible — `tracks()[i].kind()`,
  `.timebase()`, `.extra()` all still resolve, through the handle's
  `Deref`. Code that named the element type
  (`let row: &TrackInfo = &demuxer.tracks()[0]`) or that called
  `take_tracks()` does not compile; see the core log for the
  one-line migration.

### Changed

- **A packet whose stream the table does not describe is reported once
  per session rather than dropped in silence.** The arm still passes the
  packet by — it is reachable on healthy input, since a format flagged
  `AVFMTCTX_NOHEADER` (MPEG-TS, RTP) may add an `AVStream` in the middle
  of `av_read_frame` while this session's table was fixed at open — but
  a `debug_assert` would fire on a transport stream and an `Err` would
  end a session over a stream the caller never asked about. It gets a
  `tracing::debug!` instead, naming the stream index and the table's
  size, latched behind a session flag so the very case that makes the
  arm reachable — a live stream delivering on that index for as long as
  it runs — cannot turn the diagnostic into an unbounded log. Its
  previous comment, "libavformat does not produce these", was not true
  even before #51 made it the path every packet took.

## [0.12.0] - 2026-08-30

### Added

- **`ticket::CodecTicket` — the owned mirror of an `AVCodecParameters`.**
  Every one of the thirty-two seats FFmpeg n9.0 declares, held as plain
  integers and owned bytes: `extradata` as an `FfmpegBytes`,
  `coded_side_data` as a `Vec<SideDataEntry>`, and the channel layout as
  a `ChannelLayoutTicket` whose union arm follows its own `order` — a
  bitmask, or a `CustomChannel` map with each channel's raw id and its
  sixteen NUL-padded name bytes. The two length seats
  (`extradata_size`, `nb_coded_side_data`) are not stored: a carrier
  knows its own length, and a second copy of it is a second thing to
  keep in agreement.

  `CodecTicket::mirror` reads a live `AVCodecParameters` under the same
  ceiling `bounded_clone_parameters` enforces, measured before a byte is
  copied. `CodecTicket::rebuild` allocates a fresh one and writes the
  seats back, minting `extradata`'s `AV_INPUT_BUFFER_PADDING_SIZE`
  trailing zeroes as it goes; it is the one place the track-row road now
  allocates an `AVCodecParameters`, and what it hands back is what
  `avcodec_parameters_to_context` is fed, unchanged.

  Stated exactly, because the difference is load-bearing: the
  twenty-seven scalars and `ch_layout` are written unconditionally —
  those are the seats `avcodec_parameters_alloc` gives *non-zero*
  defaults to (`format` is `-1`, `profile`/`level` are the `UNKNOWN`
  sentinels, both rationals are `0/1`), so leaving any of them would let
  a default masquerade as the file's own value. The four descriptor
  seats — `extradata`/`extradata_size` and
  `coded_side_data`/`nb_coded_side_data` — are written only when there
  is something to put there, and on the empty path keep the allocator's
  zero. That is correct rather than an omission: `codec_parameters_reset`
  `memset`s the struct to zero and then assigns non-zero defaults to a
  named list containing none of those four, so a null pointer with a
  zero length is exactly what the source had.

  Not one bindgen enum is materialised out of FFmpeg memory in either
  direction: every open C enum seat — the media type, the codec id, the
  field order, the five colour seats, the alpha mode, the channel order,
  a custom channel's id, a side-data type id — crosses as the raw 32-bit
  pattern it is on the wire, so a value these bindings cannot name is
  still a value the ticket carries.

  `tests/codec_ticket_parity.rs` is the proof: a comparator naming all
  thirty-two fields, swept over 16 streams — every container the corpus
  can mint (8 of them carrying real `extradata`: H.264 SPS/PPS, AAC
  `AudioSpecificConfig`, a font's payload) plus one MOV assembled byte
  by byte in the test itself, whose `colr`/`prof` atom libavformat turns
  into an `AV_PKT_DATA_ICC_PROFILE` entry of `coded_side_data`. That
  last seat has no `ffmpeg` CLI recipe — censused: `-metadata:s:v
  rotate=` no longer writes a display matrix and MOV, MP4 and Matroska
  emit no stream-level side data of their own — so minting it is what
  lets the suite prove a *demuxer* populates it rather than only that an
  absent seat crosses. Nothing binary is committed, and that fixture
  needs no CLI, so the lane runs everywhere.

  Then the shapes no container will hand over at all, and a decoder
  opened *through* a rebuilt ticket and made to produce both a video and
  an audio frame.

- `ResampleSpec::from_ticket` — the source spec off a track row's owned
  ticket, replacing `ResampleSpec::from_parameters(track.extra()
  .parameters())`. Both roads now decide through one shared layout
  roster, so they cannot drift into admitting different layouts from the
  same file. `from_parameters` remains for a caller holding a live
  `stream.parameters()`.

- `DemuxError::ParametersChannelMap`. A channel layout that declares
  `AV_CHANNEL_ORDER_CUSTOM` without the map that order requires is
  refused rather than mirrored. This one is a crash, not a curiosity:
  `av_channel_layout_copy` — how `avcodec_parameters_to_context` moves
  the layout into a decoder's context — allocates `nb_channels` entries
  and then `memcpy`s from `src->u.map` with **no null check of its own**
  (verified against FFmpeg n9.0), so a layout naming channels it has no
  map for makes libavcodec read from null the moment a decoder opens.
  Reproducing that shape faithfully would pass a parity comparator and
  forward the crash, so the mirror fails closed instead; and the rebuild
  writes `nb_channels` **from** the map rather than from the stored
  field, so the struct handed to libavcodec can never declare more
  channels than the array it points at.

- `DemuxError::ParametersOpaque`. `AVChannelLayout::opaque` and
  `AVChannelCustom::opaque` are raw pointers FFmpeg documents as "private
  data of the user"; an owned mirror outlives the pointer's owner and may
  cross threads, so it refuses one rather than dropping it in silence.
  libavformat sets neither, so no demuxed stream reaches it — this is a
  tripwire, and the same fail-closed answer `measure_parameters` gives a
  channel order it has never heard of.

### Changed

- **BREAKING: `TrackExtra` no longer carries an
  `ffmpeg_next::codec::Parameters`.** It carries a `CodecTicket`, and
  with the raw handle gone the whole track table became `Sync`.

  That was the defect. `Parameters` is a `*mut AVCodecParameters` behind
  a `Send`-but-not-`Sync` wrapper, and it was the row's only non-`Sync`
  field: `TrackInfo<Ffmpeg>` was therefore `!Sync`,
  `Arc<TrackInfo<Ffmpeg>>` was not `Send`, and a consumer sharing a
  track row across tasks did not compile — for a struct FFmpeg documents
  as a plain descriptor with no thread affinity at all. The auto-trait
  was missing, not the safety. It is answered with the mirror this crate
  already lives by rather than an `unsafe impl` over FFI, so `Send +
  Sync` are now structural facts with no safety argument to get wrong.
  `src/ticket/tests.rs` pins them on `CodecTicket`, `TrackExtra`,
  `TrackInfo<Ffmpeg>` and `Arc<TrackInfo<Ffmpeg>>`.

  The surface that moved:

  | before | after |
  |---|---|
  | `TrackExtra::parameters() -> &Parameters` | `TrackExtra::ticket() -> &CodecTicket` |
  | `TrackExtra::new(i32, Parameters) -> Result<Self, DemuxError>` | `TrackExtra::new(i32, CodecTicket) -> Self` |
  | `TrackExtra::try_clone() -> Result<Self, DemuxError>` | `impl Clone for TrackExtra` |

  `TrackExtra::clone_parameters() -> Result<Parameters, DemuxError>` is
  **unchanged** — it rebuilds from the ticket, so every decoder-open call
  site (`FfmpegAudioStreamDecoder::open(track.extra()
  .clone_parameters()?, …)`) is untouched.

  `TrackExtra::new` lost a fallibility it no longer had a way to use. It
  returned a `Result` for exactly one reason: `Parameters`' safe
  constructors hand back a null-backed value when
  `avcodec_parameters_alloc` fails and report nothing, so a caller could
  hand the row one having never been told. That check moved to
  `CodecTicket::mirror`, beside the raw pointer where it belongs.

- **`TrackExtra` implements `Clone` again, and `try_clone` is gone.**
  This reverses a standing ruling, so it is recorded rather than
  slipped in.

  The type refused `Clone` for two releases, on two arguments, and both
  were about the raw handle. The first was safety: a derived `Clone`
  went through `ffmpeg_next`'s `Clone` for `Parameters`, which checks
  neither the allocation nor the copy, so safe public code that merely
  copied a track row could dereference a null destination or receive
  quietly incomplete parameters — and `Clone` has no way to report
  either. That derive shipped once and was genuinely reachable. The
  second was the message-carrier law: a `Clone` is a refcount bump,
  never a deep copy, and `avcodec_parameters_copy` is not that.

  Neither survives the mirror. There is no `Parameters` left to clone
  unchecked, and a copy is now plain owned Rust — a `Vec` spine and
  refcount bumps over `Arc<[u8]>` — with no FFmpeg allocator anywhere
  near it, which is precisely what the carrier law asks of a `Clone`.
  A ban whose whole rationale is spent is ceremony, so the ban is gone
  along with both stand-ins it had accumulated (the fallible
  `try_clone` and, briefly, an infallible `duplicate`). No deprecation
  ceremony: this release is breaking already.
  `demuxer::tests::the_public_track_extra_handoffs_still_answer_the_allocator`
  pins the property the derive rests on, by cloning a row under an
  FFmpeg allocator capped to refuse everything.

  This does **not** make a track row cheap to copy by accident:
  `TrackInfo` and `TrackParams` still have no `Clone` of their own, so
  the row-sharing law is untouched — a consumer that needs to share a
  row still wraps it in `Arc` once, at the door, which is now something
  it can actually do.

- `TrackExtra::parameter_bytes` now reports what a *rebuild* allocates
  rather than what the row holds. The number is the same one the session
  admitted the stream at and the same one
  `DemuxLimits::max_codec_parameter_bytes` was judged against, so no
  budget changed meaning; but the owned mirror holds its payload without
  FFmpeg's trailing padding and shares its buffers by refcount, so it is
  no longer a statement about the row's own residency.

- The demux path builds the ticket straight from the stream's
  parameters. Opening a file used to perform one full
  `avcodec_parameters_copy` per track whose only purpose was to sever the
  tie to the format context; the mirror severs it by being owned Rust, so
  that copy is gone rather than moved. The attachment road's
  `ExtradataPolicy::Omit` is honoured by the mirror exactly as it was by
  the clone — a font's extradata *is* the payload the carrier holds, and
  it is neither allocated nor charged.

### Changed (BREAKING)

- **`mediaframe` 0.7 → 0.9**, tracking the core's own crossing (see
  [`mediadecode`](../mediadecode/CHANGELOG.md#unreleased)). Two breaking
  minors at once. `mediaframe` is a public dependency of the core *and*
  a direct one here (pinned with `alloc`), and its `PixelFormat`,
  `color` and `audio` types appear in this adapter's own signatures
  (`convert`, `pixdesc`, `channel_layout`, `boundary`), so this
  adapter's surface moves with it.

  **No adapter source line moved and no behaviour changes.** The census
  behind that: upstream 0.8.0 is additive to the container /
  audio-container / image roster households, which this adapter never
  names — it takes its formats from FFmpeg's own descriptors, not from
  a filename — and 0.9.0 is the `lang` family, which retires the lossy
  `lang::Language` triple, its `LanguageError`, and `audio::Tags`'s
  language seat. None of the three appears anywhere in this crate. The
  ISO 639-2/T tag `extras`'s subtitle-track seat carries is this
  workspace's own raw `Option<[u8; 3]>`, untouched by either release.

  The surface this adapter actually consumes is unchanged in 0.9:
  `audio::{ChannelLayout, ChannelLayoutDescription, ChannelOrder,
  ChannelSpec}`, `frame::Rotation`, `pixel_format::PixelFormat` and the
  `color` household. Verified, not just argued: `cargo check`,
  `cargo test` (including the submodule-backed audio fixture lane) and
  `cargo clippy -- -D warnings`, all `--workspace --all-features`, are
  clean with zero source change.

## [0.11.0] - 2026-08-28

Tracks `mediadecode` 0.11.0, which crosses `mediatime` 0.3 → 0.4 and
`mediaframe` 0.6 → 0.7 — two breaking minors. Both are public
dependencies of the core crate, and `mediaframe`'s `PixelFormat` and
`color` types appear in this adapter's own signatures (`convert`,
`pixdesc`, `frame`), so this adapter's re-exported surface moves with
them. See [`mediadecode` 0.11.0](../mediadecode/CHANGELOG.md#0110).

Nothing about the FFmpeg boundary's behaviour changes and no source
line moved: `mediatime` 0.4 is additive only and this workspace calls
none of the new surface; `mediaframe` 0.7 is itself entirely the
`mediatime` crossing, so no pixel-format or colour variant moved
either. Verified, not just argued: `cargo check` / `cargo clippy -- -D
warnings` (default features, `--all-features`, `--no-default-features`)
and the full `cargo test` suite — including the submodule-backed
fixture tests — all pass with zero source change.

## [0.10.0] - 2026-08-27

### Fixed

- **A terminal `Received::Ended` was reversible without `flush` on the
  subtitle seam.** The new `eof` latch gated the receive side only, and
  `avcodec_decode_subtitle2` — unlike every send/receive decoder in the
  family — has no state machine of its own to refuse for it: it decodes
  whatever it is handed, every time. So a valid packet sent after
  `send_eof` was accepted, filled the seat, and made the next
  `receive_frame` answer `Frame` on a session the caller had already
  seen end. `send_packet` now refuses with the new
  `SubtitleDecodeError::AfterEof`, and the check sits **before** the
  held-cue check: the other order would answer `Sent::MustDrain`, and
  the drained retry would then be accepted — the same reversal, one
  call later. Only `flush` reopens the session.

  Named `AfterEof` after the seam one road over
  (`ResampleError::AfterEof`) rather than minting a third spelling; the
  arm is additive, since these enums are now `#[non_exhaustive]`.

  **And the video wrapper carried the same ordering, at both of its
  gates.** `send_packet` and `send_eof` checked the parked-seat flag
  before `eof_sent`, so a session that had accepted end-of-stream *and*
  had a frame parked — reachable when a delayed tail frame's carrier
  allocation fails parkably after EOF — answered `Sent::MustDrain` to a
  submission nothing could ever accept. That breaks the arm's contract
  rather than merely misnaming a fault: `MustDrain` promises that
  draining makes the same offer acceptable, and past end-of-stream the
  drained retry faults anyway, until `flush`. Both gates check
  `eof_sent` first now.

  The fault they answer is **censused, not invented**: with the seat
  free, a post-EOF submission reaches libavcodec and comes back as
  `Decode(Ffmpeg(Eof))` on all four roads (hardware and software, packet
  and EOF), so the gates short out to exactly that. Unlike the subtitle
  seam — which had to mint a word because `avcodec_decode_subtitle2` has
  no state machine to refuse for it — this face already had an answer,
  and a second spelling for one fault on one surface would be the
  disease this release is curing. A regression pins the synthesized
  value against the substrate's so the two roads cannot drift apart.

  **And the gates then interlocked with a fallback that predates them.**
  When hardware accepts end-of-stream and a post-commit exhaustion
  arrives *while draining*, the frame-time fallback opens software cold
  — and it forwarded the committed EOF only through the `Eof` failure
  arm, so this road did not. The cold decoder then answered `EAGAIN`
  forever, which reaches a caller as `Received::NeedsInput`: an
  instruction to send another packet, on a session where both send
  gates now refuse. Every exit was closed; before the gates, a repeated
  `send_eof` would have re-armed the decoder by accident.
  `degrade_to_sw` carries `eof_pending` now, exactly as the probe-era
  `fall_back_to_sw` beside it always has — one question, one mechanism
  on both fallback roads — and the EOF forward is one place rather than
  one arm.

  **The invariant behind it is now structural, not inherited.** A
  protocol state with no satisfying operation must not reach a caller,
  so `receive_frame` reads every drain answer against the session's own
  committed end: past `eof_sent`, a `NeedsInput` becomes the end. That
  does not rest on libavcodec honouring "no `EAGAIN` after a flush
  packet" — a contract this crate already documents a codec breaking,
  on the image road, and folds together there for the same reason. The
  end-of-stream escalation moved with it, so a post-commit gap that
  never closed is still reported as a lost tail when the end arrives
  spelled `EAGAIN`.

  **And the last of the class: a candidate on trial, past the end,
  producing nothing.** The probe replays its buffered history *and* the
  recorded end into each candidate, so a candidate can be sitting on a
  stream that is already over. Asked for a frame it answers `EAGAIN`,
  and both readings of that were wrong: "send me more" asks the caller
  for input the send gates refuse to accept, and "the stream ended"
  credits a backend that never decoded a frame — stopping the probe from
  trying the next one, which is the whole reason the probe exists.
  **Observable change: a post-EOF `EAGAIN` from a candidate now advances
  the probe instead of ending the stream**, so a hardware backend that
  cannot decode a clip falls through to the next one (and ultimately to
  software) rather than presenting an empty decode as a clean end.

  The fix is the shape rather than the case. `SessionPhase` is derived
  once per session type from the latches that were already there —
  whether an end has been recorded, whether a backend is still on trial
  — and both classifiers now take it, so a classification that does not
  say where the session is *does not compile*. `VideoDecoder`'s end
  latch moved out of `ProbeState` to make that derivation honest: it
  used to vanish when the probe collapsed, leaving a committed decoder
  unable to say whether the caller had signalled the end. The audio seam
  gained the same latch for the same reason — it had none, so its phase
  could only have been a guess.

  **Observable with it: a refusal this crate latched now surfaces where
  it used to be swallowed.** A `get_format` declination cannot be
  returned from the callback — it is left in the callback state for a
  funnel to collect, while libavcodec reports whatever it saw. Reading
  that report first answers `Ended` or `NeedsInput` for a frame the
  ceiling declined, and the reason dies unread. The committed hardware
  receive road did exactly that, and so did both raw hardware send arms.
  Callers that set `FrameLimits` and saw a clean end where a surface had
  been refused now get `HwSurfaceTooLarge`.

  **And the attempt log keeps that cause too.** A funnel *consumes* the
  refusal it collects, so a road that funnels twice reports the errno
  FFmpeg happened to give over the one this crate made:
  `post_commit_hw_failure` ran its own `hw_exit` after the receive road
  had already minted, and `AllBackendsFailed` recorded `InvalidData` for
  a coded surface declined over a configured ceiling. The software
  fallback still fired, so nothing broke — but a caller reading the
  attempts to find out *why* no backend worked was told the wrong thing,
  and told the one thing it could not act on. The verdict is minted once
  and threaded now; the raw errno keeps exactly one job, deciding whether
  a fallback is required.

  **And a declined coded surface now reaches the caller whatever the
  codec called it.** `get_hw_format` runs again on a post-commit format
  change — an HEVC SPS switch mid-stream — so the ceiling can decline a
  surface there. H.264 normalises that to `InvalidData`, which the
  fallback predicate matched; FFmpeg 9's HEVC path propagates the `-1`
  and `ffmpeg-next` maps it to `Other { errno: 1 }`, which it did not.
  The caller got an EPERM-shaped errno, no software fallback, and a
  latched refusal left standing to be collected by whatever ran next.
  The predicate widened to the truer condition rather than to one more
  spelling: **a latched `HwSurfaceTooLarge` is the fallback signal
  itself**, and enumerating errnos is a census of how each codec in each
  release happens to say one thing. A frame-budget refusal deliberately
  still does not count — software would decode the same oversized frame
  and be refused by the same ceiling. Observable: HEVC ceiling declines
  now fall back to software and surface `HwSurfaceTooLarge`, as the
  H.264 spelling always has.

  **And a budget refusal no longer mis-triggers that fallback.** The
  widened predicate was written as an `or`, and the `or` reached past its
  own exclusion: `judge_buffer` refuses a software allocation by latching
  `FrameBudgetExceeded` and answering libavcodec `-EINVAL` — the only
  thing a `get_buffer2` callback can answer — and `EINVAL` is on the
  hardware-decode-failure list. So the second arm fired and a frame the
  caller's own ceiling declined took the fallback road: a cold software
  decoder, a degraded resync, a bounded span of dropped frames, and the
  one actionable error buried inside `AllBackendsFailed`. A named verdict
  now outranks the raw errno in **both** directions — the errno is
  consulted only where nothing was named. Observable: a frame-budget
  refusal travels unwrapped again on the `EINVAL` spelling, naming the
  action that can succeed (raise the ceiling) rather than one that
  cannot.

  **And a latched surface refusal on the send road now goes somewhere.**
  The transient send arms returned their funnel's result the instant they
  had it, which is right for a flow signal and a dead end for anything
  else: `hw_send` can mint `HwSurfaceTooLarge`, and a minted refusal
  returned plain reaches nothing — the wrapper opens software only on
  `AllBackendsFailed`, so it stopped, and a probe still auditioning never
  advanced past the candidate that had just declined the surface. Every
  hardware road — both send faces, the receive face and the HW→CPU
  transfer — now mints once and routes through one shared policy.
  Observable: a declined coded surface met while sending advances the
  probe when a candidate is on trial, and falls back to software when one
  has committed, instead of surfacing as a plain error nobody acts on.

- **The resampler's `Again` meant two opposite things, and a caller that
  believed it spun forever.** `receive_frame` returned
  `ResampleError::Again` pre-EOF with nothing ready **and** post-EOF
  with the tail exhausted. Those are "send me more" and "there is no
  more"; a caller could tell them apart only by remembering whether it
  had itself called `send_eof`, and a drain written against the seam
  alone could not. It is now `Received::NeedsInput` and
  `Received::Ended`, and the confusion is unrepresentable.

  The regression that proves it —
  `a_drained_tail_says_ended_instead_of_asking_for_input_that_cannot_come`
  in `tests/resample.rs` — is a generic drain with no input left to
  offer and an iteration cap, so a `NeedsInput` after EOF fails the test
  instead of hanging it.

- **And the end of the tail is now latched.** `swr_get_delay` stands at
  a residue `swr_convert_frame` will never emit — 16 samples on the test
  corpus — so every poll past the end used to re-enter the flush road,
  allocating an output frame and asking `swr` again. Harmless while
  allocation succeeds and wrong when it does not: a session that had
  already answered `Ended` would start answering with an allocation
  error, turning a settled protocol state back into a fault. A `drained`
  latch makes the end cheap and unconditional; `flush` clears it with
  the rest of the session.

- **`SubtitleDecodeError::NoFrameReady` covered both conditions too, and
  worse.** This backend's `send_eof` is a documented no-op — the legacy
  `avcodec_decode_subtitle2` API buffers nothing — so "no cue yet" and
  "there will be no more cues" were *literally the same value*, and a
  caller draining a subtitle track to its end had no terminating
  condition at all. The session now keeps an `eof` latch (set by
  `send_eof`, cleared by `flush`) purely so `receive_frame` can tell the
  two apart. A held cue is still delivered after EOF: the latch ends the
  session, it does not discard what the session already made.

### Changed

- **BREAKING: every `receive_frame` on this crate's faces returns
  `mediadecode::Received`, and every `send_packet` / `send_frame` /
  `send_eof` returns `mediadecode::Sent`** — the three `mediadecode`
  decoder impls, the resampler impl, and the raw hardware
  `VideoDecoder`.

  **The FFmpeg errno stops inside this crate.** `VideoDecoder::receive_frame`
  documented itself as returning "the same transient signals as
  `ffmpeg::decoder::Video`: `Error::Ffmpeg(Other { errno: EAGAIN })` …
  and `Error::Ffmpeg(Eof)`"; it now answers `Received::NeedsInput` and
  `Received::Ended`, so no caller has an errno to decode.

  Both mappings go through one gate, `decoder::receive_status`, and it
  takes the *funnel's output* rather than libavcodec's raw error. That
  ordering is load-bearing: `software_exit` / `hw_exit` collect a
  `get_format` or allocator-judge refusal the callback state is holding,
  and a classifier placed in front of them would read a road with a
  named refusal waiting as "needs input". Every receive site funnels
  first and gates second.

- **BREAKING: four arms are gone.** `ResampleError::Again`,
  `SubtitleDecodeError::NoFrameReady`, `VideoDecodeError::FramePending`
  and `SubtitleDecodeError::FramePending` were never faults; they are
  `Received` and `Sent` states now. Every one of these enums carries
  faults only.

  `ResampleError::AfterEof` **stays an error**, and the line it sits on
  is the one this release drew: a resampler that has been told the
  stream ended will refuse the frame however much is drained first, so
  answering `MustDrain` would send the caller into a loop with no exit.
  Back pressure is "not now"; this is "not ever, until you flush".

- **The park refusal is now spelled as back pressure, and the parking
  discipline is byte-for-byte unchanged.** `FramePending` already
  documented its own escape — "call `receive_frame`, or `flush` to
  abandon it" — which is to say it was back pressure wearing an error's
  clothes. What moved is the spelling; what did not move is the seat,
  the two scratches it protects, or the fallback it prevents committing
  underneath a parked frame.

  The **audio** road still has no park refusal, and now that asymmetry
  is visible instead of hidden: it is one vocabulary with one backend
  answering it differently (single scratch, so a submission under a park
  costs nothing) rather than an arm two error types had and a third did
  not. `the_audio_face_answers_the_same_vocabulary_without_a_park_refusal`
  pins it.

- **`is_transient` had to stop treating `EAGAIN` and `EOF` alike on the
  send road, and that is a finding rather than a refactor.**
  `avcodec_send_packet` answers `AVERROR_EOF` for a different fact than
  `avcodec_receive_frame` does: not "the stream is over" but *"this
  decoder was already told the stream is over, and you sent something
  anyway"*. The old predicate collapsed the two, so a caller could not
  tell "drain and retry" from "you already sent EOF" — and under the new
  vocabulary reading the second as `MustDrain` would have sent it into a
  drain loop that can never make the next offer succeed. The send gate
  (`send_status`) therefore admits `EAGAIN` alone; `EOF` stays a fault.

  What is left of `is_transient` is the probe-replay drain, which reads
  a raw `ffmpeg_next::decoder::Video` that never crosses a public seam
  and for which "wants input" and "finished" really are one answer.

- **`send_eof`'s commit test is no longer `is_ok()`.** `Ok(Sent::MustDrain)`
  means the decoder did not take the end-of-stream; recording
  `eof_sent` there would make a later fallback inject an EOF into the
  software decoder for a signal that was never accepted — the exact
  half-mutation the local `eof_pending` argument exists to prevent on
  the failure road. The type change is what surfaced it.

- The one-shot `FfmpegImageDecoder` keeps `ImageDecodeError::NoImage`,
  and its internal `EOF`/post-`send_eof`-`EAGAIN` collapse now routes
  through the same `receive_status` gate — so the crate has exactly one
  place that knows the errno spellings. The *collapse* is the point: a
  one-shot decode has no session states, so both conditions really are
  the same fact about the payload, and that fact is an error.

- **All nine boundary error enums gain `#[non_exhaustive]`** — this
  crate's seven (`Error`, `VideoDecodeError`, `AudioDecodeError`,
  `SubtitleDecodeError`, `ImageDecodeError`, `DemuxError`,
  `ResampleError`) plus the two in the WebCodecs adapter. **This is the
  purchase the 0.10 window pays for:** adding the attribute is itself
  breaking, so it can only be done in a release that is already
  breaking, and doing it once makes every future arm additive forever.

  The counterpart decision is the status enums' — `Sent` and `Received`
  stay exhaustive. Closed protocol vocabulary versus open fault
  taxonomy: a consumer that meets an unknown *fault* should take its
  generic-fault path, and a consumer that missed a *protocol state*
  should not compile.

  Mechanically, the attribute binds across crate boundaries and
  integration-test binaries are separate crates, so an exhaustive match
  on one of these enums in `tests/` needs a rest arm. Exactly one did
  (`owned_carriers.rs`, the audio pre-allocation ordering proof), and
  the arm earns its place: a third refusal shape there would mean the
  ceiling was enforced by a road the test has never seen, which is worth
  failing on rather than absorbing.

- Every drain **and feed** loop in the crate's tests, examples, benches
  and README is rewritten to the exhaustive match. The
  `while …receive_frame(…).is_ok()` idiom is not merely discouraged now
  — under the new signature it is an infinite loop, because "needs
  input" is a success — and the two-offer dance is replaced by a loop
  that offers until the session says it took the packet.

  `examples/decode_via_trait.rs` is the one worth naming: the crate's own
  trait-generic example previously drained blind and could not
  distinguish the conditions its own comment named (`// EAGAIN: drain and
  retry`). Its non-generic sibling could, but only by matching
  `Error::Ffmpeg(Other { errno })` in user code. Both now read the same
  states on both faces, and the generic one surfaces receive-side
  failures instead of swallowing them.


## [0.9.0] - 2026-08-26

### Fixed

- **The cold software fallback still lost allocator refusals.**
  `degrade_to_sw_inner` opens a *temporary* software decoder, forwards
  the failure arm's packet or EOF into it, and drops it on any error.
  That decoder owns the callback state, so a `judge_buffer` refusal
  recorded during either forward died with it — and the caller received
  an ambiguous FFmpeg error inside a `FallbackFailed` envelope. It was
  the last software road still wrapping libavcodec's `EINVAL` raw.

  The state is captured before the forward, and both calls route
  through `software_exit`.

- **And the wrapping was renamed, because the envelope was the wrong
  one.** A budget refusal during fallback now travels **unwrapped**, as
  `FrameBudgetExceeded`, instead of arriving as
  `FallbackFailed(FrameBudgetExceeded)`:

  - `FallbackFailed` means the fallback *machinery* could not complete,
    and its contract is to hand back the unconsumed packets so a caller
    can re-drive them. On the post-commit road that set is empty by
    construction — the probe buffer is gone and no replay frames are
    retained — so the envelope carries no recovery affordance, only a
    label.
  - And the label invites the wrong action. Re-driving is the natural
    response to a fallback failure, and re-driving a budget refusal
    under the same limits refuses identically. `FrameBudgetExceeded`
    names the action that can succeed: raise the ceiling, or accept the
    refusal.

  Genuine machinery failures keep the envelope. One fact, one name.

- **The hardware-pool fallbacks underpriced, in both directions.** When
  `avcodec_get_hw_frames_parameters` could not answer, the judge fell
  back to codec-aligned dimensions — a fallback whose own comment
  admitted it could be *smaller* than the pool, since D3D11 HEVC and
  AV1 round both dimensions to 128 while the codec may round to less.
  And when the pool declared dimensions in a layout libavutil would not
  size, the stand-in was a bare `w * h * 16`, omitting the dimension
  alignment and per-plane slack every accurate estimate carries.

  Both are closed. A pool that will not declare its dimensions and
  format is now **refused** — the rule the transfer judge already kept:
  an unprovable extent is not a small one, and declining the hardware
  format falls the decode back to software where the same budget
  applies. A declared-but-unpriceable layout is charged
  `footprint::video_frame_bytes_upper_bound`, built from the footprint's
  own machinery so it dominates every layout the build could have
  priced at that extent — and its per-pixel rate comes from the live
  census, not a literal, so a build with a wider format is charged for
  it.

  A context with no hardware device attached is passed through rather
  than refused: that is *no pool*, not a pool declining to describe
  itself, and this crate's hardware road always attaches a device before
  opening.

- **The transfer judge ignored unpriceable candidates in mixed lists.**
  FFmpeg picks the destination format from
  `av_hwframe_transfer_get_formats`; this crate does not. The fold
  updated its maximum only on priceable members and reached for a
  fallback only when *nothing* priced — so a list holding one cheap
  priceable format beside one unpriceable format was judged at the cheap
  price, while FFmpeg stayed free to select the member that had been
  skipped. Every candidate is folded in now, at the conservative bound
  when it cannot be priced, with an immediate refusal if even that
  cannot be formed.

- **The software-video road never consumed `FrameBudgetExceeded`.**
  `SwDecoder` owned the callback state but only `Deref`d the decoder
  underneath, so every call site kept wrapping libavcodec's error raw —
  ordinary send, receive and EOF, the replay drain, and the
  cold-fallback forward, the last of which drops the state when it
  finishes. A budget refusal on that whole road surfaced as the bare
  `EINVAL` libavcodec also uses for corrupt input.

  `SwDecoder` exposes its state now, and all nine decode-failure sites
  route through `software_exit` before wrapping. The three remaining
  `Error::Ffmpeg` constructions on that road are caps this crate mints
  itself — a replay-queue `ENOMEM`, two retry-exhaustion `EAGAIN`s — and
  have no libavcodec verdict to reinterpret.

- **The byte-derived coarse gates over-refused cheap formats.**
  `max_pixels` was written as `min(the caller's pixel limit,
  max_frame_bytes / worst-bytes-per-pixel)` and `max_samples` as
  `max_frame_bytes / worst-bytes-per-sample`, so that the byte ceiling
  could bite before libavcodec allocated. Both translations charged
  every stream the widest format in existence, and both refused
  ordinary media: a 1920x1080 `yuv420p` frame costs 3.14 MiB and was
  refused under a 4 MiB budget at `ff_set_dimensions`; a 6-channel
  `s16` frame fitting 64 KiB was refused by a ruler pricing it at `f64`
  rates.

  Both are gone. `max_pixels` carries the caller's number verbatim, and
  `max_samples` is left alone. Nothing is traded away, because the seat
  that enforces bytes — `judge_buffer` — is *itself* pre-allocation:
  `get_buffer2` **is** the allocation, and it prices the frame's real
  format at its real dimensions through the allocator-parity footprint.
  An exact judge at the allocation beats an approximate one before it.

- **The hardware pool judge carried its own bound, as it had to.** It
  compared the pool's pixel count against `AVCodecContext.max_pixels`,
  which only worked while that field held a byte-derived number. It now
  prices the pool's declared dimensions — and its `sw_format`, read as
  the integer it is — through the same footprint, against the
  `max_frame_bytes` its callback state already holds, falling back to
  the worst per-pixel rate where a layout cannot be priced.

  That is strictly more accurate than the scalar it replaces, and the
  cropped-h264 lane shows it: a 1920x1088 NV12 pool costs 3,135,488
  bytes and was refused under a 16 MiB budget it fits five times over.
  The lane now refuses it at 1 MiB, where it genuinely does not fit,
  and asserts that 16 MiB decodes.

- **Software byte refusals surfaced as bare `EINVAL`.** A `get_buffer2`
  callback can only answer libavcodec with an errno, and `EINVAL` is
  also what libavcodec reports for corrupt input — so a caller could not
  tell a budget refusal this crate made from a broken file, and only one
  of those is worth retrying with a larger ceiling.

  The refusal is recorded in the callback state, clear-on-read, and
  collected by a `software_exit` funnel that every software decoder road
  routes through — video, audio, still and subtitle. It surfaces as
  `Error::FrameBudgetExceeded`, carrying the bytes, the limit and
  whether the frame was a picture or audio.

- **The allocator judge restated a logical limit as an arithmetic one.**
  It compared the **aligned** dimensions against `AVCodecContext.max_pixels`,
  which is `min(the caller's pixel limit, byte ceiling /
  worst-bytes-per-pixel)` — so when the pixel limit was the tighter
  seat, alignment inflation alone refused frames that satisfied *both*
  requested limits. A 65536x1 `gray8` frame under `max_pixels = 65536`
  meets the pixel limit exactly on its raw dimensions and costs about
  2 MiB; against a generous byte budget it was refused anyway, for
  arithmetic the caller never asked about.

  That comparison was R11's instrument against degenerate-shape bypass,
  added when the callback had no accurate byte check and a 65536x1 frame
  could slip thirty-two times its raw pixel count past a raw gate. Once
  the caller's `max_frame_bytes` was threaded into the callback, the
  footprint — which prices the aligned dimensions itself — subsumed it.
  So the comparison is **removed**: it was redundant against the shape
  it was built for, and wrong against every frame whose pixel limit was
  the tighter of the two.

  The seats now ask one question each. **Logical extent** is
  `max_pixels`, enforced by libavcodec in `ff_set_dimensions` against
  the **raw** dimensions — the semantics that field has and that FFmpeg
  documents. **Allocation cost** is the footprint against
  `max_frame_bytes`. The `get_format` hardware judge is untouched: it is
  a separate seat, judging the pool's own declared extent.

- **The probe meter consumed bytes it then refused to hand over.** It
  read libavformat's whole request and only afterwards discovered the
  budget was exceeded, so a 32 KiB request against a 1 KiB allowance
  took all 32 KiB from the reader, returned none of them, and reported
  having read bytes nobody received. A container that would have
  finished probing inside its allowance was refused for the shape of
  libavformat's buffer rather than for its own size.

  Requests are capped to the remaining allowance now — a short read is
  ordinary, and every `Read` may return fewer bytes than asked — and
  only bytes actually delivered are charged, so
  `ProbeBudgetExhausted::read` reports what libavformat really got.

  The boundary is defined rather than incidental: a read landing
  **exactly** on zero remaining is served in full and does not trip,
  because a budget is a ceiling on what is spent and not on asking; the
  **next** nonempty read is the one refused. A zero-length request asks
  for nothing and is passed through whatever the budget says.

- **The allocator judge invented a smaller ceiling when the pixel seat
  was the tighter one.** It recovered a byte ceiling from
  `AVCodecContext.max_pixels`, which is `min(pixel ceiling, byte ceiling
  / worst-bytes-per-pixel)` — so when the pixel seat won, `max_pixels`
  no longer encoded the byte ceiling at all. A 256x256 frame at 16 bytes
  a pixel under `max_pixels = 65536` with a 2 MiB byte budget satisfies
  **both** of the caller's limits, costs 1,050,624 bytes, and was judged
  against 1,048,576 and refused — by exactly the 2,048 bytes of
  alignment and slack the recovery could not see. The claim that the
  conflation was harmless in one direction is withdrawn; it was wrong.

  `CallbackState` carries the caller's `max_frame_bytes` verbatim now.
  That seat already existed for the `get_format` declination, and
  `build_codec_context` — the single point every decoder in the crate is
  built through — allocates and installs it for every road, so the five
  callers and the software-decoder variant each keep the box alive
  beside their context. Both media read that one number; the audio
  road's `max_samples` recovery, though exact, is gone, because two
  sources of truth for one ceiling is how the first one went wrong.

### Added

- **Tier two of the [resource governance contract][gov] enumerated** in
  the `buffer` module's accounting, beside the tier-one table it already
  kept (user ruling of 2026-08-25). Every FFmpeg resource knob this
  crate sets, and what each one bounds:

  | knob | where | bounds |
  |---|---|---|
  | `AVCodecContext.max_pixels` | every opened decoder | the coded extent, in `ff_set_dimensions`, before any frame or pool exists |
  | the `get_format` coded-dims judge | the hardware road | the pool's own declared extent, from `avcodec_get_hw_frames_parameters` |
  | `AVCodecContext.max_samples` | every opened decoder | `nb_samples * channels`, in `ff_get_buffer` |
  | the `get_buffer2` byte judge | every software decode | what the allocator will actually take, pictures and audio alike |
  | the pre-transfer judge | every `av_hwframe_transfer_data` | the CPU destination, at the frames-context pool dims |
  | `probesize` / `formatprobesize` | both demux entrypoints | what probe and analysis may consume |
  | `max_streams` | both demux entrypoints | the `AVStream` array a header can conjure |
  | the `AVIOContext` byte meter | the reader demux entrypoint | total bytes libavformat is handed, hard |

  Recorded as **defense in depth, not a proof**: together they cover
  every interposition point FFmpeg exposes, which is not the same as
  covering FFmpeg. The demux residual — libavformat builds the attached
  picture, extradata and coded side data itself, so those seats measure
  this crate's copies of allocations that already happened — is folded
  into that section rather than stated twice, and the part that cannot
  be reached (a parser's amplification, and the path entrypoint's want
  of a meter) is named as tier three with a pointer to the contract.

  [gov]: https://docs.rs/mediadecode/latest/mediadecode/adapter/index.html


## [0.9.0] - 2026-08-24

### Added — the second carrier lane

*User-ruled 2026-08-25, and folded into 0.9.0 because the release is
unpublished: the bare names change meaning, and nothing outside this
workspace has seen them yet.*

- **`FfmpegBuffer` returns**, with 0.8's name and semantics: an
  `av_buffer_ref`-refcounted view onto FFmpeg's own allocation, `Send`
  and **not** `Sync`, `Clone` a refcount bump, `AsRef<[u8]>`. It is not
  0.8 restored unchanged — every lesson from the amputation round that
  lands on this type has been re-applied:

  - the **extent is proved before a view exists**. 0.8 formed a view
    over a packet's claimed range and let a malformed `size` hand out a
    slice nobody had checked; the proof now runs in `payload_of`, which
    **both lanes share**, so the view lane cannot regress it
    independently;
  - `AV_PKT_FLAG_TRUSTED` is refused on both legs, and for a sharper
    reason here — sharing an allocation does not make its pointers own
    what they name;
  - the budgets are unchanged, because they judge sizes and not copies:
    a ceiling bounds what a caller is handed and asked to hold, and on
    this lane also how long a pool slot stays out.

- **A sealed carrier seam, whose operations are not reachable at all.**
  `FfmpegCarrier` names a lane and says what it carries; every operation
  lives on `CarrierOps`, a crate-private trait that is deliberately
  **not** a supertrait of it.

  Two walls were tried before that one held. Sealing closed the seam to
  outside *implementations* and left every operation callable. Moving
  the operations onto the seal — a `pub trait` in a private module —
  looked airtight and was not: associated items resolve through a
  **bound**, not through a path, so `fn f<C: FfmpegCarrier>()` written
  in a downstream crate type-checked `C::from_rows(1, 64, |_| &[0])`
  (safe, and an out-of-bounds read) and `C::capture_packet_payload(..)`
  (which mints the padding claim the send leg trusts). Only a trait no
  writable bound reaches actually stops a call. Six `compile_fail`
  doctests — rustdoc compiles each as a separate downstream crate — pin
  that, each against the exact error code.

  In the same spirit, `FfmpegBuffer`'s extent-taking constructors are
  crate-private, and the row gather **checks** each row's length rather
  than asserting it: a `debug_assert` is a note to the author, not a
  bounds check, and it is absent from the profile that matters.

  **The public faces are written per lane** so that no signature a
  consumer reads names a trait they cannot: `CarrierDemuxer`, the four
  decoders and `CarrierResampler` carry only `C: FfmpegCarrier` on their
  declarations, and their inherent and trait impls are macro-generated
  once per lane over crate-private generic bodies. A consumer can
  therefore hold and name `CarrierDemuxer<C>` in their own generic code
  and be generic over `C::Buffer`. What they cannot do is *drive* a lane
  generically — that needs the operations — so lane-generic helpers take
  `C::Buffer` and the doors are instantiated at the two concrete lanes.
  Compile-**pass** doctests pin the first, the six compile-fail ones pin
  the second.

- **A refusal that another attempt could survive costs no packet.**
  `av_read_frame` advances the container: once it returns, that packet
  is off the wire and nothing brings it back. A conversion that then
  failed on an *allocation* — a refcount the view lane could not take,
  a copy the shared road could not make — dropped it, leaving a live
  session that answered the next pull with the **following** packet.
  Compressed data and subtitle cues went missing under memory pressure,
  quietly, and a caller who retried could not tell.

  The read and the conversion are now one transaction with a seat
  between them: a transient refusal parks the packet, and the next pull
  re-attempts *that* packet before reading another. It is the same
  park-then-replay the decode household already runs — the video
  decoder holds `sw_replay_frames`, the probe holds its rescue history —
  for the same reason: a byte C has already given up is not re-askable.
  The provenance is parked with the packet rather than re-probed, since
  it is an observation about the moment of delivery.

  Only *transient* refusals park, decided by a crate-private
  `parks_in_demux`: an allocation
  that failed says nothing about the packet, while a payload outside its
  own buffer or a body past the ceiling is a fact about it, and parking
  those would answer every later pull with the same error instead of
  letting the session make progress.

  That predicate was briefly public as `is_transient` and should not
  have been: whether retrying helps depends on *what was retried*.
  `SharedPayload` is permanent for a caller who keeps their other
  reference and retryable the moment they drop it; `CaptureFailed` is
  worth another attempt only if the packet still exists to attempt,
  which on a consuming conversion it does not. Only the demux loop knows
  both halves. A public retry signal would have to be operation-aware —
  a door left open, not a thing approximated now.

  The arms, censused: the four timed roads need the seat and have it;
  the duplicate-attachment road converts nothing, so there is nothing to
  park; and the hoisted attachment happens during `open`, where a
  failure fails the open and no session exists to lose anything. Two
  fault lanes drive it under a capped FFmpeg allocator — one on the
  unique road where `av_buffer_ref` fails, one on the shared road where
  the copy fails — each asserting that the retry re-attempts the same
  packet and that the recovered stream equals the reference exactly.

  **The seat is cleared by a seek that happened, not by one that was
  attempted.** A successful seek discards the parked packet, which
  belongs to the position being left. A *failed* seek does not: the
  session stays where it was, and that packet is off the wire, so
  discarding it would be the same silent loss with nothing able to
  re-read it. FFmpeg does not specify where a container sits after a
  seek that errored, and this crate does not guess — it keeps a packet
  the container really did deliver, and a caller who saw the seek fail
  knows the position is not the one they asked for.

- **Decoded frames get the same transaction.** `receive_frame` advances
  libavcodec exactly as `av_read_frame` advances a container: the frame
  it fills the scratch with is out of the codec's queue and nothing
  re-offers it. A conversion that then failed on an allocation left it
  in a scratch the next call overwrote. Both timed decoders now hold the
  scratch until a carrier exists for it, and the video decoder's replay
  queue is **peeked, not popped** — that queue is the rescue history's
  only copy, so popping before the conversion committed lost the very
  frames it exists to preserve.

  Every receive road, censused:

  | road | seat | why |
  |---|---|---|
  | audio | yes | scratch held until the conversion commits |
  | video, direct (HW and SW) | yes | same, on whichever scratch the state uses — **and the sends refuse while it is taken**, see below |
  | video, replay queue (both entries) | yes | peeked; popped only on success |
  | subtitle | yes | the decoded `AVSubtitle` is held and the conversion moved to `receive_frame` |
  | image, one-shot | **not needed** | the packet is *borrowed*, so a failure leaves the caller everything needed to call again — and the decoder is now flushed on that road so the retry starts where the first attempt did |
  | resampler output | **not needed** | its queue holds frames already built: the carrier's claim is taken in `prepare_output`, before `swr` consumes anything, and `finish_output` is infallible by construction. That is what the reserve-then-commit seam was for |

  `ConvertError` gained the split this needed: `BufferAcquireFailed` was
  one arm for two facts — a plane outside the frame's own buffers, which
  is permanent, and an allocation that failed, which is not. The second
  is now `CarrierAllocFailed`, and only it parks; the subtitle bitmap's
  data copy was reclassified onto it too. Fault lanes on audio, video,
  the replay queue and subtitle drive a capped allocator at the moment
  of conversion and assert the retry yields the *same* output.

  The replay queue has **two** delivery entries, not one: the peek at
  the top of `receive_frame`, and the branch that runs when the probe is
  exhausted at *frame* time, where `fall_back_to_sw` fills the queue and
  the head is converted in that same call. The second popped before
  converting. Both peek now, and every access to that queue is
  accounted for — two entries, one `append`, one `clear`, one test
  predicate.

  **A parked video frame pins the state that parked it.** That decoder
  has two scratches and can change which one is current, so a seat that
  merely recorded "something is parked" was not enough: a
  hardware-to-software fallback committed while a hardware frame was
  parked sent the retry to the *software* scratch — a stale frame, or a
  permanent refusal that stranded a decoded one. Both send roads can
  commit that fallback, so both now refuse by name
  (`VideoDecodeError::FramePending`) while the seat is taken.

  Two shapes were weighed. Recording the producing road in the seat is
  more permissive but adds state that every future delivery road must
  remember to keep in step — which is precisely what had just gone
  wrong. Refusing instead makes the retry's state the state that parked
  it **by construction**, is smaller, and is the discipline this crate
  already keeps one seat over (`SubtitleDecodeError::FramePending`). The
  deadlock census came out clean: while parked, `receive_frame` is
  always callable and always makes progress or repeats the same
  parkable refusal, and a caller who would rather abandon the frame
  calls `flush` — the same two answers the subtitle seat has always
  given. A caller who ignores the refusal and pushes on gets named
  errors rather than silent loss, which is the contract, not a
  regression of it.

  And every delivery now passes one **commit point**. The bookkeeping a
  delivery owes — clearing the seat, and clearing a keyframe-anchored
  resync guard — was attached to some roads and not others, so a parked
  software recovery frame delivered on the retry road skipped
  `resync_on_frame` and let a clean EOF escalate with a false
  `PostCommitNeverResynced`. All five roads (both scratches, both replay
  entries, the retry) funnel through `commit_delivery`, and a regression
  drives a parked post-keyframe recovery frame to a clean EOF.

  The subtitle road needed the seat most of all, and had the least: its
  conversion ran inside `send_packet`, so an allocation that failed
  freed the scratch and returned — and the cue, which
  `avcodec_decode_subtitle2` had already consumed the packet to make,
  was simply gone. The decoded `AVSubtitle` is now held until
  `receive_frame` has a carrier for it. Unlike the image road there is
  no resubmit to fall back on: the caller's packet is spent.

- **A view is never taken from a buffer somebody else still references.**
  Consuming the source proves no other handle survives only if the
  source's buffer was the caller's alone to give — and
  `ffmpeg_next::Packet::clone` calls `av_packet_ref` followed by
  `av_packet_make_writable` and **ignores both return codes**, so under
  allocation failure it returns a "clone" that silently still shares.
  Feeding one to a consuming conversion produced a view of a buffer the
  caller kept, and could still write through.

  The payload road now asks the buffer directly, before any byte of it
  is read, and **refuses by name** — `PacketBufferError::SharedPayload`,
  carrying the count — when the answer is anything but one.

  It first answered with a silent copy, on the argument that a copy is
  always sound and keeps the API total. **That was overturned, and the
  correction is worth stating**: a copy needs a *read*, and a refcount
  above one is exactly the state in which another safe `data_mut`
  handle may exist and may be writing — on another thread, since
  `Packet` is `Send`. The read *is* the race. Totality was traded for
  soundness the wrong way round; the refusal now happens with the
  ordering visible in one function, nothing between the bounds proof and
  the refusal touching the payload.

  The rule is about **who** holds the other reference, not the count.
  `PayloadProvenance` distinguishes a delivered packet — where the
  second reference is another `Packet` and `data_mut` is one call away —
  from a container's `attached_pic`, which libavformat holds for the
  lifetime of the format context and never writes again. A blanket
  "refcount must be one" looked right and refused every embedded cover
  picture in the corpus; the distinction is now a parameter with its
  reason attached, and a test that fails without it.

  The dichotomy that survived is **delivered by libavformat** versus
  **handed over by a caller**, and it took two counterexamples to find.
  A parked cover picture is one. The other is a whole family: every
  demuxer built on `FFDemuxSubtitlesQueue` — SubRip, SubViewer,
  MicroDVD, WebVTT — parses its cues at open, keeps the packets, and
  answers each read with an `av_packet_ref` of one, so **every** packet
  it delivers has two references. A rule demanding a lone reference
  refused all of them, on both lanes.

  What separates the two sides is who can *write*, not how many
  references exist. Every secondary reference to a demux-delivered
  packet is libavformat's own: no `ffmpeg_next::Packet` wraps one, so
  there is no safe `data_mut` to race, and while this crate reads it
  holds the `AVFormatContext` exclusively. A caller's packet may share
  with another `Packet`, whose `data_mut` writes in place from safe code
  without consulting writability — that is the writer the rule exists
  for.

  Three rows, each with its argument recorded next to it:

  | provenance | unique | shared | why |
  |---|---|---|---|
  | caller-supplied | capture | **refuse** | a second `Packet`'s `data_mut` writes in place, possibly on another thread |
  | demux-delivered | capture | **copy** | the read is race-free, but a *window* would outlive the exclusivity that makes it so |
  | attached picture | capture | **capture** | written once while the container opened and never again, so a window is as stable as one onto a private buffer |

  The middle row is the deliberate one. Sharing there would have rested
  on "libavformat honours its own copy-on-write rules for as long as the
  carrier lives"; copying rests on "nothing can be writing while we hold
  the context", which is a fact about the call rather than a promise
  about anyone's future behaviour. Subtitle cues are measured in bytes,
  so the copy is not a cost worth an argument.

  The attached-picture row reaches one road more than the hoist:
  libavformat queues a stream's parked picture as that stream's **first
  packet**, and a stream carrying `ATTACHED_PIC | TIMED_THUMBNAILS` —
  chapter thumbnails — is deliberately classified here as **video**, so
  its first pull arrives through the ordinary read path.

  The identity is a **proof, not a heuristic**: `av_buffer_ref` copies
  the source's `buffer` field, so two `AVBufferRef`s name one allocation
  exactly when their `buffer` pointers match — comparing the references
  themselves would answer "no" to the very case this is for. Disposition
  bits say a stream *has* a parked picture, not that this packet is it,
  and "the first packet" is an ordering assumption libavformat's
  contract does not fix; neither is used.

  Two fault lanes prove the refusal: a deterministic shared packet built
  with `av_packet_ref`, and a subprocess lane that caps FFmpeg's
  allocator to reproduce the silently-shared clone itself.

  The lesson, recorded because it cost a round: **a dependency's happy
  path is not its contract.** The verification that missed this read
  `Clone` and confirmed it deep-copies. It did not read what happens
  when the copy fails.

- **A submission that can be recorded is not shared.** The scoped
  submission's proof — built, lent, dropped, so nothing that could take
  a `&mut` into the shared bytes ever exists — is a claim about the
  function, and the hardware probe made it false. While probing, the
  video decoder `av_packet_ref`s every accepted packet into a rescue
  history so a caller can replay it after a failed fallback, and
  `FallbackFailed::unconsumed_packets` hands those recordings back as
  owned, **mutable** `Packet`s: a live mutable alias of a carrier the
  caller may still be reading.

  The route is now chosen per submission. Inside the recording window
  the body is copied; once the probe commits — and on every software
  road, which records nothing — the send is zero-copy as before. The
  regression drives a probe-era failure on the view lane with every
  carrier retained, and checks that no rescued packet addresses any of
  them and that writing through all of them changes none. Flipping the
  route back to sharing makes it fail, which is how the lane was shown
  to be live.

- **The default is the view lane.** `FfmpegDemuxer` and the bare alias
  family (`VideoPacket`, `AudioPacket`, `SubtitlePacket`, `DataPacket`,
  `AttachmentPacket`, `DemuxedPacket`) now mean **View**; the owned
  family is explicit — `FfmpegOwnedDemuxer`, `OwnedVideoPacket`, and so
  on. Both aliases point at one generic `CarrierDemuxer<C>` with one set
  of constructors, so `FfmpegDemuxer::open` and
  `FfmpegOwnedDemuxer::open` both resolve and read the same.

  **This is a semantic shift in the bare names**, recorded here rather
  than in a migration note: what `VideoPacket` denotes changed from a
  copied payload to a viewed one. Nothing outside this workspace can
  have depended on it.

- **The send leg shares too, where it can prove it may — and only
  inside a scoped submission.** A packet a caller is *handed* is an
  `ffmpeg_next::Packet`, which lends `&mut [u8]` through `data_mut`
  while the carrier it was built from still lends `&[u8]`: a shared body
  there is an aliasing `&mut` out of entirely safe code, and `!Sync`
  does not help because one thread suffices. So the public reverse
  builders copy on **both** lanes, and the zero-copy send lives on a
  crate-private road that builds the packet, submits it to the decoder,
  and drops it inside one call — no value that could produce a `&mut`
  into the shared bytes ever exists.

  Sharing there needs two facts, not one. **Provenance**: only a payload
  captured out of an `AVPacket`'s own buffer may be shared, because that
  is the one place libavformat's zeroed-padding contract applies.
  Trailing *capacity* is not that contract — a decoded plane has more
  pixels behind it and a resampled one has more samples, and a bitstream
  reader running past the payload eats them as bitstream. Provenance is
  recorded at capture, where it is known, and narrowing a payload drops
  it. **Extent**: the view must still leave
  `AV_INPUT_BUFFER_PADDING_SIZE` between its end and the buffer's.
  Where either fails, the packet is copied.

  The empty carrier is handled before any of it: it is backed by no
  `AVBufferRef` at all, so a side-data-only packet — an ordinary thing —
  reaches the send road with nothing to dereference, and gets a
  payload-less `AVPacket` its side data attaches to.

  Worth recording: the census found 0.8 **did not have this**. Its
  reverse builders called the same copy the owned lane uses, so the
  zero-copy chain 0.8 shipped ran demux-to-consumer and stopped. This is
  new, not restored.

- **A conversion that borrows its source copies; one that shares
  consumes.** `ffmpeg_next`'s `Packet`, `frame::Video` and `frame::Audio`
  all lend `&mut [u8]` from safe code and share their buffers by
  refcount with no copy-on-write. A conversion taking `&Packet` and
  returning a view therefore left the caller holding a mutable alias of
  every byte the carrier read — and both sides are `Send`, so the two
  halves need not even be on one thread. No lifetime on the carrier
  would have helped; the carrier is `'static` by design.

  So the roads split by ownership, and the compiler enforces it:

  | road | source | lane |
  |---|---|---|
  | `{kind}_packet_from_ffmpeg_as::<C>` | **by value** | either |
  | `{kind}_packet_from_ffmpeg_in` | **by value** | view |
  | `{kind}_packet_from_ffmpeg` | borrowed | owned |
  | `owned_{kind}_packet_from_ffmpeg_in` (new) | borrowed | owned, with budgets |
  | `convert::{video,audio,image,subtitle}_frame_from` | borrowed | **owned only** |
  | `convert::av_frame_to_*::<C>` (`unsafe`) | raw pointer | either, contract states the obligation |

  The crate's own roads were unaffected in substance: the demuxer owns
  the packet it just read (it now hands it over), the decoders own the
  frames they decode into and reach the conversion through the `unsafe`
  entry, and the resampler owns its output. The zero-copy chain is
  intact end to end. What a *caller* can no longer do is ask for a view
  of something they are still holding — `compile_fail` doctests pin both
  refusals, including the exact aliasing program (`E0382`: the source is
  moved, so `data_mut` cannot be written).

- **Decoded frames carry view planes.** The frame road is lane-generic
  end to end: `VideoFrame`, `AudioFrame`, `SubtitleFrame` and
  `ImageFrame` mean the view lane, `OwnedVideoFrame` and its three
  siblings mean the other, and `empty_video_frame` /
  `empty_owned_video_frame` (with `*_as::<C>()` workers) build the
  destination each one needs. Every plane capture runs through the seam
  **after** the extent proofs that were already there, so the view lane
  inherits the bounds checks rather than restating them.

  Two rules decide what is shared, per medium:

  - a **tight** video or image plane (`linesize == row_bytes`) is a
    window at the decoder's own stride; a **padded** one is copied and
    compacted to `row_bytes` on *both* lanes, because only the first
    `row_bytes` of each row are the decoder's output and the rest is
    allocator scratch nothing wrote. Sharing that span would form a
    slice over uninitialised memory — undefined before a consumer reads
    a byte of it, and the same information leak the owned lane refused
    when it stopped exporting `linesize`;
  - an **audio** plane is a window over exactly
    `nb_samples × bytes_per_sample`, never the alignment padding
    `av_samples_get_buffer_size` allocated past it. `linesize` is used
    for one thing: proving the allocation really is as large as it
    claims.

  A palette plane — a flat `AVPALETTE_SIZE` run inside the frame's own
  buffer — is shared. A subtitle rect is not: `AVSubtitleRect` has no
  `buf[]`, its `data[]` are plain allocations `avsubtitle_free`
  releases, so both lanes copy.

- **The resampler produces view frames too**, through a
  reserve-then-commit pair on the seam. `FfmpegResampler` means the view
  lane, `FfmpegOwnedResampler` the other. The carrier's claim on each
  output plane is taken **before** `swr_convert_frame` runs and settled
  at the produced length after it — which keeps this type's founding
  rule intact on the new lane: `swr` consumes its input as it runs, so a
  failure after it has run would leave a session no caller can retry,
  and there is now no fallible step on that side of the conversion. The
  committed plane is the produced samples, never the capacity that was
  allocated for them.

  The hardware road needed no separate work: `av_hwframe_transfer_data`
  fills a refcounted destination `AVFrame`, which reaches the same
  lane-generic conversion the software road does.

- **The copy roads are fallible on both lanes.** `from_bytes` and
  `from_rows` return `Option`, so an allocation failure is a named
  refusal instead of a silently empty plane — a frame whose header says
  samples and whose planes say nothing is worse than an error. The
  **empty** carrier, by contrast, now allocates nothing on either lane:
  the view lane's is a null-backed zero-length view, which is what lets
  `empty_video_frame` stay infallible rather than reintroducing 0.8's
  four-allocations-that-could-fail placeholder under a new name.

- **What is deliberately not doubled.** The `*Extra` types and
  `SideDataEntry` stay monomorphic. Side data has no `AVBufferRef` to
  share — `AVPacketSideData` and `AVFrameSideData` payloads are plain
  allocations — so **both** lanes copy it, and a second family of extras
  would have been two names for one representation. The carrier
  parameter reaches the payload, not the annotations.

- **The parity lane compares content, not buffers.** Owned compacts a
  padded plane while View carries the tight one at the decoder's stride,
  so the spans are not comparable and asserting on them would have been
  a test of the lanes' plumbing rather than of their agreement.
  `tests/view_carriers.rs` compares row-wise for video and valid-prefix
  for audio, and proves the sharing itself by address: a tight plane's
  pointer lies inside the `AVFrame`'s own buffer, a padded plane's does
  not, an audio plane's lies inside its buffer while its span stops
  short of the padding, and a resampled plane looks into an allocation
  larger than the samples it exposes.


### Changed (BREAKING)

- **`FfmpegBuffer` is deleted, and every byte leaving FFmpeg is copied
  once into an `FfmpegBytes`.** This is the release, and everything
  else below follows from it.

  Through 0.8 a decoded frame's planes and a demuxed packet's payload
  were refcounted *views* into libavcodec's own `AVBufferRef`s, wrapped
  in an `FfmpegBuffer` whose `AsRef<[u8]>` pointed straight at FFmpeg
  memory. It was zero-copy and it was a lie of omission: a consumer
  holding a `VideoFrame` was holding libavcodec's allocation open
  without being told, `FfmpegBuffer` was `Send` and deliberately **not**
  `Sync`, so a frame could be moved to another thread but never read
  from two, and fanning one message out to two consumers was therefore
  impossible without the backend's permission. `mediadecode` 0.9 names
  that rule — the
  [D-seat amputation contract](../mediadecode/CHANGELOG.md) — and this
  crate now keeps it.

  The eight exits that changed carrier, all `FfmpegBuffer` →
  `FfmpegBytes`: the planes of `VideoFrame`, `AudioFrame` and (via its
  payload) `SubtitleFrame`, and the data of `VideoPacket`,
  `AudioPacket`, `SubtitlePacket`, `DataPacket` and `AttachmentPacket`.
  A delivered frame is now owned, `Send + Sync`, `'static`, and clones
  by refcount — see `tests/owned_carriers.rs`, which pins all nine
  forms (the four frame households and the five packet ones) from
  outside the crate.

  **What did not change is the geometry.** A video plane whose
  `linesize` is tight still arrives with the decoder's own stride and
  its full `linesize × height` bytes; a padded one still arrives
  compacted to `row_bytes`. 0.8 already copied the padded case; 0.9
  copies both. Nothing was relaid out, and the existing suites — which
  assert those strides and lengths — are unchanged.

- **`FfmpegBytes` is opaque, and that is deliberate.** It wraps an
  `Arc<[u8]>` today, through a **private** single-arm enum, and exposes
  `AsRef<[u8]>` / `as_slice` / `len` / `is_empty` / `ptr_eq` /
  `copy_from_slice` / `empty` plus `Clone`, `Debug`, `Default`, `Eq`
  and `Hash`. `Send + Sync` fall out of the arm; nothing is
  hand-asserted.

  Why not the bare `Arc<[u8]>` an earlier cut of this release used:
  the storage is going to gain a second strategy. Every exit now
  allocates, copies and frees per frame, and a 4K decode loop is
  asking the global allocator for eight megabytes sixty times a second
  and handing it back; the recorded answer is a plane pool
  ([#35](https://github.com/findit-studio/mediadecode/issues/35)),
  whose slabs satisfy the same contract — owned, `Send + Sync`,
  refcount-cloned, no FFmpeg lifetime. With a bare `Arc<[u8]>` in the
  aliases, adding the pool would change the type of every frame and
  every packet in the crate: a breaking release for a change a
  consumer cannot observe. Behind the newtype it is one new arm of a
  private enum. The enum has exactly one arm today and gains the
  second when the pool is built — this crate does not carry members
  nothing can produce.

  It is emphatically **not** 0.8's `FfmpegBuffer` under a new name.
  That type was opaque over *libavcodec's* memory, and the opacity is
  what hid the lifetime. This one is opaque over Rust-owned bytes, and
  the `AsRef<[u8]>` a consumer programs against says everything the
  consumer needs.

- **`SideDataEntry`'s payload is `FfmpegBytes`**, not `Vec<u8>`.
  `new`, `with_data` and `set_data` take one; `data()` still answers
  `&[u8]`, and a new `data_ref()` hands out the carrier for a consumer
  that wants to keep the bytes without copying them again. The reason
  is the contract one tier in: side data rides on packets and frames
  that a graph fans out, and a frame whose *pixels* clone by refcount
  and whose *metadata* deep-copies is the kind of asymmetry nobody
  remembers while profiling. `VideoFrameExtra::smpte_timecode` stays a
  `Vec<u32>` — those are parsed values, a handful of words, not bytes
  crossing a boundary.

- **`try_empty_video_frame`, `try_empty_audio_frame` and
  `try_empty_subtitle_frame` are gone; their `empty_*` siblings are
  infallible and no longer panic.** Those `Option`-returning
  constructors existed because building a placeholder meant four (or
  eight) one-byte `av_buffer_alloc` calls that could fail. The
  amputation removes the allocations — the empty carrier is made once
  for the process and cloned by refcount — and a constructor with no
  failure mode does not get to keep a `Result`-shaped door.

- **`PacketBufferError::Refcount` and `DemuxError::AttachmentAlloc`
  are gone**, with their payload structs. Both named an
  `av_buffer_ref` / `av_buffer_alloc` failure; neither call exists any
  more, so both arms were unreachable. An error variant nothing can
  produce is a claim about failure modes that is no longer true.

  `PacketBufferError::Bounds` stays, and matters **more** than it did:
  0.8 formed a view over a packet's claimed payload range and a
  malformed `size` handed out a slice nobody read; 0.9 reads every byte
  of it. Every side-data cap and every plane-geometry check is
  likewise unchanged and still runs before any allocation.

### Added

- **`FfmpegImageDecoder`** — `mediadecode`'s new one-shot
  `ImageDecoder`, backed by `libavcodec`, and with it the cover-art
  road end to end. Opened from an attachment track's codec parameters
  (`FfmpegImageDecoder::open`), fed the track's one
  `AttachmentPacket`, and answering an `ImageFrame`. The
  `AVCodecContext` is reset between pictures rather than reopened, so
  a container with a dozen attachments decodes them through one
  decoder.

  **Censused before it was written: the cover-art reclassification
  never dropped the codec identity.** This crate maps a video-shaped
  stream carrying `AV_DISPOSITION_ATTACHED_PIC` to
  `TrackKind::Attachment`, and that mapping reads
  `AVCodecParameters.codec_id` and puts it on
  `AttachmentTrackParams`, while `TrackExtra` carries a checked deep
  copy of the whole `AVCodecParameters` — width, height, pixel format
  and extradata included. Nothing had to be restored; the road from a
  track row to a decoded picture has no gap in it, and
  `tests/owned_carriers.rs` now pins that.

  Two refusals are named rather than surfacing as a bare FFmpeg
  errno: `ImageDecodeError::EmptyPayload` for an attachment carrying
  no bytes (which the demuxer really does synthesize — a cover-art
  stream that parks no payload, an attachment track with empty
  extradata), and `ImageDecodeError::NoImage` for bytes that were
  accepted and produced no picture.

- **`ImageFrameExtra`** and the `ImageAdapter` impl for `Ffmpeg`. Two
  seats: `orientation` and `side_data`.

- **EXIF orientation takes a typed seat — and the census that put it
  there corrected an earlier reading of libavcodec.** An interim
  version of this entry said orientation arrives in `AVFrame.metadata`
  as a string and therefore could not be given a seat cheaply. It was
  measured instead of reasoned about, and it does not.

  Feeding mjpeg a JPEG whose EXIF IFD carries each orientation in
  turn: for a **recognised** tag (1..=8) the decoder emits an
  `AV_FRAME_DATA_DISPLAYMATRIX` frame side-data entry and puts
  *nothing* in the metadata dictionary. Only an **out-of-range** tag
  (0, 9, …) falls through to the dictionary — as the string
  `"      9"` — and produces no matrix at all, with libavcodec logging
  `unable to attach displaymatrix from EXIF` on the way past. So the
  road was the display matrix all along, this crate was already
  carrying that entry raw, and the seat is a typed *reading* of bytes
  it already had rather than a new FFI mechanism.

  The seat is **`ImageOrientation`**, an eight-valued vocabulary
  (`TopLeft` … `LeftBottom`, EXIF tags 1–8) plus an `Other([i32; 4])`
  escape carrying the linear part of a display matrix none of the
  eight names — reachable, since the matrix is a general affine
  transform and a MOV `tkhd` can hold an arbitrary one. It offers
  `from_display_matrix`, `to_exif_code` / `from_exif_code`,
  `is_mirrored`, `linear`, and `rotation`, which answers in this
  workspace's existing `mediaframe::frame::Rotation`.

  **Why eight values and not that `Rotation` alone.** `Rotation` names
  four, and four of the eight EXIF orientations are *mirrored*; a
  rotation vocabulary cannot hold a reflection. FFmpeg's own API has
  the same gap: `av_display_rotation_get` answers `-180` for both tag
  2 and tag 3, `-90` for both 5 and 6, and `90` for both 7 and 8, so
  reading the angle alone loses the mirror on half the vocabulary.
  `rotation()` reuses `Rotation` for the half it can carry and
  `is_mirrored()` supplies the half it cannot.

  `orientation()` is `None` when the frame carried no display matrix
  — the ordinary case, and also what an out-of-range tag produces —
  which a consumer can tell apart from "the file said upright". That
  is the difference between leaving a picture alone and having decided
  to.

  This flows on the **image road only**. `VideoFrameExtra` is
  untouched: a video's rotation rides the same display-matrix side
  data and is already carried raw there, and typing it for motion
  video is a separate question with separate consumers.

- **An ICC profile deliberately gets no seat**, though it arrives as
  side data too (`AV_FRAME_DATA_ICC_PROFILE`). It is a payload, not a
  fact — kilobytes of colour transform this crate has no vocabulary
  for and no business parsing — and `side_data()` carries it unparsed
  and whole for a consumer that does. That is the line the image
  household draws: a fact a picture cannot be displayed correctly
  without is typed; a payload only a specialist reads rides raw.

- **`ImageFrame` type alias**, beside the eight that were already
  there, and `convert::image_frame_from` /
  `convert::av_frame_to_image_frame`. The image and video paths share
  one plane-extraction rule (`copy_out_planes`) rather than two that
  could drift.

### Added — resource ceilings

- **Every copy across the boundary is now checked against a finite
  budget, before it allocates.** 0.9 made every exit copy, and a copy
  is a decision to allocate whatever an untrusted container asks for: a
  header claiming 100000×100000 pixels, a packet claiming a gigabyte, a
  Matroska with four hundred attached "fonts". Through 0.8 those claims
  cost a refcount. They now cost memory, so they are judged first. The
  new `limits` module is the one home for them — `DEFAULT_*` consts and
  `Copy` options structs with `new` / getters / `with_*` / `set_*`, the
  shape `VideoDecoder::with_max_probe_pending_bytes` and
  `DEFAULT_MAX_PROBE_PENDING_BYTES` already established.

  Every seat is a **finite default**, never an `Option`. There is no
  "unlimited" spelling on purpose: that is the shape a caller reaches
  for once, in a hurry, and never revisits.

  | seat | default | why that number |
  |---|---|---|
  | `FrameLimits::max_pixels` | 256 Mpx | 8K UHD is 33 Mpx and 16K×16K is 268 Mpx; every real frame passes and the forged header does not. FFmpeg's own default for this is `INT_MAX`. |
  | `FrameLimits::max_frame_bytes` | 512 MiB | 8K 4:4:4 16-bit with alpha is ~253 MiB, 8K P010 ~96 MiB. Pixels alone do not bound the copy — depth, plane count and stride padding multiply it. |
  | `PacketLimits::max_packet_bytes` | 1 GiB | Deliberately ceiling-class. `AVPacket.size` is a `c_int`, so 2 GiB is the structural maximum and this halves it; an 8K ProRes 4444 XQ frame is ~10 MB. The job is to refuse a forged `size`, not to second-guess a codec. |
  | `DemuxLimits::max_attachment_bytes` | 64 MiB | An attachment is a whole file. A generous cover is a 4000×4000 PNG at ~20 MB; the largest CJK fonts are ~30 MB. Two orders under the packet ceiling, which is right — attachments are captured *eagerly*, at open. |
  | `DemuxLimits::max_total_attachment_bytes` | 256 MiB | Nothing in the per-attachment ceiling bounds a container that attaches four hundred modest ones. A full ASS font set is ~100 MB at the high end. |

- **Two layers, one number.** `FrameLimits::max_pixels` is also written
  into `AVCodecContext.max_pixels` for every decoder this crate opens —
  HW probe candidates, the SW fallback, the subtitle and image
  decoders. That is the layer that matters most: libavcodec refuses an
  oversized picture *before allocating it*, where checking only on this
  side would mean FFmpeg had already paid for the frame by the time we
  declined to copy it.

- **New typed refusals**, all naming both the ask and the line, all
  raised before the copy: `ConvertError::TooManyPixels`,
  `ConvertError::FrameTooLarge`, `PacketBufferError::PacketTooLarge`,
  `DemuxError::AttachmentTooLarge` and
  `DemuxError::AttachmentBudgetExhausted`. The two attachment arms are
  separate because they are separate attacks — every payload can be
  individually fine and the file can still be an attachment table — and
  the aggregate arm names the track that crossed the line, not a track
  that was individually at fault, because there need not be one.

- **Where the seats sit.** `FrameLimits` is taken by
  `FfmpegVideoStreamDecoder::open`, `FfmpegAudioStreamDecoder::open`,
  `FfmpegSubtitleStreamDecoder::open` and `FfmpegImageDecoder::open`;
  `DemuxLimits` by the new `FfmpegDemuxer::open_with` /
  `open_reader_with` (the existing `open` / `open_reader` delegate with
  defaults). Taken *at open*, not through a `with_*` builder on the
  session, and for a reason in each case: half of `FrameLimits` is
  written into an `AVCodecContext` whose ceiling cannot move after
  `avcodec_open2`, and the attachment half of `DemuxLimits` is *spent*
  during the open — every attachment payload is captured before the
  call returns, which is what makes the demux tier's "exactly one
  packet, before any timed packet" contract true by construction. A
  budget set afterwards would arrive after the spending.

  A file whose attachments exceed either budget **fails to open**.

- **A cover-art bomb is the same attack as a video bomb**, so the image
  road carries the identical ceilings — and it is the more exposed of
  the two, being what a thumbnailer reaches for without ever asking for
  video.

### Fixed — a payload that carries pointers is refused, not copied

- **`AV_PKT_FLAG_TRUSTED` was a use-after-free reachable through safe
  API.** The copy-out leg duplicated a packet's bytes without retaining
  its `AVBufferRef`, and the boundary faithfully preserved every flag
  including `TRUSTED` — which FFmpeg's wrapped-`AVFrame` producers set
  on packets whose body is not media at all but a **structure of
  pointers to other live objects**. Copying that body produced a carrier
  that satisfied every property the amputation contract states — owned,
  `Send + Sync + 'static`, clone-is-a-refcount-bump — and dangled the
  moment the source pipeline dropped. Handing it back through a reverse
  builder, flag restored, gave a wrapped-frame decoder `av_frame_ref` on
  freed memory.

  The copy moved the pointer; it could not move what the pointer named.
  There is no depth of copying that fixes this, because there is no
  bound on what such a payload's pointers reach — so the contract gains
  a corollary, written into the adapter docs: **a payload that carries
  addresses instead of bytes is uncarriable**, and must be refused by
  name on *both* legs. It is refused on all five copy-out conversions
  and all four rebuild roads, as `TrustedPayload`.

  The lane that asserted `TRUSTED` survived a round trip was itself the
  bug, and now asserts the refusal.

### Fixed — the byte ceiling was only enforced after the allocation

- **A compressible frame could make libavcodec allocate ~800 MB under a
  512 MiB ceiling.** `max_pixels` was pushed into the decoder but
  `max_frame_bytes` was not, and a pixel is not a fixed price:
  10000x10000 is 100 Mpx — comfortably under the 256 Mpx default — and
  800 MB in `rgba64`. Such a frame is a few KB on disk, so nothing
  upstream sees it coming, and the byte ceiling only spoke after
  libavcodec had already paid for the frame this crate then declined to
  copy.

  The byte ceiling is now converted into the pixel ceiling that enforces
  it, for the stream's own pixel format, and the tighter of the two is
  what the decoder is opened with. `av_image_check_size2` — reached from
  `ff_set_dimensions`, which every decoder calls when it learns its
  dimensions and *before* it allocates — is the pre-allocation judge, so
  this bites on the declared shape and on a container that lies about it
  alike. The per-pixel cost is measured through this crate's own plane
  geometry on a 256x256 probe rather than from a table; a stream that
  declares no usable format is charged the widest CPU layout FFmpeg has,
  which over-refuses only above roughly 67 Mpx at the default ceilings.

  **On `get_buffer2`:** the standard custom-allocator hook was censused
  and rejected for this release. It is the more precise instrument — it
  sees the real strides — but it needs per-context side state, and
  `AVCodecContext.opaque`, the only user pointer FFmpeg offers, is
  already owned by the hardware path's callback state. Sharing it means
  one state type threaded through the shared context builder and both
  decoder families, with an FFI callback whose lifetime bugs are
  use-after-free rather than test failures. The pixel-ceiling road
  reaches the same allocation, one layer earlier, with no callback and
  no state. Recorded here so the choice is a decision rather than an
  omission.

  The post-conversion `FrameTooLarge` stays as the backstop for frames
  that arrive without a decoder to have pushed anything down.

### Fixed — the callback reads the frame's layout, not the context's

- FFmpeg's `get_buffer2` contract says the callback uses the values on
  the **frame**, and `avcodec_default_get_buffer2` sizes from them. This
  crate priced audio from the *context's* channel layout, which is
  whatever was last negotiated and can differ outright: a context
  claiming mono against a frame carrying 255 `dblp` channels at 130,000
  samples priced about a megabyte and allocated about 265 MB. The count
  is read raw and signed now, and a non-positive one is refused rather
  than floored.

  **The lesson, recorded at the lane.** R15's exhaustive footprint
  matrix could not have caught this and never could: it feeds one
  channel count into both the estimate and the measurement, so a seam
  that reads one side where the allocator reads the other is invisible
  to it. **A matrix proves a formula; only a lane that lets the two
  sides disagree proves the seam that joins them.** The new lane drives
  `judge_buffer` with the context and the frame described
  independently.

### Fixed — the byte ceiling is recovered per medium, and a zero budget refuses

- The callback derived one byte ceiling for both media from
  `max_pixels`, and that was wrong at both edges. A caller setting
  `max_pixels = 1` gave every **audio** frame a 16-byte budget — a pixel
  ceiling has no business bounding audio, and ordinary sound was
  refused. And a `max_frame_bytes` below one worst-case pixel floors the
  pushed-down `max_pixels` to zero, where the guard was written as
  `max_pixels > 0`: the tightest ceiling a caller can ask for **turned
  the judge off**, which is the one direction a ceiling must never fail
  in.

  Audio now recovers from `max_samples`, which `build_codec_context`
  sets from `max_frame_bytes` alone — no `min` with anything else, so it
  is the caller's own number back. Video keeps the `max_pixels`
  recovery, where the conflation is provably harmless in the direction
  that matters: when the byte road was tighter it is exact, and when the
  pixel road was tighter every frame passing the pixel gate has
  `aligned_pixels <= max_pixels` and so costs at most `max_pixels *
  worst` — it can under-state the caller's byte ceiling, never refuse a
  frame the caller allowed. A zero budget refuses every nonempty
  software frame.

### Added — the demux open is bounded before libavformat builds anything

- **The one seat that reaches behind libavformat.** Every other budget
  in this crate measures a copy it makes itself, and on the demux road
  that is always after `avformat_open_input` and
  `avformat_find_stream_info` have already built the attached picture,
  the extradata and the coded side data out of the file. The attachment
  and parameter seats were measuring this crate's copies of allocations
  that had already happened.

  A parser cannot allocate from bytes it was never handed, so the input
  is bounded. `DemuxLimits` gains `max_probe_bytes` (5 MiB, FFmpeg's own
  `probesize` default) and `max_streams` (1000, FFmpeg's own), and they
  reach libavformat two ways:

  | instrument | reaches | bounds |
  |---|---|---|
  | `probesize` / `formatprobesize` | both entrypoints | what the probe and analysis may consume |
  | `max_streams` | both entrypoints | the `AVStream` array a header can conjure |
  | the `AVIOContext` byte meter | the **reader** entrypoint | total bytes handed over, hard — past the budget the reader stops answering |

  Over-budget is `DemuxError::ProbeBudgetExhausted`, and the meter is
  consulted **before** the errno: libavformat folds a reader's I/O error
  into whatever it was doing at the time — usually "invalid data" —
  which would report a refusal this crate made as a malformed file. The
  budget is released once the container is analysed; reading the media
  afterwards is the caller's own business, already bounded by the packet
  seats.

  **The residual, stated rather than implied.** Allocation
  *amplification* inside a parser is not bounded: a container can
  describe, in a handful of bytes, a structure whose in-memory form is
  far larger, and nothing outside libavformat can observe that. Bounding
  the output of that is the substrate's own hardening territory —
  FFmpeg keeps `max_streams`, `max_index_size` and `max_picture_buffer`
  for it, and this crate now sets the first. And the hard meter does not
  reach the **path** entrypoint at all, because it needs an
  `AVIOContext` this crate owns and a path is opened by libavformat's
  own protocol layer; the probe knobs still apply there, and a caller
  who wants the meter on a file opens it as a reader.

### Fixed — a candidate's refusal is read before the candidate dies

- Replay classified the failure through `self.state` — the backend still
  active — while the error being recorded belonged to `candidate_state`,
  whose `get_format` callback is the one that may have declined. The
  right state was then dropped, so the reason was gone: the attempt log
  recorded FFmpeg's `Invalid data found when processing input` for a
  coded surface this crate refused. The candidate's declination is read
  first now, and the ordering is the whole fix.

### Fixed — the allocator callback prices bytes, for pictures and for audio

- **The software video road still compared pixels.** `judge_buffer`
  bounded the aligned *extent* against `max_pixels` and then delegated —
  but an extent costs whatever its format costs. A 65536x1 `gbrap32le`
  frame is 65,536 pixels, inside a 1 MiB ceiling by that ruler, and
  33.5 MB once allocated: thirty-two times over.

- **Audio passed it unpriced entirely.** `max_samples` bounds the sample
  *count*, so one sample across eight packed `f64` channels is 64 valid
  bytes under a 64-byte ceiling and a 2,080-byte allocation — and it was
  delivered, because the copy-out only ever rechecks the valid bytes.

  Both are now priced through `crate::footprint` before the delegation,
  and an unpriceable format fails closed. The pixel check stays as the
  coarse first gate.

  **The seat, without new state.** The callback can see `max_pixels`,
  and `build_codec_context` sets it to
  `min(caller's pixel ceiling, max_frame_bytes / worst-bytes-per-pixel)`
  — so `max_pixels * worst` is never above `max_frame_bytes`: equal when
  the byte road was the tighter of the two, below it otherwise.
  Recovering the byte ceiling that way is conservative in the safe
  direction and threads nothing through a C callback. Hardware frames
  are delegated rather than failed closed: their pool is judged where it
  is declared, in `get_format`.

### Fixed — the packed-audio formula had the allocator's steps in the wrong order

- **It aligned the channel-multiplied payload once; the allocator rounds
  the sample extent and *then* multiplies by channels.** For packed
  `dbl` at eight channels that priced 576 bytes against a real 2,080; at
  255 channels, 2,560 against 65,312. Twenty-five times under, from a
  formula that looked right.

  The arithmetic is no longer restated. `av_samples_get_buffer_size`
  with `align = 0` **is** the ruler `av_frame_get_buffer` measures with,
  so the pricer asks it through a new `c_int` shim. Measured across
  every sample format this build names, the relation is exact:
  `allocated = av_samples_get_buffer_size(..., 0) + 32 * planes`, with
  planes being the channel count for planar layouts and one for packed.
  The per-plane term is charged at 512 rather than the measured 32.

- **The verification law is now a matrix, and that is the real fix.**
  The R14 lanes were a *curated list* — the shapes the findings happened
  to name — and they passed while the formula was twenty-five times
  under: they priced `s16` at eight channels (dominated) and never asked
  about `dbl` at eight channels (not). The bug lived in the order of the
  arithmetic, so it only appeared once the per-sample width grew, and no
  curated row grew it.

  The sweeps are exhaustive now: every sample format the build names
  crossed with channel counts {1, 2, 8, 32, 255} and sample counts
  {1, 1024, 65535}, and every CPU pixel format crossed with degenerate,
  odd and ordinary shapes — over 950 cells, each compared against a real
  `av_frame_get_buffer` allocation. Reinstating the old formula fails
  the sweep immediately, at `u8` with 32 channels: a cell no curated row
  had. **A curated row proves the case it names and nothing else.**

### Fixed — one funnel for every hardware exit

- **The declination had one consumer in production, not four.** R14
  reported an exits map covering the post-commit classifier, the
  `open_as` failure path and the codec-type mismatch alongside
  `advance_probe`. Only `advance_probe` was wired: the other three were
  written and then lost when the surrounding code was restructured, and
  every gate passed — because the tests exercised the *helper*, which
  was correct, rather than the *roads*, which were not.

  Every exit now goes through one `hw_exit` funnel that reads the
  declination before anything wraps or tears down state, and the
  open-time paths call its free-standing half before the guard releases
  the callback state. Five consumers, and each reachable road has a lane
  that drives it end to end.

  This also closes the quietest of the failure modes: the
  explicit-backend road took the EOF exit, which `is_hw_decode_failure`
  deliberately excludes so a genuinely drained stream is not trapped in
  a fallback loop — so a declined format reported as "stream over", with
  nothing said at all. The funnel tells the two apart, and the new lane
  fails if it stops doing so.

### Fixed — judges now price the allocator, not the payload

The rule the whole series arrives at, now written at the head of both
accounting tables: **a judge must dominate the allocator's arithmetic,
not the payload's.** Two ceilings were comparing what the bytes weigh
against what would be spent, and the gap is not small on the shapes that
matter:

| shape | tight payload | `av_frame_get_buffer(0)` |
|---|---|---|
| `nv12` 16x16 | 384 | **1,792** |
| `yuv420p` 1x1 | 3 | **2,304** |
| `gray8` 65536x1 | 65,536 | **2,097,408** |
| `s16p`, 1 sample, 8ch | 16 | **768** |
| `dblp`, 1 sample, 255ch | 2,040 | **73,440** |
| `yuv420p` 1920x1080 | 3,110,400 | 3,133,696 |

That last row is why this survived so long: on an ordinary frame the
allocator's overhead is under one percent, so every under-pricing defect
hid behind a shape big enough for the slack not to show.

- **The hardware transfer judge** priced `av_image_get_buffer_size` at a
  fixed alignment — 768 bytes for a 16x16 NV12 destination that really
  costs 1,792.
- **The resampler's output preflight** multiplied the tight plane length
  by the plane count, so a 16-byte ceiling admitted a 768-byte
  allocation for one sample across eight planar channels.

Both now go through one shared pricer, `crate::footprint`, modelling
`av_frame_get_buffer` for video *and* audio: both dimensions aligned up
(the allocator aligns the linesize *and* the plane heights, which is
what turns a 65536x1 frame into 2 MiB), per-plane slack for the padding
term, and planar audio charged per plane.

**The estimates are verified, not argued.** `libavutil/frame.c` aligns
width in a loop, aligns plane heights and adds a padding term whose
constants differ by build and CPU; transcribing that is a fragile way to
be exactly right. So the pricer is deliberately a conservative upper
bound and the tests price every shape in the table above against the
real summed `AVBufferRef.size`, asserting the estimate dominates each
one. A build whose allocator grows hungrier fails those tests instead of
quietly outgrowing a ceiling.

### Fixed — the hardware pool is asked, not calculated

- **The coded-surface judge used the codec's alignment; a hardware pool
  aligns by its own API's rules.** `avcodec_align_dimensions2` is what
  the *codec* wants, and D3D11 HEVC and AV1 round both dimensions to
  128 — a 129x129 stream priced 144x160 by codec arithmetic can be
  allocated as 256x256.

  The judge now calls `avcodec_get_hw_frames_parameters` to obtain the
  `AVHWFramesContext` the decoder is about to initialise — populated and
  not yet allocated — and reads the pool's own declared extent, unrefing
  it before returning. Measured on the VideoToolbox road: the pool
  declares 1920x1088 for the cropped fixture, where codec arithmetic
  said 1920x1090. The lane asserts the pool's figure *and* that it is
  not the codec's, so a lost ask is caught rather than approximated.

  One trap found by measuring: the query must be asked about the
  **format being negotiated**, not `ctx->pix_fmt`, which at
  `get_format` time still holds the software format and answers
  `ENOENT` — silently falling back to the arithmetic the call exists to
  replace. The codec-alignment path remains as the recorded fallback for
  accelerators that will not declare.

### Fixed — the coded-surface reason survived only the probe road

- `take_ceiling_declination` ran in `advance_probe` and nowhere else, so
  a decoder opened for one explicit backend — which has no probe —
  classified the refusal as a post-commit hardware failure carrying
  FFmpeg's `Invalid data found when processing input`: the callback's
  own declination reported as though the file were corrupt. `open_as`
  also frees the callback state on its way out, so the open-time path
  lost the reason entirely.

  The reader is free-standing now and consulted at every hardware
  classification exit — the `open_as` failure mapping, the codec-type
  mismatch, and the post-commit classifier — reading the reason before
  the state is released. It clears on read, so a refusal cannot be
  reported a second time against a backend that never declined
  anything.

### Fixed — send-side side data had no ceiling at all

- **The read side has capped side data at 64 entries and 256 KiB since
  it was written; the send side capped nothing.** The preflight looked
  at `body.len()` and then `attach_side_data` allocated every entry the
  caller handed over, one `av_packet_new_side_data` at a time, with
  nothing counting or weighing them — so annotations the demux boundary
  would have refused coming *out* of a container went straight into
  libavcodec going *in*. The same asymmetry `PacketLimits` was
  introduced to close for the packet body, one field along.

  The whole list is now judged before anything is allocated — including
  the body, because a list that cannot be carried should not cost a
  packet first — against the read side's own caps, one seat in both
  directions. Over-count and over-bytes are distinguished by name
  (`SendSideDataTooLarge`), so a caller can tell "too many annotations"
  from "too much annotation".

### Fixed — two dimension vocabularies, and judges reading the wrong one

- **`max_pixels` bounds the display extent; the allocation is the coded
  one.** `ff_set_dimensions` applies `av_image_check_size2` to the dims
  the decoder passes it, and for a cropped stream those are the
  *display* dims. Minted and measured: an h264 clip with SPS cropping of
  32x32 out of a 1920x1088 macroblock grid — a 2040x divergence —
  is admitted by `max_pixels = 5000`, because 32x32 is 1024 pixels.

  On the software road that gap was already closed, and this was
  measured rather than assumed: `get_buffer2` receives the frame at
  **coded** dims (1920x1088, aligned to 1920x1090, a 2,092,831-byte
  allocation), so `judge_buffer` bounds the real extent. The hardware
  road never reaches `get_buffer2` at all, which left the coded surface
  unjudged there.

  The `get_format` callback this crate already owns is the seat: it is
  the last moment before the hardware pool is built and the first at
  which the coded extent is known. It now applies the same `max_pixels`
  the context carries to the aligned coded dims.

  **The refusal mode, censused rather than chosen.** A `get_format`
  callback cannot return a reason — declining means `AV_PIX_FMT_NONE`,
  which libavcodec reports as `Invalid data found when processing
  input`: true about what it saw, false about what happened, since the
  data was fine and this crate declined it. So the reason is left in the
  callback's own state (already owned, already in `opaque`) and the
  probe funnel turns it back into `Error::HwSurfaceTooLarge` before
  recording the attempt. The caller gets the aligned coded pixel count
  and the ceiling, not FFmpeg's misnomer.

- **`judge_hw_transfer` priced the display extent too.** What
  `av_hwframe_transfer_data` allocates is sized from the frames context,
  not from `AVFrame.width`. This crate already had
  `hw_frames_ctx_dimensions`, whose doc comment names this exact trap
  and whose own test uses an 8192-pool-over-100-display fixture — and
  the first cut of this judge reached past it for the display dims
  anyway. It now prices every transfer candidate at the pool dims and
  takes the worst.

  It also **fails closed**: a hardware frame whose pool extent cannot be
  read is charged as unbounded, because an unprovable extent is not a
  small one. A frame that is not a hardware frame at all is passed
  through instead — `av_hwframe_transfer_data` refuses such a source
  and allocates nothing, so answering "too large" there would put a
  ceiling's name on a different fault.

- **The sweep.** Every site reading a dimension is now tabulated in the
  `convert` module docs with the vocabulary it needs. Two more reads in
  `drain_into_pending` turned out to be log fields, sizing nothing;
  `copy_out_planes` and the public `width`/`height` accessors are
  correct as they stand. The rule the table states: *the extent to judge
  is the one the allocator will use, and it is read from whatever
  structure the allocation is sized from, never assumed.*

### Fixed — a side-data allocation failure no longer publishes a partial frame

- **OOM mid-copy was absorbed.** `av_frame_new_side_data` returning null
  logged a warning, stopped copying, and returned `Ok(())` — publishing
  a frame carrying whatever side data happened to fit before the
  allocator gave out. What sits in that list is the HDR mastering
  metadata, the ICC profile and the display matrix: a picture that comes
  back with its colours or its orientation quietly missing is worse than
  one that does not come back, because nothing downstream can tell.

  The failure is returned now. The caller already knew what to do with
  it — it unrefs the partial destination and either advances to the next
  backend or surfaces the failure for a software retry — so this is a
  `break` becoming a `return`, against machinery that was already in
  place. Covered by fault injection through the crate's existing
  `av_max_alloc` subprocess harness, which forces the null for real
  rather than testing the propagation shape by hand.

### Fixed — the hardware road gets the seat the allocator hook cannot reach

- **`judge_buffer` is not a universal choke point.** `ff_get_buffer`
  calls `hwaccel->alloc_frame` directly and never reaches `get_buffer2`
  at all, and `av_hwframe_transfer_data` allocates its CPU destination
  outside both hooks. Censused on this machine: VideoToolbox is the only
  hardware config h264, hevc and vp9 advertise, the device opens, and a
  VideoToolbox h264 decode of a 160x120 clip records **zero**
  `get_buffer2` calls while producing a hardware frame.

  **What the census settled about the surface itself:** `max_pixels`
  *does* bite before `alloc_frame`, measured rather than assumed. With
  `max_pixels = 100` the same decode fails at `avcodec_open2` with
  `Picture size 160x120 exceeds specified max pixel count 100` from
  `av_image_check_size2` — zero `get_buffer2` calls, no frame. The check
  is in `ff_set_dimensions`, which every decoder runs when it learns its
  dimensions and before any surface pool exists, so the seat
  `max_pixels` already occupies covers the hardware surface too. The
  residual there is the aligned-dimensions gap, and it applies to
  **driver-owned GPU memory** rather than to anything this crate
  carries.

  **What this crate does carry off that road is the CPU frame**, and it
  now has an exact judge: `judge_hw_transfer` runs before *every*
  `av_hwframe_transfer_data` call. The destination format is not this
  crate's to choose — `dst.format` is `AV_PIX_FMT_NONE` on entry and
  FFmpeg picks from `av_hwframe_transfer_get_formats` — so the whole
  candidate list is priced at the transfer's own alignment and the worst
  taken, walked as `*const c_int` through a new shim because a driver
  may offer a format this build does not name. Over-ceiling is
  `Error::HwTransferTooLarge`.

  It is judged at the call site rather than inside `transfer_hw_frame`
  on purpose: errors from that function are FFmpeg's, and the arms
  around it reclassify them into "the hardware failed, fall back to
  software". A byte ceiling is not a hardware failure — software would
  decode the same oversized frame and be refused again — so the named
  refusal returns straight to the caller instead. On the probe-replay
  road the same judge reports through that path's existing error
  channel, where every failure is collapsed into "try the next
  backend"; the reason is logged rather than named, because a name
  there has no consumer.

  This is also what closes the degenerate-shape residual on the
  hardware road. The pixel ruler bounds `w * h * 16`, but a transfer
  pads every row: a one-pixel-wide frame of N rows costs 64N bytes for
  N pixels in NV12 at 32-byte alignment, which is four times the ceiling
  the pixel ruler would have allowed. The shape is not producible with
  any encoder available here — h264 and hevc will not encode a
  one-pixel-wide stream — so the guard is stated and reasoned rather
  than driven by a lane. What *is* pinned is its accepting path: a real
  decode through `VideoDecoder`'s auto-probe still delivers frames.

### Fixed — the sample ceiling counted channels twice

- **`max_samples` was over-divided, and it refused valid media.**
  FFmpeg compares `nb_samples * ch_layout.nb_channels` — the
  channel-sample count — against the field, which is already the
  per-channel accounting the ruler wanted. Dividing by the channel count
  as well charged it twice: at the 512 MiB default the ceiling was
  263,172 channel-samples instead of 67,108,864, and a **valid**
  6-channel FLAC block of 65,535 samples (393,210 channel-samples, about
  1.5 MiB of `s32`) was refused as a bomb.

  What made this easy to get wrong is that FFmpeg's own log message
  prints only `nb_samples` — "samples per frame 4096, exceeds
  max_samples 10000" — while having compared 4096 x 6 = 24576 against
  10000. Measured on this build to the unit: a 6-channel 4096-sample
  frame is accepted at exactly 24576 and refused at 24575.

  The ruler is now `max_frame_bytes / worst_bytes_per_sample`, and
  FFmpeg supplies the channel count itself.

  **The lesson, recorded in the test rather than only in prose.** The
  pin that was supposed to catch this asserted the same arithmetic the
  production code used, so when that arithmetic was wrong the pin agreed
  with it — a pin that restates the implementation tests nothing. It now
  pins the *consumer's* semantics: the exact channel-sample boundary,
  driven through a real 6-channel decode at 24576 and at 24575, plus the
  6-channel FLAC block that the old ruler refused.

### Fixed — the enum discipline, re-applied to the code that enforces it

- **The pixel-format census was undefined behaviour on its own reason
  for existing.** It walks this build's descriptor table to price the
  most expensive format libavcodec can emit — the point being to be
  correct about formats the bindings may not name — and the generated
  `av_pix_fmt_desc_get_id` hands those ids back as a closed
  `AVPixelFormat`. Every future format would have become an invalid Rust
  enum value on the way *into* the pricing meant to handle it.

  Local ABI-compatible shims now declare `av_pix_fmt_desc_get_id`,
  `av_image_get_buffer_size` and `av_get_bytes_per_sample` with `c_int`,
  joining `avcodec_find_decoder`. Both Rust declarations resolve to the
  same C symbol; an unknown id never becomes an enum on the Rust side.

  The sweep table in the `convert` module docs gains these rows, marked
  as what they are: the open-C-enum class **inside this crate's own new
  code**. Writing the discipline down was not enough — it had to be
  re-applied to the code that enforces it.

### Fixed — the ceiling now follows the allocator, not the header

- **Dimension alignment walked straight past the pixel ceiling.**
  `max_pixels` is checked against the frame's *raw* `width * height`;
  what libavcodec then allocates is the shape
  `avcodec_align_dimensions2` rounds that up to. For degenerate aspect
  ratios those are not the same number. Measured on this build:

  | shape | raw | aligned | inflation |
  |---|---|---|---|
  | `gray8` 65536x1 | 65,536 px / 64 KiB | 65536x32 = 2,097,152 px / 2 MiB | **32x** |
  | `gray8` 1x65536 | 65,536 px / 64 KiB | 16x65536 = 1,048,576 px / 2 MiB | 16x |
  | `yuv420p` 7680x4320 | 33,177,600 px | unchanged | 1.00x |
  | `gray8` 1024x1024 | 1,048,576 px | unchanged | 1.00x |

  So a one-pixel-tall frame slipped 32 times its declared cost past a
  1 MiB ceiling — and no value of that scalar could have fixed it,
  because bounding a product cannot bound a product whose factors are
  then rounded up independently. Real pictures inflate by nothing at
  all, which is exactly why the ceiling looked sound.

  **The road, chosen by measurement.** `get_format` was censused first,
  being the smaller instrument: it receives the context, needs no
  allocation decision, and could apply the same scalar to aligned
  dimensions. It **does not fire on every road** — on this build a
  one-shot `mjpeg` decode calls it once and a `png` decode calls it
  *zero* times, and cover art is overwhelmingly one or the other, so
  half the guarded road would have been unguarded. `get_buffer2` fired
  on both, sees the frame's real format rather than a negotiated
  candidate, and is the allocator itself.

  **The `opaque` conflict was dissolved rather than solved.** No state
  is needed: the callback reads the ceiling from
  `AVCodecContext.max_pixels`, the field this crate set itself, and
  applies it to the aligned dimensions. Same scalar, same meaning,
  applied where alignment is knowable. `opaque` is untouched, so the
  hardware path keeps it, and there is no allocation whose lifetime has
  to outlive a C callback. Panic discipline is structural rather than
  asserted: the body allocates nothing, indexes nothing, unwraps
  nothing, and calls three FFmpeg functions — and an `extern "C"` fn
  aborts rather than unwinding into C regardless.

- **Audio had no pre-allocation guard at all.**
  `AVCodecContext.max_samples` — FFmpeg's audio `max_pixels`, compared
  against a frame's `nb_samples` in `ff_get_buffer` before the planes
  are allocated — was left at `INT64_MAX`, so the audio family leaned
  entirely on this crate's post-decode byte check: refusing after
  libavcodec had already paid.

  The ruler is `max_frame_bytes` divided by the worst cost of one
  sample: the widest sample format this build can emit (`s64`/`dbl` and
  their planar twins at 8 bytes — censused through the new shim, not
  assumed) times the most channels this crate will carry (255, where
  the conversion refuses by name). At the 512 MiB default that is
  **263,172 samples per frame**, which clears everything real by a wide
  margin — FLAC's largest block is 65,535, Opus tops out at 5,760, AAC
  at 2,048, and a full second of 192 kHz audio would be 192,000.

  Planar alignment padding sits outside the ruler (at most
  `align x channels`, about 8 KiB at 255 channels) and is left to the
  post-decode check, which stays as the backstop.

  Both rulers are pinned by a unit test, because both are censuses of
  the build rather than constants: a ceiling whose documented
  derivation no longer matches its arithmetic is worse than one with no
  documentation at all.

### Fixed — the pre-allocation ceiling stops negotiating with the file

- **The byte-derived pixel ceiling was priced from the container's
  declared format, and a declaration is not an upper bound.** It may be
  unset, it may be wrong, and it may be narrower than what the decoder
  actually emits — a stream declaring `yuv420p` at 1.5 bytes per pixel
  whose decoder outputs `rgbaf32` at 16 got a ceiling more than ten
  times too generous. The undeclared-format fallback charged 8 bytes per
  pixel while this crate delivers 16-byte layouts, so 512 MiB admitted
  ~67 Mpx, which at 16 bytes is ~1 GiB inside libavcodec before the
  post-decode backstop said anything. The hole was one layer below the
  one the ceiling had just been added to close.

  The rate is no longer negotiated with the file at all: **every stream
  is charged the worst per-pixel cost any format this build can emit**,
  and that worst case is measured rather than tabulated. The descriptor
  table is walked once per process and each format priced through
  `av_image_get_buffer_size` — the same function
  `avcodec_default_get_buffer2` sizes from — so a future FFmpeg that
  adds a wider format is priced correctly without this crate learning
  its name.

  **The census:** 267 descriptors, 251 of them CPU formats that price
  (the rest are hardware surfaces carrying no CPU bytes). The maximum is
  **16.000 bytes per pixel**, reached by eight formats — `gbrapf32be/le`,
  `rgbaf32be/le`, `rgba128be/le`, `gbrap32be/le` — with the 12-byte
  `gbrpf32`/`rgbf32` family next below. Measured on a 256x256 probe,
  which divides every chroma subsampling and every alignment libavcodec
  uses, so the figure is the rate and not a rounding artefact; the same
  census at 257 reads 16.934, and that 5.8% is per-*row* padding, a term
  linear in height rather than in pixels.

  **The trade, stated:** over-refusal for cheap formats. At the 512 MiB
  default the effective ceiling is ~33.55 Mpx, so 8K (33.18 Mpx) still
  decodes in *any* format — including the 16-byte ones, where it really
  does cost 506 MiB — but a 16K `yuv420p` frame that would only have
  cost 199 MB is refused with it. That is the honest shape of a bound
  that has to hold before the format is known, and the deployment answer
  is `max_frame_bytes`, which is exactly the knob for how much memory
  one frame may cost. A compile-time assertion now pins 8K-at-16-bytes
  under the default, so lowering the default or meeting a wider format
  fails the build rather than quietly refusing 8K at run time.

  **`get_buffer2` was reconsidered and again not taken.** The blocking
  problem is unchanged and was not solved: it needs per-context side
  state, `AVCodecContext.opaque` is already owned by the hardware path's
  callback state, and composing the two means one state struct threaded
  through the shared context builder and both decoder families behind an
  FFI callback whose lifetime and panic discipline would have to be
  proved rather than asserted. The measured worst-case rate reaches the
  same allocation, one layer earlier, with no callback and no state.

### Fixed — a still bought its planes before judging its annotations

- **The side-data budget ran after `copy_out_planes`.** The refusal was
  correct and it arrived after the expensive half of the work: an
  over-budget still had already paid for up to `max_frame_bytes` of
  plane copies before its annotations were so much as totalled.

  The measuring pass — which reads only the entry count and each entry's
  declared `size`, dereferences no payload and allocates nothing — is
  hoisted to run with the other free judgements, before a single plane
  is bought. The copying pass re-runs it: repeating two comparisons that
  guard an allocation is defence in depth, and it keeps that function
  correct on its own terms rather than only in the order it happens to
  be called in.

  The rule this settles, now in the `convert` module docs: **everything
  a conversion can refuse is refused before anything it can allocate is
  allocated.**

### Fixed — the open-C-enum discipline extends to the dependency's API

- **The image decoder opened through `Decoder::video()`.** That resolves
  the codec by reading `AVCodecParameters.codec_id` as the bindgen
  `AVCodecID` and then reads `AVCodecContext.codec_type` as
  `AVMediaType`. Both are open C enums FFmpeg extends in
  ABI-compatible releases, and forming a Rust enum from a value outside
  this build's discriminant set is undefined behaviour before any
  comparison can run. The hardware path has bypassed that API since it
  was written — the image road, which is the one a *file* chooses the
  codec id on, was still going through it.

  The sweep that followed found the same call on three more roads (the
  software video, audio and subtitle decoders) and
  `Parameters::medium()` on four (track building, attachment
  classification, the resampler's spec check, and a `Debug` impl). All
  are closed: the codec is resolved off a raw `u32`, the medium is
  proved off a raw `i32` through a shared `ensure_codec_type`, and
  `medium()` is replaced by a total fold into a `MediaKind` this crate
  owns.

  The `medium()` calls had a recorded defence — `AVMediaType`'s set is
  tiny and stable — and it was true enough to have never bitten. That
  is a reason it was unlikely, not a reason it was sound, so the
  exception is gone rather than re-argued. No attacker-reachable path in
  this crate now forms a bindgen enum out of FFmpeg memory.

### Fixed — three numbers judged after they were spent

- **The corruption gate read the loud field and not the quiet one.**
  `AV_FRAME_FLAG_CORRUPT` is set rarely; `AVFrame.decode_error_flags` is
  what h264 actually writes for a frame it concealed its way through,
  with the frame flag left clear. The gate now treats any nonzero value
  as corruption — not an enumeration of the members this build names,
  which would re-open the door on the next release.

- **Crop arithmetic was unchecked and accepted a zero-extent rect.**
  The crops are `size_t`, so `left + right` is a real overflow: a panic
  in debug, a wrap in release, and a wrapped sum passes the extent test
  and narrows into a rect pointing outside the picture. Each pair is
  `checked_add`ed now, sums are refused at `>=` the extent as FFmpeg's
  own `av_frame_apply_cropping` does, and the narrowing happens only
  after both proofs.

- **An undersized stride was consumed as if it were a padded one.** A
  `linesize` *below* the format's row width fell into the branch for
  strides that are larger, so the ceiling was charged for bytes the
  plane did not have and the real refusal was left to the copy loop —
  by which time every earlier plane had been allocated and copied and
  thrown away, and the error reported was whichever ceiling the inflated
  total tripped rather than the layout fault underneath it.

  The pass structure changed with the fix: the byte ceiling is judged
  from the **geometry alone**, which no number the frame chose can
  influence, so it runs first and reads nothing; then every stride is
  judged in its own pass; only then does the copy loop allocate. The
  copy loop keeps its own form of the check, one comparison guarding a
  `from_raw_parts`.

### Fixed — a still's ICC profile no longer takes its orientation with it

- **The shared 256 KiB side-data cap silently swallowed legitimate
  profiles.** On a video stream that cap is defensible: side data there
  is small, repeated and per-frame. On a decoded still it was wrong
  twice. A still *is* its annotations — the ICC profile that decides its
  colours and the display matrix that decides its orientation both live
  there, both carried by exactly one frame — and the parameter road next
  door already admits ICC profiles up to 16 MiB, so the same profile was
  accepted as a track parameter and dropped as a frame annotation. The
  drop was also positional: entries past the cap were skipped, so a
  large profile pushed the display matrix off the end and the picture
  came back silently rotated wrong.

  The still road gets its own configurable seat,
  `FrameLimits::max_image_side_data_bytes`, defaulting to the same 16
  MiB as `DEFAULT_MAX_CODEC_PARAMETER_BYTES` — with a compile-time
  assertion that the two stay aligned, because a ceiling that admits a
  profile on one road and drops it on the other is not a policy but an
  accident of which road the file took. Over-budget is a named refusal
  (`ImageSideDataTooLarge`, `ImageSideDataEntries`), judged whole before
  anything is copied, never a truncated list. The shared stream road
  keeps its caps and its behaviour.

### Fixed — a validator that runs after its own field's first consumer

- **The channel ceiling was reading a laundered number.** The refusal
  added for 256-channel packed audio took its count off the materialised
  `ChannelLayoutDescription` — which stores `nb_channels.max(0)`. So the
  guard against a bad count was reading a value the layout helper had
  already floored: a declared `-1` arrived as a legitimate-looking `0`
  and, on a zero-sample frame, produced a frame instead of a refusal.

  Worse in the other direction, materialising *runs first*. For an
  `AV_CHANNEL_ORDER_CUSTOM` layout that means rendering the layout name
  through FFmpeg and walking `nb_channels` map entries into a
  `Vec::with_capacity(nb_channels)` — work proportional to the number
  the ceiling exists to bound, performed before the bound is applied.

  The count is now read straight off `AVChannelLayout.nb_channels` as
  the signed integer it is, every refusal is stated against that raw
  value, and only a count already proved to be in `0..=255` is allowed
  to drive the description. `UnsupportedChannelCount` carries an `i32`
  accordingly.

- **The same inversion on the picture roads, found by auditing for it
  rather than by being told.** `width` and `height` were floored with
  `.max(0)` before anything judged them, so a declared `-1` became `0`
  — and zero pixels is under every ceiling, so a `VideoFrame` (or
  `ImageFrame`) of zero extent was built and returned. Negative
  dimensions are now refused by name as `InvalidDimensions`, on both
  roads. Zero is deliberately not refused: it is what an unset dimension
  reads as, and inventing a refusal for it would be policy this audit
  has no evidence for.

  Two more on the same sweep: the byte-ceiling pass consumed
  `linesize[i]` with a `.max(0)` while the copy loop below did the
  judging, and `crop_*` — a `size_t` — was truncated with `as u32`, so a
  crop of `2^32 + 5` became a perfectly plausible `5` and produced a
  visible rect that was wrong in a way nothing could see. The stride
  refusal is hoisted into the pass that first reads it; the crops are
  kept in `u64` and checked for coherence against the frame's own
  extent, and an incoherent one now withholds the annotation instead of
  fabricating a zero-extent rect.

  The whole census — every raw header field on the audio, video and
  image paths, with its validator and its first consumer — is recorded
  as a table in the `convert` module docs, because the answer to "is
  this ordered correctly" should not have to be re-derived by reading.

### Fixed — the image seam stopped dropping packet flags

- **Every attachment reached libavcodec with a zeroed `flags` field.**
  The one-shot image road rebuilt its `AVPacket` from the payload bytes
  alone, so everything the portable packet said about itself stopped at
  the seam. The three stream families have always rebuilt through
  `write_md_flags`; the image road now does too, rather than growing a
  second copy of it.

  `AV_PKT_FLAG_DISCARD` is the flag this recovers: measured on this
  build, libavcodec genuinely obeys it and produces no frame, so
  forwarding it is both necessary and sufficient. Before this, a packet
  the caller had marked as one to drop was decoded anyway.

- **`CORRUPT` is refused at the seam, and the census is why.** Measured
  with a real cover-art payload: the flag reaches libavcodec and
  libavcodec does nothing with it — mjpeg returns a full picture and
  leaves `AV_FRAME_FLAG_CORRUPT` clear. Forwarding it would therefore
  not preserve the fact, only move where it is dropped, and `ImageFrame`
  has no flag seat to land it in.

  So a payload marked corrupt is refused by name (`ImageDecodeError::Corrupt`)
  rather than decoded into a picture whose only warning has been deleted
  en route. The sibling road is closed with it: a decoder that hands
  back a picture and marks the *frame* corrupt is refused the same way,
  because that fact has exactly as little room to live in an
  `ImageFrame`. A caller who wants the bytes decoded regardless can
  clear the flag it set.

### Fixed — the audio header is judged, never clipped

- **A malformed audio header could arrive as a well-formed frame.** Two
  numbers on the decode path were floored instead of refused, and both
  turned a bad frame into a plausible one — which is worse than an
  error, because nothing downstream has a reason to doubt it.

  `nb_samples` was clamped with `.max(0)`, so a negative count became
  the *empty frame* shape: a refusal delivered as a successful decode of
  nothing. And the sample format was only checked inside the non-zero
  arm, so a zero-sample frame with `AV_SAMPLE_FMT_NONE` — the state a
  codec context is in before its decoder opens — or an unknown format
  came back as an `AudioFrame` advertising a format nothing can
  interpret. A frame carrying no samples still has to say what it is.

  Every header field is now judged before a byte of geometry is
  computed: `InvalidSampleCount` names a negative count,
  `UnsupportedSampleFormat` names a format with no byte width, and the
  zero-sample shortcut is taken only for a count that is *exactly*
  zero. `linesize[0]` is refused when negative at any sample count
  rather than floored.

- **Packed audio above 255 channels was silently truncated.** The
  channel count was clipped to `u8::MAX`, and only *planar* layouts
  refused the excess — via the plane ceiling, which packed audio never
  reaches, because packed declares one plane at any channel count. So a
  256-channel one-sample S16 frame computed its byte product from 255,
  copied 510 of its 512 valid bytes, and handed back a frame advertising
  a channel count the file never declared. Two lies, neither reported.

  `UnsupportedChannelCount` now refuses the count before any plane
  geometry exists, and the packed byte product uses the declared count
  with no substituted `1`. A frame claiming samples across zero channels
  is refused as incoherent rather than treated as mono.

- **The same clip on the resampler, closed at the same choke point.**
  `check_spec` refused planar specs past eight planes but let a packed
  256-channel spec through, and every output frame then took
  `.clamp(0, 255)` on the way into its channel seat. It is refused at
  construction now, for both ends, as `ResampleError::UnsupportedChannelCount`
  — construction being the only place a *target* refusal can be acted
  on, since the later one arrives after `swr` has consumed the input.

  With the ceiling stated once, the three defensive floors it hid behind
  (`channels.max(0)` twice, `channels.max(1)` in the byte product) are
  gone: `plane_bytes` refuses a non-positive count through the `Option`
  it already returns instead of inventing a channel.

  The audio path now carries **no lossy numeric clamp**. The one
  surviving floor, `sample_rate.max(0)`, was censused and kept with the
  reason recorded at the site: it feeds no geometry, no allocation and
  no copy length, and zero is already this crate's "rate unspecified".

### Fixed — a zero-sample audio frame is a frame

- **An empty audio frame was refused as a layout error.** FFmpeg's
  canonical zero-sample frame carries a format, a layout and a rate with
  `data[i] == NULL`, `linesize == 0` and no `AVBufferRef` — nothing is
  allocated because there is nothing to hold. The plane loop still
  demanded every pointer be non-null and buffer-backed, so a header
  frame mid-stream came back as `InvalidPlaneLayout` and interrupted a
  decode that was going fine.

  The loop now does not run when there are no valid bytes. The declared
  layout is still reported — `plane_count` stays packed's 1 or planar's
  channel count — and those slots hold the shared empty carrier at
  stride 0, so a consumer sees the shape it expects carrying no samples,
  which is what the frame says. No allocation: the empty carrier is one
  refcount bump, and six declared planes share it.

  The non-zero path is untouched. The whole change removes one line —
  the loop header — and the body is byte-identical.

### Fixed — valid bytes, the send leg, and two exact caps

- **An audio plane exported its alignment padding.** The copy took
  `linesize[0]`, which is what `av_samples_get_buffer_size` *allocated*
  — rounded up for alignment — rather than
  `nb_samples * bytes_per_sample` (times the channel count when
  packed), which is what the decoder actually wrote. That formed a
  `&[u8]` over bytes nothing initialises, which is undefined behaviour
  before anything reads them, and handed that stale heap to a consumer
  inside a safe `FfmpegBytes`. `linesize` is now used for exactly one
  thing: proving the source allocation is as large as it claims. What
  is copied is the valid product, and the plane's stride agrees with
  it.

  **Class sweep of every exit**, and the distinction that matters:

  | exit | exports | verdict |
  |---|---|---|
  | audio planes | was `linesize`, now the valid sample product | **fixed** |
  | video/image planes, tight stride | `plane_h × linesize` where `linesize == row_bytes` | correct — no padding exists by definition of the branch |
  | video/image planes, padded stride | `row_bytes × plane_h`, copied row-wise | correct — the per-row padding is stepped over, never read |
  | subtitle bitmap | `linesize[0] × h` from the rect | correct — FFmpeg's documented valid extent for `AVSubtitleRect` |
  | palette | a fixed `AVPALETTE_SIZE` | correct — the format's own size |
  | packet payloads | `AVPacket.size` | correct — the declared payload, not the buffer |
  | resampler output | `per_sample × produced` | correct, and always was — the decode path now agrees with it |

  Video's stride-including rows are *initialised decoder output
  geometry*: a consumer reads them through the stride the plane
  carries. Audio has no such geometry — the padding past the samples is
  not part of any layout, which is why the two cases resolve
  differently.

- **The reverse packet builders enforced no budget.**
  `ffmpeg_packet_from_{video,audio,subtitle}_packet` rebuild a packet
  into an `AVPacket` through `try_packet_copy`, which checked only
  `c_int::MAX` — so a configured `max_packet_bytes` was dead on the road
  a caller feeds a decoder directly, and bytes the demux boundary would
  have refused went straight into libavcodec. All three now take
  `PacketLimits` and judge before the allocation, with a new
  `PacketBuildError::SendPayloadTooLarge` named for the direction. This
  is the **into-FFmpeg packet road** of the budget class and it is in
  the accounting table under the same rule.

  The subtitle session **discarded `DecoderLimits` after open**; it
  retains them now, like the other two, and exposes them through
  `limits()`.

- **`try_clone_parameters` hardcoded the crate default.** The active
  `max_codec_parameter_bytes` is threaded through every clone on the
  decoder tier — the initial ownership clone, the probe state's copy,
  every probe advance and the software fallback — so a lowered ceiling
  binds before `build_codec_context` and a raised one is actually
  usable between 16 MiB and the configured value.

- **Synthesized-attachment admission charged padding the clone never
  allocates.** The carrier is an `FfmpegBytes` over the payload alone
  and the clone omits extradata entirely on that road, so the padded
  figure billed 64 bytes nobody spends — rejecting a payload in the
  last 64 bytes below the ceiling that the image road, judging the same
  bytes, accepts. `ParameterFootprint` now reports the raw
  `extradata_payload` alongside the padded clone cost, and the
  admission pass charges carriers the former. Both attachment tiers are
  exact, and the demux and image roads agree at the cap.

### Fixed — the roads into FFmpeg, and two still layouts

- **Every decoder entry point now measures its parameters.**
  `avcodec_parameters_to_context` is a wholesale copy *into* libavcodec
  — it duplicates `extradata`, every `coded_side_data` entry and the
  channel map into the context — and `VideoDecoder::open_with_limits`
  and `FfmpegImageDecoder::open` reached it with whatever the caller
  declared. The outbound clone had been closed; this was the inbound
  one. **Choke point**: `build_codec_context` measures and admits
  before the call, and every decoder in this crate opens through that
  one function (the four session `open`s, the HW probe's `build_state`,
  its per-backend advances, the software fallback), so there is no
  second road to guard.

- **The image decoder's compressed input has a ceiling.** `decode`
  duplicates its payload into an `AVPacket`, capped by nothing but
  `c_int::MAX` on the road that skips the demuxer. New
  `DecoderLimits::max_image_input_bytes`, defaulting to
  `DEFAULT_MAX_ATTACHMENT_BYTES` (64 MiB) — the attachment family,
  because what this seam decodes *is* an attachment, and a packet-family
  default would have made the direct road a gigabyte more permissive
  than the demuxed one for the same bytes. New
  `ImageDecodeError::InputTooLarge`.

  Both seats ride a new `DecoderLimits`, which composes `FrameLimits`
  with the two things a decoder spends before it has produced anything.
  The four session `open`s take it in place of `FrameLimits`; the
  resampler keeps `FrameLimits`, having no parameters to copy.

- **Open C enums are no longer read as closed Rust ones.**
  `measure_parameters` formed `&AVPacketSideData` and loaded
  `AVChannelOrder` directly. Both are *open* enums: an ABI-compatible
  FFmpeg newer than the bindings emits values these do not name, and a
  typed reference asserts every field inhabits its declared type —
  undefined behaviour before a field is read. Descriptor fields now go
  through `addr_of!` and integer reads, side-data type ids are copied
  as **raw bits** (a kind this build cannot name is still a kind the
  file carries), and an unrecognised channel order **fails closed**,
  because its union arm is unknown and both "owns nothing" and "owns
  `nb_channels` of something" would be guesses. Swept the diff for
  siblings: the remaining bindgen-enum reads in this crate already went
  through raw `i32` windows (`from_av_pixel_format`,
  `collect_side_data`, `packet_side_data`, `channel_layout`,
  `build_tracks`' codec id).

- **A synthesized attachment is cloned with `extradata` omitted from
  the outset**, not copied and stripped afterwards. The old shape
  allocated the payload for no reason and — worse — charged it against
  the *parameter* ceiling on the way past, so a font between the two
  ceilings (over the 16 MiB parameter one, under the 64 MiB attachment
  one) passed the admission pass and then failed deterministically
  inside the clone. New `ExtradataPolicy` makes the clone's accounting
  the same accounting the admission did.

- **Indexed and 1-bit stills decode.** Cover art is routinely an
  indexed PNG (`pal8`) and sometimes a 1-bit one (`monob`); both were
  refused outright. **Nothing is converted** — mediadecode delivers
  what FFmpeg decoded, and turning `pal8` into RGB is colconv's job one
  tier along — so a paletted still arrives as its indices plus its
  palette and a 1-bit still as packed rows.

  **The still road was widened, not the shared one.** Measured choice:
  widening the shared road would change what every existing video
  consumer can be handed (`is_supported_cpu_pix_fmt`, the HW-transfer
  validation and the video suites all key off the same deliverability
  answer) to serve formats motion video does not occur in. The still
  road is one `PlaneRoad` enum away. Both layouts are **format**-bounded
  rather than budget-bounded: libavutil already computes a 1-bit row as
  `ceil(width / 8)`, and the palette is a flat `AVPALETTE_SIZE` run —
  256 × `AV_PIX_FMT_RGB32`, always. The palette needed its own copier
  arm because FFmpeg leaves its `linesize` at zero, which the strided
  path reads as an unusable layout.

- **The footprint counts the extradata padding.**
  `AV_INPUT_BUFFER_PADDING_SIZE` is allocated by every copy, so a
  ceiling that measured only the payload admitted 64 bytes per stream
  more than it agreed to. Counted now, and the omit policy removes
  payload *and* padding together, which makes the ceilings exact.

### Fixed — the wholesale codec-parameter copy is gone

- **`avcodec_parameters_copy` no longer runs on attacker data
  anywhere.** It deep-copies every heap field an `AVCodecParameters`
  has, and it did so before anything asked how big they were. Two
  rounds patched the field that had been noticed; the third found
  another — `coded_side_data`, where a MOV `prof` atom puts an **ICC
  profile**, on an ordinary video track that no attachment-shaped check
  ever looked at. Three findings of one class is not three bugs, so the
  copy itself was replaced rather than guarded again.

  **The inventory it was replaced against** (FFmpeg n9.0, 33 fields, 3
  of them heap): `extradata`; `coded_side_data`, meaning the descriptor
  array *and* each entry's payload; and `ch_layout`'s custom channel
  map. Everything else is scalar.

  `extras::bounded_clone_parameters` enumerates those three **by hand**.
  Scalars travel by a bytewise struct copy — so a scalar field a future
  FFmpeg adds is carried for free — and the three pointer seats are
  then nulled on the destination and rebuilt one at a time, each
  measured before it is copied and the whole footprint admitted against
  a budget first. A heap field nobody enumerated is a field that is
  **not copied**, which loses data rather than allocating it, and a
  compile-time size tripwire on `AVCodecParameters` fires when the
  struct changes shape so the omission cannot go unnoticed.

  The rule is stated where the clone lives: *no code path in this crate
  hands attacker-sized parameter data to a wholesale FFI copy.* Every
  parameter copy in the crate now goes through this one function,
  including `TrackExtra::clone_parameters` and `try_clone`, which are
  judged against the footprint their row was admitted at.

  Decode-capability parity is the oracle and it holds with **zero
  assertion edits**: `extradata` keeps its `AV_INPUT_BUFFER_PADDING_SIZE`
  trailing zeroes, every `coded_side_data` entry keeps its type and
  payload, and `ch_layout` is copied through `av_channel_layout_copy` —
  the one FFmpeg call left on the path, over a single field whose size
  was measured first. The decode, resample and image suites pass
  unchanged.

- **The preflight now sees every stream, not every attachment.**
  `admit_streams` (was `admit_attachments`) measures each stream's
  parameter footprint — allocation-free, checked arithmetic — and
  charges it against two new seats before the track loop clones
  anything.

  | seat | default | why |
  |---|---|---|
  | `DemuxLimits::max_codec_parameter_bytes` | 16 MiB | SPS/PPS is tens of bytes and FLAC headers are kilobytes; what sets the ceiling is the ICC profile in `coded_side_data`, legitimately 0.5–2 MB for a camera or display profile and up to ~10 MB for a device link. |
  | `DemuxLimits::max_total_codec_parameter_bytes` | 64 MiB | Four tracks with a large profile apiece is the realistic high end; nothing in the per-stream ceiling bounds a two-hundred-stream table. |

  New arms: `DemuxError::ParametersTooLarge` and
  `DemuxError::ParametersBudgetExhausted`, separate for the same reason
  the attachment pair are.

- **Residency accounting closed the undercharge.** A synthesized
  attachment's `extradata` is charged to the *attachment* budget, where
  the carrier holds it, and left out of the *parameter* budget, because
  the clone strips it — one set of bytes, one charge. Cover art is
  charged its parked packet to the attachment budget and its retained
  parameter heap (including any `coded_side_data`) to the parameter
  budget. `TrackExtra::parameter_bytes()` reports what a row actually
  retains.

### Fixed — two sibling paths walked around the ceilings

- **An attachment's payload was copied before its budget was charged.**
  `build_tracks` deep-copies each stream's `AVCodecParameters` on its
  way to a `TrackExtra`, and for an `AVMEDIA_TYPE_ATTACHMENT` stream
  **the extradata inside those parameters is the attachment's payload**
  (measured: a 27-byte font fixture reports `extradata_size=27`). So the
  loop paid for the payload — a full `avcodec_parameters_copy` — one
  statement before asking whether it was allowed to. A file declaring a
  gigabyte of "font" allocated the gigabyte and then reported that a
  gigabyte was too much.

  The fix is not a check moved a few lines earlier, because any
  per-track interleaving of judging and paying has the same shape: the
  aggregate budget is only knowable once every track has been *seen*.
  The new `admit_attachments` pass judges the whole file first, reading
  two integers per stream and allocating nothing, and only a container
  that passes in full reaches the loop that builds carriers and
  parameter copies.

- **An accepted attachment retained two copies of itself.** The
  synthesized road kept the payload in its carrier *and* in the
  parameter copy's extradata, behind a budget that counted one of them.
  Censused before it was changed: nothing can use that extradata —
  libavcodec has no decoder for a font, so no road opens a codec
  context from those parameters, and the payload reaches a consumer as
  the attachment packet, which is the delivery the demux tier promises.
  `build_tracks` now strips it. Cover art keeps its extradata: there the
  payload is the parked `AVPacket`, the extradata is not a copy of it, a
  still codec can legitimately need it (MJPEG with an external Huffman
  table), and the measurement says a cover-art stream carries none
  anyway.

  What the budget charges is now **residency**, not payload: a hoisted
  cover-art track is charged its parked packet *plus* the extradata it
  retains, a synthesized one only its carrier. That is what makes the
  budget an honest statement about memory rather than about file
  structure.

- **The resampler's output had no byte ceiling.** `output_capacity`
  bounds the output *sample count*, and only at `i32::MAX` — the
  structural limit of `av_frame_get_buffer`. Nothing bounded the bytes,
  and the two are related by a ratio the caller does not control: the
  capacity is `input_samples × target_rate / source_rate`, and a source
  spec read off an untrusted container can say 1 Hz. 1000 samples at a
  declared 1 Hz against a 48 kHz target is 48,000,001 output samples —
  comfortably inside the existing guard — and 384 MB of `f32` stereo,
  allocated twice counting the carrier copy, from an 8 KB input.

  `FfmpegResampler::new` now takes `FrameLimits` and
  `check_output_bytes` refuses over-budget conversions **before**
  `av_frame_get_buffer` and before any session state moves, with a new
  `ResampleError::OutputTooLarge`. `FrameLimits` rather than a seat of
  the resampler's own: the quantity is bytes of one produced audio
  frame, which is what `max_frame_bytes` means everywhere else, and one
  number for "what a frame may cost" beats a second vocabulary.

- **Terminal closure for the class.** Every `FfmpegBytes` construction
  site in the crate is now enumerated in `buffer.rs`'s module docs,
  each row naming what bounds it — a `limits` seat for a size that comes
  from a file, a format constant for a size that is a property of the
  format, and structurally-zero for the placeholder sites. The table
  lives beside the constructors so a new exit has to answer the question
  the existing ones already answered. Both rounds' findings were rows
  nobody had written down, not exceptions to a rule.

### Changed — the padded-plane copy allocates once

- A plane whose stride is padded is now written **row by row straight
  into the carrier's allocation** (`Arc::new_uninit_slice`, stabilised
  in Rust 1.82 and well under this workspace's 1.95 MSRV), through a
  new crate-internal `FfmpegBytes::from_rows`. The previous spelling
  staged the plane in a `Vec` and then copied it into an `Arc`:
  **two allocations and two copies**, so a 250 MiB frame peaked at
  750 MiB counting FFmpeg's own. It now peaks at the unavoidable 2×.

  The staging `Vec`'s `try_reserve_exact` went with it; the ceiling
  above took its place, and refuses earlier and by name. The side-data
  collectors keep their staging `Vec`, because side data is capped at
  256 KiB and a reportable first allocation is worth the doubling
  there; the plane paths are not small and are not.

### Fixed — the orientation escape lost the words that made it unnameable

- `ImageOrientation::Other` carries **all nine** display-matrix words,
  not the four of the linear part, and a matrix is read as a *named*
  orientation only when the other five are canonical — no translation
  in `x`/`y`, no perspective in `u`/`v`, and unity (`1 << 30`, 2.30
  fixed point per `libavutil/display.h`) in `w`.

  As shipped a moment ago it matched on the linear four alone, so a
  matrix that turned the picture **and** shifted or projected it was
  read as the plain turn and the extra words were dropped. That is
  precisely the collapse this crate's escape-carries-never-collapses
  law exists to prevent, one level in from where the law is usually
  applied. `linear()` remains, now documented as the four-word
  *projection* it always was, and a new `matrix()` answers the whole
  nine — reconstructing the canonical form for a named variant and
  returning what it was handed, unchanged, for the escape.

### Notes

- **The copy is infallible where the view was fallible.** `Arc<[u8]>`
  has no fallible constructor on stable Rust, so an exhausted
  allocator aborts at a copy site rather than returning
  `ConvertError::BufferAcquireFailed`. Everything that bounds *how
  much* can be asked for is unchanged and still runs first — the
  side-data entry and byte caps, the plane-geometry checks, the
  `find_backing_buffer` extent proof — so a hostile stream cannot
  reach that abort by demanding memory. Where a staging `Vec` was
  already in the path (the padded-plane copy, the side-data payload
  copies) its `try_reserve_exact` is kept, so the large allocation
  stays a named refusal and only the carrier copy that follows,
  asked for a size the allocator has just granted, is infallible.

- **The resampler's ordering law is intact.** `prepare_output` used to
  acquire one `AVBufferRef` view per output plane before
  `swr_convert_frame` ran, because wrapping a plane could fail and
  nothing fallible may run after the conversion has consumed input.
  The views are gone; the *proof* is not — every plane pointer is
  still checked non-null and checked to address `plane_len` bytes
  inside one of the frame's own buffers on the near side, so
  `finish_output` copies without anything left to judge and remains
  infallible.

## [0.8.0] - 2026-08-24

### Added

- **`FfmpegDemuxer` implements `Demuxer::take_tracks`**, the
  owned-tracks door `mediadecode`'s demux tier gained in place of a
  `TrackInfo` / `TrackParams` `Clone` — see
  [`mediadecode`'s CHANGELOG](../mediadecode/CHANGELOG.md) for the
  message-carrier law behind that redirect. The implementation is
  `mem::take` on the session's own `Vec<TrackInfo<Ffmpeg>>`, built once
  at `open` and untouched until a caller takes it.

  `TrackExtra` does not gain `Clone` either, for the same law. An
  interim version of this entry hand-wrote one through the existing
  checked `try_clone`, to satisfy the same channel bound, and it came
  back out — a full `avcodec_parameters_copy` is not the refcount bump
  a message's `Clone` is required to be. `try_clone` and
  `clone_parameters` are unaffected: they remain the checked copy this
  type offers the one caller that genuinely wants an owned duplicate,
  and `Default` remains absent for the reason it always was — there is
  no checked substitute for `Parameters::default()` to route through.

### Changed (BREAKING)

- **Every struct-shaped enum variant across this crate's errors is now
  a tuple variant wrapping a named payload struct**, following
  `mediadecode`'s own `TrackParams` / `DemuxedPacket` /
  `SubtitlePayload` sweep and the shape this crate's own `Error`
  (`HwDeviceInitFailed`, `AllBackendsFailed`, `FallbackFailed`) and
  `FrameError` already used. A struct variant has no nameable type of
  its own to return from `is_<variant>` / `unwrap_<variant>` /
  `try_unwrap_<variant>`, and traps its fields instead of giving them a
  reusable, documented, accessor-bearing home.

  - **`ResampleError`** — all twelve struct arms (`SourceChanged`,
    `PlaneCount`, `SampleCount`, `UnsupportedRate`,
    `UnsupportedFormat`, `UnsupportedLayout`, `TooManyPlanes`,
    `TimestampOutOfRange`, `ChannelDropped`, `RematrixUnsupported`,
    `TimestampOverflow`, `OutputBuffer`) move to `Variant(Payload)`.
    `Again`, `AfterEof`, `Resample(Error)` and `QueueAlloc` are
    unchanged (already unit or newtype).
  - **`PacketBufferError`** — all eight arms (`Refcount`, `Bounds`,
    `SideDataEntries`, `SideDataArray`, `SideDataPayload`,
    `SideDataBytes`, `UnrepresentableFlags`, `SideDataAlloc`) move the
    same way; every payload keeps the enum's own `Copy + Clone + Debug
    + PartialEq + Eq`.
  - **`DemuxError`** — `AttachmentAlloc`, `ParametersMissing`,
    `ParametersAlloc`, `ParametersCopy`, `PacketBuffer`, `ReaderPanic`
    move; `Ffmpeg(#[from] ffmpeg_next::Error)` is unchanged.
  - **`boundary::PacketBuildError`** — `UnknownSideData` and
    `SideDataAlloc` move. This crate already had *two* same-named
    `SideDataAlloc` payloads in scope at this file (the read-side one
    on `PacketBufferError`, imported here, and this write-side one,
    native to this module) — the import is aliased
    `SideDataAlloc as BufferSideDataAlloc`; the native struct keeps the
    bare name.
  - **`convert::ConvertError`** — `UnsupportedPixelFormat`,
    `InvalidPlaneLayout`, `BufferAcquireFailed` move (found during the
    sweep, not in the original census — this enum already violated the
    same rule). This enum hand-writes `Display` rather than using
    `thiserror`; the extracted payloads keep that idiom (their own
    `impl Display`), and the enum's `Display` delegates to it per
    variant.
  - **`video::VideoDecodeError::PostCommitNeverResynced`** — also
    found during the sweep — moves to
    `PostCommitNeverResynced(PostCommitNeverResynced)`. `Decode` and
    `Convert` were already newtype variants.

  Every extracted payload is `thiserror`-derived where the enum already
  was (`#[error(transparent)]` on the wrapper, `#[from]`, the original
  message moved verbatim onto the payload's own `#[error("...")]`) or
  hand-`Display` where the enum already was (`ConvertError`) — no
  crate's error idiom changed, only the variant shape. A match that
  used to destructure fields now binds the payload and reads it through
  accessors, e.g. `Err(ResampleError::PlaneCount { expected, found })`
  becomes `Err(ResampleError::PlaneCount(p))` with `p.expected()` /
  `p.found()`.

  **The accessor face rides the reshape, same as `mediadecode`'s.**
  `ResampleError`, `PacketBufferError`, `DemuxError`,
  `boundary::PacketBuildError`, `convert::ConvertError`,
  `video::VideoDecodeError`, `audio::AudioDecodeError`,
  `subtitle::SubtitleDecodeError`, and `error::Error` — the crate's
  full error taxonomy, not just the arms the reshape sweep touched —
  now derive `derive_more::{IsVariant, Unwrap, TryUnwrap}`, with
  `#[unwrap(ref, ref_mut)]` and `#[try_unwrap(ref, ref_mut)]` — every
  arm answers `is_<variant>()`,
  `unwrap_<variant>()` / `_ref()` / `_mut()`, and
  `try_unwrap_<variant>()` / `_ref()` / `_mut()`. `Backend`,
  `PictureType`, `resampler::SpecEnd`, and `error::FallbackOrigin` gain
  `IsVariant` too; `FallbackOrigin`'s hand-written `is_post_commit()`
  and `ResampleError`'s hand-written `is_again()` are gone — the
  derive answers both now, `&self`-receiver in place of the old
  by-value one (transparent at every call site, all of which already
  used method-call syntax). This crate did not carry a `derive_more`
  dependency before this entry; it does now, with the `is_variant` /
  `unwrap` / `try_unwrap` features (no `display` — this crate's error
  types keep `thiserror` / hand-`Display`, unchanged).

## [0.7.0] - 2026-08-23

### Added

- **`FfmpegDemuxer` — `mediadecode`'s demux tier over `libavformat`.**
  Opens from a path (`open`) or from any `Read + Seek` byte source
  through a custom `AVIOContext` (`open_reader`). `Seek` is mandatory
  and not negotiable: MP4 routinely puts `moov` at the end, so a reader
  that cannot go backwards cannot be probed at all — and the face's seek
  law would be unimplementable.

  The track table is built once at open, in stream order, so
  `TrackIndex(i)` and `AVStream.index` are the same number by
  construction. Each row carries the stream's `Parameters`, which is
  what opens a decoder for it — a deep copy with no tie back to the
  format context, so a decoder outlives the demuxer that named it.

  **Three normalizations happen here, and they are all about
  attachments.** libavformat presents cover art as a *video* stream
  carrying `AV_DISPOSITION_ATTACHED_PIC`; this layer maps it to
  `TrackKind::Attachment`, so the `Video` arm is true motion video and
  nothing else. A font's bytes never appear in the packet stream at all
  — an `AVMEDIA_TYPE_ATTACHMENT` stream produces no packets and the
  payload lives in codec extradata — so the packet is synthesized at
  open. And cover art's real packet, which libavformat parks in
  `AVStream.attached_pic` *and* some demuxers also emit, is hoisted at
  open and its duplicate dropped. Every attachment track leaves the open
  with exactly one packet queued — or the open fails — before a single
  `av_read_frame` has run, which is what makes "exactly one packet,
  before any timed packet" a property of the construction rather than a
  promise the pull loop has to keep. A stream that declares cover art
  and parks no payload (a state this build's demuxers do not produce:
  `ff_add_attached_pic` sets the disposition and fills the packet in one
  call) still gets its one packet, empty and marked `synthesized`,
  rather than a place in a queue of packets that may never arrive.

  `seek` converts the target to `AV_TIME_BASE` units and seeks over the
  window `[i64::MIN, target]` — FFmpeg's backward convention, landing on
  the nearest keyframe at or before the target. It clears only the EOF
  latch this layer set itself, leaving a genuine sticky I/O error
  intact, and does not touch the attachment bookkeeping: an attachment
  already handed out is never handed out again, and one not yet handed
  out is still owed.

  A track whose kind is `Unknown` has no delivery arm and its packets
  are not delivered; a corrupt packet is skipped and the read resumed,
  since `AVERROR_INVALIDDATA` is not latched into the `AVIOContext`.

  **A panicking reader is an error, not an abort.** libavformat drives
  an `open_reader` byte source from `extern "C"` callbacks, where a
  panic cannot unwind and terminates the process. Every call into the
  caller's reader runs under `catch_unwind`: the panic is latched,
  reported to C as an ordinary I/O error, and surfaced as
  `DemuxError::ReaderPanic` with the panic's own message from the next
  `open_reader`, `next_packet` or `seek`. The session is terminal from
  there. The caught payload is described and then deliberately
  forgotten: `panic_any` takes any `Send` value and safe code can give
  it a `Drop` that panics, and dropping it after the catch would send
  that second panic out of `read`/`seek` and into the `extern "C"`
  callback — the abort this guard exists to prevent, reached through the
  guard itself (measured: SIGABRT). Leaking one value on a path that has
  already made the session terminal is the cheaper half of that trade by
  a distance.

  **Container metadata is read as bytes, not trusted as text.**
  `ffmpeg_next`'s `DictionaryRef::get` builds its `&str` with
  `from_utf8_unchecked`, and FFmpeg does not validate demuxed metadata
  as UTF-8; a track's `filename` / `mimetype` therefore go through
  `av_dict_get` and a bounded walk, with invalid bytes replaced rather
  than trusted.

- **Boundary helpers that carry a timebase.**
  `video_packet_from_ffmpeg_in`, `audio_packet_from_ffmpeg_in`,
  `subtitle_packet_from_ffmpeg_in` and `data_packet_from_ffmpeg_in` take
  the stream's timebase and stamp it onto every timestamp. An
  `AVPacket`'s integers are ticks in a timebase the packet does not
  carry, so the existing four-argument-less helpers stamp `1/1` and
  leave the caller to remember what the ticks meant; a demuxer holds the
  track table and has no reason to forget. The originals are unchanged
  and now delegate. `attachment_packet_from_ffmpeg` joins them for
  cover-art payloads, which have no timestamps to carry.

- **`FfmpegResampler` — `mediadecode`'s resample seam over
  `swresample`.** `FfmpegResampler::new(source, target)` takes both
  specs as [`ResampleSpec`]s and neither is inferred. The source is read
  off a demuxed track (`ResampleSpec::from_parameters`, the
  "source from `TrackInfo`" path) or off the opened decoder
  (`ResampleSpec::from_decoder`); the target is the caller's options.

  **Output timestamps are counted, not computed.** The timeline is
  anchored on the first *input* timestamp and advanced by the number of
  samples actually produced, so no arithmetic depends on how many
  samples a given `swr_convert_frame` happened to yield and the frames
  drained after `send_eof` continue the same line. The tail is real:
  the 44.1 kHz → 16 kHz lane pins that `send_eof` still has a frame to
  give, which is the difference between a file's last tens of
  milliseconds surviving and not.

  **A frame whose rate, sample format or channel layout is not the
  source spec is refused by name** (`ResampleError::SourceChanged`), and
  "nothing ready yet" is `ResampleError::Again` — the same mechanism
  `AudioStreamDecoder::receive_frame` uses, one tier along.
  `send_frame` after `send_eof` is refused too rather than silently
  accepted; `flush` is the way back to a reusable resampler.

  Two things this layer had to learn about FFmpeg to be correct, both
  recorded where they are relied on. A WAV without a
  `WAVE_FORMAT_EXTENSIBLE` channel mask genuinely declares **no**
  layout, and FFmpeg reports that unspecified layout in the codec
  parameters, in the codec context and on every decoded frame —
  substituting a default would make the source spec disagree with the
  frames it describes and refuse all of them. But `swr_init` *does*
  substitute a default internally, and then compares every frame handed
  to it against that one — so the frames this layer stages carry the
  post-init layout while the mid-stream check keeps comparing against
  the declared one. Custom and ambisonic layouts are refused outright:
  both keep a heap-allocated channel map that a `Copy` layout wrapper
  cannot own safely, and a silent approximation would be worse than a
  refusal. `from_parameters` / `from_decoder` answer `None` for them,
  and — because `ResampleSpec::new` is `const` and total —
  `FfmpegResampler::new` is the choke point that refuses them by name
  (`ResampleError::UnsupportedLayout`), along with a rate of zero or
  past `c_int`, `AV_SAMPLE_FMT_NONE`, and a planar spec with more
  channels than a frame has plane slots
  (`ResampleError::TooManyPlanes` — planar 22.2 is twenty-four planes
  against the model's eight, unusable as a source because no frame
  could carry it and worse as a target because the failure would land
  after `swr` had consumed the input). Nothing hazardous, and nothing
  unusable, reaches `swr` or a staged `AVFrame` whichever route a spec
  came in by.

  **The pair is checked too, not just each end.** Two individually
  valid layouts can still lose whole channels between them: `swr` mixes
  the channel positions its rematrix table knows and processes the rest
  of the input as though it were absent. Measured against the linked
  FFmpeg 9 with a tone isolated in each source channel, packed
  22.2 → mono drops fifteen of twenty-four and `cube` → stereo drops two
  of *eight* — so channel count is not the rule. The rule is asked of
  FFmpeg: `swr_build_matrix2` builds the matrix it would use, and a pair
  where any source channel reaches no output is refused
  (`ResampleError::ChannelDropped`), as is one FFmpeg will not matrix at
  all (`ResampleError::RematrixUnsupported`). LFE's mix level is forced
  non-zero for the question, so FFmpeg's deliberate downmix policy —
  which leaves LFE out — is not mistaken for a channel it cannot carry.
  Accepting such a conversion knowingly needs an explicit mix-matrix
  seat on the spec; until that exists, refusal is the honest answer.

  The pair judged is the **effective** one — the layouts `swr` is
  configured with, resolved before the check rather than after it. An
  unspecified layout becomes FFmpeg's default for its channel count, and
  twenty-four unspecified channels resolve to exactly the 22.2 whose
  explicit conversion is refused, so judging the declared pair left that
  routing a door. This is the second half of the crate's two-layout
  bookkeeping and it composes with the first: the **declared** layout is
  what decoded frames carry and stays the yardstick for the mid-stream
  refusal (*is this frame the stream I was built for?*), while the
  **effective** layout is what `swr`, every staged `AVFrame` and now
  this check use (*what will `swr` actually do?*). A maskless WAV — one
  channel resolving to mono, two to stereo — keeps converting exactly as
  before.

  **Nothing that can fail is left on the far side of the conversion.**
  Every resource a converted frame needs — the output frame, one
  refcounted view per plane, a placeholder for every unused slot, and
  the room in the ready queue — is acquired *before*
  `swr_convert_frame` touches a sample, on the ordinary path and on the
  EOF drain alike; the views are taken at full capacity and trimmed
  afterwards by a shrink that allocates nothing. The step that turns a
  converted frame into a `mediadecode` one is consequently infallible by
  signature, which is what makes the property hold rather than
  hold-for-now: a failure after `swr` has consumed input leaves a
  session no caller can act on, since retrying feeds the same samples
  twice and continuing loses them.

  **State is committed after the work it describes succeeds.** A frame
  refused for its geometry does not anchor or advance the output
  timeline; timestamps are counted with checked arithmetic and an
  overflow is `ResampleError::TimestampOverflow` — raised *before*
  `swr` consumes the frame, checked against the most samples the call
  could produce, so a refused conversion leaves a session the caller can
  still use (the check used to run after the conversion, which left the
  filter's history moved, an output frame built and dropped, and the
  timeline anchored where every later frame overflowed too) — rather
  than `i64::MAX` repeated; the anchor itself is rescaled with the checked rung before
  anything is staged, so a timestamp that cannot land on the output
  timeline is `ResampleError::TimestampOutOfRange` with the resampler
  untouched, rather than a clamp — a positive clamp used to surface only
  after `swr` had consumed the input, and a negative one landed on
  `i64::MIN`, which is `AV_NOPTS_VALUE`, erasing the timestamp instead
  of reporting it; plane geometry is settled in checked arithmetic *before* any
  allocation is sized from a frame header, and every allocation is
  checked (`frame::Audio::new` dereferences `av_frame_alloc` unchecked
  and discards `av_frame_get_buffer`'s return, so this crate allocates
  its audio frames through a local helper that does neither). `flush`
  rebuilds the `swr` context rather than draining it: a drain that gave
  up and a drain that finished are indistinguishable from outside, and a
  reset that reports success must not leave the previous stream's tail
  inside the filter.

- **`resample` feature, on by default.** `FfmpegResampler`,
  `ResampleError`, `ResampleSpec`, `SpecEnd` and the
  `ffmpeg-next/software-resampling` link they need move behind it, so a
  decode-only consumer building `--no-default-features --features std`
  drops `libswresample` from the link line entirely rather than merely
  not calling into it. The core `mediadecode::resampler` trait module
  stays ungated — it adds no dependency of its own, backend or
  otherwise, over what the crate already compiles unconditionally.

- **`SampleFormat::to_ffmpeg` / `SampleFormat::from_ffmpeg`** — the
  direction this newtype was missing. A raw sample-format integer read
  out of a container cannot be cast back into the bindgen enum to reach
  FFmpeg's safe API, so `to_ffmpeg` matches it against compile-time
  constants instead, exactly as `boundary::from_av_pixel_format` comes
  the other way.

- **`DataPacketExtra`, `AttachmentPacketExtra`, `TrackExtra`** — the
  demux tier's `*Extra` carriers, and `impl DemuxAdapter for Ffmpeg`
  binding them. `AttachmentPacketExtra::synthesized` records whether a
  payload came from a real packet or was built out of codec extradata,
  which is the first thing to check when an attachment looks wrong.
  `TrackExtra::disposition` is the raw `AV_DISPOSITION_*` bit set rather
  than `ffmpeg_next`'s `Disposition`, whose `from_bits_truncate` would
  drop bits this build has no constant for.

### Fixed

- **A sparse thumbnail track is a timed track, not one attachment.**
  Classification tested `AV_DISPOSITION_ATTACHED_PIC` alone, and FFmpeg
  documents `AV_DISPOSITION_TIMED_THUMBNAILS` — "the stream is sparse,
  and contains thumbnail images, often corresponding to chapter
  markers" — as *only ever* used together with it. Such a stream was
  therefore read as cover art: the exactly-once attachment contract
  queued the parked copy and the delivery loop dropped every timestamped
  thumbnail after it. The two bits are now read off the raw
  `AVStream.disposition` (`ffmpeg_next`'s `Disposition` mints no
  `TIMED_THUMBNAILS` constant, and its `from_bits_truncate` drops what
  it cannot name — which is how the distinction went missing), and a
  timed-thumbnail stream goes to the **`Video`** arm. That does not
  bend "cover art is an attachment, not video": the reason behind that
  ruling is that a still with no timeline must not look like a motion
  track, and this stream *is* on the timeline — sparse video, with a
  codec id, a frame size and packets that carry timestamps.

- **A packet's side data arrives with the packet, and a side-data-only
  packet is no longer mistaken for an empty marker.** The `*Extra`
  carriers have always documented a `side_data` seat and the conversion
  never filled one; measured on this repository's own generated corpus,
  every container carries at least one packet with real side data
  (`AV_PKT_DATA_SKIP_SAMPLES` — the encoder-delay trim an MP3 or AAC
  stream must be cut by), so that data was dropped at the boundary on
  every file. Worse, a packet with **no body** and only side data —
  FFmpeg's shape for `AV_PKT_DATA_NEW_EXTRADATA` and for a parameter
  change — read as "empty, skip it", so a decoder could be left running
  on parameters the container had already replaced, with nothing said.

  All four timed arms now collect side data — **whole, or not at all**.
  The collection is bounded (64 entries or as many as this build names,
  whichever is larger; 256 KiB; `try_reserve_exact`), and every bound is
  an **error**, not a truncation: a packet whose side data cannot be
  carried complete is refused by name
  (`PacketBufferError::SideDataEntries` / `SideDataBytes` /
  `SideDataAlloc`, surfacing as `DemuxError::PacketBuffer`) rather than
  delivered missing the entries a decoder acts on. Truncating would put
  the original defect back twice over: a body-bearing packet reaching
  the codec without its `NEW_EXTRADATA`, and a side-data-only packet
  losing its only content and vanishing as an empty marker. The arms
  also deliver a side-data-only packet with an owned empty buffer.

  The same rule reaches the pointers, not only the caps: a count is
  judged before the array it describes, so a malformed or over-cap count
  is refused whether or not the array happens to be null
  (`SideDataArray` names the missing array), and an entry declaring
  bytes it does not carry is refused (`SideDataPayload`) rather than
  read as an empty entry that charges the budget nothing. A zero-size
  entry is still a marker and still welcome.
  `SubtitlePacketExtra` and `DataPacketExtra` gained the seat the other
  two already had. `AttachmentPacketExtra` deliberately did not: an
  attachment is its bytes, so a packet carrying none carries no
  attachment, and that arm still answers `Ok(None)`.

  **And the reverse direction reattaches it**, which is what makes the
  capture worth anything: the three `ffmpeg_packet_from_*_packet`
  helpers are what the trait decoders hand to the codec, and they
  rebuilt a packet from body and timestamps alone. Measured end to end
  on `cover.mp3`: with the trim reattached the decoder returns 88 200
  samples — exactly the two seconds the file holds — and without it
  89 856, the encoder's padding included. A side-data type this build of
  FFmpeg does not name is refused
  (`PacketBuildError::UnknownSideData`) rather than dropped or handed to
  C as an invalid discriminant.

- **Every packet flag survives both directions.** Forward conversion
  went through `ffmpeg_next`'s `Packet::flags()`, whose `Flags` bit set
  names `KEY` and `CORRUPT` and truncates the rest away before this
  crate sees them; the reverse rebuilt only those two. `PacketFlags` is
  a bit set whose documented lossless door is `from_bits_retain`, so
  both directions were breaking its contract — and losing
  `AV_PKT_FLAG_DISCARD`, which tells a consumer to decode a packet and
  throw its output away, makes preroll output look like something to
  keep. `AVPacket.flags` is now read and written as the raw integer it
  is, so `DISCARD`, `TRUSTED`, `DISPOSABLE` and the bits nothing names
  yet all round-trip. A compile-time assertion states that every flag
  this build names fits the byte `PacketFlags` carries; a packet
  carrying one that does not is refused
  (`PacketBufferError::UnrepresentableFlags`) rather than delivered a
  bit short. The hoisted cover-art packet reads its flags through the
  same reader: it is built by hand from `AVStream.attached_pic` rather
  than through the boundary conversion, and it used to be built with
  none at all — FFmpeg marks an attached picture `AV_PKT_FLAG_KEY`, so
  every cover this crate delivered arrived saying it was not a
  keyframe. A synthesized attachment still carries no flags, because
  no packet was parked to read them from.

- **`TrackExtra` no longer derives `Clone` or `Default`, and the
  decoder handoff is checked.** Both derives went through
  `ffmpeg_next`'s `Clone` / `Default` for `Parameters`, so copying a
  track row from safe public code reached the same unchecked
  allocation — measured, a SIGSEGV — and the documented handoff was
  `parameters().clone()`, which is that same clone. `Clone` cannot
  report a failure, so the type does not implement it:
  `TrackExtra::try_clone` is the row copy with an answer, and
  `TrackExtra::clone_parameters` is the handoff that opens a decoder.
  `parameters()` still lends the parameters for inspection —
  `ResampleSpec::from_parameters` reads them — but nothing in this crate
  asks a caller to clone them unchecked any more.

  **And a `TrackExtra` cannot exist over parameters that were never
  allocated.** `Parameters::new()` / `Default` hand back a null-backed
  value when `avcodec_parameters_alloc` failed and report nothing, so
  checking only the destination of a copy left the source trusted: once
  the allocator recovered, `avcodec_parameters_copy(out, NULL)`
  dereferenced null from safe public code. `TrackExtra::new` is
  therefore fallible and refuses one
  (`DemuxError::ParametersMissing`), which gives the type a non-null
  invariant from birth, and the copier checks its source as well —
  belt and braces, because the invariant is a promise and the check is
  a fact. `ResampleSpec::from_parameters` and `from_decoder` grew the
  same guard: the first used to ask `medium()` on the way in, which
  dereferences the pointer inside ffmpeg-next before any code of this
  crate runs.

- **A track's codec parameters are copied with both fallible steps
  checked.** `ffmpeg_next`'s `Clone` for `Parameters` checks neither:
  `Parameters::new` does not test `avcodec_parameters_alloc` for null
  and `clone_from` dereferences it immediately — measured, that is a
  SIGSEGV under a failed allocation — while the copy's return value is
  discarded, so a partial copy (a failed extradata allocation, say)
  produced parameters that look complete and open a decoder wrong.
  Every track in the table goes through this, so it is a whole session
  built on parameters that are not the file's. A local helper checks
  both legs and reports `DemuxError::ParametersAlloc` /
  `ParametersCopy`; a partial copy is freed on the way out.

### Changed (BREAKING)

- **Wrapping a packet's payload answers `Result<Option<_>>`.**
  `FfmpegBuffer::from_packet`, `video_packet_from_ffmpeg`,
  `audio_packet_from_ffmpeg`, `subtitle_packet_from_ffmpeg`, the four
  `*_packet_from_ffmpeg_in` variants and `attachment_packet_from_ffmpeg`
  now return `Result<Option<_>, PacketBufferError>`.

  `Ok(None)` is a packet that carries no payload — the empty marker some
  demuxers emit, and the only thing a pull loop may skip. An `Err` is a
  payload that *is* there and could not be carried: `av_buffer_ref`
  refusing a reference under memory pressure, or a packet whose payload
  does not lie inside its own buffer. The single `None` these used to
  share made the second look like the first, so a demuxer under memory
  pressure dropped real compressed bytes and read on as though the file
  had said so. `FfmpegDemuxer::next_packet` surfaces the failure as
  `DemuxError::PacketBuffer`, naming the stream.

  Call sites that skipped `None` add one `?` or an `expect`; nothing
  else changes.

- **The three `ffmpeg_packet_from_*_packet` helpers answer
  `Result<Packet, PacketBuildError>`.** They can now fail for a reason
  `ffmpeg_next::Error` cannot spell: a side-data entry whose type this
  build of FFmpeg does not name, or whose allocation failed.
  `Error::PacketBuild` carries it into the decoder error types.

## [0.6.0] - 2026-08-21

### Added

- **A layout that falls off the constant table now gets one more
  chance: FFmpeg's own name for it.** `channel_layout_from_ffmpeg`
  keeps its constant-arm table as the first and authoritative rung —
  a mapped constant is answered there, byte for byte, and renders
  nothing and parses nothing. Only on a miss does the adapter ask
  `av_channel_layout_describe` what FFmpeg calls the layout and read
  that word through `mediaframe::audio::ChannelLayout`'s `FromStr`,
  the total door the vocabulary already exposes.

  A named variant wins; anything the vocabulary does not name still
  lands on the absent sentinel with FFmpeg's rendering in `text()`.
  The rendering never rides `known_kind`'s `Other` escape, so
  "unrecognised" stays one value rather than one per spelling.

  What the rung reaches that the table cannot: `binaural`, `5.1.2`
  and `9.1.6` — all three named by `ChannelLayout`, none of them
  minted as an `ffmpeg_next` constant — plus FFmpeg 9's own `5.1.4`,
  whose side-surround mask `ffmpeg_sys_next`'s bundled
  `channel_layout_fixed.h` misses, its
  `AV_CH_LAYOUT_5POINT1POINT4_BACK` still carrying FFmpeg 8's
  back-surround formula. And, from here on, any layout a later
  FFmpeg adds that the vocabulary already names, with no edit in
  this crate: FFmpeg speaks the name, the vocabulary reads the word,
  one source.

  On the `ChannelLayoutDescription` path the layout is described
  once and that single rendering feeds both `known_kind` and
  `text`, so the two can never answer from different FFmpeg calls.

### Changed (BREAKING)

- **`channel_layout`'s conversions produce `mediaframe::audio` types,
  and are named after them.** `mediadecode::channel` is gone (see
  [`mediadecode`](../mediadecode/CHANGELOG.md#unreleased)); the
  vocabulary lives upstream now, and the `Kind` ornament is buried with
  the enum it named.

  | was | is | returns |
  | --- | -- | ------- |
  | `channel_layout_kind_from_ffmpeg` | `channel_layout_from_ffmpeg` | `ChannelLayout` |
  | `audio_channel_order_kind_from_ffmpeg` | `channel_order_from_ffmpeg` | `ChannelOrder` |
  | `audio_channel_order_kind_from_raw` | `channel_order_from_raw` | `ChannelOrder` |
  | `audio_channel_layout_from_ffmpeg` | `channel_layout_description_from_ffmpeg` | `ChannelLayoutDescription` |
  | `audio_channel_layout_from_raw_ptr` | `channel_layout_description_from_raw_ptr` | `ChannelLayoutDescription` |

  The bodies are unchanged: same raw-pointer discipline (`order` is
  read as `i32` before any `AVChannelOrder` value is formed), same
  union guards, same `av_channel_layout_describe` buffer growth, same
  UTF-8-lossy label decoding. Only the target vocabulary moves.
  FFmpeg's own `ChannelLayout` is imported as `AvChannelLayout` inside
  the module so the plain name belongs to what the functions produce.

- **`AudioAdapter::ChannelLayout` and the `AudioFrame` alias bind
  `mediaframe::audio::ChannelLayoutDescription`.** A caller that stored
  a decoded layout in a type of its own re-spells that type; a caller
  that only passed frames along needs nothing. The layout's *name* now
  arrives as `ChannelLayout` rather than `ChannelLayoutKind`, the
  absent case is `ChannelLayout::default()` (the `Other("")` sentinel)
  rather than `ChannelLayoutKind::Unknown`, and FFmpeg's rendering is
  read back with `text()` where it was `description()`.

- **Three layout arms answer to different idents, and one is gone.**
  Upstream's idents follow FFmpeg's constants except where a constant
  names an arrangement no reader would recognise, so `SURROUND` is
  `Ch3_0`, `AV_CH_LAYOUT_2_1` is `Ch3_0Back` and `AV_CH_LAYOUT_2_2` is
  `QuadSide`. The `_7POINT1_TOP_BACK` arm is deleted outright:
  `ffmpeg-next` defines that constant as an alias of
  `AV_CH_LAYOUT_5POINT1POINT2_BACK`, which this table already matches
  twelve arms earlier, so the arm could never be reached and the
  variant it produced does not exist upstream.

  Layouts the table still cannot name — `BINAURAL`, `_5POINT1POINT2`
  (the side one) and `_9POINT1POINT6`, all three named by
  `mediaframe::audio::ChannelLayout` — have no `ffmpeg_next`
  `ChannelLayout` constant to match against. The describe rung above
  reaches them all the same; without it they would keep arriving as
  the absent sentinel with FFmpeg's spelling preserved in `text()`,
  exactly as before.

- **`mediaframe` is a direct dependency**, pinned with `alloc`: the
  audio household is compiled only at the alloc-or-std tier.

## [0.5.0]

Tracks `mediadecode` 0.5.0, which crosses `mediaframe` 0.4 → 0.5 — a
breaking minor. `mediaframe` is a public dependency of the core crate
and its `PixelFormat` and `color` types appear in this adapter's own
signatures (`convert`, `pixdesc`, `frame`), so this adapter's
re-exported surface moves with it. See
[`mediadecode` 0.5.0](../mediadecode/CHANGELOG.md#050).

Nothing about the FFmpeg boundary's behaviour changes, and no source
line moved. The version bump is the public-dependency crossing, not a
change of conduct: `mediaframe` 0.5 adds no pixel-format or colour
variant, so this crate's exhaustive maps over `PixelFormat` — which
would have been `E0004` had one appeared — are unchanged, and the
FFmpeg constant tables still round-trip.

Two upstream conveniences arrive for free: the re-exported vocabularies
carry `ROSTER`, and `BayerPattern` is closed, so a downstream matching
it can drop its wildcard arm.

## [0.4.0] - 2026-08-19

### Changed (BREAKING)

- **`convert::ConvertError::UnsupportedPixelFormat` names the format
  again.** It was `UnsupportedPixelFormat(PixelFormat)`; it is now a
  struct variant carrying `format`, `raw` and `name`. The `format`
  field is the old payload unchanged — still `PixelFormat::None` for
  the fall-through, because this restores the *diagnostic*, not the
  `Unknown(u32)` variant mediaframe 0.3 struck. What is restored is
  everything the message lost with it:
  - `raw: i32` — the `AVFrame.format` integer exactly as FFmpeg wrote
    it, present unconditionally.
  - `name: Option<SmolStr>` — FFmpeg's own name for that integer, from
    `av_get_pix_fmt_name`, or `None` when libavutil has no descriptor
    for it.

  The rendered message goes from `unsupported pixel format None` back
  to `unsupported pixel format None (AVPixelFormat <n> =
  "videotoolbox_vld")`, and to `… (AVPixelFormat 99999, unnamed by
  libavutil)` where there is no name. This supersedes the note under
  0.4.0 below, which recorded the message losing the raw integer.

  Callers matching `ConvertError::UnsupportedPixelFormat(pf)` move to
  `ConvertError::UnsupportedPixelFormat { format, .. }`.

  The lookup does not weaken the crate's FFI stance. `av_get_pix_fmt_name`
  is redeclared with a plain `c_int` parameter rather than the bindgen
  enum, so an integer outside our build's discriminant set is never
  turned into an `AVPixelFormat` — which is the whole reason the
  fall-through exists in the first place. Two tests pin that libavutil
  answers such integers with null rather than misbehaving.

Tracks `mediadecode` 0.4.0, which crosses `mediatime` 0.1 → 0.3 and
`mediaframe` 0.1 → 0.3 — two breaking minors each. Both are public
dependencies of the core crate, so this adapter's re-exported
signatures move with them. See
[`mediadecode` 0.4.0](../mediadecode/CHANGELOG.md#040). This release
also crosses `ffmpeg-next` 8.1 → 9. Nothing about the FFmpeg
boundary's *behaviour* changes: the same raw integers are accepted,
the same formats are deliverable, and the same frames are rejected.

### Changed (BREAKING)

- **`ffmpeg-next` 8.1 → 9**, tracking FFmpeg 9. `ffmpeg_next::Packet`,
  `Frame`, `Error`, `decoder::Audio` and `decoder::Subtitle` appear in
  this crate's public signatures, so `ffmpeg-next` is a public
  dependency and downstream crates must move to the same major.
  No adapter source changed: the bump is spelling-clean, and the
  boundary's behaviour is unaffected. What it buys is the
  `ffmpeg_9_0` code path — `ffmpeg-sys-next` 8.1's version table
  topped out at `ffmpeg_8_1` and gated 9.0 off even when linked
  against a 9.x system library. Two surfaces moved in FFmpeg 9 but do
  not reach this crate: capability queries (`Audio`/`Video`'s
  `rates` / `formats` / `channel_layouts`) now read
  `avcodec_get_supported_config` instead of the codec struct's
  fields, and none of them are called here; and the codec-id
  vocabulary dropped `V308` / `V408` / `V410` while adding
  `WEBP_ANIM` / `APPLE_APAC`, which `CodecId` absorbs as a
  `#[repr(transparent)]` `i32` with a fall-through `Debug` arm. The
  pixel formats this crate already mapped to `None` for want of an
  `AV_PIX_FMT_*` constant (`V210`, `V410Le`, `Yuva420p12Le`,
  `Yuva444p14Le`) are still absent in 9, so their fall-through
  stands.
- **The pixel-format fall-through is now `PixelFormat::None`.**
  `boundary::from_av_pixel_format` returned
  `PixelFormat::Unknown(raw as u32)` for a raw `AVFrame.format`
  integer with no mapping; mediaframe 0.3 struck that variant, so the
  raw integer no longer rides along in the returned value.
  `PixelFormat::None` is a *named* member of the vocabulary (FFmpeg's
  own `AV_PIX_FMT_NONE`, and the `Default`), which is also now the
  exact answer for `AV_PIX_FMT_NONE` itself. Callers matching
  `PixelFormat::Unknown(_)` switch to `PixelFormat::None`; callers
  that read the payload have the raw integer at the call site, where
  `is_hardware_pix_fmt` already reads it. Rejection is unchanged —
  `pixdesc::to_av_pixel_format`, `pixdesc::is_deliverable`,
  `frame::is_supported_cpu_pix_fmt` and both geometry tables refuse
  the fall-through exactly as before. `boundary::empty_video_frame` /
  `try_empty_video_frame` fill the same placeholder.
- **`Timebase` construction is signed.** `mediatime` 0.2 made
  `Timebase`'s numerator and denominator `i32` / `NonZeroI32`, so the
  README example, `examples/decode_via_trait.rs`, and the three
  integration tests drop their `as u32` casts on
  `ffmpeg::Rational::numerator()` /
  `.denominator()` — which are C `int`s to begin with. A stream
  declaring a negative numerator now panics in `Timebase::new`
  instead of wrapping to a huge `u32`.

### Changed

- **`mediadecode` dep**: bumped to `0.4`.
- Crate-internal pixel-format predicates and tables now borrow rather
  than consume, since `mediaframe::PixelFormat` is no longer `Copy`:
  `pixdesc::{is_deliverable, plane_geometry, to_av_pixel_format}`,
  `frame::{is_supported_cpu_pix_fmt, plane_row_bytes_for,
  plane_height_for}` and `convert::{is_yuvj, map_range_for}` take
  `&PixelFormat`. All are `pub(crate)` or private — no public
  signature moves.
- `convert::ConvertError::UnsupportedPixelFormat` still carries the
  format by value; the two construction sites move it in after the
  borrowing checks, so no clone was introduced. Its rendered message
  changes for the fall-through case, from
  `unsupported pixel format Unknown(119)` to
  `unsupported pixel format None` — the raw integer is no longer in
  that log line.

[0.4.0]: https://github.com/findit-studio/mediadecode/releases/tag/mediadecode-ffmpeg-v0.4.0

## [0.3.3] - 2026-06-25

Every byte-addressable CPU pixel format FFmpeg can produce now decodes
and delivers ([#15](https://github.com/findit-studio/mediadecode/pull/15)).
The hand-maintained per-format geometry table is gone from the decode
path; libavutil's own descriptor is the authority.

### Added

- **`pixdesc` module.** Per-plane geometry — visible row bytes and row
  count — is derived from `av_image_fill_linesizes` and
  `av_image_fill_plane_sizes` for the exact `(format, width, height)`,
  so it is correct by construction for any format FFmpeg can describe
  rather than for the formats someone remembered to tabulate. The
  safety stance is unchanged: an `AVPixelFormat` is never constructed
  from a runtime integer, only mapped from a recognised `PixelFormat`
  to a compile-time `AV_PIX_FMT_*` constant.

### Fixed

- **The YUVJ family decodes.** `yuvj420p` / `yuvj422p` / `yuvj440p` /
  `yuvj444p` / `yuvj411p` previously fell through to `Unknown` and were
  rejected at convert; they now map, deliver, and carry their JPEG
  range on `ColorInfo::range`. This is the real-world fix for MJPEG and
  JPEG-range footage.
- **`boundary::from_av_pixel_format` covers 251 formats**, up from 63.

### Changed

- **Deliberate exclusions**, by descriptor flag rather than by omission:
  Bayer mosaics (a demosaic question, not a geometry one), GPU surface
  formats (the hardware path transfers to a CPU format first), paletted,
  monochrome and sub-byte-packed RGB. All are rejected up front by
  `pixdesc::is_deliverable` with a layout the crate can state, instead
  of being read as `linesize × height` bytes of guesswork.
- Four formats have no constant in the linked `ffmpeg-sys-next` 8.1 and
  so still fall through: `V210`, `V410LE`, `YUVA420P12LE`,
  `YUVA444P14LE`.

> Downstream note carried from the PR: this crate now delivers formats
> the colconv resample layer did not yet handle at the time, which
> decode and then fail at resample. That is not a regression — they
> failed earlier, at decode, before this release — and closing the gap
> is colconv-side work.

[0.3.3]: https://github.com/findit-studio/mediadecode/releases/tag/mediadecode-ffmpeg-v0.3.3

## [0.3.2] - 2026-06-24

Hardware decode that collapses **after** the probe has committed now
falls back to software instead of failing the stream
([#13](https://github.com/findit-studio/mediadecode/pull/13)).

### Added

- **`FallbackOrigin`** (`Probe` / `PostCommit`) on the
  `AllBackendsFailed` payload, with `AllBackendsFailed::origin()` and
  `AllBackendsFailed::new_post_commit()`. The wrapper routes its
  software-fallback replay on this explicit signal rather than inferring
  the origin from whether `unconsumed_packets` is empty — both origins
  can be empty (a probe-era failure on the very first packet has no
  history to surface either), so emptiness cannot tell them apart.
  Conflating the two made a probe-era first-packet cap trip look
  post-commit, and the packet could be dropped in silence.

### Fixed

- **Runtime HW failure no longer ends the stream.** When the committed
  backend fails after the probe has collapsed, the decoder opens a
  software decoder cold, forwards the failing call's packet (or EOF),
  and resyncs at the next keyframe — a bounded, logged gap rather than
  a dead stream. Probe-era failures keep the previous behaviour: the
  buffered history is replayed and the current packet routed on.

[0.3.2]: https://github.com/findit-studio/mediadecode/releases/tag/mediadecode-ffmpeg-v0.3.2

## [0.3.1] - 2026-06-14

Additive release ([#10](https://github.com/findit-studio/mediadecode/pull/10)).

### Added

- **`Clone` on the decode-path error types** — `Error`,
  `AllBackendsFailed`, `FallbackFailed`, `AudioDecodeError` and
  `ConvertError` — so a consumer can forward one error event to several
  per-stream subscribers. Every payload was already cheaply clonable:
  `ffmpeg_next::Error` is `Copy`, `ffmpeg_next::Packet` is `Clone`, and
  `Backend` is `Copy`. `mediadecode::AudioFrame` gains `Clone` in the
  same release — see
  [`mediadecode` 0.3.1](../mediadecode/CHANGELOG.md#031---2026-06-14).

[0.3.1]: https://github.com/findit-studio/mediadecode/releases/tag/mediadecode-ffmpeg-v0.3.1

## [0.3.0] - 2026-06-07

Tracks `mediadecode` 0.3.0, which flips the shared vocabulary crate from
`videoframe` 0.2 to `mediaframe` 0.1
([#7](https://github.com/findit-studio/mediadecode/pull/7),
[#8](https://github.com/findit-studio/mediadecode/pull/8)). See
[`mediadecode` 0.3.0](../mediadecode/CHANGELOG.md#030---2026-06-07) for
what moved in the vocabulary itself.

### Changed (BREAKING)

- **`mediadecode` dep**: bumped to `0.3`. The re-exported vocabulary
  types are `mediaframe`'s now, so this adapter's type aliases and
  signatures carry the new identity.
- **Two colour-transfer mappings are renamed**, tracking upstream:
  `ColorTransfer::Bt470M` → `Gamma22` and `Bt470Bg` → `Gamma28`. The
  FFmpeg wire mapping is untouched — the same `AVCOL_TRC_GAMMA22` /
  `AVCOL_TRC_GAMMA28` land on the same values under their new spelling.

### Changed

- Version bumped to 0.3.0 with the rest of the workspace.

[0.3.0]: https://github.com/findit-studio/mediadecode/releases/tag/mediadecode-ffmpeg-v0.3.0

## [0.2.0] - 2026-05-15

Tracks `mediadecode` 0.2.0. The pixel-vocabulary types
(`PixelFormat`, color enums, frame primitives) now live in the
`videoframe` crate and are re-exported through `mediadecode`; this
release adapts the FFmpeg boundary to the new `PixelFormat::Unknown(u32)`
shape and updates the type aliases the crate re-exports.

### Changed (BREAKING)

- **`PixelFormat::Unknown` shape**: re-exported `PixelFormat` is now
  `Unknown(u32)` (tuple variant) instead of the prior unit variant
  — see [`mediadecode` 0.2.0](../mediadecode/CHANGELOG.md#020---2026-05-15).
- **FFmpeg boundary fallback** now preserves the raw `AVPixelFormat`
  identifier through `PixelFormat::Unknown(raw as u32)` instead of
  collapsing to a bare `Unknown`. Round-trips losslessly via
  `PixelFormat::{from_u32, to_u32}`.
- **Type aliases reshape**: `VideoFrame`, `AudioFrame`,
  `SubtitleFrame`, `VideoPacket`, `AudioPacket`, `SubtitlePacket`
  inherit the upstream `PixelFormat` shape change. Downstream
  callers matching on `Unknown` in destination frames need to
  switch to `Unknown(_)`.
- **`Error` enum variants** are now newtype-tuple form wrapping
  payload structs (matches the convention in
  [`videoframe`](https://crates.io/crates/videoframe)). Affected
  variants: `HwDeviceInitFailed`, `AllBackendsFailed`,
  `FallbackFailed`. Pure tuple variants (`Ffmpeg`, `NoCodec`,
  `BackendUnsupportedByCodec`) unchanged. Callers destructuring
  `Err(Error::AllBackendsFailed { attempts, .. })` must switch to
  `Err(Error::AllBackendsFailed(p))` and call `p.attempts()` /
  `p.unconsumed_packets()`. Owning-move paths for the rescued
  packets are preserved via `p.into_unconsumed_packets()` /
  `p.into_parts()`, so non-seekable callers can still relinquish
  the `Vec<Packet>` without cloning. The hand-written `Debug` that
  printed `[N packets]` (because `ffmpeg_next::Packet` has no
  `Debug`) now lives on the payload structs.
  All three new variants also carry `#[from]`, joining `Ffmpeg`
  which already had it — so `impl From<HwDeviceInitFailed> for Error`,
  `impl From<AllBackendsFailed> for Error`, and
  `impl From<FallbackFailed> for Error` are auto-generated, and
  helpers returning `Result<_, HwDeviceInitFailed>` etc. can be
  `?`-propagated into `Result<_, Error>` directly.

### Changed

- **`mediadecode` dep**: bumped to `0.2`.
- Boundary mapping in `pixel_format_from_ffmpeg` and the
  side-data conversion paths updated to the new
  `PixelFormat::Unknown(u32)` shape (17 fallback / assertion /
  default-frame sites across `mediadecode-ffmpeg` and
  `mediadecode-webcodecs`).

### Added

- **`Debug` impl for `Frame`** — manual `core::fmt::Debug` impl
  showing dimensions / format so the only public type previously
  without `Debug` is now printable.
  Closes [issue #4 — finding 2](https://github.com/findit-studio/mediadecode/issues/4).
- **`#[must_use]`** on every consuming `with_*` builder method
  across the crate's public surface.
  Closes [issue #4 — finding 3](https://github.com/findit-studio/mediadecode/issues/4).

[0.1.0]: https://github.com/findit-studio/mediadecode/releases/tag/mediadecode-ffmpeg-v0.1.0
[0.2.0]: https://github.com/findit-studio/mediadecode/releases/tag/mediadecode-ffmpeg-v0.2.0

## [0.1.0] - 2026-05-09

Initial public release.

### Added

- **Adapter type.** `Ffmpeg` zero-sized type implementing
  `mediadecode::adapter::VideoAdapter`, `AudioAdapter`, and
  `SubtitleAdapter`.
- **Buffer.** `FfmpegBuffer` — zero-copy refcounted view over an
  `AVBufferRef`, with `empty` / `from_packet` / `from_plane`
  constructors and panic-free `try_*` counterparts.
- **Video decoder.** `FfmpegVideoStreamDecoder` mirrors
  `ffmpeg::decoder::Video`'s `send_packet` / `receive_frame` shape and
  auto-probes the host's HW backends — VideoToolbox on Apple,
  VAAPI / CUDA on Linux, D3D11VA / CUDA on Windows — falling through
  to a software decoder when none open. `open_with(_, _, Backend::…)`
  pins a specific backend (no probe).
- **Audio decoder.** `FfmpegAudioStreamDecoder` over
  `ffmpeg::decoder::Audio`, producing zero-copy `AudioFrame`s.
- **Subtitle decoder.** `FfmpegSubtitleStreamDecoder` over the legacy
  synchronous `ffmpeg::decoder::Subtitle::decode` API, bridged into the
  trait's `send_packet` / `receive_frame` shape.
- **Type aliases.** `VideoPacket`, `AudioPacket`, `SubtitlePacket`,
  `VideoFrame`, `AudioFrame`, `SubtitleFrame` — pre-parameterized with
  this crate's adapter, buffer, and extras types.
- **Boundary helpers.** `video_packet_from_ffmpeg`,
  `audio_packet_from_ffmpeg`, `subtitle_packet_from_ffmpeg` — convert a
  borrowed `ffmpeg::Packet` into the matching `mediadecode` packet
  without copying the compressed payload. Empty-frame builders
  `empty_video_frame`, `empty_audio_frame`, `empty_subtitle_frame`
  produce well-formed destinations for `receive_frame`.
- **Recovery.** `VideoDecodeError::AllBackendsFailed { unconsumed_packets, .. }`
  carries any packets the decoder had already accepted from the
  demuxer when every backend is exhausted, so non-seekable callers
  (live streams, pipes, network sources) can replay them through their
  own software decoder without re-demuxing.

### Safety

The FFmpeg FFI surface is hardened against malformed or
version-skewed decoder output:

- All bindgen enum reads go through `addr_of!` + `read_unaligned` to
  avoid creating invalid Rust enum values from raw memory.
- `AVFrameSideDataType` values are mapped through an explicit
  whitelist of known `AV_FRAME_DATA_*` constants — never `transmute`d.
- `CStr::from_ptr` calls are replaced with a bounded
  `bounded_cstr_bytes` helper that searches at most
  `SUBTITLE_MAX_TEXT_BYTES_PER_RECT + 1` bytes for a NUL terminator.
- Signed counts (`AVFrame.nb_side_data`, `AVSubtitle.num_rects`, …)
  are clamped to non-negative values before any `as usize` cast,
  preventing OOB walks under corrupt input.
- Side-data and subtitle conversions enforce caps on entries and total
  bytes (`SIDE_DATA_MAX_ENTRIES`, `SIDE_DATA_MAX_TOTAL_BYTES`,
  `HW_COPY_SIDE_DATA_MAX_*`, `SUBTITLE_MAX_*`).
- `send_packet` consumes the demuxer packet only after the probe
  rescue records it, so a non-seekable caller can rebuild the input
  stream from `unconsumed_packets` on `AllBackendsFailed`.
- `cpu_frame_bytes` sizes against the underlying `AVBufferRef.size`
  rather than `linesize × plane_height_for(AVFrame.height)`, so
  cropped or heavily aligned streams report correct byte counts.

