//! The **amputation seam**: FFmpeg's bytes leave here, copied once,
//! as Rust-owned memory.
//!
//! An `AVPacket`'s payload and an `AVFrame`'s planes both live in
//! `AVBufferRef`s — FFmpeg's own refcounted allocations. Through 0.8
//! this crate handed those out directly, wrapped in an `FfmpegBuffer`
//! whose `AsRef<[u8]>` pointed straight into libavcodec's memory. That
//! type is gone. Every byte that crosses this boundary is now copied
//! into an [`FfmpegBytes`], which is what the core's
//! [carrier amputation contract][law] requires: owned, `Send + Sync`,
//! clone-is-a-refcount-bump, and with no FFmpeg lifetime riding along.
//!
//! What is left in this module is everything the copy still has to
//! *judge*. A packet's payload has to be proved to lie inside the
//! buffer that owns it before a byte of it is read, its side data has
//! to be carried whole or refused, and its flags have to fit the
//! portable set — so [`PacketBufferError`] and its payload structs
//! outlive the buffer type they were written for. The bounds check in
//! particular matters *more* now, not less: 0.8 formed a view over the
//! claimed range, 0.9 reads it.
//!
//! # The one thing the amputation costs
//!
//! The `Arc<[u8]>` behind [`FfmpegBytes`] has no fallible constructor
//! on stable Rust, so the copy itself aborts on allocation failure
//! rather than returning an error.
//! Everything that bounds *how much* can be asked for — the side-data
//! entry and byte caps, the plane-geometry checks — is unchanged and
//! still runs before any allocation, so a hostile stream cannot reach
//! that abort by demanding memory; only a genuinely exhausted
//! allocator can.
//!
//! [`payload_of`] is where the per-packet half of that bounding lives:
//! every packet body this crate copies passes through it, and it
//! refuses an over-budget claim before reading a byte. See
//! [`crate::limits`] for the budgets and their defaults.
//!
//! # The funnel's accounting
//!
//! Every [`FfmpegBytes`] in this crate is built by
//! `FfmpegBytes::copy_from_slice`, `FfmpegBytes::from_rows` or
//! [`FfmpegBytes::empty`], and **every one of those call sites is
//! bounded before it allocates**. The table is kept here, beside the
//! constructors, so that a new exit has to answer the question the
//! existing ones already answered — the discipline is inherited by
//! being written down where the next author will be standing.
//!
//! Three review rounds found bypasses that each looked like an
//! exception: a plane path with no ceiling, an attachment whose
//! payload was copied by `avcodec_parameters_copy` before its budget
//! was charged, a resampler amplifying a small input into a huge
//! output, and then `coded_side_data` — a third heap field on the same
//! wholesale parameter copy, where a MOV `prof` atom puts an ICC
//! profile. None of them were exceptions. They were rows nobody had
//! written down.
//!
//! The third one is why the table below has a section it did not need
//! at first. `FfmpegBytes` is not the only place this crate copies
//! attacker-sized bytes: `AVCodecParameters` has heap fields of its own,
//! and the wholesale FFI copy that used to duplicate them took every
//! one — including any this crate had never enumerated. That copy is
//! gone; see
//! [`bounded_clone_parameters`](crate::extras::bounded_clone_parameters),
//! and [`CodecTicket`](crate::CodecTicket), which took the track row's
//! copy one step further: those three fields are now owned Rust, and
//! `extradata` lands in an `FfmpegBytes` like every other file-sized
//! buffer here.
//!
//! | construction site | what it carries | what bounds it |
//! |---|---|---|
//! | [`payload_of`] | any packet's payload | its `budget` argument — [`PacketLimits::max_packet_bytes`](crate::PacketLimits::max_packet_bytes) for timed packets, [`DemuxLimits::max_attachment_bytes`](crate::DemuxLimits::max_attachment_bytes) for attachments — judged against the declared `size` before a byte is read |
//! | `convert::copy_out_planes`, tight stride | one video or image plane | [`FrameLimits::max_pixels`](crate::FrameLimits::max_pixels) and [`FrameLimits::max_frame_bytes`](crate::FrameLimits::max_frame_bytes), both in a judge-pass that runs before any plane is allocated; `max_pixels` also reaches libavcodec |
//! | `convert::copy_out_planes`, padded stride | one compacted plane, via `FfmpegBytes::from_rows` | the same pre-pass |
//! | `convert::av_frame_to_audio_frame` | one audio plane | `max_frame_bytes`, checked over `plane_bytes × plane_count` before the loop |
//! | `convert::collect_side_data` | one frame side-data entry | `SIDE_DATA_MAX_ENTRIES` (64) and `SIDE_DATA_MAX_TOTAL_BYTES` (256 KiB), plus `try_reserve_exact` |
//! | `boundary::packet_side_data` | one packet side-data entry | the same two caps, as refusals rather than truncation |
//! | `convert::av_subtitle_to_subtitle_frame`, text | concatenated cue text | `SUBTITLE_MAX_TEXT_BYTES_PER_RECT` (64 KiB), `SUBTITLE_MAX_TEXT_TOTAL_BYTES` (256 KiB), `SUBTITLE_MAX_RECTS` (64) |
//! | …, bitmap | one paletted rect | `SUBTITLE_MAX_BITMAP_BYTES_PER_RECT` (16 MiB), `SUBTITLE_MAX_BITMAP_TOTAL_BYTES` (32 MiB), `SUBTITLE_MAX_RECTS` |
//! | …, palette | an RGBA palette | structurally fixed at 256 × 4 bytes by the format |
//! | `demuxer::extradata_payload` | a synthesized attachment (a font) | `demuxer::admit_attachments`, which charges every attachment in the file — per-attachment **and** aggregate — before the track loop allocates anything; re-checked here against the per-attachment ceiling |
//! | `demuxer::attached_pic_payload` | a hoisted cover-art packet | the same admission pass, then `payload_of`'s budget |
//! | `resampler::finish_output` | one converted audio plane | `FfmpegResampler::check_output_bytes`, against `max_frame_bytes`, run before the output `AVFrame` is allocated |
//! | every [`FfmpegBytes::empty`] site | nothing | structurally zero: placeholder plane slots, a payload-less packet, a null palette, a marker side-data entry |
//!
//! # The other heap this crate copies
//!
//! `AVCodecParameters` is the same class of exposure — three heap
//! fields, all sized by the file — so its rows belong in the same
//! accounting. Since the track row went owned, one of those fields
//! *is* an [`FfmpegBytes`]: [`CodecTicket`](crate::CodecTicket) holds
//! `extradata` in one, and each `coded_side_data` payload in another.
//!
//! | construction site | what it carries | what bounds it |
//! |---|---|---|
//! | [`CodecTicket::mirror`](crate::CodecTicket::mirror), `extradata` | SPS/PPS and codec headers, into an [`FfmpegBytes`] | [`DemuxLimits::max_codec_parameter_bytes`](crate::DemuxLimits::max_codec_parameter_bytes), measured by `measure_parameters` before a byte is read |
//! | …, `coded_side_data` | the descriptor array and each entry's payload — a MOV `prof` atom's ICC profile among them — into owned entries | the same limit, counting the array as well as the payloads |
//! | …, `ch_layout` custom map | a channel map, into owned entries | the same limit |
//! | [`CodecTicket::rebuild`](crate::CodecTicket::rebuild) | the same three fields, back into a fresh `AVCodecParameters` for a decoder to open from | nothing further, and nothing further is needed: it allocates exactly [`CodecTicket::footprint_bytes`](crate::CodecTicket::footprint_bytes), which is the number the mirror already admitted — no file-controlled input reaches it |
//! | [`bounded_clone_parameters`](crate::extras::bounded_clone_parameters) | the same three fields, `AVCodecParameters` to `AVCodecParameters` | the same measurement. Off the demux road since the track row went owned; it is the decoder's own re-clone (`decoder::try_clone_parameters`), bounded by [`DecoderLimits::max_codec_parameter_bytes`](crate::DecoderLimits::max_codec_parameter_bytes) |
//! | `demuxer::admit_streams` | nothing — it only measures | runs over **every** stream before the track loop mirrors anything, and charges the whole-file [`max_total_codec_parameter_bytes`](crate::DemuxLimits::max_total_codec_parameter_bytes) |
//! | `decoder::build_codec_context` → `avcodec_parameters_to_context` | the same three fields, copied *into* an `AVCodecContext` | **the choke point**: measured and admitted against [`DecoderLimits::max_codec_parameter_bytes`](crate::DecoderLimits::max_codec_parameter_bytes) right there. Every decoder in this crate opens through this function — the four session `open`s, the HW probe's `build_state`, its per-backend advances, the software fallback — and none of them reaches `avcodec_parameters_to_context` any other way |
//! | `image::FfmpegImageDecoder::decode` → `boundary::try_packet_copy` | the caller's compressed bytes, duplicated into an `AVPacket` | [`DecoderLimits::max_image_input_bytes`](crate::DecoderLimits::max_image_input_bytes), defaulting to the attachment family so the direct road is no more permissive than the demuxed one |
//! | `boundary::ffmpeg_packet_from_{video,audio,subtitle}_packet` → `try_packet_copy` | the caller's compressed bytes, duplicated into an `AVPacket` — **the send leg** | [`DecoderLimits::max_packet_bytes`](crate::DecoderLimits::max_packet_bytes), judged before the allocation. The same limit the receive leg (`payload_of`) judges, so a byte count refused coming out of a container is refused going into a decoder |
//! | still `pal8` palette plane | a fixed `AVPALETTE_SIZE` run | the **format**, not a limit: 256 × `AV_PIX_FMT_RGB32`, always, with no number a file gets to choose |
//!
//! # The rule
//!
//! **A carrier whose size comes from a file is bounded by a limit in
//! [`crate::limits`]; a carrier whose size is a property of a format is
//! bounded by that format.** There is no third kind, and a site that
//! looks like one has not been thought about yet.
//!
//! And the corollary the third round bought: **no code path hands
//! attacker-sized data to a wholesale FFI copy** — a copy that
//! duplicates every field of a struct duplicates the fields nobody
//! enumerated, which is a budget bypass that arrives with the next
//! FFmpeg release rather than with the next commit.
//!
//! # The substrate's options, and where this crate stops
//!
//! Everything above is **tier one** of the [resource governance
//! contract][gov]: allocations this crate makes itself, each bounded by
//! a named limit or by a format. This table is that tier's proof.
//!
//! Tier two is the other half — FFmpeg's own resource options, set at
//! every point libavcodec and libavformat offer one. They bound
//! allocations this crate does not make and could not otherwise see:
//!
//! | option | where it is set | what it bounds |
//! |---|---|---|
//! | `AVCodecContext.max_pixels` | every opened decoder | the caller's pixel limit, **verbatim**, applied by `ff_set_dimensions` to the raw dimensions. Extent, not cost: what a frame *costs* is the byte judge's question, two rows down |
//! | the `get_format` coded-dims ask | the hardware road | the **pool's own declared extent**, asked of `avcodec_get_hw_frames_parameters` before the pool is initialised — `max_pixels` is applied to the *display* dims, which a cropped stream can make 2000x smaller |
//! | the `get_format` byte judge | the hardware road | the **pool's** cost, priced through [`crate::footprint`] against `max_frame_bytes`. **Fails closed**: a pool that will not declare its dimensions and layout is a pool that cannot be judged, and the codec-alignment fallback that used to stand in could answer *smaller* than the pool it was standing in for |
//! | the `get_buffer2` byte judge | every software decode | what the allocator will actually take for this frame — pictures and audio both, priced through [`crate::footprint`] against the caller's own `max_frame_bytes`, carried in the codec context's callback state |
//! | the pre-transfer judge | every `av_hwframe_transfer_data` | the CPU destination a hardware download allocates, priced at the frames-context pool dims — folding **every** candidate format FFmpeg may pick, priceable or not, since FFmpeg does the picking |
//! | `probesize` / `formatprobesize` | both demux entrypoints | what the format probe and stream analysis may consume |
//! | `max_streams` | both demux entrypoints | the `AVStream` array a header can conjure |
//! | the `AVIOContext` byte meter | the **reader** demux entrypoint | total bytes libavformat is handed, hard — past the budget the reader stops answering |
//!
//! Two of those options used to carry *translated* byte ceilings —
//! `max_pixels` as `min(the caller's limit, bytes / 16)` and
//! `max_samples` as `bytes / 8` — so that the byte budget could bite
//! before libavcodec allocated. Both translations charged every stream
//! the worst format in existence, and both over-refused ordinary media:
//! a 1920x1080 `yuv420p` frame under a 4 MiB budget, a 6-channel `s16`
//! frame under 64 KiB. They are gone. The byte budget is enforced by
//! the `get_buffer2` judge, which is *itself* a pre-allocation check —
//! `get_buffer2` **is** the allocation — and prices the frame's real
//! format at its real dimensions. An exact judge at the allocation
//! beats an approximate one before it.
//!
//! Where a layout cannot be priced at all, these judges charge
//! [`crate::footprint::video_frame_bytes_upper_bound`] — the same
//! dimension alignment and per-plane overhead at the widest per-pixel
//! rate the census finds — rather than a bare `w * h * rate`, which
//! omits both and could land *below* the accurate path it was standing
//! in for. A conservative fallback that can under-state is not
//! conservative.
//!
//! **These are defense in depth, not a proof.** Each bounds what it was
//! built to bound; together they cover every interposition point FFmpeg
//! exposes, which is not the same as covering FFmpeg.
//!
//! ## What the demux limits cannot reach, and why they exist anyway
//!
//! `avformat_open_input` and `avformat_find_stream_info` build the
//! attached picture, the extradata and the coded side data out of the
//! file themselves. The attachment and parameter limits in the table
//! above therefore measure this crate's *copies* of buffers libavformat
//! has already allocated — too late, by construction, to have prevented
//! the original.
//!
//! A parser cannot allocate from bytes it was never handed, so the
//! input is bounded instead: that is what the probe options and the byte
//! meter are for. What is **not** bounded is allocation *amplification*
//! inside a parser — a container can describe, in a handful of bytes, a
//! structure whose in-memory form is far larger, and nothing outside
//! libavformat can observe it happen. Bounding that output is the
//! substrate's own hardening territory; FFmpeg keeps `max_streams`,
//! `max_index_size` and `max_picture_buffer` for it, and this crate
//! sets the first.
//!
//! The hard meter also does not reach the **path** entrypoint: it needs
//! an `AVIOContext` this crate owns, and a path is opened by
//! libavformat's own protocol layer. The probe options still apply there;
//! a caller who wants the meter on a file opens it as a reader.
//!
//! That gap is **tier three**, and it is named rather than hedged: see
//! the [contract][gov] for the boundary and for the OS-level instrument
//! a deployment needing a hard memory bound puts underneath all of
//! this. This crate is not a hypervisor for FFmpeg, and its limits
//! compose with that instrument rather than replacing it.
//!
//! [gov]: mediaspine::adapter#the-resource-governance-contract
//!
//! And the capstone, which is what every limit in this table is finally
//! for: **a judge must dominate the allocator's arithmetic, not the
//! payload's.** A budget compared against what the bytes weigh is not a
//! budget on what will be spent — see [`crate::footprint`] for the
//! measured gap and for the two judges that were caught paying it.
//!
//! And the corollary the ninth bought, which is about *whether* to
//! carry at all rather than how much: **a payload that carries
//! addresses instead of bytes is uncarriable.** `AV_PKT_FLAG_TRUSTED`
//! marks one — the wrapped-`AVFrame` producers use it for a body that
//! is an `AVFrame` pointer structure — and copying it mints a carrier
//! that passes every property this table exists to guarantee and
//! dangles the moment its source drops. It is refused on both legs
//! ([`payload_of`] and the reverse builders), because either alone
//! leaves the loop open. See [`TrustedPayload`].
//!
//! And the corollary the seventh bought, about the *inputs* to every
//! guard above rather than the guards themselves: **a number a file
//! chooses is judged or refused, never clipped.** A limit that bounds a
//! byte product still trusts the fields the product is computed from,
//! so a clamped sample count or channel count does not trip any budget
//! — it produces a smaller, plausible frame that no ceiling has any
//! reason to stop. Two of those were live on the audio path (a floored
//! negative `nb_samples`, a channel count clipped to `u8::MAX`), and
//! both turned a malformed header into a well-formed-looking frame,
//! which is strictly worse than an error. The audio road now carries no
//! lossy clamp; the one floor left, `sample_rate`, is censused at its
//! site with the reason it is metadata and sizes nothing.
//!
//! [law]: mediaspine::adapter#the-carrier-amputation-contract

use std::fmt;

use derive_more::{IsVariant, TryUnwrap, Unwrap};

/// The bytes every packet and frame this crate produces are carried in.
///
/// Owned, `Send + Sync`, `'static`, and clone-is-a-refcount-bump: the
/// core's [carrier amputation contract][law], satisfied. Nothing inside
/// reaches back into libavcodec.
///
/// # Why it is opaque
///
/// The obvious spelling was the bare `Arc<[u8]>` this type wraps, and
/// 0.9.0's first cut used it. It is opaque for one reason, and the
/// reason is not aesthetics:
///
/// **`Arc<[u8]>` is one storage strategy, and it is not going to be the
/// only one.** Every exit currently allocates, copies, and frees per
/// frame; a decode loop at 4K is asking the global allocator for eight
/// megabytes sixty times a second and handing it back. The recorded
/// answer is a plane pool — reusable slabs handed out at the boundary
/// and returned when the last consumer drops them
/// ([issue #35](https://github.com/findit-studio/mediadecode/issues/35)).
/// A pooled slab is a different carrier with the same contract: still
/// owned, still `Send + Sync`, still refcount-cloned, still holding no
/// FFmpeg lifetime.
///
/// If the carrier were `Arc<[u8]>` in the public aliases, adding the
/// pool would change the type of every frame and every packet in the
/// crate — a breaking release for a change consumers cannot observe.
/// Behind this newtype it is a new arm of a **private** enum: no
/// signature moves, no consumer recompiles differently, and the
/// `AsRef<[u8]>` a consumer actually programs against is unchanged.
/// That extension point *is* this type's justification for existing.
///
/// The enum has exactly one arm today. It gains the second when the
/// pool is built and not before — this codebase does not carry members
/// nothing can produce.
///
/// [law]: mediaspine::adapter#the-carrier-amputation-contract
#[derive(Clone, Default)]
pub struct FfmpegBytes(Inner);

/// The storage behind [`FfmpegBytes`]. **Private, and the point.**
///
/// One arm today; see the type's own docs for the arm that is coming
/// and why it can arrive without a breaking release.
#[derive(Clone)]
enum Inner {
  /// No bytes, and no allocation either.
  ///
  /// The empty carrier is frequent — a video frame allocates four
  /// plane slots and fills one to three, and a payload-less packet is
  /// ordinary — and it used to be a process-wide `Arc` singleton
  /// behind a `OnceLock`. A variant that holds nothing is simpler and
  /// allocates nothing at all, which is what the fallible road wants
  /// at its base case.
  Empty,
  /// A refcounted buffer, allocated by the copy at the boundary.
  ///
  /// **`triomphe::Arc`, and the reason is allocation failure.** The
  /// bytes here come out of a container and are therefore
  /// attacker-sized; every road into this carrier is downstream of a
  /// budget that admitted a number, and a budget is worth nothing if
  /// the allocation it admitted then aborts the process rather than
  /// reporting the failure.
  ///
  /// `std::sync::Arc` cannot do it: there is no `try_new` for
  /// `Arc<[u8]>` on stable and `Arc::new_uninit_slice` aborts like
  /// every other infallible allocator call. Staging through a
  /// `try_reserve_exact`ed `Vec` only moved the problem — the `Arc`
  /// that took the `Vec` still allocated infallibly, and one of those
  /// headers exists per coded-side-data entry, a count the file
  /// controls. `triomphe::Arc` offers `try_new` and
  /// `UniqueArc::try_new_uninit_slice`, so the whole allocation is
  /// fallible and the payload lands in the `Arc` directly, with no
  /// intermediate `Vec` and no second copy.
  ///
  /// An inline-capable carrier — `smol_bytes::Bytes`, which the text
  /// fields use — was rejected rather than overlooked: inline storage
  /// makes `Clone` copy small payloads, and [`FfmpegBytes::ptr_eq`],
  /// the amputation contract's own instrument, asks whether a clone
  /// was a refcount bump. A carrier that sometimes copies cannot
  /// answer that question. `triomphe::Arc` keeps pointer identity.
  Shared {
    /// The allocation. May be longer than `len`: a producer that sizes
    /// its output before a conversion runs cannot know the true length
    /// until afterwards, and re-sizing then would be the very
    /// allocation-after-the-point-of-no-return this carrier exists to
    /// avoid.
    bytes: triomphe::Arc<[u8]>,
    /// How much of `bytes` is real output. A carrier's span is a span
    /// a consumer may read, so everything past this is invisible.
    len: usize,
  },
}

impl Default for Inner {
  #[inline]
  fn default() -> Self {
    Self::Empty
  }
}

impl FfmpegBytes {
  /// Copies `bytes` into a fresh carrier.
  ///
  /// **The copy site.** Every exit in this crate lands here or on
  /// [`Self::empty`], so "one copy at the boundary" is a property of
  /// one constructor rather than a promise thirty call sites keep —
  /// and it is the one place a future pooled arm has to be taught
  /// about.
  ///
  /// Public because the reverse direction needs it: a consumer
  /// building a packet to feed back into a decoder has bytes and needs
  /// a carrier, and the alternative is an opaque type nobody outside
  /// this crate can construct.
  ///
  /// A zero-length copy lands on the shared empty allocation rather
  /// than minting its own.
  #[inline]
  ///
  /// **Test-only.** Its body is `try_copy_from_slice(..).expect(..)`,
  /// and a `panic` on allocation failure is no better than the abort
  /// it replaced — so no product path may reach it, and the `cfg`
  /// below is what makes that a compile-time fact rather than a grep.
  /// Everything that copies a container-controlled payload uses
  /// [`Self::try_copy_from_slice`]: a fallible staging allocation does
  /// not protect a subsequent infallible copy.
  #[cfg(test)]
  pub fn copy_from_slice(bytes: &[u8]) -> Self {
    if bytes.is_empty() {
      return Self::empty();
    }
    Self::try_copy_from_slice(bytes).expect("an infallible copy of an already-admitted payload")
  }

  /// [`Self::copy_from_slice`], reporting an allocation failure rather
  /// than aborting on one.
  ///
  /// **The road every budgeted copy takes.** The payload is reserved
  /// with `Vec::try_reserve_exact` before a byte is written, so a
  /// container whose extradata, side data or attachment the caller's
  /// ceilings admitted cannot terminate a safe `open` when the
  /// allocator declines — it answers a named error instead. What is
  /// left infallible afterwards is the `Arc` header described on
  /// [`Inner`]: three words, and the same three whatever the payload
  /// weighs.
  #[inline]
  pub fn try_copy_from_slice(bytes: &[u8]) -> Option<Self> {
    if bytes.is_empty() {
      return Some(Self::empty());
    }
    let mut uninit =
      triomphe::UniqueArc::<[core::mem::MaybeUninit<u8>]>::try_new_uninit_slice(bytes.len())
        .ok()?;
    // SAFETY: the slice was just allocated with exactly `bytes.len()`
    // slots, `uninit` is unique by type so nothing else can observe
    // them, and the two regions cannot overlap — one is a fresh
    // allocation.
    unsafe {
      core::ptr::copy_nonoverlapping(
        bytes.as_ptr(),
        uninit.as_mut_ptr().cast::<u8>(),
        bytes.len(),
      );
    }
    // SAFETY: every one of those slots was just written.
    let filled = unsafe { triomphe::UniqueArc::assume_init_slice(uninit) };
    let len = filled.len();
    Some(Self(Inner::Shared {
      bytes: filled.shareable(),
      len,
    }))
  }

  /// Claims `cap` bytes **now**, to be filled and named later.
  ///
  /// The reservation exists so that every fallible step happens on the
  /// near side of a conversion. `swr` consumes its input as it runs, so
  /// an allocation that fails afterwards leaves a caller with nothing
  /// to retry and samples that are simply gone — which is what the
  /// owned lane did until this reservation existed.
  ///
  /// The bytes are zeroed rather than left uninitialised: a carrier
  /// built from a partly-written reservation must still be a `[u8]` a
  /// consumer may read, and the zeroing happens here, before the point
  /// of no return, rather than in `commit` where nothing may fail.
  #[inline]
  pub(crate) fn reserve(cap: usize) -> Option<triomphe::UniqueArc<[u8]>> {
    let mut uninit =
      triomphe::UniqueArc::<[core::mem::MaybeUninit<u8>]>::try_new_uninit_slice(cap).ok()?;
    // SAFETY: `cap` slots were just allocated and `uninit` is unique by
    // type, so writing zeros over all of them initialises the whole
    // slice and nothing else can observe the intermediate state.
    unsafe {
      core::ptr::write_bytes(uninit.as_mut_ptr().cast::<u8>(), 0, cap);
    }
    // SAFETY: every slot was just written.
    Some(unsafe { triomphe::UniqueArc::assume_init_slice(uninit) })
  }

  /// Names how much of a [`Self::reserve`] is real output.
  ///
  /// **Infallible, and that is the point**: by the time this runs the
  /// conversion has happened and there is nothing left to fail.
  #[inline]
  pub(crate) fn from_reservation(bytes: triomphe::UniqueArc<[u8]>, len: usize) -> Self {
    let len = len.min(bytes.len());
    if len == 0 {
      return Self::empty();
    }
    Self(Inner::Shared {
      bytes: bytes.shareable(),
      len,
    })
  }

  /// The zero-length carrier, shared.
  ///
  /// Placeholder plane slots and payload-less packets are frequent — a
  /// video frame allocates four slots and populates one to three of
  /// them — and each would otherwise be its own `Arc` header
  /// allocation. One empty allocation for the process, cloned by
  /// refcount, instead.
  #[inline]
  pub fn empty() -> Self {
    Self(Inner::Empty)
  }

  /// Builds a carrier of `rows * row_bytes` bytes by writing each row
  /// in turn — **one allocation, no staging buffer**.
  ///
  /// This is the road a padded plane takes. FFmpeg lays such a plane
  /// out `linesize` bytes per row while only the first `row_bytes` of
  /// each are the decoder's output, so the copy has to be row-wise and
  /// the destination is contiguous. The obvious spelling — build a
  /// `Vec`, then `Arc::from` it — allocates the whole plane **twice**
  /// and copies it twice, so a 250 MiB frame peaks at 750 MiB counting
  /// FFmpeg's own. Writing the rows straight into
  /// `Arc::new_uninit_slice` leaves the unavoidable 2×: FFmpeg's plane
  /// and ours.
  ///
  /// `row(i)` must answer a slice of exactly `row_bytes`; a shorter or
  /// longer one is a bug in the caller's geometry and panics rather
  /// than leaving the tail of the allocation uninitialised. That
  /// assertion is what discharges the initialisation contract for the
  /// `assume_init` below: the loop visits every row, each row fills its
  /// full width, and `rows * row_bytes` is the whole allocation.
  ///
  /// Crate-internal: the public face is [`Self::copy_from_slice`], and
  /// this shape only makes sense to a caller that already holds a
  /// strided picture.
  ///
  /// # Panics
  ///
  /// If `rows * row_bytes` overflows `usize`, or if `row(i)` answers a
  /// slice that is not `row_bytes` long. Callers reach this only after
  /// the geometry has been validated and the total checked against
  /// [`FrameLimits`](crate::FrameLimits), so both are unreachable from
  /// input.
  pub(crate) fn from_rows<'a>(
    rows: usize,
    row_bytes: usize,
    mut row: impl FnMut(usize) -> &'a [u8],
  ) -> Option<Self> {
    let len = rows.checked_mul(row_bytes)?;
    if len == 0 {
      return Some(Self::empty());
    }
    // One reservation, made **fallibly**, and no staging buffer: the
    // rows are appended straight into it. `None` therefore covers two
    // refusals now — a row whose length disagrees with `row_bytes`,
    // and an allocator that declined the plane — and both are answers
    // this carrier's callers already had to handle.
    //
    // The `MaybeUninit` gather this replaced needed `unsafe` to view
    // the source as uninitialised memory and a written-every-slot
    // argument to discharge; appending cannot leave a hole, so the
    // argument goes with it.
    let mut uninit =
      triomphe::UniqueArc::<[core::mem::MaybeUninit<u8>]>::try_new_uninit_slice(len).ok()?;
    let destination = uninit.as_mut_ptr().cast::<u8>();
    for index in 0..rows {
      let source = row(index);
      if source.len() != row_bytes {
        // A length that arrives from a caller is an input, not a
        // promise: refuse rather than copy `row_bytes` out of a
        // shorter slice. The half-written allocation drops here.
        return None;
      }
      // SAFETY: `rows * row_bytes == len` slots were allocated above,
      // this row starts at `index * row_bytes` and is exactly
      // `row_bytes` long, so the write stays inside them; the
      // allocation is fresh, so it cannot overlap the source.
      unsafe {
        core::ptr::copy_nonoverlapping(
          source.as_ptr(),
          destination.add(index * row_bytes),
          row_bytes,
        );
      }
    }
    // SAFETY: the loop wrote every one of the `rows * row_bytes` slots
    // — `rows` iterations, each filling exactly `row_bytes`
    // consecutive bytes starting at `index * row_bytes`, with any
    // disagreeing row abandoning the whole gather before this point.
    let filled = unsafe { triomphe::UniqueArc::assume_init_slice(uninit) };
    let filled_len = filled.len();
    Some(Self(Inner::Shared {
      bytes: filled.shareable(),
      len: filled_len,
    }))
  }

  /// The bytes, as a slice.
  ///
  /// The same answer [`AsRef::as_ref`] gives; inherent so a caller
  /// reaching through a `&FfmpegBytes` does not have to name the trait.
  #[inline]
  pub fn as_slice(&self) -> &[u8] {
    match &self.0 {
      Inner::Empty => &[],
      Inner::Shared { bytes, len } => &bytes[..*len],
    }
  }

  /// Number of bytes carried.
  #[inline]
  pub fn len(&self) -> usize {
    self.as_slice().len()
  }

  /// `true` when this carries no bytes.
  #[inline]
  pub fn is_empty(&self) -> bool {
    self.as_slice().is_empty()
  }

  /// `true` when both handles name the same allocation — a clone of
  /// one another, rather than two copies that happen to be equal.
  ///
  /// The property the amputation contract is really about: `Clone` on
  /// a message is a refcount bump. `PartialEq` answers a different
  /// question (do these hold the same bytes), and a test that wants to
  /// prove the clone did not copy has to ask this one.
  #[inline]
  pub fn ptr_eq(&self, other: &Self) -> bool {
    match (&self.0, &other.0) {
      // Two empties name the same nothing, which is what the
      // process-wide empty singleton used to answer.
      (Inner::Empty, Inner::Empty) => true,
      // The allocation, not the span: two carriers naming different
      // prefixes of one reservation are still the same allocation, and
      // this question is about whether a clone copied.
      (Inner::Shared { bytes: a, .. }, Inner::Shared { bytes: b, .. }) => {
        triomphe::Arc::ptr_eq(a, b)
      }
      _ => false,
    }
  }
}

/// **Over the span, not the allocation.**
///
/// The derive compared `Inner` structurally, and `Inner` is not a
/// structural type: a `Shared` arm holds an allocation that may be
/// longer than the span, because a producer that sizes its output
/// before a conversion runs cannot know the true length until
/// afterwards. Two carriers whose [`FfmpegBytes::as_slice`] answers are
/// byte-for-byte identical therefore compared **unequal** when one came
/// off a reservation and the other was copied exactly — and `Empty`
/// compared unequal to a zero-length `Shared`, though both carry
/// nothing. Capacity a consumer cannot read is not part of the value.
///
/// [`FfmpegBytes::ptr_eq`] remains the instrument for the other
/// question — *is this handle a refcount bump of that one* — and it is
/// deliberately not what `==` answers.
impl PartialEq for FfmpegBytes {
  #[inline]
  fn eq(&self, other: &Self) -> bool {
    self.as_slice() == other.as_slice()
  }
}

impl Eq for FfmpegBytes {}

/// Hashes exactly what [`PartialEq`] compares, which is the contract's
/// requirement rather than a preference: the derived hash mixed in the
/// invisible capacity, so two equal carriers could land in different
/// buckets and a payload-keyed map would miss.
impl core::hash::Hash for FfmpegBytes {
  #[inline]
  fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
    core::hash::Hash::hash(self.as_slice(), state);
  }
}

impl AsRef<[u8]> for FfmpegBytes {
  #[inline]
  fn as_ref(&self) -> &[u8] {
    self.as_slice()
  }
}

impl fmt::Debug for FfmpegBytes {
  /// Length only, never the bytes.
  ///
  /// A derived `Debug` would print a decoded 4K plane one integer at a
  /// time; this type is reached from the derived `Debug` of every
  /// packet, frame and side-data entry in the crate, so the terse form
  /// is the one that keeps those useful. Mirrors what `FfmpegBuffer`'s
  /// own hand-written `Debug` did through 0.8.
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("FfmpegBytes")
      .field("len", &self.len())
      .finish()
  }
}

/// Payload for [`PacketBufferError::PacketTooLarge`].
///
/// A packet's payload is larger than the budget in force.
///
/// Refused **before** the copy: 0.8 answered a claimed payload with a
/// refcount, so an absurd `size` cost nothing; 0.9 answers it with an
/// allocation, so the claim has to be judged first.
#[derive(Copy, Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("a {bytes}-byte packet payload exceeds the {limit}-byte budget")]
pub struct PacketTooLarge {
  bytes: usize,
  limit: usize,
}

impl PacketTooLarge {
  /// Constructs a `PacketTooLarge` payload.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(bytes: usize, limit: usize) -> Self {
    Self { bytes, limit }
  }
  /// The payload length the packet declared.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn bytes(&self) -> usize {
    self.bytes
  }
  /// The budget in force.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn limit(&self) -> usize {
    self.limit
  }
}

/// Payload for [`PacketBufferError::Bounds`].
///
/// The payload does not lie inside the packet's own buffer.
/// `AVPacket` guarantees it does; a packet that says otherwise is
/// malformed, and wrapping it would hand out a view over memory the
/// buffer does not own.
#[derive(Copy, Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("a {len}-byte payload at offset {offset} does not lie inside a {size}-byte buffer")]
pub struct Bounds {
  offset: usize,
  len: usize,
  size: usize,
}

impl Bounds {
  /// Constructs a `Bounds` payload.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(offset: usize, len: usize, size: usize) -> Self {
    Self { offset, len, size }
  }
  /// Where the payload starts inside the buffer.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn offset(&self) -> usize {
    self.offset
  }
  /// The payload's length in bytes.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn len(&self) -> usize {
    self.len
  }
  /// `true` when the payload is zero bytes long.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn is_empty(&self) -> bool {
    self.len == 0
  }
  /// The buffer's own length in bytes.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn size(&self) -> usize {
    self.size
  }
}

/// Payload for [`PacketBufferError::SideDataEntries`].
///
/// A packet declares more side-data entries than this crate will
/// walk, or a negative count.
///
/// The cap bounds the work a crafted packet can demand *before* it is
/// refused. It cannot trip on anything FFmpeg's own packet API
/// produces: both `av_packet_new_side_data` and
/// `av_packet_add_side_data` replace an entry of the same type, so a
/// packet carries at most one entry per named type — forty-three in
/// this build, and the cap tracks that number if it ever grows past
/// the floor.
#[derive(Copy, Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("a packet declaring {count} side-data entries cannot be carried (limit {cap})")]
pub struct SideDataEntries {
  count: i32,
  cap: usize,
}

impl SideDataEntries {
  /// Constructs a `SideDataEntries` payload.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(count: i32, cap: usize) -> Self {
    Self { count, cap }
  }
  /// The count the packet declared.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn count(&self) -> i32 {
    self.count
  }
  /// The most entries this crate will walk.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn cap(&self) -> usize {
    self.cap
  }
}

/// Payload for [`PacketBufferError::SideDataArray`].
///
/// A packet declares side-data entries and carries no array to read
/// them from.
///
/// Malformed, and named rather than read as "no side data": a null
/// array with a positive count is the same silent loss as a truncated
/// copy, reached through the pointer instead of the cap.
#[derive(Copy, Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("a packet declaring {count} side-data entries carries no array")]
pub struct SideDataArray {
  count: i32,
}

impl SideDataArray {
  /// Constructs a `SideDataArray` payload.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(count: i32) -> Self {
    Self { count }
  }
  /// The count the packet declared.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn count(&self) -> i32 {
    self.count
  }
}

/// Payload for [`PacketBufferError::SideDataPayload`].
#[derive(Copy, Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("side-data entry {index} declares {size} bytes and carries no data")]
pub struct SideDataPayload {
  index: usize,
  size: usize,
}

impl SideDataPayload {
  /// Constructs a `SideDataPayload` payload.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(index: usize, size: usize) -> Self {
    Self { index, size }
  }
  /// The entry's position in the packet's array.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn index(&self) -> usize {
    self.index
  }
  /// The length the entry declared.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn size(&self) -> usize {
    self.size
  }
}

/// Payload for [`PacketBufferError::SideDataBytes`].
///
/// A packet's side data is larger than this crate will copy.
#[derive(Copy, Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{bytes} bytes of side data cannot be carried (limit {cap})")]
pub struct SideDataBytes {
  bytes: usize,
  cap: usize,
}

impl SideDataBytes {
  /// Constructs a `SideDataBytes` payload.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(bytes: usize, cap: usize) -> Self {
    Self { bytes, cap }
  }
  /// The total the packet's entries reached.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn bytes(&self) -> usize {
    self.bytes
  }
  /// The most bytes this crate will copy.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn cap(&self) -> usize {
    self.cap
  }
}

/// Payload for [`PacketBufferError::UnrepresentableFlags`].
///
/// A packet carries flag bits the portable vocabulary cannot hold.
///
/// `mediaspine`'s `PacketFlags` is a `u8` bit set, and every packet
/// flag FFmpeg names today lives in that byte — so this cannot fire
/// against this build. It exists so that the day one does not, the
/// packet is refused by name instead of arriving with a bit quietly
/// missing: the same rule the rest of this boundary keeps.
#[derive(Copy, Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("packet flags {raw:#x} do not fit the portable flag set")]
pub struct UnrepresentableFlags {
  raw: i32,
}

impl UnrepresentableFlags {
  /// Constructs an `UnrepresentableFlags` payload.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(raw: i32) -> Self {
    Self { raw }
  }
  /// `AVPacket.flags` as FFmpeg wrote it.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn raw(&self) -> i32 {
    self.raw
  }
}

/// Payload for [`PacketBufferError::SideDataAlloc`].
///
/// Out of memory copying a side-data entry.
#[derive(Copy, Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("out of memory copying {size} bytes of side data")]
pub struct SideDataAlloc {
  size: usize,
}

impl SideDataAlloc {
  /// Constructs a `SideDataAlloc` payload.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(size: usize) -> Self {
    Self { size }
  }
  /// The entry's length in bytes.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn size(&self) -> usize {
    self.size
  }
}
/// Why a packet could not be carried across the boundary — its payload,
/// or the side data that comes with it.
///
/// Every arm means the bytes are real and this crate could not carry
/// them — never that there were none. "No payload" is `Ok(None)` from
/// [`payload_of`], and keeping the two apart is the whole point of the
/// type: a demuxer that reads a malformed packet as an empty marker
/// drops a video packet and carries on as though the file said so. The
/// side-data arms exist for the same reason one tier along — a packet
/// whose side data cannot be carried whole is refused, never delivered
/// with some of it.
#[derive(Copy, Clone, Debug, PartialEq, Eq, thiserror::Error, IsVariant, Unwrap, TryUnwrap)]
#[unwrap(ref, ref_mut)]
#[try_unwrap(ref, ref_mut)]
pub enum PacketBufferError {
  /// The payload is larger than the budget in force. Refused before
  /// the copy.
  #[error(transparent)]
  PacketTooLarge(#[from] PacketTooLarge),

  /// The payload does not lie inside the packet's own buffer.
  #[error(transparent)]
  Bounds(#[from] Bounds),

  /// A packet declares more side-data entries than this crate will
  /// walk, or a negative count.
  #[error(transparent)]
  SideDataEntries(#[from] SideDataEntries),

  /// A packet declares side-data entries and carries no array to read
  /// them from.
  #[error(transparent)]
  SideDataArray(#[from] SideDataArray),

  /// A side-data entry declares bytes it does not carry.
  #[error(transparent)]
  SideDataPayload(#[from] SideDataPayload),

  /// A packet's side data is larger than this crate will copy.
  #[error(transparent)]
  SideDataBytes(#[from] SideDataBytes),

  /// A packet carries flag bits the portable vocabulary cannot hold.
  #[error(transparent)]
  UnrepresentableFlags(#[from] UnrepresentableFlags),

  /// A packet is marked `AV_PKT_FLAG_TRUSTED`, so its payload may hold
  /// pointers rather than bytes. See [`TrustedPayload`].
  #[error(transparent)]
  TrustedPayload(#[from] TrustedPayload),

  /// The capture itself failed — an allocation on the owned lane, a
  /// refcount on the view lane. See [`CaptureFailed`].
  #[error(transparent)]
  CaptureFailed(#[from] CaptureFailed),

  /// The payload's buffer is referenced by something other than the
  /// packet it came from. See [`SharedPayload`].
  #[error(transparent)]
  SharedPayload(#[from] SharedPayload),

  /// Out of memory copying a side-data entry.
  #[error(transparent)]
  SideDataAlloc(#[from] SideDataAlloc),
}

/// `AV_PKT_FLAG_TRUSTED` as the bit the portable `PacketFlags` byte
/// carries it in.
///
/// The core vocabulary deliberately does not *name* this flag — it is
/// FFmpeg's, not a portable fact about packets — but `from_bits_retain`
/// keeps the bit, so this crate can recognise its own flag coming back
/// without the core growing a constant for it.
pub const TRUSTED_BIT: u8 = ffmpeg_next::ffi::AV_PKT_FLAG_TRUSTED as u8;

/// Compile-time proof that the flag really does fit the byte, so the
/// cast above cannot silently become a different bit.
const _: () = {
  assert!(
    ffmpeg_next::ffi::AV_PKT_FLAG_TRUSTED > 0
      && ffmpeg_next::ffi::AV_PKT_FLAG_TRUSTED <= u8::MAX as std::ffi::c_int,
    "AV_PKT_FLAG_TRUSTED no longer fits the portable flag byte",
  );
};

/// Payload for [`PacketBufferError::TrustedPayload`] and
/// [`crate::boundary::PacketBuildError::TrustedPayload`].
///
/// A packet carrying `AV_PKT_FLAG_TRUSTED`, refused on both legs.
///
/// # Why a flag makes a payload uncarriable
///
/// `AV_PKT_FLAG_TRUSTED` is FFmpeg's marker for a packet whose bytes
/// came from a source the *decoder* may treat as its own — and the
/// wrapped-AVFrame producers use it for exactly that: the payload is
/// not media, it is a **structure containing pointers to other live
/// objects** (an `AVFrame` and its buffers), passed by address between
/// components inside one FFmpeg pipeline.
///
/// This crate copies bytes. A pointer copied by value is not owned by
/// the copy — and that is not a gap this crate can close, because there
/// is no bound on what a payload's pointers might reach. So the
/// amputation has a corollary:
///
/// > **A payload that carries addresses instead of bytes cannot be
/// > carried.** Copying it produces a message that looks owned, is
/// > `Send + Sync + 'static` by every type-level test, and dangles the
/// > moment its source is dropped — a use-after-free reachable through
/// > entirely safe API.
///
/// Refusing is not conservatism, it is the only correct answer: the
/// contract this crate exists to keep says every byte leaving FFmpeg is
/// copied once into memory Rust owns, and a pointer cannot be.
///
/// Refused at **both** legs, because either one alone leaves the loop
/// open: copy-out ([`payload_of`]) is where such a packet would enter
/// the graph, and the reverse builders are where a flag that survived
/// some other route would be handed back to a decoder that trusts it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrustedPayload {
  len: usize,
}

impl TrustedPayload {
  /// Constructs a `TrustedPayload` payload.
  #[inline]
  pub const fn new(len: usize) -> Self {
    Self { len }
  }
  /// How many bytes the packet declared.
  #[inline]
  pub const fn len(&self) -> usize {
    self.len
  }
  /// Whether the refused packet declared no bytes.
  #[inline]
  pub const fn is_empty(&self) -> bool {
    self.len == 0
  }
}

impl core::fmt::Display for TrustedPayload {
  fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
    write!(
      f,
      "packet of {} bytes carries AV_PKT_FLAG_TRUSTED; a payload that may hold \
       pointers to other objects cannot be copied into an owned carrier",
      self.len,
    )
  }
}

impl std::error::Error for TrustedPayload {}

/// The payload of a raw `AVPacket`, copied out.
///
/// Shared by the four timed boundary conversions, the attachment
/// conversion, and the demuxer's capture of `AVStream.attached_pic` —
/// an `AVPacket` embedded in the stream by value, which no safe
/// wrapper reaches. One implementation, so the empty-versus-malformed
/// distinction cannot drift between them.
///
/// `Ok(None)` means the packet carries no payload at all: an empty
/// marker, which some demuxers emit. That is a fact about the packet
/// and is kept apart from [`PacketBufferError`], which is a failure to
/// take a payload that *is* there.
///
/// A packet whose `buf` is null — a stack- or arena-allocated
/// `AVPacket` — still reads as "no payload", exactly as it did before
/// the amputation. It is tempting now that the bytes are copied to
/// serve those from `data` / `size` directly, and that is precisely the
/// case with no owning buffer to bound the read against: the claim
/// would have to be taken on faith.
///
/// # Safety
///
/// For the duration of the call, `pkt` must point at a live, aligned
/// `AVPacket` that meets [the `AVPacket` contract](crate::contracts#avpacket)
/// for what this reads: its `buf` (an
/// [`AVBufferRef`](crate::contracts#avbufferref), its reference count
/// included) and the `size` payload bytes at `data`, which the owned lane
/// copies and the view lane lends; `flags` is read as a plain integer.
///
/// `provenance` must be true of it — it decides whether a buffer
/// somebody else also references is refused, copied or windowed
/// (`PayloadProvenance::route`), and whether the carrier may claim the
/// padding behind the payload (`PayloadProvenance::vouches_for_padding`):
///
/// * [`PayloadProvenance::CallerSupplied`] is true of every packet, and
///   claims nothing about the bytes after the payload;
/// * [`PayloadProvenance::DemuxDelivered`] claims that libavformat
///   delivered the packet — so the padding behind its payload is
///   libavformat's — and that no other reference to the payload's buffer
///   is written through while this call reads it;
/// * [`PayloadProvenance::AttachedPicture`] claims that the packet is a
///   stream's parked picture, which libavformat built — its padding is
///   libavformat's too — and that nothing writes through that buffer for
///   as long as the returned carrier lives.
///
/// On the view lane the returned carrier holds a reference of its own to
/// the packet's buffer and may be a window onto it, so the packet's
/// owner must not write through it while the carrier lives. The owned
/// lane copies and owes nothing here.
pub unsafe fn payload_of<C: crate::FfmpegCarrier + crate::CarrierOps>(
  pkt: *const ffmpeg_next::ffi::AVPacket,
  budget: usize,
  provenance: PayloadProvenance,
) -> Result<Option<C::Buffer>, PacketBufferError> {
  // SAFETY: the caller's contract, forwarded unchanged.
  let Some(plan) = (unsafe { preflight_payload(pkt, budget, provenance) })? else {
    return Ok(None);
  };
  // SAFETY: the plan's extent was proved inside the preflight against
  // this same live packet, which the caller keeps alive for the call,
  // under the caller's provenance claim; the view-lane writer clause is
  // the caller's contract, forwarded.
  unsafe { capture_payload::<C>(plan) }.map(Some)
}

/// What [`preflight_payload`] decided, and everything the capture needs
/// to act on it — so the judging and the paying share one reading of
/// the packet rather than each performing their own.
#[derive(Clone, Copy)]
pub struct PayloadPlan {
  buf: *mut ffmpeg_next::ffi::AVBufferRef,
  buf_data: *const u8,
  offset: usize,
  len: usize,
  route: CaptureRoute,
  /// Whether the provenance vouches for the padding behind the payload
  /// ([`PayloadProvenance::vouches_for_padding`]) — decided here, from
  /// the caller's claim, and nowhere else.
  padding: bool,
}

/// **Every refusal a packet payload can earn from what the packet
/// declares — decided without reading a byte of it and without
/// allocating.**
///
/// Split out of [`payload_of`] because the demux admission pass needs
/// exactly these answers about every parked attachment *before* the
/// track table has allocated anything. They are all deterministic facts
/// about the `AVPacket`: a `TRUSTED` payload this crate must not copy,
/// a declared size over the caller's ceiling, a `data`/`size` pair that
/// does not lie inside the buffer it claims, and a buffer somebody else
/// holds a reference to on a road where that is not acceptable. None of
/// them will be different on a second attempt, so none of them belongs
/// after the money is spent.
///
/// What is deliberately *not* here is [`CaptureFailed`] — the allocator
/// declining the carrier. That one cannot be foreseen by any amount of
/// reading, which is what makes it the only `PacketBuffer` arm that is
/// honestly unpredictable.
///
/// `Ok(None)` is the empty answer: a packet with no buffer, no data or
/// no size is not a fault, it is a packet with nothing in it.
///
/// # Safety
///
/// For the duration of the call, `pkt` must point at a live, aligned
/// `AVPacket` whose `buf` is null or an
/// [`AVBufferRef`](crate::contracts#avbufferref): this reads `buf`'s
/// `data`, `size` and reference count, and the packet's `data`, `size`
/// and `flags` as plain values. It reads no payload byte.
///
/// `provenance` must be true of it, as [`payload_of`] states, because
/// the plan's route and its padding claim are decided by it. A plan that comes back carries raw
/// pointers into that packet's buffer; only this crate's capture acts on
/// one, so it is inert in anyone else's hands.
pub unsafe fn preflight_payload(
  pkt: *const ffmpeg_next::ffi::AVPacket,
  budget: usize,
  provenance: PayloadProvenance,
) -> Result<Option<PayloadPlan>, PacketBufferError> {
  // SAFETY: `pkt` is live per the contract above; `.buf`, `.data` and
  // `.size` are public fields on `AVPacket`, and `buf` may be null
  // (stack-allocated packets).
  let buf_ptr = unsafe { (*pkt).buf };
  let data_ptr = unsafe { (*pkt).data };
  let size_raw = unsafe { (*pkt).size };
  // **The uncarriable-payload refusal, ahead of everything.** See
  // [`TrustedPayload`]: this flag marks a payload that may be a
  // structure of pointers into other live objects rather than media
  // bytes, and copying those bytes would mint an owned-looking carrier
  // full of addresses that dangle as soon as the source is dropped.
  //
  // Judged before the empty-payload answer as well as before the copy:
  // "there is nothing to take here" is the wrong reply to a packet this
  // crate must not take *anything* from.
  //
  // SAFETY: `pkt` is live per the contract; `flags` is a public `c_int`
  // field, read as the integer it is.
  let flags_raw = unsafe { (*pkt).flags };
  if flags_raw & ffmpeg_next::ffi::AV_PKT_FLAG_TRUSTED != 0 {
    return Err(PacketBufferError::TrustedPayload(TrustedPayload::new(
      size_raw.max(0) as usize,
    )));
  }
  if buf_ptr.is_null() || data_ptr.is_null() || size_raw <= 0 {
    return Ok(None);
  }
  let len = size_raw as usize;
  // **The budget, before anything is read or allocated.** Judged on
  // the declared length rather than on what the copy turns out to
  // cost, because the point is to refuse without paying. Ahead of the
  // bounds check too: a forged `size` is exactly what both exist for,
  // and the cheaper judgement goes first.
  if len > budget {
    return Err(PacketBufferError::PacketTooLarge(PacketTooLarge::new(
      len, budget,
    )));
  }
  // SAFETY: `buf_ptr` is a live `AVBufferRef` owned by the packet.
  let buf_data = unsafe { (*buf_ptr).data };
  let size = unsafe { (*buf_ptr).size };
  if buf_data.is_null() {
    return Err(PacketBufferError::Bounds(Bounds::new(0, len, size)));
  }
  // `AVPacket` guarantees `data` lies within
  // `buf->data .. buf->data + buf->size`. Checked before the copy, not
  // instead of it: 0.8 formed a view over the claimed range and a
  // malformed `size` handed out a slice nobody read; 0.9 reads every
  // byte of it, so an unchecked claim is an out-of-bounds read rather
  // than a latent one.
  let offset = (data_ptr as usize).wrapping_sub(buf_data as usize);
  match offset.checked_add(len) {
    Some(end) if end <= size => {}
    _ => {
      return Err(PacketBufferError::Bounds(Bounds::new(offset, len, size)));
    }
  }
  // **The sharing question, asked before any byte is read.**
  //
  // Everything below reads the payload — the view lane by handing out a
  // span over it, the owned lane by copying it — so a buffer somebody
  // else references has to be classified before it is touched. What
  // matters is not the count but **who** the other holder is, and only
  // the caller of this function knows that: see [`PayloadProvenance`]
  // for the dichotomy and [`PayloadProvenance::route`] for the table.
  //
  // Placed here rather than inside the capture so the ordering is a
  // property of this function rather than of two lane impls: nothing
  // between the bounds proof and this decision touches the payload.
  //
  // SAFETY: `buf_ptr` is a live `AVBufferRef` owned by the packet;
  // `av_buffer_get_ref_count` only reads its atomic.
  let references = unsafe { ffmpeg_next::ffi::av_buffer_get_ref_count(buf_ptr.cast_const()) };
  let route = provenance.route(references != 1);
  if route == CaptureRoute::Refuse {
    return Err(PacketBufferError::SharedPayload(SharedPayload::new(
      references,
    )));
  }

  // **The capture, and the only step the two lanes spell differently.**
  // Everything above — the `TRUSTED` refusal, the empty answer, the
  // budget, the extent proof — is shared, which is what keeps the view
  // lane from having to re-earn a single one of them.
  //
  // **The padding claim is the provenance's, never the buffer's.**
  // libavcodec reads `AV_INPUT_BUFFER_PADDING_SIZE` bytes past a
  // payload, and only a packet libavformat delivered comes with padding
  // libavformat answers for. A caller's packet may have the capacity
  // and still hold anything there — bytes it never wrote, or bytes that
  // are not zero — and nothing this crate can read tells the two apart.
  // So the claim is decided by who delivered the packet, here, and the
  // view lane records it on the carrier; the send leg is the only thing
  // that reads it back — see `boundary::share_or_copy`.
  //
  // SAFETY: `offset + len` was just proved to lie inside `buf_ptr`'s
  // own `size`, and `buf_ptr` is a live `AVBufferRef` the packet owns.
  Ok(Some(PayloadPlan {
    buf: buf_ptr,
    buf_data: buf_data.cast_const(),
    offset,
    len,
    route,
    padding: provenance.vouches_for_padding(),
  }))
}

/// Acts on a [`PayloadPlan`] — **the only step that reads the payload,
/// and the only one that can fail for a reason the plan could not
/// foresee.**
///
/// # Safety
///
/// `plan` must come from [`preflight_payload`] over a packet still live
/// for this call, whose provenance claim stands; and on the view lane
/// the packet's owner must not write through its buffer while the
/// returned carrier lives.
unsafe fn capture_payload<C: crate::FfmpegCarrier + crate::CarrierOps>(
  plan: PayloadPlan,
) -> Result<C::Buffer, PacketBufferError> {
  let PayloadPlan {
    buf: buf_ptr,
    buf_data,
    offset,
    len,
    route,
    padding,
  } = plan;
  let carried = match route {
    // SAFETY: the preflight proved `offset + len` inside `buf_ptr`'s own
    // `size`, against a buffer the live packet holds. The range is the
    // packet's payload, initialised by the caller's `AVPacket` contract
    // (every exported byte written). Nothing writes through it while a
    // view lives: this route is taken only for a buffer that is unshared
    // or whose other holders cannot write (`PayloadProvenance::route`),
    // and this function's contract binds the packet's own owner. The
    // packet-payload capture — the one that claims the padding — only
    // where the provenance vouches for it: a packet libavformat
    // delivered (`PayloadProvenance::vouches_for_padding`). A caller's
    // packet takes the general capture, which claims nothing past the
    // payload, so a decoder is never handed its buffer.
    CaptureRoute::Capture if padding => unsafe { C::capture_packet_payload(buf_ptr, offset, len) },
    // SAFETY: as above, without the padding claim.
    CaptureRoute::Capture => unsafe { C::capture(buf_ptr, offset, len) },
    CaptureRoute::Copy => {
      // A demux-delivered packet whose buffer libavformat also holds.
      // Reading it here is race-free — every other reference is
      // C-owned, no `ffmpeg_next::Packet` wraps one, and this crate
      // holds the `AVFormatContext` exclusively for the duration of
      // the call — but the copy is what keeps that argument confined
      // to *this* call instead of to the carrier's whole life.
      //
      // SAFETY: the extent was proved above and `buf_data` is
      // non-null.
      let bytes = unsafe { core::slice::from_raw_parts(buf_data.add(offset), len) };
      C::from_bytes(bytes)
    }
    // Answered before the payload was touched.
    CaptureRoute::Refuse => unreachable!("a refusal returns from the preflight"),
  };
  carried.ok_or(PacketBufferError::CaptureFailed(CaptureFailed::new(len)))
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::limits::DEFAULT_MAX_PACKET_BYTES;
  use ffmpeg_next::{Packet, packet::Ref};

  #[test]
  fn a_real_payload_is_copied_out_whole() {
    let packet = Packet::copy(&[1u8, 2, 3, 4]);
    // SAFETY: `packet` owns a live `AVPacket` for the call; `CallerSupplied` is
    // true of every packet, and the owned lane copies.
    let payload = unsafe {
      payload_of::<crate::Owned>(
        packet.as_ptr(),
        DEFAULT_MAX_PACKET_BYTES,
        PayloadProvenance::CallerSupplied,
      )
    }
    .expect("a well-formed packet is carriable")
    .expect("present");
    assert_eq!(payload.as_ref(), &[1, 2, 3, 4]);
  }

  #[test]
  fn the_copy_outlives_the_packet_it_came_from() {
    // The whole point of the amputation: FFmpeg's allocation is gone
    // and the bytes are still here.
    let packet = Packet::copy(&[9u8, 8, 7]);
    // SAFETY: `packet` owns a live `AVPacket` for the call; `CallerSupplied` is
    // true of every packet, and the owned lane copies.
    let payload = unsafe {
      payload_of::<crate::Owned>(
        packet.as_ptr(),
        DEFAULT_MAX_PACKET_BYTES,
        PayloadProvenance::CallerSupplied,
      )
    }
    .expect("carriable")
    .expect("present");
    let shared = payload.clone();
    assert!(shared.ptr_eq(&payload), "the clone copied the bytes");
    drop(packet);
    drop(payload);
    assert_eq!(shared.as_ref(), &[9, 8, 7]);
  }

  #[test]
  fn an_empty_packet_has_no_payload_rather_than_a_failure() {
    let packet = Packet::empty();
    // SAFETY: `packet` owns a live `AVPacket` for the call; `CallerSupplied` is
    // true of every packet, and the owned lane copies.
    assert!(
      unsafe {
        payload_of::<crate::Owned>(
          packet.as_ptr(),
          DEFAULT_MAX_PACKET_BYTES,
          PayloadProvenance::CallerSupplied,
        )
      }
      .expect("not a failure")
      .is_none()
    );
  }

  #[test]
  fn a_payload_outside_its_own_buffer_is_refused_before_a_byte_is_read() {
    use ffmpeg_next::packet::Mut;
    let mut packet = Packet::copy(&[1u8, 2, 3, 4]);
    // SAFETY: `packet` owns a live `AVPacket`; `size` is a public
    // field. The forged claim is the read this check exists to stop.
    unsafe {
      (*packet.as_mut_ptr()).size = 1 << 20;
    }
    // SAFETY: `packet` owns a live `AVPacket` for the call; `CallerSupplied` is
    // true of every packet, and the owned lane copies.
    assert!(matches!(
      unsafe {
        payload_of::<crate::Owned>(
          packet.as_ptr(),
          DEFAULT_MAX_PACKET_BYTES,
          PayloadProvenance::CallerSupplied,
        )
      },
      Err(PacketBufferError::Bounds(_)),
    ));
  }

  #[test]
  fn the_empty_carrier_costs_no_allocation() {
    let a = FfmpegBytes::empty();
    let b = FfmpegBytes::empty();
    assert!(a.is_empty());
    assert_eq!(a.len(), 0);
    assert!(
      a.ptr_eq(&b),
      "two empties name the same nothing, which is the answer the shared singleton used to give",
    );
    // And a zero-length copy is that same nothing rather than a
    // minted allocation.
    assert!(FfmpegBytes::copy_from_slice(&[]).ptr_eq(&a));
    assert!(
      FfmpegBytes::try_copy_from_slice(&[])
        .expect("an empty copy cannot fail")
        .ptr_eq(&a),
    );
  }

  /// Equality and hashing read the span, and the two shapes that
  /// carry the same bytes with different allocations must agree.
  ///
  /// The derived implementations did not: a carrier built from a
  /// reservation keeps capacity past its span (a producer sizes its
  /// output before the conversion that fills it), so it compared and
  /// hashed differently from an exact copy of the very same bytes.
  /// Anything keyed on a payload — a cache, a dedup table — missed.
  #[test]
  fn equal_bytes_are_equal_and_hash_alike_whatever_the_allocation() {
    use core::hash::Hasher;

    fn hash_of(value: &FfmpegBytes) -> u64 {
      let mut hasher = std::collections::hash_map::DefaultHasher::new();
      core::hash::Hash::hash(value, &mut hasher);
      hasher.finish()
    }

    // Reserved wide, committed short: the allocation is sixteen bytes
    // and the span is three.
    let mut reservation = FfmpegBytes::reserve(16).expect("a sixteen-byte reservation");
    reservation[..3].copy_from_slice(&[7u8, 8, 9]);
    let reserved = FfmpegBytes::from_reservation(reservation, 3);
    // Copied exactly: allocation and span are both three bytes.
    let exact = FfmpegBytes::try_copy_from_slice(&[7u8, 8, 9]).expect("a three-byte copy");

    assert_eq!(reserved.as_slice(), exact.as_slice());
    assert_eq!(
      reserved, exact,
      "capacity a consumer cannot read is not part of the value",
    );
    assert_eq!(
      hash_of(&reserved),
      hash_of(&exact),
      "Hash must agree with Eq or a payload-keyed map misses",
    );
    assert!(
      !reserved.ptr_eq(&exact),
      "and ptr_eq still answers the other question: these are two allocations",
    );

    // The empty carrier and a zero-length span are the same value too.
    // `from_reservation` folds a zero length to `Empty`, so this holds
    // by construction today; it is asserted because the `Eq` above is
    // what keeps it true if a second storage arm ever lands and stops
    // folding.
    let empty_reservation = FfmpegBytes::reserve(8).expect("an eight-byte reservation");
    let zero_span = FfmpegBytes::from_reservation(empty_reservation, 0);
    assert_eq!(zero_span, FfmpegBytes::empty());
    assert_eq!(hash_of(&zero_span), hash_of(&FfmpegBytes::empty()));
  }

  #[test]
  fn copy_out_is_owned_and_shareable() {
    fn owned_and_shareable<T: Send + Sync + Clone + 'static>(_: &T) {}
    let carrier = FfmpegBytes::copy_from_slice(&[4u8, 5, 6]);
    owned_and_shareable(&carrier);
    assert_eq!(carrier.as_ref(), &[4, 5, 6]);
    // Terse `Debug` — the bytes never reach a log line through it.
    let rendered = format!("{carrier:?}");
    assert!(rendered.contains("len: 3"), "got {rendered}");
    assert!(!rendered.contains('4'), "got {rendered}");
  }
}

/// Where the packet a payload is taken from came from — and therefore
/// what a second reference to its buffer can do.
///
/// The dichotomy is **delivered by libavformat** versus **handed over
/// by a caller**, and it is about who can *write*, not how many
/// references there are.
///
/// * A packet this crate's own read loop just took from
///   `av_read_frame` — or hoisted out of `AVStream.attached_pic` — may
///   well share its buffer, and every other reference to it is
///   libavformat's. FFmpeg writes through a buffer only after
///   `av_buffer_make_writable`, which *copies* when the buffer is
///   shared; and while this crate is reading, it holds the
///   `AVFormatContext` exclusively, so no libavformat code is running
///   at all. There is no safe-Rust `data_mut` on any of those
///   references, because no `ffmpeg_next::Packet` wraps them.
/// * A packet a **caller** hands over may share its buffer with
///   another `ffmpeg_next::Packet` — and that type's `data_mut` writes
///   in place from safe code, without consulting writability. That is
///   the writer the uniqueness rule exists for, and it may be on
///   another thread.
///
/// Spelled out as a parameter rather than assumed at the call sites,
/// because a blanket "refcount must be one" looked right and was twice
/// wrong: it refused every embedded cover picture, and then every
/// packet from a queue-backed subtitle demuxer, which delivers
/// `av_packet_ref`s of originals it keeps in its own queue.
///
/// # And what follows the payload
///
/// The same dichotomy decides the one claim a carrier may make about the
/// bytes **after** its payload — see [`Self::vouches_for_padding`].
/// libavcodec reads `AV_INPUT_BUFFER_PADDING_SIZE` bytes past every
/// payload it is sent, and libavformat delivers each packet with that
/// padding inside its buffer: zeroes it allocated, or the rest of the
/// very packet a parser split the payload from — bytes libavformat
/// wrote, which libavcodec is built to read past. **The library never
/// vouches for a caller's buffer.** A caller's packet may have the
/// capacity and hold anything in it — bytes nobody wrote, or bytes that
/// are not zero — and no reading of the buffer tells those apart from
/// padding. Its carrier claims nothing past the payload, and a decoder
/// is handed a padded, zeroed copy of it instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PayloadProvenance {
  /// A packet a caller handed to a public conversion. A second
  /// reference may be a `Packet` with a safe `data_mut`; shared is
  /// refused by name. Claims nothing about the bytes after the payload.
  CallerSupplied,
  /// A packet this crate's demux loop just received from
  /// `av_read_frame`. Secondary references are libavformat's own, and
  /// so is the padding behind the payload.
  DemuxDelivered,
  /// The container's parked picture, whether hoisted at open or queued
  /// as a stream's first packet. Written once, with its padding, while
  /// the container was opened and never again.
  AttachedPicture,
}

/// How a payload of a given provenance may be captured once its extent
/// is proved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CaptureRoute {
  /// The ordinary road: the view lane takes a window, the owned lane
  /// copies.
  Capture,
  /// Both lanes copy. The bytes are safe to *read* — no safe-Rust
  /// writer exists — but no long-lived window may be opened onto a
  /// buffer somebody else also holds unless something stronger than
  /// "nobody is writing right now" is true of it.
  Copy,
  /// Refuse without reading a byte.
  Refuse,
}

impl PayloadProvenance {
  /// Whether a carrier captured under this provenance may claim the
  /// padding behind its payload — the claim that lets the view lane
  /// share the payload's buffer with a decoder rather than copy it.
  ///
  /// | provenance | claims the padding | the argument |
  /// |---|---|---|
  /// | [`Self::CallerSupplied`] | **no** | the library never vouches for a caller's buffer: what follows the payload may be unwritten or nonzero, and is indistinguishable from padding |
  /// | [`Self::DemuxDelivered`] | yes | libavformat delivered the packet, and every packet it delivers carries `AV_INPUT_BUFFER_PADDING_SIZE` bytes of its own writing behind the payload |
  /// | [`Self::AttachedPicture`] | yes | libavformat built the parked picture the same way, with the same padding |
  ///
  /// The extent is a second, separate question — whether the padding
  /// still fits behind a view — which the send leg asks of the carrier
  /// itself (`boundary::share_or_copy`).
  #[inline]
  pub(crate) const fn vouches_for_padding(self) -> bool {
    match self {
      Self::CallerSupplied => false,
      Self::DemuxDelivered | Self::AttachedPicture => true,
    }
  }

  /// What to do with a payload whose buffer has `shared` other
  /// references.
  ///
  /// | provenance | unique | shared | the argument |
  /// |---|---|---|---|
  /// | [`Self::CallerSupplied`] | capture | **refuse** | a second `Packet`'s `data_mut` writes in place from safe code, possibly on another thread |
  /// | [`Self::DemuxDelivered`] | capture | **copy** | the read is race-free — every other reference is C-owned and the context is held exclusively — but a *window* would outlive that exclusivity, and FFmpeg's copy-on-write discipline is a weaker guarantee than this crate wants under a long-lived span |
  /// | [`Self::AttachedPicture`] | capture | **capture** | `AVStream.attached_pic` is written once while the container opens and never again, so a window onto it is as stable as one onto a private buffer |
  ///
  /// The middle row is the deliberate one. Sharing there would have
  /// rested on "libavformat honours its own writability rules
  /// forever"; copying rests on "nothing can be writing while we hold
  /// the context", which is a fact about *this* call and needs no
  /// promise about anyone's future behaviour. Subtitle queues — the
  /// shape that produced this row — carry payloads measured in bytes,
  /// so the copy is not a cost worth an argument.
  #[inline]
  pub(crate) const fn route(self, shared: bool) -> CaptureRoute {
    match (self, shared) {
      (_, false) | (Self::AttachedPicture, true) => CaptureRoute::Capture,
      (Self::DemuxDelivered, true) => CaptureRoute::Copy,
      (Self::CallerSupplied, true) => CaptureRoute::Refuse,
    }
  }
}

/// A packet whose payload buffer somebody else still references.
///
/// **Refused without reading a byte of it, and that is the whole
/// point.** A refcount above one is exactly the state in which another
/// handle to the same allocation may exist — `ffmpeg_next::Packet`
/// hands out `&mut [u8]` through `data_mut` from entirely safe code,
/// and a `Packet` is `Send`, so that handle may be on another thread
/// writing right now. Forming a `&[u8]` over those bytes is a data race
/// whether the bytes are then viewed *or copied*: the copy needs the
/// read, and the read is the race.
///
/// An earlier round answered this shape with a silent copy, reasoning
/// that a copy is always sound and keeps the API total. That was wrong
/// in the direction that matters — it traded soundness for totality.
/// The refcount protects the allocation's *lifetime*; it says nothing
/// about who may be writing into it.
///
/// The ordinary roads never see this: a packet from `av_read_frame` is
/// uniquely referenced, and so is one the caller cloned successfully.
/// What produces it is a second reference the caller may not know they
/// have — see `ffmpeg_next::Packet::clone`, which ignores
/// `av_packet_make_writable`'s return code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error(
  "packet payload buffer is shared ({references} references): its bytes cannot be read \
   without racing whoever else holds it"
)]
pub struct SharedPayload {
  references: i32,
}

impl SharedPayload {
  /// Constructs a `SharedPayload` payload.
  #[inline]
  #[must_use]
  pub const fn new(references: i32) -> Self {
    Self { references }
  }

  /// References the payload's buffer had when it was refused.
  #[inline]
  #[must_use]
  pub const fn references(&self) -> i32 {
    self.references
  }
}

/// Payload for [`PacketBufferError::CaptureFailed`].
///
/// The proofs all passed and the carrier still could not be formed:
/// `av_buffer_alloc` returned null on the owned lane, or
/// `av_buffer_ref` did on the view lane. Distinct from
/// [`Bounds`] on purpose — a malformed packet and an exhausted
/// allocator are different facts, and 0.8 reported both as an absent
/// payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("could not capture a {len}-byte payload: the allocator or the refcount refused")]
pub struct CaptureFailed {
  len: usize,
}

impl CaptureFailed {
  /// Constructs a `CaptureFailed` payload.
  #[inline]
  pub const fn new(len: usize) -> Self {
    Self { len }
  }
  /// Bytes the capture was for.
  #[inline]
  pub const fn len(&self) -> usize {
    self.len
  }
  /// Whether the refused capture was of no bytes.
  #[inline]
  pub const fn is_empty(&self) -> bool {
    self.len == 0
  }
}
