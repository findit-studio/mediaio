//! The muxer face — compressed packets in, one container out.
//!
//! A [`Muxer`] is the write side's mirror of the `Demuxer` face in
//! `mediadecode`: an opened container session that takes packets, one
//! write per kind, and a [`finish`](Muxer::finish) that completes the
//! file. It takes the very envelopes a demuxer delivers — a
//! [`VideoTrackPacket`] carries its track coordinate with it — and
//! [`write_packet`](Muxer::write_packet) routes a [`DemuxedPacket`] to
//! the write for its kind. A refused packet comes back inside the
//! error, untouched — refused by a rule every muxer keeps
//! ([`MuxRefusal`]) or by one the backend's container keeps
//! ([`Muxer::Refusal`]).
//!
//! # A remux: collect, then open, then write
//!
//! A container writes its attachments with its header, so a muxer is
//! given them when it is opened, in its track table, and never as a
//! packet. A demuxer hands out every attachment before its first timed
//! packet, so a remux collects them first — each into its track's
//! description — opens the muxer with the whole table, and then writes
//! the timed packets:
//!
//! ```
//! use core::ops::Deref;
//! use mediaencode::{MuxError, Muxer};
//! use mediaencode::mediaspine::{
//!   demuxer::{DemuxAdapter, DemuxedPacket, TrackInfo},
//!   track::{TrackDescription, TrackMismatch},
//! };
//!
//! /// The table a muxer is opened with: one description per row, each
//! /// attachment track carrying the payload the demuxer delivered for it
//! /// before its first timed packet — which comes back too, for the
//! /// write loop.
//! ///
//! /// `rows` is what a demuxer's `tracks()` answers: handles that
//! /// dereference to the rows its session keeps. Each row is described
//! /// through a borrow, so nothing is taken from the session.
//! fn table<A: DemuxAdapter, B, H: Deref<Target = TrackInfo<A>>>(
//!   rows: &[H],
//!   packets: &mut impl Iterator<Item = DemuxedPacket<A, B>>,
//! ) -> Result<
//!   (Vec<TrackDescription<A, B, ()>>, Option<DemuxedPacket<A, B>>),
//!   TrackMismatch<A, B, ()>,
//! > {
//!   let mut files: Vec<Option<_>> = rows.iter().map(|_| None).collect();
//!   let mut first = None;
//!   for packet in packets {
//!     match packet {
//!       DemuxedPacket::Attachment(p) => {
//!         let (track, payload) = p.into_parts();
//!         if let Some(file) = files.get_mut(track.get()) {
//!           *file = Some(payload);
//!         }
//!       }
//!       timed => {
//!         first = Some(timed);
//!         break;
//!       }
//!     }
//!   }
//!   let mut table = Vec::with_capacity(rows.len());
//!   for (row, file) in rows.iter().zip(files) {
//!     // `&**row`: the handle, dereferenced to the row it shares.
//!     table.push(TrackDescription::from_track_info(&**row, file, ())?);
//!   }
//!   Ok((table, first))
//! }
//!
//! /// Writes the timed packets once the muxer is open. An attachment here
//! /// would break the demuxer's promise: `write_packet` refuses it by
//! /// name and hands it back, as it does every refused packet — the
//! /// backend's own refusals included.
//! fn write_all<M: Muxer>(
//!   muxer: &mut M,
//!   packets: impl IntoIterator<Item = DemuxedPacket<M::Adapter, M::Buffer>>,
//! ) -> Result<(), MuxError<M::Error, M::Refusal, DemuxedPacket<M::Adapter, M::Buffer>>> {
//!   for packet in packets {
//!     muxer.write_packet(packet)?;
//!   }
//!   Ok(())
//! }
//! ```
//!
//! # Construction is not on the trait
//!
//! As with the `Demuxer`, opening is each backend's own business: a
//! backend opens a muxer from one
//! [`TrackDescription`](mediaspine::track::TrackDescription) per output
//! track — the track at [`TrackIndex::new(i)`](TrackIndex::new) is the
//! `i`-th description, an attachment track's carrying its one file —
//! and from wherever the bytes go.
//!
//! # Finishing ends the session
//!
//! [`finish`](Muxer::finish) takes the muxer by value, so nothing can be
//! written after the container is complete — that is the compiler's
//! rule, not a runtime check:
//!
//! ```compile_fail,E0382
//! # use mediaencode::Muxer;
//! # use mediaencode::mediaspine::demuxer::VideoTrackPacket;
//! fn write_after_finish<M: Muxer>(
//!   mut muxer: M,
//!   first: VideoTrackPacket<M::Adapter, M::Buffer>,
//!   second: VideoTrackPacket<M::Adapter, M::Buffer>,
//! ) {
//!   let _ = muxer.write_video(first);
//!   let _ = muxer.finish();
//!   let _ = muxer.write_video(second);
//! }
//! ```

use mediaspine::{
  Timestamp,
  demuxer::{
    AudioTrackPacket, DataTrackPacket, DemuxAdapter, DemuxedPacket, SubtitleTrackPacket,
    TrackIndex, TrackKind, VideoTrackPacket,
  },
};

/// A refusal every muxer names the same way.
#[derive(Copy, Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum MuxRefusal {
  /// The packet names a track the muxer was not opened with.
  #[error("track {} is not in the muxer's table", .track.get())]
  UnknownTrack {
    /// The track the packet named.
    track: TrackIndex,
  },
  /// The packet's kind is not the track's.
  #[error("track {} carries {expected:?} packets, not {offered:?}", .track.get())]
  KindMismatch {
    /// The track the packet named.
    track: TrackIndex,
    /// The track's kind.
    expected: TrackKind,
    /// The packet's kind.
    offered: TrackKind,
  },
  /// The packet carries no decode timestamp — for a subtitle or data
  /// packet, whose decode time is its presentation time, no
  /// presentation timestamp — and a muxer never invents one.
  #[error("a packet on track {} carries no decode timestamp", .track.get())]
  MissingDts {
    /// The track the packet named.
    track: TrackIndex,
  },
  /// The packet's decode timestamp is earlier than the last one written
  /// on its track. Within a track, decode order is the order packets are
  /// written in, and the container records it as such, so decode
  /// timestamps may not go back — see [`check_decode_order`], the rule
  /// itself.
  ///
  /// **An equal one is admitted.** Two subtitle cues or two timed-data
  /// events at one instant are ordinary input, and a remux of them must
  /// not fail on the face. Whether a container takes two packets at one
  /// decode time is the container's question: a backend whose container
  /// needs them strictly increasing refuses the equal one by its own
  /// name ([`MuxError::Backend`]), and hands it back as this one does.
  #[error(
    "decode timestamps on track {} must not go back: {} after {}",
    .track.get(), .offered.pts(), .last.pts()
  )]
  NonMonotonicDts {
    /// The track the packet named.
    track: TrackIndex,
    /// The last decode timestamp written on the track.
    last: Timestamp,
    /// The decode timestamp the packet carries.
    offered: Timestamp,
  },
  /// The packet's presentation timestamp is earlier than its decode
  /// timestamp, as the container stores the two. A picture cannot be
  /// shown before it is decoded, and no container records one that is —
  /// see [`check_presentation_order`], the rule itself, and why a
  /// backend judges the stored times rather than the instants offered.
  /// The fields are the times the packet carries.
  #[error(
    "a packet on track {} is presented at {} before it is decoded at {}",
    .track.get(), .pts.pts(), .dts.pts()
  )]
  PtsBeforeDts {
    /// The track the packet named.
    track: TrackIndex,
    /// The presentation timestamp the packet carries.
    pts: Timestamp,
    /// The decode timestamp the packet carries.
    dts: Timestamp,
  },
  /// An attachment was offered as a packet. A container writes its
  /// attachments with its header, so a muxer is given each one when it
  /// is opened, in its track table, and never takes one as a packet.
  #[error(
    "the attachment on track {} is not written as a packet; \
     a muxer is given its attachments in the track table it is opened with",
    .track.get()
  )]
  Attachment {
    /// The track the packet named.
    track: TrackIndex,
  },
}

/// The portable decode-order rule, for a muxer to apply to each track:
/// `offered` may equal the `last` decode timestamp written on `track`,
/// never precede it — [`MuxRefusal::NonMonotonicDts`] otherwise.
/// Timestamps compare by the instant they name, so two rulers mix.
///
/// One function rather than a rule restated in every backend: the
/// portable face admits what this admits, and a backend stricter for its
/// container adds its own refusal after this one, in its own vocabulary
/// ([`Muxer::Refusal`], handed back in [`MuxError::Backend`]).
pub fn check_decode_order(
  track: TrackIndex,
  last: Option<Timestamp>,
  offered: Timestamp,
) -> Result<(), MuxRefusal> {
  match last {
    Some(last) if offered < last => Err(MuxRefusal::NonMonotonicDts {
      track,
      last,
      offered,
    }),
    _ => Ok(()),
  }
}

/// The portable presentation-order rule, for a muxer to apply to each
/// packet **as its container stores it**: a packet is presented no
/// earlier than it is decoded — [`MuxRefusal::PtsBeforeDts`] otherwise.
/// A packet with no presentation timestamp has nothing to judge.
///
/// It is the container's comparison, so a backend applies it to the two
/// times in the representation it hands its container — counted in the
/// stream's own time base, where that is how the container keeps them —
/// and not to the instants a caller offered. **It is no law over
/// instants**: two times a hair apart may count to one tick of the
/// stream, and a container that stores them as one tick writes the
/// packet, as a stream copy of a file with sub-tick jitter must. The
/// refusal a backend returns names the times the packet carries. The
/// timestamps given compare by the instant they name, so they may be
/// stamped in different rulers; a backend whose container stores the
/// instants themselves passes them as they are.
///
/// Every container keeps the rule over what it stores — libavformat
/// refuses such a packet itself (`compute_muxer_pkt_fields`,
/// `libavformat/mux.c` 567–572 in FFmpeg 9.0.1), comparing the counts it
/// is handed, as a fault that consumes the packet — so a backend names
/// it first, and hands the packet back.
pub fn check_presentation_order(
  track: TrackIndex,
  pts: Option<Timestamp>,
  dts: Timestamp,
) -> Result<(), MuxRefusal> {
  match pts {
    Some(pts) if pts < dts => Err(MuxRefusal::PtsBeforeDts { track, pts, dts }),
    _ => Ok(()),
  }
}

/// How a muxer write failed: a refusal every muxer names the same way,
/// or one the backend names in its own vocabulary — each with the packet
/// handed back — or the backend's own fault.
///
/// `E` is the backend's fault ([`Muxer::Error`]), `R` its own refusals
/// ([`Muxer::Refusal`]), and `P` the packet the write took — each write
/// method names its own — so a refused packet can be retried, rerouted or
/// dropped whatever its buffer and extras are: it is moved back, never
/// cloned.
///
/// # Two vocabularies of refusal
///
/// The portable one, [`MuxRefusal`], is closed: what every muxer refuses,
/// named the same way by all of them. A backend's is open: its container
/// keeps rules the face does not — two packets at one decode time in a
/// container that needs decode times strictly increasing, say — and it
/// names each in a vocabulary of its own. Either way nothing was written
/// and the packet comes back, which is what a caller needs to be able to
/// count on: a backend never has to report a refusal as a fault, losing
/// the packet, because its rule is not one the face knows.
#[derive(Debug, thiserror::Error)]
pub enum MuxError<E, R, P> {
  /// A refusal every muxer names the same way. Nothing was written.
  #[error("{refusal}")]
  Refused {
    /// What was refused, and why.
    refusal: MuxRefusal,
    /// The packet offered, handed back untouched.
    packet: P,
  },
  /// A refusal the backend names in its own vocabulary
  /// ([`Muxer::Refusal`]) — a rule its container keeps that the portable
  /// face does not. Nothing was written.
  #[error("{refusal}")]
  Backend {
    /// What was refused, and why, in the backend's words.
    refusal: R,
    /// The packet offered, handed back untouched.
    packet: P,
  },
  /// The backend's own fault. The packet is not handed back: a fault can
  /// come after the backend has taken it, so it may have been consumed.
  #[error(transparent)]
  Fault(E),
}

impl<E, R, P> MuxError<E, R, P> {
  /// Maps the handed-back packet, leaving the refusal — portable or the
  /// backend's — or the fault as it is. A caller that routes several
  /// kinds of packet through one error type gives each refused packet
  /// back its envelope this way; see the module's remux example.
  pub fn map_packet<Q>(self, f: impl FnOnce(P) -> Q) -> MuxError<E, R, Q> {
    match self {
      Self::Refused { refusal, packet } => MuxError::Refused {
        refusal,
        packet: f(packet),
      },
      Self::Backend { refusal, packet } => MuxError::Backend {
        refusal,
        packet: f(packet),
      },
      Self::Fault(fault) => MuxError::Fault(fault),
    }
  }
}

/// An opened container session that writes packets.
///
/// # Interleaving
///
/// Packets may arrive with their tracks interleaved however the caller
/// produces them; putting them in the order the container stores them
/// is the muxer's job. Within one track, decode timestamps may not go
/// back ([`check_decode_order`], [`MuxRefusal::NonMonotonicDts`]); two
/// at one instant are admitted on this face, and refused only by a
/// backend whose container cannot store them — by its own name, in
/// [`MuxError::Backend`], with the packet handed back.
///
/// # Attachments
///
/// A container writes its attachments — fonts, cover art, any file it
/// carries whole — with its header, so a muxer is given them when it is
/// opened, in its track table: an attachment track's
/// [`TrackDescription`](mediaspine::track::TrackDescription) carries its
/// one file. No method writes one as a packet, and
/// [`write_packet`](Self::write_packet) refuses a
/// [`DemuxedPacket::Attachment`] by name ([`MuxRefusal::Attachment`]).
///
/// A demuxer hands its attachments out first — the `Demuxer` face in
/// `mediadecode` promises each attachment track's one packet before any
/// timed packet — so a remux collects them all before it opens the
/// muxer. `mediadecode-ffmpeg`'s demuxer keeps that promise by
/// construction: it queues every attachment at open, and each pull
/// drains the queue before it reads the container, after a seek as
/// well.
///
/// # The refusals
///
/// A packet for a track the muxer does not have, of the wrong kind for
/// its track, without the timestamp its track is ordered by, decoded
/// before the last one on its track, presented before it is decoded, or
/// an attachment offered as a packet is refused by name — [`MuxRefusal`],
/// inside [`MuxError::Refused`] — and nothing is written. A packet that
/// breaks a rule of the backend's container is refused by the backend's
/// own name — [`Self::Refusal`], inside [`MuxError::Backend`] — and
/// nothing is written either. Every refusal hands the packet back
/// untouched, so the caller still owns it.
pub trait Muxer {
  /// Backend-specific vocabulary — the container tier's, as the
  /// `Demuxer`'s is.
  type Adapter: DemuxAdapter;
  /// Buffer type held by the packets this session takes.
  type Buffer: AsRef<[u8]>;
  /// The backend's own refusals: rules its container keeps that the
  /// portable face ([`MuxRefusal`]) does not, each refused by name in
  /// [`MuxError::Backend`] with the packet handed back. Open on purpose —
  /// what a container demands is the backend's to name. A backend whose
  /// containers keep no rule of their own names
  /// [`core::convert::Infallible`].
  type Refusal;
  /// Muxer-specific fault type.
  type Error;

  /// Writes one video packet to the track it names.
  fn write_video(
    &mut self,
    packet: VideoTrackPacket<Self::Adapter, Self::Buffer>,
  ) -> Result<(), MuxError<Self::Error, Self::Refusal, VideoTrackPacket<Self::Adapter, Self::Buffer>>>;

  /// Writes one audio packet to the track it names.
  fn write_audio(
    &mut self,
    packet: AudioTrackPacket<Self::Adapter, Self::Buffer>,
  ) -> Result<(), MuxError<Self::Error, Self::Refusal, AudioTrackPacket<Self::Adapter, Self::Buffer>>>;

  /// Writes one subtitle packet to the track it names.
  fn write_subtitle(
    &mut self,
    packet: SubtitleTrackPacket<Self::Adapter, Self::Buffer>,
  ) -> Result<
    (),
    MuxError<Self::Error, Self::Refusal, SubtitleTrackPacket<Self::Adapter, Self::Buffer>>,
  >;

  /// Writes one timed-data packet to the track it names.
  fn write_data(
    &mut self,
    packet: DataTrackPacket<Self::Adapter, Self::Buffer>,
  ) -> Result<(), MuxError<Self::Error, Self::Refusal, DataTrackPacket<Self::Adapter, Self::Buffer>>>;

  /// Writes one demuxed packet through the write for its kind — the
  /// router a remux's write loop calls.
  ///
  /// A refused packet comes back in the envelope it arrived in, and a
  /// [`DemuxedPacket::Attachment`] is refused by name
  /// ([`MuxRefusal::Attachment`]) and handed back: a muxer is given its
  /// attachments in its track table, when it is opened.
  fn write_packet(
    &mut self,
    packet: DemuxedPacket<Self::Adapter, Self::Buffer>,
  ) -> Result<(), MuxError<Self::Error, Self::Refusal, DemuxedPacket<Self::Adapter, Self::Buffer>>>
  {
    match packet {
      DemuxedPacket::Video(p) => self
        .write_video(p)
        .map_err(|e| e.map_packet(DemuxedPacket::Video)),
      DemuxedPacket::Audio(p) => self
        .write_audio(p)
        .map_err(|e| e.map_packet(DemuxedPacket::Audio)),
      DemuxedPacket::Subtitle(p) => self
        .write_subtitle(p)
        .map_err(|e| e.map_packet(DemuxedPacket::Subtitle)),
      DemuxedPacket::Data(p) => self
        .write_data(p)
        .map_err(|e| e.map_packet(DemuxedPacket::Data)),
      DemuxedPacket::Attachment(p) => Err(MuxError::Refused {
        refusal: MuxRefusal::Attachment { track: p.track() },
        packet: DemuxedPacket::Attachment(p),
      }),
    }
  }

  /// Completes the container — the packets still held for
  /// interleaving, then whatever the format writes last — and ends the
  /// session. It takes no packet and refuses nothing; an error is the
  /// backend's own fault.
  fn finish(self) -> Result<(), Self::Error>;
}

#[cfg(test)]
mod tests {
  use core::num::NonZeroI32;

  use mediaspine::{
    Timebase,
    adapter::{AudioAdapter, SubtitleAdapter, VideoAdapter},
    demuxer::{AttachmentPacket, AttachmentTrackPacket, DataPacket, DemuxedPacket},
    packet::{AudioPacket, PacketFlags, SubtitlePacket, VideoPacket},
  };

  use super::*;

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

  type Bytes = &'static [u8];

  #[derive(Debug, PartialEq, Eq)]
  struct Fault;

  /// The one rule of the strict containers the laws stand in for: decode
  /// times strictly increasing within a track, named by the backend.
  #[derive(Debug, PartialEq, Eq, thiserror::Error)]
  enum StrictRefusal {
    #[error("track {} already holds a packet at decode time {}", .track.get(), .dts.pts())]
    RepeatedDts { track: TrackIndex, dts: Timestamp },
  }

  fn ms(pts: i64) -> Timestamp {
    Timestamp::new(
      pts,
      Timebase::new(1, NonZeroI32::new(1000).expect("non-zero")),
    )
  }

  /// A six-track muxer (video, audio, two attachments whose files it
  /// was opened with, then subtitles and timed data) that checks what
  /// every muxer checks and writes nothing anywhere — and, when `strict`,
  /// keeps the rule of a container that needs decode times strictly
  /// increasing, refusing an equal one by its own name.
  struct Table {
    kinds: [TrackKind; 6],
    last: [Option<Timestamp>; 6],
    strict: bool,
    written: u32,
  }

  impl Table {
    fn new() -> Self {
      Self {
        kinds: [
          TrackKind::Video,
          TrackKind::Audio,
          TrackKind::Attachment,
          TrackKind::Attachment,
          TrackKind::Subtitle,
          TrackKind::Data,
        ],
        last: [None; 6],
        strict: false,
        written: 0,
      }
    }

    /// The same table over a strict container.
    fn strict() -> Self {
      Self {
        strict: true,
        ..Self::new()
      }
    }

    /// That `track` is in the table and carries `offered` packets.
    fn kind_of(&self, track: TrackIndex, offered: TrackKind) -> Result<(), MuxRefusal> {
      let expected = *self
        .kinds
        .get(track.get())
        .ok_or(MuxRefusal::UnknownTrack { track })?;
      if expected != offered {
        return Err(MuxRefusal::KindMismatch {
          track,
          expected,
          offered,
        });
      }
      Ok(())
    }

    /// What every timed write checks, from the packet's track, kind and
    /// timestamps alone — the packet itself is not touched — answering
    /// the decode timestamp to record.
    fn check(
      &self,
      track: TrackIndex,
      offered: TrackKind,
      (pts, dts): (Option<Timestamp>, Option<Timestamp>),
    ) -> Result<Timestamp, MuxRefusal> {
      self.kind_of(track, offered)?;
      let dts = dts.ok_or(MuxRefusal::MissingDts { track })?;
      check_decode_order(track, self.last[track.get()], dts)?;
      check_presentation_order(track, pts, dts)?;
      Ok(dts)
    }

    /// Writes `packet`, or hands it back with the refusal — the
    /// portable one, or the strict container's own after it.
    fn admit<P>(
      &mut self,
      track: TrackIndex,
      offered: TrackKind,
      times: (Option<Timestamp>, Option<Timestamp>),
      packet: P,
    ) -> Result<(), MuxError<Fault, StrictRefusal, P>> {
      let dts = match self.check(track, offered, times) {
        Ok(dts) => dts,
        Err(refusal) => return Err(MuxError::Refused { refusal, packet }),
      };
      if self.strict && self.last[track.get()] == Some(dts) {
        return Err(MuxError::Backend {
          refusal: StrictRefusal::RepeatedDts { track, dts },
          packet,
        });
      }
      self.last[track.get()] = Some(dts);
      self.written += 1;
      Ok(())
    }
  }

  type VTrack = VideoTrackPacket<Loopback, Bytes>;
  type ATrack = AudioTrackPacket<Loopback, Bytes>;
  type STrack = SubtitleTrackPacket<Loopback, Bytes>;
  type DTrack = DataTrackPacket<Loopback, Bytes>;
  type AtTrack = AttachmentTrackPacket<Loopback, Bytes>;
  type Demuxed = DemuxedPacket<Loopback, Bytes>;

  /// A write's answer in the table's vocabulary.
  type Written<P> = Result<(), MuxError<Fault, StrictRefusal, P>>;

  impl Muxer for Table {
    type Adapter = Loopback;
    type Buffer = Bytes;
    type Refusal = StrictRefusal;
    type Error = Fault;

    fn write_video(&mut self, p: VTrack) -> Written<VTrack> {
      let (track, times) = (p.track(), (p.packet().pts(), p.packet().dts()));
      self.admit(track, TrackKind::Video, times, p)
    }

    fn write_audio(&mut self, p: ATrack) -> Written<ATrack> {
      let (track, times) = (p.track(), (p.packet().pts(), p.packet().dts()));
      self.admit(track, TrackKind::Audio, times, p)
    }

    // A subtitle or data packet is decoded when it is presented.
    fn write_subtitle(&mut self, p: STrack) -> Written<STrack> {
      let (track, pts) = (p.track(), p.packet().pts());
      self.admit(track, TrackKind::Subtitle, (pts, pts), p)
    }

    fn write_data(&mut self, p: DTrack) -> Written<DTrack> {
      let (track, pts) = (p.track(), p.packet().pts());
      self.admit(track, TrackKind::Data, (pts, pts), p)
    }

    fn finish(self) -> Result<(), Fault> {
      Ok(())
    }
  }

  /// Every packet the laws offer carries these bytes, so a handed-back
  /// packet can be checked to hold the very slice it was offered with.
  const PAYLOAD: &[u8] = b"payload";

  fn video(track: usize, dts: Option<i64>) -> VTrack {
    VideoTrackPacket::new(
      TrackIndex::new(track),
      VideoPacket::new(PAYLOAD, ())
        .with_dts(dts.map(ms))
        .with_pts(dts.map(ms))
        .with_flags(PacketFlags::KEY),
    )
  }

  fn audio(track: usize, dts: i64) -> ATrack {
    AudioTrackPacket::new(
      TrackIndex::new(track),
      AudioPacket::new(PAYLOAD, ()).with_dts(Some(ms(dts))),
    )
  }

  fn subtitle(track: usize, pts: i64) -> STrack {
    SubtitleTrackPacket::new(
      TrackIndex::new(track),
      SubtitlePacket::new(PAYLOAD, ()).with_pts(Some(ms(pts))),
    )
  }

  fn data(track: usize, pts: i64) -> DTrack {
    DataTrackPacket::new(
      TrackIndex::new(track),
      DataPacket::new(PAYLOAD, ()).with_pts(Some(ms(pts))),
    )
  }

  fn attachment(track: usize) -> AtTrack {
    AttachmentTrackPacket::new(
      TrackIndex::new(track),
      AttachmentPacket::new(PAYLOAD, ()).with_flags(PacketFlags::KEY),
    )
  }

  /// Asserts that `back` is the packet `kept` was cloned from before it
  /// was offered: the same track, the very bytes it was offered with,
  /// and the same timestamps and flags.
  macro_rules! assert_handed_back {
    ($back:expr, $kept:expr; $($field:ident),*) => {{
      let (back, kept) = (&$back, &$kept);
      assert_eq!(back.track(), kept.track(), "the same track");
      let (b, k) = (back.packet(), kept.packet());
      assert!(
        core::ptr::eq(*b.data(), *k.data()),
        "the very bytes it was offered with"
      );
      $(assert_eq!(b.$field(), k.$field(), stringify!($field));)*
    }};
  }

  /// The portable refusal and the handed-back packet, or a panic naming
  /// what came instead.
  fn refused<P>(result: Written<P>) -> (MuxRefusal, P) {
    match result {
      Err(MuxError::Refused { refusal, packet }) => (refusal, packet),
      Err(MuxError::Backend { refusal, .. }) => {
        panic!("expected a portable refusal, got the backend's {refusal:?}")
      }
      Err(MuxError::Fault(fault)) => panic!("expected a refusal, got the fault {fault:?}"),
      Ok(()) => panic!("expected a refusal, the packet was written"),
    }
  }

  /// The backend's refusal and the handed-back packet, or a panic naming
  /// what came instead.
  fn refused_by_the_backend<P>(result: Written<P>) -> (StrictRefusal, P) {
    match result {
      Err(MuxError::Backend { refusal, packet }) => (refusal, packet),
      Err(MuxError::Refused { refusal, .. }) => {
        panic!("expected the backend's refusal, got the portable {refusal:?}")
      }
      Err(MuxError::Fault(fault)) => {
        panic!("expected the backend's refusal with the packet, got the fault {fault:?}")
      }
      Ok(()) => panic!("expected a refusal, the packet was written"),
    }
  }

  #[test]
  fn a_packet_for_a_track_the_muxer_lacks_is_refused_by_name() {
    let mut muxer = Table::new();
    let offered = video(8, Some(0));
    let kept = offered.clone();
    let (refusal, back) = refused(muxer.write_video(offered));
    assert!(matches!(refusal, MuxRefusal::UnknownTrack { track } if track.get() == 8));
    assert_handed_back!(back, kept; pts, dts, duration, flags);
    assert_eq!(muxer.written, 0, "nothing is written for a refused packet");
  }

  #[test]
  fn a_packet_of_the_wrong_kind_is_refused_by_name() {
    let mut muxer = Table::new();
    let offered = audio(0, 0);
    let kept = offered.clone();
    let (refusal, back) = refused(muxer.write_audio(offered));
    assert!(matches!(
      refusal,
      MuxRefusal::KindMismatch {
        expected: TrackKind::Video,
        offered: TrackKind::Audio,
        ..
      }
    ));
    assert_handed_back!(back, kept; pts, dts, duration, flags);
  }

  #[test]
  fn a_packet_without_a_decode_timestamp_is_refused_by_name() {
    let mut muxer = Table::new();
    let offered = video(0, None);
    let kept = offered.clone();
    let (refusal, back) = refused(muxer.write_video(offered));
    assert!(matches!(refusal, MuxRefusal::MissingDts { .. }));
    assert_handed_back!(back, kept; pts, dts, duration, flags);
  }

  /// **Within a track, decode timestamps may not go back; across tracks
  /// the packets interleave freely.** An audio packet stamped before the
  /// last video packet is written, and so is a video packet at the same
  /// decode time as the last; one stamped earlier is refused, naming both
  /// timestamps, and handed back.
  #[test]
  fn an_earlier_decode_timestamp_is_refused_by_name_and_an_equal_one_admitted() {
    let mut muxer = Table::new();
    muxer
      .write_video(video(0, Some(40)))
      .expect("first video packet");
    muxer
      .write_audio(audio(1, 0))
      .expect("another track, interleaved");
    muxer
      .write_video(video(0, Some(40)))
      .expect("the same decode time is admitted on the face");
    let offered = video(0, Some(30));
    let kept = offered.clone();
    let (refusal, back) = refused(muxer.write_video(offered));
    match refusal {
      MuxRefusal::NonMonotonicDts {
        track,
        last,
        offered,
      } => {
        assert_eq!(track.get(), 0);
        assert_eq!((last.pts(), offered.pts()), (40, 30));
      }
      other => panic!("expected the decode-order refusal, got {other:?}"),
    }
    assert_handed_back!(back, kept; pts, dts, duration, flags);
    // The handed-back packet is the caller's again: restamped, it goes.
    let (track, packet) = back.into_parts();
    muxer
      .write_video(VideoTrackPacket::new(
        track,
        packet.with_dts(Some(ms(80))).with_pts(Some(ms(80))),
      ))
      .expect("the same packet, advancing");
    assert_eq!(muxer.written, 4);
    muxer.finish().expect("the session ends cleanly");
  }

  /// **Two cues, and two data events, at one instant are ordinary
  /// input.** A subtitle track whose cues share a start time and a data
  /// track whose events share a timestamp are written whole on the face;
  /// a muxer that demanded strictly increasing decode times refused the
  /// second of each, and an unchanged remux failed on valid input.
  #[test]
  fn two_cues_and_two_events_at_one_instant_are_written() {
    let mut muxer = Table::new();
    muxer
      .write_subtitle(subtitle(4, 10))
      .expect("the first cue");
    muxer
      .write_subtitle(subtitle(4, 10))
      .expect("a second cue at the same instant");
    muxer.write_data(data(5, 10)).expect("the first event");
    muxer
      .write_data(data(5, 10))
      .expect("a second event at the same instant");
    assert_eq!(muxer.written, 4);
    // And still never back.
    let (refusal, _) = refused(muxer.write_subtitle(subtitle(4, 9)));
    assert!(matches!(refusal, MuxRefusal::NonMonotonicDts { track, .. } if track.get() == 4));
  }

  /// **Every write hands a refused packet back untouched** — the
  /// subtitle and data writes as well as the two above, here refused for
  /// the kind of the track they name.
  #[test]
  fn every_write_hands_a_refused_packet_back_untouched() {
    let mut muxer = Table::new();

    let offered = subtitle(0, 10);
    let kept = offered.clone();
    let (refusal, back) = refused(muxer.write_subtitle(offered));
    assert!(matches!(
      refusal,
      MuxRefusal::KindMismatch {
        offered: TrackKind::Subtitle,
        ..
      }
    ));
    assert_handed_back!(back, kept; pts, duration, flags);

    let offered = data(1, 10);
    let kept = offered.clone();
    let (refusal, back) = refused(muxer.write_data(offered));
    assert!(matches!(
      refusal,
      MuxRefusal::KindMismatch {
        offered: TrackKind::Data,
        ..
      }
    ));
    assert_handed_back!(back, kept; pts, duration, flags);
    assert_eq!(muxer.written, 0);
  }

  /// **A remux gets a refused packet back in the envelope it came in.**
  /// [`MuxError::map_packet`] carries the refusal over unchanged and
  /// wraps the packet, as the module's remux example does.
  #[test]
  fn map_packet_puts_a_refused_packet_back_in_its_envelope() {
    let mut muxer = Table::new();
    let offered = video(7, Some(0));
    let kept = offered.clone();
    let routed: Written<Demuxed> = muxer
      .write_video(offered)
      .map_err(|e| e.map_packet(DemuxedPacket::Video));
    let (refusal, back) = refused(routed);
    assert!(matches!(refusal, MuxRefusal::UnknownTrack { track } if track.get() == 7));
    match back {
      DemuxedPacket::Video(back) => {
        assert_handed_back!(back, kept; pts, dts, duration, flags);
      }
      other => panic!("expected the video envelope, got {other:?}"),
    }
  }

  /// **`write_packet` routes each kind to its write, and refuses an
  /// attachment by name.** A muxer is given its attachments in its track
  /// table; one offered as a packet comes back untouched, in its
  /// envelope, as does a timed packet a write refused.
  #[test]
  fn write_packet_routes_each_kind_and_refuses_an_attachment_by_name() {
    let mut muxer = Table::new();
    muxer
      .write_packet(DemuxedPacket::Video(video(0, Some(0))))
      .expect("a video packet goes to the video write");
    muxer
      .write_packet(DemuxedPacket::Audio(audio(1, 0)))
      .expect("an audio packet goes to the audio write");
    assert_eq!(muxer.written, 2);

    let offered = attachment(2);
    let kept = offered.clone();
    let (refusal, back) =
      refused::<Demuxed>(muxer.write_packet(DemuxedPacket::Attachment(offered)));
    assert!(matches!(refusal, MuxRefusal::Attachment { track } if track.get() == 2));
    match back {
      DemuxedPacket::Attachment(back) => assert_handed_back!(back, kept; flags),
      other => panic!("expected the attachment envelope, got {other:?}"),
    }

    let offered = video(9, Some(10));
    let kept = offered.clone();
    let (refusal, back) = refused::<Demuxed>(muxer.write_packet(DemuxedPacket::Video(offered)));
    assert!(matches!(refusal, MuxRefusal::UnknownTrack { track } if track.get() == 9));
    match back {
      DemuxedPacket::Video(back) => assert_handed_back!(back, kept; pts, dts, duration, flags),
      other => panic!("expected the video envelope, got {other:?}"),
    }
    assert_eq!(muxer.written, 2, "nothing refused was written");
  }

  /// **A backend stricter than the face refuses by its own name, through
  /// `write_packet`, and hands the packet back untouched.** Over a strict
  /// container the second video packet at one decode time is refused as
  /// the backend's `RepeatedDts` — not as a fault, which would have taken
  /// the packet — and comes back in its envelope; restamped later, the
  /// same packet is written. The portable rule still answers first: an
  /// earlier decode time is the face's refusal, not the backend's.
  #[test]
  fn a_strict_backend_refuses_an_equal_decode_time_by_name_and_hands_the_packet_back() {
    let mut muxer = Table::strict();
    muxer
      .write_packet(DemuxedPacket::Video(video(0, Some(40))))
      .expect("the first packet");
    let offered = video(0, Some(40));
    let kept = offered.clone();
    let (refusal, back) =
      refused_by_the_backend::<Demuxed>(muxer.write_packet(DemuxedPacket::Video(offered)));
    match refusal {
      StrictRefusal::RepeatedDts { track, dts } => {
        assert_eq!((track.get(), dts.pts()), (0, 40));
      }
    }
    let back = match back {
      DemuxedPacket::Video(back) => back,
      other => panic!("expected the video envelope, got {other:?}"),
    };
    assert_handed_back!(back, kept; pts, dts, duration, flags);
    assert_eq!(muxer.written, 1, "nothing refused was written");

    // The handed-back packet is the caller's again: restamped, it goes.
    let (track, packet) = back.into_parts();
    muxer
      .write_packet(DemuxedPacket::Video(VideoTrackPacket::new(
        track,
        packet.with_dts(Some(ms(80))).with_pts(Some(ms(80))),
      )))
      .expect("the same packet, advancing");
    assert_eq!(muxer.written, 2);

    // An earlier decode time is the portable rule's, answered first.
    let (refusal, _) =
      refused::<Demuxed>(muxer.write_packet(DemuxedPacket::Video(video(0, Some(10)))));
    assert!(matches!(refusal, MuxRefusal::NonMonotonicDts { .. }));
  }

  /// **`map_packet` carries the backend's refusal over as it carries the
  /// portable one**: the refusal unchanged, the packet in its new
  /// envelope.
  #[test]
  fn map_packet_keeps_the_backends_refusal_and_its_packet() {
    let offered = subtitle(4, 10);
    let kept = offered.clone();
    let error: MuxError<Fault, StrictRefusal, STrack> = MuxError::Backend {
      refusal: StrictRefusal::RepeatedDts {
        track: TrackIndex::new(4),
        dts: ms(10),
      },
      packet: offered,
    };
    match error.map_packet(DemuxedPacket::Subtitle) {
      MuxError::Backend {
        refusal: StrictRefusal::RepeatedDts { track, dts },
        packet: DemuxedPacket::Subtitle(back),
      } => {
        assert_eq!((track.get(), dts.pts()), (4, 10));
        assert_handed_back!(back, kept; pts, duration, flags);
      }
      other => panic!("expected the backend's refusal in the subtitle envelope, got {other:?}"),
    }
  }

  /// **A packet presented before it is decoded is refused by name, and
  /// handed back.** Presented at its decode time, or with no
  /// presentation time at all, it is written.
  #[test]
  fn a_presentation_before_its_decode_time_is_refused_by_name() {
    let mut muxer = Table::new();
    let offered = VideoTrackPacket::new(
      TrackIndex::new(0),
      VideoPacket::new(PAYLOAD, ())
        .with_dts(Some(ms(40)))
        .with_pts(Some(ms(30))),
    );
    let kept = offered.clone();
    let (refusal, back) = refused(muxer.write_video(offered));
    match refusal {
      MuxRefusal::PtsBeforeDts { track, pts, dts } => {
        assert_eq!((track.get(), pts.pts(), dts.pts()), (0, 30, 40));
      }
      other => panic!("expected the presentation-order refusal, got {other:?}"),
    }
    assert_handed_back!(back, kept; pts, dts, duration, flags);
    assert_eq!(muxer.written, 0, "nothing refused was written");

    muxer
      .write_video(video(0, Some(40)))
      .expect("presented when decoded");
    muxer
      .write_video(VideoTrackPacket::new(
        TrackIndex::new(0),
        VideoPacket::new(PAYLOAD, ()).with_dts(Some(ms(80))),
      ))
      .expect("no presentation time: nothing to judge");
    assert_eq!(muxer.written, 2);
  }
}
