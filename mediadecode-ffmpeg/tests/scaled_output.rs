//! The `scaled_output` capability word, proved on real decode sessions
//! on this host.
//!
//! [`mediadecode::decoder::ScaledOutputCapability`]'s unit tests (in
//! `mediadecode`'s own `src/decoder.rs`) prove the *trait default*: an
//! implementor that overrides nothing answers `Unsupported` and never
//! errors. `mediadecode-ffmpeg`'s own `src/vtscale` unit tests prove the
//! stage's arithmetic — which requests are acceptable, what a scale does
//! to a sample aspect ratio, when one session is reused and when it is
//! retired — without a GPU.
//!
//! This file proves the part neither of those can: that a **real**
//! VideoToolbox session on this host, fed real H.264, delivers pictures
//! at the size a caller asked for; that the software road still refuses
//! for its own separately documented reasons; and that a refused request
//! changes nothing about the pictures a session goes on to deliver.
//!
//! See [`mediadecode_ffmpeg::CarrierVideoStreamDecoder::
//! scaled_output_capability`]'s doc comment for the per-road census
//! this answers to, and
//! [mediadecode#55](https://github.com/findit-studio/mediadecode/issues/55)
//! for the ruling behind the design.

mod support;

use mediadecode::{Received, Sent, Timebase, decoder::ScaledOutputCapability};
use mediadecode_ffmpeg::{
  Backend, DecodePath, DecoderLimits, FfmpegOwnedVideoStreamDecoder, OwnedVideoFrame, PacketLimits,
  empty_owned_video_frame, owned_video_packet_from_ffmpeg_in,
};

/// One decode session over a fixture, plus the packets that feed it.
struct Session {
  decoder: FfmpegOwnedVideoStreamDecoder,
  input: ffmpeg_next::format::context::Input,
  stream_index: usize,
  time_base: Timebase,
}

impl Session {
  /// Opens `path`'s best video stream on `path_choice`.
  fn open(path: &std::path::Path, path_choice: DecodePath) -> Self {
    let input = ffmpeg_next::format::input(path).expect("open the fixture");
    // Scoped so the stream's borrow of `input` ends before `input`
    // moves into the session below.
    let (stream_index, time_base, parameters) = {
      let stream = input
        .streams()
        .best(ffmpeg_next::media::Type::Video)
        .expect("the fixture has a video stream");
      let tb = stream.time_base();
      (
        stream.index(),
        Timebase::new(
          tb.numerator(),
          core::num::NonZeroI32::new(tb.denominator().max(1)).expect("a non-zero denominator"),
        ),
        stream.parameters(),
      )
    };
    let decoder = FfmpegOwnedVideoStreamDecoder::open_as(
      parameters,
      time_base,
      DecoderLimits::default(),
      path_choice,
    )
    .expect("open the video decoder");
    Self {
      decoder,
      input,
      stream_index,
      time_base,
    }
  }

  /// Drives the session until one frame comes out, and answers its
  /// extent. `None` when the fixture ends first.
  fn next_frame(&mut self, frame: &mut OwnedVideoFrame) -> Option<(u32, u32)> {
    use mediadecode::decoder::VideoStreamDecoder;

    while let Some((stream, av_packet)) = self.input.packets().next() {
      if stream.index() != self.stream_index {
        continue;
      }
      let Some(packet) =
        // SAFETY: `packets()` reads each packet with `av_read_frame`, so libavformat wrote its
        // payload, and it is live for the call.
        unsafe { owned_video_packet_from_ffmpeg_in(&av_packet, self.time_base, PacketLimits::default()) }
          .expect("a wrappable video payload")
      else {
        continue;
      };
      if self.decoder.send_packet(&packet).expect("send packet") != Sent::Accepted {
        continue;
      }
      if let Received::Produced = self.decoder.receive_frame(frame).expect("receive frame") {
        return Some((frame.width(), frame.height()));
      }
    }
    None
  }
}

/// The VideoToolbox road, or a printed reason why this run cannot
/// exercise it.
///
/// A session that degraded to software before its first frame proves
/// nothing about the stage, and asserting `Supported` on one would fail
/// for the wrong reason on a machine (or a CI runner) without a working
/// VideoToolbox decoder. Skipping loudly is the same welcome the corpus
/// extends to a host without the `ffmpeg` CLI.
fn hardware_session(path: &std::path::Path, path_choice: DecodePath) -> Option<Session> {
  let session = Session::open(path, path_choice);
  if !session.decoder.is_hardware() {
    eprintln!(
      "skip: this session opened on (or degraded to) software before its first frame — the \
       scaled-output stage is the VideoToolbox road's, so there is nothing here to exercise."
    );
    return None;
  }
  Some(session)
}

/// The hardware road on this host is VideoToolbox — the one backend
/// `Backend::probe_order` names here — and it **honors** a fitted
/// request: the capability says `Supported` before anything is asked,
/// the request is accepted, and every picture that comes back is the
/// requested extent rather than the stream's 1920x1080.
#[test]
fn the_videotoolbox_road_delivers_pictures_at_the_requested_size() {
  let Some(corpus) = support::Corpus::new() else {
    return;
  };
  let path = corpus.plain_h264_1080p();
  let Some(mut session) = hardware_session(&path, DecodePath::Auto) else {
    return;
  };

  assert_eq!(
    session.decoder.scaled_output_capability(),
    ScaledOutputCapability::Supported,
    "the VideoToolbox road carries the pixel-transfer stage"
  );
  assert_eq!(
    session.decoder.request_scaled_output((512, 288)),
    ScaledOutputCapability::Supported,
    "a downscale of a 1920x1080 stream is acceptable"
  );

  let mut frame = empty_owned_video_frame();
  let mut delivered = 0_u32;
  while let Some(extent) = session.next_frame(&mut frame) {
    assert_eq!(
      extent,
      (512, 288),
      "frame {delivered} came back at {extent:?} rather than the requested 512x288"
    );
    delivered += 1;
    if delivered == 8 {
      break;
    }
  }
  assert!(
    delivered > 0,
    "the fixture produced no frames at all — the assertion above proved nothing"
  );
  // Still on hardware, still saying so: the stage does not degrade the
  // session it rides.
  assert!(session.decoder.is_hardware());
  assert_eq!(
    session.decoder.scaled_output_capability(),
    ScaledOutputCapability::Supported
  );
}

/// The #53 pin and this release's scale, paired: a session pinned to
/// `DecodePath::Hardware(Backend::VideoToolbox)` — the pin a caller who
/// needs run-to-run byte determinism is told to use alongside scaled
/// output — honors the request the same way the auto-probe does.
#[test]
fn a_pinned_videotoolbox_session_honors_a_fitted_request() {
  let Some(corpus) = support::Corpus::new() else {
    return;
  };
  let path = corpus.plain_h264_1080p();
  let Some(mut session) = hardware_session(&path, DecodePath::Hardware(Backend::VideoToolbox))
  else {
    return;
  };

  assert_eq!(
    session.decoder.request_scaled_output((480, 270)),
    ScaledOutputCapability::Supported
  );
  let mut frame = empty_owned_video_frame();
  let extent = session
    .next_frame(&mut frame)
    .expect("the pinned session produced a frame");
  assert_eq!(extent, (480, 270));
  // The pin is intact: a pinned session that degraded would have
  // reported the exhaustion instead of falling through to software.
  assert!(session.decoder.is_hardware());
}

/// A request placed **after** frames have already been delivered takes
/// effect from the next one, and the pictures before it are untouched.
#[test]
fn a_mid_stream_request_takes_effect_from_the_next_frame() {
  let Some(corpus) = support::Corpus::new() else {
    return;
  };
  let path = corpus.plain_h264_1080p();
  let Some(mut session) = hardware_session(&path, DecodePath::Auto) else {
    return;
  };

  let mut frame = empty_owned_video_frame();
  let before = session
    .next_frame(&mut frame)
    .expect("a first frame, before anything is requested");
  assert_eq!(before, (1920, 1080), "nothing was requested yet");

  assert_eq!(
    session.decoder.request_scaled_output((320, 180)),
    ScaledOutputCapability::Supported
  );
  let after = session
    .next_frame(&mut frame)
    .expect("a frame after the request");
  assert_eq!(after, (320, 180), "the very next frame carries the request");
}

/// The two refusals this decoder mints itself, on the road that could
/// otherwise have honored them: a zero extent and an upscale. Neither
/// is an error — and each returns the session to full coded size, which
/// is what the trait says an `Unsupported` answer from this decoder means
/// and the only reading a caller can act on without risking a second
/// resample of an already-fitted picture.
#[test]
fn zero_and_upscale_requests_are_refused_and_return_the_session_to_full_size() {
  let Some(corpus) = support::Corpus::new() else {
    return;
  };
  let path = corpus.plain_h264_1080p();
  let Some(mut session) = hardware_session(&path, DecodePath::Auto) else {
    return;
  };

  assert_eq!(
    session.decoder.request_scaled_output((0, 288)),
    ScaledOutputCapability::Unsupported
  );
  assert_eq!(
    session.decoder.request_scaled_output((512, 0)),
    ScaledOutputCapability::Unsupported
  );
  assert_eq!(
    session.decoder.request_scaled_output((3840, 2160)),
    ScaledOutputCapability::Unsupported,
    "an upscale is refused: the stage exists to move fewer bytes, not more"
  );
  assert_eq!(
    session.decoder.request_scaled_output((1920, 1081)),
    ScaledOutputCapability::Unsupported,
    "one dimension over the coded size is over"
  );

  let mut frame = empty_owned_video_frame();
  let extent = session.next_frame(&mut frame).expect("a frame");
  assert_eq!(
    extent,
    (1920, 1080),
    "every request was refused, so the session keeps decoding at full size"
  );

  // **And a refusal after an acceptance returns the session to full
  // size**, which is what the trait says an `Unsupported` answer from
  // this decoder means. A caller acting on it resamples for itself, so a
  // session that went on fitting to the older request would have it
  // resample an already-fitted picture — irreversibly.
  assert_eq!(
    session.decoder.request_scaled_output((640, 360)),
    ScaledOutputCapability::Supported
  );
  let extent = session.next_frame(&mut frame).expect("a fitted frame");
  assert_eq!(extent, (640, 360));
  assert_eq!(
    session.decoder.request_scaled_output((4096, 4096)),
    ScaledOutputCapability::Unsupported
  );
  let extent = session.next_frame(&mut frame).expect("another frame");
  assert_eq!(
    extent,
    (1920, 1080),
    "a refused request returns the session to full coded size"
  );
}

/// The software road (a codec no hardware backend here accelerates —
/// see `Corpus::software_only_video`'s own doc for the census) refuses,
/// for its own, separately documented reasons (`lowres`'s narrow,
/// broken, detail-skipping coverage) rather than by falling through to
/// the hardware finding.
#[test]
fn software_only_session_refuses_scaled_output_and_delivers_full_size() {
  let Some(corpus) = support::Corpus::new() else {
    return;
  };
  let path = corpus.software_only_video();
  let mut session = Session::open(&path, DecodePath::Auto);

  assert_eq!(
    session.decoder.scaled_output_capability(),
    ScaledOutputCapability::Unsupported
  );
  assert_eq!(
    session.decoder.request_scaled_output((16, 16)),
    ScaledOutputCapability::Unsupported
  );

  let mut frame = empty_owned_video_frame();
  let extent = session.next_frame(&mut frame).expect("a frame");
  assert_eq!(extent, (1280, 720));
}

/// A session pinned to software refuses regardless of what the host's
/// hardware could do — the stage belongs to the hardware road, and the
/// capability word says so rather than describing the machine.
#[test]
fn a_software_pinned_session_refuses_even_on_a_videotoolbox_host() {
  let Some(corpus) = support::Corpus::new() else {
    return;
  };
  let path = corpus.plain_h264_1080p();
  let mut session = Session::open(&path, DecodePath::Software);

  assert!(session.decoder.is_software());
  assert_eq!(
    session.decoder.scaled_output_capability(),
    ScaledOutputCapability::Unsupported
  );
  assert_eq!(
    session.decoder.request_scaled_output((512, 288)),
    ScaledOutputCapability::Unsupported
  );

  let mut frame = empty_owned_video_frame();
  let extent = session.next_frame(&mut frame).expect("a frame");
  assert_eq!(extent, (1920, 1080));
}

/// A request for the stream's own extent is accepted and delivered —
/// the picture comes back at exactly the size asked for, the stage
/// simply has nothing to scale, and the capability word keeps its
/// promise because nothing about it was broken.
///
/// This is also the path that used to strand a fitted surface, its
/// pool and its transfer session: the stage retires them when the
/// accepted size changes, rather than waiting for a later frame that a
/// same-size request never brings.
#[test]
fn a_request_for_the_streams_own_extent_is_honored_and_retires_the_old_one() {
  let Some(corpus) = support::Corpus::new() else {
    return;
  };
  let path = corpus.plain_h264_1080p();
  let Some(mut session) = hardware_session(&path, DecodePath::Auto) else {
    return;
  };

  // First a real downscale, so there is something built to retire.
  assert_eq!(
    session.decoder.request_scaled_output((512, 288)),
    ScaledOutputCapability::Supported
  );
  let mut frame = empty_owned_video_frame();
  assert_eq!(session.next_frame(&mut frame), Some((512, 288)));

  // Then the stream's own extent. Not an upscale, so accepted.
  assert_eq!(
    session.decoder.request_scaled_output((1920, 1080)),
    ScaledOutputCapability::Supported
  );
  assert_eq!(
    session.next_frame(&mut frame),
    Some((1920, 1080)),
    "the picture comes back at exactly the requested extent"
  );
  assert_eq!(
    session.decoder.scaled_output_capability(),
    ScaledOutputCapability::Supported,
    "delivering the requested extent is not a broken promise"
  );
}

/// The capability word keeps its promise across a whole run of fitted
/// frames — no frame quietly reverted to full size behind a `Supported`
/// answer.
#[test]
fn the_capability_promise_holds_across_a_run_of_fitted_frames() {
  let Some(corpus) = support::Corpus::new() else {
    return;
  };
  let path = corpus.plain_h264_1080p();
  let Some(mut session) = hardware_session(&path, DecodePath::Auto) else {
    return;
  };
  assert_eq!(
    session.decoder.request_scaled_output((320, 180)),
    ScaledOutputCapability::Supported
  );
  let mut frame = empty_owned_video_frame();
  let mut delivered = 0_u32;
  while let Some(extent) = session.next_frame(&mut frame) {
    assert_eq!(extent, (320, 180));
    assert_eq!(
      session.decoder.scaled_output_capability(),
      ScaledOutputCapability::Supported,
      "frame {delivered} was fitted, so the promise must still stand"
    );
    delivered += 1;
    if delivered == 12 {
      break;
    }
  }
  assert!(delivered > 0);
}
