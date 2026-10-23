<div align="center">
<h1>mediaencode-ffmpeg</h1>
</div>

The FFmpeg write adapter of the mediaio family: it implements the
`mediaencode` faces over libavcodec and libavformat. In this release it
is the gate every encoder will be opened through: the encoder-by-name
catalogue and its GPL rule. The muxer and the encoders themselves
arrive in the following releases.

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
