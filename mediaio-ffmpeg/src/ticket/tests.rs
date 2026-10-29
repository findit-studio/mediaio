use super::*;

use crate::extras::TrackExtra;

/// Builds an `AVCodecParameters` with the heap fields a file controls.
///
/// The same discipline the bounded clone's own fixture uses: a shape
/// the `ffmpeg` CLI cannot mint is built here, by hand, beside the
/// assertions it feeds.
fn parameters_with(extradata: usize, icc_profile: usize) -> Parameters {
  let mut out = Parameters::new();
  // SAFETY: `out` owns a live `AVCodecParameters`. Every buffer below
  // comes from FFmpeg's allocator and is handed to it, so
  // `avcodec_parameters_free` releases all of them with the struct.
  unsafe {
    let par = out.as_mut_ptr();
    if extradata > 0 {
      let buffer = av_mallocz(extradata).cast::<u8>();
      assert!(!buffer.is_null(), "av_mallocz extradata");
      core::ptr::write_bytes(buffer, 0xAB, extradata);
      (*par).extradata = buffer;
      (*par).extradata_size = extradata as i32;
    }
    if icc_profile > 0 {
      let array = av_mallocz(core::mem::size_of::<AVPacketSideData>()).cast::<AVPacketSideData>();
      assert!(!array.is_null(), "av_mallocz side-data array");
      let payload = av_mallocz(icc_profile).cast::<u8>();
      assert!(!payload.is_null(), "av_mallocz icc profile");
      core::ptr::write_bytes(payload, 0xCD, icc_profile);
      (*array).data = payload;
      (*array).size = icc_profile;
      (*array).type_ = ffmpeg_next::ffi::AVPacketSideDataType::AV_PKT_DATA_ICC_PROFILE;
      (*par).coded_side_data = array;
      (*par).nb_coded_side_data = 1;
    }
  }
  out
}

/// Builds an `AVCodecParameters` carrying `count` empty side-data
/// entries — enough descriptor array to outgrow a capped allocator
/// without any payload to outgrow it first.
fn parameters_with_side_data_entries(count: usize) -> Parameters {
  let mut out = Parameters::new();
  // SAFETY: `out` owns a live `AVCodecParameters`; the array comes from
  // FFmpeg's allocator and is handed to it, so `avcodec_parameters_free`
  // releases it with the struct. Every entry keeps the null payload
  // `av_mallocz` left, which `av_packet_side_data_free` frees as a
  // no-op.
  unsafe {
    let par = out.as_mut_ptr();
    let bytes = count * core::mem::size_of::<AVPacketSideData>();
    let array = av_mallocz(bytes).cast::<AVPacketSideData>();
    assert!(!array.is_null(), "av_mallocz side-data array");
    for index in 0..count {
      core::ptr::write_unaligned(
        core::ptr::addr_of_mut!((*array.add(index)).type_).cast::<i32>(),
        index as i32,
      );
    }
    (*par).coded_side_data = array;
    (*par).nb_coded_side_data = count as i32;
  }
  out
}

/// Builds an `AVCodecParameters` with an `AV_CHANNEL_ORDER_CUSTOM`
/// layout of `channels` mapped channels.
fn parameters_with_custom_layout(channels: usize) -> Parameters {
  let mut out = Parameters::new();
  // SAFETY: `out` owns a live `AVCodecParameters`; the map comes from
  // FFmpeg's allocator and is handed to it, so
  // `av_channel_layout_uninit` frees it with the struct.
  unsafe {
    let par = out.as_mut_ptr();
    let bytes = channels * core::mem::size_of::<AVChannelCustom>();
    let map = av_mallocz(bytes).cast::<AVChannelCustom>();
    assert!(!map.is_null(), "av_mallocz channel map");
    for index in 0..channels {
      core::ptr::write_unaligned(
        core::ptr::addr_of_mut!((*map.add(index)).id).cast::<i32>(),
        index as i32,
      );
    }
    core::ptr::write_unaligned(
      core::ptr::addr_of_mut!((*par).ch_layout.order).cast::<i32>(),
      AVChannelOrder::AV_CHANNEL_ORDER_CUSTOM as i32,
    );
    (*par).ch_layout.nb_channels = channels as i32;
    (*par).ch_layout.u.map = map;
  }
  out
}

/// [`parameters_with_custom_layout`], with the **last** entry's
/// sixteen-byte name filled edge to edge so it carries no NUL, and an
/// `extradata` field of `extradata` bytes beside it.
///
/// The last entry rather than the first on purpose: a validation that
/// walks only far enough to feel thorough passes this, and a
/// validation placed after the copies pays for `extradata` before
/// reaching it.
fn parameters_with_unterminated_last_name(channels: usize, extradata: usize) -> Parameters {
  let mut out = parameters_with_custom_layout(channels);
  // SAFETY: `out` owns a live `AVCodecParameters` whose custom map has
  // `channels` entries; the extradata buffer comes from FFmpeg's
  // allocator and is handed to it.
  unsafe {
    let par = out.as_mut_ptr();
    let map = (*par).ch_layout.u.map;
    core::ptr::write_bytes(
      core::ptr::addr_of_mut!((*map.add(channels - 1)).name).cast::<u8>(),
      b'x',
      16,
    );
    if extradata > 0 {
      let buffer = av_mallocz(extradata).cast::<u8>();
      assert!(!buffer.is_null(), "av_mallocz extradata");
      core::ptr::write_bytes(buffer, 0xAB, extradata);
      (*par).extradata = buffer;
      (*par).extradata_size = extradata as i32;
    }
  }
  out
}

/// **An unspecified layout's union is never read, mirrored, or
/// printed.**
///
/// `AVChannelLayout`'s own header declares `u` undefined for
/// `AV_CHANNEL_ORDER_UNSPEC`, and this mirror treated "not custom" as
/// "mask-backed" — importing whatever bytes happened to sit in that
/// storage into `ChannelLayoutTicket::mask`, where the public `mask()`
/// hands them out and the derived `Debug`, `PartialEq` and `Hash`
/// expose and compare them. Two equivalent unspecified layouts could
/// therefore mirror to tickets that were not equal.
///
/// The junk written below is what a container's uninitialised or
/// reused storage looks like. The stored zero is a value this crate
/// *chose*; nothing reads the union to obtain it.
#[test]
fn an_unspecified_layout_mirrors_without_reading_its_union() {
  let unspec = |channels: i32, junk: u64| {
    let mut out = Parameters::new();
    // SAFETY: `out` owns a live `AVCodecParameters`. `UNSPEC` is the
    // zero discriminant the allocator already left; the union is
    // written with bytes the order does not define, which is the state
    // under test.
    unsafe {
      let par = out.as_mut_ptr();
      core::ptr::write_unaligned(
        core::ptr::addr_of_mut!((*par).ch_layout.order).cast::<i32>(),
        AVChannelOrder::AV_CHANNEL_ORDER_UNSPEC as i32,
      );
      (*par).ch_layout.nb_channels = channels;
      (*par).ch_layout.u.mask = junk;
    }
    out
  };

  let clean = CodecTicket::mirror(&unspec(6, 0), 0, usize::MAX).expect("mirror");
  let junked =
    CodecTicket::mirror(&unspec(6, 0xDEAD_BEEF_DEAD_BEEF), 1, usize::MAX).expect("mirror");

  assert_eq!(
    clean.ch_layout().mask(),
    0,
    "nothing was read from the union"
  );
  assert_eq!(
    junked.ch_layout().mask(),
    0,
    "and nothing was read here either"
  );
  assert_eq!(
    clean.ch_layout(),
    junked.ch_layout(),
    "two unspecified layouts of one width are one layout",
  );

  let rendered = format!("{:?}", junked.ch_layout());
  assert!(
    !rendered.to_ascii_lowercase().contains("dead"),
    "the junk must not reach a log line: {rendered}",
  );

  // The rebuild does not invent one either: an order that defines no
  // arm gets no arm written.
  let rebuilt = junked.rebuild().expect("rebuild");
  // SAFETY: `rebuilt` owns a live `AVCodecParameters` for this read.
  let (order, channels, mask) = unsafe {
    let par = rebuilt.as_ptr();
    (
      core::ptr::read_unaligned(core::ptr::addr_of!((*par).ch_layout.order).cast::<i32>()),
      (*par).ch_layout.nb_channels,
      (*par).ch_layout.u.mask,
    )
  };
  assert_eq!(order, AVChannelOrder::AV_CHANNEL_ORDER_UNSPEC as i32);
  assert_eq!(channels, 6, "the count is carried across");
  assert_eq!(mask, 0, "and the union is left as the allocator zeroed it");

  // A native layout still round-trips its mask, so the assertions above
  // are about the order rather than about the mask field having gone
  // quiet.
  let native =
    CodecTicket::mirror(&parameters_with_native_layout(), 2, usize::MAX).expect("mirror");
  assert_ne!(native.ch_layout().mask(), 0);
}

/// Builds an `AVCodecParameters` with an ordinary `NATIVE` stereo
/// layout — the positive control for the lane above.
fn parameters_with_native_layout() -> Parameters {
  let mut out = Parameters::new();
  // SAFETY: `out` owns a live `AVCodecParameters`; both writes are
  // scalar stores into the `mask` arm the order names.
  unsafe {
    let par = out.as_mut_ptr();
    core::ptr::write_unaligned(
      core::ptr::addr_of_mut!((*par).ch_layout.order).cast::<i32>(),
      AVChannelOrder::AV_CHANNEL_ORDER_NATIVE as i32,
    );
    (*par).ch_layout.nb_channels = 2;
    (*par).ch_layout.u.mask = ffmpeg_next::ffi::AV_CH_LAYOUT_STEREO;
  }
  out
}

/// **A custom name with no terminator is refused, and refused as a
/// malformed map rather than as anything else.**
///
/// `AVChannelCustom::name` is documented as zeroed or NUL-terminated
/// and nothing enforces it. FFmpeg tests `name[0]` and hands the fixed
/// array to a `%s` conversion, which reads until a terminator — so a
/// name filled edge to edge makes that read run off the end of the
/// entry. The describe road refused it already; this road is the one
/// that mirrors the layout, and it used to admit it.
#[test]
fn a_custom_name_without_a_terminator_is_refused_at_the_mirror() {
  let parameters = parameters_with_unterminated_last_name(4, 0);
  match CodecTicket::mirror(&parameters, 5, usize::MAX) {
    Err(ParametersError::ChannelMap(p)) => {
      assert_eq!(p.stream_index(), 5);
      assert_eq!(p.channels(), 4);
    }
    Err(other) => panic!("expected ParametersChannelMap, got {other:?}"),
    Ok(_) => panic!("a name with no terminator must not reach FFmpeg"),
  }

  // The same map with the terminator `av_mallocz` leaves is fine, so
  // the refusal is about the terminator and not about custom layouts.
  CodecTicket::mirror(&parameters_with_custom_layout(4), 5, usize::MAX)
    .expect("a zeroed name is a terminated one");
}

/// **Structure is judged before the footprint is even measured**, which
/// is what puts it before every copy.
///
/// The ordering is invisible to a test that only checks *which* error
/// comes back for a file with one fault — both orderings refuse. So
/// this file has **two** faults at once: a malformed channel map and an
/// `extradata` field far over the budget. Whichever check runs first
/// names the error, and that makes the assertion a statement about
/// order rather than about outcome.
///
/// Before the fix, `from_raw` measured and judged the footprint, then
/// copied `extradata` and every coded-side-data payload, and only then
/// reached the channel layout — so this file answered
/// `ParametersTooLarge`, and a file inside the budget paid for both
/// copies before being refused.
#[test]
fn a_malformed_layout_is_refused_before_the_footprint_is_measured() {
  let parameters = parameters_with_unterminated_last_name(4, 64 * 1024);
  // The footprint really is over the ceiling used below, so the other
  // ordering had a refusal of its own to reach for.
  //
  // SAFETY: `parameters` owns a live `AVCodecParameters`.
  let declared = unsafe { measure_parameters(parameters.as_ptr()) }
    .and_then(|f| f.total())
    .expect("measurable");
  assert!(declared > 1024, "the extradata must outgrow the ceiling");

  match CodecTicket::mirror(&parameters, 5, 1024) {
    Err(ParametersError::ChannelMap(p)) => assert_eq!(p.channels(), 4),
    Err(other) => panic!("structure must be judged before size, got {other:?}"),
    Ok(_) => panic!("a malformed map must be refused"),
  }
}

/// **Every allocation the rebuild makes, made to fail.**
///
/// `rebuild` asks FFmpeg's allocator four separate times — the
/// `extradata` buffer, the `coded_side_data` descriptor array, each
/// entry's payload, and a custom channel map — and each of those four
/// has a null check whose branch nothing else in this suite reaches.
/// An unreached null check is a null check nobody has confirmed
/// *reports* rather than proceeds, which on this road would mean
/// handing a decoder a half-built `AVCodecParameters`.
///
/// So the allocator is capped and each branch is driven in turn, in
/// the shape `demuxer::tests::codec_parameters_whose_copy_fails_are_named`
/// established: `av_max_alloc` is process-global, so this runs in a
/// child of the test binary, alone.
///
/// The partially-built `Parameters` is dropped inside each `rebuild`
/// that fails — `out` is a local, and the error return drops it — so
/// the destructor walk over a struct whose fields are half-attached
/// executes here too. That is the other half of what these branches
/// promise: not merely that they report, but that what they report
/// over can be freed.
#[test]
fn every_rebuild_allocation_that_fails_is_named() {
  crate::fault_subprocess::in_subprocess(
    "ticket::tests::every_rebuild_allocation_that_fails_is_named",
    || {
      const HUGE: usize = 8 * 1024 * 1024;

      // Each case: what the ticket holds, the cap that admits the
      // struct itself but refuses the field, and the field's name.
      let extradata =
        CodecTicket::mirror(&parameters_with(HUGE, 0), 2, usize::MAX).expect("an uncapped mirror");
      let payload =
        CodecTicket::mirror(&parameters_with(0, HUGE), 3, usize::MAX).expect("an uncapped mirror");
      // 64 descriptors is 1.5 KiB of array — over a 1 KiB cap, while
      // the 184-byte `AVCodecParameters` itself still fits under it.
      let array = CodecTicket::mirror(&parameters_with_side_data_entries(64), 4, usize::MAX)
        .expect("an uncapped mirror");
      let layout = CodecTicket::mirror(&parameters_with_custom_layout(64), 5, usize::MAX)
        .expect("an uncapped mirror");

      for (ticket, cap, field, index) in [
        (&extradata, 64 * 1024, "extradata", 2usize),
        (&payload, 64 * 1024, "a side-data payload", 3),
        (&array, 1024, "the side-data descriptor array", 4),
        (&layout, 1024, "the custom channel map", 5),
      ] {
        // SAFETY: in `in_subprocess`'s child, alone. The capped window runs only
        // `rebuild`, which checks every allocation it makes.
        unsafe { crate::fault_subprocess::cap_ffmpeg_allocations(cap) };
        let refused = ticket.rebuild().map(|_| ());
        crate::fault_subprocess::uncap_ffmpeg_allocations();

        match refused {
          Err(ParametersError::Copy(ref p)) => assert_eq!(
            p.stream_index(),
            index,
            "{field}: the refusal named the wrong stream",
          ),
          Err(other) => panic!("{field}: expected ParametersCopy, got {other:?}"),
          Ok(()) => panic!("{field}: an allocation that could not happen was reported as success"),
        }

        // And the same rebuild succeeds once the allocator does, so
        // each refusal was the cap's answer and not a broken branch.
        let rebuilt = ticket.rebuild().expect("an uncapped rebuild");
        // SAFETY: `rebuilt` owns a live `AVCodecParameters`.
        let measured = unsafe { measure_parameters(rebuilt.as_ptr()) }
          .and_then(|f| f.total())
          .expect("measurable");
        assert_eq!(measured, ticket.footprint_bytes(), "{field}");
      }
    },
  );
}

/// The compile pin this whole road exists to raise.
///
/// `mediagraph`'s five decode households each write
/// `In: Send + Sync + 'static` into their own `where` clause, and the
/// streaming mount cell asks `Out: Send`. Every one of those bounds
/// failed on `TrackInfo<Ffmpeg>` for one reason: the row held a
/// `*mut AVCodecParameters`. The ticket removes the pointer, so the
/// auto-traits arrive structurally — and these three lines are what
/// stops them leaving again in silence.
#[test]
fn the_ticket_is_send_and_sync() {
  const fn assert_send_sync<T: Send + Sync>() {}
  assert_send_sync::<CodecTicket>();
  assert_send_sync::<ChannelLayoutTicket>();
  assert_send_sync::<CustomChannel>();
  assert_send_sync::<TrackExtra>();
  assert_send_sync::<mediaspine::demuxer::TrackInfo<crate::Ffmpeg>>();
  // And the shape the failure actually reached the application as:
  // `Arc<T>: Send` needs `T: Send + Sync`, one indirection down.
  assert_send_sync::<std::sync::Arc<mediaspine::demuxer::TrackInfo<crate::Ffmpeg>>>();
}

/// The `Omit` policy is the synthesized-attachment road: a font's
/// extradata **is** the attachment payload, the carrier already holds
/// it, and mirroring it would make the row carry the file twice.
#[test]
fn the_omit_policy_leaves_extradata_behind_and_charges_nothing_for_it() {
  let parameters = parameters_with(4_096, 128);
  let copied = CodecTicket::mirror_with(&parameters, 0, usize::MAX, ExtradataPolicy::Copy)
    .expect("mirror with extradata");
  let omitted = CodecTicket::mirror_with(&parameters, 0, usize::MAX, ExtradataPolicy::Omit)
    .expect("mirror without extradata");

  assert_eq!(copied.extradata().len(), 4_096);
  assert!(omitted.extradata().is_empty(), "the payload stayed behind");
  // Side data is untouched by the policy — only `extradata` is the
  // attachment's own payload.
  assert_eq!(omitted.coded_side_data().len(), 1);

  // And the budget agrees with the copy: omitting means never
  // allocating *and* never charging, which is the interval an earlier
  // strip-afterwards shape used to lose a file inside.
  const PAD: usize = AV_INPUT_BUFFER_PADDING_SIZE as usize;
  assert_eq!(
    copied.footprint_bytes() - omitted.footprint_bytes(),
    4_096 + PAD,
  );

  // The rebuild honours it: nothing allocated, nothing declared.
  let rebuilt = omitted.rebuild().expect("rebuild");
  // SAFETY: `rebuilt` owns a live `AVCodecParameters`.
  unsafe {
    let par = rebuilt.as_ptr();
    assert!((*par).extradata.is_null());
    assert_eq!((*par).extradata_size, 0);
  }
}

/// The footprint is a promise about the *rebuild*, so it is checked
/// against one — measured with the same counter `admit_streams` uses.
#[test]
fn the_footprint_is_exactly_what_the_rebuild_allocates() {
  for (extradata, icc) in [(0, 0), (32, 0), (0, 256), (4_096, 64 * 1024)] {
    let parameters = parameters_with(extradata, icc);
    let ticket = CodecTicket::mirror(&parameters, 0, usize::MAX).expect("mirror");
    let rebuilt = ticket.rebuild().expect("rebuild");
    // SAFETY: `rebuilt` owns a live `AVCodecParameters`.
    let measured = unsafe { measure_parameters(rebuilt.as_ptr()) }
      .and_then(|f| f.total())
      .expect("measurable");
    assert_eq!(
      ticket.footprint_bytes(),
      measured,
      "extradata {extradata}, icc {icc}",
    );
  }
}

/// The ceiling is judged before a byte is copied — the rule the whole
/// parameter road is written to, carried across to the mirror.
#[test]
fn an_oversized_mirror_is_refused_before_the_copy() {
  let parameters = parameters_with(0, 8 * 1024 * 1024);
  // SAFETY: `parameters` owns a live `AVCodecParameters`.
  let declared = unsafe { measure_parameters(parameters.as_ptr()) }
    .and_then(|f| f.total())
    .expect("measurable");

  match CodecTicket::mirror(&parameters, 3, 64 * 1024) {
    Err(ParametersError::TooLarge(p)) => {
      assert_eq!(p.stream_index(), 3);
      assert_eq!(p.bytes(), declared);
      assert_eq!(p.limit(), 64 * 1024);
    }
    Err(other) => panic!("expected ParametersTooLarge, got {other:?}"),
    Ok(_) => panic!("an 8 MiB ICC profile passed a 64 KiB ceiling"),
  }

  // Exactly at the line is not over it.
  CodecTicket::mirror(&parameters, 3, declared).expect("at the cap is not over it");
}

/// Null-backed parameters are refused where the raw pointer is, which
/// is what lets [`TrackExtra::new`] be infallible.
#[test]
fn null_backed_parameters_are_refused_at_the_mirror() {
  // `Parameters::new()` is a safe constructor over an unchecked
  // `avcodec_parameters_alloc`. It cannot be made to fail here, so the
  // null-backed shape is built directly: an empty `Parameters` whose
  // pointer this test never dereferences.
  let parameters = Parameters::default();
  // SAFETY: reading the pointer without dereferencing it.
  if unsafe { parameters.as_ptr() }.is_null() {
    assert!(matches!(
      CodecTicket::mirror(&parameters, 9, usize::MAX),
      Err(ParametersError::Missing(_)),
    ));
  }
}

/// The row's public numbers still come from one place, and the
/// duplicate is a duplicate.
#[test]
fn the_row_reports_the_tickets_footprint_and_clones_by_refcount() {
  let parameters = parameters_with(64, 128);
  let ticket = CodecTicket::mirror(&parameters, 7, usize::MAX).expect("mirror");
  let footprint = ticket.footprint_bytes();
  let extra = TrackExtra::new(7, ticket)
    .with_disposition(0x11)
    .with_start_time(Some(-3))
    .with_frame_count(Some(42));

  assert_eq!(extra.parameter_bytes(), footprint);
  assert_eq!(extra.ticket().extradata().len(), 64);

  let copy = extra.clone();
  assert_eq!(copy.stream_index(), 7);
  assert_eq!(copy.disposition(), 0x11);
  assert_eq!(copy.start_time(), Some(-3));
  assert_eq!(copy.frame_count(), Some(42));
  assert_eq!(copy.parameter_bytes(), footprint);
  // The payload was shared, not copied — the carrier law, one tier in.
  assert!(
    copy
      .ticket()
      .extradata_ref()
      .ptr_eq(extra.ticket().extradata_ref()),
    "cloning a row copied its extradata bytes",
  );
}

/// A `Debug` that printed the payload would put megabytes in a log.
#[test]
fn the_debug_prints_sizes_rather_than_payloads() {
  let parameters = parameters_with(64, 128);
  let ticket = CodecTicket::mirror(&parameters, 0, usize::MAX).expect("mirror");
  let text = format!("{ticket:?}");
  assert!(text.contains("extradata_len: 64"), "{text}");
  assert!(text.contains("coded_side_data: 1"), "{text}");
  assert!(!text.contains("171"), "the bytes themselves leaked: {text}");
}

// ---------------------------------------------------------------------------
//  The Dolby Vision configuration record — `AV_PKT_DATA_DOVI_CONF`.
// ---------------------------------------------------------------------------

/// Builds an `AVCodecParameters` carrying one `AV_PKT_DATA_DOVI_CONF`
/// entry whose payload is a byte-for-byte
/// `AVDOVIDecoderConfigurationRecord` (`dv_version_major,
/// dv_version_minor, dv_profile, dv_level, rpu_present_flag,
/// el_present_flag, bl_present_flag, dv_bl_signal_compatibility_id,
/// dv_md_compression` — nine one-byte fields, per
/// `libavutil/dovi_meta.h`).
///
/// No `ffmpeg` CLI recipe mints a `dvcC`/`dvvC` box (real Dolby Vision
/// encoding needs Dolby's own tools, which this workspace does not
/// carry), so — the same road [`parameters_with`]'s ICC-profile arm and
/// `codec_ticket_parity.rs`'s minted MOV both take for a field no
/// container the CLI can produce — the side-data entry is built by
/// hand, at the `AVCodecParameters` boundary [`CodecTicket::mirror`]
/// actually reads.
fn parameters_with_dovi_config(record: [u8; 9]) -> Parameters {
  let mut out = Parameters::new();
  // SAFETY: `out` owns a live `AVCodecParameters`; both allocations
  // come from FFmpeg's allocator and are handed to it, so
  // `avcodec_parameters_free` releases them with the struct.
  unsafe {
    let par = out.as_mut_ptr();
    let array = av_mallocz(core::mem::size_of::<AVPacketSideData>()).cast::<AVPacketSideData>();
    assert!(!array.is_null(), "av_mallocz side-data array");
    let payload = av_mallocz(record.len()).cast::<u8>();
    assert!(!payload.is_null(), "av_mallocz the dovi config record");
    core::ptr::copy_nonoverlapping(record.as_ptr(), payload, record.len());
    (*array).data = payload;
    (*array).size = record.len();
    core::ptr::write_unaligned(
      core::ptr::addr_of_mut!((*array).type_).cast::<i32>(),
      ffmpeg_next::ffi::AVPacketSideDataType::AV_PKT_DATA_DOVI_CONF as i32,
    );
    (*par).coded_side_data = array;
    (*par).nb_coded_side_data = 1;
  }
  out
}

/// Profile 8.1 (`dv_profile = 8`), base-layer compatibility id 1
/// ("HDR10 base layer") — a realistic streaming-service shape, read
/// through the whole mirror path a real MOV demuxer would also drive
/// `coded_side_data` through.
#[test]
fn dolby_vision_config_crosses_through_the_mirror() {
  // major=1 minor=0 profile=8 level=6 rpu=1 el=0 bl=1 compat_id=1 md_compression=0
  let record = [1u8, 0, 8, 6, 1, 0, 1, 1, 0];
  let parameters = parameters_with_dovi_config(record);
  let ticket = CodecTicket::mirror(&parameters, 9, usize::MAX).expect("mirror");

  let dovi = ticket
    .dolby_vision_config()
    .expect("the minted dvcC-shaped entry parses");
  assert_eq!(dovi.profile(), 8);
  assert_eq!(dovi.compatibility_id(), 1);
}

/// A stream with no `AV_PKT_DATA_DOVI_CONF` entry at all — the
/// overwhelming majority of real files — answers `None`, not a
/// default-valued record.
#[test]
fn dolby_vision_config_is_none_when_the_stream_carries_no_dovi_box() {
  let parameters = parameters_with(0, 0);
  let ticket = CodecTicket::mirror(&parameters, 10, usize::MAX).expect("mirror");
  assert!(ticket.dolby_vision_config().is_none());
}

/// An `AV_PKT_DATA_DOVI_CONF` entry present but too short to hold the
/// compatibility id (offset 7) is a version-skew or corrupt shape —
/// refused rather than read past the end.
#[test]
fn dolby_vision_config_refuses_a_short_payload() {
  // Minted directly rather than through the 9-byte helper: the
  // record's `profile`/`level`/flags are set but the array is shrunk
  // to 6 bytes — before the compatibility id at offset 7.
  let mut truncated = Parameters::new();
  unsafe {
    let par = truncated.as_mut_ptr();
    let array = av_mallocz(core::mem::size_of::<AVPacketSideData>()).cast::<AVPacketSideData>();
    assert!(!array.is_null(), "av_mallocz side-data array");
    let payload = av_mallocz(6).cast::<u8>();
    assert!(!payload.is_null(), "av_mallocz the short payload");
    core::ptr::copy_nonoverlapping([1u8, 0, 8, 6, 1, 0].as_ptr(), payload, 6);
    (*array).data = payload;
    (*array).size = 6;
    core::ptr::write_unaligned(
      core::ptr::addr_of_mut!((*array).type_).cast::<i32>(),
      ffmpeg_next::ffi::AVPacketSideDataType::AV_PKT_DATA_DOVI_CONF as i32,
    );
    (*par).coded_side_data = array;
    (*par).nb_coded_side_data = 1;
  }
  let ticket = CodecTicket::mirror(&truncated, 11, usize::MAX).expect("mirror");
  assert!(
    ticket.dolby_vision_config().is_none(),
    "a 6-byte payload has no compatibility_id field (offset 7)"
  );
  // The un-truncated sibling parses, so the refusal above is about the
  // length and not some other mistake in the fixture.
  let full = parameters_with_dovi_config([1, 0, 8, 6, 1, 0, 1, 1, 0]);
  let full_ticket = CodecTicket::mirror(&full, 12, usize::MAX).expect("mirror");
  assert!(full_ticket.dolby_vision_config().is_some());
}
