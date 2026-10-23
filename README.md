<div align="center">
<h1>mediaio</h1>
</div>
<div align="center">

Media I/O in Rust: one shared vocabulary, faces to read and write media
through, and adapters that implement them.

[<img alt="github" src="https://img.shields.io/badge/github-findit--studio/mediaio-8da0cb?style=for-the-badge&logo=Github" height="22">][Github-url]
<img alt="LoC" src="https://img.shields.io/endpoint?url=https%3A%2F%2Fgist.githubusercontent.com%2Fal8n%2F327b2a8aef9003246e45c6e47fe63937%2Fraw%2Fmediaio-root" height="22">
<img alt="license" src="https://img.shields.io/badge/License-Apache%202.0/MIT-blue.svg?style=for-the-badge&fontColor=white&logoColor=f5c076&logo=data:image/svg+xml;base64,PCFET0NUWVBFIHN2ZyBQVUJMSUMgIi0vL1czQy8vRFREIFNWRyAxLjEvL0VOIiAiaHR0cDovL3d3dy53My5vcmcvR3JhcGhpY3MvU1ZHLzEuMS9EVEQvc3ZnMTEuZHRkIj4KDTwhLS0gVXBsb2FkZWQgdG86IFNWRyBSZXBvLCB3d3cuc3ZncmVwby5jb20sIFRyYW5zZm9ybWVkIGJ5OiBTVkcgUmVwbyBNaXhlciBUb29scyAtLT4KPHN2ZyBmaWxsPSIjZmZmZmZmIiBoZWlnaHQ9IjgwMHB4IiB3aWR0aD0iODAwcHgiIHZlcnNpb249IjEuMSIgaWQ9IkNhcGFfMSIgeG1sbnM9Imh0dHA6Ly93d3cudzMub3JnLzIwMDAvc3ZnIiB4bWxuczp4bGluaz0iaHR0cDovL3d3dy53My5vcmcvMTk5OS94bGluayIgdmlld0JveD0iMCAwIDI3Ni43MTUgMjc2LjcxNSIgeG1sOnNwYWNlPSJwcmVzZXJ2ZSIgc3Ryb2tlPSIjZmZmZmZmIj4KDTxnIGlkPSJTVkdSZXBvX2JnQ2FycmllciIgc3Ryb2tlLXdpZHRoPSIwIi8+Cg08ZyBpZD0iU1ZHUmVwb190cmFjZXJDYXJyaWVyIiBzdHJva2UtbGluZWNhcD0icm91bmQiIHN0cm9rZS1saW5lam9pbj0icm91bmQiLz4KDTxnIGlkPSJTVkdSZXBvX2ljb25DYXJyaWVyIj4gPGc+IDxwYXRoIGQ9Ik0xMzguMzU3LDBDNjIuMDY2LDAsMCw2Mi4wNjYsMCwxMzguMzU3czYyLjA2NiwxMzguMzU3LDEzOC4zNTcsMTM4LjM1N3MxMzguMzU3LTYyLjA2NiwxMzguMzU3LTEzOC4zNTcgUzIxNC42NDgsMCwxMzguMzU3LDB6IE0xMzguMzU3LDI1OC43MTVDNzEuOTkyLDI1OC43MTUsMTgsMjA0LjcyMywxOCwxMzguMzU3UzcxLjk5MiwxOCwxMzguMzU3LDE4IHMxMjAuMzU3LDUzLjk5MiwxMjAuMzU3LDEyMC4zNTdTMjA0LjcyMywyNTguNzE1LDEzOC4zNTcsMjU4LjcxNXoiLz4gPHBhdGggZD0iTTE5NC43OTgsMTYwLjkwM2MtNC4xODgtMi42NzctOS43NTMtMS40NTQtMTIuNDMyLDIuNzMyYy04LjY5NCwxMy41OTMtMjMuNTAzLDIxLjcwOC0zOS42MTQsMjEuNzA4IGMtMjUuOTA4LDAtNDYuOTg1LTIxLjA3OC00Ni45ODUtNDYuOTg2czIxLjA3Ny00Ni45ODYsNDYuOTg1LTQ2Ljk4NmMxNS42MzMsMCwzMC4yLDcuNzQ3LDM4Ljk2OCwyMC43MjMgYzIuNzgyLDQuMTE3LDguMzc1LDUuMjAxLDEyLjQ5NiwyLjQxOGM0LjExOC0yLjc4Miw1LjIwMS04LjM3NywyLjQxOC0xMi40OTZjLTEyLjExOC0xNy45MzctMzIuMjYyLTI4LjY0NS01My44ODItMjguNjQ1IGMtMzUuODMzLDAtNjQuOTg1LDI5LjE1Mi02NC45ODUsNjQuOTg2czI5LjE1MiA2NC45ODYsNjQuOTg1LDY0Ljk4NmMyMi4yODEsMCw0Mi43NTktMTEuMjE4LDU0Ljc3OC0zMC4wMDkgQzIwMC4yMDgsMTY5LjE0NywxOTguOTg1LDE2My41ODIsMTk0Ljc5OCwxNjAuOTAzeiIvPiA8L2c+IDwvZz4KDTwvc3ZnPg==" height="22">

</div>

## The family

| Crate | Kind | What it holds |
| ----- | ---- | ------------- |
| [`mediaspine`](mediaspine/README.md) | vocabulary, `no_std` | frames, packets, subtitles, the push rhythm (`Sent`, `Received`), the demuxer's types, `TrackDescription`, the adapter traits, the resampler face, HDR metadata |
| [`mediadecode`](mediadecode/README.md) | read faces, `no_std` | the decoder traits, the frame sources, the `Demuxer` session, their async mirrors; re-exports the vocabulary under its old paths |
| [`mediaencode`](mediaencode/README.md) | write faces, `no_std` | the encoders, the `Muxer`, the encoder settings, the write errors |
| [`mediaio-ffmpeg`](mediaio-ffmpeg/README.md) | the FFmpeg base | the crossings between FFmpeg's memory and the vocabulary: carriers, packet and frame conversion, codec tickets, limits, the FFmpeg resampler |
| [`mediadecode-ffmpeg`](mediadecode-ffmpeg/README.md) | read adapter | the FFmpeg decoders and demuxer, with hardware-acceleration probing |
| [`mediaencode-ffmpeg`](mediaencode-ffmpeg/README.md) | write adapter | the encoder-by-name catalogue and its GPL gate; the muxer and the encoders follow |
| [`mediadecode-webcodecs`](mediadecode-webcodecs/README.md) | read adapter, `wasm32` | the WebCodecs decoders |
| [`mediaio-timeline`](mediaio-timeline/README.md) | pure model | the editing timeline as data, with OpenTimelineIO export and its exact read-back |
| [`mediaio`](mediaio/README.md) | umbrella | the family in one dependency, by feature; pure Rust by default |

The FFmpeg-dependent crates — `mediaio-ffmpeg`, `mediadecode-ffmpeg`,
`mediaencode-ffmpeg` and `mediaio` — release in lockstep; every other
crate moves only when it changes.

**FFmpeg 9 is the floor** of the FFmpeg crates — `mediaio-ffmpeg`,
`mediadecode-ffmpeg`, `mediaencode-ffmpeg` and `mediaio`'s `ffmpeg`
feature: no older release is supported, and the build refuses one by name.

The pixel-format and color vocabulary (`PixelFormat`, the H.273 color
enums, `BayerPattern`, the frame primitives) is sourced from
[`mediaframe`](https://crates.io/crates/mediaframe), and the time
primitives from [`mediatime`](https://crates.io/crates/mediatime), so
the wider ecosystem shares one definition of each.

See each crate's README for its API surface, usage, and build
requirements.

[Github-url]: https://github.com/findit-studio/mediaio
