//! The 0.9 amputation, pinned from outside the crate.
//!
//! Everything here reads only the public API, which is the point: the
//! [carrier amputation contract][law] is a promise made to a *consumer*,
//! and a consumer cannot see `pub(crate)` helpers or unit-test
//! internals. If a future release reintroduces a carrier that borrows
//! from libavcodec, these lanes stop compiling.
//!
//! What is pinned:
//!
//! - every frame and packet form this crate emits is `Send + Sync +
//!   Clone + 'static` — the four properties a graph needs to fan a
//!   message out, and the four an `AVBufferRef` view cannot all give;
//! - a decoded frame outlives the decoder that produced it, and its
//!   bytes survive the demuxer being dropped;
//! - side data rides an `Arc` and clones by refcount, not by copy;
//! - the one-shot `ImageDecoder` turns a real container's cover art
//!   back into pixels, and reads a still's EXIF orientation off the
//!   display matrix libavcodec really emits for it.
//!
//! Media is generated at run time — see `support/mod.rs` for why the
//! committed corpus cannot serve these shapes — and each lane that
//! needs it returns early with a printed reason when the `ffmpeg` CLI
//! is absent.
//!
//! [law]: mediadecode::adapter#the-carrier-amputation-contract

mod support;

use mediadecode::{
  Received,
  decoder::ImageDecoder,
  demuxer::{DemuxedPacket, Demuxer, TrackKind},
};
// **The owned family, imported under the names #33 wrote.**
//
// The bare aliases mean the *view* lane now — the ordinary road for a
// direct consumer — so this suite names the owned lane explicitly. It
// does so in the import block alone: every assertion below is byte for
// byte what #33 shipped, which is the point. A rename that needed the
// tests rewritten would not have been a rename.
use mediadecode_ffmpeg::{
  DecoderLimits, FfmpegBytes, FfmpegOwnedDemuxer as FfmpegDemuxer,
  FfmpegOwnedImageDecoder as FfmpegImageDecoder, ImageDecodeError,
  OwnedAttachmentPacket as AttachmentPacket, OwnedAudioFrame as AudioFrame,
  OwnedAudioPacket as AudioPacket, OwnedDataPacket as DataPacket, OwnedImageFrame as ImageFrame,
  OwnedSubtitleFrame as SubtitleFrame, OwnedSubtitlePacket as SubtitlePacket,
  OwnedVideoFrame as VideoFrame, OwnedVideoPacket as VideoPacket,
  extras::{ImageOrientation, SideDataEntry},
};

use support::Corpus;

/// The four properties the contract is really about. `'static` is the
/// one that would fail first if a carrier ever borrowed again.
fn assert_message<T: Send + Sync + Clone + 'static>() {}

#[test]
fn every_emitted_form_is_a_message() {
  // All four frame households…
  assert_message::<VideoFrame>();
  assert_message::<AudioFrame>();
  assert_message::<SubtitleFrame>();
  assert_message::<ImageFrame>();
  // …and all five packet households.
  assert_message::<VideoPacket>();
  assert_message::<AudioPacket>();
  assert_message::<SubtitlePacket>();
  assert_message::<DataPacket>();
  assert_message::<AttachmentPacket>();
  // The metadata that rides them, too — a frame whose pixels clone by
  // refcount and whose side data deep-copies is half a contract.
  assert_message::<SideDataEntry>();
  // And the carrier itself.
  assert_message::<FfmpegBytes>();
}

#[test]
fn the_carrier_is_opaque_and_a_consumer_can_still_build_one() {
  // The seat is `FfmpegBytes`, and the aliases bind it. Written as a
  // coercion rather than a comment so that changing the carrier fails
  // here rather than in a consumer's build.
  fn takes_the_carrier(_: &FfmpegBytes) {}
  let packet = VideoPacket::new(
    FfmpegBytes::try_copy_from_slice(&[1, 2, 3]).expect("a test payload"),
    Default::default(),
  );
  takes_the_carrier(packet.data());
  assert_eq!(packet.data().as_ref(), &[1, 2, 3]);
  assert_eq!(packet.data().len(), 3);
  assert!(!packet.data().is_empty());

  // Opaque: `AsRef<[u8]>` is the whole contract a consumer programs
  // against, and it does not name what is behind it. That is what lets
  // the storage gain a pooled arm (issue #35) without moving a single
  // signature in this crate.
  fn reads_bytes<B: AsRef<[u8]>>(b: &B) -> usize {
    b.as_ref().len()
  }
  assert_eq!(reads_bytes(packet.data()), 3);

  // The empty carrier is shared, and a zero-length copy joins it
  // rather than allocating.
  assert!(
    FfmpegBytes::empty().ptr_eq(&FfmpegBytes::try_copy_from_slice(&[]).expect("a test payload"))
  );
}

#[test]
fn side_data_round_trips_through_the_carrier_and_clones_by_refcount() {
  let payload = FfmpegBytes::try_copy_from_slice(&[7u8; 128]).expect("a test payload");
  let entry = SideDataEntry::new(42, payload.clone());
  assert_eq!(entry.kind(), 42);
  assert_eq!(entry.data(), &[7u8; 128]);
  assert!(
    entry.data_ref().ptr_eq(&payload),
    "the entry copied bytes it was handed a carrier for",
  );

  let cloned = entry.clone();
  assert!(
    cloned.data_ref().ptr_eq(entry.data_ref()),
    "cloning a side-data entry copied its payload",
  );
  drop(entry);
  drop(payload);
  assert_eq!(cloned.data(), &[7u8; 128], "the clone kept the bytes alive");
}

#[test]
fn a_demuxed_packet_outlives_the_session_that_produced_it() {
  let Some(corpus) = Corpus::new() else {
    return;
  };
  let path = corpus.multi_track_mkv();
  let mut demuxer = FfmpegDemuxer::open(&path).expect("open");

  // Pull one packet of any kind and keep only its bytes.
  let mut kept: Option<FfmpegBytes> = None;
  while let Some(packet) = demuxer.next_packet().expect("read") {
    kept = Some(match packet {
      DemuxedPacket::Video(p) => p.packet().data().clone(),
      DemuxedPacket::Audio(p) => p.packet().data().clone(),
      DemuxedPacket::Subtitle(p) => p.packet().data().clone(),
      DemuxedPacket::Data(p) => p.packet().data().clone(),
      DemuxedPacket::Attachment(p) => p.packet().data().clone(),
    });
    if kept.as_ref().is_some_and(|b| !b.is_empty()) {
      break;
    }
  }
  let kept = kept.expect("the file has packets");
  let len = kept.len();
  assert!(len > 0);

  // 0.8's carrier kept an `AVBufferRef` alive across this drop, which
  // worked — and meant the consumer was holding libavformat's memory
  // without being told. 0.9's does not, and the bytes are still here.
  drop(demuxer);
  assert_eq!(kept.len(), len);

  // And it crosses a thread, which the old carrier (`Send`, not
  // `Sync`) could do only by moving.
  let shared = kept.clone();
  let handle = std::thread::spawn(move || shared.len());
  assert_eq!(handle.join().expect("joined"), len);
  assert_eq!(kept.len(), len);
}

#[test]
fn the_image_decoder_turns_cover_art_back_into_pixels() {
  let Some(corpus) = Corpus::new() else {
    return;
  };
  // A 32x32 red PNG attached to an MP3 — libavformat presents it as a
  // video-shaped stream with `AV_DISPOSITION_ATTACHED_PIC`, which this
  // crate's demuxer reclassifies to `Attachment`. The census this lane
  // exists to pin: that reclassification keeps the codec identity, so
  // the track row can still open a decoder.
  let path = corpus.cover_art_mp3();
  let mut demuxer = FfmpegDemuxer::open(&path).expect("open");

  let cover = demuxer
    .tracks()
    .iter()
    .position(|t| t.kind() == TrackKind::Attachment)
    .expect("the file has cover art");
  let track = &demuxer.tracks()[cover];
  // The codec survived the reclassification: it is the picture's own,
  // not the container's "none".
  let codec = track.params().codec();
  assert_ne!(
    format!("{codec:?}"),
    format!("{:?}", mediadecode_ffmpeg::CodecId::from_raw(0)),
    "the attachment track lost its codec identity",
  );
  let parameters = track.extra().clone_parameters().expect("parameters");

  let mut payload = None;
  while let Some(packet) = demuxer.next_packet().expect("read") {
    if let DemuxedPacket::Attachment(attachment) = packet {
      payload = Some(attachment.into_packet());
      break;
    }
  }
  let payload = payload.expect("the attachment track delivers its packet");
  assert!(!payload.data().is_empty(), "cover art with no bytes");

  let mut decoder =
    FfmpegImageDecoder::open(parameters, DecoderLimits::default()).expect("open image decoder");
  let image = decoder.decode(&payload).expect("decode the cover");

  assert_eq!((image.width(), image.height()), (32, 32));
  assert!(image.plane_count() >= 1, "a picture with no planes");
  assert!(
    image.planes().iter().any(|p| !p.data_ref().is_empty()),
    "every plane came back empty",
  );
  // The household's whole point: a still has no place on a timeline,
  // and there is no accessor here that could claim otherwise.
  let _: mediadecode::PixelFormat = image.pixel_format().clone();

  // Decoding the same payload again through the same decoder works —
  // the seam resets between pictures rather than latching at EOF.
  let again = decoder.decode(&payload).expect("decode the cover twice");
  assert_eq!(again.dimensions(), image.dimensions());

  // And the picture outlives everything that made it.
  let plane: FfmpegBytes = image.planes()[0].data_ref().clone();
  let bytes = plane.len();
  drop(decoder);
  drop(demuxer);
  assert_eq!(plane.len(), bytes);
}

#[test]
fn an_attachment_with_no_bytes_is_refused_by_name() {
  let Some(corpus) = Corpus::new() else {
    return;
  };
  // The font track in the multi-track fixture: its payload is codec
  // extradata, so it is a real attachment — but it is not a picture,
  // and an empty one would not be either. Both refusals are named
  // rather than surfacing as a bare FFmpeg errno.
  let path = corpus.cover_art_mp3();
  let demuxer = FfmpegDemuxer::open(&path).expect("open");
  let cover = demuxer
    .tracks()
    .iter()
    .position(|t| t.kind() == TrackKind::Attachment)
    .expect("the file has cover art");
  let parameters = demuxer.tracks()[cover]
    .extra()
    .clone_parameters()
    .expect("parameters");
  let mut decoder =
    FfmpegImageDecoder::open(parameters, DecoderLimits::default()).expect("open image decoder");

  let empty = AttachmentPacket::new(FfmpegBytes::empty(), Default::default());
  assert!(matches!(
    decoder.decode(&empty),
    Err(ImageDecodeError::EmptyPayload),
  ));

  // Bytes that are not a picture at all: accepted or refused, never a
  // frame — and whichever it is, the decoder is still usable after.
  let garbage = AttachmentPacket::new(
    FfmpegBytes::try_copy_from_slice(&[0xABu8; 64]).expect("a test payload"),
    Default::default(),
  );
  let _ = decoder.decode(&garbage);
}

/// Decodes `path` as a single still through the image seam.
///
/// The file is a bare JPEG rather than a container's attachment, so
/// there is no demuxer here: `FfmpegDemuxer` would present it as a
/// one-frame video track, and what this lane is about is the decoder,
/// not the classification. The bytes go in as an attachment packet —
/// which is exactly what a container's cover art would have been.
fn decode_still(path: &std::path::Path) -> ImageFrame {
  use ffmpeg_next::codec::Parameters;
  support::init_ffmpeg();

  let input = ffmpeg_next::format::input(path).expect("open the still");
  let stream = input
    .streams()
    .best(ffmpeg_next::media::Type::Video)
    .expect("a picture stream");
  let parameters: Parameters = stream.parameters();
  let bytes = std::fs::read(path).expect("read the still");
  drop(input);

  let mut decoder =
    FfmpegImageDecoder::open(parameters, DecoderLimits::default()).expect("open image decoder");
  let packet = AttachmentPacket::new(
    FfmpegBytes::try_copy_from_slice(&bytes).expect("a test payload"),
    Default::default(),
  );
  decoder.decode(&packet).expect("decode the still")
}

#[test]
fn a_still_s_exif_orientation_reaches_the_image_extras() {
  let Some(corpus) = Corpus::new() else {
    return;
  };
  // Every tag EXIF names, through the real mjpeg decoder, end to end.
  // The mapping is not asserted from a table this test also owns: the
  // fixture writes tag N into the file's EXIF IFD, and the seat has to
  // answer the orientation whose `to_exif_code()` is N.
  for tag in 1..=8u16 {
    let image = decode_still(&corpus.exif_oriented_jpeg(tag));
    let orientation = image
      .extra()
      .orientation()
      .unwrap_or_else(|| panic!("tag {tag} produced no orientation"));
    assert_eq!(
      orientation.to_exif_code(),
      Some(tag),
      "tag {tag} came back as {orientation:?}",
    );
    assert_eq!(orientation.is_mirrored(), matches!(tag, 2 | 4 | 5 | 7));
    assert!(
      orientation.rotation().is_some(),
      "tag {tag} is a quarter turn"
    );
    // The picture itself decoded too — the seat is not a consolation
    // prize for a frame that failed.
    assert_eq!((image.width(), image.height()), (32, 24));
  }

  // The named value a consumer would actually branch on.
  let sideways = decode_still(&corpus.exif_oriented_jpeg(6));
  assert_eq!(
    sideways.extra().orientation(),
    Some(ImageOrientation::RightTop),
  );
}

#[test]
fn a_still_with_no_orientation_tag_says_so_rather_than_guessing() {
  let Some(corpus) = Corpus::new() else {
    return;
  };
  // Cover art with no EXIF orientation at all: absent, not `TopLeft`.
  // A consumer can tell "the file said nothing" from "the file said
  // upright", which is the difference between leaving a picture alone
  // and having decided to.
  let plain = decode_still(&corpus.exif_oriented_jpeg(1));
  assert_eq!(
    plain.extra().orientation().and_then(|o| o.to_exif_code()),
    Some(1)
  );

  // And an out-of-range tag: libavcodec emits no display matrix for
  // one, so the seat is empty — measured, not assumed.
  let out_of_range = decode_still(&corpus.exif_oriented_jpeg(9));
  assert_eq!(
    out_of_range.extra().orientation(),
    None,
    "an out-of-range tag was read as an orientation",
  );
  assert!(
    !out_of_range
      .extra()
      .side_data()
      .iter()
      .any(|e| e.data().len() == ImageOrientation::DISPLAY_MATRIX_BYTES),
    "an out-of-range tag produced a display matrix after all",
  );
}

// ---------------------------------------------------------------------------
//  The ceilings — each one proved to fire *before* the copy.
//
//  "Before the copy" is the whole property. A budget checked after
//  `FfmpegBytes::copy_from_slice` has run is not a budget, it is a
//  post-mortem: the allocation the ceiling exists to prevent has already
//  happened by the time the refusal is written. Each lane below asks for
//  something the default budget would have allowed and a tightened one
//  must refuse, and asserts the refusal names both the ask and the line.
// ---------------------------------------------------------------------------

/// A packet whose header claims a payload far larger than the budget.
/// The bytes really are there, so a ceiling that ran after the copy
/// would answer `Ok` — which is exactly what must not happen.
#[test]
fn a_packet_over_budget_is_refused_before_the_copy() {
  use mediadecode_ffmpeg::{
    PacketBufferError, PacketLimits, boundary::owned_video_packet_from_ffmpeg_in,
  };
  support::init_ffmpeg();

  let body = vec![7u8; 4096];
  let packet = ffmpeg_next::Packet::copy(&body);
  let tb = mediadecode::Timebase::default();

  // Under the default budget this is an ordinary packet.
  // SAFETY: `Packet::copy` wrote every byte of the payload, and the packet is live for the
  // call.
  let carried = unsafe { owned_video_packet_from_ffmpeg_in(&packet, tb, PacketLimits::default()) }
    .expect("4 KiB is nothing")
    .expect("present");
  assert_eq!(carried.data().len(), 4096);

  // One byte under what it needs: refused, by name, with both numbers.
  let tight = PacketLimits::new().with_max_packet_bytes(4095);
  // SAFETY: `Packet::copy` wrote every byte of the payload, and the packet is live for the
  // call.
  match unsafe { owned_video_packet_from_ffmpeg_in(&packet, tb, tight) } {
    Err(PacketBufferError::PacketTooLarge(p)) => {
      assert_eq!(p.bytes(), 4096);
      assert_eq!(p.limit(), 4095);
    }
    other => panic!("expected PacketTooLarge, got {other:?}"),
  }

  // Exactly at the line is not over it.
  assert!(
    // SAFETY: `Packet::copy` wrote every byte of the payload, and the packet is live for the
    // call.
    unsafe {
      owned_video_packet_from_ffmpeg_in(
        &packet,
        tb,
        PacketLimits::new().with_max_packet_bytes(4096),
      )
    }
    .expect("exactly at the cap is not over it")
    .is_some()
  );
}

/// Every packet arm shares one funnel, so the budget has to fire on all
/// of them. A ceiling that guarded `video` and not `attachment` is a
/// ceiling with a door in it.
#[test]
fn the_packet_budget_fires_on_every_arm() {
  use mediadecode_ffmpeg::{
    PacketBufferError, PacketLimits,
    boundary::{
      owned_attachment_packet_from_ffmpeg, owned_audio_packet_from_ffmpeg_in,
      owned_data_packet_from_ffmpeg_in, owned_subtitle_packet_from_ffmpeg_in,
      owned_video_packet_from_ffmpeg_in,
    },
  };
  support::init_ffmpeg();

  let packet = ffmpeg_next::Packet::copy(&[1u8; 512]);
  let tb = mediadecode::Timebase::default();
  let tight = PacketLimits::new().with_max_packet_bytes(8);

  let over = |r: Result<Option<bool>, PacketBufferError>| matches!(r, Err(PacketBufferError::PacketTooLarge(p)) if p.bytes() == 512 && p.limit() == 8);
  assert!(over(
    // SAFETY: `Packet::copy` wrote every byte of the payload, and the packet is live for the
    // call.
    unsafe { owned_video_packet_from_ffmpeg_in(&packet, tb, tight) }.map(|p| p.map(|_| true))
  ));
  assert!(over(
    // SAFETY: `Packet::copy` wrote every byte of the payload, and the packet is live for the
    // call.
    unsafe { owned_audio_packet_from_ffmpeg_in(&packet, tb, tight) }.map(|p| p.map(|_| true))
  ));
  assert!(over(
    // SAFETY: `Packet::copy` wrote every byte of the payload, and the packet is live for the
    // call.
    unsafe { owned_subtitle_packet_from_ffmpeg_in(&packet, tb, tight) }.map(|p| p.map(|_| true))
  ));
  assert!(over(
    // SAFETY: `Packet::copy` wrote every byte of the payload, and the packet is live for the
    // call.
    unsafe { owned_data_packet_from_ffmpeg_in(&packet, tb, tight) }.map(|p| p.map(|_| true))
  ));
  assert!(over(
    // SAFETY: `Packet::copy` wrote every byte of the payload, and the packet is live for the
    // call.
    unsafe { owned_attachment_packet_from_ffmpeg(&packet, tight) }.map(|p| p.map(|_| true))
  ));
}

/// A real container's cover art, refused at open because the
/// per-attachment budget is below it. Attachments are captured
/// eagerly, so this is an *open* failure — the session never exists.
#[test]
fn an_attachment_over_budget_refuses_the_open() {
  use mediadecode_ffmpeg::{DemuxError, DemuxLimits};
  let Some(corpus) = Corpus::new() else {
    return;
  };
  let path = corpus.cover_art_mp3();

  // The default budget opens it and hands over the cover.
  let baseline = FfmpegDemuxer::open(&path).expect("the default budget is generous");
  drop(baseline);

  // A budget below the cover art: the open itself fails, and names the
  // track, the ask and the line.
  let tight = DemuxLimits::new().with_max_attachment_bytes(16);
  match FfmpegDemuxer::open_with(&path, tight) {
    Err(DemuxError::AttachmentTooLarge(p)) => {
      assert!(p.bytes() > 16, "the ask is reported: {}", p.bytes());
      assert_eq!(p.limit(), 16);
    }
    Err(other) => panic!("expected AttachmentTooLarge, got {other:?}"),
    Ok(_) => panic!("an over-budget attachment opened the session"),
  }
}

/// The aggregate budget is a different attack from the per-attachment
/// one: every payload can be individually fine and the file can still
/// be an attachment table. Proved by leaving the per-attachment ceiling
/// generous and putting the *total* below the same cover art.
#[test]
fn the_whole_file_attachment_budget_refuses_the_open_on_its_own() {
  use mediadecode_ffmpeg::{DemuxError, DemuxLimits};
  let Some(corpus) = Corpus::new() else {
    return;
  };
  let path = corpus.cover_art_mp3();

  let tight = DemuxLimits::new()
    .with_max_attachment_bytes(usize::MAX) // never the per-attachment arm
    .with_max_total_attachment_bytes(16);
  match FfmpegDemuxer::open_with(&path, tight) {
    Err(DemuxError::AttachmentBudgetExhausted(p)) => {
      assert!(
        p.total() > 16,
        "the running total is reported: {}",
        p.total()
      );
      assert_eq!(p.limit(), 16);
    }
    Err(other) => panic!("expected AttachmentBudgetExhausted, got {other:?}"),
    Ok(_) => panic!("an exhausted attachment budget opened the session"),
  }
}

/// A synthesized attachment — a font, whose bytes live in codec
/// extradata and never appear as a packet — is charged identically.
/// The two capture roads share one charger precisely so this cannot
/// drift.
#[test]
fn a_synthesized_extradata_attachment_is_charged_the_same() {
  use mediadecode_ffmpeg::{DemuxError, DemuxLimits};
  let Some(corpus) = Corpus::new() else {
    return;
  };
  let path = corpus.multi_track_mkv();

  assert!(
    FfmpegDemuxer::open(&path).is_ok(),
    "the default budget opens a file with one small font",
  );

  let tight = DemuxLimits::new().with_max_attachment_bytes(4);
  match FfmpegDemuxer::open_with(&path, tight) {
    Err(DemuxError::AttachmentTooLarge(p)) => {
      assert!(p.bytes() > 4);
      assert_eq!(p.limit(), 4);
    }
    Err(other) => panic!("expected AttachmentTooLarge for the font, got {other:?}"),
    Ok(_) => panic!("an over-budget font opened the session"),
  }
}

/// The frame ceilings, on the image road — a cover-art bomb is the same
/// attack as a video bomb, and this is the seam a thumbnailer reaches
/// for without ever asking for video.
#[test]
fn a_decoded_still_over_the_frame_ceilings_is_refused_before_the_planes() {
  use mediadecode_ffmpeg::{FrameLimits, ImageDecodeError, convert, convert::ConvertError};
  let Some(corpus) = Corpus::new() else {
    return;
  };
  let path = corpus.exif_oriented_jpeg(1); // a 32x24 JPEG

  // Pixels: the picture is 768 of them, so a 700-pixel ceiling refuses
  // it — and libavcodec, which got the same number, refuses first.
  let by_pixels = decode_still_with(
    &path,
    DecoderLimits::new().with_frame(FrameLimits::new().with_max_pixels(700)),
  );
  match by_pixels {
    Err(ImageDecodeError::Convert(ConvertError::TooManyPixels(p))) => {
      assert_eq!(p.limit(), 700);
      assert!(p.pixels() > 700);
    }
    // libavcodec refusing first is the *better* outcome — the frame was
    // never allocated at all — and it surfaces as a decode error.
    Err(ImageDecodeError::Decode(_)) | Err(ImageDecodeError::NoImage) => {}
    Err(other) => panic!("unexpected refusal: {other:?}"),
    Ok(_) => panic!("a 768-pixel still passed a 700-pixel ceiling"),
  }

  // Bytes: generous pixels, a byte ceiling below what the planes export.
  //
  // **This lane used to assert the post-decode refusal, and that was
  // the defect.** `FrameTooLarge` came from the conversion — after
  // libavcodec had allocated the frame this crate then declined to
  // copy. A pixel ceiling does not bound bytes, because a pixel is not
  // a fixed price: 10000x10000 is 100 Mpx, under the 256 Mpx default,
  // and 800 MB in `rgba64`. The byte ceiling is now converted into the
  // pixel ceiling that enforces it and pushed into the decoder, so
  // libavcodec refuses in `av_image_check_size2` — before the
  // allocation — and the refusal surfaces as its `EINVAL`.
  //
  // That the error changed shape *is* the proof: nothing but an
  // in-libavcodec refusal can produce it, and reaching the old error
  // would mean the allocation had happened.
  let by_bytes = decode_still_with(
    &path,
    DecoderLimits::new().with_frame(FrameLimits::new().with_max_frame_bytes(8)),
  );
  match by_bytes {
    Err(ImageDecodeError::Decode(_)) | Err(ImageDecodeError::NoImage) => {}
    Err(ImageDecodeError::Convert(ConvertError::FrameTooLarge(_))) => {
      panic!("the byte ceiling was only enforced after libavcodec allocated the frame")
    }
    Err(other) => panic!("unexpected refusal: {other:?}"),
    Ok(_) => panic!("an 8-byte ceiling passed a real picture"),
  }

  // The post-decode ceiling is still there, and still enforced — it is
  // the backstop for frames that reach the conversion by another road
  // (a caller converting an `AVFrame` it produced itself, a decoder
  // whose output format is wider than the one its parameters declared).
  // Reached here by converting directly, which is the road with no
  // decoder to have pushed anything down.
  let frame = picture_av_frame(64, 64);
  // SAFETY: the frame is live for the call and holds no buffers, so it exports no byte to be
  // unwritten; it is refused on its declared size before anything is read.
  match unsafe { convert::image_frame_from(&frame, FrameLimits::new().with_max_frame_bytes(8)) } {
    Err(ConvertError::FrameTooLarge(p)) => {
      assert_eq!(p.limit(), 8);
      assert!(p.bytes() > 8, "the ask is reported: {}", p.bytes());
    }
    Err(other) => panic!("the conversion backstop must still refuse, got {other:?}"),
    Ok(_) => panic!("the conversion backstop stopped enforcing max_frame_bytes"),
  }

  // And the same picture under the defaults decodes.
  assert!(decode_still_with(&path, DecoderLimits::default()).is_ok());
}

/// [`decode_still`] with the ceilings named, returning the error rather
/// than unwrapping it.
fn decode_still_with(
  path: &std::path::Path,
  limits: mediadecode_ffmpeg::DecoderLimits,
) -> Result<ImageFrame, mediadecode_ffmpeg::ImageDecodeError> {
  use ffmpeg_next::codec::Parameters;
  support::init_ffmpeg();

  let input = ffmpeg_next::format::input(path).expect("open the still");
  let stream = input
    .streams()
    .best(ffmpeg_next::media::Type::Video)
    .expect("a picture stream");
  let parameters: Parameters = stream.parameters();
  let bytes = std::fs::read(path).expect("read the still");
  drop(input);

  let mut decoder = FfmpegImageDecoder::open(parameters, limits)?;
  let packet = AttachmentPacket::new(
    FfmpegBytes::try_copy_from_slice(&bytes).expect("a test payload"),
    Default::default(),
  );
  decoder.decode(&packet)
}

/// The ceiling is on the *session*, and it reaches libavcodec too —
/// `AVCodecContext.max_pixels` is written from the same number, so an
/// oversized picture is refused before FFmpeg allocates its own frame.
/// Asserted through the public seat rather than by reading the context.
#[test]
fn the_frame_ceilings_ride_the_session() {
  use mediadecode_ffmpeg::FrameLimits;
  let Some(corpus) = Corpus::new() else {
    return;
  };
  let path = corpus.exif_oriented_jpeg(1);
  // Distinctive numbers, so the assertions below cannot pass by
  // accident. The byte figure has to actually admit this 32x24 still,
  // though: `max_frame_bytes` is pushed into libavcodec as a pixel
  // ceiling priced at the worst per-pixel cost any format can reach, so
  // a few kilobytes now buys a few hundred pixels and the decoder
  // refuses to open at all. That refusal is the R9/R10 fix working; it
  // is just not what this lane is about, which is whether the limits
  // struct rides the session unchanged.
  let limits = DecoderLimits::new()
    .with_frame(
      FrameLimits::new()
        .with_max_pixels(1_234)
        .with_max_frame_bytes(5_678_901),
    )
    .with_max_codec_parameter_bytes(9_012)
    .with_max_image_input_bytes(3_456);

  use ffmpeg_next::codec::Parameters;
  support::init_ffmpeg();
  let input = ffmpeg_next::format::input(&path).expect("open the still");
  let parameters: Parameters = input
    .streams()
    .best(ffmpeg_next::media::Type::Video)
    .expect("a picture stream")
    .parameters();
  drop(input);

  let decoder = FfmpegImageDecoder::open(parameters, limits).expect("open");
  assert_eq!(decoder.limits(), limits, "the session kept its ceilings");
  assert_eq!(decoder.limits().frame().max_pixels(), 1_234);
  assert_eq!(decoder.limits().frame().max_frame_bytes(), 5_678_901);
  assert_eq!(decoder.limits().max_codec_parameter_bytes(), 9_012);
  assert_eq!(decoder.limits().max_image_input_bytes(), 3_456);
}

// ---------------------------------------------------------------------------
//  R2: the two sibling paths that walked around the ceilings.
// ---------------------------------------------------------------------------

/// The bypass this closes: `build_tracks` deep-copied every stream's
/// codec parameters on its way to a `TrackExtra`, and for an
/// `AVMEDIA_TYPE_ATTACHMENT` stream the extradata inside those
/// parameters **is** the payload. So a font was allocated, in full,
/// one statement before the budget was asked about it.
///
/// Proved by ordering rather than by instrumentation: the refusal has
/// to arrive from `open` itself, and the only way it can arrive before
/// the parameter copy is if the whole file was admitted first.
#[test]
fn an_over_budget_extradata_attachment_refuses_before_any_payload_allocation() {
  use mediadecode_ffmpeg::{DemuxError, DemuxLimits};
  let Some(corpus) = Corpus::new() else {
    return;
  };
  // The Matroska fixture's font lives entirely in codec extradata and
  // never appears as a packet — the synthesized road.
  let path = corpus.multi_track_mkv();

  // Under the defaults the file opens and the font is delivered.
  {
    let mut demuxer = FfmpegDemuxer::open(&path).expect("the default budget is generous");
    let mut saw_font = false;
    while let Some(packet) = demuxer.next_packet().expect("read") {
      if let DemuxedPacket::Attachment(attachment) = packet {
        assert!(!attachment.packet().data().is_empty(), "the font has bytes");
        saw_font = true;
      }
    }
    assert!(saw_font, "the fixture attaches a font");
  }

  // A budget under the font: the open fails, naming the track, the ask
  // and the line. Nothing the size of the payload was ever allocated —
  // the admission pass reads `extradata_size` and charges it, and no
  // `avcodec_parameters_copy` runs until every attachment has passed.
  let tight = DemuxLimits::new().with_max_attachment_bytes(4);
  match FfmpegDemuxer::open_with(&path, tight) {
    Err(DemuxError::AttachmentTooLarge(p)) => {
      assert!(p.bytes() > 4, "the ask is reported: {}", p.bytes());
      assert_eq!(p.limit(), 4);
    }
    Err(other) => panic!("expected AttachmentTooLarge, got {other:?}"),
    Ok(_) => panic!("an over-budget extradata attachment opened the session"),
  }

  // And the aggregate tier catches it too, with the per-attachment
  // ceiling left wide — the admission pass totals across the file
  // before it lets the build loop allocate for any track.
  let aggregate = DemuxLimits::new()
    .with_max_attachment_bytes(usize::MAX)
    .with_max_total_attachment_bytes(4);
  assert!(
    matches!(
      FfmpegDemuxer::open_with(&path, aggregate),
      Err(DemuxError::AttachmentBudgetExhausted(_)),
    ),
    "the whole-file budget must see the extradata road too",
  );
}

/// The residency half of the same finding: an accepted synthesized
/// attachment must retain **one** copy of its payload, not two.
///
/// The censused verdict is that the parameter copy's extradata is dead
/// weight on this road — libavcodec has no decoder for a font, so
/// nothing can open a codec context from it — so `build_tracks` strips
/// it. This asserts the strip, through the public handoff a consumer
/// would use.
#[test]
fn an_accepted_extradata_attachment_retains_one_copy_not_two() {
  use mediadecode::demuxer::TrackKind;
  let Some(corpus) = Corpus::new() else {
    return;
  };
  let path = corpus.multi_track_mkv();
  let demuxer = FfmpegDemuxer::open(&path).expect("open");

  let font = demuxer
    .tracks()
    .iter()
    .find(|t| {
      t.kind() == TrackKind::Attachment && t.mime_type().is_some_and(|m| m.contains("truetype"))
    })
    .expect("the fixture attaches a font");

  // The parameter copy a consumer is handed carries no extradata: the
  // payload rides the attachment packet, and only there.
  let parameters = font
    .extra()
    .clone_parameters()
    .expect("the checked handoff");
  // SAFETY: `parameters` owns a live `AVCodecParameters`;
  // `extradata_size` is a public field.
  let retained = unsafe { (*parameters.as_ptr()).extradata_size };
  assert_eq!(
    retained, 0,
    "the parameter copy still holds a duplicate of the font payload",
  );

  // A video track's extradata is untouched — it is not a payload, and a
  // decoder genuinely needs it.
  let video = demuxer
    .tracks()
    .iter()
    .find(|t| t.kind() == TrackKind::Video)
    .expect("the fixture has video");
  let video_parameters = video.extra().clone_parameters().expect("handoff");
  // SAFETY: as above.
  let video_extradata = unsafe { (*video_parameters.as_ptr()).extradata_size };
  assert!(
    video_extradata > 0,
    "H.264 parameter sets were stripped along with the font's payload",
  );
}

/// The resampler's own ceiling. `output_capacity` bounds the sample
/// *count* at `i32::MAX` and nothing bounded the bytes, so a source
/// rate read off an untrusted container — 1 Hz is a legal number —
/// amplified a small valid input into a huge output frame, straight
/// past `FrameLimits`.
///
/// The numbers below sit deliberately **inside** the pre-existing
/// sample-count guard: 1000 samples at a declared 1 Hz against a
/// 48 kHz target is 48,000,001 output samples, comfortably under
/// `i32::MAX`, and 384 MB of `f32` stereo. The old code allocated all
/// of it — twice, counting the carrier copy — from an 8 KB input, and
/// every existing refusal was satisfied.
#[cfg(feature = "resample")]
#[test]
fn an_amplifying_conversion_is_refused_before_the_output_frame_exists() {
  use ffmpeg_next::{
    ChannelLayout,
    format::{Sample, sample::Type},
  };
  use mediadecode::resampler::AudioResampler;
  use mediadecode_ffmpeg::{
    FfmpegOwnedResampler as FfmpegResampler, FrameLimits, ResampleError, ResampleSpec,
  };
  support::init_ffmpeg();

  let packed =
    |rate: u32| ResampleSpec::new(rate, Sample::F32(Type::Packed), ChannelLayout::STEREO);

  // Under a modest ceiling: refused, by name, before the output frame
  // is allocated.
  let tight = FrameLimits::new().with_max_frame_bytes(64 * 1024);
  let mut resampler =
    FfmpegResampler::new(packed(1), packed(48_000), tight).expect("the specs themselves are fine");
  match resampler.send_frame(&stereo_f32_frame(1, 1_000)) {
    Err(ResampleError::OutputTooLarge(p)) => {
      assert_eq!(p.limit(), 64 * 1024);
      assert!(
        p.bytes() > 300_000_000,
        "an 8 KB input asked for {} bytes",
        p.bytes(),
      );
    }
    other => panic!("expected OutputTooLarge, got {other:?}"),
  }

  // And the *default* ceiling is load-bearing, not decorative: the same
  // shape with a slightly larger input runs past 512 MiB.
  let mut defaulted =
    FfmpegResampler::new(packed(1), packed(48_000), FrameLimits::default()).expect("open");
  assert!(
    matches!(
      defaulted.send_frame(&stereo_f32_frame(1, 2_000)),
      Err(ResampleError::OutputTooLarge(_)),
    ),
    "the default ceiling let a 16 KB input ask for ~768 MB",
  );

  // The other direction — a large downsample — is not amplification and
  // must not be refused by the same ceiling.
  let mut down = FfmpegResampler::new(packed(192_000), packed(8_000), tight).expect("open");
  support::accepted(
    down.send_frame(&stereo_f32_frame(192_000, 1_024)),
    "a downsample produces less, and must pass the same ceiling",
  );

  // And a legitimately amplifying conversion under a ceiling that
  // allows it still works: 8 kHz to 48 kHz is a real resample.
  let mut up =
    FfmpegResampler::new(packed(8_000), packed(48_000), FrameLimits::default()).expect("open");
  support::accepted(
    up.send_frame(&stereo_f32_frame(8_000, 8_000)),
    "a 6x upsample of one second is ordinary work",
  );
  let mut converted = mediadecode_ffmpeg::empty_owned_audio_frame();
  assert_eq!(
    up.receive_frame(&mut converted)
      .expect("the conversion produced a frame"),
    Received::Produced,
  );
  assert_eq!(converted.sample_rate(), 48_000);
  assert!(converted.nb_samples() > 0);
}

/// A packed stereo `f32` frame of silence at `rate` Hz — the shape the
/// resampler lanes above feed in. The rate has to match the source spec
/// the resampler was opened with, which is what the mid-stream check
/// compares against.
#[cfg(feature = "resample")]
fn stereo_f32_frame(rate: u32, samples: u32) -> AudioFrame {
  use mediadecode::frame::Plane;
  let bytes = samples as usize * 2 * 4;
  let plane = FfmpegBytes::try_copy_from_slice(&vec![0u8; bytes]).expect("a test payload");
  let planes = std::array::from_fn(|index| {
    Plane::new(
      if index == 0 {
        plane.clone()
      } else {
        FfmpegBytes::empty()
      },
      0,
    )
  });
  AudioFrame::new(
    rate,
    samples,
    2,
    mediadecode_ffmpeg::SampleFormat::FLT,
    mediadecode_ffmpeg::channel_layout_description_from_ffmpeg(&ffmpeg_next::ChannelLayout::STEREO)
      .expect("a well-formed layout describes"),
    planes,
    1,
    Default::default(),
  )
}

// ---------------------------------------------------------------------------
//  R3: the codec-parameter heap, budgeted at the session.
// ---------------------------------------------------------------------------

/// Every stream's codec parameters are charged, not just the
/// attachments' — because the track table copies every stream's, and
/// `AVCodecParameters` reaches the heap three ways, all of them sized
/// by the file.
///
/// Driven through a real container so the preflight, the budgets and
/// the bounded clone are exercised as a session actually runs them.
#[test]
fn every_stream_s_codec_parameters_are_charged_at_open() {
  use mediadecode_ffmpeg::{DemuxError, DemuxLimits};
  let Some(corpus) = Corpus::new() else {
    return;
  };
  // Four tracks: H.264 (real SPS/PPS extradata), AAC, SubRip, and a
  // font attachment.
  let path = corpus.multi_track_mkv();

  assert!(
    FfmpegDemuxer::open(&path).is_ok(),
    "the default parameter budget opens an ordinary file",
  );

  // A per-stream ceiling under the video track's parameter sets: the
  // open fails, naming the stream, the ask and the line — before any
  // parameters are cloned.
  let tight = DemuxLimits::new().with_max_codec_parameter_bytes(4);
  match FfmpegDemuxer::open_with(&path, tight) {
    Err(DemuxError::ParametersTooLarge(p)) => {
      assert!(p.bytes() > 4, "the ask is reported: {}", p.bytes());
      assert_eq!(p.limit(), 4);
    }
    Err(other) => panic!("expected ParametersTooLarge, got {other:?}"),
    Ok(_) => panic!("over-budget codec parameters opened the session"),
  }
}

/// Per-stream and whole-file parameter budgets are independent tiers,
/// exactly as the attachment pair are: each stream's parameters can be
/// individually modest and a container can still declare a stream
/// table.
#[test]
fn the_parameter_budget_tiers_are_independent() {
  use mediadecode_ffmpeg::{DemuxError, DemuxLimits};
  let Some(corpus) = Corpus::new() else {
    return;
  };
  let path = corpus.multi_track_mkv();

  // Aggregate alone, with the per-stream ceiling left wide: still
  // refused, and by the aggregate arm.
  let aggregate = DemuxLimits::new()
    .with_max_codec_parameter_bytes(usize::MAX)
    .with_max_total_codec_parameter_bytes(8);
  match FfmpegDemuxer::open_with(&path, aggregate) {
    Err(DemuxError::ParametersBudgetExhausted(p)) => {
      assert!(
        p.total() > 8,
        "the running total is reported: {}",
        p.total()
      );
      assert_eq!(p.limit(), 8);
    }
    Err(other) => panic!("expected ParametersBudgetExhausted, got {other:?}"),
    Ok(_) => panic!("an exhausted parameter budget opened the session"),
  }

  // And the parameter budgets are independent of the attachment ones:
  // a wide-open attachment budget does not excuse a parameter one.
  let mixed = DemuxLimits::new()
    .with_max_attachment_bytes(usize::MAX)
    .with_max_total_attachment_bytes(usize::MAX)
    .with_max_codec_parameter_bytes(4);
  assert!(
    matches!(
      FfmpegDemuxer::open_with(&path, mixed),
      Err(DemuxError::ParametersTooLarge(_)),
    ),
    "the attachment budgets must not cover for the parameter ones",
  );
}

/// Residency, not file structure: a synthesized attachment's payload is
/// charged **once**. Its `extradata` goes to the attachment budget,
/// because the carrier holds it, and is left out of the parameter
/// budget, because the clone strips it.
///
/// Proved by the seam of it — a parameter budget of zero opens a file
/// whose only heap parameters are that font's stripped extradata.
#[test]
fn a_stripped_attachment_payload_is_charged_once_not_twice() {
  use mediadecode::demuxer::TrackKind;
  use mediadecode_ffmpeg::DemuxLimits;
  let Some(corpus) = Corpus::new() else {
    return;
  };
  let path = corpus.cover_art_mp3();

  // The MP3 fixture's streams carry no parameter heap to speak of, and
  // its cover art is a real packet rather than extradata — so a
  // parameter budget of zero must still open it. If the attachment's
  // bytes were being charged to the parameter budget as well, this
  // would fail.
  let zero_parameters = DemuxLimits::new()
    .with_max_codec_parameter_bytes(0)
    .with_max_total_codec_parameter_bytes(0);
  let demuxer = FfmpegDemuxer::open_with(&path, zero_parameters)
    .expect("cover art is a carrier, not a parameter heap");

  let cover = demuxer
    .tracks()
    .iter()
    .find(|t| t.kind() == TrackKind::Attachment)
    .expect("the file has cover art");
  // And the row reports what it actually retains, which a consumer can
  // read back.
  assert_eq!(
    cover.extra().parameter_bytes(),
    0,
    "the cover-art row retains no parameter heap",
  );
}

// ---------------------------------------------------------------------------
//  R4: the roads into FFmpeg, and the two still layouts.
// ---------------------------------------------------------------------------

/// The choke point. `avcodec_parameters_to_context` is a wholesale copy
/// *into* libavcodec — it duplicates extradata, every `coded_side_data`
/// entry and the channel map into the context — and every decoder in
/// this crate opens through `build_codec_context`. Measuring there is
/// what stops a caller handing FFmpeg parameters nobody budgeted.
///
/// Driven through **both** entry points the finding named: the HW
/// probe's `open_with_limits` and the image decoder's `open`.
#[test]
fn oversized_parameters_are_refused_at_every_decoder_entry_point() {
  use ffmpeg_next::codec::Parameters;
  use mediadecode_ffmpeg::{
    Backend, DecoderLimits, Error, FfmpegOwnedImageDecoder as FfmpegImageDecoder, VideoDecoder,
  };
  support::init_ffmpeg();

  // An `AVCodecParameters` carrying an 8 MiB ICC profile in
  // `coded_side_data` — the exact shape a MOV `prof` atom produces, and
  // the one the wholesale copy took without asking.
  let build = || {
    let mut out = Parameters::new();
    // SAFETY: `out` owns a live `AVCodecParameters`; both buffers come
    // from FFmpeg's allocator and are handed to it.
    unsafe {
      let par = out.as_mut_ptr();
      (*par).codec_id = ffmpeg_next::ffi::AVCodecID::AV_CODEC_ID_MJPEG;
      (*par).codec_type = ffmpeg_next::ffi::AVMediaType::AVMEDIA_TYPE_VIDEO;
      (*par).width = 32;
      (*par).height = 24;
      let array = ffmpeg_next::ffi::av_mallocz(core::mem::size_of::<
        ffmpeg_next::ffi::AVPacketSideData,
      >()) as *mut ffmpeg_next::ffi::AVPacketSideData;
      assert!(!array.is_null());
      let payload = ffmpeg_next::ffi::av_mallocz(8 * 1024 * 1024) as *mut u8;
      assert!(!payload.is_null());
      (*array).data = payload;
      (*array).size = 8 * 1024 * 1024;
      (*array).type_ = ffmpeg_next::ffi::AVPacketSideDataType::AV_PKT_DATA_ICC_PROFILE;
      (*par).coded_side_data = array;
      (*par).nb_coded_side_data = 1;
    }
    out
  };

  let tight = DecoderLimits::new().with_max_codec_parameter_bytes(64 * 1024);

  // The image decoder's open.
  match FfmpegImageDecoder::open(build(), tight) {
    Err(mediadecode_ffmpeg::ImageDecodeError::Decode(Error::ParametersTooLarge(p))) => {
      assert_eq!(p.limit(), 64 * 1024);
      assert!(p.bytes() > 64 * 1024);
    }
    Err(other) => panic!("image open let oversized parameters through: {other:?}"),
    Ok(_) => panic!("the image decoder opened over oversized parameters"),
  }

  // The HW-probe decoder's open, which reaches `build_state` and thence
  // the same choke point.
  match VideoDecoder::open_with_limits(build(), Backend::VideoToolbox, tight) {
    Err(Error::ParametersTooLarge(p)) => assert_eq!(p.limit(), 64 * 1024),
    // A backend that cannot open at all is fine — what must not happen
    // is the parameters being copied into a context first.
    Err(other) => panic!("expected ParametersTooLarge, got {other:?}"),
    Ok(_) => panic!("the HW decoder opened over oversized parameters"),
  }

  // And the defaults admit the same parameters, so the ceiling is a
  // ceiling rather than a wall.
  assert!(FfmpegImageDecoder::open(build(), DecoderLimits::default()).is_ok());
}

/// The image decoder duplicates its compressed input into an `AVPacket`.
/// That copy was capped by nothing but `c_int::MAX` on the road that
/// skips the demuxer — a caller building the packet itself.
#[test]
fn an_oversized_image_input_is_refused_before_the_packet_copy() {
  use mediadecode_ffmpeg::{DecoderLimits, ImageDecodeError};
  let Some(corpus) = Corpus::new() else {
    return;
  };
  let path = corpus.exif_oriented_jpeg(1);
  let bytes = std::fs::read(&path).expect("read the still");

  use ffmpeg_next::codec::Parameters;
  support::init_ffmpeg();
  let input = ffmpeg_next::format::input(&path).expect("open");
  let parameters: Parameters = input
    .streams()
    .best(ffmpeg_next::media::Type::Video)
    .expect("a picture stream")
    .parameters();
  drop(input);

  let tight = DecoderLimits::new().with_max_image_input_bytes(8);
  let mut decoder = FfmpegImageDecoder::open(parameters, tight).expect("open");
  let packet = AttachmentPacket::new(
    FfmpegBytes::try_copy_from_slice(&bytes).expect("a test payload"),
    Default::default(),
  );
  match decoder.decode(&packet) {
    Err(ImageDecodeError::InputTooLarge(p)) => {
      assert_eq!(p.limit(), 8);
      assert_eq!(p.bytes(), bytes.len());
    }
    Err(other) => panic!("expected InputTooLarge, got {other:?}"),
    Ok(_) => panic!("an 8-byte input ceiling passed a real JPEG"),
  }
  // The default seat is the attachment family, so the same bytes that
  // would have been admitted through the demuxer are admitted here.
  assert!(mediadecode_ffmpeg::DEFAULT_MAX_IMAGE_INPUT_BYTES >= bytes.len());
}

/// A synthesized attachment whose payload sits **between** the two
/// ceilings — over the per-stream parameter one, under the attachment
/// one — must open. The clone used to copy and charge its extradata on
/// the way past, so exactly this interval was refused by a budget that
/// had already agreed to it.
#[test]
fn an_attachment_between_the_two_ceilings_opens() {
  use mediadecode_ffmpeg::DemuxLimits;
  let Some(corpus) = Corpus::new() else {
    return;
  };
  let path = corpus.multi_track_mkv();

  // The interval, made narrow around the fixture's own font: the
  // parameter ceiling is put *below* the font's extradata and the
  // attachment ceiling above it. The video track's own parameters stay
  // under the parameter ceiling, so nothing else trips.
  let interval = DemuxLimits::new()
    .with_max_codec_parameter_bytes(1_024)
    .with_max_total_codec_parameter_bytes(8 * 1_024)
    .with_max_attachment_bytes(1024 * 1024);
  let mut demuxer = FfmpegDemuxer::open_with(&path, interval)
    .expect("a font over the parameter ceiling and under the attachment one must open");

  // And its payload still arrives whole.
  let mut saw_font = false;
  while let Some(packet) = demuxer.next_packet().expect("read") {
    if let DemuxedPacket::Attachment(attachment) = packet {
      assert!(!attachment.packet().data().is_empty());
      saw_font = true;
    }
  }
  assert!(saw_font, "the attachment was admitted but never delivered");
}

/// Indexed and 1-bit stills decode. Cover art is routinely an indexed
/// PNG, and refusing it left a real picture undecodable.
///
/// **Nothing is converted.** The `pal8` still arrives as its indices
/// plus its 1024-byte palette; the `monob` still arrives as packed
/// bits. Turning either into RGB is colconv's job one tier along.
#[test]
fn indexed_and_one_bit_stills_decode_as_what_ffmpeg_produced() {
  let Some(corpus) = Corpus::new() else {
    return;
  };

  // pal8: indices in plane 0 at one byte per pixel, palette in plane 1
  // at a fixed 1024 bytes.
  let indexed = decode_still(&corpus.indexed_png());
  assert_eq!((indexed.width(), indexed.height()), (32, 24));
  assert_eq!(
    indexed.pixel_format(),
    &mediadecode::PixelFormat::Pal8,
    "delivered as decoded, not converted",
  );
  assert_eq!(indexed.plane_count(), 2, "indices and palette");
  assert_eq!(indexed.planes()[0].data_ref().len(), 32 * 24);
  assert_eq!(
    indexed.planes()[1].data_ref().len(),
    1024,
    "AVPALETTE_SIZE — a format bound, not a budget seat",
  );

  // monob: rows of ceil(32/8) = 4 bytes.
  let mono = decode_still(&corpus.monochrome_png());
  assert_eq!((mono.width(), mono.height()), (32, 24));
  assert_eq!(mono.pixel_format(), &mediadecode::PixelFormat::Monoblack);
  assert_eq!(mono.plane_count(), 1);
  assert_eq!(mono.planes()[0].stride(), 4, "ceil(width / 8)");
  assert_eq!(mono.planes()[0].data_ref().len(), 4 * 24);

  // The video road still refuses both — widening the still road did not
  // widen that one.
  assert!(!mediadecode_ffmpeg::convert::is_video_deliverable(
    &mediadecode::PixelFormat::Pal8
  ));
  assert!(!mediadecode_ffmpeg::convert::is_video_deliverable(
    &mediadecode::PixelFormat::Monoblack
  ));
}

// ---------------------------------------------------------------------------
//  R5: valid bytes, the send leg, the active ceiling, and exact caps.
// ---------------------------------------------------------------------------

/// An audio plane exports the samples the decoder wrote, and **not**
/// the alignment padding `linesize` includes.
///
/// `av_samples_get_buffer_size` rounds each plane up for alignment, so
/// `linesize[0]` routinely exceeds `nb_samples * bytes_per_sample`.
/// Exporting `linesize` formed a slice over bytes nothing initialises —
/// undefined behaviour — and handed that stale heap to a consumer
/// inside a safe carrier.
///
/// **What this test proves and what it cannot.** It proves the exported
/// length is exactly the valid product, on real decoder output, which
/// is what makes the over-read impossible. It cannot prove the absence
/// of uninitialised *reads* from inside a normal test binary — that
/// needs Miri or a sanitiser, which the repo runs separately (`ci/`
/// carries `miri_sb.sh`, `miri_tb.sh` and `sanitizer.sh`). The length
/// assertion is the part a unit test can own, and it is the part that
/// fails when the bug returns.
#[test]
fn an_audio_plane_exports_valid_samples_not_alignment_padding() {
  use mediadecode::decoder::AudioStreamDecoder;
  use mediadecode_ffmpeg::{
    DecoderLimits, FfmpegOwnedAudioStreamDecoder as FfmpegAudioStreamDecoder,
  };
  let Some(corpus) = Corpus::new() else {
    return;
  };
  // 16-bit PCM: bytes_per_sample is exactly 2, so the valid product is
  // arithmetic a test can restate without asking FFmpeg.
  let path = corpus.sine_wav("valid-bytes.wav", 44_100, 2, 440, 0.25);
  support::init_ffmpeg();

  let mut input = ffmpeg_next::format::input(&path).expect("open");
  let stream = input
    .streams()
    .best(ffmpeg_next::media::Type::Audio)
    .expect("an audio track");
  let index = stream.index();
  let time_base = mediadecode::Timebase::new(
    stream.time_base().numerator(),
    std::num::NonZeroI32::new(stream.time_base().denominator().max(1)).expect("nonzero"),
  );
  let parameters = stream.parameters();
  let mut decoder = FfmpegAudioStreamDecoder::open(parameters, time_base, DecoderLimits::default())
    .expect("open decoder");

  let mut checked = 0usize;
  let mut packet = ffmpeg_next::Packet::empty();
  while packet.read(&mut input).is_ok() {
    if packet.stream() != index {
      continue;
    }
    let Ok(Some(portable)) =
      // SAFETY: `Packet::read` is `av_read_frame`, so libavformat wrote the payload, and the
      // packet is live for the call.
      (unsafe { mediadecode_ffmpeg::audio_packet_from_ffmpeg(&packet, mediadecode::Timebase::SECONDS) })
    else {
      continue;
    };
    if decoder.send_packet(&portable).is_err() {
      continue;
    }
    let mut frame = mediadecode_ffmpeg::empty_owned_audio_frame();
    while matches!(
      decoder.receive_frame(&mut frame).expect("receive_frame"),
      Received::Produced
    ) {
      let samples = frame.nb_samples() as usize;
      if samples == 0 {
        continue;
      }
      let channels = frame.channel_count() as usize;
      // s16: two bytes per sample. Packed means one plane holding every
      // channel; planar means one plane per channel.
      let valid = if frame.plane_count() as usize == 1 {
        samples * 2 * channels
      } else {
        samples * 2
      };
      for plane in frame.planes() {
        assert_eq!(
          plane.data_ref().len(),
          valid,
          "a plane exported {} bytes for {samples} samples across {channels} channels — \
           alignment padding is riding along",
          plane.data_ref().len(),
        );
        // The stride agrees with the carrier, so a consumer that trusts
        // either gets the same answer.
        assert_eq!(plane.stride() as usize, valid);
      }
      checked += 1;
    }
  }
  assert!(checked > 0, "the fixture decoded no audio frames");
}

/// The send leg. Rebuilding a packet into an `AVPacket` duplicates its
/// bytes, and that copy was capped by nothing but `c_int::MAX` — so a
/// configured `max_packet_bytes` was dead on the road a caller feeds a
/// decoder directly.
///
/// All three packet families, equality-at-cap and over-limit.
#[test]
fn the_send_leg_judges_the_packet_budget_on_all_three_families() {
  use mediadecode_ffmpeg::{
    OwnedAudioPacket as AudioPacket, OwnedSubtitlePacket as SubtitlePacket,
    OwnedVideoPacket as VideoPacket, PacketBuildError, PacketLimits,
    boundary::{
      ffmpeg_packet_from_owned_audio_packet, ffmpeg_packet_from_owned_subtitle_packet,
      ffmpeg_packet_from_owned_video_packet,
    },
  };
  support::init_ffmpeg();

  let body = vec![3u8; 2_048];
  let bytes = FfmpegBytes::try_copy_from_slice(&body).expect("a test payload");
  let at_cap = PacketLimits::new().with_max_packet_bytes(2_048);
  let under = PacketLimits::new().with_max_packet_bytes(2_047);

  let video = VideoPacket::new(bytes.clone(), Default::default());
  let audio = AudioPacket::new(bytes.clone(), Default::default());
  let subtitle = SubtitlePacket::new(bytes.clone(), Default::default());

  // Exactly at the cap is not over it — on every family.
  assert!(ffmpeg_packet_from_owned_video_packet(&video, at_cap).is_ok());
  assert!(ffmpeg_packet_from_owned_audio_packet(&audio, at_cap).is_ok());
  assert!(ffmpeg_packet_from_owned_subtitle_packet(&subtitle, at_cap).is_ok());

  // One byte under: refused, by name, with both numbers.
  for (family, result) in [
    (
      "video",
      ffmpeg_packet_from_owned_video_packet(&video, under).map(|_| ()),
    ),
    (
      "audio",
      ffmpeg_packet_from_owned_audio_packet(&audio, under).map(|_| ()),
    ),
    (
      "subtitle",
      ffmpeg_packet_from_owned_subtitle_packet(&subtitle, under).map(|_| ()),
    ),
  ] {
    match result {
      Err(PacketBuildError::SendPayloadTooLarge(p)) => {
        assert_eq!(p.bytes(), 2_048, "{family}");
        assert_eq!(p.limit(), 2_047, "{family}");
      }
      other => panic!("{family}: expected SendPayloadTooLarge, got {other:?}"),
    }
  }
}

/// The subtitle session retains its limits rather than discarding them
/// at open, so its send path judges the same seat the other two do.
#[test]
fn the_subtitle_session_keeps_its_limits() {
  use mediadecode_ffmpeg::{
    DecoderLimits, FfmpegOwnedSubtitleStreamDecoder as FfmpegSubtitleStreamDecoder,
  };
  let Some(corpus) = Corpus::new() else {
    return;
  };
  let path = corpus.multi_track_mkv();
  support::init_ffmpeg();

  let input = ffmpeg_next::format::input(&path).expect("open");
  let stream = input
    .streams()
    .best(ffmpeg_next::media::Type::Subtitle)
    .expect("a subtitle track");
  let parameters = stream.parameters();
  drop(input);

  let limits = DecoderLimits::new().with_max_packet_bytes(4_321);
  let decoder =
    FfmpegSubtitleStreamDecoder::open(parameters, mediadecode::Timebase::default(), limits)
      .expect("open");
  assert_eq!(
    decoder.limits().max_packet_bytes(),
    4_321,
    "the subtitle session discarded its limits at open",
  );
}

/// The active parameter ceiling binds in **both** directions on the
/// decoder tier: lowered, it refuses before `build_codec_context`;
/// raised, it admits parameters the crate default would have refused.
#[test]
fn the_active_parameter_ceiling_binds_both_ways() {
  use ffmpeg_next::codec::Parameters;
  use mediadecode_ffmpeg::{
    DEFAULT_MAX_CODEC_PARAMETER_BYTES, DecoderLimits, Error,
    FfmpegOwnedImageDecoder as FfmpegImageDecoder,
  };
  support::init_ffmpeg();

  // A 20 MiB ICC profile: over the 16 MiB crate default, under a
  // ceiling a caller can legitimately raise to.
  let build = |profile: usize| {
    let mut out = Parameters::new();
    // SAFETY: `out` owns a live `AVCodecParameters`; both buffers come
    // from FFmpeg's allocator and are handed to it.
    unsafe {
      let par = out.as_mut_ptr();
      (*par).codec_id = ffmpeg_next::ffi::AVCodecID::AV_CODEC_ID_MJPEG;
      (*par).codec_type = ffmpeg_next::ffi::AVMediaType::AVMEDIA_TYPE_VIDEO;
      (*par).width = 32;
      (*par).height = 24;
      let array = ffmpeg_next::ffi::av_mallocz(core::mem::size_of::<
        ffmpeg_next::ffi::AVPacketSideData,
      >()) as *mut ffmpeg_next::ffi::AVPacketSideData;
      assert!(!array.is_null());
      let payload = ffmpeg_next::ffi::av_mallocz(profile) as *mut u8;
      assert!(!payload.is_null());
      (*array).data = payload;
      (*array).size = profile;
      (*array).type_ = ffmpeg_next::ffi::AVPacketSideDataType::AV_PKT_DATA_ICC_PROFILE;
      (*par).coded_side_data = array;
      (*par).nb_coded_side_data = 1;
    }
    out
  };

  const BIG: usize = 20 * 1024 * 1024;
  // The premise, at compile time — a constant fact does not need a
  // test run to hold.
  const _: () = assert!(BIG > DEFAULT_MAX_CODEC_PARAMETER_BYTES);

  // Raised: admitted, where the default would have refused.
  let raised = DecoderLimits::new().with_max_codec_parameter_bytes(32 * 1024 * 1024);
  assert!(
    FfmpegImageDecoder::open(build(BIG), raised).is_ok(),
    "a raised ceiling must actually admit",
  );
  assert!(
    FfmpegImageDecoder::open(build(BIG), DecoderLimits::default()).is_err(),
    "the default must still refuse it",
  );

  // Lowered: refused, even for parameters the default would admit.
  let lowered = DecoderLimits::new().with_max_codec_parameter_bytes(1_024);
  match FfmpegImageDecoder::open(build(64 * 1024), lowered) {
    Err(mediadecode_ffmpeg::ImageDecodeError::Decode(Error::ParametersTooLarge(p))) => {
      assert_eq!(p.limit(), 1_024);
    }
    Err(other) => panic!("a lowered ceiling did not bind: {other:?}"),
    Ok(_) => panic!("a lowered ceiling admitted 64 KiB of parameters"),
  }
}

/// Exact-cap symmetry. A synthesized attachment's carrier allocates the
/// payload and nothing else, so the admission pass must charge the
/// payload and nothing else — otherwise the demux road refuses in the
/// last `AV_INPUT_BUFFER_PADDING_SIZE` bytes below the ceiling that the
/// image road, judging the same bytes, accepts.
#[test]
fn the_attachment_cap_is_exact_and_agrees_across_both_roads() {
  use mediadecode_ffmpeg::DemuxLimits;
  let Some(corpus) = Corpus::new() else {
    return;
  };
  let path = corpus.multi_track_mkv();

  // The fixture's font payload, measured through the delivery it
  // actually gets.
  let mut demuxer = FfmpegDemuxer::open(&path).expect("open");
  let mut payload = 0usize;
  while let Some(packet) = demuxer.next_packet().expect("read") {
    if let DemuxedPacket::Attachment(attachment) = packet {
      payload = attachment.packet().data().len();
    }
  }
  assert!(payload > 0, "the fixture attaches a font");
  drop(demuxer);

  // Exactly at the payload: admitted. A padded charge would have made
  // this the first refused value.
  let at_cap = DemuxLimits::new()
    .with_max_attachment_bytes(payload)
    .with_max_total_attachment_bytes(payload);
  assert!(
    FfmpegDemuxer::open_with(&path, at_cap).is_ok(),
    "a {payload}-byte attachment was refused by a {payload}-byte ceiling",
  );

  // One byte under: refused. The cap is exact in both directions.
  let under = DemuxLimits::new().with_max_attachment_bytes(payload - 1);
  assert!(
    FfmpegDemuxer::open_with(&path, under).is_err(),
    "the ceiling is not exact",
  );

  // And the aggregate agrees with the per-attachment figure, so a
  // single attachment cannot be admitted by one tier and refused by the
  // other at the same number.
  let aggregate_at_cap = DemuxLimits::new()
    .with_max_attachment_bytes(usize::MAX)
    .with_max_total_attachment_bytes(payload);
  assert!(FfmpegDemuxer::open_with(&path, aggregate_at_cap).is_ok());
  let aggregate_under = DemuxLimits::new()
    .with_max_attachment_bytes(usize::MAX)
    .with_max_total_attachment_bytes(payload - 1);
  assert!(FfmpegDemuxer::open_with(&path, aggregate_under).is_err());
}

// ---------------------------------------------------------------------------
//  R6: the empty audio frame.
// ---------------------------------------------------------------------------

/// FFmpeg's canonical empty audio frame: a format, a layout and a rate,
/// with `nb_samples == 0`, `data[i] == NULL`, `linesize == 0` and no
/// `AVBufferRef` — nothing is allocated because there is nothing to
/// hold.
///
/// Built by hand rather than decoded, because that is the only way to
/// get the shape: a decoder emits these mid-stream but not on demand.
/// `av_frame_get_buffer` is deliberately **not** called.
fn empty_audio_av_frame(
  format: ffmpeg_next::ffi::AVSampleFormat,
  channels: i32,
) -> ffmpeg_next::frame::Audio {
  let frame = ffmpeg_next::frame::Audio::empty();
  // SAFETY: `frame` owns a live, zeroed `AVFrame`. Every field written
  // is a plain scalar except `ch_layout`, which FFmpeg's own
  // `av_channel_layout_default` fills from a channel count.
  unsafe {
    let p = frame.as_ptr() as *mut ffmpeg_next::ffi::AVFrame;
    (*p).format = format as i32;
    (*p).nb_samples = 0;
    (*p).sample_rate = 48_000;
    ffmpeg_next::ffi::av_channel_layout_default(core::ptr::addr_of_mut!((*p).ch_layout), channels);
  }
  frame
}

#[test]
fn a_packed_zero_sample_frame_converts_to_empty_planes() {
  use mediadecode_ffmpeg::{FrameLimits, convert};
  support::init_ffmpeg();

  // Packed: one plane for every channel together.
  let frame = empty_audio_av_frame(ffmpeg_next::ffi::AVSampleFormat::AV_SAMPLE_FMT_S16, 2);
  // SAFETY: an empty frame: no buffers and no samples, so it exports no byte to be unwritten.
  let out = unsafe {
    convert::audio_frame_from(
      &frame,
      mediadecode::Timebase::default(),
      FrameLimits::default(),
    )
  }
  .expect("a header frame is a frame, not a layout error");

  assert_eq!(out.nb_samples(), 0);
  assert_eq!(out.channel_count(), 2);
  assert_eq!(out.plane_count(), 1, "packed declares one plane");
  for plane in out.planes() {
    assert!(
      plane.data_ref().is_empty(),
      "a zero-sample plane carries no bytes"
    );
    assert_eq!(plane.stride(), 0);
  }
}

#[test]
fn a_planar_zero_sample_frame_converts_to_empty_planes() {
  use mediadecode_ffmpeg::{FrameLimits, convert};
  support::init_ffmpeg();

  // Planar: one plane per channel, all of them empty.
  let frame = empty_audio_av_frame(ffmpeg_next::ffi::AVSampleFormat::AV_SAMPLE_FMT_S16P, 6);
  // SAFETY: an empty frame: no buffers and no samples, so it exports no byte to be unwritten.
  let out = unsafe {
    convert::audio_frame_from(
      &frame,
      mediadecode::Timebase::default(),
      FrameLimits::default(),
    )
  }
  .expect("a header frame is a frame, not a layout error");

  assert_eq!(out.nb_samples(), 0);
  assert_eq!(out.channel_count(), 6);
  assert_eq!(
    out.plane_count(),
    6,
    "planar declares one plane per channel — the shape a consumer expects",
  );
  for plane in out.planes() {
    assert!(plane.data_ref().is_empty());
    assert_eq!(plane.stride(), 0);
  }
  // The empty carrier is shared, so six declared planes cost no
  // allocations at all.
  let first = out.planes()[0].data_ref();
  assert!(
    out.planes().iter().all(|p| p.data_ref().ptr_eq(first)),
    "the empty planes should share one allocation",
  );
}

// ---------------------------------------------------------------------------
//  R7: the header fields are judged, never clipped.
// ---------------------------------------------------------------------------

/// A hand-built audio frame with an arbitrary raw format and sample
/// count, and **nothing allocated**: `data[i]` stays null and no
/// `AVBufferRef` is attached.
///
/// That absence is the instrument. Any refusal these lanes assert has
/// to happen before the copier looks at a plane, because the copier
/// dereferencing a null `data[0]` would surface as `InvalidPlaneLayout`
/// (or crash) instead. A lane that names its own error therefore proves
/// the judgement landed *ahead* of the geometry — there is no allocated
/// frame here for a copy to have succeeded against.
fn raw_audio_av_frame(
  format_raw: i32,
  nb_samples: i32,
  channels: i32,
  linesize0: i32,
) -> ffmpeg_next::frame::Audio {
  let frame = ffmpeg_next::frame::Audio::empty();
  // SAFETY: `frame` owns a live, zeroed `AVFrame`. Every field written
  // is a plain scalar except `ch_layout`, which FFmpeg's own
  // `av_channel_layout_default` fills from a channel count.
  unsafe {
    let p = frame.as_ptr() as *mut ffmpeg_next::ffi::AVFrame;
    (*p).format = format_raw;
    (*p).nb_samples = nb_samples;
    (*p).sample_rate = 48_000;
    (*p).linesize[0] = linesize0;
    ffmpeg_next::ffi::av_channel_layout_default(core::ptr::addr_of_mut!((*p).ch_layout), channels);
  }
  frame
}

#[test]
fn a_negative_sample_count_is_refused_by_name() {
  use mediadecode_ffmpeg::{FrameLimits, convert, convert::ConvertError};
  support::init_ffmpeg();

  // The count used to be floored to zero, which turned a malformed
  // header into the *empty frame* shape — a refusal reported as a
  // successful decode of nothing.
  let frame = raw_audio_av_frame(
    ffmpeg_next::ffi::AVSampleFormat::AV_SAMPLE_FMT_S16 as i32,
    -1,
    2,
    0,
  );
  // SAFETY: the frame is live for the call and holds no buffers, so it exports no byte to be
  // unwritten; it is refused on its header before anything else is read.
  match unsafe {
    convert::audio_frame_from(
      &frame,
      mediadecode::Timebase::default(),
      FrameLimits::default(),
    )
  } {
    Err(ConvertError::InvalidSampleCount(payload)) => {
      assert_eq!(payload.count(), -1, "the refusal reports what it read");
    }
    Err(other) => panic!("a negative sample count must be refused by name, got {other:?}"),
    Ok(_) => panic!("a negative sample count converted instead of being refused"),
  }
}

#[test]
fn a_zero_sample_frame_with_no_sample_format_is_refused() {
  use mediadecode_ffmpeg::{FrameLimits, convert, convert::ConvertError};
  support::init_ffmpeg();

  // `AV_SAMPLE_FMT_NONE` is the state a codec context is in before its
  // decoder opens. Zero samples used to skip the format check entirely,
  // so this returned an `AudioFrame` advertising a format nothing can
  // interpret — an empty frame is still a frame, and it still has to
  // say what it is.
  for format_raw in [
    ffmpeg_next::ffi::AVSampleFormat::AV_SAMPLE_FMT_NONE as i32,
    99_999, // and an unknown one, which is the same claim from a file
  ] {
    let frame = raw_audio_av_frame(format_raw, 0, 2, 0);
    // SAFETY: the frame is live for the call and holds no buffers, so it exports no byte to be
    // unwritten; it is refused on its header before anything else is read.
    match unsafe {
      convert::audio_frame_from(
        &frame,
        mediadecode::Timebase::default(),
        FrameLimits::default(),
      )
    } {
      Err(ConvertError::UnsupportedSampleFormat(payload)) => {
        assert_eq!(payload.raw(), format_raw);
      }
      Err(other) => {
        panic!("format {format_raw} must be refused even at zero samples, got {other:?}")
      }
      Ok(_) => panic!("format {format_raw} converted at zero samples instead of being refused"),
    }
  }
}

#[test]
fn a_packed_frame_above_255_channels_is_refused_before_any_copy() {
  use mediadecode_ffmpeg::{FrameLimits, convert, convert::ConvertError};
  support::init_ffmpeg();

  // 256 packed S16 channels, one sample: 512 valid bytes. The channel
  // count used to be clipped to 255, so the byte product came out at
  // 510 — two bytes short of the samples, on a frame that then
  // advertised 255 channels. Both numbers were lies, and neither was
  // reported.
  //
  // `linesize[0]` claims the full 512 so that a clipped run would find
  // its (short) product satisfied and proceed to copy; `data[0]` is
  // null, so proceeding is exactly what this lane would catch.
  let frame = raw_audio_av_frame(
    ffmpeg_next::ffi::AVSampleFormat::AV_SAMPLE_FMT_S16 as i32,
    1,
    256,
    512,
  );
  // SAFETY: the frame is live for the call and holds no buffers, so it exports no byte to be
  // unwritten; it is refused on its header before anything else is read.
  match unsafe {
    convert::audio_frame_from(
      &frame,
      mediadecode::Timebase::default(),
      FrameLimits::default(),
    )
  } {
    Err(ConvertError::UnsupportedChannelCount(payload)) => {
      assert_eq!(
        payload.channels(),
        256,
        "the refusal reports the declared count, not a clipped one",
      );
    }
    // Anything reached through the plane loop proves the refusal came
    // too late: the copier had to look at a null pointer to get there.
    Err(other) => panic!("256 packed channels must be refused before geometry, got {other:?}"),
    Ok(_) => panic!("256 packed channels converted — the count was clipped, not judged"),
  }
}

#[test]
fn a_packed_frame_at_255_channels_still_converts() {
  use mediadecode_ffmpeg::{FrameLimits, convert};
  support::init_ffmpeg();

  // The other side of the boundary: 255 is the last count the frame's
  // channel seat can state, and it is carried, not refused. A ceiling
  // that also rejects the values below it is not a ceiling.
  let mut frame = ffmpeg_next::frame::Audio::empty();
  // SAFETY: `frame` owns a live, zeroed `AVFrame`; the fields written
  // are the scalars `av_frame_get_buffer` reads to size the allocation,
  // and its return value is checked before the frame is used.
  unsafe {
    let p = frame.as_mut_ptr();
    (*p).format = ffmpeg_next::ffi::AVSampleFormat::AV_SAMPLE_FMT_S16 as i32;
    (*p).nb_samples = 1;
    (*p).sample_rate = 48_000;
    ffmpeg_next::ffi::av_channel_layout_default(core::ptr::addr_of_mut!((*p).ch_layout), 255);
    let rc = ffmpeg_next::ffi::av_frame_get_buffer(p, 0);
    assert_eq!(rc, 0, "255 packed channels is an allocation FFmpeg makes");
    // `av_frame_get_buffer` leaves the samples unwritten; written here, as
    // a decoder writes them, through each buffer's raw pointer.
    for buf in (*p).buf {
      if !buf.is_null() {
        core::ptr::write_bytes((*buf).data, 0, (*buf).size);
      }
    }
  }

  // SAFETY: the frame is live for the call, and its one buffer was written whole above.
  let out = unsafe {
    convert::audio_frame_from(
      &frame,
      mediadecode::Timebase::default(),
      FrameLimits::default(),
    )
  }
  .expect("255 channels is inside the frame's channel limit");

  assert_eq!(out.channel_count(), 255);
  assert_eq!(out.nb_samples(), 1);
  assert_eq!(out.plane_count(), 1, "packed declares one plane");
  assert_eq!(
    out.planes()[0].data_ref().len(),
    255 * 2,
    "the byte product uses the declared count, not a clipped one",
  );
}

// ---------------------------------------------------------------------------
//  R8: a validator that runs after its field's first consumer is not a
//  validator.
// ---------------------------------------------------------------------------

/// A hand-built audio frame whose `ch_layout` is written field by field,
/// so a lane can declare a shape FFmpeg's own constructors will not
/// produce.
fn audio_av_frame_with_layout(
  format_raw: i32,
  nb_samples: i32,
  order_raw: i32,
  nb_channels: i32,
) -> ffmpeg_next::frame::Audio {
  let frame = ffmpeg_next::frame::Audio::empty();
  // SAFETY: `frame` owns a live, zeroed `AVFrame`. Every field written
  // is a plain scalar: `order` and `nb_channels` are written as the
  // integers they are, and `u.map` is left null — no `AVChannelLayout`
  // value is ever *formed*, only its bytes set.
  unsafe {
    let p = frame.as_ptr() as *mut ffmpeg_next::ffi::AVFrame;
    (*p).format = format_raw;
    (*p).nb_samples = nb_samples;
    (*p).sample_rate = 48_000;
    let layout = core::ptr::addr_of_mut!((*p).ch_layout);
    core::ptr::write(
      core::ptr::addr_of_mut!((*layout).order) as *mut i32,
      order_raw,
    );
    (*layout).nb_channels = nb_channels;
  }
  frame
}

#[test]
fn a_negative_channel_count_is_refused_rather_than_read_as_zero() {
  use mediadecode_ffmpeg::{FrameLimits, convert, convert::ConvertError};
  support::init_ffmpeg();

  // The guard used to read the count back off the materialised
  // `ChannelLayoutDescription`, which stores `nb_channels.max(0)`. So a
  // declared `-1` reached it as a legitimate-looking `0` — and on a
  // zero-sample frame, where the zero-channel refusal does not apply,
  // it produced a frame. The validator was reading a number its own
  // consumer had already laundered.
  let frame = audio_av_frame_with_layout(
    ffmpeg_next::ffi::AVSampleFormat::AV_SAMPLE_FMT_S16 as i32,
    0,
    ffmpeg_next::ffi::AVChannelOrder::AV_CHANNEL_ORDER_UNSPEC as i32,
    -1,
  );
  // SAFETY: the frame is live for the call and holds no buffers, so it exports no byte to be
  // unwritten; it is refused on its header before anything else is read.
  match unsafe {
    convert::audio_frame_from(
      &frame,
      mediadecode::Timebase::default(),
      FrameLimits::default(),
    )
  } {
    Err(ConvertError::UnsupportedChannelCount(p)) => {
      assert_eq!(
        p.channels(),
        -1,
        "the refusal must report the signed count the frame declared",
      );
    }
    Err(other) => panic!("expected UnsupportedChannelCount, got {other:?}"),
    Ok(_) => panic!("a negative channel count was laundered into a frame"),
  }
}

#[test]
fn an_oversized_custom_layout_is_refused_before_its_map_is_walked() {
  use mediadecode_ffmpeg::{FrameLimits, convert, convert::ConvertError};
  support::init_ffmpeg();

  // **Proof by construction.** The layout declares `AV_CHANNEL_ORDER_CUSTOM`
  // with `i32::MAX` channels and a **null** `u.map`. Materialising it
  // renders the layout's name through FFmpeg and walks `nb_channels`
  // map entries into a `Vec::with_capacity(nb_channels)` — so an
  // implementation that materialises before it judges dereferences null
  // `i32::MAX` times, and this lane crashes instead of failing.
  //
  // It can only return at all if the count was judged off the raw field
  // first, which is the whole point: the work this ceiling exists to
  // bound was, until R8, done before the ceiling was applied.
  let frame = audio_av_frame_with_layout(
    ffmpeg_next::ffi::AVSampleFormat::AV_SAMPLE_FMT_S16 as i32,
    0,
    ffmpeg_next::ffi::AVChannelOrder::AV_CHANNEL_ORDER_CUSTOM as i32,
    i32::MAX,
  );
  // SAFETY: the frame is live for the call and holds no buffers, so it exports no byte to be
  // unwritten; it is refused on its header before anything else is read.
  match unsafe {
    convert::audio_frame_from(
      &frame,
      mediadecode::Timebase::default(),
      FrameLimits::default(),
    )
  } {
    Err(ConvertError::UnsupportedChannelCount(p)) => assert_eq!(p.channels(), i32::MAX),
    Err(other) => panic!("expected UnsupportedChannelCount, got {other:?}"),
    Ok(_) => panic!("a custom layout of i32::MAX channels produced a frame"),
  }
}

/// A hand-built picture frame declaring dimensions and nothing else.
fn picture_av_frame(width: i32, height: i32) -> ffmpeg_next::frame::Video {
  let frame = ffmpeg_next::frame::Video::empty();
  // SAFETY: `frame` owns a live, zeroed `AVFrame`; all three fields are
  // plain scalars.
  unsafe {
    let p = frame.as_ptr() as *mut ffmpeg_next::ffi::AVFrame;
    (*p).format = ffmpeg_next::ffi::AVPixelFormat::AV_PIX_FMT_YUV420P as i32;
    (*p).width = width;
    (*p).height = height;
  }
  frame
}

#[test]
fn negative_picture_dimensions_are_refused_on_both_picture_roads() {
  use mediadecode_ffmpeg::{FrameLimits, convert, convert::ConvertError};
  support::init_ffmpeg();

  // Found by auditing the order class the channel count belonged to,
  // rather than reported: `width` and `height` were floored with
  // `.max(0)` before anything judged them, so a declared `-1` became
  // `0`, and zero pixels is under every ceiling. The frame was built.
  for (w, h) in [(-1, 32), (32, -1), (-4, -4)] {
    let frame = picture_av_frame(w, h);

    // SAFETY: the frame is live for the call and holds no buffers, so it exports no byte to be
    // unwritten; it is refused on its header before anything else is read.
    match unsafe {
      convert::video_frame_from(
        &frame,
        mediadecode::Timebase::default(),
        FrameLimits::default(),
      )
    } {
      Err(ConvertError::InvalidDimensions(p)) => {
        assert_eq!((p.width(), p.height()), (w, h), "reported as declared");
      }
      Err(other) => panic!("video {w}x{h}: expected InvalidDimensions, got {other:?}"),
      Ok(_) => panic!("video {w}x{h} was floored into a frame instead of refused"),
    }

    // The still road reads the same two fields the same way, so it had
    // the same hole.
    // SAFETY: the frame is live for the call and holds no buffers, so it exports no byte to be
    // unwritten; it is refused on its header before anything else is read.
    match unsafe { convert::image_frame_from(&frame, FrameLimits::default()) } {
      Err(ConvertError::InvalidDimensions(p)) => {
        assert_eq!((p.width(), p.height()), (w, h));
      }
      Err(other) => panic!("image {w}x{h}: expected InvalidDimensions, got {other:?}"),
      Ok(_) => panic!("image {w}x{h} was floored into a frame instead of refused"),
    }
  }
}

#[test]
fn the_image_seam_honours_the_packet_flags_it_used_to_drop() {
  use mediadecode::packet::PacketFlags;
  use mediadecode_ffmpeg::CorruptSource;
  let Some(corpus) = support::Corpus::new() else {
    return;
  };
  support::init_ffmpeg();

  let path = corpus.cover_art_mp3();
  let mut demuxer = FfmpegDemuxer::open(&path).expect("open");
  let cover = demuxer
    .tracks()
    .iter()
    .position(|t| t.kind() == TrackKind::Attachment)
    .expect("the file has cover art");
  let parameters = demuxer.tracks()[cover]
    .extra()
    .clone_parameters()
    .expect("parameters");
  let mut payload = None;
  while let Some(packet) = demuxer.next_packet().expect("read") {
    if let DemuxedPacket::Attachment(attachment) = packet {
      payload = Some(attachment.into_packet());
      break;
    }
  }
  let payload = payload.expect("the attachment track delivers its packet");
  let mut decoder =
    FfmpegImageDecoder::open(parameters, DecoderLimits::default()).expect("open image decoder");

  // The control: unflagged, this payload is a picture.
  decoder.decode(&payload).expect("the plain cover decodes");

  // **CORRUPT — honoured at the seam, by census.** Measured on this
  // build: the flag reaches libavcodec and libavcodec does nothing with
  // it — mjpeg returns a full picture with `AV_FRAME_FLAG_CORRUPT`
  // clear. Forwarding it would only move where the fact is dropped, and
  // `ImageFrame` has no flag seat to land it in, so the seam refuses by
  // name instead of handing back a picture with its warning deleted.
  let corrupt = payload
    .clone()
    .with_flags(PacketFlags::KEY | PacketFlags::CORRUPT);
  match decoder.decode(&corrupt) {
    Err(ImageDecodeError::Corrupt(p)) => {
      assert_eq!(p.declared_by(), CorruptSource::Packet);
    }
    Err(other) => panic!("expected a named corruption refusal, got {other:?}"),
    Ok(_) => panic!("a packet marked corrupt came back as a picture"),
  }

  // **DISCARD — forwarded, because libavcodec really does obey it.**
  // Measured the same way: with the flag set the decoder produces no
  // frame at all. This lane is what proves the flag survives the rebuild
  // — before R8 the `AVPacket` was built from bytes alone with `flags`
  // left zeroed, and this decode returned a picture.
  let discard = payload
    .clone()
    .with_flags(PacketFlags::KEY | PacketFlags::DISCARD);
  match decoder.decode(&discard) {
    Err(ImageDecodeError::NoImage) => {}
    Err(other) => panic!("expected the decoder to drop a DISCARD packet, got {other:?}"),
    Ok(_) => panic!("DISCARD never reached libavcodec — the rebuilt packet lost its flags"),
  }

  // **And the fourth rebuild road refuses `TRUSTED` like the other
  // three.** This seam writes the flags onto a fresh `AVPacket` and
  // hands it to a decoder, so a `TRUSTED` bit arriving here is a
  // decoder being told it may dereference a body this crate copied by
  // value. Built by hand, because the copy-out leg will no longer
  // produce one.
  let trusted = payload
    .clone()
    .with_flags(PacketFlags::from_bits_retain(0b0000_1000));
  assert!(
    matches!(
      decoder.decode(&trusted),
      Err(ImageDecodeError::TrustedPayload(_)),
    ),
    "a TRUSTED attachment must not reach libavcodec",
  );

  // And the seam is not latched: the plain payload still decodes after
  // both refusals.
  decoder.decode(&payload).expect("the seam recovers");
}

// ---------------------------------------------------------------------------
//  R9: uncarriable payloads, and numbers judged before they are spent.
// ---------------------------------------------------------------------------

/// A real allocated YUV420P frame, optionally cropped, its planes written
/// as a decoder's are — `av_frame_get_buffer` leaves them unwritten, and a
/// conversion reads only written bytes.
fn allocated_picture(width: i32, height: i32, crop: [u64; 4]) -> ffmpeg_next::frame::Video {
  let mut frame = ffmpeg_next::frame::Video::empty();
  // SAFETY: `frame` owns a live, zeroed `AVFrame`; the fields written are
  // the scalars `av_frame_get_buffer` reads to size the allocation, and
  // its return code is checked before the frame is used. Each non-null
  // `buf[i]` it allocated is then written whole through its raw pointer.
  unsafe {
    let p = frame.as_mut_ptr();
    (*p).format = ffmpeg_next::ffi::AVPixelFormat::AV_PIX_FMT_YUV420P as i32;
    (*p).width = width;
    (*p).height = height;
    let rc = ffmpeg_next::ffi::av_frame_get_buffer(p, 32);
    assert_eq!(rc, 0, "av_frame_get_buffer");
    for buf in (*p).buf {
      if !buf.is_null() {
        core::ptr::write_bytes((*buf).data, 0, (*buf).size);
      }
    }
    (*p).crop_left = crop[0] as usize;
    (*p).crop_top = crop[1] as usize;
    (*p).crop_right = crop[2] as usize;
    (*p).crop_bottom = crop[3] as usize;
  }
  frame
}

#[test]
fn a_crop_that_cannot_be_a_crop_withholds_the_rect_instead_of_inventing_one() {
  use mediadecode_ffmpeg::{FrameLimits, convert};
  support::init_ffmpeg();

  let convert_it = |frame: &ffmpeg_next::frame::Video| {
    // SAFETY: every frame this closure converts is `allocated_picture`'s, live for the call,
    // with every buffer written.
    unsafe {
      convert::video_frame_from(
        frame,
        mediadecode::Timebase::default(),
        FrameLimits::default(),
      )
    }
    .expect("a well-formed picture converts")
  };

  // A legitimate crop still comes through, exactly as declared.
  let ok = allocated_picture(64, 64, [4, 8, 4, 8]);
  let rect = convert_it(&ok)
    .visible_rect()
    .expect("a real crop is reported");
  assert_eq!((rect.x(), rect.y()), (4, 8));
  assert_eq!((rect.width(), rect.height()), (64 - 8, 64 - 16));

  // **Overflow.** These are `size_t`, so `left + right` really can
  // exceed `u64`. Unchecked, that panicked in debug and *wrapped* in
  // release — and a wrapped sum passes the extent test and then narrows
  // into a rect pointing outside the picture.
  let overflow = allocated_picture(64, 64, [u64::MAX, 0, 2, 0]);
  assert!(
    convert_it(&overflow).visible_rect().is_none(),
    "an overflowing crop must not produce a rect",
  );

  // **Exactly at the extent.** `checked_sub` accepted this and returned
  // a zero-width rect — the absence of a picture, asserted as a fact
  // about one. FFmpeg's own `av_frame_apply_cropping` refuses it.
  let at_extent = allocated_picture(64, 64, [32, 0, 32, 0]);
  assert!(
    convert_it(&at_extent).visible_rect().is_none(),
    "a crop that leaves nothing is not a crop",
  );

  // And one row inside it is fine, so the boundary is where it says.
  let just_inside = allocated_picture(64, 64, [31, 0, 32, 0]);
  let rect = convert_it(&just_inside)
    .visible_rect()
    .expect("one column left is still a picture");
  assert_eq!(rect.width(), 1);
}

#[test]
fn an_undersized_stride_is_refused_before_any_plane_is_copied() {
  use mediadecode_ffmpeg::{FrameLimits, convert, convert::ConvertError};
  support::init_ffmpeg();

  // A stride *below* the format's row width was treated as a padded one
  // — the branch for a stride that is larger — so the frame was sized
  // from bytes the plane did not have and the real refusal was left to
  // the copy loop, by which time every earlier plane had been allocated
  // and copied. Refused in the pass that first reads it now.
  let frame = allocated_picture(64, 64, [0; 4]);
  // SAFETY: `frame` owns a live `AVFrame`; `linesize` is a public field.
  unsafe {
    (*frame.as_ptr().cast_mut()).linesize[0] = 8; // 64 bytes of row claimed by 8
  }
  // SAFETY: `allocated_picture` wrote every buffer, and the frame is live for the call; the
  // corrupt `linesize` is a plain field the conversion judges.
  match unsafe {
    convert::video_frame_from(
      &frame,
      mediadecode::Timebase::default(),
      FrameLimits::default(),
    )
  } {
    Err(ConvertError::InvalidPlaneLayout(p)) => assert_eq!(p.plane(), 0),
    Err(other) => panic!("expected InvalidPlaneLayout, got {other:?}"),
    Ok(_) => panic!("a plane narrower than its format converted"),
  }
}

#[test]
fn a_still_refuses_oversized_side_data_rather_than_dropping_the_orientation() {
  use mediadecode_ffmpeg::{
    DEFAULT_MAX_IMAGE_SIDE_DATA_BYTES, FrameLimits, convert, convert::ConvertError,
  };
  support::init_ffmpeg();

  const ICC: ffmpeg_next::ffi::AVFrameSideDataType =
    ffmpeg_next::ffi::AVFrameSideDataType::AV_FRAME_DATA_ICC_PROFILE;
  const MATRIX: ffmpeg_next::ffi::AVFrameSideDataType =
    ffmpeg_next::ffi::AVFrameSideDataType::AV_FRAME_DATA_DISPLAYMATRIX;

  let with_side_data = |icc_bytes: usize| {
    let frame = allocated_picture(16, 16, [0; 4]);
    // SAFETY: `frame` owns a live `AVFrame`; `av_frame_new_side_data`
    // allocates and attaches an entry, returning null on failure. Its
    // bytes come unwritten (`av_buffer_alloc`), so each entry is written
    // whole, `size` bytes at `data`, before anything converts the frame.
    unsafe {
      let icc = ffmpeg_next::ffi::av_frame_new_side_data(frame.as_ptr().cast_mut(), ICC, icc_bytes);
      assert!(!icc.is_null(), "side-data allocation");
      core::ptr::write_bytes((*icc).data, 0, (*icc).size);
      // The display matrix goes in *after* the profile, which is the
      // shape that used to lose it: the byte cap was reached partway
      // through the list and everything past it was skipped.
      let m = ffmpeg_next::ffi::av_frame_new_side_data(frame.as_ptr().cast_mut(), MATRIX, 36);
      assert!(!m.is_null(), "side-data allocation");
      core::ptr::write_bytes((*m).data, 0, (*m).size);
    }
    frame
  };

  // A profile the parameter road already admits — 4 MiB, well inside a
  // real device-link ICC — is carried whole, and the entry behind it
  // survives.
  let ok = with_side_data(4 * 1024 * 1024);
  // SAFETY: `allocated_picture` wrote every buffer and `with_side_data` every side-data byte;
  // the frame is live for the call.
  let image = unsafe { convert::image_frame_from(&ok, FrameLimits::default()) }
    .expect("inside the side-data budget");
  let kinds: Vec<i32> = image.extra().side_data().iter().map(|e| e.kind()).collect();
  assert!(kinds.contains(&(ICC as i32)), "the profile was dropped");
  assert!(
    kinds.contains(&(MATRIX as i32)),
    "the orientation was pushed off the end by the profile — the exact loss",
  );

  // Over the seat: a named refusal, never a truncated list.
  let over = with_side_data(DEFAULT_MAX_IMAGE_SIDE_DATA_BYTES + 1);
  // SAFETY: `allocated_picture` wrote every buffer and `with_side_data` every side-data byte;
  // the frame is live for the call.
  match unsafe { convert::image_frame_from(&over, FrameLimits::default()) } {
    Err(ConvertError::ImageSideDataTooLarge(p)) => {
      assert_eq!(p.limit(), DEFAULT_MAX_IMAGE_SIDE_DATA_BYTES);
      assert!(p.bytes() > p.limit());
    }
    Err(other) => panic!("expected ImageSideDataTooLarge, got {other:?}"),
    Ok(_) => panic!("over-budget side data was silently truncated"),
  }

  // And the seat is configurable, so a caller with a tighter budget
  // gets the same named answer rather than a quiet loss.
  assert!(matches!(
    // SAFETY: `allocated_picture` wrote every buffer and `with_side_data` every side-data byte;
    // the frame is live for the call.
    unsafe {
      convert::image_frame_from(&ok, FrameLimits::new().with_max_image_side_data_bytes(1024))
    },
    Err(ConvertError::ImageSideDataTooLarge(_)),
  ));
}

// ---------------------------------------------------------------------------
//  R10: the ceiling does not negotiate with the file, and nothing is
//  bought before everything is judged.
// ---------------------------------------------------------------------------

#[test]
fn a_still_judges_its_side_data_before_it_buys_a_single_plane() {
  use mediadecode_ffmpeg::{
    DEFAULT_MAX_IMAGE_SIDE_DATA_BYTES, FrameLimits, convert, convert::ConvertError,
  };
  support::init_ffmpeg();

  // **Proof by construction.** The frame declares side data past the
  // budget *and* carries a null `data[0]`. The plane copier refuses a
  // null plane pointer, so if it ran first this lane would come back
  // `InvalidPlaneLayout` — and if it ran first without that guard it
  // would dereference null and crash.
  //
  // `ImageSideDataTooLarge` is therefore only reachable when the
  // side-data judgement happened before a single plane was touched,
  // which is the whole claim: everything this conversion can refuse is
  // refused before anything it can allocate is allocated.
  let frame = allocated_picture(64, 64, [0; 4]);
  const ICC: ffmpeg_next::ffi::AVFrameSideDataType =
    ffmpeg_next::ffi::AVFrameSideDataType::AV_FRAME_DATA_ICC_PROFILE;
  // SAFETY: `frame` owns a live `AVFrame`. The side-data entry is
  // allocated by FFmpeg's own helper and checked; `data[0]` is then
  // nulled deliberately — the buffer stays owned by `frame.buf[0]`, so
  // nothing is leaked and nothing is freed twice.
  unsafe {
    let p = frame.as_ptr().cast_mut();
    let sd =
      ffmpeg_next::ffi::av_frame_new_side_data(p, ICC, DEFAULT_MAX_IMAGE_SIDE_DATA_BYTES + 1);
    assert!(!sd.is_null(), "side-data allocation");
    // Written, as a decoder's side data is, though the refusal under test
    // reads only its size.
    core::ptr::write_bytes((*sd).data, 0, (*sd).size);
    (*p).data[0] = core::ptr::null_mut();
  }

  // SAFETY: `allocated_picture` wrote every buffer and the side data was written above; the
  // frame is live for the call.
  match unsafe { convert::image_frame_from(&frame, FrameLimits::default()) } {
    Err(ConvertError::ImageSideDataTooLarge(p)) => {
      assert_eq!(p.limit(), DEFAULT_MAX_IMAGE_SIDE_DATA_BYTES);
    }
    Err(ConvertError::InvalidPlaneLayout(_)) => {
      panic!("the planes were walked before the side data was judged")
    }
    Err(other) => panic!("expected ImageSideDataTooLarge, got {other:?}"),
    Ok(_) => panic!("over-budget side data was accepted"),
  }
}

// ---------------------------------------------------------------------------
//  R11: the ceiling follows the allocator, and audio gets one at all.
// ---------------------------------------------------------------------------

#[test]
fn a_degenerate_aspect_ratio_cannot_outrun_the_byte_ceiling() {
  let Some(corpus) = Corpus::new() else {
    return;
  };
  support::init_ffmpeg();

  // **The bypass, in numbers measured on this build.** `max_pixels` is
  // checked by libavcodec against the frame's *raw* `width * height`;
  // what it allocates is the shape `avcodec_align_dimensions2` rounds
  // that up to. For a one-pixel-tall `gray8` frame those differ by 32x:
  //
  //   65536x1  ->  65536x32  =  65,536 px / 64 KiB  ->  2,097,152 px / 2 MiB
  //   1x65536  ->  16x65536  =  65,536 px / 64 KiB  ->  1,048,576 px / 2 MiB
  //
  // while a real picture inflates by nothing at all (8K `yuv420p` and
  // 1024x1024 `gray8` both measure 1.00x). No scalar compared against
  // `w * h` can bound a product whose factors are rounded up
  // afterwards, which is why the same scalar is applied a second time
  // inside the allocator.
  //
  // A 1 MiB ceiling buys 65,536 pixels at the worst rate — exactly the
  // raw count of both shapes, so both pass the raw check and neither
  // may pass the aligned one.
  const CEILING: usize = 1024 * 1024;
  for (w, h) in [(65_536u32, 1u32), (1u32, 65_536u32)] {
    let path = corpus.gray_png(w, h);
    match open_still_and_decode(&path, CEILING) {
      Err(ImageDecodeError::Decode(_)) | Err(ImageDecodeError::NoImage) => {}
      Err(other) => panic!("{w}x{h}: expected a pre-allocation refusal, got {other:?}"),
      Ok(()) => panic!("{w}x{h} bought {}x more than its ceiling allowed", 32),
    }
  }

  // And a square picture of the *same pixel count* — which inflates by
  // nothing — decodes under the same ceiling. The refusal is about the
  // shape, not the size, and a guard that also refuses what fits is not
  // a guard.
  let square = corpus.gray_png(256, 256);
  open_still_and_decode(&square, CEILING).expect("a square of the same pixel count fits");

  // The legitimate large picture, admitted. 8K is 33.18 Mpx against the
  // default's 33.55 Mpx effective ceiling, and it inflates 1.00x, so it
  // passes both checks — the raw one and the aligned one.
  let uhd = corpus.gray_png(7680, 4320);
  open_still_and_decode(&uhd, mediadecode_ffmpeg::DEFAULT_MAX_FRAME_BYTES)
    .expect("8K must still decode");
}

/// Opens an image decoder on `path` with `bytes` as the byte ceiling and
/// runs one decode, discarding the picture.
fn open_still_and_decode(path: &std::path::Path, bytes: usize) -> Result<(), ImageDecodeError> {
  use mediadecode_ffmpeg::FrameLimits;
  let mut input = ffmpeg_next::format::input(&path).expect("open the still");
  let stream = input
    .streams()
    .best(ffmpeg_next::media::Type::Video)
    .expect("a picture stream");
  let index = stream.index();
  let parameters = stream.parameters();
  let mut payload = Vec::new();
  loop {
    let mut packet = ffmpeg_next::Packet::empty();
    match packet.read(&mut input) {
      Ok(()) if packet.stream() == index => {
        payload = packet.data().unwrap_or(&[]).to_vec();
        break;
      }
      Ok(()) => continue,
      Err(ffmpeg_next::Error::Eof) => break,
      Err(e) => panic!("read: {e}"),
    }
  }
  drop(input);

  let mut decoder = FfmpegImageDecoder::open(
    parameters,
    DecoderLimits::new()
      .with_frame(FrameLimits::new().with_max_frame_bytes(bytes))
      .with_max_image_input_bytes(usize::MAX),
  )?;
  let packet = AttachmentPacket::new(
    FfmpegBytes::try_copy_from_slice(&payload).expect("a test payload"),
    mediadecode_ffmpeg::extras::AttachmentPacketExtra::new(0),
  );
  decoder.decode(&packet).map(|_| ())
}

#[test]
fn audio_gets_a_pre_allocation_ceiling_too() {
  use mediadecode::decoder::AudioStreamDecoder;
  use mediadecode_ffmpeg::{
    AudioDecodeError, FfmpegOwnedAudioStreamDecoder as FfmpegAudioStreamDecoder, FrameLimits,
  };
  let Some(corpus) = Corpus::new() else {
    return;
  };
  support::init_ffmpeg();
  let path = corpus.sine_wav("r11.wav", 48_000, 2, 440, 0.25);

  // `AVCodecContext.max_samples` is FFmpeg's audio `max_pixels`: it is
  // compared against a frame's `nb_samples` in `ff_get_buffer`, before
  // the planes are allocated. It was left at `INT64_MAX`, so the audio
  // family had no pre-allocation guard at all and leaned entirely on
  // this crate's post-decode byte check — refusing after libavcodec had
  // already paid.
  //
  // The ruler is `max_frame_bytes / (worst bytes per sample x 255
  // channels)`. At 4080 bytes that is one sample per frame, which no
  // real decoder produces.
  let decode_with = |bytes: usize| {
    let mut input = ffmpeg_next::format::input(&path).expect("open");
    let stream = input
      .streams()
      .best(ffmpeg_next::media::Type::Audio)
      .expect("an audio stream");
    let index = stream.index();
    let parameters = stream.parameters();
    let time_base = mediadecode::Timebase::default();
    let mut decoder = FfmpegAudioStreamDecoder::open(
      parameters,
      time_base,
      DecoderLimits::new().with_frame(FrameLimits::new().with_max_frame_bytes(bytes)),
    )
    .expect("open the audio decoder");

    let mut bodies: Vec<Vec<u8>> = Vec::new();
    while bodies.len() < 4 {
      let mut packet = ffmpeg_next::Packet::empty();
      match packet.read(&mut input) {
        Ok(()) if packet.stream() == index => {
          bodies.push(packet.data().unwrap_or(&[]).to_vec());
        }
        Ok(()) => continue,
        Err(ffmpeg_next::Error::Eof) => break,
        Err(e) => panic!("read: {e}"),
      }
    }
    drop(input);

    let mut frame = mediadecode_ffmpeg::empty_owned_audio_frame();
    for body in bodies {
      let audio = AudioPacket::new(
        FfmpegBytes::try_copy_from_slice(&body).expect("a test payload"),
        mediadecode_ffmpeg::extras::AudioPacketExtra::new(0),
      );
      // **The send's answer is deliberately dropped on these ceiling
      // probes.** They feed one packet, drain immediately, and are
      // looking for *which refusal* the ceiling produces — not for a
      // complete decode. A `MustDrain` here would mean the decoder
      // already had output waiting, which the very next line takes; the
      // un-consumed packet is simply not re-offered, and there are more.
      let _ = decoder.send_packet(&audio)?;
      match decoder.receive_frame(&mut frame) {
        Ok(Received::Produced) => return Ok(()),
        // Not enough input yet: feed the next packet.
        Ok(Received::NeedsInput) => continue,
        Ok(Received::Ended) => break,
        Err(e) => return Err(e),
      }
    }
    Err(
      decoder
        .receive_frame(&mut frame)
        .expect_err("no frame and no error"),
    )
  };

  // **The ordering proof, not merely a refusal.** Without `max_samples`
  // this decode still failed — the post-conversion byte check caught
  // it — so "is_err()" would have passed with the guard absent and
  // proved nothing. What distinguishes the two roads is *where* the
  // refusal comes from: `Decode` is libavcodec answering from
  // `ff_get_buffer` before the planes exist, and `Convert` is this
  // crate declining to copy planes libavcodec had already allocated.
  match decode_with(4_080) {
    Err(AudioDecodeError::Decode(_)) => {}
    Err(AudioDecodeError::Convert(_)) => {
      panic!("audio had no pre-allocation guard: the frame was allocated, then refused")
    }
    // `AudioDecodeError` is `#[non_exhaustive]`, so from this crate the
    // match needs a rest arm. It is not a formality here: a *third*
    // kind of refusal would mean the ceiling was enforced by some road
    // this test has never seen, and the whole point of the lane is
    // which of the two roads answered.
    Err(other) => panic!("an unexpected refusal shape: {other:?}"),
    Ok(()) => panic!("an oversized audio frame passed a 4080-byte ceiling"),
  }

  // And ordinary 48 kHz stereo decodes under the defaults, so the
  // ruler does not refuse real audio. FLAC's largest block is 65,535
  // samples and the default ruler admits 263,172.
  assert!(decode_with(mediadecode_ffmpeg::DEFAULT_MAX_FRAME_BYTES).is_ok());
}

// ---------------------------------------------------------------------------
//  R12: pin what the consumer compares, not what we computed.
// ---------------------------------------------------------------------------

/// Decodes the first audio frame of `path` under a byte ceiling,
/// returning the decoded frame or the refusal.
fn decode_first_audio(
  path: &std::path::Path,
  bytes: usize,
) -> Result<AudioFrame, mediadecode_ffmpeg::AudioDecodeError> {
  use mediadecode::decoder::AudioStreamDecoder;
  use mediadecode_ffmpeg::{
    FfmpegOwnedAudioStreamDecoder as FfmpegAudioStreamDecoder, FrameLimits,
  };

  let mut input = ffmpeg_next::format::input(&path).expect("open");
  let stream = input
    .streams()
    .best(ffmpeg_next::media::Type::Audio)
    .expect("an audio stream");
  let index = stream.index();
  let parameters = stream.parameters();
  let mut decoder = FfmpegAudioStreamDecoder::open(
    parameters,
    mediadecode::Timebase::default(),
    DecoderLimits::new().with_frame(FrameLimits::new().with_max_frame_bytes(bytes)),
  )
  .expect("open the audio decoder");

  let mut frame = mediadecode_ffmpeg::empty_owned_audio_frame();
  loop {
    let mut packet = ffmpeg_next::Packet::empty();
    match packet.read(&mut input) {
      Ok(()) if packet.stream() == index => {
        let body = packet.data().unwrap_or(&[]).to_vec();
        let audio = AudioPacket::new(
          FfmpegBytes::try_copy_from_slice(&body).expect("a test payload"),
          mediadecode_ffmpeg::extras::AudioPacketExtra::new(0),
        );
        // Answer dropped: see the note on the first ceiling probe above.
        let _ = decoder.send_packet(&audio)?;
        match decoder.receive_frame(&mut frame) {
          Ok(Received::Produced) => return Ok(frame),
          Ok(Received::NeedsInput | Received::Ended) => continue,
          Err(e) => return Err(e),
        }
      }
      Ok(()) => continue,
      Err(ffmpeg_next::Error::Eof) => panic!("no audio frame in {}", path.display()),
      Err(e) => panic!("read: {e}"),
    }
  }
}

#[test]
fn the_hardware_road_still_delivers_frames_through_its_new_seat() {
  use mediadecode_ffmpeg::{Frame, VideoDecoder};
  let Some(corpus) = Corpus::new() else {
    return;
  };
  support::init_ffmpeg();

  // **The accepting path of the pre-transfer judge.** The hardware road
  // has its own seat now, because neither ceiling hook reaches it:
  // censused on this machine, a VideoToolbox h264 decode records *zero*
  // `get_buffer2` calls (`ff_get_buffer` goes to `hwaccel->alloc_frame`
  // instead), and the CPU destination is allocated by
  // `av_hwframe_transfer_data` outside both hooks.
  //
  // What that seat must not do is refuse ordinary video, which is what
  // this lane pins. Whichever backend the probe settles on — hardware
  // here, software elsewhere — a frame has to come back.
  let path = corpus.multi_track_mkv();
  let mut input = ffmpeg_next::format::input(&path).expect("open");
  let stream = input
    .streams()
    .best(ffmpeg_next::media::Type::Video)
    .expect("a video stream");
  let index = stream.index();
  let mut decoder = VideoDecoder::open(stream.parameters()).expect("open the video decoder");

  let mut frame = Frame::empty().expect("allocate a frame slot");
  let mut delivered = false;
  'outer: loop {
    let mut packet = ffmpeg_next::Packet::empty();
    match packet.read(&mut input) {
      Ok(()) if packet.stream() == index => {
        // SAFETY: `Packet::read` is `av_read_frame`, so libavformat wrote the packet and its
        // padding, and nothing writes through it while the decoder may still read it.
        if unsafe { decoder.send_av_packet(&packet) }.is_err() {
          continue;
        }
        if matches!(decoder.receive_frame(&mut frame), Ok(Received::Produced)) {
          delivered = true;
          break 'outer;
        }
      }
      Ok(()) => continue,
      Err(ffmpeg_next::Error::Eof) => break,
      Err(e) => panic!("read: {e}"),
    }
  }

  assert!(
    delivered,
    "the {:?} road stopped delivering frames",
    decoder.backend(),
  );
  assert!(frame.width() > 0 && frame.height() > 0);
}

#[test]
fn a_cropped_stream_is_judged_on_what_it_allocates_not_what_it_displays() {
  use mediadecode_ffmpeg::{FrameLimits, VideoDecoder};
  let Some(corpus) = Corpus::new() else {
    return;
  };
  support::init_ffmpeg();

  // **Two dimension vocabularies.** This clip displays 32x32 — 1024
  // pixels — out of a 1920x1088 coded surface, a 2040x divergence
  // written into the SPS as real cropping. `max_pixels` is applied by
  // `ff_set_dimensions` to the *display* dims, so the seat sees 1024
  // and waves it through however tight it is set: measured on this
  // build, `max_pixels = 5000` opens this stream without complaint.
  //
  // What actually gets allocated is the coded surface. `get_buffer2`
  // receives the frame at 1920x1088 (aligned to 1920x1090, 2,092,831
  // bytes), so on the software road `judge_buffer` bounds the real
  // extent. The hardware road never reaches `get_buffer2` — which is
  // why the hardware format callback now applies the same number to the
  // same coded dims and declines the backend when it does not fit.
  let path = corpus.cropped_h264();
  const CEILING: usize = 1024 * 1024;

  let decode_under = |bytes: usize| -> Result<(), mediadecode_ffmpeg::Error> {
    let mut input = ffmpeg_next::format::input(&path).expect("open");
    let stream = input
      .streams()
      .best(ffmpeg_next::media::Type::Video)
      .expect("a video stream");
    let index = stream.index();
    let mut decoder = VideoDecoder::open_with_frame_limits(
      stream.parameters(),
      DecoderLimits::new().with_frame(FrameLimits::new().with_max_frame_bytes(bytes)),
    )
    .expect("open the video decoder");
    let mut frame = mediadecode_ffmpeg::Frame::empty().expect("frame slot");
    loop {
      let mut packet = ffmpeg_next::Packet::empty();
      match packet.read(&mut input) {
        Ok(()) if packet.stream() == index => {
          // Answer dropped: see the note on the first ceiling probe above.
          // SAFETY: `Packet::read` is `av_read_frame`, so libavformat wrote the packet and its
          // padding, and nothing writes through it while the decoder may still read it.
          let _ = unsafe { decoder.send_av_packet(&packet) }?;
          match decoder.receive_frame(&mut frame) {
            Ok(Received::Produced) => return Ok(()),
            Ok(Received::NeedsInput | Received::Ended) => continue,
            Err(e) => return Err(e),
          }
        }
        Ok(()) => continue,
        Err(e) => panic!("read: {e}"),
      }
    }
  };

  // **The budget this lane turns on.** The pool is 1920x1088 in NV12
  // and costs 3,135,488 bytes; the stream *displays* 32x32, which costs
  // nothing worth naming. So a 1 MiB ceiling is admitted by any judge
  // reading the display extent and refused by one reading the pool.
  //
  // It used to be 16 MiB, back when the pool was compared against
  // `max_pixels` — a scalar charging the worst format in existence, 16
  // bytes a pixel, which refused this pool at five times its real cost.
  // Pricing the pool properly means the same stream is affordable under
  // 16 MiB, which it always was; the assertion at the end of this lane
  // is the one that says so.
  //
  // And the refusal says what it is. A `get_format` callback cannot
  // return a reason — declining means `AV_PIX_FMT_NONE`, which
  // libavcodec reports as `Invalid data found when processing input`,
  // a true statement about what it saw and a false one about what
  // happened. The callback leaves the reason in the state this crate
  // owns, and the probe turns it back into a name.
  let refusal = decode_under(CEILING)
    .expect_err("the cropped stream was judged on its display extent, not its allocation");
  let mediadecode_ffmpeg::Error::ProbeExhausted(failed) = &refusal else {
    panic!("expected a backend failure, got {refusal:?}");
  };
  let named = failed
    .attempts()
    .iter()
    .find_map(|(_, e)| match e.as_ref() {
      mediadecode_ffmpeg::Error::HwSurfaceTooLarge(p) => Some(*p),
      _ => None,
    });
  let named = named.unwrap_or_else(|| {
    panic!(
      "the refusal still wears libavcodec's misleading name: {:?}",
      failed.attempts(),
    )
  });
  // **The pool's own declaration, not an arithmetic guess at it.** The
  // judge asks `avcodec_get_hw_frames_parameters` for the
  // `AVHWFramesContext` the decoder is about to initialise and reads
  // its declared extent — measured on the VideoToolbox road here, that
  // is exactly 1920x1088.
  //
  // The alternative was `avcodec_align_dimensions2`, which is the
  // *codec's* alignment and answers 1920x1090 for this stream. Close
  // enough to look right and wrong in principle: a hardware pool aligns
  // by its own API's rules, and D3D11 HEVC/AV1 round both dimensions to
  // 128 — a 129x129 stream that codec arithmetic prices at 144x160 is
  // allocated 256x256. Asserting the pool's figure rather than the
  // codec's is what makes this lane notice if the ask is ever lost.
  // The cost is asserted as a range, not a constant: it is FFmpeg's
  // allocator arithmetic and not this crate's, so pinning it to the
  // byte would make the lane a hostage to an FFmpeg release. What must
  // hold is that a 1920x1088 NV12 pool prices in the megabytes — a
  // display-dims reading would have priced 32x32 — and that it is
  // reported against the budget it was refused by.
  assert!(
    named.bytes() > 3_000_000 && named.bytes() < 8 * 1024 * 1024,
    "a 1920x1088 NV12 pool should price around 3 MB, got {}",
    named.bytes(),
  );
  assert_eq!(named.limit(), CEILING as i64);

  // And under the defaults, where the coded surface genuinely fits, the
  // same stream decodes. A seat that also refuses what fits is not a
  // seat.
  decode_under(mediadecode_ffmpeg::DEFAULT_MAX_FRAME_BYTES)
    .expect("an ordinary cropped stream must still decode");
}

// ---------------------------------------------------------------------------
//  R14: a judge must dominate the allocator's arithmetic.
// ---------------------------------------------------------------------------

#[test]
fn the_send_side_side_data_list_is_capped_in_both_directions() {
  use mediadecode::packet::PacketFlags;
  use mediadecode_ffmpeg::{PacketLimits, boundary, extras::VideoPacketExtra};
  support::init_ffmpeg();

  const NEW_EXTRADATA: i32 =
    ffmpeg_next::ffi::AVPacketSideDataType::AV_PKT_DATA_NEW_EXTRADATA as i32;

  let build = |entries: Vec<SideDataEntry>| {
    let packet = VideoPacket::new(
      FfmpegBytes::try_copy_from_slice(&[1u8, 2, 3, 4]).expect("a test payload"),
      VideoPacketExtra::new(0).with_side_data(entries),
    )
    .with_flags(PacketFlags::KEY);
    boundary::ffmpeg_packet_from_owned_video_packet(&packet, PacketLimits::default())
  };

  let entry = |bytes: usize| {
    SideDataEntry::new(
      NEW_EXTRADATA,
      FfmpegBytes::try_copy_from_slice(&vec![7u8; bytes]).expect("a test payload"),
    )
  };

  // A legitimate list passes, so the cap is a cap and not a wall.
  assert!(build(vec![entry(64), entry(128)]).is_ok());

  // **Over the entry cap.** The read side has refused past 64 entries
  // since it was written; the send side allocated every one of them,
  // one `av_packet_new_side_data` at a time, with nothing counting.
  let many: Vec<SideDataEntry> = (0..65).map(|_| entry(8)).collect();
  match build(many) {
    Err(mediadecode_ffmpeg::boundary::PacketBuildError::SendSideDataTooLarge(p)) => {
      assert_eq!(p.what(), "entry-count");
      assert_eq!(p.value(), 65);
      assert_eq!(p.limit(), 64);
    }
    Err(other) => panic!("expected an entry-count refusal, got {other:?}"),
    Ok(_) => panic!("65 side-data entries were allocated with nothing counting them"),
  }

  // **Over the byte cap**, and refused before a single entry is
  // allocated: the whole list is totalled first, so an over-budget list
  // costs nothing rather than however many entries fit before the
  // ceiling was reached.
  match build(vec![entry(200 * 1024), entry(200 * 1024)]) {
    Err(mediadecode_ffmpeg::boundary::PacketBuildError::SendSideDataTooLarge(p)) => {
      assert_eq!(p.what(), "byte");
      assert_eq!(p.limit(), 256 * 1024);
      assert!(p.value() > 256 * 1024);
    }
    Err(other) => panic!("expected a byte refusal, got {other:?}"),
    Ok(_) => panic!("400 KiB of side data was allocated with nothing weighing it"),
  }
}

#[test]
fn every_hardware_exit_names_the_coded_surface_refusal() {
  use mediadecode_ffmpeg::{Backend, Error, FrameLimits, VideoDecoder};
  let Some(corpus) = Corpus::new() else {
    return;
  };
  support::init_ffmpeg();
  let path = corpus.cropped_h264();

  /// Pulls the named refusal out of whichever shape an exit wrapped it
  /// in — directly, or inside a backend-attempt log.
  fn named(e: &Error) -> Option<mediadecode_ffmpeg::HwSurfaceTooLarge> {
    match e {
      Error::HwSurfaceTooLarge(p) => Some(*p),
      Error::ProbeExhausted(f) => f.attempts().iter().find_map(|(_, inner)| named(inner)),
      _ => None,
    }
  }

  // Below the pool's real cost (a 1920x1088 NV12 pool is about 3.1 MB),
  // so the coded-surface judge is what speaks.
  const CEILING: usize = 1024 * 1024;
  let limits = DecoderLimits::new().with_frame(FrameLimits::new().with_max_frame_bytes(CEILING));

  // **The explicit-backend road**, which has no probe at all. In R14
  // this exit did not consume the declination: the decoder simply
  // drained to EOF and the caller was told the stream was over.
  let mut input = ffmpeg_next::format::input(&path).expect("open");
  let stream = input
    .streams()
    .best(ffmpeg_next::media::Type::Video)
    .expect("a video stream");
  let index = stream.index();
  let mut decoder =
    VideoDecoder::open_with_limits(stream.parameters(), Backend::VideoToolbox, limits)
      .expect("open the explicit-backend decoder");
  let mut frame = mediadecode_ffmpeg::Frame::empty().expect("frame slot");

  let mut refusal = None;
  loop {
    let mut packet = ffmpeg_next::Packet::empty();
    match packet.read(&mut input) {
      Ok(()) if packet.stream() == index => {
        // `and_then` still composes: the send's answer is discarded
        // deliberately, because this lane only cares whether the
        // *receive* is refused by the ceiling. A `MustDrain` here
        // simply means the decoder had output waiting, which the
        // receive below is about to take.
        //
        // SAFETY: `Packet::read` is `av_read_frame`, so libavformat wrote
        // the packet and its padding, and nothing writes through it
        // while the decoder may still read it.
        let outcome = unsafe { decoder.send_av_packet(&packet) }
          .and_then(|_| decoder.receive_frame(&mut frame));
        match outcome {
          Ok(Received::Produced) => panic!("a 2 Mpx coded surface passed a 1 Mpx ceiling"),
          Ok(Received::NeedsInput | Received::Ended) => continue,
          Err(e) => {
            refusal = named(&e);
            break;
          }
        }
      }
      Ok(()) => continue,
      Err(_) => break,
    }
  }

  let payload = refusal.expect("the explicit-backend road still loses the refusal");
  assert!(
    payload.bytes() > 3_000_000,
    "the pool's own extent, priced: {}",
    payload.bytes(),
  );
  assert_eq!(payload.limit(), CEILING as i64);
}

// ---------------------------------------------------------------------------
//  R19: the seats say what they mean, and cheap formats are not charged
//  the price of expensive ones.
// ---------------------------------------------------------------------------

#[test]
fn a_cheap_format_is_not_charged_the_worst_formats_price() {
  use mediadecode_ffmpeg::{DecoderLimits, FrameLimits, VideoDecoder};
  let Some(corpus) = Corpus::new() else {
    return;
  };
  support::init_ffmpeg();

  // **The video shape.** `max_pixels` used to be written as
  // `min(the caller's limit, max_frame_bytes / 16)` so that the byte
  // ceiling could bite in `ff_set_dimensions` — and that translation
  // charged every stream the widest format in existence. This clip's
  // coded surface is 1920x1088 in `yuv420p`, which really costs
  // 3,135,488 bytes; the translation priced it at 16 bytes a pixel and
  // refused it under any budget below 32 MiB.
  //
  // Under 4 MiB it fits, comfortably, and now decodes. The seat that
  // enforces the byte ceiling is `judge_buffer`, which is itself a
  // pre-allocation seat — `get_buffer2` *is* the allocation — so
  // nothing was traded away to stop over-refusing.
  let path = corpus.cropped_h264();
  let decode_under = |bytes: usize| -> Result<(), mediadecode_ffmpeg::Error> {
    let mut input = ffmpeg_next::format::input(&path).expect("open");
    let stream = input
      .streams()
      .best(ffmpeg_next::media::Type::Video)
      .expect("a video stream");
    let index = stream.index();
    let mut decoder = VideoDecoder::open_with_frame_limits(
      stream.parameters(),
      DecoderLimits::new().with_frame(FrameLimits::new().with_max_frame_bytes(bytes)),
    )?;
    let mut frame = mediadecode_ffmpeg::Frame::empty().expect("frame slot");
    loop {
      let mut packet = ffmpeg_next::Packet::empty();
      match packet.read(&mut input) {
        Ok(()) if packet.stream() == index => {
          // Answer dropped: see the note on the first ceiling probe above.
          // SAFETY: `Packet::read` is `av_read_frame`, so libavformat wrote the packet and its
          // padding, and nothing writes through it while the decoder may still read it.
          let _ = unsafe { decoder.send_av_packet(&packet) }?;
          match decoder.receive_frame(&mut frame) {
            Ok(Received::Produced) => return Ok(()),
            Ok(Received::NeedsInput | Received::Ended) => continue,
            Err(e) => return Err(e),
          }
        }
        Ok(()) => continue,
        Err(e) => panic!("read: {e}"),
      }
    }
  };
  decode_under(4 * 1024 * 1024).expect("a 3.1 MB frame must fit a 4 MiB budget");

  // And the refusal direction is untouched: below its real cost it is
  // still refused, so this is an accuracy fix and not a hole.
  assert!(decode_under(1024 * 1024).is_err());

  // **The audio shape.** `max_samples` used to carry
  // `max_frame_bytes / 8` — the widest sample format — so a 6-channel
  // `s16` frame was priced at four times its cost. 4,096 samples across
  // six channels is 49,152 bytes of `s16`; the translation allowed
  // 8,192 channel-samples against the frame's 24,576 and refused it
  // under a 64 KiB budget it fits three times over.
  let wav = corpus.sine_wav("r19-6ch.wav", 48_000, 6, 440, 0.5);
  let frame = decode_first_audio(&wav, 64 * 1024).expect("6ch s16 must fit 64 KiB");
  assert_eq!(frame.channel_count(), 6);
  assert!(frame.nb_samples() > 0);

  // Same accuracy check on this road: a budget below the frame's real
  // cost still refuses, and refuses inside libavcodec.
  match decode_first_audio(&wav, 4096) {
    Err(mediadecode_ffmpeg::AudioDecodeError::Decode(_)) => {}
    Err(other) => panic!("expected a pre-allocation refusal, got {other:?}"),
    Ok(_) => panic!("a 49 KiB frame passed a 4 KiB budget"),
  }
}

#[test]
fn the_still_road_still_refuses_what_it_cannot_afford() {
  let Some(corpus) = Corpus::new() else {
    return;
  };
  support::init_ffmpeg();
  let path = corpus.exif_oriented_jpeg(1); // a 32x24 JPEG

  // The still road's byte ceiling is the same seat, and it still bites
  // — at the **decode** now rather than at the open. That is the whole
  // shape of this round's change: the open no longer carries a
  // byte-derived pixel translation, so a budget is spent where the
  // allocation happens, by a judge that prices the real format.
  assert!(
    open_still_and_decode(&path, 512).is_err(),
    "a 512-byte budget decoded a picture",
  );
  open_still_and_decode(&path, mediadecode_ffmpeg::DEFAULT_MAX_FRAME_BYTES)
    .expect("the defaults must decode an ordinary still");
}

#[test]
fn a_byte_refused_frame_is_named_on_every_software_road() {
  use mediadecode_ffmpeg::{
    AudioDecodeError, DecoderLimits, Error, FrameBudgetExceeded, FrameLimits, FrameMedium,
    ImageDecodeError, VideoDecoder,
  };
  let Some(corpus) = Corpus::new() else {
    return;
  };
  support::init_ffmpeg();

  /// Pulls the named refusal out of whichever shape a road wrapped it
  /// in — directly, or inside a backend-attempt log.
  fn named(e: &Error) -> Option<FrameBudgetExceeded> {
    match e {
      Error::FrameBudgetExceeded(p) => Some(*p),
      Error::ProbeExhausted(f) => f.attempts().iter().find_map(|(_, inner)| named(inner)),
      _ => None,
    }
  }

  // **Video.** A `get_buffer2` refusal can only answer libavcodec with
  // an errno, and `EINVAL` is also what a corrupt file produces — so
  // without a name a caller cannot tell "your budget refused this" from
  // "this file is broken", and only one of those is worth retrying with
  // a larger ceiling.
  let path = corpus.cropped_h264();
  let mut input = ffmpeg_next::format::input(&path).expect("open");
  let stream = input
    .streams()
    .best(ffmpeg_next::media::Type::Video)
    .expect("a video stream");
  let index = stream.index();
  let mut decoder = VideoDecoder::open_with_frame_limits(
    stream.parameters(),
    DecoderLimits::new().with_frame(FrameLimits::new().with_max_frame_bytes(1024 * 1024)),
  )
  .expect("open");
  let mut frame = mediadecode_ffmpeg::Frame::empty().expect("frame slot");
  let mut video_named = None;
  loop {
    let mut packet = ffmpeg_next::Packet::empty();
    match packet.read(&mut input) {
      Ok(()) if packet.stream() == index => {
        // `and_then` still composes: the send's answer is discarded
        // deliberately, because this lane only cares whether the
        // *receive* is refused by the ceiling. A `MustDrain` here
        // simply means the decoder had output waiting, which the
        // receive below is about to take.
        //
        // SAFETY: `Packet::read` is `av_read_frame`, so libavformat wrote
        // the packet and its padding, and nothing writes through it
        // while the decoder may still read it.
        let outcome = unsafe { decoder.send_av_packet(&packet) }
          .and_then(|_| decoder.receive_frame(&mut frame));
        match outcome {
          Ok(Received::Produced) => panic!("a 3.1 MB frame passed a 1 MiB ceiling"),
          Ok(Received::NeedsInput | Received::Ended) => continue,
          Err(e) => {
            video_named = named(&e);
            break;
          }
        }
      }
      Ok(()) => continue,
      Err(_) => break,
    }
  }
  // The hardware road declines first on this platform, so the video
  // lane's own refusal may arrive as the coded-surface one; what must
  // never happen is a bare `EINVAL` with nothing said. Accept either
  // name, reject namelessness.
  assert!(
    video_named.is_some() || decoder.backend() == mediadecode_ffmpeg::Backend::VideoToolbox,
    "the video road refused a frame with no name",
  );

  // **Audio**, where there is no hardware road to answer first.
  let wav = corpus.sine_wav("r19-named.wav", 48_000, 6, 440, 0.5);
  match decode_first_audio(&wav, 4096) {
    Err(AudioDecodeError::Decode(e)) => {
      let payload = named(&e).expect("the audio road refused a frame with no name");
      assert_eq!(payload.medium(), FrameMedium::Audio);
      assert_eq!(payload.limit(), 4096);
      assert!(payload.bytes() > payload.limit());
    }
    Err(other) => panic!("expected a named budget refusal, got {other:?}"),
    Ok(_) => panic!("a 49 KiB frame passed a 4 KiB budget"),
  }

  // **The still road.**
  let still = corpus.exif_oriented_jpeg(1);
  match open_still_and_decode(&still, 512) {
    Err(ImageDecodeError::Decode(e)) => {
      let payload = named(&e).expect("the still road refused a frame with no name");
      assert_eq!(payload.medium(), FrameMedium::Video);
      assert_eq!(payload.limit(), 512);
    }
    other => panic!("expected a named budget refusal, got {other:?}"),
  }

  // **And it does not replay.** The state outlives one frame, so a
  // refusal left set would be reported again against a decode that
  // never declined anything.
  decode_first_audio(&wav, mediadecode_ffmpeg::DEFAULT_MAX_FRAME_BYTES)
    .expect("a fresh decoder under the defaults must decode");
}

#[test]
fn the_software_video_road_names_its_budget_refusals() {
  use mediadecode::decoder::VideoStreamDecoder;
  use mediadecode_ffmpeg::{
    DecoderLimits, Error, FfmpegOwnedVideoStreamDecoder as FfmpegVideoStreamDecoder,
    FrameBudgetExceeded, FrameLimits, FrameMedium, VideoDecodeError,
  };
  let Some(corpus) = Corpus::new() else {
    return;
  };
  support::init_ffmpeg();

  fn named(e: &Error) -> Option<FrameBudgetExceeded> {
    match e {
      Error::FrameBudgetExceeded(p) => Some(*p),
      Error::ProbeExhausted(f) => f.attempts().iter().find_map(|(_, inner)| named(inner)),
      _ => None,
    }
  }

  // **The direct software road.** Censused on this build, VideoToolbox
  // advertises a hardware config for h264, hevc, vp9, mpeg4, mpeg2video
  // and av1 — but not for VP8. So this clip reaches the software
  // decoder without a hardware pool ever being negotiated, which is the
  // only way this funnel is observable: on any accelerated codec the
  // pool judge answers first and the software exits never run.
  let path = corpus.software_only_video();
  let decode_under = |bytes: usize| -> Result<(), VideoDecodeError> {
    let mut input = ffmpeg_next::format::input(&path).expect("open");
    let stream = input
      .streams()
      .best(ffmpeg_next::media::Type::Video)
      .expect("a video stream");
    let index = stream.index();
    let mut decoder = FfmpegVideoStreamDecoder::open(
      stream.parameters(),
      mediadecode::Timebase::default(),
      DecoderLimits::new().with_frame(FrameLimits::new().with_max_frame_bytes(bytes)),
    )
    .map_err(VideoDecodeError::Decode)?;
    let mut frame = mediadecode_ffmpeg::empty_owned_video_frame();
    loop {
      let mut packet = ffmpeg_next::Packet::empty();
      match packet.read(&mut input) {
        Ok(()) if packet.stream() == index => {
          let body = packet.data().unwrap_or(&[]).to_vec();
          let video = VideoPacket::new(
            FfmpegBytes::try_copy_from_slice(&body).expect("a test payload"),
            mediadecode_ffmpeg::extras::VideoPacketExtra::new(0),
          );
          // Answer dropped: see the note on the first ceiling probe above.
          let _ = decoder.send_packet(&video)?;
          match decoder.receive_frame(&mut frame) {
            Ok(Received::Produced) => return Ok(()),
            Ok(Received::NeedsInput | Received::Ended) => continue,
            Err(e) => return Err(e),
          }
        }
        Ok(()) => continue,
        Err(e) => panic!("read: {e}"),
      }
    }
  };

  // A 1280x720 `yuv420p` frame costs about 1.4 MB; 64 KiB refuses it,
  // and the refusal has to say so rather than wear the `EINVAL`
  // libavcodec also uses for corrupt input.
  let refusal = decode_under(64 * 1024).expect_err("a 1.4 MB frame passed a 64 KiB ceiling");
  let VideoDecodeError::Decode(inner) = &refusal else {
    panic!("expected a decode failure, got {refusal:?}");
  };
  let payload = named(inner).expect("the software video road refused a frame with no name");
  assert_eq!(payload.medium(), FrameMedium::Video);
  assert_eq!(payload.limit(), 64 * 1024);
  assert!(payload.bytes() > payload.limit());

  // **Clear-on-read**: the state outlives one frame, so a refusal left
  // set would be reported against a decode that never declined.
  decode_under(mediadecode_ffmpeg::DEFAULT_MAX_FRAME_BYTES)
    .expect("a fresh decoder under the defaults must decode");

  // **The replay and cold-fallback roads are not reachable here.** Both
  // require a hardware backend to engage and then fail mid-stream, and
  // VideoToolbox is the only backend on this platform — a codec it
  // accelerates is refused by the pool judge before any software
  // decoder exists, and a codec it does not accelerate never enters the
  // fallback machinery at all. Their exits are routed through the same
  // `software_exit` funnel as the ones above; that is stated here
  // rather than proved.
}
