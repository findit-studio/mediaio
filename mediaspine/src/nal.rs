//! What a packet's H.264 or HEVC bitstream says about its picture: whether
//! anything references it, so that dropping the packet loses that picture
//! and no other.
//!
//! [`NalStream::read`] reads one packet and answers [`Verdict::Disposable`]
//! only where the bitstream itself proves that nothing references the
//! packet's picture and that nothing else the packet carries is needed
//! later. Anything it cannot prove keeps the packet ([`Verdict::Kept`]),
//! with the reason. A demuxer ORs the verdict into
//! [`PacketFlags::DISPOSABLE`](crate::packet::PacketFlags::DISPOSABLE)
//! beside the container's own mark, so the fact is the same whichever
//! container carried the stream.
//!
//! It reads NAL unit headers, the first bytes of an HEVC parameter set, the
//! message headers of an SEI unit, and the bytes of each NAL unit of a
//! length-framed packet, for a start code. It allocates nothing.
//!
//! # The decoder
//!
//! The question is FFmpeg's, asked of its own H.264 and HEVC decoders as
//! libavcodec opens them: fed a track's packets in decoding order — every
//! packet the reading reads but those it marks — as a demuxer delivers
//! them, whole access units and never in chunks (`AV_CODEC_FLAG2_CHUNKS`),
//! with the H.264 decoder's private `is_avc` and `nal_length_size` options
//! left to the configuration record (`libavcodec/h264dec.c` 1093–1094). A
//! decoder opened afresh at a seek is answered for too
//! ([`NalStream::seeked`]). A decoder's frame threads hand its framing on
//! from packet to packet (`libavcodec/h264_slice.c` 419–421,
//! `libavcodec/hevc/hevcdec.c` 4051–4052), so a threaded decoder reads as
//! one thread does.
//!
//! # H.264
//!
//! A non-reference picture — one coded with `nal_ref_idc` equal to 0 — "is
//! not used for inter prediction of any other pictures" (H.264, 3.104), and
//! a slice or slice data partition of `nal_ref_idc` 0 is part of one
//! (7.4.1). An IDR slice's `nal_ref_idc` is never 0, nor a parameter set's
//! (7.4.1). So a packet is disposable iff it holds at least one coded slice
//! (NAL unit types 1–5, Table 7-1), every slice has `nal_ref_idc` equal to
//! 0, and none is an IDR slice (type 5). Every slice is checked, so a
//! packet that carries a field pair is disposable only where both fields
//! are.
//!
//! **A field's pair may be in the next packet.** FFmpeg's decoder joins
//! the two fields of a frame across packets, holding the first, its frame
//! and its SEI until the second arrives (`h264_slice.c` 1397–1500), so
//! dropping one field of a pair costs the other its frame. A packet is
//! therefore disposable only where every sequence parameter set the
//! decoder may read codes frames only (`frame_mbs_only_flag` 1,
//! 7.4.2.1.1): before one has been read, and from the first that lets a
//! picture be a field for good, nothing is
//! ([`KeptReason::MaybeField`]). Proving each slice a frame from its own
//! header, which would mark interlaced streams too, is not done.
//!
//! Beside the slices, only access unit delimiters (9), filler data
//! (12) and an SEI unit whose every message its own picture alone takes up
//! ([SEI](#sei)) may ride. A parameter set (7, 8, 13, 15), any other SEI
//! unit, an end of sequence or of stream (10, 11), and any type this module
//! does not read keep the packet, because a later picture, or what is
//! output after it, may need them. A NAL unit of the scalable, multiview or
//! 3-D extensions (14, 20, 21) keeps the packet too, because a multiview
//! view component of `nal_ref_idc` 0 whose `inter_view_flag` is 1 "may be
//! used for inter-view prediction by other view components in the current
//! access unit" (G.7.4.1.1). This decides for the whole packet, the unit a
//! consumer drops.
//!
//! # HEVC
//!
//! A sub-layer non-reference (SLNR) picture — NAL unit types TRAIL_N, TSA_N,
//! STSA_N, RADL_N and RASL_N, which FFmpeg's decoder decodes (H.265,
//! 7.4.2.2; `hevc/hevcdec.c` 3587–3680) — holds samples no later picture
//! **of the same sub-layer** uses, but a
//! picture of a higher sub-layer may (3.162 and its note). A picture never
//! references one of a higher sub-layer, nor an SLNR picture of its own
//! (8.3.2). So an SLNR picture is disposable at the **highest** temporal
//! sub-layer the stream declares, and only there: every slice an SLNR type
//! of layer 0 at a TemporalId equal to that highest sub-layer.
//!
//! The highest sub-layer is the most any parameter set declares:
//! `sps_max_sub_layers_minus1` (7.4.3.2.1), `vps_max_sub_layers_minus1`,
//! which bounds it, and a configuration record's `numTemporalLayers` less
//! one where it is not 0. It only ever rises, so a reading that is off can
//! only fail to mark ([what the reading learns](#what-the-reading-learns)).
//! Nothing is disposable before a sequence parameter set has been seen, in
//! the configuration record or in the stream.
//!
//! Beside the slices, only access unit delimiters (35), filler data (38)
//! and an SEI unit whose every message its own picture alone takes up
//! ([SEI](#sei)) may ride. A parameter set (32–34), any other prefix or
//! suffix SEI unit (39, 40), an end of sequence or of bitstream (36, 37), a
//! reserved or unspecified type, and any NAL unit of a layer above 0 keep
//! the packet. An IRAP picture is never disposable, and nor is one of the
//! reserved SLNR types RSV_VCL_N10, N12 and N14, which FFmpeg's decoder
//! skips without a picture, so that what rides beside one is carried over
//! to the next picture.
//!
//! **FFmpeg's own `AVDISCARD_NONREF` has no temporal guard**: it skips
//! every SLNR NAL unit at every sub-layer (`libavcodec/hevc/hevcdec.c`
//! 3766–3767, the eight types of `ff_hevc_nal_is_nonref`,
//! `libavcodec/hevc/hevcdec.h` 653–667). On a temporally scalable stream
//! it can drop a picture a higher sub-layer references; this module does
//! not.
//!
//! # SEI
//!
//! An SEI unit rides beside a disposable picture only where every message
//! in it is one FFmpeg's decoder takes up for that picture alone, leaving
//! nothing behind for a later one. The unit is read as FFmpeg reads it
//! (`libavcodec/h264_sei.c` 229–300, `libavcodec/hevc/sei.c` 264–317): the
//! bytes after its header, emulation prevention bytes dropped, are
//! messages, each a `payloadType` and a `payloadSize` coded in runs of
//! `0xFF` and then that many bytes, up to the unit's `rbsp_trailing_bits`,
//! a last byte of `0x80`. A unit that does not read so keeps the packet.
//!
//! | codec | `payloadType` (`libavcodec/sei.h`) | in a packet | why |
//! |---|---|---|---|
//! | H.264 | 1, picture timing | rides beside | FFmpeg clears it before every access unit (`h264dec.c` 593–598, `h264_sei.c` 48–61) and applies it to its own picture (`h264_slice.c` 1185–1235) |
//! | H.264, HEVC | 3, filler payload | rides beside | no decoder reads it: it falls to `ff_h2645_sei_message_decode`, which has no case for it (`h2645_sei.c` 286–317; `h264_sei.c` 282–286, `hevc/sei.c` 242–248, 251–262) |
//! | HEVC | 132, decoded picture hash | keeps the packet | FFmpeg clears it only when a frame ends (`hevc/hevcdec.c` 3505–3512), and its decoder skips decodable pictures too — every SLNR picture under `skip_frame = AVDISCARD_NONREF` (3766–3767), a RASL picture after the CRA it starts at — as well as the reserved types: beside a picture skipped so, the hash carries over to the next picture's check |
//!
//! Every other message keeps the packet: a persistent one — film grain,
//! tone mapping, mastering display, a recovery point, user data whose
//! content FFmpeg keeps (an x264 version, `h2645_sei.c` 92–99) — and every
//! type this module does not read. HEVC's picture timing is among them:
//! FFmpeg keeps its field structure for every later picture that carries
//! none (`hevc/sei.c` 52–75, `hevc/refs.c` 183–187), clearing it only at
//! the decoder's opening (`hevc/hevcdec.c` 4160). So is HEVC's decoded
//! picture hash, as the table says. FFmpeg's own
//! `skip_frame = AVDISCARD_NONREF` never skips an H.264 SEI unit, for the
//! reason this table exists (`h264dec.c` 626–628).
//!
//! # Framing
//!
//! A track's packets carry their NAL units either each after a big-endian
//! length or each after a start code (Annex B). The framing is FFmpeg's
//! decoder's own state, two fields kept apart: whether lengths are read
//! (`is_avc` for H.264, `is_nalff` for HEVC) and the length's size
//! (`nal_length_size`), 0 until a record sets it. [`NalStream`] holds both
//! and moves them where FFmpeg does:
//!
//! | event | H.264 (`h264_parse.c`, `h264dec.c`) | HEVC (`hevc/parse.c`, `hevc/hevcdec.c`) |
//! |---|---|---|
//! | no record, or an empty one | nothing moves (`h264_parse.c` 472–473, `h264dec.c` 399) | nothing moves (`hevcdec.c` 3856, 4168) |
//! | a record whose first byte is 1 (H.264), or of 23 bytes or more whose first is 1, or 0 with a nonzero second or a third above 1 (HEVC) | lengths read, before anything is checked (`h264_parse.c` 475–479) | lengths read (`parse.c` 93–97), of two bytes while the record's arrays are read (103–105) |
//! | that record read whole | the size is its `lengthSizeMinusOne` plus one (515–516) | the same (134–136) |
//! | that record failing part way: too short, or a count past its end | the size stays (481–514) | the size stays two (112–119) |
//! | any other record | start codes read, the size kept (517–522) | start codes read, the size kept (137–143) |
//! | a record on a packet of no bytes | not read: the decoder takes the packet for the stream's end (`h264dec.c` 1034–1036) | read (`hevcdec.c` 3854–3860) |
//! | a packet that is an `avcC` itself, while lengths are read | read as a record (`h264dec.c` 1045–1050) | — |
//! | a packet while the size is four | start codes for one that begins with a four-byte start code whose next four bytes are a length past it, lengths for one whose first four bytes are a length that fits (`h264dec.c` 602–607) | — |
//!
//! A size of 0 while lengths are read frames nothing FFmpeg splits
//! (`libavcodec/h2645_parse.h` 130–148), and every packet is kept
//! ([`KeptReason::Unframed`]). A size of three is read as FFmpeg reads it,
//! though ISO/IEC 14496-15 allows only 1, 2 and 4. A packet that is an
//! `avcC` is kept ([`KeptReason::Configuration`]), and so is one that
//! switches the framing ([`KeptReason::FramingChange`]), so that a decoder
//! which drops every packet marked disposable still sees each packet that
//! moves its framing.
//!
//! **Where the record's reading turns on its parameter sets.** FFmpeg
//! decodes a record's parameter sets as well; this module does not. Where
//! whether one decodes moves the size, the size is unknown, and no packet
//! is marked ([`KeptReason::FramingUnknown`]):
//!
//! - H.264 retries a parameter set that does not decode, escaped, and
//!   refuses there one of 21,802 bytes or more (`h264_parse.c` 436–437),
//!   ending the record: an `avcC` holding one moves the size only if it
//!   decodes. The size is known again when a record sets it.
//! - Under `AV_EF_EXPLODE`, an HEVC record a parameter set of which does
//!   not decode stops with a size of two (`hevc/parse.c` 72–76, 121–129).
//!   The reading cannot know the decoder's `err_recognition`, so after a
//!   mid-stream `hvcC` of any other size nothing is marked again. At the
//!   decoder's opening the same record leaves no decoder at all
//!   (`hevcdec.c` 4169–4172).
//!
//! Otherwise a record's reading is FFmpeg's, an allocation that fails
//! aside: H.264 swallows any other parameter set that does not decode
//! (`h264_parse.c` 421–464), and HEVC every one without `AV_EF_EXPLODE`
//! (`hevc/parse.c` 72–76).
//!
//! **Seeks.** A seek leaves the framing a decoder reads the next packets in
//! unknown wherever the framing differs from the one the stream opened in:
//! a decoder keeps its own across a seek, and one opened afresh starts from
//! the stream's. After such a seek nothing is marked again
//! ([`NalStream::seeked`]).
//!
//! # The NAL unit types
//!
//! | codec | `nal_unit_type` | in a packet | why (clause) |
//! |---|---|---|---|
//! | H.264 | 1–4, `nal_ref_idc` 0, every SPS coding frames only | a picture nothing references | 3.104, 7.4.1, 7.4.2.1.1 |
//! | H.264 | 1–4, `nal_ref_idc` 0, before any SPS or after one that lets fields in | kept: [`KeptReason::MaybeField`] | 7.4.2.1.1; `h264_slice.c` 1397–1500 |
//! | H.264 | 1–4, `nal_ref_idc` above 0 | kept: [`KeptReason::Referenced`] | 7.4.1 |
//! | H.264 | 5, IDR | kept: [`KeptReason::RandomAccess`] | 7.4.1 |
//! | H.264 | 6, SEI of picture timing and filler payload alone | rides beside | [SEI](#sei) |
//! | H.264 | 6, any other SEI | kept: [`KeptReason::Companion`] | Annex D; [SEI](#sei) |
//! | H.264 | 9, 12: AUD, filler | rides beside | Table 7-1 |
//! | H.264 | 7, 8, 13, 15: parameter sets | kept: [`KeptReason::Companion`] | 7.4.1 |
//! | H.264 | 10, 11: end of sequence, of stream | kept: [`KeptReason::Companion`] | Table 7-1 |
//! | H.264 | 14, 20, 21: extensions | kept: [`KeptReason::Extension`] | G.7.4.1.1 |
//! | H.264 | 0, 16–19, 22–31: auxiliary, reserved, unspecified | kept: [`KeptReason::Companion`] | Table 7-1 |
//! | HEVC | 0, 2, 4, 6, 8: SLNR, at the highest sub-layer | a picture nothing references | 3.162, 7.4.2.2, 8.3.2 |
//! | HEVC | 10, 12, 14: reserved SLNR, which FFmpeg skips | kept: [`KeptReason::Companion`] | 7.4.2.2; `hevc/hevcdec.c` 3587–3680 |
//! | HEVC | the same, below the highest sub-layer | kept: [`KeptReason::BelowHighestSubLayer`] | 3.162, note |
//! | HEVC | the same, before any SPS | kept: [`KeptReason::SubLayersUnknown`] | 7.4.3.2.1 |
//! | HEVC | 1, 3, 5, 7, 9, 11, 13, 15: sub-layer reference | kept: [`KeptReason::Referenced`] | 7.4.2.2 |
//! | HEVC | 16–23: IRAP | kept: [`KeptReason::RandomAccess`] | 7.4.2.2 |
//! | HEVC | 32–34: parameter sets | kept: [`KeptReason::Companion`] | Table 7-1 |
//! | HEVC | 35, 38: AUD, filler | rides beside | Table 7-1 |
//! | HEVC | 36, 37: end of sequence, of bitstream | kept: [`KeptReason::Companion`] | Table 7-1 |
//! | HEVC | 39, 40: SEI of filler payload alone | rides beside | [SEI](#sei) |
//! | HEVC | 39, 40: any other SEI | kept: [`KeptReason::Companion`] | Annex D; [SEI](#sei) |
//! | HEVC | 24–31, 41–63: reserved, unspecified | kept: [`KeptReason::Companion`] | Table 7-1 |
//! | HEVC | any type of a layer above 0 | kept: [`KeptReason::Extension`] | Annex F |
//!
//! # What the reading learns
//!
//! An HEVC picture is disposable only at the highest sub-layer, and an
//! H.264 one only where every sequence parameter set codes frames only, so
//! the reading must have read every parameter set the decoder may have. It
//! reads more than FFmpeg's splitter does, never less: every NAL unit of a
//! packet the framing frames, a malformed packet's too; in Annex B, the
//! bytes after every start code, past bytes that frame nothing, where
//! FFmpeg's splitter takes up the stream again (`h2645_parse.c` 559–584);
//! and within a length-framed NAL unit, the bytes after each start code in
//! it, which the splitter reads as units of their own (`h2645_parse.c`
//! 44–52, 559–584). A packet the reading cannot frame, which a decoder
//! still reads in a framing of its own, is read in every framing one may
//! read it in: Annex B, and lengths of one to four bytes. A unit of a
//! sub-layer no parameter set has declared raises the highest sub-layer to
//! its own. An H.264 sequence parameter set is read to its
//! `frame_mbs_only_flag` in each of the three readings FFmpeg's decoder may
//! adopt of one: as its RBSP, as its bytes, and as the bytes FFmpeg writes
//! when it escapes an `avcC` entry to read it again. One that any reading
//! does not prove to code frames only lets fields in. All of it only raises
//! what later packets are held to, so reading more can only fail to mark.
//!
//! # What a decoder keeps from one packet to the next
//!
//! A packet is disposable only where dropping it changes no other
//! picture's decoding, so every state FFmpeg's decoders carry from one
//! packet to a later one is accounted for:
//!
//! | state | carried by | here |
//! |---|---|---|
//! | the framing, `is_avc` / `is_nalff` and `nal_length_size` | a record, an H.264 `avcC` packet, the four-byte probe (`h264dec.c` 602–607, 1038–1050; `hevc/hevcdec.c` 3854–3860) | a packet that moves it is kept ([Framing](#framing)) |
//! | parameter sets | VPS, SPS, PPS (both codecs) | kept ([`KeptReason::Companion`]) |
//! | a frame awaiting its second field | an H.264 field (`h264_slice.c` 1397–1500) | nothing marked unless every SPS codes frames only ([`KeptReason::MaybeField`]) |
//! | references, and the reference marking | slices of `nal_ref_idc` above 0, IDR; HEVC reference and IRAP pictures, the RPS of later pictures | kept ([`KeptReason::Referenced`], [`KeptReason::RandomAccess`]); an SLNR picture below the highest sub-layer ([`KeptReason::BelowHighestSubLayer`]) |
//! | picture order counts | H.264's previous reference picture and frame-number offset (8.2.1); HEVC's previous TemporalId-0 picture that is not RASL, RADL or SLNR (8.3.1) | none from a picture marked here: H.264's offset reads alike with a non-reference picture dropped, and HEVC never counts an SLNR picture |
//! | SEI state | H.264 clears it before each access unit (`h264dec.c` 593–598); HEVC keeps it (`hevc/sei.c`, `hevc/refs.c` 183–187), but clears the picture hash at a frame's end (`hevc/hevcdec.c` 3505–3512) | only what no decoder carries rides ([SEI](#sei)) |
//! | an x264 version, a recovery point, a Dolby Vision RPU, an end of sequence | user data unregistered SEI (`h2645_sei.c` 92–99), recovery point SEI, HEVC unspecified type 62, EOS / EOB | kept ([`KeptReason::Companion`]) |
//! | the output order and its timing | the decoded picture buffer and its reorder heuristics | a dropped picture leaves the buffer as FFmpeg's own `AVDISCARD_NONREF` leaves it: no other picture's samples change |
//! | the next frame's interlaced flag, where its `pic_struct` is a field order on a frame | H.264's previous frame (`h264_slice.c` 1196–1235) | as under `AVDISCARD_NONREF`: a flag of the output, no samples |
//! | a hardware probe awaiting its first picture | `mediadecode-ffmpeg`'s unpinned decoders | not a bitstream's: under a skip policy that may leave a stream with no picture (`nonintra`, `nonkey`, `all`), an end with none is the stream's, and `all` runs no probe |
//!
//! # What is never marked
//!
//! A codec other than H.264 and HEVC is never read: its packets carry the
//! container's mark alone. A packet that does not parse is kept as
//! [`KeptReason::Malformed`]: a NAL unit whose forbidden bit is set, a
//! length of 0 or one past the packet's end (which FFmpeg refuses too,
//! `h2645_parse.h` 130–148), a length-framed NAL unit holding a start code,
//! bytes that form no NAL unit, an HEVC TemporalId of −1 or past the highest
//! sub-layer, a parameter set too short to read. A packet this module
//! cannot read is one it cannot prove disposable.
//!
//! The citations are FFmpeg 9.0.1's sources, and Rec. ITU-T H.264 (V16,
//! 06/2026) and H.265 (V11, 01/2026) by clause.

use core::fmt;

/// The codecs whose bitstream [`NalStream`] reads.
///
/// Every other codec is unclassified: whether its packets are disposable
/// is the container's to say, and a demuxer marks them from the container
/// alone.
///
/// Marked `#[non_exhaustive]`: a later codec joins as a variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum NalCodec {
  /// H.264 / AVC.
  H264,
  /// H.265 / HEVC.
  Hevc,
}

/// How a track's packets carry their NAL units.
///
/// Marked `#[non_exhaustive]`: a later framing joins as a variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Framing {
  /// Each NAL unit after a big-endian length of this many bytes, as an
  /// `avcC` or `hvcC` configuration record declares.
  Length(LengthSize),
  /// Each NAL unit after a start code, `00 00 01` (Annex B).
  AnnexB,
}

/// The size of the length before each NAL unit: a configuration record's
/// `lengthSizeMinusOne` plus one. ISO/IEC 14496-15 allows one, two and
/// four bytes; FFmpeg reads three as it reads the others
/// (`libavcodec/h2645_parse.h` 130–148), and so does this module.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LengthSize {
  /// One byte.
  One,
  /// Two bytes.
  Two,
  /// Three bytes.
  Three,
  /// Four bytes.
  Four,
}

impl LengthSize {
  /// The size, in bytes.
  pub const fn bytes(self) -> usize {
    match self {
      Self::One => 1,
      Self::Two => 2,
      Self::Three => 3,
      Self::Four => 4,
    }
  }

  /// From FFmpeg's `nal_length_size`; `None` for 0, a size no record has
  /// set, and for anything past four.
  const fn from_bytes(bytes: u8) -> Option<Self> {
    match bytes {
      1 => Some(Self::One),
      2 => Some(Self::Two),
      3 => Some(Self::Three),
      4 => Some(Self::Four),
      _ => None,
    }
  }
}

/// What [`NalStream::read`] answers for one packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Verdict {
  /// Nothing references the packet's picture, and nothing else in it is
  /// needed later: dropping it loses that picture and no other.
  Disposable,
  /// The packet is kept, for this reason.
  Kept(KeptReason),
}

impl Verdict {
  /// Whether the packet is disposable.
  pub const fn is_disposable(self) -> bool {
    matches!(self, Self::Disposable)
  }
}

/// Why [`NalStream::read`] keeps a packet: the first NAL unit, in the
/// packet's order, that keeps it, or what about the packet does.
///
/// Marked `#[non_exhaustive]`: a later reason joins as a variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum KeptReason {
  /// The packet holds no picture: no coded slice.
  NoPicture,
  /// A picture something may reference: an H.264 slice whose
  /// `nal_ref_idc` is not 0, or an HEVC sub-layer reference picture.
  Referenced,
  /// A random-access picture: an H.264 IDR slice, or an HEVC IRAP one.
  RandomAccess,
  /// A NAL unit beside the picture that a later picture, or what is
  /// output after it, may need, of this `nal_unit_type`: a parameter set,
  /// an SEI unit holding a message its picture alone does not take up, an
  /// end of sequence or of stream, a reserved type, or any other than an
  /// access unit delimiter or filler data.
  Companion(u8),
  /// A NAL unit of an extension: H.264's scalable, multiview or 3-D slices
  /// and prefixes (14, 20, 21), or any HEVC NAL unit of a layer above 0.
  Extension,
  /// An HEVC sub-layer non-reference picture below the highest temporal
  /// sub-layer, which a picture of a higher sub-layer may reference.
  BelowHighestSubLayer,
  /// An HEVC picture before any sequence parameter set: the stream's
  /// highest temporal sub-layer is not yet known.
  SubLayersUnknown,
  /// The packet does not parse as its track's framing says.
  Malformed,
  /// The track's NAL units follow lengths of a size no record has set:
  /// FFmpeg splits none of its packets.
  Unframed,
  /// An H.264 packet that is an `avcC` itself, which configures the decoder
  /// and holds no picture.
  Configuration,
  /// An H.264 packet whose first bytes switch the framing FFmpeg reads the
  /// stream in: a decoder that never saw it would read the packets after it
  /// otherwise.
  FramingChange,
  /// A packet the framing of which a decoder may read otherwise than the
  /// reading can know: after a seek that followed a change of framing, or
  /// after a record whose reading turns on whether its parameter sets
  /// decode.
  FramingUnknown,
  /// An H.264 picture of `nal_ref_idc` 0 that may be one field of a pair,
  /// which FFmpeg's decoder joins with its other field across packets: no
  /// sequence parameter set has been read, or one lets a picture be a field
  /// (`frame_mbs_only_flag` 0).
  MaybeField,
}

impl fmt::Display for KeptReason {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::NoPicture => f.write_str("no picture"),
      Self::Referenced => f.write_str("a picture something may reference"),
      Self::RandomAccess => f.write_str("a random-access picture"),
      Self::Companion(nal_unit_type) => {
        write!(
          f,
          "a NAL unit of type {nal_unit_type} a later picture may need"
        )
      }
      Self::Extension => f.write_str("a NAL unit of an extension"),
      Self::BelowHighestSubLayer => f.write_str("a picture below the highest temporal sub-layer"),
      Self::SubLayersUnknown => f.write_str("a picture before any sequence parameter set"),
      Self::Malformed => f.write_str("a packet that does not parse"),
      Self::Unframed => f.write_str("lengths of a size no record has set"),
      Self::Configuration => f.write_str("a configuration record"),
      Self::FramingChange => f.write_str("a packet that switches the framing"),
      Self::FramingUnknown => f.write_str("a framing a decoder may read otherwise"),
      Self::MaybeField => f.write_str("a picture that may be one field of a pair"),
    }
  }
}

/// One track's reading of its H.264 or HEVC bitstream: its codec, the
/// framing FFmpeg's decoder reads it in, and for HEVC the highest temporal
/// sub-layer any parameter set has declared so far.
///
/// The framing is FFmpeg's own state, held as its decoder holds it: whether
/// each NAL unit follows a length (`is_avc` in `libavcodec/h264dec.h`,
/// `is_nalff` in `hevc/hevcdec.h`) and the length's size
/// (`nal_length_size`), the two kept apart. A configuration record moves
/// them exactly as `ff_h264_decode_extradata` (`h264_parse.c` 466–524) and
/// `ff_hevc_decode_extradata` (`hevc/parse.c` 79–146) move them: the first
/// byte decides whether lengths are read, and the size moves only when the
/// record is read whole, so a four-byte size outlives a record of start
/// codes. An empty record changes nothing.
///
/// Built from the track's configuration record ([`Self::new`]), moved where
/// the configuration changes mid-stream ([`Self::reconfigure`],
/// [`Self::read_with_record`]), told of every seek ([`Self::seeked`]), and
/// fed each of the track's packets in decoding order ([`Self::read`]),
/// which learns from every parameter set it reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NalStream {
  codec: NalCodec,
  /// FFmpeg's `is_avc` or `is_nalff`: whether each NAL unit follows a
  /// length; a start code otherwise.
  lengths: bool,
  /// FFmpeg's `nal_length_size`: the bytes of each length, 0 before a
  /// record has set it; `None` where a record's reading turns on whether
  /// its parameter sets decode.
  length_size: Option<u8>,
  /// The framing the stream opened in, `(lengths, length_size)`.
  opened: (bool, Option<u8>),
  /// Whether the framing a decoder reads the next packets in is no longer
  /// known: nothing is marked again.
  lost: bool,
  /// The most any parameter set, configuration record or NAL unit has
  /// declared.
  highest_sub_layer: u8,
  /// Whether a sequence parameter set has been read.
  sps_seen: bool,
  /// For H.264: whether a sequence parameter set the decoder may read lets
  /// a picture be a field — `frame_mbs_only_flag` 0, or one this module
  /// cannot read so. It never clears.
  fields_possible: bool,
}

impl NalStream {
  /// The reading of a track of `codec` whose configuration record — its
  /// extradata — is `extradata`, empty for none, as the decoder reads it
  /// when it opens.
  pub fn new(codec: NalCodec, extradata: &[u8]) -> Self {
    let mut stream = Self {
      codec,
      lengths: false,
      length_size: Some(0),
      opened: (false, Some(0)),
      lost: false,
      highest_sub_layer: 0,
      sps_seen: false,
      fields_possible: false,
    };
    stream.record(extradata, true);
    stream.opened = (stream.lengths, stream.length_size);
    stream
  }

  /// Reads a new configuration record — a mid-stream change of the
  /// track's extradata, as `AV_PKT_DATA_NEW_EXTRADATA` brings one — as the
  /// decoder reads it, and learns from its parameter sets. What earlier
  /// parameter sets declared is kept: the highest sub-layer only rises.
  ///
  /// [`Self::read_with_record`] reads a packet together with the record it
  /// carries, as the decoder takes the two.
  pub fn reconfigure(&mut self, extradata: &[u8]) {
    self.record(extradata, false);
  }

  /// Reads one packet that carries `record` — its first
  /// `AV_PKT_DATA_NEW_EXTRADATA` entry, as `av_packet_get_side_data`
  /// answers, or `None` — as the decoder takes the two: the record first
  /// ([`Self::reconfigure`]), then the packet ([`Self::read`]). FFmpeg's
  /// H.264 decoder takes no record from a packet of no bytes, which it
  /// reads as the stream's end (`libavcodec/h264dec.c` 1034–1036); HEVC's
  /// takes it from any (`libavcodec/hevc/hevcdec.c` 3854–3860).
  pub fn read_with_record(&mut self, packet: &[u8], record: Option<&[u8]>) -> Verdict {
    if let Some(record) = record {
      if self.codec == NalCodec::Hevc || !packet.is_empty() {
        self.reconfigure(record);
      }
    }
    self.read(packet)
  }

  /// Tells the reading that the track was sought, or that a seek was
  /// attempted. Where the framing differs from the one the stream opened
  /// in, the framing a decoder reads the next packets in is no longer
  /// known — a decoder keeps its own across a seek, and one opened afresh
  /// starts from the stream's — so nothing is marked again
  /// ([`KeptReason::FramingUnknown`]).
  pub fn seeked(&mut self) {
    if (self.lengths, self.length_size) != self.opened {
      self.lost = true;
    }
  }

  /// The codec read.
  pub const fn codec(&self) -> NalCodec {
    self.codec
  }

  /// The framing the next packet is read in; `None` where the reading
  /// cannot read one — lengths of a size no record has set, or a framing
  /// a decoder may read otherwise ([`KeptReason::FramingUnknown`]).
  pub const fn framing(&self) -> Option<Framing> {
    if self.lost {
      return None;
    }
    match (self.lengths, self.length_size) {
      // H.264 reads its four-byte probe whatever `is_avc` says
      // (`h264dec.c` 602–607): a size the reading cannot know leaves even
      // its start codes unknown.
      (false, None) => match self.codec {
        NalCodec::H264 => None,
        NalCodec::Hevc => Some(Framing::AnnexB),
      },
      (false, Some(_)) => Some(Framing::AnnexB),
      (true, Some(bytes)) => match LengthSize::from_bytes(bytes) {
        Some(size) => Some(Framing::Length(size)),
        None => None,
      },
      (true, None) => None,
    }
  }

  /// For HEVC, the highest temporal sub-layer any parameter set,
  /// configuration record or NAL unit has declared so far; `None` before
  /// a sequence parameter set has been read, and for H.264.
  pub const fn highest_sub_layer(&self) -> Option<u8> {
    match self.codec {
      NalCodec::Hevc if self.sps_seen => Some(self.highest_sub_layer),
      _ => None,
    }
  }

  /// For H.264, whether every sequence parameter set read so far codes
  /// frames only (`frame_mbs_only_flag` 1); `None` before one has been
  /// read, and for HEVC.
  pub const fn frames_only(&self) -> Option<bool> {
    match self.codec {
      NalCodec::H264 if self.sps_seen => Some(!self.fields_possible),
      _ => None,
    }
  }

  /// Reads one packet: whether nothing references its picture. Packets
  /// are read in decoding order; every parameter set read raises what
  /// later packets are held to, and a packet that moves the framing
  /// moves it for the packets after.
  pub fn read(&mut self, packet: &[u8]) -> Verdict {
    if self.lost {
      return Verdict::Kept(KeptReason::FramingUnknown);
    }
    if self.codec == NalCodec::H264 {
      // A packet that is an `avcC` itself configures the decoder and holds
      // no picture (`h264dec.c` 1045–1050).
      if self.lengths && is_avcc_packet(packet) {
        self.h264_record(packet);
        return Verdict::Kept(KeptReason::Configuration);
      }
      match self.length_size {
        None => {
          self.learn_unframed(packet);
          return Verdict::Kept(KeptReason::FramingUnknown);
        }
        // With four-byte lengths a packet's first bytes may switch the
        // framing (`h264dec.c` 602–607), whatever the record said: a packet
        // that switches it is kept, so a decoder that sees it reads every
        // later packet in the framing the reading does.
        Some(4) => {
          let lengths = h264_packet_lengths(packet, self.lengths);
          if lengths != self.lengths {
            self.lengths = lengths;
            // The decoder reads this packet in the framing it switches to.
            if let Some(framing) = self.framing() {
              self.learn(packet, framing);
            }
            return Verdict::Kept(KeptReason::FramingChange);
          }
        }
        Some(_) => {}
      }
    }
    let Some(framing) = self.framing() else {
      self.learn_unframed(packet);
      return Verdict::Kept(KeptReason::Unframed);
    };
    let verdict = self.judge(packet, framing);
    self.learn(packet, framing);
    verdict
  }

  /// One packet's verdict, read in `framing` against what earlier packets
  /// taught; it learns nothing.
  fn judge(&self, packet: &[u8], framing: Framing) -> Verdict {
    let mut kept = None;
    let mut pictures = 0_usize;
    let mut units = Units::new(packet, framing);
    loop {
      match units.next() {
        Unit::End => break,
        Unit::Broken => return Verdict::Kept(KeptReason::Malformed),
        Unit::Nal(nal) => {
          let read = match self.codec {
            NalCodec::H264 => self.h264_unit(nal),
            NalCodec::Hevc => self.hevc_unit(nal),
          };
          match read {
            Read::Picture => pictures += 1,
            Read::Beside => {}
            Read::Keeps(KeptReason::Malformed) => {
              return Verdict::Kept(KeptReason::Malformed);
            }
            Read::Keeps(reason) => {
              kept.get_or_insert(reason);
            }
          }
        }
      }
    }
    match kept {
      Some(reason) => Verdict::Kept(reason),
      None if pictures == 0 => Verdict::Kept(KeptReason::NoPicture),
      None => Verdict::Disposable,
    }
  }

  /// Learns from every NAL unit FFmpeg's splitter may read in `packet`
  /// (`h2645_parse.c` 527–664), and more: in Annex B the bytes after each
  /// start code, past bytes that frame nothing; after lengths each unit,
  /// up to a length that does not fit — where the splitter refuses the
  /// packet whole — and the bytes after each start code within a unit.
  fn learn(&mut self, packet: &[u8], framing: Framing) {
    match framing {
      Framing::AnnexB => {
        for nal in AfterStartCodes::new(packet) {
          self.learn_unit(nal);
        }
      }
      Framing::Length(size) => self.learn_after_lengths(packet, size.bytes()),
    }
  }

  /// Learns from a packet the reading cannot frame, which a decoder still
  /// reads in a framing of its own: in every framing one may read it in,
  /// Annex B and lengths of one to four bytes (FFmpeg's `nal_length_size`,
  /// set from a record's two bits plus one).
  fn learn_unframed(&mut self, packet: &[u8]) {
    self.learn(packet, Framing::AnnexB);
    for size in 1..=4 {
      self.learn_after_lengths(packet, size);
    }
  }

  /// [`Self::learn`] for NAL units after lengths of `size` bytes.
  fn learn_after_lengths(&mut self, mut bytes: &[u8], size: usize) {
    while let Some((prefix, rest)) = bytes.split_at_checked(size) {
      let length = prefix
        .iter()
        .fold(0_usize, |length, &byte| (length << 8) | usize::from(byte));
      if length == 0 {
        return;
      }
      let Some((nal, rest)) = rest.split_at_checked(length) else {
        return;
      };
      // The splitter reads a unit to its first start code, and then the
      // bytes after each start code in it as units of their own
      // (`h2645_parse.c` 44–52, 559–584).
      let first = find_start_code(nal).unwrap_or(nal.len());
      self.learn_unit(trim_trailing_zeros(&nal[..first]));
      for piece in AfterStartCodes::new(nal) {
        self.learn_unit(piece);
      }
      bytes = rest;
    }
  }

  /// A configuration record, read as the codec's decoder reads one, when
  /// it opens or mid-stream.
  fn record(&mut self, extradata: &[u8], opening: bool) {
    match self.codec {
      NalCodec::H264 => self.h264_record(extradata),
      NalCodec::Hevc => self.hevc_record(extradata, opening),
    }
  }

  /// `ff_h264_decode_extradata` (`h264_parse.c` 466–524): no record
  /// changes nothing; a first byte of 1 makes the stream one of lengths
  /// before anything else is checked, and the size moves only when the
  /// `avcC` is read whole — or becomes unknown where reading it whole
  /// turns on whether a parameter set decodes; any other record makes it
  /// one of start codes, its size kept. Either way every sequence parameter
  /// set the decoder may read in it is learned.
  fn h264_record(&mut self, extradata: &[u8]) {
    let Some(&first) = extradata.first() else {
      return;
    };
    if first != 1 {
      self.lengths = false;
      // `decode_extradata_ps` splits the record as Annex B
      // (`h264_parse.c` 517–519).
      for nal in AfterStartCodes::new(extradata) {
        self.learn_unit(nal);
      }
      return;
    }
    self.lengths = true;
    // Every entry the record holds, its sequence parameter sets and then its
    // picture parameter sets, up to the first that does not fit: FFmpeg
    // reads each as `decode_extradata_ps_mp4` splits it, and a unit of
    // either array as its own type says (`h264_parse.c` 486–513, 367–413).
    let mut at = 6;
    let sps_count = extradata.get(5).map_or(0, |&count| count & 0x1F);
    if self.learn_avcc_entries(extradata, &mut at, sps_count)
      && let Some(&pps_count) = extradata.get(at)
    {
      at += 1;
      self.learn_avcc_entries(extradata, &mut at, pps_count);
    }
    match read_avcc(extradata) {
      AvccRead::Whole(size) => self.length_size = Some(size),
      AvccRead::Fails => {}
      // The size moves to the record's only if the parameter set
      // decodes: a size that is the record's already is known either way.
      AvccRead::Undecided(size) => {
        if self.length_size != Some(size) {
          self.length_size = None;
        }
      }
    }
  }

  /// Learns from `count` entries of an `avcC` from `*at`, each a two-byte
  /// length and its bytes, split as `decode_extradata_ps_mp4` splits one;
  /// whether all `count` fit.
  fn learn_avcc_entries(&mut self, record: &[u8], at: &mut usize, count: u8) -> bool {
    for _ in 0..count {
      let (Some(&high), Some(&low)) = (record.get(*at), record.get(*at + 1)) else {
        return false;
      };
      let size = usize::from(u16::from_be_bytes([high, low])) + 2;
      let Some(entry) = record.get(*at..*at + size) else {
        return false;
      };
      self.learn_after_lengths(entry, 2);
      *at += size;
    }
    true
  }

  /// `ff_hevc_decode_extradata` (`hevc/parse.c` 79–146): an `hvcC` — 23
  /// bytes or more, its first byte 1, or 0 with a nonzero second byte or a
  /// third above 1 — makes the stream one of lengths, of two bytes while
  /// its arrays are read and of the record's size once every unit has been
  /// read; any other record makes it one of start codes, its size kept.
  /// No record — the decoder reads one only of at least a byte
  /// (`hevc/hevcdec.c` 3856) — changes nothing. Either way the parameter
  /// sets declare the stream's sub-layers.
  ///
  /// Mid-stream, an `hvcC` of a size other than two leaves the framing
  /// unknown for good: under `AV_EF_EXPLODE` a parameter set that does not
  /// decode stops it at two (`hevc/parse.c` 72–76, 121–129).
  fn hevc_record(&mut self, extradata: &[u8], opening: bool) {
    if extradata.is_empty() {
      return;
    }
    let hvcc = extradata.len() >= 23
      && (extradata[0] == 1 || (extradata[0] == 0 && (extradata[1] != 0 || extradata[2] > 1)));
    if !hvcc {
      self.lengths = false;
      for nal in AfterStartCodes::new(extradata) {
        self.learn_unit(nal);
      }
      return;
    }
    self.lengths = true;
    // "nal units in the hvcC always have length coded with 2 bytes, so put
    // a fake nal_length_size = 2 while parsing them" (`hevc/parse.c`
    // 103–105): a record that fails part way leaves it so.
    self.length_size = Some(2);
    // Read as FFmpeg's `GetByteContext` reads (`bytestream.h`): past the
    // end a read gives 0.
    let mut record = Bytes::new(extradata);
    record.skip(21);
    // Byte 21: `constantFrameRate` (2), `numTemporalLayers` (3),
    // `temporalIdNested` (1), `lengthSizeMinusOne` (2)
    // (`libavformat/hevc.c` 1159–1167).
    let byte21 = record.byte();
    let arrays = record.byte();
    for _ in 0..arrays {
      let _type = record.byte();
      let count = record.be16();
      for _ in 0..count {
        // "+2 for the nal size field" (`hevc/parse.c` 113–119).
        let size = usize::from(record.peek_be16()) + 2;
        if record.left() < size {
          return;
        }
        // The decoder splits each unit after its two-byte length, as a
        // packet (`hevc/parse.c` 121–123, 24–37).
        self.learn_after_lengths(record.take(size), 2);
      }
    }
    let size = (byte21 & 3) + 1;
    if opening || size == 2 {
      self.length_size = Some(size);
    } else {
      self.lost = true;
    }
    let layers = (byte21 >> 3) & 7;
    if layers > 0 {
      self.raise(layers - 1);
    }
  }

  /// Raises the highest sub-layer to `declared`; it never falls.
  fn raise(&mut self, declared: u8) {
    if declared > self.highest_sub_layer {
      self.highest_sub_layer = declared;
    }
  }

  /// One HEVC NAL unit, read against what earlier packets taught.
  fn hevc_unit(&self, nal: &[u8]) -> Read {
    let &[first, second, ..] = nal else {
      return Read::Keeps(KeptReason::Malformed);
    };
    if first & 0x80 != 0 {
      return Read::Keeps(KeptReason::Malformed);
    }
    let nal_unit_type = (first >> 1) & 0x3F;
    let layer = ((first & 1) << 5) | (second >> 3);
    let Some(temporal_id) = (second & 7).checked_sub(1) else {
      return Read::Keeps(KeptReason::Malformed);
    };
    if layer != 0 {
      return Read::Keeps(KeptReason::Extension);
    }
    match nal_unit_type {
      // The SLNR types FFmpeg's decoder decodes: TRAIL_N, TSA_N, STSA_N,
      // RADL_N and RASL_N (7.4.2.2; `hevc/hevcdec.c` 3587–3680).
      0 | 2 | 4 | 6 | 8 => {
        if !self.sps_seen {
          Read::Keeps(KeptReason::SubLayersUnknown)
        } else if temporal_id < self.highest_sub_layer {
          Read::Keeps(KeptReason::BelowHighestSubLayer)
        } else if temporal_id > self.highest_sub_layer {
          // No parameter set declares the sub-layer: not as the stream
          // says it is.
          Read::Keeps(KeptReason::Malformed)
        } else {
          Read::Picture
        }
      }
      // The sub-layer reference types (TRAIL_R … RSV_VCL_R15).
      1 | 3 | 5 | 7 | 9 | 11 | 13 | 15 => Read::Keeps(KeptReason::Referenced),
      // The reserved SLNR types RSV_VCL_N10, N12 and N14, which FFmpeg's
      // decoder skips without a picture (`hevc/hevcdec.c` 3587–3680): what
      // rides beside them, a picture hash among it, would carry over to the
      // next picture.
      10 | 12 | 14 => Read::Keeps(KeptReason::Companion(nal_unit_type)),
      // IRAP: BLA, IDR, CRA, and the reserved IRAP types 22 and 23.
      16..=23 => Read::Keeps(KeptReason::RandomAccess),
      // A VPS or SPS whose `max_sub_layers_minus1` is past 6 does not
      // parse (7.4.3.1, 7.4.3.2.1).
      32 | 33 => match hevc_declared(nal_unit_type, nal) {
        Some(declared) if declared <= 6 => Read::Keeps(KeptReason::Companion(nal_unit_type)),
        _ => Read::Keeps(KeptReason::Malformed),
      },
      // AUD, FD.
      35 | 38 => Read::Beside,
      // Prefix and suffix SEI: beside the picture only where every message
      // its picture alone takes up.
      39 | 40 if sei_rides(&nal[2..], hevc_sei_rides) => Read::Beside,
      // PPS, EOS, EOB, any other SEI, the reserved VCL types 24–31, the
      // reserved non-VCL types 41–47 and the unspecified 48–63.
      other => Read::Keeps(KeptReason::Companion(other)),
    }
  }

  /// Learns from one NAL unit of the stream's codec.
  fn learn_unit(&mut self, nal: &[u8]) {
    match self.codec {
      NalCodec::H264 => self.h264_learn(nal),
      NalCodec::Hevc => self.hevc_learn(nal),
    }
  }

  /// Learns from one H.264 NAL unit: whether a sequence parameter set
  /// codes frames only. One that does not, or that this module cannot
  /// read so under every reading FFmpeg may make of it
  /// ([`sps_codes_frames_only`]), lets fields in for good. A unit whose
  /// forbidden bit is set, which FFmpeg's splitter passes over
  /// (`h2645_parse.c` 646–660), teaches nothing.
  fn h264_learn(&mut self, nal: &[u8]) {
    let Some(&header) = nal.first() else {
      return;
    };
    if header & 0x80 != 0 || header & 0x1F != 7 {
      return;
    }
    self.sps_seen = true;
    if !sps_codes_frames_only(&nal[1..]) {
      self.fields_possible = true;
    }
  }

  /// One H.264 NAL unit, read against what earlier packets taught.
  fn h264_unit(&self, nal: &[u8]) -> Read {
    match h264_unit(nal) {
      // A picture of `nal_ref_idc` 0 may be one field of a pair, which
      // FFmpeg's decoder joins with its other field across packets
      // (`h264_slice.c` 1397–1500): it is disposable only where every
      // sequence parameter set the decoder may read codes frames only.
      Read::Picture if !self.sps_seen || self.fields_possible => {
        Read::Keeps(KeptReason::MaybeField)
      }
      read => read,
    }
  }

  /// Learns from one HEVC NAL unit: a parameter set's
  /// `max_sub_layers_minus1`, and the sub-layer of any unit of layer 0. A
  /// unit FFmpeg's splitter passes over — its forbidden bit set, or a
  /// TemporalId of −1 (`h2645_parse.c` 646–660) — teaches nothing.
  fn hevc_learn(&mut self, nal: &[u8]) {
    let &[first, second, ..] = nal else {
      return;
    };
    if first & 0x80 != 0 {
      return;
    }
    let nal_unit_type = (first >> 1) & 0x3F;
    let layer = ((first & 1) << 5) | (second >> 3);
    let Some(temporal_id) = (second & 7).checked_sub(1) else {
      return;
    };
    if layer != 0 {
      return;
    }
    // A unit of a sub-layer no parameter set has declared: one the reading
    // has not read declares it.
    self.raise(temporal_id);
    if let Some(declared) = hevc_declared(nal_unit_type, nal) {
      if declared <= 6 {
        self.raise(declared);
        if nal_unit_type == 33 {
          self.sps_seen = true;
        }
      }
    }
  }
}

/// A VPS's or an SPS's `max_sub_layers_minus1`: three bits of the second
/// byte after a VPS's header, of the first after an SPS's (7.3.2.1,
/// 7.3.2.2.1). No emulation prevention byte can come before either, the
/// header's two bytes being nonzero. `None` for any other unit, and for
/// one too short to hold it.
fn hevc_declared(nal_unit_type: u8, nal: &[u8]) -> Option<u8> {
  let at = match nal_unit_type {
    32 => 3,
    33 => 2,
    _ => return None,
  };
  nal.get(at).map(|&byte| (byte >> 1) & 7)
}

/// Whether an H.264 sequence parameter set — `payload`, its bytes after the
/// header — codes frames only (`frame_mbs_only_flag` 1, 7.4.2.1.1), under
/// every reading of it FFmpeg's decoder may adopt
/// (`ff_h264_decode_seq_parameter_set`, `h264_ps.c` 284–451):
///
/// | reading | where FFmpeg makes it |
/// |---|---|
/// | [`Reading::Rbsp`] | first, in-band and in a record; again, past a truncation it then lets stand (`h264dec.c` 699–716; `h264_parse.c` 383–398) |
/// | [`Reading::Bytes`] | where the first fails (the same lines), and as the RBSP of an `avcC` entry it has escaped (`h264_parse.c` 421–464) |
/// | [`Reading::Reescaped`] | as the bytes of that escaped entry, where the RBSP of it fails |
///
/// Each reading is read only up to the first `00 00 00`, `00 00 01` or
/// `00 00 02`, at or before where FFmpeg's splitter ends the unit
/// (`h2645_parse.c` 44–52, 101–132), so it reads no more of the unit than
/// FFmpeg does. Up to the flag the readings differ only where the unit
/// holds `00 00 03`.
///
/// A reading that does not reach the flag, or that FFmpeg refuses on the
/// way — a value outside the range it accepts, separate colour planes —
/// is not proof, and answers `false`: the reading cannot tell which
/// parameter set the decoder is left with.
fn sps_codes_frames_only(payload: &[u8]) -> bool {
  let end = payload
    .windows(3)
    .position(|window| window[0] == 0 && window[1] == 0 && window[2] <= 2)
    .unwrap_or(payload.len());
  let unit = &payload[..end];
  [Reading::Rbsp, Reading::Bytes, Reading::Reescaped]
    .into_iter()
    .all(|reading| sps_frame_mbs_only(BitReader::new(unit, reading)) == Some(true))
}

/// `frame_mbs_only_flag`, read from a sequence parameter set as FFmpeg
/// reads it (`h264_ps.c` 284–420), with the bounds it refuses on; `None`
/// where the bits run out first or a bound fails.
fn sps_frame_mbs_only(mut bits: BitReader<'_>) -> Option<bool> {
  /// What `get_ue_golomb_31` reads: values to 30 alone (`golomb.h`).
  fn ue31(bits: &mut BitReader<'_>) -> Option<u32> {
    bits.ue().filter(|&value| value <= 30)
  }
  let profile_idc = bits.bits(8)?;
  // The constraint flags and `reserved_zero_2bits`, then `level_idc`.
  bits.bits(16)?;
  // `seq_parameter_set_id`, below `MAX_SPS_COUNT` (32).
  ue31(&mut bits)?;
  // FFmpeg's own list of the profiles that code chroma format, bit depths
  // and scaling matrices (`h264_ps.c` 343–354).
  if matches!(
    profile_idc,
    100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 144
  ) {
    let chroma_format_idc = ue31(&mut bits).filter(|&value| value <= 3)?;
    // FFmpeg refuses separate colour planes.
    if chroma_format_idc == 3 && bits.bits(1)? != 0 {
      return None;
    }
    let luma = ue31(&mut bits)?;
    let chroma = ue31(&mut bits)?;
    // Equal depths, of 8 to 14 bits.
    if luma != chroma || luma > 6 {
      return None;
    }
    // `qpprime_y_zero_transform_bypass_flag`.
    bits.bits(1)?;
    // `seq_scaling_matrix_present_flag`, then six 4x4 lists and two 8x8,
    // and four more 8x8 at 4:4:4 (`h264_ps.c` 231–266).
    if bits.bits(1)? == 1 {
      let lists = if chroma_format_idc == 3 { 12 } else { 8 };
      for list in 0..lists {
        if bits.bits(1)? == 1 {
          scaling_list(&mut bits, if list < 6 { 16 } else { 64 })?;
        }
      }
    }
  }
  // `log2_max_frame_num_minus4`, 0 to 12.
  ue31(&mut bits).filter(|&value| value <= 12)?;
  match ue31(&mut bits)? {
    // `log2_max_pic_order_cnt_lsb_minus4`, to 12.
    0 => {
      ue31(&mut bits).filter(|&value| value <= 12)?;
    }
    1 => {
      // `delta_pic_order_always_zero_flag`, `offset_for_non_ref_pic`,
      // `offset_for_top_to_bottom_field`, then a cycle of fewer than 256
      // `offset_for_ref_frame`.
      bits.bits(1)?;
      bits.se()?;
      bits.se()?;
      let cycle = bits.ue().filter(|&value| value < 256)?;
      for _ in 0..cycle {
        bits.se()?;
      }
    }
    2 => {}
    _ => return None,
  }
  // `max_num_ref_frames`, to `H264_MAX_DPB_FRAMES` (16).
  ue31(&mut bits).filter(|&value| value <= 16)?;
  // `gaps_in_frame_num_value_allowed_flag`, `pic_width_in_mbs_minus1`,
  // `pic_height_in_map_units_minus1`.
  bits.bits(1)?;
  bits.ue()?;
  bits.ue()?;
  Some(bits.bits(1)? == 1)
}

/// One scaling list of `size` deltas (7.3.2.1.1.1), each in `-128..=127`
/// as FFmpeg holds it (`h264_ps.c` 201–229).
fn scaling_list(bits: &mut BitReader<'_>, size: usize) -> Option<()> {
  let (mut last, mut next) = (8_i64, 8_i64);
  for index in 0..size {
    if next != 0 {
      let delta = bits.se().filter(|delta| (-128..=127).contains(delta))?;
      next = (last + delta) & 0xFF;
      if index == 0 && next == 0 {
        // The list is not written: the default stands.
        return Some(());
      }
    }
    if next != 0 {
      last = next;
    }
  }
  Some(())
}

/// Which bytes a [`BitReader`] reads of a NAL unit's payload.
#[derive(Clone, Copy)]
enum Reading {
  /// The RBSP: every `03` after two zero bytes dropped, as FFmpeg's
  /// splitter extracts it (`h2645_parse.c` 101–132).
  Rbsp,
  /// The bytes as they stand.
  Bytes,
  /// The bytes as `decode_extradata_ps_mp4` escapes them before it reads a
  /// parameter set again (`h264_parse.c` 438–455): a `03` written after
  /// every two zero bytes that come before a byte of 3 or less.
  Reescaped,
}

/// Bits of a NAL unit's payload, most significant first, in one
/// [`Reading`].
struct BitReader<'a> {
  reading: Reading,
  rbsp: Rbsp<'a>,
  raw: &'a [u8],
  /// For [`Reading::Reescaped`], the rest of an escape being written.
  pending: &'static [u8],
  byte: u8,
  left: u32,
}

impl<'a> BitReader<'a> {
  const fn new(bytes: &'a [u8], reading: Reading) -> Self {
    Self {
      reading,
      rbsp: Rbsp::new(bytes),
      raw: bytes,
      pending: &[],
      byte: 0,
      left: 0,
    }
  }

  /// The next byte of the reading.
  fn next_byte(&mut self) -> Option<u8> {
    match self.reading {
      Reading::Rbsp => self.rbsp.byte(),
      Reading::Bytes => {
        let (&byte, rest) = self.raw.split_first()?;
        self.raw = rest;
        Some(byte)
      }
      Reading::Reescaped => {
        if let Some((&byte, rest)) = self.pending.split_first() {
          self.pending = rest;
          return Some(byte);
        }
        // Where three bytes are left and they read `00 00 0x`, x at most
        // 3, `00 00 03` is written for the two zeros, and x is read next.
        if let [0, 0, rest @ ..] = self.raw
          && rest.first().is_some_and(|&next| next <= 3)
        {
          self.raw = rest;
          self.pending = &[0, 3];
          return Some(0);
        }
        let (&byte, rest) = self.raw.split_first()?;
        self.raw = rest;
        Some(byte)
      }
    }
  }

  fn bit(&mut self) -> Option<u32> {
    if self.left == 0 {
      self.byte = self.next_byte()?;
      self.left = 8;
    }
    self.left -= 1;
    Some(u32::from(self.byte >> self.left) & 1)
  }

  /// `n` bits, at most 32.
  fn bits(&mut self, n: u32) -> Option<u32> {
    (0..n).try_fold(0_u32, |value, _| Some((value << 1) | self.bit()?))
  }

  /// An unsigned Exp-Golomb value, `ue(v)` (9.1): at most 31 leading zero
  /// bits, as FFmpeg's longest reader takes.
  fn ue(&mut self) -> Option<u32> {
    let mut zeros = 0;
    while self.bit()? == 0 {
      zeros += 1;
      if zeros > 31 {
        return None;
      }
    }
    let rest = self.bits(zeros)?;
    Some(((1_u64 << zeros) - 1 + u64::from(rest)) as u32)
  }

  /// A signed Exp-Golomb value, `se(v)` (9.1.1).
  fn se(&mut self) -> Option<i64> {
    let k = i64::from(self.ue()?);
    Some(if k % 2 == 1 { (k + 1) / 2 } else { -(k / 2) })
  }
}

/// The size of the parameter set — its two-byte length counted — from
/// which `decode_extradata_ps_mp4` refuses one that does not decode, with
/// `ERANGE`, ending the record: `buf_size / 2 >= (INT16_MAX -
/// AV_INPUT_BUFFER_PADDING_SIZE) / 3` (`h264_parse.c` 436–437).
const ESCAPED_LIMIT: usize = 2 * ((i16::MAX as usize - 64) / 3);

/// How `ff_h264_decode_extradata` ends on an `avcC`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AvccRead {
  /// Read whole: the size is the record's, in bytes.
  Whole(u8),
  /// Refused part way: the size stays as it was.
  Fails,
  /// Read whole only if a parameter set of [`ESCAPED_LIMIT`] bytes or
  /// more decodes; the record's size, in bytes, if it does.
  Undecided(u8),
}

/// How FFmpeg reads `record`, an `avcC` (`h264_parse.c` 475–516): at least
/// seven bytes, then each count's parameter sets within the record — a
/// count FFmpeg reads past the record's end reads as 0, the zero padding
/// every FFmpeg buffer carries.
fn read_avcc(record: &[u8]) -> AvccRead {
  if record.len() < 7 {
    return AvccRead::Fails;
  }
  let mut at = 6;
  let mut undecided = false;
  if !avcc_sets(
    record,
    usize::from(record[5] & 0x1F),
    &mut at,
    &mut undecided,
  ) {
    return AvccRead::Fails;
  }
  let count = record.get(at).copied().unwrap_or(0);
  at += 1;
  if !avcc_sets(record, usize::from(count), &mut at, &mut undecided) {
    return AvccRead::Fails;
  }
  let size = (record[4] & 3) + 1;
  if undecided {
    AvccRead::Undecided(size)
  } else {
    AvccRead::Whole(size)
  }
}

/// `count` parameter sets of an `avcC` from `at`, each a two-byte size and
/// that many bytes within the record; whether all are, and whether any is
/// one of [`ESCAPED_LIMIT`] bytes or more.
fn avcc_sets(record: &[u8], count: usize, at: &mut usize, undecided: &mut bool) -> bool {
  for _ in 0..count {
    let (Some(&high), Some(&low)) = (record.get(*at), record.get(*at + 1)) else {
      return false;
    };
    let size = usize::from(u16::from_be_bytes([high, low])) + 2;
    if size > record.len() - *at {
      return false;
    }
    if size >= ESCAPED_LIMIT {
      *undecided = true;
    }
    *at += size;
  }
  true
}

/// Whether FFmpeg's H.264 decoder reads `packet` as an `avcC` of its own
/// (`h264dec.c` 1045–1046 and `is_avcc_extradata`, 899–921): at least nine
/// bytes, a first byte of 1, a third of 0 and a fifth whose high six bits
/// are set, then one or more sequence parameter sets and one or more
/// picture parameter sets, each within the packet.
fn is_avcc_packet(packet: &[u8]) -> bool {
  if packet.len() < 9 || packet[0] != 1 || packet[2] != 0 || packet[4] & 0xFC != 0xFC {
    return false;
  }
  // FFmpeg's padding: a byte past the packet reads as 0.
  let at_or_zero = |at: usize| packet.get(at).copied().unwrap_or(0);
  let sets = |count: usize, kind: u8, at: &mut usize| -> bool {
    if count == 0 {
      return false;
    }
    for _ in 0..count {
      let size = usize::from(u16::from_be_bytes([at_or_zero(*at), at_or_zero(*at + 1)])) + 2;
      if size > packet.len().saturating_sub(*at) || at_or_zero(*at + 2) & 0x9F != kind {
        return false;
      }
      *at += size;
    }
    true
  };
  let mut at = 6;
  if !sets(usize::from(packet[5] & 0x1F), 7, &mut at) {
    return false;
  }
  let count = at_or_zero(at);
  at += 1;
  sets(usize::from(count), 8, &mut at)
}

/// Whether FFmpeg's H.264 decoder reads `packet` after lengths where its
/// `nal_length_size` is 4 (`h264dec.c` 602–607): start codes for a packet
/// that begins with a four-byte start code whose next four bytes are a
/// length past the packet; lengths for one whose first four bytes are a
/// length that fits; else as the packet before it.
fn h264_packet_lengths(packet: &[u8], lengths: bool) -> bool {
  let word = |at: usize| -> Option<u32> {
    let bytes = packet.get(at..at + 4)?;
    Some(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
  };
  let size = packet.len();
  if size > 8 && word(0) == Some(1) && word(5).is_some_and(|next| next as usize > size) {
    false
  } else if size > 3 && word(0).is_some_and(|first| first > 1 && first as usize <= size) {
    true
  } else {
    lengths
  }
}

/// A record read as FFmpeg's `GetByteContext` reads one (`bytestream.h`):
/// a read past the end gives 0 and leaves the reader at the end.
struct Bytes<'a> {
  rest: &'a [u8],
}

impl<'a> Bytes<'a> {
  const fn new(bytes: &'a [u8]) -> Self {
    Self { rest: bytes }
  }

  const fn left(&self) -> usize {
    self.rest.len()
  }

  fn skip(&mut self, count: usize) {
    self.rest = &self.rest[count.min(self.rest.len())..];
  }

  fn byte(&mut self) -> u8 {
    match self.rest.split_first() {
      Some((&byte, rest)) => {
        self.rest = rest;
        byte
      }
      None => 0,
    }
  }

  fn be16(&mut self) -> u16 {
    match self.rest.split_first_chunk::<2>() {
      Some((&bytes, rest)) => {
        self.rest = rest;
        u16::from_be_bytes(bytes)
      }
      None => {
        self.rest = &self.rest[self.rest.len()..];
        0
      }
    }
  }

  fn peek_be16(&self) -> u16 {
    match self.rest.first_chunk::<2>() {
      Some(&bytes) => u16::from_be_bytes(bytes),
      None => 0,
    }
  }

  /// The next `count` bytes, which the caller has checked are left.
  fn take(&mut self, count: usize) -> &'a [u8] {
    let (taken, rest) = self.rest.split_at(count);
    self.rest = rest;
    taken
  }
}

/// One H.264 NAL unit, read.
fn h264_unit(nal: &[u8]) -> Read {
  let Some(&header) = nal.first() else {
    return Read::Keeps(KeptReason::Malformed);
  };
  if header & 0x80 != 0 {
    return Read::Keeps(KeptReason::Malformed);
  }
  let nal_ref_idc = (header >> 5) & 3;
  match header & 0x1F {
    // An IDR slice.
    5 => Read::Keeps(KeptReason::RandomAccess),
    // A slice, or a slice data partition A, B or C.
    1..=4 if nal_ref_idc != 0 => Read::Keeps(KeptReason::Referenced),
    1..=4 => Read::Picture,
    // An access unit delimiter, filler data.
    9 | 12 => Read::Beside,
    // SEI whose every message its picture alone takes up.
    6 if sei_rides(&nal[1..], h264_sei_rides) => Read::Beside,
    // A prefix NAL unit, a coded slice extension (scalable or multiview),
    // a 3-D AVC slice extension.
    14 | 20 | 21 => Read::Keeps(KeptReason::Extension),
    // SPS, PPS, any other SEI, the end of a sequence or of the stream, an
    // SPS extension, a subset SPS, a depth parameter set, an auxiliary
    // slice, and every reserved or unspecified type.
    other => Read::Keeps(KeptReason::Companion(other)),
  }
}

/// The H.264 SEI messages a picture alone takes up ([SEI](self#sei)):
/// picture timing, filler payload.
const fn h264_sei_rides(payload_type: u32) -> bool {
  matches!(payload_type, 1 | 3)
}

/// The HEVC SEI messages a picture alone takes up ([SEI](self#sei)):
/// filler payload.
const fn hevc_sei_rides(payload_type: u32) -> bool {
  matches!(payload_type, 3)
}

/// Whether an SEI unit — `payload`, its bytes after the header — holds one
/// message or more, every one of a type `rides` admits, read as FFmpeg
/// reads them (`h264_sei.c` 229–300, `hevc/sei.c` 264–317): emulation
/// prevention bytes dropped, each message a `payloadType` and a
/// `payloadSize` coded in runs of `0xFF`, then that many bytes, up to the
/// unit's last byte, `rbsp_trailing_bits`, which is `0x80`. Trailing zero
/// bytes are dropped first, as FFmpeg drops them (`h2645_parse.c`
/// 348–376). A unit that does not read so rides nowhere.
fn sei_rides(payload: &[u8], rides: fn(u32) -> bool) -> bool {
  let Some(last) = payload.iter().rposition(|&byte| byte != 0) else {
    return false;
  };
  if payload[last] != 0x80 {
    return false;
  }
  let mut rbsp = Rbsp::new(&payload[..last]);
  let mut messages = 0_usize;
  while let Some(first) = rbsp.byte() {
    let Some(payload_type) = rbsp.coded(first) else {
      return false;
    };
    let Some(size) = rbsp.byte().and_then(|first| rbsp.coded(first)) else {
      return false;
    };
    if !rides(payload_type) || !rbsp.skip(size) {
      return false;
    }
    messages += 1;
  }
  messages > 0
}

/// A NAL unit's bytes as its RBSP: each emulation prevention byte — a 3
/// after two zero bytes — dropped, as `ff_h2645_extract_rbsp` drops it
/// (`h2645_parse.c` 37–147).
struct Rbsp<'a> {
  raw: &'a [u8],
  zeros: u8,
}

impl<'a> Rbsp<'a> {
  const fn new(raw: &'a [u8]) -> Self {
    Self { raw, zeros: 0 }
  }

  fn byte(&mut self) -> Option<u8> {
    loop {
      let (&byte, rest) = self.raw.split_first()?;
      self.raw = rest;
      if self.zeros >= 2 && byte == 3 {
        self.zeros = 0;
        continue;
      }
      self.zeros = if byte == 0 {
        self.zeros.saturating_add(1)
      } else {
        0
      };
      return Some(byte);
    }
  }

  /// A value coded as SEI codes `payloadType` and `payloadSize`: each
  /// `0xFF` byte adds 255 and one more, `first` among them, ends it
  /// (7.3.2.3.1 in H.264, 7.3.5 in H.265).
  fn coded(&mut self, first: u8) -> Option<u32> {
    let mut value = u32::from(first);
    let mut byte = first;
    while byte == 0xFF {
      byte = self.byte()?;
      value = value.checked_add(u32::from(byte))?;
    }
    Some(value)
  }

  /// Passes over `count` bytes; whether there were that many.
  fn skip(&mut self, count: u32) -> bool {
    (0..count).all(|_| self.byte().is_some())
  }
}

/// What one NAL unit says about its packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Read {
  /// A slice of a picture nothing references.
  Picture,
  /// A NAL unit that may ride beside such a picture.
  Beside,
  /// A NAL unit that keeps the packet.
  Keeps(KeptReason),
}

/// The NAL units of a packet, one at a time, read strictly: any byte the
/// framing does not account for is [`Unit::Broken`].
struct Units<'a> {
  rest: &'a [u8],
  framing: Framing,
  /// Annex B: whether the first start code has been read.
  started: bool,
  /// Annex B: whether the last bytes read were a start code, which a NAL
  /// unit must follow.
  after_start_code: bool,
}

/// What [`Units::next`] yields.
enum Unit<'a> {
  /// A NAL unit, its header first.
  Nal(&'a [u8]),
  /// The packet's end.
  End,
  /// Bytes the framing does not account for.
  Broken,
}

impl<'a> Units<'a> {
  const fn new(packet: &'a [u8], framing: Framing) -> Self {
    Self {
      rest: packet,
      framing,
      started: false,
      after_start_code: false,
    }
  }

  fn next(&mut self) -> Unit<'a> {
    match self.framing {
      Framing::Length(size) => self.next_after_length(size.bytes()),
      Framing::AnnexB => self.next_after_start_code(),
    }
  }

  /// The next NAL unit after its length: a length of 0, one past the
  /// packet, bytes too few for a length, and a unit holding a start code
  /// — which FFmpeg's splitter reads as more than one unit
  /// (`h2645_parse.c` 44–52, 559–584) — are broken.
  fn next_after_length(&mut self, size: usize) -> Unit<'a> {
    if self.rest.is_empty() {
      return Unit::End;
    }
    let Some((prefix, rest)) = self.rest.split_at_checked(size) else {
      return Unit::Broken;
    };
    let length = prefix
      .iter()
      .fold(0_usize, |length, &byte| (length << 8) | usize::from(byte));
    if length == 0 {
      return Unit::Broken;
    }
    let Some((nal, rest)) = rest.split_at_checked(length) else {
      return Unit::Broken;
    };
    if find_start_code(nal).is_some() {
      return Unit::Broken;
    }
    self.rest = rest;
    Unit::Nal(nal)
  }

  /// The next NAL unit after its start code, to the next start code or the
  /// packet's end, its trailing zero bytes dropped. Zero bytes may lead the
  /// first start code; any other byte before it is broken, and so is a
  /// packet with no start code, and a start code no NAL unit follows.
  fn next_after_start_code(&mut self) -> Unit<'a> {
    if !self.started {
      let lead = self.rest.iter().take_while(|&&byte| byte == 0).count();
      if lead < 2 || self.rest.get(lead) != Some(&1) {
        return Unit::Broken;
      }
      self.rest = &self.rest[lead + 1..];
      self.started = true;
    } else if self.rest.is_empty() {
      return if self.after_start_code {
        Unit::Broken
      } else {
        Unit::End
      };
    }
    // The unit runs to the next `00 00 01`; the zeros before it are the
    // next start code's, or trailing zero bytes.
    let (unit, rest) = match find_start_code(self.rest) {
      Some(at) => {
        self.after_start_code = true;
        (&self.rest[..at], &self.rest[at + 3..])
      }
      None => {
        self.after_start_code = false;
        (self.rest, &self.rest[self.rest.len()..])
      }
    };
    self.rest = rest;
    let nal = trim_trailing_zeros(unit);
    if nal.is_empty() {
      return Unit::Broken;
    }
    Unit::Nal(nal)
  }
}

/// The bytes after each start code in a run of bytes that one byte or more
/// follows, each to the next start code or the end, its trailing zero
/// bytes dropped: every NAL unit FFmpeg's splitter may find there by
/// searching for start codes (`find_next_start_code`, `h2645_parse.c`
/// 465–478), whatever comes between.
struct AfterStartCodes<'a> {
  rest: &'a [u8],
}

impl<'a> AfterStartCodes<'a> {
  const fn new(bytes: &'a [u8]) -> Self {
    Self { rest: bytes }
  }
}

impl<'a> Iterator for AfterStartCodes<'a> {
  type Item = &'a [u8];

  fn next(&mut self) -> Option<&'a [u8]> {
    loop {
      let at = find_start_code(self.rest)?;
      let after = &self.rest[at + 3..];
      let end = find_start_code(after).unwrap_or(after.len());
      self.rest = &after[end..];
      let unit = trim_trailing_zeros(&after[..end]);
      if !unit.is_empty() {
        return Some(unit);
      }
    }
  }
}

/// `bytes` without its trailing zero bytes.
fn trim_trailing_zeros(bytes: &[u8]) -> &[u8] {
  let trailing = bytes.iter().rev().take_while(|&&byte| byte == 0).count();
  &bytes[..bytes.len() - trailing]
}

/// Where the next `00 00 01` begins in `bytes`.
fn find_start_code(bytes: &[u8]) -> Option<usize> {
  bytes.windows(3).position(|window| window == [0, 0, 1])
}

// The laws build packets in a `Vec`, so they need an allocator.
#[cfg(all(test, any(feature = "std", feature = "alloc")))]
mod tests;
