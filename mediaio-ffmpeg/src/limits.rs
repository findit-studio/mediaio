//! Resource ceilings — the finite budgets every copy across the FFmpeg
//! boundary is checked against **before** it allocates.
//!
//! These seats are tier one and tier two of the [resource governance
//! contract][gov]: what this crate allocates itself, and the FFmpeg
//! knobs it sets on the caller's behalf. The contract also states what
//! they do **not** bound, and what a deployment needing a hard memory
//! bound puts underneath them — read it before sizing these for a
//! hostile-input service.
//!
//! # Why these exist
//!
//! 0.9 made every exit copy (see [the amputation contract][law]). A copy
//! is a decision to allocate whatever the file asks for, and a container
//! is untrusted input: a header claiming 100000×100000 pixels, a packet
//! claiming a gigabyte, a Matroska with a thousand attached "fonts" all
//! cost nothing to write and everything to honour. Through 0.8 the
//! frame and packet payloads were *views*, so an absurd claim cost a
//! refcount; from 0.9 it costs memory, and the claim has to be judged
//! before it is paid.
//!
//! Every seat here is a **finite default**, not an `Option`. There is no
//! "unlimited" spelling on purpose: the shape that lets a caller ask for
//! no ceiling is the shape a caller reaches for once, in a hurry, and
//! never revisits. A caller who needs more says how much more.
//!
//! # Two layers, one number
//!
//! [`FrameLimits::max_pixels`] is enforced twice: once here, against the
//! frame this crate is about to copy, and once inside libavcodec, by
//! writing the same number to `AVCodecContext.max_pixels` when a decoder
//! is opened. The second is the one that matters most — it makes the
//! decoder refuse before allocating *its* huge frame, which this crate
//! would otherwise only get to reject after FFmpeg had already paid for
//! it.
//!
//! # The house shape
//!
//! `DEFAULT_*` consts, `Copy` options structs with `new` / getters /
//! `with_*` / `set_*`, and a `with_*` builder on each session.
//!
//! [law]: mediaspine::adapter#the-carrier-amputation-contract
//! [gov]: mediaspine::adapter#the-resource-governance-contract

/// Default ceiling on a decoded frame's pixel count — 256 mebipixels.
///
/// **Why this number.** The largest picture anything ships is 8K UHD
/// (7680×4320 ≈ 33 Mpx); 16K×16K, which nothing does, is 268 Mpx. This
/// default sits exactly there: every real frame passes, and the
/// hand-written header claiming 100000×100000 (10 Gpx) is refused
/// before a byte is allocated — by libavcodec first, since the same
/// number is written to `AVCodecContext.max_pixels`, and by this crate
/// second.
///
/// FFmpeg's own default for that option is `INT_MAX`, i.e. no ceiling
/// worth the name. Overriding it is the point.
pub const DEFAULT_MAX_PIXELS: u64 = 256 * 1024 * 1024;

/// Default ceiling on the bytes one decoded frame may export — 512 MiB.
///
/// **Why this number.** Pixels alone do not bound the copy: bit depth,
/// plane count and stride padding all multiply it. The widest realistic
/// frame is 8K 4:4:4 16-bit with alpha (7680×4320×8 bytes ≈ 253 MiB);
/// 8K P010 is ~96 MiB and 4K P010 ~24 MiB. 512 MiB clears the worst of
/// those by 2× and still bounds a single frame to something a process
/// can survive.
///
/// Checked against the sum of what the planes will actually export —
/// after the stride decision, so it is the number this crate is about
/// to allocate rather than an estimate of it.
pub const DEFAULT_MAX_FRAME_BYTES: usize = 512 * 1024 * 1024;

/// Default ceiling on one packet's payload — 1 GiB.
///
/// **Why this number.** Deliberately ceiling-class rather than tuned:
/// `AVPacket.size` is a `c_int`, so 2 GiB is the structural maximum and
/// this halves it. Real packets are nowhere near — an intra-only 8K
/// ProRes 4444 XQ frame is ~10 MB, an uncompressed v210 8K frame ~88
/// MB, and a whole-file attachment (the largest packet shape that
/// exists) is bounded far below by
/// [`DEFAULT_MAX_ATTACHMENT_BYTES`]. The job here is to refuse the
/// forged `size` field, not to second-guess a codec.
pub const DEFAULT_MAX_PACKET_BYTES: usize = 1024 * 1024 * 1024;

/// Default ceiling on one attachment's payload — 64 MiB.
///
/// **Why this number.** An attachment is a whole file: cover art or a
/// font. A generous cover is a 4000×4000 PNG at ~20 MB; the largest
/// fonts in circulation are CJK families at ~30 MB. 64 MiB clears both
/// and is two orders of magnitude under the packet ceiling, which is
/// right — an attachment is the one payload captured *eagerly*, at
/// open, before a caller has asked for anything.
pub const DEFAULT_MAX_ATTACHMENT_BYTES: usize = 64 * 1024 * 1024;

/// Default ceiling on **all** attachments in one file, together — 256
/// MiB.
///
/// **Why this number, and why it is separate.** The per-attachment
/// ceiling bounds one payload; nothing in it bounds a container that
/// attaches four hundred of them. A subtitled release with a full ASS
/// font set attaches perhaps ten to thirty fonts of a few MB each —
/// call it 100 MB at the high end. 256 MiB clears that and refuses the
/// file whose attachment table is the attack.
///
/// This budget is spent at **open**, because that is when this crate
/// captures every attachment (the demux tier's "exactly one packet,
/// before any timed packet" contract is kept by construction, and the
/// construction is eager). A file that exhausts it fails to open, with
/// the arm naming which track ran the total past the line.
pub const DEFAULT_MAX_TOTAL_ATTACHMENT_BYTES: usize = 256 * 1024 * 1024;

/// Default ceiling on one stream's codec-parameter heap — 16 MiB.
///
/// **What it bounds.** `AVCodecParameters` has three heap seats and all
/// three come from the file: `extradata`, every entry of
/// `coded_side_data`, and a custom `ch_layout` channel map. A track
/// row's codec ticket mirrors all three, and rebuilding one for a
/// decoder allocates all three again.
///
/// **Why this number.** The honest end of the range is small — H.264
/// SPS/PPS extradata is tens of bytes, HEVC's a few hundred, FLAC and
/// ALAC headers a couple of kilobytes. What sets the ceiling is
/// `coded_side_data`: a MOV `prof` atom carries an **ICC profile**, and
/// those are legitimately large — a few kilobytes for sRGB, half a
/// megabyte to two megabytes for a real camera or display profile, and
/// the largest device-link profiles in circulation reach roughly ten.
/// 16 MiB clears all of that and still refuses the forged atom.
pub const DEFAULT_MAX_CODEC_PARAMETER_BYTES: usize = 16 * 1024 * 1024;

/// Default ceiling on **every** stream's codec-parameter heap in one
/// file, together — 64 MiB.
///
/// **Why this number, and why it is separate.** The per-stream ceiling
/// bounds one track's parameters; nothing in it bounds a container that
/// declares two hundred tracks each carrying a two-megabyte profile.
/// Four tracks with a large ICC profile apiece is the realistic high
/// end, so 64 MiB clears it and refuses the stream table that is the
/// attack.
///
/// Charged over **all** streams, not just the ones a caller will
/// decode: the track table is built eagerly at open, so every stream's
/// parameters are mirrored whether or not anybody asks for them.
pub const DEFAULT_MAX_TOTAL_CODEC_PARAMETER_BYTES: usize = 64 * 1024 * 1024;

/// The defaults have to hold together, and these say how — at compile
/// time, because every term is a constant and a fact a build can check
/// is a fact no test run has to.
///
/// Each clause is a claim the doc comments above make in prose:
/// - every ceiling is finite and non-zero (a zero ceiling refuses
///   everything, which is the opposite failure and just as bad);
/// - 8K UHD, and the widest realistic frame, pass;
/// - the 100000×100000 header does not;
/// - a whole-file attachment budget below the per-attachment one, or a
///   per-packet ceiling below the per-attachment one, would be
///   incoherent — the narrower seat could never fire;
/// - a per-packet ceiling above `c_int::MAX` could never fire either,
///   since `AVPacket.size` cannot express it.
const _: () = {
  assert!(DEFAULT_MAX_PIXELS > 0 && DEFAULT_MAX_PIXELS < u64::MAX);
  assert!(DEFAULT_MAX_FRAME_BYTES > 0 && DEFAULT_MAX_FRAME_BYTES < usize::MAX);
  assert!(DEFAULT_MAX_PACKET_BYTES > 0 && DEFAULT_MAX_PACKET_BYTES < usize::MAX);
  assert!(DEFAULT_MAX_ATTACHMENT_BYTES > 0);
  assert!(DEFAULT_MAX_TOTAL_ATTACHMENT_BYTES > 0);

  // 8K UHD — the largest picture anything ships — must decode.
  assert!(7680 * 4320 < DEFAULT_MAX_PIXELS);
  // And the widest realistic frame: 8K 4:4:4 16-bit with alpha.
  assert!(7680 * 4320 * 8 < DEFAULT_MAX_FRAME_BYTES);

  // **8K must decode in the widest format that exists, not just the
  // widest realistic one.** The byte ceiling is pushed into libavcodec
  // as a pixel ceiling priced at the worst per-pixel cost any format
  // this build can emit — 16 bytes, reached by `rgbaf32` and its seven
  // siblings — because a container's declared format is not an upper
  // bound on what its decoder produces. That makes the effective pixel
  // ceiling `max_frame_bytes / 16`, and this is the assertion that
  // keeps 8K inside it: at 33.18 Mpx and 16 bytes an 8K `rgbaf32` frame
  // is 506 MiB, which 512 MiB clears with about 1% to spare.
  //
  // If `DEFAULT_MAX_FRAME_BYTES` is ever lowered, or a future FFmpeg
  // adds a format wider than 16 bytes per pixel, this fails the build
  // rather than quietly refusing 8K at run time.
  assert!(7680 * 4320 * 16 < DEFAULT_MAX_FRAME_BYTES);
  // The header a fuzzer writes must not.
  assert!(100_000 * 100_000 > DEFAULT_MAX_PIXELS);

  assert!(DEFAULT_MAX_TOTAL_ATTACHMENT_BYTES >= DEFAULT_MAX_ATTACHMENT_BYTES);
  assert!(DEFAULT_MAX_PACKET_BYTES >= DEFAULT_MAX_ATTACHMENT_BYTES);
  assert!(DEFAULT_MAX_PACKET_BYTES <= i32::MAX as usize);

  assert!(DEFAULT_MAX_CODEC_PARAMETER_BYTES > 0);
  assert!(DEFAULT_MAX_TOTAL_CODEC_PARAMETER_BYTES >= DEFAULT_MAX_CODEC_PARAMETER_BYTES);
  // A ten-megabyte device-link ICC profile is real media and must pass.
  assert!(10 * 1024 * 1024 < DEFAULT_MAX_CODEC_PARAMETER_BYTES);

  // **The two ICC policies agree, and this is what keeps them agreeing.**
  // The same profile can arrive as a track parameter (`coded_side_data`)
  // or as a decoded still's frame side data, and a ceiling that admits
  // it on one road and drops it on the other is not a policy, it is an
  // accident of which road the file took.
  assert!(DEFAULT_MAX_IMAGE_SIDE_DATA_BYTES >= DEFAULT_MAX_CODEC_PARAMETER_BYTES);
};

/// What one decoded frame may cost.
///
/// Carried by every session that decodes frames and handed to the
/// conversion that copies them. See the [module docs](self) for why the
/// seats are finite and how [`Self::max_pixels`] reaches libavcodec as
/// well as this crate.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct FrameLimits {
  max_pixels: u64,
  max_frame_bytes: usize,
  max_image_side_data_bytes: usize,
}

impl Default for FrameLimits {
  #[inline]
  fn default() -> Self {
    Self::new()
  }
}

impl FrameLimits {
  /// The defaults: [`DEFAULT_MAX_PIXELS`] and
  /// [`DEFAULT_MAX_FRAME_BYTES`].
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new() -> Self {
    Self {
      max_pixels: DEFAULT_MAX_PIXELS,
      max_frame_bytes: DEFAULT_MAX_FRAME_BYTES,
      max_image_side_data_bytes: DEFAULT_MAX_IMAGE_SIDE_DATA_BYTES,
    }
  }

  /// Most pixels one decoded frame may have.
  ///
  /// Also written to `AVCodecContext.max_pixels` when a decoder is
  /// opened from these limits, so libavcodec refuses an oversized
  /// picture before allocating it.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn max_pixels(&self) -> u64 {
    self.max_pixels
  }
  /// Most bytes one decoded frame's planes may export, together.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn max_frame_bytes(&self) -> usize {
    self.max_frame_bytes
  }
  /// The ceiling on side data one decoded **still** may carry.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn max_image_side_data_bytes(&self) -> usize {
    self.max_image_side_data_bytes
  }

  /// Sets the pixel ceiling (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_max_pixels(mut self, value: u64) -> Self {
    self.max_pixels = value;
    self
  }
  /// Sets the per-frame byte ceiling (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_max_frame_bytes(mut self, value: usize) -> Self {
    self.max_frame_bytes = value;
    self
  }
  /// Sets the decoded-still side-data ceiling (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_max_image_side_data_bytes(mut self, value: usize) -> Self {
    self.max_image_side_data_bytes = value;
    self
  }

  /// Sets the pixel ceiling in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_max_pixels(&mut self, value: u64) -> &mut Self {
    self.max_pixels = value;
    self
  }
  /// Sets the per-frame byte ceiling in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_max_frame_bytes(&mut self, value: usize) -> &mut Self {
    self.max_frame_bytes = value;
    self
  }
  /// Sets the decoded-still side-data ceiling in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_max_image_side_data_bytes(&mut self, value: usize) -> &mut Self {
    self.max_image_side_data_bytes = value;
    self
  }
}

/// Default ceiling on the bytes libavformat may **read** while probing
/// and analysing a container — 5 MiB, which is FFmpeg's own
/// `probesize` default.
///
/// **What this seat is for, and what it is not.** Every other budget in
/// this crate bounds a copy *this crate* makes. This one bounds work
/// **libavformat does before this crate is handed anything**:
/// `avformat_open_input` and `avformat_find_stream_info` build the
/// attached-picture, extradata and coded-side-data buffers themselves,
/// so the attachment budgets — which measure this crate's copies —
/// arrive after the original allocation has already happened.
///
/// A parser cannot allocate from bytes it was never given, so bounding
/// the read is the instrument that reaches furthest back. See
/// [`DemuxLimits::max_probe_bytes`] for how far it actually reaches and
/// what it does not.
pub const DEFAULT_MAX_PROBE_BYTES: u64 = 5 * 1024 * 1024;

/// Default ceiling on the number of streams a container may declare —
/// FFmpeg's own `max_streams` default.
///
/// Each declared stream costs an `AVStream` and its `AVCodecParameters`
/// inside libavformat, before this crate sees a track table, so a
/// header claiming a hundred thousand streams is an allocation this
/// crate's per-track budgets are downstream of.
pub const DEFAULT_MAX_STREAMS: u32 = 1000;

/// Default ceiling on the number of chapters a container may declare —
/// 4096.
///
/// **Why this number.** A DVD's chapter count tops out at 99, a
/// Matroska edition rarely reaches three figures, and the fattest real
/// table anyone ships is an audiobook filing each of its tracks as a
/// chapter — a few thousand at the far end. 4096 clears all of that and
/// still bounds this crate's mirror at a few hundred kibibytes.
///
/// **Why a ceiling exists at all.** A chapter costs libavformat almost
/// nothing — an `AVChapter` is four scalars and a dictionary pointer —
/// so a header can declare an enormous table for very few bytes, and
/// [`DEFAULT_MAX_STREAMS`] does not reach it: chapters are not streams.
/// The mirror this crate builds is the larger of the two (an owned
/// title, a `Timebase`, two `Timestamp`s per row), so the count is
/// judged before a byte of it is reserved.
pub const DEFAULT_MAX_CHAPTERS: u32 = 4096;

/// Default ceiling on the bytes every chapter title in one file may
/// hold together — 1 MiB.
///
/// **Why this number.** A chapter title is a line of prose: tens of
/// bytes, a few hundred at the outside. 1 MiB is 256 bytes for each of
/// [`DEFAULT_MAX_CHAPTERS`] chapters, so every real table passes with
/// room to spare.
///
/// **Why an aggregate rather than a per-title cap.** A per-title cap
/// already exists one layer down: the metadata reader refuses any
/// single dictionary value past 64 KiB rather than truncating it. On
/// its own that leaves the table's total at the count times that cap —
/// 256 MiB at the two defaults. This seat is what turns those two
/// finite numbers into a small one.
pub const DEFAULT_MAX_TOTAL_CHAPTER_TITLE_BYTES: usize = 1024 * 1024;

/// Default ceiling on the bytes every **stream's** retained metadata in
/// one file may hold together — 4 MiB.
///
/// **What it bounds.** A track row keeps three values off each
/// stream's metadata dictionary: the `filename` an attachment was
/// attached under, its declared `mimetype`, and the track's
/// `language`. All three are mirrored eagerly, for every admitted
/// stream, before the first packet is read.
///
/// **Why a ceiling exists at all.** [`DEFAULT_MAX_STREAMS`] bounds how
/// many streams a header may declare and says nothing about what each
/// one may carry. At that ceiling, three values of the 64 KiB a single
/// metadata value may reach — in bytes that are not UTF-8, so each one
/// triples on the way through lossy decoding — is roughly 562 MiB
/// retained during an open that has not been asked for a single
/// packet. The path entrypoint has no hard read meter to fall back on,
/// either.
///
/// **Why this number.** Real metadata is tiny: a filename is tens of
/// bytes, a MIME type forty, a language tag three. A file with a full
/// ASS font set — thirty attachments, each with a name and a type —
/// spends a few kilobytes. 4 MiB clears every real file by orders of
/// magnitude and turns the hostile one into a named refusal.
pub const DEFAULT_MAX_TOTAL_STREAM_METADATA_BYTES: usize = 4 * 1024 * 1024;

/// Default ceiling on the side data one decoded **still** may carry —
/// the same 16 MiB as [`DEFAULT_MAX_CODEC_PARAMETER_BYTES`], and the
/// same reason.
///
/// **Why the still road needs its own number.** The shared stream
/// collector caps frame side data at 256 KiB in total and *silently
/// drops* whatever does not fit. On a video stream that is defensible:
/// side data there is small, per-frame, and repeated. On a still it is
/// wrong twice over. A decoded image's side data is dominated by the
/// one thing that is legitimately megabytes — an **ICC profile** — and
/// the parameter budget next door already admits those up to 16 MiB, so
/// the same profile was admitted as a track parameter and swallowed as
/// a frame annotation. Worse, the drop is positional: entries after the
/// cap are skipped, and `AV_FRAME_DATA_DISPLAYMATRIX` — the orientation
/// this crate reads off a still — is a small entry that a large ICC
/// profile ahead of it pushed out. A picture came back silently rotated
/// wrong.
///
/// So the still road gets a seat sized to what it actually carries, and
/// over-budget is a **named refusal** rather than a quiet truncation:
/// side data that cannot be carried whole is a fact about the picture,
/// not a detail to drop.
pub const DEFAULT_MAX_IMAGE_SIDE_DATA_BYTES: usize = DEFAULT_MAX_CODEC_PARAMETER_BYTES;

/// Default ceiling on the compressed bytes one **image** decode may be
/// handed — 64 MiB, the attachment family.
///
/// **Why the attachment family and not the packet one.** What
/// `mediadecode-ffmpeg`'s `FfmpegImageDecoder` decodes *is* an
/// attachment: a whole
/// file a container handed over eagerly. When it arrives through the
/// demuxer it has already been charged against
/// [`DEFAULT_MAX_ATTACHMENT_BYTES`], and this seat is what keeps the
/// same ceiling in force when a caller builds the packet itself — the
/// one road that skips the demux tier entirely. A 1 GiB packet ceiling
/// here would mean the direct road was a gigabyte more permissive than
/// the demuxed one for the same bytes.
pub const DEFAULT_MAX_IMAGE_INPUT_BYTES: usize = DEFAULT_MAX_ATTACHMENT_BYTES;

/// What one packet's payload may cost.
///
/// Carried by the boundary conversions and by an open demux session.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct PacketLimits {
  max_packet_bytes: usize,
}

impl Default for PacketLimits {
  #[inline]
  fn default() -> Self {
    Self::new()
  }
}

impl PacketLimits {
  /// The default: [`DEFAULT_MAX_PACKET_BYTES`].
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new() -> Self {
    Self {
      max_packet_bytes: DEFAULT_MAX_PACKET_BYTES,
    }
  }

  /// Most bytes one packet's payload may carry.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn max_packet_bytes(&self) -> usize {
    self.max_packet_bytes
  }

  /// Sets the per-packet ceiling (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_max_packet_bytes(mut self, value: usize) -> Self {
    self.max_packet_bytes = value;
    self
  }
  /// Sets the per-packet ceiling in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_max_packet_bytes(&mut self, value: usize) -> &mut Self {
    self.max_packet_bytes = value;
    self
  }
}

/// What opening and running one **decoder** may spend.
///
/// Composes [`FrameLimits`] — what the frames it produces may cost —
/// with the two things a decoder spends before it has produced
/// anything: copying the caller's codec parameters into an
/// `AVCodecContext`, and copying the caller's compressed bytes into an
/// `AVPacket`.
///
/// Taken at `open` by every decoder session in this crate, for the
/// reason [`FrameLimits`] gives: half of it is written into an
/// `AVCodecContext` whose ceilings cannot move after `avcodec_open2`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct DecoderLimits {
  frame: FrameLimits,
  max_codec_parameter_bytes: usize,
  max_packet_bytes: usize,
  max_image_input_bytes: usize,
}

impl Default for DecoderLimits {
  #[inline]
  fn default() -> Self {
    Self::new()
  }
}

impl DecoderLimits {
  /// The defaults: [`FrameLimits::new`],
  /// [`DEFAULT_MAX_CODEC_PARAMETER_BYTES`], [`DEFAULT_MAX_PACKET_BYTES`]
  /// and [`DEFAULT_MAX_IMAGE_INPUT_BYTES`].
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new() -> Self {
    Self {
      frame: FrameLimits::new(),
      max_codec_parameter_bytes: DEFAULT_MAX_CODEC_PARAMETER_BYTES,
      max_packet_bytes: DEFAULT_MAX_PACKET_BYTES,
      max_image_input_bytes: DEFAULT_MAX_IMAGE_INPUT_BYTES,
    }
  }

  /// What one decoded frame may cost.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn frame(&self) -> FrameLimits {
    self.frame
  }
  /// Most heap bytes the codec parameters this decoder is opened from
  /// may hold.
  ///
  /// Enforced at the choke point every road into libavcodec passes
  /// through, so a decoder cannot be opened over parameters nobody
  /// measured.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn max_codec_parameter_bytes(&self) -> usize {
    self.max_codec_parameter_bytes
  }
  /// Most compressed bytes one packet handed to a **stream** decoder
  /// may carry.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn max_packet_bytes(&self) -> usize {
    self.max_packet_bytes
  }

  /// [`Self::max_packet_bytes`] as the [`PacketLimits`] the boundary
  /// conversions take, so the send leg and the receive leg are handed
  /// the same seat rather than two numbers that could drift.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn packet_limits(&self) -> PacketLimits {
    PacketLimits::new().with_max_packet_bytes(self.max_packet_bytes)
  }
  /// Most compressed bytes one **image** decode may be handed. See
  /// [`DEFAULT_MAX_IMAGE_INPUT_BYTES`] for why this is its own seat.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn max_image_input_bytes(&self) -> usize {
    self.max_image_input_bytes
  }

  /// Sets the frame ceilings (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_frame(mut self, value: FrameLimits) -> Self {
    self.frame = value;
    self
  }
  /// Sets the codec-parameter ceiling (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_max_codec_parameter_bytes(mut self, value: usize) -> Self {
    self.max_codec_parameter_bytes = value;
    self
  }
  /// Sets the per-packet ceiling (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_max_packet_bytes(mut self, value: usize) -> Self {
    self.max_packet_bytes = value;
    self
  }
  /// Sets the image-input ceiling (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_max_image_input_bytes(mut self, value: usize) -> Self {
    self.max_image_input_bytes = value;
    self
  }

  /// Sets the frame ceilings in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_frame(&mut self, value: FrameLimits) -> &mut Self {
    self.frame = value;
    self
  }
  /// Sets the codec-parameter ceiling in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_max_codec_parameter_bytes(&mut self, value: usize) -> &mut Self {
    self.max_codec_parameter_bytes = value;
    self
  }
  /// Sets the per-packet ceiling in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_max_packet_bytes(&mut self, value: usize) -> &mut Self {
    self.max_packet_bytes = value;
    self
  }
  /// Sets the image-input ceiling in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_max_image_input_bytes(&mut self, value: usize) -> &mut Self {
    self.max_image_input_bytes = value;
    self
  }
}

/// What one demux session may spend: on any single packet, on any
/// single attachment, on every attachment in the file together, and on
/// the container's chapter table.
///
/// Handed to `mediadecode-ffmpeg`'s `FfmpegDemuxer::open_with`
/// rather than set afterwards, because the attachment budget is spent
/// *during* the open — every attachment payload is captured before the
/// first timed packet is read, which is what makes the demux tier's
/// delivery contract true by construction.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct DemuxLimits {
  packet: PacketLimits,
  max_attachment_bytes: usize,
  max_total_attachment_bytes: usize,
  max_codec_parameter_bytes: usize,
  max_total_codec_parameter_bytes: usize,
  max_probe_bytes: u64,
  max_streams: u32,
  max_chapters: u32,
  max_total_chapter_title_bytes: usize,
  max_total_stream_metadata_bytes: usize,
}

impl Default for DemuxLimits {
  #[inline]
  fn default() -> Self {
    Self::new()
  }
}

impl DemuxLimits {
  /// The defaults: [`PacketLimits::new`],
  /// [`DEFAULT_MAX_ATTACHMENT_BYTES`] and
  /// [`DEFAULT_MAX_TOTAL_ATTACHMENT_BYTES`].
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new() -> Self {
    Self {
      packet: PacketLimits::new(),
      max_attachment_bytes: DEFAULT_MAX_ATTACHMENT_BYTES,
      max_total_attachment_bytes: DEFAULT_MAX_TOTAL_ATTACHMENT_BYTES,
      max_codec_parameter_bytes: DEFAULT_MAX_CODEC_PARAMETER_BYTES,
      max_total_codec_parameter_bytes: DEFAULT_MAX_TOTAL_CODEC_PARAMETER_BYTES,
      max_probe_bytes: DEFAULT_MAX_PROBE_BYTES,
      max_streams: DEFAULT_MAX_STREAMS,
      max_chapters: DEFAULT_MAX_CHAPTERS,
      max_total_chapter_title_bytes: DEFAULT_MAX_TOTAL_CHAPTER_TITLE_BYTES,
      max_total_stream_metadata_bytes: DEFAULT_MAX_TOTAL_STREAM_METADATA_BYTES,
    }
  }

  /// The ceiling on bytes libavformat may read while probing and
  /// analysing a container.
  ///
  /// # What this bounds, and what it does not
  ///
  /// **Bounded:** the total bytes libavformat is handed during
  /// `avformat_open_input` and `avformat_find_stream_info`. It reaches
  /// two ways — as `probesize` and `formatprobesize`, which every
  /// entrypoint sets before the open, and, on the reader entrypoint, as
  /// a hard byte meter on the `AVIOContext` itself: past the budget the
  /// reader answers an I/O error, so the parser gets nothing more
  /// whatever it asks for.
  ///
  /// **Not bounded:** allocation *amplification* inside a parser. A
  /// container can describe, in a handful of bytes, a structure whose
  /// in-memory form is much larger, and nothing outside libavformat can
  /// see that happen. What this seat guarantees is that the input to
  /// that amplification is finite and small; bounding its output is the
  /// substrate's own hardening territory, and FFmpeg has its own
  /// `max_streams` / `max_index_size` / `max_picture_buffer` seats for
  /// exactly that — [`Self::max_streams`] sets the first of them.
  ///
  /// **Not bounded on the path entrypoint:** the byte meter needs an
  /// `AVIOContext` this crate owns, and a path is opened by
  /// libavformat's own protocol layer. `probesize` and
  /// `formatprobesize` still apply there; the hard meter does not.
  /// A caller who wants the meter on a file can open it as a reader.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn max_probe_bytes(&self) -> u64 {
    self.max_probe_bytes
  }
  /// The ceiling on streams a container may declare. See
  /// [`Self::max_probe_bytes`] for why a seat inside libavformat is
  /// worth setting at all.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn max_streams(&self) -> u32 {
    self.max_streams
  }
  /// Sets the probe-read ceiling (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_max_probe_bytes(mut self, value: u64) -> Self {
    self.max_probe_bytes = value;
    self
  }
  /// Sets the declared-stream ceiling (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_max_streams(mut self, value: u32) -> Self {
    self.max_streams = value;
    self
  }

  /// The ceiling on chapters a container may declare.
  ///
  /// Judged **before** the chapter table is reserved, so a header
  /// claiming an enormous table is refused rather than mirrored. Unlike
  /// [`Self::max_streams`], which is handed to libavformat and enforced
  /// inside it, this one is this crate's own: libavformat has no
  /// `max_chapters` knob, and an `AVChapter` is cheap enough there that
  /// the probe budget does not reach the count either.
  ///
  /// A file over the ceiling **fails to open**, with
  /// `mediadecode-ffmpeg`'s `TooManyChapters` naming the declared
  /// count.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn max_chapters(&self) -> u32 {
    self.max_chapters
  }
  /// The ceiling on bytes every chapter title in the file may hold
  /// together.
  ///
  /// Charged title by title as the table is mirrored, and the open
  /// fails with
  /// `mediadecode-ffmpeg`'s `ChapterTitleBudgetExhausted`
  /// at the title that crosses it. The charge is made **after** that
  /// one title is read rather than before, the same way
  /// [`Self::max_total_attachment_bytes`] is charged: what bounds the
  /// overshoot is the 64 KiB the metadata reader already refuses any
  /// single value past.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn max_total_chapter_title_bytes(&self) -> usize {
    self.max_total_chapter_title_bytes
  }
  /// Sets the declared-chapter ceiling (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_max_chapters(mut self, value: u32) -> Self {
    self.max_chapters = value;
    self
  }
  /// Sets the whole-file chapter-title budget (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_max_total_chapter_title_bytes(mut self, value: usize) -> Self {
    self.max_total_chapter_title_bytes = value;
    self
  }
  /// Sets the declared-chapter ceiling in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_max_chapters(&mut self, value: u32) -> &mut Self {
    self.max_chapters = value;
    self
  }
  /// Sets the whole-file chapter-title budget in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_max_total_chapter_title_bytes(&mut self, value: usize) -> &mut Self {
    self.max_total_chapter_title_bytes = value;
    self
  }

  /// The ceiling on bytes every stream's retained metadata may hold
  /// together — the `filename`, `mimetype` and `language` a track row
  /// keeps, across every admitted stream.
  ///
  /// Charged value by value as the track table is built, and the open
  /// fails with
  /// `mediadecode-ffmpeg`'s `TrackMetadataBudgetExhausted`
  /// at the value that crosses it — **before** that value is copied.
  /// The charge is the *decoded* size, because lossy decoding expands
  /// bytes that are not UTF-8 threefold, so the raw length would
  /// under-charge exactly the hostile case.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn max_total_stream_metadata_bytes(&self) -> usize {
    self.max_total_stream_metadata_bytes
  }
  /// Sets the whole-file stream-metadata budget (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_max_total_stream_metadata_bytes(mut self, value: usize) -> Self {
    self.max_total_stream_metadata_bytes = value;
    self
  }
  /// Sets the whole-file stream-metadata budget in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_max_total_stream_metadata_bytes(&mut self, value: usize) -> &mut Self {
    self.max_total_stream_metadata_bytes = value;
    self
  }

  /// The per-packet budget timed packets are checked against.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn packet(&self) -> PacketLimits {
    self.packet
  }
  /// Most bytes one attachment may carry.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn max_attachment_bytes(&self) -> usize {
    self.max_attachment_bytes
  }
  /// Most bytes every attachment in the file may carry together.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn max_total_attachment_bytes(&self) -> usize {
    self.max_total_attachment_bytes
  }
  /// Most heap bytes one stream's codec parameters may hold —
  /// `extradata`, `coded_side_data` and a custom channel map together.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn max_codec_parameter_bytes(&self) -> usize {
    self.max_codec_parameter_bytes
  }
  /// Most heap bytes every stream's codec parameters may hold together.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn max_total_codec_parameter_bytes(&self) -> usize {
    self.max_total_codec_parameter_bytes
  }

  /// Sets the per-packet budget (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_packet(mut self, value: PacketLimits) -> Self {
    self.packet = value;
    self
  }
  /// Sets the per-attachment ceiling (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_max_attachment_bytes(mut self, value: usize) -> Self {
    self.max_attachment_bytes = value;
    self
  }
  /// Sets the whole-file attachment budget (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_max_total_attachment_bytes(mut self, value: usize) -> Self {
    self.max_total_attachment_bytes = value;
    self
  }
  /// Sets the per-stream codec-parameter ceiling (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_max_codec_parameter_bytes(mut self, value: usize) -> Self {
    self.max_codec_parameter_bytes = value;
    self
  }
  /// Sets the whole-file codec-parameter budget (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_max_total_codec_parameter_bytes(mut self, value: usize) -> Self {
    self.max_total_codec_parameter_bytes = value;
    self
  }

  /// Sets the per-packet budget in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_packet(&mut self, value: PacketLimits) -> &mut Self {
    self.packet = value;
    self
  }
  /// Sets the per-attachment ceiling in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_max_attachment_bytes(&mut self, value: usize) -> &mut Self {
    self.max_attachment_bytes = value;
    self
  }
  /// Sets the whole-file attachment budget in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_max_total_attachment_bytes(&mut self, value: usize) -> &mut Self {
    self.max_total_attachment_bytes = value;
    self
  }
  /// Sets the per-stream codec-parameter ceiling in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_max_codec_parameter_bytes(&mut self, value: usize) -> &mut Self {
    self.max_codec_parameter_bytes = value;
    self
  }
  /// Sets the whole-file codec-parameter budget in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_max_total_codec_parameter_bytes(&mut self, value: usize) -> &mut Self {
    self.max_total_codec_parameter_bytes = value;
    self
  }
}

#[cfg(test)]
mod tests;
