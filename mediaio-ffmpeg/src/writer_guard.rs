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
//! **And so is the sink of an output whose call failed, once the output
//! is torn down** ([`SinkClosed`]; R21, Codex R20 3). The output closes
//! the guard before its context is freed: what the muxer's `deinit`
//! might write, the I/O context's last flush and the sink's own `flush`
//! that ffmpeg-next's `StreamIo` runs on its way out are each refused, so
//! that the sink holds the bytes written before the failure, and nothing
//! after it.
//!
//! **And so is a sink that failed a write or a flush** (R27, Codex R26
//! [medium]). The I/O context keeps a failed write's error and writes
//! nothing more of its own (`writeout`, `aviobuf.c` 136–166 in FFmpeg
//! 9.0.1), but other roads still reach the sink: `avio_seek` flushes and
//! then seeks whatever the flush answered (303–317), and `avio_size` asks
//! the sink's size (326–347). The guard latches every failure but an
//! interrupted one — a write that takes no byte of some, or more than it
//! was handed, among them — and refuses every later write, seek and flush.
//!
//! # Every road back into the sink after a failure
//!
//! By grep of every call of the I/O context's `write_packet` and `seek`
//! over `aviobuf.c` and of the guard's entries over ffmpeg-next's
//! `stream_io.rs`, each road by which one call into libavformat, or the
//! output's teardown, reaches the sink again after one of its calls failed,
//! and what stops it:
//!
//! | after | the road back | stopped by |
//! |---|---|---|
//! | a write failed | `writeout`'s next buffer (136–166) | the I/O context itself, which skips every write once its `error` is set; and the guard |
//! | a write failed | `avio_seek`'s flush and then its seek (303–317), as the MOV family's `mdat` patch and every trailer's header patch run it; `avio_skip` (321–324) | the guard (R27): the seek refused unasked, kept as no failed seek, the write's error named |
//! | a write failed | `avio_size`'s size probe and its `SEEK_END` fallback (326–347); ffmpeg-next's `AVSEEK_SIZE`, three seeks (`stream_io.rs` 358–372) | the guard (R27) |
//! | a write failed | `Output::into_sink`'s flush, and `StreamIo`'s `Drop`'s flush of the sink (270–272) | the output's close (R21) and the guard (R27) |
//! | a write was interrupted | ffmpeg-next's loop asks it again (333–343) | nothing: by its kind, it did nothing |
//! | a seek failed | the next write, seek or flush; `avio_size`'s fallback after a failed probe (338–343) | the seek record ([`SeekRecord`]) |
//! | a flush failed | `StreamIo`'s `Drop`, after `into_sink`'s refusal | the output's close and the guard (R27) |
//! | a panic | any call | the latch |
//! | the output failed | its teardown: `deinit`, the last flush, the sink's `flush` | the close ([`SinkClosed`]) |
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
//! | [`Write::write`] | ffmpeg-next's `write` callback, which loops it over the buffer libavformat hands it (`stream_io.rs` 323–347), for every `avio_flush` and every buffer libavformat fills — at the header, a packet, the trailer, `into_sink` (`output.rs`), `StreamIo::into_inner` and its `Drop` (240, 269) | whether the sink is closed; the latch and the seek record |
//! | [`Seek::seek`], any `SeekFrom` | the `seek` callback, for `avio_seek`'s positioning (`stream_io.rs` 374–394), and for `AVSEEK_SIZE`, which asks `stream_position` — `seek(SeekFrom::Current(0))` — then `SeekFrom::End(0)`, then `SeekFrom::Start` back (358–372) | whether the sink is closed; the latch and the seek record; a failure of any kind recorded but an interrupted one, which did nothing and is asked again (388–391) |
//! | [`Write::flush`] | `into_sink`, once the I/O context is flushed (`output.rs`); `StreamIo`'s `Drop`, through `flush_stream` (`stream_io.rs` 270–272, 405–407), when the context is dropped | whether the sink is closed; the latch and the seek record |
//! | [`Seek::stream_position`] and the other provided methods | anyone holding the guard | their defaults, over the three above |
//!
//! [`GuardedWriter::into_inner`] calls nothing: it hands the sink back. A
//! sink's own `Drop` — a `BufWriter` flushing its buffer — is the sink's,
//! run when the caller's value is dropped, and no call of the guard's.
//!
//! The guard cannot help under `panic = "abort"`, where no unwind exists
//! to catch; that is a whole-binary choice the caller makes.
//!
//! # The tap
//!
//! A muxer that audits what libavformat wrote reads it here, where every
//! byte reaches the caller's sink ([`SinkTap`], installed by
//! [`Output::set_tap`](crate::output::Output::set_tap)). The guard calls
//! the tap on one road alone: a write the sink answered `Ok(n)` with
//! `0 < n ≤ len`, the road R27's latch leaves standing, and then with the
//! `n` bytes the sink took and no other. A write the latch marks failed, an
//! interrupted one, one refused before it reached the sink, and every call
//! after a panic tap nothing; ffmpeg-next's loop over a partial write
//! (`stream_io.rs` 333–343) taps each part at its own offset. So the tap
//! sees exactly the bytes the sink took, in the order it took them.
//!
//! **The offset is libavformat's**: the bytes taken since the output's
//! first, moved by each seek to the offset libavformat asked for.
//! libavformat reaches the sink's seek only with `SEEK_SET` and an
//! absolute offset (`avio_seek`, `aviobuf.c` 303–317 in FFmpeg 9.0.1) and
//! sets its own `s->pos` to that offset, not to the sink's answer; the
//! guard follows it the same way. A seek of any other kind — from the
//! end, or by a nonzero distance — leaves libavformat's offsets for good
//! ([`SinkTap::lost`]).
//!
//! **A panic in the tap stays in the guard.** The tap runs inside
//! ffmpeg-next's `extern "C"` callback, so the guard calls it under a
//! `catch_unwind` of its own, holding the tap's lock while it runs: a
//! panic poisons the lock, its payload is forgotten as the sink's is, and
//! a poisoned tap is not called again. The sink did not panic, so its
//! write stands and the latch is untouched; the output answers the
//! poisoned tap by name when it is asked for
//! ([`OutputError::TapPanicked`](crate::output::OutputError::TapPanicked)).
//!
//! **The road allocates nothing.** The tap is set once, while the output
//! is being built; each write the sink takes then costs a load, an
//! uncontended lock and a `catch_unwind` frame, which allocates nothing
//! unless a panic is thrown.

use std::{
  any::Any,
  io::{self, Seek, SeekFrom, Write},
  panic::{AssertUnwindSafe, catch_unwind},
  sync::{
    Arc, Mutex, OnceLock,
    atomic::{AtomicBool, Ordering},
  },
};

use crate::reader_guard::{InertError, PanicLatch};

/// A reader of the bytes the caller's sink takes — each run at
/// libavformat's offset, in the order the sink took them: what a muxer
/// audits of what libavformat wrote ([the module](self)).
///
/// [`Output::set_tap`](crate::output::Output::set_tap) installs one while
/// the output is being built, before the header writes its first byte, and
/// [`Output::with_tap`](crate::output::Output::with_tap) lends it back as
/// its own type. The guard calls it inside libavformat's write callback,
/// under a `catch_unwind` of its own: a tap that panics is not called
/// again, and the sink's write stands.
pub trait SinkTap: Any + Send {
  /// The sink took `bytes`, at `at`.
  ///
  /// `at` is libavformat's offset from the output's first byte: the bytes
  /// taken since, moved by each seek to the absolute offset libavformat
  /// asked for. It follows the request, as libavformat's own `s->pos`
  /// does, and not the sink's answer (`avio_seek`, `aviobuf.c` 303–317 in
  /// FFmpeg 9.0.1). Called once for each write the sink answered with some
  /// of the bytes it was handed — each part of a partial write at its own
  /// offset — and never for bytes the sink failed, refused, or was not
  /// asked to take.
  fn took(&mut self, at: u64, bytes: &[u8]);

  /// The sink's position left libavformat's absolute offsets: a seek from
  /// the end, or by a nonzero distance from where the sink stands — the
  /// size probe ffmpeg-next answers `AVSEEK_SIZE` with (`stream_io.rs`
  /// 358–372), or `avio_size`'s fallback by `SEEK_END`.
  ///
  /// Called once, and no [`took`](Self::took) follows, even after a later
  /// seek to an absolute offset. The probe's own seek back
  /// (`SeekFrom::Start`) could name the position again, but the guard does
  /// not take it back: a tap told its position is lost judges the output as
  /// one it could not follow. That fails closed, and costs no file today:
  /// none of the writers a tap is installed over asks the sink's size or
  /// seeks from its end (no `avio_size`, `SEEK_END` or `AVSEEK_SIZE` in
  /// `movenc.c`, `movenc_ttml.c`, `matroskaenc.c`, `flvenc.c`, `flacenc.c`,
  /// `mux.c` or `mux_utils.c` in FFmpeg 9.0.1), so a writer that one day
  /// probes the sink fails every file it writes rather than passing one the
  /// tap did not see whole.
  fn lost(&mut self);
}

/// What the guard and the output that owns it share of the tap: set once,
/// while the output is being built ([`Self::fill`]), and read by the guard
/// at each write the sink takes. Shared as the closing word is
/// ([`GuardedWriter::closer`]): the guard lives inside the I/O context,
/// where the output reaches it only through libavformat.
#[derive(Default)]
pub(crate) struct TapSlot {
  tap: OnceLock<Mutex<Box<dyn SinkTap>>>,
  /// What rewrites the bytes libavformat hands the guard before the sink
  /// and the tap see them: test support ([`Plant`]).
  #[cfg(any(test, feature = "test-support"))]
  plant: Mutex<Option<Plant>>,
  /// What runs around libavformat's take of each packet the output writes:
  /// test support
  /// ([`Output::set_write_hook`](crate::output::Output::set_write_hook)).
  /// Set once, so that an output of none locks nothing, and allocates
  /// nothing, at its packets.
  #[cfg(any(test, feature = "test-support"))]
  hook: OnceLock<Mutex<Box<dyn FnMut(bool) + Send>>>,
}

impl TapSlot {
  /// Sets `tap`, unless one is set: whether it was.
  pub(crate) fn fill(&self, tap: Box<dyn SinkTap>) -> bool {
    self.tap.set(Mutex::new(tap)).is_ok()
  }

  /// The tap, under its lock, if one is set.
  pub(crate) fn tap(&self) -> Option<&Mutex<Box<dyn SinkTap>>> {
    self.tap.get()
  }

  /// Sets the packet hook, unless one is set: whether it was. Its lock is
  /// taken once here, so that the lock's own first allocation falls at
  /// the set, not at a packet's write.
  #[cfg(any(test, feature = "test-support"))]
  pub(crate) fn set_hook(&self, hook: Box<dyn FnMut(bool) + Send>) -> bool {
    let set = self.hook.set(Mutex::new(hook)).is_ok();
    if let Some(hook) = self.hook.get() {
      drop(hook.lock());
    }
    set
  }

  /// Runs the packet hook, where one is set, with `before`.
  #[cfg(any(test, feature = "test-support"))]
  pub(crate) fn hook(&self, before: bool) {
    if let Some(hook) = self.hook.get() {
      let mut hook = hook
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
      (*hook)(before);
    }
  }

  /// Sets the plant, replacing any.
  #[cfg(any(test, feature = "test-support"))]
  pub(crate) fn plant(&self, change: Box<dyn FnMut(u64, &mut [u8]) + Send>) {
    // A poisoned lock holds a plain `Option`, sound to replace.
    *self
      .plant
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Plant {
      change,
      scratch: Vec::new(),
    });
  }
}

/// Test support: a change made to the bytes libavformat hands the guard,
/// in place and keeping their length, before the sink and the tap see them
/// — libavformat writing otherwise, as a muxer's laws make it
/// ([`Output::set_write_plant`](crate::output::Output::set_write_plant)).
#[cfg(any(test, feature = "test-support"))]
pub(crate) struct Plant {
  change: Box<dyn FnMut(u64, &mut [u8]) + Send>,
  /// The copy the change is made in: libavformat's buffer is not the
  /// guard's to change.
  scratch: Vec<u8>,
}

#[cfg(any(test, feature = "test-support"))]
impl Plant {
  /// `buf`, written at `at`, as the plant changes it; or the payload of
  /// the change's panic, which the caller latches as the sink's.
  fn apply(&mut self, at: u64, buf: &[u8]) -> Result<&[u8], Box<dyn Any + Send>> {
    let Self { change, scratch } = self;
    scratch.clear();
    scratch.extend_from_slice(buf);
    catch_unwind(AssertUnwindSafe(|| change(at, scratch)))?;
    Ok(scratch)
  }
}

/// Calls `tap` under its lock and a `catch_unwind` of its own: the tap
/// runs inside ffmpeg-next's `extern "C"` callback, which a panic must not
/// cross. A panic poisons the lock — its guard drops while the panic
/// unwinds — and its payload is forgotten, for the reason
/// [`GuardedWriter::guard`] forgets the sink's; a poisoned tap is not
/// called again.
fn call_tap(tap: &Mutex<Box<dyn SinkTap>>, call: impl FnOnce(&mut dyn SinkTap)) {
  // `AssertUnwindSafe` because the tap is exactly what its own panic may
  // leave inconsistent — and that is why the poisoned lock is never taken
  // again.
  let called = catch_unwind(AssertUnwindSafe(|| {
    if let Ok(mut tap) = tap.lock() {
      call(&mut **tap);
    }
  }));
  if let Err(payload) = called {
    core::mem::forget(payload);
  }
}

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

/// The word, beside the guard, that the output whose call failed is torn
/// down ([`GuardedWriter::closer`]): from then on the guard refuses every
/// write, every seek and every flush, so that nothing more reaches the
/// caller's sink (R21, Codex R20 3).
#[derive(Debug, Default)]
pub(crate) struct SinkClosed {
  closed: AtomicBool,
}

impl SinkClosed {
  /// Closes the sink to every later call: a `Release` store.
  pub(crate) fn close(&self) {
    self.closed.store(true, Ordering::Release);
  }

  /// Whether the sink is closed: an `Acquire` load, pairing with the
  /// `Release` store in [`Self::close`].
  pub(crate) fn is_closed(&self) -> bool {
    self.closed.load(Ordering::Acquire)
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
  closed: Arc<SinkClosed>,
  /// Whether a write or a flush has failed (R27, Codex R26 [medium]): the
  /// I/O context keeps the write's error and names it, but `avio_seek`
  /// still calls the sink's seek after the flush that failed.
  failed: bool,
  /// The tap's slot, shared with the output that owns the guard.
  tap: Arc<TapSlot>,
  /// Where the next byte the sink takes lands, in libavformat's offsets;
  /// `None` once the position has left them ([`SinkTap::lost`]).
  at: Option<u64>,
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
        closed: Arc::new(SinkClosed::default()),
        failed: false,
        tap: Arc::new(TapSlot::default()),
        at: Some(0),
      },
      latch,
      seeks,
    )
  }

  /// A handle on the word that closes the sink ([`SinkClosed`]), for the
  /// output that owns the guard.
  pub(crate) fn closer(&self) -> Arc<SinkClosed> {
    Arc::clone(&self.closed)
  }

  /// A handle on the tap's slot ([`TapSlot`]), for the output that owns
  /// the guard.
  pub(crate) fn tap_slot(&self) -> Arc<TapSlot> {
    Arc::clone(&self.tap)
  }

  /// The sink took `bytes`, all of them, at the position: the tap is told,
  /// and the position moves past them. Nothing, once the position is lost.
  fn took(&mut self, bytes: &[u8]) {
    let Some(at) = self.at else {
      return;
    };
    if bytes.is_empty() {
      return;
    }
    if let Some(tap) = self.tap.tap() {
      call_tap(tap, |tap| tap.took(at, bytes));
    }
    // libavformat counts its offsets in an `int64_t`, so no sink reaches
    // the end of a `u64`; were one to, its position is lost, not wrapped.
    match u64::try_from(bytes.len())
      .ok()
      .and_then(|len| at.checked_add(len))
    {
      Some(next) => self.at = Some(next),
      None => self.lose(),
    }
  }

  /// The position left libavformat's offsets: the tap is told, once, and
  /// is told of no byte after it ([`SinkTap::lost`]).
  fn lose(&mut self) {
    if self.at.take().is_some()
      && let Some(tap) = self.tap.tap()
    {
      call_tap(tap, |tap| tap.lost());
    }
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
  /// Refuses a write, a seek or a flush once the output whose call failed
  /// has closed the sink ([`SinkClosed`]), or once a seek has failed
  /// ([`SeekRecord`]).
  fn positioned(&self) -> io::Result<()> {
    if self.closed.is_closed() {
      return Err(io::Error::other(
        "the output failed, and nothing more reaches the caller's writer",
      ));
    }
    if self.failed {
      return Err(io::Error::other(
        "the caller's writer failed a write earlier, and is not called again",
      ));
    }
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
  /// A write that fails — but one interrupted, which did nothing and is
  /// asked again (`stream_io.rs` 333–343) — or that takes no byte of a
  /// buffer of some, or more than it was handed, which ffmpeg-next answers
  /// as failures, latches the sink as failed: every later write, seek and
  /// flush is refused without reaching it (R27, Codex R26 [medium]).
  ///
  /// The one write the latch leaves standing — `Ok(n)`, `0 < n ≤ len` —
  /// is the one the tap sees, with the `n` bytes the sink took
  /// ([the module](self)).
  fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
    self.positioned()?;
    #[cfg(any(test, feature = "test-support"))]
    let slot = Arc::clone(&self.tap);
    #[cfg(any(test, feature = "test-support"))]
    let mut plant = slot
      .plant
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
    #[cfg(any(test, feature = "test-support"))]
    let buf = match (plant.as_mut(), self.at) {
      (Some(plant), Some(at)) => match plant.apply(at, buf) {
        Ok(planted) => planted,
        // A plant that panics is latched as the sink's panic: the output
        // names it, and nothing more reaches the sink.
        Err(payload) => {
          self.latch.latch(&*payload);
          core::mem::forget(payload);
          self.failed = true;
          return Err(io::Error::other("the write plant panicked"));
        }
      },
      _ => buf,
    };
    let written = self.guard(|inner| inner.write(buf));
    match &written {
      Ok(taken) if (*taken == 0 && !buf.is_empty()) || *taken > buf.len() => self.failed = true,
      Ok(taken) => self.took(&buf[..*taken]),
      Err(error) if error.kind() != io::ErrorKind::Interrupted => self.failed = true,
      Err(_) => {}
    }
    written
  }

  /// A flush after a failed seek would commit what the sink buffered at a
  /// position the failure left unknown: refused, as a write is. A flush
  /// that fails latches the sink as a failed write does.
  fn flush(&mut self) -> io::Result<()> {
    self.positioned()?;
    let flushed = self.guard(|inner| inner.flush());
    if let Err(error) = &flushed
      && error.kind() != io::ErrorKind::Interrupted
    {
      self.failed = true;
    }
    flushed
  }
}

impl<W: Seek> Seek for GuardedWriter<W> {
  /// The tap's position follows a seek the sink answered: to the offset
  /// asked, by `SeekFrom::Start`; nowhere, by `SeekFrom::Current(0)`; out
  /// of libavformat's offsets for good, by any other ([`SinkTap::lost`]).
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
    // A sink that panicked, now or before, is reached no more, and its
    // position matters to no one.
    if !self.latch.is_latched() {
      match pos {
        SeekFrom::Start(to) => {
          if sought.is_ok() && self.at.is_some() {
            self.at = Some(to);
          }
        }
        SeekFrom::Current(0) => {}
        SeekFrom::Current(_) | SeekFrom::End(_) => self.lose(),
      }
    }
    sought
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  /// **A closed sink is not called again** (R21, Codex R20 3): once the
  /// output whose call failed closes it, a write, a seek and a flush each
  /// answer an error without reaching the sink, and none is kept as a
  /// failed seek.
  #[test]
  fn a_closed_sink_is_not_called_again() {
    let (mut guarded, latch, seeks) = GuardedWriter::new(io::Cursor::new(Vec::new()));
    guarded.closer().close();
    assert!(guarded.write(b"after").is_err());
    assert!(guarded.flush().is_err());
    assert!(guarded.seek(SeekFrom::Start(0)).is_err());
    assert!(!seeks.is_set(), "no failed seek kept");
    assert!(!latch.is_latched(), "no panic latched");
    assert!(
      guarded.into_inner().into_inner().is_empty(),
      "nothing reached the sink"
    );
  }

  /// A sink whose first write fails, or takes no byte, and which counts
  /// every call that reaches it.
  struct FailsOnce {
    calls: u32,
    answer: fn() -> io::Result<usize>,
  }

  impl Write for FailsOnce {
    fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
      self.calls += 1;
      (self.answer)()
    }
    fn flush(&mut self) -> io::Result<()> {
      self.calls += 1;
      Ok(())
    }
  }

  impl Seek for FailsOnce {
    fn seek(&mut self, _pos: SeekFrom) -> io::Result<u64> {
      self.calls += 1;
      Ok(0)
    }
  }

  /// **A failed write is terminal** (R27, Codex R26 [medium]): after a
  /// write the sink fails — or one that takes no byte of a buffer of some —
  /// a seek, a write and a flush each answer an error without reaching the
  /// sink, and none is kept as a failed seek; an interrupted write, which
  /// did nothing, latches nothing.
  #[test]
  fn a_failed_write_is_not_followed_into_the_sink() {
    for answer in [
      (|| Err(io::Error::other("full"))) as fn() -> io::Result<usize>,
      || Ok(0),
    ] {
      let (mut guarded, latch, seeks) = GuardedWriter::new(FailsOnce { calls: 0, answer });
      assert!(matches!(guarded.write(b"bytes"), Err(_) | Ok(0)));
      assert!(guarded.seek(SeekFrom::Start(0)).is_err());
      assert!(guarded.write(b"more").is_err());
      assert!(guarded.flush().is_err());
      assert!(!seeks.is_set(), "no failed seek kept");
      assert!(!latch.is_latched(), "no panic latched");
      assert_eq!(
        guarded.into_inner().calls,
        1,
        "only the failed write reached the sink"
      );
    }
    let (mut guarded, _, _) = GuardedWriter::new(FailsOnce {
      calls: 0,
      answer: || Err(io::ErrorKind::Interrupted.into()),
    });
    assert!(guarded.write(b"bytes").is_err());
    assert!(
      guarded.seek(SeekFrom::Start(0)).is_ok(),
      "an interrupted write latches nothing"
    );
    assert_eq!(guarded.into_inner().calls, 2);
  }

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
  /// ffmpeg-next reads of it — and neither carries a payload out. No panic
  /// is latched: an error is not a panic; but the sink is not called again
  /// (R27, Codex R26 [medium]: before, it stayed callable).
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
    assert!(latch.message().is_none(), "an error is not a panic");
    assert!(guarded.write(b"again").is_err());
    assert_eq!(
      guarded.into_inner().calls,
      1,
      "the sink is not called again"
    );
    let (mut guarded, latch, _) = GuardedWriter::new(Failing::default());
    let custom = guarded.flush().expect_err("the flush failed");
    assert_eq!(custom.kind(), io::ErrorKind::WriteZero);
    assert!(custom.get_ref().is_none(), "no payload leaves the guard");
    assert!(latch.message().is_none(), "an error is not a panic");
    assert!(guarded.flush().is_err());
    assert_eq!(
      guarded.into_inner().calls,
      1,
      "the sink is not called again"
    );
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

  /// What a tap was told, in order.
  #[derive(Debug, PartialEq, Eq)]
  enum Told {
    Took(u64, Vec<u8>),
    Lost,
  }

  /// A tap that tells a log the law holds what it is told.
  struct Logged(Arc<Mutex<Vec<Told>>>);

  impl SinkTap for Logged {
    fn took(&mut self, at: u64, bytes: &[u8]) {
      self
        .0
        .lock()
        .expect("the log")
        .push(Told::Took(at, bytes.to_vec()));
    }
    fn lost(&mut self) {
      self.0.lock().expect("the log").push(Told::Lost);
    }
  }

  /// A guard over `inner`, tapped, and the tap's log.
  fn tapped<W>(inner: W) -> (GuardedWriter<W>, Arc<PanicLatch>, Arc<Mutex<Vec<Told>>>) {
    let (guarded, latch, _) = GuardedWriter::new(inner);
    let log = Arc::new(Mutex::new(Vec::new()));
    assert!(guarded.tap_slot().fill(Box::new(Logged(Arc::clone(&log)))));
    (guarded, latch, log)
  }

  fn took(at: u64, bytes: &[u8]) -> Told {
    Told::Took(at, bytes.to_vec())
  }

  /// **B6 — a position lost stays lost.** The tap follows a seek to an
  /// absolute offset and is left where it was by `SeekFrom::Current(0)`; a
  /// seek from the end loses the position: `lost` is told once, and no byte
  /// is tapped after it — not even past a later seek to an absolute offset,
  /// which a size probe's seek back would be. The sink takes every byte all
  /// the same.
  #[test]
  fn b6_a_seek_from_the_end_loses_the_position_once_and_for_good() {
    let (mut guarded, _, log) = tapped(io::Cursor::new(Vec::new()));
    guarded.write_all(b"abc").expect("write");
    guarded.seek(SeekFrom::Start(1)).expect("seek");
    guarded.write_all(b"X").expect("write");
    assert_eq!(guarded.stream_position().expect("where"), 2);
    guarded.write_all(b"Y").expect("write");
    guarded.seek(SeekFrom::End(0)).expect("seek");
    guarded.write_all(b"de").expect("write");
    guarded.seek(SeekFrom::Start(0)).expect("seek");
    guarded.write_all(b"Z").expect("write");
    guarded.seek(SeekFrom::End(0)).expect("seek");
    assert_eq!(
      *log.lock().expect("the log"),
      [took(0, b"abc"), took(1, b"X"), took(2, b"Y"), Told::Lost]
    );
    assert_eq!(guarded.into_inner().into_inner(), b"ZXYde");
  }

  /// **A write the latch marks failed taps nothing, and neither does one
  /// interrupted**: an error, no byte taken of some, and more than handed
  /// each latch the sink, and none reaches the tap; an interrupted write,
  /// which did nothing, does not either.
  #[test]
  fn a_write_the_latch_marks_failed_taps_nothing() {
    for answer in [
      (|| Err(io::Error::other("full"))) as fn() -> io::Result<usize>,
      || Ok(0),
      || Ok(1_000),
      || Err(io::ErrorKind::Interrupted.into()),
    ] {
      let (mut guarded, _, log) = tapped(FailsOnce { calls: 0, answer });
      let answered = guarded.write(b"bytes");
      assert!(!matches!(answered, Ok(1..=5)), "the premise: {answered:?}");
      assert!(log.lock().expect("the log").is_empty(), "nothing tapped");
    }
  }

  /// **A write plant changes what the sink and the tap see**, at the
  /// write's offset and keeping its length; a plant that panics is latched
  /// as the sink's panic, and nothing reaches the sink after it.
  #[test]
  fn a_write_plant_changes_what_the_sink_and_the_tap_see() {
    let (mut guarded, _, log) = tapped(io::Cursor::new(Vec::new()));
    guarded
      .tap_slot()
      .plant(Box::new(|at: u64, bytes: &mut [u8]| {
        if at == 0 {
          bytes[1] = b'Q';
        }
      }));
    guarded.write_all(b"abc").expect("write");
    guarded.write_all(b"de").expect("write");
    assert_eq!(
      *log.lock().expect("the log"),
      [took(0, b"aQc"), took(3, b"de")]
    );
    assert_eq!(guarded.into_inner().into_inner(), b"aQcde");

    let (mut guarded, latch, log) = tapped(io::Cursor::new(Vec::new()));
    guarded.tap_slot().plant(Box::new(|_: u64, _: &mut [u8]| {
      panic!("the plant exploded")
    }));
    assert!(quietly(|| guarded.write(b"abc")).is_err());
    assert_eq!(latch.message().as_deref(), Some("the plant exploded"));
    assert!(guarded.write(b"more").is_err());
    assert!(log.lock().expect("the log").is_empty(), "nothing tapped");
    assert!(guarded.into_inner().into_inner().is_empty());
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
