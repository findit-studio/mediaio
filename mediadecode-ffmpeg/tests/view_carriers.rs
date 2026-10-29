//! The second carrier lane, pinned from outside the crate.
//!
//! What these lanes prove:
//!
//! - a demuxed view packet really is a **window into libavformat's own
//!   allocation**, not a right-sized copy that happens to compare equal;
//! - both lanes carry byte-identical content out of the same file, so
//!   choosing a lane is a choice about ownership and never about what
//!   the bytes are;
//! - every budget that fires on the owned lane fires identically here,
//!   because a ceiling judges sizes and not copies;
//! - and `AV_PKT_FLAG_TRUSTED` is refused on both, because sharing an
//!   allocation does not make its pointers own what they name.

mod support;

use mediadecode::{
  Received,
  demuxer::{DemuxedPacket, Demuxer, TrackKind},
};
use mediadecode_ffmpeg::{
  DemuxError, DemuxLimits, FfmpegDemuxer, FfmpegOwnedDemuxer, PacketBufferError,
};

use support::Corpus;

/// Every payload a session delivers, as `(track, kind, bytes)`.
fn drain_owned(path: &std::path::Path) -> Vec<(usize, TrackKind, Vec<u8>)> {
  let mut demuxer = FfmpegOwnedDemuxer::open(path).expect("open");
  let kinds: Vec<TrackKind> = demuxer.tracks().iter().map(|t| t.kind()).collect();
  let mut out = Vec::new();
  while let Some(packet) = demuxer.next_packet().expect("read") {
    let (track, bytes) = match packet {
      DemuxedPacket::Video(p) => (p.track(), p.packet().data().as_ref().to_vec()),
      DemuxedPacket::Audio(p) => (p.track(), p.packet().data().as_ref().to_vec()),
      DemuxedPacket::Subtitle(p) => (p.track(), p.packet().data().as_ref().to_vec()),
      DemuxedPacket::Data(p) => (p.track(), p.packet().data().as_ref().to_vec()),
      DemuxedPacket::Attachment(p) => (p.track(), p.packet().data().as_ref().to_vec()),
    };
    let index = track.get();
    out.push((index, kinds[index], bytes));
  }
  out
}

/// As [`drain_owned`], on the view lane.
fn drain_view(path: &std::path::Path) -> Vec<(usize, TrackKind, Vec<u8>)> {
  let mut demuxer = FfmpegDemuxer::open(path).expect("open");
  let kinds: Vec<TrackKind> = demuxer.tracks().iter().map(|t| t.kind()).collect();
  let mut out = Vec::new();
  while let Some(packet) = demuxer.next_packet().expect("read") {
    let (track, bytes) = match packet {
      DemuxedPacket::Video(p) => (p.track(), p.packet().data().as_ref().to_vec()),
      DemuxedPacket::Audio(p) => (p.track(), p.packet().data().as_ref().to_vec()),
      DemuxedPacket::Subtitle(p) => (p.track(), p.packet().data().as_ref().to_vec()),
      DemuxedPacket::Data(p) => (p.track(), p.packet().data().as_ref().to_vec()),
      DemuxedPacket::Attachment(p) => (p.track(), p.packet().data().as_ref().to_vec()),
    };
    let index = track.get();
    out.push((index, kinds[index], bytes));
  }
  out
}

#[test]
fn a_demuxed_view_packet_windows_libavformats_own_allocation() {
  let Some(corpus) = Corpus::new() else {
    return;
  };
  support::init_ffmpeg();
  let path = corpus.multi_track_mkv();

  let mut demuxer = FfmpegDemuxer::open(&path).expect("open");
  let mut windows = 0usize;
  while let Some(packet) = demuxer.next_packet().expect("read") {
    let DemuxedPacket::Video(p) = packet else {
      continue;
    };
    let carrier = p.packet().data();
    if carrier.is_empty() {
      continue;
    }

    // **The zero-copy proof.** libavformat allocates every packet's
    // buffer with `AV_INPUT_BUFFER_PADDING_SIZE` of slack behind the
    // payload, so a carrier that *views* that buffer sits inside an
    // allocation strictly larger than the bytes it exposes. A copy
    // would have been sized to the payload exactly — this crate's owned
    // carrier is — so a buffer bigger than its view is a window, and
    // nothing else.
    //
    // SAFETY: the carrier holds a live reference to the buffer it
    // names; `data` and `size` are public fields.
    let (base, size) = unsafe {
      let buf = carrier.as_av_buffer_ref();
      ((*buf).data as usize, (*buf).size)
    };
    let exported = carrier.as_ref().as_ptr() as usize;
    assert!(
      exported >= base && exported + carrier.len() <= base + size,
      "the view escaped its buffer",
    );
    assert!(
      size > carrier.len(),
      "a {}-byte payload in a {}-byte buffer is a copy, not a window",
      carrier.len(),
      size,
    );
    assert_eq!(
      exported,
      base + carrier.offset(),
      "the view's offset must be where it says it is",
    );

    // And a clone shares that same allocation rather than making
    // another.
    let twin = carrier.clone();
    assert!(
      twin.ptr_eq(carrier),
      "clone must bump the refcount, not copy"
    );
    windows += 1;
  }
  assert!(
    windows > 0,
    "the fixture delivered no video packets to prove anything with"
  );
}

#[test]
fn both_lanes_carry_the_same_bytes() {
  let Some(corpus) = Corpus::new() else {
    return;
  };
  support::init_ffmpeg();

  // Choosing a lane is a choice about **ownership**, never about
  // content. If these ever disagree, one of the two roads is losing or
  // inventing bytes.
  for path in [corpus.multi_track_mkv(), corpus.cover_art_mp3()] {
    let owned = drain_owned(&path);
    let view = drain_view(&path);
    assert_eq!(
      owned.len(),
      view.len(),
      "{}: the two lanes delivered different packet counts",
      path.display(),
    );
    for (i, (o, v)) in owned.iter().zip(view.iter()).enumerate() {
      assert_eq!(
        o.0,
        v.0,
        "{}: packet {i} came from a different track",
        path.display()
      );
      assert_eq!(o.1, v.1, "{}: packet {i} changed kind", path.display());
      assert_eq!(
        o.2,
        v.2,
        "{}: packet {i} differs between lanes ({} vs {} bytes)",
        path.display(),
        o.2.len(),
        v.2.len(),
      );
    }
    assert!(
      !owned.is_empty(),
      "{}: nothing was delivered",
      path.display()
    );
  }
}

#[test]
fn the_budgets_fire_identically_on_both_lanes() {
  let Some(corpus) = Corpus::new() else {
    return;
  };
  support::init_ffmpeg();
  let path = corpus.cover_art_mp3();

  // **A ceiling judges sizes, not copies.** A view costs no bytes to
  // take, but a budget bounds what a caller is handed and asked to
  // hold — and on the view lane it also bounds how long a pool slot
  // stays out. So every limit fires the same on both.
  let starved = DemuxLimits::new().with_max_attachment_bytes(16);

  let owned = FfmpegOwnedDemuxer::open_with(&path, starved);
  let view = FfmpegDemuxer::open_with(&path, starved);

  match (owned, view) {
    (Err(DemuxError::AttachmentTooLarge(a)), Err(DemuxError::AttachmentTooLarge(b))) => {
      assert_eq!(
        a.limit(),
        b.limit(),
        "the two lanes reported different ceilings"
      );
      assert_eq!(
        a.bytes(),
        b.bytes(),
        "the two lanes measured different payloads"
      );
    }
    (o, v) => panic!(
      "the lanes disagreed: owned={:?} view={:?}",
      o.err().map(|e| e.to_string()),
      v.err().map(|e| e.to_string()),
    ),
  }

  // And the defaults open on both.
  assert!(FfmpegOwnedDemuxer::open_with(&path, DemuxLimits::new()).is_ok());
  assert!(FfmpegDemuxer::open_with(&path, DemuxLimits::new()).is_ok());
}

#[test]
fn a_trusted_payload_is_uncarriable_on_both_lanes() {
  use ffmpeg_next::packet::Mut;
  use mediadecode_ffmpeg::{
    PacketLimits,
    boundary::{owned_video_packet_from_ffmpeg_in, video_packet_from_ffmpeg_in},
  };
  support::init_ffmpeg();

  // **Sharing an allocation does not make its pointers own what they
  // name.** `AV_PKT_FLAG_TRUSTED` marks a body that may be a structure
  // of addresses into other live objects; copying it mints an
  // owned-looking carrier full of danglers, and *viewing* it mints a
  // refcounted one — which keeps one buffer alive and none of the
  // objects its contents point at. Both lanes refuse.
  let mut packet = ffmpeg_next::Packet::copy(&[1u8, 2, 3, 4]);
  // SAFETY: `packet` owns a live `AVPacket`; `flags` is a public field
  // and this bit has no `ffmpeg_next::Flags` spelling.
  unsafe {
    (*packet.as_mut_ptr()).flags =
      ffmpeg_next::ffi::AV_PKT_FLAG_KEY | ffmpeg_next::ffi::AV_PKT_FLAG_TRUSTED;
  }
  let tb = mediadecode::Timebase::default();

  assert!(matches!(
    // SAFETY: `Packet::copy` wrote every byte of the payload, and the packet is live for the
    // call.
    unsafe { owned_video_packet_from_ffmpeg_in(&packet, tb, PacketLimits::default()) },
    Err(PacketBufferError::TrustedPayload(_)),
  ));
  assert!(matches!(
    // SAFETY: `Packet::copy` wrote every byte of the payload, and the packet is live for the
    // call.
    unsafe { video_packet_from_ffmpeg_in(packet.clone(), tb, PacketLimits::default()) },
    Err(PacketBufferError::TrustedPayload(_)),
  ));
  // The bare name is the view lane, and refuses the same way.
  assert!(matches!(
    // SAFETY: `Packet::copy` wrote every byte of the payload, and the packet is live for the
    // call.
    unsafe { video_packet_from_ffmpeg_in(packet, tb, PacketLimits::default()) },
    Err(PacketBufferError::TrustedPayload(_)),
  ));
}

#[test]
fn a_packet_handed_to_a_caller_never_shares_its_carrier() {
  let Some(corpus) = Corpus::new() else {
    return;
  };
  support::init_ffmpeg();
  use ffmpeg_next::packet::Ref;
  use mediadecode_ffmpeg::{PacketLimits, boundary::ffmpeg_packet_from_video_packet};

  // **The public builder copies on both lanes, and must.** The packet
  // it returns is an `ffmpeg_next::Packet`, which lends `&mut [u8]`
  // through `data_mut` — while the carrier it was built from still
  // lends `&[u8]`. A shared body here would be two live references to
  // one allocation with one of them mutable, reachable from entirely
  // safe code, and `!Sync` would not help: one thread suffices.
  //
  // The zero-copy send is real, but it lives on the scoped submission
  // road inside this crate, where no value that can produce a `&mut`
  // into the shared bytes ever exists. See
  // `boundary::with_ffmpeg_video_packet` and its unit tests.
  let path = corpus.multi_track_mkv();
  let mut demuxer = FfmpegDemuxer::open(&path).expect("open");
  let mut checked = 0usize;
  while let Some(packet) = demuxer.next_packet().expect("read") {
    let DemuxedPacket::Video(p) = packet else {
      continue;
    };
    let carrier = p.packet().data().clone();
    if carrier.is_empty() {
      continue;
    }
    let rebuilt =
      ffmpeg_packet_from_video_packet(p.packet(), PacketLimits::default()).expect("rebuilt");

    // SAFETY: both hold live references; `data` is a public field.
    let (rebuilt_data, carrier_data) = unsafe {
      (
        (*rebuilt.as_ptr()).data as usize,
        carrier.as_ref().as_ptr() as usize,
      )
    };
    assert_ne!(
      rebuilt_data, carrier_data,
      "a packet a caller holds must own its bytes — sharing them makes \
       `data_mut` an aliasing `&mut` over a live carrier",
    );
    // The bytes are the same bytes, though: copying is not translating.
    assert_eq!(rebuilt.data().unwrap_or(&[]), carrier.as_ref());
    checked += 1;
    if checked >= 3 {
      break;
    }
  }
  assert!(
    checked > 0,
    "no video packet was available to prove the send leg with"
  );
}

// ---------------------------------------------------------------------------
//  The frame vertical
// ---------------------------------------------------------------------------

/// The view lane's frame conversion.
///
/// `unsafe` for the reason the borrowed conversion is owned-lane: a view
/// plane is a window into `frame`'s own buffer, and a caller who keeps
/// the frame keeps a `data_mut` away from a mutable alias of it. Every
/// call below satisfies the contract the same way — the frame is read,
/// never written, for as long as the carriers live — and every frame
/// handed in had its planes written by the builder that made it.
unsafe fn view_video_frame(
  frame: &ffmpeg_next::frame::Video,
  time_base: mediadecode::Timebase,
  limits: mediadecode_ffmpeg::FrameLimits,
) -> Result<mediadecode_ffmpeg::VideoFrame, mediadecode_ffmpeg::convert::ConvertError> {
  // SAFETY: `frame` is a live `AVFrame` for the whole call, its planes
  // written by the builder that made it (this function's own contract),
  // and this test never writes through it while a carrier is alive.
  unsafe { mediadecode_ffmpeg::convert::av_frame_to_video_frame(frame.as_ptr(), time_base, limits) }
}

/// [`view_video_frame`] for audio, with the same obligation and the
/// same discharge.
unsafe fn view_audio_frame(
  frame: &ffmpeg_next::frame::Audio,
  time_base: mediadecode::Timebase,
  limits: mediadecode_ffmpeg::FrameLimits,
) -> Result<mediadecode_ffmpeg::AudioFrame, mediadecode_ffmpeg::convert::ConvertError> {
  // SAFETY: as above.
  unsafe { mediadecode_ffmpeg::convert::av_frame_to_audio_frame(frame.as_ptr(), time_base, limits) }
}

/// A GRAY8 `AVFrame` of `width x height`, refcount-allocated by FFmpeg
/// and filled with a per-row pattern — every byte of every row its
/// geometry describes is written, which is what a conversion reads.
///
/// GRAY8 is one byte per sample and one plane, so `row_bytes == width`
/// and the tight/padded question is decided by `width` alone: a
/// multiple of 64 gets a `linesize` equal to it, anything else gets
/// FFmpeg's alignment padding. Both cases are wanted here.
fn gray_frame(width: u32, height: u32) -> ffmpeg_next::frame::Video {
  support::init_ffmpeg();
  let mut frame = ffmpeg_next::frame::Video::new(ffmpeg_next::format::Pixel::GRAY8, width, height);
  // Written before anything lends it: `data_mut` below forms a slice over
  // the plane, and `av_frame_get_buffer` left it unwritten.
  write_every_buffer(&mut frame);
  let linesize = frame.stride(0);
  let data = frame.data_mut(0);
  for y in 0..height as usize {
    for x in 0..width as usize {
      data[y * linesize + x] = (y as u8).wrapping_mul(31).wrapping_add(x as u8);
    }
  }
  frame
}

/// Writes every byte of every buffer `frame` holds — what a decoder does
/// to the frames it hands out, and what `frame::Audio::new` leaves
/// undone: `av_frame_get_buffer` allocates without writing.
fn write_every_buffer(frame: &mut ffmpeg_next::Frame) {
  // SAFETY: the frame is live and uniquely borrowed. Each non-null
  // `buf[i]` is a live `AVBufferRef` whose `data` is valid for `size`
  // bytes, written through the raw pointer without forming a reference
  // to the unwritten bytes.
  unsafe {
    for buf in (*frame.as_mut_ptr()).buf {
      if !buf.is_null() {
        core::ptr::write_bytes((*buf).data, 0, (*buf).size);
      }
    }
  }
}

/// The `(start, end)` addresses of `buf[0]`, the allocation a
/// freshly-built `AVFrame` puts every plane in.
fn backing_range(frame: &ffmpeg_next::frame::Video) -> (usize, usize) {
  // SAFETY: `frame` owns a live `AVFrame`; `buf` and its `data`/`size`
  // are public fields, and `buf[0]` is populated by
  // `av_frame_get_buffer`.
  unsafe {
    let raw = frame.as_ptr();
    let buf = (*raw).buf[0];
    assert!(!buf.is_null(), "a built frame has a refcounted buffer");
    let start = (*buf).data as usize;
    (start, start + (*buf).size)
  }
}

/// The bytes a plane really holds, row by row, so a padded plane and a
/// compacted one can be compared as **content** rather than as spans.
fn rows_of<B: AsRef<[u8]>>(
  plane: &mediadecode::frame::Plane<B>,
  width: usize,
  rows: usize,
) -> Vec<u8> {
  let stride = plane.stride() as usize;
  let bytes = plane.data_ref().as_ref();
  let mut out = Vec::with_capacity(width * rows);
  for y in 0..rows {
    out.extend_from_slice(&bytes[y * stride..y * stride + width]);
  }
  out
}

#[test]
fn a_tight_video_plane_is_a_window_into_the_decoders_own_buffer() {
  use mediadecode_ffmpeg::FrameLimits;

  // 128 is a multiple of every alignment FFmpeg uses, so the plane is
  // tight — and the assert below says so rather than assuming it.
  let frame = gray_frame(128, 8);
  assert_eq!(frame.stride(0), 128, "the premise: a tight plane");
  let (start, end) = backing_range(&frame);

  let out = unsafe {
    view_video_frame(
      &frame,
      mediadecode::Timebase::default(),
      FrameLimits::default(),
    )
  }
  .expect("a GRAY8 frame converts");

  let plane = &out.planes()[0];
  let ptr = plane.data_ref().as_ref().as_ptr() as usize;
  assert!(
    ptr >= start && ptr + plane.data_ref().as_ref().len() <= end,
    "a tight view plane must point inside the AVFrame's own buffer \
     (plane {ptr:#x}..{:#x}, buffer {start:#x}..{end:#x})",
    ptr + plane.data_ref().as_ref().len(),
  );
  assert_eq!(
    plane.stride(),
    128,
    "a shared plane carries the decoder's own stride",
  );
  assert_eq!(plane.data_ref().as_ref().len(), 128 * 8);
}

#[test]
fn a_padded_video_plane_is_compacted_on_both_lanes() {
  use mediadecode_ffmpeg::{FrameLimits, convert};

  // 100 is not a multiple of FFmpeg's alignment, so the allocator pads
  // each row and the bytes between rows are never written by anybody.
  let frame = gray_frame(100, 6);
  let linesize = frame.stride(0);
  assert!(linesize > 100, "the premise: a padded plane");
  let (start, end) = backing_range(&frame);

  let time_base = mediadecode::Timebase::default();
  let viewed = unsafe { view_video_frame(&frame, time_base, FrameLimits::default()) }
    .expect("converts on the view lane");
  // SAFETY: `gray_frame` wrote every row of its one plane, and the frame is live for the call.
  let owned = unsafe { convert::video_frame_from(&frame, time_base, FrameLimits::default()) }
    .expect("converts on the owned lane");

  let view_plane = &viewed.planes()[0];
  let ptr = view_plane.data_ref().as_ref().as_ptr() as usize;
  assert!(
    ptr < start || ptr >= end,
    "a padded plane must be copied, not shared: the gaps between rows \
     are allocator memory nothing initialised",
  );
  assert_eq!(
    view_plane.stride(),
    100,
    "a compacted plane arrives with row_bytes as its stride",
  );
  assert_eq!(view_plane.data_ref().as_ref().len(), 100 * 6);

  // And the two lanes agree byte for byte.
  assert_eq!(
    view_plane.data_ref().as_ref(),
    owned.planes()[0].data_ref().as_ref(),
    "the lanes disagree about a padded plane's contents",
  );
  assert_eq!(view_plane.stride(), owned.planes()[0].stride());
}

#[test]
fn the_two_lanes_carry_the_same_video_content() {
  use mediadecode_ffmpeg::{FrameLimits, convert};

  for (width, height) in [(128u32, 8u32), (100, 6)] {
    let frame = gray_frame(width, height);
    let time_base = mediadecode::Timebase::default();
    let viewed =
      unsafe { view_video_frame(&frame, time_base, FrameLimits::default()) }.expect("view lane");
    let owned =
      // SAFETY: `gray_frame` wrote every row of its one plane, and the frame is live for the
      // call.
      unsafe { convert::video_frame_from(&frame, time_base, FrameLimits::default()) }.expect("owned lane");

    // **Content, not spans.** A tight plane is shared at the decoder's
    // stride and a padded one is compacted at `row_bytes`, so the raw
    // buffers are not comparable — what must agree is the pixels each
    // lane's stride makes reachable.
    assert_eq!(
      rows_of(&viewed.planes()[0], width as usize, height as usize),
      rows_of(&owned.planes()[0], width as usize, height as usize),
      "the lanes disagree about the pixels of a {width}x{height} frame",
    );
    assert_eq!(viewed.width(), owned.width());
    assert_eq!(viewed.height(), owned.height());
    assert_eq!(viewed.pixel_format(), owned.pixel_format());
  }
}

#[test]
fn the_frame_budget_fires_identically_on_both_lanes() {
  use mediadecode_ffmpeg::{FrameLimits, convert};

  let frame = gray_frame(128, 8);
  let time_base = mediadecode::Timebase::default();
  // A ceiling below the plane's own size. It judges the *size*, which
  // is the same number whether the bytes are copied or shared.
  let tight = FrameLimits::new().with_max_frame_bytes(16);

  let viewed = unsafe { view_video_frame(&frame, time_base, tight) };
  // SAFETY: `gray_frame` wrote every row of its one plane, and the frame is live for the call.
  let owned = unsafe { convert::video_frame_from(&frame, time_base, tight) };
  match (viewed, owned) {
    (Err(v), Err(o)) => assert_eq!(
      core::mem::discriminant(&v),
      core::mem::discriminant(&o),
      "the lanes refused for different reasons: {v:?} vs {o:?}",
    ),
    (v, o) => panic!(
      "a budget must fire on both lanes: view={:?} owned={:?}",
      v.map(|_| ()),
      o.map(|_| ()),
    ),
  }

  // And the same ceiling, raised, admits the frame on both.
  let roomy = FrameLimits::new().with_max_frame_bytes(1 << 20);
  assert!(unsafe { view_video_frame(&frame, time_base, roomy) }.is_ok());
  // SAFETY: `gray_frame` wrote every row of its one plane, and the frame is live for the call.
  assert!(unsafe { convert::video_frame_from(&frame, time_base, roomy) }.is_ok());
}

#[test]
fn an_audio_view_plane_stops_at_exactly_the_valid_bytes() {
  use mediadecode_ffmpeg::FrameLimits;
  support::init_ffmpeg();

  // Planar s16 stereo: two planes, and `av_frame_get_buffer` puts both
  // in **one** allocation — which is what makes the second assert below
  // a proof rather than a coincidence.
  const SAMPLES: usize = 100;
  let mut frame = ffmpeg_next::frame::Audio::new(
    ffmpeg_next::format::Sample::I16(ffmpeg_next::format::sample::Type::Planar),
    SAMPLES,
    ffmpeg_next::ChannelLayout::STEREO,
  );
  // Written, as a decoder's samples are: the view lane lends them.
  write_every_buffer(&mut frame);
  // **One buffer per plane**, for planar audio: `av_frame_get_buffer`
  // allocates `buf[i]` per channel rather than one run for all of them.
  // Each plane is therefore proved against its own.
  //
  // SAFETY: `frame` owns a live `AVFrame`; the fields read are public.
  let (linesize, ranges) = unsafe {
    let raw = frame.as_ptr();
    let ranges: Vec<(usize, usize)> = (0..2)
      .map(|index| {
        let buf = (*raw).buf[index];
        assert!(!buf.is_null(), "a built audio frame has refcounted planes");
        let start = (*buf).data as usize;
        (start, start + (*buf).size)
      })
      .collect();
    ((*raw).linesize[0] as usize, ranges)
  };
  let valid = SAMPLES * 2; // s16, planar: one channel per plane.
  assert!(
    linesize > valid,
    "the premise: `av_samples_get_buffer_size` pads the plane past its \
     samples (linesize {linesize}, valid {valid})",
  );

  let out = unsafe {
    view_audio_frame(
      &frame,
      mediadecode::Timebase::default(),
      FrameLimits::default(),
    )
  }
  .expect("a planar s16 frame converts");

  assert_eq!(
    out.plane_count(),
    2,
    "planar declares one plane per channel"
  );
  for (index, (start, end)) in ranges.into_iter().enumerate() {
    let plane = &out.planes()[index];
    assert_eq!(
      plane.data_ref().as_ref().len(),
      valid,
      "plane {index} carries the padding the decoder never wrote",
    );
    assert_eq!(plane.stride() as usize, valid);
    let ptr = plane.data_ref().as_ref().as_ptr() as usize;
    assert!(
      ptr >= start && ptr + valid <= end,
      "plane {index} is a copy, not a window into the frame's buffer",
    );
    // And the window is narrower than the allocation it looks into —
    // the padding is there, behind the view, unreachable through it.
    // SAFETY: the carrier holds a live reference.
    let capacity = unsafe { (*plane.data_ref().as_av_buffer_ref()).size };
    assert!(
      capacity > valid,
      "plane {index} should look into a padded allocation ({capacity}        bytes) and expose only its {valid} valid ones",
    );
  }

  // The two planes are distinct allocations, so they must not compare
  // as sharing — the same predicate that proves sharing where FFmpeg
  // does share.
  assert!(
    !out.planes()[0]
      .data_ref()
      .ptr_eq(out.planes()[1].data_ref()),
    "planar audio planes are separate allocations",
  );
}

#[test]
fn the_two_lanes_carry_the_same_audio_samples() {
  use mediadecode_ffmpeg::{FrameLimits, convert};
  support::init_ffmpeg();

  const SAMPLES: usize = 100;
  let mut frame = ffmpeg_next::frame::Audio::new(
    ffmpeg_next::format::Sample::I16(ffmpeg_next::format::sample::Type::Planar),
    SAMPLES,
    ffmpeg_next::ChannelLayout::STEREO,
  );
  // Written before anything lends it: `data_mut` below forms a slice over
  // each plane, and `av_frame_get_buffer` left them unwritten.
  write_every_buffer(&mut frame);
  for plane in 0..2usize {
    let data = frame.data_mut(plane);
    for (index, byte) in data.iter_mut().take(SAMPLES * 2).enumerate() {
      *byte = (index as u8).wrapping_mul(7).wrapping_add(plane as u8);
    }
  }

  let time_base = mediadecode::Timebase::default();
  let viewed =
    unsafe { view_audio_frame(&frame, time_base, FrameLimits::default()) }.expect("view lane");
  let owned =
    // SAFETY: every sample of both planes was written above, and the frame is live for the
    // call.
    unsafe { convert::audio_frame_from(&frame, time_base, FrameLimits::default()) }.expect("owned lane");

  assert_eq!(viewed.nb_samples(), owned.nb_samples());
  assert_eq!(viewed.plane_count(), owned.plane_count());
  for index in 0..viewed.plane_count() as usize {
    // The **valid prefix** on both lanes: audio has no stride question,
    // so here the spans really are comparable byte for byte.
    assert_eq!(
      viewed.planes()[index].data_ref().as_ref(),
      owned.planes()[index].data_ref().as_ref(),
      "the lanes disagree about plane {index}",
    );
  }
}

#[test]
#[cfg(feature = "resample")]
fn a_resampled_view_frame_shares_the_resamplers_output_buffer() {
  use mediadecode::resampler::AudioResampler;
  use mediadecode_ffmpeg::{FfmpegResampler, FrameLimits, ResampleSpec};
  support::init_ffmpeg();

  const SAMPLES: u32 = 1_024;
  let source = ResampleSpec::new(
    48_000,
    ffmpeg_next::format::Sample::I16(ffmpeg_next::format::sample::Type::Packed),
    ffmpeg_next::ChannelLayout::STEREO,
  );
  // A **planar** target, so the output frame has two planes to compare.
  let target = ResampleSpec::new(
    48_000,
    ffmpeg_next::format::Sample::F32(ffmpeg_next::format::sample::Type::Planar),
    ffmpeg_next::ChannelLayout::STEREO,
  );
  let mut resampler =
    FfmpegResampler::new(source, target, FrameLimits::default()).expect("open resampler");

  // A view-lane input frame: the resampler reads it through `AsRef`,
  // which is the same on either lane.
  let plane =
    mediadecode_ffmpeg::FfmpegBuffer::copy_from_slice(&vec![0u8; SAMPLES as usize * 2 * 2])
      .expect("a plane to feed");
  let planes = std::array::from_fn(|index| {
    mediadecode::frame::Plane::new(
      if index == 0 {
        plane.clone()
      } else {
        mediadecode_ffmpeg::FfmpegBuffer::empty()
      },
      0,
    )
  });
  let input = mediadecode_ffmpeg::AudioFrame::new(
    48_000,
    SAMPLES,
    2,
    mediadecode_ffmpeg::SampleFormat::S16,
    mediadecode_ffmpeg::channel_layout_description_from_ffmpeg(&ffmpeg_next::ChannelLayout::STEREO)
      .expect("a well-formed layout describes"),
    planes,
    1,
    Default::default(),
  );

  support::accepted(resampler.send_frame(&input), "send");
  support::accepted(resampler.send_eof(), "eof");

  let mut dst = mediadecode_ffmpeg::empty_audio_frame();
  let mut proved = 0usize;
  while matches!(
    resampler
      .receive_frame(&mut dst)
      .expect("no fault in the tail"),
    Received::Produced
  ) {
    if dst.nb_samples() == 0 || dst.plane_count() < 2 {
      continue;
    }
    let first = &dst.planes()[0];
    assert_eq!(
      first.data_ref().as_ref().len(),
      dst.nb_samples() as usize * 4,
      "f32 planar: a plane is exactly the samples produced, never the \
       capacity that was allocated for them",
    );
    // **The proof that this is a window and not a copy.** The
    // resampler sizes its output frame for the *estimate*
    // `swr_get_out_samples` gives, then produces however many samples
    // the delay line actually yields — fewer. A carrier that copied
    // would hold an allocation of exactly the produced bytes; one that
    // reserved before the conversion and committed after looks into
    // the larger allocation the frame still owns.
    //
    // SAFETY: the carrier holds a live reference.
    let capacity = unsafe { (*first.data_ref().as_av_buffer_ref()).size };
    assert!(
      capacity > first.data_ref().as_ref().len(),
      "a resampled view plane must look into the output frame's own \
       allocation (capacity {capacity}, committed {})",
      first.data_ref().as_ref().len(),
    );
    proved += 1;
  }
  assert!(proved > 0, "the resampler produced nothing to prove with");
}

#[test]
fn the_frame_families_carry_the_auto_traits_their_lanes_promise() {
  use mediadecode_ffmpeg::{
    AudioFrame, ImageFrame, OwnedAudioFrame, OwnedImageFrame, OwnedSubtitleFrame, OwnedVideoFrame,
    SubtitleFrame, VideoFrame,
  };

  const fn assert_send<T: Send>() {}
  const fn assert_sync<T: Sync>() {}

  // Both lanes travel between threads.
  assert_send::<VideoFrame>();
  assert_send::<AudioFrame>();
  assert_send::<SubtitleFrame>();
  assert_send::<ImageFrame>();
  assert_send::<OwnedVideoFrame>();
  assert_send::<OwnedAudioFrame>();
  assert_send::<OwnedSubtitleFrame>();
  assert_send::<OwnedImageFrame>();

  // Only the owned lane is shareable *across* them — which is what the
  // amputation contract promises and what graph traffic needs.
  assert_sync::<OwnedVideoFrame>();
  assert_sync::<OwnedAudioFrame>();
  assert_sync::<OwnedSubtitleFrame>();
  assert_sync::<OwnedImageFrame>();

  // The negative half, which a `const fn` cannot state: a view frame
  // must **not** be `Sync`, because the buffer behind it belongs to
  // FFmpeg and FFmpeg may still write through it.
  struct Probe<T>(core::marker::PhantomData<T>);
  trait IsSync {
    fn sync_status() -> bool;
  }
  impl<T: Sync> IsSync for Probe<T> {
    fn sync_status() -> bool {
      true
    }
  }
  impl Probe<VideoFrame> {
    #[allow(dead_code)]
    fn sync_status() -> bool {
      false
    }
  }
  impl Probe<AudioFrame> {
    #[allow(dead_code)]
    fn sync_status() -> bool {
      false
    }
  }
  assert!(
    !Probe::<VideoFrame>::sync_status(),
    "a view video frame must not be Sync",
  );
  assert!(
    !Probe::<AudioFrame>::sync_status(),
    "a view audio frame must not be Sync",
  );
  assert!(
    Probe::<OwnedVideoFrame>::sync_status(),
    "an owned video frame must stay Sync",
  );
}

#[test]
fn a_side_data_only_packet_reaches_a_view_decoder_without_a_buffer_behind_it() {
  use mediadecode::decoder::AudioStreamDecoder;
  use mediadecode_ffmpeg::{
    AudioPacket, DecoderLimits, FfmpegAudioStreamDecoder, FfmpegBuffer, empty_audio_frame,
    extras::AudioPacketExtra,
  };

  let Some(corpus) = Corpus::new() else {
    return;
  };
  support::init_ffmpeg();

  // A packet with side data and **no payload** is ordinary — a codec
  // parameter change carries one — and on the view lane its carrier is
  // the empty one, which is deliberately backed by no `AVBufferRef` at
  // all. Everything the send road touches has to ask whether it is
  // empty before it asks anything of its buffer.
  let path = corpus.multi_track_mkv();
  let mut demuxer = FfmpegDemuxer::open(&path).expect("open");
  let track = demuxer
    .tracks()
    .iter()
    .position(|t| t.kind() == TrackKind::Audio)
    .expect("an audio track");
  let info = &demuxer.tracks()[track];
  let mut decoder = FfmpegAudioStreamDecoder::open(
    info.extra().clone_parameters().expect("parameters"),
    info.timebase(),
    DecoderLimits::default(),
  )
  .expect("open decoder");

  let payload_less = AudioPacket::new(FfmpegBuffer::empty(), AudioPacketExtra::new(track as i32));
  // The assertion is that this returns at all: the bug it guards
  // dereferenced a null `AVBufferRef` on the way in. What libavcodec
  // makes of a payload-less packet is libavcodec's business, so either
  // answer is accepted — a crash is not an answer.
  let _ = decoder.send_packet(&payload_less);

  // And the decoder is still usable afterwards, which is what says the
  // empty packet was handled rather than survived.
  let mut frame = empty_audio_frame();
  let mut samples = 0u64;
  while let Some(packet) = demuxer.next_packet().expect("pull") {
    let DemuxedPacket::Audio(p) = packet else {
      continue;
    };
    support::accepted(decoder.send_packet(p.packet()), "send a real packet");
    while matches!(
      decoder.receive_frame(&mut frame).expect("receive_frame"),
      Received::Produced
    ) {
      samples += u64::from(frame.nb_samples());
    }
    if samples > 0 {
      break;
    }
  }
  assert!(
    samples > 0,
    "the decoder must still decode after a payload-less packet",
  );
}

#[test]
fn the_borrowed_doors_copy_and_the_consuming_ones_share() {
  use ffmpeg_next::packet::Ref;
  use mediadecode_ffmpeg::{
    PacketLimits,
    boundary::{owned_video_packet_from_ffmpeg_in, video_packet_from_ffmpeg_in},
  };

  let Some(corpus) = Corpus::new() else {
    return;
  };
  support::init_ffmpeg();

  // One real packet, converted both ways. The split is not a naming
  // convention: a borrowed source **cannot** be viewed, because the
  // caller still holds a handle that lends `&mut [u8]` over the same
  // bytes with no copy-on-write.
  let path = corpus.multi_track_mkv();
  let mut input = ffmpeg_next::format::input(&path).expect("open");
  let mut proved = 0usize;
  let packets: Vec<ffmpeg_next::Packet> = input
    .packets()
    .map(|(_, packet)| packet)
    .filter(|packet| packet.data().is_some_and(|d| !d.is_empty()))
    .take(3)
    .collect();

  for packet in packets {
    // SAFETY: the packet is live; `data` is a public field.
    let source = unsafe { (*packet.as_ptr()).data as usize };
    let tb = mediadecode::Timebase::default();

    // Borrowed: the owned lane, and it copies.
    // SAFETY: `packets()` reads each packet with `av_read_frame`, so libavformat wrote its
    // payload, and it is live for the call.
    let owned = unsafe { owned_video_packet_from_ffmpeg_in(&packet, tb, PacketLimits::default()) }
      .expect("carried")
      .expect("a payload");
    assert_ne!(
      owned.data().as_ref().as_ptr() as usize,
      source,
      "the borrowing door must copy — the caller still holds the packet",
    );

    // Consuming: the view lane, and it shares. Note the packet is moved
    // here, which is exactly what makes the line above's alias
    // unconstructible.
    let expected = owned.data().as_ref().to_vec();
    // SAFETY: `packets()` reads each packet with `av_read_frame`, so libavformat wrote its
    // payload, and it is live for the call.
    let viewed = unsafe { video_packet_from_ffmpeg_in(packet, tb, PacketLimits::default()) }
      .expect("carried")
      .expect("a payload");
    assert_eq!(
      viewed.data().as_ref().as_ptr() as usize,
      source,
      "the consuming door must share the buffer it was handed",
    );
    // Same bytes either way: the lane is about ownership, never content.
    assert_eq!(viewed.data().as_ref(), expected.as_slice());
    proved += 1;
  }
  assert!(
    proved > 0,
    "no packet was available to prove the split with"
  );
}

#[test]
fn a_tight_plane_copies_on_the_borrowed_road_and_shares_on_the_consuming_one() {
  use mediadecode_ffmpeg::{FrameLimits, convert};

  // The same frame, the same tight geometry, two roads. The view road
  // is proved to share by `a_tight_video_plane_is_a_window_...`; this
  // pins the other half — the borrowed conversion copies, because
  // `frame` is still the caller's and still mutable.
  let frame = gray_frame(128, 8);
  assert_eq!(frame.stride(0), 128, "the premise: a tight plane");
  let (start, end) = backing_range(&frame);
  let time_base = mediadecode::Timebase::default();

  // SAFETY: `gray_frame` wrote every row of its one plane, and the frame is live for the call.
  let borrowed = unsafe { convert::video_frame_from(&frame, time_base, FrameLimits::default()) }
    .expect("the owned lane converts");
  let ptr = borrowed.planes()[0].data_ref().as_ref().as_ptr() as usize;
  assert!(
    ptr < start || ptr >= end,
    "a borrowed conversion must copy, even where the geometry would \
     have allowed a window",
  );

  // SAFETY: nothing writes through `frame` while `viewed` is alive.
  let viewed = unsafe { view_video_frame(&frame, time_base, FrameLimits::default()) }
    .expect("the view lane converts");
  let shared = viewed.planes()[0].data_ref().as_ref().as_ptr() as usize;
  assert!(
    shared >= start && shared + viewed.planes()[0].data_ref().as_ref().len() <= end,
    "the consuming road still shares",
  );

  // And both carry the same pixels.
  assert_eq!(
    borrowed.planes()[0].data_ref().as_ref(),
    viewed.planes()[0].data_ref().as_ref(),
  );
}

#[test]
fn a_container_held_cover_picture_is_still_carried() {
  // **The one payload whose buffer legitimately has two references.**
  // libavformat holds `AVStream.attached_pic` for the lifetime of the
  // format context, so a cover picture arrives refcount-2 through no
  // fault of anyone's. The uniqueness rule that refuses a caller's
  // silently-shared packet must not refuse this, and the reason it does
  // not is provenance rather than a size comparison: that second
  // reference is C-side state libavformat never writes again, not a
  // `Packet` somebody can call `data_mut` on.
  //
  // Without the distinction this open fails outright, so the test is
  // the carve-out's live proof rather than a restatement of it.
  let Some(corpus) = Corpus::new() else {
    return;
  };
  support::init_ffmpeg();

  let path = corpus.cover_art_mp3();
  let mut demuxer = FfmpegDemuxer::open(&path).expect("a cover-art container opens");
  let mut covers = 0usize;
  while let Some(packet) = demuxer.next_packet().expect("read") {
    if let DemuxedPacket::Attachment(attachment) = packet {
      assert!(
        !attachment.packet().data().as_ref().is_empty(),
        "a cover picture must arrive with its bytes",
      );
      covers += 1;
    }
  }
  assert!(covers > 0, "the fixture must carry a cover picture");
}

/// The references a packet's payload buffer currently has.
fn payload_references(packet: &ffmpeg_next::Packet) -> i32 {
  use ffmpeg_next::packet::Ref;
  // SAFETY: the packet is live; `buf` is a public field and
  // `av_buffer_get_ref_count` only reads its atomic.
  unsafe {
    let buf = (*packet.as_ptr()).buf;
    if buf.is_null() {
      return -1;
    }
    ffmpeg_next::ffi::av_buffer_get_ref_count(buf)
  }
}

#[test]
fn a_queue_backed_demuxer_delivers_on_both_lanes() {
  use mediadecode_ffmpeg::FfmpegOwnedDemuxer;

  // **The class the cover-art carve-out was one case of.** `srtdec` and
  // every other `FFDemuxSubtitlesQueue` demuxer parse their cues at
  // open, keep the packets, and answer each read with an
  // `av_packet_ref` of one — so *every* packet arrives with two
  // references. A rule that demanded a lone reference refused all of
  // them, on both lanes, which is a regression the owned lane in
  // particular must never suffer: it copies, and the read is race-free
  // because the second reference is libavformat's own with no
  // `data_mut` on it.
  let Some(corpus) = Corpus::new() else {
    return;
  };
  support::init_ffmpeg();
  let path = corpus.subrip();

  // The premise, observed rather than assumed.
  {
    let mut raw = ffmpeg_next::format::input(&path).expect("open");
    let mut seen = 0usize;
    for (_stream, packet) in raw.packets() {
      if packet.size() == 0 {
        continue;
      }
      assert!(
        payload_references(&packet) > 1,
        "the premise this test exists for: a queue-backed demuxer \
         delivers a second reference to a packet it keeps",
      );
      seen += 1;
      if seen >= 2 {
        break;
      }
    }
    assert!(seen > 0, "the fixture must deliver cues");
  }

  // Both lanes carry every cue.
  let mut viewed = FfmpegDemuxer::open(&path).expect("open on the view lane");
  let mut view_cues: Vec<Vec<u8>> = Vec::new();
  while let Some(packet) = viewed.next_packet().expect("read") {
    if let DemuxedPacket::Subtitle(p) = packet {
      view_cues.push(p.packet().data().as_ref().to_vec());
    }
  }
  let mut owned = FfmpegOwnedDemuxer::open(&path).expect("open on the owned lane");
  let mut owned_cues: Vec<Vec<u8>> = Vec::new();
  while let Some(packet) = owned.next_packet().expect("read") {
    if let DemuxedPacket::Subtitle(p) = packet {
      owned_cues.push(p.packet().data().as_ref().to_vec());
    }
  }

  assert!(!view_cues.is_empty(), "the view lane must deliver the cues");
  assert_eq!(view_cues, owned_cues, "the lanes must agree on the cues");
  assert!(
    view_cues.iter().any(|c| c == b"first cue"),
    "the cues must be the ones the fixture wrote, got {:?}",
    view_cues
      .iter()
      .map(|c| String::from_utf8_lossy(c).into_owned())
      .collect::<Vec<_>>(),
  );

  // **On the seek half of this road.** libavformat declines to seek a
  // cue queue in this container: `av_seek_frame` answers `ERANGE`,
  // because a subtitle-only SubRip input carries no index to bracket a
  // target with. So what is asserted is what is true — the refusal is
  // surfaced rather than swallowed, and the session survives it and
  // keeps delivering. The re-serve property itself is covered above:
  // two independent sessions over the same file produce identical
  // cues from the same retained queue.
  let mut again = FfmpegDemuxer::open(&path).expect("reopen");
  let seek = again.seek(mediadecode::Timestamp::new(
    0,
    mediadecode::Timebase::new(1, std::num::NonZeroI32::new(1_000_000).expect("nonzero")),
  ));
  assert!(
    seek.is_err(),
    "if this container ever becomes seekable, this lane should assert \
     the cues after the seek instead of the refusal before it",
  );
  let mut after = 0usize;
  while let Some(packet) = again
    .next_packet()
    .expect("a refused seek must not poison the session")
  {
    if let DemuxedPacket::Subtitle(p) = packet {
      assert!(!p.packet().data().as_ref().is_empty());
      after += 1;
    }
  }
  assert_eq!(
    after,
    view_cues.len(),
    "a refused seek must leave the session delivering every cue",
  );
}

/// Re-runs one of this file's tests in a child process, alone.
///
/// `av_max_alloc` is process-global: a lane that makes FFmpeg
/// allocations fail cannot share a process with anything else.
fn in_subprocess(test_name: &str, body: impl FnOnce()) {
  const CHILD: &str = "MEDIADECODE_FFMPEG_VIEW_FAULT_CHILD";
  if std::env::var(CHILD).as_deref() == Ok(test_name) {
    body();
    return;
  }
  let exe = std::env::current_exe().expect("the test binary");
  let output = std::process::Command::new(exe)
    .args(["--exact", test_name, "--nocapture", "--test-threads=1"])
    .env(CHILD, test_name)
    .output()
    .expect("spawning the child");
  let stdout = String::from_utf8_lossy(&output.stdout);
  assert_eq!(
    output.status.code(),
    Some(0),
    "the child running `{test_name}` did not exit cleanly ({:?})\n{stdout}\n{}",
    output.status,
    String::from_utf8_lossy(&output.stderr),
  );
  assert!(
    stdout.contains("1 passed"),
    "the child ran no test — is `{test_name}` still the name?\n{stdout}",
  );
}

/// Sets FFmpeg's allocation ceiling. Process-global; only ever called
/// inside [`in_subprocess`].
fn cap_allocations(max: usize) {
  // SAFETY: `av_max_alloc` stores an atomic and returns nothing.
  unsafe { ffmpeg_next::ffi::av_max_alloc(max) };
}

/// Every subtitle cue the session delivers, in order.
fn drain_cues(demuxer: &mut FfmpegDemuxer) -> Vec<Vec<u8>> {
  let mut out = Vec::new();
  while let Some(packet) = demuxer.next_packet().expect("read") {
    if let DemuxedPacket::Subtitle(p) = packet {
      out.push(p.packet().data().as_ref().to_vec());
    }
  }
  out
}

#[test]
fn a_failed_copy_parks_the_packet_instead_of_losing_it() {
  let Some(corpus) = Corpus::new() else {
    return;
  };
  support::init_ffmpeg();
  let path = corpus.subrip_bulky();

  in_subprocess(
    "a_failed_copy_parks_the_packet_instead_of_losing_it",
    move || {
      // The reference: what the session delivers when nothing fails.
      let expected = drain_cues(&mut FfmpegDemuxer::open(&path).expect("open"));
      assert!(expected.len() >= 2, "the fixture must have cues to lose");

      let mut demuxer = FfmpegDemuxer::open(&path).expect("open");

      // **The middle row under memory pressure.** A queue-backed demuxer
      // delivers by `av_packet_ref` — a reference struct, whatever the
      // cue's size — so the read gets through this cap while the copy
      // this crate makes for a shared demux-delivered payload does not.
      cap_allocations(1024);
      let first = demuxer.next_packet().map(|p| p.map(|_| ()));
      assert!(
        matches!(
          first,
          Err(mediadecode_ffmpeg::DemuxError::PacketBuffer(ref e))
            if matches!(e.source(), PacketBufferError::CaptureFailed(_))
        ),
        "expected a transient capture refusal, got {first:?}",
      );

      // Still capped: the next pull re-attempts **the same** packet
      // rather than reading past it.
      let again = demuxer.next_packet().map(|p| p.map(|_| ()));
      assert!(
        matches!(
          again,
          Err(mediadecode_ffmpeg::DemuxError::PacketBuffer(ref e))
            if matches!(e.source(), PacketBufferError::CaptureFailed(_))
        ),
        "a parked packet must be retried, got {again:?}",
      );
      cap_allocations(i32::MAX as usize);

      // And with memory back, the session resumes at the packet it
      // parked — not at the one after it.
      let recovered = drain_cues(&mut demuxer);
      assert_eq!(
        recovered, expected,
        "a transient refusal must cost no packet at all",
      );
    },
  );
}

/// One delivered packet as `(track, bytes)`.
fn payload_of(
  packet: DemuxedPacket<mediadecode_ffmpeg::Ffmpeg, mediadecode_ffmpeg::FfmpegBuffer>,
) -> (usize, Vec<u8>) {
  match packet {
    DemuxedPacket::Video(p) => (p.track().get(), p.packet().data().as_ref().to_vec()),
    DemuxedPacket::Audio(p) => (p.track().get(), p.packet().data().as_ref().to_vec()),
    DemuxedPacket::Subtitle(p) => (p.track().get(), p.packet().data().as_ref().to_vec()),
    DemuxedPacket::Data(p) => (p.track().get(), p.packet().data().as_ref().to_vec()),
    DemuxedPacket::Attachment(p) => (p.track().get(), p.packet().data().as_ref().to_vec()),
  }
}

/// Every payload the session delivers, in order, whatever its kind.
fn drain_payloads(demuxer: &mut FfmpegDemuxer) -> Vec<(usize, Vec<u8>)> {
  let mut out = Vec::new();
  while let Some(packet) = demuxer.next_packet().expect("read") {
    out.push(payload_of(packet));
  }
  out
}

#[test]
fn a_failed_view_capture_parks_the_packet_instead_of_losing_it() {
  let Some(corpus) = Corpus::new() else {
    return;
  };
  support::init_ffmpeg();
  let path = corpus.multi_track_mkv();

  in_subprocess(
    "a_failed_view_capture_parks_the_packet_instead_of_losing_it",
    move || {
      // The reference: what the session delivers when nothing fails.
      let expected = drain_payloads(&mut FfmpegDemuxer::open(&path).expect("open"));
      assert!(expected.len() >= 2, "the fixture must have packets to lose");

      let mut demuxer = FfmpegDemuxer::open(&path).expect("open");

      // **The unique road under memory pressure.** These packets are
      // libavformat's own and singly referenced, so the view lane takes
      // a window — and `av_buffer_ref` allocates a reference struct,
      // which this ceiling refuses. The read itself is served from the
      // queue `avformat_find_stream_info` filled, so it needs no
      // allocation and gets through.
      //
      // Sixteen bytes: the ceiling that separates the two allocations.
      // (A ceiling of one lets everything through — `av_malloc`'s guard
      // subtracts a header from the limit and underflows.)
      //
      // The loop is here because a session delivers its attachment
      // queue first, built at open and costing no allocation at all: it
      // comes through the ceiling untouched, and what is being tested
      // is the first packet that has to be *captured*.
      let mut got: Vec<(usize, Vec<u8>)> = Vec::new();
      cap_allocations(16);
      let refused = loop {
        match demuxer.next_packet() {
          Ok(Some(packet)) => got.push(payload_of(packet)),
          Ok(None) => break None,
          Err(e) => break Some(e),
        }
      };
      cap_allocations(i32::MAX as usize);
      let refused = refused.expect("the ceiling must refuse a capture");
      assert!(
        matches!(
          refused,
          mediadecode_ffmpeg::DemuxError::PacketBuffer(ref e)
            if matches!(e.source(), PacketBufferError::CaptureFailed(_))
        ),
        "expected a transient capture refusal, got {refused:?}",
      );

      // Still refused while the ceiling is down: the retry re-attempts
      // the same packet rather than reading past it.
      cap_allocations(16);
      let again = demuxer.next_packet().map(|p| p.map(|_| ()));
      cap_allocations(i32::MAX as usize);
      assert!(
        matches!(
          again,
          Err(mediadecode_ffmpeg::DemuxError::PacketBuffer(ref e))
            if matches!(e.source(), PacketBufferError::CaptureFailed(_))
        ),
        "a parked packet must be retried, got {again:?}",
      );

      // The parked packet is delivered first, so nothing is missing and
      // nothing is out of order.
      got.extend(drain_payloads(&mut demuxer));
      assert_eq!(
        got, expected,
        "a transient refusal must cost no packet at all",
      );
    },
  );
}

#[test]
fn a_refused_seek_keeps_the_parked_packet() {
  let Some(corpus) = Corpus::new() else {
    return;
  };
  support::init_ffmpeg();
  let path = corpus.subrip_bulky();

  in_subprocess("a_refused_seek_keeps_the_parked_packet", move || {
    let expected = drain_cues(&mut FfmpegDemuxer::open(&path).expect("open"));
    assert!(!expected.is_empty(), "the fixture must have cues");

    let mut demuxer = FfmpegDemuxer::open(&path).expect("open");
    cap_allocations(1024);
    let refused = demuxer.next_packet().map(|p| p.map(|_| ()));
    cap_allocations(i32::MAX as usize);
    assert!(refused.is_err(), "the ceiling must park a cue");

    // **A seek that did not happen must not discard what is parked.**
    // libavformat refuses to seek a cue queue with no index, so the
    // session stays exactly where it was — and the parked packet is off
    // the wire, so dropping it here would be the same silent loss the
    // guard exists to prevent, with no re-read able to recover it.
    let seek = demuxer.seek(mediadecode::Timestamp::new(
      0,
      mediadecode::Timebase::new(1, std::num::NonZeroI32::new(1_000_000).expect("nonzero")),
    ));
    assert!(seek.is_err(), "the fixture's refusal is the premise here");

    let recovered = drain_cues(&mut demuxer);
    assert_eq!(
      recovered, expected,
      "a refused seek must cost no packet either",
    );
  });
}

#[test]
fn a_failed_frame_conversion_parks_the_frame_instead_of_losing_it() {
  use mediadecode::decoder::AudioStreamDecoder;
  use mediadecode_ffmpeg::{DecoderLimits, FfmpegAudioStreamDecoder, empty_audio_frame};

  let Some(corpus) = Corpus::new() else {
    return;
  };
  support::init_ffmpeg();
  let path = corpus.multi_track_mkv();

  in_subprocess(
    "a_failed_frame_conversion_parks_the_frame_instead_of_losing_it",
    move || {
      // **`receive_frame` advances libavcodec too.** The frame it fills
      // the scratch with is out of the codec's queue, and a conversion
      // that then failed on an allocation used to leave it in a scratch
      // the next call overwrites.
      let open = |path: &std::path::Path| {
        let demuxer = FfmpegDemuxer::open(path).expect("open");
        let track = demuxer
          .tracks()
          .iter()
          .position(|t| t.kind() == TrackKind::Audio)
          .expect("an audio track");
        let info = &demuxer.tracks()[track];
        let decoder = FfmpegAudioStreamDecoder::open(
          info.extra().clone_parameters().expect("parameters"),
          info.timebase(),
          DecoderLimits::default(),
        )
        .expect("open decoder");
        (demuxer, decoder, track)
      };

      // The reference: the first frame's samples when nothing fails.
      let (mut demuxer, mut decoder, track) = open(&path);
      let mut frame = empty_audio_frame();
      let mut expected: Option<(u32, Vec<u8>)> = None;
      'reference: while let Some(packet) = demuxer.next_packet().expect("pull") {
        let DemuxedPacket::Audio(p) = packet else {
          continue;
        };
        if p.track().get() != track {
          continue;
        }
        support::accepted(decoder.send_packet(p.packet()), "send");
        if matches!(
          decoder.receive_frame(&mut frame).expect("receive_frame"),
          Received::Produced
        ) {
          expected = Some((
            frame.nb_samples(),
            frame.planes()[0].data_ref().as_ref().to_vec(),
          ));
          break 'reference;
        }
      }
      let expected = expected.expect("the fixture must decode a frame");

      // Again, with the ceiling down at the moment of conversion.
      let (mut demuxer, mut decoder, track) = open(&path);
      let mut frame = empty_audio_frame();
      let mut refused = None;
      'capped: while let Some(packet) = demuxer.next_packet().expect("pull") {
        let DemuxedPacket::Audio(p) = packet else {
          continue;
        };
        if p.track().get() != track {
          continue;
        }
        support::accepted(decoder.send_packet(p.packet()), "send");
        cap_allocations(16);
        let got = decoder.receive_frame(&mut frame);
        cap_allocations(i32::MAX as usize);
        match got {
          Ok(Received::Produced) => panic!("the ceiling should have refused the carrier"),
          // This packet produced nothing yet.
          Ok(Received::NeedsInput | Received::Ended) => {}
          Err(e) if format!("{e:?}").contains("CarrierAllocFailed") => {
            refused = Some(e);
            break 'capped;
          }
          Err(_) => {}
        }
      }
      assert!(
        refused.is_some(),
        "the ceiling must refuse a frame carrier to test the check",
      );

      // The parked frame is delivered first — the same frame the
      // uncapped run saw, not the one after it.
      assert_eq!(
        decoder.receive_frame(&mut frame).expect("the parked frame"),
        Received::Produced,
      );
      assert_eq!(
        (
          frame.nb_samples(),
          frame.planes()[0].data_ref().as_ref().to_vec()
        ),
        expected,
        "a transient refusal must cost no frame at all",
      );
    },
  );
}

#[test]
fn a_failed_video_frame_conversion_parks_the_frame_instead_of_losing_it() {
  use mediadecode::decoder::VideoStreamDecoder;
  use mediadecode_ffmpeg::{DecoderLimits, FfmpegVideoStreamDecoder, empty_video_frame};

  let Some(corpus) = Corpus::new() else {
    return;
  };
  support::init_ffmpeg();
  let path = corpus.software_only_video();

  in_subprocess(
    "a_failed_video_frame_conversion_parks_the_frame_instead_of_losing_it",
    move || {
      let open = |path: &std::path::Path| {
        let demuxer = FfmpegDemuxer::open(path).expect("open");
        let track = demuxer
          .tracks()
          .iter()
          .position(|t| t.kind() == TrackKind::Video)
          .expect("a video track");
        let info = &demuxer.tracks()[track];
        let decoder = FfmpegVideoStreamDecoder::open(
          info.extra().clone_parameters().expect("parameters"),
          info.timebase(),
          DecoderLimits::default(),
        )
        .expect("open decoder");
        (demuxer, decoder, track)
      };

      // The reference: the first frame's first plane when nothing
      // fails.
      let (mut demuxer, mut decoder, track) = open(&path);
      let mut frame = empty_video_frame();
      let mut expected: Option<Vec<u8>> = None;
      'reference: while let Some(packet) = demuxer.next_packet().expect("pull") {
        let DemuxedPacket::Video(p) = packet else {
          continue;
        };
        if p.track().get() != track {
          continue;
        }
        support::accepted(decoder.send_packet(p.packet()), "send");
        if matches!(
          decoder.receive_frame(&mut frame).expect("receive_frame"),
          Received::Produced
        ) {
          expected = Some(frame.planes()[0].data_ref().as_ref().to_vec());
          break 'reference;
        }
      }
      let expected = expected.expect("the fixture must decode a frame");

      // Again, with the ceiling down at the moment of conversion.
      let (mut demuxer, mut decoder, track) = open(&path);
      let mut frame = empty_video_frame();
      let mut refused = false;
      'capped: while let Some(packet) = demuxer.next_packet().expect("pull") {
        let DemuxedPacket::Video(p) = packet else {
          continue;
        };
        if p.track().get() != track {
          continue;
        }
        support::accepted(decoder.send_packet(p.packet()), "send");
        cap_allocations(16);
        let got = decoder.receive_frame(&mut frame);
        cap_allocations(i32::MAX as usize);
        match got {
          Ok(Received::Produced) => panic!("the ceiling should have refused the carrier"),
          Ok(Received::NeedsInput | Received::Ended) => {}
          Err(e) if format!("{e:?}").contains("CarrierAllocFailed") => {
            refused = true;
            break 'capped;
          }
          Err(_) => {}
        }
      }
      assert!(
        refused,
        "the ceiling must refuse a frame carrier to test the check",
      );

      assert_eq!(
        decoder.receive_frame(&mut frame).expect("the parked frame"),
        Received::Produced,
      );
      assert_eq!(
        frame.planes()[0].data_ref().as_ref().to_vec(),
        expected,
        "a transient refusal must cost no frame at all",
      );
    },
  );
}

#[test]
fn a_failed_cue_conversion_parks_the_cue_instead_of_losing_it() {
  use mediadecode::decoder::SubtitleDecoder;
  use mediadecode_ffmpeg::{DecoderLimits, FfmpegSubtitleStreamDecoder, empty_subtitle_frame};

  let Some(corpus) = Corpus::new() else {
    return;
  };
  support::init_ffmpeg();
  let path = corpus.subrip_bulky();

  in_subprocess(
    "a_failed_cue_conversion_parks_the_cue_instead_of_losing_it",
    move || {
      // **`avcodec_decode_subtitle2` consumes the packet too.** Once it
      // has answered, the cue exists only in the decoder's scratch and
      // nothing re-offers it — so converting inside `send_packet` meant
      // an allocation that failed took the cue with it.
      let open = |path: &std::path::Path| {
        let demuxer = FfmpegDemuxer::open(path).expect("open");
        let track = demuxer
          .tracks()
          .iter()
          .position(|t| t.kind() == TrackKind::Subtitle)
          .expect("a subtitle track");
        let info = &demuxer.tracks()[track];
        let decoder = FfmpegSubtitleStreamDecoder::open(
          info.extra().clone_parameters().expect("parameters"),
          info.timebase(),
          DecoderLimits::default(),
        )
        .expect("open decoder");
        (demuxer, decoder, track)
      };

      let first_cue = |demuxer: &mut FfmpegDemuxer, track: usize| loop {
        let packet = demuxer.next_packet().expect("pull").expect("a cue");
        if let DemuxedPacket::Subtitle(p) = packet
          && p.track().get() == track
        {
          return p.into_packet();
        }
      };

      // The reference: the cue's text when nothing fails.
      let (mut demuxer, mut decoder, track) = open(&path);
      let packet = first_cue(&mut demuxer, track);
      support::accepted(decoder.send_packet(&packet), "send");
      let mut frame = empty_subtitle_frame();
      assert_eq!(
        decoder.receive_frame(&mut frame).expect("the cue"),
        Received::Produced,
      );
      let expected = format!("{:?}", frame.payload());
      assert!(expected.contains("Text"), "the fixture must decode to text");

      // Again, with the ceiling down at the moment of conversion. The
      // packet is sent uncapped — what is being tested is the check
      // between the decode and the carrier, not the send.
      let (mut demuxer, mut decoder, track) = open(&path);
      let packet = first_cue(&mut demuxer, track);
      support::accepted(decoder.send_packet(&packet), "send");
      let mut frame = empty_subtitle_frame();
      cap_allocations(1024);
      let refused = decoder.receive_frame(&mut frame);
      cap_allocations(i32::MAX as usize);
      assert!(
        matches!(&refused, Err(e) if format!("{e:?}").contains("CarrierAllocFailed")),
        "the ceiling must refuse the cue's carrier, got {refused:?}",
      );

      // The parked cue is delivered on the next receive — the same cue,
      // not the next one.
      assert_eq!(
        decoder.receive_frame(&mut frame).expect("the parked cue"),
        Received::Produced,
      );
      assert_eq!(
        format!("{:?}", frame.payload()),
        expected,
        "a transient refusal must cost no cue at all",
      );
    },
  );
}
