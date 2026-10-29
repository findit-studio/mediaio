//! End-to-end audio decoder coverage against every clip in the
//! [`audio-fixtures`][1] submodule. Demuxes each WAV via FFmpeg,
//! drives `FfmpegAudioStreamDecoder` through the trait surface, and
//! asserts the recovered sample stream matches the expected
//! `(sample_rate, channels, samples)` triple from upstream's
//! `manifest.json`.
//!
//! Two codec directories are exercised — `pcm_s16le/` and
//! `pcm_f32le/`. The fixture set is mostly the former; the f32le
//! sub-corpus exists because one upstream clip
//! (`01_dialogue.wav`) ships at 32-bit float depth even though
//! the rest are 16-bit. Both groups are mono 16 kHz; only the
//! sample format and per-clip sample count vary, and both are
//! checked exactly.
//!
//! When the submodule isn't initialized the test returns early
//! with a hint instead of failing — keeps `cargo test` welcoming
//! for contributors who haven't run `git submodule update --init`
//! yet, while CI (always-`submodules: recursive`) exercises the
//! full sweep. The guard asks about a **fixture file**, never
//! about the directory: an uninitialised submodule still has its
//! mount point, so a directory test answers "present" for a tree
//! that holds nothing. See [`probe_fixture`].
//!
//! [1]: https://github.com/findit-studio/audio-fixtures

use std::{num::NonZeroI32, path::PathBuf};

use ffmpeg_next as ffmpeg;
use mediadecode::{Received, Sent, Timebase, decoder::AudioStreamDecoder};
// The owned family under the names this suite was written with — the
// bare aliases mean the view lane now. Import block only; the
// assertions below are unchanged.
use mediadecode_ffmpeg::{
  FfmpegOwnedAudioStreamDecoder as FfmpegAudioStreamDecoder,
  empty_owned_audio_frame as empty_audio_frame,
};

/// One row per fixture: `(directory, file, sample_rate, channels,
/// expected_samples)`. Kept hard-coded rather than parsed at runtime
/// so a change in `audio-fixtures/manifest.json` (a new file, a
/// re-trim, a re-encode) trips the assertion path rather than
/// silently passing. Update both sides when adding a fixture.
const FIXTURES: &[(&str, &str, u32, u8, u64)] = &[
  // --- pcm_s16le/ ---
  ("pcm_s16le", "02_pyannote_sample.wav", 16_000, 1, 480_000),
  ("pcm_s16le", "03_dual_speaker.wav", 16_000, 1, 960_000),
  ("pcm_s16le", "04_three_speaker.wav", 16_000, 1, 639_573),
  ("pcm_s16le", "05_four_speaker.wav", 16_000, 1, 960_000),
  ("pcm_s16le", "06_long_recording.wav", 16_000, 1, 15_643_627),
  (
    "pcm_s16le",
    "07_yuhewei_dongbei_english.wav",
    16_000,
    1,
    404_213,
  ),
  (
    "pcm_s16le",
    "08_luyu_jinjing_freedom.wav",
    16_000,
    1,
    22_675_308,
  ),
  (
    "pcm_s16le",
    "09_mrbeast_dollar_date.wav",
    16_000,
    1,
    16_671_744,
  ),
  (
    "pcm_s16le",
    "10_mrbeast_clean_water.wav",
    16_000,
    1,
    9_911_979,
  ),
  (
    "pcm_s16le",
    "11_mrbeast_age_race.wav",
    16_000,
    1,
    22_568_310,
  ),
  ("pcm_s16le", "12_mrbeast_schools.wav", 16_000, 1, 15_426_781),
  (
    "pcm_s16le",
    "13_mrbeast_saved_animals.wav",
    16_000,
    1,
    16_882_005,
  ),
  (
    "pcm_s16le",
    "14_mrbeast_strongman_robot.wav",
    16_000,
    1,
    17_648_640,
  ),
  // --- pcm_f32le/ ---
  ("pcm_f32le", "01_dialogue.wav", 16_000, 1, 3_631_361),
];

fn fixtures_root() -> PathBuf {
  PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    .parent()
    .expect("workspace root")
    .join("tests/fixtures/audio")
}

/// A clip this suite is about to read, used as the availability probe.
///
/// **Not `fixtures_root().exists()`.** `git clone` without `--recursive`
/// leaves `tests/fixtures/audio` as an *empty directory* — the submodule
/// mount point is created either way — so a directory test passes on a
/// tree that holds no fixtures at all, and the sweep below then panicked
/// on the first `ffmpeg::format::input` instead of skipping (issue #34).
/// A skip guard that cannot tell "not fetched" from "fetched" is not a
/// guard.
///
/// The probe is the **first row of [`FIXTURES`]** rather than a path
/// written out again here, so the file the guard asks about is by
/// construction one the sweep really opens: a fixture renamed upstream
/// moves both at once, where a second hard-coded path would rot into a
/// guard that skips a corpus that is present, or admits one that is not.
fn probe_fixture() -> PathBuf {
  let (codec_dir, name, ..) = FIXTURES.first().expect("the fixture table is not empty");
  fixtures_root().join(codec_dir).join(name)
}

fn decode_clip(path: &std::path::Path, expected: (u32, u8, u64)) {
  let (expected_sample_rate, expected_channels, expected_samples) = expected;

  let mut input = ffmpeg::format::input(&path).expect("open input");
  let stream = input
    .streams()
    .best(ffmpeg::media::Type::Audio)
    .expect("audio stream");
  let stream_index = stream.index();
  let stream_tb = stream.time_base();
  let time_base = Timebase::new(
    stream_tb.numerator(),
    NonZeroI32::new(stream_tb.denominator().max(1)).expect("non-zero den"),
  );

  let mut decoder = FfmpegAudioStreamDecoder::open(
    stream.parameters(),
    time_base,
    mediadecode_ffmpeg::DecoderLimits::default(),
  )
  .expect("open audio decoder");

  let mut frame = empty_audio_frame();
  let mut total_samples: u64 = 0;
  let mut frame_count: u64 = 0;
  let mut observed_sample_rate: Option<u32> = None;
  let mut observed_channels: Option<u8> = None;

  for (s, av_packet) in input.packets() {
    if s.index() != stream_index {
      continue;
    }
    // SAFETY: `packets()` reads each packet with `av_read_frame`, so libavformat wrote its
    // payload, and it is live for the call.
    let Some(pkt) = unsafe {
      mediadecode_ffmpeg::boundary::owned_audio_packet_from_ffmpeg_in(
        &av_packet,
        mediadecode::Timebase::default(),
        mediadecode_ffmpeg::PacketLimits::default(),
      )
    }
    .expect("a wrappable payload") else {
      continue;
    };
    assert_eq!(
      decoder.send_packet(&pkt).expect("audio send_packet"),
      Sent::Accepted,
      "these fixtures feed a session this loop has just drained",
    );
    while matches!(
      decoder
        .receive_frame(&mut frame)
        .expect("audio receive_frame"),
      Received::Produced
    ) {
      total_samples = total_samples.saturating_add(frame.nb_samples() as u64);
      frame_count = frame_count.saturating_add(1);
      observed_sample_rate.get_or_insert(frame.sample_rate());
      observed_channels.get_or_insert(frame.channel_count());
    }
  }
  assert_eq!(decoder.send_eof().expect("send_eof"), Sent::Accepted);
  // The tail, and it has an end that says so.
  loop {
    match decoder
      .receive_frame(&mut frame)
      .expect("audio receive_frame")
    {
      Received::Produced => {
        total_samples = total_samples.saturating_add(frame.nb_samples() as u64);
        frame_count = frame_count.saturating_add(1);
      }
      Received::NeedsInput => panic!("a decoder at EOF asked for input"),
      Received::Ended => break,
    }
  }

  assert!(frame_count > 0, "no audio frames decoded for {path:?}");
  assert_eq!(
    observed_sample_rate,
    Some(expected_sample_rate),
    "sample rate drift on {path:?}",
  );
  assert_eq!(
    observed_channels,
    Some(expected_channels),
    "channel count drift on {path:?}",
  );
  assert_eq!(
    total_samples,
    expected_samples,
    "sample count drift on {} ({total_samples} got, {expected_samples} expected)",
    path.file_name().unwrap_or_default().to_string_lossy(),
  );
}

#[test]
fn decode_all_audio_fixtures() {
  let probe = probe_fixture();
  if !probe.is_file() {
    eprintln!(
      "skip: {} not found — run `git submodule update --init --depth=1` \
       to fetch the audio-fixtures submodule, then re-run this test.",
      probe.display()
    );
    return;
  }

  let root = fixtures_root();
  ffmpeg::init().expect("ffmpeg init");

  for (codec_dir, name, sample_rate, channels, samples) in FIXTURES {
    let path = root.join(codec_dir).join(name);
    eprintln!("decoding {codec_dir}/{name}…");
    decode_clip(&path, (*sample_rate, *channels, *samples));
  }
  eprintln!(
    "decoded {} fixtures end-to-end through the trait surface",
    FIXTURES.len(),
  );
}

/// **The skip guard's question, asked of the state that used to defeat
/// it** (issue #34).
///
/// A `git clone` without `--recursive` leaves the submodule's mount
/// point as an empty directory, so `root.exists()` — the guard this
/// suite shipped with — answered *present* and the sweep then panicked
/// opening a file that was never fetched, on exactly the fresh worktree
/// the skip exists to welcome.
///
/// The two assertions are the whole finding: the directory question says
/// yes and the fixture question says no, on one tree. Built here rather
/// than measured against the real checkout, because the bug's state is
/// *the corpus absent* and a suite that has it cannot reproduce that.
#[test]
fn an_empty_submodule_directory_does_not_read_as_a_present_corpus() {
  let scratch = tempfile::tempdir().expect("a temp dir");
  let root = scratch.path().join("audio");
  std::fs::create_dir(&root).expect("the submodule mount point");

  assert!(
    root.exists(),
    "an uninitialised submodule leaves the directory, which is why a directory test cannot \
     be the guard",
  );

  let (codec_dir, name, ..) = FIXTURES.first().expect("the fixture table is not empty");
  assert!(
    !root.join(codec_dir).join(name).is_file(),
    "the guard asks about a clip the sweep opens, and an empty mount point has none",
  );
}

/// **The uniform contract, and the one asymmetry it makes visible
/// instead of hiding.**
///
/// All three decoder faces answer [`Sent`] now, which is the point: a
/// consumer generic over the traits reads one protocol whichever
/// backend it holds. What differs between them is *behaviour*, not
/// shape — and the audio road's difference is that it never answers
/// [`Sent::MustDrain`] for a parked frame.
///
/// That is deliberate and documented at the flag: the video decoder has
/// two scratches and can change which is current mid-fallback, so a
/// submission under a park would leave the retry reading the other one;
/// this decoder has a single scratch, so a submission under a park
/// costs nothing. Before the reform the asymmetry was invisible at the
/// trait tier — video and subtitle raised a `FramePending` arm that the
/// audio error type simply did not have, so a generic consumer could
/// not have discovered either fact. Now it is one vocabulary with one
/// backend answering it differently, which is a property a test can
/// pin.
#[test]
fn the_audio_face_answers_the_same_vocabulary_without_a_park_refusal() {
  let path = fixtures_root().join("pcm_s16le/02_pyannote_sample.wav");
  if !path.exists() {
    eprintln!("skipping: run `git submodule update --init` for {path:?}");
    return;
  }
  ffmpeg::init().expect("ffmpeg init");

  let mut input = ffmpeg::format::input(&path).expect("open input");
  let stream = input
    .streams()
    .best(ffmpeg::media::Type::Audio)
    .expect("audio stream");
  let stream_index = stream.index();
  let mut decoder = FfmpegAudioStreamDecoder::open(
    stream.parameters(),
    Timebase::default(),
    mediadecode_ffmpeg::DecoderLimits::default(),
  )
  .expect("open audio decoder");

  let mut packets = input
    .packets()
    .filter_map(|(s, p)| (s.index() == stream_index).then_some(p));
  let mut take = || {
    let av = packets.next().expect("the fixture has packets");
    // SAFETY: `packets()` reads each packet with `av_read_frame`, so libavformat wrote its
    // payload, and it is live for the call.
    unsafe {
      mediadecode_ffmpeg::boundary::owned_audio_packet_from_ffmpeg_in(
        &av,
        mediadecode::Timebase::default(),
        mediadecode_ffmpeg::PacketLimits::default(),
      )
    }
    .expect("a wrappable payload")
    .expect("packet has a buffer")
  };

  // Two packets, back to back, with **no drain between them**. The
  // first fills the scratch slot; on the video or subtitle road the
  // second would be told to drain. Here it is simply taken.
  assert_eq!(
    decoder.send_packet(&take()).expect("no fault"),
    Sent::Accepted,
  );
  assert_eq!(
    decoder.send_packet(&take()).expect("no fault"),
    Sent::Accepted,
    "the audio road has one scratch, so a send under a park loses nothing",
  );

  // And the rhythm still works: frames come out, and the end says so.
  let mut frame = empty_audio_frame();
  let mut delivered = 0u32;
  while matches!(
    decoder.receive_frame(&mut frame).expect("no fault"),
    Received::Produced
  ) {
    delivered += 1;
  }
  assert!(delivered > 0, "the two packets produced no frame at all");

  assert_eq!(decoder.send_eof().expect("no fault"), Sent::Accepted);
  loop {
    match decoder.receive_frame(&mut frame).expect("no fault") {
      Received::Produced => {}
      Received::NeedsInput => panic!("a decoder at EOF asked for input"),
      Received::Ended => break,
    }
  }
}

/// **Class audit: the substrate's own post-EOF refusal, observed rather
/// than assumed.**
///
/// The subtitle seam needed a hand-rolled `eof` latch on its send side
/// because `avcodec_decode_subtitle2` has no state machine to refuse
/// for it. The claim that the *stream* decoders need no such latch —
/// that libavcodec answers `AVERROR_EOF` to a packet after a flush
/// packet, and that this crate's send gate reports it as a fault rather
/// than laundering it into `Sent::MustDrain` — is the other half of
/// that finding, and it is checked here against a live decoder instead
/// of being read off the docs.
///
/// The failure this excludes is the one the subtitle seam actually had:
/// a post-EOF packet accepted, decoded, and a terminal `Received::Ended`
/// reversed with no `flush` in the story.
#[test]
fn a_packet_after_eof_is_refused_by_the_audio_substrate() {
  let path = fixtures_root().join("pcm_s16le/02_pyannote_sample.wav");
  if !path.exists() {
    eprintln!("skipping: run `git submodule update --init` for {path:?}");
    return;
  }
  ffmpeg::init().expect("ffmpeg init");

  let mut input = ffmpeg::format::input(&path).expect("open input");
  let stream = input
    .streams()
    .best(ffmpeg::media::Type::Audio)
    .expect("audio stream");
  let stream_index = stream.index();
  let mut decoder = FfmpegAudioStreamDecoder::open(
    stream.parameters(),
    Timebase::default(),
    mediadecode_ffmpeg::DecoderLimits::default(),
  )
  .expect("open audio decoder");

  let packet = input
    .packets()
    .find_map(|(s, p)| (s.index() == stream_index).then_some(p))
    .expect("the fixture has packets");
  // SAFETY: `packets()` reads each packet with `av_read_frame`, so libavformat wrote its
  // payload, and it is live for the call.
  let pkt = unsafe {
    mediadecode_ffmpeg::boundary::owned_audio_packet_from_ffmpeg_in(
      &packet,
      mediadecode::Timebase::default(),
      mediadecode_ffmpeg::PacketLimits::default(),
    )
  }
  .expect("a wrappable payload")
  .expect("packet has a buffer");

  assert_eq!(decoder.send_packet(&pkt).expect("no fault"), Sent::Accepted);
  assert_eq!(decoder.send_eof().expect("no fault"), Sent::Accepted);

  // Drain to the settled end.
  let mut frame = empty_audio_frame();
  loop {
    match decoder.receive_frame(&mut frame).expect("no fault") {
      Received::Produced => {}
      Received::NeedsInput => panic!("a decoder at EOF asked for input"),
      Received::Ended => break,
    }
  }

  // The reversal, refused — by libavcodec, reported by the send gate.
  // **Never `MustDrain`**: draining changes nothing, so that answer
  // would be a loop whose next offer can never succeed.
  for _ in 0..2 {
    let refused = decoder.send_packet(&pkt);
    assert!(
      refused.is_err(),
      "a packet after end-of-stream must be a fault, got {refused:?}",
    );
  }
  assert_eq!(
    decoder.receive_frame(&mut frame).expect("no fault"),
    Received::Ended,
    "the refused packet must not have re-armed the decoder",
  );

  // `flush` is the way back on this road too.
  decoder.flush().expect("flush");
  assert_eq!(decoder.send_packet(&pkt).expect("no fault"), Sent::Accepted);
}
