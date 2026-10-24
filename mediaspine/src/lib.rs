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

pub mod adapter;
pub mod cfa;
pub mod color;
pub mod demuxer;
pub mod frame;
pub mod hdr;
pub mod nal;
pub mod packet;
pub mod pixel_format;
pub mod resampler;
pub mod rhythm;
pub mod subtitle;
pub mod track;

pub use pixel_format::PixelFormat;
pub use rhythm::{Received, Sent};

// Re-export the time primitives so consumers don't have to add a
// separate `mediatime` dependency. Four names sit at the root: the three
// primitives, and `InvertedRange`, because a `TimeRange` moves one end
// only by a checked method, which refuses an end before the start by that
// name. The rest of `mediatime` is reachable as `mediaspine::mediatime`,
// so every type the primitives' own methods take or answer (`Rounding`,
// `SignedDuration`, `Duration`, `Rate`, `ParseSecondsError`, ...) has a
// path through this crate as well.
pub use mediatime::{InvertedRange, TimeRange, Timebase, Timestamp};

pub use mediatime;
