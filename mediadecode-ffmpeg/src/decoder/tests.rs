use super::*;
// `Packet::as_mut_ptr` lives on this trait; the lanes below write raw
// packet fields the way libavformat would.
use ffmpeg_next::codec::packet::Mut as PacketMut;
use ffmpeg_next::ffi::AVCodecID;

#[test]
fn no_codec_for_unknown_id() {
  let err = Error::NoCodec(0);
  assert!(format!("{err}").contains("no decoder"));
}

#[test]
fn videodecoder_is_send() {
  _assert_send();
}

#[test]
fn is_transient_recognises_eagain_and_eof() {
  let eagain = ffmpeg_next::Error::Other {
    errno: ffmpeg_next::error::EAGAIN,
  };
  assert!(is_transient(&eagain));
  assert!(is_transient(&ffmpeg_next::Error::Eof));
  let other = ffmpeg_next::Error::InvalidData;
  assert!(!is_transient(&other));
}

/// Regression: a `codec::Parameters` with a null inner pointer must be
/// rejected at the entrypoint, not deref'd. ffmpeg-next's
/// `Parameters::new()` does not check `avcodec_parameters_alloc()`, so a
/// safe caller can hand us such a value under OOM.
#[test]
fn open_rejects_null_parameters() {
  // SAFETY: Parameters::wrap accepts any pointer; we explicitly construct
  // one with null inner. avcodec_parameters_free is null-safe on Drop.
  let null_params = unsafe { codec::Parameters::wrap(std::ptr::null_mut(), None) };
  match VideoDecoder::open(null_params) {
    Ok(_) => panic!("open should fail on null parameters"),
    Err(Error::Ffmpeg(ffmpeg_next::Error::Other { errno })) => {
      assert_eq!(errno, libc::ENOMEM, "expected ENOMEM, got {errno}");
    }
    Err(other) => panic!("expected Ffmpeg(Other {{ ENOMEM }}), got {other:?}"),
  }
}

#[test]
fn open_with_rejects_null_parameters() {
  // SAFETY: see open_rejects_null_parameters.
  let null_params = unsafe { codec::Parameters::wrap(std::ptr::null_mut(), None) };
  match VideoDecoder::open_with(null_params, Backend::VideoToolbox) {
    Ok(_) => panic!("open_with should fail on null parameters"),
    Err(Error::Ffmpeg(ffmpeg_next::Error::Other { errno })) => {
      assert_eq!(errno, libc::ENOMEM, "expected ENOMEM, got {errno}");
    }
    Err(other) => panic!("expected Ffmpeg(Other {{ ENOMEM }}), got {other:?}"),
  }
}

/// `try_clone_packet` calls `av_packet_ref`, which deep-copies side
/// data via `av_packet_copy_props`. The probe budget therefore has to
/// include side-data bytes — otherwise a stream with a 16-byte payload
/// and a 1 MiB side-data attachment would only consume 16 bytes of the
/// 64 MiB budget per packet, and 256 buffered clones would retain
/// ~256 MiB of side data while logs claim a few KiB.
#[test]
fn packet_side_data_counts_against_probe_budget() {
  use ffmpeg_next::ffi::{AVPacketSideDataType, av_packet_new_side_data};

  const PAYLOAD_SIZE: usize = 16;
  const SIDE_DATA_SIZE: usize = 1024 * 1024; // 1 MiB

  let mut packet = Packet::new(PAYLOAD_SIZE);
  // SAFETY: packet is a freshly allocated AVPacket; av_packet_new_side_data
  // attaches a fresh `SIDE_DATA_SIZE`-byte buffer of the requested type
  // to it and returns a writable pointer (or NULL on OOM).
  let p = unsafe {
    av_packet_new_side_data(
      packet.as_mut_ptr(),
      AVPacketSideDataType::AV_PKT_DATA_NEW_EXTRADATA,
      SIDE_DATA_SIZE,
    )
  };
  assert!(!p.is_null(), "av_packet_new_side_data returned NULL");

  assert_eq!(packet.size(), PAYLOAD_SIZE);
  let side = packet_side_data_bytes(&packet, MAX_PROBE_PACKET_SIDE_DATA_ENTRIES);
  assert!(
    side >= SIDE_DATA_SIZE,
    "side-data accounting must include the attached buffer; got {side}"
  );
  let total = packet.size().saturating_add(side);
  assert!(
    total >= PAYLOAD_SIZE + SIDE_DATA_SIZE,
    "probe budget must charge payload + side data; got {total}"
  );
}

#[test]
fn packet_side_data_is_zero_when_no_side_data() {
  let packet = Packet::new(64);
  assert_eq!(
    packet_side_data_bytes(&packet, MAX_PROBE_PACKET_SIDE_DATA_ENTRIES),
    0
  );
  assert_eq!(packet_side_data_count(&packet), 0);
}

/// Packets with many tiny side-data entries must be charged the
/// per-entry descriptor + ref overhead, even when each entry's payload
/// `size` is zero. Without `SIDE_DATA_ENTRY_OVERHEAD`, a packet stuffed
/// with N zero-byte entries would charge 0 bytes against the budget
/// while `av_packet_ref` still allocates ~`N * 80` bytes of descriptor
/// + AVBufferRef + allocator overhead per cloned copy.
#[test]
fn packet_side_data_bytes_charges_descriptor_overhead_for_zero_size_entries() {
  use ffmpeg_next::ffi::{AVPacketSideDataType, av_packet_new_side_data};

  let mut packet = Packet::new(0);
  // Attach two zero-byte entries of distinct types so neither call
  // replaces the other.
  let p1 = unsafe {
    av_packet_new_side_data(
      packet.as_mut_ptr(),
      AVPacketSideDataType::AV_PKT_DATA_NEW_EXTRADATA,
      0,
    )
  };
  let p2 = unsafe {
    av_packet_new_side_data(
      packet.as_mut_ptr(),
      AVPacketSideDataType::AV_PKT_DATA_PALETTE,
      0,
    )
  };
  assert!(
    !p1.is_null() && !p2.is_null(),
    "av_packet_new_side_data NULL"
  );

  assert_eq!(packet_side_data_count(&packet), 2);
  let bytes = packet_side_data_bytes(&packet, MAX_PROBE_PACKET_SIDE_DATA_ENTRIES);
  assert!(
    bytes >= 2 * SIDE_DATA_ENTRY_OVERHEAD,
    "must charge descriptor overhead per entry even at zero payload; got {bytes}"
  );
}

/// `packet_side_data_bytes` must clamp its walk to `max_entries`
/// regardless of `side_data_elems`. Defense-in-depth: the caller is
/// expected to short-circuit packets whose count exceeds the cap, but
/// if a corrupt or weaponised packet ever does reach the helper, the
/// internal cap prevents an unbounded raw-pointer walk.
///
/// This test attaches 5 entries of distinct types and asks the helper
/// to walk only the first 2. Result must equal exactly `2 * overhead +
/// (size_a + size_b)`, confirming entries 3-5 were not even read.
#[test]
fn packet_side_data_bytes_respects_max_entries_cap() {
  use ffmpeg_next::ffi::{AVPacketSideDataType, av_packet_new_side_data};

  let mut packet = Packet::new(0);
  // Five distinct side-data types so each `av_packet_new_side_data`
  // call appends rather than replaces.
  let types_and_sizes: [(AVPacketSideDataType, usize); 5] = [
    (AVPacketSideDataType::AV_PKT_DATA_NEW_EXTRADATA, 100),
    (AVPacketSideDataType::AV_PKT_DATA_PALETTE, 200),
    (AVPacketSideDataType::AV_PKT_DATA_REPLAYGAIN, 300),
    (AVPacketSideDataType::AV_PKT_DATA_DISPLAYMATRIX, 400),
    (AVPacketSideDataType::AV_PKT_DATA_STEREO3D, 500),
  ];
  for (ty, size) in types_and_sizes {
    let p = unsafe { av_packet_new_side_data(packet.as_mut_ptr(), ty, size) };
    assert!(!p.is_null(), "av_packet_new_side_data returned NULL");
  }
  assert_eq!(packet_side_data_count(&packet), 5);

  let walked_2 = packet_side_data_bytes(&packet, 2);
  let walked_5 = packet_side_data_bytes(&packet, 5);

  assert_eq!(
    walked_2,
    2 * SIDE_DATA_ENTRY_OVERHEAD + 100 + 200,
    "max_entries=2 must walk exactly the first two entries"
  );
  assert_eq!(
    walked_5,
    5 * SIDE_DATA_ENTRY_OVERHEAD + 100 + 200 + 300 + 400 + 500,
    "max_entries=5 must walk all five entries"
  );
  // max_entries=0 short-circuits to 0.
  assert_eq!(packet_side_data_bytes(&packet, 0), 0);
  // max_entries larger than the actual count clamps to the actual count
  // (no out-of-bounds walk past `side_data_elems`).
  let walked_huge = packet_side_data_bytes(&packet, 1_000_000);
  assert_eq!(walked_huge, walked_5);
}

/// `MAX_PROBE_PACKET_SIDE_DATA_ENTRIES` is the cliff above which a
/// packet is rejected from the probe buffer regardless of byte total —
/// pure descriptor inflation is its own attack vector. Sanity-check
/// that `packet_side_data_count` reports the value the cap is checked
/// against.
#[test]
fn packet_side_data_count_reports_attached_entries() {
  use ffmpeg_next::ffi::{AVPacketSideDataType, av_packet_new_side_data};

  let mut packet = Packet::new(0);
  let _p1 = unsafe {
    av_packet_new_side_data(
      packet.as_mut_ptr(),
      AVPacketSideDataType::AV_PKT_DATA_NEW_EXTRADATA,
      4,
    )
  };
  let _p2 = unsafe {
    av_packet_new_side_data(
      packet.as_mut_ptr(),
      AVPacketSideDataType::AV_PKT_DATA_PALETTE,
      4,
    )
  };
  assert_eq!(packet_side_data_count(&packet), 2);
}

/// `cpu_frame_bytes` must refuse to size a frame whose first plane has
/// a negative `linesize`. Pre-fix, the loop break treated negative the
/// same as zero (FFmpeg's "no more populated planes" sentinel), so a
/// vertically-flipped frame returned `Some(0)` and `drain_into_pending`
/// would queue it as a 0-byte allocation — letting up to
/// `MAX_PROBE_PENDING_FRAMES` such frames bypass the configured byte
/// budget entirely.
#[test]
fn cpu_frame_bytes_rejects_negative_first_plane_linesize() {
  let mut f = frame::Video::empty();
  // SAFETY: f is freshly allocated; we set `format` to NV12 and the
  // first plane's linesize negative (FFmpeg's vertical-flip convention).
  // No backing data buffer is allocated — cpu_frame_bytes must reject
  // before any pointer dereference.
  unsafe {
    let raw = f.as_mut_ptr();
    (*raw).format = ffmpeg_next::ffi::AVPixelFormat::AV_PIX_FMT_NV12 as i32;
    (*raw).width = 1920;
    (*raw).height = 1080;
    (*raw).linesize[0] = -1920;
    (*raw).linesize[1] = -1920;
  }
  assert!(
    cpu_frame_bytes(&f).is_none(),
    "negative linesize must be unsizeable, not Some(0)"
  );
}

/// Build a synthetic `AVHWFramesContext`-backed `AVBufferRef` for
/// tests. The buffer's data is a zeroed `AVHWFramesContext` with only
/// `width` and `height` populated — enough for [`hw_frames_ctx_dimensions`]
/// / [`estimate_transfer_bytes`] to read the allocated dims.
///
/// Returned ref has refcount 1; transfer ownership into
/// `AVFrame.hw_frames_ctx` and let `av_frame_unref` (called by
/// `frame::Video::Drop`) free it via `av_buffer_default_free`.
fn make_hw_frames_ctx_ref(w: i32, h: i32) -> *mut ffmpeg_next::ffi::AVBufferRef {
  use ffmpeg_next::ffi::av_buffer_alloc;
  use std::mem::size_of;

  // SAFETY: `av_buffer_alloc(n)` returns a fresh `AVBufferRef` whose
  // `.data` points to `n` bytes of allocator-supplied storage. We
  // zero the AVHWFramesContext and write only `width` / `height`,
  // which is all the helpers we test read.
  unsafe {
    let buf = av_buffer_alloc(size_of::<AVHWFramesContext>());
    assert!(!buf.is_null(), "av_buffer_alloc returned NULL");
    let data = (*buf).data as *mut AVHWFramesContext;
    std::ptr::write_bytes(data, 0, 1);
    (*data).width = w;
    (*data).height = h;
    buf
  }
}

/// Sanity-check the positive path with a real allocation: an
/// `av_buffer_alloc`'d 4096-byte plane attached as `buf[0]` must
/// surface as `Some(4096)`.
#[test]
fn cpu_frame_bytes_sums_buf_sizes() {
  use ffmpeg_next::ffi::av_buffer_alloc;

  let mut f = frame::Video::empty();
  // SAFETY: av_buffer_alloc returns a fresh AVBufferRef. Attaching it
  // to AVFrame.buf[0] transfers ownership to the frame; av_frame_unref
  // on Drop releases it.
  let buf0 = unsafe { av_buffer_alloc(4096) };
  let buf1 = unsafe { av_buffer_alloc(2048) };
  assert!(!buf0.is_null() && !buf1.is_null());
  unsafe {
    let raw = f.as_mut_ptr();
    (*raw).buf[0] = buf0;
    (*raw).buf[1] = buf1;
    // Positive linesize so the negative-stride rejection doesn't fire.
    (*raw).linesize[0] = 256;
  }
  assert_eq!(cpu_frame_bytes(&f), Some(4096 + 2048));
}

/// A frame with no populated `buf` entries — the empty-frame state
/// `Frame::empty()` produces — must return `Some(0)`. (Pre-fix this
/// case was sized via the linesize×plane_height table; the new
/// `buf[i].size` accounting handles it without a special branch.)
#[test]
fn cpu_frame_bytes_zero_for_empty_frame() {
  let f = frame::Video::empty();
  assert_eq!(cpu_frame_bytes(&f), Some(0));
}

/// `cpu_frame_bytes` must size against the underlying
/// `AVBufferRef.size`, not `linesize × plane_height_for(AVFrame.height)`.
/// On a cropped or heavily aligned stream the underlying buffer can
/// be far larger than `AVFrame.height` (display) suggests — a
/// height-based formula under-counts the allocation by
/// `allocated_height / display_height` and lets the real
/// allocation slip past `max_probe_pending_bytes`.
///
/// Build a 256-byte buffer, attach it as `buf[0]`, but set
/// `AVFrame.height` to 1 to simulate a cropped display. The
/// `buf[i].size` accounting must report 256, not `linesize * 1`.
#[test]
fn cpu_frame_bytes_uses_buf_size_independent_of_display_height() {
  use ffmpeg_next::ffi::av_buffer_alloc;

  let buf0 = unsafe { av_buffer_alloc(256) };
  assert!(!buf0.is_null());

  let mut f = frame::Video::empty();
  unsafe {
    let raw = f.as_mut_ptr();
    (*raw).format = ffmpeg_next::ffi::AVPixelFormat::AV_PIX_FMT_NV12 as i32;
    // Display dims tiny — pre-fix would have used `height = 1` to
    // size the plane and reported `linesize * 1` ≪ 256.
    (*raw).width = 1;
    (*raw).height = 1;
    (*raw).linesize[0] = 32;
    (*raw).buf[0] = buf0;
  }
  assert_eq!(
    cpu_frame_bytes(&f),
    Some(256),
    "cropped/aligned frames must be sized by buf[i].size, not display dims"
  );
}

/// `estimate_transfer_bytes` must read `hw_frames_ctx.width / .height`
/// (allocated dims) — not `AVFrame.width / .height` (display dims).
/// Verify with a synthetic frames context that disagrees with the
/// frame's display dims by 80×.
#[test]
fn estimate_transfer_bytes_reads_alloc_dims_from_hw_frames_ctx() {
  let buf = make_hw_frames_ctx_ref(8192, 8192);
  let mut f = frame::Video::empty();
  unsafe {
    let raw = f.as_mut_ptr();
    // Display dims: 100×100 — pre-fix the estimate was 80 KB. After
    // the fix it must be 8192×8192×8 = 512 MiB.
    (*raw).width = 100;
    (*raw).height = 100;
    (*raw).hw_frames_ctx = buf;
  }
  assert_eq!(
    estimate_transfer_bytes(&f),
    Some(8192usize * 8192 * WORST_CASE_BYTES_PER_PIXEL),
  );
}

/// A frame with no `hw_frames_ctx` cannot have its allocation extent
/// proved — the helper returns `None` so the probe-replay caller
/// fails the candidate rather than under-counting from display dims.
/// (This is the exact attack the cap is meant to prevent.)
#[test]
fn estimate_transfer_bytes_returns_none_without_hw_frames_ctx() {
  let mut f = frame::Video::empty();
  unsafe {
    let raw = f.as_mut_ptr();
    (*raw).width = 1920;
    (*raw).height = 1080;
    // hw_frames_ctx stays null.
  }
  assert!(estimate_transfer_bytes(&f).is_none());
}

/// Non-positive `hw_frames_ctx` dimensions also surface as `None` —
/// a corrupt or malformed HW pool descriptor must not get a free
/// pass.
#[test]
fn estimate_transfer_bytes_rejects_non_positive_alloc_dimensions() {
  let mut f = frame::Video::empty();
  let buf = make_hw_frames_ctx_ref(0, 1080);
  unsafe {
    (*f.as_mut_ptr()).hw_frames_ctx = buf;
  }
  assert!(estimate_transfer_bytes(&f).is_none());
}

/// 8K HDR P010 has actual ~96 MiB resident size; the estimate should
/// over-charge it (the right side to err on for a memory cap) while
/// still fitting within the configurable
/// [`DEFAULT_MAX_PROBE_PENDING_BYTES`] cap (256 MiB) for a single
/// frame so a default-configured decoder is not forced to reject 8K
/// streams outright.
#[test]
fn estimate_transfer_bytes_8k_fits_default_cap() {
  let buf = make_hw_frames_ctx_ref(7680, 4320);
  let mut f = frame::Video::empty();
  unsafe {
    (*f.as_mut_ptr()).hw_frames_ctx = buf;
  }
  let estimate = estimate_transfer_bytes(&f).expect("8K is sizable");
  assert!(
    estimate <= DEFAULT_MAX_PROBE_PENDING_BYTES,
    "8K estimate {estimate} must fit DEFAULT_MAX_PROBE_PENDING_BYTES \
     {DEFAULT_MAX_PROBE_PENDING_BYTES}; otherwise the default cap rejects \
     even a single 8K frame at probe time"
  );
  assert!(
    estimate > 96 * 1024 * 1024,
    "estimate must over-charge real 8K P010 to bound the worst case; got {estimate}"
  );
}

/// `PartialBuildState`'s `Drop` must be a no-op when both pointers are
/// null — the disarmed-by-`into_owned` post-state. A panic / double-free
/// here would break the success path of every `build_state` call.
#[test]
fn partial_build_state_drop_is_no_op_on_null_pointers() {
  let _g = PartialBuildState {
    hw_device_ref: ptr::null_mut(),
    callback_state: ptr::null_mut(),
  };
  // Drops at end of scope. Test passes if it doesn't panic / crash.
}

/// `into_owned` must return the original pointers and disarm the guard
/// (so the guard's Drop becomes a no-op and the caller can safely
/// transfer ownership to `DecoderState` without double-freeing).
#[test]
fn partial_build_state_into_owned_disarms_and_returns_originals() {
  use ffmpeg_next::ffi::{av_buffer_alloc, av_buffer_unref};

  // SAFETY: av_buffer_alloc returns a fresh AVBufferRef* with refcount
  // 1, or NULL on OOM. We free it ourselves below (after into_owned
  // disarms the guard).
  let hw_ptr = unsafe { av_buffer_alloc(64) };
  assert!(!hw_ptr.is_null(), "av_buffer_alloc(64) returned NULL");
  let cb_ptr = Box::into_raw(Box::new(CallbackState::new(u64::MAX)));

  let g = PartialBuildState {
    hw_device_ref: hw_ptr,
    callback_state: cb_ptr,
  };
  let (hw_back, cb_back) = g.into_owned();
  assert_eq!(
    hw_back, hw_ptr,
    "into_owned must return the original device ref"
  );
  assert_eq!(
    cb_back, cb_ptr,
    "into_owned must return the original callback box"
  );

  // Guard is now disarmed (its Drop ran with null pointers as soon as
  // into_owned consumed it). We own the pointers and must free them.
  // SAFETY: hw_ptr and cb_ptr are still the freshly-allocated values.
  unsafe {
    let mut hw = hw_back;
    av_buffer_unref(&mut hw);
    drop(Box::from_raw(cb_back));
  }
}

/// `send_packet` must never leave a probe alive past a packet its history
/// does not hold. The wrong order is `state.inner.send_packet → cap check
/// → keep auditioning` — the packet is in FFmpeg's state but missing from
/// `buffered_packets`, so a later runtime exhaustion would surface
/// `unconsumed_packets` without it and a non-seekable caller could not
/// rebuild the input stream.
///
/// At the byte ceiling the packet cannot be recorded, so it is the trial's
/// last: sent with the history whole, and the candidate that takes it is
/// committed at once, the probe — and any exhaustion that could lose the
/// packet — going with it. Refused, the candidate fails and the next is
/// tried with the history whole (the software-backed laws
/// `the_ceiling_commits_the_candidate_once_it_takes_the_packet_past_it` and
/// `a_candidate_that_refuses_the_packet_past_the_ceiling_fails_and_the_next_takes_the_whole_history`
/// run both roads on any machine).
///
/// `pending_frames` are preserved across the commit — they belong to the
/// active backend (possibly a candidate `advance_probe` just committed) and
/// the caller drains them via `receive_frame`.
///
/// Live HW required: a real `VideoDecoder` is the only way to
/// construct a valid `DecoderState` (its `Drop` invokes FFmpeg
/// cleanup).
#[test]
#[ignore = "requires HWDECODE_SAMPLE_VIDEO and a working hardware backend"]
fn a_packet_past_the_byte_ceiling_commits_the_candidate_that_takes_it() {
  use ffmpeg_next::{format, media};

  let path = std::env::var_os("HWDECODE_SAMPLE_VIDEO")
    .expect("HWDECODE_SAMPLE_VIDEO must be set for this test");

  ffmpeg_next::init().expect("ffmpeg init");
  let mut input = format::input(&path).expect("open input");
  let stream_index = input
    .streams()
    .best(media::Type::Video)
    .expect("video stream")
    .index();
  let stream_params = input
    .streams()
    .best(media::Type::Video)
    .expect("video stream")
    .parameters();

  let mut decoder = VideoDecoder::open(stream_params).expect("open decoder");
  assert!(
    decoder.probe.is_some(),
    "probe must be active immediately after open"
  );

  // Inject sentinel frames as if `advance_probe` had drained them from
  // a freshly-committed candidate during this same send_packet call.
  decoder.pending_frames.push_back(frame::Video::empty());
  decoder.pending_frames.push_back(frame::Video::empty());
  let pending_before = decoder.pending_frames.len();

  // Pre-stage one buffered packet, and push the byte counter to its
  // ceiling so the very next send_packet is past it.
  let pre_existing = Packet::new(8);
  decoder
    .probe
    .as_mut()
    .expect("probe present")
    .buffered_packets
    .push(pre_existing);
  decoder
    .probe
    .as_mut()
    .expect("probe present")
    .buffered_bytes = MAX_PROBE_PACKET_BYTES;

  // Find the first video packet and feed it: the candidate takes it, and
  // is committed with it.
  let mut taken = false;
  for (s, packet) in input.packets() {
    if s.index() != stream_index {
      continue;
    }
    // SAFETY: `packets()` reads each packet with `av_read_frame`, so libavformat wrote it and
    // its padding, and nothing writes through it afterwards.
    match unsafe { decoder.send_av_packet(&packet) } {
      Ok(Sent::Accepted) => {
        taken = true;
        break;
      }
      other => panic!("the candidate takes the packet past the ceiling, got {other:?}"),
    }
  }
  assert!(taken, "expected a video packet to send past the ceiling");

  assert!(
    decoder.probe.is_none(),
    "the candidate that took the packet past the ceiling is committed"
  );
  assert_eq!(
    decoder.pending_frames.len(),
    pending_before,
    "pending_frames belong to the active backend; the commit must not drop them"
  );
}

/// When `advance_probe` exhausts the probe (no more candidates and
/// the active backend just failed), the `Err(AllBackendsFailed
/// { unconsumed_packets, .. })` it returns must include the
/// packets the decoder has already consumed from the caller's
/// demuxer. For non-seekable inputs (live streams, pipes, network
/// sources), losing those packets means the caller's software
/// fallback cannot replay the initial bytes and silently drops
/// the leading frames.
///
/// Live HW required: we need a real `VideoDecoder` (its `Drop` runs
/// FFmpeg cleanup) and `advance_probe` is private — only callable
/// from the same module.
#[test]
#[ignore = "requires HWDECODE_SAMPLE_VIDEO and a working hardware backend"]
fn all_backends_failed_returns_buffered_packets_to_caller() {
  use ffmpeg_next::{format, media};

  let path = std::env::var_os("HWDECODE_SAMPLE_VIDEO")
    .expect("HWDECODE_SAMPLE_VIDEO must be set for this test");

  ffmpeg_next::init().expect("ffmpeg init");
  let input = format::input(&path).expect("open input");
  let stream_params = input
    .streams()
    .best(media::Type::Video)
    .expect("video stream")
    .parameters();

  let mut decoder = VideoDecoder::open(stream_params).expect("open decoder");
  assert!(
    decoder.probe.is_some(),
    "probe must be active immediately after open"
  );

  // Stuff the probe history with two distinct packets and clear the
  // remaining_backends list so the next advance_probe call is forced
  // into the exhaustion branch.
  let p1 = Packet::new(16);
  let p2 = Packet::new(32);
  {
    let probe = decoder.probe.as_mut().expect("probe");
    probe.buffered_packets.push(p1);
    probe.buffered_packets.push(p2);
    probe.remaining_backends.clear();
  }

  // Trigger advance_probe directly with a synthetic non-transient
  // error. The exhaustion branch must take ownership of the
  // buffered packets and surface them via `unconsumed_packets`.
  let result = decoder.advance_probe(Error::Ffmpeg(ffmpeg_next::Error::InvalidData));
  match result {
    Err(Error::AllBackendsFailed(p)) => {
      let attempts = p.attempts();
      let unconsumed_packets = p.unconsumed_packets();
      assert_eq!(
        unconsumed_packets.len(),
        2,
        "buffered probe packets must be returned to the caller for SW fallback"
      );
      assert_eq!(unconsumed_packets[0].size(), 16);
      assert_eq!(unconsumed_packets[1].size(), 32);
      // The synthetic InvalidData was recorded against the active
      // backend before the exhaustion check, so attempts is non-empty.
      assert!(
        !attempts.is_empty(),
        "the active backend's failure should be in attempts"
      );
    }
    other => panic!("expected AllBackendsFailed, got {other:?}"),
  }
}

/// `ProbeState.attempts` must carry forward `open`'s accumulated
/// failures from earlier backends in probe order. The wrong
/// shape — initialising `ProbeState.attempts` to `Vec::new()` at
/// the start of `open`'s "promote to runtime" step — drops
/// earlier failures so a runtime exhaustion surfaces an
/// `AllBackendsFailed` whose `attempts` log only mentions the
/// active backend's failure (e.g. VAAPI's earlier open failure
/// goes missing).
///
/// `open` seeds `ProbeState.attempts` with the local `attempts`
/// vec via `mem::take`, so a runtime exhaustion surfaces the
/// full failure chain in probe order.
///
/// Live HW required: opens a real decoder, manually injects a
/// synthetic earlier-backend failure into `probe.attempts` (as if
/// `open` had recorded one), then triggers exhaustion via
/// `advance_probe`. The synthetic earlier failure must appear
/// before the active backend's failure in the returned `attempts`.
#[test]
#[ignore = "requires HWDECODE_SAMPLE_VIDEO and a working hardware backend"]
fn all_backends_failed_preserves_earlier_open_failures() {
  use ffmpeg_next::{format, media};

  let path = std::env::var_os("HWDECODE_SAMPLE_VIDEO")
    .expect("HWDECODE_SAMPLE_VIDEO must be set for this test");

  ffmpeg_next::init().expect("ffmpeg init");
  let input = format::input(&path).expect("open input");
  let stream_params = input
    .streams()
    .best(media::Type::Video)
    .expect("video stream")
    .parameters();

  let mut decoder = VideoDecoder::open(stream_params).expect("open decoder");
  let active_backend = decoder.backend();

  // Pick a Backend distinct from the active one to simulate a prior
  // open failure that `open`'s seeding would have captured. We use
  // `BackendUnsupportedByCodec` as the synthetic earlier error since
  // it doesn't depend on FFmpeg state.
  //
  // Choose any Backend that isn't the active one. On macOS the only
  // backend is VideoToolbox, so we use a non-Apple backend
  // (Vaapi/Cuda/D3d11va) — its "supported by codec" status is
  // irrelevant; we're injecting the synthetic failure directly.
  let earlier_backend = match active_backend {
    Backend::VideoToolbox => Backend::Vaapi,
    Backend::Vaapi => Backend::Cuda,
    Backend::Cuda => Backend::Vaapi,
    Backend::D3d11va => Backend::Cuda,
  };
  let synthetic_earlier = Error::BackendUnsupportedByCodec(earlier_backend);

  // Seed attempts as `open` would have if backend 0 failed before
  // the active backend opened.
  {
    let probe = decoder.probe.as_mut().expect("probe present");
    probe
      .attempts
      .push((earlier_backend, Box::new(synthetic_earlier)));
    probe.remaining_backends.clear(); // force exhaustion on next advance.
  }

  let result = decoder.advance_probe(Error::Ffmpeg(ffmpeg_next::Error::InvalidData));
  match result {
    Err(Error::AllBackendsFailed(p)) => {
      let attempts = p.attempts();
      assert_eq!(
        attempts.len(),
        2,
        "AllBackendsFailed must surface BOTH the seeded earlier failure \
         and the active backend's runtime failure"
      );
      assert_eq!(
        attempts[0].0, earlier_backend,
        "earlier open failure must come first in probe order"
      );
      assert!(
        matches!(*attempts[0].1, Error::BackendUnsupportedByCodec(_)),
        "earlier failure must preserve its original error variant"
      );
      assert_eq!(
        attempts[1].0, active_backend,
        "active backend's runtime failure must come second"
      );
      assert!(
        matches!(
          *attempts[1].1,
          Error::Ffmpeg(ffmpeg_next::Error::InvalidData)
        ),
        "active backend's failure must preserve the synthetic InvalidData"
      );
    }
    other => panic!("expected AllBackendsFailed, got {other:?}"),
  }
}

/// Two dimension vocabularies, and the judges that must read the right
/// one.
///
/// `AVFrame.width`/`.height` are the **display** dims; what gets
/// allocated is the **coded** extent (software) or the frames-context
/// pool (hardware). On a cropped stream those diverge without limit —
/// measured on this build, an h264 stream carrying SPS cropping shows
/// 32x32 display over a 1920x1088 coded surface, a 2040x gap.
#[test]
fn the_transfer_judge_prices_the_pool_not_the_display() {
  use super::judge_hw_transfer;
  use crate::FrameLimits;

  // A frame whose display extent is tiny and whose pool is not. The
  // pool is what `av_hwframe_transfer_data` sizes from, so a judge
  // reading `AVFrame.width` would price 100x100 = 10,000 pixels and
  // wave through an 8192x8192 allocation — the shape this crate's own
  // `hw_frames_ctx_dimensions` was written for, and which the first cut
  // of this judge reached past it to reproduce.
  let frame = hw_frame_with_pool(100, 100, 8192, 8192);

  // Priced at the pool, an 8192x8192 surface is at least 64 Mpx, so a
  // 16 MiB ceiling cannot hold it however cheap the format.
  let refusal = unsafe {
    judge_hw_transfer(
      frame.as_ptr(),
      FrameLimits::new().with_max_frame_bytes(16 * 1024 * 1024),
    )
  };
  let err = refusal.expect_err("an 8192x8192 pool must not pass a 16 MiB ceiling");
  assert!(
    err.bytes() > 16 * 1024 * 1024,
    "the refusal reports the pool's cost, got {}",
    err.bytes(),
  );

  // Priced at the display extent it would have been 10,000 pixels —
  // comfortably inside — so this is the assertion that separates the
  // two readings rather than merely observing a refusal.
  assert!(
    err.bytes() > 100 * 100 * 16,
    "the judge is still reading the display dims",
  );

  // And a ceiling that genuinely covers the pool admits it.
  assert!(
    unsafe {
      judge_hw_transfer(
        frame.as_ptr(),
        FrameLimits::new().with_max_frame_bytes(usize::MAX),
      )
    }
    .is_ok(),
  );
}

#[test]
fn a_hardware_frame_with_an_unreadable_pool_fails_closed() {
  use super::judge_hw_transfer;
  use crate::FrameLimits;

  // A frames context whose dimensions do not read. The transfer may
  // still allocate and nothing here can say how much — an unprovable
  // extent is not a small one.
  let frame = hw_frame_with_pool(100, 100, 0, 0);
  assert!(
    unsafe { judge_hw_transfer(frame.as_ptr(), FrameLimits::new()) }.is_err(),
    "an unreadable pool extent must fail closed",
  );

  // But a frame that is not a hardware frame at all allocates nothing —
  // `av_hwframe_transfer_data` refuses it outright — so it is passed
  // through rather than given a ceiling's name for a different fault.
  let plain = ffmpeg_next::frame::Video::empty();
  assert!(unsafe { judge_hw_transfer(plain.as_ptr(), FrameLimits::new()) }.is_ok());
}

/// Builds a frame with a display extent of `dw x dh` and a hardware
/// frames context whose pool is `pw x ph`.
fn hw_frame_with_pool(dw: i32, dh: i32, pw: i32, ph: i32) -> ffmpeg_next::frame::Video {
  use ffmpeg_next::ffi;
  let mut frame = ffmpeg_next::frame::Video::empty();
  // SAFETY: a VideoToolbox device is created only to own a frames
  // context; every field written below is a plain integer or a buffer
  // reference FFmpeg allocated, and the frame owns the reference on
  // return.
  unsafe {
    let mut dev: *mut ffi::AVBufferRef = core::ptr::null_mut();
    let rc = ffi::av_hwdevice_ctx_create(
      &mut dev,
      ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_VIDEOTOOLBOX,
      core::ptr::null(),
      core::ptr::null_mut(),
      0,
    );
    assert_eq!(rc, 0, "videotoolbox device");
    let ctx_ref = ffi::av_hwframe_ctx_alloc(dev);
    assert!(!ctx_ref.is_null());
    let ctx = (*ctx_ref).data as *mut ffi::AVHWFramesContext;
    (*ctx).format = ffi::AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX;
    (*ctx).sw_format = ffi::AVPixelFormat::AV_PIX_FMT_NV12;
    (*ctx).width = pw;
    (*ctx).height = ph;

    let p = frame.as_mut_ptr();
    (*p).width = dw;
    (*p).height = dh;
    (*p).format = ffi::AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX as i32;
    (*p).hw_frames_ctx = ctx_ref;
    ffi::av_buffer_unref(&mut dev);
  }
  frame
}

#[test]
fn a_side_data_allocation_failure_is_reported_not_absorbed() {
  use mediaio_ffmpeg::test_support::{
    cap_ffmpeg_allocations, in_subprocess, uncap_ffmpeg_allocations,
  };

  // A partial frame is worse than no frame. The HDR mastering
  // metadata, the ICC profile and the display matrix all ride here, and
  // a picture that comes back with its colours or its orientation
  // quietly missing is one nothing downstream can question. This used
  // to `break` and return `Ok(())`.
  in_subprocess(
    "decoder::tests::a_side_data_allocation_failure_is_reported_not_absorbed",
    || {
      ffmpeg_next::init().expect("ffmpeg init");
      let mut src = ffmpeg_next::frame::Video::empty();
      let mut dst = ffmpeg_next::frame::Video::empty();
      // SAFETY: both frames own live `AVFrame`s; the side-data entry is
      // allocated by FFmpeg's own helper and checked.
      unsafe {
        let sp = src.as_mut_ptr();
        (*sp).width = 16;
        (*sp).height = 16;
        (*sp).format = ffmpeg_next::ffi::AVPixelFormat::AV_PIX_FMT_NV12 as i32;
        let sd = ffmpeg_next::ffi::av_frame_new_side_data(
          sp,
          ffmpeg_next::ffi::AVFrameSideDataType::AV_FRAME_DATA_DISPLAYMATRIX,
          36,
        );
        assert!(!sd.is_null(), "the source entry must exist to be copied");
      }

      // Uncapped, the copy carries the entry across.
      // SAFETY: both pointers are live for the call.
      unsafe { super::copy_frame_props_minimal(dst.as_mut_ptr(), src.as_ptr()) }
        .expect("an uncapped copy succeeds");
      // SAFETY: `dst` is live; `nb_side_data` is a plain field.
      assert_eq!(unsafe { (*dst.as_ptr()).nb_side_data }, 1);

      // Capped, `av_frame_new_side_data` returns null and the failure
      // has to reach the caller — which unrefs the partial destination
      // and advances the backend.
      let mut starved = ffmpeg_next::frame::Video::empty();
      // SAFETY: in `in_subprocess`'s child, alone. The capped window runs only
      // `copy_frame_props_minimal`, which checks the side-data allocation it
      // makes.
      unsafe { cap_ffmpeg_allocations(1) };
      // SAFETY: both pointers are live for the call.
      let refused = unsafe { super::copy_frame_props_minimal(starved.as_mut_ptr(), src.as_ptr()) };
      uncap_ffmpeg_allocations();
      assert!(
        refused.is_err(),
        "a side-data allocation failure must not publish a partial frame",
      );
    },
  );
}

/// The coded-surface refusal has to survive on **every** hardware exit,
/// not only the probe road.
///
/// A `get_format` callback cannot return a reason — declining is
/// `AV_PIX_FMT_NONE`, which libavcodec reports as `Invalid data found
/// when processing input` — so it leaves the refusal in its own state
/// and the exits read it back. `advance_probe` did; the open-time
/// failure path and the post-commit classifier did not, and on those
/// roads the caller got FFmpeg's misnomer for a refusal this crate had
/// made. `open_as` also frees the state on its way out, so the reason
/// had to be read before the guard ran.
///
/// # Per-road coverage, and why it is per-road
///
/// | road | exit | covered by |
/// |---|---|---|
/// | probe | `advance_probe` | `a_cropped_stream_is_judged_on_what_it_allocates_not_what_it_displays` |
/// | explicit backend | post-commit classifier / drained EOF | `every_hardware_exit_names_the_coded_surface_refusal` |
/// | open time | `open_as` failure, codec-type mismatch | this lane's reader contract; not reachable end-to-end here, since `get_format` fires at first decode for the codecs on this platform |
///
/// **The lesson, recorded where it was learned.** R14 reported an exits
/// map with four consumers of the declination. Production had exactly
/// one: the other three were written and then lost when the surrounding
/// code was restructured, and every gate passed anyway — because the
/// tests checked the *helper*, which was correct, rather than the
/// *roads*, which were not.
///
/// So the reader's contract is pinned here, and each reachable road has
/// a lane that drives it end to end and asserts the payload arrives.
/// A helper with no caller is not a fix; a road with no lane is not
/// covered.
#[test]
fn the_declination_reader_reports_then_clears() {
  use super::ceiling_declination_of;
  use crate::ffi::CallbackState;

  let mut quiet = CallbackState::new(u64::MAX);
  quiet.set_wanted(ffmpeg_next::ffi::AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX);
  // Nothing recorded: nothing to report, so the exits fall through to
  // whatever error they already had.
  //
  // SAFETY: a reference to a live local `CallbackState`.
  assert!(unsafe { ceiling_declination_of(&quiet) }.is_none());

  // A recorded refusal comes back named, carrying the pool's extent and
  // the ceiling it was refused against — latched exactly as the
  // `get_format` callback latches it.
  let mut declined = CallbackState::new(u64::MAX);
  declined.set_wanted(ffmpeg_next::ffi::AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX);
  // SAFETY: a pointer to a live local `CallbackState`.
  unsafe { crate::ffi::declare_ceiling_declined_for_test(&mut declined, 1920 * 1088, 1_048_576) };
  // SAFETY: a reference to a live local `CallbackState`.
  match unsafe { ceiling_declination_of(&declined) } {
    Some(Error::HwSurfaceTooLarge(p)) => {
      assert_eq!(p.bytes(), 1920 * 1088);
      assert_eq!(p.limit(), 1_048_576);
    }
    other => panic!("expected a named coded-surface refusal, got {other:?}"),
  }

  // **And it clears.** The state outlives one candidate, so a refusal
  // left set would be reported again against the next backend — which
  // would be a lie about a decoder that never declined anything.
  assert!(
    // SAFETY: a reference to a live local `CallbackState`.
    unsafe { ceiling_declination_of(&declined) }.is_none(),
    "the refusal must be consumed, not latched",
  );
}

/// The transfer judge must fold **every** candidate, priceable or not.
///
/// FFmpeg picks the destination format from the list; this crate does
/// not. So a list holding one cheap priceable format beside one the
/// build cannot price must be judged at the expensive bound — the fold
/// used to skip unpriceable members entirely and reach for a fallback
/// only when *nothing* priced, so a mixed list was judged at the cheap
/// price while FFmpeg stayed free to select the member that was ignored.
///
/// Driven at the pricing level rather than through a real transfer:
/// `av_hwframe_transfer_get_formats` returns whatever the driver
/// offers, and this build's VideoToolbox offers only priceable
/// layouts — so a genuinely mixed list is not reachable here. What is
/// pinned is the rule the fold now applies to each member.
#[test]
fn an_unpriceable_candidate_is_charged_the_conservative_bound() {
  ffmpeg_next::init().expect("ffmpeg init");
  use crate::footprint::{video_frame_bytes, video_frame_bytes_upper_bound};

  const NV12: i32 = ffmpeg_next::ffi::AVPixelFormat::AV_PIX_FMT_NV12 as i32;
  let (w, h) = (1920, 1088);

  let cheap = video_frame_bytes(NV12, w, h).expect("NV12 prices");
  let bound = video_frame_bytes_upper_bound(w, h).expect("a picture");

  // The two really are far apart, so folding one in place of the other
  // is not a rounding difference: NV12 is 1.5 bytes a pixel and the
  // bound charges the widest layout the build can emit.
  assert!(
    bound > cheap * 5,
    "the bound {bound} should dwarf the cheap candidate {cheap}",
  );

  // A hardware-only format cannot be priced, so a fold that skipped it
  // would have taken `cheap` for a list containing both — and the
  // maximum of the two is what the judge must now use.
  const VT: i32 = ffmpeg_next::ffi::AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX as i32;
  assert!(
    video_frame_bytes(VT, w, h).is_none(),
    "the harness needs a genuinely unpriceable candidate",
  );
  let folded = [NV12, VT]
    .iter()
    .map(|&f| {
      video_frame_bytes(f, w, h)
        .or_else(|| video_frame_bytes_upper_bound(w, h))
        .expect("every candidate must fold")
    })
    .max()
    .expect("a non-empty list");
  assert_eq!(
    folded, bound,
    "a mixed list must be judged at the unpriceable member's bound, not the cheap one",
  );
  assert_ne!(
    folded, cheap,
    "the old fold would have taken the cheap figure"
  );
}

/// The hardware-pool judge's boundaries.
///
/// Both are about what happens when the pool will not describe itself,
/// which is the case the old code answered with codec-aligned
/// dimensions — a fallback its own comment admitted could be *smaller*
/// than the pool, because D3D11 HEVC and AV1 round both dimensions to
/// 128 while the codec may round to less.
#[test]
fn the_pool_judge_fails_closed_and_prices_conservatively() {
  ffmpeg_next::init().expect("ffmpeg init");
  use crate::footprint::{video_frame_bytes, video_frame_bytes_upper_bound};

  // **(a) A query that cannot answer refuses.** Exercised through the
  // rule rather than by forcing `avcodec_get_hw_frames_parameters` to
  // fail — the judge answers `Some((u64::MAX, budget))` for every road
  // where the pool declines, which is a refusal against any budget a
  // caller can express.
  assert!(
    u64::MAX > u64::from(u32::MAX),
    "the refusal must exceed any real budget"
  );

  // **(b) An unpriceable declared layout is charged a bound that
  // dominates, not a bare multiply.** The old fallback was
  // `w * h * 16`, which omits the dimension alignment and the per-plane
  // slack every accurate estimate carries — so the "conservative" path
  // could price *below* the accurate one. For a 65x65 surface the bare
  // multiply says 67,600 while the real worst layout aligns to 128x128.
  for (w, h) in [(65, 65), (129, 129), (1920, 1088), (65_536, 1)] {
    let bound = video_frame_bytes_upper_bound(w, h).expect("a picture");
    let bare = (w as usize) * (h as usize) * 16;
    assert!(
      bound >= bare,
      "{w}x{h}: bound {bound} below the bare multiply {bare} it replaces",
    );
    // And it dominates the accurate price of every layout the build can
    // actually size at that extent, which is what makes it usable as a
    // stand-in for one it cannot.
    const NV12: i32 = ffmpeg_next::ffi::AVPixelFormat::AV_PIX_FMT_NV12 as i32;
    const RGBAF32: i32 = ffmpeg_next::ffi::AVPixelFormat::AV_PIX_FMT_RGBAF32LE as i32;
    for fmt in [NV12, RGBAF32] {
      if let Some(priced) = video_frame_bytes(fmt, w, h) {
        assert!(
          bound >= priced,
          "{w}x{h}: bound {bound} below {fmt} at {priced}"
        );
      }
    }
  }
}

/// **The two errno gates disagree about `AVERROR_EOF`, and that
/// disagreement is the design — now read against the session's phase.**
///
/// `avcodec_receive_frame` answers `EOF` for *"the stream is over"* — a
/// protocol state. `avcodec_send_packet` answers it for *"you already
/// told me the stream is over, and you sent something anyway"* — a
/// caller usage fault. The old `is_transient` predicate collapsed the
/// two, which is why the split had to happen before either gate could
/// exist.
///
/// What the phase adds is the second axis: the same errno also means
/// different things depending on where the session is, and every road
/// that answered that question for itself answered it differently.
/// This walks the whole table so no cell is a matter of opinion.
#[test]
fn the_gates_read_every_errno_against_every_phase() {
  use SessionPhase::{Auditioning, AuditioningPastEnd, Draining, Streaming};

  let eagain = || {
    Error::Ffmpeg(ffmpeg_next::Error::Other {
      errno: ffmpeg_next::error::EAGAIN,
    })
  };
  let eof = || Error::Ffmpeg(ffmpeg_next::Error::Eof);

  // --- back pressure, on the drain road ------------------------------
  // Where more input can come, it is the instruction it looks like.
  for phase in [Streaming, Auditioning] {
    assert_eq!(
      receive_status(eagain(), phase).expect("back pressure"),
      Received::NeedsInput,
      "{phase:?}",
    );
  }
  // Past a recorded end on a committed backend there is nothing to send
  // and the send gates refuse, so the instruction would be unsatisfiable
  // — it is the end instead.
  assert_eq!(
    receive_status(eagain(), Draining).expect("a settled end"),
    Received::Ended,
  );
  // **The arm that had no name.** A candidate handed the whole history
  // including the end, still answering "nothing yet", has produced no
  // frame and never will. Not the caller's problem to feed and not this
  // backend's stream to end: it goes back for the probe road.
  assert!(
    matches!(
      receive_status(eagain(), AuditioningPastEnd),
      Err(Error::Ffmpeg(_))
    ),
    "a candidate past the end that produced nothing must reach the probe road, \
     not be answered as a protocol state",
  );

  // --- the end, on the drain road ------------------------------------
  // Only a committed backend speaks for the stream.
  for phase in [Streaming, Draining] {
    assert_eq!(
      receive_status(eof(), phase).expect("the end of a stream"),
      Received::Ended,
      "{phase:?}",
    );
  }
  // A candidate's exhaustion is its own — it never proved it could
  // decode this content, so the probe must try the next backend.
  for phase in [Auditioning, AuditioningPastEnd] {
    assert!(
      matches!(receive_status(eof(), phase), Err(Error::Ffmpeg(_))),
      "{phase:?}: a candidate draining to EOF is a candidate failing",
    );
  }

  // --- the send road -------------------------------------------------
  // Back pressure is a promise that draining makes the offer acceptable.
  for phase in [Streaming, Auditioning] {
    assert_eq!(
      send_status(eagain(), phase).expect("back pressure"),
      Sent::MustDrain,
      "{phase:?}",
    );
  }
  // Past the end it is a promise nothing can keep, so it is not made.
  for phase in [Draining, AuditioningPastEnd] {
    assert!(
      matches!(send_status(eagain(), phase), Err(Error::Ffmpeg(_))),
      "{phase:?}: back pressure must not be promised past a recorded end",
    );
  }
  // And a send after the end stays a fault in every phase.
  for phase in [Streaming, Draining, Auditioning, AuditioningPastEnd] {
    assert!(
      matches!(
        send_status(eof(), phase),
        Err(Error::Ffmpeg(ffmpeg_next::Error::Eof))
      ),
      "{phase:?}: a send after end-of-stream must stay a fault, unlaundered",
    );
  }

  // --- and a genuine failure passes through both, in every phase -----
  for phase in [Streaming, Draining, Auditioning, AuditioningPastEnd] {
    for gate in [
      send_status(Error::Ffmpeg(ffmpeg_next::Error::InvalidData), phase).err(),
      receive_status(Error::Ffmpeg(ffmpeg_next::Error::InvalidData), phase).err(),
    ] {
      assert!(
        matches!(gate, Some(Error::Ffmpeg(ffmpeg_next::Error::InvalidData))),
        "{phase:?}",
      );
    }
  }
}

/// **Regression: the raw hardware decoder's own send arms, driven
/// against real libavcodec.**
///
/// [`VideoDecoder`] is a public, hardware-only face — a caller can hold
/// one directly, without the software-falling-back wrapper — and its
/// send arms classify libavcodec's flow control themselves. Everything
/// that reaches them through `open` needs a live GPU and a sample file,
/// so every existing lane over them is `#[ignore]`d and runs nowhere;
/// the wrapper's lanes short-circuit at its own `eof_sent` gate and
/// never arrive. That left this arm's classification asserted by
/// reading rather than by running, which is how the two roads were free
/// to drift apart.
///
/// [`VideoDecoder::from_software_for_test`] closes that by swapping only
/// the backend: the arms, the probe state and the funnels are exactly
/// what production builds. What is exercised below is libavcodec's real
/// `AVERROR_EOF` travelling the real arm.
///
/// The property: **a submission after end-of-stream is a fault.** Never
/// [`Sent::MustDrain`], which would send a caller into a drain loop
/// whose next offer can never succeed, and never [`Sent::Accepted`],
/// which would silently drop the submission.
#[test]
fn the_raw_decoder_refuses_a_send_after_end_of_stream() {
  ffmpeg_next::init().expect("ffmpeg init");
  let mut parameters = ffmpeg_next::codec::Parameters::new();
  // SAFETY: `parameters` owns a live, zeroed `AVCodecParameters`; both
  // fields are plain scalars and MPEG-4 Part 2 opens with no extradata.
  unsafe {
    let raw = parameters.as_mut_ptr();
    (*raw).codec_type = ffmpeg_next::ffi::AVMediaType::AVMEDIA_TYPE_VIDEO;
    (*raw).codec_id = ffmpeg_next::ffi::AVCodecID::AV_CODEC_ID_MPEG4;
  }
  let mut dec = VideoDecoder::from_software_for_test(
    parameters,
    crate::limits::DecoderLimits::default(),
    /*auditioning=*/ false,
  )
  .expect("a software-backed raw decoder");

  // **The receive arm's own classification, first.** Its `EAGAIN` guard
  // is narrower than the send road's — this road hands `AVERROR_EOF` to
  // the probe machinery, because a candidate that drains to EOF without
  // a frame is a candidate failing rather than a stream ending — so only
  // back pressure short-circuits here, and it is [`receive_status`] that
  // says what back pressure is called. A decoder that has been fed
  // nothing is exactly that state.
  let mut frame = crate::Frame::empty().expect("frame slot");
  assert_eq!(
    dec
      .receive_frame(&mut frame)
      .expect("an empty decoder is not a fault"),
    Received::NeedsInput,
  );

  // The end, taken on a decoder that has been fed nothing — the shortest
  // road to a committed EOF that involves no codec-specific decoding.
  assert_eq!(dec.send_eof().expect("no fault"), Sent::Accepted);

  // Drain to the settled end, so nothing below is confused with back
  // pressure from undrained output.
  loop {
    match dec
      .receive_frame(&mut frame)
      .expect("no fault while draining")
    {
      Received::Produced => {}
      Received::NeedsInput => panic!("a raw decoder at EOF asked for input"),
      Received::Ended => break,
    }
  }

  // The two roads a caller can take past the end, twice each — the
  // answer is a state of the session, not a one-shot complaint.
  for _ in 0..2 {
    let packet = crate::boundary::try_packet_copy(&[0u8; 16]).expect("a submittable packet");
    // SAFETY: `try_packet_copy` wrote every byte of the payload and zeroed the padding, and
    // nothing writes through the packet afterwards.
    let sent = unsafe { dec.send_av_packet(&packet) };
    assert!(
      matches!(sent, Err(Error::Ffmpeg(ffmpeg_next::Error::Eof))),
      "a packet after end-of-stream must be libavcodec's refusal, not back \
       pressure and not silent acceptance; got {sent:?}",
    );
    let eof_again = dec.send_eof();
    assert!(
      matches!(eof_again, Err(Error::Ffmpeg(ffmpeg_next::Error::Eof))),
      "a repeated end-of-stream must be the same refusal; got {eof_again:?}",
    );
  }

  // And the end is still the end: the refused submissions changed
  // nothing about what the decoder has left to give.
  assert_eq!(
    dec
      .receive_frame(&mut frame)
      .expect("no fault past the end"),
    Received::Ended,
  );
}

/// Builds a decoder with the probe window open and no backend left to
/// advance to, so "the probe road was taken" is observable as
/// [`Error::AllBackendsFailed`].
#[cfg(test)]
fn auditioning_decoder() -> VideoDecoder {
  ffmpeg_next::init().expect("ffmpeg init");
  let mut parameters = ffmpeg_next::codec::Parameters::new();
  // SAFETY: `parameters` owns a live, zeroed `AVCodecParameters`; both
  // fields are plain scalars and MPEG-4 Part 2 opens with no extradata.
  unsafe {
    let raw = parameters.as_mut_ptr();
    (*raw).codec_type = ffmpeg_next::ffi::AVMediaType::AVMEDIA_TYPE_VIDEO;
    (*raw).codec_id = ffmpeg_next::ffi::AVCodecID::AV_CODEC_ID_MPEG4;
  }
  VideoDecoder::from_software_for_test(
    parameters,
    crate::limits::DecoderLimits::default(),
    /*auditioning=*/ true,
  )
  .expect("a software-backed candidate on trial")
}

/// **Regression: a candidate that has been handed the end and still asks
/// for input is a candidate failing — never `NeedsInput`, never `Ended`.**
///
/// The arm that had no name until [`SessionPhase`] gave it one. The
/// probe replays its buffered history *and* the recorded end into each
/// candidate, so a candidate can be sitting on a stream that is already
/// over. Asked for a frame it answers `EAGAIN`, and every reading of
/// that was wrong: `NeedsInput` asks the caller for input it does not
/// have and the send gates refuse, while `Ended` takes a broken drain —
/// libavcodec answers a drained decoder `AVERROR_EOF` — for a finished
/// one, and stops the probe from trying the next backend.
///
/// It is the probe's business, and the probe says so: with no backend
/// left to advance to, [`Error::AllBackendsFailed`] — the caller's cue
/// to fall back to software, which is exactly what should happen when
/// no hardware backend could decode this stream.
///
/// `AVERROR_EOF` there is the other answer, and the opposite verdict: a
/// clean, empty completion that commits the candidate
/// (`a_clean_end_completes_the_candidate_whatever_it_was_fed`).
///
/// The decoder is software-backed so the lane actually runs; what is
/// under test is the classification and the road it selects, neither of
/// which is backend-specific.
#[test]
fn a_candidate_that_asks_for_input_past_the_end_fails_the_probe() {
  let mut dec = auditioning_decoder();
  assert_eq!(dec.phase(), SessionPhase::Auditioning);

  // **The latch is set directly, and the reason matters.** Reaching
  // this phase through `send_eof` would also hand the flush packet to
  // libavcodec, which then answers `AVERROR_EOF` — a different arm,
  // the clean end. The arm under test is the *other* one: a candidate
  // that has been replayed a history and an end and still answers
  // `EAGAIN`, which is precisely the state `advance_probe` leaves a
  // candidate in mid-replay. The phase is the subject here, so the
  // phase is what is constructed.
  dec.eof_sent = true;
  assert_eq!(
    dec.phase(),
    SessionPhase::AuditioningPastEnd,
    "a recorded end must move a session on trial into the phase that names it",
  );

  // A candidate with nothing in it answers `EAGAIN`.
  let mut frame = crate::Frame::empty().expect("frame slot");
  match dec.receive_frame(&mut frame) {
    Ok(Received::NeedsInput) => panic!(
      "asked the caller for input on a stream that is already over — and the \
       send gates refuse, so nothing can satisfy it",
    ),
    Ok(Received::Ended) => panic!(
      "took a candidate that broke the drain for one that finished it, \
       stopping the probe from trying the next backend",
    ),
    Ok(Received::Produced) => panic!("a candidate fed only the end produced a frame"),
    Err(Error::AllBackendsFailed(_)) => {}
    Err(other) => panic!("expected the probe road, got {other:?}"),
  }

  // **And then the session really has ended**, which is the other half
  // of the answer being honest. The probe is spent — every backend was
  // tried — so there is no candidate left to fail and the phase says so.
  // A caller that reaches here has been told to fall back to software
  // and can still drain what it holds.
  assert_eq!(
    dec.phase(),
    SessionPhase::Draining,
    "an exhausted probe leaves a committed session, not a candidate",
  );
  assert_eq!(
    dec
      .receive_frame(&mut frame)
      .expect("no fault past the end"),
    Received::Ended,
  );
}

/// **The latched-refusal variant: the funnel is no longer bypassed.**
///
/// A `get_format` declination or an allocator-judge refusal is left in
/// the callback state for a funnel to collect. The `EAGAIN`
/// short-circuit used to return before `hw_exit` ran, so on this road
/// the reason died unread and the candidate was recorded as having
/// failed with a bare errno — this crate's own refusal, lost.
///
/// Every road out of the receive arm passes the funnel now. Here the
/// refusal reaches the probe's attempt log, which is what a caller
/// reads to find out why no backend worked.
#[test]
fn a_latched_refusal_reaches_the_probe_instead_of_dying_unread() {
  let mut dec = auditioning_decoder();
  // The same phase, constructed the same way and for the same reason —
  // see the lane above.
  dec.eof_sent = true;

  // Latch a coded-surface refusal, exactly as the `get_format` callback
  // does when it declines a surface over the ceiling.
  // SAFETY: `dec` owns this state and frees it only when it drops, after this
  // call.
  unsafe {
    crate::ffi::declare_ceiling_declined_for_test(dec.state.callback_state, 8_294_400, 2_073_600)
  };

  let mut frame = crate::Frame::empty().expect("frame slot");
  let Err(Error::AllBackendsFailed(p)) = dec.receive_frame(&mut frame) else {
    panic!("the candidate must fail the probe");
  };
  let reason = format!("{:?}", p.attempts());
  assert!(
    reason.contains("HwSurfaceTooLarge"),
    "the latched refusal must be what the attempt log records, not a bare errno: {reason}",
  );
}

/// **Regression: a latched refusal outranks the errno on the committed
/// road too — every phase, both flow signals.**
///
/// The funnels exist because a `get_format` declination cannot be
/// returned from the callback: it is left in the callback state for a
/// funnel to collect, and libavcodec meanwhile reports whatever it saw.
/// Classify that report first and the caller is told the stream ended,
/// or that more input is wanted, for a frame **this crate declined** —
/// the refusal dies unread and the ceiling looks like it did nothing.
///
/// The committed road lost that ordering in the phase restructure: it
/// classified the raw errno and returned before `hw_exit` ever ran. The
/// four cells below are the whole of the exposed surface — the two
/// committed phases against the two flow signals — and each is built so
/// the errno and the latched refusal disagree, which is the only
/// arrangement where the ordering is observable at all.
#[test]
fn a_latched_refusal_outranks_the_errno_in_every_committed_phase() {
  // (name, how the cell reaches its errno, expected phase)
  let cells: [(&str, fn(&mut VideoDecoder), SessionPhase); 4] = [
    // Nothing sent: libavcodec wants input.
    ("streaming x EAGAIN", |_| {}, SessionPhase::Streaming),
    // The flush packet goes to libavcodec but the session's own latch
    // stays clear, so the phase is still `Streaming` while the errno is
    // the end. Reaching in is the point: these two facts are separate,
    // and the lane is about what happens when they disagree.
    (
      "streaming x EOF",
      |dec: &mut VideoDecoder| {
        dec
          .state
          .inner
          .send_eof()
          .expect("the substrate takes the end");
      },
      SessionPhase::Streaming,
    ),
    // The mirror: the session records the end, libavcodec is not told,
    // so the phase is `Draining` while the errno is still back pressure.
    (
      "draining x EAGAIN",
      |dec: &mut VideoDecoder| {
        dec.eof_sent = true;
      },
      SessionPhase::Draining,
    ),
    // Both together, the ordinary tail drain.
    (
      "draining x EOF",
      |dec: &mut VideoDecoder| {
        assert_eq!(dec.send_eof().expect("no fault"), Sent::Accepted);
      },
      SessionPhase::Draining,
    ),
  ];

  for (name, arrange, expected_phase) in cells {
    let mut dec = committed_decoder();
    arrange(&mut dec);
    assert_eq!(dec.phase(), expected_phase, "{name}: the phase under test");

    // The refusal the callback could not return, latched exactly as
    // `get_format` leaves it when it declines a coded surface.
    // SAFETY: `dec` owns this state and frees it only when it drops, after this
    // call.
    unsafe {
      crate::ffi::declare_ceiling_declined_for_test(dec.state.callback_state, 8_294_400, 2_073_600)
    };

    let mut frame = crate::Frame::empty().expect("frame slot");
    match dec.receive_frame(&mut frame) {
      Ok(Received::Ended) => panic!(
        "{name}: reported a clean end over a refusal this crate made — the \
         ceiling declined the surface and the caller was told the stream was over",
      ),
      Ok(Received::NeedsInput) => panic!(
        "{name}: asked for more input over a refusal this crate made — the \
         caller would feed a decoder that already declined the frame",
      ),
      Ok(Received::Produced) => panic!("{name}: a declined surface produced a frame"),
      // **Named, and that is the point.** The refusal is this picture's
      // own error, reported as it was minted: the caller reads it to find
      // out *why*, and "a ceiling you configured" is the one answer it
      // can act on.
      Err(Error::HwSurfaceTooLarge(p)) => {
        assert_eq!(p.bytes(), 8_294_400, "{name}: the refusal's own numbers");
      }
      Err(other) => panic!("{name}: expected the latched refusal, got {other:?}"),
    }

    // Nothing is remembered: the next call reaches libavcodec and answers
    // the phase's own flow state.
    let again = dec.receive_frame(&mut frame);
    assert!(
      matches!(again, Ok(Received::NeedsInput | Received::Ended)),
      "{name}: the next call reaches libavcodec, got {again:?}",
    );
  }
}

/// A committed decoder — probe collapsed, software-backed so the lane
/// runs — for the phases that are only reachable after commit.
#[cfg(test)]
fn committed_decoder() -> VideoDecoder {
  ffmpeg_next::init().expect("ffmpeg init");
  let mut parameters = ffmpeg_next::codec::Parameters::new();
  // SAFETY: `parameters` owns a live, zeroed `AVCodecParameters`; both
  // fields are plain scalars and MPEG-4 Part 2 opens with no extradata.
  unsafe {
    let raw = parameters.as_mut_ptr();
    (*raw).codec_type = ffmpeg_next::ffi::AVMediaType::AVMEDIA_TYPE_VIDEO;
    (*raw).codec_id = ffmpeg_next::ffi::AVCodecID::AV_CODEC_ID_MPEG4;
  }
  VideoDecoder::from_software_for_test(
    parameters,
    crate::limits::DecoderLimits::default(),
    /*auditioning=*/ false,
  )
  .expect("a software-backed committed decoder")
}

/// **The send road's half of the same law** — codex's next-step, and the
/// same bypass in the same shape.
///
/// The raw hardware send arms classified their errno straight out of
/// libavcodec, so a refusal latched during the submission — `get_format`
/// declining a coded surface as the decoder configures on its first
/// packet — was reported as whatever libavcodec said over the top of it.
/// A caller reads "you already sent the end" and never learns the
/// ceiling refused the surface.
///
/// A second `send_eof` is the deterministic way to a flow-control errno
/// on this road: libavcodec answers `AVERROR_EOF` to a repeated flush
/// packet, and the funnel must be what decides what the caller sees.
#[test]
fn a_latched_refusal_outranks_the_errno_on_the_send_road_too() {
  let mut dec = committed_decoder();
  assert_eq!(dec.send_eof().expect("no fault"), Sent::Accepted);

  // SAFETY: `dec` owns this state and frees it only when it drops, after this
  // call.
  unsafe {
    crate::ffi::declare_ceiling_declined_for_test(dec.state.callback_state, 8_294_400, 2_073_600)
  };

  match dec.send_eof() {
    Ok(Sent::Accepted) => panic!("a repeated end silently accepted over a refusal"),
    Ok(Sent::MustDrain) => panic!("back pressure promised over a refusal"),
    // **Named on the send road too.** On a committed backend the
    // refusal is this submission's own error, as on the receive road,
    // carrying its own numbers.
    Err(Error::HwSurfaceTooLarge(p)) => {
      assert_eq!(p.bytes(), 8_294_400, "the refusal's own numbers");
    }
    Err(other) => panic!(
      "the latched refusal must outrank libavcodec's report on the send road too, \
       got {other:?}",
    ),
  }
}

/// **Regression: a verdict is minted once, and the committed route
/// reports the one it was given rather than reaching for another.**
///
/// The committed reading used to call [`VideoDecoder::hw_exit`] itself.
/// That was right while it was the *first* funnel on its road and wrong
/// the moment it was the second: the receive road mints at the top of its
/// arm, and a funnel **consumes** the latch it reads, so the second call
/// found nothing and reported the errno libavcodec had given over a coded
/// surface this crate declined — true about what FFmpeg saw, false about
/// what happened, and not the actionable cause (a configured ceiling) the
/// caller could have acted on.
///
/// [`VideoDecoder::hw_route`] funnels nothing. Both halves are pinned
/// anyway, because either alone would pass for the wrong reason: that the
/// latch survives the route (nothing was consumed), and that what the
/// caller is handed is the verdict the route was given.
#[test]
fn the_committed_route_reports_the_verdict_it_was_given() {
  let dec = committed_decoder();
  // SAFETY: `dec` owns this state and frees it only when it drops, after this
  // call.
  unsafe {
    crate::ffi::declare_ceiling_declined_for_test(dec.state.callback_state, 8_294_400, 2_073_600)
  };

  // Handed a raw failure while a refusal sits latched: the route must
  // report the raw one. Reaching for the latch here is precisely the bug.
  let reported_raw = reported(dec.hw_route(
    Error::Ffmpeg(ffmpeg_next::Error::External),
    BareVerdict::CandidateFailure,
  ));
  assert!(
    matches!(reported_raw, Error::Ffmpeg(ffmpeg_next::Error::External)),
    "it must report the verdict it was handed, not mint a second one: {reported_raw:?}",
  );

  // And the latch is untouched, which is what makes the road's own mint
  // the only one.
  let verdict = dec.hw_exit(Error::Ffmpeg(ffmpeg_next::Error::InvalidData));
  assert!(
    matches!(verdict, Error::HwSurfaceTooLarge(ref q) if q.bytes() == 8_294_400),
    "the latch must survive the committed route: {verdict:?}",
  );

  // Threaded onward, the refusal is what a caller reads.
  let threaded = reported(dec.hw_route(verdict, BareVerdict::CandidateFailure));
  assert!(
    matches!(threaded, Error::HwSurfaceTooLarge(ref p) if p.bytes() == 8_294_400),
    "the threaded verdict must reach the caller with its numbers: {threaded:?}",
  );
}

/// **The mechanism that makes re-derivation lossy, pinned on its own.**
///
/// The invariant on [`software_receive`] rests on one fact about the
/// funnels: they *consume* what they collect, because a refusal reported
/// twice would be a refusal invented once. That is what turns a second
/// funnel call from redundant into destructive, and it is why "mint once
/// and thread" is a rule rather than a preference.
///
/// The hardware errnos themselves need a hardware backend to produce in
/// this harness — a software decoder never answers with them — so the
/// committed receive road is covered by the lanes above and this one
/// rather than end to end, and saying so is better than implying
/// otherwise.
#[test]
fn a_funnel_consumes_what_it_collects() {
  let dec = committed_decoder();
  // SAFETY: `dec` owns this state and frees it only when it drops, after this
  // call.
  unsafe { crate::ffi::declare_ceiling_declined_for_test(dec.state.callback_state, 4_096, 1_024) };

  let first = dec.hw_exit(Error::Ffmpeg(ffmpeg_next::Error::InvalidData));
  assert!(
    matches!(first, Error::HwSurfaceTooLarge(ref p) if p.bytes() == 4_096),
    "the first funnel mints the refusal: {first:?}",
  );

  let second = dec.hw_exit(Error::Ffmpeg(ffmpeg_next::Error::InvalidData));
  assert!(
    matches!(second, Error::Ffmpeg(ffmpeg_next::Error::InvalidData)),
    "the second finds nothing and answers with its fallback — which is why a \
     road that re-funnels reports the substrate's errno over its own refusal: \
     {second:?}",
  );
}

/// **Regression: the HEVC spelling of a declined surface reaches the
/// caller by name.**
///
/// `get_hw_format` runs again on a **post-commit format change** — an
/// HEVC SPS switch mid-stream — so the ceiling can decline a coded
/// surface there, latch [`Error::HwSurfaceTooLarge`], and return
/// `AV_PIX_FMT_NONE`. H.264 normalises that to `InvalidData`. FFmpeg
/// 9.0.1's HEVC path returns the `-1` (`hevc/hevcdec.c` 712–714 and
/// 3260–3264), `ffmpeg-next` maps it to `Other { errno: 1 }`, and that
/// errno says nothing of its own — so a road that reported the errno
/// returned an EPERM-shaped red herring and left the latch standing to be
/// collected by whatever ran next.
///
/// **A latched surface refusal is what the caller reads**, whatever
/// wrapper the codec put around it: on a committed backend it is that
/// picture's own error, carrying its own numbers.
#[test]
fn a_declined_surface_reaches_the_caller_whatever_errno_the_codec_wrapped_it_in() {
  // The HEVC spelling.
  let hevc = ffmpeg_next::Error::Other { errno: 1 };

  let dec = committed_decoder();
  // SAFETY: `dec` owns this state and frees it only when it drops, after this
  // call.
  unsafe {
    crate::ffi::declare_ceiling_declined_for_test(dec.state.callback_state, 8_294_400, 2_073_600)
  };

  let out = reported(dec.hw_failure(hevc, BareVerdict::CandidateFailure));
  assert!(
    matches!(out, Error::HwSurfaceTooLarge(ref p) if p.bytes() == 8_294_400),
    "the refusal's own numbers, not the codec's errno: {out:?}",
  );

  // And the latch is spent, so nothing downstream inherits it.
  let after = dec.hw_exit(Error::Ffmpeg(hevc));
  assert!(
    matches!(after, Error::Ffmpeg(ffmpeg_next::Error::Other { errno: 1 })),
    "the refusal must have been collected, not left standing: {after:?}",
  );
}

/// The other answer the routing must keep giving — **paired with the
/// spellings its real producer uses.**
///
/// `judge_buffer` refuses by answering libavcodec `-EINVAL`, and
/// libavcodec says `InvalidData` for corrupt input on the same road; a
/// failed VideoToolbox restart says `AVERROR_EXTERNAL`. A budget refusal
/// names the action that can succeed — raise the ceiling — and must
/// travel unwrapped whatever errno rides with it.
#[test]
fn a_budget_refusal_travels_unwrapped_whatever_errno_rides_with_it() {
  for (name, raw) in [
    (
      "judge_buffer's own -EINVAL",
      ffmpeg_next::Error::Other {
        errno: libc::EINVAL,
      },
    ),
    ("libavcodec's InvalidData", ffmpeg_next::Error::InvalidData),
    ("a failed restart's EXTERNAL", ffmpeg_next::Error::External),
  ] {
    let dec = committed_decoder();
    // SAFETY: `dec` owns this state and frees it only when it drops, after this
    // call.
    unsafe {
      crate::ffi::declare_frame_budget_declined_for_test(dec.state.callback_state, 12_582_912)
    };

    let out = reported(dec.hw_failure(raw, BareVerdict::CandidateFailure));
    assert!(
      matches!(out, Error::FrameBudgetExceeded(ref p) if p.bytes() == 12_582_912),
      "{name}: a budget refusal must travel unwrapped, naming the action that \
       can succeed — raise the ceiling: {out:?}",
    );
  }
}

/// The same exclusion on the **committed receive route**, where the
/// verdict is minted by `hw_receive` and the routing only reads it.
///
/// The route matters separately from the predicate: it is the one that
/// holds an already-minted verdict, so a regression there could take the
/// form of re-deriving instead of mis-judging. What a caller must see is
/// the budget refusal itself — unwrapped, with its numbers.
#[test]
fn the_committed_receive_route_lets_a_budget_refusal_travel_unwrapped() {
  let mut dec = committed_decoder();
  // SAFETY: `dec` owns this state and frees it only when it drops, after this
  // call.
  unsafe {
    crate::ffi::declare_frame_budget_declined_for_test(dec.state.callback_state, 12_582_912)
  };

  let mut frame = crate::Frame::empty().expect("frame slot");
  match dec.receive_frame(&mut frame) {
    Ok(status) => panic!("a refused frame must not read as a protocol state: {status:?}"),
    Err(Error::FrameBudgetExceeded(p)) => {
      assert_eq!(p.bytes(), 12_582_912, "the refusal's own numbers");
    }
    Err(other) => panic!("expected the budget refusal, got {other:?}"),
  }
}

/// A committed decoder cannot advance a probe it does not have, so every
/// route it takes must be a report. Unwrapping here rather than in each
/// lane keeps the assertions about the verdict.
#[cfg(test)]
#[track_caller]
fn reported(route: HwRoute) -> Error {
  match route {
    HwRoute::Report(err) => err,
    HwRoute::Advance(err) => {
      panic!("a committed decoder has no candidate to advance to, got Advance({err:?})")
    }
  }
}

/// Errnos a committed hardware backend can answer with,
/// `AVERROR_EXTERNAL` and `ENOSYS` among them.
#[cfg(test)]
const COMMITTED_FAILURES: [ffmpeg_next::Error; 9] = [
  ffmpeg_next::Error::External,
  ffmpeg_next::Error::Other {
    errno: libc::ENOSYS,
  },
  ffmpeg_next::Error::InvalidData,
  ffmpeg_next::Error::Unknown,
  ffmpeg_next::Error::Bug,
  ffmpeg_next::Error::Bug2,
  ffmpeg_next::Error::Other {
    errno: libc::EINVAL,
  },
  ffmpeg_next::Error::Other {
    errno: libc::ENOMEM,
  },
  ffmpeg_next::Error::Other { errno: 1 },
];

/// LAW (row 1): **after the first picture a failure is that picture's
/// own error, reported as it was minted, and nothing is remembered — the
/// next packet reaches libavcodec.**
///
/// FFmpeg has no reliable signal that a hardware session is gone. In
/// FFmpeg 9.0.1 `AVERROR_EXTERNAL` also answers one picture
/// (`libavcodec/videotoolbox.c` 126–129 and 556–559), `ENOSYS` is NVDEC's
/// answer to one HEVC picture (`nvdec_hevc.c` 200–227), and a failed
/// VideoToolbox restart (`videotoolbox.c` 1066–1067) is FFmpeg's to try
/// again, once a new parameter set re-arms it (446–450 and 1062–1068) —
/// until then every picture fails the same way (1071–1072). So no errno
/// is read as more than its picture's: each comes back as itself, and the
/// call after it reaches libavcodec. That is all this law pins; whether
/// the session behind the call recovers is FFmpeg's.
///
/// How "reaches libavcodec" is seen: libavcodec is handed the end first,
/// behind the session's back, so its own answer to any packet is
/// `AVERROR_EOF` — a word only libavcodec says here. A session that
/// remembered the failure would answer with the failure instead.
///
/// PLANT: a latch restored — `hw_route` remembering an
/// `AVERROR_EXTERNAL` and `send_packet` refusing with it — turns this red
/// at "the next packet reaches libavcodec".
#[test]
fn a_committed_failure_is_its_pictures_own_and_the_next_packet_reaches_libavcodec() {
  for raw in COMMITTED_FAILURES {
    let mut dec = committed_decoder();
    dec
      .state
      .inner
      .send_eof()
      .expect("libavcodec takes the end");

    let out = reported(dec.hw_failure(raw, BareVerdict::CandidateFailure));
    assert!(
      matches!(out, Error::Ffmpeg(e) if e == raw),
      "{raw:?}: reported as it was minted, got {out:?}",
    );

    let packet = crate::boundary::try_packet_copy(&[0u8; 16]).expect("a submittable packet");
    // SAFETY: `try_packet_copy` wrote every byte of the payload and zeroed the padding, and
    // nothing writes through the packet afterwards.
    let sent = unsafe { dec.send_av_packet(&packet) };
    assert!(
      matches!(sent, Err(Error::Ffmpeg(ffmpeg_next::Error::Eof))),
      "{raw:?}: the next packet reaches libavcodec, which answers with the end it \
       holds — got {sent:?}",
    );
    let mut frame = crate::Frame::empty().expect("frame slot");
    let received = dec.receive_frame(&mut frame);
    assert!(
      matches!(received, Ok(Received::Ended)),
      "{raw:?}: the next picture asked for reaches libavcodec too — got {received:?}",
    );
  }
}

/// LAW (row 1): **after a committed failure and a `flush`, the end
/// reaches libavcodec.** Nothing about the failure outlives a seek in
/// this decoder: after the flush the end is taken, libavcodec holds it —
/// a second end sent straight to it is refused, which only a libavcodec
/// that took the first one does — and the drain answers the end.
///
/// That is all it pins. `flush` rebuilds no hardware session: it reaches
/// only the codec's own flush (`libavcodec/avcodec.c` 417–418 in FFmpeg
/// 9.0.1), so a session FFmpeg left without a decoder stays without one.
///
/// The plant above turns this red at "after a flush the end reaches
/// libavcodec" when the restored latch survives `flush`.
#[test]
fn after_a_committed_failure_and_a_flush_the_end_reaches_libavcodec() {
  for raw in COMMITTED_FAILURES {
    let mut dec = committed_decoder();
    let out = reported(dec.hw_failure(raw, BareVerdict::CandidateFailure));
    assert!(
      matches!(out, Error::Ffmpeg(e) if e == raw),
      "{raw:?}: reported as it was minted, got {out:?}",
    );

    dec.flush();
    let eof = dec.send_eof();
    assert!(
      matches!(eof, Ok(Sent::Accepted)),
      "{raw:?}: after a flush the end reaches libavcodec — it is taken, got {eof:?}",
    );
    assert!(
      matches!(dec.state.inner.send_eof(), Err(ffmpeg_next::Error::Eof)),
      "{raw:?}: the end reached libavcodec, which refuses a second one",
    );
    let mut frame = crate::Frame::empty().expect("frame slot");
    assert_eq!(
      dec
        .receive_frame(&mut frame)
        .expect("a drained decoder answers the end"),
      Received::Ended,
      "{raw:?}",
    );
  }
}

/// LAW (row 2): **while a candidate is on trial, every failure still
/// advances the probe.** The probe is the only reader of a failure:
/// before the first picture, a failed restart and a picture's
/// `AVERROR_INVALIDDATA` alike mean this candidate cannot be trusted with
/// the stream.
#[test]
fn the_probe_advances_on_every_failure_as_before() {
  for failure in [
    ffmpeg_next::Error::External,
    ffmpeg_next::Error::Other {
      errno: libc::ENOSYS,
    },
    ffmpeg_next::Error::InvalidData,
    ffmpeg_next::Error::Unknown,
  ] {
    let dec = auditioning_decoder();
    match dec.hw_failure(failure, BareVerdict::CandidateFailure) {
      HwRoute::Advance(Error::Ffmpeg(e)) => assert_eq!(e, failure),
      HwRoute::Advance(other) => panic!("{failure:?}: advanced with {other:?}"),
      HwRoute::Report(err) => {
        panic!("{failure:?}: a candidate's failure must advance the probe, got Report({err:?})")
      }
    }
  }
}

/// **The send road's cross product: both refusals, both flow signals,
/// both phases, both send faces.**
///
/// The transient send arms returned their funnel's result the instant
/// they had it. For a flow signal that is right; for anything else it
/// was a dead end. `hw_send` can mint [`Error::HwSurfaceTooLarge`], and a
/// minted refusal returned plain went nowhere: a probe still auditioning
/// never advanced past the candidate that had just declined the surface.
///
/// Sixteen cells, and the two refusals must part company in every one:
/// a **surface** refusal routes — the probe advances while auditioning,
/// and once committed it is the submission's own error — while a
/// **budget** refusal exits direct and unwrapped, because software would
/// meet the same ceiling.
#[test]
fn the_send_roads_route_a_surface_refusal_and_report_a_budget_one() {
  #[derive(Clone, Copy)]
  enum Latch {
    Surface,
    Budget,
  }
  #[derive(Clone, Copy)]
  enum Face {
    Packet,
    Eof,
  }

  for latch in [Latch::Surface, Latch::Budget] {
    for auditioning in [true, false] {
      for face in [Face::Packet, Face::Eof] {
        let mut dec = if auditioning {
          auditioning_decoder()
        } else {
          committed_decoder()
        };
        // Reach a flow-signal errno on the send road: libavcodec answers
        // `AVERROR_EOF` to a submission that follows a flush packet.
        assert_eq!(dec.send_eof().expect("no fault"), Sent::Accepted);

        match latch {
          // SAFETY: `dec` owns this state and frees it only when it drops, after this
          // call.
          Latch::Surface => unsafe {
            crate::ffi::declare_ceiling_declined_for_test(
              dec.state.callback_state,
              8_294_400,
              2_073_600,
            )
          },
          Latch::Budget => {
            // SAFETY: `dec` owns this state and frees it only when it drops, after this
            // call.
            unsafe {
              crate::ffi::declare_frame_budget_declined_for_test(
                dec.state.callback_state,
                12_582_912,
              )
            }
          }
        }

        let packet = crate::boundary::try_packet_copy(&[0u8; 16]).expect("packet");
        let out = match face {
          // SAFETY: `try_packet_copy` wrote every byte of the payload and zeroed the padding,
          // and nothing writes through the packet afterwards.
          Face::Packet => unsafe { dec.send_av_packet(&packet) },
          Face::Eof => dec.send_eof(),
        };
        let name = format!(
          "{} x {} x {}",
          match latch {
            Latch::Surface => "surface",
            Latch::Budget => "budget",
          },
          if auditioning {
            "auditioning"
          } else {
            "committed"
          },
          match face {
            Face::Packet => "send_packet",
            Face::Eof => "send_eof",
          },
        );

        match (latch, out) {
          // A declined surface is this backend's refusal to take the
          // stream. Auditioning, the probe advances and — with no
          // backend left — surfaces the exhaustion, the cause in its
          // attempt log.
          (Latch::Surface, Err(Error::AllBackendsFailed(p))) if auditioning => {
            let cause = format!("{:?}", p.attempts());
            assert!(
              cause.contains("HwSurfaceTooLarge") && cause.contains("8294400"),
              "{name}: the refusal must reach the attempt log with its numbers: {cause}",
            );
          }
          // Committed, the submission's own error, with its numbers.
          (Latch::Surface, Err(Error::HwSurfaceTooLarge(p))) if !auditioning => {
            assert_eq!(p.bytes(), 8_294_400, "{name}: the refusal's own numbers");
          }
          (Latch::Surface, other) => panic!(
            "{name}: a declined surface must advance a running probe and otherwise \
             reach the caller by name: {other:?}",
          ),
          // A budget refusal names the action that can succeed. Wrapping
          // it would send the caller to a decoder the same ceiling will
          // refuse.
          (Latch::Budget, Err(Error::FrameBudgetExceeded(p))) => {
            assert_eq!(p.bytes(), 12_582_912, "{name}: the refusal's own numbers");
          }
          (Latch::Budget, other) => {
            panic!("{name}: a budget refusal must exit direct and unwrapped: {other:?}",)
          }
        }
      }
    }
  }
}

/// **The packet timebase reaches the `AVCodecContext`, and only when
/// one is declared.**
///
/// `AVCodecContext.pkt_timebase` is caller-supplied and libavcodec
/// reads it — its generic subtitle path fills `AVSubtitle.pts` and the
/// packet-duration fallback only when the field is set. Nothing in
/// this crate wrote it, on any road, so this asserts the write at the
/// one choke point every decoder passes through rather than at any one
/// caller.
///
/// The `None` half is the other law: a standalone decoder opened
/// without a declared ruler leaves the field at libavcodec's own
/// default, and a zero-numerator timebase is deliberately treated the
/// same way — unset and `0/1` are the same statement, and writing one
/// would claim a ruler no container declared.
#[test]
fn the_packet_timebase_reaches_the_codec_context() {
  use std::num::NonZeroI32;

  ffmpeg_next::init().expect("ffmpeg init");

  let parameters = || {
    let mut parameters = ffmpeg_next::codec::Parameters::new();
    // SAFETY: `parameters` owns a live, zeroed `AVCodecParameters`;
    // both fields written are plain scalars.
    unsafe {
      let raw = parameters.as_mut_ptr();
      (*raw).codec_type = ffmpeg_next::ffi::AVMediaType::AVMEDIA_TYPE_VIDEO;
      (*raw).codec_id = ffmpeg_next::ffi::AVCodecID::AV_CODEC_ID_H264;
    }
    parameters
  };
  let limits = crate::limits::DecoderLimits::default();
  let read_back = |declared: Option<mediadecode::Timebase>| {
    let (ctx, _state) = build_codec_context(
      &parameters(),
      limits,
      crate::DecoderOptions::new(),
      declared,
    )
    .expect("a context is allocated");
    // SAFETY: `ctx` owns a live `AVCodecContext` for this borrow, and
    // `pkt_timebase` is a public `AVRational` field.
    let raw = unsafe { (*ctx.as_ptr()).pkt_timebase };
    (raw.num, raw.den)
  };

  let milliseconds = mediadecode::Timebase::new(1, NonZeroI32::new(1_000).expect("non-zero"));
  assert_eq!(
    read_back(Some(milliseconds)),
    (1, 1_000),
    "a declared timebase is written verbatim",
  );

  assert_eq!(
    read_back(None),
    (0, 1),
    "and an undeclared one leaves libavcodec's own default alone",
  );

  // `0/1` is what a container that declared no timebase hands this
  // crate. Writing it would be ceremony; the field already says it.
  let unset = mediadecode::Timebase::new(0, NonZeroI32::new(1).expect("non-zero"));
  assert_eq!(read_back(Some(unset)), (0, 1));
}

/// **The decoder's options reach the codec context.** Each of the three
/// `AVDiscard` settings is written as the value it names, every value of
/// each; and the defaults leave libavcodec's own, `AVDISCARD_DEFAULT`.
#[test]
fn every_discard_value_reaches_the_codec_context() {
  ffmpeg_next::init().expect("ffmpeg init");

  let mut parameters = ffmpeg_next::codec::Parameters::new();
  // SAFETY: `parameters` owns a live, zeroed `AVCodecParameters`; both
  // fields written are plain scalars.
  unsafe {
    let raw = parameters.as_mut_ptr();
    (*raw).codec_type = ffmpeg_next::ffi::AVMediaType::AVMEDIA_TYPE_VIDEO;
    (*raw).codec_id = ffmpeg_next::ffi::AVCodecID::AV_CODEC_ID_H264;
  }
  let read_back = |options: crate::DecoderOptions| {
    let (ctx, state) = build_codec_context(
      &parameters,
      crate::limits::DecoderLimits::default(),
      options,
      None,
    )
    .expect("a context is allocated");
    // SAFETY: `ctx` owns a live `AVCodecContext` for this borrow; the
    // three are `AVDiscard` fields, read as the integers they are.
    let raw = unsafe {
      let ctx = ctx.as_ptr();
      (
        ptr::addr_of!((*ctx).skip_frame).cast::<i32>().read(),
        ptr::addr_of!((*ctx).skip_loop_filter).cast::<i32>().read(),
        ptr::addr_of!((*ctx).skip_idct).cast::<i32>().read(),
      )
    };
    drop(ctx);
    drop(state);
    raw
  };
  assert_eq!(read_back(crate::DecoderOptions::new()), (0, 0, 0));
  for value in crate::Discard::ALL {
    let av = value.as_av() as i32;
    let one = crate::DecoderOptions::new();
    assert_eq!(read_back(one.with_skip_frame(value)), (av, 0, 0), "{value}");
    assert_eq!(
      read_back(one.with_skip_loop_filter(value)),
      (0, av, 0),
      "{value}"
    );
    assert_eq!(read_back(one.with_skip_idct(value)), (0, 0, av), "{value}");
  }
}

/// **The open request starts at [`VideoDecoder::open`]'s and sets each
/// optional**, consuming and in place alike, and back to the probe and an
/// untimed decoder.
#[test]
fn the_open_request_starts_at_opens_and_sets_each_optional() {
  use std::num::NonZeroI32;

  let request = VideoDecoderOpen::new();
  assert_eq!(request, VideoDecoderOpen::default());
  assert_eq!(
    (
      request.backend(),
      request.limits(),
      request.timebase(),
      request.options()
    ),
    (
      None,
      crate::limits::DecoderLimits::new(),
      None,
      crate::DecoderOptions::new()
    )
  );

  let limits = crate::limits::DecoderLimits::new().with_max_packet_bytes(12_345);
  let timebase = mediadecode::Timebase::new(1, NonZeroI32::new(1_000).expect("non-zero"));
  let options = crate::DecoderOptions::new().with_skip_frame(crate::Discard::NonReference);
  let built = request
    .with_backend(Some(Backend::VideoToolbox))
    .with_limits(limits)
    .with_timebase(Some(timebase))
    .with_options(options);
  assert_eq!(
    (
      built.backend(),
      built.limits(),
      built.timebase(),
      built.options()
    ),
    (Some(Backend::VideoToolbox), limits, Some(timebase), options)
  );
  let mut set = VideoDecoderOpen::new();
  set
    .set_backend(Some(Backend::VideoToolbox))
    .set_limits(limits)
    .set_timebase(Some(timebase))
    .set_options(options);
  assert_eq!(set, built);
  set.set_backend(None).set_timebase(None);
  assert_eq!(
    set,
    VideoDecoderOpen::new()
      .with_limits(limits)
      .with_options(options)
  );
}

/// Codec parameters naming H.264 video and nothing more: enough for a
/// context to open.
#[cfg(target_vendor = "apple")]
fn h264_parameters() -> codec::Parameters {
  let mut parameters = codec::Parameters::new();
  // SAFETY: `parameters` owns a live, zeroed `AVCodecParameters`; both
  // fields written are plain scalars.
  unsafe {
    let raw = parameters.as_mut_ptr();
    (*raw).codec_type = ffmpeg_next::ffi::AVMediaType::AVMEDIA_TYPE_VIDEO;
    (*raw).codec_id = ffmpeg_next::ffi::AVCodecID::AV_CODEC_ID_H264;
  }
  parameters
}

/// **The probe runs from the open under every skip level, `All` among
/// them**: a level is the codec's to honour — VP9 reads none, and
/// `libdav1d` decodes key pictures under `All` — so no level commits a
/// backend before its answers, and the stream decoder's `Auto` probes
/// hardware under each.
///
/// PLANT: a commit at the open under `All` turns this red at "`all`: the
/// probe runs"; `Auto` opening software under `All`, at "`all`: `Auto`
/// probes hardware".
#[cfg(target_vendor = "apple")]
#[test]
fn the_probe_runs_from_the_open_under_every_skip_level() {
  ffmpeg_next::init().expect("ffmpeg init");
  let timebase = mediadecode::Timebase::new(1, core::num::NonZeroI32::new(25).expect("non-zero"));
  for skip in crate::Discard::ALL {
    let options = crate::DecoderOptions::new().with_skip_frame(skip);
    let opened = VideoDecoder::open_probing(
      h264_parameters(),
      crate::limits::DecoderLimits::default(),
      options,
      None,
      backend::probe_order(),
    )
    .expect("the decoder opens");
    assert_eq!(
      opened.phase(),
      SessionPhase::Auditioning,
      "{skip}: the probe runs"
    );
    assert_eq!(opened.backend(), Backend::VideoToolbox, "{skip}");

    let auto = crate::FfmpegOwnedVideoStreamDecoder::open_from(
      crate::VideoStreamDecoderOpen::new().with_options(options),
      h264_parameters(),
      timebase,
      crate::limits::DecoderLimits::default(),
    )
    .expect("the session opens");
    assert!(!auto.is_software(), "{skip}: `Auto` probes hardware");
  }
}

/// What a decoder opened as, read the way the two laws below read it:
/// whether it probes, its backend and its limits; the packet timebase and
/// the three skips its context holds, as raw integers; and the options it
/// reports.
#[cfg(target_vendor = "apple")]
type Opened = (
  bool,
  Backend,
  crate::limits::DecoderLimits,
  (i32, i32),
  (i32, i32, i32),
  crate::DecoderOptions,
);

#[cfg(target_vendor = "apple")]
fn opened(decoder: Result<VideoDecoder>) -> Opened {
  let decoder = decoder.expect("the decoder opens");
  // SAFETY: `decoder.state.inner` owns a live, opened `AVCodecContext` for
  // this borrow; `pkt_timebase` is a public `AVRational` field, and the
  // three skips are `AVDiscard` fields, read as the integers they are.
  let (timebase, skips) = unsafe {
    let ctx = decoder.state.inner.as_ptr();
    (
      ((*ctx).pkt_timebase.num, (*ctx).pkt_timebase.den),
      (
        ptr::addr_of!((*ctx).skip_frame).cast::<i32>().read(),
        ptr::addr_of!((*ctx).skip_loop_filter).cast::<i32>().read(),
        ptr::addr_of!((*ctx).skip_idct).cast::<i32>().read(),
      ),
    )
  };
  (
    decoder.probe.is_some(),
    decoder.backend(),
    decoder.frame_limits,
    timebase,
    skips,
    decoder.options(),
  )
}

/// **What the open request names reaches the decoder it opens**, on the
/// probe and on a pinned backend, untimed and timed:
///
/// - the options and the packet timebase are written into the context;
/// - the limits are held;
/// - the probe is present exactly when no backend is pinned;
/// - the options are reported.
///
/// On Apple targets, over VideoToolbox. It is the probe's one backend
/// there, and it opens a context without asking for hardware: its device
/// is created with nothing to create (`libavutil/hwcontext_videotoolbox.c`
/// 829–838 in FFmpeg 9.0.1), and the decoder's session waits for the
/// first picture.
#[cfg(target_vendor = "apple")]
#[test]
fn the_open_request_reaches_the_decoder_on_both_roads() {
  use std::num::NonZeroI32;

  ffmpeg_next::init().expect("ffmpeg init");

  // A different value in each skip, so a setting written into another's
  // field is seen.
  let options = crate::DecoderOptions::new()
    .with_skip_frame(crate::Discard::NonReference)
    .with_skip_loop_filter(crate::Discard::All)
    .with_skip_idct(crate::Discard::NonKey);
  let limits = crate::limits::DecoderLimits::new().with_max_packet_bytes(12_345);
  let milliseconds = mediadecode::Timebase::new(1, NonZeroI32::new(1_000).expect("non-zero"));
  for pinned in [None, Some(Backend::VideoToolbox)] {
    for (timebase, written) in [(None, (0, 1)), (Some(milliseconds), (1, 1_000))] {
      let request = VideoDecoderOpen::new()
        .with_backend(pinned)
        .with_limits(limits)
        .with_timebase(timebase)
        .with_options(options);
      assert_eq!(
        opened(VideoDecoder::open_from(request, h264_parameters())),
        (
          pinned.is_none(),
          Backend::VideoToolbox,
          limits,
          written,
          (8, 48, 32),
          options
        ),
        "{request:?}"
      );
    }
  }
}

/// **Each of the eight `open*` constructors opens the request its
/// arguments spell**: the probe or the pinned backend, the limits and the
/// packet timebase it names, and libavcodec's options.
///
/// On Apple targets, over VideoToolbox, for the reason the law above
/// gives.
#[cfg(target_vendor = "apple")]
#[test]
fn each_open_constructor_opens_the_request_it_spells() {
  use std::num::NonZeroI32;

  ffmpeg_next::init().expect("ffmpeg init");

  let parameters = h264_parameters;
  let default = crate::limits::DecoderLimits::default();
  let limits = crate::limits::DecoderLimits::new().with_max_packet_bytes(12_345);
  let milliseconds = mediadecode::Timebase::new(1, NonZeroI32::new(1_000).expect("non-zero"));
  let (untimed, timed) = ((0, 1), (1, 1_000));
  let pinned = Backend::VideoToolbox;
  let cases = [
    (
      "open",
      opened(VideoDecoder::open(parameters())),
      (true, default, untimed),
    ),
    (
      "open_timed",
      opened(VideoDecoder::open_timed(parameters(), milliseconds)),
      (true, default, timed),
    ),
    (
      "open_with_frame_limits",
      opened(VideoDecoder::open_with_frame_limits(parameters(), limits)),
      (true, limits, untimed),
    ),
    (
      "open_with_frame_limits_timed",
      opened(VideoDecoder::open_with_frame_limits_timed(
        parameters(),
        limits,
        milliseconds,
      )),
      (true, limits, timed),
    ),
    (
      "open_with",
      opened(VideoDecoder::open_with(parameters(), pinned)),
      (false, default, untimed),
    ),
    (
      "open_with_timed",
      opened(VideoDecoder::open_with_timed(
        parameters(),
        pinned,
        milliseconds,
      )),
      (false, default, timed),
    ),
    (
      "open_with_limits",
      opened(VideoDecoder::open_with_limits(parameters(), pinned, limits)),
      (false, limits, untimed),
    ),
    (
      "open_with_limits_timed",
      opened(VideoDecoder::open_with_limits_timed(
        parameters(),
        pinned,
        limits,
        milliseconds,
      )),
      (false, limits, timed),
    ),
  ];
  for (name, opened, (probing, limits, timebase)) in cases {
    assert_eq!(
      opened,
      (
        probing,
        pinned,
        limits,
        timebase,
        (0, 0, 0),
        crate::DecoderOptions::new()
      ),
      "{name}"
    );
  }
}

/// **A context this crate builds decodes on one thread.** Nothing here
/// sets `thread_count` or `thread_type`, and FFmpeg's own default is one
/// thread, so the allocator judge and the `get_format` callback run on
/// the decoding thread. The state they share is `Sync` regardless; this
/// pins the fact [`InstalledCallbackState`]'s safety note states, not a
/// premise the callbacks' contract rests on.
#[test]
fn a_context_this_crate_builds_decodes_on_one_thread() {
  ffmpeg_next::init().expect("ffmpeg init");

  let mut parameters = ffmpeg_next::codec::Parameters::new();
  // SAFETY: `parameters` owns a live, zeroed `AVCodecParameters`; both
  // fields written are plain scalars.
  unsafe {
    let raw = parameters.as_mut_ptr();
    (*raw).codec_type = ffmpeg_next::ffi::AVMediaType::AVMEDIA_TYPE_VIDEO;
    (*raw).codec_id = ffmpeg_next::ffi::AVCodecID::AV_CODEC_ID_H264;
  }
  let (ctx, state) = build_codec_context(
    &parameters,
    crate::limits::DecoderLimits::default(),
    crate::DecoderOptions::new(),
    None,
  )
  .expect("a context is allocated");
  // SAFETY: `ctx` owns a live `AVCodecContext` for this borrow, and
  // `thread_count` is a plain integer field.
  let threads = unsafe { (*ctx.as_ptr()).thread_count };
  assert_eq!(threads, 1, "one decoding thread, FFmpeg's default");
  // The context first, then the state it points at.
  drop(ctx);
  drop(state);
}

/// The public standalone decoder can declare a packet timebase.
///
/// Before this, only crate-private constructors could — a direct
/// `VideoDecoder` caller had no way to tell libavcodec what its
/// packets' ticks meant, however well it knew.
#[test]
fn the_public_decoder_has_a_timed_construction_road() {
  fn _timed_roads_exist() {
    let _: fn(ffmpeg_next::codec::Parameters, mediadecode::Timebase) -> Result<VideoDecoder> =
      VideoDecoder::open_timed;
    let _: fn(
      ffmpeg_next::codec::Parameters,
      crate::limits::DecoderLimits,
      mediadecode::Timebase,
    ) -> Result<VideoDecoder> = VideoDecoder::open_with_frame_limits_timed;
    let _: fn(
      ffmpeg_next::codec::Parameters,
      Backend,
      mediadecode::Timebase,
    ) -> Result<VideoDecoder> = VideoDecoder::open_with_timed;
    let _: fn(
      ffmpeg_next::codec::Parameters,
      Backend,
      crate::limits::DecoderLimits,
      mediadecode::Timebase,
    ) -> Result<VideoDecoder> = VideoDecoder::open_with_limits_timed;
  }
}

/// Side of the rawvideo pictures the rescue lanes send, in pixels. The
/// format is `GRAY8`, one byte a pixel.
const RAW_SIDE: i32 = 16;

/// Bytes in one such picture: the payload rawvideo takes as one packet.
const RAW_PICTURE: usize = (RAW_SIDE * RAW_SIDE) as usize;

/// A decoder on trial with no backend left to advance to, over
/// libavcodec's rawvideo decoder.
///
/// Rawvideo takes any payload of a picture's size, so a lane chooses
/// every byte it sends. libavcodec decodes the first packet as it is
/// sent and holds the second until a picture is asked for
/// (`avcodec_send_packet`, `libavcodec/decode.c` 745–758 in FFmpeg
/// 9.0.1): two sends taken, and no back pressure.
fn raw_auditioning_decoder() -> VideoDecoder {
  raw_auditioning_decoder_with(RAW_SIDE, crate::DecoderOptions::new())
}

/// [`raw_auditioning_decoder`], its pictures `side` pixels square and its
/// context opened with `options`.
fn raw_auditioning_decoder_with(side: i32, options: crate::DecoderOptions) -> VideoDecoder {
  ffmpeg_next::init().expect("ffmpeg init");
  let parameters = raw_parameters(side);
  let codec = find_decoder(&parameters).expect("rawvideo");
  VideoDecoder::from_software_for_test_as(
    parameters,
    codec,
    crate::limits::DecoderLimits::default(),
    options,
    /*auditioning=*/ true,
  )
  .expect("a software-backed rawvideo candidate on trial")
}

/// Codec parameters naming rawvideo `GRAY8` pictures `side` pixels square.
fn raw_parameters(side: i32) -> codec::Parameters {
  let mut parameters = ffmpeg_next::codec::Parameters::new();
  // SAFETY: `parameters` owns a live, zeroed `AVCodecParameters`; every
  // field written is a plain scalar.
  unsafe {
    let raw = parameters.as_mut_ptr();
    (*raw).codec_type = ffmpeg_next::ffi::AVMediaType::AVMEDIA_TYPE_VIDEO;
    (*raw).codec_id = ffmpeg_next::ffi::AVCodecID::AV_CODEC_ID_RAWVIDEO;
    (*raw).width = side;
    (*raw).height = side;
    (*raw).format = ffmpeg_next::ffi::AVPixelFormat::AV_PIX_FMT_GRAY8 as i32;
  }
  parameters
}

/// One picture's packet, every byte `fill`.
fn raw_packet(fill: u8) -> Packet {
  crate::boundary::try_packet_copy(&[fill; RAW_PICTURE]).expect("a submittable packet")
}

/// Where a packet's payload is, and how many references its buffer has.
fn payload_of(packet: &Packet) -> (usize, i32) {
  // SAFETY: the packet is live; `data` and `buf` are plain fields, and
  // `av_buffer_get_ref_count` only reads the buffer's atomic count.
  unsafe {
    let raw = packet.as_ptr();
    assert!(!(*raw).buf.is_null(), "the lanes send refcounted packets");
    (
      (*raw).data as usize,
      ffmpeg_next::ffi::av_buffer_get_ref_count((*raw).buf),
    )
  }
}

/// Whether every byte of `packet`'s payload is `fill`.
fn holds(packet: &Packet, fill: u8) -> bool {
  packet
    .data()
    .is_some_and(|bytes| bytes.iter().all(|&byte| byte == fill))
}

/// LAW: **the probe's copy shares nothing with its source, and costs
/// what its source was charged.**
///
/// [`try_clone_packet`] is how the probe records a packet. Its
/// `av_packet_ref` leaves the record sharing the source's payload
/// buffer, so the record is then made writable, which copies the
/// payload into a buffer of its own; the side data is the copy
/// `av_packet_copy_props` makes of each entry. The sizes are the
/// source's, so the charge the pre-flight computes from the source —
/// `size` and [`packet_side_data_bytes`] — is the copy's.
///
/// PLANT: `av_packet_make_writable` skipped in [`try_clone_packet`]
/// turns this red at "a payload of its own".
#[test]
fn the_probes_copy_shares_nothing_with_its_source() {
  use ffmpeg_next::ffi::{AVPacketSideDataType, av_packet_new_side_data};

  ffmpeg_next::init().expect("ffmpeg init");
  let mut source = crate::boundary::try_packet_copy(&[0x5A; 1024]).expect("a source packet");
  // SAFETY: `source` owns a live packet; the entry is FFmpeg's own
  // allocation of 16 bytes, written only through the pointer it returns.
  unsafe {
    let entry = av_packet_new_side_data(
      source.as_mut_ptr(),
      AVPacketSideDataType::AV_PKT_DATA_STRINGS_METADATA,
      16,
    );
    assert!(!entry.is_null(), "the entry must exist to be copied");
    core::ptr::write_bytes(entry, 0xC3, 16);
  }

  // SAFETY: `source` is `try_packet_copy`'s, every byte of its payload written, and its side
  // data is `av_packet_new_side_data`'s, written above.
  let copy = unsafe { crate::boundary::try_clone_packet(&source) }.expect("an uncapped copy");
  let (source_at, source_refs) = payload_of(&source);
  let (copy_at, copy_refs) = payload_of(&copy);
  assert_eq!(copy.data(), source.data(), "the same bytes");
  assert_ne!(copy_at, source_at, "in a payload of its own");
  assert_eq!(
    (source_refs, copy_refs),
    (1, 1),
    "each buffer referenced by its own packet alone",
  );

  // Read as raw fields, never through the bindgen enum the entry also
  // carries.
  let side = |packet: &Packet| -> (usize, Vec<u8>) {
    // SAFETY: the packet is live and carries one entry, checked first;
    // `data` and `size` are plain fields of it.
    unsafe {
      let raw = packet.as_ptr();
      assert_eq!((*raw).side_data_elems, 1, "one entry");
      let entry = (*raw).side_data;
      let (data, size) = ((*entry).data, (*entry).size);
      (
        data as usize,
        std::slice::from_raw_parts(data, size).to_vec(),
      )
    }
  };
  let (source_side_at, source_side) = side(&source);
  let (copy_side_at, copy_side) = side(&copy);
  assert_eq!(copy_side, source_side, "the same side data");
  assert_ne!(copy_side_at, source_side_at, "in an entry of its own");

  let charge = |packet: &Packet| {
    packet.size() + packet_side_data_bytes(packet, MAX_PROBE_PACKET_SIDE_DATA_ENTRIES)
  };
  assert_eq!(
    charge(&copy),
    charge(&source),
    "the copy costs what its source was charged",
  );
}

/// LAW: **a rescued packet keeps the bytes it was sent with, whatever
/// the caller writes to its own packet once the session is over, and a
/// write to it reaches nothing the caller holds.**
///
/// `Packet::data_mut` lends `&mut [u8]` without consulting writability,
/// and [`Error::AllBackendsFailed`] hands the probe's history to the
/// caller as owned packets. A history that shared the caller's payloads
/// would carry the caller's writes after the send into what it hands
/// back, and would give safe code two `&mut [u8]` over one allocation.
/// So the probe records copies, charged what the packets were.
///
/// The probe exhausts on the road a caller meets it on. The first
/// picture asked for is a CPU frame from a context on trial as hardware,
/// so its download fails — `av_hwframe_transfer_data` answers `EINVAL`
/// for a frame with no hardware frames context — and no backend is left
/// to advance to.
///
/// PLANT: `av_packet_make_writable` skipped in [`try_clone_packet`]
/// turns this red at "the bytes it was sent with".
#[test]
fn a_rescued_packet_keeps_the_bytes_it_was_sent_with() {
  let mut dec = raw_auditioning_decoder();
  let fills = [0x11u8, 0x22];
  let mut sent: Vec<Packet> = fills.iter().map(|&fill| raw_packet(fill)).collect();
  for packet in &sent {
    // SAFETY: `raw_packet` is `try_packet_copy`'s, every byte written and the padding zeroed;
    // the caller's writes below wait until the decoder is dropped.
    assert_eq!(
      unsafe { dec.send_av_packet(packet) }.expect("taken"),
      Sent::Accepted
    );
  }
  assert_eq!(
    dec.probe.as_ref().expect("still on trial").buffered_bytes,
    fills.len() * RAW_PICTURE,
    "each copy is charged its payload",
  );

  let mut frame = crate::Frame::empty().expect("frame slot");
  let Err(Error::AllBackendsFailed(p)) = dec.receive_frame(&mut frame) else {
    panic!("a CPU frame on trial fails the only backend, and the probe exhausts");
  };
  let mut rescued = p.into_unconsumed_packets();
  // The session ends here, and libavcodec's own references to what it
  // was sent go with it: only now may the caller write through its
  // packets (`send_av_packet`'s contract). The rescued copies are the
  // caller's to keep past it.
  drop(dec);

  // The caller writes to its own packets after the send, through safe
  // code.
  for packet in &mut sent {
    packet.data_mut().expect("a payload").fill(0xEE);
  }
  assert_eq!(
    rescued.len(),
    fills.len(),
    "every packet the probe took comes back"
  );
  for (packet, &fill) in rescued.iter().zip(&fills) {
    assert!(
      holds(packet, fill),
      "{fill:#04x}: the bytes it was sent with, not the caller's later writes",
    );
    assert_eq!(
      payload_of(packet).1,
      1,
      "{fill:#04x}: referenced by the rescued packet alone",
    );
  }

  // And the other way: a write to a rescued packet reaches nothing the
  // caller holds.
  for packet in &mut rescued {
    packet.data_mut().expect("a payload").fill(0x55);
  }
  for packet in &sent {
    assert!(
      holds(packet, 0xEE),
      "a write to a rescued packet reached the caller's"
    );
  }
}

/// LAW: **a copy the probe cannot make refuses the send by name, before
/// libavcodec sees the packet, and leaves the packet with the caller.**
///
/// The refusal is the probe's own, as at its byte ceiling:
/// [`Error::AllBackendsFailed`] with the history recorded so far and no
/// attempt, since no backend failed. The session stays where it was, on
/// the same backend and no longer on trial.
///
/// The allocator fails it, through `av_max_alloc` in a child process of
/// its own: capped at 512 bytes, `av_packet_ref`'s `AVBufferRef` is
/// still allocated and the payload `av_packet_make_writable` copies into
/// (8,192 bytes and the padding) is not.
///
/// PLANT: `av_packet_make_writable` skipped in [`try_clone_packet`]
/// turns this red at "refused by name".
#[test]
fn a_copy_the_probe_cannot_make_refuses_the_send_by_name() {
  use mediaio_ffmpeg::test_support::{
    cap_ffmpeg_allocations, in_subprocess, uncap_ffmpeg_allocations,
  };

  in_subprocess(
    "decoder::tests::a_copy_the_probe_cannot_make_refuses_the_send_by_name",
    || {
      let mut dec = raw_auditioning_decoder();
      let first = raw_packet(0x11);
      // SAFETY: `raw_packet` is `try_packet_copy`'s, every byte written and the padding zeroed,
      // and nothing writes through it while the decoder may still read it.
      assert_eq!(
        unsafe { dec.send_av_packet(&first) }.expect("taken"),
        Sent::Accepted
      );

      let mut refused = crate::boundary::try_packet_copy(&[0x77; 8192]).expect("a packet");
      // SAFETY: in `in_subprocess`'s child, alone. The capped window runs one send:
      // the probe's copy goes through `try_clone_packet`, which checks both of
      // its calls, and libavcodec checks its own allocations.
      unsafe { cap_ffmpeg_allocations(512) };
      // SAFETY: `try_packet_copy` wrote every byte and zeroed the padding; the probe refuses
      // the packet before libavcodec sees it.
      let answer = unsafe { dec.send_av_packet(&refused) };
      uncap_ffmpeg_allocations();

      let Err(Error::AllBackendsFailed(p)) = answer else {
        panic!("a copy that cannot be made is refused by name, got {answer:?}");
      };
      assert!(
        p.attempts().is_empty(),
        "the probe's own refusal: no backend failed, got {:?}",
        p.attempts(),
      );
      let history = p.into_unconsumed_packets();
      assert!(
        history.len() == 1 && holds(&history[0], 0x11),
        "the history recorded before the refusal, without the refused packet",
      );

      // The packet is the caller's again: neither the copy that failed
      // nor libavcodec holds a reference to it.
      assert_eq!(
        payload_of(&refused).1,
        1,
        "nothing else references the packet"
      );
      assert!(holds(&refused, 0x77), "its bytes untouched");
      refused.data_mut().expect("a payload").fill(0x78);

      // The same backend, no longer on trial; and libavcodec never saw
      // the packet. Offered now it is taken, where a decoder already
      // holding one packet beside a waiting picture answers back
      // pressure (`decode.c` 746–747).
      assert_eq!(dec.phase(), SessionPhase::Streaming);
      assert_eq!(
        // SAFETY: every byte written and the padding zeroed (`try_packet_copy`'s); the caller
        // wrote the payload while no decoder held it — the probe refused it and libavcodec
        // never saw it — and nothing writes it again.
        unsafe { dec.send_av_packet(&refused) }.expect("offered again"),
        Sent::Accepted,
        "libavcodec never saw the refused packet",
      );
    },
  );
}

/// LAW: **a candidate is replayed copies, and the history stays the
/// probe's own.**
///
/// `avcodec_send_packet` takes a reference to what it is sent and may
/// hold it past the call (`libavcodec/decode.c` 748 and 754). A
/// candidate replayed the history's own packets would share their
/// buffers for as long as it holds them, and a candidate the probe
/// commits stays the session's: when it fails in its turn and the
/// history goes to the caller, the caller's `&mut [u8]` would cover
/// bytes a live decoder reads.
///
/// The candidate is opened in software through
/// [`VideoDecoder::advance_probe_with`]. Two packets, so the replay
/// leaves one picture waiting and one packet held, and asks for no
/// drain.
///
/// PLANT: the replay sending the history's own packet turns this red
/// at "the candidate holds none of the history".
#[test]
fn a_candidate_is_replayed_copies_and_the_history_stays_the_probes_own() {
  let mut dec = raw_auditioning_decoder();
  dec
    .probe
    .as_mut()
    .expect("on trial")
    .remaining_backends
    .push(Backend::Cuda);
  let fills = [0x11u8, 0x22];
  let sent: Vec<Packet> = fills.iter().map(|&fill| raw_packet(fill)).collect();
  for packet in &sent {
    // SAFETY: `raw_packet` is `try_packet_copy`'s, every byte written and the padding zeroed,
    // and nothing writes through it while the decoder may still read it.
    assert_eq!(
      unsafe { dec.send_av_packet(packet) }.expect("taken"),
      Sent::Accepted
    );
  }

  dec
    .advance_probe_with(
      Error::Ffmpeg(ffmpeg_next::Error::InvalidData),
      VideoDecoder::software_state_for_test,
    )
    .expect("the candidate takes the replay and is committed");
  assert_eq!(
    dec.backend(),
    Backend::Cuda,
    "the candidate is the session's"
  );
  for (packet, &fill) in dec
    .probe
    .as_ref()
    .expect("still on trial: no picture yet")
    .buffered_packets
    .iter()
    .zip(&fills)
  {
    assert_eq!(
      payload_of(packet).1,
      1,
      "{fill:#04x}: the candidate holds none of the history",
    );
  }

  // The candidate fails in its turn, on the first picture asked for, and
  // the history goes to the caller while the candidate stays the
  // session's, holding what it was replayed.
  let mut frame = crate::Frame::empty().expect("frame slot");
  let Err(Error::AllBackendsFailed(p)) = dec.receive_frame(&mut frame) else {
    panic!("the candidate fails on a CPU frame, and the probe exhausts");
  };
  assert_eq!(
    p.attempts().len(),
    2,
    "the first backend's failure and the candidate's: {:?}",
    p.attempts(),
  );
  assert_eq!(
    p.unconsumed_packets().len(),
    fills.len(),
    "every packet the probe took comes back"
  );
  for (packet, &fill) in p.unconsumed_packets().iter().zip(&fills) {
    assert!(
      holds(packet, fill),
      "{fill:#04x}: the bytes it was sent with"
    );
    assert_eq!(
      payload_of(packet).1,
      1,
      "{fill:#04x}: referenced by the rescued packet alone, beside a live candidate",
    );
  }
  assert_eq!(
    dec.backend(),
    Backend::Cuda,
    "and that candidate is still live"
  );
}

/// One picture's packet, every byte `fill`, flagged `AV_PKT_FLAG_DISCARD`:
/// edit-list pre-roll, which libavcodec decodes and whose frame it drops.
fn discarded(fill: u8) -> Packet {
  let mut packet = raw_packet(fill);
  // SAFETY: `packet` owns a live `AVPacket`; `flags` is a plain `c_int`.
  unsafe { (*packet.as_mut_ptr()).flags |= ffmpeg_next::ffi::AV_PKT_FLAG_DISCARD };
  packet
}

/// Sends every one of `packets` to `dec`, each taken: a pre-roll packet's
/// frame is dropped as it is decoded, so none waits to be drained.
fn send_all(dec: &mut VideoDecoder, packets: &[Packet]) {
  for (index, packet) in packets.iter().enumerate() {
    // SAFETY: `raw_packet`'s, every byte written and the padding zeroed;
    // `packets` outlives every read of it and nothing writes through it.
    let sent = unsafe { dec.send_av_packet(packet) };
    assert!(
      matches!(sent, Ok(Sent::Accepted)),
      "packet {index} of {}: {sent:?}",
      packets.len()
    );
  }
}

/// LAW: **a clean end completes the candidate, whatever it was fed** —
/// nothing at all, edit-list pre-roll alone, or a picture that decodes to
/// no output. The candidate took the end and drained to libavcodec's own
/// `AVERROR_EOF` with no refusal latched: a successful empty completion,
/// never the candidate failing, and the stream ends on it.
///
/// PLANT: routing a clean end past the end to the candidate failing again
/// turns this red at every feed.
#[test]
fn a_clean_end_completes_the_candidate_whatever_it_was_fed() {
  let fed: [(&str, VideoDecoder, Vec<Packet>); 3] = [
    ("nothing", auditioning_decoder(), Vec::new()),
    ("pre-roll", raw_auditioning_decoder(), vec![discarded(0x11)]),
    (
      "a hidden VP9 frame",
      auditioning_by_name("vp9", AVCodecID::AV_CODEC_ID_VP9, crate::Discard::Default),
      vec![coded(&hidden(VP9_KEY_FRAME))],
    ),
  ];
  for (what, mut dec, packets) in fed {
    send_all(&mut dec, &packets);
    assert_eq!(
      dec.send_eof().expect("the end taken"),
      Sent::Accepted,
      "{what}"
    );
    assert_eq!(dec.phase(), SessionPhase::AuditioningPastEnd, "{what}");
    let mut frame = crate::Frame::empty().expect("frame slot");
    assert_eq!(
      dec
        .receive_frame(&mut frame)
        .expect("a clean end, not a backend's failure"),
      Received::Ended,
      "{what}"
    );
    assert_eq!(
      dec.phase(),
      SessionPhase::Draining,
      "{what}: the candidate is committed"
    );
  }
}

/// LAW: **an end after nothing but edit-list pre-roll completes the
/// candidate at every skip level**, within the probe's 256 packets and
/// past them.
///
/// libavcodec decodes a packet flagged `AV_PKT_FLAG_DISCARD` and drops its
/// frame (`decode.c` 464–466), so a candidate fed nothing else reaches a
/// clean end with no picture having done all it was asked. Within the
/// ceiling the end completes it; past it, the 257th packet does, taken.
#[test]
fn an_end_after_pre_roll_alone_completes_the_candidate_at_every_skip_level() {
  for skip in crate::Discard::ALL {
    for count in [120, 300] {
      let mut dec =
        raw_auditioning_decoder_with(RAW_SIDE, crate::DecoderOptions::new().with_skip_frame(skip));
      let packets: Vec<Packet> = (0..count).map(|index| discarded(index as u8)).collect();
      send_all(&mut dec, &packets);
      assert_eq!(
        dec.probe.as_ref().map(|probe| probe.buffered_packets.len()),
        (count <= MAX_PROBE_PACKETS).then_some(count),
        "{skip}, {count}: on trial with every packet held, or committed at the ceiling"
      );
      assert_eq!(dec.send_eof().expect("the end taken"), Sent::Accepted);
      let mut frame = crate::Frame::empty().expect("frame slot");
      assert_eq!(
        dec
          .receive_frame(&mut frame)
          .expect("the stream's end, not a backend's failure"),
        Received::Ended,
        "{skip}, {count}"
      );
      assert!(
        dec.probe.is_none(),
        "{skip}, {count}: the candidate is committed"
      );
    }
  }
}

/// LAW: **at the probe's ceiling the candidate is committed once it takes
/// the packet past it** — in packets and in bytes — with no failure claimed
/// for a backend that has failed nothing; and the picture of that packet is
/// the committed candidate's.
///
/// PLANT: the ceiling answering `AllBackendsFailed` before the packet is
/// sent, as before this round, turns this red at "taken, and committed".
#[test]
fn the_ceiling_commits_the_candidate_once_it_takes_the_packet_past_it() {
  // In packets: 256 of pre-roll held, then a picture.
  let mut dec = raw_auditioning_decoder();
  let preroll: Vec<Packet> = (0..MAX_PROBE_PACKETS)
    .map(|index| discarded(index as u8))
    .collect();
  send_all(&mut dec, &preroll);
  assert_eq!(
    dec.probe.as_ref().map(|probe| probe.buffered_packets.len()),
    Some(MAX_PROBE_PACKETS),
    "on trial at the ceiling"
  );
  let past = raw_packet(0x5A);
  // SAFETY: `raw_packet`'s, every byte written and the padding zeroed, and
  // nothing writes through it while the decoder may still read it.
  let sent = unsafe { dec.send_av_packet(&past) };
  assert!(
    matches!(sent, Ok(Sent::Accepted)),
    "taken, and committed: {sent:?}"
  );
  assert_eq!(dec.phase(), SessionPhase::Streaming);
  assert!(
    holds_a_picture(&mut dec),
    "its picture, the committed candidate's"
  );

  // In bytes: a history whose bytes the next packet would carry past
  // 64 MiB.
  let mut dec = raw_auditioning_decoder();
  {
    let probe = dec.probe.as_mut().expect("on trial");
    probe.buffered_packets.push(discarded(0x22));
    probe.buffered_bytes = MAX_PROBE_PACKET_BYTES;
  }
  // SAFETY: as above.
  let sent = unsafe { dec.send_av_packet(&past) };
  assert!(
    matches!(sent, Ok(Sent::Accepted)),
    "taken, and committed: {sent:?}"
  );
  assert_eq!(dec.phase(), SessionPhase::Streaming);
}

/// LAW: **a candidate that refuses the packet past the ceiling fails, and
/// the next backend is tried with the whole history** — never committed
/// before that packet's answer.
///
/// The candidate on trial is a rawvideo decoder of 32 × 32 pictures, so a
/// packet of 16 × 16 is short of a picture and refused (`rawdec.c`
/// 342–346); the probe's own parameters, from which it builds the next
/// candidate in software, say 16 × 16. The history, held to the ceiling,
/// is constructed as it stands: 256 packets of pre-roll.
///
/// PLANT: committing at the ceiling before the packet is sent, as the
/// last round did, turns this red at "the next candidate took it":
/// the refusal is reported plain, and no other backend is tried.
#[test]
fn a_candidate_that_refuses_the_packet_past_the_ceiling_fails_and_the_next_takes_the_whole_history()
{
  let history = || {
    (0..MAX_PROBE_PACKETS)
      .map(|index| discarded(index as u8))
      .collect::<Vec<Packet>>()
  };
  let at_the_ceiling = |remaining: Vec<Backend>| {
    let mut dec = raw_auditioning_decoder_with(32, crate::DecoderOptions::new());
    let probe = dec.probe.as_mut().expect("on trial");
    probe.parameters = raw_parameters(RAW_SIDE);
    probe.build = VideoDecoder::software_state_for_test;
    probe.remaining_backends = remaining;
    probe.buffered_packets = history();
    probe.buffered_bytes = MAX_PROBE_PACKETS * RAW_PICTURE;
    dec
  };
  let past = raw_packet(0x5A);

  // A backend left: it is replayed the whole history, takes the packet
  // past the ceiling, and is committed with it.
  let mut dec = at_the_ceiling(vec![Backend::Cuda]);
  // SAFETY: `raw_packet`'s, every byte written and the padding zeroed, and
  // nothing writes through it while the decoder may still read it.
  let sent = unsafe { dec.send_av_packet(&past) };
  assert!(
    matches!(sent, Ok(Sent::Accepted)),
    "the next candidate took it: {sent:?}"
  );
  assert_eq!(dec.backend(), Backend::Cuda, "the next candidate's");
  assert_eq!(dec.phase(), SessionPhase::Streaming, "and committed");
  assert!(
    holds_a_picture(&mut dec),
    "its picture, the committed candidate's"
  );

  // None left: the whole history comes back, with the refusal that
  // failed the candidate, and the packet stays the caller's.
  let mut dec = at_the_ceiling(Vec::new());
  // SAFETY: as above.
  let Err(Error::AllBackendsFailed(p)) = (unsafe { dec.send_av_packet(&past) }) else {
    panic!("the candidate fails on its own refusal, and the probe exhausts");
  };
  assert_eq!(p.attempts().len(), 1, "{:?}", p.attempts());
  assert!(
    matches!(
      *p.attempts()[0].1,
      Error::Ffmpeg(ffmpeg_next::Error::Other { errno }) if errno == libc::EINVAL
    ),
    "the candidate's own refusal: {:?}",
    p.attempts()
  );
  assert_eq!(
    p.unconsumed_packets().len(),
    MAX_PROBE_PACKETS,
    "the whole history"
  );
}

/// LAW: **a flush mid-probe forgets the history whole, and the ceiling
/// counts from it**: every count the probe keeps of its history is reset
/// with the packets ([`ProbeState::forget_history`]), and the candidate is
/// still on trial.
///
/// PLANT: a flush that keeps the packets turns this red at "forgotten".
#[test]
fn a_flush_mid_probe_forgets_the_history_and_the_ceiling_counts_from_it() {
  let held = |dec: &VideoDecoder| {
    dec
      .probe
      .as_ref()
      .map(|probe| (probe.buffered_packets.len(), probe.buffered_bytes))
  };
  let preroll = |count: usize| {
    (0..count)
      .map(|index| discarded(index as u8))
      .collect::<Vec<Packet>>()
  };
  let mut dec = raw_auditioning_decoder();
  send_all(&mut dec, &preroll(200));
  assert_eq!(held(&dec), Some((200, 200 * RAW_PICTURE)));
  dec.flush();
  assert_eq!(held(&dec), Some((0, 0)), "forgotten, and still on trial");
  // 256 after the flush are held: 456 since the open.
  send_all(&mut dec, &preroll(MAX_PROBE_PACKETS));
  assert_eq!(
    held(&dec),
    Some((MAX_PROBE_PACKETS, MAX_PROBE_PACKETS * RAW_PICTURE)),
    "the ceiling counts from the flush"
  );
  // The 257th after it is past the ceiling, taken, and commits.
  send_all(&mut dec, &preroll(1));
  assert_eq!(held(&dec), None, "committed at the ceiling");
}

/// A 64 × 64 VP9 key frame, profile 0: libvpx 1.16's grey picture at its
/// lowest quality (`ffmpeg -f lavfi -i color=c=gray:s=64x64 -frames:v 1
/// -c:v libvpx-vp9 -crf 63 -b:v 0 -auto-alt-ref 0 -lag-in-frames 0`). Its
/// first byte, `0x82`, holds the frame marker, profile 0, a key frame,
/// `show_frame` 1 and no error resilience.
const VP9_KEY_FRAME: [u8; 26] = [
  0x82, 0x49, 0x83, 0x42, 0x00, 0x03, 0xF0, 0x03, 0xF6, 0x00, 0x38, 0x24, 0x1C, 0x19, 0x70, 0x00,
  0x00, 0x30, 0x60, 0x00, 0x00, 0x27, 0x82, 0x70, 0x00, 0x00,
];

/// `frame` with its `show_frame` bit cleared: a hidden frame, decoded into
/// the references and output never (`vp9.c` 1818–1821). On a key frame the
/// header reads on unchanged past the bit (556–566).
fn hidden(mut frame: [u8; 26]) -> [u8; 26] {
  frame[0] &= !0b10;
  frame
}

/// A 64 × 64 AV1 temporal unit — a temporal delimiter, a sequence header
/// and one shown key frame — as SVT-AV1 4.2 encodes a grey picture at its
/// lowest quality (`ffmpeg -f lavfi -i color=c=gray:s=64x64 -frames:v 1
/// -c:v libsvtav1 -crf 63 -preset 12`).
const AV1_KEY_FRAME: [u8; 29] = [
  0x12, 0x00, 0x0A, 0x0B, 0x02, 0x00, 0x00, 0x05, 0x15, 0x7F, 0xFC, 0x4A, 0xF9, 0x00, 0x40, 0x32,
  0x0C, 0x10, 0x00, 0xF3, 0x82, 0x3F, 0xFE, 0x69, 0x83, 0x00, 0x00, 0x05, 0x80,
];

/// A packet of `bytes`, every byte written and the padding zeroed.
fn coded(bytes: &[u8]) -> Packet {
  crate::boundary::try_packet_copy(bytes).expect("a submittable packet")
}

/// Codec parameters naming `id` video of 64 × 64 pixels, and nothing more.
fn coded_parameters(id: AVCodecID) -> codec::Parameters {
  let mut parameters = codec::Parameters::new();
  // SAFETY: `parameters` owns a live, zeroed `AVCodecParameters`; every
  // field written is a plain scalar.
  unsafe {
    let raw = parameters.as_mut_ptr();
    (*raw).codec_type = AVMediaType::AVMEDIA_TYPE_VIDEO;
    (*raw).codec_id = id;
    (*raw).width = 64;
    (*raw).height = 64;
  }
  parameters
}

/// A decoder on trial with no backend left to advance to, over
/// libavcodec's decoder `name` for `id`, its `skip_frame` at `skip`; or
/// `None` where this FFmpeg links no decoder of that name.
fn maybe_auditioning_by_name(
  name: &str,
  id: AVCodecID,
  skip: crate::Discard,
) -> Option<VideoDecoder> {
  ffmpeg_next::init().expect("ffmpeg init");
  let codec = ffmpeg_next::decoder::find_by_name(name)?;
  Some(
    VideoDecoder::from_software_for_test_as(
      coded_parameters(id),
      codec,
      crate::limits::DecoderLimits::default(),
      crate::DecoderOptions::new().with_skip_frame(skip),
      /*auditioning=*/ true,
    )
    .expect("a software-backed candidate on trial"),
  )
}

/// [`maybe_auditioning_by_name`], for a decoder every FFmpeg build links.
fn auditioning_by_name(name: &str, id: AVCodecID, skip: crate::Discard) -> VideoDecoder {
  maybe_auditioning_by_name(name, id, skip).unwrap_or_else(|| panic!("FFmpeg links {name}"))
}

/// Asks `dec` for pictures until it says otherwise, and answers how many
/// came and what it said then.
fn pictures(dec: &mut VideoDecoder) -> (usize, Result<Received>) {
  let mut frame = crate::Frame::empty().expect("frame slot");
  let mut count = 0;
  loop {
    match dec.receive_frame(&mut frame) {
      Ok(Received::Produced) => count += 1,
      other => return (count, other),
    }
  }
}

/// Whether the candidate's own decoder has a picture to give: libavcodec's
/// answer, read short of this crate's transfer, which a software
/// candidate's CPU frame cannot cross
/// (`a_candidate_is_replayed_copies_and_the_history_stays_the_probes_own`).
fn holds_a_picture(dec: &mut VideoDecoder) -> bool {
  let mut picture = frame::Video::empty();
  dec.state.inner.receive_frame(&mut picture).is_ok()
}

/// LAW: **a VP9 stream of hidden frames ends the trial in a clean, empty
/// completion, at every skip level** — and a shown one gives its picture at
/// every level, `All` among them: FFmpeg 9.0.1's VP9 decoder reads no
/// `skip_frame` at all, so no level foretells what it gives.
///
/// A software candidate's picture cannot cross this crate's transfer
/// (`a_candidate_is_replayed_copies_and_the_history_stays_the_probes_own`),
/// so the shown one is read from libavcodec ([`holds_a_picture`]).
///
/// A hidden frame is decoded into the references and never output
/// (`vp9.c` 1818–1821): a candidate fed one alone has done all it was
/// asked when its end comes.
///
/// PLANT: routing a clean end past the end to the candidate failing again
/// turns this red at "hidden".
#[test]
fn a_hidden_vp9_frame_ends_the_trial_cleanly_at_every_skip_level() {
  for skip in crate::Discard::ALL {
    let mut dec = auditioning_by_name("vp9", AVCodecID::AV_CODEC_ID_VP9, skip);
    send_all(&mut dec, &[coded(&hidden(VP9_KEY_FRAME))]);
    assert_eq!(dec.send_eof().expect("the end taken"), Sent::Accepted);
    let (count, end) = pictures(&mut dec);
    assert!(
      count == 0 && matches!(end, Ok(Received::Ended)),
      "{skip}, hidden: {count} pictures, then {end:?}"
    );
    assert_eq!(dec.phase(), SessionPhase::Draining, "{skip}: committed");

    let mut dec = auditioning_by_name("vp9", AVCodecID::AV_CODEC_ID_VP9, skip);
    send_all(&mut dec, &[coded(&VP9_KEY_FRAME)]);
    assert!(holds_a_picture(&mut dec), "{skip}, shown: its picture");
  }
}

/// LAW: **the native AV1 decoder answers for itself under every skip
/// level**, and the probe takes its answer, never the level's.
///
/// It reads `NonIntra`, `NonKey` and `All` alone, before it asks for a
/// pixel format (`av1dec.c` 1166–1171, 1173–1179), and it decodes nothing
/// without a hardware accelerator: asked for a format in software, it
/// answers `ENOSYS` (677–682). So a key frame under `All` is skipped, and
/// the end completes the candidate cleanly; under every other level, the
/// key frame — key and intra, so `NonKey` and `NonIntra` keep it — reaches
/// that answer, and the candidate fails on its own error.
#[test]
fn the_native_av1_decoder_answers_for_itself_under_every_skip_level() {
  for skip in crate::Discard::ALL {
    let mut dec = auditioning_by_name("av1", AVCodecID::AV_CODEC_ID_AV1, skip);
    let tu = coded(&AV1_KEY_FRAME);
    // SAFETY: `coded`'s, every byte written and the padding zeroed, and
    // nothing writes through it while the decoder may still read it.
    let sent = unsafe { dec.send_av_packet(&tu) };
    if skip == crate::Discard::All {
      assert!(matches!(sent, Ok(Sent::Accepted)), "{skip}: {sent:?}");
      assert_eq!(dec.send_eof().expect("the end taken"), Sent::Accepted);
      let (count, end) = pictures(&mut dec);
      assert!(
        count == 0 && matches!(end, Ok(Received::Ended)),
        "{skip}: {count} pictures, then {end:?}"
      );
      assert_eq!(dec.phase(), SessionPhase::Draining, "{skip}: committed");
    } else {
      let Err(Error::AllBackendsFailed(p)) = sent else {
        panic!("{skip}: the candidate fails on its own error, got {sent:?}");
      };
      assert!(
        matches!(
          p.attempts(),
          [(_, error)] if matches!(
            **error,
            Error::Ffmpeg(ffmpeg_next::Error::Other { errno }) if errno == libc::ENOSYS
          )
        ),
        "{skip}: {:?}",
        p.attempts()
      );
    }
  }
}

/// LAW: **`libdav1d` gives its key picture under every skip level, `All`
/// among them**: it decodes key frames alone under `NonKey` and `All`
/// alike (`libdav1d.c` 236–243). `All` is no promise that no picture
/// comes, and a shortcut that took it for one was wrong here too.
///
/// Where this FFmpeg links no `libdav1d` (the CI build does not), the law
/// has nothing to stand on and says so.
#[test]
fn libdav1d_gives_its_key_picture_under_every_skip_level() {
  for skip in crate::Discard::ALL {
    let Some(mut dec) = maybe_auditioning_by_name("libdav1d", AVCodecID::AV_CODEC_ID_AV1, skip)
    else {
      eprintln!("skip: this FFmpeg links no libdav1d");
      return;
    };
    send_all(&mut dec, &[coded(&AV1_KEY_FRAME)]);
    assert_eq!(dec.send_eof().expect("the end taken"), Sent::Accepted);
    assert!(holds_a_picture(&mut dec), "{skip}: its key picture");
  }
}

/// LAW: **on VideoToolbox, a VP9 stream of hidden frames ends cleanly on
/// the hardware decoder at every skip level** — the standalone probe, and
/// the stream decoder's `Auto`, which stays on hardware — and a shown one
/// gives its picture there.
///
/// VideoToolbox's VP9 decoder declines a 16 × 16 picture, so the frames
/// are 64 × 64; and a virtual machine may offer no VP9 decoder at all, so
/// the law runs where one is asked for.
#[cfg(target_vendor = "apple")]
#[test]
#[ignore = "decodes VP9 on VideoToolbox, which a virtual machine may not offer"]
fn on_videotoolbox_a_hidden_vp9_frame_ends_cleanly_at_every_skip_level() {
  use mediadecode::decoder::VideoStreamDecoder;

  ffmpeg_next::init().expect("ffmpeg init");
  let timebase = mediadecode::Timebase::new(1, core::num::NonZeroI32::new(25).expect("non-zero"));
  for skip in crate::Discard::ALL {
    let options = crate::DecoderOptions::new().with_skip_frame(skip);
    for (what, frame, shown) in [
      ("hidden", hidden(VP9_KEY_FRAME), 0),
      ("shown", VP9_KEY_FRAME, 1),
    ] {
      let mut dec = VideoDecoder::open_from(
        VideoDecoderOpen::new().with_options(options),
        coded_parameters(AVCodecID::AV_CODEC_ID_VP9),
      )
      .expect("open on the probe");
      send_all(&mut dec, &[coded(&frame)]);
      assert_eq!(dec.send_eof().expect("the end taken"), Sent::Accepted);
      let (count, end) = pictures(&mut dec);
      assert!(
        count == shown && matches!(end, Ok(Received::Ended)),
        "{skip}, {what}: {count} pictures, then {end:?}"
      );
      assert_eq!(dec.backend(), Backend::VideoToolbox, "{skip}, {what}");
      assert_eq!(dec.phase(), SessionPhase::Draining, "{skip}, {what}");

      let mut auto = crate::FfmpegOwnedVideoStreamDecoder::open_from(
        crate::VideoStreamDecoderOpen::new().with_options(options),
        coded_parameters(AVCodecID::AV_CODEC_ID_VP9),
        timebase,
        crate::limits::DecoderLimits::default(),
      )
      .expect("open on Auto");
      let packet = mediadecode::packet::VideoPacket::new(
        crate::FfmpegBytes::try_copy_from_slice(&frame).expect("the packet's bytes"),
        crate::extras::VideoPacketExtra::new(0),
      )
      .with_pts(Some(mediadecode::Timestamp::new(0, timebase)));
      assert_eq!(auto.send_packet(&packet).expect("taken"), Sent::Accepted);
      assert_eq!(auto.send_eof().expect("the end taken"), Sent::Accepted);
      let mut picture = crate::empty_owned_video_frame();
      let mut count = 0;
      while let Received::Produced = auto.receive_frame(&mut picture).expect("decode cleanly") {
        count += 1;
      }
      assert_eq!(count, shown, "{skip}, {what}: Auto");
      assert!(
        !auto.is_software(),
        "{skip}, {what}: Auto stays on hardware"
      );
    }
  }
}
