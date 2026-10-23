//! Harness for faults that cannot be injected in this process.
//!
//! Two kinds of test need their own process. One is a fault switch that
//! is **global**: `av_max_alloc` makes every FFmpeg allocation past a
//! size fail, which is the only honest way to test an out-of-memory
//! path against the real library — and it would break every other lane
//! running beside it. The other is a failure whose whole point is that
//! it *used* to end the process: a test that asserts the process
//! survives has to be able to observe a process that did not.
//!
//! [`in_subprocess`] re-runs one named test in a child of the test
//! binary itself, single-threaded and alone, and the parent asserts the
//! child exited normally. A child that aborts is reported as the
//! signal it died of rather than as a passing test.

/// Runs `body` in a child process running only this test, and asserts
/// the child exited cleanly.
///
/// `test_path` must be the test's name as libtest spells it — the
/// module path inside the crate, e.g. `demuxer::tests::the_lane` — so
/// that `--exact` selects exactly this test in the child. The child runs
/// it whether or not it is `#[ignore]`d (`--include-ignored`): an ignored
/// lane — one that needs a sample or a hardware backend — reaches this only
/// in a run that asked for ignored lanes, and its child runs what its
/// parent ran.
pub fn in_subprocess(test_path: &str, body: impl FnOnce()) {
  const CHILD: &str = "MEDIAIO_FFMPEG_FAULT_CHILD";

  if std::env::var(CHILD).as_deref() == Ok(test_path) {
    body();
    return;
  }

  let exe = std::env::current_exe().expect("the test binary's own path");
  let output = std::process::Command::new(exe)
    .args([
      "--exact",
      test_path,
      "--include-ignored",
      "--nocapture",
      "--test-threads=1",
    ])
    .env(CHILD, test_path)
    .output()
    .expect("spawning the child test process");

  let stdout = String::from_utf8_lossy(&output.stdout);
  let stderr = String::from_utf8_lossy(&output.stderr);
  assert_eq!(
    output.status.code(),
    Some(0),
    "the child running `{test_path}` did not exit cleanly ({:?})\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}",
    output.status,
  );
  // A child that matched no test also exits zero. Insisting that it ran
  // exactly one is what keeps a renamed test from turning this lane
  // into a lane that asserts nothing.
  assert!(
    stdout.contains("1 passed"),
    "the child ran no test — is `{test_path}` still the name?\n--- stdout ---\n{stdout}",
  );
}

/// Makes every FFmpeg allocation larger than `max` fail, returning
/// `NULL` the way an out-of-memory allocator would.
///
/// # Safety
///
/// The cap is process-global: from this call until
/// [`uncap_ffmpeg_allocations`], every FFmpeg allocation larger than
/// `max` returns null, on every thread. ffmpeg-next's own safe
/// constructors do not check for that — `frame::Video::empty` and
/// `codec::Parameters::new` wrap the null they are handed, and the next
/// call through one dereferences it — so the caller must ensure that no
/// allocation made while the cap stands hands a null to code that will
/// dereference it. In practice: only inside [`in_subprocess`]'s child,
/// which runs one test alone, and only around code that checks what it
/// allocates or never uses what it did not get.
pub unsafe fn cap_ffmpeg_allocations(max: usize) {
  // SAFETY: `av_max_alloc` stores an atomic and returns nothing; what the
  // cap does to other allocations is this function's own contract.
  unsafe { ffmpeg_next::ffi::av_max_alloc(max) };
}

/// Lifts the cap [`cap_ffmpeg_allocations`] set.
pub fn uncap_ffmpeg_allocations() {
  // SAFETY: `av_max_alloc` stores an atomic and returns nothing, and
  // `INT_MAX` is libavutil's own default cap ("By default, the max value
  // is defined as `INT_MAX`", `libavutil/mem.h` 593 in FFmpeg 9.0.1).
  // Raising the cap cannot make an allocation fail, so nothing
  // [`cap_ffmpeg_allocations`] guards against can follow from it.
  unsafe { ffmpeg_next::ffi::av_max_alloc(i32::MAX as usize) };
}
