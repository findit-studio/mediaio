//! The demux tier's types — the track table, the five-arm packet
//! envelope, and the adapter bundle a demuxer is written against.
//!
//! A container file is a bundle of *tracks*; reading it produces a
//! single interleaved stream of *packets*, each belonging to one track.
//! This module names both halves: [`TrackInfo`] describes a track and
//! [`DemuxedPacket`] delivers one packet with its track coordinate
//! attached. A container may also carry a table of contents over the
//! whole timeline rather than over any one track — [`Chapter`].
//!
//! The session that hands these out is the `Demuxer` face in
//! `mediadecode`, a pull loop over an opened container. Its contract —
//! delivery order, the attachment rule, the three seeking laws, the
//! track and chapter tables, the layered durations — is stated there;
//! the types here are what it speaks.

use core::fmt::{self, Debug};

use derive_more::{IsVariant, TryUnwrap, Unwrap};

use crate::{
  Timebase, Timestamp,
  adapter::{AudioAdapter, SubtitleAdapter, VideoAdapter},
  packet::{AudioPacket, PacketFlags, SubtitlePacket, VideoPacket},
};

/// A track's disposition: the `AV_DISPOSITION_*` hints a container
/// declares for it — the default track of its kind, a forced subtitle
/// track, commentary, the hearing-impaired variant, cover art.
///
/// `mediaframe`'s bit set, carried whole: an unnamed bit round-trips
/// as the bit it is (`from_u32` keeps every bit), so a container's
/// newer flag survives a backend older than it.
pub use mediaframe::disposition::TrackDisposition;

// Nothing in this module owns an allocation: the whole demux tier,
// `Demuxer` included, is `core`-only. What a session's track table
// costs is the backend's own choice, made at `Demuxer::TrackHandle`
// — a heap-backed backend binds a refcounted handle, one that parses
// in place binds a borrow.
//
// `alloc` is bound here for the tests' `format!` alone, and is scoped
// to this module so a reader can see exactly what needs the heap and
// when.
#[cfg(all(test, any(feature = "std", feature = "alloc")))]
extern crate alloc;

/// A track's position in the table `Demuxer::tracks` returns.
///
/// `TrackIndex(i)` **is** the index of `tracks()[i]` — the coordinate
/// and the table position are the same number by contract, so a
/// consumer that has a packet's track can look up its description
/// without a side map. Backends whose native identifiers are sparse or
/// unordered are responsible for the translation.
#[repr(transparent)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TrackIndex(usize);

impl TrackIndex {
  /// Constructs a `TrackIndex` from a position in the track table.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(index: usize) -> Self {
    Self(index)
  }

  /// Returns the position in the track table.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn get(self) -> usize {
    self.0
  }
}

/// What a track carries.
///
/// A closed roster: these six are every kind a container can present,
/// and [`Unknown`](Self::Unknown) is the honest answer for a track the
/// backend cannot classify rather than an escape hatch for new kinds.
/// Minted here rather than borrowed: `mediaframe` has a track
/// *disposition* vocabulary but no kind vocabulary, and the dependency
/// direction forbids reaching the other way.
///
/// **Cover art is [`Attachment`](Self::Attachment), not
/// [`Video`](Self::Video).** A still image stored in a container's
/// video-shaped slot is an attachment by every property that matters —
/// one sample, no timeline, no motion — so the `Video` arm carries true
/// motion video and nothing else. See [`DemuxedPacket`] for the
/// delivery contract that follows from this.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, IsVariant)]
pub enum TrackKind {
  /// Motion video.
  Video,
  /// Audio.
  Audio,
  /// Subtitles / captions, text or bitmap.
  Subtitle,
  /// Timed opaque data — timecode, KLV, timed ID3.
  Data,
  /// A file carried inside the container: a font, cover art.
  Attachment,
  /// The backend could not classify this track.
  #[default]
  Unknown,
}

/// Backend vocabulary for the **demux tier**.
///
/// Bundles the three decoding families ([`VideoAdapter`],
/// [`AudioAdapter`], [`SubtitleAdapter`]) that already exist and adds
/// the seats only a demuxer needs: extras for the two packet kinds that
/// have no decoder face, extras for a track-table row, and the text
/// carrier for a track's identity metadata.
///
/// The three families are bound to share this adapter's
/// [`CodecId`](Self::CodecId). A demuxer reads one container, and a
/// container's track table has one codec-identifier column — the same
/// namespace names its video, its audio, its subtitles, and its cover
/// art. Keeping them one type is what lets a consumer take the codec
/// identifier off an [`Attachment`](TrackKind::Attachment) track
/// carrying cover art and hand it to a video decoder.
pub trait DemuxAdapter {
  /// Codec identifier for every track in the table, whatever its kind
  /// (e.g. a newtype around FFmpeg's `AVCodecID`, a WebCodecs codec
  /// string, a container fourcc).
  type CodecId: Copy + Eq + Debug;

  /// Vocabulary for the container's video tracks.
  type Video: VideoAdapter<CodecId = Self::CodecId>;
  /// Vocabulary for the container's audio tracks.
  type Audio: AudioAdapter<CodecId = Self::CodecId>;
  /// Vocabulary for the container's subtitle tracks.
  type Subtitle: SubtitleAdapter<CodecId = Self::CodecId>;

  /// Backend-specific extras carried on every [`DataPacket`].
  type DataExtra;
  /// Backend-specific extras carried on every [`AttachmentPacket`].
  type AttachmentExtra;
  /// Backend-specific extras carried on every [`TrackInfo`] — the place
  /// a backend keeps what the portable row has no seat for (a native
  /// stream index, disposition bits, the container's metadata bag).
  type TrackExtra;

  /// Text carrier for a track's identity metadata — the attachment
  /// filename and MIME type on [`TrackInfo`], and its language.
  ///
  /// A seat rather than a fixed string type so the core stays
  /// allocator-free: a backend with compile-time names can bind
  /// `&'static str`, one that reads them out of a container binds an
  /// owned inline string.
  ///
  /// `Clone`, as a message carrier is: a copy of `&'static str` or a
  /// refcount bump of a shared string, never a deep copy. It is how the
  /// write side's conversion,
  /// [`TrackDescription::from_track_info`](crate::track::TrackDescription::from_track_info),
  /// takes a row's text from a row it only borrows.
  type Text: AsRef<str> + Clone + Debug;
}

/// The [`VideoPacket`] a [`DemuxAdapter`] delivers over buffer `D`.
pub type DemuxVideoPacket<E, D> =
  VideoPacket<<<E as DemuxAdapter>::Video as VideoAdapter>::PacketExtra, D>;

/// The [`AudioPacket`] a [`DemuxAdapter`] delivers over buffer `D`.
pub type DemuxAudioPacket<E, D> =
  AudioPacket<<<E as DemuxAdapter>::Audio as AudioAdapter>::PacketExtra, D>;

/// The [`SubtitlePacket`] a [`DemuxAdapter`] delivers over buffer `D`.
pub type DemuxSubtitlePacket<E, D> =
  SubtitlePacket<<<E as DemuxAdapter>::Subtitle as SubtitleAdapter>::PacketExtra, D>;

/// The [`DataPacket`] a [`DemuxAdapter`] delivers over buffer `D`.
pub type DemuxDataPacket<E, D> = DataPacket<<E as DemuxAdapter>::DataExtra, D>;

/// The [`AttachmentPacket`] a [`DemuxAdapter`] delivers over buffer `D`.
pub type DemuxAttachmentPacket<E, D> = AttachmentPacket<<E as DemuxAdapter>::AttachmentExtra, D>;

// ---------------------------------------------------------------------------
//  The two packet types the demux tier adds.
//
//  They live here rather than beside their three siblings in
//  `crate::packet` because they exist only as demux products: this
//  crate has no `DataDecoder` and no `AttachmentDecoder`, so nothing
//  ever hands one of these to a decoder. Their shape still follows
//  `packet.rs` exactly — private fields, `const` accessors, `with_*`
//  consuming builders and `set_*` in-place mutators.
// ---------------------------------------------------------------------------

/// A timed opaque-data packet — timecode, KLV, timed ID3.
///
/// Data tracks are real packet streams: they run along the file's
/// timeline and carry a payload per timestamp. They are never
/// reordered, so — like [`SubtitlePacket`] and for the same reason —
/// there is no DTS seat; a data packet's presentation time is its
/// decode time.
///
/// `Clone` and `Debug` derive directly — see [`VideoPacket`]'s docs
/// for why the per-parameter bound this produces is already precise.
#[derive(Clone, Debug)]
pub struct DataPacket<E, D> {
  pts: Option<Timestamp>,
  duration: Option<Timestamp>,
  flags: PacketFlags,
  data: D,
  extra: E,
}

impl<E, D> DataPacket<E, D> {
  /// Constructs a `DataPacket` from `data` and `extra`. Timestamps
  /// default to `None` and flags to empty.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(data: D, extra: E) -> Self {
    Self {
      pts: None,
      duration: None,
      flags: PacketFlags::empty(),
      data,
      extra,
    }
  }

  /// Returns the presentation timestamp.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn pts(&self) -> Option<Timestamp> {
    self.pts
  }
  /// Returns the packet duration.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn duration(&self) -> Option<Timestamp> {
    self.duration
  }
  /// Returns the packet flags.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn flags(&self) -> PacketFlags {
    self.flags
  }
  /// Returns the payload buffer.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn data(&self) -> &D {
    &self.data
  }
  /// Returns the backend-specific extras.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn extra(&self) -> &E {
    &self.extra
  }
  /// Returns a mutable reference to the backend-specific extras.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn extra_mut(&mut self) -> &mut E {
    &mut self.extra
  }
  /// Consumes the packet and returns the buffer.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn into_data(self) -> D {
    self.data
  }
  /// Consumes the packet and returns `(buffer, extras)`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn into_parts(self) -> (D, E) {
    (self.data, self.extra)
  }

  /// Sets the PTS (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_pts(mut self, v: Option<Timestamp>) -> Self {
    self.pts = v;
    self
  }
  /// Sets the duration (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_duration(mut self, v: Option<Timestamp>) -> Self {
    self.duration = v;
    self
  }
  /// Sets the flags (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_flags(mut self, v: PacketFlags) -> Self {
    self.flags = v;
    self
  }

  /// Sets the PTS in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_pts(&mut self, v: Option<Timestamp>) -> &mut Self {
    self.pts = v;
    self
  }
  /// Sets the duration in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_duration(&mut self, v: Option<Timestamp>) -> &mut Self {
    self.duration = v;
    self
  }
  /// Sets the flags in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_flags(&mut self, v: PacketFlags) -> &mut Self {
    self.flags = v;
    self
  }
}

/// An attachment packet — a whole file carried inside the container.
///
/// **No timestamps.** A font or a cover image is not on the timeline;
/// it is present for the whole file or not at all. Its identity — the
/// filename it was attached under, its MIME type — is not repeated here
/// either: that belongs to the track, and lives on [`TrackInfo`].
/// [`PacketFlags`] is kept because `CORRUPT` still means something for
/// a payload that failed to read.
///
/// `Clone` and `Debug` derive directly — see [`VideoPacket`]'s docs
/// for why the per-parameter bound this produces is already precise.
#[derive(Clone, Debug)]
pub struct AttachmentPacket<E, D> {
  flags: PacketFlags,
  data: D,
  extra: E,
}

impl<E, D> AttachmentPacket<E, D> {
  /// Constructs an `AttachmentPacket` from `data` and `extra`. Flags
  /// default to empty.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(data: D, extra: E) -> Self {
    Self {
      flags: PacketFlags::empty(),
      data,
      extra,
    }
  }

  /// Returns the packet flags.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn flags(&self) -> PacketFlags {
    self.flags
  }
  /// Returns the attached file's bytes.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn data(&self) -> &D {
    &self.data
  }
  /// Returns the backend-specific extras.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn extra(&self) -> &E {
    &self.extra
  }
  /// Returns a mutable reference to the backend-specific extras.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn extra_mut(&mut self) -> &mut E {
    &mut self.extra
  }
  /// Consumes the packet and returns the buffer.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn into_data(self) -> D {
    self.data
  }
  /// Consumes the packet and returns `(buffer, extras)`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn into_parts(self) -> (D, E) {
    (self.data, self.extra)
  }

  /// Sets the flags (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_flags(mut self, v: PacketFlags) -> Self {
    self.flags = v;
    self
  }

  /// Sets the flags in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_flags(&mut self, v: PacketFlags) -> &mut Self {
    self.flags = v;
    self
  }
}

// ---------------------------------------------------------------------------
//  The track table.
// ---------------------------------------------------------------------------

/// Payload for [`TrackParams::Video`].
pub struct VideoTrackParams<E: DemuxAdapter> {
  codec: E::CodecId,
  width: u32,
  height: u32,
  pixel_format: <E::Video as VideoAdapter>::PixelFormat,
  frame_rate: Option<Timebase>,
}

impl<E: DemuxAdapter> VideoTrackParams<E> {
  /// Constructs a `VideoTrackParams`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(
    codec: E::CodecId,
    width: u32,
    height: u32,
    pixel_format: <E::Video as VideoAdapter>::PixelFormat,
    frame_rate: Option<Timebase>,
  ) -> Self {
    Self {
      codec,
      width,
      height,
      pixel_format,
      frame_rate,
    }
  }

  /// Returns the codec identifier.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn codec(&self) -> E::CodecId {
    self.codec
  }
  /// Returns the coded width in pixels.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn width(&self) -> u32 {
    self.width
  }
  /// Returns the coded height in pixels.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn height(&self) -> u32 {
    self.height
  }
  /// Returns the pixel format the track declares, in the backend's
  /// vocabulary.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn pixel_format(&self) -> &<E::Video as VideoAdapter>::PixelFormat {
    &self.pixel_format
  }
  /// Returns the average frame rate, as a rate-shaped [`Timebase`]
  /// (`30000/1001` for 29.97 fps), or `None` when the container does
  /// not say.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn frame_rate(&self) -> Option<Timebase> {
    self.frame_rate
  }
}

// `Debug` is hand-written for the same associated-type reason as
// `TrackParams` itself (see its own impl, below): every field here
// that is not a plain `u32` routes through an associated type on
// `E`, and each of those already carries the bound it needs on the
// trait that declares it. `#[derive(Debug)]` would add a flat `E:
// Debug` bound the struct's own fields never ask for.
//
// No `Clone` on this type, `TrackParams`, or `TrackInfo` — see
// `TrackInfo`'s own doc, below, for the message-carrier law that
// keeps it off the whole family.
impl<E: DemuxAdapter> Debug for VideoTrackParams<E> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("VideoTrackParams")
      .field("codec", &self.codec)
      .field("width", &self.width)
      .field("height", &self.height)
      .field("pixel_format", &self.pixel_format)
      .field("frame_rate", &self.frame_rate)
      .finish()
  }
}

/// Payload for [`TrackParams::Audio`].
pub struct AudioTrackParams<E: DemuxAdapter> {
  codec: E::CodecId,
  sample_rate: u32,
  channel_count: u8,
  sample_format: <E::Audio as AudioAdapter>::SampleFormat,
  channel_layout: <E::Audio as AudioAdapter>::ChannelLayout,
}

impl<E: DemuxAdapter> AudioTrackParams<E> {
  /// Constructs an `AudioTrackParams`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(
    codec: E::CodecId,
    sample_rate: u32,
    channel_count: u8,
    sample_format: <E::Audio as AudioAdapter>::SampleFormat,
    channel_layout: <E::Audio as AudioAdapter>::ChannelLayout,
  ) -> Self {
    Self {
      codec,
      sample_rate,
      channel_count,
      sample_format,
      channel_layout,
    }
  }

  /// Returns the codec identifier.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn codec(&self) -> E::CodecId {
    self.codec
  }
  /// Returns the sample rate in Hz.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn sample_rate(&self) -> u32 {
    self.sample_rate
  }
  /// Returns the channel count.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn channel_count(&self) -> u8 {
    self.channel_count
  }
  /// Returns the sample format the track declares, in the backend's
  /// vocabulary.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn sample_format(&self) -> <E::Audio as AudioAdapter>::SampleFormat {
    self.sample_format
  }
  /// Returns the channel layout the track declares, in the backend's
  /// vocabulary.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn channel_layout(&self) -> &<E::Audio as AudioAdapter>::ChannelLayout {
    &self.channel_layout
  }
}

impl<E: DemuxAdapter> Debug for AudioTrackParams<E> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("AudioTrackParams")
      .field("codec", &self.codec)
      .field("sample_rate", &self.sample_rate)
      .field("channel_count", &self.channel_count)
      .field("sample_format", &self.sample_format)
      .field("channel_layout", &self.channel_layout)
      .finish()
  }
}

/// Payload for [`TrackParams::Subtitle`].
pub struct SubtitleTrackParams<E: DemuxAdapter> {
  codec: E::CodecId,
}

impl<E: DemuxAdapter> SubtitleTrackParams<E> {
  /// Constructs a `SubtitleTrackParams`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(codec: E::CodecId) -> Self {
    Self { codec }
  }

  /// Returns the codec identifier.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn codec(&self) -> E::CodecId {
    self.codec
  }
}

impl<E: DemuxAdapter> Debug for SubtitleTrackParams<E> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("SubtitleTrackParams")
      .field("codec", &self.codec)
      .finish()
  }
}

/// Payload for [`TrackParams::Data`].
pub struct DataTrackParams<E: DemuxAdapter> {
  codec: E::CodecId,
}

impl<E: DemuxAdapter> DataTrackParams<E> {
  /// Constructs a `DataTrackParams`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(codec: E::CodecId) -> Self {
    Self { codec }
  }

  /// Returns the codec identifier.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn codec(&self) -> E::CodecId {
    self.codec
  }
}

impl<E: DemuxAdapter> Debug for DataTrackParams<E> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("DataTrackParams")
      .field("codec", &self.codec)
      .finish()
  }
}

/// Payload for [`TrackParams::Attachment`].
pub struct AttachmentTrackParams<E: DemuxAdapter> {
  /// Codec identifier — the font format, or the still image's codec
  /// for cover art.
  codec: E::CodecId,
}

impl<E: DemuxAdapter> AttachmentTrackParams<E> {
  /// Constructs an `AttachmentTrackParams`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(codec: E::CodecId) -> Self {
    Self { codec }
  }

  /// Returns the codec identifier — the font format, or the still
  /// image's codec for cover art.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn codec(&self) -> E::CodecId {
    self.codec
  }
}

impl<E: DemuxAdapter> Debug for AttachmentTrackParams<E> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("AttachmentTrackParams")
      .field("codec", &self.codec)
      .finish()
  }
}

/// Payload for [`TrackParams::Unknown`].
pub struct UnknownTrackParams<E: DemuxAdapter> {
  /// Codec identifier, which may itself be the backend's "none".
  codec: E::CodecId,
}

impl<E: DemuxAdapter> UnknownTrackParams<E> {
  /// Constructs an `UnknownTrackParams`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(codec: E::CodecId) -> Self {
    Self { codec }
  }

  /// Returns the codec identifier, which may itself be the backend's
  /// "none".
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn codec(&self) -> E::CodecId {
    self.codec
  }
}

impl<E: DemuxAdapter> Debug for UnknownTrackParams<E> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("UnknownTrackParams")
      .field("codec", &self.codec)
      .finish()
  }
}

/// A track's codec and per-kind parameters.
///
/// The arm **is** the kind: [`TrackInfo::kind`] reads it off this enum
/// rather than storing a second copy that could disagree with the
/// payload beside it.
///
/// No `Clone` — see [`TrackInfo`]'s own doc for the message-carrier
/// law that keeps it off this type too.
#[derive(IsVariant, Unwrap, TryUnwrap)]
#[unwrap(ref, ref_mut)]
#[try_unwrap(ref, ref_mut)]
pub enum TrackParams<E: DemuxAdapter> {
  /// Motion video.
  Video(VideoTrackParams<E>),
  /// Audio.
  Audio(AudioTrackParams<E>),
  /// Subtitles / captions.
  Subtitle(SubtitleTrackParams<E>),
  /// Timed opaque data.
  Data(DataTrackParams<E>),
  /// An attached file — a font, cover art.
  Attachment(AttachmentTrackParams<E>),
  /// A track the backend could not classify.
  Unknown(UnknownTrackParams<E>),
}

impl<E: DemuxAdapter> TrackParams<E> {
  /// Returns the kind this arm describes.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn kind(&self) -> TrackKind {
    match self {
      Self::Video(_) => TrackKind::Video,
      Self::Audio(_) => TrackKind::Audio,
      Self::Subtitle(_) => TrackKind::Subtitle,
      Self::Data(_) => TrackKind::Data,
      Self::Attachment(_) => TrackKind::Attachment,
      Self::Unknown(_) => TrackKind::Unknown,
    }
  }

  /// Returns the codec identifier, whichever arm this is.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn codec(&self) -> E::CodecId {
    match self {
      Self::Video(p) => p.codec(),
      Self::Audio(p) => p.codec(),
      Self::Subtitle(p) => p.codec(),
      Self::Data(p) => p.codec(),
      Self::Attachment(p) => p.codec(),
      Self::Unknown(p) => p.codec(),
    }
  }

  /// A field-by-field copy of these parameters — **named, and not
  /// `Clone`**.
  ///
  /// The message-carrier law keeps `Clone` off this type: `Clone` is a
  /// refcount bump, and a deep copy of parameters is not one. A consumer
  /// that does need parameters of its own — the write side's
  /// [`TrackDescription::from_track_info`](crate::track::TrackDescription::from_track_info),
  /// which describes a track from a row it only borrows — asks for the
  /// copy by name, so the cost is written where it is paid.
  ///
  /// Every field is copied explicitly, by destructuring, so a field added
  /// to a payload is a compile error here rather than a field silently
  /// left behind: the codec, sample format and frame rate by `Copy`, and
  /// the backend's pixel format and channel layout through their own
  /// `Clone`.
  #[must_use]
  pub fn duplicate(&self) -> Self {
    match self {
      Self::Video(VideoTrackParams {
        codec,
        width,
        height,
        pixel_format,
        frame_rate,
      }) => Self::Video(VideoTrackParams {
        codec: *codec,
        width: *width,
        height: *height,
        pixel_format: pixel_format.clone(),
        frame_rate: *frame_rate,
      }),
      Self::Audio(AudioTrackParams {
        codec,
        sample_rate,
        channel_count,
        sample_format,
        channel_layout,
      }) => Self::Audio(AudioTrackParams {
        codec: *codec,
        sample_rate: *sample_rate,
        channel_count: *channel_count,
        sample_format: *sample_format,
        channel_layout: channel_layout.clone(),
      }),
      Self::Subtitle(SubtitleTrackParams { codec }) => {
        Self::Subtitle(SubtitleTrackParams { codec: *codec })
      }
      Self::Data(DataTrackParams { codec }) => Self::Data(DataTrackParams { codec: *codec }),
      Self::Attachment(AttachmentTrackParams { codec }) => {
        Self::Attachment(AttachmentTrackParams { codec: *codec })
      }
      Self::Unknown(UnknownTrackParams { codec }) => {
        Self::Unknown(UnknownTrackParams { codec: *codec })
      }
    }
  }
}

// `Debug` is hand-written, not derived: a flat `#[derive(Debug)]`
// over the enum's own type parameter `E` would demand `E: Debug`,
// which none of the six payload structs' fields need — each already
// carries the precise bound it requires from the trait that declares
// its associated type. See each payload struct's own `Debug` impl,
// above, for the same reasoning one level down.
//
// No `Clone`: see `TrackInfo`'s own doc, below, for the
// message-carrier law.
impl<E: DemuxAdapter> Debug for TrackParams<E> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Video(p) => f.debug_tuple("Video").field(p).finish(),
      Self::Audio(p) => f.debug_tuple("Audio").field(p).finish(),
      Self::Subtitle(p) => f.debug_tuple("Subtitle").field(p).finish(),
      Self::Data(p) => f.debug_tuple("Data").field(p).finish(),
      Self::Attachment(p) => f.debug_tuple("Attachment").field(p).finish(),
      Self::Unknown(p) => f.debug_tuple("Unknown").field(p).finish(),
    }
  }
}

/// One row of the track table `Demuxer::tracks` returns.
///
/// Carries what a consumer needs to decide whether it wants the track
/// and how to open a decoder for it: the kind, the timebase every
/// timestamp on that track is expressed in, the duration the library
/// reported for it before probing, the per-kind codec parameters, the
/// language the container declares for the track, and — for
/// attachments — the identity the file was attached under.
///
/// Everything a particular backend knows and this row has no seat for
/// rides [`DemuxAdapter::TrackExtra`].
///
/// **No `Clone`, on this type or on [`TrackParams`].** The
/// message-carrier law: messages may be `Clone`, but `Clone` is
/// always a refcount bump, never a deep copy — and a track row,
/// backend metadata down to codec parameters, is not cheap to
/// duplicate. A consumer that needs to share a row shares a *handle*
/// on it instead: `Demuxer::tracks` hands out
/// `Demuxer::TrackHandle`s over rows the session keeps for its
/// whole life, so a row is built once and every consumer after that
/// shares it by refcount.
///
/// The absence is load-bearing, not incidental. It is what leaves
/// `TrackHandle`'s `Clone` bound with no cheap deep-copying carrier
/// to admit: there is no `#[derive(Clone)]` road over a row that has
/// none, and `Box<TrackInfo<_>>` is not `Clone` either. A shared
/// handle or a borrow is what an implementor reaches for; a deep copy
/// would have to be hand-written, field by field, against the law
/// this paragraph states.
pub struct TrackInfo<E: DemuxAdapter> {
  timebase: Timebase,
  duration: Option<Timestamp>,
  params: TrackParams<E>,
  filename: Option<E::Text>,
  mime_type: Option<E::Text>,
  language: Option<E::Text>,
  disposition: TrackDisposition,
  extra: E::TrackExtra,
}

impl<E: DemuxAdapter> TrackInfo<E> {
  /// Constructs a `TrackInfo`. Identity metadata defaults to `None`,
  /// and the disposition to the empty set.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(timebase: Timebase, params: TrackParams<E>, extra: E::TrackExtra) -> Self {
    Self {
      timebase,
      duration: None,
      params,
      filename: None,
      mime_type: None,
      language: None,
      disposition: TrackDisposition::empty(),
      extra,
    }
  }

  /// Returns the track's kind, read off [`Self::params`].
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn kind(&self) -> TrackKind {
    self.params.kind()
  }
  /// Returns the timebase every timestamp on this track is expressed in.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn timebase(&self) -> Timebase {
    self.timebase
  }
  /// Returns the duration the demuxing library reported for this track
  /// when it opened the container, before it probed any packet, or
  /// `None` where it reported none.
  ///
  /// That is the library's figure and no claim about the file: a
  /// format's reader may compute it itself — libavformat's WAV reader
  /// derives it from the data chunk's size — and this tier cannot tell
  /// that from a duration the file declares. What it does keep out is
  /// the fill probing adds. libavformat gives a stream that supplied no
  /// timing the container's own length once probing ends, so a figure
  /// that appears only afterwards is not carried, and a stream the
  /// library creates during probing has none here. A track stays `None`
  /// where the library reported nothing before probing — Matroska
  /// reports a length for the file and none for any track — and the
  /// container's own figure (`Demuxer::duration`) is never copied
  /// onto it.
  ///
  /// What the packets themselves show is `Demuxer::measured_end`: an
  /// endpoint on the track's timeline rather than a length, exact when
  /// it says so.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn duration(&self) -> Option<Timestamp> {
    self.duration
  }
  /// Returns the per-kind codec parameters.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn params(&self) -> &TrackParams<E> {
    &self.params
  }
  /// Returns the filename an attachment was attached under, when the
  /// container carries one. `None` for every other kind.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn filename(&self) -> Option<&E::Text> {
    self.filename.as_ref()
  }
  /// Returns an attachment's declared MIME type, when the container
  /// carries one. `None` for every other kind.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn mime_type(&self) -> Option<&E::Text> {
    self.mime_type.as_ref()
  }
  /// Returns the language the **container declares** for this track,
  /// exactly as it is written there — `None` where the file declares
  /// none.
  ///
  /// # The file's word, unfolded
  ///
  /// This is a reading, never a reckoning. `None` means the container
  /// said nothing, and a `Some` is the tag as written: an MKV's ISO
  /// 639-2/B `ger`, an MP4's 639-2/T `deu`, a decades-old muxer's `iw`,
  /// a BCP 47 `zh-Hans`, or `und` where a file declares its language
  /// unknown. Each of those is a different string for something a
  /// vocabulary may well call one language, and none of them is
  /// normalised here.
  ///
  /// **Deliberately, and the alternative was measured.** Folding those
  /// spellings together takes registry tables — the IANA subtag
  /// registry for BCP 47 and ISO 639-2's own for the alpha-3 space BCP
  /// 47 does not register — and a crate that owns one is where that
  /// fold belongs. A demux tier that folded early would be a *second*
  /// authority on the same question, disagreeing with the first in
  /// exactly the cases the registries exist for; and a narrower seat —
  /// a three-letter code, say — would have had nowhere to put
  /// `zh-Hans`, which is a tag Matroska really writes. So the seat
  /// carries the declaration whole and leaves the fold downstream,
  /// where one door can apply it once.
  ///
  /// # Every kind, not only subtitles
  ///
  /// Containers tag audio tracks as readily as subtitle ones — a dub
  /// and its captions are the same question asked twice — so the seat
  /// is on the row rather than on one arm of
  /// [`TrackParams`].
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn language(&self) -> Option<&E::Text> {
    self.language.as_ref()
  }
  /// Returns the track's disposition, as the **container declares**
  /// it — the empty set where it declares none.
  ///
  /// Like [`language`](Self::language), a reading rather than a
  /// reckoning: the bits the file wrote, carried whole (an unnamed bit
  /// included), and never inferred — a container that marks no track
  /// the default leaves every row without
  /// [`TrackDisposition::DEFAULT`], whatever a player would pick.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn disposition(&self) -> TrackDisposition {
    self.disposition
  }
  /// Returns the backend-specific extras.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn extra(&self) -> &E::TrackExtra {
    &self.extra
  }
  /// Returns a mutable reference to the backend-specific extras.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn extra_mut(&mut self) -> &mut E::TrackExtra {
    &mut self.extra
  }

  /// Sets the duration (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_duration(mut self, v: Option<Timestamp>) -> Self {
    self.duration = v;
    self
  }
  /// Sets the attachment filename (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub fn with_filename(mut self, v: Option<E::Text>) -> Self {
    self.filename = v;
    self
  }
  /// Sets the attachment MIME type (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub fn with_mime_type(mut self, v: Option<E::Text>) -> Self {
    self.mime_type = v;
    self
  }
  /// Sets the container's declared language (consuming builder).
  ///
  /// Pass the tag as the container writes it — see
  /// [`language`](Self::language) for why nothing normalises it.
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub fn with_language(mut self, v: Option<E::Text>) -> Self {
    self.language = v;
    self
  }
  /// Sets the container's declared disposition (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_disposition(mut self, v: TrackDisposition) -> Self {
    self.disposition = v;
    self
  }

  /// Sets the duration in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_duration(&mut self, v: Option<Timestamp>) -> &mut Self {
    self.duration = v;
    self
  }
  /// Sets the attachment filename in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn set_filename(&mut self, v: Option<E::Text>) -> &mut Self {
    self.filename = v;
    self
  }
  /// Sets the attachment MIME type in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn set_mime_type(&mut self, v: Option<E::Text>) -> &mut Self {
    self.mime_type = v;
    self
  }
  /// Sets the container's declared language in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn set_language(&mut self, v: Option<E::Text>) -> &mut Self {
    self.language = v;
    self
  }
  /// Sets the container's declared disposition in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_disposition(&mut self, v: TrackDisposition) -> &mut Self {
    self.disposition = v;
    self
  }
}

// `Debug` is hand-written for the same reason as `TrackParams`'s:
// `#[derive(Debug)]` would add `E: Debug`, but the field that
// actually needs a bound is `E::TrackExtra` — `E::Text: Debug` is
// already guaranteed by `DemuxAdapter::Text`'s own trait bound, and
// `TrackParams<E>` needs no extra bound at all (see its own impl,
// above). No `Clone`: see this type's own doc, above, for the
// message-carrier law.
impl<E: DemuxAdapter> Debug for TrackInfo<E>
where
  E::TrackExtra: Debug,
{
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("TrackInfo")
      .field("timebase", &self.timebase)
      .field("duration", &self.duration)
      .field("params", &self.params)
      .field("filename", &self.filename)
      .field("mime_type", &self.mime_type)
      .field("language", &self.language)
      .field("disposition", &self.disposition)
      .field("extra", &self.extra)
      .finish()
  }
}

// ---------------------------------------------------------------------------
//  The chapter table.
// ---------------------------------------------------------------------------

/// One row of the chapter table `Demuxer::chapters` returns.
///
/// A chapter is a **named stretch of the file's timeline** the
/// container itself declares: a DVD-style scene marker, a podcast
/// segment, a Matroska `ChapterAtom`. It names no track — the span
/// covers the file, and a consumer that wants the packets inside one
/// `seek`s there.
///
/// The shape mirrors libavformat's `AVChapter`, which is the shape
/// every container that carries chapters is read into: an id the
/// *container* assigned, a timebase, a start and an end expressed in
/// it, and a title.
///
/// # The id is the container's, not the row's position
///
/// [`id`](Self::id) is what the file wrote and nothing else — a
/// Matroska `ChapterUID`, an MP4 chapter track's entry. It is not a
/// table coordinate, it is not promised to be dense, and two
/// containers holding the very same chapters routinely number them
/// differently: an MKV written from an FFMETADATA file numbers from 1,
/// because Matroska forbids the UID zero, while the MP4 written from
/// that same file numbers from 0. Where a table coordinate is what is
/// wanted, the position in `Demuxer::chapters` is it — the way
/// [`TrackIndex`] is the track table's.
///
/// # The span is reported, not repaired
///
/// [`start`](Self::start) and [`end`](Self::end) are the container's
/// own two numbers. A file that writes an end before its start, or
/// leaves the end at whatever its backend spells "no timestamp",
/// reaches a consumer that way. This tier reads containers: deciding
/// what an inverted span *means* takes a policy it does not have, and
/// a clamp applied here would be indistinguishable downstream from a
/// file that was well-formed all along.
///
/// That is also why the two instants are separate seats rather than
/// one [`TimeRange`](crate::TimeRange): that type's constructor
/// refuses `end < start`, which is exactly the row this one has to be
/// able to carry. A consumer that wants a range — and has a policy for
/// the inverted case — builds one with
/// [`TimeRange::try_new`](crate::TimeRange::try_new).
///
/// # `Clone`, unlike [`TrackInfo`]
///
/// The message-carrier law that keeps `Clone` off a track row is about
/// cost: a row carries backend metadata down to codec parameters. A
/// chapter is four scalars and the backend's own text carrier, so
/// copying one costs about what copying a [`Timestamp`] does — there
/// is no deep copy here for the law to forbid, and no shared-handle
/// seat is worth minting to avoid one.
pub struct Chapter<E: DemuxAdapter> {
  id: i64,
  timebase: Timebase,
  start: Timestamp,
  end: Timestamp,
  title: Option<E::Text>,
}

impl<E: DemuxAdapter> Chapter<E> {
  /// Constructs a `Chapter`. The title defaults to `None`.
  ///
  /// `start` and `end` are expected in `timebase` — the chapter's own,
  /// which is not required to be any track's. Nothing checks that
  /// `end` follows `start`: see [the span is reported, not
  /// repaired](Self#the-span-is-reported-not-repaired).
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(id: i64, timebase: Timebase, start: Timestamp, end: Timestamp) -> Self {
    Self {
      id,
      timebase,
      start,
      end,
      title: None,
    }
  }

  /// Returns the id the **container** assigned this chapter.
  ///
  /// Not a table position, and not promised to be dense — see [the id
  /// is the
  /// container's](Self#the-id-is-the-containers-not-the-rows-position).
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn id(&self) -> i64 {
    self.id
  }
  /// Returns the timebase this chapter's start and end are expressed
  /// in.
  ///
  /// The chapter's own, as the container declares it, and independent
  /// of every track's: Matroska writes chapters in nanoseconds
  /// whatever its tracks use.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn timebase(&self) -> Timebase {
    self.timebase
  }
  /// Returns the instant the chapter starts at.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn start(&self) -> Timestamp {
    self.start
  }
  /// Returns the instant the chapter ends at, exactly as the container
  /// wrote it — which is not promised to follow
  /// [`start`](Self::start).
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn end(&self) -> Timestamp {
    self.end
  }
  /// Returns the chapter's title, when the container carries one.
  ///
  /// The container's `title` metadata entry, read exactly as
  /// [`TrackInfo::language`] is: whatever the file wrote, unfolded and
  /// untranslated, and `None` where it wrote nothing.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn title(&self) -> Option<&E::Text> {
    self.title.as_ref()
  }

  /// Sets the title (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub fn with_title(mut self, v: Option<E::Text>) -> Self {
    self.title = v;
    self
  }

  /// Sets the title in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn set_title(&mut self, v: Option<E::Text>) -> &mut Self {
    self.title = v;
    self
  }
}

// `Clone`, `Debug` and `PartialEq` are hand-written for the same
// associated-type reason as `TrackInfo`'s: `#[derive]` would add a
// flat `E: Clone` / `E: Debug` / `E: PartialEq` bound, while the only
// field that needs one is `E::Text`. `Debug` needs none at all —
// `DemuxAdapter::Text` already carries it.
impl<E: DemuxAdapter> Clone for Chapter<E>
where
  E::Text: Clone,
{
  fn clone(&self) -> Self {
    Self {
      id: self.id,
      timebase: self.timebase,
      start: self.start,
      end: self.end,
      title: self.title.clone(),
    }
  }
}

impl<E: DemuxAdapter> Debug for Chapter<E> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("Chapter")
      .field("id", &self.id)
      .field("timebase", &self.timebase)
      .field("start", &self.start)
      .field("end", &self.end)
      .field("title", &self.title)
      .finish()
  }
}

// Seat by seat, each under its own rule. The two instants compare as
// `Timestamp` compares — by *when* they are, so `1_000` over `1/1000`
// equals `1_000_000` over `1/1000000` — while the row's own
// `timebase` compares as the rational it is. Two rows covering the
// same stretch of time under different declared rulers are therefore
// **not** equal, which is the right answer for a row whose whole job
// is to report what the container declared: the ruler is part of the
// declaration.
impl<E: DemuxAdapter> PartialEq for Chapter<E>
where
  E::Text: PartialEq,
{
  fn eq(&self, other: &Self) -> bool {
    self.id == other.id
      && self.timebase == other.timebase
      && self.start == other.start
      && self.end == other.end
      && self.title == other.title
  }
}

// ---------------------------------------------------------------------------
//  The container's duration.
// ---------------------------------------------------------------------------

/// How the demuxing library came by a container-level duration, in the
/// library's own three words and no others.
///
/// The arms are libavformat's `AVDurationEstimationMethod`, verbatim.
/// Each names what libavformat *did* to produce the figure; none says
/// that a header declares it, and none should be read as if it did.
/// Which to trust, and how far, is the consumer's call — this type
/// reports and does not rank.
///
/// The figure these arms account for is a **length**, and a
/// [`MeasuredEnd`] is an endpoint on a track's timeline: different
/// quantities, which this type does not set against each other.
///
/// Carried by [`ContainerDuration`], so the account travels with the
/// figure.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, IsVariant)]
pub enum DurationSource {
  /// `AVFMT_DURATION_FROM_PTS`. libavformat's words: "Duration
  /// accurately estimated from PTSes".
  ///
  /// libavformat estimated the duration from packet timestamps — for an
  /// MPEG program or transport stream, the ones at the tail of the
  /// file. An estimate made from timestamps, and one libavformat itself
  /// calls accurate. This arm's value is zero, which is also what the
  /// field holds until libavformat sets it, so the label alone does not
  /// prove that packets were read.
  FromPts,
  /// `AVFMT_DURATION_FROM_STREAM`. libavformat's words: "Duration
  /// estimated from a stream with a known duration".
  ///
  /// libavformat took the figure from a stream's duration or from the
  /// container's own duration field, and the label does not say which:
  /// it is reported for Matroska too, where the figure is the
  /// container's own field and no stream has one. It is not a
  /// declaration by the file — a format's reader may compute a stream's
  /// duration from other fields rather than read it, and this label
  /// covers that as well.
  FromStream,
  /// `AVFMT_DURATION_FROM_BITRATE`. libavformat's words: "Duration
  /// estimated from bitrate (less accurate)".
  ///
  /// libavformat divided the file's size by a bitrate, and says itself
  /// that this is the less accurate way: it is exact only for a
  /// constant bitrate over a file that is all media.
  FromBitrate,
  /// An account this build cannot name: libavformat recorded a method
  /// it has no word for here, or the backend has a figure and no
  /// account of how it was reached. Nothing is claimed about the
  /// figure, and it is not folded into an estimate.
  Unknown,
}

/// A container-level duration with the library's account of it.
///
/// What `Demuxer::duration` answers: [`value`](Self::value) is the
/// figure, as a [`Timestamp`] the way [`TrackInfo::duration`] carries
/// one, and [`source`](Self::source) is how the library says it came by
/// it. Read the two together — the same number means one thing read off
/// timestamps and another divided out of a bitrate.
///
/// The figure is a **length**: how long the library says the file runs.
/// It is not an endpoint on any track's timeline, and it is not
/// interchangeable with a [`MeasuredEnd`].
///
/// It is the *container's* figure and no track's: a track's own comes
/// through [`TrackInfo::duration`], or is absent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContainerDuration {
  value: Timestamp,
  source: DurationSource,
}

impl ContainerDuration {
  /// Constructs a `ContainerDuration` from a figure and the library's
  /// account of it.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(value: Timestamp, source: DurationSource) -> Self {
    Self { value, source }
  }

  /// Returns the figure.
  ///
  /// How far to trust it is [`source`](Self::source)'s to say, in the
  /// library's own words.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn value(&self) -> Timestamp {
    self.value
  }

  /// Returns the library's account of how it came by the figure.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn source(&self) -> DurationSource {
    self.source
  }
}

// ---------------------------------------------------------------------------
//  The measured end.
// ---------------------------------------------------------------------------

/// Where one track's packets ended, as the walk the caller performed
/// measured it — with two qualifications that are independent of each
/// other.
///
/// What `Demuxer::measured_end` answers. [`end`](Self::end) is the
/// greatest packet end — a packet's timestamp plus its duration — over
/// the timed packets the session read on the track: an **endpoint on
/// that track's own timeline**, in its own timebase. It is qualified by
/// [`walk_complete`](Self::walk_complete), whether the walk that
/// measured it covered the file, and by [`exact`](Self::exact), whether
/// every packet's end was actually known.
///
/// That is the figure's whole promise: **exact when `exact` holds,
/// otherwise a lower bound, and covering what `walk_complete` says.**
///
/// # An endpoint, not a length
///
/// The container's duration (`Demuxer::duration`) is a *length*. The
/// two are different quantities and are not interchangeable: a track
/// that starts at ten seconds and runs for two ends at twelve, against
/// a container duration of two, and a presentation edit can shift
/// either again. Turning an end into a length means subtracting the
/// track's start and honouring any presentation edits. That is the
/// composer's job; this crate does not do it, and the figure here
/// neither performs it nor implies it.
///
/// A timed packet with no payload counts toward the end: it is a real
/// endpoint, and a later one is the end.
///
/// # Exact, or a lower bound
///
/// `exact` turns `false`, for good, the first time a packet on the
/// track lacks what an end needs: a duration or a timestamp. A timed
/// packet read with a duration that is not positive — libavformat
/// writes zero for one it does not know — ends, as far as the walk can
/// tell, where it starts; a packet read with no usable timestamp has no
/// end to place. Either leaves `end` a lower bound. The mark is made
/// when the packet is *read*, so it does not depend on what becomes of
/// the packet afterwards (delivered, refused, parked) or on whether it
/// carries a payload. A duration the library derived itself from the
/// stream's frame rate or frame size counts as a duration, and is only
/// as exact as that derivation.
///
/// # Complete, or so far
///
/// `walk_complete` is `true` only when every packet the library
/// delivered to this session was observed, from the first one to end of
/// file, with no skip. A walk that is complete is **frozen**: its
/// figures are final, and neither a later read nor a later
/// `seek` moves them or takes the flag back.
///
/// A walk that skips something before it completes — a seek, a packet
/// read and not observed (refused, skipped as corrupt, or on a stream
/// the session never described, which is what a container that adds
/// streams mid-read produces), or an end too large to represent — can
/// never complete in this session. Measurement carries on as a "so far"
/// maximum: `end` keeps rising as packets are read, and stays a lower
/// bound however far the walk goes.
///
/// **What it does not cover.** A library probes while it opens the
/// container, before this session's first read. libavformat buffers the
/// packets it reads then and replays them, but a read error it swallows
/// during probing is not exposed anywhere, so data skipped there is
/// invisible to the session, and a complete walk does not vouch for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MeasuredEnd {
  end: Timestamp,
  walk_complete: bool,
  exact: bool,
}

impl MeasuredEnd {
  /// Constructs a `MeasuredEnd` from the greatest packet end seen,
  /// whether the walk that saw it was complete, and whether every
  /// packet's end was known.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(end: Timestamp, walk_complete: bool, exact: bool) -> Self {
    Self {
      end,
      walk_complete,
      exact,
    }
  }

  /// Returns the greatest packet end the session has read on the track:
  /// an endpoint on the track's own timeline, not a length.
  ///
  /// Exact only when [`exact`](Self::exact) holds, a lower bound
  /// otherwise, and final only when [`walk_complete`](Self::walk_complete)
  /// holds, a figure so far before.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn end(&self) -> Timestamp {
    self.end
  }

  /// Returns `true` when every packet the library delivered to this
  /// session was observed, from the first to end of file, with no skip.
  ///
  /// `false` means the figure is *so far*: the walk has not finished,
  /// or a seek, a packet read and not observed, or an unrepresentable
  /// end means it did not cover the file, and it never will in this
  /// session. `true` freezes the figure and does not reach back into
  /// the library's own probing, which is not observable — see [the
  /// type's docs](MeasuredEnd#complete-or-so-far).
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn walk_complete(&self) -> bool {
    self.walk_complete
  }

  /// Returns `true` when every packet's end on the track was known, so
  /// [`end`](Self::end) is the greatest of known ends.
  ///
  /// `false` means [`end`](Self::end) is a lower bound: a packet was
  /// read with no positive duration or no timestamp, from which point
  /// on the walk cannot say where the track really ends. See [the type's
  /// docs](MeasuredEnd#exact-or-a-lower-bound).
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn exact(&self) -> bool {
    self.exact
  }
}

// ---------------------------------------------------------------------------
//  The delivery envelope.
// ---------------------------------------------------------------------------

/// Payload for [`DemuxedPacket::Video`].
pub struct VideoTrackPacket<E: DemuxAdapter, D> {
  track: TrackIndex,
  packet: DemuxVideoPacket<E, D>,
}

impl<E: DemuxAdapter, D> VideoTrackPacket<E, D> {
  /// Constructs a `VideoTrackPacket`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(track: TrackIndex, packet: DemuxVideoPacket<E, D>) -> Self {
    Self { track, packet }
  }

  /// Returns the track this packet belongs to.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn track(&self) -> TrackIndex {
    self.track
  }
  /// Returns the packet.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn packet(&self) -> &DemuxVideoPacket<E, D> {
    &self.packet
  }
  /// Consumes the envelope and returns the packet.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn into_packet(self) -> DemuxVideoPacket<E, D> {
    self.packet
  }
  /// Consumes the envelope and returns `(track, packet)`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn into_parts(self) -> (TrackIndex, DemuxVideoPacket<E, D>) {
    (self.track, self.packet)
  }
}

// `Clone` / `Debug` are hand-written for the same associated-type
// reason as `TrackParams`'s payload structs, above: the bound belongs
// on `<E::Video as VideoAdapter>::PacketExtra`, not on `E` itself,
// which `#[derive]` cannot see.
impl<E, D> Clone for VideoTrackPacket<E, D>
where
  E: DemuxAdapter,
  D: Clone,
  <E::Video as VideoAdapter>::PacketExtra: Clone,
{
  fn clone(&self) -> Self {
    Self {
      track: self.track,
      packet: self.packet.clone(),
    }
  }
}

impl<E, D> Debug for VideoTrackPacket<E, D>
where
  E: DemuxAdapter,
  D: Debug,
  <E::Video as VideoAdapter>::PacketExtra: Debug,
{
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("VideoTrackPacket")
      .field("track", &self.track)
      .field("packet", &self.packet)
      .finish()
  }
}

/// Payload for [`DemuxedPacket::Audio`].
pub struct AudioTrackPacket<E: DemuxAdapter, D> {
  track: TrackIndex,
  packet: DemuxAudioPacket<E, D>,
}

impl<E: DemuxAdapter, D> AudioTrackPacket<E, D> {
  /// Constructs an `AudioTrackPacket`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(track: TrackIndex, packet: DemuxAudioPacket<E, D>) -> Self {
    Self { track, packet }
  }

  /// Returns the track this packet belongs to.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn track(&self) -> TrackIndex {
    self.track
  }
  /// Returns the packet.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn packet(&self) -> &DemuxAudioPacket<E, D> {
    &self.packet
  }
  /// Consumes the envelope and returns the packet.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn into_packet(self) -> DemuxAudioPacket<E, D> {
    self.packet
  }
  /// Consumes the envelope and returns `(track, packet)`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn into_parts(self) -> (TrackIndex, DemuxAudioPacket<E, D>) {
    (self.track, self.packet)
  }
}

impl<E, D> Clone for AudioTrackPacket<E, D>
where
  E: DemuxAdapter,
  D: Clone,
  <E::Audio as AudioAdapter>::PacketExtra: Clone,
{
  fn clone(&self) -> Self {
    Self {
      track: self.track,
      packet: self.packet.clone(),
    }
  }
}

impl<E, D> Debug for AudioTrackPacket<E, D>
where
  E: DemuxAdapter,
  D: Debug,
  <E::Audio as AudioAdapter>::PacketExtra: Debug,
{
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("AudioTrackPacket")
      .field("track", &self.track)
      .field("packet", &self.packet)
      .finish()
  }
}

/// Payload for [`DemuxedPacket::Subtitle`].
pub struct SubtitleTrackPacket<E: DemuxAdapter, D> {
  track: TrackIndex,
  packet: DemuxSubtitlePacket<E, D>,
}

impl<E: DemuxAdapter, D> SubtitleTrackPacket<E, D> {
  /// Constructs a `SubtitleTrackPacket`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(track: TrackIndex, packet: DemuxSubtitlePacket<E, D>) -> Self {
    Self { track, packet }
  }

  /// Returns the track this packet belongs to.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn track(&self) -> TrackIndex {
    self.track
  }
  /// Returns the packet.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn packet(&self) -> &DemuxSubtitlePacket<E, D> {
    &self.packet
  }
  /// Consumes the envelope and returns the packet.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn into_packet(self) -> DemuxSubtitlePacket<E, D> {
    self.packet
  }
  /// Consumes the envelope and returns `(track, packet)`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn into_parts(self) -> (TrackIndex, DemuxSubtitlePacket<E, D>) {
    (self.track, self.packet)
  }
}

impl<E, D> Clone for SubtitleTrackPacket<E, D>
where
  E: DemuxAdapter,
  D: Clone,
  <E::Subtitle as SubtitleAdapter>::PacketExtra: Clone,
{
  fn clone(&self) -> Self {
    Self {
      track: self.track,
      packet: self.packet.clone(),
    }
  }
}

impl<E, D> Debug for SubtitleTrackPacket<E, D>
where
  E: DemuxAdapter,
  D: Debug,
  <E::Subtitle as SubtitleAdapter>::PacketExtra: Debug,
{
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("SubtitleTrackPacket")
      .field("track", &self.track)
      .field("packet", &self.packet)
      .finish()
  }
}

/// Payload for [`DemuxedPacket::Data`].
pub struct DataTrackPacket<E: DemuxAdapter, D> {
  track: TrackIndex,
  packet: DemuxDataPacket<E, D>,
}

impl<E: DemuxAdapter, D> DataTrackPacket<E, D> {
  /// Constructs a `DataTrackPacket`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(track: TrackIndex, packet: DemuxDataPacket<E, D>) -> Self {
    Self { track, packet }
  }

  /// Returns the track this packet belongs to.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn track(&self) -> TrackIndex {
    self.track
  }
  /// Returns the packet.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn packet(&self) -> &DemuxDataPacket<E, D> {
    &self.packet
  }
  /// Consumes the envelope and returns the packet.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn into_packet(self) -> DemuxDataPacket<E, D> {
    self.packet
  }
  /// Consumes the envelope and returns `(track, packet)`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn into_parts(self) -> (TrackIndex, DemuxDataPacket<E, D>) {
    (self.track, self.packet)
  }
}

impl<E, D> Clone for DataTrackPacket<E, D>
where
  E: DemuxAdapter,
  D: Clone,
  E::DataExtra: Clone,
{
  fn clone(&self) -> Self {
    Self {
      track: self.track,
      packet: self.packet.clone(),
    }
  }
}

impl<E, D> Debug for DataTrackPacket<E, D>
where
  E: DemuxAdapter,
  D: Debug,
  E::DataExtra: Debug,
{
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("DataTrackPacket")
      .field("track", &self.track)
      .field("packet", &self.packet)
      .finish()
  }
}

/// Payload for [`DemuxedPacket::Attachment`].
pub struct AttachmentTrackPacket<E: DemuxAdapter, D> {
  track: TrackIndex,
  packet: DemuxAttachmentPacket<E, D>,
}

impl<E: DemuxAdapter, D> AttachmentTrackPacket<E, D> {
  /// Constructs an `AttachmentTrackPacket`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(track: TrackIndex, packet: DemuxAttachmentPacket<E, D>) -> Self {
    Self { track, packet }
  }

  /// Returns the track this packet belongs to.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn track(&self) -> TrackIndex {
    self.track
  }
  /// Returns the packet.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn packet(&self) -> &DemuxAttachmentPacket<E, D> {
    &self.packet
  }
  /// Consumes the envelope and returns the packet.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn into_packet(self) -> DemuxAttachmentPacket<E, D> {
    self.packet
  }
  /// Consumes the envelope and returns `(track, packet)`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn into_parts(self) -> (TrackIndex, DemuxAttachmentPacket<E, D>) {
    (self.track, self.packet)
  }
}

impl<E, D> Clone for AttachmentTrackPacket<E, D>
where
  E: DemuxAdapter,
  D: Clone,
  E::AttachmentExtra: Clone,
{
  fn clone(&self) -> Self {
    Self {
      track: self.track,
      packet: self.packet.clone(),
    }
  }
}

impl<E, D> Debug for AttachmentTrackPacket<E, D>
where
  E: DemuxAdapter,
  D: Debug,
  E::AttachmentExtra: Debug,
{
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("AttachmentTrackPacket")
      .field("track", &self.track)
      .field("packet", &self.packet)
      .finish()
  }
}

/// One demuxed packet, with the track it came from.
///
/// Five arms — the whole delivery roster. Packets do not carry track
/// coordinates; this envelope does, which is what lets the same
/// [`VideoPacket`] type be handed straight to a decoder without
/// stripping a field the decoder has no use for.
///
/// A track whose kind is [`TrackKind::Unknown`] has no arm, and its
/// packets are therefore never delivered. The roster is closed at five
/// on purpose: a kind nothing can name is a kind nothing can consume.
#[derive(IsVariant, Unwrap, TryUnwrap)]
#[unwrap(ref, ref_mut)]
#[try_unwrap(ref, ref_mut)]
pub enum DemuxedPacket<E: DemuxAdapter, D> {
  /// A compressed video packet.
  Video(VideoTrackPacket<E, D>),
  /// A compressed audio packet.
  Audio(AudioTrackPacket<E, D>),
  /// A compressed subtitle packet.
  Subtitle(SubtitleTrackPacket<E, D>),
  /// A timed opaque-data packet.
  Data(DataTrackPacket<E, D>),
  /// An attachment payload.
  Attachment(AttachmentTrackPacket<E, D>),
}

impl<E: DemuxAdapter, D> DemuxedPacket<E, D> {
  /// Returns the track this packet belongs to.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn track(&self) -> TrackIndex {
    match self {
      Self::Video(p) => p.track(),
      Self::Audio(p) => p.track(),
      Self::Subtitle(p) => p.track(),
      Self::Data(p) => p.track(),
      Self::Attachment(p) => p.track(),
    }
  }

  /// Returns the kind of track this packet came from.
  ///
  /// Always equal to `demuxer.tracks()[self.track().get()].kind()`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn kind(&self) -> TrackKind {
    match self {
      Self::Video(_) => TrackKind::Video,
      Self::Audio(_) => TrackKind::Audio,
      Self::Subtitle(_) => TrackKind::Subtitle,
      Self::Data(_) => TrackKind::Data,
      Self::Attachment(_) => TrackKind::Attachment,
    }
  }
}

// `Clone` / `Debug` are hand-written for the same associated-type
// reason as `TrackParams` and `TrackInfo` above. The five payload
// types this enum carries route through `DemuxAdapter` and its three
// per-kind sub-adapters — `<E::Video as VideoAdapter>::PacketExtra`,
// `<E::Audio as AudioAdapter>::PacketExtra`, `<E::Subtitle as
// SubtitleAdapter>::PacketExtra`, `E::DataExtra`, `E::AttachmentExtra`
// — five independent associated types plus the buffer `D`, none of
// which `#[derive]`'s flat `E: Clone` bound would name.
impl<E, D> Clone for DemuxedPacket<E, D>
where
  E: DemuxAdapter,
  D: Clone,
  <E::Video as VideoAdapter>::PacketExtra: Clone,
  <E::Audio as AudioAdapter>::PacketExtra: Clone,
  <E::Subtitle as SubtitleAdapter>::PacketExtra: Clone,
  E::DataExtra: Clone,
  E::AttachmentExtra: Clone,
{
  fn clone(&self) -> Self {
    match self {
      Self::Video(p) => Self::Video(p.clone()),
      Self::Audio(p) => Self::Audio(p.clone()),
      Self::Subtitle(p) => Self::Subtitle(p.clone()),
      Self::Data(p) => Self::Data(p.clone()),
      Self::Attachment(p) => Self::Attachment(p.clone()),
    }
  }
}

impl<E, D> Debug for DemuxedPacket<E, D>
where
  E: DemuxAdapter,
  D: Debug,
  <E::Video as VideoAdapter>::PacketExtra: Debug,
  <E::Audio as AudioAdapter>::PacketExtra: Debug,
  <E::Subtitle as SubtitleAdapter>::PacketExtra: Debug,
  E::DataExtra: Debug,
  E::AttachmentExtra: Debug,
{
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Video(p) => f.debug_tuple("Video").field(p).finish(),
      Self::Audio(p) => f.debug_tuple("Audio").field(p).finish(),
      Self::Subtitle(p) => f.debug_tuple("Subtitle").field(p).finish(),
      Self::Data(p) => f.debug_tuple("Data").field(p).finish(),
      Self::Attachment(p) => f.debug_tuple("Attachment").field(p).finish(),
    }
  }
}

#[cfg(test)]
mod tests {
  use core::num::NonZeroI32;

  use super::*;

  // `format!` is not in the prelude in the alloc-without-std tier (the
  // crate is `#![no_std]` there; the crate-root `alloc`-as-`std` alias
  // only makes `std::`-qualified paths resolve, it does not inject
  // prelude items). Unconditional whenever this arm runs: the enclosing
  // `alloc` binding above is gated the same way.
  #[cfg(any(feature = "std", feature = "alloc"))]
  use alloc::format;

  struct VLoop;
  impl VideoAdapter for VLoop {
    type CodecId = u32;
    type PixelFormat = u32;
    type PacketExtra = ();
    type FrameExtra = ();
  }

  struct ALoop;
  impl AudioAdapter for ALoop {
    type CodecId = u32;
    type SampleFormat = u32;
    type ChannelLayout = u32;
    type PacketExtra = ();
    type FrameExtra = ();
  }

  struct SLoop;
  impl SubtitleAdapter for SLoop {
    type CodecId = u32;
    type PacketExtra = ();
    type FrameExtra = ();
  }

  struct Loopback;
  impl DemuxAdapter for Loopback {
    type CodecId = u32;
    type Video = VLoop;
    type Audio = ALoop;
    type Subtitle = SLoop;
    type DataExtra = ();
    type AttachmentExtra = ();
    type TrackExtra = ();
    type Text = &'static str;
  }

  fn ms_tb() -> Timebase {
    Timebase::new(1, NonZeroI32::new(1000).expect("non-zero"))
  }

  #[test]
  fn track_index_round_trips() {
    assert_eq!(TrackIndex::new(3).get(), 3);
    assert_eq!(TrackIndex::default(), TrackIndex::new(0));
  }

  #[test]
  fn the_kind_is_read_off_the_params_arm() {
    // The one-source-of-truth property: there is no way to build a
    // `TrackInfo` whose advertised kind disagrees with its payload.
    let rows: [(TrackParams<Loopback>, TrackKind); 6] = [
      (
        TrackParams::Video(VideoTrackParams::new(1, 1920, 1080, 7, None)),
        TrackKind::Video,
      ),
      (
        TrackParams::Audio(AudioTrackParams::new(2, 48_000, 2, 3, 4)),
        TrackKind::Audio,
      ),
      (
        TrackParams::Subtitle(SubtitleTrackParams::new(3)),
        TrackKind::Subtitle,
      ),
      (TrackParams::Data(DataTrackParams::new(4)), TrackKind::Data),
      (
        TrackParams::Attachment(AttachmentTrackParams::new(5)),
        TrackKind::Attachment,
      ),
      (
        TrackParams::Unknown(UnknownTrackParams::new(0)),
        TrackKind::Unknown,
      ),
    ];
    for (params, expected) in rows {
      let codec = params.codec();
      let info = TrackInfo::<Loopback>::new(ms_tb(), params, ());
      assert_eq!(info.kind(), expected);
      assert_eq!(info.params().kind(), expected);
      assert_eq!(info.params().codec(), codec);
    }
  }

  #[test]
  fn attachment_identity_lives_on_the_track() {
    let info = TrackInfo::<Loopback>::new(
      ms_tb(),
      TrackParams::Attachment(AttachmentTrackParams::new(9)),
      (),
    )
    .with_filename(Some("Arial.ttf"))
    .with_mime_type(Some("application/x-truetype-font"));
    assert_eq!(info.filename().copied(), Some("Arial.ttf"));
    assert_eq!(
      info.mime_type().copied(),
      Some("application/x-truetype-font")
    );
    assert_eq!(info.duration(), None);
  }

  /// **The language seat carries the declaration, whatever shape it is
  /// in** — and says nothing where the container said nothing.
  ///
  /// The four tags are the four real spellings a container writes for
  /// one question, and the row keeps each verbatim: an MKV's 639-2/B
  /// against an MP4's 639-2/T, a BCP 47 tag with a script subtag, and
  /// the explicit `und` an ISOBMFF writes for a track nobody tagged.
  /// Folding any pair together is a downstream vocabulary's job, and a
  /// row that folded here would have made the pair indistinguishable
  /// before that vocabulary ever saw them.
  #[test]
  fn the_language_seat_keeps_the_containers_own_spelling() {
    let row = |language| {
      TrackInfo::<Loopback>::new(
        ms_tb(),
        TrackParams::Subtitle(SubtitleTrackParams::new(3)),
        (),
      )
      .with_language(language)
    };

    assert_eq!(
      row(None).language().copied(),
      None,
      "a container that declares no language leaves the seat empty",
    );
    for tag in ["ger", "deu", "zh-Hans", "und"] {
      assert_eq!(
        row(Some(tag)).language().copied(),
        Some(tag),
        "{tag} must survive the row unchanged",
      );
    }
    assert_ne!(
      row(Some("ger")).language().copied(),
      row(Some("deu")).language().copied(),
      "two spellings of one language stay two values here; reconciling them belongs to \
       whoever owns the language registry",
    );
  }

  /// The seat has both mutators the row's other identity fields have,
  /// and the in-place one can take a declaration back off a row.
  #[test]
  fn the_language_seat_can_be_set_and_cleared_in_place() {
    let mut info = TrackInfo::<Loopback>::new(
      ms_tb(),
      TrackParams::Audio(AudioTrackParams::new(2, 48_000, 2, 3, 4)),
      (),
    );
    assert_eq!(info.language().copied(), None);

    info.set_language(Some("jpn"));
    assert_eq!(info.language().copied(), Some("jpn"));

    info.set_language(None);
    assert_eq!(info.language().copied(), None);
  }

  /// The chapter row carries the container's four numbers and its
  /// title, and the title seat has both mutators the row's other
  /// optional fields have — including the one that can take a title
  /// back off.
  #[test]
  fn a_chapter_row_follows_the_house_shape() {
    let mut chapter = Chapter::<Loopback>::new(
      7,
      ms_tb(),
      Timestamp::new(1_000, ms_tb()),
      Timestamp::new(2_500, ms_tb()),
    );
    assert_eq!(chapter.id(), 7);
    assert_eq!(chapter.timebase(), ms_tb());
    assert_eq!(chapter.start(), Timestamp::new(1_000, ms_tb()));
    assert_eq!(chapter.end(), Timestamp::new(2_500, ms_tb()));
    assert_eq!(chapter.title().copied(), None);

    chapter.set_title(Some("Opening"));
    assert_eq!(chapter.title().copied(), Some("Opening"));
    chapter.set_title(None);
    assert_eq!(chapter.title().copied(), None);

    let titled = chapter.with_title(Some("Closing"));
    assert_eq!(titled.title().copied(), Some("Closing"));
  }

  /// **The id is the container's, not the table position.** A row
  /// built for position 0 may carry any id the file wrote, including a
  /// negative one, and the row keeps it.
  #[test]
  fn the_chapter_id_is_whatever_the_container_wrote() {
    for id in [i64::MIN, -1, 0, 1, i64::MAX] {
      let chapter = Chapter::<Loopback>::new(
        id,
        ms_tb(),
        Timestamp::new(0, ms_tb()),
        Timestamp::new(1, ms_tb()),
      );
      assert_eq!(chapter.id(), id, "{id} must survive the row unchanged");
    }
  }

  /// **An inverted span survives the row.** A container that writes an
  /// end before its start has written something, and a demux tier that
  /// clamped it would hand a consumer a well-formed-looking row
  /// indistinguishable from a file that really was well formed.
  ///
  /// This is also the reason the two instants are separate seats:
  /// [`TimeRange::try_new`](crate::TimeRange::try_new) refuses this
  /// pair, which is the row that has to be carryable.
  #[test]
  fn a_chapter_keeps_an_inverted_span_as_the_container_wrote_it() {
    let chapter = Chapter::<Loopback>::new(
      1,
      ms_tb(),
      Timestamp::new(5_000, ms_tb()),
      Timestamp::new(1_000, ms_tb()),
    );
    assert_eq!(chapter.start(), Timestamp::new(5_000, ms_tb()));
    assert_eq!(chapter.end(), Timestamp::new(1_000, ms_tb()));
    assert!(
      crate::TimeRange::try_new(
        chapter.start().pts(),
        chapter.end().pts(),
        chapter.timebase()
      )
      .is_none(),
      "the range type refuses exactly the pair this row must be able to carry",
    );
  }

  // `format!` needs an allocator; see the `Vec`/`format!` import note
  // above `VLoop`.
  #[cfg(any(feature = "std", feature = "alloc"))]
  #[test]
  fn chapter_clone_matches_the_original() {
    let original = Chapter::<Loopback>::new(
      3,
      ms_tb(),
      Timestamp::new(0, ms_tb()),
      Timestamp::new(1_000, ms_tb()),
    )
    .with_title(Some("Prologue"));
    let cloned = original.clone();
    assert_eq!(cloned, original);
    assert!(format!("{cloned:?}").contains("Chapter"));

    assert_ne!(
      cloned.clone().with_title(Some("Epilogue")),
      original,
      "the title is part of the row's identity",
    );
  }

  #[test]
  fn a_container_duration_carries_the_figure_and_the_librarys_account_of_it() {
    let figure = Timestamp::new(2_021, ms_tb());
    let reported = ContainerDuration::new(figure, DurationSource::FromStream);
    assert_eq!(reported.value(), figure);
    assert_eq!(reported.source(), DurationSource::FromStream);

    // The same figure under a different account is a different answer...
    assert_ne!(
      reported,
      ContainerDuration::new(figure, DurationSource::FromBitrate),
    );
    // ...while the figure itself compares by the quantity it names, in
    // whichever ruler it is written.
    assert_eq!(
      reported,
      ContainerDuration::new(
        Timestamp::new(2_021_000, Timebase::MICROS),
        DurationSource::FromStream
      ),
    );
    let copy = reported;
    assert_eq!(copy, reported);

    // libavformat's three words and the one this tier adds for an
    // account it cannot name: four arms, four predicates, no overlap.
    for (source, flags) in [
      (DurationSource::FromPts, (true, false, false, false)),
      (DurationSource::FromStream, (false, true, false, false)),
      (DurationSource::FromBitrate, (false, false, true, false)),
      (DurationSource::Unknown, (false, false, false, true)),
    ] {
      assert_eq!(
        (
          source.is_from_pts(),
          source.is_from_stream(),
          source.is_from_bitrate(),
          source.is_unknown()
        ),
        flags,
      );
    }
  }

  #[test]
  fn a_measured_end_carries_the_figure_and_its_two_qualifications() {
    let so_far = MeasuredEnd::new(Timestamp::new(1_960, ms_tb()), false, true);
    assert_eq!(so_far.end(), Timestamp::new(1_960, ms_tb()));
    assert!(!so_far.walk_complete());
    assert!(so_far.exact());

    let covered = MeasuredEnd::new(Timestamp::new(2_000, ms_tb()), true, true);
    assert!(covered.walk_complete());
    // The figure compares as the instant it names, so the same moment
    // on another ruler is the same figure...
    assert_eq!(
      covered,
      MeasuredEnd::new(Timestamp::new(2, Timebase::SECONDS), true, true),
    );
    // ...while "so far" against "complete", and a lower bound against
    // an exact end, are different statements about it.
    assert_ne!(
      covered,
      MeasuredEnd::new(Timestamp::new(2_000, ms_tb()), false, true),
    );
    let lower_bound = MeasuredEnd::new(Timestamp::new(2_000, ms_tb()), true, false);
    assert_ne!(covered, lower_bound);
    assert!(!lower_bound.exact(), "the two flags are independent");
    assert!(lower_bound.walk_complete());
    let copy = covered;
    assert_eq!(copy, covered);
  }

  #[test]
  fn data_packet_follows_the_house_shape() {
    let pts = Timestamp::new(1500, ms_tb());
    let p: DataPacket<(), &[u8]> = DataPacket::new(&b"klv"[..], ())
      .with_pts(Some(pts))
      .with_duration(Some(Timestamp::new(40, ms_tb())))
      .with_flags(PacketFlags::KEY);
    assert_eq!(p.pts(), Some(pts));
    assert_eq!(p.duration(), Some(Timestamp::new(40, ms_tb())));
    assert!(p.flags().contains(PacketFlags::KEY));
    let (data, _) = p.into_parts();
    assert_eq!(data, b"klv");
  }

  // `format!` needs an allocator; see the `Vec`/`format!` import note
  // above `VLoop`.
  #[cfg(any(feature = "std", feature = "alloc"))]
  #[test]
  fn data_packet_clone_matches_the_original() {
    let pts = Timestamp::new(1500, ms_tb());
    let original: DataPacket<(), &[u8]> = DataPacket::new(&b"klv"[..], ())
      .with_pts(Some(pts))
      .with_duration(Some(Timestamp::new(40, ms_tb())))
      .with_flags(PacketFlags::KEY);
    let cloned = original.clone();
    assert_eq!(cloned.pts(), original.pts());
    assert_eq!(cloned.duration(), original.duration());
    assert_eq!(cloned.flags(), original.flags());
    assert_eq!(cloned.data(), original.data());
    assert!(format!("{cloned:?}").contains("DataPacket"));
  }

  #[test]
  fn an_attachment_packet_has_no_timestamp_seat() {
    let mut p: AttachmentPacket<(), &[u8]> = AttachmentPacket::new(&b"\x00\x01TTF"[..], ());
    assert_eq!(p.flags(), PacketFlags::empty());
    p.set_flags(PacketFlags::CORRUPT);
    assert!(p.flags().contains(PacketFlags::CORRUPT));
    assert_eq!(p.data(), &&b"\x00\x01TTF"[..]);
  }

  // `format!` needs an allocator; see the `Vec`/`format!` import note
  // above `VLoop`.
  #[cfg(any(feature = "std", feature = "alloc"))]
  #[test]
  fn attachment_packet_clone_matches_the_original() {
    let mut original: AttachmentPacket<(), &[u8]> = AttachmentPacket::new(&b"\x00\x01TTF"[..], ());
    original.set_flags(PacketFlags::CORRUPT);
    let cloned = original.clone();
    assert_eq!(cloned.flags(), original.flags());
    assert_eq!(cloned.data(), original.data());
    assert!(format!("{cloned:?}").contains("AttachmentPacket"));
  }

  #[test]
  fn the_envelope_carries_the_coordinate_and_the_kind() {
    let track = TrackIndex::new(2);
    let packets: [(DemuxedPacket<Loopback, &[u8]>, TrackKind); 5] = [
      (
        DemuxedPacket::Video(VideoTrackPacket::new(track, VideoPacket::new(&[][..], ()))),
        TrackKind::Video,
      ),
      (
        DemuxedPacket::Audio(AudioTrackPacket::new(track, AudioPacket::new(&[][..], ()))),
        TrackKind::Audio,
      ),
      (
        DemuxedPacket::Subtitle(SubtitleTrackPacket::new(
          track,
          SubtitlePacket::new(&[][..], ()),
        )),
        TrackKind::Subtitle,
      ),
      (
        DemuxedPacket::Data(DataTrackPacket::new(track, DataPacket::new(&[][..], ()))),
        TrackKind::Data,
      ),
      (
        DemuxedPacket::Attachment(AttachmentTrackPacket::new(
          track,
          AttachmentPacket::new(&[][..], ()),
        )),
        TrackKind::Attachment,
      ),
    ];
    for (packet, expected) in packets {
      assert_eq!(packet.track(), track);
      assert_eq!(packet.kind(), expected);
    }
  }

  // `format!` needs an allocator; see the `Vec`/`format!` import note
  // above `VLoop`.
  #[cfg(any(feature = "std", feature = "alloc"))]
  #[test]
  fn demuxed_packet_clone_matches_the_original() {
    let track = TrackIndex::new(2);
    let packets: [DemuxedPacket<Loopback, &[u8]>; 5] = [
      DemuxedPacket::Video(VideoTrackPacket::new(track, VideoPacket::new(&[][..], ()))),
      DemuxedPacket::Audio(AudioTrackPacket::new(track, AudioPacket::new(&[][..], ()))),
      DemuxedPacket::Subtitle(SubtitleTrackPacket::new(
        track,
        SubtitlePacket::new(&[][..], ()),
      )),
      DemuxedPacket::Data(DataTrackPacket::new(track, DataPacket::new(&[][..], ()))),
      DemuxedPacket::Attachment(AttachmentTrackPacket::new(
        track,
        AttachmentPacket::new(&[][..], ()),
      )),
    ];
    for packet in packets {
      let cloned = packet.clone();
      assert_eq!(cloned.track(), packet.track());
      assert_eq!(cloned.kind(), packet.kind());
      assert!(!format!("{cloned:?}").is_empty());
    }
  }

  #[test]
  fn demuxed_packet_carries_the_derived_accessor_face() {
    // `IsVariant` / `Unwrap` / `TryUnwrap` — one arm per derive family,
    // proving the house accessor face rides the enum rather than
    // asserting the shape of every variant.
    let track = TrackIndex::new(0);
    let video: DemuxedPacket<Loopback, &[u8]> =
      DemuxedPacket::Video(VideoTrackPacket::new(track, VideoPacket::new(&[][..], ())));
    assert!(video.is_video());
    assert!(!video.is_audio());
    assert_eq!(video.unwrap_video_ref().track(), track);
    assert!(video.try_unwrap_audio().is_err());
  }
}
