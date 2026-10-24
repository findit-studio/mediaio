# Changelog

All notable changes to the [`mediaspine`](https://crates.io/crates/mediaspine)
crate are documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this crate adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
Its versions are its own: they do not move with the rest of the family's.

## [0.1.2] - unreleased

### Added

- **The declared disposition on the track row and its description**:
  `TrackInfo::disposition` and `TrackDescription::disposition` (with
  their `with_` and `set_` builders) carry the container's
  `AV_DISPOSITION_*` bits as `mediaframe`'s `TrackDisposition`,
  re-exported from `mediaspine::demuxer`, whole — an unnamed bit
  included. `from_track_info` copies it. `into_parts` keeps its shape: the
  disposition, `Copy`, is read with `disposition()` before the split.

## [0.1.1] - 2026-10-23

### Added

- **`nal`: what an H.264 or HEVC packet's NAL units say about its
  picture.** `NalStream` reads each of a track's packets (`read`) and
  answers `Verdict::Disposable` only where the bitstream proves that
  nothing references the packet's picture and nothing beside it is needed
  later:
  - H.264: at least one slice, every slice of `nal_ref_idc` 0, none an
    IDR, and nothing beside them but access unit delimiters, filler data
    and SEI units of picture timing and filler payload alone — and only
    while every sequence parameter set FFmpeg's decoder may read codes
    frames only (`frame_mbs_only_flag` 1), since FFmpeg joins a frame's two
    fields across packets, lets `frame_num` jump nowhere
    (`gaps_in_frame_num_value_allowed_flag` 0) and fixes the reorder depth
    (`bitstream_restriction_flag`, a `max_num_reorder_frames` of 15 at
    most), in each of the three readings it may adopt of one: before one is
    read, and from the first that does not, nothing is marked
    (`frames_only` says whether fields are let in). Not a picture of intra
    slices alone, at which FFmpeg's frame threads reset the film grain
    seed's offset; and, since the opening or the last seek, only after a
    reference picture the decoder decodes and, for a picture holding an
    intra slice, a kept packet holding one;
  - HEVC: every slice a sub-layer non-reference type FFmpeg decodes
    (TRAIL_N, TSA_N, STSA_N, RADL_N, RASL_N) of layer 0 at the highest
    temporal sub-layer, learned from every SPS and VPS FFmpeg's splitter
    may read and from the record's `numTemporalLayers`, and nothing beside
    them but delimiters, filler data and SEI units of filler payload alone;
    not in the first packet after the opening, a seek or an end of
    sequence, which takes up FFmpeg's `eos`, while a recovery point (of
    any layer) may be pending, or while SEI state or a Dolby Vision RPU
    may be pending for the next picture decoded — left by an earlier
    packet, or by a configuration record: the one the decoder opens with,
    which a decoder opened afresh at a seek reads again, and one a packet
    carries, read before it. A packet that follows an HEVC record leaves
    pending whatever was pending before it, since a record that fails
    returns before the decoder reads its packet.
    Unlike FFmpeg's own `AVDISCARD_NONREF`, which skips sub-layer
    non-reference pictures at every sub-layer, it never marks one a higher
    sub-layer may reference.

  Parameter sets are learned from every packet a decoder reads, one the
  reading cannot frame included: that one in every framing a decoder may
  read it in. It reads plaintext only: a caller feeds it no encrypted
  payload, and none of the track's packets after one.

  The framing is FFmpeg's decoder's own state — whether lengths are read,
  and their size, kept apart — moved where FFmpeg moves it: by the
  configuration record at the opening (`new`) and mid-stream
  (`reconfigure`, and `read_with_record`, which takes a packet's record
  as the decoder takes it), by an H.264 packet that is an `avcC`, and by
  H.264's four-byte probe; a packet that moves it is kept. A seek after the
  framing moved (`seeked`), an `avcC` whose reading turns on whether a
  parameter set decodes, and a mid-stream `hvcC` of other than two-byte
  lengths leave the framing unknown, and nothing is marked. `LengthSize`
  is one to four bytes, as FFmpeg reads them.

  Anything else keeps the packet, with the reason (`KeptReason`): a
  reference or random-access picture, a parameter set, an SEI unit of any
  other message, or an end, an extension's NAL unit, a picture below the
  highest sub-layer or before any SPS, an H.264 picture that may be a
  field, where `frame_num` may jump or the reorder depth is not fixed, of
  intra slices alone, or before the reference picture or the intra slice
  it awaits; an HEVC packet a sequence may start at, or a picture while a
  recovery point or what an earlier packet left may be pending; a packet
  that does not parse — a length-framed unit holding a start code among
  them — a configuration record, a switch of framing, and a framing
  unknown or unset. `NalCodec` names the two codecs read; every other
  codec is unclassified.

### Changed

- **`PacketFlags::DISPOSABLE` is the container's mark or the
  bitstream's**: nothing references this picture. Its contract is
  unchanged — absent means unknown — and a demuxer that reads the
  bitstream sets it on more H.264 and HEVC packets than the container
  marks. Where a demuxer reads a track, the bitstream's verdict decides:
  the container's mark is cleared on every other verdict. Its promise is pixels: dropping a marked packet changes no later
  picture's pixels, and promises nothing of a later frame's best-effort
  timestamp, which a decoder may guess from every frame before; a frame's
  own pts is the exact one. A container's bitstream is read for it only
  where the caller declares its demuxer's inputs plaintext: by default the
  raw `h264` and `hevc` streams alone are read, since FFmpeg cannot prove a
  container's payload plaintext — an MP4 track under OMA DRM 2's PDCF
  reaches the demuxer as ciphertext with nothing to say so. The
  declaration is the caller's assertion, and its risk; inside a declared
  demuxer every encryption signal FFmpeg gives still ends a reading. The
  container's own mark reaches a packet only on a track libavformat does
  not parse as it reads: never on an H.264 track of MP4.

## [0.1.0] - 2026-10-16

### Added

- **The shared vocabulary of the mediaio family**, carved out of
  `mediadecode` 0.18 unchanged but for one name: `frame`, `packet`
  (`PacketFlags` with `DISPOSABLE`), `subtitle`, `rhythm`, the demuxer's
  types with `DemuxAdapter`, the adapter traits, the resampler face, and
  the `pixel_format`, `color` and `cfa` re-exports from `mediaframe`.
  `mediadecode` re-exports all of it under its old paths.
- **`Received::Produced`**, the answer that delivers output — a frame
  from a decoder or a resampler, a packet from an encoder. It was
  `Received::Frame` in `mediadecode`.
- **`hdr`**: `MasteringDisplay` and `ContentLightLevel`, lifted from
  `mediadecode-ffmpeg`'s extras so a write backend can take them too.
- **`track::TrackDescription`**: what a muxer needs to open one output
  track — the timebase, the codec and its parameters (`TrackParams`),
  optional extradata and the backend's side data, and the portable
  track metadata the read side states: the declared language, and for
  an attachment track its file (`AttachedFile` — the payload the
  demuxer delivered, its filename and MIME type). An attachment track
  carries exactly one file, by construction: `TrackDescription::attachment`
  takes one, and `TrackDescription::new` refuses an attachment's
  parameters by name (`AttachmentWithoutFile`).
- **`TrackDescription::from_track_info`**: the lossless conversion
  from a demuxed `TrackInfo` and the payload delivered for it, for a
  remux. It **borrows** the row, so a remux describes each track from
  the handle `Demuxer::tracks` hands out and the session keeps its rows;
  the portable fields are copied explicitly. What only the read side
  states (the library's duration, the backend's extras) stays on the
  row. A row that does not fit — an attachment without its payload, a
  payload for another kind, an attachment's identity claimed by another
  kind — is refused by name, the row's kind named and the payload and
  side data handed back (`TrackMismatch`, `MismatchReason`).
- **`TrackParams::duplicate`**: a field-by-field copy of a track's
  parameters, by name — the message-carrier law keeps `Clone` off the
  type, and a consumer that needs parameters of its own spells the copy
  out.
- **`DemuxAdapter::Text` is `Clone`**: a copy of `&'static str` or a
  refcount bump of a shared string, how the conversion takes a row's
  text from a row it borrows.
- `no_std` with no allocator required; `alloc`, `std`, `serde`,
  `arbitrary` and `quickcheck` as in `mediadecode`.
