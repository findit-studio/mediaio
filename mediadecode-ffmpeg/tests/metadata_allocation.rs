//! **Nothing allocates after the budget charge.**
//!
//! A container's metadata values are attacker-sized, so the demuxer
//! measures one, charges it against a budget, and only then builds it.
//! That chain is worth nothing if the last step allocates again — and
//! it used to: the carrier was a `SmolStr`, whose constructor takes a
//! `&str` and copies into a fresh `Arc<str>` past 23 bytes. A second,
//! **infallible** allocation of an attacker-sized value, made while the
//! fallibly reserved buffer was still live, so failing it aborted the
//! process the budget existed to keep alive.
//!
//! The carrier is `smol_bytes::Utf8Bytes` now, and this file is the
//! proof rather than the claim: a counting global allocator watches the
//! conversion itself.
//!
//! The carrier lanes need no `ffmpeg` CLI and no fixture — they
//! exercise the conversion directly, which is the step under test. The
//! last lane opens a real container and watches the whole admission
//! transaction, which does need one, and returns early with the
//! corpus's own printed reason where it is absent.

use std::{
  alloc::{GlobalAlloc, Layout, System},
  cell::Cell,
};

mod support;

use mediadecode::demuxer::Demuxer;
use mediadecode_ffmpeg::{DemuxError, DemuxLimits, FfmpegOwnedDemuxer};
use smol_bytes::{INLINE_CAP, Utf8Bytes};

// **Thread-local, not global, and that is the whole trick.**
//
// A `#[global_allocator]` sees the entire process, and the test
// harness allocates on its own threads while a lane is running — so a
// process-wide counter measures whoever happened to be busy, not the
// conversion under test. This file learned that the hard way: the same
// assertion answered 0, then 1, then 3 across three CI runs.
//
// Counting per thread makes the window exact: only the thread inside
// `measured` contributes, and everything the harness does elsewhere is
// invisible. `Cell` in a `const`-initialised `thread_local!` neither
// allocates nor registers a destructor, so the instrument cannot
// perturb what it measures.
thread_local! {
  static WATCHING: Cell<bool> = const { Cell::new(false) };
  static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
  static ALLOCATED_BYTES: Cell<usize> = const { Cell::new(0) };
  static LARGEST: Cell<usize> = const { Cell::new(0) };
}

struct Counting;

fn record(size: usize) {
  // `try_with`, because a thread tearing down has already destroyed
  // its locals and must not be made to resurrect them.
  let watching = WATCHING.try_with(Cell::get).unwrap_or(false);
  if watching {
    let _ = ALLOCATIONS.try_with(|c| c.set(c.get() + 1));
    let _ = ALLOCATED_BYTES.try_with(|c| c.set(c.get() + size));
    let _ = LARGEST.try_with(|c| c.set(c.get().max(size)));
  }
}

// SAFETY: every method forwards to `System`, which is a correct
// allocator; the bookkeeping reads and writes `Cell`s in a
// `const`-initialised thread-local, which allocates nothing and so
// cannot recurse into this allocator.
unsafe impl GlobalAlloc for Counting {
  unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
    record(layout.size());
    unsafe { System.alloc(layout) }
  }
  unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
    unsafe { System.dealloc(ptr, layout) }
  }
  unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
    record(new_size);
    unsafe { System.realloc(ptr, layout, new_size) }
  }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// Runs `body` with this thread's allocations counted, and answers
/// `(value, allocations, bytes)`.
fn measured<T>(body: impl FnOnce() -> T) -> (T, usize, usize) {
  let (out, allocations, bytes, _) = measured_with_largest(body);
  (out, allocations, bytes)
}

/// [`measured`], and the size of the single largest allocation.
fn measured_with_largest<T>(body: impl FnOnce() -> T) -> (T, usize, usize, usize) {
  ALLOCATIONS.set(0);
  ALLOCATED_BYTES.set(0);
  LARGEST.set(0);
  WATCHING.set(true);
  let out = body();
  WATCHING.set(false);
  (out, ALLOCATIONS.get(), ALLOCATED_BYTES.get(), LARGEST.get())
}

/// Builds the buffer exactly as `demuxer::lossy_text` does: one
/// fallible, exact reservation, filled to precisely that length.
///
/// Built **outside** the measured window on purpose: the reservation
/// is the allocation the budget already charged for, and what is under
/// test is whether anything happens *after* it.
fn charged_buffer(len: usize) -> String {
  let mut buffer = String::new();
  buffer
    .try_reserve_exact(len)
    .expect("the test's own reservation");
  buffer.extend(std::iter::repeat_n('x', len));
  assert_eq!(buffer.len(), len);
  buffer
}

/// The largest allocation `bytes` can make while *moving* a buffer: a
/// fixed reference-count header, the same size whatever the value is.
///
/// It is what separates the two claims this file makes. "Nothing is
/// copied" is absolute. "Nothing is allocated" holds whenever the
/// vector's length equals its capacity — `Bytes::from(Vec<u8>)` takes
/// the `into_boxed_slice` road there and only stashes the pointer —
/// and where an allocator hands back more capacity than was asked for,
/// what remains is this header and nothing that scales with the value.
const HEADER_CEILING: usize = 64;

/// **A value past the inline window moves; it is not copied.**
///
/// 65,535 bytes is the largest single metadata value the reader
/// admits, so this is the worst case the budget ever charges for.
#[test]
fn a_large_value_reaches_the_carrier_without_allocating() {
  let buffer = charged_buffer(65_535);
  let (text, allocations, bytes) = measured(|| Utf8Bytes::from(buffer));

  assert_eq!(text.as_str().len(), 65_535);
  assert_eq!(
    allocations, 0,
    "the fallibly reserved buffer is moved into the carrier, not copied — {bytes} bytes were \
     allocated after the charge",
  );
}

/// The same, one byte past the inline window, where the heap road
/// begins — and where the header above can appear.
///
/// The assertion is on **bytes**, not on a count, because that is the
/// property the budget cares about: whatever happens here does not
/// scale with the value. One byte over the inline window and a full
/// 64 KiB value must cost the same.
#[test]
fn the_first_heap_sized_value_costs_no_copy() {
  let buffer = charged_buffer(INLINE_CAP + 1);
  let (text, _, bytes) = measured(|| Utf8Bytes::from(buffer));

  assert_eq!(text.as_str().len(), INLINE_CAP + 1);
  assert!(
    bytes <= HEADER_CEILING,
    "the heap road starts here; {bytes} bytes were allocated, which is more than a header and \
     therefore a copy",
  );
}

/// And a value inside the inline window never touches the heap at all.
#[test]
fn a_small_value_is_stored_inline() {
  let buffer = charged_buffer(INLINE_CAP);
  let (text, allocations, _) = measured(|| Utf8Bytes::from(buffer));

  assert_eq!(text.as_str().len(), INLINE_CAP);
  assert_eq!(allocations, 0, "inline storage, so no heap at all");
}

/// **The carrier this replaced, measured for contrast.**
///
/// Not a regression guard — `SmolStr` is gone from the crate — but the
/// number that made the finding real: the same value, through the old
/// carrier, allocated a second time, infallibly, at full size. Written
/// with `Arc<str>` directly because that is precisely what
/// `SmolStr::new` did past its inline window.
#[test]
fn the_copying_carrier_allocated_a_second_time() {
  let buffer = charged_buffer(65_535);
  let (copy, allocations, bytes) = measured(|| std::sync::Arc::<str>::from(buffer.as_str()));

  assert_eq!(copy.len(), 65_535);
  // The count is not the point and is not asserted — a copy may take
  // one allocation or several. The **size** is the point: the old
  // carrier allocated the whole attacker-sized value a second time,
  // three orders of magnitude past `HEADER_CEILING`, which is the
  // difference this file exists to show.
  assert!(allocations >= 1);
  assert!(
    bytes >= 65_535,
    "only {bytes} bytes were allocated, so this is no longer a full copy and the contrast this \
     lane draws has gone stale",
  );
}

/// **The whole admission transaction, watched.**
///
/// Everything between admission and a successful `open` allocates on
/// this thread: the track table, each stream's codec-ticket mirror
/// (extradata, coded side data, the channel map), the attachment
/// queue and its owned carriers, the chapter table, and every retained
/// metadata value. The review that prompted this lane found those
/// copies infallible — a container whose footprint the caller's
/// ceilings had already *admitted* could still abort a safe open when
/// the allocator declined.
///
/// Two properties, and neither is a byte count that would rot:
///
/// 1. **Nothing is copied twice.** Every payload in this fixture is
///    small — an H.264 SPS/PPS, a 27-byte attachment, a handful of
///    metadata strings — so a single allocation anywhere near the
///    admitted per-stream ceiling would mean a payload had been
///    staged and then copied again. `LARGEST` is what catches that,
///    and it is the shape of the defect rather than its size.
/// 2. **The total stays in proportion to the file**, not to the
///    ceilings. 16 MiB per stream and 64 MiB aggregate are what the
///    defaults *admit*; a correct open of this fixture spends a tiny
///    fraction of it, and a regression that reserved against the
///    ceiling rather than the file would show here immediately.
#[test]
fn a_budgeted_open_makes_no_oversized_or_duplicated_allocation() {
  let Some(corpus) = support::Corpus::new() else {
    return;
  };
  let path = corpus.multi_track_mkv();

  let (demuxer, allocations, bytes, largest) =
    measured_with_largest(|| FfmpegOwnedDemuxer::open(&path));
  let demuxer = demuxer.expect("the fixture opens");

  // The open really did the work being measured.
  assert_eq!(demuxer.tracks().len(), 4);
  assert!(
    allocations > 0,
    "an open that allocated nothing was not an open"
  );

  assert!(
    largest < 256 * 1024,
    "the largest single allocation during the open was {largest} bytes; every payload in this \
     fixture is tiny, so a block that size means one was staged and copied again",
  );
  assert!(
    bytes < 4 * 1024 * 1024,
    "the open allocated {bytes} bytes in total for a fixture whose payloads are a few kilobytes \
     — a reservation made against the ceiling rather than against the file",
  );
}

/// **A refused chapter table costs nothing, because the refusal comes
/// first.**
///
/// The ordering defect this pins: `from_input` built the *whole* track
/// table before `build_chapters` ran its cheap count check — every
/// attachment carrier (up to 256 MiB by default), every codec-parameter
/// mirror, every retained metadata value, one `Arc` per row — and only
/// then discovered that the file's chapter count made the open
/// impossible. A file certain to be refused could first be made to cost
/// hundreds of megabytes, and repeating the open repeated the bill.
///
/// Both checks were correct; their order was the whole defect, which is
/// why a test that only asserts the refusal cannot see it. The
/// instrument has to be the allocator, and the property is
/// **comparative**: the same file, refused and accepted, and the
/// refusal must not have paid for the acceptance's table.
///
/// `admit_chapters` allocates nothing at all — the title measurement
/// borrows libavutil's own buffer — so what the refused open spends is
/// only what opening the container costs before either table exists.
#[test]
fn a_refused_chapter_table_is_refused_before_the_tracks_are_built() {
  let Some(corpus) = support::Corpus::new() else {
    return;
  };
  let path = corpus.chaptered_mkv();

  let (refused, _, refused_bytes, refused_largest) = measured_with_largest(|| {
    FfmpegOwnedDemuxer::open_with(&path, DemuxLimits::new().with_max_chapters(1))
  });
  assert!(
    matches!(refused, Err(DemuxError::TooManyChapters(_))),
    "the ceiling must refuse this file",
  );

  let (accepted, _, accepted_bytes, _) = measured_with_largest(|| FfmpegOwnedDemuxer::open(&path));
  let accepted = accepted.expect("the same file opens under the default ceiling");
  assert_eq!(accepted.chapters().len(), 2);
  assert!(
    accepted_bytes > 0,
    "an open that allocated nothing was not an open",
  );

  assert!(
    refused_bytes * 2 < accepted_bytes,
    "the refused open spent {refused_bytes} bytes against the accepted open's {accepted_bytes}: \
     the refusal is paying for a table it was always going to throw away",
  );
  assert!(
    refused_largest < 64 * 1024,
    "the largest single allocation before the refusal was {refused_largest} bytes — a carrier, \
     which means a payload was copied for a file that was never going to open",
  );
}

/// **Over-budget stream metadata on the *last* stream is refused
/// before the *first* stream is materialised.**
///
/// The same ordering defect the chapter pre-pass was added to prevent,
/// found one table over. The metadata charge lived inside
/// `build_tracks`'s loop, so a value crossing the aggregate budget on
/// the last stream was refused only after every earlier stream's
/// parameter clone and attachment carrier had been retained and this
/// stream's own codec ticket copied. A file certain to be refused could
/// be made to cost the whole aggregate — up to 64 MiB of parameter
/// mirrors and 256 MiB of attachment carriers by default — on every
/// open, and the open could be repeated.
///
/// The fixture has exactly that shape by accident of how it is built:
/// its video, audio and subtitle streams carry almost no metadata, and
/// the **attachment stream, which is last, carries both `filename` and
/// `mimetype`** — thirty-odd bytes that are the bulk of the file's
/// total. Setting the ceiling one byte under that total therefore
/// refuses on the final stream, which is the only place this defect is
/// visible.
///
/// The instrument has to be the allocator again: the refusal itself is
/// unchanged by the fix, and only what the refusal *cost* tells the two
/// orderings apart.
#[test]
fn over_budget_metadata_on_the_last_stream_costs_no_track_material() {
  let Some(corpus) = support::Corpus::new() else {
    return;
  };
  let path = corpus.multi_track_mkv();

  // What the file actually retains, measured from a good open: the
  // ceiling below is derived from the fixture rather than guessed, so
  // it keeps meaning what it means if the fixture changes.
  let (accepted, _, accepted_bytes, _) = measured_with_largest(|| FfmpegOwnedDemuxer::open(&path));
  let accepted = accepted.expect("the fixture opens");
  let retained: usize = accepted
    .tracks()
    .iter()
    .map(|track| {
      let len = |value: Option<&Utf8Bytes>| value.map_or(0, |text| text.len());
      len(track.filename()) + len(track.mime_type()) + len(track.language())
    })
    .sum();
  assert!(
    retained > 0,
    "the fixture must retain some stream metadata for this lane to mean anything",
  );

  let (refused, _, refused_bytes, refused_largest) = measured_with_largest(|| {
    FfmpegOwnedDemuxer::open_with(
      &path,
      DemuxLimits::new().with_max_total_stream_metadata_bytes(retained - 1),
    )
  });
  // Matched rather than formatted: the demuxer is deliberately not
  // `Debug`, so only the error side can be printed.
  match refused {
    Err(DemuxError::TrackMetadataBudgetExhausted(_)) => {}
    Err(other) => panic!("the budget must be what refuses this file, got {other:?}"),
    Ok(_) => panic!("one byte under what the file retains must be refused"),
  }

  assert!(
    refused_bytes * 2 < accepted_bytes,
    "the refused open spent {refused_bytes} bytes against the accepted open's {accepted_bytes}: \
     the refusal is paying for a table it was always going to throw away",
  );
  assert!(
    refused_largest < 64 * 1024,
    "the largest single allocation before the refusal was {refused_largest} bytes — an \
     attachment carrier or a parameter mirror, bought for a file that was never going to open",
  );
}

/// **Reading FFmpeg's static descriptor tables allocates nothing** —
/// however long the name is.
///
/// `table_text` used to hand back an owned `Utf8Bytes` built from the
/// borrowed bytes, which **copies** past `smol_bytes::INLINE_CAP`. And
/// FFmpeg's own tables reach past it — the SER demuxer's long name is
/// sixty-five bytes — so opening an ordinary container, or asking a
/// codec for its description, allocated infallibly to duplicate a
/// string the process already owns for its whole life.
///
/// The reader borrows now and the fields store the borrow with
/// `Utf8Bytes::from_static`. This walks a wide span of codec ids so the
/// long descriptions are certainly included, and asserts the whole scan
/// costs nothing — plus that it really did meet a name past the inline
/// window, so the lane cannot pass by never reaching the case it is
/// about.
#[test]
fn reading_the_descriptor_tables_allocates_nothing() {
  use mediadecode_ffmpeg::CodecId;

  let ((longest, seen), allocations, bytes) = measured(|| {
    let mut longest = 0usize;
    let mut seen = 0usize;
    for raw in 0..1200i32 {
      let id = CodecId::from_raw(raw);
      if let Some(name) = id.name() {
        longest = longest.max(name.len());
        seen += 1;
      }
      if let Some(long) = id.long_name() {
        longest = longest.max(long.len());
      }
    }
    (longest, seen)
  });

  assert!(
    seen > 100,
    "the descriptor table should not be nearly empty"
  );
  assert!(
    longest > INLINE_CAP,
    "the scan never met a name past the {INLINE_CAP}-byte window, so it cannot speak to the \
         case this lane is about; it saw at most {longest} bytes",
  );
  assert_eq!(
    allocations, 0,
    "reading names out of a static table must not copy them; the scan allocated {bytes} bytes",
  );
}

/// **Reporting an unsupported pixel format allocates nothing.**
///
/// The payload used to carry the vocabulary's `PixelFormat`; mediaframe
/// 0.11 widened that type's text arm, so the error grew, and boxing it
/// to shrink the error put an **infallible allocation on the refusal
/// path** — a container-selected unsupported format could abort the
/// process precisely while the converter was trying to report it.
///
/// It is a tag now: the raw id, and libavutil's own name **borrowed**
/// from the static descriptor table that outlives the process. The type
/// has no field that can allocate, which is a stronger guarantee than
/// this lane — but the lane covers the half a type cannot state, that
/// reading the name out of FFmpeg's table does not copy it.
#[test]
fn reporting_an_unsupported_pixel_format_allocates_nothing() {
  use mediadecode_ffmpeg::convert::UnsupportedPixelFormat;

  // Both arms: a format libavutil names, and one it does not.
  let (named, allocations, bytes) = measured(|| UnsupportedPixelFormat::new(0, Some("yuv420p")));
  assert_eq!(
    allocations, 0,
    "naming a refused format must not need the allocator; it allocated {bytes} bytes",
  );
  assert_eq!(named.raw(), 0);
  assert_eq!(named.name(), Some("yuv420p"));

  let (unnamed, allocations, _) = measured(|| UnsupportedPixelFormat::new(-99_999, None));
  assert_eq!(allocations, 0);
  assert!(unnamed.name().is_none());

  // The name is a `&'static str` borrowed out of libavutil's static
  // descriptor table, so the reader that produces it cannot copy
  // either — that half is guaranteed by the return type rather than
  // by this lane, and its own unit lanes cover the lookup.
}

/// **Naming a length allocates nothing**, which is what lets the
/// owned resampler's `commit` be infallible.
///
/// The shape behind this lane is a data-loss regression rather than an
/// abort. The owned lane used to allocate its carrier in `commit` —
/// *after* `swr` had consumed the input. Once that allocation became
/// fallible the failure turned into `None`, `finish_output` had already
/// advanced `next_pts`, the send arm still answered `Accepted`, and the
/// converted samples were simply gone: no error, no retry.
///
/// The allocation happens in `reserve` now, before `swr` runs, and
/// `commit` returns a carrier rather than an `Option` — so the
/// compiler, not a test, is what rules the loss out. What a test can
/// still add is the other half of that pair: that producing a carrier
/// costs exactly one allocation and nothing happens afterwards, which
/// is the property `commit` relies on when it promises not to fail.
#[test]
fn producing_a_carrier_costs_one_allocation_and_nothing_after_it() {
  let (carrier, allocations, _) = measured(|| {
    mediadecode_ffmpeg::FfmpegBytes::try_copy_from_slice(&[7u8; 4096])
      .expect("a four-kibibyte carrier")
  });
  assert_eq!(carrier.len(), 4096);
  assert_eq!(
    allocations, 1,
    "one allocation for the payload, and nothing after it — no staging \
     buffer, and no second copy into a refcount",
  );
}
