//! [`mediadecode::demuxer::Demuxer`] impl backed by `libavformat`.
//!
//! Opens a container — from a path, or from any `Read + Seek` reader
//! through a custom `AVIOContext` — builds its track table once, and
//! then hands packets out one at a time in interleaved file order.
//!
//! The table is built at open and **kept for the life of the session**:
//! it is what every packet is classified against, so reading it takes
//! nothing away and may happen at any point. Rows are handed out as
//! `triomphe::Arc<TrackInfo<Ffmpeg>>` handles — see
//! [`Demuxer::TrackHandle`](mediadecode::demuxer::Demuxer::TrackHandle).
//!
//! A container's **table of contents** is read at that same moment and
//! kept the same way: `AVFormatContext.chapters`, mirrored into
//! [`Chapter`] rows and answered by
//! [`Demuxer::chapters`](mediadecode::demuxer::Demuxer::chapters). See
//! [`build_chapters`] for what is read, what is bounded before it is
//! allocated, and what is deliberately left as the container wrote it.
//!
//! Opening is two calls, `avformat_open_input` and
//! `avformat_find_stream_info`, with one read between them: each
//! stream's duration is taken as libavformat held it **before probing**
//! — see [`PreProbeDurations`] — because probing fills in figures that
//! afterwards cannot be told from the ones a reader computed.
//!
//! The container-level **duration** is read at open too —
//! `AVFormatContext.duration`, with libavformat's account of how it
//! came by it, answered by
//! [`Demuxer::duration`](mediadecode::demuxer::Demuxer::duration) — and
//! it is the container's, never a track's: see [`container_duration`].
//!
//! What the **walk** measures is kept beside the tables, and costs no
//! read of its own: each timed packet [`Demuxer::next_packet`] reads
//! folds its end into its track's running figure, and
//! [`Demuxer::measured_end`](mediadecode::demuxer::Demuxer::measured_end)
//! answers it — marked complete only once a walk that skipped nothing
//! has reached end of file, and exact only while every packet's end was
//! known. See [`Measured`].
//!
//! # What normalization this layer does
//!
//! libavformat's track table is not quite the one the demux tier
//! promises, and the gap is entirely about attachments:
//!
//! - **Cover art is an attachment, not video.** A still image in an
//!   MP3, FLAC or MP4 arrives as a video stream carrying
//!   `AV_DISPOSITION_ATTACHED_PIC`. This layer maps it to
//!   [`TrackKind::Attachment`], so the `Video` arm carries true motion
//!   video and nothing else.
//! - **A font's bytes are not in the packet stream at all.** An
//!   `AVMEDIA_TYPE_ATTACHMENT` stream never produces a packet; its
//!   payload lives in `AVCodecParameters.extradata`. This layer
//!   synthesizes the packet at open time.
//! - **Cover art's packet is hoisted.** libavformat parks the real
//!   packet in `AVStream.attached_pic`; some demuxers also emit it in
//!   the packet stream, some do not. This layer takes it from
//!   `attached_pic` at open time and drops the duplicate if it ever
//!   arrives, so the count is exactly one either way.
//!
//! Both kinds are queued at open — every attachment track, without
//! exception, or the open fails. That is what makes the face's "exactly
//! one packet, before any timed packet" true *by construction* here:
//! the queue is complete and drains before the first `av_read_frame`
//! call ever runs, so no packet on an attachment track can be anything
//! but a duplicate, and no seek can move a packet that was never on the
//! timeline.
//!
//! # Seeking
//!
//! `seek` converts the target to `AV_TIME_BASE` units and calls
//! `avformat_seek_file` over the window `[i64::MIN, target]`, which is
//! FFmpeg's backward convention: the landing point is the nearest
//! keyframe at or before the target, never after. `avformat_seek_file`
//! flushes libavformat's own buffers; this layer clears the EOF latch
//! it set itself, and deliberately does **not** touch the attachment
//! bookkeeping — an attachment already handed out is never handed out
//! again, and one not yet handed out is still owed.

use std::{
  collections::VecDeque,
  ffi::{CStr, CString, c_int},
  io::{Read, Seek},
  num::NonZeroI32,
  path::Path,
  ptr::{self, addr_of, read_unaligned},
  sync::Arc,
};

// **The track handle's refcount is triomphe's, not `std`'s**, for the
// reason [`crate::buffer`] gives at length: `std::sync::Arc::new`
// aborts when the allocator declines, and the number of these headers
// is the container's stream count. `triomphe::Arc::try_new` reports
// it. `std::sync::Arc` stays for the reader-panic latch, whose one
// allocation is per session rather than per stream.
use triomphe::Arc as TrackArc;

use derive_more::{IsVariant, TryUnwrap, Unwrap};
use ffmpeg_next::{
  Dictionary, Packet, Rational,
  ffi::{
    AV_DISPOSITION_ATTACHED_PIC, AV_DISPOSITION_TIMED_THUMBNAILS, AV_NOPTS_VALUE, AVDictionary,
    AVDurationEstimationMethod, AVFMT_FLAG_CUSTOM_IO, AVFormatContext, AVPacketSideDataType,
    AVStream, av_dict_get, avformat_alloc_context, avformat_find_stream_info, avformat_open_input,
  },
  format::context::{Input, StreamIo},
};
use mediadecode::{
  Timebase, Timestamp,
  demuxer::{
    AttachmentPacket, AttachmentTrackPacket, AttachmentTrackParams, AudioTrackPacket,
    AudioTrackParams, Chapter, ContainerDuration, DataTrackPacket, DataTrackParams, DemuxedPacket,
    Demuxer, DurationSource, MeasuredEnd, SubtitleTrackPacket, SubtitleTrackParams, TrackIndex,
    TrackInfo, TrackKind, TrackParams, UnknownTrackParams, VideoTrackPacket, VideoTrackParams,
  },
  nal::{NalCodec, NalStream},
  packet::PacketFlags,
};
use smol_bytes::Utf8Bytes;

use crate::{
  CodecId, Ffmpeg, SampleFormat, boundary,
  buffer::PacketBufferError,
  extras::{AttachmentPacketExtra, TrackExtra},
  limits::DemuxLimits,
  measured::Measured,
  reader_guard::{GuardedReader, PanicLatch},
  text::{lossy_len, lossy_text},
};
// The codec-parameter refusals are the base's: `mediaio-ffmpeg` mirrors,
// copies and rebuilds parameters, and this demuxer reports each of its
// refusals under the same payload (`From<ParametersError>` below).
use mediaio_ffmpeg::error::{
  ParametersAlloc, ParametersChannelMap, ParametersCopy, ParametersError, ParametersLayoutShape,
  ParametersMissing, ParametersOpaque, ParametersTooLarge,
};

/// One microsecond — the timebase `avformat_seek_file` expects when no
/// reference stream is named (`stream_index == -1`).
fn av_time_base_q() -> Timebase {
  Timebase::new(1, NonZeroI32::new(1_000_000).expect("1e6 is non-zero"))
}

/// A read that answers in place of libavformat's next one. Test only.
#[cfg(test)]
enum InjectedRead {
  /// libavformat answers this error. `AVERROR_INVALIDDATA` is the one
  /// that matters: damage is worded differently in every container, and
  /// the policy for it is one.
  Fault(ffmpeg_next::Error),
  /// libavformat hands over a packet on `stream` carrying this timing
  /// and payload. An empty payload is a packet with nothing in it: some
  /// demuxers write timed ones, and the end of a track can be one.
  Packet {
    stream: usize,
    pts: Option<i64>,
    duration: i64,
    payload: Vec<u8>,
  },
}

/// `mediadecode::demuxer::Demuxer` impl wrapping `ffmpeg::format::context::Input`.
///
/// Construction is deliberately not on the trait — see [`Self::open`]
/// and [`Self::open_reader`].
///
/// # The lanes, from outside this crate
///
/// Holding a lane-parameterized type generically — the public structs
/// carry **only** this bound, so a consumer's own generic code can pass
/// them around:
///
/// ```
/// use mediadecode::demuxer::TrackInfo;
/// use mediadecode_ffmpeg::{
///   CarrierAudioStreamDecoder, CarrierDemuxer, CarrierVideoStreamDecoder, Ffmpeg,
///   FfmpegCarrier, Owned, View,
/// };
///
/// fn tracks_of<C: FfmpegCarrier>(demuxer: &CarrierDemuxer<C>) -> usize {
///   // A field read is not an operation on the lane, so this is
///   // exactly as generic as it looks.
///   core::mem::size_of_val(demuxer)
/// }
///
/// fn hold<C: FfmpegCarrier>(
///   _demuxer: &CarrierDemuxer<C>,
///   _audio: &CarrierAudioStreamDecoder<C>,
///   _video: &CarrierVideoStreamDecoder<C>,
/// ) {
/// }
///
/// let _: fn(&CarrierDemuxer<View>) -> usize = tracks_of::<View>;
/// let _: fn(&CarrierDemuxer<Owned>) -> usize = tracks_of::<Owned>;
/// let _ = hold::<View>;
/// let _ = hold::<Owned>;
/// let _: fn() -> Vec<TrackInfo<Ffmpeg>> = || Vec::new();
/// ```
///
/// And calling at either concrete lane, through the aliases or through
/// the `Carrier*` names directly:
///
/// ```no_run
/// use mediadecode::demuxer::Demuxer;
/// use mediadecode_ffmpeg::{
///   CarrierDemuxer, FfmpegDemuxer, FfmpegOwnedDemuxer, Owned, View,
/// };
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let mut viewed = FfmpegDemuxer::open("clip.mkv")?;
/// let mut owned = CarrierDemuxer::<Owned>::open("clip.mkv")?;
/// let _ = viewed.tracks().len();
/// let _ = owned.next_packet()?;
/// let _: fn(&std::path::Path) -> _ = CarrierDemuxer::<View>::open::<std::path::Path>;
/// let _: fn(&std::path::Path) -> _ = FfmpegOwnedDemuxer::open::<std::path::Path>;
/// # Ok(())
/// # }
/// ```
///
/// The generic bodies behind the per-lane faces stay out of reach:
/// they carry the lane operations' bound, so reaching one at a
/// concrete lane would drive the operations without the proofs the
/// per-lane faces supply.
///
/// ```compile_fail,E0624
/// use mediadecode_ffmpeg::{CarrierAudioStreamDecoder, DecoderLimits, Owned};
/// fn downstream(parameters: ffmpeg_next::codec::Parameters) {
///   let _ = CarrierAudioStreamDecoder::<Owned>::open_impl(
///     parameters,
///     mediadecode::Timebase::default(),
///     DecoderLimits::default(),
///   );
/// }
/// ```
///
/// ```compile_fail,E0624
/// use mediadecode_ffmpeg::{CarrierDemuxer, View};
/// fn downstream() {
///   let _ = CarrierDemuxer::<View>::open_impl("clip.mkv");
/// }
/// ```
pub struct CarrierDemuxer<C: crate::FfmpegCarrier> {
  input: Input,
  /// The track table, built once at open and held for the life of the
  /// session — **this is the table `next_packet` classifies against**,
  /// so nothing may take it away.
  ///
  /// Rows are `Arc`-wrapped at the door rather than by each consumer:
  /// [`TrackInfo`] is not `Clone` (the message-carrier law), so a
  /// consumer that needs a row past a borrow of this session needs a
  /// shared handle, and one allocation per track at open is the whole
  /// cost of every fan-out afterwards. `Arc` and not `Rc` because
  /// [`CodecTicket`](crate::ticket::CodecTicket) made these rows
  /// `Send + Sync` by construction precisely so a track table could
  /// cross tasks.
  tracks: Vec<TrackArc<TrackInfo<Ffmpeg>>>,
  /// The container's table of contents, mirrored once at open and held
  /// for the life of the session — see [`build_chapters`].
  ///
  /// Not `Arc`-wrapped, unlike the track rows above, and for the
  /// reason [`Chapter`] gives: a chapter is four scalars and a title,
  /// so a consumer that wants one past a borrow of this session clones
  /// the row itself and there is no carrier choice worth making.
  ///
  /// Empty for the overwhelming majority of files — nothing allocates
  /// where a container declares no chapters.
  chapters: Vec<Chapter<Ffmpeg>>,
  /// What libavformat decided the bytes are wrapped in, read once at
  /// open — see [`CarrierDemuxer::format`].
  ///
  /// Held rather than re-derived because it is a property of the
  /// session: `avformat_open_input` picks the demuxer and never changes
  /// it, so the answer cannot move and a second read could only cost
  /// more. `None` only where libavformat left `iformat` null or its
  /// name is not readable text — neither of which a successful open
  /// produces.
  format: Option<crate::ContainerFormat>,
  /// The container's duration and its provenance, read once at open —
  /// see [`container_duration`].
  duration: Option<ContainerDuration>,
  /// What the walk has measured so far, one figure per track — see
  /// [`Measured`]. Fed by [`Self::next_packet_impl`] as packets are
  /// read; it reads nothing itself.
  measured: Measured,
  /// Each track's reading of its H.264 or HEVC bitstream, by track
  /// index — `None` for every other track — through which
  /// [`Self::next_packet_impl`] marks a video packet
  /// [`DISPOSABLE`](mediadecode::packet::PacketFlags::DISPOSABLE) where
  /// its NAL units prove that nothing references its picture
  /// ([`mediadecode::nal`]), in any container, and clears the mark where
  /// the packet carries side data that persists past it
  /// ([`marked_from_bitstream`]). Built at open from each stream's codec
  /// and extradata ([`bitstreams`]); told of every seek ([`sought`]).
  bitstreams: Vec<Option<NalStream>>,
  /// Reads that answer in place of libavformat's next ones. Test only:
  /// see [`Self::read_into`].
  #[cfg(test)]
  injected_reads: VecDeque<InjectedRead>,
  pending: VecDeque<(
    TrackIndex,
    AttachmentPacket<AttachmentPacketExtra, C::Buffer>,
  )>,
  /// `true` once this session has answered `Ok(None)`. Only then does
  /// [`Self::seek`] clear the `AVIOContext`'s EOF latch — clearing it
  /// unconditionally would also erase a genuine sticky I/O error, which
  /// `Input::seek` goes out of its way to preserve.
  eof: bool,
  /// `true` once this session has reported a packet whose stream the
  /// track table does not describe.
  ///
  /// The diagnostic is **once per session, not once per packet**. A
  /// format that adds an `AVStream` mid-read (`AVFMTCTX_NOHEADER`:
  /// MPEG-TS, RTP) then delivers packets on it at the wire's own rate,
  /// and a line each would be an unbounded log on healthy input — a
  /// live stream could fill a disk with it. One line names the
  /// condition; the rest of the session stays quiet. Never cleared,
  /// including across a seek: it records that this session has said
  /// its piece, which a seek does not undo.
  unplaceable_reported: bool,
  /// Set for a session opened over a caller's reader: where a panic
  /// raised inside that reader is recorded. `None` for a path-opened
  /// session, which runs no caller code.
  reader_panic: Option<Arc<PanicLatch>>,
  /// The budgets this session spends: on any one timed packet, and —
  /// already spent, at open — on the file's attachments.
  limits: DemuxLimits,
  /// A packet `av_read_frame` has already handed over and whose
  /// conversion has **not committed**, with the provenance that was
  /// observed for it.
  ///
  /// `av_read_frame` advances the container: once it returns, that
  /// packet is off the wire and nothing brings it back. A conversion
  /// that then fails on an *allocation* — a refcount the view lane
  /// could not take, a copy the middle row could not make — used to
  /// drop it, leaving a live session that answered the next pull with
  /// the **following** packet. Compressed data and subtitle cues went
  /// missing under memory pressure, quietly.
  ///
  /// So the read and the conversion are one transaction with a seat
  /// between them: a transient refusal parks the packet here and the
  /// next pull re-attempts *this* packet before reading another. It is
  /// the same park-then-replay the decode household already runs —
  /// `CarrierVideoStreamDecoder` holds `sw_replay_frames`, and the
  /// probe holds its rescue history — for the same reason: a byte C
  /// has already given up is not re-askable.
  ///
  /// The provenance is parked **with** the packet rather than re-probed
  /// on replay. It is an observation about the moment of delivery, and
  /// a queue that has moved on could answer it differently.
  unconverted: Option<(Packet, crate::buffer::PayloadProvenance)>,
}

// The generic bodies. Crate-private, because their bound is: they are
// the implementation, and the public faces below are written per lane
// so that no signature a consumer reads names a trait they cannot.
impl<C: crate::FfmpegCarrier + crate::CarrierOps> CarrierDemuxer<C> {
  /// Opens a container from a filesystem path.
  ///
  /// Runs `avformat_open_input` followed by
  /// `avformat_find_stream_info`, then builds the track table and
  /// captures every attachment payload.
  ///
  /// Call [`ffmpeg_next::init`] once before the first open if you want
  /// FFmpeg's logging and network protocols configured; probing a local
  /// container does not require it.
  pub(crate) fn open_impl<P: AsRef<Path> + ?Sized>(path: &P) -> Result<Self, DemuxError> {
    Self::open_with_impl(path, DemuxLimits::default())
  }

  /// [`Self::open`], with the session's resource budgets named.
  ///
  /// The budgets are taken **at open** rather than through a `with_*`
  /// builder because the attachment half of them is spent here: every
  /// attachment payload in the file is captured before this call
  /// returns, which is what makes the demux tier's "exactly one packet,
  /// before any timed packet" contract true by construction. A budget
  /// set afterwards would arrive after the spending.
  ///
  /// A file whose attachments exceed the budget **fails to open**, with
  /// [`DemuxError::AttachmentTooLarge`] or
  /// [`DemuxError::AttachmentBudgetExhausted`] naming the track that
  /// crossed the line.
  pub(crate) fn open_with_impl<P: AsRef<Path> + ?Sized>(
    path: &P,
    limits: DemuxLimits,
  ) -> Result<Self, DemuxError> {
    // **The probe knobs, set before libavformat reads a byte.** See
    // [`DemuxLimits::max_probe_bytes`]: `avformat_open_input` and
    // `avformat_find_stream_info` build the attachment, extradata and
    // coded-side-data buffers themselves, so every budget that measures
    // *this crate's* copies arrives after the original allocation. The
    // instrument that reaches behind that is the one bounding what the
    // parser is handed in the first place.
    //
    // On this entrypoint that is `probesize` / `formatprobesize` /
    // `max_streams` only: the hard byte meter needs an `AVIOContext`
    // this crate owns, and a path is opened by libavformat's own
    // protocol layer. The reader entrypoint gets both.
    let (input, pre_probe) = open_path(path.as_ref(), limits)?;
    Self::from_input(input, pre_probe, limits)
  }

  /// Opens a container from any `Read + Seek` byte source, through a
  /// custom `AVIOContext`.
  ///
  /// `Seek` is mandatory and not negotiable: MP4 files routinely put
  /// `moov` at the end, so a reader that cannot go backwards cannot be
  /// probed at all — and the seek law on the face would be
  /// unimplementable.
  ///
  /// `filename` is a probe hint, not a path: libavformat uses its
  /// extension to break ties between formats whose byte signatures are
  /// ambiguous. Pass `None` when there is nothing to hint with.
  ///
  /// # A panicking reader
  ///
  /// libavformat drives the reader from `extern "C"` callbacks, where a
  /// panic would abort the process rather than unwind. Every call into
  /// `reader` therefore runs under `catch_unwind`: a panic becomes an
  /// I/O error for libavformat and surfaces here — or from the next
  /// [`next_packet`](Demuxer::next_packet) / [`seek`](Demuxer::seek) —
  /// as [`DemuxError::ReaderPanic`], carrying the panic's message. The
  /// session is terminal from that point: the `AVIOContext`'s error
  /// state is sticky and the reader's own state is unknown.
  pub(crate) fn open_reader_impl<R: Read + Seek + Send + 'static>(
    reader: R,
    filename: Option<&str>,
  ) -> Result<Self, DemuxError> {
    Self::open_reader_with_impl(reader, filename, DemuxLimits::default())
  }

  /// [`Self::open_reader`], with the session's resource budgets named.
  /// See [`Self::open_with`] for why they are taken at open.
  pub(crate) fn open_reader_with_impl<R: Read + Seek + Send + 'static>(
    reader: R,
    filename: Option<&str>,
    limits: DemuxLimits,
  ) -> Result<Self, DemuxError> {
    let (guarded, latch, meter) = GuardedReader::new(reader, limits.max_probe_bytes());
    let io = StreamIo::from_read_seek(guarded)?;
    let (input, pre_probe) = open_stream(io, filename, probe_options(limits)).map_err(|fault| {
      let OpenFault::Ffmpeg(e) = fault else {
        return DemuxError::from(fault);
      };
      // Three ways this can fail, and they must not be confused: a
      // panicked reader, a probe budget reached, or libavformat's own
      // verdict. The meter is consulted before the errno because
      // libavformat folds the reader's I/O error into whatever it was
      // doing at the time — usually "invalid data" — which would
      // report a refusal this crate made as a malformed file.
      reader_panic(&latch)
        .or_else(|| {
          meter.tripped().then(|| {
            DemuxError::ProbeBudgetExhausted(ProbeBudgetExhausted::new(
              meter.read(),
              meter.budget(),
            ))
          })
        })
        .unwrap_or(DemuxError::Ffmpeg(e))
    })?;
    if meter.tripped() {
      return Err(DemuxError::ProbeBudgetExhausted(ProbeBudgetExhausted::new(
        meter.read(),
        meter.budget(),
      )));
    }
    // Open and analysed: the seat bounds *probing*, and reading the
    // media itself afterwards is the caller's business, packet by
    // packet, already bounded by the packet seats.
    meter.release();
    // A panic libavformat tolerated (a failed probe it recovered from)
    // still poisoned the reader; the session must not open over it.
    if let Some(panicked) = reader_panic(&latch) {
      return Err(panicked);
    }
    let mut demuxer = Self::from_input(input, pre_probe, limits)?;
    demuxer.reader_panic = Some(latch);
    Ok(demuxer)
  }

  /// Borrows the wrapped `ffmpeg::format::context::Input` — for
  /// `av_dump_format`, container-level metadata, and anything else the
  /// portable track and chapter tables have no seat for.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub(crate) const fn input_impl(&self) -> &Input {
    &self.input
  }

  /// The budgets this session was opened with.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub(crate) const fn limits_impl(&self) -> DemuxLimits {
    self.limits
  }

  /// What libavformat decided this session's bytes are wrapped in.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub(crate) const fn format_impl(&self) -> Option<&crate::ContainerFormat> {
    self.format.as_ref()
  }

  /// The container's duration and its provenance — see
  /// [`container_duration`].
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub(crate) const fn duration_impl(&self) -> Option<ContainerDuration> {
    self.duration
  }

  fn from_input(
    input: Input,
    pre_probe: PreProbeDurations,
    limits: DemuxLimits,
  ) -> Result<Self, DemuxError> {
    // **Everything the container declares is judged before anything it
    // declares is paid for**, and that is a property of the *whole*
    // open rather than of either table.
    //
    // The order is: [`admit_chapters`] here, then [`admit_streams`] as
    // `build_tracks`' first statement, and only then a reservation. Both
    // passes allocate nothing — they read integers, and price metadata
    // through a borrow of libavutil's own buffer — so every budgeted
    // quantity this open can refuse is refused while the process has
    // spent nothing on the file.
    //
    // The class, stated once so it can be checked rather than
    // rediscovered: probe bytes are metered during the read;
    // `max_streams` is libavformat's own option, set before the header
    // is parsed; per-stream and whole-file codec parameters — which
    // includes extradata, every coded-side-data payload and a custom
    // channel map, all through
    // [`measure_parameters`](crate::extras::measure_parameters) — the
    // per-attachment and whole-file attachment payloads, and the
    // whole-file stream metadata are charged by `admit_streams`; the
    // chapter count and the chapter titles by `admit_chapters`; and a
    // single metadata value is bounded by [`metadata_value`]'s own
    // walk, which stops at [`METADATA_VALUE_MAX_BYTES`] rather than
    // reading past it. Three rounds of review found three instances of
    // one defect here — a correct check placed after the memory it was
    // meant to protect — which is why the list is written down.
    admit_chapters(&input, limits)?;
    let (tracks, pending) = build_tracks::<C>(&input, &pre_probe, limits)?;
    // One allocation per track, here and never again: the session
    // keeps these handles and hands out clones of them.
    //
    // The table is reserved fallibly and so is each row's handle:
    // `triomphe::Arc::try_new` reports an allocator refusal where
    // `std::sync::Arc::new` would abort, and the count is the
    // container's. See [`crate::buffer`] for why this crate's
    // refcount is triomphe's.
    let count = tracks.len();
    let mut handles: Vec<TrackArc<TrackInfo<Ffmpeg>>> = Vec::new();
    handles
      .try_reserve_exact(count)
      .map_err(|_| DemuxError::TrackTableAlloc(TrackTableAlloc::new(count)))?;
    for row in tracks {
      handles.push(
        TrackArc::try_new(row)
          .map_err(|_| DemuxError::TrackTableAlloc(TrackTableAlloc::new(count)))?,
      );
    }
    let tracks = handles;
    // The chapter table is read here for the same reason the track
    // table is: `avformat_find_stream_info` has run, so the container's
    // answer is final and a session that holds it can be asked at any
    // point without touching the file again.
    let chapters = build_chapters(&input, limits)?;
    // SAFETY: `input` owns a live `AVFormatContext` for the whole of
    // this call, and the read takes copies of the two static-table
    // strings rather than borrowing from it.
    let format = unsafe { crate::ContainerFormat::from_context(input.as_ptr()) };
    let duration = container_duration(&input);
    // One figure per track, reserved fallibly like the table it rides
    // beside: the count is the container's, already past
    // `max_streams`.
    let measured =
      Measured::new(count).map_err(|_| DemuxError::TrackTableAlloc(TrackTableAlloc::new(count)))?;
    let bitstreams = bitstreams(&input)?;
    Ok(Self {
      input,
      tracks,
      chapters,
      format,
      duration,
      measured,
      bitstreams,
      #[cfg(test)]
      injected_reads: VecDeque::new(),
      pending,
      unconverted: None,
      eof: false,
      unplaceable_reported: false,
      reader_panic: None,
      limits,
    })
  }

  /// The error a panicked reader owes this session, if one panicked.
  fn panicked(&self) -> Option<DemuxError> {
    self.reader_panic.as_deref().and_then(reader_panic)
  }
}

/// The libavformat options this crate sets before a container is
/// opened.
///
/// Passed as an `AVDictionary` because that is the only route to these
/// fields that works for both entrypoints: `avformat_open_input`
/// applies the dictionary to the context it allocates itself, and the
/// same names reach the context behind a custom `AVIOContext`.
///
/// * `probesize` / `formatprobesize` bound what the format probe and
///   the stream analysis are allowed to consume;
/// * `max_streams` bounds the `AVStream` array a header can conjure —
///   a container claiming a hundred thousand streams is an allocation
///   this crate's per-track budgets are downstream of.
fn probe_options(limits: DemuxLimits) -> ffmpeg_next::Dictionary<'static> {
  let mut options = ffmpeg_next::Dictionary::new();
  let probe = limits.max_probe_bytes().to_string();
  options.set("probesize", &probe);
  options.set("formatprobesize", &probe);
  options.set("max_streams", &limits.max_streams().to_string());
  options
}

/// Why [`open_path`] or [`open_stream`] could not produce an input.
enum OpenFault {
  /// libavformat's own verdict, from either call it was asked to make.
  Ffmpeg(ffmpeg_next::Error),
  /// The record of pre-probe durations could not be reserved.
  Alloc(TrackTableAlloc),
}

impl From<OpenFault> for DemuxError {
  fn from(fault: OpenFault) -> Self {
    match fault {
      OpenFault::Ffmpeg(e) => DemuxError::Ffmpeg(e),
      OpenFault::Alloc(alloc) => DemuxError::TrackTableAlloc(alloc),
    }
  }
}

/// One stream's `AVStream.duration` as libavformat held it after
/// `avformat_open_input` and before `avformat_find_stream_info`, with the
/// ruler it was in.
#[derive(Clone, Copy)]
struct PreProbeDuration {
  /// `AVStream.duration` at that moment; `AV_NOPTS_VALUE` where
  /// libavformat held none.
  ticks: i64,
  /// `AVStream.time_base` at the same moment.
  num: i32,
  den: i32,
}

/// libavformat's figure for each stream's duration **before it probes**:
/// read after `avformat_open_input` and before
/// `avformat_find_stream_info`.
///
/// This is libavformat's figure and no claim about the file. A format's
/// reader may compute it itself rather than read it — libavformat's WAV
/// reader derives it from the data chunk's size — and this record
/// cannot tell that from a duration the file declares.
///
/// What it is for is keeping probing's fill out. Probing rewrites
/// `AVStream.duration`: a stream that supplied no timing while
/// libavformat read packets — an attachment, or a timed stream whose
/// first packet lies past the probe, such as a sparse subtitle track —
/// is handed the *container's* start and length in its place, and from
/// then on that cannot be told from a figure a reader had computed (an
/// MP4 track whose own figure equals the movie's looks identical).
/// Looking before the fill is the one road that separates them, so that
/// is when this reads, and a value that appears only after probing is
/// not carried.
///
/// **Its limit.** Only streams that exist before probing are recorded.
/// A format that creates a stream while it is being probed has no figure
/// here, whatever its demuxer set on that stream, because afterwards the
/// figure cannot be told from the fill. The per-track figure measured
/// from packets is `measured_end`.
///
/// The time base is kept with each figure, so a figure keeps the ruler
/// it was in even if probing later refines the stream's own.
struct PreProbeDurations(Vec<PreProbeDuration>);

impl PreProbeDurations {
  /// Reads every stream libavformat has opened. The count is the
  /// container's, already capped by `max_streams`, and the record is
  /// reserved fallibly.
  fn read(input: &Input) -> Result<Self, OpenFault> {
    // SAFETY: `input` owns a live, opened `AVFormatContext`;
    // `nb_streams` and `streams` are public fields of it, and `streams`
    // holds `nb_streams` entries.
    let (count, streams) = unsafe {
      let context = input.as_ptr();
      ((*context).nb_streams as usize, (*context).streams)
    };
    let mut pre_probe = Vec::new();
    pre_probe
      .try_reserve_exact(count)
      .map_err(|_| OpenFault::Alloc(TrackTableAlloc::new(count)))?;
    for index in 0..count {
      // SAFETY: entries `0..nb_streams` of `streams` are the context's
      // own `AVStream` pointers, live as long as `input` is.
      let stream = unsafe { *streams.add(index) };
      pre_probe.push(if stream.is_null() {
        PreProbeDuration {
          ticks: AV_NOPTS_VALUE,
          num: 0,
          den: 1,
        }
      } else {
        // SAFETY: a non-null entry points at a live `AVStream`.
        unsafe {
          PreProbeDuration {
            ticks: (*stream).duration,
            num: (*stream).time_base.num,
            den: (*stream).time_base.den,
          }
        }
      });
    }
    Ok(Self(pre_probe))
  }

  /// The duration libavformat held for stream `index` before probing, or
  /// `None` where it held none: `AV_NOPTS_VALUE`, not positive, a stream
  /// that did not yet exist, or a ruler that is not one.
  fn get(&self, index: usize) -> Option<Timestamp> {
    let held = self.0.get(index)?;
    if held.ticks == AV_NOPTS_VALUE || held.ticks <= 0 {
      return None;
    }
    let ruler = rational_to_timebase(Rational::new(held.num, held.den))?;
    Some(Timestamp::new(held.ticks, ruler))
  }
}

/// Opens a container from a path, in the two steps
/// `ffmpeg_next::format::input_with_dictionary` makes —
/// `avformat_open_input`, then `avformat_find_stream_info` — with
/// [`PreProbeDurations`] read between them, which that function gives no
/// chance to do.
fn open_path(path: &Path, limits: DemuxLimits) -> Result<(Input, PreProbeDurations), OpenFault> {
  let path = path_cstring(path).map_err(OpenFault::Ffmpeg)?;
  // SAFETY: `context` starts null, which `avformat_open_input` allocates
  // into; the options dictionary is handed over for the call and what
  // libavformat leaves of it is taken back and freed; on success the
  // context is owned by the `Input` wrapped around it.
  unsafe {
    let mut context: *mut AVFormatContext = ptr::null_mut();
    let mut options = probe_options(limits).disown();
    let status = avformat_open_input(&mut context, path.as_ptr(), ptr::null_mut(), &mut options);
    drop(Dictionary::own(options));
    if status != 0 {
      return Err(OpenFault::Ffmpeg(ffmpeg_next::Error::from(status)));
    }
    probe(Input::wrap(context))
  }
}

/// Opens a container over a custom `AVIOContext`, in the same two steps
/// as [`open_path`] and with `ffmpeg_next::format::input_from_stream`'s
/// own setup of the context.
fn open_stream(
  mut io: StreamIo,
  filename: Option<&str>,
  options: Dictionary<'static>,
) -> Result<(Input, PreProbeDurations), OpenFault> {
  debug_assert!(
    !io.is_writable(),
    "a demux session is only ever opened over a read source",
  );
  let filename = filename
    .map(CString::new)
    .transpose()
    .map_err(|_| OpenFault::Ffmpeg(invalid_argument()))?;
  let filename_ptr = filename.as_ref().map_or(ptr::null(), |name| name.as_ptr());
  // SAFETY: the context is allocated here and given the caller's
  // `AVIOContext` with `AVFMT_FLAG_CUSTOM_IO`, so libavformat neither
  // opens nor closes an I/O context of its own; `avformat_open_input`
  // frees the context itself on failure, in which case `io` drops here;
  // on success the `Input` owns both and closes the context before the
  // I/O context goes.
  unsafe {
    let mut context = avformat_alloc_context();
    if context.is_null() {
      return Err(OpenFault::Ffmpeg(ffmpeg_next::Error::Other {
        errno: libc::ENOMEM,
      }));
    }
    (*context).pb = io.as_mut_ptr();
    (*context).flags |= AVFMT_FLAG_CUSTOM_IO;
    let mut options = options.disown();
    let status = avformat_open_input(&mut context, filename_ptr, ptr::null_mut(), &mut options);
    drop(Dictionary::own(options));
    if status != 0 {
      return Err(OpenFault::Ffmpeg(ffmpeg_next::Error::from(status)));
    }
    probe(Input::wrap_with_custom_io(context, io))
  }
}

/// Reads the pre-probe durations of an opened, unprobed `input`, then
/// probes it. A failure drops `input`, which closes the context.
fn probe(mut input: Input) -> Result<(Input, PreProbeDurations), OpenFault> {
  let pre_probe = PreProbeDurations::read(&input)?;
  // SAFETY: `input` owns a live context that `avformat_open_input` has
  // opened and nothing has probed.
  let status = unsafe { avformat_find_stream_info(input.as_mut_ptr(), ptr::null_mut()) };
  if status < 0 {
    return Err(OpenFault::Ffmpeg(ffmpeg_next::Error::from(status)));
  }
  Ok((input, pre_probe))
}

/// `EINVAL`, the answer for an argument libavformat could never take.
fn invalid_argument() -> ffmpeg_next::Error {
  ffmpeg_next::Error::Other {
    errno: libc::EINVAL,
  }
}

/// A path as the C string `avformat_open_input` takes. A path that is
/// not valid UTF-8, or that holds a NUL byte, is refused with `EINVAL`
/// where `ffmpeg-next`'s own `input_*` functions panic.
fn path_cstring(path: &Path) -> Result<CString, ffmpeg_next::Error> {
  path
    .to_str()
    .and_then(|text| CString::new(text).ok())
    .ok_or_else(invalid_argument)
}

/// The container-level duration libavformat holds once the open has
/// finished, with libavformat's own account of how it came by it.
///
/// `AVFormatContext.duration` counts `AV_TIME_BASE` ticks —
/// microseconds — and is `AV_NOPTS_VALUE` where nothing is known. The
/// raw microsecond **length** is preserved without rescaling, so
/// nothing is rounded. Being a length, it cannot be compared with a
/// track's coordinates — its timestamps and its measured end, which are
/// positions on that track's timeline — until the track's start time
/// and any presentation edits have been applied. A value that is not
/// positive reports no length and answers `None`, the rule a track's
/// duration follows.
///
/// The account is `AVFormatContext.duration_estimation_method`,
/// libavformat's record of how it estimated the figure, read as the raw
/// integer it is; see [`duration_source`] for the map, which keeps
/// libavformat's three words.
///
/// This is the **container's** figure and nothing else's. It is never
/// the answer for a track: a Matroska file reports a length for itself
/// and none for any track, and a row that borrowed this one would
/// present the file's length as the track's own.
fn container_duration(input: &Input) -> Option<ContainerDuration> {
  let raw = input.duration();
  if raw == AV_NOPTS_VALUE || raw <= 0 {
    return None;
  }
  // SAFETY: `input` owns a live `AVFormatContext` for the whole of this
  // call. The field is read as the 4-byte integer the C enum is, never
  // as the bindgen enum — see [`duration_source`].
  let method =
    unsafe { read_unaligned(addr_of!((*input.as_ptr()).duration_estimation_method).cast::<i32>()) };
  Some(ContainerDuration::new(
    Timestamp::new(raw, av_time_base_q()),
    duration_source(method),
  ))
}

/// Maps libavformat's `AVDurationEstimationMethod` onto
/// [`DurationSource`], word for word.
///
/// - `AVFMT_DURATION_FROM_PTS` is [`FromPts`](DurationSource::FromPts).
/// - `AVFMT_DURATION_FROM_STREAM` is
///   [`FromStream`](DurationSource::FromStream).
/// - `AVFMT_DURATION_FROM_BITRATE` is
///   [`FromBitrate`](DurationSource::FromBitrate).
///
/// A method this build does not name is
/// [`Unknown`](DurationSource::Unknown): nothing is claimed about how
/// libavformat came by the figure, and it is not folded into an
/// estimate.
///
/// The method arrives as the integer it is on the wire. Forming the
/// bindgen enum from it would be undefined behaviour for any value
/// outside this build's discriminant set, so only the constants are
/// ever cast, and only to integers.
fn duration_source(method: i32) -> DurationSource {
  const FROM_PTS: i32 = AVDurationEstimationMethod::AVFMT_DURATION_FROM_PTS as i32;
  const FROM_STREAM: i32 = AVDurationEstimationMethod::AVFMT_DURATION_FROM_STREAM as i32;
  const FROM_BITRATE: i32 = AVDurationEstimationMethod::AVFMT_DURATION_FROM_BITRATE as i32;
  match method {
    FROM_PTS => DurationSource::FromPts,
    FROM_STREAM => DurationSource::FromStream,
    FROM_BITRATE => DurationSource::FromBitrate,
    _ => DurationSource::Unknown,
  }
}

/// Payload for [`DemuxError::ProbeBudgetExhausted`].
///
/// libavformat wanted more of the file than the probe budget allows.
///
/// # What this bounds
///
/// This is the only seat in the crate that reaches *behind*
/// libavformat: `avformat_open_input` and `avformat_find_stream_info`
/// build the attached picture, the extradata and the coded side data
/// out of the file themselves, so every budget measuring this crate's
/// own copies necessarily arrives after those allocations happened.
///
/// A parser cannot allocate from bytes it was never handed, so the
/// input is bounded instead. What is **not** bounded is amplification
/// inside a parser — a container can describe, in a few bytes, a
/// structure whose in-memory form is far larger, and nothing outside
/// libavformat can observe that. Bounding the output of that is the
/// substrate's own hardening territory; FFmpeg keeps `max_streams`,
/// `max_index_size` and `max_picture_buffer` for it, and this crate
/// sets the first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("libavformat read {read} bytes probing the container, over a budget of {budget}")]
pub struct ProbeBudgetExhausted {
  read: u64,
  budget: u64,
}

impl ProbeBudgetExhausted {
  /// Constructs a `ProbeBudgetExhausted` payload.
  #[inline]
  pub const fn new(read: u64, budget: u64) -> Self {
    Self { read, budget }
  }
  /// Bytes libavformat was handed before the budget was reached.
  #[inline]
  pub const fn read(&self) -> u64 {
    self.read
  }
  /// The budget in force.
  #[inline]
  pub const fn budget(&self) -> u64 {
    self.budget
  }
}

/// Turns a latched reader panic into the error that names it.
fn reader_panic(latch: &PanicLatch) -> Option<DemuxError> {
  latch
    .message()
    .map(|message| DemuxError::ReaderPanic(ReaderPanic::new(message)))
}

impl<C: crate::FfmpegCarrier + crate::CarrierOps> CarrierDemuxer<C> {
  pub(crate) fn tracks_impl(&self) -> &[TrackArc<TrackInfo<Ffmpeg>>] {
    &self.tracks
  }

  pub(crate) fn chapters_impl(&self) -> &[Chapter<Ffmpeg>] {
    &self.chapters
  }

  /// One `av_read_frame` into `packet`.
  ///
  /// Under test a queued [`InjectedRead`] answers in its place, which is
  /// how a corrupt read and a timed empty packet are stood in for:
  /// libavformat words damage differently in every container, and the
  /// policy for it is one.
  fn read_into(&mut self, packet: &mut Packet) -> Result<(), ffmpeg_next::Error> {
    #[cfg(test)]
    if let Some(injected) = self.injected_reads.pop_front() {
      return match injected {
        InjectedRead::Fault(fault) => Err(fault),
        InjectedRead::Packet {
          stream,
          pts,
          duration,
          payload,
        } => {
          if !payload.is_empty() {
            *packet = Packet::copy(&payload);
          }
          packet.set_stream(stream);
          packet.set_pts(pts);
          packet.set_duration(duration);
          Ok(())
        }
      };
    }
    packet.read(&mut self.input)
  }

  /// Where `track`'s packets ended, as this session's walk measured
  /// it — see [`Measured`].
  pub(crate) fn measured_end_impl(&self, track: TrackIndex) -> Option<MeasuredEnd> {
    let index = track.get();
    self.measured.get(index, self.tracks.get(index)?.timebase())
  }

  pub(crate) fn next_packet_impl(
    &mut self,
  ) -> Result<Option<DemuxedPacket<Ffmpeg, C::Buffer>>, DemuxError> {
    // A latched reader panic is terminal, and terminal starts here. The
    // queue is filled at open and owes nothing to the reader, so a pull
    // that drained it would answer `Ok` to a caller the session has
    // already told the truth to — `seek` can latch a panic while
    // attachments are still queued.
    if let Some(panicked) = self.panicked() {
      return Err(panicked);
    }

    // The attachment queue drains first and drains completely, which is
    // the whole of "exactly one packet, before any timed packet": no
    // `av_read_frame` has run yet when the last one leaves.
    if let Some((track, packet)) = self.pending.pop_front() {
      return Ok(Some(DemuxedPacket::Attachment(AttachmentTrackPacket::new(
        track, packet,
      ))));
    }

    loop {
      // **A parked packet is re-attempted before another is read.**
      // See [`Self::unconverted`]: `av_read_frame` has already given
      // this one up, so reading past it would lose it.
      let (packet, parked_provenance) = match self.unconverted.take() {
        Some((packet, provenance)) => (packet, Some(provenance)),
        None => {
          let mut packet = Packet::empty();
          let read = self.read_into(&mut packet);
          // A panicking reader reported an ordinary I/O error to C, and
          // libavformat may answer that with the error, with EOF (a
          // stream it cannot read looks finished), or with a packet it
          // had already buffered. None of those are the file's word, so
          // the latch is consulted whatever the outcome was.
          if let Some(panicked) = self.panicked() {
            return Err(panicked);
          }
          match read {
            Ok(()) => {}
            Err(ffmpeg_next::Error::Eof) => {
              self.eof = true;
              self.measured.end_of_file();
              return Ok(None);
            }
            // A demuxer can resync past a corrupt packet, and
            // `AVERROR_INVALIDDATA` is not latched into the
            // `AVIOContext`, so reading again makes progress. Every
            // other error is sticky and is surfaced.
            //
            // **The read advanced past data this session never saw**, so
            // the walk has not covered the file and can no longer call
            // its measured ends complete.
            Err(ffmpeg_next::Error::InvalidData) => {
              self.measured.break_walk();
              continue;
            }
            Err(e) => return Err(DemuxError::Ffmpeg(e)),
          }
          (packet, None)
        }
      };

      let index = packet.stream();
      // A packet for a stream the table does not describe cannot be
      // placed, so it is passed by — the same answer the `Unknown` arm
      // below gives a track nothing can name. Passed by is not
      // observed, so it breaks the walk: a session that read a packet
      // on a stream it never described never answers complete.
      //
      // **Neither an assertion nor an error.** The arm is reachable on
      // healthy input: a format flagged `AVFMTCTX_NOHEADER` — MPEG-TS,
      // RTP and the rest that carry no up-front stream list — may add
      // an `AVStream` in the middle of `av_read_frame`, and this
      // session's table was fixed at open, which is the contract
      // `TrackIndex` needs (position in `tracks()`, dense and stable
      // for the life of the session). A `debug_assert` would fire on a
      // transport stream, and an `Err` would end a session over a
      // stream the caller never asked about.
      //
      // It is no longer the *every* packet path. It was, for one
      // release: the take-the-table door emptied this very `Vec`, so
      // every index fell out of range at once and a healthy file
      // demuxed to nothing (issue #51). The table cannot be taken
      // away any more; what is left here is the genuinely
      // out-of-range index the arm was written for. Reported rather
      // than silent, because silence is what made the old failure
      // invisible — and reported **once**, because the very case that
      // makes the arm reachable is a live stream that would otherwise
      // log a line per packet for as long as it runs. See
      // [`Self::unplaceable_reported`].
      let Some(info) = self.tracks.get(index) else {
        self.measured.break_walk();
        if !self.unplaceable_reported {
          self.unplaceable_reported = true;
          tracing::debug!(
            stream = index,
            tracks = self.tracks.len(),
            "demux: no track row describes this packet's stream; passing it and any further \
             such packet by, without repeating this line",
          );
        }
        continue;
      };
      let track = TrackIndex::new(index);
      let time_base = info.timebase();

      // **Timing is observed before the payload conversion.** It is a
      // property of the packet libavformat produced, and a timed packet
      // with no payload is still a real endpoint — a later one is the
      // end of the track — so the end is folded in here, ahead of
      // whatever the conversion below decides about the bytes. A packet
      // with no usable `pts` moves no end but is marked here too: the
      // track's end is a lower bound from the moment such a packet is
      // read, whether the conversion below delivers it, refuses it or
      // parks it, and whether or not it carries a payload. Only the four
      // timed kinds are measured: an attachment is off the timeline, and
      // an unknown track breaks the walk in its own arm. A replayed
      // packet is observed again, which a running maximum and a mark
      // that is already made do not mind.
      if matches!(
        info.kind(),
        TrackKind::Video | TrackKind::Audio | TrackKind::Subtitle | TrackKind::Data
      ) {
        self
          .measured
          .observe(index, packet.pts(), packet.duration());
      }

      // A payload that is there and cannot be referenced is an error,
      // never a silently dropped packet: `Ok(None)` below means the
      // packet carried nothing, and that is the only thing that reads
      // the next one.
      // **Everything this loop delivers is demux-delivered**, whatever
      // its refcount: libavformat just handed it over, so any other
      // reference to its buffer is libavformat's own and no
      // `ffmpeg_next::Packet` wraps one. That is not the hazard a
      // caller's second handle is — see
      // [`crate::buffer::PayloadProvenance`].
      //
      // Sharing is ordinary here. A queue-backed demuxer — SubRip,
      // SubViewer and the rest of the `FFDemuxSubtitlesQueue` family —
      // keeps its parsed cues and delivers `av_packet_ref`s of them,
      // so *every* packet it produces arrives with two references.
      //
      // The one sub-case that is stronger still is the container's
      // parked picture, which a stream carrying
      // `ATTACHED_PIC | TIMED_THUMBNAILS` delivers as its first packet:
      // written once while the container opened, so the view lane may
      // window it rather than copy. See [`is_streams_attached_pic`] for
      // the identity proof.
      //
      // SAFETY: both the session's `AVFormatContext` and `packet` are
      // live here.
      let provenance = match parked_provenance {
        // Observed when this packet was delivered, and kept with it.
        Some(provenance) => provenance,
        None if unsafe { is_streams_attached_pic(&self.input, index, &packet) } => {
          crate::buffer::PayloadProvenance::AttachedPicture
        }
        None => crate::buffer::PayloadProvenance::DemuxDelivered,
      };

      // The packet this loop just read is **handed over**, not lent: the
      // view lane's carrier is a window into its buffer, and a source
      // that survived the conversion would be a mutable alias of it.
      // Exactly one arm runs, so exactly one move happens.
      // **The conversion borrows what this session owns.** The packet
      // stays in hand until the carrier exists, which is what lets a
      // failure park it instead of dropping it; on success it falls out
      // of scope at the end of the iteration and the carrier keeps its
      // buffer alive by refcount, exactly as when the conversion
      // consumed it. Nothing outside this loop ever sees the packet, so
      // the borrow cannot become the aliasing shape the public faces
      // refuse.
      //
      // SAFETY (each of the four arms): `packet` came out of
      // `av_read_frame`, so libavformat wrote its payload and its side
      // data — every byte it exports. `provenance` is this loop's own
      // reading of `packet` — `DemuxDelivered` for what `av_read_frame`
      // just handed a session that holds its `AVFormatContext` through
      // `&mut self`, so every other reference to the buffer is
      // libavformat's and none is written while the conversion reads;
      // `AttachedPicture` only where `is_streams_attached_pic` proved the
      // identity; a parked packet keeps the reading made when it was
      // delivered. Either way libavformat delivered the packet, with its
      // padding behind the payload, which is what lets the view lane's
      // carrier claim that padding and a decoder share it. `packet` is
      // this loop's own value and is never written through: it drops at
      // the end of the iteration, or waits in `self.unconverted` to be
      // read again.
      let converted = match info.kind() {
        TrackKind::Video => unsafe {
          boundary::video_packet_from_borrowed::<C>(
            &packet,
            time_base,
            self.limits.packet(),
            provenance,
          )
        }
        .map(|built| {
          built.map(|p| {
            let p = marked_from_bitstream(self.bitstreams.get_mut(index), p);
            DemuxedPacket::Video(VideoTrackPacket::new(track, p))
          })
        }),
        TrackKind::Audio => unsafe {
          boundary::audio_packet_from_borrowed::<C>(
            &packet,
            time_base,
            self.limits.packet(),
            provenance,
          )
        }
        .map(|built| built.map(|p| DemuxedPacket::Audio(AudioTrackPacket::new(track, p)))),
        TrackKind::Subtitle => unsafe {
          boundary::subtitle_packet_from_borrowed::<C>(
            &packet,
            time_base,
            self.limits.packet(),
            provenance,
          )
        }
        .map(|built| built.map(|p| DemuxedPacket::Subtitle(SubtitleTrackPacket::new(track, p)))),
        TrackKind::Data => unsafe {
          boundary::data_packet_from_borrowed::<C>(
            &packet,
            time_base,
            self.limits.packet(),
            provenance,
          )
        }
        .map(|built| built.map(|p| DemuxedPacket::Data(DataTrackPacket::new(track, p)))),
        // Every attachment track's one packet was queued at open time,
        // so anything arriving on one now is the duplicate some
        // demuxers emit for cover art. Drop it — the contract is
        // exactly one, and the one has already left. Nothing is
        // converted here, so there is nothing to park, and nothing is
        // lost, so the walk stays complete.
        TrackKind::Attachment => continue,
        // The roster of arms is five; a track nothing can name has no
        // arm and its packets are not delivered — which is data read
        // and passed by, so the walk breaks.
        TrackKind::Unknown => {
          self.measured.break_walk();
          continue;
        }
      };

      let built = match converted {
        Ok(built) => built,
        Err(source) => {
          // **Park a refusal that another attempt could survive.** An
          // allocation that failed says nothing about the packet, and
          // the packet is off the wire either way. Anything else is a
          // fact about the packet itself — a malformed one is not made
          // well-formed by retrying, and parking it would answer every
          // later pull with the same error instead of letting the
          // session make progress.
          if parks_in_demux(&source) {
            self.unconverted = Some((packet, provenance));
          } else {
            // Dropped: this walk will not have observed every packet,
            // so it can no longer call its figures complete.
            self.measured.break_walk();
          }
          return Err(DemuxError::PacketBuffer(PacketBuffer::new(index, source)));
        }
      };

      // `None` here means the packet carried no payload — an empty
      // packet, which some demuxers emit as a marker. Nothing to
      // deliver; its timing, if it had any, was observed above. Read the
      // next one.
      if let Some(out) = built {
        return Ok(Some(out));
      }
    }
  }

  pub(crate) fn seek_impl(&mut self, target: Timestamp) -> Result<(), DemuxError> {
    // Any seek ends the claim to be one unbroken walk — a failed one
    // too, since where the container sits afterwards is unspecified.
    self.measured.break_walk();
    // So every reading of a bitstream hears of it, for the same reason:
    // the packets after it may meet a decoder that kept its framing or
    // one opened afresh.
    sought(&mut self.bitstreams);
    let ts = target.rescale_to(av_time_base_q()).pts();
    // Only our own EOF latch is cleared, and only before the seek —
    // the seek machinery gates on `eof_reached`, so clearing it
    // afterwards would be too late.
    if self.eof {
      self.input.clear_eof();
      self.eof = false;
    }
    // `..ts` is how ffmpeg-next spells the seek window: it reads only
    // the endpoint, and `avformat_seek_file`'s `max_ts` is inclusive,
    // so the window is `[i64::MIN, ts]`. FFmpeg picks the closest seek
    // point inside it — the nearest keyframe at or before the target.
    // Never after: a decoder started past the target has no reference
    // frame.
    let sought = self.input.seek(ts, ..ts);
    if let Some(panicked) = self.panicked() {
      return Err(panicked);
    }
    sought?;
    // **The seat is cleared by a seek that happened, not by one that
    // was attempted.** A parked packet belongs to the position the
    // session is leaving, so a successful seek discards it. A *failed*
    // one leaves the session where it was — and that packet is off the
    // wire, so dropping it here would be the same silent loss the seat
    // exists to prevent, with no re-read able to recover it.
    //
    // FFmpeg does not specify where a container sits after a seek that
    // returned an error, and this crate does not guess: it keeps a
    // packet the container really did deliver, and a caller who saw the
    // seek fail already knows the position is not the one they asked
    // for. Every timestamp needed to tell is on the packet.
    self.unconverted = None;
    Ok(())
  }
}

macro_rules! demuxer_lane_face {
  ($($lane:ty),+ $(,)?) => { $(
    impl CarrierDemuxer<$lane> {
      /// Opens a container from a filesystem path.
      ///
      /// Runs `avformat_open_input` followed by
      /// `avformat_find_stream_info`, then builds the track table and
      /// captures every attachment payload.
      ///
      /// Call [`ffmpeg_next::init`] once before the first open if you
      /// want FFmpeg's logging and network protocols configured;
      /// probing a local container does not require it.
      pub fn open<P: AsRef<Path> + ?Sized>(path: &P) -> Result<Self, DemuxError> {
        Self::open_impl(path)
      }

      /// [`Self::open`], with the session's resource budgets named.
      ///
      /// The budgets are taken **at open** rather than through a
      /// `with_*` builder because the attachment half of them is spent
      /// here: every attachment payload is captured during this call.
      pub fn open_with<P: AsRef<Path> + ?Sized>(
        path: &P,
        limits: DemuxLimits,
      ) -> Result<Self, DemuxError> {
        Self::open_with_impl(path, limits)
      }

      /// Opens a container from any `Read + Seek` source.
      pub fn open_reader<R: Read + Seek + Send + 'static>(
        reader: R,
        url: Option<&str>,
      ) -> Result<Self, DemuxError> {
        Self::open_reader_impl(reader, url)
      }

      /// [`Self::open_reader`], with the session's budgets named.
      pub fn open_reader_with<R: Read + Seek + Send + 'static>(
        reader: R,
        url: Option<&str>,
        limits: DemuxLimits,
      ) -> Result<Self, DemuxError> {
        Self::open_reader_with_impl(reader, url, limits)
      }

      /// The wrapped `AVFormatContext`.
      pub const fn input(&self) -> &Input {
        self.input_impl()
      }

      /// The budgets this session was opened with.
      pub const fn limits(&self) -> DemuxLimits {
        self.limits_impl()
      }

      /// **What the container IS**, as libavformat identified it from
      /// the bytes — the demuxer it chose, with the short names that
      /// demuxer handles and its description.
      ///
      /// Decided during the open and fixed for the life of the
      /// session, so this answers the same thing at any point and
      /// costs nothing to ask.
      ///
      /// `None` only where libavformat left `iformat` null or its name
      /// is not readable text; neither happens on a session that
      /// opened successfully.
      ///
      /// **Nothing here looked at a path.** A file's extension is a
      /// claim about its bytes, and this is a reading of them — which
      /// is what makes the answer usable on a content-addressed row,
      /// where the same bytes under two names are one content. See
      /// [`ContainerFormat`](crate::ContainerFormat) for what the
      /// demuxer's name does and does not narrow to.
      pub const fn format(&self) -> Option<&crate::ContainerFormat> {
        self.format_impl()
      }
    }

    impl Demuxer for CarrierDemuxer<$lane> {
      type Adapter = Ffmpeg;
      type Buffer = <$lane as crate::FfmpegCarrier>::Buffer;
      type TrackHandle = TrackArc<TrackInfo<Ffmpeg>>;
      type Error = DemuxError;

      /// The track table, held for the life of the session.
      ///
      /// A row's `duration` is libavformat's figure for the stream as it
      /// stood after `avformat_open_input` and before
      /// `avformat_find_stream_info`. It is no claim about the file — a
      /// format's reader may compute it itself — and a value that
      /// appears only after probing is libavformat's fill, which hands a
      /// stream that supplied no timing the container's length, so it is
      /// not carried; neither is one for a stream libavformat creates
      /// during probing. The last packet end this backend observed on
      /// the track's timeline is [`measured_end`](Demuxer::measured_end);
      /// a length needs the track's start and its presentation edits
      /// applied, which is the composer's job.
      ///
      /// Reading the table takes nothing away — clone the handles worth
      /// keeping. `Arc` is the carrier because
      /// [`CodecTicket`](crate::ticket::CodecTicket) mirrors an
      /// `AVCodecParameters` into owned Rust, which is what makes a
      /// row `Send + Sync` and a table shareable across tasks.
      fn tracks(&self) -> &[TrackArc<TrackInfo<Ffmpeg>>] {
        self.tracks_impl()
      }

      /// The container's chapter table, in the order
      /// `AVFormatContext.chapters` holds it, mirrored at open and
      /// held for the life of the session.
      ///
      /// Empty where the file declares no chapters — which is the
      /// provided answer too, so the override changes nothing for a
      /// container that has none.
      ///
      /// **Nothing is repaired.** A chapter whose `end` precedes its
      /// `start`, and one whose end libavformat left at its
      /// no-timestamp sentinel because the file declared none, are
      /// mirrored exactly as written; see [`Chapter`] for why this
      /// layer reports rather than clamps.
      fn chapters(&self) -> &[Chapter<Ffmpeg>] {
        self.chapters_impl()
      }

      /// The container-level duration with libavformat's account of it,
      /// from `AVFormatContext.duration` and
      /// `AVFormatContext.duration_estimation_method`, read once at
      /// open and held for the life of the session.
      ///
      /// The container's figure, never a track's: Matroska reports a
      /// length here and none on any track, and the tracks' rows keep
      /// saying so. `None` where libavformat holds no positive figure.
      ///
      /// The figure is counted in `AV_TIME_BASE` ticks — microseconds —
      /// and carried in that timebase, not rescaled. The account is
      /// libavformat's own, word for word: `FROM_PTS` is
      /// [`FromPts`](DurationSource::FromPts), `FROM_STREAM` is
      /// [`FromStream`](DurationSource::FromStream), `FROM_BITRATE` is
      /// [`FromBitrate`](DurationSource::FromBitrate), and a method this
      /// build does not name is [`Unknown`](DurationSource::Unknown).
      fn duration(&self) -> Option<ContainerDuration> {
        self.duration_impl()
      }

      /// Where `track`'s packets ended, as this session's walk measured
      /// it: the greatest `pts + duration` over the timed packets read
      /// on it, in the track's own timebase.
      ///
      /// The figure is an **endpoint on the track's own timeline, not a
      /// length**. The container's duration is a length, and the two
      /// are different quantities: a track that starts at ten seconds
      /// and runs for two ends at twelve. Turning an end into a length
      /// means subtracting the track's start — the backend's
      /// [`TrackExtra::start_time`](crate::extras::TrackExtra::start_time)
      /// is `AVStream.start_time` as it stands after probing, which
      /// libavformat may itself have filled in — and honouring
      /// presentation edits, which is the composer's job and not this
      /// crate's. The figure's own promise is only that it is exact when
      /// [`MeasuredEnd::exact`] says so, and covers what
      /// [`MeasuredEnd::walk_complete`] says.
      ///
      /// A packet that carries no duration contributes its `pts`
      /// alone; libavformat derives a duration from the stream's frame
      /// rate or frame size for most packets whose demuxer wrote none,
      /// so that is the rare case. A packet with no `pts` moves no end.
      /// A timed packet with no payload still counts: it is timed
      /// before the payload conversion runs, because it is a real
      /// endpoint and a later one is the end. Nothing is read for this
      /// and nothing is sought: the figure rides the pulls the caller
      /// makes.
      ///
      /// **Exactness.** [`MeasuredEnd::exact`] turns `false`, for good,
      /// the first time a timed packet on the track is read with a
      /// duration that is not positive (libavformat writes zero for one
      /// it does not know) or a packet on it is read with no usable
      /// `pts`. The mark is made when the packet is read, before the
      /// payload conversion, so it does not depend on whether the packet
      /// is then delivered, refused or parked, nor on whether it carries
      /// a payload. A packet with no usable duration advances the end to
      /// its `pts` only; either way the figure is a lower bound. A
      /// duration libavformat derived from the stream's frame rate or
      /// frame size counts as a duration, and is only as exact as that
      /// derivation.
      ///
      /// **Completeness.** [`MeasuredEnd::walk_complete`] holds once a
      /// walk that skipped nothing has answered `Ok(None)`, and a
      /// complete walk is frozen: a later read or seek neither moves
      /// its figures nor takes the flag back. The walk stops being
      /// complete for good, before it completes, at a
      /// [`seek`](Demuxer::seek) and at every packet `av_read_frame`
      /// produced that this session did not observe: refused and
      /// dropped, skipped as corrupt (`AVERROR_INVALIDDATA`), or on a
      /// stream the table never described — which is what a container
      /// that adds streams after open (MPEG-TS, RTP) produces, so a
      /// session that read a packet on a grown stream never answers
      /// complete. Accounting for such streams as they appear is not
      /// done here. A cover picture's duplicate packet, whose one
      /// delivery happened at open, and an untimed empty packet lose
      /// nothing and do not break the walk (the untimed one still costs
      /// the track its exactness). After a break the
      /// figure keeps rising as a "so far" maximum and stays a lower
      /// bound.
      ///
      /// **What it does not cover.** libavformat probes while the
      /// container opens, before this session's first read. It buffers
      /// the packets it reads and replays them, but a read error it
      /// swallows during probing is not exposed anywhere, so data
      /// skipped there is invisible and a complete walk does not vouch
      /// for it.
      ///
      /// An attachment track answers `None`, and so does a track whose
      /// end does not fit in an `i64`, which breaks the walk as well.
      fn measured_end(&self, track: TrackIndex) -> Option<MeasuredEnd> {
        self.measured_end_impl(track)
      }

      /// Pulls the next packet.
      ///
      /// **A refusal that another attempt could survive costs no
      /// packet.** `av_read_frame` advances the container, so a
      /// conversion that then fails on an allocation would otherwise
      /// drop bytes nothing can ask for again. Such a packet is parked
      /// instead, and this method re-attempts *it* before reading
      /// another — so a caller who pulls again loses nothing. A refusal
      /// about the packet itself is not parked: retrying a malformed
      /// packet forever would be worse than passing it by.
      fn next_packet(
        &mut self,
      ) -> Result<Option<DemuxedPacket<Ffmpeg, Self::Buffer>>, DemuxError> {
        self.next_packet_impl()
      }

      fn seek(&mut self, target: Timestamp) -> Result<(), DemuxError> {
        self.seek_impl(target)
      }
    }
  )+ };
}

demuxer_lane_face!(crate::View, crate::Owned);

/// Payload for [`DemuxError::AttachmentTooLarge`].
///
/// One attachment's payload exceeds
/// [`DemuxLimits::max_attachment_bytes`].
#[derive(thiserror::Error, Debug, Clone, Copy, PartialEq, Eq)]
#[error(
  "the attachment on stream {stream_index} is {bytes} bytes, over the {limit}-byte per-attachment budget"
)]
pub struct AttachmentTooLarge {
  stream_index: usize,
  bytes: usize,
  limit: usize,
}

impl AttachmentTooLarge {
  /// Constructs an `AttachmentTooLarge` payload.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(stream_index: usize, bytes: usize, limit: usize) -> Self {
    Self {
      stream_index,
      bytes,
      limit,
    }
  }
  /// The `AVStream.index` carrying the oversized attachment.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn stream_index(&self) -> usize {
    self.stream_index
  }
  /// The attachment's payload length.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn bytes(&self) -> usize {
    self.bytes
  }
  /// The per-attachment budget in force.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn limit(&self) -> usize {
    self.limit
  }
}

/// Payload for [`DemuxError::AttachmentBudgetExhausted`].
///
/// The file's attachments, together, exceed
/// [`DemuxLimits::max_total_attachment_bytes`].
///
/// Separate from [`AttachmentTooLarge`] because it is a different
/// attack: every attachment can be modest and there can still be four
/// hundred of them. This arm names the track that ran the total past
/// the line, not the track that was individually at fault — there
/// need not be one.
#[derive(thiserror::Error, Debug, Clone, Copy, PartialEq, Eq)]
#[error(
  "the attachment on stream {stream_index} brings the file's attachments to {total} bytes, over the {limit}-byte budget"
)]
pub struct AttachmentBudgetExhausted {
  stream_index: usize,
  total: usize,
  limit: usize,
}

impl AttachmentBudgetExhausted {
  /// Constructs an `AttachmentBudgetExhausted` payload.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(stream_index: usize, total: usize, limit: usize) -> Self {
    Self {
      stream_index,
      total,
      limit,
    }
  }
  /// The `AVStream.index` whose attachment crossed the line.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn stream_index(&self) -> usize {
    self.stream_index
  }
  /// The running total, including this attachment.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn total(&self) -> usize {
    self.total
  }
  /// The whole-file budget in force.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn limit(&self) -> usize {
    self.limit
  }
}

/// Payload for [`DemuxError::ParametersBudgetExhausted`].
///
/// Every stream's codec parameters, together, hold more heap bytes than
/// [`DemuxLimits::max_total_codec_parameter_bytes`] allows.
///
/// A separate attack from [`ParametersTooLarge`], and separate for the
/// same reason the attachment pair are: each stream's parameters can be
/// individually modest and a container can still declare two hundred
/// streams. The arm names the stream that ran the total past the line,
/// which need not be one that was individually at fault.
#[derive(thiserror::Error, Debug, Clone, Copy, PartialEq, Eq)]
#[error(
  "the codec parameters on stream {stream_index} bring the file's to {total} heap bytes, over the {limit}-byte budget"
)]
pub struct ParametersBudgetExhausted {
  stream_index: usize,
  total: usize,
  limit: usize,
}

impl ParametersBudgetExhausted {
  /// Constructs a `ParametersBudgetExhausted` payload.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(stream_index: usize, total: usize, limit: usize) -> Self {
    Self {
      stream_index,
      total,
      limit,
    }
  }
  /// The `AVStream.index` whose parameters crossed the line.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn stream_index(&self) -> usize {
    self.stream_index
  }
  /// The running total, including this stream.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn total(&self) -> usize {
    self.total
  }
  /// The whole-file budget in force.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn limit(&self) -> usize {
    self.limit
  }
}

/// Payload for [`DemuxError::TrackTimebaseInvalid`].
///
/// A stream declares an `AVRational` timebase that is not a
/// [`Timebase`]: a zero or negative denominator, or a negative
/// numerator.
///
/// **Refused rather than repaired.** The value is what every timestamp
/// on that track would be measured against, and there is no honest
/// substitute — a ruler invented here is indistinguishable downstream
/// from one the file declared. `0/1`, libavformat's own "not set yet"
/// default, is **not** this error: see
/// [`TrackInfo::timebase`](mediadecode::demuxer::TrackInfo::timebase).
#[derive(thiserror::Error, Debug, Clone, Copy, PartialEq, Eq)]
#[error("stream {stream_index} declares the timebase {num}/{den}, which is not a usable one")]
pub struct TrackTimebaseInvalid {
  stream_index: usize,
  num: i32,
  den: i32,
}

impl TrackTimebaseInvalid {
  /// Constructs a `TrackTimebaseInvalid` payload.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(stream_index: usize, num: i32, den: i32) -> Self {
    Self {
      stream_index,
      num,
      den,
    }
  }
  /// The `AVStream.index` that declared it.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn stream_index(&self) -> usize {
    self.stream_index
  }
  /// The numerator the container wrote.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn num(&self) -> i32 {
    self.num
  }
  /// The denominator the container wrote.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn den(&self) -> i32 {
    self.den
  }
}

/// Payload for [`DemuxError::ChapterTimebaseInvalid`].
///
/// A chapter declares an `AVRational` timebase that cannot rule its
/// span: a zero or negative denominator, a negative numerator, or a
/// **zero** numerator.
///
/// Stricter than [`TrackTimebaseInvalid`] by that last case, and
/// deliberately: a chapter's ruler is written by whatever wrote the
/// chapter, so `0/den` there is not an absence but a claim that every
/// boundary in the table falls on one instant.
///
/// **The whole open fails, rather than the row being dropped.** A
/// chapter table is a table: a reader that quietly returned the other
/// eleven rows would be handing a consumer something that disagrees
/// with the file and says nothing about it. This names the row, its
/// container id and the rational, so a caller learns exactly what the
/// file wrote — and repairing a container is a job for something that
/// rewrites containers, not for a reader that would have to invent the
/// number it repaired with.
#[derive(thiserror::Error, Debug, Clone, Copy, PartialEq, Eq)]
#[error(
  "chapter {index} (container id {id}) declares the timebase {num}/{den}, which cannot rule its span"
)]
pub struct ChapterTimebaseInvalid {
  index: usize,
  id: i64,
  num: i32,
  den: i32,
}

impl ChapterTimebaseInvalid {
  /// Constructs a `ChapterTimebaseInvalid` payload.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(index: usize, id: i64, num: i32, den: i32) -> Self {
    Self {
      index,
      id,
      num,
      den,
    }
  }
  /// The chapter's position in `AVFormatContext.chapters`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn index(&self) -> usize {
    self.index
  }
  /// The id the container assigned that chapter.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn id(&self) -> i64 {
    self.id
  }
  /// The numerator the container wrote.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn num(&self) -> i32 {
    self.num
  }
  /// The denominator the container wrote.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn den(&self) -> i32 {
    self.den
  }
}

/// Payload for [`DemuxError::TooManyChapters`].
///
/// The container declares more chapters than
/// [`DemuxLimits::max_chapters`] allows. Refused at open, before the
/// table is reserved.
#[derive(thiserror::Error, Debug, Clone, Copy, PartialEq, Eq)]
#[error("the container declares {declared} chapters, over the ceiling of {limit}")]
pub struct TooManyChapters {
  declared: usize,
  limit: u32,
}

impl TooManyChapters {
  /// Constructs a `TooManyChapters` payload.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(declared: usize, limit: u32) -> Self {
    Self { declared, limit }
  }
  /// `AVFormatContext.nb_chapters`, as the container declared it.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn declared(&self) -> usize {
    self.declared
  }
  /// The ceiling in force.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn limit(&self) -> u32 {
    self.limit
  }
}

/// Payload for [`DemuxError::ChapterTitleBudgetExhausted`].
///
/// The file's chapter titles, together, are over
/// [`DemuxLimits::max_total_chapter_title_bytes`].
#[derive(thiserror::Error, Debug, Clone, Copy, PartialEq, Eq)]
#[error(
  "the chapter titles reach {bytes} bytes at chapter {index}, over the {limit}-byte whole-file budget"
)]
pub struct ChapterTitleBudgetExhausted {
  index: usize,
  bytes: usize,
  limit: usize,
}

impl ChapterTitleBudgetExhausted {
  /// Constructs a `ChapterTitleBudgetExhausted` payload.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(index: usize, bytes: usize, limit: usize) -> Self {
    Self {
      index,
      bytes,
      limit,
    }
  }
  /// The chapter whose title ran the total past the line.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn index(&self) -> usize {
    self.index
  }
  /// The running total at that chapter.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn bytes(&self) -> usize {
    self.bytes
  }
  /// The whole-file budget in force.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn limit(&self) -> usize {
    self.limit
  }
}

/// Payload for [`DemuxError::ChapterTitleTooLong`].
///
/// A chapter's `title` has no terminator inside
/// [`METADATA_VALUE_MAX_BYTES`].
///
/// **Distinct from the budget error, and deliberately so.**
/// [`ChapterTitleBudgetExhausted`] means the file's titles together
/// exceed what the caller allowed, and a caller answers it by raising
/// [`DemuxLimits::max_total_chapter_title_bytes`](crate::DemuxLimits::max_total_chapter_title_bytes).
/// This one means a single value runs past a structural cap this crate
/// owns and no seat can move — folding the two together would offer a
/// knob that cannot fix it.
///
/// Refused rather than truncated (a truncated title is a different
/// title), and refused *visibly*: reporting it as an absent title, as
/// this road used to, made the mirrored table silently disagree with
/// the container and slipped the value past the budget entirely.
#[derive(thiserror::Error, Debug, Clone, Copy, PartialEq, Eq)]
#[error("the title on chapter {index} runs past the {limit}-byte cap on one metadata value")]
pub struct ChapterTitleTooLong {
  index: usize,
  limit: usize,
}

impl ChapterTitleTooLong {
  /// Constructs a `ChapterTitleTooLong` payload.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(index: usize, limit: usize) -> Self {
    Self { index, limit }
  }
  /// The chapter's position in `AVFormatContext.chapters`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn index(&self) -> usize {
    self.index
  }
  /// The per-value cap in force.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn limit(&self) -> usize {
    self.limit
  }
}

/// Payload for [`DemuxError::ChapterTitleAlloc`].
///
/// A chapter title that passed the budget could not be decoded into
/// owned text. The size had already been charged, so this is the
/// allocator refusing rather than the file asking for too much.
#[derive(thiserror::Error, Debug, Clone, Copy, PartialEq, Eq)]
#[error("out of memory decoding the {bytes}-byte title on chapter {index}")]
pub struct ChapterTitleAlloc {
  index: usize,
  bytes: usize,
}

impl ChapterTitleAlloc {
  /// Constructs a `ChapterTitleAlloc` payload.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(index: usize, bytes: usize) -> Self {
    Self { index, bytes }
  }
  /// The chapter's position in `AVFormatContext.chapters`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn index(&self) -> usize {
    self.index
  }
  /// The decoded size that could not be reserved.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn bytes(&self) -> usize {
    self.bytes
  }
}

/// Payload for [`DemuxError::TrackMetadataTooLong`].
///
/// One of a stream's retained metadata values — `filename`,
/// `mimetype` or `language` — has no terminator inside
/// [`METADATA_VALUE_MAX_BYTES`]. `key` says which.
#[derive(thiserror::Error, Debug, Clone, Copy, PartialEq, Eq)]
#[error("the {key} on stream {stream_index} runs past the {limit}-byte cap on one metadata value")]
pub struct TrackMetadataTooLong {
  stream_index: usize,
  key: &'static str,
  limit: usize,
}

impl TrackMetadataTooLong {
  /// Constructs a `TrackMetadataTooLong` payload.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(stream_index: usize, key: &'static str, limit: usize) -> Self {
    Self {
      stream_index,
      key,
      limit,
    }
  }
  /// The `AVStream.index` carrying it.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn stream_index(&self) -> usize {
    self.stream_index
  }
  /// The dictionary key that was being read.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn key(&self) -> &'static str {
    self.key
  }
  /// The per-value cap in force.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn limit(&self) -> usize {
    self.limit
  }
}

/// Payload for [`DemuxError::TrackMetadataBudgetExhausted`].
///
/// The file's stream metadata, together, is over
/// [`DemuxLimits::max_total_stream_metadata_bytes`].
///
/// The budget is whole-file rather than per-stream because the
/// exposure is: `max_streams` bounds how many streams a header may
/// declare and says nothing about what each may carry, and every
/// admitted stream's three values are mirrored eagerly at open.
#[derive(thiserror::Error, Debug, Clone, Copy, PartialEq, Eq)]
#[error(
  "the stream metadata reaches {bytes} bytes at the {key} on stream {stream_index}, over the {limit}-byte whole-file budget"
)]
pub struct TrackMetadataBudgetExhausted {
  stream_index: usize,
  key: &'static str,
  bytes: usize,
  limit: usize,
}

impl TrackMetadataBudgetExhausted {
  /// Constructs a `TrackMetadataBudgetExhausted` payload.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(stream_index: usize, key: &'static str, bytes: usize, limit: usize) -> Self {
    Self {
      stream_index,
      key,
      bytes,
      limit,
    }
  }
  /// The `AVStream.index` whose value ran the total past the line.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn stream_index(&self) -> usize {
    self.stream_index
  }
  /// The dictionary key that was being read.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn key(&self) -> &'static str {
    self.key
  }
  /// The running total at that value.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn bytes(&self) -> usize {
    self.bytes
  }
  /// The whole-file budget in force.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn limit(&self) -> usize {
    self.limit
  }
}

/// Payload for [`DemuxError::TrackMetadataAlloc`].
///
/// A stream metadata value that passed the budget could not be decoded
/// into owned text.
#[derive(thiserror::Error, Debug, Clone, Copy, PartialEq, Eq)]
#[error("out of memory decoding the {bytes}-byte {key} on stream {stream_index}")]
pub struct TrackMetadataAlloc {
  stream_index: usize,
  key: &'static str,
  bytes: usize,
}

impl TrackMetadataAlloc {
  /// Constructs a `TrackMetadataAlloc` payload.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(stream_index: usize, key: &'static str, bytes: usize) -> Self {
    Self {
      stream_index,
      key,
      bytes,
    }
  }
  /// The `AVStream.index` carrying it.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn stream_index(&self) -> usize {
    self.stream_index
  }
  /// The dictionary key that was being read.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn key(&self) -> &'static str {
    self.key
  }
  /// The decoded size that could not be reserved.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn bytes(&self) -> usize {
    self.bytes
  }
}

/// Payload for [`DemuxError::TrackTableAlloc`].
///
/// The track table, or the attachment queue or the per-track
/// measurement built beside it, could not be reserved. The stream count
/// had already passed
/// [`DemuxLimits::max_streams`](crate::DemuxLimits::max_streams), so
/// this is the allocator declining rather than the file asking for too
/// much — reported instead of aborting, which is what an infallible
/// reservation would have done.
#[derive(thiserror::Error, Debug, Clone, Copy, PartialEq, Eq)]
#[error("out of memory reserving the table of {streams} streams")]
pub struct TrackTableAlloc {
  streams: usize,
}

impl TrackTableAlloc {
  /// Constructs a `TrackTableAlloc` payload.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(streams: usize) -> Self {
    Self { streams }
  }
  /// The stream count the reservation was for.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn streams(&self) -> usize {
    self.streams
  }
}

/// Payload for [`DemuxError::ChapterAlloc`].
///
/// The chapter table could not be reserved. The count had already
/// passed [`DemuxLimits::max_chapters`], so this is the allocator
/// refusing rather than the file asking for too much — reported
/// instead of aborting, which is what an infallible reservation would
/// have done.
#[derive(thiserror::Error, Debug, Clone, Copy, PartialEq, Eq)]
#[error("out of memory reserving the table of {declared} chapters")]
pub struct ChapterAlloc {
  declared: usize,
}

impl ChapterAlloc {
  /// Constructs a `ChapterAlloc` payload.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(declared: usize) -> Self {
    Self { declared }
  }
  /// The chapter count the reservation was for.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn declared(&self) -> usize {
    self.declared
  }
}

/// Payload for [`DemuxError::PacketBuffer`].
///
/// A packet's payload could not be referenced — the bytes are there
/// and this layer could not carry them.
///
/// Never raised for a packet that simply has no payload: an empty
/// packet is a marker some demuxers emit, and it is skipped in
/// silence. Distinguishing the two is what keeps a refcount failure
/// under memory pressure from looking like the file's own word and
/// dropping real compressed bytes.
#[derive(thiserror::Error, Debug, Clone)]
#[error("stream {stream_index}: {source}")]
pub struct PacketBuffer {
  stream_index: usize,
  #[source]
  source: PacketBufferError,
}

impl PacketBuffer {
  /// Constructs a `PacketBuffer` payload.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(stream_index: usize, source: PacketBufferError) -> Self {
    Self {
      stream_index,
      source,
    }
  }
  /// The `AVStream.index` the packet belongs to.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn stream_index(&self) -> usize {
    self.stream_index
  }
  /// What went wrong.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn source(&self) -> &PacketBufferError {
    &self.source
  }
}

/// Payload for [`DemuxError::ReaderPanic`].
///
/// The `Read + Seek` source given to [`FfmpegDemuxer::open_reader`]
/// panicked inside a libavformat callback.
///
/// The panic was caught before it could cross the `extern "C"`
/// boundary and abort the process; this is what it said. The session
/// is terminal — every later call reports the same panic.
#[derive(thiserror::Error, Debug, Clone)]
#[error("the reader panicked: {message}")]
pub struct ReaderPanic {
  message: Utf8Bytes,
}

impl ReaderPanic {
  /// Constructs a `ReaderPanic` payload.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(message: Utf8Bytes) -> Self {
    Self { message }
  }
  /// What the panic payload said.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn message(&self) -> &str {
    self.message.as_str()
  }
}

/// Errors from [`FfmpegDemuxer`].
///
/// **Open fault taxonomy, so it is `#[non_exhaustive]`.** New ways to
/// fail are discovered — a backend, a ceiling, a corruption a codec
/// learns to report — and a consumer that meets one it has never heard
/// of should take its generic-fault path. That is exactly what the
/// wildcard arm this attribute forces is for. The two status
/// vocabularies opposite it,
/// [`Sent`](mediadecode::Sent) and [`Received`](mediadecode::Received),
/// are exhaustive for the mirror-image reason: their arms are the
/// substrate's fixed state set, and there the wildcard would be dead
/// weight hiding a state a consumer forgot.
#[derive(thiserror::Error, Debug, Clone, IsVariant, Unwrap, TryUnwrap)]
#[unwrap(ref, ref_mut)]
#[try_unwrap(ref, ref_mut)]
#[non_exhaustive]
pub enum DemuxError {
  /// The wrapped libavformat call reported an error — open, read or
  /// seek.
  #[error(transparent)]
  Ffmpeg(#[from] ffmpeg_next::Error),

  /// libavformat asked for more bytes than the probe budget allows
  /// while opening and analysing the container. See
  /// [`ProbeBudgetExhausted`].
  #[error(transparent)]
  ProbeBudgetExhausted(#[from] ProbeBudgetExhausted),

  /// One attachment's payload is over the per-attachment budget.
  /// Refused at open, before the copy.
  #[error(transparent)]
  AttachmentTooLarge(#[from] AttachmentTooLarge),

  /// The file's attachments, together, are over the whole-file budget.
  /// Refused at open, before the copy that would have crossed it.
  #[error(transparent)]
  AttachmentBudgetExhausted(#[from] AttachmentBudgetExhausted),

  /// One stream's codec parameters hold more heap bytes than the
  /// budget allows. Refused at open, before the clone.
  #[error(transparent)]
  ParametersTooLarge(#[from] ParametersTooLarge),

  /// Every stream's codec parameters together are over the whole-file
  /// budget. Refused at open, before the clone that would have crossed
  /// it.
  #[error(transparent)]
  ParametersBudgetExhausted(#[from] ParametersBudgetExhausted),

  /// Codec parameters arrived that were never allocated.
  #[error(transparent)]
  ParametersMissing(#[from] ParametersMissing),

  /// Codec parameters for a track could not be allocated.
  #[error(transparent)]
  ParametersAlloc(#[from] ParametersAlloc),

  /// Copying a track's codec parameters failed part way.
  #[error(transparent)]
  ParametersCopy(#[from] ParametersCopy),

  /// A channel layout arrived carrying user-private data an owned
  /// codec ticket cannot mirror.
  #[error(transparent)]
  ParametersOpaque(#[from] ParametersOpaque),

  /// A channel layout declared a custom order without the map that
  /// order requires — a shape `av_channel_layout_copy` would `memcpy`
  /// from null.
  #[error(transparent)]
  ParametersChannelMap(#[from] ParametersChannelMap),

  /// A stream's channel layout declares a shape FFmpeg's own helpers
  /// cannot be given — see [`ParametersLayoutShape`].
  #[error(transparent)]
  ParametersLayoutShape(#[from] ParametersLayoutShape),

  /// A stream declares a timebase that is not one. Refused at open,
  /// rather than repaired with a number this crate would have had to
  /// invent.
  #[error(transparent)]
  TrackTimebaseInvalid(#[from] TrackTimebaseInvalid),

  /// A chapter declares a timebase that cannot rule its span. Refused
  /// at open, for the reason [`ChapterTimebaseInvalid`] gives.
  #[error(transparent)]
  ChapterTimebaseInvalid(#[from] ChapterTimebaseInvalid),

  /// The container declares more chapters than the ceiling allows.
  /// Refused at open, before the table is reserved.
  #[error(transparent)]
  TooManyChapters(#[from] TooManyChapters),

  /// The file's chapter titles, together, are over the whole-file
  /// budget. Refused at the title that crossed it, before it was
  /// copied.
  #[error(transparent)]
  ChapterTitleBudgetExhausted(#[from] ChapterTitleBudgetExhausted),

  /// One chapter title runs past the cap on a single metadata value —
  /// refused, rather than reported as an absent title.
  #[error(transparent)]
  ChapterTitleTooLong(#[from] ChapterTitleTooLong),

  /// A chapter title the budget admitted could not be decoded.
  #[error(transparent)]
  ChapterTitleAlloc(#[from] ChapterTitleAlloc),

  /// One of a stream's retained metadata values runs past the cap on a
  /// single metadata value.
  #[error(transparent)]
  TrackMetadataTooLong(#[from] TrackMetadataTooLong),

  /// The file's stream metadata, together, is over the whole-file
  /// budget. Refused at the value that crossed it, before it was
  /// copied.
  #[error(transparent)]
  TrackMetadataBudgetExhausted(#[from] TrackMetadataBudgetExhausted),

  /// A stream metadata value the budget admitted could not be decoded.
  #[error(transparent)]
  TrackMetadataAlloc(#[from] TrackMetadataAlloc),

  /// The chapter table could not be reserved.
  #[error(transparent)]
  ChapterAlloc(#[from] ChapterAlloc),

  /// The track table, or the attachment queue beside it, could not be
  /// reserved.
  #[error(transparent)]
  TrackTableAlloc(#[from] TrackTableAlloc),

  /// A packet's payload could not be referenced — the bytes are there
  /// and this layer could not carry them.
  #[error(transparent)]
  PacketBuffer(#[from] PacketBuffer),

  /// The `Read + Seek` source given to
  /// [`FfmpegDemuxer::open_reader`] panicked inside a libavformat
  /// callback.
  #[error(transparent)]
  ReaderPanic(#[from] ReaderPanic),
}

/// The base's parameter refusals, each reported under the arm that has
/// always named it here — the payload is the same value.
///
/// Exhaustive on purpose: [`ParametersError`] is closed, so an arm added
/// there fails to compile here until the demuxer names it too.
impl From<ParametersError> for DemuxError {
  fn from(e: ParametersError) -> Self {
    match e {
      ParametersError::TooLarge(p) => Self::ParametersTooLarge(p),
      ParametersError::Missing(p) => Self::ParametersMissing(p),
      ParametersError::Alloc(p) => Self::ParametersAlloc(p),
      ParametersError::Copy(p) => Self::ParametersCopy(p),
      ParametersError::Opaque(p) => Self::ParametersOpaque(p),
      ParametersError::ChannelMap(p) => Self::ParametersChannelMap(p),
      ParametersError::LayoutShape(p) => Self::ParametersLayoutShape(p),
    }
  }
}

/// Whether the demux session should **park** the packet a payload
/// refusal came from and re-attempt it on the next pull.
///
/// Named for the decision rather than for a property of the error, and
/// kept here rather than on `PacketBufferError`: whether retrying helps
/// depends on *what was retried*. `SharedPayload` is permanent for a
/// caller who keeps their other reference and retryable the moment they
/// drop it; `CaptureFailed` is worth another attempt only if the packet
/// still exists to attempt, which on a **consuming** conversion it does
/// not. Only the demux loop knows both halves — it still holds the
/// packet, and it knows nobody else does.
///
/// An allocation that failed says nothing about the packet, and the
/// demux loop is holding the bytes; everything else is a fact about the
/// packet itself, and parking it would answer every later pull with the
/// same error instead of letting the session make progress.
///
/// **The door left open:** a public retry signal would have to know
/// which operation produced the error and what the caller still holds —
/// an operation-aware answer, not a property of `PacketBufferError`. If
/// one is ever wanted it is designed then, not approximated now.
fn parks_in_demux(source: &PacketBufferError) -> bool {
  matches!(
    source,
    PacketBufferError::CaptureFailed(_) | PacketBufferError::SideDataAlloc(_)
  )
}

// ---------------------------------------------------------------------------
//  Track-table construction.
// ---------------------------------------------------------------------------

type BuiltTracks<C> = (
  Vec<TrackInfo<Ffmpeg>>,
  VecDeque<(
    TrackIndex,
    AttachmentPacket<AttachmentPacketExtra, <C as crate::FfmpegCarrier>::Buffer>,
  )>,
);

fn build_tracks<C: crate::FfmpegCarrier + crate::CarrierOps>(
  input: &Input,
  pre_probe: &PreProbeDurations,
  limits: DemuxLimits,
) -> Result<BuiltTracks<C>, DemuxError> {
  // **Admission before allocation.** Every attachment in the file is
  // judged here, in full, before the loop below allocates anything at
  // all — see [`admit_streams`] for why the charge cannot live
  // inside the capture.
  admit_streams(input, limits)?;

  let count = input.streams().len();
  // **Reserved fallibly, both of them.** The stream count is the
  // container's; `max_streams` bounds it, and a bound the caller chose
  // is exactly the number that must come back as an error rather than
  // an abort when the allocator declines it.
  let mut tracks = Vec::new();
  tracks
    .try_reserve_exact(count)
    .map_err(|_| DemuxError::TrackTableAlloc(TrackTableAlloc::new(count)))?;
  let mut pending = VecDeque::new();
  pending
    .try_reserve(count)
    .map_err(|_| DemuxError::TrackTableAlloc(TrackTableAlloc::new(count)))?;
  // The whole file's stream-metadata budget, spent across every
  // admitted stream rather than per stream: `max_streams` bounds how
  // many streams a header may declare, and nothing bounded what each
  // could carry.
  let mut metadata_spent: usize = 0;

  for stream in input.streams() {
    let index = stream.index();
    // `AVStream.index` is the stream's position in `ic->streams[]` and
    // libavformat keeps the two identical. The demux tier makes
    // `TrackIndex` mean "position in `tracks()`", so the two agree by
    // construction — but only if they really are dense and in order,
    // which is cheap to insist on rather than assume.
    debug_assert_eq!(
      index,
      tracks.len(),
      "AVStream indices are dense and ordered"
    );

    let parameters = stream.parameters();
    let par = unsafe { parameters.as_ptr() };
    // Never read `AVCodecParameters.codec_type` / `.codec_id` as their
    // bindgen enums: a value outside this build's discriminant set is
    // UB the moment it exists. Both are read as the raw integers they
    // are on the wire — the medium through [`boundary::media_kind_of`],
    // which folds anything unnamed into `Unknown`.
    //
    // The medium used to go through `Parameters::medium()` on the
    // argument that `AVMediaType`'s set is tiny and stable. It is; that
    // made the read unlikely to bite, not sound. The exception is gone
    // rather than defended, so no attacker-reachable path in this crate
    // forms a bindgen enum out of FFmpeg memory.
    let medium = boundary::media_kind_of(&parameters);
    let codec =
      CodecId::from_raw(unsafe { read_unaligned(addr_of!((*par).codec_id).cast::<i32>()) });

    let disposition = unsafe { (*stream.as_ptr()).disposition };
    let attached_pic = is_attachment_disposition(disposition);

    // Already judged by [`admit_streams`], which refuses a malformed
    // ruler before this loop allocates anything; the same function is
    // called here because this is where the value is actually needed,
    // and one function is how the two passes are kept from becoming
    // two rules.
    let time_base = stream_timebase(index, stream.time_base())?;
    // **The duration libavformat held for this stream before it probed
    // — or none.**
    //
    // Read from [`PreProbeDurations`], captured before probing, and no
    // claim about the file: a format's reader may compute the figure
    // itself. `AVStream.duration` as it stands now is not carried,
    // because probing hands a stream that supplied no timing the
    // *container's* length — every font and cover picture, and any
    // timed stream whose first packet lies past the probe, such as a
    // sparse subtitle track. A figure that appears only after probing
    // is libavformat's fill and is dropped here, and so is one for a
    // stream libavformat created while probing; the per-track figure
    // measured from packets is `measured_end`.
    let duration = pre_probe.get(index);
    let raw_start = stream.start_time();
    let frames = stream.frames();

    let params = if attached_pic {
      // Cover art. A still image in a video-shaped slot is an
      // attachment by every property that matters, and the `Video` arm
      // is reserved for motion video.
      TrackParams::Attachment(AttachmentTrackParams::new(codec))
    } else {
      match medium {
        boundary::MediaKind::Video => TrackParams::Video(VideoTrackParams::new(
          codec,
          unsafe { (*par).width }.max(0) as u32,
          unsafe { (*par).height }.max(0) as u32,
          boundary::from_av_pixel_format(unsafe { (*par).format }),
          rate_to_timebase(stream.avg_frame_rate()),
        )),
        boundary::MediaKind::Audio => {
          let ch_layout = unsafe { std::ptr::addr_of!((*par).ch_layout) };
          // **This is the trusted road, and here is why it is trusted.**
          //
          // The `unsafe` form is the only one that reads a custom
          // channel map, because its contract asks the caller for the
          // map's extent — the one thing a pointer cannot be asked.
          // This call site can supply it: `par` is the
          // `AVCodecParameters` libavformat itself built for this
          // stream, and libavformat fills `ch_layout` through
          // `av_channel_layout_copy`, which allocates the map and sizes
          // it to `nb_channels` in the same operation. The layout is
          // never handed in by a caller of this crate, so there is no
          // road by which `nb_channels` and the map can disagree.
          //
          // SAFETY: (1) `par` is a live `*const AVCodecParameters` for
          // the life of `parameters`, so `ch_layout` is a live, aligned
          // `*const AVChannelLayout`. (2) For a `CUSTOM` order, `u.map`
          // is FFmpeg's own allocation of exactly `nb_channels`
          // `AVChannelCustom` entries, per the paragraph above. The
          // helper validates `order` as an `i32` before constructing any
          // `AVChannelOrder`, and refuses a null map or an
          // unterminated name before FFmpeg is allowed to render the
          // layout — a precondition, not a courtesy.
          let channel_layout =
            unsafe { crate::channel_layout::channel_layout_description_from_raw_ptr(ch_layout) }
              .map_err(|fault| ParametersError::from_layout_fault(index, fault))?;
          TrackParams::Audio(AudioTrackParams::new(
            codec,
            unsafe { (*par).sample_rate }.max(0) as u32,
            channel_layout.channels().min(255) as u8,
            SampleFormat::from_raw(unsafe { (*par).format }),
            channel_layout,
          ))
        }
        boundary::MediaKind::Subtitle => TrackParams::Subtitle(SubtitleTrackParams::new(codec)),
        boundary::MediaKind::Data => TrackParams::Data(DataTrackParams::new(codec)),
        boundary::MediaKind::Attachment => {
          TrackParams::Attachment(AttachmentTrackParams::new(codec))
        }
        boundary::MediaKind::Unknown => TrackParams::Unknown(UnknownTrackParams::new(codec)),
      }
    };

    // The parameter mirror. For an `AVMEDIA_TYPE_ATTACHMENT` stream its
    // `extradata` **is** the attachment's payload — the same bytes the
    // carrier below already holds — so it is left behind rather than
    // copied. Censused before it was: nothing can use it. libavcodec
    // has no decoder for a font (`avcodec_find_decoder` answers null
    // for `AV_CODEC_ID_TTF` and its siblings), so no road in this crate
    // or downstream of it opens a codec context from these parameters;
    // the payload reaches a consumer as the attachment packet, which is
    // the delivery the demux tier promises.
    //
    // **Omitted, not stripped.** An earlier shape copied the extradata
    // and freed it immediately afterwards, which allocated the payload
    // for no reason and — worse — charged it against the *parameter*
    // ceiling on the way past. A font between the two ceilings passed
    // the admission pass and then failed inside the clone. See
    // [`ExtradataPolicy`](crate::extras::ExtradataPolicy).
    //
    // Cover art keeps its extradata: there the payload is the parked
    // `AVPacket`, extradata is *not* a copy of it, and a still codec
    // can legitimately need it (MJPEG with an external Huffman table).
    // Measured on this build: a cover-art stream carries none anyway.
    let extradata_policy = if medium.is_attachment() {
      crate::extras::ExtradataPolicy::Omit
    } else {
      crate::extras::ExtradataPolicy::Copy
    };
    // Straight from the stream's own parameters into the owned ticket.
    // The row used to reach here through an intermediate
    // `avcodec_parameters_copy` — one ffmpeg-native deep copy per
    // track, whose only purpose was to sever the tie to the format
    // context. The mirror severs it by being owned Rust, so that copy
    // is gone rather than moved.
    let ticket = crate::ticket::CodecTicket::mirror_with(
      &parameters,
      index,
      limits.max_codec_parameter_bytes(),
      extradata_policy,
    )?;
    let extra = TrackExtra::new(index as i32, ticket)
      .with_disposition(disposition)
      .with_start_time((raw_start != AV_NOPTS_VALUE).then_some(raw_start))
      .with_frame_count((frames > 0).then_some(frames));

    // SAFETY: `stream` keeps the `AVStream` — and so its metadata
    // dictionary — live across every read below. The dictionary is
    // read through `av_dict_get` rather than through
    // `DictionaryRef::get`: see [`metadata_value`].
    let metadata = unsafe { (*stream.as_ptr()).metadata };
    // **Every retained value is measured, charged and only then
    // built**, against one whole-file budget. Three values a stream
    // apiece, mirrored eagerly for every admitted stream, is an
    // open-time allocation a container controls the size of — and
    // `max_streams` bounds the count of streams, not the bytes each
    // one can carry.
    let mut text = |key: &'static CStr| -> Result<Option<Utf8Bytes>, DemuxError> {
      // SAFETY: as above — `stream` keeps the dictionary live, and the
      // borrow does not outlive this call.
      let raw = unsafe { metadata_value(metadata, key) };
      retain_metadata(
        raw,
        &mut metadata_spent,
        limits.max_total_stream_metadata_bytes(),
      )
      .map_err(|fault| stream_metadata_error(index, key, fault, limits))
    };
    let info = TrackInfo::new(time_base, params, extra)
      .with_duration(duration)
      // The same three keys `RETAINED_STREAM_METADATA` names, in the
      // same order — `admit_streams` has already charged every one of
      // them across the whole file, so nothing here can refuse a value
      // the pre-pass admitted. The charge stays as defence in depth:
      // it is what makes `metadata_spent` real rather than notional,
      // and it is the arm that reports an allocator refusal, which no
      // measurement can foresee.
      .with_filename(text(c"filename")?)
      .with_mime_type(text(c"mimetype")?)
      // **`language` is where every container's tag lands.** libavformat
      // normalises the *key*, not the value: Matroska's `Language`
      // element, MP4's `mdhd` language code and an `elng`/ISO 639-2
      // atom, Matroska's BCP 47 `LanguageBCP47`, an ASF descriptor and
      // an ID3 `TLAN` frame all arrive on this one entry. What each
      // wrote is what is read — see
      // [`TrackInfo::language`](mediadecode::demuxer::TrackInfo::language)
      // for why nothing folds it here.
      .with_language(text(c"language")?);

    // Capture the attachment payload now, so the queue is complete
    // before a single timed packet has been read. Every attachment
    // track leaves this loop with exactly one packet queued, or the
    // open fails: that is what makes "exactly one packet, before any
    // timed packet" a property of the construction rather than a
    // promise the pull loop has to keep.
    if info.kind() == TrackKind::Attachment {
      let packet = if attached_pic {
        // SAFETY: `stream` keeps the format context (and so the
        // `AVStream`) live; `attached_pic` is an `AVPacket` embedded by
        // value, and `addr_of!` reaches it without forming a reference
        // to the stream.
        let pkt = unsafe { std::ptr::addr_of!((*stream.as_ptr()).attached_pic) };
        // SAFETY: `pkt` is this stream's own parked picture, live with the
        // format context `stream` borrows, and written only while the
        // container opened — which is over.
        unsafe { attached_pic_payload::<C>(pkt, index, limits) }?
      } else {
        extradata_payload::<C>(&stream, limits)?
      };
      pending.push_back((TrackIndex::new(index), packet));
    }

    tracks.push(info);
  }

  Ok((tracks, pending))
}

// ---------------------------------------------------------------------------
//  Chapter-table construction.
// ---------------------------------------------------------------------------

/// Mirrors `AVFormatContext.chapters` into owned rows.
///
/// libavformat fills the array while it parses the header — the MOV
/// `chpl`/chapter track, Matroska's `Chapters` element, an Ogg
/// `CHAPTER` comment — and `avformat_find_stream_info` has returned by
/// the time this runs, so the answer is final. That is what lets the
/// session hold the mirror beside the track table and answer
/// [`Demuxer::chapters`] at any point without touching the file again.
///
/// # Read raw, for the reasons the rest of this file is
///
/// The fields come off the `AVChapter` directly rather than through
/// `ffmpeg_next`'s own chapter wrapper. Two reasons, both already
/// standing here: that wrapper reads the metadata dictionary through
/// `DictionaryRef`, whose `&str` is built with `from_utf8_unchecked`
/// over bytes no container validates — see [`metadata_value`], the
/// measured road every metadata value in this file takes — and its
/// `as_ptr` dereferences the array entry without checking it, where
/// this walk answers a null entry by skipping it.
///
/// Nothing read here is a bindgen enum: an id, an `AVRational`, two
/// tick counts and a dictionary pointer.
///
/// # Nothing is repaired
///
/// A chapter whose `end` precedes its `start`, and one whose end
/// libavformat left at `AV_NOPTS_VALUE` because the file declared
/// none, are both mirrored exactly as written. This layer reports what
/// the container says; see [`Chapter`]'s own doc for why a clamp here
/// would be worse than the inverted row it replaced.
///
/// # Admission before allocation
///
/// The judging happens twice, and the first time is
/// [`admit_chapters`] — which runs before the *track* table is built,
/// because that table is where an open spends its memory and a file
/// certain to be refused should not pay for it. What follows here is
/// the same judgement repeated on the way to a row it has to build
/// anyway; see that function for why repeating it is how the two
/// passes stay one rule.
///
/// `nb_chapters` is file-controlled and libavformat has no
/// `max_chapters` knob to bound it with, so this crate judges the count
/// itself before reserving anything —
/// [`DemuxLimits::max_chapters`](crate::DemuxLimits::max_chapters), the
/// same posture [`admit_streams`] takes for the track table. Past the
/// ceiling the open fails with [`TooManyChapters`]; the reservation
/// that follows is **fallible** (`try_reserve_exact`), so an allocator
/// that refuses a count inside the ceiling is a named error rather than
/// an abort.
///
/// Titles are charged as they are read, against
/// [`DemuxLimits::max_total_chapter_title_bytes`](crate::DemuxLimits::max_total_chapter_title_bytes),
/// and the charge is made after each single title rather than before —
/// exactly as [`charge_attachment`] does. What bounds the overshoot is
/// [`METADATA_VALUE_MAX_BYTES`], which [`metadata_value`] refuses any
/// one dictionary value past: the worst this can hold before refusing
/// is the budget plus one 64 KiB title.
///
/// Note that libavformat having already built its own chapter array
/// does **not** bound this one. That array is four scalars and a
/// pointer per entry; a row here owns a title as well, so the mirror is
/// the larger of the two and a table that parsed successfully can still
/// be one this process should not pay for.
/// Reads the chapter array's shape and **allocates nothing**.
///
/// Shared by [`admit_chapters`] and [`build_chapters`] so the two
/// passes cannot come to disagree about what the container declared.
/// Returns `None` when there is no table to walk at all.
///
/// Safe because the borrow is the whole precondition: `input` owns the
/// `AVFormatContext` these two fields belong to, and it is live for as
/// long as the reference is.
fn chapter_array(input: &Input) -> Option<(usize, *mut *mut ffmpeg_next::ffi::AVChapter)> {
  // SAFETY: `input` owns a live `AVFormatContext` for the whole of
  // this call; `nb_chapters` and `chapters` are public fields of it.
  let (count, array) = unsafe {
    let context = input.as_ptr();
    ((*context).nb_chapters as usize, (*context).chapters)
  };
  if array.is_null() || count == 0 {
    None
  } else {
    Some((count, array))
  }
}

/// Judges the whole chapter table **before anything is materialised**,
/// allocating not one byte.
///
/// # Why this is a pass of its own
///
/// The judgement used to live inside [`build_chapters`], which runs
/// after the track table — and the track table is where this open
/// spends its memory: attachment carriers up to
/// [`DemuxLimits::max_total_attachment_bytes`](crate::DemuxLimits::max_total_attachment_bytes),
/// codec-parameter mirrors, retained stream metadata, and one `Arc` per
/// row. A file whose chapter count is a hundred times the ceiling was
/// therefore *certain* to be refused, and paid for hundreds of
/// megabytes of track material first. Repeating the open repeats the
/// bill, which is a resource-exhaustion road built out of two correct
/// checks in the wrong order.
///
/// So the cheap, allocation-free judgement runs beside stream
/// admission, before either table exists. What it judges is exactly
/// what [`build_chapters`] would have: the declared count against
/// [`DemuxLimits::max_chapters`](crate::DemuxLimits::max_chapters),
/// every chapter's timebase, and every title's decoded length against
/// the aggregate title budget.
///
/// # Why the second pass still judges
///
/// [`build_chapters`] repeats these checks rather than trusting this
/// one. It needs the timebase and the title anyway to build a row, and
/// re-deriving them is how the two passes stay one rule: nothing
/// between the two can change the container's answer —
/// `avformat_find_stream_info` has long returned — so a divergence
/// would be a bug in this file rather than a state to handle. The
/// repeat costs a pointer walk and a `strlen`; it allocates nothing.
fn admit_chapters(input: &Input, limits: DemuxLimits) -> Result<(), DemuxError> {
  let Some((count, array)) = chapter_array(input) else {
    return Ok(());
  };
  if count > limits.max_chapters() as usize {
    return Err(DemuxError::TooManyChapters(TooManyChapters::new(
      count,
      limits.max_chapters(),
    )));
  }

  let mut title_spent: usize = 0;
  for index in 0..count {
    // SAFETY: `array` is the context's own array of `count` chapter
    // pointers and `index` is below `count`.
    let chapter = unsafe { *array.add(index) };
    if chapter.is_null() {
      continue;
    }
    // SAFETY: a non-null entry is an `AVChapter` the context owns for
    // its whole life. Every field read is a plain scalar, an
    // `AVRational` or a dictionary pointer — never a bindgen enum.
    let (id, time_base, metadata) = unsafe {
      (
        (*chapter).id,
        Rational::from((*chapter).time_base),
        (*chapter).metadata,
      )
    };
    if positive_rational_to_timebase(time_base).is_none() {
      return Err(DemuxError::ChapterTimebaseInvalid(
        ChapterTimebaseInvalid::new(index, id, time_base.numerator(), time_base.denominator()),
      ));
    }
    // The title is **measured, not built**: `metadata_value` returns a
    // borrow of libavutil's own buffer and `lossy_len` prices the
    // decoding without producing it, so an over-budget table is
    // refused having touched no heap at all.
    //
    // SAFETY: `metadata` is the chapter's own dictionary, owned by the
    // context and live for the whole of this call.
    let raw_title = unsafe { metadata_value(metadata, c"title") };
    // Measured and charged; the row itself is built by
    // [`build_chapters`], which is where the bytes are actually spent.
    let _admitted = charge_metadata(
      raw_title,
      &mut title_spent,
      limits.max_total_chapter_title_bytes(),
    )
    .map_err(|fault| chapter_title_error(index, fault, limits))?;
  }
  Ok(())
}

fn build_chapters(input: &Input, limits: DemuxLimits) -> Result<Vec<Chapter<Ffmpeg>>, DemuxError> {
  let Some((count, array)) = chapter_array(input) else {
    return Ok(Vec::new());
  };
  if count > limits.max_chapters() as usize {
    return Err(DemuxError::TooManyChapters(TooManyChapters::new(
      count,
      limits.max_chapters(),
    )));
  }

  let mut out = Vec::new();
  out
    .try_reserve_exact(count)
    .map_err(|_| DemuxError::ChapterAlloc(ChapterAlloc::new(count)))?;
  let mut title_spent: usize = 0;

  for index in 0..count {
    // SAFETY: `array` is the context's own array of `count` chapter
    // pointers and `index` is below `count`.
    let chapter = unsafe { *array.add(index) };
    if chapter.is_null() {
      continue;
    }
    // SAFETY: a non-null entry is an `AVChapter` the context owns for
    // its whole life. Every field below is a plain scalar, an
    // `AVRational` or a dictionary pointer — never a bindgen enum.
    let (id, time_base, start, end, metadata) = unsafe {
      (
        (*chapter).id,
        Rational::from((*chapter).time_base),
        (*chapter).start,
        (*chapter).end,
        (*chapter).metadata,
      )
    };
    // **The ruler is judged before the row is built.** A chapter's
    // timebase is file-controlled and libavformat does not validate it
    // — the FFMETADATA parser stores `TIMEBASE=-1/1000` as written —
    // so this is where a container's malformed rational becomes a
    // refusal instead of a fabricated ruler or a panic.
    let timebase =
      positive_rational_to_timebase(time_base).ok_or(DemuxError::ChapterTimebaseInvalid(
        ChapterTimebaseInvalid::new(index, id, time_base.numerator(), time_base.denominator()),
      ))?;

    // **`title` is where every container's chapter name lands.**
    // libavformat normalises the key, not the value: a Matroska
    // `ChapterDisplay`'s `ChapString`, a MOV chapter track's text
    // sample and an FFMETADATA `title=` all arrive on this one entry,
    // and what each wrote is what is read.
    //
    // **Measured and charged before a byte of it is copied.** The
    // reading below borrows libavutil's buffer, so a title the budget
    // refuses costs no heap at all — see [`retain_metadata`].
    //
    // SAFETY: `metadata` is the chapter's own dictionary, owned by the
    // context and live for the whole of this call.
    let raw_title = unsafe { metadata_value(metadata, c"title") };
    let title = retain_metadata(
      raw_title,
      &mut title_spent,
      limits.max_total_chapter_title_bytes(),
    )
    .map_err(|fault| chapter_title_error(index, fault, limits))?;

    // Inside the reservation above, which was for `count` rows and is
    // never pushed past — so no growth, fallible or otherwise, happens
    // here.
    out.push(
      Chapter::new(
        id,
        timebase,
        Timestamp::new(start, timebase),
        Timestamp::new(end, timebase),
      )
      .with_title(title),
    );
  }
  Ok(out)
}

/// Whether `packet`'s payload is the very allocation the container has
/// parked in `AVStream.attached_pic` for stream `index`.
///
/// # Why this exists
///
/// libavformat queues a stream's attached picture as its **first
/// packet** — `read_frame_internal` does `av_packet_ref(pkt,
/// &st->attached_pic)` and keeps its own reference — so that packet
/// arrives with two references through nobody's fault. A pure cover-art
/// stream never reaches this road (it is an attachment, hoisted at
/// open), but a stream carrying `ATTACHED_PIC | TIMED_THUMBNAILS` is
/// deliberately classified as **video** by
/// [`is_attachment_disposition`], so its first pull comes through here
/// and would be refused as a shared payload. Every packet after it is
/// an ordinary timed one with a buffer of its own.
///
/// # The probe, and why it is a proof rather than a guess
///
/// `av_buffer_ref` sets the new reference's `buffer` field to the
/// source's, so two `AVBufferRef`s name one allocation **iff** their
/// `buffer` pointers are equal — the same identity
/// [`crate::FfmpegBuffer::ptr_eq`] rests on. Comparing them therefore
/// establishes the fact the carve-out needs: this payload's allocation
/// *is* `AVStream.attached_pic`'s, so one of its outstanding references
/// is the container's own.
///
/// The alternatives were heuristics and are not used: the disposition
/// bits say a stream *has* an attached picture, not that this packet is
/// it; "the first packet on the stream" is an ordering assumption that
/// nothing in libavformat's contract fixes.
///
/// # The soundness argument, restated for this packet
///
/// It is the same one the hoisted-attachment road rests on, and it
/// holds here for the same reason. `AVStream.attached_pic` is written
/// once, while the container is being opened, and never again; the
/// reference this crate is looking at is the container's, held for the
/// lifetime of the `AVFormatContext`, and there is no
/// `ffmpeg_next::Packet` wrapping it for anyone to call `data_mut` on.
/// What the uniqueness rule guards against is a *safe Rust* handle that
/// may write while this crate reads, and the container's reference is
/// not one.
///
/// # Safety
///
/// `input` and `packet` must both be live for the duration of the call.
unsafe fn is_streams_attached_pic(input: &Input, index: usize, packet: &Packet) -> bool {
  // SAFETY: `input` owns a live `AVFormatContext`; `streams` is an
  // array of `nb_streams` pointers, and `index` is checked against it.
  let stream = unsafe {
    let context = input.as_ptr();
    if index >= (*context).nb_streams as usize {
      return false;
    }
    *(*context).streams.add(index)
  };
  if stream.is_null() {
    return false;
  }
  // SAFETY: `stream` is one of the context's own live `AVStream`s and
  // `packet` is live per this function's contract.
  unsafe { packet_is_parked_picture(stream, packet) }
}

/// The identity itself: whether `packet`'s payload allocation is the
/// one `stream` has parked in `attached_pic`.
///
/// Split out from [`is_streams_attached_pic`] so the comparison can be
/// tested against a hand-built pair without forging an
/// `AVFormatContext` — see `a_queued_attached_picture_is_recognised`.
///
/// # Safety
///
/// `stream` must be a live `AVStream` and `packet` a live `AVPacket`.
unsafe fn packet_is_parked_picture(stream: *const AVStream, packet: &Packet) -> bool {
  use ffmpeg_next::packet::Ref;

  // SAFETY: both are live per the contract; `attached_pic` is an inline
  // `AVPacket` and both `buf` fields may be null, which is answered
  // before either is read through.
  unsafe {
    let parked = (*stream).attached_pic.buf;
    let carried = (*packet.as_ptr()).buf;
    if parked.is_null() || carried.is_null() {
      return false;
    }
    // The shared `AVBuffer`, not the `AVBufferRef`: `av_packet_ref`
    // mints a new reference struct around the same allocation, so
    // comparing the references themselves would answer "no" to exactly
    // the case this is for.
    (*parked).buffer == (*carried).buffer
  }
}

/// Whether a stream's disposition makes it an **attachment** — a
/// payload with no place on the timeline — rather than a timed track.
///
/// `AV_DISPOSITION_ATTACHED_PIC` alone says "cover art": one still
/// image, parked in `AVStream.attached_pic`, no timeline. But FFmpeg
/// pairs it with `AV_DISPOSITION_TIMED_THUMBNAILS` for a different
/// thing entirely — "the stream is sparse, and contains thumbnail
/// images, often corresponding to chapter markers", a flag its own
/// header documents as *only ever* used together with `ATTACHED_PIC`.
/// Such a stream has many images and every one of them has a
/// timestamp.
///
/// Classifying that as an attachment loses all but the first: the
/// attachment contract is exactly one packet, so the queue takes the
/// parked copy and the delivery loop drops every timed packet on the
/// track. It goes to the **`Video`** arm instead. That does not
/// contradict "cover art is an attachment, not video" — the reason
/// behind that ruling is that a single still with no timeline must not
/// look like a motion track, and a timed-thumbnail stream *is* on the
/// timeline. It is sparse video: a codec id, a frame size, a pixel
/// format and packets with timestamps, which is everything a consumer
/// needs to decode the images. The `Data` arm was the alternative and
/// is worse: it would strand encoded pictures in an arm that names no
/// decoder.
///
/// The bits are tested against the raw `AVStream.disposition` rather
/// than through `ffmpeg_next`'s `Disposition`, which mints no
/// `TIMED_THUMBNAILS` constant at all — its `from_bits_truncate` drops
/// every bit this build of the wrapper has no name for, which is how
/// the distinction went missing in the first place.
const fn is_attachment_disposition(disposition: c_int) -> bool {
  disposition & AV_DISPOSITION_ATTACHED_PIC != 0
    && disposition & AV_DISPOSITION_TIMED_THUMBNAILS == 0
}

/// Each stream's reading of its H.264 or HEVC bitstream, by stream
/// index: a [`NalStream`] for a video stream of either codec, built from
/// its extradata, and `None` for every other stream, whose packets carry
/// the container's mark alone.
///
/// The codec and the medium are read as the raw integers they are, as
/// [`build_tracks`] reads them; the extradata is borrowed, not copied —
/// the reading keeps only its framing and the sub-layers its parameter
/// sets declare.
fn bitstreams(input: &Input) -> Result<Vec<Option<NalStream>>, DemuxError> {
  let count = input.streams().len();
  let mut out = Vec::new();
  out
    .try_reserve_exact(count)
    .map_err(|_| DemuxError::TrackTableAlloc(TrackTableAlloc::new(count)))?;
  for stream in input.streams() {
    let parameters = stream.parameters();
    // SAFETY: the stream's parameters are live for this iteration.
    let par = unsafe { parameters.as_ptr() };
    // SAFETY: `codec_id` is a public field of the live parameters, read
    // as the integer it is rather than as its bindgen enum.
    let codec =
      CodecId::from_raw(unsafe { read_unaligned(addr_of!((*par).codec_id).cast::<i32>()) });
    let nal = match codec {
      CodecId::H264 => Some(NalCodec::H264),
      CodecId::HEVC => Some(NalCodec::Hevc),
      _ => None,
    }
    .filter(|_| boundary::media_kind_of(&parameters).is_video())
    .map(|codec| {
      // SAFETY: `extradata` and `extradata_size` are public fields of the
      // stream's live parameters; libavformat keeps `extradata` readable
      // for `extradata_size` bytes while the stream exists, and the slice
      // is read before this iteration ends.
      let extradata = unsafe {
        let ptr = (*par).extradata;
        let len = (*par).extradata_size.max(0) as usize;
        if ptr.is_null() || len == 0 {
          &[][..]
        } else {
          core::slice::from_raw_parts(ptr, len)
        }
      };
      NalStream::new(codec, extradata)
    });
    out.push(nal);
  }
  Ok(out)
}

/// The packet side data a decoder may take up past its own packet: a
/// packet carrying any is never
/// [`DISPOSABLE`](PacketFlags::DISPOSABLE), whoever marked it, because
/// dropping it would lose what the decoder, or a later picture, needs. Every
/// `AVPacketSideDataType` FFmpeg 9.0.1 names (`libavcodec/packet.h` 41–394)
/// is in this list or in [`RIDING_SIDE_DATA`], never both; a type in
/// neither — one a later FFmpeg adds — persists ([`side_data_persists`]).
///
/// **A kind a release after 5.1 added is named only where the linked
/// FFmpeg is that release or newer** (`ffmpeg_7_0` to `ffmpeg_9_0`, which
/// `build.rs` sets from `ffmpeg-sys-next`'s report of the release, the gates
/// `ffmpeg-next` puts on the same kinds): the bindings are generated from
/// the linked headers, which name no newer kind, and a library that names
/// none cannot hand one over. FFmpeg appends each new kind to the enum, so
/// a build's gates leave out its last kinds alone.
///
/// | type | since | why it persists |
/// |---|---|---|
/// | `PALETTE` | | "a new palette is present" (`packet.h` 43–47), which a decoder keeps (`libavcodec/decode.c` 2321–2334) |
/// | `NEW_EXTRADATA` | | a new configuration (`h264dec.c` 1038–1044, `hevc/hevcdec.c` 3854–3860) |
/// | `PARAM_CHANGE` | | new dimensions or a new rate (`decode.c` 117–176) |
/// | `FALLBACK_TRACK`, `CPB_PROPERTIES` | | a description of the stream, not of the packet |
/// | `MATROSKA_BLOCKADDITIONAL` | | a block addition of any kind, which nothing here reads |
/// | `METADATA_UPDATE` | | the stream's metadata from the packet on |
/// | `A53_CC` | | caption bytes a caption decoder reads across pictures |
/// | `ENCRYPTION_INIT_INFO` | | a key system's initialization, for the packets after |
/// | `AFD` | | an active format a receiver may hold until the next |
/// | `DOVI_CONF` | | a Dolby Vision configuration the decoder keeps (`hevc/hevcdec.c` 3862–3869) |
/// | `IAMF_MIX_GAIN_PARAM`, `IAMF_DEMIXING_INFO_PARAM`, `IAMF_RECON_GAIN_INFO_PARAM` | 7.0 | a parameter that applies for a duration |
/// | `LCEVC` | 7.1 | an enhancement whose decoder keeps state across pictures (`decode.c` 1640–1666, 1704–1775) |
/// | `HEVC_CONF` | 9.0 | an enhancement layer's decoder configuration |
const PERSISTING_SIDE_DATA: &[AVPacketSideDataType] = &[
  AVPacketSideDataType::AV_PKT_DATA_PALETTE,
  AVPacketSideDataType::AV_PKT_DATA_NEW_EXTRADATA,
  AVPacketSideDataType::AV_PKT_DATA_PARAM_CHANGE,
  AVPacketSideDataType::AV_PKT_DATA_FALLBACK_TRACK,
  AVPacketSideDataType::AV_PKT_DATA_CPB_PROPERTIES,
  AVPacketSideDataType::AV_PKT_DATA_MATROSKA_BLOCKADDITIONAL,
  AVPacketSideDataType::AV_PKT_DATA_METADATA_UPDATE,
  AVPacketSideDataType::AV_PKT_DATA_A53_CC,
  AVPacketSideDataType::AV_PKT_DATA_ENCRYPTION_INIT_INFO,
  AVPacketSideDataType::AV_PKT_DATA_AFD,
  AVPacketSideDataType::AV_PKT_DATA_DOVI_CONF,
  #[cfg(ffmpeg_7_0)]
  AVPacketSideDataType::AV_PKT_DATA_IAMF_MIX_GAIN_PARAM,
  #[cfg(ffmpeg_7_0)]
  AVPacketSideDataType::AV_PKT_DATA_IAMF_DEMIXING_INFO_PARAM,
  #[cfg(ffmpeg_7_0)]
  AVPacketSideDataType::AV_PKT_DATA_IAMF_RECON_GAIN_INFO_PARAM,
  #[cfg(ffmpeg_7_1)]
  AVPacketSideDataType::AV_PKT_DATA_LCEVC,
  #[cfg(ffmpeg_9_0)]
  AVPacketSideDataType::AV_PKT_DATA_HEVC_CONF,
];

/// The packet side data that rides with its own packet and no further:
/// what FFmpeg's decoding copies onto the packet's own frame alone
/// (`ff_sd_global_map`, `libavcodec/avcodec.c` 57–70; the packet map and
/// the metadata of `ff_decode_frame_props_from_pkt`, `decode.c` 1492–1596),
/// and what nothing decoding reads, about the packet itself. A kind a
/// release after 5.1 added is named only where the linked FFmpeg is that
/// release or newer, as in [`PERSISTING_SIDE_DATA`]; one left out persists.
///
/// | type | since | why it rides |
/// |---|---|---|
/// | `REPLAYGAIN`, `DISPLAYMATRIX`, `STEREO3D`, `AUDIO_SERVICE_TYPE`, `MASTERING_DISPLAY_METADATA`, `SPHERICAL`, `CONTENT_LIGHT_LEVEL`, `ICC_PROFILE` | | copied onto the packet's frame alone (`avcodec.c` 57–70) |
/// | `AMBIENT_VIEWING_ENVIRONMENT` | 7.0 | the same |
/// | `3D_REFERENCE_DISPLAYS` | 8.0 | the same |
/// | `EXIF` | 8.1 | the same |
/// | `SKIP_SAMPLES`, `S12M_TIMECODE`, `DYNAMIC_HDR10_PLUS` | | the packet's frame's own, copied onto it alone (`decode.c` 1550–1596) |
/// | `DYNAMIC_HDR_SMPTE_2094_APP5` | 9.0 | the same |
/// | `STRINGS_METADATA` | | metadata of the packet's frame alone (`decode.c` 1538–1548) |
/// | `H263_MB_INFO`, `QUALITY_STATS` | | an encoder's account of the packet |
/// | `JP_DUALMONO` | | which channel of the packet's audio to use |
/// | `SUBTITLE_POSITION`, `WEBVTT_IDENTIFIER`, `WEBVTT_SETTINGS` | | the packet's own subtitle event or cue |
/// | `MPEGTS_STREAM_ID` | | the packet's PES stream id, for a muxer (`libavformat/mpegts.c` 1119) |
/// | `ENCRYPTION_INFO` | | how to decrypt the packet itself |
/// | `PRFT` | | the time the packet was produced |
/// | `RTCP_SR` | 8.0 | the last sender report at the packet |
/// | `FRAME_CROPPING` | 7.1 | the packet's frame's cropping, which no decoder reads from a packet |
const RIDING_SIDE_DATA: &[AVPacketSideDataType] = &[
  AVPacketSideDataType::AV_PKT_DATA_H263_MB_INFO,
  AVPacketSideDataType::AV_PKT_DATA_REPLAYGAIN,
  AVPacketSideDataType::AV_PKT_DATA_DISPLAYMATRIX,
  AVPacketSideDataType::AV_PKT_DATA_STEREO3D,
  AVPacketSideDataType::AV_PKT_DATA_AUDIO_SERVICE_TYPE,
  AVPacketSideDataType::AV_PKT_DATA_QUALITY_STATS,
  AVPacketSideDataType::AV_PKT_DATA_SKIP_SAMPLES,
  AVPacketSideDataType::AV_PKT_DATA_JP_DUALMONO,
  AVPacketSideDataType::AV_PKT_DATA_STRINGS_METADATA,
  AVPacketSideDataType::AV_PKT_DATA_SUBTITLE_POSITION,
  AVPacketSideDataType::AV_PKT_DATA_WEBVTT_IDENTIFIER,
  AVPacketSideDataType::AV_PKT_DATA_WEBVTT_SETTINGS,
  AVPacketSideDataType::AV_PKT_DATA_MPEGTS_STREAM_ID,
  AVPacketSideDataType::AV_PKT_DATA_MASTERING_DISPLAY_METADATA,
  AVPacketSideDataType::AV_PKT_DATA_SPHERICAL,
  AVPacketSideDataType::AV_PKT_DATA_CONTENT_LIGHT_LEVEL,
  AVPacketSideDataType::AV_PKT_DATA_ENCRYPTION_INFO,
  AVPacketSideDataType::AV_PKT_DATA_PRFT,
  AVPacketSideDataType::AV_PKT_DATA_ICC_PROFILE,
  AVPacketSideDataType::AV_PKT_DATA_S12M_TIMECODE,
  AVPacketSideDataType::AV_PKT_DATA_DYNAMIC_HDR10_PLUS,
  #[cfg(ffmpeg_7_0)]
  AVPacketSideDataType::AV_PKT_DATA_AMBIENT_VIEWING_ENVIRONMENT,
  #[cfg(ffmpeg_7_1)]
  AVPacketSideDataType::AV_PKT_DATA_FRAME_CROPPING,
  #[cfg(ffmpeg_8_0)]
  AVPacketSideDataType::AV_PKT_DATA_3D_REFERENCE_DISPLAYS,
  #[cfg(ffmpeg_8_0)]
  AVPacketSideDataType::AV_PKT_DATA_RTCP_SR,
  #[cfg(ffmpeg_8_1)]
  AVPacketSideDataType::AV_PKT_DATA_EXIF,
  #[cfg(ffmpeg_9_0)]
  AVPacketSideDataType::AV_PKT_DATA_DYNAMIC_HDR_SMPTE_2094_APP5,
];

/// Whether packet side data of `kind`, the raw `AVPacketSideDataType` it
/// arrives as, may be taken up past its own packet: a kind of
/// [`PERSISTING_SIDE_DATA`], and a kind of neither list — one a later
/// FFmpeg names, or one this build's gates leave out — every kind but
/// those of [`RIDING_SIDE_DATA`]. The kind is compared as the integer it
/// is, never formed into the bindings' enum, which a kind they do not name
/// would make undefined behaviour.
fn side_data_persists(kind: c_int) -> bool {
  let listed = |table: &[AVPacketSideDataType]| table.iter().any(|&named| named as c_int == kind);
  listed(PERSISTING_SIDE_DATA) || !listed(RIDING_SIDE_DATA)
}

/// `packet`, its [`PacketFlags::DISPOSABLE`] set where its track's
/// bitstream proves that nothing references its picture
/// ([`NalStream::read_with_record`]) and cleared, whoever set it, where it
/// carries side data that persists past it ([`PERSISTING_SIDE_DATA`]). The
/// reading takes the packet with the `AV_PKT_DATA_NEW_EXTRADATA` it
/// carries — the first such entry, as `av_packet_get_side_data` answers —
/// as the decoder takes the two.
///
/// Every packet of a track is read, a corrupt one too, because the decoder
/// reads it, and what it moves — the framing, the sub-layers its parameter
/// sets declare — moves for the packets after; a corrupt packet is never
/// marked. A track without a reading — a codec other than H.264 and HEVC —
/// keeps the container's mark, unless side data persists.
fn marked_from_bitstream<D: AsRef<[u8]>>(
  bitstream: Option<&mut Option<NalStream>>,
  packet: mediadecode::packet::VideoPacket<crate::extras::VideoPacketExtra, D>,
) -> mediadecode::packet::VideoPacket<crate::extras::VideoPacketExtra, D> {
  const NEW_EXTRADATA: c_int = AVPacketSideDataType::AV_PKT_DATA_NEW_EXTRADATA as c_int;
  let side_data = packet.extra().side_data();
  let persists = side_data
    .iter()
    .any(|entry| side_data_persists(entry.kind()));
  let record = side_data
    .iter()
    .find(|entry| entry.kind() == NEW_EXTRADATA)
    .map(crate::extras::SideDataEntry::data);
  let proven = match bitstream {
    Some(Some(bitstream)) => bitstream
      .read_with_record(packet.data().as_ref(), record)
      .is_disposable(),
    _ => false,
  };
  let mut flags = packet.flags();
  if persists {
    flags.remove(PacketFlags::DISPOSABLE);
  } else if proven && !flags.contains(PacketFlags::CORRUPT) {
    flags.insert(PacketFlags::DISPOSABLE);
  }
  if flags == packet.flags() {
    packet
  } else {
    packet.with_flags(flags)
  }
}

/// Tells each stream's reading of a seek, or of a seek attempted
/// ([`NalStream::seeked`]).
fn sought(bitstreams: &mut [Option<NalStream>]) {
  for bitstream in bitstreams.iter_mut().flatten() {
    bitstream.seeked();
  }
}

/// Upper bound on the NUL search in [`metadata_value`].
///
/// Generous by four orders of magnitude for a filename or a MIME type,
/// and there only so that a value libavutil did not terminate cannot
/// turn the walk into an unbounded read — the same discipline
/// [`crate::channel_layout`] and the pixel-format namer follow. A value
/// longer than this is refused rather than truncated: a truncated
/// filename is a different filename.
const METADATA_VALUE_MAX_BYTES: usize = 64 * 1024;

/// What a metadata dictionary holds for one key — **measured, and not
/// yet copied**.
///
/// Three outcomes rather than an `Option`, because the answer that
/// used to go missing is the third one: a value with no terminator
/// inside [`METADATA_VALUE_MAX_BYTES`] is not an absent value, and
/// reporting it as one made a container's declaration vanish silently
/// *and* escape every budget charged against it.
///
/// `Present` borrows libavutil's own buffer. That is the load-bearing
/// property of this function and the reason it exists at all: a borrow
/// allocates nothing, so a caller can learn a value's size and refuse
/// it **before** any owning conversion exists.
enum MetadataValue<'a> {
  /// The dictionary has no such key, or the entry's value is null.
  Absent,
  /// The value, borrowed from the dictionary. Not NUL-terminated here:
  /// the terminator is what bounded the walk.
  Present(&'a [u8]),
  /// No terminator below [`METADATA_VALUE_MAX_BYTES`]. Refused rather
  /// than truncated — a truncated filename is a different filename —
  /// and now refused *visibly*, unlike an absent one.
  NotTerminated,
}

/// Measures one dictionary entry without copying it.
///
/// # Safety
///
/// `dict` must be null or a live `*const AVDictionary` for the
/// duration of this call, and the returned borrow is valid only while
/// that dictionary is neither modified nor freed.
unsafe fn metadata_value<'a>(dict: *const AVDictionary, key: &CStr) -> MetadataValue<'a> {
  if dict.is_null() {
    return MetadataValue::Absent;
  }
  // SAFETY: `dict` is live per the contract above and `key` is a
  // NUL-terminated C string by construction; `av_dict_get` reads both
  // and returns a borrowed entry owned by the dictionary.
  let entry = unsafe { av_dict_get(dict, key.as_ptr(), std::ptr::null(), 0) };
  if entry.is_null() {
    return MetadataValue::Absent;
  }
  // SAFETY: a non-null entry is a live `AVDictionaryEntry` for as long
  // as the dictionary is not modified, which it is not here.
  let value = unsafe { (*entry).value };
  if value.is_null() {
    return MetadataValue::Absent;
  }
  for len in 0..METADATA_VALUE_MAX_BYTES {
    // SAFETY: `value` is a NUL-terminated string libavutil allocated
    // with `av_strdup`; the walk reads at most one byte past the last
    // value byte and stops at the terminator.
    if unsafe { *value.add(len).cast::<u8>() } == 0 {
      // SAFETY: the `len` bytes below the terminator were just walked,
      // so the slice is in bounds and initialised. The borrow lives as
      // long as the dictionary does, which this function's contract
      // requires of its caller.
      return MetadataValue::Present(unsafe {
        std::slice::from_raw_parts(value.cast::<u8>(), len)
      });
    }
  }
  MetadataValue::NotTerminated
}

/// Names the fault a stream's metadata ran into, for the key it was
/// reading.
///
/// One mapper rather than three copies of the same `match`: the three
/// values a track row retains — `filename`, `mimetype` and `language`
/// — share one budget and one road, and differ only in which key is
/// reported.
/// Names the fault a chapter's title ran into, the way
/// [`stream_metadata_error`] does for a stream's — one mapper so the
/// admission pass and the materialisation cannot report the same fault
/// two different ways.
fn chapter_title_error(index: usize, fault: MetadataFault, limits: DemuxLimits) -> DemuxError {
  match fault {
    MetadataFault::TooLong => {
      DemuxError::ChapterTitleTooLong(ChapterTitleTooLong::new(index, METADATA_VALUE_MAX_BYTES))
    }
    MetadataFault::BudgetExhausted(total) => DemuxError::ChapterTitleBudgetExhausted(
      ChapterTitleBudgetExhausted::new(index, total, limits.max_total_chapter_title_bytes()),
    ),
    MetadataFault::Alloc(bytes) => {
      DemuxError::ChapterTitleAlloc(ChapterTitleAlloc::new(index, bytes))
    }
  }
}

fn stream_metadata_error(
  index: usize,
  key: &'static CStr,
  fault: MetadataFault,
  limits: DemuxLimits,
) -> DemuxError {
  let key = key.to_str().unwrap_or("<non-utf8 key>");
  match fault {
    MetadataFault::TooLong => DemuxError::TrackMetadataTooLong(TrackMetadataTooLong::new(
      index,
      key,
      METADATA_VALUE_MAX_BYTES,
    )),
    MetadataFault::BudgetExhausted(total) => {
      DemuxError::TrackMetadataBudgetExhausted(TrackMetadataBudgetExhausted::new(
        index,
        key,
        total,
        limits.max_total_stream_metadata_bytes(),
      ))
    }
    MetadataFault::Alloc(bytes) => {
      DemuxError::TrackMetadataAlloc(TrackMetadataAlloc::new(index, key, bytes))
    }
  }
}

/// Why a metadata value this crate meant to retain was not retained.
///
/// Crate-private on purpose: each call site maps it to an error that
/// names *what* was being read, because "the chapter titles are over
/// budget" and "this stream's language is over budget" are different
/// things to a caller even though the mechanism is one.
#[derive(Debug)]
enum MetadataFault {
  /// [`MetadataValue::NotTerminated`].
  TooLong,
  /// The running total, which is over the budget.
  BudgetExhausted(usize),
  /// The decoded size that could not be reserved.
  Alloc(usize),
}

/// **Measure, charge, then materialise — in that order.**
///
/// The order is the whole of it. Reading the size is a borrow of
/// libavutil's buffer and allocates nothing, so a value the budget
/// refuses costs no heap at all; only a value already admitted is
/// built, and it is built through [`lossy_text`]'s fallible
/// reservation.
///
/// `spent` advances only for a value actually retained.
fn retain_metadata(
  value: MetadataValue<'_>,
  spent: &mut usize,
  limit: usize,
) -> Result<Option<Utf8Bytes>, MetadataFault> {
  let Some((bytes, decoded)) = charge_metadata(value, spent, limit)? else {
    return Ok(None);
  };
  lossy_text(bytes, decoded)
    .map(Some)
    .map_err(|_| MetadataFault::Alloc(decoded))
}

/// **The judging half of [`retain_metadata`], on its own** — measure
/// and charge, build nothing.
///
/// It is separate because the judging has to happen in a place the
/// building cannot: an admission pass that runs before any table is
/// materialised. Sharing one function is what stops the two passes
/// from drifting into two rules, which for a budget would mean a file
/// admitted by one and refused by the other after the memory was
/// already spent.
///
/// Returns the admitted bytes with [`lossy_len`]'s price for them, so a
/// caller that *is* going to build can hand both straight to
/// [`lossy_text`] without measuring twice. `spent` advances only for a
/// value that was admitted.
fn charge_metadata<'a>(
  value: MetadataValue<'a>,
  spent: &mut usize,
  limit: usize,
) -> Result<Option<(&'a [u8], usize)>, MetadataFault> {
  let bytes = match value {
    MetadataValue::Absent => return Ok(None),
    MetadataValue::NotTerminated => return Err(MetadataFault::TooLong),
    MetadataValue::Present(bytes) => bytes,
  };
  let decoded = lossy_len(bytes);
  let total = spent.saturating_add(decoded);
  if total > limit {
    return Err(MetadataFault::BudgetExhausted(total));
  }
  *spent = total;
  Ok(Some((bytes, decoded)))
}

/// The three metadata keys a track row retains, in the order
/// [`build_tracks`] reads them — so the admission pass and the
/// materialisation refuse on the *same* value and name the same key.
const RETAINED_STREAM_METADATA: [&CStr; 3] = [c"filename", c"mimetype", c"language"];

/// Wraps `AVStream.attached_pic` — the real packet libavformat parsed
/// for a cover-art stream — as this track's one attachment packet.
///
/// A stream that declares cover art but parks no payload still gets a
/// packet: an empty one, marked `synthesized`, because the contract is
/// one packet per attachment track and a consumer that sees an empty
/// payload learns something true about the file. The alternative shipped
/// once — waiting for the payload to arrive as a packet later — and it
/// cannot hold: nothing stops a timed packet, or a seek, from coming
/// first, so the track's packet would arrive out of order or never.
///
/// Measured before it was written: across MP3 (ID3 APIC), M4A (`covr`),
/// FLAC (`METADATA_BLOCK_PICTURE`) and Matroska (an `image/*`
/// attachment), every stream libavformat gives
/// `AV_DISPOSITION_ATTACHED_PIC` also carries the parked packet —
/// `ff_add_attached_pic` sets the disposition and fills
/// `attached_pic` in the same breath. The empty case is the honest
/// answer to a state this build's demuxers do not produce, not a
/// fallback anything relies on.
///
/// # Safety
///
/// `pkt` must be a live `*const AVPacket` for the duration of this call,
/// and it must be the container's parked picture — the `attached_pic`
/// embedded in the `AVStream` at `index`, written once while the
/// container opened — or a packet nothing writes through for as long as
/// the returned carrier lives: the capture is made under
/// `PayloadProvenance::AttachedPicture`, which may window a buffer
/// libavformat also holds.
unsafe fn attached_pic_payload<C: crate::FfmpegCarrier + crate::CarrierOps>(
  pkt: *const ffmpeg_next::ffi::AVPacket,
  index: usize,
  limits: DemuxLimits,
) -> Result<AttachmentPacket<AttachmentPacketExtra, C::Buffer>, DemuxError> {
  // Already admitted: [`admit_streams`] charged this payload — and
  // every other attachment in the file — before `build_tracks`
  // allocated anything. The per-attachment budget is passed down as
  // this packet's own ceiling anyway, so the funnel is guarded even if
  // a future caller reaches it without the admission pass.
  //
  // SAFETY: `pkt` is live per the contract above, and libavformat wrote
  // its payload when the container opened (`avformat_open_input` reads
  // the cover art into `attached_pic`), so every byte it exports is
  // written. The `AttachedPicture` claim is the contract's own: nothing
  // writes through the buffer while the carrier lives.
  // **The container's own cover art**, whose buffer libavformat also
  // holds — see [`crate::buffer::PayloadProvenance`] for why that
  // second reference is not the hazard a caller's second `Packet` is.
  let captured = unsafe {
    crate::buffer::payload_of::<C>(
      pkt,
      limits.max_attachment_bytes(),
      crate::buffer::PayloadProvenance::AttachedPicture,
    )
  }
  .map_err(|source| DemuxError::PacketBuffer(PacketBuffer::new(index, source)))?;
  let extra = AttachmentPacketExtra::new(index as i32);
  Ok(match captured {
    Some(payload) => {
      // The hoisted packet's own flags, through the same raw reader the
      // five boundary conversions use. FFmpeg marks an attached picture
      // `AV_PKT_FLAG_KEY` — a still image is a keyframe if anything is
      // — and building this one with empty flags dropped that, along
      // with `CORRUPT` and every other bit the packet really carried.
      // SAFETY: `pkt` points at the live embedded `AVPacket`.
      let flags = unsafe { boundary::md_flags_from_av_packet(pkt) }
        .map_err(|source| DemuxError::PacketBuffer(PacketBuffer::new(index, source)))?;
      AttachmentPacket::new(payload, extra).with_flags(flags)
    }
    // Nothing was parked, so there are no flags to read: an empty set
    // is the honest answer for a packet this layer invented.
    None => AttachmentPacket::new(C::empty(), extra.with_synthesized(true)),
  })
}

/// Builds an attachment payload out of a track's codec extradata — the
/// only place a font's bytes ever live, since an
/// `AVMEDIA_TYPE_ATTACHMENT` stream produces no packets at all.
///
/// A track with no extradata still gets a packet, with an empty
/// payload: the contract is one packet per attachment track, and a
/// consumer that sees an empty one learns something true about the
/// file. Only an allocation failure is an error.
fn extradata_payload<C: crate::FfmpegCarrier + crate::CarrierOps>(
  stream: &ffmpeg_next::format::stream::Stream<'_>,
  limits: DemuxLimits,
) -> Result<AttachmentPacket<AttachmentPacketExtra, C::Buffer>, DemuxError> {
  let index = stream.index();
  let parameters = stream.parameters();
  // SAFETY: `parameters` keeps the `AVCodecParameters` live;
  // `extradata` / `extradata_size` are public fields.
  let par = unsafe { parameters.as_ptr() };
  let ptr = unsafe { (*par).extradata };
  let len = unsafe { (*par).extradata_size }.max(0) as usize;
  // Already admitted, exactly as on the hoisted cover-art path — see
  // [`admit_streams`]. Re-judged here against the per-attachment
  // ceiling alone, so the helper is safe to call on its own.
  if len > limits.max_attachment_bytes() {
    return Err(DemuxError::AttachmentTooLarge(AttachmentTooLarge::new(
      index,
      len,
      limits.max_attachment_bytes(),
    )));
  }
  let bytes: &[u8] = if ptr.is_null() || len == 0 {
    &[]
  } else {
    // SAFETY: libavformat guarantees `extradata` is readable for
    // `extradata_size` bytes (plus its padding) while the parameters
    // live, and the slice is consumed before this function returns.
    unsafe { std::slice::from_raw_parts(ptr, len) }
  };
  // Extradata is a plain allocation with no `AVBufferRef` behind it —
  // an `AVMEDIA_TYPE_ATTACHMENT` stream produces no packets, so a
  // font's bytes never live in a refcounted buffer. **Both** lanes copy
  // here, which is what `from_bytes` is for.
  Ok(AttachmentPacket::new(
    C::from_bytes(bytes).ok_or_else(|| {
      DemuxError::PacketBuffer(PacketBuffer::new(
        index,
        crate::buffer::PacketBufferError::CaptureFailed(crate::buffer::CaptureFailed::new(len)),
      ))
    })?,
    AttachmentPacketExtra::new(index as i32).with_synthesized(true),
  ))
}

/// **The admission pass**: judges every stream in the file before the
/// track table allocates anything at all.
///
/// # Why this cannot live inside the capture
///
/// It used to, and that was a bypass. `build_tracks` deep-copies each
/// stream's `AVCodecParameters` on its way to building a `TrackExtra`,
/// and for an `AVMEDIA_TYPE_ATTACHMENT` stream **the extradata inside
/// those parameters is the attachment's payload**. So the loop paid for
/// the payload — a full `avcodec_parameters_copy` — one statement
/// before asking whether it was allowed to. A file declaring a gigabyte
/// of "font" allocated the gigabyte and then reported that a gigabyte
/// was too much.
///
/// The fix is not a check moved a few lines earlier: any per-track
/// interleaving of judging and paying has the same shape, because the
/// aggregate budget is only knowable once every track has been *seen*.
/// So the whole file is admitted here, in a pass that allocates
/// nothing — it reads two integers per stream — and only a container
/// that passes in full reaches the loop that builds carriers and
/// parameter copies.
///
/// # Why it is every stream, not every attachment
///
/// Because the track table copies **every** stream's codec parameters,
/// and `AVCodecParameters` reaches the heap three ways — `extradata`,
/// every `coded_side_data` entry, a custom channel map — all of them
/// sized by the file. A pass that walked only attachment streams left
/// the other road wide open: a MOV puts an ICC profile in
/// `coded_side_data`, on an ordinary video track, and the wholesale
/// copy took it before anything asked how big it was. That was the same
/// class of defect three review rounds running, which is why the copy
/// itself is gone (see
/// [`bounded_clone_parameters`](crate::extras::bounded_clone_parameters))
/// and why this pass sees everything.
///
/// # What is charged
///
/// The bytes this session will **retain**, which is not always the
/// declared size:
///
/// - every stream is charged its parameter clone's footprint against
///   the per-stream and whole-file codec-parameter budgets;
/// - a synthesized attachment's `extradata` is charged to the
///   *attachment* budget and left out of the parameter one, because the
///   clone strips it and the carrier holds it — one set of bytes, one
///   charge;
/// - the attachment budgets then see:
///
/// - a hoisted cover-art track retains its parked `AVPacket`'s payload
///   *and* the extradata in its parameter copy, which the still decoder
///   may need and which is not a duplicate of the payload;
/// - a synthesized `AVMEDIA_TYPE_ATTACHMENT` track retains only the
///   carrier, because `build_tracks` strips the duplicate extradata out
///   of the parameter copy (see the comment there for the census).
///
/// Charging residency rather than payload is what keeps the budget an
/// honest statement about memory instead of about file structure.
///
/// The per-attachment ceiling is judged first for each track: when a
/// single payload is itself over the line, that is the more specific
/// fact, and naming the aggregate instead would send a reader looking
/// for four hundred attachments that are not there.
fn admit_streams(input: &Input, limits: DemuxLimits) -> Result<(), DemuxError> {
  let mut attachment_spent: usize = 0;
  let mut parameter_spent: usize = 0;
  let mut metadata_spent: usize = 0;

  for stream in input.streams() {
    let index = stream.index();
    let parameters = stream.parameters();
    // SAFETY: `parameters` keeps the `AVCodecParameters` live for this
    // measurement, which allocates nothing and dereferences only what
    // it counts.
    let par = unsafe { parameters.as_ptr() };
    if par.is_null() {
      return Err(DemuxError::ParametersMissing(ParametersMissing::new(index)));
    }

    // **The ruler, judged here rather than during materialisation.**
    //
    // This is the judge-before-pay invariant for a refusal that is not
    // a budget, and it was the third place the invariant failed. A
    // malformed `AVStream.time_base` is a permanent, deterministic fact
    // about the container — it does not depend on how much memory the
    // machine has — so it can and must be decided while nothing has
    // been spent. It used to be decided inside `build_tracks`' loop,
    // which meant a malformed ruler on the *last* stream was refused
    // only after every earlier stream's codec ticket, metadata and
    // attachment carrier had been materialised.
    //
    // Structure before budget, deliberately: a stream that is malformed
    // is a more specific thing to say than a file that is too large,
    // and the two can be true of one container at once.
    let _ruler = stream_timebase(index, stream.time_base())?;

    // **And the layout's structure, for the same reason.** A non-null
    // `ch_layout.opaque`, a custom order with no map or a non-positive
    // count, and a non-null `opaque` on any map entry are all
    // permanent facts about the container that the codec ticket would
    // otherwise discover mid-materialisation — after every earlier
    // stream had been paid for. Deciding them costs a pointer walk and
    // no allocation; see
    // [`validate_channel_layout`](crate::ticket::validate_channel_layout),
    // which the ticket builder calls again as its own first statement.
    //
    // SAFETY: `par` is the live `AVCodecParameters` checked non-null
    // above, owned by `parameters` for this iteration, and for a custom
    // order libavformat filled its map with `nb_channels` entries
    // through `av_channel_layout_copy` — the same argument the demux
    // road's own channel-layout read makes.
    unsafe { crate::ticket::validate_channel_layout(par, index) }?;

    let footprint =
      unsafe { crate::extras::measure_parameters(par) }.ok_or(DemuxError::ParametersTooLarge(
        ParametersTooLarge::new(index, usize::MAX, limits.max_codec_parameter_bytes()),
      ))?;

    // SAFETY: `stream` keeps the `AVStream` live; `disposition` is a
    // public field.
    let disposition = unsafe { (*stream.as_ptr()).disposition };
    let cover_art = is_attachment_disposition(disposition);
    let synthesized = !cover_art && boundary::media_kind_of(&parameters).is_attachment();

    // What the *parameter clone* will retain for this stream. The
    // synthesized-attachment road strips `extradata` — the font's
    // payload rides the carrier instead — so counting it here would
    // charge the same bytes twice and make the budget a statement about
    // the file rather than about memory.
    let retained_parameters = if synthesized {
      footprint.total_without_extradata()
    } else {
      footprint.total()
    }
    .ok_or(DemuxError::ParametersTooLarge(ParametersTooLarge::new(
      index,
      usize::MAX,
      limits.max_codec_parameter_bytes(),
    )))?;

    if retained_parameters > limits.max_codec_parameter_bytes() {
      return Err(DemuxError::ParametersTooLarge(ParametersTooLarge::new(
        index,
        retained_parameters,
        limits.max_codec_parameter_bytes(),
      )));
    }
    parameter_spent = parameter_spent.saturating_add(retained_parameters);
    if parameter_spent > limits.max_total_codec_parameter_bytes() {
      return Err(DemuxError::ParametersBudgetExhausted(
        ParametersBudgetExhausted::new(
          index,
          parameter_spent,
          limits.max_total_codec_parameter_bytes(),
        ),
      ));
    }

    // **And the three metadata values this row will retain**, measured
    // here for the same reason everything else in this pass is: a
    // budget checked during materialisation is a budget that has
    // already been paid. The charge used to live in `build_tracks`'s
    // loop, which meant an over-budget value on the *last* stream was
    // refused only after every earlier stream's parameter clone and
    // attachment carrier had been retained and this stream's ticket
    // copied — so a file certain to be refused could first be made to
    // cost the whole aggregate, on every open.
    //
    // Nothing here allocates: `metadata_value` hands back a borrow of
    // libavutil's own buffer and `lossy_len` prices the decoding
    // without producing it. The keys are read in `build_tracks`' own
    // order so both passes refuse on the same value and name the same
    // key, and both call `charge_metadata`, so there is one rule
    // rather than two.
    //
    // SAFETY: `stream` keeps the `AVStream` — and so its metadata
    // dictionary — live across the reads below, and no borrow outlives
    // this loop iteration.
    let metadata = unsafe { (*stream.as_ptr()).metadata };
    for key in RETAINED_STREAM_METADATA {
      // SAFETY: as above.
      let raw = unsafe { metadata_value(metadata, key) };
      let _admitted = charge_metadata(
        raw,
        &mut metadata_spent,
        limits.max_total_stream_metadata_bytes(),
      )
      .map_err(|fault| stream_metadata_error(index, key, fault, limits))?;
    }

    // And what the *carrier* will hold, for the two attachment roads.
    let carrier = if cover_art {
      // SAFETY: `attached_pic` is an `AVPacket` embedded in the
      // `AVStream` by value; `addr_of!` reaches it without forming a
      // reference to the stream.
      let pkt = unsafe { std::ptr::addr_of!((*stream.as_ptr()).attached_pic) };

      // **The parked packet's own structure, judged here.**
      //
      // `PacketBuffer` is not one fault: it carries a `TRUSTED` payload
      // this crate must not copy, a `data`/`size` pair that does not lie
      // inside the buffer it claims, a buffer somebody else holds a
      // reference to, flags outside the portable set — **and** the
      // allocator declining the carrier. Only the last of those is
      // unforeseeable; the rest are permanent facts about an `AVPacket`
      // that is already parked and already readable. Deciding them in
      // the capture meant a bad final attachment was refused after every
      // earlier stream's ticket, metadata and carrier had been retained.
      //
      // The budget passed here is deliberately `usize::MAX`: the size
      // question belongs to `charge_attachment` below, which answers it
      // as `AttachmentTooLarge` against the attachment seats rather than
      // as a packet's own ceiling. This call is asked only for the
      // structural answers.
      //
      // SAFETY: `pkt` points at the live embedded `AVPacket` for the
      // whole of this call, and no plan outlives it — it is discarded
      // here, the capture happens later against the same packet. It is
      // the stream's parked picture, which is what `AttachedPicture`
      // claims.
      let _plan = unsafe {
        crate::buffer::preflight_payload(
          pkt,
          usize::MAX,
          crate::buffer::PayloadProvenance::AttachedPicture,
        )
      }
      .map_err(|source| DemuxError::PacketBuffer(PacketBuffer::new(index, source)))?;
      // And the flags the packet will be rebuilt with, which is the one
      // remaining deterministic `PacketBuffer` arm.
      //
      // SAFETY: as above.
      unsafe { boundary::md_flags_from_av_packet(pkt) }
        .map_err(|source| DemuxError::PacketBuffer(PacketBuffer::new(index, source)))?;

      // SAFETY: a plain `int` field of the live embedded packet.
      unsafe { (*pkt).size }.max(0) as usize
    } else if synthesized {
      // The **payload**, not the padded clone figure. The carrier is
      // an `FfmpegBytes` over exactly these bytes and the clone omits
      // extradata entirely on this road, so nothing here allocates the
      // padding — charging it would bill sixty-four bytes that are
      // never spent, reject a payload in the last sixty-four below the
      // ceiling, and disagree with the image road about the same file
      // at exactly the cap.
      footprint.extradata_payload()
    } else {
      // Not an attachment: nothing is captured eagerly for it, so
      // nothing more is charged.
      continue;
    };

    charge_attachment(index, carrier, limits, &mut attachment_spent)?;
  }
  Ok(())
}

/// Charges `declared` bytes against both attachment budgets, refusing
/// before anything is copied. The one place a file's attachment
/// spending is decided; see [`admit_streams`] for when it runs.
fn charge_attachment(
  index: usize,
  declared: usize,
  limits: DemuxLimits,
  spent: &mut usize,
) -> Result<(), DemuxError> {
  if declared > limits.max_attachment_bytes() {
    return Err(DemuxError::AttachmentTooLarge(AttachmentTooLarge::new(
      index,
      declared,
      limits.max_attachment_bytes(),
    )));
  }
  let total = spent.saturating_add(declared);
  if total > limits.max_total_attachment_bytes() {
    return Err(DemuxError::AttachmentBudgetExhausted(
      AttachmentBudgetExhausted::new(index, total, limits.max_total_attachment_bytes()),
    ));
  }
  *spent = total;
  Ok(())
}

/// A stream's own ruler, refused rather than repaired — **one
/// conversion and one error, for the two passes that need it**.
///
/// A timebase is file-controlled and there is no honest substitute for
/// it: it is what every timestamp on the track is measured against, so
/// a malformed one makes the track's whole timeline a fabrication
/// rather than a detail. `0/1` — libavformat's own "not set" — is not
/// malformed and is admitted; see [`rational_to_timebase`].
///
/// This exists as a function because the judgement has to happen twice
/// and must not become two judgements. [`admit_streams`] calls it while
/// nothing has been allocated, which is what makes a malformed ruler on
/// the *last* stream refuse the open before the first stream's codec
/// ticket is copied; [`build_tracks`] calls it again where the value is
/// actually used. Two call sites, one rule, and no way for the pass
/// that pays to refuse something the pass that judges admitted.
fn stream_timebase(index: usize, declared: Rational) -> Result<Timebase, DemuxError> {
  rational_to_timebase(declared).ok_or(DemuxError::TrackTimebaseInvalid(TrackTimebaseInvalid::new(
    index,
    declared.numerator(),
    declared.denominator(),
  )))
}

/// A file-controlled `AVRational` as a [`Timebase`], or `None` where it
/// is not one.
///
/// **Nothing is clamped and nothing is invented.** `None` means exactly
/// that [`Timebase`] has no such value: a denominator that is zero or
/// negative, or a negative numerator. A zero numerator *is* admitted,
/// because it is not malformed — `0/1` is libavformat's own "this
/// stream has no timebase yet" default, which ordinary containers carry
/// on untimed streams, and passing it through is reporting rather than
/// guessing. See [`positive_rational_to_timebase`] for the stricter
/// rule the seats that cannot mean *absent* take.
///
/// # Why this is fallible now
///
/// It used to clamp the denominator up to 1 and hand the numerator to
/// `Timebase::new` unexamined, on the argument that a malformed
/// timebase makes one track's timestamps meaningless rather than the
/// file unreadable. The first half of that was a fabrication — a `1/1`
/// invented here is indistinguishable downstream from a `1/1` the file
/// really declared — and the second half was a **panic**:
/// `Timebase::new` asserts a non-negative numerator, and an
/// `AVRational` out of a container can be negative. libavformat's
/// FFMETADATA parser stores `TIMEBASE=-1/1000` verbatim, so sixty bytes
/// of text were enough to abort a safe `open`. Every caller now answers
/// a `None` with a named error instead.
fn rational_to_timebase(value: Rational) -> Option<Timebase> {
  Timebase::try_new(value.numerator(), NonZeroI32::new(value.denominator())?)
}

/// [`rational_to_timebase`], and the numerator must be positive too.
///
/// The rule for a seat where a zero numerator cannot mean "absent".
///
/// A chapter's `time_base` is one such seat: `avpriv_new_chapter` takes
/// it as an argument, so whatever wrote the chapter wrote its ruler
/// too, and `0/den` there is not an unset default but a declaration
/// that every boundary in the table is the same instant — which is the
/// whole content of the row, malformed. A declared frame *rate* is the
/// other: zero frames per second is not a rate.
fn positive_rational_to_timebase(value: Rational) -> Option<Timebase> {
  (value.numerator() > 0)
    .then(|| rational_to_timebase(value))
    .flatten()
}

/// A frame *rate* as a rate-shaped [`Timebase`] (`30000/1001` for
/// 29.97 fps), or `None` when the container declares none.
fn rate_to_timebase(value: Rational) -> Option<Timebase> {
  positive_rational_to_timebase(value)
}

#[cfg(test)]
mod tests {
  use ffmpeg_next::ffi::{av_dict_free, av_dict_set};

  use ffmpeg_next::codec::Parameters;

  use super::*;
  use crate::extras::TrackExtra;

  /// Builds a dictionary holding one entry whose *value* is the given
  /// raw bytes. The bytes go in as a C string, which is all
  /// `av_dict_set` promises to copy — FFmpeg never asks whether they
  /// are UTF-8, which is the whole point of the lane below.
  fn dict_with(key: &CStr, value: &[u8]) -> *mut AVDictionary {
    let mut dict: *mut AVDictionary = std::ptr::null_mut();
    let mut terminated = value.to_vec();
    terminated.push(0);
    let rc = unsafe {
      av_dict_set(
        &mut dict,
        key.as_ptr(),
        terminated.as_ptr().cast::<std::ffi::c_char>(),
        0,
      )
    };
    assert!(rc >= 0, "av_dict_set failed: {rc}");
    dict
  }

  /// **The account is a word-for-word map over libavformat's three
  /// methods, and anything else is `Unknown`.** The method arrives as
  /// the integer it is on the wire, so a value a newer libavformat adds
  /// — or a corrupt one — has to land somewhere without ever forming
  /// the bindgen enum, and it must not be folded into an estimate.
  #[test]
  fn libavformats_estimation_method_becomes_its_own_word() {
    use ffmpeg_next::ffi::AVDurationEstimationMethod as Method;
    assert_eq!(
      duration_source(Method::AVFMT_DURATION_FROM_PTS as i32),
      DurationSource::FromPts,
    );
    assert_eq!(
      duration_source(Method::AVFMT_DURATION_FROM_STREAM as i32),
      DurationSource::FromStream,
    );
    assert_eq!(
      duration_source(Method::AVFMT_DURATION_FROM_BITRATE as i32),
      DurationSource::FromBitrate,
    );
    for unnamed in [3, 7, -1, i32::MAX, i32::MIN] {
      assert_eq!(
        duration_source(unnamed),
        DurationSource::Unknown,
        "method {unnamed} is not one this build names, and is not folded into an estimate",
      );
    }
  }

  /// A PCM WAV built by hand — 8 kHz mono 16-bit silence, `packets`
  /// demux packets long. Hermetic: no `ffmpeg` CLI and no file, so the
  /// lanes below that stand a fault in for libavformat's run anywhere.
  fn silence_wav(packets: usize) -> Vec<u8> {
    let data_len = packets * 4096;
    let mut wav = Vec::with_capacity(44 + data_len);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&((36 + data_len) as u32).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&1u16.to_le_bytes()); // mono
    wav.extend_from_slice(&8_000u32.to_le_bytes()); // sample rate
    wav.extend_from_slice(&16_000u32.to_le_bytes()); // byte rate
    wav.extend_from_slice(&2u16.to_le_bytes()); // block align
    wav.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&(data_len as u32).to_le_bytes());
    wav.resize(44 + data_len, 0);
    wav
  }

  fn open_silence(packets: usize) -> CarrierDemuxer<crate::Owned> {
    CarrierDemuxer::<crate::Owned>::open_reader(
      std::io::Cursor::new(silence_wav(packets)),
      Some("silence.wav"),
    )
    .expect("a hand-built WAV opens")
  }

  fn open_silence_with(packets: usize, limits: DemuxLimits) -> CarrierDemuxer<crate::Owned> {
    CarrierDemuxer::<crate::Owned>::open_reader_with(
      std::io::Cursor::new(silence_wav(packets)),
      Some("silence.wav"),
      limits,
    )
    .expect("a hand-built WAV opens")
  }

  /// Pulls to end of file and counts the packets delivered.
  fn walk(demuxer: &mut CarrierDemuxer<crate::Owned>) -> usize {
    let mut delivered = 0;
    while demuxer.next_packet().expect("pull").is_some() {
      delivered += 1;
    }
    delivered
  }

  /// **A read libavformat answers `InvalidData` to is data the session
  /// passed by, so no figure of the walk is complete.**
  ///
  /// The undisturbed walk is the control: it ends complete. Then a fault
  /// stands in for one read, part-way through — libavformat words
  /// damage differently in every container, and the policy for it is
  /// one — and the walk still carries on and still delivers every
  /// packet, but its figure is no longer one a complete walk stands
  /// behind.
  #[test]
  fn a_corrupt_read_the_session_skips_keeps_the_walk_from_being_complete() {
    let mut clean = open_silence(5);
    let packets = walk(&mut clean);
    assert!(
      packets >= 2,
      "the WAV yields several packets, got {packets}"
    );
    let control = clean.measured_end(TrackIndex::new(0)).expect("a figure");
    assert!(
      control.walk_complete(),
      "control: an undisturbed walk ends complete"
    );

    let mut skipped = open_silence(5);
    for _ in 0..2 {
      assert!(skipped.next_packet().expect("pull").is_some());
    }
    skipped
      .injected_reads
      .push_back(InjectedRead::Fault(ffmpeg_next::Error::InvalidData));
    assert_eq!(
      walk(&mut skipped),
      packets - 2,
      "the walk carried on past the skip and delivered every packet",
    );
    let figure = skipped.measured_end(TrackIndex::new(0)).expect("a figure");
    assert!(
      !figure.walk_complete(),
      "a read was skipped as corrupt, so the walk is not complete",
    );
    assert_eq!(
      figure.end(),
      control.end(),
      "the figure itself is untouched"
    );
  }

  /// **A packet on a stream the table never described is data the
  /// session passed by, so no figure of the walk is complete.**
  ///
  /// The table is fixed at open, so a stream a container adds
  /// afterwards — MPEG-TS and RTP do — has no row. Here that is stood in
  /// for by dropping the only row after one packet has been delivered:
  /// every packet that follows is on an undescribed stream. The first
  /// packet's figure is still on the books and is read past the emptied
  /// table.
  #[test]
  fn a_packet_on_a_stream_the_table_does_not_describe_keeps_the_walk_from_being_complete() {
    let mut demuxer = open_silence(5);
    assert!(demuxer.next_packet().expect("pull").is_some());
    demuxer.tracks.clear();
    assert_eq!(walk(&mut demuxer), 0, "every later packet is passed by");
    let figure = demuxer
      .measured
      .get(0, Timebase::MILLIS)
      .expect("the first packet's figure");
    assert!(
      !figure.walk_complete(),
      "packets were passed by, so the walk is not complete",
    );
  }

  /// **A track nothing can name has packets the session passes by, so
  /// no figure of the walk is complete.** The row is replaced with one
  /// of the `Unknown` kind after a packet has given the track a figure.
  #[test]
  fn a_track_of_unknown_kind_keeps_the_walk_from_being_complete() {
    let mut demuxer = open_silence(5);
    assert!(demuxer.next_packet().expect("pull").is_some());
    let row = &demuxer.tracks[0];
    let unknown = TrackInfo::new(
      row.timebase(),
      TrackParams::Unknown(UnknownTrackParams::new(CodecId::from_raw(0))),
      row.extra().clone(),
    );
    demuxer.tracks[0] = TrackArc::new(unknown);
    assert_eq!(
      walk(&mut demuxer),
      0,
      "no packet of an unnameable track is delivered"
    );
    let figure = demuxer
      .measured
      .get(0, Timebase::MILLIS)
      .expect("the first packet's figure");
    assert!(
      !figure.walk_complete(),
      "packets were passed by, so the walk is not complete",
    );
  }

  /// **A timed packet with no payload is a real endpoint, and the end
  /// of the track can be one.**
  ///
  /// After every real packet of the WAV has been read, a packet with
  /// nothing in it but a timestamp and a duration arrives, a second past
  /// where the real ones stopped. It carries no payload, so it is never
  /// delivered — but it is timed, so the track's measured end moves to
  /// it, and the walk that read it is still complete and the end still
  /// exact. A measurement that filtered on payload first would have
  /// stopped at the last real packet.
  #[test]
  fn a_timed_empty_packet_at_the_end_moves_the_measured_end() {
    let mut clean = open_silence(5);
    let packets = walk(&mut clean);
    let control = clean.measured_end(TrackIndex::new(0)).expect("a figure");
    assert!(
      control.exact(),
      "every real packet carries a pts and a positive duration"
    );
    let tail = control.end().rescale_to(Timebase::HZ_8K).pts();

    let mut demuxer = open_silence(5);
    for _ in 0..packets {
      assert!(demuxer.next_packet().expect("pull").is_some());
    }
    demuxer.injected_reads.push_back(InjectedRead::Packet {
      stream: 0,
      pts: Some(tail),
      duration: 8_000,
      payload: Vec::new(),
    });
    assert!(
      demuxer.next_packet().expect("pull").is_none(),
      "the empty packet is not delivered, and the walk reaches end of file",
    );
    let figure = demuxer.measured_end(TrackIndex::new(0)).expect("a figure");
    assert_eq!(
      figure.end(),
      Timestamp::new(tail + 8_000, Timebase::HZ_8K),
      "the timed empty packet is the end",
    );
    assert!(figure.end() > control.end());
    assert!(
      figure.walk_complete(),
      "an empty packet is observed, not skipped, so the walk is still complete",
    );
    assert!(figure.exact(), "its duration is known");
  }

  /// **A last packet whose duration is unknown ends at its `pts`, and
  /// the end is a lower bound.**
  ///
  /// The same timed empty packet, with the duration libavformat writes
  /// when it does not know one — zero. The end advances to the packet's
  /// `pts`, half a second past where the real packets stopped, and no
  /// further: the walk cannot say where that packet really ends, so the
  /// figure is complete but not exact. A variable-frame-rate track whose
  /// last packet has no derivable duration is this case.
  #[test]
  fn a_last_packet_with_no_duration_ends_at_its_pts_and_the_end_is_a_lower_bound() {
    let mut clean = open_silence(5);
    let packets = walk(&mut clean);
    let control = clean.measured_end(TrackIndex::new(0)).expect("a figure");
    let tail = control.end().rescale_to(Timebase::HZ_8K).pts();

    let mut demuxer = open_silence(5);
    for _ in 0..packets {
      assert!(demuxer.next_packet().expect("pull").is_some());
    }
    demuxer.injected_reads.push_back(InjectedRead::Packet {
      stream: 0,
      pts: Some(tail + 4_000),
      duration: 0,
      payload: Vec::new(),
    });
    assert!(demuxer.next_packet().expect("pull").is_none());
    let figure = demuxer.measured_end(TrackIndex::new(0)).expect("a figure");
    assert_eq!(
      figure.end(),
      Timestamp::new(tail + 4_000, Timebase::HZ_8K),
      "an unknown duration advances the end to the pts and no further",
    );
    assert!(
      figure.walk_complete(),
      "the walk covered the file, whatever the end is"
    );
    assert!(
      !figure.exact(),
      "a packet whose end is unknown leaves the figure a lower bound"
    );
  }

  /// **A packet read with no `pts` has no end to place, and the end is a
  /// lower bound.** The packet is real — it has a payload and is
  /// delivered — and it costs the track its exactness: the mark is made
  /// when the packet is read.
  #[test]
  fn a_packet_read_with_no_pts_makes_the_end_a_lower_bound() {
    let mut clean = open_silence(5);
    let packets = walk(&mut clean);
    let control = clean.measured_end(TrackIndex::new(0)).expect("a figure");

    let mut demuxer = open_silence(5);
    for _ in 0..2 {
      assert!(demuxer.next_packet().expect("pull").is_some());
    }
    demuxer.injected_reads.push_back(InjectedRead::Packet {
      stream: 0,
      pts: None,
      duration: 0,
      payload: vec![0; 64],
    });
    assert_eq!(
      walk(&mut demuxer),
      packets - 2 + 1,
      "the untimed packet is delivered as well as every real one",
    );
    let figure = demuxer.measured_end(TrackIndex::new(0)).expect("a figure");
    assert_eq!(figure.end(), control.end(), "it moved no end");
    assert!(figure.walk_complete());
    assert!(!figure.exact());
  }

  /// **A packet with no `pts` whose conversion is refused still makes
  /// the end a lower bound.**
  ///
  /// The packet is read and then refused — it is over the per-packet
  /// ceiling — so it is never delivered and the walk loses it: the walk
  /// is not complete, and the track's end must not read as exact either,
  /// because a packet whose end the walk could not place was dropped. The
  /// mark is made when the packet is read, not when it is delivered, so
  /// it does not depend on the conversion's outcome. A mark made only on
  /// delivery would leave the end exact here.
  #[test]
  fn a_refused_packet_with_no_pts_still_makes_the_end_a_lower_bound() {
    let limits = DemuxLimits::new()
      .with_packet(crate::limits::PacketLimits::new().with_max_packet_bytes(5_000));
    let mut clean = open_silence_with(5, limits);
    let packets = walk(&mut clean);
    let control = clean.measured_end(TrackIndex::new(0)).expect("a figure");
    assert!(control.exact() && control.walk_complete());

    let mut demuxer = open_silence_with(5, limits);
    for _ in 0..2 {
      assert!(demuxer.next_packet().expect("pull").is_some());
    }
    demuxer.injected_reads.push_back(InjectedRead::Packet {
      stream: 0,
      pts: None,
      duration: 0,
      payload: vec![0; 6_000],
    });
    let (mut delivered, mut refused) = (0, 0);
    loop {
      match demuxer.next_packet() {
        Ok(Some(_)) => delivered += 1,
        Ok(None) => break,
        Err(DemuxError::PacketBuffer(_)) => refused += 1,
        Err(other) => panic!("only the ceiling is expected to refuse here: {other:?}"),
      }
    }
    assert_eq!(refused, 1, "the over-ceiling packet is refused");
    assert_eq!(
      delivered,
      packets - 2,
      "every real packet is still delivered"
    );
    let figure = demuxer.measured_end(TrackIndex::new(0)).expect("a figure");
    assert_eq!(
      figure.end(),
      control.end(),
      "the refused packet moved no end"
    );
    assert!(
      !figure.walk_complete(),
      "a packet was dropped, so the walk did not cover the file",
    );
    assert!(
      !figure.exact(),
      "and the dropped packet had no end the walk could place",
    );
  }

  /// **An empty packet with no timestamp moves no end and breaks no walk,
  /// but it is a packet read with no usable `pts`, so it costs the
  /// track its exactness.** The figure is untouched and the walk stays
  /// complete.
  #[test]
  fn an_untimed_empty_packet_breaks_no_walk_but_leaves_the_end_inexact() {
    let mut clean = open_silence(5);
    let packets = walk(&mut clean);
    let control = clean.measured_end(TrackIndex::new(0)).expect("a figure");
    assert!(control.exact());

    let mut demuxer = open_silence(5);
    for _ in 0..2 {
      assert!(demuxer.next_packet().expect("pull").is_some());
    }
    demuxer.injected_reads.push_back(InjectedRead::Packet {
      stream: 0,
      pts: None,
      duration: 0,
      payload: Vec::new(),
    });
    assert_eq!(walk(&mut demuxer), packets - 2);
    assert_eq!(
      demuxer.measured_end(TrackIndex::new(0)),
      Some(MeasuredEnd::new(control.end(), true, false)),
      "the end is unmoved and the walk complete, but the end is a lower bound",
    );
  }

  /// **The figure libavformat holds before probing survives, and only
  /// that.** A WAV has no duration field: libavformat's reader derives
  /// the length from the data chunk's size when it opens, so the row
  /// carries a figure the reader computed — five 4096-byte chunks of
  /// 16-bit 8 kHz samples is 1.28 s — and no claim that the file
  /// declares it.
  #[test]
  fn a_wav_row_carries_the_length_libavformat_computes_from_its_data_chunk() {
    let demuxer = open_silence(5);
    let pre_probe = demuxer.tracks()[0]
      .duration()
      .expect("libavformat holds a length for the WAV before probing");
    assert_eq!(pre_probe, Timestamp::new(1_280, Timebase::MILLIS));
  }

  #[test]
  fn metadata_that_is_not_utf8_is_read_lossily_not_unsoundly() {
    // The bytes a real container can hold: a Latin-1 "café.ttf" whose
    // 0xE9 is not valid UTF-8 on its own. Read through
    // `DictionaryRef::get` this produced a `&str` that violates the
    // type's invariant — undefined behaviour before anything ever
    // copied it.
    let raw = b"caf\xE9.ttf".to_vec();
    assert!(
      std::str::from_utf8(&raw).is_err(),
      "the source bytes really are not UTF-8",
    );
    let dict = dict_with(c"filename", &raw);
    let mut spent = 0usize;
    let text = retain_metadata(
      unsafe { metadata_value(dict, c"filename") },
      &mut spent,
      usize::MAX,
    )
    .expect("a readable value")
    .expect("the entry exists");
    assert_eq!(text.as_str(), "caf\u{FFFD}.ttf");
    assert_eq!(
      spent,
      "caf\u{FFFD}.ttf".len(),
      "the charge is the decoded size, which the replacement made longer than the raw bytes",
    );
    // A key the dictionary does not hold, and a null dictionary, are
    // both simply absent.
    assert!(matches!(
      unsafe { metadata_value(dict, c"mimetype") },
      MetadataValue::Absent,
    ));
    assert!(matches!(
      unsafe { metadata_value(std::ptr::null(), c"filename") },
      MetadataValue::Absent,
    ));
    unsafe { av_dict_free(&mut { dict }) };
  }

  #[test]
  fn valid_metadata_survives_unchanged() {
    let dict = dict_with(c"mimetype", b"application/x-truetype-font");
    let mut spent = 0usize;
    assert_eq!(
      retain_metadata(
        unsafe { metadata_value(dict, c"mimetype") },
        &mut spent,
        usize::MAX,
      )
      .expect("a readable value")
      .as_deref(),
      Some("application/x-truetype-font"),
    );
    unsafe { av_dict_free(&mut { dict }) };
  }

  #[test]
  fn an_unterminated_length_is_refused_rather_than_truncated() {
    // Nothing libavutil produces is this long; the cap exists so a
    // value it did not terminate cannot walk off the end. A value that
    // reaches the cap is refused — and, since this shape was fixed,
    // refused *visibly*: it is no longer the same answer as absent.
    let long = vec![b'a'; METADATA_VALUE_MAX_BYTES + 1];
    let dict = dict_with(c"filename", &long);
    assert!(matches!(
      unsafe { metadata_value(dict, c"filename") },
      MetadataValue::NotTerminated,
    ));
    unsafe { av_dict_free(&mut { dict }) };
  }

  /// A reader that panics with a payload whose destructor panics in
  /// turn. Both panics are safe code; the second one is what used to
  /// leave the guard and enter the `extern "C"` AVIO callback.
  struct PanicsWithAHostilePayload;

  struct PanicOnDrop;

  impl Drop for PanicOnDrop {
    fn drop(&mut self) {
      panic!("and the payload went too");
    }
  }

  impl std::io::Read for PanicsWithAHostilePayload {
    fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
      std::panic::panic_any(PanicOnDrop);
    }
  }

  impl std::io::Seek for PanicsWithAHostilePayload {
    fn seek(&mut self, _pos: std::io::SeekFrom) -> std::io::Result<u64> {
      std::panic::panic_any(PanicOnDrop);
    }
  }

  #[test]
  fn a_reader_panic_with_a_hostile_payload_does_not_abort_the_process() {
    // In its own process, because the assertion *is* the process: a
    // parent that sees the child exit cleanly has seen the abort not
    // happen. The guard caught the reader's panic and then dropped its
    // payload outside `catch_unwind`, so a payload whose `Drop` panics
    // sent that second panic straight out of `read` and into C —
    // through the very guard that exists to stop it.
    mediaio_ffmpeg::test_support::in_subprocess(
      "demuxer::tests::a_reader_panic_with_a_hostile_payload_does_not_abort_the_process",
      || {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let opened =
          CarrierDemuxer::<crate::Owned>::open_reader(PanicsWithAHostilePayload, Some("x.mkv"));
        std::panic::set_hook(previous);
        match opened {
          Err(DemuxError::ReaderPanic(_)) => {}
          Err(other) => panic!("expected ReaderPanic, got {other:?}"),
          Ok(_) => panic!("a reader that only panics cannot open a container"),
        }
      },
    );
  }

  #[test]
  fn codec_parameters_that_cannot_be_allocated_are_named() {
    // `Parameters::new` does not check `avcodec_parameters_alloc`, and
    // `clone_from` dereferences the result immediately: under a failed
    // allocation the shipped clone would write through null.
    mediaio_ffmpeg::test_support::in_subprocess(
      "demuxer::tests::codec_parameters_that_cannot_be_allocated_are_named",
      || {
        let source = Parameters::new();
        assert!(
          !unsafe { source.as_ptr() }.is_null(),
          "the source allocates before the cap goes on",
        );
        // SAFETY: in `in_subprocess`'s child, alone. The capped window runs only
        // `bounded_clone_parameters`, which checks the allocation it makes.
        unsafe { mediaio_ffmpeg::test_support::cap_ffmpeg_allocations(1) };
        let refused = crate::extras::bounded_clone_parameters(&source, 4, usize::MAX);
        mediaio_ffmpeg::test_support::uncap_ffmpeg_allocations();
        assert!(
          matches!(
            refused,
            Err(ParametersError::Alloc(ref p)) if p.stream_index() == 4
          ),
          "expected ParametersAlloc, got {:?}",
          refused.map(|_| ()),
        );
        // And with the cap lifted the same copy succeeds, so the
        // refusal was the allocator's answer and not a broken helper.
        crate::extras::bounded_clone_parameters(&source, 4, usize::MAX).expect("an uncapped copy");
      },
    );
  }

  #[test]
  fn the_public_track_extra_handoffs_still_answer_the_allocator() {
    // The lane this replaces guarded a hazard that no longer exists:
    // `TrackExtra` derived `Clone` over `ffmpeg_next`'s `Parameters`,
    // whose clone dereferences an unchecked allocation, so safe public
    // code reached a SIGSEGV by copying a track row. The row holds no
    // `Parameters` at all now, and the two public handoffs have split
    // in kind because of it:
    //
    // * `Clone` allocates **nothing from FFmpeg** — it copies an
    //   owned mirror, which is a `Vec` spine and a refcount bump — so
    //   it survives a capped allocator rather than reporting through
    //   one. That is what makes the derive honest under the carrier
    //   law, and a capped allocator is the only way to pin it.
    // * `clone_parameters` is the rebuild, and it is where FFmpeg
    //   allocation moved to. It still answers.
    mediaio_ffmpeg::test_support::in_subprocess(
      "demuxer::tests::the_public_track_extra_handoffs_still_answer_the_allocator",
      || {
        let source = Parameters::new();
        assert!(!unsafe { source.as_ptr() }.is_null(), "allocated uncapped");
        let extra = TrackExtra::new(
          6,
          crate::ticket::CodecTicket::mirror(&source, 6, usize::MAX).expect("uncapped"),
        );

        // SAFETY: in `in_subprocess`'s child, alone. The capped window holds a
        // `TrackExtra` clone, which allocates nothing from FFmpeg, and
        // `clone_parameters`, which checks the allocation it makes.
        unsafe { mediaio_ffmpeg::test_support::cap_ffmpeg_allocations(1) };
        let cloned = extra.clone();
        let handed = extra.clone_parameters().map(|_| ());
        mediaio_ffmpeg::test_support::uncap_ffmpeg_allocations();

        assert_eq!(
          cloned.parameter_bytes(),
          extra.parameter_bytes(),
          "the row cloned under an allocator that refuses everything",
        );
        assert!(
          matches!(handed, Err(ParametersError::Alloc(ref p)) if p.stream_index() == 6),
          "TrackExtra::clone_parameters: {handed:?}",
        );

        // And the rebuild works once the allocator does.
        extra.clone_parameters().expect("an uncapped handoff");
      },
    );
  }

  #[test]
  fn parameters_that_never_allocated_are_refused_at_the_door() {
    // The route the destination check could not see. A safe
    // `Parameters::new()` under a failed allocation hands back a
    // null-backed value and says nothing; the copier then allocated its
    // own destination happily — the allocator having recovered by
    // then — and called `avcodec_parameters_copy(out, NULL)`, which
    // dereferences its source. Same crash, one recovery later, still
    // from safe public code.
    mediaio_ffmpeg::test_support::in_subprocess(
      "demuxer::tests::parameters_that_never_allocated_are_refused_at_the_door",
      || {
        // The cap is on *while the source is built* — that is the whole
        // difference from the destination lane.
        // SAFETY: in `in_subprocess`'s child, alone. `Parameters::new` hands back a
        // null-backed value here, and the lane below only inspects it and hands
        // it to doors that refuse a null.
        unsafe { mediaio_ffmpeg::test_support::cap_ffmpeg_allocations(1) };
        let never_allocated = Parameters::new();
        mediaio_ffmpeg::test_support::uncap_ffmpeg_allocations();
        assert!(
          unsafe { never_allocated.as_ptr() }.is_null(),
          "the safe constructor really does hand back a null-backed value",
        );

        // The door moved with the handle. `TrackExtra::new` no longer
        // takes a `Parameters` at all, so the only way a null-backed
        // one reaches a track row is through the mirror — which is
        // where the check now lives, and where it belongs: beside the
        // raw pointer rather than one type downstream of it.
        let refused = crate::ticket::CodecTicket::mirror(&never_allocated, 9, usize::MAX);
        let Err(ParametersError::Missing(p)) = refused.map(|_| ()) else {
          panic!("a null-backed source must not become a codec ticket");
        };
        assert_eq!(p.stream_index(), 9);

        // And the copier refuses it too, so the invariant is not the
        // only thing standing between this and a null dereference.
        let never_allocated = {
          // SAFETY: as above — the null-backed value is only handed to a door that
          // refuses it.
          unsafe { mediaio_ffmpeg::test_support::cap_ffmpeg_allocations(1) };
          let p = Parameters::new();
          mediaio_ffmpeg::test_support::uncap_ffmpeg_allocations();
          p
        };
        assert!(matches!(
          crate::extras::bounded_clone_parameters(&never_allocated, 9, usize::MAX).map(|_| ()),
          Err(ParametersError::Missing(p)) if p.stream_index() == 9,
        ));

        // A row built over real parameters still hands off both ways,
        // so the refusal is about the null and nothing else.
        let real = Parameters::new();
        let extra = TrackExtra::new(
          9,
          crate::ticket::CodecTicket::mirror(&real, 9, usize::MAX).expect("real parameters"),
        );
        let _ = extra.clone();
        extra.clone_parameters().expect("handoff");
      },
    );
  }

  #[cfg(feature = "resample")]
  #[test]
  fn a_spec_read_from_parameters_that_never_allocated_is_absent() {
    // The same trap at another public door, found by the sweep:
    // `ResampleSpec::from_parameters` asks `parameters.medium()`
    // first, and *that* dereferences the pointer inside ffmpeg-next
    // before any code of ours runs.
    mediaio_ffmpeg::test_support::in_subprocess(
      "demuxer::tests::a_spec_read_from_parameters_that_never_allocated_is_absent",
      || {
        // SAFETY: in `in_subprocess`'s child, alone. `Parameters::new` hands back a
        // null-backed value, which the lane only inspects and hands to
        // `ResampleSpec::from_parameters`, which refuses a null.
        unsafe { mediaio_ffmpeg::test_support::cap_ffmpeg_allocations(1) };
        let never_allocated = Parameters::new();
        mediaio_ffmpeg::test_support::uncap_ffmpeg_allocations();
        assert!(unsafe { never_allocated.as_ptr() }.is_null());
        assert_eq!(
          crate::ResampleSpec::from_parameters(&never_allocated),
          None,
          "parameters that do not exist describe no audio",
        );
      },
    );
  }

  #[test]
  fn codec_parameters_whose_copy_fails_are_named() {
    // The other leg: the destination allocates, and the deep copy of
    // the extradata does not. `clone_from` discards that return value,
    // so the shipped clone handed back parameters missing the very
    // bytes a decoder needs to open — and said nothing.
    mediaio_ffmpeg::test_support::in_subprocess(
      "demuxer::tests::codec_parameters_whose_copy_fails_are_named",
      || {
        const EXTRADATA: usize = 8 * 1024 * 1024;
        let mut source = Parameters::new();
        // SAFETY: `source` owns a live `AVCodecParameters`; the buffer
        // comes from FFmpeg's allocator and is handed to it, so
        // `avcodec_parameters_free` releases it with the rest.
        unsafe {
          let par = source.as_mut_ptr();
          let extradata = ffmpeg_next::ffi::av_mallocz(EXTRADATA) as *mut u8;
          assert!(!extradata.is_null(), "av_mallocz");
          (*par).extradata = extradata;
          (*par).extradata_size = EXTRADATA as i32;
        }

        // Big enough for the destination `AVCodecParameters`, far too
        // small for its extradata.
        // SAFETY: in `in_subprocess`'s child, alone. The capped window runs only
        // `bounded_clone_parameters`, which checks every allocation it makes.
        unsafe { mediaio_ffmpeg::test_support::cap_ffmpeg_allocations(64 * 1024) };
        let refused = crate::extras::bounded_clone_parameters(&source, 2, usize::MAX);
        mediaio_ffmpeg::test_support::uncap_ffmpeg_allocations();
        match refused {
          Err(ParametersError::Copy(p)) => assert_eq!(p.stream_index(), 2),
          Err(other) => panic!("expected ParametersCopy, got {other:?}"),
          Ok(_) => panic!("a copy that could not copy the extradata must not succeed"),
        }
        crate::extras::bounded_clone_parameters(&source, 2, usize::MAX).expect("an uncapped copy");
      },
    );
  }

  /// A stream whose `attached_pic` is `parked`, and the packet
  /// libavformat would queue for it.
  ///
  /// # On the fixture road
  ///
  /// The container shape this guards — a stream carrying
  /// `ATTACHED_PIC | TIMED_THUMBNAILS` — **cannot be minted by the
  /// ffmpeg CLI**, and that was censused rather than assumed: no muxer
  /// has a field for those bits (`-disposition:v
  /// attached_pic+timed_thumbnails` round-trips to nothing through
  /// mp4, mov and matroska alike), because the mov *demuxer* derives
  /// them from a chapter-track reference its own muxer does not write
  /// in that direction.
  ///
  /// What is reproducible, and what actually matters, is the **packet
  /// shape**: `read_frame_internal` queues a stream's parked picture
  /// with `av_packet_ref` while keeping its own reference, which is
  /// exactly what `av_packet_ref` builds here. The classification half
  /// — that such a stream is video rather than an attachment — is
  /// pinned separately by
  /// [`a_timed_thumbnail_stream_is_not_an_attachment`].
  fn parked_picture_stream(parked: &Packet) -> (Box<AVStream>, Packet) {
    use ffmpeg_next::packet::{Mut, Ref};

    let mut stream: Box<AVStream> = Box::new(unsafe { std::mem::zeroed() });
    let mut queued = Packet::empty();
    // SAFETY: `parked` is a live refcounted packet; `av_packet_ref`
    // takes a reference to its buffer, which is precisely what
    // libavformat does when it queues an attached picture. The stream
    // is zeroed apart from the one field the probe reads.
    unsafe {
      assert_eq!(
        ffmpeg_next::ffi::av_packet_ref(queued.as_mut_ptr(), parked.as_ptr()),
        0,
      );
      stream.attached_pic.buf = (*parked.as_ptr()).buf;
      stream.attached_pic.data = (*parked.as_ptr()).data;
      stream.attached_pic.size = (*parked.as_ptr()).size;
    }
    (stream, queued)
  }

  #[test]
  fn a_queued_attached_picture_is_recognised() {
    use ffmpeg_next::packet::Ref;

    let parked = Packet::copy(&[9u8; 2048]);
    let (stream, queued) = parked_picture_stream(&parked);

    // The two references are different structs around one allocation —
    // which is the whole reason the probe compares `buffer` and not the
    // `AVBufferRef`. Asserting the difference is what makes this a test
    // of the right comparison rather than of a lucky one.
    // SAFETY: both packets are live.
    unsafe {
      assert_ne!(
        (*queued.as_ptr()).buf,
        (*parked.as_ptr()).buf,
        "av_packet_ref must mint a new reference struct",
      );
    }
    // SAFETY: the stream is a zeroed `AVStream` whose only populated
    // fields are the ones the probe reads, and `queued` is live.
    assert!(unsafe { packet_is_parked_picture(&*stream, &queued) });

    // An ordinary timed packet — the shape every pull after the first
    // one has — is not the parked picture.
    let ordinary = Packet::copy(&[1u8; 2048]);
    // SAFETY: as above.
    assert!(!unsafe { packet_is_parked_picture(&*stream, &ordinary) });

    // And a stream that parks nothing recognises nothing.
    let bare: Box<AVStream> = Box::new(unsafe { std::mem::zeroed() });
    // SAFETY: as above.
    assert!(!unsafe { packet_is_parked_picture(&*bare, &queued) });
  }

  #[test]
  fn the_queued_picture_is_admitted_and_later_packets_take_the_ordinary_road() {
    use crate::buffer::{PacketBufferError, PayloadProvenance, payload_of};
    use ffmpeg_next::packet::Ref;

    let parked = Packet::copy(&[9u8; 2048]);
    let (_stream, queued) = parked_picture_stream(&parked);
    // SAFETY: the packet is live; `buf` is a public field.
    let parked_buffer = unsafe { (*parked.as_ptr()).buf };

    // **The first pull.** Two references, one of them the container's.
    // From a *caller* that shape is refused, because a caller's second
    // reference may be a `Packet` with a safe `data_mut`.
    // SAFETY: `queued` is live for every call in this test, and nothing in
    // it writes through either packet that shares its buffer, so every
    // provenance claim below holds: no write while a call reads, and none
    // while a carrier lives.
    assert!(matches!(
      unsafe {
        payload_of::<crate::View>(
          queued.as_ptr(),
          usize::MAX,
          PayloadProvenance::CallerSupplied,
        )
      },
      Err(PacketBufferError::SharedPayload(_)),
    ));

    // Delivered by the demux loop, the same shape is carried — by copy,
    // because a window would outlive the exclusivity the read rests on.
    // SAFETY: as above.
    let copied = unsafe {
      payload_of::<crate::View>(
        queued.as_ptr(),
        usize::MAX,
        PayloadProvenance::DemuxDelivered,
      )
    }
    .expect("a demux-delivered shared payload is carriable")
    .expect("it has a payload");
    assert_eq!(copied.as_ref(), &[9u8; 2048][..]);
    // SAFETY: the packet is live; `data` is a public field.
    unsafe {
      assert_ne!(
        copied.as_ref().as_ptr() as usize,
        (*queued.as_ptr()).data as usize,
        "a shared demux-delivered payload is copied, not windowed",
      );
    }

    // With the provenance the probe establishes, both lanes carry it.
    // SAFETY: as above.
    let viewed = unsafe {
      payload_of::<crate::View>(
        queued.as_ptr(),
        usize::MAX,
        PayloadProvenance::AttachedPicture,
      )
    }
    .expect("the container's own picture is carriable")
    .expect("it has a payload");
    assert_eq!(viewed.as_ref(), &[9u8; 2048][..]);
    // And on the view lane it is a window into the parked allocation
    // rather than a copy of it.
    // SAFETY: both are live; `data`/`size` are public fields.
    unsafe {
      let start = (*parked_buffer).data as usize;
      let end = start + (*parked_buffer).size;
      let at = viewed.as_ref().as_ptr() as usize;
      assert!(
        at >= start && at + viewed.len() <= end,
        "the queued picture must be viewed, not copied",
      );
    }
    // SAFETY: as above.
    let owned = unsafe {
      payload_of::<crate::Owned>(
        queued.as_ptr(),
        usize::MAX,
        PayloadProvenance::AttachedPicture,
      )
    }
    .expect("the owned lane carries it too")
    .expect("it has a payload");
    assert_eq!(owned.as_ref(), &[9u8; 2048][..]);

    // **Every pull after it.** A timed packet has a buffer of its own,
    // so it stays on the `Delivered` road, is unique, and the view lane
    // shares it.
    let later = Packet::copy(&[4u8; 1024]);
    // SAFETY: `later` is live, its buffer unshared, and nothing writes
    // through it while the view lives.
    let shared = unsafe {
      payload_of::<crate::View>(
        later.as_ptr(),
        usize::MAX,
        PayloadProvenance::DemuxDelivered,
      )
    }
    .expect("an ordinary packet is carriable")
    .expect("it has a payload");
    // SAFETY: as above.
    unsafe {
      assert_eq!(
        shared.as_ref().as_ptr() as usize,
        (*later.as_ptr()).data as usize,
        "a uniquely-referenced packet is still shared, not copied",
      );
    }
  }

  #[test]
  fn a_timed_thumbnail_stream_is_not_an_attachment() {
    // `TIMED_THUMBNAILS` is documented as only ever appearing beside
    // `ATTACHED_PIC`, so testing the picture bit alone reads a sparse
    // chapter-thumbnail track as cover art — and the attachment
    // contract then delivers exactly one of its images and drops the
    // rest, every one of which had a timestamp.
    assert!(
      is_attachment_disposition(AV_DISPOSITION_ATTACHED_PIC),
      "a plain attached picture is still an attachment",
    );
    assert!(
      !is_attachment_disposition(AV_DISPOSITION_ATTACHED_PIC | AV_DISPOSITION_TIMED_THUMBNAILS),
      "a timed-thumbnail stream is a timed track, whatever else it is flagged",
    );
    // Neither bit, and the other bits that ride along, change nothing.
    assert!(!is_attachment_disposition(0));
    assert!(!is_attachment_disposition(AV_DISPOSITION_TIMED_THUMBNAILS));
    assert!(is_attachment_disposition(
      AV_DISPOSITION_ATTACHED_PIC | ffmpeg_next::ffi::AV_DISPOSITION_DEFAULT
    ));
    // And the reason the raw bits are read at all: the wrapper's own
    // flag set cannot express the distinction.
    assert!(
      ffmpeg_next::format::stream::Disposition::from_bits(AV_DISPOSITION_TIMED_THUMBNAILS)
        .is_none(),
      "ffmpeg_next mints no TIMED_THUMBNAILS bit — from_bits_truncate would drop it silently",
    );
  }

  #[test]
  fn an_uncapturable_cover_still_gets_its_one_packet() {
    // The state the shipped `AwaitingPacket` fallback existed for: a
    // stream that declares cover art and parks no payload. The fallback
    // waited for a packet that may never come, and let timed packets —
    // and seeks — go first, which the face forbids. The track now gets
    // its one packet at open like every other attachment track: empty,
    // and marked as this layer's own work.
    //
    // Not reachable from a file: across MP3, M4A, FLAC and Matroska,
    // every ATTACHED_PIC stream libavformat produces carries the parked
    // packet, because `ff_add_attached_pic` sets the disposition and
    // fills it in the same call. A zeroed `AVPacket` is exactly what
    // `attached_pic` would hold if one ever did not.
    // SAFETY: an all-zero `AVPacket` is a valid empty packet — null
    // buffer and data, zero size.
    let empty: ffmpeg_next::ffi::AVPacket = unsafe { std::mem::zeroed() };
    // SAFETY: `empty` is a live local, and it has no buffer at all, so
    // nothing can write through one while the (owned, empty) carrier
    // lives — the contract's "or a packet nothing writes through".
    let packet = unsafe { attached_pic_payload::<crate::Owned>(&empty, 7, DemuxLimits::default()) }
      .expect("an unparked cover is a degenerate track, not an unreadable file");
    assert!(packet.data().as_ref().is_empty());
    assert!(
      packet.extra().synthesized(),
      "nothing in the container handed this payload over",
    );
    assert_eq!(packet.extra().stream_index(), 7);
  }

  /// **Measure, charge, materialise — and a value the budget refuses is
  /// never materialised at all.**
  ///
  /// The ordering is a property of the types rather than of the
  /// control flow, which is what makes it hold: [`metadata_value`]
  /// hands back a **borrow** of libavutil's buffer, so the size is
  /// known before any owning conversion exists, and the refusal below
  /// happens with nothing on the heap. A zero budget therefore costs
  /// nothing however long the value is.
  #[test]
  fn a_refused_metadata_value_is_never_materialised() {
    let long = vec![b'x'; 4096];
    let mut spent = 0usize;
    match retain_metadata(MetadataValue::Present(&long), &mut spent, 0) {
      Err(MetadataFault::BudgetExhausted(total)) => assert_eq!(total, 4096),
      _ => panic!("a zero budget must refuse a 4096-byte value"),
    }
    assert_eq!(spent, 0, "a refused value does not advance the budget");

    // Under a budget that admits it, the same value is retained and
    // charged its decoded size — once.
    let mut spent = 0usize;
    let text = retain_metadata(MetadataValue::Present(&long), &mut spent, 8192)
      .expect("admitted")
      .expect("present");
    assert_eq!(text.len(), 4096);
    assert_eq!(spent, 4096);

    // And the three outcomes stay apart.
    let mut spent = 0usize;
    assert!(
      retain_metadata(MetadataValue::Absent, &mut spent, 0)
        .expect("absent is not a fault")
        .is_none(),
    );
    assert!(matches!(
      retain_metadata(MetadataValue::NotTerminated, &mut spent, usize::MAX),
      Err(MetadataFault::TooLong),
    ));
    assert_eq!(spent, 0);
  }

  /// **A value with no terminator is not an absent value.**
  ///
  /// The shape that used to erase it: the reader answered `None` for
  /// both, so a declared title of exactly 65,536 bytes reached a
  /// consumer as an untitled chapter, uncharged against any budget.
  #[test]
  fn the_unterminated_case_is_distinct_from_the_absent_one() {
    let mut spent = 0usize;
    assert!(matches!(
      retain_metadata(MetadataValue::NotTerminated, &mut spent, usize::MAX),
      Err(MetadataFault::TooLong),
    ));
    assert!(matches!(
      retain_metadata(MetadataValue::Absent, &mut spent, usize::MAX),
      Ok(None),
    ));
  }

  /// **A malformed rational is refused, never clamped and never a
  /// panic.**
  ///
  /// This replaces a lane that asserted the opposite — that `1/0` came
  /// back as `1/1`. That clamp was a fabrication a consumer could not
  /// tell from a declaration, and it did not cover the case that
  /// actually bites: `Timebase::new` asserts a non-negative numerator,
  /// so a negative one panicked a safe `open`. libavformat stores
  /// `TIMEBASE=-1/1000` out of an FFMETADATA sidecar verbatim, which
  /// made sixty bytes of text enough to abort the process.
  #[test]
  fn a_malformed_rational_is_refused_rather_than_clamped() {
    for (num, den) in [(1, 0), (1, -1000), (-1, 1000), (-1, -1000)] {
      assert_eq!(
        rational_to_timebase(Rational::new(num, den)),
        None,
        "{num}/{den} is not a timebase, and inventing one for it would be indistinguishable \
         downstream from a file that declared it",
      );
    }
  }

  /// **One rule for a stream's ruler, and both passes hold it.**
  ///
  /// The refusal used to live inside `build_tracks`' materialisation
  /// loop, so a malformed ruler on the *last* stream was decided only
  /// after every earlier stream's codec ticket, metadata and attachment
  /// carrier had been paid for. It is decided in `admit_streams` now,
  /// where nothing has been allocated — and by *this* function, which
  /// is the only place the conversion and the error are written, so the
  /// pass that pays cannot refuse something the pass that judges
  /// admitted.
  ///
  /// **On reachability, stated rather than implied.** Unlike
  /// `AVChapter.time_base` — which libavformat stores exactly as an
  /// FFMETADATA sidecar wrote it, `TIMEBASE=-1/1000` included, and
  /// which the lane above pins against a real container — a stream's
  /// timebase normally arrives through `avpriv_set_pts_info`, which
  /// refuses a non-positive value itself. No container was found that
  /// reaches this refusal, so it is a defensive one: demuxers that
  /// assign `st->time_base` directly are not obliged to go through that
  /// helper, and a check whose cost is one comparison is not worth
  /// trading for an assumption about every demuxer in libavformat. What
  /// this lane can pin is the rule and the report; what the ordering
  /// rests on is that the pass it now lives in allocates nothing at
  /// all.
  #[test]
  fn a_stream_ruler_is_refused_by_one_rule_that_names_what_was_declared() {
    for (num, den) in [(1, 0), (1, -1000), (-1, 1000), (-1, -1000)] {
      match stream_timebase(7, Rational::new(num, den)) {
        Err(DemuxError::TrackTimebaseInvalid(fault)) => {
          assert_eq!(fault.stream_index(), 7, "the refusal names the stream");
          assert_eq!(
            (fault.num(), fault.den()),
            (num, den),
            "{num}/{den} is reported as the container wrote it, not as a repair",
          );
        }
        // Split so nothing here relies on `Timebase` being `Debug`.
        Err(other) => panic!("{num}/{den} must be a timebase fault, got {other:?}"),
        Ok(_) => panic!("{num}/{den} must be refused, and it was admitted"),
      }
    }

    // And the two shapes a stream may legitimately carry are admitted:
    // an ordinary ruler, and libavformat's own "never set".
    assert_eq!(
      stream_timebase(0, Rational::new(1, 90_000))
        .map(|tb| (tb.num(), tb.den().get()))
        .ok(),
      Some((1, 90_000)),
    );
    assert_eq!(
      stream_timebase(0, Rational::new(0, 1))
        .map(|tb| (tb.num(), tb.den().get()))
        .ok(),
      Some((0, 1)),
      "`0/1` is not malformed for a track seat — see the lane below",
    );
  }

  /// A zero numerator is the one rational the two rules disagree on,
  /// and the disagreement is the point.
  ///
  /// `0/1` is libavformat's own default for a stream whose demuxer
  /// never set one, so a track seat reads it as the container declaring
  /// nothing. A chapter's ruler is written by whatever wrote the
  /// chapter, so the same value there is a claim that every boundary in
  /// the table falls on one instant — malformed, and refused.
  #[test]
  fn a_zero_numerator_is_absent_for_a_track_and_malformed_for_a_chapter() {
    let zero = Rational::new(0, 1);
    assert_eq!(
      rational_to_timebase(zero).map(|tb| (tb.num(), tb.den().get())),
      Some((0, 1)),
      "the track rule passes libavformat's own unset default through",
    );
    assert_eq!(
      positive_rational_to_timebase(zero),
      None,
      "the strict rule refuses it",
    );
  }

  #[test]
  fn a_declared_frame_rate_becomes_a_rate_shaped_timebase() {
    let ntsc = rate_to_timebase(Rational::new(30_000, 1001)).expect("declared");
    assert_eq!((ntsc.num(), ntsc.den().get()), (30_000, 1001));
    assert_eq!(
      rate_to_timebase(Rational::new(0, 1)),
      None,
      "0 fps is absent"
    );
    assert_eq!(
      rate_to_timebase(Rational::new(30, 0)),
      None,
      "no denominator"
    );
  }

  #[test]
  fn the_seek_timebase_is_microseconds() {
    // `avformat_seek_file` with `stream_index == -1` takes AV_TIME_BASE
    // units; a target expressed in anything else has to arrive there.
    let tb = av_time_base_q();
    assert_eq!((tb.num(), tb.den().get()), (1, 1_000_000));
    let target = Timestamp::new(1_500, Timebase::new(1, NonZeroI32::new(1000).expect("ms")));
    assert_eq!(target.rescale_to(tb).pts(), 1_500_000);
  }

  /// An H.264 packet of length-framed NAL units: an access unit
  /// delimiter, then a B slice of `nal_ref_idc` `reference`.
  fn avc_packet(reference: u8) -> Vec<u8> {
    vec![
      0,
      0,
      0,
      2,
      0x09,
      0xF0,
      0,
      0,
      0,
      4,
      (reference << 5) | 1,
      0x9C,
      0x84,
      0x21,
    ]
  }

  /// A minimal `avcC` of four-byte lengths: a Baseline sequence parameter
  /// set of one macroblock coding frames only (`frame_mbs_only_flag` 1),
  /// without gaps in `frame_num`, its VUI fixing a reorder depth of one
  /// (`bitstream_restriction_flag` 1); and a picture parameter set.
  fn avcc() -> Vec<u8> {
    vec![
      1, 0x42, 0xC0, 0x1E, 0xFF, 0xE1, 0, 12, 0x67, 0x42, 0xC0, 0x1E, 0xDA, 0x7A, 0x01, 0xB4, 0x11,
      0x08, 0xA2, 0xC0, 1, 0, 4, 0x68, 0xEE, 0x3C, 0x80,
    ]
  }

  /// Reads a reference packet, which the reading awaits after its opening
  /// before it marks anything.
  fn past_a_reference(bitstream: &mut Option<NalStream>) {
    let read = marked_from_bitstream(Some(bitstream), video_packet(avc_packet(2), vec![]));
    assert!(!read.flags().contains(PacketFlags::DISPOSABLE));
  }

  fn video_packet(
    data: Vec<u8>,
    side_data: Vec<crate::extras::SideDataEntry>,
  ) -> mediadecode::packet::VideoPacket<crate::extras::VideoPacketExtra, Vec<u8>> {
    mediadecode::packet::VideoPacket::new(
      data,
      crate::extras::VideoPacketExtra::new(0).with_side_data(side_data),
    )
  }

  #[test]
  fn a_non_reference_h264_packet_is_marked_and_a_reference_one_is_not() {
    let mut bitstream = Some(NalStream::new(NalCodec::H264, &avcc()));
    let kept = marked_from_bitstream(Some(&mut bitstream), video_packet(avc_packet(2), vec![]));
    assert!(!kept.flags().contains(PacketFlags::DISPOSABLE));
    let marked = marked_from_bitstream(Some(&mut bitstream), video_packet(avc_packet(0), vec![]));
    assert!(marked.flags().contains(PacketFlags::DISPOSABLE));
    // The marks the packet already carries stay.
    let keyed = video_packet(avc_packet(0), vec![]).with_flags(PacketFlags::KEY);
    let keyed = marked_from_bitstream(Some(&mut bitstream), keyed);
    assert_eq!(keyed.flags(), PacketFlags::KEY | PacketFlags::DISPOSABLE);
  }

  #[test]
  fn a_corrupt_packet_is_read_but_never_marked_and_a_track_without_a_reading_keeps_its_mark() {
    // A corrupt packet that switches the framing: the decoder reads it, so
    // the reading follows, and nothing corrupt is marked.
    let mut bitstream = Some(NalStream::new(NalCodec::H264, &avcc()));
    let annex = vec![0, 0, 0, 1, 0x09, 0xF0, 0, 0, 1, 0x01, 0x9C, 0x84, 0x21];
    let corrupt = video_packet(annex.clone(), vec![]).with_flags(PacketFlags::CORRUPT);
    let corrupt = marked_from_bitstream(Some(&mut bitstream), corrupt);
    assert_eq!(corrupt.flags(), PacketFlags::CORRUPT);
    assert_eq!(
      bitstream.as_ref().and_then(NalStream::framing),
      Some(mediadecode::nal::Framing::AnnexB)
    );
    let corrupt = video_packet(annex.clone(), vec![]).with_flags(PacketFlags::CORRUPT);
    let corrupt = marked_from_bitstream(Some(&mut bitstream), corrupt);
    assert_eq!(corrupt.flags(), PacketFlags::CORRUPT);
    // Past a reference picture, the same packet uncorrupted is marked.
    let reference = vec![0, 0, 0, 1, 0x09, 0xF0, 0, 0, 1, 0x41, 0x9C, 0x84, 0x21];
    let read = marked_from_bitstream(Some(&mut bitstream), video_packet(reference, vec![]));
    assert!(!read.flags().contains(PacketFlags::DISPOSABLE));
    let read = marked_from_bitstream(Some(&mut bitstream), video_packet(annex, vec![]));
    assert!(read.flags().contains(PacketFlags::DISPOSABLE));
    // No reading: the packet keeps what the container said.
    let mut unread: Option<NalStream> = None;
    let passed = marked_from_bitstream(Some(&mut unread), video_packet(avc_packet(0), vec![]));
    assert_eq!(passed.flags(), PacketFlags::empty());
    let container = video_packet(avc_packet(0), vec![]).with_flags(PacketFlags::DISPOSABLE);
    let passed = marked_from_bitstream(None, container);
    assert_eq!(passed.flags(), PacketFlags::DISPOSABLE);
  }

  fn side_data(kind: c_int, data: &[u8]) -> crate::extras::SideDataEntry {
    crate::extras::SideDataEntry::new(
      kind,
      crate::FfmpegBytes::try_copy_from_slice(data).expect("the side data's bytes"),
    )
  }

  #[test]
  fn new_extradata_on_a_packet_reframes_its_track_and_keeps_the_packet() {
    // The track opened as Annex B; the packet brings an `avcC` of
    // four-byte lengths and is read in it — and, carrying a configuration
    // the decoder must see, is not marked.
    let mut bitstream = Some(NalStream::new(NalCodec::H264, &[]));
    let entry = side_data(
      AVPacketSideDataType::AV_PKT_DATA_NEW_EXTRADATA as c_int,
      &avcc(),
    );
    let read = marked_from_bitstream(
      Some(&mut bitstream),
      video_packet(avc_packet(0), vec![entry]),
    );
    assert!(!read.flags().contains(PacketFlags::DISPOSABLE));
    assert_eq!(
      bitstream.as_ref().and_then(NalStream::framing),
      Some(mediadecode::nal::Framing::Length(
        mediadecode::nal::LengthSize::Four
      ))
    );
    // The next packets carry none: past a reference picture, one is marked.
    past_a_reference(&mut bitstream);
    let marked = marked_from_bitstream(Some(&mut bitstream), video_packet(avc_packet(0), vec![]));
    assert!(marked.flags().contains(PacketFlags::DISPOSABLE));
  }

  /// An `hvcC` of two-byte lengths holding `nals`, each after its two-byte
  /// length, in one array.
  fn hvcc(nals: &[&[u8]]) -> Vec<u8> {
    let mut record = vec![0; 23];
    record[0] = 1;
    record[21] = 1;
    record[22] = 1;
    record.push(0x80 | 33);
    record.extend_from_slice(&(nals.len() as u16).to_be_bytes());
    for nal in nals {
      record.extend_from_slice(&(nal.len() as u16).to_be_bytes());
      record.extend_from_slice(nal);
    }
    record
  }

  /// One HEVC picture's first slice segment of `nal_unit_type`, layer 0,
  /// TemporalId 0, after its two-byte length.
  fn hevc_packet(nal_unit_type: u8) -> Vec<u8> {
    vec![0, 3, nal_unit_type << 1, 0x01, 0xAF]
  }

  #[test]
  fn the_picture_that_takes_up_a_record_s_sei_is_not_marked() {
    // An SPS of one sub-layer; a prefix SEI unit of film grain that does
    // not persist.
    let sps: &[u8] = &[0x42, 0x01, 0x01, 0x01, 0x60];
    let grain: &[u8] = &[0x4E, 0x01, 0x13, 0x02, 0x00, 0x02, 0x80];
    let mut bitstream = Some(NalStream::new(NalCodec::Hevc, &hvcc(&[sps])));
    let disposable = |bitstream: &mut Option<NalStream>, data: Vec<u8>, entries| {
      marked_from_bitstream(Some(bitstream), video_packet(data, entries))
        .flags()
        .contains(PacketFlags::DISPOSABLE)
    };
    // A CRA picture starts the sequence, so the decoder skips the RASL
    // picture after it, and the film grain of the record that packet
    // carries waits for the next picture decoded.
    assert!(!disposable(&mut bitstream, hevc_packet(21), vec![]));
    let record = side_data(
      AVPacketSideDataType::AV_PKT_DATA_NEW_EXTRADATA as c_int,
      &hvcc(&[sps, grain]),
    );
    assert!(!disposable(&mut bitstream, hevc_packet(9), vec![record]));
    assert!(!disposable(&mut bitstream, hevc_packet(0), vec![]));
    // That picture took it up.
    assert!(disposable(&mut bitstream, hevc_packet(0), vec![]));
  }

  /// The kinds the bindings name that this build's gates leave out: the
  /// enum's last ones, since FFmpeg appends each new kind, and none where
  /// the gates are the linked FFmpeg's.
  fn left_out() -> core::ops::Range<c_int> {
    let listed = (PERSISTING_SIDE_DATA.len() + RIDING_SIDE_DATA.len()) as c_int;
    listed..AVPacketSideDataType::AV_PKT_DATA_NB as c_int
  }

  #[test]
  fn side_data_that_persists_clears_the_mark_whoever_set_it() {
    // A kind of neither table — one the bindings do not name, or one the
    // build's gates leave out — persists too.
    let persisting = PERSISTING_SIDE_DATA
      .iter()
      .map(|&kind| kind as c_int)
      .chain(left_out())
      .chain([
        AVPacketSideDataType::AV_PKT_DATA_NB as c_int,
        AVPacketSideDataType::AV_PKT_DATA_NB as c_int + 1,
        -1,
        c_int::MAX,
      ]);
    for kind in persisting {
      // The bitstream's mark is not set...
      let mut bitstream = Some(NalStream::new(NalCodec::H264, &avcc()));
      past_a_reference(&mut bitstream);
      let read = marked_from_bitstream(
        Some(&mut bitstream),
        video_packet(avc_packet(0), vec![side_data(kind, &[1])]),
      );
      assert!(!read.flags().contains(PacketFlags::DISPOSABLE), "{kind}");
      // ...and the container's is cleared, with a reading or without.
      let container = video_packet(avc_packet(0), vec![side_data(kind, &[1])])
        .with_flags(PacketFlags::DISPOSABLE | PacketFlags::KEY);
      let cleared = marked_from_bitstream(Some(&mut bitstream), container);
      assert_eq!(cleared.flags(), PacketFlags::KEY, "{kind}");
      let container = video_packet(avc_packet(0), vec![side_data(kind, &[1])])
        .with_flags(PacketFlags::DISPOSABLE);
      let cleared = marked_from_bitstream(None, container);
      assert_eq!(cleared.flags(), PacketFlags::empty(), "{kind}");
    }
    // Side data that rides with its packet leaves the mark to the
    // bitstream.
    for &kind in RIDING_SIDE_DATA {
      let mut bitstream = Some(NalStream::new(NalCodec::H264, &avcc()));
      past_a_reference(&mut bitstream);
      let read = marked_from_bitstream(
        Some(&mut bitstream),
        video_packet(avc_packet(0), vec![side_data(kind as c_int, &[1])]),
      );
      assert!(read.flags().contains(PacketFlags::DISPOSABLE), "{kind:?}");
    }
  }

  #[test]
  fn every_side_data_type_ffmpeg_names_rides_or_persists_and_not_both() {
    let named = AVPacketSideDataType::AV_PKT_DATA_NB as c_int;
    let left_out = left_out();
    // Gated as the linked FFmpeg is, the tables name every kind it names;
    // gated as an older release, they leave out what a newer one appended.
    #[cfg(not(ffmpeg_gate_ceiling))]
    assert!(left_out.is_empty(), "{left_out:?} of {named}");
    #[cfg(ffmpeg_gate_ceiling)]
    assert!(!left_out.is_empty(), "{left_out:?} of {named}");
    for kind in 0..named {
      let persisting = PERSISTING_SIDE_DATA
        .iter()
        .filter(|&&listed| listed as c_int == kind)
        .count();
      let riding = RIDING_SIDE_DATA
        .iter()
        .filter(|&&listed| listed as c_int == kind)
        .count();
      if left_out.contains(&kind) {
        // A kind the gates leave out is in neither table, and persists.
        assert_eq!(persisting + riding, 0, "{kind}");
        assert!(side_data_persists(kind), "{kind}");
        continue;
      }
      assert_eq!(persisting + riding, 1, "{kind}");
      assert_eq!(side_data_persists(kind), persisting == 1, "{kind}");
    }
  }

  #[test]
  fn a_seek_reaches_every_reading() {
    // One reading whose framing moved and one whose did not: after a seek
    // the first marks nothing, the second reads on.
    let mut moved = NalStream::new(NalCodec::H264, &avcc());
    moved.reconfigure(&[0, 0, 0, 1, 0x67, 0x64, 0x00, 0x28]);
    let still = NalStream::new(NalCodec::H264, &avcc());
    let mut bitstreams = vec![Some(moved), None, Some(still)];
    sought(&mut bitstreams);
    assert_eq!(bitstreams[0].as_ref().and_then(NalStream::framing), None);
    assert_eq!(
      bitstreams[2].as_ref().and_then(NalStream::framing),
      Some(mediadecode::nal::Framing::Length(
        mediadecode::nal::LengthSize::Four
      ))
    );
  }
}
