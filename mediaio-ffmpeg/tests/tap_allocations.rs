//! **B7 — the tap's road allocates nothing.** Its own test binary, so that
//! its allocator counts: every allocation through Rust's global allocator
//! is counted on the thread that makes it, and N packets, then 2N, are
//! written through an MP4 output whose guard is tapped. libavformat flushes
//! its write context after each packet (`flush_if_needed`, `mux.c` 426–434
//! in FFmpeg 9.0.1), so each packet reaches the sink, and the tap, at
//! least once; the count the packets make is the same for N and for 2N.
//!
//! **The road it measures is the build's.** The gate runs it with the
//! default features, which measure the production road alone; with
//! `test-support` on, the measured write also runs the write plant's
//! prelude — a clone of the slot's `Arc` and its lock, no plant set —
//! which allocates nothing either.

use std::{
  alloc::{GlobalAlloc, Layout, System},
  cell::Cell,
  io::{self, Seek, SeekFrom, Write},
  num::NonZeroI32,
  ptr::{addr_of_mut, write_unaligned},
};

use mediaio_ffmpeg::{
  CodecId, FfmpegBytes, Owned, OwnedAudioPacket,
  extras::AudioPacketExtra,
  limits::PacketLimits,
  output::{Output, OutputPacket, PacketTimes, StreamSpec},
  ticket::CodecTicket,
  writer_guard::SinkTap,
};
use mediaspine::Timebase;

/// Rust's global allocator, counting each allocation on the thread that
/// makes it.
struct Counting;

thread_local! {
  // A `const` initializer and no destructor: reading it allocates nothing,
  // so the allocator can.
  static ALLOCATIONS: Cell<u64> = const { Cell::new(0) };
}

fn count() {
  let _ = ALLOCATIONS.try_with(|count| count.set(count.get() + 1));
}

fn allocations() -> u64 {
  ALLOCATIONS.with(Cell::get)
}

// SAFETY: every call is forwarded to the system allocator with the
// caller's own arguments; counting touches a thread-local `Cell` only.
unsafe impl GlobalAlloc for Counting {
  unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
    count();
    // SAFETY: the caller's contract, forwarded.
    unsafe { System.alloc(layout) }
  }
  unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
    count();
    // SAFETY: the caller's contract, forwarded.
    unsafe { System.alloc_zeroed(layout) }
  }
  unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
    count();
    // SAFETY: the caller's contract, forwarded.
    unsafe { System.realloc(ptr, layout, new_size) }
  }
  unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
    // SAFETY: the caller's contract, forwarded.
    unsafe { System.dealloc(ptr, layout) }
  }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// A sink that keeps nothing and allocates nothing: it takes every byte and
/// follows every seek.
#[derive(Default)]
struct Nowhere {
  at: u64,
  end: u64,
}

impl Write for Nowhere {
  fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
    self.at += buf.len() as u64;
    self.end = self.end.max(self.at);
    Ok(buf.len())
  }
  fn flush(&mut self) -> io::Result<()> {
    Ok(())
  }
}

impl Seek for Nowhere {
  fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
    self.at = match pos {
      SeekFrom::Start(to) => to,
      SeekFrom::Current(by) => self.at.saturating_add_signed(by),
      SeekFrom::End(by) => self.end.saturating_add_signed(by),
    };
    Ok(self.at)
  }
}

/// A tap that counts its calls and the bytes in them, and allocates
/// nothing.
#[derive(Default)]
struct Counted {
  calls: u64,
  bytes: u64,
}

impl SinkTap for Counted {
  fn took(&mut self, _at: u64, bytes: &[u8]) {
    self.calls += 1;
    self.bytes += bytes.len() as u64;
  }
  fn lost(&mut self) {}
}

/// An AAC stream's ticket, at 44.1 kHz in stereo.
fn aac() -> CodecTicket {
  let config = [0x12_u8, 0x10];
  let mut parameters = ffmpeg_next::codec::Parameters::new();
  // SAFETY: `Parameters::new` allocated a live `AVCodecParameters` (it is
  // checked non-null first) that nothing else holds; the enums are written
  // as the integers they are stored as, the layout by libavutil's own, and
  // the extradata into an allocation of libavutil's, zero-padded.
  unsafe {
    let par = parameters.as_mut_ptr();
    assert!(!par.is_null(), "avcodec_parameters_alloc");
    write_unaligned(
      addr_of_mut!((*par).codec_type).cast::<i32>(),
      ffmpeg_next::ffi::AVMediaType::AVMEDIA_TYPE_AUDIO as i32,
    );
    write_unaligned(
      addr_of_mut!((*par).codec_id).cast::<i32>(),
      CodecId::AAC.raw(),
    );
    (*par).sample_rate = 44_100;
    ffmpeg_next::ffi::av_channel_layout_default(addr_of_mut!((*par).ch_layout), 2);
    let padding = ffmpeg_next::ffi::AV_INPUT_BUFFER_PADDING_SIZE as usize;
    let extradata = ffmpeg_next::ffi::av_mallocz(config.len() + padding).cast::<u8>();
    assert!(!extradata.is_null(), "av_mallocz");
    core::ptr::copy_nonoverlapping(config.as_ptr(), extradata, config.len());
    (*par).extradata = extradata;
    (*par).extradata_size = config.len() as i32;
  }
  CodecTicket::mirror(&parameters, 0, 1 << 20).expect("a ticket")
}

/// The allocations `packets` packets make on their way through a tapped
/// MP4 output, and the tap's count of its calls: the packets built, the
/// header written and the tap set first, outside the count.
fn counted_through_a_tapped_output(packets: usize) -> (u64, u64) {
  let ticket = aac();
  let millis = Timebase::new(1, NonZeroI32::new(1000).expect("non-zero"));
  let built: Vec<OwnedAudioPacket> = (0..packets)
    .map(|i| {
      OwnedAudioPacket::new(
        FfmpegBytes::try_copy_from_slice(&[0x21; 5][..4 + i % 2]).expect("a payload"),
        AudioPacketExtra::new(0),
      )
    })
    .collect();
  let mut output = Output::open(Nowhere::default(), "mp4").expect("open");
  output
    .add_stream(&StreamSpec::new(&ticket, millis))
    .expect("a stream");
  output.set_tap(Counted::default()).expect("the tap");
  output.write_header().expect("the header");
  let (before_calls, _) = output
    .with_tap(|tap: &mut Counted| (tap.calls, tap.bytes))
    .expect("the tap");
  let before = allocations();
  for (i, packet) in built.iter().enumerate() {
    let at = i as i64 * 20;
    output
      .write_packet::<Owned>(
        0,
        OutputPacket::Audio(packet),
        PacketTimes::new(Some(at), Some(at), 20),
        PacketLimits::default(),
      )
      .expect("a packet");
  }
  let made = allocations() - before;
  let calls = output
    .with_tap(|tap: &mut Counted| tap.calls)
    .expect("the tap")
    - before_calls;
  output.write_trailer().expect("the trailer");
  drop(output.into_sink().expect("the sink back"));
  (made, calls)
}

#[test]
fn b7_the_taps_road_allocates_nothing_per_write() {
  ffmpeg_next::init().expect("ffmpeg init");
  const N: usize = 500;
  let (once, calls_once) = counted_through_a_tapped_output(N);
  let (twice, calls_twice) = counted_through_a_tapped_output(2 * N);
  assert!(
    calls_once >= N as u64 && calls_twice >= 2 * N as u64,
    "the premise: each packet reached the tap ({calls_once}, {calls_twice})"
  );
  assert_eq!(
    once, twice,
    "the allocations of N packets and of 2N: none grows with the writes"
  );
}
