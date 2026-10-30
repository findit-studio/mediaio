//! The panic guard on the custom-writer boundary — the write side of
//! [`crate::reader_guard`].
//!
//! A muxer writes into any `Write + Seek` a caller hands it, and
//! libavformat drives that sink through `AVIOContext` callbacks that are
//! `extern "C"` functions (`ffmpeg_next::format::context::StreamIo`'s
//! `write` and `seek`), with no `catch_unwind` between the caller's code
//! and C. A panic crossing an `extern "C"` frame is not an unwind any
//! caller can catch — it aborts the process.
//!
//! [`GuardedWriter`] closes that on this side of the boundary, exactly as
//! the reader guard does: every call into the caller's sink runs under
//! [`catch_unwind`](std::panic::catch_unwind), a panic is latched in the
//! shared [`PanicLatch`] and reported to C as an ordinary I/O error, and
//! the muxer names the latch once control is back in Rust. The panic
//! never reaches the `extern "C"` frame.
//!
//! **Nor does an error's payload.** ffmpeg-next drops an error the sink
//! returns inside its `extern "C"` callback, and an `io::Error` can box
//! a payload whose `Drop` panics. Every error the sink returns is
//! reduced inside the guard's `catch_unwind` to its OS code or kind, its
//! payload dropped there ([`InertError`]): a destructor that panics is
//! latched like any other panic, and what reaches C owns nothing.
//!
//! **A latched sink is terminal.** Unlike a reader, which libavformat may
//! simply stop reading, a sink that panicked part way through a write is
//! in a state nothing can describe — some bytes of a packet may have
//! landed and some not. Every call after the latch answers an error
//! without touching the sink again, so nothing more is written into a
//! file whose writer has already failed in a way it could not report.
//!
//! **So is a sink that failed a seek** ([`SeekRecord`]). libavformat keeps
//! no record of a failed seek, and a muxer patching what it wrote earlier
//! may write on regardless, into the wrong place; and a `Seek` that fails,
//! of any kind, says nothing of where it left the sink. The guard records
//! every failure, refuses every later call — write, seek and flush — and
//! the context names it.
//!
//! # Every entry into the sink
//!
//! The guard is the one way into the caller's sink: `Output` builds its
//! I/O context over it alone (`StreamIo::from_write_seek`), and hands back
//! the sink only out of it. Each road in, by what calls it, and what the
//! guard asks before it lets the call through:
//!
//! | entry | called from | asked first |
//! |---|---|---|
//! | [`Write::write`] | ffmpeg-next's `write` callback, which loops it over the buffer libavformat hands it (`stream_io.rs` 323–347), for every `avio_flush` and every buffer libavformat fills — at the header, a packet, the trailer, `into_sink` (`output.rs`), `StreamIo::into_inner` and its `Drop` (240, 269) | the latch, then the seek record |
//! | [`Seek::seek`], any `SeekFrom` | the `seek` callback, for `avio_seek`'s positioning (`stream_io.rs` 374–394), and for `AVSEEK_SIZE`, which asks `stream_position` — `seek(SeekFrom::Current(0))` — then `SeekFrom::End(0)`, then `SeekFrom::Start` back (358–372) | the latch, then the seek record; a failure of any kind recorded but an interrupted one, which did nothing and is asked again (388–391) |
//! | [`Write::flush`] | `into_sink`, once the I/O context is flushed (`output.rs`); `StreamIo`'s `Drop`, through `flush_stream` (`stream_io.rs` 270–272, 405–407), when the context is dropped | the latch, then the seek record |
//! | [`Seek::stream_position`] and the other provided methods | anyone holding the guard | their defaults, over the three above |
//!
//! [`GuardedWriter::into_inner`] calls nothing: it hands the sink back. A
//! sink's own `Drop` — a `BufWriter` flushing its buffer — is the sink's,
//! run when the caller's value is dropped, and no call of the guard's.
//!
//! The guard cannot help under `panic = "abort"`, where no unwind exists
//! to catch; that is a whole-binary choice the caller makes.

use std::{
  io::{self, Seek, SeekFrom, Write},
  panic::{AssertUnwindSafe, catch_unwind},
  sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
  },
};

use crate::reader_guard::{InertError, PanicLatch};

/// Where a sink's failed seek is kept until the muxing context can name
/// it.
///
/// **A failed seek leaves nothing behind in libavformat.** `avio_seek`
/// answers the sink's error but keeps none — it sets no
/// `AVIOContext.error` (`aviobuf.c` 236–319 in FFmpeg 9.0.1) — and a
/// muxer patching a header it wrote earlier may ignore the answer and
/// write on, into the wrong place. So the guard keeps the first failed
/// seek here and, from then on, refuses every write, every seek and every
/// flush, so nothing more reaches the sink; the context reports it once
/// libavformat returns.
///
/// **Every failed seek, of any kind.** `Seek` promises nothing of where a
/// failed seek leaves the sink, so no failure can prove the position
/// unchanged — but an interrupted one, which by its kind's word did nothing
/// and is asked again: not `avio_seek`'s positioning (`SeekFrom::Start`, the offset
/// made absolute first, `aviobuf.c` 303–317), nor the size probe —
/// ffmpeg-next's answer to `AVSEEK_SIZE`, `SeekFrom::Current(0)`,
/// `SeekFrom::End(0)` and `SeekFrom::Start` back (`stream_io.rs` 358–372),
/// and `avio_size`'s own fallback by `SEEK_END` (`aviobuf.c` 326–347) —
/// which libavformat survives failing, while the sink it leaves misplaced
/// would take the next write where it does not belong.
#[derive(Debug, Default)]
pub struct SeekRecord {
  failed: AtomicBool,
  error: Mutex<Option<InertError>>,
}

impl SeekRecord {
  /// Keeps `error`; the first failure wins, as the panic latch's does.
  fn record(&self, error: InertError) {
    // A poisoned lock holds a plain `Option`, sound to use; refusing to
    // record would lose the very failure this type carries.
    let mut slot = self.error.lock().unwrap_or_else(|e| e.into_inner());
    if slot.is_none() {
      *slot = Some(error);
    }
    drop(slot);
    self.failed.store(true, Ordering::Release);
  }

  /// Whether a seek has failed: an `Acquire` load, pairing with the
  /// `Release` store in [`Self::record`].
  pub(crate) fn is_set(&self) -> bool {
    self.failed.load(Ordering::Acquire)
  }

  /// The failed seek's error, rebuilt owning nothing, or `None`.
  pub(crate) fn failure(&self) -> Option<io::Error> {
    if !self.is_set() {
      return None;
    }
    self
      .error
      .lock()
      .unwrap_or_else(|e| e.into_inner())
      .map(InertError::rebuild)
  }
}

/// A caller's byte sink, wrapped so that a panic inside it becomes an
/// I/O error instead of an abort, and stays one.
///
/// Deliberately does **not** override `Write::write_all`,
/// `Write::write_vectored` or `Seek::stream_position`: their default
/// implementations are written in terms of [`Write::write`] and
/// [`Seek::seek`], which are guarded, so forwarding them would only add
/// unguarded paths.
pub struct GuardedWriter<W> {
  inner: W,
  latch: Arc<PanicLatch>,
  seeks: Arc<SeekRecord>,
}

impl<W> GuardedWriter<W> {
  /// Wraps `inner`, returning the guard and handles on the latch and the
  /// seek record the muxer keeps to name a panic, or a failed seek, after
  /// the fact.
  pub fn new(inner: W) -> (Self, Arc<PanicLatch>, Arc<SeekRecord>) {
    let latch = Arc::new(PanicLatch::default());
    let seeks = Arc::new(SeekRecord::default());
    (
      Self {
        inner,
        latch: Arc::clone(&latch),
        seeks: Arc::clone(&seeks),
      },
      latch,
      seeks,
    )
  }

  /// The caller's sink, handed back.
  ///
  /// Whatever state a sink that panicked is in, it is the caller's to
  /// judge: the guard returns it either way, and the latch says whether
  /// it panicked.
  pub fn into_inner(self) -> W {
    self.inner
  }

  /// Runs one call into the caller's sink under `catch_unwind` — or none
  /// at all, once a panic is latched — an error it returns reduced there
  /// ([`InertError`]).
  fn guard<T>(&mut self, call: impl FnOnce(&mut W) -> io::Result<T>) -> io::Result<T> {
    let Self { inner, latch, .. } = self;
    if latch.is_latched() {
      return Err(io::Error::other(
        "the caller's writer panicked earlier, and is not called again",
      ));
    }
    // `AssertUnwindSafe` because the sink is exactly what may be left
    // inconsistent by its own panic — and that is why the latch makes the
    // sink terminal: nothing calls it again.
    //
    // An error the sink returns is reduced inside the catch, so its
    // payload is dropped here and not in ffmpeg-next's `extern "C"`
    // callback: a payload whose `Drop` panics takes the path below, and
    // the sink that returned it is terminal like one that panicked.
    match catch_unwind(AssertUnwindSafe(|| call(inner).map_err(InertError::reduce))) {
      Ok(result) => result.map_err(InertError::rebuild),
      Err(payload) => {
        latch.latch(&*payload);
        // Described, then deliberately forgotten, for the reason the
        // reader guard gives: a payload whose `Drop` panics would send
        // that second panic out of `write` and into ffmpeg-next's
        // `extern "C"` callback — the abort this guard exists to
        // prevent, reached through the guard itself.
        core::mem::forget(payload);
        Err(io::Error::other("the caller's writer panicked"))
      }
    }
  }
}

impl<W> GuardedWriter<W> {
  /// Refuses a write, a seek or a flush once a seek has failed
  /// ([`SeekRecord`]).
  fn positioned(&self) -> io::Result<()> {
    if self.seeks.is_set() {
      Err(io::Error::other(
        "the caller's writer failed a seek earlier, and nothing more is written",
      ))
    } else {
      Ok(())
    }
  }
}

impl<W: Write> Write for GuardedWriter<W> {
  fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
    self.positioned()?;
    self.guard(|inner| inner.write(buf))
  }

  /// A flush after a failed seek would commit what the sink buffered at a
  /// position the failure left unknown: refused, as a write is.
  fn flush(&mut self) -> io::Result<()> {
    self.positioned()?;
    self.guard(|inner| inner.flush())
  }
}

impl<W: Seek> Seek for GuardedWriter<W> {
  fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
    self.positioned()?;
    let sought = self.guard(|inner| inner.seek(pos));
    // Every seek that failed is kept, of whatever kind — but one
    // interrupted, which by `io::ErrorKind::Interrupted`'s word did nothing
    // and is to be asked again, as ffmpeg-next asks it (`stream_io.rs`
    // 388–391); a panic is the latch's.
    if let Err(error) = &sought
      && error.kind() != io::ErrorKind::Interrupted
      && !self.latch.is_latched()
    {
      self.seeks.record(InertError::of(error));
    }
    sought
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  /// A sink that panics in each of its three calls, and counts them.
  #[derive(Default)]
  struct Panicking {
    calls: u32,
  }

  impl Write for Panicking {
    fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
      self.calls += 1;
      panic!("write exploded");
    }
    fn flush(&mut self) -> io::Result<()> {
      self.calls += 1;
      panic!("flush exploded");
    }
  }

  impl Seek for Panicking {
    fn seek(&mut self, _pos: SeekFrom) -> io::Result<u64> {
      self.calls += 1;
      panic!("seek exploded");
    }
  }

  /// Silences the default hook for one call, so a deliberate panic does
  /// not print a backtrace into the test log. The hook is global; the
  /// swap is confined to this module's laws, as the reader guard's is.
  fn quietly<T>(call: impl FnOnce() -> T) -> T {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let out = call();
    std::panic::set_hook(previous);
    out
  }

  /// **A panicking write becomes an error, latches its message, and the
  /// sink is never called again.** The second write, the flush and the
  /// seek all answer errors without reaching it — a sink that failed in a
  /// way it could not report is not written into further.
  #[test]
  fn a_panicking_write_latches_and_the_sink_is_not_called_again() {
    let (mut guarded, latch, _) = GuardedWriter::new(Panicking::default());
    assert!(latch.message().is_none(), "nothing has panicked yet");

    let err = quietly(|| guarded.write(b"bytes")).expect_err("a panic is an error, not an abort");
    assert_eq!(err.kind(), io::ErrorKind::Other);
    assert_eq!(latch.message().as_deref(), Some("write exploded"));

    assert!(guarded.write(b"more").is_err());
    assert!(guarded.flush().is_err());
    assert!(guarded.seek(SeekFrom::Start(0)).is_err());
    assert_eq!(
      guarded.into_inner().calls,
      1,
      "only the first call reached the sink",
    );
  }

  /// **A panicking seek, and a panicking flush, latch the same way** —
  /// and `stream_position`, the default over `seek`, is guarded with it.
  #[test]
  fn a_panicking_seek_or_flush_latches_its_message() {
    let (mut guarded, latch, _) = GuardedWriter::new(Panicking::default());
    assert!(quietly(|| guarded.seek(SeekFrom::Start(4))).is_err());
    assert_eq!(latch.message().as_deref(), Some("seek exploded"));
    assert!(quietly(|| guarded.stream_position()).is_err());

    let (mut guarded, latch, _) = GuardedWriter::new(Panicking::default());
    assert!(quietly(|| guarded.flush()).is_err());
    assert_eq!(latch.message().as_deref(), Some("flush exploded"));
  }

  /// A panic payload whose destructor panics in turn.
  struct PanicOnDrop;

  impl Drop for PanicOnDrop {
    fn drop(&mut self) {
      panic!("and the payload went too");
    }
  }

  struct PanicsWithAPayload;

  impl Write for PanicsWithAPayload {
    fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
      std::panic::panic_any(PanicOnDrop);
    }
    fn flush(&mut self) -> io::Result<()> {
      Ok(())
    }
  }

  /// **A payload that panics on drop is not dropped** — this law
  /// completing at all is the assertion, as the reader guard's is.
  #[test]
  fn a_payload_that_panics_on_drop_does_not_get_dropped() {
    let (mut guarded, latch, _) = GuardedWriter::new(PanicsWithAPayload);
    assert!(quietly(|| guarded.write(b"bytes")).is_err());
    assert!(latch.message().is_some());
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

  /// A sink whose every call returns an error carrying that payload, and
  /// counts the calls.
  #[derive(Default)]
  struct ReturnsAHostileError {
    calls: u32,
  }

  impl ReturnsAHostileError {
    fn fail<T>(&mut self) -> io::Result<T> {
      self.calls += 1;
      Err(io::Error::new(io::ErrorKind::StorageFull, HostilePayload))
    }
  }

  impl Write for ReturnsAHostileError {
    fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
      self.fail()
    }
    fn flush(&mut self) -> io::Result<()> {
      self.fail()
    }
  }

  impl Seek for ReturnsAHostileError {
    fn seek(&mut self, _pos: SeekFrom) -> io::Result<u64> {
      self.fail()
    }
  }

  /// **An error whose payload panics on drop is reduced inside the
  /// guard: the destructor's panic is latched and named, and the sink is
  /// not called again.** ffmpeg-next drops a returned error inside its
  /// `extern "C"` callback, where that panic would abort the process
  /// (`output::tests` drives the whole road in a child process); here it
  /// is caught where the error is reduced, and what leaves the guard is
  /// the guard's own error.
  #[test]
  fn an_error_whose_payload_panics_on_drop_is_latched_and_named() {
    let (mut guarded, latch, _) = GuardedWriter::new(ReturnsAHostileError::default());
    let err = quietly(|| guarded.write(b"bytes")).expect_err("the write failed");
    let said = err.to_string();
    // Forgotten, not dropped: were the guard to hand the payload out,
    // dropping it while this law fails would panic during the unwind and
    // abort the test binary instead of naming the defect.
    core::mem::forget(err);
    assert_eq!(said, "the caller's writer panicked");
    assert_eq!(
      latch.message().as_deref(),
      Some("the error's payload exploded")
    );
    assert!(guarded.write(b"more").is_err());
    assert!(guarded.flush().is_err());
    assert!(guarded.seek(SeekFrom::Start(0)).is_err());
    assert_eq!(
      guarded.into_inner().calls,
      1,
      "a sink whose error panicked is not called again",
    );
  }

  /// **An error leaves the guard saying what it said, and owning
  /// nothing**: an OS error keeps its code, any other its kind — all
  /// ffmpeg-next reads of it — and neither carries a payload out. Nothing
  /// is latched: an error is not a panic, and the sink stays callable.
  #[test]
  fn a_returned_error_keeps_its_code_or_kind_and_no_payload() {
    #[derive(Default)]
    struct Failing {
      calls: u32,
    }
    impl Write for Failing {
      fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
        self.calls += 1;
        Err(io::Error::from_raw_os_error(28))
      }
      fn flush(&mut self) -> io::Result<()> {
        self.calls += 1;
        Err(io::Error::new(
          io::ErrorKind::WriteZero,
          "the sink's own words",
        ))
      }
    }
    let (mut guarded, latch, _) = GuardedWriter::new(Failing::default());
    let os = guarded.write(b"bytes").expect_err("the write failed");
    assert_eq!(os.raw_os_error(), Some(28));
    assert!(os.get_ref().is_none(), "no payload leaves the guard");
    let custom = guarded.flush().expect_err("the flush failed");
    assert_eq!(custom.kind(), io::ErrorKind::WriteZero);
    assert!(custom.get_ref().is_none(), "no payload leaves the guard");
    assert!(latch.message().is_none(), "an error is not a panic");
    assert!(guarded.write(b"again").is_err());
    assert_eq!(guarded.into_inner().calls, 3, "the sink is called again");
  }

  /// A buffer that refuses to seek back before where it stands, as a
  /// pipe or an append-only store does, and counts the calls that reach
  /// it.
  #[derive(Default)]
  struct ForwardOnly {
    bytes: io::Cursor<Vec<u8>>,
    calls: u32,
  }

  impl Write for ForwardOnly {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
      self.calls += 1;
      self.bytes.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
      self.calls += 1;
      Ok(())
    }
  }

  impl Seek for ForwardOnly {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
      self.calls += 1;
      let here = self.bytes.position();
      let end = self.bytes.get_ref().len() as u64;
      let target = match pos {
        SeekFrom::Start(at) => Some(at),
        SeekFrom::Current(by) => here.checked_add_signed(by),
        SeekFrom::End(by) => end.checked_add_signed(by),
      };
      match target {
        Some(target) if target >= here => self.bytes.seek(SeekFrom::Start(target)),
        _ => Err(io::Error::from_raw_os_error(libc::ESPIPE)),
      }
    }
  }

  /// **A positioning seek that fails is kept, and nothing more reaches the
  /// sink.** The seek back fails with the sink's own error, which the record
  /// keeps — its code, owning nothing — and every later write, seek and
  /// flush is refused without reaching the sink, so nothing lands where it
  /// does not belong. Nothing is latched: a failed seek is no panic.
  #[test]
  fn a_failed_positioning_seek_is_kept_and_nothing_more_is_written() {
    let (mut guarded, latch, seeks) = GuardedWriter::new(ForwardOnly::default());
    guarded.write_all(b"header").expect("write");
    assert!(seeks.failure().is_none(), "nothing has failed yet");
    let refused = guarded.seek(SeekFrom::Start(0)).expect_err("a seek back");
    assert_eq!(refused.raw_os_error(), Some(libc::ESPIPE));
    let kept = seeks.failure().expect("the failed seek is kept");
    assert_eq!(kept.raw_os_error(), Some(libc::ESPIPE));
    assert!(kept.get_ref().is_none(), "kept owning nothing");
    assert!(guarded.write(b"patch").is_err(), "no write after it");
    assert!(guarded.seek(SeekFrom::Start(6)).is_err(), "no seek either");
    assert!(guarded.flush().is_err(), "nor a flush");
    assert!(latch.message().is_none(), "a failed seek is not a panic");
    let sink = guarded.into_inner();
    assert_eq!(sink.bytes.into_inner(), b"header", "only what came before");
    // The write and the failed seek: nothing else reached it.
    assert_eq!(sink.calls, 2);
  }

  /// A sink whose `SeekFrom::Current` or `SeekFrom::End` fails — the size
  /// probe's two first questions — and counts every call that reaches it.
  struct FailsTo {
    current: bool,
    end: bool,
    calls: u32,
  }

  impl Write for FailsTo {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
      self.calls += 1;
      Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
      self.calls += 1;
      Ok(())
    }
  }

  impl Seek for FailsTo {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
      self.calls += 1;
      match pos {
        SeekFrom::Current(_) if self.current => Err(io::Error::from_raw_os_error(libc::EIO)),
        SeekFrom::End(_) if self.end => Err(io::Error::from_raw_os_error(libc::ESPIPE)),
        _ => Ok(0),
      }
    }
  }

  /// **A size probe that fails is kept, and nothing more reaches the
  /// sink.** ffmpeg-next answers libavformat's `AVSEEK_SIZE` with
  /// `stream_position` — `SeekFrom::Current(0)` — then `SeekFrom::End(0)`;
  /// either failing leaves the sink where nothing says, so each is terminal
  /// as a positioning seek is: no write, seek or flush reaches the sink
  /// after it.
  #[test]
  fn a_size_probe_that_fails_is_kept_and_terminal() {
    for (current, end, code) in [(true, false, libc::EIO), (false, true, libc::ESPIPE)] {
      let (mut guarded, latch, seeks) = GuardedWriter::new(FailsTo {
        current,
        end,
        calls: 0,
      });
      let probe = if current {
        guarded.stream_position()
      } else {
        guarded.seek(SeekFrom::End(0))
      };
      assert!(probe.is_err(), "the probe fails");
      assert_eq!(
        seeks.failure().and_then(|error| error.raw_os_error()),
        Some(code),
        "kept"
      );
      assert!(guarded.write(b"more").is_err());
      assert!(guarded.seek(SeekFrom::Start(0)).is_err());
      assert!(guarded.flush().is_err());
      assert!(latch.message().is_none());
      assert_eq!(guarded.into_inner().calls, 1, "only the probe reached it");
    }
  }

  /// **An interrupted seek is not kept**: it did nothing, and the next one
  /// reaches the sink — as ffmpeg-next asks it again.
  #[test]
  fn an_interrupted_seek_is_not_kept() {
    struct Interrupting {
      once: bool,
      calls: u32,
    }
    impl Seek for Interrupting {
      fn seek(&mut self, _pos: SeekFrom) -> io::Result<u64> {
        self.calls += 1;
        if std::mem::replace(&mut self.once, false) {
          return Err(io::ErrorKind::Interrupted.into());
        }
        Ok(0)
      }
    }
    let (mut guarded, _, seeks) = GuardedWriter::new(Interrupting {
      once: true,
      calls: 0,
    });
    assert!(guarded.seek(SeekFrom::Start(0)).is_err(), "interrupted");
    assert!(seeks.failure().is_none(), "not kept");
    assert_eq!(guarded.seek(SeekFrom::Start(0)).expect("asked again"), 0);
    assert_eq!(guarded.into_inner().calls, 2);
  }

  /// **A seek that panics is the latch's, not the record's**: the panic
  /// is named as one, and the sink is not called again either way.
  #[test]
  fn a_seek_that_panics_is_latched_not_kept() {
    let (mut guarded, latch, seeks) = GuardedWriter::new(Panicking::default());
    assert!(quietly(|| guarded.seek(SeekFrom::Start(0))).is_err());
    assert_eq!(latch.message().as_deref(), Some("seek exploded"));
    assert!(seeks.failure().is_none(), "a panic is not a failed seek");
  }

  #[test]
  fn an_unremarkable_writer_passes_straight_through() {
    let (mut guarded, latch, _) = GuardedWriter::new(io::Cursor::new(Vec::new()));
    guarded.write_all(b"abcd").expect("write");
    assert_eq!(guarded.seek(SeekFrom::Start(1)).expect("seek"), 1);
    guarded.write_all(b"X").expect("write");
    guarded.flush().expect("flush");
    assert!(latch.message().is_none());
    assert_eq!(guarded.into_inner().into_inner(), b"aXcd");
  }
}
