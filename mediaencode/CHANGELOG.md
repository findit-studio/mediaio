# Changelog

All notable changes to the [`mediaencode`](https://crates.io/crates/mediaencode)
crate are documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this crate adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
The FFmpeg-dependent crates — `mediaio-ffmpeg`, `mediadecode-ffmpeg`,
`mediaencode-ffmpeg` and `mediaio` — release in lockstep; every other
crate moves only when it changes.

## [Unreleased]

### Added

- **A packet is presented no earlier than it is decoded**:
  `MuxRefusal::PtsBeforeDts` and `check_presentation_order`, the rule
  every container keeps over the times as it stores them — libavformat
  compares the counts it is handed, and refuses only after taking the
  packet. A backend applies it to its stored representation, not to the
  instants offered, names it first and hands the packet back.

## [0.19.0] - 2026-10-16

### Added

- **The write faces of the mediaio family**, `no_std` with no allocator
  required. `VideoEncoder` and `AudioEncoder` take raw frames and give
  packets in the push rhythm the decoders keep (`Sent`, `Received`),
  with `send_eof` draining to `Ended`, `flush` answering
  `Flushed::Reset` or `Flushed::Refused`, `force_keyframe` and
  `frame_size`. An audio encoder takes every frame at its frame size
  and the stream's last, which may be shorter, through
  `send_final_frame`, which ends the stream as `send_eof` does. `Muxer`
  takes the per-kind track packets a demuxer delivers, with
  `write_packet` routing a `DemuxedPacket` to the write for its kind,
  and `finish(self)` ends the session. A muxer is given its attachments
  when it is opened, in its track table — each attachment track's
  `TrackDescription` carries its one file — and never as a packet.
- `EncoderSettings` with `VideoParams` / `AudioParams`, `RateControl`,
  `Profile`, `Level` and `Gop`.
- The write errors: `EncodeError` / `EncodeRefusal` and `MuxError` /
  `MuxRefusal` — the refusals every backend names the same way, beside
  its own faults. `EncodeRefusal` names a frame of the wrong
  dimensions, format, sample rate, channel layout or size, and one sent
  after the end; `MuxRefusal` names an unknown track, a packet of the
  wrong kind, a missing decode timestamp, one that goes back, and an
  attachment offered as a packet. The decode-order rule is
  `check_decode_order`, one function for every backend: within a track
  decode timestamps may repeat — two subtitle cues or data events at one
  instant are ordinary input — and may not go back; a backend whose
  container needs them strictly increasing refuses a repeat by its own
  name. That name is the backend's: `Muxer::Refusal` is an open
  vocabulary each backend gives its containers' own rules, refused in
  `MuxError::Backend`, while `MuxRefusal` stays the closed portable one.
  A refused mux write hands its packet back untouched, in
  `MuxError::Refused` or `MuxError::Backend` — each write's error names
  its own packet type — and `MuxError::map_packet` re-wraps it, as a
  remux routing every kind through one error type does.
