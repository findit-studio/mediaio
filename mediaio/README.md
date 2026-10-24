<div align="center">
<h1>mediaio</h1>
</div>

The mediaio family in one dependency. Each feature brings one member,
re-exported under a short name:

| feature | default | module | crate |
|---|---|---|---|
| `core` | yes | `mediaio::spine` | `mediaspine` — the shared vocabulary: frames, packets, subtitles, the push rhythm, the demuxer's types, the adapter traits |
| `decode` | yes | `mediaio::decode` | `mediadecode` — the read faces: the decoders and the `Demuxer` |
| `encode` | yes | `mediaio::encode` | `mediaencode` — the write faces: the encoders and the `Muxer` |
| `timeline` | yes | `mediaio::timeline` | `mediaio-timeline` — the editing timeline as data |
| `ffmpeg` | no | `mediaio::ffmpeg::{base, decode, encode}` | `mediaio-ffmpeg`, `mediadecode-ffmpeg`, `mediaencode-ffmpeg` — the FFmpeg adapters |
| `webcodecs` | no | `mediaio::webcodecs` | `mediadecode-webcodecs` — the WebCodecs read adapter (`wasm32`) |
| `time` | no | `mediaio::time` | `mediatime` |
| `frame` | no | `mediaio::frame` | `mediaframe` |
| `render` | no | — | reserved for the renderer a later release adds |

The default build is pure Rust: nothing links FFmpeg until `ffmpeg` is
enabled. Enabling it links libavcodec and libavformat, which must be
**FFmpeg 9** or newer — the family's floor, refused by name below it; GPL
encoders stay
closed even then, behind `mediaencode-ffmpeg`'s own `gpl` feature (and
linking a GPL FFmpeg makes the binary GPL in any case — see that
crate's documentation).

Every member is brought in at its `std` tier. A `no_std` build depends
on `mediaspine`, `mediadecode` or `mediaencode` directly.

#### License

`mediaio` is under the terms of both the MIT license and the
Apache License (Version 2.0).

See [LICENSE-APACHE](LICENSE-APACHE), [LICENSE-MIT](LICENSE-MIT) for details.

Copyright (c) 2026 FinDIT Studio authors.
