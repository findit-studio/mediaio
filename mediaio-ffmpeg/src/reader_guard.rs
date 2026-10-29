//! The panic guard on the custom-reader boundary.
//!
//! `mediadecode-ffmpeg`'s `FfmpegDemuxer::open_reader` accepts any safe
//! `Read + Seek`, and libavformat drives it through
//! `AVIOContext` callbacks that are `extern "C"` functions:
//! `ffmpeg_next::format::context::StreamIo`'s `read` and `seek` call
//! the stream directly, with no `catch_unwind` anywhere between the
//! caller's code and C. A panic crossing an `extern "C"` frame is not
//! an unwind that some caller can catch — it aborts the process. One
//! misbehaving reader would therefore take the whole service down.
//!
//! [`GuardedReader`] closes that on this side of the boundary: every
//! call into the caller's reader runs under
//! [`catch_unwind`](std::panic::catch_unwind), a panic is latched in
//! shared state and reported to C as an ordinary I/O error, and the
//! demuxer turns the latch into
//! its `DemuxError::ReaderPanic` once
//! control comes back to Rust. The panic never reaches the `extern "C"`
//! frame at all.
//!
//! The guard cannot help under `panic = "abort"`, where no unwind
//! exists to catch; that is a whole-binary choice the caller makes.

use std::{
  any::Any,
  io::{self, Read, Seek, SeekFrom},
  panic::{AssertUnwindSafe, catch_unwind},
  sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
  },
};

use smol_bytes::Utf8Bytes;

/// Where a panic raised inside a caller's reader is recorded until the
/// demuxer can name it.
///
/// The flag is what the hot path reads — one relaxed-ordering load per
/// pull — and the message is behind the lock only a latched panic ever
/// takes.
#[derive(Debug, Default)]
pub struct PanicLatch {
  latched: AtomicBool,
  message: Mutex<Option<Utf8Bytes>>,
}

impl PanicLatch {
  /// Records a panic payload. The first one wins: a reader that panics
  /// again after libavformat retried is describing the same fault, and
  /// the first description is the one nearest the cause.
  ///
  /// Shared with the writer guard ([`crate::writer_guard`]), which
  /// latches a sink's panic the same way.
  pub(crate) fn latch(&self, payload: &(dyn Any + Send)) {
    let message = describe(payload);
    // A poisoned lock means a previous holder panicked while
    // describing a panic. The stored value is still a plain
    // `Option<Utf8Bytes>` and is sound to use, and refusing to record
    // here would lose the very error this type exists to carry.
    let mut slot = self.message.lock().unwrap_or_else(|e| e.into_inner());
    if slot.is_none() {
      *slot = Some(message);
    }
    drop(slot);
    self.latched.store(true, Ordering::Release);
  }

  /// Whether a panic has been latched: an `Acquire` load, pairing with
  /// the `Release` store in [`PanicLatch::latch`].
  pub(crate) fn is_latched(&self) -> bool {
    self.latched.load(Ordering::Acquire)
  }

  /// The latched panic's message, or `None` when no reader has
  /// panicked. Reading it does not clear it: a poisoned `AVIOContext`
  /// stays poisoned, so every later call must report the same cause.
  pub fn message(&self) -> Option<Utf8Bytes> {
    if !self.latched.load(Ordering::Acquire) {
      return None;
    }
    self
      .message
      .lock()
      .unwrap_or_else(|e| e.into_inner())
      .clone()
      .or_else(|| Some(Utf8Bytes::from_static(UNNAMED)))
  }
}

/// What a panic payload says, for the payload shapes `panic!` produces.
fn describe(payload: &(dyn Any + Send)) -> Utf8Bytes {
  if let Some(s) = payload.downcast_ref::<&'static str>() {
    // A `panic!("literal")` payload is already `'static`; storing the
    // borrow beats copying it, and a panic message is the last place
    // that should need the allocator.
    return Utf8Bytes::from_static(s);
  }
  if let Some(s) = payload.downcast_ref::<String>() {
    return Utf8Bytes::from(s.as_str());
  }
  Utf8Bytes::from_static(UNNAMED)
}

/// Stand-in for a payload that is neither `&str` nor `String` — a
/// caller may panic with any `Any + Send`.
const UNNAMED: &str = "panicked with a payload of an unknown type";

/// An I/O error a caller's stream returned, reduced to what it says — its
/// OS code, or else its kind — and holding nothing the caller owned.
///
/// **An error's payload is the caller's code too.** `io::Error::new`
/// boxes any `Error + Send + Sync`, and safe code can give that box a
/// `Drop` that panics. ffmpeg-next's AVIO callbacks map the error a
/// stream returns to an `AVERROR` and drop it inside the `extern "C"`
/// frame (`map_io_error`, `format/context/stream_io.rs` 420–458 in
/// ffmpeg-next 9.0.0), so a payload that panics there aborts the
/// process — past a guard that caught every panic the stream raised while
/// it ran. Both guards therefore reduce each error **inside**
/// `catch_unwind`, where its payload is dropped: a destructor that panics
/// is caught there and latched like any other panic, and the error that
/// leaves the guard is rebuilt from the reduction ([`Self::rebuild`]),
/// owning nothing.
///
/// What C is told is unchanged: ffmpeg-next reads an error's OS code
/// where it has one and its kind otherwise, and the rebuilt error has
/// exactly that code, or that kind. Only the payload's message is gone,
/// and no `AVERROR` carried it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InertError {
  /// An OS error, by its raw code.
  Os(i32),
  /// Any other error, by its kind.
  Kind(io::ErrorKind),
}

impl InertError {
  /// What `error` says. `error` itself — its payload with it — is dropped
  /// here, so a guard that calls this inside `catch_unwind` catches a
  /// destructor's panic too.
  pub(crate) fn reduce(error: io::Error) -> Self {
    let inert = match error.raw_os_error() {
      Some(code) => Self::Os(code),
      None => Self::Kind(error.kind()),
    };
    drop(error);
    inert
  }

  /// What `error` says, read from it in place: for an error that owns no
  /// payload of the caller's — one this guard rebuilt — where reading
  /// runs no caller's code.
  pub(crate) fn of(error: &io::Error) -> Self {
    match error.raw_os_error() {
      Some(code) => Self::Os(code),
      None => Self::Kind(error.kind()),
    }
  }

  /// An error that says the same and owns nothing: the OS code, or the
  /// bare kind.
  pub(crate) fn rebuild(self) -> io::Error {
    match self {
      Self::Os(code) => io::Error::from_raw_os_error(code),
      Self::Kind(kind) => io::Error::from(kind),
    }
  }
}

/// A caller's byte source, wrapped so that a panic inside it becomes an
/// I/O error instead of an abort.
///
/// Deliberately does **not** override `Seek::stream_position` or
/// `Read::read_exact`: their default implementations are written in
/// terms of [`Seek::seek`] / [`Read::read`], which are guarded, so
/// forwarding them would only add unguarded paths.
pub struct GuardedReader<R> {
  inner: R,
  latch: Arc<PanicLatch>,
  /// Bytes handed to libavformat so far, and the ceiling on them.
  ///
  /// **A parser cannot allocate from bytes it was never given.** Every
  /// other budget in this crate measures a copy this crate makes, which
  /// on the demux road is always *after* `avformat_open_input` and
  /// `avformat_find_stream_info` have already built the attached
  /// picture, the extradata and the coded side data out of the file.
  /// This meter is the one instrument that reaches behind that: past
  /// the budget the reader stops answering, so the parser gets nothing
  /// more however much it asks for.
  ///
  /// Shared with the demuxer so the refusal can be named after
  /// libavformat has folded the I/O error into its own.
  meter: Arc<ReadMeter>,
}

/// The byte meter behind [`GuardedReader`], readable after the fact.
#[derive(Debug, Default)]
pub struct ReadMeter {
  read: AtomicU64,
  budget: AtomicU64,
  tripped: AtomicBool,
}

impl ReadMeter {
  /// A meter with `budget` bytes to spend.
  pub fn new(budget: u64) -> Self {
    Self {
      read: AtomicU64::new(0),
      budget: AtomicU64::new(budget),
      tripped: AtomicBool::new(false),
    }
  }
  /// Whether the budget was reached.
  pub fn tripped(&self) -> bool {
    self.tripped.load(Ordering::Acquire)
  }
  /// Bytes handed over.
  pub fn read(&self) -> u64 {
    self.read.load(Ordering::Relaxed)
  }
  /// The ceiling in force.
  pub fn budget(&self) -> u64 {
    self.budget.load(Ordering::Relaxed)
  }
  /// Lifts the ceiling once the container is open and analysed.
  ///
  /// The slot bounds *probing*: reading the media itself afterwards is
  /// the caller's own business, packet by packet, and already bounded
  /// by the packet slots.
  pub fn release(&self) {
    self.budget.store(u64::MAX, Ordering::Relaxed);
  }
  /// Bytes still available to hand over.
  fn remaining(&self) -> u64 {
    self
      .budget
      .load(Ordering::Relaxed)
      .saturating_sub(self.read.load(Ordering::Relaxed))
  }

  /// Records `bytes` as delivered.
  ///
  /// Infallible by construction: the reader caps every request to
  /// [`Self::remaining`], so a charge can never exceed the budget.
  fn charge(&self, bytes: u64) {
    self.read.fetch_add(bytes, Ordering::Relaxed);
  }

  /// Marks the budget as reached, for the read that had to be refused.
  fn trip(&self) {
    self.tripped.store(true, Ordering::Release);
  }
}

impl<R> GuardedReader<R> {
  /// Wraps `inner`, returning the guard and a handle on the latch the
  /// demuxer keeps to name a panic after the fact.
  pub fn new(inner: R, budget: u64) -> (Self, Arc<PanicLatch>, Arc<ReadMeter>) {
    let latch = Arc::new(PanicLatch::default());
    let meter = Arc::new(ReadMeter::new(budget));
    (
      Self {
        inner,
        latch: Arc::clone(&latch),
        meter: Arc::clone(&meter),
      },
      latch,
      meter,
    )
  }

  /// Runs one call into the caller's reader under `catch_unwind`, an
  /// error it returns reduced there ([`InertError`]).
  fn guard<T>(&mut self, call: impl FnOnce(&mut R) -> io::Result<T>) -> io::Result<T> {
    let Self {
      inner,
      latch,
      meter: _,
    } = self;
    // `AssertUnwindSafe` because the reader is exactly what may be left
    // inconsistent by its own panic — and that is fine here: the latch
    // makes the session terminal, so nothing reads through this reader
    // again except libavformat, whose own error state is already
    // poisoned by the error returned below.
    //
    // An error the reader returns is reduced inside the catch, so its
    // payload is dropped here and not in ffmpeg-next's `extern "C"`
    // callback: a payload whose `Drop` panics takes the path below.
    match catch_unwind(AssertUnwindSafe(|| call(inner).map_err(InertError::reduce))) {
      Ok(result) => result.map_err(InertError::rebuild),
      Err(payload) => {
        latch.latch(&*payload);
        // The payload is described and then deliberately forgotten.
        // `panic_any` takes any `Send` value, and safe code can give it
        // a `Drop` that panics; dropping it here — after the unwind has
        // been caught, outside any `catch_unwind` — would send that
        // second panic straight out of `read`/`seek` and into
        // ffmpeg-next's `extern "C"` AVIO callback. That is the abort
        // this guard exists to prevent, reached through the guard
        // itself. Leaking one value on a path that has already made the
        // session terminal is the cheaper half of that trade by a
        // distance, and a nested `catch_unwind` would only move the
        // question to the payload's payload.
        core::mem::forget(payload);
        Err(io::Error::other("the caller's reader panicked"))
      }
    }
  }
}

impl<R: Read> Read for GuardedReader<R> {
  fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
    // **The request is capped, not the answer refused.** Reading the
    // caller's full buffer and *then* discovering the budget was
    // exceeded consumed bytes from the reader that libavformat never
    // received: a 32 KiB request against a 1 KiB allowance took all
    // 32 KiB, returned none of them, and reported having read bytes
    // nobody was handed. A container that would have finished probing
    // inside its allowance was refused for the shape of libavformat's
    // buffer rather than for its own size.
    //
    // A short read is ordinary — every `Read` implementation is
    // entitled to return fewer bytes than asked — so capping the slice
    // is the honest instrument: libavformat gets exactly what the
    // budget affords, and the meter counts exactly what it got.
    //
    // # The boundary, stated
    //
    // * a read that lands **exactly** on zero remaining is served in
    //   full and does not trip: the container may well have finished,
    //   and a budget is a ceiling on what is spent, not on asking;
    // * the **next** nonempty read after that is refused, and that is
    //   the one that trips the meter.
    //
    // A zero-length request asks for nothing, so it is passed through
    // whatever the budget says — refusing it would turn an empty probe
    // into an error.
    if buf.is_empty() {
      return self.guard(|inner| inner.read(buf));
    }
    let remaining = self.meter.remaining();
    if remaining == 0 {
      self.meter.trip();
      return Err(io::Error::other("mediadecode: probe read budget exhausted"));
    }
    let capped = usize::try_from(remaining)
      .unwrap_or(usize::MAX)
      .min(buf.len());
    let read = self.guard(|inner| inner.read(&mut buf[..capped]))?;
    // Only what was actually handed over, so `ProbeBudgetExhausted`
    // reports bytes libavformat really received.
    self.meter.charge(read as u64);
    Ok(read)
  }
}

impl<R: Seek> Seek for GuardedReader<R> {
  fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
    self.guard(|inner| inner.seek(pos))
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  struct Panicking;

  impl Read for Panicking {
    fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
      panic!("read exploded");
    }
  }

  impl Seek for Panicking {
    fn seek(&mut self, _pos: SeekFrom) -> io::Result<u64> {
      panic!("seek exploded");
    }
  }

  /// Silences the default hook for one call, so a deliberate panic does
  /// not print a backtrace into the test log. The hook is global; the
  /// swap is confined to this module's single-threaded lane.
  fn quietly<T>(call: impl FnOnce() -> T) -> T {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let out = call();
    std::panic::set_hook(previous);
    out
  }

  #[test]
  fn a_panicking_read_becomes_an_error_and_latches_its_message() {
    let (mut guarded, latch, _meter) = GuardedReader::new(Panicking, u64::MAX);
    assert!(latch.message().is_none(), "nothing has panicked yet");

    let mut buf = [0u8; 8];
    let err = quietly(|| guarded.read(&mut buf)).expect_err("a panic is an error, not an abort");
    assert_eq!(err.kind(), io::ErrorKind::Other);
    assert_eq!(latch.message().as_deref(), Some("read exploded"));
  }

  #[test]
  fn a_panicking_seek_becomes_an_error_and_latches_its_message() {
    let (mut guarded, latch, _meter) = GuardedReader::new(Panicking, u64::MAX);
    let err = quietly(|| guarded.seek(SeekFrom::Start(4))).expect_err("a panic is an error");
    assert_eq!(err.kind(), io::ErrorKind::Other);
    assert_eq!(latch.message().as_deref(), Some("seek exploded"));

    // `stream_position` is the default implementation over `seek`, so
    // it is guarded by the same call and must not reach C either.
    assert!(quietly(|| guarded.stream_position()).is_err());
  }

  #[test]
  fn the_first_panic_is_the_one_reported() {
    struct TwoFaced(u32);
    impl Read for TwoFaced {
      fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
        self.0 += 1;
        panic!("panic number {}", self.0);
      }
    }

    let (mut guarded, latch, _meter) = GuardedReader::new(TwoFaced(0), u64::MAX);
    let mut buf = [0u8; 4];
    quietly(|| {
      let _ = guarded.read(&mut buf);
      let _ = guarded.read(&mut buf);
    });
    assert_eq!(
      latch.message().as_deref(),
      Some("panic number 1"),
      "the description nearest the cause is the one kept",
    );
  }

  /// A panic payload whose destructor panics in turn — safe code, and
  /// the shape that turned this guard into the abort it prevents.
  struct PanicOnDrop;

  impl Drop for PanicOnDrop {
    fn drop(&mut self) {
      panic!("and the payload went too");
    }
  }

  struct PanicsWithAPayload;

  impl Read for PanicsWithAPayload {
    fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
      std::panic::panic_any(PanicOnDrop);
    }
  }

  impl Seek for PanicsWithAPayload {
    fn seek(&mut self, _pos: SeekFrom) -> io::Result<u64> {
      std::panic::panic_any(PanicOnDrop);
    }
  }

  #[test]
  fn a_payload_that_panics_on_drop_does_not_get_dropped() {
    // The guard used to borrow the payload for its message and then let
    // it fall out of scope — outside `catch_unwind`, so the
    // destructor's panic unwound out of `read` and into C. This test
    // completing at all is the assertion; the process reaching the end
    // of it is what used to be impossible.
    let (mut guarded, latch, _meter) = GuardedReader::new(PanicsWithAPayload, u64::MAX);
    let mut buf = [0u8; 8];
    assert!(quietly(|| guarded.read(&mut buf)).is_err());
    assert_eq!(latch.message().as_deref(), Some(UNNAMED));
    assert!(quietly(|| guarded.seek(SeekFrom::Start(0))).is_err());
  }

  /// An error's payload whose destructor panics — safe code, returned
  /// rather than raised.
  #[derive(Debug)]
  struct HostilePayload;

  impl core::fmt::Display for HostilePayload {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
      f.write_str("an error whose payload panics when dropped")
    }
  }

  impl std::error::Error for HostilePayload {}

  impl Drop for HostilePayload {
    fn drop(&mut self) {
      panic!("the error's payload exploded");
    }
  }

  /// A reader whose every call returns an error carrying that payload.
  struct ReturnsAHostileError;

  impl Read for ReturnsAHostileError {
    fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
      Err(io::Error::new(io::ErrorKind::InvalidData, HostilePayload))
    }
  }

  impl Seek for ReturnsAHostileError {
    fn seek(&mut self, _pos: SeekFrom) -> io::Result<u64> {
      Err(io::Error::new(io::ErrorKind::InvalidData, HostilePayload))
    }
  }

  /// **An error whose payload panics on drop is reduced inside the
  /// guard: the destructor's panic is latched and named.** ffmpeg-next
  /// drops a returned error inside its `extern "C"` callback, where that
  /// panic would abort the process; here it is caught where the error is
  /// reduced, and what leaves the guard is the guard's own error.
  #[test]
  fn an_error_whose_payload_panics_on_drop_is_latched_and_named() {
    // Each error is forgotten, not dropped: were the guard to hand the
    // payload out, dropping it while this law fails would panic during
    // the unwind and abort the test binary instead of naming the defect.
    let (mut guarded, latch, _meter) = GuardedReader::new(ReturnsAHostileError, u64::MAX);
    let mut buf = [0u8; 8];
    let err = quietly(|| guarded.read(&mut buf)).expect_err("the read failed");
    let said = err.to_string();
    core::mem::forget(err);
    assert_eq!(said, "the caller's reader panicked");
    assert_eq!(
      latch.message().as_deref(),
      Some("the error's payload exploded")
    );
    let (mut guarded, latch, _meter) = GuardedReader::new(ReturnsAHostileError, u64::MAX);
    let err = quietly(|| guarded.seek(SeekFrom::Start(0))).expect_err("the seek failed");
    let said = err.to_string();
    core::mem::forget(err);
    assert_eq!(said, "the caller's reader panicked");
    assert_eq!(
      latch.message().as_deref(),
      Some("the error's payload exploded")
    );
  }

  /// **An error leaves the guard saying what it said, and owning
  /// nothing**: an OS error keeps its code, any other its kind — all
  /// ffmpeg-next reads of it — and neither carries a payload out.
  #[test]
  fn a_returned_error_keeps_its_code_or_kind_and_no_payload() {
    struct Failing(io::ErrorKind);
    impl Read for Failing {
      fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
        Err(io::Error::from_raw_os_error(5))
      }
    }
    impl Seek for Failing {
      fn seek(&mut self, _pos: SeekFrom) -> io::Result<u64> {
        Err(io::Error::new(self.0, "the reader's own words"))
      }
    }
    let (mut guarded, latch, _meter) =
      GuardedReader::new(Failing(io::ErrorKind::InvalidData), u64::MAX);
    let mut buf = [0u8; 8];
    let os = guarded.read(&mut buf).expect_err("the read failed");
    assert_eq!(os.raw_os_error(), Some(5));
    assert!(os.get_ref().is_none(), "no payload leaves the guard");
    let custom = guarded
      .seek(SeekFrom::Start(0))
      .expect_err("the seek failed");
    assert_eq!(custom.kind(), io::ErrorKind::InvalidData);
    assert!(custom.get_ref().is_none(), "no payload leaves the guard");
    assert!(latch.message().is_none(), "an error is not a panic");
  }

  #[test]
  fn an_unremarkable_reader_passes_straight_through() {
    let (mut guarded, latch, _meter) =
      GuardedReader::new(std::io::Cursor::new(vec![1u8, 2, 3, 4]), u64::MAX);
    let mut buf = [0u8; 4];
    assert_eq!(guarded.read(&mut buf).expect("read"), 4);
    assert_eq!(buf, [1, 2, 3, 4]);
    assert_eq!(guarded.seek(SeekFrom::Start(1)).expect("seek"), 1);
    assert!(latch.message().is_none());
  }
}
