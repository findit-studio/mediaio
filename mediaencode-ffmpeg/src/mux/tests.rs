use core::num::NonZeroI32;

use mediaio_ffmpeg::FfmpegBytes;

use super::*;

fn millis() -> Timebase {
  Timebase::new(1, NonZeroI32::new(1000).expect("non-zero"))
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

/// **MP4's AC-3 handling is held as MP4 holds it**: a header that does not
/// parse; an E-AC-3 track's first packet tiled by dependent frames; a
/// second independent frame ending the first set; frames of fewer than six
/// blocks joined into sixes, and none past.
#[test]
fn mp4s_ac3_handling_is_mirrored() {
  let start = Ac3State::default();
  let ac3 = frame(&AC3, 448);
  let learned = ac3_packet(start, false, &ac3).expect("an AC-3 frame");
  assert_eq!(
    learned,
    Ac3State {
      done: true,
      bsid: 8,
      pending: 0
    }
  );
  assert_eq!(
    ac3_packet(learned, false, &[1, 2, 3]),
    Err(PayloadRule::Ac3Header)
  );
  // E-AC-3: the first packet, alone or with a dependent frame after it.
  let independent = frame(&eac3(0, 3), 768);
  let first = ac3_packet(start, true, &independent).expect("one frame");
  assert_eq!(
    first,
    Ac3State {
      done: false,
      bsid: 16,
      pending: 0
    }
  );
  let mut with_dependent = independent.clone();
  with_dependent.extend(frame(&eac3(1, 3), 768));
  assert!(ac3_packet(start, true, &with_dependent).is_ok());
  let mut with_independent = independent.clone();
  with_independent.extend(frame(&eac3(0, 3), 768));
  assert_eq!(
    ac3_packet(start, true, &with_independent),
    Err(PayloadRule::Ac3Substreams)
  );
  let mut left_over = independent.clone();
  left_over.extend([1, 2, 3]);
  assert_eq!(
    ac3_packet(start, true, &left_over),
    Err(PayloadRule::Ac3Substreams)
  );
  // The second independent frame ends the first set: no tiling is asked
  // of it.
  let second = ac3_packet(first, true, &left_over).expect("the second packet");
  assert!(second.done);
  // Two-block frames join into sixes; a three-block frame after four
  // blocks, or a six-block frame after two, runs past.
  let two = frame(&eac3(0, 1), 768);
  let three = frame(&eac3(0, 2), 768);
  let mut state = second;
  for pending in [2, 4, 0] {
    state = ac3_packet(state, true, &two).expect("two blocks");
    assert_eq!(state.pending, pending);
  }
  let four = ac3_packet(ac3_packet(state, true, &two).expect("two"), true, &two).expect("four");
  assert_eq!(ac3_packet(four, true, &three), Err(PayloadRule::Ac3Blocks));
  let held = ac3_packet(state, true, &two).expect("two");
  assert_eq!(
    ac3_packet(held, true, &independent),
    Err(PayloadRule::Ac3Blocks)
  );
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
  let out = |reaches, largest: Option<usize>, held: Option<usize>| Step {
    reaches,
    largest: largest.map(|bytes| bytes as u64),
    held: held.map(|bytes| bytes as u64),
  };
  assert_eq!(
    pgs_frame_merge(start, &whole, 0, PACKET_BYTES_MAX),
    Ok((start, out(true, Some(whole.len()), None)))
  );
  // The presentation segment's packet carries 100 bytes of side data, which
  // the set is put out with.
  let held = PgsState {
    holding: true,
    presentation: true,
    bytes: pcs.len() as u64,
    side: 100,
    packets: 1,
  };
  assert_eq!(
    pgs_frame_merge(start, &pcs, 100, PACKET_BYTES_MAX),
    Ok((held, out(true, None, Some(pcs.len() + 3 + 100))))
  );
  assert_eq!(
    pgs_frame_merge(
      held,
      &[ods.clone(), end.clone()].concat(),
      7,
      PACKET_BYTES_MAX
    ),
    Ok((start, out(false, Some(whole.len() + 100), None))),
    "the set is timed by its presentation segment's packet, and takes its side data"
  );
  let unpresented = PgsState {
    holding: true,
    presentation: false,
    bytes: ods.len() as u64,
    side: 0,
    packets: 1,
  };
  assert_eq!(
    pgs_frame_merge(start, &ods, 9, PACKET_BYTES_MAX),
    Ok((unpresented, out(false, None, Some(ods.len() + 3))))
  );
  assert_eq!(
    pgs_frame_merge(
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
        packets: 3,
      },
      out(true, None, Some(2 * ods.len() + pcs.len() + 3 + 5))
    )),
    "a third packet joins the set, its presentation segment with it"
  );
  assert_eq!(
    pgs_frame_merge(unpresented, &end, 0, PACKET_BYTES_MAX),
    Err(PayloadRule::PgsSegments)
  );
  assert_eq!(
    pgs_frame_merge(
      start,
      &[ods.clone(), end.clone()].concat(),
      0,
      PACKET_BYTES_MAX
    ),
    Err(PayloadRule::PgsSegments),
    "a whole set with no presentation segment"
  );
  assert_eq!(
    pgs_frame_merge(start, &pcs[..6], 0, PACKET_BYTES_MAX),
    Err(PayloadRule::PgsSegments),
    "a segment past the packet"
  );
  assert_eq!(
    pgs_frame_merge(
      start,
      &[pcs.clone(), pcs.clone(), end.clone()].concat(),
      0,
      PACKET_BYTES_MAX
    ),
    Err(PayloadRule::PgsSegments),
    "two presentation segments in a set"
  );
  assert_eq!(
    pgs_frame_merge(
      start,
      &[segment(0x16, 2), end.clone()].concat(),
      0,
      PACKET_BYTES_MAX
    ),
    Err(PayloadRule::PgsSegments),
    "a presentation segment shorter than 11 bytes"
  );
  assert_eq!(
    pgs_frame_merge(
      start,
      &[whole.clone(), whole.clone()].concat(),
      4,
      PACKET_BYTES_MAX
    ),
    Ok((start, out(true, Some(whole.len() + 4), None))),
    "two sets in one packet, each put out with its side data"
  );
  // A set completed, and the start of the next held, from one packet.
  assert_eq!(
    pgs_frame_merge(
      held,
      &[ods.clone(), end.clone(), pcs.clone()].concat(),
      2,
      PACKET_BYTES_MAX
    ),
    Ok((
      PgsState { side: 2, ..held },
      out(true, Some(whole.len() + 100), Some(pcs.len() + 3 + 2))
    ))
  );
  assert_eq!(
    pgs_frame_merge(start, &[], 0, PACKET_BYTES_MAX),
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
/// the side-data table: MP4 its new extradata, and its skip samples
/// possibly; Matroska and WebM an audio block's end skip and its first
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
      codec,
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
      Filter::None,
      "{muxer}: a configured track takes raw AAC alone"
    );
  }
  for muxer in ["flv", "latm", "mov", "ipod"] {
    assert_eq!(
      settle(muxer, None, CodecId::AAC, &[0x11, 0x90], true),
      Filter::AdtsUndecided {
        configured: true,
        parses: true
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
  let some = |filter: Filter, bytes: &[u8]| filter.step(bytes, 0, PACKET_BYTES_MAX);
  let elsewhere = Filter::AdtsUndecided {
    configured: false,
    parses: false,
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
  assert_eq!((next, step.reaches), (Filter::None, true));
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
        parses: false
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
        parses: false
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
  let (held, _) = pgs_frame_merge(start, &pcs, 0, 20).expect("the presentation segment, held");
  assert_eq!(
    pgs_frame_merge(held, &[ods.clone(), end.clone()].concat(), 0, 20),
    Err(PayloadRule::DisplaySetSize)
  );
  assert!(pgs_frame_merge(held, &[ods.clone(), end.clone()].concat(), 0, 21).is_ok());
  // 11 and 7 held leave 2 bytes, where no end segment fits.
  assert_eq!(
    pgs_frame_merge(held, &ods, 0, 20),
    Err(PayloadRule::DisplaySetSize)
  );
  assert!(pgs_frame_merge(held, &ods, 0, 21).is_ok());
  // A whole set passes as it is, held by nothing.
  let whole = [pcs, ods, end].concat();
  assert!(pgs_frame_merge(start, &whole, 0, 20).is_ok());
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
  let (vp9, step) = vp9.step(&hidden, 50, PACKET_BYTES_MAX).expect("held");
  assert_eq!(
    step,
    Step {
      reaches: false,
      largest: None,
      held: Some(4 + 1 + 2 + 2),
    },
    "four bytes held and a byte shown, their index of two sizes"
  );
  let (vp9, step) = vp9.step(&shown, 30, PACKET_BYTES_MAX).expect("joined");
  assert_eq!(
    step,
    Step {
      reaches: true,
      largest: Some(4 + 3 + 2 + 2 + 30),
      held: None,
    },
    "the superframe, with the shown frame's side data"
  );
  let (_, step) = vp9.step(&shown, 30, PACKET_BYTES_MAX).expect("alone");
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
  assert!(ac3_packet(Ac3State::default(), false, &frame(8)).is_ok());
  for id in [9, 10] {
    assert_eq!(
      ac3_packet(Ac3State::default(), false, &frame(id)),
      Err(PayloadRule::Ac3BitStreamId),
      "{id}"
    );
  }
  let after = ac3_packet(Ac3State::default(), false, &frame(8)).expect("the first");
  assert!(ac3_packet(after, false, &frame(9)).is_ok(), "a later frame");
  // E-AC-3's dec3 carries no such bound.
  assert!(ac3_packet(Ac3State::default(), true, &frame(9)).is_ok());
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
