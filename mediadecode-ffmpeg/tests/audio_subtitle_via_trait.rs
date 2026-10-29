//! Compile-time + runtime checks that `FfmpegAudioStreamDecoder` and
//! `FfmpegSubtitleStreamDecoder` reach through `mediadecode`'s trait
//! surface using this crate's **safe** public API. Same shape as
//! `tests/decode_via_trait.rs`, for audio + subtitle.

use ffmpeg_next as ffmpeg;
use mediadecode::{
  Received, Sent, Timebase,
  decoder::{AudioStreamDecoder, SubtitleDecoder},
  subtitle::SubtitlePayload,
};
// The owned family under the names this suite was written with — the
// bare aliases mean the view lane now. Import block only; the
// assertions below are unchanged.
use mediadecode_ffmpeg::{
  DecoderLimits, Ffmpeg, FfmpegBytes, FfmpegOwnedAudioStreamDecoder as FfmpegAudioStreamDecoder,
  FfmpegOwnedSubtitleStreamDecoder as FfmpegSubtitleStreamDecoder, OwnedAudioFrame as AudioFrame,
  SubtitleDecodeError, empty_owned_audio_frame as empty_audio_frame,
  empty_owned_subtitle_frame as empty_subtitle_frame,
};
use std::num::NonZeroI32;

#[test]
fn ffmpeg_audio_decoder_implements_trait() {
  fn _accepts_audio<D>(_: D)
  where
    D: AudioStreamDecoder<Adapter = Ffmpeg, Buffer = FfmpegBytes>,
  {
  }
  fn _check_audio_at_compile_time() {
    let opt: Option<FfmpegAudioStreamDecoder> = None;
    if let Some(d) = opt {
      _accepts_audio(d);
    }
  }
}

#[test]
fn ffmpeg_subtitle_decoder_implements_trait() {
  fn _accepts_subtitle<D>(_: D)
  where
    D: SubtitleDecoder<Adapter = Ffmpeg, Buffer = FfmpegBytes>,
  {
  }
  fn _check_subtitle_at_compile_time() {
    let opt: Option<FfmpegSubtitleStreamDecoder> = None;
    if let Some(d) = opt {
      _accepts_subtitle(d);
    }
  }
}

const AUDIO_SAMPLE_ENV: &str = "MEDIADECODE_SAMPLE_AUDIO";
const SUBTITLE_SAMPLE_ENV: &str = "MEDIADECODE_SAMPLE_SUBTITLE";

#[test]
#[ignore = "requires MEDIADECODE_SAMPLE_AUDIO env var pointing at an audio file (or container with an audio track)"]
fn decode_one_audio_frame_through_trait() {
  let path =
    std::env::var_os(AUDIO_SAMPLE_ENV).unwrap_or_else(|| panic!("{AUDIO_SAMPLE_ENV} not set"));

  ffmpeg::init().expect("ffmpeg init");

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

  let mut decoder =
    FfmpegAudioStreamDecoder::open(stream.parameters(), time_base, DecoderLimits::default())
      .expect("open audio decoder");

  let mut dst: AudioFrame = empty_audio_frame();
  let mut got_frame = false;

  for (s, av_packet) in input.packets() {
    if s.index() != stream_index {
      continue;
    }
    // SAFETY: `packets()` reads each packet with `av_read_frame`, so libavformat wrote its
    // payload, and it is live for the call.
    let pkt = match unsafe {
      mediadecode_ffmpeg::boundary::owned_audio_packet_from_ffmpeg_in(
        &av_packet,
        mediadecode::Timebase::default(),
        mediadecode_ffmpeg::PacketLimits::default(),
      )
    }
    .expect("a wrappable payload")
    {
      Some(p) => p,
      None => continue,
    };
    assert_eq!(
      decoder.send_packet(&pkt).expect("audio send_packet"),
      Sent::Accepted,
      "these fixtures feed a session this loop has just drained",
    );
    match decoder.receive_frame(&mut dst) {
      Ok(Received::Produced) => {
        eprintln!(
          "audio frame: rate={}Hz samples={} channels={} format={:?}",
          dst.sample_rate(),
          dst.nb_samples(),
          dst.channel_count(),
          dst.sample_format(),
        );
        assert!(dst.sample_rate() > 0);
        assert!(dst.nb_samples() > 0);
        assert!(dst.channel_count() > 0);
        got_frame = true;
        break;
      }
      Ok(Received::NeedsInput) => continue,
      Ok(Received::Ended) => break,
      Err(e) => panic!("audio receive_frame: {e}"),
    }
  }

  assert!(
    got_frame,
    "no audio frame delivered through the trait surface"
  );
}

#[test]
#[ignore = "requires MEDIADECODE_SAMPLE_SUBTITLE env var pointing at a container with a subtitle track"]
fn decode_one_subtitle_through_trait() {
  let path = std::env::var_os(SUBTITLE_SAMPLE_ENV)
    .unwrap_or_else(|| panic!("{SUBTITLE_SAMPLE_ENV} not set"));

  ffmpeg::init().expect("ffmpeg init");

  let mut input = ffmpeg::format::input(&path).expect("open input");
  let stream = input
    .streams()
    .best(ffmpeg::media::Type::Subtitle)
    .expect("subtitle stream");
  let stream_index = stream.index();
  let stream_tb = stream.time_base();
  let time_base = Timebase::new(
    stream_tb.numerator(),
    NonZeroI32::new(stream_tb.denominator().max(1)).expect("non-zero den"),
  );

  let mut decoder =
    FfmpegSubtitleStreamDecoder::open(stream.parameters(), time_base, DecoderLimits::default())
      .expect("open subtitle decoder");

  let mut dst = empty_subtitle_frame();
  let mut got_frame = false;

  for (s, av_packet) in input.packets() {
    if s.index() != stream_index {
      continue;
    }
    // SAFETY: `packets()` reads each packet with `av_read_frame`, so libavformat wrote its
    // payload, and it is live for the call.
    let pkt = match unsafe {
      mediadecode_ffmpeg::boundary::owned_subtitle_packet_from_ffmpeg_in(
        &av_packet,
        mediadecode::Timebase::default(),
        mediadecode_ffmpeg::PacketLimits::default(),
      )
    }
    .expect("a wrappable payload")
    {
      Some(p) => p,
      None => continue,
    };
    assert_eq!(
      decoder.send_packet(&pkt).expect("subtitle send_packet"),
      Sent::Accepted,
    );
    match decoder.receive_frame(&mut dst) {
      Ok(Received::Produced) => {
        match dst.payload() {
          SubtitlePayload::Text(p) => {
            let bytes = p.text().as_ref().to_vec();
            let s = std::string::String::from_utf8_lossy(&bytes);
            eprintln!("subtitle text: {s:?}");
            assert!(!s.is_empty(), "decoded subtitle text was empty");
          }
          SubtitlePayload::Bitmap(p) => {
            eprintln!("subtitle bitmap regions: {}", p.regions().len());
            assert!(!p.regions().is_empty());
          }
        }
        got_frame = true;
        break;
      }
      Ok(Received::NeedsInput) => continue,
      Ok(Received::Ended) => break,
      Err(e) => panic!("subtitle receive_frame: {e}"),
    }
  }

  assert!(got_frame, "no subtitle delivered through the trait surface");
}

/// **The subtitle half of the spin-forever regression.**
///
/// `send_eof` on this backend forwards nothing to libavcodec — the
/// legacy `decode()` API buffers no tail — so before the reform a
/// drained subtitle session and one still waiting for packets were
/// literally the same answer: `NoFrameReady`, forever. A caller
/// draining to the end of a subtitle track therefore had no terminating
/// condition at all, which is why the arm is gone and the two states
/// have separate words.
///
/// No sample file is needed: the states under test are the session's,
/// not any codec's.
#[test]
fn a_subtitle_session_that_was_told_the_stream_ended_says_so() {
  ffmpeg::init().expect("ffmpeg init");

  let mut parameters = ffmpeg::codec::Parameters::new();
  // SAFETY: `parameters` owns a live, zeroed `AVCodecParameters`; both
  // fields are plain scalars and `SUBRIP` is a text subtitle codec that
  // opens with no extradata.
  unsafe {
    let raw = parameters.as_mut_ptr();
    (*raw).codec_type = ffmpeg::ffi::AVMediaType::AVMEDIA_TYPE_SUBTITLE;
    (*raw).codec_id = ffmpeg::ffi::AVCodecID::AV_CODEC_ID_SUBRIP;
  }

  let mut decoder =
    FfmpegSubtitleStreamDecoder::open(parameters, Timebase::default(), DecoderLimits::default())
      .expect("open a subrip decoder");

  let mut dst = empty_subtitle_frame();

  // Open session, nothing held: the caller is being asked for input.
  assert_eq!(
    decoder.receive_frame(&mut dst).expect("no fault"),
    Received::NeedsInput,
  );

  assert_eq!(decoder.send_eof().expect("send_eof"), Sent::Accepted);

  // And now the same empty slot means the opposite thing — which is the
  // whole point. A drain loop stops here instead of asking forever.
  for _ in 0..4 {
    assert_eq!(
      decoder.receive_frame(&mut dst).expect("no fault"),
      Received::Ended,
      "a session told the stream ended must not go back to asking for input",
    );
  }

  // `flush` reopens it — the end was a session state, not a death.
  decoder.flush().expect("flush");
  assert_eq!(
    decoder.receive_frame(&mut dst).expect("no fault"),
    Received::NeedsInput,
  );
}

/// Opens a text-subtitle decoder with no fixture file. The states under
/// test are the session's, not any container's.
fn subrip_decoder() -> FfmpegSubtitleStreamDecoder {
  subrip_decoder_in(Timebase::default())
}

/// [`subrip_decoder`], with the stream timebase the decoder is opened
/// against named — which is what reaches `AVCodecContext.pkt_timebase`.
fn subrip_decoder_in(time_base: Timebase) -> FfmpegSubtitleStreamDecoder {
  ffmpeg::init().expect("ffmpeg init");
  let mut parameters = ffmpeg::codec::Parameters::new();
  // SAFETY: `parameters` owns a live, zeroed `AVCodecParameters`; both
  // fields are plain scalars and `SUBRIP` is a text subtitle codec that
  // opens with no extradata.
  unsafe {
    let raw = parameters.as_mut_ptr();
    (*raw).codec_type = ffmpeg::ffi::AVMediaType::AVMEDIA_TYPE_SUBTITLE;
    (*raw).codec_id = ffmpeg::ffi::AVCodecID::AV_CODEC_ID_SUBRIP;
  }
  FfmpegSubtitleStreamDecoder::open(parameters, time_base, DecoderLimits::default())
    .expect("open a subrip decoder")
}

/// **A decoded cue names the right instant, in the timebase FFmpeg
/// defines for it.**
///
/// Two defects met here, and one lane catches both:
///
/// 1. The shared codec-context builder never wrote
///    `AVCodecContext.pkt_timebase`. libavcodec's generic subtitle path
///    fills `AVSubtitle.pts` by rescaling the packet's PTS out of that
///    field, and skips it entirely when the field is unset — so before
///    the fix this cue came back with **no timestamp at all**, and the
///    `expect` below is what proves the field now reaches libavcodec.
/// 2. `AVSubtitle.pts` is in `AV_TIME_BASE` units — microseconds — and
///    the converter labelled it with the *stream's* timebase without
///    rescaling. A cue five seconds in, on this 1/1000 stream, reported
///    itself as 5,000,000 over 1/1000: five thousand seconds.
///
/// The instant assertion is the one that matters to a consumer;
/// `Timestamp` compares by *when*, so it would hold under either
/// labelling that was arithmetically honest. The label assertion
/// beside it pins which of the two honest answers this crate gives.
#[test]
fn a_decoded_cue_is_labelled_in_microseconds_and_names_the_right_instant() {
  let milliseconds = Timebase::new(1, NonZeroI32::new(1_000).expect("non-zero"));
  let mut decoder = subrip_decoder_in(milliseconds);

  // Five seconds in, counted in the stream's own milliseconds.
  let packet = cue_packet().with_pts(Some(mediadecode::Timestamp::new(5_000, milliseconds)));
  assert_eq!(decoder.send_packet(&packet).expect("send"), Sent::Accepted);

  let mut dst = empty_subtitle_frame();
  assert_eq!(
    decoder.receive_frame(&mut dst).expect("receive"),
    Received::Produced,
  );

  let pts = dst
    .pts()
    .expect("libavcodec fills AVSubtitle.pts only once pkt_timebase is set");
  assert_eq!(
    pts,
    mediadecode::Timestamp::new(5, Timebase::SECONDS),
    "the cue is five seconds in, however it is labelled",
  );
  assert_eq!(
    pts.timebase(),
    Timebase::MICROS,
    "AVSubtitle.pts is in AV_TIME_BASE units, and the label says so rather than borrowing the \
     stream's ruler",
  );
  assert_eq!(pts.pts(), 5_000_000);
}

fn cue_packet() -> mediadecode_ffmpeg::OwnedSubtitlePacket {
  mediadecode::packet::SubtitlePacket::new(
    mediadecode_ffmpeg::FfmpegBytes::try_copy_from_slice(b"hello world").expect("a test payload"),
    mediadecode_ffmpeg::extras::SubtitlePacketExtra::default(),
  )
}

/// **The ordinary rhythm, end to end, on the surface that used to have
/// no vocabulary for any of it.**
///
/// `avcodec_decode_subtitle2` produces its cue inline and cannot queue a
/// second, so a send under a held cue has to be refused. It used to be
/// refused as `SubtitleDecodeError::FramePending` — a fault-shaped value
/// that left a generic caller choosing between giving up and guessing,
/// and the guess that survived was the two-offer rule. The discipline is
/// unchanged and its spelling is now honest: nothing was consumed, so
/// drain and offer again.
#[test]
fn a_subtitle_session_answers_the_ordinary_rhythm() {
  let mut decoder = subrip_decoder();
  let packet = cue_packet();
  let mut dst = empty_subtitle_frame();

  // Empty slot: the packet goes in.
  assert_eq!(
    decoder.send_packet(&packet).expect("no fault"),
    Sent::Accepted,
  );

  // Slot full. **Twice**, to pin that the answer is a state and not a
  // one-shot complaint that clears itself.
  for _ in 0..2 {
    assert_eq!(
      decoder.send_packet(&packet).expect("no fault"),
      Sent::MustDrain,
      "a send under a held cue must be told to drain, not refused as a fault",
    );
  }

  // Draining is the escape, exactly as the old error arm's docs said.
  assert_eq!(
    decoder.receive_frame(&mut dst).expect("no fault"),
    Received::Produced
  );
  assert_eq!(
    decoder.receive_frame(&mut dst).expect("no fault"),
    Received::NeedsInput
  );

  // And the same packet — never consumed — is taken now.
  assert_eq!(
    decoder.send_packet(&packet).expect("no fault"),
    Sent::Accepted,
  );

  // `send_eof` is always taken, including under a held cue: the end is a
  // fact about the input side, and the held cue is still delivered.
  assert_eq!(decoder.send_eof().expect("no fault"), Sent::Accepted);
  assert_eq!(
    decoder.receive_frame(&mut dst).expect("no fault"),
    Received::Produced
  );
  assert_eq!(
    decoder.receive_frame(&mut dst).expect("no fault"),
    Received::Ended
  );
}

/// **One loop, both faces, no backend type named.**
///
/// The trait-generic feeder the send-side reform makes writable: it
/// offers until the session takes the packet, draining whenever told to,
/// and `?` gives up only on a fault. Before the reform this could not be
/// written — "drain me first" and "this packet is damaged" were both
/// `Err` on a `Self::Error` the bound says nothing about — so consumers
/// offered every packet twice and treated the second failure as real.
fn feed<D: SubtitleDecoder>(
  decoder: &mut D,
  packet: &mediadecode::packet::SubtitlePacket<
    <D::Adapter as mediadecode::adapter::SubtitleAdapter>::PacketExtra,
    D::Buffer,
  >,
  dst: &mut mediadecode::frame::SubtitleFrame<
    <D::Adapter as mediadecode::adapter::SubtitleAdapter>::FrameExtra,
    D::Buffer,
  >,
  cues: &mut u32,
  offers: &mut u32,
) -> Result<(), D::Error> {
  loop {
    *offers += 1;
    match decoder.send_packet(packet)? {
      Sent::Accepted => return Ok(()),
      Sent::MustDrain => {
        while let Received::Produced = decoder.receive_frame(dst)? {
          *cues += 1;
        }
      }
    }
  }
}

#[test]
fn the_feeder_loop_is_writable_against_the_trait_alone() {
  let mut decoder = subrip_decoder();
  let packet = cue_packet();
  let mut dst = empty_subtitle_frame();
  let (mut cues, mut offers) = (0, 0);

  feed(&mut decoder, &packet, &mut dst, &mut cues, &mut offers).expect("no fault");
  assert_eq!(
    (offers, cues),
    (1, 0),
    "the first offer lands on an empty slot"
  );

  feed(&mut decoder, &packet, &mut dst, &mut cues, &mut offers).expect("no fault");
  assert_eq!(
    (offers, cues),
    (3, 1),
    "one refused offer, one drained cue, one accepted offer",
  );
}

/// **Regression: a terminal `Ended` must not be reversible without
/// `flush`. Empty-slot ordering.**
///
/// The subtitle seam is hand-rolled around `avcodec_decode_subtitle2`,
/// which has no send/receive state machine — it decodes whatever it is
/// handed, every time. Every other decoder in the family gets this
/// refusal from its substrate (`avcodec_send_packet` answers
/// `AVERROR_EOF`; the WebCodecs decoder tracks its own resolved flush),
/// so this session's `eof` latch is the only thing that knows.
///
/// It used to gate the *receive* side alone. A caller that observed
/// `Received::Ended` and then sent one more packet got `Sent::Accepted`,
/// a cue held in its output, and a `Received::Produced` on the next poll — the end of
/// a stream un-ended, with no `flush` anywhere in the story.
#[test]
fn a_packet_after_eof_is_refused_on_an_empty_slot() {
  let mut decoder = subrip_decoder();
  let packet = cue_packet();
  let mut dst = empty_subtitle_frame();

  assert_eq!(decoder.send_eof().expect("no fault"), Sent::Accepted);
  assert_eq!(
    decoder.receive_frame(&mut dst).expect("no fault"),
    Received::Ended,
  );

  // The reversal, refused. Twice, to pin that it is a state and not a
  // one-shot complaint.
  for _ in 0..2 {
    assert!(
      matches!(
        decoder.send_packet(&packet),
        Err(SubtitleDecodeError::AfterEof)
      ),
      "a packet after end-of-stream must be a fault, never accepted",
    );
  }

  // And the end held: nothing was decoded into the slot.
  assert_eq!(
    decoder.receive_frame(&mut dst).expect("no fault"),
    Received::Ended,
    "the refused packet must not have filled the slot",
  );

  // `flush` is the only way back, and it really is a way back.
  decoder.flush().expect("flush");
  assert_eq!(
    decoder.receive_frame(&mut dst).expect("no fault"),
    Received::NeedsInput,
    "flush must retract the end it declared",
  );
  assert_eq!(
    decoder.send_packet(&packet).expect("no fault"),
    Sent::Accepted,
    "flush must reopen the send side too",
  );
}

/// **Regression: the same reversal, held-cue ordering — and why the
/// EOF gate sits *before* the held-cue gate.**
///
/// With a cue in the slot the two gates disagree about what to answer,
/// and only one order is safe. Checking the slot first answers
/// `Sent::MustDrain` — an instruction to drain and re-offer — and the
/// drained retry would then be *accepted*, reversing the end one call
/// later instead of immediately. The fault has to win.
#[test]
fn a_packet_after_eof_is_refused_under_a_held_cue() {
  let mut decoder = subrip_decoder();
  let packet = cue_packet();
  let mut dst = empty_subtitle_frame();

  // Park a cue, then declare the end. `send_eof` is taken under a held
  // cue — the end is a fact about the input side.
  assert_eq!(
    decoder.send_packet(&packet).expect("no fault"),
    Sent::Accepted,
  );
  assert_eq!(decoder.send_eof().expect("no fault"), Sent::Accepted);

  assert!(
    matches!(
      decoder.send_packet(&packet),
      Err(SubtitleDecodeError::AfterEof)
    ),
    "with a cue held, the post-EOF offer must be the fault and not `MustDrain` — \
     draining would make the retry succeed and reverse the end",
  );

  // The held cue is still delivered: the latch ends the session, it
  // does not discard what the session already made.
  assert_eq!(
    decoder.receive_frame(&mut dst).expect("no fault"),
    Received::Produced,
    "the refusal must not have cost the parked cue",
  );
  assert_eq!(
    decoder.receive_frame(&mut dst).expect("no fault"),
    Received::Ended,
  );

  // And the drained slot does not reopen the send side.
  assert!(
    matches!(
      decoder.send_packet(&packet),
      Err(SubtitleDecodeError::AfterEof)
    ),
    "draining is not flushing",
  );
}

/// Re-declaring the end is not a fault — the other half of the family's
/// line, pinned. Sending *data* after the end is input the caller
/// believes will be decoded and will not be; a second `send_eof`
/// restates a fact already true and costs nothing. The `swresample`
/// seam one tier along agrees.
#[test]
fn a_second_end_of_stream_is_taken_without_complaint() {
  let mut decoder = subrip_decoder();
  let mut dst = empty_subtitle_frame();

  for _ in 0..3 {
    assert_eq!(decoder.send_eof().expect("no fault"), Sent::Accepted);
  }
  assert_eq!(
    decoder.receive_frame(&mut dst).expect("no fault"),
    Received::Ended,
  );
}
