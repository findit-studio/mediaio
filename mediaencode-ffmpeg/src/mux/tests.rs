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
    (family, audited_for(family)),
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
  flags.difference(
    kept(
      (family, audited_for(family)),
      Road::Timed,
      kind,
      codec,
      flags,
    )
    .flags,
  )
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
    (Family::Matroska, Audited::Matroska),
    Road::Attachment,
    TrackKind::Attachment,
    CodecId::TTF,
    D::DEFAULT | D::FORCED,
  );
  assert!(attachment.filename && attachment.mime_type);
  assert!(attachment.language.keeps("und") && attachment.language.keeps(""));
  assert_eq!(attachment.flags, D::empty());
  let cover = kept(
    (Family::Mp4, Audited::Mov(MovMode::Mp4)),
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
    rewrites(
      (family, audited_for(family), TrackKind::Video),
      codec,
      extradata,
      true,
    )
    .collect::<Vec<_>>()
  };
  assert!(configuration_missing(
    Family::Mp4,
    CodecId::H264,
    &[],
    (true, false)
  ));
  assert!(configuration_missing(
    Family::Mp4,
    CodecId::HEVC,
    &[],
    (true, false)
  ));
  assert!(!configuration_missing(
    Family::Mp4,
    CodecId::H264,
    &[1, 2],
    (true, false)
  ));
  assert!(configuration_missing(
    Family::Matroska,
    CodecId::AV1,
    &[],
    (true, false)
  ));
  assert!(configuration_missing(
    Family::WebM,
    CodecId::AV1,
    &[],
    (true, false)
  ));
  assert!(!configuration_missing(
    Family::Other,
    CodecId::H264,
    &[],
    (true, false)
  ));
  // MP4's AAC, AV1, FLAC and MPEG-4 ALS, and Matroska's AAC, take their
  // first packet's new extradata as their configuration (R9: ruling 4a);
  // not ISMV's nor AVIF's, nor WebM's.
  for (family, audited, codec, takes) in [
    (Family::Mp4, Audited::Mov(MovMode::Mp4), CodecId::AAC, true),
    (Family::Mp4, Audited::Mov(MovMode::Mp4), CodecId::AV1, true),
    (Family::Mp4, Audited::Mov(MovMode::Mp4), CodecId::FLAC, true),
    (Family::Mp4, Audited::Mov(MovMode::Mp4), MP4ALS, true),
    (Family::Mp4, Audited::Mov(MovMode::Mov), CodecId::AAC, true),
    (
      Family::Mp4,
      Audited::Mov(MovMode::Mp4),
      CodecId::H264,
      false,
    ),
    (
      Family::Mp4,
      Audited::Mov(MovMode::Mp4),
      CodecId::OPUS,
      false,
    ),
    (Family::Mp4, Audited::Mov(MovMode::Ism), CodecId::AAC, false),
    (
      Family::Mp4,
      Audited::Mov(MovMode::Avif),
      CodecId::AV1,
      false,
    ),
    (Family::Matroska, Audited::Matroska, CodecId::AAC, true),
    (Family::Matroska, Audited::Matroska, CodecId::AV1, true),
    (Family::WebM, Audited::WebM, CodecId::AV1, true),
  ] {
    assert_eq!(
      first_packet_configures(family, audited, codec),
      takes,
      "{audited:?} {codec:?}"
    );
    assert_eq!(
      configuration_missing(family, codec, &[], (false, takes)),
      !takes,
      "{audited:?} {codec:?}"
    );
  }
  // AAC's comes from its first ADTS header with the automatic filters on;
  // with them off MP4 and Matroska need it stated, or brought by the first
  // packet.
  for family in [Family::Mp4, Family::Matroska] {
    assert!(
      configuration_missing(family, CodecId::AAC, &[], (false, false)),
      "{family:?}"
    );
    assert!(
      !configuration_missing(family, CodecId::AAC, &[], (true, false)),
      "{family:?}"
    );
    assert!(
      !configuration_missing(family, CodecId::AAC, &[0x11, 0x90], (false, false)),
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
  // Table H's field rows: a raw stream writes no time and no key flag;
  // IVF the presentation time alone; WAV's PCM, whose every frame is a key
  // frame, loses no flag.
  let field_rows = |audited, codec| {
    rewrites((Family::Other, audited, TrackKind::Video), codec, &[], true).collect::<Vec<_>>()
  };
  assert_eq!(
    field_rows(Audited::Raw(CodecId::H264), CodecId::H264),
    vec![Rewrite::Timeline, Rewrite::SyncFromBitstream]
  );
  assert_eq!(
    field_rows(Audited::Ivf, CodecId::VP9),
    vec![Rewrite::Durations, Rewrite::SyncFromBitstream]
  );
  assert_eq!(
    field_rows(Audited::Wav { w64: false }, CodecId::PCM_S16LE),
    vec![Rewrite::Timeline]
  );
  // ASF: the decode time and the key flag, no duration — and an audio
  // track's key flag, which its reader sets on every packet, is reported
  // where a packet is offered unmarked, not at open.
  for stream in [false, true] {
    assert_eq!(
      field_rows(Audited::Asf { stream }, CodecId::H264),
      vec![Rewrite::Durations]
    );
    assert_eq!(
      rewrites(
        (Family::Other, Audited::Asf { stream }, TrackKind::Audio),
        CodecId::AAC,
        &[],
        true
      )
      .collect::<Vec<_>>(),
      vec![Rewrite::Durations]
    );
  }
}

/// The audited muxer a family's unit laws stand for.
fn audited_for(family: Family) -> Audited {
  match family {
    Family::Mp4 => Audited::Mov(MovMode::Mp4),
    Family::Matroska => Audited::Matroska,
    Family::WebM => Audited::WebM,
    Family::Other => Audited::Mxf(MxfKind::Mxf),
  }
}

/// **The durations a container does not keep are reported at open**:
/// Matroska's and WebM's of a video or audio track with no default
/// duration. MP4's are reported while writing, where the packets do not
/// abut, so none is at open; APV's size prefix is not, as MP4's reader
/// takes it back off.
#[test]
fn a_duration_the_container_does_not_keep_is_reported() {
  let rewrites_of = |family, _kind: TrackKind, codec, default_duration| {
    rewrites(
      (family, audited_for(family), TrackKind::Video),
      codec,
      &[],
      default_duration,
    )
    .collect::<Vec<_>>()
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
  assert!(configuration_missing(
    Family::Mp4,
    CodecId::EVC,
    &[],
    (true, false)
  ));
  assert!(!configuration_missing(
    Family::Mp4,
    CodecId::VP9,
    &[],
    (true, false)
  ));
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
    packets: 6,
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
      audited: Some(Audited::Mov(MovMode::Mp4)),
      kind: TrackKind::Audio,
      codec: CodecId::EAC3,
      sample_rate: 48_000,
      frame_size,
      initial_padding: 0,
      format: -1,
      extradata: Vec::new(),
      alone: false,
    },
    written: false,
    cues: Cues::default(),
    ac3: Ac3State::default(),
    document: 0,
    adts: None,
    bytes: 0,
    place: 0,
    avi_frames: 0,
    ts_held: 0,
    ts_trim: Some(0),
    ts_pce: Vec::new(),
    ts_pce_pending: false,
    mp4_place: None,
    mp4_samples: 0,
    mp4_entries: 0,
    descriptions: Descriptions::default(),
    first_held: Vec::new(),
    moov: None,
    stsd: 0,
    apv: Vec::new(),
    key_times: None,
    mxf: MxfMirror::default(),
    buffered: 0,
    dts_shift: None,
    flv_rewrites: false,
    presented: None,
    configured: false,
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
  let (learned, _) = check_packet_rules(
    state,
    time_base,
    Neighbours::default(),
    (
      timed,
      Outputs {
        held: &[],
        emits: Emits::OWN,
        own: timed,
      },
      0,
    ),
    payload,
    step,
    Bounds::REAL,
  )?;
  let mut lines = Vec::new();
  learned_losses(state, track, &[], payload, &learned, true, &mut lines).expect("room");
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
    audited: Some(audited_for(family)),
    kind,
    codec,
    sample_rate: 48_000,
    frame_size: 1024,
    initial_padding: 1024,
    format: -1,
    extradata: vec![0x11, 0x90],
    alone: false,
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
/// on; AVIF's and ISMV's none; AAC's in the MOV family and Matroska for a
/// track with no configuration, and in FLV and LATM undecided until the
/// first packet; PGS's in Matroska's video muxer, and neither AAC's nor
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
    "matroska", "webm", "mov", "3gp", "mp4", "psp", "3g2", "ipod", "f4v",
  ] {
    assert_eq!(settle(muxer, None, CodecId::VP9, &[], true), vp9, "{muxer}");
    assert_eq!(
      settle(muxer, None, CodecId::VP9, &[], false),
      Filter::None,
      "{muxer}"
    );
  }
  for muxer in ["avif", "ismv", "mpegts", "flv", "nut"] {
    assert_eq!(
      settle(muxer, None, CodecId::VP9, &[], true),
      Filter::None,
      "{muxer}"
    );
  }
  for muxer in ["mp4", "matroska", "mov", "ipod"] {
    // A track of no configuration's first packet decides too: an ADTS one
    // inserts the filter, a configuration's packet none (R9: ruling 4a).
    assert_eq!(
      settle(muxer, None, CodecId::AAC, &[], true),
      Filter::AdtsUndecided {
        configured: false,
        parses: false,
        whole: true,
      },
      "{muxer}: no configuration, the first packet decides"
    );
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
  for muxer in ["flv", "latm"] {
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
    (true, false)
  ));
  assert!(configuration_missing(
    Family::Matroska,
    CodecId::AAC,
    &[],
    (false, false)
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
  const MP4: (Family, Audited) = (Family::Mp4, Audited::Mov(MovMode::Mp4));
  const MATROSKA: (Family, Audited) = (Family::Matroska, Audited::Matroska);
  // A stream of no tag, no pixel format, an unknown profile, not flagged
  // multilayer.
  const NONE: ((u32, i32, i32), bool) = ((0, -1, -99), false);
  let flac = [0u8; 34];
  assert!(!configuration_invalid(MP4, CodecId::FLAC, NONE, &flac));
  assert!(configuration_invalid(MP4, CodecId::FLAC, NONE, &flac[..33]));
  assert!(configuration_invalid(
    MP4,
    CodecId::FLAC,
    NONE,
    &[flac.as_slice(), &[0; 4]].concat()
  ));
  assert!(
    !configuration_invalid(MATROSKA, CodecId::FLAC, NONE, &flac[..33]),
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
  assert!(configuration_invalid(MP4, CodecId::OPUS, NONE, &head[..10]));
  // TTML's header strings: none, or a terminated string and another.
  assert!(ttml_header_strings(b""));
  assert!(ttml_header_strings(b"a\0b\0"));
  assert!(ttml_header_strings(b"\0\0"));
  assert!(!ttml_header_strings(b"a"));
  assert!(!ttml_header_strings(b"a\0"));
  assert!(!ttml_header_strings(b"a\0b"));
  assert!(configuration_invalid(
    MP4,
    CodecId::TTML,
    NONE,
    b"lavc-ttmlenca\0"
  ));
  assert!(!configuration_invalid(MP4, CodecId::TTML, NONE, b"<tt/>"));
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
      configuration_missing(Family::Mp4, codec, &[], (true, false)),
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
      .now
      .by_pts
  );
  assert!(
    !of(
      Family::Matroska,
      "matroska",
      &[(TrackKind::Video, CodecId::JPEG2000, true), audio]
    )
    .expect("mirrored")
    .now
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
  assert!(
    !of(Family::Other, "flv", &[video])
      .expect("mirrored")
      .now
      .by_pts
  );

  // One stream interleaved, beside SMPTE 2038 data: the first packet is
  // the first write, and settles the move.
  let alone = of(Family::Other, "mpegts", &[video, data]).expect("mirrored");
  assert!(alone.now.passthrough);
  let moved = alone
    .take_alone(0, (-40, -40, 0), Some(0), ms)
    .expect("before the write");
  assert!(moved.now.written);
  assert_eq!(moved.now.earliest, Some(Timestamp::new(-40, ms)));
  assert!(
    moved.take_alone(1, (-40, -40, 0), Some(0), ms).is_ok(),
    "lifted to zero"
  );
  assert_eq!(
    moved.take_alone(1, (-41, -41, 0), Some(0), ms),
    Err(Early::BeforeEarliest),
    "left before it"
  );

  // Two: written once both hold a packet, the early one moving the other.
  let both = of(Family::Matroska, "matroska", &[video, audio]).expect("mirrored");
  assert!(!both.now.passthrough);
  let held = both.take_alone(0, (0, 0, 0), Some(0), ms).expect("video");
  assert!(!held.now.written, "the audio may still come earlier");
  let moved = held
    .take_alone(1, (-30, -30, -21), Some(-21), ms)
    .expect("before the write");
  assert!(moved.now.written);
  assert_eq!(moved.now.earliest, Some(Timestamp::new(-9, ms)));
  assert!(moved.take_alone(0, (-9, -9, 0), Some(0), ms).is_ok());
  assert_eq!(
    moved.take_alone(0, (-10, -10, 0), Some(0), ms),
    Err(Early::BeforeEarliest)
  );

  // Each stream's distance rounded up to its own tick.
  let mixed = of(Family::Other, "mpegts", &[video, audio]).expect("mirrored");
  let fine = mixed
    .take_alone(0, (-1, -1, 0), Some(0), ninety)
    .and_then(|shift| shift.take_alone(1, (0, 0, 0), Some(0), ms))
    .expect("both held");
  assert!(fine.now.written);
  assert!(
    fine.take_alone(1, (-1, -1, 0), Some(0), ms).is_ok(),
    "a tick of 90 kHz lifts a millisecond"
  );
  assert_eq!(
    fine.take_alone(1, (-2, -2, 0), Some(0), ms),
    Err(Early::BeforeEarliest)
  );
  let coarse = mixed
    .take_alone(1, (-1, -1, 0), Some(0), ms)
    .and_then(|shift| shift.take_alone(0, (0, 0, 0), Some(0), ninety))
    .expect("both held");
  assert!(
    coarse.take_alone(0, (-90, -90, 0), Some(0), ninety).is_ok(),
    "a millisecond is 90 ticks"
  );
  assert_eq!(
    coarse.take_alone(0, (-91, -91, 0), Some(0), ninety),
    Err(Early::BeforeEarliest)
  );

  // The delay: written once the packets held span more than 10 s.
  let spanned = both
    .take_alone(0, (0, 0, 0), Some(0), ms)
    .and_then(|shift| shift.take_alone(0, (10_000, 10_000, 0), Some(0), ms))
    .expect("video");
  assert!(!spanned.now.written, "10 s is not past the delay");
  let late = spanned
    .take_alone(0, (10_001, 10_001, 0), Some(0), ms)
    .expect("video");
  assert!(late.now.written);
  assert_eq!(
    late.take_alone(1, (-22, -22, -21), Some(-21), ms),
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
    .take_alone(1, (0, 0, -21), Some(-21), ms)
    .and_then(|shift| shift.take_alone(1, (20_000, 20_000, -21), Some(-21), ms))
    .expect("audio");
  assert!(!waiting.now.written, "the interleaving waits on VP8");
  // ... and not by a subtitle's span.
  let cues = of(Family::Matroska, "matroska", &[video, audio, subtitle]).expect("mirrored");
  let waiting = cues
    .take_alone(0, (0, 0, 0), Some(0), ms)
    .and_then(|shift| shift.take_alone(2, (20_000, 20_000, 0), Some(0), ms))
    .expect("video and a cue");
  assert!(!waiting.now.written, "a subtitle's span is left aside");

  // SMPTE 2038 data, not interleaved, still counts among those holding.
  let counted = of(Family::Other, "mpegts", &[video, audio, data]).expect("mirrored");
  assert!(!counted.now.passthrough);
  let written = counted
    .take_alone(0, (0, 0, 0), Some(0), ms)
    .and_then(|shift| shift.take_alone(2, (0, 0, 0), Some(0), ms))
    .expect("video and data");
  assert!(
    written.now.written,
    "two hold a packet, as two are interleaved"
  );

  // FLV keeps the first packet it wrote as its reference, moved: a packet
  // of the stream the delay left behind, before it, it refuses; another
  // container takes it.
  for (muxer, refused) in [("flv", true), ("mpegts", false)] {
    let first = of(Family::Other, muxer, &[video, audio])
      .expect("mirrored")
      .take_alone(0, (5_000, 5_000, 0), Some(0), ms)
      .and_then(|shift| shift.take_alone(0, (15_001, 15_001, 0), Some(0), ms))
      .expect("video, past the delay");
    assert!(first.now.written);
    assert_eq!(
      first.take_alone(1, (2_000, 2_000, 0), Some(0), ms).err(),
      refused.then_some(Early::BeforeFirst),
      "{muxer}"
    );
    assert!(
      first.take_alone(1, (5_000, 5_000, 0), Some(0), ms).is_ok(),
      "{muxer}"
    );
  }
  let moved = of(Family::Other, "flv", &[video, audio])
    .expect("mirrored")
    .take_alone(0, (-100, -100, 0), Some(0), ms)
    .and_then(|shift| shift.take_alone(0, (9_901, 9_901, 0), Some(0), ms))
    .expect("video, past the delay");
  assert!(
    moved.take_alone(1, (-50, -50, 0), Some(0), ms).is_ok(),
    "moved past zero, the first"
  );
  assert_eq!(
    moved.take_alone(1, (-150, -150, 0), Some(0), ms),
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

/// **Matroska's header reserves an AAC track's output sampling frequency
/// only where its configuration states an extension sample rate, or none
/// is stated** (`get_aac_sample_rates`, `matroskaenc.c` 1116–1150;
/// 2129–2135): explicit SBR, a sync extension whose SBR flag is set; not
/// AAC LC, nor libavcodec's encoder's sync extension whose flag is clear.
#[test]
fn matroska_reserves_aacs_output_rate_as_its_header_writes_it() {
  assert!(aac_output_rate_reserved(&[]), "none stated");
  assert!(
    !aac_output_rate_reserved(&[0x11, 0x90]),
    "AAC LC, 48 kHz, stereo"
  );
  assert!(
    !aac_output_rate_reserved(&[0x11, 0x90, 0x56, 0xE5, 0x00]),
    "libavcodec's: a sync extension of SBR, its flag clear"
  );
  assert!(
    aac_output_rate_reserved(&[0x13, 0x10, 0x56, 0xE5, 0x98]),
    "a sync extension of SBR at 48 kHz, its flag set"
  );
  assert!(
    aac_output_rate_reserved(&[0x2B, 0x11, 0x88, 0x00]),
    "explicit SBR: 24 kHz, its extension 48 kHz"
  );
}

/// **libavformat's language table, as `ff_convert_lang_to` reads it**
/// (`avlanguage.c` 741–770): a bibliographic code to its ISO 639-1 one and
/// back; a terminological one to its bibliographic sibling; a code no
/// codespace names, none. ASF keeps a tag only where it is the round
/// trip's own.
#[test]
fn a_language_converts_as_libavformat_converts_it() {
  use LanguageSpace as L;
  assert_eq!(convert_language("eng", L::Iso639_1), Some("en"));
  assert_eq!(
    convert_language("en", L::Iso639_2Bibliographic),
    Some("eng")
  );
  assert_eq!(convert_language("fra", L::Iso639_1), Some("fr"));
  assert_eq!(
    convert_language("fr", L::Iso639_2Bibliographic),
    Some("fre")
  );
  assert_eq!(
    convert_language("fra", L::Iso639_2Bibliographic),
    Some("fre")
  );
  assert_eq!(convert_language("ger", L::Iso639_2Terminology), Some("deu"));
  assert_eq!(
    convert_language("eng", L::Iso639_2Terminology),
    Some("eng"),
    "B where T is the same"
  );
  assert_eq!(convert_language("zzz", L::Iso639_1), None);
  assert!(LanguageRule::ThroughIso639_1.keeps("eng"));
  assert!(!LanguageRule::ThroughIso639_1.keeps("fra"));
  assert!(!LanguageRule::ThroughIso639_1.keeps("en"));
  assert!(
    !LanguageRule::ThroughIso639_1.keeps("ace"),
    "no ISO 639-1 code: not written"
  );
}

/// **NUT's time bases, as its header sets them** (`choose_timebase`,
/// `nut_write_header`, `nutenc.c` 48–59, 749–753): an audio stream's its
/// sample rate's; any other's made at least 1/48000 fine, its numerator
/// divided by 2, 3, 5, 7, 9, 11 and 13 while it divides, then its
/// denominator doubled below 2^24 — every tick of the base handed a whole
/// number of the new one's.
#[test]
fn nuts_time_base_is_the_one_its_header_chooses() {
  assert_eq!(
    nut_time_base(TrackKind::Audio, 44_100, (1, 1000)),
    (1, 44_100)
  );
  assert_eq!(nut_time_base(TrackKind::Video, 0, (1, 1000)), (1, 64_000));
  assert_eq!(
    nut_time_base(TrackKind::Video, 0, (1001, 30_000)),
    (1, 60_000)
  );
  assert_eq!(nut_time_base(TrackKind::Video, 0, (1, 90_000)), (1, 90_000));
  assert_eq!(nut_time_base(TrackKind::Video, 0, (1, 25)), (1, 51_200));
  assert_eq!(
    nut_time_base(TrackKind::Subtitle, 0, (17, 1000)),
    (17, 1_024_000)
  );
  assert_eq!(
    nut_time_base(TrackKind::Audio, 0, (1, 1000)),
    (1, 64_000),
    "an audio stream of no sample rate as any other"
  );
}

/// **The Xiph headers, as `avpriv_split_xiph_headers` splits them**
/// (`libavcodec/xiph.c` 25–66): three lengths of 16 bits, the first the
/// identification header's; or Xiph lacing, its lengths runs of 255 and a
/// last byte, the third header the rest; any other refused.
#[test]
fn xiph_headers_split_as_libavcodec_splits_them() {
  let mut sized = vec![0, 30];
  sized.extend([1; 30]);
  sized.extend([0, 2, 7, 8, 0, 1, 9]);
  let headers = xiph_headers(&sized, 30).expect("three sized headers");
  assert_eq!(headers.map(<[u8]>::len), [30, 2, 1]);
  assert_eq!(headers[1], [7, 8]);
  assert!(
    xiph_headers(&sized, 42).is_none(),
    "the first length is the header's"
  );
  assert!(
    xiph_headers(&sized[..sized.len() - 1], 30).is_none(),
    "short"
  );
  let mut laced = vec![2, 0xff, 1, 3];
  laced.extend([5; 256]);
  laced.extend([6; 3]);
  laced.extend([7; 4]);
  let headers = xiph_headers(&laced, 30).expect("laced headers");
  assert_eq!(headers.map(<[u8]>::len), [256, 3, 4]);
  assert_eq!(headers[2], [7; 4]);
  assert!(xiph_headers(&[3, 0, 0], 30).is_none(), "neither form");
}

/// **libavformat's dictionary, as `av_packet_unpack_dictionary` unpacks
/// it** (`libavcodec/packet.c` 349–371): empty, or terminated pairs with
/// no empty key.
#[test]
fn a_dictionary_unpacks_as_libavformat_unpacks_it() {
  assert!(dictionary_unpacks(b""));
  assert!(dictionary_unpacks(b"title\0hello\0"));
  assert!(dictionary_unpacks(b"a\0\0b\0c\0"), "an empty value");
  assert!(!dictionary_unpacks(b"title\0hello"), "unterminated");
  assert!(!dictionary_unpacks(b"\0hello\0"), "an empty key");
  assert!(!dictionary_unpacks(b"title\0"), "a key with no value");
}

/// **The ISO 639-1 block's tail is found as the C library's `bsearch`
/// finds it** (R9: Fable R1; `avlanguage.c` 683–693, 748–755): libavformat's
/// block of ISO 639-1 codes is out of order at its end, so that a binary
/// search's answer there is its probes'. glibc's `bsearch` and macOS's —
/// FreeBSD's — walked here over every code of the table and a few it holds
/// not, agree, and the lookup follows them by name: every code each finds
/// found at its entry, the ten neither finds not, `xh` and `yi` among the
/// found — so that ASF keeps `xho` and `yid`, and not `chi`.
#[test]
fn the_iso_639_1_tail_is_found_as_the_c_librarys_bsearch_finds_it() {
  use core::cmp::Ordering;
  type Search = fn(&str, &[(&str, u16)]) -> Option<usize>;
  fn glibc(key: &str, block: &[(&str, u16)]) -> Option<usize> {
    let (mut low, mut high) = (0, block.len());
    while low < high {
      let middle = (low + high) / 2;
      match key.cmp(block[middle].0) {
        Ordering::Less => high = middle,
        Ordering::Greater => low = middle + 1,
        Ordering::Equal => return Some(middle),
      }
    }
    None
  }
  fn bsd(key: &str, block: &[(&str, u16)]) -> Option<usize> {
    let (mut base, mut limit) = (0, block.len());
    while limit != 0 {
      let probe = base + (limit >> 1);
      match key.cmp(block[probe].0) {
        Ordering::Equal => return Some(probe),
        Ordering::Greater => {
          base = probe + 1;
          limit -= 1;
        }
        Ordering::Less => {}
      }
      limit >>= 1;
    }
    None
  }
  let found = |search: Search, tag: &str| {
    LANGUAGE_SPACES.iter().find_map(|&(start, count)| {
      search(tag, &LANGUAGES[start..start + count]).map(|at| start + at)
    })
  };
  let mut unfound = Vec::new();
  for &(code, _) in LANGUAGES.iter().chain(&[("zzz", 0), ("xx", 0), ("", 0)]) {
    let by_glibc = found(glibc, code);
    assert_eq!(
      by_glibc,
      found(bsd, code),
      "{code}: the two libraries agree"
    );
    assert_eq!(language_entry(code), by_glibc, "{code}");
    if by_glibc.is_none() && LANGUAGES.iter().any(|&(held, _)| held == code) {
      unfound.push(code);
    }
  }
  assert_eq!(unfound, ISO_639_1_UNFOUND, "the codes named, and no other");
  assert!(LanguageRule::ThroughIso639_1.keeps("xho"));
  assert!(LanguageRule::ThroughIso639_1.keeps("yid"));
  assert!(!LanguageRule::ThroughIso639_1.keeps("chi"));
  assert!(!LanguageRule::ThroughIso639_1.keeps("zul"));
}

/// **MPEG-TS's descriptors' codes, as its loops write them** (R9: Codex R8
/// [high]; `mpegtsenc.c` 696–797): the ISO 639 descriptor's, each run of
/// three bytes before a comma or three bytes left in all — a comma among
/// them — any other run skipped, 63 at most; the DVB subtitle and teletext
/// descriptors', three bytes and one skipped while three are left. A tag
/// is kept where it reads back as its codes joined by commas.
#[test]
fn mpeg_ts_writes_a_languages_codes_as_its_loops_do() {
  let iso = |tag: &str| mpeg_ts_iso639_codes(tag.as_bytes()).join(&b","[..]);
  let dvb = |tag: &str| mpeg_ts_dvb_codes(tag.as_bytes()).join(&b","[..]);
  assert_eq!(iso("eng,fra"), b"eng,fra");
  assert_eq!(iso("english,fr,deu"), b"deu", "runs not of three skipped");
  assert_eq!(iso("eng,"), b"eng");
  assert_eq!(
    iso("eng,a,b"),
    b"eng,a,b",
    "three bytes left, a comma among them"
  );
  let many = (0..64)
    .map(|index: u8| {
      format!(
        "a{}{}",
        char::from(b'a' + index / 26),
        char::from(b'a' + index % 26)
      )
    })
    .collect::<Vec<_>>()
    .join(",");
  assert_eq!(
    mpeg_ts_iso639_codes(many.as_bytes()).len(),
    63,
    "255 / 4 codes"
  );
  assert_eq!(dvb("english"), b"eng,ish");
  assert_eq!(dvb("eng,fra"), b"eng,fra");
  assert_eq!(dvb("eng-fra"), b"eng,fra", "any byte skipped");
  assert!(mpeg_ts_dvb_codes(b"en").is_empty());
  assert!(LanguageRule::Iso639Codes.keeps("eng,fra"));
  assert!(!LanguageRule::Iso639Codes.keeps("english"));
  assert!(!LanguageRule::Iso639Codes.keeps(&many));
  assert!(LanguageRule::DvbCodes.keeps("eng,fra"));
  assert!(!LanguageRule::DvbCodes.keeps("eng-fra"));
  assert!(!LanguageRule::DvbCodes.keeps("en"));
}

/// **AVI places an XSUB cue one packet on, however far on its time** (R9:
/// Codex R8 [medium]; `avienc.c` 745–766, 407–408; `avidec.c` 686–699,
/// 1548–1590): `write_skip_frames` fills no gap of an XSUB stream, and its
/// reader counts its cues by packet in the main header's frame period — so
/// that a cue 100 000 ticks after the first is placed one packet on, never
/// refused, and reported where its time is not its count's; on the frames'
/// road, a stream of no block alignment's, it would be refused past the
/// 60 000 empty frames AVI fills (L84: AVI's tag tables name no XSUB, so
/// that the open refuses it first).
///
/// PLANT: XSUB on the frames' road refuses its far cue.
#[test]
fn an_avi_xsub_cue_is_placed_one_packet_on_however_far() {
  let place = |road: AviPlace| {
    let mut state = eac3_track(0);
    state.stream = Some((0, millis()));
    state.rules = PacketRules {
      avi: Some(road),
      ..PacketRules::default()
    };
    let mut steps = Vec::new();
    for at in [0, 100_000] {
      let timed = timed_at(at);
      let payload = Payload {
        bytes: b"[00:00:00.000-00:00:01.000]",
        key: true,
        flags: PacketFlags::KEY,
        side_data: &[],
      };
      let step = Step {
        owns: true,
        emits: Emits::OWN,
        largest: Some(payload.bytes.len() as u64),
        held: None,
        inserted: None,
      };
      match check_packet_rules(
        &state,
        millis(),
        Neighbours::default(),
        (
          timed,
          Outputs {
            held: &[],
            emits: Emits::OWN,
            own: timed,
          },
          0,
        ),
        payload,
        step,
        Bounds::REAL,
      ) {
        Ok((learned, _)) => {
          let (next, moved) = learned.place.expect("a place");
          state.place = next;
          state.written = true;
          steps.push(Ok(moved));
        }
        Err(refusal) => steps.push(Err(refusal)),
      }
    }
    steps
  };
  assert_eq!(
    place(AviPlace::Packets { reader: (1, 25) }),
    [Ok(false), Ok(true)],
    "the first at its place, the second one packet on, 40 ms, offered at 100 s"
  );
  assert!(matches!(
    place(AviPlace::Frames)[1],
    Err(FfmpegMuxRefusal::FrameGap { .. })
  ));
}

/// AV1's configurations as `ff_isom_write_av1c` takes them (R9: ruling 4c;
/// `av1.c` 125–502 in FFmpeg 9.0.1): an `av1C` record of version 1 and four
/// bytes or more; or OBUs whose headers parse, holding one sequence header
/// `parse_sequence_header` reads to its last bit — SVT-AV1's, a reduced
/// still picture's, a monochrome one's, a twelve-bit Professional one's,
/// and one of timing information, a `uvlc`, a decoder model, an operating
/// point of a tier, frame ids, order hints, screen content tools and sRGB
/// colour — and no empty metadata. The configuration FLV's
/// `extract_extradata` makes is the unit's sequence headers, one of no
/// bits split off unread.
///
/// PLANT: the exact bit count dropped takes a header of a bit to spare.
#[test]
fn av1c_is_refused_where_ff_isom_write_av1c_would_fail() {
  /// `fields`' bits from the front, then AV1's trailing bits: a one, and
  /// zeros to the byte.
  fn trailed(fields: &[(u32, u32)]) -> Vec<u8> {
    let mut bits = Vec::new();
    for &(value, count) in fields {
      for bit in (0..count).rev() {
        bits.push(((value >> bit) & 1) == 1);
      }
    }
    bits.push(true);
    while bits.len() % 8 != 0 {
      bits.push(false);
    }
    bits
      .chunks(8)
      .map(|byte| {
        byte
          .iter()
          .fold(0_u8, |acc, &bit| (acc << 1) | u8::from(bit))
      })
      .collect()
  }
  /// A sequence header OBU of `payload`, its size one LEB128 byte.
  fn sequence(payload: &[u8]) -> Vec<u8> {
    let mut obu = vec![0x0A, u8::try_from(payload.len()).expect("short")];
    obu.extend_from_slice(payload);
    obu
  }
  // SVT-AV1's, of the temporal unit the integration tests' fixture holds.
  let svt = [
    0x02, 0x00, 0x00, 0x05, 0x15, 0x7F, 0xFC, 0x4A, 0xF9, 0x00, 0x40,
  ];
  // Profile, still picture, reduced header, level; four bits each of the
  // dimensions' bits less one, then one bit each of the dimensions less
  // one; the six tool flags; then the colour.
  let reduced = |profile: u32| {
    [
      (profile, 3),
      (1, 1),
      (1, 1),
      (0, 5),
      (0, 4),
      (0, 4),
      (0, 1),
      (0, 1),
      (0, 3),
      (0, 3),
    ]
  };
  let with = |head: &[(u32, u32)], tail: &[(u32, u32)]| [head, tail].concat();
  // 8-bit 4:2:0: no high bit depth, not monochrome, no colour description,
  // a colour range, a chroma sample position, no separate UV delta, no
  // film grain.
  let still = trailed(&with(
    &reduced(0),
    &[(0, 1), (0, 1), (0, 1), (0, 1), (0, 2), (0, 1), (0, 1)],
  ));
  // Monochrome: a colour range, and no separate UV delta.
  let mono = trailed(&with(
    &reduced(0),
    &[(0, 1), (1, 1), (0, 1), (0, 1), (0, 1)],
  ));
  let mono_spare = trailed(&with(
    &reduced(0),
    &[(0, 1), (1, 1), (0, 1), (0, 1), (0, 1), (0, 1)],
  ));
  // Professional, twelve bits, chroma subsampled both ways and its sample
  // position read.
  let professional = trailed(&with(
    &reduced(2),
    &[
      (1, 1),
      (1, 1),
      (0, 1),
      (0, 1),
      (0, 1),
      (1, 1),
      (1, 1),
      (0, 2),
      (0, 1),
      (0, 1),
    ],
  ));
  #[rustfmt::skip]
  let timed = trailed(&[
    (0, 3), (0, 1), (0, 1),
    // Timing information; an equal picture interval, its `uvlc` of three
    // leading zeros; a decoder model of 4-bit buffer delays.
    (1, 1), (1, 32), (30, 32), (1, 1), (0, 3), (1, 1), (0b101, 3),
    (1, 1), (3, 5), (1, 32), (0, 10),
    // A display delay; one operating point of level 8, so a tier bit, its
    // decoder model and its display delay.
    (1, 1), (0, 5), (0, 12), (8, 5), (1, 1), (1, 1), (5, 4), (6, 4), (0, 1), (1, 1), (2, 4),
    // Four-bit dimensions; frame ids.
    (3, 4), (3, 4), (15, 4), (15, 4), (1, 1), (0, 7),
    // Tools; order hints; screen content tools chosen, integer motion
    // vectors chosen; the order hint's bits.
    (0, 3), (0, 4), (1, 1), (0, 2), (1, 1), (1, 1), (6, 3), (0, 3),
    // BT.709 primaries, sRGB transfer, the identity matrix: no colour
    // range, no subsampling.
    (0, 1), (0, 1), (1, 1), (1, 8), (13, 8), (0, 8), (0, 1), (0, 1),
  ]);
  let cat = |parts: &[&[u8]]| parts.concat();
  for (name, config, written) in [
    ("none", Vec::new(), false),
    ("an av1C record", vec![0x81, 0, 0, 0], true),
    ("a short record", vec![0x81, 0, 0], false),
    ("a record of version 2", vec![0x82, 0, 0, 0], false),
    ("SVT-AV1's", sequence(&svt), true),
    ("a reduced still picture's", sequence(&still), true),
    ("a monochrome one's", sequence(&mono), true),
    (
      "a monochrome one of a bit to spare",
      sequence(&mono_spare),
      false,
    ),
    ("a Professional one's", sequence(&professional), true),
    ("a timed one's", sequence(&timed), true),
    ("two", cat(&[&sequence(&svt), &sequence(&svt)]), false),
    (
      "empty metadata first",
      cat(&[&[0x2A, 0x00], &sequence(&svt)]),
      false,
    ),
    (
      "metadata first",
      cat(&[&[0x2A, 0x01, 0x01], &sequence(&svt)]),
      true,
    ),
    (
      "a delimiter, padding",
      cat(&[&[0x12, 0x00], &sequence(&svt), &[0x7A, 0x00]]),
      true,
    ),
    ("a delimiter alone", vec![0x12, 0x00], false),
    ("an empty sequence header", vec![0x0A, 0x00], false),
    ("a one bit past", sequence(&cat(&[&svt, &[0x80]])), false),
    ("a zero byte past", sequence(&cat(&[&svt, &[0x00]])), true),
    (
      "no trailing one",
      sequence(&cat(&[&svt[..10], &[0x00]])),
      false,
    ),
    ("of no size field", cat(&[&[0x08], &svt]), true),
    ("a header cut", vec![0x0A], false),
    ("an extension and no size", vec![0x0E, 0x00], false),
    ("a size past the end", cat(&[&[0x0A, 0x0C], &svt]), false),
  ] {
    assert_eq!(av1c_written(&config), written, "{name}");
  }
  let frame: &[u8] = &[0x32, 0x01, 0x10];
  let unit = cat(&[&[0x12, 0x00], &sequence(&svt), frame]);
  assert_eq!(
    av1_extracted_written(&unit),
    Some(av1c_written(&sequence(&svt))),
    "the unit's sequence header, as written of itself"
  );
  assert_eq!(av1_extracted_written(&unit), Some(true));
  assert_eq!(av1_extracted_written(&cat(&[&[0x12, 0x00], frame])), None);
  // A sequence header of no bits split off unread: the configuration made
  // of the rest written, where the unit as a configuration is not.
  let unread = cat(&[&[0x0A, 0x01, 0x00], &sequence(&svt)]);
  assert!(!av1c_written(&unread));
  assert_eq!(av1_extracted_written(&unread), Some(true));
  assert_eq!(
    av1_extracted_written(&cat(&[&sequence(&svt), frame, &sequence(&svt)])),
    Some(false),
    "two sequence headers"
  );
}

/// Bits as the standards' syntax writes them, for the configuration
/// records' laws.
#[derive(Clone, Default)]
struct Syntax(Vec<bool>);

impl Syntax {
  /// `u(n)`.
  fn u(mut self, value: u64, count: u32) -> Self {
    for bit in (0..count).rev() {
      self.0.push((value >> bit) & 1 == 1);
    }
    self
  }

  /// `ue(v)`.
  fn ue(self, value: u32) -> Self {
    let code = u64::from(value) + 1;
    let len = 64 - code.leading_zeros();
    self.u(0, len - 1).u(code, len)
  }

  /// `se(v)`.
  fn se(self, value: i32) -> Self {
    let mapped = if value > 0 {
      2 * value.unsigned_abs() - 1
    } else {
      2 * value.unsigned_abs()
    };
    self.ue(mapped)
  }

  /// Ones to the byte's end.
  fn aligned(mut self) -> Self {
    while self.0.len() % 8 != 0 {
      self.0.push(true);
    }
    self
  }

  /// The RBSP: its stop bit and zeros to its byte's end.
  fn rbsp(mut self) -> Vec<u8> {
    self.0.push(true);
    while self.0.len() % 8 != 0 {
      self.0.push(false);
    }
    self
      .0
      .chunks(8)
      .map(|byte| byte.iter().fold(0, |acc, &bit| (acc << 1) | u8::from(bit)))
      .collect()
  }
}

/// A NAL unit of `header` and `rbsp`, an emulation prevention byte after
/// every two zeros a byte of 3 or less follows.
fn escaped(header: &[u8], rbsp: &[u8]) -> Vec<u8> {
  let mut out = header.to_vec();
  let mut zeros = 0;
  for &byte in rbsp {
    if zeros >= 2 && byte <= 3 {
      out.push(3);
      zeros = 0;
    }
    out.push(byte);
    zeros = if byte == 0 { zeros + 1 } else { 0 };
  }
  out
}

/// `units`, each behind a 4-byte start code.
fn start_coded(units: &[&[u8]]) -> Vec<u8> {
  units
    .iter()
    .flat_map(|unit| [&[0_u8, 0, 0, 1][..], unit].concat())
    .collect()
}

/// `count` copies of `unit`.
fn copies(unit: &[u8], count: usize) -> Vec<&[u8]> {
  vec![unit; count]
}

/// A NAL unit of `header` and `len - header.len()` bytes of 0x11.
fn sized(header: &[u8], len: usize) -> Vec<u8> {
  let mut unit = header.to_vec();
  unit.resize(len, 0x11);
  unit
}

#[test]
fn h264s_configuration_is_judged_as_ff_isom_write_avcc_writes_it() {
  use records::{Wrote, avcc};
  let sps: &[u8] = &[0x67, 0x64, 0x00, 0x1F, 0xAC, 0xD9, 0x40];
  let pps: &[u8] = &[0x68, 0xEB, 0xE3, 0xCB, 0x22, 0xC0];
  let extension: &[u8] = &[0x6D, 0x80];
  let with = |sps_n: usize, pps_n: usize, ext_n: usize| {
    let mut units = copies(sps, sps_n);
    units.extend(copies(pps, pps_n));
    units.extend(copies(extension, ext_n));
    start_coded(&units)
  };
  let big_sps = sized(&[0x67, 0x64], 65_536);
  let big_pps = sized(&[0x68], 65_536);
  let big_extension = sized(&[0x6D], 65_536);
  let largest_sps = sized(&[0x67, 0x64], 65_535);
  for (name, config, wrote) in [
    (
      "an avcC record",
      vec![1, 0x64, 0, 0x1F, 0xFF, 0xE1, 0x00],
      Wrote::Whole,
    ),
    (
      "a record of six bytes",
      vec![1, 0x64, 0, 0x1F, 0xFF, 0xE1],
      Wrote::Failed,
    ),
    (
      "six bytes of a start code",
      vec![0, 0, 0, 1, 0x67, 0x64],
      Wrote::Failed,
    ),
    ("an SPS and a PPS", with(1, 1, 0), Wrote::Whole),
    (
      "behind 3-byte start codes",
      [&[0, 0, 1][..], sps, &[0, 0, 1], pps].concat(),
      Wrote::Whole,
    ),
    ("31 SPSs", with(31, 1, 0), Wrote::Whole),
    ("32 SPSs", with(32, 1, 0), Wrote::Failed),
    ("255 PPSs", with(1, 255, 0), Wrote::Whole),
    ("256 PPSs", with(1, 256, 0), Wrote::Failed),
    ("255 SPS extensions", with(1, 1, 255), Wrote::Whole),
    ("256 SPS extensions", with(1, 1, 256), Wrote::Failed),
    (
      "an SPS of 65 535 bytes",
      start_coded(&[&largest_sps, pps]),
      Wrote::Whole,
    ),
    (
      "an SPS of 65 536 bytes",
      start_coded(&[&big_sps, pps]),
      Wrote::Failed,
    ),
    (
      "a PPS of 65 536 bytes",
      start_coded(&[sps, &big_pps]),
      Wrote::Failed,
    ),
    (
      "an SPS extension of 65 536 bytes",
      start_coded(&[sps, pps, &big_extension]),
      Wrote::Failed,
    ),
    ("no PPS", with(1, 0, 0), Wrote::Failed),
    ("no SPS", with(0, 2, 0), Wrote::Failed),
    (
      "an SPS of three bytes",
      start_coded(&[&[0x67, 0x64, 0x01], pps]),
      Wrote::Failed,
    ),
    (
      "an SPS of four bytes",
      start_coded(&[&[0x67, 0x64, 0x01, 0x1F], pps]),
      Wrote::Whole,
    ),
    (
      "an SPS's trailing zeros dropped",
      start_coded(&[&[0x67, 0x64, 0x01, 0x00, 0x00], pps]),
      Wrote::Failed,
    ),
  ] {
    assert_eq!(avcc(&config), wrote, "{name}");
  }
}

/// An HEVC NAL unit header: its type, its layer, a temporal id of zero.
fn hevc_header(kind: u8, layer: u8) -> [u8; 2] {
  ((u16::from(kind) << 9) | (u16::from(layer) << 3) | 1).to_be_bytes()
}

/// A general profile, tier and level: Main, level 3.1.
fn hevc_general_ptl(syntax: Syntax) -> Syntax {
  syntax
    .u(0, 2)
    .u(0, 1)
    .u(1, 5)
    .u(0x6000_0000, 32)
    .u(0x9000_0000_0000, 48)
    .u(93, 8)
}

/// An HEVC VPS `id` of one layer, or of two whose second, `alpha`, its
/// extension names an auxiliary alpha layer.
fn hevc_vps(id: u8, alpha: Option<u8>) -> Vec<u8> {
  let syntax = Syntax::default()
    .u(u64::from(id), 4)
    .u(1, 1)
    .u(1, 1)
    .u(u64::from(alpha.is_some()), 6)
    .u(0, 3)
    .u(1, 1)
    .u(0xFFFF, 16);
  let syntax = hevc_general_ptl(syntax)
    .u(1, 1)
    .ue(4)
    .ue(0)
    .ue(0)
    .u(0, 6)
    .ue(0)
    .u(0, 1);
  let syntax = match alpha {
    None => syntax.u(0, 1),
    // The extension: the second layer's level, no splitting, the
    // auxiliary dimension alone, of one bit, its layer id given, alpha.
    Some(layer) => syntax
      .u(1, 1)
      .aligned()
      .u(93, 8)
      .u(0, 1)
      .u(0x1000, 16)
      .u(0, 3)
      .u(1, 1)
      .u(u64::from(layer), 6)
      .u(1, 1),
  };
  escaped(&hevc_header(32, 0), &syntax.rbsp())
}

/// An HEVC SPS of the base layer, or of `layer` as its VPS `vps`'s
/// multilayer extension, of `short_term` short-term reference picture
/// sets and `long_term` long-term reference pictures where any.
fn hevc_sps(layer: u8, vps: u8, short_term: u32, long_term: Option<u32>) -> Vec<u8> {
  let mut syntax = Syntax::default().u(u64::from(vps), 4);
  if layer == 0 {
    syntax = hevc_general_ptl(syntax.u(0, 3).u(1, 1))
      .ue(0)
      .ue(1)
      .ue(64)
      .ue(64)
      .u(0, 1)
      .ue(0)
      .ue(0);
  } else {
    syntax = syntax.u(7, 3).ue(1).u(0, 1);
  }
  syntax = syntax.ue(4);
  if layer == 0 {
    syntax = syntax.u(1, 1).ue(4).ue(0).ue(0);
  }
  syntax = syntax
    .ue(0)
    .ue(1)
    .ue(0)
    .ue(1)
    .ue(0)
    .ue(0)
    .u(0, 4)
    .ue(short_term);
  for index in 0..short_term {
    if index > 0 {
      syntax = syntax.u(0, 1);
    }
    syntax = syntax.ue(1).ue(0).ue(0).u(1, 1);
  }
  syntax = match long_term {
    None => syntax.u(0, 1),
    Some(count) => {
      let mut syntax = syntax.u(1, 1).ue(count);
      for _ in 0..count {
        syntax = syntax.u(0, 8).u(1, 1);
      }
      syntax
    }
  };
  escaped(&hevc_header(33, layer), &syntax.u(0, 4).rbsp())
}

/// An HEVC PPS.
fn hevc_pps() -> Vec<u8> {
  let syntax = Syntax::default()
    .ue(0)
    .ue(0)
    .u(0, 7)
    .ue(0)
    .ue(0)
    .se(0)
    .u(0, 3)
    .se(0)
    .se(0)
    .u(0, 6)
    .u(0, 4);
  escaped(&hevc_header(34, 0), &syntax.rbsp())
}

/// An `hvcC` of `arrays`, each its NAL unit type and units.
fn hvcc_record(arrays: &[(u8, Vec<Vec<u8>>)]) -> Vec<u8> {
  let mut record = vec![
    1, 0x01, 0x60, 0, 0, 0, 0x90, 0, 0, 0, 0, 0, 93, 0xF0, 0x00, 0xFC, 0xFD, 0xF8, 0xF8, 0x00,
    0x00, 0x0F,
  ];
  record.push(u8::try_from(arrays.len()).expect("arrays"));
  for (kind, units) in arrays {
    record.push(0x80 | kind);
    record.extend_from_slice(&u16::try_from(units.len()).expect("count").to_be_bytes());
    for unit in units {
      record.extend_from_slice(&u16::try_from(unit.len()).expect("length").to_be_bytes());
      record.extend_from_slice(unit);
    }
  }
  record
}

#[test]
fn hevcs_configuration_is_judged_as_ff_isom_write_hvcc_writes_it() {
  use records::{Wrote, hvcc};
  let (vps, sps, pps) = (hevc_vps(0, None), hevc_sps(0, 0, 1, None), hevc_pps());
  let sei = |len: usize| sized(&hevc_header(39, 0), len);
  let record = |vps_n: usize, sps_n: usize, pps_n: usize| {
    hvcc_record(&[
      (32, vec![vps.clone(); vps_n]),
      (33, vec![sps.clone(); sps_n]),
      (34, vec![pps.clone(); pps_n]),
    ])
  };
  let whole = record(1, 1, 1);
  let annex = |units: &[&[u8]]| start_coded(units);
  let base = annex(&[&vps, &sps, &pps]);
  let alpha_vps = hevc_vps(0, Some(1));
  let layered = hvcc_record(&[
    (32, vec![vps.clone()]),
    (
      33,
      [vec![sps.clone(); 16], vec![hevc_sps(1, 0, 0, None)]].concat(),
    ),
    (34, vec![pps.clone()]),
  ]);
  let seis = hvcc_record(&[(39, vec![Vec::new(); 65_535]), (39, vec![Vec::new()])]);
  #[rustfmt::skip]
  let cases: Vec<(&str, Vec<u8>, bool, Wrote)> = vec![
    ("five bytes", vec![1, 0, 0, 0, 0], false, Wrote::Failed),
    ("five bytes of a start code", vec![0, 0, 0, 1, 0x40], false, Wrote::Failed),
    ("a record", whole.clone(), true, Wrote::Whole),
    ("a record of 22 bytes", whole[..22].to_vec(), false, Wrote::Failed),
    ("a record whose unit runs past it", whole[..whole.len() - 1].to_vec(), false, Wrote::Corrupt),
    ("a record of no VPS, incomplete", record(0, 1, 1), false, Wrote::Whole),
    ("a record of no VPS, complete", record(0, 1, 1), true, Wrote::Failed),
    ("16 SPSs, complete", record(1, 16, 1), true, Wrote::Whole),
    ("17 SPSs, complete", record(1, 17, 1), true, Wrote::Failed),
    ("64 PPSs, complete", record(1, 1, 64), true, Wrote::Whole),
    ("65 PPSs, complete", record(1, 1, 65), true, Wrote::Failed),
    ("17 VPSs, complete", record(17, 1, 1), true, Wrote::Failed),
    ("no PPS, complete", record(1, 1, 0), true, Wrote::Failed),
    ("a layer's SPS beside 16, complete", layered, true, Wrote::Whole),
    ("65 536 SEIs over two arrays", seis, false, Wrote::Failed),
    ("of neither form", [&[2, 0, 0, 1][..], &vps].concat(), false, Wrote::Failed),
    ("Annex B", base.clone(), true, Wrote::Whole),
    (
      "an SPS of 31 long-term pictures",
      annex(&[&vps, &hevc_sps(0, 0, 1, Some(31)), &pps]), true, Wrote::Whole,
    ),
    (
      "an SPS of 32 long-term pictures",
      annex(&[&vps, &hevc_sps(0, 0, 1, Some(32)), &pps]), false, Wrote::Failed,
    ),
    (
      "an SPS of 65 short-term sets, the trust model's",
      annex(&[&vps, &hevc_sps(0, 0, 65, None), &pps]), false, Wrote::Whole,
    ),
    (
      "a layer's SPS no alpha names",
      annex(&[&vps, &sps, &hevc_sps(1, 0, 0, Some(32)), &pps]), true, Wrote::Whole,
    ),
    (
      "the alpha layer's SPS of 32 long-term pictures",
      annex(&[&alpha_vps, &sps, &hevc_sps(1, 0, 0, Some(32)), &pps]), true, Wrote::Failed,
    ),
    (
      "the alpha layer's SPS of 31",
      annex(&[&alpha_vps, &sps, &hevc_sps(1, 0, 0, Some(31)), &pps]), true, Wrote::Whole,
    ),
    (
      "the alpha layer's SPS of a VPS the record holds not",
      annex(&[&alpha_vps, &sps, &hevc_sps(1, 1, 0, None), &pps]), true, Wrote::Failed,
    ),
    ("an SEI of 65 535 bytes", annex(&[&vps, &sps, &pps, &sei(65_535)]), true, Wrote::Whole),
    ("an SEI of 65 536 bytes", annex(&[&vps, &sps, &pps, &sei(65_536)]), true, Wrote::Corrupt),
  ];
  for (name, config, complete, wrote) in cases {
    assert_eq!(hvcc(&config, complete), wrote, "{name}");
  }
  // What `init_get_bits8` takes: a record, and a unit's RBSP — of a layer
  // the record skips, which no other condition reaches.
  let mut record = vec![0_u8; records::GET_BITS_BYTES_MAX + 1];
  record[0] = 1;
  assert_eq!(
    hvcc(&record, false),
    Wrote::Failed,
    "a record past the reader"
  );
  record.pop();
  assert_eq!(
    hvcc(&record, false),
    Wrote::Whole,
    "a record of what the reader takes"
  );
  drop(record);
  let layered = |len: usize| {
    let mut config = start_coded(&[&vps, &sps, &pps]);
    config.extend_from_slice(&[0, 0, 0, 1]);
    config.extend_from_slice(&hevc_header(39, 1));
    config.resize(config.len() + len - 2, 0x11);
    config
  };
  assert_eq!(
    hvcc(&layered(records::GET_BITS_BYTES_MAX + 1), false),
    Wrote::Failed,
    "a skipped layer's unit past the reader"
  );
  assert_eq!(
    hvcc(&layered(records::GET_BITS_BYTES_MAX), false),
    Wrote::Whole,
    "a skipped layer's unit the reader takes"
  );
}

/// A VVC NAL unit of `kind`, of a byte past its header.
fn vvc_unit(kind: u8) -> Vec<u8> {
  vec![0x00, (kind << 3) | 1, 0x80]
}

#[test]
fn vvcs_configuration_is_judged_as_ff_isom_write_vvcc_writes_it() {
  use records::{Wrote, vvcc};
  let (vps, sps, pps, sei) = (vvc_unit(14), vvc_unit(15), vvc_unit(16), vvc_unit(23));
  let with = |vps_n: usize, sps_n: usize, pps_n: usize| {
    let mut units = copies(&vps, vps_n);
    units.extend(copies(&sps, sps_n));
    units.extend(copies(&pps, pps_n));
    start_coded(&units)
  };
  let big = sized(&[0x00, (23 << 3) | 1], 65_536);
  let largest = sized(&[0x00, (23 << 3) | 1], 65_535);
  #[rustfmt::skip]
  let cases: Vec<(&str, Vec<u8>, Wrote)> = vec![
    ("five bytes", vec![0xFF, 0, 0, 0, 0], Wrote::Failed),
    ("a record", vec![0xFF, 0, 0, 0, 0, 0], Wrote::Whole),
    ("of neither form", vec![0x02, 0, 0, 1, 0, 0x79], Wrote::Failed),
    ("an SPS and a PPS", with(0, 1, 1), Wrote::Whole),
    ("no SPS", with(1, 0, 1), Wrote::Failed),
    ("no PPS", with(1, 1, 0), Wrote::Failed),
    ("16 VPSs", with(16, 1, 1), Wrote::Whole),
    ("17 VPSs", with(17, 1, 1), Wrote::Failed),
    ("16 SPSs", with(0, 16, 1), Wrote::Whole),
    ("17 SPSs", with(0, 17, 1), Wrote::Failed),
    ("64 PPSs", with(0, 1, 64), Wrote::Whole),
    ("65 PPSs", with(0, 1, 65), Wrote::Failed),
    ("an SEI of 65 535 bytes", start_coded(&[&sps, &pps, &largest]), Wrote::Whole),
    ("an SEI of 65 536 bytes", start_coded(&[&sps, &pps, &big]), Wrote::Corrupt),
    (
      "65 535 SEIs",
      start_coded(&[&[&sps[..], &pps[..]][..], &copies(&sei, 65_535)].concat()),
      Wrote::Whole,
    ),
    (
      "65 536 SEIs",
      start_coded(&[&[&sps[..], &pps[..]][..], &copies(&sei, 65_536)].concat()),
      Wrote::Corrupt,
    ),
  ];
  for (name, config, wrote) in cases {
    assert_eq!(vvcc(&config), wrote, "{name}");
  }
  // An SEI past what `init_get_bits8` takes: refused, before its length is
  // cut.
  let mut config = start_coded(&[&sps, &pps]);
  config.extend_from_slice(&[0, 0, 0, 1, 0x00, (23 << 3) | 1]);
  config.resize(config.len() + records::GET_BITS_BYTES_MAX - 1, 0x11);
  assert_eq!(vvcc(&config), Wrote::Failed, "an SEI past the reader");
}

/// EVC units, each behind its 4-byte length.
fn length_prefixed_units(units: &[&[u8]]) -> Vec<u8> {
  units
    .iter()
    .flat_map(|unit| {
      [
        &u32::try_from(unit.len()).expect("length").to_be_bytes()[..],
        unit,
      ]
      .concat()
    })
    .collect()
}

/// An EVC NAL unit header of `kind`.
fn evc_header(kind: u8) -> [u8; 2] {
  [(kind + 1) << 1, 0]
}

/// An EVC SPS `id` of `chroma` and bit depths `luma` and `chroma_depth`
/// less eight.
fn evc_sps(id: u32, chroma: u32, luma: u32, chroma_depth: u32) -> Vec<u8> {
  let syntax = Syntax::default()
    .ue(id)
    .u(1, 8)
    .u(51, 8)
    .u(0, 32)
    .u(0, 32)
    .ue(chroma)
    .ue(64)
    .ue(64)
    .ue(luma)
    .ue(chroma_depth)
    .u(0, 1);
  [&evc_header(24)[..], &syntax.rbsp()].concat()
}

#[test]
fn evcs_configuration_is_judged_as_ff_isom_write_evcc_writes_it() {
  use records::{Wrote, evcc};
  let sps = evc_sps(0, 1, 2, 2);
  let pps = [&evc_header(25)[..], &[0x80]].concat();
  let sei = [&evc_header(28)[..], &[0x80]].concat();
  let units = |units: &[&[u8]]| length_prefixed_units(units);
  let big = sized(&evc_header(28), 65_536);
  #[rustfmt::skip]
  let cases: Vec<(&str, Vec<u8>, Wrote)> = vec![
    ("seven bytes", vec![0, 0, 0, 3, 0x32, 0, 0x80], Wrote::Failed),
    ("a record", vec![1, 1, 51, 0, 0, 0, 0, 0], Wrote::Whole),
    ("an SPS and a PPS", units(&[&sps, &pps]), Wrote::Whole),
    ("no SPS", units(&[&pps, &pps]), Wrote::Failed),
    ("16 SPSs", units(&copies(&sps, 16)), Wrote::Whole),
    ("17 SPSs", units(&copies(&sps, 17)), Wrote::Failed),
    ("a bit depth of 14", units(&[&evc_sps(0, 1, 6, 6), &pps]), Wrote::Whole),
    ("a luma bit depth of 15", units(&[&evc_sps(0, 1, 7, 6), &pps]), Wrote::Failed),
    ("a chroma bit depth of 15", units(&[&evc_sps(0, 1, 6, 7), &pps]), Wrote::Failed),
    ("a bit depth held in 8 bits", units(&[&evc_sps(0, 1, 262, 6), &pps]), Wrote::Whole),
    ("an id past 15, the trust model's", units(&[&evc_sps(16, 1, 7, 7), &pps]), Wrote::Whole),
    ("a chroma format past 3, the trust model's", units(&[&evc_sps(0, 4, 7, 7), &pps]), Wrote::Whole),
    ("an SPS of a byte", units(&[&sps, &[0x32], &pps]), Wrote::Failed),
    ("a unit's header past what is left", units(&[&sps, &[0x34]]), Wrote::Failed),
    ("a unit of the forbidden bit", units(&[&sps, &[0xB2, 0x00]]), Wrote::Failed),
    ("a unit of no type", units(&[&sps, &[0x00, 0x00]]), Wrote::Failed),
    ("a unit past what is left, the walk stopped", [units(&[&sps]), vec![0, 0, 0, 9, 0x50, 0]].concat(), Wrote::Whole),
    ("an SEI of 65 536 bytes", units(&[&sps, &big]), Wrote::Corrupt),
    ("65 535 SEIs", units(&[&[&sps[..]][..], &copies(&sei, 65_535)].concat()), Wrote::Whole),
    ("65 536 SEIs", units(&[&[&sps[..]][..], &copies(&sei, 65_536)].concat()), Wrote::Corrupt),
  ];
  for (name, config, wrote) in cases {
    assert_eq!(evcc(&config), wrote, "{name}");
  }
}

/// An LCEVC unit of `kind` — 29 IDR, 28 non-IDR — of `blocks`, each a
/// payload's size type and type byte, then its bytes, and its stop byte.
fn lcevc_unit_of(kind: u8, blocks: &[&[u8]]) -> Vec<u8> {
  let mut unit = vec![0x40 | (kind << 1) | 1, 0xFF];
  for block in blocks {
    unit.extend_from_slice(block);
  }
  unit.push(0x80);
  unit
}

#[test]
fn lcevcs_configuration_is_judged_as_ff_isom_write_lvcc_writes_it() {
  use records::{Wrote, lvcc};
  // A sequence configuration of a byte; a global configuration of two, of
  // resolution type 17 — 1280 × 720 — or another.
  let sequence: &[u8] = &[0x20, 0x11];
  let global = |resolution: u8| vec![0x41, resolution << 1, 0x40];
  let idr = lcevc_unit_of(29, &[sequence, &global(17)]);
  let annex = |units: &[&[u8]]| start_coded(units);
  let info = |len: usize| {
    let mut block = vec![0xE5];
    let mut size = Vec::new();
    let mut left = len;
    loop {
      size.insert(0, u8::try_from(left & 0x7F).expect("seven bits"));
      left >>= 7;
      if left == 0 {
        break;
      }
    }
    let last = size.len() - 1;
    for byte in &mut size[..last] {
      *byte |= 0x80;
    }
    block.extend(size);
    block.resize(block.len() + len, 0x11);
    block
  };
  #[rustfmt::skip]
  let cases: Vec<(&str, Vec<u8>, Wrote)> = vec![
    ("six bytes", vec![0, 0, 0, 1, 0x7B, 0xFF], Wrote::Failed),
    ("an lvcC of six bytes", vec![1, 0, 0, 0, 0, 0], Wrote::Failed),
    ("an lvcC record", vec![1, 0, 0, 0, 0, 0, 0], Wrote::Whole),
    ("an IDR unit", annex(&[&idr]), Wrote::Whole),
    ("a non-IDR unit", annex(&[&lcevc_unit_of(28, &[sequence, &global(17)])]), Wrote::Whole),
    ("no IDR or non-IDR unit", annex(&[&lcevc_unit_of(1, &[sequence])]), Wrote::Failed),
    ("a unit of no global configuration", annex(&[&lcevc_unit_of(29, &[sequence])]), Wrote::Failed),
    ("a unit of no sequence configuration", annex(&[&lcevc_unit_of(29, &[&global(17)])]), Wrote::Failed),
    (
      "a global configuration of a reserved resolution, the trust model's",
      annex(&[&lcevc_unit_of(29, &[sequence, &global(50)])]), Wrote::Whole,
    ),
    ("a reserved size type, the trust model's", annex(&[&lcevc_unit_of(29, &[&[0xC0, 0, 0]])]), Wrote::Whole),
    // Written: 3 bytes, the sequence block's 2, the global block's 3, and
    // the information block's type byte, its 3 size bytes and payload.
    (
      "a unit of 65 535 bytes written",
      annex(&[&lcevc_unit_of(29, &[sequence, &info(65_535 - 3 - 2 - 3 - 4), &global(17)])]),
      Wrote::Whole,
    ),
    (
      "a unit of 65 536 bytes written",
      annex(&[&lcevc_unit_of(29, &[sequence, &info(65_536 - 3 - 2 - 3 - 4), &global(17)])]),
      Wrote::Corrupt,
    ),
  ];
  for (name, config, wrote) in cases {
    assert_eq!(lvcc(&config), wrote, "{name}");
  }
  // A unit of more bits than `init_get_bits` takes: the split refuses.
  let mut config = annex(&[&idr]);
  config.extend_from_slice(&[0, 0, 0, 1, 0x40 | (1 << 1) | 1, 0xFF]);
  config.resize(config.len() + records::GET_BITS_BYTES_MAX, 0x11);
  config.push(0x80);
  assert_eq!(lvcc(&config), Wrote::Failed, "a unit past the reader");
}

#[test]
fn vp9s_vpcc_is_judged_as_ff_isom_write_vpcc_writes_it() {
  use ffmpeg_next::ffi::AVPixelFormat as P;
  use records::{GET_BITS_BYTES_MAX, Wrote, vpcc};
  const UNKNOWN: i32 = -99;
  let past = GET_BITS_BYTES_MAX + 1;
  for (name, (len, format, profile), wrote) in [
    ("4:2:0", (0, P::AV_PIX_FMT_YUV420P as i32, 0), Wrote::Whole),
    ("4:2:2", (0, P::AV_PIX_FMT_YUV422P as i32, 1), Wrote::Whole),
    ("4:4:4", (0, P::AV_PIX_FMT_YUV444P as i32, 1), Wrote::Whole),
    ("RGB", (0, P::AV_PIX_FMT_GBRP as i32, 1), Wrote::Whole),
    ("4:4:0", (0, P::AV_PIX_FMT_YUV440P as i32, 1), Wrote::Failed),
    (
      "no format",
      (0, P::AV_PIX_FMT_NONE as i32, 0),
      Wrote::Failed,
    ),
    ("a format past the table", (0, i32::MAX, 0), Wrote::Failed),
    (
      "an unknown profile's configuration read whole",
      (GET_BITS_BYTES_MAX, P::AV_PIX_FMT_YUV420P as i32, UNKNOWN),
      Wrote::Whole,
    ),
    (
      "an unknown profile's configuration past the reader",
      (past, P::AV_PIX_FMT_YUV420P as i32, UNKNOWN),
      Wrote::Failed,
    ),
    (
      "a known profile's configuration not read",
      (past, P::AV_PIX_FMT_YUV420P as i32, 0),
      Wrote::Whole,
    ),
    (
      "a format of no depth's configuration past the reader",
      (past, P::AV_PIX_FMT_VIDEOTOOLBOX as i32, 0),
      Wrote::Failed,
    ),
  ] {
    assert_eq!(vpcc(len, format, profile), wrote, "{name}");
  }
}

#[test]
fn avs3s_and_vc1s_configurations_are_judged_as_mp4_writes_them() {
  use records::{Wrote, av3c, dvc1};
  let mut long = vec![0_u8; 65_536];
  long[0] = 0;
  for (name, config, wrote) in [
    ("three bytes", vec![1, 0, 0], Wrote::Failed),
    ("a record", [&[1][..], &[0; 70_000]].concat(), Wrote::Whole),
    (
      "a sequence header of 65 535 bytes",
      long[..65_535].to_vec(),
      Wrote::Whole,
    ),
    (
      "a sequence header of 65 536 bytes",
      long.clone(),
      Wrote::Corrupt,
    ),
  ] {
    assert_eq!(av3c(&config), wrote, "AVS3: {name}");
  }
  let header = |profile: u8| vec![0, 0, 1, 0x0F, profile << 6 | 0x08, 0x0A];
  let entry: &[u8] = &[0, 0, 1, 0x0E, 0x48, 0x80];
  for (name, config, wrote) in [
    (
      "a simple profile's STRUCT_C",
      vec![0x4E, 0x09, 0x30, 0x01],
      Wrote::Failed,
    ),
    ("an advanced sequence header", header(3), Wrote::Whole),
    (
      "an advanced sequence header and an entry point",
      [&header(3)[..], entry].concat(),
      Wrote::Whole,
    ),
    ("an entry point alone", entry.to_vec(), Wrote::Failed),
    (
      "a byte before the sequence header",
      [&[0x2B][..], &header(3)].concat(),
      Wrote::Whole,
    ),
    (
      "a sequence header of no byte",
      vec![0x2B, 0, 0, 1, 0x0F],
      Wrote::Failed,
    ),
    (
      "a sequence header cut by the next marker",
      [&[0, 0, 1, 0x0F][..], entry].concat(),
      Wrote::Failed,
    ),
    (
      "a main profile's sequence header, the trust model's",
      header(1),
      Wrote::Whole,
    ),
  ] {
    assert_eq!(dvc1(&config), wrote, "VC-1: {name}");
  }
}

#[test]
fn flvs_filter_makes_h26x_configurations_as_extract_extradata_makes_them() {
  use records::Units;
  let extracted_bytes = |units, bytes: &[u8]| {
    records::h2645_extracted(units, bytes).map(|made| made.map(records::Extracted::bytes))
  };
  let sps: &[u8] = &[0x67, 0x64, 0x00, 0x1F, 0xAC, 0xD9, 0x40];
  let pps: &[u8] = &[0x68, 0xEB, 0xE3, 0xCB, 0x22, 0xC0];
  let idr: &[u8] = &[0x65, 0x88, 0x84, 0x21];
  let packet = |units: &[&[u8]]| start_coded(units);
  // Each unit to the next start code, a 4-byte one's leading zero its own,
  // behind a 4-byte start code of its own.
  let made = |units: &[&[u8]]| -> Vec<u8> {
    units
      .iter()
      .flat_map(|unit| [&[0_u8, 0, 0, 1][..], unit, &[0]].concat())
      .collect()
  };
  assert_eq!(
    extracted_bytes(Units::H264, &packet(&[sps, pps, idr])),
    Ok(Some(made(&[sps, pps]))),
    "its parameter sets, each behind a 4-byte start code"
  );
  assert_eq!(
    extracted_bytes(Units::H264, &packet(&[sps, idr])),
    Ok(Some(made(&[sps]))),
    "an SPS alone"
  );
  assert_eq!(
    extracted_bytes(Units::H264, &packet(&[pps, idr])),
    Ok(None),
    "no SPS"
  );
  assert_eq!(
    extracted_bytes(Units::H264, &[0, 0, 0, 5, 0x65, 0x88, 0x84, 0x21, 0x10]),
    Err(()),
    "length-prefixed: no start code"
  );
  assert_eq!(
    extracted_bytes(Units::H264, &[0x65, 0x88, 0x84]),
    Ok(None),
    "three bytes"
  );
  assert_eq!(
    extracted_bytes(Units::H264, &packet(&[&[0x80, 0x01]])),
    Ok(None),
    "its one unit's header of the forbidden bit, too little after it to search"
  );
  assert_eq!(
    extracted_bytes(
      Units::H264,
      &[
        0, 0, 1, 0x80, 0x01, 0, 0, 3, 1, 0, 0, 2, 0x11, 0x11, 0x11, 0x11
      ]
    ),
    Err(()),
    "a unit an escape and `00 00 02` end, its header refused, then a search reaching the end"
  );
  assert_eq!(
    extracted_bytes(
      Units::H264,
      &[&[0, 0, 1][..], sps, &[0, 0, 1], pps, &[0, 0, 1], idr].concat()
    ),
    Ok(Some(packet(&[sps, pps]))),
    "3-byte start codes"
  );
  let (vps, hsps, hpps) = (hevc_vps(0, None), hevc_sps(0, 0, 1, None), hevc_pps());
  let slice = escaped(&hevc_header(19, 0), &[0xAF, 0x88]);
  assert_eq!(
    extracted_bytes(Units::Hevc, &packet(&[&vps, &hsps, &hpps, &slice])),
    Ok(Some(made(&[&vps, &hsps, &hpps]))),
  );
  assert_eq!(
    extracted_bytes(Units::Hevc, &packet(&[&hsps, &hpps, &slice])),
    Ok(None),
    "HEVC of no VPS"
  );
  let (vsps, vpps) = (vvc_unit(15), vvc_unit(16));
  assert_eq!(
    extracted_bytes(Units::Vvc, &packet(&[&vsps, &vpps, &vvc_unit(8)])),
    Ok(Some(made(&[&vsps, &vpps]))),
  );
}

/// **A track's channel layout reads back as each container writes it and
/// each decoder reads it** (table E, R10's L1): RIFF's extensible form
/// alone carries a mask, and that only below `0x40000`; Matroska, NUT, MXF
/// and MP4 of no layout box write the count alone; FLAC's comment carries
/// what its own order does not name, and its decoder fills its own order
/// in where the container states none; FLV's multichannel configuration
/// drops the channels it has no bit or id for; a decoder of its own layout
/// keeps the declared one, a custom channel's name aside.
#[test]
fn channel_layouts_read_back_as_each_container_and_decoder_reads_them() {
  use AVChannelOrder as Order;
  use ffmpeg_next::ffi::AVCodecID as Id;
  const LFE2: u64 = 1 << 35;
  const WIDE_LEFT: i32 = 31;
  fn native(mask: u64) -> LayoutView<'static> {
    LayoutView {
      order: Order::AV_CHANNEL_ORDER_NATIVE as i32,
      channels: mask.count_ones() as i32,
      mask,
      map: &[],
    }
  }
  fn unordered(channels: i32) -> LayoutView<'static> {
    LayoutView {
      order: Order::AV_CHANNEL_ORDER_UNSPEC as i32,
      channels,
      mask: 0,
      map: &[],
    }
  }
  fn custom(map: &[CustomChannel]) -> LayoutView<'_> {
    LayoutView {
      order: Order::AV_CHANNEL_ORDER_CUSTOM as i32,
      channels: map.len() as i32,
      mask: 0,
      map,
    }
  }
  let id = |id: Id| CodecId::from_raw(id as i32);
  let unnamed = |channel: i32| CustomChannel::new(channel, [0; 16]);
  let reversed = [unnamed(1), unnamed(0)];
  let wide = [unnamed(1), unnamed(0), unnamed(WIDE_LEFT)];
  let mut left = [0; 16];
  left[..4].copy_from_slice(b"left");
  let named = [CustomChannel::new(0, left), unnamed(1)];
  // First-order ambisonics: four channels, no others.
  let ambisonic = LayoutView {
    order: Order::AV_CHANNEL_ORDER_AMBISONIC as i32,
    channels: 4,
    mask: 0,
    map: &[],
  };
  let at = |codec: CodecId, tag: u32, rate: i32| AudioStream { codec, tag, rate };
  let pcm = at(CodecId::PCM_S16LE, 1, 48_000);
  let flac = at(CodecId::FLAC, 0xF1AC, 48_000);
  let wav = Audited::Wav { w64: false };
  let mp4 = Audited::Mov(MovMode::Mp4);
  let alone = |channels: i32| LayoutReadBack::Unordered { channels };
  let mask = |mask: u64| LayoutReadBack::Native { mask };
  let order = |channels: &[i32]| LayoutReadBack::Custom {
    channels: channels.to_vec(),
  };
  // A codec this build has no decoder for keeps the container's.
  let mpegh = id(Id::AV_CODEC_ID_MPEGH_3D_AUDIO);
  let mpegh_back = if mediaio_ffmpeg::ffi::find_decoder_raw(mpegh.raw()).is_none() {
    alone(6)
  } else {
    mask(0x60F)
  };
  let unknown = AVChannel::AV_CHAN_UNKNOWN as i32;
  for (what, audited, stream, layout, back) in [
    ("WAV PCM mono", wav, pcm, native(0x4), alone(1)),
    ("WAV PCM stereo", wav, pcm, native(0x3), alone(2)),
    ("WAV PCM 2.1", wav, pcm, native(0xB), mask(0xB)),
    (
      "WAV PCM past 0x40000",
      wav,
      pcm,
      native(0x3 | LFE2),
      alone(3),
    ),
    ("WAV PCM custom", wav, pcm, custom(&reversed), alone(2)),
    ("WAV PCM unordered", wav, pcm, unordered(2), alone(2)),
    (
      "WAV PCM at 96 kHz",
      wav,
      at(CodecId::PCM_S16LE, 1, 96_000),
      native(0x3),
      mask(0x3),
    ),
    (
      "WAV PCM of 24 bits",
      wav,
      at(CodecId::PCM_S24LE, 1, 48_000),
      native(0x3),
      mask(0x3),
    ),
    (
      "WAV float",
      wav,
      at(CodecId::PCM_F32LE, 3, 48_000),
      native(0x3),
      alone(2),
    ),
    ("WAV ambisonic", wav, pcm, ambisonic, alone(4)),
    (
      "WAV WMA",
      wav,
      at(id(Id::AV_CODEC_ID_WMAV2), 0x161, 48_000),
      native(0x3),
      alone(2),
    ),
    (
      "WAV ADPCM",
      wav,
      at(id(Id::AV_CODEC_ID_ADPCM_MS), 2, 48_000),
      native(0x3),
      alone(2),
    ),
    (
      "WAV G.726, mono of its own",
      wav,
      at(id(Id::AV_CODEC_ID_ADPCM_G726), 0x45, 8_000),
      native(0x4),
      mask(0x4),
    ),
    (
      "WAV MP3, of its own",
      wav,
      at(CodecId::MP3, 0x55, 48_000),
      native(0x3),
      mask(0x3),
    ),
    ("WAV FLAC stereo", wav, flac, native(0x3), mask(0x3)),
    (
      "WAV FLAC past 0x40000",
      wav,
      flac,
      native(0x3 | LFE2),
      mask(0x7),
    ),
    ("AVI PCM stereo", Audited::Avi, pcm, native(0x3), alone(2)),
    (
      "ASF PCM stereo",
      Audited::Asf { stream: false },
      pcm,
      native(0x3),
      alone(2),
    ),
    (
      "Matroska PCM",
      Audited::Matroska,
      pcm,
      native(0x3),
      alone(2),
    ),
    (
      "Matroska FLAC 2.1",
      Audited::Matroska,
      flac,
      native(0xB),
      mask(0xB),
    ),
    (
      "Matroska FLAC stereo",
      Audited::Matroska,
      flac,
      native(0x3),
      mask(0x3),
    ),
    (
      "Matroska FLAC past 0x3FFFF",
      Audited::Matroska,
      flac,
      native(0x3 | LFE2),
      mask(0x7),
    ),
    ("NUT PCM", Audited::Nut, pcm, native(0x3), alone(2)),
    ("NUT FLAC 2.1", Audited::Nut, flac, native(0xB), mask(0x7)),
    ("MP4 PCM mono", mp4, pcm, native(0x4), alone(1)),
    ("MP4 PCM stereo", mp4, pcm, native(0x3), mask(0x3)),
    (
      "MP4 PCM custom",
      mp4,
      pcm,
      custom(&reversed),
      order(&[1, 0]),
    ),
    ("MP4 PCM named", mp4, pcm, custom(&named), mask(0x3)),
    ("MP4 FLAC 2.1", mp4, flac, native(0xB), mask(0x7)),
    (
      "MP4 MPEG-H, of no decoder here",
      mp4,
      at(mpegh, 0, 48_000),
      native(0x60F),
      mpegh_back,
    ),
    (
      "MXF PCM mono",
      Audited::Mxf(MxfKind::OpAtom),
      pcm,
      native(0x4),
      alone(1),
    ),
    (
      "FLV FLAC, a wide channel",
      Audited::Flv,
      flac,
      native(0x3 | 1 << WIDE_LEFT),
      mask(0x7),
    ),
    (
      "FLV FLAC custom, a wide channel",
      Audited::Flv,
      flac,
      custom(&wide),
      order(&[1, 0, unknown]),
    ),
    (
      "FLV FLAC custom",
      Audited::Flv,
      flac,
      custom(&reversed),
      order(&[1, 0]),
    ),
    (
      "FLV FLAC of LFE2",
      Audited::Flv,
      flac,
      native(0x3 | LFE2),
      mask(0x3 | LFE2),
    ),
    (
      "FLV FLAC unordered",
      Audited::Flv,
      flac,
      unordered(3),
      mask(0xB),
    ),
    ("FLV PCM stereo", Audited::Flv, pcm, native(0x3), mask(0x3)),
    (
      "Ogg FLAC 2.1",
      Audited::Ogg(OggKind::Ogg),
      flac,
      native(0xB),
      mask(0x7),
    ),
    ("FLAC 2.1", Audited::Flac, flac, native(0xB), mask(0xB)),
    (
      "FLAC past 0x3FFFF",
      Audited::Flac,
      flac,
      native(0x3 | LFE2),
      mask(0x7),
    ),
    (
      "Matroska AAC named, of its own",
      Audited::Matroska,
      at(CodecId::AAC, 0, 48_000),
      custom(&named),
      mask(0x3),
    ),
    (
      "MPEG-PS DVD PCM, of its own",
      Audited::MpegPs(PsKind::Dvd),
      at(id(Id::AV_CODEC_ID_PCM_DVD), 0, 48_000),
      native(0x3),
      mask(0x3),
    ),
  ] {
    let read = layout_read_back(audited, stream, layout);
    assert_eq!(read, back, "{what}");
    let named = layout
      .map
      .iter()
      .any(|channel| channel.name_bytes()[0] != 0);
    assert_eq!(
      layout_kept(layout, &read),
      !named && LayoutReadBack::canonical(layout) == read,
      "{what}: kept"
    );
  }
}

/// An APV PBU of type `kind`: its header, a frame header of a 16 by 16
/// picture of 4:2:2, `depth_minus8`, the capture time distance `distance`,
/// its reserved bits `reserved`, and one tile (`cbs_apv_syntax_template.c`
/// 19–330).
fn apv_pbu(kind: u8, depth_minus8: u8, distance: u8, reserved: u8) -> Vec<u8> {
  let mut header = vec![33, 30, reserved & 0x1F];
  header.extend_from_slice(&[0, 0, 16, 0, 0, 16]);
  header.extend_from_slice(&[0x20 | depth_minus8, distance, 0, 0]);
  // Colour description and quantization matrix absent, then the tile info,
  // 16 by 8 macroblocks, and the last reserved byte, aligned.
  header.extend_from_slice(&[0x00, 0x00, 0x40, 0x00, 0x02, 0x00, 0x00]);
  let mut tile = Vec::new();
  tile.extend_from_slice(&20_u16.to_be_bytes());
  tile.extend_from_slice(&0_u16.to_be_bytes());
  for _ in 0..3 {
    tile.extend_from_slice(&1_u32.to_be_bytes());
  }
  tile.extend_from_slice(&[22, 22, 22, 0, 0, 0, 0]);
  let mut pbu = vec![kind, 0, 1, 0];
  pbu.extend_from_slice(&header);
  pbu.extend_from_slice(&(tile.len() as u32).to_be_bytes());
  pbu.extend_from_slice(&tile);
  pbu
}

/// An APV access unit of `pbus`, each behind its size.
fn apv_access_unit(pbus: &[Vec<u8>]) -> Vec<u8> {
  let mut au = b"aPv1".to_vec();
  for pbu in pbus {
    au.extend_from_slice(&(pbu.len() as u32).to_be_bytes());
    au.extend_from_slice(pbu);
  }
  au
}

/// R11, ruling 2: MP4's parse of an APV access unit for `apvC`
/// (`ff_isom_parse_apvc`, `apv.c` 253–318) — each frame's information with
/// its PBU type, in order; none of a packet of no signature, of a PBU
/// that does not split or of a reserved field set, which its parse
/// refuses; none past a frame of an odd bit depth, where it stops; a
/// metadata PBU's none.
#[test]
fn apv_frame_infos_follow_mp4s_parse_of_the_access_unit() {
  let infos = |au: &[u8]| -> Vec<(u8, u8)> {
    records::apv_frame_infos(au)
      .into_iter()
      .map(|(kind, info)| (kind, format!("{info:?}").len() as u8))
      .collect()
  };
  let kinds = |au: &[u8]| -> Vec<u8> {
    records::apv_frame_infos(au)
      .into_iter()
      .map(|(kind, _)| kind)
      .collect()
  };
  let primary = apv_pbu(1, 2, 3, 0);
  let alpha = apv_pbu(27, 2, 3, 0);
  let metadata = vec![66, 0, 1, 0, 0, 0, 0, 0];
  assert_eq!(kinds(&apv_access_unit(&[primary.clone()])), [1]);
  assert_eq!(
    kinds(&apv_access_unit(&[
      primary.clone(),
      metadata.clone(),
      alpha.clone()
    ])),
    [1, 27]
  );
  // No signature, a PBU short of 8 bytes, a PBU past what is left.
  let mut unsigned = apv_access_unit(&[primary.clone()]);
  unsigned[0] = b'b';
  assert!(infos(&unsigned).is_empty());
  assert!(kinds(&apv_access_unit(&[primary.clone(), vec![1, 0, 1]])).is_empty());
  let mut long = apv_access_unit(&[primary.clone()]);
  long[7] = long[7].wrapping_add(1);
  assert!(kinds(&long).is_empty());
  // A reserved field set, or 8 bits a sample, fails the parse of the unit.
  assert!(kinds(&apv_access_unit(&[primary.clone(), apv_pbu(2, 2, 3, 1)])).is_empty());
  assert!(kinds(&apv_access_unit(&[apv_pbu(1, 0, 3, 0)])).is_empty());
  // An odd bit depth stops the parse: the frames after it unrecorded.
  assert_eq!(
    kinds(&apv_access_unit(&[
      primary.clone(),
      apv_pbu(2, 3, 3, 0),
      alpha.clone()
    ])),
    [1]
  );
  // Two distances, two informations; the same twice, equal.
  let a: Vec<_> = records::apv_frame_infos(&apv_access_unit(&[apv_pbu(1, 2, 3, 0)])).collect();
  let b: Vec<_> = records::apv_frame_infos(&apv_access_unit(&[apv_pbu(1, 2, 4, 0)])).collect();
  let c: Vec<_> = records::apv_frame_infos(&apv_access_unit(&[apv_pbu(1, 2, 3, 0)])).collect();
  assert_ne!(a, b);
  assert_eq!(a, c);
}

/// The MOV family's table of a track's samples holds what its allocation
/// does (R12; [`super::PayloadRule::SampleTable`]): movenc grows it 1024
/// entries of `MOVIentry` at a time, refused past `INT_MAX` bytes, so that
/// its last growth lies within them and the next past — 29 826 048 entries
/// of 72 bytes, or 33 553 408 of 64, as the target's C layout makes it.
#[test]
fn movs_sample_table_holds_what_its_allocation_does() {
  let entry = core::mem::size_of::<super::MovEntry>() as u64;
  assert!(
    entry == 72 || entry == 64,
    "MOVIentry's layout, {entry} bytes"
  );
  let max = super::MOV_SAMPLES_MAX;
  assert_eq!(max % 1024, 0, "a whole growth");
  assert!(
    max * entry <= i32::MAX as u64,
    "the last growth within INT_MAX"
  );
  assert!((max + 1024) * entry > i32::MAX as u64, "the next past it");
  assert_eq!(max, if entry == 72 { 29_826_048 } else { 33_553_408 });
}

/// The movie `udta` of two covers at the default packet budget passes the
/// dynamic buffer movenc makes it in, and one cover filling its bound to
/// the byte — `INT_MAX` less its header, R15 — comes to it exactly (R14b,
/// ruled; [`super::MuxOpenError::CoversTooLarge`]):
/// `meta`, `hdlr`, `ilst` and `©too` of libavformat's name, 77 bytes and
/// the name; `covr`, 8 and 16 a cover beside its picture, an empty one
/// skipped; the Nero list, 17 and 9 a chapter beside its title.
#[test]
fn the_movie_udta_of_two_gibibyte_covers_passes_its_buffer() {
  let version = ffmpeg_next::format::version();
  let name = format!(
    "Lavf{}.{}.{}",
    version >> 16,
    (version >> 8) & 0xFF,
    version & 0xFF
  );
  let own = super::udta_bytes(0, &[]);
  assert_eq!(own, 77 + name.len() as u64, "the iTunes metadata of {name}");
  let budget = mediaio_ffmpeg::DEFAULT_MAX_PACKET_BYTES as u64;
  let two = super::udta_bytes(super::covr_bytes([budget, budget].into_iter()), &[]);
  assert_eq!(two, own + 8 + 2 * (16 + budget));
  assert!(two > i32::MAX as u64, "two gibibyte covers pass INT_MAX");
  let filling = super::UDTA_BYTES_MAX - own - 24;
  let one = super::udta_bytes(super::covr_bytes([filling].into_iter()), &[]);
  assert_eq!(
    one,
    super::UDTA_BYTES_MAX,
    "one cover fills the bound to the byte"
  );
  assert_eq!(super::covr_bytes([0, 5].into_iter()), 24 + 5);
  assert_eq!(super::covr_bytes([5, 0, 7].into_iter()), 24 + 5 + 16 + 7);
  assert_eq!(
    super::covr_bytes([0, 0].into_iter()),
    0,
    "no picture, no covr"
  );
}

/// The movie `udta`'s last byte (R15, Codex R14 1;
/// [`super::UDTA_BYTES_MAX`]): movenc writes its size as the buffer's and 8
/// in an `int` (`movenc.c` 5113–5115), so a `udta` of `INT_MAX - 8` bytes
/// beside its header is taken and one of a byte more refused, at the cover
/// that brings it, by the bound the muxer holds — and two covers of the
/// default packet budget refused at the second.
#[test]
fn the_movie_udta_holds_int_max_less_its_header() {
  let bound = super::Bounds::REAL.user_data;
  assert_eq!(bound, i32::MAX as u64 - 8, "INT_MAX less the udta's header");
  let own = super::udta_bytes(0, &[]);
  let filling = bound - own - 24;
  assert_eq!(
    super::covers_too_large([(3, filling)].into_iter(), &[], bound),
    None,
    "the udta of INT_MAX - 8 bytes taken"
  );
  assert_eq!(
    super::covers_too_large([(3, filling + 1)].into_iter(), &[], bound),
    Some(3),
    "one byte more refused at its cover"
  );
  assert_eq!(
    super::covers_too_large([(1, filling - 16), (2, 0), (4, 16)].into_iter(), &[], bound),
    Some(4),
    "refused at the cover that passes, an empty one counting nothing"
  );
  let budget = mediaio_ffmpeg::DEFAULT_MAX_PACKET_BYTES as u64;
  assert_eq!(
    super::covers_too_large([(1, budget), (2, budget)].into_iter(), &[], bound),
    Some(2),
    "two gibibyte covers refused at the second"
  );
}

/// MP3's ID3v2 tag holds `2^28 - 1` bytes beside its header (R15b, ruled:
/// a length that wraps is refused; [`super::ID3V2_BYTES_MAX`]):
/// `ff_id3v2_finish` writes the size of its frames and their padding in 28
/// syncsafe bits (`id3v2enc.c` 421–443), so a tag of `2^28 - 1` bytes of
/// frames is taken and one a byte more refused, at the chapter that brings
/// it, by the bound the muxer holds — each frame counted as `id3v2enc`
/// writes it: the `encoder`'s `TSSE`, the table of contents of the
/// chapters' element ids, each chapter's frame and its title's `TIT2`.
#[test]
fn the_mp3_id3v2_tag_holds_two_to_the_28_less_one() {
  let bound = super::Bounds::REAL.tag;
  assert_eq!(bound, (1 << 28) - 1, "the tag's 28 syncsafe bits");
  let ident = super::libavformat_ident_bytes();
  // The `TSSE`; the table of contents of `ch0` and `ch1`; the untitled
  // chapter; the titled one, and its `TIT2` but the title.
  let around =
    (10 + 1 + ident + 1) + (10 + 4 + 1 + 1 + 4 + 4) + (10 + 4 + 16) + (10 + 4 + 16 + 10 + 1 + 1);
  let filling = bound - around;
  assert_eq!(
    super::id3v2_too_large([None, Some(filling)].into_iter(), bound),
    None,
    "a tag of 2^28 - 1 bytes taken"
  );
  assert_eq!(
    super::id3v2_too_large([None, Some(filling + 1)].into_iter(), bound),
    Some(1),
    "one byte more refused at its chapter"
  );
  assert_eq!(
    super::id3v2_too_large([Some(bound), None].into_iter(), bound),
    Some(0),
    "refused at the first chapter that passes"
  );
  assert_eq!(
    super::id3v2_too_large(core::iter::empty(), bound),
    None,
    "a tag of no chapter, the encoder's alone"
  );
}

/// An Ogg stream's comment header packet holds `INT_MAX` bytes, its codec's
/// own beside the comments, and FLAC's comments `2^24 - 1` (R19, Codex R18
/// 2; [`super::ogg_comment_refused`]): libavformat measures the comments in
/// a null buffer of `INT_MAX` bytes and refuses the packet past `INT_MAX`,
/// each refusal returned as `ENOMEM` (`ogg_write_vorbiscomment`, `oggenc.c`
/// 291–319), and writes FLAC's comments' length in 24 bits (352) — so
/// each codec's packet is taken at the bound and refused a byte past it,
/// FLAC's at its 24 bits; and each chapter's start is the width `snprintf`
/// writes, hours of three digits and C's `int` of seconds among them.
#[test]
fn an_ogg_comment_header_holds_int_max() {
  let bound = super::Bounds::REAL.comment;
  assert_eq!(bound, i32::MAX as u64, "the int libavformat builds it in");
  for (codec, own) in [
    (CodecId::OPUS, 8),
    (CodecId::VORBIS, 8),
    (super::THEORA, 7),
    (CodecId::VP8, 7),
    (super::SPEEX, 0),
  ] {
    assert!(
      !super::ogg_comment_refused(codec, bound - own, bound),
      "{codec:?}: a packet of INT_MAX bytes taken"
    );
    assert!(
      super::ogg_comment_refused(codec, bound - own + 1, bound),
      "{codec:?}: a byte more refused"
    );
  }
  assert!(!super::ogg_comment_refused(CodecId::FLAC, 0xFF_FFFF, bound));
  assert!(
    super::ogg_comment_refused(CodecId::FLAC, 0x100_0000, bound),
    "FLAC's comments past their 24 bits"
  );
  let ms = Timebase::new(1, NonZeroI32::new(1000).expect("a denominator"));
  let seconds = Timebase::new(1, NonZeroI32::new(1).expect("a denominator"));
  for (index, base, start, written) in [
    (0, ms, 0, "CHAPTER000=00:00:00.000"),
    // The seconds rounded to the nearest apart from the milliseconds:
    // 3725.5 s written 01:02:06.500.
    (7, ms, 3_725_500, "CHAPTER007=01:02:06.500"),
    (999, ms, 360_000_000, "CHAPTER999=100:00:00.000"),
    // Rounded to the nearest, each apart: 0.9995 s is a second and 1000
    // milliseconds' remainder of none.
    (
      1,
      Timebase::new(1, NonZeroI32::new(10_000).expect("a denominator")),
      9_995,
      "CHAPTER001=00:00:01.000",
    ),
    // Seconds past `INT_MAX`, held in C's `int`: its low 32 bits.
    (2, seconds, 1 << 31, "CHAPTER002=-596523:-14:-8.000"),
  ] {
    assert_eq!(super::ogg_chapter_time(index, base, start), written);
  }
}

/// AVIF's `moov` is bounded at `INT_MAX`, the null buffer its trailer
/// measures it in (R19; [`super::avif_movie_bound`]): the real bound, the
/// `UINT32_MAX` of the box's size, lowered to it; a lower one kept.
#[test]
fn avifs_movie_box_holds_int_max() {
  assert_eq!(super::Bounds::REAL.movie, u64::from(u32::MAX));
  assert_eq!(
    super::avif_movie_bound(super::Bounds::REAL.movie),
    i32::MAX as u64
  );
  assert_eq!(
    super::avif_movie_bound(i32::MAX as u64 + 1),
    i32::MAX as u64
  );
  assert_eq!(super::avif_movie_bound(4096), 4096);
}

/// A NUT info packet's tags are counted exactly and held to `INT_MAX` less
/// the 64 bytes of padding its dynamic buffer's close writes (R20, by the
/// class; [`super::NUT_INFO_BYTES_MAX`]): `put_v`'s bytes, seven bits a
/// byte (`nutenc.c` 306–327); a chapter's `title` tag, the longest title
/// taken and a byte more refused; a stream's language, dispositions and
/// frame rate, each tag as `add_info` writes it (509–515, 545–585).
#[test]
fn a_nut_info_packet_holds_int_max_less_its_padding() {
  let bound = super::Bounds::REAL.info;
  assert_eq!(
    bound,
    i32::MAX as u64 - 64,
    "INT_MAX less the close's padding"
  );
  for (value, bytes) in [
    (0, 1),
    (127, 1),
    (128, 2),
    (16_383, 2),
    (16_384, 3),
    ((1 << 28) - 1, 4),
    (1 << 28, 5),
    (u64::from(u32::MAX), 5),
    (1 << 35, 6),
  ] {
    assert_eq!(super::nut_v_bytes(value), bytes, "put_v of {value}");
  }
  // `title` behind its length, the type, the title behind its five bytes
  // of length.
  let longest = bound - (1 + 5 + 1 + 5);
  assert_eq!(super::nut_chapter_info_bytes(Some(longest)), bound, "taken");
  assert_eq!(
    super::nut_chapter_info_bytes(Some(longest + 1)),
    bound + 1,
    "a byte more refused"
  );
  assert_eq!(super::nut_chapter_info_bytes(None), 0, "no tag");
  assert_eq!(
    super::nut_chapter_info_bytes(Some(0)),
    8,
    "an empty title's tag"
  );
  let none = TrackDisposition::empty();
  let all = TrackDisposition::DEFAULT
    | TrackDisposition::DUB
    | TrackDisposition::ORIGINAL
    | TrackDisposition::COMMENT
    | TrackDisposition::LYRICS
    | TrackDisposition::KARAOKE;
  assert_eq!(super::nut_stream_info_bytes(None, none, None), 0);
  assert_eq!(
    super::nut_stream_info_bytes(Some(3), none, None),
    1 + 10 + 1 + 1 + 3,
    "X-Language and three bytes"
  );
  assert_eq!(
    super::nut_stream_info_bytes(None, all, None),
    6 * (1 + 11 + 1 + 1) + (7 + 3 + 8 + 7 + 6 + 7),
    "six Disposition tags"
  );
  assert_eq!(
    super::nut_stream_info_bytes(None, TrackDisposition::FORCED, None),
    0,
    "a flag NUT names not"
  );
  assert_eq!(
    super::nut_stream_info_bytes(None, none, Some(None)),
    1 + 12 + 1 + 1 + 3,
    "a video stream of no frame rate: 0/0"
  );
  let ntsc = Timebase::new(30_000, NonZeroI32::new(1001).expect("a denominator"));
  assert_eq!(
    super::nut_stream_info_bytes(None, none, Some(Some(ntsc))),
    1 + 12 + 1 + 1 + 10,
    "30000/1001"
  );
  assert_eq!(super::decimal_digits(i32::MIN), 11, "the sign among them");
  assert_eq!(super::decimal_digits(0), 1);
  // The longest language taken, its tag and the rest at their least.
  let longest = bound - (1 + 10 + 1 + 5);
  assert_eq!(
    super::nut_stream_info_bytes(Some(longest), none, None),
    bound
  );
}

/// AVI's counts hold their `int` and their 32 bits, and FLV's sequence
/// header its 24 (R20, by the class): the real bounds; FLV's header bytes
/// of each road of `flv_write_codec_header` (`flvenc.c` 807–914) — an
/// extended header and two more past a kind's first, AAC's legacy two,
/// H.264's legacy five — beside the record as its writer makes it.
#[test]
fn avis_counts_and_flvs_sequence_header_hold_their_bits() {
  let real = super::Bounds::REAL;
  assert_eq!(real.frames, i32::MAX as u64, "AVI's frames, an int");
  assert_eq!(real.blocks, u64::from(u32::MAX), "AVI's length, 32 bits");
  assert_eq!(real.sequence, (1 << 24) - 1, "FLV's tag size, 24 bits");
  let aac = [0x12, 0x10];
  assert_eq!(
    super::flv_sequence_bytes(TrackKind::Audio, CodecId::AAC, &aac, 0),
    Some(2 + 2),
    "AAC's legacy header"
  );
  assert_eq!(
    super::flv_sequence_bytes(TrackKind::Audio, CodecId::AAC, &aac, 1),
    Some(7 + 2),
    "AAC past the first: extended, multitrack"
  );
  assert_eq!(
    super::flv_sequence_bytes(TrackKind::Audio, CodecId::MP3, &[], 0),
    None,
    "the first MP3: no sequence header"
  );
  assert_eq!(
    super::flv_sequence_bytes(TrackKind::Audio, CodecId::MP3, &[], 1),
    Some(7),
    "MP3 past the first: its header alone"
  );
  assert_eq!(
    super::flv_sequence_bytes(TrackKind::Audio, CodecId::EAC3, &[], 0),
    Some(5),
    "E-AC-3's extended header, no record"
  );
  assert_eq!(
    super::flv_sequence_bytes(TrackKind::Video, CodecId::VP9, &[], 0),
    Some(5 + 12),
    "VP9's vpcC"
  );
  // An `avcC` written as it is, behind H.264's legacy five bytes.
  let avcc = [
    1, 0x64, 0, 0x1F, 0xFF, 0xE1, 0, 2, 0x67, 0x64, 1, 0, 2, 0x68, 0xEE,
  ];
  assert_eq!(
    super::flv_sequence_bytes(TrackKind::Video, CodecId::H264, &avcc, 0),
    Some(5 + avcc.len() as u64)
  );
  // Of Annex B units: six bytes, the SPS behind its length, the count and
  // the PPS so — profile 66, no extension's four bytes.
  let annex_b = [
    0, 0, 0, 1, 0x67, 66, 0, 30, 0xAB, 0, 0, 0, 1, 0x68, 0xCE, 0x38,
  ];
  assert_eq!(
    super::records::avcc_written_len(&annex_b),
    6 + (2 + 5) + 1 + (2 + 3)
  );
  // Profile 100: the four bytes and the extensions' count.
  let high = [
    0, 0, 0, 1, 0x67, 100, 0, 30, 0xAB, 0, 0, 0, 1, 0x68, 0xCE, 0x38,
  ];
  assert_eq!(
    super::records::avcc_written_len(&high),
    6 + (2 + 5) + 1 + (2 + 3) + 4
  );
}

/// The configuration writers' Annex B rewrite is judged where they make it
/// (R20, r2): the MOV family's, FLV's and Matroska's H.264, HEVC and VVC
/// writers, each of the configuration it reaches the rewrite of — H.264's
/// of more than 6 bytes, HEVC's and VVC's of 6 or more — its bytes each
/// unit behind a 4-byte length, against the bound; no other codec's or
/// container's, nor a record of its own form.
#[test]
fn configuration_rewrites_are_held_where_their_writers_make_them() {
  assert_eq!(super::Bounds::REAL.rewrite, i32::MAX as u64 - 64);
  let mp4 = (Family::Mp4, Audited::Mov(MovMode::Mp4));
  let flv = (Family::Other, Audited::Flv);
  let mkv = (Family::Matroska, Audited::Matroska);
  // A 4-byte start code, then a 3-byte one: 13 bytes rewritten of 12.
  let units = [0, 0, 0, 1, 0x67, 1, 2, 0, 0, 1, 0x68, 3];
  for family in [mp4, flv, mkv] {
    for codec in [CodecId::H264, CodecId::HEVC, CodecId::VVC] {
      assert!(
        super::rewrite_cut(family, codec, &units, 12),
        "{family:?} {codec:?}: a byte below"
      );
      assert!(
        !super::rewrite_cut(family, codec, &units, 13),
        "{family:?} {codec:?}: at it"
      );
    }
    assert!(
      !super::rewrite_cut(family, CodecId::AV1, &units, 0),
      "AV1's none"
    );
  }
  assert!(
    !super::rewrite_cut((Family::Other, Audited::MpegTs), CodecId::H264, &units, 0),
    "MPEG-TS writes no record"
  );
  let avcc = [1, 0x64, 0, 0x1F, 0xFF, 0xE1, 0, 2, 0x67, 0x64];
  assert!(
    !super::rewrite_cut(mp4, CodecId::H264, &avcc, 0),
    "an avcC as it is"
  );
  let six = [0, 0, 1, 0x67, 1, 2];
  assert!(
    !super::rewrite_cut(mp4, CodecId::H264, &six, 0),
    "H.264's of 6 bytes refused first"
  );
  assert!(
    super::rewrite_cut(mp4, CodecId::HEVC, &six, 0),
    "HEVC's of 6 rewritten"
  );
  assert!(
    super::rewrite_cut(mp4, CodecId::VVC, &six, 0),
    "VVC's of 6 rewritten"
  );
  assert!(
    !super::rewrite_cut(mp4, CodecId::HEVC, &six[..5], 0),
    "of 5 refused first"
  );
}

/// The MOV family's `esds` is counted where it writes one (R20, r2): an
/// entry of `mp4v` or `mp4a`, MOV's AAC through its `wave`, MOV's MPEG-4
/// Part 2 of its own tag where that is `mp4v` or none — not another tag's,
/// written with a `glbl`; DVD subtitles' holds the palette, not their
/// configuration.
#[test]
fn an_esds_is_counted_where_the_mov_family_writes_one() {
  use super::{MovMode as M, esds_carries};
  assert_eq!(super::Bounds::REAL.descriptor, (1 << 28) - 1);
  let mp4v = u32::from_le_bytes(*b"mp4v");
  let xvid = u32::from_le_bytes(*b"XVID");
  let mov = |mode| Audited::Mov(mode);
  assert!(esds_carries(mov(M::Mp4), CodecId::AAC, 0));
  assert!(
    esds_carries(mov(M::Mp4), CodecId::MP3, 0),
    "MP4's MP3, mp4a"
  );
  assert!(
    esds_carries(mov(M::Mp4), CodecId::MJPEG, 0),
    "MP4's MJPEG, mp4v"
  );
  assert!(!esds_carries(mov(M::Mp4), CodecId::H264, 0), "an avcC");
  assert!(
    !esds_carries(mov(M::Mp4), CodecId::DVD_SUBTITLE, 0),
    "the palette's"
  );
  assert!(esds_carries(mov(M::Ipod), CodecId::MPEG4, 0));
  assert!(esds_carries(mov(M::Ism), CodecId::AAC, 0));
  assert!(!esds_carries(mov(M::F4v), CodecId::MP3, 0), "F4V's .mp3");
  assert!(esds_carries(mov(M::Mov), CodecId::AAC, xvid), "MOV's wave");
  assert!(
    esds_carries(mov(M::Mov), CodecId::MPEG4, 0),
    "mp4v, its first"
  );
  assert!(esds_carries(mov(M::Mov), CodecId::MPEG4, mp4v));
  assert!(!esds_carries(mov(M::Mov), CodecId::MPEG4, xvid), "a glbl");
  assert!(!esds_carries(mov(M::Avif), CodecId::AV1, 0));
  assert!(!esds_carries(Audited::Matroska, CodecId::AAC, 0));
}

/// A VC-1 unit's bits are counted where their `int` wraps, and a sequence
/// header's where its reader stops (R20, r2): each unit's bytes unescaped,
/// a `3` after two zeros and before a byte below 4 dropped; a unit of the
/// wrap's bytes or more past it, of any type; a sequence header past the
/// reader's; and a sequence header of another profile than the advanced
/// one ends the walk.
#[test]
fn a_vc1_units_bits_are_counted_where_their_int_wraps() {
  use records::{GET_BITS_BYTES_MAX, VC1_UNIT_BYTES_WRAP, dvc1_reads_past};
  assert_eq!(VC1_UNIT_BYTES_WRAP, 1 << 28);
  // `init_get_bits` takes eight bits a byte of the most, refuses a byte's
  // more (`get_bits.h` 522).
  let limit = i64::from(i32::MAX) - 512;
  assert!((GET_BITS_BYTES_MAX as i64) * 8 < limit);
  assert!((GET_BITS_BYTES_MAX as i64 + 1) * 8 >= limit);
  assert_eq!(
    super::Bounds::REAL.reader,
    GET_BITS_BYTES_MAX as u64,
    "the reader's"
  );
  let wrap = 1 << 28;
  let header = [0, 0, 1, 0x0F, 0xC8, 0x0A];
  assert!(!dvc1_reads_past(&header, 2, wrap));
  assert!(dvc1_reads_past(&header, 1, wrap));
  let escaped = [0, 0, 1, 0x0F, 0xC8, 0x0A, 0, 0, 3, 1, 0x0A];
  assert!(!dvc1_reads_past(&escaped, 6, wrap), "its escape dropped");
  assert!(dvc1_reads_past(&escaped, 5, wrap));
  // An entry point of four bytes, its bits' count past a wrap of four.
  let entry = [0, 0, 1, 0x0E, 0x48, 0x80, 0x11, 0x22];
  assert!(dvc1_reads_past(&entry, 0, 4), "any unit's count");
  assert!(!dvc1_reads_past(&entry, 0, 5), "no sequence header read");
  // A simple profile's sequence header ends the walk; an advanced one's
  // goes on to the entry point.
  let walk = |profile: u8| [&[0, 0, 1, 0x0F, profile, 0x0A][..], &entry].concat();
  assert!(!dvc1_reads_past(&walk(0x08), 2, 4), "the walk ended");
  assert!(dvc1_reads_past(&walk(0xC8), 2, 4), "the walk went on");
  assert!(
    dvc1_reads_past(&walk(0x08), 1, 4),
    "read before its profile"
  );
}

/// LCEVC's split is held to what libavcodec allocates of it (R20, r2): its
/// RBSP buffer, the configuration's bytes and `MAX_MBPAIR_SIZE` and the
/// padding, within `INT_MAX`; its unit list, a slot asked for each unit
/// after a kept one, refused at `INT_MAX` over an `H2645NAL`'s bytes; and
/// each slot's emulation prevention positions, 1024 at first, doubled as a
/// unit's outrun them, their bytes within `INT_MAX` — `2^28` at most.
#[test]
fn lcevcs_split_is_held_to_what_libavcodec_allocates() {
  use records::{SplitLimits, Wrote, lvcc_within};
  let real = SplitLimits::REAL;
  assert_eq!(real.bytes, i32::MAX as usize - 64 - 256 * 1024);
  #[cfg(target_pointer_width = "64")]
  assert_eq!(real.slots, i32::MAX as u64 / 88, "an H2645NAL of 88 bytes");
  assert_eq!(real.positions, i32::MAX as u64);
  let mut positions = 1024_u64;
  while positions * 2 * 4 <= real.positions {
    positions *= 2;
  }
  assert_eq!(positions, 1 << 28, "the most a unit's positions hold");
  let sequence: &[u8] = &[0x20, 0x11];
  let global: &[u8] = &[0x41, 17 << 1, 0x40];
  let idr = lcevc_unit_of(29, &[sequence, global]);
  let other = lcevc_unit_of(1, &[sequence]);
  let config = start_coded(&[&idr, &other, &other]);
  let within = |limits: SplitLimits| lvcc_within(&config, limits);
  assert_eq!(within(real), Wrote::Whole, "the premise");
  assert_eq!(
    within(SplitLimits {
      bytes: config.len(),
      ..real
    }),
    Wrote::Whole
  );
  assert_eq!(
    within(SplitLimits {
      bytes: config.len() - 1,
      ..real
    }),
    Wrote::Failed
  );
  // Three units kept, the third's slot the third asked for.
  assert_eq!(within(SplitLimits { slots: 4, ..real }), Wrote::Whole);
  assert_eq!(within(SplitLimits { slots: 3, ..real }), Wrote::Failed);
  // A unit of 1025 emulation prevention bytes: its slot's 1024 positions
  // doubled to 2048, 8192 bytes.
  let escapes: Vec<u8> = (0..1025).flat_map(|_| [0, 0, 3, 0x11]).collect();
  let escaped = [&[0x40 | (1 << 1) | 1, 0xFF][..], &escapes, &[0x80]].concat();
  let config = start_coded(&[&idr, &escaped]);
  let within = |limits: SplitLimits| lvcc_within(&config, limits);
  assert_eq!(within(real), Wrote::Whole);
  assert_eq!(
    within(SplitLimits {
      positions: 2048 * 4,
      ..real
    }),
    Wrote::Whole
  );
  assert_eq!(
    within(SplitLimits {
      positions: 2048 * 4 - 1,
      ..real
    }),
    Wrote::Failed
  );
}

/// MPEG-TS reads a unit's type as its start code search leaves it, and
/// adds a delimiter and the configuration as it reads them (R20, r2):
/// `avpriv_find_start_code` past the byte after each start code, its state
/// the four bytes before; H.264's delimiter added, or moved before the
/// configuration where the picture has its own; an SPS before dropping the
/// configuration; HEVC's configuration only before a random access
/// picture; VVC's first type read as 0, a delimiter of its own among them.
#[test]
fn mpeg_ts_reads_a_units_type_as_its_start_code_search_leaves_it() {
  use super::{TsVideo, find_start_code, ts_extradd, ts_prefixed_bytes};
  let bytes = [0, 0, 0, 1, 0x65, 0x88, 0, 0, 1, 0x41, 0x9A];
  let mut state = u32::MAX;
  let at = find_start_code(&bytes, 0, &mut state);
  assert_eq!((at, state), (5, 0x0000_0165));
  let at = find_start_code(&bytes, at, &mut state);
  assert_eq!((at, state), (10, 0x0000_0141));
  assert_eq!(find_start_code(&bytes, at, &mut state), 11, "the end");
  let config = 20;
  let idr = [0, 0, 0, 1, 0x65, 0x88, 0x84];
  assert_eq!(
    ts_prefixed_bytes(TsVideo::H264, &idr, true, config),
    7 + 6 + 20
  );
  assert_eq!(ts_prefixed_bytes(TsVideo::H264, &idr, false, config), 7 + 6);
  let delimited = [0, 0, 0, 1, 9, 0xF0, 0, 0, 0, 1, 0x65, 0x88, 0x84];
  assert_eq!(
    ts_prefixed_bytes(TsVideo::H264, &delimited, true, config),
    13 + 1 + 20,
    "moved"
  );
  assert_eq!(
    ts_prefixed_bytes(TsVideo::H264, &delimited, false, config),
    13
  );
  let with_sps = [0, 0, 0, 1, 0x67, 0x42, 0, 0, 0, 1, 0x65, 0x88, 0x84];
  assert_eq!(
    ts_prefixed_bytes(TsVideo::H264, &with_sps, true, config),
    13 + 6
  );
  let hevc = |kind: u8| [0, 0, 0, 1, kind << 1, 1, 0xAF];
  assert_eq!(
    ts_prefixed_bytes(TsVideo::Hevc, &hevc(19), true, config),
    7 + 7 + 20
  );
  assert_eq!(
    ts_prefixed_bytes(TsVideo::Hevc, &hevc(1), true, config),
    7 + 7
  );
  assert_eq!(ts_prefixed_bytes(TsVideo::Hevc, &hevc(35), true, config), 7);
  let vvc_delimiter = [0, 0, 0, 1, 0, (20 << 3) | 1, 0x80];
  assert_eq!(
    ts_prefixed_bytes(TsVideo::Vvc, &vvc_delimiter, true, config),
    7 + 7 + 20,
    "VVC's first type read as 0"
  );
  assert_eq!(ts_extradd(&[0, 0, 0, 1, 0x67]), 5);
  assert_eq!(ts_extradd(&[0, 0, 1, 0x67]), 4);
  assert_eq!(ts_extradd(&[0]), 1, "read on into the padding's zeros");
  assert_eq!(ts_extradd(&[0, 0, 2]), 0);
  assert_eq!(ts_extradd(&[1, 0x64, 0]), 0, "an avcC");
  assert_eq!(ts_extradd(&[]), 0);
}

/// Opus's control header is counted with its trims (R20, r2): two bytes, a
/// byte for each 255 of the packet and one more; two while a start trim is
/// left — taken as left where its count wrapped — the packet's samples
/// trimmed off it; two for an end trim its side data makes, of 32
/// unsigned bits; none for a packet of its own control header.
#[test]
fn opus_control_headers_are_counted_with_their_trims() {
  use super::{opus_control_bytes, opus_ts_end_trim, opus_ts_samples, opus_ts_trim};
  // Hybrid fullband, 20 ms, code 0: 960 samples.
  let mut packet = vec![15 << 3];
  packet.resize(300, 0x11);
  assert_eq!(opus_ts_samples(&packet), 960);
  assert_eq!(opus_ts_samples(&[(15 << 3) | 3, 3]), 2880);
  assert_eq!(opus_ts_samples(&[(15 << 3) | 3, 7]), 0, "past 120 ms");
  assert_eq!(opus_ts_samples(&[(15 << 3) | 3]), 0, "no count byte");
  assert_eq!(opus_control_bytes(&packet, Some(0), false), (304, Some(0)));
  assert_eq!(
    opus_control_bytes(&packet, Some(312), false),
    (306, Some(0))
  );
  assert_eq!(
    opus_control_bytes(&packet, Some(2000), false),
    (306, Some(1040))
  );
  assert_eq!(opus_control_bytes(&packet, None, false), (306, None));
  assert_eq!(opus_control_bytes(&packet, Some(0), true), (306, Some(0)));
  let own = [0x7F, 0xE0, 0x11];
  assert_eq!(opus_control_bytes(&own, Some(312), true), (3, Some(312)));
  assert_eq!(opus_ts_trim(312, 48_000), Some(312));
  assert_eq!(opus_ts_trim(44_739, 48_000), Some(44_739));
  assert_eq!(opus_ts_trim(44_740, 48_000), None, "its int wrapped");
  let skip = |end: u32| {
    let mut data = vec![0; 10];
    data[4..8].copy_from_slice(&end.to_le_bytes());
    data
  };
  assert!(!opus_ts_end_trim(Some(&skip(0)), 48_000));
  assert!(opus_ts_end_trim(Some(&skip(1)), 48_000));
  assert!(opus_ts_end_trim(Some(&skip(89_478)), 48_000));
  assert!(
    !opus_ts_end_trim(Some(&skip(89_479)), 48_000),
    "its 32 bits wrapped below the rate"
  );
  assert!(
    !opus_ts_end_trim(Some(&skip(1)[..9]), 48_000),
    "of fewer than 10 bytes"
  );
  assert!(!opus_ts_end_trim(None, 48_000));
}

/// MPEG-TS's raw AAC is framed as FFmpeg's ADTS writer frames it (R20,
/// r3): the header's fields — the sync word, MPEG-4, no CRC, the profile,
/// the sampling index, the channel configuration across two bytes, the
/// 13-bit length, the fullness `0x7FF`, one block — and the program
/// config element behind its element id 5, the writer aligned with zeros,
/// the comment's count and bytes, its length the mirror's.
#[test]
fn adts_frames_are_made_as_ffmpegs_adts_writer_makes_them() {
  use super::{AdtsFields, AdtsFraming, adts_configuration, adts_frame_header, adts_pce_bytes};
  // AAC LC, 44.1 kHz, stereo.
  let config = adts_configuration(&[0x12, 0x10]).expect("LC");
  assert_eq!(
    config.fields,
    AdtsFields {
      profile: 1,
      sampling: 4,
      channels: 2,
    }
  );
  assert_eq!(
    adts_frame_header(config.fields, 7 + 300),
    [0xFF, 0xF1, 0x50, 0x80, 0x26, 0x7F, 0xFC]
  );
  // A length of 13 bits, its top two bits in the fourth byte.
  let long = adts_frame_header(config.fields, 8191);
  assert_eq!(
    (usize::from(long[3] & 3) << 11) | (usize::from(long[4]) << 3) | usize::from(long[5] >> 5),
    8191
  );
  // Seven channels' configuration, its top bit in the third byte.
  let seven = adts_frame_header(
    AdtsFields {
      profile: 1,
      sampling: 3,
      channels: 7,
    },
    7,
  );
  assert_eq!((seven[2] & 1, seven[3] >> 6), (1, 3));
  // Channel configuration 0 and a program config element of one front
  // pair, no comment: 39 bits of fields behind the element id's 3, aligned,
  // and the comment's count — 7 bytes.
  let pce_config = [0x12, 0x00, 0x05, 0x04, 0x00, 0x00, 0x20, 0x00];
  let config = adts_configuration(&pce_config).expect("a PCE");
  assert_eq!(config.pce, 7);
  let pce = adts_pce_bytes(&pce_config, config.pce_at);
  assert_eq!(pce, [0xA0, 0xA0, 0x80, 0x00, 0x04, 0x00, 0x00]);
  let framing = AdtsFraming::of(config.fields, 7 + 7 + 10, &pce);
  assert!(framing.pce);
  assert_eq!(&framing.bytes()[7..], &pce[..]);
  assert_eq!(framing.bytes().len(), 14);
}
