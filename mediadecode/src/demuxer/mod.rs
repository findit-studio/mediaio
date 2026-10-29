//! The demux tier — the track table, the five-arm packet envelope, and
//! the [`Demuxer`] session face.
//!
//! A container file is a bundle of *tracks*; reading it produces a
//! single interleaved stream of *packets*, each belonging to one track.
//! This module names both halves: [`TrackInfo`] describes a track,
//! [`DemuxedPacket`] delivers one packet with its track coordinate
//! attached, and [`Demuxer`] is the pull session that hands them out.
//! A container may also carry a table of contents over the whole
//! timeline rather than over any one track — [`Chapter`], read off
//! [`Demuxer::chapters`].
//!
//! # The session is pull-style
//!
//! The caller owns the loop. [`Demuxer::next_packet`] returns the next
//! packet in interleaved file order and `Ok(None)` at end of file. One
//! packet is in hand at a time, whatever the file's length, and the
//! caller pulls only when it is ready to consume — backpressure with no
//! machinery. This is the same rhythm the decoder faces already keep
//! (the caller schedules; see [`crate::decoder`]).
//!
//! # Construction is not on the trait
//!
//! [`Demuxer`] covers the **opened session** only: [`tracks`],
//! [`next_packet`], [`seek`]. Opening is each backend's own business
//! and each backend's is different — FFmpeg opens from a path or from a
//! `Read + Seek` reader, a WebAssembly container parser opens from a
//! byte slice. Putting a constructor on the trait would force one of
//! those spellings onto all of them, which is exactly what the decoder
//! traits already decline to do.
//!
//! # Not every backend is a demuxer
//!
//! R3D and BRAW are clip-oriented SDKs: they expose frames by index,
//! never packets, so there is nothing for them to demux. They
//! deliberately do **not** implement this trait and join a pipeline one
//! tier up, through [`crate::decoder::VideoFrameSource`] and
//! [`crate::decoder::AudioFrameSource`]. A graph that wants both shapes
//! unifies them at the *frame* tier, not here.
//!
//! # Where the types live
//!
//! Everything in this module but [`Demuxer`] itself is the shared
//! vocabulary's — `mediaspine::demuxer`, re-exported here whole, so
//! every path through this module still names it. A muxer speaks the
//! same types; the session face is this crate's.
//!
//! [`tracks`]: Demuxer::tracks
//! [`next_packet`]: Demuxer::next_packet
//! [`seek`]: Demuxer::seek

use core::ops::Deref;

pub use mediaspine::demuxer::*;

use crate::Timestamp;

// The session face owns no allocation either: what a track table costs
// is the backend's choice, made at `Demuxer::TrackHandle`. `alloc` is
// bound here for the test mock alone, whose table is a `Vec`, and is
// scoped to this module so a reader can see exactly what needs the
// heap and when.
#[cfg(all(test, any(feature = "std", feature = "alloc")))]
extern crate alloc;

// ---------------------------------------------------------------------------
//  The session face.
// ---------------------------------------------------------------------------

/// An opened container session: the track table, a pull loop, and a seek.
///
/// # Delivery order
///
/// [`next_packet`](Self::next_packet) returns packets in **interleaved
/// file order** — the order the container stores them in, tracks mixed
/// together exactly as written. `Ok(None)` means end of file, and once
/// it is returned it stays returned until a [`seek`](Self::seek) moves
/// the session somewhere else.
///
/// # Why `Option` and not [`Received`](crate::Received)
///
/// The decoders' three-state answer has a state this face does not:
/// **needs-input cannot happen to a demuxer.** A demuxer is pulled, not
/// fed — there is no caller-supplied input it could be waiting on, so a
/// `NeedsInput` arm here would be a state no backend can ever produce
/// and every consumer would still have to write a `match` arm for.
/// `Ok(None)` is the same fact in the shape that fits: two states, two
/// values, and the packet itself riding in the `Ok` arm rather than
/// into a `dst`.
///
/// What the two faces share is the law, not the type: **a protocol
/// state never travels in `Err`.** End of file is not a failure here
/// for the same reason [`Received::Ended`](crate::Received::Ended) is
/// not one there.
///
/// # The attachment contract
///
/// An [`Attachment`](TrackKind::Attachment) track delivers **exactly
/// one packet, before any timed packet**. Attachments are not on the
/// timeline, so a consumer must be able to collect them all before it
/// starts consuming time — a subtitle renderer needs its fonts before
/// the first cue, and a thumbnailer wants the cover art without reading
/// the file to its end. Backends satisfy this by synthesising the
/// packet when the container keeps the payload outside the packet
/// stream (fonts, whose bytes live in the track's codec extradata) and
/// by hoisting the natural one when it exists (cover art, which is a
/// real packet the container stores).
///
/// A track's identity — the filename it was attached under, its MIME
/// type — is on [`TrackInfo`], not repeated on every packet.
///
/// # Seeking
///
/// [`seek`](Self::seek) obeys three laws:
///
/// 1. **It flushes session state.** Anything buffered from before the
///    seek is discarded; the next [`next_packet`](Self::next_packet)
///    reads from the new position.
/// 2. **It lands on the nearest keyframe at or before the target.**
///    Never after. A decoder fed from a landing point past the target
///    would have no reference frame, so "at or before" is a
///    correctness requirement, not a preference — the caller discards
///    the packets between the landing point and the target itself.
/// 3. **Attachments are never replayed.** An attachment already
///    delivered is not delivered again, however many times the session
///    seeks. One attachment track yields one packet for the life of the
///    session — a seek moves the *timeline*, and attachments are not on
///    it.
///
/// # The track table
///
/// [`tracks`](Self::tracks) is a **non-destructive** read, and a
/// session holds its table for its whole life: the rows a caller reads
/// before the first pull are the same rows the session classifies
/// every packet against, and they are still there after end of file.
/// Reading the table therefore has no ordering rule at all — before
/// the first packet, between two of them, after EOF, twice, never.
///
/// The table is *shared*, not handed over. `TrackInfo` has no `Clone`
/// (see its own doc for the message-carrier law), so a caller that
/// needs a row beyond a borrow of `&self` — to fan it out, or simply
/// to hold it across the `&mut self` of the pull loop — clones a
/// [`TrackHandle`](Self::TrackHandle) instead.
///
/// This face used to carry a `take_tracks` that *moved* the table out
/// of the session. It is gone, root and branch: a demuxer that has
/// given its table away can no longer say which track a packet
/// belongs to, and the one backend that implemented it classified
/// against the very `Vec` the move emptied — so following the
/// documented order made every packet in a healthy file vanish. A
/// read that costs the session the state it runs on is not a door
/// worth having.
///
/// # The chapter table
///
/// [`chapters`](Self::chapters) is the container's own table of
/// contents, read the same way and under the same rules: held for the
/// session's whole life, non-destructive, no ordering rule. It is
/// **provided**, answering an empty slice — most files declare no
/// chapters, and a backend for a format that cannot carry them has
/// nothing to override.
///
/// Rows come back by borrow rather than through a handle type. A
/// [`Chapter`] is small and `Clone`s where the backend's text carrier
/// does, so a consumer that needs one past a borrow of the session
/// copies it; there is nothing here expensive enough to be worth a
/// carrier choice of its own, which is the whole reason
/// [`TrackHandle`](Self::TrackHandle) exists on the other table.
///
/// # Durations
///
/// The figures that bear on a file's duration come in layers, and each
/// layer is answered by its own method so that none is mistaken for
/// another. Two are lengths — the container-level figure and a track's
/// — and one is an endpoint, the walk's measured end:
///
/// - [`duration`](Self::duration) is the **container-level** figure the
///   library reports for the file as a whole — a *length* — with the
///   library's own account of how it came by it ([`DurationSource`]).
///   That account names what the library did — read timestamps, took a
///   stream's duration, divided a size by a bitrate — and not what the
///   file declares.
/// - [`TrackInfo::duration`] is the library's figure for **one track**
///   as it stood when the container was opened, before probing. It is
///   no claim about the file either, and a track the library reported
///   none for stays `None`: the container's figure is never copied onto
///   it.
/// - [`measured_end`](Self::measured_end) is what the **walk**
///   measured: where a track's packets ended — an *endpoint* on that
///   track's own timeline. It rides the pulls the caller already makes
///   and carries two qualifications of its own: whether the walk
///   covered the file, and whether every packet's end was known.
///
/// The first and third are different quantities, a length and an
/// endpoint. This tier does not rank one against the other and does not
/// turn either into the other: making a length out of an end means
/// subtracting the track's start and honouring presentation edits, and
/// that is the composer's job.
///
/// # What is not here
///
/// Opening. See the [module docs](self#construction-is-not-on-the-trait).
pub trait Demuxer {
  /// Backend-specific vocabulary.
  type Adapter: DemuxAdapter;
  /// Buffer type held by the packets this session produces.
  type Buffer: AsRef<[u8]>;
  /// A shareable handle on one row of the track table.
  ///
  /// The row is read *through* the handle, and a consumer that needs
  /// to keep a row past a borrow of the session clones the handle.
  /// The backend picks the carrier, the same way it picks
  /// [`Buffer`](Self::Buffer): a heap-backed, thread-crossing backend
  /// binds `Arc<TrackInfo<..>>`, a single-threaded one binds `Rc`,
  /// and one whose rows live in memory it already borrows binds
  /// `&TrackInfo<..>` — which is what keeps this whole tier
  /// allocator-free.
  ///
  /// **`Clone` on a handle must be a refcount bump or a copied
  /// borrow, never a deep copy of the row** — the message-carrier law
  /// [`TrackInfo`] states. An implementor is what upholds it; the
  /// bound is what makes upholding it the path of least resistance,
  /// since `TrackInfo` is not `Clone`, `Box<TrackInfo<_>>` therefore
  /// is not either, and no `#[derive(Clone)]` reaches a carrier that
  /// owns a row outright. A deep copy here would have to be
  /// hand-written against the law.
  type TrackHandle: Clone + Deref<Target = TrackInfo<Self::Adapter>>;
  /// Demuxer-specific error type.
  type Error;

  /// Returns the container's track table.
  ///
  /// Position `i` describes [`TrackIndex::new(i)`](TrackIndex::new) —
  /// the coordinate every [`DemuxedPacket`] carries.
  ///
  /// Non-destructive, and callable whenever: see [the track
  /// table](Self#the-track-table) on the trait.
  fn tracks(&self) -> &[Self::TrackHandle];

  /// Returns the container's chapter table, **in table order**.
  ///
  /// A container that declares none answers an empty slice — which is
  /// what the provided body does, so a backend whose format has no
  /// chapters, and every implementor written before this method existed,
  /// answers it correctly without writing a line.
  ///
  /// Non-destructive and callable whenever, exactly like
  /// [`tracks`](Self::tracks): see [the chapter
  /// table](Self#the-chapter-table) on the trait.
  #[cfg_attr(not(tarpaulin), inline(always))]
  fn chapters(&self) -> &[Chapter<Self::Adapter>] {
    &[]
  }

  /// Returns the container-level duration the library reports for the
  /// file as a whole, with the library's account of how it came by it,
  /// or `None` where it reports none.
  ///
  /// [`ContainerDuration::source`] is that account in the library's own
  /// words ([`DurationSource`]). It says what the library did, not what
  /// the file declares. It is a *length*: [`measured_end`] answers a
  /// different quantity, an endpoint on a track's timeline, and the two
  /// are not interchangeable. See [durations](Self#durations) on the
  /// trait.
  ///
  /// [`measured_end`]: Self::measured_end
  ///
  /// It is not any track's duration: a track the library reported none
  /// for stays `None`, and this figure is never copied onto it.
  ///
  /// Fixed for the life of the session and callable whenever, like the
  /// tables. Provided: a backend with no such figure answers `None`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  fn duration(&self) -> Option<ContainerDuration> {
    None
  }

  /// Returns where `track`'s packets ended, as the walk this session
  /// has performed measured it, or `None` where the session has read no
  /// packet on `track` that carries a timestamp.
  ///
  /// A packet's end is its timestamp plus its duration, and its
  /// timestamp alone where it carries no duration; the figure is the
  /// greatest end over the timed packets read, whether or not a packet
  /// carries a payload. It rides [`next_packet`](Self::next_packet) —
  /// each timed packet read moves it — so nothing is read for it,
  /// nothing is sought, and there is no second pass.
  ///
  /// The figure is an **endpoint on the track's own timeline, not a
  /// length**; making a length of it is the composer's job (see
  /// [`MeasuredEnd`]).
  ///
  /// At any moment the answer is the figure *so far*, qualified twice:
  /// [`MeasuredEnd::walk_complete`] says when the walk covered the file
  /// and what that does not vouch for, and [`MeasuredEnd::exact`] says
  /// whether every packet's end was known or the figure is a lower
  /// bound. See [durations](Self#durations) on the trait.
  ///
  /// A track that is off the timeline — an attachment — a track outside
  /// the table, and a track whose end is too large to represent all
  /// answer `None`. Provided: a backend that does not measure answers
  /// `None` for every track.
  #[cfg_attr(not(tarpaulin), inline(always))]
  fn measured_end(&self, _track: TrackIndex) -> Option<MeasuredEnd> {
    None
  }

  /// Pulls the next packet in interleaved file order, or `Ok(None)` at
  /// end of file.
  fn next_packet(
    &mut self,
  ) -> Result<Option<DemuxedPacket<Self::Adapter, Self::Buffer>>, Self::Error>;

  /// Seeks to `target`, landing on the nearest keyframe at or before it.
  ///
  /// See the [three laws](Self#seeking) on the trait.
  fn seek(&mut self, target: Timestamp) -> Result<(), Self::Error>;
}

#[cfg(test)]
mod tests {
  use core::num::NonZeroI32;

  use super::*;
  #[cfg(any(feature = "std", feature = "alloc"))]
  use crate::packet::AudioPacket;
  use crate::{
    Timebase,
    adapter::{AudioAdapter, SubtitleAdapter, VideoAdapter},
    packet::SubtitlePacket,
  };

  // `Vec` is not in the prelude in the alloc-without-std tier (the
  // crate is `#![no_std]` there; the crate-root `alloc`-as-`std` alias
  // only makes `std::`-qualified paths resolve, it does not inject
  // prelude items). `Rc` needs the same bridge.
  // Unconditional whenever this arm runs: the enclosing `alloc`
  // binding above is gated the same way.
  #[cfg(any(feature = "std", feature = "alloc"))]
  use alloc::{rc::Rc, vec, vec::Vec};

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

  /// Trivial loopback session — proves the trait is implementable and
  /// that its associated types resolve through the adapter bundle.
  ///
  /// Binds `Rc` rather than `Arc` as the [`Demuxer::TrackHandle`] on
  /// purpose: the handle is a backend's choice, and a single-threaded
  /// session should not be made to pay for atomics. `Vec`-backed, so
  /// the mock and the tests that construct it are gated on the
  /// allocating tier — see the `Vec`/`format!` import note above
  /// `VLoop`. [`BorrowedDemuxer`], below, is the same face with no
  /// allocator at all.
  #[cfg(any(feature = "std", feature = "alloc"))]
  struct LoopDemuxer {
    tracks: Vec<Rc<TrackInfo<Loopback>>>,
    drained: bool,
  }

  #[derive(Debug)]
  struct LoopError;

  #[cfg(any(feature = "std", feature = "alloc"))]
  impl Demuxer for LoopDemuxer {
    type Adapter = Loopback;
    type Buffer = &'static [u8];
    type TrackHandle = Rc<TrackInfo<Loopback>>;
    type Error = LoopError;

    fn tracks(&self) -> &[Rc<TrackInfo<Loopback>>] {
      &self.tracks
    }

    fn next_packet(&mut self) -> Result<Option<DemuxedPacket<Loopback, &'static [u8]>>, LoopError> {
      if self.drained {
        return Ok(None);
      }
      self.drained = true;
      Ok(Some(DemuxedPacket::Audio(AudioTrackPacket::new(
        TrackIndex::new(0),
        AudioPacket::new(&[][..], ()),
      ))))
    }

    fn seek(&mut self, _target: Timestamp) -> Result<(), LoopError> {
      self.drained = false;
      Ok(())
    }
  }

  #[cfg(any(feature = "std", feature = "alloc"))]
  #[test]
  fn the_session_face_is_implementable_and_none_means_eof() {
    fn _accepts<D: Demuxer>() {}
    _accepts::<LoopDemuxer>();

    let mut d = LoopDemuxer {
      tracks: vec![Rc::new(TrackInfo::new(
        ms_tb(),
        TrackParams::Audio(AudioTrackParams::new(1, 48_000, 2, 0, 0)),
        (),
      ))],
      drained: false,
    };
    assert_eq!(d.tracks().len(), 1);
    assert_eq!(d.tracks()[0].kind(), TrackKind::Audio);
    assert!(matches!(
      d.next_packet().expect("pull"),
      Some(DemuxedPacket::Audio(_))
    ));
    assert!(d.next_packet().expect("pull").is_none());
    assert!(d.next_packet().expect("pull").is_none(), "EOF is sticky");
    d.seek(Timestamp::new(0, ms_tb())).expect("seek");
    assert!(d.next_packet().expect("pull").is_some());
  }

  /// **The chapter table is provided, and neither mock writes a line
  /// for it.** That is the additivity claim stated as a test: both
  /// implementations in this file predate the method, both still
  /// compile, and both answer the empty table a container without
  /// chapters has.
  #[cfg(any(feature = "std", feature = "alloc"))]
  #[test]
  fn a_demuxer_that_declares_no_chapters_answers_an_empty_table() {
    let d = LoopDemuxer {
      tracks: vec![Rc::new(TrackInfo::new(
        ms_tb(),
        TrackParams::Audio(AudioTrackParams::new(1, 48_000, 2, 0, 0)),
        (),
      ))],
      drained: false,
    };
    assert!(d.chapters().is_empty());
    assert_eq!(
      d.tracks().len(),
      1,
      "the chapter table is a different table, and reading it is not reading that one",
    );
  }

  /// **The container duration is provided, and neither mock writes a
  /// line for it.** A backend with no figure of its own answers `None`,
  /// and asking costs the session nothing: the packet it owes is still
  /// owed afterwards.
  #[cfg(any(feature = "std", feature = "alloc"))]
  #[test]
  fn a_demuxer_with_no_container_duration_answers_none() {
    let mut d = LoopDemuxer {
      tracks: vec![Rc::new(TrackInfo::new(
        ms_tb(),
        TrackParams::Audio(AudioTrackParams::new(1, 48_000, 2, 0, 0)),
        (),
      ))],
      drained: false,
    };
    assert_eq!(d.duration(), None);
    assert_eq!(d.tracks()[0].duration(), None, "a different question");
    assert!(
      d.next_packet().expect("pull").is_some(),
      "reading the duration is not a pull",
    );
  }

  /// **The measured end is provided too.** A backend that does not
  /// measure answers `None` for every track — the one the mock has and
  /// the one it does not — and still delivers the packet it owes.
  #[cfg(any(feature = "std", feature = "alloc"))]
  #[test]
  fn a_demuxer_that_measures_nothing_answers_none_for_every_track() {
    let mut d = LoopDemuxer {
      tracks: vec![Rc::new(TrackInfo::new(
        ms_tb(),
        TrackParams::Audio(AudioTrackParams::new(1, 48_000, 2, 0, 0)),
        (),
      ))],
      drained: false,
    };
    assert_eq!(d.measured_end(TrackIndex::new(0)), None);
    assert_eq!(
      d.measured_end(TrackIndex::new(9)),
      None,
      "outside the table"
    );
    assert!(d.next_packet().expect("pull").is_some());
    assert_eq!(
      d.measured_end(TrackIndex::new(0)),
      None,
      "a backend that never overrode the method measures nothing, whatever it delivered",
    );
  }

  /// Reading the table is non-destructive, and a handle taken before
  /// the first pull is still the session's own row after EOF.
  ///
  /// This is the unit half of the regression behind
  /// [issue #51](https://github.com/findit-studio/mediadecode/issues/51):
  /// the face this replaced moved the table out of the session, and
  /// the backend that classified packets against that same table
  /// answered a clean end-of-file to every caller who followed the
  /// documented order.
  #[cfg(any(feature = "std", feature = "alloc"))]
  #[test]
  fn the_table_survives_the_session_and_the_handles_stay_the_rows() {
    let mut d = LoopDemuxer {
      tracks: vec![
        Rc::new(TrackInfo::new(
          ms_tb(),
          TrackParams::Audio(AudioTrackParams::new(1, 48_000, 2, 0, 0)),
          (),
        )),
        Rc::new(TrackInfo::new(
          ms_tb(),
          TrackParams::Subtitle(SubtitleTrackParams::new(2)),
          (),
        )),
      ],
      drained: false,
    };

    // The documented order: take the handles first, pull afterwards.
    let held: Vec<Rc<TrackInfo<Loopback>>> = d.tracks().to_vec();
    let expected: Vec<TrackKind> = held.iter().map(|t| t.kind()).collect();
    assert_eq!(expected, vec![TrackKind::Audio, TrackKind::Subtitle]);

    let mut pulled = 0;
    while d.next_packet().expect("pull").is_some() {
      pulled += 1;
      assert_eq!(d.tracks().len(), 2, "the table is not spent by a pull");
    }
    assert_eq!(pulled, 1, "the mock's one packet, not an empty session");

    // After EOF the table is the same table, row for row, and the
    // handles taken before the first pull address those very rows.
    assert_eq!(d.tracks().len(), held.len());
    for (before, after) in held.iter().zip(d.tracks()) {
      assert!(Rc::ptr_eq(before, after), "the same row, not a copy");
    }
    assert_eq!(
      d.tracks().iter().map(|t| t.kind()).collect::<Vec<_>>(),
      expected,
    );
    // The rows the handles address are readable, not dangling.
    assert_eq!(held[0].timebase(), ms_tb());
  }

  /// The same session face with **no allocator**: the row handle is a
  /// borrow, so the demux tier stays `core`-only.
  ///
  /// The choice [`Demuxer::TrackHandle`] opens is the reason this
  /// compiles at all — the table's carrier is the backend's choice,
  /// exactly as [`Demuxer::Buffer`] already was. Deliberately outside
  /// the `alloc` gate: if the tier ever grew a hard dependency on the
  /// heap, this mock would stop compiling first.
  struct BorrowedDemuxer<'a> {
    tracks: &'a [&'a TrackInfo<Loopback>],
    drained: bool,
  }

  impl<'a> Demuxer for BorrowedDemuxer<'a> {
    type Adapter = Loopback;
    type Buffer = &'static [u8];
    type TrackHandle = &'a TrackInfo<Loopback>;
    type Error = LoopError;

    fn tracks(&self) -> &[&'a TrackInfo<Loopback>] {
      self.tracks
    }

    fn next_packet(&mut self) -> Result<Option<DemuxedPacket<Loopback, &'static [u8]>>, LoopError> {
      if self.drained {
        return Ok(None);
      }
      self.drained = true;
      Ok(Some(DemuxedPacket::Subtitle(SubtitleTrackPacket::new(
        TrackIndex::new(0),
        SubtitlePacket::new(&[][..], ()),
      ))))
    }

    fn seek(&mut self, _target: Timestamp) -> Result<(), LoopError> {
      self.drained = false;
      Ok(())
    }
  }

  #[test]
  fn a_borrowed_table_binds_the_handle_slot_without_an_allocator() {
    let row = TrackInfo::<Loopback>::new(
      ms_tb(),
      TrackParams::Subtitle(SubtitleTrackParams::new(7)),
      (),
    );
    let rows = [&row];
    let mut d = BorrowedDemuxer {
      tracks: &rows,
      drained: false,
    };

    let held: &TrackInfo<Loopback> = d.tracks()[0];
    assert!(d.next_packet().expect("pull").is_some());
    assert!(d.next_packet().expect("pull").is_none());
    assert_eq!(d.tracks().len(), 1, "the table outlives the pull loop");
    assert_eq!(held.kind(), TrackKind::Subtitle);
    assert!(core::ptr::eq(held, d.tracks()[0]));
  }

  /// **The provided chapter table costs no allocator.** Deliberately
  /// outside the `alloc` gate, for the same reason
  /// [`BorrowedDemuxer`] itself is: the empty slice the default hands
  /// back is a promoted `&'static []`, not a `Vec`, and if that ever
  /// stopped being true this lane would be the first to say so.
  #[test]
  fn the_provided_chapter_table_needs_no_allocator() {
    let row = TrackInfo::<Loopback>::new(
      ms_tb(),
      TrackParams::Subtitle(SubtitleTrackParams::new(7)),
      (),
    );
    let rows = [&row];
    let d = BorrowedDemuxer {
      tracks: &rows,
      drained: false,
    };
    assert!(d.chapters().is_empty());
  }

  /// The provided container duration needs no allocator either: it is
  /// an `Option` of a `Copy` value, and this lane sits outside the
  /// `alloc` gate so that stays true.
  #[test]
  fn the_provided_container_duration_needs_no_allocator() {
    let row = TrackInfo::<Loopback>::new(
      ms_tb(),
      TrackParams::Subtitle(SubtitleTrackParams::new(7)),
      (),
    );
    let rows = [&row];
    let d = BorrowedDemuxer {
      tracks: &rows,
      drained: false,
    };
    assert_eq!(d.duration(), None);
  }

  /// The provided measured end needs no allocator: a `Copy` value in an
  /// `Option`, outside the `alloc` gate like its siblings.
  #[test]
  fn the_provided_measured_end_needs_no_allocator() {
    let row = TrackInfo::<Loopback>::new(
      ms_tb(),
      TrackParams::Subtitle(SubtitleTrackParams::new(7)),
      (),
    );
    let rows = [&row];
    let d = BorrowedDemuxer {
      tracks: &rows,
      drained: false,
    };
    assert_eq!(d.measured_end(TrackIndex::new(0)), None);
  }
}
