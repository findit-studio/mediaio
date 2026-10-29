# Changelog

All notable changes to the [`mediaspine`](https://crates.io/crates/mediaspine)
crate are documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this crate adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
Its versions are its own: they do not move with the rest of the family's.

## [0.1.3] - unreleased

### Added

- **`nal`: interlaced H.264, read field by field.** Where a sequence
  parameter set lets a picture be a field (`frame_mbs_only_flag` 0),
  `NalStream` no longer keeps every packet. It reads each slice's
  `field_pic_flag` and `bottom_field_flag` as FFmpeg's decoder does: by the
  sequence parameter set the slice's picture parameter set was read on,
  from FFmpeg's parameter-set lists, each entry as far as the reading
  knows it. It marks a packet that holds a frame — an MBAFF frame among
  them — or both fields of a pair FFmpeg joins, while no first field may
  wait at it, and a pair only while no field may have gone unmatched. A
  pair split across packets, as FFmpeg's parser splits a raw or MPEG-TS
  stream's, is kept whole, both halves: dropping one moves the other's
  pairing, and the next frame's. Two slices' parameter sets are held to
  each other by what FFmpeg compares — a second field's picture parameter
  set to the active one's `sps_id`, `transform_8x8_mode` and sequence
  parameter set object, an object FFmpeg keeps only for a set read again
  byte for byte — never by their ids; and a picture whose sequence
  parameter set gives the decoder another size, bit depth, chroma format,
  sample aspect ratio or colour matrix is taken as FFmpeg takes it, its
  context set up again and flushed.
- `KeptReason::UnpairedField`, `KeptReason::PendingField` and
  `KeptReason::UnmatchedFields`: an interlaced packet kept for its fields.

### Changed

- `KeptReason::MaybeField` names, after a sequence parameter set that lets
  fields in, a slice the reading cannot read for its field, where it named
  every picture of `nal_ref_idc` 0.
- The reference picture and the intra slice an H.264 reading awaits are
  counted only where FFmpeg decodes them — a picture it starts or joins at
  a slice before any unit it may stop at under `AV_EF_EXPLODE`, a slice it
  refuses none — under every sequence parameter set, and the reference
  picture is awaited again after a picture at which the decoder may set its
  context up again, which flushes it (`KeptReason::AwaitingReference`).
- An H.264 sequence parameter set's id is read as FFmpeg's
  `get_ue_golomb_31` reads it, 31 among the ids FFmpeg takes.
- An H.264 slice of a picture parameter set that carries redundant counts
  is read on to its `redundant_pic_cnt`, as FFmpeg reads it, where every
  such slice was one the reading could not read: a primary picture's
  slice is counted, and a redundant one, which FFmpeg discards, is no
  reference picture, no intra slice and no field. A packet of redundant
  slices alone is marked whatever their `nal_ref_idc`, but where a first
  field may wait at it, or an IDR slice keeps it.
- A packet whose every slice FFmpeg refuses in its header — one naming a
  picture parameter set it does not hold, among them — or discards makes
  no picture, and no rule of a picture holds it
  (`KeptReason::AwaitingReference`, `AwaitingIntraSlice`, `IntraOnly`,
  `FrameNumGaps`, `ReorderUnfixed`); a slice of `nal_ref_idc` above 0 FFmpeg
  refuses still keeps its packet (`KeptReason::Referenced`).
- The reference picture is awaited again at an IDR unit, which FFmpeg
  resets at whatever it makes of the slice, and after a packet not read
  whole that may hold one, or a start that may set the decoder's context
  up again (`KeptReason::AwaitingReference`); and a packet of slices that
  start no picture is kept while a first field may wait, which FFmpeg ends
  again at it (`KeptReason::PendingField`).

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
