//! The muxing context: libavformat's `AVFormatContext` for output, owned
//! in Rust and writing through the [writer guard](crate::writer_guard)
//! into a caller's sink.
//!
//! [`Output`] is the one place this family hands libavformat a packet to
//! write, and every pointer it gives libavformat is one it owns: the
//! context, the I/O context over the guarded sink, the streams and
//! chapters it adds, and packets it builds from the portable carriers on
//! the **copy road** ([`BodyRoute::Copy`]) — `av_interleaved_write_frame`
//! keeps a packet in its interleaving queue past the call, so nothing a
//! caller holds is ever lent to it. Nothing here takes a pointer from a
//! caller, so nothing here is an `unsafe fn`: what libavformat is owed is
//! established inside, and each `unsafe` block says how.
//!
//! # A context's life
//!
//! [`Output::open`] names the container; streams and chapters are added
//! while the context is being built; [`Output::write_header`] ends that
//! and starts the writing; packets are written; [`Output::write_trailer`]
//! completes the file; [`Output::into_sink`] frees the context and hands
//! the sink back. libavformat leaves a call out of that order undefined
//! — `av_write_trailer` twice runs a muxer's trailer over state its first
//! run freed — so the order is kept here, at run time, and a call out of
//! it is refused by name ([`OutputError::OutOfOrder`]).
//!
//! # The sink
//!
//! libavformat drives the sink from `extern "C"` callbacks, so every call
//! into it runs under the writer guard: a panic is caught there, latched,
//! and reported to libavformat as an I/O error, and every FFmpeg error
//! this context reports is checked against the latch first, so a panic is
//! named as one ([`OutputError::SinkPanic`]) rather than as the error C
//! folded it into. A panicked sink is never called again.
//!
//! A write the sink fails is kept by the I/O context, which writes
//! nothing after it (`writeout`, `libavformat/aviobuf.c` 136–166 in FFmpeg
//! 9.0.1). libavformat answers it as its own error where it reads it — a
//! packet's write and the trailer do (`mux.c` 751–752, 1270–1272) — and
//! answers success where it does not: the header flushes its bytes only
//! after it has read it (`mux.c` 482–486). Every call that reached
//! libavformat reads it once libavformat returns, and names it the sink's
//! ([`OutputError::SinkWrite`]), and [`Output::into_sink`] reads it once
//! more after its last flush — so a sink missing bytes is never handed
//! back as a whole one, nor its failure named libavformat's.
//!
//! A seek the sink fails is kept by the guard, which libavformat does not
//! do ([`SeekRecord`](crate::writer_guard::SeekRecord)): a muxer going
//! back to patch a header may ignore the answer and write on, into the
//! wrong place. The guard writes nothing after it, and every call that
//! reached libavformat names it once libavformat returns, whatever
//! libavformat answered ([`OutputError::SinkSeek`]) — so a container
//! whose patches went missing never ends in `Ok`.
//!
//! **The order, at every call that reached libavformat:** a panic first,
//! then a failed seek, then a failed write, then libavformat's own error.
//! The guard refuses every write after a panic or a failed seek, and the
//! I/O context keeps that refusal as its failed write, so the cause is
//! named ahead of its echo. On the containers this family writes,
//! nothing but the sink's write sets the I/O context's error: the one
//! other writer of it, an allocation `avio_vprintf` fails (`aviobuf.c`
//! 1185–1203), is reached on the sink's context only by muxers that print
//! into it — MP4's ISMV manifest (`movenc.c` 5457–5496) and text muxers,
//! none of them Matroska's or MP4's.
//!
//! # Ownership and lifetime
//!
//! Every allocation an [`Output`] makes or hands libavformat, who owns it,
//! and what frees it:
//!
//! | what | allocated by | owned by | freed by |
//! |---|---|---|---|
//! | the `AVFormatContext` (`ctx`) | `avformat_alloc_output_context2`, in [`Output::open`] | the `Output`, alone (`NonNull`) | `avformat_free_context`, exactly once: in `Drop`, or in [`Output::into_sink`], which holds `self` in `ManuallyDrop` so `Drop` never runs after it. Always before the I/O context: a muxer's `deinit` may still reach `pb` |
//! | the rebuilt `AVCodecParameters` a stream's parameters are copied from | `avcodec_parameters_alloc` and its copies, inside [`CodecTicket::rebuild`] (null-checked there) | this crate, for the length of [`Output::add_stream`] (the ticket's `Parameters`) | `avcodec_parameters_free`, in `Parameters`' `Drop`: just after the copy, or on each early return before it |
//! | each stream, its `codecpar`, its metadata | `avformat_new_stream`; `avcodec_parameters_copy` (extradata, side data, channel map); `av_dict_set` (a copy of each key and value) | the context | `avformat_free_context` |
//! | a stream's replaced extradata | `av_mallocz`, padded, in `replace_extradata`; the copied extradata it replaces is freed by `av_freep` first | the stream's `codecpar` | `avformat_free_context` |
//! | a chapter and its title | `av_mallocz` (zeroed, as `avpriv_new_chapter` makes one) and `av_dict_set`, in [`Output::add_chapter`] | this crate until `av_dynarray_add_nofree` takes it; then the context | before that, `free_chapter` (`av_dict_free`, `av_free`) on each failure; after, `avformat_free_context`, which frees each chapter and the array |
//! | the `AVIOContext` and its buffer | `StreamIo::from_write_seek` (ffmpeg-next): `av_mallocz` (32 KiB), `avio_alloc_context` | the `StreamIo`, held in `ManuallyDrop` by the `Output`; `ctx.pb` borrows it, and `AVFMT_FLAG_CUSTOM_IO` keeps libavformat from closing or freeing it | `StreamIo`'s `Drop` (`avio_flush`, the sink's flush through the guard, `av_freep` of the buffer, `avio_context_free`), dropped after the context; or `StreamIo::into_inner` in [`Output::into_sink`] |
//! | the opaque: the boxed [`GuardedWriter`] over the caller's sink | `Box::into_raw`, inside `StreamIo` | the `StreamIo` | `Box::from_raw` in `StreamIo`'s `Drop`, dropping the sink; or in `into_inner`, which hands the sink back |
//! | the panic latch and the seek record | `GuardedWriter::new` | shared by the guard and the `Output` (`Arc`: two owners) | the last of the two |
//! | a packet | the boundary's builders on the copy road ([`BodyRoute::Copy`]): `av_new_packet` and the payload copied, `av_packet_new_side_data` for each entry | this crate, until `av_interleaved_write_frame` takes its reference into the interleaving queue, which leaves the packet blank, even on error | the blank `Packet` drops; libavformat frees the queued reference when it writes it, at the trailer, or in `avformat_free_context` |
//!
//! **Every road out:**
//!
//! | road | what runs, in order | why nothing is freed twice or read after it is freed |
//! |---|---|---|
//! | a refusal in `open` | before `avformat_alloc_output_context2`, nothing is allocated; after a failed one, the `StreamIo` drops (it flushes nothing, then the sink's flush through the guard, then frees) and the sink with it | `ctx` is null on failure and owns nothing of this crate's |
//! | the file finished: `write_trailer`, then `into_sink` | the context freed; `avio_flush`; `AVIOContext.error` read; `into_inner` frees the buffer and the I/O context and hands back the guard; the sink's flush; then the latch, the seek record, the write error and the flush error are named, in that order, or the sink is handed back | each field is moved out of the `ManuallyDrop<Self>` once (`ptr::read`) and never used through it again; `pb` is read after the context that pointed at it is gone, and before `into_inner` frees it |
//! | `into_sink` before the trailer — from `Building`, `Writing` or `Failed` | exactly the file-finished road: the context freed (from `Writing`, the muxer's `deinit` and the queued packets with it, as in `Drop`), then the I/O context flushed, read and freed, and the sink handed back holding what was written — a container without its trailer, which no phase check refuses: the call is the caller's way to take the sink back from any phase | as above; nothing in the road depends on the phase |
//! | an error from any call | the call answers it, and the context stays as the phase says; a call out of order is refused before libavformat is reached (a second trailer would run a muxer's trailer over state its first freed) | the phase |
//! | a panic in the sink, in any callback | caught by the guard under `catch_unwind`, latched, answered to C as an I/O error; every later call into the sink answers an error without reaching it; the context names the panic first | the panic never reaches the `extern "C"` frame; nothing is freed while a callback runs |
//! | an error whose payload panics when dropped | reduced inside the guard's `catch_unwind` to its OS code or kind, the payload dropped there and its panic latched | what reaches C owns nothing |
//! | a failed seek | kept by the guard; every later write and seek refused; named by every call that reached libavformat, whatever libavformat answered | — |
//! | the `Output` dropped, at any point — mid-file, after an error, while the caller unwinds | `avformat_free_context` (the muxer's `deinit`, the queued packets freed), then the `StreamIo` drops: buffered bytes flushed through the guard, which writes nothing after a panic or a failed seek, then the sink's flush, then the frees | the context is freed before the I/O context its `pb` points at, and each exactly once |
//!
//! # Every `unsafe` read and write
//!
//! No `unsafe fn` is public here; each block's contract is established
//! inside the block's function, and its `SAFETY` comment says how. By
//! site:
//!
//! | site | what is read or written | the contract |
//! |---|---|---|
//! | `open` | `av_guess_format`; the static muxer's `flags`; `avformat_alloc_output_context2`; `ctx.pb` and `ctx.flags` written | a NUL-terminated name live for the call; a non-null static muxer; a valid out-pointer, null on failure; the I/O context outlives the context, and is the caller's (`AVFMT_FLAG_CUSTOM_IO`) |
//! | `container`, `strict_decode_times`, `shifts_early_times` | the static muxer's `name` (bounded, `table_text`) and `flags` | `oformat` is never null on a context `open` returned |
//! | `sink_fault` | the I/O context's `error` (`pb`, kept from `open`) | the I/O context the `Output` owns, live as long as it is; a plain `int` libavformat writes only inside the `&mut self` calls, read after they return |
//! | `query_codec` | `avformat_query_codec` through a shim taking the codec id as `c_int` | no bindgen enum formed from an integer; libavformat searches its tag tables, never indexes them |
//! | `streams`, `stream`, `stream_time_base` | `nb_streams`; `streams[index]`; a stream's `time_base` | `index < nb_streams`, checked first |
//! | `stream_codec` | a stream's `codecpar` fields, the codec id and media type as integers, the extradata as `extradata_size` bytes; `av_get_audio_frame_duration2` | libavutil's allocation, null when empty; the call reads plain fields and writes nothing |
//! | `set_automatic_filters` | `ctx.flags` (`AVFMT_FLAG_AUTO_BSF`) | read by libavformat only when it writes a packet |
//! | `add_stream` | `avformat_new_stream`; `avcodec_parameters_copy`; the media type written as an integer; `replace_extradata`; `settle_codec_tag`; `stream_copy_audio_rule`; the stream's `time_base`, `disposition`, `sample_aspect_ratio`, `avg_frame_rate`; `av_dict_set` | the stream just added, its `codecpar` its own; the ticket's parameters live and null-checked; extradata whose size with its padding fits an `int`, judged first; no `DONT_STRDUP`, so the texts are copied |
//! | `replace_extradata` | `av_freep` of the old extradata; `av_mallocz`; the copy | the old one null or libavutil's; a fresh allocation of `bytes.len()` plus the padding, which cannot overlap `bytes` |
//! | `settle_codec_tag` | the parameters' tag and codec id; the muxer's `codec_tag` list; `av_codec_get_id`, `av_codec_get_tag2` through shims taking integers; the tag cleared | the list null or libavformat's static, null-terminated tables |
//! | `stream_copy_audio_rule` | the media type, the codec id and `block_align` | plain fields, the enums read as integers |
//! | `add_chapter` | `nb_chapters`; `av_mallocz`; the chapter's fields; `av_dict_set`; `av_dynarray_add_nofree`; `nb_chapters` written | the count as the `int` the call takes, `c_int::MAX` refused; the chapter ours alone until the array takes it |
//! | `free_chapter` | `av_dict_free`, `av_free` | a chapter this module allocated and no array took |
//! | `write_header`, `write_trailer` | `avformat_write_header`, `av_write_trailer` | the phase allows exactly one of each, in order |
//! | `write_packet` | the boundary's builders, on the copy road; the packet's `stream_index`; `av_interleaved_write_frame` | every builder copies, so nothing a caller holds is lent; a stream of this context; the reference taken over, the packet left blank |
//! | `into_sink` | the fields moved out of `ManuallyDrop`; `avformat_free_context`; `avio_flush`; `AVIOContext.error` | each field moved once; the context freed before the I/O context; `pb` live until `into_inner` |
//! | `Drop` | `avformat_free_context`; `ManuallyDrop::drop` of the I/O context | each exactly once, the context first |
//! | `unsafe impl Send` | — | the context and the I/O context are heap allocations reached only through `&mut self`; the sink is `W: Send` |
//!
//! And the other reads of FFmpeg the muxer reaches, outside this module:
//!
//! | site | what is read | the contract |
//! |---|---|---|
//! | `CodecId::mime_type` (`codec_id.rs`) | the codec descriptor's `mime_types`, its first entry, the text | the descriptor a static table entry of libavcodec's or null; a non-null list holds at least its terminating null; the text bounded (`table_text`) |
//! | `CodecId::intra_only` | the descriptor's media type and `props` | as above, the media type read as an integer |
//! | `CodecId::media_type` | the descriptor's media type | as above |
//! | `CodecId::bits_per_sample` | `av_get_bits_per_sample` through a shim taking `c_int` | no bindgen enum formed; libavcodec answers 0 for a codec it does not know |
//! | `packet_side_data_name` (`extras.rs`) | `av_packet_side_data_name` through a shim taking `c_int` | as above; null for a type it does not name, the text bounded |
//! | `hdr_metadata::check_hdr10_plus`, `check_smpte2094_app5` | the linked libavutil's struct size, through its allocator (redeclared shims), the struct freed at once; the entry itself read through the mirrors' offsets as integers, with no `unsafe` | the allocator answers a zeroed struct or null, and writes its size to a valid out-pointer |
//! | `hdr_metadata::hdr10_plus_from_t35`, `smpte2094_app5_from_t35` | the struct libavutil parses from a payload, copied out as its bytes | the payload live for the call, its length passed; the struct zeroed by its allocator and freed exactly once |
//! | the boundary's builders, `build_data_packet` and `build_attachment_packet` among them (`boundary.rs`) | the carrier's payload, copied (`packet_body` on the copy road), its side data attached | each builder's contract, forwarded: on the copy road nothing is lent, so there is nothing to prove |
//!
//! # Every FFmpeg call, its failures, and whose they are
//!
//! An [`OutputError`] says whose a failure is: the caller's sink
//! (`SinkPanic`, `SinkSeek`, `SinkWrite`, `Sink`), libavformat's own
//! answer (`Ffmpeg`), or a refusal this crate makes of the caller's input
//! before libavformat is reached (`UnknownContainer`, `NoByteStream`,
//! `Nul`, `ExtradataTooLarge`, `TooManyChapters`, `NoSuchStream`,
//! `OutOfOrder`, `TimeBase`, `Build`, `Parameters`). A call that reached
//! the sink names the sink's fault before libavformat's answer: a panic
//! first, then a failed seek, then a failed write, whatever libavformat
//! answered.
//!
//! | call | reached from | its failures | named as |
//! |---|---|---|---|
//! | `av_guess_format` | `open` | null | `UnknownContainer` |
//! | `StreamIo::from_write_seek`: `av_mallocz`, `avio_alloc_context` | `open` | `ENOMEM` | `Ffmpeg` |
//! | `avformat_alloc_output_context2` | `open` | a negative code, or a null context | `Ffmpeg` (`AVERROR_UNKNOWN` were it to answer success with no context, which `mux.c` 95–159 never does) |
//! | `avformat_query_codec` | `query_codec` | a negative code: libavformat cannot tell | `CodecQuery::Unknown` |
//! | `avformat_new_stream` | `add_stream` | null | `Ffmpeg(ENOMEM)`; nothing added |
//! | `avcodec_parameters_copy` | `add_stream` | a negative code (an allocation) | `Ffmpeg`; the context `Failed`, a stream half made |
//! | `av_mallocz`, after `av_freep` of the old extradata | `replace_extradata` | null | `Ffmpeg(ENOMEM)`; `Failed` |
//! | `av_codec_get_id`, `av_codec_get_tag2` | `settle_codec_tag` | none: an answer | — |
//! | `av_dict_set` | `add_stream`, `add_chapter` | a negative code (an allocation) | `Ffmpeg`; in `add_stream` the context `Failed`; in `add_chapter` the chapter not yet taken is freed, and the context still being built |
//! | `av_mallocz`, `av_dynarray_add_nofree` | `add_chapter` | null; a negative code | `Ffmpeg(ENOMEM)`; `Ffmpeg`, the chapter freed |
//! | `av_get_audio_frame_duration2` | `stream_codec` | none: zero for a codec it does not know | — |
//! | the ticket's rebuild (`avcodec_parameters_alloc` and its copies) | `add_stream` | its allocations, its limits | `Parameters`, before any stream is added |
//! | `avcodec_parameters_free`, in the rebuilt `Parameters`' `Drop` | `add_stream` | none | — |
//! | the copy road: `av_new_packet`, `av_packet_new_side_data` | `write_packet` | an allocation; a limit; side data libavcodec cannot name | `Build`, before libavformat is reached |
//! | `avformat_write_header` | `write_header` | a negative code; the sink's panic, failed seek or failed write — the last read from the I/O context, as the header's own flush follows its check of it (`mux.c` 482–486) | the sink's fault, else `Ffmpeg`; the context `Failed` |
//! | `av_interleaved_write_frame` | `write_packet` | a negative code; the sink's panic, failed seek or failed write (`mux.c` 751–752 answers the last as its own) | the sink's fault, else `Ffmpeg` |
//! | `av_packet_unref`, in the blank `Packet`'s `Drop` | `write_packet` | none: a no-op on the packet `av_interleaved_write_frame` left blank | — |
//! | `av_write_trailer` | `write_trailer` | a negative code; the sink's panic, failed seek or failed write (`mux.c` 1270–1272 answers the last as its own) | the sink's fault, else `Ffmpeg`; the context `Finished` either way |
//! | `avio_flush` and `AVIOContext.error` | `into_sink` | the first failed write, which the I/O context keeps | `SinkWrite`, after a panic and a failed seek |
//! | the sink's `flush`, through the guard | `into_sink` | its `io::Error` | `Sink`, last |
//! | `av_dict_free`, `av_free`, in `free_chapter` | `add_chapter`, on each failure before the array takes the chapter | none | — |
//! | `avformat_free_context`, `StreamIo`'s `Drop` and `into_inner` (`av_freep`, `avio_context_free`) | `into_sink`, `Drop` | none | — |

use core::{
  ffi::{c_int, c_uint, c_void},
  marker::PhantomData,
  mem::ManuallyDrop,
  num::NonZeroI32,
  ptr::{self, NonNull, addr_of, addr_of_mut, read_unaligned, write_unaligned},
};
use std::{
  ffi::CString,
  io::{self, Seek, Write},
  sync::Arc,
};

use ffmpeg_next::{
  Packet,
  ffi::{
    AV_DISPOSITION_ATTACHED_PIC, AV_INPUT_BUFFER_PADDING_SIZE, AVChapter, AVCodecParameters,
    AVFMT_FLAG_AUTO_BSF, AVFMT_FLAG_CUSTOM_IO, AVFMT_NOFILE, AVFMT_NOTIMESTAMPS, AVFMT_TS_NEGATIVE,
    AVFMT_TS_NONSTRICT, AVFormatContext, AVIOContext, AVRational, AVStream, av_dict_free,
    av_dict_set, av_dynarray_add_nofree, av_free, av_freep, av_guess_format,
    av_interleaved_write_frame, av_mallocz, av_write_trailer, avcodec_parameters_copy,
    avformat_alloc_output_context2, avformat_free_context, avformat_new_stream,
    avformat_write_header, avio_flush,
  },
  format::context::StreamIo,
  packet::Mut,
};
use mediaspine::{
  Timebase,
  demuxer::{AttachmentPacket, DataPacket},
  packet::{AudioPacket, SubtitlePacket, VideoPacket},
};
use smol_bytes::Utf8Bytes;

use crate::{
  CodecId,
  boundary::{
    PacketBuildError, build_attachment_packet, build_audio_packet, build_data_packet,
    build_subtitle_packet, build_video_packet,
  },
  carrier::BodyRoute,
  error::ParametersError,
  extras::{
    AttachmentPacketExtra, AudioPacketExtra, DataPacketExtra, SubtitlePacketExtra, VideoPacketExtra,
  },
  ffi::c_shims,
  limits::PacketLimits,
  reader_guard::PanicLatch,
  ticket::CodecTicket,
  writer_guard::{GuardedWriter, SeekRecord},
};

/// `AVMEDIA_TYPE_VIDEO`, as the integer `AVCodecParameters.codec_type`
/// stores it.
const MEDIA_VIDEO: i32 = ffmpeg_next::ffi::AVMediaType::AVMEDIA_TYPE_VIDEO as i32;
/// `AVMEDIA_TYPE_AUDIO`, likewise.
const MEDIA_AUDIO: i32 = ffmpeg_next::ffi::AVMediaType::AVMEDIA_TYPE_AUDIO as i32;
/// `AVMEDIA_TYPE_ATTACHMENT`, likewise.
const MEDIA_ATTACHMENT: i32 = ffmpeg_next::ffi::AVMediaType::AVMEDIA_TYPE_ATTACHMENT as i32;

/// Where a context is in its life — see [the module](self).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum OutputPhase {
  /// Streams and chapters are being added; nothing is written yet.
  Building,
  /// The header is written; packets may be written.
  Writing,
  /// The trailer was written, or attempted: the muxer is done.
  Finished,
  /// A step libavformat cannot be asked to continue from failed: the
  /// header, or the addition of a stream it half-built.
  Failed,
}

/// A call [`Output`] refused for being out of order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum OutputCall {
  /// [`Output::add_stream`].
  AddStream,
  /// [`Output::add_chapter`].
  AddChapter,
  /// [`Output::write_header`].
  WriteHeader,
  /// [`Output::write_packet`].
  WritePacket,
  /// [`Output::write_trailer`].
  WriteTrailer,
  /// [`Output::set_automatic_filters`].
  AutomaticFilters,
}

/// A text handed to libavformat as a C string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum OutputText {
  /// The container's short name.
  ContainerName,
  /// A stream's language.
  Language,
  /// An attachment's filename.
  Filename,
  /// An attachment's MIME type.
  MimeType,
  /// A chapter's title.
  ChapterTitle,
}

/// Why an [`Output`] call failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum OutputError {
  /// libavformat has no muxer by that short name.
  #[error("libavformat has no muxer named {name:?}")]
  UnknownContainer {
    /// The name asked for.
    name: Utf8Bytes,
  },
  /// The muxer writes no byte stream of its own (`AVFMT_NOFILE` — an
  /// image sequence, a device), so a caller's sink would never receive
  /// its output.
  #[error("the {name:?} muxer writes no byte stream, so it cannot write into a sink")]
  NoByteStream {
    /// The muxer's name.
    name: Utf8Bytes,
  },
  /// A text holds a NUL byte, which a C string cannot carry.
  #[error("the {text:?} holds a NUL byte, which a C string cannot carry")]
  Nul {
    /// Which text.
    text: OutputText,
  },
  /// Extradata, or an attachment's file, too large for an
  /// `AVCodecParameters` to hold (`extradata_size` is a C `int`, and the
  /// padding libavcodec reads past it is allocated with it).
  #[error("{bytes} bytes of extradata do not fit an AVCodecParameters")]
  ExtradataTooLarge {
    /// The size offered.
    bytes: usize,
  },
  /// More chapters than libavformat's `int` count can hold.
  #[error("libavformat cannot count another chapter")]
  TooManyChapters,
  /// The stream index names no stream of this context.
  #[error("stream {stream} is not in the context")]
  NoSuchStream {
    /// The index offered.
    stream: usize,
  },
  /// A call out of the order libavformat defines — see [the
  /// module](self).
  #[error("{call:?} is out of order: the context is {phase:?}")]
  OutOfOrder {
    /// The call refused.
    call: OutputCall,
    /// Where the context was.
    phase: OutputPhase,
  },
  /// A time base libavformat chose that this family cannot carry (a
  /// zero or negative part).
  #[error("libavformat chose the time base {num}/{den} for stream {stream}")]
  TimeBase {
    /// The stream.
    stream: usize,
    /// The numerator libavformat chose.
    num: i32,
    /// The denominator libavformat chose.
    den: i32,
  },
  /// The caller's sink panicked. libavformat saw an I/O error; this is
  /// what it was.
  #[error("the sink panicked: {message}")]
  SinkPanic {
    /// The panic's message.
    message: Utf8Bytes,
  },
  /// The caller's sink failed its final flush.
  #[error("the sink failed its final flush: {0}")]
  Sink(#[source] io::Error),
  /// A seek into the caller's sink failed, so what libavformat placed
  /// after it — a header it went back to patch, an index at the end — is
  /// not where it belongs. libavformat keeps no record of a failed seek,
  /// and a muxer may ignore the answer and write on; the writer guard
  /// keeps the first failure and writes nothing after it, and every call
  /// that reached libavformat names it
  /// ([`SeekRecord`](crate::writer_guard::SeekRecord)).
  #[error("a seek into the sink failed, so what libavformat placed after it is missing: {0}")]
  SinkSeek(#[source] io::Error),
  /// A write into the caller's sink failed, so bytes meant for it are
  /// not in it — whatever its own flush says. The I/O context keeps the
  /// first failed write's error (`AVIOContext.error`) and writes nothing
  /// after it; every call that reached libavformat reads it there once
  /// libavformat returns, and [`Output::into_sink`] once more after its
  /// last flush. The error is the sink's, as ffmpeg-next's write callback
  /// handed it to C (see [the module](self)).
  #[error("a write into the sink failed, so bytes meant for it are missing: {0}")]
  SinkWrite(#[source] ffmpeg_next::Error),
  /// libavformat's own error.
  #[error(transparent)]
  Ffmpeg(#[from] ffmpeg_next::Error),
  /// A packet could not be built from its carrier.
  #[error(transparent)]
  Build(#[from] PacketBuildError),
  /// A stream's codec parameters could not be rebuilt from its ticket.
  #[error(transparent)]
  Parameters(#[from] ParametersError),
}

/// What a muxer can say about carrying a codec, asked before the header
/// rather than learned from its failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodecQuery {
  /// The muxer can store the codec.
  Carried,
  /// The muxer cannot store the codec.
  NotCarried,
  /// The muxer cannot tell: the header is where it will say.
  Unknown,
}

/// How a stream's packets reach the container — what kind of stream
/// [`Output::add_stream`] makes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum StreamMedia {
  /// A timed stream of the ticket's own media type, written packet by
  /// packet.
  Timed,
  /// An `AVMEDIA_TYPE_ATTACHMENT` stream whose extradata is the file —
  /// how Matroska carries an attachment, with its header. It never takes
  /// a packet.
  Attachment,
  /// A video stream whose disposition is exactly
  /// `AV_DISPOSITION_ATTACHED_PIC`, its picture written as its one
  /// packet after the header — how MP4 keeps cover art (`movenc.c`
  /// 184–189, 7606–7618 in FFmpeg 9.0.1).
  Cover,
}

/// What [`Output::add_stream`] needs to make one stream.
///
/// The codec parameters come from a [`CodecTicket`], rebuilt and copied
/// into the stream; everything else the stream carries is set beside
/// them. Texts are Rust strings, refused by name if they hold a NUL.
#[derive(Debug, Clone, Copy)]
pub struct StreamSpec<'a> {
  ticket: &'a CodecTicket,
  time_base: Timebase,
  media: StreamMedia,
  extradata: Option<&'a [u8]>,
  disposition: i32,
  frame_rate: Option<Timebase>,
  language: Option<&'a str>,
  filename: Option<&'a str>,
  mime_type: Option<&'a str>,
}

impl<'a> StreamSpec<'a> {
  /// A timed stream over `ticket`'s parameters, its packets stamped in
  /// `time_base` — the hint libavformat starts from; it may choose
  /// another at the header ([`Output::stream_time_base`]).
  pub const fn new(ticket: &'a CodecTicket, time_base: Timebase) -> Self {
    Self {
      ticket,
      time_base,
      media: StreamMedia::Timed,
      extradata: None,
      disposition: 0,
      frame_rate: None,
      language: None,
      filename: None,
      mime_type: None,
    }
  }

  /// Makes the stream of `media`'s kind.
  #[must_use]
  pub const fn with_media(mut self, media: StreamMedia) -> Self {
    self.media = media;
    self
  }

  /// Replaces the ticket's extradata — with an attachment's file, for
  /// [`StreamMedia::Attachment`].
  #[must_use]
  pub const fn with_extradata(mut self, extradata: Option<&'a [u8]>) -> Self {
    self.extradata = extradata;
    self
  }

  /// The `AV_DISPOSITION_*` bits the stream declares. A
  /// [`StreamMedia::Cover`] stream declares exactly
  /// `AV_DISPOSITION_ATTACHED_PIC`, whatever is given here.
  #[must_use]
  pub const fn with_disposition(mut self, disposition: i32) -> Self {
    self.disposition = disposition;
    self
  }

  /// The average frame rate a video stream declares.
  #[must_use]
  pub const fn with_frame_rate(mut self, frame_rate: Option<Timebase>) -> Self {
    self.frame_rate = frame_rate;
    self
  }

  /// The stream's `language` metadata.
  #[must_use]
  pub const fn with_language(mut self, language: Option<&'a str>) -> Self {
    self.language = language;
    self
  }

  /// An attachment's `filename` metadata.
  #[must_use]
  pub const fn with_filename(mut self, filename: Option<&'a str>) -> Self {
    self.filename = filename;
    self
  }

  /// An attachment's `mimetype` metadata.
  #[must_use]
  pub const fn with_mime_type(mut self, mime_type: Option<&'a str>) -> Self {
    self.mime_type = mime_type;
    self
  }
}

/// One packet for [`Output::write_packet`], by kind, on carrier `C`.
#[non_exhaustive]
pub enum OutputPacket<'a, C: crate::FfmpegCarrier> {
  /// A video packet.
  Video(&'a VideoPacket<VideoPacketExtra, C::Buffer>),
  /// An audio packet.
  Audio(&'a AudioPacket<AudioPacketExtra, C::Buffer>),
  /// A subtitle packet.
  Subtitle(&'a SubtitlePacket<SubtitlePacketExtra, C::Buffer>),
  /// A timed-data packet.
  Data(&'a DataPacket<DataPacketExtra, C::Buffer>),
  /// A [`StreamMedia::Cover`] stream's one picture.
  Cover(&'a AttachmentPacket<AttachmentPacketExtra, C::Buffer>),
}

/// A packet's timestamps in its stream's time base, as the caller
/// rescaled them — what libavformat is handed, in place of the
/// carrier's own.
///
/// `i64::MIN` is libavformat's `AV_NOPTS_VALUE`: a presentation or decode
/// time of that count reads as none at all, so a caller refuses such a
/// time before building these — `mediaencode-ffmpeg`'s muxer does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PacketTimes {
  pts: Option<i64>,
  dts: Option<i64>,
  duration: i64,
}

impl PacketTimes {
  /// The presentation and decode timestamps, and the duration (`0` for
  /// unknown), each counted in the stream's time base.
  pub const fn new(pts: Option<i64>, dts: Option<i64>, duration: i64) -> Self {
    Self { pts, dts, duration }
  }
}

/// What libavformat's packet path reads of a stream's codec parameters,
/// as the stream holds them once [`Output::add_stream`] made it — the
/// facts a caller needs to keep a packet from a rule libavformat would
/// apply only after taking it ([`Output::stream_codec`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamCodec {
  codec: CodecId,
  video_delay: i32,
  sample_rate: i32,
  channels: i32,
  frame_size: i32,
  frame_duration: i32,
  block_align: i32,
  initial_padding: i32,
  extradata: Vec<u8>,
}

impl StreamCodec {
  /// The stream's codec.
  pub const fn codec(&self) -> CodecId {
    self.codec
  }

  /// How many frames a video stream's decoder holds back before the
  /// first comes out (`video_delay`): above zero for a stream that
  /// reorders, whose packets' presentation times libavformat cannot
  /// make up from their decode times.
  pub const fn video_delay(&self) -> i32 {
    self.video_delay
  }

  /// An audio stream's sample rate, or `0`.
  pub const fn sample_rate(&self) -> i32 {
    self.sample_rate
  }

  /// An audio stream's channel count, or `0`.
  pub const fn channels(&self) -> i32 {
    self.channels
  }

  /// The samples in each of an audio stream's frames where the codec
  /// fixes them, or `0`.
  pub const fn frame_size(&self) -> i32 {
    self.frame_size
  }

  /// The samples every frame of an audio stream's codec holds, as
  /// libavcodec tells them from its parameters alone
  /// (`av_get_audio_frame_duration2` of no bytes), or `0` where the codec
  /// does not fix them.
  pub const fn frame_duration(&self) -> i32 {
    self.frame_duration
  }

  /// An audio stream's bytes per block, or `0`.
  pub const fn block_align(&self) -> i32 {
    self.block_align
  }

  /// The samples an audio stream's encoder put before its first, or
  /// `0`.
  pub const fn initial_padding(&self) -> i32 {
    self.initial_padding
  }

  /// The stream's extradata — its codec's configuration — as the stream
  /// holds it, without the padding.
  pub fn extradata(&self) -> &[u8] {
    &self.extradata
  }
}

/// libavformat's muxing context over a caller's sink — see [the
/// module](self).
pub struct Output<W> {
  /// The context, owned: allocated by [`Self::open`], freed exactly once
  /// — by `Drop`, or by [`Self::into_sink`] — and always before `io`.
  ctx: NonNull<AVFormatContext>,
  /// The I/O context over the guarded sink. `ctx.pb` points at it, and
  /// `AVFMT_FLAG_CUSTOM_IO` keeps libavformat from closing it.
  io: ManuallyDrop<StreamIo>,
  /// The `AVIOContext` `io` owns — `ctx.pb` — kept so that `&self` can
  /// read the failed write it keeps. Owns nothing: freed with `io`.
  pb: NonNull<AVIOContext>,
  /// The writer guard's latch, to name a sink's panic.
  latch: Arc<PanicLatch>,
  /// The writer guard's record of a failed seek, to name it.
  seeks: Arc<SeekRecord>,
  phase: OutputPhase,
  /// The sink's type, held by `io` type-erased.
  _sink: PhantomData<fn(W) -> W>,
}

// SAFETY: the context and the I/O context are heap allocations tied to no
// thread, and every access to either goes through `&mut self` — `&self`
// methods read fields libavformat writes only inside `&mut self` calls —
// so moving an `Output` to another thread moves exclusive ownership of
// both. The sink inside the I/O context is a `W: Send` (`open` demands
// it). `Sync` is not implemented.
unsafe impl<W: Send> Send for Output<W> {}

impl<W> Output<W> {
  /// The phase the context is in.
  pub const fn phase(&self) -> OutputPhase {
    self.phase
  }

  /// The muxer's short name — `"matroska"`, `"mp4"`.
  pub fn container(&self) -> &'static str {
    // SAFETY: `ctx` is live, and its `oformat` is the static muxer
    // `open` found, never null on a context `open` returned; its `name`
    // is a NUL-terminated literal in libavformat's static tables.
    unsafe {
      let oformat = (*self.ctx.as_ptr()).oformat;
      crate::ffi::table_text((*oformat).name, 256).unwrap_or("")
    }
  }

  /// Whether the muxer refuses a packet whose decode time equals the
  /// last one written on its stream — libavformat's rule for every muxer
  /// without `AVFMT_TS_NONSTRICT`, on every stream but a subtitle or data
  /// stream, and only after a stream's first nonzero decode time
  /// (`compute_muxer_pkt_fields`, `libavformat/mux.c` 557–566 in FFmpeg
  /// 9.0.1). A muxer with `AVFMT_NOTIMESTAMPS` ignores the check, and
  /// answers `false` here.
  pub fn strict_decode_times(&self) -> bool {
    // SAFETY: as [`Self::container`] — a plain integer field of the
    // static muxer.
    let flags = unsafe { (*(*self.ctx.as_ptr()).oformat).flags };
    flags & (AVFMT_TS_NONSTRICT | AVFMT_NOTIMESTAMPS) == 0
  }

  /// Whether the muxer can store `codec`, asked of libavformat
  /// (`avformat_query_codec`) before the header rather than learned from
  /// its failure.
  pub fn query_codec(&self, codec: CodecId) -> CodecQuery {
    // SAFETY: `ctx` is live and `oformat` is its static muxer; the
    // redeclared shim takes the codec id as a plain `c_int`, so no
    // bindgen enum is formed from it, and libavformat answers every
    // integer (its tag tables are searched, never indexed).
    let answer = unsafe {
      let ctx = self.ctx.as_ptr();
      c_shims::avformat_query_codec((*ctx).oformat, codec.raw(), (*ctx).strict_std_compliance)
    };
    match answer {
      1.. => CodecQuery::Carried,
      0 => CodecQuery::NotCarried,
      _ => CodecQuery::Unknown,
    }
  }

  /// The time base libavformat chose for `stream` — after the header,
  /// the one its packets must be counted in.
  pub fn stream_time_base(&self, stream: usize) -> Result<Timebase, OutputError> {
    let st = self.stream(stream)?;
    // SAFETY: `st` is a live stream of the live context.
    let AVRational { num, den } = unsafe { (*st).time_base };
    match (num > 0, NonZeroI32::new(den)) {
      (true, Some(nz)) if den > 0 => Ok(Timebase::new(num, nz)),
      _ => Err(OutputError::TimeBase { stream, num, den }),
    }
  }

  /// The number of streams added so far.
  pub fn streams(&self) -> usize {
    // SAFETY: a plain integer field of the live context.
    unsafe { (*self.ctx.as_ptr()).nb_streams as usize }
  }

  /// The panic the sink raised, if it raised one.
  pub fn sink_panic(&self) -> Option<Utf8Bytes> {
    self.latch.message()
  }

  /// Whether libavformat moves every stream's times when the first one it
  /// writes lies below the lowest its stream allows
  /// (`handle_avoid_negative_ts`, `libavformat/mux.c` 636–717; the mode,
  /// 411–421, in FFmpeg 9.0.1). A muxer that takes negative times as they
  /// are (`AVFMT_TS_NEGATIVE`: MP4) or writes none
  /// (`AVFMT_NOTIMESTAMPS`) is left alone.
  pub fn shifts_early_times(&self) -> bool {
    // SAFETY: as [`Self::container`] — a plain integer field of the
    // static muxer.
    let flags = unsafe { (*(*self.ctx.as_ptr()).oformat).flags };
    flags & (AVFMT_TS_NEGATIVE | AVFMT_NOTIMESTAMPS) == 0
  }

  /// What libavformat's packet path reads of `stream`'s codec parameters,
  /// as the stream holds them ([`StreamCodec`]).
  pub fn stream_codec(&self, stream: usize) -> Result<StreamCodec, OutputError> {
    let st = self.stream(stream)?;
    // SAFETY: `st` is a live stream of the live context and its
    // `codecpar` the stream's own; plain integer fields are read, the
    // codec id and media type as the integers they are stored as, and the
    // extradata as the `extradata_size` bytes libavutil allocated (null
    // when there is none). `av_get_audio_frame_duration2` reads the same
    // parameters' plain fields and writes nothing.
    unsafe {
      let par = (*st).codecpar;
      let extradata = match ((*par).extradata, usize::try_from((*par).extradata_size)) {
        (data, Ok(len)) if !data.is_null() && len > 0 => {
          core::slice::from_raw_parts(data, len).to_vec()
        }
        _ => Vec::new(),
      };
      // libavcodec reads only the parameters' plain fields, and answers
      // zero for a codec it does not know.
      let audio = read_unaligned(addr_of!((*par).codec_type).cast::<i32>())
        == ffmpeg_next::ffi::AVMediaType::AVMEDIA_TYPE_AUDIO as i32;
      let frame_duration = if audio {
        ffmpeg_next::ffi::av_get_audio_frame_duration2(par, 0)
      } else {
        0
      };
      Ok(StreamCodec {
        codec: CodecId::from_raw(read_unaligned(addr_of!((*par).codec_id).cast::<i32>())),
        video_delay: (*par).video_delay,
        sample_rate: (*par).sample_rate,
        channels: (*par).ch_layout.nb_channels,
        frame_size: (*par).frame_size,
        frame_duration,
        block_align: (*par).block_align,
        initial_padding: (*par).initial_padding,
        extradata,
      })
    }
  }

  /// The stream at `index`, or a refusal naming it.
  fn stream(&self, index: usize) -> Result<*mut AVStream, OutputError> {
    if index >= self.streams() {
      return Err(OutputError::NoSuchStream { stream: index });
    }
    // SAFETY: `streams` holds `nb_streams` live stream pointers, and
    // `index` is below that count.
    Ok(unsafe { *(*self.ctx.as_ptr()).streams.add(index) })
  }

  /// libavformat's `code`, named — the sink's panic first, if it raised
  /// one, then a seek it failed, then a write it failed: libavformat
  /// reports each as the I/O error the guard or the sink answered, folded
  /// into whatever it was doing, or not at all.
  fn fault(&self, code: c_int) -> OutputError {
    self
      .sink_fault()
      .unwrap_or_else(|| OutputError::Ffmpeg(ffmpeg_next::Error::from(code)))
  }

  /// What the sink did that libavformat cannot be trusted to report: a
  /// panic, else a failed seek, else a failed write — the first the I/O
  /// context kept (see [the module](self)).
  fn sink_fault(&self) -> Option<OutputError> {
    if let Some(message) = self.latch.message() {
      return Some(OutputError::SinkPanic { message });
    }
    if let Some(error) = self.seeks.failure() {
      return Some(OutputError::SinkSeek(error));
    }
    // SAFETY: `pb` is the I/O context `io` owns, live for as long as this
    // value is (it is freed only by `Drop` or `into_sink`, which consume
    // it); `error` is a plain `int` field libavformat writes only inside
    // the calls this type makes through `&mut self`, which have returned.
    let written = unsafe { (*self.pb.as_ptr()).error };
    (written < 0).then(|| OutputError::SinkWrite(ffmpeg_next::Error::from(written)))
  }

  /// libavformat's answer `status` to a call that reached the sink, named:
  /// the sink's own fault first, whatever libavformat answered — a muxer
  /// may carry on past a seek it failed and answer success, and the
  /// header answers success over a write its own last flush failed — then
  /// libavformat's error.
  fn settled(&self, status: c_int) -> Result<(), OutputError> {
    match self.sink_fault() {
      Some(fault) => Err(fault),
      None if status < 0 => Err(OutputError::Ffmpeg(ffmpeg_next::Error::from(status))),
      None => Ok(()),
    }
  }

  /// Refuses `call` unless the context is `phase`.
  fn require(&self, call: OutputCall, phase: OutputPhase) -> Result<(), OutputError> {
    if self.phase == phase {
      Ok(())
    } else {
      Err(OutputError::OutOfOrder {
        call,
        phase: self.phase,
      })
    }
  }
}

impl<W: Write + Seek + Send + 'static> Output<W> {
  /// Opens a context for the muxer libavformat names `container` — its
  /// short name, `"matroska"` or `"mp4"` — writing into `sink`.
  ///
  /// Refused by name: a name libavformat does not know, a name holding a
  /// NUL, and a muxer that writes no byte stream of its own. `sink` is
  /// dropped with a refusal: nothing was written into it.
  pub fn open(sink: W, container: &str) -> Result<Self, OutputError> {
    let name = CString::new(container).map_err(|_| OutputError::Nul {
      text: OutputText::ContainerName,
    })?;
    // SAFETY: `name` is a NUL-terminated string live for the call; the
    // other two arguments may be null. libavformat answers a static
    // muxer or null.
    let oformat = unsafe { av_guess_format(name.as_ptr(), ptr::null(), ptr::null()) };
    if oformat.is_null() {
      return Err(OutputError::UnknownContainer {
        name: Utf8Bytes::from(container),
      });
    }
    // SAFETY: `oformat` is a non-null static muxer; `flags` is a plain
    // integer field of it.
    if unsafe { (*oformat).flags } & AVFMT_NOFILE != 0 {
      return Err(OutputError::NoByteStream {
        name: Utf8Bytes::from(container),
      });
    }

    let (guarded, latch, seeks) = GuardedWriter::new(sink);
    let mut io = StreamIo::from_write_seek(guarded)?;
    // `from_write_seek` answers a live I/O context or an error, so the
    // pointer is never null; checked rather than assumed, before anything
    // else is allocated.
    let pb =
      NonNull::new(io.as_mut_ptr()).ok_or(OutputError::Ffmpeg(ffmpeg_next::Error::Other {
        errno: libc::ENOMEM,
      }))?;
    let mut ctx: *mut AVFormatContext = ptr::null_mut();
    // SAFETY: `ctx` is a valid out-pointer; `oformat` is the static muxer
    // just found, so the two names may be null. On failure libavformat
    // leaves `ctx` null and owns nothing of ours.
    let status =
      unsafe { avformat_alloc_output_context2(&mut ctx, oformat, ptr::null(), ptr::null()) };
    let Some(ctx) = NonNull::new(ctx).filter(|_| status >= 0) else {
      // A success with no context is one `avformat_alloc_output_context2`
      // never answers (`mux.c` 95–159); named as libavformat's unknown
      // error, not as an errno it did not report.
      let code = if status < 0 {
        status
      } else {
        ffmpeg_next::ffi::AVERROR_UNKNOWN
      };
      return Err(OutputError::Ffmpeg(ffmpeg_next::Error::from(code)));
    };
    // SAFETY: `ctx` is the live context just allocated, and nothing else
    // holds it. `pb` is set to the I/O context this value keeps alive
    // longer than the context — `Drop` and `into_sink` free the context
    // first — and `AVFMT_FLAG_CUSTOM_IO` tells libavformat the I/O
    // context is the caller's, so it never closes or frees it.
    unsafe {
      (*ctx.as_ptr()).pb = pb.as_ptr();
      (*ctx.as_ptr()).flags |= AVFMT_FLAG_CUSTOM_IO;
    }
    Ok(Self {
      ctx,
      io: ManuallyDrop::new(io),
      pb,
      latch,
      seeks,
      phase: OutputPhase::Building,
      _sink: PhantomData,
    })
  }

  /// Turns libavformat's automatic bitstream filters on or off
  /// (`AVFMT_FLAG_AUTO_BSF`, on by default: `libavformat/options_table.h`
  /// 42 in FFmpeg 9.0.1). With them on, a muxer may filter a stream's
  /// packets before it writes them (`check_bitstream`, `mux.c` 1056–1073)
  /// — strip AAC's ADTS headers, join VP9 frames into superframes or PGS
  /// segments into display sets — so what reaches the file is not what
  /// was handed over; with them off, every packet is written as it was
  /// handed over, or refused by the muxer.
  ///
  /// Only while the context is being built: libavformat decides at the
  /// first packet.
  pub fn set_automatic_filters(&mut self, on: bool) -> Result<(), OutputError> {
    self.require(OutputCall::AutomaticFilters, OutputPhase::Building)?;
    // SAFETY: a plain integer field of the live context, read by
    // libavformat only when it writes a packet, after the header.
    unsafe {
      let ctx = self.ctx.as_ptr();
      if on {
        (*ctx).flags |= AVFMT_FLAG_AUTO_BSF;
      } else {
        (*ctx).flags &= !AVFMT_FLAG_AUTO_BSF;
      }
    }
    Ok(())
  }

  /// Adds one stream, as `spec` describes it, and answers its index.
  ///
  /// Only while the context is being built. The texts are refused by
  /// name before anything is added; a failure after the stream was
  /// added leaves the context [`OutputPhase::Failed`], since libavformat
  /// would write a header for a stream half made.
  pub fn add_stream(&mut self, spec: &StreamSpec<'_>) -> Result<usize, OutputError> {
    self.require(OutputCall::AddStream, OutputPhase::Building)?;
    let language = optional_text(spec.language, OutputText::Language)?;
    let filename = optional_text(spec.filename, OutputText::Filename)?;
    let mime_type = optional_text(spec.mime_type, OutputText::MimeType)?;
    if let Some(bytes) = spec.extradata
      && bytes.len() > (c_int::MAX - AV_INPUT_BUFFER_PADDING_SIZE) as usize
    {
      return Err(OutputError::ExtradataTooLarge { bytes: bytes.len() });
    }
    // Rebuilt before the stream exists, so its failure adds nothing.
    let parameters = spec.ticket.rebuild()?;

    let ctx = self.ctx.as_ptr();
    // SAFETY: `ctx` is live and still being built; a null codec is
    // allowed. libavformat answers the new stream, owned by the context,
    // or null.
    let st = unsafe { avformat_new_stream(ctx, ptr::null()) };
    if st.is_null() {
      return Err(OutputError::Ffmpeg(ffmpeg_next::Error::Other {
        errno: libc::ENOMEM,
      }));
    }
    // From here a failure leaves a stream libavformat would write.
    self.phase = OutputPhase::Failed;

    // SAFETY: `st` is the live stream just added, its `codecpar`
    // allocated by `avformat_new_stream`; `parameters` owns a live
    // `AVCodecParameters` (rebuilt and null-checked by the ticket). The
    // copy allocates the stream's own extradata, side data and channel
    // map.
    let status = unsafe { avcodec_parameters_copy((*st).codecpar, parameters.as_ptr()) };
    if status < 0 {
      return Err(self.fault(status));
    }
    drop(parameters);
    // SAFETY: `st` is live, and its `codecpar` is the stream's own,
    // filled just above; nothing else holds either.
    let par = unsafe { (*st).codecpar };

    match spec.media {
      StreamMedia::Timed => {}
      // SAFETY: `par` is the stream's own parameters; the media type is
      // stored as the integer it is, so no bindgen enum is formed.
      StreamMedia::Attachment => unsafe {
        write_unaligned(
          addr_of_mut!((*par).codec_type).cast::<i32>(),
          MEDIA_ATTACHMENT,
        );
      },
      // SAFETY: as above.
      StreamMedia::Cover => unsafe {
        write_unaligned(addr_of_mut!((*par).codec_type).cast::<i32>(), MEDIA_VIDEO);
      },
    }
    if let Some(bytes) = spec.extradata {
      // SAFETY: `par` is the stream's own; `bytes` fits the `int` size
      // with its padding (judged above).
      unsafe { replace_extradata(par, bytes) }.map_err(|code| self.fault(code))?;
    }
    // SAFETY: `par` is the stream's own, and `ctx` the live context
    // whose static muxer's tag table is read.
    unsafe { settle_codec_tag(ctx, par) };
    if spec.media == StreamMedia::Timed {
      // SAFETY: `par` is the stream's own; plain fields.
      unsafe { stream_copy_audio_rule(par) };
    }

    let disposition = match spec.media {
      StreamMedia::Cover => AV_DISPOSITION_ATTACHED_PIC,
      _ => spec.disposition,
    };
    // SAFETY: `st` is live and the context is still being built: these
    // are the plain fields libavformat reads at the header — the time
    // base it starts from, the disposition, the sample aspect ratio the
    // header checks against the parameters' (`mux.c`'s init_muxer), and
    // the frame rate.
    unsafe {
      (*st).time_base = rational(spec.time_base);
      (*st).disposition = disposition;
      (*st).sample_aspect_ratio = (*par).sample_aspect_ratio;
      if let Some(rate) = spec.frame_rate {
        (*st).avg_frame_rate = rational(rate);
      }
    }
    for (key, value) in [
      (c"language", &language),
      (c"filename", &filename),
      (c"mimetype", &mime_type),
    ] {
      if let Some(value) = value {
        // SAFETY: `st` is live; `av_dict_set` copies key and value (no
        // `DONT_STRDUP` flag), both NUL-terminated and live for the call.
        let status = unsafe {
          av_dict_set(
            addr_of_mut!((*st).metadata),
            key.as_ptr(),
            value.as_ptr(),
            0,
          )
        };
        if status < 0 {
          return Err(self.fault(status));
        }
      }
    }

    self.phase = OutputPhase::Building;
    // SAFETY: a plain integer field of the live stream.
    Ok(unsafe { (*st).index } as usize)
  }

  /// Adds one chapter: `start` and `end` counted in `time_base`.
  ///
  /// Only while the context is being built. libavformat does not judge a
  /// chapter here; a muxer that cannot write one fails its header
  /// instead, so a caller judges the span first.
  pub fn add_chapter(
    &mut self,
    id: i64,
    time_base: Timebase,
    start: i64,
    end: i64,
    title: Option<&str>,
  ) -> Result<(), OutputError> {
    self.require(OutputCall::AddChapter, OutputPhase::Building)?;
    let title = optional_text(title, OutputText::ChapterTitle)?;
    let ctx = self.ctx.as_ptr();
    // SAFETY: a plain integer field of the live context.
    let count = unsafe { (*ctx).nb_chapters };
    let Ok(mut count) = c_int::try_from(count) else {
      return Err(OutputError::TooManyChapters);
    };
    if count == c_int::MAX {
      return Err(OutputError::TooManyChapters);
    }
    // SAFETY: a zeroed allocation the size of an `AVChapter`, the way
    // libavformat allocates its own (`avpriv_new_chapter`); null on
    // failure, checked before any write.
    let chapter = unsafe { av_mallocz(size_of::<AVChapter>()) }.cast::<AVChapter>();
    if chapter.is_null() {
      return Err(OutputError::Ffmpeg(ffmpeg_next::Error::Other {
        errno: libc::ENOMEM,
      }));
    }
    // SAFETY: `chapter` is the live, zeroed allocation just made, and
    // nothing else holds it; plain fields.
    unsafe {
      (*chapter).id = id;
      (*chapter).time_base = rational(time_base);
      (*chapter).start = start;
      (*chapter).end = end;
    }
    if let Some(title) = &title {
      // SAFETY: `chapter` is live and its metadata null; `av_dict_set`
      // copies the NUL-terminated key and value.
      let status = unsafe {
        av_dict_set(
          addr_of_mut!((*chapter).metadata),
          c"title".as_ptr(),
          title.as_ptr(),
          0,
        )
      };
      if status < 0 {
        // SAFETY: the chapter is still ours alone: its dictionary and the
        // chapter itself are freed, each exactly once.
        unsafe { free_chapter(chapter) };
        return Err(self.fault(status));
      }
    }
    // SAFETY: `chapters` is the context's array of `nb_chapters` pointers
    // (null while empty), allocated by libavutil; `count` mirrors that
    // count as the `int` the call takes. `av_dynarray_add_nofree` grows
    // the array and appends `chapter`, or leaves both untouched and fails.
    let status = unsafe {
      av_dynarray_add_nofree(
        addr_of_mut!((*ctx).chapters).cast::<c_void>(),
        &mut count,
        chapter.cast::<c_void>(),
      )
    };
    if status < 0 {
      // SAFETY: the array did not take the chapter, so it is still ours.
      unsafe { free_chapter(chapter) };
      return Err(self.fault(status));
    }
    // SAFETY: the array now holds `count` chapters; the context frees each
    // with it (`avformat_free_context`).
    unsafe { (*ctx).nb_chapters = count as c_uint };
    Ok(())
  }

  /// Writes the header: the end of building, the start of writing.
  ///
  /// A failure leaves the context [`OutputPhase::Failed`]: libavformat
  /// takes no packet and no trailer after a header it did not write — nor
  /// after one whose sink failed a seek, which nothing more reaches.
  pub fn write_header(&mut self) -> Result<(), OutputError> {
    self.require(OutputCall::WriteHeader, OutputPhase::Building)?;
    // SAFETY: `ctx` is live, its `oformat` and `pb` set and its streams
    // complete; the muxer's options are its defaults (null dictionary).
    let status = unsafe { avformat_write_header(self.ctx.as_ptr(), ptr::null_mut()) };
    if let Err(error) = self.settled(status) {
      self.phase = OutputPhase::Failed;
      return Err(error);
    }
    self.phase = OutputPhase::Writing;
    Ok(())
  }

  /// Writes one packet to `stream`, with `times` in place of the
  /// carrier's own timestamps.
  ///
  /// The packet is built on the **copy road** — its payload and side data
  /// copied into an allocation libavformat alone then holds — and handed
  /// to `av_interleaved_write_frame`, which takes it and may keep it
  /// queued past the call. Nothing the caller holds is lent.
  pub fn write_packet<C: crate::FfmpegCarrier + crate::CarrierOps>(
    &mut self,
    stream: usize,
    packet: OutputPacket<'_, C>,
    times: PacketTimes,
    limits: PacketLimits,
  ) -> Result<(), OutputError> {
    self.require(OutputCall::WritePacket, OutputPhase::Writing)?;
    let index = c_int::try_from(stream).map_err(|_| OutputError::NoSuchStream { stream })?;
    self.stream(stream)?;
    // SAFETY: `BodyRoute::Copy` — every builder copies the payload into
    // a packet of its own, so there is nothing to prove; the packet goes
    // to libavformat, which may keep it.
    let mut av_packet: Packet = unsafe {
      match packet {
        OutputPacket::Video(p) => build_video_packet::<C>(p, limits, BodyRoute::Copy),
        OutputPacket::Audio(p) => build_audio_packet::<C>(p, limits, BodyRoute::Copy),
        OutputPacket::Subtitle(p) => build_subtitle_packet::<C>(p, limits, BodyRoute::Copy),
        OutputPacket::Data(p) => build_data_packet::<C>(p, limits, BodyRoute::Copy),
        OutputPacket::Cover(p) => build_attachment_packet::<C>(p, limits, BodyRoute::Copy),
      }
    }?;
    av_packet.set_pts(times.pts);
    av_packet.set_dts(times.dts);
    av_packet.set_duration(times.duration);
    // SAFETY: `av_packet` owns a live `AVPacket`; `stream_index` is a
    // plain field, set to a stream of this context.
    unsafe { (*av_packet.as_mut_ptr()).stream_index = index };
    // SAFETY: `ctx` is live and writing; `av_packet` is a refcounted
    // packet this call built, which `av_interleaved_write_frame` takes
    // over — it moves the reference into its queue and leaves the packet
    // blank, which then drops here.
    let status = unsafe { av_interleaved_write_frame(self.ctx.as_ptr(), av_packet.as_mut_ptr()) };
    drop(av_packet);
    self.settled(status)
  }

  /// Writes the trailer: the packets still queued for interleaving, then
  /// whatever the muxer writes last.
  ///
  /// Asked once: the context is [`OutputPhase::Finished`] after it,
  /// whatever it answers, since libavformat ends the muxer either way.
  pub fn write_trailer(&mut self) -> Result<(), OutputError> {
    self.require(OutputCall::WriteTrailer, OutputPhase::Writing)?;
    // SAFETY: `ctx` is live and its header was written; this is the one
    // trailer call the phase allows.
    let status = unsafe { av_write_trailer(self.ctx.as_ptr()) };
    self.phase = OutputPhase::Finished;
    self.settled(status)
  }

  /// Frees the context and hands the sink back, flushed — and only when
  /// every byte libavformat wrote reached it.
  ///
  /// The bytes still buffered in the I/O context are written through
  /// the guard first. A write into the sink that failed — then or at any
  /// earlier point — is reported ([`OutputError::SinkWrite`]): the I/O
  /// context keeps the first failed write's error and writes nothing
  /// after it, so the sink is missing bytes however its own `flush`
  /// answers. Then the sink's `flush` runs and its error is reported
  /// ([`OutputError::Sink`]). A panic the sink raised, at any point, is
  /// reported before either ([`OutputError::SinkPanic`]): a sink that
  /// panicked is not called again; whatever state it is in, the error
  /// says so. A seek it failed, at any point, comes next
  /// ([`OutputError::SinkSeek`]): what libavformat placed after it is
  /// missing, however the rest went.
  ///
  /// **From any phase.** Called before the trailer — while the context is
  /// being built, while it is writing, or after a step failed — it frees
  /// the context exactly as dropping it does (from
  /// [`OutputPhase::Writing`], the muxer's `deinit` and the packets still
  /// queued for interleaving go with it), and hands back the sink holding
  /// what was written: a container without its trailer, answered `Ok`
  /// when nothing failed. No phase is refused: this is how a caller takes
  /// its sink back from wherever the context is, and the muxer above
  /// writes the trailer first.
  pub fn into_sink(self) -> Result<W, OutputError> {
    let this = ManuallyDrop::new(self);
    // SAFETY: `this` is never dropped, so each field is moved out exactly
    // once here, and none is used through `this` again.
    let (ctx, io, latch, seeks) = unsafe {
      (
        this.ctx,
        ptr::read(&this.io),
        ptr::read(&this.latch),
        ptr::read(&this.seeks),
      )
    };
    // SAFETY: the context is this value's, freed exactly once — before
    // the I/O context its `pb` points at, which `AVFMT_FLAG_CUSTOM_IO`
    // keeps libavformat from touching.
    unsafe { avformat_free_context(ctx.as_ptr()) };
    let mut io = ManuallyDrop::into_inner(io);
    let pb = io.as_mut_ptr();
    // SAFETY: `pb` is the live I/O context `io` owns, a write context
    // (`open` built it over the sink), and no format context points at it
    // any more. `avio_flush` writes what is still buffered through the
    // guarded sink; `error` is the plain field in which `writeout` keeps
    // the first failed write's code (`aviobuf.c` 136–166 in FFmpeg
    // 9.0.1), read before `into_inner` frees the context. ffmpeg-next's
    // `into_inner` flushes once more, and finds nothing buffered.
    let written = unsafe {
      avio_flush(pb);
      (*pb).error
    };
    let guarded = match io.into_inner::<GuardedWriter<W>>() {
      Ok(guarded) => guarded,
      // `open` built the I/O context over exactly this type.
      Err(_) => unreachable!("the I/O context holds the guarded sink it was built over"),
    };
    let mut guarded = guarded;
    let flushed = guarded.flush();
    if let Some(message) = latch.message() {
      return Err(OutputError::SinkPanic { message });
    }
    if let Some(error) = seeks.failure() {
      return Err(OutputError::SinkSeek(error));
    }
    if written < 0 {
      return Err(OutputError::SinkWrite(ffmpeg_next::Error::from(written)));
    }
    flushed.map_err(OutputError::Sink)?;
    Ok(guarded.into_inner())
  }
}

impl<W> Drop for Output<W> {
  fn drop(&mut self) {
    // SAFETY: the context is this value's, freed exactly once — here, or
    // in `into_sink`, which does not drop `self`. Freed before the I/O
    // context: `avformat_free_context` may run the muxer's `deinit`,
    // which must find `pb` alive, and leaves `pb` alone.
    unsafe { avformat_free_context(self.ctx.as_ptr()) };
    // SAFETY: dropped exactly once, after the context. Its `Drop` flushes
    // the buffered bytes and the sink through the guard, then frees.
    unsafe { ManuallyDrop::drop(&mut self.io) };
  }
}

impl<W> core::fmt::Debug for Output<W> {
  fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
    f.debug_struct("Output")
      .field("container", &self.container())
      .field("phase", &self.phase)
      .field("streams", &self.streams())
      .finish_non_exhaustive()
  }
}

/// An optional text as a C string, refused by name if it holds a NUL.
fn optional_text(text: Option<&str>, which: OutputText) -> Result<Option<CString>, OutputError> {
  text
    .map(|text| CString::new(text).map_err(|_| OutputError::Nul { text: which }))
    .transpose()
}

/// A [`Timebase`] as libavformat's `AVRational`.
fn rational(timebase: Timebase) -> AVRational {
  AVRational {
    num: timebase.num(),
    den: timebase.den().get(),
  }
}

/// Replaces `par`'s extradata with `bytes`, padded with the zeroes
/// libavcodec reads past it, and answers an FFmpeg error code on
/// failure.
///
/// # Safety
///
/// `par` must be a live `AVCodecParameters` nothing else holds, whose
/// `extradata` is null or libavutil's allocation; `bytes.len()` plus the
/// padding must fit a C `int`.
unsafe fn replace_extradata(par: *mut AVCodecParameters, bytes: &[u8]) -> Result<(), c_int> {
  // SAFETY: `extradata` is null or libavutil's own (this function's
  // contract); `av_freep` frees it and nulls the field.
  unsafe {
    av_freep(addr_of_mut!((*par).extradata).cast::<c_void>());
    (*par).extradata_size = 0;
  }
  if bytes.is_empty() {
    return Ok(());
  }
  let size = bytes.len() + AV_INPUT_BUFFER_PADDING_SIZE as usize;
  // SAFETY: a zeroed allocation of `size` bytes, null on failure; the
  // zeroes past `bytes` are the padding.
  let data = unsafe { av_mallocz(size) }.cast::<u8>();
  if data.is_null() {
    return Err(ffmpeg_next::ffi::AVERROR(libc::ENOMEM));
  }
  // SAFETY: `data` holds `size >= bytes.len()` bytes, a fresh allocation
  // that cannot overlap `bytes`; then attached to `par`, which frees it
  // with itself. `bytes.len()` fits a C `int` (this function's contract).
  unsafe {
    ptr::copy_nonoverlapping(bytes.as_ptr(), data, bytes.len());
    (*par).extradata = data;
    (*par).extradata_size = bytes.len() as c_int;
  }
  Ok(())
}

/// FFmpeg's own stream-copy rule for a codec tag (`fftools/
/// ffmpeg_mux_init.c` 1031–1039 in FFmpeg 9.0.1): keep the source's tag
/// where the muxer has no tag table, where its table maps that tag back
/// to the same codec, or where it names no tag for the codec at all;
/// otherwise clear it, so the muxer picks its own — the header refuses a
/// tag its table gives another codec (`mux.c`'s `validate_codec_tag`).
///
/// # Safety
///
/// `ctx` must be a live context with its muxer set, and `par` a live
/// `AVCodecParameters` nothing else holds.
unsafe fn settle_codec_tag(ctx: *mut AVFormatContext, par: *mut AVCodecParameters) {
  // SAFETY: plain fields of the live parameters; the codec id is read as
  // the integer it is stored as.
  let (tag, codec) = unsafe {
    (
      (*par).codec_tag,
      read_unaligned(addr_of!((*par).codec_id).cast::<i32>()),
    )
  };
  if tag == 0 {
    return;
  }
  // SAFETY: the static muxer's `codec_tag`: null, or a null-terminated
  // list of libavformat's static tag tables.
  let tables = unsafe { (*(*ctx).oformat).codec_tag };
  if tables.is_null() {
    return;
  }
  let mut found: c_uint = 0;
  // SAFETY: `tables` is the muxer's own list; the redeclared shims take
  // the codec id and tag as plain integers, so no bindgen enum is formed,
  // and search the tables without indexing them by either.
  let keep = unsafe {
    c_shims::av_codec_get_id(tables, tag) == codec
      || c_shims::av_codec_get_tag2(tables, codec, &mut found) == 0
  };
  if !keep {
    // SAFETY: a plain field of the live parameters.
    unsafe { (*par).codec_tag = 0 };
  }
}

/// FFmpeg's own stream-copy rule for an audio stream's `block_align`
/// (`fftools/ffmpeg_mux_init.c` 1076–1083 in FFmpeg 9.0.1): an MP3
/// stream's value of 1, 576 or 1152 is a frame size some demuxers report
/// there, and an AC-3 stream's is none a muxer should write; both are
/// cleared.
///
/// # Safety
///
/// `par` must be a live `AVCodecParameters` nothing else holds.
unsafe fn stream_copy_audio_rule(par: *mut AVCodecParameters) {
  // SAFETY: plain fields of the live parameters, the enums read as the
  // integers they are stored as.
  unsafe {
    let media = read_unaligned(addr_of!((*par).codec_type).cast::<i32>());
    let codec = read_unaligned(addr_of!((*par).codec_id).cast::<i32>());
    if media != MEDIA_AUDIO {
      return;
    }
    let block_align = (*par).block_align;
    if (codec == CodecId::MP3.raw() && matches!(block_align, 1 | 576 | 1152))
      || codec == CodecId::AC3.raw()
    {
      (*par).block_align = 0;
    }
  }
}

/// Frees a chapter this module allocated and no array took.
///
/// # Safety
///
/// `chapter` must be a live `av_mallocz` allocation of an `AVChapter`
/// that nothing else holds, its `metadata` null or libavutil's own.
unsafe fn free_chapter(chapter: *mut AVChapter) {
  // SAFETY: this function's contract: the dictionary and the chapter are
  // libavutil's allocations, each freed exactly once.
  unsafe {
    av_dict_free(addr_of_mut!((*chapter).metadata));
    av_free(chapter.cast::<c_void>());
  }
}

/// A text the context may hand libavformat, as the bytes it reads.
///
/// Kept for the laws: what `av_dict_set` was given comes back out of the
/// stream's own dictionary.
#[cfg(test)]
fn metadata_value(
  dict: *const ffmpeg_next::ffi::AVDictionary,
  key: &core::ffi::CStr,
) -> Option<String> {
  // SAFETY: `dict` is null or a live dictionary of the test's own
  // context; the entry's value is a NUL-terminated copy it owns.
  unsafe {
    let entry = ffmpeg_next::ffi::av_dict_get(dict, key.as_ptr(), ptr::null(), 0);
    if entry.is_null() {
      return None;
    }
    let value: *const core::ffi::c_char = (*entry).value;
    Some(
      core::ffi::CStr::from_ptr(value)
        .to_string_lossy()
        .into_owned(),
    )
  }
}

#[cfg(test)]
mod tests;
