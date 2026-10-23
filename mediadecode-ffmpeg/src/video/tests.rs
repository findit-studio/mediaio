use super::*;

use mediadecode::decoder::VideoStreamDecoder;
use std::{
  num::NonZeroI32,
  sync::{Arc, Mutex},
};

// The hardware-fallback suite runs on the **owned** lane, because it
// replays one `Vec<Packet>` through several decoders: a borrowed source
// can only be copied, which is exactly what that lane is. Nothing the
// suite proves is lane-specific — probe order, keyframe gating, pool
// defence and the exit funnels are all decisions about contexts and
// formats, taken before a carrier exists. The view lane's end-to-end
// decoder coverage lives in `tests/view_carriers.rs`, where the packets
// come from a demuxer that hands them over.
use crate::{FfmpegBytes, FfmpegOwnedVideoStreamDecoder as FfmpegVideoStreamDecoder};

/// A `frame::Video::new` whose planes are written, as a decoder's are.
///
/// `av_frame_get_buffer` allocates the planes without writing them, and
/// a frame reaches the conversions only with [every exported byte
/// written](mediaio_ffmpeg::contracts#every-exported-byte-initialised) —
/// which is also what [`Frame::as_inner_mut`] asks of the frame a fake
/// seam leaves behind.
fn written_video(format: ffmpeg_next::format::Pixel, width: u32, height: u32) -> frame::Video {
  let mut av = frame::Video::new(format, width, height);
  // SAFETY: the frame is live and uniquely owned. Each non-null `buf[i]`
  // is a live `AVBufferRef` whose `data` is valid for `size` bytes,
  // written through the raw pointer without forming a reference to the
  // unwritten bytes.
  unsafe {
    for buf in (*av.as_mut_ptr()).buf {
      if !buf.is_null() {
        core::ptr::write_bytes((*buf).data, 0, (*buf).size);
      }
    }
  }
  av
}

// ---------------------------------------------------------------------------
//  Fake-HW fallback seam: synthetic clip + driver
// ---------------------------------------------------------------------------

/// A synthetic encoded clip: real mpeg4 packets (so the SW decoder genuinely
/// decodes them) plus their key flags and PTS. Encoded with a moving pattern
/// and a fixed GOP so the stream has real keyframes and P-frames.
struct SyntheticClip {
  parameters: ffmpeg_next::codec::Parameters,
  /// Encoded packets in decode order.
  packets: Vec<Packet>,
}

/// Encode a small multi-GOP mpeg4 clip in-process. `gop` forces a keyframe
/// every `gop` frames; a moving diagonal gradient gives the encoder real
/// inter-frame prediction so P-frames actually appear. `max_b_frames == 0`
/// keeps decode order == display order (simple monotonic PTS).
fn encode_synthetic_clip(width: u32, height: u32, frames: usize, gop: u32) -> SyntheticClip {
  use ffmpeg_next as ff;
  ff::init().expect("ffmpeg init");

  let codec = ff::codec::encoder::find(ff::codec::Id::MPEG4).expect("mpeg4 encoder present");
  let ctx = ff::codec::context::Context::new_with_codec(codec);
  let mut enc = ctx.encoder().video().expect("video encoder context");
  enc.set_width(width);
  enc.set_height(height);
  enc.set_format(ff::format::Pixel::YUV420P);
  enc.set_time_base(ff::Rational::new(1, 25));
  enc.set_gop(gop);
  enc.set_max_b_frames(0);
  enc.set_bit_rate(500_000);
  let mut opened = enc.open_as(codec).expect("open encoder");
  let parameters = ff::codec::Parameters::from(&opened);

  let mut packets: Vec<Packet> = Vec::new();
  let drain = |opened: &mut ff::codec::encoder::Video, out: &mut Vec<Packet>| {
    loop {
      let mut pkt = Packet::empty();
      match opened.receive_packet(&mut pkt) {
        Ok(()) => out.push(pkt),
        Err(_) => break,
      }
    }
  };

  // Written before `data_mut` lends it: `av_frame_get_buffer` leaves the
  // planes unwritten.
  let mut frame = written_video(ff::format::Pixel::YUV420P, width, height);
  for i in 0..frames as i64 {
    let ystride = frame.stride(0);
    {
      let data = frame.data_mut(0);
      for y in 0..height as usize {
        for x in 0..width as usize {
          data[y * ystride + x] = ((x + y + i as usize * 4) & 0xff) as u8;
        }
      }
    }
    let cstride = frame.stride(1);
    for p in 1..3usize {
      let data = frame.data_mut(p);
      for y in 0..(height as usize / 2) {
        for x in 0..(width as usize / 2) {
          data[y * cstride + x] = (128 + ((x as i64 - i) & 0x3f)) as u8;
        }
      }
    }
    frame.set_pts(Some(i));
    opened.send_frame(&frame).expect("send_frame");
    drain(&mut opened, &mut packets);
  }
  opened.send_eof().expect("encoder send_eof");
  drain(&mut opened, &mut packets);

  assert!(
    packets.len() >= 8,
    "synthetic clip needs enough packets ({} too few)",
    packets.len()
  );
  assert!(packets[0].is_key(), "first packet must be a keyframe");
  SyntheticClip {
    parameters,
    packets,
  }
}

/// Where a packet or a view carrier keeps its payload: the address its
/// bytes start at, and the `AVBuffer` that holds them.
///
/// The `AVBuffer`, not the `AVBufferRef`: `av_buffer_ref` mints a new
/// reference around the same buffer, so two holders of one allocation
/// differ in the reference and agree here (see `FfmpegBuffer::ptr_eq`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Storage {
  /// Where the payload starts.
  data: usize,
  /// The `AVBuffer` behind the holder's reference, or 0 for none.
  buffer: usize,
}

impl Storage {
  /// Read while `packet` is live.
  fn of_packet(packet: &Packet) -> Self {
    use ffmpeg_next::packet::Ref;
    // SAFETY: `packet` is live; `data` and `buf` are public fields, and
    // a non-null `buf` is the live `AVBufferRef` the packet holds, whose
    // `buffer` field is read as an address and never dereferenced.
    unsafe {
      let raw = packet.as_ptr();
      Self {
        data: (*raw).data as usize,
        buffer: (*raw).buf.as_ref().map_or(0, |held| held.buffer as usize),
      }
    }
  }

  /// The same two facts for a view carrier.
  fn of_carrier(carrier: &crate::FfmpegBuffer) -> Self {
    // SAFETY: a non-null reference is the live `AVBufferRef` the carrier
    // holds; its `buffer` field is read as an address and never
    // dereferenced.
    let held = unsafe { carrier.as_av_buffer_ref().as_ref() };
    Self {
      data: carrier.as_ref().as_ptr() as usize,
      buffer: held.map_or(0, |held| held.buffer as usize),
    }
  }
}

/// The clip's packets as this crate's demuxer delivers them on the view
/// lane, with the track's timebase.
///
/// Muxed into a Matroska file and read back through
/// [`crate::FfmpegDemuxer`], so every carrier is a window onto a buffer
/// libavformat delivered, with libavformat's padding behind the payload —
/// the one shape a view send shares with a decoder
/// (`boundary::share_or_copy`). A carrier converted from a caller's packet
/// never is: the library does not vouch for a caller's padding, and its
/// send is a copy.
fn demuxed_view_packets(clip: &SyntheticClip) -> (Vec<crate::VideoPacket>, Timebase) {
  use ffmpeg_next as ff;
  use std::sync::atomic::{AtomicUsize, Ordering};

  static NEXT: AtomicUsize = AtomicUsize::new(0);
  let path = std::env::temp_dir().join(format!(
    "mediadecode-view-clip-{}-{}.mkv",
    std::process::id(),
    NEXT.fetch_add(1, Ordering::Relaxed),
  ));
  // The encoder's ruler; the container picks its own at the header.
  let encoder_tb = ff::Rational::new(1, 25);
  {
    let mut output = ff::format::output(&path).expect("a Matroska output");
    {
      let mut stream = output.add_stream(ff::codec::Id::MPEG4).expect("a stream");
      stream.set_parameters(clip.parameters.clone());
      stream.set_time_base(encoder_tb);
    }
    output.write_header().expect("the header");
    let stream_tb = output.stream(0).expect("the stream").time_base();
    for packet in &clip.packets {
      let mut packet = packet.clone();
      packet.set_stream(0);
      packet.rescale_ts(encoder_tb, stream_tb);
      packet.write_interleaved(&mut output).expect("a packet");
    }
    output.write_trailer().expect("the trailer");
  }
  let mut demuxer = crate::FfmpegDemuxer::open(&path).expect("open the clip");
  let tb = demuxer.tracks_impl()[0].timebase();
  let mut packets = Vec::new();
  while let Some(packet) = demuxer.next_packet_impl().expect("a packet or the end") {
    if let mediadecode::demuxer::DemuxedPacket::Video(packet) = packet {
      packets.push(packet.into_parts().1);
    }
  }
  drop(demuxer);
  let _ = std::fs::remove_file(&path);
  assert_eq!(
    packets.len(),
    clip.packets.len(),
    "every packet comes back from the container",
  );
  (packets, tb)
}

/// A test HW seam modelling a probe that exhausts.
///
/// * `inert()` — never driven (a placeholder seam).
/// * `never_failing(...)` — delivers a frame 1:1 for the whole clip.
/// * `failing(.., doom_from_send, fail_at_send)` — models a candidate that
///   decodes the early frames fine and then meets content it cannot decode.
///   It delivers a well-formed CPU frame 1:1 for every accepted packet until
///   `doom_from_send`; from that send onward it still *accepts* packets but
///   delivers **no** frames for them; on the `fail_at_send` send it raises
///   the probe's exhaustion — `ProbeExhausted` carrying every packet
///   accepted so far — without accepting that packet.
struct FakeHw {
  width: u32,
  height: u32,
  /// First `send_packet` index (0-based) from which packets are accepted but
  /// no frame is delivered — modelling a HW decoder that buffered packets but
  /// cannot produce frames from them.
  doom_from_send: usize,
  /// `send_packet` index at which to fail. `usize::MAX` => never fail.
  fail_at_send: usize,
  /// Number of `send_packet` calls seen so far.
  sends: usize,
  /// CPU frames queued by accepted pre-doom `send_packet`s, delivered FIFO by
  /// `receive_frame`. Each carries the accepted packet's PTS.
  ///
  /// **Built at send time, not at receive time.** A real hardware seam
  /// has the frame in hand by the time it is asked for; allocating it
  /// inside `receive_frame` made the fake's own allocation the first
  /// thing an allocator ceiling hit, which is not the thing under test
  /// when a lane caps the ceiling to refuse a *carrier*.
  queued: VecDeque<frame::Video>,
  /// Copies of every packet accepted so far, made as the probe makes
  /// them (`boundary::try_clone_packet`) — the probe's
  /// `unconsumed_packets` history, surfaced when it exhausts.
  history: Vec<Packet>,
  /// Where each `send_packet`'s packet kept its payload, read while it
  /// was live. Shared, so a lane can read it after the seam has moved
  /// into a decoder: see [`FakeHw::submitted`].
  submitted: Arc<Mutex<Vec<Storage>>>,
  /// When set, raise a **probe-era** exhaustion from `receive_frame`
  /// rather than from `send_packet`.
  ///
  /// That is the decoder's *second* delivery path onto the replay
  /// queue: `fall_back_to_sw` fills it inside the `receive_frame` call
  /// and the head is converted there and then. Reaching it needs a
  /// hardware seam that fails at frame time, which nothing else here
  /// does.
  fail_at_receive: bool,
  /// When set, raise the probe's exhaustion from `send_eof` rather than
  /// taking the end — the EOF road, carrying the history recorded so far.
  fail_at_eof: bool,
}

impl FakeHw {
  fn inert() -> Self {
    Self {
      width: 0,
      height: 0,
      doom_from_send: usize::MAX,
      fail_at_send: usize::MAX,
      sends: 0,
      queued: VecDeque::new(),
      history: Vec::new(),
      submitted: Arc::default(),
      fail_at_receive: false,
      fail_at_eof: false,
    }
  }

  fn failing(width: u32, height: u32, doom_from_send: usize, fail_at_send: usize) -> Self {
    Self {
      width,
      height,
      doom_from_send,
      fail_at_send,
      sends: 0,
      queued: VecDeque::new(),
      history: Vec::new(),
      submitted: Arc::default(),
      fail_at_receive: false,
      fail_at_eof: false,
    }
  }

  /// Accepts every packet, then raises probe-era exhaustion the first
  /// time a frame is asked for — the receive-time fallback road.
  fn failing_at_receive(width: u32, height: u32) -> Self {
    let mut hw = Self::failing(width, height, 0, usize::MAX);
    hw.fail_at_receive = true;
    hw
  }

  /// Accepts every packet, then raises the probe's exhaustion when the
  /// end of the stream is offered — the EOF road.
  fn failing_at_eof(width: u32, height: u32) -> Self {
    let mut hw = Self::failing(width, height, 0, usize::MAX);
    hw.fail_at_eof = true;
    hw
  }

  /// Never fails — stays on the HW path for the whole clip, delivering 1:1.
  fn never_failing(width: u32, height: u32) -> Self {
    Self::failing(width, height, usize::MAX, usize::MAX)
  }

  /// A handle on where each send's packet kept its payload.
  fn submitted(&self) -> Arc<Mutex<Vec<Storage>>> {
    Arc::clone(&self.submitted)
  }
}

impl HwInner for FakeHw {
  unsafe fn send_packet(&mut self, packet: &Packet) -> Result<Sent, Error> {
    self
      .submitted
      .lock()
      .expect("no lane panics while holding the record")
      .push(Storage::of_packet(packet));
    let idx = self.sends;
    self.sends += 1;
    if idx == self.fail_at_send {
      // The packet is NOT accepted: the probe exhausts, handing back every
      // packet it took.
      return Err(Error::ProbeExhausted(crate::error::ProbeExhausted::new(
        Vec::new(),
        std::mem::take(&mut self.history),
      )));
    }
    // Accept the packet. Track it as probe-era history, and deliver a frame for
    // it only before the doomed span.
    // SAFETY: the seam's contract: `packet` meets the `AVPacket`
    // contract with every byte written.
    if let Ok(cloned) = unsafe { crate::boundary::try_clone_packet(packet) } {
      self.history.push(cloned);
    }
    if idx < self.doom_from_send {
      let mut av = written_video(ffmpeg_next::format::Pixel::YUV420P, self.width, self.height);
      av.set_pts(packet.pts());
      self.queued.push_back(av);
    }
    Ok(Sent::Accepted)
  }

  fn receive_frame(&mut self, frame: &mut Frame) -> Result<Received, Error> {
    if self.fail_at_receive {
      // Once: the decoder falls back and never asks this seam again.
      self.fail_at_receive = false;
      return Err(Error::ProbeExhausted(crate::error::ProbeExhausted::new(
        Vec::new(),
        std::mem::take(&mut self.history),
      )));
    }
    match self.queued.pop_front() {
      Some(av) => {
        // SAFETY: the slot is replaced by a whole `frame::Video`, its geometry and
        // buffers together, every plane written.
        *unsafe { frame.as_inner_mut() } = av;
        Ok(Received::Produced)
      }
      None => Ok(Received::NeedsInput),
    }
  }

  fn send_eof(&mut self) -> Result<Sent, Error> {
    if self.fail_at_eof {
      // Once: the probe is spent with this answer.
      self.fail_at_eof = false;
      return Err(Error::ProbeExhausted(crate::error::ProbeExhausted::new(
        Vec::new(),
        std::mem::take(&mut self.history),
      )));
    }
    Ok(Sent::Accepted)
  }

  fn flush(&mut self) -> Result<(), Error> {
    self.queued.clear();
    Ok(())
  }

  fn as_video_decoder(&self) -> Option<&VideoDecoder> {
    None
  }
}

/// A HW seam whose probe exhausts at `send_eof` — the only way to drive the
/// `send_eof` fallback arm (the general [`FakeHw`]'s `send_eof` always
/// succeeds). Every `send_packet` is accepted and queued for FIFO delivery,
/// and the exhaustion hands back no history, as a probe that recorded
/// nothing would.
struct FakeHwEofFails {
  width: u32,
  height: u32,
  /// PTS of accepted packets, delivered FIFO by `receive_frame`.
  queued: VecDeque<i64>,
}

impl FakeHwEofFails {
  fn new(width: u32, height: u32) -> Self {
    Self {
      width,
      height,
      queued: VecDeque::new(),
    }
  }
}

impl HwInner for FakeHwEofFails {
  unsafe fn send_packet(&mut self, packet: &Packet) -> Result<Sent, Error> {
    self.queued.push_back(packet.pts().unwrap_or(0));
    Ok(Sent::Accepted)
  }

  fn receive_frame(&mut self, frame: &mut Frame) -> Result<Received, Error> {
    match self.queued.pop_front() {
      Some(pts) => {
        let mut av = written_video(ffmpeg_next::format::Pixel::YUV420P, self.width, self.height);
        av.set_pts(Some(pts));
        // SAFETY: the slot is replaced by a whole `frame::Video`, its geometry and
        // buffers together, every plane written.
        *unsafe { frame.as_inner_mut() } = av;
        Ok(Received::Produced)
      }
      None => Ok(Received::NeedsInput),
    }
  }

  fn send_eof(&mut self) -> Result<Sent, Error> {
    Err(Error::ProbeExhausted(crate::error::ProbeExhausted::new(
      Vec::new(),
      Vec::new(),
    )))
  }

  fn flush(&mut self) -> Result<(), Error> {
    self.queued.clear();
    Ok(())
  }

  fn as_video_decoder(&self) -> Option<&VideoDecoder> {
    None
  }
}

/// A hardware seam whose `send_eof` answers back pressure rather than
/// taking the end. Nothing else about it matters.
struct FakeHwEofBackpressures;

impl HwInner for FakeHwEofBackpressures {
  unsafe fn send_packet(&mut self, _: &Packet) -> Result<Sent, Error> {
    Ok(Sent::Accepted)
  }
  fn receive_frame(&mut self, _: &mut Frame) -> Result<Received, Error> {
    Ok(Received::NeedsInput)
  }
  fn send_eof(&mut self) -> Result<Sent, Error> {
    Ok(Sent::MustDrain)
  }
  fn flush(&mut self) -> Result<(), Error> {
    Ok(())
  }
  fn as_video_decoder(&self) -> Option<&VideoDecoder> {
    None
  }
}

/// **Class audit: `is_ok()` is not the commit test, and this is the
/// ordering that proves it.**
///
/// `send_eof` answering `Ok(Sent::MustDrain)` means the end-of-stream
/// was **not** recorded — but it is still an `Ok`. Committing
/// `eof_sent` off `is_ok()` would mark the transaction done for a
/// signal the decoder never took, and a *later* fallback would then
/// inject an EOF into the freshly-opened software decoder on the
/// strength of it. That is the same half-mutation the failed-fallback
/// lane above guards from the error side; this guards it from the side
/// the send-status vocabulary opened.
#[test]
fn back_pressured_eof_does_not_commit_the_eof_transaction() {
  let mut dec = unopenable_sw_decoder(Box::new(FakeHwEofBackpressures));
  assert!(
    !dec.eof_sent_for_test(),
    "precondition: eof_sent starts false"
  );

  for _ in 0..3 {
    assert!(
      matches!(dec.send_eof(), Ok(Sent::MustDrain)),
      "the seam refuses the end until its output is drained",
    );
    assert!(
      !dec.eof_sent_for_test(),
      "an end-of-stream that was not taken must not commit the transaction",
    );
  }
}

/// Drive the decoder over `clip`, draining every available frame after each
/// `send_packet` and after EOF. Returns the PTS of every delivered frame in
/// order. A `None` PTS surfaces as `i64::MIN` so a hole is visible.
fn drive(dec: &mut FfmpegVideoStreamDecoder, clip: &SyntheticClip) -> Vec<i64> {
  let mut out: Vec<i64> = Vec::new();
  let mut dst = crate::empty_owned_video_frame();

  let mut drain_frames = |dec: &mut FfmpegVideoStreamDecoder, out: &mut Vec<i64>| {
    loop {
      match dec.receive_frame(&mut dst) {
        Ok(Received::Produced) => out.push(dst.pts().map(|t| t.pts()).unwrap_or(i64::MIN)),
        Ok(Received::NeedsInput | Received::Ended) => break,
        Err(e) => panic!("receive_frame: {e:?}"),
      }
    }
  };

  for av_pkt in &clip.packets {
    // SAFETY: the packet came from `avcodec_receive_packet` or `av_read_frame`, which wrote its
    // payload.
    let vpkt =
      unsafe { boundary::video_packet_from_ffmpeg(av_pkt, mediadecode::Timebase::SECONDS) }
        .expect("a wrappable payload")
        .expect("packet has a buffer");
    crate::accepted(dec.send_packet(&vpkt), "send_packet");
    drain_frames(dec, &mut out);
  }
  crate::accepted(dec.send_eof(), "send_eof");
  drain_frames(dec, &mut out);
  out
}

/// Sanity: with no injected failure the fake HW stays on the HW path for the
/// whole clip and delivers one frame per packet. Guards against the seam itself
/// dropping frames or spuriously falling back.
#[test]
fn fake_hw_without_failure_stays_on_hardware() {
  let (w, h) = (128u32, 96u32);
  let clip = encode_synthetic_clip(w, h, 12, 6);

  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));
  let mut dec = FfmpegVideoStreamDecoder::from_hw_inner_for_test(
    Box::new(FakeHw::never_failing(w, h)),
    clip.parameters.clone(),
    tb,
  )
  .expect("build test decoder");

  let pts_out = drive(&mut dec, &clip);

  assert!(dec.is_hardware(), "no failure => stays on the HW seam");
  assert_eq!(
    pts_out.len(),
    clip.packets.len(),
    "HW path must deliver one frame per packet"
  );
}

// ---------------------------------------------------------------------------
//  Probe-era fallback: lossless
// ---------------------------------------------------------------------------

/// A HW failure **before the first frame** surfaces the decoder's buffered
/// history in `unconsumed_packets`, which the wrapper replays losslessly
/// through SW (then forwards the still-unconsumed current packet). No frame was
/// ever delivered on the HW path, so every source frame must come out exactly
/// once — a probe-era fallback loses nothing.
#[test]
fn probe_era_failure_replays_history_losslessly() {
  let (w, h) = (128u32, 96u32);
  let clip = encode_synthetic_clip(w, h, 16, 6);

  // Fail a few packets in WITHOUT delivering any frame first (doom_from_send =
  // 0 => nothing is delivered on HW; every accepted packet is buffered as
  // probe history). The failing packet is not accepted; the buffered history is
  // packets [0, fail_at).
  let fail_at = 5;
  assert!(fail_at < clip.packets.len(), "fail target in range");

  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));
  let mut dec = FfmpegVideoStreamDecoder::from_hw_inner_for_test(
    Box::new(FakeHw::failing(w, h, 0, fail_at)),
    clip.parameters.clone(),
    tb,
  )
  .expect("build test decoder");
  assert!(dec.is_hardware(), "must start on the HW seam");

  let pts_out = drive(&mut dec, &clip);

  assert!(
    dec.is_software(),
    "probe-era HW failure must trigger the SW fallback"
  );
  // Lossless: the replayed history + forwarded current packet + the remaining
  // forwarded packets reconstruct the whole stream — every PTS exactly once.
  assert!(
    !pts_out.contains(&i64::MIN),
    "no delivered frame may have a missing PTS: {pts_out:?}"
  );
  let mut sorted = pts_out.clone();
  sorted.sort_unstable();
  let expected: Vec<i64> = (0..clip.packets.len() as i64).collect();
  assert_eq!(
    sorted, expected,
    "a probe-era fallback must lose no frames — every source PTS delivered \
     exactly once: {pts_out:?}"
  );
}

/// `UYVY422`, a CPU layout a [`Frame`] cannot carry: what a stood-in
/// download chooses for an unfit candidate
/// ([`crate::decoder::DownloadsForTest`]).
const UYVY: i32 = ffmpeg_next::ffi::AVPixelFormat::AV_PIX_FMT_UYVY422 as i32;

/// LAW: **under `Auto`, a first candidate unfit for the stream hands
/// software the whole stream** (the ruling on Codex R8's finding 5, item
/// 6a). The probe's only candidate decodes the first picture, and its
/// download chooses `UYVY422`, a layout a [`Frame`] cannot carry. That is no
/// delivery, so the candidate is unfit, and with no candidate left the probe
/// is exhausted. Its record is one attempt, the unfitness naming the
/// layout, with no errno anywhere, and every packet it took. Under
/// [`DecodePath::Auto`] they are replayed into software, which decodes the
/// whole stream: every picture once.
///
/// The candidate is the real probe, software-backed
/// ([`VideoDecoder::from_software_for_test_as`]), its downloads stood in for
/// ([`crate::decoder::DownloadsForTest`]). The probe is read alone first,
/// then wrapped in the stream decoder: the same construction both times.
///
/// PLANT: an unfit attempt recorded as a synthesized errno, as R8's `EINVAL`
/// was, turns this red at "no errno anywhere".
#[test]
fn under_auto_an_unfit_first_candidate_hands_software_the_whole_stream() {
  let (w, h) = (128u32, 96u32);
  let clip = encode_synthetic_clip(w, h, 12, 6);
  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));
  let _downloads = crate::decoder::DownloadsForTest::set(|_| Some(UYVY));
  let candidate = || {
    let codec = ffmpeg_next::decoder::find(ffmpeg_next::codec::Id::MPEG4).expect("mpeg4");
    VideoDecoder::from_software_for_test_as(
      try_clone_parameters(&clip.parameters, usize::MAX).expect("the parameters"),
      codec,
      DecoderLimits::default(),
      crate::DecoderOptions::new(),
      /*auditioning=*/ true,
    )
    .expect("a software-backed candidate on trial")
  };

  // The probe alone: exhausted at its first picture, its record the
  // unfitness.
  let mut probe = candidate();
  let mut frame = Frame::empty().expect("frame slot");
  let mut sent = 0;
  let exhausted = loop {
    // SAFETY: the encoder wrote every byte of each packet, its padding
    // zeroed, and nothing writes through it while the decoder may read it.
    let accepted = unsafe { probe.send_av_packet(&clip.packets[sent]) };
    assert!(matches!(accepted, Ok(Sent::Accepted)), "{accepted:?}");
    sent += 1;
    match probe.receive_frame(&mut frame) {
      Err(Error::ProbeExhausted(exhausted)) => break exhausted,
      Ok(Received::NeedsInput) => {}
      other => panic!("the probe exhausted at its first picture: {other:?}"),
    }
  };
  assert_eq!(exhausted.attempts().len(), 1, "one attempt");
  assert!(
    exhausted
      .attempts()
      .iter()
      .all(|(_, attempt)| matches!(**attempt, Error::DownloadFormatUnsupported(layout) if layout.format() == UYVY)),
    "the unfitness, naming the layout, no errno anywhere: {:?}",
    exhausted.attempts()
  );
  assert_eq!(
    exhausted.unconsumed_packets().len(),
    sent,
    "every packet it took"
  );

  // Under `Auto`: software, the whole stream.
  let mut dec = FfmpegVideoStreamDecoder::from_hw_inner_for_test_as(
    Box::new(candidate()),
    clip.parameters.clone(),
    tb,
    DecodePath::Auto,
  )
  .expect("build test decoder");
  assert!(dec.is_hardware(), "starts on the probe");
  let pts_out = drive(&mut dec, &clip);
  assert!(dec.is_software(), "software took the stream");
  let mut sorted = pts_out.clone();
  sorted.sort_unstable();
  assert_eq!(
    sorted,
    (0..clip.packets.len() as i64).collect::<Vec<_>>(),
    "every picture once: {pts_out:?}"
  );
}

// ---------------------------------------------------------------------------
//  Transactional SW-open failure: stays on HW, surfaces FallbackFailed
// ---------------------------------------------------------------------------

/// A decoder whose stored `parameters` cannot open a SW decoder. An empty
/// `Parameters` has codec id `NONE`, so `open_sw_decoder` (`build_codec_context`
/// → `.decoder().video()`) fails — exactly the SW-open failure the transactional
/// rollback must survive.
fn unopenable_sw_decoder(hw: Box<dyn HwInner>) -> FfmpegVideoStreamDecoder {
  ffmpeg_next::init().expect("ffmpeg init");
  let params = ffmpeg_next::codec::Parameters::new();
  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));
  FfmpegVideoStreamDecoder::from_hw_inner_for_test(hw, params, tb).expect("build test decoder")
}

/// On a probe-era fallback whose SW decoder fails to OPEN, the transition is
/// transactional: the wrapper surfaces `FallbackFailed` (carrying the rescued
/// packets — empty here, the failure landing on the first packet) and stays
/// on the HW state. It must NOT silently commit a broken SW decoder or lose
/// the HW path.
#[test]
fn probe_era_sw_open_failure_stays_on_hw_transactionally() {
  let (w, h) = (64u32, 64u32);
  // The probe exhausts on the very first send. The stored `Parameters` are
  // empty, so `open_sw_decoder` fails and the fallback must roll back to HW.
  let mut dec = unopenable_sw_decoder(Box::new(FakeHw::failing(w, h, 0, 0)));
  assert!(dec.is_hardware(), "must start on the HW seam");

  // Build a throwaway packet to send (content is irrelevant — HW fails before
  // touching it). Written, all the same: the conversion copies it.
  let mut raw = Packet::copy(&[0u8; 16]);
  raw.set_pts(Some(0));
  // SAFETY: `Packet::copy` wrote every byte of the payload.
  let vpkt = unsafe { boundary::video_packet_from_ffmpeg(&raw, mediadecode::Timebase::SECONDS) }
    .expect("a wrappable payload")
    .expect("packet has a buffer");

  let err = dec
    .send_packet(&vpkt)
    .expect_err("SW-open failure must surface an error");
  match err {
    VideoDecodeError::Decode(Error::FallbackFailed(_)) => {}
    other => panic!("expected FallbackFailed on SW-open failure, got {other:?}"),
  }
  assert!(
    dec.is_hardware(),
    "a failed fallback (SW could not open) must leave the decoder on its prior \
     HW state — transactional rollback, not a half-committed SW"
  );
}

// ---------------------------------------------------------------------------
//  Drain-error propagation: a non-transient SW decode error surfaces
// ---------------------------------------------------------------------------

/// Zero a packet's payload in place — enough to make the mpeg4 SW decoder
/// reject it with `InvalidData` ("header damaged") when it tries to decode it.
fn corrupt_packet_payload(pkt: &mut Packet) {
  if let Some(d) = pkt.data_mut() {
    for b in d.iter_mut() {
      *b = 0;
    }
  }
}

/// A non-transient SW decode error during the fallback replay drain must
/// SURFACE (as `FallbackFailed` carrying the replay packets), not be swallowed
/// and the fallback silently committed over corruption. Exercised via the
/// **probe-era** replay path (the only path that replays packets): we poison a
/// P-frame in the buffered history the SW decoder replays; when the drain
/// decodes it the SW decoder returns `InvalidData`, which the drain propagates.
///
/// Without the drain-error fix the drain treats `InvalidData` like EAGAIN/EOF
/// (`break`), swallowing it: the fallback "succeeds", masking the corruption.
#[test]
fn sw_replay_drain_surfaces_non_transient_decode_error() {
  let (w, h) = (128u32, 96u32);
  // Single long GOP so the whole buffered history (with the corrupt packet) is
  // replayed on the probe-era fallback.
  let mut clip = encode_synthetic_clip(w, h, 12, 100);
  let p1 = clip
    .packets
    .iter()
    .position(|p| !p.is_key())
    .expect("clip has P-frames");
  assert!(
    p1 + 2 < clip.packets.len(),
    "need packets after the corrupt one"
  );
  corrupt_packet_payload(&mut clip.packets[p1]);

  // Probe-era: deliver NO frames (doom_from_send = 0), accept-and-buffer every
  // packet as history, then fail probe-era a few packets after the corrupt one.
  // The buffered history the SW decoder replays is {keyframe, corrupt_P, P, ...}
  // → the drain surfaces InvalidData.
  let fail_at = p1 + 3;
  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));
  let mut dec = FfmpegVideoStreamDecoder::from_hw_inner_for_test(
    Box::new(FakeHw::failing(w, h, 0, fail_at)),
    clip.parameters.clone(),
    tb,
  )
  .expect("build test decoder");

  let mut dst = crate::empty_owned_video_frame();
  let mut err = None;
  for av_pkt in &clip.packets {
    // SAFETY: the packet came from `avcodec_receive_packet` or `av_read_frame`, which wrote its
    // payload.
    let vpkt =
      unsafe { boundary::video_packet_from_ffmpeg(av_pkt, mediadecode::Timebase::SECONDS) }
        .expect("a wrappable payload")
        .expect("packet has a buffer");
    if let Err(e) = dec.send_packet(&vpkt) {
      err = Some(e);
      break;
    }
    loop {
      match dec.receive_frame(&mut dst) {
        Ok(Received::Produced) => {}
        Ok(Received::NeedsInput | Received::Ended) => break,
        Err(e) => {
          err = Some(e);
          break;
        }
      }
    }
    if err.is_some() {
      break;
    }
  }

  let err = err.expect("the corrupt replayed packet must surface an error, not be swallowed");
  match err {
    VideoDecodeError::Decode(Error::FallbackFailed(f)) => {
      assert!(
        !f.unconsumed_packets().is_empty(),
        "FallbackFailed must carry the replay packets for recovery"
      );
      assert!(
        matches!(f.source(), Error::Ffmpeg(ffmpeg_next::Error::InvalidData)),
        "the surfaced error must be the SW InvalidData decode failure; got {:?}",
        f.source()
      );
    }
    other => panic!("expected FallbackFailed surfacing InvalidData, got {other:?}"),
  }

  assert!(
    dec.is_hardware(),
    "a failed fallback must leave the decoder on its prior (HW) state, not \
     commit SW over swallowed corruption"
  );
}

/// The transactional commit boundary: SW **ACCEPTS every replayed packet**
/// (no EAGAIN backpressure, so the mid-replay drains never fire) and only then
/// returns `InvalidData` from `receive_frame`. The drain-before-commit must
/// catch that deferred error so it surfaces as `FallbackFailed` (rescued
/// packets retained) and the decoder stays HW — NOT as a plain decode error
/// after a half-done commit (frames appended + `state` flipped to `Sw` +
/// rescued packets dropped), which would break probe-era recovery on
/// non-seekable input.
///
/// This is the deferred-error counterpart to
/// `sw_replay_drain_surfaces_non_transient_decode_error`: there the corrupt
/// packet sits mid-history so a *subsequent send's* EAGAIN-drain decodes it
/// early; here the corrupt packet is the LAST in the buffered history, so no
/// per-send drain ever touches it — only the final drain-before-commit does.
/// Without that drain the fallback would commit and the `InvalidData` would
/// reach the caller plainly on the first `receive_frame` after the fallback
/// committed.
#[test]
fn sw_replay_deferred_error_surfaces_fallback_failed_at_commit() {
  let (w, h) = (128u32, 96u32);
  // Single long GOP so the whole prefix is one replayed history with no
  // intervening keyframe; corrupt the LAST P-frame of that prefix.
  let mut clip = encode_synthetic_clip(w, h, 12, 100);
  // `fail_at` is probe-era: the buffered history is packets [0, fail_at). Put
  // the corrupt packet at fail_at - 1 (the last replayed packet) so the only
  // decode of it happens in the final drain-before-commit.
  let fail_at = 5;
  assert!(
    fail_at >= 2 && fail_at < clip.packets.len(),
    "need a multi-packet history with room for a corrupt tail"
  );
  let corrupt_idx = fail_at - 1;
  assert!(
    !clip.packets[corrupt_idx].is_key(),
    "the corrupt last-history packet must be a P-frame (a corrupt keyframe \
     could fail SW's send_packet instead of receive_frame)"
  );
  corrupt_packet_payload(&mut clip.packets[corrupt_idx]);

  // Probe-era, deliver NO frames (doom_from_send = 0): every accepted packet is
  // buffered as history; fail probe-era at `fail_at`. History replayed through
  // SW is {keyframe, clean P.., corrupt_P} — the sends accept it all, and the
  // final drain decodes corrupt_P and surfaces InvalidData.
  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));
  let mut dec = FfmpegVideoStreamDecoder::from_hw_inner_for_test(
    Box::new(FakeHw::failing(w, h, 0, fail_at)),
    clip.parameters.clone(),
    tb,
  )
  .expect("build test decoder");

  // Send exactly the history-then-failing packets; the failing send triggers
  // the fallback whose commit-time drain must surface the deferred error.
  let mut surfaced = None;
  let mut dst = crate::empty_owned_video_frame();
  for av_pkt in clip.packets.iter().take(fail_at + 1) {
    // SAFETY: the packet came from `avcodec_receive_packet` or `av_read_frame`, which wrote its
    // payload.
    let vpkt =
      unsafe { boundary::video_packet_from_ffmpeg(av_pkt, mediadecode::Timebase::SECONDS) }
        .expect("a wrappable payload")
        .expect("packet has a buffer");
    match dec.send_packet(&vpkt) {
      // A full decoder would need draining first; here nothing is
      // queued, so the two send answers are handled the same way.
      Ok(Sent::Accepted | Sent::MustDrain) => {
        // Drain anything available (none expected pre-fallback — doom = 0).
        loop {
          match dec.receive_frame(&mut dst) {
            Ok(Received::Produced) => {}
            Ok(Received::NeedsInput | Received::Ended) => break,
            Err(e) => {
              surfaced = Some(e);
              break;
            }
          }
        }
      }
      Err(e) => {
        surfaced = Some(e);
        break;
      }
    }
    if surfaced.is_some() {
      break;
    }
  }

  let err = surfaced.expect(
    "the deferred InvalidData must surface at the fallback commit boundary, not be \
     committed over",
  );
  match err {
    VideoDecodeError::Decode(Error::FallbackFailed(f)) => {
      assert!(
        !f.unconsumed_packets().is_empty(),
        "FallbackFailed must retain the rescued replay packets for recovery"
      );
      assert!(
        matches!(f.source(), Error::Ffmpeg(ffmpeg_next::Error::InvalidData)),
        "the surfaced error must be the deferred SW InvalidData; got {:?}",
        f.source()
      );
    }
    other => panic!("expected FallbackFailed surfacing the deferred InvalidData, got {other:?}"),
  }
  assert!(
    dec.is_hardware(),
    "a deferred-error fallback caught at the commit boundary must leave the \
     decoder on its prior HW state — nothing committed"
  );
}

// ---------------------------------------------------------------------------
//  Failed EOF fallback: eof_sent is RESTORED (no half-mutation), stays HW
// ---------------------------------------------------------------------------

/// `send_eof` meets the probe's exhaustion and the SW decoder cannot open
/// (empty `Parameters`). The fallback returns `FallbackFailed`, so the decoder
/// stays HW — and `eof_sent` must be RESTORED to its prior value (`false`),
/// never left half-mutated `true`. A stale `eof_sent = true` would make a
/// *later* fallback inject EOF into the new SW decoder though this `send_eof`
/// errored.
#[test]
fn failed_eof_fallback_restores_eof_sent_and_stays_on_hw() {
  let (w, h) = (64u32, 64u32);
  // `FakeHwEofFails::send_eof` raises the probe's `ProbeExhausted`, driving
  // the send_eof fallback arm; the empty `Parameters` from `unopenable_sw_decoder`
  // make `open_sw_decoder` fail, so the fallback returns `FallbackFailed` and the
  // transaction must roll back (HW retained, `eof_sent` un-mutated).
  let mut dec = unopenable_sw_decoder(Box::new(FakeHwEofFails::new(w, h)));
  assert!(dec.is_hardware(), "must start on the HW seam");
  assert!(
    !dec.eof_sent_for_test(),
    "precondition: eof_sent starts false"
  );

  let err = dec
    .send_eof()
    .expect_err("a failed EOF fallback must surface an error");
  match err {
    VideoDecodeError::Decode(Error::FallbackFailed(_)) => {}
    other => panic!("expected FallbackFailed on the failed EOF fallback, got {other:?}"),
  }

  assert!(
    dec.is_hardware(),
    "a failed EOF fallback (SW could not open) must leave the decoder on its \
     prior HW state — transactional rollback"
  );
  assert!(
    !dec.eof_sent_for_test(),
    "eof_sent must be RESTORED to its prior value (false) after a failed EOF \
     fallback — a stale true would inject EOF into a later SW fallback"
  );

  // A subsequent operation must not see stale EOF: a normal send_eof on the
  // (still-HW, EOF-never-accepted) decoder behaves as a first EOF. Our seam's
  // send_eof keeps failing the same way, so this just re-confirms HW + the
  // rolled-back flag rather than silently succeeding off a stale `eof_sent`.
  let err2 = dec.send_eof().expect_err(
    "the still-HW decoder must re-attempt (and re-fail) EOF, not no-op off stale state",
  );
  assert!(
    matches!(err2, VideoDecodeError::Decode(Error::FallbackFailed(_))),
    "second send_eof must again drive the fallback (proving no stale-EOF short-circuit)"
  );
  assert!(
    !dec.eof_sent_for_test(),
    "still rolled back after the retry"
  );
}

// ---------------------------------------------------------------------------
//  Placeholder seam smoke check
// ---------------------------------------------------------------------------

/// The inert seam builds a decoder on the HW path without driving anything —
/// guards `from_hw_inner_for_test` + the trimmed struct against regressions.
#[test]
fn inert_seam_builds_on_hardware() {
  ffmpeg_next::init().expect("ffmpeg init");
  let params = ffmpeg_next::codec::Parameters::new();
  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));
  let dec = FfmpegVideoStreamDecoder::from_hw_inner_for_test(Box::new(FakeHw::inert()), params, tb)
    .expect("build test decoder");
  assert!(dec.is_hardware(), "inert seam starts on the HW path");
  assert!(!dec.is_software());
}

// ---------------------------------------------------------------------------
//  Deferred real-fixture integration test
// ---------------------------------------------------------------------------

/// Real-media check of the FX3 road: an actual Sony FX3 H.264 **High
/// 4:2:2 10-bit** clip through the real VideoToolbox probe.
///
/// FFmpeg 9.0.1 offers VideoToolbox for every 10-bit H.264 stream that is
/// not RGB, 4:2:2 included (`libavcodec/h264_slice.c` 812–815), so the
/// probe opens on it. A VideoToolbox that cannot take the format fails its
/// session's creation (`ENOSYS`, `videotoolbox.c` 1023–1025), and
/// `ff_get_format` withdraws the hardware format and asks again without it
/// (`decode.c` 1341–1343 and 1348–1357), so the codec fails the picture in
/// its own words. No picture has come out yet, so that is the probe era:
/// `Auto` falls back to software with the probe's packets replayed, and
/// the whole stream decodes. Were this road ever to fail only after its
/// first picture, the session would stay on hardware and report that
/// picture's own error — and this check says so rather than passing.
///
/// An **instrumented experiment**: it captures the starting backend, where
/// the session moved to software, how many pictures had come out by then,
/// and every PTS delivered, all printed under `--nocapture`. The
/// assertions at the end encode the road observed on this fixture.
///
/// Gated on `MEDIADECODE_FX3_SAMPLE` (absolute path to the fixture); skips
/// cleanly when unset so `cargo test` stays green without it. Run with:
///
/// ```sh
/// MEDIADECODE_FX3_SAMPLE=/path/to/12_sony_fx3_xavc.mp4 \
///   cargo test -p mediadecode-ffmpeg --all-features \
///   fx3_high_422_10bit -- --ignored --nocapture
/// ```
#[test]
#[ignore = "requires a Sony FX3 H.264 High 4:2:2 10-bit fixture (user-provided); \
            set MEDIADECODE_FX3_SAMPLE to its path"]
fn fx3_high_422_10bit_falls_back_to_software_and_decodes_whole_stream() {
  use ffmpeg_next::{format, media};

  let Some(path) = std::env::var_os("MEDIADECODE_FX3_SAMPLE") else {
    eprintln!(
      "skipping: set MEDIADECODE_FX3_SAMPLE to the Sony FX3 H.264 422-10bit fixture path to run \
       this experiment"
    );
    return;
  };

  ffmpeg_next::init().expect("ffmpeg init");

  let mut input = format::input(&path).expect("open FX3 input");
  let stream = input
    .streams()
    .best(media::Type::Video)
    .expect("video stream");
  let stream_index = stream.index();
  // SAFETY: `stream.parameters()` exposes a live `*const AVCodecParameters`
  // for the duration of the borrow; reading the geometry fields is sound.
  let (expected_w, expected_h) = unsafe {
    let p = stream.parameters();
    ((*p.as_ptr()).width as u32, (*p.as_ptr()).height as u32)
  };
  // A nominal time base for frame labelling — the experiment only inspects the
  // coverage/ordering of the resulting PTS, not its real-time scale.
  let tb = Timebase::new(1, NonZeroI32::new(24).expect("nonzero"));

  let mut dec =
    FfmpegVideoStreamDecoder::open(stream.parameters(), tb, crate::DecoderLimits::default())
      .expect("open FX3 decoder");

  let mut obs = Fx3Observation::new(dec.is_hardware());
  eprintln!(
    "FX3 experiment: {expected_w}x{expected_h}; started_on_hw={} (is_software={})",
    obs.started_on_hw,
    dec.is_software()
  );

  let mut dst = crate::empty_owned_video_frame();

  'feed: for (s, packet) in input.packets() {
    if s.index() != stream_index {
      continue;
    }
    let is_key = packet.is_key();
    let pkt_pts = packet.pts();
    // SAFETY: the packet came from `avcodec_receive_packet` or `av_read_frame`, which wrote its
    // payload.
    let Some(vpkt) =
      unsafe { boundary::video_packet_from_ffmpeg(&packet, mediadecode::Timebase::SECONDS) }
        .expect("a wrappable payload")
    else {
      continue; // empty packet (no payload) — skip
    };

    // send_packet, draining on back pressure.
    let mut attempts = 0u32;
    loop {
      match dec.send_packet(&vpkt) {
        Ok(Sent::Accepted) => break,
        // Back pressure, named: drain, then offer the same packet again.
        Ok(Sent::MustDrain) => {
          if let Err(err) = obs.drain(&mut dec, &mut dst) {
            obs.abort = Some(format!("during send #{} EAGAIN-drain: {err}", obs.send_idx));
            break 'feed;
          }
          attempts += 1;
          assert!(
            attempts <= 64,
            "send_packet stuck on EAGAIN at send #{}",
            obs.send_idx
          );
        }
        Err(e) => {
          obs.abort = Some(format!(
            "send_packet #{} (key={is_key}, pts={pkt_pts:?}) errored: {e:?}",
            obs.send_idx
          ));
          break 'feed;
        }
      }
    }
    obs.note_transition(&dec);
    if let Err(err) = obs.drain(&mut dec, &mut dst) {
      obs.abort = Some(format!(
        "after send #{} (key={is_key}): {err}",
        obs.send_idx
      ));
      break 'feed;
    }
    obs.send_idx += 1;
  }

  // EOF + final drain (only if we did not already abort mid-feed).
  if obs.abort.is_none() {
    match dec.send_eof() {
      Ok(Sent::Accepted) => {
        if let Err(err) = obs.drain(&mut dec, &mut dst) {
          obs.abort = Some(format!("during post-EOF drain: {err}"));
        }
      }
      Ok(Sent::MustDrain) => {
        obs.abort = Some("send_eof asked for a drain after the feed loop drained".into());
      }
      Err(e) => obs.abort = Some(format!("send_eof errored: {e:?}")),
    }
  }

  // ----- Report -----------------------------------------------------------
  let ended_on_sw = dec.is_software();
  let unique: std::collections::HashSet<i64> = obs.pts_out.iter().copied().collect();
  eprintln!("FX3 experiment RESULT:");
  eprintln!("  started_on_hw        = {}", obs.started_on_hw);
  eprintln!(
    "  moved_to_sw          = {} (at send #{:?}, after {:?} pictures)",
    obs.moved_to_sw, obs.move_send_idx, obs.pictures_before_move
  );
  eprintln!("  ended_on_sw          = {ended_on_sw}");
  eprintln!(
    "  frames_delivered     = {} (unique pts = {})",
    obs.pts_out.len(),
    unique.len()
  );
  eprintln!("  delivered_pts        = {:?}", obs.pts_out);
  eprintln!("  abort                = {:?}", obs.abort);

  // ----- Assertions on the OBSERVED road ----------------------------------
  // (1) The probe opened on hardware — otherwise the fixture never reached
  //     VideoToolbox and the experiment is inconclusive.
  assert!(
    obs.started_on_hw,
    "expected the probe to open on VideoToolbox; if it opened straight to SW the hardware \
     road was never tried on this run"
  );

  // (2) The move to software happened in the probe era — before any picture
  //     came out of the hardware — which is the only time `Auto` moves.
  assert!(
    obs.moved_to_sw && ended_on_sw,
    "expected the probe to fall back to software on the FX3 clip; observed moved={}, \
     ended_on_sw={ended_on_sw}, abort={:?}",
    obs.moved_to_sw,
    obs.abort
  );
  assert_eq!(
    obs.pictures_before_move,
    Some(0),
    "the fallback must come before the first picture — a later failure would be that \
     picture's own error, not a fallback"
  );

  // (3) Nothing aborted the drive: no decode fault.
  assert!(
    obs.abort.is_none(),
    "the drive aborted before EOF on the real H.264 codec: {:?}",
    obs.abort
  );

  // (4) The software session decoded the stream: a non-trivial set of
  //     frames, every one with a real PTS, none twice.
  assert!(!obs.pts_out.is_empty(), "no frames were delivered at all");
  assert!(
    !obs.pts_out.contains(&i64::MIN),
    "every delivered frame must carry a real PTS: {:?}",
    obs.pts_out
  );
  assert_eq!(
    unique.len(),
    obs.pts_out.len(),
    "the replay must not re-emit a frame (no duplicate PTS): {:?}",
    obs.pts_out
  );
}

/// Instrumentation accumulator for the FX3 experiment: the observed backend
/// trajectory, the delivered PTS, and any terminal error. Bundled into one
/// value so the drive loop's drain step is a single method call.
struct Fx3Observation {
  /// Whether the decoder opened on the HW path.
  started_on_hw: bool,
  /// Set once the SW path is first observed active.
  moved_to_sw: bool,
  /// `send_packet` index at which the move to software was first observed.
  move_send_idx: Option<usize>,
  /// Pictures delivered before the move to software was observed.
  pictures_before_move: Option<usize>,
  /// 0-based index of the current `send_packet`, advanced by the drive loop.
  send_idx: usize,
  /// PTS of every delivered frame, in delivery order (`i64::MIN` marks a hole).
  pts_out: Vec<i64>,
  /// `Debug` of the terminal error if the drive aborted before EOF.
  abort: Option<String>,
}

impl Fx3Observation {
  fn new(started_on_hw: bool) -> Self {
    Self {
      started_on_hw,
      moved_to_sw: false,
      move_send_idx: None,
      pictures_before_move: None,
      send_idx: 0,
      pts_out: Vec::new(),
      abort: None,
    }
  }

  /// Note the move to software the first time it is observed, with how many
  /// pictures had come out before it.
  fn note_transition(&mut self, dec: &FfmpegVideoStreamDecoder) {
    if !self.moved_to_sw && dec.is_software() {
      self.moved_to_sw = true;
      self.move_send_idx = Some(self.send_idx);
      self.pictures_before_move = Some(self.pts_out.len());
      eprintln!(
        "  -> moved to software at/after send #{} ({} pictures delivered before it)",
        self.send_idx,
        self.pts_out.len()
      );
    }
  }

  /// Drain every ready frame, recording delivered PTS. Returns `Err(Debug)`
  /// on any fault, since that is the decisive observation.
  fn drain(
    &mut self,
    dec: &mut FfmpegVideoStreamDecoder,
    dst: &mut VideoFrame<mediadecode::PixelFormat, VideoFrameExtra, FfmpegBytes>,
  ) -> Result<(), String> {
    loop {
      match dec.receive_frame(dst) {
        Ok(Received::Produced) => {
          let pts = VideoFrame::pts(dst).map(|t| t.pts()).unwrap_or(i64::MIN);
          self.pts_out.push(pts);
        }
        Ok(Received::NeedsInput | Received::Ended) => break,
        Err(e) => return Err(format!("{e:?}")),
      }
    }
    Ok(())
  }
}

/// LAW: **a rescued packet never aliases a view carrier**, on `Auto`'s
/// road, where the probe's history comes back through a failed fallback.
///
/// PLANT: `av_packet_make_writable` skipped in `boundary::try_clone_packet`
/// turns this red at "addresses a retained view carrier's storage".
#[test]
fn a_rescued_packet_never_aliases_a_view_carrier() {
  use crate::{CarrierVideoStreamDecoder, View};
  use ffmpeg_next::packet::Ref;
  use mediadecode::decoder::VideoStreamDecoder;

  // **The scoped submission's proof leans on the recorder.** "Built,
  // lent, dropped inside this call" is true of the function — and would
  // be false of a probe that recorded by reference, because
  // `FallbackFailed::unconsumed_packets` hands its rescue history back
  // to the caller as owned, **mutable** `Packet`s. The view lane's
  // submission shares its carrier's buffer in the probe window too, so
  // a recording by reference would leave that call as a live mutable
  // alias of bytes a view carrier is still lending.
  //
  // So the probe records copies of its own, and that copy is the only
  // one: this law stands on it alone. That the submission does share is
  // `a_view_send_in_the_probe_window_is_zero_copy`'s — for the carriers a
  // demuxer delivers, which is why these are demuxed: a carrier converted
  // from a caller's packet is copied at the send, and would leave the
  // recorder nothing to alias.
  let (w, h) = (128u32, 96u32);
  let mut clip = encode_synthetic_clip(w, h, 12, 100);
  let p1 = clip
    .packets
    .iter()
    .position(|p| !p.is_key())
    .expect("clip has P-frames");
  assert!(
    p1 + 2 < clip.packets.len(),
    "need packets after the corrupt one"
  );
  corrupt_packet_payload(&mut clip.packets[p1]);

  let fail_at = p1 + 3;
  let (carriers, tb) = demuxed_view_packets(&clip);
  let mut dec = CarrierVideoStreamDecoder::<View>::from_hw_inner_for_test(
    Box::new(FakeHw::failing(w, h, 0, fail_at)),
    clip.parameters.clone(),
    tb,
  )
  .expect("build test decoder");

  // Retain every carrier — which is exactly what a consumer parking
  // view packets would do, and what makes an alias observable.
  let mut retained: Vec<crate::VideoPacket> = Vec::new();
  let mut dst = crate::boundary::empty_video_frame();
  let mut rescued: Vec<ffmpeg_next::Packet> = Vec::new();
  for vpkt in carriers {
    match dec.send_packet(&vpkt) {
      Ok(Sent::Accepted | Sent::MustDrain) => {}
      Err(VideoDecodeError::Decode(Error::FallbackFailed(f))) => {
        retained.push(vpkt);
        rescued = f.into_unconsumed_packets();
        break;
      }
      Err(e) => panic!("send_packet: {e:?}"),
    }
    retained.push(vpkt);
    // Exhaustive, not `.is_ok()`: "needs input" is a success now, so a
    // predicate loop here would never leave.
    while matches!(dec.receive_frame(&mut dst), Ok(Received::Produced)) {}
  }

  assert!(
    !rescued.is_empty(),
    "the failed fallback must surface a rescue history to check",
  );
  let carriers: Vec<usize> = retained
    .iter()
    .map(|p| p.data().as_ref().as_ptr() as usize)
    .collect();
  for packet in &rescued {
    // SAFETY: the packet is live; `data` is a public field.
    let address = unsafe { (*packet.as_ptr()).data as usize };
    assert!(
      !carriers.contains(&address),
      "a rescued packet addresses a retained view carrier's storage — \
       `data_mut` on it would be an aliasing write",
    );
  }

  // And the rescued packets really are writable, which is what makes
  // the aliasing question live rather than theoretical. Writing through
  // every one of them must leave every carrier's bytes alone.
  let before: Vec<Vec<u8>> = retained
    .iter()
    .map(|p| p.data().as_ref().to_vec())
    .collect();
  {
    for packet in &mut rescued {
      if let Some(slot) = packet.data_mut() {
        for byte in slot.iter_mut() {
          *byte ^= 0xFF;
        }
      }
    }
  }
  for (packet, expected) in retained.iter().zip(before) {
    assert_eq!(
      packet.data().as_ref(),
      expected.as_slice(),
      "writing a rescued packet reached a retained view carrier",
    );
  }
}

/// LAW: **a view send in the probe window is zero-copy, and the probe's
/// copy is the one copy.**
///
/// The probe window is the one stretch of a session with a recorder in
/// it: the probe keeps every packet it takes, and its exhaustion hands
/// them to the caller as owned, mutable `Packet`s. It records copies of
/// its own (`boundary::try_clone_packet`), so the view lane's submission
/// shares its carrier's buffer there as it does after the first picture.
/// On a pinned session, whose exhaustion reports the history as the seam
/// recorded it: each send hands the seam the retained carrier's own
/// storage, held by a reference to its buffer; no rescued packet is that
/// storage, each is referenced once and holds the bytes it was sent
/// with; and after its call each carrier is its buffer's only holder.
///
/// PLANT: the route forced to `BodyRoute::Copy` in `send_packet_impl`
/// turns this red at "the submission must be the carrier's own storage";
/// `av_packet_make_writable` skipped in `boundary::try_clone_packet` turns
/// it red at "shares a retained carrier's storage".
#[test]
fn a_view_send_in_the_probe_window_is_zero_copy() {
  use crate::{CarrierVideoStreamDecoder, View};
  use ffmpeg_next::{ffi::av_buffer_get_ref_count, packet::Ref};

  const PADDING: usize = ffmpeg_next::ffi::AV_INPUT_BUFFER_PADDING_SIZE as usize;

  let (w, h) = (64u32, 48u32);
  let clip = encode_synthetic_clip(w, h, 8, 100);
  let fail_at = 4;
  // Demuxed: the zero-copy send is for the packets libavformat delivers.
  let (carriers, tb) = demuxed_view_packets(&clip);
  // Takes every packet into its history and delivers nothing, then
  // exhausts on the send at `fail_at`.
  let seam = FakeHw::failing(w, h, 0, fail_at);
  let submitted = seam.submitted();
  let mut dec = CarrierVideoStreamDecoder::<View>::from_hw_inner_for_test_as(
    Box::new(seam),
    clip.parameters.clone(),
    tb,
    DecodePath::AnyHardware,
  )
  .expect("build a pinned test decoder");

  // Every carrier is retained, so its storage stays its own for the
  // whole lane: an address can match only by being that storage.
  let mut retained: Vec<crate::VideoPacket> = Vec::new();
  let mut rescued: Vec<ffmpeg_next::Packet> = Vec::new();
  for vpkt in carriers.into_iter().take(fail_at + 1) {
    // The premise: a payload libavformat delivered, with its padding
    // behind it, the one shape `boundary::share_or_copy` shares.
    let body = vpkt.data();
    assert_eq!(body.origin(), crate::view::Origin::PacketPayload);
    // SAFETY: the carrier holds this live, non-null reference; `size`
    // is a public field.
    let capacity = unsafe { (*body.as_av_buffer_ref()).size };
    assert!(
      capacity
        .checked_sub(body.offset() + body.len())
        .is_some_and(|slack| slack >= PADDING),
      "the premise: the padding behind the payload",
    );
    let answer = dec.send_packet(&vpkt);
    retained.push(vpkt);
    match answer {
      Ok(Sent::Accepted) => {}
      Err(VideoDecodeError::Decode(Error::ProbeExhausted(p))) => {
        rescued = p.into_unconsumed_packets();
        break;
      }
      other => panic!("send_packet: {other:?}"),
    }
  }
  assert_eq!(
    retained.len(),
    fail_at + 1,
    "the exhaustion arrives on its send"
  );
  assert_eq!(rescued.len(), fail_at, "the history comes back as recorded");

  let carriers: Vec<Storage> = retained
    .iter()
    .map(|p| Storage::of_carrier(p.data()))
    .collect();
  let sends = submitted.lock().expect("the seam is done").clone();
  assert_eq!(sends.len(), carriers.len(), "one record per send");
  for (send, (sent, carrier)) in sends.iter().zip(&carriers).enumerate() {
    assert_eq!(
      sent, carrier,
      "send {send}: the submission must be the carrier's own storage, held \
       by a reference to its buffer — a copy here is a second copy",
    );
  }
  for (index, packet) in rescued.iter().enumerate() {
    let kept = Storage::of_packet(packet);
    assert!(
      carriers
        .iter()
        .all(|carrier| carrier.data != kept.data && carrier.buffer != kept.buffer),
      "rescued packet {index} shares a retained carrier's storage — the \
       probe must record a copy",
    );
    // SAFETY: the packet is live and holds the buffer just read.
    let references = unsafe { av_buffer_get_ref_count((*packet.as_ptr()).buf) };
    assert_eq!(
      references, 1,
      "rescued packet {index}: a payload of its own"
    );
    assert_eq!(
      packet.data(),
      Some(retained[index].data().as_ref()),
      "rescued packet {index}: the bytes it was sent with",
    );
  }
  // And the shared reference died with its call: each carrier is its
  // buffer's only holder again.
  for (index, carrier) in retained.iter().enumerate() {
    // SAFETY: the carrier holds this live, non-null reference.
    let references = unsafe { av_buffer_get_ref_count(carrier.data().as_av_buffer_ref()) };
    assert_eq!(
      references, 1,
      "carrier {index}: nothing kept the submission"
    );
  }
}

#[test]
fn the_receive_time_fallback_queue_survives_a_failed_carrier() {
  use crate::{CarrierVideoStreamDecoder, View, boundary::video_packet_from_ffmpeg_in};
  use mediadecode::decoder::VideoStreamDecoder;

  // **The replay queue's second delivery path.** `fall_back_to_sw`
  // fills `sw_replay_frames` from inside `receive_frame` when the probe
  // is exhausted at *frame* time, and that branch converts the head
  // then and there. It used to `pop_front` first, so an allocation that
  // failed advanced past a frame the rescue history holds the only copy
  // of — the very loss the queue exists to prevent. It peeks now, like
  // the entry at the top of `receive_frame`.
  //
  // The ceiling is process-global, so this runs alone.
  mediaio_ffmpeg::test_support::in_subprocess(
    "video::tests::the_receive_time_fallback_queue_survives_a_failed_carrier",
    || {
      let (w, h) = (64u32, 48u32);
      let clip = encode_synthetic_clip(w, h, 8, 100);
      let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));

      // `cap_when_queued`: lower the ceiling on the first receive that
      // provably comes off the replay queue — checked with the queue's
      // own emptiness, so the lane cannot drift onto the scratch road
      // and quietly assert nothing.
      let drive = |cap_when_queued: bool| -> (Vec<Vec<u8>>, bool) {
        let mut dec = CarrierVideoStreamDecoder::<View>::from_hw_inner_for_test(
          Box::new(FakeHw::failing_at_receive(w, h)),
          clip.parameters.clone(),
          tb,
        )
        .expect("build test decoder");

        let mut frame = crate::boundary::empty_video_frame();
        let mut planes = Vec::new();
        let mut queue_was_hit = false;
        let mut armed = false;

        // **Send first, drain second.** The fake keeps every accepted
        // packet as rescue history and queues no frames of its own, so
        // the history is as deep as the clip by the time a frame is
        // asked for — which is what makes `fall_back_to_sw` replay more
        // than one packet and leave more than one frame on the queue.
        // Interleaving send and receive would fail the seam on the
        // first packet and give the queue a single frame, consumed in
        // the same call, leaving this lane nothing to cap.
        for av_pkt in &clip.packets {
          let Some(vpkt) =
            // SAFETY: the packet came from `avcodec_receive_packet` or `av_read_frame`, which
            // wrote its payload.
            unsafe { video_packet_from_ffmpeg_in(av_pkt.clone(), tb, crate::PacketLimits::default()) }
              .expect("a wrappable payload")
          else {
            continue;
          };
          if dec.send_packet(&vpkt).is_err() {
            break;
          }
        }

        loop {
          let capped = armed;
          if capped {
            armed = false;
            queue_was_hit = true;
            // SAFETY: in `in_subprocess`'s child, alone. The capped window runs one
            // receive off the replay queue: this crate's conversion, which checks
            // every allocation it makes.
            unsafe { mediaio_ffmpeg::test_support::cap_ffmpeg_allocations(16) };
          }
          let got = dec.receive_frame(&mut frame);
          if capped {
            mediaio_ffmpeg::test_support::uncap_ffmpeg_allocations();
          }
          match got {
            Ok(Received::Produced) => {
              planes.push(frame.planes()[0].data_ref().as_ref().to_vec());
              // The next receive will come off the queue, which is the
              // road this lane is for.
              if cap_when_queued && !queue_was_hit && !dec.sw_replay_frames_is_empty_for_test() {
                armed = true;
              }
            }
            // Nothing more from this packet — feed the next one.
            Ok(Received::NeedsInput | Received::Ended) => break,
            Err(VideoDecodeError::Convert(e)) => {
              assert!(capped, "no refusal was asked for here, got {e:?}");
              assert!(
                crate::decoder::parks_in_decode(&e),
                "the ceiling must produce a parkable refusal, got {e:?}",
              );
              // Retry immediately, uncapped: the same frame must come
              // back rather than the one after it.
              continue;
            }
            Err(_) => break,
          }
        }
        (planes, queue_was_hit)
      };

      let (reference, _) = drive(false);
      assert!(
        reference.len() >= 2,
        "the receive-time fallback must deliver replayed frames to test with",
      );
      // **Where the ceiling can go, and where it cannot.** The very
      // first delivery on this road happens in the same call that runs
      // `fall_back_to_sw` — which opens a software decoder and replays
      // packets through it — so a ceiling there refuses the fallback
      // rather than the carrier, and nothing outside can lower it
      // between the two. What *is* reachable is the queue that
      // fallback filled: a carrier failing on a later delivery must
      // leave its frame at the head.
      let (recovered, queue_was_hit) = drive(true);
      assert!(
        queue_was_hit,
        "the ceiling never reached the replay queue — this lane would \
         assert nothing",
      );
      assert_eq!(
        recovered, reference,
        "a transient refusal must cost no replayed frame at all",
      );
    },
  );
}

// ---------------------------------------------------------------------------
//  R2: the end of the stream outranks the parked seat, on both send gates
// ---------------------------------------------------------------------------

/// The cross product this lane needs: a session that has **accepted**
/// end-of-stream *and* has a frame parked in its seat.
///
/// Reaching it takes both halves at once — `send_eof` committed, then a
/// delayed tail frame drained out of the decoder whose carrier
/// allocation fails parkably. `hw` picks which scratch holds it.
fn eof_with_a_parked_frame(
  hw: bool,
) -> (
  crate::CarrierVideoStreamDecoder<crate::View>,
  SyntheticClip,
  Timebase,
) {
  use crate::{CarrierVideoStreamDecoder, View, boundary::video_packet_from_ffmpeg_in};
  use mediadecode::decoder::VideoStreamDecoder;

  let (w, h) = (64u32, 48u32);
  let clip = encode_synthetic_clip(w, h, 8, 100);
  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));
  // A seam that never fails keeps us on the hardware scratch; one that
  // raises probe-era exhaustion drops us onto the real software decoder
  // with the history replayed losslessly, so both scratches — the two
  // the parked-seat gate exists for — are proved.
  let seam: Box<dyn HwInner> = if hw {
    Box::new(FakeHw::never_failing(w, h))
  } else {
    Box::new(FakeHw::failing(w, h, 0, 2))
  };
  let mut dec =
    CarrierVideoStreamDecoder::<View>::from_hw_inner_for_test(seam, clip.parameters.clone(), tb)
      .expect("build test decoder");

  let packet = |index: usize| {
    // SAFETY: the packet came from `avcodec_receive_packet` or `av_read_frame`, which wrote its
    // payload.
    unsafe {
      video_packet_from_ffmpeg_in(
        clip.packets[index].clone(),
        tb,
        crate::PacketLimits::default(),
      )
    }
    .expect("a wrappable payload")
    .expect("packet has a buffer")
  };
  let mut frame = crate::boundary::empty_video_frame();

  // **The two roads need different feeds, and saying so is the point.**
  // The hardware seam hands back exactly the frames it was given, so it
  // must still be holding one at EOF. The software road arrives through
  // a probe-era fallback, whose replayed history lands in a *queue*
  // rather than the scratch — so that queue is drained to empty first,
  // and only then is a real `sw.receive_frame` frame available to park.
  if hw {
    for index in 0..4 {
      crate::accepted(dec.send_packet(&packet(index)), "send_packet");
    }
  } else {
    for index in 0..3 {
      crate::accepted(dec.send_packet(&packet(index)), "send_packet");
    }
    while matches!(dec.receive_frame(&mut frame), Ok(Received::Produced)) {}
    // The feeder loop this reform made writable: the software decoder
    // has real output now, so it exerts real back pressure, and the
    // answer to that is to drain and re-offer the same packet.
    for index in 3..clip.packets.len().min(6) {
      let pkt = packet(index);
      loop {
        match dec.send_packet(&pkt).expect("no fault while feeding") {
          Sent::Accepted => break,
          Sent::MustDrain => {
            while matches!(dec.receive_frame(&mut frame), Ok(Received::Produced)) {}
          }
        }
      }
    }
  }
  assert_eq!(dec.is_hardware(), hw, "the intended road");
  assert!(
    dec.sw_replay_frames_is_empty_for_test(),
    "the replay queue must be empty, or the park below lands in it \
     instead of the scratch seat this lane is about",
  );

  // The end, accepted — this is what sets `eof_sent`.
  crate::accepted(dec.send_eof(), "send_eof");
  assert!(
    dec.eof_sent_for_test(),
    "precondition: the end must be committed for this lane to mean anything",
  );

  // Now park a tail frame: the ceiling refuses the carrier, and the
  // refusal is one another attempt could survive, so the seat keeps it.
  // SAFETY: only reached inside `in_subprocess`'s child — both callers of this
  // helper run under it — alone. The capped window runs one receive:
  // libavcodec and this crate's conversion, which check what they
  // allocate.
  unsafe { mediaio_ffmpeg::test_support::cap_ffmpeg_allocations(16) };
  let refused = dec.receive_frame(&mut frame);
  mediaio_ffmpeg::test_support::uncap_ffmpeg_allocations();
  match refused {
    Err(VideoDecodeError::Convert(e)) => assert!(
      crate::decoder::parks_in_decode(&e),
      "the ceiling must produce a parkable refusal, got {e:?}",
    ),
    other => panic!("expected a parkable refusal after EOF, got {other:?}"),
  }

  (dec, clip, tb)
}

/// **Regression: `Sent::MustDrain` is a promise, and past end-of-stream
/// it is one this face cannot keep.**
///
/// The arm's whole contract is *drain the output and this same offer
/// becomes acceptable*. With `eof_sent` committed it never becomes
/// acceptable — draining empties the seat and the retry faults anyway,
/// until `flush`. So a caller that obeys the contract loops, drains,
/// re-offers, and is refused: the same fault-under-back-pressure
/// inversion the subtitle seam carried, one surface over.
///
/// Both send gates, and both **before and after** the drain that the
/// bad answer would have sent the caller to do.
fn a_post_eof_send_is_a_fault_not_backpressure(hw: bool) {
  use crate::boundary::video_packet_from_ffmpeg_in;
  use mediadecode::decoder::VideoStreamDecoder;

  let (mut dec, clip, tb) = eof_with_a_parked_frame(hw);
  let packet = |index: usize| {
    // SAFETY: the packet came from `avcodec_receive_packet` or `av_read_frame`, which wrote its
    // payload.
    unsafe {
      video_packet_from_ffmpeg_in(
        clip.packets[index].clone(),
        tb,
        crate::PacketLimits::default(),
      )
    }
    .expect("a wrappable payload")
    .expect("packet has a buffer")
  };

  let is_after_eof = |got: &Result<Sent, VideoDecodeError>| {
    matches!(
      got,
      Err(VideoDecodeError::Decode(Error::Ffmpeg(
        ffmpeg_next::Error::Eof
      )))
    )
  };

  // --- with the seat still parked -----------------------------------
  let sent = dec.send_packet(&packet(4));
  assert!(
    is_after_eof(&sent),
    "a packet after a committed EOF must be the fault even with the seat \
     parked — `MustDrain` here promises a retry that can never succeed; got {sent:?}",
  );
  let eof_again = dec.send_eof();
  assert!(
    is_after_eof(&eof_again),
    "the same for a repeated end-of-stream; got {eof_again:?}",
  );

  // --- the drain the bad answer would have prescribed ----------------
  // It succeeds (the parked frame is still deliverable), and it changes
  // nothing about the send side. That is the point: `MustDrain` would
  // have sent the caller here for nothing.
  let mut frame = crate::boundary::empty_video_frame();
  let mut drained = 0u32;
  for _ in 0..64 {
    match dec.receive_frame(&mut frame) {
      Ok(Received::Produced) => drained += 1,
      Ok(Received::NeedsInput | Received::Ended) => break,
      Err(e) => panic!("the parked frame must still be deliverable: {e:?}"),
    }
  }
  assert!(drained > 0, "the parked frame was never recovered");

  // --- with the seat free -------------------------------------------
  let sent_after = dec.send_packet(&packet(5));
  assert!(
    is_after_eof(&sent_after),
    "draining did not make the offer acceptable, which is exactly why the \
     parked answer must not have been `MustDrain`; got {sent_after:?}",
  );
  assert!(
    is_after_eof(&dec.send_eof()),
    "and the same for the repeated end-of-stream",
  );

  // `flush` is the only way back, and it really is one.
  dec.flush().expect("flush");
  assert!(
    !dec.eof_sent_for_test(),
    "flush must retract the committed end",
  );
  crate::accepted(dec.send_packet(&packet(0)), "flush reopened the send side");
}

/// The hardware scratch holds the parked frame.
#[test]
fn a_post_eof_send_is_a_fault_not_backpressure_on_the_hardware_road() {
  mediaio_ffmpeg::test_support::in_subprocess(
    "video::tests::a_post_eof_send_is_a_fault_not_backpressure_on_the_hardware_road",
    || a_post_eof_send_is_a_fault_not_backpressure(true),
  );
}

/// And the software scratch, after a probe-era fallback put us there —
/// the two scratches are the reason the parked-seat gate exists at all,
/// so the ordering is proved against both.
#[test]
fn a_post_eof_send_is_a_fault_not_backpressure_on_the_software_road() {
  mediaio_ffmpeg::test_support::in_subprocess(
    "video::tests::a_post_eof_send_is_a_fault_not_backpressure_on_the_software_road",
    || a_post_eof_send_is_a_fault_not_backpressure(false),
  );
}

/// **Regression: a protocol state with no satisfying operation must not
/// reach the caller.**
///
/// The road: the hardware accepts end-of-stream, so `eof_sent` commits;
/// then the probe exhausts *while draining*, before any picture, and the
/// frame-time fallback opens software and replays the probe's history. If
/// the committed end does not travel with that fallback, the software
/// decoder answers `EAGAIN` once the history is drained —
/// [`Received::NeedsInput`], an instruction to send another packet — on a
/// session where both send gates now refuse. The caller can only spin or
/// quietly keep a truncated tail.
///
/// This is an **interlock**, not a plain bug: the gates are correct and
/// the fallback was correct before them; together they closed every
/// exit. Before the gates existed, a repeated `send_eof` would have
/// re-armed the software decoder by accident, which is the sort of luck a
/// protocol should not depend on.
///
/// What must be true afterwards is stated as the property rather than
/// the mechanism: **whatever the decoder answers, it is never
/// `NeedsInput`,** every replayed picture comes out, and the drain
/// terminates.
#[test]
fn a_post_eof_frame_time_fallback_never_strands_the_caller_in_needs_input() {
  use crate::{CarrierVideoStreamDecoder, View, boundary::video_packet_from_ffmpeg_in};
  use mediadecode::decoder::VideoStreamDecoder;

  let (w, h) = (64u32, 48u32);
  let clip = encode_synthetic_clip(w, h, 8, 100);
  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));
  let mut dec = CarrierVideoStreamDecoder::<View>::from_hw_inner_for_test(
    // Takes every packet into its history and delivers nothing; the first
    // picture asked for exhausts the probe.
    Box::new(FakeHw::failing_at_receive(w, h)),
    clip.parameters.clone(),
    tb,
  )
  .expect("build test decoder");

  for av_pkt in &clip.packets {
    // SAFETY: the packet came from `avcodec_receive_packet` or `av_read_frame`, which wrote its
    // payload.
    let pkt =
      unsafe { video_packet_from_ffmpeg_in(av_pkt.clone(), tb, crate::PacketLimits::default()) }
        .expect("a wrappable payload")
        .expect("packet has a buffer");
    crate::accepted(dec.send_packet(&pkt), "send_packet");
  }

  // The end, accepted on the hardware seam — `eof_sent` commits here.
  crate::accepted(dec.send_eof(), "send_eof");
  assert!(
    dec.eof_sent_for_test(),
    "precondition: the end must be committed before the fallback fires",
  );

  // Drain. The first poll exhausts the probe and takes the frame-time
  // fallback road.
  let mut frame = crate::boundary::empty_video_frame();
  let mut pictures = 0usize;
  let mut terminal = false;
  for _ in 0..64 {
    match dec.receive_frame(&mut frame) {
      Ok(Received::Produced) => pictures += 1,
      Ok(Received::NeedsInput) => panic!(
        "stranded: the decoder asked for input on a session whose end is \
         committed, and both send gates refuse — no legal operation can \
         satisfy this answer",
      ),
      Ok(Received::Ended) => {
        terminal = true;
        break;
      }
      Err(e) => panic!("unexpected fault while draining: {e:?}"),
    }
  }
  assert!(terminal, "the drain never reached a terminal answer");
  assert!(dec.is_software(), "the frame-time fallback did commit");
  assert_eq!(
    pictures,
    clip.packets.len(),
    "the replayed history comes out whole — the fallback is lossless",
  );

  // **Isolating the forwarding from whatever else might have done the
  // work.** A decoder that was handed the end answers `AVERROR_EOF`; one
  // that was not answers `EAGAIN`, which reaches a caller as `NeedsInput`.
  let DecodeState::Sw(sw) = &mut dec.state else {
    panic!("the software decoder must be the one in place");
  };
  let mut scratch = alloc_av_video_frame().expect("frame slot");
  let raw = sw
    .receive_frame(&mut scratch)
    .expect_err("a drained decoder produces no further frame");
  assert!(
    matches!(raw, ffmpeg_next::Error::Eof),
    "the software decoder never received the committed end — it answered \
     {raw:?}, which reaches a caller as `NeedsInput` and cannot be satisfied",
  );

  // And the session stays terminal: polling past the end keeps
  // answering the end, never sending the caller back for input.
  for _ in 0..3 {
    assert_eq!(
      dec
        .receive_frame(&mut frame)
        .expect("no fault past the end"),
      Received::Ended,
    );
  }
}

/// **The synthesized fault, checked against the substrate — reaching
/// both sides this time.**
///
/// The previous version of this lane was a tautology and passed for the
/// wrong reason: it called `send_eof` on the *wrapper*, which commits
/// `eof_sent`, so the later `send_packet` returned through the wrapper's
/// own gate. It compared [`CarrierVideoStreamDecoder::after_eof`] with
/// itself and would have passed had libavcodec diverged completely.
///
/// The lesson generalises past this one test: **revert-verification
/// catches a deleted gate, not a comparison that never crossed the
/// seam.** A parity pin has to reach both sides it claims to compare,
/// and be written so that it fails if either moves.
///
/// So this one goes around the gate: it reaches the raw inner software
/// decoder — the actual `ffmpeg::decoder::Video` — feeds it the flush
/// packet directly, and reads what libavcodec really answers to a
/// submission after end-of-stream.
#[test]
fn the_post_eof_fault_is_the_one_the_substrate_gives() {
  use crate::{CarrierVideoStreamDecoder, View, boundary::video_packet_from_ffmpeg_in};
  use mediadecode::decoder::VideoStreamDecoder;

  let (w, h) = (64u32, 48u32);
  let clip = encode_synthetic_clip(w, h, 8, 100);
  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));
  // Probe-era exhaustion puts the real software decoder in the seat —
  // the fake seam has no EOF state machine to interrogate.
  let mut dec = CarrierVideoStreamDecoder::<View>::from_hw_inner_for_test(
    Box::new(FakeHw::failing(w, h, 0, 2)),
    clip.parameters.clone(),
    tb,
  )
  .expect("build test decoder");

  for item in clip.packets.iter().take(3) {
    // SAFETY: the packet came from `avcodec_receive_packet` or `av_read_frame`, which wrote its
    // payload.
    let pkt =
      unsafe { video_packet_from_ffmpeg_in(item.clone(), tb, crate::PacketLimits::default()) }
        .expect("a wrappable payload")
        .expect("packet has a buffer");
    crate::accepted(dec.send_packet(&pkt), "send_packet");
  }

  assert!(
    dec.is_software(),
    "the substrate under test is libavcodec's"
  );

  // **Past the wrapper entirely.** `tests` is a child module, so the
  // private state is reachable; the point is that nothing below asks
  // the wrapper anything.
  let DecodeState::Sw(sw) = &mut dec.state else {
    panic!("the software decoder must be the one in the seat");
  };
  sw.send_eof().expect("the substrate takes the end");

  // What libavcodec actually answers a packet after the flush packet.
  let substrate = sw
    .send_packet(&clip.packets[3])
    .expect_err("libavcodec must refuse a packet after end-of-stream");
  assert!(
    matches!(substrate, ffmpeg_next::Error::Eof),
    "the substrate's post-EOF refusal moved: got {substrate:?}",
  );

  // Both wrapper roads wrap that value identically — the software road
  // through `software_exit` (which passes it through when no refusal
  // was recorded) and the hardware road through its own
  // `Err(e @ Eof) => Err(Error::Ffmpeg(e))` arm. Pinning the funnel's
  // output makes the hardware claim a checked identity rather than an
  // assertion: it is the same `Error::Ffmpeg` construction, on a value
  // the line above proved is what the substrate gives.
  // SAFETY: a null state is allowed, and nothing is read through it.
  let wrapped = unsafe { crate::decoder::software_exit(core::ptr::null(), substrate) };
  assert!(
    matches!(wrapped, Error::Ffmpeg(ffmpeg_next::Error::Eof)),
    "the funnel changed how a post-EOF refusal is wrapped: got {wrapped:?}",
  );

  // And that is exactly what the gates hand back without asking.
  let synthesized = CarrierVideoStreamDecoder::<View>::after_eof();
  assert!(
    matches!(
      synthesized,
      VideoDecodeError::Decode(Error::Ffmpeg(ffmpeg_next::Error::Eof))
    ),
    "the synthesized post-EOF fault drifted from the substrate's: got {synthesized:?}",
  );
}

#[test]
fn a_parked_hardware_frame_is_delivered_before_any_fallback() {
  use crate::{CarrierVideoStreamDecoder, View, boundary::video_packet_from_ffmpeg_in};
  use mediadecode::decoder::VideoStreamDecoder;

  // **A parked frame pins the state that parked it.** The scratch a
  // retry reads is chosen by the *current* `DecodeState`, so a
  // hardware-to-software fallback committed while a hardware frame was
  // parked would send the retry to the software scratch — delivering a
  // stale frame, or refusing permanently and stranding a decoded one.
  // Both send roads can commit that fallback, so both refuse while the
  // seat is taken.
  mediaio_ffmpeg::test_support::in_subprocess(
    "video::tests::a_parked_hardware_frame_is_delivered_before_any_fallback",
    || {
      let (w, h) = (64u32, 48u32);
      let clip = encode_synthetic_clip(w, h, 8, 100);
      let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));
      let mut dec = CarrierVideoStreamDecoder::<View>::from_hw_inner_for_test(
        // The second send is the one that would commit the probe's
        // fallback — which is exactly the transition that must not
        // happen underneath a parked frame.
        Box::new(FakeHw::failing(w, h, usize::MAX, 1)),
        clip.parameters.clone(),
        tb,
      )
      .expect("build test decoder");

      let packet = |index: usize| {
        // SAFETY: the packet came from `avcodec_receive_packet` or `av_read_frame`, which wrote
        // its payload.
        unsafe {
          video_packet_from_ffmpeg_in(
            clip.packets[index].clone(),
            tb,
            crate::PacketLimits::default(),
          )
        }
        .expect("a wrappable payload")
        .expect("packet has a buffer")
      };

      crate::accepted(dec.send_packet(&packet(0)), "send_packet");
      assert!(dec.is_hardware(), "the seam under test is the hardware one");

      // Park the hardware frame.
      let mut frame = crate::boundary::empty_video_frame();
      // SAFETY: in `in_subprocess`'s child, alone. The capped window runs one
      // receive: libavcodec and this crate's conversion, which check what
      // they allocate.
      unsafe { mediaio_ffmpeg::test_support::cap_ffmpeg_allocations(16) };
      let refused = dec.receive_frame(&mut frame);
      mediaio_ffmpeg::test_support::uncap_ffmpeg_allocations();
      match refused {
        Err(VideoDecodeError::Convert(e)) => assert!(
          crate::decoder::parks_in_decode(&e),
          "the ceiling must produce a parkable refusal, got {e:?}",
        ),
        other => panic!("expected a parkable refusal, got {other:?}"),
      }

      // **Nothing may be sent while it is parked** — this is the send
      // that could otherwise have committed a fallback underneath it.
      // The discipline is unchanged; it is spelled as back pressure
      // now, which is what it always was: nothing was consumed, and
      // the escape has always been `receive_frame` or `flush`.
      assert!(
        matches!(dec.send_packet(&packet(1)), Ok(Sent::MustDrain)),
        "a send under a parked frame must be told to drain first",
      );
      assert!(
        matches!(dec.send_eof(), Ok(Sent::MustDrain)),
        "EOF under a parked frame must be told to drain first too",
      );

      // The parked frame is still the hardware one, and it arrives.
      assert_eq!(
        dec.receive_frame(&mut frame).expect("the parked frame"),
        Received::Produced,
      );
      assert!(
        dec.is_hardware(),
        "no fallback can have happened while the frame was parked",
      );
      assert!(
        !frame.planes()[0].data_ref().as_ref().is_empty(),
        "the delivered frame must carry the decoded planes",
      );

      // And once the parked frame is gone the send reaches the seam —
      // this is the one that would have committed the fallback
      // underneath it. What the software decoder then makes of the
      // packet is not this lane's business; that it is no longer
      // *refused* is.
      let after = dec.send_packet(&packet(1));
      assert!(
        !matches!(after, Ok(Sent::MustDrain)),
        "with the seat free the send must reach the seam, got {after:?}",
      );
    },
  );
}

// ---------------------------------------------------------------------------
//  The decode-path pin (issue #50)
// ---------------------------------------------------------------------------

/// **The software door, open at last.**
///
/// Before `open_as` the only way to reach the software decoder as a
/// session was to pick a codec no hardware backend carries — which
/// changes the stream, and therefore compares nothing. This opens it on
/// the stream the caller actually has, and the session decodes.
#[test]
fn the_software_path_opens_without_probing_anything() {
  let (w, h) = (64u32, 48u32);
  let clip = encode_synthetic_clip(w, h, 12, 6);
  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));

  let mut dec = FfmpegVideoStreamDecoder::open_as(
    clip.parameters.clone(),
    tb,
    DecoderLimits::default(),
    DecodePath::Software,
  )
  .expect("the software path opens for a stream libavcodec can decode");

  assert!(dec.is_software(), "the pin put the session on software");
  assert!(!dec.is_hardware());
  assert!(
    dec.hardware_inner().is_none(),
    "there is no hardware decoder in this session to borrow",
  );

  // And it really decodes — a door that opened onto nothing would pass
  // every assertion above.
  let mut dst = crate::empty_owned_video_frame();
  let mut delivered = 0usize;
  for av_pkt in &clip.packets {
    // SAFETY: the packet came from `avcodec_receive_packet` or `av_read_frame`, which wrote its
    // payload.
    let vpkt =
      unsafe { boundary::video_packet_from_ffmpeg(av_pkt, mediadecode::Timebase::SECONDS) }
        .expect("a wrappable payload")
        .expect("packet has a buffer");
    crate::accepted(dec.send_packet(&vpkt), "send_packet");
    while let Ok(Received::Produced) = dec.receive_frame(&mut dst) {
      delivered += 1;
    }
  }
  crate::accepted(dec.send_eof(), "send_eof");
  while let Ok(Received::Produced) = dec.receive_frame(&mut dst) {
    delivered += 1;
  }
  assert_eq!(
    delivered,
    clip.packets.len(),
    "the pinned software session decoded every packet of the clip",
  );
}

/// **The session's open request starts at `open`'s and sets each
/// optional**, consuming and in place alike.
#[test]
fn the_sessions_open_request_starts_at_opens_and_sets_each_optional() {
  let request = VideoStreamDecoderOpen::new();
  assert_eq!(request, VideoStreamDecoderOpen::default());
  assert_eq!(
    (request.path(), request.options()),
    (DecodePath::Auto, crate::DecoderOptions::new())
  );
  let options = crate::DecoderOptions::new().with_skip_frame(crate::Discard::NonReference);
  let built = request
    .with_path(DecodePath::Software)
    .with_options(options);
  assert_eq!(
    (built.path(), built.options()),
    (DecodePath::Software, options)
  );
  let mut set = VideoStreamDecoderOpen::new();
  set.set_path(DecodePath::Software).set_options(options);
  assert_eq!(set, built);
}

/// The three skips the session's active decoder context holds, raw: the
/// hardware decoder's, or the software one's.
fn session_skips(dec: &FfmpegVideoStreamDecoder) -> (i32, i32, i32) {
  match &dec.state {
    DecodeState::Hw(hw) => hw
      .as_video_decoder()
      .expect("the real hardware decoder, not a fake")
      .skips_for_test(),
    // SAFETY: `sw.decoder` owns a live, opened context for this borrow.
    DecodeState::Sw(sw) => unsafe { crate::decoder::skips_of(sw.decoder.as_ptr()) },
  }
}

/// **The session's open request reaches every context the session
/// opens**, on every road, and is reported by `options()`:
///
/// - the software path;
/// - `Auto` with no backend to probe, which opens software at once;
/// - the probe-era fallback, after a hardware probe exhausts before its
///   first picture;
/// - on Apple targets, `Auto`, `AnyHardware` and the pin over
///   VideoToolbox, which opens a context without asking for hardware.
///
/// `open` and `open_as` name no options and leave libavcodec's defaults.
#[test]
fn the_sessions_request_reaches_every_context_it_opens() {
  let (w, h) = (64u32, 48u32);
  let clip = encode_synthetic_clip(w, h, 8, 4);
  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));
  // A different value in each skip, so a setting written into another's
  // field is seen.
  let options = crate::DecoderOptions::new()
    .with_skip_frame(crate::Discard::NonReference)
    .with_skip_loop_filter(crate::Discard::All)
    .with_skip_idct(crate::Discard::NonKey);
  let named = ((8, 48, 32), options);
  let request = |path| {
    VideoStreamDecoderOpen::new()
      .with_path(path)
      .with_options(options)
  };
  let read = |dec: &FfmpegVideoStreamDecoder| (session_skips(dec), dec.options());

  let software = FfmpegVideoStreamDecoder::open_from(
    request(DecodePath::Software),
    clip.parameters.clone(),
    tb,
    DecoderLimits::default(),
  )
  .expect("the software path opens");
  assert!(software.is_software());
  assert_eq!(read(&software), named, "the software path");

  let at_once = FfmpegVideoStreamDecoder::open_from_in(
    request(DecodePath::Auto),
    clip.parameters.clone(),
    tb,
    DecoderLimits::default(),
    &[],
  )
  .expect("Auto opens software where no hardware backend exists");
  assert!(at_once.is_software());
  assert_eq!(read(&at_once), named, "Auto with no backend");

  // A probe that takes the first packet into its history and exhausts on
  // the second: the session falls back, replaying the first.
  let mut fallen = FfmpegVideoStreamDecoder::from_hw_inner_for_test_from(
    Box::new(FakeHw::failing(w, h, 0, 1)),
    request(DecodePath::Auto),
    clip.parameters.clone(),
    tb,
  )
  .expect("build a test decoder");
  crate::accepted(fallen.send_packet(&pushed(&clip, 0)), "send_packet");
  assert!(fallen.is_hardware(), "still on the probe");
  crate::accepted(fallen.send_packet(&pushed(&clip, 1)), "send_packet");
  assert!(fallen.is_software(), "the probe's exhaustion fell back");
  assert_eq!(read(&fallen), named, "the probe-era fallback");

  #[cfg(target_vendor = "apple")]
  for path in [
    DecodePath::Auto,
    DecodePath::AnyHardware,
    DecodePath::Hardware(Backend::VideoToolbox),
  ] {
    let hardware = FfmpegVideoStreamDecoder::open_from(
      request(path),
      clip.parameters.clone(),
      tb,
      DecoderLimits::default(),
    )
    .expect("VideoToolbox opens a context");
    assert!(hardware.is_hardware(), "{path:?}");
    assert_eq!(read(&hardware), named, "{path:?}");
  }

  let defaults = ((0, 0, 0), crate::DecoderOptions::new());
  let bare = FfmpegVideoStreamDecoder::open(clip.parameters.clone(), tb, DecoderLimits::default())
    .expect("the bare constructor opens");
  assert_eq!(read(&bare), defaults, "open");
  let pinned_software = FfmpegVideoStreamDecoder::open_as(
    clip.parameters.clone(),
    tb,
    DecoderLimits::default(),
    DecodePath::Software,
  )
  .expect("the software path opens");
  assert_eq!(read(&pinned_software), defaults, "open_as");
}

/// `Auto` is the old constructor, unmoved — the promise `open_as` is
/// built on. Both sessions are asked the same questions and answer the
/// same way, on whatever path this machine's probe lands them.
#[test]
fn the_auto_path_is_the_bare_constructor() {
  let (w, h) = (64u32, 48u32);
  let clip = encode_synthetic_clip(w, h, 8, 4);
  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));

  let bare = FfmpegVideoStreamDecoder::open(clip.parameters.clone(), tb, DecoderLimits::default())
    .expect(
      "the bare constructor opens: it falls back to software when no backend takes the stream",
    );
  let named = FfmpegVideoStreamDecoder::open_as(
    clip.parameters.clone(),
    tb,
    DecoderLimits::default(),
    DecodePath::Auto,
  )
  .expect("the Auto arm opens wherever the bare constructor does");

  assert_eq!(bare.is_software(), named.is_software());
  assert_eq!(bare.is_hardware(), named.is_hardware());
}

/// A backend this platform does not probe, **named anyway**: the pin
/// fails the open rather than quietly handing back a software session.
///
/// That is the whole difference between a preference and a pin at open
/// time, and it is the failure a caller most needs to see — a
/// determinism run that silently got software twice would report that
/// the two paths agree.
#[test]
fn a_hardware_pin_on_an_absent_backend_fails_instead_of_falling_back() {
  let (w, h) = (64u32, 48u32);
  let clip = encode_synthetic_clip(w, h, 8, 4);
  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));

  // Whatever this platform's probe order is, at least one of the four
  // backends is not in it — a single-backend platform leaves three, and
  // a platform with no order at all leaves four.
  let order = crate::backend::probe_order();
  let absent = [
    Backend::VideoToolbox,
    Backend::Vaapi,
    Backend::Cuda,
    Backend::D3d11va,
  ]
  .into_iter()
  .find(|backend| !order.contains(backend))
  .expect("no platform probes all four backends");

  let refused = FfmpegVideoStreamDecoder::open_as(
    clip.parameters.clone(),
    tb,
    DecoderLimits::default(),
    DecodePath::Hardware(absent),
  );

  assert!(
    refused.is_err(),
    "a pin to {absent:?} must fail rather than open the software decoder the Auto arm \
     would have reached for",
  );
}

/// **The pin survives its probe's exhaustion**, on the send road.
///
/// A hardware backend that opens and then cannot take the stream before
/// its first picture raises exactly the exhaustion `Auto` reads as its cue
/// to open software. Under a pin that cue is reported instead: the session
/// stays on hardware, the caller keeps every packet the probe consumed,
/// and it learns the backend failed rather than silently receiving
/// software pixels for the rest of the stream. After the first picture
/// there is no exhaustion to report on any path — see
/// `after_the_first_picture_nothing_changes_the_road_on_any_path`.
#[test]
fn a_hardware_pin_reports_its_probes_exhaustion_instead_of_falling_back() {
  let (w, h) = (64u32, 48u32);
  let clip = encode_synthetic_clip(w, h, 12, 6);
  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));
  let fail_at = 2;

  let mut dec = FfmpegVideoStreamDecoder::from_hw_inner_for_test_as(
    // Takes every packet into its history and delivers nothing, then
    // exhausts on the send at `fail_at`.
    Box::new(FakeHw::failing(w, h, 0, fail_at)),
    clip.parameters.clone(),
    tb,
    DecodePath::Hardware(Backend::VideoToolbox),
  )
  .expect("build a pinned test decoder");

  for index in 0..fail_at {
    crate::accepted(dec.send_packet(&pushed(&clip, index)), "send_packet");
  }
  let refusal = dec
    .send_packet(&pushed(&clip, fail_at))
    .expect_err("the probe exhausts on this packet, so a refusal must arrive");
  let VideoDecodeError::Decode(Error::ProbeExhausted(p)) = &refusal else {
    panic!(
      "the pinned session must report the exhaustion with its payload intact, got {refusal:?}"
    );
  };
  assert!(p.origin().is_probe(), "the probe's own exhaustion");
  assert_eq!(
    p.unconsumed_packets().len(),
    fail_at,
    "the packets the probe consumed ride back with the refusal",
  );
  assert!(
    dec.is_hardware(),
    "the pin holds after the refusal: nothing opened a software decoder behind it",
  );
  assert!(!dec.is_software());
}

/// The same promise on the **receive** road, where the exhaustion
/// arrives with no packet in hand.
#[test]
fn a_hardware_pin_reports_a_frame_time_exhaustion_too() {
  let (w, h) = (64u32, 48u32);
  let clip = encode_synthetic_clip(w, h, 8, 4);
  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));

  let mut dec = FfmpegVideoStreamDecoder::from_hw_inner_for_test_as(
    Box::new(FakeHw::failing_at_receive(w, h)),
    clip.parameters.clone(),
    tb,
    DecodePath::Hardware(Backend::VideoToolbox),
  )
  .expect("build a pinned test decoder");

  // SAFETY: the packet came from `avcodec_receive_packet` or `av_read_frame`, which wrote its
  // payload.
  let vpkt =
    unsafe { boundary::video_packet_from_ffmpeg(&clip.packets[0], mediadecode::Timebase::SECONDS) }
      .expect("a wrappable payload")
      .expect("packet has a buffer");
  crate::accepted(dec.send_packet(&vpkt), "send_packet");

  let mut dst = crate::empty_owned_video_frame();
  let refusal = dec
    .receive_frame(&mut dst)
    .expect_err("the seam fails the first time a frame is asked for");
  assert!(
    matches!(&refusal, VideoDecodeError::Decode(Error::ProbeExhausted(_)),),
    "got {refusal:?}",
  );
  assert!(dec.is_hardware(), "the pin holds on the receive road too");
}

/// And on the **EOF** road, the third and last place a hardware
/// exhaustion can reach the wrapper.
///
/// Covering all three is the point: the pin is one promise, and a road
/// that forgot to ask would break it in a way no caller could see.
#[test]
fn a_hardware_pin_reports_an_exhaustion_raised_at_eof() {
  let (w, h) = (64u32, 48u32);
  let clip = encode_synthetic_clip(w, h, 8, 4);
  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));

  let mut dec = FfmpegVideoStreamDecoder::from_hw_inner_for_test_as(
    Box::new(FakeHwEofFails::new(w, h)),
    clip.parameters.clone(),
    tb,
    DecodePath::Hardware(Backend::VideoToolbox),
  )
  .expect("build a pinned test decoder");

  // SAFETY: the packet came from `avcodec_receive_packet` or `av_read_frame`, which wrote its
  // payload.
  let vpkt =
    unsafe { boundary::video_packet_from_ffmpeg(&clip.packets[0], mediadecode::Timebase::SECONDS) }
      .expect("a wrappable payload")
      .expect("packet has a buffer");
  crate::accepted(dec.send_packet(&vpkt), "send_packet");

  let refusal = dec
    .send_eof()
    .expect_err("this seam raises its exhaustion from send_eof");
  assert!(
    matches!(&refusal, VideoDecodeError::Decode(Error::ProbeExhausted(_)),),
    "got {refusal:?}",
  );
  assert!(dec.is_hardware(), "the pin holds on the EOF road too");
}

/// **`Auto` still falls back during its probe**, checked beside the pin
/// rather than assumed: the gate added for the pin must not have
/// quietened the arm it was written around.
///
/// The same seam, the same failure packet and the same send road as the
/// pinned lane above — only the [`DecodePath`] differs, which is what
/// makes this a control rather than a second scenario.
#[test]
fn the_auto_path_still_falls_back_where_a_pin_would_not() {
  let (w, h) = (64u32, 48u32);
  let clip = encode_synthetic_clip(w, h, 12, 6);
  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));
  let fail_at = 2;

  let mut dec = FfmpegVideoStreamDecoder::from_hw_inner_for_test_as(
    Box::new(FakeHw::failing(w, h, 0, fail_at)),
    clip.parameters.clone(),
    tb,
    DecodePath::Auto,
  )
  .expect("build an auto test decoder");

  let mut pictures = Vec::new();
  for index in 0..=fail_at {
    crate::accepted(dec.send_packet(&pushed(&clip, index)), "send_packet");
    drain_pictures(&mut dec, &mut pictures);
  }

  assert!(
    dec.is_software(),
    "the same seam, the same failure, and the Auto arm falls back — which is what makes the \
     pin's refusal a choice rather than a breakage",
  );
  assert_eq!(
    pictures,
    (0..=fail_at as i64).collect::<Vec<_>>(),
    "the probe's packets replayed, then the refused one — nothing lost",
  );
}

// ---------------------------------------------------------------------------
//  The committed road: every failure is its picture's own
// ---------------------------------------------------------------------------

/// A committed hardware seam with FFmpeg's own software decoder standing
/// in for the hardware: every picture comes out of libavcodec, and where
/// the script says so the hardware answers a raw errno instead — on a
/// send (the packet's picture is lost), on a picture asked for (that
/// picture is lost), or at the end of the stream, the first time it is
/// offered.
///
/// **Each failure reaches the wrapper the way `VideoDecoder`'s committed
/// arms hand one over:** the verdict as it was minted — with nothing
/// latched, `Error::Ffmpeg` and the errno — and nothing remembered, so the
/// next call reaches the decoder behind the seam. What the wrapper does
/// with it is what it does in production; `VideoDecoder`'s own half is
/// pinned in `decoder/tests.rs`.
struct ScriptedHw {
  sw: super::SwDecoder,
  /// `(send index, errno)`: that send is answered with the errno.
  failing_sends: Vec<(usize, ffmpeg_next::Error)>,
  /// `(pts, errno)`: the picture with that PTS, asked for, is answered
  /// with the errno instead.
  failing_pictures: Vec<(i64, ffmpeg_next::Error)>,
  /// What the end of the stream is answered with, the first time it is
  /// offered.
  failing_eof: Option<ffmpeg_next::Error>,
  sent: usize,
}

impl ScriptedHw {
  fn new(parameters: &Parameters) -> Self {
    Self {
      sw: super::open_sw_decoder(
        parameters,
        DecoderLimits::default(),
        crate::DecoderOptions::new(),
        None,
      )
      .expect("FFmpeg's own decoder for the stream"),
      failing_sends: Vec::new(),
      failing_pictures: Vec::new(),
      failing_eof: None,
      sent: 0,
    }
  }

  fn failing_send(mut self, at: usize, raw: ffmpeg_next::Error) -> Self {
    self.failing_sends.push((at, raw));
    self
  }

  fn failing_picture(mut self, pts: i64, raw: ffmpeg_next::Error) -> Self {
    self.failing_pictures.push((pts, raw));
    self
  }

  fn failing_eof(mut self, raw: ffmpeg_next::Error) -> Self {
    self.failing_eof = Some(raw);
    self
  }
}

impl HwInner for ScriptedHw {
  unsafe fn send_packet(&mut self, packet: &Packet) -> Result<Sent, Error> {
    let index = self.sent;
    if let Some(&(_, raw)) = self.failing_sends.iter().find(|(at, _)| *at == index) {
      self.sent += 1;
      return Err(Error::Ffmpeg(raw));
    }
    match self.sw.send_packet(packet) {
      Ok(()) => {
        self.sent += 1;
        Ok(Sent::Accepted)
      }
      // Not taken: the same packet is offered again, under the same index.
      Err(ffmpeg_next::Error::Other { errno }) if errno == ffmpeg_next::error::EAGAIN => {
        Ok(Sent::MustDrain)
      }
      Err(raw) => {
        self.sent += 1;
        Err(Error::Ffmpeg(raw))
      }
    }
  }

  fn receive_frame(&mut self, frame: &mut Frame) -> Result<Received, Error> {
    // SAFETY: the software decoder fills the slot whole.
    match self.sw.receive_frame(unsafe { frame.as_inner_mut() }) {
      Ok(()) => {
        // SAFETY: the frame is only read.
        let pts = unsafe { frame.as_inner_mut() }.pts();
        match self
          .failing_pictures
          .iter()
          .find(|(at, _)| Some(*at) == pts)
        {
          Some(&(_, raw)) => Err(Error::Ffmpeg(raw)),
          None => Ok(Received::Produced),
        }
      }
      Err(ffmpeg_next::Error::Other { errno }) if errno == ffmpeg_next::error::EAGAIN => {
        Ok(Received::NeedsInput)
      }
      Err(ffmpeg_next::Error::Eof) => Ok(Received::Ended),
      Err(raw) => Err(Error::Ffmpeg(raw)),
    }
  }

  fn send_eof(&mut self) -> Result<Sent, Error> {
    if let Some(raw) = self.failing_eof.take() {
      return Err(Error::Ffmpeg(raw));
    }
    match self.sw.send_eof() {
      Ok(()) => Ok(Sent::Accepted),
      Err(ffmpeg_next::Error::Other { errno }) if errno == ffmpeg_next::error::EAGAIN => {
        Ok(Sent::MustDrain)
      }
      Err(raw) => Err(Error::Ffmpeg(raw)),
    }
  }

  fn flush(&mut self) -> Result<(), Error> {
    self.sw.flush();
    Ok(())
  }

  fn as_video_decoder(&self) -> Option<&VideoDecoder> {
    None
  }
}

/// Every picture a session has ready, by PTS.
fn drain_pictures(dec: &mut FfmpegVideoStreamDecoder, pictures: &mut Vec<i64>) {
  let mut dst = crate::empty_owned_video_frame();
  loop {
    match dec.receive_frame(&mut dst) {
      Ok(Received::Produced) => pictures.push(dst.pts().map_or(i64::MIN, |t| t.pts())),
      Ok(Received::NeedsInput | Received::Ended) => break,
      Err(e) => panic!("receive_frame: {e:?}"),
    }
  }
}

/// The packet at `index`, as a caller hands it over.
fn pushed(clip: &SyntheticClip, index: usize) -> crate::OwnedVideoPacket {
  // SAFETY: the packet came from `avcodec_receive_packet` or `av_read_frame`, which wrote its
  // payload.
  unsafe {
    boundary::video_packet_from_ffmpeg(&clip.packets[index], mediadecode::Timebase::SECONDS)
  }
  .expect("a wrappable payload")
  .expect("packet has a buffer")
}

/// What a session answered across a clip: the pictures that came out, by
/// PTS, and each failure as `(road, packet index, errno)` — the index of
/// the packet just sent for a send or a picture, the clip's length for
/// the end.
#[derive(Debug, Default)]
struct Answers {
  pictures: Vec<i64>,
  failures: Vec<(&'static str, usize, ffmpeg_next::Error)>,
}

/// Drains a session after the packet at `index`. A picture that fails is
/// recorded and the drain goes on: the next call is the session's to
/// answer.
fn drain_answers(
  dec: &mut FfmpegVideoStreamDecoder,
  answers: &mut Answers,
  index: usize,
  path: DecodePath,
) {
  let mut dst = crate::empty_owned_video_frame();
  loop {
    match dec.receive_frame(&mut dst) {
      Ok(Received::Produced) => answers
        .pictures
        .push(dst.pts().map_or(i64::MIN, |t| t.pts())),
      Ok(Received::NeedsInput | Received::Ended) => break,
      Err(VideoDecodeError::Decode(Error::Ffmpeg(e))) => {
        answers.failures.push(("picture", index, e))
      }
      Err(other) => panic!(
        "{path:?}, packet {index}: a picture's failure is reported as it was minted, got {other:?}"
      ),
    }
  }
}

/// Feeds the whole clip and its end, draining after each, and records what
/// came back. Every failure must arrive as it was minted, and the session
/// must still be on hardware after every call — nothing changes the road.
/// An end that fails is offered once more, as a caller would.
fn feed_through_failures(
  dec: &mut FfmpegVideoStreamDecoder,
  clip: &SyntheticClip,
  path: DecodePath,
) -> Answers {
  let mut answers = Answers::default();
  for index in 0..clip.packets.len() {
    match dec.send_packet(&pushed(clip, index)) {
      Ok(Sent::Accepted) => {}
      Ok(Sent::MustDrain) => panic!("{path:?}: a decoder drained after every packet pushed back"),
      Err(VideoDecodeError::Decode(Error::Ffmpeg(e))) => answers.failures.push(("send", index, e)),
      Err(other) => panic!(
        "{path:?}, packet {index}: a send's failure is reported as it was minted, got {other:?}"
      ),
    }
    assert!(
      dec.is_hardware() && !dec.is_software(),
      "{path:?}, packet {index}: nothing changes the road after the first picture",
    );
    drain_answers(dec, &mut answers, index, path);
    assert!(
      dec.is_hardware() && !dec.is_software(),
      "{path:?}, packet {index}: nothing changes the road after the first picture",
    );
  }
  let end = clip.packets.len();
  match dec.send_eof() {
    Ok(Sent::Accepted) => {}
    Ok(Sent::MustDrain) => panic!("{path:?}: a drained decoder pushed back on the end"),
    Err(VideoDecodeError::Decode(Error::Ffmpeg(e))) => {
      answers.failures.push(("end", end, e));
      crate::accepted(dec.send_eof(), "the end, offered again after its failure");
    }
    Err(other) => panic!("{path:?}: the end's failure is reported as it was minted, got {other:?}"),
  }
  assert!(
    dec.is_hardware() && !dec.is_software(),
    "{path:?}, at the end: nothing changes the road after the first picture",
  );
  drain_answers(dec, &mut answers, end, path);
  answers
}

/// LAW (row 1): **after the first picture every failure is its picture's
/// own, on every path that can hold hardware — reported as it was minted,
/// the session kept, the next packet reaching libavcodec.**
///
/// FFmpeg has no reliable signal that a hardware session is gone, so
/// nothing after the first picture is classified: in FFmpeg 9.0.1
/// `AVERROR_EXTERNAL` also answers one picture (`libavcodec/videotoolbox.c`
/// 126–129 and 556–559), and `ENOSYS` is NVDEC's answer to one HEVC
/// picture (`nvdec_hevc.c` 200–227). The seam fails three sends and two
/// pictures asked for, `AVERROR_EXTERNAL` and `ENOSYS` among them. Each
/// must reach the caller as itself on the road that met it, the session
/// must stay on hardware throughout, and every other picture must come
/// out. The decoder behind the seam is FFmpeg's own and was never broken,
/// so what the law sees is this wrapper passing every call through — not
/// a hardware session recovering, which is FFmpeg's.
///
/// PLANT: a wrapper that remembers a failure and refuses what follows
/// turns this red at "each next packet reaching libavcodec".
#[test]
fn every_failure_after_the_first_picture_is_its_pictures_own_on_every_hardware_path() {
  let (w, h) = (64u32, 48u32);
  let clip = encode_synthetic_clip(w, h, 12, 4);
  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));
  let enosys = ffmpeg_next::Error::Other {
    errno: libc::ENOSYS,
  };
  let failing_sends = [
    (2usize, ffmpeg_next::Error::External),
    (5, enosys),
    (9, ffmpeg_next::Error::InvalidData),
  ];
  let failing_pictures = [
    (6i64, ffmpeg_next::Error::Unknown),
    (10, ffmpeg_next::Error::External),
  ];
  for at in failing_sends
    .iter()
    .map(|&(at, _)| at)
    .chain(failing_pictures.iter().map(|&(pts, _)| pts as usize))
  {
    assert!(
      !clip.packets[at].is_key(),
      "the scripted failures sit on P-frames"
    );
  }

  for path in [
    DecodePath::Auto,
    DecodePath::AnyHardware,
    DecodePath::Hardware(Backend::VideoToolbox),
  ] {
    let seam = failing_sends
      .iter()
      .fold(ScriptedHw::new(&clip.parameters), |seam, &(at, raw)| {
        seam.failing_send(at, raw)
      });
    let seam = failing_pictures
      .iter()
      .fold(seam, |seam, &(pts, raw)| seam.failing_picture(pts, raw));
    let mut dec = FfmpegVideoStreamDecoder::from_hw_inner_for_test_as(
      Box::new(seam),
      clip.parameters.clone(),
      tb,
      path,
    )
    .expect("build test decoder");

    let answers = feed_through_failures(&mut dec, &clip, path);

    assert_eq!(
      answers.pictures,
      [0, 1, 3, 4, 7, 8, 11],
      "{path:?}: every other picture came out, each next packet reaching libavcodec",
    );
    assert_eq!(
      answers.failures,
      [
        ("send", 2, ffmpeg_next::Error::External),
        ("send", 5, enosys),
        ("picture", 6, ffmpeg_next::Error::Unknown),
        ("send", 9, ffmpeg_next::Error::InvalidData),
        ("picture", 10, ffmpeg_next::Error::External),
      ],
      "{path:?}: each failure, as itself, on the road and the packet that met it",
    );
    assert!(
      dec.sw_replay_frames_is_empty_for_test(),
      "{path:?}: nothing was replayed into anything",
    );
  }
}

/// Where a scripted failure arrives.
#[derive(Clone, Copy, Debug)]
enum FailureRoad {
  /// On the send of the packet at this index.
  Send(usize),
  /// On the picture with this PTS, asked for.
  Picture(i64),
  /// On the end of the stream, the first time it is offered.
  Eof,
}

/// LAW (rows 1 and 2): **after the first picture nothing changes the road
/// — under `Auto` as on every path, on each of the three roads a failure
/// arrives on.**
///
/// `Auto` probes at open and at no other time. An `AVERROR_EXTERNAL` on a
/// send, on a picture asked for, or at the end of the stream is that
/// call's own error: it is reported as itself on that road, the session
/// stays on hardware, nothing reaches the probe-era replay queue, and the
/// next call reaches libavcodec behind the seam — every other picture
/// comes out, and the end drains. A failed VideoToolbox restart answers
/// `AVERROR_EXTERNAL` (`libavcodec/videotoolbox.c` 1066–1067 in FFmpeg
/// 9.0.1), and every picture after it fails the same way until a new
/// parameter set re-arms the restart (1071–1072, 446–450). Whether the
/// session recovers is FFmpeg's, and when to rebuild on software is the
/// caller's; what this law pins is that nothing here changes the road.
///
/// PLANT: an `Auto` that opens software on a committed session's failure
/// turns this red at "nothing changes the road".
#[test]
fn after_the_first_picture_nothing_changes_the_road_on_any_path() {
  let (w, h) = (64u32, 48u32);
  let clip = encode_synthetic_clip(w, h, 12, 4);
  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));
  let external = ffmpeg_next::Error::External;

  for path in [
    DecodePath::Auto,
    DecodePath::AnyHardware,
    DecodePath::Hardware(Backend::VideoToolbox),
  ] {
    for road in [
      FailureRoad::Send(5),
      FailureRoad::Picture(5),
      FailureRoad::Eof,
    ] {
      let seam = ScriptedHw::new(&clip.parameters);
      let (seam, expected) = match road {
        FailureRoad::Send(at) => (seam.failing_send(at, external), ("send", at)),
        FailureRoad::Picture(pts) => (
          seam.failing_picture(pts, external),
          ("picture", pts as usize),
        ),
        FailureRoad::Eof => (seam.failing_eof(external), ("end", clip.packets.len())),
      };
      let mut dec = FfmpegVideoStreamDecoder::from_hw_inner_for_test_as(
        Box::new(seam),
        clip.parameters.clone(),
        tb,
        path,
      )
      .expect("build test decoder");

      let answers = feed_through_failures(&mut dec, &clip, path);

      let failed = match road {
        FailureRoad::Send(at) => Some(at as i64),
        FailureRoad::Picture(pts) => Some(pts),
        FailureRoad::Eof => None,
      };
      let expected_pictures: Vec<i64> = (0..clip.packets.len() as i64)
        .filter(|pts| Some(*pts) != failed)
        .collect();
      assert_eq!(
        answers.pictures, expected_pictures,
        "{path:?} {road:?}: every other picture came out — the next call reaches libavcodec",
      );
      assert_eq!(
        answers.failures,
        [(expected.0, expected.1, external)],
        "{path:?} {road:?}: the failure, as itself, on the road that met it",
      );
      assert!(
        dec.is_hardware() && !dec.is_software(),
        "{path:?} {road:?}: nothing changes the road after the first picture",
      );
      assert!(
        dec.sw_replay_frames_is_empty_for_test(),
        "{path:?} {road:?}: nothing was replayed into anything",
      );
    }
  }
}

/// LAW (row 1): **after a failure and a `flush`, the packets reach
/// libavcodec, on every path that can hold hardware.** An
/// `AVERROR_EXTERNAL` on a send, then a seek's flush: the packets from the
/// next keyframe on reach the decoder behind the seam, every picture from
/// that keyframe comes out, and the session is still on hardware.
///
/// The decoder behind the seam is FFmpeg's own and was never broken, so
/// the law pins what this wrapper's `flush` keeps of a failure — nothing —
/// and not that a hardware session recovers: `flush` rebuilds none (see
/// `VideoDecoder::flush`).
#[test]
fn after_a_failure_and_a_flush_the_packets_reach_libavcodec_on_every_hardware_path() {
  let (w, h) = (64u32, 48u32);
  let clip = encode_synthetic_clip(w, h, 12, 4);
  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));
  let fail_at = 2;
  let resume_at = 4;
  assert!(
    !clip.packets[fail_at].is_key() && clip.packets[resume_at].is_key(),
    "the failure sits on a P-frame, and the seek lands on a keyframe",
  );

  for path in [
    DecodePath::Auto,
    DecodePath::AnyHardware,
    DecodePath::Hardware(Backend::VideoToolbox),
  ] {
    let mut dec = FfmpegVideoStreamDecoder::from_hw_inner_for_test_as(
      Box::new(
        ScriptedHw::new(&clip.parameters).failing_send(fail_at, ffmpeg_next::Error::External),
      ),
      clip.parameters.clone(),
      tb,
      path,
    )
    .expect("build test decoder");

    let mut pictures = Vec::new();
    for index in 0..fail_at {
      crate::accepted(dec.send_packet(&pushed(&clip, index)), "send_packet");
      drain_pictures(&mut dec, &mut pictures);
    }
    let failed = dec.send_packet(&pushed(&clip, fail_at));
    assert!(
      matches!(
        failed,
        Err(VideoDecodeError::Decode(Error::Ffmpeg(
          ffmpeg_next::Error::External
        )))
      ),
      "{path:?}: the send's own failure, as itself, got {failed:?}",
    );

    dec.flush().expect("flush");
    let mut after = Vec::new();
    for index in resume_at..clip.packets.len() {
      crate::accepted(
        dec.send_packet(&pushed(&clip, index)),
        "after a flush the packet reaches libavcodec",
      );
      drain_pictures(&mut dec, &mut after);
    }
    crate::accepted(dec.send_eof(), "after a flush the end reaches libavcodec");
    drain_pictures(&mut dec, &mut after);

    assert_eq!(
      pictures,
      [0, 1],
      "{path:?}: the pictures before the failure"
    );
    assert_eq!(
      after,
      (resume_at as i64..clip.packets.len() as i64).collect::<Vec<_>>(),
      "{path:?}: after a flush the packets reach libavcodec — every picture from the keyframe on",
    );
    assert!(
      dec.is_hardware() && !dec.is_software(),
      "{path:?}: nothing changes the road after the first picture",
    );
  }
}

// ---------------------------------------------------------------------------
//  AnyHardware: the probe, and never software
// ---------------------------------------------------------------------------

/// LAW (row 3): **`AnyHardware` refuses at open on a platform with no
/// hardware backend, with no packet consumed — where `Auto` opens
/// software.**
///
/// No machine that runs this suite is such a platform, so the probe is
/// handed the order one has: none. `AnyHardware` answers
/// [`Error::ProbeExhausted`] at once — no attempt, since there is no
/// backend to try, and no packet, since none was sent — and the control
/// beside it, `Auto` over the same order, opens libavcodec's own decoder.
///
/// PLANT: an `AnyHardware` arm that falls back as `Auto` does turns this
/// red at "refuses at open".
#[test]
fn any_hardware_refuses_at_open_on_a_platform_with_no_hardware_backend() {
  let (w, h) = (64u32, 48u32);
  let clip = encode_synthetic_clip(w, h, 8, 4);
  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));

  match FfmpegVideoStreamDecoder::open_from_in(
    VideoStreamDecoderOpen::new().with_path(DecodePath::AnyHardware),
    clip.parameters.clone(),
    tb,
    DecoderLimits::default(),
    &[],
  ) {
    Ok(dec) => panic!(
      "AnyHardware refuses at open where no hardware backend exists — it opened a {} session",
      if dec.is_software() {
        "software"
      } else {
        "hardware"
      },
    ),
    Err(Error::ProbeExhausted(p)) => {
      assert!(p.origin().is_probe(), "the probe's own exhaustion");
      assert!(
        p.attempts().is_empty(),
        "no backend to try, so no attempt: {:?}",
        p.attempts()
      );
      assert!(
        p.unconsumed_packets().is_empty(),
        "no packet was consumed: none had been sent"
      );
    }
    Err(other) => panic!("expected the probe's exhaustion, got {other:?}"),
  }

  let auto = FfmpegVideoStreamDecoder::open_from_in(
    VideoStreamDecoderOpen::new(),
    clip.parameters.clone(),
    tb,
    DecoderLimits::default(),
    &[],
  )
  .expect("Auto opens software where no hardware backend exists");
  assert!(
    auto.is_software(),
    "the control: Auto over the same order is a software session"
  );
}

/// The probe's exhaustion under `AnyHardware`, checked on whichever road
/// met it: the probe's origin, the packets it took — the first `taken`
/// of the clip, in the order they were sent — and a session that is
/// still, and only, a hardware one.
#[track_caller]
fn assert_the_probes_packets_come_back(
  dec: &FfmpegVideoStreamDecoder,
  refusal: VideoDecodeError,
  clip: &SyntheticClip,
  taken: usize,
  road: &str,
) {
  let VideoDecodeError::Decode(Error::ProbeExhausted(p)) = &refusal else {
    panic!("{road}: expected the probe's exhaustion, got {refusal:?}");
  };
  assert!(p.origin().is_probe(), "{road}: the probe's own exhaustion");
  let body = |packet: &Packet| packet.data().unwrap_or_default().to_vec();
  let rescued: Vec<Vec<u8>> = p.unconsumed_packets().iter().map(body).collect();
  let sent: Vec<Vec<u8>> = clip.packets[..taken].iter().map(body).collect();
  assert!(
    rescued == sent,
    "{road}: the packets the probe took come back, in order ({} rescued, {} sent)",
    rescued.len(),
    sent.len(),
  );
  assert!(
    dec.sw_replay_frames_is_empty_for_test(),
    "{road}: nothing was replayed into anything"
  );
}

/// LAW (row 3): **a probe-era exhaustion under `AnyHardware` hands back
/// the packets the probe took, on every road, and never opens software.**
///
/// The seam takes every packet into its history and delivers nothing
/// until its probe exhausts — on a send, on the first picture asked for,
/// or at the end of the stream. Each road must answer
/// [`Error::ProbeExhausted`] with the probe's origin and the packets in
/// the order they were sent, so the caller can replay them into a
/// software session of its own; and the session must stay a hardware
/// one, with nothing replayed behind the caller.
///
/// PLANT: letting `AnyHardware` open software on its probe's exhaustion
/// turns this red at "never opens software".
#[test]
fn any_hardware_hands_back_the_probes_packets_and_never_opens_software() {
  let (w, h) = (64u32, 48u32);
  let clip = encode_synthetic_clip(w, h, 12, 6);
  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));
  let taken = 3;
  let any_hardware = |seam: FakeHw| {
    FfmpegVideoStreamDecoder::from_hw_inner_for_test_as(
      Box::new(seam),
      clip.parameters.clone(),
      tb,
      DecodePath::AnyHardware,
    )
    .expect("build an AnyHardware test decoder")
  };

  // On a send: the probe exhausts on the packet after the ones it took.
  let mut dec = any_hardware(FakeHw::failing(w, h, 0, taken));
  for index in 0..taken {
    crate::accepted(dec.send_packet(&pushed(&clip, index)), "send_packet");
  }
  let answer = dec.send_packet(&pushed(&clip, taken));
  assert!(
    dec.is_hardware() && !dec.is_software(),
    "on a send: AnyHardware never opens software"
  );
  let refusal = answer.expect_err("on a send: the probe's exhaustion is reported");
  assert_the_probes_packets_come_back(&dec, refusal, &clip, taken, "on a send");

  // On the first picture asked for.
  let mut dec = any_hardware(FakeHw::failing_at_receive(w, h));
  for index in 0..taken {
    crate::accepted(dec.send_packet(&pushed(&clip, index)), "send_packet");
  }
  let mut dst = crate::empty_owned_video_frame();
  let answer = dec.receive_frame(&mut dst);
  assert!(
    dec.is_hardware() && !dec.is_software(),
    "on a picture: AnyHardware never opens software"
  );
  let refusal = answer.expect_err("on a picture: the probe's exhaustion is reported");
  assert_the_probes_packets_come_back(&dec, refusal, &clip, taken, "on a picture");

  // At the end of the stream.
  let mut dec = any_hardware(FakeHw::failing_at_eof(w, h));
  for index in 0..taken {
    crate::accepted(dec.send_packet(&pushed(&clip, index)), "send_packet");
  }
  let answer = dec.send_eof();
  assert!(
    dec.is_hardware() && !dec.is_software(),
    "at the end: AnyHardware never opens software"
  );
  let refusal = answer.expect_err("at the end: the probe's exhaustion is reported");
  assert_the_probes_packets_come_back(&dec, refusal, &clip, taken, "at the end");
  assert!(
    !dec.eof_sent_for_test(),
    "a refused end is not a committed one"
  );
}

/// LAW (row 3): **on this platform's own backends too, `AnyHardware` is
/// never a software session** — whatever its hardware makes of the
/// stream. The probe opens on a backend or refuses at open with no packet
/// consumed; after that the session decodes on hardware, reports its
/// probe's exhaustion, or reports a picture's own error — and at no point
/// has it opened software.
#[test]
fn any_hardware_never_becomes_software_on_this_platform() {
  let (w, h) = (64u32, 48u32);
  let clip = encode_synthetic_clip(w, h, 8, 4);
  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));

  let mut dec = match FfmpegVideoStreamDecoder::open_as(
    clip.parameters.clone(),
    tb,
    DecoderLimits::default(),
    DecodePath::AnyHardware,
  ) {
    Ok(dec) => dec,
    Err(Error::ProbeExhausted(p)) => {
      assert!(
        p.origin().is_probe() && p.unconsumed_packets().is_empty(),
        "refused at open, with no packet consumed"
      );
      return;
    }
    Err(other) => panic!("AnyHardware opens or refuses with the probe's exhaustion, got {other:?}"),
  };
  assert!(
    dec.is_hardware() && !dec.is_software(),
    "opened on hardware"
  );

  let mut dst = crate::empty_owned_video_frame();
  for index in 0..clip.packets.len() {
    let sent = dec.send_packet(&pushed(&clip, index));
    assert!(
      !dec.is_software(),
      "packet {index}: AnyHardware never becomes software"
    );
    if let Err(VideoDecodeError::Decode(Error::ProbeExhausted(p))) = &sent {
      assert!(p.origin().is_probe(), "an exhaustion here is the probe's");
      return;
    }
    while let Ok(Received::Produced) = dec.receive_frame(&mut dst) {}
    assert!(
      !dec.is_software(),
      "packet {index}: AnyHardware never becomes software"
    );
  }
}
