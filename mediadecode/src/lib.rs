#![doc = include_str!("../README.md")]
#![cfg_attr(not(feature = "std"), no_std)]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![cfg_attr(docsrs, allow(unused_attributes))]
#![deny(missing_docs)]
#![forbid(unsafe_code)]
#![allow(clippy::type_complexity)]

// Workspace pattern (mirrors mediatime / colconv / scenesdetect) — alias
// `alloc` as `std` so `std::vec::Vec` etc. resolves in alloc-only builds.
// `unused_extern_crates` is suppressed because the public API currently
// uses only `core::` paths.
#[cfg(all(not(feature = "std"), feature = "alloc"))]
#[allow(unused_extern_crates)]
extern crate alloc as std;

#[cfg(feature = "std")]
extern crate std;

pub mod decoder;
pub mod demuxer;

#[cfg(feature = "future")]
#[cfg_attr(docsrs, doc(cfg(feature = "future")))]
pub mod future;

// The vocabulary — frames, packets, subtitles, the push rhythm, the
// adapter traits, the resampler face and the `mediaframe` re-exports —
// is `mediaspine`'s. Each module is re-exported under the name it had
// here, so every `mediadecode::` path still resolves, to the same item
// a `mediaspine::` path names. (`demuxer` is this crate's own module: it
// re-exports the vocabulary's demux types beside the `Demuxer` face.)
pub use mediaspine::{
  adapter, cfa, color, frame, nal, packet, pixel_format, resampler, rhythm, subtitle,
};

pub use mediaspine::{PixelFormat, Received, Sent};

// Re-export the time primitives so consumers don't have to add a
// separate `mediatime` dependency. Four names sit at the root: the three
// primitives, and `InvertedRange`, because a `TimeRange` moves one end
// only by a checked method, which refuses an end before the start by that
// name. The rest of `mediatime` is reachable as `mediadecode::mediatime`,
// so every type the primitives' own methods take or answer (`Rounding`,
// `SignedDuration`, `Duration`, `Rate`, `ParseSecondsError`, ...) has a
// path through this crate as well.
pub use mediaspine::{InvertedRange, TimeRange, Timebase, Timestamp};

pub use mediaspine::mediatime;

pub use mediaspine;
