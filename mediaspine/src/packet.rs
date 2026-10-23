//! Compressed `Packet` types and `PacketFlags`.
//!
//! The Packet types proper land in later tasks; this module starts
//! with `PacketFlags` so dependent types can use it.

use bitflags::bitflags;

bitflags! {
  /// Per-packet flags.
  ///
  /// Bit values are the public API:
  /// - `KEY = 0b001` — packet starts a keyframe (FFmpeg `AV_PKT_FLAG_KEY`,
  ///   WebCodecs `'key'`, ProRes RAW absence of
  ///   `kCMSampleAttachmentKey_NotSync`).
  /// - `CORRUPT = 0b010` — packet is known-corrupt (FFmpeg
  ///   `AV_PKT_FLAG_CORRUPT`).
  /// - `DISCARD = 0b100` — packet should be skipped during reconstruction
  ///   (FFmpeg `AV_PKT_FLAG_DISCARD`).
  /// - `DISPOSABLE = 0b1_0000` — nothing references this picture: the
  ///   container's mark or the bitstream's (FFmpeg
  ///   `AV_PKT_FLAG_DISPOSABLE`, and [`nal`](crate::nal)). Absent means
  ///   unknown, never "referenced".
  ///
  /// # Text form
  ///
  /// This type deliberately has **no** `Display` / `FromStr`, and its
  /// serde shape is the raw [`bits`](Self::bits) as a number. A
  /// vocabulary of *names* takes a text form; a bit *set* takes a
  /// number. A flag-set grammar (`"key|discard"`) would need two shapes
  /// rather than one, because a bit this build has no constant for can
  /// only be printed as a bare literal — and there are such bits today:
  /// FFmpeg carries `AV_PKT_FLAG_TRUSTED` (`0b0_1000`), which this set
  /// does not name. Human-readable names live in `Debug` and in whatever
  /// consumer surface wants them. This is `mediaframe::TrackDisposition`'s
  /// stance, for the same reason.
  #[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
  pub struct PacketFlags: u8 {
    /// Keyframe / sync sample.
    const KEY     = 0b001;
    /// Bitstream-level corruption known.
    const CORRUPT = 0b010;
    /// Demuxer hint: skip this packet.
    const DISCARD = 0b100;
    /// Nothing references this picture: the container's mark or the
    /// bitstream's. Dropping the packet loses its own picture and no
    /// other (FFmpeg `AV_PKT_FLAG_DISPOSABLE`).
    ///
    /// **The promise is pixels:** dropping the packet changes no later
    /// picture's pixels. It does not promise a later frame's best-effort
    /// timestamp, which a decoder may guess from every frame it output
    /// before — FFmpeg's does ([`nal`](crate::nal#what-the-mark-promises)),
    /// for a packet the container marks as for one the bitstream does. A
    /// frame's own pts, its packet's, is the exact one.
    ///
    /// **The container's mark.** In libavformat 9.0.1 one demuxer sets
    /// it: MP4/MOV, from `sdtp`, for a sample nothing depends on
    /// (`libavformat/mov.c` 11720–11723). Matroska's own discardable bit
    /// is NOT mapped (`libavformat/matroskadec.c` reads the block's
    /// keyframe bit alone, 4319–4320).
    ///
    /// **The bitstream's.** For H.264 and HEVC, a demuxer also marks
    /// every packet whose NAL units prove it — a non-reference picture,
    /// or an HEVC sub-layer non-reference picture at the highest temporal
    /// sub-layer, with nothing beside it a later picture needs
    /// ([`nal::NalStream`](crate::nal::NalStream)). **Where it reads a
    /// track, the bitstream's verdict decides**: the bit is set on a packet
    /// the bitstream proves and on no other, the container's mark cleared
    /// on every other verdict and on a corrupt packet, since a picture
    /// nothing references may still take up what an earlier packet left. A
    /// track it does not read carries the container's mark alone. No other
    /// codec's bitstream is read, and no encrypted payload: a demuxer reads
    /// no packet that carries encryption side data, and nothing of its
    /// track after it, nor any packet of a track its container names
    /// encrypted at the open.
    ///
    /// **Only an audited demuxer's tracks are read.** `mediadecode-ffmpeg`'s
    /// demuxer reads the bitstream of a demuxer on an allowlist alone, each
    /// row audited by FFmpeg 9.0.1's source for how an encrypted sample is
    /// signalled there: `mov` (MP4 and QuickTime), whose protected sample
    /// entry, `pssh` and per-sample encryption info it reads, and the raw
    /// `h264` and `hevc` elementary streams, plaintext by construction.
    /// Every other demuxer is container-only, and its packets are never
    /// marked from the bitstream: a recursive or proxy one (`concat`,
    /// `hls`, `dash`), Matroska and WebM, ASF, WTV, MXF, MPEG-TS, and every
    /// one not audited. FFmpeg sets the container's mark in `mov` alone, so
    /// a packet of theirs carries no mark at all.
    ///
    /// **A named limit:** a `mov` track's protected sample description that
    /// is not its first is outside the mark's guarantee. FFmpeg 9.0.1 reads
    /// a track's first sample description's protection alone, and passes a
    /// later description of another four-cc with no signal
    /// (`libavformat/mov.c` 3168–3192, 8684–8685).
    ///
    /// **Never with side data a decoder takes up past the packet** — a
    /// new configuration, a parameter change: dropping such a packet
    /// loses more than its picture, so a demuxer clears the bit there,
    /// whoever set it.
    ///
    /// The bit's ABSENCE means unknown, never "referenced": that is the
    /// rule a consumer must honour, because a container may never mark
    /// it, a codec's bitstream may not be read, and a packet without it
    /// may be disposable all the same.
    const DISPOSABLE = 0b1_0000;
  }
}

use crate::Timestamp;

/// A compressed video packet.
///
/// Generic over the [`VideoAdapter`](crate::adapter::VideoAdapter) (which contributes
/// `A::PacketExtra`) and the buffer type `B: AsRef<[u8]>`.
///
/// `pts` / `dts` / `duration` are `Option<Timestamp>` because not
/// every backend supplies all three (WebCodecs `EncodedVideoChunk`
/// has no DTS; vendor RAW SDKs that produce packets at all derive
/// timestamps from frame index × fps).
///
/// `Clone` and `Debug` derive directly: every field is either a
/// concrete `Copy` type or one of `E` / `D` themselves, so the
/// derive's per-parameter bound (`E: Clone, D: Clone`) is exactly
/// what cloning the fields requires — no associated-type indirection
/// to route around.
#[derive(Clone, Debug)]
pub struct VideoPacket<E, D> {
  pts: Option<Timestamp>,
  dts: Option<Timestamp>,
  duration: Option<Timestamp>,
  flags: PacketFlags,
  data: D,
  extra: E,
}

impl<E, D> VideoPacket<E, D> {
  /// Constructs a `VideoPacket` from `data` and `extra`. All
  /// timestamps default to `None` and flags to empty.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(data: D, extra: E) -> Self {
    Self {
      pts: None,
      dts: None,
      duration: None,
      flags: PacketFlags::empty(),
      data,
      extra,
    }
  }

  /// Returns the presentation timestamp.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn pts(&self) -> Option<Timestamp> {
    self.pts
  }
  /// Returns the decompression timestamp.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn dts(&self) -> Option<Timestamp> {
    self.dts
  }
  /// Returns the packet duration.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn duration(&self) -> Option<Timestamp> {
    self.duration
  }
  /// Returns the packet flags.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn flags(&self) -> PacketFlags {
    self.flags
  }
  /// Returns the compressed data buffer.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn data(&self) -> &D {
    &self.data
  }
  /// Returns the backend-specific extras.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn extra(&self) -> &E {
    &self.extra
  }
  /// Returns a mutable reference to the backend-specific extras.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn extra_mut(&mut self) -> &mut E {
    &mut self.extra
  }
  /// Consumes the packet and returns the buffer.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn into_data(self) -> D {
    self.data
  }
  /// Consumes the packet and returns `(buffer, extras)`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn into_parts(self) -> (D, E) {
    (self.data, self.extra)
  }

  /// Sets the PTS (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_pts(mut self, v: Option<Timestamp>) -> Self {
    self.pts = v;
    self
  }
  /// Sets the DTS (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_dts(mut self, v: Option<Timestamp>) -> Self {
    self.dts = v;
    self
  }
  /// Sets the duration (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_duration(mut self, v: Option<Timestamp>) -> Self {
    self.duration = v;
    self
  }
  /// Sets the flags (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_flags(mut self, v: PacketFlags) -> Self {
    self.flags = v;
    self
  }

  /// Sets the PTS in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_pts(&mut self, v: Option<Timestamp>) -> &mut Self {
    self.pts = v;
    self
  }
  /// Sets the DTS in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_dts(&mut self, v: Option<Timestamp>) -> &mut Self {
    self.dts = v;
    self
  }
  /// Sets the duration in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_duration(&mut self, v: Option<Timestamp>) -> &mut Self {
    self.duration = v;
    self
  }
  /// Sets the flags in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_flags(&mut self, v: PacketFlags) -> &mut Self {
    self.flags = v;
    self
  }
}

/// A compressed audio packet.
#[derive(Clone, Debug)]
pub struct AudioPacket<E, D> {
  pts: Option<Timestamp>,
  dts: Option<Timestamp>,
  duration: Option<Timestamp>,
  flags: PacketFlags,
  data: D,
  extra: E,
}

impl<E, D> AudioPacket<E, D> {
  /// Constructs an `AudioPacket` from `data` and `extra`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(data: D, extra: E) -> Self {
    Self {
      pts: None,
      dts: None,
      duration: None,
      flags: PacketFlags::empty(),
      data,
      extra,
    }
  }

  /// Returns the presentation timestamp.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn pts(&self) -> Option<Timestamp> {
    self.pts
  }
  /// Returns the decompression timestamp.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn dts(&self) -> Option<Timestamp> {
    self.dts
  }
  /// Returns the duration.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn duration(&self) -> Option<Timestamp> {
    self.duration
  }
  /// Returns the flags.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn flags(&self) -> PacketFlags {
    self.flags
  }
  /// Returns the compressed audio data.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn data(&self) -> &D {
    &self.data
  }
  /// Returns the backend extras.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn extra(&self) -> &E {
    &self.extra
  }
  /// Returns a mutable reference to the backend extras.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn extra_mut(&mut self) -> &mut E {
    &mut self.extra
  }
  /// Consumes the packet and returns the buffer.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn into_data(self) -> D {
    self.data
  }
  /// Consumes the packet and returns `(buffer, extras)`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn into_parts(self) -> (D, E) {
    (self.data, self.extra)
  }

  /// Sets the PTS (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_pts(mut self, v: Option<Timestamp>) -> Self {
    self.pts = v;
    self
  }
  /// Sets the DTS (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_dts(mut self, v: Option<Timestamp>) -> Self {
    self.dts = v;
    self
  }
  /// Sets the duration (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_duration(mut self, v: Option<Timestamp>) -> Self {
    self.duration = v;
    self
  }
  /// Sets the flags (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_flags(mut self, v: PacketFlags) -> Self {
    self.flags = v;
    self
  }

  /// Sets the PTS in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_pts(&mut self, v: Option<Timestamp>) -> &mut Self {
    self.pts = v;
    self
  }
  /// Sets the DTS in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_dts(&mut self, v: Option<Timestamp>) -> &mut Self {
    self.dts = v;
    self
  }
  /// Sets the duration in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_duration(&mut self, v: Option<Timestamp>) -> &mut Self {
    self.duration = v;
    self
  }
  /// Sets the flags in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_flags(&mut self, v: PacketFlags) -> &mut Self {
    self.flags = v;
    self
  }
}

/// A compressed subtitle packet.
#[derive(Clone, Debug)]
pub struct SubtitlePacket<E, D> {
  pts: Option<Timestamp>,
  duration: Option<Timestamp>,
  flags: PacketFlags,
  data: D,
  extra: E,
}

impl<E, D> SubtitlePacket<E, D> {
  /// Constructs a `SubtitlePacket` from `data` and `extra`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(data: D, extra: E) -> Self {
    Self {
      pts: None,
      duration: None,
      flags: PacketFlags::empty(),
      data,
      extra,
    }
  }

  /// Returns the presentation timestamp.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn pts(&self) -> Option<Timestamp> {
    self.pts
  }
  /// Returns the duration.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn duration(&self) -> Option<Timestamp> {
    self.duration
  }
  /// Returns the flags.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn flags(&self) -> PacketFlags {
    self.flags
  }
  /// Returns the compressed subtitle data.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn data(&self) -> &D {
    &self.data
  }
  /// Returns the backend extras.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn extra(&self) -> &E {
    &self.extra
  }
  /// Returns a mutable reference to the backend extras.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn extra_mut(&mut self) -> &mut E {
    &mut self.extra
  }
  /// Consumes the packet and returns the buffer.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn into_data(self) -> D {
    self.data
  }
  /// Consumes the packet and returns `(buffer, extras)`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn into_parts(self) -> (D, E) {
    (self.data, self.extra)
  }

  /// Sets the PTS (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_pts(mut self, v: Option<Timestamp>) -> Self {
    self.pts = v;
    self
  }
  /// Sets the duration (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_duration(mut self, v: Option<Timestamp>) -> Self {
    self.duration = v;
    self
  }
  /// Sets the flags (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_flags(mut self, v: PacketFlags) -> Self {
    self.flags = v;
    self
  }

  /// Sets the PTS in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_pts(&mut self, v: Option<Timestamp>) -> &mut Self {
    self.pts = v;
    self
  }
  /// Sets the duration in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_duration(&mut self, v: Option<Timestamp>) -> &mut Self {
    self.duration = v;
    self
  }
  /// Sets the flags in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_flags(&mut self, v: PacketFlags) -> &mut Self {
    self.flags = v;
    self
  }
}

// ---------------------------------------------------------------------------
//  Optional trait matrices (`serde` / `arbitrary` / `quickcheck`) for
//  `PacketFlags`. The packet types themselves are generic over a caller's
//  buffer and extras and are not a wire vocabulary; the flag set is.
// ---------------------------------------------------------------------------

#[cfg(feature = "serde")]
#[cfg_attr(docsrs, doc(cfg(feature = "serde")))]
mod serde_impls {
  //! The bit set travels as its number.
  //!
  //! This is the opposite of the choice `channel`'s two vocabularies
  //! make, and for the opposite reason. There, a `u32` wire would let an
  //! unrecognised code decode to `Unknown` — inventing a value — so the
  //! name is the only faithful shape. Here every bit pattern *is* a
  //! value, including the ones this build has no constant for
  //! (`AV_PKT_FLAG_TRUSTED` among them), so the number is
  //! the only shape that carries them all. `from_bits_retain` is what
  //! keeps that round trip lossless; `from_bits` would reject the very
  //! bits the wire exists to preserve.
  //!
  //! `mediaframe::TrackDisposition` sits on this same wire, so a
  //! consumer that stores both sees one convention.

  use serde::{Deserialize, Deserializer, Serialize, Serializer};

  use super::PacketFlags;

  impl Serialize for PacketFlags {
    #[cfg_attr(not(tarpaulin), inline(always))]
    fn serialize<S: Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
      ser.serialize_u8(self.bits())
    }
  }

  impl<'de> Deserialize<'de> for PacketFlags {
    #[cfg_attr(not(tarpaulin), inline(always))]
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
      u8::deserialize(de).map(Self::from_bits_retain)
    }
  }
}

#[cfg(feature = "arbitrary")]
#[cfg_attr(docsrs, doc(cfg(feature = "arbitrary")))]
mod arbitrary_impls {
  //! Uniform over `u8`, decoded with `from_bits_retain`.
  //!
  //! Again the opposite of `channel`'s roster draw, and again because a
  //! bit set has no fallback variant to collapse into: every one of the
  //! 256 patterns is a distinct value, each named bit is set in half of
  //! them, and the unnamed bits — the ones a real FFmpeg packet does
  //! carry — appear at the same rate. Choosing from a roster of the
  //! four named flags would generate exactly the inputs that cannot go
  //! wrong.

  use arbitrary::{Arbitrary, Result, Unstructured};

  use super::PacketFlags;

  impl<'a> Arbitrary<'a> for PacketFlags {
    fn arbitrary(u: &mut Unstructured<'a>) -> Result<Self> {
      Ok(Self::from_bits_retain(u8::arbitrary(u)?))
    }
  }
}

#[cfg(feature = "quickcheck")]
#[cfg_attr(docsrs, doc(cfg(feature = "quickcheck")))]
mod quickcheck_impls {
  //! The `quickcheck` half of what `arbitrary_impls` gives, drawn the
  //! same way and for the same reason.

  use quickcheck::{Arbitrary, Gen};

  use super::PacketFlags;

  impl Arbitrary for PacketFlags {
    fn arbitrary(g: &mut Gen) -> Self {
      Self::from_bits_retain(u8::arbitrary(g))
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  // `format!` needs an allocator, and this module carries no
  // file-level `extern crate alloc;` (production `packet.rs` is
  // core-only — its buffer type `B` is caller-supplied, never an
  // owned `Vec` this crate allocates). Scoped to the test module that
  // actually needs it; the enclosing `#[cfg(test)]` mod is not itself
  // feature-gated, so this import (and each of its three call sites,
  // below) carries its own gate — mirrors `serde_tests`' own
  // `extern crate alloc;` / `use alloc::string::ToString;` a little
  // further down, which needs the same bridge for the same reason.
  #[cfg(any(feature = "std", feature = "alloc"))]
  extern crate alloc;
  #[cfg(any(feature = "std", feature = "alloc"))]
  use alloc::format;

  #[test]
  fn flag_bits_are_stable() {
    assert_eq!(PacketFlags::KEY.bits(), 0b001);
    assert_eq!(PacketFlags::CORRUPT.bits(), 0b010);
    assert_eq!(PacketFlags::DISCARD.bits(), 0b100);
    assert_eq!(PacketFlags::DISPOSABLE.bits(), 0b1_0000);
    // The named census: TRUSTED (0b0_1000) and the top three bits stay
    // unnamed, carried by `from_bits_retain`.
    assert_eq!(PacketFlags::all().bits(), 0b1_0111);
  }

  #[test]
  fn flags_combine() {
    let f = PacketFlags::KEY | PacketFlags::CORRUPT;
    assert!(f.contains(PacketFlags::KEY));
    assert!(f.contains(PacketFlags::CORRUPT));
    assert!(!f.contains(PacketFlags::DISCARD));
  }

  #[test]
  fn empty_default() {
    assert_eq!(PacketFlags::default(), PacketFlags::empty());
  }

  use crate::Timebase;
  use core::num::NonZeroI32;

  fn ms_tb() -> Timebase {
    Timebase::new(1, NonZeroI32::new(1000).unwrap())
  }

  #[test]
  fn video_packet_construct_and_access() {
    let data: &[u8] = &[1, 2, 3];
    let p: VideoPacket<_, &[u8]> = VideoPacket::new(data, ());
    assert_eq!(p.pts(), None);
    assert_eq!(p.flags(), PacketFlags::empty());
    assert_eq!(*p.data(), data);
  }

  #[test]
  fn video_packet_builders_chain() {
    let pts = crate::Timestamp::new(1500, ms_tb());
    let p: VideoPacket<_, &[u8]> = VideoPacket::new(&[][..], ())
      .with_pts(Some(pts))
      .with_flags(PacketFlags::KEY);
    assert_eq!(p.pts(), Some(pts));
    assert!(p.flags().contains(PacketFlags::KEY));
  }

  #[test]
  fn video_packet_into_parts() {
    let p: VideoPacket<_, &[u8]> = VideoPacket::new(&[1u8, 2][..], ());
    let (data, _extra) = p.into_parts();
    assert_eq!(data, &[1, 2]);
  }

  // `format!` needs an allocator; see the import note above.
  #[cfg(any(feature = "std", feature = "alloc"))]
  #[test]
  fn video_packet_clone_matches_the_original() {
    let pts = crate::Timestamp::new(1500, ms_tb());
    let original: VideoPacket<_, &[u8]> = VideoPacket::new(&[1u8, 2, 3][..], ())
      .with_pts(Some(pts))
      .with_dts(Some(pts))
      .with_flags(PacketFlags::KEY);
    let cloned = original.clone();
    assert_eq!(cloned.pts(), original.pts());
    assert_eq!(cloned.dts(), original.dts());
    assert_eq!(cloned.duration(), original.duration());
    assert_eq!(cloned.flags(), original.flags());
    assert_eq!(cloned.data(), original.data());
    assert!(format!("{cloned:?}").contains("VideoPacket"));
  }

  #[test]
  fn audio_packet_round_trip() {
    let data: &[u8] = &[7, 8, 9];
    let p: AudioPacket<_, &[u8]> = AudioPacket::new(data, ()).with_flags(PacketFlags::KEY);
    assert_eq!(*p.data(), data);
    assert!(p.flags().contains(PacketFlags::KEY));
    let (recovered, _) = p.into_parts();
    assert_eq!(recovered, data);
  }

  // `format!` needs an allocator; see the import note above.
  #[cfg(any(feature = "std", feature = "alloc"))]
  #[test]
  fn audio_packet_clone_matches_the_original() {
    let data: &[u8] = &[7, 8, 9];
    let original: AudioPacket<_, &[u8]> = AudioPacket::new(data, ()).with_flags(PacketFlags::KEY);
    let cloned = original.clone();
    assert_eq!(cloned.flags(), original.flags());
    assert_eq!(cloned.data(), original.data());
    assert!(format!("{cloned:?}").contains("AudioPacket"));
  }

  #[test]
  fn subtitle_packet_round_trip() {
    let data: &[u8] = b"hi";
    let p: SubtitlePacket<_, &[u8]> = SubtitlePacket::new(data, ());
    assert_eq!(*p.data(), data);
  }

  // `format!` needs an allocator; see the import note above.
  #[cfg(any(feature = "std", feature = "alloc"))]
  #[test]
  fn subtitle_packet_clone_matches_the_original() {
    let data: &[u8] = b"hi";
    let original: SubtitlePacket<_, &[u8]> = SubtitlePacket::new(data, ());
    let cloned = original.clone();
    assert_eq!(cloned.data(), original.data());
    assert!(format!("{cloned:?}").contains("SubtitlePacket"));
  }

  // -------------------------------------------------------------------
  //  Optional matrices (`serde` / `arbitrary` / `quickcheck`)
  // -------------------------------------------------------------------

  // The wire assertions need a real self-describing format, which needs
  // an allocator; the impls themselves compile at every tier.
  #[cfg(all(feature = "serde", any(feature = "alloc", feature = "std")))]
  mod serde_tests {
    // The crate root links `alloc` under the name `std` (the workspace's
    // alloc-as-std alias), so `alloc` has to be named here to reach
    // `ToString` — which the std prelude would otherwise have supplied.
    // Both this and the `use` are valid under `std` too, hence no `cfg`:
    // the enclosing module is already gated on `alloc` or `std`.
    extern crate alloc;

    use alloc::string::ToString;

    use super::*;

    #[test]
    fn the_wire_is_a_number_not_a_flag_grammar() {
      // The ruling this pins: a bit set reaches the wire as its bits.
      // `bitflags`' own serde would have written `"KEY | CORRUPT"` here
      // for a human-readable format, which is why that sub-feature is
      // not the mechanism.
      let flags = PacketFlags::KEY | PacketFlags::CORRUPT;
      assert_eq!(
        serde_json::to_string(&flags).expect("flags always serialize"),
        "3"
      );
      assert_eq!(
        serde_json::to_string(&PacketFlags::empty()).expect("flags always serialize"),
        "0"
      );
    }

    #[test]
    fn the_disposable_bit_is_ffmpegs_number_and_its_absence_reads_unknown() {
      // Naming the bit moved no number: `0b1_0000` is what the wire
      // already carried for an FFmpeg packet marked
      // `AV_PKT_FLAG_DISPOSABLE`, and now it reads back as the name.
      let disposable = PacketFlags::KEY | PacketFlags::DISPOSABLE;
      assert_eq!(
        serde_json::to_string(&disposable).expect("flags always serialize"),
        "17"
      );
      let read: PacketFlags = serde_json::from_str("16").expect("a number parses");
      assert!(read.contains(PacketFlags::DISPOSABLE));
      // A packet without the hint does not carry it; that says the demuxer
      // stated nothing, not that the packet is referenced.
      let unmarked: PacketFlags = serde_json::from_str("1").expect("a number parses");
      assert!(!unmarked.contains(PacketFlags::DISPOSABLE));
    }

    #[test]
    fn a_name_is_not_a_number() {
      assert!(serde_json::from_str::<PacketFlags>(r#""KEY""#).is_err());
      assert!(serde_json::from_str::<PacketFlags>(r#""key|corrupt""#).is_err());
    }

    #[test]
    fn every_bit_pattern_round_trips_including_the_unnamed_ones() {
      // 0b0000_1000 is FFmpeg's TRUSTED, which this set does not name.
      // It still has to survive, which is what `from_bits_retain` buys
      // and what `from_bits` would lose.
      for bits in 0..=u8::MAX {
        let flags = PacketFlags::from_bits_retain(bits);
        let json = serde_json::to_string(&flags).expect("flags always serialize");
        assert_eq!(json, bits.to_string());
        assert_eq!(
          serde_json::from_str::<PacketFlags>(&json).expect("its own output parses"),
          flags,
          "round-trip failed for {bits:#010b}"
        );
      }
    }

    #[test]
    fn a_value_no_u8_can_hold_is_refused() {
      assert!(serde_json::from_str::<PacketFlags>("256").is_err());
      assert!(serde_json::from_str::<PacketFlags>("-1").is_err());
    }
  }

  // -------------------------------------------------------------------
  //  `serde` feature forwarding pin (`mediaframe/serde`)
  // -------------------------------------------------------------------

  // `PixelFormat` and `BayerPattern` carry no `Serialize` / `Deserialize`
  // impl in this crate — they are mediaframe vocabulary types the facade
  // only re-exports. They compile here solely because mediadecode's
  // `serde` feature also turns on `mediaframe/serde`, which is where
  // mediaframe writes those impls. If that forwarding entry in
  // `Cargo.toml` is ever dropped, this module stops compiling under
  // `--features serde` instead of silently losing coverage.
  #[cfg(all(feature = "serde", any(feature = "alloc", feature = "std")))]
  mod mediaframe_serde_forwarding_tests {
    #[test]
    fn pixel_format_round_trips_through_json() {
      let value = crate::PixelFormat::Yuv420p;
      let json = serde_json::to_string(&value).expect("mediaframe/serde forwards PixelFormat");
      assert_eq!(json, "\"yuv420p\"");
      assert_eq!(
        serde_json::from_str::<crate::PixelFormat>(&json).expect("its own output parses"),
        value
      );
    }

    #[test]
    fn bayer_pattern_round_trips_through_json() {
      let value = crate::cfa::BayerPattern::Rggb;
      let json = serde_json::to_string(&value).expect("mediaframe/serde forwards BayerPattern");
      assert_eq!(json, "\"rggb\"");
      assert_eq!(
        serde_json::from_str::<crate::cfa::BayerPattern>(&json).expect("its own output parses"),
        value
      );
    }
  }

  #[cfg(feature = "arbitrary")]
  mod arbitrary_tests {
    use arbitrary::{Arbitrary, Unstructured};

    use super::*;

    #[test]
    fn every_bit_pattern_is_reachable() {
      let mut seen = [false; 256];
      for byte in 0..=u8::MAX {
        let data = [byte];
        let mut u = Unstructured::new(&data);
        let flags = PacketFlags::arbitrary(&mut u).expect("the generator is total");
        seen[flags.bits() as usize] = true;
      }
      assert!(
        seen.iter().all(|&s| s),
        "a bit pattern the generator never produces"
      );
    }
  }

  #[cfg(feature = "quickcheck")]
  mod quickcheck_tests {
    use quickcheck::{Arbitrary, Gen};

    use super::*;

    #[test]
    fn the_named_flags_and_the_unnamed_bits_are_both_reachable() {
      // 4000 draws over 256 patterns: missing a named flag here means
      // the generator is skewed, not unlucky.
      let mut g = Gen::new(16);
      let mut union = PacketFlags::empty();
      let mut saw_unnamed = false;
      for _ in 0..4000 {
        let flags = PacketFlags::arbitrary(&mut g);
        union |= flags;
        saw_unnamed |= !PacketFlags::all().contains(flags);
      }
      assert_eq!(
        union,
        PacketFlags::from_bits_retain(u8::MAX),
        "a bit the generator never sets"
      );
      assert!(saw_unnamed, "an unnamed bit is never produced");
    }
  }
}
