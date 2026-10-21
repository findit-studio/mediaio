//! A clip, the medium it reads, and the gain and fades applied to it.

use core::fmt;
use std::vec::Vec;

use mediatime::{Duration, Rate, TimeRange};
#[cfg(feature = "serde")]
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use smol_bytes::Utf8Bytes;

use crate::{Marker, Metadata};

/// A stretch of one medium placed at an explicit position on a track.
///
/// Two ranges place it, each in its own ruler:
///
/// - [`source_range`](Self::source_range) — the stretch of the medium played,
///   counted in the medium's own timebase;
/// - [`record`](Self::record) — where it sits on the timeline, counted at the
///   timeline's edit rate from the timeline's zero.
///
/// The record is **explicit**: it is stored, not derived from the clips
/// before it, so trimming one clip moves no other. With no time-warp in
/// schema 1 the two ranges are one stretch of time:
/// [`validate`](fn@crate::validate) holds the source range to a whole number
/// of edit-rate ticks and the record to exactly that length.
///
/// A clip is identified by its [`id`](Self::id), which its creator mints,
/// unique in the timeline. [`diff`](fn@crate::diff) matches clips by it
/// alone, so a clip renamed or pointed at another medium is still the same
/// clip. Its name and its medium may repeat: a cut back to one shot, or a
/// loop of it, places one medium twice, under two ids. `speed` is reserved
/// for a later schema; it is no part of schema 1.
///
/// Its [`markers`](Self::markers) are counted in its medium's time, as its
/// source range is. On the wire they are written only when there are
/// some, so a clip without any writes the document it wrote before
/// markers existed.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
pub struct Clip {
  id: ClipId,
  name: Utf8Bytes,
  media: MediaRef,
  #[cfg_attr(feature = "serde", serde(deserialize_with = "crate::wire::time_range"))]
  source_range: TimeRange,
  #[cfg_attr(feature = "serde", serde(deserialize_with = "crate::wire::time_range"))]
  record: TimeRange,
  enabled: bool,
  gain: Option<Gain>,
  fades: Fades,
  metadata: Metadata,
  #[cfg_attr(
    feature = "serde",
    serde(default, skip_serializing_if = "Vec::is_empty")
  )]
  markers: Vec<Marker>,
}

impl Clip {
  /// The clip `id`, playing `source_range` of `media` at `record` — enabled,
  /// with no gain, no fades, no metadata and no marker.
  pub fn new(
    id: ClipId,
    name: impl Into<Utf8Bytes>,
    media: MediaRef,
    source_range: TimeRange,
    record: TimeRange,
  ) -> Self {
    Self {
      id,
      name: name.into(),
      media,
      source_range,
      record,
      enabled: true,
      gain: None,
      fades: Fades::new(),
      metadata: Metadata::new(),
      markers: Vec::new(),
    }
  }

  /// The clip's identity, unique in its timeline.
  pub const fn id(&self) -> &ClipId {
    &self.id
  }

  /// The clip's name. Two clips may share one: the [`id`](Self::id) tells
  /// them apart.
  pub fn name(&self) -> &str {
    self.name.as_str()
  }

  /// The medium the clip reads.
  pub const fn media(&self) -> &MediaRef {
    &self.media
  }

  /// The medium the clip reads, for editing in place.
  pub const fn media_mut(&mut self) -> &mut MediaRef {
    &mut self.media
  }

  /// The stretch of the medium played, in the medium's own timebase.
  pub const fn source_range(&self) -> TimeRange {
    self.source_range
  }

  /// Where the clip sits on the timeline: counted at the edit rate, from the
  /// timeline's zero.
  pub const fn record(&self) -> TimeRange {
    self.record
  }

  /// Whether the clip plays. A disabled clip keeps its place and is skipped.
  pub const fn enabled(&self) -> bool {
    self.enabled
  }

  /// The gain applied to the clip's sound, if any.
  pub const fn gain(&self) -> Option<Gain> {
    self.gain
  }

  /// The fades at the clip's two edges.
  pub const fn fades(&self) -> Fades {
    self.fades
  }

  /// The clip's own notes.
  pub const fn metadata(&self) -> &Metadata {
    &self.metadata
  }

  /// The clip's own notes, for editing in place.
  pub const fn metadata_mut(&mut self) -> &mut Metadata {
    &mut self.metadata
  }

  /// The markers noted on the clip, counted in its medium's time.
  pub const fn markers(&self) -> &[Marker] {
    self.markers.as_slice()
  }

  /// The markers, for editing in place.
  pub const fn markers_mut(&mut self) -> &mut Vec<Marker> {
    &mut self.markers
  }

  /// Gives the clip another id in place: a copy placed elsewhere, or the
  /// second half of a clip cut in two, is another clip.
  pub fn set_id(&mut self, id: ClipId) -> &mut Self {
    self.id = id;
    self
  }

  /// Renames the clip in place.
  pub fn set_name(&mut self, name: impl Into<Utf8Bytes>) -> &mut Self {
    self.name = name.into();
    self
  }

  /// Replaces the medium in place.
  pub fn set_media(&mut self, media: MediaRef) -> &mut Self {
    self.media = media;
    self
  }

  /// Sets the source range in place.
  pub const fn set_source_range(&mut self, source_range: TimeRange) -> &mut Self {
    self.source_range = source_range;
    self
  }

  /// Sets the record in place.
  pub const fn set_record(&mut self, record: TimeRange) -> &mut Self {
    self.record = record;
    self
  }

  /// Sets whether the clip plays (consuming builder).
  #[must_use]
  pub fn with_enabled(mut self, enabled: bool) -> Self {
    self.enabled = enabled;
    self
  }

  /// Sets whether the clip plays, in place.
  pub const fn set_enabled(&mut self, enabled: bool) -> &mut Self {
    self.enabled = enabled;
    self
  }

  /// Sets the gain (consuming builder).
  #[must_use]
  pub fn with_gain(mut self, gain: Option<Gain>) -> Self {
    self.gain = gain;
    self
  }

  /// Sets the gain in place.
  pub const fn set_gain(&mut self, gain: Option<Gain>) -> &mut Self {
    self.gain = gain;
    self
  }

  /// Sets the fades (consuming builder).
  #[must_use]
  pub fn with_fades(mut self, fades: Fades) -> Self {
    self.fades = fades;
    self
  }

  /// Sets the fades in place.
  pub const fn set_fades(&mut self, fades: Fades) -> &mut Self {
    self.fades = fades;
    self
  }

  /// Replaces the clip's notes (consuming builder).
  #[must_use]
  pub fn with_metadata(mut self, metadata: Metadata) -> Self {
    self.metadata = metadata;
    self
  }

  /// Appends `marker` (consuming builder).
  #[must_use]
  pub fn with_marker(mut self, marker: Marker) -> Self {
    self.markers.push(marker);
    self
  }
}

/// A clip's identity: a word its creator mints, unique in its timeline.
///
/// The model reads no meaning into an id — a UUID, a counter, an edit
/// plan's own key — beyond that it is not empty. It is minted once per
/// placement: a shot cut back to is two clips with two ids, and so is one
/// medium placed on a video and an audio track.
/// [`validate`](fn@crate::validate) refuses an empty id
/// ([`Refusal::EmptyClipId`](crate::Refusal::EmptyClipId)) and one two
/// clips of a timeline carry
/// ([`Refusal::DuplicateClipId`](crate::Refusal::DuplicateClipId)), and
/// [`diff`](fn@crate::diff) matches clips by it. On the wire an id is the
/// bare string.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(transparent))]
pub struct ClipId(Utf8Bytes);

impl ClipId {
  /// The id `id`.
  pub fn new(id: impl Into<Utf8Bytes>) -> Self {
    Self(id.into())
  }

  /// The id, as the string it is.
  pub fn as_str(&self) -> &str {
    self.0.as_str()
  }
}

/// Writes the id as it is.
impl fmt::Display for ClipId {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(&self.0)
  }
}

/// The medium a clip reads: where to find it, and what is known about it.
///
/// [`locator`](Self::locator) is the medium's address as the caller names it
/// — a path or a URL; the model neither resolves nor rewrites it. The rest is
/// what is known about the medium, each `None` until someone knows it:
///
/// - [`available_range`](Self::available_range) — the stretch the medium
///   holds, in its own timebase. [`validate`](fn@crate::validate) holds a source
///   range and a transition's handles inside it when it is known. It starts
///   where the medium's own clock starts: a medium read through `mediadecode`
///   starts at zero until the read side exposes the container's timecode;
/// - [`rate`](Self::rate) — the medium's frame or sample rate;
/// - [`reel`](Self::reel) — the tape or card name an edit decision list
///   carries.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
pub struct MediaRef {
  locator: Utf8Bytes,
  #[cfg_attr(
    feature = "serde",
    serde(default, deserialize_with = "crate::wire::option_time_range")
  )]
  available_range: Option<TimeRange>,
  #[cfg_attr(
    feature = "serde",
    serde(default, deserialize_with = "crate::wire::option_rate")
  )]
  rate: Option<Rate>,
  reel: Option<Utf8Bytes>,
}

impl MediaRef {
  /// A medium at `locator`, with nothing else known about it.
  pub fn new(locator: impl Into<Utf8Bytes>) -> Self {
    Self {
      locator: locator.into(),
      available_range: None,
      rate: None,
      reel: None,
    }
  }

  /// The medium's address.
  pub fn locator(&self) -> &str {
    self.locator.as_str()
  }

  /// The stretch the medium holds, in its own timebase, when known.
  pub const fn available_range(&self) -> Option<TimeRange> {
    self.available_range
  }

  /// The medium's frame or sample rate, when known.
  pub const fn rate(&self) -> Option<Rate> {
    self.rate
  }

  /// The medium's reel name, when known.
  pub fn reel(&self) -> Option<&str> {
    match &self.reel {
      Some(reel) => Some(reel.as_str()),
      None => None,
    }
  }

  /// Sets the address in place.
  pub fn set_locator(&mut self, locator: impl Into<Utf8Bytes>) -> &mut Self {
    self.locator = locator.into();
    self
  }

  /// Sets the available range (consuming builder).
  #[must_use]
  pub fn with_available_range(mut self, available_range: Option<TimeRange>) -> Self {
    self.available_range = available_range;
    self
  }

  /// Sets the available range in place.
  pub const fn set_available_range(&mut self, available_range: Option<TimeRange>) -> &mut Self {
    self.available_range = available_range;
    self
  }

  /// Sets the rate (consuming builder).
  #[must_use]
  pub fn with_rate(mut self, rate: Option<Rate>) -> Self {
    self.rate = rate;
    self
  }

  /// Sets the rate in place.
  pub const fn set_rate(&mut self, rate: Option<Rate>) -> &mut Self {
    self.rate = rate;
    self
  }

  /// Sets the reel name (consuming builder).
  #[must_use]
  pub fn with_reel(mut self, reel: Option<Utf8Bytes>) -> Self {
    self.reel = reel;
    self
  }

  /// Sets the reel name in place.
  pub fn set_reel(&mut self, reel: Option<Utf8Bytes>) -> &mut Self {
    self.reel = reel;
    self
  }
}

/// The fades at a clip's two edges: a fade-in from its start and a fade-out
/// to its end, each optional.
///
/// A fade runs inside the clip's record — a fade-in over its first
/// [`duration`](Fade::duration), a fade-out over its last — so it needs no
/// media beyond the source range. On the wire the two edges are `in` and
/// `out`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
pub struct Fades {
  #[cfg_attr(feature = "serde", serde(rename = "in"))]
  in_: Option<Fade>,
  out: Option<Fade>,
}

impl Fades {
  /// No fade at either edge.
  pub const fn new() -> Self {
    Self {
      in_: None,
      out: None,
    }
  }

  /// The fade-in from the clip's start, if any.
  pub const fn in_(&self) -> Option<Fade> {
    self.in_
  }

  /// The fade-out to the clip's end, if any.
  pub const fn out(&self) -> Option<Fade> {
    self.out
  }

  /// Sets the fade-in (consuming builder).
  #[must_use]
  pub const fn with_in(mut self, fade: Option<Fade>) -> Self {
    self.in_ = fade;
    self
  }

  /// Sets the fade-out (consuming builder).
  #[must_use]
  pub const fn with_out(mut self, fade: Option<Fade>) -> Self {
    self.out = fade;
    self
  }

  /// Sets the fade-in in place.
  pub const fn set_in(&mut self, fade: Option<Fade>) -> &mut Self {
    self.in_ = fade;
    self
  }

  /// Sets the fade-out in place.
  pub const fn set_out(&mut self, fade: Option<Fade>) -> &mut Self {
    self.out = fade;
    self
  }
}

/// One fade: how long it runs, counted at the edit rate, and its curve.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
pub struct Fade {
  #[cfg_attr(feature = "serde", serde(deserialize_with = "crate::wire::duration"))]
  duration: Duration,
  shape: FadeShape,
}

impl Fade {
  /// A fade running `duration` along `shape`.
  pub const fn new(duration: Duration, shape: FadeShape) -> Self {
    Self { duration, shape }
  }

  /// How long the fade runs, counted at the edit rate.
  pub const fn duration(&self) -> Duration {
    self.duration
  }

  /// The fade's curve.
  pub const fn shape(&self) -> FadeShape {
    self.shape
  }
}

/// The curve a fade follows.
///
/// Marked `#[non_exhaustive]`: a later curve joins as a variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, derive_more::IsVariant, derive_more::Display)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
#[non_exhaustive]
pub enum FadeShape {
  /// Level changes evenly with time.
  #[display("linear")]
  Linear,
  /// Power changes evenly with time — the curve that keeps a crossfade of
  /// uncorrelated sound at a steady loudness.
  #[display("equal_power")]
  EqualPower,
}

/// A gain in decibels, finite by construction.
///
/// `0.0` is unity. A gain that is not finite has no level to apply and no
/// JSON number to write, so neither the constructor nor the reader admits
/// one; silence is a disabled clip. On the wire a gain is the bare number.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct Gain(f32);

// Every `Gain` is finite, so `==` is reflexive.
impl Eq for Gain {}

impl Gain {
  /// Unity gain: `0.0` dB.
  pub const UNITY: Self = Self(0.0);

  /// A gain of `db` decibels, or `None` when `db` is not finite.
  pub const fn from_db(db: f32) -> Option<Self> {
    if db.is_finite() { Some(Self(db)) } else { None }
  }

  /// The gain in decibels.
  pub const fn db(self) -> f32 {
    self.0
  }
}

/// Writes the gain as `-6 dB`.
impl fmt::Display for Gain {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "{} dB", self.0)
  }
}

#[cfg(feature = "serde")]
const _: () = {
  impl Serialize for Gain {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
      serializer.serialize_f32(self.0)
    }
  }

  impl<'de> Deserialize<'de> for Gain {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
      let db = f32::deserialize(deserializer)?;
      Self::from_db(db).ok_or_else(|| de::Error::custom(format_args!("gain {db} dB is not finite")))
    }
  }
};
