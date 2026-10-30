#![doc = include_str!("../README.md")]
#![cfg_attr(not(feature = "std"), no_std)]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![cfg_attr(docsrs, allow(unused_attributes))]
#![deny(missing_docs)]
#![forbid(unsafe_code)]
#![allow(clippy::type_complexity)]

#[cfg(feature = "std")]
extern crate std;

pub mod encoder;
pub mod mux;
pub mod settings;

pub use encoder::{AudioEncoder, EncodeError, EncodeRefusal, Flushed, VideoEncoder};
pub use mux::{MuxError, MuxRefusal, Muxer, check_decode_order, check_presentation_order};
pub use settings::{
  AudioEncoderSettings, AudioParams, EncoderSettings, Gop, Level, Profile, RateControl,
  VideoEncoderSettings, VideoParams,
};

// The push rhythm the encoders answer is the vocabulary's own — the same
// `Sent` and `Received` every decoder answers — and the vocabulary is
// reachable through this crate for every type the faces name.
pub use mediaspine::{self, Received, Sent};
