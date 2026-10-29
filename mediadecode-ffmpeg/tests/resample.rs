//! The resample seam's contracts, pinned against real audio.
//!
//! The pipeline under test is the whole substrate: `FfmpegDemuxer` →
//! `FfmpegAudioStreamDecoder` → `FfmpegResampler`, driven through the
//! `mediadecode` traits. The media is PCM, generated at run time (see
//! `support/mod.rs`), so the decode step contributes no error of its
//! own to what the measurements below see.
//!
//! What is pinned:
//!
//! - a 48 kHz → 16 kHz conversion keeps the tone and lands within a
//!   few samples of the exact 3:1 length;
//! - a 44.1 kHz → 16 kHz conversion — a fractional ratio, 441:160 —
//!   does too, and leaves a tail inside the filter that only
//!   `send_eof` gets out;
//! - stereo folds to mono, layout and plane count following;
//! - output timestamps are continuous across every frame including the
//!   drained tail: `pts(n+1) == pts(n) + nb_samples(n)`;
//! - a mid-stream format change is refused by name, with no file
//!   involved.

mod support;

use ffmpeg_next::{
  ChannelLayout,
  format::{Sample, sample::Type},
};
use mediadecode::{
  Received,
  decoder::AudioStreamDecoder,
  demuxer::{DemuxedPacket, Demuxer, TrackKind},
  resampler::AudioResampler,
};
// The owned family under the names this suite was written with — the
// bare aliases mean the view lane now. Import block only; the
// assertions below are unchanged.
use mediadecode_ffmpeg::{
  FfmpegOwnedAudioStreamDecoder as FfmpegAudioStreamDecoder, FfmpegOwnedDemuxer as FfmpegDemuxer,
  FfmpegOwnedResampler as FfmpegResampler, ResampleError, ResampleSpec,
  empty_owned_audio_frame as empty_audio_frame,
};
use support::Corpus;

/// `true` while frames are still coming out, panicking on a fault.
///
/// The pre-EOF half of a drain, and the reason it is a named helper: a
/// `.is_ok()` loop cannot be written any more — "needs input" is a
/// success — and writing the match out at each of the dozen drain sites
/// in this file would bury what each test is actually about.
fn frame_ready<E: core::fmt::Debug>(status: Result<Received, E>) -> bool {
  match status.expect("a fault-free drain") {
    Received::Produced => true,
    Received::NeedsInput | Received::Ended => false,
  }
}

/// Everything one run of the pipeline produced.
struct Converted {
  /// Interleaved samples, in the target format's native width.
  samples: Vec<i16>,
  /// `(pts, nb_samples)` per delivered frame, in target-rate ticks.
  frames: Vec<(i64, u32)>,
  /// How many frames came out only after `send_eof` — the conversion
  /// tail.
  tail_frames: usize,
  /// The channel count the produced frames advertised.
  channels: u8,
  /// The plane count the produced frames advertised.
  planes: u8,
}

/// Demuxes, decodes and resamples one file end to end through the
/// trait surface.
fn run(path: &std::path::Path, target: ResampleSpec) -> Converted {
  let mut demuxer = FfmpegDemuxer::open(path).expect("open");
  let track = demuxer
    .tracks()
    .iter()
    .position(|t| t.kind() == TrackKind::Audio)
    .expect("an audio track");
  let info = &demuxer.tracks()[track];
  let mut decoder = FfmpegAudioStreamDecoder::open(
    info
      .extra()
      .clone_parameters()
      .expect("the checked handoff"),
    info.timebase(),
    mediadecode_ffmpeg::DecoderLimits::default(),
  )
  .expect("open decoder");

  // The source spec comes off the opened decoder: it is exactly what
  // the frames will carry. The track's declared parameters agree here
  // (PCM), and the lane below checks that they do.
  let source = ResampleSpec::from_decoder(decoder.inner()).expect("the decoder names its shape");
  assert_eq!(
    ResampleSpec::from_ticket(info.extra().ticket()),
    Some(source),
    "for PCM the declared spec and the decoder's own spec are the same",
  );

  let mut resampler =
    FfmpegResampler::new(source, target, mediadecode_ffmpeg::FrameLimits::default())
      .expect("open resampler");
  let mut decoded = empty_audio_frame();
  let mut out = empty_audio_frame();
  let mut converted = Converted {
    samples: Vec::new(),
    frames: Vec::new(),
    tail_frames: 0,
    channels: 0,
    planes: 0,
  };

  let collect = |converted: &mut Converted, frame: &mediadecode_ffmpeg::OwnedAudioFrame| {
    converted.channels = frame.channel_count();
    converted.planes = frame.plane_count();
    converted.frames.push((
      frame.pts().expect("every output frame is stamped").pts(),
      frame.nb_samples(),
    ));
    // The target format below is packed s16, so plane 0 interleaves
    // every channel. The plane's buffer is padded to FFmpeg's
    // alignment; the valid range is what the header claims.
    let valid = frame.nb_samples() as usize * frame.channel_count() as usize * 2;
    let bytes = &frame.planes()[0].data_ref().as_ref()[..valid];
    converted.samples.extend(
      bytes
        .as_chunks::<2>()
        .0
        .iter()
        .copied()
        .map(i16::from_le_bytes),
    );
  };

  while let Some(packet) = demuxer.next_packet().expect("pull") {
    let DemuxedPacket::Audio(p) = packet else {
      continue;
    };
    let (t, packet) = p.into_parts();
    if t.get() != track {
      continue;
    }
    support::accepted(decoder.send_packet(&packet), "send_packet");
    while frame_ready(decoder.receive_frame(&mut decoded)) {
      support::accepted(resampler.send_frame(&decoded), "send_frame");
      while frame_ready(resampler.receive_frame(&mut out)) {
        collect(&mut converted, &out);
      }
    }
  }
  support::accepted(decoder.send_eof(), "decoder eof");
  while frame_ready(decoder.receive_frame(&mut decoded)) {
    support::accepted(resampler.send_frame(&decoded), "send_frame");
    while frame_ready(resampler.receive_frame(&mut out)) {
      collect(&mut converted, &out);
    }
  }

  // The tail. Everything after this point is what would be lost — and
  // the loop that collects it terminates on `Ended`, not on a refusal
  // it has to guess the meaning of.
  support::accepted(resampler.send_eof(), "resampler eof");
  loop {
    match resampler
      .receive_frame(&mut out)
      .expect("no fault in the tail")
    {
      Received::Produced => {
        converted.tail_frames += 1;
        collect(&mut converted, &out);
      }
      Received::NeedsInput => {
        panic!("a resampler at EOF asked for input the caller does not have")
      }
      Received::Ended => break,
    }
  }
  converted
}

/// Sign changes across the signal — for a pure tone, twice its
/// frequency times its duration. Cheap, exact enough to tell 440 Hz
/// from the 1320 Hz a skipped resample or the 147 Hz a doubled one
/// would produce, and readable without a transform.
fn zero_crossings(samples: &[i16]) -> usize {
  samples
    .windows(2)
    .filter(|w| (w[0] >= 0) != (w[1] >= 0))
    .count()
}

fn mono_16k() -> ResampleSpec {
  ResampleSpec::new(16_000, Sample::I16(Type::Packed), ChannelLayout::MONO)
}

#[test]
fn forty_eight_to_sixteen_keeps_the_tone_and_the_length() {
  let Some(corpus) = Corpus::new() else { return };
  let path = corpus.sine_wav("sine48.wav", 48_000, 1, 440, 1.0);
  let out = run(&path, mono_16k());

  // 3:1 exactly. The window is ±0.25% rather than exact so a different
  // `swr` engine may round its edges differently; FFmpeg 9's default
  // lands on 16000 on the nose, and a conversion that forgot to drain
  // or forgot to filter misses by a whole factor.
  assert!(
    (15_960..=16_040).contains(&out.samples.len()),
    "16000 samples expected, got {}",
    out.samples.len(),
  );
  // 440 Hz over one second: 880 sign changes. A conversion that
  // dropped samples instead of filtering would land near 2640.
  let crossings = zero_crossings(&out.samples);
  assert!(
    (860..=900).contains(&crossings),
    "440 Hz means ~880 zero crossings, got {crossings}",
  );
  assert_eq!(out.channels, 1);
}

#[test]
fn a_fractional_ratio_still_lands_on_length_and_leaves_a_tail() {
  let Some(corpus) = Corpus::new() else { return };
  // 44100 -> 16000 is 441:160: no whole-number relationship, so the
  // filter never empties on a frame boundary and the tail is real.
  let path = corpus.sine_wav("sine44.wav", 44_100, 1, 440, 1.0);
  let out = run(&path, mono_16k());

  assert!(
    (15_960..=16_040).contains(&out.samples.len()),
    "16000 samples expected, got {}",
    out.samples.len(),
  );
  let crossings = zero_crossings(&out.samples);
  assert!(
    (860..=900).contains(&crossings),
    "440 Hz means ~880 zero crossings, got {crossings}",
  );
  assert!(
    out.tail_frames > 0,
    "send_eof drained nothing — every file would lose its last tens of milliseconds",
  );
}

#[test]
fn stereo_folds_to_mono() {
  let Some(corpus) = Corpus::new() else { return };
  let path = corpus.sine_wav("sine48-stereo.wav", 48_000, 2, 440, 1.0);

  let source = {
    let demuxer = FfmpegDemuxer::open(&path).expect("open");
    ResampleSpec::from_ticket(demuxer.tracks()[0].extra().ticket()).expect("audio spec")
  };
  assert_eq!(source.channels(), 2, "the file really is stereo");

  let out = run(&path, mono_16k());
  assert_eq!(out.channels, 1, "the target layout is what comes out");
  assert_eq!(out.planes, 1, "packed s16 mono is one plane");
  assert!(
    (15_960..=16_040).contains(&out.samples.len()),
    "one sample per output frame per channel, and there is one channel now: got {}",
    out.samples.len(),
  );
}

#[test]
fn output_timestamps_are_continuous_through_the_tail() {
  let Some(corpus) = Corpus::new() else { return };
  let path = corpus.sine_wav("sine44.wav", 44_100, 1, 440, 1.0);
  let out = run(&path, mono_16k());

  assert!(out.frames.len() > 1, "more than one frame to compare");
  assert_eq!(
    out.frames[0].0, 0,
    "the source starts at zero, so does the output"
  );
  for pair in out.frames.windows(2) {
    let (pts, samples) = pair[0];
    let (next, _) = pair[1];
    assert_eq!(
      next,
      pts + i64::from(samples),
      "a gap or an overlap in the output timeline at pts {pts}",
    );
  }
  let (last_pts, last_len) = *out.frames.last().expect("at least one frame");
  assert_eq!(
    last_pts + i64::from(last_len),
    out.samples.len() as i64,
    "the timeline ends where the samples do",
  );
}

#[test]
fn a_mid_stream_format_change_is_refused_by_name() {
  // No file: the refusal is a property of the face, and a decoded
  // frame with the wrong header is enough to provoke it.
  support::init_ffmpeg();

  let source = ResampleSpec::new(48_000, Sample::I16(Type::Packed), ChannelLayout::STEREO);
  let mut resampler = FfmpegResampler::new(
    source,
    mono_16k(),
    mediadecode_ffmpeg::FrameLimits::default(),
  )
  .expect("open resampler");

  let good = mediadecode_ffmpeg::OwnedAudioFrame::new(
    48_000,
    0,
    2,
    mediadecode_ffmpeg::SampleFormat::S16,
    mediadecode_ffmpeg::channel_layout_description_from_ffmpeg(&ChannelLayout::STEREO)
      .expect("a well-formed layout describes"),
    std::array::from_fn(|_| {
      mediadecode::frame::Plane::new(mediadecode_ffmpeg::FfmpegBytes::empty(), 0)
    }),
    1,
    Default::default(),
  );
  support::accepted(resampler.send_frame(&good), "the declared source spec");

  // Same everything but the rate.
  let changed = mediadecode_ffmpeg::OwnedAudioFrame::new(
    44_100,
    0,
    2,
    mediadecode_ffmpeg::SampleFormat::S16,
    mediadecode_ffmpeg::channel_layout_description_from_ffmpeg(&ChannelLayout::STEREO)
      .expect("a well-formed layout describes"),
    std::array::from_fn(|_| {
      mediadecode::frame::Plane::new(mediadecode_ffmpeg::FfmpegBytes::empty(), 0)
    }),
    1,
    Default::default(),
  );
  let err = resampler
    .send_frame(&changed)
    .expect_err("the face never silently reconfigures");
  match err {
    ResampleError::SourceChanged(p) => {
      assert_eq!((p.expected_rate(), p.found_rate()), (48_000, 44_100));
    }
    other => panic!("expected a named refusal, got {other:?}"),
  }

  // And the same for a layout change at the same rate.
  let mono = mediadecode_ffmpeg::OwnedAudioFrame::new(
    48_000,
    0,
    1,
    mediadecode_ffmpeg::SampleFormat::S16,
    mediadecode_ffmpeg::channel_layout_description_from_ffmpeg(&ChannelLayout::MONO)
      .expect("a well-formed layout describes"),
    std::array::from_fn(|_| {
      mediadecode::frame::Plane::new(mediadecode_ffmpeg::FfmpegBytes::empty(), 0)
    }),
    1,
    Default::default(),
  );
  assert!(matches!(
    resampler.send_frame(&mono),
    Err(ResampleError::SourceChanged(_)),
  ));
}

#[test]
fn the_needs_more_signal_lives_in_the_ok_arm() {
  support::init_ffmpeg();
  let source = ResampleSpec::new(48_000, Sample::I16(Type::Packed), ChannelLayout::STEREO);
  let mut resampler = FfmpegResampler::new(
    source,
    mono_16k(),
    mediadecode_ffmpeg::FrameLimits::default(),
  )
  .expect("open resampler");
  let mut dst = empty_audio_frame();

  assert_eq!(
    resampler
      .receive_frame(&mut dst)
      .expect("an empty session is not a fault"),
    Received::NeedsInput,
  );

  // After EOF with nothing inside, the drain is empty — and says the
  // *other* thing, which is the distinction that did not exist before.
  support::accepted(resampler.send_eof(), "eof");
  assert_eq!(
    resampler
      .receive_frame(&mut dst)
      .expect("an empty tail is not a fault"),
    Received::Ended,
  );

  // `send_frame` after EOF is refused rather than silently accepted.
  let frame = mediadecode_ffmpeg::OwnedAudioFrame::new(
    48_000,
    0,
    2,
    mediadecode_ffmpeg::SampleFormat::S16,
    mediadecode_ffmpeg::channel_layout_description_from_ffmpeg(&ChannelLayout::STEREO)
      .expect("a well-formed layout describes"),
    std::array::from_fn(|_| {
      mediadecode::frame::Plane::new(mediadecode_ffmpeg::FfmpegBytes::empty(), 0)
    }),
    1,
    Default::default(),
  );
  assert!(matches!(
    resampler.send_frame(&frame),
    Err(ResampleError::AfterEof),
  ));

  // Flush is the way back: the resampler is reusable for another
  // stream on the same two specs.
  resampler.flush().expect("flush");
  support::accepted(resampler.send_frame(&frame), "reusable after flush");
}

/// **The spin-forever regression, on the real `swresample` road.**
///
/// The conflation this proves gone: `ResampleError::Again` was returned
/// *pre-EOF with nothing ready* and *post-EOF with the tail exhausted*.
/// Those are opposite instructions — "send me more" and "there is no
/// more" — and a caller polling the seam could tell them apart only by
/// remembering whether it had itself called `send_eof`. A generic drain,
/// which does not know, therefore either stopped early (losing the tail)
/// or asked forever.
///
/// The loop below is that generic drain: it has no input left to offer,
/// so treating `NeedsInput` as "feed it" would hang. It terminates
/// because the end of the tail has its own word. The iteration cap turns
/// what would have been a hanging test into a failing one.
#[test]
fn a_drained_tail_says_ended_instead_of_asking_for_input_that_cannot_come() {
  support::init_ffmpeg();
  let mut resampler = FfmpegResampler::new(
    ResampleSpec::new(48_000, Sample::I16(Type::Packed), ChannelLayout::STEREO),
    mono_16k(),
    mediadecode_ffmpeg::FrameLimits::default(),
  )
  .expect("open resampler");

  // Enough input for the 48k->16k filter to hold a real tail back.
  let samples = 4_800u32;
  for index in 0..4 {
    support::accepted(
      resampler.send_frame(&stereo_frame(
        samples,
        samples as usize * 2 * 2,
        Some(i64::from(index) * i64::from(samples)),
      )),
      "send_frame",
    );
  }
  support::accepted(resampler.send_eof(), "send_eof");
  assert!(
    resampler.delay() > 0,
    "there has to be a tail inside the filter for this to prove anything",
  );

  let mut out = empty_audio_frame();
  let mut tail_frames = 0u32;
  let mut ended = false;
  for _ in 0..256 {
    match resampler
      .receive_frame(&mut out)
      .expect("a clean drain raises no fault")
    {
      Received::Produced => tail_frames += 1,
      Received::NeedsInput => panic!(
        "the resampler asked for input after being told the stream ended —          the caller has nothing left to send, so this is the hang the old          `Again` arm produced",
      ),
      Received::Ended => {
        ended = true;
        break;
      }
    }
  }
  assert!(ended, "the drain never reached the end of the tail");
  assert!(
    tail_frames > 0,
    "no tail was drained, so nothing was proved"
  );
  // **`delay()` is deliberately not asserted to be zero here.**
  // `swr_get_delay` stands at a small residue (16 samples on this
  // corpus) that `swr_convert_frame` will never emit — which is exactly
  // why the old code spun: `delay > 0` sent it back into the flush road,
  // the flush produced nothing, and the answer was the same `Again` that
  // means "send more input" before EOF. The end of a tail is what the
  // converter will still produce, not what the delay counter says.

  // And it is a settled answer, not a momentary one: polling past the
  // end keeps saying the same thing rather than sending the caller back
  // for input it does not have.
  for _ in 0..4 {
    assert_eq!(
      resampler
        .receive_frame(&mut out)
        .expect("no fault past the end"),
      Received::Ended,
    );
  }

  // `flush` is the only thing that retracts it — the end is a session
  // state, and a new stream on the same specs starts over.
  resampler.flush().expect("flush");
  assert_eq!(
    resampler.receive_frame(&mut out).expect("no fault"),
    Received::NeedsInput,
  );
}

// ---------------------------------------------------------------------------
//  Adversarial lanes. Every one of these is a shape a container, a
//  decoder or a caller can produce and no honest file does.
// ---------------------------------------------------------------------------

/// A source frame in the 48 kHz stereo packed-s16 spec, whose header
/// claims `samples` samples while its single plane really holds
/// `plane_len` bytes. The two agree for an honest frame
/// (`samples * 2 channels * 2 bytes`) and disagree for a forged one.
fn stereo_frame(
  samples: u32,
  plane_len: usize,
  pts: Option<i64>,
) -> mediadecode_ffmpeg::OwnedAudioFrame {
  filled_frame(
    48_000,
    samples,
    2,
    ChannelLayout::STEREO,
    &vec![0u8; plane_len],
    pts,
  )
}

/// A mono packed-s16 frame at `rate` holding `samples` samples of
/// constant amplitude — DC, which a resampler's low-pass keeps rather
/// than smooths away, so a tail left inside the filter is visible in
/// the next stream's output.
fn mono_frame(
  rate: u32,
  samples: u32,
  amplitude: i16,
  pts: Option<i64>,
) -> mediadecode_ffmpeg::OwnedAudioFrame {
  let bytes: Vec<u8> = std::iter::repeat_n(amplitude.to_le_bytes(), samples as usize)
    .flatten()
    .collect();
  filled_frame(rate, samples, 1, ChannelLayout::MONO, &bytes, pts)
}

fn filled_frame(
  rate: u32,
  samples: u32,
  channels: u8,
  layout: ChannelLayout,
  bytes: &[u8],
  pts: Option<i64>,
) -> mediadecode_ffmpeg::OwnedAudioFrame {
  let plane = mediadecode_ffmpeg::FfmpegBytes::try_copy_from_slice(bytes).expect("a test payload");
  let planes = std::array::from_fn(|index| {
    mediadecode::frame::Plane::new(
      if index == 0 {
        plane.clone()
      } else {
        mediadecode_ffmpeg::FfmpegBytes::empty()
      },
      0,
    )
  });
  mediadecode_ffmpeg::OwnedAudioFrame::new(
    rate,
    samples,
    channels,
    mediadecode_ffmpeg::SampleFormat::S16,
    mediadecode_ffmpeg::channel_layout_description_from_ffmpeg(&layout)
      .expect("a well-formed layout describes"),
    planes,
    1,
    Default::default(),
  )
  .with_pts(pts.map(|pts| {
    mediadecode::Timestamp::new(
      pts,
      mediadecode::Timebase::new(
        1,
        std::num::NonZeroI32::new(rate as i32).expect("a real rate"),
      ),
    )
  }))
}

#[test]
fn a_custom_layout_is_refused_by_name() {
  support::init_ffmpeg();

  // `ResampleSpec::new` is `const` and total, so this is the route that
  // walks straight past `from_parameters` / `from_decoder` and their
  // refusals. A CUSTOM `AVChannelLayout` owns a heap channel map that
  // FFmpeg frees with `av_channel_layout_uninit` — which every owning
  // `AVFrame` runs on drop — while `ffmpeg_next::ChannelLayout` copies
  // it by assignment and has no destructor. One staged frame dropped
  // would free the map this test still holds.
  let mut raw: ffmpeg_next::ffi::AVChannelLayout = unsafe { std::mem::zeroed() };
  let rc = unsafe { ffmpeg_next::ffi::av_channel_layout_custom_init(&mut raw, 2) };
  assert_eq!(rc, 0, "av_channel_layout_custom_init");
  assert_eq!(
    raw.order,
    ffmpeg_next::ffi::AVChannelOrder::AV_CHANNEL_ORDER_CUSTOM,
    "the layout under test really is a custom one",
  );
  let custom = ChannelLayout(raw);

  let hazardous = ResampleSpec::new(48_000, Sample::I16(Type::Packed), custom);
  match FfmpegResampler::new(
    hazardous,
    mono_16k(),
    mediadecode_ffmpeg::FrameLimits::default(),
  ) {
    Err(ResampleError::UnsupportedLayout(p)) => {
      assert_eq!(p.end().to_string(), "source");
      assert_eq!(p.channels(), 2);
    }
    Err(other) => panic!("expected UnsupportedLayout, got {other:?}"),
    Ok(_) => panic!("a custom layout must not reach swr or a staged frame"),
  }

  // The same refusal from the other end.
  let source = ResampleSpec::new(48_000, Sample::I16(Type::Packed), ChannelLayout::STEREO);
  assert!(matches!(
    FfmpegResampler::new(
      source,
      ResampleSpec::new(16_000, Sample::I16(Type::Packed), custom), mediadecode_ffmpeg::FrameLimits::default()),
    Err(ResampleError::UnsupportedLayout(p)) if p.end() == mediadecode_ffmpeg::SpecEnd::Target,
  ));

  // The map is still ours to free: nothing took a copy of it.
  unsafe { ffmpeg_next::ffi::av_channel_layout_uninit(&mut raw) };

  // The rest of the roster the choke point owns, since the `const`
  // constructor cannot.
  assert!(matches!(
    FfmpegResampler::new(
      ResampleSpec::new(0, Sample::I16(Type::Packed), ChannelLayout::STEREO),
      mono_16k(), mediadecode_ffmpeg::FrameLimits::default()),
    Err(ResampleError::UnsupportedRate(p)) if p.rate() == 0,
  ));
  assert!(matches!(
    FfmpegResampler::new(
      ResampleSpec::new(48_000, Sample::None, ChannelLayout::STEREO),
      mono_16k(),
      mediadecode_ffmpeg::FrameLimits::default()
    ),
    Err(ResampleError::UnsupportedFormat(_)),
  ));
  let mut empty: ffmpeg_next::ffi::AVChannelLayout = unsafe { std::mem::zeroed() };
  empty.nb_channels = 0;
  assert!(matches!(
    FfmpegResampler::new(
      ResampleSpec::new(48_000, Sample::I16(Type::Packed), ChannelLayout(empty)),
      mono_16k(), mediadecode_ffmpeg::FrameLimits::default()),
    Err(ResampleError::UnsupportedLayout(p)) if p.channels() == 0,
  ));
}

/// A packed s16 frame at `rate` whose 440 Hz tone lives in exactly one
/// of `layout`'s channels — every other channel is silence. Sweeping
/// `channel` across a layout answers "does this input channel reach the
/// output at all".
fn tone_in_one_channel(
  rate: u32,
  samples: u32,
  layout: ChannelLayout,
  channel: usize,
) -> mediadecode_ffmpeg::OwnedAudioFrame {
  let channels = layout.channels() as usize;
  let mut bytes = Vec::with_capacity(samples as usize * channels * 2);
  for n in 0..samples {
    let phase = f64::from(n) * 2.0 * std::f64::consts::PI * 440.0 / f64::from(rate);
    let value = (phase.sin() * 20_000.0) as i16;
    for ch in 0..channels {
      bytes.extend_from_slice(&if ch == channel { value } else { 0 }.to_le_bytes());
    }
  }
  filled_frame(rate, samples, channels as u8, layout, &bytes, None)
}

/// Root-mean-square of everything a conversion produced for that frame.
/// Zero means the channel the tone was in reached nothing.
fn converted_rms(
  resampler: &mut FfmpegResampler,
  frame: &mediadecode_ffmpeg::OwnedAudioFrame,
) -> f64 {
  let mut out = empty_audio_frame();
  let mut energy = 0f64;
  let mut count = 0usize;
  for _ in 0..3 {
    support::accepted(resampler.send_frame(frame), "send_frame");
    while frame_ready(resampler.receive_frame(&mut out)) {
      let valid = out.nb_samples() as usize * out.channel_count() as usize * 2;
      for chunk in out.planes()[0].data_ref().as_ref()[..valid]
        .as_chunks::<2>()
        .0
      {
        let sample = f64::from(i16::from_le_bytes(*chunk));
        energy += sample * sample;
        count += 1;
      }
    }
  }
  if count == 0 {
    0.0
  } else {
    (energy / count as f64).sqrt()
  }
}

#[test]
fn no_accepted_conversion_silently_drops_a_channel() {
  support::init_ffmpeg();
  // The bar this lane exists to meet: a tone isolated in each source
  // channel in turn, measured at the output. A conversion this crate
  // accepts may not lose one.
  let rate = 48_000;
  let samples = 4_800;

  // 7.1 -> mono: an everyday downmix, accepted.
  let source = ResampleSpec::new(rate, Sample::I16(Type::Packed), ChannelLayout::_7POINT1);
  for channel in 0..8 {
    let mut resampler = FfmpegResampler::new(
      source,
      mono_16k(),
      mediadecode_ffmpeg::FrameLimits::default(),
    )
    .expect("7.1 -> mono opens");
    let rms = converted_rms(
      &mut resampler,
      &tone_in_one_channel(rate, samples, ChannelLayout::_7POINT1, channel),
    );
    if channel == 3 {
      // Channel 3 is LFE, and FFmpeg's default downmix leaves it out
      // *on purpose* (`lfe_mix_level` is zero unless a caller asks
      // otherwise). Naming it here is the difference between a policy
      // and a defect: the construction check asks whether a channel
      // *can* reach the output, with LFE's mix level forced non-zero,
      // so this exception cannot hide a channel swr simply cannot mix.
      assert!(rms < 1.0, "LFE unexpectedly mixed at {rms}");
      continue;
    }
    assert!(rms > 100.0, "source channel {channel} vanished (rms {rms})");
  }

  // 22.2 -> 22.2 at another rate: no rematrixing, twenty-four channels,
  // every one of them must come through — LFE included, since nothing
  // is being mixed away.
  let big = ChannelLayout::_22POINT2;
  let source = ResampleSpec::new(rate, Sample::I16(Type::Packed), big);
  let target = ResampleSpec::new(16_000, Sample::I16(Type::Packed), big);
  for channel in 0..24 {
    let mut resampler =
      FfmpegResampler::new(source, target, mediadecode_ffmpeg::FrameLimits::default())
        .expect("22.2 -> 22.2 opens");
    let rms = converted_rms(
      &mut resampler,
      &tone_in_one_channel(rate, samples, big, channel),
    );
    assert!(
      rms > 10.0,
      "source channel {channel} vanished from a same-layout conversion (rms {rms})",
    );
  }
}

#[test]
fn a_rematrix_that_would_drop_channels_is_refused_by_name() {
  support::init_ffmpeg();
  // Packed 22.2 -> mono: swr mixes nine of the twenty-four channels and
  // processes the rest as though they were not there. It used to open
  // happily — the planar-only refusal never looked at it.
  let source = ResampleSpec::new(48_000, Sample::I16(Type::Packed), ChannelLayout::_22POINT2);
  match FfmpegResampler::new(
    source,
    mono_16k(),
    mediadecode_ffmpeg::FrameLimits::default(),
  ) {
    Err(ResampleError::ChannelDropped(p)) => {
      assert_eq!((p.source_channels(), p.target_channels()), (24, 1));
      assert_eq!(
        p.channel(),
        9,
        "the first channel swr's matrix cannot route"
      );
    }
    Err(other) => panic!("expected ChannelDropped, got {other:?}"),
    Ok(_) => panic!("a conversion that drops fifteen channels must not open"),
  }

  // And the case a channel-count threshold would have waved through:
  // `cube` is eight channels, and two of them reach nothing.
  assert!(
    matches!(
      FfmpegResampler::new(
        ResampleSpec::new(48_000, Sample::I16(Type::Packed), ChannelLayout::CUBE),
        ResampleSpec::new(16_000, Sample::I16(Type::Packed), ChannelLayout::STEREO), mediadecode_ffmpeg::FrameLimits::default())
      .map(|_| ()),
      Err(ResampleError::ChannelDropped(p)) if p.channel() == 6,
    ),
    "eight channels is not a safe count, it is just a small one",
  );

  // The controls: every one of these routes every source channel, and
  // every one of them still opens.
  for (name, source_layout, target_layout) in [
    ("7.1 -> mono", ChannelLayout::_7POINT1, ChannelLayout::MONO),
    (
      "octagonal -> mono",
      ChannelLayout::OCTAGONAL,
      ChannelLayout::MONO,
    ),
    (
      "7.1.2 -> stereo",
      ChannelLayout::_7POINT1POINT2,
      ChannelLayout::STEREO,
    ),
    (
      "stereo -> 22.2",
      ChannelLayout::STEREO,
      ChannelLayout::_22POINT2,
    ),
    (
      "22.2 -> 22.2",
      ChannelLayout::_22POINT2,
      ChannelLayout::_22POINT2,
    ),
    (
      "5.1 -> 7.1",
      ChannelLayout::_5POINT1,
      ChannelLayout::_7POINT1,
    ),
  ] {
    FfmpegResampler::new(
      ResampleSpec::new(48_000, Sample::I16(Type::Packed), source_layout),
      ResampleSpec::new(16_000, Sample::I16(Type::Packed), target_layout),
      mediadecode_ffmpeg::FrameLimits::default(),
    )
    .unwrap_or_else(|e| panic!("{name} must still open: {e}"));
  }
}

/// Runs one conversion straight through `swresample`, with none of this
/// crate's checks in the way, and returns the output's RMS for a tone
/// isolated in `channel`. The only way to measure a pair the crate now
/// refuses — which is the point: the refusal has to match what the
/// library really does with it.
fn raw_swr_rms(source_layout: ChannelLayout, channel: usize) -> f64 {
  use ffmpeg_next::{frame, software::resampling::Context};

  let channels = source_layout.channels() as usize;
  let samples = 4_800usize;
  let mut context = Context::get(
    Sample::I16(Type::Packed),
    source_layout,
    48_000,
    Sample::I16(Type::Packed),
    ChannelLayout::MONO,
    16_000,
  )
  .expect("swresample opens this pair happily — that is the whole problem");

  // The frame carries the *resolved* layout, not the declared one:
  // `swr_init` substitutes its default for an unspecified layout and
  // then compares every frame against that, answering
  // `AVERROR_INPUT_CHANGED` to anything else. Which is the same fact
  // this round is about — the conversion that runs is between the
  // effective layouts — arriving from the other side.
  let staged = if source_layout.is_empty() {
    ChannelLayout::default(source_layout.channels())
  } else {
    source_layout
  };
  let mut input = frame::Audio::new(Sample::I16(Type::Packed), samples, staged);
  input.set_rate(48_000);
  {
    let plane = input.data_mut(0);
    for n in 0..samples {
      let phase = n as f64 * 2.0 * std::f64::consts::PI * 440.0 / 48_000.0;
      let value = (phase.sin() * 20_000.0) as i16;
      for ch in 0..channels {
        let offset = (n * channels + ch) * 2;
        let sample = if ch == channel { value } else { 0 };
        plane[offset..offset + 2].copy_from_slice(&sample.to_le_bytes());
      }
    }
  }

  let mut out = frame::Audio::new(Sample::I16(Type::Packed), samples, ChannelLayout::MONO);
  out.set_rate(16_000);
  context.run(&input, &mut out).expect("convert");
  let produced = out.samples();
  if produced == 0 {
    return 0.0;
  }
  let bytes = &out.data(0)[..produced * 2];
  let energy: f64 = bytes
    .as_chunks::<2>()
    .0
    .iter()
    .map(|chunk| {
      let sample = f64::from(i16::from_le_bytes(*chunk));
      sample * sample
    })
    .sum();
  (energy / produced as f64).sqrt()
}

#[test]
fn an_unspecified_layout_cannot_smuggle_a_lossy_conversion_past_the_pair_check() {
  support::init_ffmpeg();
  // The bypass: the pair was judged on the layouts the caller
  // *declared*, and an unspecified one declares nothing to rematrix.
  // But construction resolves it to FFmpeg's default for its channel
  // count before opening `swr` — and twenty-four unspecified channels
  // resolve to exactly the 22.2 whose explicit conversion is refused.
  assert_eq!(
    ChannelLayout::default(24),
    ChannelLayout::_22POINT2,
    "the resolution that made the door: 24 unspecified channels are 22.2",
  );

  // What the library really does with the effective pair, measured
  // through raw `swresample` because this crate will not open it now.
  let unspec24 = ResampleSpec::unspecified_layout(24);
  assert!(
    raw_swr_rms(unspec24, 0) > 100.0,
    "channel 0 must survive, or the probe measures nothing",
  );
  for channel in [9, 23] {
    assert_eq!(
      raw_swr_rms(unspec24, channel),
      0.0,
      "source channel {channel} was expected to vanish through raw swr",
    );
  }

  // And the refusal names the first channel that vanishes.
  let source = ResampleSpec::new(48_000, Sample::I16(Type::Packed), unspec24);
  for target_layout in [ChannelLayout::MONO, ChannelLayout::STEREO] {
    let target = ResampleSpec::new(16_000, Sample::I16(Type::Packed), target_layout);
    match FfmpegResampler::new(source, target, mediadecode_ffmpeg::FrameLimits::default())
      .map(|_| ())
    {
      Err(ResampleError::ChannelDropped(p)) => {
        assert_eq!(p.source_channels(), 24);
        assert_eq!(
          p.channel(),
          9,
          "the refusal names the channel the tone probe found silent",
        );
      }
      other => panic!("unspecified 24 channels must not open: {other:?}"),
    }
  }
}

#[test]
fn the_maskless_wav_population_still_converts() {
  support::init_ffmpeg();
  // The legitimate unspecified population, and the reason the check is
  // about the *effective* pair rather than a blanket refusal of
  // unspecified layouts: a WAV without a `WAVE_FORMAT_EXTENSIBLE`
  // channel mask declares no layout at all, and every one of them must
  // keep working. One channel resolves to mono (an identical effective
  // pair, nothing to rematrix); two resolve to stereo, whose downmix
  // routes both channels.
  let rate = 48_000;
  let samples = 4_800;

  let mono = ResampleSpec::new(
    rate,
    Sample::I16(Type::Packed),
    ResampleSpec::unspecified_layout(1),
  );
  let mut resampler =
    FfmpegResampler::new(mono, mono_16k(), mediadecode_ffmpeg::FrameLimits::default())
      .expect("maskless mono still opens");
  let rms = converted_rms(
    &mut resampler,
    &tone_in_one_channel(rate, samples, ResampleSpec::unspecified_layout(1), 0),
  );
  assert!(
    rms > 100.0,
    "maskless mono converted to silence (rms {rms})"
  );

  let stereo_layout = ResampleSpec::unspecified_layout(2);
  let stereo = ResampleSpec::new(rate, Sample::I16(Type::Packed), stereo_layout);
  for channel in 0..2 {
    let mut resampler = FfmpegResampler::new(
      stereo,
      mono_16k(),
      mediadecode_ffmpeg::FrameLimits::default(),
    )
    .expect("maskless stereo still opens");
    let rms = converted_rms(
      &mut resampler,
      &tone_in_one_channel(rate, samples, stereo_layout, channel),
    );
    assert!(
      rms > 100.0,
      "maskless stereo lost channel {channel} (rms {rms})",
    );
  }

  // And an unspecified pair that resolves to the same layout on both
  // ends still needs no matrix at all, whatever its channel count.
  FfmpegResampler::new(
    ResampleSpec::new(
      rate,
      Sample::I16(Type::Packed),
      ResampleSpec::unspecified_layout(24),
    ),
    ResampleSpec::new(
      16_000,
      Sample::I16(Type::Packed),
      ResampleSpec::unspecified_layout(24),
    ),
    mediadecode_ffmpeg::FrameLimits::default(),
  )
  .expect("nothing is being rematrixed here");
}

#[test]
fn a_planar_layout_past_eight_channels_is_refused_at_construction() {
  support::init_ffmpeg();
  // 22.2 is twenty-four channels. Planar, that is twenty-four planes,
  // and a `mediadecode` audio frame has eight slots. As a source no
  // frame could ever arrive; as a target `swr` would produce one this
  // crate cannot hand back — and only after consuming the input, so the
  // failure would land on a session the caller cannot retry. Both are
  // refused where nothing has happened yet.
  let planar_22_2 = ResampleSpec::new(48_000, Sample::F32(Type::Planar), ChannelLayout::_22POINT2);
  assert_eq!(planar_22_2.channels(), 24, "22.2 really is 24 channels");

  match FfmpegResampler::new(
    planar_22_2,
    mono_16k(),
    mediadecode_ffmpeg::FrameLimits::default(),
  ) {
    Err(ResampleError::TooManyPlanes(p)) => {
      assert_eq!(p.end().to_string(), "source");
      assert_eq!((p.channels(), p.limit()), (24, 8));
    }
    Err(other) => panic!("expected TooManyPlanes for the source, got {other:?}"),
    Ok(_) => panic!("a source no frame can represent must not open"),
  }

  let stereo = ResampleSpec::new(48_000, Sample::I16(Type::Packed), ChannelLayout::STEREO);
  assert!(
    matches!(
      FfmpegResampler::new(stereo, planar_22_2, mediadecode_ffmpeg::FrameLimits::default()).map(|_| ()),
      Err(ResampleError::TooManyPlanes(p))
        if p.end() == mediadecode_ffmpeg::SpecEnd::Target && p.channels() == 24,
    ),
    "the target end is the one that used to fail mid-stream",
  );

  // Packed 22.2 is one plane and stays welcome — to *this* rule.
  // Converting it to mono is refused by the pair check instead, for
  // dropping channels, which `a_rematrix_that_would_drop_channels_is_refused_by_name`
  // pins; a same-layout conversion has no rematrixing to refuse and
  // opens, which is what proves this rule is about plane slots rather
  // than channel count.
  FfmpegResampler::new(
    ResampleSpec::new(48_000, Sample::I16(Type::Packed), ChannelLayout::_22POINT2),
    ResampleSpec::new(16_000, Sample::I16(Type::Packed), ChannelLayout::_22POINT2),
    mediadecode_ffmpeg::FrameLimits::default(),
  )
  .expect("packed 22.2 is one plane");
  assert!(
    matches!(
      FfmpegResampler::new(
        ResampleSpec::new(48_000, Sample::F32(Type::Planar), ChannelLayout::_22POINT2),
        ResampleSpec::new(16_000, Sample::F32(Type::Planar), ChannelLayout::_22POINT2),
        mediadecode_ffmpeg::FrameLimits::default()
      )
      .map(|_| ()),
      Err(ResampleError::TooManyPlanes(_)),
    ),
    "and the same conversion planar is refused for its planes, not its pair",
  );
  FfmpegResampler::new(
    ResampleSpec::new(48_000, Sample::F32(Type::Planar), ChannelLayout::_7POINT1),
    mono_16k(),
    mediadecode_ffmpeg::FrameLimits::default(),
  )
  .expect("eight planar channels is exactly the limit");
}

#[test]
fn a_forged_frame_geometry_is_refused_before_it_can_allocate() {
  support::init_ffmpeg();
  let source = ResampleSpec::new(48_000, Sample::I16(Type::Packed), ChannelLayout::STEREO);
  let mut resampler = FfmpegResampler::new(
    source,
    mono_16k(),
    mediadecode_ffmpeg::FrameLimits::default(),
  )
  .expect("open resampler");

  // A header claiming more samples than a `c_int` can hold, over a
  // plane holding sixteen bytes. Sizing the staging allocation off the
  // header first does not merely waste memory: `av_frame_get_buffer`
  // refuses the truncated count, the failure is not checked, and the
  // unbacked frame goes on to `swr_convert_frame` — with a negative
  // sample count. The geometry has to be settled before any of that.
  let forged = stereo_frame(u32::MAX, 16, None);
  match resampler.send_frame(&forged) {
    Err(ResampleError::PlaneCount(p)) => {
      assert_eq!(
        p.found(),
        16,
        "the plane's real length is what was compared"
      );
      assert!(p.expected() > p.found());
    }
    other => panic!("expected a geometry refusal, got {other:?}"),
  }

  // A packed frame with no planes at all.
  let planeless = mediadecode_ffmpeg::OwnedAudioFrame::new(
    48_000,
    128,
    2,
    mediadecode_ffmpeg::SampleFormat::S16,
    mediadecode_ffmpeg::channel_layout_description_from_ffmpeg(&ChannelLayout::STEREO)
      .expect("a well-formed layout describes"),
    std::array::from_fn(|_| {
      mediadecode::frame::Plane::new(mediadecode_ffmpeg::FfmpegBytes::empty(), 0)
    }),
    0,
    Default::default(),
  );
  assert!(matches!(
    resampler.send_frame(&planeless),
    Err(ResampleError::PlaneCount(p)) if p.expected() == 1 && p.found() == 0,
  ));

  // And an honest frame still goes through afterwards: the refusals
  // above left nothing broken behind them.
  support::accepted(
    resampler.send_frame(&stereo_frame(480, 480 * 2 * 2, Some(0))),
    "an honest frame",
  );
}

#[test]
fn a_refused_frame_does_not_stamp_the_next_good_one() {
  support::init_ffmpeg();
  let source = ResampleSpec::new(48_000, Sample::I16(Type::Packed), ChannelLayout::STEREO);
  let mut resampler = FfmpegResampler::new(
    source,
    mono_16k(),
    mediadecode_ffmpeg::FrameLimits::default(),
  )
  .expect("open resampler");

  // The first frame the session ever sees is a forged one carrying a
  // timestamp far down the timeline. Anchoring before staging took its
  // word for where the stream starts.
  let forged = stereo_frame(4_800, 16, Some(48_000 * 60));
  assert!(matches!(
    resampler.send_frame(&forged),
    Err(ResampleError::PlaneCount(_)),
  ));

  // Then the real first frame, at zero.
  support::accepted(
    resampler.send_frame(&stereo_frame(4_800, 4_800 * 2 * 2, Some(0))),
    "an honest frame",
  );

  let mut out = empty_audio_frame();
  assert_eq!(
    resampler.receive_frame(&mut out).expect("converted output"),
    Received::Produced,
  );
  assert_eq!(
    out.pts().expect("stamped").pts(),
    0,
    "the rejected frame's timestamp must not have anchored the timeline",
  );
}

#[test]
fn a_timestamp_that_cannot_be_rescaled_is_refused_before_anything_moves() {
  support::init_ffmpeg();
  // 48 kHz in, 192 kHz out: the anchor is multiplied by four on its way
  // to the output timeline, so a timestamp near either end of `i64`
  // leaves it. Saturating hid both: the positive end reached the
  // counted timeline only after `swr` had eaten the input, and the
  // negative end landed on `i64::MIN`, which is `AV_NOPTS_VALUE` — the
  // timestamp was not clamped, it was erased.
  let source = ResampleSpec::new(48_000, Sample::I16(Type::Packed), ChannelLayout::STEREO);
  let target = ResampleSpec::new(192_000, Sample::I16(Type::Packed), ChannelLayout::MONO);
  let mut resampler =
    FfmpegResampler::new(source, target, mediadecode_ffmpeg::FrameLimits::default())
      .expect("open resampler");

  let samples = 480;
  let bytes = samples as usize * 2 * 2;
  for pts in [i64::MAX - 1, i64::MIN + 1, i64::MIN] {
    match resampler.send_frame(&stereo_frame(samples, bytes, Some(pts))) {
      Err(ResampleError::TimestampOutOfRange(p)) => {
        assert_eq!(p.pts(), pts, "the refusal names the timestamp it read");
      }
      other => panic!("expected TimestampOutOfRange for {pts}, got {other:?}"),
    }
    // Nothing was staged, nothing was converted, nothing is owed: the
    // refusal happened before `swr` saw a sample.
    assert_eq!(
      resampler.delay(),
      0,
      "a refused timestamp left input inside the filter",
    );
    let mut dst = empty_audio_frame();
    assert_eq!(
      resampler
        .receive_frame(&mut dst)
        .expect("nothing was converted, and that is not a fault"),
      Received::NeedsInput,
    );
  }

  // And the anchor never moved: the first frame the session accepts is
  // still the one that fixes where the stream starts.
  support::accepted(
    resampler.send_frame(&stereo_frame(samples, bytes, Some(0))),
    "an honest frame",
  );
  let mut out = empty_audio_frame();
  assert_eq!(
    resampler.receive_frame(&mut out).expect("converted output"),
    Received::Produced,
  );
  assert_eq!(
    out.pts().expect("stamped").pts(),
    0,
    "a refused timestamp anchored the timeline anyway",
  );
}

#[test]
fn the_output_timeline_refuses_to_overflow() {
  support::init_ffmpeg();
  // Equal rates, so the input timestamp reaches the output timeline
  // unrescaled and the arithmetic under test is the only thing that
  // can move it.
  let source = ResampleSpec::new(48_000, Sample::I16(Type::Packed), ChannelLayout::STEREO);
  let target = ResampleSpec::new(48_000, Sample::I16(Type::Packed), ChannelLayout::MONO);
  let mut resampler =
    FfmpegResampler::new(source, target, mediadecode_ffmpeg::FrameLimits::default())
      .expect("open resampler");

  let samples = 4_800;
  let frame = stereo_frame(samples, samples as usize * 2 * 2, Some(i64::MAX - 8));
  match resampler.send_frame(&frame) {
    Err(ResampleError::TimestampOverflow(p)) => {
      assert_eq!(p.pts(), i64::MAX - 8);
      assert!(p.samples() > 8, "the count that would not fit");
    }
    other => panic!("expected TimestampOverflow, got {other:?}"),
  }

  // And the refusal is transactional. The check used to live after
  // `ctx.run`, so the frame was already inside the filter when the
  // error came back: the caller was told nothing happened while the
  // filter's history had moved and an output frame had been built and
  // dropped.
  assert_eq!(
    resampler.delay(),
    0,
    "the refused frame was consumed before the timeline was checked",
  );
  let mut dst = empty_audio_frame();
  assert_eq!(
    resampler
      .receive_frame(&mut dst)
      .expect("nothing was converted, and that is not a fault"),
    Received::NeedsInput,
    "a refused frame left output ready",
  );

  // A session refused this way is still usable: the very next honest
  // frame converts, and anchors the timeline itself.
  support::accepted(
    resampler.send_frame(&stereo_frame(samples, samples as usize * 2 * 2, Some(0))),
    "the session survived the refusal",
  );
  assert_eq!(
    resampler
      .receive_frame(&mut dst)
      .expect("and converts the next frame"),
    Received::Produced,
  );
  assert_eq!(dst.pts().expect("stamped").pts(), 0);
}

#[test]
fn flush_leaves_nothing_of_the_previous_stream_behind() {
  support::init_ffmpeg();
  // A fractional ratio, so the filter really holds a tail between
  // calls: 44100 -> 16000 is 441:160.
  let source = ResampleSpec::new(44_100, Sample::I16(Type::Packed), ChannelLayout::MONO);
  let mut resampler = FfmpegResampler::new(
    source,
    mono_16k(),
    mediadecode_ffmpeg::FrameLimits::default(),
  )
  .expect("open resampler");

  let mut out = empty_audio_frame();
  for index in 0..4 {
    let pts = index * 4_410;
    support::accepted(
      resampler.send_frame(&mono_frame(44_100, 4_410, 20_000, Some(pts))),
      "send_frame",
    );
    while frame_ready(resampler.receive_frame(&mut out)) {}
  }
  assert!(
    resampler.delay() > 0,
    "the filter has to be holding something for the reset to matter",
  );

  resampler.flush().expect("flush");
  assert_eq!(
    resampler.delay(),
    0,
    "a flush that reports success cannot leave the old delay line inside swr",
  );

  // A second stream, silent, starting at zero. Anything the first one
  // left behind arrives here as the loud DC it was.
  let mut loudest = 0i16;
  let mut first_pts = None;
  for index in 0..4 {
    support::accepted(
      resampler.send_frame(&mono_frame(44_100, 4_410, 0, Some(index * 4_410))),
      "send_frame",
    );
    while frame_ready(resampler.receive_frame(&mut out)) {
      first_pts.get_or_insert(out.pts().expect("stamped").pts());
      let valid = out.nb_samples() as usize * 2;
      for chunk in out.planes()[0].data_ref().as_ref()[..valid]
        .as_chunks::<2>()
        .0
      {
        loudest = loudest.max(i16::from_le_bytes(*chunk).abs());
      }
    }
  }
  assert_eq!(first_pts, Some(0), "the new stream owns the new timeline");
  assert!(
    loudest < 100,
    "silence came out at amplitude {loudest}: the previous stream's tail survived the flush",
  );
}

#[test]
fn a_packed_spec_above_255_channels_is_refused_at_construction() {
  support::init_ffmpeg();

  // The packed sibling of the plane ceiling. `TooManyPlanes` only ever
  // looked at planar specs, because packed audio declares one plane at
  // any channel count — so a 256-channel packed spec walked through and
  // had its count clipped to 255 on the way into every output frame,
  // which then advertised a shape its bytes were not computed from.
  //
  // Refused at construction, like every other spec this backend cannot
  // carry: the target end's refusal would otherwise arrive after `swr`
  // had consumed the input.
  let wide = ResampleSpec::new(
    48_000,
    Sample::I16(Type::Packed),
    ChannelLayout::default(256),
  );
  match FfmpegResampler::new(wide, mono_16k(), mediadecode_ffmpeg::FrameLimits::default()) {
    Err(ResampleError::UnsupportedChannelCount(p)) => {
      assert_eq!(p.end(), mediadecode_ffmpeg::SpecEnd::Source);
      assert_eq!(p.channels(), 256, "the refusal reports the declared count");
      assert_eq!(p.limit(), 255);
    }
    Err(other) => panic!("expected UnsupportedChannelCount, got {other:?}"),
    Ok(_) => panic!("256 packed channels must not reach a frame's channel field"),
  }

  // The same refusal from the other end.
  let source = ResampleSpec::new(48_000, Sample::I16(Type::Packed), ChannelLayout::STEREO);
  assert!(matches!(
    FfmpegResampler::new(
      source,
      ResampleSpec::new(16_000, Sample::I16(Type::Packed), ChannelLayout::default(256)),
      mediadecode_ffmpeg::FrameLimits::default(),
    ),
    Err(ResampleError::UnsupportedChannelCount(p)) if p.end() == mediadecode_ffmpeg::SpecEnd::Target,
  ));

  // And the ceiling does not fire below itself. It cannot be asserted
  // as a *success* at 255: `swr` has its own, lower limit (`SWR_CH_MAX`
  // is 64) and answers EINVAL well before this arm's boundary. So the
  // claim this lane can make — and the one that matters — is that a
  // count inside the field's range is never attributed to *this* refusal. Every
  // ordinary count in the rest of this file constructs fine.
  let ok = ResampleSpec::new(
    48_000,
    Sample::I16(Type::Packed),
    ChannelLayout::default(255),
  );
  assert!(
    !matches!(
      FfmpegResampler::new(ok, mono_16k(), mediadecode_ffmpeg::FrameLimits::default()),
      Err(ResampleError::UnsupportedChannelCount(_)),
    ),
    "255 is inside the field's range; whatever refuses it, it is not this ceiling",
  );
}

/// **Class audit: the resampler's post-EOF orderings, pinned.**
///
/// The subtitle seam's `Ended` turned out to be reversible by a send
/// (its latch gated the receive side only). This face keeps two latches
/// — `eof`, set by `send_eof`, and the internal `drained` one that
/// closes the tail — so the same question has to be asked of both:
/// *can any submission move either backwards?*
///
/// It cannot. `send_frame` refuses before touching anything, `send_eof`
/// is idempotent and never clears, and only `flush` resets. The tail's
/// end is therefore terminal in the way the subtitle seam's was not.
#[test]
fn no_submission_can_reverse_the_end_of_a_resampler() {
  support::init_ffmpeg();
  let mut resampler = FfmpegResampler::new(
    ResampleSpec::new(48_000, Sample::I16(Type::Packed), ChannelLayout::STEREO),
    mono_16k(),
    mediadecode_ffmpeg::FrameLimits::default(),
  )
  .expect("open resampler");

  let samples = 4_800u32;
  let frame = || stereo_frame(samples, samples as usize * 2 * 2, Some(0));
  support::accepted(resampler.send_frame(&frame()), "a first frame");
  support::accepted(resampler.send_eof(), "send_eof");

  // Drain to the settled end.
  let mut out = empty_audio_frame();
  let mut ended = false;
  for _ in 0..256 {
    match resampler.receive_frame(&mut out).expect("no fault") {
      Received::Produced => {}
      Received::NeedsInput => panic!("a resampler at EOF asked for input"),
      Received::Ended => {
        ended = true;
        break;
      }
    }
  }
  assert!(ended, "the drain never reached the end of the tail");

  // **A second `send_eof` is taken and changes nothing.** Re-declaring
  // the end is not a fault — the family's line is that sending *data*
  // after the end is. The subtitle seam agrees one tier over.
  for _ in 0..3 {
    support::accepted(resampler.send_eof(), "a repeated end-of-stream");
    assert_eq!(
      resampler.receive_frame(&mut out).expect("no fault"),
      Received::Ended,
      "a repeated end-of-stream must not reopen the tail",
    );
  }

  // **A frame after the end is the fault, and it stays the fault.**
  // Never `MustDrain`: draining changes nothing, so that answer would
  // be a loop with no exit.
  for _ in 0..2 {
    assert!(
      matches!(resampler.send_frame(&frame()), Err(ResampleError::AfterEof)),
      "a frame after end-of-stream must be a usage fault",
    );
    assert_eq!(
      resampler.receive_frame(&mut out).expect("no fault"),
      Received::Ended,
      "the refused frame must not have re-armed the tail",
    );
  }

  // And `flush` is the only way back — for both latches at once.
  resampler.flush().expect("flush");
  assert_eq!(
    resampler.receive_frame(&mut out).expect("no fault"),
    Received::NeedsInput,
  );
  support::accepted(
    resampler.send_frame(&frame()),
    "flush reopened the send side",
  );
}
