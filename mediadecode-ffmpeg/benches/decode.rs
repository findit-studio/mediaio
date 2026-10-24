//! Benchmark comparing software-only decode (via `ffmpeg-next` directly,
//! since `mediadecode-ffmpeg` is hardware-only) against `mediadecode-ffmpeg`'s auto-probed
//! hardware backend on the same input file.
//!
//! Set `HWDECODE_SAMPLE_VIDEO` to a video file path. The hardware bench is
//! skipped (with a notice) when no hardware backend is available on the host.
//!
//! ```sh
//! HWDECODE_SAMPLE_VIDEO=/path/to/clip.mp4 cargo bench
//! ```

use std::{path::PathBuf, time::Duration};

use criterion::{Criterion, criterion_group, criterion_main};
use ffmpeg::{codec::Context as CodecContext, format, frame, media};
use ffmpeg_next as ffmpeg;
use mediadecode::{Received, Sent};
use mediadecode_ffmpeg::{Frame, VideoDecoder};

const SAMPLE_ENV: &str = "HWDECODE_SAMPLE_VIDEO";

fn sample_path() -> Option<PathBuf> {
  std::env::var_os(SAMPLE_ENV).map(PathBuf::from)
}

/// Decode every frame using `mediadecode-ffmpeg`'s auto-probed hardware backend.
fn decode_all_hw(path: &PathBuf) -> Result<usize, mediadecode_ffmpeg::Error> {
  let mut input = format::input(path).map_err(mediadecode_ffmpeg::Error::Ffmpeg)?;
  let stream =
    input
      .streams()
      .best(media::Type::Video)
      .ok_or(mediadecode_ffmpeg::Error::Ffmpeg(
        ffmpeg::Error::StreamNotFound,
      ))?;
  let stream_index = stream.index();

  let mut decoder = VideoDecoder::open(stream.parameters())?;
  let mut frame = Frame::empty()?;
  let mut count = 0_usize;

  let mut drain =
    |decoder: &mut VideoDecoder, count: &mut usize| -> Result<(), mediadecode_ffmpeg::Error> {
      loop {
        match decoder.receive_frame(&mut frame) {
          Ok(Received::Produced) => *count += 1,
          Ok(Received::NeedsInput | Received::Ended) => return Ok(()),
          Err(e) => return Err(e),
        }
      }
    };

  for (s, packet) in input.packets() {
    if s.index() != stream_index {
      continue;
    }

    loop {
      // SAFETY: `packets()` reads each packet with `av_read_frame`, so libavformat wrote it,
      // and nothing writes through it while the decoder may still read it.
      match unsafe { decoder.send_av_packet(&packet) } {
        Ok(Sent::Accepted) => break,
        // Back pressure, named: drain and offer again. No errno
        // to decode, and no second offer needed to find out.
        Ok(Sent::MustDrain) => {
          drain(&mut decoder, &mut count)?;
        }
        Err(e) => return Err(e),
      }
    }

    drain(&mut decoder, &mut count)?;
  }

  loop {
    match decoder.send_eof() {
      Ok(Sent::Accepted) => break,
      // Back pressure, named: drain and offer again. No errno
      // to decode, and no second offer needed to find out.
      Ok(Sent::MustDrain) => {
        drain(&mut decoder, &mut count)?;
      }
      Err(e) => return Err(e),
    }
  }
  drain(&mut decoder, &mut count)?;
  Ok(count)
}

/// Decode every frame using a plain software `ffmpeg-next` decoder. Used as
/// the SW baseline since `mediadecode-ffmpeg` no longer exposes a software backend.
fn decode_all_sw(path: &PathBuf) -> Result<usize, ffmpeg::Error> {
  let mut input = format::input(path)?;
  let stream = input
    .streams()
    .best(media::Type::Video)
    .ok_or(ffmpeg::Error::StreamNotFound)?;
  let stream_index = stream.index();
  let mut decoder = CodecContext::from_parameters(stream.parameters())?
    .decoder()
    .video()?;

  let mut frame = frame::Video::empty();
  let mut count = 0_usize;

  let mut drain =
    |decoder: &mut ffmpeg::decoder::Video, count: &mut usize| -> Result<(), ffmpeg::Error> {
      loop {
        match decoder.receive_frame(&mut frame) {
          Ok(()) => *count += 1,
          Err(ffmpeg::Error::Other { errno }) if errno == ffmpeg::error::EAGAIN => return Ok(()),
          Err(ffmpeg::Error::Eof) => return Ok(()),
          Err(e) => return Err(e),
        }
      }
    };

  for (s, packet) in input.packets() {
    if s.index() != stream_index {
      continue;
    }

    loop {
      match decoder.send_packet(&packet) {
        Ok(()) => {
          drain(&mut decoder, &mut count)?;
          break;
        }
        Err(ffmpeg::Error::Other { errno }) if errno == ffmpeg::error::EAGAIN => {
          drain(&mut decoder, &mut count)?;
        }
        Err(e) => return Err(e),
      }
    }
  }

  loop {
    match decoder.send_eof() {
      Ok(()) => {
        drain(&mut decoder, &mut count)?;
        break;
      }
      Err(ffmpeg::Error::Other { errno }) if errno == ffmpeg::error::EAGAIN => {
        drain(&mut decoder, &mut count)?;
      }
      Err(e) => return Err(e),
    }
  }
  Ok(count)
}

fn bench_decode(c: &mut Criterion) {
  ffmpeg::init().expect("ffmpeg init");

  let Some(path) = sample_path() else {
    eprintln!("skipping benches: set {SAMPLE_ENV} to a video file path");
    return;
  };

  // Probe by decoding one frame so the probe collapses to the backend that
  // actually produced output. None means no HW backend is available — we
  // skip the HW arm and bench SW only.
  let probed_backend = {
    let mut input = format::input(&path).expect("open input");
    let stream = input
      .streams()
      .best(media::Type::Video)
      .expect("video stream");
    let stream_index = stream.index();
    match VideoDecoder::open(stream.parameters()) {
      Ok(mut dec) => {
        let mut frame = Frame::empty().expect("alloc probe frame");
        // FFmpeg's send/receive contract: `send_packet` may return
        // EAGAIN if the decoder's internal queue is full; the caller
        // must drain `receive_frame` before retrying. Streams with deep
        // B-frame buffering can hit this on probe-window inputs.
        'probe: for (s, packet) in input.packets() {
          if s.index() != stream_index {
            continue;
          }
          // Loop until the packet is accepted. On EAGAIN, drain one
          // frame (which either completes the probe or frees queue
          // space), then retry send_packet.
          loop {
            // SAFETY: `packets()` reads each packet with `av_read_frame`, so libavformat wrote
            // it, and nothing writes through it while the decoder may still read it.
            match unsafe { dec.send_av_packet(&packet) } {
              Ok(Sent::Accepted) => break,
              Ok(Sent::MustDrain) => {
                match dec.receive_frame(&mut frame) {
                  Ok(Received::Produced) => break 'probe,
                  // Defensive: per FFmpeg's send/receive contract, a
                  // decoder that asked to be drained has something to
                  // give. Retry the send anyway rather than spinning.
                  Ok(Received::NeedsInput) => {}
                  Ok(Received::Ended) => break 'probe,
                  Err(e) => panic!("probe receive_frame (drain): {e}"),
                }
              }
              Err(e) => panic!("probe send_packet: {e}"),
            }
          }
          match dec.receive_frame(&mut frame) {
            Ok(Received::Produced) => break 'probe,
            Ok(Received::NeedsInput) => continue,
            Ok(Received::Ended) => break 'probe,
            Err(e) => panic!("probe receive_frame: {e}"),
          }
        }
        Some(dec.backend())
      }
      Err(mediadecode_ffmpeg::Error::ProbeExhausted(_)) => None,
      Err(e) => panic!("mediadecode-ffmpeg HW probe: {e}"),
    }
  };
  match probed_backend {
    Some(b) => eprintln!("auto-probe settled on backend: {b:?}"),
    None => eprintln!("no hardware backend available — hardware bench will be skipped"),
  }

  let mut group = c.benchmark_group("decode");
  group.measurement_time(Duration::from_secs(15));
  group.sample_size(20);

  group.bench_function("software", |b| {
    b.iter(|| decode_all_sw(&path).expect("software decode"))
  });

  if probed_backend.is_some() {
    group.bench_function("hardware", |b| {
      b.iter(|| {
        let n = decode_all_hw(&path).expect("hardware decode");
        std::hint::black_box(n);
      })
    });
  }

  group.finish();
}

criterion_group!(benches, bench_decode);
criterion_main!(benches);
