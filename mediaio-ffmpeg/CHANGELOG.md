# Changelog

All notable changes to the [`mediaio-ffmpeg`](https://crates.io/crates/mediaio-ffmpeg)
crate are documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this crate adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
The FFmpeg-dependent crates — `mediaio-ffmpeg`, `mediadecode-ffmpeg`,
`mediaencode-ffmpeg` and `mediaio` — release in lockstep; every other
crate moves only when it changes.

## [Unreleased]

### Added

- **The write crossing**: `output::Output`, libavformat's muxing
  context owned in Rust over a caller's `Write + Seek`, for
  `mediaencode-ffmpeg`'s muxer. It takes no pointer from a caller, so
  nothing in it is `unsafe`; `contracts` gains "Writing a container" and
  "`AVOutputFormat`" for what it keeps with libavformat: the context
  freed before its I/O context, the call order kept at run time
  (`OutputError::OutOfOrder`), packets copied in on the copy road,
  parameters rebuilt from a `CodecTicket`, ids crossing as integers.
- **`writer_guard::GuardedWriter`**: a caller's sink behind the reader
  guard's panic latch. A panic inside it becomes a latched I/O error,
  named by the context (`OutputError::SinkPanic`), and a sink that
  panicked is never called again.
- Both guards reduce an error the stream returns to its OS code or its
  kind inside their `catch_unwind`, dropping its payload there:
  ffmpeg-next drops a returned error inside its `extern "C"` callback,
  where a payload whose `Drop` panics aborted the process. That panic is
  now latched and named like any other.
- `Output::into_sink` hands the sink back only when every byte reached
  it: a write the sink failed, which the I/O context keeps and writes
  nothing after, is read there after the last flush and reported
  (`OutputError::SinkWrite`), whatever the sink's own flush answers.
- A seek the sink fails is kept by the writer guard
  (`writer_guard::SeekRecord`, the third handle `GuardedWriter::new`
  answers), which libavformat does not do: a muxer going back to patch
  its header may ignore the answer and write on. The guard writes and
  seeks nothing after it, and every `Output` call that reached
  libavformat — the header, a packet, the trailer, `into_sink` — names
  it once libavformat returns, whatever libavformat answered
  (`OutputError::SinkSeek`), after a panic and before libavformat's own
  error.
- `CodecId::mime_type` — the descriptor's first MIME type — and the
  `PNG`, `BMP`, `TTF`, `OTF` and `TEXT` ids.
- What libavformat's packet path reads of a stream, for a muxer that
  keeps each packet from a rule libavformat would apply only after
  taking it: `Output::stream_codec` answers a stream's `StreamCodec` — its
  codec, video delay, sample rate, channels, frame size, the samples its
  codec fixes in each frame, block alignment, initial padding and
  extradata — once `add_stream` made it; `Output::shifts_early_times`
  whether libavformat moves every stream's times for an early first one;
  and `Output::set_automatic_filters` turns its automatic bitstream
  filters on or off — on by default, as libavformat's own — so that a
  caller who wants every packet to reach the muxer as handed over can
  have it (`OutputCall::AutomaticFilters`).
- `CodecId::intra_only` — whether libavformat takes every packet of the
  codec for a key frame — and `CodecId::bits_per_sample`, with the
  `LCEVC`, `EVC`, `APV`, `WAVPACK`, `TRUEHD` and `TTML` ids; the
  big-endian `PCM_S24BE`, `PCM_S32BE`, `PCM_F32BE` and `PCM_F64BE`, and
  `ATRAC3`, `COOK`, `RA_288`, `SIPR`, `RV10`, `RV20` and `RV30`.
- `extras::packet_side_data_name`: libavcodec's name for a packet
  side-data type, asked as the integer it is on the wire, or `None`
  where this build names no such type.

## [0.21.0] - 2026-10-24

Released in lockstep with `mediadecode-ffmpeg` 0.21.0.

### Fixed

- **`try_clone_packet` copies a packet with no payload with none**, its
  properties and side data alone. `av_packet_ref` and
  `av_packet_make_writable` gave the copy of a side-data-only packet a
  buffer of padding and data with no size, which `avcodec_send_packet`
  refuses (`AVERROR(EINVAL)`, `libavcodec/decode.c` 742–743 in FFmpeg
  9.0.1) where it takes the source: `mediadecode-ffmpeg`'s hardware probe,
  which records such copies, replayed one into the next backend and failed
  it.

### Added

- **`options`: how a decoder decodes, beside what it may spend.**
  `DecoderOptions` carries libavcodec's three `AVDiscard` settings —
  `skip_frame`, `skip_loop_filter` and `skip_idct` — each a `Discard`,
  FFmpeg's seven values (`Nothing`, `Default`, `NonReference`,
  `Bidirectional`, `NonIntra`, `NonKey`, `All`), written and read as
  FFmpeg's option constants (`none` … `all`, any case on reading;
  `ParseDiscardError` for any other name). The defaults are libavcodec's.
  `DecoderLimits` stays what a decoder may spend; these are decode
  behaviour, and `mediadecode-ffmpeg` writes them into every video
  `AVCodecContext` a decoder opened with them opens, through each video
  decoder's open request (`VideoStreamDecoderOpen`, `VideoDecoderOpen`).
  Which levels a decoder reads is the decoder's: a level is a request,
  never a promise that no picture comes.

## [0.20.0] - 2026-10-23

### Changed

- **BREAKING: FFmpeg 9 is the floor, for this crate and every crate of the
  family built on it** — `mediadecode-ffmpeg`, `mediaencode-ffmpeg`, and
  `mediaio`'s `ffmpeg` feature. The build script refuses an FFmpeg older
  than 9 by name, before any of the family's code is compiled against it:
  it reads the linked release from `ffmpeg-sys-next`'s `links` metadata
  (`DEP_FFMPEG_FFMPEG_9_0`, which `ffmpeg-sys-next` sets from libavcodec
  63.1, FFmpeg 9.0's), names the release it found instead, and says where
  to point the build. This crate now depends on `ffmpeg-sys-next` directly
  for that metadata alone, enabling no feature of it.

### Added

- **`DemuxerName`: a demuxer the linked FFmpeg registers, by its whole
  name** — an `AVInputFormat.name` exactly as libavformat spells it, the
  comma list where FFmpeg registers one demuxer for a family
  (`mov,mp4,m4a,3gp,3g2,mj2`), never a word of it. It is made only from
  a name the linked libavformat registers (`DemuxerName::registered`, and
  `FromStr`), found with `av_demuxer_iterate`, and borrows libavformat's
  own static text. Any other name is refused by name with
  `UnregisteredDemuxer`, which gives the whole name to use where the
  refused one is a word of it. `ContainerFormat::is` tells whether a
  session's format is that demuxer.

## [0.19.0] - 2026-10-16

### Added

- **The FFmpeg base of the mediaio family**: the crossings between
  FFmpeg's memory and the `mediaspine` vocabulary, moved out of
  `mediadecode-ffmpeg` (which re-exports them): the carriers and their
  operations, packet and frame conversion, codec tickets and
  identifiers, containers, limits, the AVFrame crossing, the allocator
  judge, and the FFmpeg resampler behind `resample`, with `Ffmpeg` and
  its adapter impls.
- **The crossing's contract.** Every function whose soundness rests on
  something its caller proves — a pointer's liveness, an extent, a claim
  about who else can write a buffer, a geometry a frame must keep — is
  an `unsafe fn` with a `# Safety` section; what a consumer holds is safe
  to use without any of that.
- `ParametersError`, what the codec-parameter copies answer, and
  `CrossingError`, what the AVFrame allocators, `Frame::empty` and the
  resampler's `Resample` arm carry.
- `try_clone_packet`: a deep copy of a packet that checks both
  `av_packet_ref` and `av_packet_make_writable`.
- **No safe function reads the storage of an `ffmpeg_next` frame,
  packet or subtitle.** `ffmpeg_next`'s safe API builds objects whose
  bytes were never written — `frame::Video::new` and `frame::Audio::new`
  (`av_frame_get_buffer`), `Packet::new` and `Packet::grow` — so every
  conversion from one is an `unsafe fn` whose caller vouches for the new
  `contracts` section, "Every exported byte initialised", and for the
  object's own: `convert::video_frame_from`, `image_frame_from`,
  `audio_frame_from` and `subtitle_frame_from`, and the thirteen
  `boundary` packet conversions (`*_packet_from_ffmpeg`, `*_in`,
  `owned_*`, `attachment_packet_from_ffmpeg`). Their named refusals stay.
  A call without `unsafe` does not compile (`compile_fail,E0133`).
  `try_clone_packet` joins them: its `av_packet_ref` and
  `av_packet_make_writable` copy every payload byte of the packet they
  are given.
- **A custom channel map is refused unread on the borrowed road.**
  `convert::audio_frame_from` answers
  `ConvertError::UnverifiableChannelMap` for a frame whose layout is a
  custom map, before reading it: its contract does not ask for the map.
  A frame libavcodec produced converts through the raw entries, whose
  `# Safety` states the map's extent.
- **The allocator judge has one door.** `judge_buffer` is private, and
  the plumbing's `ffi::install_allocator_judge` installs it together
  with the `CallbackState` it reads, under a contract that states the
  state's lifetime, that its pointer must not be invalidated (no `Box`
  it came from may move afterwards), and the threads the callbacks may
  run on.
- **`contracts`**: what the `unsafe` crossings read through the
  pointers they are handed, stated once per FFmpeg object. Every
  publicly reachable `unsafe fn` names, in its own `# Safety`, each
  pointer it dereferences and links the section for its object; a
  wrapper forwards its worker's contract whole.
- **The raw extern declarations are crate-private.** `ffi::c_shims` is
  `pub(crate)`; what `mediadecode-ffmpeg` needs goes through two
  documented doors, the safe `ffi::find_decoder_raw` and
  `ffi::visit_hwframe_download_formats`.
- **The borrowed roads refuse what they cannot bound, before reading.**
  `convert::audio_frame_from` answers `ConvertError::NullFrame` for a
  null-backed frame before reading any field, and
  `convert::subtitle_frame_from` refuses a bitmap rect carrying a plane
  (`ConvertError::UnverifiableBitmap`): an `AVSubtitleRect` records no
  plane size, and `ffmpeg_next`'s safe `BitmapMut::set_height` moves
  the height alone. Bitmaps convert through the raw entries, whose
  caller vouches for the extent.
- **`ffi::visit_hwframe_download_formats` frees its list on unwind.**
  The list libavutil allocates is owned by a guard from the moment it
  exists, so a visitor that panics no longer leaks it; on an FFmpeg
  error there is no list, and nothing is freed.
- **A side-data type is admitted by the linked library's word.** The
  write side refused every type at or above the bindings'
  `AV_PKT_DATA_NB`, though the read side carries a type a newer library
  hands it as the integer it is. It now asks the library it runs
  against — `av_packet_side_data_name`, redeclared over a plain `c_int`
  so no enum is formed — and refuses only what that library does not
  name. `UnknownSideData` carries the type alone; its `limit` is gone,
  since the bindings' count is no longer the judge.
- **A subtitle rect's fields are read only under its own kind.** The
  subtitle worker reads a rect's `type` first and alone, then a text
  rect's `text`, an ASS rect's `ass`, or a bitmap rect's plane, palette
  and geometry — never a field of another kind, which a producer need
  not have written. The `AVSubtitle` contract says so.
- **A decoder is handed a buffer by reference only where the library
  vouches for its padding.** libavcodec reads
  `AV_INPUT_BUFFER_PADDING_SIZE` bytes past every payload, and only a
  packet libavformat delivered — a demuxed packet, a stream's parked
  picture — comes with padding libavformat answers for. A caller's packet
  converted on the view lane claims nothing past its payload, whatever
  its buffer holds there — the claim is the provenance's
  (`PayloadProvenance`), never the buffer's — and its send copies it into
  a fresh allocation with the padding zeroed.
  The `AVPacket` contract states the padding clause for every road that
  hands a packet to libavcodec: the padding is zeroed or libavformat's
  own, and neither it nor the payload is written while the decoder may
  read them.
- `boundary::write_md_flags` is a safe fn: it writes one field of the
  `AVPacket` an `ffmpeg_next::Packet` keeps inline.
- A `test-support` feature (off by default): the fault-subprocess
  harness, the allocation cap and the callback-state hooks.
