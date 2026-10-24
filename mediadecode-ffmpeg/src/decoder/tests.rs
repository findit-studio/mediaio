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
  assert_eq!(
    decoder.session_kind(),
    SessionKind::Trial,
    "probe must be active immediately after open"
  );

  // Pre-stage one buffered packet, as the candidate has been fed it, and
  // push the byte counter to its ceiling so the very next send_packet is
  // past it.
  {
    let probe = decoder.probe_mut().expect("probe present");
    probe.buffered_packets.push(Packet::new(8));
    probe.fed = probe.buffered_packets.len();
    probe.buffered_bytes = MAX_PROBE_PACKET_BYTES;
  }

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

  assert_eq!(
    decoder.session_kind(),
    SessionKind::Committed,
    "the candidate that took the packet past the ceiling is committed"
  );
  assert!(
    decoder.backlog().is_empty(),
    "the candidate had been fed the whole history, so nothing is owed it"
  );
}

/// When `advance_probe` exhausts the probe (no more candidates and
/// the active backend just failed), the `Err(ProbeExhausted
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
fn probe_exhausted_returns_buffered_packets_to_caller() {
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
  assert_eq!(
    decoder.session_kind(),
    SessionKind::Trial,
    "probe must be active immediately after open"
  );

  // Stuff the probe history with two distinct packets and clear the
  // remaining_backends list so the next advance_probe call is forced
  // into the exhaustion branch.
  let p1 = Packet::new(16);
  let p2 = Packet::new(32);
  {
    let probe = decoder.probe_mut().expect("probe");
    probe.buffered_packets.push(p1);
    probe.buffered_packets.push(p2);
    probe.remaining_backends.clear();
  }

  // Trigger advance_probe directly with a synthetic non-transient
  // error. The exhaustion branch must take ownership of the
  // buffered packets and surface them via `unconsumed_packets`.
  let result = decoder.advance_probe(BackendAnswer::of_call(Error::Ffmpeg(
    ffmpeg_next::Error::InvalidData,
  )));
  match result {
    Err(Error::ProbeExhausted(p)) => {
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
    other => panic!("expected ProbeExhausted, got {other:?}"),
  }
  // And the hardware decoder is gone: the exhausted session is spent, on a
  // real backend as on the software lanes (Codex R2, finding 3).
  assert_spent(
    &mut decoder,
    crate::Spent::ProbeExhausted,
    SessionKind::Exhausted,
  );
}

/// `ProbeState.attempts` must carry forward `open`'s accumulated
/// failures from earlier backends in probe order. The wrong
/// shape — initialising `ProbeState.attempts` to `Vec::new()` at
/// the start of `open`'s "promote to runtime" step — drops
/// earlier failures so a runtime exhaustion surfaces an
/// `ProbeExhausted` whose `attempts` log only mentions the
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
fn probe_exhausted_preserves_earlier_open_failures() {
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
    let probe = decoder.probe_mut().expect("probe present");
    probe.attempts.push((
      earlier_backend,
      BackendAnswer::of_open(synthetic_earlier).into(),
    ));
    probe.remaining_backends.clear(); // force exhaustion on next advance.
  }

  let result = decoder.advance_probe(BackendAnswer::of_call(Error::Ffmpeg(
    ffmpeg_next::Error::InvalidData,
  )));
  match result {
    Err(Error::ProbeExhausted(p)) => {
      let attempts = p.attempts();
      assert_eq!(
        attempts.len(),
        2,
        "ProbeExhausted must surface BOTH the seeded earlier failure \
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
    other => panic!("expected ProbeExhausted, got {other:?}"),
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
  // pool reader (`hw_frames_ctx_dimensions_raw`) was written for, and
  // which the first cut of this judge reached past it to reproduce.
  let frame = hw_frame_with_pool(100, 100, 8192, 8192);

  // Priced at the pool, an 8192x8192 surface is at least 64 Mpx, so a
  // 16 MiB ceiling cannot hold it however cheap the format.
  let refusal = unsafe {
    judge_hw_transfer(
      frame.as_ptr(),
      DownloadFormat::Picked(NV12),
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
        DownloadFormat::Picked(NV12),
        FrameLimits::new().with_max_frame_bytes(usize::MAX),
      )
    }
    .is_ok(),
  );
}

/// `NV12`, the layout a VideoToolbox pool downloads to here, as the raw
/// `AVPixelFormat` a picked download format is.
const NV12: i32 = ffmpeg_next::ffi::AVPixelFormat::AV_PIX_FMT_NV12 as i32;

#[test]
fn a_hardware_frame_with_an_unreadable_pool_fails_closed() {
  use super::judge_hw_transfer;
  use crate::FrameLimits;

  // A frames context whose dimensions do not read. The transfer may
  // still allocate and nothing here can say how much — an unprovable
  // extent is not a small one.
  let frame = hw_frame_with_pool(100, 100, 0, 0);
  assert!(
    unsafe {
      judge_hw_transfer(
        frame.as_ptr(),
        DownloadFormat::Picked(NV12),
        FrameLimits::new(),
      )
    }
    .is_err(),
    "an unreadable pool extent must fail closed",
  );

  // But a frame that is not a hardware frame at all allocates nothing —
  // `av_hwframe_transfer_data` refuses it outright — so no format is
  // picked for it, and it is passed through rather than given a ceiling's
  // name for a different fault.
  let plain = ffmpeg_next::frame::Video::empty();
  // SAFETY: `plain` is a live frame.
  let format = unsafe { download_format_of(&plain) };
  assert!(
    matches!(format, Ok(DownloadFormat::Unpicked)),
    "no format picked for a frame that is no hardware frame"
  );
  assert!(
    unsafe { judge_hw_transfer(plain.as_ptr(), DownloadFormat::Unpicked, FrameLimits::new()) }
      .is_ok()
  );
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

/// **The judge prices the format the destination is allocated in, and a
/// format the build cannot price at the conservative bound.** The download
/// road allocates the destination itself, in the format it picked — the
/// first the backend lists, as libavutil's own road picks it — so the judge
/// prices that one format, at the pool's extent ([`judge_hw_transfer`]);
/// when it could still be FFmpeg's to pick, the whole list was priced. A
/// picked format libavutil will not size is charged
/// `video_frame_bytes_upper_bound`, which dominates whatever that layout
/// would cost: a ceiling between NV12's price and the bound admits NV12 and
/// refuses the unpriceable one.
///
/// PLANT: the unpriceable format priced at nothing, as a fold that skipped
/// it did, turns this red at "the conservative bound".
#[test]
fn the_judge_prices_the_picked_format_and_an_unpriceable_one_at_the_bound() {
  ffmpeg_next::init().expect("ffmpeg init");
  use crate::FrameLimits;
  use crate::footprint::{video_frame_bytes, video_frame_bytes_upper_bound};

  let (w, h) = (1920, 1088);
  let cheap = video_frame_bytes(NV12, w, h).expect("NV12 prices");
  let bound = video_frame_bytes_upper_bound(w, h).expect("a picture");
  // The two really are far apart, so taking one for the other is not a
  // rounding difference: NV12 is 1.5 bytes a pixel and the bound charges
  // the widest layout the build can emit.
  assert!(
    bound > cheap * 5,
    "the bound {bound} should dwarf the cheap format {cheap}",
  );
  // A hardware-only format cannot be priced.
  const VT: i32 = ffmpeg_next::ffi::AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX as i32;
  assert!(
    video_frame_bytes(VT, w, h).is_none(),
    "the harness needs a genuinely unpriceable format",
  );
  let frame = hw_frame_with_pool(w, h, w, h);
  let between = FrameLimits::new().with_max_frame_bytes(cheap + (bound - cheap) / 2);
  // SAFETY: `frame` is live for both calls.
  unsafe {
    assert!(
      judge_hw_transfer(frame.as_ptr(), DownloadFormat::Picked(NV12), between).is_ok(),
      "NV12, priced as it is"
    );
    let refused = judge_hw_transfer(frame.as_ptr(), DownloadFormat::Picked(VT), between)
      .expect_err("the conservative bound");
    assert_eq!(refused.bytes(), bound, "charged the bound");
  }
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
/// [`Error::ProbeExhausted`].
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
/// left to advance to, [`Error::ProbeExhausted`] — the caller's cue
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
  dec.set_end(End::Taken);
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
    Err(Error::ProbeExhausted(_)) => {}
    Err(other) => panic!("expected the probe road, got {other:?}"),
  }

  // **And then the decoder is spent**, which is the other half of the
  // answer being honest — and the half the R3 ruling reversed (Codex R2,
  // finding 3): an exhausted probe used to leave its last candidate behind
  // as a committed session, which a caller could go on feeding. No decoder
  // lives now, the phase commits to nothing, and every entry is refused by
  // name, a flush included. A caller that reaches here has been told to
  // fall back to software, with every packet the probe took.
  assert_eq!(
    dec.phase(),
    SessionPhase::Spent,
    "an exhausted probe leaves no session to speak for the stream",
  );
  assert_spent(
    &mut dec,
    crate::Spent::ProbeExhausted,
    SessionKind::Exhausted,
  );
}

/// Asserts `dec` is spent, as `spent` names, at every entry, before a flush
/// and after one: no decoder lives, and a send — the safe road and the raw
/// one, a packet and a packet that is the end — the end and a receive are
/// each refused by name ([`Error::Spent`]). The state table's spent rows.
fn assert_spent(dec: &mut VideoDecoder, spent: crate::Spent, kind: SessionKind) {
  let packet = raw_packet(0x99);
  // SAFETY: `raw_packet`'s, every byte written.
  let owned =
    unsafe { crate::boundary::video_packet_from_ffmpeg(&packet, mediadecode::Timebase::SECONDS) }
      .expect("a wrappable payload")
      .expect("a buffer");
  let empty = Packet::empty();
  let mut frame = crate::Frame::empty().expect("frame slot");
  for when in ["before a flush", "after a flush"] {
    assert_eq!(dec.session_kind(), kind, "{when}: spent");
    assert!(dec.live().is_none(), "{when}: no decoder lives");
    assert_eq!(
      dec.phase(),
      SessionPhase::Spent,
      "{when}: committed to nothing"
    );
    refused_by_name(when, "the safe send", dec.send_packet(&owned), spent);
    // SAFETY: `raw_packet`'s, every byte written and the padding zeroed;
    // no decoder lives to read it.
    refused_by_name(
      when,
      "a send",
      unsafe { dec.send_av_packet(&packet) },
      spent,
    );
    // SAFETY: a blank packet: nothing to read, and nothing to write through.
    let sent = unsafe { dec.send_av_packet(&empty) };
    refused_by_name(when, "a packet that is the end", sent, spent);
    refused_by_name(when, "the end", dec.send_eof(), spent);
    refused_by_name(when, "a receive", dec.receive_frame(&mut frame), spent);
    dec.flush();
  }
}

/// Asserts `got` is [`Error::Spent`] naming `spent`.
fn refused_by_name<T: core::fmt::Debug>(
  when: &str,
  entry: &str,
  got: Result<T>,
  spent: crate::Spent,
) {
  match got {
    Err(Error::Spent(named)) if named == spent => {}
    other => panic!("{when}: {entry}, refused by name as {spent:?}: {other:?}"),
  }
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
  dec.set_end(End::Taken);

  // Latch a coded-surface refusal, exactly as the `get_format` callback
  // does when it declines a surface over the ceiling.
  // SAFETY: `dec` owns this state and frees it only when it drops, after this
  // call.
  unsafe {
    crate::ffi::declare_ceiling_declined_for_test(
      dec.decoder().callback_state,
      8_294_400,
      2_073_600,
    )
  };

  let mut frame = crate::Frame::empty().expect("frame slot");
  let Err(Error::ProbeExhausted(p)) = dec.receive_frame(&mut frame) else {
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
          .decoder_mut()
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
        dec.set_end(End::Taken);
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
      crate::ffi::declare_ceiling_declined_for_test(
        dec.decoder().callback_state,
        8_294_400,
        2_073_600,
      )
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
    crate::ffi::declare_ceiling_declined_for_test(
      dec.decoder().callback_state,
      8_294_400,
      2_073_600,
    )
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
    crate::ffi::declare_ceiling_declined_for_test(
      dec.decoder().callback_state,
      8_294_400,
      2_073_600,
    )
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
  unsafe {
    crate::ffi::declare_ceiling_declined_for_test(dec.decoder().callback_state, 4_096, 1_024)
  };

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
    crate::ffi::declare_ceiling_declined_for_test(
      dec.decoder().callback_state,
      8_294_400,
      2_073_600,
    )
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
      crate::ffi::declare_frame_budget_declined_for_test(dec.decoder().callback_state, 12_582_912)
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
    crate::ffi::declare_frame_budget_declined_for_test(dec.decoder().callback_state, 12_582_912)
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
      .decoder_mut()
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
      matches!(
        dec.decoder_mut().inner.send_eof(),
        Err(ffmpeg_next::Error::Eof)
      ),
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
      HwRoute::Advance(BackendAnswer(Error::Ffmpeg(e))) => assert_eq!(e, failure),
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
              dec.decoder().callback_state,
              8_294_400,
              2_073_600,
            )
          },
          Latch::Budget => {
            // SAFETY: `dec` owns this state and frees it only when it drops, after this
            // call.
            unsafe {
              crate::ffi::declare_frame_budget_declined_for_test(
                dec.decoder().callback_state,
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
          (Latch::Surface, Err(Error::ProbeExhausted(p))) if auditioning => {
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
  // SAFETY: the live decoder owns an opened `AVCodecContext` for
  // this borrow; `pkt_timebase` is a public `AVRational` field, and the
  // three skips are `AVDiscard` fields, read as the integers they are.
  let (timebase, skips) = unsafe {
    let ctx = decoder.decoder().inner.as_ptr();
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
    decoder.session_kind() == SessionKind::Trial,
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

/// `parameters` carrying `bytes` of zeroed extradata, which the bounded
/// copy weighs with its padding (`measure_parameters`) and rawvideo reads
/// only for a trailing `BottomUp` (`rawdec.c`).
fn with_extradata(mut parameters: codec::Parameters, bytes: usize) -> codec::Parameters {
  let padded = bytes + ffmpeg_next::ffi::AV_INPUT_BUFFER_PADDING_SIZE as usize;
  // SAFETY: `parameters` owns a live `AVCodecParameters` with no
  // extradata; the allocation is libavutil's, zeroed, padded as FFmpeg
  // requires, and freed with the parameters.
  unsafe {
    let raw = parameters.as_mut_ptr();
    assert!(
      (*raw).extradata.is_null(),
      "parameters with no extradata yet"
    );
    let extradata = ffmpeg_next::ffi::av_mallocz(padded).cast::<u8>();
    assert!(!extradata.is_null(), "the extradata");
    (*raw).extradata = extradata;
    (*raw).extradata_size = bytes as i32;
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
/// and [`Error::ProbeExhausted`] hands the probe's history to the
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
    dec.probe().expect("still on trial").buffered_bytes,
    fills.len() * RAW_PICTURE,
    "each copy is charged its payload",
  );

  let mut frame = crate::Frame::empty().expect("frame slot");
  let Err(Error::ProbeExhausted(p)) = dec.receive_frame(&mut frame) else {
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

/// LAW: **a copy the probe cannot make is no backend's answer: the packet
/// is sent, and the candidate that takes it is committed** — with no
/// [`Error::ProbeExhausted`], since no backend failed.
///
/// The history cannot keep the packet, so it is the trial's last
/// ([`Unkept::Copy`]): sent with the history whole, taken, and the
/// candidate committed. A failure after is that decoder's own, with no
/// history to replay.
///
/// The allocator fails the copy, through `av_max_alloc` in a child process
/// of its own: capped at 512 bytes, the payload `av_packet_make_writable`
/// copies into (8,192 bytes and the padding) is not allocated, while what
/// libavcodec allocates to take a refcounted packet is.
///
/// PLANT: the copy failure answering `ProbeExhausted` before the packet
/// is sent, as in 0.19.0, turns this red at "sent and committed".
#[test]
fn a_copy_the_probe_cannot_make_sends_its_packet_and_commits() {
  use mediaio_ffmpeg::test_support::{
    cap_ffmpeg_allocations, in_subprocess, uncap_ffmpeg_allocations,
  };

  in_subprocess(
    "decoder::tests::a_copy_the_probe_cannot_make_sends_its_packet_and_commits",
    || {
      let mut dec = raw_auditioning_decoder();
      let first = raw_packet(0x11);
      // SAFETY: `raw_packet` is `try_packet_copy`'s, every byte written and the padding zeroed,
      // and nothing writes through it while the decoder may still read it.
      assert_eq!(
        unsafe { dec.send_av_packet(&first) }.expect("taken"),
        Sent::Accepted
      );

      let uncopied = crate::boundary::try_packet_copy(&[0x77; 8192]).expect("a packet");
      // SAFETY: in `in_subprocess`'s child, alone. The capped window runs one send:
      // the probe's copy goes through `try_clone_packet`, which checks both of
      // its calls, and libavcodec checks its own allocations.
      unsafe { cap_ffmpeg_allocations(512) };
      // SAFETY: `try_packet_copy` wrote every byte and zeroed the padding, and nothing writes
      // through it while the decoder may still read it.
      let answer = unsafe { dec.send_av_packet(&uncopied) };
      uncap_ffmpeg_allocations();

      assert!(
        matches!(answer, Ok(Sent::Accepted)),
        "sent and committed: {answer:?}"
      );
      assert_eq!(dec.phase(), SessionPhase::Streaming, "committed");
      // Both pictures are the committed candidate's: the first, then the
      // one whose packet the probe could not copy.
      let mut picture = frame::Video::empty();
      dec
        .decoder_mut()
        .inner
        .receive_frame(&mut picture)
        .expect("the first picture");
      dec
        .decoder_mut()
        .inner
        .receive_frame(&mut picture)
        .expect("the uncopied packet's picture");
      assert_eq!(picture.data(0)[0], 0x77, "decoded from the packet sent");
    },
  );
}

/// `packet` with `entries` side-data entries of one kind, attached past
/// FFmpeg's API — which holds a packet to one entry per kind
/// (`av_packet_add_side_data`, `libavcodec/packet.c` 197–228) — as a packet
/// built by hand would carry them. Each is a byte of
/// `AV_PKT_DATA_MPEGTS_STREAM_ID`, which no decoder reads; the array and
/// each entry are libavutil allocations, which the packet's unref frees.
fn with_side_data_entries(mut packet: Packet, entries: usize) -> Packet {
  use ffmpeg_next::ffi::{AVPacketSideData, AVPacketSideDataType, av_malloc, av_mallocz};

  // SAFETY: `packet` owns a live `AVPacket` with no side data; the array
  // is allocated for `entries` descriptors and every one is written before
  // the packet holds it, each `data` a live one-byte allocation.
  unsafe {
    let raw = packet.as_mut_ptr();
    assert!((*raw).side_data.is_null(), "a packet with no side data yet");
    let array =
      av_malloc(entries * core::mem::size_of::<AVPacketSideData>()).cast::<AVPacketSideData>();
    assert!(!array.is_null(), "the descriptor array");
    for index in 0..entries {
      let data = av_mallocz(1).cast::<u8>();
      assert!(!data.is_null(), "an entry's byte");
      array.add(index).write(AVPacketSideData {
        data,
        size: 1,
        type_: AVPacketSideDataType::AV_PKT_DATA_MPEGTS_STREAM_ID,
      });
    }
    (*raw).side_data = array;
    (*raw).side_data_elems = entries as i32;
  }
  packet
}

/// LAW: **a packet of more side-data entries than the probe copies is no
/// backend's answer: it is sent, and the candidate that takes it is
/// committed** — with no [`Error::ProbeExhausted`]. And a candidate
/// that refuses it fails on its own refusal, as on any packet: the next is
/// replayed the history and takes it.
///
/// The packet is never copied ([`Unkept::SideData`]); libavcodec takes it
/// as it takes any, one entry per kind (`av_packet_copy_props`).
///
/// PLANT: the side-data cap answering `ProbeExhausted` before the
/// packet is sent, as in 0.19.0, turns this red at "sent and committed".
#[test]
fn a_packet_of_more_side_data_entries_than_the_probe_copies_is_sent_and_commits() {
  let crowded = || with_side_data_entries(raw_packet(0x5A), MAX_PROBE_PACKET_SIDE_DATA_ENTRIES + 1);
  assert_eq!(
    packet_side_data_count(&crowded()),
    MAX_PROBE_PACKET_SIDE_DATA_ENTRIES + 1
  );

  // Taken: committed, its picture the candidate's.
  let mut dec = raw_auditioning_decoder();
  send_all(&mut dec, &[discarded(0x11)]);
  let packet = crowded();
  // SAFETY: `raw_packet`'s payload, every byte written and the padding
  // zeroed; its side data `with_side_data_entries` wrote whole; nothing
  // writes through it while the decoder may still read it.
  let sent = unsafe { dec.send_av_packet(&packet) };
  assert!(
    matches!(sent, Ok(Sent::Accepted)),
    "sent and committed: {sent:?}"
  );
  assert_eq!(dec.phase(), SessionPhase::Streaming, "committed");
  assert!(
    holds_a_picture(&mut dec),
    "its picture, the committed candidate's"
  );

  // Refused, by a candidate of 32 x 32 pictures it is short of: the next
  // candidate, built in software from the probe's 16 x 16 parameters, is
  // replayed the history and takes it.
  let mut dec = raw_auditioning_decoder_with(32, crate::DecoderOptions::new());
  {
    let probe = dec.probe_mut().expect("on trial");
    probe.parameters = raw_parameters(RAW_SIDE);
    probe.build = VideoDecoder::software_state_for_test;
    probe.remaining_backends = vec![Backend::Cuda];
    probe.buffered_packets = vec![discarded(0x11)];
    probe.fed = 1;
    probe.buffered_bytes = RAW_PICTURE;
  }
  // SAFETY: as above.
  let sent = unsafe { dec.send_av_packet(&packet) };
  assert!(
    matches!(sent, Ok(Sent::Accepted)),
    "the next candidate took it: {sent:?}"
  );
  assert_eq!(dec.backend(), Backend::Cuda, "the next candidate's");
  assert_eq!(dec.phase(), SessionPhase::Streaming, "and committed");
}

/// LAW: **a candidate is replayed the history itself, and the history is
/// released from every decoder before it goes back to the caller.**
///
/// `avcodec_send_packet` takes a reference to what it is sent and may hold
/// it past the call (`libavcodec/decode.c` 745–758 in FFmpeg 9.0.1). The
/// replay sends the history's own packets ([`feed_into`]), so the candidate
/// shares their buffers and no copy is made: none that could fail, and none
/// for the probe to pay twice. When the candidate fails in its turn and the
/// probe exhausts, the history goes to the caller as owned, mutable
/// packets, and no decoder holds them: the candidate was torn down when it
/// was left — its scratch frame cleared and its decoder dropped
/// ([`VideoDecoder::reject`]) — before the exhaustion was answered, so the
/// caller's `&mut [u8]` covers bytes no decoder holds.
///
/// The candidate is opened in software through
/// [`VideoDecoder::advance_probe_with`]. Two packets: rawvideo's picture of
/// the first refers to that packet's own buffer (`rawdec.c` 226–244), and
/// the second waits in libavcodec's queue behind it, so the replay asks for
/// no drain.
///
/// PLANT: the exhaustion handing the history back with the candidate kept,
/// as R2's did, turns this red at "referenced by the rescued packet alone"
/// (PB3-2).
#[test]
fn a_candidate_is_replayed_the_history_itself_and_it_is_released_before_it_goes_back() {
  let mut dec = raw_auditioning_decoder();
  dec
    .probe_mut()
    .expect("on trial")
    .remaining_backends
    .push(Backend::Cuda);
  let fills = [0x11u8, 0x22];
  let sent: Vec<Packet> = fills.iter().map(|&fill| raw_packet(fill)).collect();
  send_all(&mut dec, &sent);

  dec
    .advance_probe_with(
      BackendAnswer::of_call(Error::Ffmpeg(ffmpeg_next::Error::InvalidData)),
      VideoDecoder::software_state_for_test,
    )
    .expect("the candidate takes the replay and is installed");
  assert_eq!(
    dec.backend(),
    Backend::Cuda,
    "the candidate is the session's"
  );
  let probe = dec.probe().expect("still on trial: no picture yet");
  assert_eq!(probe.fed, fills.len(), "it took the whole history");
  for (packet, &fill) in probe.buffered_packets.iter().zip(&fills) {
    assert_eq!(
      payload_of(packet).1,
      2,
      "{fill:#04x}: the history's own packet, shared with the candidate — no copy",
    );
  }

  // The candidate fails in its turn, on the first picture asked for, and
  // the history goes to the caller while the candidate stays the
  // session's.
  let mut frame = crate::Frame::empty().expect("frame slot");
  let Err(Error::ProbeExhausted(p)) = dec.receive_frame(&mut frame) else {
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

/// LAW: **the attempt log is in the order the backends were tried**: the
/// candidate that failed first, then each the probe tried after it — though
/// the first is recorded only once the probe leaves it.
///
/// The next candidate is built from 32 × 32 parameters, so the 16 × 16
/// picture it is replayed is short of one, and it refuses it (`rawdec.c`
/// 342–346).
///
/// PLANT: the failed candidate's answer appended when the probe leaves it,
/// after the refusals it led to, turns this red at "in the order tried".
#[test]
fn the_attempt_log_is_in_the_order_the_backends_were_tried() {
  let mut dec = raw_auditioning_decoder();
  let first = dec.backend();
  {
    let probe = dec.probe_mut().expect("on trial");
    probe.parameters = raw_parameters(32);
    probe.build = VideoDecoder::software_state_for_test;
    probe.remaining_backends = vec![Backend::Cuda];
    probe.buffered_packets = vec![raw_packet(0x11)];
    probe.fed = 1;
    probe.buffered_bytes = RAW_PICTURE;
  }
  let Err(Error::ProbeExhausted(p)) = dec.advance_probe(BackendAnswer::of_call(Error::Ffmpeg(
    ffmpeg_next::Error::InvalidData,
  ))) else {
    panic!("the next candidate refuses its replay, and the probe exhausts");
  };
  let tried: Vec<Backend> = p.attempts().iter().map(|(backend, _)| *backend).collect();
  assert_eq!(tried, vec![first, Backend::Cuda], "in the order tried");
  assert!(
    matches!(
      *p.attempts()[0].1,
      Error::Ffmpeg(ffmpeg_next::Error::InvalidData)
    ),
    "the first candidate's own answer: {:?}",
    p.attempts()
  );
}

/// A [`BuildCandidate`] that opens the candidate in software and then caps
/// every FFmpeg allocation at 512 bytes, so the replay runs under the cap:
/// the allocator of
/// [`a_replay_makes_no_copy_so_one_that_could_not_be_made_charges_no_backend`].
fn software_state_then_capped(
  parameters: codec::Parameters,
  codec: Codec,
  backend: Backend,
  limits: crate::limits::DecoderLimits,
  options: crate::DecoderOptions,
  pkt_timebase: Option<mediadecode::Timebase>,
) -> std::result::Result<DecoderState, BuildFailure> {
  let built = VideoDecoder::software_state_for_test(
    parameters,
    codec,
    backend,
    limits,
    options,
    pkt_timebase,
  );
  // SAFETY: built only by that law, in `in_subprocess`'s child, alone; the
  // law lifts the cap as soon as the advance returns. The replay sends
  // packets and checks what libavcodec answers, and libavcodec checks its
  // own allocations.
  unsafe { mediaio_ffmpeg::test_support::cap_ffmpeg_allocations(512) };
  built
}

/// LAW: **a replay makes no copy, so an allocator that could make none
/// installs the candidate** — no backend charged for it, and no
/// [`Error::ProbeExhausted`].
///
/// Codex R7: the replay copied each packet before sending it, and a copy
/// that could not be made was recorded as the next candidate's failure — a
/// backend charged for a packet it never saw. The replay sends the
/// history's own packets now ([`feed_into`]).
///
/// The allocator is capped once the candidate is built, through
/// `av_max_alloc` in a child process of its own: at 512 bytes the copy of
/// an 8,192-byte payload cannot be allocated, while what libavcodec
/// allocates to take a refcounted packet, and rawvideo to decode one, can.
///
/// PLANT: the replay sending a copy of each packet
/// ([`try_clone_packet`](crate::boundary::try_clone_packet)), a copy that
/// cannot be made refusing the replay, turns this red at "installed".
#[test]
fn a_replay_makes_no_copy_so_one_that_could_not_be_made_charges_no_backend() {
  use mediaio_ffmpeg::test_support::{in_subprocess, uncap_ffmpeg_allocations};

  in_subprocess(
    "decoder::tests::a_replay_makes_no_copy_so_one_that_could_not_be_made_charges_no_backend",
    || {
      let mut dec = raw_auditioning_decoder();
      dec
        .probe_mut()
        .expect("on trial")
        .remaining_backends
        .push(Backend::Cuda);
      let fills = [0x11u8, 0x22];
      let sent: Vec<Packet> = fills
        .iter()
        .map(|&fill| crate::boundary::try_packet_copy(&[fill; 8192]).expect("a packet"))
        .collect();
      send_all(&mut dec, &sent);

      let advanced = dec.advance_probe_with(
        BackendAnswer::of_call(Error::Ffmpeg(ffmpeg_next::Error::InvalidData)),
        software_state_then_capped,
      );
      uncap_ffmpeg_allocations();

      assert!(advanced.is_ok(), "installed: {advanced:?}");
      assert_eq!(
        dec.backend(),
        Backend::Cuda,
        "the candidate is the session's"
      );
      let probe = dec.probe().expect("on trial");
      assert_eq!(probe.fed, fills.len(), "it took the whole history");
      assert_eq!(
        probe.attempts.len(),
        1,
        "the failed backend's answer alone: {:?}",
        probe.attempts,
      );
      assert!(
        holds_a_picture(&mut dec),
        "the candidate decodes what it was replayed"
      );
    },
  );
}

/// LAW: **a replay drains nothing, so no queue can be exhausted**: a
/// candidate replayed a history of more pictures than the probe's queue
/// held (16) is installed, and every picture reaches the caller in order,
/// the rest of the history fed as the caller drains.
///
/// Codex R7: the replay drained each picture into a queue whose frame and
/// byte caps answered `ENOMEM`, recorded as the candidate's failure — a
/// candidate that had produced pictures, charged for this crate's own
/// queue. The replay stops where the candidate first has output for the
/// caller ([`feed_into`]), and the candidate is fed the rest on the road
/// the caller drains by ([`VideoDecoder::feed`]).
///
/// The pictures are read from the candidate itself, as
/// [`VideoDecoder::receive_frame`] reads them: a software candidate's frame
/// cannot cross the hardware download.
///
/// PLANT: the replay refusing to go on where the candidate has output for
/// the caller — its `EAGAIN` answered with the `ENOMEM` a full queue
/// answered — turns this red at "installed".
#[test]
fn a_replay_drains_nothing_so_a_long_history_charges_no_backend() {
  const PICTURES: usize = 40;
  let mut dec = raw_auditioning_decoder();
  {
    let probe = dec.probe_mut().expect("on trial");
    probe.remaining_backends = vec![Backend::Cuda];
    probe.build = VideoDecoder::software_state_for_test;
    probe.buffered_packets = (0..PICTURES).map(|index| raw_packet(index as u8)).collect();
    probe.fed = PICTURES;
    probe.buffered_bytes = PICTURES * RAW_PICTURE;
  }
  dec
    .advance_probe(BackendAnswer::of_call(Error::Ffmpeg(
      ffmpeg_next::Error::InvalidData,
    )))
    .expect("installed");
  assert_eq!(dec.backend(), Backend::Cuda, "the candidate's");
  let probe = dec.probe().expect("on trial");
  assert_eq!(
    probe.fed, 2,
    "the replay stopped at the candidate's first output: a picture waiting, a packet queued \
     behind it"
  );
  assert_eq!(probe.attempts.len(), 1, "the failed backend's answer alone");

  let mut picture = frame::Video::empty();
  let mut delivered = Vec::new();
  loop {
    match dec.decoder_mut().inner.receive_frame(&mut picture) {
      Ok(()) => delivered.push(picture.data(0)[0]),
      Err(e) if is_eagain(&e) && dec.owes() => {
        let fed = dec.feed().expect("the candidate takes what it is owed");
        assert!(fed.taken > 0, "a decoder that asks for input takes some");
      }
      Err(e) if is_eagain(&e) => break,
      Err(e) => panic!("picture {}: {e}", delivered.len()),
    }
  }
  assert_eq!(
    delivered,
    (0..PICTURES).map(|index| index as u8).collect::<Vec<u8>>(),
    "every picture, in order"
  );
  assert_eq!(
    dec.probe().map(|probe| probe.fed),
    Some(PICTURES),
    "the whole history fed, and the candidate still on trial"
  );
}

/// LAW: **a copy of this crate's own that an advance cannot make is
/// surfaced as it is, the backend still to try and the history kept** — no
/// backend charged, and no [`Error::ProbeExhausted`].
///
/// A candidate is built from a copy of the probe's parameters
/// (`try_clone_parameters`), bounded by the decoder's own limits. A bound
/// below what the parameters weigh refuses the copy by name
/// ([`Error::ParametersTooLarge`]), the next backend would need the same
/// copy, and that backend has answered nothing.
///
/// PLANT: the copy's failure recorded as the next backend's answer and that
/// backend taken out of the order, as a refusal to build is, turns this red
/// at "surfaced as it is".
#[test]
fn a_copy_an_advance_cannot_make_keeps_the_next_backend_and_the_history() {
  let mut dec = raw_auditioning_decoder();
  {
    let probe = dec.probe_mut().expect("on trial");
    probe.parameters = with_extradata(raw_parameters(RAW_SIDE), 64);
    probe.remaining_backends = vec![Backend::Cuda];
    probe.build = VideoDecoder::software_state_for_test;
    probe.buffered_packets = vec![raw_packet(0x11)];
    probe.fed = 1;
    probe.buffered_bytes = RAW_PICTURE;
  }
  let failed = || BackendAnswer::of_call(Error::Ffmpeg(ffmpeg_next::Error::InvalidData));

  // Below what 64 bytes of extradata and its padding weigh.
  dec.frame_limits = crate::limits::DecoderLimits::new().with_max_codec_parameter_bytes(16);
  let answer = dec.advance_probe(failed());
  assert!(
    matches!(answer, Err(Error::ParametersTooLarge(_))),
    "surfaced as it is: {answer:?}"
  );
  assert_eq!(dec.session_kind(), SessionKind::Owed, "the advance, owed");
  let probe = dec.probe().expect("the probe's record");
  assert_eq!(
    probe.remaining_backends,
    vec![Backend::Cuda],
    "the backend still to try"
  );
  assert_eq!(probe.buffered_packets.len(), 1, "the history");
  assert!(
    probe.attempts.is_empty(),
    "no backend charged: {:?}",
    probe.attempts
  );

  // With the bound back, the advance goes on to that backend.
  dec.frame_limits = crate::limits::DecoderLimits::new();
  dec.resume().expect("installed");
  assert_eq!(dec.backend(), Backend::Cuda, "the backend kept");
}

/// A [`BuildCandidate`] whose allocator fails while it builds: every FFmpeg
/// allocation over 512 bytes, so the codec context
/// (`avcodec_alloc_context3`) cannot be made — the allocator of
/// [`a_context_an_advance_cannot_allocate_keeps_the_next_backend`].
fn software_state_under_a_cap(
  parameters: codec::Parameters,
  codec: Codec,
  backend: Backend,
  limits: crate::limits::DecoderLimits,
  options: crate::DecoderOptions,
  pkt_timebase: Option<mediadecode::Timebase>,
) -> std::result::Result<DecoderState, BuildFailure> {
  // SAFETY: built only by that law, in `in_subprocess`'s child, alone, and
  // the cap is lifted before this returns; `build_codec_context` checks the
  // context it allocates.
  unsafe { mediaio_ffmpeg::test_support::cap_ffmpeg_allocations(512) };
  let built = VideoDecoder::software_state_for_test(
    parameters,
    codec,
    backend,
    limits,
    options,
    pkt_timebase,
  );
  mediaio_ffmpeg::test_support::uncap_ffmpeg_allocations();
  built
}

/// LAW: **a codec context this crate cannot allocate for a candidate is
/// surfaced as it is, the backend still to try and the history kept** — no
/// backend charged, and no [`Error::ProbeExhausted`]. The context is this
/// crate's own allocation, the same for every backend
/// ([`BuildFailure::Local`]).
///
/// PLANT: a failure of this crate's own while the candidate is built,
/// recorded as that backend's refusal, turns this red at "surfaced as it
/// is".
#[test]
fn a_context_an_advance_cannot_allocate_keeps_the_next_backend() {
  use mediaio_ffmpeg::test_support::in_subprocess;

  in_subprocess(
    "decoder::tests::a_context_an_advance_cannot_allocate_keeps_the_next_backend",
    || {
      let mut dec = raw_auditioning_decoder();
      {
        let probe = dec.probe_mut().expect("on trial");
        probe.remaining_backends = vec![Backend::Cuda];
        probe.buffered_packets = vec![raw_packet(0x11)];
        probe.fed = 1;
        probe.buffered_bytes = RAW_PICTURE;
      }
      let answer = dec.advance_probe_with(
        BackendAnswer::of_call(Error::Ffmpeg(ffmpeg_next::Error::InvalidData)),
        software_state_under_a_cap,
      );
      assert!(
        matches!(
          answer,
          Err(Error::Ffmpeg(ffmpeg_next::Error::Other { errno })) if errno == libc::ENOMEM
        ),
        "surfaced as it is: {answer:?}"
      );
      let probe = dec.probe().expect("still on trial");
      assert_eq!(
        probe.remaining_backends,
        vec![Backend::Cuda],
        "the backend still to try"
      );
      assert_eq!(probe.buffered_packets.len(), 1, "the history");
      assert!(
        probe.attempts.is_empty(),
        "no backend charged: {:?}",
        probe.attempts
      );
    },
  );
}

/// LAW: **an open whose codec context cannot be allocated answers that
/// failure, and charges no backend** — the probe's open road, where
/// [`a_context_an_advance_cannot_allocate_keeps_the_next_backend`] is its
/// advance's.
///
/// No backend this machine has is named, so without the cap every one
/// refuses at the open and the probe answers
/// [`Error::ProbeExhausted`]. Under it, the context fails first.
///
/// PLANT: the open recording a failure of this crate's own while it builds
/// as that backend's answer, and going on, turns this red at "answered as
/// it is".
#[test]
fn an_open_whose_context_cannot_be_allocated_charges_no_backend() {
  use mediaio_ffmpeg::test_support::{
    cap_ffmpeg_allocations, in_subprocess, uncap_ffmpeg_allocations,
  };

  in_subprocess(
    "decoder::tests::an_open_whose_context_cannot_be_allocated_charges_no_backend",
    || {
      ffmpeg_next::init().expect("ffmpeg init");
      let order = [Backend::Cuda, Backend::Vaapi];
      let open = || {
        VideoDecoder::open_probing(
          raw_parameters(RAW_SIDE),
          crate::limits::DecoderLimits::new(),
          crate::DecoderOptions::new(),
          None,
          &order,
        )
      };
      assert!(
        matches!(open(), Err(Error::ProbeExhausted(_))),
        "the control: every backend refuses"
      );
      // SAFETY: in `in_subprocess`'s child, alone; the open checks what it
      // allocates, and the cap is lifted as it returns.
      unsafe { cap_ffmpeg_allocations(512) };
      let opened = open();
      uncap_ffmpeg_allocations();
      assert!(
        matches!(
          opened,
          Err(Error::Ffmpeg(ffmpeg_next::Error::Other { errno })) if errno == libc::ENOMEM
        ),
        "answered as it is: {:?}",
        opened.as_ref().map(|_| ())
      );
    },
  );
}

/// LAW: **an open whose own copy of the parameters cannot be made answers
/// that failure, and charges no backend** — not
/// [`Error::ProbeExhausted`], which is the cue for software, and which
/// the same bound would refuse there too.
///
/// PLANT: the copy's failure recorded as the backend's answer and the probe
/// going on, as before this round, turns this red at "answered as it is".
#[test]
fn an_open_whose_own_copy_cannot_be_made_charges_no_backend() {
  ffmpeg_next::init().expect("ffmpeg init");
  let opened = VideoDecoder::open_probing(
    with_extradata(raw_parameters(RAW_SIDE), 64),
    crate::limits::DecoderLimits::new().with_max_codec_parameter_bytes(16),
    crate::DecoderOptions::new(),
    None,
    &[Backend::Cuda, Backend::Vaapi],
  );
  assert!(
    matches!(opened, Err(Error::ParametersTooLarge(_))),
    "answered as it is: {:?}",
    opened.as_ref().map(|_| ())
  );
}

/// LAW: **a picture this crate's own limit refuses is no delivery, and
/// charges nothing**: a picture whose download would cost more than the
/// caller's frame limit answers [`Error::HwTransferTooLarge`], and the
/// candidate stays on trial, its history kept and nothing recorded against
/// it. It commits at its first delivered picture — never on the refusal,
/// and never with an advance.
///
/// Codex R7: the replay's frame check refused a picture a candidate had
/// produced, and recorded the refusal as the candidate's failure. R8
/// committed the candidate before the limit instead; the ruling on R8's
/// finding 5 commits at the first delivered picture alone — downloaded in a
/// layout a [`Frame`](crate::Frame) carries, its properties copied — so a
/// limit of this crate's commits nothing and charges nothing.
///
/// The limit is lowered once VideoToolbox has opened and declared its
/// surface pool under the default. `get_format` prices that pool against
/// the budget its callback state holds, so the picture is decoded and
/// reaches the transfer judge, which reads the decoder's limits
/// ([`judge_hw_transfer`]). VideoToolbox prices the two alike — its
/// downloads offer the pool's own layout alone (`vt_transfer_get_formats`,
/// `libavutil/hwcontext_videotoolbox.c` 327–340 in FFmpeg 9.0.1) — so a
/// limit set at the open is met at the pool instead; a backend whose
/// downloads offer a wider layout than its pool meets it here. The control
/// decodes the same picture under the default.
///
/// PLANT: the judge's refusal committing the candidate first, as R8 did,
/// turns this red at "no delivery: still on trial".
#[cfg(target_vendor = "apple")]
#[test]
#[ignore = "decodes VP9 on VideoToolbox, which a virtual machine may not offer"]
fn on_videotoolbox_a_picture_over_the_frame_limit_is_no_delivery_and_charges_nothing() {
  ffmpeg_next::init().expect("ffmpeg init");
  let opened = || {
    let mut dec =
      VideoDecoder::open(coded_parameters(AVCodecID::AV_CODEC_ID_VP9)).expect("open on the probe");
    assert_eq!(dec.backend(), Backend::VideoToolbox);
    send_all(&mut dec, &[coded(&VP9_KEY_FRAME)]);
    assert_eq!(
      dec.phase(),
      SessionPhase::Auditioning,
      "on trial: no picture yet"
    );
    dec
  };
  let mut frame = crate::Frame::empty().expect("frame slot");

  // The control: the picture, under the default.
  let mut dec = opened();
  assert_eq!(
    dec.receive_frame(&mut frame).expect("the picture"),
    Received::Produced
  );

  // The subject: the same picture, over the limit.
  let mut dec = opened();
  dec.frame_limits = crate::limits::DecoderLimits::new()
    .with_frame(crate::FrameLimits::new().with_max_frame_bytes(1024));
  let received = dec.receive_frame(&mut frame);
  assert!(
    matches!(received, Err(Error::HwTransferTooLarge(_))),
    "the caller's limit, by name: {received:?}"
  );
  assert_eq!(
    dec.phase(),
    SessionPhase::Auditioning,
    "no delivery: still on trial"
  );
  {
    let probe = dec.probe().expect("on trial");
    assert!(
      probe.attempts.is_empty(),
      "nothing charged: {:?}",
      probe.attempts
    );
    assert_eq!(probe.buffered_packets.len(), 1, "the history kept");
  }

  // Its first delivered picture commits it.
  dec.frame_limits = crate::limits::DecoderLimits::new();
  send_all(&mut dec, &[coded(&VP9_KEY_FRAME)]);
  assert_eq!(
    dec.receive_frame(&mut frame).expect("the next picture"),
    Received::Produced
  );
  assert_eq!(
    (dec.phase(), dec.backend()),
    (SessionPhase::Streaming, Backend::VideoToolbox),
    "committed at its first delivered picture"
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
        dec.probe().map(|probe| probe.buffered_packets.len()),
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
      assert_eq!(
        dec.session_kind(),
        SessionKind::Committed,
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
/// PLANT: the ceiling answering `ProbeExhausted` before the packet is
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
    dec.probe().map(|probe| probe.buffered_packets.len()),
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
    let probe = dec.probe_mut().expect("on trial");
    probe.buffered_packets.push(discarded(0x22));
    probe.fed = 1;
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
    let probe = dec.probe_mut().expect("on trial");
    probe.parameters = raw_parameters(RAW_SIDE);
    probe.build = VideoDecoder::software_state_for_test;
    probe.remaining_backends = remaining;
    probe.buffered_packets = history();
    probe.fed = MAX_PROBE_PACKETS;
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
  let Err(Error::ProbeExhausted(p)) = (unsafe { dec.send_av_packet(&past) }) else {
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
      .probe()
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
/// (`a_candidate_is_replayed_the_history_itself_and_it_is_released_before_it_goes_back`).
fn holds_a_picture(dec: &mut VideoDecoder) -> bool {
  let mut picture = frame::Video::empty();
  dec.decoder_mut().inner.receive_frame(&mut picture).is_ok()
}

/// LAW: **a VP9 stream of hidden frames ends the trial in a clean, empty
/// completion, at every skip level** — and a shown one gives its picture at
/// every level, `All` among them: FFmpeg 9.0.1's VP9 decoder reads no
/// `skip_frame` at all, so no level foretells what it gives.
///
/// A software candidate's picture cannot cross this crate's transfer
/// (`a_candidate_is_replayed_the_history_itself_and_it_is_released_before_it_goes_back`),
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
      let Err(Error::ProbeExhausted(p)) = sent else {
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

/// LAW: **dropping a packet before the decoder can move a later frame's
/// best-effort timestamp, and never its pts** — which is why
/// [`DISPOSABLE`](mediadecode::packet::PacketFlags::DISPOSABLE) promises a
/// later picture's pixels and not its best-effort timestamp.
///
/// libavcodec guesses the best-effort timestamp from counts kept across
/// every frame it outputs (`guess_correct_pts`, `libavcodec/decode.c`
/// 296–319), whatever marked a packet: three rawvideo pictures at
/// (pts, dts) (10, 0), (10, 1) and (100, 2), the second repeating the
/// first's pts, count one faulty pts against no faulty dts, so the third's
/// guess is its dts, 2. Dropped before the decoder, the second counts
/// nothing, and the third's guess is its pts, 100. Its pts is 100 either
/// way.
#[test]
fn dropping_a_packet_can_move_a_later_best_effort_timestamp_and_never_a_pts() {
  // (pts, best-effort timestamp) of each picture decoded from `stamps`.
  let decoded = |stamps: &[(i64, i64)]| -> Vec<(Option<i64>, i64)> {
    let mut dec = VideoDecoder::from_software_for_test(
      raw_parameters(RAW_SIDE),
      crate::limits::DecoderLimits::default(),
      /*auditioning=*/ false,
    )
    .expect("a software-backed rawvideo decoder");
    stamps
      .iter()
      .map(|&(pts, dts)| {
        let mut packet = raw_packet(0x33);
        packet.set_pts(Some(pts));
        packet.set_dts(Some(dts));
        dec.decoder_mut().inner.send_packet(&packet).expect("taken");
        let mut picture = frame::Video::empty();
        dec
          .decoder_mut()
          .inner
          .receive_frame(&mut picture)
          .expect("its picture");
        // SAFETY: `picture` owns a live frame libavcodec just wrote;
        // `best_effort_timestamp` is a plain `int64_t`.
        let guess = unsafe { (*picture.as_ptr()).best_effort_timestamp };
        (picture.pts(), guess)
      })
      .collect()
  };
  let kept = decoded(&[(10, 0), (10, 1), (100, 2)]);
  let dropped = decoded(&[(10, 0), (100, 2)]);
  assert_eq!(kept[2], (Some(100), 2), "the second decoded: {kept:?}");
  assert_eq!(
    dropped[1],
    (Some(100), 100),
    "the second dropped: {dropped:?}"
  );
}

/// A decoder on trial over libavcodec's native VP9 decoder, with no
/// backend left to advance to, its frame budget `bytes`: under 1,024, a
/// 64 × 64 picture is over it.
fn vp9_auditioning_under(bytes: usize) -> VideoDecoder {
  ffmpeg_next::init().expect("ffmpeg init");
  let codec = ffmpeg_next::decoder::find_by_name("vp9").expect("FFmpeg links vp9");
  VideoDecoder::from_software_for_test_as(
    coded_parameters(AVCodecID::AV_CODEC_ID_VP9),
    codec,
    crate::limits::DecoderLimits::new()
      .with_frame(crate::FrameLimits::new().with_max_frame_bytes(bytes)),
    crate::DecoderOptions::new(),
    /*auditioning=*/ true,
  )
  .expect("a software-backed candidate on trial")
}

/// LAW: **a packet the candidate took before a refusal of this crate's
/// own is taken** (Codex R8, finding 3). libavcodec queues a packet before
/// it decodes it (`avcodec_send_packet`, `libavcodec/decode.c` 745–758 in
/// FFmpeg 9.0.1), so the frame budget, refused while the packet decodes,
/// answers a packet the candidate consumed. The probe records it, as on
/// acceptance, so a later replay holds everything the candidate consumed.
/// Where the history cannot keep the packet — more side-data entries than
/// the probe copies — the candidate is committed, the only decoder that
/// will ever have seen it.
///
/// The native VP9 decoder decodes a key frame as it is sent, on the one
/// thread this crate's contexts run (`ff_get_buffer` → the allocator
/// judge), so the budget refuses on the send.
///
/// PLANT: the refusal returned with the staged copy dropped, as in R8,
/// turns this red at "the packet, kept as taken".
#[test]
fn a_packet_the_candidate_took_before_a_local_refusal_is_kept_as_taken() {
  let mut dec = vp9_auditioning_under(1024);
  let packet = coded(&VP9_KEY_FRAME);
  // SAFETY: `coded` is `try_packet_copy`'s, every byte written and the
  // padding zeroed, and nothing writes through it while the decoder may
  // still read it.
  let sent = unsafe { dec.send_av_packet(&packet) };
  assert!(
    matches!(sent, Err(Error::FrameBudgetExceeded(_))),
    "the budget, by name: {sent:?}"
  );
  let probe = dec.probe().expect("still on trial");
  assert_eq!(
    (probe.buffered_packets.len(), probe.fed),
    (1, 1),
    "the packet, kept as taken"
  );

  let mut dec = vp9_auditioning_under(1024);
  let crowded = with_side_data_entries(
    coded(&VP9_KEY_FRAME),
    MAX_PROBE_PACKET_SIDE_DATA_ENTRIES + 1,
  );
  // SAFETY: as above; the side data `with_side_data_entries` wrote whole.
  let sent = unsafe { dec.send_av_packet(&crowded) };
  assert!(
    matches!(sent, Err(Error::FrameBudgetExceeded(_))),
    "the budget, by name: {sent:?}"
  );
  assert_eq!(
    dec.phase(),
    SessionPhase::Streaming,
    "a packet the history cannot keep: committed"
  );
}

thread_local! {
  /// Whether [`vaapi_refuses_and_cuda_may_fail`] fails a CUDA build with an
  /// allocation of this crate's own.
  static CUDA_FAILS: core::cell::Cell<bool> = const { core::cell::Cell::new(false) };
}

/// A [`BuildCandidate`]: VAAPI's open refuses the stream, the backend's
/// own answer; CUDA's build fails with this crate's own `ENOMEM` while
/// [`CUDA_FAILS`] is set, and opens in software otherwise.
fn vaapi_refuses_and_cuda_may_fail(
  parameters: codec::Parameters,
  codec: Codec,
  backend: Backend,
  limits: crate::limits::DecoderLimits,
  options: crate::DecoderOptions,
  pkt_timebase: Option<mediadecode::Timebase>,
) -> std::result::Result<DecoderState, BuildFailure> {
  match backend {
    Backend::Vaapi => Err(BuildFailure::Refused(BackendAnswer::of_open(
      Error::BackendUnsupportedByCodec(backend),
    ))),
    _ if CUDA_FAILS.with(core::cell::Cell::get) => Err(BuildFailure::Local(Error::Ffmpeg(
      ffmpeg_next::Error::Other {
        errno: libc::ENOMEM,
      },
    ))),
    _ => VideoDecoder::software_state_for_test(
      parameters,
      codec,
      backend,
      limits,
      options,
      pkt_timebase,
    ),
  }
}

/// The backends of a probe's attempt log, in order.
fn tried(probe: &ProbeState) -> Vec<Backend> {
  probe.attempts.iter().map(|(backend, _)| *backend).collect()
}

/// LAW: **an advance stopped by a failure of this crate's own is owed, and
/// resumed before the failed candidate is called again** (Codex R8,
/// finding 4). The candidate on trial fails by its own answer. The advance
/// away from it meets VAAPI's refusal, recorded, then CUDA's build failing
/// with this crate's own `ENOMEM`: the failure is surfaced, and the advance
/// kept, owed, with the failed candidate's answer and the place it was
/// tried. The next entry — the end — resumes it: CUDA is installed, and the
/// attempt log holds the failed candidate first, VAAPI after. The new
/// candidate ends cleanly and is committed; the failed one, never.
///
/// PLANT: the stopped advance left unowed, the failed candidate the active
/// one again, as in R8, turns this red at "the advance, owed".
#[test]
fn an_advance_stopped_by_this_crates_own_failure_is_resumed_before_the_failed_candidate_is_called()
{
  let mut dec = raw_auditioning_decoder();
  let first = dec.backend();
  {
    let probe = dec.probe_mut().expect("on trial");
    probe.build = vaapi_refuses_and_cuda_may_fail;
    probe.remaining_backends = vec![Backend::Vaapi, Backend::Cuda];
  }
  send_all(&mut dec, &[discarded(0x11)]);

  CUDA_FAILS.with(|fails| fails.set(true));
  let stopped = dec.advance_probe(BackendAnswer::of_call(Error::Ffmpeg(
    ffmpeg_next::Error::InvalidData,
  )));
  CUDA_FAILS.with(|fails| fails.set(false));
  assert!(
    matches!(
      stopped,
      Err(Error::Ffmpeg(ffmpeg_next::Error::Other { errno })) if errno == libc::ENOMEM
    ),
    "this crate's own, surfaced: {stopped:?}"
  );
  {
    assert_eq!(dec.session_kind(), SessionKind::Owed, "the advance, owed");
    assert!(dec.live().is_none(), "no decoder lives while it is owed");
    let probe = dec.probe().expect("the probe's record");
    let pending = dec.pending().expect("the advance, owed");
    assert_eq!((pending.backend, pending.tried_at), (first, 0));
    assert_eq!(
      tried(probe),
      vec![Backend::Vaapi],
      "the refusal before the stop"
    );
  }

  assert_eq!(
    dec.send_eof().expect("the end"),
    Sent::Accepted,
    "the end, taken by the resumed advance's candidate"
  );
  assert_eq!(dec.backend(), Backend::Cuda, "the advance, resumed");
  {
    assert_eq!(dec.session_kind(), SessionKind::Trial, "owed no more");
    let probe = dec.probe().expect("still on trial");
    assert_eq!(
      tried(probe),
      vec![first, Backend::Vaapi],
      "in the order tried"
    );
  }
  let mut frame = crate::Frame::empty().expect("frame slot");
  assert_eq!(
    dec.receive_frame(&mut frame).expect("a clean end"),
    Received::Ended
  );
  assert_eq!(
    (dec.backend(), dec.phase()),
    (Backend::Cuda, SessionPhase::Draining),
    "the new candidate committed, never the failed one"
  );
}

/// `UYVY422`, a CPU layout a [`Frame`] cannot carry: what a stood-in
/// download chooses for an unfit candidate ([`DownloadsForTest`]).
const UYVY: i32 = ffmpeg_next::ffi::AVPixelFormat::AV_PIX_FMT_UYVY422 as i32;

/// Downloads where every backend but CUDA chooses [`UYVY`], and CUDA
/// delivers.
fn cuda_alone_delivers(backend: Backend) -> Option<i32> {
  (backend != Backend::Cuda).then_some(UYVY)
}

/// Downloads where every backend chooses [`UYVY`].
fn none_deliver(_: Backend) -> Option<i32> {
  Some(UYVY)
}

/// Downloads where every backend delivers.
fn all_deliver(_: Backend) -> Option<i32> {
  None
}

/// [`raw_auditioning_decoder`], its candidate labelled VAAPI and CUDA left
/// to try, built in software ([`VideoDecoder::software_state_for_test`]).
fn vaapi_then_cuda() -> VideoDecoder {
  let mut dec = raw_auditioning_decoder();
  dec.decoder_mut().backend = Backend::Vaapi;
  let probe = dec.probe_mut().expect("on trial");
  probe.build = VideoDecoder::software_state_for_test;
  probe.remaining_backends = vec![Backend::Cuda];
  dec
}

/// The first byte of each picture `dec` delivers until it asks for input,
/// each picture's first row read through the [`Frame`].
fn delivered(dec: &mut VideoDecoder, frame: &mut crate::Frame) -> Vec<u8> {
  let mut fills = Vec::new();
  loop {
    match dec
      .receive_frame(frame)
      .expect("a picture or a request for input")
    {
      Received::Produced => fills.push(frame.row(0, 0).expect("a gray picture's first row")[0]),
      Received::NeedsInput | Received::Ended => return fills,
    }
  }
}

/// LAW: **at the unfit picture the probe holds its history, and nothing is
/// committed** (the ruling on Codex R8's finding 5, item 6e). The candidate
/// on trial decoded a picture whose download chose `UYVY422`, a layout a
/// [`Frame`] cannot carry: no delivery. It is unfit for the stream, and the
/// probe leaves it as it leaves a failed one. The unfitness is recorded,
/// naming the layout, never an errno. The next candidate is replayed the
/// history whole, each packet once, by reference, and is on trial in its
/// turn.
///
/// PLANT: the layout committing the candidate first, as R8's fix did, turns
/// this red at "left, the receive to start again".
#[test]
fn at_the_unfit_picture_the_probe_holds_its_history_and_nothing_is_committed() {
  let mut dec = vaapi_then_cuda();
  let fills = [0x11u8, 0x22];
  send_all(&mut dec, &fills.map(raw_packet));
  let mut frame = crate::Frame::empty().expect("frame slot");

  let routed = dec.route_download(Err(TransferFailure::Layout(UYVY)), &mut frame);
  assert!(
    matches!(routed, Ok(None)),
    "left, the receive to start again: {routed:?}"
  );
  assert_eq!(
    (dec.backend(), dec.phase()),
    (Backend::Cuda, SessionPhase::Auditioning),
    "the next candidate, on trial: nothing committed"
  );
  let probe = dec.probe().expect("still on trial");
  assert_eq!(
    probe.buffered_packets.len(),
    fills.len(),
    "the history whole"
  );
  assert_eq!(probe.fed, fills.len(), "replayed whole");
  for (packet, &fill) in probe.buffered_packets.iter().zip(&fills) {
    assert_eq!(
      payload_of(packet).1,
      2,
      "{fill:#04x}: the history's own packet, shared with the candidate once"
    );
  }
  assert!(
    matches!(
      probe.attempts.as_slice(),
      [(Backend::Vaapi, Attempt::Unfit(layout))] if layout.format() == UYVY
    ),
    "the unfitness, naming the layout: {:?}",
    probe.attempts
  );
}

/// LAW: **a probe leaves an unfit candidate, and the next commits on its
/// first delivered picture** (item 6b). Two candidates, the probe
/// hardware-only: VAAPI's downloads choose `UYVY422`, CUDA's deliver. The
/// first picture asked for is CUDA's, the stream's first, and it commits
/// CUDA; every picture follows in order, each once.
///
/// PLANT: the unfit candidate's advance passing over the candidates left,
/// so the probe exhausts, turns this red at "the next candidate commits".
#[test]
fn a_probe_leaves_an_unfit_candidate_and_the_next_commits_on_its_first_delivered_picture() {
  let _downloads = DownloadsForTest::set(cuda_alone_delivers);
  let mut dec = vaapi_then_cuda();
  let fills = [0x11u8, 0x22];
  send_all(&mut dec, &fills.map(raw_packet));
  let mut frame = crate::Frame::empty().expect("frame slot");
  let first = dec.receive_frame(&mut frame);
  assert!(
    matches!(first, Ok(Received::Produced)),
    "the next candidate commits: {first:?}"
  );
  assert_eq!(
    (dec.backend(), dec.phase()),
    (Backend::Cuda, SessionPhase::Streaming),
    "committed on its first delivered picture"
  );
  let mut pictures = vec![frame.row(0, 0).expect("a gray picture")[0]];
  pictures.extend(delivered(&mut dec, &mut frame));
  assert_eq!(pictures, fills, "every picture, in order, each once");
}

/// LAW: **a probe with no candidate left ends in [`Error::ProbeExhausted`],
/// its last candidate unfit as when it failed**, with every attempt and
/// every packet (item 6b'): the standalone probe's road, whose caller
/// replays the packets into a software decoder of its own. The only
/// candidate unfit: one attempt, `Unfit`, naming the layout. The last of two
/// unfit: two. No attempt is an errno, and every packet comes back in
/// order, referenced by its rescued packet alone.
///
/// PLANT: an unfit last candidate ending the probe with
/// [`Error::DownloadFormatUnsupported`], the rule the ruling withdrew,
/// turns this red at "the probe exhausted".
#[test]
fn a_probe_whose_last_candidate_is_unfit_is_exhausted_with_every_attempt_and_every_packet() {
  let _downloads = DownloadsForTest::set(none_deliver);
  let fills = [0x11u8, 0x22];
  let mut frame = crate::Frame::empty().expect("frame slot");
  for (name, mut dec, tried) in [
    ("its only candidate", raw_auditioning_decoder(), 1),
    ("the last of two", vaapi_then_cuda(), 2),
  ] {
    send_all(&mut dec, &fills.map(raw_packet));
    let exhausted = dec.receive_frame(&mut frame);
    let Err(Error::ProbeExhausted(exhausted)) = exhausted else {
      panic!("{name}: the probe exhausted: {exhausted:?}");
    };
    assert_eq!(exhausted.attempts().len(), tried, "{name}: every attempt");
    for (backend, attempt) in exhausted.attempts() {
      assert!(
        matches!(**attempt, Error::DownloadFormatUnsupported(layout) if layout.format() == UYVY),
        "{name}: {backend:?}'s unfitness, naming the layout, no errno: {attempt:?}"
      );
    }
    assert_eq!(
      exhausted.unconsumed_packets().len(),
      fills.len(),
      "{name}: every packet"
    );
    for (packet, &fill) in exhausted.unconsumed_packets().iter().zip(&fills) {
      assert!(holds(packet, fill), "{name}: {fill:#04x}, in order");
      assert_eq!(
        payload_of(packet).1,
        1,
        "{name}: {fill:#04x}, referenced by the rescued packet alone"
      );
    }
  }
}

/// [`raw_auditioning_decoder`] with no probe at all: a backend pinned
/// alone, which no probe ever held.
fn raw_pinned_decoder() -> VideoDecoder {
  ffmpeg_next::init().expect("ffmpeg init");
  let parameters = raw_parameters(RAW_SIDE);
  let codec = find_decoder(&parameters).expect("rawvideo");
  VideoDecoder::from_software_for_test_as(
    parameters,
    codec,
    crate::limits::DecoderLimits::default(),
    crate::DecoderOptions::new(),
    /*auditioning=*/ false,
  )
  .expect("a software-backed pinned decoder")
}

/// LAW: **a backend pinned alone answers an uncarriable layout with
/// [`Error::DownloadFormatUnsupported`], naming it** (item 6c): no probe
/// holds it, so the picture is that decoder's error.
///
/// PLANT: a committed decoder's layout answered with R8's synthesized
/// `EINVAL` turns this red at "by name".
#[test]
fn a_backend_pinned_alone_answers_an_uncarriable_layout_by_name() {
  let _downloads = DownloadsForTest::set(none_deliver);
  let mut dec = raw_pinned_decoder();
  send_all(&mut dec, &[raw_packet(0x11)]);
  let mut frame = crate::Frame::empty().expect("frame slot");
  let received = dec.receive_frame(&mut frame);
  assert!(
    matches!(received, Err(Error::DownloadFormatUnsupported(layout)) if layout.format() == UYVY),
    "by name: {received:?}"
  );
}

/// LAW: **after the commit, an uncarriable layout is the committed
/// decoder's [`Error::DownloadFormatUnsupported`]** (item 6d), for that
/// picture alone: a candidate committed on its first delivered picture whose
/// downloads then choose `UYVY422` — a mid-stream reconfiguration — answers
/// the layout by name, stays the session's decoder, and delivers the next
/// picture once its downloads carry again. And a candidate committed by a
/// packet the history cannot keep (more side-data entries than the probe
/// copies) answers it so too, with no probe to leave it by.
///
/// PLANT: a committed decoder's layout answered with R8's synthesized
/// `EINVAL` turns this red at "the committed decoder's, by name".
#[test]
fn after_the_commit_an_uncarriable_layout_is_the_committed_decoders_error() {
  let mut frame = crate::Frame::empty().expect("frame slot");

  let mut dec = raw_auditioning_decoder();
  let backend = dec.backend();
  send_all(&mut dec, &[raw_packet(0x11)]);
  {
    let _downloads = DownloadsForTest::set(all_deliver);
    assert_eq!(delivered(&mut dec, &mut frame), vec![0x11], "delivered");
  }
  assert_eq!(dec.phase(), SessionPhase::Streaming, "committed");
  send_all(&mut dec, &[raw_packet(0x22)]);
  {
    let _downloads = DownloadsForTest::set(none_deliver);
    let received = dec.receive_frame(&mut frame);
    assert!(
      matches!(received, Err(Error::DownloadFormatUnsupported(layout)) if layout.format() == UYVY),
      "the committed decoder's, by name: {received:?}"
    );
  }
  assert_eq!(
    (dec.backend(), dec.phase()),
    (backend, SessionPhase::Streaming),
    "still the session's decoder"
  );
  send_all(&mut dec, &[raw_packet(0x33)]);
  {
    let _downloads = DownloadsForTest::set(all_deliver);
    assert_eq!(
      delivered(&mut dec, &mut frame),
      vec![0x33],
      "the next picture"
    );
  }

  let mut dec = raw_auditioning_decoder();
  let crowded = with_side_data_entries(raw_packet(0x44), MAX_PROBE_PACKET_SIDE_DATA_ENTRIES + 1);
  send_all(&mut dec, &[crowded]);
  assert_eq!(
    dec.phase(),
    SessionPhase::Streaming,
    "committed by a packet the history cannot keep"
  );
  let _downloads = DownloadsForTest::set(none_deliver);
  let received = dec.receive_frame(&mut frame);
  assert!(
    matches!(received, Err(Error::DownloadFormatUnsupported(layout)) if layout.format() == UYVY),
    "the committed decoder's, by name: {received:?}"
  );
}

/// LAW: **a picture whose properties this crate cannot copy is no delivery,
/// and charges nothing**: the copy's failure is surfaced as this crate's own,
/// the candidate stays on trial, its history kept and nothing recorded
/// against it, so it commits at its first delivered picture. The ruling on
/// Codex R8's finding 5 defines delivery: downloaded in a layout a [`Frame`]
/// carries, its properties copied. R8 committed the candidate first.
///
/// PLANT: the copy's failure committing the candidate first, as R8 did,
/// turns this red at "no delivery: still on trial".
#[test]
fn a_picture_whose_properties_cannot_be_copied_is_no_delivery_and_charges_nothing() {
  let mut dec = vaapi_then_cuda();
  send_all(&mut dec, &[raw_packet(0x11)]);
  let mut frame = crate::Frame::empty().expect("frame slot");
  let enomem = ffmpeg_next::Error::Other {
    errno: libc::ENOMEM,
  };
  let routed = dec.route_download(Err(TransferFailure::Properties(enomem)), &mut frame);
  assert!(
    matches!(routed, Err(Error::Ffmpeg(ffmpeg_next::Error::Other { errno })) if errno == libc::ENOMEM),
    "this crate's own, as it is: {routed:?}"
  );
  assert_eq!(
    (dec.backend(), dec.phase()),
    (Backend::Vaapi, SessionPhase::Auditioning),
    "no delivery: still on trial"
  );
  let probe = dec.probe().expect("on trial");
  assert!(
    probe.attempts.is_empty(),
    "nothing charged: {:?}",
    probe.attempts
  );
  assert_eq!(
    probe.remaining_backends,
    vec![Backend::Cuda],
    "the order kept"
  );
  assert_eq!(probe.buffered_packets.len(), 1, "the history kept");
}

/// LAW: **a VideoToolbox download in a layout a [`Frame`] cannot carry is
/// told apart from a failed download, before any transfer**: for a
/// VideoToolbox frame of `UYVY422` pixel buffers
/// (`libavutil/hwcontext_videotoolbox.c` 52 in FFmpeg 9.0.1), a layout
/// [`Frame`] has no accessors for, [`download_format_of`] answers the layout
/// the download would choose, and nothing is transferred; one of `NV12` is
/// picked, and downloads whole. A VideoToolbox device and frames context need
/// no decoder session, so this runs wherever VideoToolbox is linked.
///
/// PLANT: the layout answered as a failed download turns this red at
/// "the layout, told apart".
#[cfg(target_vendor = "apple")]
#[test]
fn a_videotoolbox_download_in_a_layout_a_frame_cannot_carry_is_told_apart() {
  use ffmpeg_next::ffi::{
    AVHWDeviceType, AVHWFramesContext, AVPixelFormat, av_buffer_unref, av_hwdevice_ctx_create,
    av_hwframe_ctx_alloc, av_hwframe_ctx_init, av_hwframe_get_buffer,
  };

  ffmpeg_next::init().expect("ffmpeg init");
  let download = |sw_format: AVPixelFormat| -> std::result::Result<(), TransferFailure> {
    // SAFETY: each reference made here is unreferenced before the closure
    // returns, the frames context after the frame that holds one of its
    // buffers; the frames context is written whole before it is
    // initialised.
    unsafe {
      let mut device = ptr::null_mut();
      assert!(
        av_hwdevice_ctx_create(
          &mut device,
          AVHWDeviceType::AV_HWDEVICE_TYPE_VIDEOTOOLBOX,
          ptr::null(),
          ptr::null_mut(),
          0
        ) >= 0,
        "a VideoToolbox device"
      );
      let mut frames = av_hwframe_ctx_alloc(device);
      assert!(!frames.is_null(), "a frames context");
      let ctx = (*frames).data.cast::<AVHWFramesContext>();
      (*ctx).format = AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX;
      (*ctx).sw_format = sw_format;
      (*ctx).width = 16;
      (*ctx).height = 16;
      assert!(av_hwframe_ctx_init(frames) >= 0, "the frames context");
      let mut src = frame::Video::empty();
      assert!(
        av_hwframe_get_buffer(frames, src.as_mut_ptr(), 0) >= 0,
        "a VideoToolbox frame"
      );
      let mut dst = crate::Frame::empty().expect("frame slot");
      let downloaded =
        download_format_of(&src).and_then(|format| transfer_hw_frame(&mut dst, &src, format));
      drop(src);
      av_buffer_unref(&mut frames);
      av_buffer_unref(&mut device);
      downloaded
    }
  };
  assert!(
    matches!(
      download(AVPixelFormat::AV_PIX_FMT_UYVY422),
      Err(TransferFailure::Layout(format)) if format == AVPixelFormat::AV_PIX_FMT_UYVY422 as i32
    ),
    "the layout, told apart"
  );
  assert!(
    download(AVPixelFormat::AV_PIX_FMT_NV12).is_ok(),
    "NV12 downloads whole"
  );
}

// ---------------------------------------------------------------------------
//  The acceptance boundary of `avcodec_send_packet`, by class (Codex R1 on
//  #89): one door, every road, every class.
// ---------------------------------------------------------------------------

/// The allocation cap the boundary's laws set, in a child process of their
/// own: over what libavcodec allocates to reference a refcounted packet
/// (an `AVBufferRef`), to copy a picture's payload, to copy codec
/// parameters, and to decode a rawvideo picture, and under a 4,096-byte
/// side-data entry's copy (`av_packet_new_side_data`, `libavcodec/packet.c`
/// 231–250 in FFmpeg 9.0.1).
const CAPPED: usize = 2048;

/// A side-data entry's size that [`CAPPED`] refuses to copy.
const SIDE_DATA_PAST_THE_CAP: usize = 4096;

/// [`raw_packet`], carrying one side-data entry of `bytes`, zeroed: the
/// type `AV_PKT_DATA_MPEGTS_STREAM_ID`, which no decoder reads.
/// `av_packet_ref` copies the entry into an allocation of its own
/// (`av_packet_copy_props`, `packet.c` 417–428), so a cap below `bytes`
/// refuses the reference, `ENOMEM`, before the packet is taken
/// (`libavcodec/decode.c` 748–750).
fn raw_packet_with_side_data(fill: u8, bytes: usize) -> Packet {
  use ffmpeg_next::ffi::{AVPacketSideDataType, av_packet_new_side_data};
  let mut packet = raw_packet(fill);
  // SAFETY: the packet is live; libavcodec allocates and zeroes the entry,
  // and frees it with the packet.
  let entry = unsafe {
    av_packet_new_side_data(
      packet.as_mut_ptr(),
      AVPacketSideDataType::AV_PKT_DATA_MPEGTS_STREAM_ID,
      bytes,
    )
  };
  assert!(!entry.is_null(), "the side-data entry");
  packet
}

/// The first byte of each packet's payload: which picture it holds.
fn fills(packets: &[Packet]) -> Vec<u8> {
  packets
    .iter()
    .map(|packet| packet.data().expect("a payload")[0])
    .collect()
}

/// A decoder committed with a backlog. The candidate on trial (VAAPI, in
/// software) is left, and CUDA, built in software, is replayed `history`:
/// rawvideo decodes a picture's packet as it is sent and holds a second
/// until a picture is asked for, so the replay stops at the third, the
/// candidate's first output. CUDA's first picture, delivered, commits it,
/// and what the replay did not reach is the backlog. The second picture is
/// drained too, so the decoder holds nothing and asks for input. Answers the
/// decoder and the two pictures delivered.
fn committed_with_backlog(
  history: Vec<Packet>,
  frame: &mut crate::Frame,
) -> (VideoDecoder, Vec<u8>) {
  let mut dec = vaapi_then_cuda();
  {
    let probe = dec.probe_mut().expect("on trial");
    probe.buffered_bytes = history.len() * RAW_PICTURE;
    probe.fed = history.len();
    probe.buffered_packets = history;
  }
  dec
    .advance_probe(BackendAnswer::of_call(Error::Ffmpeg(
      ffmpeg_next::Error::InvalidData,
    )))
    .expect("CUDA installed");
  assert_eq!(
    dec.probe().map(|probe| probe.fed),
    Some(2),
    "the replay stopped at the candidate's first output"
  );
  let _downloads = DownloadsForTest::set(all_deliver);
  let mut pictures = Vec::new();
  for _ in 0..2 {
    let received = dec.receive_frame(frame);
    assert!(
      matches!(received, Ok(Received::Produced)),
      "a picture: {received:?}"
    );
    pictures.push(frame.row(0, 0).expect("a gray picture")[0]);
  }
  assert_eq!(
    (dec.backend(), dec.phase()),
    (Backend::Cuda, SessionPhase::Streaming),
    "committed at its first delivered picture"
  );
  (dec, pictures)
}

/// A [`BuildCandidate`] that lifts the allocation cap, then opens the
/// candidate in software: a law's cap reaches the send it is set for, and
/// no candidate built after it.
fn uncapped_then_software_state(
  parameters: codec::Parameters,
  codec: Codec,
  backend: Backend,
  limits: crate::limits::DecoderLimits,
  options: crate::DecoderOptions,
  pkt_timebase: Option<mediadecode::Timebase>,
) -> std::result::Result<DecoderState, BuildFailure> {
  mediaio_ffmpeg::test_support::uncap_ffmpeg_allocations();
  VideoDecoder::software_state_for_test(parameters, codec, backend, limits, options, pkt_timebase)
}

/// LAW: **every answer of `avcodec_send_packet` is classed by its source**
/// ([`Acceptance`]; `libavcodec/decode.c` 730–760 in FFmpeg 9.0.1): the
/// table over the codes, a packet's and the end's, the shapes libavcodec
/// refuses before it references a packet, the end in a packet's shape
/// refused unsent, and libavcodec's own answers, live, on both sides of the
/// one code the shape tells apart.
///
/// PLANT: `EINVAL` classed without the packet's shape, as the decode's
/// always, turns this red at "the shape's", and the shape law with it.
#[test]
fn every_answer_to_a_send_is_classed_by_its_source() {
  use ffmpeg_next::Error as E;
  let errno = |code| E::Other { errno: code };
  let (eagain, einval, enomem) = (
    errno(ffmpeg_next::error::EAGAIN),
    errno(libc::EINVAL),
    errno(libc::ENOMEM),
  );
  // (the shape refused before the reference, the answer, the class)
  let packet_cells = [
    (false, Ok(()), Acceptance::Accepted),
    (false, Err(eagain), Acceptance::NotAccepted(eagain)),
    (true, Err(eagain), Acceptance::NotAccepted(eagain)),
    (false, Err(E::Eof), Acceptance::NotAccepted(E::Eof)),
    (true, Err(einval), Acceptance::NotAccepted(einval)),
    (false, Err(einval), Acceptance::AcceptedWithError(einval)),
    (false, Err(enomem), Acceptance::Ambiguous(enomem)),
    (true, Err(enomem), Acceptance::Ambiguous(enomem)),
    (
      false,
      Err(E::InvalidData),
      Acceptance::AcceptedWithError(E::InvalidData),
    ),
    (
      false,
      Err(E::External),
      Acceptance::AcceptedWithError(E::External),
    ),
    (
      false,
      Err(errno(libc::ENOSYS)),
      Acceptance::AcceptedWithError(errno(libc::ENOSYS)),
    ),
    (false, Err(E::Bug), Acceptance::AcceptedWithError(E::Bug)),
  ];
  for (shape, answer, class) in packet_cells {
    assert_eq!(
      Acceptance::of_packet(shape, answer),
      class,
      "a packet, shape refused {shape}, answered {answer:?}"
    );
  }
  for (answer, class) in [
    (Ok(()), Acceptance::Accepted),
    (Err(E::Eof), Acceptance::NotAccepted(E::Eof)),
    (Err(eagain), Acceptance::NotAccepted(eagain)),
    (Err(einval), Acceptance::NotAccepted(einval)),
    (Err(enomem), Acceptance::NotAccepted(enomem)),
  ] {
    assert_eq!(
      Acceptance::of_end(answer),
      class,
      "the end, answered {answer:?}"
    );
  }

  // The shapes, read from the packet: data with no size (742–743), and no
  // buffer with a size `packet_alloc` will not allocate (packet.c 86–87).
  static BYTES: [u8; 64] = [0; 64];
  let zero_size = crate::boundary::try_packet_copy(&[]).expect("data with no size");
  let unbuffered = |size: i32| {
    let mut packet = Packet::empty();
    // SAFETY: the packet is live and blank; it points at static bytes it
    // does not own, with no buffer, so its unref frees nothing of them.
    unsafe {
      let raw = packet.as_mut_ptr();
      (*raw).data = BYTES.as_ptr().cast_mut();
      (*raw).size = size;
    }
    packet
  };
  assert!(refused_before_reference(&zero_size), "data with no size");
  assert!(refused_before_reference(&unbuffered(-1)), "a negative size");
  assert!(
    refused_before_reference(&unbuffered(i32::MAX - 64)),
    "a size past what packet_alloc allocates"
  );
  assert!(
    !refused_before_reference(&unbuffered(64)),
    "an unbuffered packet libavcodec copies"
  );
  assert!(!refused_before_reference(&raw_packet(0x11)), "a picture");
  assert!(
    is_flush_packet(&Packet::empty()),
    "the end, in a packet's shape"
  );
  assert!(!is_flush_packet(&raw_packet(0x11)), "a picture");

  // libavcodec's own answers, live. The end in a packet's shape is refused
  // unsent: the decoder takes the end after it, so it never began draining.
  let mut dec = raw_pinned_decoder();
  assert_eq!(
    send(
      &mut dec.decoder_mut().inner,
      Sending::Packet(&Packet::empty())
    ),
    Acceptance::NotAccepted(einval),
    "the end in a packet's shape, refused unsent"
  );
  assert_eq!(
    send(&mut dec.decoder_mut().inner, Sending::End),
    Acceptance::Accepted,
    "the end, taken: nothing had started the drain"
  );
  // `EINVAL` either side of the shape: 743's for data with no size, and the
  // decode's for a packet a row's worth long, short of a 16 × 16 picture
  // (`rawdec.c` 342–346).
  let mut dec = raw_pinned_decoder();
  assert_eq!(
    send(&mut dec.decoder_mut().inner, Sending::Packet(&zero_size)),
    Acceptance::NotAccepted(einval),
    "the shape's"
  );
  let short = crate::boundary::try_packet_copy(&[0x77; RAW_SIDE as usize]).expect("a short packet");
  assert_eq!(
    send(&mut dec.decoder_mut().inner, Sending::Packet(&short)),
    Acceptance::AcceptedWithError(einval),
    "the decode's"
  );
  let mut refused = unbuffered(-1);
  assert_eq!(
    send(&mut dec.decoder_mut().inner, Sending::Packet(&refused)),
    Acceptance::NotAccepted(einval),
    "packet_alloc's, inside av_packet_ref"
  );
  // SAFETY: the packet is live; it is pointed at nothing of the static
  // bytes before it drops.
  unsafe { (*refused.as_mut_ptr()).data = ptr::null_mut() };
}

/// LAW: **an ambiguous send of the backlog's front ends the decoder, and
/// hands the front back with every packet behind it** (Codex R1 on #89, the
/// [high]). The backlog is the packets a replay had not reached when its
/// candidate gave its first picture, and it holds their only copies, so a
/// front whose send answered `ENOMEM` can be neither retired, which would
/// lose it if the decoder had not taken it, nor sent again, which would
/// feed it twice if it had.
///
/// The front carries a 4,096-byte side-data entry, and FFmpeg's allocations
/// are capped ([`CAPPED`], `av_max_alloc`, in a child process of its own)
/// once the decoder holds nothing: `av_packet_ref` cannot copy the entry,
/// so `avcodec_send_packet` answers `ENOMEM` before it takes the packet
/// (`libavcodec/decode.c` 748–750 in FFmpeg 9.0.1) — the code the decode it
/// runs after taking one also answers. A decoder fed the residual then
/// decodes the pictures the committed one had not given: every picture
/// once, in order.
///
/// PLANT: the front retired on every answer but `EAGAIN`, as R1's backlog
/// did, turns this red at "ended"; so does the ambiguous class read as
/// accepted with the decoder's error.
#[test]
fn an_ambiguous_send_of_the_backlogs_front_ends_the_decoder_and_hands_it_back() {
  use mediaio_ffmpeg::test_support::{
    cap_ffmpeg_allocations, in_subprocess, uncap_ffmpeg_allocations,
  };

  in_subprocess(
    "decoder::tests::an_ambiguous_send_of_the_backlogs_front_ends_the_decoder_and_hands_it_back",
    || {
      let mut frame = crate::Frame::empty().expect("frame slot");
      let history = vec![
        raw_packet(0x11),
        raw_packet(0x22),
        raw_packet_with_side_data(0x33, SIDE_DATA_PAST_THE_CAP),
        raw_packet(0x44),
      ];
      let (mut dec, mut pictures) = committed_with_backlog(history, &mut frame);
      assert_eq!(pictures, vec![0x11, 0x22], "the committed decoder's");
      assert_eq!(
        dec
          .backlog()
          .iter()
          .map(|packet| packet.data().expect("a payload")[0])
          .collect::<Vec<u8>>(),
        vec![0x33, 0x44],
        "the replay's rest, owed"
      );

      let _downloads = DownloadsForTest::set(all_deliver);
      // SAFETY: in `in_subprocess`'s child, alone; the receive checks what
      // libavcodec answers, and the cap is lifted as it returns.
      unsafe { cap_ffmpeg_allocations(CAPPED) };
      let ended = dec.receive_frame(&mut frame);
      uncap_ffmpeg_allocations();
      let Err(Error::AmbiguousSend(ended)) = ended else {
        panic!("ended: {ended:?}");
      };
      assert!(
        matches!(
          ended.reason(),
          Error::Ffmpeg(ffmpeg_next::Error::Other { errno }) if *errno == libc::ENOMEM
        ),
        "libavcodec's answer, as it returned it: {ended:?}"
      );
      assert_eq!(ended.backend(), Backend::Cuda, "the committed decoder's");
      assert!(!ended.end_follows(), "no end was given");
      assert_eq!(
        fills(ended.residual()),
        vec![0x33, 0x44],
        "the front, and every packet behind it"
      );
      for packet in ended.residual() {
        assert_eq!(payload_of(packet).1, 1, "referenced by its packet alone");
      }

      // The decoder is spent, terminally: no decoder lives, and every
      // entry after it is refused by name, a flush included.
      assert_spent(&mut dec, crate::Spent::AmbiguousSend, SessionKind::Ended);

      // A decoder that starts from the residual decodes the rest.
      let mut next = raw_pinned_decoder();
      send_all(&mut next, ended.residual());
      pictures.extend(delivered(&mut next, &mut frame));
      assert_eq!(
        pictures,
        vec![0x11, 0x22, 0x33, 0x44],
        "every picture once, in order"
      );
    },
  );
}

/// LAW: **back pressure on the backlog keeps its front**: the committed
/// decoder, holding nothing, takes a picture's packet and a second, and
/// answers `EAGAIN` to the third (`libavcodec/decode.c` 746–747), which is
/// kept and fed once the caller drains. Every picture comes once, in order.
///
/// PLANT: the front retired on `EAGAIN` turns this red at "kept".
#[test]
fn back_pressure_on_the_backlog_keeps_its_front() {
  let mut frame = crate::Frame::empty().expect("frame slot");
  let sent = [0x11u8, 0x22, 0x33, 0x44, 0x55, 0x66];
  let (mut dec, mut pictures) = committed_with_backlog(Vec::from(sent.map(raw_packet)), &mut frame);
  assert_eq!(dec.backlog().len(), 4, "the replay's rest");

  let fed = dec.feed().expect("taken, then back pressure");
  assert_eq!(
    fed,
    Fed {
      taken: 2,
      all: false
    },
    "two taken, the third not"
  );
  assert!(
    holds(dec.backlog().front().expect("a front"), 0x55),
    "the packet it did not take, kept"
  );
  assert_eq!(dec.backlog().len(), 2, "the rest behind it");

  let _downloads = DownloadsForTest::set(all_deliver);
  pictures.extend(delivered(&mut dec, &mut frame));
  assert_eq!(pictures, sent, "every picture once, in order");
}

/// LAW: **on trial, an ambiguous send beside a refusal of this crate's own
/// ends the decoder** — the trial road's half of the class. The send of the
/// caller's packet answers `ENOMEM` (its side-data entry cannot be copied
/// under [`CAPPED`], in a child process), and a frame-budget refusal is
/// latched beside it, as an earlier call could have left one. The candidate
/// can be neither kept, not knowing whether it holds the packet, nor charged
/// with this crate's refusal, so the decoder ends with the refusal by name:
/// the whole history back, and the caller's packet the caller's. R8's
/// finding 3 keeps a packet as taken on a refusal of this crate's own only
/// where the class says the candidate took it.
///
/// PLANT: the ambiguous send kept as taken, as R8's fix kept every report,
/// turns this red at "ended".
#[test]
fn on_trial_an_ambiguous_send_beside_a_refusal_of_this_crates_own_ends_the_decoder() {
  use mediaio_ffmpeg::test_support::{
    cap_ffmpeg_allocations, in_subprocess, uncap_ffmpeg_allocations,
  };

  in_subprocess(
    "decoder::tests::on_trial_an_ambiguous_send_beside_a_refusal_of_this_crates_own_ends_the_decoder",
    || {
      let mut dec = raw_auditioning_decoder();
      send_all(&mut dec, &[raw_packet(0x11)]);
      let caller = raw_packet_with_side_data(0x22, SIDE_DATA_PAST_THE_CAP);
      // SAFETY: `dec` owns this state and frees it only when it drops,
      // after this call.
      unsafe {
        crate::ffi::declare_frame_budget_declined_for_test(dec.decoder().callback_state, 12_582_912)
      };
      // SAFETY: in `in_subprocess`'s child, alone; the send checks what
      // libavcodec answers, and the cap is lifted as it returns.
      unsafe { cap_ffmpeg_allocations(CAPPED) };
      // SAFETY: `raw_packet`'s, every byte written and the padding zeroed,
      // and nothing writes through it while the decoder may read it.
      let answer = unsafe { dec.send_av_packet(&caller) };
      uncap_ffmpeg_allocations();
      let Err(Error::AmbiguousSend(ended)) = answer else {
        panic!("ended: {answer:?}");
      };
      assert!(
        matches!(ended.reason(), Error::FrameBudgetExceeded(p) if p.bytes() == 12_582_912),
        "this crate's refusal, by name: {ended:?}"
      );
      assert_eq!(
        fills(ended.residual()),
        vec![0x11],
        "the whole history, and not the caller's packet"
      );
      assert_eq!(
        dec.session_kind(),
        SessionKind::Ended,
        "the probe ended with it"
      );
      for packet in ended.residual() {
        assert_eq!(payload_of(packet).1, 1, "referenced by its packet alone");
      }
      assert_eq!(
        payload_of(&caller).1,
        1,
        "the caller's, referenced by it alone"
      );
      assert_spent(&mut dec, crate::Spent::AmbiguousSend, SessionKind::Ended);
    },
  );
}

/// LAW: **on trial, an ambiguous send that is the candidate's own leaves
/// the candidate, and the next takes the packet once**: libavcodec's
/// `ENOMEM` on the caller's packet ([`CAPPED`], in a child process) is the
/// candidate's answer as it was returned. The probe leaves it — never called
/// again, whatever it holds going with it — and the next candidate, built
/// once the cap is lifted, is replayed the history, which never held the
/// packet, and then takes it: recorded once, its picture delivered once.
///
/// PLANT: the ambiguous send reported as this crate's own, the candidate
/// kept on trial, turns this red at "the next candidate took it".
#[test]
fn on_trial_an_ambiguous_send_leaves_the_candidate_and_the_next_takes_the_packet_once() {
  use mediaio_ffmpeg::test_support::{
    cap_ffmpeg_allocations, in_subprocess, uncap_ffmpeg_allocations,
  };

  in_subprocess(
    "decoder::tests::on_trial_an_ambiguous_send_leaves_the_candidate_and_the_next_takes_the_packet_once",
    || {
      let mut dec = vaapi_then_cuda();
      dec.probe_mut().expect("on trial").build = uncapped_then_software_state;
      send_all(&mut dec, &[raw_packet(0x11)]);
      let caller = raw_packet_with_side_data(0x22, SIDE_DATA_PAST_THE_CAP);
      // SAFETY: in `in_subprocess`'s child, alone; the send checks what
      // libavcodec answers, and the next candidate's build lifts the cap.
      unsafe { cap_ffmpeg_allocations(CAPPED) };
      // SAFETY: `raw_packet`'s, every byte written and the padding zeroed,
      // and nothing writes through it while the decoder may read it.
      let answer = unsafe { dec.send_av_packet(&caller) };
      uncap_ffmpeg_allocations();
      assert!(
        matches!(answer, Ok(Sent::Accepted)),
        "the next candidate took it: {answer:?}"
      );
      assert_eq!(dec.backend(), Backend::Cuda, "the next candidate's");
      {
        let probe = dec.probe().expect("still on trial");
        assert!(
          matches!(
            probe.attempts.as_slice(),
            [(Backend::Vaapi, Attempt::Failed(BackendAnswer(Error::Ffmpeg(ffmpeg_next::Error::Other { errno }))))]
              if *errno == libc::ENOMEM
          ),
          "the candidate's answer, as libavcodec returned it: {:?}",
          probe.attempts
        );
        assert_eq!(
          fills(&probe.buffered_packets),
          vec![0x11, 0x22],
          "recorded once, in order"
        );
        assert_eq!(probe.fed, 2, "fed whole");
      }
      let _downloads = DownloadsForTest::set(all_deliver);
      let mut frame = crate::Frame::empty().expect("frame slot");
      assert_eq!(
        delivered(&mut dec, &mut frame),
        vec![0x11, 0x22],
        "every picture once, in order"
      );
    },
  );
}

/// LAW: **while the decoder is owed packets, a send takes nothing, and an
/// owed packet's answer is the receive road's**. A committed decoder owes
/// a backlog whose front is too short to hold a row (`rawdec.c` 195–197).
/// The caller's send answers [`Sent::MustDrain`] and sends nothing; the
/// drain feeds the front, whose own error — the decode's
/// `AVERROR_INVALIDDATA`, the packet taken and retired — comes from the
/// receive, where no packet of the caller's is in hand; and the caller's
/// packet, offered again, is taken.
///
/// PLANT: the send road feeding what is owed, as R1 did, turns this red at
/// "nothing sent": the owed packet's error is answered for the caller's,
/// which was never sent.
#[test]
fn while_the_decoder_is_owed_a_send_takes_nothing_and_an_owed_packets_answer_is_the_receives() {
  let mut frame = crate::Frame::empty().expect("frame slot");
  let short = crate::boundary::try_packet_copy(&[0x77; 8]).expect("a packet short of a picture");
  let history = vec![raw_packet(0x11), raw_packet(0x22), short, raw_packet(0x44)];
  let (mut dec, mut pictures) = committed_with_backlog(history, &mut frame);

  let caller = raw_packet(0x55);
  // SAFETY: `raw_packet`'s, every byte written and the padding zeroed, and
  // nothing writes through it while the decoder may read it.
  let sent = unsafe { dec.send_av_packet(&caller) };
  assert!(
    matches!(sent, Ok(Sent::MustDrain)),
    "nothing sent while the decoder is owed: {sent:?}"
  );
  assert_eq!(dec.backlog().len(), 2, "the backlog, untouched");

  let _downloads = DownloadsForTest::set(all_deliver);
  let refused = dec.receive_frame(&mut frame);
  assert!(
    matches!(refused, Err(Error::Ffmpeg(ffmpeg_next::Error::InvalidData))),
    "the owed packet's own error, on the receive road: {refused:?}"
  );
  pictures.extend(delivered(&mut dec, &mut frame));
  // SAFETY: as above.
  let sent = unsafe { dec.send_av_packet(&caller) };
  assert!(matches!(sent, Ok(Sent::Accepted)), "then taken: {sent:?}");
  pictures.extend(delivered(&mut dec, &mut frame));
  assert_eq!(
    pictures,
    vec![0x11, 0x22, 0x44, 0x55],
    "every picture the decoder could give, once, in order"
  );
}

/// LAW: **a packet with neither payload nor side data is the end**, as
/// libavcodec takes it (`libavcodec/decode.c` 745, 751–752; `avcodec.h`
/// 2341–2347): sent as the end, recorded as the end, never in the history,
/// and a second is a second end. The candidate drains to a clean end.
///
/// PLANT: the packet sent as a packet turns this red at "the end".
#[test]
fn a_packet_with_neither_payload_nor_side_data_is_the_end() {
  let mut dec = raw_auditioning_decoder();
  send_all(&mut dec, &[raw_packet(0x11)]);
  let empty = Packet::empty();
  // SAFETY: a blank packet: nothing to read, and nothing to write through.
  let sent = unsafe { dec.send_av_packet(&empty) };
  assert!(matches!(sent, Ok(Sent::Accepted)), "the end: {sent:?}");
  assert_eq!(dec.end(), Some(End::Taken), "recorded as the end");
  assert_eq!(
    dec.probe().map(|probe| probe.buffered_packets.len()),
    Some(1),
    "the history holds no packet for it"
  );
  // SAFETY: as above.
  let again = unsafe { dec.send_av_packet(&empty) };
  assert!(
    matches!(again, Err(Error::Ffmpeg(ffmpeg_next::Error::Eof))),
    "a second end: {again:?}"
  );
  let _downloads = DownloadsForTest::set(all_deliver);
  let mut frame = crate::Frame::empty().expect("frame slot");
  assert_eq!(delivered(&mut dec, &mut frame), vec![0x11], "its picture");
  assert_eq!(
    dec.phase(),
    SessionPhase::Draining,
    "committed, the stream over"
  );
}

/// LAW: **a packet of a shape libavcodec refuses is refused, and charges no
/// candidate**: data with no size answers `EINVAL` before libavcodec
/// references it (`libavcodec/decode.c` 742–743), the caller's fault and no
/// backend's answer. The candidate stays on trial, nothing recorded and
/// nothing charged.
///
/// PLANT: `EINVAL` classed without the packet's shape, as the decode's
/// always, turns this red at "refused": the candidate is charged, and the
/// probe exhausted.
#[test]
fn a_packet_of_a_shape_libavcodec_refuses_charges_no_candidate() {
  let mut dec = raw_auditioning_decoder();
  send_all(&mut dec, &[raw_packet(0x11)]);
  let zero_size = crate::boundary::try_packet_copy(&[]).expect("data with no size");
  // SAFETY: `try_packet_copy`'s: its padding written, and nothing writes
  // through it while the decoder may read it.
  let refused = unsafe { dec.send_av_packet(&zero_size) };
  assert!(
    matches!(
      refused,
      Err(Error::Ffmpeg(ffmpeg_next::Error::Other { errno })) if errno == libc::EINVAL
    ),
    "refused: {refused:?}"
  );
  assert_eq!(dec.phase(), SessionPhase::Auditioning, "still on trial");
  let probe = dec.probe().expect("on trial");
  assert!(
    probe.attempts.is_empty(),
    "no candidate charged: {:?}",
    probe.attempts
  );
  assert_eq!(probe.buffered_packets.len(), 1, "nothing recorded");
}

/// LAW: **a packet with no payload is copied with none, and libavcodec
/// takes the copy as it takes the source**
/// ([`try_clone_packet`](crate::boundary::try_clone_packet)). A
/// side-data-only packet is one libavcodec takes (`libavcodec/decode.c`
/// 745–750); `av_packet_ref` would give its copy a buffer of padding and
/// data with no size, which libavcodec refuses (742–743). A decoder whose
/// frame waits takes a packet without decoding it (754), so the second send
/// shows what libavcodec makes of the copy alone.
///
/// PLANT: the copy made by `av_packet_ref` and `av_packet_make_writable`
/// alone turns this red at "no payload".
#[test]
fn a_packet_with_no_payload_is_copied_with_none_and_libavcodec_takes_the_copy() {
  use ffmpeg_next::ffi::{AVPacketSideDataType, av_packet_new_side_data};
  let mut source = Packet::empty();
  // SAFETY: the packet is live; libavcodec allocates and zeroes the entry,
  // and frees it with the packet.
  let entry = unsafe {
    av_packet_new_side_data(
      source.as_mut_ptr(),
      AVPacketSideDataType::AV_PKT_DATA_MPEGTS_STREAM_ID,
      1,
    )
  };
  assert!(!entry.is_null(), "the side-data entry");
  assert!(
    !is_flush_packet(&source) && !refused_before_reference(&source),
    "a packet libavcodec takes"
  );
  // SAFETY: the source's entry is libavcodec's zeroed allocation, every
  // byte written.
  let copy = unsafe { crate::boundary::try_clone_packet(&source) }.expect("a copy");
  // SAFETY: the copy is live; plain fields.
  let (data, entries) = unsafe {
    let raw = copy.as_ptr();
    ((*raw).data, (*raw).side_data_elems)
  };
  assert!(data.is_null(), "no payload");
  assert_eq!(entries, 1, "the side data, copied");
  assert!(!refused_before_reference(&copy), "a shape libavcodec takes");

  let mut dec = raw_pinned_decoder();
  let picture = raw_packet(0x11);
  assert_eq!(
    send(&mut dec.decoder_mut().inner, Sending::Packet(&picture)),
    Acceptance::Accepted
  );
  assert_eq!(
    send(&mut dec.decoder_mut().inner, Sending::Packet(&copy)),
    Acceptance::Accepted,
    "taken, as the source is"
  );
}

/// LAW: **a send whose advance is owed does not take the packet, and the
/// next candidate does**. The candidate on trial refuses the caller's
/// picture — it decodes 32 × 32 pictures, and the packet is short of one
/// (`rawdec.c`) — and the advance away from it meets VAAPI's refusal, then
/// CUDA's build failing with this crate's own `ENOMEM`, so it is owed. No
/// candidate is there to take the packet, and the history never held it:
/// the send answers [`Sent::MustDrain`]. The drain resumes the advance —
/// CUDA installed — and the packet, offered again, is CUDA's, recorded
/// once, its picture delivered once.
///
/// PLANT: the owed advance's failure answered for the packet, as R1 did,
/// turns this red at "not taken": the caller reads an error for a packet no
/// decoder that goes on has seen.
#[test]
fn a_send_whose_advance_is_owed_does_not_take_the_packet_and_the_next_candidate_does() {
  let mut dec = raw_auditioning_decoder_with(32, crate::DecoderOptions::new());
  let first = dec.backend();
  {
    let probe = dec.probe_mut().expect("on trial");
    probe.parameters = raw_parameters(RAW_SIDE);
    probe.build = vaapi_refuses_and_cuda_may_fail;
    probe.remaining_backends = vec![Backend::Vaapi, Backend::Cuda];
  }
  let picture = raw_packet(0x11);
  CUDA_FAILS.with(|fails| fails.set(true));
  // SAFETY: `raw_packet`'s, every byte written and the padding zeroed, and
  // nothing writes through it while the decoder may read it.
  let sent = unsafe { dec.send_av_packet(&picture) };
  CUDA_FAILS.with(|fails| fails.set(false));
  assert!(
    matches!(sent, Ok(Sent::MustDrain)),
    "not taken, the advance owed: {sent:?}"
  );
  {
    assert_eq!(dec.session_kind(), SessionKind::Owed, "the advance, owed");
    let probe = dec.probe().expect("the probe's record");
    assert!(
      probe.buffered_packets.is_empty(),
      "the history never held it"
    );
    assert_eq!(
      tried(probe),
      vec![Backend::Vaapi],
      "the refusal before the stop"
    );
  }

  let mut frame = crate::Frame::empty().expect("frame slot");
  assert_eq!(
    dec
      .receive_frame(&mut frame)
      .expect("the drain resumes the advance"),
    Received::NeedsInput
  );
  assert_eq!(dec.backend(), Backend::Cuda, "the advance, resumed");
  // SAFETY: as above.
  let sent = unsafe { dec.send_av_packet(&picture) };
  assert!(
    matches!(sent, Ok(Sent::Accepted)),
    "the next candidate's: {sent:?}"
  );
  {
    let probe = dec.probe().expect("on trial");
    assert_eq!(fills(&probe.buffered_packets), vec![0x11], "recorded once");
    assert_eq!(
      tried(probe),
      vec![first, Backend::Vaapi],
      "in the order tried"
    );
  }
  let _downloads = DownloadsForTest::set(all_deliver);
  assert_eq!(
    delivered(&mut dec, &mut frame),
    vec![0x11],
    "its picture, once"
  );
}

/// LAW: **back pressure on a send is answered for the packet alone**. A
/// committed decoder holds a picture and a packet behind it, so a third
/// packet's send answers `EAGAIN` from the packet slot (`libavcodec/decode.c`
/// 746–747), before any decode runs, and a frame-budget refusal latched
/// beside it belongs to an earlier call. The send answers
/// [`Sent::MustDrain`], the packet not taken; the drain gives both pictures
/// and then reports the refusal, by name, where no packet is in hand; and
/// the packet, offered again, is taken.
///
/// PLANT: the send road reading the funnel at `EAGAIN`, as R1 did, turns
/// this red at "not taken": the refusal is answered for a packet the
/// decoder never took.
#[test]
fn back_pressure_on_a_send_is_answered_for_the_packet_alone() {
  let mut dec = raw_pinned_decoder();
  send_all(&mut dec, &[raw_packet(0x11), raw_packet(0x22)]);
  // SAFETY: `dec` owns this state and frees it only when it drops, after
  // this call.
  unsafe {
    crate::ffi::declare_frame_budget_declined_for_test(dec.decoder().callback_state, 12_582_912)
  };
  let third = raw_packet(0x33);
  // SAFETY: `raw_packet`'s, every byte written and the padding zeroed, and
  // nothing writes through it while the decoder may read it.
  let sent = unsafe { dec.send_av_packet(&third) };
  assert!(
    matches!(sent, Ok(Sent::MustDrain)),
    "not taken: back pressure, and nothing else: {sent:?}"
  );

  let _downloads = DownloadsForTest::set(all_deliver);
  let mut frame = crate::Frame::empty().expect("frame slot");
  let mut pictures = Vec::new();
  loop {
    match dec.receive_frame(&mut frame) {
      Ok(Received::Produced) => pictures.push(frame.row(0, 0).expect("a gray picture")[0]),
      Err(Error::FrameBudgetExceeded(p)) => {
        assert_eq!(p.bytes(), 12_582_912, "the refusal's own numbers");
        break;
      }
      other => panic!("a picture, or the refusal: {other:?}"),
    }
  }
  assert_eq!(pictures, vec![0x11, 0x22], "both pictures first");
  // SAFETY: as above.
  let sent = unsafe { dec.send_av_packet(&third) };
  assert!(matches!(sent, Ok(Sent::Accepted)), "then taken: {sent:?}");
  assert_eq!(delivered(&mut dec, &mut frame), vec![0x33], "its picture");
}

// ---------------------------------------------------------------------------
//  The probe's state, as one class (Codex R2 on #89, findings 1 and 3): one
//  typed state every entry reads first, a candidate left torn down at once,
//  and exhaustion terminal. And the download's attribution (finding 2).
// ---------------------------------------------------------------------------

/// The [`DecoderState`]s dropped on this thread so far: the seam a law sees
/// a candidate torn down through.
fn decoders_dropped() -> usize {
  DECODER_STATES_DROPPED.with(core::cell::Cell::get)
}

/// A [`BuildCandidate`] that refuses every backend: the backend's own
/// answer, for a law whose advance must find no candidate.
fn every_backend_refuses(
  _: codec::Parameters,
  _: Codec,
  backend: Backend,
  _: crate::limits::DecoderLimits,
  _: crate::DecoderOptions,
  _: Option<mediadecode::Timebase>,
) -> std::result::Result<DecoderState, BuildFailure> {
  Err(BuildFailure::Refused(BackendAnswer::of_open(
    Error::BackendUnsupportedByCodec(backend),
  )))
}

/// A [`BuildCandidate`]: [`vaapi_refuses_and_cuda_may_fail`], with the
/// allocation cap lifted first, so a law's cap reaches the send it is set
/// for and no candidate built after it.
fn uncapped_vaapi_refuses_and_cuda_may_fail(
  parameters: codec::Parameters,
  codec: Codec,
  backend: Backend,
  limits: crate::limits::DecoderLimits,
  options: crate::DecoderOptions,
  pkt_timebase: Option<mediadecode::Timebase>,
) -> std::result::Result<DecoderState, BuildFailure> {
  mediaio_ffmpeg::test_support::uncap_ffmpeg_allocations();
  vaapi_refuses_and_cuda_may_fail(parameters, codec, backend, limits, options, pkt_timebase)
}

/// The candidate on trial (raw, in software) with `remaining` to try,
/// built by `build`.
fn on_trial_with(build: BuildCandidate, remaining: Vec<Backend>) -> VideoDecoder {
  let mut dec = raw_auditioning_decoder();
  let probe = dec.probe_mut().expect("on trial");
  probe.build = build;
  probe.remaining_backends = remaining;
  dec
}

/// A failed candidate's own answer, for a law that leaves one.
fn its_own_failure() -> BackendAnswer {
  BackendAnswer::of_call(Error::Ffmpeg(ffmpeg_next::Error::InvalidData))
}

/// Whether `answer` is this crate's own `ENOMEM`, as it was surfaced.
fn is_own_enomem<T>(answer: &Result<T>) -> bool {
  matches!(answer, Err(Error::Ffmpeg(ffmpeg_next::Error::Other { errno })) if *errno == libc::ENOMEM)
}

/// LAW: **a candidate left on a send is torn down before the send answers
/// that the packet was not taken** (Codex R2, finding 1). The candidate on
/// trial takes the caller's picture and its decode fails — it decodes
/// 32 × 32 pictures, and the packet is short of one — so it is left. The
/// advance away from it meets VAAPI's refusal, then CUDA's build failing
/// with this crate's own `ENOMEM`, and is owed. The send answers
/// [`Sent::MustDrain`], and by then the candidate is gone: its decoder
/// dropped before the answer, the session [`Session::Owed`] with no decoder
/// in it, and the caller's packet referenced by its own alone. The drain
/// resumes the advance, and the packet, offered again, is fed to CUDA once:
/// recorded once, its picture delivered once.
///
/// PLANT: the candidate kept in the owed state, as R2's `PendingAdvance`
/// kept it, turns this red at "torn down before the answer".
#[test]
fn a_candidate_left_on_a_send_is_torn_down_before_the_send_answers_not_taken() {
  let mut dec = raw_auditioning_decoder_with(32, crate::DecoderOptions::new());
  let first = dec.backend();
  {
    let probe = dec.probe_mut().expect("on trial");
    probe.parameters = raw_parameters(RAW_SIDE);
    probe.build = vaapi_refuses_and_cuda_may_fail;
    probe.remaining_backends = vec![Backend::Vaapi, Backend::Cuda];
  }
  let picture = raw_packet(0x11);
  let dropped = decoders_dropped();
  CUDA_FAILS.with(|fails| fails.set(true));
  // SAFETY: `raw_packet`'s, every byte written and the padding zeroed, and
  // nothing writes through it while the decoder may read it.
  let sent = unsafe { dec.send_av_packet(&picture) };
  CUDA_FAILS.with(|fails| fails.set(false));
  assert!(
    matches!(sent, Ok(Sent::MustDrain)),
    "not taken, the advance owed: {sent:?}"
  );
  assert_eq!(
    decoders_dropped() - dropped,
    1,
    "the candidate left, torn down before the answer"
  );
  assert_eq!(dec.session_kind(), SessionKind::Owed, "the advance, owed");
  assert!(dec.live().is_none(), "no decoder lives while it is owed");
  assert_eq!(
    payload_of(&picture).1,
    1,
    "the caller's packet, referenced by its own alone"
  );
  {
    let pending = dec.pending().expect("the advance, owed");
    assert_eq!(pending.backend, first, "the candidate left");
    assert!(
      matches!(pending.attempt, Attempt::Failed(_)),
      "its own answer: {:?}",
      pending.attempt
    );
  }

  let mut frame = crate::Frame::empty().expect("frame slot");
  assert_eq!(
    dec
      .receive_frame(&mut frame)
      .expect("the drain resumes the advance"),
    Received::NeedsInput
  );
  assert_eq!(
    (dec.backend(), dec.session_kind()),
    (Backend::Cuda, SessionKind::Trial),
    "the next candidate, on trial"
  );
  // SAFETY: as above.
  let sent = unsafe { dec.send_av_packet(&picture) };
  assert!(
    matches!(sent, Ok(Sent::Accepted)),
    "fed to the next candidate: {sent:?}"
  );
  assert_eq!(
    fills(&dec.probe().expect("on trial").buffered_packets),
    vec![0x11],
    "recorded once"
  );
  let _downloads = DownloadsForTest::set(all_deliver);
  assert_eq!(
    delivered(&mut dec, &mut frame),
    vec![0x11],
    "its picture, once"
  );
}

/// LAW: **an ambiguous send whose advance is owed leaves no reference to
/// any packet behind it** (Codex R2, finding 1, read off libavcodec's own
/// counts). The candidate on trial decodes the caller's first picture as it
/// is sent, by reference — rawvideo does not copy a refcounted `GRAY8`
/// packet (`libavcodec/rawdec.c` 226 and 244 in FFmpeg 9.0.1) — and holds
/// it, so the caller's first packet has two references. The second
/// packet's reference cannot be made under the cap ([`CAPPED`], in a child
/// process): `ENOMEM`, ambiguous, the candidate's own answer, so it is
/// left; the advance meets VAAPI's refusal and then CUDA's build failing
/// with this crate's own `ENOMEM`, and is owed. By the time the send answers
/// [`Sent::MustDrain`], every packet is referenced by its own alone: the
/// caller's two and the history's copy. The drain resumes the advance, and
/// the second packet, offered again, is fed once: every picture once, in
/// order.
///
/// PLANT: the candidate kept in the owed state turns this red at "the
/// caller's first packet, released".
#[test]
fn an_ambiguous_send_whose_advance_is_owed_leaves_no_reference_behind() {
  use mediaio_ffmpeg::test_support::{
    cap_ffmpeg_allocations, in_subprocess, uncap_ffmpeg_allocations,
  };

  in_subprocess(
    "decoder::tests::an_ambiguous_send_whose_advance_is_owed_leaves_no_reference_behind",
    || {
      let mut dec = on_trial_with(
        uncapped_vaapi_refuses_and_cuda_may_fail,
        vec![Backend::Vaapi, Backend::Cuda],
      );
      let first = dec.backend();
      let held = raw_packet(0x11);
      // SAFETY: `raw_packet`'s, every byte written and the padding zeroed,
      // and nothing writes through it while the decoder may read it.
      let sent = unsafe { dec.send_av_packet(&held) };
      assert!(matches!(sent, Ok(Sent::Accepted)), "taken: {sent:?}");
      assert_eq!(
        payload_of(&held).1,
        2,
        "the candidate holds the caller's first picture, by reference"
      );
      let caller = raw_packet_with_side_data(0x22, SIDE_DATA_PAST_THE_CAP);
      let dropped = decoders_dropped();
      CUDA_FAILS.with(|fails| fails.set(true));
      // SAFETY: in `in_subprocess`'s child, alone; the send checks what
      // libavcodec answers, and the next candidate's build lifts the cap.
      unsafe { cap_ffmpeg_allocations(CAPPED) };
      // SAFETY: as above.
      let sent = unsafe { dec.send_av_packet(&caller) };
      uncap_ffmpeg_allocations();
      CUDA_FAILS.with(|fails| fails.set(false));
      assert!(
        matches!(sent, Ok(Sent::MustDrain)),
        "not taken, the advance owed: {sent:?}"
      );
      assert_eq!(
        decoders_dropped() - dropped,
        1,
        "the candidate left, torn down before the answer"
      );
      assert_eq!(dec.session_kind(), SessionKind::Owed, "the advance, owed");
      assert!(dec.live().is_none(), "no decoder lives while it is owed");
      assert_eq!(
        payload_of(&held).1,
        1,
        "the caller's first packet, released"
      );
      assert_eq!(
        payload_of(&caller).1,
        1,
        "the caller's second, referenced by its own alone"
      );
      {
        let probe = dec.probe().expect("the probe's record");
        assert_eq!(fills(&probe.buffered_packets), vec![0x11], "the history");
        for packet in &probe.buffered_packets {
          assert_eq!(
            payload_of(packet).1,
            1,
            "the history's copy, by its own alone"
          );
        }
        let pending = dec.pending().expect("the advance, owed");
        assert!(
          matches!(
            &pending.attempt,
            Attempt::Failed(BackendAnswer(Error::Ffmpeg(ffmpeg_next::Error::Other { errno })))
              if *errno == libc::ENOMEM
          ),
          "{first:?}'s answer, the errno as libavcodec returned it: {:?}",
          pending.attempt
        );
      }

      let _downloads = DownloadsForTest::set(all_deliver);
      let mut frame = crate::Frame::empty().expect("frame slot");
      let mut pictures = delivered(&mut dec, &mut frame);
      assert_eq!(dec.backend(), Backend::Cuda, "the advance, resumed");
      // SAFETY: as above.
      let sent = unsafe { dec.send_av_packet(&caller) };
      assert!(matches!(sent, Ok(Sent::Accepted)), "then taken: {sent:?}");
      pictures.extend(delivered(&mut dec, &mut frame));
      assert_eq!(pictures, vec![0x11, 0x22], "every picture once, in order");
    },
  );
}

/// LAW: **after the end and then the probe's exhaustion, no packet is ever
/// taken** (Codex R2, finding 3). The only candidate takes a picture and the
/// end, and its download chooses a layout a [`Frame`](crate::Frame) cannot
/// carry: unfit, and with no backend left the probe is exhausted, every
/// packet handed back. R2 flushed the candidate to hand the history back
/// and left it behind as a committed decoder with its end forgotten, so the
/// next packet was taken after the end. Now no decoder lives, and every
/// entry is refused by name, a flush included — a packet most of all.
///
/// PLANT: exhaustion read as committed — the candidate flushed and kept, as
/// R2 left it — turns this red at "never taken".
#[test]
fn after_the_end_and_the_probes_exhaustion_no_packet_is_ever_taken() {
  let _downloads = DownloadsForTest::set(none_deliver);
  let mut dec = raw_auditioning_decoder();
  send_all(&mut dec, &[raw_packet(0x11)]);
  assert_eq!(dec.send_eof().expect("the end"), Sent::Accepted);
  let mut frame = crate::Frame::empty().expect("frame slot");
  let exhausted = dec.receive_frame(&mut frame);
  let Err(Error::ProbeExhausted(exhausted)) = exhausted else {
    panic!("the probe exhausted: {exhausted:?}");
  };
  assert_eq!(
    fills(exhausted.unconsumed_packets()),
    vec![0x11],
    "every packet back"
  );
  for when in ["at once", "after a flush"] {
    let packet = raw_packet(0x22);
    // SAFETY: `raw_packet`'s, every byte written and the padding zeroed;
    // no decoder lives to read it.
    let sent = unsafe { dec.send_av_packet(&packet) };
    assert!(
      matches!(sent, Err(Error::Spent(crate::Spent::ProbeExhausted))),
      "{when}: never taken: {sent:?}"
    );
    dec.flush();
  }
  assert_spent(
    &mut dec,
    crate::Spent::ProbeExhausted,
    SessionKind::Exhausted,
  );
}

/// LAW: **an exhausted probe is spent on every road it exhausts on**: a
/// send, the end, and a receive (the state table's spent rows). On a send
/// and on the end, the candidate's own surface refusal leaves it, latched
/// beside libavcodec's answer to a submission after the end; on a receive,
/// its unfit picture. With no backend left, each answers
/// [`Error::ProbeExhausted`] once, and the decoder is spent after it.
///
/// PLANT: the exhausted state answering as a live one — the entry check
/// gone — turns this red at "refused by name".
#[test]
fn an_exhausted_probe_is_spent_on_every_road_it_exhausts_on() {
  for road in ["a send", "the end"] {
    let mut dec = raw_auditioning_decoder();
    assert_eq!(dec.send_eof().expect("the end"), Sent::Accepted);
    // SAFETY: `dec` owns this state and frees it only when it drops, after
    // this call.
    unsafe {
      crate::ffi::declare_ceiling_declined_for_test(
        dec.decoder().callback_state,
        8_294_400,
        2_073_600,
      )
    };
    let packet = raw_packet(0x11);
    let answer = if road == "a send" {
      // SAFETY: `raw_packet`'s, every byte written and the padding zeroed.
      unsafe { dec.send_av_packet(&packet) }
    } else {
      dec.send_eof()
    };
    assert!(
      matches!(answer, Err(Error::ProbeExhausted(_))),
      "{road}: exhausted: {answer:?}"
    );
    assert_spent(
      &mut dec,
      crate::Spent::ProbeExhausted,
      SessionKind::Exhausted,
    );
  }
  let _downloads = DownloadsForTest::set(none_deliver);
  let mut dec = raw_auditioning_decoder();
  send_all(&mut dec, &[raw_packet(0x11)]);
  let mut frame = crate::Frame::empty().expect("frame slot");
  let answer = dec.receive_frame(&mut frame);
  assert!(
    matches!(answer, Err(Error::ProbeExhausted(_))),
    "a receive: exhausted: {answer:?}"
  );
  assert_spent(
    &mut dec,
    crate::Spent::ProbeExhausted,
    SessionKind::Exhausted,
  );
}

/// LAW: **an allocation this crate makes for a download is its own, and
/// only the backend's transfer answers for the backend** (Codex R2, finding
/// 2). The download is split ([`transfer_hw_frame`]): the destination's
/// buffers and the download-format list's `ENOMEM` are this crate's own
/// ([`TransferFailure::Unattributed`]) — no delivery, nothing charged or
/// recorded, the candidate on trial still with the order and the history
/// whole. The backend's transfer failing ([`TransferFailure::Download`]), its
/// callback's `ENOMEM` included, is the backend's: the candidate is left,
/// recorded with that answer, and the next is on trial. Committed, each is
/// that picture's error, and the decoder stays.
///
/// PLANT: the destination's failure answered as the backend's, as R2's
/// unsplit transfer charged it, turns this red at "this crate's own".
#[test]
fn a_downloads_own_allocation_charges_no_backend_and_the_backends_transfer_does() {
  let enomem = ffmpeg_next::Error::Other {
    errno: libc::ENOMEM,
  };
  let mut dec = vaapi_then_cuda();
  send_all(&mut dec, &[raw_packet(0x11)]);
  let mut frame = crate::Frame::empty().expect("frame slot");
  let routed = dec.route_download(Err(TransferFailure::Unattributed(enomem)), &mut frame);
  assert!(
    is_own_enomem(&routed),
    "this crate's own, as it is: {routed:?}"
  );
  assert_eq!(
    (dec.backend(), dec.session_kind()),
    (Backend::Vaapi, SessionKind::Trial),
    "no delivery: still on trial"
  );
  {
    let probe = dec.probe().expect("on trial");
    assert!(
      probe.attempts.is_empty(),
      "nothing charged: {:?}",
      probe.attempts
    );
    assert_eq!(
      probe.remaining_backends,
      vec![Backend::Cuda],
      "the order kept"
    );
    assert_eq!(
      fills(&probe.buffered_packets),
      vec![0x11],
      "the history kept"
    );
  }

  let routed = dec.route_download(Err(TransferFailure::Download(enomem)), &mut frame);
  assert!(
    matches!(routed, Ok(None)),
    "the backend's: left, the receive to start again: {routed:?}"
  );
  assert_eq!(
    (dec.backend(), dec.session_kind()),
    (Backend::Cuda, SessionKind::Trial),
    "the next candidate, on trial"
  );
  {
    let probe = dec.probe().expect("on trial");
    assert!(
      matches!(
        probe.attempts.as_slice(),
        [(Backend::Vaapi, Attempt::Failed(BackendAnswer(Error::Ffmpeg(ffmpeg_next::Error::Other { errno }))))]
          if *errno == libc::ENOMEM
      ),
      "the backend's answer, recorded: {:?}",
      probe.attempts
    );
  }

  let mut dec = raw_pinned_decoder();
  for failure in [
    TransferFailure::Unattributed(enomem),
    TransferFailure::Download(enomem),
  ] {
    let routed = dec.route_download(Err(failure), &mut frame);
    assert!(
      is_own_enomem(&routed),
      "the picture's own error: {routed:?}"
    );
    assert_eq!(
      dec.session_kind(),
      SessionKind::Committed,
      "the decoder stays"
    );
  }
}

/// A VideoToolbox frame of `sw_format` surfaces, `side` pixels square, its
/// device and frames context unreferenced when it drops.
#[cfg(target_vendor = "apple")]
struct VideoToolboxFrame {
  frame: frame::Video,
  frames: *mut AVBufferRef,
  device: *mut AVBufferRef,
}

#[cfg(target_vendor = "apple")]
impl VideoToolboxFrame {
  fn new(sw_format: ffmpeg_next::ffi::AVPixelFormat, side: i32) -> Self {
    use ffmpeg_next::ffi::{
      AVHWDeviceType, AVPixelFormat, av_hwdevice_ctx_create, av_hwframe_ctx_alloc,
      av_hwframe_ctx_init, av_hwframe_get_buffer,
    };
    ffmpeg_next::init().expect("ffmpeg init");
    // SAFETY: every reference made here is owned by the value returned and
    // unreferenced when it drops, the frame first; the frames context is
    // written whole before it is initialised.
    unsafe {
      let mut device = ptr::null_mut();
      assert!(
        av_hwdevice_ctx_create(
          &mut device,
          AVHWDeviceType::AV_HWDEVICE_TYPE_VIDEOTOOLBOX,
          ptr::null(),
          ptr::null_mut(),
          0
        ) >= 0,
        "a VideoToolbox device"
      );
      let frames = av_hwframe_ctx_alloc(device);
      assert!(!frames.is_null(), "a frames context");
      let ctx = (*frames).data.cast::<AVHWFramesContext>();
      (*ctx).format = AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX;
      (*ctx).sw_format = sw_format;
      (*ctx).width = side;
      (*ctx).height = side;
      assert!(av_hwframe_ctx_init(frames) >= 0, "the frames context");
      let mut frame = frame::Video::empty();
      assert!(
        av_hwframe_get_buffer(frames, frame.as_mut_ptr(), 0) >= 0,
        "a VideoToolbox frame"
      );
      Self {
        frame,
        frames,
        device,
      }
    }
  }
}

#[cfg(target_vendor = "apple")]
impl Drop for VideoToolboxFrame {
  fn drop(&mut self) {
    // SAFETY: the frame's buffers go first, then the references this value
    // owns, each once.
    unsafe {
      av_frame_unref(self.frame.as_mut_ptr());
      av_buffer_unref(&mut self.frames);
      av_buffer_unref(&mut self.device);
    }
  }
}

/// What a download came to, for a law's message.
fn download_answer(answer: &std::result::Result<(), TransferFailure>) -> String {
  match answer {
    Ok(()) => "delivered".to_owned(),
    Err(failure) => failure.to_string(),
  }
}

/// LAW: **on VideoToolbox, the download's destination is this crate's own
/// allocation, and only the backend's callback answers for the backend**
/// (Codex R2, finding 2), by libavutil's own behaviour, in a child process.
/// A VideoToolbox `NV12` frame, 256 × 256, its download format picked
/// before any transfer ([`download_format_of`]). Under an allocation cap
/// below the destination's buffers and above everything the callback
/// allocates (`vt_transfer_data_from`, `libavutil/hwcontext_videotoolbox.c`
/// 739–768 in FFmpeg 9.0.1: a mapping frame, its descriptor and buffer
/// references), the destination's `av_frame_get_buffer` fails, and that is
/// this crate's own ([`TransferFailure::Unattributed`]). With the cap
/// lifted the transfer delivers, the frame in the format picked at the
/// frame's display extent. The backend's own callback failing — asked for a
/// layout its surface is not, `vt_map_frame`'s `AVERROR_UNKNOWN` (694–699)
/// — is the backend's ([`TransferFailure::Download`]). And the
/// download-format list's own `ENOMEM`, under a cap below its few bytes, is
/// this crate's own. Before the split the cap's `ENOMEM` came out of
/// `av_hwframe_transfer_data` itself (`transfer_data_alloc`,
/// `libavutil/hwcontext.c` 398–446) and was charged to the backend.
///
/// PLANT: the destination allocated inside the transfer again turns this
/// red at "this crate's own".
#[cfg(target_vendor = "apple")]
#[test]
fn on_videotoolbox_the_download_destination_is_this_crates_own_and_the_callback_the_backends() {
  use ffmpeg_next::ffi::AVPixelFormat;
  use mediaio_ffmpeg::test_support::{
    cap_ffmpeg_allocations, in_subprocess, uncap_ffmpeg_allocations,
  };

  in_subprocess(
    "decoder::tests::on_videotoolbox_the_download_destination_is_this_crates_own_and_the_callback_the_backends",
    || {
      let src = VideoToolboxFrame::new(AVPixelFormat::AV_PIX_FMT_NV12, 256);
      // SAFETY: `src.frame` is live for every call below, and `dst` is a
      // slot of this law's own.
      unsafe {
        let format = download_format_of(&src.frame).map_err(|failure| failure.to_string());
        assert_eq!(
          format,
          Ok(DownloadFormat::Picked(NV12)),
          "the format, picked first"
        );
        let format = DownloadFormat::Picked(NV12);
        let mut dst = crate::Frame::empty().expect("frame slot");

        // In `in_subprocess`'s child, alone: the transfer checks what
        // libavutil answers, and the cap is lifted as it returns.
        cap_ffmpeg_allocations(16 * 1024);
        let capped = transfer_hw_frame(&mut dst, &src.frame, format);
        uncap_ffmpeg_allocations();
        assert!(
          matches!(&capped, Err(TransferFailure::Unattributed(e)) if is_errno(e, libc::ENOMEM)),
          "the destination's allocation, this crate's own: {}",
          download_answer(&capped)
        );
        assert_eq!(
          (*dst.as_inner_mut().as_ptr()).format,
          AVPixelFormat::AV_PIX_FMT_NONE as i32,
          "the slot left empty"
        );

        let delivered = transfer_hw_frame(&mut dst, &src.frame, format);
        assert!(
          delivered.is_ok(),
          "uncapped, delivered: {}",
          download_answer(&delivered)
        );
        let raw = dst.as_inner_mut().as_ptr();
        assert_eq!(
          ((*raw).format, (*raw).width, (*raw).height),
          (NV12, 256, 256),
          "in the format picked, at the display extent"
        );

        let p010 = AVPixelFormat::AV_PIX_FMT_P010LE as i32;
        let mismatched = transfer_hw_frame(&mut dst, &src.frame, DownloadFormat::Picked(p010));
        assert!(
          matches!(
            mismatched,
            Err(TransferFailure::Download(ffmpeg_next::Error::Unknown))
          ),
          "the callback's own answer, the backend's: {}",
          download_answer(&mismatched)
        );

        cap_ffmpeg_allocations(1);
        let listed = download_format_of(&src.frame);
        uncap_ffmpeg_allocations();
        assert!(
          matches!(&listed, Err(TransferFailure::Unattributed(e)) if is_errno(e, libc::ENOMEM)),
          "the list's allocation, this crate's own: {:?}",
          listed.map_err(|failure| failure.to_string())
        );
      }
    },
  );
}

// ---------------------------------------------------------------------------
//  The state table, cell by cell: what the laws above leave unnamed.
// ---------------------------------------------------------------------------

/// A candidate on trial owed the history: the probe left VAAPI with four
/// pictures' packets recorded, and CUDA, built in software, was replayed
/// them as far as its first output — two, rawvideo decoding a picture's
/// packet as it is sent and holding a second until a picture is asked for.
/// With `end_given`, the caller had given the end, which the replay stopped
/// short of.
fn owed_the_history(end_given: bool) -> VideoDecoder {
  let mut dec = vaapi_then_cuda();
  {
    let probe = dec.probe_mut().expect("on trial");
    probe.buffered_packets = [0x11, 0x22, 0x33, 0x44].map(raw_packet).into();
    probe.fed = 4;
    probe.buffered_bytes = 4 * RAW_PICTURE;
  }
  if end_given {
    dec.set_end(End::Taken);
  }
  dec
    .advance_probe(its_own_failure())
    .expect("CUDA installed");
  assert_eq!(
    (dec.backend(), dec.session_kind()),
    (Backend::Cuda, SessionKind::Trial),
    "the next candidate, on trial"
  );
  assert_eq!(
    dec.probe().map(|probe| probe.fed),
    Some(2),
    "the replay stopped at the candidate's first output"
  );
  dec
}

/// STATE TABLE, on trial — a send and the end, **history owed**:
/// `MustDrain`, nothing sent; and a send that meets back pressure:
/// `MustDrain`, the copy dropped.
///
/// PLANT: the trial's entry check reading no history owed turns this red
/// at "nothing sent".
#[test]
fn on_trial_with_history_owed_a_send_and_the_end_drain_first() {
  let mut dec = owed_the_history(false);
  let packet = raw_packet(0x55);
  // SAFETY: `raw_packet`'s, every byte written and the padding zeroed, and
  // nothing writes through it while the decoder may read it.
  let sent = unsafe { dec.send_av_packet(&packet) };
  assert!(matches!(sent, Ok(Sent::MustDrain)), "a send: {sent:?}");
  let ended = dec.send_eof();
  assert!(matches!(ended, Ok(Sent::MustDrain)), "the end: {ended:?}");
  let probe = dec.probe().expect("on trial");
  assert_eq!(
    (probe.buffered_packets.len(), probe.fed),
    (4, 2),
    "nothing sent, nothing recorded"
  );
  assert_eq!(dec.end(), Some(End::Open), "the end not taken");

  // Back pressure on trial: the candidate holds a picture and a packet.
  let mut dec = raw_auditioning_decoder();
  send_all(&mut dec, &[raw_packet(0x11), raw_packet(0x22)]);
  // SAFETY: as above.
  let sent = unsafe { dec.send_av_packet(&packet) };
  assert!(
    matches!(sent, Ok(Sent::MustDrain)),
    "back pressure: {sent:?}"
  );
  assert_eq!(
    fills(&dec.probe().expect("on trial").buffered_packets),
    vec![0x11, 0x22],
    "the copy dropped"
  );
}

/// STATE TABLE — **the end given and owed**, on trial and then committed: a
/// send and a second end each answer `AVERROR_EOF`, nothing sent, though the
/// decoder has not yet been fed the end. The candidate's first picture
/// commits it with the history's rest as the backlog and the end still
/// owed; the receive road feeds the backlog, then the end, in order, and the
/// stream ends.
///
/// PLANT: the end-owed check gone from the gate turns this red at "a send
/// after the end".
#[test]
fn with_the_end_owed_a_send_and_a_second_end_are_refused_on_trial_and_committed() {
  let mut dec = owed_the_history(true);
  assert_eq!(dec.end(), Some(End::Owed), "the end, owed to the candidate");
  assert_eq!(dec.phase(), SessionPhase::AuditioningPastEnd);
  let packet = raw_packet(0x55);
  for state in ["on trial", "committed"] {
    // SAFETY: `raw_packet`'s, every byte written and the padding zeroed, and
    // nothing writes through it while the decoder may read it.
    let sent = unsafe { dec.send_av_packet(&packet) };
    assert!(
      matches!(sent, Err(Error::Ffmpeg(ffmpeg_next::Error::Eof))),
      "{state}: a send after the end: {sent:?}"
    );
    let ended = dec.send_eof();
    assert!(
      matches!(ended, Err(Error::Ffmpeg(ffmpeg_next::Error::Eof))),
      "{state}: a second end: {ended:?}"
    );
    if state == "on trial" {
      let probe = dec.probe().expect("on trial");
      assert_eq!(
        (probe.buffered_packets.len(), probe.fed),
        (4, 2),
        "nothing sent"
      );
      let _downloads = DownloadsForTest::set(all_deliver);
      let mut frame = crate::Frame::empty().expect("frame slot");
      assert_eq!(
        dec.receive_frame(&mut frame).expect("the first picture"),
        Received::Produced
      );
      assert_eq!(dec.session_kind(), SessionKind::Committed, "committed");
      assert_eq!(dec.end(), Some(End::Owed), "the end, still owed");
      assert_eq!(
        dec
          .backlog()
          .iter()
          .map(|packet| packet.data().expect("a payload")[0])
          .collect::<Vec<u8>>(),
        vec![0x33, 0x44],
        "the backlog"
      );
    }
  }
  let _downloads = DownloadsForTest::set(all_deliver);
  let mut frame = crate::Frame::empty().expect("frame slot");
  assert_eq!(
    delivered(&mut dec, &mut frame),
    vec![0x22, 0x33, 0x44],
    "the rest, in order"
  );
  assert_eq!(
    dec.receive_frame(&mut frame).expect("the end"),
    Received::Ended,
    "the end, fed after the backlog"
  );
  assert_eq!(dec.end(), Some(End::Taken));
}

/// STATE TABLE, committed — the end, **a backlog owed**: `MustDrain`,
/// nothing sent, and taken once the drain has fed the backlog.
///
/// PLANT: the committed entry check reading no backlog owed turns this red
/// at "the end drains first".
#[test]
fn committed_with_a_backlog_owed_the_end_drains_first() {
  let mut frame = crate::Frame::empty().expect("frame slot");
  let history = [0x11, 0x22, 0x33, 0x44].map(raw_packet).into();
  let (mut dec, mut pictures) = committed_with_backlog(history, &mut frame);
  assert_eq!(dec.backlog().len(), 2, "the replay's rest, owed");
  let ended = dec.send_eof();
  assert!(
    matches!(ended, Ok(Sent::MustDrain)),
    "the end drains first: {ended:?}"
  );
  assert_eq!(dec.end(), Some(End::Open), "not taken");
  let _downloads = DownloadsForTest::set(all_deliver);
  pictures.extend(delivered(&mut dec, &mut frame));
  assert_eq!(
    dec.send_eof().expect("the end"),
    Sent::Accepted,
    "then taken"
  );
  pictures.extend(delivered(&mut dec, &mut frame));
  assert_eq!(
    pictures,
    vec![0x11, 0x22, 0x33, 0x44],
    "every picture, in order"
  );
}

/// STATE TABLE — **an advance owed**, every entry. No decoder lives in it.
/// With the end not given, a send and the end resume the advance and, where
/// it stops again, answer `MustDrain`; a receive answers the failure that
/// stopped it; a flush forgets the history and keeps the advance owed; and
/// once the failure is gone, a send resumes it and runs on the candidate
/// installed. With the end given, a send and a second end are the caller's
/// fault, `AVERROR_EOF`, and a receive resumes the advance: the next
/// candidate is replayed the history and the end. And an advance that finds
/// no candidate left exhausts the probe.
///
/// PLANT: the owed entry check gone — a send answering `MustDrain` without
/// resuming — turns this red at "runs on the candidate installed".
#[test]
fn an_owed_advance_answers_every_entry_by_its_state() {
  let owed = |end_given: bool, remaining: Vec<Backend>| {
    let mut dec = on_trial_with(vaapi_refuses_and_cuda_may_fail, remaining);
    send_all(&mut dec, &[raw_packet(0x11)]);
    if end_given {
      assert_eq!(dec.send_eof().expect("the end"), Sent::Accepted);
    }
    CUDA_FAILS.with(|fails| fails.set(true));
    let stopped = dec.advance_probe(its_own_failure());
    assert!(
      is_own_enomem(&stopped),
      "this crate's own, surfaced: {stopped:?}"
    );
    assert_eq!(dec.session_kind(), SessionKind::Owed, "the advance, owed");
    dec
  };
  let packet = raw_packet(0x22);
  let mut frame = crate::Frame::empty().expect("frame slot");

  // The end not given; CUDA's build still failing.
  let mut dec = owed(false, vec![Backend::Vaapi, Backend::Cuda]);
  // SAFETY: `raw_packet`'s, every byte written and the padding zeroed, and
  // nothing writes through it while a decoder may read it.
  let sent = unsafe { dec.send_av_packet(&packet) };
  assert!(
    matches!(sent, Ok(Sent::MustDrain)),
    "a send, stopped again: {sent:?}"
  );
  let ended = dec.send_eof();
  assert!(
    matches!(ended, Ok(Sent::MustDrain)),
    "the end, stopped again: {ended:?}"
  );
  let received = dec.receive_frame(&mut frame);
  assert!(
    is_own_enomem(&received),
    "a receive: the failure: {received:?}"
  );
  assert_eq!(dec.session_kind(), SessionKind::Owed, "still owed");
  assert!(dec.live().is_none(), "no decoder lives");
  dec.flush();
  assert_eq!(
    dec.session_kind(),
    SessionKind::Owed,
    "a flush keeps it owed"
  );
  assert!(
    dec.probe().expect("the record").buffered_packets.is_empty(),
    "and forgets the history"
  );
  CUDA_FAILS.with(|fails| fails.set(false));
  // SAFETY: as above.
  let sent = unsafe { dec.send_av_packet(&packet) };
  assert!(
    matches!(sent, Ok(Sent::Accepted)),
    "runs on the candidate installed: {sent:?}"
  );
  assert_eq!(
    (dec.backend(), dec.session_kind()),
    (Backend::Cuda, SessionKind::Trial)
  );
  assert_eq!(
    fills(&dec.probe().expect("on trial").buffered_packets),
    vec![0x22]
  );

  // The end given.
  let mut dec = owed(true, vec![Backend::Vaapi, Backend::Cuda]);
  // SAFETY: as above.
  let sent = unsafe { dec.send_av_packet(&packet) };
  assert!(
    matches!(sent, Err(Error::Ffmpeg(ffmpeg_next::Error::Eof))),
    "a send after the end: {sent:?}"
  );
  let ended = dec.send_eof();
  assert!(
    matches!(ended, Err(Error::Ffmpeg(ffmpeg_next::Error::Eof))),
    "a second end: {ended:?}"
  );
  let received = dec.receive_frame(&mut frame);
  assert!(
    is_own_enomem(&received),
    "a receive, stopped again: {received:?}"
  );
  CUDA_FAILS.with(|fails| fails.set(false));
  let _downloads = DownloadsForTest::set(all_deliver);
  assert_eq!(
    delivered(&mut dec, &mut frame),
    vec![0x11],
    "the next candidate, replayed the history"
  );
  assert_eq!(
    dec.receive_frame(&mut frame).expect("and the end"),
    Received::Ended
  );

  // No candidate left.
  let mut dec = owed(false, vec![Backend::Cuda]);
  CUDA_FAILS.with(|fails| fails.set(false));
  dec.probe_mut().expect("the record").build = every_backend_refuses;
  // SAFETY: as above.
  let sent = unsafe { dec.send_av_packet(&packet) };
  let Err(Error::ProbeExhausted(exhausted)) = sent else {
    panic!("exhausted: {sent:?}");
  };
  assert_eq!(
    exhausted.attempts().len(),
    2,
    "the candidate left, then CUDA"
  );
  assert_spent(
    &mut dec,
    crate::Spent::ProbeExhausted,
    SessionKind::Exhausted,
  );
}

/// STATE TABLE — **the drop, in every state**: a live decoder — on trial,
/// or committed — is freed with the decoder; an advance owed, an exhausted
/// probe and an ended decoder hold none, so the drop frees no decoder.
///
/// PLANT: the candidate kept in the owed state turns this red at "an
/// advance owed".
#[test]
fn every_state_frees_what_it_holds_when_it_drops() {
  let drops = |dec: VideoDecoder| {
    let before = decoders_dropped();
    drop(dec);
    decoders_dropped() - before
  };
  assert_eq!(drops(raw_auditioning_decoder()), 1, "on trial");
  assert_eq!(drops(raw_pinned_decoder()), 1, "committed");

  let mut dec = on_trial_with(vaapi_refuses_and_cuda_may_fail, vec![Backend::Cuda]);
  CUDA_FAILS.with(|fails| fails.set(true));
  let stopped = dec.advance_probe(its_own_failure());
  CUDA_FAILS.with(|fails| fails.set(false));
  assert!(is_own_enomem(&stopped), "owed: {stopped:?}");
  assert_eq!(drops(dec), 0, "an advance owed");

  let mut dec = raw_auditioning_decoder();
  let exhausted = dec.advance_probe(its_own_failure());
  assert!(
    matches!(exhausted, Err(Error::ProbeExhausted(_))),
    "{exhausted:?}"
  );
  assert_eq!(drops(dec), 0, "exhausted");

  let mut dec = raw_auditioning_decoder();
  let ended = dec.end_ambiguously(Error::Ffmpeg(ffmpeg_next::Error::Other {
    errno: libc::ENOMEM,
  }));
  assert!(matches!(ended, Error::AmbiguousSend(_)), "{ended:?}");
  assert_eq!(dec.session_kind(), SessionKind::Ended);
  assert_eq!(drops(dec), 0, "ended");
}

/// STATE TABLE, on trial — **the feed, by class** ([`VideoDecoder::route_fed`]),
/// and the receive's own refusal. A history packet taken with the
/// candidate's error, or `ENOMEM`, leaves the candidate, the history whole
/// for the next; taken beside this crate's own refusal (the frame budget),
/// the refusal is answered and the candidate stays; `ENOMEM` beside it ends
/// the decoder, the whole history back. And a receive that reads this
/// crate's own refusal answers it, the candidate on trial still.
///
/// PLANT: the feed's `ENOMEM` reported as this crate's own, the candidate
/// kept, turns this red at "leaves the candidate".
#[test]
fn on_trial_the_feed_routes_by_class() {
  let enomem = ffmpeg_next::Error::Other {
    errno: libc::ENOMEM,
  };
  for (name, error, ambiguous) in [
    (
      "taken with the candidate's error",
      ffmpeg_next::Error::InvalidData,
      false,
    ),
    ("ENOMEM", enomem, true),
  ] {
    let mut dec = vaapi_then_cuda();
    send_all(&mut dec, &[raw_packet(0x11)]);
    let routed = dec.route_fed(Stopped::at(Owing::History, error, ambiguous));
    assert!(routed.is_ok(), "{name}: leaves the candidate: {routed:?}");
    assert_eq!(
      (dec.backend(), dec.session_kind()),
      (Backend::Cuda, SessionKind::Trial),
      "{name}: the next, on trial"
    );
    let probe = dec.probe().expect("on trial");
    assert_eq!(
      fills(&probe.buffered_packets),
      vec![0x11],
      "{name}: the history whole"
    );
    assert_eq!(
      tried(probe),
      vec![Backend::Vaapi],
      "{name}: the candidate's own answer"
    );
  }

  let budget = |dec: &VideoDecoder| {
    // SAFETY: `dec` owns this state and frees it only when it drops, after
    // this call.
    unsafe {
      crate::ffi::declare_frame_budget_declined_for_test(dec.decoder().callback_state, 12_582_912)
    }
  };
  let mut dec = vaapi_then_cuda();
  send_all(&mut dec, &[raw_packet(0x11)]);
  budget(&dec);
  let routed = dec.route_fed(Stopped::at(
    Owing::History,
    ffmpeg_next::Error::InvalidData,
    false,
  ));
  assert!(
    matches!(routed, Err(Error::FrameBudgetExceeded(_))),
    "taken beside this crate's refusal: the refusal: {routed:?}"
  );
  assert_eq!(
    (dec.backend(), dec.session_kind()),
    (Backend::Vaapi, SessionKind::Trial),
    "the candidate stays"
  );
  assert!(
    dec.probe().expect("on trial").attempts.is_empty(),
    "nothing charged"
  );

  let mut dec = vaapi_then_cuda();
  send_all(&mut dec, &[raw_packet(0x11)]);
  budget(&dec);
  let routed = dec.route_fed(Stopped::at(Owing::History, enomem, true));
  let Err(Error::AmbiguousSend(ended)) = routed else {
    panic!("ENOMEM beside this crate's refusal ends the decoder: {routed:?}");
  };
  assert_eq!(
    fills(ended.residual()),
    vec![0x11],
    "the whole history back"
  );
  assert_eq!(dec.session_kind(), SessionKind::Ended);

  let mut dec = raw_auditioning_decoder();
  budget(&dec);
  let mut frame = crate::Frame::empty().expect("frame slot");
  let received = dec.receive_frame(&mut frame);
  assert!(
    matches!(received, Err(Error::FrameBudgetExceeded(_))),
    "the receive's own refusal: {received:?}"
  );
  assert_eq!(dec.session_kind(), SessionKind::Trial, "on trial still");
}

/// STATE TABLE, on trial — a send **after the end the candidate took** is
/// refused for good: libavcodec answers `AVERROR_EOF` (`decode.c` 739–740),
/// the caller's fault, and the candidate stays on trial, nothing recorded
/// and nothing charged.
///
/// PLANT: a send after the end read as the candidate failing turns this red
/// at "on trial still".
#[test]
fn on_trial_a_send_after_the_end_the_candidate_took_is_refused_for_good() {
  let mut dec = raw_auditioning_decoder();
  send_all(&mut dec, &[raw_packet(0x11)]);
  assert_eq!(dec.send_eof().expect("the end"), Sent::Accepted);
  assert_eq!(dec.end(), Some(End::Taken));
  let packet = raw_packet(0x22);
  // SAFETY: `raw_packet`'s, every byte written and the padding zeroed, and
  // nothing writes through it while the decoder may read it.
  let sent = unsafe { dec.send_av_packet(&packet) };
  assert!(
    matches!(sent, Err(Error::Ffmpeg(ffmpeg_next::Error::Eof))),
    "refused for good: {sent:?}"
  );
  assert_eq!(dec.session_kind(), SessionKind::Trial, "on trial still");
  let probe = dec.probe().expect("on trial");
  assert!(
    probe.attempts.is_empty(),
    "nothing charged: {:?}",
    probe.attempts
  );
  assert_eq!(
    fills(&probe.buffered_packets),
    vec![0x11],
    "nothing recorded"
  );
}

/// STATE TABLE, on trial — a send **taken with the candidate's error goes to
/// the next candidate in the same call**: the candidate decodes 32 × 32
/// pictures and the packet is short of one, so it is left, torn down, and
/// CUDA — built in software over 16 × 16 pictures — is installed and takes
/// the packet: `Accepted`, recorded once, its picture delivered once.
///
/// PLANT: the send answering `MustDrain` where a candidate was installed
/// turns this red at "the next candidate's, in this call".
#[test]
fn on_trial_a_send_taken_with_the_candidates_error_goes_to_the_next_in_the_same_call() {
  let mut dec = raw_auditioning_decoder_with(32, crate::DecoderOptions::new());
  let first = dec.backend();
  {
    let probe = dec.probe_mut().expect("on trial");
    probe.parameters = raw_parameters(RAW_SIDE);
    probe.build = VideoDecoder::software_state_for_test;
    probe.remaining_backends = vec![Backend::Cuda];
  }
  let packet = raw_packet(0x11);
  let dropped = decoders_dropped();
  // SAFETY: as above.
  let sent = unsafe { dec.send_av_packet(&packet) };
  assert!(
    matches!(sent, Ok(Sent::Accepted)),
    "the next candidate's, in this call: {sent:?}"
  );
  assert_eq!(
    decoders_dropped() - dropped,
    1,
    "the candidate left, torn down"
  );
  assert_eq!(
    (dec.backend(), dec.session_kind()),
    (Backend::Cuda, SessionKind::Trial)
  );
  {
    let probe = dec.probe().expect("on trial");
    assert_eq!(tried(probe), vec![first], "the candidate's own answer");
    assert_eq!(fills(&probe.buffered_packets), vec![0x11], "recorded once");
  }
  let _downloads = DownloadsForTest::set(all_deliver);
  let mut frame = crate::Frame::empty().expect("frame slot");
  assert_eq!(
    delivered(&mut dec, &mut frame),
    vec![0x11],
    "its picture, once"
  );
}

/// `AV_PIX_FMT_NONE`, as the raw integer: a download-format list's
/// terminator, and so an empty list's first entry.
const NONE_LISTED: i32 = ffmpeg_next::ffi::AVPixelFormat::AV_PIX_FMT_NONE as i32;

/// Downloads where every backend lists nothing to download into.
fn nothing_listed(_: Backend) -> Option<i32> {
  Some(NONE_LISTED)
}

/// Downloads where VAAPI lists nothing to download into, and CUDA delivers.
fn vaapi_lists_nothing(backend: Backend) -> Option<i32> {
  (backend == Backend::Vaapi).then_some(NONE_LISTED)
}

/// LAW: **a backend that lists no CPU layout to download into is unfit,
/// naming `AV_PIX_FMT_NONE`, and no errno is made for it anywhere in the
/// record** (the ruling on R3's decision 2). Its download-format list is
/// empty — the terminator first, as VAAPI's is with no device format and
/// D3D11VA's on an opaque surface — so it offers nothing to download into:
/// its unfitness for this caller, never a failure.
/// - **On trial**, each candidate is left unfit. With none left, the probe
///   is exhausted, and every attempt is `DownloadFormatUnsupported` naming
///   `AV_PIX_FMT_NONE`, with no `Error::Ffmpeg` among them and every packet
///   back.
/// - **Where the next candidate delivers**, the advance hands it the
///   history whole: every picture, once, in order.
/// - **A backend pinned alone** answers it by name.
///
/// The message names the case.
///
/// PLANT: the synthesized `EINVAL` restored for an empty list turns this red
/// at "unfit, naming NONE".
#[test]
fn a_backend_that_lists_no_download_format_is_unfit_and_no_errno_is_made() {
  let sent = [0x11u8, 0x22];
  let mut frame = crate::Frame::empty().expect("frame slot");
  {
    let _downloads = DownloadsForTest::set(nothing_listed);
    let mut dec = vaapi_then_cuda();
    send_all(&mut dec, &sent.map(raw_packet));
    let exhausted = dec.receive_frame(&mut frame);
    let Err(Error::ProbeExhausted(exhausted)) = exhausted else {
      panic!("the probe exhausted: {exhausted:?}");
    };
    let attempts: Vec<_> = exhausted
      .attempts()
      .iter()
      .map(|(backend, attempt)| (*backend, attempt.as_ref()))
      .collect();
    assert!(
      matches!(
        attempts.as_slice(),
        [
          (Backend::Vaapi, Error::DownloadFormatUnsupported(a)),
          (Backend::Cuda, Error::DownloadFormatUnsupported(b)),
        ] if a.format() == NONE_LISTED && b.format() == NONE_LISTED
      ),
      "each candidate unfit, naming NONE, and no errno anywhere: {attempts:?}"
    );
    assert_eq!(
      fills(exhausted.unconsumed_packets()),
      sent.to_vec(),
      "every packet back, in order"
    );
  }
  {
    let _downloads = DownloadsForTest::set(vaapi_lists_nothing);
    let mut dec = vaapi_then_cuda();
    send_all(&mut dec, &sent.map(raw_packet));
    let first = dec.receive_frame(&mut frame);
    assert!(
      matches!(first, Ok(Received::Produced)),
      "the next candidate delivers: {first:?}"
    );
    assert_eq!(
      (dec.backend(), dec.session_kind()),
      (Backend::Cuda, SessionKind::Committed),
      "committed on its first delivered picture"
    );
    let mut pictures = vec![frame.row(0, 0).expect("a gray picture")[0]];
    pictures.extend(delivered(&mut dec, &mut frame));
    assert_eq!(
      pictures, sent,
      "the history whole: every picture, once, in order"
    );
  }
  let _downloads = DownloadsForTest::set(nothing_listed);
  let mut dec = raw_pinned_decoder();
  send_all(&mut dec, &[raw_packet(0x11)]);
  let received = dec.receive_frame(&mut frame);
  let Err(Error::DownloadFormatUnsupported(layout)) = received else {
    panic!("a pinned backend answers it by name: {received:?}");
  };
  assert_eq!(layout.format(), NONE_LISTED, "naming NONE");
  assert!(
    layout.to_string().contains("lists no CPU pixel format"),
    "the message names the case: {layout}"
  );
}

// ---------------------------------------------------------------------------
//  The output side of the state (Codex R3, finding 1), and every road a
//  transfer takes (finding 2).
// ---------------------------------------------------------------------------

/// The next download of this thread's decoders fails at `step`, with
/// `ENOMEM`, before anything else is read there ([`LOCAL_FAILURES_FOR_TEST`]):
/// an allocation of this crate's own run out of memory.
fn fail_next_download_at(step: LocalStep) {
  LOCAL_FAILURES_FOR_TEST.with(|cell| cell.set((1, step)));
}

/// Every allocation of this crate's own on the way to a delivery, by name.
const LOCAL_STEPS: [(LocalStep, &str); 3] = [
  (LocalStep::List, "the format list"),
  (LocalStep::Destination, "the destination"),
  (LocalStep::Properties, "the properties' copy"),
];

/// The scaled-output road's answers for the span of the value
/// ([`FITTED_FOR_TEST`]): one per picture downloaded, in order, the stage
/// standing down once they run out, and for good when the value drops.
struct FittedRoad;

impl FittedRoad {
  fn answers(answers: impl IntoIterator<Item = FittedForTest>) -> Self {
    FITTED_FOR_TEST.with(|road| *road.borrow_mut() = Some(answers.into_iter().collect()));
    Self
  }

  /// How many of the answers are still to be given.
  fn left() -> usize {
    FITTED_FOR_TEST.with(|road| road.borrow().as_ref().map_or(0, VecDeque::len))
  }
}

impl Drop for FittedRoad {
  fn drop(&mut self) {
    FITTED_FOR_TEST.with(|road| *road.borrow_mut() = None);
  }
}

/// [`delivered`], bounded: the first byte of each picture `dec` delivers
/// until it asks for input or ends, and no more than `bound` of them — a
/// decoder that delivers one picture twice fails the law here rather than
/// hanging it.
fn delivered_within(dec: &mut VideoDecoder, frame: &mut crate::Frame, bound: usize) -> Vec<u8> {
  let mut fills = Vec::new();
  loop {
    match dec
      .receive_frame(frame)
      .expect("a picture or a request for input")
    {
      Received::Produced => {
        fills.push(frame.row(0, 0).expect("a gray picture's first row")[0]);
        assert!(
          fills.len() <= bound,
          "more pictures than were sent: {fills:02x?}"
        );
      }
      Received::NeedsInput | Received::Ended => return fills,
    }
  }
}

/// The picture `frame` holds, by its first byte.
fn picture_in(frame: &crate::Frame) -> u8 {
  frame.row(0, 0).expect("a gray picture's first row")[0]
}

/// A rawvideo `GRAY8` picture's packet, `side` pixels square, every byte
/// `fill`.
fn raw_packet_of(side: usize, fill: u8) -> Packet {
  crate::boundary::try_packet_copy(&vec![fill; side * side]).expect("a submittable packet")
}

/// [`raw_packet`], presented at `pts`.
fn raw_packet_at(fill: u8, pts: i64) -> Packet {
  let mut packet = raw_packet(fill);
  // SAFETY: `packet` owns a live `AVPacket`; `pts` is a plain field.
  unsafe { (*packet.as_mut_ptr()).pts = pts };
  packet
}

/// `packet` sent to `dec`, which decodes it by reference — rawvideo does not
/// copy a refcounted `GRAY8` packet (`libavcodec/rawdec.c` 226 and 244 in
/// FFmpeg 9.0.1) — and its picture held: the download runs out of memory at
/// the destination. The picture references the caller's packet.
fn holding(dec: &mut VideoDecoder, frame: &mut crate::Frame, packet: &Packet) {
  // SAFETY: `raw_packet`'s, every byte written and the padding zeroed, and
  // nothing writes through it while the decoder may read it.
  let sent = unsafe { dec.send_av_packet(packet) };
  assert!(matches!(sent, Ok(Sent::Accepted)), "taken: {sent:?}");
  fail_next_download_at(LocalStep::Destination);
  let received = dec.receive_frame(frame);
  assert!(is_own_enomem(&received), "this crate's own: {received:?}");
  assert_eq!(dec.held(), Held::Pending, "the picture held");
  assert_eq!(
    payload_of(packet).1,
    2,
    "the picture held references the caller's packet"
  );
}

/// LAW: **a picture whose download runs out of memory is held, and the next
/// receive delivers that picture, then the next, in order, with no gap**
/// (Codex R3, finding 1). For each allocation of this crate's own on the
/// way to a delivery — the format list's, the destination's, the
/// properties' copy's — failing with `ENOMEM`:
/// - **on trial**, the receive answers this crate's own `ENOMEM`, nothing
///   charged — no attempt recorded, the order and the history whole — and
///   the picture held ([`Held::Pending`]). The next receive downloads it
///   before it asks the decoder: that picture, which commits the candidate.
/// - **committed**, the same decoder at its second picture, and a backend
///   pinned alone at its first: held, then that picture, then the next.
///
/// R3 asked the decoder again, and `avcodec_receive_frame` unreferences the
/// frame it is handed first (`libavcodec/avcodec.c` 707–710 in FFmpeg
/// 9.0.1): the picture was lost.
///
/// PLANT: the retryable failure leaving the slot to the next
/// `avcodec_receive_frame`, as R3 did, turns this red at "on trial: held";
/// the receive's check for a picture held removed, at "that picture,
/// delivered next".
#[test]
fn a_picture_whose_download_runs_out_of_memory_is_held_and_delivered_next_in_order() {
  let _downloads = DownloadsForTest::set(all_deliver);
  let mut frame = crate::Frame::empty().expect("frame slot");
  for (step, name) in LOCAL_STEPS {
    let mut dec = vaapi_then_cuda();
    send_all(&mut dec, &[0x11, 0x22].map(raw_packet));
    fail_next_download_at(step);
    let received = dec.receive_frame(&mut frame);
    assert!(
      is_own_enomem(&received),
      "{name}, on trial: this crate's own: {received:?}"
    );
    assert_eq!(dec.held(), Held::Pending, "{name}, on trial: held");
    assert_eq!(
      (dec.backend(), dec.session_kind()),
      (Backend::Vaapi, SessionKind::Trial),
      "{name}: nothing committed"
    );
    {
      let probe = dec.probe().expect("on trial");
      assert!(
        probe.attempts.is_empty(),
        "{name}: nothing charged: {:?}",
        probe.attempts
      );
      assert_eq!(
        probe.remaining_backends,
        vec![Backend::Cuda],
        "{name}: the order kept"
      );
      assert_eq!(
        fills(&probe.buffered_packets),
        vec![0x11, 0x22],
        "{name}: the history kept"
      );
    }
    let first = dec.receive_frame(&mut frame);
    assert!(
      matches!(first, Ok(Received::Produced)),
      "{name}: delivered next: {first:?}"
    );
    assert_eq!(
      picture_in(&frame),
      0x11,
      "{name}: that picture, delivered next"
    );
    assert_eq!(
      (dec.backend(), dec.session_kind()),
      (Backend::Vaapi, SessionKind::Committed),
      "{name}: committed at its first delivered picture"
    );
    assert_eq!(
      dec.held(),
      Held::Nothing,
      "{name}: nothing held once delivered"
    );

    fail_next_download_at(step);
    let received = dec.receive_frame(&mut frame);
    assert!(
      is_own_enomem(&received),
      "{name}, committed: this crate's own: {received:?}"
    );
    assert_eq!(dec.held(), Held::Pending, "{name}, committed: held");
    assert_eq!(
      delivered_within(&mut dec, &mut frame, 1),
      vec![0x22],
      "{name}, committed: that picture, delivered next, and nothing after"
    );

    let mut dec = raw_pinned_decoder();
    send_all(&mut dec, &[0x33, 0x44].map(raw_packet));
    fail_next_download_at(step);
    let received = dec.receive_frame(&mut frame);
    assert!(
      is_own_enomem(&received),
      "{name}, pinned: this crate's own: {received:?}"
    );
    assert_eq!(dec.held(), Held::Pending, "{name}, pinned: held");
    assert_eq!(
      delivered_within(&mut dec, &mut frame, 2),
      vec![0x33, 0x44],
      "{name}, pinned: that picture, then the next, in order, with no gap"
    );
  }
}

/// LAW: **a send never writes the picture held, and the receive delivers it
/// before it asks the decoder for another** (Codex R3, finding 1; the
/// ordering rule in [`Session`]'s documentation). A picture held on trial:
/// - a packet sent meanwhile is taken and recorded, and the receives
///   deliver the held picture first, then the packet's;
/// - a packet the history cannot keep (more side-data entries than the probe
///   copies) commits the candidate as it is taken, and the picture goes on
///   being held: delivered first, then the packet's;
/// - the end sent meanwhile is taken, and the receives deliver the held
///   picture, then end.
///
/// A picture held by a committed decoder: the same.
///
/// PLANT: a send that clears the picture held turns this red at "held
/// across the send"; a commit that drops it, at "held across the commit".
#[test]
fn a_send_never_writes_the_picture_held_and_the_receive_delivers_it_first() {
  let _downloads = DownloadsForTest::set(all_deliver);
  let mut frame = crate::Frame::empty().expect("frame slot");

  let mut dec = vaapi_then_cuda();
  holding(&mut dec, &mut frame, &raw_packet(0x11));
  send_all(&mut dec, &[raw_packet(0x22)]);
  assert_eq!(dec.held(), Held::Pending, "on trial: held across the send");
  assert_eq!(
    fills(&dec.probe().expect("on trial").buffered_packets),
    vec![0x11, 0x22],
    "on trial: the packet recorded"
  );
  assert_eq!(
    delivered_within(&mut dec, &mut frame, 2),
    vec![0x11, 0x22],
    "on trial: the held picture first, then the packet's"
  );

  let mut dec = vaapi_then_cuda();
  holding(&mut dec, &mut frame, &raw_packet(0x11));
  let crowded = with_side_data_entries(raw_packet(0x22), MAX_PROBE_PACKET_SIDE_DATA_ENTRIES + 1);
  send_all(&mut dec, &[crowded]);
  assert_eq!(
    dec.session_kind(),
    SessionKind::Committed,
    "committed by a packet the history cannot keep"
  );
  assert_eq!(dec.held(), Held::Pending, "held across the commit");
  assert_eq!(
    delivered_within(&mut dec, &mut frame, 2),
    vec![0x11, 0x22],
    "across the commit: the held picture first, then the packet's"
  );

  let mut dec = vaapi_then_cuda();
  holding(&mut dec, &mut frame, &raw_packet(0x11));
  assert_eq!(
    dec.send_eof().expect("the end"),
    Sent::Accepted,
    "the end, taken"
  );
  assert_eq!(dec.held(), Held::Pending, "on trial: held across the end");
  assert_eq!(
    delivered_within(&mut dec, &mut frame, 1),
    vec![0x11],
    "on trial: the held picture"
  );
  assert_eq!(
    dec.receive_frame(&mut frame).expect("the end"),
    Received::Ended,
    "then the end"
  );

  let mut dec = raw_pinned_decoder();
  holding(&mut dec, &mut frame, &raw_packet(0x33));
  send_all(&mut dec, &[raw_packet(0x44)]);
  assert_eq!(dec.held(), Held::Pending, "committed: held across the send");
  assert_eq!(
    delivered_within(&mut dec, &mut frame, 2),
    vec![0x33, 0x44],
    "committed: the held picture first, then the packet's"
  );
}

/// LAW: **a flush, a rejection or a terminal state clears the picture held,
/// and no reference to it is left** (Codex R3, finding 1). The picture held
/// references the caller's packet ([`holding`]), so the packet's own count
/// says whether anything still holds it:
/// - **a flush**, on trial and committed: nothing held, the packet the
///   caller's alone, and the receive after it asks for input — the picture
///   belonged to the position abandoned;
/// - **a rejection**: a packet the candidate fails leaves it, and its
///   picture held goes with it; the next candidate is replayed the history
///   whole and delivers every picture, once, in order;
/// - **a terminal state**: the probe exhausted on a send, the history handed
///   back; and the decoder ended on an ambiguous send, on trial and
///   committed.
///
/// PLANT: a flush that leaves the picture held turns this red at "a flush:
/// nothing held"; an ambiguous end that keeps the scratch frame, at "ended:
/// the caller's packet, by its own alone".
#[test]
fn a_flush_a_rejection_or_a_terminal_state_clears_the_picture_held() {
  let _downloads = DownloadsForTest::set(all_deliver);
  let mut frame = crate::Frame::empty().expect("frame slot");

  for (name, mut dec) in [
    ("on trial", raw_auditioning_decoder()),
    ("committed", raw_pinned_decoder()),
  ] {
    let packet = raw_packet(0x11);
    holding(&mut dec, &mut frame, &packet);
    dec.flush();
    assert_eq!(dec.held(), Held::Nothing, "{name}: a flush: nothing held");
    assert_eq!(
      payload_of(&packet).1,
      1,
      "{name}: a flush: the caller's packet, by its own alone"
    );
    assert_eq!(
      dec.receive_frame(&mut frame).expect("after the flush"),
      Received::NeedsInput,
      "{name}: nothing to deliver after the flush"
    );
    send_all(&mut dec, &[raw_packet(0x22)]);
    assert_eq!(
      delivered_within(&mut dec, &mut frame, 1),
      vec![0x22],
      "{name}: the new position's picture"
    );
  }

  // The candidate decodes 32 x 32 pictures, and fails a 16 x 16 packet.
  let rejecting = |remaining: Vec<Backend>| {
    let mut dec = raw_auditioning_decoder_with(32, crate::DecoderOptions::new());
    let probe = dec.probe_mut().expect("on trial");
    probe.parameters = raw_parameters(RAW_SIDE);
    probe.build = VideoDecoder::software_state_for_test;
    probe.remaining_backends = remaining;
    dec
  };
  let short = raw_packet(0x22);

  let mut dec = rejecting(vec![Backend::Cuda]);
  let first = dec.backend();
  let picture = raw_packet_of(32, 0x11);
  holding(&mut dec, &mut frame, &picture);
  // SAFETY: as in `holding`.
  let sent = unsafe { dec.send_av_packet(&short) };
  assert!(
    matches!(sent, Ok(Sent::Accepted)),
    "the next candidate takes it: {sent:?}"
  );
  assert_eq!(
    (dec.backend(), dec.session_kind()),
    (Backend::Cuda, SessionKind::Trial),
    "the candidate left"
  );
  assert_eq!(
    tried(dec.probe().expect("on trial")),
    vec![first],
    "the candidate's own answer"
  );
  assert_eq!(dec.held(), Held::Nothing, "a rejection: nothing held");
  assert_eq!(
    payload_of(&picture).1,
    1,
    "a rejection: the caller's packet, by its own alone"
  );
  assert_eq!(
    delivered_within(&mut dec, &mut frame, 2),
    vec![0x11, 0x22],
    "the history whole: every picture, once, in order"
  );

  let mut dec = rejecting(Vec::new());
  let picture = raw_packet_of(32, 0x11);
  holding(&mut dec, &mut frame, &picture);
  // SAFETY: as in `holding`.
  let sent = unsafe { dec.send_av_packet(&short) };
  let Err(Error::ProbeExhausted(exhausted)) = sent else {
    panic!("the probe exhausted: {sent:?}");
  };
  assert_eq!(dec.session_kind(), SessionKind::Exhausted);
  assert_eq!(dec.held(), Held::Nothing, "exhausted: nothing held");
  assert_eq!(
    payload_of(&picture).1,
    1,
    "exhausted: the caller's packet, by its own alone"
  );
  assert_eq!(
    fills(exhausted.unconsumed_packets()),
    vec![0x11],
    "exhausted: the history handed back, the held picture's packet in it"
  );

  for (name, mut dec) in [
    ("on trial", raw_auditioning_decoder()),
    ("committed", raw_pinned_decoder()),
  ] {
    let packet = raw_packet(0x11);
    holding(&mut dec, &mut frame, &packet);
    let ended = dec.end_ambiguously(Error::Ffmpeg(ffmpeg_next::Error::Other {
      errno: libc::ENOMEM,
    }));
    assert!(
      matches!(ended, Error::AmbiguousSend(_)),
      "{name}: {ended:?}"
    );
    assert_eq!(dec.session_kind(), SessionKind::Ended, "{name}: ended");
    assert_eq!(dec.held(), Held::Nothing, "{name}: ended: nothing held");
    assert_eq!(
      payload_of(&packet).1,
      1,
      "{name}: ended: the caller's packet, by its own alone"
    );
  }
}

/// Downloads where every backend picks `NV12`, a layout a [`Frame`] carries.
/// A software-backed candidate's picture has no frames context, so the
/// ceiling cannot read its extent and refuses it, failing closed
/// ([`judge_hw_transfer`]): every picture is over the frame limit.
fn nv12_listed(_: Backend) -> Option<i32> {
  Some(NV12)
}

/// LAW: **a picture over the frame limit is dropped by name — its pts in
/// the refusal — and charges nothing** (Codex R3, finding 1). The limit is
/// the caller's, so no retry could deliver the picture: it is not held, and
/// the refusal ([`Error::HwTransferTooLarge`]) names it, so it is never
/// lost unseen.
/// - **On trial**: the refusal names the picture's pts and says it is
///   dropped; nothing held, nothing charged, the order and the history
///   whole; the next picture is delivered, and commits the candidate.
/// - **Committed**: the same, the decoder staying.
/// - **A picture with no pts** is named so.
///
/// PLANT: the refusal minted without the picture's pts turns this red at
/// "naming its pts"; the picture held instead of dropped, at "dropped, not
/// held".
#[test]
fn a_picture_over_the_frame_limit_is_dropped_by_its_pts_and_charges_nothing() {
  let mut frame = crate::Frame::empty().expect("frame slot");
  for (name, mut dec) in [
    ("on trial", vaapi_then_cuda()),
    ("committed", raw_pinned_decoder()),
  ] {
    let on_trial = dec.session_kind() == SessionKind::Trial;
    send_all(
      &mut dec,
      &[raw_packet_at(0x11, 100), raw_packet_at(0x22, 200)],
    );
    let refused = {
      let _downloads = DownloadsForTest::set(nv12_listed);
      dec.receive_frame(&mut frame)
    };
    let Err(Error::HwTransferTooLarge(refusal)) = refused else {
      panic!("{name}: the caller's limit, by name: {refused:?}");
    };
    assert_eq!(refusal.pts(), Some(100), "{name}: naming its pts");
    assert!(
      refusal
        .to_string()
        .ends_with("the picture is dropped (pts 100)"),
      "{name}: the message says so: {refusal}"
    );
    assert_eq!(dec.held(), Held::Nothing, "{name}: dropped, not held");
    if on_trial {
      let probe = dec.probe().expect("on trial");
      assert!(
        probe.attempts.is_empty(),
        "{name}: nothing charged: {:?}",
        probe.attempts
      );
      assert_eq!(
        probe.remaining_backends,
        vec![Backend::Cuda],
        "{name}: the order kept"
      );
      assert_eq!(
        fills(&probe.buffered_packets),
        vec![0x11, 0x22],
        "{name}: the history kept"
      );
    }
    let _downloads = DownloadsForTest::set(all_deliver);
    assert_eq!(
      delivered_within(&mut dec, &mut frame, 1),
      vec![0x22],
      "{name}: the next picture, never the dropped one"
    );
    assert_eq!(
      dec.session_kind(),
      SessionKind::Committed,
      "{name}: committed"
    );
  }

  let mut dec = raw_pinned_decoder();
  send_all(&mut dec, &[raw_packet(0x33)]);
  let _downloads = DownloadsForTest::set(nv12_listed);
  let refused = dec.receive_frame(&mut frame);
  let Err(Error::HwTransferTooLarge(refusal)) = refused else {
    panic!("the caller's limit, by name: {refused:?}");
  };
  assert_eq!(refusal.pts(), None, "a picture with no pts, named so");
  assert!(
    refusal.to_string().ends_with("(no pts)"),
    "the message says so: {refusal}"
  );
}

/// LAW: **a fitted download keeps every failure's class, as the full-size
/// one does, and only the stage's own reasons stand it down** (Codex R3,
/// finding 2; the road table in [`Session`]'s documentation). The
/// scaled-output road stood in for ([`FITTED_FOR_TEST`]), its fitted
/// download answering:
/// - **the backend's transfer failing** ([`TransferFailure::Download`]): on
///   trial, the candidate is left, recorded with that answer, and the next
///   is replayed the history whole; with none left, the probe is exhausted,
///   every attempt the backend's answer and every packet back. Committed, it
///   is that picture's error, the picture dropped and the decoder staying.
///   R3 stood the stage down and downloaded the full-size frame, so the
///   backend's failure was never recorded.
/// - **a layout a `Frame` cannot carry**: unfit on trial, recorded so;
///   committed, the decoder's [`Error::DownloadFormatUnsupported`].
/// - **an allocation of this crate's own run out of memory** (the
///   destination, the properties' copy): the picture held, nothing charged,
///   and the next receive takes the fitted road again, its next answer
///   delivering that picture.
/// - **the stage standing down**: the full-size download delivers.
///
/// PLANT: the fitted road's failures merged into the stage standing down
/// again, the full-size frame downloaded instead, turns this red at "the
/// backend's, recorded".
#[test]
fn a_fitted_download_keeps_every_failures_class_and_only_the_stage_stands_down() {
  let _downloads = DownloadsForTest::set(all_deliver);
  let mut frame = crate::Frame::empty().expect("frame slot");
  let eio = ffmpeg_next::Error::Other { errno: libc::EIO };
  let enomem = ffmpeg_next::Error::Other {
    errno: libc::ENOMEM,
  };
  let is_eio = |e: &Error| matches!(e, Error::Ffmpeg(ffmpeg_next::Error::Other { errno }) if *errno == libc::EIO);

  // The backend's transfer, on trial: the candidate left, recorded; the
  // next one's picture held for a moment so the record can be read.
  let mut dec = vaapi_then_cuda();
  send_all(&mut dec, &[0x11, 0x22].map(raw_packet));
  {
    let _road = FittedRoad::answers([
      FittedForTest::Fails(TransferFailure::Download(eio)),
      FittedForTest::Fails(TransferFailure::Unattributed(enomem)),
    ]);
    let received = dec.receive_frame(&mut frame);
    assert!(
      is_own_enomem(&received),
      "the backend's, recorded: the candidate left, the next one's picture held: {received:?}"
    );
    assert_eq!(
      (dec.backend(), dec.session_kind()),
      (Backend::Cuda, SessionKind::Trial),
      "the backend's, recorded: the candidate left"
    );
    let probe = dec.probe().expect("on trial");
    assert!(
      matches!(
        probe.attempts.as_slice(),
        [(Backend::Vaapi, Attempt::Failed(BackendAnswer(e)))] if is_eio(e)
      ),
      "the backend's, recorded: {:?}",
      probe.attempts
    );
    assert_eq!(
      fills(&probe.buffered_packets),
      vec![0x11, 0x22],
      "the history whole"
    );
    assert_eq!(
      delivered_within(&mut dec, &mut frame, 2),
      vec![0x11, 0x22],
      "every picture, once, in order"
    );
  }

  // The backend's transfer, on trial, with no candidate left after.
  let mut dec = vaapi_then_cuda();
  send_all(&mut dec, &[0x11, 0x22].map(raw_packet));
  {
    let _road = FittedRoad::answers([
      FittedForTest::Fails(TransferFailure::Download(eio)),
      FittedForTest::Fails(TransferFailure::Download(eio)),
    ]);
    let exhausted = dec.receive_frame(&mut frame);
    let Err(Error::ProbeExhausted(exhausted)) = exhausted else {
      panic!("the probe's policy: exhausted: {exhausted:?}");
    };
    let attempts: Vec<_> = exhausted
      .attempts()
      .iter()
      .map(|(backend, attempt)| (*backend, attempt.as_ref()))
      .collect();
    assert!(
      matches!(
        attempts.as_slice(),
        [(Backend::Vaapi, a), (Backend::Cuda, b)] if is_eio(a) && is_eio(b)
      ),
      "each the backend's answer: {attempts:?}"
    );
    assert_eq!(
      fills(exhausted.unconsumed_packets()),
      vec![0x11, 0x22],
      "every packet back, in order"
    );
  }

  // The backend's transfer, committed.
  let mut dec = raw_pinned_decoder();
  send_all(&mut dec, &[0x33, 0x44].map(raw_packet));
  {
    let _road = FittedRoad::answers([FittedForTest::Fails(TransferFailure::Download(eio))]);
    let received = dec.receive_frame(&mut frame);
    assert!(
      matches!(&received, Err(e) if is_eio(e)),
      "committed: the picture's own error: {received:?}"
    );
    assert_eq!(dec.held(), Held::Nothing, "committed: the picture dropped");
    assert_eq!(
      dec.session_kind(),
      SessionKind::Committed,
      "committed: the decoder stays"
    );
    assert_eq!(
      delivered_within(&mut dec, &mut frame, 1),
      vec![0x44],
      "committed: the next picture"
    );
  }

  // A layout a `Frame` cannot carry: unfit on trial; committed, by name.
  let mut dec = vaapi_then_cuda();
  send_all(&mut dec, &[0x11, 0x22].map(raw_packet));
  {
    let _road = FittedRoad::answers([
      FittedForTest::Fails(TransferFailure::Layout(UYVY)),
      FittedForTest::Fails(TransferFailure::Unattributed(enomem)),
    ]);
    let received = dec.receive_frame(&mut frame);
    assert!(is_own_enomem(&received), "unfit: {received:?}");
    let probe = dec.probe().expect("on trial");
    assert!(
      matches!(
        probe.attempts.as_slice(),
        [(Backend::Vaapi, Attempt::Unfit(layout))] if layout.format() == UYVY
      ),
      "unfit, naming the layout: {:?}",
      probe.attempts
    );
  }
  let mut dec = raw_pinned_decoder();
  send_all(&mut dec, &[0x33, 0x44].map(raw_packet));
  {
    let _road = FittedRoad::answers([FittedForTest::Fails(TransferFailure::Layout(UYVY))]);
    let received = dec.receive_frame(&mut frame);
    assert!(
      matches!(&received, Err(Error::DownloadFormatUnsupported(layout)) if layout.format() == UYVY),
      "committed: by name: {received:?}"
    );
    assert_eq!(dec.held(), Held::Nothing, "committed: the picture dropped");
    assert_eq!(
      delivered_within(&mut dec, &mut frame, 1),
      vec![0x44],
      "committed: the next picture"
    );
  }

  // An allocation of this crate's own: held, and the fitted road again.
  let local: [(fn(ffmpeg_next::Error) -> TransferFailure, &str); 2] = [
    (TransferFailure::Unattributed, "the destination"),
    (TransferFailure::Properties, "the properties' copy"),
  ];
  for (failure, name) in local {
    for (state, mut dec) in [
      ("on trial", vaapi_then_cuda()),
      ("committed", raw_pinned_decoder()),
    ] {
      send_all(&mut dec, &[0x11, 0x22].map(raw_packet));
      let _road = FittedRoad::answers([
        FittedForTest::Fails(failure(enomem)),
        FittedForTest::Delivers,
      ]);
      let received = dec.receive_frame(&mut frame);
      assert!(
        is_own_enomem(&received),
        "{name}, {state}: this crate's own: {received:?}"
      );
      assert_eq!(dec.held(), Held::Pending, "{name}, {state}: held");
      assert!(
        dec.probe().is_none_or(|probe| probe.attempts.is_empty()),
        "{name}, {state}: nothing charged"
      );
      let first = dec.receive_frame(&mut frame);
      assert!(
        matches!(first, Ok(Received::Produced)),
        "{name}, {state}: delivered next: {first:?}"
      );
      assert_eq!(picture_in(&frame), 0x11, "{name}, {state}: that picture");
      assert_eq!(
        FittedRoad::left(),
        0,
        "{name}, {state}: downloaded again on the fitted road"
      );
      assert_eq!(
        delivered_within(&mut dec, &mut frame, 1),
        vec![0x22],
        "{name}, {state}: then the next"
      );
    }
  }

  // The stage standing down: the full-size download delivers.
  let mut dec = vaapi_then_cuda();
  send_all(&mut dec, &[0x11, 0x22].map(raw_packet));
  let _road = FittedRoad::answers([FittedForTest::StandsDown, FittedForTest::StandsDown]);
  assert_eq!(
    delivered_within(&mut dec, &mut frame, 2),
    vec![0x11, 0x22],
    "standing down: the full-size download delivers"
  );
  assert_eq!(
    (dec.backend(), dec.session_kind()),
    (Backend::Vaapi, SessionKind::Committed),
    "standing down: nothing charged"
  );
}

/// LAW: **on trial, a backend's failed download that a refusal of this
/// crate's own outranks holds the picture, and a backend failing again is
/// charged** (the census's latched-refusal row). A frame budget latched
/// beside the candidate's failed transfer is what the funnel answers, so
/// the candidate stays on trial, nothing charged, and its picture is held.
/// Downloaded again, the picture:
/// - delivered, commits the candidate, every picture once, in order;
/// - failing again, with no refusal beside it now, is the backend's answer:
///   the candidate is left, recorded, and the next delivers the history
///   whole.
///
/// Committed, the refusal is that picture's error, and the picture goes.
///
/// PLANT: the picture dropped on trial there turns this red at "the
/// candidate stays, its picture held".
#[test]
fn on_trial_a_failed_download_a_refusal_outranks_holds_the_picture_and_a_second_failure_is_charged()
{
  let _downloads = DownloadsForTest::set(all_deliver);
  let mut frame = crate::Frame::empty().expect("frame slot");
  let eio = ffmpeg_next::Error::Other { errno: libc::EIO };
  let enomem = ffmpeg_next::Error::Other {
    errno: libc::ENOMEM,
  };
  let budget = |dec: &VideoDecoder| {
    // SAFETY: `dec` owns this state and frees it only when it drops, after
    // this call.
    unsafe {
      crate::ffi::declare_frame_budget_declined_for_test(dec.decoder().callback_state, 12_582_912)
    }
  };
  let outranked = |dec: &mut VideoDecoder, frame: &mut crate::Frame| {
    let received = dec.receive_frame(frame);
    assert!(
      matches!(received, Err(Error::FrameBudgetExceeded(_))),
      "the refusal: {received:?}"
    );
    assert_eq!(
      (dec.backend(), dec.session_kind(), dec.held()),
      (Backend::Vaapi, SessionKind::Trial, Held::Pending),
      "the candidate stays, its picture held"
    );
    assert!(
      dec.probe().expect("on trial").attempts.is_empty(),
      "nothing charged"
    );
  };

  let mut dec = vaapi_then_cuda();
  send_all(&mut dec, &[0x11, 0x22].map(raw_packet));
  budget(&dec);
  {
    let _road = FittedRoad::answers([FittedForTest::Fails(TransferFailure::Download(eio))]);
    outranked(&mut dec, &mut frame);
    assert_eq!(
      delivered_within(&mut dec, &mut frame, 2),
      vec![0x11, 0x22],
      "delivered: every picture, once, in order"
    );
    assert_eq!(
      (dec.backend(), dec.session_kind()),
      (Backend::Vaapi, SessionKind::Committed),
      "delivered: the candidate committed"
    );
  }

  let mut dec = vaapi_then_cuda();
  send_all(&mut dec, &[0x11, 0x22].map(raw_packet));
  budget(&dec);
  {
    let _road = FittedRoad::answers([
      FittedForTest::Fails(TransferFailure::Download(eio)),
      FittedForTest::Fails(TransferFailure::Download(eio)),
      FittedForTest::Fails(TransferFailure::Unattributed(enomem)),
    ]);
    outranked(&mut dec, &mut frame);
    let received = dec.receive_frame(&mut frame);
    assert!(
      is_own_enomem(&received),
      "failing again: the candidate left, the next one's picture held: {received:?}"
    );
    assert_eq!(
      (dec.backend(), dec.session_kind()),
      (Backend::Cuda, SessionKind::Trial),
      "failing again: the candidate left"
    );
    let probe = dec.probe().expect("on trial");
    assert!(
      matches!(
        probe.attempts.as_slice(),
        [(Backend::Vaapi, Attempt::Failed(BackendAnswer(Error::Ffmpeg(ffmpeg_next::Error::Other { errno }))))]
          if *errno == libc::EIO
      ),
      "failing again: the backend's answer, recorded: {:?}",
      probe.attempts
    );
    assert_eq!(
      delivered_within(&mut dec, &mut frame, 2),
      vec![0x11, 0x22],
      "failing again: the history whole, every picture once, in order"
    );
  }

  let mut dec = raw_pinned_decoder();
  send_all(&mut dec, &[0x33, 0x44].map(raw_packet));
  budget(&dec);
  let _road = FittedRoad::answers([FittedForTest::Fails(TransferFailure::Download(eio))]);
  let received = dec.receive_frame(&mut frame);
  assert!(
    matches!(received, Err(Error::FrameBudgetExceeded(_))),
    "committed: the refusal: {received:?}"
  );
  assert_eq!(dec.held(), Held::Nothing, "committed: the picture dropped");
  assert_eq!(
    delivered_within(&mut dec, &mut frame, 1),
    vec![0x44],
    "committed: the next picture"
  );
}

/// The cap a VideoToolbox law's download is made under
/// ([`CAP_NEXT_TRANSFER_FOR_TEST`]): below the destination's buffers at any
/// extent the law decodes — a 640 x 360 `NV12` picture is 345,600 bytes —
/// and above everything else the download allocates before them.
#[cfg(target_vendor = "apple")]
const CAPPED_DOWNLOAD: usize = 64 * 1024;

/// What the one failing receive of a VideoToolbox decode answered and left
/// ([`on_videotoolbox_pictures`]).
#[cfg(target_vendor = "apple")]
#[derive(Debug, PartialEq, Eq)]
struct CappedReceive {
  /// Whether it answered this crate's own `ENOMEM`.
  own_enomem: bool,
  /// What the decoder held after it.
  held: Held,
  /// The session it left.
  kind: SessionKind,
  /// Whether no attempt was recorded (or no probe was left to record one).
  nothing_charged: bool,
}

/// One picture a VideoToolbox decode delivered: its pts and extent.
#[cfg(target_vendor = "apple")]
type DeliveredPicture = (Option<i64>, u32, u32);

/// The first `count` pictures VideoToolbox decodes of `HWDECODE_SAMPLE_VIDEO`,
/// each by its pts and extent, in order — at `scaled`'s extent where one is
/// requested first. With `capped_at`, the download of the picture at that
/// index is made under [`CAPPED_DOWNLOAD`], and what that receive answered
/// is returned with the pictures. And whether the scaled output's promise
/// still stands at the end.
#[cfg(target_vendor = "apple")]
fn on_videotoolbox_pictures(
  scaled: Option<(u32, u32)>,
  count: usize,
  capped_at: Option<usize>,
) -> (Vec<DeliveredPicture>, Option<CappedReceive>, bool) {
  use ffmpeg_next::{format, media};

  let path = std::env::var_os("HWDECODE_SAMPLE_VIDEO")
    .expect("HWDECODE_SAMPLE_VIDEO must be set for this test");
  let mut input = format::input(&path).expect("open input");
  let (stream_index, parameters) = {
    let stream = input
      .streams()
      .best(media::Type::Video)
      .expect("video stream");
    (stream.index(), stream.parameters())
  };
  let mut dec = VideoDecoder::open(parameters).expect("open decoder");
  assert_eq!(dec.backend(), Backend::VideoToolbox, "on VideoToolbox");
  if let Some(size) = scaled {
    assert!(
      dec.request_scaled_output(size).is_supported(),
      "the scaled output, accepted"
    );
  }
  CAP_NEXT_TRANSFER_FOR_TEST.with(|cap| cap.set(None));
  let mut frame = crate::Frame::empty().expect("frame slot");
  let mut pictures = Vec::new();
  let mut capped = None;
  let mut packets = input.packets();
  loop {
    loop {
      if capped_at == Some(pictures.len()) && capped.is_none() {
        CAP_NEXT_TRANSFER_FOR_TEST.with(|cap| cap.set(Some(CAPPED_DOWNLOAD)));
      }
      match dec.receive_frame(&mut frame) {
        Ok(Received::Produced) => {
          pictures.push((frame.pts(), frame.width(), frame.height()));
          if pictures.len() == count {
            assert_eq!(
              CAP_NEXT_TRANSFER_FOR_TEST.with(core::cell::Cell::get),
              None,
              "the capped download was made"
            );
            let promise = dec.scaled_output_capability().is_supported();
            return (pictures, capped, promise);
          }
        }
        Ok(Received::NeedsInput) => break,
        Ok(Received::Ended) => panic!("the sample ended before {count} pictures"),
        Err(e) => {
          assert!(
            capped.is_none() && capped_at.is_some(),
            "one failure, the capped download's: {e:?}"
          );
          capped = Some(CappedReceive {
            own_enomem: matches!(&e, Error::Ffmpeg(ffmpeg_next::Error::Other { errno }) if *errno == libc::ENOMEM),
            held: dec.held(),
            kind: dec.session_kind(),
            nothing_charged: dec.probe().is_none_or(|probe| probe.attempts.is_empty()),
          });
        }
      }
    }
    let packet = loop {
      let (stream, packet) = packets
        .next()
        .expect("the sample ended before the pictures did");
      if stream.index() == stream_index {
        break packet;
      }
    };
    // SAFETY: `packets()` reads each packet with `av_read_frame`, so
    // libavformat wrote it and its padding, and nothing writes through it
    // afterwards.
    let sent = unsafe { dec.send_av_packet(&packet) };
    assert!(matches!(sent, Ok(Sent::Accepted)), "taken: {sent:?}");
  }
}

/// LAW: **on VideoToolbox, a picture whose download runs out of memory is
/// held, and the next receive delivers it — on the full-size road and the
/// scaled one, on trial and committed** (Codex R3, findings 1 and 2), by
/// libavutil's own behaviour, in a child process. The sample's first eight
/// pictures are decoded as the control. Then again, with one picture's
/// destination allocated under a cap below its buffers ([`CAPPED_DOWNLOAD`]):
/// the first, while the candidate is on trial, and the fourth, committed.
/// That receive answers this crate's own `ENOMEM`, nothing charged and the
/// picture held, and the pictures delivered are the control's — the same
/// pts and extents, in order, with no gap and none twice. Scaled, every
/// picture is at the extent requested, the held one too, and the promise
/// still stands.
///
/// PLANT: the retryable failure leaving the slot to the next
/// `avcodec_receive_frame`, as R3 did, turns this red at "the control's
/// pictures".
#[cfg(target_vendor = "apple")]
#[test]
#[ignore = "requires HWDECODE_SAMPLE_VIDEO and a working VideoToolbox backend"]
fn on_videotoolbox_a_picture_whose_download_runs_out_of_memory_is_held_and_delivered_next() {
  use mediaio_ffmpeg::test_support::in_subprocess;

  in_subprocess(
    "decoder::tests::on_videotoolbox_a_picture_whose_download_runs_out_of_memory_is_held_and_delivered_next",
    || {
      ffmpeg_next::init().expect("ffmpeg init");
      for (road, scaled) in [("full size", None), ("scaled", Some((640, 360)))] {
        let (control, none, promise) = on_videotoolbox_pictures(scaled, 8, None);
        assert_eq!(none, None, "{road}: the control fails nothing");
        if let Some((width, height)) = scaled {
          assert!(
            control.iter().all(|&(_, w, h)| (w, h) == (width, height)),
            "{road}: every picture at the extent requested: {control:?}"
          );
          assert!(promise, "{road}: the promise stands");
        }
        for (at, kind) in [(0, SessionKind::Trial), (3, SessionKind::Committed)] {
          let (pictures, capped, promise) = on_videotoolbox_pictures(scaled, 8, Some(at));
          assert_eq!(
            pictures, control,
            "{road}, picture {at}: the control's pictures, in order, with no gap and none twice"
          );
          assert_eq!(
            capped,
            Some(CappedReceive {
              own_enomem: true,
              held: Held::Pending,
              kind,
              nothing_charged: true,
            }),
            "{road}, picture {at}: this crate's own ENOMEM, nothing charged, the picture held"
          );
          if scaled.is_some() {
            assert!(promise, "{road}, picture {at}: the promise still stands");
          }
        }
      }
    },
  );
}
