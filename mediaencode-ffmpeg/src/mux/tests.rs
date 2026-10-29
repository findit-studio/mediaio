use core::num::NonZeroI32;

use mediaio_ffmpeg::FfmpegBytes;

use super::*;

fn millis() -> Timebase {
  Timebase::new(1, NonZeroI32::new(1000).expect("non-zero"))
}

/// A packet's times on track 0 at `ms` milliseconds, as a filter's step is
/// handed them.
fn timed_at(ms: i64) -> Timed {
  let at = Timestamp::new(ms, millis());
  Timed {
    track: TrackIndex::new(0),
    pts: Some(at),
    dts: at,
    duration: None,
    pts_ticks: Some(ms),
    dts_ticks: ms,
    duration_ticks: 0,
  }
}

/// PGS's filter mirror over a packet at zero.
fn merge(
  state: PgsState,
  bytes: &[u8],
  side: u64,
  limit: u64,
) -> Result<(PgsState, Step), PayloadRule> {
  pgs_frame_merge(state, timed_at(0), bytes, side, limit)
}

/// The report table's language rule for a timed track of `family`.
fn language(family: Family, tag: &str) -> bool {
  kept(
    family,
    Road::Timed,
    TrackKind::Audio,
    CodecId::AAC,
    TrackDisposition::empty(),
  )
  .language
  .keeps(tag)
}

/// The flags a timed track of `family`, `kind` and `codec` declaring
/// `flags` loses, by the report table.
fn timed_loses(
  family: Family,
  kind: TrackKind,
  codec: CodecId,
  flags: TrackDisposition,
) -> TrackDisposition {
  flags.difference(kept(family, Road::Timed, kind, codec, flags).flags)
}

/// **MP4 keeps a language only where its three letters pack and unpack
/// to themselves**: three bytes in `0x60..=0x7F`, the first not `0x60`
/// and not all three `0x7F`. Matroska and WebM keep any tag but an empty
/// one and `und`, which read back as none; another container is claimed
/// to keep everything.
#[test]
fn each_container_keeps_the_languages_that_read_back_as_themselves() {
  for carried in ["eng", "jpn", "und", "ger", "a``", "z\u{7f}\u{7f}"] {
    assert!(language(Family::Mp4, carried), "{carried:?}");
  }
  for dropped in [
    "en",
    "en-US",
    "engl",
    "ENG",
    "",
    "e1g",
    "`ab",
    "\u{7f}\u{7f}\u{7f}",
    "éng",
  ] {
    assert!(!language(Family::Mp4, dropped), "{dropped:?}");
  }
  for family in [Family::Matroska, Family::WebM] {
    for carried in ["eng", "en-US", "jpn"] {
      assert!(language(family, carried), "{family:?} {carried:?}");
    }
    for dropped in ["", "und"] {
      assert!(!language(family, dropped), "{family:?} {dropped:?}");
    }
  }
  for tag in ["", "und", "en-US"] {
    assert!(language(Family::Other, tag), "{tag:?}");
  }
}

/// **Each container drops the flags it has no field for on a timed
/// stream, and only those** — Matroska's text descriptions on a subtitle
/// track alone.
#[test]
fn each_container_reports_the_disposition_flags_it_drops() {
  use TrackDisposition as D;
  let every = D::DEFAULT | D::FORCED | D::COMMENT | D::KARAOKE | D::ORIGINAL;
  assert_eq!(
    timed_loses(Family::Matroska, TrackKind::Video, CodecId::H264, every),
    D::KARAOKE
  );
  // Original and dub at once: Matroska writes neither.
  assert_eq!(
    timed_loses(
      Family::Matroska,
      TrackKind::Audio,
      CodecId::AAC,
      D::ORIGINAL | D::DUB
    ),
    D::ORIGINAL | D::DUB
  );
  // Text descriptions: a subtitle track's alone.
  assert_eq!(
    timed_loses(
      Family::Matroska,
      TrackKind::Audio,
      CodecId::AAC,
      D::DESCRIPTIONS | D::VISUAL_IMPAIRED
    ),
    D::DESCRIPTIONS
  );
  assert_eq!(
    timed_loses(
      Family::Matroska,
      TrackKind::Subtitle,
      CodecId::SUBRIP,
      D::DESCRIPTIONS
    ),
    D::empty()
  );
  assert_eq!(
    timed_loses(Family::WebM, TrackKind::Video, CodecId::VP9, every),
    D::COMMENT | D::KARAOKE | D::ORIGINAL
  );
  // A WebVTT track's first kind travels in its codec id.
  assert_eq!(
    timed_loses(
      Family::WebM,
      TrackKind::Subtitle,
      CodecId::WEBVTT,
      D::CAPTIONS | D::METADATA
    ),
    D::METADATA
  );
  assert_eq!(
    timed_loses(Family::Mp4, TrackKind::Video, CodecId::H264, every),
    D::KARAOKE | D::ORIGINAL
  );
  // An attached picture on a timed track is dropped, so MP4 does not
  // take the track for a cover.
  assert_eq!(
    timed_loses(
      Family::Mp4,
      TrackKind::Video,
      CodecId::PNG,
      D::ATTACHED_PIC | D::DEFAULT
    ),
    D::ATTACHED_PIC
  );
  // An unnamed bit is no flag any container here writes.
  let unnamed = D::from_bits_retain(1 << 30);
  assert_eq!(
    timed_loses(Family::Matroska, TrackKind::Video, CodecId::H264, unnamed),
    unnamed
  );
  assert_eq!(
    timed_loses(
      Family::Other,
      TrackKind::Video,
      CodecId::H264,
      every | unnamed
    ),
    D::empty(),
    "nothing is claimed for a container this release does not know",
  );
}

/// **An attachment's road keeps fewer fields than a timed stream.** An
/// attachment Matroska writes keeps its filename, MIME type and language
/// but no flag; MP4's cover keeps its attached-picture flag alone.
#[test]
fn an_attachment_road_keeps_what_its_container_writes_for_it() {
  use TrackDisposition as D;
  let attachment = kept(
    Family::Matroska,
    Road::Attachment,
    TrackKind::Attachment,
    CodecId::TTF,
    D::DEFAULT | D::FORCED,
  );
  assert!(attachment.filename && attachment.mime_type);
  assert!(attachment.language.keeps("und") && attachment.language.keeps(""));
  assert_eq!(attachment.flags, D::empty());
  let cover = kept(
    Family::Mp4,
    Road::Cover,
    TrackKind::Attachment,
    CodecId::PNG,
    D::ATTACHED_PIC | D::DEFAULT | D::COMMENT,
  );
  assert!(!cover.filename && !cover.mime_type && !cover.language.keeps("eng"));
  assert_eq!(cover.flags, D::ATTACHED_PIC);
}

/// **Matroska labels an attachment with its codec's MIME type, or
/// `text/plain` for text, and nothing else.**
#[test]
fn matroska_names_an_attachments_mime_type_from_its_codec() {
  assert_eq!(
    matroska_mime_type(CodecId::TTF),
    Some("application/x-truetype-font")
  );
  assert_eq!(matroska_mime_type(CodecId::PNG), Some("image/png"));
  assert_eq!(matroska_mime_type(CodecId::TEXT), Some("text/plain"));
  assert_eq!(matroska_mime_type(CodecId::NONE), None);
  assert!(is_cover_codec(CodecId::PNG) && is_cover_codec(CodecId::MJPEG));
  assert!(!is_cover_codec(CodecId::TTF));
}

/// **A chapter's span is counted in its own time base, and one that
/// starts before zero or runs backwards is refused by name.**
#[test]
fn a_chapter_span_is_counted_in_its_own_ruler_and_judged() {
  let nanos = Timebase::new(1, NonZeroI32::new(1_000_000_000).expect("non-zero"));
  let chapter = Chapter::<Ffmpeg>::new(
    1,
    nanos,
    Timestamp::new(1500, millis()),
    Timestamp::new(3000, millis()),
  );
  assert_eq!(
    chapter_span(0, &chapter).expect("a span"),
    (1_500_000_000, 3_000_000_000)
  );
  let backwards = Chapter::<Ffmpeg>::new(
    2,
    millis(),
    Timestamp::new(30, millis()),
    Timestamp::new(10, millis()),
  );
  assert!(matches!(
    chapter_span(4, &backwards),
    Err(MuxOpenError::ChapterSpan { chapter: 4 })
  ));
  let early = Chapter::<Ffmpeg>::new(
    3,
    millis(),
    Timestamp::new(-1, millis()),
    Timestamp::new(10, millis()),
  );
  assert!(matches!(
    chapter_span(5, &early),
    Err(MuxOpenError::ChapterSpan { chapter: 5 })
  ));
  let titled = chapter.with_title(Some(Utf8Bytes::from("a\0b")));
  assert!(matches!(
    chapter_span(6, &titled),
    Err(MuxOpenError::ChapterTitleWithNul { chapter: 6 })
  ));
}

/// **An ADTS header is its first twelve bits set**, in a packet of more
/// than two bytes, as MP4 and Matroska read one.
#[test]
fn an_adts_header_is_read_as_mp4_and_matroska_read_one() {
  assert!(adts_header(&[0xFF, 0xF1, 0x50]));
  assert!(adts_header(&[0xFF, 0xF9, 0x00, 0x00]));
  assert!(!adts_header(&[0xFF, 0xF1]), "two bytes are not enough");
  assert!(!adts_header(&[0x21, 0x10, 0x05]), "raw AAC");
  assert!(!adts_header(&[0xFF, 0xE1, 0x00]));
}

/// **OBUs parse as libavcodec's `parse_obu_header` parses them**: a
/// header byte with its forbidden bit clear, an extension byte where
/// flagged, a LEB128 size where flagged — else the rest of the packet —
/// and a payload inside the packet, OBU after OBU.
#[test]
fn obus_parse_as_libavcodec_parses_them() {
  // A temporal delimiter (type 2, sized, 0 bytes), then a frame OBU
  // (type 6, sized, 3 bytes).
  assert!(obus_parse(&[0x12, 0x00, 0x32, 0x03, 1, 2, 3]));
  // An OBU without a size runs to the packet's end.
  assert!(obus_parse(&[0x30, 1, 2, 3, 4]));
  // With an extension byte.
  assert!(obus_parse(&[0x36, 0x08, 0x02, 9, 9]));
  // A two-byte LEB128 size: 128.
  let mut long = vec![0x32, 0x80, 0x01];
  long.extend([7u8; 128]);
  assert!(obus_parse(&long));
  assert!(obus_parse(&[]), "nothing to parse");
  // The forbidden bit, a size past the packet, a header cut short, a
  // LEB128 running past the packet.
  assert!(!obus_parse(&[0x92, 0x00]));
  assert!(!obus_parse(&[0x32, 0x05, 1, 2]));
  assert!(!obus_parse(&[0x36]));
  assert!(!obus_parse(&[0x32, 0x80, 0x80]));
  assert!(!obus_parse(&[0x36, 0x00, 0x80, 0x80]));
  // A LEB128 is read for eight bytes at most (`leb.h` 57–68): eight with
  // their continuation bits set end it, a size of nothing, and the ten
  // bytes are a whole header.
  assert!(obus_parse(&[
    0x36, 0x00, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x00
  ]));
}

/// A WavPack block of `payload` bytes after its 32-byte header.
fn wavpack_block(payload: usize) -> Vec<u8> {
  let mut block = b"wvpk".to_vec();
  block.extend(u32::try_from(payload + 24).expect("small").to_le_bytes());
  block.extend([0u8; 24]);
  block.extend(vec![5u8; payload]);
  block
}

/// **WavPack blocks parse as Matroska's reformatter parses them, whole**:
/// each tagged `wvpk`, its size in 24..=2^20, and nothing left over.
#[test]
fn wavpack_blocks_parse_whole() {
  let mut two = wavpack_block(10);
  two.extend(wavpack_block(0));
  assert!(wavpack_parse(&two));
  assert!(wavpack_parse(&[]));
  let mut over = wavpack_block(10);
  over.extend([1, 2, 3]);
  assert!(
    !wavpack_parse(&over),
    "bytes left over after the last block"
  );
  let mut short = wavpack_block(10);
  short.truncate(40);
  assert!(!wavpack_parse(&short), "a block cut short");
  let mut tag = wavpack_block(4);
  tag[0] = b'W';
  assert!(!wavpack_parse(&tag));
  let mut size = wavpack_block(4);
  size[4..8].copy_from_slice(&23u32.to_le_bytes());
  assert!(!wavpack_parse(&size), "a size below the header's");
}

/// **Skip-samples side data shortens an MP4 sample as MP4 shortens it**:
/// the longer of a frame and the packet's duration, less the samples to
/// drop from its end, each rounded to the nearest tick.
#[test]
fn skip_samples_shorten_an_mp4_sample_as_mp4_does() {
  let side = |end: u32| {
    let mut bytes = 0u32.to_le_bytes().to_vec();
    bytes.extend(end.to_le_bytes());
    bytes.extend([0, 0]);
    vec![SideDataEntry::new(
      AVPacketSideDataType::AV_PKT_DATA_SKIP_SAMPLES as i32,
      FfmpegBytes::try_copy_from_slice(&bytes).expect("a copy"),
    )]
  };
  // AAC at 48 kHz, in milliseconds: a frame is 21 ms.
  let aac = (1024, 48_000);
  assert!(!skips_below_nothing(&side(1024), 0, aac, millis()));
  assert!(skips_below_nothing(&side(2048), 0, aac, millis()));
  assert!(!skips_below_nothing(&side(2048), 43, aac, millis()));
  assert!(!skips_below_nothing(&[], 0, aac, millis()), "no side data");
  let short = vec![SideDataEntry::new(
    AVPacketSideDataType::AV_PKT_DATA_SKIP_SAMPLES as i32,
    FfmpegBytes::try_copy_from_slice(&[0; 9]).expect("a copy"),
  )];
  assert!(
    !skips_below_nothing(&short, 0, aac, millis()),
    "under 10 bytes"
  );
}

/// **Rescaling rounds to the nearest, half away from zero**, as
/// `av_rescale_q` does.
#[test]
fn a_rescale_rounds_as_av_rescale_q_does() {
  // 1024 samples at 48 kHz: 21.33 ms.
  assert_eq!(rescale_nearest(1024, 1000, 48_000), 21);
  // 312 samples at 48 kHz: 6.5 ms, half away from zero.
  assert_eq!(rescale_nearest(312, 1000, 48_000), 7);
  assert_eq!(rescale_nearest(0, 1000, 48_000), 0);
}

/// **MP4 configures from the extradata a stream will hold**: H.264 with
/// none is refused, with an `avcC` written as offered, with an Annex B
/// configuration rewritten as length prefixes — in Matroska too, which
/// reads Annex B by its start code.
#[test]
fn a_configuration_is_judged_and_its_rewrite_reported() {
  let rewrites_of = |family, _kind: TrackKind, codec, extradata: &[u8]| {
    rewrites(family, codec, extradata, true).collect::<Vec<_>>()
  };
  assert!(configuration_missing(Family::Mp4, CodecId::H264, &[], true));
  assert!(configuration_missing(Family::Mp4, CodecId::HEVC, &[], true));
  assert!(!configuration_missing(
    Family::Mp4,
    CodecId::H264,
    &[1, 2],
    true
  ));
  assert!(configuration_missing(
    Family::Matroska,
    CodecId::AV1,
    &[],
    true
  ));
  assert!(configuration_missing(Family::WebM, CodecId::AV1, &[], true));
  assert!(!configuration_missing(
    Family::Other,
    CodecId::H264,
    &[],
    true
  ));
  // AAC's comes from its first ADTS header with the automatic filters on;
  // with them off MP4 and Matroska need it stated.
  for family in [Family::Mp4, Family::Matroska] {
    assert!(
      configuration_missing(family, CodecId::AAC, &[], false),
      "{family:?}"
    );
    assert!(
      !configuration_missing(family, CodecId::AAC, &[], true),
      "{family:?}"
    );
    assert!(
      !configuration_missing(family, CodecId::AAC, &[0x11, 0x90], false),
      "{family:?}"
    );
  }
  let avcc = [1, 0x64, 0, 0x1F];
  let annex = [0, 0, 0, 1, 0x67];
  assert_eq!(
    rewrites_of(Family::Mp4, TrackKind::Video, CodecId::H264, &avcc),
    vec![]
  );
  assert_eq!(
    rewrites_of(Family::Mp4, TrackKind::Video, CodecId::H264, &annex),
    vec![Rewrite::LengthPrefixed]
  );
  assert_eq!(
    rewrites_of(Family::Matroska, TrackKind::Video, CodecId::H264, &annex),
    vec![Rewrite::LengthPrefixed]
  );
  assert_eq!(
    rewrites_of(Family::Matroska, TrackKind::Video, CodecId::H264, &avcc),
    vec![]
  );
  // HEVC needs more than six bytes.
  assert_eq!(
    rewrites_of(Family::Mp4, TrackKind::Video, CodecId::HEVC, &annex),
    vec![]
  );
  assert_eq!(
    rewrites_of(
      Family::Mp4,
      TrackKind::Video,
      CodecId::HEVC,
      &[0, 0, 1, 0x40, 1, 2, 3]
    ),
    vec![Rewrite::LengthPrefixed]
  );
  assert_eq!(
    rewrites_of(Family::WebM, TrackKind::Video, CodecId::AV1, &[0x81]),
    vec![Rewrite::ObusFiltered]
  );
  assert_eq!(
    rewrites_of(Family::Mp4, TrackKind::Subtitle, CodecId::MOV_TEXT, &[]),
    vec![Rewrite::CueEnds]
  );
  assert_eq!(
    rewrites_of(
      Family::Mp4,
      TrackKind::Subtitle,
      CodecId::TTML,
      b"lavc-ttmlenc"
    ),
    vec![Rewrite::ParagraphsJoined]
  );
  assert_eq!(
    rewrites_of(Family::Mp4, TrackKind::Audio, CodecId::EAC3, &[]),
    vec![Rewrite::Ac3Parsed]
  );
  assert_eq!(
    rewrites_of(Family::Matroska, TrackKind::Audio, CodecId::WAVPACK, &[]),
    vec![Rewrite::WavPackHeaders]
  );
  assert_eq!(
    rewrites_of(Family::Other, TrackKind::Video, CodecId::H264, &annex),
    vec![],
    "nothing is claimed for a container this release does not know",
  );
}

/// **The durations a container does not keep are reported at open**:
/// Matroska's and WebM's of a video or audio track with no default
/// duration. MP4's are reported while writing, where the packets do not
/// abut, so none is at open; APV's size prefix is not, as MP4's reader
/// takes it back off.
#[test]
fn a_duration_the_container_does_not_keep_is_reported() {
  let rewrites_of = |family, _kind: TrackKind, codec, default_duration| {
    rewrites(family, codec, &[], default_duration).collect::<Vec<_>>()
  };
  for family in [Family::Matroska, Family::WebM] {
    assert_eq!(
      rewrites_of(family, TrackKind::Video, CodecId::VP9, false),
      vec![Rewrite::Durations],
      "{family:?}"
    );
    assert_eq!(
      rewrites_of(family, TrackKind::Audio, CodecId::OPUS, true),
      vec![],
      "{family:?}"
    );
  }
  assert_eq!(
    rewrites_of(Family::Mp4, TrackKind::Video, CodecId::H264, false),
    vec![],
    "MP4 keeps a video packet's duration as the distance to the next"
  );
  assert_eq!(
    rewrites_of(Family::Mp4, TrackKind::Video, CodecId::APV, true),
    vec![]
  );
  assert!(configuration_missing(Family::Mp4, CodecId::EVC, &[], true));
  assert!(!configuration_missing(Family::Mp4, CodecId::VP9, &[], true));
}

/// A ProRes frame of `len` bytes, opening on its atom.
fn prores_frame(len: usize) -> Vec<u8> {
  let mut frame = u32::try_from(len).expect("small").to_be_bytes().to_vec();
  frame.extend(b"icpf");
  frame.resize(len, 7);
  frame
}

/// **A ProRes packet reads back from Matroska only where it opens on its
/// own atom**: the packet's size and `icpf`, with no second atom after it.
#[test]
fn a_prores_packet_reads_back_from_matroska_only_on_its_own_atom() {
  assert!(prores_atom(&prores_frame(64)));
  assert!(prores_atom(&prores_frame(8)));
  let mut wrong_size = prores_frame(64);
  wrong_size[3] = 63;
  assert!(!prores_atom(&wrong_size));
  let mut wrong_tag = prores_frame(64);
  wrong_tag[4] = b'x';
  assert!(!prores_atom(&wrong_tag));
  assert!(!prores_atom(&prores_frame(64)[..7]), "shorter than an atom");
  let mut twice = prores_frame(64);
  twice[12..16].copy_from_slice(b"icpf");
  assert!(!prores_atom(&twice), "a second atom after the first");
}

/// **A WebVTT cue reads back from Matroska's lines only where none of its
/// parts breaks a line it should not**: an identifier and settings each a
/// line, not empty, and a text that does not end in a line break.
#[test]
fn a_webvtt_cue_reads_back_from_its_lines_as_itself() {
  let entry = |kind: AVPacketSideDataType, text: &[u8]| {
    SideDataEntry::new(
      kind as i32,
      FfmpegBytes::try_copy_from_slice(text).expect("a copy"),
    )
  };
  let id = |text: &[u8]| entry(AVPacketSideDataType::AV_PKT_DATA_WEBVTT_IDENTIFIER, text);
  let settings = |text: &[u8]| entry(AVPacketSideDataType::AV_PKT_DATA_WEBVTT_SETTINGS, text);
  assert!(webvtt_cue(b"hello", &[]));
  assert!(webvtt_cue(b"two\nlines", &[id(b"c1"), settings(b"line:0")]));
  assert!(webvtt_cue(b"", &[]), "an empty cue");
  assert!(
    !webvtt_cue(b"hello\n", &[]),
    "a text ending in a line break"
  );
  assert!(!webvtt_cue(b"hello\r", &[]));
  assert!(!webvtt_cue(b"hello", &[id(b"c\n1")]));
  assert!(!webvtt_cue(b"hello", &[settings(b"a\rb")]));
  assert!(
    !webvtt_cue(b"hello", &[id(b"")]),
    "an empty identifier given"
  );
}

/// An AC-3 frame header: bit stream id 8, 48 kHz, frame size code 14 —
/// 448 bytes.
const AC3: [u8; 8] = [0x0B, 0x77, 0, 0, 0x0E, 0x40, 0, 0];

/// An E-AC-3 frame header of `blocks_code` and `kind` — 0 independent, 1
/// dependent — substream 0, 768 bytes, stereo, bit stream id 16.
fn eac3(kind: u8, blocks_code: u8) -> Vec<u8> {
  vec![
    0x0B,
    0x77,
    (kind << 6) | 0x01,
    0x7F,
    (blocks_code << 4) | 0x04,
    0x80,
    0,
    0,
  ]
}

/// `header` padded to the frame size it states: `size` bytes.
fn frame(header: &[u8], size: usize) -> Vec<u8> {
  let mut frame = header.to_vec();
  frame.resize(size, 0);
  frame
}

/// **AC-3 and E-AC-3 headers parse as libavcodec's parser parses them**:
/// its sync word and bit stream id, AC-3's sample rate and frame size
/// codes, E-AC-3's frame type, size, sample rates, substream and a
/// dependent frame's channel map — reading zeros past the bytes it has.
#[test]
fn ac3_headers_parse_as_libavcodec_parses_them() {
  assert_eq!(
    ac3_header(&AC3),
    Some(Ac3Header {
      bsid: 8,
      frame_type: 2,
      size: 448,
      blocks: 6
    })
  );
  assert_eq!(
    ac3_header(&eac3(0, 3)),
    Some(Ac3Header {
      bsid: 16,
      frame_type: 0,
      size: 768,
      blocks: 6
    })
  );
  assert_eq!(ac3_header(&eac3(0, 1)).map(|header| header.blocks), Some(2));
  // Two bytes of sync and zeros past them: an AC-3 frame of 128 bytes.
  assert_eq!(
    ac3_header(&[0x0B, 0x77]).map(|header| header.size),
    Some(128)
  );
  let mut no_sync = AC3;
  no_sync[0] = 0x0C;
  assert_eq!(ac3_header(&no_sync), None);
  let mut bsid = AC3;
  bsid[5] = 17 << 3;
  assert_eq!(ac3_header(&bsid), None, "a bit stream id past 16");
  let mut rate = AC3;
  rate[4] = 0xC0 | 0x0E;
  assert_eq!(ac3_header(&rate), None, "AC-3's reserved sample rate");
  let mut size = AC3;
  size[4] = 38;
  assert_eq!(ac3_header(&size), None, "a frame size code past 37");
  let mut reserved = eac3(0, 3);
  reserved[2] |= 0xC0;
  assert_eq!(ac3_header(&reserved), None, "E-AC-3's reserved frame type");
  let mut substream = eac3(0, 3);
  substream[2] |= 1 << 3;
  assert_eq!(
    ac3_header(&substream),
    None,
    "a substream other than the first"
  );
  let mut tiny = eac3(0, 3);
  tiny[2..4].copy_from_slice(&[0x00, 0x02]);
  assert_eq!(ac3_header(&tiny), None, "a frame of 6 bytes");
  let mut rates = eac3(0, 3);
  rates[4] = 0xF4;
  assert_eq!(ac3_header(&rates), None, "both sample rate codes reserved");
  // A dependent frame's channel map: 22 channels is past 16; two is not.
  let mut wide = eac3(1, 3);
  wide.truncate(6);
  wide.extend([0x1F, 0xFF, 0xF0]);
  assert_eq!(ac3_header(&wide), None);
  let mut pair = eac3(1, 3);
  pair.truncate(6);
  pair.extend([0x1A, 0x00, 0x00]);
  assert_eq!(ac3_header(&pair).map(|header| header.frame_type), Some(1));
}

/// An AC-3 or E-AC-3 packet of `bytes` at decode time `dts`, of `duration`
/// ticks as offered and handed to MP4, without skip samples.
fn ac3_at(bytes: &[u8], dts: i64, duration: i64) -> Ac3Packet<'_> {
  Ac3Packet {
    bytes,
    dts,
    duration,
    offered: (duration > 0).then_some(duration),
    skip_end: None,
  }
}

/// What MP4 learns of the track from `bytes`, at zero, of no duration, the
/// sample it joins unbounded.
fn take(state: Ac3State, eac3: bool, bytes: &[u8]) -> Result<Ac3State, PayloadRule> {
  ac3_packet(state, eac3, ac3_at(bytes, 0, 0), u64::MAX >> 1).map(|(state, _)| state)
}

/// **MP4's AC-3 handling is held as MP4 holds it**: a header that does not
/// parse; an E-AC-3 track's first packet tiled by dependent frames; a
/// second independent frame ending the first set; frames of fewer than six
/// blocks joined into sixes, and none past.
#[test]
fn mp4s_ac3_handling_is_mirrored() {
  let start = Ac3State::default();
  let alone = Some(Ac3Sample {
    duration: 0,
    closing: 0,
  });
  let ac3 = frame(&AC3, 448);
  let learned = take(start, false, &ac3).expect("an AC-3 frame");
  assert_eq!(
    learned,
    Ac3State {
      done: true,
      bsid: 8,
      run: None,
      last: alone,
    }
  );
  assert_eq!(
    take(learned, false, &[1, 2, 3]),
    Err(PayloadRule::Ac3Header)
  );
  // E-AC-3: the first packet, alone or with a dependent frame after it.
  let independent = frame(&eac3(0, 3), 768);
  let first = take(start, true, &independent).expect("one frame");
  assert_eq!(
    first,
    Ac3State {
      done: false,
      bsid: 16,
      run: None,
      last: alone,
    }
  );
  let mut with_dependent = independent.clone();
  with_dependent.extend(frame(&eac3(1, 3), 768));
  assert!(take(start, true, &with_dependent).is_ok());
  let mut with_independent = independent.clone();
  with_independent.extend(frame(&eac3(0, 3), 768));
  assert_eq!(
    take(start, true, &with_independent),
    Err(PayloadRule::Ac3Substreams)
  );
  let mut left_over = independent.clone();
  left_over.extend([1, 2, 3]);
  assert_eq!(
    take(start, true, &left_over),
    Err(PayloadRule::Ac3Substreams)
  );
  // The second independent frame ends the first set: no tiling is asked
  // of it.
  let second = take(first, true, &left_over).expect("the second packet");
  assert!(second.done);
  // Two-block frames join into sixes; a three-block frame after four
  // blocks, or a six-block frame after two, runs past.
  let two = frame(&eac3(0, 1), 768);
  let three = frame(&eac3(0, 2), 768);
  let mut state = second;
  for held in [2, 4, 0] {
    state = take(state, true, &two).expect("two blocks");
    assert_eq!(state.run.map_or(0, |run| run.blocks), held);
  }
  let four = take(take(state, true, &two).expect("two"), true, &two).expect("four");
  assert_eq!(take(four, true, &three), Err(PayloadRule::Ac3Blocks));
  let held = take(state, true, &two).expect("two");
  assert_eq!(take(held, true, &independent), Err(PayloadRule::Ac3Blocks));
}

/// The kind of a track of `codec`, as the laws' tracks are: audio for AAC
/// and PCM, a subtitle for PGS and EIA-608, else video.
fn kind_of(codec: CodecId) -> TrackKind {
  if codec == CodecId::AAC || codec == CodecId::PCM_S16LE {
    TrackKind::Audio
  } else if codec == CodecId::HDMV_PGS_SUBTITLE || codec == EIA_608 {
    TrackKind::Subtitle
  } else {
    TrackKind::Video
  }
}

/// **Every filter table B names is settled where its muxer inserts it, and
/// named for the track where open decides it** (E″ of the census): every
/// `check_bitstream` answers 1 or the insertion's answer, so libavformat
/// decides a muxer's filters at a track's first packet, never later
/// (`mux.c` 1063–1068); a muxer's start inserts the rest whatever the
/// option says. `pcm_rechunk` in DAUD, GXF and the three MXF muxers;
/// `av1_metadata` in IVF at its start and in OBU at the first packet;
/// `eia608_to_smpte436m` in MCC and the MXF muxers; `extract_extradata` in
/// FLV where the track states no configuration; the Annex B conversions,
/// undecided until the first packet, in MPEG-TS — and RTP MPEG-TS's nested
/// one, the option aside — in MXF and MXF OP-Atom for H.264, and in each
/// raw muxer for its codec.
#[test]
fn every_filter_table_b_names_is_settled_where_its_muxer_inserts_it() {
  use AutomaticFilter as A;
  let settle = |muxer: &str, codec, extradata: &[u8], on| {
    Filter::settle(
      Sites::of(muxer, None),
      Family::of(muxer),
      (kind_of(codec), codec),
      extradata,
      on,
    )
  };
  let named =
    |muxer: &str, codec, extradata: &[u8], on| settle(muxer, codec, extradata, on).named();
  for on in [true, false] {
    for muxer in ["daud", "gxf", "mxf", "mxf_d10", "mxf_opatom"] {
      assert_eq!(
        named(muxer, CodecId::PCM_S16LE, &[], on),
        Some(A::PcmRechunk),
        "{muxer}"
      );
    }
    for muxer in ["mcc", "mxf", "mxf_d10", "mxf_opatom"] {
      assert_eq!(
        named(muxer, EIA_608, &[], on),
        Some(A::Eia608ToSmpte436m),
        "{muxer}"
      );
    }
    assert_eq!(named("ivf", CodecId::AV1, &[], on), Some(A::Av1Metadata));
  }
  assert_eq!(named("wav", CodecId::PCM_S16LE, &[], true), None);
  assert_eq!(named("mov", EIA_608, &[], true), None);
  assert_eq!(named("obu", CodecId::AV1, &[], true), Some(A::Av1Metadata));
  assert_eq!(named("obu", CodecId::AV1, &[], false), None);
  assert_eq!(named("mp4", CodecId::AV1, &[1], true), None);
  for codec in [
    CodecId::H264,
    CodecId::HEVC,
    CodecId::VVC,
    CodecId::AV1,
    CodecId::MPEG4,
  ] {
    assert_eq!(
      named("flv", codec, &[], true),
      Some(A::ExtractExtradata),
      "{codec:?}"
    );
    assert_eq!(
      named("flv", codec, &[1, 2], true),
      None,
      "{codec:?}: configured"
    );
    assert_eq!(named("flv", codec, &[], false), None, "{codec:?}: off");
  }
  let undecided = |filter, configured| Filter::AnnexBUndecided { filter, configured };
  for (codec, filter) in [
    (CodecId::H264, A::H264Mp4ToAnnexB),
    (CodecId::HEVC, A::HevcMp4ToAnnexB),
    (CodecId::VVC, A::VvcMp4ToAnnexB),
  ] {
    assert_eq!(settle("mpegts", codec, &[], true), undecided(filter, false));
    assert_eq!(settle("mpegts", codec, &[], false), Filter::None);
    assert_eq!(
      settle("rtp_mpegts", codec, &[], false),
      undecided(filter, false),
      "nested"
    );
    let raw = match codec {
      CodecId::H264 => "h264",
      CodecId::HEVC => "hevc",
      _ => "vvc",
    };
    assert_eq!(
      settle(raw, codec, &[1], true),
      undecided(filter, false),
      "{raw}"
    );
    assert_eq!(
      settle(raw, codec, &[], true).named(),
      None,
      "{raw}: undecided at open"
    );
  }
  // MPEG-TS asks the configuration's first byte: `configurationVersion` 1,
  // or VVC's reserved `11111`.
  assert_eq!(
    settle("mpegts", CodecId::H264, &[1, 0x64], true),
    undecided(A::H264Mp4ToAnnexB, true)
  );
  assert_eq!(
    settle("mpegts", CodecId::H264, &[0, 0, 0, 1], true),
    undecided(A::H264Mp4ToAnnexB, false)
  );
  assert_eq!(
    settle("mpegts", CodecId::VVC, &[0xFF], true),
    undecided(A::VvcMp4ToAnnexB, true)
  );
  assert_eq!(
    settle("mpegts", CodecId::VVC, &[0x01], true),
    undecided(A::VvcMp4ToAnnexB, false)
  );
  for muxer in ["mxf", "mxf_opatom"] {
    assert_eq!(
      settle(muxer, CodecId::H264, &[1], true),
      undecided(A::H264Mp4ToAnnexB, false),
      "{muxer}"
    );
    assert_eq!(
      settle(muxer, CodecId::HEVC, &[], true),
      Filter::None,
      "{muxer}"
    );
  }
  assert_eq!(
    settle("mxf_d10", CodecId::H264, &[], true),
    Filter::None,
    "no check_bitstream"
  );
  assert_eq!(A::PcmRechunk.name(), "pcm_rechunk");
  assert_eq!(A::Av1Metadata.name(), "av1_metadata");
  assert_eq!(A::Eia608ToSmpte436m.name(), "eia608_to_smpte436m");
  assert_eq!(A::ExtractExtradata.name(), "extract_extradata");
  assert_eq!(A::H264Mp4ToAnnexB.name(), "h264_mp4toannexb");
  assert_eq!(A::HevcMp4ToAnnexB.name(), "hevc_mp4toannexb");
  assert_eq!(A::VvcMp4ToAnnexB.name(), "vvc_mp4toannexb");
}

/// **A filter the first packet decides is inserted, and named, at the
/// packet that decides it, and never after** (E″ of the census): AAC's at
/// an ADTS header of more than two bytes (`flvenc.c` 1491–1493;
/// `latmenc.c` 255–257); the Annex B conversions at a packet of five bytes
/// or more opening on no 4-byte start code, nor on a 3-byte one but where
/// MPEG-TS finds the configuration length-prefixed (`mpegtsenc.c`
/// 2326–2331). A packet that inserts none leaves none to insert.
///
/// PLANT: without `Step::inserted`, the write that inserts the filter
/// names none.
#[test]
fn a_first_packet_filter_is_named_at_the_packet_that_inserts_it() {
  use AutomaticFilter as A;
  let adts = adts(16, false, 3, 2, 0, 0x11);
  let raw = [0x21, 0x10, 0x05];
  let undecided = Filter::AdtsUndecided {
    configured: false,
    parses: false,
    whole: false,
  };
  let (after, step) = undecided
    .step(timed_at(0), &adts, 0, PACKET_BYTES_MAX)
    .expect("converted");
  assert_eq!(step.inserted, Some(A::AacAdtsToAsc));
  assert_eq!(after.named(), Some(A::AacAdtsToAsc));
  let (_, step) = after
    .step(timed_at(0), &adts, 0, PACKET_BYTES_MAX)
    .expect("converted");
  assert_eq!(step.inserted, None, "once");
  let (after, step) = undecided
    .step(timed_at(0), &raw, 0, PACKET_BYTES_MAX)
    .expect("passed");
  assert_eq!((after, step.inserted), (Filter::None, None));
  let annexb = |configured| Filter::AnnexBUndecided {
    filter: A::H264Mp4ToAnnexB,
    configured,
  };
  let length_prefixed = [0, 0, 0, 2, 0x65, 0x88];
  for (bytes, configured, inserted) in [
    (&length_prefixed[..], false, true),
    (&[0, 0, 0, 1, 0x65, 0x88][..], false, false),
    (&[0, 0, 1, 0x65, 0x88][..], false, false),
    (&[0, 0, 1, 0x65, 0x88][..], true, true),
    (&[0, 0, 0, 2][..], false, false),
  ] {
    let (after, step) = annexb(configured)
      .step(timed_at(0), bytes, 0, PACKET_BYTES_MAX)
      .expect("passed");
    assert_eq!(
      step.inserted,
      inserted.then_some(A::H264Mp4ToAnnexB),
      "{bytes:?}, configured {configured}"
    );
    assert_eq!(
      after,
      if inserted {
        Filter::Unmirrored(A::H264Mp4ToAnnexB)
      } else {
        Filter::None
      }
    );
    assert!(step.owns, "the filter holds nothing");
  }
  let (after, _) = annexb(false)
    .step(timed_at(0), &length_prefixed, 0, PACKET_BYTES_MAX)
    .expect("inserted");
  let (_, step) = after
    .step(timed_at(0), &length_prefixed, 0, PACKET_BYTES_MAX)
    .expect("passed");
  assert_eq!(step.inserted, None, "once");
}

/// **MP4 joins E-AC-3 frames of fewer than six blocks into the sample it
/// writes when they make six, and writes nothing before** (`handle_eac3`,
/// `movenc.c` 577–606; table C″ of the census): the sample takes its first
/// packet's decode time and skip samples, its packets' durations summed and
/// their bytes joined — within `av_grow_packet`'s bound, a byte past the
/// largest packet — and its last packet's own duration ends the track's
/// edit list where it is the last. A frame of six blocks is a sample of its
/// own.
#[test]
fn mp4_joins_e_ac3s_short_frames_into_the_sample_it_writes() {
  let one = frame(&eac3(0, 0), 768);
  let six = frame(&eac3(0, 3), 768);
  let mut state = take(Ac3State::default(), true, &six).expect("the first set");
  let (alone, step) = ac3_packet(state, true, ac3_at(&six, 10, 6), 1 << 20).expect("six");
  assert_eq!(step, Ac3Step::Alone);
  assert_eq!(
    alone.last,
    Some(Ac3Sample {
      duration: 6,
      closing: 6,
    })
  );
  state = alone;
  let mut skipped = ac3_at(&one, 100, 1);
  skipped.skip_end = Some(2000);
  let (opened, step) = ac3_packet(state, true, skipped, 1 << 20).expect("one block");
  assert_eq!(step, Ac3Step::Opens);
  assert_eq!(opened.last, alone.last, "nothing written");
  state = opened;
  for at in 101..105 {
    let (joined, step) = ac3_packet(state, true, ac3_at(&one, at, 2), 1 << 20).expect("one");
    assert_eq!(step, Ac3Step::Joins);
    assert_eq!(joined.last, alone.last, "nothing written");
    state = joined;
  }
  let (completed, step) = ac3_packet(state, true, ac3_at(&one, 105, 3), 1 << 20).expect("six");
  let run = Ac3Run {
    blocks: 6,
    dts: 100,
    duration: 1 + 4 * 2 + 3,
    offered: Some(12),
    bytes: 6 * 768,
    skip_end: Some(2000),
  };
  assert_eq!(step, Ac3Step::Completes(run));
  assert_eq!(completed.run, None);
  assert_eq!(
    completed.last,
    Some(Ac3Sample {
      duration: 12,
      closing: 3,
    })
  );
  // A packet offering no duration leaves the sum's offer unknown.
  let (opened, _) = ac3_packet(completed, true, ac3_at(&one, 200, 0), 1 << 20).expect("one");
  assert_eq!(opened.run.and_then(|run| run.offered), None);
  // The bytes: five frames of 768 joined, a sixth taking them to the bound
  // and a byte past it, then a byte more.
  let mut state = completed;
  for at in 300..305 {
    state = ac3_packet(state, true, ac3_at(&one, at, 1), 6 * 768 - 1)
      .expect("within the bound")
      .0;
  }
  assert!(ac3_packet(state, true, ac3_at(&one, 305, 1), 6 * 768 - 1).is_ok());
  assert_eq!(
    ac3_packet(state, true, ac3_at(&one, 305, 1), 6 * 768 - 2).map(|(_, step)| step),
    Err(PayloadRule::Ac3SampleSize)
  );
}

/// An MP4 E-AC-3 track at 48 kHz, counted in its samples, with a frame of
/// `frame_size` samples stated — which MP4 reads skip samples by (`movenc.c`
/// 7311–7312), and which no demuxer's ticket of AC-3 or E-AC-3 states:
/// libavcodec's parser and decoder set none, and `avformat_find_stream_info`
/// asks it of MPEG audio and Codec 2 alone (`demux.c` 2076–2087).
fn eac3_track(frame_size: i32) -> TrackState {
  let time_base = Timebase::new(1, NonZeroI32::new(48_000).expect("non-zero"));
  TrackState {
    kind: TrackKind::Audio,
    stream: Some((0, time_base)),
    strict: true,
    last: None,
    last_ticks: None,
    handed: None,
    pending: Vec::new(),
    keyed: false,
    sample: None,
    rules: PacketRules {
      mp4: true,
      no_empty: true,
      skip_samples: (frame_size != 0).then_some((frame_size, 48_000)),
      ac3: Some(Ac3Rules {
        eac3: true,
        guessed: if frame_size > 1 {
          i64::from(frame_size)
        } else {
          0
        },
        durations_read: frame_size != 1,
      }),
      ..PacketRules::default()
    },
    filter: Filter::None,
    sides: SideRules {
      family: Family::Mp4,
      kind: TrackKind::Audio,
      codec: CodecId::EAC3,
      sample_rate: 48_000,
      frame_size,
      initial_padding: 0,
      extradata: Vec::new(),
    },
    written: false,
    cues: Cues::default(),
    ac3: Ac3State::default(),
    document: 0,
  }
}

/// Skip samples dropping `end` samples from a packet's end.
fn skipping(end: u32) -> Vec<SideDataEntry> {
  let mut bytes = [0u8; 10];
  bytes[4..8].copy_from_slice(&end.to_le_bytes());
  vec![SideDataEntry::new(
    AVPacketSideDataType::AV_PKT_DATA_SKIP_SAMPLES as i32,
    FfmpegBytes::try_copy_from_slice(&bytes).expect("a copy"),
  )]
}

/// Holds an E-AC-3 packet of `bytes` and `side`, at `dts` of `duration`
/// ticks, to `state`'s rules, and settles what MP4 learns from it as the
/// muxer does: the rules' answer, and the report's lines.
fn admit_eac3(
  state: &mut TrackState,
  bytes: &[u8],
  side: &[SideDataEntry],
  (dts, duration): (i64, i64),
) -> Result<Vec<Uncarried>, FfmpegMuxRefusal> {
  let (_, time_base) = state.stream.expect("a stream");
  let track = TrackIndex::new(0);
  let at = |ticks: i64| Timestamp::new(ticks, time_base);
  let payload = Payload {
    bytes,
    key: true,
    flags: PacketFlags::KEY,
    side_data: side,
  };
  let timed = Timed {
    track,
    pts: Some(at(dts)),
    dts: at(dts),
    duration: Some(at(duration)),
    pts_ticks: Some(dts),
    dts_ticks: dts,
    duration_ticks: duration,
  };
  let step = Step {
    owns: true,
    emits: Emits::OWN,
    largest: Some(bytes.len() as u64),
    held: None,
    inserted: None,
  };
  let learned = check_packet_rules(
    state,
    time_base,
    Neighbours::default(),
    (timed, &[timed]),
    payload,
    step,
    Bounds::REAL,
  )?;
  let lines = learned_losses(state, track, &[], payload, &learned, true);
  if let Some(sample) = learned.sample {
    state.sample = Some(sample);
  }
  if let Some(ac3) = learned.ac3 {
    state.ac3 = ac3;
  }
  state.written = true;
  Ok(lines)
}

/// **MP4 judges a joined E-AC-3 sample's skip samples at the packet that
/// completes it, against that packet's own duration, and drops a joined
/// packet's own** (table C″ of the census): the sample takes its first
/// packet's side data (`handle_eac3`, `movenc.c` 586–603), and MP4 reads
/// its skip samples once it writes it, its end counted with the duration of
/// the packet it was handed last (6940, 7311–7320). With a frame of 1536
/// samples stated, a first packet dropping 2000 — past its duration and
/// its frame — is taken, its skip samples reported as possibly dropped, as
/// on any sample; a joined packet's skip samples are reported dropped; a
/// completing packet of one tick is refused, one of 2000 taken.
///
/// PLANT: the skip samples judged at every packet with its own, as before:
/// the first packet is refused.
#[test]
fn a_joined_samples_skip_samples_are_judged_at_its_last_packet() {
  use SideDataFate::{Dropped, PossiblyDropped};
  let one = frame(&eac3(0, 0), 768);
  let mut state = eac3_track(1536);
  let line = |fate| Uncarried::SideData {
    track: TrackIndex::new(0),
    kind: AVPacketSideDataType::AV_PKT_DATA_SKIP_SAMPLES as i32,
    name: packet_side_data_name(AVPacketSideDataType::AV_PKT_DATA_SKIP_SAMPLES as i32),
    fate,
  };
  assert_eq!(
    admit_eac3(&mut state, &one, &skipping(2000), (0, 1)),
    Ok(vec![line(PossiblyDropped)]),
    "held: its skip samples ride on the sample"
  );
  assert_eq!(
    admit_eac3(&mut state, &one, &skipping(10), (1, 1)),
    Ok(vec![line(Dropped)]),
    "joined: never read"
  );
  for dts in 2..5 {
    assert_eq!(admit_eac3(&mut state, &one, &[], (dts, 1)), Ok(vec![]));
  }
  let before = state.clone();
  assert_eq!(
    admit_eac3(&mut state, &one, &[], (5, 1)),
    Err(FfmpegMuxRefusal::Payload {
      track: TrackIndex::new(0),
      rule: PayloadRule::SkipSamples
    })
  );
  assert_eq!(state.ac3, before.ac3, "nothing learned of a refused packet");
  assert_eq!(admit_eac3(&mut state, &one, &[], (5, 2000)), Ok(vec![]));
  assert_eq!(
    state.sample,
    Some(Sample {
      dts: 0,
      offered: Some(1 + 4 + 2000),
    }),
    "the sample, at its first packet's decode time"
  );
  // Without a frame size, as every demuxer's ticket has it, MP4 reads no
  // skip samples, and drops them.
  let mut state = eac3_track(0);
  assert_eq!(
    admit_eac3(&mut state, &one, &skipping(2000), (0, 1)),
    Ok(vec![line(Dropped)])
  );
}

/// **MP4 marks a `mov_text` cue's end with an empty sample only where the
/// next cue starts past it** (`movenc.c` 7646–7653; table C″ of the
/// census): at zero before a first cue past zero; at the last cue's end,
/// counted from the track's first sample, before a cue past it; none before
/// a cue at or before it, whose gap `check_pkt` counts from the last cue. A
/// first cue before zero starts the track, and every end after it is
/// counted from there.
#[test]
fn mp4_marks_a_cues_end_only_where_the_next_starts_past_it() {
  let start = Cues::default();
  let first = start.take(50, 100);
  assert_eq!(first.reference, Some(0), "the empty sample at zero first");
  assert!(!first.early);
  assert_eq!(
    first.next,
    Cues {
      start: Some(0),
      end: 150,
      last: Some(50),
      ended: false,
      cue: Some((50, 100)),
    }
  );
  let past = first.next.take(200, 10);
  assert_eq!(
    past.reference,
    Some(150),
    "the empty sample at the end first"
  );
  assert!(!past.early);
  let at = first.next.take(150, 10);
  assert_eq!(
    at.reference,
    Some(50),
    "no empty sample: the gap from the cue"
  );
  assert!(!at.early);
  let before = first.next.take(100, 10);
  assert_eq!(before.reference, Some(50));
  assert!(before.early, "before the end MP4 marks");
  assert_eq!(before.next.end, 110);
  let zero = start.take(0, 10);
  assert_eq!(zero.reference, None, "the track's first sample");
  assert!(!zero.early);
  let negative = start.take(-30, 10);
  assert_eq!(negative.reference, None);
  assert!(negative.early, "before zero");
  assert_eq!(
    negative.next,
    Cues {
      start: Some(-30),
      end: 10,
      last: Some(-30),
      ended: false,
      cue: Some((-30, 10)),
    }
  );
  // The end is counted from the track's first sample, and read as a
  // decode time: the empty sample at 10, not at the cue's end, -20.
  let after = negative.next.take(20, 5);
  assert_eq!(after.reference, Some(10));
  assert_eq!(after.next.end, 20 + 30 + 5);
}

/// An ADTS frame `len` bytes long: its header — a CRC after it where `crc`,
/// MPEG-4 AAC LC at the sample rate index `rate`, the channel configuration
/// `channels`, `blocks` raw data blocks less one — then `payload` to the
/// frame's end.
fn adts(len: usize, crc: bool, rate: u8, channels: u8, blocks: u8, payload: u8) -> Vec<u8> {
  let fields: [(u64, u32); 15] = [
    (0xFFF, 12),
    (0, 1),
    (0, 2),
    (u64::from(!crc), 1),
    (1, 2),
    (u64::from(rate), 4),
    (0, 1),
    (u64::from(channels), 3),
    (0, 1),
    (0, 1),
    (0, 1),
    (0, 1),
    (len as u64, 13),
    (0x7FF, 11),
    (u64::from(blocks), 2),
  ];
  let header = fields
    .iter()
    .fold(0u64, |bits, &(value, width)| (bits << width) | value);
  let mut frame = header.to_be_bytes()[1..].to_vec();
  frame.resize(len, payload);
  frame
}

/// **An ADTS frame is one the automatic filter converts whole**: its sync
/// word, a sample rate the table names, a frame length of the packet's
/// own, no CRC over several raw data blocks, bytes after the header and
/// its CRC, and on the track's first frame a channel configuration or a
/// program config element opening the payload.
#[test]
fn an_adts_frame_is_one_the_automatic_filter_converts_whole() {
  let frame = adts(20, false, 3, 2, 0, 0x21);
  assert!(adts_frame(&frame, true));
  assert!(adts_frame(&frame, false));
  assert!(!adts_frame(&frame[..6], false), "shorter than its header");
  let mut sync = frame.clone();
  sync[0] = 0x7F;
  assert!(!adts_frame(&sync, false), "no sync word");
  assert!(
    !adts_frame(&adts(20, false, 13, 2, 0, 0x21), false),
    "a sample rate the table does not name"
  );
  assert!(
    !adts_frame(&adts(21, false, 3, 2, 0, 0x21)[..20], false),
    "a frame longer than its packet"
  );
  let mut two = frame.clone();
  two.extend(adts(20, false, 3, 2, 0, 0x21));
  assert!(
    !adts_frame(&two, false),
    "two frames: the filter strips the first header alone"
  );
  assert!(
    !adts_frame(&adts(20, true, 3, 2, 1, 0x21), false),
    "a CRC over two raw data blocks"
  );
  assert!(
    adts_frame(&adts(20, true, 3, 2, 0, 0x21), false),
    "a CRC over one"
  );
  assert!(
    !adts_frame(&adts(9, true, 3, 2, 0, 0x21), false),
    "nothing after the header and its CRC"
  );
  assert!(
    !adts_frame(&adts(7, false, 3, 2, 0, 0x21), false),
    "nothing after the header"
  );
  // A first frame whose channels are its program config element's opens
  // its payload with one, element id 5, whole and with bytes after it; a
  // later frame is not asked.
  let with = |payload: &[u8]| {
    let mut frame = adts(7 + payload.len(), false, 3, 0, 0, 0);
    frame[7..].copy_from_slice(payload);
    frame
  };
  let element = pce(&[], b"");
  assert_eq!(pce_span(&element), Some(element.len()));
  assert!(adts_frame(
    &with(&[element.as_slice(), &[0x21; 6]].concat()),
    true
  ));
  assert!(
    !adts_frame(&with(&element), true),
    "a program config element that ends the payload leaves it empty"
  );
  assert!(
    !adts_frame(&with(&element[..element.len() - 1]), true),
    "one that runs past it"
  );
  let commented = pce(&[], b"a comment of 22 bytes.");
  assert!(
    !adts_frame(
      &with(&[&commented[..commented.len() - 4], &[0x21; 3]].concat()),
      true
    ),
    "a comment longer than what is left"
  );
  assert!(adts_frame(
    &with(&[commented.as_slice(), &[0x21]].concat()),
    true
  ));
  assert!(
    !adts_frame(&adts(20, false, 3, 0, 0, 0x21), true),
    "no element id 5"
  );
  assert!(adts_frame(&adts(20, false, 3, 0, 0, 0x21), false));
}

/// A program config element, as AAC writes one into a raw data block: its
/// element id, tag 0, AAC LC at sample rate index 3, the front, side,
/// back, LFE, data and coupling elements `counts` names — each written as
/// zeros — no mixdown, the byte alignment, then `comment`.
fn pce(counts: &[u32], comment: &[u8]) -> Vec<u8> {
  fn put(bits: &mut Vec<bool>, value: u64, width: u32) {
    for bit in (0..width).rev() {
      bits.push(value >> bit & 1 == 1);
    }
  }
  let mut bits: Vec<bool> = Vec::new();
  put(&mut bits, 5, 3);
  put(&mut bits, 0, 4);
  put(&mut bits, 1, 2);
  put(&mut bits, 3, 4);
  // Front, side, back (4 bits), LFE (2), data (3), coupling (4).
  let widths = [4, 4, 4, 2, 3, 4];
  let mut elements = 0;
  for (index, width) in widths.into_iter().enumerate() {
    let count = counts.get(index).copied().unwrap_or(0);
    put(&mut bits, u64::from(count), width);
    elements += count * if index == 3 || index == 4 { 4 } else { 5 };
  }
  put(&mut bits, 0, 3);
  for _ in 0..elements {
    bits.push(false);
  }
  while bits.len() % 8 != 0 {
    bits.push(false);
  }
  put(&mut bits, comment.len() as u64, 8);
  for &byte in comment {
    put(&mut bits, u64::from(byte), 8);
  }
  bits
    .chunks(8)
    .map(|byte| byte.iter().fold(0u8, |acc, &bit| acc << 1 | u8::from(bit)))
    .collect()
}

/// **A program config element's span is read as the filter reads it**:
/// through every element its counts name, the byte alignment and its
/// comment.
#[test]
fn a_program_config_elements_span_is_read_as_the_filter_reads_it() {
  // 37 bits before the elements, then 5 front elements of 5 bits and 2
  // LFE ones of 4: 70 bits, aligned to 72; then the comment's size, and
  // its 3 bytes.
  let element = pce(&[5, 0, 0, 2], b"abc");
  assert_eq!(element.len(), 9 + 1 + 3);
  assert_eq!(pce_span(&element), Some(13));
  assert_eq!(pce_span(&[0x21, 0, 0]), None, "element id 1");
}

/// **VP9's hidden frames are held and joined as the superframe filter holds
/// and joins them**: a frame shown passes, or joins those held; a hidden
/// one is held, its times dropped; a superframe or an empty packet passes
/// where none is held and is refused where some are; a seventh hidden frame
/// in a row is refused.
#[test]
fn vp9_hidden_frames_are_held_and_joined_as_the_filter_holds_them() {
  // The frame marker, profile 0, then show-existing, frame type and show
  // bits: `0x82` is shown, `0x80` hidden, `0x88` shows an existing frame.
  let (shown, hidden, existing) = ([0x82, 1], [0x80, 1], [0x88, 1]);
  // Profile 3 reads a reserved bit before the rest.
  let (hidden_3, shown_3) = ([0xB0, 1], [0xB1, 1]);
  // Two frames of two bytes, then the index: its marker, two sizes of one
  // byte, its marker again.
  let superframe = [0x80, 1, 0x82, 2, 0xC1, 2, 2, 0xC1];
  assert_eq!(vp9_superframe(0, &shown), Ok((0, true)));
  assert_eq!(vp9_superframe(0, &existing), Ok((0, true)));
  assert_eq!(vp9_superframe(0, &hidden), Ok((1, false)));
  assert_eq!(vp9_superframe(0, &hidden_3), Ok((1, false)));
  assert_eq!(vp9_superframe(1, &shown_3), Ok((0, true)), "joined");
  assert_eq!(vp9_superframe(0, &superframe), Ok((0, true)));
  assert_eq!(vp9_superframe(1, &superframe), Err(PayloadRule::Superframe));
  assert_eq!(vp9_superframe(0, &[]), Ok((0, true)));
  assert_eq!(vp9_superframe(2, &[]), Err(PayloadRule::Superframe));
  assert_eq!(vp9_superframe(5, &hidden), Ok((6, false)));
  assert_eq!(
    vp9_superframe(6, &hidden),
    Err(PayloadRule::Superframe),
    "a seventh in a row"
  );
  assert_eq!(vp9_superframe(6, &shown), Ok((0, true)), "seven joined");
  // An index whose first marker does not match is no superframe: a frame
  // shown, of any bytes.
  assert_eq!(vp9_superframe(1, &[0x82, 1, 2, 0xC1]), Ok((0, true)));
}

/// One PGS segment of `kind` with `payload` bytes after its header.
fn segment(kind: u8, payload: usize) -> Vec<u8> {
  let mut bytes = vec![kind];
  bytes.extend(u16::try_from(payload).expect("small").to_be_bytes());
  bytes.extend(vec![0x40; payload]);
  bytes
}

/// **PGS segments are joined into display sets as the filter joins them**:
/// a whole set passes as it is; a set across packets is held to its end,
/// timed by its presentation segment's packet and put out with that
/// packet's side data; a set completed with no presentation segment,
/// segments that do not parse, and an empty packet are refused.
#[test]
fn pgs_segments_are_joined_into_display_sets_as_the_filter_joins_them() {
  let start = PgsState::default();
  let (pcs, end, ods) = (segment(0x16, 8), segment(0x80, 0), segment(0x15, 4));
  let whole = [pcs.clone(), ods.clone(), end.clone()].concat();
  let out = |owns, emits, largest: Option<usize>, held: Option<usize>| Step {
    owns,
    emits,
    largest: largest.map(|bytes| bytes as u64),
    held: held.map(|bytes| bytes as u64),
    inserted: None,
  };
  assert_eq!(
    merge(start, &whole, 0, PACKET_BYTES_MAX),
    Ok((start, out(true, Emits::OWN, Some(whole.len()), None)))
  );
  // The presentation segment's packet, at 100, carries 100 bytes of side
  // data: the set is put out at its end segment's packet, at 200, timed at
  // 100 and with that side data.
  let held = PgsState {
    holding: true,
    presentation: true,
    bytes: pcs.len() as u64,
    side: 100,
    times: Some(timed_at(100)),
    packets: 1,
  };
  assert_eq!(
    pgs_frame_merge(start, timed_at(100), &pcs, 100, PACKET_BYTES_MAX),
    Ok((
      held,
      out(true, Emits::NONE, None, Some(pcs.len() + 3 + 100))
    )),
    "held: nothing put out"
  );
  assert_eq!(
    pgs_frame_merge(
      held,
      timed_at(200),
      &[ods.clone(), end.clone()].concat(),
      7,
      PACKET_BYTES_MAX
    ),
    Ok((
      start,
      out(
        false,
        Emits {
          held: Some(timed_at(100)),
          own: false
        },
        Some(whole.len() + 100),
        None
      )
    )),
    "the set is put out now, timed by its presentation segment's packet"
  );
  assert_eq!(
    pgs_frame_merge(
      held,
      timed_at(200),
      &[ods.clone(), end.clone(), whole.clone()].concat(),
      7,
      PACKET_BYTES_MAX
    )
    .map(|(_, step)| step.emits),
    Ok(Emits {
      held: Some(timed_at(100)),
      own: true
    }),
    "the held set first, then a whole one at the packet's own times"
  );
  let unpresented = PgsState {
    holding: true,
    presentation: false,
    bytes: ods.len() as u64,
    side: 0,
    times: None,
    packets: 1,
  };
  assert_eq!(
    merge(start, &ods, 9, PACKET_BYTES_MAX),
    Ok((
      unpresented,
      out(false, Emits::NONE, None, Some(ods.len() + 3))
    ))
  );
  assert_eq!(
    merge(
      PgsState {
        packets: 2,
        bytes: 2 * ods.len() as u64,
        ..unpresented
      },
      &pcs,
      5,
      PACKET_BYTES_MAX
    ),
    Ok((
      PgsState {
        holding: true,
        presentation: true,
        bytes: (2 * ods.len() + pcs.len()) as u64,
        side: 5,
        times: Some(timed_at(0)),
        packets: 3,
      },
      out(
        true,
        Emits::NONE,
        None,
        Some(2 * ods.len() + pcs.len() + 3 + 5)
      )
    )),
    "a third packet joins the set, its presentation segment with it"
  );
  assert_eq!(
    merge(unpresented, &end, 0, PACKET_BYTES_MAX),
    Err(PayloadRule::PgsSegments)
  );
  assert_eq!(
    merge(
      start,
      &[ods.clone(), end.clone()].concat(),
      0,
      PACKET_BYTES_MAX
    ),
    Err(PayloadRule::PgsSegments),
    "a whole set with no presentation segment"
  );
  assert_eq!(
    merge(start, &pcs[..6], 0, PACKET_BYTES_MAX),
    Err(PayloadRule::PgsSegments),
    "a segment past the packet"
  );
  assert_eq!(
    merge(
      start,
      &[pcs.clone(), pcs.clone(), end.clone()].concat(),
      0,
      PACKET_BYTES_MAX
    ),
    Err(PayloadRule::PgsSegments),
    "two presentation segments in a set"
  );
  assert_eq!(
    merge(
      start,
      &[segment(0x16, 2), end.clone()].concat(),
      0,
      PACKET_BYTES_MAX
    ),
    Err(PayloadRule::PgsSegments),
    "a presentation segment shorter than 11 bytes"
  );
  assert_eq!(
    merge(
      start,
      &[whole.clone(), whole.clone()].concat(),
      4,
      PACKET_BYTES_MAX
    ),
    Ok((start, out(true, Emits::OWN, Some(whole.len() + 4), None))),
    "two sets in one packet, each put out with its side data"
  );
  // A set completed, and the start of the next held, from one packet.
  assert_eq!(
    merge(
      held,
      &[ods.clone(), end.clone(), pcs.clone()].concat(),
      2,
      PACKET_BYTES_MAX
    ),
    Ok((
      PgsState {
        side: 2,
        times: Some(timed_at(0)),
        ..held
      },
      out(
        true,
        Emits {
          held: Some(timed_at(100)),
          own: false
        },
        Some(whole.len() + 100),
        Some(pcs.len() + 3 + 2)
      )
    )),
    "the held set put out at its presentation's times; the next opened at this packet's"
  );
  assert_eq!(
    merge(start, &[], 0, PACKET_BYTES_MAX),
    Err(PayloadRule::Empty)
  );
}

/// **MP4's PCM takes the channel layouts its `chnl` box writes**: every
/// channel one ISO/IEC 23001-8 gives a position; an unspecified layout
/// names none, and a downmix's or an ambisonic layout's channels have none.
/// One channel writes no `chnl`, and another codec none either.
#[test]
fn mp4_pcm_takes_the_layouts_its_chnl_box_writes() {
  use AVChannelOrder as Order;
  let native = |mask: u64, channels: i32| LayoutView {
    order: Order::AV_CHANNEL_ORDER_NATIVE as i32,
    channels,
    mask,
    map: &[],
  };
  let unspecified = LayoutView {
    order: Order::AV_CHANNEL_ORDER_UNSPEC as i32,
    channels: 2,
    mask: 0,
    map: &[],
  };
  assert!(mp4_pcm_layout_written(CodecId::PCM_S16LE, native(0b11, 2)));
  // 5.1 (side): FL FR FC LFE SL SR.
  assert!(mp4_pcm_layout_written(CodecId::PCM_S24LE, native(0x60F, 6)));
  assert!(!mp4_pcm_layout_written(CodecId::PCM_S16LE, unspecified));
  assert!(mp4_pcm_layout_written(
    CodecId::PCM_S16LE,
    LayoutView {
      channels: 1,
      ..unspecified
    }
  ));
  // The stereo downmix's two channels.
  assert!(!mp4_pcm_layout_written(
    CodecId::PCM_F32LE,
    native((1 << 29) | (1 << 30), 2)
  ));
  let custom = |channels: &[AVChannel]| {
    channels
      .iter()
      .map(|&channel| CustomChannel::new(channel as i32, [0; 16]))
      .collect::<Vec<_>>()
  };
  let front = custom(&[
    AVChannel::AV_CHAN_FRONT_LEFT,
    AVChannel::AV_CHAN_FRONT_RIGHT,
  ]);
  let unknown = custom(&[AVChannel::AV_CHAN_UNKNOWN, AVChannel::AV_CHAN_FRONT_RIGHT]);
  let of = |map| LayoutView {
    order: Order::AV_CHANNEL_ORDER_CUSTOM as i32,
    channels: 2,
    mask: 0,
    map,
  };
  assert!(mp4_pcm_layout_written(CodecId::PCM_S32LE, of(&front)));
  assert!(!mp4_pcm_layout_written(CodecId::PCM_S32LE, of(&unknown)));
  let ambisonic = LayoutView {
    order: Order::AV_CHANNEL_ORDER_AMBISONIC as i32,
    channels: 6,
    mask: 0b11,
    map: &[],
  };
  assert!(!mp4_pcm_layout_written(CodecId::PCM_S16LE, ambisonic));
  assert!(mp4_pcm_layout_written(CodecId::AAC, unspecified));
  // As `av_channel_layout_channel_from_index` names each channel.
  assert_eq!(native(0b1010, 2).channel_at(1), Some(3));
  assert_eq!(native(0b1010, 2).channel_at(2), None);
  assert_eq!(unspecified.channel_at(0), None);
  assert_eq!(
    ambisonic.channel_at(3),
    Some(AVChannel::AV_CHAN_AMBISONIC_BASE as i32 + 3)
  );
  assert_eq!(ambisonic.channel_at(4), Some(0));
  assert_eq!(ambisonic.channel_at(5), Some(1));
}

/// **Each container keeps the packet side data it has a place for**, by
/// the side-data table: MP4 its new extradata — but where the codec's
/// sample entry reads none of it, AC-3's, E-AC-3's, APV's, PCM's and
/// TTML's (grep G19) — and its skip samples possibly; Matroska and WebM an audio block's end skip and its first
/// start skip, a WebVTT cue's lines, a block addition of id 1, a video
/// block's T.35 additions; neither anything else; another container
/// nothing claimed.
#[test]
fn each_container_keeps_the_side_data_it_has_a_place_for() {
  use AVPacketSideDataType as T;
  use SideDataFate::{Dropped, PossiblyDropped};
  let entry = |kind: T, data: &[u8]| {
    SideDataEntry::new(
      kind as i32,
      FfmpegBytes::try_copy_from_slice(data).expect("a copy"),
    )
  };
  let rules = |family, kind, codec| SideRules {
    family,
    kind,
    codec,
    sample_rate: 48_000,
    frame_size: 1024,
    initial_padding: 1024,
    extradata: vec![0x11, 0x90],
  };
  let skip = |start: u32, end: u32| {
    let mut bytes = start.to_le_bytes().to_vec();
    bytes.extend(end.to_le_bytes());
    bytes.extend([0, 0]);
    bytes
  };
  let mp4 = rules(Family::Mp4, TrackKind::Audio, CodecId::AAC);
  let skipping = entry(T::AV_PKT_DATA_SKIP_SAMPLES, &skip(0, 100));
  assert_eq!(
    mp4.fate(&entry(T::AV_PKT_DATA_NEW_EXTRADATA, &[0x12, 0x10]), false),
    None
  );
  for (kind, codec) in [
    (TrackKind::Video, CodecId::H264),
    (TrackKind::Audio, CodecId::OPUS),
    (TrackKind::Subtitle, CodecId::MOV_TEXT),
  ] {
    assert_eq!(
      rules(Family::Mp4, kind, codec).fate(&entry(T::AV_PKT_DATA_NEW_EXTRADATA, &[1, 2]), false),
      None,
      "{codec:?}: its sample entry is written from it"
    );
  }
  for (kind, codec) in [
    (TrackKind::Audio, CodecId::AC3),
    (TrackKind::Audio, CodecId::EAC3),
    (TrackKind::Audio, CodecId::PCM_S16LE),
    (TrackKind::Audio, CodecId::PCM_F64BE),
    (TrackKind::Video, CodecId::APV),
    (TrackKind::Subtitle, CodecId::TTML),
  ] {
    let parsed = rules(Family::Mp4, kind, codec);
    assert_eq!(
      parsed.fate(&entry(T::AV_PKT_DATA_NEW_EXTRADATA, &[0x12, 0x10]), false),
      Some(Dropped),
      "{codec:?}: its sample entry reads none of it"
    );
    assert_eq!(
      parsed.fate(&entry(T::AV_PKT_DATA_NEW_EXTRADATA, &[0x11, 0x90]), false),
      None,
      "{codec:?}: the track's own says nothing new"
    );
  }
  assert_eq!(mp4.fate(&skipping, false), Some(PossiblyDropped));
  assert_eq!(
    mp4.fate(&entry(T::AV_PKT_DATA_SKIP_SAMPLES, &skip(0, 0)), false),
    None,
    "it says nothing"
  );
  let unframed = SideRules {
    frame_size: 0,
    ..mp4.clone()
  };
  assert_eq!(unframed.fate(&skipping, false), Some(Dropped));
  assert_eq!(
    mp4.fate(&entry(T::AV_PKT_DATA_PRFT, &[1; 24]), false),
    Some(Dropped)
  );
  assert_eq!(
    mp4.fate(&entry(T::AV_PKT_DATA_A53_CC, &[1]), false),
    Some(Dropped)
  );

  let mkv = rules(Family::Matroska, TrackKind::Audio, CodecId::AAC);
  let start = entry(T::AV_PKT_DATA_SKIP_SAMPLES, &skip(1024, 0));
  assert_eq!(mkv.fate(&skipping, false), None, "as discard padding");
  assert_eq!(mkv.fate(&start, true), None, "as the codec delay");
  assert_eq!(mkv.fate(&start, false), Some(Dropped), "after the first");
  assert_eq!(
    mkv.fate(&entry(T::AV_PKT_DATA_SKIP_SAMPLES, &skip(100, 0)), true),
    Some(Dropped),
    "another than the stream's padding"
  );
  let mut reasons = skip(0, 100);
  reasons[8] = 1;
  assert_eq!(
    mkv.fate(&entry(T::AV_PKT_DATA_SKIP_SAMPLES, &reasons), false),
    Some(Dropped)
  );
  assert_eq!(
    mkv.fate(&entry(T::AV_PKT_DATA_NEW_EXTRADATA, &[0x12, 0x10]), false),
    Some(PossiblyDropped)
  );
  assert_eq!(
    mkv.fate(&entry(T::AV_PKT_DATA_NEW_EXTRADATA, &[0x11, 0x90]), false),
    None,
    "the track's own"
  );

  let video = rules(Family::WebM, TrackKind::Video, CodecId::VP9);
  assert_eq!(
    video.fate(&entry(T::AV_PKT_DATA_NEW_EXTRADATA, &[1, 2]), false),
    Some(Dropped)
  );
  for kind in [
    T::AV_PKT_DATA_DYNAMIC_HDR10_PLUS,
    T::AV_PKT_DATA_DYNAMIC_HDR_SMPTE_2094_APP5,
    T::AV_PKT_DATA_LCEVC,
  ] {
    assert_eq!(video.fate(&entry(kind, &[1]), false), None, "{kind:?}");
    assert_eq!(
      mkv.fate(&entry(kind, &[1]), false),
      Some(Dropped),
      "{kind:?} on an audio track"
    );
  }
  let addition = |id: u64| {
    let mut bytes = id.to_be_bytes().to_vec();
    bytes.push(7);
    entry(T::AV_PKT_DATA_MATROSKA_BLOCKADDITIONAL, &bytes)
  };
  assert_eq!(video.fate(&addition(1), false), None);
  assert_eq!(video.fate(&addition(2), false), Some(Dropped));
  let webvtt = rules(Family::Matroska, TrackKind::Subtitle, CodecId::WEBVTT);
  let identifier = entry(T::AV_PKT_DATA_WEBVTT_IDENTIFIER, b"id");
  assert_eq!(webvtt.fate(&identifier, false), None);
  assert_eq!(mkv.fate(&identifier, false), Some(Dropped));
  assert_eq!(mp4.fate(&identifier, false), Some(Dropped));
  assert_eq!(
    SideRules::unclaimed().fate(&entry(T::AV_PKT_DATA_A53_CC, &[1]), false),
    None,
    "nothing is claimed of another container"
  );
}

/// **The filters are those each muxer inserts, by grep 1 and 2 of the
/// census** (table B): IVF's VP9 at its start, the filters on or off;
/// Matroska's, WebM's and the MOV family's at the first packet, with them
/// on; AVIF's none; AAC's in MP4 and Matroska for a track with no
/// configuration, and in FLV, LATM and MOV's other modes undecided until
/// the first packet; PGS's in Matroska's video muxer, and neither AAC's nor
/// PGS's in its audio one; PCM rechunked in DAUD, GXF and MXF.
#[test]
fn the_filters_are_those_each_muxer_inserts() {
  use AutomaticFilter as A;
  let settle = |muxer: &str, mime: Option<&str>, codec, extradata: &[u8], on| {
    Filter::settle(
      Sites::of(muxer, mime),
      Family::of(muxer),
      (kind_of(codec), codec),
      extradata,
      on,
    )
  };
  let vp9 = Filter::Vp9 {
    held: 0,
    bytes: 0,
    largest: 0,
  };
  for on in [true, false] {
    assert_eq!(
      settle("ivf", None, CodecId::VP9, &[], on),
      vp9,
      "IVF, on {on}"
    );
  }
  for muxer in [
    "matroska", "webm", "mov", "3gp", "mp4", "psp", "3g2", "ipod", "ismv", "f4v",
  ] {
    assert_eq!(settle(muxer, None, CodecId::VP9, &[], true), vp9, "{muxer}");
    assert_eq!(
      settle(muxer, None, CodecId::VP9, &[], false),
      Filter::None,
      "{muxer}"
    );
  }
  for muxer in ["avif", "mpegts", "flv", "nut"] {
    assert_eq!(
      settle(muxer, None, CodecId::VP9, &[], true),
      Filter::None,
      "{muxer}"
    );
  }
  for muxer in ["mp4", "matroska"] {
    let filter = settle(muxer, None, CodecId::AAC, &[], true);
    assert_eq!(filter.named(), Some(A::AacAdtsToAsc), "{muxer}");
    assert_eq!(
      settle(muxer, None, CodecId::AAC, &[0x11, 0x90], true),
      Filter::AdtsUndecided {
        configured: true,
        parses: true,
        whole: true,
      },
      "{muxer}: a configured track's first packet decides"
    );
    assert_eq!(
      settle(muxer, None, CodecId::AAC, &[0x11, 0x90], false),
      Filter::None,
      "{muxer}: no filter with them off"
    );
  }
  for muxer in ["flv", "latm", "mov", "ipod"] {
    assert_eq!(
      settle(muxer, None, CodecId::AAC, &[0x11, 0x90], true),
      Filter::AdtsUndecided {
        configured: true,
        parses: true,
        whole: false,
      },
      "{muxer}"
    );
    assert_eq!(
      settle(muxer, None, CodecId::AAC, &[], true).named(),
      None,
      "{muxer}: undecided at open"
    );
  }
  assert_eq!(
    settle(
      "matroska",
      Some("audio/x-matroska"),
      CodecId::AAC,
      &[],
      true
    ),
    Filter::None,
    "Matroska's audio muxer, reached only without the video one"
  );
  assert_eq!(
    settle(
      "matroska",
      Some("video/x-matroska"),
      CodecId::HDMV_PGS_SUBTITLE,
      &[],
      true
    )
    .named(),
    Some(A::PgsFrameMerge)
  );
  assert_eq!(
    settle(
      "matroska",
      Some("audio/x-matroska"),
      CodecId::HDMV_PGS_SUBTITLE,
      &[],
      true
    ),
    Filter::None
  );
  assert_eq!(
    settle("mp4", None, CodecId::HDMV_PGS_SUBTITLE, &[], true),
    Filter::None
  );
  for muxer in ["daud", "gxf", "mxf", "mxf_d10", "mxf_opatom"] {
    assert!(Sites::of(muxer, None).rechunk, "{muxer}");
  }
  assert!(!Sites::of("wav", None).rechunk);
  assert_eq!(A::AacAdtsToAsc.name(), "aac_adtstoasc");
  assert_eq!(A::Vp9Superframe.to_string(), "vp9_superframe");
  assert_eq!(A::PgsFrameMerge.name(), "pgs_frame_merge");
  // The headers that refuse what the query admits.
  assert!(header_refuses(Family::Mp4, CodecId::TRUEHD));
  assert!(!header_refuses(Family::Matroska, CodecId::TRUEHD));
  for codec in [CodecId::COOK, CodecId::RV10, CodecId::RV30, CodecId::ATRAC3] {
    assert!(header_refuses(Family::Matroska, codec), "{codec:?}");
    assert!(header_refuses(Family::WebM, codec), "{codec:?}");
  }
  assert!(!header_refuses(Family::Other, CodecId::TRUEHD));
  // AAC's configuration made by its filter only where it is inserted.
  assert!(!configuration_missing(
    Family::Matroska,
    CodecId::AAC,
    &[],
    true
  ));
  assert!(configuration_missing(
    Family::Matroska,
    CodecId::AAC,
    &[],
    false
  ));
}

/// **AAC's filter is mirrored as it converts, in every container it is
/// inserted in**: a configured track's raw packet passes; a frame is
/// stripped of its header; the first converted, of channel configuration 0,
/// of its program config element too — which, running past its payload,
/// would be put out at a negative size, and is refused in every container,
/// while one that ends it is refused in MP4 and Matroska alone, which take
/// a frame whole; the track's first packet decides elsewhere.
#[test]
fn aacs_filter_is_mirrored_as_it_converts() {
  let frame = adts(20, false, 3, 2, 0, 0x21);
  assert_eq!(
    aac_conversion(&frame, true, false),
    Some(AacConversion::Frame {
      frame: 20,
      output: 13
    })
  );
  assert_eq!(
    aac_conversion(&[0x21, 0x10, 0x05], true, true),
    Some(AacConversion::Passed),
    "raw, on a configured track"
  );
  assert_eq!(
    aac_conversion(&[0x21], true, true),
    None,
    "too small to pass"
  );
  assert_eq!(aac_conversion(&frame[..6], false, false), None);
  // Two frames in a packet: converted, the second's header kept — the
  // filter's own way, which MP4 and Matroska refuse.
  let two = [frame.clone(), frame.clone()].concat();
  assert_eq!(
    aac_conversion(&two, false, false),
    Some(AacConversion::Frame {
      frame: 20,
      output: 33
    })
  );
  let with = |payload: &[u8]| {
    let mut frame = adts(7 + payload.len(), false, 3, 0, 0, 0);
    frame[7..].copy_from_slice(payload);
    frame
  };
  let element = pce(&[], b"");
  let past = with(&element[..element.len() - 1]);
  assert_eq!(
    aac_conversion(&past, true, false),
    Some(AacConversion::Frame {
      frame: past.len(),
      output: -1
    }),
    "a program config element a byte past its payload"
  );
  let ends = with(&element);
  let some = |filter: Filter, bytes: &[u8]| filter.step(timed_at(0), bytes, 0, PACKET_BYTES_MAX);
  let elsewhere = Filter::AdtsUndecided {
    configured: false,
    parses: false,
    whole: false,
  };
  let known = Filter::Adts {
    converted: false,
    configured: false,
    whole: true,
  };
  for filter in [elsewhere, known] {
    assert_eq!(
      some(filter, &past).map(|_| ()),
      Err(PayloadRule::AdtsFrame),
      "{filter:?}"
    );
  }
  assert!(some(elsewhere, &ends).is_ok(), "put out empty, elsewhere");
  assert_eq!(
    some(known, &ends).map(|_| ()),
    Err(PayloadRule::AdtsFrame),
    "MP4 and Matroska take a frame whole"
  );
  // Elsewhere, a first packet that opens with no ADTS header inserts no
  // filter, for good; one that does inserts it, converted.
  let (next, step) = some(elsewhere, &[0x21, 0x10, 0x05]).expect("raw");
  assert_eq!((next, step.owns), (Filter::None, true));
  let (next, _) = some(elsewhere, &frame).expect("a frame");
  assert_eq!(
    next,
    Filter::Adts {
      converted: true,
      configured: false,
      whole: false
    }
  );
  assert!(some(next, &two).is_ok(), "two frames, the filter's own way");
  // A configuration libavcodec does not parse fails the filter's start.
  assert_eq!(
    some(
      Filter::AdtsUndecided {
        configured: true,
        parses: false,
        whole: false,
      },
      &frame
    )
    .map(|_| ()),
    Err(PayloadRule::AdtsFrame)
  );
  assert!(
    some(
      Filter::AdtsUndecided {
        configured: true,
        parses: false,
        whole: false,
      },
      &[0x21, 0x10, 0x05]
    )
    .is_ok(),
    "a raw first packet inserts no filter"
  );
  // The first conversion's new extradata is the filter's.
  assert!(first_conversion(elsewhere, &frame));
  assert!(!first_conversion(elsewhere, &[0x21, 0x10, 0x05]));
  assert!(!first_conversion(next, &frame));
}

#[test]
fn a_tracks_kind_agrees_with_its_media_type_as_table_d_says() {
  use ffmpeg_next::ffi::AVMediaType as M;
  let (video, audio, subtitle, data, attachment) = (
    M::AVMEDIA_TYPE_VIDEO as i32,
    M::AVMEDIA_TYPE_AUDIO as i32,
    M::AVMEDIA_TYPE_SUBTITLE as i32,
    M::AVMEDIA_TYPE_DATA as i32,
    M::AVMEDIA_TYPE_ATTACHMENT as i32,
  );
  for (kind, media) in [
    (TrackKind::Video, video),
    (TrackKind::Audio, audio),
    (TrackKind::Subtitle, subtitle),
    (TrackKind::Data, data),
  ] {
    assert!(media_type_agrees(kind, media, Some(media)), "{kind:?}");
    assert!(
      media_type_agrees(kind, media, None),
      "{kind:?}: no descriptor"
    );
    for other in [video, audio, subtitle, data, attachment] {
      if other != media {
        assert!(
          !media_type_agrees(kind, other, Some(media)),
          "{kind:?}: the ticket's {other}"
        );
        assert!(
          !media_type_agrees(kind, media, Some(other)),
          "{kind:?}: the codec's {other}"
        );
      }
    }
  }
  // An attachment: its road makes its stream's media type, whatever its
  // ticket's and its codec's.
  for media in [attachment, video, audio, -1] {
    assert!(media_type_agrees(TrackKind::Attachment, media, Some(media)));
    assert!(media_type_agrees(TrackKind::Attachment, media, None));
  }
  assert!(!media_type_agrees(TrackKind::Unknown, video, Some(video)));
}

/// **A VP9 run is refused at the frame that decides its superframe would
/// pass the limit** — table B of the census, the limit lowered from
/// libavcodec's largest packet to 100 bytes (the seam): `merge_superframe`
/// sums the frames in 32 bits and allocates by the sum, so past the limit
/// the sum wraps or the allocation is refused after the frames were taken.
/// A hidden frame is refused where no frame shown after it could join the
/// run within the limit; a frame shown, where its superframe passes it.
///
/// PLANT: counting the held frames alone, as before, admits all three.
#[test]
fn a_vp9_run_past_the_limit_is_refused_at_the_frame_that_decides_it() {
  // Frames of 40 bytes: a size byte each in the index, marker twice.
  assert_eq!(superframe_bytes(80 + 10, 40, 3), 90 + 2 + 3);
  // A frame of 300 bytes: two bytes a size.
  assert_eq!(superframe_bytes(300, 300, 1), 300 + 2 + 2);
  // Past 16 MiB, four bytes a size; and no wrap at 4 GiB.
  assert_eq!(superframe_bytes(5 << 30, 1 << 30, 5), (5 << 30) + 2 + 4 * 5);
  let limit = 100;
  let held = vp9_run((0, 0, 0), 1, 40, limit).expect("one hidden frame of 40");
  assert_eq!(held, (40, 40));
  let held =
    vp9_run((1, held.0, held.1), 2, 40, limit).expect("two: 80, and a byte could join them");
  assert_eq!(held, (80, 40));
  assert_eq!(
    vp9_run((2, held.0, held.1), 3, 40, limit),
    Err(PayloadRule::SuperframeSize),
    "a third: no frame shown after it could join them within 100"
  );
  assert_eq!(
    vp9_run((2, held.0, held.1), 0, 20, limit),
    Err(PayloadRule::SuperframeSize),
    "a frame shown of 20 joins them at 105"
  );
  assert_eq!(
    vp9_run((2, held.0, held.1), 0, 10, limit),
    Ok((0, 0)),
    "a frame shown of 10 joins them at 95"
  );
  assert_eq!(
    vp9_run((0, 0, 0), 0, 1000, limit),
    Ok((0, 0)),
    "a frame shown alone passes"
  );
  // At the real limit, three frames of a gigabyte: the third is refused.
  let gib = 1 << 30;
  let held = vp9_run((0, 0, 0), 1, gib, PACKET_BYTES_MAX).expect("a gigabyte");
  let held = vp9_run((1, held.0, held.1), 2, gib - 1024, PACKET_BYTES_MAX).expect("two");
  assert_eq!(
    vp9_run((2, held.0, held.1), 3, gib, PACKET_BYTES_MAX),
    Err(PayloadRule::SuperframeSize)
  );
}

/// **A PGS display set past the limit is refused at the packet that would
/// grow it there, or that leaves it no room for its end segment** — the
/// limit lowered to 20 bytes (the seam).
#[test]
fn a_pgs_display_set_past_the_limit_is_refused() {
  let (pcs, end, ods) = (segment(0x16, 8), segment(0x80, 0), segment(0x15, 4));
  let start = PgsState::default();
  // 11 bytes held, then 7 more and the end's 3: 21.
  let (held, _) = merge(start, &pcs, 0, 20).expect("the presentation segment, held");
  assert_eq!(
    merge(held, &[ods.clone(), end.clone()].concat(), 0, 20),
    Err(PayloadRule::DisplaySetSize)
  );
  assert!(merge(held, &[ods.clone(), end.clone()].concat(), 0, 21).is_ok());
  // 11 and 7 held leave 2 bytes, where no end segment fits.
  assert_eq!(merge(held, &ods, 0, 20), Err(PayloadRule::DisplaySetSize));
  assert!(merge(held, &ods, 0, 21).is_ok());
  // A whole set passes as it is, held by nothing.
  let whole = [pcs, ods, end].concat();
  assert!(merge(start, &whole, 0, 20).is_ok());
}

/// **VP9's filter puts out each superframe sized with the hidden frames it
/// holds, its index and the shown frame's side data, and what it holds is
/// sized at the least it can come to** — the outputs Matroska's block
/// bound is held to.
#[test]
fn vp9s_filter_puts_out_superframes_sized_with_what_it_holds() {
  let vp9 = Filter::Vp9 {
    held: 0,
    bytes: 0,
    largest: 0,
  };
  let hidden = [0x80, 1, 1, 1];
  let shown = [0x82, 2, 2];
  let (vp9, step) = vp9
    .step(timed_at(0), &hidden, 50, PACKET_BYTES_MAX)
    .expect("held");
  assert_eq!(
    step,
    Step {
      owns: false,
      emits: Emits::NONE,
      largest: None,
      held: Some(4 + 1 + 2 + 2),
      inserted: None,
    },
    "four bytes held and a byte shown, their index of two sizes"
  );
  let (vp9, step) = vp9
    .step(timed_at(0), &shown, 30, PACKET_BYTES_MAX)
    .expect("joined");
  assert_eq!(
    step,
    Step {
      owns: true,
      emits: Emits::OWN,
      largest: Some(4 + 3 + 2 + 2 + 30),
      held: None,
      inserted: None,
    },
    "the superframe, with the shown frame's side data"
  );
  let (_, step) = vp9
    .step(timed_at(0), &shown, 30, PACKET_BYTES_MAX)
    .expect("alone");
  assert_eq!(
    step.largest,
    Some(3 + 30),
    "a frame shown alone passes as it is"
  );
}

/// **The Annex B rewrite is bounded by a byte more for each start code.**
#[test]
fn the_annex_b_rewrite_is_bounded_by_its_start_codes() {
  assert_eq!(length_prefixed_bytes(&[0, 0, 1, 9, 9, 0, 0, 1, 8]), 9 + 2);
  assert_eq!(length_prefixed_bytes(&[0, 0, 0, 1, 9]), 5 + 1);
  assert_eq!(length_prefixed_bytes(&[1, 2, 3]), 3);
}

/// **The configurations MP4's trailer refuses are known at open**: FLAC's
/// one 34-byte STREAMINFO, Opus's OpusHead and its mapping table, the
/// header strings of libavcodec's TTML encoder; and a frame cropping that
/// leaves no picture.
#[test]
fn the_configurations_mp4s_trailer_refuses_are_known_at_open() {
  let flac = [0u8; 34];
  assert!(!configuration_invalid(Family::Mp4, CodecId::FLAC, &flac));
  assert!(configuration_invalid(
    Family::Mp4,
    CodecId::FLAC,
    &flac[..33]
  ));
  assert!(configuration_invalid(
    Family::Mp4,
    CodecId::FLAC,
    &[flac.as_slice(), &[0; 4]].concat()
  ));
  assert!(
    !configuration_invalid(Family::Matroska, CodecId::FLAC, &flac[..33]),
    "Matroska's header writes it"
  );
  // An OpusHead of 19 bytes, mapping family 0; of family 1, two bytes and
  // one a channel after them.
  let mut head = b"OpusHead".to_vec();
  head.extend([1, 2, 0x38, 0x01, 0x80, 0xBB, 0, 0, 0, 0, 0]);
  assert_eq!(head.len(), 19);
  assert!(opus_head_written(&head));
  assert!(!opus_head_written(&head[..18]));
  head[18] = 1;
  assert!(!opus_head_written(&head), "family 1 with no table");
  head.extend([1, 0, 0, 1]);
  assert!(opus_head_written(&head));
  assert!(configuration_invalid(
    Family::Mp4,
    CodecId::OPUS,
    &head[..10]
  ));
  // TTML's header strings: none, or a terminated string and another.
  assert!(ttml_header_strings(b""));
  assert!(ttml_header_strings(b"a\0b\0"));
  assert!(ttml_header_strings(b"\0\0"));
  assert!(!ttml_header_strings(b"a"));
  assert!(!ttml_header_strings(b"a\0"));
  assert!(!ttml_header_strings(b"a\0b"));
  assert!(configuration_invalid(
    Family::Mp4,
    CodecId::TTML,
    b"lavc-ttmlenca\0"
  ));
  assert!(!configuration_invalid(Family::Mp4, CodecId::TTML, b"<tt/>"));
  // Frame cropping: 16 bytes of top, bottom, left and right.
  let crop = |top: u32, bottom: u32, left: u32, right: u32| {
    [top, bottom, left, right]
      .iter()
      .flat_map(|count| count.to_le_bytes())
      .collect::<Vec<_>>()
  };
  assert!(!crop_leaves_no_picture(Some(&crop(1, 1, 2, 2)), 64, 48));
  assert!(crop_leaves_no_picture(Some(&crop(0, 0, 32, 32)), 64, 48));
  assert!(crop_leaves_no_picture(Some(&crop(24, 24, 0, 0)), 64, 48));
  assert!(
    crop_leaves_no_picture(Some(&crop(0, 0, 0, 0)), 0, 48),
    "no width"
  );
  assert!(!crop_leaves_no_picture(
    Some(&crop(0, 0, 32, 32)[..15]),
    64,
    48
  ));
  assert!(!crop_leaves_no_picture(None, 0, 0));
  // No configuration where MP4 needs one.
  for codec in [CodecId::AV1, CodecId::FLAC, CodecId::OPUS, CodecId::ALAC] {
    assert!(
      configuration_missing(Family::Mp4, codec, &[], true),
      "{codec:?}"
    );
  }
}

/// **An AudioSpecificConfig parses as libavcodec parses it**: it fails on
/// no bytes, a channel configuration past 14, and an ALS configuration
/// short of its header.
#[test]
fn an_audio_specific_config_parses_as_libavcodec_parses_it() {
  // AAC LC, 48 kHz, stereo: 00010 0011 0010 000.
  assert!(audio_specific_config_parses(&[0x11, 0x90]));
  // Channel configuration 15.
  assert!(!audio_specific_config_parses(&[0x11, 0xF8]));
  assert!(!audio_specific_config_parses(&[]));
  // An escaped object type, 32 + 4 = 36, ALS: 11111 000100, rate index 3,
  // channels 2, then nothing of its 112 bits.
  assert!(!audio_specific_config_parses(&[0xF8, 0x86, 0x40, 0x00]));
}

/// **MP4's AC-3 takes no first frame its `dac3` does not define**: a bit
/// stream id of 9 or 10 on the frame MP4 writes `dac3` from is refused; on
/// a later frame it is not asked.
#[test]
fn mp4s_ac3_refuses_a_first_frame_dac3_does_not_define() {
  // AC-3 at 48 kHz, frame size code 0 (64 words), bit stream id `id`.
  let frame = |id: u8| {
    let mut bytes = vec![0x0B, 0x77, 0, 0, 0x00, id << 3, 0, 0];
    bytes.resize(128, 0);
    bytes
  };
  assert!(take(Ac3State::default(), false, &frame(8)).is_ok());
  for id in [9, 10] {
    assert_eq!(
      take(Ac3State::default(), false, &frame(id)),
      Err(PayloadRule::Ac3BitStreamId),
      "{id}"
    );
  }
  let after = take(Ac3State::default(), false, &frame(8)).expect("the first");
  assert!(take(after, false, &frame(9)).is_ok(), "a later frame");
  // E-AC-3's dec3 carries no such bound.
  assert!(take(Ac3State::default(), true, &frame(9)).is_ok());
}

/// **PCM rechunking's sample size is counted as its start counts it**, in
/// C's `int`: 33 554 431 channels of 64 bits fit, one more does not.
#[test]
fn pcm_rechunkings_sample_size_is_counted_in_an_int() {
  assert!(!rechunk_overflows(2, 16));
  assert!(!rechunk_overflows(33_554_431, 64));
  assert!(rechunk_overflows(33_554_432, 64));
  assert!(
    !rechunk_overflows(i32::MAX, 0),
    "no fixed width: no filter takes it"
  );
}

/// **MP4's movie timescale is `mov_init`'s fold of `av_gcd_q`**: the least
/// common multiple of the streams' denominators below `INT_MAX`, at least
/// 1000, none for good once it passes.
#[test]
fn mp4s_movie_timescale_is_mov_inits_fold() {
  let of = |dens: &[i64]| movie_timescale(dens.iter().copied());
  assert_eq!(of(&[]), 1000, "no stream: MOV_TIMESCALE");
  assert_eq!(of(&[1000]), 1000);
  assert_eq!(of(&[25]), 1000, "at least 1000");
  assert_eq!(of(&[90_000, 48_000]), 720_000);
  assert_eq!(of(&[90_000, 44_100]), 4_410_000);
  assert_eq!(of(&[1_000_000, 48_000, 30_000]), 6_000_000);
  // Past `INT_MAX` the fold keeps `{1, 0}`: 1000, whatever follows.
  assert_eq!(of(&[2_147_483_629, 2]), 1000);
  assert_eq!(of(&[2_147_483_629, 2, 90_000]), 1000);
}

/// **MP4's chapter track refuses or moves the samples `check_pkt` would**:
/// counted in the movie timescale, a duration past `INT_MAX`, a start
/// `INT_MAX` or more after the last — the first's after zero — or before it.
#[test]
fn mp4s_chapter_samples_are_held_to_check_pkt() {
  let chapter = |start: i64, end: i64| {
    Chapter::<Ffmpeg>::new(
      0,
      millis(),
      Timestamp::new(start, millis()),
      Timestamp::new(end, millis()),
    )
  };
  let refused = |spans: &[(i64, i64)], timescale: i64| {
    let chapters: Vec<_> = spans
      .iter()
      .map(|&(start, end)| chapter(start, end))
      .collect();
    mp4_chapter_sample_refused(&chapters, spans, timescale)
  };
  let edge = i64::from(i32::MAX);
  assert_eq!(refused(&[(0, edge)], 1000), None, "INT_MAX ticks long");
  assert_eq!(refused(&[(0, edge + 1)], 1000), Some(0));
  assert_eq!(refused(&[(edge - 1, edge)], 1000), None);
  assert_eq!(
    refused(&[(edge, edge)], 1000),
    Some(0),
    "INT_MAX after zero"
  );
  assert_eq!(
    refused(&[(0, 10), (edge, edge)], 1000),
    Some(1),
    "INT_MAX after the last's start"
  );
  assert_eq!(
    refused(&[(5, 10), (3, 20)], 1000),
    Some(1),
    "before the last"
  );
  assert_eq!(
    refused(&[(5, 10), (5, 20)], 1000),
    None,
    "at the last's start"
  );
  // At 90 kHz, INT_MAX ticks are 23 860.93 s.
  assert_eq!(refused(&[(0, 23_860_000)], 90_000), None);
  assert_eq!(refused(&[(0, 23_861_000)], 90_000), Some(0));
}

/// **What a filter holds at the end is reported by its count**: VP9's
/// hidden frames; nothing for a filter that holds nothing, nor for PGS,
/// whose open display sets this muxer holds itself and counts at the end.
#[test]
fn what_a_filter_holds_at_the_end_is_counted() {
  assert_eq!(
    held_at_end(Filter::Vp9 {
      held: 3,
      bytes: 30,
      largest: 10
    }),
    Some((AutomaticFilter::Vp9Superframe, 3))
  );
  assert_eq!(
    held_at_end(Filter::Vp9 {
      held: 0,
      bytes: 0,
      largest: 0
    }),
    None
  );
  let holding = PgsState {
    holding: true,
    presentation: true,
    bytes: 40,
    side: 0,
    times: Some(timed_at(0)),
    packets: 2,
  };
  assert_eq!(held_at_end(Filter::Pgs(holding)), None, "the muxer's own");
  assert_eq!(held_at_end(Filter::Pgs(PgsState::default())), None);
  assert_eq!(held_at_end(Filter::None), None);
}

/// **libavformat's move of every stream's times is mirrored where its
/// first write settles it** (`handle_avoid_negative_ts`, `mux.c` 636–717;
/// table G): MP4 makes none; GXF, MXF, and Matroska beside a video track of
/// no native id are not mirrored; Matroska judges presentation times, but
/// decode times beside JPEG 2000, and another container decode times. With
/// one stream interleaved — SMPTE 2038 data not counted — the first packet
/// is written as it comes. With two: once as many hold a packet as are
/// interleaved, SMPTE 2038 data's counted among those holding; or once the
/// packets held span more than 10 s, every stream that holds none one that
/// may be lacked — not VP8 — the subtitles' span left aside. Each stream's
/// distance is rounded up to its own tick, and after the write a packet the
/// move leaves before the earliest is refused.
#[test]
fn libavformats_move_of_early_times_is_settled_by_its_first_write() {
  let ms = millis();
  let ninety = Timebase::new(1, NonZeroI32::new(90_000).expect("non-zero"));
  let video = (TrackKind::Video, CodecId::H264, true);
  let audio = (TrackKind::Audio, CodecId::AAC, true);
  let subtitle = (TrackKind::Subtitle, CodecId::SUBRIP, true);
  let data = (TrackKind::Data, SMPTE_2038, true);
  let of = |family, muxer: &str, tracks: &[(TrackKind, CodecId, bool)]| {
    Shift::of(family, muxer, true, tracks)
  };

  assert_eq!(Shift::of(Family::Mp4, "mp4", false, &[video]), None);
  for muxer in ["gxf", "mxf", "mxf_d10", "mxf_opatom"] {
    assert_eq!(of(Family::Other, muxer, &[video]), None, "{muxer}");
  }
  assert_eq!(
    Shift::of(Family::Other, "ogg", false, &[audio]),
    None,
    "a container that takes negative times"
  );
  assert!(
    of(Family::Matroska, "matroska", &[video, audio])
      .expect("mirrored")
      .by_pts
  );
  assert!(
    !of(
      Family::Matroska,
      "matroska",
      &[(TrackKind::Video, CodecId::JPEG2000, true), audio]
    )
    .expect("mirrored")
    .by_pts,
    "VFW mode"
  );
  assert_eq!(
    of(
      Family::Matroska,
      "matroska",
      &[
        (
          TrackKind::Video,
          CodecId::from_raw(ffmpeg_next::ffi::AVCodecID::AV_CODEC_ID_H263 as i32),
          true,
        ),
        audio
      ]
    ),
    None,
    "VFW or QuickTime mode, by tables not mirrored"
  );
  assert!(!of(Family::Other, "flv", &[video]).expect("mirrored").by_pts);

  // One stream interleaved, beside SMPTE 2038 data: the first packet is
  // the first write, and settles the move.
  let alone = of(Family::Other, "mpegts", &[video, data]).expect("mirrored");
  assert!(alone.passthrough);
  let moved = alone.take(0, (-40, -40, 0), ms).expect("before the write");
  assert!(moved.written);
  assert_eq!(moved.earliest, Some(Timestamp::new(-40, ms)));
  assert!(moved.take(1, (-40, -40, 0), ms).is_ok(), "lifted to zero");
  assert_eq!(
    moved.take(1, (-41, -41, 0), ms),
    Err(Early::BeforeEarliest),
    "left before it"
  );

  // Two: written once both hold a packet, the early one moving the other.
  let both = of(Family::Matroska, "matroska", &[video, audio]).expect("mirrored");
  assert!(!both.passthrough);
  let held = both.take(0, (0, 0, 0), ms).expect("video");
  assert!(!held.written, "the audio may still come earlier");
  let moved = held.take(1, (-30, -30, -21), ms).expect("before the write");
  assert!(moved.written);
  assert_eq!(moved.earliest, Some(Timestamp::new(-9, ms)));
  assert!(moved.take(0, (-9, -9, 0), ms).is_ok());
  assert_eq!(moved.take(0, (-10, -10, 0), ms), Err(Early::BeforeEarliest));

  // Each stream's distance rounded up to its own tick.
  let mixed = of(Family::Other, "mpegts", &[video, audio]).expect("mirrored");
  let fine = mixed
    .take(0, (-1, -1, 0), ninety)
    .and_then(|shift| shift.take(1, (0, 0, 0), ms))
    .expect("both held");
  assert!(fine.written);
  assert!(
    fine.take(1, (-1, -1, 0), ms).is_ok(),
    "a tick of 90 kHz lifts a millisecond"
  );
  assert_eq!(fine.take(1, (-2, -2, 0), ms), Err(Early::BeforeEarliest));
  let coarse = mixed
    .take(1, (-1, -1, 0), ms)
    .and_then(|shift| shift.take(0, (0, 0, 0), ninety))
    .expect("both held");
  assert!(
    coarse.take(0, (-90, -90, 0), ninety).is_ok(),
    "a millisecond is 90 ticks"
  );
  assert_eq!(
    coarse.take(0, (-91, -91, 0), ninety),
    Err(Early::BeforeEarliest)
  );

  // The delay: written once the packets held span more than 10 s.
  let spanned = both
    .take(0, (0, 0, 0), ms)
    .and_then(|shift| shift.take(0, (10_000, 10_000, 0), ms))
    .expect("video");
  assert!(!spanned.written, "10 s is not past the delay");
  let late = spanned.take(0, (10_001, 10_001, 0), ms).expect("video");
  assert!(late.written);
  assert_eq!(
    late.take(1, (-22, -22, -21), ms),
    Err(Early::BeforeEarliest),
    "the audio's, after it"
  );
  // ... but not where the stream that holds none may not be lacked.
  let vp8 = of(
    Family::WebM,
    "webm",
    &[(TrackKind::Video, CodecId::VP8, true), audio],
  )
  .expect("mirrored");
  let waiting = vp8
    .take(1, (0, 0, -21), ms)
    .and_then(|shift| shift.take(1, (20_000, 20_000, -21), ms))
    .expect("audio");
  assert!(!waiting.written, "the interleaving waits on VP8");
  // ... and not by a subtitle's span.
  let cues = of(Family::Matroska, "matroska", &[video, audio, subtitle]).expect("mirrored");
  let waiting = cues
    .take(0, (0, 0, 0), ms)
    .and_then(|shift| shift.take(2, (20_000, 20_000, 0), ms))
    .expect("video and a cue");
  assert!(!waiting.written, "a subtitle's span is left aside");

  // SMPTE 2038 data, not interleaved, still counts among those holding.
  let counted = of(Family::Other, "mpegts", &[video, audio, data]).expect("mirrored");
  assert!(!counted.passthrough);
  let written = counted
    .take(0, (0, 0, 0), ms)
    .and_then(|shift| shift.take(2, (0, 0, 0), ms))
    .expect("video and data");
  assert!(written.written, "two hold a packet, as two are interleaved");

  // FLV keeps the first packet it wrote as its reference, moved: a packet
  // of the stream the delay left behind, before it, it refuses; another
  // container takes it.
  for (muxer, refused) in [("flv", true), ("mpegts", false)] {
    let first = of(Family::Other, muxer, &[video, audio])
      .expect("mirrored")
      .take(0, (5_000, 5_000, 0), ms)
      .and_then(|shift| shift.take(0, (15_001, 15_001, 0), ms))
      .expect("video, past the delay");
    assert!(first.written);
    assert_eq!(
      first.take(1, (2_000, 2_000, 0), ms).err(),
      refused.then_some(Early::BeforeFirst),
      "{muxer}"
    );
    assert!(first.take(1, (5_000, 5_000, 0), ms).is_ok(), "{muxer}");
  }
  let moved = of(Family::Other, "flv", &[video, audio])
    .expect("mirrored")
    .take(0, (-100, -100, 0), ms)
    .and_then(|shift| shift.take(0, (9_901, 9_901, 0), ms))
    .expect("video, past the delay");
  assert!(
    moved.take(1, (-50, -50, 0), ms).is_ok(),
    "moved past zero, the first"
  );
  assert_eq!(
    moved.take(1, (-150, -150, 0), ms),
    Err(Early::BeforeEarliest),
    "before zero, moved"
  );
}

/// **MP4's sync-sample table is decided by movenc's own test** (Codex R7
/// 1): its video, TrueHD, MPEG-H 3D Audio and USAC AAC tracks, USAC by the
/// stream's profile alone (`movenc.c` 3412–3418) — USAC's profile gets the
/// table whatever the configuration says; another profile, or none, gets
/// none, a USAC configuration notwithstanding (the configuration is not an
/// argument: movenc reads none).
#[test]
fn mp4s_sync_table_is_decided_by_the_streams_profile() {
  const USAC: i32 = 41;
  assert!(mp4_syncs(TrackKind::Video, CodecId::H264, -99));
  assert!(mp4_syncs(TrackKind::Audio, CodecId::TRUEHD, -99));
  assert!(
    mp4_syncs(TrackKind::Audio, CodecId::AAC, USAC),
    "USAC's profile"
  );
  for profile in [-99, 0, 1, 4, 28] {
    assert!(
      !mp4_syncs(TrackKind::Audio, CodecId::AAC, profile),
      "AAC of profile {profile}"
    );
  }
  assert!(
    !mp4_syncs(TrackKind::Audio, CodecId::OPUS, USAC),
    "the number is AAC's alone"
  );
  assert!(!mp4_syncs(TrackKind::Subtitle, CodecId::MOV_TEXT, -99));
}
