//! [`DemuxerOpen`]: everything [`CarrierDemuxer::open_from`] and
//! [`CarrierDemuxer::open_reader_from`] take beside the input.

use crate::{DemuxerName, limits::DemuxLimits};

#[cfg(doc)]
use super::CarrierDemuxer;

/// How to open a demuxer: every optional of its construction in one value,
/// which [`CarrierDemuxer::open_from`] takes beside a path and
/// [`CarrierDemuxer::open_reader_from`] beside a reader.
///
/// [`Self::new`] is [`CarrierDemuxer::open`]'s request:
///
/// - [`Self::limits`]: what the session may spend ([`DemuxLimits::new`]
///   here). It is spent at open; [`CarrierDemuxer::open_with`] says why.
/// - [`Self::plaintext_demuxers`]: the demuxers whose inputs the caller
///   declares plaintext, so that their H.264 and HEVC tracks are read for
///   the bitstream's `DISPOSABLE` marks. None here: by default the raw
///   `h264` and `hevc` elementary streams alone are read.
///
/// Each optional has a getter, a consuming `with_*` and an in-place
/// `set_*`. Each of the four `open*` constructors opens one such request:
/// [`CarrierDemuxer::open_with`]`(path, limits)` opens
/// `DemuxerOpen::new().with_limits(limits)`.
///
/// Marked `#[non_exhaustive]`: a later optional joins as a field with its
/// own methods, not as another constructor.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct DemuxerOpen {
  limits: DemuxLimits,
  plaintext_demuxers: Vec<DemuxerName>,
}

impl Default for DemuxerOpen {
  #[cfg_attr(not(tarpaulin), inline(always))]
  fn default() -> Self {
    Self::new()
  }
}

impl DemuxerOpen {
  /// [`CarrierDemuxer::open`]'s request: the default limits, and no demuxer
  /// declared plaintext.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new() -> Self {
    Self {
      limits: DemuxLimits::new(),
      plaintext_demuxers: Vec::new(),
    }
  }

  /// What the session may spend.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn limits(&self) -> DemuxLimits {
    self.limits
  }

  /// **The demuxers whose inputs the caller declares plaintext**, each by
  /// its whole name ([`DemuxerName`], which the linked libavformat must
  /// register): an H.264 or HEVC track of an input one of them opens is
  /// read, and its packets are marked
  /// [`DISPOSABLE`](mediadecode::packet::PacketFlags::DISPOSABLE) where its
  /// NAL units prove that nothing references their picture
  /// ([`mediadecode::nal`]). Empty by default.
  ///
  /// **The library reads no container's bitstream unless it is declared
  /// here.** FFmpeg cannot prove a container's payload plaintext: an MP4
  /// track protected under OMA DRM 2's PDCF reaches the demuxer as
  /// ciphertext, with nothing public to say so. So by default only the raw
  /// `h264` and `hevc` elementary streams are read, plaintext by
  /// construction, and every other demuxer's packets carry its container's
  /// own mark alone, as FFmpeg delivers it: MP4's `sdtp`, which FFmpeg sets
  /// in `mov` alone and keeps on a track it does not parse as it reads —
  /// never on H.264, whose packets its parse rebuilds without the mark.
  ///
  /// **The declaration is the caller's assertion, and its risk the
  /// caller's.** Declaring a demuxer asserts that no input it opens is
  /// encrypted content that will be decrypted downstream, and accepts that
  /// an encrypted input could be marked falsely: its ciphertext, read as a
  /// bitstream, can look like a picture nothing references, so a packet
  /// whose decrypted picture a later one references can be marked, and a
  /// consumer that drops it would lose that later picture's pixels. A
  /// caller that decodes without keys, as is typical, cannot decode an
  /// encrypted input in any case.
  ///
  /// **Inside a declared demuxer every signal FFmpeg gives still fails
  /// closed**, and none adds a reading: a packet carrying
  /// `AV_PKT_DATA_ENCRYPTION_INFO` or `AV_PKT_DATA_ENCRYPTION_INIT_INFO` is
  /// not read, and its track's reading ends there for good — **latched as
  /// the packet enters the demuxer, whatever becomes of it**: delivered,
  /// refused by a limit, parked after a failed allocation, or discarded by a
  /// seek. And a track FFmpeg names encrypted at the open is never read — the stream's
  /// `ENCRYPTION_INIT_INFO` (`mov`'s `pssh`), its `enc_key_id` (Matroska's
  /// key id), or a `codec_tag` of `encv`. What no signal answers, by
  /// demuxer, in FFmpeg 9.0.1:
  ///
  /// | demuxer | what reaches a declared reading with no signal |
  /// |---|---|
  /// | `mov,mp4,m4a,3gp,3g2,mj2` | a track under a protection scheme with no CENC index and no `pssh`, such as OMA DRM 2's PDCF (`odkm`): FFmpeg takes the codec from `frma`, replacing the `encv` four-cc, and keeps the scheme private; and a protected sample description that is not the track's first |
  /// | `matroska,webm` | a `ContentEncryption` without a key id, and a track of more than one encoding |
  /// | `asf` | its content encryption, which FFmpeg names in the container's metadata alone, unread here, returning the payload as stored |
  /// | `asf_o` | its content encryption, whose objects FFmpeg skips |
  /// | `wtv` | its protection (`DSATTRIB_WMDRMProtectionInfo`) and a DVB-scrambled stream, each of which FFmpeg logs, nothing more |
  /// | `mxf` | an encrypted triplet read without a key, which FFmpeg returns as its ciphertext |
  /// | `mpegts`, `mpegtsraw` | a scrambled packet: FFmpeg never reads the transport scrambling control |
  /// | `concat`, `hls`, `dash` | any input they open, each through its own demuxer: a declaration of one is a declaration of every input it opens |
  /// | every other | not audited |
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn plaintext_demuxers(&self) -> &[DemuxerName] {
    &self.plaintext_demuxers
  }

  /// Sets [`Self::limits`] (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub fn with_limits(mut self, value: DemuxLimits) -> Self {
    self.limits = value;
    self
  }

  /// Sets [`Self::plaintext_demuxers`] (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub fn with_plaintext_demuxers(mut self, value: Vec<DemuxerName>) -> Self {
    self.plaintext_demuxers = value;
    self
  }

  /// Sets [`Self::limits`] in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_limits(&mut self, value: DemuxLimits) -> &mut Self {
    self.limits = value;
    self
  }

  /// Sets [`Self::plaintext_demuxers`] in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn set_plaintext_demuxers(&mut self, value: Vec<DemuxerName>) -> &mut Self {
    self.plaintext_demuxers = value;
    self
  }
}
