//! Backend-specific `*Extra` carriers used as the
//! `mediaspine::*Adapter::*Extra` associated types.
//!
//! Fields are private; values are read through getters and set through
//! `with_*` (consuming builders) / `set_*` (in-place mutators) — the
//! crate-wide encapsulation convention. `const fn` is used wherever
//! the field type permits (i.e. anything but `Vec`).
//!
//! # The extras obey the amputation contract too
//!
//! [`SideDataEntry`] carries its payload as an `FfmpegBytes`, not a
//! `Vec<u8>`. Those bytes ride on packets and frames that a graph fans
//! out, and the [carrier contract][law] says a clone of a message is a
//! refcount bump — a `Vec` in the extras would have made a frame's
//! *payload* cheap to clone and its metadata expensive, which is the
//! kind of asymmetry nobody remembers while profiling.
//! `smpte_timecode` stays a `Vec<u32>` on [`VideoFrameExtra`]: those
//! are parsed values, a handful of words, not bytes crossing a
//! boundary.
//!
//! [law]: mediaspine::adapter#the-carrier-amputation-contract

use std::vec::Vec;

use mediaframe::frame::Rotation;

use crate::FfmpegBytes;

use derive_more::IsVariant;
use ffmpeg_next::codec::Parameters;

use crate::{
  error::{
    ParametersAlloc, ParametersCopy, ParametersError, ParametersMissing, ParametersTooLarge,
  },
  ticket::CodecTicket,
};

/// Per-`VideoPacket` extras.
#[derive(Clone, Debug, Default)]
pub struct VideoPacketExtra {
  stream_index: i32,
  byte_pos: Option<i64>,
  side_data: Vec<SideDataEntry>,
}

impl VideoPacketExtra {
  /// Constructs a `VideoPacketExtra` with the given stream index.
  /// `byte_pos` defaults to `None` and `side_data` to empty.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(stream_index: i32) -> Self {
    Self {
      stream_index,
      byte_pos: None,
      side_data: Vec::new(),
    }
  }

  /// Returns the source `AVStream.index`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn stream_index(&self) -> i32 {
    self.stream_index
  }

  /// Returns the byte position of the packet in the input file, or
  /// `None` if unknown.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn byte_pos(&self) -> Option<i64> {
    self.byte_pos
  }

  /// Returns the raw side-data entries from `AVPacket.side_data`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn side_data(&self) -> &[SideDataEntry] {
    self.side_data.as_slice()
  }

  /// Sets the stream index (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_stream_index(mut self, value: i32) -> Self {
    self.stream_index = value;
    self
  }
  /// Sets the byte position (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_byte_pos(mut self, value: Option<i64>) -> Self {
    self.byte_pos = value;
    self
  }
  /// Sets the side-data list (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub fn with_side_data(mut self, value: Vec<SideDataEntry>) -> Self {
    self.side_data = value;
    self
  }

  /// Sets the stream index in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_stream_index(&mut self, value: i32) -> &mut Self {
    self.stream_index = value;
    self
  }
  /// Sets the byte position in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_byte_pos(&mut self, value: Option<i64>) -> &mut Self {
    self.byte_pos = value;
    self
  }
  /// Sets the side-data list in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn set_side_data(&mut self, value: Vec<SideDataEntry>) -> &mut Self {
    self.side_data = value;
    self
  }
}

/// Per-`VideoFrame` extras carrying everything the unified
/// `mediaspine::ColorInfo` doesn't already cover.
#[derive(Clone, Debug, Default)]
pub struct VideoFrameExtra {
  sample_aspect_ratio: Option<(u32, u32)>,
  picture_type: PictureType,
  key_frame: bool,
  interlaced: bool,
  top_field_first: bool,
  best_effort_timestamp: Option<i64>,
  mastering_display: Option<MasteringDisplay>,
  content_light_level: Option<ContentLightLevel>,
  smpte_timecode: Vec<u32>,
  side_data: Vec<SideDataEntry>,
}

impl VideoFrameExtra {
  /// Constructs an empty `VideoFrameExtra` (all fields at default).
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new() -> Self {
    Self {
      sample_aspect_ratio: None,
      picture_type: PictureType::Unspecified,
      key_frame: false,
      interlaced: false,
      top_field_first: false,
      best_effort_timestamp: None,
      mastering_display: None,
      content_light_level: None,
      smpte_timecode: Vec::new(),
      side_data: Vec::new(),
    }
  }

  /// Sample aspect ratio (par numerator / denominator), `None` if 1:1
  /// or unspecified.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn sample_aspect_ratio(&self) -> Option<(u32, u32)> {
    self.sample_aspect_ratio
  }
  /// Frame picture type (I/P/B/etc.).
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn picture_type(&self) -> PictureType {
    self.picture_type
  }
  /// `True` if this frame is a key frame.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn key_frame(&self) -> bool {
    self.key_frame
  }
  /// `True` if the frame is interlaced.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn interlaced(&self) -> bool {
    self.interlaced
  }
  /// `True` if the top field is first (only meaningful with `interlaced`).
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn top_field_first(&self) -> bool {
    self.top_field_first
  }
  /// FFmpeg's heuristic best-effort PTS, or `None` if unknown.
  ///
  /// **A guess, and one that a dropped packet can move.** libavcodec picks
  /// the frame's pts or its packet's dts by counts it keeps across every
  /// frame the decoder has output (`guess_correct_pts`,
  /// `libavcodec/decode.c` 296–319 in FFmpeg 9.0.1, called for each frame
  /// at 694–696), so a packet dropped before the decoder — one marked
  /// [`DISPOSABLE`](mediaspine::packet::PacketFlags::DISPOSABLE) among
  /// them, by the container or the bitstream — can move it for a later
  /// frame. The frame's own pts ([`VideoFrame::pts`](crate::VideoFrame),
  /// `AVFrame.pts`: the pts of the packet its picture came in) moves for
  /// no other packet, and is the exact one.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn best_effort_timestamp(&self) -> Option<i64> {
    self.best_effort_timestamp
  }
  /// HDR10 mastering-display metadata, if present on the source frame.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn mastering_display(&self) -> Option<MasteringDisplay> {
    self.mastering_display
  }
  /// HDR10 content-light-level.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn content_light_level(&self) -> Option<ContentLightLevel> {
    self.content_light_level
  }
  /// SMPTE ST 12-M timecode entries (raw 32-bit BCD-packed values).
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn smpte_timecode(&self) -> &[u32] {
    self.smpte_timecode.as_slice()
  }
  /// Raw side-data entries from `AVFrame.side_data`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn side_data(&self) -> &[SideDataEntry] {
    self.side_data.as_slice()
  }

  /// Sets the sample aspect ratio (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn with_sample_aspect_ratio(mut self, value: Option<(u32, u32)>) -> Self {
    self.sample_aspect_ratio = value;
    self
  }
  /// Sets the picture type (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_picture_type(mut self, value: PictureType) -> Self {
    self.picture_type = value;
    self
  }
  /// Sets the key-frame flag (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_key_frame(mut self, value: bool) -> Self {
    self.key_frame = value;
    self
  }
  /// Sets the interlaced flag (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_interlaced(mut self, value: bool) -> Self {
    self.interlaced = value;
    self
  }
  /// Sets the top-field-first flag (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_top_field_first(mut self, value: bool) -> Self {
    self.top_field_first = value;
    self
  }
  /// Sets the best-effort timestamp (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_best_effort_timestamp(mut self, value: Option<i64>) -> Self {
    self.best_effort_timestamp = value;
    self
  }
  /// Sets the mastering-display metadata (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_mastering_display(mut self, value: Option<MasteringDisplay>) -> Self {
    self.mastering_display = value;
    self
  }
  /// Sets the content-light-level metadata (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_content_light_level(mut self, value: Option<ContentLightLevel>) -> Self {
    self.content_light_level = value;
    self
  }
  /// Sets the SMPTE timecode list (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub fn with_smpte_timecode(mut self, value: Vec<u32>) -> Self {
    self.smpte_timecode = value;
    self
  }
  /// Sets the side-data list (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub fn with_side_data(mut self, value: Vec<SideDataEntry>) -> Self {
    self.side_data = value;
    self
  }

  /// Sets the sample aspect ratio in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_sample_aspect_ratio(&mut self, value: Option<(u32, u32)>) -> &mut Self {
    self.sample_aspect_ratio = value;
    self
  }
  /// Sets the picture type in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_picture_type(&mut self, value: PictureType) -> &mut Self {
    self.picture_type = value;
    self
  }
  /// Sets the key-frame flag in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_key_frame(&mut self, value: bool) -> &mut Self {
    self.key_frame = value;
    self
  }
  /// Sets the interlaced flag in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_interlaced(&mut self, value: bool) -> &mut Self {
    self.interlaced = value;
    self
  }
  /// Sets the top-field-first flag in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_top_field_first(&mut self, value: bool) -> &mut Self {
    self.top_field_first = value;
    self
  }
  /// Sets the best-effort timestamp in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_best_effort_timestamp(&mut self, value: Option<i64>) -> &mut Self {
    self.best_effort_timestamp = value;
    self
  }
  /// Sets the mastering-display metadata in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_mastering_display(&mut self, value: Option<MasteringDisplay>) -> &mut Self {
    self.mastering_display = value;
    self
  }
  /// Sets the content-light-level metadata in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_content_light_level(&mut self, value: Option<ContentLightLevel>) -> &mut Self {
    self.content_light_level = value;
    self
  }
  /// Sets the SMPTE timecode list in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn set_smpte_timecode(&mut self, value: Vec<u32>) -> &mut Self {
    self.smpte_timecode = value;
    self
  }
  /// Sets the side-data list in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn set_side_data(&mut self, value: Vec<SideDataEntry>) -> &mut Self {
    self.side_data = value;
    self
  }
}

/// Per-`AudioPacket` extras.
#[derive(Clone, Debug, Default)]
pub struct AudioPacketExtra {
  stream_index: i32,
  byte_pos: Option<i64>,
  side_data: Vec<SideDataEntry>,
}

impl AudioPacketExtra {
  /// Constructs an `AudioPacketExtra` with the given stream index.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(stream_index: i32) -> Self {
    Self {
      stream_index,
      byte_pos: None,
      side_data: Vec::new(),
    }
  }

  /// Returns the source `AVStream.index`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn stream_index(&self) -> i32 {
    self.stream_index
  }
  /// Returns the byte position, or `None` if unknown.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn byte_pos(&self) -> Option<i64> {
    self.byte_pos
  }
  /// Returns the raw side-data entries.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn side_data(&self) -> &[SideDataEntry] {
    self.side_data.as_slice()
  }

  /// Sets the stream index (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_stream_index(mut self, value: i32) -> Self {
    self.stream_index = value;
    self
  }
  /// Sets the byte position (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_byte_pos(mut self, value: Option<i64>) -> Self {
    self.byte_pos = value;
    self
  }
  /// Sets the side-data list (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub fn with_side_data(mut self, value: Vec<SideDataEntry>) -> Self {
    self.side_data = value;
    self
  }

  /// Sets the stream index in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_stream_index(&mut self, value: i32) -> &mut Self {
    self.stream_index = value;
    self
  }
  /// Sets the byte position in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_byte_pos(&mut self, value: Option<i64>) -> &mut Self {
    self.byte_pos = value;
    self
  }
  /// Sets the side-data list in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn set_side_data(&mut self, value: Vec<SideDataEntry>) -> &mut Self {
    self.side_data = value;
    self
  }
}

/// Per-`AudioFrame` extras.
#[derive(Clone, Debug, Default)]
pub struct AudioFrameExtra {
  best_effort_timestamp: Option<i64>,
  side_data: Vec<SideDataEntry>,
}

impl AudioFrameExtra {
  /// Constructs an empty `AudioFrameExtra`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new() -> Self {
    Self {
      best_effort_timestamp: None,
      side_data: Vec::new(),
    }
  }

  /// FFmpeg's heuristic best-effort PTS, or `None` if unknown: a guess
  /// from every frame the decoder output before, as
  /// [`VideoFrameExtra::best_effort_timestamp`] describes; the frame's own
  /// pts is the exact one.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn best_effort_timestamp(&self) -> Option<i64> {
    self.best_effort_timestamp
  }
  /// Returns the raw side-data entries.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn side_data(&self) -> &[SideDataEntry] {
    self.side_data.as_slice()
  }

  /// Sets the best-effort timestamp (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_best_effort_timestamp(mut self, value: Option<i64>) -> Self {
    self.best_effort_timestamp = value;
    self
  }
  /// Sets the side-data list (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub fn with_side_data(mut self, value: Vec<SideDataEntry>) -> Self {
    self.side_data = value;
    self
  }

  /// Sets the best-effort timestamp in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_best_effort_timestamp(&mut self, value: Option<i64>) -> &mut Self {
    self.best_effort_timestamp = value;
    self
  }
  /// Sets the side-data list in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn set_side_data(&mut self, value: Vec<SideDataEntry>) -> &mut Self {
    self.side_data = value;
    self
  }
}

/// Per-`SubtitlePacket` extras.
#[derive(Clone, Debug, Default)]
pub struct SubtitlePacketExtra {
  stream_index: i32,
  language: Option<[u8; 3]>,
  forced: bool,
  side_data: Vec<SideDataEntry>,
}

impl SubtitlePacketExtra {
  /// Constructs a `SubtitlePacketExtra` with the given stream index.
  /// `side_data` defaults to empty.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(stream_index: i32) -> Self {
    Self {
      stream_index,
      language: None,
      forced: false,
      side_data: Vec::new(),
    }
  }

  /// Returns the source `AVStream.index`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn stream_index(&self) -> i32 {
    self.stream_index
  }
  /// Returns the ISO 639-2/T language tag, or `None` if unspecified.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn language(&self) -> Option<[u8; 3]> {
    self.language
  }
  /// Returns whether this subtitle stream is marked "forced".
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn forced(&self) -> bool {
    self.forced
  }
  /// Returns the raw side-data entries from `AVPacket.side_data`.
  ///
  /// A subtitle packet's side data is rare but not absent — and a
  /// packet that carries *nothing else* is exactly the case this slot
  /// exists for: with no slot, a side-data-only packet has nowhere to
  /// put its only content.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn side_data(&self) -> &[SideDataEntry] {
    self.side_data.as_slice()
  }

  /// Sets the stream index (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_stream_index(mut self, value: i32) -> Self {
    self.stream_index = value;
    self
  }
  /// Sets the language tag (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_language(mut self, value: Option<[u8; 3]>) -> Self {
    self.language = value;
    self
  }
  /// Sets the side-data list (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub fn with_side_data(mut self, value: Vec<SideDataEntry>) -> Self {
    self.side_data = value;
    self
  }
  /// Sets the side-data list in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn set_side_data(&mut self, value: Vec<SideDataEntry>) -> &mut Self {
    self.side_data = value;
    self
  }
  /// Sets the forced flag (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_forced(mut self, value: bool) -> Self {
    self.forced = value;
    self
  }

  /// Sets the stream index in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_stream_index(&mut self, value: i32) -> &mut Self {
    self.stream_index = value;
    self
  }
  /// Sets the language tag in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_language(&mut self, value: Option<[u8; 3]>) -> &mut Self {
    self.language = value;
    self
  }
  /// Sets the forced flag in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_forced(&mut self, value: bool) -> &mut Self {
    self.forced = value;
    self
  }
}

/// Per-`SubtitleFrame` extras.
#[derive(Clone, Debug, Default)]
pub struct SubtitleFrameExtra {
  start_display_time: u32,
  end_display_time: u32,
}

impl SubtitleFrameExtra {
  /// Constructs a `SubtitleFrameExtra`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(start_display_time: u32, end_display_time: u32) -> Self {
    Self {
      start_display_time,
      end_display_time,
    }
  }

  /// `AVSubtitle.start_display_time` — milliseconds from `pts`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn start_display_time(&self) -> u32 {
    self.start_display_time
  }
  /// `AVSubtitle.end_display_time` — milliseconds from `pts`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn end_display_time(&self) -> u32 {
    self.end_display_time
  }

  /// Sets the start display time (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_start_display_time(mut self, value: u32) -> Self {
    self.start_display_time = value;
    self
  }
  /// Sets the end display time (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_end_display_time(mut self, value: u32) -> Self {
    self.end_display_time = value;
    self
  }

  /// Sets the start display time in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_start_display_time(&mut self, value: u32) -> &mut Self {
    self.start_display_time = value;
    self
  }
  /// Sets the end display time in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_end_display_time(&mut self, value: u32) -> &mut Self {
    self.end_display_time = value;
    self
  }
}

/// Where the first stored row and column of a still belong on screen —
/// the eight orientations EXIF names, tags 1 through 8.
///
/// # How it reaches us
///
/// Not through `AVFrame.metadata`, which is where an earlier reading of
/// libavcodec put it. **Measured**, against this build, by feeding
/// mjpeg a JPEG whose EXIF IFD carries each orientation in turn: for a
/// recognised tag (1..=8) the decoder emits an
/// `AV_FRAME_DATA_DISPLAYMATRIX` frame side-data entry and puts
/// *nothing* in the metadata dictionary; only an out-of-range tag (0,
/// 9, …) is left to fall through to the dictionary, as the string
/// `"      9"`. So the display matrix is the road, and the metadata
/// dictionary is where malformed tags go to be ignored.
///
/// The matrix that arrives is the one below, in units of 65536 (the
/// 16.16 fixed point `libavutil/display.h` specifies), with
/// `(a, b, c, d)` the entries at indices 0, 1, 3, 4:
///
/// | tag | variant | a | b | c | d |
/// |-----|---------|---|---|---|---|
/// | 1 | [`TopLeft`](Self::TopLeft)         |  1 |  0 |  0 |  1 |
/// | 2 | [`TopRight`](Self::TopRight)       | -1 |  0 |  0 |  1 |
/// | 3 | [`BottomRight`](Self::BottomRight) | -1 |  0 |  0 | -1 |
/// | 4 | [`BottomLeft`](Self::BottomLeft)   |  1 |  0 |  0 | -1 |
/// | 5 | [`LeftTop`](Self::LeftTop)         |  0 |  1 |  1 |  0 |
/// | 6 | [`RightTop`](Self::RightTop)       |  0 |  1 | -1 |  0 |
/// | 7 | [`RightBottom`](Self::RightBottom) |  0 | -1 | -1 |  0 |
/// | 8 | [`LeftBottom`](Self::LeftBottom)   |  0 | -1 |  1 |  0 |
///
/// # Why a vocabulary of its own
///
/// [`mediaframe::frame::Rotation`] is this crate's home for a quarter
/// turn and is reused by [`Self::rotation`] — but it names four values
/// and there are eight. The other four are *mirrored*, and a rotation
/// vocabulary cannot hold a reflection. The same gap shows up one
/// level down in FFmpeg's own API: `av_display_rotation_get` answers
/// `-180` for **both** tag 2 and tag 3, `-90` for both 5 and 6, and
/// `90` for both 7 and 8 — reading only the angle loses the mirror on
/// half the vocabulary. This type is the eight-value reading that does
/// not.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, IsVariant)]
#[non_exhaustive]
pub enum ImageOrientation {
  /// Tag 1 — stored as displayed. The default and the overwhelming
  /// majority of files.
  #[default]
  TopLeft,
  /// Tag 2 — mirrored horizontally.
  TopRight,
  /// Tag 3 — turned half way round.
  BottomRight,
  /// Tag 4 — mirrored vertically.
  BottomLeft,
  /// Tag 5 — transposed (mirrored, then a quarter turn clockwise).
  LeftTop,
  /// Tag 6 — a quarter turn clockwise.
  RightTop,
  /// Tag 7 — transverse (mirrored, then three quarter turns
  /// clockwise).
  RightBottom,
  /// Tag 8 — three quarter turns clockwise.
  LeftBottom,
  /// A display matrix none of the eight names, carried verbatim as
  /// **all nine** of its words.
  ///
  /// Reachable: the display matrix is a general affine transform, and
  /// a container (a MOV `tkhd`, a hand-built stream) may carry an
  /// arbitrary one. Rather than answer `TopLeft` for a transform this
  /// vocabulary cannot name — the silent-loss failure this crate
  /// refuses everywhere else — the whole matrix rides along and
  /// [`Self::to_exif_code`] admits it has no tag for it.
  ///
  /// **All nine words, not the four that carry the orientation.** The
  /// escape's job is to lose nothing: a matrix with the right linear
  /// part and a translation, or a perspective term, is *not* one of
  /// the eight, and carrying only `[a, b, c, d]` would have thrown
  /// away the very words that made it different. That is the same
  /// collapse the escape exists to prevent, one level in.
  Other([i32; 9]),
}

impl ImageOrientation {
  /// One 16.16 fixed-point unit — the value a display-matrix entry
  /// takes for ±1.
  const UNIT: i32 = 1 << 16;

  /// One 2.30 fixed-point unit — the value the matrix's `w` term takes
  /// for 1, and the only value it takes for a picture that is merely
  /// turned rather than projected.
  const PERSPECTIVE_UNIT: i32 = 1 << 30;

  /// The number of bytes an `AV_FRAME_DATA_DISPLAYMATRIX` entry
  /// carries: nine `int32_t`, per `libavutil/display.h`.
  pub const DISPLAY_MATRIX_BYTES: usize = 9 * core::mem::size_of::<i32>();

  /// The nine-word matrix a named orientation stands for — the exact
  /// inverse of what [`Self::from_display_matrix`] reads, and for
  /// [`Self::Other`] the words it was handed, unchanged.
  ///
  /// `libavutil/display.h` lays the matrix out row-major as
  ///
  /// ```text
  /// | a b u |     | 0 1 2 |
  /// | c d v |  =  | 3 4 5 |
  /// | x y w |     | 6 7 8 |
  /// ```
  ///
  /// where `a b c d x y` are 16.16 fixed point and `u v w` are 2.30.
  /// A named orientation puts the rotation-or-reflection in `a b c d`,
  /// no translation in `x y`, no perspective in `u v`, and unity in
  /// `w`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn matrix(&self) -> [i32; 9] {
    match self {
      Self::Other(matrix) => *matrix,
      named => {
        let [a, b, c, d] = named.linear();
        [a, b, 0, c, d, 0, 0, 0, Self::PERSPECTIVE_UNIT]
      }
    }
  }

  /// Reads an orientation out of the raw bytes of an
  /// `AV_FRAME_DATA_DISPLAYMATRIX` side-data entry.
  ///
  /// `None` when `bytes` is not exactly
  /// [`Self::DISPLAY_MATRIX_BYTES`] long — a malformed entry is not an
  /// orientation, and guessing one from a truncated matrix would be
  /// the invention this slot exists to avoid.
  ///
  /// The entries are `int32_t` in **native** byte order: the side data
  /// is a C array as it sits in memory, not a wire format.
  pub fn from_display_matrix(bytes: &[u8]) -> Option<Self> {
    if bytes.len() != Self::DISPLAY_MATRIX_BYTES {
      return None;
    }
    let mut matrix = [0i32; 9];
    for (index, word) in matrix.iter_mut().enumerate() {
      let mut raw = [0u8; 4];
      raw.copy_from_slice(&bytes[index * 4..index * 4 + 4]);
      *word = i32::from_ne_bytes(raw);
    }
    Some(Self::from_matrix(matrix))
  }

  /// The orientation a whole nine-word matrix names. Anything but the
  /// eight rides [`Self::Other`], whole.
  ///
  /// **A named variant requires the other five words to be canonical**
  /// — no translation in `x`/`y`, no perspective in `u`/`v`, and unity
  /// in `w` — not merely a linear part that matches. A matrix that
  /// turns the picture *and* shifts it is not "turned"; answering
  /// `RightTop` for it would drop the shift on the floor, which is the
  /// collapse the escape exists to prevent.
  fn from_matrix(matrix: [i32; 9]) -> Self {
    const P: i32 = ImageOrientation::UNIT;
    const N: i32 = -ImageOrientation::UNIT;
    const W: i32 = ImageOrientation::PERSPECTIVE_UNIT;
    match matrix {
      [P, 0, 0, 0, P, 0, 0, 0, W] => Self::TopLeft,
      [N, 0, 0, 0, P, 0, 0, 0, W] => Self::TopRight,
      [N, 0, 0, 0, N, 0, 0, 0, W] => Self::BottomRight,
      [P, 0, 0, 0, N, 0, 0, 0, W] => Self::BottomLeft,
      [0, P, 0, P, 0, 0, 0, 0, W] => Self::LeftTop,
      [0, P, 0, N, 0, 0, 0, 0, W] => Self::RightTop,
      [0, N, 0, N, 0, 0, 0, 0, W] => Self::RightBottom,
      [0, N, 0, P, 0, 0, 0, 0, W] => Self::LeftBottom,
      other => Self::Other(other),
    }
  }

  /// The EXIF tag value, 1 through 8.
  ///
  /// `None` for [`Self::Other`]: it names a transform EXIF has no tag
  /// for, and there is no number to invent for it.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn to_exif_code(&self) -> Option<u16> {
    Some(match self {
      Self::TopLeft => 1,
      Self::TopRight => 2,
      Self::BottomRight => 3,
      Self::BottomLeft => 4,
      Self::LeftTop => 5,
      Self::RightTop => 6,
      Self::RightBottom => 7,
      Self::LeftBottom => 8,
      Self::Other(_) => return None,
    })
  }

  /// Decodes an EXIF tag value. `None` outside 1..=8 — never a silent
  /// collapse onto [`Self::TopLeft`], which is what a viewer that
  /// clamps an out-of-range tag ends up showing.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn from_exif_code(code: u16) -> Option<Self> {
    Some(match code {
      1 => Self::TopLeft,
      2 => Self::TopRight,
      3 => Self::BottomRight,
      4 => Self::BottomLeft,
      5 => Self::LeftTop,
      6 => Self::RightTop,
      7 => Self::RightBottom,
      8 => Self::LeftBottom,
      _ => return None,
    })
  }

  /// `true` when displaying the picture correctly requires a
  /// reflection as well as a turn — EXIF tags 2, 4, 5 and 7.
  ///
  /// Read off the sign of the linear part's determinant, which is what
  /// a reflection *is*, so [`Self::Other`] answers too.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn is_mirrored(&self) -> bool {
    let [a, b, c, d] = self.linear();
    // Every entry is 0 or ±65536 for the eight named orientations, and
    // an arbitrary matrix's entries are bounded by `i32`; widening to
    // `i64` keeps the products exact for both.
    (a as i64) * (d as i64) - (b as i64) * (c as i64) < 0
  }

  /// The quarter turn, clockwise, in the EXIF specification's
  /// mirror-then-rotate decomposition — expressed in this workspace's
  /// existing rotation vocabulary.
  ///
  /// `None` for [`Self::Other`], whose transform need not be a
  /// multiple of 90° at all.
  ///
  /// This is the specification's decomposition, not a measurement:
  /// what was measured here is which matrix carries which *tag* (see
  /// the type's own docs), and the tag's meaning is EXIF's to define.
  /// [`Self::is_mirrored`] supplies the half a [`Rotation`] cannot
  /// hold.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn rotation(&self) -> Option<Rotation> {
    Some(match self {
      Self::TopLeft | Self::TopRight => Rotation::D0,
      Self::RightTop | Self::LeftTop => Rotation::D90,
      Self::BottomRight | Self::BottomLeft => Rotation::D180,
      Self::LeftBottom | Self::RightBottom => Rotation::D270,
      Self::Other(_) => return None,
    })
  }

  /// The linear part of the display matrix this orientation stands
  /// for, `[a, b, c, d]` in 16.16 fixed point — matrix indices 0, 1, 3
  /// and 4.
  ///
  /// A **projection**, not the whole value: for [`Self::Other`] it
  /// drops the five words that made the matrix unnameable. Use
  /// [`Self::matrix`] when nothing may be lost; this is for the
  /// rotation-and-reflection question, which the four words answer on
  /// their own.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn linear(&self) -> [i32; 4] {
    const P: i32 = ImageOrientation::UNIT;
    const N: i32 = -ImageOrientation::UNIT;
    match self {
      Self::TopLeft => [P, 0, 0, P],
      Self::TopRight => [N, 0, 0, P],
      Self::BottomRight => [N, 0, 0, N],
      Self::BottomLeft => [P, 0, 0, N],
      Self::LeftTop => [0, P, P, 0],
      Self::RightTop => [0, P, N, 0],
      Self::RightBottom => [0, N, N, 0],
      Self::LeftBottom => [0, N, P, 0],
      Self::Other(matrix) => [matrix[0], matrix[1], matrix[3], matrix[4]],
    }
  }
}

/// Per-[`ImageFrame`](mediaspine::frame::ImageFrame) extras — cover
/// art, an embedded thumbnail, a poster frame.
///
/// **One slot, deliberately.** This household is minimal at birth
/// because a still image's `AVFrame` carries almost nothing a motion
/// frame's does: no picture type, no field order, no best-effort
/// timestamp, no SAR worth a slot. What it does carry is side data,
/// and that is what is here.
///
/// **EXIF orientation takes a slot here**, read off the frame's
/// `AV_FRAME_DATA_DISPLAYMATRIX` side data — see [`ImageOrientation`]
/// for the measurement that establishes that road, and for why an
/// earlier reading of libavcodec (which put orientation in
/// `AVFrame.metadata`) was wrong about where it arrives.
///
/// **An ICC profile** arrives as side data too
/// (`AV_FRAME_DATA_ICC_PROFILE`) and does *not* get a slot: it is a
/// payload, not a fact — kilobytes of colour transform this crate has
/// no vocabulary for and no business parsing. [`Self::side_data`]
/// carries it unparsed and whole for a consumer that does. That is the
/// line: a fact a picture cannot be displayed correctly without is
/// typed; a payload only a specialist reads rides raw.
#[derive(Clone, Debug, Default)]
pub struct ImageFrameExtra {
  orientation: Option<ImageOrientation>,
  side_data: Vec<SideDataEntry>,
}

impl ImageFrameExtra {
  /// Constructs an `ImageFrameExtra` with no orientation and no side
  /// data.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new() -> Self {
    Self {
      orientation: None,
      side_data: Vec::new(),
    }
  }

  /// How the picture should be turned for display, when the file said.
  ///
  /// `None` means the frame carried no display matrix — the ordinary
  /// case for a still with no EXIF orientation tag, and also what an
  /// **out-of-range** tag produces, because libavcodec emits no matrix
  /// for one. A malformed matrix (not nine `int32_t`) is `None` too.
  /// In every one of those cases the raw entry, if there was one, is
  /// still in [`Self::side_data`].
  ///
  /// A consumer that ignores this displays a sideways photograph.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn orientation(&self) -> Option<ImageOrientation> {
    self.orientation
  }

  /// Returns the raw side-data entries from `AVFrame.side_data` —
  /// `AV_FRAME_DATA_ICC_PROFILE` and the display matrix among them.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn side_data(&self) -> &[SideDataEntry] {
    self.side_data.as_slice()
  }

  /// Sets the orientation (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_orientation(mut self, value: Option<ImageOrientation>) -> Self {
    self.orientation = value;
    self
  }

  /// Sets the side-data list (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub fn with_side_data(mut self, value: Vec<SideDataEntry>) -> Self {
    self.side_data = value;
    self
  }

  /// Sets the orientation in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_orientation(&mut self, value: Option<ImageOrientation>) -> &mut Self {
    self.orientation = value;
    self
  }

  /// Sets the side-data list in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn set_side_data(&mut self, value: Vec<SideDataEntry>) -> &mut Self {
    self.side_data = value;
    self
  }
}

/// Picture type per `AVFrame.pict_type`.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash, IsVariant)]
#[non_exhaustive]
pub enum PictureType {
  /// Unspecified / unset.
  #[default]
  Unspecified,
  /// Intra (I-frame).
  I,
  /// Predicted (P-frame).
  P,
  /// Bi-directional predicted (B-frame).
  B,
  /// S(GMC)-VOP from MPEG-4.
  S,
  /// Switching Intra (H.264).
  Si,
  /// Switching Predicted (H.264).
  Sp,
  /// Bi-predicted intra (BI-frame).
  Bi,
}

/// libavcodec's name for the packet side-data type `kind` — `"New
/// Extradata"`, `"Skip Samples"` — or `None` where this build names no
/// such type (`av_packet_side_data_name`).
///
/// Asked as the integer the type is on the wire, never formed into
/// `AVPacketSideDataType`, whose discriminant set is this build's: a
/// packet from a newer demuxer may carry a type it does not name.
pub fn packet_side_data_name(kind: i32) -> Option<&'static str> {
  // SAFETY: the redeclared shim takes the type as a plain `c_int` and
  // answers null or one of libavcodec's string literals, NUL-terminated
  // and live for the process — the contract `table_text` is given. No
  // name is longer than the bound.
  unsafe {
    crate::ffi::table_text(
      crate::ffi::c_shims::av_packet_side_data_name(kind),
      PACKET_SIDE_DATA_NAME_MAX_BYTES,
    )
  }
}

/// Bound on the NUL search in [`packet_side_data_name`]: libavcodec's
/// longest name is under fifty bytes, and the bound only keeps a skewed
/// table from turning the walk into an unbounded read.
const PACKET_SIDE_DATA_NAME_MAX_BYTES: usize = 256;

/// Raw side-data entry carrying the FFmpeg type id and the unparsed
/// byte buffer. Type ids correspond to FFmpeg's
/// `AV_FRAME_DATA_*` / `AV_PKT_DATA_*` constants — see
/// `libavutil/frame.h` and `libavcodec/packet.h`.
///
/// The payload is an `FfmpegBytes` — see the [module docs](self) for the
/// reason it is not a `Vec<u8>`.
#[derive(Clone, Debug)]
pub struct SideDataEntry {
  kind: i32,
  data: FfmpegBytes,
}

impl SideDataEntry {
  /// Constructs a `SideDataEntry`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(kind: i32, data: FfmpegBytes) -> Self {
    Self { kind, data }
  }

  /// FFmpeg side-data type id.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn kind(&self) -> i32 {
    self.kind
  }
  /// Side-data payload as raw bytes.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn data(&self) -> &[u8] {
    self.data.as_slice()
  }
  /// The payload's carrier, for a consumer that wants to keep the
  /// bytes without copying them again.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn data_ref(&self) -> &FfmpegBytes {
    &self.data
  }

  /// Sets the type id (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_kind(mut self, value: i32) -> Self {
    self.kind = value;
    self
  }
  /// Sets the payload (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub fn with_data(mut self, value: FfmpegBytes) -> Self {
    self.data = value;
    self
  }

  /// Sets the type id in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_kind(&mut self, value: i32) -> &mut Self {
    self.kind = value;
    self
  }
  /// Sets the payload in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn set_data(&mut self, value: FfmpegBytes) -> &mut Self {
    self.data = value;
    self
  }
}

/// HDR10 mastering display metadata and content light level — the
/// vocabulary's own types, re-exported so every path through this module
/// still names them.
pub use mediaspine::hdr::{ContentLightLevel, MasteringDisplay};

// ---------------------------------------------------------------------------
//  The demux tier's carriers.
// ---------------------------------------------------------------------------

/// Per-`DataPacket` extras — timecode, KLV, timed ID3.
///
/// The same three slots as [`VideoPacketExtra`]. The side-data list was
/// left off at first — data demuxers carry their whole payload in the
/// packet body — and then earned its place: a packet with no body and
/// only side data is a real packet, and without this slot its only
/// content would have nowhere to go.
#[derive(Clone, Debug, Default)]
pub struct DataPacketExtra {
  stream_index: i32,
  byte_pos: Option<i64>,
  side_data: Vec<SideDataEntry>,
}

impl DataPacketExtra {
  /// Constructs a `DataPacketExtra` with the given stream index.
  /// `byte_pos` defaults to `None` and `side_data` to empty.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(stream_index: i32) -> Self {
    Self {
      stream_index,
      byte_pos: None,
      side_data: Vec::new(),
    }
  }

  /// Returns the source `AVStream.index`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn stream_index(&self) -> i32 {
    self.stream_index
  }
  /// Returns the byte position of the packet in the input file, or
  /// `None` if unknown.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn byte_pos(&self) -> Option<i64> {
    self.byte_pos
  }
  /// Returns the raw side-data entries from `AVPacket.side_data`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn side_data(&self) -> &[SideDataEntry] {
    self.side_data.as_slice()
  }

  /// Sets the stream index (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_stream_index(mut self, value: i32) -> Self {
    self.stream_index = value;
    self
  }
  /// Sets the byte position (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_byte_pos(mut self, value: Option<i64>) -> Self {
    self.byte_pos = value;
    self
  }
  /// Sets the side-data list (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub fn with_side_data(mut self, value: Vec<SideDataEntry>) -> Self {
    self.side_data = value;
    self
  }

  /// Sets the stream index in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_stream_index(&mut self, value: i32) -> &mut Self {
    self.stream_index = value;
    self
  }
  /// Sets the byte position in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_byte_pos(&mut self, value: Option<i64>) -> &mut Self {
    self.byte_pos = value;
    self
  }
  /// Sets the side-data list in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn set_side_data(&mut self, value: Vec<SideDataEntry>) -> &mut Self {
    self.side_data = value;
    self
  }
}

/// Per-`AttachmentPacket` extras — fonts, cover art.
///
/// `synthesized` records where the payload came from, which is not a
/// detail: an attachment track's single packet is either a real packet
/// the container stores (cover art, which libavformat parks in
/// `AVStream.attached_pic`) or one this crate builds out of the
/// track's codec extradata (fonts, whose bytes never appear in the
/// packet stream at all). A consumer chasing a payload that looks
/// wrong needs to know which.
#[derive(Clone, Debug, Default)]
pub struct AttachmentPacketExtra {
  stream_index: i32,
  synthesized: bool,
}

impl AttachmentPacketExtra {
  /// Constructs an `AttachmentPacketExtra` with the given stream index.
  /// `synthesized` defaults to `false`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(stream_index: i32) -> Self {
    Self {
      stream_index,
      synthesized: false,
    }
  }

  /// Returns the source `AVStream.index`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn stream_index(&self) -> i32 {
    self.stream_index
  }
  /// `true` when the payload was built from the track's codec
  /// extradata rather than taken from a packet the container stores.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn synthesized(&self) -> bool {
    self.synthesized
  }

  /// Sets the stream index (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_stream_index(mut self, value: i32) -> Self {
    self.stream_index = value;
    self
  }
  /// Sets the synthesized flag (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_synthesized(mut self, value: bool) -> Self {
    self.synthesized = value;
    self
  }

  /// Sets the stream index in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_stream_index(&mut self, value: i32) -> &mut Self {
    self.stream_index = value;
    self
  }
  /// Sets the synthesized flag in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_synthesized(&mut self, value: bool) -> &mut Self {
    self.synthesized = value;
    self
  }
}

/// The heap bytes an `AVCodecParameters` holds — measured, never
/// allocated.
///
/// See [`bounded_clone_parameters`] for the rule this measurement
/// serves and for the field-by-field inventory it is derived from.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct ParameterFootprint {
  extradata: usize,
  extradata_payload: usize,
  coded_side_data: usize,
  channel_map: usize,
}

impl ParameterFootprint {
  /// Bytes a copy of `extradata` allocates — the payload **plus** the
  /// `AV_INPUT_BUFFER_PADDING_SIZE` trailing zeroes decoders read past
  /// the end into.
  ///
  /// The padding is counted because it is allocated: a ceiling that
  /// measured only the payload admitted a copy sixty-four bytes larger
  /// than it agreed to, once per stream. Zero when there is no
  /// extradata at all — no payload, no padding.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn extradata(&self) -> usize {
    self.extradata
  }

  /// The `extradata_size` the parameters declare — the payload alone,
  /// with no padding.
  ///
  /// [`Self::extradata`] is what a *parameter clone* costs, padding
  /// included. This is what a **carrier** costs: the synthesized
  /// attachment road copies these bytes into an [`FfmpegBytes`], which
  /// allocates exactly them, so charging the padded figure against the
  /// attachment budget would bill sixty-four bytes nobody allocates —
  /// and reject a payload sitting in the last sixty-four bytes below
  /// the ceiling that the image road, judging the same bytes, accepts.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn extradata_payload(&self) -> usize {
    self.extradata_payload
  }
  /// Bytes across every `coded_side_data` entry, plus the array of
  /// descriptors itself.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn coded_side_data(&self) -> usize {
    self.coded_side_data
  }
  /// Bytes in a custom channel map, or zero for every other layout
  /// order.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn channel_map(&self) -> usize {
    self.channel_map
  }
  /// Everything a clone of these parameters would retain.
  ///
  /// `None` on overflow — a set of declared sizes that cannot be added
  /// up is not a set anything should try to copy.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn total(&self) -> Option<usize> {
    match self.extradata.checked_add(self.coded_side_data) {
      Some(sum) => sum.checked_add(self.channel_map),
      None => None,
    }
  }

  /// The same total with `extradata` left out — what a clone retains
  /// when the caller is going to strip it.
  ///
  /// The synthesized-attachment road does exactly that: a font's
  /// extradata *is* its payload, the carrier already holds it, and
  /// counting it twice would make the budget describe the file rather
  /// than the memory.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn total_without_extradata(&self) -> Option<usize> {
    self.coded_side_data.checked_add(self.channel_map)
  }
}

/// `AVCodecParameters` has thirty-two fields in FFmpeg n9.0 and
/// exactly three of them reach the heap. This tripwire fires if the
/// struct changes shape, because [`measure_parameters`] and
/// [`bounded_clone_parameters`] enumerate those three **by hand** and a
/// fourth would silently go uncounted and uncopied.
///
/// [`CodecTicket`](crate::ticket::CodecTicket) raises the stake: it
/// enumerates **every** field by hand, heap and scalar alike, so a new
/// *scalar* — which the bounded clone's bytewise sweep would have
/// carried for free — is now a slot the mirror would drop. A field
/// that is removed fails the build on its own; a field that is added
/// fails it here.
///
/// A size assertion is a tripwire, not a proof: it catches a struct
/// that grew or was reordered, which is how a new slot arrives in
/// practice. Gated to 64-bit because the number is pointer-width
/// dependent and every target this crate links FFmpeg on is 64-bit;
/// elsewhere the hand-written clone still runs, it just loses the
/// alarm.
#[cfg(target_pointer_width = "64")]
const _: () = {
  assert!(
    core::mem::size_of::<ffmpeg_next::ffi::AVCodecParameters>() == 184,
    "AVCodecParameters changed shape — re-census its fields against \
     `measure_parameters`, `bounded_clone_parameters` and `ticket::CodecTicket` \
     before raising this",
  );
};

/// Measures an `AVCodecParameters`' heap slots without allocating
/// anything.
///
/// `None` when a declared size cannot be added up — a count or length
/// whose arithmetic overflows is malformed, and refusing beats
/// saturating into a number that then passes a budget.
///
/// # Safety
///
/// For the duration of the call, `par` must point at a live, aligned
/// `AVCodecParameters` that meets [the `AVCodecParameters`
/// contract](crate::contracts#avcodecparameters) for a reader that
/// measures. `extradata` is read as a pointer value and `extradata_size`
/// as an integer; `coded_side_data`, when it is not null and
/// `nb_coded_side_data > 0`, must be valid for `nb_coded_side_data`
/// `AVPacketSideData` entries, each of whose `size` is read — no entry's
/// `data` is touched; `ch_layout`'s `order` and `nb_channels` are read as
/// plain integers, and its map is not read.
#[doc(hidden)]
pub unsafe fn measure_parameters(
  par: *const ffmpeg_next::ffi::AVCodecParameters,
) -> Option<ParameterFootprint> {
  // SAFETY: `par` is live per the contract; `extradata` and
  // `extradata_size` are a pointer and an integer.
  let extradata_payload = if unsafe { (*par).extradata }.is_null() {
    0
  } else {
    usize::try_from(unsafe { (*par).extradata_size }).ok()?
  };
  // The padding a *clone* allocates is part of what a clone costs. A
  // carrier of the same bytes allocates only the payload, which is why
  // both numbers are kept.
  let extradata = if extradata_payload == 0 {
    0
  } else {
    extradata_payload.checked_add(ffmpeg_next::ffi::AV_INPUT_BUFFER_PADDING_SIZE as usize)?
  };

  let side_data_ptr = unsafe { (*par).coded_side_data };
  let side_data_count = unsafe { (*par).nb_coded_side_data };
  let coded_side_data = if side_data_ptr.is_null() || side_data_count <= 0 {
    0
  } else {
    let count = usize::try_from(side_data_count).ok()?;
    let mut total = count.checked_mul(core::mem::size_of::<ffmpeg_next::ffi::AVPacketSideData>())?;
    for index in 0..count {
      // **Never `&*entry`.** `AVPacketSideData` carries a `type` field
      // of an *open* C enum: FFmpeg adds side-data kinds between
      // releases, and an ABI-compatible library newer than the
      // bindings this crate was built against will emit values absent
      // from the generated Rust enum. Forming a typed reference to such
      // a struct asserts every field inhabits its declared type, which
      // is undefined behaviour before a single field is read. Only the
      // `size` field is needed here, and it is reached through
      // `addr_of!` and read as the plain `usize` it is.
      //
      // SAFETY: the array is valid for `nb_coded_side_data` contiguous
      // entries per FFmpeg's contract, `index` is below that count, and
      // `addr_of!` computes a field address without forming a reference
      // to the struct that contains it.
      let size =
        unsafe { core::ptr::read_unaligned(core::ptr::addr_of!((*side_data_ptr.add(index)).size)) };
      total = total.checked_add(size)?;
    }
    total
  };

  let channel_map = {
    // The channel order is an open enum too, and this one decides
    // whether the layout owns a heap allocation at all. Read as the
    // integer it is on the wire and compared against the orders whose
    // heap semantics this crate has censused; anything else **fails
    // closed**, because a future order might own memory nobody here
    // knows how to measure and guessing zero would admit it unbudgeted.
    //
    // SAFETY: `ch_layout` is embedded by value; `addr_of!` reaches
    // `order` without forming a reference to the layout, and the field
    // has the layout of a `c_int`.
    let order = unsafe {
      core::ptr::read_unaligned(core::ptr::addr_of!((*par).ch_layout.order).cast::<i32>())
    };
    const UNSPEC: i32 = ffmpeg_next::ffi::AVChannelOrder::AV_CHANNEL_ORDER_UNSPEC as i32;
    const NATIVE: i32 = ffmpeg_next::ffi::AVChannelOrder::AV_CHANNEL_ORDER_NATIVE as i32;
    const CUSTOM: i32 = ffmpeg_next::ffi::AVChannelOrder::AV_CHANNEL_ORDER_CUSTOM as i32;
    const AMBISONIC: i32 = ffmpeg_next::ffi::AVChannelOrder::AV_CHANNEL_ORDER_AMBISONIC as i32;
    match order {
      // These three describe their channels with the union's `mask`
      // arm, which owns nothing.
      UNSPEC | NATIVE | AMBISONIC => 0,
      // Only this one owns a heap map.
      CUSTOM => {
        let channels = usize::try_from(unsafe { (*par).ch_layout.nb_channels }).ok()?;
        channels.checked_mul(core::mem::size_of::<ffmpeg_next::ffi::AVChannelCustom>())?
      }
      // An order this build has never heard of. Refusing is the only
      // honest answer: its union arm is unknown, so both "it owns
      // nothing" and "it owns `nb_channels` of something" are guesses.
      _ => return None,
    }
  };

  Some(ParameterFootprint {
    extradata,
    extradata_payload,
    coded_side_data,
    channel_map,
  })
}

/// Whether a parameter clone carries `extradata` across.
///
/// The synthesized-attachment road omits it: a font's `extradata`
/// **is** the attachment payload, the carrier already holds it, and a
/// second copy is residency nobody asked for.
///
/// **Omitted from the outset, never stripped afterwards.** Stripping
/// was the shape that shipped first and it was wrong twice over: the
/// bytes were allocated before being freed, and — worse — the clone
/// measured and charged them against the *parameter* ceiling on the
/// way past. A payload sitting between the two ceilings (over the
/// 16 MiB parameter one, under the 64 MiB attachment one) therefore
/// passed the session's admission and then failed deterministically
/// inside the clone, refusing a file the budgets had already agreed to
/// open. Omitting from the outset makes the clone's accounting the
/// same accounting the admission did.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash)]
#[doc(hidden)]
pub enum ExtradataPolicy {
  /// Copy `extradata` across — every road but one.
  #[default]
  Copy,
  /// Leave `extradata` behind: never allocated, never charged.
  Omit,
}

/// The `ENOMEM` a heap slot's copy reports when its allocation fails.
///
/// [`ParametersError::Alloc`] names the *destination struct*
/// failing to allocate; a slot that fails after that is the copy
/// failing part way, which is what
/// [`ParametersError::Copy`] was minted for and what
/// `avcodec_parameters_copy` itself would have returned here.
fn slot_copy_failed(stream_index: usize) -> ParametersError {
  ParametersError::Copy(ParametersCopy::new(
    stream_index,
    ffmpeg_next::Error::Other {
      errno: libc::ENOMEM,
    },
  ))
}

/// A deep copy of codec parameters in which **every heap field passes a
/// budget before it is copied**.
///
/// # The rule this exists to enforce
///
/// > **No code path in this crate hands attacker-sized parameter data
/// > to a wholesale FFI copy.**
///
/// `avcodec_parameters_copy` is that wholesale copy, and it was the
/// same defect three review rounds running. It deep-copies every heap
/// field an `AVCodecParameters` has — `extradata`, every
/// `coded_side_data` entry, a custom channel map — and it does so
/// before anything has asked how big they are. The first two rounds
/// patched the field that had been noticed; the third found another
/// (`coded_side_data`, where a MOV `prof` atom puts an ICC profile),
/// which is the signal that patching fields is not a fix. So the
/// wholesale copy is gone from every attacker-facing path and this
/// function replaces it.
///
/// The shape that makes the class unable to recur: the heap slots are
/// enumerated **by hand**, each is measured before it is copied, and
/// the whole footprint is admitted against a budget first. A field
/// nobody enumerated is a field that is not copied — which loses data
/// rather than allocating it — and the compile-time size tripwire above
/// fires when one appears.
///
/// # What it copies
///
/// Every scalar, by a bytewise copy of the struct — so a *scalar* field
/// a future FFmpeg adds travels for free. Then the three heap slots are
/// nulled on the destination (the bytewise copy left them aliasing the
/// source) and rebuilt one at a time:
///
/// 1. `extradata`, with the `AV_INPUT_BUFFER_PADDING_SIZE` trailing
///    zeroes decoders read past the end into;
/// 2. `coded_side_data` — the descriptor array, then each entry's
///    payload;
/// 3. `ch_layout`, through `av_channel_layout_copy`, whose one
///    allocation is the custom map this function has already measured.
///
/// Nothing a decoder consumes is dropped: the SPS/PPS in `extradata`,
/// the side data codecs read, and the channel layout all survive, which
/// is what the decode, resample and image suites prove by continuing to
/// pass unchanged.
#[doc(hidden)]
pub fn bounded_clone_parameters(
  source: &Parameters,
  stream_index: usize,
  budget: usize,
) -> Result<Parameters, ParametersError> {
  bounded_clone_parameters_with(source, stream_index, budget, ExtradataPolicy::Copy)
}

/// [`bounded_clone_parameters`], with the `extradata` policy named.
pub(crate) fn bounded_clone_parameters_with(
  source: &Parameters,
  stream_index: usize,
  budget: usize,
  extradata_policy: ExtradataPolicy,
) -> Result<Parameters, ParametersError> {
  // The *source* first. `Parameters::new()` and `Parameters::default()`
  // hand back a value whose pointer is null when
  // `avcodec_parameters_alloc` failed — safe code, no error, no way to
  // tell — and every read below dereferences it.
  // SAFETY: reading the pointer without dereferencing it.
  let src = unsafe { source.as_ptr() };
  if src.is_null() {
    return Err(ParametersError::Missing(ParametersMissing::new(
      stream_index,
    )));
  }

  // **Structure before size, and long before the copy.**
  //
  // `measure_parameters` prices a custom map from `nb_channels` alone;
  // it does not look at `u.map`. So a layout with a positive count and
  // a null map passed the budget and reached the
  // `av_channel_layout_copy` below, whose `memcpy` reads from that
  // null — and a `NATIVE` count that disagrees with its mask, or an
  // `AMBISONIC` layout whose channels form no ambisonic order, reached
  // the same helper with the same absence of checking. One preflight,
  // shared with the demux admission pass and the decoder, decides all
  // of it and allocates nothing.
  //
  // SAFETY: `src` is a live `AVCodecParameters` owned by `source` for
  // the duration of this call, and for a custom order its map is
  // FFmpeg's own allocation of `nb_channels` entries.
  unsafe { crate::channel_layout::layout_preflight(core::ptr::addr_of!((*src).ch_layout)) }
    .map_err(|fault| ParametersError::from_layout_fault(stream_index, fault))?;

  // SAFETY: `src` is a live `AVCodecParameters` owned by `source` for
  // the duration of this call.
  let footprint = unsafe { measure_parameters(src) }.ok_or_else(|| {
    ParametersError::TooLarge(ParametersTooLarge::new(stream_index, usize::MAX, budget))
  })?;
  // Charged for what this clone will actually retain, which is what
  // the session's admission pass charged for too — see
  // [`ExtradataPolicy`] for the interval that disagreeing about this
  // used to lose.
  let total = match extradata_policy {
    ExtradataPolicy::Copy => footprint.total(),
    ExtradataPolicy::Omit => footprint.total_without_extradata(),
  }
  .ok_or_else(|| {
    ParametersError::TooLarge(ParametersTooLarge::new(stream_index, usize::MAX, budget))
  })?;
  if total > budget {
    return Err(ParametersError::TooLarge(ParametersTooLarge::new(
      stream_index,
      total,
      budget,
    )));
  }

  let mut out = Parameters::new();
  // SAFETY: reading the pointer the constructor stored without
  // dereferencing it — which is exactly what the check is for.
  let dst = unsafe { out.as_mut_ptr() };
  if dst.is_null() {
    return Err(ParametersError::Alloc(ParametersAlloc::new(stream_index)));
  }

  // SAFETY: `src` and `dst` are both live, non-null, distinct
  // `AVCodecParameters` allocations. The bytewise copy carries every
  // scalar across — including any this crate has never heard of — and
  // leaves the three pointer slots aliasing `src`, which the very next
  // statements overwrite before anything can observe or free them.
  unsafe {
    core::ptr::copy_nonoverlapping(src, dst, 1);
    (*dst).extradata = core::ptr::null_mut();
    (*dst).extradata_size = 0;
    (*dst).coded_side_data = core::ptr::null_mut();
    (*dst).nb_coded_side_data = 0;
    // `ch_layout` is zeroed rather than nulled field-by-field:
    // `AV_CHANNEL_ORDER_UNSPEC` is `0`, so an all-zero layout is the
    // valid "nothing here yet" state `av_channel_layout_copy` expects
    // to be handed, and it owns no map.
    (*dst).ch_layout = core::mem::zeroed();
  }

  // 1. extradata, with the padding decoders read into — unless the
  // caller asked for it to be left behind, in which case nothing is
  // allocated and the destination keeps the null the cleanup above put
  // there. `footprint.extradata()` already counts that padding.
  if footprint.extradata() > 0 && matches!(extradata_policy, ExtradataPolicy::Copy) {
    let padded = footprint.extradata();
    // SAFETY: `av_mallocz` returns zeroed memory or null; the copy
    // writes exactly the measured length into an allocation that is
    // `AV_INPUT_BUFFER_PADDING_SIZE` longer, leaving the padding zero.
    unsafe {
      let buffer = ffmpeg_next::ffi::av_mallocz(padded) as *mut u8;
      if buffer.is_null() {
        return Err(slot_copy_failed(stream_index));
      }
      let payload =
        usize::try_from((*src).extradata_size).map_err(|_| slot_copy_failed(stream_index))?;
      core::ptr::copy_nonoverlapping((*src).extradata, buffer, payload);
      (*dst).extradata = buffer;
      (*dst).extradata_size = (*src).extradata_size;
    }
  }

  // 2. coded_side_data — the descriptor array, then each payload.
  //
  // SAFETY: the count and array were measured above; every entry is
  // read within the declared count, and each payload is copied at the
  // length its own descriptor declares.
  unsafe {
    let count = (*src).nb_coded_side_data;
    if count > 0 && !(*src).coded_side_data.is_null() {
      let entries = usize::try_from(count)
        .ok()
        .and_then(|c| c.checked_mul(core::mem::size_of::<ffmpeg_next::ffi::AVPacketSideData>()))
        .ok_or_else(|| slot_copy_failed(stream_index))?;
      let array = ffmpeg_next::ffi::av_mallocz(entries) as *mut ffmpeg_next::ffi::AVPacketSideData;
      if array.is_null() {
        return Err(slot_copy_failed(stream_index));
      }
      // Attached before the payloads are filled in, so a failure part
      // way leaves `out`'s own destructor a well-formed array to walk:
      // the entries it has not reached are zeroed, and freeing a null
      // payload is a no-op.
      (*dst).coded_side_data = array;
      (*dst).nb_coded_side_data = count;
      for index in 0..count as usize {
        // Field pointers, never `&AVPacketSideData` — the `type` field
        // is an open C enum and a newer-but-ABI-compatible FFmpeg emits
        // kinds these bindings do not name. See `measure_parameters`
        // for the whole argument.
        let from = (*src).coded_side_data.add(index);
        let into = array.add(index);
        // The type id travels as the **raw bits it is on the wire**.
        // Reading it as the Rust enum would be the very UB this avoids,
        // and a kind this build cannot name is still a kind the file
        // carries and a decoder may want.
        let kind = core::ptr::read_unaligned(core::ptr::addr_of!((*from).type_).cast::<i32>());
        core::ptr::write_unaligned(core::ptr::addr_of_mut!((*into).type_).cast::<i32>(), kind);

        let size = core::ptr::read_unaligned(core::ptr::addr_of!((*from).size));
        let data = core::ptr::read_unaligned(core::ptr::addr_of!((*from).data));
        if size > 0 && !data.is_null() {
          let payload = ffmpeg_next::ffi::av_mallocz(size) as *mut u8;
          if payload.is_null() {
            return Err(slot_copy_failed(stream_index));
          }
          core::ptr::copy_nonoverlapping(data, payload, size);
          core::ptr::write_unaligned(core::ptr::addr_of_mut!((*into).data), payload);
          core::ptr::write_unaligned(core::ptr::addr_of_mut!((*into).size), size);
        } else {
          core::ptr::write_unaligned(core::ptr::addr_of_mut!((*into).size), 0);
        }
      }
    }
  }

  // 3. ch_layout. One FFmpeg call, over one field whose only allocation
  // is the custom map this function measured and admitted above.
  //
  // SAFETY: both layouts are live; the destination is the zeroed
  // (`AV_CHANNEL_ORDER_UNSPEC`) state this function put it in, which is
  // what `av_channel_layout_copy` requires of a destination it may
  // overwrite.
  let rc = unsafe {
    ffmpeg_next::ffi::av_channel_layout_copy(
      core::ptr::addr_of_mut!((*dst).ch_layout),
      core::ptr::addr_of!((*src).ch_layout),
    )
  };
  if rc < 0 {
    return Err(ParametersError::Copy(ParametersCopy::new(
      stream_index,
      ffmpeg_next::Error::from(rc),
    )));
  }

  Ok(out)
}

/// Per-`TrackInfo` extras — the FFmpeg side of one track-table row.
///
/// Carries the stream's [`CodecTicket`] — the owned mirror of its
/// `AVCodecParameters` — which is what opens a decoder for the track,
/// through [`Self::clone_parameters`]. That call rebuilds a fresh
/// `AVCodecParameters` from the ticket with no tie back to the format
/// context, so a decoder outlives the demuxer that named it.
///
/// # The handle is gone, and `Sync` arrived with it
///
/// This type used to hold an `ffmpeg_next::codec::Parameters` — a
/// `*mut AVCodecParameters` behind a `Send`-but-not-`Sync` wrapper.
/// It was the row's only non-`Sync` field, and through it the whole
/// track table was `!Sync`: `TrackInfo<Ffmpeg>` could not be shared,
/// `Arc<TrackInfo<Ffmpeg>>` was not `Send`, and every consumer that
/// hands a track row to more than one task stopped compiling — for a
/// struct FFmpeg documents as a plain descriptor with no thread
/// affinity at all.
///
/// The answer is the mirror this crate already lives by rather than an
/// `unsafe impl` over FFI: [`CodecTicket`] holds every slot as owned
/// bytes and plain integers, so `Send + Sync` are structural facts and
/// there is no safety argument to get wrong. See
/// `crate::ticket::tests::the_ticket_is_send_and_sync` for the pins,
/// and the ticket's own docs for the parity the rebuild is held to.
///
/// # `Clone`, and the ruling that banned it
///
/// This type refused `Clone` for two releases, on two arguments. Both
/// were about the raw handle, and both died with it.
///
/// The first was safety. A derived `Clone` went through
/// `ffmpeg_next`'s `Clone` for `Parameters`, which checks neither the
/// allocation nor the copy — so safe public code that merely copied a
/// track row could dereference a null destination or receive
/// parameters that were quietly incomplete, and `Clone` has no way to
/// report either. That derive really did ship once, and really was
/// reachable.
///
/// The second was the message-carrier law: a `Clone` is a refcount
/// bump, never a deep copy, and `avcodec_parameters_copy` is not that.
///
/// Neither survives the mirror. There is no `Parameters` left to clone
/// unchecked, so there is nothing for a `Clone` to fail to report; and
/// a copy is now plain owned Rust — a `Vec` spine and refcount bumps
/// over `Arc<[u8]>` — with no FFmpeg allocator anywhere near it, which
/// is exactly what the carrier law asks of a `Clone`. A ban whose
/// whole rationale is spent is ceremony, so the ban is gone, together
/// with the fallible `try_clone` and the infallible `duplicate` that
/// stood in for it.
///
/// **This does not make a track row cheap to copy by accident.**
/// [`TrackInfo`](mediaspine::demuxer::TrackInfo) and its
/// `TrackParams` still have no `Clone` of their own, so the
/// row-sharing law is untouched: a consumer that needs to share a row
/// still wraps it in `Arc` once, at the door — which is now a thing it
/// can actually do, because the row finally became `Sync`.
///
/// No `Default`, though. A track row with no codec parameters
/// describes no track.
///
/// `disposition` is the raw `AV_DISPOSITION_*` bit set, not
/// `ffmpeg_next::format::stream::Disposition`. That type's
/// `from_bits_truncate` drops bits the linked build has no constant
/// for, and this crate's stance on bit sets is that every pattern is a
/// value — the same reason `PacketFlags` reaches the wire as a number.
///
/// [`CodecTicket`]: crate::ticket::CodecTicket
#[derive(Clone)]
pub struct TrackExtra {
  stream_index: i32,
  disposition: i32,
  start_time: Option<i64>,
  frame_count: Option<i64>,
  ticket: CodecTicket,
}

impl TrackExtra {
  /// Constructs a `TrackExtra` from the stream index and the owned
  /// codec ticket. Everything else starts absent.
  ///
  /// **Infallible, and that is the news.** The old constructor
  /// returned a `Result` for one reason: `Parameters::new()` and
  /// `Parameters::default()` are safe constructors over an unchecked
  /// `avcodec_parameters_alloc`, so a caller could hand this type a
  /// null-backed value having never been told. A [`CodecTicket`] has
  /// no such state — it cannot be null-backed and it cannot be
  /// unmeasurable, because [`CodecTicket::mirror`] refused both before
  /// it existed. The check moved to where the raw pointer is, and the
  /// row stopped carrying an error it could no longer raise.
  ///
  /// Not `const fn`: the ticket owns heap.
  ///
  /// [`CodecTicket`]: crate::ticket::CodecTicket
  /// [`CodecTicket::mirror`]: crate::ticket::CodecTicket::mirror
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn new(stream_index: i32, ticket: CodecTicket) -> Self {
    Self {
      stream_index,
      disposition: 0,
      start_time: None,
      frame_count: None,
      ticket,
    }
  }

  /// The heap bytes a rebuild of this row's codec parameters allocates
  /// — `extradata` with its padding, the `coded_side_data` descriptor
  /// array and every entry's payload, and a custom channel map.
  ///
  /// The number the session admitted this stream at, and the ceiling
  /// `DemuxLimits::max_codec_parameter_bytes` was judged against — so
  /// a row that opened is a row whose every
  /// [`Self::clone_parameters`] fits the ceiling it opened under.
  ///
  /// Not the row's own residency: the ticket holds the payload without
  /// FFmpeg's trailing padding and shares its buffers by refcount.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn parameter_bytes(&self) -> usize {
    self.ticket.footprint_bytes()
  }

  /// A live `AVCodecParameters`, rebuilt from the ticket — the handoff
  /// that opens a decoder for this track.
  ///
  /// `FfmpegAudioStreamDecoder::open(track.extra().clone_parameters()?,
  /// track.timebase(), limits)`. Fallible because allocation is: a
  /// failure here is the difference between a decoder that is not
  /// opened and one opened on parameters that are not the file's.
  ///
  /// Every slot the file declared is written back, so what a decoder
  /// receives is what the demuxer read — proved field by field, per
  /// codec, by `tests/codec_ticket_parity.rs`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn clone_parameters(&self) -> Result<Parameters, ParametersError> {
    self.ticket.rebuild()
  }

  /// Returns the source `AVStream.index`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn stream_index(&self) -> i32 {
    self.stream_index
  }
  /// Returns the raw `AVStream.disposition` bit set.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn disposition(&self) -> i32 {
    self.disposition
  }
  /// Returns the stream's start time in the track's timebase, or
  /// `None` when the container does not carry one.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn start_time(&self) -> Option<i64> {
    self.start_time
  }
  /// Returns `AVStream.nb_frames` when the container carries it.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn frame_count(&self) -> Option<i64> {
    self.frame_count
  }
  /// Returns the stream's owned codec ticket — every slot of its
  /// `AVCodecParameters`, and what [`Self::clone_parameters`] rebuilds
  /// a decoder's parameters from.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn ticket(&self) -> &CodecTicket {
    &self.ticket
  }

  /// Sets the disposition bits (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_disposition(mut self, value: i32) -> Self {
    self.disposition = value;
    self
  }
  /// Sets the start time (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_start_time(mut self, value: Option<i64>) -> Self {
    self.start_time = value;
    self
  }
  /// Sets the frame count (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_frame_count(mut self, value: Option<i64>) -> Self {
    self.frame_count = value;
    self
  }

  /// Sets the disposition bits in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_disposition(&mut self, value: i32) -> &mut Self {
    self.disposition = value;
    self
  }
  /// Sets the start time in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_start_time(&mut self, value: Option<i64>) -> &mut Self {
    self.start_time = value;
    self
  }
  /// Sets the frame count in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_frame_count(&mut self, value: Option<i64>) -> &mut Self {
    self.frame_count = value;
    self
  }
}

impl std::fmt::Debug for TrackExtra {
  /// Hand-written so the ticket prints as its own summary rather than
  /// as a wall of bytes — see [`CodecTicket`](crate::ticket::CodecTicket)'s
  /// `Debug`, which prints sizes for the same reason.
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("TrackExtra")
      .field("stream_index", &self.stream_index)
      .field("disposition", &format_args!("{:#x}", self.disposition))
      .field("start_time", &self.start_time)
      .field("frame_count", &self.frame_count)
      .field("ticket", &self.ticket)
      .finish()
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  /// **A packet side-data type is named as libavcodec names it**, asked as
  /// the integer it is; a type this build names none of has no name.
  #[test]
  fn a_packet_side_data_type_is_named_as_libavcodec_names_it() {
    use ffmpeg_next::ffi::AVPacketSideDataType as T;
    assert_eq!(
      packet_side_data_name(T::AV_PKT_DATA_NEW_EXTRADATA as i32),
      Some("New Extradata")
    );
    assert_eq!(
      packet_side_data_name(T::AV_PKT_DATA_SKIP_SAMPLES as i32),
      Some("Skip Samples")
    );
    for unnamed in [-1, T::AV_PKT_DATA_NB as i32, i32::MAX] {
      assert_eq!(packet_side_data_name(unnamed), None, "{unnamed}");
    }
  }

  #[test]
  fn defaults_construct() {
    let v = VideoPacketExtra::default();
    assert_eq!(v.stream_index(), 0);
    assert!(v.side_data().is_empty());

    let f = VideoFrameExtra::default();
    assert_eq!(f.picture_type(), PictureType::Unspecified);
    assert!(!f.key_frame());
    assert!(f.mastering_display().is_none());

    let s = SubtitleFrameExtra::default();
    assert_eq!(s.start_display_time(), 0);
    assert_eq!(s.end_display_time(), 0);
  }

  #[test]
  fn picture_type_default_is_unspecified() {
    assert_eq!(PictureType::default(), PictureType::Unspecified);
  }

  /// Builds an `AVCodecParameters` with the heap slots a file
  /// controls, so the preflight and the bounded clone can be driven
  /// without a container.
  ///
  /// The same discipline as the EXIF fixture: a shape the `ffmpeg` CLI
  /// cannot mint is built here, by hand, beside the assertions it
  /// feeds. A MOV `prof` atom lands in `coded_side_data` as an
  /// `AV_PKT_DATA_ICC_PROFILE` entry — that is the road this
  /// constructs, at whatever size the test asks for.
  fn parameters_with(extradata: usize, icc_profile: usize) -> Parameters {
    let mut out = Parameters::new();
    // SAFETY: `out` owns a live `AVCodecParameters`. Every buffer below
    // comes from FFmpeg's allocator and is handed to it, so
    // `avcodec_parameters_free` releases all of them with the struct.
    unsafe {
      let par = out.as_mut_ptr();
      if extradata > 0 {
        let buffer = ffmpeg_next::ffi::av_mallocz(extradata) as *mut u8;
        assert!(!buffer.is_null(), "av_mallocz extradata");
        (*par).extradata = buffer;
        (*par).extradata_size = extradata as i32;
      }
      if icc_profile > 0 {
        let array = ffmpeg_next::ffi::av_mallocz(core::mem::size_of::<
          ffmpeg_next::ffi::AVPacketSideData,
        >()) as *mut ffmpeg_next::ffi::AVPacketSideData;
        assert!(!array.is_null(), "av_mallocz side-data array");
        let payload = ffmpeg_next::ffi::av_mallocz(icc_profile) as *mut u8;
        assert!(!payload.is_null(), "av_mallocz icc profile");
        (*array).data = payload;
        (*array).size = icc_profile;
        (*array).type_ = ffmpeg_next::ffi::AVPacketSideDataType::AV_PKT_DATA_ICC_PROFILE;
        (*par).coded_side_data = array;
        (*par).nb_coded_side_data = 1;
      }
    }
    out
  }

  fn footprint_of(parameters: &Parameters) -> ParameterFootprint {
    // SAFETY: `parameters` owns a live `AVCodecParameters`.
    unsafe { measure_parameters(parameters.as_ptr()) }.expect("measurable")
  }

  #[test]
  fn the_measurement_counts_every_heap_slot_and_allocates_nothing() {
    // The inventory the bounded clone is written against: `extradata`,
    // `coded_side_data` (payload *and* descriptor array), and a custom
    // channel map. A slot the measurement misses is a slot the budget
    // never sees, which is how this class kept coming back.
    const PAD: usize = ffmpeg_next::ffi::AV_INPUT_BUFFER_PADDING_SIZE as usize;
    const DESCRIPTOR: usize = core::mem::size_of::<ffmpeg_next::ffi::AVPacketSideData>();
    let parameters = parameters_with(4_096, 64 * 1024);
    let footprint = footprint_of(&parameters);
    // The padding a copy allocates is part of what a copy costs: a
    // ceiling that counted only the payload admitted an allocation
    // `AV_INPUT_BUFFER_PADDING_SIZE` larger than it agreed to.
    assert_eq!(footprint.extradata(), 4_096 + PAD);
    assert_eq!(
      footprint.coded_side_data(),
      64 * 1024 + DESCRIPTOR,
      "the descriptor array is an allocation too",
    );
    assert_eq!(footprint.channel_map(), 0, "no custom layout here");
    assert_eq!(
      footprint.total(),
      Some(4_096 + PAD + 64 * 1024 + DESCRIPTOR),
    );
    // The omit-aware total leaves extradata *and its padding* out,
    // which is what keeps a synthesized attachment from being charged
    // for bytes the clone never allocates.
    assert_eq!(
      footprint.total_without_extradata(),
      Some(64 * 1024 + DESCRIPTOR),
    );
    // And no extradata at all means no padding either.
    assert_eq!(footprint_of(&parameters_with(0, 8)).extradata(), 0);
  }

  #[test]
  fn an_oversized_coded_side_data_entry_is_refused_before_the_clone() {
    // The R3 finding, at unit level: `avcodec_parameters_copy` deep-copies
    // every `coded_side_data` entry, and a MOV `prof` atom is where an
    // attacker-sized one arrives. The bounded clone measures first.
    let parameters = parameters_with(0, 8 * 1024 * 1024);
    let declared = footprint_of(&parameters).total().expect("measurable");

    match bounded_clone_parameters(&parameters, 3, 64 * 1024) {
      Err(ParametersError::TooLarge(p)) => {
        assert_eq!(p.stream_index(), 3);
        assert_eq!(p.bytes(), declared);
        assert_eq!(p.limit(), 64 * 1024);
      }
      Err(other) => panic!("expected ParametersTooLarge, got {other:?}"),
      Ok(_) => panic!("an 8 MiB ICC profile passed a 64 KiB ceiling"),
    }

    // Exactly at the line is not over it, and the copy is faithful.
    let cloned =
      bounded_clone_parameters(&parameters, 3, declared).expect("at the cap is not over it");
    assert_eq!(footprint_of(&cloned), footprint_of(&parameters));
  }

  #[test]
  fn a_legitimate_multi_megabyte_icc_profile_is_admitted_by_default() {
    // The honest end of the range this budget has to clear: a real
    // camera or display ICC profile. A ceiling that refused these would
    // be a ceiling nobody could ship behind.
    let parameters = parameters_with(1_024, 4 * 1024 * 1024);
    let cloned = bounded_clone_parameters(
      &parameters,
      0,
      crate::limits::DEFAULT_MAX_CODEC_PARAMETER_BYTES,
    )
    .expect("a 4 MiB ICC profile is real media");
    // And it arrived whole — a clone that silently dropped the profile
    // would pass a size assertion and fail a consumer.
    assert_eq!(footprint_of(&cloned), footprint_of(&parameters));
  }

  #[test]
  fn the_bounded_clone_keeps_every_field_a_decoder_consumes() {
    // Decode-capability parity, asserted directly as well as by the
    // decode suites passing unchanged: scalars, extradata bytes (with
    // the padding decoders read into), and each side-data entry's type
    // and payload all survive.
    let parameters = parameters_with(32, 128);
    // SAFETY: `parameters` owns a live `AVCodecParameters`; the writes
    // below are plain scalar fields.
    unsafe {
      let par = parameters.as_ptr() as *mut ffmpeg_next::ffi::AVCodecParameters;
      (*par).codec_id = ffmpeg_next::ffi::AVCodecID::AV_CODEC_ID_H264;
      (*par).width = 1920;
      (*par).height = 1080;
      (*par).bit_rate = 5_000_000;
      (*par).sample_rate = 48_000;
      core::ptr::write_bytes((*par).extradata, 0xAB, 32);
      core::ptr::write_bytes((*(*par).coded_side_data).data, 0xCD, 128);
    }

    let cloned = bounded_clone_parameters(&parameters, 0, usize::MAX).expect("clone");
    // SAFETY: both own live `AVCodecParameters`.
    unsafe {
      let src = parameters.as_ptr();
      let dst = cloned.as_ptr();
      assert_eq!((*dst).codec_id, (*src).codec_id, "the scalar sweep");
      assert_eq!(((*dst).width, (*dst).height), (1920, 1080));
      assert_eq!((*dst).bit_rate, 5_000_000);
      assert_eq!((*dst).sample_rate, 48_000);

      assert_eq!((*dst).extradata_size, 32);
      assert_ne!(
        (*dst).extradata,
        (*src).extradata,
        "it is a copy, not an alias"
      );
      let extradata = core::slice::from_raw_parts((*dst).extradata, 32);
      assert!(extradata.iter().all(|&b| b == 0xAB), "SPS/PPS survived");
      // The padding a decoder reads past the end into is present and
      // zeroed — `avcodec_parameters_copy` guarantees it and so must we.
      let padded = core::slice::from_raw_parts(
        (*dst).extradata.add(32),
        ffmpeg_next::ffi::AV_INPUT_BUFFER_PADDING_SIZE as usize,
      );
      assert!(padded.iter().all(|&b| b == 0), "the read-past padding");

      assert_eq!((*dst).nb_coded_side_data, 1);
      let entry = &*(*dst).coded_side_data;
      assert_eq!(entry.size, 128);
      assert_eq!(
        entry.type_,
        ffmpeg_next::ffi::AVPacketSideDataType::AV_PKT_DATA_ICC_PROFILE,
      );
      assert_ne!(entry.data, (*(*src).coded_side_data).data, "a copy");
      let payload = core::slice::from_raw_parts(entry.data, 128);
      assert!(
        payload.iter().all(|&b| b == 0xCD),
        "the ICC profile survived"
      );
    }
  }

  #[test]
  fn side_data_entry_carries_bytes() {
    let entry = SideDataEntry::new(12345, FfmpegBytes::copy_from_slice(&[1, 2, 3, 4]));
    assert_eq!(entry.kind(), 12345);
    assert_eq!(entry.data(), &[1, 2, 3, 4]);
  }

  #[test]
  fn side_data_entry_clone_shares_its_payload() {
    // The amputation contract's consumer-side half, one tier in: a
    // frame's metadata clones as cheaply as its pixels do.
    let entry = SideDataEntry::new(7, FfmpegBytes::copy_from_slice(&[9u8; 64]));
    let cloned = entry.clone();
    assert!(
      entry.data_ref().ptr_eq(cloned.data_ref()),
      "cloning a side-data entry copied its bytes",
    );
    assert_eq!(cloned.data(), entry.data());
  }

  /// The eight matrices, exactly as this build of libavcodec emits
  /// them — measured by feeding mjpeg a JPEG whose EXIF IFD carries
  /// each tag in turn, then reading the frame's
  /// `AV_FRAME_DATA_DISPLAYMATRIX` entry back.
  const MEASURED: [(u16, ImageOrientation, [i32; 4]); 8] = [
    (1, ImageOrientation::TopLeft, [65536, 0, 0, 65536]),
    (2, ImageOrientation::TopRight, [-65536, 0, 0, 65536]),
    (3, ImageOrientation::BottomRight, [-65536, 0, 0, -65536]),
    (4, ImageOrientation::BottomLeft, [65536, 0, 0, -65536]),
    (5, ImageOrientation::LeftTop, [0, 65536, 65536, 0]),
    (6, ImageOrientation::RightTop, [0, 65536, -65536, 0]),
    (7, ImageOrientation::RightBottom, [0, -65536, -65536, 0]),
    (8, ImageOrientation::LeftBottom, [0, -65536, 65536, 0]),
  ];

  /// A full nine-word display matrix with `linear` in the four
  /// load-bearing slots and canonical values everywhere else, laid out
  /// as `libavutil/display.h` specifies and in native byte order, as
  /// the side data really is.
  fn display_matrix(linear: [i32; 4]) -> Vec<u8> {
    words_to_bytes([
      linear[0],
      linear[1],
      0,
      linear[2],
      linear[3],
      0,
      0,
      0,
      1 << 30,
    ])
  }

  fn words_to_bytes(words: [i32; 9]) -> Vec<u8> {
    words.iter().flat_map(|w| w.to_ne_bytes()).collect()
  }

  #[test]
  fn every_measured_display_matrix_reads_back_as_its_exif_tag() {
    for (tag, expected, linear) in MEASURED {
      let read = ImageOrientation::from_display_matrix(&display_matrix(linear))
        .expect("a nine-word matrix is readable");
      assert_eq!(read, expected, "tag {tag}");
      assert_eq!(read.to_exif_code(), Some(tag));
      assert_eq!(ImageOrientation::from_exif_code(tag), Some(read));
      // The inverse: the variant knows the matrix it came from.
      assert_eq!(read.linear(), linear, "tag {tag}");
    }
  }

  #[test]
  fn the_four_mirrored_tags_are_the_ones_exif_says_they_are() {
    // 2, 4, 5, 7 — and this is exactly the half `av_display_rotation_get`
    // cannot distinguish, which is why the slot is eight-valued.
    for (tag, orientation, _) in MEASURED {
      assert_eq!(
        orientation.is_mirrored(),
        matches!(tag, 2 | 4 | 5 | 7),
        "tag {tag}",
      );
    }
  }

  #[test]
  fn the_quarter_turn_lands_in_the_workspace_rotation_vocabulary() {
    use ImageOrientation::*;
    assert_eq!(TopLeft.rotation(), Some(Rotation::D0));
    assert_eq!(TopRight.rotation(), Some(Rotation::D0));
    assert_eq!(RightTop.rotation(), Some(Rotation::D90));
    assert_eq!(LeftTop.rotation(), Some(Rotation::D90));
    assert_eq!(BottomRight.rotation(), Some(Rotation::D180));
    assert_eq!(BottomLeft.rotation(), Some(Rotation::D180));
    assert_eq!(LeftBottom.rotation(), Some(Rotation::D270));
    assert_eq!(RightBottom.rotation(), Some(Rotation::D270));
    // Four rotations, eight orientations: the rotation alone cannot
    // tell 1 from 2, which is the whole reason this type exists.
    assert_eq!(TopLeft.rotation(), TopRight.rotation());
    assert_ne!(TopLeft, TopRight);
  }

  #[test]
  fn a_transform_the_vocabulary_cannot_name_is_carried_not_collapsed() {
    // An arbitrary affine linear part — a MOV `tkhd` can carry one.
    // Answering `TopLeft` here is the silent loss this crate refuses.
    let odd = [46_341, 46_341, -46_341, 46_341]; // ~45 degrees
    let words: [i32; 9] = [odd[0], odd[1], 0, odd[2], odd[3], 0, 0, 0, 1 << 30];
    let read =
      ImageOrientation::from_display_matrix(&display_matrix(odd)).expect("readable, just unnamed");
    assert_eq!(read, ImageOrientation::Other(words));
    assert_eq!(read.to_exif_code(), None, "there is no tag to invent");
    assert_eq!(read.rotation(), None, "it is not a quarter turn");
    assert_eq!(read.linear(), odd, "the linear projection still answers");
    assert_eq!(read.matrix(), words, "and nothing was dropped");
    // The determinant still answers, because a reflection is a
    // determinant sign whatever the angle.
    assert!(!read.is_mirrored());
    assert!(ImageOrientation::Other([65536, 0, 0, 0, -65536, 0, 0, 0, 1 << 30]).is_mirrored());
  }

  #[test]
  fn a_noncanonical_word_keeps_a_matrix_out_of_the_named_variants() {
    // The escape-carries-never-collapses law, at the exact place it was
    // being broken: a matrix whose *linear* four say "quarter turn
    // clockwise" but which also translates, or projects, is not tag 6.
    // Reading it as tag 6 throws away the word that made it different.
    //
    // Every non-linear word, one at a time, against a linear part that
    // would otherwise be named.
    let named = ImageOrientation::RightTop;
    let canonical = named.matrix();
    assert_eq!(
      ImageOrientation::from_display_matrix(&words_to_bytes(canonical)),
      Some(named),
      "the canonical matrix must still be named",
    );

    for index in [2usize, 5, 6, 7, 8] {
      let mut forged = canonical;
      // A value that is wrong for that slot: any non-zero for the
      // translation and perspective terms, anything but unity for `w`.
      forged[index] = if index == 8 { 1 << 29 } else { 4_096 };
      let read = ImageOrientation::from_display_matrix(&words_to_bytes(forged))
        .expect("nine words are readable");
      assert_eq!(
        read,
        ImageOrientation::Other(forged),
        "word {index} was collapsed into a named variant",
      );
      assert_eq!(read.to_exif_code(), None, "word {index}");
      assert_eq!(read.matrix(), forged, "word {index} round-trips whole");
      // The four that do carry the orientation are still projectable.
      assert_eq!(read.linear(), named.linear(), "word {index}");
    }
  }

  #[test]
  fn the_escape_round_trips_every_word_losslessly() {
    // Nine distinct, deliberately hostile words: negative, extreme,
    // and nothing canonical anywhere.
    let words: [i32; 9] = [1, -2, 3, -4, 5, -6, i32::MIN, i32::MAX, 0];
    let read = ImageOrientation::from_display_matrix(&words_to_bytes(words)).expect("readable");
    assert_eq!(read, ImageOrientation::Other(words));
    assert_eq!(read.matrix(), words);
    // And back through the bytes again: the escape is a fixed point.
    let again =
      ImageOrientation::from_display_matrix(&words_to_bytes(read.matrix())).expect("readable");
    assert_eq!(again, read);
  }

  #[test]
  fn every_named_orientation_reconstructs_its_canonical_matrix() {
    for (tag, orientation, linear) in MEASURED {
      let matrix = orientation.matrix();
      assert_eq!(
        [matrix[0], matrix[1], matrix[3], matrix[4]],
        linear,
        "tag {tag}",
      );
      assert_eq!(
        [matrix[2], matrix[5], matrix[6], matrix[7]],
        [0, 0, 0, 0],
        "tag {tag}: no translation, no perspective",
      );
      assert_eq!(matrix[8], 1 << 30, "tag {tag}: unity `w`");
      // Round-trip: the reconstruction reads back as the same value.
      assert_eq!(
        ImageOrientation::from_display_matrix(&words_to_bytes(matrix)),
        Some(orientation),
        "tag {tag}",
      );
    }
  }

  #[test]
  fn a_malformed_matrix_is_no_orientation_rather_than_a_guessed_one() {
    assert_eq!(ImageOrientation::from_display_matrix(&[]), None);
    assert_eq!(ImageOrientation::from_display_matrix(&[0u8; 16]), None);
    assert_eq!(ImageOrientation::from_display_matrix(&[0u8; 40]), None);
    // The one length that is right.
    assert_eq!(
      ImageOrientation::DISPLAY_MATRIX_BYTES,
      36,
      "nine int32, per libavutil/display.h",
    );
    assert!(ImageOrientation::from_display_matrix(&[0u8; 36]).is_some());
  }

  #[test]
  fn an_out_of_range_exif_tag_is_refused_not_clamped() {
    for code in [0u16, 9, 255, u16::MAX] {
      assert_eq!(ImageOrientation::from_exif_code(code), None, "code {code}");
    }
  }

  #[test]
  fn the_orientation_slot_rides_the_image_extras() {
    let extra = ImageFrameExtra::default();
    assert_eq!(extra.orientation(), None, "absent until a file says");

    let carried = ImageFrameExtra::new().with_orientation(Some(ImageOrientation::RightTop));
    assert_eq!(carried.orientation(), Some(ImageOrientation::RightTop));

    let mut mutated = carried.clone();
    mutated.set_orientation(None);
    assert_eq!(mutated.orientation(), None);
    assert_eq!(carried.orientation(), Some(ImageOrientation::RightTop));
  }

  #[test]
  fn the_image_household_is_one_slot() {
    let extra = ImageFrameExtra::default();
    assert!(extra.side_data().is_empty());
    let carried = ImageFrameExtra::new().with_side_data(vec![SideDataEntry::new(
      3,
      FfmpegBytes::copy_from_slice(&[1]),
    )]);
    assert_eq!(carried.side_data().len(), 1);
    assert_eq!(carried.side_data()[0].kind(), 3);
    let mut mutated = carried.clone();
    mutated.set_side_data(Vec::new());
    assert!(mutated.side_data().is_empty());
    assert_eq!(carried.side_data().len(), 1);
  }

  #[test]
  fn content_light_level_default_is_zero() {
    let cll = ContentLightLevel::default();
    assert_eq!(cll.max_cll(), 0);
    assert_eq!(cll.max_fall(), 0);
  }

  #[test]
  fn builders_chain() {
    let v = VideoPacketExtra::new(7)
      .with_byte_pos(Some(1234))
      .with_side_data(vec![SideDataEntry::new(
        1,
        FfmpegBytes::copy_from_slice(&[0xAB]),
      )]);
    assert_eq!(v.stream_index(), 7);
    assert_eq!(v.byte_pos(), Some(1234));
    assert_eq!(v.side_data().len(), 1);
  }

  #[test]
  fn setters_chain() {
    let mut v = VideoPacketExtra::default();
    v.set_stream_index(3).set_byte_pos(Some(99));
    assert_eq!(v.stream_index(), 3);
    assert_eq!(v.byte_pos(), Some(99));
  }
}
