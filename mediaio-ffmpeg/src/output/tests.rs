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
  // R22, Codex R21 1: `Finished` is the trailer's success alone.
  assert_eq!(output.phase(), OutputPhase::Failed);
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

/// **An audio packet framed reaches libavformat behind its header.** A
/// raw PCM stream, whose muxer writes each packet as it is: a packet of
/// four bytes, behind a header of two, is written as the six — the header
/// first — one packet built of both, its padding the copy road's, the
/// caller's bytes untouched (R20, r3: MPEG-TS's raw AAC, which
/// `mediaencode-ffmpeg`'s muxer frames in ADTS itself).
#[test]
fn an_audio_packet_framed_reaches_libavformat_behind_its_header() {
  ffmpeg_next::init().expect("ffmpeg init");
  let mut parameters = ffmpeg_next::codec::Parameters::new();
  // SAFETY: `Parameters::new` allocated a live `AVCodecParameters` (it is
  // checked non-null first) that nothing else holds; the enums are written
  // as the integers they are stored as, the layout by libavutil's own.
  unsafe {
    let par = parameters.as_mut_ptr();
    assert!(!par.is_null(), "avcodec_parameters_alloc");
    write_unaligned(
      addr_of_mut!((*par).codec_type).cast::<i32>(),
      ffmpeg_next::ffi::AVMediaType::AVMEDIA_TYPE_AUDIO as i32,
    );
    write_unaligned(
      addr_of_mut!((*par).codec_id).cast::<i32>(),
      CodecId::PCM_S16LE.raw(),
    );
    (*par).sample_rate = 48_000;
    ffmpeg_next::ffi::av_channel_layout_default(addr_of_mut!((*par).ch_layout), 1);
  }
  let ticket = CodecTicket::mirror(&parameters, 0, 1 << 20).expect("a ticket");
  let mut output = Output::open(Cursor::new(Vec::new()), "s16le").expect("open");
  let stream = output
    .add_stream(&StreamSpec::new(&ticket, millis()))
    .expect("a stream");
  output.write_header().expect("the header");
  let packet = AudioPacket::new(
    FfmpegBytes::copy_from_slice(&[1, 2, 3, 4]),
    AudioPacketExtra::new(0),
  )
  .with_pts(Some(ms(0)))
  .with_dts(Some(ms(0)));
  output
    .write_packet::<crate::Owned>(
      stream,
      OutputPacket::AudioFramed {
        packet: &packet,
        header: &[0xAA, 0xBB],
      },
      times(0),
      PacketLimits::default(),
    )
    .expect("a framed packet");
  output.write_trailer().expect("the trailer");
  let bytes = output.into_sink().expect("the sink back").into_inner();
  assert_eq!(
    bytes,
    [0xAA, 0xBB, 1, 2, 3, 4],
    "the header, then the payload"
  );
  assert_eq!(
    packet.data().as_ref(),
    [1, 2, 3, 4],
    "the caller's bytes untouched"
  );
}

/// An AAC stream's ticket, at 44.1 kHz in stereo, of the configuration
/// `config`.
fn aac(config: &[u8]) -> CodecTicket {
  audio(CodecId::AAC, 44_100, config)
}

/// A stereo audio stream of `codec` at `rate` Hz, of the configuration
/// `config`.
fn audio(codec: CodecId, rate: i32, config: &[u8]) -> CodecTicket {
  let mut parameters = ffmpeg_next::codec::Parameters::new();
  // SAFETY: `Parameters::new` allocated a live `AVCodecParameters` (it is
  // checked non-null first) that nothing else holds; the enums are written
  // as the integers they are stored as, the layout by libavutil's own, and
  // the extradata into an allocation of libavutil's, zero-padded.
  unsafe {
    let par = parameters.as_mut_ptr();
    assert!(!par.is_null(), "avcodec_parameters_alloc");
    write_unaligned(
      addr_of_mut!((*par).codec_type).cast::<i32>(),
      ffmpeg_next::ffi::AVMediaType::AVMEDIA_TYPE_AUDIO as i32,
    );
    write_unaligned(addr_of_mut!((*par).codec_id).cast::<i32>(), codec.raw());
    (*par).sample_rate = rate;
    ffmpeg_next::ffi::av_channel_layout_default(addr_of_mut!((*par).ch_layout), 2);
    let extradata = av_mallocz(config.len() + PADDING).cast::<u8>();
    assert!(!extradata.is_null(), "av_mallocz");
    ptr::copy_nonoverlapping(config.as_ptr(), extradata, config.len());
    (*par).extradata = extradata;
    (*par).extradata_size = config.len() as i32;
  }
  CodecTicket::mirror(&parameters, 0, 1 << 20).expect("a ticket")
}

/// **A context whose call failed writes nothing more into its sink, by
/// either road out** (R21, Codex R20 3; r1, refined: fail closed). FLV
/// writes an AAC packet's new configuration as a sequence header, then
/// refuses the packet, its tag past 24 bits (`flv_write_packet`,
/// `flvenc.c` 1250–1263, 1346–1351 in FFmpeg 9.0.1): a failure that is not
/// the sink's, the header's bytes still in the I/O context's buffer. Then
/// the context is dropped, or handed back: no byte and no flush reaches
/// the sink after the failure — the buffered bytes discarded, the sink's
/// flush not called — and the sink holds what was written before it; its
/// hand-back answers the failure again, libavformat's error kept (R22,
/// Codex R21 1), the sink dropped once.
///
/// PLANT: `Drop` torn down open; `into_sink` torn down open; the guard's
/// closing word unread.
#[test]
fn a_failed_context_writes_nothing_more_into_its_sink() {
  ffmpeg_next::init().expect("ffmpeg init");
  let ticket = aac(&[0x12, 0x10]);
  for road in ["drop", "into_sink"] {
    let sink = Counted::default();
    let tally = std::sync::Arc::clone(&sink.tally);
    let mut output = Output::open(sink, "flv").expect("open");
    let stream = output
      .add_stream(&StreamSpec::new(&ticket, millis()))
      .expect("a stream");
    output.write_header().expect("the header");
    let packet = AudioPacket::new(
      FfmpegBytes::copy_from_slice(&vec![0; 1 << 24]),
      AudioPacketExtra::new(0).with_side_data(vec![crate::extras::SideDataEntry::new(
        ffmpeg_next::ffi::AVPacketSideDataType::AV_PKT_DATA_NEW_EXTRADATA as i32,
        FfmpegBytes::copy_from_slice(&[0x11, 0x90]),
      )]),
    )
    .with_pts(Some(ms(0)))
    .with_dts(Some(ms(0)));
    let failed = output.write_packet::<crate::Owned>(
      stream,
      OutputPacket::Audio(&packet),
      times(0),
      PacketLimits::default(),
    );
    let Err(OutputError::Ffmpeg(code)) = failed else {
      panic!("{road}: the premise, a failure not the sink's: {failed:?}");
    };
    assert_eq!(output.phase(), OutputPhase::Failed, "{road}");
    let (written, flushes) = (
      tally.written.lock().expect("the tally").len(),
      tally.flushes.load(std::sync::atomic::Ordering::Relaxed),
    );
    assert!(
      output.position() > written as i64,
      "{road}: the premise, the sequence header's bytes still buffered"
    );
    if road == "drop" {
      drop(output);
    } else {
      let handed = output.into_sink().map(drop);
      assert!(
        matches!(handed, Err(OutputError::Ffmpeg(kept)) if kept == code),
        "{road}: the hand-back answering the failure again: {handed:?}"
      );
    }
    assert_eq!(
      tally.written.lock().expect("the tally").len(),
      written,
      "{road}: no byte after the failure"
    );
    assert_eq!(
      tally.flushes.load(std::sync::atomic::Ordering::Relaxed),
      flushes,
      "{road}: no flush after the failure"
    );
    assert_eq!(
      tally.drops.load(std::sync::atomic::Ordering::Relaxed),
      1,
      "{road}: the sink dropped once"
    );
  }
}

/// A buffer that keeps a tally of what reaches it, as [`Counted`] does,
/// and whose writes fail as a full disk fails them once its switch is
/// thrown, as [`FillsUp`]'s do.
#[derive(Debug, Default)]
struct Tallied {
  bytes: Cursor<Vec<u8>>,
  tally: std::sync::Arc<Tally>,
  full: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl Write for Tallied {
  fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
    if self.full.load(std::sync::atomic::Ordering::Relaxed) {
      return Err(io::Error::from_raw_os_error(libc::ENOSPC));
    }
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

impl Seek for Tallied {
  fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
    self.bytes.seek(pos)
  }
}

impl Drop for Tallied {
  fn drop(&mut self) {
    self
      .tally
      .drops
      .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
  }
}

/// What has reached the sink a [`Tally`] counts: its bytes and its flushes.
fn reached(tally: &Tally) -> (usize, u32) {
  (
    tally.written.lock().expect("the tally").len(),
    tally.flushes.load(std::sync::atomic::Ordering::Relaxed),
  )
}

/// **A trailer that failed leaves the context failed, and nothing more
/// reaches the sink, by either road out** (R22, Codex R21 1: `Finished`
/// is the trailer's success alone). Two failures: the sink's — Matroska
/// writes the cluster it holds at its trailer, into a sink that has filled
/// up — and one not the sink's — MP4 writes an Opus track's `dOps` at its
/// trailer, and refuses a configuration shorter than an `OpusHead`'s 19
/// bytes (`mov_write_dops_tag`, `movenc.c` 877–887 in FFmpeg 9.0.1), its
/// start and its packet taken. Each trailer answers its failure; the
/// context is failed, a second trailer refused by name. Then it is
/// dropped, or handed back: no byte and no flush reach the sink after the
/// call — what libavformat's own trailer flushed within it, as it does
/// whatever the muxer answered (`mux.c` 1267–1272), counted before — the
/// hand-back answering the failure again, the sink's, else libavformat's
/// error kept; and the sink is dropped once.
///
/// PLANT: the trailer's phase `Finished` whatever it answered;
/// libavformat's error not kept, the hand-back refused instead.
#[test]
fn a_failed_trailer_ends_the_context_failed_and_writes_nothing_more() {
  ffmpeg_next::init().expect("ffmpeg init");
  let opus = audio(CodecId::OPUS, 48_000, &[0; 10]);
  for failure in ["the sink's", "libavformat's"] {
    for road in ["drop", "into_sink"] {
      let sink = Tallied::default();
      let (tally, full) = (
        std::sync::Arc::clone(&sink.tally),
        std::sync::Arc::clone(&sink.full),
      );
      let mut output = if failure == "the sink's" {
        let mut output = Output::open(sink, "matroska").expect("open");
        output
          .add_stream(&StreamSpec::new(&subrip(), millis()))
          .expect("a stream");
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
        output
      } else {
        let mut output = Output::open(sink, "mp4").expect("open");
        output
          .add_stream(&StreamSpec::new(&opus, millis()))
          .expect("a stream");
        output.write_header().expect("the header");
        let packet = AudioPacket::new(
          FfmpegBytes::copy_from_slice(&[0xF8, 0xFF, 0xFE]),
          AudioPacketExtra::new(0),
        )
        .with_pts(Some(ms(0)))
        .with_dts(Some(ms(0)));
        output
          .write_packet::<crate::Owned>(
            0,
            OutputPacket::Audio(&packet),
            PacketTimes::new(Some(0), Some(0), 960),
            PacketLimits::default(),
          )
          .expect("a packet");
        output
      };
      let answered = output.write_trailer();
      match (failure, &answered) {
        ("the sink's", Err(OutputError::SinkWrite(error))) => {
          assert_eq!(*error, enospc(), "{failure}, {road}");
        }
        ("libavformat's", Err(OutputError::Ffmpeg(error))) => {
          assert_eq!(*error, ffmpeg_next::Error::InvalidData, "{failure}, {road}")
        }
        _ => panic!("{failure}, {road}: the trailer's failure, got {answered:?}"),
      }
      assert_eq!(output.phase(), OutputPhase::Failed, "{failure}, {road}");
      assert!(
        matches!(
          output.write_trailer(),
          Err(OutputError::FailedEarlier {
            call: OutputCall::WriteTrailer
          })
        ),
        "{failure}, {road}: a second trailer refused by name"
      );
      let before = reached(&tally);
      if road == "drop" {
        drop(output);
      } else {
        let handed = output.into_sink().map(drop);
        match (failure, &handed) {
          ("the sink's", Err(OutputError::SinkWrite(error))) => {
            assert_eq!(*error, enospc(), "{failure}, {road}: the hand-back");
          }
          ("libavformat's", Err(OutputError::Ffmpeg(error))) => assert_eq!(
            *error,
            ffmpeg_next::Error::InvalidData,
            "{failure}, {road}: the hand-back, the error kept"
          ),
          _ => panic!("{failure}, {road}: the hand-back answering the failure, got {handed:?}"),
        }
      }
      assert_eq!(
        reached(&tally),
        before,
        "{failure}, {road}: no byte and no flush after the failed trailer"
      );
      assert_eq!(
        tally.drops.load(std::sync::atomic::Ordering::Relaxed),
        1,
        "{failure}, {road}: the sink dropped once"
      );
    }
  }
}

/// **A context dropped before its header calls nothing of its sink, and
/// one whose stream libavformat would not add is failed** (R22, Codex R21
/// 1). Before the header nothing was written, so a drop flushes nothing:
/// the guard closed as from a failure, the sink dropped once, its flush
/// never asked — a refusal made before libavformat, a language holding a
/// NUL, leaving the context as it was. Past the context's `max_streams`,
/// 1000 by default, libavformat adds no stream (`avformat_new_stream`,
/// `options.c` 252–257 in FFmpeg 9.0.1): the call answers `ENOMEM`, the
/// context is failed, a later stream and the header refused by name, and
/// the hand-back answers the failure again, nothing reaching the sink.
///
/// PLANT: `Drop` from `Building` torn down open; the stream libavformat
/// would not add leaving the context as it was.
#[test]
fn a_context_before_its_header_calls_nothing_of_its_sink() {
  ffmpeg_next::init().expect("ffmpeg init");
  let ticket = subrip();
  // Dropped while being built, after a refusal.
  let sink = Tallied::default();
  let tally = std::sync::Arc::clone(&sink.tally);
  let mut output = Output::open(sink, "matroska").expect("open");
  output
    .add_stream(&StreamSpec::new(&ticket, millis()))
    .expect("a stream");
  assert!(
    matches!(
      output.add_stream(&StreamSpec::new(&ticket, millis()).with_language(Some("e\0n"))),
      Err(OutputError::Nul {
        text: OutputText::Language
      })
    ),
    "a language holding a NUL refused by name"
  );
  assert_eq!(output.phase(), OutputPhase::Building, "refused, as it was");
  drop(output);
  assert_eq!(
    reached(&tally),
    (0, 0),
    "dropped while built: nothing asked"
  );
  assert_eq!(
    tally.drops.load(std::sync::atomic::Ordering::Relaxed),
    1,
    "the sink dropped once"
  );
  // Past `max_streams`: failed, by either road out.
  for road in ["drop", "into_sink"] {
    let sink = Tallied::default();
    let tally = std::sync::Arc::clone(&sink.tally);
    let mut output = Output::open(sink, "matroska").expect("open");
    for stream in 0..1000 {
      output
        .add_stream(&StreamSpec::new(&ticket, millis()))
        .unwrap_or_else(|error| panic!("{road}: stream {stream}: {error:?}"));
    }
    let refused = output.add_stream(&StreamSpec::new(&ticket, millis()));
    let Err(OutputError::Ffmpeg(code)) = refused else {
      panic!("{road}: the 1001st stream: {refused:?}");
    };
    assert_eq!(
      code,
      ffmpeg_next::Error::Other {
        errno: libc::ENOMEM
      },
      "{road}"
    );
    assert_eq!(output.phase(), OutputPhase::Failed, "{road}");
    assert!(
      matches!(
        output.write_header(),
        Err(OutputError::FailedEarlier {
          call: OutputCall::WriteHeader
        })
      ),
      "{road}: the header refused by name"
    );
    if road == "drop" {
      drop(output);
    } else {
      let handed = output.into_sink().map(drop);
      assert!(
        matches!(handed, Err(OutputError::Ffmpeg(kept)) if kept == code),
        "{road}: the hand-back answering the failure again: {handed:?}"
      );
    }
    assert_eq!(reached(&tally), (0, 0), "{road}: nothing asked of the sink");
    assert_eq!(
      tally.drops.load(std::sync::atomic::Ordering::Relaxed),
      1,
      "{road}: the sink dropped once"
    );
  }
}

/// **An abandoned context writes nothing more into its sink** (R22, Codex
/// R21 1): [`Output::abandon`], the road a caller whose own judgment ends
/// the output takes — a trailer it refuses. Two SubRip streams in
/// Matroska, the header written and a cue held in libavformat's
/// interleaving queue: abandoned, no byte and no flush reach the sink, and
/// the sink is dropped once; dropped instead, the sink's flush is asked.
///
/// PLANT: `abandon` leaving the phase as it was.
#[test]
fn an_abandoned_context_writes_nothing_more_into_its_sink() {
  ffmpeg_next::init().expect("ffmpeg init");
  let ticket = subrip();
  for road in ["abandon", "drop"] {
    let sink = Tallied::default();
    let tally = std::sync::Arc::clone(&sink.tally);
    let mut output = Output::open(sink, "matroska").expect("open");
    for _ in 0..2 {
      output
        .add_stream(&StreamSpec::new(&ticket, millis()))
        .expect("a stream");
    }
    output.write_header().expect("the header");
    output
      .write_packet::<crate::Owned>(
        0,
        OutputPacket::Subtitle(&cue(b"a held cue", 0)),
        times(0),
        PacketLimits::default(),
      )
      .expect("a cue, held");
    let (written, flushes) = reached(&tally);
    if road == "abandon" {
      output.abandon();
      assert_eq!(
        reached(&tally),
        (written, flushes),
        "{road}: no byte and no flush"
      );
    } else {
      drop(output);
      assert_eq!(
        reached(&tally),
        (written, flushes + 1),
        "{road}: the sink's flush asked once"
      );
    }
    assert_eq!(
      tally.drops.load(std::sync::atomic::Ordering::Relaxed),
      1,
      "{road}: the sink dropped once"
    );
  }
}

const ALLOCATION_CHILD: &str = "MEDIAIO_FFMPEG_OUTPUT_ALLOCATION_CHILD";

/// **An allocation's failure leaves the context as its road says, and
/// nothing reaches the sink after it** (R22, Codex R21 1). In a process of
/// its own, libavutil's allocations held below a size at each step
/// (`av_max_alloc`, `mem.c` 74–78 in FFmpeg 9.0.1):
/// - the I/O context's 32 KiB buffer refused: the open answers `ENOMEM`,
///   the sink dropped once, nothing asked of it;
/// - a stream's replaced extradata refused (`replace_extradata`), and its
///   language's copy (`av_dict_set`): the context failed, libavformat's
///   error kept and answered again at the hand-back, nothing asked of the
///   sink;
/// - a chapter's own allocation refused, and its title's copy: refused,
///   the context as it was, still being built; the chapters' array's
///   growth refused, at the fifth chapter (`av_dynarray_add_nofree`, each
///   growth a doubling): the context failed.
///
/// PLANT: the replaced extradata's failure leaving the context as it was;
/// the array's failure leaving it as it was.
#[test]
fn an_allocations_failure_leaves_the_context_as_its_road_says() {
  if std::env::var_os(ALLOCATION_CHILD).is_some() {
    return;
  }
  let status = std::process::Command::new(std::env::current_exe().expect("the lane"))
    .args([
      "--exact",
      "output::tests::allocation_failures_in_a_process_of_their_own",
      "--test-threads=1",
      "--nocapture",
    ])
    .env(ALLOCATION_CHILD, "1")
    .status()
    .expect("the process of its own");
  assert!(status.success(), "each road as the census says: {status}");
}

/// The body of [`an_allocations_failure_leaves_the_context_as_its_road_says`],
/// in a process of its own (its caller sets [`ALLOCATION_CHILD`]);
/// elsewhere nothing.
#[test]
fn allocation_failures_in_a_process_of_their_own() {
  if std::env::var_os(ALLOCATION_CHILD).is_none() {
    return;
  }
  ffmpeg_next::init().expect("ffmpeg init");
  let capped = |cap: usize| {
    // SAFETY: a store of the process's cap, which libavutil reads
    // atomically at every allocation; this process runs this law alone.
    unsafe { ffmpeg_next::ffi::av_max_alloc(cap) };
  };
  let uncapped = || {
    // SAFETY: as above: the cap back at libavutil's own, `INT_MAX`.
    unsafe { ffmpeg_next::ffi::av_max_alloc(i32::MAX as usize) };
  };
  let enomem = ffmpeg_next::Error::Other {
    errno: libc::ENOMEM,
  };
  let ticket = subrip();

  // The I/O context's buffer.
  let sink = Tallied::default();
  let tally = std::sync::Arc::clone(&sink.tally);
  capped(16 * 1024);
  let opened = Output::open(sink, "matroska").map(drop);
  uncapped();
  assert!(
    matches!(opened, Err(OutputError::Ffmpeg(code)) if code == enomem),
    "the buffer refused: {opened:?}"
  );
  assert_eq!(reached(&tally), (0, 0), "the buffer: nothing asked");
  assert_eq!(
    tally.drops.load(std::sync::atomic::Ordering::Relaxed),
    1,
    "the buffer: the sink dropped once"
  );

  // A stream's replaced extradata, and its language's copy.
  let large = vec![0x5A_u8; 300_000];
  let language = "x".repeat(300_000);
  for (what, spec) in [
    (
      "the extradata",
      StreamSpec::new(&ticket, millis()).with_extradata(Some(&large)),
    ),
    (
      "the language",
      StreamSpec::new(&ticket, millis()).with_language(Some(&language)),
    ),
  ] {
    for road in ["drop", "into_sink"] {
      let sink = Tallied::default();
      let tally = std::sync::Arc::clone(&sink.tally);
      let mut output = Output::open(sink, "matroska").expect("open");
      capped(200_000);
      let added = output.add_stream(&spec);
      uncapped();
      let Err(OutputError::Ffmpeg(code)) = added else {
        panic!("{what}, {road}: refused, {added:?}");
      };
      assert_eq!(code, enomem, "{what}, {road}");
      assert_eq!(output.phase(), OutputPhase::Failed, "{what}, {road}");
      if road == "drop" {
        drop(output);
      } else {
        let handed = output.into_sink().map(drop);
        assert!(
          matches!(handed, Err(OutputError::Ffmpeg(kept)) if kept == enomem),
          "{what}, {road}: the hand-back answering the failure again: {handed:?}"
        );
      }
      assert_eq!(reached(&tally), (0, 0), "{what}, {road}: nothing asked");
    }
  }

  // A chapter's own allocation, its title's copy, and the array's growth.
  let sink = Tallied::default();
  let tally = std::sync::Arc::clone(&sink.tally);
  let mut output = Output::open(sink, "matroska").expect("open");
  output
    .add_stream(&StreamSpec::new(&ticket, millis()))
    .expect("a stream");
  capped(39);
  let own = output.add_chapter(0, millis(), 0, 1000, None);
  uncapped();
  assert!(
    matches!(own, Err(OutputError::Ffmpeg(code)) if code == enomem),
    "the chapter's own: {own:?}"
  );
  assert_eq!(output.phase(), OutputPhase::Building, "the chapter's own");
  let title = "t".repeat(100);
  capped(40);
  let titled = output.add_chapter(0, millis(), 0, 1000, Some(&title));
  assert!(
    matches!(titled, Err(OutputError::Ffmpeg(code)) if code == enomem),
    "the title's copy: {titled:?}"
  );
  assert_eq!(output.phase(), OutputPhase::Building, "the title's copy");
  for chapter in 0..4 {
    let added = output.add_chapter(chapter, millis(), 0, 1000, None);
    assert!(added.is_ok(), "chapter {chapter}: {added:?}");
  }
  let fifth = output.add_chapter(4, millis(), 0, 1000, None);
  uncapped();
  assert!(
    matches!(fifth, Err(OutputError::Ffmpeg(code)) if code == enomem),
    "the array's growth: {fifth:?}"
  );
  assert_eq!(output.phase(), OutputPhase::Failed, "the array's growth");
  drop(output);
  assert_eq!(reached(&tally), (0, 0), "the chapters: nothing asked");
}

/// A tap that keeps every run it is handed, and counts the losses of its
/// position.
#[derive(Debug, Default)]
struct Recorder {
  runs: Vec<(u64, Vec<u8>)>,
  lost: u32,
}

impl SinkTap for Recorder {
  fn took(&mut self, at: u64, bytes: &[u8]) {
    self.runs.push((at, bytes.to_vec()));
  }
  fn lost(&mut self) {
    self.lost += 1;
  }
}

/// A tap of another type than [`Recorder`].
struct Other;

impl SinkTap for Other {
  fn took(&mut self, _at: u64, _bytes: &[u8]) {}
  fn lost(&mut self) {}
}

/// The file `runs` make, replayed in order over a blank buffer.
fn replayed(runs: &[(u64, Vec<u8>)]) -> Vec<u8> {
  let mut file = Vec::new();
  for (at, bytes) in runs {
    let at = usize::try_from(*at).expect("an offset in memory");
    let end = at + bytes.len();
    if file.len() < end {
      file.resize(end, 0);
    }
    file[at..end].copy_from_slice(bytes);
  }
  file
}

/// A buffer that counts the writes it takes below its end: the patches
/// that reached it on their own.
#[derive(Default)]
struct Patched {
  bytes: Cursor<Vec<u8>>,
  patches: u32,
}

impl Write for Patched {
  fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
    if self.bytes.position() < self.bytes.get_ref().len() as u64 {
      self.patches += 1;
    }
    self.bytes.write(buf)
  }
  fn flush(&mut self) -> io::Result<()> {
    Ok(())
  }
}

impl Seek for Patched {
  fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
    self.bytes.seek(pos)
  }
}

/// The bytes `runs` carry, counted.
fn carried(runs: &[(u64, Vec<u8>)]) -> u64 {
  runs.iter().map(|(_, bytes)| bytes.len() as u64).sum()
}

/// Where two files first differ, if they do.
fn first_difference(a: &[u8], b: &[u8]) -> Option<usize> {
  a.iter()
    .zip(b)
    .position(|(x, y)| x != y)
    .or_else(|| (a.len() != b.len()).then(|| a.len().min(b.len())))
}

/// MP4 over `sink`, one AAC track and `tap` if any: the header, `packets`
/// packets of 4 and 5 bytes in turn — sizes that differ, so that the
/// sample-size table lists each, in 4 bytes: at 10 000 the `moov` passes
/// the I/O context's 32 KiB, and the sizes of its boxes are patched after
/// their first bytes reached the sink — and the trailer. Answers the output
/// and the first failure, if any.
fn mp4_of<W: Write + Seek + Send + 'static>(
  sink: W,
  packets: usize,
  tap: Option<impl SinkTap>,
) -> (Output<W>, Result<(), OutputError>) {
  let ticket = aac(&[0x12, 0x10]);
  let mut output = Output::open(sink, "mp4").expect("open");
  output
    .add_stream(&StreamSpec::new(&ticket, millis()))
    .expect("a stream");
  if let Some(tap) = tap {
    output.set_tap(tap).expect("the tap");
  }
  let written = (|| {
    output.write_header()?;
    for i in 0..packets {
      let packet = AudioPacket::new(
        FfmpegBytes::copy_from_slice(&[0x21; 5][..4 + i % 2]),
        AudioPacketExtra::new(0),
      );
      let at = i as i64 * 20;
      output.write_packet::<crate::Owned>(
        0,
        OutputPacket::Audio(&packet),
        PacketTimes::new(Some(at), Some(at), 20),
        PacketLimits::default(),
      )?;
    }
    output.write_trailer()
  })();
  (output, written)
}

/// **B1 — every byte the sink takes reaches the tap, at its offset.** A
/// recording tap over MP4 whose `moov` passes the I/O context's 32 KiB, so
/// that the sizes of its boxes — and the `mdat`'s, always, the header
/// flushing its placeholder (`movenc.c` 8888 in FFmpeg 9.0.1) — reach the
/// sink as patches of their own, below what it holds: replayed in order
/// over a blank buffer, the runs the tap saw make the sink's final bytes
/// exactly. The tap is whole when the trailer answers (`mux.c` 1270): the
/// hand-back adds no byte.
///
/// PLANT P608: a seek does not move the tap's position.
#[test]
fn b1_every_byte_the_sink_takes_reaches_the_tap_at_its_offset() {
  ffmpeg_next::init().expect("ffmpeg init");
  let (mut output, written) = mp4_of(Patched::default(), 10_000, Some(Recorder::default()));
  written.expect("the file");
  let (runs, lost) = output
    .with_tap(|tap: &mut Recorder| (core::mem::take(&mut tap.runs), tap.lost))
    .expect("the tap, as its own type");
  let sink = output.into_sink().expect("the sink back");
  assert!(
    sink.patches >= 2,
    "the premise: the mdat's size and the moov's boxes' patched on their own ({})",
    sink.patches
  );
  let bytes = sink.bytes.into_inner();
  assert_eq!(lost, 0, "the position never lost");
  let replay = replayed(&runs);
  assert!(
    replay == bytes,
    "the replay is the sink's file: {} and {} bytes, first apart at {:?}",
    replay.len(),
    bytes.len(),
    first_difference(&replay, &bytes)
  );
}

/// A sink taking at most 7 bytes of each write, as a pipe may, and
/// counting what it took.
#[derive(Default)]
struct Sips {
  bytes: Cursor<Vec<u8>>,
  taken: u64,
}

impl Write for Sips {
  fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
    let taken = self.bytes.write(&buf[..buf.len().min(7)])?;
    self.taken += taken as u64;
    Ok(taken)
  }
  fn flush(&mut self) -> io::Result<()> {
    Ok(())
  }
}

impl Seek for Sips {
  fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
    self.bytes.seek(pos)
  }
}

/// **B2 — a partial write is tapped as it was taken.** The file of
/// [B1](b1_every_byte_the_sink_takes_reaches_the_tap_at_its_offset) into a
/// sink taking at most 7 bytes of each write: ffmpeg-next asks again for
/// the rest (`stream_io.rs` 333–343), and the tap sees each part at its own
/// offset — no run longer than 7 bytes, the runs carrying exactly the bytes
/// the sink took, and their replay the sink's file.
///
/// PLANT P605: the guard taps the bytes it was handed, not those taken.
#[test]
fn b2_a_partial_write_is_tapped_as_the_sink_took_it() {
  ffmpeg_next::init().expect("ffmpeg init");
  let (mut output, written) = mp4_of(Sips::default(), 10_000, Some(Recorder::default()));
  written.expect("the file");
  let runs = output
    .with_tap(|tap: &mut Recorder| core::mem::take(&mut tap.runs))
    .expect("the tap");
  let sink = output.into_sink().expect("the sink back");
  let longest = runs.iter().map(|(_, bytes)| bytes.len()).max();
  assert!(
    longest <= Some(7),
    "no run past what the sink took: {longest:?}"
  );
  assert_eq!(
    carried(&runs),
    sink.taken,
    "the bytes the sink took, no more"
  );
  let bytes = sink.bytes.into_inner();
  let replay = replayed(&runs);
  assert!(
    replay == bytes,
    "the replay is the sink's file: first apart at {:?}",
    first_difference(&replay, &bytes)
  );
}

/// A sink whose `fail_at`-th write fails as a full disk fails it, keeping
/// its bytes, and its count of them, where the law reads them once the
/// output has dropped it.
struct FailsAt {
  bytes: std::sync::Arc<std::sync::Mutex<(Cursor<Vec<u8>>, u64)>>,
  writes: u32,
  fail_at: u32,
}

impl Write for FailsAt {
  fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
    self.writes += 1;
    if self.writes == self.fail_at {
      return Err(io::Error::from_raw_os_error(libc::ENOSPC));
    }
    let (bytes, taken) = &mut *self.bytes.lock().expect("the bytes");
    let written = bytes.write(buf)?;
    *taken += written as u64;
    Ok(written)
  }
  fn flush(&mut self) -> io::Result<()> {
    Ok(())
  }
}

impl Seek for FailsAt {
  fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
    self.bytes.lock().expect("the bytes").0.seek(pos)
  }
}

/// **B3 — bytes the sink refused are not tapped.** The file of
/// [B1](b1_every_byte_the_sink_takes_reaches_the_tap_at_its_offset) into a
/// sink failing its 40th write: the output fails as the sink's write, and
/// the tap ends at the sink's last byte taken — the runs carry exactly the
/// bytes the sink took, and their replay is what it holds.
///
/// PLANT P606: the tap called before the sink answered, R27's latch
/// unread.
#[test]
fn b3_bytes_the_sink_refused_are_not_tapped() {
  ffmpeg_next::init().expect("ffmpeg init");
  let bytes = std::sync::Arc::new(std::sync::Mutex::new((Cursor::new(Vec::new()), 0)));
  let sink = FailsAt {
    bytes: std::sync::Arc::clone(&bytes),
    writes: 0,
    fail_at: 40,
  };
  let (mut output, written) = mp4_of(sink, 10_000, Some(Recorder::default()));
  assert!(
    matches!(written, Err(OutputError::SinkWrite(_))),
    "the premise, the sink's write failed: {written:?}"
  );
  let runs = output
    .with_tap(|tap: &mut Recorder| core::mem::take(&mut tap.runs))
    .expect("the tap, after the failure");
  drop(output);
  let sink = bytes.lock().expect("the bytes");
  let (held, taken) = &*sink;
  assert_eq!(carried(&runs), *taken, "the bytes the sink took, no more");
  let replay = replayed(&runs);
  assert!(
    replay.as_slice() == held.get_ref().as_slice(),
    "the replay is what the sink holds: first apart at {:?}",
    first_difference(&replay, held.get_ref())
  );
}

/// A tap that panics at its `at`-th call, counting every call that
/// reaches it.
struct PanicsAt {
  calls: std::sync::Arc<std::sync::atomic::AtomicU32>,
  at: u32,
}

impl SinkTap for PanicsAt {
  fn took(&mut self, _at: u64, _bytes: &[u8]) {
    let call = self
      .calls
      .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
      + 1;
    if call == self.at {
      panic!("the tap exploded");
    }
  }
  fn lost(&mut self) {}
}

/// **B4 — a tap that panics is contained, and not called again.** In a
/// process of its own, so that a panic past the guard aborts it rather than
/// the lane: a tap panicking at its 3rd call, inside libavformat's write
/// callback. The panic stays in the guard: the file is written whole —
/// byte for byte the file written untapped — the tap is called no more,
/// [`Output::with_tap`] answers it poisoned by name, and the sink's latch
/// is clear: the sink did not panic.
///
/// PLANT P607: the tap's `catch_unwind` removed (the child aborts).
#[test]
fn b4_a_tap_that_panics_is_contained_and_not_called_again() {
  crate::fault_subprocess::in_subprocess(
    "output::tests::b4_a_tap_that_panics_is_contained_and_not_called_again",
    || {
      ffmpeg_next::init().expect("ffmpeg init");
      let (untapped, written) = mp4_of(Cursor::new(Vec::new()), 50, None::<Recorder>);
      written.expect("the untapped file");
      let whole = untapped.into_sink().expect("the sink back").into_inner();
      let calls = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
      let tap = PanicsAt {
        calls: std::sync::Arc::clone(&calls),
        at: 3,
      };
      let (mut output, written) = quietly(|| mp4_of(Cursor::new(Vec::new()), 50, Some(tap)));
      written.expect("the file, whole");
      assert_eq!(
        calls.load(std::sync::atomic::Ordering::Relaxed),
        3,
        "not called again after its panic"
      );
      assert!(
        matches!(
          output.with_tap(|_: &mut PanicsAt| ()),
          Err(OutputError::TapPanicked)
        ),
        "the poisoned tap answered by name"
      );
      assert!(output.sink_panic().is_none(), "the sink's latch is clear");
      let bytes = output.into_sink().expect("the sink back").into_inner();
      assert!(
        bytes == whole,
        "the file written whole: first apart at {:?}",
        first_difference(&bytes, &whole)
      );
    },
  );
}

/// **B5 — the tap is set once, while the context is being built, and lent
/// back by its type.** No tap is answered by name; a second tap while
/// building, and a tap after the header, are refused as out of order, and a
/// tap after a failed call as after a failure; a tap of another type than
/// the one asked is answered by name, and the one set is lent as its own
/// type, holding the header's bytes.
#[test]
fn b5_the_tap_is_set_once_while_building_and_lent_by_its_type() {
  ffmpeg_next::init().expect("ffmpeg init");
  let ticket = subrip();
  let mut output = Output::open(Cursor::new(Vec::new()), "matroska").expect("open");
  output
    .add_stream(&StreamSpec::new(&ticket, millis()))
    .expect("a stream");
  assert!(
    matches!(
      output.with_tap(|_: &mut Recorder| ()),
      Err(OutputError::NoTap)
    ),
    "no tap yet"
  );
  output.set_tap(Recorder::default()).expect("the tap");
  assert!(
    matches!(
      output.set_tap(Recorder::default()),
      Err(OutputError::OutOfOrder {
        call: OutputCall::Tap,
        phase: OutputPhase::Building
      })
    ),
    "once"
  );
  assert!(
    matches!(
      output.with_tap(|_: &mut Other| ()),
      Err(OutputError::TapMismatch)
    ),
    "another type"
  );
  output.write_header().expect("the header");
  assert!(
    matches!(
      output.set_tap(Recorder::default()),
      Err(OutputError::OutOfOrder {
        call: OutputCall::Tap,
        phase: OutputPhase::Writing
      })
    ),
    "after the header"
  );
  let header = output
    .with_tap(|tap: &mut Recorder| carried(&tap.runs))
    .expect("lent as its own type");
  assert!(header > 0, "the header's bytes tapped");

  // SubRip, which MP4 has no tag for: the header refused.
  let mut failed = Output::open(Cursor::new(Vec::new()), "mp4").expect("open");
  failed
    .add_stream(&StreamSpec::new(&ticket, millis()))
    .expect("a stream");
  assert!(
    failed.write_header().is_err(),
    "the premise, a failed header"
  );
  assert!(
    matches!(
      failed.set_tap(Recorder::default()),
      Err(OutputError::FailedEarlier {
        call: OutputCall::Tap
      })
    ),
    "after a failure"
  );
}

/// **The write hook runs around libavformat's take of each packet** (test
/// support; the write audit's design gate, condition 5): with `true` just
/// before `av_interleaved_write_frame`, the packet built, and with `false`
/// as it answers — three packets, three pairs, and nothing at the header
/// or the trailer.
#[test]
fn the_write_hook_runs_around_each_packets_take() {
  let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
  let ticket = aac(&[0x12, 0x10]);
  let mut output = Output::open(Cursor::new(Vec::new()), "mp4").expect("open");
  output
    .add_stream(&StreamSpec::new(&ticket, millis()))
    .expect("a stream");
  let seen = std::sync::Arc::clone(&calls);
  output.set_write_hook(move |before| seen.lock().expect("the calls").push(before));
  output.write_header().expect("the header");
  for i in 0..3_i64 {
    let packet = AudioPacket::new(
      FfmpegBytes::copy_from_slice(&[0x21; 4]),
      AudioPacketExtra::new(0),
    );
    output
      .write_packet::<crate::Owned>(
        0,
        OutputPacket::Audio(&packet),
        PacketTimes::new(Some(i * 20), Some(i * 20), 20),
        PacketLimits::default(),
      )
      .expect("a packet");
  }
  output.write_trailer().expect("the trailer");
  assert_eq!(
    *calls.lock().expect("the calls"),
    vec![true, false, true, false, true, false]
  );
}
