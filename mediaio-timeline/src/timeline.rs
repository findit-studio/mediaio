//! The timeline and its tracks.

use std::vec::Vec;

use mediatime::{Rate, Timebase, Timestamp};
use smol_bytes::Utf8Bytes;

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

use crate::{Clip, Marker, Metadata, ReaderMetadata, Schema, Transition};

/// An edit: tracks of clips at explicit positions, counted at one edit rate.
///
/// Every position on the timeline — a clip's record, a transition's cut, a
/// fade's length — is counted at [`rate`](Self::rate), from the timeline's
/// zero. [`start`](Self::start) is the timecode that zero carries
/// (`01:00:00:00` is 86 400 frames at 24 fps); it labels the timeline and
/// moves nothing on it.
///
/// The value is plain data: building one checks nothing, and
/// [`validate`](fn@crate::validate) is where a timeline is judged. Tracks stack
/// in order, the first at the bottom.
///
/// Its own [`markers`](Self::markers) are counted as a track's are: at the
/// edit rate, from the timeline's zero. OpenTimelineIO writes them on its
/// stack, from the stack's zero, and the stack's markers are the ones
/// DaVinci Resolve 21.1 imports as its timeline's: it drops a track's, and
/// attaches a clip's to its item (findit-studio/application#265).
///
/// On the wire the document's first field is [`schema`](Self::schema), and
/// a field this reader does not know is refused by name. Its
/// [`markers`](Self::markers), and its [`readers`](Self::readers) — other
/// readers' own metadata — are written only when there are some.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
pub struct Timeline {
  schema: Schema,
  name: Utf8Bytes,
  #[cfg_attr(feature = "serde", serde(deserialize_with = "crate::wire::rate"))]
  rate: Rate,
  #[cfg_attr(feature = "serde", serde(deserialize_with = "crate::wire::timestamp"))]
  start: Timestamp,
  metadata: Metadata,
  tracks: Vec<Track>,
  #[cfg_attr(
    feature = "serde",
    serde(default, skip_serializing_if = "Vec::is_empty")
  )]
  markers: Vec<Marker>,
  #[cfg_attr(
    feature = "serde",
    serde(default, skip_serializing_if = "ReaderMetadata::is_empty")
  )]
  readers: ReaderMetadata,
}

impl Timeline {
  /// An empty timeline at the edit rate `rate`, its zero labelled with the
  /// timecode zero: no track, no marker.
  ///
  /// The start is counted at `rate`; for a rate of zero, which
  /// [`validate`](fn@crate::validate) refuses, it is counted in `1/1`.
  pub fn new(name: impl Into<Utf8Bytes>, rate: Rate) -> Self {
    let ruler = rate.checked_to_timebase().unwrap_or_default();
    Self {
      schema: Schema::V1,
      name: name.into(),
      rate,
      start: Timestamp::new(0, ruler),
      metadata: Metadata::new(),
      tracks: Vec::new(),
      markers: Vec::new(),
      readers: ReaderMetadata::new(),
    }
  }

  /// Which shape of document this is.
  pub const fn schema(&self) -> Schema {
    self.schema
  }

  /// The timeline's name.
  pub fn name(&self) -> &str {
    self.name.as_str()
  }

  /// The edit rate every position on the timeline is counted at.
  pub const fn rate(&self) -> Rate {
    self.rate
  }

  /// The timebase positions are counted in — the edit rate's reciprocal —
  /// or `None` when the rate is zero and has none.
  pub const fn edit_timebase(&self) -> Option<Timebase> {
    self.rate.checked_to_timebase()
  }

  /// The timecode the timeline's zero carries.
  pub const fn start(&self) -> Timestamp {
    self.start
  }

  /// The timeline's own notes.
  pub const fn metadata(&self) -> &Metadata {
    &self.metadata
  }

  /// The timeline's own notes, for editing in place.
  pub const fn metadata_mut(&mut self) -> &mut Metadata {
    &mut self.metadata
  }

  /// Other readers' own metadata on the timeline.
  pub const fn readers(&self) -> &ReaderMetadata {
    &self.readers
  }

  /// Other readers' own metadata on the timeline, for editing in place.
  pub const fn readers_mut(&mut self) -> &mut ReaderMetadata {
    &mut self.readers
  }

  /// The tracks, bottom first.
  pub const fn tracks(&self) -> &[Track] {
    self.tracks.as_slice()
  }

  /// The tracks, for editing in place.
  pub const fn tracks_mut_slice(&mut self) -> &mut [Track] {
    self.tracks.as_mut_slice()
  }

  /// The tracks, for editing in place as a `Vec<Track>`.
  pub const fn tracks_mut(&mut self) -> &mut Vec<Track> {
    &mut self.tracks
  }

  /// The markers noted on the timeline itself, counted at the edit rate
  /// from its zero.
  pub const fn markers(&self) -> &[Marker] {
    self.markers.as_slice()
  }

  /// The timeline's markers, for editing in place.
  pub const fn markers_mut(&mut self) -> &mut Vec<Marker> {
    &mut self.markers
  }

  /// Renames the timeline in place.
  pub fn set_name(&mut self, name: impl Into<Utf8Bytes>) -> &mut Self {
    self.name = name.into();
    self
  }

  /// Sets the edit rate in place. Nothing is recounted: positions keep their
  /// counts, and [`validate`](fn@crate::validate) refuses those not counted at
  /// the new rate.
  pub const fn set_rate(&mut self, rate: Rate) -> &mut Self {
    self.rate = rate;
    self
  }

  /// Sets the timecode the timeline's zero carries (consuming builder).
  #[must_use]
  pub fn with_start(mut self, start: Timestamp) -> Self {
    self.start = start;
    self
  }

  /// Sets the timecode the timeline's zero carries, in place.
  pub const fn set_start(&mut self, start: Timestamp) -> &mut Self {
    self.start = start;
    self
  }

  /// Replaces the timeline's notes (consuming builder).
  #[must_use]
  pub fn with_metadata(mut self, metadata: Metadata) -> Self {
    self.metadata = metadata;
    self
  }

  /// Replaces other readers' own metadata on the timeline (consuming
  /// builder).
  #[must_use]
  pub fn with_readers(mut self, readers: ReaderMetadata) -> Self {
    self.readers = readers;
    self
  }

  /// Appends `track` on top of the stack (consuming builder).
  #[must_use]
  pub fn with_track(mut self, track: Track) -> Self {
    self.tracks.push(track);
    self
  }

  /// Replaces the tracks, replacing any existing ones (consuming builder).
  #[must_use]
  pub fn with_tracks(mut self, tracks: impl Into<Vec<Track>>) -> Self {
    self.tracks = tracks.into();
    self
  }

  /// Sets the tracks, replacing any existing ones, in place.
  pub fn set_tracks(&mut self, tracks: impl Into<Vec<Track>>) -> &mut Self {
    self.tracks = tracks.into();
    self
  }

  /// Appends `marker` to the timeline's own (consuming builder).
  #[must_use]
  pub fn with_marker(mut self, marker: Marker) -> Self {
    self.markers.push(marker);
    self
  }
}

/// One layer of a timeline: clips of one kind of medium, in record order,
/// the transitions between them, and the markers noted on it.
///
/// A track's [`markers`](Self::markers) are counted as its records are: at
/// the edit rate, from the timeline's zero. On the wire they are written
/// only when there are some, so a track without any writes the document
/// it wrote before markers existed; so are its [`readers`](Self::readers),
/// other readers' own metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
pub struct Track {
  kind: TrackKind,
  name: Utf8Bytes,
  enabled: bool,
  clips: Vec<Clip>,
  transitions: Vec<Transition>,
  #[cfg_attr(
    feature = "serde",
    serde(default, skip_serializing_if = "Vec::is_empty")
  )]
  markers: Vec<Marker>,
  #[cfg_attr(
    feature = "serde",
    serde(default, skip_serializing_if = "ReaderMetadata::is_empty")
  )]
  readers: ReaderMetadata,
}

impl Track {
  /// An empty, enabled track of `kind`, with no marker and no reader's
  /// metadata.
  pub fn new(kind: TrackKind, name: impl Into<Utf8Bytes>) -> Self {
    Self {
      kind,
      name: name.into(),
      enabled: true,
      clips: Vec::new(),
      transitions: Vec::new(),
      markers: Vec::new(),
      readers: ReaderMetadata::new(),
    }
  }

  /// The kind of medium the track carries.
  pub const fn kind(&self) -> TrackKind {
    self.kind
  }

  /// The track's name.
  pub fn name(&self) -> &str {
    self.name.as_str()
  }

  /// Whether the track plays. A disabled track keeps its clips and is
  /// skipped.
  pub const fn enabled(&self) -> bool {
    self.enabled
  }

  /// The clips, in record order.
  pub const fn clips(&self) -> &[Clip] {
    self.clips.as_slice()
  }

  /// The clips, for editing in place.
  pub const fn clips_mut(&mut self) -> &mut Vec<Clip> {
    &mut self.clips
  }

  /// The transitions between adjacent clips.
  pub const fn transitions(&self) -> &[Transition] {
    self.transitions.as_slice()
  }

  /// The transitions, for editing in place.
  pub const fn transitions_mut(&mut self) -> &mut Vec<Transition> {
    &mut self.transitions
  }

  /// The markers noted on the track, counted at the edit rate from the
  /// timeline's zero.
  pub const fn markers(&self) -> &[Marker] {
    self.markers.as_slice()
  }

  /// The markers, for editing in place.
  pub const fn markers_mut(&mut self) -> &mut Vec<Marker> {
    &mut self.markers
  }

  /// Other readers' own metadata on the track.
  pub const fn readers(&self) -> &ReaderMetadata {
    &self.readers
  }

  /// Other readers' own metadata on the track, for editing in place.
  pub const fn readers_mut(&mut self) -> &mut ReaderMetadata {
    &mut self.readers
  }

  /// Renames the track in place.
  pub fn set_name(&mut self, name: impl Into<Utf8Bytes>) -> &mut Self {
    self.name = name.into();
    self
  }

  /// Sets whether the track plays (consuming builder).
  #[must_use]
  pub fn with_enabled(mut self, enabled: bool) -> Self {
    self.enabled = enabled;
    self
  }

  /// Sets whether the track plays, in place.
  pub const fn set_enabled(&mut self, enabled: bool) -> &mut Self {
    self.enabled = enabled;
    self
  }

  /// Appends `clip` (consuming builder).
  #[must_use]
  pub fn with_clip(mut self, clip: Clip) -> Self {
    self.clips.push(clip);
    self
  }

  /// Appends `transition` (consuming builder).
  #[must_use]
  pub fn with_transition(mut self, transition: Transition) -> Self {
    self.transitions.push(transition);
    self
  }

  /// Appends `marker` (consuming builder).
  #[must_use]
  pub fn with_marker(mut self, marker: Marker) -> Self {
    self.markers.push(marker);
    self
  }

  /// Replaces other readers' own metadata on the track (consuming builder).
  #[must_use]
  pub fn with_readers(mut self, readers: ReaderMetadata) -> Self {
    self.readers = readers;
    self
  }
}

/// The kind of medium a track carries.
///
/// Marked `#[non_exhaustive]`: a later kind joins as a variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, derive_more::IsVariant, derive_more::Display)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
#[non_exhaustive]
pub enum TrackKind {
  /// Pictures.
  #[display("video")]
  Video,
  /// Sound.
  #[display("audio")]
  Audio,
}
