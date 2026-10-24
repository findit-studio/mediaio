//! Integration test: open the auto-probed decoder against a real video file
//! and decode the first 30 frames. Skipped (with a clear message) when no
//! sample is configured.
//!
//! Set `HWDECODE_SAMPLE_VIDEO` to an absolute path to enable.

use ffmpeg::{format, media};
use ffmpeg_next as ffmpeg;
use mediadecode::{Received, Sent};
use mediadecode_ffmpeg::{Frame, VideoDecoder};

const SAMPLE_ENV: &str = "HWDECODE_SAMPLE_VIDEO";

#[test]
fn auto_open_decodes_at_least_one_frame() {
  let Some(path) = std::env::var_os(SAMPLE_ENV) else {
    eprintln!("skipping: set {SAMPLE_ENV} to a video file path to run this test");
    return;
  };

  ffmpeg::init().expect("ffmpeg init");

  let mut input = format::input(&path).expect("open input");
  let stream = input
    .streams()
    .best(media::Type::Video)
    .expect("video stream");
  let stream_index = stream.index();
  let expected_w = unsafe { (*stream.parameters().as_ptr()).width as u32 };
  let expected_h = unsafe { (*stream.parameters().as_ptr()).height as u32 };

  let mut decoder = match VideoDecoder::open(stream.parameters()) {
    Ok(d) => d,
    Err(mediadecode_ffmpeg::Error::ProbeExhausted(p)) => {
      eprintln!(
        "skipping: no hardware backend available ({} attempts)",
        p.attempts().len()
      );
      return;
    }
    Err(e) => panic!("open decoder: {e}"),
  };
  eprintln!("optimistic backend = {:?}", decoder.backend());

  assert_eq!(decoder.width(), expected_w);
  assert_eq!(decoder.height(), expected_h);

  let mut frame = Frame::empty().expect("alloc frame");
  let mut count = 0_usize;
  let target = 30_usize;

  'outer: for (s, packet) in input.packets() {
    if s.index() != stream_index {
      continue;
    }
    loop {
      // SAFETY: `packets()` reads each packet with `av_read_frame`, so libavformat wrote it
      // and its padding, and nothing writes through it while the decoder may still read it.
      match unsafe { decoder.send_av_packet(&packet) } {
        Ok(Sent::Accepted) => break,
        // The two-offer rule, retired: the decoder says which it is,
        // so this drains and re-offers the same packet instead of
        // guessing that a second failure means damage.
        Ok(Sent::MustDrain) => loop {
          match decoder.receive_frame(&mut frame) {
            Ok(Received::Produced) => {
              assert_eq!(frame.width(), expected_w);
              assert_eq!(frame.height(), expected_h);
              count += 1;
              if count >= target {
                break 'outer;
              }
            }
            Ok(Received::NeedsInput | Received::Ended) => break,
            Err(e) => panic!("receive_frame: {e}"),
          }
        },
        Err(e) => panic!("send packet: {e}"),
      }
    }
    loop {
      match decoder.receive_frame(&mut frame) {
        Ok(Received::Produced) => {
          assert_eq!(frame.width(), expected_w);
          assert_eq!(frame.height(), expected_h);
          count += 1;
          if count >= target {
            break 'outer;
          }
        }
        Ok(Received::NeedsInput | Received::Ended) => break,
        Err(e) => panic!("receive_frame: {e}"),
      }
    }
  }

  assert!(count >= 1, "expected at least 1 decoded frame, got {count}");
  eprintln!("decoded {count} frames via backend {:?}", decoder.backend());
}
