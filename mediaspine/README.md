<div align="center">
<h1>mediaspine</h1>
</div>

The shared media vocabulary of the mediaio family: the types and traits
every reader, writer and backend in the family agrees on. It holds no
decoder, no encoder and no FFmpeg, and it builds with `no_std` and no
allocator.

| module | what it names |
|---|---|
| `frame` | `VideoFrame`, `AudioFrame`, `SubtitleFrame`, `ImageFrame`, `Plane`, `Dimensions`, `Rect` |
| `packet` | `VideoPacket`, `AudioPacket`, `SubtitlePacket`, `PacketFlags` |
| `subtitle` | the subtitle payloads: `SubtitlePayload`, `Text`, `Bitmap`, `BitmapRegion` |
| `rhythm` | the push rhythm's answers: `Sent::{Accepted, MustDrain}`, `Received::{Produced, NeedsInput, Ended}` |
| `demuxer` | the demux tier's types: `DemuxAdapter`, `TrackIndex`, `TrackKind`, `TrackParams`, `TrackInfo`, `Chapter`, `ContainerDuration`, `MeasuredEnd`, `DataPacket`, `AttachmentPacket`, `DemuxedPacket` |
| `track` | `TrackDescription`: what a muxer needs to open one output track, with the portable metadata (language; an attachment's file) and a lossless conversion from `TrackInfo` |
| `adapter` | the per-kind adapter traits: `VideoAdapter`, `AudioAdapter`, `SubtitleAdapter`, `ImageAdapter` |
| `resampler` | the resampler face: `AudioResampler`, `AdapterAudioFrame` |
| `hdr` | HDR metadata: `MasteringDisplay`, `ContentLightLevel` |
| `nal` | what an H.264 or HEVC packet's NAL units say: `NalStream` reads whether nothing references the packet's picture (`Verdict`, `KeptReason`), for `PacketFlags::DISPOSABLE` in any container |
| `pixel_format`, `color`, `cfa` | re-exports from `mediaframe` |

The time primitives (`Timebase`, `Timestamp`, `TimeRange`) are
re-exported from `mediatime`.

The faces that use this vocabulary live in their own crates:
`mediadecode` holds the read faces (the `Demuxer` session and the
decoder traits), and the FFmpeg base `mediaio-ffmpeg` holds the
crossings between FFmpeg's memory and these types.

## Features

| feature | default | what it adds |
|---|---|---|
| `std` | yes | the `std` tier of `mediatime` |
| `alloc` | no | the allocating tier, for builds without `std` |
| `serde` | no | `Serialize` / `Deserialize` where the vocabulary carries them |
| `arbitrary`, `quickcheck` | no | generators for property tests and fuzzing |

With no features the crate is `core`-only.

#### License

`mediaspine` is under the terms of both the MIT license and the
Apache License (Version 2.0).

See [LICENSE-APACHE](LICENSE-APACHE), [LICENSE-MIT](LICENSE-MIT) for details.

Copyright (c) 2026 FinDIT Studio authors.
