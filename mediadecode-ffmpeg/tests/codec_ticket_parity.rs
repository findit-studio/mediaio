//! The codec ticket's parity proof.
//!
//! [`CodecTicket`] replaced a `*mut AVCodecParameters` inside every
//! track row with an owned Rust mirror. The whole road rests on one
//! claim: **a rebuilt `AVCodecParameters` is the original**. Not
//! "close enough for the decoder", not "every field somebody
//! remembered" — equal, field by field, across the whole struct.
//!
//! So that is what this file asserts, and it asserts it the only way
//! that cannot rot: [`compare`] names all thirty-two fields FFmpeg
//! n9.0 declares, in the struct's own order, and a field nobody
//! compared is a field this file does not have. `avcodec_parameters_
//! to_context` reads a different subset per medium — video takes the
//! colour and geometry fields, audio takes the layout and padding
//! fields, subtitle takes only width and height — and every one of the
//! thirty-two is read on at least one of those branches, so nothing
//! here is decorative.
//!
//! # What is swept
//!
//! Every stream of every container the corpus can mint, which is
//! where the real shapes live: H.264 with genuine SPS/PPS extradata,
//! AAC with its `AudioSpecificConfig`, SubRip, a TTF attachment whose
//! payload *is* its extradata, MP3 with a PNG cover, a `tmcd` data
//! track, VP8, and 6-channel FLAC for a populated channel mask.
//!
//! Plus one container the CLI cannot produce and this file therefore
//! assembles byte by byte: a MOV whose video sample entry carries a
//! `colr` atom of colour type `prof`, which libavformat turns into an
//! `AV_PKT_DATA_ICC_PROFILE` entry of `coded_side_data`. That field is
//! the one this crate's parameter budgets were written for, and
//! minting it is what lets the sweep prove a *demuxer* populates it
//! rather than only that an absent field crosses. Nothing binary is
//! committed: see [`mint_mp4_with_icc_profile`].
//!
//! Then the shapes no container will hand over at all — a custom
//! channel map with per-channel names, a side-data kind these bindings
//! cannot name, a layout carrying the user pointer the mirror refuses,
//! every scalar set off its default — are built beside their
//! assertions.
//!
//! And a decoder is opened through a rebuilt ticket and made to
//! produce a frame, because equality of a struct is not by itself
//! proof that libavcodec agrees.
//!
//! [`CodecTicket`]: mediadecode_ffmpeg::CodecTicket

mod support;

use ffmpeg_next::{
  codec::Parameters,
  ffi::{
    AV_INPUT_BUFFER_PADDING_SIZE, AVChannelCustom, AVChannelOrder, AVCodecParameters,
    AVPacketSideData, av_mallocz,
  },
};
use mediadecode::{
  Received, Sent, Timebase,
  decoder::{AudioStreamDecoder, VideoStreamDecoder},
  demuxer::{Demuxer, TrackKind},
};
use mediadecode_ffmpeg::{
  CodecTicket, DecoderLimits, DemuxError, DemuxLimits, FfmpegOwnedAudioStreamDecoder,
  FfmpegOwnedDemuxer, FfmpegOwnedVideoStreamDecoder, OwnedAudioFrame, OwnedVideoFrame,
  PacketLimits, ParametersError, empty_owned_audio_frame, empty_owned_video_frame,
  owned_audio_packet_from_ffmpeg_in, owned_video_packet_from_ffmpeg_in,
};

// ---------------------------------------------------------------------------
//  The comparator: all thirty-two fields, in the struct's own order.
// ---------------------------------------------------------------------------

/// Compares two `AVCodecParameters` field by field, returning one
/// message per disagreement.
///
/// Every enum field is read as the raw `i32` it is on the wire rather
/// than through its bindgen enum — the same discipline the mirror
/// itself is written to, and for the same reason: a comparator that
/// materialised an unnamed discriminant would be undefined behaviour
/// on exactly the file this suite exists to notice.
///
/// # Safety
///
/// `src` and `dst` must both be live `*const AVCodecParameters`.
unsafe fn compare(src: *const AVCodecParameters, dst: *const AVCodecParameters) -> Vec<String> {
  use core::ptr::{addr_of, read_unaligned};

  let mut bad = Vec::new();
  macro_rules! scalar {
    ($name:ident) => {
      // SAFETY: both are live per this function's contract.
      let (a, b) = unsafe { ((*src).$name, (*dst).$name) };
      if a != b {
        bad.push(format!("{}: {:?} != {:?}", stringify!($name), a, b));
      }
    };
  }
  macro_rules! raw_enum {
    ($name:ident) => {
      // SAFETY: both are live; `addr_of!` reaches the field without
      // forming a reference to a struct whose enum may hold a
      // discriminant these bindings cannot name.
      let (a, b) = unsafe {
        (
          read_unaligned(addr_of!((*src).$name).cast::<i32>()),
          read_unaligned(addr_of!((*dst).$name).cast::<i32>()),
        )
      };
      if a != b {
        bad.push(format!("{}: {} != {}", stringify!($name), a, b));
      }
    };
  }
  macro_rules! rational {
    ($name:ident) => {
      // SAFETY: both are live; `AVRational` is two `c_int`s by value.
      let (a, b) = unsafe { ((*src).$name, (*dst).$name) };
      if (a.num, a.den) != (b.num, b.den) {
        bad.push(format!(
          "{}: {}/{} != {}/{}",
          stringify!($name),
          a.num,
          a.den,
          b.num,
          b.den,
        ));
      }
    };
  }

  //  1-3. identity
  raw_enum!(codec_type);
  raw_enum!(codec_id);
  scalar!(codec_tag);

  //  4-5. extradata
  // SAFETY: both are live.
  let (src_ex, src_len, dst_ex, dst_len) = unsafe {
    (
      (*src).extradata,
      (*src).extradata_size,
      (*dst).extradata,
      (*dst).extradata_size,
    )
  };
  if src_len != dst_len {
    bad.push(format!("extradata_size: {src_len} != {dst_len}"));
  } else if src_ex.is_null() != dst_ex.is_null() {
    bad.push(format!(
      "extradata: presence differs ({} vs {})",
      src_ex.is_null(),
      dst_ex.is_null(),
    ));
  } else if !src_ex.is_null() && src_len > 0 {
    if src_ex == dst_ex {
      bad.push("extradata: the rebuild aliased the original".to_owned());
    }
    let len = src_len as usize;
    // SAFETY: FFmpeg's contract makes `extradata` readable for
    // `extradata_size` bytes plus `AV_INPUT_BUFFER_PADDING_SIZE`, on
    // both sides — the original because libavformat allocated it that
    // way, the rebuild because `write_extradata` did.
    let (a, b) = unsafe {
      (
        core::slice::from_raw_parts(src_ex, len + AV_INPUT_BUFFER_PADDING_SIZE as usize),
        core::slice::from_raw_parts(dst_ex, len + AV_INPUT_BUFFER_PADDING_SIZE as usize),
      )
    };
    if a[..len] != b[..len] {
      bad.push(format!("extradata: {len} payload bytes differ"));
    }
    // The zeroes decoders read past the end into are part of the
    // contract, not slack: `avcodec_parameters_copy` guarantees them
    // and so must the rebuild.
    if b[len..].iter().any(|&byte| byte != 0) {
      bad.push("extradata: the rebuild's trailing padding is not zeroed".to_owned());
    }
  }

  //  6-7. coded_side_data
  // SAFETY: both are live.
  let (src_sd, src_n, dst_sd, dst_n) = unsafe {
    (
      (*src).coded_side_data,
      (*src).nb_coded_side_data,
      (*dst).coded_side_data,
      (*dst).nb_coded_side_data,
    )
  };
  if src_n != dst_n {
    bad.push(format!("nb_coded_side_data: {src_n} != {dst_n}"));
  } else if src_n > 0 {
    if src_sd == dst_sd {
      bad.push("coded_side_data: the rebuild aliased the original".to_owned());
    }
    for index in 0..src_n as usize {
      // Field pointers only — `type` is an open C enum.
      // SAFETY: both arrays hold `nb_coded_side_data` entries and
      // `index` is below that count.
      let (a_kind, a_size, a_data, b_kind, b_size, b_data) = unsafe {
        let a = src_sd.add(index);
        let b = dst_sd.add(index);
        (
          read_unaligned(addr_of!((*a).type_).cast::<i32>()),
          read_unaligned(addr_of!((*a).size)),
          read_unaligned(addr_of!((*a).data)),
          read_unaligned(addr_of!((*b).type_).cast::<i32>()),
          read_unaligned(addr_of!((*b).size)),
          read_unaligned(addr_of!((*b).data)),
        )
      };
      if a_kind != b_kind {
        bad.push(format!(
          "coded_side_data[{index}].type: {a_kind} != {b_kind}"
        ));
      }
      if a_size != b_size {
        bad.push(format!(
          "coded_side_data[{index}].size: {a_size} != {b_size}"
        ));
      } else if a_size > 0 && !a_data.is_null() && !b_data.is_null() {
        if a_data == b_data {
          bad.push(format!("coded_side_data[{index}]: aliased the original"));
        }
        // SAFETY: each descriptor declares `size` readable bytes.
        let (a, b) = unsafe {
          (
            core::slice::from_raw_parts(a_data, a_size),
            core::slice::from_raw_parts(b_data, b_size),
          )
        };
        if a != b {
          bad.push(format!(
            "coded_side_data[{index}]: {a_size} payload bytes differ"
          ));
        }
      }
    }
  }

  //  8-15. the shared scalars and video geometry
  scalar!(format);
  scalar!(bit_rate);
  scalar!(bits_per_coded_sample);
  scalar!(bits_per_raw_sample);
  scalar!(profile);
  scalar!(level);
  scalar!(width);
  scalar!(height);

  // 16-24. the video fields
  rational!(sample_aspect_ratio);
  rational!(framerate);
  raw_enum!(field_order);
  raw_enum!(color_range);
  raw_enum!(color_primaries);
  raw_enum!(color_trc);
  raw_enum!(color_space);
  raw_enum!(chroma_location);
  scalar!(video_delay);

  // 25. ch_layout — the union is discriminated by `order`, so the
  //     comparison has to be too. Reading the `mask` arm of a CUSTOM
  //     layout would compare two pointers that are *supposed* to
  //     differ and call a correct rebuild a failure.
  // SAFETY: both are live; `ch_layout` is embedded by value.
  let (a_order, a_ch, a_opaque, b_order, b_ch, b_opaque) = unsafe {
    (
      read_unaligned(addr_of!((*src).ch_layout.order).cast::<i32>()),
      (*src).ch_layout.nb_channels,
      (*src).ch_layout.opaque,
      read_unaligned(addr_of!((*dst).ch_layout.order).cast::<i32>()),
      (*dst).ch_layout.nb_channels,
      (*dst).ch_layout.opaque,
    )
  };
  if a_order != b_order {
    bad.push(format!("ch_layout.order: {a_order} != {b_order}"));
  }
  if a_ch != b_ch {
    bad.push(format!("ch_layout.nb_channels: {a_ch} != {b_ch}"));
  }
  if a_opaque != b_opaque {
    bad.push("ch_layout.opaque: differs".to_owned());
  }
  if a_order == b_order {
    if a_order == AVChannelOrder::AV_CHANNEL_ORDER_CUSTOM as i32 {
      // SAFETY: the order names the `map` arm on both sides.
      let (a_map, b_map) = unsafe { ((*src).ch_layout.u.map, (*dst).ch_layout.u.map) };
      if a_map.is_null() != b_map.is_null() {
        bad.push("ch_layout.u.map: presence differs".to_owned());
      } else if !a_map.is_null() {
        if a_map == b_map {
          bad.push("ch_layout.u.map: the rebuild aliased the original".to_owned());
        }
        for index in 0..a_ch.max(0) as usize {
          // SAFETY: the map holds `nb_channels` entries and `index`
          // is below that count; `id` is an open enum, read raw.
          let (a_id, a_name, a_op, b_id, b_name, b_op) = unsafe {
            let a = a_map.add(index);
            let b = b_map.add(index);
            (
              read_unaligned(addr_of!((*a).id).cast::<i32>()),
              read_unaligned(addr_of!((*a).name).cast::<[u8; 16]>()),
              read_unaligned(addr_of!((*a).opaque)),
              read_unaligned(addr_of!((*b).id).cast::<i32>()),
              read_unaligned(addr_of!((*b).name).cast::<[u8; 16]>()),
              read_unaligned(addr_of!((*b).opaque)),
            )
          };
          if (a_id, a_name, a_op) != (b_id, b_name, b_op) {
            bad.push(format!("ch_layout.u.map[{index}]: differs"));
          }
        }
      }
    } else if a_order == AVChannelOrder::AV_CHANNEL_ORDER_NATIVE as i32
      || a_order == AVChannelOrder::AV_CHANNEL_ORDER_AMBISONIC as i32
    {
      // SAFETY: these two orders — and only these two — name the
      // `mask` arm.
      let (a_mask, b_mask) = unsafe { ((*src).ch_layout.u.mask, (*dst).ch_layout.u.mask) };
      if a_mask != b_mask {
        bad.push(format!("ch_layout.u.mask: {a_mask:#x} != {b_mask:#x}"));
      }
    }
    // `UNSPEC`, and any order this build does not name: the union is
    // undefined for them, so there is nothing here to compare. This
    // comparator used to read the mask for every non-`CUSTOM` order and
    // would have called a correct rebuild broken over a field that
    // means nothing — the same defect the mirror itself carried.
  }

  // 26-32. the audio fields, and the alpha mode
  scalar!(sample_rate);
  scalar!(block_align);
  scalar!(frame_size);
  scalar!(initial_padding);
  scalar!(trailing_padding);
  scalar!(seek_preroll);
  raw_enum!(alpha_mode);

  bad
}

/// Mirrors `original`, rebuilds it, and asserts the two are equal.
///
/// Returns the ticket, so a caller can go on to open a decoder from
/// the very parameters this proved faithful.
fn assert_round_trip(original: &Parameters, what: &str) -> CodecTicket {
  let ticket = CodecTicket::mirror(original, 0, usize::MAX)
    .unwrap_or_else(|e| panic!("{what}: the mirror refused: {e}"));
  let rebuilt = ticket
    .rebuild()
    .unwrap_or_else(|e| panic!("{what}: the rebuild failed: {e}"));

  // SAFETY: both own a live `AVCodecParameters` for this call.
  let bad = unsafe { compare(original.as_ptr(), rebuilt.as_ptr()) };
  assert!(
    bad.is_empty(),
    "{what}: the rebuild is not the original:\n  {}",
    bad.join("\n  ")
  );

  // The footprint is a claim about the rebuild, so it is checked
  // against one rather than against itself.
  let rebuilt_again = ticket.rebuild().expect("a second rebuild");
  // SAFETY: both are live.
  let bad = unsafe { compare(original.as_ptr(), rebuilt_again.as_ptr()) };
  assert!(bad.is_empty(), "{what}: the second rebuild diverged");
  ticket
}

// ---------------------------------------------------------------------------
//  The corpus sweep — every stream of every container.
// ---------------------------------------------------------------------------

/// One fixture: its name, and a container this suite can produce.
///
/// `icc.mp4` is not the corpus's — it is minted byte by byte by
/// [`mint_mp4_with_icc_profile`], written into the corpus directory so
/// it lives and dies with the run. It is in this list because it is
/// the only container in it that carries stream-level
/// `coded_side_data`, and a sweep that never saw that field populated
/// would be a sweep that proved nothing about it.
fn fixtures(corpus: &support::Corpus) -> Vec<(&'static str, std::path::PathBuf)> {
  let minted = corpus.dir().join("icc.mp4");
  std::fs::write(
    &minted,
    mint_mp4_with_icc_profile(&icc_profile_bytes(3_072)),
  )
  .expect("write the minted container");
  vec![
    // Hand-minted: a MOV `colr`/`prof` atom carrying an ICC profile.
    ("icc.mp4", minted),
    // H.264 + AAC + SubRip + a TTF attachment, all in one Matroska.
    ("multi.mkv", corpus.multi_track_mkv()),
    // MP3 with a PNG cover riding an `attached_pic` video stream.
    ("cover.mp3", corpus.cover_art_mp3()),
    // H.264 + AAC + a `tmcd` data track, in QuickTime.
    ("timecode.mov", corpus.timecode_mov()),
    // H.264 whose SPS carries real cropping.
    ("cropped.mp4", corpus.cropped_h264()),
    // VP8 in WebM — the software-only road.
    ("swonly.webm", corpus.software_only_video()),
    // 6-channel FLAC: a populated channel mask, s32 samples.
    ("surround.flac", corpus.surround_flac()),
    // Bare SubRip, from the queue-backed demuxer family.
    ("cues.srt", corpus.subrip()),
    // A 1-bit PNG, and an indexed one.
    ("mono.png", corpus.monochrome_png()),
    ("indexed.png", corpus.indexed_png()),
  ]
}

#[test]
fn every_stream_of_every_fixture_rebuilds_into_itself() {
  let Some(corpus) = support::Corpus::new() else {
    return;
  };

  let mut streams = 0usize;
  let mut with_extradata = 0usize;
  let mut with_side_data = 0usize;
  for (name, path) in fixtures(&corpus) {
    let input = ffmpeg_next::format::input(&path)
      .unwrap_or_else(|e| panic!("{name}: libavformat could not open the fixture: {e}"));
    for stream in input.streams() {
      let index = stream.index();
      let parameters = stream.parameters();
      let ticket = assert_round_trip(&parameters, &format!("{name} stream {index}"));
      streams += 1;
      if !ticket.extradata().is_empty() {
        with_extradata += 1;
      }
      if !ticket.coded_side_data().is_empty() {
        with_side_data += 1;
      }
    }
  }

  assert!(streams >= 16, "only {streams} streams swept");
  // Not decoration: a corpus that happened to carry no extradata
  // would pass every assertion above while proving nothing about the
  // field this whole road is really about — the SPS/PPS a decoder
  // cannot start without.
  assert!(
    with_extradata >= 4,
    "only {with_extradata} streams carried extradata; the sweep proves nothing about it",
  );
  // **`coded_side_data`, asserted rather than hoped for.** Censused on
  // this build: no container the `ffmpeg` CLI can mint carries
  // stream-level side data — `-metadata:s:v rotate=` no longer writes
  // a display matrix, and MOV, MP4 and Matroska emit none of their
  // own. So the sweep would have proved only that an *absent* field
  // crosses. `icc.mp4` is minted here for exactly that reason, and
  // this assertion is what keeps it in the list.
  assert!(
    with_side_data >= 1,
    "no stream carried coded side data; the minted fixture is not reaching the sweep",
  );
  eprintln!(
    "codec-ticket parity: {streams} streams, {with_extradata} with extradata, \
     {with_side_data} with coded side data",
  );
}

/// The same sweep, one tier up: through the demuxer's own track rows,
/// which is where the ticket actually lives.
#[test]
fn every_track_row_hands_back_the_parameters_the_file_declared() {
  let Some(corpus) = support::Corpus::new() else {
    return;
  };

  for (name, path) in fixtures(&corpus) {
    // The row's own parameters, read straight from libavformat, are
    // the reference. The row is built from the same stream, so the two
    // must agree field for field.
    let input = ffmpeg_next::format::input(&path).expect("open for reference");
    let reference: Vec<Parameters> = input.streams().map(|s| s.parameters()).collect();

    let demuxer = FfmpegOwnedDemuxer::open(&path).unwrap_or_else(|e| panic!("{name}: {e}"));
    for (index, track) in demuxer.tracks().iter().enumerate() {
      let extra = track.extra();
      let rebuilt = extra
        .clone_parameters()
        .unwrap_or_else(|e| panic!("{name} track {index}: {e}"));

      // The attachment road deliberately leaves `extradata` behind —
      // a font's extradata *is* the payload the carrier already holds,
      // and counting it twice would make the row carry the file. That
      // is the one documented divergence, and it is asserted rather
      // than excused.
      let attachment = track.kind() == TrackKind::Attachment;
      // SAFETY: both own a live `AVCodecParameters`.
      let bad = unsafe { compare(reference[index].as_ptr(), rebuilt.as_ptr()) };
      if attachment {
        assert!(
          bad.iter().all(|m| m.starts_with("extradata")),
          "{name} track {index}: an attachment diverged beyond its extradata:\n  {}",
          bad.join("\n  "),
        );
        assert!(
          extra.ticket().extradata().is_empty(),
          "{name} track {index}: the attachment road kept its extradata",
        );
      } else {
        assert!(
          bad.is_empty(),
          "{name} track {index}: the row is not the file:\n  {}",
          bad.join("\n  "),
        );
      }
      // The number the session admitted the stream at is the number a
      // rebuild costs — the budget still means what it said.
      assert_eq!(extra.parameter_bytes(), extra.ticket().footprint_bytes());
    }
  }
}

// ---------------------------------------------------------------------------
//  Decoding through a rebuilt ticket.
// ---------------------------------------------------------------------------

/// Struct equality is not libavcodec's agreement. A decoder is opened
/// from a rebuilt ticket — never from the demuxer's own handle — and
/// made to produce a frame on both the video and the audio road.
#[test]
fn a_decoder_opened_from_a_rebuilt_ticket_produces_frames() {
  let Some(corpus) = support::Corpus::new() else {
    return;
  };
  let path = corpus.multi_track_mkv();
  let mut input = ffmpeg_next::format::input(&path).expect("open multi.mkv");

  let video = input
    .streams()
    .best(ffmpeg_next::media::Type::Video)
    .expect("multi.mkv has video");
  let audio = input
    .streams()
    .best(ffmpeg_next::media::Type::Audio)
    .expect("multi.mkv has audio");
  let (video_index, audio_index) = (video.index(), audio.index());
  let video_tb = timebase_of(&video);
  let audio_tb = timebase_of(&audio);

  // Both decoders are opened from parameters that exist *only* because
  // the ticket rebuilt them — the original handles are dropped before
  // a packet is sent. H.264 cannot start without the SPS/PPS in
  // `extradata` and AAC cannot without its `AudioSpecificConfig`, so a
  // mirror that lost either fails here rather than subtly.
  let video_ticket = CodecTicket::mirror(&video.parameters(), video_index, usize::MAX)
    .expect("mirror the video parameters");
  let audio_ticket = CodecTicket::mirror(&audio.parameters(), audio_index, usize::MAX)
    .expect("mirror the audio parameters");
  assert!(
    !video_ticket.extradata().is_empty(),
    "libx264 wrote no extradata — this lane would prove nothing",
  );
  assert!(
    !audio_ticket.extradata().is_empty(),
    "the AAC encoder wrote no AudioSpecificConfig — this lane would prove nothing",
  );

  let mut video_decoder = FfmpegOwnedVideoStreamDecoder::open(
    video_ticket
      .rebuild()
      .expect("rebuild the video parameters"),
    video_tb,
    DecoderLimits::default(),
  )
  .expect("open a video decoder from the rebuilt ticket");
  let mut audio_decoder = FfmpegOwnedAudioStreamDecoder::open(
    audio_ticket
      .rebuild()
      .expect("rebuild the audio parameters"),
    audio_tb,
    DecoderLimits::default(),
  )
  .expect("open an audio decoder from the rebuilt ticket");

  let mut video_frame: OwnedVideoFrame = empty_owned_video_frame();
  let mut audio_frame: OwnedAudioFrame = empty_owned_audio_frame();
  let (mut video_frames, mut audio_frames) = (0usize, 0usize);

  for (stream, av_packet) in input.packets() {
    if video_frames > 0 && audio_frames > 0 {
      break;
    }
    let index = stream.index();
    if index == video_index && video_frames == 0 {
      let Some(packet) =
        // SAFETY: `packets()` reads each packet with `av_read_frame`, so libavformat wrote its
        // payload, and it is live for the call.
        unsafe { owned_video_packet_from_ffmpeg_in(&av_packet, video_tb, PacketLimits::default()) }
          .expect("a wrappable video payload")
      else {
        continue;
      };
      if video_decoder.send_packet(&packet).expect("send video") != Sent::Accepted {
        continue;
      }
      while let Received::Produced = video_decoder
        .receive_frame(&mut video_frame)
        .expect("video")
      {
        assert!(video_frame.width() > 0 && video_frame.height() > 0);
        video_frames += 1;
      }
    } else if index == audio_index && audio_frames == 0 {
      let Some(packet) =
        // SAFETY: `packets()` reads each packet with `av_read_frame`, so libavformat wrote its
        // payload, and it is live for the call.
        unsafe { owned_audio_packet_from_ffmpeg_in(&av_packet, audio_tb, PacketLimits::default()) }
          .expect("a wrappable audio payload")
      else {
        continue;
      };
      if audio_decoder.send_packet(&packet).expect("send audio") != Sent::Accepted {
        continue;
      }
      while let Received::Produced = audio_decoder
        .receive_frame(&mut audio_frame)
        .expect("audio")
      {
        assert!(audio_frame.sample_rate() > 0 && audio_frame.nb_samples() > 0);
        audio_frames += 1;
      }
    }
  }

  assert!(
    video_frames > 0,
    "H.264 decoded nothing through the rebuilt ticket",
  );
  assert!(
    audio_frames > 0,
    "AAC decoded nothing through the rebuilt ticket",
  );
}

/// A stream's timebase, in the core crate's vocabulary.
fn timebase_of(stream: &ffmpeg_next::format::stream::Stream<'_>) -> Timebase {
  let tb = stream.time_base();
  Timebase::new(
    tb.numerator(),
    core::num::NonZeroI32::new(tb.denominator().max(1)).expect("a non-zero denominator"),
  )
}

// ---------------------------------------------------------------------------
//  The shapes no container will hand over.
// ---------------------------------------------------------------------------

/// A custom channel map is the one channel-layout arm that owns heap,
/// the one whose union arm is a pointer, and the one no `ffmpeg` CLI
/// invocation will put in a file. It is built here, with per-channel
/// names, because it is precisely the arm a mirror gets wrong.
#[test]
fn a_custom_channel_map_survives_the_round_trip() {
  support::init_ffmpeg();
  let mut parameters = Parameters::new();
  // SAFETY: `parameters` owns a live `AVCodecParameters`; the map is
  // allocated from FFmpeg's allocator and handed to it, so
  // `avcodec_parameters_free` releases it with the struct.
  unsafe {
    let par = parameters.as_mut_ptr();
    let count = 3usize;
    let map = av_mallocz(count * core::mem::size_of::<AVChannelCustom>()).cast::<AVChannelCustom>();
    assert!(!map.is_null(), "av_mallocz the channel map");
    for (index, (id, name)) in [(0i32, b"FL\0"), (1, b"FR\0"), (2, b"FC\0")]
      .into_iter()
      .enumerate()
    {
      let entry = map.add(index);
      core::ptr::write_unaligned(core::ptr::addr_of_mut!((*entry).id).cast::<i32>(), id);
      let mut bytes = [0u8; 16];
      bytes[..name.len()].copy_from_slice(name);
      core::ptr::write_unaligned(
        core::ptr::addr_of_mut!((*entry).name).cast::<[u8; 16]>(),
        bytes,
      );
    }
    core::ptr::write_unaligned(
      core::ptr::addr_of_mut!((*par).ch_layout.order).cast::<i32>(),
      AVChannelOrder::AV_CHANNEL_ORDER_CUSTOM as i32,
    );
    (*par).ch_layout.nb_channels = count as i32;
    (*par).ch_layout.u.map = map;
  }

  let ticket = assert_round_trip(&parameters, "a custom channel map");
  let layout = ticket.ch_layout();
  assert_eq!(
    layout.order(),
    AVChannelOrder::AV_CHANNEL_ORDER_CUSTOM as i32
  );
  assert_eq!(layout.channels(), 3);
  assert_eq!(layout.map().len(), 3);
  assert_eq!(layout.map()[2].id(), 2);
  assert_eq!(&layout.map()[2].name_bytes()[..3], b"FC\0");
  // The `mask` arm is a pointer under this order, so the mirror must
  // report nothing there rather than an address.
  assert_eq!(layout.mask(), 0, "a pointer leaked into the mask field");
}

/// **A custom order with no map is refused, not reproduced.**
///
/// `av_channel_layout_copy` — the call `avcodec_parameters_to_context`
/// moves this field through — allocates `nb_channels` entries and then
/// `memcpy`s from `src->u.map` with no null check of its own. So a
/// layout naming channels it has no map for is not a curiosity that
/// round-trips harmlessly; it is a `memcpy` from null waiting for the
/// next decoder to open.
///
/// A faithful mirror of that shape would pass the parity comparator
/// and hand the crash on — which is the finding: fidelity is the wrong
/// test for a malformed input, so the mirror fails closed instead.
#[test]
fn a_custom_layout_without_a_map_is_refused() {
  support::init_ffmpeg();

  // Three channels declared, and no map to describe them.
  let mut parameters = Parameters::new();
  // SAFETY: `parameters` owns a live `AVCodecParameters`; both writes
  // are scalar stores, and the union's `map` arm keeps the null
  // `avcodec_parameters_alloc` left there.
  unsafe {
    let par = parameters.as_mut_ptr();
    core::ptr::write_unaligned(
      core::ptr::addr_of_mut!((*par).ch_layout.order).cast::<i32>(),
      AVChannelOrder::AV_CHANNEL_ORDER_CUSTOM as i32,
    );
    (*par).ch_layout.nb_channels = 3;
  }
  match CodecTicket::mirror(&parameters, 4, usize::MAX) {
    Err(ParametersError::ChannelMap(ref p)) => {
      assert_eq!(p.stream_index(), 4);
      assert_eq!(p.channels(), 3);
    }
    Err(other) => panic!("expected ParametersChannelMap, got {other:?}"),
    Ok(_) => panic!("a custom layout with a null map was mirrored"),
  }

  // A custom order naming no channels at all is the same refusal: the
  // map that order requires is still absent, and `av_malloc_array(0)`
  // hands back null, so libavcodec would answer `ENOMEM` for a file
  // that is simply malformed.
  let mut empty = Parameters::new();
  // SAFETY: as above.
  unsafe {
    core::ptr::write_unaligned(
      core::ptr::addr_of_mut!((*empty.as_mut_ptr()).ch_layout.order).cast::<i32>(),
      AVChannelOrder::AV_CHANNEL_ORDER_CUSTOM as i32,
    );
  }
  assert!(matches!(
    CodecTicket::mirror(&empty, 0, usize::MAX),
    Err(ParametersError::ChannelMap(_)),
  ));

  // A *negative* count is the same refusal, and it did not used to be.
  //
  // `mirror` measured the footprint before it read a single field, and
  // for a custom order that measurement is
  // `nb_channels * size_of::<AVChannelCustom>()` — a count that will
  // not convert to a `usize` is a footprint that cannot be computed, so
  // `measure_parameters` failed closed and the answer was
  // `ParametersTooLarge`. Two true statements about one file, and the
  // less useful one won because it was asked first.
  //
  // The structural validator runs before the measurement now, so the
  // answer is the one that describes the file: a custom layout
  // declaring minus one channels is a malformed map, not an oversized
  // one. The reordering is what puts every structural refusal ahead of
  // the copies, and this lane is where its one visible consequence is
  // recorded.
  let mut negative = Parameters::new();
  // SAFETY: as above; a negative channel count is a value a hostile
  // file can declare.
  unsafe {
    let par = negative.as_mut_ptr();
    core::ptr::write_unaligned(
      core::ptr::addr_of_mut!((*par).ch_layout.order).cast::<i32>(),
      AVChannelOrder::AV_CHANNEL_ORDER_CUSTOM as i32,
    );
    (*par).ch_layout.nb_channels = -1;
  }
  match CodecTicket::mirror(&negative, 7, usize::MAX) {
    Err(ParametersError::ChannelMap(ref p)) => {
      assert_eq!(p.stream_index(), 7);
      assert_eq!(p.channels(), -1, "the count is reported as declared");
    }
    Err(other) => panic!("expected ParametersChannelMap, got {other:?}"),
    Ok(_) => panic!("a custom layout declaring -1 channels was mirrored"),
  }
}

/// A rebuilt layout can never declare more channels than the array it
/// points at: the count is written **from** the map, so the two cannot
/// drift even if a future constructor broke the mirror's invariant.
#[test]
fn a_rebuilt_custom_layout_declares_exactly_its_map() {
  support::init_ffmpeg();
  let mut parameters = Parameters::new();
  // SAFETY: `parameters` owns a live `AVCodecParameters`; the map comes
  // from FFmpeg's allocator and is handed to it, so
  // `av_channel_layout_uninit` frees it with the struct.
  unsafe {
    let par = parameters.as_mut_ptr();
    let count = 5usize;
    let map = av_mallocz(count * core::mem::size_of::<AVChannelCustom>()).cast::<AVChannelCustom>();
    assert!(!map.is_null(), "av_mallocz the channel map");
    for index in 0..count {
      core::ptr::write_unaligned(
        core::ptr::addr_of_mut!((*map.add(index)).id).cast::<i32>(),
        index as i32,
      );
    }
    core::ptr::write_unaligned(
      core::ptr::addr_of_mut!((*par).ch_layout.order).cast::<i32>(),
      AVChannelOrder::AV_CHANNEL_ORDER_CUSTOM as i32,
    );
    (*par).ch_layout.nb_channels = count as i32;
    (*par).ch_layout.u.map = map;
  }

  let ticket = assert_round_trip(&parameters, "a five-channel custom layout");
  assert_eq!(ticket.ch_layout().map().len(), 5);
  let rebuilt = ticket.rebuild().expect("rebuild");
  // SAFETY: `rebuilt` owns a live `AVCodecParameters`.
  unsafe {
    let par = rebuilt.as_ptr();
    assert_eq!((*par).ch_layout.nb_channels, 5);
    assert!(!(*par).ch_layout.u.map.is_null());
  }
}

/// A side-data kind these bindings cannot name is still a kind the
/// file carries and a decoder may want. The mirror carries the raw
/// bits, so a number far outside `AVPacketSideDataType` round-trips.
#[test]
fn a_side_data_kind_the_bindings_cannot_name_survives() {
  support::init_ffmpeg();
  const UNNAMED: i32 = 0x7f00_0001;
  let mut parameters = Parameters::new();
  // SAFETY: `parameters` owns a live `AVCodecParameters`; both
  // buffers come from FFmpeg's allocator.
  unsafe {
    let par = parameters.as_mut_ptr();
    let array = av_mallocz(2 * core::mem::size_of::<AVPacketSideData>()).cast::<AVPacketSideData>();
    assert!(!array.is_null(), "av_mallocz the descriptor array");
    for (index, (kind, len, fill)) in [(UNNAMED, 37usize, 0x5Au8), (0x1234_5678, 0, 0)]
      .into_iter()
      .enumerate()
    {
      let entry = array.add(index);
      core::ptr::write_unaligned(core::ptr::addr_of_mut!((*entry).type_).cast::<i32>(), kind);
      if len > 0 {
        let payload = av_mallocz(len).cast::<u8>();
        assert!(!payload.is_null(), "av_mallocz the payload");
        core::ptr::write_bytes(payload, fill, len);
        core::ptr::write_unaligned(core::ptr::addr_of_mut!((*entry).data), payload);
        core::ptr::write_unaligned(core::ptr::addr_of_mut!((*entry).size), len);
      }
    }
    (*par).coded_side_data = array;
    (*par).nb_coded_side_data = 2;
  }

  let ticket = assert_round_trip(&parameters, "an unnamed side-data kind");
  assert_eq!(ticket.coded_side_data().len(), 2);
  assert_eq!(ticket.coded_side_data()[0].kind(), UNNAMED);
  assert_eq!(ticket.coded_side_data()[0].data().len(), 37);
  assert!(
    ticket.coded_side_data()[0]
      .data()
      .iter()
      .all(|&b| b == 0x5A)
  );
  // A zero-length entry is a real shape and keeps its type id.
  assert_eq!(ticket.coded_side_data()[1].kind(), 0x1234_5678);
  assert!(ticket.coded_side_data()[1].data().is_empty());
}

/// `AVChannelLayout::opaque` is a raw pointer FFmpeg documents as the
/// user's private data. An owned mirror outlives the pointer's owner
/// and may cross threads, so it cannot carry one — and it refuses
/// rather than dropping it in silence.
#[test]
fn a_layout_carrying_user_private_data_is_refused() {
  support::init_ffmpeg();
  let mut parameters = Parameters::new();
  let mut anything = 0u8;
  // SAFETY: `parameters` owns a live `AVCodecParameters`. `opaque` is
  // a plain pointer field FFmpeg never dereferences; it is restored to
  // null below so `avcodec_parameters_free` never sees the borrow.
  unsafe {
    let par = parameters.as_mut_ptr();
    // A mask means something only under an order that defines it —
    // see the scalar-field fixture's note.
    core::ptr::write_unaligned(
      core::ptr::addr_of_mut!((*par).ch_layout.order).cast::<i32>(),
      AVChannelOrder::AV_CHANNEL_ORDER_NATIVE as i32,
    );
    (*par).ch_layout.nb_channels = 2;
    (*par).ch_layout.u.mask = 3;
    (*par).ch_layout.opaque = core::ptr::addr_of_mut!(anything).cast();
  }

  let refused = CodecTicket::mirror(&parameters, 5, usize::MAX);
  match refused {
    Err(ParametersError::Opaque(ref p)) => {
      assert_eq!(p.stream_index(), 5);
      assert_eq!(p.channel(), None, "the pointer was on the layout itself");
    }
    Err(other) => panic!("expected ParametersOpaque, got {other:?}"),
    Ok(_) => panic!("a layout carrying a user pointer was mirrored"),
  }

  // SAFETY: the same live struct; the borrow is released before it is
  // dropped, so nothing outlives `anything`.
  unsafe {
    (*parameters.as_mut_ptr()).ch_layout.opaque = core::ptr::null_mut();
  }
  // And with the pointer gone, the same layout mirrors cleanly — the
  // refusal is about the field, not about the layout.
  assert_round_trip(&parameters, "a mask layout");
}

/// The same refusal, one level down: a custom map's per-channel
/// `opaque`, which names the entry it was found on.
#[test]
fn a_custom_channel_carrying_user_private_data_is_refused() {
  support::init_ffmpeg();
  let mut parameters = Parameters::new();
  let mut anything = 0u8;
  // SAFETY: `parameters` owns a live `AVCodecParameters`; the map is
  // FFmpeg-allocated and the borrowed pointer is cleared below.
  unsafe {
    let par = parameters.as_mut_ptr();
    let map = av_mallocz(2 * core::mem::size_of::<AVChannelCustom>()).cast::<AVChannelCustom>();
    assert!(!map.is_null(), "av_mallocz the channel map");
    core::ptr::write_unaligned(
      core::ptr::addr_of_mut!((*map.add(1)).opaque),
      core::ptr::addr_of_mut!(anything).cast(),
    );
    core::ptr::write_unaligned(
      core::ptr::addr_of_mut!((*par).ch_layout.order).cast::<i32>(),
      AVChannelOrder::AV_CHANNEL_ORDER_CUSTOM as i32,
    );
    (*par).ch_layout.nb_channels = 2;
    (*par).ch_layout.u.map = map;
  }

  match CodecTicket::mirror(&parameters, 2, usize::MAX) {
    Err(ParametersError::Opaque(ref p)) => {
      assert_eq!(p.stream_index(), 2);
      assert_eq!(p.channel(), Some(1), "the entry the pointer was on");
    }
    Err(other) => panic!("expected ParametersOpaque, got {other:?}"),
    Ok(_) => panic!("a custom channel carrying a user pointer was mirrored"),
  }

  // SAFETY: the same live map; the borrow is released before the
  // struct is dropped.
  unsafe {
    core::ptr::write_unaligned(
      core::ptr::addr_of_mut!((*(*parameters.as_mut_ptr()).ch_layout.u.map.add(1)).opaque),
      core::ptr::null_mut(),
    );
  }
}

// ---------------------------------------------------------------------------
//  A container that really does carry stream-level side data, minted here.
// ---------------------------------------------------------------------------

/// One ISO-BMFF box: `[size:u32be][type:4cc][body]`, the size counting
/// its own header.
///
/// Layout per the ISO base media file format, cross-read against
/// `exifast`'s QuickTime port — the house's own box oracle, whose
/// walker documents the same header and whose `colr` decoder reads the
/// same payload shape: a 4-cc colour type at offset 0, and for `prof`
/// the ICC bytes straight after it.
fn iso_box(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
  let mut out = Vec::with_capacity(8 + body.len());
  out.extend_from_slice(
    &u32::try_from(8 + body.len())
      .expect("a small box")
      .to_be_bytes(),
  );
  out.extend_from_slice(kind);
  out.extend_from_slice(body);
  out
}

/// The identity transformation matrix every `mvhd` and `tkhd` carries.
fn matrix_bytes() -> Vec<u8> {
  [0x0001_0000u32, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000]
    .iter()
    .flat_map(|w| w.to_be_bytes())
    .collect()
}

/// A complete, minimal MP4 holding one 16x16 `raw ` video track whose
/// sample entry carries a `colr` box of colour type `prof` — the atom
/// libavformat's MOV demuxer turns into an `AV_PKT_DATA_ICC_PROFILE`
/// entry of `codecpar->coded_side_data`.
///
/// # Why this is minted rather than transcoded
///
/// `coded_side_data` is the field this crate's parameter budgets were
/// *written for*: a MOV `prof` atom is where an attacker-sized ICC
/// profile arrives, and finding it there is what killed the wholesale
/// `avcodec_parameters_copy`. It is also the one field no `ffmpeg` CLI
/// invocation will produce — censused directly: `-metadata:s:v rotate=`
/// no longer writes a display matrix, and MOV, MP4 and Matroska emit no
/// stream-level side data of their own.
///
/// So the container is assembled here, byte by byte. Nothing binary is
/// committed — the same law the generated corpus keeps, held one step
/// further: this fixture needs no `ffmpeg` CLI either, so the lane that
/// proves the field runs everywhere.
///
/// The track carries no samples, and nothing here decodes. What is
/// being proved is that a *demuxer* populated the field and that the
/// field survived the round trip, which the header alone settles.
fn mint_mp4_with_icc_profile(icc: &[u8]) -> Vec<u8> {
  let be32 = |v: u32| v.to_be_bytes().to_vec();
  let be16 = |v: u16| v.to_be_bytes().to_vec();

  // `colr`, colour type `prof`: the ICC profile, verbatim.
  let colr = iso_box(b"colr", &[b"prof".as_slice(), icc].concat());

  // A 78-byte VisualSampleEntry body, then its child boxes — the
  // 86-byte fixed prefix (8 header + 78 body) `exifast` documents as
  // where an entry's child-atom region begins.
  let mut visual = Vec::new();
  visual.extend_from_slice(&[0u8; 6]); // SampleEntry.reserved
  visual.extend_from_slice(&be16(1)); // data_reference_index
  visual.extend_from_slice(&[0u8; 16]); // pre_defined, reserved, pre_defined[3]
  visual.extend_from_slice(&be16(16)); // width
  visual.extend_from_slice(&be16(16)); // height
  visual.extend_from_slice(&be32(0x0048_0000)); // horizresolution, 72 dpi
  visual.extend_from_slice(&be32(0x0048_0000)); // vertresolution
  visual.extend_from_slice(&be32(0)); // reserved
  visual.extend_from_slice(&be16(1)); // frame_count
  visual.push(4); // compressorname, Pascal-style
  visual.extend_from_slice(b"mint");
  visual.extend_from_slice(&[0u8; 27]);
  visual.extend_from_slice(&be16(24)); // depth
  visual.extend_from_slice(&[0xFF, 0xFF]); // pre_defined, -1
  assert_eq!(visual.len(), 78, "the VisualSampleEntry body is fixed-size");
  visual.extend_from_slice(&colr);
  let entry = iso_box(b"raw ", &visual);

  let stbl = iso_box(
    b"stbl",
    &[
      iso_box(b"stsd", &[vec![0; 4], be32(1), entry].concat()),
      iso_box(b"stts", &[vec![0; 4], be32(0)].concat()),
      iso_box(b"stsc", &[vec![0; 4], be32(0)].concat()),
      iso_box(b"stsz", &[vec![0; 4], be32(0), be32(0)].concat()),
      iso_box(b"stco", &[vec![0; 4], be32(0)].concat()),
    ]
    .concat(),
  );
  // One self-contained data reference: the samples, had there been any,
  // would live in this same file.
  let dinf = iso_box(
    b"dinf",
    &iso_box(
      b"dref",
      &[vec![0; 4], be32(1), iso_box(b"url ", &[0, 0, 0, 1])].concat(),
    ),
  );
  let minf = iso_box(
    b"minf",
    &[
      iso_box(b"vmhd", &[vec![0, 0, 0, 1], vec![0; 8]].concat()),
      dinf,
      stbl,
    ]
    .concat(),
  );
  let mdhd = iso_box(
    b"mdhd",
    &[
      vec![0; 4],
      be32(0),
      be32(0),
      be32(600),
      be32(0),
      be16(0x55c4), // language, "und"
      be16(0),
    ]
    .concat(),
  );
  let hdlr = iso_box(
    b"hdlr",
    &[
      vec![0; 8],
      b"vide".to_vec(),
      vec![0; 12],
      b"mint\0".to_vec(),
    ]
    .concat(),
  );
  let mdia = iso_box(b"mdia", &[mdhd, hdlr, minf].concat());
  let tkhd = iso_box(
    b"tkhd",
    &[
      vec![0, 0, 0, 3], // enabled | in movie
      be32(0),
      be32(0),
      be32(1), // track_ID
      be32(0),
      be32(0), // duration
      vec![0; 8],
      be16(0), // layer
      be16(0), // alternate_group
      be16(0), // volume
      be16(0), // reserved
      matrix_bytes(),
      be32(16 << 16), // width, 16.16 fixed point
      be32(16 << 16), // height
    ]
    .concat(),
  );
  let trak = iso_box(b"trak", &[tkhd, mdia].concat());
  let mvhd = iso_box(
    b"mvhd",
    &[
      vec![0; 4],
      be32(0),
      be32(0),
      be32(600),         // timescale
      be32(0),           // duration
      be32(0x0001_0000), // rate
      be16(0x0100),      // volume
      vec![0; 2],
      vec![0; 8],
      matrix_bytes(),
      vec![0; 24],
      be32(2), // next_track_ID
    ]
    .concat(),
  );
  let moov = iso_box(b"moov", &[mvhd, trak].concat());
  let ftyp = iso_box(
    b"ftyp",
    &[b"isom".as_slice(), &be32(512), b"isomiso2mp41"].concat(),
  );
  [ftyp, moov].concat()
}

/// Profile-shaped bytes: a plausible ICC header — the declared size at
/// offset 0 and the `acsp` signature at offset 36 — over a
/// deterministic body, so the payload is recognisable in a failure
/// message and is not mistaken for a real profile either.
fn icc_profile_bytes(len: usize) -> Vec<u8> {
  assert!(len >= 128, "an ICC header is 128 bytes");
  let mut icc: Vec<u8> = (0..len).map(|i| ((i * 7 + 3) & 0xFF) as u8).collect();
  icc[..4].copy_from_slice(&u32::try_from(len).expect("a small profile").to_be_bytes());
  icc[36..40].copy_from_slice(b"acsp");
  icc
}

/// **The `coded_side_data` field, proved through a real demuxer.**
///
/// Every other assertion about that field in this file is made over
/// parameters built by hand, which shows the mirror carries what it is
/// handed and says nothing about whether a container ever hands it
/// anything. This lane closes the other half: libavformat parses the
/// minted `colr`/`prof` atom, populates `codecpar->coded_side_data`
/// itself, and the row is then followed through the ticket and back
/// out.
#[test]
fn a_minted_container_carries_its_icc_profile_through_the_ticket() {
  support::init_ffmpeg();
  let icc = icc_profile_bytes(3_072);
  let dir = tempfile::tempdir().expect("temp dir");
  let path = dir.path().join("icc.mp4");
  std::fs::write(&path, mint_mp4_with_icc_profile(&icc)).expect("write the minted container");

  // First, that libavformat really did populate the field. If this
  // fails the fixture is wrong rather than the mirror, so it is
  // asserted separately and says so.
  let input = ffmpeg_next::format::input(&path).expect("libavformat opens the minted container");
  let stream = input.streams().next().expect("the minted track");
  let parameters = stream.parameters();
  // SAFETY: `parameters` owns a live `AVCodecParameters` for this read.
  let populated = unsafe { (*parameters.as_ptr()).nb_coded_side_data };
  assert_eq!(
    populated, 1,
    "the MOV demuxer did not route the minted `colr`/`prof` atom into `coded_side_data` — \
     the fixture is wrong, or FFmpeg changed that road",
  );

  // Then the road this lane exists for: through the demuxer's own row.
  let demuxer = FfmpegOwnedDemuxer::open(&path).expect("open the minted container");
  let track = demuxer.tracks().first().expect("one track");
  let ticket = track.extra().ticket();

  let entries = ticket.coded_side_data();
  assert_eq!(entries.len(), 1, "the ticket lost the profile");
  assert_eq!(
    entries[0].kind(),
    ffmpeg_next::ffi::AVPacketSideDataType::AV_PKT_DATA_ICC_PROFILE as i32,
    "the type id did not survive",
  );
  assert_eq!(
    entries[0].data(),
    icc.as_slice(),
    "the profile bytes differ"
  );
  // The field is charged, so a forged atom meets the same ceiling a real
  // profile does.
  assert!(
    ticket.footprint_bytes() >= icc.len(),
    "a {}-byte profile was charged {} bytes",
    icc.len(),
    ticket.footprint_bytes(),
  );

  // And back out, field by field, against libavformat's own original.
  let rebuilt = track.extra().clone_parameters().expect("rebuild");
  // SAFETY: both own a live `AVCodecParameters`.
  let bad = unsafe { compare(parameters.as_ptr(), rebuilt.as_ptr()) };
  assert!(
    bad.is_empty(),
    "the minted container's row is not the file:\n  {}",
    bad.join("\n  "),
  );
}

/// The ceiling applies to a *container's* side data, not only to
/// parameters a test built — the R3 finding's own shape, end to end.
#[test]
fn an_oversized_minted_profile_is_refused_at_open() {
  support::init_ffmpeg();
  let icc = icc_profile_bytes(512 * 1024);
  let dir = tempfile::tempdir().expect("temp dir");
  let path = dir.path().join("fat-icc.mp4");
  std::fs::write(&path, mint_mp4_with_icc_profile(&icc)).expect("write the minted container");

  let limits = DemuxLimits::default().with_max_codec_parameter_bytes(64 * 1024);
  match FfmpegOwnedDemuxer::open_with(&path, limits) {
    Err(DemuxError::ParametersTooLarge(ref p)) => {
      assert_eq!(p.stream_index(), 0);
      assert!(p.bytes() >= icc.len(), "charged {} bytes", p.bytes());
      assert_eq!(p.limit(), 64 * 1024);
    }
    Err(other) => panic!("expected ParametersTooLarge, got {other:?}"),
    Ok(_) => panic!("a 512 KiB ICC profile passed a 64 KiB ceiling"),
  }

  // And the default ceiling admits it: a real camera profile is this
  // size, so a budget that refused it would be one nobody could ship
  // behind.
  let demuxer = FfmpegOwnedDemuxer::open(&path).expect("the default ceiling admits a real profile");
  assert_eq!(
    demuxer.tracks()[0].extra().ticket().coded_side_data()[0].data(),
    icc.as_slice(),
  );
}

/// Every scalar field, set to a value that is not its default, so a
/// field the rebuild forgot to write cannot hide behind
/// `avcodec_parameters_alloc`'s own initialisation.
///
/// This is the assertion that would have caught `alpha_mode` — a field
/// new in FFmpeg n9.0, absent from the mirror's first draft, and
/// invisible to the corpus because every fixture leaves it at zero.
#[test]
fn every_scalar_field_is_written_back() {
  support::init_ffmpeg();
  let mut parameters = Parameters::new();
  // SAFETY: `parameters` owns a live `AVCodecParameters`; every write
  // below is a scalar store into a struct nothing else references.
  // Enum fields are written as the raw patterns they are on the wire,
  // and the values are chosen inside each enum's real range so the
  // struct stays one libavcodec would accept.
  unsafe {
    let par = parameters.as_mut_ptr();
    core::ptr::write_unaligned(core::ptr::addr_of_mut!((*par).codec_type).cast::<i32>(), 0);
    core::ptr::write_unaligned(core::ptr::addr_of_mut!((*par).codec_id).cast::<i32>(), 27);
    (*par).codec_tag = 0x3462_6461;
    (*par).format = 12;
    (*par).bit_rate = 5_000_000;
    (*par).bits_per_coded_sample = 8;
    (*par).bits_per_raw_sample = 10;
    (*par).profile = 100;
    (*par).level = 41;
    (*par).width = 1920;
    (*par).height = 1080;
    (*par).sample_aspect_ratio.num = 4;
    (*par).sample_aspect_ratio.den = 3;
    (*par).framerate.num = 24_000;
    (*par).framerate.den = 1_001;
    core::ptr::write_unaligned(core::ptr::addr_of_mut!((*par).field_order).cast::<i32>(), 2);
    core::ptr::write_unaligned(core::ptr::addr_of_mut!((*par).color_range).cast::<i32>(), 2);
    core::ptr::write_unaligned(
      core::ptr::addr_of_mut!((*par).color_primaries).cast::<i32>(),
      9,
    );
    core::ptr::write_unaligned(core::ptr::addr_of_mut!((*par).color_trc).cast::<i32>(), 16);
    core::ptr::write_unaligned(core::ptr::addr_of_mut!((*par).color_space).cast::<i32>(), 9);
    core::ptr::write_unaligned(
      core::ptr::addr_of_mut!((*par).chroma_location).cast::<i32>(),
      2,
    );
    (*par).video_delay = 3;
    core::ptr::write_unaligned(core::ptr::addr_of_mut!((*par).alpha_mode).cast::<i32>(), 1);
    // The order is what makes the mask mean anything: FFmpeg defines
    // `u.mask` for `NATIVE` and `AMBISONIC` only, and this fixture used
    // to leave the order at the `UNSPEC` zero the allocator wrote while
    // setting a mask anyway — a shape libavformat never produces.
    core::ptr::write_unaligned(
      core::ptr::addr_of_mut!((*par).ch_layout.order).cast::<i32>(),
      AVChannelOrder::AV_CHANNEL_ORDER_NATIVE as i32,
    );
    (*par).ch_layout.nb_channels = 6;
    (*par).ch_layout.u.mask = 0x3f;
    (*par).sample_rate = 48_000;
    (*par).block_align = 4;
    (*par).frame_size = 1_024;
    (*par).initial_padding = 1_024;
    (*par).trailing_padding = 512;
    (*par).seek_preroll = 30;
  }

  let ticket = assert_round_trip(&parameters, "every scalar field");

  // And the ticket reports what it mirrored, not a default: the
  // accessors are the consumer's road and a wrong one is as bad as a
  // dropped field.
  assert_eq!(ticket.codec_id(), 27);
  assert_eq!(ticket.codec_tag(), 0x3462_6461);
  assert_eq!(ticket.format(), 12);
  assert_eq!(ticket.bit_rate(), 5_000_000);
  assert_eq!(ticket.bits_per_coded_sample(), 8);
  assert_eq!(ticket.bits_per_raw_sample(), 10);
  assert_eq!(ticket.profile(), 100);
  assert_eq!(ticket.level(), 41);
  assert_eq!((ticket.width(), ticket.height()), (1920, 1080));
  assert_eq!(
    (
      ticket.sample_aspect_ratio().num(),
      ticket.sample_aspect_ratio().den()
    ),
    (4, 3),
  );
  assert_eq!(
    (ticket.framerate().num(), ticket.framerate().den()),
    (24_000, 1_001)
  );
  assert_eq!(ticket.field_order(), 2);
  assert_eq!(ticket.color_range(), 2);
  assert_eq!(ticket.color_primaries(), 9);
  assert_eq!(ticket.color_trc(), 16);
  assert_eq!(ticket.color_space(), 9);
  assert_eq!(ticket.chroma_location(), 2);
  assert_eq!(ticket.video_delay(), 3);
  assert_eq!(ticket.alpha_mode(), 1);
  assert_eq!(ticket.ch_layout().channels(), 6);
  assert_eq!(ticket.ch_layout().mask(), 0x3f);
  assert_eq!(ticket.sample_rate(), 48_000);
  assert_eq!(ticket.block_align(), 4);
  assert_eq!(ticket.frame_size(), 1_024);
  assert_eq!(ticket.initial_padding(), 1_024);
  assert_eq!(ticket.trailing_padding(), 512);
  assert_eq!(ticket.seek_preroll(), 30);
  // `AVMEDIA_TYPE_VIDEO` is zero, so this field is the one place a
  // "wrote nothing" bug and a correct mirror agree — which is why the
  // round trip above, not this line, is the assertion that matters.
  assert_eq!(ticket.codec_type(), 0);
}
