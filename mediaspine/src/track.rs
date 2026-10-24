//! What a muxer needs to open one output track.
//!
//! A demuxer describes the tracks it reads with [`TrackInfo`]; a muxer
//! has to be told the same facts about each track it writes, and a few
//! more that a decoder never needed. [`TrackDescription`] is that
//! handoff: the codec and its per-kind parameters, the timebase the
//! track's packets are stamped in, the codec's configuration bytes
//! (*extradata* — an `avcC`, `hvcC` or `esds` record, a Vorbis or Opus
//! header), and the side data that travels with the stream.
//!
//! It is filled from either end of a pipeline: from a demuxed track,
//! for a remux that copies packets, or from an encoder's output, for a
//! track that is encoded fresh.
//!
//! [`TrackInfo`]: crate::demuxer::TrackInfo

use core::fmt::{self, Debug};

use crate::{
  Timebase,
  demuxer::{
    AttachmentTrackParams, DemuxAdapter, DemuxAttachmentPacket, TrackDisposition, TrackInfo,
    TrackKind, TrackParams,
  },
};

/// The description of one track a muxer writes.
///
/// Generic over the adapter `E` (whose vocabulary names the codec, the
/// per-kind parameters and the text carrier), the byte carrier `D` of
/// the extradata and of an attachment's payload, and the side-data
/// carrier `S`, which is the backend's own choice — a list of typed
/// entries for one backend, a borrowed table for another. Nothing here
/// allocates.
///
/// The kind and the codec are read off [`params`](Self::params), never
/// stored beside it, so a description cannot disagree with itself.
///
/// # The portable track metadata
///
/// What the read side states about a track that a muxer writes back
/// travels here too, so a remux keeps it: the [language](Self::language)
/// and the [disposition](Self::disposition) the container declared, for
/// every kind, and for an attachment track its [file](Self::file) — the
/// payload the demuxer delivered and the filename and MIME type it was
/// attached under.
///
/// # An attachment's file, exactly once
///
/// A container writes its attachments with its header, so a muxer is
/// given each one when it is opened, in the track table, and never as
/// a packet. **An attachment track carries exactly one file, by
/// construction**: [`Self::attachment`] takes one, [`Self::new`] refuses
/// an attachment's parameters by name ([`AttachmentWithoutFile`]), and
/// a description holds no second seat for another.
///
/// No `Clone`, for the reason [`TrackInfo`] has none: the parameters
/// are not cheap to duplicate, and a consumer that needs to share a
/// description shares a handle on it.
pub struct TrackDescription<E: DemuxAdapter, D, S> {
  timebase: Timebase,
  params: TrackParams<E>,
  extradata: Option<D>,
  side_data: S,
  language: Option<E::Text>,
  disposition: TrackDisposition,
  /// `Some` exactly when `params` is the attachment arm: the
  /// constructors are the only writers, and they keep the two in step.
  file: Option<AttachedFile<E, D>>,
}

impl<E: DemuxAdapter, D, S> TrackDescription<E, D, S> {
  /// Constructs the description of a track of packets — video, audio,
  /// subtitles, timed data, or one the backend could not classify — with
  /// no extradata, no language and an empty disposition.
  ///
  /// An attachment track is not one: it is described with its file, by
  /// [`Self::attachment`]. Its parameters are refused here by name, and
  /// everything given comes back in the refusal.
  pub fn new(
    timebase: Timebase,
    params: TrackParams<E>,
    side_data: S,
  ) -> Result<Self, AttachmentWithoutFile<E, S>> {
    if let TrackParams::Attachment(_) = params {
      return Err(AttachmentWithoutFile {
        timebase,
        params,
        side_data,
      });
    }
    Ok(Self {
      timebase,
      params,
      extradata: None,
      side_data,
      language: None,
      disposition: TrackDisposition::empty(),
      file: None,
    })
  }

  /// Constructs the description of an attachment track: its parameters
  /// and its one file, with no extradata, no language and an empty
  /// disposition.
  pub fn attachment(
    timebase: Timebase,
    params: AttachmentTrackParams<E>,
    file: AttachedFile<E, D>,
    side_data: S,
  ) -> Self {
    Self {
      timebase,
      params: TrackParams::Attachment(params),
      extradata: None,
      side_data,
      language: None,
      disposition: TrackDisposition::empty(),
      file: Some(file),
    }
  }

  /// The description of a demuxed track, for a remux that copies its
  /// packets — **lossless for everything the two rows share**: the
  /// timebase, the parameters, the declared language and disposition,
  /// and for an attachment track its file, from `payload` (the packet
  /// the demuxer delivered for it at open) and the row's filename and
  /// MIME type.
  ///
  /// **The row is borrowed, never consumed.** A demuxer keeps its rows
  /// for its whole session and hands out handles on them — the
  /// `Demuxer::tracks` face of `mediadecode` answers a slice of handles
  /// that dereference to rows — so a remux describes each track from the
  /// handle it holds (`&**handle`), and the session keeps every row. The
  /// portable fields are copied explicitly: the timebase and the
  /// disposition by `Copy`, the parameters by their named
  /// [`TrackParams::duplicate`], and the language, filename and MIME type
  /// by the text carrier's `Clone`, a refcount bump.
  ///
  /// What only the read side states stays on the row: the duration the
  /// library reported, and the backend's extras. Extradata is the
  /// backend's to set ([`with_extradata`](Self::with_extradata)): this
  /// tier's row does not carry it.
  ///
  /// Refused by name, with what was given handed back
  /// ([`TrackMismatch`]): an attachment track with no `payload`, a
  /// `payload` for a track of another kind, and a row of another kind
  /// that claims an attachment's filename or MIME type — which the row
  /// states for attachments alone, and which this side has nowhere to
  /// carry for any other kind.
  pub fn from_track_info(
    info: &TrackInfo<E>,
    payload: Option<DemuxAttachmentPacket<E, D>>,
    side_data: S,
  ) -> Result<Self, TrackMismatch<E, D, S>> {
    let kind = info.kind();
    let attachment = matches!(kind, TrackKind::Attachment);
    let reason = match (attachment, payload.is_some()) {
      (true, false) => Some(MismatchReason::MissingFile),
      (false, true) => Some(MismatchReason::StrayFile),
      (false, false) if info.filename().is_some() || info.mime_type().is_some() => {
        Some(MismatchReason::StrayIdentity)
      }
      _ => None,
    };
    if let Some(reason) = reason {
      return Err(TrackMismatch {
        reason,
        kind,
        payload,
        side_data,
      });
    }

    // The portable fields, each copied by name from the borrowed row.
    let timebase = info.timebase();
    let params = info.params().duplicate();
    let language = info.language().cloned();
    let disposition = info.disposition();
    Ok(match (params, payload) {
      (TrackParams::Attachment(params), Some(payload)) => {
        let file = AttachedFile::new(payload)
          .with_filename(info.filename().cloned())
          .with_mime_type(info.mime_type().cloned());
        Self::attachment(timebase, params, file, side_data)
          .with_language(language)
          .with_disposition(disposition)
      }
      // Any other kind, with no payload (the reasons above leave nothing
      // else), and no identity to carry.
      (params, _) => Self {
        timebase,
        params,
        extradata: None,
        side_data,
        language,
        disposition,
        file: None,
      },
    })
  }

  /// Returns the track's kind, read off [`Self::params`].
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn kind(&self) -> TrackKind {
    self.params.kind()
  }

  /// Returns the codec identifier, read off [`Self::params`].
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn codec(&self) -> E::CodecId {
    self.params.codec()
  }

  /// Returns the timebase every timestamp on this track is expressed in.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn timebase(&self) -> Timebase {
    self.timebase
  }

  /// Returns the codec and its per-kind parameters.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn params(&self) -> &TrackParams<E> {
    &self.params
  }

  /// Returns the codec's configuration bytes, or `None` where the codec
  /// carries none (a stream that configures itself in band).
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn extradata(&self) -> Option<&D> {
    self.extradata.as_ref()
  }

  /// Returns the side data that travels with the stream.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn side_data(&self) -> &S {
    &self.side_data
  }

  /// Returns the side data mutably, for a caller that adds an entry.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn side_data_mut(&mut self) -> &mut S {
    &mut self.side_data
  }

  /// Returns the language the track is declared in, exactly as the read
  /// side states it ([`TrackInfo::language`]) — `None` where none is
  /// declared. Carried whole, never normalised.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn language(&self) -> Option<&E::Text> {
    self.language.as_ref()
  }

  /// Returns the track's disposition, exactly as the read side states it
  /// ([`TrackInfo::disposition`]) — the empty set where none is declared.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn disposition(&self) -> TrackDisposition {
    self.disposition
  }

  /// Returns an attachment track's file — `Some` exactly when the track
  /// is an attachment.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn file(&self) -> Option<&AttachedFile<E, D>> {
    self.file.as_ref()
  }

  /// Sets the extradata (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub fn with_extradata(mut self, value: Option<D>) -> Self {
    self.extradata = value;
    self
  }

  /// Sets the extradata in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn set_extradata(&mut self, value: Option<D>) -> &mut Self {
    self.extradata = value;
    self
  }

  /// Sets the declared language (consuming builder). Pass the tag as the
  /// container writes it; see [`TrackInfo::language`] for why nothing
  /// normalises it.
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub fn with_language(mut self, value: Option<E::Text>) -> Self {
    self.language = value;
    self
  }

  /// Sets the declared language in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn set_language(&mut self, value: Option<E::Text>) -> &mut Self {
    self.language = value;
    self
  }

  /// Sets the declared disposition (consuming builder). Pass the bits
  /// the container declares; see [`TrackInfo::disposition`].
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_disposition(mut self, value: TrackDisposition) -> Self {
    self.disposition = value;
    self
  }

  /// Sets the declared disposition in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_disposition(&mut self, value: TrackDisposition) -> &mut Self {
    self.disposition = value;
    self
  }

  /// Splits the description into its timebase, parameters, extradata,
  /// side data, language and — for an attachment track — its file.
  ///
  /// The disposition is `Copy`, and is not among the parts: read it with
  /// [`disposition`](Self::disposition) before the split.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn into_parts(
    self,
  ) -> (
    Timebase,
    TrackParams<E>,
    Option<D>,
    S,
    Option<E::Text>,
    Option<AttachedFile<E, D>>,
  ) {
    (
      self.timebase,
      self.params,
      self.extradata,
      self.side_data,
      self.language,
      self.file,
    )
  }
}

// `Debug` is hand-written for the reason `TrackInfo`'s is: a derive
// would demand `E: Debug`, which no field needs — the parameters carry
// their own bounds through the adapter's associated types.
impl<E: DemuxAdapter, D: Debug, S: Debug> Debug for TrackDescription<E, D, S>
where
  E::AttachmentExtra: Debug,
{
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("TrackDescription")
      .field("timebase", &self.timebase)
      .field("params", &self.params)
      .field("extradata", &self.extradata)
      .field("side_data", &self.side_data)
      .field("language", &self.language)
      .field("disposition", &self.disposition)
      .field("file", &self.file)
      .finish()
  }
}

/// An attachment track's file, as a muxer writes it: the payload the
/// demuxer delivered — bytes, flags and the backend's extras — and the
/// identity it was attached under.
pub struct AttachedFile<E: DemuxAdapter, D> {
  payload: DemuxAttachmentPacket<E, D>,
  filename: Option<E::Text>,
  mime_type: Option<E::Text>,
}

impl<E: DemuxAdapter, D> AttachedFile<E, D> {
  /// Constructs a file from its payload, with no filename and no MIME
  /// type.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(payload: DemuxAttachmentPacket<E, D>) -> Self {
    Self {
      payload,
      filename: None,
      mime_type: None,
    }
  }

  /// Returns the payload: the attached file's bytes, flags and extras.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn payload(&self) -> &DemuxAttachmentPacket<E, D> {
    &self.payload
  }

  /// Returns the filename the file was attached under, when known.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn filename(&self) -> Option<&E::Text> {
    self.filename.as_ref()
  }

  /// Returns the file's declared MIME type, when known.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn mime_type(&self) -> Option<&E::Text> {
    self.mime_type.as_ref()
  }

  /// Sets the filename (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub fn with_filename(mut self, value: Option<E::Text>) -> Self {
    self.filename = value;
    self
  }

  /// Sets the MIME type (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub fn with_mime_type(mut self, value: Option<E::Text>) -> Self {
    self.mime_type = value;
    self
  }

  /// Sets the filename in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn set_filename(&mut self, value: Option<E::Text>) -> &mut Self {
    self.filename = value;
    self
  }

  /// Sets the MIME type in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn set_mime_type(&mut self, value: Option<E::Text>) -> &mut Self {
    self.mime_type = value;
    self
  }

  /// Splits the file into its payload, filename and MIME type.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn into_parts(
    self,
  ) -> (
    DemuxAttachmentPacket<E, D>,
    Option<E::Text>,
    Option<E::Text>,
  ) {
    (self.payload, self.filename, self.mime_type)
  }
}

impl<E: DemuxAdapter, D: Debug> Debug for AttachedFile<E, D>
where
  E::AttachmentExtra: Debug,
{
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("AttachedFile")
      .field("payload", &self.payload)
      .field("filename", &self.filename)
      .field("mime_type", &self.mime_type)
      .finish()
  }
}

/// [`TrackDescription::new`] was given an attachment track's
/// parameters. An attachment is described with its file, by
/// [`TrackDescription::attachment`]; everything given comes back here.
pub struct AttachmentWithoutFile<E: DemuxAdapter, S> {
  timebase: Timebase,
  params: TrackParams<E>,
  side_data: S,
}

impl<E: DemuxAdapter, S> AttachmentWithoutFile<E, S> {
  /// Returns everything [`TrackDescription::new`] was given.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn into_parts(self) -> (Timebase, TrackParams<E>, S) {
    (self.timebase, self.params, self.side_data)
  }
}

impl<E: DemuxAdapter, S> Debug for AttachmentWithoutFile<E, S> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("AttachmentWithoutFile")
      .field("timebase", &self.timebase)
      .field("params", &self.params)
      .finish_non_exhaustive()
  }
}

impl<E: DemuxAdapter, S> fmt::Display for AttachmentWithoutFile<E, S> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str("an attachment track is described with its file, by `TrackDescription::attachment`")
  }
}

impl<E: DemuxAdapter, S> core::error::Error for AttachmentWithoutFile<E, S> {}

/// Why [`TrackDescription::from_track_info`] refused a row.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum MismatchReason {
  /// The row is an attachment track, and no payload came with it.
  MissingFile,
  /// A payload came for a row of another kind.
  StrayFile,
  /// A row of another kind claims a filename or MIME type — an
  /// attachment's identity, which the write side carries for
  /// attachments alone.
  StrayIdentity,
}

/// [`TrackDescription::from_track_info`] refused a row, by name: the
/// [reason](Self::reason) and the row's [kind](Self::kind), and what it
/// was given by value — the payload and the side data — handed back. The
/// row itself was only borrowed, and is still the caller's.
pub struct TrackMismatch<E: DemuxAdapter, D, S> {
  reason: MismatchReason,
  kind: TrackKind,
  payload: Option<DemuxAttachmentPacket<E, D>>,
  side_data: S,
}

impl<E: DemuxAdapter, D, S> TrackMismatch<E, D, S> {
  /// Why the row was refused.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn reason(&self) -> MismatchReason {
    self.reason
  }

  /// The kind of the row that was refused.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn kind(&self) -> TrackKind {
    self.kind
  }

  /// Returns the payload and the side data, as they were given.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn into_parts(self) -> (Option<DemuxAttachmentPacket<E, D>>, S) {
    (self.payload, self.side_data)
  }
}

impl<E: DemuxAdapter, D, S> Debug for TrackMismatch<E, D, S> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("TrackMismatch")
      .field("reason", &self.reason)
      .field("kind", &self.kind)
      .finish_non_exhaustive()
  }
}

impl<E: DemuxAdapter, D, S> fmt::Display for TrackMismatch<E, D, S> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self.reason {
      MismatchReason::MissingFile => {
        f.write_str("an attachment track came without the payload the demuxer delivered for it")
      }
      MismatchReason::StrayFile => {
        write!(f, "an attachment payload came for a {:?} track", self.kind)
      }
      MismatchReason::StrayIdentity => write!(
        f,
        "a {:?} track claims an attachment's filename or MIME type",
        self.kind
      ),
    }
  }
}

impl<E: DemuxAdapter, D, S> core::error::Error for TrackMismatch<E, D, S> {}

#[cfg(test)]
mod tests {
  use core::num::NonZeroI32;

  use super::*;
  use crate::{
    Timestamp,
    adapter::{AudioAdapter, SubtitleAdapter, VideoAdapter},
    demuxer::{
      AttachmentPacket, AudioTrackParams, DataTrackParams, SubtitleTrackParams, UnknownTrackParams,
      VideoTrackParams,
    },
    packet::PacketFlags,
  };

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

  /// The kind and the codec are the parameters' own, so there is no
  /// way to build a description that disagrees with itself.
  #[test]
  fn the_kind_and_the_codec_are_read_off_the_params() {
    let video = TrackDescription::<Loopback, &[u8], ()>::new(
      ms_tb(),
      TrackParams::Video(VideoTrackParams::new(27, 1920, 1080, 0, None)),
      (),
    )
    .expect("a video track");
    assert_eq!(video.kind(), TrackKind::Video);
    assert_eq!(video.codec(), 27);
    assert_eq!(video.timebase(), ms_tb());

    let audio = TrackDescription::<Loopback, &[u8], ()>::new(
      ms_tb(),
      TrackParams::Audio(AudioTrackParams::new(86018, 48_000, 2, 8, 3)),
      (),
    )
    .expect("an audio track");
    assert_eq!(audio.kind(), TrackKind::Audio);
    assert_eq!(audio.codec(), 86018);
  }

  /// Extradata is absent until it is given, and what goes in comes back
  /// out of `into_parts` unchanged, with the side data beside it.
  #[test]
  fn extradata_is_absent_until_given_and_every_part_comes_back() {
    const AVCC: &[u8] = &[0x01, 0x64, 0x00, 0x28];
    let description = TrackDescription::<Loopback, &[u8], [u8; 2]>::new(
      ms_tb(),
      TrackParams::Video(VideoTrackParams::new(27, 1280, 720, 0, None)),
      [7, 9],
    )
    .expect("a video track");
    assert_eq!(description.extradata(), None);

    let mut description = description.with_extradata(Some(AVCC));
    assert_eq!(description.extradata(), Some(&AVCC));
    description.side_data_mut()[1] = 8;
    assert_eq!(description.side_data(), &[7, 8]);

    let disposition = description.disposition();
    let (timebase, params, extradata, side_data, language, file) = description.into_parts();
    assert_eq!(timebase, ms_tb());
    assert_eq!(params.kind(), TrackKind::Video);
    assert_eq!(extradata, Some(AVCC));
    assert_eq!(side_data, [7, 8]);
    assert_eq!(language, None);
    assert_eq!(disposition, TrackDisposition::empty());
    assert!(file.is_none(), "a video track carries no file");
  }

  const FONT: &[u8] = b"OTTO, the font's bytes";

  fn font_row() -> TrackInfo<Loopback> {
    TrackInfo::new(
      ms_tb(),
      TrackParams::Attachment(AttachmentTrackParams::new(98_307)),
      (),
    )
    .with_filename(Some("Noto.otf"))
    .with_mime_type(Some("font/otf"))
    .with_language(Some("und"))
    // A bit no `TrackDisposition` constant names: carried as the bit it
    // is.
    .with_disposition(TrackDisposition::from_u32(0x0040_0000))
  }

  fn font_payload() -> DemuxAttachmentPacket<Loopback, &'static [u8]> {
    AttachmentPacket::new(FONT, ()).with_flags(PacketFlags::KEY)
  }

  /// **An attachment's identity and payload survive the conversion.** The
  /// filename and MIME type the read side states, and the payload the
  /// demuxer delivered — the very bytes, its flags — come through into
  /// the track's one file, with the track's language and disposition
  /// beside it (an unnamed disposition bit included).
  #[test]
  fn an_attachments_identity_and_payload_survive_the_conversion() {
    let row = font_row();
    let description =
      TrackDescription::<Loopback, &[u8], ()>::from_track_info(&row, Some(font_payload()), ())
        .expect("an attachment track with its file");
    assert_eq!(description.kind(), TrackKind::Attachment);
    assert_eq!(description.codec(), 98_307);
    assert_eq!(description.language(), Some(&"und"));
    assert_eq!(
      description.disposition(),
      TrackDisposition::from_u32(0x0040_0000)
    );
    let file = description
      .file()
      .expect("an attachment track carries its file");
    assert_eq!(file.filename(), Some(&"Noto.otf"));
    assert_eq!(file.mime_type(), Some(&"font/otf"));
    assert!(
      core::ptr::eq(*file.payload().data(), FONT),
      "the very bytes the demuxer delivered"
    );
    assert_eq!(file.payload().flags(), PacketFlags::KEY);
    assert_eq!(row.filename(), Some(&"Noto.otf"), "the row is still whole");
  }

  /// **A track's language survives the conversion**, for every kind, and
  /// what only the read side states — the library's duration — stays on
  /// the row, which the conversion only borrows.
  #[test]
  fn a_tracks_language_survives_the_conversion() {
    let reported = Timestamp::new(90_000, ms_tb());
    let row = TrackInfo::<Loopback>::new(
      ms_tb(),
      TrackParams::Audio(AudioTrackParams::new(86018, 48_000, 2, 8, 3)),
      (),
    )
    .with_language(Some("deu"))
    .with_duration(Some(reported));
    let description = TrackDescription::<Loopback, &[u8], ()>::from_track_info(&row, None, ())
      .expect("an audio track");
    assert_eq!(description.language(), Some(&"deu"));
    assert!(description.file().is_none());
    assert_eq!(row.duration(), Some(reported), "the row keeps its duration");
    assert_eq!(row.language(), Some(&"deu"), "and its language");
  }

  /// **The conversion borrows its row and copies every portable field.**
  /// A row is described twice from one borrow — as a remux describes it
  /// from the handle a demuxer keeps — and both descriptions carry every
  /// field of every arm's parameters, the timebase, the language and the
  /// disposition, while the row stays as it was.
  #[test]
  fn the_conversion_borrows_its_row_and_copies_every_portable_field() {
    let tb = Timebase::new(1, core::num::NonZeroI32::new(90_000).expect("non-zero"));
    let rate = Some(Timebase::new(
      30_000,
      core::num::NonZeroI32::new(1001).expect("non-zero"),
    ));
    let arms: [TrackParams<Loopback>; 5] = [
      TrackParams::Video(VideoTrackParams::new(27, 1920, 1080, 64, rate)),
      TrackParams::Audio(AudioTrackParams::new(86018, 44_100, 6, 8, 1551)),
      TrackParams::Subtitle(SubtitleTrackParams::new(94_210)),
      TrackParams::Data(DataTrackParams::new(98_313)),
      TrackParams::Unknown(UnknownTrackParams::new(0)),
    ];
    let declared = TrackDisposition::DEFAULT | TrackDisposition::FORCED;
    for params in arms {
      let row = TrackInfo::<Loopback>::new(tb, params, ())
        .with_language(Some("jpn"))
        .with_disposition(declared);
      for _ in 0..2 {
        let description = TrackDescription::<Loopback, &[u8], ()>::from_track_info(&row, None, ())
          .expect("a track of packets");
        assert_eq!(description.timebase(), tb);
        assert_eq!(description.language(), Some(&"jpn"));
        assert_eq!(description.disposition(), declared);
        assert_eq!(description.kind(), row.kind());
        match (description.params(), row.params()) {
          (TrackParams::Video(copy), TrackParams::Video(source)) => {
            assert_eq!(copy.codec(), source.codec());
            assert_eq!(
              (copy.width(), copy.height()),
              (source.width(), source.height())
            );
            assert_eq!(copy.pixel_format(), source.pixel_format());
            assert_eq!(copy.frame_rate(), source.frame_rate());
          }
          (TrackParams::Audio(copy), TrackParams::Audio(source)) => {
            assert_eq!(copy.codec(), source.codec());
            assert_eq!(copy.sample_rate(), source.sample_rate());
            assert_eq!(copy.channel_count(), source.channel_count());
            assert_eq!(copy.sample_format(), source.sample_format());
            assert_eq!(copy.channel_layout(), source.channel_layout());
          }
          (copy, source) => {
            assert_eq!(copy.kind(), source.kind());
            assert_eq!(copy.codec(), source.codec());
          }
        }
      }
      assert_eq!(row.language(), Some(&"jpn"), "the row is untouched");
    }
  }

  /// **An attachment track has exactly one file.** Without one it is
  /// refused by name — by the conversion and by [`TrackDescription::new`]
  /// alike — and everything given comes back; a second has no seat.
  #[test]
  fn an_attachment_track_without_its_file_is_refused_by_name() {
    let row = font_row();
    let refused = TrackDescription::<Loopback, &[u8], ()>::from_track_info(&row, None, ())
      .expect_err("no file, no attachment track");
    assert_eq!(refused.reason(), MismatchReason::MissingFile);
    assert_eq!(refused.kind(), TrackKind::Attachment);
    let (payload, ()) = refused.into_parts();
    assert_eq!(
      row.filename(),
      Some(&"Noto.otf"),
      "the row was only borrowed, and is whole"
    );
    assert!(payload.is_none());

    let refused = TrackDescription::<Loopback, &[u8], ()>::new(
      ms_tb(),
      TrackParams::Attachment(AttachmentTrackParams::new(98_307)),
      (),
    )
    .expect_err("an attachment's parameters alone describe no track");
    let (timebase, params, ()) = refused.into_parts();
    assert_eq!((timebase, params.kind()), (ms_tb(), TrackKind::Attachment));
  }

  /// **A file belongs to an attachment track alone.** A payload offered
  /// for a track of another kind is refused by name and handed back, and
  /// so is a row of another kind that claims an attachment's identity.
  #[test]
  fn a_file_for_another_kind_is_refused_by_name() {
    let video = || {
      TrackInfo::<Loopback>::new(
        ms_tb(),
        TrackParams::Video(VideoTrackParams::new(27, 1920, 1080, 0, None)),
        (),
      )
    };
    let refused =
      TrackDescription::<Loopback, &[u8], ()>::from_track_info(&video(), Some(font_payload()), ())
        .expect_err("a video track takes no file");
    assert_eq!(refused.reason(), MismatchReason::StrayFile);
    assert_eq!(refused.kind(), TrackKind::Video);
    let (payload, ()) = refused.into_parts();
    assert!(
      core::ptr::eq(*payload.expect("handed back").data(), FONT),
      "the payload comes back untouched"
    );

    let refused = TrackDescription::<Loopback, &[u8], ()>::from_track_info(
      &video().with_filename(Some("not-a-font.otf")),
      None,
      (),
    )
    .expect_err("a video track carries no attachment identity");
    assert_eq!(refused.reason(), MismatchReason::StrayIdentity);
  }
}
