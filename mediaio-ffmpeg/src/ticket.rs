//! The owned codec ticket — one stream's `AVCodecParameters`, mirrored
//! into plain Rust and rebuilt on demand.
//!
//! # Why the mirror exists
//!
//! [`ffmpeg_next::codec::Parameters`] is a `*mut AVCodecParameters`
//! behind a `Send`-but-not-`Sync` wrapper. A track row that stores one
//! is `!Sync`, an `Arc` of that row is `!Send`, and every consumer that
//! shares a track table across tasks stops compiling — for a struct
//! FFmpeg documents as a plain *descriptor* with no thread affinity at
//! all. The auto-trait is missing, not the safety.
//!
//! The crate already answers that class one way: it mirrors what a
//! consumer needs into owned Rust —
//! [`TrackParams`](mediaspine::demuxer::TrackParams) mirrors the
//! common fields, [`SideDataEntry`] mirrors a frame's metadata,
//! [`FfmpegBytes`] mirrors its pixels. [`CodecTicket`] walks that road
//! its last mile: **every** field of an `AVCodecParameters`, held as
//! owned bytes and plain integers, with `Sync` arriving by
//! construction rather than by an `unsafe impl` over FFI.
//!
//! # The two halves
//!
//! * [`CodecTicket::mirror`] reads a live `AVCodecParameters` into the
//!   ticket. It is the only place that reads one.
//! * [`CodecTicket::rebuild`] allocates a fresh `AVCodecParameters` and
//!   writes every field back. It is the only place in this crate's
//!   track-row road that allocates one, and what it hands back is what
//!   `avcodec_parameters_to_context` is fed — unchanged from before the
//!   mirror existed.
//!
//! The pair is proved in `tests/codec_ticket_parity.rs`: for every
//! stream of every fixture the corpus can mint, the rebuilt struct is
//! compared with the original **field by field**, including
//! `extradata` bytes, the `AV_INPUT_BUFFER_PADDING_SIZE` zeroes past
//! their end, every `coded_side_data` entry's type id and payload, and
//! the channel layout down to a custom map's per-channel names. The
//! shapes no container will hand over — a custom map, an unnamed
//! side-data kind, every scalar set off its default — are built by
//! hand in the same file, and a decoder is opened through a rebuilt
//! ticket and made to produce a frame.
//!
//! # The reading discipline, inherited
//!
//! Not one bindgen enum is materialised out of FFmpeg memory. Every
//! open C enum field — the media type, the codec id, the field order,
//! the five colour fields, the alpha mode, the channel order, a custom
//! channel's id, a side-data type id — travels as **the raw 32-bit
//! pattern it is on the wire**, read and written through the same
//! `i32` cast, so a value this build's bindings cannot name is still a
//! value this ticket carries. That is the same rule
//! [`crate::extras::bounded_clone_parameters`] is written to, for the
//! same reason: forming a typed reference to a struct whose enum field
//! holds an unnamed discriminant is undefined behaviour before a
//! single field is read.
//!
//! [`SideDataEntry`]: crate::extras::SideDataEntry
//! [`FfmpegBytes`]: crate::FfmpegBytes

use core::ptr::{addr_of, addr_of_mut, copy_nonoverlapping, read_unaligned, write_unaligned};

use ffmpeg_next::{
  codec::Parameters,
  ffi::{
    AV_INPUT_BUFFER_PADDING_SIZE, AVChannelCustom, AVChannelOrder, AVCodecParameters,
    AVPacketSideData, AVPacketSideDataType, av_mallocz,
  },
};

use crate::{
  FfmpegBytes,
  error::{
    ParametersAlloc, ParametersChannelMap, ParametersCopy, ParametersError, ParametersMissing,
    ParametersOpaque, ParametersTooLarge,
  },
  extras::{ExtradataPolicy, SideDataEntry, measure_parameters},
};

/// A verbatim `AVRational` field.
///
/// Its own type rather than a [`mediatime::Timebase`] because the two
/// fields it carries — `sample_aspect_ratio` and `framerate` — are not
/// timebases and are not always valid ratios: FFmpeg spells "unknown"
/// as a zero numerator (`sample_aspect_ratio`) or as `0/1`
/// (`framerate`), and a mirror that normalised either would fail its
/// own parity test. Nothing here reduces, validates or interprets;
/// the numbers cross unchanged.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Ratio {
  num: i32,
  den: i32,
}

impl Ratio {
  /// Constructs a `Ratio` from a numerator and denominator, verbatim.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(num: i32, den: i32) -> Self {
    Self { num, den }
  }
  /// The numerator.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn num(&self) -> i32 {
    self.num
  }
  /// The denominator.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn den(&self) -> i32 {
    self.den
  }
}

/// The Dolby Vision decoder configuration record's two routing fields —
/// the profile number and the base-layer signal-compatibility id —
/// read from the container's `dvcC`/`dvvC`/`dwvC` box when present.
///
/// **Numbers, not interpretation.** This crate does not map `profile`
/// onto a named Dolby Vision profile (5, 7, 8.1, …) or `compatibility_id`
/// onto "HDR10-compatible" / "SDR-compatible" / etc. — those tables are
/// Dolby's own and change independently of this crate's release cycle;
/// the consumer that already routes base-layer-vs-refuse on this value
/// (per the sealed ground this type answers to) owns that table. What
/// crosses here is exactly what the box declared, unchanged.
///
/// `Copy`: two bytes, nothing owned.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct DolbyVisionConfig {
  profile: u8,
  compatibility_id: u8,
}

impl DolbyVisionConfig {
  /// Constructs a `DolbyVisionConfig` from its two routing numbers.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(profile: u8, compatibility_id: u8) -> Self {
    Self {
      profile,
      compatibility_id,
    }
  }
  /// The Dolby Vision profile number (`dv_profile`), verbatim.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn profile(&self) -> u8 {
    self.profile
  }
  /// The base-layer signal-compatibility id (`dv_bl_signal_
  /// compatibility_id`), verbatim — what the consumer this record was
  /// sealed for routes base-layer-vs-refuse on.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn compatibility_id(&self) -> u8 {
    self.compatibility_id
  }
}

/// Byte offset of `dv_profile` in FFmpeg's in-process
/// `AVDOVIDecoderConfigurationRecord` (`libavutil/dovi_meta.h`): two
/// leading version bytes, then the profile.
const DOVI_CONFIG_PROFILE_OFFSET: usize = 2;
/// Byte offset of `dv_bl_signal_compatibility_id`: version (2) +
/// profile (1) + level (1) + three one-byte presence flags (3).
const DOVI_CONFIG_COMPATIBILITY_ID_OFFSET: usize = 7;
/// Minimum payload length [`parse_dolby_vision_config`] needs — enough
/// to read the compatibility id, the later of the two fields. The full
/// struct FFmpeg n9.0 allocates is nine bytes (a ninth,
/// `dv_md_compression`, follows); this function reads neither that
/// byte nor relies on the struct's total size, which its own header
/// documents as **not** part of the public ABI.
const DOVI_CONFIG_MIN_BYTES: usize = DOVI_CONFIG_COMPATIBILITY_ID_OFFSET + 1;

/// Parses an `AV_PKT_DATA_DOVI_CONF` payload — a byte-for-byte copy of
/// FFmpeg's `AVDOVIDecoderConfigurationRecord` (`dv_version_major,
/// dv_version_minor, dv_profile, dv_level, rpu_present_flag,
/// el_present_flag, bl_present_flag, dv_bl_signal_compatibility_id,
/// [dv_md_compression]`, every field one byte) — into the two routing
/// numbers. `None` when the payload is shorter than
/// [`DOVI_CONFIG_MIN_BYTES`] — a version-skew or corrupt entry.
fn parse_dolby_vision_config(bytes: &[u8]) -> Option<DolbyVisionConfig> {
  if bytes.len() < DOVI_CONFIG_MIN_BYTES {
    return None;
  }
  Some(DolbyVisionConfig::new(
    bytes[DOVI_CONFIG_PROFILE_OFFSET],
    bytes[DOVI_CONFIG_COMPATIBILITY_ID_OFFSET],
  ))
}

/// One entry of a custom channel map — the `AV_CHANNEL_ORDER_CUSTOM`
/// arm of [`ChannelLayoutTicket`].
///
/// `name` is FFmpeg's inline `char[16]`, carried as the sixteen bytes
/// it is. It is a NUL-padded label, not a Rust string: FFmpeg's own
/// contract is "may be filled with a 0-terminated string … otherwise
/// it must be zeroed", so the bytes cross verbatim and any decoding
/// into text is the consumer's choice, not the mirror's.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct CustomChannel {
  id: i32,
  name: [u8; 16],
}

impl CustomChannel {
  /// Constructs a `CustomChannel` from a raw `AVChannel` id and the
  /// sixteen name bytes.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(id: i32, name: [u8; 16]) -> Self {
    Self { id, name }
  }
  /// The raw `AVChannel` id. Negative values are real: `AV_CHAN_NONE`
  /// is `-1`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn id(&self) -> i32 {
    self.id
  }
  /// The sixteen name bytes, NUL-padded, exactly as FFmpeg holds them.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn name_bytes(&self) -> &[u8; 16] {
    &self.name
  }
}

/// The owned mirror of an `AVChannelLayout`.
///
/// The union is discriminated by `order`, and this type keeps that
/// discrimination honest: `mask` is read and written **only** for the
/// orders whose union arm is the bitmask, and `map` **only** for
/// `AV_CHANNEL_ORDER_CUSTOM`, whose arm is a pointer. Reading the
/// pointer arm as a mask would put a raw address in an owned mirror,
/// which is the whole thing this type exists to stop.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct ChannelLayoutTicket {
  order: i32,
  channels: i32,
  mask: u64,
  map: Vec<CustomChannel>,
}

impl ChannelLayoutTicket {
  /// The raw `AVChannelOrder`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn order(&self) -> i32 {
    self.order
  }
  /// `nb_channels`, verbatim.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn channels(&self) -> i32 {
    self.channels
  }
  /// The channel bitmask, meaningful for every order but
  /// `AV_CHANNEL_ORDER_CUSTOM`, where it reads zero and [`Self::map`]
  /// carries the layout instead.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn mask(&self) -> u64 {
    self.mask
  }
  /// The custom channel map — empty for every order but
  /// `AV_CHANNEL_ORDER_CUSTOM`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn map(&self) -> &[CustomChannel] {
    self.map.as_slice()
  }

  // `is_custom` used to live here, and its removal is the point rather
  // than tidying. Every caller asked it "is this custom?" and treated
  // `false` as "mask-backed" — which is how an unspecified layout's
  // undefined union came to be read into `mask`. The question has three
  // answers, not two, and
  // [`LayoutArm`](crate::channel_layout::LayoutArm) is the only thing
  // that answers it now.
}

/// Every field of one stream's `AVCodecParameters`, owned.
///
/// # The roster
///
/// All thirty-two fields FFmpeg n9.0 declares, in that struct's own
/// order. Nothing is elided as "video only" or "audio only":
/// `avcodec_parameters_to_context` reads a different subset per medium
/// but the *file* carries whatever it carries, and a mirror that kept
/// only one medium's subset would lose a field the moment a container
/// declared something unusual.
///
/// They land in three kinds. Three fields own heap and become owned
/// Rust: `extradata`, `coded_side_data`, and `ch_layout`'s custom map.
/// Two are lengths of those — `extradata_size` and
/// `nb_coded_side_data` — and are **not stored**: a carrier already
/// knows its own length, and a second copy of it is a second thing to
/// keep in agreement. The remaining twenty-seven are scalars, held as
/// the integers (or, for the two `AVRational` fields, the [`Ratio`])
/// they are.
///
/// # `Send + Sync`, by construction
///
/// Every field is an integer, an [`FfmpegBytes`](crate::FfmpegBytes)
/// (an `Arc<[u8]>`), or a `Vec` of those. There is no raw pointer, so
/// there is no `unsafe impl` and no safety argument to get wrong —
/// which is exactly the point of the road this type is on. See
/// `tests::the_ticket_is_send_and_sync`.
///
/// # What it does not carry, and why that is a refusal rather than a
/// loss
///
/// `AVChannelLayout::opaque` and `AVChannelCustom::opaque` are
/// documented as "private data of the user": raw pointers, set by
/// nobody but the caller who owns them, and unreadable to a mirror
/// that must outlive the pointer's owner. libavformat never sets
/// either, so no demuxed stream reaches this type carrying one. If one
/// ever did, [`CodecTicket::mirror`] refuses with
/// [`ParametersError::Opaque`] rather than dropping it in
/// silence — the same fail-closed answer
/// [`measure_parameters`](crate::extras) gives a channel order it has
/// never heard of.
#[derive(Clone)]
pub struct CodecTicket {
  /// The `AVStream.index` this mirror was taken at.
  ///
  /// Carried for one reason: so [`Self::rebuild`]'s errors can name
  /// the stream they are about. A rebuild that reports `ENOMEM`
  /// without saying which track it was opening is a log line nobody
  /// can act on, and the mirror is the last place that knows.
  stream_index: usize,
  codec_type: i32,
  codec_id: i32,
  codec_tag: u32,
  extradata: FfmpegBytes,
  coded_side_data: Vec<SideDataEntry>,
  format: i32,
  bit_rate: i64,
  bits_per_coded_sample: i32,
  bits_per_raw_sample: i32,
  profile: i32,
  level: i32,
  width: i32,
  height: i32,
  sample_aspect_ratio: Ratio,
  framerate: Ratio,
  field_order: i32,
  color_range: i32,
  color_primaries: i32,
  color_trc: i32,
  color_space: i32,
  chroma_location: i32,
  video_delay: i32,
  ch_layout: ChannelLayoutTicket,
  sample_rate: i32,
  block_align: i32,
  frame_size: i32,
  initial_padding: i32,
  trailing_padding: i32,
  seek_preroll: i32,
  alpha_mode: i32,
  /// What [`Self::rebuild`] will ask FFmpeg's allocator for — see
  /// [`Self::footprint_bytes`].
  footprint_bytes: usize,
}

impl CodecTicket {
  /// Mirrors a live set of codec parameters into an owned ticket.
  ///
  /// `budget` is the ceiling the mirror's heap fields must fit under,
  /// measured before a byte is copied — the same admission
  /// [`crate::extras::bounded_clone_parameters`] performs and the same
  /// number `admit_streams` charges against the session's total. A set
  /// of parameters over the ceiling is refused with
  /// [`ParametersError::TooLarge`], never truncated.
  ///
  /// Fails with [`ParametersError::Missing`] when `source` is
  /// null-backed — `Parameters::new()` and `Parameters::default()` are
  /// safe constructors over an unchecked `avcodec_parameters_alloc`,
  /// so a caller can hold one without ever having been told.
  pub fn mirror(
    source: &Parameters,
    stream_index: usize,
    budget: usize,
  ) -> Result<Self, ParametersError> {
    Self::mirror_with(source, stream_index, budget, ExtradataPolicy::Copy)
  }

  /// [`Self::mirror`], with the `extradata` policy named.
  #[doc(hidden)]
  pub fn mirror_with(
    source: &Parameters,
    stream_index: usize,
    budget: usize,
    extradata_policy: ExtradataPolicy,
  ) -> Result<Self, ParametersError> {
    // SAFETY: reading the pointer without dereferencing it — which is
    // what the null check exists for.
    let par = unsafe { source.as_ptr() };
    if par.is_null() {
      return Err(ParametersError::Missing(ParametersMissing::new(
        stream_index,
      )));
    }
    // SAFETY: `par` is a live `AVCodecParameters` owned by `source`
    // for the duration of this call.
    unsafe { Self::from_raw(par, stream_index, budget, extradata_policy) }
  }

  /// [`Self::mirror`] over a raw pointer.
  ///
  /// # Safety
  ///
  /// `par` must be a non-null, live `*const AVCodecParameters` for the
  /// duration of this call.
  pub(crate) unsafe fn from_raw(
    par: *const AVCodecParameters,
    stream_index: usize,
    budget: usize,
    extradata_policy: ExtradataPolicy,
  ) -> Result<Self, ParametersError> {
    let too_large = |bytes: usize| {
      ParametersError::TooLarge(ParametersTooLarge::new(stream_index, bytes, budget))
    };

    // **Structure first, before the measurement and long before the
    // copies.** The demux road already ran this during admission, but
    // this constructor is reachable on its own, and it used to copy
    // `extradata` and every coded-side-data payload — the two
    // attacker-sized fields — and only then reach
    // [`channel_layout_of`], which is where a malformed layout was
    // discovered. A deterministic refusal derivable from the struct's
    // own declared fields has no business arriving after two payload
    // copies, whichever caller asked for the mirror.
    //
    // It costs a pointer walk and allocates nothing. See
    // [`validate_channel_layout`], which is the whole of what can be
    // refused here from structure alone.
    //
    // SAFETY: this function's own contract, forwarded — `par` is a
    // non-null, live `*const AVCodecParameters`.
    unsafe { validate_channel_layout(par, stream_index) }?;

    // Measured before a byte is copied, exactly as the bounded clone
    // does it: the footprint enumerates the same three heap fields this
    // mirror is about to read, and refusing here is what keeps an
    // attacker-sized `extradata` or ICC profile from being copied into
    // Rust memory just to be refused afterwards.
    //
    // SAFETY: `par` is live per this function's contract; the
    // measurement allocates nothing and dereferences only what it
    // counts.
    let footprint = unsafe { measure_parameters(par) }.ok_or_else(|| too_large(usize::MAX))?;
    let footprint_bytes = match extradata_policy {
      ExtradataPolicy::Copy => footprint.total(),
      ExtradataPolicy::Omit => footprint.total_without_extradata(),
    }
    .ok_or_else(|| too_large(usize::MAX))?;
    if footprint_bytes > budget {
      return Err(too_large(footprint_bytes));
    }

    // SAFETY: `par` is live; every read below either takes a scalar
    // field by value or reaches one through `addr_of!`, and no enum
    // field is read as anything but the `i32` pattern it is on the
    // wire. See the module docs for why that distinction is
    // load-bearing rather than stylistic.
    let ticket = unsafe {
      Self {
        stream_index,
        codec_type: read_unaligned(addr_of!((*par).codec_type).cast::<i32>()),
        codec_id: read_unaligned(addr_of!((*par).codec_id).cast::<i32>()),
        codec_tag: (*par).codec_tag,
        extradata: extradata_of(par, extradata_policy, stream_index)?,
        coded_side_data: side_data_of(par, stream_index)?,
        format: (*par).format,
        bit_rate: (*par).bit_rate,
        bits_per_coded_sample: (*par).bits_per_coded_sample,
        bits_per_raw_sample: (*par).bits_per_raw_sample,
        profile: (*par).profile,
        level: (*par).level,
        width: (*par).width,
        height: (*par).height,
        sample_aspect_ratio: Ratio::new(
          (*par).sample_aspect_ratio.num,
          (*par).sample_aspect_ratio.den,
        ),
        framerate: Ratio::new((*par).framerate.num, (*par).framerate.den),
        field_order: read_unaligned(addr_of!((*par).field_order).cast::<i32>()),
        color_range: read_unaligned(addr_of!((*par).color_range).cast::<i32>()),
        color_primaries: read_unaligned(addr_of!((*par).color_primaries).cast::<i32>()),
        color_trc: read_unaligned(addr_of!((*par).color_trc).cast::<i32>()),
        color_space: read_unaligned(addr_of!((*par).color_space).cast::<i32>()),
        chroma_location: read_unaligned(addr_of!((*par).chroma_location).cast::<i32>()),
        video_delay: (*par).video_delay,
        ch_layout: channel_layout_of(par, stream_index)?,
        sample_rate: (*par).sample_rate,
        block_align: (*par).block_align,
        frame_size: (*par).frame_size,
        initial_padding: (*par).initial_padding,
        trailing_padding: (*par).trailing_padding,
        seek_preroll: (*par).seek_preroll,
        alpha_mode: read_unaligned(addr_of!((*par).alpha_mode).cast::<i32>()),
        footprint_bytes,
      }
    };
    Ok(ticket)
  }

  /// Rebuilds a live `AVCodecParameters` from the ticket.
  ///
  /// **The one ffmpeg-native allocation on the track row's road**, and
  /// the handoff a decoder is opened from:
  ///
  /// ```ignore
  /// FfmpegAudioStreamDecoder::open(
  ///   track.extra().clone_parameters()?,
  ///   track.timebase(),
  ///   limits,
  /// )
  /// ```
  ///
  /// Every field that can hold a non-default value is written, so the
  /// result depends on the ticket rather than on what
  /// `avcodec_parameters_alloc` happened to leave behind. Stated
  /// exactly, because the difference is load-bearing:
  ///
  /// * The **twenty-seven scalars** are written unconditionally. Those
  ///   are the fields `avcodec_parameters_alloc` gives non-zero defaults
  ///   to — `format` is `-1`, `profile` and `level` are
  ///   `AV_PROFILE_UNKNOWN` / `AV_LEVEL_UNKNOWN`, both rationals are
  ///   `0/1`, and so on — so leaving any of them would let a default
  ///   masquerade as the file's own value.
  /// * The **four descriptor fields** — `extradata` and
  ///   `extradata_size`, `coded_side_data` and `nb_coded_side_data` —
  ///   are written only when the ticket has something to put there. On
  ///   the empty path they keep the allocator's zero, and that is
  ///   correct rather than an omission: `codec_parameters_reset`
  ///   `memset`s the whole struct to zero and then assigns non-zero
  ///   defaults to a named list that contains none of these four. A
  ///   null pointer with a zero length is exactly what "no extradata"
  ///   and "no side data" mean, and it is what the source had.
  /// * `ch_layout` is always written — order, channel count, and either
  ///   the mask or the map.
  ///
  /// That is what makes field-by-field parity with the original
  /// provable rather than hopeful, and
  /// `tests/codec_ticket_parity.rs::every_scalar_field_is_written_back`
  /// is the assertion that a field quietly relying on a default cannot
  /// pass.
  ///
  /// Fallible because allocation is: `ParametersAlloc` when the struct
  /// itself cannot be allocated, `ParametersCopy` carrying `ENOMEM`
  /// when one of the heap fields cannot. Nothing here consults a
  /// budget — the bytes are already resident and were admitted at
  /// [`Self::mirror`]; what this allocates is exactly
  /// [`Self::footprint_bytes`].
  pub fn rebuild(&self) -> Result<Parameters, ParametersError> {
    let stream_index = self.stream_index;
    let mut out = Parameters::new();
    // SAFETY: reading the pointer the constructor stored without
    // dereferencing it — `Parameters::new` does not check
    // `avcodec_parameters_alloc` and hands back a null on failure.
    let dst = unsafe { out.as_mut_ptr() };
    if dst.is_null() {
      return Err(ParametersError::Alloc(ParametersAlloc::new(stream_index)));
    }

    // SAFETY: `dst` is a live, freshly allocated `AVCodecParameters`
    // whose heap fields are still null. Every write below is a scalar
    // store, or an `addr_of_mut!` store of the same 32-bit pattern the
    // mirror read, into a struct nothing else holds a reference to.
    unsafe {
      write_unaligned(
        addr_of_mut!((*dst).codec_type).cast::<i32>(),
        self.codec_type,
      );
      write_unaligned(addr_of_mut!((*dst).codec_id).cast::<i32>(), self.codec_id);
      (*dst).codec_tag = self.codec_tag;
      (*dst).format = self.format;
      (*dst).bit_rate = self.bit_rate;
      (*dst).bits_per_coded_sample = self.bits_per_coded_sample;
      (*dst).bits_per_raw_sample = self.bits_per_raw_sample;
      (*dst).profile = self.profile;
      (*dst).level = self.level;
      (*dst).width = self.width;
      (*dst).height = self.height;
      (*dst).sample_aspect_ratio.num = self.sample_aspect_ratio.num();
      (*dst).sample_aspect_ratio.den = self.sample_aspect_ratio.den();
      (*dst).framerate.num = self.framerate.num();
      (*dst).framerate.den = self.framerate.den();
      write_unaligned(
        addr_of_mut!((*dst).field_order).cast::<i32>(),
        self.field_order,
      );
      write_unaligned(
        addr_of_mut!((*dst).color_range).cast::<i32>(),
        self.color_range,
      );
      write_unaligned(
        addr_of_mut!((*dst).color_primaries).cast::<i32>(),
        self.color_primaries,
      );
      write_unaligned(addr_of_mut!((*dst).color_trc).cast::<i32>(), self.color_trc);
      write_unaligned(
        addr_of_mut!((*dst).color_space).cast::<i32>(),
        self.color_space,
      );
      write_unaligned(
        addr_of_mut!((*dst).chroma_location).cast::<i32>(),
        self.chroma_location,
      );
      (*dst).video_delay = self.video_delay;
      (*dst).sample_rate = self.sample_rate;
      (*dst).block_align = self.block_align;
      (*dst).frame_size = self.frame_size;
      (*dst).initial_padding = self.initial_padding;
      (*dst).trailing_padding = self.trailing_padding;
      (*dst).seek_preroll = self.seek_preroll;
      write_unaligned(
        addr_of_mut!((*dst).alpha_mode).cast::<i32>(),
        self.alpha_mode,
      );
    }

    // The three heap fields, each allocated from FFmpeg's allocator so
    // `avcodec_parameters_free` releases them with the struct — the
    // same allocator discipline `bounded_clone_parameters` uses, and
    // the same one `avcodec_parameters_copy` would have used.
    //
    // SAFETY: `dst` is live and its heap fields are null; each
    // allocation below is checked, and each is attached to `dst`
    // before the next one is attempted, so a failure part way leaves
    // `out`'s own destructor a well-formed struct to free.
    unsafe {
      write_extradata(dst, self.extradata.as_slice(), stream_index)?;
      write_side_data(dst, &self.coded_side_data, stream_index)?;
      write_channel_layout(dst, &self.ch_layout, stream_index)?;
    }

    Ok(out)
  }

  /// What [`Self::rebuild`] asks FFmpeg's allocator for: `extradata`
  /// with the `AV_INPUT_BUFFER_PADDING_SIZE` decoders read past the
  /// end into, the `coded_side_data` descriptor array and every
  /// entry's payload, and a custom channel map.
  ///
  /// The number the session admitted this stream at, and the number
  /// `DemuxLimits::max_codec_parameter_bytes` was judged against — so
  /// a row that opened is a row whose every rebuild fits the ceiling
  /// it opened under.
  ///
  /// Not the ticket's own residency: the owned mirror holds the
  /// payload without FFmpeg's trailing padding, and shares its buffers
  /// by refcount.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn footprint_bytes(&self) -> usize {
    self.footprint_bytes
  }

  /// The `AVStream.index` this mirror was taken at — what
  /// [`Self::rebuild`]'s errors name.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn stream_index(&self) -> usize {
    self.stream_index
  }
  /// The raw `AVMediaType`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn codec_type(&self) -> i32 {
    self.codec_type
  }
  /// The raw `AVCodecID`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn codec_id(&self) -> i32 {
    self.codec_id
  }
  /// The codec tag — the AVI FOURCC, when the container carries one.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn codec_tag(&self) -> u32 {
    self.codec_tag
  }
  /// The decoder-initialisation bytes — SPS/PPS for H.264, the
  /// `AudioSpecificConfig` for AAC, a font's payload for an
  /// attachment. Empty when the stream carries none, or when the row
  /// was built on the attachment road that leaves them to the carrier.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn extradata(&self) -> &[u8] {
    self.extradata.as_slice()
  }
  /// The extradata's carrier, for a consumer that wants the bytes
  /// without copying them again.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn extradata_ref(&self) -> &FfmpegBytes {
    &self.extradata
  }
  /// Stream-level side data — where a MOV `prof` atom's ICC profile
  /// arrives, among others.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn coded_side_data(&self) -> &[SideDataEntry] {
    self.coded_side_data.as_slice()
  }
  /// The Dolby Vision configuration record — profile number and base-
  /// layer compatibility id — from the container's `dvcC` / `dvvC` /
  /// `dwvC` box, when the stream carries one.
  ///
  /// `None` when [`Self::coded_side_data`] holds no
  /// `AV_PKT_DATA_DOVI_CONF` entry (an ordinary, non-Dolby-Vision
  /// stream — the overwhelming majority) or the entry's payload is too
  /// short to hold both fields. Absent configuration answers absent,
  /// same as every other field this crate exposes as an `Option`.
  ///
  /// This is the **configuration-record** half of Dolby Vision — the
  /// two numbers a consumer routes base-layer-vs-refuse on before a
  /// single frame decodes. The **per-frame** half — the RPU buffer
  /// (`AV_FRAME_DATA_DOVI_RPU_BUFFER`) and parsed dynamic metadata
  /// (`AV_FRAME_DATA_DOVI_METADATA`), plus HDR10+ dynamic metadata
  /// (`AV_FRAME_DATA_DYNAMIC_HDR_PLUS`) — is not exposed by this crate
  /// yet: [mediadecode#54](https://github.com/findit-studio/mediadecode/issues/54).
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn dolby_vision_config(&self) -> Option<DolbyVisionConfig> {
    let kind = AVPacketSideDataType::AV_PKT_DATA_DOVI_CONF as i32;
    self
      .coded_side_data
      .iter()
      .find(|entry| entry.kind() == kind)
      .and_then(|entry| parse_dolby_vision_config(entry.data()))
  }
  /// The pixel format (video) or sample format (audio), as the raw
  /// integer both enums share this field as.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn format(&self) -> i32 {
    self.format
  }
  /// Average bitrate in bits per second.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn bit_rate(&self) -> i64 {
    self.bit_rate
  }
  /// Bits per sample in the coded bitstream.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn bits_per_coded_sample(&self) -> i32 {
    self.bits_per_coded_sample
  }
  /// Valid bits in each output sample.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn bits_per_raw_sample(&self) -> i32 {
    self.bits_per_raw_sample
  }
  /// The codec profile.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn profile(&self) -> i32 {
    self.profile
  }
  /// The codec level.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn level(&self) -> i32 {
    self.level
  }
  /// Frame width in pixels — video, and the subtitle canvas.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn width(&self) -> i32 {
    self.width
  }
  /// Frame height in pixels — video, and the subtitle canvas.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn height(&self) -> i32 {
    self.height
  }
  /// The sample aspect ratio. A zero numerator means unknown.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn sample_aspect_ratio(&self) -> Ratio {
    self.sample_aspect_ratio
  }
  /// The ticket, its sample aspect ratio `stream`'s where its parameters
  /// state none — a zero numerator — and `stream` is a ratio, both its
  /// terms positive: the ratio a demuxer keeps on the stream rather than
  /// in the parameters, as MOV's does of `pasp` and Matroska's of the
  /// display size where the codec's decoder states none — ProRes's,
  /// DNxHD's — which libavformat's own guess takes first
  /// (`av_guess_sample_aspect_ratio`, `libavformat/avformat.c` 784–805 in
  /// FFmpeg 9.0.1), and FFmpeg's tool for a stream copy. A ratio the
  /// parameters state is kept. Every other field stays the parameters'.
  #[doc(hidden)]
  #[must_use]
  pub const fn with_stream_aspect_ratio(mut self, stream: Ratio) -> Self {
    if self.sample_aspect_ratio.num == 0 && stream.num > 0 && stream.den > 0 {
      self.sample_aspect_ratio = stream;
    }
    self
  }
  /// The codec-level frame rate. `0/1` when frames differ in duration
  /// or the value is not known.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn framerate(&self) -> Ratio {
    self.framerate
  }
  /// The raw `AVFieldOrder`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn field_order(&self) -> i32 {
    self.field_order
  }
  /// The raw `AVColorRange`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn color_range(&self) -> i32 {
    self.color_range
  }
  /// The raw `AVColorPrimaries`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn color_primaries(&self) -> i32 {
    self.color_primaries
  }
  /// The raw `AVColorTransferCharacteristic`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn color_trc(&self) -> i32 {
    self.color_trc
  }
  /// The raw `AVColorSpace`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn color_space(&self) -> i32 {
    self.color_space
  }
  /// The raw `AVChromaLocation`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn chroma_location(&self) -> i32 {
    self.chroma_location
  }
  /// Number of delayed frames — the decoder's `has_b_frames`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn video_delay(&self) -> i32 {
    self.video_delay
  }
  /// The channel layout.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn ch_layout(&self) -> &ChannelLayoutTicket {
    &self.ch_layout
  }
  /// Audio samples per second.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn sample_rate(&self) -> i32 {
    self.sample_rate
  }
  /// Bytes per coded audio frame — `nBlockAlign` in `WAVEFORMATEX`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn block_align(&self) -> i32 {
    self.block_align
  }
  /// Audio frame size, when the format fixes one.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn frame_size(&self) -> i32 {
    self.frame_size
  }
  /// Leading padding samples the encoder inserted.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn initial_padding(&self) -> i32 {
    self.initial_padding
  }
  /// Trailing padding samples the encoder appended.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn trailing_padding(&self) -> i32 {
    self.trailing_padding
  }
  /// Samples to skip after a discontinuity.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn seek_preroll(&self) -> i32 {
    self.seek_preroll
  }
  /// The raw `AVAlphaMode` — how an alpha channel relates to the
  /// colour values, and the last field `AVCodecParameters` declares.
  ///
  /// New in FFmpeg n9.0, and the field this mirror's first draft
  /// dropped: video-only, left at its zero by every fixture the corpus
  /// can mint, and therefore reading back identically whether it is
  /// mirrored or forgotten. The parity comparator names every field
  /// for exactly that reason.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn alpha_mode(&self) -> i32 {
    self.alpha_mode
  }
}

impl core::fmt::Debug for CodecTicket {
  /// Sizes rather than payloads. An `extradata` blob and an ICC
  /// profile are both megabyte-scale and neither is readable; what a
  /// reader of a log wants is the stream's identity and whether the
  /// heap fields are populated.
  fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
    f.debug_struct("CodecTicket")
      .field(
        "medium",
        &crate::boundary::media_kind_from_raw(self.codec_type),
      )
      .field("codec_id", &self.codec_id)
      .field("codec_tag", &format_args!("{:#010x}", self.codec_tag))
      .field("format", &self.format)
      .field("width", &self.width)
      .field("height", &self.height)
      .field("sample_rate", &self.sample_rate)
      .field("channels", &self.ch_layout.channels)
      .field("extradata_len", &self.extradata.len())
      .field("coded_side_data", &self.coded_side_data.len())
      .field("footprint_bytes", &self.footprint_bytes)
      .finish_non_exhaustive()
  }
}

// ---------------------------------------------------------------------------
//  Reading the three heap fields.
// ---------------------------------------------------------------------------

/// Copies `extradata` into an owned carrier — the payload only.
///
/// FFmpeg's `AV_INPUT_BUFFER_PADDING_SIZE` trailing zeroes are an
/// allocation contract, not content: decoders read past the end of the
/// buffer and the padding is what makes that defined. Carrying them in
/// the mirror would store zeroes an owned `Arc<[u8]>` needs no reader
/// to be safe past; [`write_extradata`] mints them again on the way
/// back out, which is where they mean something.
///
/// # Safety
///
/// `par` must be a live `*const AVCodecParameters`.
unsafe fn extradata_of(
  par: *const AVCodecParameters,
  policy: ExtradataPolicy,
  stream_index: usize,
) -> Result<FfmpegBytes, ParametersError> {
  if matches!(policy, ExtradataPolicy::Omit) {
    return Ok(FfmpegBytes::empty());
  }
  // SAFETY: `par` is live per the contract; both fields are a pointer
  // and an integer.
  let (ptr, size) = unsafe { ((*par).extradata, (*par).extradata_size) };
  let Ok(len) = usize::try_from(size) else {
    return Ok(FfmpegBytes::empty());
  };
  if ptr.is_null() || len == 0 {
    return Ok(FfmpegBytes::empty());
  }
  // SAFETY: libavformat guarantees `extradata` is readable for
  // `extradata_size` bytes while the parameters live, and the slice is
  // consumed before this function returns.
  // **Fallibly**, because the size is the container's: the footprint
  // above admitted it against the caller's ceiling, and an admitted
  // size that then aborts the process is a budget that did not do its
  // job.
  FfmpegBytes::try_copy_from_slice(unsafe { core::slice::from_raw_parts(ptr, len) })
    .ok_or_else(|| ParametersError::Alloc(ParametersAlloc::new(stream_index)))
}

/// Copies every `coded_side_data` entry into owned entries.
///
/// # Safety
///
/// `par` must be a live `*const AVCodecParameters`.
unsafe fn side_data_of(
  par: *const AVCodecParameters,
  stream_index: usize,
) -> Result<Vec<SideDataEntry>, ParametersError> {
  // SAFETY: `par` is live per the contract.
  let (array, count) = unsafe { ((*par).coded_side_data, (*par).nb_coded_side_data) };
  let alloc = || ParametersError::Alloc(ParametersAlloc::new(stream_index));
  let Ok(count) = usize::try_from(count) else {
    return Ok(Vec::new());
  };
  if array.is_null() || count == 0 {
    return Ok(Vec::new());
  }
  // The descriptor table, reserved fallibly: `nb_coded_side_data` is
  // the container's number too.
  let mut entries = Vec::new();
  entries.try_reserve_exact(count).map_err(|_| alloc())?;
  for index in 0..count {
    // **Never `&*entry`.** `AVPacketSideData::type` is an open C enum
    // and an ABI-compatible FFmpeg newer than these bindings emits
    // kinds absent from the generated Rust enum; forming a typed
    // reference asserts every field inhabits its declared type, which
    // is undefined behaviour before a single field is read.
    //
    // SAFETY: the array is valid for `nb_coded_side_data` contiguous
    // entries per FFmpeg's contract, `index` is below that count, and
    // `addr_of!` computes a field address without forming a reference
    // to the struct containing it.
    let (kind, data, size) = unsafe {
      let entry = array.add(index);
      (
        read_unaligned(addr_of!((*entry).type_).cast::<i32>()),
        read_unaligned(addr_of!((*entry).data)),
        read_unaligned(addr_of!((*entry).size)),
      )
    };
    let payload = if data.is_null() || size == 0 {
      FfmpegBytes::empty()
    } else {
      // SAFETY: the descriptor declares `size` readable bytes at
      // `data`, and the slice is consumed before the loop advances.
      FfmpegBytes::try_copy_from_slice(unsafe { core::slice::from_raw_parts(data, size) })
        .ok_or_else(alloc)?
    };
    entries.push(SideDataEntry::new(kind, payload));
  }
  Ok(entries)
}

/// **Every refusal a channel layout can earn from the container's own
/// declared facts — and not one byte allocated deciding them.**
///
/// Split out of [`channel_layout_of`] so the judging can happen where
/// the paying has not: `admit_streams` calls this for every stream
/// before the track table reserves a row, so a malformed layout on the
/// *last* stream refuses the open before the *first* stream's ticket is
/// copied. `channel_layout_of` calls it again as its own first
/// statement, which is what lets the build below carry no refusals at
/// all and makes the two passes one rule rather than two.
///
/// Judged here, in this order:
///
/// - a non-null `ch_layout.opaque`, which this crate has no way to
///   mirror and will not silently drop;
/// - every structural fault of a `CUSTOM` map —a negative or zero
///   channel count, a null `u.map`, or a sixteen-byte name with no NUL
///   inside it — through
///   [`custom_map_fault`](crate::channel_layout::custom_map_fault),
///   which is the single place that rule is written and which the
///   describe road applies too. `av_channel_layout_copy` (which
///   `avcodec_parameters_to_context` moves this field through)
///   allocates `nb_channels` entries and `memcpy`s from `src->u.map`
///   with no null check of its own, and `av_channel_layout_describe`
///   hands each fixed `name` array to a `%s` conversion; carrying
///   either across would be a faithful round trip of a crash;
/// - a non-null `opaque` on any entry of that map, which only this road
///   cares about because only this road mirrors it.
///
/// Refusing the incomplete map here is also what lets
/// [`write_channel_layout`] rely on `map.len() == nb_channels` for a
/// custom order.
///
/// # Safety
///
/// For the duration of the call, `par` must point at a live, aligned
/// `AVCodecParameters` whose `ch_layout` meets [the `AVChannelLayout`
/// contract](crate::contracts#avchannellayout): `order`, `nb_channels`
/// and `opaque` are read as plain values, the layout is preflighted
/// ([`layout_preflight`](crate::channel_layout::layout_preflight)), and
/// for a `CUSTOM` order with a count the preflight accepts and a non-null
/// map, each of the `nb_channels` entries' `name`, `id` and `opaque` is
/// read — `opaque` as a pointer value, never dereferenced. That is
/// FFmpeg's own contract for a layout it filled, which is the only kind
/// this crate hands in.
#[doc(hidden)]
pub unsafe fn validate_channel_layout(
  par: *const AVCodecParameters,
  stream_index: usize,
) -> Result<(), ParametersError> {
  // SAFETY: `ch_layout` is embedded by value; `addr_of!` reaches each
  // field without forming a reference to the layout, and `order` has
  // the layout of a `c_int`.
  let (order, channels, opaque) = unsafe {
    (
      read_unaligned(addr_of!((*par).ch_layout.order).cast::<i32>()),
      (*par).ch_layout.nb_channels,
      (*par).ch_layout.opaque,
    )
  };
  if !opaque.is_null() {
    return Err(ParametersError::Opaque(ParametersOpaque::new(
      stream_index,
      None,
    )));
  }
  // **The layout's whole structure, through the one function that
  // decides it — every order, not only `CUSTOM`.**
  //
  // The custom map's rules (null map, non-positive count, a name with
  // no NUL) and the other orders' (a count outside FFmpeg's arithmetic
  // range, a `NATIVE` mask that disagrees with its count, an
  // `AMBISONIC` layout whose channels form no ambisonic order) are one
  // preflight, and the describe road, the decoder and the resampler
  // apply exactly this one — so the pass that admits and the road that
  // materialises cannot come to different answers about a layout.
  //
  // An earlier round stopped here for every order but `CUSTOM`, on the
  // argument that a `uint64_t` mask cannot be malformed. The mask is
  // not the only field.
  //
  // SAFETY: this function's own contract, forwarded: `ch_layout` is
  // embedded in a live `AVCodecParameters`, `order` was read as the
  // `c_int` it is, and for a custom order its map holds `nb_channels`
  // entries.
  unsafe { crate::channel_layout::layout_preflight(addr_of!((*par).ch_layout)) }
    .map_err(|fault| ParametersError::from_layout_fault(stream_index, fault))?;

  if order != AVChannelOrder::AV_CHANNEL_ORDER_CUSTOM as i32 {
    return Ok(());
  }
  // The map is sound to walk now; what remains is the one thing the
  // describe road has no opinion about, because it never mirrors it.
  //
  // SAFETY: the order names the `map` arm, and the preflight above
  // proved it non-null with a positive count.
  let map = unsafe { (*par).ch_layout.u.map };
  let count = channels.max(0) as usize;
  for index in 0..count {
    // Field pointers, never `&AVChannelCustom`: `id` is an open enum
    // with the same hazard as a side-data type id.
    //
    // SAFETY: the contract above makes the map `count` entries long,
    // `index` is below that count, and the read goes through
    // `addr_of!`.
    let opaque = unsafe { read_unaligned(addr_of!((*map.add(index)).opaque)) };
    if !opaque.is_null() {
      return Err(ParametersError::Opaque(ParametersOpaque::new(
        stream_index,
        Some(index),
      )));
    }
  }
  Ok(())
}

/// Mirrors the embedded `AVChannelLayout`.
///
/// Carries no refusals of its own beyond an allocator's: everything
/// this layout can be refused for is decided by
/// [`validate_channel_layout`], which runs first here and has already
/// run during admission.
///
/// # Safety
///
/// `par` must be a live `*const AVCodecParameters`, with
/// [`validate_channel_layout`]'s contract for a custom map.
unsafe fn channel_layout_of(
  par: *const AVCodecParameters,
  stream_index: usize,
) -> Result<ChannelLayoutTicket, ParametersError> {
  // SAFETY: the caller's contract, forwarded unchanged.
  unsafe { validate_channel_layout(par, stream_index) }?;

  // SAFETY: as in the validation above — field reads through
  // `addr_of!`, with `order` read as the `c_int` it is.
  let (order, channels) = unsafe {
    (
      read_unaligned(addr_of!((*par).ch_layout.order).cast::<i32>()),
      (*par).ch_layout.nb_channels,
    )
  };

  let mut layout = ChannelLayoutTicket {
    order,
    channels,
    mask: 0,
    map: Vec::new(),
  };
  match crate::channel_layout::LayoutArm::of(order) {
    // SAFETY: `NATIVE` and `AMBISONIC` are the two orders whose
    // contract defines `u.mask`, and this arm names exactly them.
    crate::channel_layout::LayoutArm::Mask => {
      layout.mask = unsafe { (*par).ch_layout.u.mask };
      return Ok(layout);
    }
    // **`UNSPEC`, and any order this build has never heard of: the
    // union is not read at all.** FFmpeg's own header declares it
    // undefined for an unspecified layout, and this mirror used to
    // treat "not custom" as "mask-backed" — importing whatever bytes
    // happened to sit there into a field that `mask()` hands out and
    // that the derived `Debug`, `PartialEq` and `Hash` then expose and
    // compare. The stored zero is a value this crate chose, not one it
    // read.
    crate::channel_layout::LayoutArm::Undefined => return Ok(layout),
    crate::channel_layout::LayoutArm::Map => {}
  }

  // SAFETY: the order names the `map` arm, and the validation above
  // proved it non-null with a positive count.
  let map = unsafe { (*par).ch_layout.u.map };
  let count = channels.max(0) as usize;
  layout
    .map
    .try_reserve_exact(count)
    .map_err(|_| ParametersError::Alloc(ParametersAlloc::new(stream_index)))?;
  for index in 0..count {
    // SAFETY: FFmpeg's contract makes the map `count` entries long,
    // `index` is below that count, and every read goes through
    // `addr_of!`.
    let (id, name) = unsafe {
      let entry = map.add(index);
      (
        read_unaligned(addr_of!((*entry).id).cast::<i32>()),
        read_unaligned(addr_of!((*entry).name).cast::<[u8; 16]>()),
      )
    };
    layout.map.push(CustomChannel::new(id, name));
  }
  Ok(layout)
}

// ---------------------------------------------------------------------------
//  Writing the three heap fields.
// ---------------------------------------------------------------------------

/// The `ENOMEM` a heap field's allocation reports when it fails.
fn field_alloc_failed(stream_index: usize) -> ParametersError {
  ParametersError::Copy(ParametersCopy::new(
    stream_index,
    ffmpeg_next::Error::Other {
      errno: libc::ENOMEM,
    },
  ))
}

/// Allocates `extradata` and its padding, and copies the payload in.
///
/// # Safety
///
/// `dst` must be a live `*mut AVCodecParameters` whose `extradata` is
/// null.
unsafe fn write_extradata(
  dst: *mut AVCodecParameters,
  payload: &[u8],
  stream_index: usize,
) -> Result<(), ParametersError> {
  if payload.is_empty() {
    return Ok(());
  }
  let size = i32::try_from(payload.len()).map_err(|_| field_alloc_failed(stream_index))?;
  let padded = payload
    .len()
    .checked_add(AV_INPUT_BUFFER_PADDING_SIZE as usize)
    .ok_or_else(|| field_alloc_failed(stream_index))?;
  // SAFETY: `av_mallocz` returns zeroed memory or null; the copy
  // writes exactly `payload.len()` bytes into an allocation that is
  // `AV_INPUT_BUFFER_PADDING_SIZE` longer, leaving the padding zero —
  // which is the contract decoders read past the end under.
  unsafe {
    let buffer = av_mallocz(padded).cast::<u8>();
    if buffer.is_null() {
      return Err(field_alloc_failed(stream_index));
    }
    copy_nonoverlapping(payload.as_ptr(), buffer, payload.len());
    (*dst).extradata = buffer;
    (*dst).extradata_size = size;
  }
  Ok(())
}

/// Allocates the `coded_side_data` descriptor array and each payload.
///
/// # Safety
///
/// `dst` must be a live `*mut AVCodecParameters` whose
/// `coded_side_data` is null.
unsafe fn write_side_data(
  dst: *mut AVCodecParameters,
  entries: &[SideDataEntry],
  stream_index: usize,
) -> Result<(), ParametersError> {
  if entries.is_empty() {
    return Ok(());
  }
  let count = i32::try_from(entries.len()).map_err(|_| field_alloc_failed(stream_index))?;
  let bytes = entries
    .len()
    .checked_mul(core::mem::size_of::<AVPacketSideData>())
    .ok_or_else(|| field_alloc_failed(stream_index))?;

  // SAFETY: `dst` is live with a null `coded_side_data`. The array is
  // attached before any payload is filled in, so a failure part way
  // leaves the destructor a well-formed array to walk: the entries it
  // has not reached are zeroed, and freeing a null payload is a no-op.
  unsafe {
    let array = av_mallocz(bytes).cast::<AVPacketSideData>();
    if array.is_null() {
      return Err(field_alloc_failed(stream_index));
    }
    (*dst).coded_side_data = array;
    (*dst).nb_coded_side_data = count;

    for (index, entry) in entries.iter().enumerate() {
      let into = array.add(index);
      // The type id travels as the raw bits it is on the wire, for the
      // reason the read did: a kind these bindings cannot name is
      // still a kind the file carries and a decoder may want.
      write_unaligned(addr_of_mut!((*into).type_).cast::<i32>(), entry.kind());
      let payload = entry.data();
      if payload.is_empty() {
        continue;
      }
      let buffer = av_mallocz(payload.len()).cast::<u8>();
      if buffer.is_null() {
        return Err(field_alloc_failed(stream_index));
      }
      copy_nonoverlapping(payload.as_ptr(), buffer, payload.len());
      write_unaligned(addr_of_mut!((*into).data), buffer);
      write_unaligned(addr_of_mut!((*into).size), payload.len());
    }
  }
  Ok(())
}

/// Writes the channel layout, allocating a custom map when the order
/// names one.
///
/// Written field by field rather than through
/// `av_channel_layout_copy`, because the source it would copy from is
/// the thing this road has abolished: there is no live
/// `AVChannelLayout` to copy, only owned Rust. The one allocation is
/// the custom map, which [`CodecTicket::mirror`] already measured and
/// admitted.
///
/// # Safety
///
/// `dst` must be a live `*mut AVCodecParameters` whose `ch_layout` is
/// the zeroed (`AV_CHANNEL_ORDER_UNSPEC`) state
/// `avcodec_parameters_alloc` leaves, owning no map.
unsafe fn write_channel_layout(
  dst: *mut AVCodecParameters,
  layout: &ChannelLayoutTicket,
  stream_index: usize,
) -> Result<(), ParametersError> {
  // SAFETY: `dst` is live and its layout owns nothing yet; `order` is
  // written as the same 32-bit pattern the mirror read.
  unsafe {
    write_unaligned(
      addr_of_mut!((*dst).ch_layout.order).cast::<i32>(),
      layout.order(),
    );
    (*dst).ch_layout.opaque = core::ptr::null_mut();
  }

  match crate::channel_layout::LayoutArm::of(layout.order()) {
    // SAFETY: the order names the `mask` arm.
    crate::channel_layout::LayoutArm::Mask => {
      unsafe {
        (*dst).ch_layout.nb_channels = layout.channels();
        (*dst).ch_layout.u.mask = layout.mask();
      }
      return Ok(());
    }
    // **The union is left as `avcodec_parameters_alloc` zeroed it.**
    // An order that defines no arm gets no arm written: handing
    // libavcodec a mask for an unspecified layout would be inventing a
    // fact about a field FFmpeg says means nothing there, and the
    // mirror does not read one either.
    crate::channel_layout::LayoutArm::Undefined => {
      // SAFETY: `dst` is live and the count is a plain `int` field.
      unsafe {
        (*dst).ch_layout.nb_channels = layout.channels();
      }
      return Ok(());
    }
    crate::channel_layout::LayoutArm::Map => {}
  }

  // **`nb_channels` comes from the map, not from the stored field.**
  // The two are equal — [`channel_layout_of`] refuses a custom layout
  // it cannot map in full, and the fields are private, so no other
  // value can exist. Writing the count from the array anyway is what
  // makes that structural rather than remembered: the layout handed to
  // libavcodec can never declare more channels than the array it points
  // at, which is precisely the shape `av_channel_layout_copy` would
  // `memcpy` past the end of.
  debug_assert_eq!(
    layout.map().len(),
    layout.channels().max(0) as usize,
    "a custom layout's map length is its channel count",
  );
  let count = layout.map().len();
  if count == 0 {
    // Unreachable through `mirror`, and fail-closed if a future
    // constructor ever makes it reachable.
    return Err(ParametersError::ChannelMap(ParametersChannelMap::new(
      stream_index,
      layout.channels(),
    )));
  }

  let declared = i32::try_from(count).map_err(|_| field_alloc_failed(stream_index))?;
  let bytes = count
    .checked_mul(core::mem::size_of::<AVChannelCustom>())
    .ok_or_else(|| field_alloc_failed(stream_index))?;
  // SAFETY: `av_mallocz` returns zeroed memory or null. The map and the
  // count it describes are attached together, before the entries are
  // filled in, so a later failure leaves a well-formed (zeroed) map for
  // the destructor to free, and every write goes through
  // `addr_of_mut!` rather than a typed reference.
  unsafe {
    let map = av_mallocz(bytes).cast::<AVChannelCustom>();
    if map.is_null() {
      return Err(field_alloc_failed(stream_index));
    }
    (*dst).ch_layout.u.map = map;
    (*dst).ch_layout.nb_channels = declared;
    for (index, channel) in layout.map().iter().enumerate() {
      let into = map.add(index);
      write_unaligned(addr_of_mut!((*into).id).cast::<i32>(), channel.id());
      write_unaligned(
        addr_of_mut!((*into).name).cast::<[u8; 16]>(),
        *channel.name_bytes(),
      );
      write_unaligned(addr_of_mut!((*into).opaque), core::ptr::null_mut());
    }
  }
  Ok(())
}

#[cfg(test)]
mod tests;
