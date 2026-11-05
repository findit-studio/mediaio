//! `CodecId` newtype wrapping FFmpeg's `AVCodecID` discriminant.
//!
//! Constructed from hardcoded `AVCodecID` enum variants in our build's
//! bindgen-generated bindings, so we never cast an arbitrary `i32` into
//! the bindgen enum (that cast is UB when the value isn't in the enum's
//! discriminant set — the same hazard `crate::pix_fmt` documents). The
//! raw `i32` stored inside is what ends up passed to FFmpeg's C API
//! (which declares the codec id as `c_int`), so the boundary is sound.

use core::fmt;

use ffmpeg_next::ffi::AVCodecID;
use smol_bytes::Utf8Bytes;

/// Upper bound on the NUL search in [`CodecId::name`] and
/// [`CodecId::long_name`].
///
/// FFmpeg's longest codec name is a couple of dozen bytes and its
/// longest description a couple of hundred; the cap is generous for
/// both and exists only so that a version-skewed descriptor table
/// cannot turn the walk into an unbounded read — the discipline the
/// rest of this crate's FFI text handling follows.
const DESCRIPTOR_TEXT_MAX_BYTES: usize = 1024;

/// Codec identifier. Wraps the integer value of an `AVCodecID` enum
/// variant; comparisons and storage work without ever transmuting back
/// into the bindgen enum.
///
/// # The number is the identity; the name is a rendering of it
///
/// [`raw`](Self::raw) is what this type *is* — what FFmpeg passed, what
/// a store keys on, what equality compares. [`name`](Self::name) and
/// [`long_name`](Self::long_name) read libavcodec's descriptor table for
/// the word beside that number, so a consumer can cross into a typed
/// codec vocabulary (`mediaframe`'s, say) or write a human row without
/// this crate owning a second table that could come to disagree with
/// FFmpeg's. Neither is a key: two builds of FFmpeg agree on the number
/// long before they agree on every string.
#[repr(transparent)]
#[derive(Copy, Clone, Eq, PartialEq, Hash)]
pub struct CodecId(i32);

impl CodecId {
  /// Constructs a `CodecId` from the raw integer FFmpeg uses for
  /// `AVCodecContext::codec_id` etc. Use this only when you have a
  /// value that came from FFmpeg or that you know maps to a real
  /// codec; arbitrary integers are still legal but `Debug` will fall
  /// back to printing the raw value.
  #[inline]
  pub const fn from_raw(raw: i32) -> Self {
    Self(raw)
  }

  /// Returns the underlying integer.
  #[inline]
  pub const fn raw(self) -> i32 {
    self.0
  }

  // --- The descriptor's words ------------------------------------------

  /// FFmpeg's own short name for this codec — `"h264"`, `"aac"`,
  /// `"subrip"` — or `None` where this build's libavcodec has no
  /// descriptor for the id.
  ///
  /// **An open vocabulary, deliberately.** The answer is whatever the
  /// linked libavcodec's `codec_descriptors[]` says, not a set this
  /// crate enumerates: a codec FFmpeg learns in its next release names
  /// itself here with no change on this side, which an `enum` of codecs
  /// could never do. The constants above are the ids this crate has
  /// reason to *compare* against; they are not the roster of what this
  /// method can answer.
  ///
  /// **`None` is a real answer**, and it is why the descriptor table is
  /// read rather than `avcodec_get_name`: that function never fails, and
  /// for an id it cannot place it hands back the string
  /// `"unknown_codec"` — a sentinel a consumer would store as if it were
  /// a codec's word. Here an id with no descriptor has no name, said
  /// plainly.
  ///
  /// The word is the one FFmpeg's CLI and its containers use, which is
  /// what makes it the right thing to cross a typed vocabulary with;
  /// [`raw`](Self::raw) stays the identity.
  pub fn name(self) -> Option<Utf8Bytes> {
    let descriptor = self.descriptor();
    if descriptor.is_null() {
      return None;
    }
    // SAFETY: `descriptor` is non-null and points into libavcodec's
    // `static const codec_descriptors[]`, so the field read is in
    // bounds; `addr_of!` reaches it **without forming a reference** —
    // see [`Self::descriptor`] for why that matters here. The pointer
    // it yields is null or a NUL-terminated string literal in that same
    // table, which is the reader's contract.
    let name = unsafe { core::ptr::addr_of!((*descriptor).name).read() };
    // SAFETY: as above — the name is a static, NUL-terminated literal.
    // `from_static` stores the borrow rather than copying it; see
    // [`crate::ffi::table_text`].
    unsafe { crate::ffi::table_text(name, DESCRIPTOR_TEXT_MAX_BYTES) }.map(Utf8Bytes::from_static)
  }

  /// FFmpeg's human description for this codec — `"H.264 / AVC / MPEG-4
  /// AVC / MPEG-4 part 10"` — or `None` where the build has no
  /// descriptor, or the descriptor carries no description.
  ///
  /// A display string and nothing more: it is FFmpeg's prose, it is not
  /// stable across releases, and nothing should key on it. Use
  /// [`name`](Self::name) to cross vocabularies and [`raw`](Self::raw)
  /// to identify.
  pub fn long_name(self) -> Option<Utf8Bytes> {
    let descriptor = self.descriptor();
    if descriptor.is_null() {
      return None;
    }
    // SAFETY: as [`Self::name`], for the same table and the same
    // reason. `long_name` is independently nullable — a descriptor may
    // carry a name and no description — which the reader answers with
    // `None`.
    let long_name = unsafe { core::ptr::addr_of!((*descriptor).long_name).read() };
    // SAFETY: as above.
    unsafe { crate::ffi::table_text(long_name, DESCRIPTOR_TEXT_MAX_BYTES) }
      .map(Utf8Bytes::from_static)
  }

  /// The MIME type libavcodec's descriptor names first for this codec —
  /// `"image/png"`, `"application/x-truetype-font"` — or `None` where the
  /// build has no descriptor for the id, or the descriptor names none.
  ///
  /// The word Matroska's muxer writes for an attachment whose row names
  /// no MIME type of its own (`get_mimetype`, `libavformat/matroskaenc.c`
  /// 2496–2511 in FFmpeg 9.0.1), and so the one a writer asks before it
  /// opens one: an attachment with neither is refused there. Like
  /// [`name`](Self::name), an open vocabulary — whatever the linked
  /// libavcodec's table says.
  pub fn mime_type(self) -> Option<&'static str> {
    let descriptor = self.descriptor();
    if descriptor.is_null() {
      return None;
    }
    // SAFETY: as [`Self::name`]: `descriptor` points into libavcodec's
    // static table and the field is reached through `addr_of!`, without
    // forming a reference. `mime_types` is null, or a null-terminated
    // array of NUL-terminated literals in that same table.
    let list = unsafe { core::ptr::addr_of!((*descriptor).mime_types).read() };
    if list.is_null() {
      return None;
    }
    // SAFETY: a non-null `mime_types` holds at least its terminating
    // null, so its first element is in bounds and initialised.
    let first = unsafe { list.read() };
    // SAFETY: `first` is null or a static, NUL-terminated literal, the
    // reader's contract — a borrow valid for the process.
    unsafe { crate::ffi::table_text(first, DESCRIPTOR_TEXT_MAX_BYTES) }
  }

  /// Whether every frame of this codec is a key frame, as libavformat
  /// judges it when it writes one (`ff_is_intra_only`,
  /// `libavformat/avformat.c` 903–912 in FFmpeg 9.0.1): a codec whose
  /// descriptor is neither audio nor video — every subtitle and data codec
  /// — or one marked `AV_CODEC_PROP_INTRA_ONLY`. `false` where this build
  /// has no descriptor for the id, as libavformat answers.
  ///
  /// libavformat marks every packet of such a codec a key frame on its way
  /// into a muxer, and every demuxer's packet of it on the way out
  /// (`libavformat/demux.c` 1168).
  pub fn intra_only(self) -> bool {
    let descriptor = self.descriptor();
    if descriptor.is_null() {
      return false;
    }
    // SAFETY: as [`Self::name`]: `descriptor` points into libavcodec's
    // static table, and each field is reached through `addr_of!` without
    // forming a reference; the media type is read as the integer it is
    // stored as, so no bindgen enum is formed from the linked library's
    // value.
    let (media, props) = unsafe {
      (
        core::ptr::addr_of!((*descriptor).type_)
          .cast::<i32>()
          .read_unaligned(),
        core::ptr::addr_of!((*descriptor).props).read(),
      )
    };
    let audio_or_video = media == ffmpeg_next::ffi::AVMediaType::AVMEDIA_TYPE_VIDEO as i32
      || media == ffmpeg_next::ffi::AVMediaType::AVMEDIA_TYPE_AUDIO as i32;
    !audio_or_video || props & ffmpeg_next::ffi::AV_CODEC_PROP_INTRA_ONLY != 0
  }

  /// The media type libavcodec's descriptor gives this codec —
  /// `AVMEDIA_TYPE_*`, as the integer `AVCodecDescriptor.type` stores it
  /// (`avcodec_get_type`) — or `None` where this build has no descriptor
  /// for the id.
  pub fn media_type(self) -> Option<i32> {
    let descriptor = self.descriptor();
    if descriptor.is_null() {
      return None;
    }
    // SAFETY: as [`Self::intra_only`]: the descriptor is a static table
    // entry of libavcodec's, its media type read through `addr_of!` as the
    // integer it is stored as, so no bindgen enum is formed from it.
    Some(unsafe {
      core::ptr::addr_of!((*descriptor).type_)
        .cast::<i32>()
        .read_unaligned()
    })
  }

  /// The bits one sample of this codec takes where its samples have a
  /// fixed size — 16 for `PCM_S16LE` — else `0`
  /// (`av_get_bits_per_sample`).
  pub fn bits_per_sample(self) -> u32 {
    // SAFETY: the redeclared shim takes a plain `c_int`, so no bindgen
    // enum is formed from `self.0`, and libavcodec answers every integer
    // from a `switch`.
    let bits = unsafe { crate::ffi::c_shims::av_get_bits_per_sample(self.0) };
    u32::try_from(bits).unwrap_or(0)
  }

  /// libavcodec's descriptor for this id, or null where this build
  /// names no codec for it.
  ///
  /// # Two enum hazards, and both are kept out
  ///
  /// **Going in**, the id crosses as the `c_int` it is — through the
  /// crate's own `avcodec_descriptor_get` redeclaration in
  /// `decoder::c_shims` — so no `AVCodecID` is ever constructed from a
  /// value that may not be in this build's discriminant set. libavcodec
  /// bounds-checks the id itself and answers null for anything outside
  /// its table, so every `i32` this type can hold is a defined call.
  ///
  /// **Coming back**, the pointer is left raw and the callers read
  /// their one field through `addr_of!`. `AVCodecDescriptor` embeds an
  /// `AVCodecID` and an `AVMediaType`, and forming a
  /// `&AVCodecDescriptor` would assert every field valid — including
  /// those two, which come from the *linked* library and need not lie
  /// inside the discriminant set the *bindings* were generated from.
  /// That is the same skew `crate::ffi`'s module note is about, met on
  /// the way out of C rather than on the way in.
  fn descriptor(self) -> *const ffmpeg_next::ffi::AVCodecDescriptor {
    // SAFETY: the redeclared shim takes a plain `c_int`, so no bindgen
    // enum is formed from `self.0`, and libavcodec answers null for an
    // id it cannot place.
    unsafe { crate::ffi::c_shims::avcodec_descriptor_get(self.0) }
  }

  // --- Sentinels -------------------------------------------------------

  /// `AV_CODEC_ID_NONE` — sentinel for "no codec."
  pub const NONE: Self = Self(AVCodecID::AV_CODEC_ID_NONE as i32);

  // --- Video codecs ----------------------------------------------------

  /// H.264 / AVC (ITU-T H.264 / ISO/IEC 14496-10).
  pub const H264: Self = Self(AVCodecID::AV_CODEC_ID_H264 as i32);
  /// H.265 / HEVC (ITU-T H.265 / ISO/IEC 23008-2).
  pub const HEVC: Self = Self(AVCodecID::AV_CODEC_ID_HEVC as i32);
  /// AV1 (Alliance for Open Media).
  pub const AV1: Self = Self(AVCodecID::AV_CODEC_ID_AV1 as i32);
  /// VP9 (Google).
  pub const VP9: Self = Self(AVCodecID::AV_CODEC_ID_VP9 as i32);
  /// VP8 (Google).
  pub const VP8: Self = Self(AVCodecID::AV_CODEC_ID_VP8 as i32);
  /// MPEG-2 Video (ITU-T H.262 / ISO/IEC 13818-2).
  pub const MPEG2VIDEO: Self = Self(AVCodecID::AV_CODEC_ID_MPEG2VIDEO as i32);
  /// MPEG-4 Part 2 Visual (ISO/IEC 14496-2).
  pub const MPEG4: Self = Self(AVCodecID::AV_CODEC_ID_MPEG4 as i32);
  /// Apple ProRes.
  pub const PRORES: Self = Self(AVCodecID::AV_CODEC_ID_PRORES as i32);
  /// Avid DNxHD / DNxHR (SMPTE VC-3).
  pub const DNXHD: Self = Self(AVCodecID::AV_CODEC_ID_DNXHD as i32);
  /// FFV1 — lossless intra-frame.
  pub const FFV1: Self = Self(AVCodecID::AV_CODEC_ID_FFV1 as i32);
  /// JPEG 2000.
  pub const JPEG2000: Self = Self(AVCodecID::AV_CODEC_ID_JPEG2000 as i32);
  /// MJPEG.
  pub const MJPEG: Self = Self(AVCodecID::AV_CODEC_ID_MJPEG as i32);
  /// VC-1 (SMPTE 421M, Microsoft Windows Media Video 9).
  pub const VC1: Self = Self(AVCodecID::AV_CODEC_ID_VC1 as i32);
  /// VVC / H.266 (ITU-T H.266).
  pub const VVC: Self = Self(AVCodecID::AV_CODEC_ID_VVC as i32);
  /// MPEG-5 LCEVC (ISO/IEC 23094-2) enhancement data.
  pub const LCEVC: Self = Self(AVCodecID::AV_CODEC_ID_LCEVC as i32);
  /// MPEG-5 EVC (ISO/IEC 23094-1).
  pub const EVC: Self = Self(AVCodecID::AV_CODEC_ID_EVC as i32);
  /// APV (Advanced Professional Video).
  pub const APV: Self = Self(AVCodecID::AV_CODEC_ID_APV as i32);
  /// RealVideo 1.0.
  pub const RV10: Self = Self(AVCodecID::AV_CODEC_ID_RV10 as i32);
  /// RealVideo 2.0.
  pub const RV20: Self = Self(AVCodecID::AV_CODEC_ID_RV20 as i32);
  /// RealVideo 3.0.
  pub const RV30: Self = Self(AVCodecID::AV_CODEC_ID_RV30 as i32);

  // --- Audio codecs ----------------------------------------------------

  /// AAC (ISO/IEC 14496-3).
  pub const AAC: Self = Self(AVCodecID::AV_CODEC_ID_AAC as i32);
  /// MP3 (MPEG-1/2 Audio Layer III).
  pub const MP3: Self = Self(AVCodecID::AV_CODEC_ID_MP3 as i32);
  /// Opus (RFC 6716).
  pub const OPUS: Self = Self(AVCodecID::AV_CODEC_ID_OPUS as i32);
  /// FLAC — Free Lossless Audio Codec.
  pub const FLAC: Self = Self(AVCodecID::AV_CODEC_ID_FLAC as i32);
  /// AC-3 (ATSC A/52, Dolby Digital).
  pub const AC3: Self = Self(AVCodecID::AV_CODEC_ID_AC3 as i32);
  /// E-AC-3 (Dolby Digital Plus).
  pub const EAC3: Self = Self(AVCodecID::AV_CODEC_ID_EAC3 as i32);
  /// Apple Lossless Audio Codec.
  pub const ALAC: Self = Self(AVCodecID::AV_CODEC_ID_ALAC as i32);
  /// DTS / DTS-HD.
  pub const DTS: Self = Self(AVCodecID::AV_CODEC_ID_DTS as i32);
  /// Vorbis.
  pub const VORBIS: Self = Self(AVCodecID::AV_CODEC_ID_VORBIS as i32);
  /// PCM signed 16-bit little-endian.
  pub const PCM_S16LE: Self = Self(AVCodecID::AV_CODEC_ID_PCM_S16LE as i32);
  /// PCM signed 16-bit big-endian.
  pub const PCM_S16BE: Self = Self(AVCodecID::AV_CODEC_ID_PCM_S16BE as i32);
  /// PCM signed 24-bit little-endian.
  pub const PCM_S24LE: Self = Self(AVCodecID::AV_CODEC_ID_PCM_S24LE as i32);
  /// PCM signed 24-bit big-endian.
  pub const PCM_S24BE: Self = Self(AVCodecID::AV_CODEC_ID_PCM_S24BE as i32);
  /// PCM signed 32-bit little-endian.
  pub const PCM_S32LE: Self = Self(AVCodecID::AV_CODEC_ID_PCM_S32LE as i32);
  /// PCM signed 32-bit big-endian.
  pub const PCM_S32BE: Self = Self(AVCodecID::AV_CODEC_ID_PCM_S32BE as i32);
  /// PCM 32-bit float little-endian.
  pub const PCM_F32LE: Self = Self(AVCodecID::AV_CODEC_ID_PCM_F32LE as i32);
  /// PCM 32-bit float big-endian.
  pub const PCM_F32BE: Self = Self(AVCodecID::AV_CODEC_ID_PCM_F32BE as i32);
  /// PCM 64-bit float little-endian.
  pub const PCM_F64LE: Self = Self(AVCodecID::AV_CODEC_ID_PCM_F64LE as i32);
  /// PCM 64-bit float big-endian.
  pub const PCM_F64BE: Self = Self(AVCodecID::AV_CODEC_ID_PCM_F64BE as i32);
  /// WavPack.
  pub const WAVPACK: Self = Self(AVCodecID::AV_CODEC_ID_WAVPACK as i32);
  /// Dolby TrueHD.
  pub const TRUEHD: Self = Self(AVCodecID::AV_CODEC_ID_TRUEHD as i32);
  /// Sony ATRAC3.
  pub const ATRAC3: Self = Self(AVCodecID::AV_CODEC_ID_ATRAC3 as i32);
  /// RealAudio Cook.
  pub const COOK: Self = Self(AVCodecID::AV_CODEC_ID_COOK as i32);
  /// RealAudio 2.0 (28.8 kbit/s).
  pub const RA_288: Self = Self(AVCodecID::AV_CODEC_ID_RA_288 as i32);
  /// RealAudio SIPR.
  pub const SIPR: Self = Self(AVCodecID::AV_CODEC_ID_SIPR as i32);

  // --- Still-image and attachment codecs -------------------------------

  /// PNG — a cover picture's usual codec.
  pub const PNG: Self = Self(AVCodecID::AV_CODEC_ID_PNG as i32);
  /// BMP — a cover picture MP4 can carry.
  pub const BMP: Self = Self(AVCodecID::AV_CODEC_ID_BMP as i32);
  /// TrueType font — a Matroska attachment.
  pub const TTF: Self = Self(AVCodecID::AV_CODEC_ID_TTF as i32);
  /// OpenType font — a Matroska attachment.
  pub const OTF: Self = Self(AVCodecID::AV_CODEC_ID_OTF as i32);
  /// Plain text — an attachment Matroska labels `text/plain`.
  pub const TEXT: Self = Self(AVCodecID::AV_CODEC_ID_TEXT as i32);

  // --- Subtitle codecs -------------------------------------------------

  /// SubRip (.srt).
  pub const SUBRIP: Self = Self(AVCodecID::AV_CODEC_ID_SUBRIP as i32);
  /// Advanced SubStation Alpha (.ass / .ssa).
  pub const ASS: Self = Self(AVCodecID::AV_CODEC_ID_ASS as i32);
  /// WebVTT (.vtt).
  pub const WEBVTT: Self = Self(AVCodecID::AV_CODEC_ID_WEBVTT as i32);
  /// 3GPP Timed Text / MOV text track.
  pub const MOV_TEXT: Self = Self(AVCodecID::AV_CODEC_ID_MOV_TEXT as i32);
  /// DVB subtitle (bitmap).
  pub const DVB_SUBTITLE: Self = Self(AVCodecID::AV_CODEC_ID_DVB_SUBTITLE as i32);
  /// HDMV / Blu-ray PGS subtitle (bitmap).
  pub const HDMV_PGS_SUBTITLE: Self = Self(AVCodecID::AV_CODEC_ID_HDMV_PGS_SUBTITLE as i32);
  /// DVD VOBSUB subtitle (bitmap).
  pub const DVD_SUBTITLE: Self = Self(AVCodecID::AV_CODEC_ID_DVD_SUBTITLE as i32);
  /// TTML (Timed Text Markup Language).
  pub const TTML: Self = Self(AVCodecID::AV_CODEC_ID_TTML as i32);
}

impl fmt::Debug for CodecId {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let name = match *self {
      Self::NONE => "NONE",
      Self::H264 => "H264",
      Self::HEVC => "HEVC",
      Self::AV1 => "AV1",
      Self::VP9 => "VP9",
      Self::VP8 => "VP8",
      Self::MPEG2VIDEO => "MPEG2VIDEO",
      Self::MPEG4 => "MPEG4",
      Self::PRORES => "PRORES",
      Self::DNXHD => "DNXHD",
      Self::FFV1 => "FFV1",
      Self::JPEG2000 => "JPEG2000",
      Self::MJPEG => "MJPEG",
      Self::VC1 => "VC1",
      Self::VVC => "VVC",
      Self::LCEVC => "LCEVC",
      Self::EVC => "EVC",
      Self::APV => "APV",
      Self::RV10 => "RV10",
      Self::RV20 => "RV20",
      Self::RV30 => "RV30",
      Self::AAC => "AAC",
      Self::MP3 => "MP3",
      Self::OPUS => "OPUS",
      Self::FLAC => "FLAC",
      Self::AC3 => "AC3",
      Self::EAC3 => "EAC3",
      Self::ALAC => "ALAC",
      Self::DTS => "DTS",
      Self::VORBIS => "VORBIS",
      Self::PCM_S16LE => "PCM_S16LE",
      Self::PCM_S16BE => "PCM_S16BE",
      Self::PCM_S24LE => "PCM_S24LE",
      Self::PCM_S24BE => "PCM_S24BE",
      Self::PCM_S32LE => "PCM_S32LE",
      Self::PCM_S32BE => "PCM_S32BE",
      Self::PCM_F32LE => "PCM_F32LE",
      Self::PCM_F32BE => "PCM_F32BE",
      Self::PCM_F64LE => "PCM_F64LE",
      Self::PCM_F64BE => "PCM_F64BE",
      Self::WAVPACK => "WAVPACK",
      Self::TRUEHD => "TRUEHD",
      Self::ATRAC3 => "ATRAC3",
      Self::COOK => "COOK",
      Self::RA_288 => "RA_288",
      Self::SIPR => "SIPR",
      Self::SUBRIP => "SUBRIP",
      Self::ASS => "ASS",
      Self::WEBVTT => "WEBVTT",
      Self::MOV_TEXT => "MOV_TEXT",
      Self::DVB_SUBTITLE => "DVB_SUBTITLE",
      Self::HDMV_PGS_SUBTITLE => "HDMV_PGS_SUBTITLE",
      Self::DVD_SUBTITLE => "DVD_SUBTITLE",
      Self::TTML => "TTML",
      _ => return write!(f, "CodecId({})", self.0),
    };
    write!(f, "CodecId::{name}")
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn from_raw_round_trips() {
    let id = CodecId::from_raw(27);
    assert_eq!(id.raw(), 27);
  }

  #[test]
  fn known_constants_match_av_values() {
    assert_eq!(CodecId::H264.raw(), AVCodecID::AV_CODEC_ID_H264 as i32);
    assert_eq!(CodecId::AAC.raw(), AVCodecID::AV_CODEC_ID_AAC as i32);
    assert_eq!(CodecId::SUBRIP.raw(), AVCodecID::AV_CODEC_ID_SUBRIP as i32);
  }

  #[test]
  fn debug_uses_name_for_known_codecs() {
    assert_eq!(format!("{:?}", CodecId::H264), "CodecId::H264");
    assert_eq!(format!("{:?}", CodecId::AAC), "CodecId::AAC");
  }

  #[test]
  fn debug_falls_back_to_raw_for_unknown() {
    let unknown = CodecId::from_raw(-99_999);
    assert_eq!(format!("{:?}", unknown), "CodecId(-99999)");
  }

  #[test]
  fn equality_is_value_based() {
    assert_eq!(CodecId::H264, CodecId::from_raw(CodecId::H264.raw()));
    assert_ne!(CodecId::H264, CodecId::HEVC);
  }

  /// **The name producer, against the linked libavcodec** — the whole of
  /// issue #43's ask: a codec id two crates deep now renders as the word
  /// FFmpeg itself spells it with.
  ///
  /// The three arms span the media kinds, because the descriptor table
  /// is one table and a consumer crossing into a typed vocabulary
  /// crosses on all three.
  #[test]
  fn known_codecs_name_themselves() {
    assert_eq!(CodecId::H264.name().as_deref(), Some("h264"));
    assert_eq!(CodecId::AAC.name().as_deref(), Some("aac"));
    assert_eq!(CodecId::SUBRIP.name().as_deref(), Some("subrip"));
  }

  /// The long name is prose and is only asked to *be* there — pinning
  /// FFmpeg's wording would pin a string its next release may reword,
  /// which is exactly why nothing should key on it.
  #[test]
  fn a_known_codec_carries_a_description_too() {
    let long = CodecId::H264.long_name().expect("h264 has a description");
    assert!(
      long.contains("H.264"),
      "libavcodec's description for h264 should name the standard, got {long:?}",
    );
  }

  /// **An id with no descriptor has no name**, which is the reason the
  /// descriptor table is read rather than `avcodec_get_name` — that
  /// function answers `"unknown_codec"` here, a sentinel a consumer
  /// would store as though it were a codec's word.
  ///
  /// Two shapes, both reachable through the public `from_raw` door: a
  /// negative id, and one far past the end of any table.
  #[test]
  fn an_id_with_no_descriptor_has_no_name() {
    for unknown in [CodecId::from_raw(-99_999), CodecId::from_raw(i32::MAX)] {
      assert_eq!(unknown.name(), None, "{unknown:?} named itself");
      assert_eq!(unknown.long_name(), None, "{unknown:?} described itself");
    }
  }

  /// `AV_CODEC_ID_NONE` is a **sentinel, not a codec**, and libavcodec
  /// keeps no descriptor for it — so the "no codec" row reads as absent
  /// rather than as a track coded with something called `none`.
  #[test]
  fn the_none_sentinel_names_no_codec() {
    assert_eq!(CodecId::NONE.name(), None);
  }

  /// The number stays the key. A name is read *off* the id and never
  /// back into it: two ids that name differently are two ids, and the
  /// identity a store writes is [`CodecId::raw`].
  #[test]
  fn the_name_is_a_rendering_and_the_number_is_the_identity() {
    let from_number = CodecId::from_raw(CodecId::H264.raw());
    assert_eq!(from_number, CodecId::H264);
    assert_eq!(from_number.name(), CodecId::H264.name());
    assert_ne!(CodecId::H264.name(), CodecId::HEVC.name());
  }
}
