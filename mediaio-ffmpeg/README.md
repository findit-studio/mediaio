<div align="center">
<h1>mediaio-ffmpeg</h1>
</div>

The FFmpeg base of the mediaio family: the one place that links
libavcodec and libavformat (through
[`ffmpeg-next`](https://crates.io/crates/ffmpeg-next)) and the
**crossings** between FFmpeg's memory and the shared media vocabulary.

**FFmpeg 9 is the floor**, for this crate and every crate of the family
built on it: no older release is supported, and the build script refuses
one by name. It reads the linked release from `ffmpeg-sys-next`'s `links`
metadata (`DEP_FFMPEG_FFMPEG_9_0`, set from libavcodec 63.1, FFmpeg 9.0's).

| here | not here |
|---|---|
| the carriers (`FfmpegBytes`, `FfmpegBuffer`, the `Owned` / `View` lanes) and the operations that fill them | the decoders and the demuxer — `mediadecode-ffmpeg` |
| packet and frame conversion in both directions (`boundary`, `convert`) | the push-style decoder sessions and their hardware probing |
| the muxing context over a caller's sink, behind the writer guard's panic latch | the muxer and its container rules — `mediaencode-ffmpeg` |
| `CodecTicket`, `CodecId`, `ContainerFormat`, `SampleFormat`, the channel-layout mapping | |
| the adapter type `Ffmpeg` and its adapter-trait impls | |
| `DecoderLimits`, `DemuxLimits`, `FrameLimits`, `PacketLimits` | |
| the FFmpeg resampler (feature `resample`) | |
| the allocator judge's callbacks and their shared state | |

## The crossing's safety model

Every operation whose soundness rests on something only its caller can
establish — a raw pointer's liveness, an extent, a claim about who else
can write a buffer, a geometry a frame must keep, that every byte an
object exports was written — is an `unsafe fn` with a `# Safety` section
saying what the caller proves, against the sections of
`mediaio_ffmpeg::contracts`. That includes every conversion from an
`ffmpeg_next` frame, packet or subtitle: `ffmpeg_next`'s safe API builds
ones whose bytes were never written (`frame::Video::new`, `Packet::new`),
so no safe function here reads their storage. The machines in
`mediadecode-ffmpeg` call them with their proofs written beside each
call. What a consumer holds — a packet, a frame, a carrier — is safe to
use without any of that.

The write side takes no pointer from a caller, so none of it is
`unsafe`: the muxing context owns every pointer it hands libavformat,
copies each packet into an allocation of its own before
`av_interleaved_write_frame` takes it, keeps libavformat's call order at
run time, and drives the caller's sink under a guard that turns a panic
into a latched I/O error instead of an unwind into C
(`contracts` § "Writing a container").

## Features

| feature | default | what it adds |
|---|---|---|
| `std` | yes | the vocabulary's `std` tier |
| `resample` | yes | `FfmpegResampler` over `libswresample` |
| `test-support` | no | the fault-subprocess harness, the FFmpeg allocation cap and the callback-state hooks, for the lanes of this crate and of crates built on it |

#### License

`mediaio-ffmpeg` is under the terms of both the MIT license and the
Apache License (Version 2.0).

See [LICENSE-APACHE](LICENSE-APACHE), [LICENSE-MIT](LICENSE-MIT) for details.

Copyright (c) 2026 FinDIT Studio authors.
