#![doc = include_str!("../README.md")]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![cfg_attr(docsrs, allow(unused_attributes))]
#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub mod catalogue;
pub mod mux;

pub use catalogue::{Admitted, CATALOGUE, EncoderRefusal, Entry, License, encoder_by_name, lookup};
pub use mux::{
  AutomaticFilter, CarrierMuxer, CodedSideDataReadBack, ContainerName, FfmpegMuxRefusal,
  FfmpegMuxer, FfmpegOwnedMuxer, LayoutReadBack, MuxFault, MuxOpenError, MuxOptions, MuxReport,
  MuxStage, PayloadRule, Rewrite, SideDataFate, Uncarried,
};
// What a fault and a refusal name, from the base's muxing context. That
// module is hidden from the base's documentation, but these four are this
// crate's public face: the FFmpeg crates move in lockstep, and a change to
// them is a change to this crate's API, the hidden marking there
// notwithstanding.
pub use mediaio_ffmpeg::output::{OutputCall, OutputError, OutputPhase, OutputText};

// The faces this adapter implements, and the vocabulary they speak.
pub use mediaencode;
