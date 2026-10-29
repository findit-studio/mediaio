//! Adapter traits — the per-kind backend "vocabulary."
//!
//! A backend implements only the kinds it handles. R3D / BRAW /
//! ARRIRAW / X-OCN / Canon RAW Light implement only [`VideoAdapter`].
//! FFmpeg implements all four. The buffer type is **not** part of
//! these traits — it's a struct generic on `Packet` / `Frame` so the
//! same adapter can be used with different buffer types at different
//! call sites.
//!
//! # The carrier amputation contract
//!
//! Every packet and frame in this crate carries a `D` — the buffer
//! type its bytes live in — and every backend chooses what to put
//! there. This is the one law that choice has to obey:
//!
//! > **A backend's `D` carrier must be owned, `Send + Sync`, and
//! > cheap to clone (a refcount bump), with its lifetime fully
//! > decoupled from the backend's internal buffers at the exit. No
//! > backend-internal lifetime — an FFI pointer, a pooled buffer, a
//! > JavaScript handle — crosses the seam.**
//!
//! The bytes are copied **once**, at the boundary, and what leaves is
//! Rust-owned memory. A backend that hands out a view into its own
//! allocation instead has not exported a frame; it has exported a
//! borrow with the lifetime erased, and every consumer downstream
//! inherits a rule it cannot see: hold this only as long as the
//! decoder lives, do not send it to another thread, do not put it in
//! a cache. A graph made of such frames cannot fan a message out to
//! two consumers without the backend's permission.
//!
//! What the contract buys, in the order it matters:
//!
//! - **`Send + Sync`.** A frame can cross a channel and be *read* from
//!   several threads at once. Refcounted-view carriers are routinely
//!   `Send` and not `Sync`, which is exactly the shape that makes
//!   fan-out impossible.
//! - **A lifetime that is nobody's business.** The decoder can be
//!   dropped, flushed, seeked, or reopened while a frame it produced is
//!   still in flight.
//! - **Clone is a refcount bump.** The message-carrier law (see
//!   [`TrackInfo`](crate::demuxer::TrackInfo)) says `Clone` on a message
//!   is never a deep copy; a carrier that is already owned and
//!   refcounted keeps that true for free.
//!
//! The cost is one copy per exit, and it is the honest one: the
//! alternative was not zero copies, it was a copy the consumer had to
//! make itself, later, without knowing why.
//!
//! ## The corollary: some payloads cannot be carried at all
//!
//! Copying bytes only produces ownership when the bytes *are* the
//! payload. A backend may be handed something whose bytes are
//! **addresses of other live objects** — and copying those yields a
//! carrier that is owned by every type-level test the contract states,
//! `Send + Sync + 'static`, clone-is-a-refcount-bump, and that dangles
//! the instant its source is dropped. The copy moved the pointer; it
//! could not move what the pointer names.
//!
//! > **A payload that carries addresses instead of bytes is
//! > uncarriable.** A backend that meets one must refuse it by name, on
//! > *both* legs — the one that takes payloads out of the backend and
//! > the one that hands them back in — and never mint a carrier for it.
//!
//! There is no bound on what such a payload's pointers might reach, so
//! there is no depth of copying that would make it safe; "deeply own
//! the referenced objects" is not a smaller version of this problem,
//! it is a different backend. Refusal is the correct answer, not a
//! conservative one.
//!
//! FFmpeg's `AV_PKT_FLAG_TRUSTED` is the concrete instance —
//! wrapped-`AVFrame` producers use it for packets whose body is an
//! `AVFrame` pointer structure passed between components inside one
//! pipeline — and `mediadecode-ffmpeg` refuses it at both legs. A
//! backend with an equivalent (a handle table, a shared-memory cookie,
//! an object id) has the same duty.
//!
//! **This crate does not name the carrier.** No signature here or in
//! the faces built on it mentions a concrete buffer type; `D` and
//! `Buffer` stay generic and their bounds stay minimal
//! (`AsRef<[u8]>`), so a consumer states what it spends. What
//! satisfies the contract is the backend's use-site choice —
//! `mediadecode-ffmpeg` binds an opaque `FfmpegBytes` over an
//! `alloc::sync::Arc<[u8]>`, `mediadecode-webcodecs` binds its own
//! `Arc`-backed view — and a `no_std` backend with a static arena can
//! satisfy it too. The core neither knows nor cares which, which is
//! also what lets a backend change its storage without changing its
//! frames.
//!
//! # The two carrier lanes
//!
//! *User-ruled 2026-08-25.*
//!
//! The amputation contract above governs one lane. A backend may offer
//! a second, and `mediadecode-ffmpeg` does — both first-class, neither
//! a feature flag on the other:
//!
//! * the **view** lane hands out a refcounted handle onto the
//!   allocation the substrate already made. Nothing is copied. It is
//!   the **default**, because the ordinary consumer decodes in place:
//!   read the frame, use it, drop it, decode on. Paying for a copy of
//!   bytes you were going to discard is a cost with nothing on the
//!   other side of it.
//! * the **owned** lane is the amputation contract: every byte copied
//!   once at the boundary into memory the caller's language owns.
//!
//! ## Which lane, and why
//!
//! | | view (default) | owned |
//! |---|---|---|
//! | cost per frame | none | one copy |
//! | `Send` | yes | yes |
//! | `Sync` | **no** | yes |
//! | `Clone` | refcount bump | refcount bump |
//! | lifetime | pinned to the backend's pools | answerable to nobody |
//! | fan-out to N consumers | no | yes |
//! | outlives the decoder | the buffer does; the **pool slot** stays out | freely |
//! | pool recycling | native — it *is* the substrate's pool | needs one built (see the FFmpeg backend's issue #35) |
//!
//! And per payload shape, which is where a lane stops being a slogan:
//!
//! | payload | view | owned |
//! |---|---|---|
//! | compressed packet | window onto the demuxer's buffer | copy |
//! | packet **submitted** to a decoder | shared when the payload's provenance and extent both prove the substrate may read past it, else copied | copy |
//! | packet **handed back to the caller** | copy | copy |
//! | tight video/image plane | window, at the decoder's own stride | copy, at the decoder's own stride |
//! | padded video/image plane | **copy**, compacted to `row_bytes` | copy, compacted to `row_bytes` |
//! | audio plane | window over **exactly** the valid samples | copy of exactly the valid samples |
//! | resampled plane | window onto the resampler's output frame | copy |
//! | subtitle rect, side data, extradata | **copy** — no refcount exists to share | copy |
//!
//! ## The two rules those rows come from
//!
//! **A view stops where the writing stopped.** A carrier is an
//! `AsRef<[u8]>`, so the span it names is a span a consumer may read —
//! and a span is only nameable if every byte in it was written. An
//! audio plane is allocated with alignment padding past its samples and
//! a view stops at the samples, because exposing the rest through a
//! window is the same information leak it would have been through a
//! copy. The lanes differ in who owns the bytes, never in which bytes
//! exist.
//!
//! **Sharing is conditional, and the condition is a proof.** Where the
//! extent is provably all output, the view lane takes a reference;
//! where it is not, the view lane copies and says so. A padded video
//! plane is the standing example: only the first `row_bytes` of each
//! `linesize`-wide row are the decoder's, so *no* lane can hand out the
//! padded span, and both compact it. Which is why the two lanes carry
//! identical **content** while their strides and spans may differ — a
//! parity check between them compares pixels, not buffers.
//!
//! ## The rule that decides which conversions may exist
//!
//! **A conversion that borrows its source can only copy.** Substrate
//! packet and frame types share their buffers by refcount and hand out
//! mutable slices with no copy-on-write, so a caller who still holds
//! the source holds a mutable alias of every byte a view would read —
//! from entirely safe code, on either thread, `!Sync` notwithstanding.
//! There is no signature that borrows a source and returns a shared
//! view soundly.
//!
//! So the view lane's conversions **consume**: hand the packet or frame
//! over, and the carrier that comes back is the only thing left
//! pointing at those bytes. The borrowing conversions are the owned
//! lane, where copying makes the source's fate irrelevant. Two shapes,
//! one law, and the compiler enforces the difference — a program that
//! keeps the source and mutates it no longer type-checks.
//!
//! This falls out naturally for a backend's own roads: a demuxer owns
//! the packet it just read, a decoder owns the frame it just decoded,
//! and a resampler owns its output. The zero-copy chain is intact
//! end to end; what changed is that a *caller* cannot ask for a view of
//! something they are still holding.
//!
//! ## Two rules that came from being wrong once
//!
//! **A shared buffer is refused, not copied.** When a payload's buffer
//! is referenced by a handle other than the packet it came from, this
//! backend declines to carry it *by name* — and the reason is sharper
//! than it first looks. The obvious answer is to copy: a copy is
//! always sound, and it keeps the API total. But a copy needs a
//! **read**, and a second reference held by safe code is exactly the
//! state in which somebody may be writing those bytes right now, from
//! another thread. The read is the race. Totality is not worth that,
//! and a refcount protects an allocation's lifetime while saying
//! nothing about its contents.
//!
//! The rule is about *who* holds the other reference, not about the
//! count. A substrate that keeps its own reference to a container-held
//! payload — cover art is the standing example — is not the hazard a
//! caller's second, mutable handle is, and refusing it would have
//! refused every embedded picture there is.
//!
//! **What a decoder hands back, it copies.** "Built, submitted and
//! dropped inside one call" is a claim about a function, and it stops
//! being true when the thing you submit to *keeps* what it is given by
//! reference and hands it back. A hardware probe that records packets
//! so it can replay them after a fallback, and then hands that history
//! to the caller as owned mutable packets, would turn a scoped
//! submission into an escape. So the recorder keeps copies of its own,
//! and the send stays zero-copy on every road — before the probe
//! commits as after it, hardware or software.
//!
//! ## Two more rules the send direction adds
//!
//! **A payload a caller holds owns its bytes.** Going *into* a decoder
//! is the one direction where a backend could hand the substrate its
//! own buffer — but a substrate's packet type is usually mutable, and a
//! value a caller holds can be asked for that mutable view while the
//! carrier it came from is still readable. That is an aliasing `&mut`
//! out of entirely safe code, and being `!Sync` does not prevent it:
//! one thread suffices. So the zero-copy send is **scoped** — built,
//! submitted and dropped inside the backend, never surfacing as a value
//! anyone can hold — and every packet a public API returns is a copy on
//! both lanes.
//!
//! **Trailing capacity is not padding.** Decoders read a fixed number
//! of bytes past a packet's payload, and a container's own packets
//! carry exactly that much zeroed slack behind them. Bytes merely
//! *existing* after a view is not the same fact: a video plane has more
//! pixels after it and a resampled plane has more samples, and a
//! bitstream reader running past the payload would consume either as
//! though it were bitstream. So a carrier records **where it came
//! from**, at capture, and only a payload captured out of a packet may
//! be shared back into a decoder — everything else is copied into a
//! properly padded one.
//!
//! **Take the view lane** when a consumer reads a frame and is done
//! with it — a thumbnailer, a probe, a transcode step, an analysis pass
//! that reduces each frame to numbers. This is most consumers, which is
//! why it is the default.
//!
//! **Take the owned lane** when a frame has to *travel*: into a graph
//! that fans it out to several consumers, across a channel to threads
//! that will both read it, into a cache that outlives the session, or
//! anywhere `Sync` is required. `mediagraph` is on the owned lane and
//! belongs there — a graph node cannot hold a pool slot for as long as
//! a graph might hold a message.
//!
//! ## The pool-hostage warning
//!
//! A view carrier keeps the substrate's buffer alive. That is the
//! point, and it is also the catch: **a frame held is a pool slot
//! held.** Decoders allocate from a fixed pool, and one that runs out
//! blocks or fails. A consumer that parks view frames in a queue is a
//! consumer that stalls its own decoder, and the symptom — a decode
//! that mysteriously stops making progress — points nowhere near the
//! queue that caused it.
//!
//! So the view lane's rule is: **read in place, drop, decode on.** If a
//! frame needs to outlive the loop that produced it, that is the
//! question the owned lane answers.
//!
//! ## The symmetry worth noticing
//!
//! The two lanes converge from opposite directions. The view lane rides
//! the substrate's own buffer recycling natively, because its carrier
//! *is* a reference into that pool. The owned lane copies, and copying
//! per frame is an allocator call per frame — which is why an owned
//! backend eventually wants a pool of its own to recycle its copies
//! into.
//!
//! One lane gets pooling for free and pays in lifetime; the other gets
//! lifetime for free and pays for pooling. Neither is the better
//! answer, which is why both are first-class.
//!
//! # The resource governance contract
//!
//! *User-ruled 2026-08-25.*
//!
//! A decoding backend stands between a caller and a substrate — a C
//! library, a browser API, a driver — and every one of those substrates
//! allocates memory in response to bytes an attacker chose. A backend
//! therefore owes the caller an answer to "how much can this cost?",
//! and the honest answer has three tiers, not one. Stating where they
//! end is part of the contract: a boundary that is never written down
//! gets rediscovered, one review round at a time, as though it were a
//! defect.
//!
//! ## Tier one — what the backend allocates itself
//!
//! **Every byte a backend copies or allocates is bounded, by a named
//! limit or by a format.** No exceptions and no third kind: a buffer
//! whose size comes from a file answers to a configured ceiling, and a
//! buffer whose size is a property of a format answers to that format.
//! A site that appears to be neither has not been thought about yet.
//!
//! This tier is provable, and it is proved by enumeration rather than
//! by assertion: a backend is expected to keep an accounting of its own
//! allocation sites and what bounds each one.
//! `mediadecode-ffmpeg` keeps that table in its `buffer` module.
//!
//! Two rules this tier has cost real defects to learn:
//!
//! - **A judge must dominate the allocator's arithmetic, not the
//!   payload's.** A budget compared against what the bytes nominally
//!   weigh is not a budget on what will be spent. Allocators align,
//!   pad, and round; on ordinary inputs the difference is under one
//!   percent, which is exactly why under-pricing hides.
//! - **Everything a conversion can refuse is refused before anything it
//!   can allocate is allocated.** A correct refusal that arrives after
//!   the expensive half of the work is a correct refusal that did not
//!   help.
//!
//! ## Tier two — the substrate's own options
//!
//! **A backend sets every resource option its substrate offers, at every
//! interposition point the substrate exposes.** These are not the
//! backend's allocations; they are the substrate's, intercepted where
//! the substrate agreed to be interrupted — a ceiling field, an
//! allocation callback, a negotiation hook, a custom I/O layer.
//!
//! This tier is **defense in depth, and it is not a proof.** Each option
//! bounds what that option was built to bound, and the union of them is
//! whatever the substrate's authors chose to make interruptible. A
//! backend is obliged to use all of them and obliged not to claim that
//! using them is the same as bounding the substrate.
//!
//! `mediadecode-ffmpeg` enumerates its options — what each one is, and
//! what each one bounds — in its `buffer` module, beside the tier-one
//! table.
//!
//! ## Tier three — the boundary
//!
//! **Allocations internal to the substrate, past its option surface, are
//! the substrate's territory.** A parser can describe, in a handful of
//! bytes, a structure whose in-memory form is far larger, and nothing
//! outside that parser can observe it happen. A driver can size a pool
//! however it likes behind a declared extent. Where a substrate offers
//! no interposition point, a backend outside it has none either.
//!
//! **This crate does not promise to be a hypervisor for its
//! substrates.** It promises tier one, it performs tier two, and it
//! names tier three rather than papering over it. Bounding the input to
//! an amplification is achievable and is done — a parser cannot
//! allocate from bytes it was never handed — but bounding the output is
//! the substrate's own hardening work.
//!
//! **A deployment that needs a hard memory bound places the decode
//! behind an OS-level instrument**: an address-space or memory rlimit,
//! a cgroup, or a memory-limited worker process it can restart. That is
//! the industry answer for `libav*` and for every comparable substrate,
//! and it is the only instrument that actually bounds a foreign
//! allocator. The limits in tiers one and two **compose with** it — they
//! turn most hostile inputs into a named error instead of a killed
//! worker, and they make the worker's limit a backstop rather than a
//! first line — but they do not replace it, and a caller who treats
//! them as a replacement has been told otherwise here.

use core::fmt::Debug;

/// Backend vocabulary for compressed/decoded **video**.
pub trait VideoAdapter {
  /// Codec identifier (e.g. backend-specific newtype around
  /// FFmpeg `AVCodecID`, WebCodecs codec string, etc.).
  type CodecId: Copy + Eq + Debug;
  /// Pixel format identifier (e.g. backend-specific newtype around
  /// FFmpeg `AVPixelFormat`, WebCodecs `VideoPixelFormat`, RAW
  /// `VideoPixelType`, BRAW `BlackmagicRawResourceFormat`).
  ///
  /// `Clone`, not `Copy`, for the same reason
  /// [`AudioAdapter::ChannelLayout`] is: mediaframe 0.3's
  /// `PixelFormat` carries an owned `Other(SmolStr)` arm at the
  /// `alloc` tier, and that is the identifier the FFmpeg and
  /// WebCodecs adapters bind here.
  type PixelFormat: Clone + Eq + Debug;
  /// Backend-specific extras carried on every `VideoPacket` (e.g.
  /// FFmpeg side-data, WebCodecs metadata).
  type PacketExtra;
  /// Backend-specific extras carried on every `VideoFrame` (e.g.
  /// HDR mastering display, RAW sensor metadata, picture type).
  type FrameExtra;
}

/// Backend vocabulary for compressed/decoded **audio**.
pub trait AudioAdapter {
  /// Codec identifier.
  type CodecId: Copy + Eq + Debug;
  /// Sample format identifier (e.g. FFmpeg `AVSampleFormat`,
  /// WebCodecs `AudioSampleFormat`).
  type SampleFormat: Copy + Eq + Debug;
  /// Channel layout identifier (FFmpeg `AVChannelLayout`,
  /// WebCodecs raw count, RAW SDK fixed layouts).
  type ChannelLayout: Clone + Eq + Debug;
  /// Backend-specific extras carried on every `AudioPacket`.
  type PacketExtra;
  /// Backend-specific extras carried on every `AudioFrame`.
  type FrameExtra;
}

/// Backend vocabulary for compressed/decoded **subtitles**.
pub trait SubtitleAdapter {
  /// Codec identifier.
  type CodecId: Copy + Eq + Debug;
  /// Backend-specific extras carried on every `SubtitlePacket`.
  type PacketExtra;
  /// Backend-specific extras carried on every `SubtitleFrame`.
  type FrameExtra;
}

/// Backend vocabulary for a compressed/decoded **still image** — cover
/// art, an embedded thumbnail, a poster frame.
///
/// A separate vocabulary from [`VideoAdapter`] rather than a reuse of
/// it, because the two disagree about the one thing an adapter exists
/// to name: what rides the frame. A still's extras are EXIF, an ICC
/// profile, an orientation — not a picture type, not a field order,
/// not a best-effort timestamp. Bending [`VideoAdapter::FrameExtra`]
/// over both would give every motion frame field that only a still
/// fills and every still field that only motion fills.
///
/// The compressed side is an
/// [`AttachmentPacket`](crate::demuxer::AttachmentPacket): a still
/// image inside a container is an attachment, one whole file, no
/// timeline. [`PacketExtra`](Self::PacketExtra) is therefore the
/// attachment's extras, and a backend that also implements
/// [`DemuxAdapter`](crate::demuxer::DemuxAdapter) will normally bind
/// the same type in both slots — the packet a demuxer hands out is the
/// packet an image decoder is fed.
pub trait ImageAdapter {
  /// Codec identifier — the still's own codec (MJPEG, PNG, BMP, …),
  /// in the same namespace the rest of the backend uses.
  type CodecId: Copy + Eq + Debug;
  /// Pixel format identifier.
  ///
  /// `Clone`, not `Copy`, for the same reason
  /// [`VideoAdapter::PixelFormat`] is.
  type PixelFormat: Clone + Eq + Debug;
  /// Backend-specific extras carried on the
  /// [`AttachmentPacket`](crate::demuxer::AttachmentPacket) an image
  /// decoder is fed.
  type PacketExtra;
  /// Backend-specific extras carried on every
  /// [`ImageFrame`](crate::frame::ImageFrame) — EXIF, ICC profile,
  /// side data.
  type FrameExtra;
}

#[cfg(test)]
mod tests {
  use super::*;

  /// Zero-sized "loopback" adapter that implements all three traits
  /// with `()` extras. Proves the traits are object-safe-ish in the
  /// associated-type sense (i.e. they can be implemented).
  pub struct Loopback;

  impl VideoAdapter for Loopback {
    type CodecId = u32;
    type PixelFormat = u32;
    type PacketExtra = ();
    type FrameExtra = ();
  }

  impl AudioAdapter for Loopback {
    type CodecId = u32;
    type SampleFormat = u32;
    type ChannelLayout = u32;
    type PacketExtra = ();
    type FrameExtra = ();
  }

  impl SubtitleAdapter for Loopback {
    type CodecId = u32;
    type PacketExtra = ();
    type FrameExtra = ();
  }

  impl ImageAdapter for Loopback {
    type CodecId = u32;
    type PixelFormat = u32;
    type PacketExtra = ();
    type FrameExtra = ();
  }

  #[test]
  fn loopback_compiles() {
    // The fact that this test compiles means the four traits
    // are implementable. No runtime assertions necessary.
    fn _video<A: VideoAdapter>() {}
    fn _audio<A: AudioAdapter>() {}
    fn _subtitle<A: SubtitleAdapter>() {}
    fn _image<A: ImageAdapter>() {}
    _video::<Loopback>();
    _audio::<Loopback>();
    _subtitle::<Loopback>();
    _image::<Loopback>();
  }
}
