<div align="center">
<h1>mediadecode-ffmpeg</h1>
</div>
<div align="center">

FFmpeg adapter for the [`mediadecode`](../mediadecode) abstraction
layer, built on top of
[`ffmpeg-next`](https://crates.io/crates/ffmpeg-next).

[<img alt="github" src="https://img.shields.io/badge/github-findit--studio/mediaio-8da0cb?style=for-the-badge&logo=Github" height="22">][Github-url]
<img alt="LoC" src="https://img.shields.io/endpoint?url=https%3A%2F%2Fgist.githubusercontent.com%2Fal8n%2F327b2a8aef9003246e45c6e47fe63937%2Fraw%2Fmediadecode-ffmpeg" height="22">
[<img alt="Build" src="https://img.shields.io/github/actions/workflow/status/findit-studio/mediaio/ci-ffmpeg.yml?logo=Github-Actions&style=for-the-badge" height="22">][CI-url]
[<img alt="codecov" src="https://img.shields.io/codecov/c/gh/findit-studio/mediaio?style=for-the-badge&logo=codecov" height="22">][codecov-url]

[<img alt="docs.rs" src="https://img.shields.io/badge/docs.rs-mediadecode--ffmpeg-66c2a5?style=for-the-badge&labelColor=555555&logo=data:image/svg+xml;base64,PHN2ZyByb2xlPSJpbWciIHhtbG5zPSJodHRwOi8vd3d3LnczLm9yZy8yMDAwL3N2ZyIgdmlld0JveD0iMCAwIDUxMiA1MTIiPjxwYXRoIGZpbGw9IiNmNWY1ZjUiIGQ9Ik00ODguNiAyNTAuMkwzOTIgMjE0VjEwNS41YzAtMTUtOS4zLTI4LjQtMjMuNC0zMy43bC0xMDAtMzcuNWMtOC4xLTMuMS0xNy4xLTMuMS0yNS4zIDBsLTEwMCAzNy41Yy0xNC4xIDUuMy0yMy40IDE4LjctMjMuNCAzMy43VjIxNGwtOTYuNiAzNi4yQzkuMyAyNTUuNSAwIDI2OC45IDAgMjgzLjlWMzk0YzAgMTMuNiA3LjcgMjYuMSAxOS45IDMyLjJsMTAwIDUwYzEwLjEgNS4xIDIyLjEgNS4xIDMyLjIgMGwxMDMuOS01MiAxMDMuOSA1MmMxMC4xIDUuMSAyMi4xIDUuMSAzMi4yIDBsMTAwLTUwYzEyLjItNi4xIDE5LjktMTguNiAxOS45LTMyLjJWMjgzLjljMC0xNS05LjMtMjguNC0yMy40LTMzLjd6TTM1OCAyMTQuOGwtODUgMzEuOXYtNjguMmw4NS0zN3Y3My4zek0xNTQgMTA0LjFsMTAyLTM4LjIgMTAyIDM4LjJ2LjZsLTEwMiA0MS40LTEwMi00MS40di0uNnptODQgMjkxLjFsLTg1IDQyLjV2LTc5LjFsODUtMzguOHY3NS40em0wLTExMmwtMTAyIDQxLjQtMTAyLTQxLjR2LS42bDEwMi0zOC4yIDEwMiAzOC4ydi42em0yNDAgMTEybC04NSA0Mi41di03OS4xbDg1LTM4Ljh2NzUuNHptMC0xMTJsLTEwMiA0MS40LTEwMi00MS40di0uNmwxMDItMzguMiAxMDIgMzguMnYuNnoiPjwvcGF0aD48L3N2Zz4K" height="20">][doc-url]
[<img alt="crates.io" src="https://img.shields.io/crates/v/mediadecode-ffmpeg?style=for-the-badge&logo=data:image/svg+xml;base64,PD94bWwgdmVyc2lvbj0iMS4wIiBlbmNvZGluZz0iaXNvLTg4NTktMSI/Pg0KPCEtLSBHZW5lcmF0b3I6IEFkb2JlIElsbHVzdHJhdG9yIDE5LjAuMCwgU1ZHIEV4cG9ydCBQbHVnLUluIC4gU1ZHIFZlcnNpb246IDYuMDAgQnVpbGQgMCkgIC0tPg0KPHN2ZyB2ZXJzaW9uPSIxLjEiIGlkPSJMYXllcl8xIiB4bWxucz0iaHR0cDovL3d3dy53My5vcmcvMjAwMC9zdmciIHhtbG5zOnhsaW5rPSJodHRwOi8vd3d3LnczLm9yZy8xOTk5L3hsaW5rIiB4PSIwcHgiIHk9IjBweCINCgkgdmlld0JveD0iMCAwIDUxMiA1MTIiIHhtbDpzcGFjZT0icHJlc2VydmUiPg0KPGc+DQoJPGc+DQoJCTxwYXRoIGQ9Ik0yNTYsMEwzMS41MjgsMTEyLjIzNnYyODcuNTI4TDI1Niw1MTJsMjI0LjQ3Mi0xMTIuMjM2VjExMi4yMzZMMjU2LDB6IE0yMzQuMjc3LDQ1Mi41NjRMNzQuOTc0LDM3Mi45MTNWMTYwLjgxDQoJCQlsMTU5LjMwMyw3OS42NTFWNDUyLjU2NHogTTEwMS44MjYsMTI1LjY2MkwyNTYsNDguNTc2bDE1NC4xNzQsNzcuMDg3TDI1NiwyMDIuNzQ5TDEwMS44MjYsMTI1LjY2MnogTTQzNy4wMjYsMzcyLjkxMw0KCQkJbC0xNTkuMzAzLDc5LjY1MVYyNDAuNDYxbDE1OS4zMDMtNzkuNjUxVjM3Mi45MTN6IiBmaWxsPSIjRkZGIi8+DQoJPC9nPg0KPC9nPg0KPGc+DQo8L2c+DQo8Zz4NCjwvZz4NCjxnPg0KPC9nPg0KPGc+DQo8L2c+DQo8Zz4NCjwvZz4NCjxnPg0KPC9nPg0KPGc+DQo8L2c+DQo8Zz4NCjwvZz4NCjxnPg0KPC9nPg0KPGc+DQo8L2c+DQo8Zz4NCjwvZz4NCjxnPg0KPC9nPg0KPGc+DQo8L2c+DQo8Zz4NCjwvZz4NCjxnPg0KPC9nPg0KPC9zdmc+DQo=" height="22">][crates-url]
[<img alt="crates.io" src="https://img.shields.io/crates/d/mediadecode-ffmpeg?color=critical&logo=data:image/svg+xml;base64,PD94bWwgdmVyc2lvbj0iMS4wIiBzdGFuZGFsb25lPSJubyI/PjwhRE9DVFlQRSBzdmcgUFVCTElDICItLy9XM0MvL0RURCBTVkcgMS4xLy9FTiIgImh0dHA6Ly93d3cudzMub3JnL0dyYXBoaWNzL1NWRy8xLjEvRFREL3N2ZzExLmR0ZCI+PHN2ZyB0PSIxNjQ1MTE3MzMyOTU5IiBjbGFzcz0iaWNvbiIgdmlld0JveD0iMCAwIDEwMjQgMTAyNCIgdmVyc2lvbj0iMS4xIiB4bWxucz0iaHR0cDovL3d3dy53My5vcmcvMjAwMC9zdmciIHAtaWQ9IjM0MjEiIGRhdGEtc3BtLWFuY2hvci1pZD0iYTMxM3guNzc4MTA2OS4wLmkzIiB3aWR0aD0iNDgiIGhlaWdodD0iNDgiIHhtbG5zOnhsaW5rPSJodHRwOi8vd3d3LnczLm9yZy8xOTk5L3hsaW5rIj48ZGVmcz48c3R5bGUgdHlwZT0idGV4dC9jc3MiPjwvc3R5bGU+PC9kZWZzPjxwYXRoIGQ9Ik00NjkuMzEyIDU3MC4yNHYtMjU2aDg1LjM3NnYyNTZoMTI4TDUxMiA3NTYuMjg4IDM0MS4zMTIgNTcwLjI0aDEyOHpNMTAyNCA2NDAuMTI4QzEwMjQgNzgyLjkxMiA5MTkuODcyIDg5NiA3ODcuNjQ4IDg5NmgtNTEyQzEyMy45MDQgODk2IDAgNzYxLjYgMCA1OTcuNTA0IDAgNDUxLjk2OCA5NC42NTYgMzMxLjUyIDIyNi40MzIgMzAyLjk3NiAyODQuMTYgMTk1LjQ1NiAzOTEuODA4IDEyOCA1MTIgMTI4YzE1Mi4zMiAwIDI4Mi4xMTIgMTA4LjQxNiAzMjMuMzkyIDI2MS4xMkM5NDEuODg4IDQxMy40NCAxMDI0IDUxOS4wNCAxMDI0IDY0MC4xOTJ6IG0tMjU5LjItMjA1LjMxMmMtMjQuNDQ4LTEyOS4wMjQtMTI4Ljg5Ni0yMjIuNzItMjUyLjgtMjIyLjcyLTk3LjI4IDAtMTgzLjA0IDU3LjM0NC0yMjQuNjQgMTQ3LjQ1NmwtOS4yOCAyMC4yMjQtMjAuOTI4IDIuOTQ0Yy0xMDMuMzYgMTQuNC0xNzguMzY4IDEwNC4zMi0xNzguMzY4IDIxNC43MiAwIDExNy45NTIgODguODMyIDIxNC40IDE5Ni45MjggMjE0LjRoNTEyYzg4LjMyIDAgMTU3LjUwNC03NS4xMzYgMTU3LjUwNC0xNzEuNzEyIDAtODguMDY0LTY1LjkyLTE2NC45MjgtMTQ0Ljk2LTE3MS43NzZsLTI5LjUwNC0yLjU2LTUuODg4LTMwLjk3NnoiIGZpbGw9IiNmZmZmZmYiIHAtaWQ9IjM0MjIiIGRhdGEtc3BtLWFuY2hvci1pZD0iYTMxM3guNzc4MTA2OS4wLmkwIiBjbGFzcz0iIj48L3BhdGg+PC9zdmc+&style=for-the-badge" height="22">][crates-url]
<img alt="license" src="https://img.shields.io/badge/License-Apache%202.0/MIT-blue.svg?style=for-the-badge&fontColor=white&logoColor=f5c076&logo=data:image/svg+xml;base64,PCFET0NUWVBFIHN2ZyBQVUJMSUMgIi0vL1czQy8vRFREIFNWRyAxLjEvL0VOIiAiaHR0cDovL3d3dy53My5vcmcvR3JhcGhpY3MvU1ZHLzEuMS9EVEQvc3ZnMTEuZHRkIj4KDTwhLS0gVXBsb2FkZWQgdG86IFNWRyBSZXBvLCB3d3cuc3ZncmVwby5jb20sIFRyYW5zZm9ybWVkIGJ5OiBTVkcgUmVwbyBNaXhlciBUb29scyAtLT4KPHN2ZyBmaWxsPSIjZmZmZmZmIiBoZWlnaHQ9IjgwMHB4IiB3aWR0aD0iODAwcHgiIHZlcnNpb249IjEuMSIgaWQ9IkNhcGFfMSIgeG1sbnM9Imh0dHA6Ly93d3cudzMub3JnLzIwMDAvc3ZnIiB4bWxuczp4bGluaz0iaHR0cDovL3d3dy53My5vcmcvMTk5OS94bGluayIgdmlld0JveD0iMCAwIDI3Ni43MTUgMjc2LjcxNSIgeG1sOnNwYWNlPSJwcmVzZXJ2ZSIgc3Ryb2tlPSIjZmZmZmZmIj4KDTxnIGlkPSJTVkdSZXBvX2JnQ2FycmllciIgc3Ryb2tlLXdpZHRoPSIwIi8+Cg08ZyBpZD0iU1ZHUmVwb190cmFjZXJDYXJyaWVyIiBzdHJva2UtbGluZWNhcD0icm91bmQiIHN0cm9rZS1saW5lam9pbj0icm91bmQiLz4KDTxnIGlkPSJTVkdSZXBvX2ljb25DYXJyaWVyIj4gPGc+IDxwYXRoIGQ9Ik0xMzguMzU3LDBDNjIuMDY2LDAsMCw2Mi4wNjYsMCwxMzguMzU3czYyLjA2NiwxMzguMzU3LDEzOC4zNTcsMTM4LjM1N3MxMzguMzU3LTYyLjA2NiwxMzguMzU3LTEzOC4zNTcgUzIxNC42NDgsMCwxMzguMzU3LDB6IE0xMzguMzU3LDI1OC43MTVDNzEuOTkyLDI1OC43MTUsMTgsMjA0LjcyMywxOCwxMzguMzU3UzcxLjk5MiwxOCwxMzguMzU3LDE4IHMxMjAuMzU3LDUzLjk5MiwxMjAuMzU3LDEyMC4zNTdTMjA0LjcyMywyNTguNzE1LDEzOC4zNTcsMjU4LjcxNXoiLz4gPHBhdGggZD0iTTE5NC43OTgsMTYwLjkwM2MtNC4xODgtMi42NzctOS43NTMtMS40NTQtMTIuNDMyLDIuNzMyYy04LjY5NCwxMy41OTMtMjMuNTAzLDIxLjcwOC0zOS42MTQsMjEuNzA4IGMtMjUuOTA4LDAtNDYuOTg1LTIxLjA3OC00Ni45ODUtNDYuOTg2czIxLjA3Ny00Ni45ODYsNDYuOTg1LTQ2Ljk4NmMxNS42MzMsMCwzMC4yLDcuNzQ3LDM4Ljk2OCwyMC43MjMgYzIuNzgyLDQuMTE3LDguMzc1LDUuMjAxLDEyLjQ5NiwyLjQxOGM0LjExOC0yLjc4Miw1LjIwMS04LjM3NywyLjQxOC0xMi40OTZjLTEyLjExOC0xNy45MzctMzIuMjYyLTI4LjY0NS01My44ODItMjguNjQ1IGMtMzUuODMzLDAtNjQuOTg1LDI5LjE1Mi02NC45ODUsNjQuOTg2czI5LjE1MiA2NC45ODYsNjQuOTg1LDY0Ljk4NmMyMi4yODEsMCw0Mi43NTktMTEuMjE4LDU0Ljc3OC0zMC4wMDkgQzIwMC4yMDgsMTY5LjE0NywxOTguOTg1LDE2My41ODIsMTk0Ljc5OCwxNjAuOTAzeiIvPiA8L2c+IDwvZz4KDTwvc3ZnPg==" height="22">

</div>

Implements `mediadecode`'s `VideoAdapter` / `AudioAdapter` /
`SubtitleAdapter` / `ImageAdapter` traits, the matching push-style
`*StreamDecoder` traits, the one-shot `ImageDecoder`, and `Demuxer`.

Every byte a frame or packet carries is **copied once, at the FFmpeg
boundary, into an `FfmpegBytes`** — `mediadecode` 0.9's carrier
amputation contract. A delivered frame is owned, `Send + Sync`, and
cheap to clone (a refcount bump); it holds nothing of libavcodec's
open, so it can cross a channel, be read from several threads, and
outlive the decoder that produced it. Through 0.8 the planes were
refcounted views into `AVBufferRef` behind an `FfmpegBuffer` type,
which meant every consumer inherited an FFmpeg lifetime it could not
see. That type is gone.

`FfmpegVideoStreamDecoder` mirrors the `send_packet` / `receive_frame`
shape of `ffmpeg::decoder::Video`, auto-probes the host's HW backends,
and falls through to a software decoder when none takes the stream
before its first picture. Audio and
subtitles use parallel `FfmpegAudioStreamDecoder` /
`FfmpegSubtitleStreamDecoder` types.

## Backends

`FfmpegVideoStreamDecoder::open` walks this probe order, opening the
first backend that accepts the stream:

| Target              | Probe order                       |
| ------------------- | --------------------------------- |
| macOS / iOS / tvOS  | VideoToolbox → software           |
| Linux               | VAAPI → CUDA → software           |
| Windows             | D3D11VA → CUDA → software         |
| other               | software                          |

Output frames are CPU-side, downloaded with `av_hwframe_transfer_data`
(NV12 for 8-bit, P010/P012/P016/P210/P212/P216/P410/P412/P416 for
10/12/16-bit). Pixel-format conversion is intentionally out of scope
— downstream
[`colconv`](https://github.com/findit-studio/colconv) handles it.

The probe keeps every packet it consumes until the first picture comes
out. When no backend takes the stream by then, `open` (that is,
`DecodePath::Auto`) replays those packets into the software decoder, so
nothing is lost; `DecodePath::AnyHardware` and a `DecodePath::Hardware`
pin report `VideoDecodeError::Decode(Error::AllBackendsFailed(p))`
instead, carrying them (`p.unconsumed_packets()` /
`p.into_unconsumed_packets()`) — so non-seekable callers (live streams,
pipes, network sources) can replay them through a software decoder of
their own without re-demuxing.

After the first picture nothing changes the road, on any path, and
nothing is classified. A decoder failure, `VideoDecodeError::Decode`, is
that picture's own error, reported as the decoder minted it, and nothing
of it is remembered, so the next call reaches libavcodec. FFmpeg has no
reliable signal that a hardware session is gone (`AVERROR_EXTERNAL`, for
one, also answers a single picture), and whether a hardware session
recovers is FFmpeg's, not this crate's. FFmpeg 9.0.1 does not always
recover one. After a VideoToolbox restart that fails, every picture
fails the same way until a new parameter set re-arms the restart, and
`flush` does not change that: it drops the pictures and references the
codec holds, and it rebuilds no hardware session. `VideoDecoder`'s
documentation cites the FFmpeg lines.

A `VideoDecodeError::Convert` is the wrapper's own failure, not the
decoder's: `FfmpegVideoStreamDecoder` could not convert a decoded
picture into a frame (a frame ceiling, a pixel format or plane layout it
cannot carry, an allocation). One that failed on an allocation parks the
picture: the next `receive_frame` converts it again, and until one
delivers it, `send_packet` and `send_eof` answer `Sent::MustDrain`
without reaching libavcodec. That is the wrapper's back pressure, not a
failure.

So a caller that sees a hardware session's decoder failures persist
rebuilds: it opens a session on `DecodePath::Software` from the same
parameters and feeds it forward. What the new session can be fed depends
on the road the failure came from:

- **`send_packet`**: the failure names the packet in hand, and the new
  session is given that packet.
- **`receive_frame`**: the failure may concern a packet accepted earlier.
  FFmpeg decouples input from output and may hold several pictures
  (`libavcodec/avcodec.h` 90–139 in FFmpeg 9.0.1), so no packet is
  named: the new session is given the next packet, and the caller
  accepts the gap.
- **`send_eof`**: the end has no packet to give a new session. What the
  hardware session still held is recoverable only from packets kept from
  before, or by a seek.

When to rebuild is the caller's policy, not the decoder's, because it is
the caller that sees the failures on the three roads, the packets' key
flags and what it has delivered. What a policy counts is the hardware
session's decoder failures; a `Convert` error is reported, not counted.
The usage example below carries the simplest one.

## Usage

A file's video track, decoded under the simplest policy for a hardware
session the caller stops trusting. It counts the hardware session's
decoder failures on all three roads, and only a delivered picture ends
the count. At `FAILURES_BEFORE_SOFTWARE` failures in a row it opens a
session on `DecodePath::Software` from the same parameters and feeds it
forward: the packet in hand when the failure came from `send_packet`,
otherwise what comes next, the next packet or the end. It keeps no
packets and replays nothing, so every picture the new session delivers
is delivered as it is. A `Convert` error is reported, not counted: the
example returns it, as it returns every error it does not handle.

```rust,no_run
use ffmpeg_next as ffmpeg;
use ffmpeg::{codec, format, media};
use mediadecode::{Received, Sent, Timebase, decoder::VideoStreamDecoder};
use mediadecode_ffmpeg::{
  DecodePath, DecoderLimits, Error, FfmpegVideoStreamDecoder, PacketLimits, VideoDecodeError,
  VideoFrame, VideoPacket, empty_video_frame, video_packet_from_ffmpeg_in,
};

type BoxError = Box<dyn std::error::Error>;

/// This caller's threshold, not the decoder's: how many decoder failures
/// in a row, with no picture delivered between them, it takes from a
/// hardware session before it rebuilds on software.
const FAILURES_BEFORE_SOFTWARE: u32 = 3;

fn main() -> Result<(), BoxError> {
  ffmpeg::init()?;

  let path = std::env::args().nth(1).expect("usage: <input-file>");
  let mut input = format::input(&path)?;
  let stream = input.streams().best(media::Type::Video).unwrap();
  let stream_index = stream.index();
  let time_base = Timebase::new(
    stream.time_base().numerator(),
    std::num::NonZeroI32::new(stream.time_base().denominator()).unwrap(),
  );
  // A copy of its own: a software session opened later opens from it.
  let parameters = stream.parameters().clone();

  let mut track = Track::open(parameters, time_base)?;
  for (s, av_packet) in input.packets() {
    if s.index() != stream_index { continue; }
    // `Ok(None)` is an empty packet; an `Err` is a payload that is
    // there and could not be referenced, which is never silently
    // skipped.
    // **By value.** The bare names are the view lane, where a packet's
    // payload is a window into libavformat's own buffer — so the source
    // is handed over rather than lent. (The borrowing doors,
    // `owned_*`, are the owned lane: they copy, so the packet stays
    // yours.)
    //
    // SAFETY: `packets()` reads each packet with `av_read_frame`, so
    // libavformat wrote its payload; it is consumed here.
    let Some(pkt) =
      (unsafe { video_packet_from_ffmpeg_in(av_packet, time_base, PacketLimits::default()) })?
    else { continue };
    track.push(&pkt)?;
  }
  track.finish()
}

/// A video track decoded under this caller's policy for a hardware
/// session it stops trusting.
///
/// The policy feeds forward and does nothing else: it keeps no packets,
/// replays nothing and matches no pictures. That costs the pictures from
/// a failure to the next keyframe: a new session holds no reference
/// pictures, so libavcodec drops or conceals what comes before one, and
/// what the hardware session still held goes with it. A caller that
/// cannot afford that gap keeps the packets since the last clean keyframe
/// and replays them with a picture identity of its own; that bookkeeping
/// is the caller's design and is not shown here, because a doc example
/// cannot carry it correctly.
struct Track {
  parameters: codec::Parameters,
  time_base: Timebase,
  decoder: FfmpegVideoStreamDecoder,
  frame: VideoFrame,
  /// The hardware session's decoder failures since the last picture
  /// delivered.
  failures_in_a_row: u32,
}

impl Track {
  fn open(parameters: codec::Parameters, time_base: Timebase) -> Result<Self, BoxError> {
    // Probes HW backends in order and falls back to software when none
    // takes the stream before its first picture — so an error here means
    // software could not open it either.
    let decoder =
      FfmpegVideoStreamDecoder::open(parameters.clone(), time_base, DecoderLimits::default())?;
    Ok(Self {
      parameters,
      time_base,
      decoder,
      frame: empty_video_frame(),
      failures_in_a_row: 0,
    })
  }

  /// The `send_packet` road. A failure here names this packet.
  fn push(&mut self, pkt: &VideoPacket) -> Result<(), BoxError> {
    loop {
      match self.decoder.send_packet(pkt) {
        Ok(Sent::Accepted) => return self.drain(),
        // Back pressure, not a failure: nothing was consumed, so drain
        // and offer this same packet again. The old idiom — submit
        // twice and treat the second failure as real — is what this
        // arm replaces.
        Ok(Sent::MustDrain) => self.drain()?,
        Err(VideoDecodeError::Decode(e)) => {
          // A software session opened for this failure is given this
          // packet; otherwise the packet is behind us.
          if self.failed(e)? {
            continue;
          }
          return self.drain();
        }
        // `VideoDecodeError` is `#[non_exhaustive]`: a fault this code
        // has never heard of takes the generic road, which is the right
        // handling for one.
        Err(e) => return Err(e.into()),
      }
    }
  }

  /// The `send_eof` road, then the tail. The end has no packet: a
  /// software session opened for a failure here is given the end and
  /// nothing else, so what the hardware session still held is lost.
  fn finish(&mut self) -> Result<(), BoxError> {
    loop {
      match self.decoder.send_eof() {
        Ok(Sent::Accepted) => return self.drain(),
        Ok(Sent::MustDrain) => self.drain()?,
        Err(VideoDecodeError::Decode(e)) => {
          if self.failed(e)? {
            continue;
          }
          // The end stays refused: what is ready is drained, and what
          // the session still holds is lost with it.
          return self.drain();
        }
        Err(e) => return Err(e.into()),
      }
    }
  }

  /// The `receive_frame` road: every picture the session has ready, up
  /// to `NeedsInput`, or to `Ended` once the end is taken. `Received` is
  /// a state, so the loop stops on one rather than on whatever the last
  /// error was.
  fn drain(&mut self) -> Result<(), BoxError> {
    loop {
      match self.decoder.receive_frame(&mut self.frame) {
        Ok(Received::Produced) => self.deliver(),
        Ok(Received::NeedsInput | Received::Ended) => return Ok(()),
        // A failure here may concern a packet accepted earlier: FFmpeg
        // decouples input from output and may hold several pictures. No
        // packet is named, so a software session opened for it is given
        // the next packet.
        Err(VideoDecodeError::Decode(e)) => {
          if self.failed(e)? {
            return Ok(());
          }
        }
        // Not counted: a `Convert` error is the wrapper's own, a decoded
        // picture it could not convert into a frame, and not a decoder
        // failure. It is reported, as is anything this code has never
        // heard of.
        Err(e) => return Err(e.into()),
      }
    }
  }

  /// A picture, delivered to the rest of the program as it is.
  fn deliver(&mut self) {
    // The count ends here, at a delivered picture, and nowhere else.
    self.failures_in_a_row = 0;
    // self.frame.pixel_format(), .width(), .height(), .planes() — view
    // carriers: read them here and drop. A frame held is a pool slot
    // held. Use the `Owned*` family when a frame has to outlive this.
  }

  /// A decoder failure on any of the three roads. On hardware it counts,
  /// and at `FAILURES_BEFORE_SOFTWARE` the session is replaced by one on
  /// `DecodePath::Software`, opened from the same parameters. Answers
  /// whether it was, so the road can feed the new session.
  fn failed(&mut self, e: Error) -> Result<bool, BoxError> {
    eprintln!("decode failure: {e}");
    // Software is where this policy ends: its failures are reported,
    // and the stream goes on.
    if !self.decoder.is_hardware() {
      return Ok(false);
    }
    self.failures_in_a_row += 1;
    if self.failures_in_a_row < FAILURES_BEFORE_SOFTWARE {
      return Ok(false);
    }
    self.decoder = FfmpegVideoStreamDecoder::open_as(
      self.parameters.clone(),
      self.time_base,
      DecoderLimits::default(),
      DecodePath::Software,
    )?;
    Ok(true)
  }
}
```

Audio and subtitle decoding share the shape — see
[`examples/decode_via_trait.rs`](examples/decode_via_trait.rs) and
[`tests/audio_subtitle_via_trait.rs`](tests/audio_subtitle_via_trait.rs)
for end-to-end demuxer-driven runs that cover all three streams.

## Public surface map

- **Decoders**: `FfmpegVideoStreamDecoder`,
  `FfmpegAudioStreamDecoder`, `FfmpegSubtitleStreamDecoder`. Plus
  their error types: `VideoDecodeError`, `AudioDecodeError`,
  `SubtitleDecodeError`.
- **Decode path**: `DecodePath` and `FfmpegVideoStreamDecoder::open_as`
  — `Auto` (what `open` does: probe hardware, fall back to software
  before the first picture), `AnyHardware` (the same probe, never
  software), `Software`, or the pin `Hardware(Backend)`. After the first
  picture no path changes its decoder, and a decoder failure is the
  picture's own error; when to stop trusting a hardware session is the
  caller's policy. `is_hardware()` / `is_software()` stay the live
  reading of where a session is.
- **Demuxer**: `FfmpegDemuxer` — `mediadecode`'s `Demuxer` over
  `libavformat`, opened from a path (`open`) or from any
  `Read + Seek` byte source through a custom `AVIOContext`
  (`open_reader`), or with every optional of the open named in a
  `DemuxerOpen` (`open_from`, `open_reader_from`). Plus `DemuxError`, and
  `format()` → `ContainerFormat`
  — libavformat's own identification of the bytes (`"matroska,webm"`,
  `"mov,mp4,m4a,3gp,3g2,mj2"`), read from the container rather than
  guessed from a path. A video packet of an H.264 or HEVC track is
  marked `PacketFlags::DISPOSABLE` where its NAL units prove that
  nothing references it (`mediadecode::nal`) — a non-reference picture,
  or an HEVC sub-layer non-reference picture at the highest temporal
  sub-layer, with nothing beside it a later picture needs — and only
  there: where a track is read, its reading decides, the container's own
  mark (MP4's `sdtp`) with it. **A container is read only where the
  caller declares its demuxer's inputs plaintext**
  (`DemuxerOpen::plaintext_demuxers`, each a `DemuxerName` the linked
  FFmpeg registers, by its whole name: `"mov,mp4,m4a,3gp,3g2,mj2"`).
  FFmpeg cannot prove a container's payload plaintext — an MP4 track
  protected under OMA DRM 2's PDCF reaches the demuxer as ciphertext,
  with nothing public to say so — so by default the raw `h264` and `hevc`
  elementary streams alone are read, plaintext by construction, and every
  other demuxer is container-only: its packets carry the container's mark
  alone, as FFmpeg delivers it, which it sets in `mov` alone and never on
  an H.264 track. **The declaration is the caller's assertion, and its
  risk:** an encrypted input's ciphertext, read as a bitstream, can be
  marked falsely. Inside a declared demuxer every encryption signal
  FFmpeg gives — `ENCRYPTION_INFO` on a packet, `pssh`'s
  `ENCRYPTION_INIT_INFO` or Matroska's `enc_key_id` on a stream — still
  ends a reading. A Sony XAVC original, which carries no `sdtp`, has two
  pictures in three marked where `mov` is declared; a consumer that keeps
  only reference pictures drops them before they are decoded.
- **Codec identity**: `CodecId` — the number FFmpeg keys on, with
  `name()` / `long_name()` reading libavcodec's own descriptor table
  for the word beside it (`"h264"`, `"aac"`), so a stored track row can
  cross into a typed codec vocabulary. The number stays the identity;
  an id libavcodec has no descriptor for has no name, rather than a
  `"unknown_codec"` sentinel.
- **Resampler** (`resample` feature, on by default): `FfmpegResampler`
  — `mediadecode`'s `AudioResampler` over `swresample`, built from two
  explicit `ResampleSpec`s (the source read off a track or off the
  opened decoder, the target the caller's). Plus `ResampleError`,
  which carries faults and the `SourceChanged` mid-stream refusal —
  "needs more input" and "the tail is finished" are
  `mediadecode::Received` states out of `receive_frame`, not error
  variants. Disabling the feature drops the type and the
  `libswresample` link along with it.
- **`FfmpegImageDecoder`**: the one-shot `ImageDecoder` — cover art in,
  `ImageFrame` out. Opened from an attachment track's codec parameters,
  which the demuxer's cover-art reclassification retains in full. The
  picture's EXIF orientation comes back typed on `ImageFrameExtra`,
  read off the display matrix libavcodec emits for it.
- **Type aliases**: `VideoPacket`, `AudioPacket`, `SubtitlePacket`,
  `DataPacket`, `AttachmentPacket`, `DemuxedPacket`, `VideoFrame`,
  `AudioFrame`, `SubtitleFrame`, `ImageFrame`, `TrackInfo`,
  `TrackParams` — the `mediadecode` generic types pre-parameterized
  with this crate's adapter / carrier / extras, so you don't have to
  spell them out.
- **Carrier**: `FfmpegBytes` — owned, `Send + Sync`, `AsRef<[u8]>`,
  cloning by refcount. Opaque over an `Arc<[u8]>`, because the storage
  gains a pooled strategy later
  ([#35](https://github.com/findit-studio/mediadecode/issues/35)) and a
  consumer must not have to recompile for it. Construct one with
  `FfmpegBytes::copy_from_slice` / `::empty` when feeding a packet back
  into a decoder.
- **Boundary helpers**: `video_packet_from_ffmpeg`,
  `audio_packet_from_ffmpeg`, `subtitle_packet_from_ffmpeg` — convert a
  borrowed `ffmpeg::Packet` into the matching `mediadecode` packet,
  copying the compressed payload out. Their `*_in` siblings
  (`video_packet_from_ffmpeg_in`, `audio_packet_from_ffmpeg_in`,
  `subtitle_packet_from_ffmpeg_in`, `data_packet_from_ffmpeg_in`) take
  the stream's timebase, so the produced `Timestamp` says what its
  ticks mean instead of carrying the 1/1 placeholder an `AVPacket`
  alone leaves you with. `attachment_packet_from_ffmpeg` wraps a
  cover-art packet, which has no timestamps to carry. Every one is
  `unsafe`: a borrow of an `ffmpeg_next` packet cannot vouch that its
  payload was ever written (`Packet::new` leaves it unwritten), so the
  caller does — a packet `av_read_frame` filled always was. The demuxer,
  `FfmpegDemuxer`, converts its own packets and needs no `unsafe`.
- **Empty-frame builders**: `empty_video_frame`, `empty_audio_frame`,
  `empty_subtitle_frame` — well-formed destinations for `receive_frame`.

## Running tests and benches

The fixture-gated tests and the benchmark expect real media files,
named by five environment variables. The unit tests run
unconditionally; the gated ones are `#[ignore]`d, so `--ignored`
is what opts into them.

| Variable | Feeds |
| --- | --- |
| `HWDECODE_SAMPLE_VIDEO` | `tests/decode.rs`, `tests/hw_smoke.rs`, `benches/decode.rs`, and the three `decoder::tests` backend cases |
| `MEDIADECODE_SAMPLE_VIDEO` | `tests/decode_via_trait.rs` |
| `MEDIADECODE_SAMPLE_AUDIO` | the audio-through-trait case (any container with an audio track) |
| `MEDIADECODE_SAMPLE_SUBTITLE` | the subtitle-through-trait case (needs a container that really carries a subtitle track) |
| `MEDIADECODE_FX3_SAMPLE` | the Sony FX3 H.264 High 4:2:2 10-bit probe-era HW→SW fallback case |

```sh
HWDECODE_SAMPLE_VIDEO=/path/to/clip.mp4 cargo test --test hw_smoke -- --ignored
HWDECODE_SAMPLE_VIDEO=/path/to/clip.mp4 cargo bench

# The whole fixture-gated set at once.
HWDECODE_SAMPLE_VIDEO=/path/to/clip.mp4 \
MEDIADECODE_SAMPLE_VIDEO=/path/to/clip.mp4 \
MEDIADECODE_SAMPLE_AUDIO=/path/to/clip.mp4 \
MEDIADECODE_SAMPLE_SUBTITLE=/path/to/subtitled.mkv \
MEDIADECODE_FX3_SAMPLE=/path/to/12_sony_fx3_xavc.mp4 \
  cargo test --all-features -- --ignored
```

A variable left unset is not always a quiet skip: the FX3 case prints
a notice and returns, but the trait cases panic on a missing path
once `--ignored` has opted into them.

## Build requirements

- A system **FFmpeg 9** or newer, linkable via `pkg-config`; no older
  release is supported. The build refuses an older one by name:
  `mediaio-ffmpeg`'s build script reads the linked release from
  `ffmpeg-sys-next`'s `links` metadata (`DEP_FFMPEG_FFMPEG_9_0`). Tested
  against 9.0.1. Verify with `ffmpeg -hwaccels` that your build
  has the backends you expect compiled in
  (e.g. `videotoolbox` on macOS, `vaapi` / `cuda` on Linux,
  `d3d11va` / `cuda` on Windows).
- Rust ≥ **1.95**, edition 2024.

## License

`mediadecode-ffmpeg` is under the terms of both the MIT license and the
Apache License (Version 2.0).

See [LICENSE-APACHE](LICENSE-APACHE), [LICENSE-MIT](LICENSE-MIT) for details.

Copyright (c) 2026 FinDIT Studio authors.

[Github-url]: https://github.com/findit-studio/mediaio
[CI-url]: https://github.com/findit-studio/mediaio/actions/workflows/ci-ffmpeg.yml
[codecov-url]: https://app.codecov.io/gh/findit-studio/mediaio/
[doc-url]: https://docs.rs/mediadecode-ffmpeg
[crates-url]: https://crates.io/crates/mediadecode-ffmpeg
