//! What the `unsafe` crossings read through the pointers they are handed
//! — one section per FFmpeg object, stated once.
//!
//! Every `unsafe fn` of this crate (and of `mediadecode-ffmpeg`, which
//! re-exports them) names, in its own `# Safety` section, each pointer it
//! dereferences, directly or through the helpers it calls, and links the
//! section below that says what such a pointer must hold. A wrapper
//! forwards its worker's contract whole, by naming the same sections.
//!
//! # Reading
//!
//! A pointer a crossing **reads** — and every element of an array it
//! reads, for the whole declared count — must be, for the duration of the
//! call:
//!
//! - **live**: pointing at memory allocated for its type and not yet
//!   freed;
//! - **aligned** for its type;
//! - **initialised** in every byte the crossing reads;
//! - **unwritten**: nothing else — no other alias, no other thread —
//!   writes through it while the call reads it.
//!
//! A field marked *plain* below is an integer, or an open C enum, that a
//! crossing reads by value and judges itself: it may hold any value, and
//! an enum is read as the integer it is stored as, never as a Rust enum.
//! A field marked *checked* is a pointer or count whose consistency a
//! crossing establishes before it relies on it — it may be null, or
//! disagree with its neighbours, and is refused rather than trusted.
//! Only what remains carries an obligation, and that is what each
//! section lists.
//!
//! # What a crossing hands back
//!
//! On the **owned** lane every byte a returned carrier holds is a copy,
//! and the source may be dropped or written at once. On the **view** lane
//! a returned carrier may be a window onto the source's own
//! `AVBufferRef`, holding a reference of its own: the buffer outlives
//! the source by that reference, and nothing may write through those
//! bytes while the carrier — or any clone of it — lives.
//!
//! # Every exported byte initialised
//!
//! An object's **exported bytes** are the ones it offers a reader: each
//! plane of a frame — every row its format, width and height describe,
//! not the padding out to `linesize`, which no crossing reads — and each
//! of its side-data entries' bytes; a packet's payload and each side-data
//! entry's bytes; a subtitle rect's text, and a bitmap rect's plane and
//! palette. Every one of them is initialised before the
//! object reaches a crossing — written by the library that produced it,
//! or by its caller. A crossing reads only exported bytes, and reading one
//! that was never written is undefined behaviour whatever is done with it
//! afterwards, so no check inside a crossing can stand in for this.
//!
//! The objects libav* hands out meet it: a frame `avcodec_receive_frame`
//! filled (libavcodec's frame pools are zeroed when they are allocated,
//! `libavcodec/get_buffer.c` 122–125 and 145–148 in FFmpeg 9.0.1, and a
//! decoder writes the picture over them), a frame
//! `av_hwframe_transfer_data` or `sws_scale` wrote, a packet
//! `av_read_frame` or `avcodec_receive_packet` filled, a subtitle
//! `avcodec_decode_subtitle2` filled — and so do the ones this crate
//! builds from bytes it owns.
//!
//! **`ffmpeg_next`'s safe API builds objects that do not.**
//! `frame::Video::new` and `frame::Audio::new` allocate their planes with
//! `av_frame_get_buffer`, which leaves them unwritten (its buffer is
//! `av_buffer_alloc`, `libavutil/frame.c` 122); `Packet::new` allocates
//! its payload with `av_new_packet` and `Packet::grow` extends one with
//! `av_grow_packet`, both of which zero only the padding. A borrow of an
//! `ffmpeg_next` object therefore cannot vouch for this section, and **no
//! safe function of this crate reads the storage of an `ffmpeg_next`
//! frame, packet or subtitle**: every conversion from one is an `unsafe
//! fn` whose caller vouches for this section and for the object's own
//! below. Those conversions keep their named refusals — a null frame, a
//! custom channel map, a bitmap rect's plane — for what their contracts
//! do not ask the caller to vouch for.
//!
//! # `AVBufferRef`
//!
//! - the `AVBufferRef` itself, read;
//! - its `buffer`, live: its reference count is read
//!   (`av_buffer_get_ref_count`), and on the view lane taken
//!   (`av_buffer_ref`);
//! - its `data`, valid for reads of `size` bytes. The bytes a crossing
//!   copies or lends are initialised; the rest of the buffer is never
//!   read.
//!
//! # `AVPacket`
//!
//! - `buf`: null, or an [`AVBufferRef`](#avbufferref) — the payload's
//!   owner;
//! - the `size` payload bytes at `data`, when `buf` is non-null and
//!   `size > 0`: initialised, as
//!   [every exported byte](#every-exported-byte-initialised) is. Their
//!   extent is *checked* against `buf`'s
//!   own `data` and `size` before anything is read;
//! - `side_data`, when `side_data_elems > 0`: valid for
//!   `side_data_elems` `AVPacketSideData` entries (the count is *checked*
//!   against this crate's cap and the array against null before the first
//!   entry is read), and each entry's `data`, when its `size > 0`, valid
//!   for and initialised in `size` bytes;
//! - *plain*: `flags`, `pts`, `dts`, `duration`, `stream_index`, each
//!   entry's `type`.
//!
//! **A packet handed to a decoder owes two things more**, because
//! libavcodec reads past the payload and keeps what it is sent. Every
//! road that hands an `AVPacket` to libavcodec — `mediadecode-ffmpeg`'s
//! raw `VideoDecoder::send_av_packet`, and the seams behind it — asks its
//! caller for both:
//!
//! - **the padding**: the `AV_INPUT_BUFFER_PADDING_SIZE` bytes after the
//!   payload lie inside `buf` and are initialised to zero — or are the
//!   padding libavformat delivered the packet with, unmodified (see
//!   below). Bitstream readers read 32 or 64 bits at a time past the end
//!   of what they are given;
//! - **no writer**: neither the payload nor that padding is written while
//!   the decoder may still read them — `avcodec_send_packet` keeps a
//!   reference to a refcounted packet's buffer past the call
//!   (`libavcodec/avcodec.h` 2333–2335 in FFmpeg 9.0.1), and a
//!   frame-threaded decoder reads it on another thread — until the
//!   decoder is flushed or dropped.
//!
//! A packet `av_new_packet` allocated meets the padding clause:
//! `Packet::new` and `Packet::copy`, an `av_packet_make_writable` copy,
//! and this crate's builders all zero it (`packet_alloc`,
//! `libavcodec/packet.c` 83–96). A packet `av_read_frame` delivered
//! carries libavformat's own padding — zeroes where libavformat allocated
//! the packet, or, where a parser split it out of a larger one in place
//! (`libavformat/demux.c` 1253–1262), the bytes that follow it in that
//! one — initialised either way, and what libavcodec is built to read.
//!
//! **The library never vouches for a caller's buffer.** A conversion from
//! a caller's packet reads no byte past the payload and claims nothing
//! about those bytes: its carrier is marked foreign, and the view lane's
//! send hands a decoder a padded, zeroed copy of it rather than the
//! caller's buffer. Only a packet this crate's demuxer took from
//! libavformat — a demuxed packet, a stream's parked picture — reaches a
//! decoder by reference: the claim is the packet's provenance
//! (`buffer::PayloadProvenance`), never what its buffer holds.
//!
//! An `ffmpeg_next::Packet` keeps its `AVPacket` inline, so the packet
//! itself is always live and aligned behind a borrow of one, and its
//! `buf`, `data`, `size` and `side_data` are written only by libavformat,
//! libavcodec and ffmpeg-next's own allocating calls (`copy`, `grow`,
//! `shrink`), which keep them in step. What the borrow cannot vouch for is
//! the payload's initialisation: `Packet::new` and `Packet::grow` leave
//! bytes unwritten (see [above](#every-exported-byte-initialised)) —
//! bytes ffmpeg-next's own safe `Packet::data` already lends as `&[u8]`,
//! an upstream defect this crate records and does not inherit. So every
//! conversion from an `ffmpeg_next::Packet` is `unsafe`, and its caller
//! vouches that the packet came from a road that writes its payload:
//! `av_read_frame`, an encoder, `Packet::copy`, or this crate's own
//! builders.
//!
//! # A picture `AVFrame`
//!
//! - `buf[0..8]`: each null, or an [`AVBufferRef`](#avbufferref);
//! - each plane's rows — `data[i]` plus `linesize[i]` per row, for the
//!   plane's height, `row_bytes` read from each — initialised, as
//!   [every exported byte](#every-exported-byte-initialised) is. The planes
//!   are *checked*: a plane is read only after a buffer in `buf[]` is
//!   found whose `data .. data + size` contains every row it reads;
//! - `side_data`, when `nb_side_data > 0` and the array is not null:
//!   valid for `nb_side_data` pointers — at most the first 64 are
//!   read — each null (skipped) or a live
//!   `AVFrameSideData` whose `data` is valid for and initialised in its
//!   `size` bytes;
//! - *plain*: `format`, `width`, `height`, `linesize[]`, `crop_*`,
//!   `color_primaries`, `color_trc`, `colorspace`, `color_range`,
//!   `chroma_location`, `sample_aspect_ratio`, `pict_type`, `flags`,
//!   `pts`, `duration`, `best_effort_timestamp`, each side-data entry's
//!   `type`.
//!
//! # An audio `AVFrame`
//!
//! - `buf[0..8]`: each null, or an [`AVBufferRef`](#avbufferref);
//! - each plane's samples — `data[i]`, for the bytes `nb_samples` of the
//!   frame's format occupy in that plane — initialised, as
//!   [every exported byte](#every-exported-byte-initialised) is. The planes are
//!   *checked* as a picture's are, against `buf[]`;
//! - `ch_layout`: an [`AVChannelLayout`](#avchannellayout), read in
//!   place;
//! - `side_data`, as for a picture;
//! - *plain*: `format`, `sample_rate`, `nb_samples`, `linesize[0]`, `pts`,
//!   `duration`, `best_effort_timestamp`.
//!
//! # `AVChannelLayout`
//!
//! - `u.map`, when `order` is `AV_CHANNEL_ORDER_CUSTOM`, `nb_channels`
//!   is a count the reader accepts — `1..=65_535` for the layout
//!   readers, `1..=255` for an audio frame's conversion — and the map is
//!   not null (both *checked*): valid for and initialised in `nb_channels`
//!   `AVChannelCustom` entries. An entry's `id` is read as an integer,
//!   its `name` as sixteen bytes, and its `opaque` as a pointer value
//!   that is never dereferenced;
//! - *plain*: `order`, `nb_channels`, `u.mask`.
//!
//! A layout FFmpeg filled — `av_channel_layout_copy`,
//! `av_channel_layout_custom_init` — always holds its map for its count.
//! Nothing safe can establish that of a layout it did not fill, which is
//! why the borrowed conversions — whose contracts do not ask for it —
//! refuse a custom map rather than read one.
//!
//! # `AVSubtitle`
//!
//! - `rects`, when `num_rects > 0`: valid for `num_rects` pointers (null
//!   is *checked*; at most the first 64 are read), each null or a live
//!   `AVSubtitleRect`;
//! - each rect's `type` (*plain*), read first and alone. **Every other
//!   field is read only under its own kind**, so a field of another kind
//!   is never read and need not be initialised:
//!   - a text rect's `text` and an ASS rect's `ass`: null, or readable
//!     byte by byte up to and including the NUL terminator or for 65,537
//!     bytes, whichever comes first — the reader stops at whichever it
//!     meets;
//!   - a bitmap rect's `data[0]`: null, or valid for and initialised in
//!     `linesize[0] × h` bytes, when that product is within the reader's
//!     caps (16 MiB a rect; a larger one is refused before it is read);
//!     its `data[1]`: null, or valid for and initialised in 1,024 bytes —
//!     the 256-entry RGBA palette, read whole; and *plain*, its `x`, `y`,
//!     `w`, `h`, `linesize[0]`;
//! - *plain*: `num_rects`, `pts`, `start_display_time`,
//!   `end_display_time`.
//!
//! An `ffmpeg_next::Subtitle` keeps its `AVSubtitle` inline, so the
//! top-level struct is live behind a borrow of one; the rects and what
//! they point at are the list above.
//!
//! # `AVCodecParameters`
//!
//! - `extradata`: null, or valid for and initialised in `extradata_size`
//!   bytes, for a reader that copies it (one that only measures reads the
//!   pointer value and the size);
//! - `coded_side_data`, when `nb_coded_side_data > 0`: valid for
//!   `nb_coded_side_data` `AVPacketSideData` entries — each entry's `size`
//!   is read — and, for a reader that copies them, each entry's `data`
//!   valid for and initialised in its `size` bytes;
//! - `ch_layout`: an [`AVChannelLayout`](#avchannellayout), read in
//!   place;
//! - *plain*: every other field a reader reads.
//!
//! An `ffmpeg_next::codec::Parameters` holds a **pointer**, and its safe
//! constructor stores whatever `avcodec_parameters_alloc` answered —
//! null after an allocation failure. The safe readers of this crate check
//! for that before they read a field.
//!
//! # `AVFormatContext`
//!
//! - `iformat`: null, or a live `AVInputFormat` whose `name` and
//!   `long_name` are each null or a NUL-terminated string that stays
//!   unwritten for the life of the process — libavformat's own static
//!   tables (read for at most 1,024 bytes, stopping at the NUL).
//!
//! # Writing a container
//!
//! The write side's crossings take no pointer from a caller, so none of
//! them is an `unsafe fn`: [`Output`](crate::output::Output) owns every
//! pointer it hands libavformat, and what libavformat is owed is
//! established inside it. Stated here once all the same, because it is a
//! contract — the one this crate keeps with libavformat:
//!
//! - **the context**: allocated by `avformat_alloc_output_context2`,
//!   freed exactly once by `avformat_free_context` — always before the
//!   I/O context its `pb` points at, since the muxer's `deinit` may run
//!   inside the free and must find `pb` alive — and marked
//!   `AVFMT_FLAG_CUSTOM_IO`, so libavformat neither closes nor frees that
//!   I/O context;
//! - **the order**: streams and chapters only before the header, packets
//!   only between the header and the trailer, the trailer at most once,
//!   and nothing after a header that failed. libavformat leaves each of
//!   these undefined — a second `av_write_trailer` runs a muxer's trailer
//!   over state the first one freed — so the context keeps the order at
//!   run time and refuses a call out of it by name;
//! - **the packets**: built from the portable carriers on the **copy
//!   road**, their payload and side data in allocations of their own with
//!   the padding zeroed (`av_new_packet`), and handed over whole —
//!   `av_interleaved_write_frame` takes the packet's reference and may
//!   keep it queued past the call, so no buffer a caller holds is ever
//!   lent to it;
//! - **the parameters**: rebuilt from a
//!   [`CodecTicket`](crate::ticket::CodecTicket) and copied into the
//!   stream with `avcodec_parameters_copy`. An attachment's file replaces
//!   the extradata in a fresh allocation with the padding zeroed, and
//!   codec ids and media types are written and read as the integers they
//!   are stored as, so no bindgen enum is formed from a value the linked
//!   library may know and the bindings do not;
//! - **the sink**: driven from `extern "C"` callbacks under the
//!   [writer guard](crate::writer_guard), so a panic never unwinds into
//!   C — nor an error's payload: every error the sink returns is reduced
//!   to its OS code or kind inside the guard's `catch_unwind`, its
//!   payload dropped there, because ffmpeg-next drops a returned error
//!   inside the callback. A sink that panicked, or whose error's payload
//!   did, is latched, and never called again; a write it failed is
//!   read from the I/O context's `error` when the sink is handed back,
//!   after the last flush, so a sink missing bytes is reported, never
//!   returned as a whole one; and a positioning seek it failed, which
//!   libavformat keeps no record of, is kept by the guard, which writes
//!   and seeks nothing after it, and named by every call that reached
//!   libavformat, whatever libavformat answered.
//!
//! # `AVOutputFormat`
//!
//! - the muxer itself: one `av_guess_format` answered, which points into
//!   libavformat's static tables and lives for the process;
//! - `name`: a NUL-terminated literal in those tables, read for at most
//!   256 bytes;
//! - `codec_tag`: null, or a null-terminated list of static tag tables,
//!   searched by `av_codec_get_id` and `av_codec_get_tag2` with the codec
//!   id and the tag as plain integers;
//! - *plain*: `flags`.
//!
//! # `AVCodecContext`, for the callbacks
//!
//! - `opaque`: null, or the [`CallbackState`](#callbackstate) that
//!   [`install_allocator_judge`](crate::ffi::install_allocator_judge)
//!   installed;
//! - `hw_device_ctx`: null, or a live `AVBufferRef` to an
//!   `AVHWDeviceContext` — handed to `avcodec_get_hw_frames_parameters`,
//!   whose answer, an `AVHWFramesContext`, is read and released inside
//!   the call;
//! - everything else libavcodec itself reads when it is called with the
//!   context: these are libavcodec's callbacks, and a context libavcodec
//!   passes them holds it.
//!
//! # `AVCodec`
//!
//! - the `AVCodec` itself: one libavcodec handed out (`avcodec_find_decoder`
//!   and its kin), which points into a table that lives for the process.
//!   Its hardware configurations are read through `avcodec_get_hw_config`,
//!   each one's `methods`, `device_type` and `pix_fmt` as integers.
//!
//! # `CallbackState`
//!
//! - null, or a live, aligned `CallbackState`. Its atomics are read and
//!   written through shared access, so another thread may use the state
//!   during the call; its plain fields (`wanted`, `wanted_int`,
//!   `max_frame_bytes`) are read and must not be written while the call
//!   runs.
