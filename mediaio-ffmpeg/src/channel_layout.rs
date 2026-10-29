//! Conversions from FFmpeg's [`ffmpeg_next::ChannelLayout`] /
//! [`ffmpeg_next::ffi::AVChannelOrder`] to the channel-layout vocabulary
//! [`mediaframe`] owns ([`ChannelLayout`], [`ChannelOrder`],
//! [`ChannelSpec`], [`ChannelLayoutDescription`]).
//!
//! These live as **free functions** (not `From` trait impls) because of
//! Rust's orphan rule: this crate owns neither `From` nor
//! `mediaframe::audio::*`, so we can't write the `impl` here. Calling
//! `mediaio_ffmpeg::channel_layout_description_from_ffmpeg(layout)`
//! is the ergonomic boundary instead.
//!
//! FFmpeg's own type is imported as [`AvChannelLayout`] so the name
//! [`ChannelLayout`] can stay with the vocabulary these functions
//! produce.

use core::{ffi::c_char, slice, str::FromStr};

use ffmpeg_next::{ChannelLayout as AvChannelLayout, ffi};
use mediaframe::audio::{ChannelLayout, ChannelLayoutDescription, ChannelOrder, ChannelSpec};
use smol_bytes::Utf8Bytes;
use std::vec::Vec;

/// Maps an FFmpeg [`AvChannelLayout`] to the named
/// [`ChannelLayout`] vocabulary.
///
/// Two rungs, in order:
///
/// 1. the **constant-arm table** — exactly `ffmpeg_next`'s
///    `ChannelLayout` constant set, compared through
///    `av_channel_layout_compare`;
/// 2. the **describe rung** — for a layout that falls off the table,
///    FFmpeg names it via `av_channel_layout_describe` and that name
///    goes through [`ChannelLayout`]'s own total door (`FromStr`).
///
/// The second rung is what makes `binaural` / `5.1.2` / `9.1.6`
/// reachable: [`ChannelLayout`] names all three, `ffmpeg_next` 9.0.0
/// mints no constant for any of them, so the table alone can never
/// produce them. It is also why a layout a *later* FFmpeg adds is
/// reachable with no edit here, as long as the vocabulary already
/// names it — FFmpeg speaks the name, the vocabulary reads the word,
/// one source.
///
/// Returns [`ChannelLayout::default`] — the `Other("")` absent sentinel
/// — when neither rung names the layout. The rendering itself is not
/// smuggled into `Other`: an unrecognised layout stays *absent*, and
/// [`ChannelLayoutDescription::text`] is where its FFmpeg rendering
/// lives.
pub fn channel_layout_from_ffmpeg(
  value: &AvChannelLayout,
) -> Result<ChannelLayout, ChannelLayoutFault> {
  // One road for both exported conversions; see
  // [`channel_layout_description_from_ffmpeg`] for what the safe one
  // will and will not touch.
  Ok(
    channel_layout_description_from_ffmpeg(value)?
      .known_kind()
      .clone(),
  )
}

/// The constant-arm table — the first and authoritative rung of
/// [`channel_layout_from_ffmpeg`]. `None` means the layout fell off the
/// table and the caller should try the describe rung.
///
/// The arm list is exactly `ffmpeg_next`'s `ChannelLayout` constant set:
/// its `_7POINT1_TOP_BACK` is a `#define` alias of
/// `AV_CH_LAYOUT_5POINT1POINT2_BACK` and so has no arm of its own, and
/// its `BINAURAL` / `_5POINT1POINT2` / `_9POINT1POINT6` siblings — which
/// [`ChannelLayout`] does name — have no constant to match against.
fn mapped_constant(value: &AvChannelLayout) -> Option<ChannelLayout> {
  let named = match () {
    () if value.eq(&AvChannelLayout::MONO) => ChannelLayout::Mono,
    () if value.eq(&AvChannelLayout::STEREO) => ChannelLayout::Stereo,
    () if value.eq(&AvChannelLayout::STEREO_DOWNMIX) => ChannelLayout::StereoDownmix,
    () if value.eq(&AvChannelLayout::SURROUND) => ChannelLayout::Ch3_0,
    () if value.eq(&AvChannelLayout::QUAD) => ChannelLayout::Quad,
    () if value.eq(&AvChannelLayout::HEXAGONAL) => ChannelLayout::Hexagonal,
    () if value.eq(&AvChannelLayout::OCTAGONAL) => ChannelLayout::Octagonal,
    () if value.eq(&AvChannelLayout::HEXADECAGONAL) => ChannelLayout::Hexadecagonal,
    () if value.eq(&AvChannelLayout::CUBE) => ChannelLayout::Cube,
    () if value.eq(&AvChannelLayout::_2POINT1) => ChannelLayout::Ch2_1,
    () if value.eq(&AvChannelLayout::_2_1) => ChannelLayout::Ch3_0Back,
    () if value.eq(&AvChannelLayout::_2_2) => ChannelLayout::QuadSide,
    () if value.eq(&AvChannelLayout::_3POINT1) => ChannelLayout::Ch3_1,
    () if value.eq(&AvChannelLayout::_3POINT1POINT2) => ChannelLayout::Ch3_1_2,
    () if value.eq(&AvChannelLayout::_4POINT0) => ChannelLayout::Ch4_0,
    () if value.eq(&AvChannelLayout::_4POINT1) => ChannelLayout::Ch4_1,
    () if value.eq(&AvChannelLayout::_5POINT0) => ChannelLayout::Ch5_0,
    () if value.eq(&AvChannelLayout::_5POINT0_BACK) => ChannelLayout::Ch5_0Back,
    () if value.eq(&AvChannelLayout::_5POINT1) => ChannelLayout::Ch5_1,
    () if value.eq(&AvChannelLayout::_5POINT1_BACK) => ChannelLayout::Ch5_1Back,
    () if value.eq(&AvChannelLayout::_5POINT1POINT2_BACK) => ChannelLayout::Ch5_1_2Back,
    () if value.eq(&AvChannelLayout::_5POINT1POINT4_BACK) => ChannelLayout::Ch5_1_4Back,
    () if value.eq(&AvChannelLayout::_6POINT0) => ChannelLayout::Ch6_0,
    () if value.eq(&AvChannelLayout::_6POINT0_FRONT) => ChannelLayout::Ch6_0Front,
    () if value.eq(&AvChannelLayout::_6POINT1) => ChannelLayout::Ch6_1,
    () if value.eq(&AvChannelLayout::_6POINT1_BACK) => ChannelLayout::Ch6_1Back,
    () if value.eq(&AvChannelLayout::_6POINT1_FRONT) => ChannelLayout::Ch6_1Front,
    () if value.eq(&AvChannelLayout::_7POINT0) => ChannelLayout::Ch7_0,
    () if value.eq(&AvChannelLayout::_7POINT0_FRONT) => ChannelLayout::Ch7_0Front,
    () if value.eq(&AvChannelLayout::_7POINT1) => ChannelLayout::Ch7_1,
    () if value.eq(&AvChannelLayout::_7POINT1_WIDE) => ChannelLayout::Ch7_1Wide,
    () if value.eq(&AvChannelLayout::_7POINT1_WIDE_BACK) => ChannelLayout::Ch7_1WideBack,
    () if value.eq(&AvChannelLayout::_7POINT1POINT2) => ChannelLayout::Ch7_1_2,
    () if value.eq(&AvChannelLayout::_7POINT1POINT4_BACK) => ChannelLayout::Ch7_1_4Back,
    () if value.eq(&AvChannelLayout::_7POINT2POINT3) => ChannelLayout::Ch7_2_3,
    () if value.eq(&AvChannelLayout::_9POINT1POINT4_BACK) => ChannelLayout::Ch9_1_4Back,
    () if value.eq(&AvChannelLayout::_22POINT2) => ChannelLayout::Ch22_2,
    () => return None,
  };
  Some(named)
}

/// The describe rung: read FFmpeg's own rendering of a layout
/// (`av_channel_layout_describe`, e.g. `"binaural"`, `"5.1(side)"`)
/// through [`ChannelLayout`]'s total `FromStr` door.
///
/// A **named** variant wins. Anything the vocabulary does not name —
/// `FromStr`'s `Other` escape, which is where `"3 channels (FL+FR+TFL)"`
/// and every custom-order rendering land — collapses to
/// [`ChannelLayout::default`], the absent sentinel. That collapse is
/// deliberate: `known_kind` answers *which named layout is this*, and
/// "none of them" is `Other("")`; the rendering is already carried
/// verbatim by [`ChannelLayoutDescription::text`], so letting it ride
/// `Other` too would put a second, differently-shaped copy of the same
/// string in the same struct.
fn channel_layout_from_describe(rendered: &str) -> ChannelLayout {
  // **A rendering too long to be a slug never reaches `from_str`.**
  //
  // `ChannelLayout::from_str` is total: what it does not recognise it
  // wraps in `Other(Utf8Bytes::from(s))`, and because `s` is *borrowed*
  // that constructor **copies** — infallibly — for anything past
  // `smol_bytes::INLINE_CAP`. The line below then throws that copy
  // away. So an unnamed layout whose rendering the container controls
  // (a long `FL+FR+…` enumeration, a custom order's channel list) paid
  // for an allocation this crate had no use for and could not refuse:
  // an abort reachable from a file, on the road that exists to answer
  // "is this one of the layouts we name?".
  //
  // Every slug the vocabulary recognises is short — the longest in
  // mediaframe 0.11's roster is `7.1(wide-side)` at fourteen bytes,
  // against an inline window of sixty-two — so a rendering past that
  // window cannot be a match, and skipping the parse loses nothing.
  // `renderings_of_named_layouts_fit_the_inline_window` pins that
  // against FFmpeg's own standard-layout roster rather than against
  // this comment.
  //
  // Within the window `from_str` allocates nothing: it folds into a
  // stack buffer and compares byte slices, and the `Other` arm it can
  // still reach stores its bytes inline.
  //
  // (A `ChannelLayout::parse_known(&str) -> Option<Self>` that never
  // constructs `Other` is the real fix and is filed against mediaframe
  // for 0.11.1; this is the surgical one, on this side of the seam.)
  if rendered.len() > smol_bytes::INLINE_CAP {
    return ChannelLayout::default();
  }
  ChannelLayout::from_str(rendered)
    .ok()
    .filter(|layout| !matches!(layout, ChannelLayout::Other(_)))
    .unwrap_or_default()
}

/// Maps FFmpeg's [`AVChannelOrder`](ffi::AVChannelOrder) to the
/// [`ChannelOrder`] tag.
pub fn channel_order_from_ffmpeg(value: ffi::AVChannelOrder) -> ChannelOrder {
  // Compare via integer rather than enum-matching: the caller often
  // sources `value` from raw FFmpeg memory (`AVChannelLayout.order`),
  // and an unknown variant would already be UB before reaching this
  // function. Going through `as i32` here is sound because the caller
  // is responsible for the up-conversion path; for the raw-pointer
  // path use [`channel_order_from_raw`].
  channel_order_from_raw(value as i32)
}

/// Variant of [`channel_order_from_ffmpeg`] that takes the raw integer
/// directly. Use this when the caller has just read
/// `AVChannelLayout.order` from FFmpeg memory and doesn't want to
/// risk constructing an invalid bindgen enum value first.
pub fn channel_order_from_raw(raw: i32) -> ChannelOrder {
  match raw {
    x if x == ffi::AVChannelOrder::AV_CHANNEL_ORDER_NATIVE as i32 => ChannelOrder::Native,
    x if x == ffi::AVChannelOrder::AV_CHANNEL_ORDER_CUSTOM as i32 => ChannelOrder::Custom,
    x if x == ffi::AVChannelOrder::AV_CHANNEL_ORDER_AMBISONIC as i32 => ChannelOrder::Ambisonic,
    _ => ChannelOrder::Unspecified,
  }
}

/// Why a channel layout could not be described.
///
/// Three answers, and the first two are **memory-safety** ones rather
/// than resource ones. `av_channel_layout_describe` walks `u.map[i]`
/// for each of `nb_channels` when the order is `CUSTOM`, and FFmpeg's
/// struct puts nothing between a caller and that walk: the map is a
/// bare pointer and the count is a bare `int`. Refusing is not
/// politeness, it is the precondition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ChannelLayoutFault {
  /// A `CUSTOM` order reached a **safe** conversion, which cannot
  /// establish that the map it points at is as long as the count it
  /// declares.
  ///
  /// `ffmpeg_next::ChannelLayout` is a public newtype over a public
  /// `AVChannelLayout`, so safe Rust can write `nb_channels = 2` beside
  /// a `u.map` that points at one entry — or at a dangling address, or
  /// at something misaligned. Checking for null and for a terminator
  /// inside each name does not help: **neither provenance nor extent is
  /// observable from the pointer**, and the loop that would check the
  /// names is itself the out-of-bounds read. There is no validation a
  /// safe function can perform here, so it performs none and refuses.
  ///
  /// The extent has to come from the *caller* instead, which is what
  /// [`channel_layout_description_from_raw_ptr`]'s `unsafe` contract
  /// asks for. Inside this crate the demux and convert roads satisfy it
  /// from FFmpeg's own `AVCodecParameters` and `AVFrame`, where
  /// `av_channel_layout_copy` allocated the map and sized it — and they
  /// argue exactly that at each call.
  #[error(
    "a custom channel layout declaring {channels} channels reached a safe conversion, which \
     cannot verify that its map has that many entries"
  )]
  UnverifiableCustomMap {
    /// `nb_channels`, as the layout declared it.
    channels: i32,
  },
  /// A `CUSTOM` order whose `u.map` is null, or whose channel count is
  /// not positive, or one of whose sixteen-byte names carries no NUL.
  /// FFmpeg would dereference the map, or read past a name, while
  /// describing it.
  #[error("a custom channel layout declares {channels} channels and carries no usable map")]
  MalformedCustomMap {
    /// `nb_channels`, as the layout declared it.
    channels: i32,
  },
  /// A layout whose **declared shape** is not one FFmpeg's own helpers
  /// can be given, for an order other than `CUSTOM`.
  ///
  /// `av_channel_layout_describe` and `av_channel_layout_compare` both
  /// assume the invariants `av_channel_layout_check` states, and
  /// nothing between a caller and those helpers enforces them: a
  /// `NATIVE` layout whose `nb_channels` disagrees with its mask's
  /// population, an `AMBISONIC` layout whose channels do not form an
  /// ambisonic order, and any layout declaring a count outside the
  /// range every downstream calculation assumes. FFmpeg computes
  /// `nb_channels - popcount(mask)` and takes an integer square root of
  /// it in signed C arithmetic; a count a safe caller can simply write
  /// into the public struct is enough to take that somewhere it was
  /// never meant to go.
  #[error(
    "a channel layout of order {order} declaring {channels} channels is not a shape FFmpeg's \
     own helpers can be given"
  )]
  MalformedLayout {
    /// `order`, as the raw `c_int` it is on the wire.
    order: i32,
    /// `nb_channels`, as the layout declared it.
    channels: i32,
  },
  /// The description could not be allocated.
  #[error("out of memory describing a channel layout")]
  Alloc,
}

/// Which arm of an `AVChannelLayout`'s union an order defines, if any —
/// **the one dispatcher every union read in this crate goes through.**
///
/// It lives here rather than beside any one of its callers because the
/// rule was discovered twice: R13 fixed the resampler's equality and
/// `Debug`, and R14 found the codec-ticket mirror still reading
/// `u.mask` for an unspecified layout. A rule written in two places
/// becomes two rules, and this one governs whether a read is defined at
/// all.
///
/// The distinction FFmpeg's header makes and this crate had been
/// eliding: `NATIVE` and `AMBISONIC` define `u.mask`, `CUSTOM` defines
/// `u.map`, and for `UNSPEC` the union is **undefined and must not be
/// used**. An order this build does not name defines nothing either —
/// a future FFmpeg may give it an arm, and until this crate is taught
/// which, reading one would be a guess about storage a container
/// supplied.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub(crate) enum LayoutArm {
  /// `u.mask` — `NATIVE` and `AMBISONIC`.
  Mask,
  /// `u.map` — `CUSTOM`.
  Map,
  /// Neither: `UNSPEC`, and every order this build has never heard of.
  Undefined,
}

impl LayoutArm {
  /// Folds a raw `AVChannelOrder` — never an `AVChannelOrder` value,
  /// which a container may hold outside this build's discriminant set.
  pub(crate) const fn of(order_raw: i32) -> Self {
    if order_raw == ffi::AVChannelOrder::AV_CHANNEL_ORDER_NATIVE as i32
      || order_raw == ffi::AVChannelOrder::AV_CHANNEL_ORDER_AMBISONIC as i32
    {
      Self::Mask
    } else if order_raw == ffi::AVChannelOrder::AV_CHANNEL_ORDER_CUSTOM as i32 {
      Self::Map
    } else {
      Self::Undefined
    }
  }
}

/// The widest channel count this crate will hand to an FFmpeg layout
/// helper.
///
/// Not a capacity and not a policy: a bound that keeps FFmpeg's own
/// `int` arithmetic — `nb_channels - popcount(mask)`, and the integer
/// square root taken of it for an ambisonic order — nowhere near where
/// signed overflow lives. Nothing real comes close: a `NATIVE` layout
/// is capped at sixty-four by its own mask, and the largest ambisonic
/// order anyone records is a two-digit number of channels.
const MAX_DECLARED_CHANNELS: i32 = 65_535;

/// **The complete, allocation-free preflight for every channel-layout
/// order — and every FFmpeg layout helper in this crate is behind it.**
///
/// # Why it is one function and not a check at each call
///
/// FFmpeg's layout helpers (`av_channel_layout_describe`,
/// `av_channel_layout_compare`, `av_channel_layout_copy`, and
/// `swr_alloc_set_opts2` / `swr_build_matrix2` through the contexts they
/// configure) all assume the invariants `av_channel_layout_check`
/// states, and none of them checks. `AVChannelLayout` is a public
/// struct with public fields, and `ffmpeg_next::ChannelLayout` is a
/// public newtype over it — so every one of those invariants is
/// something *safe* Rust can break, and the helper that trips over it
/// does so inside C.
///
/// Earlier rounds of review closed the custom-map road and left the
/// others on the assumption that an order which describes its channels
/// through a `uint64_t` mask cannot be malformed. It can: the mask is
/// not the only field: `nb_channels` is an `int` a caller writes, and
/// an `AMBISONIC` layout with an extreme one reaches arithmetic that
/// was never given a bound. The lesson each round has taught is that a
/// rule written in two places becomes two rules, so this one is written
/// once and everything that calls a helper goes through it.
///
/// # What it decides, per order
///
/// - **any order**: a count below zero, or above
///   [`MAX_DECLARED_CHANNELS`] — reported as the *map* fault for a
///   `CUSTOM` layout, because that is the specific thing to say about
///   one, and as the shape fault for the rest;
/// - **any order but an unspecified one**: a count of zero, which
///   `av_channel_layout_check` refuses for every order in its opening
///   line. The exception is deliberate and narrow: the all-zero
///   layout — `UNSPEC` with no channels — is how a container says it
///   declared none, and no FFmpeg helper is ever called for it;
/// - **`NATIVE`**: `popcount(u.mask) == nb_channels`, FFmpeg's own
///   invariant — which also caps such a layout at sixty-four channels
///   for free;
/// - **`AMBISONIC`**: `av_channel_layout_check`'s own rule and only it —
///   the non-diegetic channels named by `u.mask` must leave at least
///   one channel for the ambisonic part, i.e. `popcount(mask) <
///   nb_channels`. The remainder is deliberately **not** required to be
///   a perfect square: that is
///   `av_channel_layout_ambisonic_order`'s question, which answers
///   `EINVAL` for what its own comment calls an "incomplete order", and
///   an incomplete-order layout is one FFmpeg will still describe and
///   convert;
/// - **`CUSTOM`**: [`custom_map_fault`]'s rule, unchanged — null map,
///   non-positive count, or a name with no NUL inside its sixteen
///   bytes;
/// - **`UNSPEC`**, and any order this build does not name: nothing
///   beyond the count, because no helper is called for them.
///
/// # Safety
///
/// For the duration of the call, `ptr` must point at a live, aligned
/// `AVChannelLayout` that meets [the `AVChannelLayout`
/// contract](crate::contracts#avchannellayout): its `order`,
/// `nb_channels` and `u.mask` are read as plain values, and for a
/// `CUSTOM` order with a count in `1..=65_535` and a non-null map, each
/// of the `nb_channels` map entries' `name` (sixteen bytes) and `id`.
#[doc(hidden)]
pub unsafe fn layout_preflight(ptr: *const ffi::AVChannelLayout) -> Result<(), ChannelLayoutFault> {
  use core::ptr::{addr_of, read_unaligned};

  // SAFETY: `ptr` is live per the contract; `addr_of!` reaches `order`
  // without forming a reference to it, and reading it as `i32` matches
  // the bindgen enum's `c_int` storage — the field may hold a value no
  // variant names.
  let order = unsafe { read_unaligned(addr_of!((*ptr).order).cast::<i32>()) };
  // SAFETY: a plain `int` field of a live struct.
  let channels = unsafe { (*ptr).nb_channels };
  let malformed = Err(ChannelLayoutFault::MalformedLayout { order, channels });

  // **The order's own rule first, where it has one.** A `CUSTOM` layout
  // declaring a count outside the bound is malformed as a *map* — that
  // is the specific thing to say about it, and the count rule below is
  // the general one. Reporting the general fault for a custom layout
  // would tell a caller less than this crate knows.
  if order == ffi::AVChannelOrder::AV_CHANNEL_ORDER_CUSTOM as i32 {
    if !(0..=MAX_DECLARED_CHANNELS).contains(&channels) {
      return Err(ChannelLayoutFault::MalformedCustomMap { channels });
    }
    // SAFETY: the caller's contract, forwarded.
    return match unsafe { custom_map_fault(ptr, order) } {
      Some(channels) => Err(ChannelLayoutFault::MalformedCustomMap { channels }),
      None => Ok(()),
    };
  }
  if !(0..=MAX_DECLARED_CHANNELS).contains(&channels) {
    return malformed;
  }
  // **A positive count, as `av_channel_layout_check` requires of every
  // order — with exactly one deliberate exception.**
  //
  // `check` opens with `if (nb_channels <= 0) return 0;`, so a `NATIVE`
  // layout declaring zero channels and a zero mask is invalid to
  // FFmpeg even though its own mask rule (`popcount == nb_channels`)
  // would be satisfied by it. This crate admitted exactly that.
  //
  // The exception is the all-zero `AVChannelLayout`: `UNSPEC` with no
  // channels is `ffmpeg_next::ChannelLayout::default()` and the state
  // `avcodec_parameters_alloc` leaves behind, and this crate reads it
  // as **"the container declared no layout"** rather than as a claim
  // about one. Nothing is ever handed to an FFmpeg helper for it — the
  // `Undefined` arm below returns before any call — so admitting it
  // asserts nothing that could be wrong. Declaring it invalid here
  // would refuse every stream that simply has no layout.
  if channels == 0 && !matches!(LayoutArm::of(order), LayoutArm::Undefined) {
    return malformed;
  }
  if matches!(LayoutArm::of(order), LayoutArm::Undefined) {
    // `UNSPEC`, and any order this build does not name: no helper is
    // called for them and the union is not theirs to read, so the count
    // above is the whole of the rule.
    return Ok(());
  }
  if order == ffi::AVChannelOrder::AV_CHANNEL_ORDER_NATIVE as i32 {
    // SAFETY: the order names the `mask` arm of the union.
    let mask = unsafe { (*ptr).u.mask };
    return if i64::from(mask.count_ones()) == i64::from(channels) {
      Ok(())
    } else {
      malformed
    };
  }
  if order == ffi::AVChannelOrder::AV_CHANNEL_ORDER_AMBISONIC as i32 {
    // SAFETY: the order names the `mask` arm of the union.
    let mask = unsafe { (*ptr).u.mask };
    // **FFmpeg's own validity rule, and nothing more than it.**
    // `av_channel_layout_check` reads, in full:
    //
    // ```c
    // case AV_CHANNEL_ORDER_AMBISONIC:
    //     /* If non-diegetic channels are present, ensure they are
    //        taken into account */
    //     return av_popcount64(channel_layout->u.mask) < channel_layout->nb_channels;
    // ```
    //
    // The mask names the non-diegetic channels that follow the
    // ambisonic ones, so at least one channel must be left for the
    // ambisonic part. That is the whole of it — and the comparison
    // also refuses a zero count, which is why no separate positivity
    // test is needed here.
    //
    // This arm used to demand that the remainder be a perfect square,
    // `(order + 1)²`. That is a real property and FFmpeg computes it —
    // in `av_channel_layout_ambisonic_order`, whose own comment calls
    // the failing case "incomplete order - some harmonics are missing"
    // and which answers `AVERROR(EINVAL)` for it. **It is that
    // function's question, not validity's**: `check` never consults it,
    // an incomplete-order layout is a layout FFmpeg will describe and
    // convert, and refusing one here turned a real file away. Asking a
    // stricter question than the library asks is not caution; it is a
    // different answer to a question nobody posed.
    return if i64::from(mask.count_ones()) < i64::from(channels) {
      Ok(())
    } else {
      malformed
    };
  }
  // Unreachable: `LayoutArm` folds every order into one of the three
  // arms and all three are answered above. Kept total rather than
  // asserted, because an order that grows a fourth arm should reach a
  // conservative answer rather than a panic.
  Ok(())
}

/// Builds a fully-populated [`ChannelLayoutDescription`] from an FFmpeg
/// [`AvChannelLayout`].
///
/// - Native / Ambisonic layouts populate `native_mask` from
///   [`AvChannelLayout::bits`] (clearing it to `None` if zero).
/// - Custom layouts populate `custom_channels` from FFmpeg's per-channel
///   list (`AVChannelLayout.u.map`), with each label drawn from
///   `AVChannelCustom.name`.
/// - `text` carries the result of `av_channel_layout_describe`
///   (FFmpeg's human-readable rendering — e.g. `"5.1(side)"`).
/// - `known_kind` runs [`channel_layout_from_ffmpeg`]'s two rungs
///   against that same single rendering: constant table first, then
///   the describe rung.
pub fn channel_layout_description_from_ffmpeg(
  value: &AvChannelLayout,
) -> Result<ChannelLayoutDescription, ChannelLayoutFault> {
  use core::ptr::{addr_of, read_unaligned};

  let ptr = &value.0 as *const ffi::AVChannelLayout;
  // **A `CUSTOM` order is refused here rather than read.**
  //
  // This is the boundary between what a *safe* signature can promise
  // and what it cannot. `AvChannelLayout` is `ffmpeg_next::ChannelLayout`,
  // a public newtype over a public `AVChannelLayout`: safe Rust can set
  // `nb_channels` to two and point `u.map` at one entry, at a dangling
  // address, or at something misaligned, and nothing in the type says
  // otherwise. Walking `nb_channels` entries to check them would *be*
  // the out-of-bounds read, and every FFmpeg helper this module calls —
  // `av_channel_layout_describe`, `av_channel_layout_compare` — makes
  // the same indexing assumption. Provenance and extent are not
  // observable from a pointer, so no check placed here can establish
  // them.
  //
  // The extent comes from the caller instead, through
  // [`channel_layout_description_from_raw_ptr`]'s `unsafe` contract.
  // See [`ChannelLayoutFault::UnverifiableCustomMap`].
  //
  // SAFETY: `value` is a live reference, so `ptr` is a live
  // `*const AVChannelLayout`; `addr_of!` reaches `order` without
  // forming a reference to it, and reading it as `i32` matches the
  // bindgen enum's `c_int` storage — the field may hold a value no
  // variant names.
  let order =
    channel_order_from_raw(unsafe { read_unaligned(addr_of!((*ptr).order).cast::<i32>()) });
  if matches!(order, ChannelOrder::Custom) {
    // SAFETY: as above — a plain `int` field of a live struct.
    let channels = unsafe { (*ptr).nb_channels };
    return Err(ChannelLayoutFault::UnverifiableCustomMap { channels });
  }

  // SAFETY: the order is one of the non-`CUSTOM` variants, and for
  // those the implementation below reads `u.mask` (a `uint64_t`, not a
  // pointer) and never `u.map`, so the map's extent — the one thing a
  // safe caller could lie about — is not consulted. `value` is a live
  // reference for the duration of the call, which is the rest of the
  // contract.
  unsafe { channel_layout_description_from_raw_ptr(ptr) }
}

/// Pointer variant of [`channel_layout_description_from_ffmpeg`], and
/// **the only road that reads a custom channel map**.
///
/// The safe form refuses a `CUSTOM` order outright, because nothing it
/// is handed can establish the map's extent; this one requires that
/// extent of its caller instead, and is therefore `unsafe`. The pointer
/// shape also lets the convert path pass
/// `addr_of!((*av_frame).ch_layout)` straight through without
/// materializing a typed reference.
///
/// # Safety
///
/// For the duration of the call, `ptr` must point at a live, aligned
/// `AVChannelLayout` that meets [the `AVChannelLayout`
/// contract](crate::contracts#avchannellayout). Its plain fields are read
/// first and the layout is preflighted ([`layout_preflight`]); for a
/// `CUSTOM` order with a count the preflight accepts and a non-null map,
/// each of the `nb_channels` map entries' `id` and `name` is then read
/// again — directly, and by `av_channel_layout_describe`, which renders
/// the layout's text from the same entries.
pub unsafe fn channel_layout_description_from_raw_ptr(
  ptr: *const ffi::AVChannelLayout,
) -> Result<ChannelLayoutDescription, ChannelLayoutFault> {
  use core::ptr::{addr_of, read_unaligned};
  // Read `order` as a raw integer first — never let Rust assume
  // the field is a valid `AVChannelOrder`.
  // SAFETY: `ptr` is a valid `*const AVChannelLayout`; `addr_of!`
  // computes the field address without forming a reference; reading
  // as `i32` matches the bindgen enum's `c_int` storage.
  let order_raw = unsafe { read_unaligned(addr_of!((*ptr).order) as *const i32) };
  let order = channel_order_from_raw(order_raw);
  let nb_channels = unsafe { (*ptr).nb_channels };

  // Native / Ambisonic carry the bitmask in the union. Only read
  // `u.mask` after the order is validated so we don't trip on an
  // unknown order writing into a future variant of the union.
  let native_mask = match LayoutArm::of(order_raw) {
    // SAFETY: this arm names exactly the orders whose contract defines
    // `u.mask`; see [`LayoutArm`], which is where that rule lives.
    LayoutArm::Mask => {
      let mask = unsafe { (*ptr).u.mask };
      if mask != 0 { Some(mask) } else { None }
    }
    LayoutArm::Map | LayoutArm::Undefined => None,
  };

  // Build name / rendering through ffmpeg-next helpers. They take
  // `&AvChannelLayout` (which is `repr(transparent)` over
  // `AVChannelLayout`), but at this point we've already validated
  // `order`, so forming the reference is sound: the only enum-typed
  // field in `AVChannelLayout` is `order`, and it now holds a value
  // that came back from `channel_order_from_raw` with the
  // unknown bucket folded into a known variant — but the *underlying
  // struct* still has the original raw bytes. We can't form `&AVChannelLayout`
  // over an unknown order without UB, so for those helpers we
  // explicitly only call them when order is one of the known variants.
  // **The custom map is validated before FFmpeg is allowed to look at
  // the layout at all**, and that ordering is the whole of this
  // paragraph. `av_channel_layout_describe` renders a `CUSTOM` layout
  // by walking `u.map[i]` for each of `nb_channels`; a layout that
  // declares channels and carries a null map makes it read from null.
  // The codec ticket's own null-map refusal happens *later* and
  // therefore cannot protect this call — and this function's contract
  // asks its caller for a live pointer and nothing more, so a layout
  // like that is an input, not a caller error.
  //
  // SAFETY: `order` has been folded from the raw integer, so the
  // `map` arm is read only for the order that names it; `addr_of!`
  // reaches the union field without forming a reference to the layout.
  // **Judged before FFmpeg is allowed to look at the layout at all** —
  // every order, through [`layout_preflight`], which is the one place
  // these rules are written. See its doc for what it decides and why
  // the codec ticket, the decoder and the resampler share it rather
  // than restating it.
  //
  // SAFETY: the caller's contract, forwarded: `ptr` is live, and for a
  // `CUSTOM` order its map holds `nb_channels` entries.
  unsafe { layout_preflight(ptr) }?;

  let (known_kind, text) = if matches!(order, ChannelOrder::Unspecified) {
    (ChannelLayout::default(), Utf8Bytes::default())
  } else {
    // SAFETY: `order` is one of {Native, Custom, Ambisonic} — all of
    // which are valid `AVChannelOrder` discriminants present in our
    // bindgen output, so `&*ptr` is sound to form here.
    let layout_ref = unsafe { &*(ptr as *const AvChannelLayout) };
    let text = describe_layout(layout_ref)?;
    // Constant-arm table first, exactly as in
    // `channel_layout_from_ffmpeg`; the rendering is consulted only
    // when the layout falls off it. Describing once and feeding both
    // fields from that one string keeps `known_kind` and `text`
    // answering from the same FFmpeg call.
    let known_kind =
      mapped_constant(layout_ref).unwrap_or_else(|| channel_layout_from_describe(&text));
    (known_kind, text)
  };
  let custom_channels_vec = unsafe { custom_channels_raw(ptr, order) }?;

  Ok(
    ChannelLayoutDescription::new(nb_channels.max(0) as u32)
      .with_order(order)
      .with_known_kind(known_kind)
      .with_native_mask(native_mask)
      .with_custom_channels(custom_channels_vec)
      .with_text(text),
  )
}

/// **Every structural fault a `CUSTOM` channel map can carry, decided
/// without allocating** — `Some(nb_channels)` when FFmpeg must not be
/// asked to read this layout, `None` when it may.
///
/// # The one place this rule is written
///
/// Two roads need it and they must not come to two answers. This
/// module's own describe road refuses with
/// [`ChannelLayoutFault::MalformedCustomMap`]; the codec ticket's
/// admission pass refuses with `DemuxError::ParametersChannelMap`,
/// *before the track table has allocated anything* — and a rule that
/// admission applied less strictly than materialisation would mean a
/// deterministic refusal arriving after the memory was already spent,
/// which is the defect this function exists to make impossible.
///
/// # What it decides
///
/// - a null `u.map`, or a channel count that is not positive:
///   `av_channel_layout_describe` walks `u.map[i]` for each of
///   `nb_channels` and `av_channel_layout_copy` `memcpy`s from it, both
///   with no null check of their own;
/// - **a sixteen-byte name with no NUL inside it.** FFmpeg does not
///   only walk the map: it tests `u.map[i].name[0]` and, when set,
///   hands the fixed array to a `%s` conversion, which reads until a
///   terminator. A full sixteen bytes of name is type-valid, safely
///   constructible, and makes that read run off the end of the entry.
///   `AVChannelCustom` documents the field as zeroed or
///   NUL-terminated; nothing enforces it;
/// - **an entry whose `id` is `AV_CHAN_NONE`.**
///   `av_channel_layout_check` walks the map for exactly this and
///   refuses the layout, because an entry that names no channel is a
///   hole in the map rather than a channel with an unusual id. Admitted,
///   it reached a codec ticket and was published as `u32::MAX`.
///
/// Everything but `CUSTOM` describes its channels through the union's
/// `mask` arm, a `uint64_t` that cannot be malformed, so those orders
/// are always `None`.
///
/// # Safety
///
/// `ptr` must be a live `*const AVChannelLayout`, and `order_raw` must
/// be its own `order` field read as the `c_int` it is on the wire —
/// never as an `AVChannelOrder`, which a container may hold a value
/// outside. For a `CUSTOM` order with a non-null `u.map`, that map must
/// hold `nb_channels` live `AVChannelCustom` entries — FFmpeg's own
/// contract for a layout it filled.
pub(crate) unsafe fn custom_map_fault(
  ptr: *const ffi::AVChannelLayout,
  order_raw: i32,
) -> Option<i32> {
  use core::ptr::{addr_of, read_unaligned};

  // Compared raw rather than through the folded vocabulary: an order
  // this build does not name folds to `Unspecified`, and "not custom"
  // has to mean exactly that the `map` arm is not the live one.
  if order_raw != ffi::AVChannelOrder::AV_CHANNEL_ORDER_CUSTOM as i32 {
    return None;
  }
  // SAFETY: `ptr` is live per the contract, and the order names the
  // `map` arm of the union.
  let (map_ptr, channels) = unsafe { ((*ptr).u.map, (*ptr).nb_channels) };
  if map_ptr.is_null() || channels <= 0 {
    return Some(channels);
  }
  for index in 0..channels as usize {
    // SAFETY: the map is `nb_channels` entries long per the contract
    // above and `index` is below that; `addr_of!` reaches the name
    // array without forming a reference to the entry, whose `id` is an
    // open enum.
    let name = unsafe { read_unaligned(addr_of!((*map_ptr.add(index)).name).cast::<[u8; 16]>()) };
    if !name.contains(&0) {
      return Some(channels);
    }
    // **And no entry may be `AV_CHAN_NONE`.** `av_channel_layout_check`
    // walks the map for exactly this and refuses the layout; an entry
    // that names no channel is a hole in the map, not a channel with an
    // unusual id. Read raw — `id` is an open enum and a container may
    // write a value outside this build's discriminant set, which makes
    // an `AVChannel`-typed read undefined before any comparison on it
    // could run.
    //
    // SAFETY: as above — the map holds `channels` entries and `addr_of!`
    // reaches the field without forming a reference to it.
    let id = unsafe { read_unaligned(addr_of!((*map_ptr.add(index)).id).cast::<i32>()) };
    if id == ffi::AVChannel::AV_CHAN_NONE as i32 {
      return Some(channels);
    }
  }
  None
}

/// Pointer-form of `custom_channels`. `order` must be the result of
/// reading `(*ptr).order` as `i32` and folding through
/// [`channel_order_from_raw`]; this skips re-reading it.
///
/// # Safety
/// `ptr` must be a live `*const AVChannelLayout`. Reads only fields
/// (`u.map`, `nb_channels`, and the per-channel array) — no `&AVChannelLayout`
/// reference is ever formed.
unsafe fn custom_channels_raw(
  ptr: *const ffi::AVChannelLayout,
  order: ChannelOrder,
) -> Result<Vec<ChannelSpec>, ChannelLayoutFault> {
  use core::ptr::{addr_of, read_unaligned};
  if !matches!(order, ChannelOrder::Custom) {
    return Ok(Vec::new());
  }
  let count = unsafe { (*ptr).nb_channels }.max(0) as usize;
  if count == 0 {
    return Ok(Vec::new());
  }
  // SAFETY: The `u` field is a union; reading `.map` is sound when
  // `order == CUSTOM` per FFmpeg's documented contract. Guard
  // explicitly for null.
  let map_ptr = unsafe { (*ptr).u.map };
  if map_ptr.is_null() {
    return Ok(Vec::new());
  }
  // Iterate the AVChannelCustom array via raw pointers — never form
  // `&[AVChannelCustom]` or `&AVChannelCustom`, because each entry
  // contains `id: AVChannel`, a bindgen enum. If FFmpeg writes an
  // unknown channel id (version skew / hostile decoder), the
  // reference itself would be UB before the raw `id` read could
  // sanitize it.
  // `nb_channels` is the container's number, so the table it sizes is
  // reserved fallibly: this runs after admission, and a count the
  // caller's ceilings let through must come back as an error rather
  // than an abort.
  let mut out = Vec::new();
  out
    .try_reserve_exact(count)
    .map_err(|_| ChannelLayoutFault::Alloc)?;
  for index in 0..count {
    // SAFETY: `map_ptr` points to `count == nb_channels` valid
    // `AVChannelCustom` entries per FFmpeg's contract; `index < count`,
    // so `entry_ptr` lies inside the allocation.
    let entry_ptr: *const ffi::AVChannelCustom = unsafe { map_ptr.add(index) };
    // SAFETY: `entry_ptr` is a valid pointer; `addr_of!((*p).field)`
    // computes the field address without forming a reference.
    let raw_id = unsafe { read_unaligned(addr_of!((*entry_ptr).id) as *const i32) };
    // The label is built fallibly and carried whole; an allocator
    // refusal on a file-declared channel count is reported rather than
    // absorbed. SAFETY: as above.
    let label = unsafe { custom_channel_label_raw(entry_ptr) }?;
    out.push(ChannelSpec::new(index as u32, raw_id as u32).with_label(label));
  }
  Ok(out)
}

/// Pointer-form of `custom_channel_label` — never forms
/// `&AVChannelCustom`, since the struct contains an enum-typed `id`.
///
/// # Safety
/// `entry_ptr` must be a live `*const AVChannelCustom`.
/// Decodes FFmpeg's bytes into the vocabulary's own carrier —
/// **fallibly, and whole**.
///
/// # What this replaced, and why the replacement is the point
///
/// mediaframe's text fields took `SmolStr`, whose constructor is
/// infallible and allocates past a twenty-three-byte inline window. On
/// a road that runs once per channel of a file-declared count, that is
/// an abort this crate could not report — so for several rounds a
/// label or rendering too long to store inline was reported **absent**
/// instead: a truthful refusal, but a lossy one, and a workaround
/// rather than an answer.
///
/// mediaframe 0.11 moves those fields to [`Utf8Bytes`], whose road can
/// be made fallible end to end: measure the decoding without producing
/// it, reserve exactly that much, build it, and **move** the buffer
/// into the carrier. Nothing is truncated and nothing is silently
/// dropped; a label is either carried in full or the open is refused by
/// name.
///
/// The measuring and building halves are [`crate::text::lossy_len`]
/// and [`crate::text::lossy_text`] — the same two the metadata road
/// uses, shared rather than restated, because a second copy of a
/// lossy-decoding rule is a second rule.
fn decode_text(bytes: &[u8]) -> Result<Utf8Bytes, ChannelLayoutFault> {
  let decoded = crate::text::lossy_len(bytes);
  crate::text::lossy_text(bytes, decoded).map_err(|_| ChannelLayoutFault::Alloc)
}

unsafe fn custom_channel_label_raw(
  entry_ptr: *const ffi::AVChannelCustom,
) -> Result<Utf8Bytes, ChannelLayoutFault> {
  use core::ptr::addr_of;
  // SAFETY: `name: [c_char; 16]` is an inline byte array — no
  // validity invariant beyond initialization (FFmpeg guarantees that).
  // `addr_of!` computes the address; we then re-interpret as `*const u8`
  // for UTF-8 lossy decoding.
  let name_ptr = unsafe { addr_of!((*entry_ptr).name) } as *const u8;
  // SAFETY: `name` is exactly 16 bytes wide.
  let bytes = unsafe { slice::from_raw_parts(name_ptr, 16) };
  let end = bytes
    .iter()
    .position(|byte| *byte == 0)
    .unwrap_or(bytes.len());
  if end == 0 {
    // An empty name is "there is none", which is the field's own
    // documented absent value — not a refusal and not a loss.
    return Ok(Utf8Bytes::default());
  }
  decode_text(&bytes[..end])
}

// **`custom_channels` and `custom_channel_label` were deleted here, and
// the deletion is the finding rather than tidying.** Both were dead
// (`#[allow(dead_code)]`), superseded by [`custom_channels_raw`] — and
// both formed `&[AVChannelCustom]` over FFmpeg's map, which is the very
// undefined behaviour the raw walk was written to avoid: each entry
// carries `id: AVChannel`, a bindgen enum, and a container that writes
// a value outside this build's discriminant set makes the *reference*
// UB before any `match` on it can run. Unreachable code that would be
// unsound if revived is a landmine, not documentation.

/// Renders a layout the way FFmpeg names it (`av_channel_layout_describe`).
fn describe_layout(layout: &AvChannelLayout) -> Result<Utf8Bytes, ChannelLayoutFault> {
  // `av_channel_layout_describe` returns the number of bytes needed
  // (excluding the NUL terminator). Start with a 128-byte buffer —
  // comfortably bigger than every named layout — and grow once if it
  // wasn't enough. Use `c_char` for portability (signed on
  // x86/aarch64-Apple, unsigned on aarch64-Linux).
  // Reserved fallibly, both times: the second length is FFmpeg's
  // rendering of a layout whose channel count came out of a file.
  let mut buf: std::vec::Vec<c_char> = std::vec::Vec::new();
  buf
    .try_reserve_exact(128)
    .map_err(|_| ChannelLayoutFault::Alloc)?;
  buf.resize(128, 0 as c_char);
  let mut needed =
    unsafe { ffi::av_channel_layout_describe(&layout.0 as *const _, buf.as_mut_ptr(), buf.len()) };
  if needed < 0 {
    return Ok(Utf8Bytes::default());
  }
  if needed as usize >= buf.len() {
    let want = needed as usize + 1;
    buf
      .try_reserve_exact(want - buf.len())
      .map_err(|_| ChannelLayoutFault::Alloc)?;
    buf.resize(want, 0 as c_char);
    needed = unsafe {
      ffi::av_channel_layout_describe(&layout.0 as *const _, buf.as_mut_ptr(), buf.len())
    };
    if needed < 0 {
      return Ok(Utf8Bytes::default());
    }
  }
  // SAFETY: buf is heap-allocated, NUL-terminated by FFmpeg's contract.
  let bytes = unsafe { slice::from_raw_parts(buf.as_ptr() as *const u8, buf.len()) };
  // Clamped, because `needed` is FFmpeg's answer rather than this
  // buffer's: the grow above makes `needed < buf.len()` true, and the
  // `min` is what keeps that an argument rather than an assumption.
  let end = bytes
    .iter()
    .position(|byte| *byte == 0)
    .unwrap_or(needed as usize)
    .min(bytes.len());
  if end == 0 {
    return Ok(Utf8Bytes::default());
  }
  // **Carried whole, and fallibly.** The rendering is FFmpeg's answer
  // for a layout whose channel count came out of a file, so its size is
  // the container's business; what this crate owes is an honest
  // refusal rather than a truncation or an abort. See [`decode_text`]
  // for the road, and for what the `SmolStr` field used to force here.
  decode_text(&bytes[..end])
}

#[cfg(test)]
mod tests {
  use super::*;

  /// Builds a NATIVE-order layout from a channel mask, the way a decoder
  /// hands one over. This is how a layout `ffmpeg_next` mints no constant
  /// for can be reached at all: `av_channel_layout_from_mask` fills in the
  /// order and the channel count, so nothing about the value is
  /// hand-forged.
  fn native(mask: u64) -> AvChannelLayout {
    // SAFETY: an all-zero `AVChannelLayout` is `AV_CHANNEL_ORDER_UNSPEC`
    // with no channels — a valid value, and the same starting point
    // `ffmpeg_next`'s own `ChannelLayout::default` uses. The constructor
    // then overwrites every field.
    let mut raw: ffi::AVChannelLayout = unsafe { core::mem::zeroed() };
    // SAFETY: `raw` is a live, writable `AVChannelLayout`.
    let rc = unsafe { ffi::av_channel_layout_from_mask(&mut raw, mask) };
    assert_eq!(rc, 0, "av_channel_layout_from_mask({mask:#x}) failed");
    AvChannelLayout(raw)
  }

  /// `AV_CH_LAYOUT_BINAURAL`, spelled the way the FFmpeg header spells it
  /// (`1ULL << AV_CHAN_BINAURAL_*`). The composed `AV_CH_BINAURAL_LEFT` /
  /// `_RIGHT` macros do not survive bindgen's macro evaluation, but the
  /// `AVChannel` enum they shift by does, so the mask is still derived
  /// from FFmpeg's own numbers rather than typed out.
  fn binaural_mask() -> u64 {
    (1u64 << ffi::AVChannel::AV_CHAN_BINAURAL_LEFT as u64)
      | (1u64 << ffi::AVChannel::AV_CHAN_BINAURAL_RIGHT as u64)
  }

  /// The three layouts [`ChannelLayout`] names but `ffmpeg_next` 9.0.0
  /// mints no constant for. The constant table cannot reach them by
  /// construction; the describe rung does, because FFmpeg's own layout
  /// map names all three and the vocabulary reads that word.
  #[test]
  fn orphan_layouts_are_named_through_the_describe_rung() {
    let cases = [
      (binaural_mask(), "binaural", ChannelLayout::Binaural),
      (
        ffi::AV_CH_LAYOUT_5POINT1 | ffi::AV_CH_TOP_FRONT_LEFT | ffi::AV_CH_TOP_FRONT_RIGHT,
        "5.1.2",
        ChannelLayout::Ch5_1_2,
      ),
      (
        ffi::AV_CH_LAYOUT_9POINT1POINT4_BACK | ffi::AV_CH_TOP_SIDE_LEFT | ffi::AV_CH_TOP_SIDE_RIGHT,
        "9.1.6",
        ChannelLayout::Ch9_1_6,
      ),
    ];
    for (mask, slug, expected) in cases {
      let layout = native(mask);
      assert_eq!(
        mapped_constant(&layout),
        None,
        "{slug} must fall off the constant table — that is what makes it an orphan"
      );
      assert_eq!(
        describe_layout(&layout)
          .expect("a well-formed layout describes")
          .as_str(),
        slug,
        "FFmpeg must name {slug} for the rung to have a word to read"
      );
      assert_eq!(
        channel_layout_from_ffmpeg(&layout).expect("a well-formed layout names"),
        expected,
        "{slug} must reach its named variant through the rung"
      );

      let described =
        channel_layout_description_from_ffmpeg(&layout).expect("a well-formed layout describes");
      assert_eq!(
        described.known_kind(),
        &expected,
        "{slug} must be named on the description path too"
      );
      assert_eq!(described.text(), slug, "{slug} rendering rides `text`");
    }
  }

  /// FFmpeg 9's actual `5.1.4`: the *side*-surround mask
  /// (`FL+FR+FC+LFE+SL+SR` plus the four heights), which its layout map
  /// names and no constant here reaches.
  ///
  /// `ffmpeg_sys_next` 9.0.0 bundles a `channel_layout_fixed.h` that
  /// `#undef`s FFmpeg's layout macros and re-declares them as C
  /// constants, and its `AV_CH_LAYOUT_5POINT1POINT4_BACK` still carries
  /// FFmpeg 8's *back*-surround formula. So `ffmpeg_next`'s
  /// `_5POINT1POINT4_BACK` constant — the one the table compares against
  /// — is a mask FFmpeg 9 no longer names, and the mask FFmpeg 9 *does*
  /// name has no constant at all. This is the ruling's "a layout the
  /// vocabulary already names is reachable with zero adapter edits",
  /// arriving earlier than expected.
  ///
  /// Asserted through the public entry point alone, deliberately: if the
  /// upstream shim is ever refreshed the constant table will start
  /// answering this mask itself, and `5.1.4` must come out named either
  /// way.
  #[test]
  fn ffmpeg_nines_own_5_1_4_is_named() {
    let layout = native(
      ffi::AV_CH_LAYOUT_5POINT1
        | ffi::AV_CH_TOP_FRONT_LEFT
        | ffi::AV_CH_TOP_FRONT_RIGHT
        | ffi::AV_CH_TOP_BACK_LEFT
        | ffi::AV_CH_TOP_BACK_RIGHT,
    );
    assert_eq!(
      describe_layout(&layout)
        .expect("a well-formed layout describes")
        .as_str(),
      "5.1.4"
    );
    assert_eq!(
      channel_layout_from_ffmpeg(&layout).expect("a well-formed layout names"),
      ChannelLayout::Ch5_1_4Back
    );
  }

  /// The constant table is the first rung and answers alone.
  ///
  /// `Some` here *is* the bypass proof: [`channel_layout_from_ffmpeg`] is
  /// `mapped_constant(..).unwrap_or_else(<describe rung>)`, and
  /// `unwrap_or_else` does not evaluate its closure on `Some` — so a
  /// mapped constant never renders, never parses, and cannot be
  /// re-answered by a word.
  ///
  /// The sample is the crossed-slug family (where FFmpeg qualifies the
  /// *side* layout in one place and the *back* one in another, so a
  /// name-based answer is the one that could plausibly differ), plus the
  /// `_7POINT1_TOP_BACK` alias that shares `_5POINT1POINT2_BACK`'s mask
  /// and therefore has no arm of its own.
  #[test]
  fn mapped_constants_are_answered_by_the_table_alone() {
    let table = [
      ("MONO", AvChannelLayout::MONO, ChannelLayout::Mono),
      ("STEREO", AvChannelLayout::STEREO, ChannelLayout::Stereo),
      (
        "STEREO_DOWNMIX",
        AvChannelLayout::STEREO_DOWNMIX,
        ChannelLayout::StereoDownmix,
      ),
      ("SURROUND", AvChannelLayout::SURROUND, ChannelLayout::Ch3_0),
      ("_5POINT0", AvChannelLayout::_5POINT0, ChannelLayout::Ch5_0),
      (
        "_5POINT0_BACK",
        AvChannelLayout::_5POINT0_BACK,
        ChannelLayout::Ch5_0Back,
      ),
      ("_5POINT1", AvChannelLayout::_5POINT1, ChannelLayout::Ch5_1),
      (
        "_5POINT1_BACK",
        AvChannelLayout::_5POINT1_BACK,
        ChannelLayout::Ch5_1Back,
      ),
      (
        "_5POINT1POINT2_BACK",
        AvChannelLayout::_5POINT1POINT2_BACK,
        ChannelLayout::Ch5_1_2Back,
      ),
      (
        "_7POINT1_TOP_BACK",
        AvChannelLayout::_7POINT1_TOP_BACK,
        ChannelLayout::Ch5_1_2Back,
      ),
      (
        "_7POINT1_WIDE",
        AvChannelLayout::_7POINT1_WIDE,
        ChannelLayout::Ch7_1Wide,
      ),
      (
        "_7POINT1_WIDE_BACK",
        AvChannelLayout::_7POINT1_WIDE_BACK,
        ChannelLayout::Ch7_1WideBack,
      ),
      (
        "_22POINT2",
        AvChannelLayout::_22POINT2,
        ChannelLayout::Ch22_2,
      ),
    ];
    for (name, layout, expected) in table {
      assert_eq!(
        mapped_constant(&layout),
        Some(expected.clone()),
        "{name} must be answered by the constant table, not by a rendering"
      );
      assert_eq!(
        channel_layout_from_ffmpeg(&layout).expect("a well-formed layout names"),
        expected,
        "{name}"
      );
    }
  }

  /// **Every rendering FFmpeg gives a layout it names fits the inline
  /// window** — which is what makes the parse bypass lossless.
  ///
  /// `channel_layout_from_describe` skips `ChannelLayout::from_str`
  /// above `smol_bytes::INLINE_CAP`, because past that the `Other` arm
  /// copies a borrowed string on the heap, infallibly, only for the
  /// line below to discard it. The bypass is only sound if no
  /// *recognisable* slug is that long.
  ///
  /// This pins it against FFmpeg's own roster rather than against a
  /// list written here: `av_channel_layout_standard` iterates every
  /// standard layout the linked library knows, and each one's rendering
  /// is measured. A future FFmpeg that grows a sixty-three-byte layout
  /// name fails this lane rather than silently losing that layout.
  #[test]
  fn renderings_of_named_layouts_fit_the_inline_window() {
    let mut opaque: *mut core::ffi::c_void = core::ptr::null_mut();
    let mut seen = 0usize;
    loop {
      // SAFETY: `av_channel_layout_standard` walks a static table and
      // is documented to take the address of an opaque cursor, which
      // starts null and is advanced by the call.
      let layout = unsafe { ffi::av_channel_layout_standard(&mut opaque) };
      if layout.is_null() {
        break;
      }
      // SAFETY: the pointer is into libavutil's own static roster, live
      // for the process.
      let rendered = unsafe { describe_layout(&*(layout as *const AvChannelLayout)) }
        .expect("a standard layout describes");
      assert!(
        rendered.len() <= smol_bytes::INLINE_CAP,
        "{rendered:?} is {} bytes, past the {} the parse bypass assumes",
        rendered.len(),
        smol_bytes::INLINE_CAP,
      );
      seen += 1;
    }
    assert!(seen > 10, "the standard roster should not be nearly empty");
  }

  /// A rendering past the inline window is answered *absent* without
  /// the parse — so the `Other` copy that would be discarded is never
  /// made.
  #[test]
  fn a_long_rendering_is_not_parsed_at_all() {
    let long = "FL+FR+FC+LFE+BL+BR+FLC+FRC+BC+SL+SR+TC+TFL+TFC+TFR+TBL+TBC+TBR".repeat(4);
    assert!(long.len() > smol_bytes::INLINE_CAP);
    assert_eq!(
      channel_layout_from_describe(&long),
      ChannelLayout::default(),
      "an unnameable rendering is absent, and nothing was copied to decide that",
    );
    // And a slug at the window's own width still parses, so the bound
    // is a bound rather than a shortcut.
    assert_eq!(
      channel_layout_from_describe("5.1(side)"),
      ChannelLayout::Ch5_1
    );
  }

  /// A layout nobody names stays *absent*. The rung upgrades the sentinel
  /// to a named variant or leaves it alone; it never smuggles FFmpeg's
  /// rendering into `known_kind`'s escape, because `text` already carries
  /// that rendering verbatim.
  #[test]
  fn an_unnamed_layout_stays_absent_with_its_rendering_in_text() {
    // FL+FR+TFL: a native mask FFmpeg's layout map does not carry, so
    // `av_channel_layout_describe` falls back to listing the channels.
    let layout = native(ffi::AV_CH_FRONT_LEFT | ffi::AV_CH_FRONT_RIGHT | ffi::AV_CH_TOP_FRONT_LEFT);
    assert_eq!(mapped_constant(&layout), None);

    let rendering = describe_layout(&layout).expect("a well-formed layout describes");
    assert!(
      rendering.contains("TFL"),
      "FFmpeg should list the channels it cannot name: {rendering:?}"
    );
    assert_eq!(
      channel_layout_from_ffmpeg(&layout).expect("a well-formed layout names"),
      ChannelLayout::default(),
      "an unnamed layout must land on the absent sentinel"
    );

    let described =
      channel_layout_description_from_ffmpeg(&layout).expect("a well-formed layout describes");
    assert_eq!(described.known_kind(), &ChannelLayout::default());
    assert_eq!(
      described.text(),
      rendering.as_str(),
      "the rendering is what `text` carries"
    );
  }

  /// The rung itself, on describe-shaped strings — the half of the door
  /// that needs no `AVChannelLayout` to exercise.
  #[test]
  fn the_describe_rung_reads_names_and_refuses_everything_else() {
    // The three orphans, as words.
    assert_eq!(
      channel_layout_from_describe("binaural"),
      ChannelLayout::Binaural
    );
    assert_eq!(
      channel_layout_from_describe("5.1.2"),
      ChannelLayout::Ch5_1_2
    );
    assert_eq!(
      channel_layout_from_describe("9.1.6"),
      ChannelLayout::Ch9_1_6
    );
    // The crossed slugs: unqualified `5.1` is the *back* layout and the
    // side one is qualified, so reading the word is the only way to tell
    // these two apart.
    assert_eq!(
      channel_layout_from_describe("5.1"),
      ChannelLayout::Ch5_1Back
    );
    assert_eq!(
      channel_layout_from_describe("5.1(side)"),
      ChannelLayout::Ch5_1
    );
    // Case folding is the vocabulary's, not ours.
    assert_eq!(
      channel_layout_from_describe("BINAURAL"),
      ChannelLayout::Binaural
    );

    // Everything else is absent — never `Other(<the rendering>)`.
    for unnamed in [
      "",
      "3 channels",
      "3 channels (FL+FR+TFL)",
      "FL@Left+FR@Right",
      "ambisonic 2",
      "not-a-layout",
    ] {
      assert_eq!(
        channel_layout_from_describe(unnamed),
        ChannelLayout::default(),
        "{unnamed:?} must stay absent"
      );
    }
  }
}

#[cfg(test)]
mod null_map_tests {
  use super::*;

  /// **A custom layout with no map is refused before FFmpeg sees it.**
  ///
  /// `av_channel_layout_describe` renders a `CUSTOM` layout by walking
  /// `u.map[i]` for each of `nb_channels`. This function's contract
  /// asks its caller for a live pointer and nothing more, so a layout
  /// that declares channels and carries a null map is an *input* — and
  /// it used to reach `describe_layout` and `mapped_constant` before
  /// anything looked at the map, which is a read from null inside
  /// FFmpeg rather than an error out of this crate.
  ///
  /// The layout below is exactly that shape. If the validation is ever
  /// reordered behind the description again, this lane does not fail —
  /// it crashes, which is the honest signal for the defect it pins.
  #[test]
  fn a_custom_layout_without_a_map_is_refused_before_ffmpeg_is_called() {
    // SAFETY: a zeroed `AVChannelLayout` is a valid value; `order` is
    // then set to the CUSTOM discriminant and `nb_channels` to a
    // positive count, leaving `u.map` null — the shape under test.
    let mut layout: ffi::AVChannelLayout = unsafe { std::mem::zeroed() };
    layout.order = ffi::AVChannelOrder::AV_CHANNEL_ORDER_CUSTOM;
    layout.nb_channels = 6;

    // SAFETY: `layout` is live for the call and never escapes it.
    let described =
      unsafe { channel_layout_description_from_raw_ptr(&layout as *const ffi::AVChannelLayout) };
    assert_eq!(
      described,
      Err(ChannelLayoutFault::MalformedCustomMap { channels: 6 }),
    );
  }

  /// **The safe road refuses a custom layout outright — it never reads
  /// the map, not even to check it.**
  ///
  /// The layout below is the shape no check can survive: `nb_channels`
  /// says two, the map holds one entry, and both the pointer and the
  /// count are things safe Rust set. A null check passes. A
  /// NUL-terminator walk over `nb_channels` entries passes the first
  /// one and then reads past the array — the check *is* the
  /// out-of-bounds read. FFmpeg's own helpers index the same way.
  ///
  /// So the safe conversions do not look. They fold `order`, see
  /// `CUSTOM`, and refuse; the extent has to come from an `unsafe`
  /// caller that can vouch for it. If this is ever "fixed" by
  /// validating instead of refusing, this lane does not fail — it
  /// reads a second `AVChannelCustom` that was never allocated, which
  /// under Miri is a hard error and in the wild is whatever happens to
  /// follow it in memory.
  #[test]
  fn a_safe_conversion_refuses_a_custom_layout_rather_than_trusting_its_count() {
    // One entry, correctly terminated: everything about it is valid
    // except that the layout beside it claims there are two.
    let mut name = [0 as core::ffi::c_char; 16];
    name[0] = b'F' as core::ffi::c_char;
    name[1] = b'L' as core::ffi::c_char;
    let map = [ffi::AVChannelCustom {
      id: ffi::AVChannel::AV_CHAN_FRONT_LEFT,
      name,
      opaque: core::ptr::null_mut(),
    }];
    // SAFETY: a zeroed `AVChannelLayout` is a valid value; the fields
    // below are set to the shape under test and `map` outlives the
    // call.
    let mut inner: ffi::AVChannelLayout = unsafe { std::mem::zeroed() };
    inner.order = ffi::AVChannelOrder::AV_CHANNEL_ORDER_CUSTOM;
    inner.nb_channels = 2;
    inner.u.map = map.as_ptr().cast_mut();
    // `ffmpeg_next::ChannelLayout` is a public newtype over that public
    // struct, which is the whole of why this is reachable from safe
    // code.
    let layout = AvChannelLayout(inner);

    assert_eq!(
      channel_layout_description_from_ffmpeg(&layout),
      Err(ChannelLayoutFault::UnverifiableCustomMap { channels: 2 }),
      "the safe description road must refuse a custom layout, not validate it",
    );
    assert_eq!(
      channel_layout_from_ffmpeg(&layout),
      Err(ChannelLayoutFault::UnverifiableCustomMap { channels: 2 }),
      "and so must the safe naming road, which shares the implementation",
    );
  }

  /// A layout the safe road *can* answer for: `u.map` is never read for
  /// a native order, so nothing about a custom map's extent is in
  /// question and the description is produced as before.
  #[test]
  fn a_safe_conversion_still_answers_for_a_native_layout() {
    let layout = AvChannelLayout::STEREO;
    let described =
      channel_layout_description_from_ffmpeg(&layout).expect("a native layout describes");
    assert_eq!(described.channels(), 2);
    assert_eq!(
      channel_layout_from_ffmpeg(&layout).expect("a native layout names"),
      ChannelLayout::Stereo,
    );
  }

  /// **A layout that is not `CUSTOM` can still be malformed, and the
  /// preflight is what says so before FFmpeg is handed it.**
  ///
  /// For ten rounds the non-custom orders were admitted on the argument
  /// that a `uint64_t` mask cannot be wrong. The mask is not the only
  /// field: `nb_channels` is an `int` that safe Rust writes into a
  /// public struct, and FFmpeg's own arithmetic over it —
  /// `nb_channels - popcount(mask) - 1`, and an integer square root of
  /// that — was never given a bound.
  ///
  /// The rule the refusal applies is `av_channel_layout_check`'s and
  /// **only** its. A first cut demanded a complete ambisonic order as
  /// well, which is `av_channel_layout_ambisonic_order`'s question and
  /// refused layouts FFmpeg accepts. Both halves are asserted below:
  /// what the bound still guards, and what it must no longer refuse.
  #[test]
  fn a_non_custom_layout_with_a_broken_shape_is_refused() {
    // SAFETY: a zeroed `AVChannelLayout` is a valid value; each case
    // below sets only scalar fields and the `mask` arm of the union,
    // and the layout never leaves this function.
    let build = |order: ffi::AVChannelOrder, channels: i32, mask: u64| unsafe {
      let mut layout: ffi::AVChannelLayout = std::mem::zeroed();
      layout.order = order;
      layout.nb_channels = channels;
      layout.u.mask = mask;
      layout
    };
    let refused = |layout: &ffi::AVChannelLayout| {
      // SAFETY: `layout` is live for the call and never escapes it.
      unsafe { layout_preflight(layout as *const ffi::AVChannelLayout) }
    };

    // An ambisonic layout with a count near `i32::MAX`: the arithmetic
    // FFmpeg does over it was never given a bound, so this is refused
    // on the count alone, before any helper sees it.
    let huge = build(ffi::AVChannelOrder::AV_CHANNEL_ORDER_AMBISONIC, i32::MAX, 0);
    assert_eq!(
      refused(&huge),
      Err(ChannelLayoutFault::MalformedLayout {
        order: ffi::AVChannelOrder::AV_CHANNEL_ORDER_AMBISONIC as i32,
        channels: i32::MAX,
      }),
    );

    // **An incomplete-order ambisonic layout is VALID**, and this crate
    // used to refuse it. Five channels are not `(order + 1)²` for any
    // order — `av_channel_layout_ambisonic_order` answers `EINVAL` and
    // its own comment calls the case "incomplete order - some harmonics
    // are missing" — but `av_channel_layout_check` never asks that
    // question, so FFmpeg describes and converts the layout and so must
    // this crate.
    assert_eq!(
      refused(&build(
        ffi::AVChannelOrder::AV_CHANNEL_ORDER_AMBISONIC,
        5,
        0
      )),
      Ok(()),
      "an incomplete order is not an invalid layout",
    );
    // Four is a complete first-order layout and is equally fine; the
    // validity rule does not distinguish them.
    assert_eq!(
      refused(&build(
        ffi::AVChannelOrder::AV_CHANNEL_ORDER_AMBISONIC,
        4,
        0
      )),
      Ok(()),
    );
    // What the rule *does* say, in full: the non-diegetic channels the
    // mask names must leave at least one for the ambisonic part.
    // Sixteen declared against two named is fine, square or not.
    let stereo_mask = ffi::AV_CH_FRONT_LEFT | ffi::AV_CH_FRONT_RIGHT;
    assert_eq!(
      refused(&build(
        ffi::AVChannelOrder::AV_CHANNEL_ORDER_AMBISONIC,
        16,
        stereo_mask,
      )),
      Ok(()),
    );
    // Two named and two declared leaves none, which is the one thing
    // `av_channel_layout_check` refuses on this arm — and one declared
    // against two named is the same refusal.
    for declared in [2, 1] {
      assert!(
        refused(&build(
          ffi::AVChannelOrder::AV_CHANNEL_ORDER_AMBISONIC,
          declared,
          stereo_mask,
        ))
        .is_err(),
        "{declared} channels against a two-channel mask leaves no ambisonic part",
      );
    }

    // A native layout whose count disagrees with its mask: FFmpeg's own
    // invariant, and the one that also caps such a layout at sixty-four
    // channels for free.
    let lying = build(ffi::AVChannelOrder::AV_CHANNEL_ORDER_NATIVE, 7, stereo_mask);
    assert_eq!(
      refused(&lying),
      Err(ChannelLayoutFault::MalformedLayout {
        order: ffi::AVChannelOrder::AV_CHANNEL_ORDER_NATIVE as i32,
        channels: 7,
      }),
    );
    assert_eq!(
      refused(&build(
        ffi::AVChannelOrder::AV_CHANNEL_ORDER_NATIVE,
        2,
        stereo_mask,
      )),
      Ok(()),
      "the same mask with its true count is a layout FFmpeg names",
    );

    // A negative count is refused whatever the order.
    assert!(refused(&build(ffi::AVChannelOrder::AV_CHANNEL_ORDER_UNSPEC, -1, 0)).is_err(),);

    // **A zero count is refused for every order but an unspecified
    // one**, which is `av_channel_layout_check`'s opening line. A
    // `NATIVE` layout declaring no channels with an empty mask
    // satisfies that order's *own* rule — `popcount(0) == 0` — and is
    // invalid all the same; this crate used to admit it.
    for order in [
      ffi::AVChannelOrder::AV_CHANNEL_ORDER_NATIVE,
      ffi::AVChannelOrder::AV_CHANNEL_ORDER_AMBISONIC,
      ffi::AVChannelOrder::AV_CHANNEL_ORDER_CUSTOM,
    ] {
      assert!(
        refused(&build(order, 0, 0)).is_err(),
        "{order:?} with no channels is not a layout",
      );
    }
    // The one exception, and it is a statement about *absence* rather
    // than about a layout: the all-zero `AVChannelLayout` is how a
    // container says it declared none, and no FFmpeg helper is ever
    // called for it.
    assert_eq!(
      refused(&build(ffi::AVChannelOrder::AV_CHANNEL_ORDER_UNSPEC, 0, 0)),
      Ok(()),
    );
  }

  /// A custom layout that declares **no** channels is not malformed —
  /// there is nothing for FFmpeg to walk — so it describes normally.
  #[test]
  fn a_custom_layout_with_no_channels_is_not_malformed() {
    // SAFETY: as above.
    let mut layout: ffi::AVChannelLayout = unsafe { std::mem::zeroed() };
    layout.order = ffi::AVChannelOrder::AV_CHANNEL_ORDER_CUSTOM;

    // SAFETY: `layout` is live for the call.
    let described =
      unsafe { channel_layout_description_from_raw_ptr(&layout as *const ffi::AVChannelLayout) };
    assert!(
      matches!(
        described,
        Err(ChannelLayoutFault::MalformedCustomMap { channels: 0 })
      ),
      "a zero-channel custom layout carries no map either, and is refused the same way",
    );
  }

  /// **A name with no NUL inside its sixteen bytes is refused too.**
  ///
  /// FFmpeg does not only walk the map: it tests `u.map[i].name[0]`
  /// and hands the fixed array to a `%s` conversion, which reads until
  /// a NUL. Sixteen non-zero bytes are type-valid and safely
  /// constructible, and they make that read run off the end of the
  /// entry — so the map being present and correctly sized is not
  /// enough, and this is the only place the difference can be caught.
  #[test]
  fn a_custom_name_without_a_terminator_is_refused() {
    let entries = [ffi::AVChannelCustom {
      id: ffi::AVChannel::AV_CHAN_FRONT_LEFT,
      name: [b'x' as core::ffi::c_char; 16],
      opaque: core::ptr::null_mut(),
    }];
    // SAFETY: a zeroed layout is valid; the map below is live for the
    // call, correctly sized for the one channel declared, and its one
    // entry deliberately carries no terminator.
    let mut layout: ffi::AVChannelLayout = unsafe { std::mem::zeroed() };
    layout.order = ffi::AVChannelOrder::AV_CHANNEL_ORDER_CUSTOM;
    layout.nb_channels = 1;
    layout.u.map = entries.as_ptr().cast_mut();

    // SAFETY: `layout` and `entries` are live for the call.
    let described =
      unsafe { channel_layout_description_from_raw_ptr(&layout as *const ffi::AVChannelLayout) };
    assert_eq!(
      described,
      Err(ChannelLayoutFault::MalformedCustomMap { channels: 1 }),
      "a correctly sized map is not a describable one if a name never ends",
    );
  }

  /// **A map entry that names no channel is a hole, and the layout is
  /// refused.**
  ///
  /// `av_channel_layout_check` walks a custom map for exactly this:
  /// `if (channel_layout->u.map[i].id == AV_CHAN_NONE) return 0;`. This
  /// crate checked the map's presence, its count and each name's
  /// terminator, and never looked at the id — so a layout with a hole
  /// in it passed admission, reached a codec ticket, and published
  /// `AV_CHAN_NONE` to a consumer as `u32::MAX`.
  ///
  /// The id is read raw because it is an open enum: a container may
  /// write a value outside this build's discriminant set, and an
  /// `AVChannel`-typed read would be undefined before any comparison
  /// could run.
  #[test]
  fn a_map_entry_that_names_no_channel_is_refused() {
    let mut name = [0 as core::ffi::c_char; 16];
    name[0] = b'F' as core::ffi::c_char;
    name[1] = b'L' as core::ffi::c_char;
    // Two entries, the *second* of which is the hole — a validation
    // that stops early passes anything else.
    let entries = [
      ffi::AVChannelCustom {
        id: ffi::AVChannel::AV_CHAN_FRONT_LEFT,
        name,
        opaque: core::ptr::null_mut(),
      },
      ffi::AVChannelCustom {
        id: ffi::AVChannel::AV_CHAN_NONE,
        name,
        opaque: core::ptr::null_mut(),
      },
    ];
    // SAFETY: a zeroed layout is valid; the map below is live for the
    // call and correctly sized for the two channels declared.
    let mut layout: ffi::AVChannelLayout = unsafe { std::mem::zeroed() };
    layout.order = ffi::AVChannelOrder::AV_CHANNEL_ORDER_CUSTOM;
    layout.nb_channels = 2;
    layout.u.map = entries.as_ptr().cast_mut();

    // SAFETY: `layout` and `entries` are live for the call.
    let described =
      unsafe { channel_layout_description_from_raw_ptr(&layout as *const ffi::AVChannelLayout) };
    assert_eq!(
      described,
      Err(ChannelLayoutFault::MalformedCustomMap { channels: 2 }),
      "an entry naming no channel is a hole in the map, not a channel",
    );

    // The same map with both entries naming a channel describes, so the
    // refusal is about the hole rather than about custom layouts.
    let whole = [
      entries[0],
      ffi::AVChannelCustom {
        id: ffi::AVChannel::AV_CHAN_FRONT_RIGHT,
        name,
        opaque: core::ptr::null_mut(),
      },
    ];
    // SAFETY: as above.
    let mut ok_layout: ffi::AVChannelLayout = unsafe { std::mem::zeroed() };
    ok_layout.order = ffi::AVChannelOrder::AV_CHANNEL_ORDER_CUSTOM;
    ok_layout.nb_channels = 2;
    ok_layout.u.map = whole.as_ptr().cast_mut();
    // SAFETY: as above.
    unsafe { channel_layout_description_from_raw_ptr(&ok_layout as *const ffi::AVChannelLayout) }
      .expect("a map with no holes describes");
  }

  /// The same map with a terminator describes normally, so the check
  /// above is about the terminator rather than about custom layouts.
  #[test]
  fn a_terminated_custom_name_describes() {
    let mut name = [0 as core::ffi::c_char; 16];
    name[0] = b'F' as core::ffi::c_char;
    name[1] = b'L' as core::ffi::c_char;
    let entries = [ffi::AVChannelCustom {
      id: ffi::AVChannel::AV_CHAN_FRONT_LEFT,
      name,
      opaque: core::ptr::null_mut(),
    }];
    // SAFETY: as above, with a terminated name.
    let mut layout: ffi::AVChannelLayout = unsafe { std::mem::zeroed() };
    layout.order = ffi::AVChannelOrder::AV_CHANNEL_ORDER_CUSTOM;
    layout.nb_channels = 1;
    layout.u.map = entries.as_ptr().cast_mut();

    // SAFETY: `layout` and `entries` are live for the call.
    let described =
      unsafe { channel_layout_description_from_raw_ptr(&layout as *const ffi::AVChannelLayout) }
        .expect("a terminated name is describable");
    assert_eq!(described.channels(), 1);
  }

  /// **Text is carried whole and built fallibly** — the workaround
  /// that reported it *absent* is gone with the field that forced it.
  ///
  /// Two lanes used to stand here, both asserting that a label or a
  /// rendering past `SmolStr`'s twenty-three-byte inline window came
  /// back empty. That was honest about what the crate did and dishonest
  /// about what the container said: mediaframe's fields took `SmolStr`,
  /// whose constructor is infallible, so on a road that runs once per
  /// channel of a file-declared count the only alternative to an
  /// unreportable abort was dropping the text. mediaframe 0.11 moves
  /// those fields to `Utf8Bytes`, and the road is fallible end to end.
  #[test]
  fn text_is_decoded_whole_and_fallibly() {
    for raw in [
      &b""[..],
      &b"FL"[..],
      &b"\xff"[..],
      &b"\xff\xfe\xfd"[..],
      &b"ok\xffafter"[..],
      &b"\xe2\x82"[..],
      &[0xffu8; 16][..],
      b"FrontLeftSurrnd",
      b"5.1(side)",
      // The two shapes the old rule dropped: a rendering longer than
      // the inline window, and one whose lossy expansion crossed it.
      b"FL+FR+FC+LFE+BL+BR+SL+SR",
      &b"a very long rendering\xff\xff that is not text either"[..],
    ] {
      let decoded = std::string::String::from_utf8_lossy(raw);
      assert_eq!(
        decode_text(raw)
          .expect("an allocator that is not refusing")
          .as_str(),
        decoded.as_ref(),
        "{raw:?} must be carried exactly as the lossy decoder reads it, whatever its length",
      );
    }
  }
}
