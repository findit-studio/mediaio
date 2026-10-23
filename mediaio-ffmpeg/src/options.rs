//! How a decoder decodes, as opposed to what it may spend
//! ([`crate::limits`]): the `AVCodecContext` fields a caller sets to have
//! libavcodec skip work.
//!
//! [`DecoderOptions`] carries libavcodec's three `AVDiscard` settings —
//! `skip_frame`, `skip_loop_filter` and `skip_idct` (`libavcodec/avcodec.h`
//! 1653–1667 in FFmpeg 9.0.1, "decoding: Set by user") — each a [`Discard`].
//! The default is libavcodec's own, [`Discard::Default`] for all three.
//!
//! **Not the road for a picture cadence.** `skip_frame` decides inside the
//! decoder, after the packet is sent, and decodes nothing it skips. A
//! consumer that keeps only the pictures something else references drops
//! the packets the demuxer marks
//! [`DISPOSABLE`](mediaspine::packet::PacketFlags::DISPOSABLE) before they
//! reach a decoder. The two are not equivalent on HEVC:
//! [`Discard::NonReference`] skips every sub-layer non-reference picture at
//! every temporal sub-layer (`libavcodec/hevc/hevcdec.c` 3766–3767), a
//! picture of a higher sub-layer may reference among them; the
//! bitstream's mark ([`mediaspine::nal`]) holds such a picture to the
//! highest sub-layer.

use core::{fmt, str::FromStr};

use ffmpeg_next::ffi::AVDiscard;

/// Which frames a libavcodec decoder skips a stage of work for: FFmpeg's
/// `AVDiscard` (`libavcodec/defs.h` 226–232 in 9.0.1), each value a
/// threshold — a frame is skipped where it falls at or past it.
///
/// Its text form is FFmpeg's own option constant (`libavcodec/options_table.h`
/// 258–264): `none`, `default`, `noref`, `bidir`, `nointra`, `nokey`,
/// `all`. [`Display`](fmt::Display) writes it; [`FromStr`] reads it in any
/// case. The set is FFmpeg's and closed: a name outside it is refused
/// ([`ParseDiscardError`]), never read as another.
///
/// Marked `#[non_exhaustive]`: a later FFmpeg value joins as a variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum Discard {
  /// `AVDISCARD_NONE` (−16): discard nothing.
  Nothing,
  /// `AVDISCARD_DEFAULT` (0): discard what is useless, such as a packet of
  /// no bytes in AVI — libavcodec's default.
  #[default]
  Default,
  /// `AVDISCARD_NONREF` (8): discard every non-reference frame.
  NonReference,
  /// `AVDISCARD_BIDIR` (16): discard every bidirectionally predicted
  /// frame.
  Bidirectional,
  /// `AVDISCARD_NONINTRA` (24): discard every frame but intra ones.
  NonIntra,
  /// `AVDISCARD_NONKEY` (32): discard every frame but keyframes.
  NonKey,
  /// `AVDISCARD_ALL` (48): discard every frame.
  All,
}

impl Discard {
  /// Every value, in FFmpeg's order.
  pub const ALL: [Self; 7] = [
    Self::Nothing,
    Self::Default,
    Self::NonReference,
    Self::Bidirectional,
    Self::NonIntra,
    Self::NonKey,
    Self::All,
  ];

  /// FFmpeg's option constant for the value: `none`, `default`, `noref`,
  /// `bidir`, `nointra`, `nokey`, `all`.
  pub const fn as_str(self) -> &'static str {
    match self {
      Self::Nothing => "none",
      Self::Default => "default",
      Self::NonReference => "noref",
      Self::Bidirectional => "bidir",
      Self::NonIntra => "nointra",
      Self::NonKey => "nokey",
      Self::All => "all",
    }
  }

  /// The `AVDiscard` libavcodec reads.
  pub const fn as_av(self) -> AVDiscard {
    match self {
      Self::Nothing => AVDiscard::AVDISCARD_NONE,
      Self::Default => AVDiscard::AVDISCARD_DEFAULT,
      Self::NonReference => AVDiscard::AVDISCARD_NONREF,
      Self::Bidirectional => AVDiscard::AVDISCARD_BIDIR,
      Self::NonIntra => AVDiscard::AVDISCARD_NONINTRA,
      Self::NonKey => AVDiscard::AVDISCARD_NONKEY,
      Self::All => AVDiscard::AVDISCARD_ALL,
    }
  }
}

/// Writes FFmpeg's option constant: `noref`.
impl fmt::Display for Discard {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(self.as_str())
  }
}

/// Why a name is no [`Discard`]: it is none of FFmpeg's seven constants, in
/// any case.
///
/// Opaque: the input is not retained. `#[non_exhaustive]` keeps it
/// constructible only here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, thiserror::Error)]
#[error("not an FFmpeg discard name: none, default, noref, bidir, nointra, nokey or all")]
#[non_exhaustive]
pub struct ParseDiscardError;

impl FromStr for Discard {
  type Err = ParseDiscardError;

  /// Reads FFmpeg's option constant, in any ASCII case: `noref`, `NoRef`.
  ///
  /// # Errors
  ///
  /// [`ParseDiscardError`] for any other text.
  fn from_str(text: &str) -> Result<Self, Self::Err> {
    Self::ALL
      .into_iter()
      .find(|value| value.as_str().eq_ignore_ascii_case(text))
      .ok_or(ParseDiscardError)
  }
}

/// How a decoder decodes: libavcodec's three `AVDiscard` settings. A
/// decoder opened with them writes them into every video `AVCodecContext`
/// it opens — each hardware candidate its probe builds, a software
/// decoder, a fallback's.
///
/// What a decoder may spend is [`DecoderLimits`](crate::limits::DecoderLimits)'s;
/// this is what it does. The defaults are libavcodec's: [`Discard::Default`]
/// for each.
///
/// **`skip_frame` = [`Discard::All`] decodes no picture**, so
/// `mediadecode-ffmpeg`'s decoders run no hardware probe under it — a probe
/// commits a backend on its first picture, and none comes. An unpinned
/// stream decoder (`DecodePath::Auto`) opens software at once; one held to
/// hardware (`DecodePath::AnyHardware`), and the standalone decoder's probe,
/// commit to the first backend that opens; a pinned backend opens as asked.
/// Each drains to its end with no picture, cleanly. Under every other level
/// above [`Discard::Default`] — [`Discard::NonReference`],
/// [`Discard::Bidirectional`], [`Discard::NonIntra`], [`Discard::NonKey`],
/// and any FFmpeg adds — a stream may give no picture too: each value is a
/// threshold, and a stream of pictures the level skips alone gives none. So
/// the probe runs while pictures come, and a clean end, or a probe that
/// fills, with none is the stream's end: the backend that opened is
/// committed, and an unpinned stream decoder ends on software. `Default`
/// and `Nothing` skip no picture.
///
/// Marked `#[non_exhaustive]`: a later setting joins as a field, read and
/// set through its own methods.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub struct DecoderOptions {
  skip_frame: Discard,
  skip_loop_filter: Discard,
  skip_idct: Discard,
}

impl DecoderOptions {
  /// libavcodec's defaults: [`Discard::Default`] for each setting.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new() -> Self {
    Self {
      skip_frame: Discard::Default,
      skip_loop_filter: Discard::Default,
      skip_idct: Discard::Default,
    }
  }

  /// Which frames the decoder skips decoding entirely
  /// (`AVCodecContext.skip_frame`).
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn skip_frame(&self) -> Discard {
    self.skip_frame
  }

  /// Which frames the decoder skips its loop filter for
  /// (`AVCodecContext.skip_loop_filter`).
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn skip_loop_filter(&self) -> Discard {
    self.skip_loop_filter
  }

  /// Which frames the decoder skips the inverse transform and
  /// dequantization for (`AVCodecContext.skip_idct`).
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn skip_idct(&self) -> Discard {
    self.skip_idct
  }

  /// Sets [`Self::skip_frame`] (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_skip_frame(mut self, value: Discard) -> Self {
    self.skip_frame = value;
    self
  }

  /// Sets [`Self::skip_loop_filter`] (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_skip_loop_filter(mut self, value: Discard) -> Self {
    self.skip_loop_filter = value;
    self
  }

  /// Sets [`Self::skip_idct`] (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_skip_idct(mut self, value: Discard) -> Self {
    self.skip_idct = value;
    self
  }

  /// Sets [`Self::skip_frame`] in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_skip_frame(&mut self, value: Discard) -> &mut Self {
    self.skip_frame = value;
    self
  }

  /// Sets [`Self::skip_loop_filter`] in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_skip_loop_filter(&mut self, value: Discard) -> &mut Self {
    self.skip_loop_filter = value;
    self
  }

  /// Sets [`Self::skip_idct`] in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_skip_idct(&mut self, value: Discard) -> &mut Self {
    self.skip_idct = value;
    self
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn each_discard_is_ffmpegs_value_and_name() {
    let expected = [
      (Discard::Nothing, -16, "none"),
      (Discard::Default, 0, "default"),
      (Discard::NonReference, 8, "noref"),
      (Discard::Bidirectional, 16, "bidir"),
      (Discard::NonIntra, 24, "nointra"),
      (Discard::NonKey, 32, "nokey"),
      (Discard::All, 48, "all"),
    ];
    assert_eq!(Discard::ALL.len(), expected.len());
    for (value, av, name) in expected {
      assert_eq!(value.as_av() as i32, av, "{name}");
      assert_eq!(std::string::ToString::to_string(&value), name);
      assert_eq!(name.parse::<Discard>(), Ok(value));
      assert_eq!(name.to_ascii_uppercase().parse::<Discard>(), Ok(value));
    }
    assert_eq!("NoRef".parse::<Discard>(), Ok(Discard::NonReference));
    for refused in ["", "nonref", "non_reference", " noref", "noref ", "8"] {
      assert_eq!(
        refused.parse::<Discard>(),
        Err(ParseDiscardError),
        "{refused:?}"
      );
    }
    assert_eq!(Discard::default(), Discard::Default);
  }

  #[test]
  fn the_options_default_to_libavcodecs_and_set_each_field() {
    let options = DecoderOptions::new();
    assert_eq!(options, DecoderOptions::default());
    assert_eq!(
      (
        options.skip_frame(),
        options.skip_loop_filter(),
        options.skip_idct()
      ),
      (Discard::Default, Discard::Default, Discard::Default)
    );
    let built = options
      .with_skip_frame(Discard::NonReference)
      .with_skip_loop_filter(Discard::All)
      .with_skip_idct(Discard::NonKey);
    assert_eq!(
      (
        built.skip_frame(),
        built.skip_loop_filter(),
        built.skip_idct()
      ),
      (Discard::NonReference, Discard::All, Discard::NonKey)
    );
    let mut set = DecoderOptions::new();
    set
      .set_skip_frame(Discard::NonReference)
      .set_skip_loop_filter(Discard::All)
      .set_skip_idct(Discard::NonKey);
    assert_eq!(set, built);
  }
}
