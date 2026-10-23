//! A marker: a named point or stretch of time on the timeline, a track or a
//! clip.

use mediatime::TimeRange;
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};
use smol_bytes::Utf8Bytes;

use crate::ReaderMetadata;

/// A named point or stretch of time noted on the
/// [`Timeline`](crate::Timeline), a [`Track`](crate::Track) or a
/// [`Clip`](crate::Clip): OpenTimelineIO's `Marker`.
///
/// Its [`range`](Self::range) is counted where its owner counts time:
///
/// - the timeline's marker and a track's at the timeline's edit rate, from
///   the timeline's zero, as a track's records are;
/// - a clip's marker in its medium's time, as the clip's source range is —
///   OpenTimelineIO's `marked_range` on an item, in the item's own time.
///
/// A range of no length marks a point. [`validate`](fn@crate::validate)
/// holds the timeline's marker and a track's to the edit rate, a clip's to
/// a real ruler, and each to a range no longer than `i64::MAX` ticks.
/// Nothing holds a marker inside its owner's time: a clip's marker past its
/// source range notes media the clip does not play.
///
/// Its [`readers`](Self::readers) are other readers' own metadata, written
/// in OpenTimelineIO's `metadata` beside this crate's words: DaVinci
/// Resolve reads a marker's note from `Resolve_OTIO`'s `Note`. On the wire
/// they are written only when there are some.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
pub struct Marker {
  name: Utf8Bytes,
  #[cfg_attr(feature = "serde", serde(deserialize_with = "crate::wire::time_range"))]
  range: TimeRange,
  color: MarkerColor,
  comment: Utf8Bytes,
  #[cfg_attr(
    feature = "serde",
    serde(default, skip_serializing_if = "ReaderMetadata::is_empty")
  )]
  readers: ReaderMetadata,
}

impl Marker {
  /// A marker named `name` over `range`: green, as OpenTimelineIO's are
  /// by default, with no comment and no reader's metadata.
  pub fn new(name: impl Into<Utf8Bytes>, range: TimeRange) -> Self {
    Self {
      name: name.into(),
      range,
      color: MarkerColor::Green,
      comment: Utf8Bytes::new(),
      readers: ReaderMetadata::new(),
    }
  }

  /// The marker's name.
  pub fn name(&self) -> &str {
    self.name.as_str()
  }

  /// The stretch of time it marks, counted where its owner counts time.
  pub const fn range(&self) -> TimeRange {
    self.range
  }

  /// Its color.
  pub const fn color(&self) -> MarkerColor {
    self.color
  }

  /// Its comment; empty when it has none.
  pub fn comment(&self) -> &str {
    self.comment.as_str()
  }

  /// Other readers' own metadata on the marker.
  pub const fn readers(&self) -> &ReaderMetadata {
    &self.readers
  }

  /// Other readers' own metadata on the marker, for editing in place.
  pub const fn readers_mut(&mut self) -> &mut ReaderMetadata {
    &mut self.readers
  }

  /// Renames the marker in place.
  pub fn set_name(&mut self, name: impl Into<Utf8Bytes>) -> &mut Self {
    self.name = name.into();
    self
  }

  /// Sets the marked range in place.
  pub const fn set_range(&mut self, range: TimeRange) -> &mut Self {
    self.range = range;
    self
  }

  /// Sets the color (consuming builder).
  #[must_use]
  pub const fn with_color(mut self, color: MarkerColor) -> Self {
    self.color = color;
    self
  }

  /// Sets the color in place.
  pub const fn set_color(&mut self, color: MarkerColor) -> &mut Self {
    self.color = color;
    self
  }

  /// Sets the comment (consuming builder).
  #[must_use]
  pub fn with_comment(mut self, comment: impl Into<Utf8Bytes>) -> Self {
    self.comment = comment.into();
    self
  }

  /// Sets the comment in place.
  pub fn set_comment(&mut self, comment: impl Into<Utf8Bytes>) -> &mut Self {
    self.comment = comment.into();
    self
  }

  /// Replaces other readers' own metadata (consuming builder).
  #[must_use]
  pub fn with_readers(mut self, readers: ReaderMetadata) -> Self {
    self.readers = readers;
    self
  }
}

/// A marker's color: OpenTimelineIO's eleven.
///
/// OpenTimelineIO spells each in capitals (`GREEN`), and a document reader
/// matches the name ignoring its case; the stored document writes it as
/// [`Display`](core::fmt::Display) does, in lower case (`green`).
///
/// Marked `#[non_exhaustive]`: a later color joins as a variant.
#[derive(
  Debug, Clone, Copy, Default, PartialEq, Eq, Hash, derive_more::IsVariant, derive_more::Display,
)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
#[non_exhaustive]
pub enum MarkerColor {
  /// Pink.
  #[display("pink")]
  Pink,
  /// Red.
  #[display("red")]
  Red,
  /// Orange.
  #[display("orange")]
  Orange,
  /// Yellow.
  #[display("yellow")]
  Yellow,
  /// Green: OpenTimelineIO's default.
  #[default]
  #[display("green")]
  Green,
  /// Cyan.
  #[display("cyan")]
  Cyan,
  /// Blue.
  #[display("blue")]
  Blue,
  /// Purple.
  #[display("purple")]
  Purple,
  /// Magenta.
  #[display("magenta")]
  Magenta,
  /// Black.
  #[display("black")]
  Black,
  /// White.
  #[display("white")]
  White,
}

impl MarkerColor {
  /// Every color, in OpenTimelineIO's order.
  pub const ALL: [Self; 11] = [
    Self::Pink,
    Self::Red,
    Self::Orange,
    Self::Yellow,
    Self::Green,
    Self::Cyan,
    Self::Blue,
    Self::Purple,
    Self::Magenta,
    Self::Black,
    Self::White,
  ];

  /// The name OpenTimelineIO writes for the color: `GREEN`.
  pub(crate) const fn otio_name(self) -> &'static str {
    match self {
      Self::Pink => "PINK",
      Self::Red => "RED",
      Self::Orange => "ORANGE",
      Self::Yellow => "YELLOW",
      Self::Green => "GREEN",
      Self::Cyan => "CYAN",
      Self::Blue => "BLUE",
      Self::Purple => "PURPLE",
      Self::Magenta => "MAGENTA",
      Self::Black => "BLACK",
      Self::White => "WHITE",
    }
  }

  /// The color whose OpenTimelineIO name is `name`, exactly as
  /// OpenTimelineIO writes it — capitals — or `None` for any other: its
  /// reader keeps the string as written (`marker.cpp` 27 in 0.17.0), so
  /// `Green` is not `GREEN`.
  pub(crate) fn from_otio_name(name: &str) -> Option<Self> {
    Self::ALL
      .into_iter()
      .find(|color| color.otio_name() == name)
  }
}
