use std::io::{self, Cursor, SeekFrom};

use ffmpeg_next::ffi::{AV_DISPOSITION_DEFAULT, AV_DISPOSITION_FORCED, av_buffer_get_ref_count};
use mediaspine::Timestamp;

use super::*;
use crate::{FfmpegBuffer, FfmpegBytes, View, view::Origin};

const PADDING: usize = AV_INPUT_BUFFER_PADDING_SIZE as usize;

fn ms(pts: i64) -> Timestamp {
  Timestamp::new(pts, millis())
}

fn millis() -> Timebase {
  Timebase::new(1, NonZeroI32::new(1000).expect("non-zero"))
}

/// A ticket for a stream of `media` and `codec`, as a demuxer's mirror
/// of it would read: the two fields written, everything else the
/// allocator's defaults.
fn ticket(media: ffmpeg_next::ffi::AVMediaType, codec: CodecId) -> CodecTicket {
  let mut parameters = ffmpeg_next::codec::Parameters::new();
  // SAFETY: `Parameters::new` allocated a live `AVCodecParameters` (it
  // is checked non-null first) that nothing else holds; the two enums
  // are written as the integers they are stored as.
  unsafe {
    let par = parameters.as_mut_ptr();
    assert!(!par.is_null(), "avcodec_parameters_alloc");
    write_unaligned(addr_of_mut!((*par).codec_type).cast::<i32>(), media as i32);
    write_unaligned(addr_of_mut!((*par).codec_id).cast::<i32>(), codec.raw());
  }
  CodecTicket::mirror(&parameters, 0, 1 << 20).expect("a mirrorable ticket")
}

fn subrip() -> CodecTicket {
  ticket(
    ffmpeg_next::ffi::AVMediaType::AVMEDIA_TYPE_SUBTITLE,
    CodecId::SUBRIP,
  )
}

/// A cue on the owned lane.
fn cue(text: &[u8], pts: i64) -> SubtitlePacket<SubtitlePacketExtra, FfmpegBytes> {
  SubtitlePacket::new(
    FfmpegBytes::copy_from_slice(text),
    SubtitlePacketExtra::new(0),
  )
  .with_pts(Some(ms(pts)))
  .with_duration(Some(ms(500)))
}

fn times(pts: i64) -> PacketTimes {
  PacketTimes::new(Some(pts), Some(pts), 500)
}

/// **A context writes a whole container into a caller's sink, and hands
/// the sink back.** A Matroska file with one SubRip track: the header,
/// two cues and the trailer reach the sink, which comes back holding
/// them — the EBML signature first, the cues' bytes inside.
#[test]
fn a_container_is_written_into_the_sink_and_the_sink_handed_back() {
  ffmpeg_next::init().expect("ffmpeg init");
  let mut output = Output::open(Cursor::new(Vec::new()), "matroska").expect("open");
  assert_eq!(output.container(), "matroska");
  assert!(
    !output.strict_decode_times(),
    "Matroska is AVFMT_TS_NONSTRICT"
  );
  let ticket = subrip();
  let stream = output
    .add_stream(&StreamSpec::new(&ticket, millis()))
    .expect("a stream");
  assert_eq!(stream, 0);
  output.write_header().expect("the header");
  assert_eq!(output.stream_time_base(0).expect("a ruler"), millis());
  for (text, pts) in [(&b"first cue"[..], 0), (&b"second cue"[..], 1000)] {
    output
      .write_packet::<crate::Owned>(
        stream,
        OutputPacket::Subtitle(&cue(text, pts)),
        times(pts),
        PacketLimits::default(),
      )
      .expect("a cue");
  }
  output.write_trailer().expect("the trailer");
  let bytes = output.into_sink().expect("the sink back").into_inner();
  assert_eq!(&bytes[..4], &[0x1A, 0x45, 0xDF, 0xA3], "the EBML signature");
  for text in [&b"first cue"[..], &b"second cue"[..]] {
    assert!(
      bytes.windows(text.len()).any(|w| w == text),
      "the cue {:?} is in the file",
      String::from_utf8_lossy(text),
    );
  }
}

/// **libavformat's order is kept, and a call out of it refused by
/// name.** A packet before the header, a stream or chapter after it, a
/// second header and a second trailer are each refused, naming the call
/// and where the context was; nothing reaches libavformat.
#[test]
fn a_call_out_of_order_is_refused_by_name() {
  ffmpeg_next::init().expect("ffmpeg init");
  let ticket = subrip();
  let mut output = Output::open(Cursor::new(Vec::new()), "matroska").expect("open");
  let stream = output
    .add_stream(&StreamSpec::new(&ticket, millis()))
    .expect("a stream");
  let early = output.write_packet::<crate::Owned>(
    stream,
    OutputPacket::Subtitle(&cue(b"early", 0)),
    times(0),
    PacketLimits::default(),
  );
  assert!(matches!(
    early,
    Err(OutputError::OutOfOrder {
      call: OutputCall::WritePacket,
      phase: OutputPhase::Building
    })
  ));
  assert!(matches!(
    output.write_trailer(),
    Err(OutputError::OutOfOrder {
      call: OutputCall::WriteTrailer,
      phase: OutputPhase::Building
    })
  ));
  output.write_header().expect("the header");
  assert!(matches!(
    output.add_stream(&StreamSpec::new(&ticket, millis())),
    Err(OutputError::OutOfOrder {
      call: OutputCall::AddStream,
      phase: OutputPhase::Writing
    })
  ));
  assert!(matches!(
    output.add_chapter(1, millis(), 0, 10, None),
    Err(OutputError::OutOfOrder {
      call: OutputCall::AddChapter,
      ..
    })
  ));
  assert!(matches!(
    output.write_header(),
    Err(OutputError::OutOfOrder {
      call: OutputCall::WriteHeader,
      ..
    })
  ));
  assert!(matches!(
    output.write_packet::<crate::Owned>(
      7,
      OutputPacket::Subtitle(&cue(b"nowhere", 0)),
      times(0),
      PacketLimits::default(),
    ),
    Err(OutputError::NoSuchStream { stream: 7 })
  ));
  output.write_trailer().expect("the trailer");
  assert!(matches!(
    output.write_trailer(),
    Err(OutputError::OutOfOrder {
      call: OutputCall::WriteTrailer,
      phase: OutputPhase::Finished
    })
  ));
}

/// **A container is named, and refused by name**: one libavformat does
/// not know, a name holding a NUL, and a muxer that writes no byte
/// stream of its own (`image2` writes a file per frame).
#[test]
fn an_unknown_container_and_one_without_a_byte_stream_are_refused_by_name() {
  ffmpeg_next::init().expect("ffmpeg init");
  assert!(matches!(
    Output::open(Cursor::new(Vec::new()), "no-such-muxer"),
    Err(OutputError::UnknownContainer { name }) if name == "no-such-muxer"
  ));
  assert!(matches!(
    Output::open(Cursor::new(Vec::new()), "mat\0roska"),
    Err(OutputError::Nul {
      text: OutputText::ContainerName
    })
  ));
  assert!(matches!(
    Output::open(Cursor::new(Vec::new()), "image2"),
    Err(OutputError::NoByteStream { name }) if name == "image2"
  ));
}

/// **What a stream declares reaches it**: the language and the
/// disposition, and for an attachment stream its kind, its file as
/// padded extradata, and its filename and MIME type.
#[test]
fn a_streams_metadata_disposition_and_attached_file_reach_it() {
  ffmpeg_next::init().expect("ffmpeg init");
  let mut output = Output::open(Cursor::new(Vec::new()), "matroska").expect("open");
  let cues = subrip();
  let disposition = AV_DISPOSITION_DEFAULT | AV_DISPOSITION_FORCED;
  output
    .add_stream(
      &StreamSpec::new(&cues, millis())
        .with_language(Some("jpn"))
        .with_disposition(disposition),
    )
    .expect("the cue stream");
  let font = ticket(
    ffmpeg_next::ffi::AVMediaType::AVMEDIA_TYPE_ATTACHMENT,
    CodecId::TTF,
  );
  let file = b"\x00\x01\x00\x00a font";
  output
    .add_stream(
      &StreamSpec::new(&font, millis())
        .with_media(StreamMedia::Attachment)
        .with_extradata(Some(file))
        .with_filename(Some("font.ttf"))
        .with_mime_type(Some("font/ttf")),
    )
    .expect("the attachment stream");

  let cue_stream = output.stream(0).expect("stream 0");
  let font_stream = output.stream(1).expect("stream 1");
  // SAFETY: both are live streams of the live context; plain fields and
  // the dictionaries the context owns are read, nothing written.
  unsafe {
    assert_eq!((*cue_stream).disposition, disposition);
    assert_eq!(
      metadata_value((*cue_stream).metadata, c"language").as_deref(),
      Some("jpn")
    );
    let par = (*font_stream).codecpar;
    assert_eq!(
      read_unaligned(addr_of!((*par).codec_type).cast::<i32>()),
      MEDIA_ATTACHMENT
    );
    let extradata =
      core::slice::from_raw_parts((*par).extradata, (*par).extradata_size as usize + PADDING);
    assert_eq!(&extradata[..file.len()], file, "the file is the extradata");
    assert!(
      extradata[file.len()..].iter().all(|&byte| byte == 0),
      "with zeroed padding behind it",
    );
    assert_eq!(
      metadata_value((*font_stream).metadata, c"filename").as_deref(),
      Some("font.ttf")
    );
    assert_eq!(
      metadata_value((*font_stream).metadata, c"mimetype").as_deref(),
      Some("font/ttf")
    );
  }

  // A NUL in a text is refused before anything is added.
  assert!(matches!(
    output.add_stream(&StreamSpec::new(&cues, millis()).with_language(Some("e\0ng"))),
    Err(OutputError::Nul {
      text: OutputText::Language
    })
  ));
  assert_eq!(output.streams(), 2, "nothing was added for the refusal");
  assert_eq!(output.phase(), OutputPhase::Building);
}

/// **A chapter reaches the context with its span and title**, and the
/// context owns it from then on (it is freed with the context).
#[test]
fn a_chapter_is_added_with_its_span_and_title() {
  ffmpeg_next::init().expect("ffmpeg init");
  let mut output = Output::open(Cursor::new(Vec::new()), "matroska").expect("open");
  output
    .add_chapter(7, millis(), 0, 1500, Some("Opening"))
    .expect("a chapter");
  output
    .add_chapter(8, millis(), 1500, 3000, None)
    .expect("another");
  let ctx = output.ctx.as_ptr();
  // SAFETY: the live context's chapter array holds `nb_chapters` live
  // chapters; plain fields and their dictionaries are read.
  unsafe {
    assert_eq!((*ctx).nb_chapters, 2);
    let first = *(*ctx).chapters;
    assert_eq!(((*first).id, (*first).start, (*first).end), (7, 0, 1500));
    assert_eq!(((*first).time_base.num, (*first).time_base.den), (1, 1000));
    assert_eq!(
      metadata_value((*first).metadata, c"title").as_deref(),
      Some("Opening")
    );
    let second = *(*ctx).chapters.add(1);
    assert_eq!(metadata_value((*second).metadata, c"title"), None);
  }
  assert!(matches!(
    output.add_chapter(9, millis(), 0, 1, Some("a\0b")),
    Err(OutputError::Nul {
      text: OutputText::ChapterTitle
    })
  ));
}

/// **A packet reaches libavformat as a copy, never as a reference to
/// the caller's buffer** — even while libavformat holds it in its
/// interleaving queue past the call.
///
/// Two streams, so the packet written to the first is queued until the
/// second has one. The carrier is a view-lane payload with the padding
/// behind it — the one shape the scoped decoder submission would share —
/// and its buffer stays referenced by the carrier alone throughout.
///
/// PLANT: `BodyRoute::Submission` in `write_packet` turns this red at
/// "libavformat's queue holds a reference to the caller's buffer".
#[test]
fn a_packet_reaches_libavformat_as_a_copy_never_the_callers_buffer() {
  ffmpeg_next::init().expect("ffmpeg init");
  let text = b"a cue whose buffer is the caller's";
  // SAFETY: a plain allocation of `len + PADDING` bytes, checked for null,
  // written whole — the payload, then zeroes — through its own pointer.
  let raw = unsafe {
    let raw = ffmpeg_next::ffi::av_buffer_alloc(text.len() + PADDING);
    assert!(!raw.is_null(), "av_buffer_alloc");
    ptr::copy_nonoverlapping(text.as_ptr(), (*raw).data, text.len());
    ptr::write_bytes((*raw).data.add(text.len()), 0, PADDING);
    raw
  };
  // SAFETY: `raw` is live and the extent is inside it; the view takes a
  // reference of its own, and the test's reference is released below.
  let carrier =
    unsafe { FfmpegBuffer::view_of(raw, 0, text.len(), Origin::PacketPayload) }.expect("a view");
  // SAFETY: the test's own reference, released once; the carrier keeps
  // the buffer alive by its own.
  unsafe { ffmpeg_next::ffi::av_buffer_unref(&mut { raw }) };
  let packet = SubtitlePacket::new(carrier, SubtitlePacketExtra::new(0))
    .with_pts(Some(ms(0)))
    .with_duration(Some(ms(500)));

  let ticket = subrip();
  let mut output = Output::open(Cursor::new(Vec::new()), "matroska").expect("open");
  let first = output
    .add_stream(&StreamSpec::new(&ticket, millis()))
    .expect("a stream");
  output
    .add_stream(&StreamSpec::new(&ticket, millis()))
    .expect("a second stream");
  output.write_header().expect("the header");
  output
    .write_packet::<View>(
      first,
      OutputPacket::Subtitle(&packet),
      times(0),
      PacketLimits::default(),
    )
    .expect("a cue");
  // SAFETY: the carrier holds a live reference; the count is an atomic
  // read.
  let references = unsafe { av_buffer_get_ref_count(packet.data().as_av_buffer_ref()) };
  assert_eq!(
    references, 1,
    "libavformat's queue holds a reference to the caller's buffer",
  );
  output.write_trailer().expect("the trailer");
  drop(output);
  assert_eq!(
    packet.data().as_ref(),
    text,
    "the caller's bytes are untouched"
  );
}

/// A sink that counts its calls and panics in every one.
#[derive(Default)]
struct Exploding {
  calls: std::sync::Arc<std::sync::atomic::AtomicU32>,
}

impl Write for Exploding {
  fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
    self
      .calls
      .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    panic!("the sink exploded");
  }
  fn flush(&mut self) -> io::Result<()> {
    self
      .calls
      .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    panic!("the sink exploded");
  }
}

impl Seek for Exploding {
  fn seek(&mut self, _pos: SeekFrom) -> io::Result<u64> {
    self
      .calls
      .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    panic!("the sink exploded");
  }
}

/// Silences the default hook for one call: the panics are deliberate.
fn quietly<T>(call: impl FnOnce() -> T) -> T {
  let previous = std::panic::take_hook();
  std::panic::set_hook(Box::new(|_| {}));
  let out = call();
  std::panic::set_hook(previous);
  out
}

/// **A sink that panics is named, and never called again.** The first
/// call into it panics inside libavformat's write; the guard latches the
/// panic and answers an I/O error, and the context reports the panic by
/// name, not the error libavformat folded it into. Every later call —
/// the trailer, the final flush — is answered without reaching the sink.
#[test]
fn a_sink_that_panics_is_named_and_never_called_again() {
  ffmpeg_next::init().expect("ffmpeg init");
  let sink = Exploding::default();
  let calls = std::sync::Arc::clone(&sink.calls);
  let ticket = subrip();
  let outcome = quietly(|| {
    let mut output = Output::open(sink, "matroska").expect("open");
    output
      .add_stream(&StreamSpec::new(&ticket, millis()))
      .expect("a stream");
    let mut results = vec![output.write_header().err()];
    if output.phase() == OutputPhase::Writing {
      results.push(
        output
          .write_packet::<crate::Owned>(
            0,
            OutputPacket::Subtitle(&cue(b"a cue", 0)),
            times(0),
            PacketLimits::default(),
          )
          .err(),
      );
      results.push(output.write_trailer().err());
    }
    let panic = output.sink_panic();
    (results, panic, output.into_sink().err())
  });
  let (results, panic, last) = outcome;
  assert_eq!(panic.as_deref(), Some("the sink exploded"));
  assert!(
    results.iter().flatten().any(
      |error| matches!(error, OutputError::SinkPanic { message } if message == "the sink exploded")
    ),
    "the panic is named where libavformat failed: {results:?}",
  );
  assert!(matches!(last, Some(OutputError::SinkPanic { .. })));
  assert_eq!(
    calls.load(std::sync::atomic::Ordering::Relaxed),
    1,
    "only the first call reached the sink",
  );
}

/// An error's payload whose destructor panics — safe code, returned
/// rather than raised.
#[derive(Debug)]
struct HostilePayload;

impl core::fmt::Display for HostilePayload {
  fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
    f.write_str("an error whose payload panics when dropped")
  }
}

impl std::error::Error for HostilePayload {}

impl Drop for HostilePayload {
  fn drop(&mut self) {
    panic!("the error's payload exploded");
  }
}

/// A sink whose every write fails with an error carrying that payload,
/// and counts the writes; it seeks and flushes as a buffer does.
#[derive(Default)]
struct ReturnsAHostileError {
  seekable: Cursor<Vec<u8>>,
  writes: std::sync::Arc<std::sync::atomic::AtomicU32>,
}

impl Write for ReturnsAHostileError {
  fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
    self
      .writes
      .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    Err(io::Error::new(io::ErrorKind::StorageFull, HostilePayload))
  }
  fn flush(&mut self) -> io::Result<()> {
    Ok(())
  }
}

impl Seek for ReturnsAHostileError {
  fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
    self.seekable.seek(pos)
  }
}

/// **A sink error whose payload panics on drop is latched and named,
/// and the process does not abort.** libavformat writes the header
/// through ffmpeg-next's `extern "C"` callback, which drops the error a
/// sink returns before it answers C; the guard reduces the error inside
/// its `catch_unwind` first, so the destructor's panic is latched, the
/// context names it, and the sink is never written again.
///
/// It runs in a child process ([`crate::fault_subprocess`]): the parent
/// re-runs this one test in a child of the test binary and asserts the
/// child exited cleanly. Without the reduction the payload is dropped in
/// the callback, the panic cannot unwind through C, and the child dies
/// of the abort — which the parent reports as this law failing, with the
/// child's output, instead of losing the test binary to it.
#[test]
fn a_sink_error_whose_payload_panics_on_drop_is_named_without_an_abort() {
  crate::fault_subprocess::in_subprocess(
    "output::tests::a_sink_error_whose_payload_panics_on_drop_is_named_without_an_abort",
    || {
      ffmpeg_next::init().expect("ffmpeg init");
      let sink = ReturnsAHostileError::default();
      let writes = std::sync::Arc::clone(&sink.writes);
      let ticket = subrip();
      let (panic, handed_back) = quietly(|| {
        let mut output = Output::open(sink, "matroska").expect("open");
        output
          .add_stream(&StreamSpec::new(&ticket, millis()))
          .expect("a stream");
        // Whether the header's own call reports the failed write is
        // libavformat's business; the context hands the sink back
        // either way, and names the panic there.
        drop(output.write_header());
        (output.sink_panic(), output.into_sink().err())
      });
      assert_eq!(panic.as_deref(), Some("the error's payload exploded"));
      assert!(
        matches!(
          &handed_back,
          Some(OutputError::SinkPanic { message }) if message == "the error's payload exploded"
        ),
        "the panic is named: {handed_back:?}",
      );
      assert_eq!(
        writes.load(std::sync::atomic::Ordering::Relaxed),
        1,
        "only the first write reached the sink",
      );
    },
  );
}

/// A sink whose writes fail as a full disk fails them, and whose flush
/// succeeds; it seeks as a buffer does.
#[derive(Debug, Default)]
struct Full {
  seekable: Cursor<Vec<u8>>,
}

impl Write for Full {
  fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
    Err(io::Error::from_raw_os_error(libc::ENOSPC))
  }
  fn flush(&mut self) -> io::Result<()> {
    Ok(())
  }
}

impl Seek for Full {
  fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
    self.seekable.seek(pos)
  }
}

/// **A write the sink failed is reported when the sink is handed back,
/// though its own flush succeeds.** The header's bytes reach the sink in
/// a write that fails as a full disk fails it; the I/O context keeps that
/// error and writes nothing after it, and `into_sink` reports it rather
/// than hand back a sink missing the header — whatever the header's own
/// call was answered (it names the same write: the law above).
#[test]
fn a_failed_write_is_reported_when_the_sink_is_handed_back() {
  ffmpeg_next::init().expect("ffmpeg init");
  let ticket = subrip();
  let mut output = Output::open(Full::default(), "matroska").expect("open");
  output
    .add_stream(&StreamSpec::new(&ticket, millis()))
    .expect("a stream");
  drop(output.write_header());
  match output.into_sink() {
    Err(OutputError::SinkWrite(error)) => assert_eq!(
      error,
      ffmpeg_next::Error::from(ffmpeg_next::ffi::AVERROR(libc::ENOSPC)),
      "the failed write's own error",
    ),
    other => panic!("expected the failed write reported, got {other:?}"),
  }
}

/// A buffer whose writes fail as a full disk fails them once its switch
/// is thrown; it seeks and flushes as a buffer does.
#[derive(Debug, Default)]
struct FillsUp {
  bytes: Cursor<Vec<u8>>,
  full: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl Write for FillsUp {
  fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
    if self.full.load(std::sync::atomic::Ordering::Relaxed) {
      return Err(io::Error::from_raw_os_error(libc::ENOSPC));
    }
    self.bytes.write(buf)
  }
  fn flush(&mut self) -> io::Result<()> {
    Ok(())
  }
}

impl Seek for FillsUp {
  fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
    self.bytes.seek(pos)
  }
}

/// The failed write a full disk answers, as the I/O context keeps it.
fn enospc() -> ffmpeg_next::Error {
  ffmpeg_next::Error::from(ffmpeg_next::ffi::AVERROR(libc::ENOSPC))
}

/// **A write the sink fails is named the sink's at the call that met it —
/// the header, a packet, the trailer — never libavformat's.** The header
/// flushes its bytes only after it has read the I/O context's error
/// (`mux.c` 482–486 in FFmpeg 9.0.1), so libavformat answers it success
/// over a sink that took none of them; a packet's write and the trailer
/// answer the failed write as libavformat's own code (751–752, 1270–1272).
/// Each call reads the I/O context's error after the panic and the seek
/// records, and names it [`OutputError::SinkWrite`]. The packet is MP4's,
/// which writes each sample as it comes; Matroska holds its packets in a
/// cluster, which reaches the sink at the trailer — the third case. After
/// the header's and the packet's, the context is failed (R20, ruled: the
/// write-failure poison): a later packet and the trailer are refused by
/// name, nothing reaching libavformat — the packet's though the sink would
/// take it again.
///
/// PLANT: without that read in `sink_fault`, the header answers `Ok` and
/// the packet and the trailer `Ffmpeg(ENOSPC)`. Without the poison, the
/// later packet and the trailer reach libavformat, which answers the I/O
/// context's kept failure.
#[test]
fn a_failed_write_is_named_the_sinks_at_the_header_a_packet_and_the_trailer() {
  ffmpeg_next::init().expect("ffmpeg init");
  let subrip = subrip();
  let mov_text = ticket(
    ffmpeg_next::ffi::AVMediaType::AVMEDIA_TYPE_SUBTITLE,
    CodecId::MOV_TEXT,
  );
  let open = |container: &str, full_from_the_start: bool| {
    let sink = FillsUp::default();
    let full = std::sync::Arc::clone(&sink.full);
    full.store(full_from_the_start, std::sync::atomic::Ordering::Relaxed);
    let mut output = Output::open(sink, container).expect("open");
    let ticket = if container == "mp4" {
      &mov_text
    } else {
      &subrip
    };
    output
      .add_stream(&StreamSpec::new(ticket, millis()))
      .expect("a stream");
    (output, full)
  };
  let named = |what: &str, answer: Result<(), OutputError>| match answer {
    Err(OutputError::SinkWrite(error)) => assert_eq!(error, enospc(), "{what}"),
    other => panic!("{what}: expected the sink's failed write named, got {other:?}"),
  };

  // At the header: nothing of it reaches the sink.
  let (mut output, _) = open("matroska", true);
  named("the header", output.write_header());
  assert_eq!(output.phase(), OutputPhase::Failed);
  assert!(
    matches!(
      output.write_packet::<crate::Owned>(
        0,
        OutputPacket::Subtitle(&cue(b"after", 0)),
        times(0),
        PacketLimits::default(),
      ),
      Err(OutputError::FailedEarlier {
        call: OutputCall::WritePacket
      })
    ),
    "a packet after a failed header refused by name"
  );
  named("the sink handed back", output.into_sink().map(drop));

  // At a packet: the header was written; the cue's sample meets a full
  // disk. A 16-bit length, then the text: how `mov_text` frames a cue.
  let (mut output, full) = open("mp4", false);
  output.write_header().expect("the header");
  full.store(true, std::sync::atomic::Ordering::Relaxed);
  named(
    "a packet",
    output.write_packet::<crate::Owned>(
      0,
      OutputPacket::Subtitle(&cue(b"\x00\x05a cue", 0)),
      times(0),
      PacketLimits::default(),
    ),
  );
  // The failed write is not continued from (R20, ruled): the context is
  // failed, a later packet and the trailer refused by name, the failure
  // answered once.
  assert_eq!(output.phase(), OutputPhase::Failed);
  full.store(false, std::sync::atomic::Ordering::Relaxed);
  assert!(
    matches!(
      output.write_packet::<crate::Owned>(
        0,
        OutputPacket::Subtitle(&cue(b"\x00\x05later", 1000)),
        times(1000),
        PacketLimits::default(),
      ),
      Err(OutputError::FailedEarlier {
        call: OutputCall::WritePacket
      })
    ),
    "a packet after the failed one refused by name"
  );
  assert!(
    matches!(
      output.write_trailer(),
      Err(OutputError::FailedEarlier {
        call: OutputCall::WriteTrailer
      })
    ),
    "the trailer refused by name"
  );
  named("the sink handed back", output.into_sink().map(drop));

  // At the trailer: the header and every cue were written, the cues into
  // the cluster Matroska holds until it ends.
  let (mut output, full) = open("matroska", false);
  output.write_header().expect("the header");
  for pts in [0, 1000] {
    output
      .write_packet::<crate::Owned>(
        0,
        OutputPacket::Subtitle(&cue(b"a cue", pts)),
        times(pts),
        PacketLimits::default(),
      )
      .expect("a cue");
  }
  full.store(true, std::sync::atomic::Ordering::Relaxed);
  named("the trailer", output.write_trailer());
  assert_eq!(output.phase(), OutputPhase::Finished);
  named("the sink handed back", output.into_sink().map(drop));
}

/// What reached a [`Counted`] sink, kept outside it.
#[derive(Debug, Default)]
struct Tally {
  written: std::sync::Mutex<Vec<u8>>,
  flushes: std::sync::atomic::AtomicU32,
  drops: std::sync::atomic::AtomicU32,
}

/// A buffer that keeps a tally of what reaches it: the bytes written, its
/// flushes and its own drop.
#[derive(Debug, Default)]
struct Counted {
  bytes: Cursor<Vec<u8>>,
  tally: std::sync::Arc<Tally>,
}

impl Write for Counted {
  fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
    let written = self.bytes.write(buf)?;
    self
      .tally
      .written
      .lock()
      .expect("the tally")
      .extend_from_slice(&buf[..written]);
    Ok(written)
  }
  fn flush(&mut self) -> io::Result<()> {
    self
      .tally
      .flushes
      .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    Ok(())
  }
}

impl Seek for Counted {
  fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
    self.bytes.seek(pos)
  }
}

impl Drop for Counted {
  fn drop(&mut self) {
    self
      .tally
      .drops
      .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
  }
}

/// **A context dropped mid-file frees everything once, and writes nothing
/// it had not written.** Two SubRip streams, the header written and one
/// cue handed to the first — held in libavformat's interleaving queue, as
/// the second has none — then the context is dropped while writing, its
/// trailer never asked for. `avformat_free_context` runs the muxer's
/// `deinit` and frees the queued packet (`avformat.c` 148–200 in FFmpeg
/// 9.0.1); then the I/O context drops: what it still buffers goes through
/// the guard, the sink's flush runs once, and the sink is dropped once.
/// The sink holds the header and not the cue.
///
/// PLANT: a `Drop` without its `ManuallyDrop::drop` of the I/O context
/// leaves the sink alive (`drops` 0, `flushes` 0); a `Drop` that writes the
/// trailer before it frees writes the held cue.
#[test]
fn a_context_dropped_mid_file_frees_once_and_writes_nothing_it_held() {
  ffmpeg_next::init().expect("ffmpeg init");
  let ticket = subrip();
  let sink = Counted::default();
  let tally = std::sync::Arc::clone(&sink.tally);
  let mut output = Output::open(sink, "matroska").expect("open");
  let first = output
    .add_stream(&StreamSpec::new(&ticket, millis()))
    .expect("a stream");
  output
    .add_stream(&StreamSpec::new(&ticket, millis()))
    .expect("a second stream");
  output.write_header().expect("the header");
  let held = b"a cue the interleaving queue holds";
  output
    .write_packet::<crate::Owned>(
      first,
      OutputPacket::Subtitle(&cue(held, 0)),
      times(0),
      PacketLimits::default(),
    )
    .expect("a cue");
  assert_eq!(output.phase(), OutputPhase::Writing);
  assert_eq!(
    tally.drops.load(std::sync::atomic::Ordering::Relaxed),
    0,
    "the sink lives as long as the context",
  );

  drop(output);

  assert_eq!(
    tally.drops.load(std::sync::atomic::Ordering::Relaxed),
    1,
    "the sink is dropped once, with the context",
  );
  assert_eq!(
    tally.flushes.load(std::sync::atomic::Ordering::Relaxed),
    1,
    "and flushed once before it",
  );
  let written = tally.written.lock().expect("the tally");
  assert_eq!(
    &written[..4],
    &[0x1A, 0x45, 0xDF, 0xA3],
    "the header was written"
  );
  assert!(
    !written.windows(held.len()).any(|window| window == held),
    "the cue the queue held was freed, never written",
  );
}

/// A buffer that refuses to seek back before where it stands, as a pipe
/// or an append-only store does; it writes and seeks forward as a buffer
/// does.
#[derive(Debug, Default)]
struct ForwardOnly {
  bytes: Cursor<Vec<u8>>,
}

impl Write for ForwardOnly {
  fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
    self.bytes.write(buf)
  }
  fn flush(&mut self) -> io::Result<()> {
    Ok(())
  }
}

impl Seek for ForwardOnly {
  fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
    let here = self.bytes.position();
    let end = self.bytes.get_ref().len() as u64;
    let target = match pos {
      SeekFrom::Start(at) => Some(at),
      SeekFrom::Current(by) => here.checked_add_signed(by),
      SeekFrom::End(by) => end.checked_add_signed(by),
    };
    match target {
      Some(target) if target >= here => self.bytes.seek(SeekFrom::Start(target)),
      _ => Err(io::Error::from_raw_os_error(libc::ESPIPE)),
    }
  }
}

/// **A seek the sink refuses is named, and the container never ends in
/// `Ok`.** Forty cues of a kilobyte each — more than the 32 KiB the I/O
/// context buffers — go into MP4 and into Matroska, each of which goes
/// back at its trailer to patch what it wrote first (MP4 its `mdat` size,
/// without checking the seek: `movenc.c` 8997–8999 in FFmpeg 9.0.1;
/// Matroska its segment's size, 3398–3399), through a sink that refuses
/// every seek back. The guard keeps the failure and writes nothing after
/// it; the trailer and `into_sink` both name it.
///
/// PLANT: without the record, MP4's trailer answers `Ok` — its muxer
/// writes on past the failed seek — and Matroska's names libavformat's
/// error instead.
#[test]
fn a_seek_the_sink_refuses_is_named_and_never_ends_in_ok() {
  ffmpeg_next::init().expect("ffmpeg init");
  for (container, codec) in [("mp4", CodecId::MOV_TEXT), ("matroska", CodecId::SUBRIP)] {
    let ticket = ticket(ffmpeg_next::ffi::AVMediaType::AVMEDIA_TYPE_SUBTITLE, codec);
    let mut output = Output::open(ForwardOnly::default(), container).expect("open");
    let stream = output
      .add_stream(&StreamSpec::new(&ticket, millis()))
      .expect("a stream");
    output.write_header().expect("the header");
    assert_eq!(output.stream_time_base(stream).expect("a ruler"), millis());
    // A 16-bit length, then the text: how mov_text frames a cue, and
    // bytes SubRip carries as they are.
    let mut text = 1000u16.to_be_bytes().to_vec();
    text.extend([b'x'; 1000]);
    for index in 0..40 {
      let pts = index * 1000;
      let cue = SubtitlePacket::new(
        FfmpegBytes::copy_from_slice(&text),
        SubtitlePacketExtra::new(0),
      )
      .with_pts(Some(ms(pts)))
      .with_duration(Some(ms(500)));
      output
        .write_packet::<crate::Owned>(
          stream,
          OutputPacket::Subtitle(&cue),
          times(pts),
          PacketLimits::default(),
        )
        .unwrap_or_else(|error| panic!("{container}: cue {index}: {error}"));
    }
    let trailer = output.write_trailer();
    assert!(
      matches!(trailer, Err(OutputError::SinkSeek(_))),
      "{container}: the trailer names the failed seek: {trailer:?}",
    );
    match output.into_sink() {
      Err(OutputError::SinkSeek(error)) => {
        assert_eq!(error.raw_os_error(), Some(libc::ESPIPE), "{container}");
      }
      other => panic!(
        "{container}: expected the failed seek named, got {:?}",
        other.map(|sink| sink.bytes.into_inner().len())
      ),
    }
  }
}

/// A [`ForwardOnly`] buffer that keeps a [`Tally`] of what reaches it.
#[derive(Debug, Default)]
struct CountedForwardOnly {
  bytes: ForwardOnly,
  tally: std::sync::Arc<Tally>,
}

impl Write for CountedForwardOnly {
  fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
    self.bytes.write(buf)
  }
  fn flush(&mut self) -> io::Result<()> {
    self
      .tally
      .flushes
      .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    Ok(())
  }
}

impl Seek for CountedForwardOnly {
  fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
    self.bytes.seek(pos)
  }
}

impl Drop for CountedForwardOnly {
  fn drop(&mut self) {
    self
      .tally
      .drops
      .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
  }
}

/// **After a seek the sink failed, nothing reaches it again — its flush
/// included, in `into_sink` and in the context's drop alike.** MP4's trailer
/// goes back to patch its `mdat` size through a sink that refuses every
/// seek back (as above); then the sink is taken back, or the context is
/// dropped. Either way the sink is flushed never and dropped once: a flush
/// would commit what it buffered at a position the failure left unknown.
///
/// PLANT: a flush let through after the record reaches the sink once, on
/// each road.
#[test]
fn no_flush_reaches_the_sink_after_a_failed_seek() {
  ffmpeg_next::init().expect("ffmpeg init");
  let ticket = ticket(
    ffmpeg_next::ffi::AVMediaType::AVMEDIA_TYPE_SUBTITLE,
    CodecId::MOV_TEXT,
  );
  for hand_back in [true, false] {
    let sink = CountedForwardOnly::default();
    let tally = std::sync::Arc::clone(&sink.tally);
    let mut output = Output::open(sink, "mp4").expect("open");
    let stream = output
      .add_stream(&StreamSpec::new(&ticket, millis()))
      .expect("a stream");
    output.write_header().expect("the header");
    let mut text = 1000u16.to_be_bytes().to_vec();
    text.extend([b'x'; 1000]);
    for index in 0..40 {
      let pts = index * 1000;
      let cue = SubtitlePacket::new(
        FfmpegBytes::copy_from_slice(&text),
        SubtitlePacketExtra::new(0),
      )
      .with_pts(Some(ms(pts)))
      .with_duration(Some(ms(500)));
      output
        .write_packet::<crate::Owned>(
          stream,
          OutputPacket::Subtitle(&cue),
          times(pts),
          PacketLimits::default(),
        )
        .expect("a cue");
    }
    assert!(matches!(
      output.write_trailer(),
      Err(OutputError::SinkSeek(_))
    ));
    if hand_back {
      assert!(matches!(output.into_sink(), Err(OutputError::SinkSeek(_))));
    } else {
      drop(output);
    }
    let count = |of: &std::sync::atomic::AtomicU32| of.load(std::sync::atomic::Ordering::Relaxed);
    assert_eq!(
      count(&tally.flushes),
      0,
      "hand back {hand_back}: never flushed"
    );
    assert_eq!(
      count(&tally.drops),
      1,
      "hand back {hand_back}: dropped once"
    );
  }
}
