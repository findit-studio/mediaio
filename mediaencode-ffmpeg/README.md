<div align="center">
<h1>mediaencode-ffmpeg</h1>
</div>

The FFmpeg write adapter of the mediaio family: it implements the
`mediaencode` faces over libavcodec and libavformat. In this release:
the **muxer**, over libavformat, and the gate every encoder will be
opened through — the encoder-by-name catalogue and its GPL rule. The
encoders themselves arrive in the following releases.

## The muxer

`FfmpegMuxer` writes any container libavformat names (`ContainerName`)
into a caller's `Write + Seek`, from the track table a remux collects —
one `TrackDescription` per track, each with the `CodecTicket` its codec
parameters are rebuilt from, an attachment track with its one file.

- **Opening judges the whole table first.** What the container cannot
  take at all is refused by name, with nothing written: a codec the
  muxer says it cannot store, a Matroska attachment without a filename
  or a MIME type, a chapter that runs backwards. What it can take only
  by dropping something is written, and the loss is **reported**
  (`MuxReport`): a font MP4 has no place for, a language MP4 cannot
  spell, a disposition flag the container has no field for. The report
  covers Matroska, WebM and MP4; another container opens the same way,
  without the report's claims.
- **Attachments**: Matroska keeps each with its filename, MIME type and
  language, but no disposition flag; MP4 keeps a JPEG, PNG or BMP as
  cover art, the picture alone. The report names each field a road
  drops — a cover's filename and MIME type among them.
- **Writing keeps the face's rules, then the container's.** A packet
  for no track, of the wrong kind, without a decode time, decoded before
  the last or presented before it is decoded is refused by the face's
  name — presentation judged, as libavformat judges it, on the counts in
  the stream's time base, so two times that count to one tick are
  written. A time the stream cannot count, or one that lands on
  libavformat's no-timestamp value (`FfmpegMuxRefusal::Unrepresentable`),
  and an equal decode time on a strict container's video or audio
  stream — MP4 — (`FfmpegMuxRefusal::RepeatedDts`) are refused by the
  muxer's own. Every refusal hands the packet back untouched.
- **Every rule libavformat applies to a packet is accounted for** in
  Matroska, WebM and MP4, at FFmpeg 9.0.1: libavformat interleaves
  packets before its muxer sees them, so a muxer's own refusal comes
  after taking the packet, often while writing another. Each rule is
  refused by name first — the container's timing rules
  (`FfmpegMuxRefusal`) and its rules over a packet's bytes and side data
  (`FfmpegMuxRefusal::Payload`, `PayloadRule`) — or reported at open,
  where the container writes a track's packets in a form of its own
  (`Uncarried::Rewritten`, `Rewrite`), or written and read back as handed
  over. MP4's durations, which it keeps only as the distances between
  the samples it writes, are reported as they are lost, in the muxer's
  own `report`; so are libavformat's move of every stream's times, where
  a packet lies before the earliest the container writes
  (`Uncarried::TimesShifted`), and a negative duration it writes as zero.
- **libavformat's automatic bitstream filters run, each reported.** As
  `ffmpeg -c copy` runs them: AAC's ADTS headers stripped, VP9's hidden
  frames joined into superframes, PGS segments joined into display sets,
  and every other filter a muxer inserts — each named for its track
  (`Uncarried::Filtered`), at open where open decides it, else at the
  write of the packet that inserts it. libavformat inserts a filter at a
  track's first packet, so the first packet decides, and on an AAC track
  with no configuration the muxer admits only what the filter converts.
  Every rule of a packet's times meets what a filter puts out, when it
  puts it out; PGS's open display sets the muxer holds itself, handing
  them to libavformat whole, and `take_held` hands back one whose
  completion is refused.
  `MuxOptions::set_automatic_filters(false)` turns them off, and the
  refusals by name apply instead.
- **Packet side data a container has no place for is reported**, by
  name, for its track (`Uncarried::SideData`), from a table of what each
  container writes; where the source does not settle a kind's fate, it is
  reported as possibly dropped.
- **What a header or trailer would refuse is refused at open**: TrueHD
  into MP4, RealMedia's codecs into Matroska, MP4 PCM of a channel layout
  it cannot write (`MuxOpenError`).
- **Packets are copied into libavformat**, never lent, and a sink that
  panics is caught before C sees the unwind, named in the muxer's fault,
  and never called again.

It builds against **FFmpeg 9** or newer, the family's floor: no older
release is supported, and `mediaio-ffmpeg`'s build script refuses one by
name.

## Encoders by name

An encoder is named by its FFmpeg name and passes two gates. The first
is the catalogue: the name must be one this crate knows, and a GPL
encoder (`libx264`, `libx264rgb`, `libx265`) passes only when the crate
is built with its `gpl` feature — without it the name is refused by
name, before libavcodec is asked for anything. The second is the linked
libavcodec, which must carry an encoder by that name; one it lacks is
refused by name, never replaced by another.

| encoder | kind | license |
|---|---|---|
| `h264_videotoolbox`, `hevc_videotoolbox`, `prores_videotoolbox` | video, hardware | LGPL |
| `prores_ks`, `libopenh264` | video | LGPL (`libopenh264` is BSD) |
| `libx264`, `libx264rgb`, `libx265` | video | GPL — behind `gpl` |
| `aac`, `aac_at` | audio | LGPL |

The catalogue is closed on purpose: FFmpeg has no API that states an
encoder's license, so a name this crate has not classified cannot be
vouched for and is refused. It grows by pull request — an entry is a
name, its kind and its license, checked against the encoder's source.

## GPL and your binary

**Linking an FFmpeg built with `--enable-gpl` makes your binary GPL**,
whatever this crate's features say — Homebrew's FFmpeg is such a build.
The `gpl` feature decides only whether this crate will ask for a GPL
encoder by name. A build that links an LGPL FFmpeg, or that must not
depend on GPL code, leaves the feature off, and every road to a GPL
encoder through this crate stays closed.

## Features

| feature | default | what it adds |
|---|---|---|
| `std` | yes | the `std` tier of the faces and of the FFmpeg base |
| `gpl` | no | opens the GPL encoders by name |

#### License

`mediaencode-ffmpeg` is under the terms of both the MIT license and the
Apache License (Version 2.0); see the section above for what linking a
GPL FFmpeg does to a binary.

See [LICENSE-APACHE](LICENSE-APACHE), [LICENSE-MIT](LICENSE-MIT) for details.

Copyright (c) 2026 FinDIT Studio authors.
