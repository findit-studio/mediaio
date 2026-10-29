//! WebCodecs adapter for the [`mediadecode`] abstraction layer.
//!
//! Implements [`mediadecode::adapter::VideoAdapter`] and
//! [`mediadecode::adapter::AudioAdapter`] for the [`WebCodecs`]
//! zero-sized type, plus the matching push-style decoder traits
//! from
//! [`mediadecode::future::local`](mediadecode::future::local),
//! on top of the browser's
//! [WebCodecs API](https://developer.mozilla.org/en-US/docs/Web/API/WebCodecs_API)
//! via [`web-sys`](https://crates.io/crates/web-sys).
//!
//! This crate is **`wasm32`-only**. On non-`wasm32` targets it
//! compiles to an empty stub so workspace builds and `cargo check`
//! still succeed — every public item is gated behind
//! `#[cfg(target_arch = "wasm32")]`.
//!
//! See `docs/superpowers/specs/2026-05-09-webcodecs-design.md`
//! for the full design.
//!
//! # Async-only, `!Send`
//!
//! WebCodecs is fundamentally async: `VideoDecoder.decode`
//! returns immediately and decoded frames arrive on the `output`
//! callback registered at construction. Every value held during
//! a decode is a `JsValue` (or `Closure` / `Promise`), which is
//! `!Send` by design — the browser's event loop is
//! single-threaded anyway. This adapter therefore implements
//! only the [`mediadecode::future::local`] trait variants (no
//! `Send` bound, native `async fn`); there is no sync
//! implementation, and no
//! [`mediadecode::future::send`] implementation.
//!
//! Internally the decoder still runs a frame queue: the `output`
//! callback `spawn_local`s a copy task that pulls bytes out of
//! the JS-side `VideoFrame` (only available via async `copyTo`),
//! pushes the result onto the queue, and wakes the receive
//! future. `receive_frame` is a `poll_fn` that drains the queue,
//! answers [`Received::Ended`](mediadecode::Received) once
//! `send_eof` has resolved and the drain is empty, or registers a
//! waker and yields.
//!
//! # Subtitles
//!
//! WebCodecs has no subtitle surface. This crate intentionally
//! does **not** implement
//! [`SubtitleAdapter`](mediadecode::adapter::SubtitleAdapter).
//!
//! # Still images: a door, not a gap
//!
//! `mediadecode` 0.9 added a fourth household —
//! [`ImageFrame`](mediadecode::frame::ImageFrame) and the one-shot
//! [`ImageDecoder`](mediadecode::decoder::ImageDecoder) seam — for
//! cover art and other stills. This crate does **not** implement it
//! yet, and the absence is a schedule rather than a judgement: the
//! browser's answer to "decode these bytes into a picture" is
//! [`createImageBitmap`](https://developer.mozilla.org/en-US/docs/Web/API/createImageBitmap),
//! which is not part of the WebCodecs surface this crate wraps and
//! carries its own async shape and its own pixel-readback problem (an
//! `ImageBitmap` has no `copyTo`; the bytes come back through a canvas
//! or through `VideoFrame::new(ImageBitmap)`). That is separate work
//! with separate failure modes, and half of it would have shipped an
//! [`ImageAdapter`](mediadecode::adapter::ImageAdapter) whose fields
//! nothing could fill.
//!
//! The seam this crate will implement when that work happens is
//! [`mediadecode::future::local::ImageDecoder`], the `async` mirror —
//! `createImageBitmap` returns a `Promise`, so the sync face was never
//! the one on offer here.
//!
//! # The carrier
//!
//! [`WebCodecsBuffer`] satisfies `mediadecode` 0.9's
//! [carrier amputation contract][law], and always did: it is an
//! `Arc<Vec<u8>>` view over bytes `copyTo` has already handed to Rust,
//! so nothing in a delivered frame reaches back into a JS handle. The
//! FFmpeg adapter had to be rebuilt for that release; this one had to
//! be checked, and it passed unchanged.
//!
//! [law]: mediadecode::adapter#the-carrier-amputation-contract

#![cfg_attr(docsrs, feature(doc_cfg))]
#![cfg_attr(docsrs, allow(unused_attributes))]
#![deny(missing_docs)]
#![allow(clippy::type_complexity)]

// `web-sys` gates the WebCodecs APIs (`VideoDecoder`,
// `AudioDecoder`, `VideoFrame`, `AudioData`, …) behind
// `--cfg web_sys_unstable_apis` because the WebIDL is not yet
// stable across all browsers. Without it, every WebCodecs type
// disappears and this crate fails with a confusing wall of
// "cannot find type X in `web_sys`" errors. Surface a clear
// compile-time message instead.
#[cfg(all(target_arch = "wasm32", not(web_sys_unstable_apis)))]
compile_error!(
  "mediadecode-webcodecs requires `--cfg web_sys_unstable_apis`. \
Add the following to your project's `.cargo/config.toml`:\n\n  \
[target.wasm32-unknown-unknown]\n  \
rustflags = [\"--cfg=web_sys_unstable_apis\"]\n\n\
or set `RUSTFLAGS=\"--cfg=web_sys_unstable_apis\"` in your environment / CI. \
This is a `web-sys` constraint — the WebCodecs WebIDL is still marked \
unstable upstream."
);

#[cfg(target_arch = "wasm32")]
mod adapter;
#[cfg(target_arch = "wasm32")]
mod audio;
#[cfg(target_arch = "wasm32")]
mod boundary;
#[cfg(target_arch = "wasm32")]
mod buffer;
#[cfg(target_arch = "wasm32")]
mod codec_id;
#[cfg(target_arch = "wasm32")]
pub mod codec_string;
#[cfg(target_arch = "wasm32")]
mod dispatch;
#[cfg(target_arch = "wasm32")]
mod error;
#[cfg(target_arch = "wasm32")]
mod extras;
#[cfg(target_arch = "wasm32")]
mod sample_format;
#[cfg(target_arch = "wasm32")]
mod state;
#[cfg(target_arch = "wasm32")]
mod video;

#[cfg(target_arch = "wasm32")]
pub use adapter::WebCodecs;
#[cfg(target_arch = "wasm32")]
pub use audio::WebCodecsAudioStreamDecoder;
#[cfg(target_arch = "wasm32")]
pub use boundary::{empty_audio_frame, empty_video_frame};
#[cfg(target_arch = "wasm32")]
pub use buffer::WebCodecsBuffer;
#[cfg(target_arch = "wasm32")]
pub use codec_id::{AudioCodecId, VideoCodecId};
#[cfg(target_arch = "wasm32")]
pub use error::{AudioDecodeError, Error, VideoDecodeError};
#[cfg(target_arch = "wasm32")]
pub use extras::{AudioFrameExtra, AudioPacketExtra, VideoFrameExtra, VideoPacketExtra};
#[cfg(target_arch = "wasm32")]
pub use sample_format::SampleFormat;
#[cfg(target_arch = "wasm32")]
pub use video::WebCodecsVideoStreamDecoder;
