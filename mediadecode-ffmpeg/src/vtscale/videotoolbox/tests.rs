//! The session cache's decision, proved without a GPU.
//!
//! [`super::cache_plan`] is the whole of it: everything the stage does
//! with a `VTPixelTransferSession` hangs off which of the three answers
//! this function gives, and it is arithmetic over [`super::StageKey`]s
//! rather than over Core Foundation objects — so the cache's contract
//! ("one session per stream-and-requested-size, a failed build latched
//! rather than retried per frame") is testable on any machine that can
//! compile the crate.

use ffmpeg_next::ffi::{
  AVFrame, AVFrameSideDataType, av_frame_alloc, av_frame_free, av_frame_new_side_data,
};

use super::{
  CachePlan, ScaledOutput, StageKey, ThreadOwner, cache_plan, is_acceptable_request, is_stale,
  scaled_sample_aspect_ratio, side_data_forbids_scaling,
};

/// A 1080p NV12 stream fitted to 512x288 — the shape the real-media
/// lane exercises.
fn key() -> StageKey {
  StageKey {
    // 'x420' — 8-bit 4:2:0 video range, what VideoToolbox hands back
    // for an ordinary H.264 stream.
    source_cv_format: 0x78343230,
    source: (1920, 1080),
    fitted: (512, 288),
    // `AV_PIX_FMT_NV12` in this build's bindings; the value's identity
    // does not matter here, only that a change in it changes the key.
    sw_format: 23,
  }
}

#[test]
fn an_empty_cache_builds() {
  assert_eq!(cache_plan(None, key()), CachePlan::Build);
}

#[test]
fn the_same_key_reuses_the_standing_session() {
  assert_eq!(cache_plan(Some(key()), key()), CachePlan::Reuse);
}

#[test]
fn a_different_requested_size_retires_the_standing_session() {
  let mut want = key();
  want.fitted = (256, 144);
  assert_eq!(cache_plan(Some(key()), want), CachePlan::Build);
}

#[test]
fn a_mid_stream_change_in_the_stream_itself_retires_the_session() {
  for want in [
    StageKey {
      source: (1280, 720),
      ..key()
    },
    StageKey {
      source_cv_format: 0x78343230 + 1,
      ..key()
    },
    StageKey {
      sw_format: key().sw_format + 1,
      ..key()
    },
  ] {
    assert_eq!(
      cache_plan(Some(key()), want),
      CachePlan::Build,
      "a session built for {:?} must not be reused for {want:?}",
      key()
    );
  }
}

/// The other half of the cache's contract, and the one that makes
/// `VideoDecoder`'s `Send` claim hold: a session recognises the thread
/// it was created on, and does not recognise any other.
///
/// `VideoToolbox.framework` marks `VTPixelTransferSessionRef`
/// `CM_SWIFT_NONSENDABLE` and publishes no cross-thread mobility
/// guarantee, so the stage does not rely on one — it retires and
/// rebuilds when the decoder has moved. This is the predicate that
/// decision reads.
#[test]
fn a_thread_owner_recognises_only_its_own_thread() {
  let here = ThreadOwner::current();
  assert!(here.is_current(), "the creating thread owns it");

  let elsewhere = std::thread::spawn(move || {
    // The owner taken on the parent thread must not match this one...
    let seen_from_there = here.is_current();
    // ...and one taken here must match here.
    let theirs = ThreadOwner::current();
    (seen_from_there, theirs.is_current(), theirs)
  })
  .join()
  .expect("the probe thread finished");

  assert!(
    !elsewhere.0,
    "a session created on the parent thread is not owned by a child one"
  );
  assert!(elsewhere.1, "the child thread owns what it took there");
  assert!(
    !elsewhere.2.is_current(),
    "and back on the parent thread, the child's owner is foreign again"
  );
  assert!(here.is_current(), "the parent's own claim is unchanged");
}

/// **The recycling regression.** A `pthread_t` is reused after its
/// thread ends — a later thread can be handed the same handle, and
/// `pthread_equal` will call the two one and the same. That would let a
/// session built on a thread that has since exited be reused on a live
/// stranger, which is precisely the off-owner use the guard exists to
/// stop. [`ThreadOwner`] is a [`std::thread::ThreadId`], documented
/// never to be reused for the lifetime of the process, so this holds by
/// the type's own contract; the test pins it against a regression to
/// any handle that is merely *usually* unique.
#[test]
fn an_owner_whose_thread_has_exited_never_matches_a_later_one() {
  let first = std::thread::spawn(ThreadOwner::current)
    .join()
    .expect("the first probe thread finished");
  // The first thread is gone by the time the second one starts, so a
  // recycled OS handle is exactly what a second thread might be given.
  for _ in 0..64 {
    let matched = std::thread::spawn(move || first.is_current())
      .join()
      .expect("a later probe thread finished");
    assert!(
      !matched,
      "an owner taken on a thread that has since exited must never match a later thread"
    );
  }
}

/// An `AVFrame` with `count` side-data entries of `kind`, and a closure
/// run against it before it is freed.
///
/// The frames are real: `av_frame_new_side_data` allocates the entry
/// and the array exactly as libavcodec does, so the walk under test
/// reads the layout it will meet in production.
fn with_side_data<R>(
  entries: &[AVFrameSideDataType],
  probe: impl FnOnce(*const AVFrame) -> R,
) -> R {
  // SAFETY: a fresh frame, filled through FFmpeg's own side-data
  // allocator and freed through its own free.
  unsafe {
    let frame = av_frame_alloc();
    assert!(!frame.is_null(), "av_frame_alloc");
    for kind in entries {
      let entry = av_frame_new_side_data(frame, *kind, 16);
      assert!(!entry.is_null(), "av_frame_new_side_data");
    }
    let answer = probe(frame.cast_const());
    let mut frame = frame;
    av_frame_free(&mut frame);
    answer
  }
}

#[test]
fn a_frame_with_no_side_data_is_scalable() {
  // SAFETY: a live frame with an empty side-data array.
  assert!(!with_side_data(&[], |frame| unsafe {
    side_data_forbids_scaling(frame)
  }));
}

#[test]
fn side_data_a_resize_would_not_strand_is_scalable() {
  // Whitelisted and grid-independent: colour volume, light level, a
  // rotation matrix. None of these describe pixel coordinates.
  for kind in [
    AVFrameSideDataType::AV_FRAME_DATA_MASTERING_DISPLAY_METADATA,
    AVFrameSideDataType::AV_FRAME_DATA_CONTENT_LIGHT_LEVEL,
    AVFrameSideDataType::AV_FRAME_DATA_DISPLAYMATRIX,
    AVFrameSideDataType::AV_FRAME_DATA_A53_CC,
  ] {
    // SAFETY: a live frame carrying one real side-data entry.
    let forbids = with_side_data(&[kind], |frame| unsafe { side_data_forbids_scaling(frame) });
    assert!(!forbids, "{kind:?} does not describe the picture's grid");
  }
}

#[test]
fn side_data_expressed_in_the_pictures_grid_forbids_scaling() {
  // FFmpeg 9 marks seven kinds size-dependent; these are the three
  // this crate also copies, so these are the three a resize could
  // strand. See `side_data_forbids_scaling`.
  for kind in [
    AVFrameSideDataType::AV_FRAME_DATA_PANSCAN,
    AVFrameSideDataType::AV_FRAME_DATA_SPHERICAL,
    AVFrameSideDataType::AV_FRAME_DATA_REGIONS_OF_INTEREST,
  ] {
    // SAFETY: a live frame carrying one real side-data entry.
    let forbids = with_side_data(&[kind], |frame| unsafe { side_data_forbids_scaling(frame) });
    assert!(forbids, "{kind:?} is expressed in the source grid");
  }
  // And it is found behind a run of harmless entries, not only first.
  let mixed = [
    AVFrameSideDataType::AV_FRAME_DATA_MASTERING_DISPLAY_METADATA,
    AVFrameSideDataType::AV_FRAME_DATA_CONTENT_LIGHT_LEVEL,
    AVFrameSideDataType::AV_FRAME_DATA_REGIONS_OF_INTEREST,
  ];
  // SAFETY: as above.
  assert!(with_side_data(&mixed, |frame| unsafe {
    side_data_forbids_scaling(frame)
  }));
}

/// Bookkeeping this walk cannot trust is a refusal, not an absence.
///
/// **Every vector here is rejected before the array is indexed**, and
/// that is a property of the test as much as of the code: a forged
/// count that is positive and within the cap would send the walk past
/// the end of the real array, so probing one would not test the guard —
/// it would perform the very out-of-bounds read the guard cannot catch
/// and the walk's documentation says it trusts FFmpeg not to produce.
/// The vectors are therefore exactly the shapes a real array provably
/// cannot back.
#[test]
fn malformed_side_data_bookkeeping_forbids_scaling() {
  for hostile in [
    -1,
    i32::MIN,
    crate::decoder::HW_COPY_SIDE_DATA_MAX_ENTRIES as i32 + 1,
    i32::MAX,
  ] {
    // SAFETY: `nb_side_data` is overwritten with a hostile value for
    // the duration of the probe and restored to the frame's true count
    // before `av_frame_free` reads it — a frame freed under a forged
    // count would itself walk out of bounds.
    let forbids = with_side_data(
      &[AVFrameSideDataType::AV_FRAME_DATA_A53_CC],
      |frame| unsafe {
        let raw = frame.cast_mut();
        let real = (*raw).nb_side_data;
        (*raw).nb_side_data = hostile;
        let answer = side_data_forbids_scaling(frame);
        (*raw).nb_side_data = real;
        answer
      },
    );
    assert!(forbids, "nb_side_data = {hostile} is not walkable");
  }
}

#[test]
fn a_positive_count_with_no_array_forbids_scaling() {
  // SAFETY: the array pointer is nulled for the duration of the probe
  // and restored before `av_frame_free` frees through it.
  let forbids = with_side_data(
    &[AVFrameSideDataType::AV_FRAME_DATA_A53_CC],
    |frame| unsafe {
      let raw = frame.cast_mut();
      let real = (*raw).side_data;
      (*raw).side_data = core::ptr::null_mut();
      let answer = side_data_forbids_scaling(frame);
      (*raw).side_data = real;
      answer
    },
  );
  assert!(forbids, "a positive count with no array is malformed");
}

#[test]
fn a_null_entry_inside_the_array_forbids_scaling() {
  // SAFETY: one slot is nulled for the duration of the probe and
  // restored before `av_frame_free` frees through it.
  let forbids = with_side_data(
    &[
      AVFrameSideDataType::AV_FRAME_DATA_A53_CC,
      AVFrameSideDataType::AV_FRAME_DATA_CONTENT_LIGHT_LEVEL,
    ],
    |frame| unsafe {
      let raw = frame.cast_mut();
      let slot = (*raw).side_data.add(1);
      let real = *slot;
      *slot = core::ptr::null_mut();
      let answer = side_data_forbids_scaling(frame);
      *slot = real;
      answer
    },
  );
  assert!(
    forbids,
    "a null entry inside a non-empty array is malformed"
  );
}

#[test]
fn a_downscale_is_acceptable_and_an_upscale_is_not() {
  assert!(is_acceptable_request((512, 288), (1920, 1080)));
  assert!(is_acceptable_request((1, 1), (1920, 1080)));
  // Equal is not an upscale.
  assert!(is_acceptable_request((1920, 1080), (1920, 1080)));
  // One dimension over is over.
  assert!(!is_acceptable_request((1921, 1080), (1920, 1080)));
  assert!(!is_acceptable_request((1920, 1081), (1920, 1080)));
  assert!(!is_acceptable_request((3840, 2160), (1920, 1080)));
}

#[test]
fn a_zero_extent_is_never_acceptable() {
  assert!(!is_acceptable_request((0, 288), (1920, 1080)));
  assert!(!is_acceptable_request((512, 0), (1920, 1080)));
  assert!(!is_acceptable_request((0, 0), (1920, 1080)));
  // Including against a source that is itself degenerate — nothing
  // divides by a zero extent here.
  assert!(!is_acceptable_request((0, 0), (0, 0)));
}

#[test]
fn a_uniform_scale_leaves_the_sample_aspect_ratio_alone() {
  // The ordinary case: a pixel-budget fit preserves the shape, so the
  // storage grid's non-squareness is unchanged.
  assert_eq!(
    scaled_sample_aspect_ratio((1, 1), (1920, 1080), (960, 540)),
    Some((1, 1))
  );
  assert_eq!(
    scaled_sample_aspect_ratio((40, 33), (720, 480), (360, 240)),
    Some((40, 33))
  );
}

#[test]
fn the_answer_is_reduced_even_when_the_source_ratio_was_not() {
  // The correction is a rational multiply followed by a reduction, so
  // an unreduced source ratio comes back reduced — the same ratio, in
  // the form `av_reduce` would have produced. FFmpeg's own decoders
  // emit reduced ratios, so this is a property rather than a path.
  assert_eq!(
    scaled_sample_aspect_ratio((2, 2), (1920, 1080), (960, 540)),
    Some((1, 1))
  );
  assert_eq!(
    scaled_sample_aspect_ratio((80, 66), (720, 480), (360, 240)),
    Some((40, 33))
  );
}

#[test]
fn a_non_uniform_scale_corrects_the_sample_aspect_ratio() {
  // Half the width, full height: each stored pixel now covers twice
  // the horizontal extent it did, so the ratio doubles and the
  // *display* aspect ratio the source declared survives.
  assert_eq!(
    scaled_sample_aspect_ratio((1, 1), (1920, 1080), (480, 540)),
    Some((2, 1))
  );
  // The mirror case, and the reduction with it: 1:2, not 1080:2160.
  assert_eq!(
    scaled_sample_aspect_ratio((1, 1), (1920, 1080), (1920, 270)),
    Some((1, 4))
  );
}

#[test]
fn an_unspecified_sample_aspect_ratio_stays_unspecified() {
  // FFmpeg spells "unknown" `0/1`, and a scale must not invent a ratio
  // the stream never declared.
  assert_eq!(
    scaled_sample_aspect_ratio((0, 1), (1920, 1080), (480, 540)),
    Some((0, 1))
  );
  assert_eq!(
    scaled_sample_aspect_ratio((1, 0), (1920, 1080), (480, 540)),
    Some((1, 0))
  );
  assert_eq!(
    scaled_sample_aspect_ratio((-1, 1), (1920, 1080), (480, 540)),
    Some((-1, 1))
  );
}

#[test]
fn a_degenerate_extent_leaves_the_sample_aspect_ratio_alone() {
  assert_eq!(
    scaled_sample_aspect_ratio((1, 1), (0, 1080), (480, 540)),
    Some((1, 1))
  );
  assert_eq!(
    scaled_sample_aspect_ratio((1, 1), (1920, 0), (480, 540)),
    Some((1, 1))
  );
  assert_eq!(
    scaled_sample_aspect_ratio((1, 1), (1920, 1080), (0, 540)),
    Some((1, 1))
  );
  assert_eq!(
    scaled_sample_aspect_ratio((1, 1), (1920, 1080), (480, 0)),
    Some((1, 1))
  );
}

#[test]
fn a_ratio_that_will_not_fit_the_pair_refuses_rather_than_lying() {
  // Coprime terms at the top of the range: the corrected ratio cannot
  // be expressed in the `c_int` pair `AVRational` holds. Answering the
  // *source's* ratio would be a lie about a grid that moved — the
  // display aspect this vector needs is about 2.008 and the source pair
  // reads about 1.0 — so the answer is `None` and the stage stands down
  // and delivers the unscaled frame instead.
  let huge = (i32::MAX, i32::MAX - 1);
  assert_eq!(
    scaled_sample_aspect_ratio(huge, (1920, 1080), (479, 541)),
    None
  );
}

#[test]
fn the_widest_inputs_the_types_admit_neither_overflow_nor_panic() {
  // Not a shape any decoder produces — `FrameLimits` refuses extents
  // orders of magnitude smaller — but this is a pure function over a
  // caller-supplied request, so it may not have a size at which it
  // stops being right. Under `overflow-checks` (this crate's test
  // profile) an `i64` intermediate would panic here rather than
  // answering.
  let widest = (i32::MAX, i32::MAX);
  assert_eq!(
    scaled_sample_aspect_ratio(widest, (u32::MAX, u32::MAX), (u32::MAX, u32::MAX)),
    Some((1, 1)),
    "a no-op scale answers the same ratio in reduced form"
  );
  // And the same extents with the terms pulled apart, so the reduction
  // cannot collapse to the identity.
  assert_eq!(
    scaled_sample_aspect_ratio(widest, (u32::MAX, 1), (1, u32::MAX)),
    None,
    "a result too wide for the `c_int` pair refuses rather than lying"
  );
}

/// Standing down leaves the request standing.
///
/// The stage stands itself down for reasons of its own — a frame it cannot
/// scale — and the frame goes out full size. With nothing built there is
/// nothing to retire, and the standing request survives either way, so a
/// caller who asks for a different size gets a fresh attempt rather than a
/// session quietly dead for good.
#[test]
fn standing_down_leaves_the_request_standing() {
  let mut stage = ScaledOutput::new();
  // Nothing built yet: standing down is a no-op rather than a panic or a
  // state that refuses everything afterwards.
  stage.stand_down();
  assert_eq!(stage.requested(), None);

  assert!(stage.request((512, 288), (1920, 1080)).is_supported());
  stage.stand_down();
  assert_eq!(
    stage.requested(),
    Some((512, 288)),
    "the caller's request outlives a stand-down"
  );
  // And a later, different request is still accepted.
  assert!(stage.request((256, 144), (1920, 1080)).is_supported());
  assert_eq!(stage.requested(), Some((256, 144)));
}

/// The promise the capability word makes, and the moment it stops
/// making it.
///
/// The trait's contract is that `Supported` lets a caller skip its own
/// resampler. This stage can stand down per frame, so the first frame
/// that does not come back at the requested extent has to be observable
/// — otherwise a caller who acted on the answer collects mixed extents
/// with nothing to notice them by.
#[test]
fn a_broken_promise_is_observable_and_a_new_request_renews_it() {
  let mut stage = ScaledOutput::new();
  assert!(
    stage.promise_stands(),
    "nothing has been promised, so nothing is broken"
  );
  assert!(stage.request((512, 288), (1920, 1080)).is_supported());
  assert!(stage.promise_stands());

  // The stage stands down for a frame, and that is a frame the caller
  // receives full size.
  stage.stand_down();
  assert!(
    !stage.promise_stands(),
    "a frame went out unfitted, so the capability word must stop promising"
  );
  assert_eq!(
    stage.requested(),
    Some((512, 288)),
    "the request itself outlives the broken promise"
  );

  // Asking again is how a caller recovers, so asking again must work.
  assert!(stage.request((256, 144), (1920, 1080)).is_supported());
  assert!(
    stage.promise_stands(),
    "a fresh request buys a fresh promise"
  );
}

/// A refusal renews no promise, and returns the session to full size.
#[test]
fn a_refused_request_renews_nothing_and_returns_to_full_size() {
  let mut stage = ScaledOutput::new();
  assert!(stage.request((512, 288), (1920, 1080)).is_supported());
  stage.stand_down();
  assert!(!stage.promise_stands());
  // An upscale is refused, so it cannot quietly renew what it never
  // accepted.
  assert!(!stage.request((3840, 2160), (1920, 1080)).is_supported());
  assert!(
    !stage.promise_stands(),
    "a refused request must not renew a broken promise"
  );
  // And it clears the standing request: `Unsupported` from this seat
  // means the session is back to full coded size, which is the only
  // reading a caller can act on without risking a second resample of
  // an already-fitted picture.
  assert_eq!(stage.requested(), None);
}

/// A cache built for one stream shape does not outlive it.
///
/// The finding this pins: a mid-stream drop to a smaller source turns
/// the standing request into an upscale, and the stand-down for that
/// used to return before the cache was ever compared — leaving a 4K
/// surface, its frames context and a session retained for the whole
/// lower-resolution run. `is_stale` is the comparison that decision
/// reads.
#[test]
fn a_cache_built_for_another_stream_shape_is_stale() {
  let built = StageKey {
    source_cv_format: 0x78343230,
    source: (3840, 2160),
    fitted: (512, 288),
    sw_format: 23,
  };
  // Nothing cached is never stale.
  assert!(!is_stale(
    None,
    built.source_cv_format,
    built.source,
    built.sw_format
  ));
  // The same stream: a comparison, not a rebuild.
  assert!(!is_stale(
    Some(built),
    built.source_cv_format,
    built.source,
    built.sw_format
  ));
  // A different extent, format or layout: each on its own retires it.
  for (cv, extent, sw) in [
    (built.source_cv_format, (1280, 720), built.sw_format),
    (built.source_cv_format + 1, built.source, built.sw_format),
    (built.source_cv_format, built.source, built.sw_format + 1),
  ] {
    assert!(
      is_stale(Some(built), cv, extent, sw),
      "state built for {built:?} must not survive a source of {cv:?} {extent:?} {sw:?}"
    );
  }
  // And the requested size is deliberately NOT part of this comparison:
  // that is `cache_plan`'s question, and a same-stream size change must
  // still reach it as a rebuild rather than a retirement.
  let mut resized = built;
  resized.fitted = (256, 144);
  assert!(!is_stale(
    Some(resized),
    built.source_cv_format,
    built.source,
    built.sw_format
  ));
  assert_eq!(cache_plan(Some(resized), built), CachePlan::Build);
}

/// The promise is **one-way**, and that is the whole point of it.
///
/// A caller told `Unsupported` resumes resampling for itself. If the
/// stage then quietly fitted a later frame, that caller would resample
/// an already-fitted picture — or collect mixed extents from a session
/// that had said it was done. So a broken promise stops the staging as
/// well as the answer, and only an accepted request restarts either.
#[test]
fn a_broken_promise_stops_the_staging_too() {
  let mut stage = ScaledOutput::new();
  assert!(stage.request((512, 288), (1920, 1080)).is_supported());
  assert!(stage.promise_stands());
  assert!(stage.staging_armed());

  // One frame the stage could not honor.
  stage.stand_down();
  assert!(!stage.promise_stands(), "the caller is told");
  assert!(
    !stage.staging_armed(),
    "and the stage stays off, so a later scalable frame is not fitted behind that answer"
  );
  assert_eq!(
    stage.requested(),
    Some((512, 288)),
    "the request itself is remembered, so an explicit renewal knows what to renew"
  );

  // Only an accepted request restarts it — and both halves come back
  // together, so the answer and the behaviour can never disagree.
  assert!(stage.request((512, 288), (1920, 1080)).is_supported());
  assert!(stage.promise_stands());
  assert!(stage.staging_armed());
}

/// A refused request cannot restart a stage that stood down, because it
/// was never accepted.
#[test]
fn a_refused_request_does_not_rearm_the_stage() {
  let mut stage = ScaledOutput::new();
  assert!(stage.request((512, 288), (1920, 1080)).is_supported());
  stage.stand_down();
  assert!(!stage.staging_armed());
  assert!(!stage.request((3840, 2160), (1920, 1080)).is_supported());
  assert!(
    !stage.staging_armed(),
    "an upscale is refused, so it renews nothing"
  );
  assert!(!stage.promise_stands());
}

/// Cancellation is a refusal that did not go through `request`.
///
/// The wrapper refuses a request placed while a decoded picture is
/// parked, and a refusal has to mean the same thing there as everywhere
/// else — the session returns to full coded size. This is the operation
/// that road calls, and it must clear exactly what an ordinary refusal
/// clears.
#[test]
fn cancelling_clears_the_request_the_way_a_refusal_does() {
  let mut cancelled = ScaledOutput::new();
  assert!(cancelled.request((512, 288), (1920, 1080)).is_supported());
  cancelled.cancel();

  let mut refused = ScaledOutput::new();
  assert!(refused.request((512, 288), (1920, 1080)).is_supported());
  assert!(!refused.request((3840, 2160), (1920, 1080)).is_supported());

  assert_eq!(cancelled.requested(), None);
  assert_eq!(
    cancelled.requested(),
    refused.requested(),
    "the two refusal roads must leave the session in the same state"
  );
  // Cancelling is not a broken promise — nothing was delivered
  // unfitted, the caller simply has no request standing.
  assert!(cancelled.promise_stands());
  assert!(cancelled.staging_armed());
  // And asking again works, from either road.
  assert!(cancelled.request((256, 144), (1920, 1080)).is_supported());
  assert_eq!(cancelled.requested(), Some((256, 144)));
}

/// Cancelling with nothing standing is a no-op, not a state change.
#[test]
fn cancelling_an_idle_stage_changes_nothing() {
  let mut stage = ScaledOutput::new();
  stage.cancel();
  assert_eq!(stage.requested(), None);
  assert!(stage.promise_stands());
  assert!(stage.staging_armed());
}
