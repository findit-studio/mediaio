# Changelog

All notable changes to the [`mediaio-ffmpeg`](https://crates.io/crates/mediaio-ffmpeg)
crate are documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this crate adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
The family — `mediadecode`, `mediadecode-ffmpeg`, `mediadecode-webcodecs`,
`mediaencode`, `mediaencode-ffmpeg` and `mediaio` — releases in lockstep.

## [0.19.1] - unreleased

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
  Under `skip_frame` = `all` those decoders run no hardware probe: no
  picture comes to commit one on. Under `nonintra` and `nonkey`, where a
  stream may give none either, an end with no picture is the stream's.

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
