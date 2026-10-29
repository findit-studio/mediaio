//! The FFmpeg muxer over real containers: what it writes, read back by
//! the FFmpeg demuxer.
//!
//! The media is the corpus `mediadecode-ffmpeg`'s lanes generate with the
//! `ffmpeg` CLI, shared by path, and so is the generic remux — written
//! against the `Demuxer` and `Muxer` faces alone — which the
//! `generic_remux` lane there runs over a muxer that records and this
//! lane runs over the FFmpeg muxer.

#[path = "../../mediadecode-ffmpeg/tests/support/mod.rs"]
mod support;

use std::{
  fs::File,
  io::{self, Seek, SeekFrom, Write},
  num::NonZeroI32,
  path::{Path, PathBuf},
  sync::{
    Arc,
    atomic::{AtomicU32, Ordering},
  },
};

use mediadecode::{
  Timebase, Timestamp,
  demuxer::{
    AttachmentTrackPacket, AudioTrackPacket, DemuxedPacket, Demuxer, SubtitleTrackPacket,
    TrackDisposition, TrackIndex, TrackInfo, TrackKind, VideoTrackPacket,
  },
};
use mediadecode_ffmpeg::{CodecId, Ffmpeg, FfmpegDemuxer, FfmpegOwnedDemuxer};
use mediaencode::{
  MuxError, MuxRefusal, Muxer,
  mediaspine::{
    demuxer::{AttachmentPacket, AttachmentTrackParams, Chapter, TrackParams},
    mediatime::Rounding,
    packet::PacketFlags,
    track::{AttachedFile, TrackDescription},
  },
};
use mediaencode_ffmpeg::{
  AutomaticFilter, ContainerName, FfmpegMuxRefusal, FfmpegMuxer, FfmpegOwnedMuxer, LayoutReadBack,
  MuxOpenError, MuxOptions, MuxStage, OutputError, OutputText, PayloadRule, Rewrite, SideDataFate,
  Uncarried,
};
use mediaio_ffmpeg::{
  CodecTicket, FfmpegBytes, PacketLimits,
  extras::{AttachmentPacketExtra, SideDataEntry},
};
use smol_bytes::Utf8Bytes;
use support::{Corpus, remux};

/// The codec parameters a row carries — the side-data carrier the FFmpeg
/// muxer's table takes.
fn ticket(row: &TrackInfo<Ffmpeg>) -> CodecTicket {
  row.extra().ticket().clone()
}

fn millis() -> Timebase {
  Timebase::new(1, NonZeroI32::new(1000).expect("non-zero"))
}

fn ms(value: i64) -> Timestamp {
  Timestamp::new(value, millis())
}

/// What a row declares, owned, for comparing two files' tables.
#[derive(Debug, PartialEq)]
struct Row {
  kind: TrackKind,
  codec: CodecId,
  language: Option<String>,
  disposition: TrackDisposition,
  filename: Option<String>,
  mime_type: Option<String>,
}

fn rows(path: &Path) -> Vec<Row> {
  let demuxer = FfmpegOwnedDemuxer::open(path).expect("open");
  demuxer
    .tracks()
    .iter()
    .map(|row| Row {
      kind: row.kind(),
      codec: row.params().codec(),
      language: row.language().map(|text| text.as_str().to_owned()),
      disposition: row.disposition(),
      filename: row.filename().map(|text| text.as_str().to_owned()),
      mime_type: row.mime_type().map(|text| text.as_str().to_owned()),
    })
    .collect()
}

/// One packet as a file holds it.
#[derive(Debug, PartialEq)]
struct Seen {
  bytes: Vec<u8>,
  pts: Option<Timestamp>,
  dts: Option<Timestamp>,
  /// Compared only where the container records it per packet: a cue's.
  duration: Option<Timestamp>,
  key: bool,
}

/// Every packet of `path`, per track, in the order the demuxer delivers
/// them.
fn packets(path: &Path) -> Vec<Vec<Seen>> {
  let mut demuxer = FfmpegOwnedDemuxer::open(path).expect("open");
  let mut out: Vec<Vec<Seen>> = demuxer.tracks().iter().map(|_| Vec::new()).collect();
  while let Some(packet) = demuxer.next_packet().expect("a packet or the end") {
    let track = packet.track().get();
    let seen = match &packet {
      DemuxedPacket::Video(p) => {
        let p = p.packet();
        Seen {
          bytes: p.data().as_ref().to_vec(),
          pts: p.pts(),
          dts: p.dts(),
          duration: None,
          key: p.flags().contains(PacketFlags::KEY),
        }
      }
      DemuxedPacket::Audio(p) => {
        let p = p.packet();
        Seen {
          bytes: p.data().as_ref().to_vec(),
          pts: p.pts(),
          dts: p.dts(),
          duration: None,
          key: p.flags().contains(PacketFlags::KEY),
        }
      }
      DemuxedPacket::Subtitle(p) => {
        let p = p.packet();
        Seen {
          bytes: p.data().as_ref().to_vec(),
          pts: p.pts(),
          dts: p.pts(),
          duration: p.duration(),
          key: p.flags().contains(PacketFlags::KEY),
        }
      }
      DemuxedPacket::Data(p) => {
        let p = p.packet();
        Seen {
          bytes: p.data().as_ref().to_vec(),
          pts: p.pts(),
          dts: p.pts(),
          duration: p.duration(),
          key: p.flags().contains(PacketFlags::KEY),
        }
      }
      DemuxedPacket::Attachment(p) => Seen {
        bytes: p.packet().data().as_ref().to_vec(),
        pts: None,
        dts: None,
        duration: None,
        key: false,
      },
    };
    out[track].push(seen);
  }
  out
}

fn chapters(path: &Path) -> Vec<Chapter<Ffmpeg>> {
  FfmpegOwnedDemuxer::open(path)
    .expect("open")
    .chapters()
    .to_vec()
}

/// Remuxes `source` into `container` at `out` through the generic remux
/// over the FFmpeg pair, answering what the muxer reported.
fn remux_through_ffmpeg(
  source: &Path,
  container: ContainerName,
  out: &Path,
) -> mediaencode_ffmpeg::MuxReport {
  let mut demuxer = FfmpegDemuxer::open(source).expect("open the source");
  let sink = File::create(out).expect("create the output");
  let mut report = None;
  remux(&mut demuxer, ticket, |table, chapters| {
    let (muxer, opened) =
      FfmpegMuxer::open(sink, container, table, chapters, PacketLimits::default())
        .expect("the muxer opens");
    report = Some(opened);
    muxer
  });
  report.expect("the muxer was opened")
}

fn output(corpus: &Corpus, name: &str) -> PathBuf {
  corpus.dir().join(name)
}

/// **L1 — a Matroska remux into Matroska keeps everything both rows
/// carry.** Through the generic remux over the FFmpeg pair, the copy
/// reads back with the source's table — each track's kind, codec,
/// language and disposition, the font's filename and MIME type — its
/// chapters, and every packet's bytes, timestamps and key flag (and a
/// cue's duration), with nothing reported.
#[test]
fn l1_a_matroska_remux_into_matroska_keeps_what_both_rows_carry() {
  let Some(corpus) = Corpus::new() else { return };
  let source = corpus.remux_source_mkv();
  let copy = output(&corpus, "l1.mkv");
  let report = remux_through_ffmpeg(&source, ContainerName::MATROSKA, &copy);
  assert!(report.is_empty(), "Matroska carries it all: {report:?}");

  let (before, after) = (rows(&source), rows(&copy));
  assert_eq!(after, before, "the table");
  assert!(
    before
      .iter()
      .any(|row| row.language.as_deref() == Some("jpn"))
      && before
        .iter()
        .any(|row| row.disposition.contains(TrackDisposition::FORCED)),
    "the premise: the source declares languages and a forced track",
  );
  assert_eq!(chapters(&copy), chapters(&source), "the chapters");
  assert_eq!(chapters(&source).len(), 2, "the premise: two chapters");

  let (before, after) = (packets(&source), packets(&copy));
  assert_eq!(after.len(), before.len());
  for (track, (after, before)) in after.iter().zip(&before).enumerate() {
    assert!(!before.is_empty(), "the premise: track {track} has packets");
    assert_eq!(after, before, "track {track}'s packets");
  }
  assert_eq!(
    before[3],
    vec![Seen {
      bytes: support::FONT_PAYLOAD.to_vec(),
      pts: None,
      dts: None,
      duration: None,
      key: false,
    }],
    "the font, whole",
  );
}

/// **L2 — into MP4, SubRip is refused by name and the font is
/// reported.** MP4 stores no SubRip, so a table carrying it is refused at
/// open — nothing written — naming the track and the codec. Without it,
/// the muxer opens and reports the font MP4 has no place for; video and
/// audio come back packet for packet, and no attachment does.
#[test]
fn l2_into_mp4_subrip_is_refused_by_name_and_the_font_is_reported() {
  let Some(corpus) = Corpus::new() else { return };
  let source = corpus.multi_track_mkv();
  let target = output(&corpus, "l2.mp4");

  let table = |keep: &dyn Fn(TrackKind) -> bool| {
    let mut demuxer = FfmpegDemuxer::open(&source).expect("open the source");
    let mut files: Vec<Option<_>> = demuxer.tracks().iter().map(|_| None).collect();
    let mut timed = Vec::new();
    while let Some(packet) = demuxer.next_packet().expect("a packet or the end") {
      match packet {
        DemuxedPacket::Attachment(attachment) => {
          let (track, payload) = attachment.into_parts();
          files[track.get()] = Some(payload);
        }
        packet => timed.push(packet),
      }
    }
    let mut kept = Vec::new();
    let mut table = Vec::new();
    for (index, (row, file)) in demuxer.tracks().iter().zip(files).enumerate() {
      if keep(row.kind()) {
        kept.push(index);
        table.push(
          TrackDescription::from_track_info(&**row, file, ticket(row)).expect("a description"),
        );
      }
    }
    (table, kept, timed)
  };

  let (everything, _, _) = table(&|_| true);
  let refused = FfmpegMuxer::open(
    File::create(&target).expect("create"),
    ContainerName::MP4,
    everything,
    &[],
    PacketLimits::default(),
  );
  match refused {
    Err(MuxOpenError::CodecNotCarried { track, codec }) => {
      assert_eq!((track.get(), codec), (2, CodecId::SUBRIP));
    }
    other => panic!("expected the SubRip track refused by name, got {other:?}"),
  }
  assert_eq!(
    std::fs::metadata(&target).expect("the file").len(),
    0,
    "nothing was written"
  );

  let (without_cues, kept, timed) = table(&|kind| kind != TrackKind::Subtitle);
  let (mut muxer, report) = FfmpegMuxer::open(
    File::create(&target).expect("create"),
    ContainerName::MP4,
    without_cues,
    &[],
    PacketLimits::default(),
  )
  .expect("the muxer opens without the cues");
  assert!(
    report.uncarried().contains(&Uncarried::Attachment {
      track: TrackIndex::new(2)
    }),
    "the font is reported: {report:?}",
  );
  for packet in timed {
    let Some(index) = kept.iter().position(|&kept| kept == packet.track().get()) else {
      continue;
    };
    let track = TrackIndex::new(index);
    let routed = match packet {
      DemuxedPacket::Video(p) => {
        DemuxedPacket::Video(VideoTrackPacket::new(track, p.into_parts().1))
      }
      DemuxedPacket::Audio(p) => {
        DemuxedPacket::Audio(AudioTrackPacket::new(track, p.into_parts().1))
      }
      other => panic!("only video and audio are kept: {other:?}"),
    };
    muxer
      .write_packet(routed)
      .map_err(|error| error.map_packet(drop))
      .expect("a packet");
  }
  muxer.finish().expect("the container is finished");

  let written = rows(&target);
  assert_eq!(
    written.iter().map(|row| row.kind).collect::<Vec<_>>(),
    vec![TrackKind::Video, TrackKind::Audio],
    "video and audio, and no attachment",
  );
  let (before, after) = (packets(&source), packets(&target));
  for (track, source_track) in [(0, 0), (1, 1)] {
    let bytes = |seen: &[Seen]| seen.iter().map(|p| p.bytes.clone()).collect::<Vec<_>>();
    assert_eq!(
      bytes(&after[track]),
      bytes(&before[source_track]),
      "track {track}'s bytes"
    );
  }
}

/// **L3 — an MP3's cover art reaches MP4 as its cover.** The cover
/// track goes to MP4 as a picture of the cover kind, written once after
/// the header, and reads back as the attachment it was, byte for byte;
/// the audio comes with it.
#[test]
fn l3_an_mp3s_cover_art_reaches_mp4_as_its_cover() {
  let Some(corpus) = Corpus::new() else { return };
  let source = corpus.cover_art_mp3();
  let target = output(&corpus, "l3.mp4");
  let report = remux_through_ffmpeg(&source, ContainerName::MP4, &target);
  assert!(
    !report
      .uncarried()
      .iter()
      .any(|lost| matches!(lost, Uncarried::Attachment { .. })),
    "MP4 carries cover art: {report:?}",
  );

  let (before, after) = (packets(&source), packets(&target));
  let cover = |rows: &[Row], packets: &[Vec<Seen>]| {
    let index = rows
      .iter()
      .position(|row| row.kind == TrackKind::Attachment)
      .expect("a cover track");
    (rows[index].codec, packets[index][0].bytes.clone())
  };
  let (source_rows, target_rows) = (rows(&source), rows(&target));
  let (codec, bytes) = cover(&source_rows, &before);
  assert_eq!(codec, CodecId::PNG, "the premise: a PNG cover");
  assert_eq!(
    cover(&target_rows, &after),
    (codec, bytes),
    "the cover, whole"
  );
  let audio = |rows: &[Row], packets: &[Vec<Seen>]| {
    let index = rows
      .iter()
      .position(|row| row.kind == TrackKind::Audio)
      .expect("an audio track");
    packets[index]
      .iter()
      .map(|p| p.bytes.clone())
      .collect::<Vec<_>>()
  };
  assert_eq!(
    audio(&target_rows, &after),
    audio(&source_rows, &before),
    "the audio"
  );
}

/// Owned packets taken from a file for the refusal laws.
struct Sample {
  video: Vec<VideoTrackPacket<Ffmpeg, FfmpegBytes>>,
  font: Option<AttachmentTrackPacket<Ffmpeg, FfmpegBytes>>,
  audio: Option<AudioTrackPacket<Ffmpeg, FfmpegBytes>>,
}

/// The first `count` video packets of `path`, owned, and its first
/// attachment and audio packets.
fn owned_video_and_font(path: &Path, count: usize) -> Sample {
  let mut demuxer = FfmpegOwnedDemuxer::open(path).expect("open");
  let mut sample = Sample {
    video: Vec::new(),
    font: None,
    audio: None,
  };
  while let Some(packet) = demuxer.next_packet().expect("a packet or the end") {
    match packet {
      DemuxedPacket::Video(p) if sample.video.len() < count => sample.video.push(p),
      DemuxedPacket::Attachment(p) if sample.font.is_none() => sample.font = Some(p),
      DemuxedPacket::Audio(p) if sample.audio.is_none() => sample.audio = Some(p),
      _ => {}
    }
    if sample.video.len() == count && sample.font.is_some() && sample.audio.is_some() {
      break;
    }
  }
  sample
}

/// A one-track table: `path`'s video track.
fn video_table(path: &Path) -> Vec<TrackDescription<Ffmpeg, FfmpegBytes, CodecTicket>> {
  let demuxer = FfmpegOwnedDemuxer::open(path).expect("open");
  let row = demuxer
    .tracks()
    .iter()
    .find(|row| row.kind() == TrackKind::Video)
    .expect("a video track");
  vec![TrackDescription::from_track_info(&**row, None, ticket(row)).expect("a description")]
}

/// `packet` on track `track`, decoded at `dts` and presented at `pts`.
fn restamped(
  packet: &VideoTrackPacket<Ffmpeg, FfmpegBytes>,
  track: usize,
  pts: Option<i64>,
  dts: Option<i64>,
) -> VideoTrackPacket<Ffmpeg, FfmpegBytes> {
  let (_, packet) = packet.clone().into_parts();
  VideoTrackPacket::new(
    TrackIndex::new(track),
    packet.with_pts(pts.map(ms)).with_dts(dts.map(ms)),
  )
}

/// The portable refusal and the packet handed back, or a panic naming
/// what came instead.
fn refused<P: core::fmt::Debug>(
  result: Result<(), MuxError<mediaencode_ffmpeg::MuxFault, FfmpegMuxRefusal, P>>,
) -> (MuxRefusal, P) {
  match result {
    Err(MuxError::Refused { refusal, packet }) => (refusal, packet),
    other => panic!("expected a portable refusal, got {other:?}"),
  }
}

/// **L4 — every refusal hands the packet back.** On an open FFmpeg
/// muxer: a track it does not have, a packet of another kind, a missing
/// decode time, an earlier one, a presentation before its decode and an
/// attachment offered as a packet are refused by the face's names; an
/// equal decode time is refused by MP4's own name and admitted by
/// Matroska. Every refused packet comes back holding the very bytes it
/// was offered with, and nothing refused reaches the file.
#[test]
fn l4_every_refusal_hands_the_packet_back() {
  let Some(corpus) = Corpus::new() else { return };
  let source = corpus.multi_track_mkv();
  let Sample { video, font, audio } = owned_video_and_font(&source, 3);
  let (font, audio) = (font.expect("the font"), audio.expect("an audio packet"));
  assert_eq!(video.len(), 3, "the premise: three video packets");

  let target = output(&corpus, "l4.mp4");
  let (mut muxer, _) = FfmpegOwnedMuxer::open(
    File::create(&target).expect("create"),
    ContainerName::MP4,
    video_table(&source),
    &[],
    PacketLimits::default(),
  )
  .expect("open");
  muxer
    .write_video(restamped(&video[0], 0, Some(0), Some(0)))
    .expect("the first packet");
  muxer
    .write_video(restamped(&video[1], 0, Some(40), Some(40)))
    .expect("the second packet");

  let check = |offered: &VideoTrackPacket<Ffmpeg, FfmpegBytes>,
               back: &VideoTrackPacket<Ffmpeg, FfmpegBytes>| {
    assert_eq!(back.track(), offered.track());
    assert!(
      back.packet().data().ptr_eq(offered.packet().data()),
      "the very bytes it was offered with",
    );
    assert_eq!(back.packet().pts(), offered.packet().pts());
    assert_eq!(back.packet().dts(), offered.packet().dts());
  };

  let offered = restamped(&video[2], 5, Some(80), Some(80));
  let (refusal, back) = refused(muxer.write_video(offered.clone()));
  assert!(matches!(refusal, MuxRefusal::UnknownTrack { track } if track.get() == 5));
  check(&offered, &back);

  let (_, audio_packet) = audio.into_parts();
  let offered_audio = AudioTrackPacket::new(TrackIndex::new(0), audio_packet);
  let (refusal, back) = refused(muxer.write_audio(offered_audio.clone()));
  assert!(matches!(
    refusal,
    MuxRefusal::KindMismatch {
      expected: TrackKind::Video,
      offered: TrackKind::Audio,
      ..
    }
  ));
  assert!(back.packet().data().ptr_eq(offered_audio.packet().data()));

  let offered = restamped(&video[2], 0, Some(80), None);
  let (refusal, back) = refused(muxer.write_video(offered.clone()));
  assert!(matches!(refusal, MuxRefusal::MissingDts { .. }));
  check(&offered, &back);

  let offered = restamped(&video[2], 0, Some(80), Some(20));
  let (refusal, back) = refused(muxer.write_video(offered.clone()));
  assert!(matches!(refusal, MuxRefusal::NonMonotonicDts { .. }));
  check(&offered, &back);

  let offered = restamped(&video[2], 0, Some(60), Some(80));
  let (refusal, back) = refused(muxer.write_video(offered.clone()));
  assert!(matches!(refusal, MuxRefusal::PtsBeforeDts { .. }));
  check(&offered, &back);

  let offered = restamped(&video[2], 0, Some(80), Some(40));
  match muxer.write_video(offered.clone()) {
    Err(MuxError::Backend {
      refusal: FfmpegMuxRefusal::RepeatedDts { track, dts },
      packet: back,
    }) => {
      assert_eq!((track.get(), dts), (0, ms(40)));
      check(&offered, &back);
    }
    other => panic!("expected MP4's own refusal of a repeated decode time, got {other:?}"),
  }

  let offered_font = DemuxedPacket::Attachment(font.clone());
  let (refusal, back) = refused(muxer.write_packet(offered_font));
  assert!(matches!(refusal, MuxRefusal::Attachment { .. }));
  match back {
    DemuxedPacket::Attachment(back) => {
      assert!(back.packet().data().ptr_eq(font.packet().data()));
    }
    other => panic!("expected the attachment back, got {other:?}"),
  }

  muxer
    .write_video(restamped(&video[2], 0, Some(80), Some(80)))
    .expect("the third packet, advancing");
  muxer.finish().expect("the container is finished");
  assert_eq!(
    packets(&target)[0].len(),
    3,
    "only what was admitted was written"
  );

  // Matroska is not strict: the repeat is written.
  let (mut muxer, _) = FfmpegOwnedMuxer::open(
    File::create(output(&corpus, "l4.mkv")).expect("create"),
    ContainerName::MATROSKA,
    video_table(&source),
    &[],
    PacketLimits::default(),
  )
  .expect("open");
  muxer
    .write_video(restamped(&video[0], 0, Some(0), Some(0)))
    .expect("the first packet");
  muxer
    .write_video(restamped(&video[1], 0, Some(40), Some(40)))
    .expect("the second packet");
  muxer
    .write_video(restamped(&video[2], 0, Some(80), Some(40)))
    .expect("a repeated decode time, admitted by Matroska");
  muxer.finish().expect("the container is finished");
}

/// **L5 — an open refusal names what the container cannot take, and
/// writes nothing.** An unknown container; Matroska's attachment with no
/// filename, and one whose MIME type nobody knows; a codec WebM cannot
/// store; a chapter that runs backwards; a language holding a NUL.
#[test]
fn l5_an_open_refusal_names_what_the_container_cannot_take() {
  let Some(corpus) = Corpus::new() else { return };
  let source = corpus.multi_track_mkv();
  let target = output(&corpus, "l5.mkv");
  let open = |container: ContainerName,
              table: Vec<TrackDescription<Ffmpeg, FfmpegBytes, CodecTicket>>,
              chapters: &[Chapter<Ffmpeg>]| {
    let result = FfmpegOwnedMuxer::open(
      File::create(&target).expect("create"),
      container,
      table,
      chapters,
      PacketLimits::default(),
    );
    assert_eq!(
      std::fs::metadata(&target).expect("the file").len(),
      0,
      "nothing was written"
    );
    result.map(drop)
  };

  assert!(matches!(
    open(ContainerName::new("no-such-muxer"), video_table(&source), &[]),
    Err(MuxOpenError::UnknownContainer { name }) if name.as_str() == "no-such-muxer"
  ));

  // An attachment with no filename, and one with no MIME type Matroska
  // could name.
  let attachment = |codec: CodecId, filename: Option<&str>, ticket: CodecTicket| {
    let payload = AttachmentPacket::new(
      FfmpegBytes::try_copy_from_slice(b"a file").expect("a payload"),
      AttachmentPacketExtra::new(0),
    );
    TrackDescription::attachment(
      millis(),
      AttachmentTrackParams::new(codec),
      AttachedFile::new(payload).with_filename(filename.map(Into::into)),
      ticket,
    )
  };
  let font_ticket = {
    let demuxer = FfmpegOwnedDemuxer::open(&source).expect("open");
    ticket(
      demuxer
        .tracks()
        .iter()
        .find(|row| row.kind() == TrackKind::Attachment)
        .expect("the font"),
    )
  };
  let none_ticket = CodecTicket::mirror(&ffmpeg_next::codec::Parameters::new(), 0, 1 << 20)
    .expect("an empty ticket");
  assert!(matches!(
    open(
      ContainerName::MATROSKA,
      vec![attachment(CodecId::TTF, None, font_ticket.clone())],
      &[]
    ),
    Err(MuxOpenError::AttachmentWithoutFilename { track }) if track.get() == 0
  ));
  assert!(matches!(
    open(
      ContainerName::MATROSKA,
      vec![attachment(CodecId::NONE, Some("file.bin"), none_ticket)],
      &[]
    ),
    Err(MuxOpenError::AttachmentWithoutMimeType { track }) if track.get() == 0
  ));

  assert!(matches!(
    open(ContainerName::WEBM, video_table(&source), &[]),
    Err(MuxOpenError::CodecNotCarried { track, codec }) if track.get() == 0 && codec == CodecId::H264
  ));

  let backwards = Chapter::<Ffmpeg>::new(1, millis(), ms(2000), ms(1000));
  assert!(matches!(
    open(ContainerName::MATROSKA, video_table(&source), &[backwards]),
    Err(MuxOpenError::ChapterSpan { chapter: 0 })
  ));

  let mut table = video_table(&source);
  table[0].set_language(Some("e\0ng".into()));
  assert!(matches!(
    open(ContainerName::MATROSKA, table, &[]),
    Err(MuxOpenError::TextWithNul {
      track,
      text: OutputText::Language
    }) if track.get() == 0
  ));
}

/// A sink that counts its calls and panics in every one.
#[derive(Default)]
struct Exploding {
  calls: Arc<AtomicU32>,
}

impl Write for Exploding {
  fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
    self.calls.fetch_add(1, Ordering::Relaxed);
    panic!("the sink exploded");
  }
  fn flush(&mut self) -> io::Result<()> {
    self.calls.fetch_add(1, Ordering::Relaxed);
    panic!("the sink exploded");
  }
}

impl Seek for Exploding {
  fn seek(&mut self, _pos: SeekFrom) -> io::Result<u64> {
    self.calls.fetch_add(1, Ordering::Relaxed);
    panic!("the sink exploded");
  }
}

/// **L6 — a sink that panics is named in the fault, and never called
/// again.** libavformat drives the sink from C; the panic inside it is
/// caught at the boundary, the session's fault names it, and no later
/// call reaches the sink — the process does not abort.
#[test]
fn l6_a_sink_that_panics_is_named_in_the_fault() {
  let Some(corpus) = Corpus::new() else { return };
  let source = corpus.multi_track_mkv();
  let video = owned_video_and_font(&source, 3).video;
  let sink = Exploding::default();
  let calls = Arc::clone(&sink.calls);

  let previous = std::panic::take_hook();
  std::panic::set_hook(Box::new(|_| {}));
  let fault = (|| {
    let (mut muxer, _) = match FfmpegOwnedMuxer::open(
      sink,
      ContainerName::MATROSKA,
      video_table(&source),
      &[],
      PacketLimits::default(),
    ) {
      Ok(opened) => opened,
      Err(MuxOpenError::Fault(fault)) => return fault,
      Err(other) => panic!("expected the muxer to open or fault, got {other:?}"),
    };
    for (index, packet) in video.iter().enumerate() {
      let at = 40 * index as i64;
      if let Err(error) = muxer.write_video(restamped(packet, 0, Some(at), Some(at))) {
        match error {
          MuxError::Fault(fault) => return fault,
          other => panic!("expected a fault, got {other:?}"),
        }
      }
    }
    muxer.finish().expect_err("the sink panicked")
  })();
  std::panic::set_hook(previous);

  assert_eq!(
    fault.sink_panic().map(|message| message.as_str()),
    Some("the sink exploded"),
    "the fault names the panic: {fault:?}",
  );
  assert!(matches!(fault.error(), Some(OutputError::SinkPanic { .. })));
  assert!(matches!(
    fault.stage(),
    MuxStage::Header | MuxStage::Write { .. } | MuxStage::Trailer
  ));
  assert_eq!(
    calls.load(Ordering::Relaxed),
    1,
    "only the first call reached the sink"
  );
}

/// **The generic remux runs over the FFmpeg pair, cues at one instant
/// included.** Matroska is not strict and a subtitle stream never is, so
/// the two cues sharing a start time are both written, and read back at
/// that one instant.
#[test]
fn the_generic_remux_writes_cues_that_share_an_instant_through_the_ffmpeg_muxer() {
  let Some(corpus) = Corpus::new() else { return };
  let source = corpus.simultaneous_cues_mkv();
  let copy = output(&corpus, "cues.mkv");
  let report = remux_through_ffmpeg(&source, ContainerName::MATROSKA, &copy);
  assert!(report.is_empty(), "{report:?}");
  let cues = &packets(&copy)[1];
  assert_eq!(cues.len(), 2, "both cues were written");
  assert_eq!(cues[0].pts, cues[1].pts, "at one instant");
  assert_eq!(packets(&copy), packets(&source), "and everything else");
}

/// **L7 — a presentation or decode time that lands on `AV_NOPTS_VALUE`
/// is refused by name, the packet untouched.** `i64::MIN` is the count
/// libavformat reads as no timestamp at all. Into Matroska's 1/1000
/// stream a decode time of `i64::MIN` milliseconds counts to it, and so
/// does a presentation time; each is refused as `Unrepresentable`,
/// naming the time offered, and handed back, and only the packet with
/// real times reaches the file.
#[test]
fn l7_a_time_that_lands_on_no_timestamp_is_refused_by_name() {
  let Some(corpus) = Corpus::new() else { return };
  let source = corpus.multi_track_mkv();
  let video = owned_video_and_font(&source, 1).video;
  let target = output(&corpus, "l7.mkv");
  let (mut muxer, _) = FfmpegOwnedMuxer::open(
    File::create(&target).expect("create"),
    ContainerName::MATROSKA,
    video_table(&source),
    &[],
    PacketLimits::default(),
  )
  .expect("open");

  for (pts, dts, named) in [
    (Some(0), Some(i64::MIN), ms(i64::MIN)),
    (Some(i64::MIN), Some(0), ms(i64::MIN)),
  ] {
    let offered = restamped(&video[0], 0, pts, dts);
    match muxer.write_video(offered.clone()) {
      Err(MuxError::Backend {
        refusal: FfmpegMuxRefusal::Unrepresentable { track, timestamp },
        packet: back,
      }) => {
        assert_eq!(track.get(), 0);
        assert_eq!(
          (timestamp.pts(), timestamp.timebase()),
          (named.pts(), named.timebase()),
          "the time offered is named",
        );
        assert!(back.packet().data().ptr_eq(offered.packet().data()));
        assert_eq!(back.packet().pts(), offered.packet().pts());
        assert_eq!(back.packet().dts(), offered.packet().dts());
      }
      other => panic!("expected the time refused as unrepresentable, got {other:?}"),
    }
  }

  muxer
    .write_video(restamped(&video[0], 0, Some(0), Some(0)))
    .expect("real times are written");
  muxer.finish().expect("the container is finished");
  assert_eq!(
    packets(&target)[0].len(),
    1,
    "only what was admitted was written"
  );
}

/// **L8 — presentation order is judged on the counts libavformat is
/// handed.** Into Matroska's 1/1000 stream, a packet presented at
/// 3/10 000 s and decoded at 4/10 000 s counts to one tick, 0, for both —
/// which libavformat writes, as a stream copy of a file with sub-tick
/// jitter does — so it is admitted. One presented at 30 ms and decoded at
/// 40 ms is an inversion of the counts: refused by the face's name with
/// the times it was offered with, not their counts, and handed back.
#[test]
fn l8_presentation_order_is_judged_on_the_counts_libavformat_is_handed() {
  let Some(corpus) = Corpus::new() else { return };
  let source = corpus.multi_track_mkv();
  let video = owned_video_and_font(&source, 2).video;
  let target = output(&corpus, "l8.mkv");
  let (mut muxer, _) = FfmpegOwnedMuxer::open(
    File::create(&target).expect("create"),
    ContainerName::MATROSKA,
    video_table(&source),
    &[],
    PacketLimits::default(),
  )
  .expect("open");
  let tenth_ms = Timebase::new(1, NonZeroI32::new(10_000).expect("non-zero"));
  let at = |packet: &VideoTrackPacket<Ffmpeg, FfmpegBytes>, pts: i64, dts: i64| {
    let (_, packet) = packet.clone().into_parts();
    VideoTrackPacket::new(
      TrackIndex::new(0),
      packet
        .with_pts(Some(Timestamp::new(pts, tenth_ms)))
        .with_dts(Some(Timestamp::new(dts, tenth_ms))),
    )
  };

  muxer
    .write_video(at(&video[0], 3, 4))
    .expect("presented and decoded within one tick: admitted");

  let offered = at(&video[1], 300, 400);
  let (refusal, back) = refused(muxer.write_video(offered.clone()));
  match refusal {
    MuxRefusal::PtsBeforeDts { track, pts, dts } => {
      assert_eq!(track.get(), 0);
      assert_eq!(
        (pts.pts(), pts.timebase(), dts.pts(), dts.timebase()),
        (300, tenth_ms, 400, tenth_ms),
        "the times the packet carries, not their counts",
      );
    }
    other => panic!("expected the presentation-order refusal, got {other:?}"),
  }
  assert!(back.packet().data().ptr_eq(offered.packet().data()));
  assert_eq!(back.packet().pts(), offered.packet().pts());
  assert_eq!(back.packet().dts(), offered.packet().dts());

  muxer.finish().expect("the container is finished");
  let written = &packets(&target)[0];
  assert_eq!(written.len(), 1, "only the admitted packet was written");
  assert_eq!(written[0].pts, Some(ms(0)), "presented at its one tick");
}

/// What a description offers, as a row: what a read-back row is judged
/// against.
fn offered(description: &TrackDescription<Ffmpeg, FfmpegBytes, CodecTicket>) -> Row {
  let text = |text: Option<&Utf8Bytes>| text.map(|text| text.as_str().to_owned());
  let file = description.file();
  Row {
    kind: description.kind(),
    codec: description.codec(),
    language: text(description.language()),
    disposition: description.disposition(),
    filename: text(file.and_then(|file| file.filename())),
    mime_type: text(file.and_then(|file| file.mime_type())),
  }
}

/// What `written` lost of `offered`, as the report names it, in its
/// order: each field the track declared that the file does not hold — a
/// declared text read back otherwise, or not at all, and each declared
/// flag read back unset — and each flag read back set that it did not
/// declare.
fn lost(track: usize, offered: &Row, written: &Row) -> Vec<Uncarried> {
  let track = TrackIndex::new(track);
  let dropped = |declared: &Option<String>, held: &Option<String>| {
    declared
      .as_ref()
      .filter(|declared| held.as_ref() != Some(*declared))
      .map(|declared| Utf8Bytes::from(declared.as_str()))
  };
  let mut lost = Vec::new();
  if let Some(filename) = dropped(&offered.filename, &written.filename) {
    lost.push(Uncarried::Filename { track, filename });
  }
  if let Some(mime_type) = dropped(&offered.mime_type, &written.mime_type) {
    lost.push(Uncarried::MimeType { track, mime_type });
  }
  if let Some(tag) = dropped(&offered.language, &written.language) {
    lost.push(Uncarried::Language { track, tag });
  }
  let flags = offered.disposition.difference(written.disposition);
  if !flags.is_empty() {
    lost.push(Uncarried::Disposition { track, flags });
  }
  let added = written.disposition.difference(offered.disposition);
  if !added.is_empty() {
    lost.push(Uncarried::DispositionAdded {
      track,
      flags: added,
    });
  }
  lost
}

/// **L9 — the report names every field a Matroska attachment drops, and
/// nothing else.** A font declaring its filename, MIME type, a language
/// and two flags reaches Matroska with its file, filename and MIME type,
/// and its language among the tags written for it — but an attachment
/// there has no track entry for its flags. Read back, the report and the
/// file agree track by track: the report names exactly what each track
/// declared and the file does not hold.
#[test]
fn l9_the_report_names_every_field_a_matroska_attachment_drops() {
  let Some(corpus) = Corpus::new() else { return };
  let source = corpus.multi_track_mkv();
  let video = owned_video_and_font(&source, 3).video;
  let font_ticket = {
    let demuxer = FfmpegOwnedDemuxer::open(&source).expect("open");
    ticket(
      demuxer
        .tracks()
        .iter()
        .find(|row| row.kind() == TrackKind::Attachment)
        .expect("the font"),
    )
  };
  let payload = AttachmentPacket::new(
    FfmpegBytes::try_copy_from_slice(support::FONT_PAYLOAD).expect("a payload"),
    AttachmentPacketExtra::new(0),
  );
  let font = TrackDescription::attachment(
    millis(),
    AttachmentTrackParams::new(CodecId::TTF),
    AttachedFile::new(payload)
      .with_filename(Some("font.ttf".into()))
      .with_mime_type(Some("application/x-truetype-font".into())),
    font_ticket,
  )
  .with_language(Some("jpn".into()))
  .with_disposition(TrackDisposition::DEFAULT | TrackDisposition::FORCED);
  let mut table = video_table(&source);
  table.push(font);
  let offered: Vec<Row> = table.iter().map(offered).collect();

  let target = output(&corpus, "l9.mkv");
  let (mut muxer, report) = FfmpegOwnedMuxer::open(
    File::create(&target).expect("create"),
    ContainerName::MATROSKA,
    table,
    &[],
    PacketLimits::default(),
  )
  .expect("open");
  for (index, packet) in video.iter().enumerate() {
    let at = 40 * index as i64;
    muxer
      .write_video(restamped(packet, 0, Some(at), Some(at)))
      .map_err(|error| error.map_packet(drop))
      .expect("a packet");
  }
  muxer.finish().expect("the container is finished");

  let written = rows(&target);
  assert_eq!(written.len(), offered.len(), "the video and the font");
  let expected: Vec<Uncarried> = offered
    .iter()
    .zip(&written)
    .enumerate()
    .flat_map(|(track, (offered, written))| lost(track, offered, written))
    .collect();
  assert_eq!(
    report.uncarried(),
    expected.as_slice(),
    "the report names exactly what the file lost",
  );
  assert_eq!(
    expected,
    vec![Uncarried::Disposition {
      track: TrackIndex::new(1),
      flags: TrackDisposition::DEFAULT | TrackDisposition::FORCED,
    }],
    "the premise: the font kept its filename, MIME type and language, and lost its flags",
  );
}

/// **L10 — the report names every field MP4's cover drops, and nothing
/// else.** An MP3's cover art, declared with a filename, a MIME type, a
/// language and two flags beside its own, reaches MP4 as its cover: the
/// picture alone, read back as an attached picture. The report and the
/// file agree track by track — the cover's filename, MIME type, language
/// and flags each named, and the audio's losses, if any, with them.
#[test]
fn l10_the_report_names_every_field_mp4s_cover_drops() {
  let Some(corpus) = Corpus::new() else { return };
  let source = corpus.cover_art_mp3();
  let mut demuxer = FfmpegOwnedDemuxer::open(&source).expect("open the source");
  let mut files: Vec<Option<_>> = demuxer.tracks().iter().map(|_| None).collect();
  let mut timed = Vec::new();
  while let Some(packet) = demuxer.next_packet().expect("a packet or the end") {
    match packet {
      DemuxedPacket::Attachment(attachment) => {
        let (track, payload) = attachment.into_parts();
        files[track.get()] = Some(payload);
      }
      packet => timed.push(packet),
    }
  }
  let mut table: Vec<TrackDescription<Ffmpeg, FfmpegBytes, CodecTicket>> = demuxer
    .tracks()
    .iter()
    .zip(files)
    .map(|(row, file)| {
      TrackDescription::from_track_info(&**row, file, ticket(row)).expect("a description")
    })
    .collect();
  let cover = table
    .iter()
    .position(|description| description.kind() == TrackKind::Attachment)
    .expect("the premise: a cover track");
  let (timebase, params, _, cover_ticket, _, disposition, file) =
    table.remove(cover).into_parts_with_disposition();
  let TrackParams::Attachment(params) = params else {
    panic!("the premise: the cover is an attachment");
  };
  let (payload, _, _) = file.expect("the cover's picture").into_parts();
  table.insert(
    cover,
    TrackDescription::attachment(
      timebase,
      params,
      AttachedFile::new(payload)
        .with_filename(Some("cover.png".into()))
        .with_mime_type(Some("image/png".into())),
      cover_ticket,
    )
    .with_language(Some("eng".into()))
    .with_disposition(disposition | TrackDisposition::DEFAULT | TrackDisposition::COMMENT),
  );
  let offered: Vec<Row> = table.iter().map(offered).collect();

  let target = output(&corpus, "l10.mp4");
  let (mut muxer, report) = FfmpegOwnedMuxer::open(
    File::create(&target).expect("create"),
    ContainerName::MP4,
    table,
    &[],
    PacketLimits::default(),
  )
  .expect("open");
  for packet in timed {
    muxer
      .write_packet(packet)
      .map_err(|error| error.map_packet(drop))
      .expect("a packet");
  }
  muxer.finish().expect("the container is finished");

  // MP4 reads its tracks back in an order of its own: each offered track
  // is judged against the written track of its kind.
  let written = rows(&target);
  assert_eq!(written.len(), offered.len(), "the audio and the cover");
  let expected: Vec<Uncarried> = offered
    .iter()
    .enumerate()
    .flat_map(|(track, offered)| {
      let written = written
        .iter()
        .find(|row| row.kind == offered.kind)
        .expect("a written track of each kind");
      lost(track, offered, written)
    })
    .collect();
  assert_eq!(
    report.uncarried(),
    expected.as_slice(),
    "the report names exactly what the file lost",
  );
  let track = TrackIndex::new(cover);
  let on_the_cover = |lost: &&Uncarried| match lost {
    Uncarried::Filename { track: at, .. }
    | Uncarried::MimeType { track: at, .. }
    | Uncarried::Language { track: at, .. }
    | Uncarried::Disposition { track: at, .. } => *at == track,
    _ => false,
  };
  assert_eq!(
    expected.iter().filter(on_the_cover).count(),
    4,
    "the premise: the cover lost its filename, MIME type, language and flags: {expected:?}",
  );
  for named in [
    Uncarried::Filename {
      track,
      filename: "cover.png".into(),
    },
    Uncarried::MimeType {
      track,
      mime_type: "image/png".into(),
    },
    Uncarried::Language {
      track,
      tag: "eng".into(),
    },
  ] {
    assert!(expected.contains(&named), "{named:?} in {expected:?}");
  }
}

// ---------------------------------------------------------------------------
//  The packet-rule table: one law per refused row.
// ---------------------------------------------------------------------------

/// A file the `ffmpeg` CLI makes in the corpus from `args`, once.
fn generated(corpus: &Corpus, name: &str, args: &[&str]) -> PathBuf {
  let out = output(corpus, name);
  if !out.exists() {
    let status = std::process::Command::new("ffmpeg")
      .args(["-y", "-loglevel", "error"])
      .args(args)
      .arg(&out)
      .status()
      .expect("the ffmpeg CLI runs");
    assert!(status.success(), "ffmpeg made {name}");
  }
  out
}

/// `bytes`, copied into the carrier's buffer.
fn bytes_of(bytes: &[u8]) -> FfmpegBytes {
  FfmpegBytes::try_copy_from_slice(bytes).expect("a copy")
}

/// The one-track table of `path`'s first track of `kind`, afresh: a
/// description is handed to the muxer, not cloned.
fn table_of(
  path: &Path,
  kind: TrackKind,
) -> Vec<TrackDescription<Ffmpeg, FfmpegBytes, CodecTicket>> {
  first_track(path, kind, 0).0
}

/// A text file written into the corpus, once.
fn written_text(corpus: &Corpus, name: &str, text: &str) -> PathBuf {
  let out = output(corpus, name);
  if !out.exists() {
    std::fs::write(&out, text).expect("write");
  }
  out
}

/// A 64 × 64 AV1 temporal unit — a temporal delimiter, a sequence header
/// and one shown key frame — as SVT-AV1 4.2 encodes a grey picture at its
/// lowest quality (`ffmpeg -f lavfi -i color=c=gray:s=64x64 -frames:v 1
/// -c:v libsvtav1 -crf 63 -preset 12`).
const AV1_TEMPORAL_UNIT: [u8; 29] = [
  0x12, 0x00, 0x0A, 0x0B, 0x02, 0x00, 0x00, 0x05, 0x15, 0x7F, 0xFC, 0x4A, 0xF9, 0x00, 0x40, 0x32,
  0x0C, 0x10, 0x00, 0xF3, 0x82, 0x3F, 0xFE, 0x69, 0x83, 0x00, 0x00, 0x05, 0x80,
];

/// AV1 video in Matroska, made with no AV1 encoder: ten
/// [`AV1_TEMPORAL_UNIT`]s, read as a low-overhead bitstream (the `obu`
/// demuxer, at its default 25 frames a second) and copied. An `ffmpeg` CLI
/// need not be built with an AV1 encoder, and CI's is not; every build has
/// the `obu` demuxer and the Matroska muxer, and the copy carries the
/// sequence header as the track's configuration (`av1C`).
#[rustfmt::skip]
fn av1_mkv(corpus: &Corpus) -> PathBuf {
  let units = output(corpus, "av1.obu");
  if !units.exists() {
    std::fs::write(&units, AV1_TEMPORAL_UNIT.repeat(10)).expect("write");
  }
  generated(corpus, "av1.mkv", &[
    "-f", "obu", "-i", units.to_str().expect("utf-8 path"), "-c:v", "copy",
  ])
}

/// H.264 with B-frames — a stream that reorders — in Matroska.
#[rustfmt::skip]
fn reordering_mkv(corpus: &Corpus) -> PathBuf {
  generated(corpus, "bframes.mkv", &[
    "-f", "lavfi", "-i", "testsrc2=size=64x48:rate=25:duration=1",
    "-c:v", "libx264", "-preset", "medium", "-bf", "2", "-g", "25", "-pix_fmt", "yuv420p",
  ])
}

/// An audio file of `codec` at 48 kHz, stereo, in `name`'s container.
#[rustfmt::skip]
fn audio_file(corpus: &Corpus, name: &str, codec: &str) -> PathBuf {
  generated(corpus, name, &[
    "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000:duration=0.5",
    "-ac", "2", "-c:a", codec,
  ])
}

/// Stereo PCM in QuickTime, its channel layout stated: MP4's `ipcm`
/// writes one (`mov_write_chnl_tag`, `movenc.c` 1299–1323), which
/// Matroska does not keep.
#[rustfmt::skip]
fn pcm_mov(corpus: &Corpus) -> PathBuf {
  generated(corpus, "pcm.mov", &[
    "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000:duration=0.5",
    "-ch_layout", "stereo", "-c:a", "pcm_s16le",
  ])
}

/// ProRes video in QuickTime.
#[rustfmt::skip]
fn prores_mov(corpus: &Corpus) -> PathBuf {
  generated(corpus, "prores.mov", &[
    "-f", "lavfi", "-i", "testsrc2=size=64x64:rate=25:duration=0.2",
    "-c:v", "prores_ks", "-profile:v", "0", "-pix_fmt", "yuv422p10le",
  ])
}

/// Two `mov_text` cues in MP4.
#[rustfmt::skip]
fn mov_text_mp4(corpus: &Corpus) -> PathBuf {
  let srt = written_text(
    corpus,
    "two.srt",
    "1\n00:00:00,000 --> 00:00:01,000\nhello\n\n2\n00:00:01,000 --> 00:00:02,000\nworld\n\n",
  );
  generated(corpus, "movtext.mp4", &[
    "-i", srt.to_str().expect("utf-8 path"), "-c:s", "mov_text",
  ])
}

/// Two WebVTT cues in Matroska.
#[rustfmt::skip]
fn webvtt_mkv(corpus: &Corpus) -> PathBuf {
  let vtt = written_text(
    corpus,
    "two.vtt",
    "WEBVTT\n\n00:00.000 --> 00:01.000\nhello\n\n00:01.000 --> 00:02.000\nworld\n",
  );
  generated(corpus, "webvtt.mkv", &[
    "-i", vtt.to_str().expect("utf-8 path"), "-c:s", "webvtt",
  ])
}

/// A one-track table, and packets of that track.
type Track = (
  Vec<TrackDescription<Ffmpeg, FfmpegBytes, CodecTicket>>,
  Vec<DemuxedPacket<Ffmpeg, FfmpegBytes>>,
);

/// The first track of `kind` in `path` as a one-track table, and that
/// track's first `count` packets, owned.
fn first_track(path: &Path, kind: TrackKind, count: usize) -> Track {
  let mut demuxer = FfmpegOwnedDemuxer::open(path).expect("open");
  let (index, description) = demuxer
    .tracks()
    .iter()
    .enumerate()
    .find(|(_, row)| row.kind() == kind)
    .map(|(index, row)| {
      (
        index,
        TrackDescription::from_track_info(&**row, None, ticket(row)).expect("a description"),
      )
    })
    .expect("a track of the kind");
  let mut packets = Vec::new();
  while packets.len() < count {
    let Some(packet) = demuxer.next_packet().expect("a packet or the end") else {
      break;
    };
    if packet.track().get() == index {
      packets.push(packet);
    }
  }
  assert_eq!(packets.len(), count, "the premise: {count} packets");
  (vec![description], packets)
}

type Video = mediaencode::mediaspine::demuxer::DemuxVideoPacket<Ffmpeg, FfmpegBytes>;
type Audio = mediaencode::mediaspine::demuxer::DemuxAudioPacket<Ffmpeg, FfmpegBytes>;
type Subtitle = mediaencode::mediaspine::demuxer::DemuxSubtitlePacket<Ffmpeg, FfmpegBytes>;

fn video_of(packet: &DemuxedPacket<Ffmpeg, FfmpegBytes>) -> Video {
  match packet {
    DemuxedPacket::Video(p) => p.packet().clone(),
    other => panic!("expected video, got {other:?}"),
  }
}

fn audio_of(packet: &DemuxedPacket<Ffmpeg, FfmpegBytes>) -> Audio {
  match packet {
    DemuxedPacket::Audio(p) => p.packet().clone(),
    other => panic!("expected audio, got {other:?}"),
  }
}

fn subtitle_of(packet: &DemuxedPacket<Ffmpeg, FfmpegBytes>) -> Subtitle {
  match packet {
    DemuxedPacket::Subtitle(p) => p.packet().clone(),
    other => panic!("expected a subtitle, got {other:?}"),
  }
}

/// `packet` on track 0, presented at `pts` and decoded at `dts`
/// milliseconds.
fn video_at(packet: &Video, pts: Option<i64>, dts: i64) -> VideoTrackPacket<Ffmpeg, FfmpegBytes> {
  VideoTrackPacket::new(
    TrackIndex::new(0),
    packet.clone().with_pts(pts.map(ms)).with_dts(Some(ms(dts))),
  )
}

fn audio_at(packet: &Audio, at: i64) -> AudioTrackPacket<Ffmpeg, FfmpegBytes> {
  AudioTrackPacket::new(
    TrackIndex::new(0),
    packet.clone().with_pts(Some(ms(at))).with_dts(Some(ms(at))),
  )
}

fn subtitle_at(
  packet: &Subtitle,
  at: i64,
  duration: i64,
) -> SubtitleTrackPacket<Ffmpeg, FfmpegBytes> {
  SubtitleTrackPacket::new(
    TrackIndex::new(0),
    packet
      .clone()
      .with_pts(Some(ms(at)))
      .with_duration(Some(ms(duration))),
  )
}

/// `packet` holding `bytes` instead, everything else as it was.
fn video_holding(packet: &Video, bytes: &[u8]) -> Video {
  let (_, extra) = packet.clone().into_parts();
  mediaencode::mediaspine::packet::VideoPacket::new(bytes_of(bytes), extra)
    .with_pts(packet.pts())
    .with_dts(packet.dts())
    .with_duration(packet.duration())
    .with_flags(packet.flags())
}

fn audio_holding(packet: &Audio, bytes: &[u8]) -> Audio {
  let (_, extra) = packet.clone().into_parts();
  mediaencode::mediaspine::packet::AudioPacket::new(bytes_of(bytes), extra)
    .with_pts(packet.pts())
    .with_dts(packet.dts())
    .with_duration(packet.duration())
    .with_flags(packet.flags())
}

fn subtitle_holding(packet: &Subtitle, bytes: &[u8]) -> Subtitle {
  let (_, extra) = packet.clone().into_parts();
  mediaencode::mediaspine::packet::SubtitlePacket::new(bytes_of(bytes), extra)
    .with_pts(packet.pts())
    .with_duration(packet.duration())
    .with_flags(packet.flags())
}

/// One side data entry of `kind`.
fn side(kind: ffmpeg_next::ffi::AVPacketSideDataType, bytes: &[u8]) -> SideDataEntry {
  SideDataEntry::new(kind as i32, bytes_of(bytes))
}

/// A muxer for `container` into `target`, over `table`.
fn muxer_into(
  target: &Path,
  container: ContainerName,
  table: Vec<TrackDescription<Ffmpeg, FfmpegBytes, CodecTicket>>,
) -> FfmpegOwnedMuxer<File> {
  FfmpegOwnedMuxer::open(
    File::create(target).expect("create"),
    container,
    table,
    &[],
    PacketLimits::default(),
  )
  .expect("open")
  .0
}

/// A muxer for `container` into `target`, over `table`, as `options` say.
fn muxer_with(
  target: &Path,
  container: ContainerName,
  table: Vec<TrackDescription<Ffmpeg, FfmpegBytes, CodecTicket>>,
  options: MuxOptions,
) -> FfmpegOwnedMuxer<File> {
  FfmpegOwnedMuxer::open_with(
    File::create(target).expect("create"),
    container,
    table,
    &[],
    options,
  )
  .expect("open")
  .0
}

/// The options with the automatic filters `on` or off.
fn filters(on: bool) -> MuxOptions {
  MuxOptions::new().with_automatic_filters(on)
}

/// The container's own refusal and the packet handed back, or a panic
/// naming what came instead.
fn by_container<P: core::fmt::Debug>(
  result: Result<(), MuxError<mediaencode_ffmpeg::MuxFault, FfmpegMuxRefusal, P>>,
) -> (FfmpegMuxRefusal, P) {
  match result {
    Err(MuxError::Backend { refusal, packet }) => (refusal, packet),
    other => panic!("expected the container's refusal, got {other:?}"),
  }
}

/// A payload rule's refusal on track 0.
fn payload(rule: PayloadRule) -> FfmpegMuxRefusal {
  FfmpegMuxRefusal::Payload {
    track: TrackIndex::new(0),
    rule,
  }
}

/// The packet handed back is the one offered: its track, its very bytes,
/// its times and flags.
macro_rules! untouched {
  ($offered:expr, $back:expr) => {{
    let (offered, back) = (&$offered, &$back);
    assert_eq!(offered.track(), back.track());
    assert!(
      back.packet().data().ptr_eq(offered.packet().data()),
      "the very bytes it was offered with"
    );
    assert_eq!(offered.packet().pts(), back.packet().pts());
    assert_eq!(offered.packet().duration(), back.packet().duration());
    assert_eq!(offered.packet().flags(), back.packet().flags());
  }};
}

/// Every packet `path` holds, across its tracks.
fn packet_count(path: &Path) -> usize {
  packets(path).iter().map(Vec::len).sum()
}

/// A time of 2^31 seconds: past `i32::MAX` ticks in any time base of a
/// tick a second or finer.
const PAST_I32_SECONDS: i64 = 1 << 31;

/// **L11 — a negative duration is written as libavformat makes it, and
/// reported; a subtitle's, which libavformat hands the container as it is,
/// is refused by name outside Matroska and WebM** (table G). libavformat
/// writes zero, or its own guess, in place of a video or audio packet's
/// (`guess_pkt_duration`, `mux.c` 605–633), and Matroska zero for a
/// subtitle's (`matroskaenc.c` 3134), the cue read back with none: taken,
/// the durations reported. MP4 refuses a subtitle's after taking the packet
/// (`movenc.c` 6925–6928): refused, untouched, the file holding only what
/// was admitted.
///
/// PLANT: the refusal on every track refuses the video packet; on none, MP4
/// refuses the cue after taking it.
#[test]
fn l11_a_negative_duration_is_written_as_libavformat_makes_it() {
  let Some(corpus) = Corpus::new() else { return };
  let source = corpus.multi_track_mkv();
  let (_, frames) = first_track(&source, TrackKind::Video, 2);
  for (container, name) in [
    (ContainerName::MP4, "l11.mp4"),
    (ContainerName::MATROSKA, "l11.mkv"),
  ] {
    let target = output(&corpus, name);
    let mut muxer = muxer_into(&target, container, table_of(&source, TrackKind::Video));
    muxer
      .write_video(video_at(&video_of(&frames[0]), Some(0), 0))
      .expect("the first packet");
    assert_eq!(
      durations_of(muxer.report()),
      0,
      "{name}: the premise, nothing rewritten"
    );
    muxer
      .write_video(video_at(
        &video_of(&frames[1]).with_duration(Some(ms(-40))),
        Some(40),
        40,
      ))
      .map_err(|error| error.map_packet(drop))
      .expect("written with libavformat's duration");
    assert_eq!(durations_of(muxer.report()), 1, "{name}: reported");
    muxer.finish().expect("finished");
    assert_eq!(packet_count(&target), 2, "{name}");
  }

  let (cue_table, cues) = first_track(&source, TrackKind::Subtitle, 1);
  let cue = subtitle_of(&cues[0]);
  let target = output(&corpus, "l11-cue.mkv");
  let mut muxer = muxer_into(&target, ContainerName::MATROSKA, cue_table);
  assert_eq!(durations_of(muxer.report()), 0, "the premise");
  muxer
    .write_subtitle(subtitle_at(&cue, 0, -40))
    .map_err(|error| error.map_packet(drop))
    .expect("Matroska writes zero");
  assert_eq!(durations_of(muxer.report()), 1, "reported");
  muxer.finish().expect("finished");
  let seen = &packets(&target)[0];
  assert_eq!(seen.len(), 1);
  assert_eq!(seen[0].pts, Some(ms(0)));
  assert_eq!(
    seen[0].duration, None,
    "written as zero, which Matroska writes no duration for (`matroskaenc.c` 2896)"
  );

  let mov_text = mov_text_mp4(&corpus);
  let (mov_table, mov_cues) = first_track(&mov_text, TrackKind::Subtitle, 1);
  let base = mov_table[0].timebase();
  for (name, container, table, cue) in [(
    "l11-cue.mp4",
    ContainerName::MP4,
    mov_table,
    cue_on(0, &subtitle_of(&mov_cues[0]), base, (0, -1)),
  )] {
    let target = output(&corpus, name);
    let mut muxer = muxer_into(&target, container, table);
    let duration = cue.packet().duration().expect("a duration");
    let (refusal, back) = by_container(muxer.write_subtitle(cue.clone()));
    assert_eq!(
      refusal,
      FfmpegMuxRefusal::NegativeDuration {
        track: TrackIndex::new(0),
        duration,
      },
      "{name}"
    );
    untouched!(cue, back);
    muxer.finish().expect("finished");
    // MP4's trailer writes the empty sample of a `mov_text` track that took
    // no cue (`movenc.c` 9025–9031). The cue's bytes are nowhere.
    let empty = std::fs::metadata(&target).expect("the file").len() == 0;
    assert!(
      empty
        || packets(&target)
          .iter()
          .flatten()
          .all(|seen| seen.bytes != cue.packet().data().as_ref()),
      "{name}: only what was admitted"
    );
  }
}

/// **L12 — MP4 refuses by name a duration past `i32::MAX` ticks, a decode
/// time that far after the last, and a presentation that far after its
/// decode.** It would refuse the first after taking the packet, rewrite
/// the second to one tick after the last with no presentation time, and
/// overflow its 32-bit composition offset on the third. The gap is judged
/// in `i128`: from a decode time far below zero to one far above, past
/// what an `i64` holds, it is refused, not overflowed.
#[test]
fn l12_mp4_refuses_times_past_its_32_bit_fields() {
  let Some(corpus) = Corpus::new() else { return };
  let source = corpus.multi_track_mkv();
  let (table, packets) = first_track(&source, TrackKind::Video, 2);
  let track = TrackIndex::new(0);
  let past = ms(PAST_I32_SECONDS * 1000);
  let target = output(&corpus, "l12.mp4");
  let mut muxer = muxer_into(
    &target,
    ContainerName::MP4,
    table_of(&source, TrackKind::Video),
  );
  muxer
    .write_video(video_at(&video_of(&packets[0]), Some(0), 0))
    .expect("the first packet");

  let long = video_at(
    &video_of(&packets[1]).with_duration(Some(past)),
    Some(40),
    40,
  );
  let (refusal, back) = by_container(muxer.write_video(long.clone()));
  assert_eq!(
    refusal,
    FfmpegMuxRefusal::DurationTooLong {
      track,
      duration: past
    }
  );
  untouched!(long, back);

  let gap = video_at(&video_of(&packets[1]), Some(past.pts()), past.pts());
  let (refusal, back) = by_container(muxer.write_video(gap.clone()));
  assert_eq!(refusal, FfmpegMuxRefusal::DtsGap { track, dts: past });
  untouched!(gap, back);

  let late = video_at(&video_of(&packets[1]), Some(past.pts()), 40);
  let (refusal, back) = by_container(muxer.write_video(late.clone()));
  assert_eq!(
    refusal,
    FfmpegMuxRefusal::PresentationOffset {
      track,
      pts: past,
      dts: ms(40),
    }
  );
  untouched!(late, back);
  muxer.finish().expect("finished");
  assert_eq!(packet_count(&target), 1, "only what was admitted");

  // MP4 takes negative times as they are; a gap from far below zero to far
  // above it is past an `i64` of ticks, and judged without overflow.
  let target = output(&corpus, "l12-wide.mp4");
  let mut muxer = muxer_into(&target, ContainerName::MP4, table);
  let far = 300_000_000_000_000_000;
  muxer
    .write_video(video_at(&video_of(&packets[0]), Some(-far), -far))
    .expect("a time far below zero");
  let across = video_at(&video_of(&packets[1]), Some(far), far);
  let (refusal, _) = by_container(muxer.write_video(across));
  assert_eq!(
    refusal,
    FfmpegMuxRefusal::DtsGap {
      track,
      dts: ms(far)
    }
  );
}

/// **L13 — a stream that reorders has no presentation time made up for
/// it: a packet without one is refused by name** in Matroska, which
/// refuses it after taking it, and in MP4, which presents it at its
/// decode time. In Matroska its decode time before zero changes nothing:
/// libavformat's move of early times judges the presentation time, and
/// judges no packet without one (`mux.c` 650–651). A stream that does not
/// reorder is presented at its decode time by libavformat, and admitted.
#[test]
fn l13_a_reordering_stream_needs_a_presentation_time() {
  let Some(corpus) = Corpus::new() else { return };
  let reordering = reordering_mkv(&corpus);
  let (_, packets) = first_track(&reordering, TrackKind::Video, 2);
  for (container, name) in [
    (ContainerName::MATROSKA, "l13.mkv"),
    (ContainerName::MP4, "l13.mp4"),
  ] {
    let target = output(&corpus, name);
    let mut muxer = muxer_into(&target, container, table_of(&reordering, TrackKind::Video));
    muxer
      .write_video(video_at(&video_of(&packets[0]), Some(80), 0))
      .expect("the first packet");
    let offered = video_at(&video_of(&packets[1]), None, 40);
    let (refusal, back) = by_container(muxer.write_video(offered.clone()));
    assert_eq!(
      refusal,
      FfmpegMuxRefusal::MissingPts {
        track: TrackIndex::new(0)
      },
      "{name}"
    );
    untouched!(offered, back);
    muxer.finish().expect("finished");
  }
  let mut muxer = muxer_into(
    &output(&corpus, "l13-early.mkv"),
    ContainerName::MATROSKA,
    table_of(&reordering, TrackKind::Video),
  );
  muxer
    .write_video(video_at(&video_of(&packets[0]), Some(0), -80))
    .expect("the first packet");
  let offered = video_at(&video_of(&packets[1]), None, -60);
  let (refusal, back) = by_container(muxer.write_video(offered.clone()));
  assert_eq!(
    refusal,
    FfmpegMuxRefusal::MissingPts {
      track: TrackIndex::new(0)
    },
    "a decode time before zero"
  );
  untouched!(offered, back);
  muxer.finish().expect("finished");
  let (table, packets) = first_track(&corpus.multi_track_mkv(), TrackKind::Video, 1);
  let mut muxer = muxer_into(
    &output(&corpus, "l13-plain.mkv"),
    ContainerName::MATROSKA,
    table,
  );
  muxer
    .write_video(video_at(&video_of(&packets[0]), None, 0))
    .expect("a stream that does not reorder is presented at its decode time");
  muxer.finish().expect("finished");
}

/// **L14 — a time before the earliest the container writes is written as
/// libavformat moves it, every stream's times with it, and reported; one
/// that libavformat's first write leaves before the earliest is refused by
/// name** (table G). At its first write libavformat moves every stream's
/// times later by the earliest distance before its stream's earliest of the
/// packet it writes or of one its interleaving holds then
/// (`handle_avoid_negative_ts`, `mux.c` 636–717): in Matroska a
/// presentation time below zero, or below an audio track's codec delay
/// before it; in FLV a decode time below zero. Alone, a track's first
/// packet is that write, and a reordering stream's later packet the move
/// leaves before zero is refused, untouched. Beside another stream, the
/// write waits until both hold a packet, so an early packet of the second
/// moves the first's times too; or until one's packets span more than
/// libavformat's 10 s of interleaving, after which the other's early packet
/// is refused. Read back: every time moved, each stream's distance rounded
/// up to its own tick.
///
/// PLANT: the move unmirrored — every time before the earliest refused, as
/// round 2 refused it — refuses the early first packets; the packets after
/// the first write unjudged takes the late ones.
#[test]
fn l14_a_time_before_the_earliest_written_is_moved_as_libavformat_moves_it() {
  let Some(corpus) = Corpus::new() else { return };
  let shifted = |muxer: &FfmpegOwnedMuxer<File>| {
    muxer
      .report()
      .uncarried()
      .contains(&Uncarried::TimesShifted)
  };
  let source = corpus.multi_track_mkv();
  let (_, frames) = first_track(&source, TrackKind::Video, 2);
  let (_, audio) = first_track(&source, TrackKind::Audio, 2);
  let audio_on = |track: usize, packet: &Audio, at: i64| {
    AudioTrackPacket::new(
      TrackIndex::new(track),
      packet.clone().with_pts(Some(ms(at))).with_dts(Some(ms(at))),
    )
  };
  let presented = |path: &Path, track: usize| -> Vec<Option<Timestamp>> {
    packets(path)[track].iter().map(|seen| seen.pts).collect()
  };

  // Alone, the first packet is libavformat's first write.
  for (container, name) in [
    (ContainerName::MATROSKA, "l14.mkv"),
    (ContainerName::new("flv"), "l14.flv"),
  ] {
    let target = output(&corpus, name);
    let mut muxer = muxer_into(&target, container, table_of(&source, TrackKind::Video));
    muxer
      .write_video(video_at(&video_of(&frames[0]), Some(-40), -40))
      .map_err(|error| error.map_packet(drop))
      .expect("before zero: every stream moved");
    assert!(shifted(&muxer), "{name}");
    muxer
      .write_video(video_at(&video_of(&frames[1]), Some(0), 0))
      .map_err(|error| error.map_packet(drop))
      .expect("the next");
    muxer.finish().expect("finished");
    assert_eq!(
      presented(&target, 0),
      vec![Some(ms(0)), Some(ms(40))],
      "{name}: moved by 40 ms"
    );
  }

  // AAC's encoder delay of 1024 samples at 48 kHz: 21 ms before zero, the
  // earliest Matroska writes; a packet at −22 moves the track by a tick.
  let target = output(&corpus, "l14-audio.mkv");
  let mut muxer = muxer_into(
    &target,
    ContainerName::MATROSKA,
    table_of(&source, TrackKind::Audio),
  );
  muxer
    .write_audio(audio_at(&audio_of(&audio[0]), -22))
    .map_err(|error| error.map_packet(drop))
    .expect("below the codec delay: moved");
  assert!(shifted(&muxer));
  muxer
    .write_audio(audio_at(&audio_of(&audio[1]), 0))
    .map_err(|error| error.map_packet(drop))
    .expect("the next");
  muxer.finish().expect("finished");
  assert_eq!(presented(&target, 0), vec![Some(ms(-21)), Some(ms(1))]);

  // A reordering stream alone: its first packet settles the move, and a
  // later presentation time it leaves before zero is refused.
  let reordering = reordering_mkv(&corpus);
  let (_, bframes) = first_track(&reordering, TrackKind::Video, 2);
  let mut muxer = muxer_into(
    &output(&corpus, "l14-reordering.mkv"),
    ContainerName::MATROSKA,
    table_of(&reordering, TrackKind::Video),
  );
  muxer
    .write_video(video_at(&video_of(&bframes[0]), Some(-40), -80))
    .map_err(|error| error.map_packet(drop))
    .expect("moved by 40 ms");
  let late = video_at(&video_of(&bframes[1]), Some(-50), -60);
  let (refusal, back) = by_container(muxer.write_video(late.clone()));
  assert_eq!(
    refusal,
    FfmpegMuxRefusal::BeforeZero {
      track: TrackIndex::new(0),
      timestamp: ms(-50)
    }
  );
  untouched!(late, back);
  muxer
    .write_video(video_at(&video_of(&bframes[1]), Some(0), -40))
    .map_err(|error| error.map_packet(drop))
    .expect("moved past zero");
  muxer.finish().expect("finished");

  // Beside audio: the write waits until both hold a packet, so the early
  // audio packet moves the video too, by 9 ms.
  let mut table = table_of(&source, TrackKind::Video);
  table.extend(table_of(&source, TrackKind::Audio));
  let target = output(&corpus, "l14-both.mkv");
  let mut muxer = muxer_into(&target, ContainerName::MATROSKA, table);
  muxer
    .write_video(video_at(&video_of(&frames[0]), Some(0), 0))
    .map_err(|error| error.map_packet(drop))
    .expect("video");
  assert!(!shifted(&muxer), "nothing early yet");
  muxer
    .write_audio(audio_on(1, &audio_of(&audio[0]), -30))
    .map_err(|error| error.map_packet(drop))
    .expect("9 ms below the codec delay, before the first write");
  assert!(shifted(&muxer));
  muxer.finish().expect("finished");
  assert_eq!(presented(&target, 0), vec![Some(ms(9))], "the video moved");
  assert_eq!(
    presented(&target, 1),
    vec![Some(ms(-21))],
    "the audio moved"
  );

  // Beside audio that holds none, the write comes once the video spans more
  // than 10 s; the audio's early packet after it is refused.
  let mut table = table_of(&source, TrackKind::Video);
  table.extend(table_of(&source, TrackKind::Audio));
  let mut muxer = muxer_into(
    &output(&corpus, "l14-late.mkv"),
    ContainerName::MATROSKA,
    table,
  );
  for (frame, at) in frames.iter().zip([0, 10_001]) {
    muxer
      .write_video(video_at(&video_of(frame), Some(at), at))
      .map_err(|error| error.map_packet(drop))
      .expect("video");
  }
  let late = audio_on(1, &audio_of(&audio[0]), -22);
  let (refusal, back) = by_container(muxer.write_audio(late.clone()));
  assert_eq!(
    refusal,
    FfmpegMuxRefusal::BeforeZero {
      track: TrackIndex::new(1),
      timestamp: ms(-22)
    }
  );
  untouched!(late, back);
  muxer
    .write_audio(audio_on(1, &audio_of(&audio[0]), -21))
    .map_err(|error| error.map_packet(drop))
    .expect("within the codec delay");
  assert!(!shifted(&muxer), "nothing moved");
  muxer.finish().expect("finished");
}

/// **L15 — beside video counted in finer ticks, MP4 refuses by name a
/// `mov_text` cue before zero, or before the last cue ends** (table C″):
/// a video packet whose decode time, in its own ticks, passes the cue
/// track's end makes MP4 mark that end first, and MP4 then moves the cue
/// past the mark (`movenc.c` 7646–7653, 6917–6923). A cue where the last
/// one ends is written. Alone, MP4 writes both (L65).
#[test]
fn l15_mp4_refuses_a_cue_out_of_order() {
  let Some(corpus) = Corpus::new() else { return };
  let video_source = corpus.multi_track_mkv();
  let (_, frames) = first_track(&video_source, TrackKind::Video, 1);
  let source = mov_text_ms_mp4(&corpus);
  let (_, cues) = first_track(&source, TrackKind::Subtitle, 1);
  let cue = subtitle_of(&cues[0]);
  let track = TrackIndex::new(1);
  let base = millis();
  let table = || {
    let mut table = table_of(&video_source, TrackKind::Video);
    table.extend(table_of(&source, TrackKind::Subtitle));
    table
  };
  let mut muxer = muxer_into(&output(&corpus, "l15.mp4"), ContainerName::MP4, table());
  muxer
    .write_video(video_at(&video_of(&frames[0]), Some(0), 0))
    .map_err(|error| error.map_packet(drop))
    .expect("video");
  muxer
    .write_subtitle(cue_on(1, &cue, base, (0, 1000)))
    .map_err(|error| error.map_packet(drop))
    .expect("the first cue");
  let early = cue_on(1, &cue, base, (500, 100));
  let (refusal, back) = by_container(muxer.write_subtitle(early.clone()));
  assert_eq!(
    refusal,
    FfmpegMuxRefusal::CueOrder {
      track,
      pts: ms(500)
    }
  );
  untouched!(early, back);
  muxer
    .write_subtitle(cue_on(1, &cue, base, (1000, 500)))
    .map_err(|error| error.map_packet(drop))
    .expect("a cue where the last one ends");
  muxer.finish().expect("finished");

  let mut muxer = muxer_into(
    &output(&corpus, "l15-early.mp4"),
    ContainerName::MP4,
    table(),
  );
  let before = cue_on(1, &cue, base, (-10, 100));
  let (refusal, back) = by_container(muxer.write_subtitle(before.clone()));
  assert_eq!(
    refusal,
    FfmpegMuxRefusal::CueOrder {
      track,
      pts: ms(-10)
    }
  );
  untouched!(before, back);
}

/// **L16 — a packet libavformat marks a key frame is written as one, and
/// reported** (table G): libavformat marks every packet of a codec whose
/// descriptor counts every frame a key frame — libavcodec's
/// `AV_CODEC_PROP_INTRA_ONLY`, PCM's and MP3's among them (`codec_desc.c`
/// 1998–2002, 2792–2796) — or of any subtitle codec (`ff_is_intra_only`,
/// `avformat.c` 903–912; `mux.c` 820–821): the library's own claim. An
/// unmarked such packet is taken into MP4 and Matroska, reads back a key
/// frame, and the report says so; Matroska writes every subtitle block as
/// one, so an unmarked cue is reported too. A codec without the property
/// is L71's.
///
/// PLANT: the mark unreported leaves the line out.
#[test]
fn l16_a_key_frame_libavformat_marks_is_written_and_reported() {
  let Some(corpus) = Corpus::new() else { return };
  for (source, at, codec) in [
    (pcm_mov(&corpus), 10, "pcm"),
    (audio_file(&corpus, "l16.mp3", "libmp3lame"), 24, "mp3"),
  ] {
    let (_, frames) = first_track(&source, TrackKind::Audio, 2);
    for (container, name) in [
      (ContainerName::MP4, format!("l16-{codec}.mp4")),
      (ContainerName::MATROSKA, format!("l16-{codec}.mkv")),
    ] {
      let target = output(&corpus, &name);
      let mut muxer = muxer_into(&target, container, table_of(&source, TrackKind::Audio));
      muxer
        .write_audio(audio_at(&audio_of(&frames[0]), 0))
        .map_err(|error| error.map_packet(drop))
        .expect("a key frame");
      assert_eq!(marked_of(muxer.report()), 0, "{name}: the premise");
      muxer
        .write_audio(AudioTrackPacket::new(
          TrackIndex::new(0),
          audio_of(&frames[1])
            .with_pts(Some(ms(at)))
            .with_dts(Some(ms(at)))
            .with_flags(PacketFlags::empty()),
        ))
        .map_err(|error| error.map_packet(drop))
        .expect("marked by libavformat");
      assert_eq!(marked_of(muxer.report()), 1, "{name}: reported");
      muxer.finish().expect("finished");
      let keys: Vec<_> = packets(&target)[0].iter().map(|seen| seen.key).collect();
      assert_eq!(keys, vec![true, true], "{name}: read back a key frame");
    }
  }
  let (table, cues) = first_track(&corpus.multi_track_mkv(), TrackKind::Subtitle, 1);
  let mut muxer = muxer_into(
    &output(&corpus, "l16-cue.mkv"),
    ContainerName::MATROSKA,
    table,
  );
  muxer
    .write_subtitle(SubtitleTrackPacket::new(
      TrackIndex::new(0),
      subtitle_of(&cues[0])
        .with_pts(Some(ms(0)))
        .with_flags(PacketFlags::empty()),
    ))
    .map_err(|error| error.map_packet(drop))
    .expect("Matroska writes a subtitle as a key frame regardless");
  assert_eq!(marked_of(muxer.report()), 1, "reported");
  muxer.finish().expect("finished");
}

/// **L17 — an empty packet a container drops is refused by name**: any in
/// MP4, an audio packet in Matroska. Matroska writes an empty video
/// packet as a block of its own.
#[test]
fn l17_an_empty_packet_a_container_drops_is_refused() {
  let Some(corpus) = Corpus::new() else { return };
  let source = corpus.multi_track_mkv();
  let (video_table, video) = first_track(&source, TrackKind::Video, 2);
  let mut muxer = muxer_into(&output(&corpus, "l17.mp4"), ContainerName::MP4, video_table);
  muxer
    .write_video(video_at(&video_of(&video[0]), Some(0), 0))
    .expect("the first packet");
  let empty = video_at(&video_holding(&video_of(&video[1]), &[]), Some(40), 40);
  let (refusal, back) = by_container(muxer.write_video(empty.clone()));
  assert_eq!(refusal, payload(PayloadRule::Empty));
  untouched!(empty, back);
  muxer.finish().expect("finished");

  let (audio_table, audio) = first_track(&source, TrackKind::Audio, 1);
  let mut muxer = muxer_into(
    &output(&corpus, "l17.mkv"),
    ContainerName::MATROSKA,
    audio_table,
  );
  let empty = audio_at(&audio_holding(&audio_of(&audio[0]), &[]), 0);
  let (refusal, back) = by_container(muxer.write_audio(empty.clone()));
  assert_eq!(refusal, payload(PayloadRule::Empty));
  untouched!(empty, back);
  muxer.finish().expect("finished");

  let target = output(&corpus, "l17-video.mkv");
  let mut muxer = muxer_into(
    &target,
    ContainerName::MATROSKA,
    table_of(&source, TrackKind::Video),
  );
  muxer
    .write_video(video_at(&video_of(&video[0]), Some(0), 0))
    .expect("the first packet");
  muxer
    .write_video(video_at(
      &video_holding(&video_of(&video[1]), &[]),
      Some(40),
      40,
    ))
    .expect("Matroska writes an empty video packet");
  muxer.finish().expect("finished");
}

/// **L18 — AAC in an ADTS header is refused by name on a track whose
/// configuration is stated where no filter converts it**, into MP4, which
/// would refuse the first such packet after taking it and write the rest
/// with their headers inside (`movenc.c` 7076–7086), and into Matroska,
/// which would write them into a track that declares raw AAC: with the
/// automatic filters off, and with them on after a raw first packet, which
/// inserts none (`check_bitstream`, `mux.c` 1063–1068). An ADTS first
/// packet inserts the filter (L67).
#[test]
fn l18_aac_in_an_adts_header_is_refused_on_a_configured_track() {
  let Some(corpus) = Corpus::new() else { return };
  let source = corpus.multi_track_mkv();
  let (_, packets) = first_track(&source, TrackKind::Audio, 1);
  let raw = audio_of(&packets[0]);
  let mut framed = vec![0xFF, 0xF1, 0x4C, 0x80, 0x00, 0x00, 0xFC];
  framed.extend_from_slice(raw.data().as_ref());
  for (container, name) in [
    (ContainerName::MP4, "l18.mp4"),
    (ContainerName::MATROSKA, "l18.mkv"),
  ] {
    for on in [true, false] {
      let mut muxer = muxer_with(
        &output(&corpus, name),
        container.clone(),
        table_of(&source, TrackKind::Audio),
        filters(on),
      );
      if on {
        muxer
          .write_audio(audio_at(&raw, 0))
          .map_err(|error| error.map_packet(drop))
          .expect("a raw first packet, which inserts no filter");
      }
      let offered = audio_at(&audio_holding(&raw, &framed), 21);
      let (refusal, back) = by_container(muxer.write_audio(offered.clone()));
      assert_eq!(
        refusal,
        payload(PayloadRule::AdtsHeader),
        "{name}, the filters on: {on}"
      );
      untouched!(offered, back);
    }
  }
}

/// **L19 — MP4 refuses PCM that is not a whole number of frames**, whose
/// rest it would not count.
#[test]
fn l19_mp4_refuses_a_partial_pcm_frame() {
  let Some(corpus) = Corpus::new() else { return };
  let (table, packets) = first_track(&pcm_mov(&corpus), TrackKind::Audio, 1);
  let mut muxer = muxer_into(&output(&corpus, "l19.mp4"), ContainerName::MP4, table);
  let whole = audio_of(&packets[0]);
  let mut bytes = whole.data().as_ref().to_vec();
  bytes.push(0);
  let offered = audio_at(&audio_holding(&whole, &bytes), 0);
  let (refusal, back) = by_container(muxer.write_audio(offered.clone()));
  assert_eq!(refusal, payload(PayloadRule::PartialSample));
  untouched!(offered, back);
  muxer
    .write_audio(audio_at(&whole, 0))
    .expect("whole frames");
  muxer.finish().expect("finished");
}

/// **L20 — AV1 whose OBUs do not parse is refused by name** in MP4,
/// which would write a sample of a negative size, and in Matroska, which
/// refuses it after taking it.
#[test]
fn l20_av1_whose_obus_do_not_parse_is_refused() {
  let Some(corpus) = Corpus::new() else { return };
  let av1 = av1_mkv(&corpus);
  let (_, packets) = first_track(&av1, TrackKind::Video, 1);
  for (container, name) in [
    (ContainerName::MP4, "l20.mp4"),
    (ContainerName::MATROSKA, "l20.mkv"),
  ] {
    let mut muxer = muxer_into(
      &output(&corpus, name),
      container,
      table_of(&av1, TrackKind::Video),
    );
    let packet = video_of(&packets[0]);
    let mut bytes = packet.data().as_ref().to_vec();
    bytes[0] |= 0x80;
    let offered = video_at(&video_holding(&packet, &bytes), Some(0), 0);
    let (refusal, back) = by_container(muxer.write_video(offered.clone()));
    assert_eq!(refusal, payload(PayloadRule::Obus), "{name}");
    untouched!(offered, back);
    muxer
      .write_video(video_at(&packet, Some(0), 0))
      .expect("OBUs that parse");
    muxer.finish().expect("finished");
  }
}

/// **L21 — Matroska refuses WavPack blocks that do not parse whole**: one
/// cut short, which it refuses after taking it, and one with bytes left
/// over, which it would drop.
#[test]
fn l21_matroska_refuses_wavpack_that_does_not_parse_whole() {
  let Some(corpus) = Corpus::new() else { return };
  let (table, packets) = first_track(
    &audio_file(&corpus, "wavpack.mkv", "wavpack"),
    TrackKind::Audio,
    1,
  );
  let mut muxer = muxer_into(&output(&corpus, "l21.mkv"), ContainerName::MATROSKA, table);
  let packet = audio_of(&packets[0]);
  let whole = packet.data().as_ref().to_vec();
  let mut over = whole.clone();
  over.extend([1, 2, 3]);
  for bytes in [&whole[..whole.len() - 1], &over[..]] {
    let offered = audio_at(&audio_holding(&packet, bytes), 0);
    let (refusal, back) = by_container(muxer.write_audio(offered.clone()));
    assert_eq!(refusal, payload(PayloadRule::WavPackBlocks));
    untouched!(offered, back);
  }
  muxer
    .write_audio(audio_at(&packet, 0))
    .expect("whole blocks");
  muxer.finish().expect("finished");
}

/// **L22 — side data a container would refuse is refused by name**: MP4's
/// skip-samples that shorten a packet below nothing, and Matroska's new
/// FLAC STREAMINFO of another length than the track's. A STREAMINFO of
/// the track's length is written.
#[test]
fn l22_side_data_a_container_refuses_is_refused_by_name() {
  let Some(corpus) = Corpus::new() else { return };
  let (table, packets) = first_track(&corpus.multi_track_mkv(), TrackKind::Audio, 1);
  let mut muxer = muxer_into(&output(&corpus, "l22.mp4"), ContainerName::MP4, table);
  let mut skip = 0u32.to_le_bytes().to_vec();
  skip.extend(4096u32.to_le_bytes());
  skip.extend([0, 0]);
  let mut skipping = audio_of(&packets[0])
    .with_pts(Some(ms(0)))
    .with_dts(Some(ms(0)));
  skipping.extra_mut().set_side_data(vec![side(
    ffmpeg_next::ffi::AVPacketSideDataType::AV_PKT_DATA_SKIP_SAMPLES,
    &skip,
  )]);
  let offered = AudioTrackPacket::new(TrackIndex::new(0), skipping);
  let (refusal, back) = by_container(muxer.write_audio(offered.clone()));
  assert_eq!(refusal, payload(PayloadRule::SkipSamples));
  untouched!(offered, back);

  let (table, packets) = first_track(
    &audio_file(&corpus, "flac.mkv", "flac"),
    TrackKind::Audio,
    2,
  );
  let streaminfo = table[0]
    .extradata()
    .map(|bytes| bytes.as_ref().to_vec())
    .unwrap_or_else(|| table[0].side_data().extradata().to_vec());
  let mut muxer = muxer_into(&output(&corpus, "l22.mkv"), ContainerName::MATROSKA, table);
  let with_extradata = |packet: &Audio, at: i64, bytes: &[u8]| {
    let mut packet = packet.clone().with_pts(Some(ms(at))).with_dts(Some(ms(at)));
    packet.extra_mut().set_side_data(vec![side(
      ffmpeg_next::ffi::AVPacketSideDataType::AV_PKT_DATA_NEW_EXTRADATA,
      bytes,
    )]);
    AudioTrackPacket::new(TrackIndex::new(0), packet)
  };
  let offered = with_extradata(&audio_of(&packets[0]), 0, &[1; 10]);
  let (refusal, back) = by_container(muxer.write_audio(offered.clone()));
  assert_eq!(refusal, payload(PayloadRule::FlacStreamInfo));
  untouched!(offered, back);
  muxer
    .write_audio(with_extradata(&audio_of(&packets[0]), 0, &streaminfo))
    .expect("a STREAMINFO of the track's length");
  muxer.finish().expect("finished");
}

/// **L23 — Matroska refuses by name a ProRes packet that does not open on
/// its own frame atom**, which it would drop and its reader put back
/// otherwise. One that does is written and reads back as itself.
#[test]
fn l23_matroska_refuses_prores_off_its_atom() {
  let Some(corpus) = Corpus::new() else { return };
  let (table, frames) = first_track(&prores_mov(&corpus), TrackKind::Video, 2);
  let target = output(&corpus, "l23.mkv");
  let mut muxer = muxer_into(&target, ContainerName::MATROSKA, table);
  let packet = video_of(&frames[0]);
  let mut bytes = packet.data().as_ref().to_vec();
  assert_eq!(&bytes[4..8], b"icpf", "the premise: a ProRes frame's atom");
  bytes[4] = b'x';
  let offered = video_at(&video_holding(&packet, &bytes), Some(0), 0);
  let (refusal, back) = by_container(muxer.write_video(offered.clone()));
  assert_eq!(refusal, payload(PayloadRule::ProResAtom));
  untouched!(offered, back);
  for (index, packet) in frames.iter().enumerate() {
    let at = 40 * index as i64;
    muxer
      .write_video(video_at(&video_of(packet), Some(at), at))
      .expect("a frame on its atom");
  }
  muxer.finish().expect("finished");
  let written = &packets(&target)[0];
  for (index, packet) in frames.iter().enumerate() {
    assert_eq!(
      written[index].bytes,
      video_of(packet).data().as_ref(),
      "read back as itself"
    );
  }
}

/// **L24 — Matroska and WebM refuse by name a WebVTT cue that would not
/// read back from its lines**, here one whose text ends in a line break.
/// A cue that would is written and reads back as itself.
#[test]
fn l24_a_webvtt_cue_that_would_not_read_back_is_refused() {
  let Some(corpus) = Corpus::new() else { return };
  let source = webvtt_mkv(&corpus);
  let (_, cues) = first_track(&source, TrackKind::Subtitle, 1);
  for (container, name) in [
    (ContainerName::MATROSKA, "l24.mkv"),
    (ContainerName::WEBM, "l24.webm"),
  ] {
    let target = output(&corpus, name);
    let mut muxer = muxer_into(&target, container, table_of(&source, TrackKind::Subtitle));
    let cue = subtitle_of(&cues[0]);
    let mut text = cue.data().as_ref().to_vec();
    assert!(!text.ends_with(b"\n"), "the premise: a cue's text");
    text.push(b'\n');
    let offered = subtitle_at(&subtitle_holding(&cue, &text), 0, 1000);
    let (refusal, back) = by_container(muxer.write_subtitle(offered.clone()));
    assert_eq!(refusal, payload(PayloadRule::WebVttCue), "{name}");
    untouched!(offered, back);
    muxer
      .write_subtitle(subtitle_at(&cue, 0, 1000))
      .expect("a cue that reads back");
    muxer.finish().expect("finished");
    let written = &packets(&target)[0];
    assert_eq!(written.len(), 1);
    assert_eq!(
      written[0].bytes,
      cue.data().as_ref(),
      "{name}: read back as itself"
    );
  }
}

/// **L25 — MP4 refuses by name AC-3 and E-AC-3 packets its handling would
/// drop or refuse**: an AC-3 packet whose header does not parse; an E-AC-3
/// track's first packet with bytes after its frame that are no dependent
/// frame; and an E-AC-3 frame whose six blocks would follow three held
/// back — the first packet's own, recoded as three — past a sample of six.
#[test]
fn l25_mp4_refuses_ac3_it_would_drop_or_refuse() {
  let Some(corpus) = Corpus::new() else { return };
  let (table, packets) = first_track(&audio_file(&corpus, "ac3.mkv", "ac3"), TrackKind::Audio, 1);
  let mut muxer = muxer_into(&output(&corpus, "l25.mp4"), ContainerName::MP4, table);
  let packet = audio_of(&packets[0]);
  let mut bytes = packet.data().as_ref().to_vec();
  bytes[0] = 0;
  let offered = audio_at(&audio_holding(&packet, &bytes), 0);
  let (refusal, back) = by_container(muxer.write_audio(offered.clone()));
  assert_eq!(refusal, payload(PayloadRule::Ac3Header));
  untouched!(offered, back);
  muxer
    .write_audio(audio_at(&packet, 0))
    .expect("an AC-3 frame");
  muxer.finish().expect("finished");

  let eac3 = audio_file(&corpus, "eac3.mkv", "eac3");
  let (table, packets) = first_track(&eac3, TrackKind::Audio, 2);
  let first = audio_of(&packets[0]);
  let mut tail = first.data().as_ref().to_vec();
  tail.extend([1, 2, 3]);
  let mut muxer = muxer_into(&output(&corpus, "l25-eac3.mp4"), ContainerName::MP4, table);
  let offered = audio_at(&audio_holding(&first, &tail), 0);
  let (refusal, back) = by_container(muxer.write_audio(offered.clone()));
  assert_eq!(refusal, payload(PayloadRule::Ac3Substreams));
  untouched!(offered, back);

  // The first frame recoded as three blocks: `numblkscod`, bits 4–5 of
  // its fifth byte, at 48 kHz.
  let mut three = first.data().as_ref().to_vec();
  assert_eq!(three[4] >> 6, 0, "the premise: 48 kHz");
  assert_eq!((three[4] >> 4) & 3, 3, "the premise: six blocks");
  three[4] = (three[4] & !0x30) | 0x20;
  let mut muxer = muxer_into(
    &output(&corpus, "l25-blocks.mp4"),
    ContainerName::MP4,
    table_of(&eac3, TrackKind::Audio),
  );
  muxer
    .write_audio(audio_at(&audio_holding(&first, &three), 0))
    .expect("three blocks, held back");
  let six = audio_at(&audio_of(&packets[1]), 32);
  let (refusal, back) = by_container(muxer.write_audio(six.clone()));
  assert_eq!(refusal, payload(PayloadRule::Ac3Blocks));
  untouched!(six, back);
}

/// **L26 — a configuration the container needs and the track does not
/// carry is refused at open, by name, and nothing is written**: H.264
/// without one into MP4, Opus without one into MP4, whose `dOps` refuses
/// its absence at the trailer. AAC's is refused only with the automatic
/// filters off (L29); WebM's AV1 takes its first packet's (L124).
#[test]
fn l26_a_missing_configuration_is_refused_at_open() {
  let Some(corpus) = Corpus::new() else { return };
  let open = |name: &str, container: ContainerName, table| {
    let target = output(&corpus, name);
    let result = FfmpegOwnedMuxer::open(
      File::create(&target).expect("create"),
      container,
      table,
      &[],
      PacketLimits::default(),
    );
    assert_eq!(
      std::fs::metadata(&target).expect("the file").len(),
      0,
      "nothing was written"
    );
    result.map(drop)
  };
  let none = || Some(bytes_of(&[]));
  let (h264, _) = first_track(&corpus.multi_track_mkv(), TrackKind::Video, 1);
  let h264 = h264
    .into_iter()
    .map(|row| row.with_extradata(none()))
    .collect();
  assert!(matches!(
    open("l26.mp4", ContainerName::MP4, h264),
    Err(MuxOpenError::ConfigurationMissing { codec, .. }) if codec == CodecId::H264
  ));
  let (opus, _) = first_track(&opus_mkv(&corpus), TrackKind::Audio, 1);
  let opus = opus
    .into_iter()
    .map(|row| row.with_extradata(none()))
    .collect();
  assert!(matches!(
    open("l26-opus.mp4", ContainerName::MP4, opus),
    Err(MuxOpenError::ConfigurationMissing { codec, .. }) if codec == CodecId::OPUS
  ));
}

/// **L27 — the report names exactly what each container drops of a timed
/// track's language and flags, and the file agrees**, case by case:
/// Matroska keeps a language but an empty one and `und`, the default and
/// forced flags, commentary, the impairments and original or dub — one of
/// them — and no other; WebM the default and forced flags; MP4 a packed
/// code that unpacks to itself, the enabled flag and its kind boxes. Read
/// back, the report names exactly each declared field the file does not
/// hold.
#[test]
fn l27_the_report_names_what_each_timed_road_drops() {
  use TrackDisposition as D;
  let Some(corpus) = Corpus::new() else { return };
  let h264_source = corpus.multi_track_mkv();
  let av1_source = av1_mkv(&corpus);
  let h264 = first_track(&h264_source, TrackKind::Video, 2).1;
  let av1 = first_track(&av1_source, TrackKind::Video, 2).1;
  let cases = [
    (
      ContainerName::MATROSKA,
      "mkv",
      Some("eng"),
      D::DEFAULT | D::FORCED | D::COMMENT | D::HEARING_IMPAIRED | D::VISUAL_IMPAIRED | D::ORIGINAL,
    ),
    (ContainerName::MATROSKA, "mkv", Some("en-US"), D::DUB),
    (
      ContainerName::MATROSKA,
      "mkv",
      Some(""),
      D::ORIGINAL | D::DUB,
    ),
    (
      ContainerName::MATROSKA,
      "mkv",
      Some("und"),
      D::KARAOKE
        | D::LYRICS
        | D::CLEAN_EFFECTS
        | D::CAPTIONS
        | D::DESCRIPTIONS
        | D::METADATA
        | D::NON_DIEGETIC,
    ),
    (
      ContainerName::WEBM,
      "webm",
      Some("eng"),
      D::DEFAULT | D::FORCED,
    ),
    (
      ContainerName::WEBM,
      "webm",
      Some(""),
      D::COMMENT | D::HEARING_IMPAIRED,
    ),
    (
      ContainerName::MP4,
      "mp4",
      Some("eng"),
      D::DEFAULT | D::HEARING_IMPAIRED | D::COMMENT | D::DUB | D::FORCED,
    ),
    (
      ContainerName::MP4,
      "mp4",
      Some("en"),
      D::VISUAL_IMPAIRED | D::DESCRIPTIONS | D::CAPTIONS,
    ),
    (
      ContainerName::MP4,
      "mp4",
      Some("ENG"),
      D::KARAOKE | D::ORIGINAL,
    ),
    (ContainerName::MP4, "mp4", Some("`ab"), D::empty()),
  ];
  for (index, (container, extension, language, flags)) in cases.into_iter().enumerate() {
    let (source, packets) = if container == ContainerName::WEBM {
      (&av1_source, &av1)
    } else {
      (&h264_source, &h264)
    };
    let mut table = table_of(source, TrackKind::Video);
    table[0].set_language(language.map(Into::into));
    table[0].set_disposition(flags);
    let offered: Vec<Row> = table.iter().map(offered).collect();
    let target = output(&corpus, &format!("l27-{index}.{extension}"));
    let (mut muxer, report) = FfmpegOwnedMuxer::open(
      File::create(&target).expect("create"),
      container,
      table,
      &[],
      PacketLimits::default(),
    )
    .expect("open");
    for (at, packet) in packets.iter().enumerate() {
      let at = 40 * at as i64;
      muxer
        .write_video(video_at(&video_of(packet), Some(at), at))
        .map_err(|error| error.map_packet(drop))
        .expect("a packet");
    }
    muxer.finish().expect("the container is finished");
    let written = rows(&target);
    let fields: Vec<Uncarried> = report
      .uncarried()
      .iter()
      .filter(|lost| !matches!(lost, Uncarried::Rewritten { .. }))
      .cloned()
      .collect();
    assert_eq!(
      fields,
      lost(0, &offered[0], &written[0]),
      "case {index}: the report names exactly what the file lost",
    );
  }
}

/// **L28 — a packet the container writes in its own form is reported at
/// open, and reads back so**: MP4 marks each `mov_text` cue's end with an
/// empty sample of its own; MP4 writes a transport stream's Annex B H.264
/// with length prefixes; Matroska strips a WavPack block's header to the
/// fields it keeps.
#[test]
fn l28_a_rewrite_is_reported_and_reads_back_so() {
  let Some(corpus) = Corpus::new() else { return };
  let track = TrackIndex::new(0);
  let open = |name: &str, container: ContainerName, table| {
    FfmpegOwnedMuxer::open(
      File::create(output(&corpus, name)).expect("create"),
      container,
      table,
      &[],
      PacketLimits::default(),
    )
    .expect("open")
  };

  let (table, cues) = first_track(&mov_text_mp4(&corpus), TrackKind::Subtitle, 1);
  let (mut muxer, report) = open("l28.mp4", ContainerName::MP4, table);
  assert!(report.uncarried().contains(&Uncarried::Rewritten {
    track,
    rewrite: Rewrite::CueEnds
  }));
  let cue = subtitle_of(&cues[0]);
  muxer
    .write_subtitle(subtitle_at(&cue, 0, 500))
    .expect("a cue");
  muxer
    .write_subtitle(subtitle_at(&cue, 1000, 500))
    .expect("another, after a gap");
  muxer.finish().expect("finished");
  let ends: Vec<(Option<i64>, bool)> = packets(&output(&corpus, "l28.mp4"))[0]
    .iter()
    .map(|seen| {
      (
        seen
          .pts
          .and_then(|pts| pts.checked_rescale_with(millis(), Rounding::Nearest))
          .map(|pts| pts.pts()),
        seen.bytes == [0, 0],
      )
    })
    .collect();
  // The last cue's end, written at the trailer (`movenc.c` 9024–9031), is
  // not read back.
  assert_eq!(
    ends,
    vec![(Some(0), false), (Some(500), true), (Some(1000), false)],
    "an empty sample at the end of a cue a later one passes, read back as a cue of its own",
  );

  let (table, video) = first_track(&corpus.transport_stream(), TrackKind::Video, 1);
  let (mut muxer, report) = open("l28-annexb.mp4", ContainerName::MP4, table);
  assert!(report.uncarried().contains(&Uncarried::Rewritten {
    track,
    rewrite: Rewrite::LengthPrefixed
  }));
  let offered = video_of(&video[0]);
  assert!(
    offered.data().as_ref().starts_with(&[0, 0, 0, 1])
      || offered.data().as_ref().starts_with(&[0, 0, 1]),
    "the premise: Annex B"
  );
  muxer
    .write_video(video_at(&offered, Some(0), 0))
    .expect("written");
  muxer.finish().expect("finished");
  assert_ne!(
    packets(&output(&corpus, "l28-annexb.mp4"))[0][0].bytes,
    offered.data().as_ref(),
    "read back with length prefixes",
  );

  let (table, blocks) = first_track(
    &audio_file(&corpus, "wavpack.wv", "wavpack"),
    TrackKind::Audio,
    1,
  );
  let (mut muxer, report) = open("l28-wavpack.mkv", ContainerName::MATROSKA, table);
  assert!(report.uncarried().contains(&Uncarried::Rewritten {
    track,
    rewrite: Rewrite::WavPackHeaders
  }));
  let block = audio_of(&blocks[0]);
  muxer.write_audio(audio_at(&block, 0)).expect("written");
  muxer.finish().expect("finished");
  assert_ne!(
    packets(&output(&corpus, "l28-wavpack.mkv"))[0][0].bytes,
    block.data().as_ref(),
    "read back with the header Matroska rebuilds",
  );
}

/// The side data of each packet of `path`'s first track, in order: each
/// entry's kind and bytes.
fn side_data_of(path: &Path) -> Vec<Vec<(i32, Vec<u8>)>> {
  let mut demuxer = FfmpegOwnedDemuxer::open(path).expect("open");
  let mut out = Vec::new();
  while let Some(packet) = demuxer.next_packet().expect("a packet or the end") {
    if packet.track().get() != 0 {
      continue;
    }
    let entries = match &packet {
      DemuxedPacket::Video(p) => p.packet().extra().side_data().to_vec(),
      DemuxedPacket::Audio(p) => p.packet().extra().side_data().to_vec(),
      other => panic!("video or audio: {other:?}"),
    };
    out.push(
      entries
        .iter()
        .map(|entry| (entry.kind(), entry.data().to_vec()))
        .collect(),
    );
  }
  out
}

/// The lines of `report` a kind of loss fills.
fn lines(
  report: &mediaencode_ffmpeg::MuxReport,
  of: impl Fn(&Uncarried) -> bool,
) -> Vec<Uncarried> {
  report
    .uncarried()
    .iter()
    .filter(|line| of(line))
    .cloned()
    .collect()
}

/// **L29 — a transport stream's AAC, framed in ADTS, is remuxed into MP4
/// and Matroska, the conversion reported**: the automatic filter strips
/// each frame's header and makes the track's configuration from the first,
/// as `ffmpeg -c copy` does, and the report names it for the track at the
/// first frame, which inserts it. Each frame reads back as its raw AAC.
/// With the filters off the track opens, its first packet to bring its
/// configuration (R9: ruling 4a, L108): an ADTS frame, which nothing
/// converts, refused by name ([`PayloadRule::AdtsHeader`]), untouched.
#[test]
fn l29_a_transport_streams_adts_aac_is_converted_and_reported() {
  let Some(corpus) = Corpus::new() else { return };
  let ts = corpus.transport_stream();
  let (_, frames) = first_track(&ts, TrackKind::Audio, 3);
  let track = TrackIndex::new(0);
  for (container, name) in [
    (ContainerName::MP4, "l29.mp4"),
    (ContainerName::MATROSKA, "l29.mkv"),
  ] {
    let target = output(&corpus, name);
    let (mut muxer, report) = FfmpegOwnedMuxer::open(
      File::create(&target).expect("create"),
      container.clone(),
      table_of(&ts, TrackKind::Audio),
      &[],
      PacketLimits::default(),
    )
    .expect("the filters make the configuration");
    let filtered = Uncarried::Filtered {
      track,
      filter: AutomaticFilter::AacAdtsToAsc,
    };
    assert!(
      !report.uncarried().contains(&filtered),
      "{name}: the first packet decides"
    );
    let mut offered = Vec::new();
    for (index, frame) in frames.iter().enumerate() {
      let frame = audio_of(frame);
      let bytes = frame.data().as_ref().to_vec();
      assert_eq!(
        (bytes[0], bytes[1] & 0xF0),
        (0xFF, 0xF0),
        "the premise: an ADTS frame"
      );
      muxer
        .write_audio(audio_at(&frame, 21 * index as i64))
        .map_err(|error| error.map_packet(drop))
        .expect("an ADTS frame the filter converts");
      offered.push(bytes);
    }
    assert!(
      muxer.report().uncarried().contains(&filtered),
      "{name}: named at the frame that inserts it"
    );
    muxer.finish().expect("finished");
    let written = &packets(&target)[0];
    assert_eq!(written.len(), offered.len(), "{name}");
    for (seen, bytes) in written.iter().zip(&offered) {
      // Seven bytes of header, and two more of CRC where it is present.
      let header = if bytes[1] & 1 == 1 { 7 } else { 9 };
      assert_eq!(seen.bytes, bytes[header..], "{name}: the raw frame");
    }
    let demuxer = FfmpegOwnedDemuxer::open(&target).expect("open");
    assert!(
      !demuxer.tracks()[0].extra().ticket().extradata().is_empty(),
      "{name}: configured from the first frame's header"
    );
  }
  for (container, name) in [
    (ContainerName::MP4, "l29-off.mp4"),
    (ContainerName::MATROSKA, "l29-off.mkv"),
  ] {
    let target = output(&corpus, name);
    let mut muxer = muxer_with(
      &target,
      container,
      table_of(&ts, TrackKind::Audio),
      filters(false),
    );
    let framed = audio_at(&audio_of(&frames[0]), 0);
    let (refusal, back) = by_container(muxer.write_audio(framed.clone()));
    assert_eq!(refusal, payload(PayloadRule::AdtsHeader), "{name}");
    untouched!(framed, back);
    muxer.finish().expect("finished");
  }
  // The filter's own rule: a packet that is not one ADTS frame — here two
  // in one — is refused by name, untouched.
  let mut muxer = muxer_into(
    &output(&corpus, "l29-two.mp4"),
    ContainerName::MP4,
    table_of(&ts, TrackKind::Audio),
  );
  let first = audio_of(&frames[0]);
  let mut two = first.data().as_ref().to_vec();
  two.extend_from_slice(audio_of(&frames[1]).data().as_ref());
  let offered = audio_at(&audio_holding(&first, &two), 0);
  let (refusal, back) = by_container(muxer.write_audio(offered.clone()));
  assert_eq!(refusal, payload(PayloadRule::AdtsFrame));
  untouched!(offered, back);
}

/// **L30 — MP4 keeps a duration only as the distance to the next decode
/// time, and reports a track whose packets do not abut**, once, when the
/// packet showing it is written. Packets that abut report nothing.
#[test]
fn l30_mp4_reports_the_durations_it_does_not_keep() {
  let Some(corpus) = Corpus::new() else { return };
  let source = corpus.multi_track_mkv();
  let (_, frames) = first_track(&source, TrackKind::Video, 3);
  let at = |frame: &DemuxedPacket<Ffmpeg, FfmpegBytes>, at: i64| {
    video_at(&video_of(frame).with_duration(Some(ms(40))), Some(at), at)
  };
  let durations = |muxer: &FfmpegOwnedMuxer<File>| {
    lines(muxer.report(), |line| {
      matches!(
        line,
        Uncarried::Rewritten {
          rewrite: Rewrite::Durations,
          ..
        }
      )
    })
  };
  let mut muxer = muxer_into(
    &output(&corpus, "l30.mp4"),
    ContainerName::MP4,
    table_of(&source, TrackKind::Video),
  );
  for (index, frame) in frames.iter().enumerate() {
    muxer
      .write_video(at(frame, 40 * index as i64))
      .map_err(|error| error.map_packet(drop))
      .expect("a packet");
  }
  assert_eq!(durations(&muxer), vec![], "packets that abut lose nothing");
  muxer.finish().expect("finished");

  let mut muxer = muxer_into(
    &output(&corpus, "l30-gap.mp4"),
    ContainerName::MP4,
    table_of(&source, TrackKind::Video),
  );
  muxer
    .write_video(at(&frames[0], 0))
    .map_err(|error| error.map_packet(drop))
    .expect("a packet");
  assert_eq!(durations(&muxer), vec![], "nothing shows it yet");
  // 80 ms after the first, which offered 40.
  muxer
    .write_video(at(&frames[1], 80))
    .map_err(|error| error.map_packet(drop))
    .expect("a packet");
  let reported = vec![Uncarried::Rewritten {
    track: TrackIndex::new(0),
    rewrite: Rewrite::Durations,
  }];
  assert_eq!(durations(&muxer), reported);
  muxer
    .write_video(at(&frames[2], 200))
    .map_err(|error| error.map_packet(drop))
    .expect("a packet");
  assert_eq!(durations(&muxer), reported, "once for the track");
  muxer.finish().expect("finished");
}

/// **L31 — packet side data a container has no place for is reported for
/// its track, by name, once**: MP4 drops closed captions, and may drop
/// skip samples; Matroska keeps a block addition of id 1, which reads back
/// as itself, and drops one of another id.
#[test]
fn l31_side_data_a_container_drops_is_reported_by_name() {
  use ffmpeg_next::ffi::AVPacketSideDataType as T;
  let Some(corpus) = Corpus::new() else { return };
  let source = corpus.multi_track_mkv();
  let (_, video) = first_track(&source, TrackKind::Video, 2);
  let track = TrackIndex::new(0);
  let with = |packet: &DemuxedPacket<Ffmpeg, FfmpegBytes>, at: i64, entries: Vec<SideDataEntry>| {
    let mut packet = video_of(packet)
      .with_pts(Some(ms(at)))
      .with_dts(Some(ms(at)));
    packet.extra_mut().set_side_data(entries);
    VideoTrackPacket::new(TrackIndex::new(0), packet)
  };
  let sides = |report: &mediaencode_ffmpeg::MuxReport| {
    lines(report, |line| matches!(line, Uncarried::SideData { .. }))
  };

  let mut muxer = muxer_into(
    &output(&corpus, "l31.mp4"),
    ContainerName::MP4,
    table_of(&source, TrackKind::Video),
  );
  for (index, packet) in video.iter().enumerate() {
    muxer
      .write_video(with(
        packet,
        40 * index as i64,
        vec![side(T::AV_PKT_DATA_A53_CC, &[1, 2, 3])],
      ))
      .map_err(|error| error.map_packet(drop))
      .expect("a packet");
  }
  assert_eq!(
    sides(muxer.report()),
    vec![Uncarried::SideData {
      track,
      kind: T::AV_PKT_DATA_A53_CC as i32,
      name: Some("A53 Closed Captions"),
      fate: SideDataFate::Dropped,
    }],
    "once, for two packets"
  );
  muxer.finish().expect("finished");

  let (_, audio) = first_track(&source, TrackKind::Audio, 1);
  let mut muxer = muxer_into(
    &output(&corpus, "l31-audio.mp4"),
    ContainerName::MP4,
    table_of(&source, TrackKind::Audio),
  );
  let mut skip = 0u32.to_le_bytes().to_vec();
  skip.extend(100u32.to_le_bytes());
  skip.extend([0, 0]);
  let mut skipping = audio_of(&audio[0])
    .with_pts(Some(ms(0)))
    .with_dts(Some(ms(0)));
  skipping
    .extra_mut()
    .set_side_data(vec![side(T::AV_PKT_DATA_SKIP_SAMPLES, &skip)]);
  muxer
    .write_audio(AudioTrackPacket::new(track, skipping))
    .map_err(|error| error.map_packet(drop))
    .expect("a packet");
  assert_eq!(
    sides(muxer.report()),
    vec![Uncarried::SideData {
      track,
      kind: T::AV_PKT_DATA_SKIP_SAMPLES as i32,
      name: Some("Skip Samples"),
      fate: SideDataFate::PossiblyDropped,
    }]
  );
  muxer.finish().expect("finished");

  let target = output(&corpus, "l31.mkv");
  let mut muxer = muxer_into(
    &target,
    ContainerName::MATROSKA,
    table_of(&source, TrackKind::Video),
  );
  let addition = |id: u64| {
    let mut bytes = id.to_be_bytes().to_vec();
    bytes.extend([7, 7]);
    bytes
  };
  muxer
    .write_video(with(
      &video[0],
      0,
      vec![side(T::AV_PKT_DATA_MATROSKA_BLOCKADDITIONAL, &addition(1))],
    ))
    .map_err(|error| error.map_packet(drop))
    .expect("a packet");
  assert_eq!(sides(muxer.report()), vec![], "kept");
  muxer
    .write_video(with(
      &video[1],
      40,
      vec![side(T::AV_PKT_DATA_MATROSKA_BLOCKADDITIONAL, &addition(2))],
    ))
    .map_err(|error| error.map_packet(drop))
    .expect("a packet");
  assert_eq!(
    sides(muxer.report()),
    vec![Uncarried::SideData {
      track,
      kind: T::AV_PKT_DATA_MATROSKA_BLOCKADDITIONAL as i32,
      name: Some("Matroska BlockAdditional"),
      fate: SideDataFate::Dropped,
    }]
  );
  muxer.finish().expect("finished");
  let read = side_data_of(&target);
  let kind = T::AV_PKT_DATA_MATROSKA_BLOCKADDITIONAL as i32;
  assert!(
    read[0].contains(&(kind, addition(1))),
    "read back as itself: {read:?}"
  );
  assert!(
    read[1].iter().all(|(entry, _)| *entry != kind),
    "not written: {read:?}"
  );
}

/// PCM in Matroska, whose reader states no channel layout.
#[rustfmt::skip]
fn pcm_mkv(corpus: &Corpus) -> PathBuf {
  generated(corpus, "pcm.mkv", &[
    "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000:duration=0.5",
    "-ac", "2", "-c:a", "pcm_s16le",
  ])
}

/// **L32 — MP4 PCM whose channel layout its `chnl` box cannot write is
/// refused at open, by name, nothing written**: Matroska's PCM reads back
/// with no layout, whose channels MP4 can place nowhere, and would fail
/// the trailer. QuickTime's, its layout stated, is written (L16, L19).
#[test]
fn l32_mp4_refuses_pcm_whose_layout_it_cannot_write() {
  let Some(corpus) = Corpus::new() else { return };
  let table = table_of(&pcm_mkv(&corpus), TrackKind::Audio);
  assert_eq!(
    (
      table[0].side_data().ch_layout().order(),
      table[0].side_data().ch_layout().channels()
    ),
    (
      ffmpeg_next::ffi::AVChannelOrder::AV_CHANNEL_ORDER_UNSPEC as i32,
      2
    ),
    "the premise: two channels, no layout"
  );
  let target = output(&corpus, "l32.mp4");
  let refused = FfmpegOwnedMuxer::open(
    File::create(&target).expect("create"),
    ContainerName::MP4,
    table,
    &[],
    PacketLimits::default(),
  );
  assert!(
    matches!(refused, Err(MuxOpenError::ChannelLayout { track }) if track.get() == 0),
    "{refused:?}"
  );
  assert_eq!(std::fs::metadata(&target).expect("the file").len(), 0);
  let (_muxer, _) = FfmpegOwnedMuxer::open(
    File::create(output(&corpus, "l32.mkv")).expect("create"),
    ContainerName::MATROSKA,
    table_of(&pcm_mkv(&corpus), TrackKind::Audio),
    &[],
    PacketLimits::default(),
  )
  .expect("Matroska writes no layout");
}

/// TrueHD in Matroska, from libavcodec's experimental encoder.
#[rustfmt::skip]
fn truehd_mkv(corpus: &Corpus) -> PathBuf {
  generated(corpus, "truehd.mkv", &[
    "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000:duration=0.5",
    "-ac", "2", "-c:a", "truehd", "-strict", "-2",
  ])
}

/// **L33 — TrueHD into MP4 is refused at open, by name, nothing written**:
/// MP4 admits it to its query, and refuses its header unless experiments
/// are allowed.
#[test]
fn l33_mp4_refuses_truehd() {
  let Some(corpus) = Corpus::new() else { return };
  let target = output(&corpus, "l33.mp4");
  let refused = FfmpegOwnedMuxer::open(
    File::create(&target).expect("create"),
    ContainerName::MP4,
    table_of(&truehd_mkv(&corpus), TrackKind::Audio),
    &[],
    PacketLimits::default(),
  );
  assert!(
    matches!(
      refused,
      Err(MuxOpenError::CodecNotCarried { track, codec })
        if track.get() == 0 && codec == CodecId::TRUEHD
    ),
    "{refused:?}"
  );
  assert_eq!(std::fs::metadata(&target).expect("the file").len(), 0);
}

/// RealVideo 1.0 in RealMedia.
#[rustfmt::skip]
fn rv10_rm(corpus: &Corpus) -> PathBuf {
  generated(corpus, "rv10.rm", &[
    "-f", "lavfi", "-i", "testsrc2=size=64x48:rate=25:duration=0.2",
    "-c:v", "rv10",
  ])
}

/// **L34 — RealVideo into Matroska is refused at open, by name, nothing
/// written**: Matroska admits it to its query, and refuses its header.
#[test]
fn l34_matroska_refuses_realvideo() {
  let Some(corpus) = Corpus::new() else { return };
  let target = output(&corpus, "l34.mkv");
  let refused = FfmpegOwnedMuxer::open(
    File::create(&target).expect("create"),
    ContainerName::MATROSKA,
    table_of(&rv10_rm(&corpus), TrackKind::Video),
    &[],
    PacketLimits::default(),
  );
  assert!(
    matches!(
      refused,
      Err(MuxOpenError::CodecNotCarried { track, codec })
        if track.get() == 0 && codec == CodecId::RV10
    ),
    "{refused:?}"
  );
  assert_eq!(std::fs::metadata(&target).expect("the file").len(), 0);
}

/// VP9 in WebM, made by libvpx.
#[rustfmt::skip]
fn vp9_webm(corpus: &Corpus) -> PathBuf {
  generated(corpus, "vp9.webm", &[
    "-f", "lavfi", "-i", "testsrc2=size=64x64:rate=25:duration=0.2",
    "-c:v", "libvpx-vp9", "-deadline", "realtime", "-cpu-used", "8",
  ])
}

/// **L35 — with the automatic filters on, Matroska joins VP9's hidden
/// frames into the superframe of the next frame shown, the conversion
/// reported**: two hidden frames and a shown one read back as one
/// superframe, at the shown frame's time; a seventh hidden frame in a row
/// is refused by name. With the filters off, each frame is written as it
/// was handed over.
#[test]
fn l35_vp9_hidden_frames_join_a_superframe() {
  let Some(corpus) = Corpus::new() else { return };
  let source = vp9_webm(&corpus);
  let (_, frames) = first_track(&source, TrackKind::Video, 1);
  let frame = video_of(&frames[0]);
  // The frame marker and profile 0, then: shown (`0x82`), hidden (`0x80`).
  let hidden = |tag: u8| [0x80, tag, tag];
  let shown = [0x82, 9, 9, 9];
  let track = TrackIndex::new(0);
  let target = output(&corpus, "l35.mkv");
  let (mut muxer, report) = FfmpegOwnedMuxer::open(
    File::create(&target).expect("create"),
    ContainerName::MATROSKA,
    table_of(&source, TrackKind::Video),
    &[],
    PacketLimits::default(),
  )
  .expect("open");
  assert!(report.uncarried().contains(&Uncarried::Filtered {
    track,
    filter: AutomaticFilter::Vp9Superframe
  }));
  for (at, bytes) in [(0, &hidden(1)[..]), (40, &hidden(2)[..]), (80, &shown[..])] {
    muxer
      .write_video(video_at(&video_holding(&frame, bytes), Some(at), at))
      .map_err(|error| error.map_packet(drop))
      .expect("a frame the filter takes");
  }
  for index in 0..6 {
    let at = 120 + 40 * index;
    muxer
      .write_video(video_at(&video_holding(&frame, &hidden(3)), Some(at), at))
      .map_err(|error| error.map_packet(drop))
      .expect("up to six hidden frames held");
  }
  let seventh = video_at(&video_holding(&frame, &hidden(4)), Some(400), 400);
  let (refusal, back) = by_container(muxer.write_video(seventh.clone()));
  assert_eq!(refusal, payload(PayloadRule::Superframe));
  untouched!(seventh, back);
  muxer
    .write_video(video_at(&video_holding(&frame, &shown), Some(440), 440))
    .map_err(|error| error.map_packet(drop))
    .expect("the six joined");
  muxer.finish().expect("finished");
  let written = &packets(&target)[0];
  let mut first = [hidden(1).as_slice(), &hidden(2), &shown].concat();
  // The index: its marker — sizes of one byte, three frames — the sizes,
  // the marker again.
  first.extend([0xC2, 3, 3, 4, 0xC2]);
  assert_eq!(written.len(), 2, "two superframes");
  assert_eq!(written[0].bytes, first, "the first, joined");
  assert_eq!(
    written[0]
      .pts
      .and_then(|pts| pts.checked_rescale_with(millis(), Rounding::Nearest))
      .map(|pts| pts.pts()),
    Some(80),
    "at the shown frame's time"
  );

  let off = output(&corpus, "l35-off.mkv");
  let mut muxer = muxer_with(
    &off,
    ContainerName::MATROSKA,
    table_of(&source, TrackKind::Video),
    filters(false),
  );
  for (at, bytes) in [(0, &hidden(1)[..]), (40, &hidden(2)[..]), (80, &shown[..])] {
    muxer
      .write_video(video_at(&video_holding(&frame, bytes), Some(at), at))
      .map_err(|error| error.map_packet(drop))
      .expect("a frame");
  }
  muxer.finish().expect("finished");
  assert_eq!(
    packets(&off)[0]
      .iter()
      .map(|seen| seen.bytes.clone())
      .collect::<Vec<_>>(),
    vec![hidden(1).to_vec(), hidden(2).to_vec(), shown.to_vec()],
    "each as handed over"
  );
}

/// A file whose writes fail as a full disk fails them once its switch is
/// thrown; it seeks and flushes as a file does.
struct FillsUp {
  file: File,
  full: Arc<std::sync::atomic::AtomicBool>,
}

impl Write for FillsUp {
  fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
    if self.full.load(Ordering::Relaxed) {
      return Err(io::Error::from_raw_os_error(ENOSPC));
    }
    self.file.write(buf)
  }
  fn flush(&mut self) -> io::Result<()> {
    self.file.flush()
  }
}

impl Seek for FillsUp {
  fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
    self.file.seek(pos)
  }
}

/// `ENOSPC`, as the platform numbers it.
const ENOSPC: i32 = ffmpeg_next::util::error::ENOSPC;

/// **L36 — a write the sink fails is named the sink's where the muxer met
/// it: at the header, the open's fault; at the trailer, the finish's.**
/// Over a sink full from the start the header's bytes never land, and the
/// open faults at [`MuxStage::Header`] naming [`OutputError::SinkWrite`] —
/// libavformat answers success there, its flush following its check
/// (`mux.c` 482–486 in FFmpeg 9.0.1). Over one that fills up after the last
/// packet, `finish` faults at [`MuxStage::Trailer`], naming the same.
///
/// PLANT: without the base's read of the I/O context's error at every
/// call, the open succeeds over the full sink and the trailer's fault
/// names `Ffmpeg(ENOSPC)`.
#[test]
fn l36_a_failed_write_is_named_the_sinks_at_the_header_and_the_trailer() {
  let Some(corpus) = Corpus::new() else { return };
  let source = corpus.multi_track_mkv();
  let video = owned_video_and_font(&source, 3).video;
  let sink = |name: &str, full: bool| {
    let full = Arc::new(std::sync::atomic::AtomicBool::new(full));
    (
      FillsUp {
        file: File::create(output(&corpus, name)).expect("create"),
        full: Arc::clone(&full),
      },
      full,
    )
  };
  let names_the_write = |error: &OutputError| match error {
    OutputError::SinkWrite(error) => {
      assert_eq!(
        *error,
        ffmpeg_next::Error::from(ffmpeg_next::ffi::AVERROR(ENOSPC))
      );
    }
    other => panic!("expected the sink's failed write, got {other:?}"),
  };

  let (full_sink, _) = sink("l36-header.mkv", true);
  match FfmpegOwnedMuxer::open(
    full_sink,
    ContainerName::MATROSKA,
    video_table(&source),
    &[],
    PacketLimits::default(),
  ) {
    Err(MuxOpenError::Fault(fault)) => {
      assert_eq!(fault.stage(), MuxStage::Header);
      names_the_write(fault.error().expect("the context's account"));
    }
    other => panic!("expected the header's fault, got {:?}", other.map(drop)),
  }

  let (filling, full) = sink("l36-trailer.mkv", false);
  let (mut muxer, _) = FfmpegOwnedMuxer::open(
    filling,
    ContainerName::MATROSKA,
    video_table(&source),
    &[],
    PacketLimits::default(),
  )
  .expect("open");
  for (index, packet) in video.iter().enumerate() {
    let at = 40 * index as i64;
    muxer
      .write_video(restamped(packet, 0, Some(at), Some(at)))
      .map_err(|error| error.map_packet(drop))
      .expect("a packet");
  }
  full.store(true, Ordering::Relaxed);
  let fault = muxer.finish().expect_err("the disk is full");
  assert_eq!(fault.stage(), MuxStage::Trailer);
  names_the_write(fault.error().expect("the context's account"));
}

/// What reached a [`Tallied`] sink, kept outside it.
#[derive(Default)]
struct Tally {
  written: std::sync::Mutex<Vec<u8>>,
  flushes: AtomicU32,
  drops: AtomicU32,
}

/// A file that keeps a tally of what reaches it: the bytes written, its
/// flushes and its own drop.
struct Tallied {
  file: File,
  tally: Arc<Tally>,
}

impl Write for Tallied {
  fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
    let written = self.file.write(buf)?;
    self
      .tally
      .written
      .lock()
      .expect("the tally")
      .extend_from_slice(&buf[..written]);
    Ok(written)
  }
  fn flush(&mut self) -> io::Result<()> {
    self.tally.flushes.fetch_add(1, Ordering::Relaxed);
    self.file.flush()
  }
}

impl Seek for Tallied {
  fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
    self.file.seek(pos)
  }
}

impl Drop for Tallied {
  fn drop(&mut self) {
    self.tally.drops.fetch_add(1, Ordering::Relaxed);
  }
}

/// **L37 — a muxer dropped mid-file frees its sink once and writes nothing
/// it held.** A video and an audio track of the multi-track file; one video
/// packet written — held in libavformat's interleaving queue, as the audio
/// track has none yet — and the muxer dropped, `finish` never called. The
/// context goes first, with the muxer's `deinit` and the queued packet,
/// then the I/O context: the sink is flushed once and dropped once, and it
/// holds Matroska's header and not the packet.
///
/// PLANT: a `Drop` on the base's context that leaves its I/O context
/// undropped keeps the sink alive (`drops` 0).
#[test]
fn l37_a_muxer_dropped_mid_file_frees_its_sink_once() {
  let Some(corpus) = Corpus::new() else { return };
  let source = corpus.multi_track_mkv();
  let video = owned_video_and_font(&source, 1).video;
  let table = {
    let demuxer = FfmpegOwnedDemuxer::open(&source).expect("open");
    [TrackKind::Video, TrackKind::Audio]
      .into_iter()
      .map(|kind| {
        let row = demuxer
          .tracks()
          .iter()
          .find(|row| row.kind() == kind)
          .expect("a track of the kind");
        TrackDescription::from_track_info(&**row, None, ticket(row)).expect("a description")
      })
      .collect::<Vec<_>>()
  };
  let tally = Arc::new(Tally::default());
  let (mut muxer, _) = FfmpegOwnedMuxer::open(
    Tallied {
      file: File::create(output(&corpus, "l37.mkv")).expect("create"),
      tally: Arc::clone(&tally),
    },
    ContainerName::MATROSKA,
    table,
    &[],
    PacketLimits::default(),
  )
  .expect("open");
  let held = restamped(&video[0], 0, Some(0), Some(0));
  let bytes = held.packet().data().as_ref().to_vec();
  muxer
    .write_video(held)
    .map_err(|error| error.map_packet(drop))
    .expect("a packet the queue holds");
  assert_eq!(tally.drops.load(Ordering::Relaxed), 0);

  drop(muxer);

  assert_eq!(tally.drops.load(Ordering::Relaxed), 1, "dropped once");
  assert_eq!(tally.flushes.load(Ordering::Relaxed), 1, "flushed once");
  let written = tally.written.lock().expect("the tally");
  assert_eq!(&written[..4], &[0x1A, 0x45, 0xDF, 0xA3], "the header");
  assert!(
    !written
      .windows(bytes.len())
      .any(|window| window == bytes.as_slice()),
    "the packet the queue held was freed, never written",
  );
}

/// **L38 — a track whose kind is not its parameters' media type is
/// refused at open, and nothing is written** (table D of the census): a
/// video track over an AAC stream's parameters, which libavformat would
/// make an audio stream of while the muxer judged its packets as video.
///
/// PLANT: without the check in `judge`, Matroska opens over it.
#[test]
fn l38_a_video_track_over_audio_parameters_is_refused_at_open() {
  let Some(corpus) = Corpus::new() else { return };
  let source = corpus.multi_track_mkv();
  let demuxer = FfmpegOwnedDemuxer::open(&source).expect("open");
  let row = |kind: TrackKind| {
    demuxer
      .tracks()
      .iter()
      .find(|row| row.kind() == kind)
      .expect("a track of the kind")
  };
  let pixel_format = match row(TrackKind::Video).params() {
    TrackParams::Video(params) => params.pixel_format().clone(),
    other => panic!("expected video parameters, got {other:?}"),
  };
  let audio = row(TrackKind::Audio);
  let video_over_audio = TrackDescription::new(
    millis(),
    TrackParams::Video(mediaencode::mediaspine::demuxer::VideoTrackParams::new(
      CodecId::AAC,
      64,
      64,
      pixel_format,
      None,
    )),
    ticket(audio),
  )
  .expect("a description");
  let target = output(&corpus, "l38.mkv");
  let refused = FfmpegOwnedMuxer::open(
    File::create(&target).expect("create"),
    ContainerName::MATROSKA,
    vec![video_over_audio],
    &[],
    PacketLimits::default(),
  );
  assert!(
    matches!(refused, Err(MuxOpenError::MediaTypeMismatch { track }) if track.get() == 0),
    "{:?}",
    refused.map(drop)
  );
  assert_eq!(std::fs::metadata(&target).expect("the file").len(), 0);
}

/// One ADTS frame of AAC LC at 48 kHz whose channels are its program
/// config element's — channel configuration 0 — no CRC, `payload` after
/// its header.
fn adts_with(payload: &[u8]) -> Vec<u8> {
  let len = 7 + payload.len();
  let fields: [(u64, u32); 15] = [
    (0xFFF, 12),
    (0, 1),
    (0, 2),
    (1, 1),
    (1, 2),
    (3, 4),
    (0, 1),
    (0, 3),
    (0, 1),
    (0, 1),
    (0, 1),
    (0, 1),
    (len as u64, 13),
    (0x7FF, 11),
    (0, 2),
  ];
  let header = fields
    .iter()
    .fold(0u64, |bits, &(value, width)| (bits << width) | value);
  [&header.to_be_bytes()[1..], payload].concat()
}

/// A program config element naming no element and no comment: its element
/// id 5, tag 0, AAC LC at sample rate index 3, every count zero, no
/// mixdown, the byte alignment and a comment of no bytes — six bytes.
const BARE_PCE: [u8; 6] = [0xA0, 0x98, 0, 0, 0, 0];

/// **L39 — a first ADTS frame whose program config element runs past its
/// payload, or ends it, is refused by name, the packet untouched** (table
/// F of the census): `aac_adtstoasc` consumes the element with no check
/// that it fits and leaves the packet a negative size, or an empty one
/// (`aac_adtstoasc.c` 80–95 in FFmpeg 9.0.1). Into MP4 and Matroska, over
/// a transport stream's AAC track, which states no configuration: an
/// element cut a byte short, and one with nothing after it, are refused;
/// the whole element with its raw data after it is converted.
///
/// PLANT: reading the element's id alone, as before, admits the cut one.
#[test]
fn l39_a_truncated_program_config_element_is_refused() {
  let Some(corpus) = Corpus::new() else { return };
  let ts = corpus.transport_stream();
  let (_, frames) = first_track(&ts, TrackKind::Audio, 1);
  let frame = audio_of(&frames[0]);
  for (container, name) in [
    (ContainerName::MP4, "l39.mp4"),
    (ContainerName::MATROSKA, "l39.mkv"),
  ] {
    let mut muxer = muxer_into(
      &output(&corpus, name),
      container.clone(),
      table_of(&ts, TrackKind::Audio),
    );
    for element in [&BARE_PCE[..5], &BARE_PCE[..]] {
      let cut = audio_at(&audio_holding(&frame, &adts_with(element)), 0);
      let (refusal, back) = by_container(muxer.write_audio(cut.clone()));
      assert_eq!(refusal, payload(PayloadRule::AdtsFrame), "{container}");
      untouched!(cut, back);
    }
    let whole = [&BARE_PCE[..], &[0x21, 0x10, 0x05]].concat();
    muxer
      .write_audio(audio_at(&audio_holding(&frame, &adts_with(&whole)), 0))
      .map_err(|error| error.map_packet(drop))
      .expect("a whole element, its raw data after it");
    muxer.finish().expect("finished");
  }
}

/// An HDR10+ ITU-T T.35 payload — the bytes after the T.35 header — of one
/// window, every field the syntax carries set: application version 1, the
/// targeted display at 400, maxscl 1000, 2000, 3000 and an average of 500,
/// two percentiles, the bright pixels at 10, a knee at (100, 200), three
/// anchors and a saturation weight of 5 (the base's laws build it field by
/// field, `mediaio-ffmpeg/src/hdr_metadata/tests.rs`).
const HDR10_PLUS_T35: [u8; 30] = [
  0x01, 0x40, 0x00, 0x0C, 0x80, 0x07, 0xD0, 0x07, 0xD0, 0x05, 0xDC, 0x00, 0x7D, 0x08, 0x08, 0x01,
  0x91, 0x90, 0x4E, 0x20, 0x0A, 0x41, 0x90, 0x32, 0x0C, 0x0A, 0x05, 0x01, 0xE8, 0xA0,
];

/// An SMPTE ST 2094 application 5 payload: a custom reference white of
/// 203, a baseline of 1000, one alternate image at 2000 with a curve of two
/// points (built field by field in the base's laws).
const APP5_T35: [u8; 19] = [
  0x00, 0xC0, 0x00, 0xCB, 0x03, 0xE8, 0x10, 0x07, 0xD0, 0x00, 0x0C, 0x00, 0x64, 0x00, 0xC8, 0x01,
  0x2C, 0x01, 0x90,
];

/// **L40 — HDR metadata side data reaches Matroska only as a struct its
/// writer serializes whole, and then reads back as itself** (table A of
/// the census). Matroska casts an HDR10+ or APP5 entry to libavutil's
/// struct without asking its size (`matroskaenc.c` 2947–2990 in FFmpeg
/// 9.0.1): an entry of sixteen bytes, and an HDR10+ struct naming four
/// windows, are refused by name, each packet handed back untouched; the
/// struct libavutil parses from a T.35 payload is written, and the packet
/// reads back carrying the very bytes. Two entries of one kind are refused
/// in every container, MP4's too.
///
/// PLANT: without the check in `check_packet_rules`, the sixteen bytes
/// reach the writer, which reads the struct past them.
#[test]
fn l40_hdr_metadata_reaches_matroska_only_whole() {
  use ffmpeg_next::ffi::AVPacketSideDataType as T;
  use mediaio_ffmpeg::hdr_metadata::{hdr10_plus_from_t35, smpte2094_app5_from_t35};
  let Some(corpus) = Corpus::new() else { return };
  let source = corpus.multi_track_mkv();
  let (_, video) = first_track(&source, TrackKind::Video, 3);
  let with = |index: usize, entries: Vec<SideDataEntry>| {
    let at = 40 * index as i64;
    let mut packet = video_of(&video[index])
      .with_pts(Some(ms(at)))
      .with_dts(Some(ms(at)));
    packet.extra_mut().set_side_data(entries);
    VideoTrackPacket::new(TrackIndex::new(0), packet)
  };
  let plus = hdr10_plus_from_t35(&HDR10_PLUS_T35).expect("libavutil parses the payload");
  let app5 = smpte2094_app5_from_t35(&APP5_T35).expect("libavutil parses the payload");
  let (hdr10_plus, smpte_app5) = (
    T::AV_PKT_DATA_DYNAMIC_HDR10_PLUS,
    T::AV_PKT_DATA_DYNAMIC_HDR_SMPTE_2094_APP5,
  );

  let target = output(&corpus, "l40.mkv");
  let mut muxer = muxer_into(
    &target,
    ContainerName::MATROSKA,
    table_of(&source, TrackKind::Video),
  );
  let mut four_windows = plus.clone();
  four_windows[2] = 4;
  for (entries, rule) in [
    (
      vec![side(hdr10_plus, &[0; 16])],
      PayloadRule::DynamicHdrPlus,
    ),
    (
      vec![side(hdr10_plus, &four_windows)],
      PayloadRule::DynamicHdrPlus,
    ),
    (
      vec![side(smpte_app5, &[0; 16])],
      PayloadRule::DynamicHdrApp5,
    ),
    (
      vec![side(hdr10_plus, &plus), side(hdr10_plus, &plus)],
      PayloadRule::SideDataRepeated,
    ),
  ] {
    let offered = with(0, entries);
    let (refusal, back) = by_container(muxer.write_video(offered.clone()));
    assert_eq!(refusal, payload(rule));
    untouched!(offered, back);
  }
  muxer
    .write_video(with(0, vec![side(hdr10_plus, &plus)]))
    .map_err(|error| error.map_packet(drop))
    .expect("an HDR10+ struct Matroska writes whole");
  muxer
    .write_video(with(1, vec![side(smpte_app5, &app5)]))
    .map_err(|error| error.map_packet(drop))
    .expect("an APP5 struct Matroska writes whole");
  muxer
    .write_video(with(2, Vec::new()))
    .map_err(|error| error.map_packet(drop))
    .expect("a packet");
  assert!(muxer.report().is_empty(), "{:?}", muxer.report());
  muxer.finish().expect("finished");
  let read = side_data_of(&target);
  assert!(
    read[0].contains(&(hdr10_plus as i32, plus.clone())),
    "the HDR10+ struct reads back as itself: {:?}",
    read[0]
      .iter()
      .map(|(kind, bytes)| (kind, bytes.len()))
      .collect::<Vec<_>>()
  );
  assert!(
    read[1].contains(&(smpte_app5 as i32, app5.clone())),
    "the APP5 struct reads back as itself"
  );

  let mut muxer = muxer_into(
    &output(&corpus, "l40.mp4"),
    ContainerName::MP4,
    table_of(&source, TrackKind::Video),
  );
  let offered = with(
    0,
    vec![
      side(T::AV_PKT_DATA_A53_CC, &[1]),
      side(T::AV_PKT_DATA_A53_CC, &[2]),
    ],
  );
  let (refusal, back) = by_container(muxer.write_video(offered.clone()));
  assert_eq!(refusal, payload(PayloadRule::SideDataRepeated));
  untouched!(offered, back);
}

/// **L41 — MP4 refuses the AC-3 its trailer would fail the whole file on:
/// a first frame `dac3` does not define, at that frame; a track that took
/// no frame, at `finish`** (table C of the census). MP4 writes `dac3` from
/// the first frame it parses and refuses a bit stream id past 8 at the
/// trailer (`movenc.c` 431–437 in FFmpeg 9.0.1): the frame recoded with id
/// 9 is refused, untouched, as the first; a later frame is not asked.
/// Without any frame the trailer cannot write the box at all (424–429):
/// `finish` refuses by name before it runs, its fault naming no account of
/// the context's.
///
/// PLANT: without the id's check, the recoded first frame is taken and
/// the trailer fails the file.
#[test]
fn l41_mp4_refuses_the_ac3_its_trailer_would_fail_on() {
  let Some(corpus) = Corpus::new() else { return };
  let source = audio_file(&corpus, "ac3.mkv", "ac3");
  let (table, frames) = first_track(&source, TrackKind::Audio, 2);
  let target = output(&corpus, "l41.mp4");
  let mut muxer = muxer_into(&target, ContainerName::MP4, table);
  let recoded = |packet: &Audio| {
    let mut bytes = packet.data().as_ref().to_vec();
    assert_eq!(bytes[5] >> 3, 8, "the premise: bit stream id 8");
    bytes[5] = (bytes[5] & 0x07) | (9 << 3);
    audio_holding(packet, &bytes)
  };
  let first = audio_of(&frames[0]);
  let offered = audio_at(&recoded(&first), 0);
  let (refusal, back) = by_container(muxer.write_audio(offered.clone()));
  assert_eq!(refusal, payload(PayloadRule::Ac3BitStreamId));
  untouched!(offered, back);
  muxer
    .write_audio(audio_at(&first, 0))
    .map_err(|error| error.map_packet(drop))
    .expect("the first frame, id 8");
  muxer
    .write_audio(audio_at(&recoded(&audio_of(&frames[1])), 32))
    .map_err(|error| error.map_packet(drop))
    .expect("a later frame is not asked");
  muxer.finish().expect("finished");
  assert_eq!(packets(&target)[0].len(), 2);

  let none = output(&corpus, "l41-none.mp4");
  let muxer = muxer_into(
    &none,
    ContainerName::MP4,
    table_of(&source, TrackKind::Audio),
  );
  let fault = muxer.finish().expect_err("a track that took no frame");
  assert_eq!(fault.stage(), MuxStage::Trailer);
  assert_eq!(
    fault.refusal(),
    Some(FfmpegMuxRefusal::NoFrame {
      track: TrackIndex::new(0)
    })
  );
  assert!(fault.error().is_none(), "no account of the context's");
}

/// Opus in Matroska, made by libavcodec's own Opus encoder.
#[rustfmt::skip]
fn opus_mkv(corpus: &Corpus) -> PathBuf {
  generated(corpus, "opus.mkv", &[
    "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000:duration=0.5",
    "-ac", "2", "-c:a", "opus", "-strict", "-2",
  ])
}

/// [`opus_mkv`] 10 ms shorter, its encoder's last packet no lone
/// table-of-contents byte: a stream MPEG-TS takes whole.
#[rustfmt::skip]
fn opus_of_two_bytes_mkv(corpus: &Corpus) -> PathBuf {
  generated(corpus, "opus-two-bytes.mkv", &[
    "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000:duration=0.49",
    "-ac", "2", "-c:a", "opus", "-strict", "-2",
  ])
}

/// Two TTML paragraphs in MP4, from libavcodec's TTML encoder.
#[rustfmt::skip]
fn ttml_mp4(corpus: &Corpus) -> PathBuf {
  let srt = written_text(
    corpus,
    "two.srt",
    "1\n00:00:00,000 --> 00:00:01,000\nhello\n\n2\n00:00:01,000 --> 00:00:02,000\nworld\n\n",
  );
  generated(corpus, "ttml.mp4", &[
    "-i", srt.to_str().expect("utf-8 path"), "-c:s", "ttml",
  ])
}

/// **L42 — a configuration MP4's trailer would refuse, or write empty, is
/// refused at open, by name, nothing written** (table C of the census):
/// FLAC's not one 34-byte STREAMINFO, Opus's shorter than an OpusHead,
/// TTML's header strings from libavcodec's encoder cut short; and Opus
/// with none. Each track opens with its own; FLAC and AV1 with none open,
/// their first packet to bring it (R9: ruling 4a, L108).
///
/// PLANT: without `configuration_invalid`, MP4 opens over the cut FLAC
/// STREAMINFO and its trailer fails the file.
#[test]
fn l42_a_configuration_mp4s_trailer_refuses_is_refused_at_open() {
  let Some(corpus) = Corpus::new() else { return };
  let target = output(&corpus, "l42.mp4");
  let open = |table: Vec<TrackDescription<Ffmpeg, FfmpegBytes, CodecTicket>>| {
    let opened = FfmpegOwnedMuxer::open(
      File::create(&target).expect("create"),
      ContainerName::MP4,
      table,
      &[],
      PacketLimits::default(),
    );
    opened.map(drop)
  };
  let with = |path: &Path, kind: TrackKind, extradata: Option<&[u8]>| {
    let mut table = table_of(path, kind);
    if let Some(bytes) = extradata {
      table[0].set_extradata(Some(bytes_of(bytes)));
    }
    table
  };
  let refused = |result: Result<(), MuxOpenError>| match result {
    Err(MuxOpenError::ConfigurationInvalid { track, codec }) if track.get() == 0 => {
      assert_eq!(std::fs::metadata(&target).expect("the file").len(), 0);
      Some(codec)
    }
    Err(MuxOpenError::ConfigurationMissing { track, codec }) if track.get() == 0 => {
      assert_eq!(std::fs::metadata(&target).expect("the file").len(), 0);
      Some(codec)
    }
    Ok(()) => None,
    other => panic!("expected a configuration refused, got {other:?}"),
  };

  let flac = audio_file(&corpus, "flac.flac", "flac");
  assert_eq!(
    refused(open(with(&flac, TrackKind::Audio, None))),
    None,
    "its own"
  );
  assert!(matches!(
    open(with(&flac, TrackKind::Audio, Some(&[0; 33]))),
    Err(MuxOpenError::ConfigurationInvalid { codec, .. }) if codec == CodecId::FLAC
  ));
  assert_eq!(
    refused(open(with(&flac, TrackKind::Audio, Some(&[])))),
    None,
    "none: its first packet brings it"
  );

  let opus = opus_mkv(&corpus);
  assert_eq!(
    refused(open(with(&opus, TrackKind::Audio, None))),
    None,
    "its own"
  );
  assert!(matches!(
    open(with(&opus, TrackKind::Audio, Some(b"OpusHead\x01\x02"))),
    Err(MuxOpenError::ConfigurationInvalid { codec, .. }) if codec == CodecId::OPUS
  ));
  assert!(matches!(
    open(with(&opus, TrackKind::Audio, Some(&[]))),
    Err(MuxOpenError::ConfigurationMissing { codec, .. }) if codec == CodecId::OPUS
  ));

  assert_eq!(
    refused(open(with(&av1_mkv(&corpus), TrackKind::Video, Some(&[])))),
    None,
    "none: its first packet brings it"
  );

  // TTML read back from MP4 is a document; the configuration of
  // libavcodec's TTML encoder, whose paragraphs MP4 joins, comes only from
  // the encoder, so the track's own is replaced by one cut short.
  let ttml = ttml_mp4(&corpus);
  assert_eq!(
    refused(open(with(&ttml, TrackKind::Subtitle, None))),
    None,
    "its own"
  );
  assert!(matches!(
    open(with(&ttml, TrackKind::Subtitle, Some(b"lavc-ttmlenca\0"))),
    Err(MuxOpenError::ConfigurationInvalid { codec, .. }) if codec == CodecId::TTML
  ));
}

/// **L43 — new extradata a container would refuse, or that would change
/// how it writes the packets after it, is refused by name, the packet
/// untouched** (table C of the census): into MP4, Opus's that is no
/// OpusHead `dOps` takes, H.264's Annex B on a track configured by an
/// `avcC`, FLAC's of another length than 34; into Matroska, AAC's that
/// libavcodec does not parse, that is larger than the space Matroska's
/// header reserved, or any where the header reserved no output sampling
/// frequency (L72). Each container takes what it writes whole: an HE-AAC
/// track's configuration within its space reads back.
///
/// PLANT: without the check of new extradata, the Annex B on an `avcC`
/// track is taken and MP4 rewrites the packets after it.
#[test]
fn l43_new_extradata_a_container_would_refuse_is_refused() {
  use ffmpeg_next::ffi::AVPacketSideDataType as T;
  let Some(corpus) = Corpus::new() else { return };
  let new_extradata = |bytes: &[u8]| vec![side(T::AV_PKT_DATA_NEW_EXTRADATA, bytes)];
  let carrying = |packet: &Audio, at: i64, entries: Vec<SideDataEntry>| {
    let mut inner = packet.clone().with_pts(Some(ms(at))).with_dts(Some(ms(at)));
    inner.extra_mut().set_side_data(entries);
    AudioTrackPacket::new(TrackIndex::new(0), inner)
  };

  let opus = opus_mkv(&corpus);
  let (table, frames) = first_track(&opus, TrackKind::Audio, 2);
  let own = table[0].side_data().extradata().to_vec();
  let mut muxer = muxer_into(&output(&corpus, "l43-opus.mp4"), ContainerName::MP4, table);
  let offered = carrying(&audio_of(&frames[0]), 0, new_extradata(b"OpusHead"));
  let (refusal, back) = by_container(muxer.write_audio(offered.clone()));
  assert_eq!(refusal, payload(PayloadRule::NewExtradata));
  untouched!(offered, back);
  muxer
    .write_audio(carrying(&audio_of(&frames[0]), 0, new_extradata(&own)))
    .map_err(|error| error.map_packet(drop))
    .expect("an OpusHead dOps takes");
  muxer.finish().expect("finished");

  let source = corpus.multi_track_mkv();
  let (_, video) = first_track(&source, TrackKind::Video, 1);
  let mut muxer = muxer_into(
    &output(&corpus, "l43-h264.mp4"),
    ContainerName::MP4,
    table_of(&source, TrackKind::Video),
  );
  let mut inner = video_of(&video[0])
    .with_pts(Some(ms(0)))
    .with_dts(Some(ms(0)));
  inner
    .extra_mut()
    .set_side_data(new_extradata(&[0, 0, 0, 1, 0x67, 0x42, 0, 0x0A]));
  let offered = VideoTrackPacket::new(TrackIndex::new(0), inner);
  let (refusal, back) = by_container(muxer.write_video(offered.clone()));
  assert_eq!(
    refusal,
    payload(PayloadRule::NewExtradata),
    "Annex B on an avcC track"
  );
  untouched!(offered, back);

  let flac = audio_file(&corpus, "flac.flac", "flac");
  let (table, frames) = first_track(&flac, TrackKind::Audio, 1);
  let mut muxer = muxer_into(&output(&corpus, "l43-flac.mp4"), ContainerName::MP4, table);
  let offered = carrying(&audio_of(&frames[0]), 0, new_extradata(&[0; 33]));
  let (refusal, back) = by_container(muxer.write_audio(offered.clone()));
  assert_eq!(refusal, payload(PayloadRule::FlacStreamInfo));
  untouched!(offered, back);

  let (table, frames) = first_track(&source, TrackKind::Audio, 2);
  let stated = table[0].side_data().extradata().len();
  assert!(stated >= 2, "the premise: the track's AudioSpecificConfig");
  // AAC LC at 48 kHz, stereo, its bytes after the first two read as zeros.
  let config = |len: usize| {
    let mut bytes = vec![0x11, 0x90];
    bytes.resize(len, 0);
    bytes
  };
  // HE-AAC's explicit SBR — 24 kHz, its extension 48 kHz, stereo — whose
  // header reserved the output sampling frequency every update rewrites
  // (L72), in a space of its four bytes.
  const HE: i32 = 4;
  let he = [0x2B, 0x11, 0x88, 0x00];
  let target = output(&corpus, "l43-aac.mkv");
  let mut muxer = muxer_into(
    &target,
    ContainerName::MATROSKA,
    audio_with(&source, HE, Some(&he)),
  );
  // Channel configuration 15, past libavcodec's table; and a valid one
  // longer than the space Matroska's header reserved, the track's own.
  for bytes in [vec![0x11, 0xF8], config(he.len() + 1)] {
    let offered = carrying(&audio_of(&frames[0]), 0, new_extradata(&bytes));
    let (refusal, back) = by_container(muxer.write_audio(offered.clone()));
    assert_eq!(refusal, payload(PayloadRule::NewExtradata), "{bytes:?}");
    untouched!(offered, back);
  }
  muxer
    .write_audio(carrying(
      &audio_of(&frames[0]),
      0,
      new_extradata(&config(he.len())),
    ))
    .map_err(|error| error.map_packet(drop))
    .expect("an AudioSpecificConfig within the space");
  muxer.finish().expect("finished");
  let (written, _) = first_track(&target, TrackKind::Audio, 0);
  assert_eq!(
    written[0].side_data().extradata(),
    &config(he.len())[..],
    "read back"
  );
  // The track's own AAC LC configuration, libavcodec's encoder's, has the
  // header reserve no output sampling frequency: one within the space is
  // refused all the same, the update running over the track entry.
  let mut muxer = muxer_into(
    &output(&corpus, "l43-lc.mkv"),
    ContainerName::MATROSKA,
    table,
  );
  let offered = carrying(&audio_of(&frames[0]), 0, new_extradata(&config(stated)));
  let (refusal, back) = by_container(muxer.write_audio(offered.clone()));
  assert_eq!(refusal, payload(PayloadRule::NewExtradata), "AAC LC's");
  untouched!(offered, back);
}

/// A chapter of `id` from `start` to `end` milliseconds, titled where
/// `title` is given.
fn chapter_of(id: i64, start: i64, end: i64, title: Option<&str>) -> Chapter<Ffmpeg> {
  Chapter::<Ffmpeg>::new(id, millis(), ms(start), ms(end)).with_title(title.map(Into::into))
}

/// A chapter's span in milliseconds, as read back.
fn span_ms(chapter: &Chapter<Ffmpeg>) -> (i64, i64) {
  let in_ms = |time: Timestamp| {
    time
      .checked_rescale_with(millis(), Rounding::Nearest)
      .map(|time| time.pts())
      .expect("a chapter time")
  };
  (in_ms(chapter.start()), in_ms(chapter.end()))
}

/// **L44 — what a container adds, reinterprets or drops of the table is
/// reported at open, and reads back so** (table E of the census): MP4's
/// default flag on the first video and audio track where none declares it
/// (`enable_tracks`, `movenc.c` 7883–7923 in FFmpeg 9.0.1); a Matroska
/// attachment of an image type, read back as an attached picture; an MP4
/// cover of no bytes, not written; MP4's chapters, numbered by position and
/// each ended where the next starts, and a chapter without a title refused;
/// Matroska's chapters, renumbered from 1 where one's id is 0.
///
/// PLANT: without `mp4_defaults_added`, the report is silent on the flag
/// both tracks read back with.
#[test]
fn l44_what_a_container_adds_or_drops_of_the_table_is_reported() {
  let Some(corpus) = Corpus::new() else { return };
  let source = corpus.multi_track_mkv();

  // MP4's default selection.
  let mut table = Vec::new();
  let mut firsts = Vec::new();
  for kind in [TrackKind::Video, TrackKind::Audio] {
    let (mut one, frames) = first_track(&source, kind, 1);
    one[0].set_disposition(TrackDisposition::empty());
    table.extend(one);
    firsts.push(frames.into_iter().next().expect("a packet"));
  }
  let target = output(&corpus, "l44.mp4");
  let (mut muxer, report) = FfmpegOwnedMuxer::open(
    File::create(&target).expect("create"),
    ContainerName::MP4,
    table,
    &[],
    PacketLimits::default(),
  )
  .expect("open");
  let added = lines(&report, |line| {
    matches!(line, Uncarried::DispositionAdded { .. })
  });
  assert_eq!(
    added,
    vec![
      Uncarried::DispositionAdded {
        track: TrackIndex::new(0),
        flags: TrackDisposition::DEFAULT
      },
      Uncarried::DispositionAdded {
        track: TrackIndex::new(1),
        flags: TrackDisposition::DEFAULT
      },
    ]
  );
  muxer
    .write_video(VideoTrackPacket::new(
      TrackIndex::new(0),
      video_of(&firsts[0])
        .with_pts(Some(ms(0)))
        .with_dts(Some(ms(0))),
    ))
    .map_err(|error| error.map_packet(drop))
    .expect("a video packet");
  muxer
    .write_audio(AudioTrackPacket::new(
      TrackIndex::new(1),
      audio_of(&firsts[1])
        .with_pts(Some(ms(0)))
        .with_dts(Some(ms(0))),
    ))
    .map_err(|error| error.map_packet(drop))
    .expect("an audio packet");
  muxer.finish().expect("finished");
  for row in rows(&target) {
    assert!(
      row.disposition.contains(TrackDisposition::DEFAULT),
      "{row:?}"
    );
  }

  // An image attachment in Matroska reads back as an attached picture: an
  // MP3's PNG cover, its parameters the picture's, as a file of its own.
  let (cover_codec, cover_ticket) = {
    let demuxer = FfmpegOwnedDemuxer::open(&corpus.cover_art_mp3()).expect("open");
    let row = demuxer
      .tracks()
      .iter()
      .find(|row| row.kind() == TrackKind::Attachment)
      .expect("the cover");
    (row.params().codec(), ticket(row))
  };
  assert_eq!(cover_codec, CodecId::PNG, "the premise: a PNG cover");
  let image = |payload: &[u8], mime: Option<&str>| {
    TrackDescription::attachment(
      millis(),
      AttachmentTrackParams::new(cover_codec),
      AttachedFile::new(AttachmentPacket::new(
        bytes_of(payload),
        AttachmentPacketExtra::new(0),
      ))
      .with_filename(Some("cover.png".into()))
      .with_mime_type(mime.map(Into::into)),
      cover_ticket.clone(),
    )
  };
  let png = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 0];
  let target = output(&corpus, "l44.mkv");
  let (_, video) = first_track(&source, TrackKind::Video, 3);
  let mut table = table_of(&source, TrackKind::Video);
  table.push(image(&png, Some("image/png")));
  let (mut muxer, report) = FfmpegOwnedMuxer::open(
    File::create(&target).expect("create"),
    ContainerName::MATROSKA,
    table,
    &[],
    PacketLimits::default(),
  )
  .expect("open");
  assert_eq!(
    lines(&report, |line| matches!(
      line,
      Uncarried::DispositionAdded { .. }
    )),
    vec![Uncarried::DispositionAdded {
      track: TrackIndex::new(1),
      flags: TrackDisposition::ATTACHED_PIC
    }]
  );
  for (index, packet) in video.iter().enumerate() {
    let at = 40 * index as i64;
    muxer
      .write_video(video_at(&video_of(packet), Some(at), at))
      .map_err(|error| error.map_packet(drop))
      .expect("a packet");
  }
  muxer.finish().expect("finished");
  assert!(
    rows(&target)[1]
      .disposition
      .contains(TrackDisposition::ATTACHED_PIC),
    "read back as an attached picture"
  );

  // An MP4 cover of no bytes: not written, and reported.
  let target = output(&corpus, "l44-cover.mp4");
  let mut table = table_of(&source, TrackKind::Video);
  table.push(image(&[], Some("image/png")));
  let (_, report) = FfmpegOwnedMuxer::open(
    File::create(&target).expect("create"),
    ContainerName::MP4,
    table,
    &[],
    PacketLimits::default(),
  )
  .expect("open");
  assert!(
    report.uncarried().contains(&Uncarried::Attachment {
      track: TrackIndex::new(1)
    }),
    "{report:?}"
  );

  // MP4's chapters: by position, each ending where the next starts.
  let chapters = [
    chapter_of(5, 0, 1000, Some("a")),
    chapter_of(6, 1500, 2000, Some("b")),
  ];
  let target = output(&corpus, "l44-chapters.mp4");
  let (mut muxer, report) = FfmpegOwnedMuxer::open(
    File::create(&target).expect("create"),
    ContainerName::MP4,
    table_of(&source, TrackKind::Video),
    &chapters,
    PacketLimits::default(),
  )
  .expect("open");
  assert_eq!(
    lines(&report, |line| matches!(
      line,
      Uncarried::ChapterId { .. } | Uncarried::ChapterSpan { .. }
    )),
    vec![
      Uncarried::ChapterId { chapter: 0, id: 5 },
      Uncarried::ChapterSpan { chapter: 0 },
      Uncarried::ChapterId { chapter: 1, id: 6 },
    ]
  );
  for (index, packet) in video.iter().enumerate() {
    let at = 1000 * index as i64;
    muxer
      .write_video(video_at(&video_of(packet), Some(at), at))
      .map_err(|error| error.map_packet(drop))
      .expect("a packet");
  }
  muxer.finish().expect("finished");
  let read = self::chapters(&target);
  assert_eq!(
    read
      .iter()
      .map(|chapter| (chapter.id(), span_ms(chapter)))
      .collect::<Vec<_>>(),
    vec![(0, (0, 1500)), (1, (1500, 2000))],
    "numbered by position, the first ending where the second starts"
  );
  assert!(matches!(
    FfmpegOwnedMuxer::open(
      File::create(&target).expect("create"),
      ContainerName::MP4,
      table_of(&source, TrackKind::Video),
      &[chapter_of(0, 0, 1000, None)],
      PacketLimits::default(),
    ),
    Err(MuxOpenError::ChapterWithoutTitle { chapter: 0 })
  ));

  // Matroska renumbers every chapter where one's id is 0.
  let target = output(&corpus, "l44-chapters.mkv");
  let (mut muxer, report) = FfmpegOwnedMuxer::open(
    File::create(&target).expect("create"),
    ContainerName::MATROSKA,
    table_of(&source, TrackKind::Video),
    &[
      chapter_of(0, 0, 1000, Some("a")),
      chapter_of(7, 1000, 2000, Some("b")),
    ],
    PacketLimits::default(),
  )
  .expect("open");
  assert_eq!(
    lines(&report, |line| matches!(line, Uncarried::ChapterId { .. })),
    vec![
      Uncarried::ChapterId { chapter: 0, id: 0 },
      Uncarried::ChapterId { chapter: 1, id: 7 },
    ]
  );
  for (index, packet) in video.iter().enumerate() {
    let at = 40 * index as i64;
    muxer
      .write_video(video_at(&video_of(packet), Some(at), at))
      .map_err(|error| error.map_packet(drop))
      .expect("a packet");
  }
  muxer.finish().expect("finished");
  assert_eq!(
    self::chapters(&target)
      .iter()
      .map(Chapter::id)
      .collect::<Vec<_>>(),
    vec![1, 2]
  );
}

/// The report at open of `table` under `container`, and the muxer.
fn opened(
  target: &Path,
  container: ContainerName,
  table: Vec<TrackDescription<Ffmpeg, FfmpegBytes, CodecTicket>>,
) -> (FfmpegOwnedMuxer<File>, Vec<Uncarried>) {
  let (muxer, report) = FfmpegOwnedMuxer::open(
    File::create(target).expect("create"),
    container,
    table,
    &[],
    PacketLimits::default(),
  )
  .expect("open");
  (muxer, report.uncarried().to_vec())
}

/// **L45 — MP4 spelled in any case is MP4**: libavformat matches a muxer's
/// name case-insensitively (`av_match_name`), so `MP4` and `Mp4` open the
/// muxer named `mp4`, and the muxer is judged by that name — MP4's 32-bit
/// fields refused as under `mp4`, its report the same.
#[test]
fn l45_mp4_spelled_in_any_case_meets_mp4s_rules() {
  let Some(corpus) = Corpus::new() else { return };
  let source = corpus.multi_track_mkv();
  let (_, packets) = first_track(&source, TrackKind::Video, 2);
  let track = TrackIndex::new(0);
  let past = ms(PAST_I32_SECONDS * 1000);
  let (_, canonical) = opened(
    &output(&corpus, "l45-canonical.mp4"),
    ContainerName::MP4,
    table_of(&source, TrackKind::Video),
  );
  for spelling in ["MP4", "Mp4"] {
    let (mut muxer, report) = opened(
      &output(&corpus, &format!("l45-{spelling}.mp4")),
      ContainerName::new(spelling),
      table_of(&source, TrackKind::Video),
    );
    assert_eq!(report, canonical, "{spelling}: the report at open");
    muxer
      .write_video(video_at(&video_of(&packets[0]), Some(0), 0))
      .expect("the first packet");
    let gap = video_at(&video_of(&packets[1]), Some(past.pts()), past.pts());
    let (refusal, back) = by_container(muxer.write_video(gap.clone()));
    assert_eq!(
      refusal,
      FfmpegMuxRefusal::DtsGap { track, dts: past },
      "{spelling}"
    );
    untouched!(gap, back);
    let long = video_at(
      &video_of(&packets[1]).with_duration(Some(past)),
      Some(40),
      40,
    );
    let (refusal, _) = by_container(muxer.write_video(long));
    assert_eq!(
      refusal,
      FfmpegMuxRefusal::DurationTooLong {
        track,
        duration: past
      },
      "{spelling}"
    );
    muxer.finish().expect("finished");
  }
}

/// **L46 — Matroska spelled in capitals is Matroska**: `MATROSKA` opens the
/// muxer named `matroska`, and its presentation rule holds — an AAC packet
/// inside the codec delay before zero written as it is, one below it
/// moving every stream, as libavformat moves it, reported — and its report
/// is the one under `matroska`.
#[test]
fn l46_matroska_spelled_in_capitals_meets_matroskas_rules() {
  let Some(corpus) = Corpus::new() else { return };
  let source = corpus.multi_track_mkv();
  let (_, audio) = first_track(&source, TrackKind::Audio, 1);
  let mut reports = Vec::new();
  for (spelling, container) in [
    ("canonical", ContainerName::MATROSKA),
    ("capitals", ContainerName::new("MATROSKA")),
  ] {
    for (at, moved) in [(-21, false), (-22, true)] {
      let (mut muxer, report) = opened(
        &output(&corpus, &format!("l46-{spelling}{at}.mkv")),
        container.clone(),
        table_of(&source, TrackKind::Audio),
      );
      muxer
        .write_audio(audio_at(&audio_of(&audio[0]), at))
        .map_err(|error| error.map_packet(drop))
        .expect("as Matroska writes it");
      assert_eq!(
        muxer
          .report()
          .uncarried()
          .contains(&Uncarried::TimesShifted),
        moved,
        "{spelling}: at {at}"
      );
      reports.push((report, muxer.report().clone()));
      muxer.finish().expect("finished");
    }
  }
  assert_eq!(reports[..2], reports[2..], "the reports at open and after");
}

/// **L47 — WebM spelled in capitals is WebM**: `WEBM` opens the muxer named
/// `webm`; its report names VP9's superframe filter as under `webm`, and the
/// filter's rule holds — a seventh hidden frame in a row refused.
#[test]
fn l47_webm_spelled_in_capitals_meets_webms_rules() {
  let Some(corpus) = Corpus::new() else { return };
  let source = vp9_webm(&corpus);
  let (_, frames) = first_track(&source, TrackKind::Video, 1);
  let frame = video_of(&frames[0]);
  let hidden = [0x80, 3, 3];
  let track = TrackIndex::new(0);
  let (_, canonical) = opened(
    &output(&corpus, "l47-canonical.webm"),
    ContainerName::WEBM,
    table_of(&source, TrackKind::Video),
  );
  let (mut muxer, report) = opened(
    &output(&corpus, "l47.webm"),
    ContainerName::new("WEBM"),
    table_of(&source, TrackKind::Video),
  );
  assert_eq!(report, canonical, "the report at open");
  assert!(report.contains(&Uncarried::Filtered {
    track,
    filter: AutomaticFilter::Vp9Superframe
  }));
  for index in 0..6 {
    let at = 40 * index;
    muxer
      .write_video(video_at(&video_holding(&frame, &hidden), Some(at), at))
      .map_err(|error| error.map_packet(drop))
      .expect("up to six hidden frames held");
  }
  let seventh = video_at(&video_holding(&frame, &hidden), Some(240), 240);
  let (refusal, back) = by_container(muxer.write_video(seventh.clone()));
  assert_eq!(refusal, payload(PayloadRule::Superframe));
  untouched!(seventh, back);
}

/// A VP9 frame of `len` bytes, `shown` or hidden, the rest `tag`.
fn vp9_frame(len: usize, shown: bool, tag: u8) -> Vec<u8> {
  let mut bytes = vec![tag; len];
  // The frame marker and profile 0, then show-existing, frame type and
  // show bits.
  bytes[0] = if shown { 0x82 } else { 0x80 };
  bytes
}

/// **L48 — IVF joins VP9's hidden frames whether the automatic filters run
/// or not, and the run is bounded either way** (table B of the census):
/// IVF inserts `vp9_superframe` at its start (`ivf_init`, `ivfenc.c` 42),
/// not at a track's first packet, so turning the filters off leaves it in
/// place — and its conversion is reported. With libavcodec's largest packet
/// lowered to 100 bytes (the seam), a third hidden frame of 40, which no
/// frame shown after it could join within 100, is refused by name, the
/// packet untouched; a shown frame of 10 joins the two held, and the file
/// holds their one superframe.
///
/// PLANT: IVF's filter taken as inserted at the first packet admits the
/// third hidden frame.
#[test]
fn l48_ivfs_vp9_superframes_are_bounded_with_the_filters_off() {
  let Some(corpus) = Corpus::new() else { return };
  let source = vp9_webm(&corpus);
  let (_, frames) = first_track(&source, TrackKind::Video, 1);
  let frame = video_of(&frames[0]);
  let track = TrackIndex::new(0);
  let target = output(&corpus, "l48.ivf");
  let (mut muxer, report) = FfmpegOwnedMuxer::open_with(
    File::create(&target).expect("create"),
    ContainerName::new("ivf"),
    table_of(&source, TrackKind::Video),
    &[],
    filters(false).with_lowered_bounds(100, u64::MAX),
  )
  .expect("open");
  assert_eq!(
    report.uncarried(),
    &[
      Uncarried::Filtered {
        track,
        filter: AutomaticFilter::Vp9Superframe
      },
      // IVF writes the presentation time alone, and no key flag (table H).
      Uncarried::Rewritten {
        track,
        rewrite: Rewrite::Durations
      },
      Uncarried::Rewritten {
        track,
        rewrite: Rewrite::SyncFromBitstream
      },
    ],
    "inserted at IVF's start, the filters off"
  );
  for (at, tag) in [(0, 1), (40, 2)] {
    muxer
      .write_video(video_at(
        &video_holding(&frame, &vp9_frame(40, false, tag)),
        Some(at),
        at,
      ))
      .map_err(|error| error.map_packet(drop))
      .expect("held");
  }
  let third = video_at(
    &video_holding(&frame, &vp9_frame(40, false, 3)),
    Some(80),
    80,
  );
  let (refusal, back) = by_container(muxer.write_video(third.clone()));
  assert_eq!(refusal, payload(PayloadRule::SuperframeSize));
  untouched!(third, back);
  muxer
    .write_video(video_at(
      &video_holding(&frame, &vp9_frame(10, true, 9)),
      Some(80),
      80,
    ))
    .map_err(|error| error.map_packet(drop))
    .expect("joins the two held");
  muxer.finish().expect("finished");
  let written = &packets(&target)[0];
  assert_eq!(written.len(), 1, "one superframe");
  let mut joined = [
    vp9_frame(40, false, 1),
    vp9_frame(40, false, 2),
    vp9_frame(10, true, 9),
  ]
  .concat();
  joined.extend([0xC2, 40, 40, 10, 0xC2]);
  assert_eq!(written[0].bytes, joined);
}

/// **L49 — Matroska bounds every block its filters put out, not the packet
/// handed over** (table B): VP9's superframe is one block of every frame it
/// joins. With a block's payload lowered to 100 bytes (the seam), hidden
/// frames of 40 — each within it — are refused at the third, which no frame
/// shown after it could join within the bound, the packet untouched before
/// any of it is taken; a shown frame of 20, which would join the two at
/// 105, is refused too, and one of 10 joins them at 95.
///
/// PLANT: the block bound held to the packet handed over admits the third
/// hidden frame.
#[test]
fn l49_matroska_bounds_the_blocks_its_filters_put_out() {
  let Some(corpus) = Corpus::new() else { return };
  let source = vp9_webm(&corpus);
  let (_, frames) = first_track(&source, TrackKind::Video, 1);
  let frame = video_of(&frames[0]);
  let target = output(&corpus, "l49.mkv");
  let mut muxer = muxer_with(
    &target,
    ContainerName::MATROSKA,
    table_of(&source, TrackKind::Video),
    MuxOptions::new().with_lowered_bounds(u64::MAX, 100),
  );
  for (at, tag) in [(0, 1), (40, 2)] {
    muxer
      .write_video(video_at(
        &video_holding(&frame, &vp9_frame(40, false, tag)),
        Some(at),
        at,
      ))
      .map_err(|error| error.map_packet(drop))
      .expect("held");
  }
  for offered in [vp9_frame(40, false, 3), vp9_frame(20, true, 4)] {
    let packet = video_at(&video_holding(&frame, &offered), Some(80), 80);
    let (refusal, back) = by_container(muxer.write_video(packet.clone()));
    assert_eq!(refusal, payload(PayloadRule::ClusterSize), "{offered:?}");
    untouched!(packet, back);
  }
  muxer
    .write_video(video_at(
      &video_holding(&frame, &vp9_frame(10, true, 9)),
      Some(80),
      80,
    ))
    .map_err(|error| error.map_packet(drop))
    .expect("joins the two held");
  muxer.finish().expect("finished");
  assert_eq!(packets(&target)[0].len(), 1, "one superframe");
}

/// **L50 — AAC's filter in another container is mirrored as it converts**
/// (table B): FLV inserts `aac_adtstoasc` at an AAC track's first packet
/// where it opens with an ADTS header (`flv_check_bitstream`, `flvenc.c`
/// 1491–1493), and the filter consumes a first frame's program config
/// element with no check that it fits. Over the transport stream's AAC,
/// which states no configuration: an element cut a byte short — put out at
/// size −1 — is refused by name, the packet untouched; one that ends its
/// payload is put out empty, the filter's own way, and admitted. A raw
/// first packet inserts no filter, for good: with no configuration, and
/// none brought, it is refused (table H's
/// [`PayloadRule::ConfigurationAbsent`]); with new extradata, taken; an
/// ADTS packet after it would be written with its header in, and is
/// refused ([`PayloadRule::AdtsHeader`]).
///
/// PLANT: FLV taken off AAC's filter sites admits the cut element.
#[test]
fn l50_aacs_filter_is_mirrored_in_another_container() {
  use ffmpeg_next::ffi::AVPacketSideDataType as T;
  let Some(corpus) = Corpus::new() else { return };
  let ts = corpus.transport_stream();
  let (_, frames) = first_track(&ts, TrackKind::Audio, 1);
  let frame = audio_of(&frames[0]);
  let mut muxer = muxer_into(
    &output(&corpus, "l50.flv"),
    ContainerName::new("flv"),
    table_of(&ts, TrackKind::Audio),
  );
  let cut = audio_at(&audio_holding(&frame, &adts_with(&BARE_PCE[..5])), 0);
  let (refusal, back) = by_container(muxer.write_audio(cut.clone()));
  assert_eq!(refusal, payload(PayloadRule::AdtsFrame));
  untouched!(cut, back);
  muxer
    .write_audio(audio_at(&audio_holding(&frame, &adts_with(&BARE_PCE)), 0))
    .map_err(|error| error.map_packet(drop))
    .expect("an element that ends its payload, put out empty");
  muxer.finish().expect("finished");

  let mut muxer = muxer_into(
    &output(&corpus, "l50-raw.flv"),
    ContainerName::new("flv"),
    table_of(&ts, TrackKind::Audio),
  );
  let bare = audio_at(&audio_holding(&frame, &[0x21, 0x10, 0x05]), 0);
  let (refusal, back) = by_container(muxer.write_audio(bare.clone()));
  assert_eq!(
    refusal,
    payload(PayloadRule::ConfigurationAbsent),
    "none brought"
  );
  untouched!(bare, back);
  let mut configuring = audio_holding(&frame, &[0x21, 0x10, 0x05]);
  configuring
    .extra_mut()
    .set_side_data(vec![side(T::AV_PKT_DATA_NEW_EXTRADATA, &[0x11, 0x90])]);
  muxer
    .write_audio(audio_at(&configuring, 0))
    .map_err(|error| error.map_packet(drop))
    .expect("a raw first packet bringing its configuration");
  let framed = audio_at(&audio_holding(&frame, &adts_with(&BARE_PCE[..5])), 21);
  let (refusal, back) = by_container(muxer.write_audio(framed.clone()));
  assert_eq!(
    refusal,
    payload(PayloadRule::AdtsHeader),
    "no filter converts it"
  );
  untouched!(framed, back);
  muxer.finish().expect("finished");
}

/// `fields`, each `(value, width)`, written most significant bit first and
/// padded with zeros to a byte; a field wider than 64 bits is zeros above.
fn bit_payload(fields: &[(u64, u32)]) -> Vec<u8> {
  let mut bits = Vec::new();
  for &(value, width) in fields {
    for bit in (0..width).rev() {
      bits.push(bit < 64 && value >> bit & 1 == 1);
    }
  }
  bits
    .chunks(8)
    .map(|byte| {
      byte
        .iter()
        .chain(core::iter::repeat(&false))
        .take(8)
        .fold(0u8, |acc, &bit| acc << 1 | u8::from(bit))
    })
    .collect()
}

/// **L51 — HDR10+ reaches Matroska exactly where libavutil writes and reads
/// it back whole** (table A of the census; Codex R4's findings 2 and 7). Two
/// windows with no peak table, no percentile and every tone mapping and
/// saturation flag zero — 360 bits declared, 358 written, in 45 bytes
/// either way, the reader reading the two missing flags from the padding's
/// zeros — is admitted, and reads back as itself; one window with a 3 × 3
/// targeted peak table and neither flag — 169 bits declared, 168 written,
/// its 22nd byte never written — is refused by name, the packet untouched.
#[test]
fn l51_hdr10_plus_reaches_matroska_where_libavutil_round_trips_it() {
  use ffmpeg_next::ffi::AVPacketSideDataType as T;
  use mediaio_ffmpeg::hdr_metadata::hdr10_plus_from_t35;
  let Some(corpus) = Corpus::new() else { return };
  let source = corpus.multi_track_mkv();
  let (_, video) = first_track(&source, TrackKind::Video, 2);
  let with = |index: usize, entry: Option<&[u8]>| {
    let at = 40 * index as i64;
    let mut packet = video_of(&video[index])
      .with_pts(Some(ms(at)))
      .with_dts(Some(ms(at)));
    packet.extra_mut().set_side_data(
      entry
        .map(|entry| side(T::AV_PKT_DATA_DYNAMIC_HDR10_PLUS, entry))
        .into_iter()
        .collect(),
    );
    VideoTrackPacket::new(TrackIndex::new(0), packet)
  };
  // The version, two windows, the second's geometry, the luminance and no
  // peak table, two windows' statistics, no mastering table, then nothing.
  let zero_windows = hdr10_plus_from_t35(&bit_payload(&[
    (1, 8),
    (2, 2),
    (0, 153),
    (0, 27),
    (0, 1),
    (0, 82),
    (0, 82),
    (0, 1),
  ]))
  .expect("libavutil parses the two windows");
  // The version, one window, the luminance, a 3 × 3 targeted peak table,
  // the window's statistics, no mastering table, then its two flags clear.
  let unwritten = hdr10_plus_from_t35(&bit_payload(&[
    (1, 8),
    (1, 2),
    (0, 27),
    (1, 1),
    (3, 5),
    (3, 5),
    (0, 36),
    (0, 82),
    (0, 1),
    (0, 2),
  ]))
  .expect("libavutil parses the window");
  let target = output(&corpus, "l51.mkv");
  let mut muxer = muxer_into(
    &target,
    ContainerName::MATROSKA,
    table_of(&source, TrackKind::Video),
  );
  let refused = with(0, Some(&unwritten));
  let (refusal, back) = by_container(muxer.write_video(refused.clone()));
  assert_eq!(refusal, payload(PayloadRule::DynamicHdrPlus));
  untouched!(refused, back);
  muxer
    .write_video(with(0, Some(&zero_windows)))
    .map_err(|error| error.map_packet(drop))
    .expect("two windows of nothing, written whole");
  muxer
    .write_video(with(1, None))
    .map_err(|error| error.map_packet(drop))
    .expect("a packet");
  muxer.finish().expect("finished");
  assert!(
    side_data_of(&target)[0].contains(&(T::AV_PKT_DATA_DYNAMIC_HDR10_PLUS as i32, zero_windows)),
    "the two windows read back as themselves"
  );
}

/// `chapters` in seconds: each `(start, end)`, titled.
fn chapters_s(spans: &[(i64, i64)]) -> Vec<Chapter<Ffmpeg>> {
  spans
    .iter()
    .enumerate()
    .map(|(index, &(start, end))| {
      chapter_of(index as i64, start * 1000, end * 1000, Some("chapter"))
    })
    .collect()
}

/// **L52 — MP4's chapter track's samples are counted in the movie
/// timescale its streams make, and a sample MP4 would refuse or move is
/// refused at open** (table C′ of the census; Codex R4's finding 6). MP4
/// counts its chapters in `av_gcd_q` of its streams' time bases — the
/// transport stream's 90 kHz here (`mov_init`, `movenc.c` 8427–8446) — and
/// `check_pkt` refuses a chapter sample past `INT_MAX` ticks, an answer
/// `mov_create_chapter_track` ignores (7807): a chapter of 23 861 s is
/// refused, one of 23 860 s opens and reads back counted in 1/90000; a
/// chapter starting `INT_MAX` ticks or more after the last is refused.
///
/// PLANT: the movie timescale taken as 1000, as before, opens the chapter
/// of 23 861 s.
#[test]
fn l52_mp4s_chapter_samples_are_counted_in_its_movie_timescale() {
  let Some(corpus) = Corpus::new() else { return };
  let ts = corpus.transport_stream();
  let (_, frames) = first_track(&ts, TrackKind::Audio, 1);
  let open = |name: &str, chapters: &[Chapter<Ffmpeg>]| {
    FfmpegOwnedMuxer::open(
      File::create(output(&corpus, name)).expect("create"),
      ContainerName::MP4,
      table_of(&ts, TrackKind::Audio),
      chapters,
      PacketLimits::default(),
    )
  };
  assert!(matches!(
    open("l52-long.mp4", &chapters_s(&[(0, 23_861)])),
    Err(MuxOpenError::ChapterSample { chapter: 0 })
  ));
  assert!(matches!(
    open("l52-gap.mp4", &chapters_s(&[(0, 10), (23_861, 23_862)])),
    Err(MuxOpenError::ChapterSample { chapter: 1 })
  ));
  let (mut muxer, _) = open("l52.mp4", &chapters_s(&[(0, 23_860)])).expect("open");
  muxer
    .write_audio(audio_at(&audio_of(&frames[0]), 0))
    .map_err(|error| error.map_packet(drop))
    .expect("a frame");
  muxer.finish().expect("finished");
  let read = self::chapters(&output(&corpus, "l52.mp4"));
  assert_eq!(read.len(), 1);
  assert_eq!(
    read[0].timebase().den().get(),
    90_000,
    "the movie timescale"
  );
  assert_eq!(span_ms(&read[0]).0, 0);
}

/// A subtitle packet of `packet`'s at `at` ticks of `base`, lasting
/// `duration` of them.
fn subtitle_ticks(
  packet: &Subtitle,
  base: Timebase,
  at: i64,
  duration: i64,
) -> SubtitleTrackPacket<Ffmpeg, FfmpegBytes> {
  SubtitleTrackPacket::new(
    TrackIndex::new(0),
    packet
      .clone()
      .with_pts(Some(Timestamp::new(at, base)))
      .with_duration(Some(Timestamp::new(duration, base))),
  )
}

/// **L53 — MP4's `mov_text` cues are judged from the samples MP4 writes
/// around them** (table C′): an empty sample at zero before the first cue
/// and at each cue's end (`movenc.c` 7625–7656), from which `check_pkt`
/// counts the next one's gap, in the track's own ticks. A first cue 2^31
/// ticks after zero is refused — MP4 would move it; a cue of `INT_MAX`
/// ticks is refused — its end sample would be moved; a cue `INT_MAX` − 1
/// ticks after the last one ends is admitted, as MP4 writes it, and reads
/// back at its time.
///
/// PLANT: the gap counted from the last cue's decode time, as before,
/// admits the first cue 2^31 ticks after zero.
#[test]
fn l53_mov_text_cues_are_judged_from_the_samples_mp4_writes_around_them() {
  let Some(corpus) = Corpus::new() else { return };
  let source = mov_text_mp4(&corpus);
  let (table, cues) = first_track(&source, TrackKind::Subtitle, 1);
  let base = table[0].timebase();
  let cue = subtitle_of(&cues[0]);
  let track = TrackIndex::new(0);
  let at = |ticks: i64| Timestamp::new(ticks, base);
  let (far, edge) = (1i64 << 31, i64::from(i32::MAX));
  let mut muxer = muxer_into(
    &output(&corpus, "l53-far.mp4"),
    ContainerName::MP4,
    table_of(&source, TrackKind::Subtitle),
  );
  let first = subtitle_ticks(&cue, base, far, 1000);
  let (refusal, back) = by_container(muxer.write_subtitle(first.clone()));
  assert_eq!(
    refusal,
    FfmpegMuxRefusal::DtsGap {
      track,
      dts: at(far)
    }
  );
  untouched!(first, back);
  let (refusal, _) = by_container(muxer.write_subtitle(subtitle_ticks(&cue, base, 0, edge)));
  assert_eq!(
    refusal,
    FfmpegMuxRefusal::DurationTooLong {
      track,
      duration: at(edge)
    }
  );
  muxer.finish().expect("finished");

  let target = output(&corpus, "l53.mp4");
  let mut muxer = muxer_into(&target, ContainerName::MP4, table);
  let late = 1000 + edge - 1;
  for (start, duration) in [(0, 1000), (late, 10)] {
    muxer
      .write_subtitle(subtitle_ticks(&cue, base, start, duration))
      .map_err(|error| error.map_packet(drop))
      .expect("a cue MP4 writes as it is");
  }
  muxer.finish().expect("finished");
  assert!(
    packets(&target)[0]
      .iter()
      .any(|seen| seen.pts == Some(at(late))),
    "the late cue reads back at its time"
  );
}

/// **L54 — MP4's TTML paragraphs are judged by the one sample MP4 joins
/// them into** (table C′): from zero to the latest end, at the trailer
/// (`movenc_ttml.c` 236–317), which `check_pkt` refuses past `INT_MAX`
/// ticks, failing the trailer and the file. Over a track configured as
/// libavcodec's TTML encoder configures its own — whose paragraphs MP4
/// joins: a paragraph ending at `INT_MAX` ticks is admitted, and the file
/// finishes; one at `INT_MAX` + 1 is refused by name, the packet untouched.
///
/// PLANT: without the joined sample's rule, the paragraph past it is
/// admitted, and `finish` fails the file.
#[test]
fn l54_mp4s_ttml_is_judged_by_the_sample_it_joins() {
  let Some(corpus) = Corpus::new() else { return };
  let source = ttml_mp4(&corpus);
  let (mut table, paragraphs) = first_track(&source, TrackKind::Subtitle, 1);
  table[0].set_extradata(Some(bytes_of(b"lavc-ttmlenc")));
  let base = table[0].timebase();
  let paragraph = subtitle_holding(&subtitle_of(&paragraphs[0]), b"hello");
  let track = TrackIndex::new(0);
  let edge = i64::from(i32::MAX);
  let mut muxer = muxer_into(&output(&corpus, "l54.mp4"), ContainerName::MP4, table);
  let past = subtitle_ticks(&paragraph, base, edge + 1, 0);
  let (refusal, back) = by_container(muxer.write_subtitle(past.clone()));
  assert_eq!(
    refusal,
    FfmpegMuxRefusal::DocumentSpan {
      track,
      pts: Timestamp::new(edge + 1, base)
    }
  );
  untouched!(past, back);
  muxer
    .write_subtitle(subtitle_ticks(&paragraph, base, edge, 0))
    .map_err(|error| error.map_packet(drop))
    .expect("a paragraph ending at INT_MAX");
  muxer.finish().expect("the joined sample, INT_MAX long");
}

/// `path`'s every track, each attachment with its file.
fn table_with_files(path: &Path) -> Vec<TrackDescription<Ffmpeg, FfmpegBytes, CodecTicket>> {
  let mut demuxer = FfmpegOwnedDemuxer::open(path).expect("open the source");
  let mut files: Vec<Option<_>> = demuxer.tracks().iter().map(|_| None).collect();
  while let Some(packet) = demuxer.next_packet().expect("a packet or the end") {
    if let DemuxedPacket::Attachment(attachment) = packet {
      let (track, payload) = attachment.into_parts();
      files[track.get()] = Some(payload);
    }
  }
  demuxer
    .tracks()
    .iter()
    .zip(files)
    .map(|(row, file)| {
      TrackDescription::from_track_info(&**row, file, ticket(row)).expect("a description")
    })
    .collect()
}

/// **L55 — what a packet's builder would refuse is refused by name, the
/// packet handed back** (Q858 R1): the base's one judgment of what a packet
/// alone decides (`check_packet_build`), which the copy road's builder
/// makes only after the muxer took the packet. Into Matroska: 300 KiB of
/// side data, past the 256 KiB a packet carries; a side-data kind libavcodec
/// does not name; a packet marked trusted; and, under limits of 16 bytes, a
/// 40-byte payload. MP4's cover, whose packet is built after the header,
/// is refused at open, nothing written.
///
/// PLANT: without the judgment in `admit`, the side data reaches the
/// builder, and the write is a fault, the packet lost.
#[test]
fn l55_what_a_packets_builder_refuses_is_refused_with_the_packet_back() {
  use ffmpeg_next::ffi::AVPacketSideDataType as T;
  let Some(corpus) = Corpus::new() else { return };
  let source = corpus.multi_track_mkv();
  let (_, video) = first_track(&source, TrackKind::Video, 1);
  let frame = video_of(&video[0]);
  let with = |packet: Video| VideoTrackPacket::new(TrackIndex::new(0), packet);
  let mut muxer = muxer_into(
    &output(&corpus, "l55.mkv"),
    ContainerName::MATROSKA,
    table_of(&source, TrackKind::Video),
  );
  let mut bulky = frame.clone().with_pts(Some(ms(0))).with_dts(Some(ms(0)));
  bulky
    .extra_mut()
    .set_side_data(vec![side(T::AV_PKT_DATA_A53_CC, &vec![0; 300 * 1024])]);
  let mut unnamed = frame.clone().with_pts(Some(ms(0))).with_dts(Some(ms(0)));
  unnamed
    .extra_mut()
    .set_side_data(vec![SideDataEntry::new(9999, bytes_of(&[1]))]);
  let trusted = frame
    .clone()
    .with_pts(Some(ms(0)))
    .with_dts(Some(ms(0)))
    .with_flags(PacketFlags::from_bits_retain(
      mediaio_ffmpeg::buffer::TRUSTED_BIT,
    ));
  for (packet, rule) in [
    (bulky, PayloadRule::SideDataBudget),
    (unnamed, PayloadRule::SideDataUnnamed),
    (trusted, PayloadRule::Trusted),
  ] {
    let offered = with(packet);
    let (refusal, back) = by_container(muxer.write_video(offered.clone()));
    assert_eq!(refusal, payload(rule));
    untouched!(offered, back);
  }
  muxer.finish().expect("finished");

  let small = PacketLimits::default().with_max_packet_bytes(16);
  let mut muxer = muxer_with(
    &output(&corpus, "l55-small.mkv"),
    ContainerName::MATROSKA,
    table_of(&source, TrackKind::Video),
    MuxOptions::new().with_limits(small),
  );
  let large = with(
    video_holding(&frame, &[0x42; 40])
      .with_pts(Some(ms(0)))
      .with_dts(Some(ms(0))),
  );
  let (refusal, back) = by_container(muxer.write_video(large.clone()));
  assert_eq!(refusal, payload(PayloadRule::PayloadTooLarge));
  untouched!(large, back);

  let cover = corpus.cover_art_mp3();
  let target = output(&corpus, "l55-cover.mp4");
  let opened = FfmpegOwnedMuxer::open(
    File::create(&target).expect("create"),
    ContainerName::MP4,
    table_with_files(&cover),
    &[],
    small,
  );
  assert!(matches!(
    opened,
    Err(MuxOpenError::Cover {
      rule: PayloadRule::PayloadTooLarge,
      ..
    })
  ));
  assert_eq!(
    std::fs::metadata(&target).expect("the file").len(),
    0,
    "nothing written"
  );
}

/// **L56 — the packets an automatic filter still holds at the end are
/// reported, with the report `finish_into_sink` hands back** (Q858 R2):
/// VP9's two hidden frames, which no later frame shows, are dropped by the
/// filter's flush (`vp9_superframe.c` 198–207); a stream ending on a frame
/// shown leaves none.
///
/// PLANT: without the held lines, the report is silent on the two frames.
#[test]
fn l56_what_a_filter_still_holds_at_the_end_is_reported() {
  let Some(corpus) = Corpus::new() else { return };
  let source = vp9_webm(&corpus);
  let (_, frames) = first_track(&source, TrackKind::Video, 1);
  let frame = video_of(&frames[0]);
  let track = TrackIndex::new(0);
  for (name, ends_shown) in [("l56-held.mkv", false), ("l56-shown.mkv", true)] {
    let mut muxer = muxer_into(
      &output(&corpus, name),
      ContainerName::MATROSKA,
      table_of(&source, TrackKind::Video),
    );
    let mut frames = vec![
      vp9_frame(4, true, 1),
      vp9_frame(4, false, 2),
      vp9_frame(4, false, 3),
    ];
    if ends_shown {
      frames.push(vp9_frame(4, true, 4));
    }
    for (index, bytes) in frames.iter().enumerate() {
      let at = 40 * index as i64;
      muxer
        .write_video(video_at(&video_holding(&frame, bytes), Some(at), at))
        .map_err(|error| error.map_packet(drop))
        .expect("a frame the filter takes");
    }
    let (_, report) = muxer.finish_into_sink().expect("finished");
    let held: Vec<_> = report
      .uncarried()
      .iter()
      .filter(|line| matches!(line, Uncarried::Held { .. }))
      .cloned()
      .collect();
    if ends_shown {
      assert_eq!(held, vec![], "{name}");
    } else {
      assert_eq!(
        held,
        vec![Uncarried::Held {
          track,
          filter: AutomaticFilter::Vp9Superframe,
          packets: 2
        }],
        "{name}"
      );
    }
  }
}

// ---------------------------------------------------------------------------
//  Table C″: what MP4 writes, not what it is handed.
// ---------------------------------------------------------------------------

/// The time base MP4 counts a 48 kHz audio track in: its sample rate's
/// (`mov_init`, `movenc.c` 8539).
fn at_48k() -> Timebase {
  Timebase::new(1, NonZeroI32::new(48_000).expect("non-zero"))
}

/// `frame`'s bytes, its `numblkscod` — bits 4–5 of its fifth byte, at
/// 48 kHz — recoded as one block.
fn one_block(frame: &Audio) -> Vec<u8> {
  let mut bytes = frame.data().as_ref().to_vec();
  assert_eq!(bytes[4] >> 6, 0, "the premise: 48 kHz");
  bytes[4] &= !0x30;
  bytes
}

/// `packet` on track 0, decoded and presented at `dts` and lasting
/// `duration` ticks of 48 kHz.
fn audio_ticks(packet: &Audio, dts: i64, duration: i64) -> AudioTrackPacket<Ffmpeg, FfmpegBytes> {
  let at = |ticks: i64| Timestamp::new(ticks, at_48k());
  AudioTrackPacket::new(
    TrackIndex::new(0),
    packet
      .clone()
      .with_pts(Some(at(dts)))
      .with_dts(Some(at(dts)))
      .with_duration(Some(at(duration))),
  )
}

/// The first E-AC-3 frame of the corpus's E-AC-3 file — six blocks, 48 kHz
/// — and its one-block recoding, with the file's path.
fn eac3_frames(corpus: &Corpus) -> (PathBuf, Audio, Audio) {
  let source = audio_file(corpus, "eac3.mkv", "eac3");
  let (_, packets) = first_track(&source, TrackKind::Audio, 1);
  let six = audio_of(&packets[0]);
  let one = audio_holding(&six, &one_block(&six));
  (source, six, one)
}

/// **L57 — MP4 judges an E-AC-3 packet's decode time from the last sample
/// it wrote, which a packet it holds does not move** (table C″ of the
/// census). `check_pkt` counts each packet's gap from the last sample MP4
/// wrote on the track, and none before the first (`movenc.c` 6897–6930);
/// MP4 holds a frame of fewer than six blocks, writing nothing, until six
/// make one sample at the first one's decode time (`handle_eac3`,
/// 577–606). Six one-block packets 0.5 × 10⁹ ticks apart are taken — none
/// is judged before the first sample — and make the sample at 0; a packet
/// at 3 × 10⁹ is refused, 3 × 10⁹ after that sample though 0.5 × 10⁹ after
/// the packet before it. A run opened at 2.0 × 10⁹ moves nothing until it
/// is written: a packet at 2.2 × 10⁹ is refused, that far after the sample
/// at 0; once the run is written at 2.0 × 10⁹, a packet at 4.0 × 10⁹ is
/// taken, 2.0 × 10⁹ after it.
///
/// PLANT: the reference advanced on a held packet, as before: the packet at
/// 3 × 10⁹ is taken, and MP4 would write it one tick after the sample at 0,
/// its presentation time dropped.
#[test]
fn l57_mp4_judges_e_ac3_from_the_last_sample_it_wrote() {
  let Some(corpus) = Corpus::new() else { return };
  let (source, _, one) = eac3_frames(&corpus);
  let track = TrackIndex::new(0);
  let taken = |muxer: &mut FfmpegOwnedMuxer<File>, dts: i64, what: &str| {
    muxer
      .write_audio(audio_ticks(&one, dts, 256))
      .map_err(|error| error.map_packet(drop))
      .expect(what);
  };
  let gap = |muxer: &mut FfmpegOwnedMuxer<File>, dts: i64| {
    let offered = audio_ticks(&one, dts, 256);
    let (refusal, back) = by_container(muxer.write_audio(offered.clone()));
    assert_eq!(
      refusal,
      FfmpegMuxRefusal::DtsGap {
        track,
        dts: Timestamp::new(dts, at_48k())
      }
    );
    untouched!(offered, back);
  };
  let step = 500_000_000;
  let mut muxer = muxer_into(
    &output(&corpus, "l57.mp4"),
    ContainerName::MP4,
    table_of(&source, TrackKind::Audio),
  );
  for index in 0..6 {
    taken(&mut muxer, index * step, "held, then joined at 0");
  }
  gap(&mut muxer, 6 * step);
  muxer.finish().expect("finished");

  let mut muxer = muxer_into(
    &output(&corpus, "l57-run.mp4"),
    ContainerName::MP4,
    table_of(&source, TrackKind::Audio),
  );
  for dts in 0..6 {
    taken(&mut muxer, dts, "the sample at 0");
  }
  let run = 2_000_000_000;
  taken(&mut muxer, run, "a run opened 2.0e9 after the sample at 0");
  gap(&mut muxer, 2_200_000_000);
  for dts in 2_100_000_000..2_100_000_005 {
    taken(&mut muxer, dts, "the run, written at 2.0e9");
  }
  for dts in 4_000_000_000..4_000_000_006 {
    taken(&mut muxer, dts, "2.0e9 after the run's sample");
  }
  muxer.finish().expect("finished");
}

/// **L58 — MP4 joins E-AC-3 frames only within the largest packet
/// `av_grow_packet` grows** (table C″): it grows the sample it joins by
/// each frame, and refuses one that would take it past `INT_MAX` less the
/// padding after taking it (`handle_eac3`, `movenc.c` 592–595;
/// `libavcodec/packet.c` 121–127). The bound lowered, six one-block frames
/// that come to it are joined; one byte less, the sixth is refused, by
/// name, untouched.
///
/// PLANT: without the bound, the sixth frame is taken.
#[test]
fn l58_mp4_joins_e_ac3_within_the_largest_packet() {
  let Some(corpus) = Corpus::new() else { return };
  let (source, _, one) = eac3_frames(&corpus);
  let sum = 6 * one.data().as_ref().len() as u64;
  for (name, bound, sixth_taken) in [("l58.mp4", sum - 1, true), ("l58-past.mp4", sum - 2, false)] {
    let mut muxer = muxer_with(
      &output(&corpus, name),
      ContainerName::MP4,
      table_of(&source, TrackKind::Audio),
      MuxOptions::new().with_lowered_bounds(bound, u64::MAX),
    );
    for dts in 0..5 {
      muxer
        .write_audio(audio_ticks(&one, dts * 256, 256))
        .map_err(|error| error.map_packet(drop))
        .expect("held");
    }
    let sixth = audio_ticks(&one, 5 * 256, 256);
    if sixth_taken {
      muxer
        .write_audio(sixth)
        .map_err(|error| error.map_packet(drop))
        .expect("the sample at the bound");
      muxer.finish().expect("finished");
    } else {
      let (refusal, back) = by_container(muxer.write_audio(sixth.clone()));
      assert_eq!(refusal, payload(PayloadRule::Ac3SampleSize), "{name}");
      untouched!(sixth, back);
    }
  }
}

/// **L59 — MP4 refuses at `finish` an E-AC-3 track whose last sample, a
/// joined one, lasts past `INT_MAX` ticks** (table C″): the joined
/// sample's duration is its packets' sum (`handle_eac3`, `movenc.c` 597),
/// and the trailer reads the last sample's from it and asserts it fits
/// (`get_cluster_duration`, 1248–1256), ending the process. Six one-block
/// packets of 400 × 10⁶ ticks each, 2.4 × 10⁹ in all: as the last sample,
/// `finish` refuses by name before the trailer runs; with a six-block
/// frame after them, the file finishes.
///
/// PLANT: without the check, FFmpeg's assertion ends the test process.
#[test]
fn l59_mp4_refuses_a_last_joined_sample_past_int_max() {
  let Some(corpus) = Corpus::new() else { return };
  let (source, six, one) = eac3_frames(&corpus);
  let long = 400_000_000;
  for (name, followed) in [("l59.mp4", false), ("l59-followed.mp4", true)] {
    let mut muxer = muxer_into(
      &output(&corpus, name),
      ContainerName::MP4,
      table_of(&source, TrackKind::Audio),
    );
    for index in 0..6 {
      muxer
        .write_audio(audio_ticks(&one, index * long, long))
        .map_err(|error| error.map_packet(drop))
        .expect("held, each within INT_MAX");
    }
    if followed {
      muxer
        .write_audio(audio_ticks(&six, 2_100_000_000, 1536))
        .map_err(|error| error.map_packet(drop))
        .expect("a sample of its own after the joined one");
      muxer.finish().expect("finished");
    } else {
      let fault = muxer.finish().expect_err("the last sample past INT_MAX");
      assert_eq!(fault.stage(), MuxStage::Trailer);
      assert_eq!(
        fault.refusal(),
        Some(FfmpegMuxRefusal::LastSampleTooLong {
          track: TrackIndex::new(0)
        })
      );
      assert!(fault.error().is_none(), "no account of the context's");
    }
  }
}

/// **L60 — an E-AC-3 track whose last sample MP4 joins ends where its last
/// packet's duration ends it, and the report says so at `finish`** (table
/// C″): MP4 ends the track's edit list at its last sample's decode time and
/// the duration of the packet it was handed last (`movenc.c` 6940,
/// 7329–7342), so a joined sample is presented short of its frames. Twelve
/// abutting one-block packets: their durations are reported, by the report
/// `finish_into_sink` hands back; two abutting six-block frames: nothing.
///
/// PLANT: without the end's line, the report is silent.
#[test]
fn l60_a_last_joined_sample_is_reported_cut_short() {
  let Some(corpus) = Corpus::new() else { return };
  let (source, six, one) = eac3_frames(&corpus);
  let durations = |report: &mediaencode_ffmpeg::MuxReport| {
    report
      .uncarried()
      .iter()
      .filter(|line| {
        matches!(
          line,
          Uncarried::Rewritten {
            rewrite: Rewrite::Durations,
            ..
          }
        )
      })
      .count()
  };
  for (name, frame, blocks, count, reported) in [
    ("l60.mp4", &one, 256, 12, 1),
    ("l60-six.mp4", &six, 1536, 2, 0),
  ] {
    let mut muxer = muxer_into(
      &output(&corpus, name),
      ContainerName::MP4,
      table_of(&source, TrackKind::Audio),
    );
    for index in 0..count {
      muxer
        .write_audio(audio_ticks(frame, index * blocks, blocks))
        .map_err(|error| error.map_packet(drop))
        .expect("an abutting packet");
    }
    assert_eq!(durations(muxer.report()), 0, "{name}: samples that abut");
    let (_, report) = muxer.finish_into_sink().expect("finished");
    assert_eq!(durations(&report), reported, "{name}");
  }
}

/// **L61 — the blocks an E-AC-3 track leaves short of a sample at its end
/// are dropped, the joining reported at open and the run at `finish`**
/// (table C″; [`Rewrite::Ac3Parsed`], [`Uncarried::Unjoined`]): MP4 frees
/// the run it holds without writing it (`mov_free`, `movenc.c`
/// 7951–7955). Two six-block frames and three one-block ones read back as
/// the two, the three counted.
///
/// PLANT: the run unreported at `finish`: no line counts the three.
#[test]
fn l61_e_ac3_blocks_short_of_a_sample_at_the_end_are_dropped() {
  let Some(corpus) = Corpus::new() else { return };
  let (source, six, one) = eac3_frames(&corpus);
  let target = output(&corpus, "l61.mp4");
  let mut muxer = muxer_into(
    &target,
    ContainerName::MP4,
    table_of(&source, TrackKind::Audio),
  );
  assert!(muxer.report().uncarried().contains(&Uncarried::Rewritten {
    track: TrackIndex::new(0),
    rewrite: Rewrite::Ac3Parsed,
  }));
  for (frame, dts) in [
    (&six, 0),
    (&six, 1536),
    (&one, 3072),
    (&one, 3328),
    (&one, 3584),
  ] {
    muxer
      .write_audio(audio_ticks(frame, dts, 256))
      .map_err(|error| error.map_packet(drop))
      .expect("a frame");
  }
  assert!(
    !muxer
      .report()
      .uncarried()
      .iter()
      .any(|line| matches!(line, Uncarried::Unjoined { .. })),
    "not before the end decides"
  );
  let (_, report) = muxer.finish_into_sink().expect("finished");
  assert_eq!(
    lines(&report, |line| matches!(line, Uncarried::Unjoined { .. })),
    vec![Uncarried::Unjoined {
      track: TrackIndex::new(0),
      packets: 3,
    }],
    "the run MP4 frees, counted"
  );
  assert_eq!(packets(&target)[0].len(), 2, "the two six-block frames");
}

// ---------------------------------------------------------------------------
//  E″: the filters FFmpeg decides, named where it decides them.
// ---------------------------------------------------------------------------

/// The filter lines of `report`.
fn filtered(report: &mediaencode_ffmpeg::MuxReport) -> Vec<Uncarried> {
  lines(report, |line| matches!(line, Uncarried::Filtered { .. }))
}

/// **L62 — FLV's AAC filter is named for its track at the packet that
/// inserts it, once, and never where none is** (E″ of the census): its
/// `check_bitstream` inserts `aac_adtstoasc` at a track's first packet
/// where it opens with an ADTS header (`flvenc.c` 1491–1493; `mux.c`
/// 1063–1068). LATM's does the same (`latmenc.c` 255–257), and is not
/// audited (L73). A transport stream's ADTS
/// frames: nothing is named at open, one line at the first frame's write,
/// none more at the second. A configured track's raw AAC: none.
///
/// PLANT: without the transition's line, the report names no filter for
/// the frames it converts.
#[test]
fn l62_flv_names_aacs_filter_at_the_packet_that_inserts_it() {
  let Some(corpus) = Corpus::new() else { return };
  let ts = corpus.transport_stream();
  let (_, frames) = first_track(&ts, TrackKind::Audio, 2);
  let mkv = audio_file(&corpus, "aac.mkv", "aac");
  let (_, raw) = first_track(&mkv, TrackKind::Audio, 1);
  let named = vec![Uncarried::Filtered {
    track: TrackIndex::new(0),
    filter: AutomaticFilter::AacAdtsToAsc,
  }];
  for container in ["flv"] {
    let mut muxer = muxer_into(
      &output(&corpus, &format!("l62.{container}")),
      ContainerName::new(container),
      table_of(&ts, TrackKind::Audio),
    );
    assert_eq!(
      filtered(muxer.report()),
      vec![],
      "{container}: undecided at open"
    );
    for (index, frame) in frames.iter().enumerate() {
      muxer
        .write_audio(audio_at(&audio_of(frame), 21 * index as i64))
        .map_err(|error| error.map_packet(drop))
        .expect("an ADTS frame the filter converts");
      assert_eq!(filtered(muxer.report()), named, "{container}: once");
    }
    muxer.finish().expect("finished");

    let mut muxer = muxer_into(
      &output(&corpus, &format!("l62-raw.{container}")),
      ContainerName::new(container),
      table_of(&mkv, TrackKind::Audio),
    );
    muxer
      .write_audio(audio_at(&audio_of(&raw[0]), 0))
      .map_err(|error| error.map_packet(drop))
      .expect("raw AAC");
    assert_eq!(filtered(muxer.report()), vec![], "{container}: no filter");
    muxer.finish().expect("finished");
  }
}

/// **L63 — MPEG-TS's Annex B conversion is named at the length-prefixed
/// first packet that inserts it, and not for an Annex B one** (E″): its
/// `check_bitstream` inserts `h264_mp4toannexb` at a first packet of five
/// bytes or more opening on no start code (`mpegtsenc.c` 2310–2334).
///
/// PLANT: without the transition's line, the conversion goes unnamed.
#[test]
fn l63_mpeg_ts_names_its_annex_b_conversion_at_the_packet_that_inserts_it() {
  let Some(corpus) = Corpus::new() else { return };
  let mkv = corpus.multi_track_mkv();
  let (_, prefixed) = first_track(&mkv, TrackKind::Video, 2);
  let ts = corpus.transport_stream();
  let (_, annex_b) = first_track(&ts, TrackKind::Video, 1);
  let mut muxer = muxer_into(
    &output(&corpus, "l63.ts"),
    ContainerName::new("mpegts"),
    table_of(&mkv, TrackKind::Video),
  );
  assert_eq!(filtered(muxer.report()), vec![], "undecided at open");
  for (index, frame) in prefixed.iter().enumerate() {
    let at = 40 * index as i64;
    muxer
      .write_video(video_at(&video_of(frame), Some(at), at))
      .map_err(|error| error.map_packet(drop))
      .expect("a length-prefixed frame the filter converts");
    assert_eq!(
      filtered(muxer.report()),
      vec![Uncarried::Filtered {
        track: TrackIndex::new(0),
        filter: AutomaticFilter::H264Mp4ToAnnexB,
      }],
      "once"
    );
  }
  muxer.finish().expect("finished");

  let mut muxer = muxer_into(
    &output(&corpus, "l63-annex-b.ts"),
    ContainerName::new("mpegts"),
    table_of(&ts, TrackKind::Video),
  );
  muxer
    .write_video(video_at(&video_of(&annex_b[0]), Some(0), 0))
    .map_err(|error| error.map_packet(drop))
    .expect("an Annex B frame");
  assert_eq!(filtered(muxer.report()), vec![], "no filter");
  muxer.finish().expect("finished");
}

/// **L64 — the filters open decides are named at open** (E″): FLV's
/// `extract_extradata` for H.264 with no configuration (`flvenc.c`
/// 1495–1501), none with one; IVF's `av1_metadata`, inserted at its start
/// whatever the option says (`ivfenc.c` 46); the OBU muxer's, at the first
/// packet, with the filters on alone (`rawenc.c` 572).
///
/// PLANT: an unmirrored filter left unnamed: none of these is named.
#[test]
fn l64_the_filters_open_decides_are_named_at_open() {
  let Some(corpus) = Corpus::new() else { return };
  let named = |name: &str, container: &str, table, on: bool| {
    let muxer = muxer_with(
      &output(&corpus, name),
      ContainerName::new(container),
      table,
      filters(on),
    );
    lines(muxer.report(), |line| {
      matches!(line, Uncarried::Filtered { .. })
    })
    .into_iter()
    .map(|line| match line {
      Uncarried::Filtered { filter, .. } => filter,
      other => panic!("{other:?}"),
    })
    .collect::<Vec<_>>()
  };
  let mkv = corpus.multi_track_mkv();
  let bare = table_of(&mkv, TrackKind::Video)
    .into_iter()
    .map(|row| row.with_extradata(Some(bytes_of(&[]))))
    .collect();
  assert_eq!(
    named("l64.flv", "flv", bare, true),
    vec![AutomaticFilter::ExtractExtradata]
  );
  assert_eq!(
    named(
      "l64-configured.flv",
      "flv",
      table_of(&mkv, TrackKind::Video),
      true
    ),
    vec![]
  );
  let av1 = av1_mkv(&corpus);
  for on in [true, false] {
    assert_eq!(
      named("l64.ivf", "ivf", table_of(&av1, TrackKind::Video), on),
      vec![AutomaticFilter::Av1Metadata],
      "IVF, the filters on {on}"
    );
    assert_eq!(
      named("l64.obu", "obu", table_of(&av1, TrackKind::Video), on),
      if on {
        vec![AutomaticFilter::Av1Metadata]
      } else {
        vec![]
      },
      "OBU, the filters on {on}"
    );
  }
}

// ---------------------------------------------------------------------------
//  The refusals check: refused only where FFmpeg refuses, loses or corrupts.
// ---------------------------------------------------------------------------

/// Two `mov_text` cues in MP4, the track counted in milliseconds — as the
/// `ffmpeg` CLI writes it with `-time_base:s 1/1000`.
#[rustfmt::skip]
fn mov_text_ms_mp4(corpus: &Corpus) -> PathBuf {
  let srt = written_text(
    corpus,
    "two.srt",
    "1\n00:00:00,000 --> 00:00:01,000\nhello\n\n2\n00:00:01,000 --> 00:00:02,000\nworld\n\n",
  );
  generated(corpus, "movtext-ms.mp4", &[
    "-i", srt.to_str().expect("utf-8 path"), "-c:s", "mov_text", "-time_base:s", "1/1000",
  ])
}

/// `packet` on `track`, presented at `at` and lasting `duration` ticks of
/// `base`.
fn cue_on(
  track: usize,
  packet: &Subtitle,
  base: Timebase,
  (at, duration): (i64, i64),
) -> SubtitleTrackPacket<Ffmpeg, FfmpegBytes> {
  SubtitleTrackPacket::new(
    TrackIndex::new(track),
    packet
      .clone()
      .with_pts(Some(Timestamp::new(at, base)))
      .with_duration(Some(Timestamp::new(duration, base))),
  )
}

/// The `Rewrite::Durations` lines of `report`.
fn durations_of(report: &mediaencode_ffmpeg::MuxReport) -> usize {
  lines(report, |line| {
    matches!(
      line,
      Uncarried::Rewritten {
        rewrite: Rewrite::Durations,
        ..
      }
    )
  })
  .len()
}

/// **L65 — MP4 takes a `mov_text` cue that starts before the last one
/// ends as it is, and the report says what it keeps of the last one's
/// duration; a first cue before zero it does not keep** (table C″; the
/// refusals check). MP4 writes an empty sample at the last cue's end only
/// where the next cue starts past it (`movenc.c` 7646–7653), so a cue at 50
/// after one at 0 of 100 ticks shortens that one to 50, reported at its
/// write; a cue past the end gets its empty sample. Alone in the file, no
/// other track can make MP4 write an end first. Read back: each sample
/// where MP4 put it. A first cue at −30 MP4 writes, but its reader drops a
/// sample that starts before the edit list's start, and counts the samples
/// after it from there (`mov_fix_index`, `mov.c` 4374–4682): refused, by
/// name, untouched.
///
/// PLANT: `mov_text` strict again — a cue before the end refused in every
/// file — refuses the cue at 50.
#[test]
fn l65_mp4_takes_cues_before_the_last_ones_end_as_it_writes_them() {
  let Some(corpus) = Corpus::new() else { return };
  let source = mov_text_ms_mp4(&corpus);
  let (table, cues) = first_track(&source, TrackKind::Subtitle, 1);
  let base = table[0].timebase();
  assert_eq!(base, millis(), "the premise: milliseconds");
  let cue = subtitle_of(&cues[0]);
  let target = output(&corpus, "l65.mp4");
  let mut muxer = muxer_into(&target, ContainerName::MP4, table);
  muxer
    .write_subtitle(cue_on(0, &cue, base, (0, 100)))
    .map_err(|error| error.map_packet(drop))
    .expect("a cue");
  assert_eq!(durations_of(muxer.report()), 0, "nothing shortened yet");
  muxer
    .write_subtitle(cue_on(0, &cue, base, (50, 100)))
    .map_err(|error| error.map_packet(drop))
    .expect("a cue before the last one ends, which MP4 takes");
  assert_eq!(durations_of(muxer.report()), 1, "the first one shortened");
  muxer
    .write_subtitle(cue_on(0, &cue, base, (200, 10)))
    .map_err(|error| error.map_packet(drop))
    .expect("a cue past the end");
  muxer.finish().expect("finished");
  let at = |ticks| Some(Timestamp::new(ticks, base));
  // The trailer's empty sample, at 210, lies at the end of the edit list
  // MP4 writes for the track, where FFmpeg's reader presents none.
  let seen: Vec<_> = packets(&target)[0].iter().map(|seen| seen.pts).collect();
  assert_eq!(
    seen,
    vec![at(0), at(50), at(150), at(200)],
    "the cues, and the empty sample at the end MP4 marks before the third"
  );

  let mut muxer = muxer_into(
    &output(&corpus, "l65-before-zero.mp4"),
    ContainerName::MP4,
    table_of(&source, TrackKind::Subtitle),
  );
  let before = cue_on(0, &cue, base, (-30, 10));
  let (refusal, back) = by_container(muxer.write_subtitle(before.clone()));
  assert_eq!(
    refusal,
    FfmpegMuxRefusal::CueOrder {
      track: TrackIndex::new(0),
      pts: Timestamp::new(-30, base),
    }
  );
  untouched!(before, back);
}

/// **L66 — beside another track, MP4 takes a cue before the last one's end
/// only where no packet of that track can make it write the end first**
/// (table C″): MP4 marks the last cue's end before any packet whose decode
/// time, in its own track's ticks, passes the end counted in the cue
/// track's (`movenc.c` 7646–7653), and then moves the cue past it, its
/// presentation time dropped (6917–6923) — as the `ffmpeg` CLI does to such
/// a cue beside video, logging "Packet duration: -1000 / dts: 2000 ... is
/// out of range". Beside video MP4 counts in sixteenths of a millisecond,
/// finer than a cue track's milliseconds: refused, by name, untouched.
/// Beside it, a cue track counted in microseconds, every video packet
/// handed within the end and libavformat's 10 s of interleaving: taken;
/// with a video packet handed 11 s ahead, which libavformat may hand on
/// first: refused.
///
/// PLANT: without the neighbours' check, the cue beside finer video is
/// taken, and MP4 moves it.
#[test]
fn l66_beside_another_track_an_early_cue_is_taken_only_where_none_can_end_it_first() {
  let Some(corpus) = Corpus::new() else { return };
  let video_source = corpus.multi_track_mkv();
  let (_, frames) = first_track(&video_source, TrackKind::Video, 3);
  for (name, cues_source, beside_finer) in [
    ("l66.mp4", mov_text_ms_mp4(&corpus), true),
    ("l66-coarse.mp4", mov_text_mp4(&corpus), false),
  ] {
    let (cue_table, cues) = first_track(&cues_source, TrackKind::Subtitle, 1);
    let base = cue_table[0].timebase();
    let per_ms = i64::from(base.den().get()) / 1000;
    let cue = subtitle_of(&cues[0]);
    let mut table = table_of(&video_source, TrackKind::Video);
    table.extend(cue_table);
    let mut muxer = muxer_into(&output(&corpus, name), ContainerName::MP4, table);
    muxer
      .write_video(video_at(&video_of(&frames[0]), Some(0), 0))
      .map_err(|error| error.map_packet(drop))
      .expect("video");
    muxer
      .write_subtitle(cue_on(1, &cue, base, (0, 100 * per_ms)))
      .map_err(|error| error.map_packet(drop))
      .expect("a cue");
    muxer
      .write_video(video_at(&video_of(&frames[1]), Some(40), 40))
      .map_err(|error| error.map_packet(drop))
      .expect("video");
    let early = cue_on(1, &cue, base, (50 * per_ms, 100 * per_ms));
    if beside_finer {
      let (refusal, back) = by_container(muxer.write_subtitle(early.clone()));
      assert_eq!(
        refusal,
        FfmpegMuxRefusal::CueOrder {
          track: TrackIndex::new(1),
          pts: Timestamp::new(50 * per_ms, base),
        },
        "{name}"
      );
      untouched!(early, back);
    } else {
      muxer
        .write_subtitle(early)
        .map_err(|error| error.map_packet(drop))
        .expect("beside coarser video, handed in time");
      muxer
        .write_video(video_at(&video_of(&frames[2]), Some(11_000), 11_000))
        .map_err(|error| error.map_packet(drop))
        .expect("video 11 s ahead");
      let late = cue_on(1, &cue, base, (60 * per_ms, 10 * per_ms));
      let (refusal, back) = by_container(muxer.write_subtitle(late.clone()));
      assert_eq!(
        refusal,
        FfmpegMuxRefusal::CueOrder {
          track: TrackIndex::new(1),
          pts: Timestamp::new(60 * per_ms, base),
        },
        "{name}: a packet handed past the end and the delay"
      );
      untouched!(late, back);
    }
    muxer.finish().expect("finished");
  }
}

/// One ADTS frame of AAC LC at 48 kHz, its channel configuration
/// `channels`, no CRC, `payload` after its header.
fn adts_frame_of(payload: &[u8], channels: u64) -> Vec<u8> {
  let mut frame = adts_with(payload);
  // The channel configuration's three bits straddle the third and fourth
  // bytes (`adts_header.c` 47–52).
  frame[2] = (frame[2] & !1) | u8::from(channels & 4 != 0);
  frame[3] = (frame[3] & 0x3F) | (((channels & 3) as u8) << 6);
  frame
}

/// **L67 — a configured AAC track's first packet decides AAC's filter in
/// MP4 and Matroska, as libavformat decides it** (the refusals check):
/// `mov_check_bitstream` and `mkv_check_bitstream` insert `aac_adtstoasc` at
/// a first packet that opens with an ADTS header, whatever the
/// configuration (`movenc.c` 9143–9145; `matroskaenc.c` 3635–3637), which
/// then converts each ADTS frame and passes raw AAC (`aac_adtstoasc.c`
/// 51–52). Into MP4, an ADTS first frame is taken, the filter named at its
/// write, and reads back as its raw payload; raw and ADTS packets after it
/// are taken. Matroska rewrites the track's configuration with the one the
/// filter makes of the first frame, within the space its header reserved —
/// the stated configuration's length — and refuses one past it after
/// taking the packet (`matroskaenc.c` 3065–3082, 1334–1365); but it
/// rewrites the sampling frequencies with it too, over bytes its header
/// reserved only where the stated configuration had an extension sample
/// rate (3076–3081, 2129–2135), which libavcodec's AAC encoder's has not:
/// the track entry would read back corrupt, so the ADTS first frame is
/// refused, by name, untouched, and a raw one taken with no filter. A first
/// frame whose channels its program config element gives makes one of
/// eight bytes, refused; MP4 makes it a sample description, and takes it.
///
/// PLANT: a configured track taking raw AAC alone, as before: MP4's ADTS
/// first frame is refused; the output sampling frequency held reserved:
/// Matroska's is taken.
#[test]
fn l67_a_configured_aac_tracks_first_packet_decides_its_filter() {
  let Some(corpus) = Corpus::new() else { return };
  let source = corpus.multi_track_mkv();
  let (table, frames) = first_track(&source, TrackKind::Audio, 1);
  let stated = table[0].side_data().extradata().len();
  assert!(
    (2..8).contains(&stated),
    "the premise: a configuration shorter than eight bytes, {stated}"
  );
  let raw = audio_of(&frames[0]);
  let framed = adts_frame_of(raw.data().as_ref(), 2);
  for (container, name) in [
    (ContainerName::MP4, "l67.mp4"),
    (ContainerName::MATROSKA, "l67.mkv"),
  ] {
    let target = output(&corpus, name);
    let mut muxer = muxer_into(
      &target,
      container.clone(),
      table_of(&source, TrackKind::Audio),
    );
    assert_eq!(
      filtered(muxer.report()),
      vec![],
      "{name}: undecided at open"
    );
    if container == ContainerName::MATROSKA {
      // The configuration the filter makes of the first frame Matroska
      // writes over its header's sampling frequencies, which reserved no
      // output one for this configuration's: the track entry would read
      // back corrupt (`matroskaenc.c` 3076–3081, 2129–2135).
      let first = audio_at(&audio_holding(&raw, &framed), 0);
      let (refusal, back) = by_container(muxer.write_audio(first.clone()));
      assert_eq!(refusal, payload(PayloadRule::NewExtradata), "{name}");
      untouched!(first, back);
      muxer
        .write_audio(audio_at(&raw, 0))
        .map_err(|error| error.map_packet(drop))
        .expect("a raw first frame, no filter");
      assert_eq!(filtered(muxer.report()), vec![], "{name}: none inserted");
      muxer.finish().expect("finished");
      continue;
    }
    for (index, bytes) in [&framed, raw.data().as_ref(), &framed]
      .into_iter()
      .enumerate()
    {
      muxer
        .write_audio(audio_at(&audio_holding(&raw, bytes), 21 * index as i64))
        .map_err(|error| error.map_packet(drop))
        .expect("an ADTS first frame inserts the filter, which takes what follows");
    }
    assert_eq!(
      filtered(muxer.report()),
      vec![Uncarried::Filtered {
        track: TrackIndex::new(0),
        filter: AutomaticFilter::AacAdtsToAsc,
      }],
      "{name}"
    );
    muxer.finish().expect("finished");
    let seen = &packets(&target)[0];
    assert_eq!(seen.len(), 3, "{name}");
    assert!(
      seen
        .iter()
        .all(|packet| packet.bytes == raw.data().as_ref()),
      "{name}: each reads back as the raw payload"
    );

    let element = [&BARE_PCE[..], raw.data().as_ref()].concat();
    let offered = audio_at(&audio_holding(&raw, &adts_frame_of(&element, 0)), 0);
    let mut muxer = muxer_into(
      &output(&corpus, &format!("pce-{name}")),
      container.clone(),
      table_of(&source, TrackKind::Audio),
    );
    if container == ContainerName::MATROSKA {
      let (refusal, back) = by_container(muxer.write_audio(offered.clone()));
      assert_eq!(refusal, payload(PayloadRule::NewExtradata), "{name}");
      untouched!(offered, back);
    } else {
      muxer
        .write_audio(offered)
        .map_err(|error| error.map_packet(drop))
        .expect("MP4: a sample description of the configuration made");
      muxer.finish().expect("finished");
    }
  }
}

/// **L68 — MP4 reports new extradata dropped where the track's sample entry
/// reads none of it** (the side-data table; grep G19 of #82): the sample
/// description a packet's new extradata opens (`movenc.c` 7044–7074) is
/// written by the codec's writer, and AC-3's and E-AC-3's `dac3` and `dec3`
/// are MP4's own parse of the frames (418–462, 612–666; 1084–1087,
/// 1507–1510), PCM's `ipcm` holds no configuration (1520–1528): none of its
/// bytes is written. Reported for the track, by name, at the first packet
/// carrying it; nothing reads back with them.
///
/// PLANT: MP4's new extradata kept for every codec again leaves the line
/// out; kept for every codec but AC-3 and E-AC-3, as before the sweep,
/// leaves PCM's out.
#[test]
fn l68_mp4_reports_ac3s_new_extradata_dropped() {
  use ffmpeg_next::ffi::AVPacketSideDataType as T;
  let Some(corpus) = Corpus::new() else { return };
  let added = [1, 2, 3, 4];
  for codec in ["ac3", "eac3", "pcm"] {
    let source = if codec == "pcm" {
      pcm_mov(&corpus)
    } else {
      audio_file(&corpus, &format!("{codec}.mkv"), codec)
    };
    let (table, frames) = first_track(&source, TrackKind::Audio, 2);
    let target = output(&corpus, &format!("l68-{codec}.mp4"));
    let mut muxer = muxer_into(&target, ContainerName::MP4, table);
    let mut first = audio_of(&frames[0])
      .with_pts(Some(ms(0)))
      .with_dts(Some(ms(0)));
    first
      .extra_mut()
      .set_side_data(vec![side(T::AV_PKT_DATA_NEW_EXTRADATA, &added)]);
    muxer
      .write_audio(AudioTrackPacket::new(TrackIndex::new(0), first))
      .map_err(|error| error.map_packet(drop))
      .expect("a frame with new extradata");
    assert_eq!(
      lines(muxer.report(), |line| matches!(
        line,
        Uncarried::SideData { .. }
      )),
      vec![Uncarried::SideData {
        track: TrackIndex::new(0),
        kind: T::AV_PKT_DATA_NEW_EXTRADATA as i32,
        name: Some("New Extradata"),
        fate: SideDataFate::Dropped,
      }],
      "{codec}"
    );
    muxer
      .write_audio(audio_at(&audio_of(&frames[1]), 32))
      .map_err(|error| error.map_packet(drop))
      .expect("the next frame");
    muxer.finish().expect("finished");
    let (written, _) = first_track(&target, TrackKind::Audio, 0);
    assert_ne!(written[0].side_data().extradata(), &added[..], "{codec}");
    assert!(
      side_data_of(&target)
        .iter()
        .flatten()
        .all(|(_, bytes)| bytes.as_slice() != &added[..]),
      "{codec}: no packet reads back with them"
    );
  }
}

/// One PGS display set as FFmpeg's SUP demuxer reads it: a presentation
/// segment of no objects, then an end segment, a packet each, at zero
/// (`supdec.c` 39–68).
fn pgs_sup(corpus: &Corpus) -> PathBuf {
  let out = output(corpus, "one-set.sup");
  if !out.exists() {
    let segment = |kind: u8, payload: &[u8]| {
      let mut bytes = b"PG".to_vec();
      bytes.extend(0u32.to_be_bytes());
      bytes.extend(0u32.to_be_bytes());
      bytes.push(kind);
      bytes.extend(u16::try_from(payload.len()).expect("small").to_be_bytes());
      bytes.extend(payload);
      bytes
    };
    // 1920 × 1080, composition 0, an epoch start, no palette update, no
    // objects.
    let presentation = [0x07, 0x80, 0x04, 0x38, 0x10, 0, 0, 0x80, 0, 0, 0];
    let set = [segment(0x16, &presentation), segment(0x80, &[])].concat();
    std::fs::write(&out, set).expect("write");
  }
  out
}

/// **L69 — every mirror of libavformat's timing moves on what an automatic
/// filter puts out, not on what it takes** (Codex R6 [high]; table G).
/// PGS's filter puts a display set out at its end segment's packet, with
/// the times of the packet that held its presentation segment
/// (`pgs_frame_merge.c` 132–151): this muxer holds the set itself until
/// then, and hands it to libavformat whole. Beside video and audio in
/// Matroska, a presentation segment at −40 s is held; video at 0 and audio
/// 10 s after make libavformat's first write without it (`mux.c` 939–1003);
/// its end segment would then put the set out at −40 s, which that write's
/// move no longer lifts — Matroska would wrap its 16-bit block time
/// (`matroskaenc.c` 2888, 3146–3163) — so it is refused by name, untouched,
/// no move reported, and the held packet handed back whole, nothing of the
/// set written. Ended before the other streams' packets, the same set is
/// put out first, and its −40 s moves every stream: read back at 0, the
/// video at 40 s.
///
/// PLANT: the mirrors moved on what the filter takes (`Step::owns`), the
/// muxer holding nothing: the held presentation segment moves every stream,
/// and the end segment is taken.
#[test]
fn l69_every_mirror_moves_on_what_a_filter_puts_out() {
  let Some(corpus) = Corpus::new() else { return };
  let source = corpus.multi_track_mkv();
  let (_, frames) = first_track(&source, TrackKind::Video, 1);
  let (_, audio) = first_track(&source, TrackKind::Audio, 1);
  let sup = pgs_sup(&corpus);
  let (_, segments) = first_track(&sup, TrackKind::Subtitle, 2);
  let table = || {
    let mut table = table_of(&source, TrackKind::Video);
    table.extend(table_of(&source, TrackKind::Audio));
    table.extend(table_of(&sup, TrackKind::Subtitle));
    table
  };
  let segment = |index: usize, at: i64| {
    SubtitleTrackPacket::new(
      TrackIndex::new(2),
      subtitle_of(&segments[index]).with_pts(Some(ms(at))),
    )
  };
  let video = |at: i64| {
    VideoTrackPacket::new(
      TrackIndex::new(0),
      video_of(&frames[0])
        .with_pts(Some(ms(at)))
        .with_dts(Some(ms(at))),
    )
  };
  let sound = |at: i64| {
    AudioTrackPacket::new(
      TrackIndex::new(1),
      audio_of(&audio[0])
        .with_pts(Some(ms(at)))
        .with_dts(Some(ms(at))),
    )
  };
  let shifted = |muxer: &FfmpegOwnedMuxer<File>| {
    muxer
      .report()
      .uncarried()
      .contains(&Uncarried::TimesShifted)
  };

  let target = output(&corpus, "l69.mkv");
  let mut muxer = muxer_into(&target, ContainerName::MATROSKA, table());
  let presentation = segment(0, -40_000);
  muxer
    .write_subtitle(presentation.clone())
    .map_err(|error| error.map_packet(drop))
    .expect("held by the muxer");
  muxer
    .write_video(video(0))
    .map_err(|error| error.map_packet(drop))
    .expect("video");
  muxer
    .write_audio(sound(10_001))
    .map_err(|error| error.map_packet(drop))
    .expect("audio 10 s on: libavformat's first write");
  assert!(!shifted(&muxer), "nothing early reached libavformat");
  let end = segment(1, -39_999);
  let (refusal, back) = by_container(muxer.write_subtitle(end.clone()));
  assert_eq!(
    refusal,
    FfmpegMuxRefusal::BeforeZero {
      track: TrackIndex::new(2),
      timestamp: ms(-40_000),
    },
    "the set's times, its presentation segment's"
  );
  untouched!(end, back);
  let held = muxer.take_held();
  assert_eq!(held.len(), 1, "the presentation segment's packet");
  untouched!(presentation, held[0]);
  assert!(!shifted(&muxer));
  muxer.finish().expect("finished");
  let seen = packets(&target);
  assert_eq!(seen[2].len(), 0, "nothing of the set written");
  assert_eq!(seen[0][0].pts, Some(ms(0)), "the video not moved");

  let target = output(&corpus, "l69-first.mkv");
  let mut muxer = muxer_into(&target, ContainerName::MATROSKA, table());
  for (index, at) in [(0, -40_000), (1, -39_999)] {
    muxer
      .write_subtitle(segment(index, at))
      .map_err(|error| error.map_packet(drop))
      .expect("the set, put out at its end");
  }
  muxer
    .write_video(video(0))
    .map_err(|error| error.map_packet(drop))
    .expect("video");
  assert!(shifted(&muxer), "the set before zero moves every stream");
  muxer
    .write_audio(sound(10_001))
    .map_err(|error| error.map_packet(drop))
    .expect("audio");
  muxer.finish().expect("finished");
  let seen = packets(&target);
  assert_eq!(
    seen[2].iter().map(|seen| seen.pts).collect::<Vec<_>>(),
    vec![Some(ms(0))],
    "the set, one block, moved by 40 s"
  );
  assert_eq!(seen[0][0].pts, Some(ms(40_000)), "the video moved with it");
}

/// **L70 — FLV refuses by name a packet decoded before the first one FLV
/// wrote** (grep G18 of #82; table G): FLV keeps the first packet it writes
/// as its reference, and refuses every later packet before it after taking
/// it — "Packets are not in the proper order with respect to DTS"
/// (`flvenc.c` 1267–1274). Beside audio that holds nothing, libavformat
/// writes the video at 5 s once a packet 10 s on arrives (`mux.c`
/// 939–1003): audio at 2 s after that is refused, untouched; at 5 s, taken.
///
/// PLANT: FLV's reference unmirrored takes the audio at 2 s, which FLV then
/// refuses after taking it.
#[test]
fn l70_flv_refuses_a_packet_before_the_first_it_wrote() {
  let Some(corpus) = Corpus::new() else { return };
  let source = corpus.multi_track_mkv();
  let (_, frames) = first_track(&source, TrackKind::Video, 2);
  let (_, audio) = first_track(&source, TrackKind::Audio, 1);
  let mut table = table_of(&source, TrackKind::Video);
  table.extend(table_of(&source, TrackKind::Audio));
  let target = output(&corpus, "l70.flv");
  let mut muxer = muxer_into(&target, ContainerName::new("flv"), table);
  for (frame, at) in frames.iter().zip([5_000, 15_001]) {
    muxer
      .write_video(video_at(&video_of(frame), Some(at), at))
      .map_err(|error| error.map_packet(drop))
      .expect("video");
  }
  let sound = |at: i64| {
    AudioTrackPacket::new(
      TrackIndex::new(1),
      audio_of(&audio[0])
        .with_pts(Some(ms(at)))
        .with_dts(Some(ms(at))),
    )
  };
  let early = sound(2_000);
  let (refusal, back) = by_container(muxer.write_audio(early.clone()));
  assert_eq!(
    refusal,
    FfmpegMuxRefusal::BeforeFirstDts {
      track: TrackIndex::new(1),
      dts: ms(2_000),
    }
  );
  untouched!(early, back);
  muxer
    .write_audio(sound(5_000))
    .map_err(|error| error.map_packet(drop))
    .expect("at the first packet FLV wrote");
  muxer.finish().expect("finished");
  let seen = packets(&target);
  assert_eq!(
    seen[1].iter().map(|seen| seen.dts).collect::<Vec<_>>(),
    vec![Some(ms(5_000))],
    "what was admitted"
  );
}

/// The `Rewrite::KeyFramesMarked` lines of `report`.
fn marked_of(report: &mediaencode_ffmpeg::MuxReport) -> usize {
  lines(report, |line| {
    matches!(
      line,
      Uncarried::Rewritten {
        rewrite: Rewrite::KeyFramesMarked,
        ..
      }
    )
  })
  .len()
}

/// `path`'s first audio track as a one-track table, its stream's codec
/// profile forced to `profile` and, where given, its configuration to
/// `config` — an AAC track with USAC's profile over an AAC LC
/// configuration, say, or the other way about, which no `ffmpeg` CLI
/// encodes.
fn audio_with(
  path: &Path,
  profile: i32,
  config: Option<&[u8]>,
) -> Vec<TrackDescription<Ffmpeg, FfmpegBytes, CodecTicket>> {
  use ffmpeg_next::ffi::{AV_INPUT_BUFFER_PADDING_SIZE, av_freep, av_mallocz};
  let mut table = table_of(path, TrackKind::Audio);
  let index = table[0].side_data().stream_index();
  let mut parameters = table[0].side_data().rebuild().expect("the parameters");
  // SAFETY: `parameters` owns its `AVCodecParameters`, live for the
  // writes; `profile` and `extradata_size` are plain integer fields, and
  // the extradata is replaced as libavcodec allocates it — freed with
  // `av_freep`, allocated zeroed with its padding by `av_mallocz`.
  unsafe {
    let par = parameters.as_mut_ptr();
    (*par).profile = profile;
    if let Some(config) = config {
      av_freep(core::ptr::addr_of_mut!((*par).extradata).cast());
      let data = av_mallocz(config.len() + AV_INPUT_BUFFER_PADDING_SIZE as usize).cast::<u8>();
      assert!(!data.is_null(), "allocated");
      core::ptr::copy_nonoverlapping(config.as_ptr(), data, config.len());
      (*par).extradata = data;
      (*par).extradata_size = i32::try_from(config.len()).expect("small");
    }
  }
  *table[0].side_data_mut() =
    CodecTicket::mirror(&parameters, index, usize::MAX).expect("the ticket");
  table
}

/// **L71 — an unmarked packet MP4 writes as a key frame is taken and
/// reported, by movenc's own test, the stream's profile** (table G; the
/// Fable delta gate's C1; Codex R7 1): MP4 writes no sync-sample table for
/// an audio track but TrueHD's, MPEG-H 3D Audio's and USAC AAC's
/// (`movenc.c` 3412–3418), USAC judged by the stream's profile, not its
/// configuration, so an unmarked AAC packet reads back a key frame, as it
/// would marked: taken, reported. An AAC track whose stream carries USAC's
/// profile over an AAC LC configuration gets the table where its flags
/// mix: its unmarked packet reads back unmarked, nothing reported; one
/// whose configuration is USAC's under AAC LC's profile gets none, and is
/// reported. Matroska keeps the flag. A video track none of whose packets is marked
/// gets no table, every sample a key frame by the file, reported at
/// `finish`; one with a key frame gets its table.
///
/// PLANT: USAC judged by the configuration's object type, as before Codex
/// R7: the USAC-profiled track's unmarked packet reported, and the
/// USAC-configured one's not.
#[test]
fn l71_mp4_marks_by_its_sync_table_as_movenc_decides_it() {
  let Some(corpus) = Corpus::new() else { return };
  let source = corpus.multi_track_mkv();
  let (_, audio) = first_track(&source, TrackKind::Audio, 2);
  let unmarked = |at: i64| {
    AudioTrackPacket::new(
      TrackIndex::new(0),
      audio_of(&audio[1])
        .with_pts(Some(ms(at)))
        .with_dts(Some(ms(at)))
        .with_flags(PacketFlags::empty()),
    )
  };
  /// `AV_PROFILE_AAC_LOW` and `AV_PROFILE_AAC_USAC` (`defs.h` 67, 76).
  const LOW: i32 = 1;
  const USAC: i32 = 41;
  // An AudioSpecificConfig of object type 42, USAC's: 31, then 10 in six
  // bits; 48 kHz, stereo (`mpeg4audio.c` 77–83).
  const USAC_CONFIG: [u8; 3] = [0xF9, 0x46, 0x40];
  for (name, profile, config, reported) in [
    ("l71.mp4", LOW, None, 1),
    ("l71-usac.mp4", USAC, None, 0),
    ("l71-usac-config.mp4", LOW, Some(&USAC_CONFIG[..]), 1),
  ] {
    let target = output(&corpus, name);
    let mut muxer = muxer_into(
      &target,
      ContainerName::MP4,
      audio_with(&source, profile, config),
    );
    muxer
      .write_audio(audio_at(&audio_of(&audio[0]), 0))
      .map_err(|error| error.map_packet(drop))
      .expect("a key frame");
    muxer
      .write_audio(unmarked(21))
      .map_err(|error| error.map_packet(drop))
      .expect("taken: the file is the same as for it marked");
    assert_eq!(marked_of(muxer.report()), reported, "{name}");
    muxer.finish().expect("finished");
    let written = std::fs::read(&target).expect("the file");
    assert_eq!(
      written.windows(4).any(|box_type| box_type == b"stss"),
      reported == 0,
      "{name}: the sync-sample table, by the profile"
    );
    if config.is_none() {
      let keys: Vec<_> = packets(&target)[0].iter().map(|seen| seen.key).collect();
      assert_eq!(keys, vec![true, reported == 1], "{name}: read back");
    }
  }

  let target = output(&corpus, "l71.mkv");
  let mut muxer = muxer_into(
    &target,
    ContainerName::MATROSKA,
    table_of(&source, TrackKind::Audio),
  );
  muxer
    .write_audio(audio_at(&audio_of(&audio[0]), 0))
    .map_err(|error| error.map_packet(drop))
    .expect("a key frame");
  muxer
    .write_audio(unmarked(21))
    .map_err(|error| error.map_packet(drop))
    .expect("Matroska keeps the flag");
  assert_eq!(marked_of(muxer.report()), 0, "nothing marked");
  muxer.finish().expect("finished");
  let keys: Vec<_> = packets(&target)[0].iter().map(|seen| seen.key).collect();
  assert_eq!(keys, vec![true, false], "read back as offered");

  let (_, frames) = first_track(&source, TrackKind::Video, 2);
  for (name, first_key, reported) in [("l71-none.mp4", false, 1), ("l71-mixed.mp4", true, 0)] {
    let target = output(&corpus, name);
    let mut muxer = muxer_into(
      &target,
      ContainerName::MP4,
      table_of(&source, TrackKind::Video),
    );
    for (index, frame) in frames.iter().enumerate() {
      let at = 40 * index as i64;
      let flags = if index == 0 && first_key {
        PacketFlags::KEY
      } else {
        PacketFlags::empty()
      };
      muxer
        .write_video(VideoTrackPacket::new(
          TrackIndex::new(0),
          video_of(frame)
            .with_pts(Some(ms(at)))
            .with_dts(Some(ms(at)))
            .with_flags(flags),
        ))
        .map_err(|error| error.map_packet(drop))
        .expect("video");
    }
    let (_, report) = muxer.finish_into_sink().expect("finished");
    assert_eq!(marked_of(&report), reported, "{name}");
    // The sync-sample table: none, where no sample is a key frame — every
    // sample one by the file, though FFmpeg's reader parses H.264's own
    // from the bitstream (`mov.c` 3159–3160).
    let written = std::fs::read(&target).expect("the file");
    assert_eq!(
      written.windows(4).any(|box_type| box_type == b"stss"),
      first_key,
      "{name}: the sync-sample table"
    );
  }
}

/// **L72 — a configuration's packet, of no payload and carrying new
/// extradata, is taken where the container takes the configuration, and
/// refused as empty where it drops it** (Codex R7 2; tables C and C″): MP4
/// rewrites an MPEG-4 ALS, AAC, AV1 or FLAC track's current sample
/// description with it and writes no sample (`movenc.c` 7412–7441);
/// Matroska an AAC or FLAC track's CodecPrivate, and writes no block
/// (`matroskaenc.c` 3065–3097, 3266–3268). Into MP4, FLAC's 34-byte
/// STREAMINFO is taken, reported possibly dropped — a later one would
/// replace it — and read back as the track's configuration; one of another
/// length is refused (`dfLa`, 857–876). No sample is counted of it, so a
/// packet more than `i32::MAX` ticks after the last sample is still
/// refused, the configuration's packet just before it notwithstanding
/// (`check_pkt` counts from the last sample written). MP4's H.264 takes no
/// such packet: refused, as the empty packet MP4 drops. Into Matroska,
/// AAC's configuration is taken where the header reserved the output
/// sampling frequency its update rewrites — an HE-AAC track's, whose
/// configuration states an extension sample rate — and refused on an AAC
/// LC track's, whose update would run over the track entry
/// (`matroskaenc.c` 2129–2135, 3076–3081); AC-3's, which Matroska
/// ignores, refused as empty.
///
/// PLANT: the road closed refuses the FLAC configuration's packet as
/// empty; the road counting a sample takes the packet after it; the
/// output sampling frequency held reserved takes AAC LC's.
#[test]
fn l72_a_configurations_packet_is_taken_where_the_container_takes_it() {
  use ffmpeg_next::ffi::AVPacketSideDataType as T;
  let Some(corpus) = Corpus::new() else { return };
  let configuration = |packet: &Audio, track: usize, at: i64, new: &[u8]| {
    let mut empty = audio_holding(packet, &[])
      .with_pts(Some(ms(at)))
      .with_dts(Some(ms(at)));
    empty
      .extra_mut()
      .set_side_data(vec![side(T::AV_PKT_DATA_NEW_EXTRADATA, new)]);
    AudioTrackPacket::new(TrackIndex::new(track), empty)
  };
  let new_extradata = |report: &mediaencode_ffmpeg::MuxReport| {
    lines(report, |line| {
      matches!(line, Uncarried::SideData { kind, .. }
        if *kind == T::AV_PKT_DATA_NEW_EXTRADATA as i32)
    })
  };

  let flac = audio_file(&corpus, "l72.flac", "flac");
  let (table, frames) = first_track(&flac, TrackKind::Audio, 1);
  let mut streaminfo = table[0].side_data().extradata().to_vec();
  assert_eq!(streaminfo.len(), 34, "the premise: a STREAMINFO");
  *streaminfo.last_mut().expect("bytes") ^= 0xFF;
  let target = output(&corpus, "l72.mp4");
  let mut muxer = muxer_into(&target, ContainerName::MP4, table);
  let frame = audio_of(&frames[0]);
  muxer
    .write_audio(audio_at(&frame, 0))
    .map_err(|error| error.map_packet(drop))
    .expect("a frame");
  let short = configuration(&frame, 0, 10, &streaminfo[..33]);
  let (refusal, back) = by_container(muxer.write_audio(short.clone()));
  assert_eq!(
    refusal,
    payload(PayloadRule::FlacStreamInfo),
    "dfLa's 34 bytes"
  );
  untouched!(short, back);
  // 2^31 ticks of 48 kHz are 44 739 242.7 ms: the configuration's packet
  // lies past them, the frame after it too.
  muxer
    .write_audio(configuration(&frame, 0, 44_739_300, &streaminfo))
    .map_err(|error| error.map_packet(drop))
    .expect("the track's configuration, of no sample");
  assert_eq!(
    new_extradata(muxer.report()),
    vec![Uncarried::SideData {
      track: TrackIndex::new(0),
      kind: T::AV_PKT_DATA_NEW_EXTRADATA as i32,
      name: Some("New Extradata"),
      fate: SideDataFate::PossiblyDropped,
    }]
  );
  let late = audio_at(&frame, 44_739_301);
  let (refusal, back) = by_container(muxer.write_audio(late.clone()));
  assert!(
    matches!(refusal, FfmpegMuxRefusal::DtsGap { .. }),
    "counted from the last sample, not the configuration's packet: {refusal:?}"
  );
  untouched!(late, back);
  muxer.finish().expect("finished");
  let (written, _) = first_track(&target, TrackKind::Audio, 0);
  assert_eq!(
    written[0].side_data().extradata(),
    &streaminfo[..],
    "the configuration read back"
  );
  assert_eq!(packet_count(&target), 1, "no sample of it");

  let source = corpus.multi_track_mkv();
  let (_, video) = first_track(&source, TrackKind::Video, 1);
  let mut muxer = muxer_into(
    &output(&corpus, "l72-h264.mp4"),
    ContainerName::MP4,
    table_of(&source, TrackKind::Video),
  );
  muxer
    .write_video(video_at(&video_of(&video[0]), Some(0), 0))
    .map_err(|error| error.map_packet(drop))
    .expect("a frame");
  let mut empty = video_holding(&video_of(&video[0]), &[])
    .with_pts(Some(ms(40)))
    .with_dts(Some(ms(40)));
  empty
    .extra_mut()
    .set_side_data(vec![side(T::AV_PKT_DATA_NEW_EXTRADATA, &[1, 2, 3, 4])]);
  let empty = VideoTrackPacket::new(TrackIndex::new(0), empty);
  let (refusal, back) = by_container(muxer.write_video(empty.clone()));
  assert_eq!(refusal, payload(PayloadRule::Empty), "MP4's H.264 drops it");
  untouched!(empty, back);
  muxer.finish().expect("finished");

  let (table, audio) = first_track(&source, TrackKind::Audio, 1);
  let stated = table[0].side_data().extradata().to_vec();
  assert_eq!(
    &stated[..2],
    &[0x11, 0x90],
    "the premise: AAC LC at 48 kHz, stereo"
  );
  let frame = audio_of(&audio[0]);
  // HE-AAC's explicit SBR — 24 kHz, its extension 48 kHz, stereo — whose
  // extension sample rate Matroska's header writes as the output sampling
  // frequency, reserving the bytes every update rewrites (`matroskaenc.c`
  // 2129–2135, 3076–3081); the same, mono — its channel configuration,
  // bits 9 to 12, 1 — of its length, within the space reserved.
  const HE: i32 = 4;
  let he = [0x2B, 0x11, 0x88, 0x00];
  let mono = [0x2B, 0x09, 0x88, 0x00];
  let target = output(&corpus, "l72.mkv");
  let mut muxer = muxer_into(
    &target,
    ContainerName::MATROSKA,
    audio_with(&source, HE, Some(&he)),
  );
  muxer
    .write_audio(audio_at(&frame, 0))
    .map_err(|error| error.map_packet(drop))
    .expect("a frame");
  muxer
    .write_audio(configuration(&frame, 0, 21, &mono))
    .map_err(|error| error.map_packet(drop))
    .expect("the track's CodecPrivate, of no block");
  assert_eq!(
    new_extradata(muxer.report()).len(),
    1,
    "possibly dropped, as Matroska's AAC's"
  );
  muxer.finish().expect("finished");
  let (written, _) = first_track(&target, TrackKind::Audio, 0);
  assert_eq!(written[0].side_data().extradata(), &mono[..], "read back");
  assert_eq!(packet_count(&target), 1, "no block of it");
  // AAC LC's stated configuration, libavcodec's encoder's, states no
  // extension sample rate, and its header reserved no output sampling
  // frequency: the update would run over the track entry. Refused, by
  // name, untouched.
  let mut lc_mono = stated.clone();
  lc_mono[1] = 0x88;
  let mut muxer = muxer_into(
    &output(&corpus, "l72-lc.mkv"),
    ContainerName::MATROSKA,
    table,
  );
  muxer
    .write_audio(audio_at(&frame, 0))
    .map_err(|error| error.map_packet(drop))
    .expect("a frame");
  let unreserved = configuration(&frame, 0, 21, &lc_mono);
  let (refusal, back) = by_container(muxer.write_audio(unreserved.clone()));
  assert_eq!(refusal, payload(PayloadRule::NewExtradata), "AAC LC's");
  untouched!(unreserved, back);
  muxer.finish().expect("finished");

  let ac3 = audio_file(&corpus, "ac3.mkv", "ac3");
  let (table, frames) = first_track(&ac3, TrackKind::Audio, 1);
  let mut muxer = muxer_into(
    &output(&corpus, "l72-ac3.mkv"),
    ContainerName::MATROSKA,
    table,
  );
  let frame = audio_of(&frames[0]);
  muxer
    .write_audio(audio_at(&frame, 0))
    .map_err(|error| error.map_packet(drop))
    .expect("a frame");
  let ignored = configuration(&frame, 0, 32, &[1, 2, 3]);
  let (refusal, back) = by_container(muxer.write_audio(ignored.clone()));
  assert_eq!(
    refusal,
    payload(PayloadRule::Empty),
    "Matroska ignores AC-3's"
  );
  untouched!(ignored, back);
  muxer.finish().expect("finished");
}

// ---------------------------------------------------------------------------
//  H: the muxers this release audited.
// ---------------------------------------------------------------------------

/// The muxers this release audited, by the names libavformat resolves
/// (table H of the census).
const AUDITED: [&str; 45] = [
  "mov",
  "mp4",
  "psp",
  "3gp",
  "3g2",
  "ipod",
  "ismv",
  "f4v",
  "avif",
  "matroska",
  "webm",
  "mpegts",
  "mpeg",
  "vcd",
  "vob",
  "svcd",
  "dvd",
  "flv",
  "avi",
  "asf",
  "asf_stream",
  "nut",
  "ogg",
  "oga",
  "ogv",
  "opus",
  "spx",
  "mxf",
  "mxf_d10",
  "mxf_opatom",
  "ivf",
  "wav",
  "w64",
  "adts",
  "ac3",
  "eac3",
  "dts",
  "flac",
  "mp2",
  "mp3",
  "h264",
  "hevc",
  "obu",
  "m4v",
  "mpeg2video",
];

/// **L73 — a muxer opens only where this release audited it** (table H of
/// the census; the owner's ruling, R8): each muxer the linked libavformat
/// registers, opened by its own name, is refused at open by that name
/// where it is not audited ([`MuxOpenError::ContainerNotAudited`]) — before
/// any stream is added, nothing written — or, where it writes no byte
/// stream, as before ([`MuxOpenError::NoByteStream`]); each audited one
/// passes the check, to the table's own refusal (a track whose ticket is
/// another codec's, [`MuxOpenError::TicketMismatch`]), nothing written
/// either.
///
/// PLANT: the check skipped: `srt` reaches the table's refusal.
#[test]
fn l73_a_muxer_opens_only_where_this_release_audited_it() {
  use ffmpeg_next::ffi::{AVFMT_NOFILE, av_muxer_iterate};
  let Some(corpus) = Corpus::new() else { return };
  let source = corpus.multi_track_mkv();
  let demuxer = FfmpegOwnedDemuxer::open(&source).expect("open");
  let row = demuxer
    .tracks()
    .iter()
    .find(|row| row.kind() == TrackKind::Video)
    .expect("a video track");
  let pixel_format = match row.params() {
    TrackParams::Video(params) => params.pixel_format().clone(),
    other => panic!("expected video parameters, got {other:?}"),
  };
  // HEVC parameters over the H.264 stream's ticket.
  let mismatched = || {
    TrackDescription::new(
      millis(),
      TrackParams::Video(mediaencode::mediaspine::demuxer::VideoTrackParams::new(
        CodecId::HEVC,
        64,
        64,
        pixel_format.clone(),
        None,
      )),
      ticket(row),
    )
    .expect("a description")
  };
  let target = output(&corpus, "l73.out");
  let (mut registered, mut admitted, mut refused) = (0, Vec::new(), Vec::new());
  let mut opaque = core::ptr::null_mut();
  loop {
    // SAFETY: `opaque` is the iteration state `av_muxer_iterate` keeps,
    // null at the start; each answer is a static muxer, or null past the
    // last, its `name` a static NUL-terminated string and `flags` a plain
    // integer.
    let (name, flags) = unsafe {
      let muxer = av_muxer_iterate(&mut opaque);
      if muxer.is_null() {
        break;
      }
      (
        std::ffi::CStr::from_ptr((*muxer).name)
          .to_str()
          .expect("a UTF-8 name"),
        (*muxer).flags,
      )
    };
    registered += 1;
    // `stream_segment,ssegment`: a name list, matched by each name.
    let first = name.split(',').next().expect("a name");
    let result = FfmpegOwnedMuxer::open(
      File::create(&target).expect("create"),
      ContainerName::new(first.to_owned()),
      vec![mismatched()],
      &[],
      PacketLimits::default(),
    )
    .map(drop);
    assert_eq!(
      std::fs::metadata(&target).expect("the file").len(),
      0,
      "{name}: nothing written"
    );
    if flags & AVFMT_NOFILE as i32 != 0 {
      assert!(
        matches!(result, Err(MuxOpenError::NoByteStream { .. })),
        "{name}: {result:?}"
      );
    } else if AUDITED.contains(&first) {
      assert!(
        matches!(result, Err(MuxOpenError::TicketMismatch { track }) if track.get() == 0),
        "{name}: past the check, {result:?}"
      );
      admitted.push(first);
    } else {
      assert!(
        matches!(&result, Err(MuxOpenError::ContainerNotAudited { name: refused })
          if refused.as_str() == name),
        "{name}: {result:?}"
      );
      refused.push(first);
    }
  }
  // FFmpeg 9.0.1 registers 185; a build leaves out those whose libraries
  // it lacks, Chromaprint's among them.
  assert!(
    registered > 100,
    "the linked libavformat's muxers: {registered}"
  );
  admitted.sort_unstable();
  admitted.dedup();
  assert_eq!(
    admitted.len(),
    AUDITED.len(),
    "every audited name, each one muxer's"
  );
  assert!(refused.contains(&"srt") && refused.contains(&"image2pipe"));
}

/// Every packet of `path`'s first track of `kind`, and its one-track table.
fn whole_track(path: &Path, kind: TrackKind) -> Track {
  let mut demuxer = FfmpegOwnedDemuxer::open(path).expect("open");
  let (index, description) = demuxer
    .tracks()
    .iter()
    .enumerate()
    .find(|(_, row)| row.kind() == kind)
    .map(|(index, row)| {
      (
        index,
        TrackDescription::from_track_info(&**row, None, ticket(row)).expect("a description"),
      )
    })
    .expect("a track of the kind");
  let mut packets = Vec::new();
  while let Some(packet) = demuxer.next_packet().expect("a packet or the end") {
    if packet.track().get() == index {
      packets.push(packet);
    }
  }
  (vec![description], packets)
}

/// Writes `packets`, as read, into `container` at `target` over `table` —
/// its one track the packets' — and answers the report.
fn written_through(
  target: &Path,
  container: ContainerName,
  (table, packets): Track,
) -> mediaencode_ffmpeg::MuxReport {
  let mut muxer = muxer_into(target, container, table);
  for packet in &packets {
    let track = TrackIndex::new(0);
    match packet {
      DemuxedPacket::Video(p) => muxer
        .write_video(VideoTrackPacket::new(track, p.packet().clone()))
        .map_err(|error| error.map_packet(drop)),
      DemuxedPacket::Audio(p) => muxer
        .write_audio(AudioTrackPacket::new(track, p.packet().clone()))
        .map_err(|error| error.map_packet(drop)),
      other => panic!("expected a video or an audio packet, got {other:?}"),
    }
    .unwrap_or_else(|error| panic!("{}: a packet as read: {error:?}", target.display()));
  }
  muxer.finish_into_sink().expect("finished").1
}

/// The bytes of `packets`, joined.
fn joined_bytes(packets: &[DemuxedPacket<Ffmpeg, FfmpegBytes>]) -> Vec<u8> {
  packets
    .iter()
    .flat_map(|packet| match packet {
      DemuxedPacket::Video(p) => p.packet().data().as_ref().to_vec(),
      DemuxedPacket::Audio(p) => p.packet().data().as_ref().to_vec(),
      other => panic!("expected a video or an audio packet, got {other:?}"),
    })
    .collect()
}

/// The lines of `report` table H's field rows and table B's filters make.
fn field_rows(report: &mediaencode_ffmpeg::MuxReport) -> Vec<Uncarried> {
  lines(report, |line| {
    matches!(
      line,
      Uncarried::Rewritten { .. } | Uncarried::Filtered { .. }
    )
  })
}

/// **L74 — one well-formed stream round-trips through each raw stream's
/// muxer, IVF, WAV and Wave64, and the report says what each writes of it**
/// (table H of the census): raw AC-3, E-AC-3, DTS, MPEG audio layer II and
/// III, FLAC, MPEG-4 Part 2 and MPEG-2 video read back as the bytes
/// written; ADTS reads back each raw AAC frame behind its header, and an
/// ADTS stream as it was; H.264 and HEVC from Matroska and AV1 as as many
/// packets as written, the first two converted to Annex B by the filter
/// their first packet inserts, which writes the configuration; IVF's VP8
/// and WAV's and Wave64's PCM as written. None of them writes a time — but
/// IVF the presentation time, keeping no duration — nor a key flag, which
/// the report says for the codecs that do not count every frame a key
/// frame. A raw stream writes no configuration but what its packets carry,
/// which MPEG-4's, MPEG-2's and AV1's first packets here do.
#[test]
fn l74_each_raw_stream_round_trips_through_its_muxer() {
  let Some(corpus) = Corpus::new() else { return };
  let track = TrackIndex::new(0);
  let rewritten = |rewrites: &[Rewrite]| {
    rewrites
      .iter()
      .map(|&rewrite| Uncarried::Rewritten { track, rewrite })
      .collect::<Vec<_>>()
  };
  let timeline = rewritten(&[Rewrite::Timeline]);
  let untimed = rewritten(&[Rewrite::Timeline, Rewrite::SyncFromBitstream]);
  let sine = |name: &str, codec: &str, extra: &[&str]| {
    let mut args = vec![
      "-f",
      "lavfi",
      "-i",
      "sine=frequency=440:sample_rate=48000:duration=0.5",
      "-ac",
      "2",
      "-c:a",
      codec,
    ];
    args.extend_from_slice(extra);
    generated(&corpus, name, &args)
  };
  let picture = |name: &str, codec: &str| {
    generated(
      &corpus,
      name,
      &[
        "-f",
        "lavfi",
        "-i",
        "testsrc2=size=64x64:rate=25:duration=0.2",
        "-c:v",
        codec,
      ],
    )
  };
  // The streams whose bytes read back as written.
  for (container, source, kind, expected) in [
    (
      "ac3",
      sine("ac3.mkv", "ac3", &[]),
      TrackKind::Audio,
      timeline.clone(),
    ),
    (
      "eac3",
      sine("eac3.mkv", "eac3", &[]),
      TrackKind::Audio,
      timeline.clone(),
    ),
    (
      "dts",
      sine("dts.mkv", "dca", &["-strict", "-2"]),
      TrackKind::Audio,
      timeline.clone(),
    ),
    (
      "mp2",
      sine("mp2.mkv", "mp2", &[]),
      TrackKind::Audio,
      timeline.clone(),
    ),
    (
      "mp3",
      sine("mp3.mkv", "libmp3lame", &[]),
      TrackKind::Audio,
      timeline.clone(),
    ),
    (
      "flac",
      sine("flac.flac", "flac", &[]),
      TrackKind::Audio,
      timeline.clone(),
    ),
    ("wav", pcm_mkv(&corpus), TrackKind::Audio, timeline.clone()),
    ("w64", pcm_mkv(&corpus), TrackKind::Audio, timeline.clone()),
    (
      "m4v",
      // A raw stream's: its video object layer in band, as the raw
      // muxer's stream needs it.
      generated(
        &corpus,
        "mpeg4.m4v",
        &[
          "-f",
          "lavfi",
          "-i",
          "testsrc2=size=64x64:rate=25:duration=0.2",
          "-c:v",
          "mpeg4",
          "-f",
          "m4v",
        ],
      ),
      TrackKind::Video,
      untimed.clone(),
    ),
    (
      "mpeg2video",
      picture("mpeg2.mkv", "mpeg2video"),
      TrackKind::Video,
      untimed.clone(),
    ),
    (
      "adts",
      corpus.transport_stream(),
      TrackKind::Audio,
      untimed.clone(),
    ),
    (
      "ivf",
      picture("vp8.webm", "libvpx"),
      TrackKind::Video,
      rewritten(&[Rewrite::Durations, Rewrite::SyncFromBitstream]),
    ),
  ] {
    let target = output(&corpus, &format!("l74.{container}"));
    let offered = whole_track(&source, kind);
    let bytes = joined_bytes(&offered.1);
    let report = written_through(&target, ContainerName::new(container), offered);
    assert_eq!(field_rows(&report), expected, "{container}");
    if let Err(error) = FfmpegOwnedDemuxer::open(&target) {
      panic!("{container}: read back: {error:?}");
    }
    let back = packets(&target);
    assert_eq!(back.len(), 1, "{container}: one track");
    let read: Vec<u8> = back[0].iter().flat_map(|seen| seen.bytes.clone()).collect();
    assert!(
      read == bytes,
      "{container}: the bytes written, {} of {}",
      read.len(),
      bytes.len()
    );
  }
  // AAC with a configuration: each frame behind its header, of no CRC.
  let (table, frames) = whole_track(&corpus.multi_track_mkv(), TrackKind::Audio);
  let raw: Vec<Vec<u8>> = frames
    .iter()
    .map(|packet| audio_of(packet).data().as_ref().to_vec())
    .collect();
  let target = output(&corpus, "l74-configured.adts");
  let report = written_through(&target, ContainerName::new("adts"), (table, frames));
  assert_eq!(
    field_rows(&report),
    [rewritten(&[Rewrite::AdtsHeaders]), untimed.clone()].concat(),
    "libavcodec's configuration, its sync extension among it"
  );
  let back = &packets(&target)[0];
  assert_eq!(back.len(), raw.len(), "a frame a packet");
  for (seen, raw) in back.iter().zip(&raw) {
    assert_eq!(
      &seen.bytes[7..],
      &raw[..],
      "the raw frame behind its header"
    );
  }
  // The streams a filter converts.
  for (container, source, filter) in [
    (
      "h264",
      corpus.multi_track_mkv(),
      Some(AutomaticFilter::H264Mp4ToAnnexB),
    ),
    (
      "hevc",
      // No B-frames: Matroska's reader gives no decode time to a frame
      // that reorders.
      generated(
        &corpus,
        "hevc.mkv",
        &[
          "-f",
          "lavfi",
          "-i",
          "testsrc2=size=64x64:rate=25:duration=0.2",
          "-c:v",
          "libx265",
          "-x265-params",
          "bframes=0:log-level=none",
        ],
      ),
      Some(AutomaticFilter::HevcMp4ToAnnexB),
    ),
    ("obu", av1_mkv(&corpus), Some(AutomaticFilter::Av1Metadata)),
  ] {
    let target = output(&corpus, &format!("l74.{container}"));
    let offered = whole_track(&source, TrackKind::Video);
    let count = offered.1.len();
    let report = written_through(&target, ContainerName::new(container), offered);
    let mut expected = filter
      .map(|filter| vec![Uncarried::Filtered { track, filter }])
      .unwrap_or_default();
    expected.extend(untimed.clone());
    let mut seen = field_rows(&report);
    seen.sort_by_key(|line| format!("{line:?}"));
    expected.sort_by_key(|line| format!("{line:?}"));
    assert_eq!(seen, expected, "{container}");
    assert_eq!(
      packets(&target)[0].len(),
      count,
      "{container}: a packet a packet"
    );
  }
}

/// The track `packet` is on, as an audio packet on track 0.
fn audio_on_zero(packet: Audio) -> AudioTrackPacket<Ffmpeg, FfmpegBytes> {
  AudioTrackPacket::new(TrackIndex::new(0), packet)
}

/// **L75 — the ADTS muxer's refusals: what it cannot write in its headers,
/// a header it would write twice, a frame its 13-bit length cannot count,
/// and a frame without a header on a track with no configuration** (table
/// H of the census; `adtsenc.c` 52–190): a stated configuration of USAC,
/// an object type ADTS has no profile for, refused at open
/// ([`MuxOpenError::ConfigurationInvalid`]); on a configured track, an
/// ADTS frame ([`PayloadRule::AdtsHeader`]) and a raw frame of 8185 bytes,
/// 8192 with its header ([`PayloadRule::AdtsFrameLength`]) — one of 8184
/// taken; on a track with no configuration, a raw frame
/// ([`PayloadRule::AdtsFrame`]), and new extradata the writer refuses
/// ([`PayloadRule::NewExtradata`]) — one it takes configures the track,
/// each raw frame read back behind its header from then on; an empty
/// packet, which the writer drops ([`PayloadRule::Empty`]). Each refused
/// by name, untouched.
///
/// PLANT: each check removed takes its packet.
#[test]
fn l75_the_adts_muxer_refuses_what_its_headers_cannot_carry() {
  use ffmpeg_next::ffi::AVPacketSideDataType as T;
  let Some(corpus) = Corpus::new() else { return };
  let source = corpus.multi_track_mkv();
  let adts = || ContainerName::new("adts");
  // USAC — object type 42, behind the escape — at 48 kHz, stereo.
  let usac = [0xF9, 0x46, 0x40];
  let refused = FfmpegOwnedMuxer::open(
    File::create(output(&corpus, "l75-usac.adts")).expect("create"),
    adts(),
    audio_with(&source, 41, Some(&usac)),
    &[],
    PacketLimits::default(),
  );
  assert!(
    matches!(refused, Err(MuxOpenError::ConfigurationInvalid { codec, .. }) if codec == CodecId::AAC),
    "{:?}",
    refused.map(drop)
  );

  let (table, frames) = first_track(&source, TrackKind::Audio, 2);
  let raw = audio_of(&frames[0]);
  let mut muxer = muxer_into(&output(&corpus, "l75.adts"), adts(), table);
  let framed = audio_on_zero(audio_holding(&raw, &adts_frame_of(raw.data().as_ref(), 2)));
  let long = audio_on_zero(audio_holding(&raw, &vec![0x21; 8185]));
  let empty = audio_on_zero(audio_holding(&raw, &[]));
  for (offered, rule) in [
    (framed, PayloadRule::AdtsHeader),
    (long, PayloadRule::AdtsFrameLength),
    (empty, PayloadRule::Empty),
  ] {
    let (refusal, back) = by_container(muxer.write_audio(offered.clone()));
    assert_eq!(refusal, payload(rule), "{rule:?}");
    untouched!(offered, back);
  }
  muxer
    .write_audio(audio_on_zero(audio_holding(&raw, &vec![0x21; 8184])))
    .map_err(|error| error.map_packet(drop))
    .expect("8191 bytes with its header");
  muxer.finish().expect("finished");

  let ts = corpus.transport_stream();
  let (table, frames) = first_track(&ts, TrackKind::Audio, 3);
  let first = audio_of(&frames[0]);
  let bytes = first.data().as_ref();
  // The frame's raw payload: past its seven bytes of header, and two of
  // CRC where it has one.
  let payload_of = |bytes: &[u8]| bytes[if bytes[1] & 1 == 1 { 7 } else { 9 }..].to_vec();
  let unframed = audio_on_zero(audio_holding(&first, &payload_of(bytes)));
  let target = output(&corpus, "l75-configures.adts");
  let mut muxer = muxer_into(&target, adts(), table);
  let (refusal, back) = by_container(muxer.write_audio(unframed.clone()));
  assert_eq!(
    refusal,
    payload(PayloadRule::AdtsFrame),
    "no header to write it behind"
  );
  untouched!(unframed, back);
  let mut configuring = audio_holding(&first, &payload_of(bytes));
  configuring
    .extra_mut()
    .set_side_data(vec![side(T::AV_PKT_DATA_NEW_EXTRADATA, &usac)]);
  let configuring = audio_on_zero(configuring);
  let (refusal, back) = by_container(muxer.write_audio(configuring.clone()));
  assert_eq!(refusal, payload(PayloadRule::NewExtradata), "USAC's");
  untouched!(configuring, back);
  // AAC LC at 48 kHz, stereo: from here on, a header before each frame.
  for (index, frame) in frames.iter().enumerate() {
    let mut raw = audio_holding(
      &audio_of(frame),
      &payload_of(audio_of(frame).data().as_ref()),
    );
    if index == 0 {
      raw
        .extra_mut()
        .set_side_data(vec![side(T::AV_PKT_DATA_NEW_EXTRADATA, &[0x11, 0x90])]);
    }
    muxer
      .write_audio(audio_on_zero(raw))
      .map_err(|error| error.map_packet(drop))
      .expect("a raw frame, the track configured");
  }
  let after = audio_of(&frames[2])
    .dts()
    .expect("a decode time")
    .saturating_add_duration(core::time::Duration::from_millis(100));
  let framed = audio_on_zero(
    audio_of(&frames[0])
      .with_pts(Some(after))
      .with_dts(Some(after)),
  );
  let (refusal, back) = by_container(muxer.write_audio(framed.clone()));
  assert_eq!(refusal, payload(PayloadRule::AdtsHeader), "configured now");
  untouched!(framed, back);
  muxer.finish().expect("finished");
  let back = &packets(&target)[0];
  assert_eq!(back.len(), 3);
  for (seen, frame) in back.iter().zip(&frames) {
    assert_eq!(
      &seen.bytes[7..],
      &payload_of(audio_of(frame).data().as_ref())[..]
    );
  }
}

/// **L76 — FLAC and MP3 refuse at open what they would not write, and FLAC
/// takes a STREAMINFO of no payload** (table H): a video track, which
/// either writes only as one attached picture and this muxer makes none
/// there ([`MuxOpenError::CodecNotCarried`]); FLAC with no configuration
/// ([`MuxOpenError::ConfigurationMissing`]), and one opening with the
/// `fLaC` marker, whose first 34 bytes the header would write as the
/// STREAMINFO ([`MuxOpenError::ConfigurationInvalid`]). An empty FLAC
/// packet is refused ([`PayloadRule::Empty`]); one carrying a STREAMINFO
/// is taken, no frame written, and the trailer writes it into the header,
/// where it reads back.
///
/// PLANT: the open refusals removed open the track; the configuration's
/// road closed refuses the STREAMINFO's packet as empty.
#[test]
fn l76_flac_and_mp3_refuse_what_they_would_not_write() {
  use ffmpeg_next::ffi::AVPacketSideDataType as T;
  let Some(corpus) = Corpus::new() else { return };
  let source = corpus.multi_track_mkv();
  let flac = audio_file(&corpus, "flac.flac", "flac");
  let open = |name: &str, container: &str, table| {
    FfmpegOwnedMuxer::open(
      File::create(output(&corpus, name)).expect("create"),
      ContainerName::new(container),
      table,
      &[],
      PacketLimits::default(),
    )
    .map(drop)
  };
  for container in ["flac", "mp3"] {
    assert!(
      matches!(
        open("l76-video.out", container, table_of(&source, TrackKind::Video)),
        Err(MuxOpenError::CodecNotCarried { codec, .. }) if codec == CodecId::H264
      ),
      "{container}"
    );
  }
  let streaminfo = table_of(&flac, TrackKind::Audio)[0]
    .side_data()
    .extradata()
    .to_vec();
  assert_eq!(streaminfo.len(), 34, "the premise: a STREAMINFO");
  let marked = [&b"fLaC\x80\x00\x00\x22"[..], &streaminfo].concat();
  assert!(matches!(
    open("l76-none.flac", "flac", audio_with(&flac, -99, Some(&[]))),
    Err(MuxOpenError::ConfigurationMissing { .. })
  ));
  assert!(matches!(
    open(
      "l76-marked.flac",
      "flac",
      audio_with(&flac, -99, Some(&marked))
    ),
    Err(MuxOpenError::ConfigurationInvalid { .. })
  ));

  let (table, frames) = first_track(&flac, TrackKind::Audio, 1);
  let frame = audio_of(&frames[0]);
  let target = output(&corpus, "l76.flac");
  let mut muxer = muxer_into(&target, ContainerName::new("flac"), table);
  muxer
    .write_audio(audio_on_zero(frame.clone()))
    .map_err(|error| error.map_packet(drop))
    .expect("a frame");
  let empty = audio_on_zero(audio_holding(&frame, &[]));
  let (refusal, back) = by_container(muxer.write_audio(empty.clone()));
  assert_eq!(refusal, payload(PayloadRule::Empty));
  untouched!(empty, back);
  let mut updated = streaminfo.clone();
  *updated.last_mut().expect("bytes") ^= 0xFF;
  let mut configuring = audio_holding(&frame, &[]);
  configuring
    .extra_mut()
    .set_side_data(vec![side(T::AV_PKT_DATA_NEW_EXTRADATA, &updated)]);
  muxer
    .write_audio(audio_on_zero(configuring))
    .map_err(|error| error.map_packet(drop))
    .expect("the STREAMINFO, of no frame");
  muxer.finish().expect("finished");
  let (written, _) = first_track(&target, TrackKind::Audio, 0);
  assert_eq!(
    written[0].side_data().extradata(),
    &updated[..],
    "the header's"
  );
  assert_eq!(packet_count(&target), 1, "no frame of it");
}

/// A one-track video table of `path`'s first video track, its stream
/// stated `width` × `height`.
fn video_sized(
  path: &Path,
  width: i32,
  height: i32,
) -> Vec<TrackDescription<Ffmpeg, FfmpegBytes, CodecTicket>> {
  let mut table = table_of(path, TrackKind::Video);
  let index = table[0].side_data().stream_index();
  let mut parameters = table[0].side_data().rebuild().expect("the parameters");
  // SAFETY: `parameters` owns its `AVCodecParameters`, live for the
  // writes; `width` and `height` are plain integer fields.
  unsafe {
    let par = parameters.as_mut_ptr();
    (*par).width = width;
    (*par).height = height;
  }
  *table[0].side_data_mut() =
    CodecTicket::mirror(&parameters, index, usize::MAX).expect("the ticket");
  table
}

/// **L77 — IVF refuses a dimension past its 16-bit field, and reports the
/// decode times and the side data it does not write** (table H;
/// `ivfenc.c` 54–88): a width of 65536 refused at open
/// ([`MuxOpenError::Dimensions`]), nothing written; a packet decoded
/// before it is presented reported at that packet
/// ([`Rewrite::DecodeTimes`]), read back at its presentation time; side
/// data reported dropped, by name.
///
/// PLANT: the dimension unjudged opens the track.
#[test]
fn l77_ivf_refuses_a_dimension_past_its_field() {
  use ffmpeg_next::ffi::AVPacketSideDataType as T;
  let Some(corpus) = Corpus::new() else { return };
  let vp8 = generated(
    &corpus,
    "vp8.webm",
    &[
      "-f",
      "lavfi",
      "-i",
      "testsrc2=size=64x64:rate=25:duration=0.2",
      "-c:v",
      "libvpx",
    ],
  );
  let target = output(&corpus, "l77-wide.ivf");
  let refused = FfmpegOwnedMuxer::open(
    File::create(&target).expect("create"),
    ContainerName::new("ivf"),
    video_sized(&vp8, 65536, 64),
    &[],
    PacketLimits::default(),
  );
  assert!(
    matches!(refused, Err(MuxOpenError::Dimensions { track }) if track.get() == 0),
    "{:?}",
    refused.map(drop)
  );
  assert_eq!(std::fs::metadata(&target).expect("the file").len(), 0);

  let (table, frames) = first_track(&vp8, TrackKind::Video, 2);
  let target = output(&corpus, "l77.ivf");
  let mut muxer = muxer_into(&target, ContainerName::new("ivf"), table);
  let track = TrackIndex::new(0);
  muxer
    .write_video(video_at(&video_of(&frames[0]), Some(0), 0))
    .map_err(|error| error.map_packet(drop))
    .expect("the first frame");
  assert!(
    lines(muxer.report(), |line| matches!(
      line,
      Uncarried::Rewritten {
        rewrite: Rewrite::DecodeTimes,
        ..
      }
    ))
    .is_empty(),
    "times that agree: nothing"
  );
  let mut late = video_of(&frames[1])
    .with_pts(Some(ms(80)))
    .with_dts(Some(ms(40)));
  late
    .extra_mut()
    .set_side_data(vec![side(T::AV_PKT_DATA_A53_CC, &[1, 2, 3])]);
  muxer
    .write_video(VideoTrackPacket::new(track, late))
    .map_err(|error| error.map_packet(drop))
    .expect("a frame decoded before it is presented");
  assert_eq!(
    lines(muxer.report(), |line| matches!(
      line,
      Uncarried::Rewritten {
        rewrite: Rewrite::DecodeTimes,
        ..
      } | Uncarried::SideData { .. }
    )),
    vec![
      Uncarried::SideData {
        track,
        kind: T::AV_PKT_DATA_A53_CC as i32,
        name: Some("A53 Closed Captions"),
        fate: SideDataFate::Dropped,
      },
      Uncarried::Rewritten {
        track,
        rewrite: Rewrite::DecodeTimes,
      },
    ]
  );
  muxer.finish().expect("finished");
  let back = &packets(&target)[0];
  assert_eq!(back[1].pts, Some(ms(80)), "presented as offered");
  assert_ne!(back[1].dts, Some(ms(40)), "but not decoded as offered");
}

/// **L78 — WAV refuses the packet that would take its file past what its
/// RIFF header counts, and Wave64 takes it** (table H; `wavenc.c`
/// 418–495): with the 4 GiB lowered to 10 000 bytes, PCM packets are taken
/// until the one after which the file, less eight, would pass it
/// ([`PayloadRule::FileSize`]), refused by name, untouched; Wave64, whose
/// sizes are 64-bit, takes them all.
///
/// PLANT: the size unjudged takes the packet.
#[test]
fn l78_wav_refuses_a_file_past_its_32_bit_size() {
  let Some(corpus) = Corpus::new() else { return };
  let pcm = pcm_mkv(&corpus);
  let (table, frames) = whole_track(&pcm, TrackKind::Audio);
  const BOUND: u64 = 10_000;
  let options = MuxOptions::new().with_lowered_file_bound(BOUND);
  let target = output(&corpus, "l78.wav");
  let mut muxer = muxer_with(&target, ContainerName::new("wav"), table, options);
  let mut taken = 0;
  let refused = frames.iter().find_map(|frame| {
    let offered = audio_on_zero(audio_of(frame));
    match muxer.write_audio(offered.clone()) {
      Ok(()) => {
        taken += 1;
        None
      }
      Err(error) => Some((offered, error)),
    }
  });
  let (offered, error) = refused.expect("a packet past the bound");
  let (refusal, back) = by_container(Err::<(), _>(error));
  assert_eq!(refusal, payload(PayloadRule::FileSize));
  untouched!(offered, back);
  assert!(taken > 0, "the packets within it taken");
  muxer.finish().expect("finished");
  assert!(std::fs::metadata(&target).expect("the file").len() - 8 <= BOUND);

  let (table, frames) = whole_track(&pcm, TrackKind::Audio);
  let options = MuxOptions::new().with_lowered_file_bound(BOUND);
  let mut muxer = muxer_with(
    &output(&corpus, "l78.w64"),
    ContainerName::new("w64"),
    table,
    options,
  );
  for frame in &frames {
    muxer
      .write_audio(audio_on_zero(audio_of(frame)))
      .map_err(|error| error.map_packet(drop))
      .expect("Wave64 counts in 64 bits");
  }
  muxer.finish().expect("finished");
}

/// **L79 — a raw stream refuses what it would write unreadable, and an
/// empty packet** (table H; `rawenc.c` 31–35, 386–443): H.264 into raw
/// H.264 with the filters off, length-prefixed, refused
/// ([`PayloadRule::StartCode`]) — the filter on converts it; MPEG-4 Part 2
/// from Matroska, its video object layer in the track's configuration
/// alone, refused at its first packet ([`PayloadRule::InBandConfiguration`]);
/// an empty AC-3 packet, which the muxer writes as nothing
/// ([`PayloadRule::Empty`]). Each by name, untouched.
///
/// PLANT: each check removed takes its packet.
#[test]
fn l79_a_raw_stream_refuses_what_it_would_write_unreadable() {
  let Some(corpus) = Corpus::new() else { return };
  let source = corpus.multi_track_mkv();
  let (table, video) = first_track(&source, TrackKind::Video, 1);
  let mut muxer = muxer_with(
    &output(&corpus, "l79.h264"),
    ContainerName::new("h264"),
    table,
    filters(false),
  );
  let offered = video_at(&video_of(&video[0]), Some(0), 0);
  let (refusal, back) = by_container(muxer.write_video(offered.clone()));
  assert_eq!(refusal, payload(PayloadRule::StartCode), "length-prefixed");
  untouched!(offered, back);
  muxer.finish().expect("finished");

  let mpeg4 = generated(
    &corpus,
    "mpeg4.mkv",
    &[
      "-f",
      "lavfi",
      "-i",
      "testsrc2=size=64x64:rate=25:duration=0.2",
      "-c:v",
      "mpeg4",
    ],
  );
  let (table, video) = first_track(&mpeg4, TrackKind::Video, 1);
  assert!(
    !table[0].side_data().extradata().is_empty(),
    "the premise: the configuration out of band"
  );
  let mut muxer = muxer_into(
    &output(&corpus, "l79.m4v"),
    ContainerName::new("m4v"),
    table,
  );
  let offered = video_at(&video_of(&video[0]), Some(0), 0);
  let (refusal, back) = by_container(muxer.write_video(offered.clone()));
  assert_eq!(refusal, payload(PayloadRule::InBandConfiguration));
  untouched!(offered, back);
  muxer.finish().expect("finished");

  let ac3 = audio_file(&corpus, "ac3.mkv", "ac3");
  let (table, audio) = first_track(&ac3, TrackKind::Audio, 1);
  let mut muxer = muxer_into(
    &output(&corpus, "l79.ac3"),
    ContainerName::new("ac3"),
    table,
  );
  let empty = audio_on_zero(audio_holding(&audio_of(&audio[0]), &[]));
  let (refusal, back) = by_container(muxer.write_audio(empty.clone()));
  assert_eq!(refusal, payload(PayloadRule::Empty));
  untouched!(empty, back);
  muxer.finish().expect("finished");
}

/// **L80 — the chapters each simple muxer writes, and those it does not**
/// (table H; table E's chapter rows): MP3's ID3v2 chapters, numbered by
/// position from 0 and counted in milliseconds — a chapter's id other
/// than its position reported ([`Uncarried::ChapterId`]), a span off the
/// millisecond reported ([`Uncarried::ChapterSpan`]), each read back by
/// its title; WAV's and the raw streams' none, each chapter reported
/// ([`Uncarried::Chapter`]). MP3's table of contents counts its chapters
/// in 8 bits (`id3v2enc.c` 274; R9, the class): 255 open, 256 refused by
/// name ([`MuxOpenError::ChapterCount`]), nothing written.
///
/// PLANT: the chapters a container does not write unreported; MP3's count
/// unjudged opens the 256.
#[test]
fn l80_the_chapters_each_simple_muxer_writes() {
  let Some(corpus) = Corpus::new() else { return };
  let mp3 = audio_file(&corpus, "mp3.mkv", "libmp3lame");
  let chapters = [
    chapter_of(0, 0, 200, Some("first")),
    Chapter::<Ffmpeg>::new(
      7,
      Timebase::new(1, NonZeroI32::new(2000).expect("non-zero")),
      Timestamp::new(
        400,
        Timebase::new(1, NonZeroI32::new(2000).expect("non-zero")),
      ),
      Timestamp::new(
        801,
        Timebase::new(1, NonZeroI32::new(2000).expect("non-zero")),
      ),
    )
    .with_title(Some("second".into())),
  ];
  let target = output(&corpus, "l80.mp3");
  let (mut muxer, report) = FfmpegOwnedMuxer::open(
    File::create(&target).expect("create"),
    ContainerName::new("mp3"),
    table_of(&mp3, TrackKind::Audio),
    &chapters,
    PacketLimits::default(),
  )
  .expect("open");
  assert_eq!(
    lines(&report, |line| matches!(
      line,
      Uncarried::Chapter { .. } | Uncarried::ChapterId { .. } | Uncarried::ChapterSpan { .. }
    )),
    vec![
      Uncarried::ChapterId { chapter: 1, id: 7 },
      Uncarried::ChapterSpan { chapter: 1 },
    ]
  );
  let (_, frames) = whole_track(&mp3, TrackKind::Audio);
  for frame in &frames {
    muxer
      .write_audio(audio_on_zero(audio_of(frame)))
      .map_err(|error| error.map_packet(drop))
      .expect("a frame");
  }
  muxer.finish().expect("finished");
  let demuxer = FfmpegOwnedDemuxer::open(&target).expect("open");
  let titles: Vec<_> = demuxer
    .chapters()
    .iter()
    .map(|chapter| chapter.title().map(|title| title.as_str().to_owned()))
    .collect();
  assert_eq!(
    titles,
    [Some("first".to_owned()), Some("second".to_owned())]
  );
  let many = |count: i64| -> Vec<Chapter<Ffmpeg>> {
    (0..count)
      .map(|at| chapter_of(at, at * 10, at * 10 + 10, Some("a chapter")))
      .collect()
  };
  for (count, opens) in [(255, true), (256, false)] {
    let target = output(&corpus, &format!("l80-{count}.mp3"));
    let opened = FfmpegOwnedMuxer::open(
      File::create(&target).expect("create"),
      ContainerName::new("mp3"),
      table_of(&mp3, TrackKind::Audio),
      &many(count),
      PacketLimits::default(),
    );
    match opened {
      Ok((muxer, _)) => {
        assert!(opens, "{count} opened");
        muxer.finish().expect("finished");
      }
      Err(refusal) => {
        assert!(!opens, "{count}: {refusal:?}");
        assert!(
          matches!(refusal, MuxOpenError::ChapterCount { count: 256 }),
          "{refusal:?}"
        );
        assert_eq!(
          std::fs::metadata(&target).expect("the file").len(),
          0,
          "nothing written"
        );
      }
    }
  }

  for (container, source) in [
    ("wav", pcm_mkv(&corpus)),
    ("ac3", audio_file(&corpus, "ac3.mkv", "ac3")),
  ] {
    let (_, report) = FfmpegOwnedMuxer::open(
      File::create(output(&corpus, &format!("l80.{container}"))).expect("create"),
      ContainerName::new(container),
      table_of(&source, TrackKind::Audio),
      &chapters,
      PacketLimits::default(),
    )
    .expect("open");
    assert_eq!(
      lines(&report, |line| matches!(line, Uncarried::Chapter { .. })),
      vec![
        Uncarried::Chapter { chapter: 0 },
        Uncarried::Chapter { chapter: 1 }
      ],
      "{container}"
    );
  }
}

/// A video and an audio track of `video` and `audio` in the container
/// `name` names, made once. Matroska stores no decode time for a frame
/// that reorders, MP4 does.
fn picture_and_sound(corpus: &Corpus, name: &str, video: &[&str], audio: &[&str]) -> PathBuf {
  let mut args = vec![
    "-f",
    "lavfi",
    "-i",
    "testsrc2=size=64x48:rate=25:duration=1",
    "-f",
    "lavfi",
    "-i",
    "sine=frequency=440:sample_rate=44100:duration=1",
    "-map",
    "0:v",
    "-map",
    "1:a",
  ];
  args.extend_from_slice(video);
  args.extend_from_slice(audio);
  generated(corpus, name, &args)
}

/// Every packet of `path`'s tracks as they read back, by track: bytes, and
/// times in milliseconds.
fn timed_bytes(path: &Path) -> Vec<Vec<(Vec<u8>, Option<i64>, Option<i64>)>> {
  let to_ms = |time: Option<Timestamp>| {
    time.map(|time| {
      time
        .checked_rescale_with(millis(), Rounding::Nearest)
        .map(|time| time.pts())
        .expect("a time in milliseconds")
    })
  };
  packets(path)
    .into_iter()
    .map(|track| {
      track
        .into_iter()
        .map(|seen| (seen.bytes, to_ms(seen.pts), to_ms(seen.dts)))
        .collect()
    })
    .collect()
}

/// **L81 — H.264 that reorders and AAC round-trip through FLV, and the
/// report says what FLV does not write** (table H; `flvenc.c` 1209–1486):
/// every packet reads back with its bytes, its decode time and — H.264's
/// through its composition time — its presentation time, each moved by
/// the distance MP4's first decode time lay before zero, libavformat's
/// move ([`Uncarried::TimesShifted`]); FLV writes no duration
/// ([`Rewrite::Durations`]), reported at open.
#[test]
fn l81_a_stream_round_trips_through_flv() {
  let Some(corpus) = Corpus::new() else { return };
  let source = picture_and_sound(
    &corpus,
    "h264-aac.mp4",
    &[
      "-c:v", "libx264", "-preset", "medium", "-bf", "2", "-g", "25", "-pix_fmt", "yuv420p",
    ],
    &["-c:a", "aac", "-ac", "2"],
  );
  let target = output(&corpus, "l81.flv");
  let report = remux_through_ffmpeg(&source, ContainerName::new("flv"), &target);
  for track in [0, 1] {
    assert!(
      report.uncarried().contains(&Uncarried::Rewritten {
        track: TrackIndex::new(track),
        rewrite: Rewrite::Durations,
      }),
      "track {track}: {report:?}"
    );
  }
  // MP4's first decode time lies before zero, which FLV does not take:
  // libavformat moves every stream later by it (`mux.c` 636–717).
  let offered = timed_bytes(&source);
  let back = timed_bytes(&target);
  let lift = -offered[0][0].2.expect("a decode time");
  assert!(lift > 0, "the premise: decoded before zero");
  assert_eq!(back.len(), 2);
  for (track, times_of) in [(0, true), (1, false)] {
    assert_eq!(
      back[track].len(),
      offered[track].len(),
      "track {track}: a packet a packet"
    );
    for (index, (seen, sent)) in back[track].iter().zip(&offered[track]).enumerate() {
      assert!(seen.0 == sent.0, "track {track}, packet {index}: the bytes");
      assert_eq!(
        seen.2,
        sent.2.map(|dts| dts + lift),
        "track {track}, packet {index}: decoded"
      );
      if times_of {
        assert_eq!(
          seen.1,
          sent.1.map(|pts| pts + lift),
          "track {track}, packet {index}: presented"
        );
      }
    }
  }
}

/// **L82 — FLV's refusals** (table H; `flvenc.c` 975–1486): a packet of a
/// reordering H.264 stream with no presentation time
/// ([`FfmpegMuxRefusal::MissingPts`]); a decode time of 2^32 ms, past its
/// 32 bits ([`FfmpegMuxRefusal::TimeWraps`]); a presentation 2^23 ms after
/// its decode, past its 24-bit composition time
/// ([`FfmpegMuxRefusal::PresentationOffset`]); a tag of 2^24 bytes with its
/// flags ([`PayloadRule::TagSize`]); with the filters off, ADTS AAC on a
/// configured track ([`PayloadRule::AdtsHeader`]); AAC with no
/// configuration whose first packet brings none
/// ([`PayloadRule::ConfigurationAbsent`]) — one that brings new extradata
/// taken. At open: Opus with no configuration, which FLV's header asserts,
/// and H.264 with none and the filters off
/// ([`MuxOpenError::ConfigurationMissing`]); VP8, which FLV's start admits
/// in a second video track and its packets refuse after taking (`flvenc.c`
/// 997–1007, 1424–1429), is in no FLV tag table, and is refused at open
/// first ([`MuxOpenError::CodecNotCarried`]).
///
/// PLANT: each check removed takes its packet, or opens.
#[test]
fn l82_flv_refuses_what_its_tags_cannot_carry() {
  use ffmpeg_next::ffi::AVPacketSideDataType as T;
  let Some(corpus) = Corpus::new() else { return };
  let flv = || ContainerName::new("flv");
  let source = picture_and_sound(
    &corpus,
    "h264-aac.mkv",
    &[
      "-c:v", "libx264", "-preset", "medium", "-bf", "2", "-g", "25", "-pix_fmt", "yuv420p",
    ],
    &["-c:a", "aac", "-ac", "2"],
  );
  let (table, frames) = first_track(&source, TrackKind::Video, 1);
  let frame = video_of(&frames[0]);
  let mut muxer = muxer_into(&output(&corpus, "l82.flv"), flv(), table);
  muxer
    .write_video(video_at(&frame, Some(0), 0))
    .map_err(|error| error.map_packet(drop))
    .expect("the first frame");
  let track = TrackIndex::new(0);
  let at = |pts: Option<i64>, dts: i64| {
    VideoTrackPacket::new(
      track,
      frame.clone().with_pts(pts.map(ms)).with_dts(Some(ms(dts))),
    )
  };
  for (offered, refusal) in [
    (at(None, 40), FfmpegMuxRefusal::MissingPts { track }),
    (
      at(Some(1 << 32), 1 << 32),
      FfmpegMuxRefusal::TimeWraps {
        track,
        timestamp: ms(1 << 32),
      },
    ),
    (
      at(Some(80 + (1 << 23)), 80),
      FfmpegMuxRefusal::PresentationOffset {
        track,
        pts: ms(80 + (1 << 23)),
        dts: ms(80),
      },
    ),
  ] {
    let (refused, back) = by_container(muxer.write_video(offered.clone()));
    assert_eq!(refused, refusal);
    untouched!(offered, back);
  }
  let large = VideoTrackPacket::new(
    track,
    video_holding(&frame, &vec![0x21; (1 << 24) - 5])
      .with_pts(Some(ms(120)))
      .with_dts(Some(ms(120))),
  );
  let (refused, back) = by_container(muxer.write_video(large.clone()));
  assert_eq!(
    refused,
    payload(PayloadRule::TagSize),
    "2^24 with its five flags"
  );
  untouched!(large, back);
  muxer.finish().expect("finished");

  let (table, audio) = first_track(&source, TrackKind::Audio, 1);
  let raw = audio_of(&audio[0]);
  let mut muxer = muxer_with(
    &output(&corpus, "l82-off.flv"),
    flv(),
    table,
    filters(false),
  );
  let framed = audio_on_zero(audio_holding(&raw, &adts_frame_of(raw.data().as_ref(), 2)));
  let (refused, back) = by_container(muxer.write_audio(framed.clone()));
  assert_eq!(
    refused,
    payload(PayloadRule::AdtsHeader),
    "no filter converts it"
  );
  untouched!(framed, back);
  muxer.finish().expect("finished");

  let ts = corpus.transport_stream();
  let (table, frames) = first_track(&ts, TrackKind::Audio, 1);
  let first = audio_of(&frames[0]);
  let bytes = first.data().as_ref();
  let unframed = audio_holding(&first, &bytes[if bytes[1] & 1 == 1 { 7 } else { 9 }..]);
  let mut muxer = muxer_into(&output(&corpus, "l82-absent.flv"), flv(), table);
  let bare = audio_on_zero(unframed.clone());
  let (refused, back) = by_container(muxer.write_audio(bare.clone()));
  assert_eq!(refused, payload(PayloadRule::ConfigurationAbsent));
  untouched!(bare, back);
  let mut configuring = unframed;
  configuring
    .extra_mut()
    .set_side_data(vec![side(T::AV_PKT_DATA_NEW_EXTRADATA, &[0x11, 0x90])]);
  muxer
    .write_audio(audio_on_zero(configuring))
    .map_err(|error| error.map_packet(drop))
    .expect("a raw frame bringing its configuration");
  muxer.finish().expect("finished");

  let open = |name: &str, table, options: MuxOptions| {
    FfmpegOwnedMuxer::open_with(
      File::create(output(&corpus, name)).expect("create"),
      flv(),
      table,
      &[],
      options,
    )
    .map(drop)
  };
  assert!(matches!(
    open(
      "l82-opus.flv",
      audio_with(&opus_mkv(&corpus), -99, Some(&[])),
      MuxOptions::new()
    ),
    Err(MuxOpenError::ConfigurationMissing { codec, .. }) if codec == CodecId::OPUS
  ));
  let bare = table_of(&source, TrackKind::Video)
    .into_iter()
    .map(|row| row.with_extradata(Some(bytes_of(&[]))))
    .collect();
  assert!(matches!(
    open("l82-bare.flv", bare, filters(false)),
    Err(MuxOpenError::ConfigurationMissing { codec, .. }) if codec == CodecId::H264
  ));
  let vp8 = generated(
    &corpus,
    "vp8.webm",
    &[
      "-f",
      "lavfi",
      "-i",
      "testsrc2=size=64x64:rate=25:duration=0.2",
      "-c:v",
      "libvpx",
    ],
  );
  let mut two = table_of(&source, TrackKind::Video);
  two.extend(table_of(&vp8, TrackKind::Video));
  assert!(matches!(
    open("l82-two.flv", two, MuxOptions::new()),
    Err(MuxOpenError::CodecNotCarried { track, codec }) if track.get() == 1 && codec == CodecId::VP8
  ));
}

/// **L83 — MPEG-4 Part 2 and PCM round-trip through AVI, each time kept as
/// the packet's place** (table H; `avienc.c` 744–913): video counted in
/// its stream's ticks, each a frame, the gaps between frames filled with
/// empty ones its reader skips; PCM counted in samples. Every packet reads
/// back with its bytes and its times; AVI writes no duration
/// ([`Rewrite::Durations`]).
#[test]
fn l83_a_stream_round_trips_through_avi() {
  let Some(corpus) = Corpus::new() else { return };
  let source = picture_and_sound(
    &corpus,
    "mpeg4-pcm.mkv",
    &["-c:v", "mpeg4"],
    &["-c:a", "pcm_s16le", "-ac", "2"],
  );
  let target = output(&corpus, "l83.avi");
  let report = remux_through_ffmpeg(&source, ContainerName::new("avi"), &target);
  for track in [0, 1] {
    assert!(
      report.uncarried().contains(&Uncarried::Rewritten {
        track: TrackIndex::new(track),
        rewrite: Rewrite::Durations,
      }),
      "track {track}: {report:?}"
    );
  }
  let offered = timed_bytes(&source);
  let back = timed_bytes(&target);
  assert_eq!(back.len(), 2);
  assert_eq!(
    back[0].len(),
    offered[0].len(),
    "video: a packet a packet, the empty ones skipped"
  );
  for (index, (seen, sent)) in back[0].iter().zip(&offered[0]).enumerate() {
    assert!(seen.0 == sent.0, "video {index}: the bytes");
    assert_eq!(seen.2, sent.2, "video {index}: decoded");
  }
  let pcm = |track: &[(Vec<u8>, Option<i64>, Option<i64>)]| -> Vec<u8> {
    track.iter().flat_map(|(bytes, ..)| bytes.clone()).collect()
  };
  assert!(pcm(&back[1]) == pcm(&offered[1]), "PCM: the samples");
}

/// **L84 — AVI's refusals, and what it moves** (table H; `avienc.c`
/// 744–913): a frame more than 60 000 frames after the stream's place,
/// which AVI would fill with empty frames no further
/// ([`FfmpegMuxRefusal::FrameGap`]) — one 60 000 after taken; H.264 it
/// tags `H264` not opening on a start code, which it checks and inserts no
/// filter for ([`PayloadRule::StartCode`]); raw RGB video it would write
/// as a bitmap of another pixel format, "unreadable"
/// ([`MuxOpenError::ConfigurationInvalid`]). A PCM packet whose time is
/// not the samples before it moves to them, reported
/// ([`Rewrite::Timeline`]); MPEG-4 Part 2 presented in another order than
/// decoded loses its presentation times ([`Rewrite::PresentationTimes`]).
/// XSUB, whose gaps AVI fills not (`avienc.c` 752–753; R9: Codex R8
/// [medium]), has its own road — a cue placed one packet on however far on
/// its time, reported where it is not its place, the unit law
/// `an_avi_xsub_cue_is_placed_one_packet_on_however_far` — but AVI's tag
/// tables, the bitmap and WAVE ones, name no XSUB (`riff.c`), so that
/// `avformat_query_codec` answers it not carried and it is refused at open
/// ([`MuxOpenError::CodecNotCarried`]), nothing written.
///
/// PLANT: each check removed takes its packet, or opens; the report
/// unmade.
#[test]
fn l84_avi_refuses_what_it_cannot_place() {
  let Some(corpus) = Corpus::new() else { return };
  let avi = || ContainerName::new("avi");
  let track = TrackIndex::new(0);
  let source = picture_and_sound(
    &corpus,
    "mpeg4-pcm.mkv",
    &["-c:v", "mpeg4"],
    &["-c:a", "pcm_s16le", "-ac", "2"],
  );
  let (table, video) = first_track(&source, TrackKind::Video, 1);
  let frame = video_of(&video[0]);
  let mut muxer = muxer_into(&output(&corpus, "l84.avi"), avi(), table);
  muxer
    .write_video(video_at(&frame, Some(0), 0))
    .map_err(|error| error.map_packet(drop))
    .expect("the first frame: place 1 after it");
  muxer
    .write_video(video_at(&frame, Some(60_001), 60_001))
    .map_err(|error| error.map_packet(drop))
    .expect("60 000 empty frames before it");
  let far = video_at(&frame, Some(120_003), 120_003);
  let (refusal, back) = by_container(muxer.write_video(far.clone()));
  assert_eq!(
    refusal,
    FfmpegMuxRefusal::FrameGap {
      track,
      dts: ms(120_003),
    }
  );
  untouched!(far, back);
  muxer.finish().expect("finished");

  let h264 = corpus.multi_track_mkv();
  let (table, video) = first_track(&h264, TrackKind::Video, 1);
  let mut muxer = muxer_into(&output(&corpus, "l84-h264.avi"), avi(), table);
  let offered = video_at(&video_of(&video[0]), Some(0), 0);
  let (refusal, back) = by_container(muxer.write_video(offered.clone()));
  assert_eq!(
    refusal,
    payload(PayloadRule::StartCode),
    "length-prefixed, tagged H264"
  );
  untouched!(offered, back);
  muxer.finish().expect("finished");

  let rgb = generated(
    &corpus,
    "rgb24.mov",
    &[
      "-f",
      "lavfi",
      "-i",
      "testsrc2=size=32x32:rate=25:duration=0.2",
      "-c:v",
      "rawvideo",
      "-pix_fmt",
      "rgb24",
    ],
  );
  let refused = FfmpegOwnedMuxer::open(
    File::create(output(&corpus, "l84-rgb.avi")).expect("create"),
    avi(),
    table_of(&rgb, TrackKind::Video),
    &[],
    PacketLimits::default(),
  );
  assert!(
    matches!(refused, Err(MuxOpenError::ConfigurationInvalid { track, .. }) if track.get() == 0),
    "{:?}",
    refused.map(drop)
  );

  let (table, audio) = first_track(&source, TrackKind::Audio, 2);
  let mut muxer = muxer_into(&output(&corpus, "l84-pcm.avi"), avi(), table);
  muxer
    .write_audio(audio_on_zero(
      audio_of(&audio[0])
        .with_pts(Some(ms(0)))
        .with_dts(Some(ms(0))),
    ))
    .map_err(|error| error.map_packet(drop))
    .expect("the first samples");
  assert!(
    lines(muxer.report(), |line| matches!(
      line,
      Uncarried::Rewritten {
        rewrite: Rewrite::Timeline,
        ..
      }
    ))
    .is_empty(),
    "at its place"
  );
  muxer
    .write_audio(audio_on_zero(
      audio_of(&audio[1])
        .with_pts(Some(ms(500)))
        .with_dts(Some(ms(500))),
    ))
    .map_err(|error| error.map_packet(drop))
    .expect("samples after a gap, at the place after the first");
  assert_eq!(
    lines(muxer.report(), |line| matches!(
      line,
      Uncarried::Rewritten {
        rewrite: Rewrite::Timeline,
        ..
      }
    )),
    vec![Uncarried::Rewritten {
      track,
      rewrite: Rewrite::Timeline,
    }]
  );
  muxer.finish().expect("finished");

  let reordering = picture_and_sound(
    &corpus,
    "mpeg4-bframes.mp4",
    &["-c:v", "mpeg4", "-bf", "2"],
    &["-c:a", "pcm_s16le", "-ac", "2"],
  );
  let (table, video) = whole_track(&reordering, TrackKind::Video);
  let mut muxer = muxer_into(&output(&corpus, "l84-bframes.avi"), avi(), table);
  for packet in &video {
    muxer
      .write_video(VideoTrackPacket::new(track, video_of(packet)))
      .map_err(|error| error.map_packet(drop))
      .expect("a frame");
  }
  assert!(
    muxer.report().uncarried().contains(&Uncarried::Rewritten {
      track,
      rewrite: Rewrite::PresentationTimes,
    }),
    "presented in another order"
  );
  muxer.finish().expect("finished");

  let ass = generated(
    &corpus,
    "ass.mkv",
    &[
      "-i",
      written_text(
        &corpus,
        "two.srt",
        "1\n00:00:00,000 --> 00:00:01,000\nhello\n\n2\n00:00:01,000 --> 00:00:02,000\nworld\n\n",
      )
      .to_str()
      .expect("utf-8 path"),
      "-c:s",
      "ass",
    ],
  );
  let (table, cues) = first_track(&ass, TrackKind::Subtitle, 2);
  let xsub = recoded(
    table.into_iter().next().expect("a row"),
    ffmpeg_next::ffi::AVCodecID::AV_CODEC_ID_XSUB,
    &[],
  );
  drop(cues);
  let target = output(&corpus, "l84-xsub.avi");
  let refused = FfmpegOwnedMuxer::open(
    File::create(&target).expect("create"),
    avi(),
    vec![xsub],
    &[],
    PacketLimits::default(),
  );
  assert!(
    matches!(refused, Err(MuxOpenError::CodecNotCarried { codec, .. })
      if codec.raw() == ffmpeg_next::ffi::AVCodecID::AV_CODEC_ID_XSUB as i32),
    "{:?}",
    refused.map(drop)
  );
  assert_eq!(std::fs::metadata(&target).expect("the file").len(), 0);
}

/// **L85 — WMV2 and WMA2 round-trip through ASF and its streaming form,
/// and the report says what ASF changes** (table H; `asfenc.c` 893–1050):
/// every packet reads back with its bytes and its decode time — written as
/// its presentation time, moved by libavformat's shift — and a language
/// whose ISO 639-1 code turns back into it, `eng`, is kept, while `fra`,
/// which turns back into `fre`, and `ace`, which ISO 639-1 names no code
/// for, are reported ([`Uncarried::Language`]) — `ace` not handed to ASF,
/// which would number it as the first language it lists, so that it reads
/// back with none, not as `eng`; ASF writes no duration
/// ([`Rewrite::Durations`]); a chapter's end it does not write, its reader
/// numbering them from 0 and ending each where the next starts, the last
/// at the file's end ([`Uncarried::ChapterSpan`]). The streaming form
/// writes no chapter ([`Uncarried::Chapter`]), and wraps its header and
/// each packet in a chunk, which FFmpeg reads only over its MMS-over-HTTP
/// protocol: unwrapped, it reads back the same.
///
/// PLANT: the language rule as given, `fra` unreported; `ace` handed,
/// read back as `eng`; the markers' rule off, chapter 1's span unreported;
/// the streaming form's chapters judged as markers, its losses unreported.
#[test]
fn l85_a_stream_round_trips_through_asf() {
  let Some(corpus) = Corpus::new() else { return };
  let source = picture_and_sound(
    &corpus,
    "wmv2-wma2.mkv",
    &["-c:v", "wmv2"],
    &["-c:a", "wmav2", "-ac", "2"],
  );
  let chapters = [
    chapter_of(0, 0, 500, Some("one")),
    chapter_of(1, 500, 1000, Some("two")),
  ];
  for container in ["asf", "asf_stream"] {
    let target = output(&corpus, &format!("l85.{container}"));
    let demuxer = FfmpegOwnedDemuxer::open(&source).expect("open");
    let mut table: Vec<TrackDescription<Ffmpeg, FfmpegBytes, CodecTicket>> = demuxer
      .tracks()
      .iter()
      .map(|row| {
        TrackDescription::from_track_info(&**row, None, ticket(row)).expect("a description")
      })
      .collect();
    // `ace` beside `eng` in the file, `fra` in the streaming form, whose
    // reading back is the same writer's.
    let second = if container == "asf" { "ace" } else { "fra" };
    table[0].set_language(Some("eng".into()));
    table[1].set_language(Some(second.into()));
    let (mut muxer, report) = FfmpegOwnedMuxer::open(
      File::create(&target).expect("create"),
      ContainerName::new(container),
      table,
      &chapters,
      PacketLimits::default(),
    )
    .expect("open");
    let languages = lines(&report, |line| matches!(line, Uncarried::Language { .. }));
    assert_eq!(
      languages,
      vec![Uncarried::Language {
        track: TrackIndex::new(1),
        tag: second.into(),
      }],
      "{container}"
    );
    let durations = |track| Uncarried::Rewritten {
      track: TrackIndex::new(track),
      rewrite: Rewrite::Durations,
    };
    assert_eq!(
      field_rows(&report),
      vec![durations(0), durations(1)],
      "{container}"
    );
    let chapter_lines = lines(&report, |line| {
      matches!(
        line,
        Uncarried::Chapter { .. } | Uncarried::ChapterSpan { .. } | Uncarried::ChapterId { .. }
      )
    });
    assert_eq!(
      chapter_lines,
      if container == "asf" {
        vec![Uncarried::ChapterSpan { chapter: 1 }]
      } else {
        vec![
          Uncarried::Chapter { chapter: 0 },
          Uncarried::Chapter { chapter: 1 },
        ]
      },
      "{container}"
    );
    let mut demuxer = FfmpegOwnedDemuxer::open(&source).expect("open");
    while let Some(packet) = demuxer.next_packet().expect("a packet or the end") {
      match packet {
        DemuxedPacket::Video(p) => muxer
          .write_video(VideoTrackPacket::new(p.track(), p.packet().clone()))
          .map_err(|error| error.map_packet(drop)),
        DemuxedPacket::Audio(p) => muxer
          .write_audio(AudioTrackPacket::new(p.track(), p.packet().clone()))
          .map_err(|error| error.map_packet(drop)),
        other => panic!("{other:?}"),
      }
      .expect("a packet");
    }
    muxer.finish().expect("finished");
    // The streaming form wraps the header and each packet in a chunk of its
    // own — two bytes of type, two of length, eight more — which FFmpeg
    // reads only through its MMS-over-HTTP protocol (`put_chunk`,
    // `asfenc.c` 281–295): unwrapped, it is ASF.
    let target = if container == "asf_stream" {
      let bytes = std::fs::read(&target).expect("the file");
      let (mut at, mut unwrapped) = (0, Vec::new());
      while at + 12 <= bytes.len() {
        let kind = u16::from_le_bytes([bytes[at], bytes[at + 1]]);
        let length = usize::from(u16::from_le_bytes([bytes[at + 2], bytes[at + 3]]));
        unwrapped.extend_from_slice(&bytes[at + 12..at + 4 + length]);
        at += 4 + length;
        if kind == 0x4524 {
          break;
        }
      }
      let plain = output(&corpus, "l85-unwrapped.asf");
      std::fs::write(&plain, unwrapped).expect("write");
      plain
    } else {
      target
    };
    let offered = timed_bytes(&source);
    let back = timed_bytes(&target);
    // WMA's first decode time lies before zero: libavformat moves every
    // stream later by it (`mux.c` 636–717).
    let lift = offered
      .iter()
      .filter_map(|track| track.first().and_then(|first| first.2))
      .map(|dts| -dts)
      .max()
      .unwrap_or(0)
      .max(0);
    assert_eq!(back.len(), 2, "{container}");
    for track in 0..2 {
      assert_eq!(
        back[track].len(),
        offered[track].len(),
        "{container} {track}"
      );
      for (index, (seen, sent)) in back[track].iter().zip(&offered[track]).enumerate() {
        assert!(seen.0 == sent.0, "{container} {track}/{index}: the bytes");
        assert_eq!(
          seen.2,
          sent.2.map(|dts| dts + lift),
          "{container} {track}/{index}: decoded"
        );
      }
    }
    let read = FfmpegOwnedDemuxer::open(&target).expect("open");
    assert_eq!(
      read.tracks()[0].language().map(Utf8Bytes::as_str),
      Some("eng"),
      "{container}"
    );
    assert_eq!(
      read.tracks()[1].language().map(Utf8Bytes::as_str),
      if container == "asf" {
        None
      } else {
        Some("fre")
      },
      "{container}"
    );
    let chapters: Vec<_> = read
      .chapters()
      .iter()
      .map(|chapter| {
        (
          chapter.id(),
          chapter.title().map(|title| title.as_str().to_owned()),
          span_ms(chapter).0,
        )
      })
      .collect();
    if container == "asf" {
      assert_eq!(
        chapters,
        [
          (0, Some("one".to_owned()), 0),
          (1, Some("two".to_owned()), 500)
        ]
      );
      assert_eq!(span_ms(&read.chapters()[0]), (0, 500));
    } else {
      assert!(chapters.is_empty(), "{chapters:?}");
    }
  }
}

/// **L86 — ASF refuses a decode time past its 32-bit presentation time,
/// its preroll added, and takes a late stream's early start, which its
/// interleaving waits for** (table H; `put_payload_header`, `asfenc.c`
/// 886, 937; 744): a packet at 2^32 − 3100 ms refused
/// ([`FfmpegMuxRefusal::TimeWraps`]), one a millisecond before it taken.
/// ASF sets libavformat's interleaving delta to 0, so that nothing is
/// written before every stream holds a packet: eleven seconds of video,
/// then an audio packet half a second before zero, which libavformat's
/// shift then lifts with every stream — taken, not refused as after a
/// first write. A presentation time past 2 147 479 996 900 ms, which ASF
/// refuses after taking it (`asfenc.c` 1025–1029), refused
/// ([`FfmpegMuxRefusal::PresentationOffset`]); a duration whose end passes
/// `i64` in 100 ns ticks, the preroll's added (1030–1031), refused
/// ([`FfmpegMuxRefusal::DurationTooLong`]), a millisecond shorter taken;
/// an empty packet, of which ASF writes no payload (901), refused
/// ([`PayloadRule::Empty`]).
///
/// PLANT: the field unjudged takes it; the delta counted as another
/// container's refuses the audio; the presentation unjudged, ASF's own
/// refusal after taking it; the play duration unjudged takes it; the
/// empty packet taken.
#[test]
fn l86_asf_refuses_a_time_past_its_field() {
  let Some(corpus) = Corpus::new() else { return };
  let source = picture_and_sound(
    &corpus,
    "wmv2-wma2.mkv",
    &["-c:v", "wmv2"],
    &["-c:a", "wmav2", "-ac", "2"],
  );
  let (table, video) = first_track(&source, TrackKind::Video, 1);
  let frame = video_of(&video[0]);
  let mut muxer = muxer_into(
    &output(&corpus, "l86.asf"),
    ContainerName::new("asf"),
    table,
  );
  let track = TrackIndex::new(0);
  let at = |time: i64| {
    VideoTrackPacket::new(
      track,
      frame
        .clone()
        .with_pts(Some(ms(time)))
        .with_dts(Some(ms(time))),
    )
  };
  muxer
    .write_video(at(0))
    .map_err(|error| error.map_packet(drop))
    .expect("the first");
  muxer
    .write_video(at((1 << 32) - 3101))
    .map_err(|error| error.map_packet(drop))
    .expect("within the field");
  let past = at((1 << 32) - 3100);
  let (refusal, back) = by_container(muxer.write_video(past.clone()));
  assert_eq!(
    refusal,
    FfmpegMuxRefusal::TimeWraps {
      track,
      timestamp: ms((1 << 32) - 3100),
    }
  );
  untouched!(past, back);
  muxer.finish().expect("finished");

  let demuxer = FfmpegOwnedDemuxer::open(&source).expect("open");
  let table: Vec<TrackDescription<Ffmpeg, FfmpegBytes, CodecTicket>> = demuxer
    .tracks()
    .iter()
    .map(|row| TrackDescription::from_track_info(&**row, None, ticket(row)).expect("a description"))
    .collect();
  let (_, audio) = first_track(&source, TrackKind::Audio, 1);
  let mut muxer = muxer_into(
    &output(&corpus, "l86-late.asf"),
    ContainerName::new("asf"),
    table,
  );
  for index in 0..276 {
    muxer
      .write_video(at(40 * index))
      .map_err(|error| error.map_packet(drop))
      .expect("eleven seconds of video, held");
  }
  muxer
    .write_audio(AudioTrackPacket::new(
      TrackIndex::new(1),
      audio_of(&audio[0])
        .with_pts(Some(ms(-500)))
        .with_dts(Some(ms(-500))),
    ))
    .map_err(|error| error.map_packet(drop))
    .expect("before zero, every stream lifted with it");
  muxer.finish().expect("finished");

  let (table, _) = first_track(&source, TrackKind::Video, 1);
  let mut muxer = muxer_into(
    &output(&corpus, "l86-far.asf"),
    ContainerName::new("asf"),
    table,
  );
  let presented = |pts: i64, dts: i64| {
    VideoTrackPacket::new(
      track,
      frame
        .clone()
        .with_pts(Some(ms(pts)))
        .with_dts(Some(ms(dts))),
    )
  };
  muxer
    .write_video(presented(0, 0))
    .map_err(|error| error.map_packet(drop))
    .expect("the first");
  let far = presented(2_147_479_996_901, 40);
  let (refusal, back) = by_container(muxer.write_video(far.clone()));
  assert_eq!(
    refusal,
    FfmpegMuxRefusal::PresentationOffset {
      track,
      pts: ms(2_147_479_996_901),
      dts: ms(40),
    }
  );
  untouched!(far, back);
  muxer
    .write_video(presented(10_040, 40))
    .map_err(|error| error.map_packet(drop))
    .expect("presented ten seconds after its decode");
  assert_eq!(
    lines(muxer.report(), |line| matches!(
      line,
      Uncarried::Rewritten {
        rewrite: Rewrite::PresentationTimes,
        ..
      }
    )),
    vec![Uncarried::Rewritten {
      track,
      rewrite: Rewrite::PresentationTimes,
    }],
    "ASF writes the decode time alone"
  );
  let lasting = |dts: i64, duration: i64| {
    VideoTrackPacket::new(
      track,
      frame
        .clone()
        .with_pts(Some(ms(dts)))
        .with_dts(Some(ms(dts)))
        .with_duration(Some(ms(duration))),
    )
  };
  // (80 + 3100 + duration) · 10^4 within `i64::MAX`.
  let long = lasting(80, 922_337_203_682_298);
  let (refusal, back) = by_container(muxer.write_video(long.clone()));
  assert_eq!(
    refusal,
    FfmpegMuxRefusal::DurationTooLong {
      track,
      duration: ms(922_337_203_682_298),
    }
  );
  untouched!(long, back);
  muxer
    .write_video(lasting(80, 922_337_203_682_297))
    .map_err(|error| error.map_packet(drop))
    .expect("a millisecond shorter");
  let empty = VideoTrackPacket::new(
    track,
    video_holding(&frame, &[])
      .with_pts(Some(ms(120)))
      .with_dts(Some(ms(120))),
  );
  let (refusal, back) = by_container(muxer.write_video(empty.clone()));
  assert_eq!(refusal, payload(PayloadRule::Empty));
  untouched!(empty, back);
  muxer.finish().expect("finished");
}

/// The H.264 that reorders and the AAC of L81 and L82, in `name`'s
/// container: Matroska stores no decode time for a frame that reorders,
/// MP4 does.
fn h264_aac(corpus: &Corpus, name: &str) -> PathBuf {
  picture_and_sound(
    corpus,
    name,
    &[
      "-c:v", "libx264", "-preset", "medium", "-bf", "2", "-g", "25", "-pix_fmt", "yuv420p",
    ],
    &["-c:a", "aac", "-ac", "2"],
  )
}

/// `path`'s whole table, each row described.
fn whole_table(path: &Path) -> Vec<TrackDescription<Ffmpeg, FfmpegBytes, CodecTicket>> {
  FfmpegOwnedDemuxer::open(path)
    .expect("open")
    .tracks()
    .iter()
    .map(|row| TrackDescription::from_track_info(&**row, None, ticket(row)).expect("a description"))
    .collect()
}

/// A time in milliseconds, rounded to the nearest.
fn in_ms(time: Option<Timestamp>) -> Option<i64> {
  time.map(|time| {
    time
      .checked_rescale_with(millis(), Rounding::Nearest)
      .map(|time| time.pts())
      .expect("a time in milliseconds")
  })
}

/// **L89 — H.264 that reorders and AAC round-trip through NUT, and the
/// report says what NUT keeps** (table H; `nutenc.c` 545–612, 963–1184):
/// every packet reads back with its bytes, its key flag and its
/// presentation time, moved by libavformat's shift; NUT writes no decode
/// time — its reader makes one of the presentation times and the stream's
/// decode delay — reported at the first packet whose times differ
/// ([`Rewrite::DecodeTimes`]), and no duration ([`Rewrite::Durations`]);
/// a stream's language is kept, and of its flags the default, dub,
/// original, comment, lyrics and karaoke ones, the forced flag reported
/// ([`Uncarried::Disposition`]); its chapters keep their spans and titles,
/// numbered by their position from 1 ([`Uncarried::ChapterId`]). The
/// video, of MP4's 1/12 800, is counted in 1/51 200, its tick halved until
/// NUT's 1/48 000 is reached; the audio in its sample rate's ticks.
///
/// PLANT: the flags kept as every one, the forced flag unreported; the
/// chapters' ids unjudged, chapter 1's unreported.
#[test]
fn l89_a_stream_round_trips_through_nut() {
  let Some(corpus) = Corpus::new() else { return };
  let source = h264_aac(&corpus, "h264-aac.mp4");
  let target = output(&corpus, "l89.nut");
  let mut table = whole_table(&source);
  table[0].set_language(Some("eng".into()));
  table[0].set_disposition(TrackDisposition::DEFAULT | TrackDisposition::FORCED);
  table[1].set_language(Some("fra".into()));
  table[1].set_disposition(TrackDisposition::COMMENT);
  let chapters = [
    chapter_of(1, 0, 500, Some("one")),
    chapter_of(5, 500, 1000, Some("two")),
  ];
  let (mut muxer, report) = FfmpegOwnedMuxer::open(
    File::create(&target).expect("create"),
    ContainerName::new("nut"),
    table,
    &chapters,
    PacketLimits::default(),
  )
  .expect("open");
  assert_eq!(
    lines(&report, |line| matches!(
      line,
      Uncarried::Disposition { .. } | Uncarried::Language { .. }
    )),
    vec![Uncarried::Disposition {
      track: TrackIndex::new(0),
      flags: TrackDisposition::FORCED,
    }]
  );
  assert_eq!(
    lines(&report, |line| matches!(
      line,
      Uncarried::Chapter { .. } | Uncarried::ChapterSpan { .. } | Uncarried::ChapterId { .. }
    )),
    vec![Uncarried::ChapterId { chapter: 1, id: 5 }]
  );
  let durations = |track| Uncarried::Rewritten {
    track: TrackIndex::new(track),
    rewrite: Rewrite::Durations,
  };
  assert_eq!(field_rows(&report), vec![durations(0), durations(1)]);
  let mut demuxer = FfmpegOwnedDemuxer::open(&source).expect("open");
  while let Some(packet) = demuxer.next_packet().expect("a packet or the end") {
    match packet {
      DemuxedPacket::Video(p) => muxer
        .write_video(VideoTrackPacket::new(p.track(), p.packet().clone()))
        .map_err(|error| error.map_packet(drop)),
      DemuxedPacket::Audio(p) => muxer
        .write_audio(AudioTrackPacket::new(p.track(), p.packet().clone()))
        .map_err(|error| error.map_packet(drop)),
      other => panic!("{other:?}"),
    }
    .expect("a packet");
  }
  assert!(
    muxer.report().uncarried().contains(&Uncarried::Rewritten {
      track: TrackIndex::new(0),
      rewrite: Rewrite::DecodeTimes,
    }),
    "the reordering video's decode times"
  );
  muxer.finish().expect("finished");

  let offered = packets(&source);
  let back = packets(&target);
  // The first decode time, before zero: libavformat moves every stream
  // later by it (`mux.c` 636–717).
  let lift = offered
    .iter()
    .filter_map(|track| in_ms(track.first().and_then(|first| first.dts)))
    .min()
    .map_or(0, |dts| (-dts).max(0));
  assert!(lift > 0, "the premise: decoded before zero");
  assert_eq!(back.len(), 2);
  for track in 0..2 {
    assert_eq!(back[track].len(), offered[track].len(), "track {track}");
    for (index, (seen, sent)) in back[track].iter().zip(&offered[track]).enumerate() {
      assert!(seen.bytes == sent.bytes, "track {track}/{index}: the bytes");
      assert_eq!(seen.key, sent.key, "track {track}/{index}: the key flag");
      assert_eq!(
        in_ms(seen.pts),
        in_ms(sent.pts).map(|pts| pts + lift),
        "track {track}/{index}: presented"
      );
    }
  }
  let read = FfmpegOwnedDemuxer::open(&target).expect("open");
  let rows: Vec<_> = read
    .tracks()
    .iter()
    .map(|row| {
      (
        row.language().map(|tag| tag.as_str().to_owned()),
        row.disposition(),
      )
    })
    .collect();
  assert_eq!(
    rows,
    [
      (Some("eng".to_owned()), TrackDisposition::DEFAULT),
      (Some("fra".to_owned()), TrackDisposition::COMMENT),
    ]
  );
  assert_eq!(
    table_of(&source, TrackKind::Video)[0].timebase(),
    Timebase::new(1, NonZeroI32::new(12_800).expect("non-zero")),
    "the premise"
  );
  assert_eq!(
    read.tracks()[0].timebase(),
    Timebase::new(1, NonZeroI32::new(51_200).expect("non-zero"))
  );
  let chapters: Vec<_> = read
    .chapters()
    .iter()
    .map(|chapter| {
      (
        chapter.id(),
        chapter.title().map(|title| title.as_str().to_owned()),
        span_ms(chapter),
      )
    })
    .collect();
  assert_eq!(
    chapters,
    [
      (1, Some("one".to_owned()), (0, 500)),
      (2, Some("two".to_owned()), (500, 1000)),
    ]
  );
}

/// An audio table of `path`'s first audio track, its stream stated at
/// `rate` samples a second.
fn audio_rated(path: &Path, rate: i32) -> Vec<TrackDescription<Ffmpeg, FfmpegBytes, CodecTicket>> {
  let mut table = table_of(path, TrackKind::Audio);
  let index = table[0].side_data().stream_index();
  let mut parameters = table[0].side_data().rebuild().expect("the parameters");
  // SAFETY: `parameters` owns its `AVCodecParameters`, live for the
  // write; `sample_rate` is a plain integer field.
  unsafe {
    (*parameters.as_mut_ptr()).sample_rate = rate;
  }
  *table[0].side_data_mut() =
    CodecTicket::mirror(&parameters, index, usize::MAX).expect("the ticket");
  table
}

/// **L90 — NUT's refusals** (table H; `nutenc.c` 329–333, 466–472,
/// 615–667, 963–1184): a packet of a reordering stream with no
/// presentation time, which NUT refuses after taking it
/// ([`FfmpegMuxRefusal::MissingPts`]); a key frame presented before the
/// stream's last key frame, or at it but decoded at another time, which
/// NUT's index asserts against at its trailer
/// ([`FfmpegMuxRefusal::KeyFrameOrder`]) — two subtitle cues at one time
/// taken; after libavformat's first write, a frame decoded before zero
/// and presented after it, whose decode time a syncpoint would count
/// unsigned ([`FfmpegMuxRefusal::BeforeZero`]); with three time bases, a
/// presentation time past what NUT's 64-bit count of them holds — which
/// bounds the decode time no later than it — refused
/// ([`FfmpegMuxRefusal::TimeWraps`]), and a chapter starting past it
/// ([`MuxOpenError::ChapterSpan`]), one a tick earlier taken. A
/// subtitle's negative duration, which NUT does not write, is taken. At
/// open: raw video of a tag NUT's tables do not hold, which libavformat
/// would write under the tables' first, RGB 5:5:5's
/// ([`MuxOpenError::ConfigurationInvalid`]) — I420 raw video, which they
/// hold, read back as itself; a video stream of no width and an audio one
/// of no sample rate, whose stream headers NUT's reader refuses
/// ([`MuxOpenError::Dimensions`], [`MuxOpenError::ConfigurationInvalid`]).
/// A time base not in lowest terms, which NUT's reader refuses, is handed
/// reduced: the file reads back.
///
/// PLANT: each check removed takes its packet, or opens; the time bases
/// handed as they are, the file unreadable.
#[test]
fn l90_nut_refuses_what_its_frames_and_index_cannot_hold() {
  let Some(corpus) = Corpus::new() else { return };
  let nut = || ContainerName::new("nut");
  let source = h264_aac(&corpus, "h264-aac.mkv");
  let track = TrackIndex::new(0);
  let (table, frames) = first_track(&source, TrackKind::Video, 1);
  let frame = video_of(&frames[0]);
  let at = |pts: Option<i64>, dts: i64, key: bool| {
    VideoTrackPacket::new(
      track,
      frame
        .clone()
        .with_pts(pts.map(ms))
        .with_dts(Some(ms(dts)))
        .with_flags(if key {
          PacketFlags::KEY
        } else {
          PacketFlags::empty()
        }),
    )
  };
  let mut muxer = muxer_into(&output(&corpus, "l90-keys.nut"), nut(), table);
  let offered = at(None, 0, true);
  let (refusal, back) = by_container(muxer.write_video(offered.clone()));
  assert_eq!(refusal, FfmpegMuxRefusal::MissingPts { track });
  untouched!(offered, back);
  for (pts, dts, key) in [(200, 0, true), (220, 40, false)] {
    muxer
      .write_video(at(Some(pts), dts, key))
      .map_err(|error| error.map_packet(drop))
      .expect("in order");
  }
  for (pts, dts) in [(100, 80), (200, 80)] {
    let offered = at(Some(pts), dts, true);
    let (refusal, back) = by_container(muxer.write_video(offered.clone()));
    assert_eq!(
      refusal,
      FfmpegMuxRefusal::KeyFrameOrder {
        track,
        pts: ms(pts),
      },
      "presented at {pts}, decoded at {dts}"
    );
    untouched!(offered, back);
  }
  muxer
    .write_video(at(Some(240), 80, true))
    .map_err(|error| error.map_packet(drop))
    .expect("after the last key frame");
  muxer.finish().expect("finished");

  // Eleven seconds of audio, which libavformat's interleaving writes ahead
  // of the video: its first write settles no move.
  let (_, audio) = first_track(&source, TrackKind::Audio, 1);
  let sound = audio_of(&audio[0]);
  let mut muxer = muxer_into(
    &output(&corpus, "l90-late.nut"),
    nut(),
    whole_table(&source),
  );
  for index in 0..516 {
    muxer
      .write_audio(AudioTrackPacket::new(
        TrackIndex::new(1),
        sound
          .clone()
          .with_pts(Some(ms(21 * index)))
          .with_dts(Some(ms(21 * index))),
      ))
      .map_err(|error| error.map_packet(drop))
      .expect("audio");
  }
  let early = at(Some(40), -80, true);
  let (refusal, back) = by_container(muxer.write_video(early.clone()));
  assert_eq!(
    refusal,
    FfmpegMuxRefusal::BeforeZero {
      track,
      timestamp: ms(-80),
    }
  );
  untouched!(early, back);
  muxer.finish().expect("finished");

  // Three time bases — the video's 1/64 000, the audio's 1/44 100, the
  // chapter's 1/1000: a count is three times the time and its base's
  // place, within `u64::MAX`.
  let limit = (u64::MAX - 2) / 3;
  let ok_ms = i64::try_from(limit / 64).expect("an i64");
  let far = [chapter_of(1, 0, 1000, Some("one"))];
  let (mut muxer, _) = FfmpegOwnedMuxer::open(
    File::create(output(&corpus, "l90-far.nut")).expect("create"),
    nut(),
    whole_table(&source),
    &far,
    PacketLimits::default(),
  )
  .expect("open");
  muxer
    .write_audio(AudioTrackPacket::new(
      TrackIndex::new(1),
      sound.clone().with_pts(Some(ms(0))).with_dts(Some(ms(0))),
    ))
    .map_err(|error| error.map_packet(drop))
    .expect("audio");
  muxer
    .write_video(at(Some(0), 0, true))
    .map_err(|error| error.map_packet(drop))
    .expect("the first frame");
  let offered = at(Some(ok_ms + 1), ok_ms - 1, true);
  let (refusal, back) = by_container(muxer.write_video(offered.clone()));
  assert_eq!(
    refusal,
    FfmpegMuxRefusal::TimeWraps {
      track,
      timestamp: ms(ok_ms + 1),
    }
  );
  untouched!(offered, back);
  muxer
    .write_video(at(Some(ok_ms), ok_ms, true))
    .map_err(|error| error.map_packet(drop))
    .expect("within the count");
  muxer.finish().expect("finished");
  let start = i64::try_from(limit).expect("an i64");
  for (start, refused) in [(start + 1, true), (start, false)] {
    let opened = FfmpegOwnedMuxer::open(
      File::create(output(&corpus, "l90-chapter.nut")).expect("create"),
      nut(),
      whole_table(&source),
      &[chapter_of(1, start, start + 1000, Some("far"))],
      PacketLimits::default(),
    );
    match opened {
      Err(MuxOpenError::ChapterSpan { chapter: 0 }) if refused => {}
      Ok((muxer, _)) if !refused => muxer.finish().expect("finished"),
      other => panic!("a chapter at {start}: {:?}", other.map(drop)),
    }
  }

  // A subtitle's negative duration is not written; two cues at one time
  // are taken, the second key frame at the first's times.
  let srt = written_text(
    &corpus,
    "two.srt",
    "1\n00:00:00,000 --> 00:00:01,000\nhello\n\n2\n00:00:01,000 --> 00:00:02,000\nworld\n\n",
  );
  let ass = generated(
    &corpus,
    "ass.mkv",
    &["-i", srt.to_str().expect("utf-8 path"), "-c:s", "ass"],
  );
  let (table, cues) = first_track(&ass, TrackKind::Subtitle, 1);
  let (mut muxer, report) = FfmpegOwnedMuxer::open(
    File::create(output(&corpus, "l90-cues.nut")).expect("create"),
    nut(),
    table,
    &[],
    PacketLimits::default(),
  )
  .expect("open");
  assert_eq!(
    field_rows(&report),
    vec![Uncarried::Rewritten {
      track,
      rewrite: Rewrite::Durations,
    }]
  );
  let cue = subtitle_of(&cues[0]);
  for (pts, duration) in [(0, -1000), (1000, 500), (1000, 500)] {
    muxer
      .write_subtitle(subtitle_at(&cue, pts, duration))
      .map_err(|error| error.map_packet(drop))
      .unwrap_or_else(|error| panic!("the cue at {pts}: {error:?}"));
  }
  muxer.finish().expect("finished");

  let open = |name: &str, table| {
    let target = output(&corpus, name);
    let result = FfmpegOwnedMuxer::open(
      File::create(&target).expect("create"),
      nut(),
      table,
      &[],
      PacketLimits::default(),
    );
    match result {
      Ok((muxer, _)) => {
        muxer.finish().expect("finished");
        Ok(target)
      }
      Err(error) => {
        assert_eq!(std::fs::metadata(&target).expect("the file").len(), 0);
        Err(error)
      }
    }
  };
  let rgb = generated(
    &corpus,
    "rgb24.mov",
    &[
      "-f",
      "lavfi",
      "-i",
      "testsrc2=size=32x32:rate=25:duration=0.2",
      "-c:v",
      "rawvideo",
      "-pix_fmt",
      "rgb24",
    ],
  );
  assert!(matches!(
    open("l90-rgb.nut", table_of(&rgb, TrackKind::Video)),
    Err(MuxOpenError::ConfigurationInvalid { track, .. }) if track.get() == 0
  ));
  let i420 = generated(
    &corpus,
    "i420.y4m",
    &[
      "-f",
      "lavfi",
      "-i",
      "testsrc2=size=32x32:rate=25:duration=0.2",
      "-c:v",
      "rawvideo",
      "-pix_fmt",
      "yuv420p",
    ],
  );
  let (table, frames) = first_track(&i420, TrackKind::Video, 1);
  let target = output(&corpus, "l90-i420.nut");
  let mut muxer = muxer_into(&target, nut(), table);
  muxer
    .write_video(video_at(&video_of(&frames[0]), Some(0), 0))
    .map_err(|error| error.map_packet(drop))
    .expect("a frame");
  muxer.finish().expect("finished");
  assert_eq!(
    table_of(&target, TrackKind::Video)[0].side_data().format(),
    ffmpeg_next::ffi::AVPixelFormat::AV_PIX_FMT_YUV420P as i32,
    "read back as itself"
  );
  assert!(matches!(
    open("l90-wide.nut", video_sized(&source, 0, 48)),
    Err(MuxOpenError::Dimensions { track }) if track.get() == 0
  ));
  assert!(matches!(
    open("l90-rate.nut", audio_rated(&source, 0)),
    Err(MuxOpenError::ConfigurationInvalid { track, .. }) if track.get() == 0
  ));

  // A video time base of 2/180 000, and a chapter's of 2/2000.
  let unreduced = |num: i32, den: i32| Timebase::new(num, NonZeroI32::new(den).expect("non-zero"));
  let (description, frames) = first_track(&source, TrackKind::Video, 1);
  let (_, params, extradata, side, _, _, _) = description
    .into_iter()
    .next()
    .expect("a row")
    .into_parts_with_disposition();
  let table = vec![
    TrackDescription::new(unreduced(2, 180_000), params, side)
      .expect("a timed track")
      .with_extradata(extradata),
  ];
  let base = unreduced(2, 2000);
  let chapter = Chapter::<Ffmpeg>::new(1, base, Timestamp::new(0, base), Timestamp::new(250, base))
    .with_title(Some("half".into()));
  let target = output(&corpus, "l90-unreduced.nut");
  let (mut muxer, _) = FfmpegOwnedMuxer::open(
    File::create(&target).expect("create"),
    nut(),
    table,
    &[chapter],
    PacketLimits::default(),
  )
  .expect("open");
  muxer
    .write_video(video_at(&video_of(&frames[0]), Some(0), 0))
    .map_err(|error| error.map_packet(drop))
    .expect("a frame");
  muxer.finish().expect("finished");
  let read = FfmpegOwnedDemuxer::open(&target).expect("NUT's reader takes the file");
  let titles: Vec<_> = read
    .chapters()
    .iter()
    .map(|chapter| {
      (
        chapter.title().map(|title| title.as_str().to_owned()),
        span_ms(chapter),
      )
    })
    .collect();
  assert_eq!(titles, [(Some("half".to_owned()), (0, 250))]);
}

/// `row` as `codec`, its configuration `extradata`: its ticket's codec id
/// and its parameters' rewritten, everything else kept.
fn recoded(
  row: TrackDescription<Ffmpeg, FfmpegBytes, CodecTicket>,
  codec: ffmpeg_next::ffi::AVCodecID,
  extradata: &[u8],
) -> TrackDescription<Ffmpeg, FfmpegBytes, CodecTicket> {
  use mediaencode::mediaspine::demuxer::{AudioTrackParams, SubtitleTrackParams, VideoTrackParams};
  let index = row.side_data().stream_index();
  let mut parameters = row.side_data().rebuild().expect("the parameters");
  // SAFETY: `parameters` owns its `AVCodecParameters`, live for the
  // write; `codec_id` is a plain field, written a value of its own enum.
  unsafe {
    (*parameters.as_mut_ptr()).codec_id = codec;
  }
  let ticket = CodecTicket::mirror(&parameters, index, usize::MAX).expect("the ticket");
  let id = CodecId::from_raw(codec as i32);
  let (timebase, params, _, _, language, disposition, _) = row.into_parts_with_disposition();
  let params = match params {
    TrackParams::Video(params) => TrackParams::Video(VideoTrackParams::new(
      id,
      params.width(),
      params.height(),
      params.pixel_format().clone(),
      params.frame_rate(),
    )),
    TrackParams::Subtitle(_) => TrackParams::Subtitle(SubtitleTrackParams::new(id)),
    TrackParams::Audio(params) => TrackParams::Audio(AudioTrackParams::new(
      id,
      params.sample_rate(),
      params.channel_count(),
      params.sample_format(),
      params.channel_layout().clone(),
    )),
    other => other,
  };
  TrackDescription::new(timebase, params, ticket)
    .expect("a timed track")
    .with_extradata(Some(bytes_of(extradata)))
    .with_language(language)
    .with_disposition(disposition)
}

/// Every packet of `path` written into a muxer of `container` over `table`
/// at `target`, each as it is; the report.
fn remuxed(
  path: &Path,
  target: &Path,
  container: &str,
  table: Vec<TrackDescription<Ffmpeg, FfmpegBytes, CodecTicket>>,
  chapters: &[Chapter<Ffmpeg>],
) -> mediaencode_ffmpeg::MuxReport {
  let (mut muxer, _) = FfmpegOwnedMuxer::open(
    File::create(target).expect("create"),
    ContainerName::new(container),
    table,
    chapters,
    PacketLimits::default(),
  )
  .expect("open");
  let mut demuxer = FfmpegOwnedDemuxer::open(path).expect("open");
  while let Some(packet) = demuxer.next_packet().expect("a packet or the end") {
    match packet {
      DemuxedPacket::Video(p) => muxer
        .write_video(VideoTrackPacket::new(p.track(), p.packet().clone()))
        .map_err(|error| error.map_packet(drop)),
      DemuxedPacket::Audio(p) => muxer
        .write_audio(AudioTrackPacket::new(p.track(), p.packet().clone()))
        .map_err(|error| error.map_packet(drop)),
      other => panic!("{other:?}"),
    }
    .expect("a packet");
  }
  let report = muxer.report().clone();
  muxer.finish().expect("finished");
  report
}

/// Every packet of `target` reads back as `source`'s: its bytes, its key
/// flag, and — on a track `timed` names — its presentation time within a
/// millisecond of it, a reader counting it from the samples its source's
/// milliseconds rounded.
fn read_back_as(source: &Path, target: &Path, timed: &[usize]) {
  read_back_lifted(source, target, timed, 0);
}

/// [`read_back_as`], every time read back `lift` milliseconds later.
fn read_back_lifted(source: &Path, target: &Path, timed: &[usize], lift: i64) {
  let offered = packets(source);
  let back = packets(target);
  assert_eq!(back.len(), offered.len());
  for (track, (seen, sent)) in back.iter().zip(&offered).enumerate() {
    assert_eq!(seen.len(), sent.len(), "track {track}: a packet a packet");
    for (index, (seen, sent)) in seen.iter().zip(sent).enumerate() {
      assert!(seen.bytes == sent.bytes, "track {track}/{index}: the bytes");
      assert_eq!(seen.key, sent.key, "track {track}/{index}: the key flag");
      let (seen_ms, sent_ms) = (in_ms(seen.pts), in_ms(sent.pts));
      assert!(
        !timed.contains(&track)
          || matches!((seen_ms, sent_ms), (Some(a), Some(b)) if (a - b - lift).abs() <= 1),
        "track {track}/{index}: presented at {seen_ms:?}, offered at {sent_ms:?}"
      );
    }
  }
}

/// **L91 — VP8 and Vorbis, Opus, and FLAC round-trip through Ogg, and the
/// report says what Ogg keeps** (table H; `oggenc.c` 497–765): every
/// packet reads back with its bytes and its key flag, and each contiguous
/// stream with its times — Ogg writes no time a packet keeps of its own, a
/// page the granule of its last, a reader counting the others' times from
/// the samples or frames ([`Rewrite::Timeline`], at open), and VP8's key
/// frames by the bitstream ([`Rewrite::SyncFromBitstream`]); a stream's
/// language is kept. Chapters Ogg writes into an Opus stream's tags alone,
/// numbered from 0 — a start's whole seconds rounded to the nearest and
/// its milliseconds rounded apart, so that one at 1.6 s is written at 2.6
/// s, and the chapter before it ends there ([`Uncarried::ChapterSpan`]) —
/// and without an Opus stream none ([`Uncarried::Chapter`]). A lone Opus
/// stream's later strings metadata Ogg writes as a chained stream's tags,
/// not reported; the first packet's start skip, its pre-skip, its
/// OpusHead carries; the file reads back whole.
///
/// PLANT: the written times not reported; the chapters' rule off, or the
/// next chapter's start taken as declared, chapter 1's end unreported; the
/// metadata update reported dropped.
#[test]
fn l91_streams_round_trip_through_ogg() {
  use ffmpeg_next::ffi::AVPacketSideDataType as T;
  let Some(corpus) = Corpus::new() else { return };
  let rewritten = |track: usize, rewrite| Uncarried::Rewritten {
    track: TrackIndex::new(track),
    rewrite,
  };
  let chapter_lines = |report: &mediaencode_ffmpeg::MuxReport| {
    lines(report, |line| {
      matches!(
        line,
        Uncarried::Chapter { .. } | Uncarried::ChapterSpan { .. } | Uncarried::ChapterId { .. }
      )
    })
  };

  let source = picture_and_sound(
    &corpus,
    "vp8-vorbis.webm",
    &["-c:v", "libvpx"],
    &["-c:a", "vorbis", "-strict", "-2", "-ac", "2"],
  );
  let mut table = whole_table(&source);
  table[0].set_language(Some("eng".into()));
  table[1].set_language(Some("fra".into()));
  let target = output(&corpus, "l91.ogg");
  let report = remuxed(
    &source,
    &target,
    "ogg",
    table,
    &[chapter_of(0, 0, 500, Some("one"))],
  );
  assert_eq!(
    field_rows(&report),
    vec![
      rewritten(0, Rewrite::Timeline),
      rewritten(0, Rewrite::SyncFromBitstream),
      rewritten(1, Rewrite::Timeline),
    ]
  );
  assert_eq!(
    lines(&report, |line| matches!(line, Uncarried::Language { .. })),
    vec![]
  );
  assert_eq!(
    chapter_lines(&report),
    vec![Uncarried::Chapter { chapter: 0 }]
  );
  // VP8's granule counts a frame's end (`oggenc.c` 739–748): the first
  // frame, alone on its page as a key frame, Ogg's reader presents at its
  // end less a tick (`vp8_gptopts`, `oggparsevp8.c`), each after it as it
  // was offered — times the report's line says are not kept as given.
  read_back_as(&source, &target, &[1]);
  let late: Vec<_> = packets(&target)[0]
    .iter()
    .zip(&packets(&source)[0])
    .map(|(seen, sent)| in_ms(seen.pts).zip(in_ms(sent.pts)).map(|(a, b)| a - b))
    .collect();
  assert_eq!(late[0], Some(39), "the first frame, at its end less a tick");
  assert!(late[1..].iter().all(|late| *late == Some(0)), "{late:?}");
  let read = FfmpegOwnedDemuxer::open(&target).expect("open");
  let languages: Vec<_> = read
    .tracks()
    .iter()
    .map(|row| row.language().map(|tag| tag.as_str().to_owned()))
    .collect();
  assert_eq!(languages, [Some("eng".to_owned()), Some("fra".to_owned())]);
  assert!(read.chapters().is_empty());

  let opus = opus_mkv(&corpus);
  let chapters = [
    chapter_of(0, 0, 1400, Some("one")),
    chapter_of(1, 1400, 1600, Some("two")),
    chapter_of(2, 1600, 2000, Some("three")),
  ];
  let target = output(&corpus, "l91.opus");
  let (mut muxer, report) = FfmpegOwnedMuxer::open(
    File::create(&target).expect("create"),
    ContainerName::new("opus"),
    table_of(&opus, TrackKind::Audio),
    &chapters,
    PacketLimits::default(),
  )
  .expect("open");
  assert_eq!(
    chapter_lines(&report),
    vec![
      Uncarried::ChapterSpan { chapter: 1 },
      Uncarried::ChapterSpan { chapter: 2 },
    ]
  );
  let (_, frames) = whole_track(&opus, TrackKind::Audio);
  let skips = |packet: &DemuxedPacket<Ffmpeg, FfmpegBytes>| {
    audio_of(packet)
      .extra()
      .side_data()
      .iter()
      .any(|entry| entry.kind() == T::AV_PKT_DATA_SKIP_SAMPLES as i32)
  };
  assert!(
    skips(&frames[0]),
    "the premise: the first packet's pre-skip"
  );
  let skipped = |report: &mediaencode_ffmpeg::MuxReport| {
    lines(report, |line| {
      matches!(
        line,
        Uncarried::SideData { kind, .. } if *kind == T::AV_PKT_DATA_SKIP_SAMPLES as i32
      )
    })
  };
  for (index, frame) in frames.iter().enumerate() {
    if index == 1 {
      assert_eq!(
        skipped(muxer.report()),
        vec![],
        "the first packet's start skip, Opus's pre-skip, kept"
      );
    }
    let mut packet = audio_of(frame);
    if index == 3 {
      packet.extra_mut().set_side_data(vec![side(
        T::AV_PKT_DATA_STRINGS_METADATA,
        b"title\0later\0",
      )]);
    }
    muxer
      .write_audio(AudioTrackPacket::new(TrackIndex::new(0), packet))
      .map_err(|error| error.map_packet(drop))
      .expect("a frame");
  }
  assert!(
    lines(muxer.report(), |line| matches!(
      line,
      Uncarried::SideData { kind, .. } if *kind == T::AV_PKT_DATA_STRINGS_METADATA as i32
    ))
    .is_empty(),
    "written as a chained stream's tags"
  );
  muxer.finish().expect("finished");
  read_back_as(&opus, &target, &[0]);
  let read = FfmpegOwnedDemuxer::open(&target).expect("open");
  let read_chapters: Vec<_> = read
    .chapters()
    .iter()
    .map(|chapter| {
      (
        chapter.id(),
        chapter.title().map(|title| title.as_str().to_owned()),
        span_ms(chapter).0,
      )
    })
    .collect();
  assert_eq!(
    read_chapters,
    [
      (0, Some("one".to_owned()), 0),
      (1, Some("two".to_owned()), 1400),
      (2, Some("three".to_owned()), 2600),
    ]
  );

  let flac = audio_file(&corpus, "flac.mkv", "flac");
  let target = output(&corpus, "l91.oga");
  let report = remuxed(
    &flac,
    &target,
    "oga",
    table_of(&flac, TrackKind::Audio),
    &[],
  );
  assert_eq!(field_rows(&report), vec![rewritten(0, Rewrite::Timeline)]);
  read_back_as(&flac, &target, &[0]);
}

/// **L92 — Ogg's refusals** (table H; `oggenc.c` 319–765): at open, a
/// codec Ogg's start does not take ([`MuxOpenError::CodecNotCarried`]);
/// Opus with no configuration ([`MuxOpenError::ConfigurationMissing`]); a
/// configuration Ogg's start refuses — Opus's shorter than its 19-byte
/// OpusHead, FLAC's than its STREAMINFO, Speex's than its 80-byte header,
/// Vorbis's that does not split into three headers, Theora's whose first
/// is shorter than its 42 bytes — and an audio stream of no sample rate
/// ([`MuxOpenError::ConfigurationInvalid`]), each a byte longer opening;
/// VP8 of a width past 16 bits or an aspect ratio's term past 24
/// ([`MuxOpenError::Dimensions`]). In packets: an empty packet, which its
/// reader drops, moving the next ([`PayloadRule::Empty`]); a VP8 frame
/// ending at 2^31 ticks or before zero — of no duration, at the frame rate
/// libavformat guesses it — and a Theora frame ending at 2^57, past their
/// granules' signed shifts ([`FfmpegMuxRefusal::TimeWraps`]), a tick
/// earlier taken; an Opus packet whose granule, its pre-skip added, is
/// −1 ([`FfmpegMuxRefusal::UnsetGranule`]); a lone Opus stream's later
/// metadata update that does not unpack ([`PayloadRule::MetadataUpdate`])
/// — on its first packet ignored, and taken.
///
/// PLANT: each check removed takes its packet, or opens.
#[test]
fn l92_ogg_refuses_what_its_start_and_granules_cannot_hold() {
  use ffmpeg_next::ffi::{AVCodecID as Id, AVPacketSideDataType as T};
  let Some(corpus) = Corpus::new() else { return };
  let open = |name: &str, container: &str, table| {
    let target = output(&corpus, name);
    let result = FfmpegOwnedMuxer::open(
      File::create(&target).expect("create"),
      ContainerName::new(container),
      table,
      &[],
      PacketLimits::default(),
    );
    match result {
      Ok((muxer, _)) => {
        muxer.finish().expect("finished");
        Ok(())
      }
      Err(error) => {
        assert_eq!(std::fs::metadata(&target).expect("the file").len(), 0);
        Err(error)
      }
    }
  };
  let opus = opus_mkv(&corpus);
  let vp8 = picture_and_sound(
    &corpus,
    "vp8-vorbis.webm",
    &["-c:v", "libvpx"],
    &["-c:a", "vorbis", "-strict", "-2", "-ac", "2"],
  );
  let opus_row = || table_of(&opus, TrackKind::Audio).remove(0);
  let video_row = || table_of(&vp8, TrackKind::Video).remove(0);

  assert!(matches!(
    open(
      "l92-aac.ogg",
      "ogg",
      table_of(&corpus.multi_track_mkv(), TrackKind::Audio)
    ),
    Err(MuxOpenError::CodecNotCarried { .. })
  ));
  assert!(matches!(
    open(
      "l92-bare.opus",
      "opus",
      vec![opus_row().with_extradata(Some(bytes_of(&[])))]
    ),
    Err(MuxOpenError::ConfigurationMissing { .. })
  ));
  let head = opus_row().side_data().extradata_ref().as_ref().to_vec();
  assert!(head.len() >= 19, "the premise");
  assert!(matches!(
    open(
      "l92-opus.opus",
      "opus",
      vec![opus_row().with_extradata(Some(bytes_of(&head[..18])))]
    ),
    Err(MuxOpenError::ConfigurationInvalid { .. })
  ));
  open(
    "l92-opus.opus",
    "opus",
    vec![opus_row().with_extradata(Some(bytes_of(&head[..19])))],
  )
  .expect("an OpusHead of its 19 bytes");
  for (name, codec, short, long) in [
    (
      "l92-flac.oga",
      Id::AV_CODEC_ID_FLAC,
      vec![0; 33],
      vec![0; 34],
    ),
    (
      "l92-speex.spx",
      Id::AV_CODEC_ID_SPEEX,
      vec![0; 79],
      vec![0; 80],
    ),
  ] {
    assert!(
      matches!(
        open(name, "ogg", vec![recoded(opus_row(), codec, &short)]),
        Err(MuxOpenError::ConfigurationInvalid { .. })
      ),
      "{name}"
    );
    open(name, "ogg", vec![recoded(opus_row(), codec, &long)])
      .unwrap_or_else(|error| panic!("{name}: {error:?}"));
  }
  assert!(matches!(
    open(
      "l92-vorbis.ogg",
      "ogg",
      vec![recoded(opus_row(), Id::AV_CODEC_ID_VORBIS, &[3, 0, 0])]
    ),
    Err(MuxOpenError::ConfigurationInvalid { .. })
  ));
  // Theora's headers, laced: an identification header of `first` bytes,
  // a comment header of one, a setup header of one.
  let theora = |first: usize| {
    let mut extradata = vec![2, u8::try_from(first).expect("a byte"), 1];
    let mut identification = vec![0; first];
    if first >= 42 {
      identification[..7].copy_from_slice(b"\x80theora");
      identification[7..10].copy_from_slice(&[3, 2, 1]);
      identification[22..26].copy_from_slice(&25_u32.to_be_bytes());
      identification[26..30].copy_from_slice(&1_u32.to_be_bytes());
      identification[41] = 6 << 5;
    }
    extradata.extend(identification);
    extradata.extend([0x81, 0x82]);
    extradata
  };
  assert!(matches!(
    open(
      "l92-theora.ogg",
      "ogg",
      vec![recoded(video_row(), Id::AV_CODEC_ID_THEORA, &theora(41))]
    ),
    Err(MuxOpenError::ConfigurationInvalid { .. })
  ));
  assert!(matches!(
    open("l92-rate.opus", "opus", audio_rated(&opus, 0)),
    Err(MuxOpenError::ConfigurationInvalid { .. })
  ));
  assert!(matches!(
    open("l92-wide.ogg", "ogg", video_sized(&vp8, 65_536, 48)),
    Err(MuxOpenError::Dimensions { .. })
  ));
  open("l92-wide.ogg", "ogg", video_sized(&vp8, 65_535, 48)).expect("16 bits");

  // An empty packet, and VP8's granule: the end in its upper 32 bits.
  let (table, frames) = first_track(&vp8, TrackKind::Video, 1);
  let frame = video_of(&frames[0]);
  let track = TrackIndex::new(0);
  let mut muxer = muxer_into(
    &output(&corpus, "l92-vp8.ogg"),
    ContainerName::new("ogg"),
    table,
  );
  let empty = VideoTrackPacket::new(track, video_holding(&frame, &[]));
  let (refusal, back) = by_container(muxer.write_video(empty.clone()));
  assert_eq!(refusal, payload(PayloadRule::Empty));
  untouched!(empty, back);
  let lasting = |pts: i64, duration: i64| {
    VideoTrackPacket::new(
      track,
      frame
        .clone()
        .with_pts(Some(ms(pts)))
        .with_dts(Some(ms(pts)))
        .with_duration(Some(ms(duration))),
    )
  };
  let wrap = 1_i64 << 31;
  for (offered, pts) in [
    (lasting(-40, 39), -40),
    (lasting(wrap - 40, 40), wrap - 40),
    (lasting(wrap - 40, 0), wrap - 40),
  ] {
    let (refusal, back) = by_container(muxer.write_video(offered.clone()));
    assert_eq!(
      refusal,
      FfmpegMuxRefusal::TimeWraps {
        track,
        timestamp: ms(pts),
      },
      "presented at {pts}"
    );
    untouched!(offered, back);
  }
  muxer
    .write_video(lasting(wrap - 41, 0))
    .map_err(|error| error.map_packet(drop))
    .expect("ending a tick before 2^31, at the guessed frame");
  muxer.finish().expect("finished");

  // Theora's: the end shifted by the header's six bits.
  let mut muxer = muxer_into(
    &output(&corpus, "l92-theora.ogg"),
    ContainerName::new("ogg"),
    vec![recoded(video_row(), Id::AV_CODEC_ID_THEORA, &theora(42))],
  );
  let frame_rate = Timebase::new(1, NonZeroI32::new(25).expect("non-zero"));
  let theora_frame = |pts: i64| {
    VideoTrackPacket::new(
      track,
      video_holding(&frame, &[0x40, 1, 2])
        .with_pts(Some(Timestamp::new(pts, frame_rate)))
        .with_dts(Some(Timestamp::new(pts, frame_rate)))
        .with_duration(Some(Timestamp::new(1, frame_rate)))
        .with_flags(PacketFlags::KEY),
    )
  };
  let past = theora_frame((1 << 57) - 1);
  let (refusal, back) = by_container(muxer.write_video(past.clone()));
  assert_eq!(
    refusal,
    FfmpegMuxRefusal::TimeWraps {
      track,
      timestamp: Timestamp::new((1 << 57) - 1, frame_rate),
    }
  );
  untouched!(past, back);
  muxer
    .write_video(theora_frame((1 << 57) - 2))
    .map_err(|error| error.map_packet(drop))
    .expect("ending a frame before 2^57");
  muxer.finish().expect("finished");

  // Opus's granule, its pre-skip added; a lone stream's metadata update.
  let (table, frames) = first_track(&opus, TrackKind::Audio, 3);
  let skip = table[0].side_data().initial_padding();
  assert!(skip > 0, "the premise: a pre-skip");
  let at_48k = Timebase::new(1, NonZeroI32::new(48_000).expect("non-zero"));
  let mut muxer = muxer_into(
    &output(&corpus, "l92-granule.opus"),
    ContainerName::new("opus"),
    table,
  );
  let ending = |pts: i64, frame: &DemuxedPacket<Ffmpeg, FfmpegBytes>, metadata: Option<&[u8]>| {
    let mut packet = audio_of(frame)
      .with_pts(Some(Timestamp::new(pts, at_48k)))
      .with_dts(Some(Timestamp::new(pts, at_48k)))
      .with_duration(Some(Timestamp::new(960, at_48k)));
    if let Some(metadata) = metadata {
      packet
        .extra_mut()
        .set_side_data(vec![side(T::AV_PKT_DATA_STRINGS_METADATA, metadata)]);
    }
    AudioTrackPacket::new(track, packet)
  };
  let unset = ending(-961 - i64::from(skip), &frames[0], None);
  let (refusal, back) = by_container(muxer.write_audio(unset.clone()));
  assert_eq!(
    refusal,
    FfmpegMuxRefusal::UnsetGranule {
      track,
      end: Timestamp::new(-1, at_48k),
    }
  );
  untouched!(unset, back);
  muxer
    .write_audio(ending(-960 - i64::from(skip), &frames[0], Some(b"title")))
    .map_err(|error| error.map_packet(drop))
    .expect("ending at zero; the first packet's metadata ignored");
  let malformed = ending(-i64::from(skip), &frames[1], Some(b"title\0later"));
  let (refusal, back) = by_container(muxer.write_audio(malformed.clone()));
  assert_eq!(refusal, payload(PayloadRule::MetadataUpdate));
  untouched!(malformed, back);
  muxer
    .write_audio(ending(
      -i64::from(skip),
      &frames[1],
      Some(b"title\0later\0"),
    ))
    .map_err(|error| error.map_packet(drop))
    .expect("a metadata update that unpacks");
  muxer.finish().expect("finished");
}

/// **L93 — MPEG-2 video and MPEG audio round-trip through the MPEG
/// program stream's five muxers, and the report says what they keep**
/// (table H; `mpegenc.c` 640–1257): every packet reads back with its bytes
/// and its key flag, its reader's parser framing the stream again, and
/// with its presentation time moved by the preload, or with none where its
/// start shares a PES packet with an earlier one's — the first packet
/// decoded before half a second, every stream's times moved so that it
/// lies there ([`Uncarried::TimesShifted`]); the writer keeps the times of
/// the first packet each PES packet holds the start of alone, the reader
/// counting the others' ([`Rewrite::Timeline`]), and no key flag
/// ([`Rewrite::SyncFromBitstream`] for the video). DVD writes its
/// navigation packets beside them, which its reader takes for a data
/// stream of their own. A picture decoded from zero, which no time before
/// it moves, is moved by the preload alone, and reported so.
///
/// PLANT: the preload unreported.
#[test]
fn l93_streams_round_trip_through_mpeg_ps() {
  let Some(corpus) = Corpus::new() else { return };
  let source = picture_and_sound(
    &corpus,
    "mpeg2-mp2.mkv",
    &["-c:v", "mpeg2video", "-bf", "0"],
    &["-c:a", "mp2", "-ac", "2"],
  );
  let rewritten = |track: usize, rewrite| Uncarried::Rewritten {
    track: TrackIndex::new(track),
    rewrite,
  };
  for container in ["mpeg", "vcd", "vob", "svcd", "dvd"] {
    let target = output(&corpus, &format!("l93.{container}"));
    let report = remuxed(&source, &target, container, whole_table(&source), &[]);
    assert_eq!(
      field_rows(&report),
      vec![
        rewritten(0, Rewrite::Timeline),
        rewritten(0, Rewrite::SyncFromBitstream),
        rewritten(1, Rewrite::Timeline),
      ],
      "{container}"
    );
    assert!(
      report.uncarried().contains(&Uncarried::TimesShifted),
      "{container}: the preload"
    );
    let offered = packets(&source);
    let back = packets(&target);
    let first = offered
      .iter()
      .filter_map(|track| in_ms(track.first().and_then(|first| first.dts)))
      .min()
      .expect("a first decode time");
    let preload = 500 - first;
    // The video and the audio as their reader numbers them; DVD's
    // navigation packets a stream of their own.
    let tracks: Vec<_> = FfmpegOwnedDemuxer::open(&target)
      .expect("open")
      .tracks()
      .iter()
      .map(|row| row.kind())
      .collect();
    if container == "dvd" {
      assert!(tracks.contains(&TrackKind::Data), "{container}: {tracks:?}");
    }
    let of_kind = |kind: TrackKind| {
      tracks
        .iter()
        .position(|&seen| seen == kind)
        .unwrap_or_else(|| panic!("{container}: a {kind:?} track in {tracks:?}"))
    };
    for (track, kind) in [(0, TrackKind::Video), (1, TrackKind::Audio)] {
      let seen = &back[of_kind(kind)];
      assert_eq!(
        seen.len(),
        offered[track].len(),
        "{container}: track {track}"
      );
      for (index, (seen, sent)) in seen.iter().zip(&offered[track]).enumerate() {
        assert!(
          seen.bytes == sent.bytes,
          "{container} {track}/{index}: the bytes"
        );
        assert_eq!(
          seen.key, sent.key,
          "{container} {track}/{index}: the key flag"
        );
        // A packet whose start shares a PES packet with an earlier one's
        // reads back with no time of its own.
        let (seen_ms, sent_ms) = (in_ms(seen.pts), in_ms(sent.pts));
        assert!(
          index > 0 && seen_ms.is_none()
            || matches!((seen_ms, sent_ms), (Some(a), Some(b)) if (a - b - preload).abs() <= 1),
          "{container} {track}/{index}: presented at {seen_ms:?}, offered at {sent_ms:?}"
        );
      }
    }
  }
  // A picture alone, decoded from zero: no time before it to move, the
  // preload's move alone reported.
  let (table, frames) = first_track(&intra_mpeg2_mkv(&corpus), TrackKind::Video, 3);
  let mut muxer = muxer_into(
    &output(&corpus, "l93-picture.mpg"),
    ContainerName::new("mpeg"),
    table,
  );
  for (index, frame) in frames.iter().enumerate() {
    let at = 40 * index as i64;
    muxer
      .write_video(video_at(&video_of(frame), Some(at), at))
      .map_err(|error| error.map_packet(drop))
      .expect("a picture");
  }
  assert!(
    muxer
      .report()
      .uncarried()
      .contains(&Uncarried::TimesShifted),
    "the preload alone"
  );
  muxer.finish().expect("finished");
}

/// **L94 — the MPEG program stream's refusals** (table H; `mpegenc.c`
/// 303–1257; `mpeg.c` 528–630): at open, a codec its start or its reader
/// does not take — Opus; big-endian PCM, which reads back as DVD PCM; a
/// subtitle but DVD's ([`MuxOpenError::CodecNotCarried`]); DVD PCM of a
/// sample rate its header cannot name ([`MuxOpenError::ConfigurationInvalid`]);
/// a ninth AC-3 stream, past AC-3's ids into DTS's; a third MPEG-2 video
/// stream beside H.264, its id H.264's first; a 35th stream the system
/// header has no room for ([`MuxOpenError::StreamIds`]) — each one fewer
/// opening. In packets: an empty packet, which the writer writes nothing
/// of ([`PayloadRule::Empty`]); a DVD PCM packet shorter than its 3-byte
/// header ([`PayloadRule::PcmHeader`]); a time past half of `int64_t`'s
/// range, which leaves the writer's clock no room before it overflows
/// ([`FfmpegMuxRefusal::TimeWraps`]), a tick earlier taken.
///
/// PLANT: each check removed takes its packet, or opens.
#[test]
fn l94_mpeg_ps_refuses_what_its_ids_and_headers_cannot_hold() {
  use ffmpeg_next::ffi::AVCodecID as Id;
  let Some(corpus) = Corpus::new() else { return };
  let open = |name: &str, container: &str, table| {
    let target = output(&corpus, name);
    let result = FfmpegOwnedMuxer::open(
      File::create(&target).expect("create"),
      ContainerName::new(container),
      table,
      &[],
      PacketLimits::default(),
    );
    match result {
      Ok((muxer, _)) => {
        muxer.finish().expect("finished");
        Ok(())
      }
      Err(error) => {
        assert_eq!(std::fs::metadata(&target).expect("the file").len(), 0);
        Err(error)
      }
    }
  };
  let source = picture_and_sound(
    &corpus,
    "mpeg2-mp2.mkv",
    &["-c:v", "mpeg2video", "-bf", "0"],
    &["-c:a", "mp2", "-ac", "2"],
  );
  let pcm = pcm_mkv(&corpus);
  let ac3 = audio_file(&corpus, "ac3.mkv", "ac3");
  for (name, table) in [
    ("l94-opus.mpg", table_of(&opus_mkv(&corpus), TrackKind::Audio)),
    (
      "l94-s16be.mpg",
      vec![recoded(table_of(&pcm, TrackKind::Audio).remove(0), Id::AV_CODEC_ID_PCM_S16BE, &[])],
    ),
    (
      "l94-ass.mpg",
      table_of(
        &generated(
          &corpus,
          "ass.mkv",
          &[
            "-i",
            written_text(
              &corpus,
              "two.srt",
              "1\n00:00:00,000 --> 00:00:01,000\nhello\n\n2\n00:00:01,000 --> 00:00:02,000\nworld\n\n",
            )
            .to_str()
            .expect("utf-8 path"),
            "-c:s",
            "ass",
          ],
        ),
        TrackKind::Subtitle,
      ),
    ),
  ] {
    assert!(
      matches!(
        open(name, "mpeg", table),
        Err(MuxOpenError::CodecNotCarried { .. })
      ),
      "{name}"
    );
  }
  let dvd_pcm = |rate: i32| {
    vec![recoded(
      audio_rated(&pcm, rate).remove(0),
      Id::AV_CODEC_ID_PCM_DVD,
      &[],
    )]
  };
  assert!(matches!(
    open("l94-rate.vob", "vob", dvd_pcm(22_050)),
    Err(MuxOpenError::ConfigurationInvalid { .. })
  ));
  open("l94-rate.vob", "vob", dvd_pcm(48_000)).expect("48 kHz");

  let copies = |path: &Path, kind: TrackKind, count: usize| {
    (0..count)
      .map(|_| table_of(path, kind).remove(0))
      .collect::<Vec<_>>()
  };
  assert!(matches!(
    open("l94-ac3.vob", "vob", copies(&ac3, TrackKind::Audio, 9)),
    Err(MuxOpenError::StreamIds { track }) if track.get() == 8
  ));
  open("l94-ac3.vob", "vob", copies(&ac3, TrackKind::Audio, 8)).expect("eight AC-3 ids");
  let h264 = table_of(&corpus.multi_track_mkv(), TrackKind::Video).remove(0);
  let mut videos = copies(&source, TrackKind::Video, 3);
  videos.push(h264);
  assert!(matches!(
    open("l94-video.vob", "vob", videos),
    Err(MuxOpenError::StreamIds { track }) if track.get() == 3
  ));
  let mut videos = copies(&source, TrackKind::Video, 2);
  videos.push(table_of(&corpus.multi_track_mkv(), TrackKind::Video).remove(0));
  open("l94-video.vob", "vob", videos).expect("two MPEG-2 ids before H.264's");
  let mut many = copies(&source, TrackKind::Audio, 32);
  many.extend(copies(&source, TrackKind::Video, 3));
  assert!(matches!(
    open("l94-many.mpg", "mpeg", many),
    Err(MuxOpenError::StreamIds { track }) if track.get() == 34
  ));
  let mut many = copies(&source, TrackKind::Audio, 32);
  many.extend(copies(&source, TrackKind::Video, 2));
  open("l94-many.mpg", "mpeg", many).expect("a system header of 114 bytes");

  let (table, frames) = first_track(&source, TrackKind::Audio, 1);
  let mut muxer = muxer_into(
    &output(&corpus, "l94-empty.mpg"),
    ContainerName::new("mpeg"),
    table,
  );
  let empty = audio_at(&audio_holding(&audio_of(&frames[0]), &[]), 0);
  let (refusal, back) = by_container(muxer.write_audio(empty.clone()));
  assert_eq!(refusal, payload(PayloadRule::Empty));
  untouched!(empty, back);
  muxer.finish().expect("finished");

  // A time past half of `int64_t`'s range, in 90 kHz.
  let (table, frames) = first_track(&source, TrackKind::Audio, 1);
  let mut muxer = muxer_into(
    &output(&corpus, "l94-end.mpg"),
    ContainerName::new("mpeg"),
    table,
  );
  let at_90k = Timebase::new(1, NonZeroI32::new(90_000).expect("non-zero"));
  let ending = |at: i64| {
    AudioTrackPacket::new(
      TrackIndex::new(0),
      audio_of(&frames[0])
        .with_pts(Some(Timestamp::new(at, at_90k)))
        .with_dts(Some(Timestamp::new(at, at_90k))),
    )
  };
  muxer
    .write_audio(ending(0))
    .map_err(|error| error.map_packet(drop))
    .expect("the first");
  let far = ending(i64::MAX / 2 + 1);
  let (refusal, back) = by_container(muxer.write_audio(far.clone()));
  assert_eq!(
    refusal,
    FfmpegMuxRefusal::TimeWraps {
      track: TrackIndex::new(0),
      timestamp: Timestamp::new(i64::MAX / 2 + 1, at_90k),
    }
  );
  untouched!(far, back);
  muxer
    .write_audio(ending(i64::MAX / 2))
    .map_err(|error| error.map_packet(drop))
    .expect("a tick earlier");
  muxer.finish().expect("finished");

  let (_, frames) = first_track(&pcm, TrackKind::Audio, 1);
  let mut muxer = muxer_into(
    &output(&corpus, "l94-pcm.vob"),
    ContainerName::new("vob"),
    dvd_pcm(48_000),
  );
  let short = audio_at(&audio_holding(&audio_of(&frames[0]), &[0, 1]), 0);
  let (refusal, back) = by_container(muxer.write_audio(short.clone()));
  assert_eq!(refusal, payload(PayloadRule::PcmHeader));
  untouched!(short, back);
  muxer
    .write_audio(audio_at(
      &audio_holding(&audio_of(&frames[0]), &[0, 1, 0x80, 0, 0]),
      0,
    ))
    .map_err(|error| error.map_packet(drop))
    .expect("a header and a sample");
  muxer.finish().expect("finished");
}

/// **L95 — H.264 that reorders, AAC, MPEG audio, AC-3 and Opus round-trip
/// through MPEG-TS, and the report says what MPEG-TS keeps** (table H;
/// `mpegtsenc.c` 537–2240): a video packet reads back with its times, in
/// a PES packet of its own, its key flag, and an access unit delimiter put
/// before it ([`Rewrite::AccessUnitDelimiters`]); audio gathered into PES
/// packets, its times counted by the reader's parser ([`Rewrite::Timeline`]),
/// AAC's frames behind the ADTS headers its configuration makes
/// ([`Rewrite::AdtsHeaders`]), the rest as written, Opus's times moved
/// past its pre-skip's start before zero ([`Uncarried::TimesShifted`]),
/// its trims kept in its control headers — MPEG audio's and AC-3's, which
/// MPEG-TS has no place for, reported dropped ([`Uncarried::SideData`]);
/// no duration
/// ([`Rewrite::Durations`]); the H.264 filter named at the packet
/// it converts; an audio stream's language of three bytes, with the first
/// of its clean-effects, hearing-impaired and visual-impaired flags, kept —
/// the video's language and flags, and the audio's other flags, reported
/// ([`Uncarried::Language`], [`Uncarried::Disposition`]).
///
/// PLANT: the delimiters unreported; the audio's flags kept as every one;
/// the audio's times taken as written; Opus's trims reported dropped.
#[test]
fn l95_streams_round_trip_through_mpeg_ts() {
  let Some(corpus) = Corpus::new() else { return };
  let source = h264_aac(&corpus, "h264-aac.mp4");
  let rewritten = |track: usize, rewrite| Uncarried::Rewritten {
    track: TrackIndex::new(track),
    rewrite,
  };
  let mut table = whole_table(&source);
  table[0].set_language(Some("eng".into()));
  table[0].set_disposition(TrackDisposition::DEFAULT);
  table[1].set_language(Some("fra".into()));
  table[1].set_disposition(TrackDisposition::HEARING_IMPAIRED | TrackDisposition::VISUAL_IMPAIRED);
  let target = output(&corpus, "l95.ts");
  let report = remuxed(&source, &target, "mpegts", table, &[]);
  assert_eq!(
    field_rows(&report),
    vec![
      rewritten(0, Rewrite::AccessUnitDelimiters),
      rewritten(0, Rewrite::Durations),
      rewritten(0, Rewrite::SyncFromBitstream),
      rewritten(1, Rewrite::AdtsHeaders),
      rewritten(1, Rewrite::Timeline),
      rewritten(1, Rewrite::SyncFromBitstream),
      // Named at the first packet, the one it converts.
      Uncarried::Filtered {
        track: TrackIndex::new(0),
        filter: AutomaticFilter::H264Mp4ToAnnexB,
      },
    ]
  );
  assert_eq!(
    lines(&report, |line| matches!(
      line,
      Uncarried::Language { .. } | Uncarried::Disposition { .. }
    )),
    vec![
      Uncarried::Language {
        track: TrackIndex::new(0),
        tag: "eng".into(),
      },
      Uncarried::Disposition {
        track: TrackIndex::new(0),
        flags: TrackDisposition::DEFAULT,
      },
      Uncarried::Disposition {
        track: TrackIndex::new(1),
        flags: TrackDisposition::VISUAL_IMPAIRED,
      },
    ]
  );
  let offered = packets(&source);
  let back = packets(&target);
  let lift = offered
    .iter()
    .filter_map(|track| in_ms(track.first().and_then(|first| first.dts)))
    .min()
    .map_or(0, |dts| (-dts).max(0));
  assert_eq!(back[0].len(), offered[0].len(), "a video packet a packet");
  for (index, (seen, sent)) in back[0].iter().zip(&offered[0]).enumerate() {
    assert!(
      seen.bytes.starts_with(&[0, 0, 0, 1, 9]) || seen.bytes.starts_with(&[0, 0, 1, 9]),
      "video {index}: behind a delimiter"
    );
    assert_eq!(seen.key, sent.key, "video {index}: the key flag");
    assert_eq!(
      in_ms(seen.pts),
      in_ms(sent.pts).map(|pts| pts + lift),
      "video {index}"
    );
    assert_eq!(
      in_ms(seen.dts),
      in_ms(sent.dts).map(|dts| dts + lift),
      "video {index}"
    );
  }
  assert_eq!(back[1].len(), offered[1].len(), "an AAC frame a packet");
  for (index, (seen, sent)) in back[1].iter().zip(&offered[1]).enumerate() {
    assert!(
      seen.bytes.len() == sent.bytes.len() + 7 && seen.bytes[7..] == sent.bytes[..],
      "AAC {index}: the frame behind its ADTS header"
    );
    assert!(
      matches!((in_ms(seen.pts), in_ms(sent.pts)), (Some(a), Some(b)) if (a - b - lift).abs() <= 1),
      "AAC {index}"
    );
  }
  let read = FfmpegOwnedDemuxer::open(&target).expect("open");
  let rows: Vec<_> = read
    .tracks()
    .iter()
    .map(|row| {
      (
        row.language().map(|tag| tag.as_str().to_owned()),
        row.disposition(),
      )
    })
    .collect();
  assert_eq!(
    rows,
    [
      (None, TrackDisposition::empty()),
      (Some("fra".to_owned()), TrackDisposition::HEARING_IMPAIRED),
    ]
  );

  for (name, path) in [
    ("mp2", audio_file(&corpus, "mp2.mkv", "mp2")),
    ("ac3", audio_file(&corpus, "ac3.mkv", "ac3")),
    ("opus", opus_of_two_bytes_mkv(&corpus)),
  ] {
    assert!(
      packets(&path)[0]
        .iter()
        .all(|packet| packet.bytes.len() >= 2),
      "{name}: the premise, no packet MPEG-TS refuses as too short"
    );
    let target = output(&corpus, &format!("l95-{name}.ts"));
    let report = remuxed(
      &path,
      &target,
      "mpegts",
      table_of(&path, TrackKind::Audio),
      &[],
    );
    assert_eq!(
      field_rows(&report),
      vec![rewritten(0, Rewrite::Timeline)],
      "{name}"
    );
    // Opus's first packet, before zero by its pre-skip, moves the stream.
    let lift = packets(&path)[0]
      .first()
      .and_then(|first| in_ms(first.pts))
      .map_or(0, |pts| (-pts).max(0));
    assert_eq!(
      report.uncarried().contains(&Uncarried::TimesShifted),
      lift > 0,
      "{name}"
    );
    // Opus's start and end trims, in its control headers; MPEG audio's
    // and AC-3's MPEG-TS has no place for.
    let trims = lines(&report, |line| matches!(line, Uncarried::SideData { .. }));
    if name == "opus" {
      assert_eq!(trims, vec![], "{name}: the side data MPEG-TS keeps");
    } else {
      assert!(
        trims.iter().all(|line| matches!(
          line,
          Uncarried::SideData {
            fate: SideDataFate::Dropped,
            ..
          }
        )),
        "{name}: {trims:?}"
      );
    }
    read_back_lifted(&path, &target, &[0], lift);
  }
}

/// **L96 — MPEG-TS's refusals** (table H; `mpegtsenc.c` 216–2240): at open,
/// a codec MPEG-TS writes as private data its reader may not name —
/// Vorbis ([`MuxOpenError::CodecNotCarried`]); Opus of a channel mapping
/// its PMT cannot describe, and AAC of a configuration ADTS cannot write
/// ([`MuxOpenError::ConfigurationInvalid`]); a 91st MPEG audio stream with
/// its language, starting past the PMT section's 988th byte, and a DVB
/// subtitle stream of three codes that takes the section past its 1012
/// ([`MuxOpenError::StreamIds`]), one fewer stream before each opening.
/// In packets: a reordering H.264 packet with no presentation time
/// ([`FfmpegMuxRefusal::MissingPts`]); with the filters off, H.264 off a
/// start code, and an Annex B packet of four bytes
/// ([`PayloadRule::StartCode`]); an AAC packet of one byte
/// ([`PayloadRule::TooShort`]); a raw AAC frame on a track with no
/// configuration ([`PayloadRule::AdtsFrame`]), and one past the 13 bits of
/// its ADTS header ([`PayloadRule::AdtsFrameLength`]), an ADTS frame taken
/// as it is; an empty packet ([`PayloadRule::Empty`]); a time past the
/// room of its clock reference, 300 times its decode time in `int64_t`
/// ([`FfmpegMuxRefusal::TimeWraps`]), one at it taken.
///
/// PLANT: each check removed takes its packet, or opens.
#[test]
fn l96_mpeg_ts_refuses_what_its_pmt_and_pes_cannot_hold() {
  use ffmpeg_next::ffi::AVCodecID as Id;
  let Some(corpus) = Corpus::new() else { return };
  let ts = || ContainerName::new("mpegts");
  let open = |name: &str, table| {
    let target = output(&corpus, name);
    let result = FfmpegOwnedMuxer::open(
      File::create(&target).expect("create"),
      ts(),
      table,
      &[],
      PacketLimits::default(),
    );
    match result {
      Ok((muxer, _)) => {
        muxer.finish().expect("finished");
        Ok(())
      }
      Err(error) => {
        assert_eq!(std::fs::metadata(&target).expect("the file").len(), 0);
        Err(error)
      }
    }
  };
  let vorbis = picture_and_sound(
    &corpus,
    "vp8-vorbis.webm",
    &["-c:v", "libvpx"],
    &["-c:a", "vorbis", "-strict", "-2", "-ac", "2"],
  );
  assert!(matches!(
    open("l96-vorbis.ts", table_of(&vorbis, TrackKind::Audio)),
    Err(MuxOpenError::CodecNotCarried { .. })
  ));
  let opus = opus_mkv(&corpus);
  let mut head = table_of(&opus, TrackKind::Audio)[0]
    .side_data()
    .extradata_ref()
    .as_ref()
    .to_vec();
  head.truncate(19);
  head[18] = 255;
  assert!(matches!(
    open(
      "l96-opus.ts",
      vec![
        table_of(&opus, TrackKind::Audio)
          .remove(0)
          .with_extradata(Some(bytes_of(&head)))
      ]
    ),
    Err(MuxOpenError::ConfigurationInvalid { .. })
  ));
  let aac = table_of(&corpus.multi_track_mkv(), TrackKind::Audio);
  assert!(matches!(
    open(
      "l96-aac.ts",
      aac
        .into_iter()
        .map(|row| row.with_extradata(Some(bytes_of(&[0xF8, 0x00]))))
        .collect()
    ),
    Err(MuxOpenError::ConfigurationInvalid { .. })
  ));
  let mp2 = audio_file(&corpus, "mp2.mkv", "mp2");
  let languaged = |count: usize| {
    (0..count)
      .map(|_| {
        let mut row = table_of(&mp2, TrackKind::Audio).remove(0);
        row.set_language(Some("eng".into()));
        row
      })
      .collect::<Vec<_>>()
  };
  assert!(matches!(
    open("l96-pmt.ts", languaged(91)),
    Err(MuxOpenError::StreamIds { track }) if track.get() == 90
  ));
  open("l96-pmt.ts", languaged(90)).expect("90 streams of 11 bytes in the PMT");
  // 88 streams to the 972nd byte, then a DVB subtitle stream of three
  // codes — 31 bytes — within 1012; after 89, past it.
  let ass = generated(
    &corpus,
    "ass.mkv",
    &[
      "-i",
      written_text(
        &corpus,
        "two.srt",
        "1\n00:00:00,000 --> 00:00:01,000\nhello\n\n2\n00:00:01,000 --> 00:00:02,000\nworld\n\n",
      )
      .to_str()
      .expect("utf-8 path"),
      "-c:s",
      "ass",
    ],
  );
  // Its configuration an entry a code, five bytes each (L107).
  let dvb = || {
    let mut row = recoded(
      table_of(&ass, TrackKind::Subtitle).remove(0),
      Id::AV_CODEC_ID_DVB_SUBTITLE,
      &[0, 1, 0, 1, 0x10].repeat(3),
    );
    row.set_language(Some("eng,fra,deu".into()));
    row
  };
  let mut full = languaged(88);
  full.push(dvb());
  open("l96-section.ts", full).expect("1003 bytes");
  let mut full = languaged(89);
  full.push(dvb());
  assert!(matches!(
    open("l96-section.ts", full),
    Err(MuxOpenError::StreamIds { track }) if track.get() == 89
  ));

  let source = h264_aac(&corpus, "h264-aac.mp4");
  let (table, frames) = first_track(&source, TrackKind::Video, 1);
  let frame = video_of(&frames[0]);
  let track = TrackIndex::new(0);
  let mut muxer = muxer_with(&output(&corpus, "l96-h264.ts"), ts(), table, filters(false));
  let missing = video_at(&frame, None, 0);
  let (refusal, back) = by_container(muxer.write_video(missing.clone()));
  assert_eq!(refusal, FfmpegMuxRefusal::MissingPts { track });
  untouched!(missing, back);
  for bytes in [frame.data().as_ref().to_vec(), vec![0, 0, 1, 0x65]] {
    let offered = video_at(&video_holding(&frame, &bytes), Some(0), 0);
    let (refusal, back) = by_container(muxer.write_video(offered.clone()));
    assert_eq!(refusal, payload(PayloadRule::StartCode));
    untouched!(offered, back);
  }
  muxer.finish().expect("finished");

  let (table, frames) = first_track(&source, TrackKind::Audio, 1);
  let raw = audio_of(&frames[0]);
  let mut muxer = muxer_into(&output(&corpus, "l96-aac.ts"), ts(), table);
  for (bytes, rule) in [
    (vec![0x21], PayloadRule::TooShort),
    (vec![0x21; 8185], PayloadRule::AdtsFrameLength),
    (vec![], PayloadRule::Empty),
  ] {
    let offered = audio_at(&audio_holding(&raw, &bytes), 0);
    let (refusal, back) = by_container(muxer.write_audio(offered.clone()));
    assert_eq!(refusal, payload(rule), "{} bytes", bytes.len());
    untouched!(offered, back);
  }
  let mut framed = vec![0xFF, 0xF1, 0x4C, 0x80, 0x00, 0x00, 0xFC];
  framed.extend_from_slice(raw.data().as_ref());
  let length = framed.len();
  framed[3] = 0x80 | ((length >> 11) & 3) as u8;
  framed[4] = (length >> 3) as u8;
  framed[5] = (((length & 7) << 5) | 0x1F) as u8;
  muxer
    .write_audio(audio_at(&audio_holding(&raw, &framed), 0))
    .map_err(|error| error.map_packet(drop))
    .expect("an ADTS frame, written as it is");
  muxer.finish().expect("finished");
  // A time past the clock's room, `i64::MAX / 600` ticks of 90 kHz.
  let (table, frames) = first_track(&mp2, TrackKind::Audio, 1);
  let mut muxer = muxer_into(&output(&corpus, "l96-clock.ts"), ts(), table);
  let at_90k = Timebase::new(1, NonZeroI32::new(90_000).expect("non-zero"));
  let at = |ticks: i64| {
    AudioTrackPacket::new(
      TrackIndex::new(0),
      audio_of(&frames[0])
        .with_pts(Some(Timestamp::new(ticks, at_90k)))
        .with_dts(Some(Timestamp::new(ticks, at_90k))),
    )
  };
  muxer
    .write_audio(at(0))
    .map_err(|error| error.map_packet(drop))
    .expect("the first");
  let far = at(i64::MAX / 600 + 1);
  let (refusal, back) = by_container(muxer.write_audio(far.clone()));
  assert_eq!(
    refusal,
    FfmpegMuxRefusal::TimeWraps {
      track: TrackIndex::new(0),
      timestamp: Timestamp::new(i64::MAX / 600 + 1, at_90k),
    }
  );
  untouched!(far, back);
  muxer
    .write_audio(at(i64::MAX / 600))
    .map_err(|error| error.map_packet(drop))
    .expect("within it");
  muxer.finish().expect("finished");

  let bare: Vec<_> = table_of(&source, TrackKind::Audio)
    .into_iter()
    .map(|row| row.with_extradata(Some(bytes_of(&[]))))
    .collect();
  let mut muxer = muxer_with(&output(&corpus, "l96-bare.ts"), ts(), bare, filters(false));
  let offered = audio_at(&raw, 0);
  let (refusal, back) = by_container(muxer.write_audio(offered.clone()));
  assert_eq!(refusal, payload(PayloadRule::AdtsFrame));
  untouched!(offered, back);
  muxer.finish().expect("finished");
}

/// **L87 — what ASF, and the RIFF formats' audio header, cannot count is
/// refused at open, nothing written** (table H): a chapter title whose
/// marker entry passes 16 bits — 12 bytes and the title in UTF-16 with its
/// terminator (`asfenc.c` 337) — refused
/// ([`MuxOpenError::ChapterTitleTooLong`]), one a character shorter
/// opening, and opening in the streaming form, which writes no marker; a
/// chapter starting at 2^32 ms, whose marker's send time could pass its
/// DWORD (297–308, 338), refused ([`MuxOpenError::ChapterSpan`]), one a
/// millisecond earlier opening; a video configuration past the 16-bit
/// format data size ASF writes it with (645), and an audio one past the
/// WAVEFORMATEX's `cbSize` WAV, AVI and ASF write it after (`riffenc.c`
/// 193–223), refused ([`MuxOpenError::ConfigurationInvalid`]), a byte
/// shorter opening; and a table whose streaming header could pass the
/// 16-bit length of its chunk (`asfenc.c` 281–295, 708–716) refused
/// ([`MuxOpenError::HeaderTooLong`]), while one just within the bound
/// writes its header's chunk with its length whole: the first data chunk
/// lies where it says. A picture whose bytes the bitmap header AVI and ASF
/// write counts past an `int` (`riffenc.c` 265) is refused
/// ([`MuxOpenError::Dimensions`]), the largest square within it opening.
///
/// PLANT: each judgment off, its case opening.
#[test]
fn l87_asf_and_riff_refuse_at_open_what_their_fields_cannot_count() {
  let Some(corpus) = Corpus::new() else { return };
  let source = picture_and_sound(
    &corpus,
    "wmv2-wma2.mkv",
    &["-c:v", "wmv2"],
    &["-c:a", "wmav2", "-ac", "2"],
  );
  let table = |kinds: &[TrackKind], video: Option<usize>, audio: Option<usize>| {
    let demuxer = FfmpegOwnedDemuxer::open(&source).expect("open");
    demuxer
      .tracks()
      .iter()
      .filter(|row| kinds.contains(&row.kind()))
      .map(|row| {
        let description =
          TrackDescription::from_track_info(&**row, None, ticket(row)).expect("a description");
        let bytes = match row.kind() {
          TrackKind::Video => video,
          _ => audio,
        };
        match bytes {
          Some(len) => description.with_extradata(Some(bytes_of(&vec![0x5A; len]))),
          None => description,
        }
      })
      .collect::<Vec<_>>()
  };
  let open = |name: &str,
              container: &str,
              table: Vec<TrackDescription<Ffmpeg, FfmpegBytes, CodecTicket>>,
              chapters: &[Chapter<Ffmpeg>]| {
    let target = output(&corpus, name);
    let result = FfmpegOwnedMuxer::open(
      File::create(&target).expect("create"),
      ContainerName::new(container),
      table,
      chapters,
      PacketLimits::default(),
    );
    match result {
      Ok((muxer, report)) => {
        muxer.finish().expect("finished");
        Ok(report)
      }
      Err(error) => {
        assert_eq!(
          std::fs::metadata(&target).expect("the file").len(),
          0,
          "{name}: nothing written"
        );
        Err(error)
      }
    }
  };
  let both = [TrackKind::Video, TrackKind::Audio];

  let titled = |characters: usize| [chapter_of(0, 0, 500, Some(&"a".repeat(characters)))];
  assert!(matches!(
    open(
      "l87-title.asf",
      "asf",
      table(&both, None, None),
      &titled(32_761)
    ),
    Err(MuxOpenError::ChapterTitleTooLong { chapter: 0 })
  ));
  open(
    "l87-title.asf",
    "asf",
    table(&both, None, None),
    &titled(32_760),
  )
  .expect("12 bytes and 32 761 characters of UTF-16: 65 534");
  let report = open(
    "l87-title.asf_stream",
    "asf_stream",
    table(&both, None, None),
    &titled(32_761),
  )
  .expect("no marker written");
  assert_eq!(
    lines(&report, |line| matches!(line, Uncarried::Chapter { .. })),
    vec![Uncarried::Chapter { chapter: 0 }]
  );

  let starting = |start: i64| [chapter_of(0, start, start + 1000, Some("far"))];
  assert!(matches!(
    open(
      "l87-start.asf",
      "asf",
      table(&both, None, None),
      &starting(1 << 32)
    ),
    Err(MuxOpenError::ChapterSpan { chapter: 0 })
  ));
  open(
    "l87-start.asf",
    "asf",
    table(&both, None, None),
    &starting((1 << 32) - 1),
  )
  .expect("a millisecond earlier");

  assert!(matches!(
    open("l87-video.asf", "asf", table(&[TrackKind::Video], Some(65_496), None), &[]),
    Err(MuxOpenError::ConfigurationInvalid { track, .. }) if track.get() == 0
  ));
  open(
    "l87-video.asf",
    "asf",
    table(&[TrackKind::Video], Some(65_495), None),
    &[],
  )
  .expect("40 bytes and 65 495: 65 535");
  for container in ["wav", "avi", "asf"] {
    let name = format!("l87-audio.{container}");
    assert!(
      matches!(
        open(&name, container, table(&[TrackKind::Audio], None, Some(65_536)), &[]),
        Err(MuxOpenError::ConfigurationInvalid { track, .. }) if track.get() == 0
      ),
      "{container}"
    );
    open(
      &name,
      container,
      table(&[TrackKind::Audio], None, Some(65_535)),
      &[],
    )
    .unwrap_or_else(|error| panic!("{container}: {error:?}"));
  }
  // The WAVEFORMATEXTENSIBLE's 22 bytes, for six channels of PCM in their
  // native layout — WAV states it, Matroska not — and HE-AAC's 12, for its
  // tag 0x1610, before the configuration.
  #[rustfmt::skip]
  let pcm51 = generated(&corpus, "pcm51.wav", &[
    "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000:duration=0.2",
    "-ac", "6", "-c:a", "pcm_s16le",
  ]);
  #[rustfmt::skip]
  let heaac = generated(&corpus, "aac-1610.avi", &[
    "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000:duration=0.2",
    "-ac", "2", "-c:a", "aac", "-tag:a", "0x1610",
  ]);
  let configured = |path: &Path, len: usize| {
    table_of(path, TrackKind::Audio)
      .into_iter()
      .map(|row| row.with_extradata(Some(bytes_of(&vec![0x5A; len]))))
      .collect::<Vec<_>>()
  };
  for (path, beside) in [(&pcm51, 22), (&heaac, 12)] {
    for container in ["wav", "avi", "asf"] {
      let name = format!("l87-beside.{container}");
      assert!(
        matches!(
          open(&name, container, configured(path, 65_536 - beside), &[]),
          Err(MuxOpenError::ConfigurationInvalid { .. })
        ),
        "{container}, {beside}"
      );
      open(&name, container, configured(path, 65_535 - beside), &[])
        .unwrap_or_else(|error| panic!("{container}, {beside}: {error:?}"));
    }
  }

  // The bitmap header's count of a picture's bytes, an `int`: the largest
  // square within it opens, one a pixel wider and taller is refused.
  let bits = match table(&[TrackKind::Video], None, None)[0]
    .side_data()
    .bits_per_coded_sample()
  {
    0 => 24,
    bits => i64::from(bits),
  };
  let side = (1..65_536_i64)
    .rev()
    .find(|side| side * side * bits + 7 <= i64::from(i32::MAX))
    .expect("a side");
  for container in ["avi", "asf"] {
    let name = format!("l87-picture.{container}");
    let sized = |side: i64| {
      let side = i32::try_from(side).expect("a side");
      video_sized(&source, side, side)
    };
    assert!(
      matches!(
        open(&name, container, sized(side + 1), &[]),
        Err(MuxOpenError::Dimensions { track }) if track.get() == 0
      ),
      "{container}"
    );
    open(&name, container, sized(side), &[])
      .unwrap_or_else(|error| panic!("{container}, {side}: {error:?}"));
  }

  // 600 and 470 a stream beside the configurations: WMA2's ten bytes and
  // the video's, to 65 535 at most.
  assert!(matches!(
    open(
      "l87-header.asf_stream",
      "asf_stream",
      table(&both, Some(63_986), None),
      &[]
    ),
    Err(MuxOpenError::HeaderTooLong)
  ));
  let target = output(&corpus, "l87-header.asf_stream");
  let (mut muxer, _) = FfmpegOwnedMuxer::open(
    File::create(&target).expect("create"),
    ContainerName::new("asf_stream"),
    table(&both, Some(63_985), None),
    &[],
    PacketLimits::default(),
  )
  .expect("within the bound");
  let mut demuxer = FfmpegOwnedDemuxer::open(&source).expect("open");
  while let Some(packet) = demuxer.next_packet().expect("a packet or the end") {
    match packet {
      DemuxedPacket::Video(p) => muxer
        .write_video(VideoTrackPacket::new(p.track(), p.packet().clone()))
        .map_err(|error| error.map_packet(drop)),
      DemuxedPacket::Audio(p) => muxer
        .write_audio(AudioTrackPacket::new(p.track(), p.packet().clone()))
        .map_err(|error| error.map_packet(drop)),
      other => panic!("{other:?}"),
    }
    .expect("a packet");
  }
  muxer.finish().expect("finished");
  let bytes = std::fs::read(&target).expect("the file");
  let kind = |at: usize| u16::from_le_bytes([bytes[at], bytes[at + 1]]);
  let length = usize::from(u16::from_le_bytes([bytes[2], bytes[3]]));
  assert_eq!(kind(0), 0x4824, "the header's chunk");
  assert!(length > 63_985, "the configuration within it");
  assert_eq!(
    kind(4 + length),
    0x4424,
    "the first data chunk where the length says"
  );
}

/// **L88 — WAV, AVI and ASF take a configured AAC track's packets raw, and
/// ASF's reader marks every audio packet a key frame** (table H): an
/// ADTS-framed packet on a track whose configuration is stated, which
/// they would write as it comes into a stream whose format declares raw
/// AAC (`riffenc.c` 193–223), refused ([`PayloadRule::AdtsHeader`]), the
/// raw frame taken; an AAC packet ASF is offered unmarked, which its
/// reader marks a key frame (`asfdec_f.c` 1256–1259), reported at that
/// packet ([`Rewrite::KeyFramesMarked`]), and read back marked.
///
/// PLANT: the raw rule off for them, the ADTS packet taken; ASF's audio
/// not all key frames, the report silent.
#[test]
fn l88_riff_formats_take_aac_raw_and_asf_marks_its_audio() {
  let Some(corpus) = Corpus::new() else { return };
  let source = corpus.multi_track_mkv();
  let (_, packets) = first_track(&source, TrackKind::Audio, 2);
  let raw = audio_of(&packets[0]);
  let mut framed = vec![0xFF, 0xF1, 0x4C, 0x80, 0x00, 0x00, 0xFC];
  framed.extend_from_slice(raw.data().as_ref());
  for container in ["wav", "avi", "asf"] {
    let target = output(&corpus, &format!("l88.{container}"));
    let mut muxer = muxer_into(
      &target,
      ContainerName::new(container),
      table_of(&source, TrackKind::Audio),
    );
    let offered = audio_at(&audio_holding(&raw, &framed), 0);
    let (refusal, back) = by_container(muxer.write_audio(offered.clone()));
    assert_eq!(refusal, payload(PayloadRule::AdtsHeader), "{container}");
    untouched!(offered, back);
    muxer
      .write_audio(audio_at(&raw, 0))
      .map_err(|error| error.map_packet(drop))
      .expect("the raw frame");
    if container == "asf" {
      let unmarked = audio_at(&audio_of(&packets[1]), 21);
      let unmarked = AudioTrackPacket::new(
        unmarked.track(),
        unmarked.packet().clone().with_flags(PacketFlags::empty()),
      );
      muxer
        .write_audio(unmarked)
        .map_err(|error| error.map_packet(drop))
        .expect("an unmarked frame");
      assert_eq!(
        lines(muxer.report(), |line| matches!(
          line,
          Uncarried::Rewritten {
            rewrite: Rewrite::KeyFramesMarked,
            ..
          }
        )),
        vec![Uncarried::Rewritten {
          track: TrackIndex::new(0),
          rewrite: Rewrite::KeyFramesMarked,
        }]
      );
    }
    muxer.finish().expect("finished");
    if container == "asf" {
      let (_, back) = whole_track(&target, TrackKind::Audio);
      assert!(
        back
          .iter()
          .all(|packet| audio_of(packet).flags().contains(PacketFlags::KEY)),
        "every audio packet read back a key frame"
      );
    }
  }
}

/// MPEG-2 video of intra pictures alone and 48 kHz PCM in Matroska, the
/// sound 30 ms longer than the picture: 25 frames, 49 440 samples.
#[rustfmt::skip]
fn mxf_source(corpus: &Corpus) -> PathBuf {
  generated(corpus, "mxf-source.mkv", &[
    "-f", "lavfi", "-i", "testsrc2=size=64x48:rate=25:duration=1",
    "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000:duration=1.03",
    "-map", "0:v", "-map", "1:a",
    "-c:v", "mpeg2video", "-g", "1", "-bf", "0", "-c:a", "pcm_s16le", "-ac", "2",
  ])
}

/// MPEG-2 video of intra pictures alone, in Matroska, by itself.
#[rustfmt::skip]
fn intra_mpeg2_mkv(corpus: &Corpus) -> PathBuf {
  generated(corpus, "mpeg2-intra.mkv", &[
    "-f", "lavfi", "-i", "testsrc2=size=64x48:rate=25:duration=0.4",
    "-c:v", "mpeg2video", "-g", "1", "-bf", "0",
  ])
}

/// MPEG-2 video of intra and predicted pictures, a picture in twelve
/// intra, in Matroska.
#[rustfmt::skip]
fn long_gop_mkv(corpus: &Corpus) -> PathBuf {
  generated(corpus, "mpeg2-gop.mkv", &[
    "-f", "lavfi", "-i", "testsrc2=size=64x48:rate=25:duration=0.4",
    "-c:v", "mpeg2video", "-g", "12", "-bf", "0",
  ])
}

/// DNxHR LB at 320 × 240 in Matroska: ten frames of one size.
#[rustfmt::skip]
fn dnxhr_mkv(corpus: &Corpus) -> PathBuf {
  generated(corpus, "dnxhr.mkv", &[
    "-f", "lavfi", "-i", "testsrc=size=320x240:rate=25:duration=0.4",
    "-c:v", "dnxhd", "-profile:v", "dnxhr_lb", "-pix_fmt", "yuv422p",
  ])
}

/// ProRes 422 Proxy at 320 × 240 in Matroska.
#[rustfmt::skip]
fn prores_mkv(corpus: &Corpus) -> PathBuf {
  generated(corpus, "prores.mkv", &[
    "-f", "lavfi", "-i", "testsrc=size=320x240:rate=25:duration=0.4",
    "-c:v", "prores_ks", "-profile:v", "0", "-pix_fmt", "yuv422p10le",
  ])
}

/// MXF D-10's 625-line picture: MPEG-2 4:2:2 at 50 Mbit/s, every frame
/// 250 000 bytes, and stereo PCM, in MOV, whose `btrt` box carries the bit
/// rate back.
#[rustfmt::skip]
fn imx_mov(corpus: &Corpus) -> PathBuf {
  let m2v = generated(corpus, "imx.m2v", &[
    "-f", "lavfi", "-i", "testsrc=size=720x608:rate=25:duration=0.2",
    "-pix_fmt", "yuv422p", "-c:v", "mpeg2video", "-g", "1", "-bf", "0",
    "-intra_vlc", "1", "-non_linear_quant", "1", "-dc", "10", "-qmax", "12",
    "-b:v", "50M", "-minrate", "50M", "-maxrate", "50M", "-bufsize", "2000000",
    "-rc_init_occupancy", "2000000", "-rc_max_vbv_use", "1", "-rc_min_vbv_use", "1",
  ]);
  generated(corpus, "imx.mov", &[
    "-i", m2v.to_str().expect("utf-8 path"),
    "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000:duration=0.2",
    "-c:v", "copy", "-c:a", "pcm_s16le", "-ac", "2",
  ])
}

/// `row` with its ticket's parameters rewritten by `edit`.
fn reticketed(
  mut row: TrackDescription<Ffmpeg, FfmpegBytes, CodecTicket>,
  edit: impl FnOnce(&mut ffmpeg_next::ffi::AVCodecParameters),
) -> TrackDescription<Ffmpeg, FfmpegBytes, CodecTicket> {
  let index = row.side_data().stream_index();
  let mut parameters = row.side_data().rebuild().expect("the parameters");
  // SAFETY: `parameters` owns its `AVCodecParameters`, live for the edit,
  // which writes plain fields.
  edit(unsafe { &mut *parameters.as_mut_ptr() });
  *row.side_data_mut() = CodecTicket::mirror(&parameters, index, usize::MAX).expect("the ticket");
  row
}

/// `row`, a video track's, stating `frame_rate`.
fn rerated(
  row: TrackDescription<Ffmpeg, FfmpegBytes, CodecTicket>,
  frame_rate: Option<Timebase>,
) -> TrackDescription<Ffmpeg, FfmpegBytes, CodecTicket> {
  use mediaencode::mediaspine::demuxer::VideoTrackParams;
  let (timebase, params, extradata, ticket, language, disposition, _) =
    row.into_parts_with_disposition();
  let params = match params {
    TrackParams::Video(params) => TrackParams::Video(VideoTrackParams::new(
      params.codec(),
      params.width(),
      params.height(),
      params.pixel_format().clone(),
      frame_rate,
    )),
    other => other,
  };
  TrackDescription::new(timebase, params, ticket)
    .expect("a timed track")
    .with_extradata(extradata)
    .with_language(language)
    .with_disposition(disposition)
}

/// Every packet of `path` written into a muxer of `container` over
/// `table` at `target`, each as it is, and the muxer finished: the whole
/// report, what only the end decides with it.
fn remuxed_to_the_end(
  path: &Path,
  target: &Path,
  container: &str,
  table: Vec<TrackDescription<Ffmpeg, FfmpegBytes, CodecTicket>>,
  chapters: &[Chapter<Ffmpeg>],
) -> mediaencode_ffmpeg::MuxReport {
  let (mut muxer, _) = FfmpegOwnedMuxer::open(
    File::create(target).expect("create"),
    ContainerName::new(container),
    table,
    chapters,
    PacketLimits::default(),
  )
  .expect("open");
  let mut demuxer = FfmpegOwnedDemuxer::open(path).expect("open");
  while let Some(packet) = demuxer.next_packet().expect("a packet or the end") {
    match packet {
      DemuxedPacket::Video(p) => muxer
        .write_video(VideoTrackPacket::new(p.track(), p.packet().clone()))
        .map_err(|error| error.map_packet(drop)),
      DemuxedPacket::Audio(p) => muxer
        .write_audio(AudioTrackPacket::new(p.track(), p.packet().clone()))
        .map_err(|error| error.map_packet(drop)),
      other => panic!("{other:?}"),
    }
    .expect("a packet");
  }
  muxer.finish_into_sink().expect("finished").1
}

/// Every packet's bytes of `packets`, one after another.
fn joined(packets: &[Seen]) -> Vec<u8> {
  packets
    .iter()
    .flat_map(|packet| packet.bytes.iter().copied())
    .collect()
}

/// **L97 — MPEG-2 video and PCM round-trip through MXF and MXF D-10, DNxHD,
/// MPEG-2 and PCM through MXF OP-Atom, and ProRes through MXF, and the
/// report says what each keeps** (table H; `mxfenc.c` 2941–3658): a video
/// packet reads back with its bytes, presented at its count of frames —
/// the times the writer replaces with each packet's count on its stream,
/// its reader counting them again ([`Rewrite::Timeline`]) — and MPEG-2's
/// key flag judged by its reader's parser ([`Rewrite::SyncFromBitstream`]);
/// PCM cut into the edit units of the video's frame rate, its bytes kept
/// in order ([`AutomaticFilter::PcmRechunk`]), MXF D-10's through its AES3
/// element; no language ([`Uncarried::Language`]); no chapter
/// ([`Uncarried::Chapter`]). At the end the packets past the last edit
/// unit every track fills — here the last of PCM's 26 units, padded with
/// silence, beside 25 frames — are dropped, and reported
/// ([`Uncarried::Trailing`]).
///
/// PLANT: the times reported kept; the trailing unit unreported, or the
/// sound counted in its packets as offered.
#[test]
fn l97_streams_round_trip_through_mxf() {
  let Some(corpus) = Corpus::new() else { return };
  let rewritten = |track: usize, rewrite| Uncarried::Rewritten {
    track: TrackIndex::new(track),
    rewrite,
  };
  let source = mxf_source(&corpus);
  let mut table = whole_table(&source);
  table[1].set_language(Some("eng".into()));
  let chapters = [chapter_of(1, 0, 500, Some("one"))];
  let target = output(&corpus, "l97.mxf");
  let report = remuxed_to_the_end(&source, &target, "mxf", table, &chapters);
  assert_eq!(
    field_rows(&report),
    vec![
      rewritten(0, Rewrite::Timeline),
      rewritten(0, Rewrite::SyncFromBitstream),
      Uncarried::Filtered {
        track: TrackIndex::new(1),
        filter: AutomaticFilter::PcmRechunk,
      },
      rewritten(1, Rewrite::Timeline),
    ]
  );
  assert_eq!(
    lines(&report, |line| matches!(
      line,
      Uncarried::Language { .. } | Uncarried::Chapter { .. } | Uncarried::Trailing { .. }
    )),
    vec![
      Uncarried::Language {
        track: TrackIndex::new(1),
        tag: "eng".into(),
      },
      Uncarried::Chapter { chapter: 0 },
      Uncarried::Trailing {
        track: TrackIndex::new(1),
        packets: 1,
      },
    ]
  );
  let offered = packets(&source);
  let back = packets(&target);
  assert_eq!(back[0].len(), offered[0].len(), "a frame a packet");
  for (index, (seen, sent)) in back[0].iter().zip(&offered[0]).enumerate() {
    assert!(seen.bytes == sent.bytes, "frame {index}: the bytes");
    assert_eq!(seen.key, sent.key, "frame {index}: the key flag");
    assert_eq!(in_ms(seen.pts), Some(40 * index as i64), "frame {index}");
  }
  // 25 edit units of 1920 samples, four bytes each: the 26th, its last 30
  // ms padded, dropped.
  assert_eq!(back[1].len(), 25, "an edit unit a packet");
  assert!(back[1].iter().all(|packet| packet.bytes.len() == 1920 * 4));
  assert!(
    joined(&back[1])[..] == joined(&offered[1])[..25 * 1920 * 4],
    "the samples, in order"
  );
  for (index, seen) in back[1].iter().enumerate() {
    assert_eq!(in_ms(seen.pts), Some(40 * index as i64), "unit {index}");
  }

  // MXF D-10: its 50 Mbit/s frames, of 250 000 bytes each, and the sound
  // through its AES3 element.
  let imx = imx_mov(&corpus);
  let table: Vec<_> = whole_table(&imx)
    .into_iter()
    .map(|row| match row.kind() {
      TrackKind::Audio => reticketed(row, |par| par.block_align = 4),
      _ => row,
    })
    .collect();
  let target = output(&corpus, "l97-d10.mxf");
  remuxed_to_the_end(&imx, &target, "mxf_d10", table, &[]);
  let offered = packets(&imx);
  let back = packets(&target);
  assert_eq!(back[0].len(), offered[0].len(), "D-10: a frame a packet");
  for (index, (seen, sent)) in back[0].iter().zip(&offered[0]).enumerate() {
    assert!(seen.bytes == sent.bytes, "D-10 frame {index}: the bytes");
  }
  let sound = joined(&back[1]);
  assert!(
    sound[..] == joined(&offered[1])[..sound.len()],
    "D-10: the samples, in order"
  );

  // OP-Atom: one track each — DNxHD, MPEG-2, mono PCM.
  let mono = generated(
    &corpus,
    "mono.wav",
    &[
      "-f",
      "lavfi",
      "-i",
      "sine=frequency=440:sample_rate=48000:duration=0.4",
      "-ac",
      "1",
      "-c:a",
      "pcm_s16le",
    ],
  );
  for (name, path, kind) in [
    ("dnxhr", dnxhr_mkv(&corpus), TrackKind::Video),
    ("mpeg2", intra_mpeg2_mkv(&corpus), TrackKind::Video),
    ("mono", mono, TrackKind::Audio),
  ] {
    let target = output(&corpus, &format!("l97-{name}.mxf"));
    let report = remuxed_to_the_end(&path, &target, "mxf_opatom", table_of(&path, kind), &[]);
    assert!(
      lines(&report, |line| matches!(line, Uncarried::Trailing { .. })).is_empty(),
      "{name}: one track, nothing trailing"
    );
    let offered = packets(&path);
    let offered = &offered[0];
    let back = packets(&target);
    if kind == TrackKind::Video {
      assert_eq!(back[0].len(), offered.len(), "{name}: a frame a packet");
      for (index, (seen, sent)) in back[0].iter().zip(offered).enumerate() {
        assert!(seen.bytes == sent.bytes, "{name} {index}: the bytes");
        assert_eq!(in_ms(seen.pts), Some(40 * index as i64), "{name} {index}");
      }
    } else {
      let sound = joined(&back[0]);
      assert!(
        sound[..] == joined(offered)[..sound.len()] && sound.len() >= joined(offered).len(),
        "{name}: the samples, in order, padded"
      );
    }
  }

  // ProRes in MXF, alone.
  let prores = prores_mkv(&corpus);
  let target = output(&corpus, "l97-prores.mxf");
  remuxed_to_the_end(&prores, &target, "mxf", whole_table(&prores), &[]);
  let offered = packets(&prores);
  let back = packets(&target);
  assert_eq!(back[0].len(), offered[0].len(), "ProRes: a frame a packet");
  for (index, (seen, sent)) in back[0].iter().zip(&offered[0]).enumerate() {
    assert!(seen.bytes == sent.bytes, "ProRes {index}: the bytes");
  }
}

/// **L98 — what the MXF muxers cannot place, or this release does not
/// admit, is refused at open, nothing written** (table H; `mxfenc.c`
/// 2941–3165): a codec but MPEG-2, DNxHD and ProRes video — MXF D-10's
/// MPEG-2 alone — and 16- and 24-bit PCM, H.264 among them, and every
/// subtitle ([`MuxOpenError::CodecNotCarried`]); a table whose first track
/// is not its one video track, OP-Atom's of two tracks, and MXF D-10's
/// audio track third ([`MuxOpenError::TrackLayout`]); a frame rate MXF
/// names no content package for, or none; no pixel format; ProRes of no
/// profile its labels name; sound but at 48 kHz, of no channel, OP-Atom's
/// but mono; MXF
/// D-10's video at another bit rate than it names, or at 24 frames a
/// second, which it labels 525-line; its sound of nine channels, or of a
/// block alignment not its channels' samples, which its writer divides a
/// packet by ([`MuxOpenError::ConfigurationInvalid`]). Each beside a table
/// that opens.
///
/// PLANT: each judgment off, its case opening or failing in FFmpeg.
#[test]
fn l98_mxf_refuses_at_open_what_it_cannot_place() {
  use ffmpeg_next::ffi::AVCodecID as Id;
  let Some(corpus) = Corpus::new() else { return };
  let open = |name: &str, container: &str, table| {
    let target = output(&corpus, name);
    let result = FfmpegOwnedMuxer::open(
      File::create(&target).expect("create"),
      ContainerName::new(container),
      table,
      &[],
      PacketLimits::default(),
    );
    match result {
      Ok((muxer, _)) => {
        drop(muxer);
        Ok(())
      }
      Err(error) => {
        assert_eq!(std::fs::metadata(&target).expect("the file").len(), 0);
        Err(error)
      }
    }
  };
  let source = mxf_source(&corpus);
  let video = || table_of(&source, TrackKind::Video).remove(0);
  let sound = || table_of(&source, TrackKind::Audio).remove(0);
  open("l98.mxf", "mxf", vec![video(), sound()]).expect("MPEG-2 and PCM");

  let h264 = table_of(&corpus.multi_track_mkv(), TrackKind::Video).remove(0);
  let aac = table_of(&corpus.multi_track_mkv(), TrackKind::Audio).remove(0);
  let ass = generated(
    &corpus,
    "ass.mkv",
    &[
      "-i",
      written_text(
        &corpus,
        "two.srt",
        "1\n00:00:00,000 --> 00:00:01,000\nhello\n\n2\n00:00:01,000 --> 00:00:02,000\nworld\n\n",
      )
      .to_str()
      .expect("utf-8 path"),
      "-c:s",
      "ass",
    ],
  );
  let caption = recoded(
    table_of(&ass, TrackKind::Subtitle).remove(0),
    Id::AV_CODEC_ID_EIA_608,
    &[],
  );
  for (name, container, table, track) in [
    ("l98-h264.mxf", "mxf", vec![h264], 0),
    ("l98-aac.mxf", "mxf", vec![video(), aac], 1),
    ("l98-608.mxf", "mxf", vec![video(), caption], 1),
    (
      "l98-dnxhr.mxf",
      "mxf_d10",
      table_of(&dnxhr_mkv(&corpus), TrackKind::Video),
      0,
    ),
  ] {
    assert!(
      matches!(
        open(name, container, table),
        Err(MuxOpenError::CodecNotCarried { track: at, .. }) if at.get() == track
      ),
      "{name}"
    );
  }
  for (name, container, table, track) in [
    ("l98-first.mxf", "mxf", vec![sound(), video()], 0),
    ("l98-two.mxf", "mxf", vec![video(), video()], 1),
    ("l98-atom.mxf", "mxf_opatom", vec![video(), video()], 1),
  ] {
    assert!(
      matches!(
        open(name, container, table),
        Err(MuxOpenError::TrackLayout { track: at }) if at.get() == track
      ),
      "{name}"
    );
  }
  let rate = |num: i32, den: i32| Some(Timebase::new(num, NonZeroI32::new(den).expect("non-zero")));
  let invalid = |name: &str, container: &str, table| {
    assert!(
      matches!(
        open(name, container, table),
        Err(MuxOpenError::ConfigurationInvalid { .. })
      ),
      "{name}"
    );
  };
  invalid("l98-15.mxf", "mxf", vec![rerated(video(), rate(15, 1))]);
  invalid("l98-none.mxf", "mxf", vec![rerated(video(), None)]);
  open(
    "l98-24.mxf",
    "mxf",
    vec![rerated(video(), rate(24_000, 1001))],
  )
  .expect("23.976");
  invalid(
    "l98-format.mxf",
    "mxf",
    vec![reticketed(video(), |par| par.format = -1)],
  );
  let prores = || table_of(&prores_mkv(&corpus), TrackKind::Video).remove(0);
  invalid(
    "l98-prores.mxf",
    "mxf",
    vec![reticketed(prores(), |par| par.profile = -99)],
  );
  open("l98-prores.mxf", "mxf", vec![prores()]).expect("ProRes 422 Proxy");
  invalid(
    "l98-44k.mxf",
    "mxf",
    vec![video(), audio_rated(&source, 44_100).remove(0)],
  );
  invalid("l98-stereo.mxf", "mxf_opatom", vec![sound()]);
  invalid(
    "l98-silent.mxf",
    "mxf",
    vec![
      video(),
      reticketed(sound(), |par| {
        // SAFETY: the layout is the parameters' own, uninitialised to no
        // channel.
        unsafe { ffmpeg_next::ffi::av_channel_layout_uninit(&mut par.ch_layout) };
        par.block_align = 0;
      }),
    ],
  );

  let imx = imx_mov(&corpus);
  let picture = || table_of(&imx, TrackKind::Video).remove(0);
  let stereo = || {
    reticketed(table_of(&imx, TrackKind::Audio).remove(0), |par| {
      par.block_align = 4
    })
  };
  open("l98-d10.mxf", "mxf_d10", vec![picture(), stereo()]).expect("D-10");
  assert!(matches!(
    open("l98-d10-third.mxf", "mxf_d10", vec![picture(), stereo(), stereo()]),
    Err(MuxOpenError::TrackLayout { track }) if track.get() == 2
  ));
  invalid(
    "l98-d10-rate.mxf",
    "mxf_d10",
    vec![reticketed(picture(), |par| par.bit_rate = 25_000_000)],
  );
  invalid(
    "l98-d10-24.mxf",
    "mxf_d10",
    vec![rerated(picture(), rate(24, 1))],
  );
  invalid(
    "l98-d10-align.mxf",
    "mxf_d10",
    vec![picture(), reticketed(stereo(), |par| par.block_align = 0)],
  );
  invalid(
    "l98-d10-nine.mxf",
    "mxf_d10",
    vec![
      picture(),
      reticketed(stereo(), |par| {
        // SAFETY: the layout is the parameters' own, uninitialised and
        // set afresh to nine unordered channels.
        unsafe {
          ffmpeg_next::ffi::av_channel_layout_uninit(&mut par.ch_layout);
          ffmpeg_next::ffi::av_channel_layout_default(&mut par.ch_layout, 9);
        }
        par.block_align = 18;
      }),
    ],
  );
}

/// **L99 — what the MXF muxers would refuse of a frame after taking it,
/// or lose of a track at the end, is refused, the packet untouched**
/// (table H; `mxfenc.c` 2361–2397, 2793–2858, 3325–3458, 3487–3606): an
/// MPEG-2 picture predicted from others, whose index offsets this release
/// does not mirror ([`PayloadRule::PredictedPicture`]); one of coding type
/// 0, and one whose sequence extension states a profile and level MXF
/// labels no codec for ([`PayloadRule::FrameHeader`]); a DNxHD stream's
/// first frame of 43 bytes, and one of a compression id its labels do not
/// name ([`PayloadRule::FrameHeader`]), a later frame of another size
/// than the first, and an MXF D-10 frame of another than its bit rate's
/// ([`PayloadRule::FrameSize`]); an empty packet ([`PayloadRule::Empty`]);
/// and at the end a track no packet reached, without which the
/// interleaving writes nothing ([`FfmpegMuxRefusal::NoFrame`]).
///
/// PLANT: each check off, its packet taken or failing in FFmpeg.
#[test]
fn l99_mxf_refuses_what_its_frame_parsers_and_interleaving_lose() {
  let Some(corpus) = Corpus::new() else { return };
  let mxf = || ContainerName::new("mxf");
  let gop = long_gop_mkv(&corpus);
  let (table, frames) = first_track(&gop, TrackKind::Video, 2);
  let mut muxer = muxer_into(&output(&corpus, "l99-gop.mxf"), mxf(), table);
  muxer
    .write_video(video_at(&video_of(&frames[0]), Some(0), 0))
    .map_err(|error| error.map_packet(drop))
    .expect("an intra picture");
  let predicted = video_at(&video_of(&frames[1]), Some(40), 40);
  let (refusal, back) = by_container(muxer.write_video(predicted.clone()));
  assert_eq!(refusal, payload(PayloadRule::PredictedPicture));
  untouched!(predicted, back);
  // The intra picture with its first picture header's coding type 0, then
  // with its sequence extension's profile Simple (5).
  let intra = video_of(&frames[0]).data().as_ref().to_vec();
  let header = intra
    .windows(4)
    .position(|window| window == [0, 0, 1, 0])
    .expect("a picture header");
  let mut untyped = intra.clone();
  untyped[header + 5] &= !0x38;
  let extension = intra
    .windows(5)
    .position(|window| window[..4] == [0, 0, 1, 0xb5] && window[4] & 0xf0 == 0x10)
    .expect("a sequence extension");
  let mut simple = intra.clone();
  simple[extension + 4] = (simple[extension + 4] & 0xf8) | 5;
  for bytes in [untyped, simple] {
    let offered = video_at(&video_holding(&video_of(&frames[0]), &bytes), Some(40), 40);
    let (refusal, back) = by_container(muxer.write_video(offered.clone()));
    assert_eq!(refusal, payload(PayloadRule::FrameHeader));
    untouched!(offered, back);
  }
  let empty = video_at(&video_holding(&video_of(&frames[0]), &[]), Some(40), 40);
  let (refusal, back) = by_container(muxer.write_video(empty.clone()));
  assert_eq!(refusal, payload(PayloadRule::Empty));
  untouched!(empty, back);
  muxer
    .write_video(video_at(&video_of(&frames[0]), Some(40), 40))
    .map_err(|error| error.map_packet(drop))
    .expect("an intra picture after");
  muxer.finish().expect("finished");

  // DNxHD: the first frame's compression id, then every frame its size.
  let (table, frames) = first_track(&dnxhr_mkv(&corpus), TrackKind::Video, 2);
  let first = video_of(&frames[0]).data().as_ref().to_vec();
  let mut muxer = muxer_into(&output(&corpus, "l99-dnxhr.mxf"), mxf(), table);
  let mut unnamed = first.clone();
  unnamed[0x2b] ^= 0x40;
  for bytes in [first[..43].to_vec(), unnamed] {
    let offered = video_at(&video_holding(&video_of(&frames[0]), &bytes), Some(0), 0);
    let (refusal, back) = by_container(muxer.write_video(offered.clone()));
    assert_eq!(refusal, payload(PayloadRule::FrameHeader));
    untouched!(offered, back);
  }
  muxer
    .write_video(video_at(&video_of(&frames[0]), Some(0), 0))
    .map_err(|error| error.map_packet(drop))
    .expect("the first frame");
  let mut longer = video_of(&frames[1]).data().as_ref().to_vec();
  longer.push(0);
  let offered = video_at(&video_holding(&video_of(&frames[1]), &longer), Some(40), 40);
  let (refusal, back) = by_container(muxer.write_video(offered.clone()));
  assert_eq!(refusal, payload(PayloadRule::FrameSize));
  untouched!(offered, back);
  muxer
    .write_video(video_at(&video_of(&frames[1]), Some(40), 40))
    .map_err(|error| error.map_packet(drop))
    .expect("a frame of the first's size");
  muxer.finish().expect("finished");

  // MXF D-10: a frame a byte short of its bit rate's.
  let imx = imx_mov(&corpus);
  let (table, frames) = first_track(&imx, TrackKind::Video, 1);
  let mut muxer = muxer_into(
    &output(&corpus, "l99-d10.mxf"),
    ContainerName::new("mxf_d10"),
    table,
  );
  let frame = video_of(&frames[0]).data().as_ref().to_vec();
  assert_eq!(frame.len(), 250_000, "the premise: a frame of 50 Mbit/s");
  let short = video_at(
    &video_holding(&video_of(&frames[0]), &frame[..frame.len() - 1]),
    Some(0),
    0,
  );
  let (refusal, back) = by_container(muxer.write_video(short.clone()));
  assert_eq!(refusal, payload(PayloadRule::FrameSize));
  untouched!(short, back);
  muxer
    .write_video(video_at(&video_of(&frames[0]), Some(0), 0))
    .map_err(|error| error.map_packet(drop))
    .expect("a frame of 250 000 bytes");
  muxer.finish().expect("finished");

  // A track no packet reached: the interleaving writes nothing.
  let source = mxf_source(&corpus);
  let (_, frames) = first_track(&source, TrackKind::Video, 1);
  let mut muxer = muxer_into(
    &output(&corpus, "l99-silent.mxf"),
    mxf(),
    whole_table(&source),
  );
  muxer
    .write_video(video_at(&video_of(&frames[0]), Some(0), 0))
    .map_err(|error| error.map_packet(drop))
    .expect("a frame");
  let fault = muxer.finish().expect_err("a track that took no frame");
  assert_eq!(fault.stage(), MuxStage::Trailer);
  assert_eq!(
    fault.refusal(),
    Some(FfmpegMuxRefusal::NoFrame {
      track: TrackIndex::new(1)
    })
  );
}

/// AAC with a PNG cover attached, in MP4.
#[rustfmt::skip]
fn aac_cover_mp4(corpus: &Corpus) -> PathBuf {
  let mp3 = corpus.cover_art_mp3();
  generated(corpus, "aac-cover.mp4", &[
    "-i", mp3.to_str().expect("utf-8 path"),
    "-map", "0:a", "-map", "0:v", "-c:a", "aac", "-c:v", "copy",
    "-disposition:v", "attached_pic",
  ])
}

/// Each chapter of `path` as its id, its title and its span in
/// milliseconds.
fn chapter_rows(path: &Path) -> Vec<(i64, Option<String>, (i64, i64))> {
  chapters(path)
    .iter()
    .map(|chapter| {
      (
        chapter.id(),
        chapter.title().map(|title| title.as_str().to_owned()),
        span_ms(chapter),
      )
    })
    .collect()
}

/// **L100 — H.264 and AAC round-trip through the MOV family's modes
/// beside MP4 — MOV, 3GP, 3G2, PSP, iPod, F4V — and the report says what
/// each keeps** (table H; `movenc.c` 1380–1560, 2780–3110, 4330–4440,
/// 5040–5110, 8129–8700): every packet reads back with its bytes, its key
/// flag and its times — F4V's moved by libavformat, its writer of no
/// negative times ([`Uncarried::TimesShifted`]); a language MOV writes
/// through the Macintosh table and the others pack; the enabled flag
/// alone of the DASH roles MP4's mode writes ([`Uncarried::Disposition`]);
/// chapters MOV's and iPod's chapter track keeps as MP4's does, 3GP's,
/// 3G2's and F4V's Nero list from their starts, its last ended at the
/// file's end ([`Uncarried::ChapterSpan`]), PSP's none
/// ([`Uncarried::Chapter`]); a cover iPod's and F4V's iTunes metadata
/// write, MOV's, 3GP's and ISMV's none ([`Uncarried::Attachment`]).
///
/// PLANT: the modes out of MP4's family; MOV's language taken packed; the
/// roles taken as MP4's; the chapters taken as MP4's in every mode; F4V's
/// move unmirrored; every mode taken to write a cover.
#[test]
fn l100_streams_round_trip_through_the_mov_familys_modes() {
  let Some(corpus) = Corpus::new() else { return };
  let source = h264_aac(&corpus, "h264-aac.mp4");
  let table = || {
    let mut table = whole_table(&source);
    table[1].set_language(Some("fra".into()));
    table[1].set_disposition(TrackDisposition::DEFAULT | TrackDisposition::COMMENT);
    table
  };
  let chapters = [
    chapter_of(0, 0, 400, Some("one")),
    chapter_of(1, 400, 1000, Some("two")),
  ];
  let offered = packets(&source);
  for container in ["mov", "3gp", "3g2", "psp", "ipod", "f4v"] {
    let target = output(&corpus, &format!("l100.{container}"));
    let report = remuxed_to_the_end(&source, &target, container, table(), &chapters);
    let lifted = container == "f4v";
    assert_eq!(
      report.uncarried().contains(&Uncarried::TimesShifted),
      lifted,
      "{container}"
    );
    // MOV's Macintosh table holds no `und`, which every other mode packs.
    let mut kept = Vec::new();
    if container == "mov" {
      kept.push(Uncarried::Language {
        track: TrackIndex::new(0),
        tag: "und".into(),
      });
    }
    kept.push(Uncarried::Disposition {
      track: TrackIndex::new(1),
      flags: TrackDisposition::COMMENT,
    });
    assert_eq!(
      lines(&report, |line| matches!(
        line,
        Uncarried::Language { .. } | Uncarried::Disposition { .. }
      )),
      kept,
      "{container}: the role MP4's mode alone writes"
    );
    let chapter_lines = lines(&report, |line| {
      matches!(
        line,
        Uncarried::Chapter { .. } | Uncarried::ChapterId { .. } | Uncarried::ChapterSpan { .. }
      )
    });
    match container {
      "mov" | "ipod" => assert_eq!(chapter_lines, vec![], "{container}"),
      "psp" => assert_eq!(
        chapter_lines,
        vec![
          Uncarried::Chapter { chapter: 0 },
          Uncarried::Chapter { chapter: 1 }
        ],
        "{container}"
      ),
      _ => assert_eq!(
        chapter_lines,
        vec![Uncarried::ChapterSpan { chapter: 1 }],
        "{container}: the last ends at the file's end"
      ),
    }
    if container != "psp" {
      let back = chapter_rows(&target);
      assert_eq!(
        back[0],
        (0, Some("one".to_owned()), (0, 400)),
        "{container}"
      );
      assert_eq!(
        (back[1].0, back[1].1.as_deref(), back[1].2.0),
        (1, Some("two"), 400),
        "{container}"
      );
    }
    let back = packets(&target);
    let lift = if lifted {
      offered
        .iter()
        .filter_map(|track| in_ms(track.first().and_then(|first| first.dts)))
        .min()
        .map_or(0, |dts| (-dts).max(0))
    } else {
      0
    };
    for track in 0..2 {
      assert_eq!(
        back[track].len(),
        offered[track].len(),
        "{container} {track}"
      );
      for (index, (seen, sent)) in back[track].iter().zip(&offered[track]).enumerate() {
        assert!(
          seen.bytes == sent.bytes,
          "{container} {track}/{index}: the bytes"
        );
        assert_eq!(
          seen.key, sent.key,
          "{container} {track}/{index}: the key flag"
        );
        assert!(
          matches!((in_ms(seen.pts), in_ms(sent.pts)), (Some(a), Some(b)) if (a - b - lift).abs() <= 1),
          "{container} {track}/{index}: presented at {:?}, offered at {:?}",
          in_ms(seen.pts),
          in_ms(sent.pts)
        );
      }
    }
    let rows: Vec<_> = FfmpegOwnedDemuxer::open(&target)
      .expect("open")
      .tracks()
      .iter()
      .map(|row| row.language().map(|tag| tag.as_str().to_owned()))
      .collect();
    assert_eq!(rows[1].as_deref(), Some("fra"), "{container}: the language");
  }
  // A cover: MP4's, iPod's and F4V's iTunes metadata write it; MOV's,
  // 3GP's, 3G2's and ISMV's none, its track not written.
  let covered = aac_cover_mp4(&corpus);
  let cover = FfmpegOwnedDemuxer::open(&covered)
    .expect("open")
    .tracks()
    .iter()
    .position(|row| row.kind() == TrackKind::Attachment)
    .expect("the premise: a cover track");
  for (container, written) in [
    ("ipod", true),
    ("f4v", true),
    ("mov", false),
    ("3gp", false),
    ("ismv", false),
  ] {
    let target = output(&corpus, &format!("l100-cover.{container}"));
    let report = remux_through_ffmpeg(&covered, ContainerName::new(container), &target);
    assert_eq!(
      report.uncarried().contains(&Uncarried::Attachment {
        track: TrackIndex::new(cover)
      }),
      !written,
      "{container}: {report:?}"
    );
  }
}

/// **L101 — H.264 and AAC round-trip through ISMV, and AV1 through AVIF,
/// and the report says what each keeps** (table H; `movenc.c` 5560–6160,
/// 6935–7445, 8129–8250, 9153–9212): ISMV fragments, its start clearing
/// the automatic filters and writing no edit list, every track's times
/// counted from its first decode time, moved by its first composition
/// offset, its reader moving the presentation times past the largest
/// negative one, so that each track's times are its own
/// ([`Rewrite::Timeline`]), AAC's start skip, its priming, dropped
/// ([`Uncarried::SideData`]) — every packet's bytes and key flag kept;
/// AVIF writes its one AV1
/// track's OBUs filtered ([`Rewrite::ObusFiltered`]), every frame read
/// back from its sequence, the first also as the file's still picture,
/// which its reader reads as a track of its own.
///
/// PLANT: ISMV's times reported kept; its start skip taken as MP4's.
#[test]
fn l101_streams_round_trip_through_ismv_and_avif() {
  let Some(corpus) = Corpus::new() else { return };
  let rewritten = |track: usize, rewrite| Uncarried::Rewritten {
    track: TrackIndex::new(track),
    rewrite,
  };
  let source = h264_aac(&corpus, "h264-aac.mp4");
  let target = output(&corpus, "l101.ismv");
  let report = remuxed_to_the_end(&source, &target, "ismv", whole_table(&source), &[]);
  assert_eq!(
    field_rows(&report),
    vec![
      rewritten(0, Rewrite::Timeline),
      rewritten(1, Rewrite::Timeline),
      // MP4's own: the AAC packets' durations, the distance to the next.
      rewritten(1, Rewrite::Durations),
    ]
  );
  // AAC's first packet's start skip, its priming, which ISMV's missing
  // edit list drops.
  assert!(
    lines(&report, |line| matches!(line, Uncarried::SideData { .. })).contains(
      &Uncarried::SideData {
        track: TrackIndex::new(1),
        kind: ffmpeg_next::ffi::AVPacketSideDataType::AV_PKT_DATA_SKIP_SAMPLES as i32,
        name: Some("Skip Samples".into()),
        fate: SideDataFate::Dropped,
      }
    ),
    "{report:?}"
  );
  let offered = packets(&source);
  let back = packets(&target);
  for track in 0..2 {
    assert_eq!(back[track].len(), offered[track].len(), "ISMV {track}");
    for (index, (seen, sent)) in back[track].iter().zip(&offered[track]).enumerate() {
      assert!(seen.bytes == sent.bytes, "ISMV {track}/{index}: the bytes");
      assert_eq!(seen.key, sent.key, "ISMV {track}/{index}: the key flag");
    }
  }

  let av1 = av1_mkv(&corpus);
  let target = output(&corpus, "l101.avif");
  let report = remuxed_to_the_end(&av1, &target, "avif", whole_table(&av1), &[]);
  assert_eq!(
    field_rows(&report),
    vec![rewritten(0, Rewrite::ObusFiltered)]
  );
  // The still picture, its primary item, and the sequence beside it, each
  // a track its reader reads.
  let back: Vec<usize> = packets(&target).iter().map(Vec::len).collect();
  assert_eq!(
    back,
    vec![1, packets(&av1)[0].len()],
    "AVIF: the picture, then every frame"
  );
}

/// **L102 — what the MOV family's modes refuse, or this release does not
/// admit, is refused at open, nothing written** (table H; `movenc.c`
/// 6431–6448, 8129–8650): a codec of a mode's tag table its paths write
/// otherwise than MP4's — MOV's MP3 and PCM, 3GP's AMR, PSP's ALAC, F4V's
/// AC-3 through its tag table, ISMV's subtitles, AVIF's every codec but
/// AV1 — and MOV's H.264 of an intra profile, written with no `avcC`
/// ([`MuxOpenError::CodecNotCarried`]); in every mode a picture past 65535
/// ([`MuxOpenError::Dimensions`]), MPEG audio layer III below 16 kHz
/// outside MOV ([`MuxOpenError::ConfigurationInvalid`]); PSP's table of
/// other than one video and one audio track, AVIF's of more than one
/// ([`MuxOpenError::TrackLayout`]); the Nero list's untitled chapter,
/// which its reader hands back titled empty, no longer untitled (R9:
/// Fable R2, judged by the ruled rule), and title past 255 bytes, which it
/// cuts ([`MuxOpenError::ChapterWithoutTitle`],
/// [`MuxOpenError::ChapterTitleTooLong`]); ISMV's AAC of no configuration,
/// its start clearing the filter MP4's makes one with
/// ([`MuxOpenError::ConfigurationMissing`]); MP4's TTML of ISMV's `dfxp`
/// tag ([`MuxOpenError::ConfigurationInvalid`]) — each beside a table
/// that opens.
///
/// PLANT: each judgment off, its case opening or failing in FFmpeg.
#[test]
fn l102_the_mov_familys_modes_refuse_at_open_what_they_write_otherwise() {
  let Some(corpus) = Corpus::new() else { return };
  let open = |name: &str, container: &str, table, chapters: &[Chapter<Ffmpeg>]| {
    let target = output(&corpus, name);
    let result = FfmpegOwnedMuxer::open(
      File::create(&target).expect("create"),
      ContainerName::new(container),
      table,
      chapters,
      PacketLimits::default(),
    );
    match result {
      Ok((muxer, _)) => {
        drop(muxer);
        Ok(())
      }
      Err(error) => {
        assert_eq!(std::fs::metadata(&target).expect("the file").len(), 0);
        Err(error)
      }
    }
  };
  let source = h264_aac(&corpus, "h264-aac.mp4");
  let video = || table_of(&source, TrackKind::Video).remove(0);
  let sound = || table_of(&source, TrackKind::Audio).remove(0);
  let mp3 = audio_file(&corpus, "l16.mp3", "libmp3lame");
  let pcm = pcm_mkv(&corpus);
  let alac = audio_file(&corpus, "alac.m4a", "alac");
  let ac3 = audio_file(&corpus, "ac3.mkv", "ac3");
  for (name, container, table, track) in [
    (
      "l102-mp3.mov",
      "mov",
      vec![table_of(&mp3, TrackKind::Audio).remove(0)],
      0,
    ),
    (
      "l102-pcm.mov",
      "mov",
      vec![table_of(&pcm, TrackKind::Audio).remove(0)],
      0,
    ),
    (
      "l102-alac.psp",
      "psp",
      vec![video(), table_of(&alac, TrackKind::Audio).remove(0)],
      1,
    ),
    (
      "l102-ac3.f4v",
      "f4v",
      vec![table_of(&ac3, TrackKind::Audio).remove(0)],
      0,
    ),
    ("l102-h264.avif", "avif", vec![video()], 0),
    (
      "l102-intra.mov",
      "mov",
      vec![reticketed(video(), |par| {
        par.profile = ffmpeg_next::ffi::AV_PROFILE_H264_HIGH_10_INTRA;
      })],
      0,
    ),
  ] {
    assert!(
      matches!(
        open(name, container, table, &[]),
        Err(MuxOpenError::CodecNotCarried { track: at, .. }) if at.get() == track
      ),
      "{name}"
    );
  }
  open("l102.mov", "mov", vec![video(), sound()], &[]).expect("MOV: H.264 and AAC");
  assert!(matches!(
    open(
      "l102-wide.mp4",
      "mp4",
      vec![reticketed(video(), |par| par.width = 65_536)],
      &[]
    ),
    Err(MuxOpenError::Dimensions { .. })
  ));
  assert!(matches!(
    open(
      "l102-mp3.mp4",
      "mp4",
      vec![audio_rated(&mp3, 8_000).remove(0)],
      &[]
    ),
    Err(MuxOpenError::ConfigurationInvalid { .. })
  ));
  open(
    "l102-mp3.mp4",
    "mp4",
    vec![audio_rated(&mp3, 16_000).remove(0)],
    &[],
  )
  .expect("16 kHz");
  for (name, container, table, track) in [
    ("l102-alone.psp", "psp", vec![video()], 0),
    ("l102-two.psp", "psp", vec![video(), sound(), video()], 2),
    (
      "l102-two.avif",
      "avif",
      {
        let av1 = av1_mkv(&corpus);
        vec![
          table_of(&av1, TrackKind::Video).remove(0),
          table_of(&av1, TrackKind::Video).remove(0),
        ]
      },
      1,
    ),
  ] {
    assert!(
      matches!(
        open(name, container, table, &[]),
        Err(MuxOpenError::TrackLayout { track: at }) if at.get() == track
      ),
      "{name}"
    );
  }
  open("l102.psp", "psp", vec![video(), sound()], &[]).expect("PSP: one video, one audio");
  let untitled = [chapter_of(0, 0, 500, None)];
  assert!(matches!(
    open("l102-untitled.3gp", "3gp", vec![video()], &untitled),
    Err(MuxOpenError::ChapterWithoutTitle { chapter: 0 })
  ));
  let titled = |bytes: usize| [chapter_of(0, 0, 500, Some(&"a".repeat(bytes)))];
  assert!(matches!(
    open("l102-title.3gp", "3gp", vec![video()], &titled(256)),
    Err(MuxOpenError::ChapterTitleTooLong { chapter: 0 })
  ));
  open("l102-title.3gp", "3gp", vec![video()], &titled(255)).expect("255 bytes");
  // ISMV clears the automatic filters: AAC with no configuration has no
  // filter to make one.
  assert!(matches!(
    open(
      "l102-bare.ismv",
      "ismv",
      vec![sound().with_extradata(Some(bytes_of(&[])))],
      &[]
    ),
    Err(MuxOpenError::ConfigurationMissing { .. })
  ));
  open(
    "l102-bare.mp4",
    "mp4",
    vec![sound().with_extradata(Some(bytes_of(&[])))],
    &[],
  )
  .expect("MP4's filter makes it of the first frame");
  // TTML of ISMV's `dfxp` tag outside ISMV.
  let ttml = || table_of(&ttml_mp4(&corpus), TrackKind::Subtitle).remove(0);
  assert!(matches!(
    open(
      "l102-dfxp.mp4",
      "mp4",
      vec![reticketed(ttml(), |par| par.codec_tag = u32::from_le_bytes(*b"dfxp"))],
      &[]
    ),
    Err(MuxOpenError::ConfigurationInvalid { .. })
  ));
  open("l102-ttml.mp4", "mp4", vec![ttml()], &[]).expect("TTML of its own tag");
  // PSP writes no chapter at all: none refused, each reported.
  open(
    "l102-untitled.psp",
    "psp",
    vec![video(), sound()],
    &untitled,
  )
  .expect("PSP");
}

/// **L103 — ISMV's and AVIF's held samples, and the Nero list's 255
/// chapters** (table H; `movenc.c` 5040–5064, 6961–6981, 9153–9212): a
/// sample that would take what ISMV or AVIF write of a track into their
/// dynamic buffer — counted from its first sample, though ISMV flushes it
/// at each video key frame — past the buffer is refused, the packet
/// untouched ([`PayloadRule::FragmentSize`]); 3GP's chapters past the
/// 255th are not written ([`Uncarried::Chapter`]).
///
/// PLANT: the bound off, the sample taken; the Nero list counted whole.
#[test]
fn l103_the_mov_familys_held_samples_and_nero_chapters() {
  let Some(corpus) = Corpus::new() else { return };
  let source = h264_aac(&corpus, "h264-aac.mp4");
  let (table, frames) = first_track(&source, TrackKind::Audio, 2);
  let first = audio_of(&frames[0]);
  let bound = first.data().as_ref().len() as u64 + 10;
  let mut muxer = muxer_with(
    &output(&corpus, "l103.ismv"),
    ContainerName::new("ismv"),
    table,
    MuxOptions::new().with_lowered_fragment_bound(bound),
  );
  muxer
    .write_audio(AudioTrackPacket::new(TrackIndex::new(0), first.clone()))
    .map_err(|error| error.map_packet(drop))
    .expect("within the bound");
  let next = AudioTrackPacket::new(TrackIndex::new(0), audio_of(&frames[1]));
  let (refusal, back) = by_container(muxer.write_audio(next.clone()));
  assert_eq!(refusal, payload(PayloadRule::FragmentSize));
  untouched!(next, back);
  muxer.finish().expect("finished");

  let av1 = av1_mkv(&corpus);
  let (table, frames) = first_track(&av1, TrackKind::Video, 2);
  let unit = video_of(&frames[0]);
  let mut muxer = muxer_with(
    &output(&corpus, "l103.avif"),
    ContainerName::new("avif"),
    table,
    MuxOptions::new().with_lowered_fragment_bound(unit.data().as_ref().len() as u64),
  );
  muxer
    .write_video(VideoTrackPacket::new(TrackIndex::new(0), unit.clone()))
    .map_err(|error| error.map_packet(drop))
    .expect("the first frame");
  let next = VideoTrackPacket::new(TrackIndex::new(0), video_of(&frames[1]));
  let (refusal, back) = by_container(muxer.write_video(next.clone()));
  assert_eq!(refusal, payload(PayloadRule::FragmentSize));
  untouched!(next, back);
  muxer.finish().expect("finished");

  let chapters: Vec<_> = (0..257)
    .map(|index| chapter_of(index, index * 2, index * 2 + 2, Some("c")))
    .collect();
  let target = output(&corpus, "l103.3gp");
  let report = remuxed_to_the_end(&source, &target, "3gp", whole_table(&source), &chapters);
  assert_eq!(
    lines(&report, |line| matches!(line, Uncarried::Chapter { .. })),
    vec![
      Uncarried::Chapter { chapter: 255 },
      Uncarried::Chapter { chapter: 256 }
    ]
  );
  assert!(
    report
      .uncarried()
      .contains(&Uncarried::ChapterSpan { chapter: 254 })
  );
  assert!(
    !report
      .uncarried()
      .contains(&Uncarried::ChapterSpan { chapter: 253 })
  );
  assert_eq!(chapter_rows(&target).len(), 255);
}

/// **L104 — WAV refuses the packet that would take its fact chunk's count
/// of samples past its 32 bits** (R9, the class; table H; `wavenc.c`
/// 347–352, 409–420, 448–462): WAV writes a fact chunk for every codec tag
/// but integer PCM's, its count of samples the presentation span with the
/// last packet's duration, cut past 32 bits at the trailer, which its
/// reader takes as the stream's length. With the bound lowered to 20 000,
/// IMA ADPCM — two samples a byte — is refused at the packet whose span,
/// its duration counted, passes it ([`PayloadRule::FileSize`]), by name,
/// untouched, its file not much more than half the bound (L78: integer
/// PCM, of no fact chunk, refused only where its file passes it).
///
/// PLANT: the count unjudged takes the packets until the file passes.
#[test]
fn l104_wav_refuses_a_fact_count_past_its_32_bits() {
  let Some(corpus) = Corpus::new() else { return };
  #[rustfmt::skip]
  let adpcm = generated(&corpus, "adpcm-ima.wav", &[
    "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=8000:duration=6",
    "-ac", "1", "-c:a", "adpcm_ima_wav",
  ]);
  let (table, frames) = whole_track(&adpcm, TrackKind::Audio);
  const BOUND: i64 = 20_000;
  // Each packet's samples, the source's packets abutting: its distance to
  // the next, the last of its predecessor's length.
  let ticks =
    |frame: &DemuxedPacket<Ffmpeg, FfmpegBytes>| audio_of(frame).pts().expect("a time").pts();
  let origin = ticks(&frames[0]);
  let lengths: Vec<i64> = (0..frames.len())
    .map(|index| match frames.get(index + 1) {
      Some(next) => ticks(next) - ticks(&frames[index]),
      None => ticks(&frames[index]) - ticks(&frames[index - 1]),
    })
    .collect();
  let expected = (0..frames.len())
    .find(|&index| ticks(&frames[index]) - origin + lengths[index] > BOUND)
    .expect("a packet past the bound");
  assert!(expected > 2, "the premise: a few packets within it");
  let options = MuxOptions::new().with_lowered_file_bound(BOUND as u64);
  let target = output(&corpus, "l104.wav");
  let mut muxer = muxer_with(&target, ContainerName::new("wav"), table, options);
  for (index, frame) in frames.iter().enumerate() {
    let packet = audio_of(frame);
    let time_base = packet.pts().expect("a time").timebase();
    let offered =
      audio_on_zero(packet.with_duration(Some(Timestamp::new(lengths[index], time_base))));
    let result = muxer.write_audio(offered.clone());
    if index < expected {
      result
        .map_err(|error| error.map_packet(drop))
        .expect("within the count");
      continue;
    }
    let (refusal, back) = by_container(result);
    assert_eq!(refusal, payload(PayloadRule::FileSize), "at packet {index}");
    untouched!(offered, back);
    break;
  }
  muxer.finish().expect("finished");
  let written = std::fs::metadata(&target).expect("the file").len();
  assert!(
    written < BOUND as u64 * 3 / 5,
    "the file at {written} bytes: the count passed first"
  );
}

/// `row` with its stream's channel layout libavutil's own for `channels`
/// — a native layout, as `av_channel_layout_default` makes it — where its
/// reader stated an unordered one.
fn relaid(
  row: TrackDescription<Ffmpeg, FfmpegBytes, CodecTicket>,
  channels: i32,
) -> TrackDescription<Ffmpeg, FfmpegBytes, CodecTicket> {
  reticketed(row, |par| {
    // SAFETY: `par` is live, its layout its own; `av_channel_layout_uninit`
    // frees any map and `av_channel_layout_default` writes a native one in
    // place.
    unsafe {
      ffmpeg_next::ffi::av_channel_layout_uninit(&mut par.ch_layout);
      ffmpeg_next::ffi::av_channel_layout_default(&mut par.ch_layout, channels);
    }
  })
}

/// **L105 — FLV refuses at open the legacy audio its flags would write
/// otherwise, or its multichannel header would end the process with** (R9,
/// the class; table H; `flvenc.c` 140–240, 910–913, 497–540; `flvdec.c`
/// 314–360, 1657–1670): 16-bit PCM at 44.1 kHz in stereo, its layout
/// libavutil's own, is taken and reads back so; the same PCM of the
/// unordered layout Matroska's reader states, and of 5.1, refused
/// ([`MuxOpenError::ChannelLayout`]) — FFmpeg would name its fourcc, of
/// which PCM has none, and assert; big-endian PCM, which reads back
/// little-endian ([`MuxOpenError::CodecNotCarried`]); PCM at 8 kHz, written
/// as the 5.5 kHz code, and G.711 µ-law at 44.1 kHz, read back at 8 kHz
/// ([`MuxOpenError::ConfigurationInvalid`]) — µ-law at 8 kHz taken. Each
/// refusal at open writes nothing.
///
/// PLANT: each check removed opens — the layout's ending the process.
#[test]
fn l105_flv_refuses_the_legacy_audio_it_would_write_otherwise() {
  let Some(corpus) = Corpus::new() else { return };
  let flv = || ContainerName::new("flv");
  #[rustfmt::skip]
  let pcm_at = |name: &str, codec: &str, rate: &str, channels: &str| generated(&corpus, name, &[
    "-f", "lavfi", "-i", &format!("sine=frequency=440:sample_rate={rate}:duration=0.3"),
    "-ac", channels, "-c:a", codec,
  ]);
  let stereo = pcm_at("pcm-44k.mkv", "pcm_s16le", "44100", "2");
  let target = output(&corpus, "l105.flv");
  let (_, frames) = whole_track(&stereo, TrackKind::Audio);
  let mut muxer = muxer_into(
    &target,
    flv(),
    vec![relaid(table_of(&stereo, TrackKind::Audio).remove(0), 2)],
  );
  for frame in &frames {
    muxer
      .write_audio(audio_on_zero(audio_of(frame)))
      .map_err(|error| error.map_packet(drop))
      .expect("stereo PCM");
  }
  muxer.finish().expect("finished");
  let back = table_of(&target, TrackKind::Audio).remove(0);
  assert_eq!(back.codec(), CodecId::PCM_S16LE);
  assert_eq!(back.side_data().sample_rate(), 44_100);
  assert_eq!(back.side_data().ch_layout().channels(), 2);
  assert_eq!(
    joined_bytes(&whole_track(&target, TrackKind::Audio).1),
    joined_bytes(&frames),
    "the samples"
  );

  let open = |name: &str, table| {
    let target = output(&corpus, name);
    let opened = FfmpegOwnedMuxer::open(
      File::create(&target).expect("create"),
      flv(),
      table,
      &[],
      PacketLimits::default(),
    )
    .map(drop);
    assert_eq!(
      std::fs::metadata(&target).expect("the file").len() == 0,
      opened.is_err(),
      "{name}: nothing written where refused"
    );
    opened
  };
  let surround = pcm_at("pcm-44k-6.mkv", "pcm_s16le", "44100", "6");
  for (name, table) in [
    ("l105-unordered.flv", table_of(&stereo, TrackKind::Audio)),
    (
      "l105-surround.flv",
      vec![relaid(table_of(&surround, TrackKind::Audio).remove(0), 6)],
    ),
  ] {
    assert!(
      matches!(open(name, table), Err(MuxOpenError::ChannelLayout { track }) if track.get() == 0),
      "{name}"
    );
  }
  let big = pcm_at("pcm-be-44k.mkv", "pcm_s16be", "44100", "2");
  assert!(matches!(
    open("l105-be.flv", vec![relaid(table_of(&big, TrackKind::Audio).remove(0), 2)]),
    Err(MuxOpenError::CodecNotCarried { codec, .. }) if codec == CodecId::PCM_S16BE
  ));
  let narrow = pcm_at("pcm-8k.mkv", "pcm_s16le", "8000", "1");
  let mulaw = pcm_at("mulaw-44k.wav", "pcm_mulaw", "44100", "1");
  for (name, source) in [("l105-8k.flv", &narrow), ("l105-mulaw.flv", &mulaw)] {
    assert!(
      matches!(
        open(name, vec![relaid(table_of(source, TrackKind::Audio).remove(0), 1)]),
        Err(MuxOpenError::ConfigurationInvalid { track, .. }) if track.get() == 0
      ),
      "{name}"
    );
  }
  let telephone = pcm_at("mulaw-8k.wav", "pcm_mulaw", "8000", "1");
  open(
    "l105-mulaw-8k.flv",
    vec![relaid(table_of(&telephone, TrackKind::Audio).remove(0), 1)],
  )
  .expect("µ-law at 8 kHz");
}

/// **L106 — FLV's Annex B rewrite is reported where the configuration as
/// of the packet makes it** (R9, the class; table H; `flvenc.c` 1250–1263,
/// 1315–1327, 1495–1501): H.264 of an Annex B configuration has its
/// packets rewritten to length prefixes, reported at open
/// ([`Rewrite::LengthPrefixed`]) and read back so; of an `avcC` one,
/// nothing at open, and a packet whose new extradata is Annex B turns the
/// rewrite on, reported there; of none, `extract_extradata` makes an
/// Annex B one of the first packet's parameter sets, reported at that
/// packet — an `avcC` the packet brings too replaced by it (R9: ruling 4c,
/// the filter's side data).
///
/// PLANT: the rewrite decided at open alone leaves the three later ones
/// unreported; the packet's own configuration taken first leaves the
/// replaced one unreported.
#[test]
fn l106_flvs_annex_b_rewrite_is_reported_where_the_configuration_makes_it() {
  use ffmpeg_next::ffi::AVPacketSideDataType as T;
  let Some(corpus) = Corpus::new() else { return };
  let flv = || ContainerName::new("flv");
  #[rustfmt::skip]
  let annexb = generated(&corpus, "annexb.h264", &[
    "-f", "lavfi", "-i", "testsrc2=size=64x48:rate=25:duration=0.4",
    "-c:v", "libx264", "-preset", "medium", "-bf", "0", "-g", "5", "-pix_fmt", "yuv420p",
  ]);
  let (table, frames) = whole_track(&annexb, TrackKind::Video);
  let configuration = stream_extradata_of(&table[0]);
  assert!(
    configuration.first() == Some(&0),
    "the premise: an Annex B configuration"
  );
  let rewritten = |report: &mediaencode_ffmpeg::MuxReport| {
    report.uncarried().contains(&Uncarried::Rewritten {
      track: TrackIndex::new(0),
      rewrite: Rewrite::LengthPrefixed,
    })
  };
  let target = output(&corpus, "l106.flv");
  let (mut muxer, report) = FfmpegOwnedMuxer::open(
    File::create(&target).expect("create"),
    flv(),
    table,
    &[],
    PacketLimits::default(),
  )
  .expect("open");
  assert!(rewritten(&report), "at open, of the configuration");
  // The raw stream's packets carry no times: each frame's at 25 a second.
  for (index, frame) in frames.iter().enumerate() {
    let at = 40 * i64::try_from(index).expect("few");
    muxer
      .write_video(video_at(&video_of(frame), Some(at), at))
      .map_err(|error| error.map_packet(drop))
      .expect("a frame");
  }
  muxer.finish().expect("finished");
  // Every packet read back a run of NAL units behind 4-byte lengths that
  // end at its end; every one offered opened on a start code.
  let prefixed = |bytes: &[u8]| {
    let mut at = 0;
    while let Some(length) = bytes.get(at..at + 4) {
      let length = u32::from_be_bytes([length[0], length[1], length[2], length[3]]);
      at += 4 + usize::try_from(length).expect("a length");
    }
    at == bytes.len()
  };
  let back = whole_track(&target, TrackKind::Video).1;
  assert_eq!(back.len(), frames.len(), "every frame");
  for (seen, sent) in back.iter().zip(&frames) {
    assert!(
      video_of(sent).data().as_ref().starts_with(&[0, 0, 0, 1])
        || video_of(sent).data().as_ref().starts_with(&[0, 0, 1]),
      "offered Annex B"
    );
    assert!(
      prefixed(video_of(seen).data().as_ref()),
      "read back length-prefixed"
    );
  }

  let avcc = stream_extradata_of(&table_of(&corpus.multi_track_mkv(), TrackKind::Video)[0]);
  assert_eq!(avcc.first(), Some(&1), "the premise: an avcC");
  for (name, stated, side) in [
    (
      "l106-switch.flv",
      Some(avcc.clone()),
      Some(configuration.clone()),
    ),
    ("l106-extracted.flv", Some(Vec::new()), None),
    // `extract_extradata`'s configuration replaces the packet's own
    // (`av_packet_add_side_data`, `libavcodec/packet.c` 197–212).
    ("l106-replaced.flv", Some(Vec::new()), Some(avcc)),
  ] {
    let row = table_of(&annexb, TrackKind::Video)
      .remove(0)
      .with_extradata(stated.map(|bytes| bytes_of(&bytes)));
    let (mut muxer, report) = FfmpegOwnedMuxer::open(
      File::create(output(&corpus, name)).expect("create"),
      flv(),
      vec![row],
      &[],
      PacketLimits::default(),
    )
    .expect("open");
    assert!(!rewritten(&report), "{name}: nothing at open");
    let mut packet = video_of(&frames[0]);
    if let Some(side) = &side {
      packet
        .extra_mut()
        .set_side_data(vec![self::side(T::AV_PKT_DATA_NEW_EXTRADATA, side)]);
    }
    muxer
      .write_video(video_at(&packet, Some(0), 0))
      .map_err(|error| error.map_packet(drop))
      .expect("the first frame");
    assert!(rewritten(muxer.report()), "{name}: at the first frame");
    muxer.finish().expect("finished");
  }
}

/// The extradata `row`'s stream will hold: the description's own, else
/// its ticket's.
fn stream_extradata_of(row: &TrackDescription<Ffmpeg, FfmpegBytes, CodecTicket>) -> Vec<u8> {
  match row.extradata() {
    Some(bytes) => bytes.as_ref().to_vec(),
    None => row.side_data().extradata().to_vec(),
  }
}

/// `count` distinct three-letter codes, from `aaa`, joined by commas.
fn codes(count: usize) -> String {
  (0..count)
    .map(|index| {
      let letter = |at: usize| char::from(b'a' + u8::try_from(at % 26).expect("a letter"));
      [letter(index / 676), letter(index / 26), letter(index)]
        .into_iter()
        .collect::<String>()
    })
    .collect::<Vec<_>>()
    .join(",")
}

/// **L107 — MPEG-TS writes a language's every code its loops write, and
/// the PMT is counted at their exact size** (R9: Codex R8 [high]; table H;
/// `mpegtsenc.c` 686–798; `mpegts.c` 2152–2283): an MPEG audio stream's
/// `eng,fra` reads back whole and is not reported; of 64 codes the 63 its
/// ISO 639 descriptor holds — 252 bytes — read back, the tag reported
/// ([`Uncarried::Language`]); four streams of 63 codes, 259 bytes each,
/// take the PMT past its section, which FFmpeg writes no PMT for, success
/// answered — refused by name at the fourth ([`MuxOpenError::StreamIds`]);
/// a visually impaired stream reads back with descriptions too, reported
/// added ([`Uncarried::DispositionAdded`]). A DVB subtitle's descriptor, 8
/// bytes a code, and a teletext's, 5, of an 8-bit length: 31 and 51 codes
/// open, 32 and 52 refused ([`MuxOpenError::LanguageTooLong`]); a DVB
/// subtitle's configuration of another size than its codes' entries, five
/// bytes a code, refused ([`MuxOpenError::ConfigurationInvalid`]), none
/// refused ([`MuxOpenError::ConfigurationMissing`]) — read back with the
/// defaults' page ids, which the decoder keeps to; one of a subtitling type
/// for the hard of hearing reads back so, reported added.
///
/// PLANT: the descriptor's codes as R8 counted them — a tag of three bytes
/// alone — reports `eng,fra` lost and opens the four; each 8-bit length
/// unjudged opens; the configuration's size unjudged opens.
#[test]
fn l107_mpeg_ts_writes_a_languages_every_code_and_counts_them() {
  use ffmpeg_next::ffi::AVCodecID as Id;
  let Some(corpus) = Corpus::new() else { return };
  let ts = || ContainerName::new("mpegts");
  let mp2 = audio_file(&corpus, "mp2.mkv", "mp2");
  let tagged = |tag: &str, disposition: TrackDisposition| {
    let mut row = table_of(&mp2, TrackKind::Audio).remove(0);
    row.set_language(Some(tag.into()));
    row.with_disposition(disposition)
  };
  let open = |name: &str, table| {
    let target = output(&corpus, name);
    let opened = FfmpegOwnedMuxer::open(
      File::create(&target).expect("create"),
      ts(),
      table,
      &[],
      PacketLimits::default(),
    );
    if opened.is_err() {
      assert_eq!(
        std::fs::metadata(&target).expect("the file").len(),
        0,
        "{name}"
      );
    }
    opened
  };
  let (_, frames) = whole_track(&mp2, TrackKind::Audio);
  for (name, tag, kept, back) in [
    (
      "l107-two.ts",
      "eng,fra".to_owned(),
      true,
      "eng,fra".to_owned(),
    ),
    ("l107-many.ts", codes(64), false, codes(63)),
  ] {
    let target = output(&corpus, name);
    let (mut muxer, report) =
      open(name, vec![tagged(&tag, TrackDisposition::empty())]).expect("open");
    assert_eq!(
      lines(&report, |line| matches!(line, Uncarried::Language { .. })).is_empty(),
      kept,
      "{name}"
    );
    for frame in &frames {
      muxer
        .write_audio(audio_on_zero(audio_of(frame)))
        .map_err(|error| error.map_packet(drop))
        .expect("a frame");
    }
    muxer.finish().expect("finished");
    assert_eq!(
      rows(&target)[0].language.as_deref(),
      Some(back.as_str()),
      "{name}"
    );
  }
  let crowded: Vec<_> = (0..4)
    .map(|_| tagged(&codes(63), TrackDisposition::empty()))
    .collect();
  assert!(matches!(
    open("l107-crowded.ts", crowded),
    Err(MuxOpenError::StreamIds { track }) if track.get() == 3
  ));
  let three: Vec<_> = (0..3)
    .map(|_| tagged(&codes(63), TrackDisposition::empty()))
    .collect();
  open("l107-crowded.ts", three)
    .expect("three of 259 bytes")
    .0
    .finish()
    .expect("finished");
  let (_, report) = open(
    "l107-described.ts",
    vec![tagged("eng", TrackDisposition::VISUAL_IMPAIRED)],
  )
  .expect("open");
  assert!(report.uncarried().contains(&Uncarried::DispositionAdded {
    track: TrackIndex::new(0),
    flags: TrackDisposition::DESCRIPTIONS,
  }));

  let ass = generated(
    &corpus,
    "ass.mkv",
    &[
      "-i",
      written_text(
        &corpus,
        "two.srt",
        "1\n00:00:00,000 --> 00:00:01,000\nhello\n\n2\n00:00:01,000 --> 00:00:02,000\nworld\n\n",
      )
      .to_str()
      .expect("utf-8 path"),
      "-c:s",
      "ass",
    ],
  );
  let subtitle = |codec, count: usize, entry: &[u8], entries: usize| {
    let mut row = recoded(
      table_of(&ass, TrackKind::Subtitle).remove(0),
      codec,
      &entry.repeat(entries),
    );
    row.set_language(Some(codes(count).into()));
    row
  };
  for (codec, entry, most) in [
    (Id::AV_CODEC_ID_DVB_SUBTITLE, &[0, 1, 0, 1, 0x10][..], 31),
    (Id::AV_CODEC_ID_DVB_TELETEXT, &[0x08, 0x00][..], 51),
  ] {
    open("l107-codes.ts", vec![subtitle(codec, most, entry, most)])
      .unwrap_or_else(|error| panic!("{codec:?}, {most} codes: {error:?}"))
      .0
      .finish()
      .expect("finished");
    assert!(
      matches!(
        open("l107-codes.ts", vec![subtitle(codec, most + 1, entry, most + 1)]),
        Err(MuxOpenError::LanguageTooLong { track }) if track.get() == 0
      ),
      "{codec:?}"
    );
  }
  let dvb = Id::AV_CODEC_ID_DVB_SUBTITLE;
  assert!(matches!(
    open(
      "l107-entries.ts",
      vec![subtitle(dvb, 2, &[0, 1, 0, 1, 0x10], 1)]
    ),
    Err(MuxOpenError::ConfigurationInvalid { .. })
  ));
  assert!(matches!(
    open(
      "l107-entries.ts",
      vec![subtitle(dvb, 1, &[0, 1, 0, 1, 0x10], 0)]
    ),
    Err(MuxOpenError::ConfigurationMissing { .. })
  ));
  let (_, report) = open(
    "l107-entries.ts",
    vec![subtitle(dvb, 1, &[0, 1, 0, 1, 0x20], 1)],
  )
  .expect("a configuration of one entry");
  assert!(report.uncarried().contains(&Uncarried::DispositionAdded {
    track: TrackIndex::new(0),
    flags: TrackDisposition::HEARING_IMPAIRED,
  }));
}

/// **L110 — ASF's languages follow the C library's search over its table's
/// tail** (R9: Fable R1; `avlanguage.c` 683–693, 748–760; `asfdec_f.c`
/// 843–853): `xho` and `yid`, whose ISO 639-1 codes `xh` and `yi` glibc's
/// and macOS's `bsearch` find in the block out of order at its end, are
/// kept and read back as themselves; `chi`, whose `zh` neither finds,
/// reads back with none, reported ([`Uncarried::Language`]).
///
/// PLANT: the search left to Rust's probes, which miss `xh` and `yi`,
/// reports `xho` and `yid` lost and hands ASF neither.
#[test]
fn l110_asfs_languages_follow_the_c_librarys_search_over_its_tail() {
  let Some(corpus) = Corpus::new() else { return };
  let source = picture_and_sound(
    &corpus,
    "wmv2-wma2.mkv",
    &["-c:v", "wmv2"],
    &["-c:a", "wmav2", "-ac", "2"],
  );
  for (name, tags, lost) in [
    ("l110.asf", ["xho", "yid"], None),
    ("l110-chi.asf", ["xho", "chi"], Some(1)),
  ] {
    let mut table = table_of(&source, TrackKind::Video);
    table.extend(table_of(&source, TrackKind::Audio));
    for (row, tag) in table.iter_mut().zip(tags) {
      row.set_language(Some(tag.into()));
    }
    let target = output(&corpus, name);
    let (mut muxer, report) = FfmpegOwnedMuxer::open(
      File::create(&target).expect("create"),
      ContainerName::new("asf"),
      table,
      &[],
      PacketLimits::default(),
    )
    .expect("open");
    assert_eq!(
      lines(&report, |line| matches!(line, Uncarried::Language { .. })),
      lost
        .map(|track| Uncarried::Language {
          track: TrackIndex::new(track),
          tag: tags[track].into(),
        })
        .into_iter()
        .collect::<Vec<_>>(),
      "{name}"
    );
    let mut demuxer = FfmpegOwnedDemuxer::open(&source).expect("open");
    while let Some(packet) = demuxer.next_packet().expect("a packet or the end") {
      match packet {
        DemuxedPacket::Video(p) => muxer
          .write_video(VideoTrackPacket::new(
            TrackIndex::new(0),
            p.packet().clone(),
          ))
          .map_err(|error| error.map_packet(drop)),
        DemuxedPacket::Audio(p) => muxer
          .write_audio(AudioTrackPacket::new(
            TrackIndex::new(1),
            p.packet().clone(),
          ))
          .map_err(|error| error.map_packet(drop)),
        other => panic!("{other:?}"),
      }
      .expect("a packet");
    }
    muxer.finish().expect("finished");
    let languages: Vec<_> = rows(&target).into_iter().map(|row| row.language).collect();
    let expected: Vec<_> = tags
      .iter()
      .enumerate()
      .map(|(track, tag)| (lost != Some(track)).then(|| (*tag).to_owned()))
      .collect();
    assert_eq!(languages, expected, "{name}");
  }
}

/// **L111 — after libavformat's first write, the MPEG program stream takes
/// a time within its preload, and F4V a time before zero, as each writes
/// it** (R9: Fable R3; table G; `mpegenc.c` 1186–1202; `movenc.c`
/// 4133–4239; `mux.c` 692–715): beside audio that holds nothing,
/// libavformat writes the video at zero once a packet 10 s on arrives, and
/// hands a later packet before zero on as it is. The program stream adds
/// its preload, half a second less that first decode time, to every time
/// it writes: audio at −200 ms is taken and reads back 200 ms before the
/// video, both moved by the preload; at −600 ms, past it, refused
/// ([`FfmpegMuxRefusal::BeforeZero`]). F4V writes negative times as MP4
/// does, through its edit list: audio at −1 s taken.
///
/// PLANT: the earliest after the first write zero refuses the audio at
/// −200 ms and at −1 s.
#[test]
fn l111_after_the_first_write_each_container_writes_its_own_earliest() {
  let Some(corpus) = Corpus::new() else { return };
  for (container, source, early, refused) in [
    (
      "mpeg",
      picture_and_sound(
        &corpus,
        "mpeg2-mp2.mkv",
        &["-c:v", "mpeg2video", "-bf", "0"],
        &["-c:a", "mp2", "-ac", "2"],
      ),
      -200,
      Some(-600),
    ),
    ("f4v", h264_aac(&corpus, "h264-aac.mp4"), -1_000, None),
  ] {
    let (_, frames) = first_track(&source, TrackKind::Video, 2);
    let (_, audio) = first_track(&source, TrackKind::Audio, 1);
    let mut table = table_of(&source, TrackKind::Video);
    table.extend(table_of(&source, TrackKind::Audio));
    let target = output(&corpus, &format!("l111.{container}"));
    let mut muxer = muxer_into(&target, ContainerName::new(container), table);
    for (frame, at) in frames.iter().zip([0, 10_001]) {
      muxer
        .write_video(video_at(&video_of(frame), Some(at), at))
        .map_err(|error| error.map_packet(drop))
        .expect("video");
    }
    let sound = |at: i64| {
      AudioTrackPacket::new(
        TrackIndex::new(1),
        audio_of(&audio[0])
          .with_pts(Some(ms(at)))
          .with_dts(Some(ms(at))),
      )
    };
    if let Some(past) = refused {
      let offered = sound(past);
      let (refusal, back) = by_container(muxer.write_audio(offered.clone()));
      assert_eq!(
        refusal,
        FfmpegMuxRefusal::BeforeZero {
          track: TrackIndex::new(1),
          timestamp: ms(past),
        },
        "{container}"
      );
      untouched!(offered, back);
    }
    muxer
      .write_audio(sound(early))
      .map_err(|error| error.map_packet(drop))
      .unwrap_or_else(|error| panic!("{container}: {error:?}"));
    muxer.finish().expect("finished");
    if container == "mpeg" {
      let seen = packets(&target);
      let first = |track: &[Seen]| in_ms(track[0].pts).expect("a time");
      assert_eq!(
        first(&seen[1]) - first(&seen[0]),
        early,
        "200 ms before the video"
      );
    }
  }
}

/// **L108 — a track of no configuration MP4 or Matroska takes its first
/// packet's new extradata for is taken where its first packet brings one,
/// and reads back configured** (R9: the coordinator's ruling 4a, judged by
/// the round trip; `movenc.c` 7412–7428; `matroskaenc.c` 3065–3087): into
/// MP4 an AAC track opens with the filters off, its first raw frame
/// bringing nothing refused ([`PayloadRule::ConfigurationAbsent`]), a
/// configuration's packet — no payload, new extradata — taken, the frames
/// after it taken and read back raw under that configuration, an ADTS
/// frame after it refused ([`PayloadRule::AdtsHeader`]); with the filters
/// on the configuration's packet inserts no filter. Matroska's AAC is
/// configured so too, a configuration libavcodec does not parse refused
/// ([`PayloadRule::NewExtradata`]). MP4's FLAC takes a first frame's
/// 34-byte STREAMINFO, and refuses another length
/// ([`PayloadRule::FlacStreamInfo`]); MP4's AV1 a first frame's `av1C`.
/// ISMV's and AVIF's, as R8 left them, stay refused at open (L102).
///
/// PLANT: the first packet unjudged takes the raw frame bringing nothing;
/// the filter named at open, as R8 had it, names it for the
/// configuration's packet.
#[test]
fn l108_a_tracks_first_packet_brings_the_configuration_where_the_container_takes_it() {
  use ffmpeg_next::ffi::AVPacketSideDataType as T;
  let Some(corpus) = Corpus::new() else { return };
  let source = corpus.multi_track_mkv();
  let (table, frames) = first_track(&source, TrackKind::Audio, 4);
  let configuration = stream_extradata_of(&table[0]);
  assert!(
    !configuration.is_empty(),
    "the premise: a configured AAC track"
  );
  let bare = || {
    table_of(&source, TrackKind::Audio)
      .into_iter()
      .map(|row| row.with_extradata(Some(bytes_of(&[]))))
      .collect::<Vec<_>>()
  };
  let bringing = |packet: &Audio, bytes: &[u8], new: &[u8]| {
    let mut packet = audio_holding(packet, bytes);
    packet
      .extra_mut()
      .set_side_data(vec![side(T::AV_PKT_DATA_NEW_EXTRADATA, new)]);
    audio_on_zero(packet)
  };
  for (container, name, on) in [
    (ContainerName::MP4, "l108-off.mp4", false),
    (ContainerName::MP4, "l108-on.mp4", true),
    (ContainerName::MATROSKA, "l108-off.mkv", false),
  ] {
    let target = output(&corpus, name);
    let (mut muxer, report) = FfmpegOwnedMuxer::open_with(
      File::create(&target).expect("create"),
      container,
      bare(),
      &[],
      filters(on),
    )
    .expect("open: the first packet brings it");
    assert!(
      lines(&report, |line| matches!(line, Uncarried::Filtered { .. })).is_empty(),
      "{name}: no filter named at open"
    );
    let first = audio_of(&frames[0]);
    let raw = audio_on_zero(first.clone());
    let (refusal, back) = by_container(muxer.write_audio(raw.clone()));
    assert_eq!(refusal, payload(PayloadRule::ConfigurationAbsent), "{name}");
    untouched!(raw, back);
    // The configuration's packet a tick before the first frame: a strict
    // container's decode times rise (`mux.c` 541–553).
    let before = {
      let at = first.dts().expect("a time");
      let earlier = Timestamp::new(at.pts() - 1, at.timebase());
      first
        .clone()
        .with_pts(Some(earlier))
        .with_dts(Some(earlier))
    };
    if name.ends_with(".mkv") {
      let unparsed = bringing(&before, &[], &[0xF8, 0x86, 0x40, 0x00]);
      let (refusal, back) = by_container(muxer.write_audio(unparsed.clone()));
      assert_eq!(refusal, payload(PayloadRule::NewExtradata), "{name}");
      untouched!(unparsed, back);
    }
    muxer
      .write_audio(bringing(&before, &[], &configuration))
      .map_err(|error| error.map_packet(drop))
      .expect("a configuration's packet");
    for frame in &frames[..3] {
      muxer
        .write_audio(audio_on_zero(audio_of(frame)))
        .map_err(|error| error.map_packet(drop))
        .expect("a raw frame under it");
    }
    assert!(
      lines(muxer.report(), |line| matches!(
        line,
        Uncarried::Filtered { .. }
      ))
      .is_empty(),
      "{name}: no filter inserted"
    );
    let framed = audio_on_zero(audio_holding(
      &audio_of(&frames[3]),
      &adts_frame_of(audio_of(&frames[3]).data().as_ref(), 2),
    ));
    let (refusal, back) = by_container(muxer.write_audio(framed.clone()));
    assert_eq!(refusal, payload(PayloadRule::AdtsHeader), "{name}");
    untouched!(framed, back);
    muxer.finish().expect("finished");
    assert_eq!(
      stream_extradata_of(&table_of(&target, TrackKind::Audio)[0]),
      configuration,
      "{name}: read back configured"
    );
    let back = &packets(&target)[0];
    assert_eq!(back.len(), 3, "{name}");
    for (seen, sent) in back.iter().zip(&frames) {
      assert!(
        seen.bytes == audio_of(sent).data().as_ref(),
        "{name}: the raw frames"
      );
    }
  }

  let flac = audio_file(&corpus, "flac.flac", "flac");
  let (table, frames) = whole_track(&flac, TrackKind::Audio);
  let streaminfo = stream_extradata_of(&table[0]);
  assert_eq!(streaminfo.len(), 34, "the premise: one STREAMINFO");
  let unconfigured = || {
    table_of(&flac, TrackKind::Audio)
      .into_iter()
      .map(|row| row.with_extradata(Some(bytes_of(&[]))))
      .collect::<Vec<_>>()
  };
  let target = output(&corpus, "l108.flac.mp4");
  let mut muxer = muxer_into(&target, ContainerName::MP4, unconfigured());
  let first = audio_of(&frames[0]);
  let short = bringing(&first, first.data().as_ref(), &streaminfo[..33]);
  let (refusal, back) = by_container(muxer.write_audio(short.clone()));
  assert_eq!(refusal, payload(PayloadRule::FlacStreamInfo));
  untouched!(short, back);
  muxer
    .write_audio(bringing(&first, first.data().as_ref(), &streaminfo))
    .map_err(|error| error.map_packet(drop))
    .expect("the first frame, bringing its STREAMINFO");
  for frame in &frames[1..] {
    muxer
      .write_audio(audio_on_zero(audio_of(frame)))
      .map_err(|error| error.map_packet(drop))
      .expect("a frame");
  }
  muxer.finish().expect("finished");
  assert_eq!(
    stream_extradata_of(&table_of(&target, TrackKind::Audio)[0]),
    streaminfo,
    "FLAC read back configured"
  );

  let av1 = av1_mkv(&corpus);
  let (table, frames) = whole_track(&av1, TrackKind::Video);
  let av1c = stream_extradata_of(&table[0]);
  let target = output(&corpus, "l108.av1.mp4");
  let unconfigured = table_of(&av1, TrackKind::Video)
    .into_iter()
    .map(|row| row.with_extradata(Some(bytes_of(&[]))))
    .collect();
  let mut muxer = muxer_into(&target, ContainerName::MP4, unconfigured);
  for (index, frame) in frames.iter().enumerate() {
    let mut packet = video_of(frame);
    if index == 0 {
      packet
        .extra_mut()
        .set_side_data(vec![side(T::AV_PKT_DATA_NEW_EXTRADATA, &av1c)]);
    }
    muxer
      .write_video(VideoTrackPacket::new(TrackIndex::new(0), packet))
      .map_err(|error| error.map_packet(drop))
      .expect("a frame, the first bringing its av1C");
  }
  muxer.finish().expect("finished");
  assert_eq!(
    stream_extradata_of(&table_of(&target, TrackKind::Video)[0]),
    av1c,
    "AV1 read back configured"
  );
}

/// **L109 — AV1's configuration is refused where `ff_isom_write_av1c`
/// would fail** (R9: ruling 4c; `av1.c` 399–502; `movenc.c` 1573–1582;
/// `flvenc.c` 892–893, 1250–1263): MP4 and FLV write it through that
/// function and ignore its failure, the `av1C` or the sequence header left
/// empty. At open, nothing written: a record of version 2, a record of
/// three bytes, two sequence headers, a sequence header of a bit to spare,
/// refused; the fixture's record and its sequence header alone taken, each
/// read back as that record. At a packet: new extradata of a record of
/// version 2 refused, the packet handed back. FLV of no configuration,
/// `extract_extradata` making it of each unit's sequence headers: a unit
/// of two refused, one whose OBUs do not parse refused as the filter
/// refuses it, the fixture's taken.
///
/// PLANT: each refusal removed opens, or takes its packet.
#[test]
fn l109_av1s_configuration_is_refused_where_ff_isom_write_av1c_would_fail() {
  use ffmpeg_next::ffi::AVPacketSideDataType as T;
  let Some(corpus) = Corpus::new() else { return };
  let av1 = av1_mkv(&corpus);
  let (table, frames) = whole_track(&av1, TrackKind::Video);
  let record = stream_extradata_of(&table[0]);
  assert_eq!(record.first(), Some(&0x81), "the premise: an av1C record");
  let sequence = record[4..].to_vec();
  assert!(
    sequence[0] == 0x0A && sequence.len() == 2 + usize::from(sequence[1]) && sequence[1] < 0x7F,
    "the premise: one sequence header after the record's four bytes"
  );
  let spare = {
    let mut obu = vec![0x0A, sequence[1] + 1];
    obu.extend_from_slice(&sequence[2..]);
    obu.push(0x80);
    obu
  };
  let flv = || ContainerName::new("flv");
  let configured = |config: &[u8]| {
    table_of(&av1, TrackKind::Video)
      .into_iter()
      .map(|row| row.with_extradata(Some(bytes_of(config))))
      .collect::<Vec<_>>()
  };
  for (container, extension) in [(ContainerName::MP4, "mp4"), (flv(), "flv")] {
    for (name, config) in [
      ("version-2", vec![0x82, 0, 0, 0]),
      ("short", record[..3].to_vec()),
      ("two", [&sequence[..], &sequence[..]].concat()),
      ("spare", spare.clone()),
    ] {
      let target = output(&corpus, &format!("l109-{name}.{extension}"));
      let refused = FfmpegOwnedMuxer::open(
        File::create(&target).expect("create"),
        container.clone(),
        configured(&config),
        &[],
        PacketLimits::default(),
      );
      assert!(
        matches!(
          refused,
          Err(MuxOpenError::ConfigurationInvalid { codec, .. }) if codec == CodecId::AV1
        ),
        "{name}.{extension}: {:?}",
        refused.map(drop)
      );
      assert_eq!(
        std::fs::metadata(&target).expect("the file").len(),
        0,
        "{name}.{extension}: nothing written"
      );
    }
    for (name, config) in [("record", &record), ("sequence", &sequence)] {
      let target = output(&corpus, &format!("l109-{name}.{extension}"));
      let mut muxer = muxer_into(&target, container.clone(), configured(config));
      let mut first = video_of(&frames[0]);
      first
        .extra_mut()
        .set_side_data(vec![side(T::AV_PKT_DATA_NEW_EXTRADATA, &[0x82, 0, 0, 0])]);
      let offered = VideoTrackPacket::new(TrackIndex::new(0), first);
      let (refusal, back) = by_container(muxer.write_video(offered.clone()));
      assert_eq!(
        refusal,
        payload(PayloadRule::NewExtradata),
        "{name}.{extension}"
      );
      untouched!(offered, back);
      for frame in &frames {
        muxer
          .write_video(VideoTrackPacket::new(TrackIndex::new(0), video_of(frame)))
          .map_err(|error| error.map_packet(drop))
          .expect("a frame");
      }
      muxer.finish().expect("finished");
      assert_eq!(
        stream_extradata_of(&table_of(&target, TrackKind::Video)[0]),
        record,
        "{name}.{extension}: read back as the record"
      );
    }
  }

  let target = output(&corpus, "l109-extracted.flv");
  let mut muxer = muxer_with(&target, flv(), configured(&[]), filters(true));
  let unit = video_of(&frames[0]);
  assert!(
    unit.data().as_ref().starts_with(&sequence),
    "the premise: each unit opens on the sequence header"
  );
  for (name, bytes, rule) in [
    (
      "two",
      [&sequence[..], unit.data().as_ref()].concat(),
      PayloadRule::NewExtradata,
    ),
    (
      "unparsed",
      [&[0x80][..], unit.data().as_ref()].concat(),
      PayloadRule::Obus,
    ),
  ] {
    let offered = VideoTrackPacket::new(TrackIndex::new(0), video_holding(&unit, &bytes));
    let (refusal, back) = by_container(muxer.write_video(offered.clone()));
    assert_eq!(refusal, payload(rule), "{name}");
    untouched!(offered, back);
  }
  for frame in &frames {
    muxer
      .write_video(VideoTrackPacket::new(TrackIndex::new(0), video_of(frame)))
      .map_err(|error| error.map_packet(drop))
      .expect("a frame, its configuration made of it");
  }
  muxer.finish().expect("finished");
}

/// **L112 — the program stream's preload is refused where its `int`
/// would wrap** (R10; `mpegenc.c` 89, 1189–1191): its first write adds
/// the half second less the first decode time, in microseconds, to an
/// `int` — on DVD whatever that time, elsewhere where it lies before the
/// half second — so that a first time about 2147 s from it wraps: refused
/// at the packet, handed back, and the next one, of a time it holds,
/// taken.
///
/// PLANT: the wrap unjudged takes the far packet.
#[test]
fn l112_the_program_streams_preload_is_refused_where_it_would_wrap() {
  let Some(corpus) = Corpus::new() else { return };
  let source = picture_and_sound(
    &corpus,
    "mpeg2-mp2.mkv",
    &["-c:v", "mpeg2video", "-bf", "0"],
    &["-c:a", "mp2", "-ac", "2"],
  );
  let (_, frames) = first_track(&source, TrackKind::Video, 2);
  for (container, far, near) in [
    ("dvd", 2_200_000, 2_000_000),
    ("mpeg", -2_200_000, -2_000_000),
  ] {
    let target = output(&corpus, &format!("l112.{container}"));
    let mut muxer = muxer_into(
      &target,
      ContainerName::new(container),
      table_of(&source, TrackKind::Video),
    );
    let offered = video_at(&video_of(&frames[0]), Some(far), far);
    let (refusal, back) = by_container(muxer.write_video(offered.clone()));
    assert_eq!(
      refusal,
      FfmpegMuxRefusal::TimeWraps {
        track: TrackIndex::new(0),
        timestamp: ms(far),
      },
      "{container}"
    );
    untouched!(offered, back);
    for (frame, at) in frames.iter().zip([near, near + 40]) {
      muxer
        .write_video(video_at(&video_of(frame), Some(at), at))
        .map_err(|error| error.map_packet(drop))
        .expect("a time the preload holds");
    }
    muxer.finish().expect("finished");
  }
}

/// **L113 — MP4's DVD subtitle configuration is taken where it reads back,
/// its rewrite reported** (R10, Codex R9 2; `movenc.c` 7976–8045, 7044–7074;
/// `dvdclut.c` 28–70; `mov.c` 11372–11389): MP4 writes the text's palette
/// as sixteen YUV colours, and its reader writes a palette text of its own
/// back — every colour as given: reported (`Rewrite::Configuration`); a
/// colour the integer conversion moves, white among them: reported
/// (`Rewrite::PaletteMoved`); a palette line of fewer than sixteen, the
/// rest written unset: refused at open, nothing written; new extradata,
/// which a new sample description holds unconverted: refused, the packet
/// handed back.
///
/// PLANT: each rule removed reports nothing, opens, or takes the packet.
#[test]
fn l113_mp4s_dvd_subtitle_configuration_is_taken_where_it_reads_back() {
  use ffmpeg_next::ffi::{AVCodecID as Id, AVPacketSideDataType as T};
  let Some(corpus) = Corpus::new() else { return };
  #[rustfmt::skip]
  let ass = generated(&corpus, "ass.mkv", &[
    "-i",
    written_text(&corpus, "two.srt", "1\n00:00:00,000 --> 00:00:01,000\nhello\n\n2\n00:00:01,000 --> 00:00:02,000\nworld\n\n")
      .to_str().expect("utf-8 path"),
    "-c:s", "ass",
  ]);
  let palette = |colours: &[&str]| format!("size: 720x480\npalette: {}\n", colours.join(", "));
  let row = |config: &str| {
    recoded(
      table_of(&ass, TrackKind::Subtitle).remove(0),
      Id::AV_CODEC_ID_DVD_SUBTITLE,
      config.as_bytes(),
    )
  };
  let rewritten = |report: &mediaencode_ffmpeg::MuxReport, rewrite| {
    report.uncarried().contains(&Uncarried::Rewritten {
      track: TrackIndex::new(0),
      rewrite,
    })
  };
  let black = palette(&["000000"; 16]);
  let white = palette(&[["000000"; 15].as_slice(), &["ffffff"]].concat());
  let short = palette(&["000000"; 15]);
  for (name, config, rewrite) in [
    ("black", &black, Rewrite::Configuration),
    ("white", &white, Rewrite::PaletteMoved),
  ] {
    let target = output(&corpus, &format!("l113-{name}.mp4"));
    let (mut muxer, report) = FfmpegOwnedMuxer::open(
      File::create(&target).expect("create"),
      ContainerName::MP4,
      vec![row(config)],
      &[],
      PacketLimits::default(),
    )
    .expect("open");
    assert!(
      rewritten(&report, rewrite),
      "{name}: {:?}",
      report.uncarried()
    );
    let (_, cues) = whole_track(&ass, TrackKind::Subtitle);
    let mut first = subtitle_of(&cues[0]);
    first
      .extra_mut()
      .set_side_data(vec![side(T::AV_PKT_DATA_NEW_EXTRADATA, black.as_bytes())]);
    let offered = SubtitleTrackPacket::new(TrackIndex::new(0), first);
    let (refusal, back) = by_container(muxer.write_subtitle(offered.clone()));
    assert_eq!(refusal, payload(PayloadRule::NewExtradata), "{name}");
    untouched!(offered, back);
    muxer
      .write_subtitle(SubtitleTrackPacket::new(
        TrackIndex::new(0),
        subtitle_of(&cues[0]),
      ))
      .map_err(|error| error.map_packet(drop))
      .expect("a cue");
    muxer.finish().expect("finished");
    // Read back: the reader's own palette text, each colour as the mirror
    // computes it — white one level down in each channel.
    let read = String::from_utf8(stream_extradata_of(
      &table_of(&target, TrackKind::Subtitle)[0],
    ))
    .expect("text");
    let last = if name == "white" { "fefefe" } else { "000000" };
    assert!(
      read.starts_with(&format!("palette: {}, {last}", ["000000"; 15].join(", "))),
      "{name}: {read:?}"
    );
  }
  let target = output(&corpus, "l113-short.mp4");
  let refused = FfmpegOwnedMuxer::open(
    File::create(&target).expect("create"),
    ContainerName::MP4,
    vec![row(&short)],
    &[],
    PacketLimits::default(),
  );
  assert!(
    matches!(refused, Err(MuxOpenError::ConfigurationInvalid { .. })),
    "{:?}",
    refused.map(drop)
  );
  assert_eq!(std::fs::metadata(&target).expect("the file").len(), 0);
}

// ---------------------------------------------------------------------------
//  The configuration writers MP4 and FLV call and ignore the answers of
//  (R10, Codex R9 [high]): one law for each condition a stream the
//  standards allow can reach, at open and as a packet's new extradata.
// ---------------------------------------------------------------------------

type Described = TrackDescription<Ffmpeg, FfmpegBytes, CodecTicket>;

/// Bits as the standards' syntax writes them.
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

/// A unit of `header` and as many bytes of 0x11 after it as make `len`.
fn sized(header: &[u8], len: usize) -> Vec<u8> {
  let mut unit = header.to_vec();
  unit.resize(len, 0x11);
  unit
}

/// `row` restated: of `codec`, stating `config`, of the codec tag `tag`
/// and the pixel format `format` where given.
fn restated(
  row: &Described,
  codec: ffmpeg_next::ffi::AVCodecID,
  config: &[u8],
  tag: Option<&[u8; 4]>,
  format: Option<i32>,
) -> Described {
  restated_profile(row, codec, config, (tag, format, None))
}

/// [`restated`], of the profile `profile` where given.
fn restated_profile(
  row: &Described,
  codec: ffmpeg_next::ffi::AVCodecID,
  config: &[u8],
  (tag, format, profile): (Option<&[u8; 4]>, Option<i32>, Option<i32>),
) -> Described {
  let index = row.side_data().stream_index();
  let mut parameters = row.side_data().rebuild().expect("the parameters");
  // SAFETY: `parameters` owns its `AVCodecParameters`, live for the
  // writes; `codec_id` is written a value of its own enum, the tag and the
  // format plain integers.
  unsafe {
    let par = parameters.as_mut_ptr();
    (*par).codec_id = codec;
    if let Some(tag) = tag {
      (*par).codec_tag = u32::from_le_bytes(*tag);
    }
    if let Some(format) = format {
      core::ptr::addr_of_mut!((*par).format).write(format);
    }
    if let Some(profile) = profile {
      (*par).profile = profile;
    }
  }
  let ticket = CodecTicket::mirror(&parameters, index, usize::MAX).expect("the ticket");
  let id = CodecId::from_raw(codec as i32);
  let params = match row.params() {
    TrackParams::Video(params) => {
      TrackParams::Video(mediaencode::mediaspine::demuxer::VideoTrackParams::new(
        id,
        params.width(),
        params.height(),
        params.pixel_format().clone(),
        params.frame_rate(),
      ))
    }
    other => other.duplicate(),
  };
  TrackDescription::new(row.timebase(), params, ticket)
    .expect("a timed track")
    .with_extradata(Some(bytes_of(config)))
    .with_language(row.language().cloned())
    .with_disposition(row.disposition())
}

/// The containers a configuration law runs in, and their extensions.
fn mp4_and_flv() -> Vec<(ContainerName, &'static str)> {
  vec![
    (ContainerName::MP4, "mp4"),
    (ContainerName::new("flv"), "flv"),
  ]
}

/// A configuration law in each of `containers`, of `cases` — each a
/// configuration, and whether the container refuses it. At open, a track
/// `track` makes stating it: refused by name, nothing written; or taken,
/// `frames` written and the file finished. As new extradata, where
/// `packets`: after a track stating `base` opens, the first frame bringing
/// it refused, handed back untouched, or taken; the frames written and the
/// file finished either way.
fn configuration_law(
  corpus: &Corpus,
  law: &str,
  containers: &[(ContainerName, &str)],
  track: &dyn Fn(&[u8]) -> Described,
  (frames, base, packets): (&[Video], &[u8], bool),
  cases: &[(&str, Vec<u8>, bool)],
) {
  use ffmpeg_next::ffi::AVPacketSideDataType as T;
  let write = |muxer: &mut FfmpegOwnedMuxer<File>, frames: &[Video], what: &str| {
    for frame in frames {
      muxer
        .write_video(VideoTrackPacket::new(TrackIndex::new(0), frame.clone()))
        .map_err(|error| error.map_packet(drop))
        .unwrap_or_else(|error| panic!("{what}: a frame: {error:?}"));
    }
  };
  for (container, extension) in containers {
    for (index, (name, config, refused)) in cases.iter().enumerate() {
      let what = format!("{law} {name}, {extension}");
      let target = output(corpus, &format!("{law}-{index}.{extension}"));
      let opened = FfmpegOwnedMuxer::open(
        File::create(&target).expect("create"),
        container.clone(),
        vec![track(config)],
        &[],
        PacketLimits::default(),
      );
      if *refused {
        assert!(
          matches!(opened, Err(MuxOpenError::ConfigurationInvalid { .. })),
          "{what}: {:?}",
          opened.map(drop)
        );
        assert_eq!(
          std::fs::metadata(&target).expect("the file").len(),
          0,
          "{what}: nothing written"
        );
      } else {
        let (mut muxer, _) = opened.unwrap_or_else(|error| panic!("{what}: {error:?}"));
        write(&mut muxer, frames, &what);
        muxer
          .finish()
          .unwrap_or_else(|error| panic!("{what}: {error:?}"));
      }
      if !packets {
        continue;
      }
      let target = output(corpus, &format!("{law}-{index}-new.{extension}"));
      let mut muxer = muxer_into(&target, container.clone(), vec![track(base)]);
      let mut first = frames[0].clone();
      first
        .extra_mut()
        .set_side_data(vec![side(T::AV_PKT_DATA_NEW_EXTRADATA, config)]);
      let offered = VideoTrackPacket::new(TrackIndex::new(0), first);
      if *refused {
        let (refusal, back) = by_container(muxer.write_video(offered.clone()));
        assert_eq!(refusal, payload(PayloadRule::NewExtradata), "{what}, new");
        untouched!(offered, back);
        write(&mut muxer, frames, &what);
      } else {
        muxer
          .write_video(offered)
          .map_err(|error| error.map_packet(drop))
          .unwrap_or_else(|error| panic!("{what}, new: {error:?}"));
        write(&mut muxer, &frames[1..], &what);
      }
      muxer
        .finish()
        .unwrap_or_else(|error| panic!("{what}, new: {error:?}"));
    }
  }
}

/// The H.264 laws' fixture: the corpus's H.264 track, its first three
/// frames rewritten from length prefixes to start codes, and its SPS and
/// PPS read off its `avcC`.
fn h264_fixture(corpus: &Corpus) -> (Described, Vec<Video>, Vec<u8>, Vec<u8>) {
  let (mut table, packets) = first_track(&corpus.multi_track_mkv(), TrackKind::Video, 3);
  let row = table.remove(0);
  let record = stream_extradata_of(&row);
  assert_eq!(
    (record[0], record[5] & 0x1F),
    (1, 1),
    "the premise: an avcC of one SPS"
  );
  let sps_len = usize::from(u16::from_be_bytes([record[6], record[7]]));
  let sps = record[8..8 + sps_len].to_vec();
  let at = 8 + sps_len;
  assert_eq!(record[at], 1, "the premise: one PPS");
  let pps_len = usize::from(u16::from_be_bytes([record[at + 1], record[at + 2]]));
  let pps = record[at + 3..at + 3 + pps_len].to_vec();
  let frames = packets
    .iter()
    .map(|packet| {
      let frame = video_of(packet);
      let mut bytes = Vec::new();
      let mut rest = frame.data().as_ref();
      while rest.len() >= 4 {
        let len = u32::from_be_bytes([rest[0], rest[1], rest[2], rest[3]]) as usize;
        bytes.extend_from_slice(&[0, 0, 0, 1]);
        bytes.extend_from_slice(&rest[4..4 + len]);
        rest = &rest[4 + len..];
      }
      video_holding(&frame, &bytes)
    })
    .collect();
  (row, frames, sps, pps)
}

/// An H.264 law: `cases` against `ff_isom_write_avcc`, in MP4 and FLV, at
/// open and as new extradata after a track stating one SPS and one PPS.
fn avcc_law(law: &str, cases: &dyn Fn(&[u8], &[u8]) -> Vec<(&'static str, Vec<u8>, bool)>) {
  avcc_law_of(law, true, cases);
}

/// [`avcc_law`], on the packet road where `packets`.
fn avcc_law_of(
  law: &str,
  packets: bool,
  cases: &dyn Fn(&[u8], &[u8]) -> Vec<(&'static str, Vec<u8>, bool)>,
) {
  let Some(corpus) = Corpus::new() else { return };
  let (row, frames, sps, pps) = h264_fixture(&corpus);
  let base = start_coded(&[&sps, &pps]);
  let track = |config: &[u8]| {
    restated(
      &row,
      ffmpeg_next::ffi::AVCodecID::AV_CODEC_ID_H264,
      config,
      None,
      None,
    )
  };
  configuration_law(
    &corpus,
    law,
    &mp4_and_flv(),
    &track,
    (&frames, &base, packets),
    &cases(&sps, &pps),
  );
}

/// `count` copies of `unit`.
fn copies(unit: &[u8], count: usize) -> Vec<&[u8]> {
  vec![unit; count]
}

/// **L114a — an H.264 configuration of fewer than 7 bytes is refused**
/// (`avc.c` 39–40; `movenc.c` 1584–1593; `flvenc.c` 897): `ff_isom_write_avcc`
/// refuses it, of either form, and MP4 and FLV, ignoring the answer, leave
/// the `avcC` or the sequence header empty — at open, nothing written, and
/// as new extradata, the packet handed back; an `avcC` of 7 bytes, written
/// as it is, taken. MP4's packet road refuses an `avcC` after an Annex B
/// configuration for its form, beside.
///
/// PLANT: the length unjudged opens, and takes FLV's packet.
#[test]
fn l114a_an_h264_configuration_of_fewer_than_7_bytes_is_refused() {
  avcc_law_of("l114a-avcc", false, &|sps, _| {
    vec![
      (
        "an avcC of six bytes",
        vec![1, sps[1], 0, 0x1F, 0xFF, 0xE1],
        true,
      ),
      (
        "an avcC of seven bytes",
        vec![1, sps[1], 0, 0x1F, 0xFF, 0xE1, 0],
        false,
      ),
    ]
  });
  avcc_law("l114a", &|sps, pps| {
    vec![
      (
        "six bytes of a start code",
        vec![0, 0, 0, 1, 0x67, sps[1]],
        true,
      ),
      ("an SPS and a PPS", start_coded(&[sps, pps]), false),
    ]
  });
}

/// **L114b — a 32nd SPS is refused** (`avc.c` 75–78): H.264 numbers 32
/// sequence parameter sets, and `ff_isom_write_avcc` refuses the 32nd of
/// an Annex B configuration; 31 taken.
///
/// PLANT: the count unjudged opens, and takes the packet.
#[test]
fn l114b_an_h264_configuration_of_32_spss_is_refused() {
  avcc_law("l114b", &|sps, pps| {
    vec![
      (
        "32 SPSs",
        start_coded(&[copies(sps, 32), vec![pps]].concat()),
        true,
      ),
      (
        "31 SPSs",
        start_coded(&[copies(sps, 31), vec![pps]].concat()),
        false,
      ),
    ]
  });
}

/// **L114c — a 256th PPS is refused** (`avc.c` 83–86): H.264 numbers 256
/// picture parameter sets; 255 taken.
///
/// PLANT: the count unjudged opens, and takes the packet.
#[test]
fn l114c_an_h264_configuration_of_256_ppss_is_refused() {
  avcc_law("l114c", &|sps, pps| {
    vec![
      (
        "256 PPSs",
        start_coded(&[vec![sps], copies(pps, 256)].concat()),
        true,
      ),
      (
        "255 PPSs",
        start_coded(&[vec![sps], copies(pps, 255)].concat()),
        false,
      ),
    ]
  });
}

/// **L114d — a 256th SPS extension is refused** (`avc.c` 91–94); 255
/// taken.
///
/// PLANT: the count unjudged opens, and takes the packet.
#[test]
fn l114d_an_h264_configuration_of_256_sps_extensions_is_refused() {
  avcc_law("l114d", &|sps, pps| {
    let extension: &[u8] = &[0x6D, 0x80];
    vec![
      (
        "256 extensions",
        start_coded(&[vec![sps, pps], copies(extension, 256)].concat()),
        true,
      ),
      (
        "255 extensions",
        start_coded(&[vec![sps, pps], copies(extension, 255)].concat()),
        false,
      ),
    ]
  });
}

/// **L114e — an SPS past 65 535 bytes is refused** (`avc.c` 75–78): its
/// length is written in 16 bits, and `ff_isom_write_avcc` refuses it; one
/// of 65 535 taken.
///
/// PLANT: the length unjudged opens, and takes the packet.
#[test]
fn l114e_an_h264_sps_past_65_535_bytes_is_refused() {
  avcc_law("l114e", &|sps, pps| {
    vec![
      (
        "an SPS of 65 536 bytes",
        start_coded(&[&sized(&sps[..4], 65_536), pps]),
        true,
      ),
      (
        "an SPS of 65 535 bytes",
        start_coded(&[&sized(&sps[..4], 65_535), pps]),
        false,
      ),
    ]
  });
}

/// **L114f — a PPS past 65 535 bytes is refused** (`avc.c` 83–86).
///
/// PLANT: the length unjudged opens, and takes the packet.
#[test]
fn l114f_an_h264_pps_past_65_535_bytes_is_refused() {
  avcc_law("l114f", &|sps, pps| {
    vec![
      (
        "a PPS of 65 536 bytes",
        start_coded(&[sps, &sized(&pps[..1], 65_536)]),
        true,
      ),
      (
        "a PPS of 65 535 bytes",
        start_coded(&[sps, &sized(&pps[..1], 65_535)]),
        false,
      ),
    ]
  });
}

/// **L114g — an SPS extension past 65 535 bytes is refused** (`avc.c`
/// 91–94).
///
/// PLANT: the length unjudged opens, and takes the packet.
#[test]
fn l114g_an_h264_sps_extension_past_65_535_bytes_is_refused() {
  avcc_law("l114g", &|sps, pps| {
    vec![
      (
        "an extension of 65 536 bytes",
        start_coded(&[sps, pps, &sized(&[0x6D], 65_536)]),
        true,
      ),
      (
        "an extension of 65 535 bytes",
        start_coded(&[sps, pps, &sized(&[0x6D], 65_535)]),
        false,
      ),
    ]
  });
}

/// **L114h — an H.264 configuration of no SPS is refused** (`avc.c`
/// 106–109): the SPSs with their lengths fewer than 6 bytes — none, or
/// one of three; one of four taken.
///
/// PLANT: the SPSs unjudged opens, and takes the packet.
#[test]
fn l114h_an_h264_configuration_of_no_sps_is_refused() {
  avcc_law("l114h", &|sps, pps| {
    vec![
      ("no SPS", start_coded(&[pps, pps]), true),
      (
        "an SPS of three bytes",
        start_coded(&[&sps[..3], pps]),
        true,
      ),
      (
        "an SPS of four bytes",
        start_coded(&[&sps[..4], pps]),
        false,
      ),
    ]
  });
}

/// **L114i — an H.264 configuration of no PPS is refused** (`avc.c`
/// 106–109): its PPS may come in band, and `ff_isom_write_avcc` refuses
/// the configuration.
///
/// PLANT: the PPS unjudged opens, and takes the packet.
#[test]
fn l114i_an_h264_configuration_of_no_pps_is_refused() {
  avcc_law("l114i", &|sps, pps| {
    vec![
      ("no PPS", start_coded(&[sps, sps]), true),
      ("an SPS and a PPS", start_coded(&[sps, pps]), false),
    ]
  });
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
    .ue(0)
    .u(0, 3)
    .ue(0)
    .ue(0)
    .u(0, 10);
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

/// A video track recoded to `codec`, of `tag` where given, of the corpus's
/// H.264 track; its three frames holding `frame` instead, in their times.
fn recoded_fixture(corpus: &Corpus, frame: &[u8]) -> (Described, Vec<Video>) {
  let (row, frames, ..) = h264_fixture(corpus);
  let frames = frames
    .iter()
    .map(|video| video_holding(video, frame))
    .collect();
  (row, frames)
}

/// A configuration law over the corpus's H.264 track recoded to `codec`,
/// of the codec tag `tag` where given, its frames holding `frame`.
fn recoded_law(
  corpus: &Corpus,
  law: &str,
  containers: &[(ContainerName, &str)],
  (codec, tag, frame): (ffmpeg_next::ffi::AVCodecID, Option<&[u8; 4]>, &[u8]),
  (base, packets): (&[u8], bool),
  cases: &[(&str, Vec<u8>, bool)],
) {
  let (row, frames) = recoded_fixture(corpus, frame);
  let track = |config: &[u8]| restated(&row, codec, config, tag, None);
  configuration_law(
    corpus,
    law,
    containers,
    &track,
    (&frames, base, packets),
    cases,
  );
}

/// An HEVC law: `cases` against `ff_isom_write_hvcc` in `containers`, of
/// the tag `tag` where given, after a track stating `base` on the packet
/// road where `packets`.
fn hvcc_law(
  law: &str,
  containers: &[(ContainerName, &str)],
  tag: Option<&[u8; 4]>,
  (base, packets): (&[u8], bool),
  cases: &[(&str, Vec<u8>, bool)],
) {
  let Some(corpus) = Corpus::new() else { return };
  let slice = start_coded(&[&escaped(&hevc_header(19, 0), &[0xAF, 0x88, 0x42])]);
  recoded_law(
    &corpus,
    law,
    containers,
    (ffmpeg_next::ffi::AVCodecID::AV_CODEC_ID_HEVC, tag, &slice),
    (base, packets),
    cases,
  );
}

/// The HEVC laws' parameter sets: a VPS, an SPS and a PPS, Annex B.
fn hevc_base() -> Vec<u8> {
  start_coded(&[&hevc_vps(0, None), &hevc_sps(0, 0, 1, None), &hevc_pps()])
}

/// The HEVC laws' record of `vps`, `sps` and `pps` units.
fn hevc_record(vps: usize, sps: usize, pps: usize) -> Vec<u8> {
  hvcc_record(&[
    (32, vec![hevc_vps(0, None); vps]),
    (33, vec![hevc_sps(0, 0, 1, None); sps]),
    (34, vec![hevc_pps(); pps]),
  ])
}

fn matroska() -> Vec<(ContainerName, &'static str)> {
  vec![(ContainerName::MATROSKA, "mkv")]
}

/// **L115a — an HEVC configuration of fewer than 6 bytes is refused**
/// (`hevc.c` 1294–1296; `movenc.c` 1639–1652; `flvenc.c` 889); one of
/// its parameter sets taken.
///
/// PLANT: the length unjudged opens, and takes FLV's packet.
#[test]
fn l115a_an_hevc_configuration_of_fewer_than_6_bytes_is_refused() {
  hvcc_law(
    "l115a",
    &mp4_and_flv(),
    None,
    (&hevc_base(), true),
    &[
      ("five bytes", vec![0, 0, 0, 1, 0x40], true),
      ("its parameter sets", hevc_base(), false),
    ],
  );
}

/// **L115b — an `hvcC` of fewer than 23 bytes is refused** (`hevc.c`
/// 1302–1303): its fixed fields alone take 23.
///
/// PLANT: the length unjudged opens, and takes the packet.
#[test]
fn l115b_an_hvcc_of_fewer_than_23_bytes_is_refused() {
  let record = hevc_record(1, 1, 1);
  hvcc_law(
    "l115b",
    &mp4_and_flv(),
    None,
    (&record, true),
    &[
      ("22 bytes", record[..22].to_vec(), true),
      ("the record", record.clone(), false),
    ],
  );
}

/// **L115c — an `hvcC` whose unit runs past it is refused**, in Matroska
/// too (`hevc.c` 1339–1341): `ff_isom_write_hvcc` answers success having
/// written nothing, and Matroska writes the track of no configuration.
///
/// PLANT: the record's walk unjudged opens, and takes the packet.
#[test]
fn l115c_an_hvcc_whose_unit_runs_past_it_is_refused_in_matroska_too() {
  let record = hevc_record(1, 1, 1);
  let cases = [
    (
      "cut a byte short",
      record[..record.len() - 1].to_vec(),
      true,
    ),
    ("the record", record.clone(), false),
  ];
  hvcc_law("l115c", &mp4_and_flv(), None, (&record, true), &cases);
  hvcc_law("l115c-mkv", &matroska(), None, (&record, false), &cases);
}

/// **L115d — an `hvcC` array of 65 536 units is refused** (`hevc.c`
/// 875–876): the record counts each in 16 bits; 65 535 taken.
///
/// PLANT: the count unjudged opens, and takes the packet.
#[test]
fn l115d_an_hvcc_array_of_65_536_units_is_refused() {
  let seis = |count: usize| {
    hvcc_record(&[
      (32, vec![hevc_vps(0, None)]),
      (33, vec![hevc_sps(0, 0, 1, None)]),
      (34, vec![hevc_pps()]),
      (39, vec![Vec::new(); count.min(65_535)]),
      (39, vec![Vec::new(); count.saturating_sub(65_535)]),
    ])
  };
  hvcc_law(
    "l115d",
    &mp4_and_flv(),
    None,
    (&hevc_record(1, 1, 1), true),
    &[
      ("65 536 SEIs", seis(65_536), true),
      ("65 535 SEIs", seis(65_535), false),
    ],
  );
}

/// **L115e — MP4's `hvc1` refuses a configuration of no VPS, or of more
/// than 16** (`hevc.c` 1146–1149; `movenc.c` 1645): its arrays are
/// complete, and the writer holds them to it; FLV's are not, and take
/// none.
///
/// PLANT: the completeness unjudged opens, and takes the packet.
#[test]
fn l115e_mp4s_hvc1_refuses_a_configuration_of_no_vps_or_17() {
  let cases = |refused: bool| {
    vec![
      ("no VPS", hevc_record(0, 1, 1), refused),
      ("17 VPSs", hevc_record(17, 1, 1), refused),
      ("16 VPSs", hevc_record(16, 1, 1), false),
    ]
  };
  let mp4 = vec![(ContainerName::MP4, "mp4")];
  hvcc_law(
    "l115e",
    &mp4,
    Some(b"hvc1"),
    (&hevc_record(1, 1, 1), true),
    &cases(true),
  );
  hvcc_law(
    "l115e-hev1",
    &mp4,
    Some(b"hev1"),
    (&hevc_record(1, 1, 1), true),
    &cases(false),
  );
  let flv = vec![(ContainerName::new("flv"), "flv")];
  hvcc_law(
    "l115e-flv",
    &flv,
    None,
    (&hevc_record(1, 1, 1), true),
    &cases(false),
  );
}

/// **L115f — MP4's `hvc1` refuses a configuration of no SPS or PPS, or of
/// more than 16 SPSs or 64 PPSs** (`hevc.c` 1150–1153).
///
/// PLANT: the completeness unjudged opens, and takes the packet.
#[test]
fn l115f_mp4s_hvc1_refuses_no_sps_or_pps_or_too_many() {
  hvcc_law(
    "l115f",
    &[(ContainerName::MP4, "mp4")],
    Some(b"hvc1"),
    (&hevc_record(1, 1, 1), true),
    &[
      ("no SPS", hevc_record(1, 0, 1), true),
      ("17 SPSs", hevc_record(1, 17, 1), true),
      ("16 SPSs", hevc_record(1, 16, 1), false),
      ("no PPS", hevc_record(1, 1, 0), true),
      ("65 PPSs", hevc_record(1, 1, 65), true),
      ("64 PPSs", hevc_record(1, 1, 64), false),
    ],
  );
}

/// **L115g — an HEVC configuration of neither form is refused** (`hevc.c`
/// 1347–1350): neither an `hvcC` of version 1 nor Annex B.
///
/// PLANT: the form unjudged opens, and takes the packet.
#[test]
fn l115g_an_hevc_configuration_of_neither_form_is_refused() {
  let record = hevc_record(1, 1, 1);
  let mut other = record.clone();
  other[0] = 2;
  hvcc_law(
    "l115g",
    &mp4_and_flv(),
    None,
    (&record, true),
    &[
      ("of version 2", other, true),
      ("the record", record.clone(), false),
    ],
  );
}

/// **L115h — an HEVC unit past 65 535 bytes is refused**, in Matroska too
/// (`hevc.c` 879–881, 1187–1197): written with its length cut to 16 bits,
/// success answered; one of 65 535 taken.
///
/// PLANT: the length unjudged opens, and takes the packet.
#[test]
fn l115h_an_hevc_unit_past_65_535_bytes_is_refused_in_matroska_too() {
  let with = |len: usize| {
    start_coded(&[
      &hevc_vps(0, None),
      &hevc_sps(0, 0, 1, None),
      &hevc_pps(),
      &sized(&hevc_header(39, 0), len),
    ])
  };
  let cases = [
    ("an SEI of 65 536 bytes", with(65_536), true),
    ("an SEI of 65 535 bytes", with(65_535), false),
  ];
  hvcc_law("l115h", &mp4_and_flv(), None, (&hevc_base(), true), &cases);
  hvcc_law(
    "l115h-mkv",
    &matroska(),
    None,
    (&hevc_base(), false),
    &cases,
  );
}

/// **L115i — an alpha layer's SPS whose VPS the configuration holds not
/// earlier is refused** (`hevc.c` 630–643, 418–440): a VPS extension
/// naming an auxiliary alpha layer keeps that layer's parameter sets, and
/// a multilayer SPS of a VPS the record holds not is refused.
///
/// PLANT: the VPS's lookup unjudged opens, and takes the packet.
#[test]
fn l115i_an_alpha_layers_sps_of_no_vps_is_refused() {
  let with = |vps: u8| {
    start_coded(&[
      &hevc_vps(0, Some(1)),
      &hevc_sps(0, 0, 1, None),
      &hevc_sps(1, vps, 0, None),
      &hevc_pps(),
    ])
  };
  hvcc_law(
    "l115i",
    &mp4_and_flv(),
    None,
    (&hevc_base(), true),
    &[("of VPS 1", with(1), true), ("of VPS 0", with(0), false)],
  );
}

/// **L115j — an HEVC SPS of 32 long-term reference pictures is refused**
/// (`hevc.c` 724–733): the standard allows 32, libavcodec reads 32
/// (`hevc/ps.c` 1522–1528), and `ff_isom_write_hvcc` refuses more than 31;
/// 31 taken.
///
/// PLANT: the count unjudged opens, and takes the packet.
#[test]
fn l115j_an_hevc_sps_of_32_long_term_pictures_is_refused() {
  let with = |count: u32| {
    start_coded(&[
      &hevc_vps(0, None),
      &hevc_sps(0, 0, 1, Some(count)),
      &hevc_pps(),
    ])
  };
  hvcc_law(
    "l115j",
    &mp4_and_flv(),
    None,
    (&hevc_base(), true),
    &[("32", with(32), true), ("31", with(31), false)],
  );
}

/// **L122 — the trust model's boundary: an HEVC SPS of 65 short-term
/// reference picture sets is taken** (`hevc.c` 714–716): the standard
/// caps them at 64, and `ff_isom_write_hvcc` refuses more, an SPS no
/// stream the standards allow holds — the parse stops there, unrefused,
/// as the module's documentation states the class.
///
/// PLANT: the trust model's stop refused refuses it at open.
#[test]
fn l122_an_hevc_sps_past_the_standards_cap_rests_on_the_trust_model() {
  hvcc_law(
    "l122",
    &mp4_and_flv(),
    None,
    (&hevc_base(), true),
    &[(
      "65 sets",
      start_coded(&[&hevc_vps(0, None), &hevc_sps(0, 0, 65, None), &hevc_pps()]),
      false,
    )],
  );
}

/// A VVC NAL unit of `kind`, of a byte past its header.
fn vvc_unit(kind: u8) -> Vec<u8> {
  vec![0x00, (kind << 3) | 1, 0x80]
}

/// A VVC law: `cases` against `ff_isom_write_vvcc` in `containers`.
fn vvcc_law(
  law: &str,
  containers: &[(ContainerName, &str)],
  (base, packets): (&[u8], bool),
  cases: &[(&str, Vec<u8>, bool)],
) {
  let Some(corpus) = Corpus::new() else { return };
  let frame = start_coded(&[&vvc_unit(8)]);
  recoded_law(
    &corpus,
    law,
    containers,
    (ffmpeg_next::ffi::AVCodecID::AV_CODEC_ID_VVC, None, &frame),
    (base, packets),
    cases,
  );
}

/// The VVC laws' Annex B configuration: `vps` VPSs, `sps` SPSs, `pps`
/// PPSs.
fn vvc_sets(vps: usize, sps: usize, pps: usize) -> Vec<u8> {
  let (v, s, p) = (vvc_unit(14), vvc_unit(15), vvc_unit(16));
  let mut units = copies(&v, vps);
  units.extend(copies(&s, sps));
  units.extend(copies(&p, pps));
  start_coded(&units)
}

/// **L116a — a VVC configuration of fewer than 6 bytes is refused**
/// (`vvc.c` 875–877; `movenc.c` 1706–1723; `flvenc.c` 891).
///
/// PLANT: the length unjudged opens, and takes FLV's packet.
#[test]
fn l116a_a_vvc_configuration_of_fewer_than_6_bytes_is_refused() {
  vvcc_law(
    "l116a-vvcc",
    &mp4_and_flv(),
    (&vvc_sets(0, 1, 1), false),
    &[
      ("a vvcC of five bytes", vec![0xFF, 0, 0, 0, 0], true),
      ("a vvcC of six bytes", vec![0xFF, 0, 0, 0, 0, 0], false),
    ],
  );
  vvcc_law(
    "l116a",
    &mp4_and_flv(),
    (&vvc_sets(0, 1, 1), true),
    &[
      ("five bytes of a start code", vec![0, 0, 0, 1, 0], true),
      ("an SPS and a PPS", vvc_sets(0, 1, 1), false),
    ],
  );
}

/// **L116b — a VVC configuration of neither form is refused** (`vvc.c`
/// 878–885): neither a `vvcC`, its first five bits set, nor Annex B.
///
/// PLANT: the form unjudged opens, and takes the packet.
#[test]
fn l116b_a_vvc_configuration_of_neither_form_is_refused() {
  let record = vec![0xFF, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
  vvcc_law(
    "l116b",
    &mp4_and_flv(),
    (&record, true),
    &[
      (
        "a byte before its sets",
        [&[0x02][..], &vvc_sets(0, 1, 1)].concat(),
        true,
      ),
      ("a record", record.clone(), false),
    ],
  );
}

/// **L116c — a VVC configuration of more than 16 VPSs is refused**
/// (`vvc.c` 680–681).
///
/// PLANT: the count unjudged opens, and takes the packet.
#[test]
fn l116c_a_vvc_configuration_of_17_vpss_is_refused() {
  vvcc_law(
    "l116c",
    &mp4_and_flv(),
    (&vvc_sets(0, 1, 1), true),
    &[
      ("17 VPSs", vvc_sets(17, 1, 1), true),
      ("16 VPSs", vvc_sets(16, 1, 1), false),
    ],
  );
}

/// **L116d — a VVC configuration of no SPS, or of more than 16, is
/// refused** (`vvc.c` 682–683).
///
/// PLANT: the count unjudged opens, and takes the packet.
#[test]
fn l116d_a_vvc_configuration_of_no_sps_or_17_is_refused() {
  vvcc_law(
    "l116d",
    &mp4_and_flv(),
    (&vvc_sets(0, 1, 1), true),
    &[
      ("no SPS", vvc_sets(1, 0, 1), true),
      ("17 SPSs", vvc_sets(0, 17, 1), true),
      ("16 SPSs", vvc_sets(0, 16, 1), false),
    ],
  );
}

/// **L116e — a VVC configuration of no PPS, or of more than 64, is
/// refused** (`vvc.c` 684–685).
///
/// PLANT: the count unjudged opens, and takes the packet.
#[test]
fn l116e_a_vvc_configuration_of_no_pps_or_65_is_refused() {
  vvcc_law(
    "l116e",
    &mp4_and_flv(),
    (&vvc_sets(0, 1, 1), true),
    &[
      ("no PPS", vvc_sets(0, 1, 0), true),
      ("65 PPSs", vvc_sets(0, 1, 65), true),
      ("64 PPSs", vvc_sets(0, 1, 64), false),
    ],
  );
}

/// **L116f — a VVC unit past 65 535 bytes is refused**, in Matroska too
/// (`vvc.c` 471, 779–785): written with its length cut to 16 bits,
/// success answered.
///
/// PLANT: the length unjudged opens, and takes the packet.
#[test]
fn l116f_a_vvc_unit_past_65_535_bytes_is_refused_in_matroska_too() {
  let with = |len: usize| {
    start_coded(&[
      &vvc_unit(15),
      &vvc_unit(16),
      &sized(&[0x00, (23 << 3) | 1], len),
    ])
  };
  let cases = [
    ("an SEI of 65 536 bytes", with(65_536), true),
    ("an SEI of 65 535 bytes", with(65_535), false),
  ];
  vvcc_law("l116f", &mp4_and_flv(), (&vvc_sets(0, 1, 1), true), &cases);
  vvcc_law(
    "l116f-mkv",
    &matroska(),
    (&vvc_sets(0, 1, 1), false),
    &cases,
  );
}

/// **L116g — a VVC array of 65 536 units is refused**, in Matroska too
/// (`vvc.c` 460–473): its 16-bit count wraps, the record written
/// otherwise, success answered. New extradata of its size passes the
/// side data's budget, refused first.
///
/// PLANT: the count unjudged opens.
#[test]
fn l116g_a_vvc_array_of_65_536_units_is_refused_in_matroska_too() {
  let with = |count: usize| {
    let (s, p, sei) = (vvc_unit(15), vvc_unit(16), vvc_unit(23));
    start_coded(&[vec![&s[..], &p[..]], copies(&sei, count)].concat())
  };
  let cases = [
    ("65 536 SEIs", with(65_536), true),
    ("65 535 SEIs", with(65_535), false),
  ];
  let containers = [mp4_and_flv(), matroska()].concat();
  vvcc_law("l116g", &containers, (&vvc_sets(0, 1, 1), false), &cases);
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

/// An EVC SPS of bit depths `luma` and `chroma` less eight.
fn evc_sps(luma: u32, chroma: u32) -> Vec<u8> {
  let syntax = Syntax::default()
    .ue(0)
    .u(1, 8)
    .u(51, 8)
    .u(0, 32)
    .u(0, 32)
    .ue(1)
    .ue(64)
    .ue(64)
    .ue(luma)
    .ue(chroma)
    .u(0, 1);
  [&evc_header(24)[..], &syntax.rbsp()].concat()
}

/// An EVC law: `cases` against `ff_isom_write_evcc` in MP4.
fn evcc_law(law: &str, packets: bool, cases: &[(&str, Vec<u8>, bool)]) {
  let Some(corpus) = Corpus::new() else { return };
  let frame = length_prefixed_units(&[&[&evc_header(1)[..], &[0x80]].concat()]);
  recoded_law(
    &corpus,
    law,
    &[(ContainerName::MP4, "mp4")],
    (ffmpeg_next::ffi::AVCodecID::AV_CODEC_ID_EVC, None, &frame),
    (&evc_base(), packets),
    cases,
  );
}

/// The EVC laws' configuration: an SPS and a PPS.
fn evc_base() -> Vec<u8> {
  length_prefixed_units(&[&evc_sps(2, 2), &[&evc_header(25)[..], &[0x80]].concat()])
}

/// **L117a — an EVC configuration of fewer than 8 bytes is refused**
/// (`evc.c` 293–295; `movenc.c` 1676–1691).
///
/// PLANT: the length unjudged opens, and takes the packet.
#[test]
fn l117a_an_evc_configuration_of_fewer_than_8_bytes_is_refused() {
  evcc_law(
    "l117a",
    true,
    &[
      ("seven bytes", vec![0, 0, 0, 3, 0x32, 0, 0x80], true),
      ("an SPS and a PPS", evc_base(), false),
    ],
  );
}

/// **L117b — an EVC unit whose header has the forbidden bit or no type is
/// refused** (`evc.c` 312–316; `evc.h` 32–46).
///
/// PLANT: the header unjudged opens, and takes the packet.
#[test]
fn l117b_an_evc_unit_of_the_forbidden_bit_or_no_type_is_refused() {
  evcc_law(
    "l117b",
    true,
    &[
      (
        "the forbidden bit",
        [evc_base(), length_prefixed_units(&[&[0xB2, 0]])].concat(),
        true,
      ),
      (
        "no type",
        [evc_base(), length_prefixed_units(&[&[0, 0]])].concat(),
        true,
      ),
      ("an SPS and a PPS", evc_base(), false),
    ],
  );
}

/// **L117c — an EVC configuration of no SPS, or of more than 16, is
/// refused** (`evc.c` 219–223).
///
/// PLANT: the count unjudged opens, and takes the packet.
#[test]
fn l117c_an_evc_configuration_of_no_sps_or_17_is_refused() {
  let sps = evc_sps(2, 2);
  evcc_law(
    "l117c",
    true,
    &[
      (
        "no SPS",
        length_prefixed_units(&[&[&evc_header(25)[..], &[0x80]].concat()]),
        true,
      ),
      ("17 SPSs", length_prefixed_units(&copies(&sps, 17)), true),
      ("16 SPSs", length_prefixed_units(&copies(&sps, 16)), false),
    ],
  );
}

/// **L117d — an EVC SPS of a bit depth past 14 is refused** (`evc.c`
/// 96–98): the record holds three bits of it, and the writer refuses
/// more than 6.
///
/// PLANT: the depth unjudged opens, and takes the packet.
#[test]
fn l117d_an_evc_sps_of_a_bit_depth_past_14_is_refused() {
  evcc_law(
    "l117d",
    true,
    &[
      ("luma 15", length_prefixed_units(&[&evc_sps(7, 6)]), true),
      ("chroma 15", length_prefixed_units(&[&evc_sps(6, 7)]), true),
      ("14", length_prefixed_units(&[&evc_sps(6, 6)]), false),
    ],
  );
}

/// **L117e — an EVC SPS of a byte is refused** (`evc.c` 67–71): its
/// reader past the unit's header takes fewer than none.
///
/// PLANT: the reader's length unjudged opens, and takes the packet.
#[test]
fn l117e_an_evc_sps_of_a_byte_is_refused() {
  evcc_law(
    "l117e",
    true,
    &[
      (
        "an SPS of a byte",
        [evc_base(), length_prefixed_units(&[&[0x32], &[0x34, 0]])].concat(),
        true,
      ),
      ("an SPS and a PPS", evc_base(), false),
    ],
  );
}

/// **L117f — an EVC unit past 65 535 bytes is refused** (`evc.c` 129–131,
/// 266–271): written with its length cut to 16 bits, success answered.
///
/// PLANT: the length unjudged opens, and takes the packet.
#[test]
fn l117f_an_evc_unit_past_65_535_bytes_is_refused() {
  let with = |len: usize| {
    [
      evc_base(),
      length_prefixed_units(&[&sized(&evc_header(28), len)]),
    ]
    .concat()
  };
  evcc_law(
    "l117f",
    true,
    &[
      ("65 536 bytes", with(65_536), true),
      ("65 535 bytes", with(65_535), false),
    ],
  );
}

/// **L117g — an EVC array of 65 536 units is refused** (`evc.c` 112–131):
/// its 16-bit count wraps. New extradata of its size passes the side
/// data's budget, refused first.
///
/// PLANT: the count unjudged opens.
#[test]
fn l117g_an_evc_array_of_65_536_units_is_refused() {
  let sei = [&evc_header(28)[..], &[0x80]].concat();
  let with = |count: usize| [evc_base(), length_prefixed_units(&copies(&sei, count))].concat();
  evcc_law(
    "l117g",
    false,
    &[
      ("65 536 SEIs", with(65_536), true),
      ("65 535 SEIs", with(65_535), false),
    ],
  );
}

/// An LCEVC unit of `kind` — 29 IDR, 28 non-IDR — of `blocks` and its stop
/// byte.
fn lcevc_unit_of(kind: u8, blocks: &[&[u8]]) -> Vec<u8> {
  let mut unit = vec![0x40 | (kind << 1) | 1, 0xFF];
  for block in blocks {
    unit.extend_from_slice(block);
  }
  unit.push(0x80);
  unit
}

/// LCEVC blocks: a sequence configuration of a byte; a global
/// configuration of resolution type 17, 1280 × 720.
const LCEVC_SEQUENCE: [u8; 2] = [0x20, 0x11];
const LCEVC_GLOBAL: [u8; 3] = [0x41, 17 << 1, 0x40];

/// An additional information block of `len` bytes, its size in a
/// multi-byte count.
fn lcevc_info(len: usize) -> Vec<u8> {
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
  let mut block = vec![0xE5];
  block.extend(size);
  block.resize(block.len() + len, 0x11);
  block
}

/// An LCEVC law: `cases` against `ff_isom_write_lvcc` in MP4.
fn lvcc_law(law: &str, packets: bool, cases: &[(&str, Vec<u8>, bool)]) {
  let Some(corpus) = Corpus::new() else { return };
  let frame = start_coded(&[&lcevc_unit_of(28, &[&[0x23, 0x11, 0x11, 0x11]])]);
  let base = start_coded(&[&lcevc_unit_of(29, &[&LCEVC_SEQUENCE, &LCEVC_GLOBAL])]);
  recoded_law(
    &corpus,
    law,
    &[(ContainerName::MP4, "mp4")],
    (ffmpeg_next::ffi::AVCodecID::AV_CODEC_ID_LCEVC, None, &frame),
    (&base, packets),
    cases,
  );
}

/// **L118a — an LCEVC configuration of at most 6 bytes is refused**
/// (`lcevc.c` 281–282; `movenc.c` 1693–1704).
///
/// PLANT: the length unjudged opens, and takes the packet.
#[test]
fn l118a_an_lcevc_configuration_of_6_bytes_is_refused() {
  lvcc_law(
    "l118a-lvcc",
    false,
    &[
      ("an lvcC of six bytes", vec![1, 0, 0, 0, 0, 0], true),
      ("an lvcC of seven bytes", vec![1, 0, 0, 0, 0, 0, 0], false),
    ],
  );
  lvcc_law(
    "l118a",
    true,
    &[
      (
        "six bytes of a start code",
        vec![0, 0, 0, 1, 0x7B, 0xFF],
        true,
      ),
      (
        "an IDR unit",
        start_coded(&[&lcevc_unit_of(29, &[&LCEVC_SEQUENCE, &LCEVC_GLOBAL])]),
        false,
      ),
    ],
  );
}

/// **L118b — an LCEVC configuration of no IDR or non-IDR unit is
/// refused** (`lcevc.c` 316–319).
///
/// PLANT: the units unjudged opens, and takes the packet.
#[test]
fn l118b_an_lcevc_configuration_of_no_idr_unit_is_refused() {
  lvcc_law(
    "l118b",
    true,
    &[
      (
        "a unit of another type",
        start_coded(&[&lcevc_unit_of(1, &[&LCEVC_SEQUENCE])]),
        true,
      ),
      (
        "a non-IDR unit",
        start_coded(&[&lcevc_unit_of(28, &[&LCEVC_SEQUENCE, &LCEVC_GLOBAL])]),
        false,
      ),
    ],
  );
}

/// **L118c — an LCEVC unit of no sequence or no global configuration is
/// refused** (`lcevc.c` 139–140).
///
/// PLANT: the blocks unjudged opens, and takes the packet.
#[test]
fn l118c_an_lcevc_unit_of_no_sequence_or_global_configuration_is_refused() {
  lvcc_law(
    "l118c",
    true,
    &[
      (
        "no global configuration",
        start_coded(&[&lcevc_unit_of(29, &[&LCEVC_SEQUENCE])]),
        true,
      ),
      (
        "no sequence configuration",
        start_coded(&[&lcevc_unit_of(29, &[&LCEVC_GLOBAL])]),
        true,
      ),
      (
        "both",
        start_coded(&[&lcevc_unit_of(29, &[&LCEVC_SEQUENCE, &LCEVC_GLOBAL])]),
        false,
      ),
    ],
  );
}

/// **L118d — an LCEVC unit written past 65 535 bytes is refused**
/// (`lcevc.c` 141–147): its length written in 16 bits, cut, success
/// answered.
///
/// PLANT: the length unjudged opens, and takes the packet.
#[test]
fn l118d_an_lcevc_unit_past_65_535_bytes_is_refused() {
  // Written: 3 bytes, the sequence block's 2, the global block's 3, and the
  // information block's type byte, its 3 size bytes and its payload.
  let with = |total: usize| {
    start_coded(&[&lcevc_unit_of(
      29,
      &[
        &LCEVC_SEQUENCE,
        &lcevc_info(total - 3 - 2 - 3 - 4),
        &LCEVC_GLOBAL,
      ],
    )])
  };
  lvcc_law(
    "l118d",
    true,
    &[
      ("65 536 bytes", with(65_536), true),
      ("65 535 bytes", with(65_535), false),
    ],
  );
}

/// **L118e — an LCEVC configuration of 65 536 IDR units is refused**
/// (`lcevc.c` 331–334): their count is written in 16 bits. New extradata
/// of its size passes the side data's budget, refused first.
///
/// PLANT: the count unjudged opens.
#[test]
fn l118e_an_lcevc_configuration_of_65_536_idr_units_is_refused() {
  let unit = lcevc_unit_of(29, &[&LCEVC_SEQUENCE, &LCEVC_GLOBAL]);
  lvcc_law(
    "l118e",
    false,
    &[
      ("65 536 units", start_coded(&copies(&unit, 65_536)), true),
      ("65 535 units", start_coded(&copies(&unit, 65_535)), false),
    ],
  );
}

/// A VP9 law: the corpus's VP9 track of the pixel format of `cases`, in
/// MP4 and FLV; the format is the stream's, no packet's.
fn vpcc_law(law: &str, cases: &[(&str, i32, bool)]) {
  let Some(corpus) = Corpus::new() else { return };
  let (mut table, packets) = first_track(&vp9_webm(&corpus), TrackKind::Video, 3);
  let row = table.remove(0);
  let config = stream_extradata_of(&row);
  let frames: Vec<Video> = packets.iter().map(video_of).collect();
  for (name, format, refused) in cases {
    let track = |config: &[u8]| {
      restated(
        &row,
        ffmpeg_next::ffi::AVCodecID::AV_CODEC_ID_VP9,
        config,
        None,
        Some(*format),
      )
    };
    configuration_law(
      &corpus,
      &format!("{law}-{format}"),
      &mp4_and_flv(),
      &track,
      (&frames, &config, false),
      &[(name, config.clone(), *refused)],
    );
  }
}

/// **L119a — a VP9 track of a pixel format of no descriptor is refused**
/// (`vpcc.c` 59–68, 157–158; `movenc.c` 1628–1637; `flvenc.c` 895): the
/// `vpcC` reads the stream's format whatever its configuration.
///
/// PLANT: the descriptor unjudged opens.
#[test]
fn l119a_a_vp9_track_of_no_pixel_format_is_refused() {
  use ffmpeg_next::ffi::AVPixelFormat as P;
  vpcc_law(
    "l119a",
    &[
      ("no format", P::AV_PIX_FMT_NONE as i32, true),
      ("4:2:0", P::AV_PIX_FMT_YUV420P as i32, false),
    ],
  );
}

/// **L119b — a VP9 track of 4:4:0 is refused** (`vpcc.c` 38–56): VP9's
/// profiles 1 and 3 take it, and the `vpcC` names no such subsampling.
///
/// PLANT: the subsampling unjudged opens.
#[test]
fn l119b_a_vp9_track_of_4_4_0_is_refused() {
  use ffmpeg_next::ffi::AVPixelFormat as P;
  vpcc_law(
    "l119b",
    &[
      ("4:4:0", P::AV_PIX_FMT_YUV440P as i32, true),
      ("4:4:4", P::AV_PIX_FMT_YUV444P as i32, false),
    ],
  );
}

/// **L119c — a VP9 configuration past what `init_get_bits8` takes, read
/// for an unknown profile, is refused** (`vpcc.c` 163–171): at open, and
/// MP4's first sample where the track states none, which MP4 takes as
/// its configuration (`movenc.c` 7024–7041). FLV's packet road writes no
/// sample as a configuration.
///
/// PLANT: the reader's bound unjudged opens, and takes the sample.
#[test]
fn l119c_a_vp9_configuration_past_the_reader_is_refused() {
  let Some(corpus) = Corpus::new() else { return };
  let (mut table, packets) = first_track(&vp9_webm(&corpus), TrackKind::Video, 1);
  let row = table.remove(0);
  let frame = video_of(&packets[0]);
  // `init_get_bits8` takes 268 435 391 bytes at most.
  let past = 268_435_392;
  let unknown = |config: &[u8]| {
    restated_profile(
      &row,
      ffmpeg_next::ffi::AVCodecID::AV_CODEC_ID_VP9,
      config,
      (None, None, Some(-99)),
    )
  };
  let large = vec![0x11_u8; past];
  for (container, extension) in mp4_and_flv() {
    let target = output(&corpus, &format!("l119c.{extension}"));
    let refused = FfmpegOwnedMuxer::open(
      File::create(&target).expect("create"),
      container,
      vec![unknown(&large)],
      &[],
      PacketLimits::default(),
    );
    assert!(
      matches!(refused, Err(MuxOpenError::ConfigurationInvalid { .. })),
      "{extension}: {:?}",
      refused.map(drop)
    );
  }
  drop(large);
  let target = output(&corpus, "l119c-sample.mp4");
  let mut muxer = muxer_with(
    &target,
    ContainerName::MP4,
    vec![unknown(&[])],
    filters(false),
  );
  let mut sample = frame.data().as_ref().to_vec();
  sample.resize(past, 0);
  let offered = VideoTrackPacket::new(TrackIndex::new(0), video_holding(&frame, &sample));
  drop(sample);
  let (refusal, back) = by_container(muxer.write_video(offered.clone()));
  assert_eq!(refusal, payload(PayloadRule::NewExtradata));
  untouched!(offered, back);
  drop((offered, back));
  muxer
    .write_video(VideoTrackPacket::new(TrackIndex::new(0), frame))
    .map_err(|error| error.map_packet(drop))
    .expect("a sample the reader takes");
  muxer.finish().expect("finished");
}

/// AVS3, which the base names no constant for.
const AVS3_ID: ffmpeg_next::ffi::AVCodecID = ffmpeg_next::ffi::AVCodecID::AV_CODEC_ID_AVS3;

/// **L120a — an AVS3 configuration of fewer than 4 bytes is refused**
/// (`movenc.c` 1599–1602, 1618–1626): `mov_write_av3c` refuses it, the
/// `av3c` left empty; and one of none, refused as missing.
///
/// PLANT: the length unjudged opens, and takes the packet.
#[test]
fn l120a_an_avs3_configuration_of_fewer_than_4_bytes_is_refused() {
  let Some(corpus) = Corpus::new() else { return };
  recoded_law(
    &corpus,
    "l120a",
    &[(ContainerName::MP4, "mp4")],
    (AVS3_ID, None, &[0, 0, 1, 0xB0, 0x11]),
    (&[1, 0, 0, 0], true),
    &[
      ("three bytes", vec![1, 0, 0], true),
      ("four", vec![1, 0, 0, 0], false),
    ],
  );
  let (row, _) = recoded_fixture(&corpus, &[0x11]);
  let target = output(&corpus, "l120a-none.mp4");
  let refused = FfmpegOwnedMuxer::open(
    File::create(&target).expect("create"),
    ContainerName::MP4,
    vec![restated(&row, AVS3_ID, &[], None, None)],
    &[],
    PacketLimits::default(),
  );
  assert!(
    matches!(refused, Err(MuxOpenError::ConfigurationMissing { .. })),
    "{:?}",
    refused.map(drop)
  );
}

/// **L120b — an AVS3 sequence header past 65 535 bytes is refused**
/// (`movenc.c` 1610–1611): written behind its 16-bit length, cut.
///
/// PLANT: the length unjudged opens, and takes the packet.
#[test]
fn l120b_an_avs3_sequence_header_past_65_535_bytes_is_refused() {
  let Some(corpus) = Corpus::new() else { return };
  recoded_law(
    &corpus,
    "l120b",
    &[(ContainerName::MP4, "mp4")],
    (AVS3_ID, None, &[0, 0, 1, 0xB0, 0x11]),
    (&[1, 0, 0, 0], true),
    &[
      ("65 536 bytes", vec![0; 65_536], true),
      ("65 535 bytes", vec![0; 65_535], false),
    ],
  );
}

/// **L120c — a VC-1 configuration of no sequence header is refused**
/// (`movenc.c` 1102–1160, 2915–2916): `mov_write_dvc1_tag` refuses it and
/// MP4 writes the sample entry of no `dvc1` — VC-1's simple and main
/// profiles' `STRUCT_C` among them.
///
/// PLANT: the sequence header unjudged opens, and takes the packet.
#[test]
fn l120c_a_vc1_configuration_of_no_sequence_header_is_refused() {
  let Some(corpus) = Corpus::new() else { return };
  let header = vec![0, 0, 1, 0x0F, 0xC8, 0x0A];
  recoded_law(
    &corpus,
    "l120c",
    &[(ContainerName::MP4, "mp4")],
    (
      ffmpeg_next::ffi::AVCodecID::AV_CODEC_ID_VC1,
      None,
      &[0, 0, 1, 0x0D, 0x11],
    ),
    (&header, true),
    &[
      ("a STRUCT_C", vec![0x4E, 0x09, 0x30, 0x01], true),
      (
        "an entry point alone",
        vec![0, 0, 1, 0x0E, 0x48, 0x80],
        true,
      ),
      ("an advanced sequence header", header.clone(), false),
    ],
  );
}

/// **L121 — FLV's `extract_extradata` road** (`extract_extradata.c`
/// 166–275; `flvenc.c` 1250–1263, 1495–1501): on an H.264 or HEVC track of
/// no configuration, a packet of no unit the filter splits off — one of
/// length prefixes — is refused by name (the filter refuses it and
/// libavformat drops it); a packet whose parameter sets make a
/// configuration the writer refuses — an SPS and no PPS — is refused as its
/// new extradata; then the packets of sets the writer takes, taken.
///
/// PLANT: each refusal removed takes the packet.
#[test]
fn l121_flvs_filter_makes_a_configuration_the_writer_must_take() {
  let Some(corpus) = Corpus::new() else { return };
  let (row, frames, sps, pps) = h264_fixture(&corpus);
  let target = output(&corpus, "l121.flv");
  let mut muxer = muxer_with(
    &target,
    ContainerName::new("flv"),
    vec![restated(
      &row,
      ffmpeg_next::ffi::AVCodecID::AV_CODEC_ID_H264,
      &[],
      None,
      None,
    )],
    filters(true),
  );
  let frame = frames[0].data().as_ref().to_vec();
  // A unit behind a 4-byte length no start code lies in.
  let prefixed = vec![0, 0, 0, 5, 0x65, 0x88, 0x84, 0x21, 0x10];
  let units = nal_units_of(&frame);
  let slices: Vec<&[u8]> = units
    .iter()
    .copied()
    .filter(|unit| unit[0] & 0x1F != 7 && unit[0] & 0x1F != 8)
    .collect();
  for (name, bytes, rule) in [
    ("length-prefixed", prefixed, PayloadRule::NalUnits),
    (
      "an SPS and no PPS",
      start_coded(&[vec![&sps[..]], slices.clone()].concat()),
      PayloadRule::NewExtradata,
    ),
  ] {
    let offered = VideoTrackPacket::new(TrackIndex::new(0), video_holding(&frames[0], &bytes));
    let (refusal, back) = by_container(muxer.write_video(offered.clone()));
    assert_eq!(refusal, payload(rule), "{name}");
    untouched!(offered, back);
  }
  let whole = start_coded(&[vec![&sps[..], &pps[..]], slices].concat());
  muxer
    .write_video(VideoTrackPacket::new(
      TrackIndex::new(0),
      video_holding(&frames[0], &whole),
    ))
    .map_err(|error| error.map_packet(drop))
    .expect("the sets the writer takes");
  for frame in &frames[1..] {
    muxer
      .write_video(VideoTrackPacket::new(TrackIndex::new(0), frame.clone()))
      .map_err(|error| error.map_packet(drop))
      .expect("a frame");
  }
  muxer.finish().expect("finished");
  let record = stream_extradata_of(&table_of(&target, TrackKind::Video)[0]);
  assert_eq!(
    (record.first(), record.get(5).map(|count| count & 0x1F)),
    (Some(&1), Some(1)),
    "read back an avcC of the packet's one SPS"
  );
  // HEVC: a packet of no unit split off, refused alike.
  let target = output(&corpus, "l121-hevc.flv");
  let mut muxer = muxer_with(
    &target,
    ContainerName::new("flv"),
    vec![restated(
      &row,
      ffmpeg_next::ffi::AVCodecID::AV_CODEC_ID_HEVC,
      &[],
      None,
      None,
    )],
    filters(true),
  );
  let offered = VideoTrackPacket::new(
    TrackIndex::new(0),
    video_holding(&frames[0], &[0, 0, 0, 3, 0x26, 0x01, 0xAF]),
  );
  let (refusal, back) = by_container(muxer.write_video(offered.clone()));
  assert_eq!(refusal, payload(PayloadRule::NalUnits), "HEVC");
  untouched!(offered, back);
}

/// The NAL units of an Annex B `bytes`, each after its start code.
fn nal_units_of(bytes: &[u8]) -> Vec<&[u8]> {
  let starts: Vec<usize> = (0..bytes.len().saturating_sub(3))
    .filter(|&at| bytes[at..at + 3] == [0, 0, 1])
    .map(|at| at + 3)
    .collect();
  starts
    .iter()
    .enumerate()
    .map(|(index, &start)| {
      let end = starts.get(index + 1).map_or(bytes.len(), |&next| {
        let mut end = next - 3;
        while end > start && bytes[end - 1] == 0 {
          end -= 1;
        }
        end
      });
      &bytes[start..end]
    })
    .collect()
}

/// ISMV's 10 MHz timescale, every track's (`movenc.c` 8622–8626): the laws
/// count its ticks exactly.
fn ism_ticks() -> Timebase {
  Timebase::new(1, NonZeroI32::new(10_000_000).expect("non-zero"))
}

/// One second of H.264 at 25 frames a second with two B-frames and a key
/// frame every tenth, its frames in decode order, each at its decode index
/// in 40 ms steps and presented at its display index the stream's reorder
/// delay later, in ISMV's ticks — and the delay: its first frame's
/// composition offset, the shift ISMV moves every decode time by.
#[rustfmt::skip]
fn ism_frames(corpus: &Corpus) -> (PathBuf, Vec<(Video, i64, i64)>, i64) {
  let source = generated(corpus, "bframes-gop10.mkv", &[
    "-f", "lavfi", "-i", "testsrc2=size=64x48:rate=25:duration=1",
    "-c:v", "libx264", "-preset", "medium", "-bf", "2", "-g", "10", "-keyint_min", "10",
    "-sc_threshold", "0", "-pix_fmt", "yuv420p",
  ]);
  let (_, packets) = first_track(&source, TrackKind::Video, 25);
  let frames: Vec<Video> = packets.iter().map(video_of).collect();
  let shown: Vec<i64> = frames.iter().map(|frame| in_ms(frame.pts()).expect("a pts")).collect();
  let start = *shown.iter().min().expect("frames");
  let display: Vec<i64> = shown.iter().map(|at| (at - start) / 40).collect();
  let delay = display
    .iter()
    .enumerate()
    .map(|(index, &at)| index as i64 - at)
    .max()
    .expect("frames");
  const FRAME: i64 = 400_000;
  let timed = frames
    .into_iter()
    .zip(&display)
    .enumerate()
    .map(|(index, (frame, &at))| (frame, (at + delay) * FRAME, index as i64 * FRAME))
    .collect();
  (source, timed, delay * FRAME)
}

/// `frame` on `track`, presented at `pts` and decoded at `dts` ISMV ticks.
fn ism_at(
  track: usize,
  frame: &Video,
  pts: i64,
  dts: i64,
) -> VideoTrackPacket<Ffmpeg, FfmpegBytes> {
  VideoTrackPacket::new(
    TrackIndex::new(track),
    frame
      .clone()
      .with_pts(Some(Timestamp::new(pts, ism_ticks())))
      .with_dts(Some(Timestamp::new(dts, ism_ticks()))),
  )
}

/// The frames `from..to` of `frames` written on `tracks` in turn, counted
/// in `base` — ISMV's ticks over `per` — each moved `after` of them later.
fn write_ism(
  muxer: &mut FfmpegOwnedMuxer<File>,
  tracks: &[usize],
  frames: &[(Video, i64, i64)],
  (from, to, after): (usize, usize, i64),
  (base, per): (Timebase, i64),
) {
  for (frame, pts, dts) in &frames[from..to] {
    for &track in tracks {
      let packet = frame
        .clone()
        .with_pts(Some(Timestamp::new(pts / per + after, base)))
        .with_dts(Some(Timestamp::new(dts / per + after, base)));
      muxer
        .write_video(VideoTrackPacket::new(TrackIndex::new(track), packet))
        .map_err(|error| error.map_packet(drop))
        .unwrap_or_else(|error| panic!("a frame at {dts}: {error:?}"));
    }
  }
}

/// **L123a — ISMV refuses a decode gap its second `check_pkt` call would
/// move** (R10, L6; `movenc.c` 6897–6923, 7396–7410, 1248–1256): ISMV's
/// negative composition offsets move each decode time by the track's first
/// composition offset `s` between `check_pkt`'s two calls, its reference
/// not, so that the second counts a gap `s` longer, and one of
/// `INT_MAX − s` ticks — short of the first call's `INT_MAX` — is moved,
/// its decode time rewritten `s − 1` ticks back, and the next fragment's
/// flush asserts on it, ending the process. Refused by name, the packet
/// handed back, before a predicted picture, and the file written whole
/// without it. A key frame of the one video track — a second is refused
/// at open (L132) — flushes before the second call and keeps the first's
/// bound: refused at `INT_MAX`.
///
/// PLANT: the shift unmirrored, the predicted picture taken — FFmpeg then
/// ends the process on `Assertion next_dts >= 0 failed at
/// libavformat/movenc.c:1255`; the flushing key frame's gap unbounded, the
/// second file's key frame taken; the shift the packet's own offset, the
/// predicted picture's of none, it taken.
#[test]
fn l123a_ismv_refuses_a_gap_its_shifted_second_check_would_move() {
  let Some(corpus) = Corpus::new() else { return };
  let (source, frames, shift) = ism_frames(&corpus);
  assert!(shift > 0, "the premise: a reorder delay, {shift}");
  let limit = i64::from(i32::MAX) - shift;
  let key = |index: usize| frames[index].0.flags().contains(PacketFlags::KEY);
  let predicted = (3..frames.len())
    .find(|&index| !key(index))
    .expect("a predicted picture");
  let second_key = (1..frames.len())
    .find(|&index| key(index))
    .expect("a second key frame");

  let target = output(&corpus, "l123a.ismv");
  let mut muxer = muxer_into(
    &target,
    ContainerName::new("ismv"),
    table_of(&source, TrackKind::Video),
  );
  write_ism(
    &mut muxer,
    &[0],
    &frames,
    (0, predicted, 0),
    (ism_ticks(), 1),
  );
  let (frame, pts, dts) = &frames[predicted];
  let after = limit - (dts - frames[predicted - 1].2);
  let offered = ism_at(0, frame, pts + after, dts + after);
  let (refusal, back) = by_container(muxer.write_video(offered.clone()));
  assert_eq!(
    refusal,
    FfmpegMuxRefusal::DtsGap {
      track: TrackIndex::new(0),
      dts: Timestamp::new(dts + after, ism_ticks()),
    }
  );
  untouched!(offered, back);
  write_ism(
    &mut muxer,
    &[0],
    &frames,
    (predicted, frames.len(), 0),
    (ism_ticks(), 1),
  );
  muxer.finish().expect("finished");
  assert_eq!(packet_count(&target), frames.len());

  // The only video track's key frame flushes before the second call: its
  // bound is the first's, `INT_MAX`.
  let target = output(&corpus, "l123a-key.ismv");
  let mut muxer = muxer_into(
    &target,
    ContainerName::new("ismv"),
    table_of(&source, TrackKind::Video),
  );
  write_ism(
    &mut muxer,
    &[0],
    &frames,
    (0, second_key, 0),
    (ism_ticks(), 1),
  );
  let (frame, pts, dts) = &frames[second_key];
  let after = i64::from(i32::MAX) - (dts - frames[second_key - 1].2);
  let offered = ism_at(0, frame, pts + after, dts + after);
  let (refusal, back) = by_container(muxer.write_video(offered.clone()));
  assert_eq!(
    refusal,
    FfmpegMuxRefusal::DtsGap {
      track: TrackIndex::new(0),
      dts: Timestamp::new(dts + after, ism_ticks()),
    },
    "a key frame at INT_MAX"
  );
  untouched!(offered, back);
  write_ism(
    &mut muxer,
    &[0],
    &frames,
    (second_key, frames.len(), 0),
    (ism_ticks(), 1),
  );
  muxer.finish().expect("finished");
}

/// **L123b — ISMV takes the gap one tick short of it, and the gap itself
/// before a key frame of the file's only video track** (R10, L6;
/// `movenc.c` 7445–7471): one tick short, the second call's count passes;
/// before the only video track's key frame, the fragment's flush comes
/// between the two calls, the second counting from the key frame's own
/// decode time. MP4, of no negative composition offsets, counts once, and
/// takes the gap ISMV refuses. Each file finishes, every packet read back,
/// the gap kept.
///
/// PLANT: the bound a tick lower, the first gap refused; the key frame's
/// flush unmirrored, the second refused; the shift MP4's too, the third
/// refused.
#[test]
fn l123b_ismv_takes_the_gap_short_of_it_and_before_a_key_frame() {
  let Some(corpus) = Corpus::new() else { return };
  let (source, frames, shift) = ism_frames(&corpus);
  let limit = i64::from(i32::MAX) - shift;
  let key = |index: usize| frames[index].0.flags().contains(PacketFlags::KEY);
  let predicted = (3..frames.len())
    .find(|&index| !key(index))
    .expect("a predicted picture");
  let second_key = (1..frames.len())
    .find(|&index| key(index))
    .expect("a second key frame");
  // MP4's video timescale: the track's 1/1000 doubled to 16 000 (`movenc.c`
  // 8485–8492), ISMV's ticks over 625.
  let mp4 = (
    Timebase::new(1, NonZeroI32::new(16_000).expect("non-zero")),
    625,
  );
  for (name, container, (base, per), at, gap) in [
    (
      "short of it",
      "ismv",
      (ism_ticks(), 1),
      predicted,
      limit - 1,
    ),
    (
      "before a key frame",
      "ismv",
      (ism_ticks(), 1),
      second_key,
      limit,
    ),
    // MP4, of no negative composition offsets: one count.
    (
      "in MP4",
      "mp4",
      mp4,
      predicted,
      i64::from(i32::MAX) - shift / 625,
    ),
  ] {
    let target = output(&corpus, &format!("l123b-{at}.{container}"));
    let mut muxer = muxer_into(
      &target,
      ContainerName::new(container),
      table_of(&source, TrackKind::Video),
    );
    write_ism(&mut muxer, &[0], &frames, (0, at, 0), (base, per));
    let after = gap - (frames[at].2 - frames[at - 1].2) / per;
    write_ism(
      &mut muxer,
      &[0],
      &frames,
      (at, frames.len(), after),
      (base, per),
    );
    muxer
      .finish()
      .unwrap_or_else(|error| panic!("{name}: {error:?}"));
    let back = &packets(&target)[0];
    assert_eq!(back.len(), frames.len(), "{name}: every packet");
    let ticks = |index: usize| {
      back[index]
        .dts
        .expect("a decode time")
        .checked_rescale_with(base, Rounding::Nearest)
        .expect("in the file's ticks")
        .pts()
    };
    assert_eq!(ticks(at) - ticks(at - 1), gap, "{name}: the gap kept");
  }
}

/// A metadata OBU of `len` bytes in all, of a type for private use (6),
/// its payload zeros to its trailing bits — two bytes of size, for a
/// payload of 128 to 16 383 bytes.
fn av1_metadata_obu(len: usize) -> Vec<u8> {
  let size = len - 3;
  assert!((128..16_384).contains(&size), "a two-byte size: {size}");
  let mut obu = vec![0x2A, (size & 0x7F) as u8 | 0x80, (size >> 7) as u8, 0x06];
  obu.resize(len - 1, 0);
  obu.push(0x80);
  obu
}

/// The AV1 fixture's table with no configuration, its frames, and its
/// `av1C`: the record's four bytes and the sequence header OBU.
fn unconfigured_av1(corpus: &Corpus) -> (PathBuf, Vec<Video>, Vec<u8>) {
  let av1 = av1_mkv(corpus);
  let (table, frames) = whole_track(&av1, TrackKind::Video);
  let av1c = stream_extradata_of(&table[0]);
  assert_eq!(
    av1c.len(),
    4 + 13,
    "the premise: the record and its sequence header"
  );
  (av1, frames.iter().map(video_of).collect(), av1c)
}

/// The Matroska and WebM AV1 laws' muxer: the fixture's track of no
/// configuration, into `target`.
fn unconfigured_av1_muxer(av1: &Path, target: &Path, container: &str) -> FfmpegOwnedMuxer<File> {
  let table = table_of(av1, TrackKind::Video)
    .into_iter()
    .map(|row| row.with_extradata(Some(bytes_of(&[]))))
    .collect();
  muxer_into(target, ContainerName::new(container), table)
}

/// `frame` on track 0, carrying `config` as new extradata.
fn configuring(frame: &Video, config: &[u8]) -> VideoTrackPacket<Ffmpeg, FfmpegBytes> {
  use ffmpeg_next::ffi::AVPacketSideDataType as T;
  let mut packet = frame.clone();
  packet
    .extra_mut()
    .set_side_data(vec![side(T::AV_PKT_DATA_NEW_EXTRADATA, config)]);
  VideoTrackPacket::new(TrackIndex::new(0), packet)
}

/// **L124a — Matroska and WebM take an AV1 track's first packet's
/// configuration where the track states none** (R10, L4: ruling 4a;
/// `matroskaenc.c` 1160–1165, 3098–3115, 1334–1369): the header reserves
/// the track's CodecPrivate room, `(AV1_SANE_SEQUENCE_HEADER_MAX_BITS + 7)
/// / 8 + 100` bytes, and the first packet's new extradata is written into
/// it as `ff_isom_write_av1c` writes it — a record as it is, OBUs behind
/// the record's four bytes — and becomes the stream's, every later one
/// ignored. Taken, at the room's last byte: a record, and OBUs — a sequence
/// header and a metadata OBU — each read back as written; a later packet's
/// new extradata, past the room, taken and reported dropped, the first's
/// not reported.
///
/// PLANT: Matroska's and WebM's AV1 left refused at open; the room a byte
/// short, the 493-byte configurations refused; every new extradata judged
/// against the room, the later one refused; the first's reported dropped.
#[test]
fn l124a_matroskas_av1_takes_its_first_packets_configuration() {
  use ffmpeg_next::ffi::AVPacketSideDataType as T;
  let Some(corpus) = Corpus::new() else { return };
  let (av1, frames, av1c) = unconfigured_av1(&corpus);
  let new_extradata = |line: &Uncarried| matches!(line, Uncarried::SideData { kind, .. } if *kind == T::AV_PKT_DATA_NEW_EXTRADATA as i32);
  // The record, and the OBUs, each written into 493 bytes.
  let record = [&av1c[..], &av1_metadata_obu(493 - av1c.len())].concat();
  let obus = [&av1c[4..], &av1_metadata_obu(493 - av1c.len())].concat();
  let written_obus = [&av1c[..4], &obus[..]].concat();
  for container in ["mkv", "webm"] {
    let name = if container == "mkv" {
      "matroska"
    } else {
      "webm"
    };
    for (form, config, written) in [
      ("a record", &record, &record),
      ("OBUs", &obus, &written_obus),
    ] {
      let what = format!("{container}, {form}");
      let target = output(&corpus, &format!("l124a-{}.{container}", form.len()));
      let mut muxer = unconfigured_av1_muxer(&av1, &target, name);
      muxer
        .write_video(configuring(&frames[0], config))
        .map_err(|error| error.map_packet(drop))
        .unwrap_or_else(|error| panic!("{what}: the first frame: {error:?}"));
      assert!(
        lines(muxer.report(), new_extradata).is_empty(),
        "{what}: the configuration is not dropped"
      );
      muxer
        .write_video(configuring(
          &frames[1],
          &[&av1c[..], &av1_metadata_obu(600)].concat(),
        ))
        .map_err(|error| error.map_packet(drop))
        .unwrap_or_else(|error| panic!("{what}: a later configuration: {error:?}"));
      assert_eq!(
        lines(muxer.report(), new_extradata),
        vec![Uncarried::SideData {
          track: TrackIndex::new(0),
          kind: T::AV_PKT_DATA_NEW_EXTRADATA as i32,
          name: Some("New Extradata"),
          fate: SideDataFate::Dropped,
        }],
        "{what}: the later one ignored"
      );
      for frame in &frames[2..] {
        muxer
          .write_video(VideoTrackPacket::new(TrackIndex::new(0), frame.clone()))
          .map_err(|error| error.map_packet(drop))
          .unwrap_or_else(|error| panic!("{what}: a frame: {error:?}"));
      }
      muxer
        .finish()
        .unwrap_or_else(|error| panic!("{what}: {error:?}"));
      assert_eq!(
        stream_extradata_of(&table_of(&target, TrackKind::Video)[0]),
        *written,
        "{what}: read back configured"
      );
      assert_eq!(packet_count(&target), frames.len(), "{what}: every frame");
    }
  }
}

/// **L124b — Matroska and WebM refuse an AV1 track's first configuration
/// past the room their header reserved** (R10, L4; `matroskaenc.c`
/// 1349–1358): past it, `mkv_update_codecprivate` writes the record's first
/// four bytes alone, its OBUs — the sequence header among them — lost.
/// Refused by name, the packet handed back, a byte past the room, as a
/// record and as OBUs; and a first packet bringing no configuration, which
/// Matroska refuses after taking it (3108–3109).
///
/// PLANT: the room unjudged, both taken; the OBUs counted without their
/// metadata, the OBUs taken; a record counted as its four bytes, the
/// record taken; the first packet's configuration not required, the
/// frame of none taken.
#[test]
fn l124b_matroskas_av1_refuses_a_configuration_past_its_room() {
  let Some(corpus) = Corpus::new() else { return };
  let (av1, frames, av1c) = unconfigured_av1(&corpus);
  let record = [&av1c[..], &av1_metadata_obu(494 - av1c.len())].concat();
  let obus = [&av1c[4..], &av1_metadata_obu(494 - av1c.len())].concat();
  for container in ["mkv", "webm"] {
    let name = if container == "mkv" {
      "matroska"
    } else {
      "webm"
    };
    for (form, config) in [("a record", &record), ("OBUs", &obus)] {
      let what = format!("{container}, {form}");
      let target = output(&corpus, &format!("l124b-{}.{container}", form.len()));
      let mut muxer = unconfigured_av1_muxer(&av1, &target, name);
      let offered = configuring(&frames[0], config);
      let (refusal, back) = by_container(muxer.write_video(offered.clone()));
      assert_eq!(refusal, payload(PayloadRule::NewExtradata), "{what}");
      untouched!(offered, back);
    }
    let target = output(&corpus, &format!("l124b-none.{container}"));
    let mut muxer = unconfigured_av1_muxer(&av1, &target, name);
    let offered = VideoTrackPacket::new(TrackIndex::new(0), frames[0].clone());
    let (refusal, back) = by_container(muxer.write_video(offered.clone()));
    assert_eq!(
      refusal,
      payload(PayloadRule::ConfigurationAbsent),
      "{container}: no configuration"
    );
    untouched!(offered, back);
  }
}

/// A fifth of a second of a tone at 48 kHz, panned by `pan` (its filter's
/// syntax, each output channel the input's), in `codec`, into `name`'s
/// container — `format` named where its extension does not say it.
fn panned(corpus: &Corpus, name: &str, pan: &str, codec: &str, format: Option<&str>) -> PathBuf {
  let pan = format!("pan={pan}");
  let mut args = vec![
    "-f",
    "lavfi",
    "-i",
    "sine=frequency=440:sample_rate=48000:duration=0.2",
    "-af",
    &pan,
    "-c:a",
    codec,
  ];
  if let Some(format) = format {
    args.extend(["-f", format]);
  }
  generated(corpus, name, &args)
}

/// The channel layout of `path`'s audio track as the demuxer reads it back.
fn layout_read_back_of(path: &Path) -> LayoutReadBack {
  use ffmpeg_next::ffi::AVChannelOrder as Order;
  let (table, _) = first_track(path, TrackKind::Audio, 0);
  let layout = table[0].side_data().ch_layout().clone();
  if layout.order() == Order::AV_CHANNEL_ORDER_NATIVE as i32 {
    LayoutReadBack::Native {
      mask: layout.mask(),
    }
  } else if layout.order() == Order::AV_CHANNEL_ORDER_CUSTOM as i32 {
    LayoutReadBack::Custom {
      channels: layout.map().iter().map(|channel| channel.id()).collect(),
    }
  } else {
    LayoutReadBack::Unordered {
      channels: layout.channels(),
    }
  }
}

/// The layout fixtures: PCM of each layout — mono and stereo in QuickTime,
/// whose `chan` box states them, 2.1, a stereo pair and LFE2, and a custom
/// order in MP4, whose `chnl` box does, stereo of no order in WAV, stereo
/// at 44.1 kHz, which FLV's legacy flags take — and FLAC's 2.1 raw, by its
/// mask comment, and of LFE2 in FLV, by its multichannel configuration.
struct Layouts {
  mono: PathBuf,
  stereo: PathBuf,
  two_one: PathBuf,
  lfe2: PathBuf,
  reversed: PathBuf,
  unordered: PathBuf,
  stereo_44: PathBuf,
  flac: PathBuf,
  flac_lfe2: PathBuf,
}

impl Layouts {
  fn of(corpus: &Corpus) -> Self {
    let stereo = panned(
      corpus,
      "layout-stereo.mov",
      "stereo|c0=c0|c1=c0",
      "pcm_s16le",
      None,
    );
    let unordered = output(corpus, "layout-unordered.wav");
    if !unordered.exists() {
      let status = std::process::Command::new("ffmpeg")
        .args(["-y", "-loglevel", "error", "-i"])
        .arg(&stereo)
        .args(["-c", "copy"])
        .arg(&unordered)
        .status()
        .expect("the ffmpeg CLI runs");
      assert!(status.success(), "ffmpeg made the WAV");
    }
    let stereo_44 = output(corpus, "layout-stereo-44.mov");
    if !stereo_44.exists() {
      let status = std::process::Command::new("ffmpeg")
        .args(["-y", "-loglevel", "error", "-f", "lavfi", "-i"])
        .arg("sine=frequency=440:sample_rate=44100:duration=0.2")
        .args(["-af", "pan=stereo|c0=c0|c1=c0", "-c:a", "pcm_s16le"])
        .arg(&stereo_44)
        .status()
        .expect("the ffmpeg CLI runs");
      assert!(status.success(), "ffmpeg made the 44.1 kHz stereo");
    }
    Self {
      mono: panned(corpus, "layout-mono.mov", "mono|c0=c0", "pcm_s16le", None),
      stereo,
      two_one: panned(
        corpus,
        "layout-2.1.mp4",
        "2.1|c0=c0|c1=c0|c2=c0",
        "pcm_s16le",
        None,
      ),
      lfe2: panned(
        corpus,
        "layout-lfe2.mp4",
        "FL+FR+LFE2|c0=c0|c1=c0|c2=c0",
        "pcm_s16le",
        None,
      ),
      reversed: panned(
        corpus,
        "layout-reversed.mp4",
        "FR+FL|c0=c0|c1=c0",
        "pcm_s16le",
        None,
      ),
      unordered,
      stereo_44,
      flac: panned(
        corpus,
        "layout-2.1.flac",
        "2.1|c0=c0|c1=c0|c2=c0",
        "flac",
        None,
      ),
      flac_lfe2: panned(
        corpus,
        "layout-lfe2.flv",
        "FL+FR+LFE2|c0=c0|c1=c0|c2=c0",
        "flac",
        None,
      ),
    }
  }
}

/// Writes `source`'s audio track whole into `container` at `target`, and
/// checks the report's channel layout line against `read_back` — none where
/// it is `None` — and the file's layout against it, or `source`'s.
fn layout_law(source: &Path, target: &Path, container: &str, read_back: Option<LayoutReadBack>) {
  let what = format!("{} into {container}", source.display());
  let (table, packets) = whole_track(source, TrackKind::Audio);
  let mut muxer = muxer_into(target, ContainerName::new(container), table);
  let lines = lines(muxer.report(), |line| {
    matches!(line, Uncarried::ChannelLayout { .. })
  });
  assert_eq!(
    lines,
    read_back
      .iter()
      .map(|read_back| Uncarried::ChannelLayout {
        track: TrackIndex::new(0),
        read_back: read_back.clone(),
      })
      .collect::<Vec<_>>(),
    "{what}: the report"
  );
  for packet in &packets {
    muxer
      .write_audio(AudioTrackPacket::new(TrackIndex::new(0), audio_of(packet)))
      .map_err(|error| error.map_packet(drop))
      .unwrap_or_else(|error| panic!("{what}: a packet: {error:?}"));
  }
  muxer
    .finish()
    .unwrap_or_else(|error| panic!("{what}: {error:?}"));
  assert_eq!(
    layout_read_back_of(target),
    read_back.unwrap_or_else(|| layout_read_back_of(source)),
    "{what}: the file"
  );
}

/// **L125 — RIFF's formats read a channel layout back as their mask
/// carries it** (R10, L1; table E; `riffenc.c` 79–84, 188–197; `riffdec.c`
/// 62–92, 258–263): WAV, Wave64, AVI and ASF write the mask of an
/// extensible format alone — a native layout but mono and stereo, below
/// `0x40000` — so that PCM's mono and stereo read back of no order, and a
/// mask past `0x40000` and a custom order of none, reported
/// ([`Uncarried::ChannelLayout`]); 2.1 and a layout of no order read back
/// as written; and in WAV, FLAC's decoder fills its own order in, and
/// MP3's sets its own.
///
/// PLANT: mono and stereo written extensible, the report's lines missing;
/// a mask past `0x40000` written, LFE2's line missing; a custom order read
/// as its mask; FLAC of no order of its own, WAV's FLAC line added; MP3's
/// decoder the container's, its line added.
#[test]
fn l125_riffs_formats_read_a_layout_back_as_their_mask_carries_it() {
  let Some(corpus) = Corpus::new() else { return };
  let layouts = Layouts::of(&corpus);
  let alone = |channels: i32| Some(LayoutReadBack::Unordered { channels });
  for container in ["wav", "w64", "avi", "asf"] {
    for (source, read_back) in [
      (&layouts.mono, alone(1)),
      (&layouts.stereo, alone(2)),
      (&layouts.two_one, None),
      (&layouts.lfe2, alone(3)),
      (&layouts.reversed, alone(2)),
      (&layouts.unordered, None),
    ] {
      let name = source
        .file_stem()
        .expect("a name")
        .to_string_lossy()
        .into_owned();
      layout_law(
        source,
        &output(&corpus, &format!("l125-{name}.{container}")),
        container,
        read_back,
      );
    }
  }
  let flac_stereo = panned(
    &corpus,
    "layout-stereo.flac",
    "stereo|c0=c0|c1=c0",
    "flac",
    None,
  );
  layout_law(&flac_stereo, &output(&corpus, "l125-flac.wav"), "wav", None);
  let mp3 = panned(
    &corpus,
    "layout-stereo.mp3",
    "stereo|c0=c0|c1=c0",
    "libmp3lame",
    None,
  );
  layout_law(&mp3, &output(&corpus, "l125-mp3.wav"), "wav", None);
}

/// **L126 — Matroska reads a channel layout back as its count, but FLAC's
/// by its comment** (R10, L1; table E; `matroskaenc.c` 1077–1114, 2130;
/// `matroskadec.c` 2183–2196, 2881–2887): PCM stereo reads back of no
/// order, reported; FLAC's 2.1 by its comment as written; FLAC's of LFE2,
/// past the comment's `0x3FFFF`, as FLAC's own 3.0, reported.
///
/// PLANT: Matroska's PCM read as written, its line missing; the comment
/// past `0x3FFFF`, LFE2's line missing.
#[test]
fn l126_matroska_reads_a_layout_back_as_its_count_but_flacs_by_its_comment() {
  let Some(corpus) = Corpus::new() else { return };
  let layouts = Layouts::of(&corpus);
  for (source, name, read_back) in [
    (
      &layouts.stereo,
      "pcm",
      Some(LayoutReadBack::Unordered { channels: 2 }),
    ),
    (&layouts.flac, "flac", None),
    (
      &layouts.flac_lfe2,
      "flac-lfe2",
      Some(LayoutReadBack::Native { mask: 0x7 }),
    ),
  ] {
    layout_law(
      source,
      &output(&corpus, &format!("l126-{name}.mkv")),
      "matroska",
      read_back,
    );
  }
}

/// **L127 — NUT reads a channel layout back as its count** (R10, L1;
/// table E; `nutenc.c` 487; `nutdec.c` 470): PCM stereo of no order,
/// FLAC's 2.1 as FLAC's own 3.0, each reported.
///
/// PLANT: NUT's layout read as written, both lines missing; FLAC of no
/// order of its own, its line naming no order.
#[test]
fn l127_nut_reads_a_layout_back_as_its_count() {
  let Some(corpus) = Corpus::new() else { return };
  let layouts = Layouts::of(&corpus);
  layout_law(
    &layouts.stereo,
    &output(&corpus, "l127-pcm.nut"),
    "nut",
    Some(LayoutReadBack::Unordered { channels: 2 }),
  );
  layout_law(
    &layouts.flac,
    &output(&corpus, "l127-flac.nut"),
    "nut",
    Some(LayoutReadBack::Native { mask: 0x7 }),
  );
}

/// **L128 — MP4 reads PCM's layout back from its `chnl` box, mono's and
/// FLAC's of its count** (R10, L1; table E; `movenc.c` 1299–1339,
/// 1524–1525; `mov.c` 2900–2901; `mov_chan.c` 768–859): PCM stereo and a
/// custom order read back as written; mono, of no `chnl`, of no order,
/// reported; FLAC's 2.1 as FLAC's own 3.0, reported.
///
/// PLANT: mono read from a `chnl`, its line missing; PCM of two channels
/// read as its count, stereo's line added.
#[test]
fn l128_mp4_reads_pcms_layout_back_from_its_chnl_box() {
  let Some(corpus) = Corpus::new() else { return };
  let layouts = Layouts::of(&corpus);
  for (source, name, read_back) in [
    (&layouts.stereo, "stereo", None),
    (&layouts.reversed, "reversed", None),
    (
      &layouts.mono,
      "mono",
      Some(LayoutReadBack::Unordered { channels: 1 }),
    ),
    (
      &layouts.flac,
      "flac",
      Some(LayoutReadBack::Native { mask: 0x7 }),
    ),
  ] {
    layout_law(
      source,
      &output(&corpus, &format!("l128-{name}.mp4")),
      "mp4",
      read_back,
    );
  }
}

/// **L129 — MXF reads a channel layout back as its count** (R10, L1;
/// table E; `mxfenc.c` 1714–1728; `mxfdec.c` 3066): OP-Atom's mono PCM of
/// no order, reported.
///
/// PLANT: MXF's layout read as written, the line missing.
#[test]
fn l129_mxf_reads_a_layout_back_as_its_count() {
  let Some(corpus) = Corpus::new() else { return };
  let layouts = Layouts::of(&corpus);
  layout_law(
    &layouts.mono,
    &output(&corpus, "l129.mxf"),
    "mxf_opatom",
    Some(LayoutReadBack::Unordered { channels: 1 }),
  );
}

/// **L130 — Ogg reads FLAC's layout back as its count, raw FLAC by its
/// comment** (R10, L1; table E; `flacenc.c` 244–259; `flacdec.c`
/// 207–219): Ogg's FLAC 2.1 as FLAC's own 3.0, reported; raw FLAC's 2.1
/// as written; its LFE2, past the comment's `0x3FFFF`, as 3.0, reported.
///
/// PLANT: Ogg's FLAC read as written, its line missing; the comment past
/// `0x3FFFF`, LFE2's line missing.
#[test]
fn l130_ogg_reads_flacs_layout_back_as_its_count_raw_flac_by_its_comment() {
  let Some(corpus) = Corpus::new() else { return };
  let layouts = Layouts::of(&corpus);
  let three = Some(LayoutReadBack::Native { mask: 0x7 });
  layout_law(
    &layouts.flac,
    &output(&corpus, "l130.ogg"),
    "ogg",
    three.clone(),
  );
  layout_law(
    &layouts.flac,
    &output(&corpus, "l130-2.1.flac"),
    "flac",
    None,
  );
  layout_law(
    &layouts.flac_lfe2,
    &output(&corpus, "l130-lfe2.flac"),
    "flac",
    three,
  );
}

/// **L131 — FLV reads FLAC's layout back from its multichannel
/// configuration, and PCM's mono or stereo from its flag** (R10, L1; table
/// E; `flvenc.c` 722–761, 910–913; `flvdec.c` 1657–1669, 1706–1745): FLAC's
/// 2.1 and its LFE2, whose bit FLV keeps, as written; PCM's stereo as
/// written — FLV's other changes are the unit law's, past what FLAC's own
/// files carry.
///
/// PLANT: FLV's FLAC mask cut to its first 18 bits, LFE2's line added.
#[test]
fn l131_flv_reads_flacs_layout_back_from_its_multichannel_configuration() {
  let Some(corpus) = Corpus::new() else { return };
  let layouts = Layouts::of(&corpus);
  for (source, name) in [
    (&layouts.flac, "flac"),
    (&layouts.flac_lfe2, "flac-lfe2"),
    (&layouts.stereo_44, "pcm"),
  ] {
    layout_law(
      source,
      &output(&corpus, &format!("l131-{name}.flv")),
      "flv",
      None,
    );
  }
}

/// **L132 — ISMV of a second video track is refused at open, booked to
/// Q870** (R11, Codex R10 2; `movenc.c` 7396–7471): each video track's key
/// frame flushes every track's fragment where the track holds a sample,
/// and whether it does before `check_pkt`'s second count of a packet the
/// negative composition offsets moved rests on the order libavformat's
/// interleaving hands the tracks' packets on, which this release does not
/// mirror. Two video tracks are refused by the second's name
/// (`MuxOpenError::TrackLayout`) before anything is written; one video
/// track beside an audio track opens, its packets under L123's rule.
///
/// PLANT: ISMV's second video track admitted, the first table opened.
#[test]
fn l132_ismv_of_a_second_video_track_is_refused_at_open() {
  let Some(corpus) = Corpus::new() else { return };
  let source = h264_aac(&corpus, "h264-aac.mp4");
  let video = || table_of(&source, TrackKind::Video).remove(0);
  let sound = || table_of(&source, TrackKind::Audio).remove(0);
  let open = |name: &str, table| {
    let target = output(&corpus, name);
    let result = FfmpegOwnedMuxer::open(
      File::create(&target).expect("create"),
      ContainerName::new("ismv"),
      table,
      &[],
      PacketLimits::default(),
    );
    (target, result.map(drop))
  };
  let (target, result) = open("l132-two.ismv", vec![sound(), video(), video()]);
  assert!(
    matches!(
      result,
      Err(MuxOpenError::TrackLayout { track }) if track == TrackIndex::new(2)
    ),
    "{result:?}"
  );
  assert_eq!(std::fs::metadata(&target).expect("the file").len(), 0);
  let (_, result) = open("l132-one.ismv", vec![sound(), video()]);
  result.expect("one video track");
}

/// `table` opened into FLV at `name`, the file empty where it is refused.
fn opened_flv(
  corpus: &Corpus,
  name: &str,
  table: Vec<TrackDescription<Ffmpeg, FfmpegBytes, CodecTicket>>,
) -> Result<(), MuxOpenError> {
  let target = output(corpus, name);
  let opened = FfmpegOwnedMuxer::open(
    File::create(&target).expect("create"),
    ContainerName::new("flv"),
    table,
    &[],
    PacketLimits::default(),
  )
  .map(drop);
  assert_eq!(
    std::fs::metadata(&target).expect("the file").len() == 0,
    opened.is_err(),
    "{name}: nothing written where refused"
  );
  opened
}

/// **L133 — FLV refuses at open, by name, the audio its start refuses**
/// (R11, ruling 4; `get_audio_flags`, `flvenc.c` 141–240; `flv_init`,
/// 1050–1051): a rate its flags' table names no code for — PCM at 48 and
/// 32 kHz, MP3 at 32 and 16 kHz, Nellymoser at 48 kHz, Speex at 8 kHz
/// (`MuxOpenError::ConfigurationInvalid`) — and Speex of two channels
/// (`MuxOpenError::ChannelLayout`), each before anything is written, where
/// the header would fail opaquely. MP3 at 48 kHz, under 44.1's code,
/// Nellymoser at 16 kHz, its own, and Speex mono at 16 kHz open.
///
/// PLANT: MP3's 48 kHz refused, its file refused; a rate past the table
/// taken, PCM's 32 kHz header failing; Speex's rate or channels unjudged,
/// the header failing.
#[test]
fn l133_flv_refuses_at_open_the_audio_its_start_refuses() {
  let Some(corpus) = Corpus::new() else { return };
  #[rustfmt::skip]
  let pcm_at = |name: &str, rate: &str, channels: &str| generated(&corpus, name, &[
    "-f", "lavfi", "-i", &format!("sine=frequency=440:sample_rate={rate}:duration=0.3"),
    "-ac", channels, "-c:a", "pcm_s16le",
  ]);
  #[rustfmt::skip]
  let mp3_at = |name: &str, rate: &str| generated(&corpus, name, &[
    "-f", "lavfi", "-i", &format!("sine=frequency=440:sample_rate={rate}:duration=0.3"),
    "-ac", "1", "-c:a", "libmp3lame",
  ]);
  let audio =
    |path: &Path, channels: i32| relaid(table_of(path, TrackKind::Audio).remove(0), channels);
  let as_codec = |path: &Path, channels: i32, codec| recoded(audio(path, channels), codec, &[]);
  use ffmpeg_next::ffi::AVCodecID::{
    AV_CODEC_ID_NELLYMOSER as NELLYMOSER, AV_CODEC_ID_SPEEX as SPEEX,
  };
  let invalid = |name: &str, row| {
    let opened = opened_flv(&corpus, name, vec![row]);
    assert!(
      matches!(opened, Err(MuxOpenError::ConfigurationInvalid { track, .. }) if track.get() == 0),
      "{name}: {opened:?}"
    );
  };
  invalid(
    "l133-pcm-48k.flv",
    audio(&pcm_at("pcm-48k.mkv", "48000", "2"), 2),
  );
  invalid(
    "l133-pcm-32k.flv",
    audio(&pcm_at("pcm-32k.mkv", "32000", "2"), 2),
  );
  invalid(
    "l133-mp3-32k.flv",
    audio(&mp3_at("mp3-32k.mp3", "32000"), 1),
  );
  invalid(
    "l133-mp3-16k.flv",
    audio(&mp3_at("mp3-16k.mp3", "16000"), 1),
  );
  invalid(
    "l133-nelly-48k.flv",
    as_codec(&pcm_at("pcm-48k-1.mkv", "48000", "1"), 1, NELLYMOSER),
  );
  invalid(
    "l133-speex-8k.flv",
    as_codec(&pcm_at("pcm-8k.mkv", "8000", "1"), 1, SPEEX),
  );
  let stereo_speex = opened_flv(
    &corpus,
    "l133-speex-stereo.flv",
    vec![as_codec(&pcm_at("pcm-16k-2.mkv", "16000", "2"), 2, SPEEX)],
  );
  assert!(
    matches!(stereo_speex, Err(MuxOpenError::ChannelLayout { track }) if track.get() == 0),
    "{stereo_speex:?}"
  );
  for (name, row) in [
    (
      "l133-mp3-48k.flv",
      audio(&mp3_at("mp3-48k.mp3", "48000"), 1),
    ),
    (
      "l133-nelly-16k.flv",
      as_codec(&pcm_at("pcm-16k-1.mkv", "16000", "1"), 1, NELLYMOSER),
    ),
    (
      "l133-speex-16k.flv",
      as_codec(&pcm_at("pcm-16k-1.mkv", "16000", "1"), 1, SPEEX),
    ),
  ] {
    opened_flv(&corpus, name, vec![row]).unwrap_or_else(|error| panic!("{name}: {error:?}"));
  }
}

/// **L134 — FLV refuses at open, by name, the video and the tables its
/// start refuses** (R11, ruling 4; `flv_init`, `flvenc.c` 985–1046):
/// MPEG-4 Part 2 and H.263, no part of FLV's specification, at the normal
/// compliance (`MuxOpenError::CodecNotCarried`); a video track past the
/// first of a codec its multitrack tags carry not — Sorenson H.263 after
/// H.264 — and an audio track past the first of a codec of no extended
/// header — PCM after PCM (`MuxOpenError::TrackLayout`), each by name
/// before anything is written. Sorenson H.263 opens, and before H.264; PCM
/// before AAC.
///
/// PLANT: MPEG-4 and H.263 unjudged, the header failing; the table
/// unjudged, the header failing; the first of a kind judged as a later one,
/// Sorenson H.263 before H.264 refused.
#[test]
fn l134_flv_refuses_at_open_the_video_and_the_tables_its_start_refuses() {
  let Some(corpus) = Corpus::new() else { return };
  let mpeg4 = picture_and_sound(
    &corpus,
    "mpeg4-pcm.mkv",
    &["-c:v", "mpeg4"],
    &["-c:a", "pcm_s16le", "-ac", "2"],
  );
  #[rustfmt::skip]
  let h263 = generated(&corpus, "h263.mkv", &[
    "-f", "lavfi", "-i", "testsrc2=size=352x288:rate=25:duration=0.2", "-c:v", "h263",
  ]);
  #[rustfmt::skip]
  let sorenson = generated(&corpus, "flv1.flv", &[
    "-f", "lavfi", "-i", "testsrc2=size=64x48:rate=25:duration=0.2", "-c:v", "flv",
  ]);
  #[rustfmt::skip]
  let pcm = generated(&corpus, "pcm-44k.mkv", &[
    "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=44100:duration=0.3",
    "-ac", "2", "-c:a", "pcm_s16le",
  ]);
  let source = h264_aac(&corpus, "h264-aac.mp4");
  let video = |path: &Path| table_of(path, TrackKind::Video).remove(0);
  let stereo = || relaid(table_of(&pcm, TrackKind::Audio).remove(0), 2);
  for (name, path, codec) in [
    ("l134-mpeg4.flv", &mpeg4, CodecId::MPEG4),
    (
      "l134-h263.flv",
      &h263,
      CodecId::from_raw(ffmpeg_next::ffi::AVCodecID::AV_CODEC_ID_H263 as i32),
    ),
  ] {
    let opened = opened_flv(&corpus, name, vec![video(path)]);
    assert!(
      matches!(opened, Err(MuxOpenError::CodecNotCarried { track, codec: refused }) if track.get() == 0 && refused == codec),
      "{name}: {opened:?}"
    );
  }
  for (name, table) in [
    ("l134-h264-flv1.flv", vec![video(&source), video(&sorenson)]),
    ("l134-pcm-pcm.flv", vec![stereo(), stereo()]),
  ] {
    let opened = opened_flv(&corpus, name, table);
    assert!(
      matches!(opened, Err(MuxOpenError::TrackLayout { track }) if track.get() == 1),
      "{name}: {opened:?}"
    );
  }
  for (name, table) in [
    ("l134-flv1.flv", vec![video(&sorenson)]),
    ("l134-flv1-h264.flv", vec![video(&sorenson), video(&source)]),
    (
      "l134-pcm-aac.flv",
      vec![stereo(), table_of(&source, TrackKind::Audio).remove(0)],
    ),
  ] {
    opened_flv(&corpus, name, table).unwrap_or_else(|error| panic!("{name}: {error:?}"));
  }
}

/// `row` declaring the native layout of `mask`, its ticket's layout alone
/// rewritten.
fn masked(row: Described, mask: u64) -> Described {
  reticketed(row, |par| {
    // SAFETY: `par` is live, its layout its own; `av_channel_layout_uninit`
    // frees any map and `av_channel_layout_from_mask` writes a native one in
    // place.
    let written = unsafe {
      ffmpeg_next::ffi::av_channel_layout_uninit(&mut par.ch_layout);
      ffmpeg_next::ffi::av_channel_layout_from_mask(&mut par.ch_layout, mask)
    };
    assert_eq!(written, 0, "a mask of {mask:#x}");
  })
}

/// **L135 — FLV refuses at open a native layout of no channel its
/// multichannel configuration keeps** (R11, Codex R10 1; table E;
/// `flvenc.c` 739–744, 910–913; `flvdec.c` 1733–1742): FLV writes the
/// configuration for an AAC or FLAC track of a layout neither mono nor
/// stereo, keeping a native mask's first 18 bits and the six from
/// `AV_CHAN_LOW_FREQUENCY_2`; of downmix's stereo, the wide pair or the
/// side surround pair it writes a mask of zero, which its reader refuses at
/// the first audio tag, the stream read back of no channels, which no
/// decoder takes — refused by name
/// (`MuxOpenError::ChannelLayout`) before anything is written, before
/// AAC's decoder could set a layout its own. The top side pair, which FLV
/// keeps, opens, and the file reads back, every packet in it.
///
/// PLANT: the reader's refusal unmirrored, the first table opened; the
/// six channels from `AV_CHAN_LOW_FREQUENCY_2` dropped from the kept mask,
/// the top side pair refused.
#[test]
fn l135_flv_refuses_a_native_layout_its_reader_cannot_take() {
  let Some(corpus) = Corpus::new() else { return };
  use ffmpeg_next::ffi::AVChannel as C;
  let bits = |channels: &[C]| -> u64 {
    channels
      .iter()
      .fold(0, |mask, &channel| mask | 1 << (channel as u32))
  };
  let source = h264_aac(&corpus, "h264-aac.mp4");
  let aac = || table_of(&source, TrackKind::Audio).remove(0);
  let layouts = Layouts::of(&corpus);
  let flac = || table_of(&layouts.flac, TrackKind::Audio).remove(0);
  for (name, row) in [
    (
      "l135-aac-downmix.flv",
      masked(
        aac(),
        bits(&[C::AV_CHAN_STEREO_LEFT, C::AV_CHAN_STEREO_RIGHT]),
      ),
    ),
    (
      "l135-aac-surround.flv",
      masked(
        aac(),
        bits(&[
          C::AV_CHAN_SIDE_SURROUND_LEFT,
          C::AV_CHAN_SIDE_SURROUND_RIGHT,
        ]),
      ),
    ),
    (
      "l135-flac-wide.flv",
      masked(
        flac(),
        bits(&[
          C::AV_CHAN_WIDE_LEFT,
          C::AV_CHAN_WIDE_RIGHT,
          C::AV_CHAN_SURROUND_DIRECT_LEFT,
        ]),
      ),
    ),
  ] {
    let opened = opened_flv(&corpus, name, vec![row]);
    assert!(
      matches!(opened, Err(MuxOpenError::ChannelLayout { track }) if track.get() == 0),
      "{name}: {opened:?}"
    );
  }
  let (table, packets) = whole_track(&source, TrackKind::Audio);
  let top = bits(&[C::AV_CHAN_TOP_SIDE_LEFT, C::AV_CHAN_TOP_SIDE_RIGHT]);
  let table = table.into_iter().map(|row| masked(row, top)).collect();
  let target = output(&corpus, "l135-aac-top.flv");
  written_through(&target, ContainerName::new("flv"), (table, packets.clone()));
  let (_, back) = whole_track(&target, TrackKind::Audio);
  assert_eq!(back.len(), packets.len(), "every packet read back");
}

/// `table` opened into `container` at `name`, the file empty where it is
/// refused.
fn opened_in(
  corpus: &Corpus,
  name: &str,
  container: &str,
  table: Vec<TrackDescription<Ffmpeg, FfmpegBytes, CodecTicket>>,
) -> Result<(), MuxOpenError> {
  let target = output(corpus, name);
  let opened = FfmpegOwnedMuxer::open(
    File::create(&target).expect("create"),
    ContainerName::new(container),
    table,
    &[],
    PacketLimits::default(),
  )
  .map(drop);
  assert_eq!(
    std::fs::metadata(&target).expect("the file").len() == 0,
    opened.is_err(),
    "{name}: nothing written where refused"
  );
  opened
}

/// **L136 — MOV refuses at open a video whose track aperture overflows**
/// (R11, Q869 L6; `mov_write_tapt_tag`, `movenc.c` 4098–4126, 4430–4436):
/// MOV writes a `tapt` for a video track whose aspect ratio is stated and
/// not 1, its coded width, height and display width each shifted left 16
/// in an `int` — H.264 at 20 000 pixels of an aspect ratio of 2, its
/// display width 40 000, and at a height of 40 000 of 4/3, refused by name
/// (`MuxOpenError::Dimensions`) before anything is written. At an aspect
/// ratio of 1 MOV writes none, and 40 000 pixels open; at a display width
/// of 32 000, and in MP4, which writes no `tapt`, 20 000 of 2 open.
///
/// PLANT: the aperture unjudged, the first table opened; the ratio of 1
/// judged too, the square 40 000 refused.
#[test]
fn l136_mov_refuses_a_video_whose_track_aperture_overflows() {
  let Some(corpus) = Corpus::new() else { return };
  let source = h264_aac(&corpus, "h264-aac.mp4");
  let video = |width: i32, height: i32, (num, den): (i32, i32)| {
    vec![reticketed(
      table_of(&source, TrackKind::Video).remove(0),
      |par| {
        par.width = width;
        par.height = height;
        par.sample_aspect_ratio.num = num;
        par.sample_aspect_ratio.den = den;
      },
    )]
  };
  for (name, table) in [
    ("l136-wide.mov", video(20_000, 64, (2, 1))),
    ("l136-tall.mov", video(64, 40_000, (4, 3))),
  ] {
    let opened = opened_in(&corpus, name, "mov", table);
    assert!(
      matches!(opened, Err(MuxOpenError::Dimensions { track }) if track.get() == 0),
      "{name}: {opened:?}"
    );
  }
  for (name, container, table) in [
    ("l136-square.mov", "mov", video(40_000, 64, (1, 1))),
    ("l136-display.mov", "mov", video(16_000, 64, (2, 1))),
    ("l136-wide.mp4", "mp4", video(20_000, 64, (2, 1))),
  ] {
    opened_in(&corpus, name, container, table).unwrap_or_else(|error| panic!("{name}: {error:?}"));
  }
}

/// **L137 — a stream's aspect ratio its parameters state not reaches the
/// ticket, and round-trips** (R11, Q869 L11; `mov.c`'s `pasp` and
/// `matroskadec.c`'s display size, kept on the stream;
/// `av_guess_sample_aspect_ratio`, `avformat.c` 784–805): ProRes's decoder
/// states no ratio, so a MOV's `pasp` of 10/11 lives on the stream alone;
/// the base's demuxer takes it into the ticket where the parameters state
/// none, and the muxer writes it — into Matroska as the display size, read
/// back 10/11. VP9, a codec MP4 admits, from WebM's display size, into
/// MP4's `pasp`, the same.
///
/// PLANT: the stream's ratio not taken, each source's ticket of none.
#[test]
fn l137_a_streams_aspect_ratio_reaches_the_ticket_and_round_trips() {
  let Some(corpus) = Corpus::new() else { return };
  #[rustfmt::skip]
  let prores = generated(&corpus, "prores-sar.mov", &[
    "-f", "lavfi", "-i", "testsrc2=size=64x48:rate=25", "-frames:v", "3",
    "-c:v", "prores_ks", "-vf", "setsar=10/11",
  ]);
  #[rustfmt::skip]
  let vp9 = generated(&corpus, "vp9-sar.webm", &[
    "-f", "lavfi", "-i", "testsrc2=size=64x48:rate=25", "-frames:v", "3",
    "-c:v", "libvpx-vp9", "-aspect", "16:9",
  ]);
  let ratio = |path: &Path| {
    let ratio = table_of(path, TrackKind::Video)
      .remove(0)
      .side_data()
      .sample_aspect_ratio();
    (ratio.num(), ratio.den())
  };
  for (source, container, name, stated) in [
    (&prores, "matroska", "l137-prores.mkv", (10, 11)),
    (&vp9, "mp4", "l137-vp9.mp4", (85, 64)),
  ] {
    assert_eq!(ratio(source), stated, "{name}: the source's ticket");
    let target = output(&corpus, name);
    remux_through_ffmpeg(source, ContainerName::new(container), &target);
    assert_eq!(ratio(&target), stated, "{name}: read back");
  }
}

/// `packet` on track 0 at `samples` of a 48 kHz clock, both times.
fn audio_at_sample(packet: &Audio, samples: i64) -> AudioTrackPacket<Ffmpeg, FfmpegBytes> {
  let clock = Timebase::new(1, NonZeroI32::new(48_000).expect("non-zero"));
  AudioTrackPacket::new(
    TrackIndex::new(0),
    packet
      .clone()
      .with_pts(Some(Timestamp::new(samples, clock)))
      .with_dts(Some(Timestamp::new(samples, clock))),
  )
}

/// **L138 — MP4 keeps a PCM packet's time as its place, and reports the
/// timeline it moves** (R11, Q869 L7; `mov_write_stts_tag`, `movenc.c`
/// 3241–3256; `mov_build_index`, `mov.c` 5015–5100): MP4's sample table
/// counts every PCM sample one tick from the track's first, so that a
/// packet's time is the samples before it — a second's gap after the
/// second packet is gone, the third packet and every later one read back
/// at their places, a second early. Reported at that packet, once, as the
/// timeline it moves (`Rewrite::Timeline`), not as the durations: each
/// packet's duration, its samples, is kept. The packets that abut report
/// nothing; the samples read back whole.
///
/// PLANT: the place unjudged, the line missing; the durations judged by
/// the packets' abutting, `Durations` reported instead.
#[test]
fn l138_mp4_keeps_a_pcm_packets_time_as_its_place() {
  let Some(corpus) = Corpus::new() else { return };
  let source = pcm_mov(&corpus);
  let (table, audio) = first_track(&source, TrackKind::Audio, 4);
  let audio: Vec<Audio> = audio.iter().map(audio_of).collect();
  // Stereo 16-bit PCM: four bytes a sample.
  let samples = |packet: &Audio| packet.data().as_ref().len() as i64 / 4;
  let target = output(&corpus, "l138.mp4");
  let mut muxer = muxer_into(&target, ContainerName::new("mp4"), table);
  let timeline = |report: &mediaencode_ffmpeg::MuxReport| {
    lines(report, |line| {
      matches!(
        line,
        Uncarried::Rewritten {
          rewrite: Rewrite::Timeline | Rewrite::Durations,
          ..
        }
      )
    })
  };
  let mut place = 0;
  for (index, packet) in audio.iter().enumerate() {
    let at = if index >= 2 { place + 48_000 } else { place };
    muxer
      .write_audio(audio_at_sample(packet, at))
      .map_err(|error| error.map_packet(drop))
      .unwrap_or_else(|error| panic!("packet {index}: {error:?}"));
    if index == 1 {
      assert!(timeline(muxer.report()).is_empty(), "abutting: nothing");
    }
    place += samples(packet);
  }
  assert_eq!(
    timeline(muxer.report()),
    vec![Uncarried::Rewritten {
      track: TrackIndex::new(0),
      rewrite: Rewrite::Timeline,
    }]
  );
  muxer.finish().expect("finished");
  let back = &packets(&target)[0];
  let offered: Vec<u8> = audio
    .iter()
    .flat_map(|packet| packet.data().as_ref().to_vec())
    .collect();
  let read: Vec<u8> = back.iter().flat_map(|seen| seen.bytes.clone()).collect();
  assert!(read == offered, "the samples whole");
  let end = back.last().and_then(|seen| seen.pts).expect("a time");
  let last_samples = back.last().map_or(0, |seen| seen.bytes.len() as i64 / 4);
  assert_eq!(
    end
      .checked_rescale_with(
        Timebase::new(1, NonZeroI32::new(48_000).expect("non-zero")),
        Rounding::Nearest
      )
      .map(|time| time.pts() + last_samples),
    Some(place),
    "the gap gone: the last sample at the place, not a second later"
  );
}

/// **L139 — MP4 refuses the PCM packet that would take its samples past
/// what it counts them in** (R11, Q869 L7; `mov_write_stsz_tag`,
/// `movenc.c` 228–241): MP4 sums a PCM track's samples in an `int`, a
/// signed overflow past `INT_MAX` — 12.4 hours at 48 kHz. With the bound
/// lowered to the first two packets' samples, the third is refused by name
/// (`PayloadRule::SampleCount`), untouched, and the file finishes with the
/// two.
///
/// PLANT: the count unjudged, the third taken.
#[test]
fn l139_mp4_refuses_pcm_samples_past_what_it_counts_them_in() {
  let Some(corpus) = Corpus::new() else { return };
  let source = pcm_mov(&corpus);
  let (table, audio) = first_track(&source, TrackKind::Audio, 3);
  let audio: Vec<Audio> = audio.iter().map(audio_of).collect();
  let samples = |packet: &Audio| packet.data().as_ref().len() as u64 / 4;
  let bound = samples(&audio[0]) + samples(&audio[1]);
  let target = output(&corpus, "l139.mp4");
  let mut muxer = muxer_with(
    &target,
    ContainerName::new("mp4"),
    table,
    MuxOptions::new().with_lowered_sample_bound(bound),
  );
  let mut place = 0;
  for packet in &audio[..2] {
    muxer
      .write_audio(audio_at_sample(packet, place))
      .map_err(|error| error.map_packet(drop))
      .expect("within the bound");
    place += samples(packet) as i64;
  }
  let offered = audio_at_sample(&audio[2], place);
  let (refusal, back) = by_container(muxer.write_audio(offered.clone()));
  assert_eq!(refusal, payload(PayloadRule::SampleCount));
  untouched!(offered, back);
  muxer.finish().expect("finished");
  let read: usize = packets(&target)[0]
    .iter()
    .map(|seen| seen.bytes.len())
    .sum();
  assert_eq!(read as u64, bound * 4, "the two packets' samples");
}

/// An APV access unit of one primary frame, 16 by 16 pixels of 4:2:2 at
/// 10 bits, its capture time distance `distance`: the signature, and one
/// PBU — its header, the frame header with its frame information, one
/// tile of 16 by 8 macroblocks, its header and a byte a component — as
/// `cbs_apv_syntax_template.c` reads them (19–330).
fn apv_au(distance: u8) -> Vec<u8> {
  let mut bits: Vec<bool> = Vec::new();
  let mut put = |value: u64, count: u32| {
    for bit in (0..count).rev() {
      bits.push((value >> bit) & 1 == 1);
    }
  };
  // The frame information.
  put(33, 8); // profile_idc
  put(30, 8); // level_idc
  put(0, 3); // band_idc
  put(0, 5); // reserved_zero_5bits
  put(16, 24); // frame_width
  put(16, 24); // frame_height
  put(2, 4); // chroma_format_idc: 4:2:2
  put(2, 4); // bit_depth_minus8: 10 bits
  put(u64::from(distance), 8); // capture_time_distance
  put(0, 8); // reserved_zero_8bits
  // The rest of the frame header.
  put(0, 8); // reserved_zero_8bits
  put(0, 1); // color_description_present_flag
  put(0, 1); // use_q_matrix
  put(16, 20); // tile_width_in_mbs
  put(8, 20); // tile_height_in_mbs
  put(0, 1); // tile_size_present_in_fh_flag
  put(0, 8); // reserved_zero_8bits_2
  while bits.len() % 8 != 0 {
    bits.push(false);
  }
  let header: Vec<u8> = bits
    .chunks(8)
    .map(|byte| byte.iter().fold(0_u8, |acc, &bit| acc << 1 | u8::from(bit)))
    .collect();
  // One tile: its size, its header for three components, a byte each.
  let mut tile = Vec::new();
  tile.extend_from_slice(&20_u16.to_be_bytes()); // tile_header_size
  tile.extend_from_slice(&0_u16.to_be_bytes()); // tile_index
  for _ in 0..3 {
    tile.extend_from_slice(&1_u32.to_be_bytes()); // tile_data_size
  }
  tile.extend_from_slice(&[22, 22, 22, 0]); // tile_qp, reserved_zero_8bits
  tile.extend_from_slice(&[0, 0, 0]);
  let mut pbu = vec![1, 0, 1, 0]; // pbu_type 1, group_id 1, reserved
  pbu.extend_from_slice(&header);
  pbu.extend_from_slice(&(tile.len() as u32).to_be_bytes());
  pbu.extend_from_slice(&tile);
  let mut au = b"aPv1".to_vec();
  au.extend_from_slice(&(pbu.len() as u32).to_be_bytes());
  au.extend_from_slice(&pbu);
  au
}

/// **L140 — MP4 refuses the APV packet whose frame would bring a 256th
/// frame information to its `apvC`** (R11, ruling 2; `ff_isom_parse_apvc`,
/// `apv.c` 233–318; `movenc.c` 7199, 1725–1738): MP4 records each distinct
/// frame information of a PBU type it parses out of the packets, and
/// counts them in 8 bits, its parse failing at the 256th, its answer
/// ignored, the record written without it. 255 frames of 255 capture time
/// distances are taken, and a repeated one; the frame of the 256th
/// distance is refused by name (`PayloadRule::ApvFrameInfo`), untouched;
/// the file's `apvC` reads back of one entry of 255 — FFmpeg's own parse
/// of each frame.
///
/// PLANT: the count unjudged, the 256th taken; the capture time distance
/// left out of the frame information, every frame one, the 256th taken.
#[test]
fn l140_mp4_refuses_an_apv_frame_past_its_configurations_count() {
  let Some(corpus) = Corpus::new() else { return };
  let source = h264_aac(&corpus, "h264-aac.mp4");
  let (table, video) = first_track(&source, TrackKind::Video, 1);
  let table: Vec<_> = table
    .into_iter()
    .map(|row| recoded(row, ffmpeg_next::ffi::AVCodecID::AV_CODEC_ID_APV, &[]))
    .collect();
  let carrier = video_of(&video[0]);
  let target = output(&corpus, "l140.mp4");
  let mut muxer = muxer_into(&target, ContainerName::new("mp4"), table);
  let frame = |at: i64, distance: u8| {
    VideoTrackPacket::new(
      TrackIndex::new(0),
      video_holding(&carrier, &apv_au(distance))
        .with_pts(Some(ms(at)))
        .with_dts(Some(ms(at)))
        .with_flags(PacketFlags::KEY),
    )
  };
  for distance in 0..=254_u8 {
    muxer
      .write_video(frame(i64::from(distance) * 40, distance))
      .map_err(|error| error.map_packet(drop))
      .unwrap_or_else(|error| panic!("distance {distance}: {error:?}"));
  }
  muxer
    .write_video(frame(255 * 40, 7))
    .map_err(|error| error.map_packet(drop))
    .expect("a frame information recorded already");
  let offered = frame(256 * 40, 255);
  let (refusal, back) = by_container(muxer.write_video(offered.clone()));
  assert_eq!(refusal, payload(PayloadRule::ApvFrameInfo));
  untouched!(offered, back);
  muxer.finish().expect("finished");
  let back = table_of(&target, TrackKind::Video).remove(0);
  let record = back.side_data().extradata().to_vec();
  // The box's version and flags, its configuration version, its one entry:
  // the primary frame's type and its count of frame informations.
  assert_eq!(&record[..4], &[0, 0, 0, 0], "{record:?}");
  assert_eq!(record[4], 1, "configurationVersion");
  assert_eq!(record[5], 1, "one entry");
  assert_eq!((record[6], record[7]), (1, 255), "the primary frame's 255");
}

/// An HEVC PPS of `layer`.
fn hevc_pps_of(layer: u8) -> Vec<u8> {
  let mut pps = hevc_pps();
  pps[..2].copy_from_slice(&hevc_header(34, layer));
  pps
}

/// `AV_DISPOSITION_MULTILAYER`, which the base's flags name not.
fn multilayer() -> TrackDisposition {
  TrackDisposition::from_bits_retain(1 << 21)
}

/// An HEVC configuration law over the corpus's track recoded to HEVC, of
/// the tag `tag`, flagged multilayer where `flagged`.
fn hevc_layer_law(
  corpus: &Corpus,
  law: &str,
  containers: &[(ContainerName, &str)],
  (tag, flagged): (&[u8; 4], bool),
  (base, packets): (&[u8], bool),
  cases: &[(&str, Vec<u8>, bool)],
) {
  let slice = start_coded(&[&escaped(&hevc_header(19, 0), &[0xAF, 0x88, 0x42])]);
  let (row, frames) = recoded_fixture(corpus, &slice);
  let track = |config: &[u8]| {
    let row = restated(
      &row,
      ffmpeg_next::ffi::AVCodecID::AV_CODEC_ID_HEVC,
      config,
      Some(tag),
      None,
    );
    if flagged {
      let flags = row.disposition() | multilayer();
      row.with_disposition(flags)
    } else {
      row
    }
  };
  configuration_law(
    corpus,
    law,
    containers,
    &track,
    (&frames, base, packets),
    cases,
  );
}

/// **L141 — HEVC's parameter sets of a layer `hvcC` drops are refused, but
/// where MP4 writes them into `lhvC`** (R11, ruling 1; `hevc.c` 888–889,
/// 1000–1013, 1408–1413; `movenc.c` 2893–2898, 1654–1674; `mov.c`
/// 8878–8925): `ff_isom_write_hvcc` keeps the base layer's units and the
/// alpha layer's a VPS extension names, of Annex B; of an `hvcC`, the base
/// layer's alone. A configuration of a second layer's SPS and PPS is
/// refused in Matroska, FLV and MP4 (`MuxOpenError::ConfigurationInvalid`,
/// `PayloadRule::NewExtradata`) — and taken where the layer is the alpha
/// layer its VPS names. MP4 of a stream flagged multilayer writes them into
/// `lhvC`, whose reader merges them back: taken, but under `hvc1` where the
/// layer holds no PPS, `lhvC`'s arrays then incomplete and not written.
///
/// PLANT: the layers' drop unjudged, Matroska's first case opened; `lhvC`
/// unmirrored, MP4's flagged layers refused.
#[test]
fn l141_hevcs_layers_hvcc_drops_are_refused_but_where_lhvc_takes_them() {
  let Some(corpus) = Corpus::new() else { return };
  let layered = start_coded(&[
    &hevc_vps(0, None),
    &hevc_sps(0, 0, 1, None),
    &hevc_pps(),
    &hevc_sps(1, 0, 0, None),
    &hevc_pps_of(1),
  ]);
  let alpha = start_coded(&[
    &hevc_vps(0, Some(1)),
    &hevc_sps(0, 0, 1, None),
    &hevc_pps(),
    &hevc_sps(1, 0, 0, None),
    &hevc_pps_of(1),
  ]);
  let sps_only = start_coded(&[
    &hevc_vps(0, None),
    &hevc_sps(0, 0, 1, None),
    &hevc_pps(),
    &hevc_sps(1, 0, 0, None),
  ]);
  let record = hvcc_record(&[
    (32, vec![hevc_vps(0, None)]),
    (33, vec![hevc_sps(0, 0, 1, None), hevc_sps(1, 0, 0, None)]),
    (34, vec![hevc_pps(), hevc_pps_of(1)]),
  ]);
  let base = hevc_base();
  let base_record = hevc_record(1, 1, 1);
  let annex_b = |flagged_hvc1: Option<bool>| -> Vec<(&str, Vec<u8>, bool)> {
    vec![
      ("a second layer", layered.clone(), flagged_hvc1.is_none()),
      ("the alpha layer", alpha.clone(), false),
      (
        "a second layer's SPS alone",
        sps_only.clone(),
        flagged_hvc1 != Some(false),
      ),
    ]
  };
  // Matroska, FLV, and MP4 of no multilayer flag: the layer lost.
  hevc_layer_law(
    &corpus,
    "l141-mkv",
    &matroska(),
    (b"hev1", false),
    (&base, false),
    &annex_b(None),
  );
  hevc_layer_law(
    &corpus,
    "l141",
    &mp4_and_flv(),
    (b"hev1", false),
    (&base, true),
    &annex_b(None),
  );
  hevc_layer_law(
    &corpus,
    "l141-record",
    &mp4_and_flv(),
    (b"hev1", false),
    (&base_record, true),
    &[("an hvcC of a second layer", record.clone(), true)],
  );
  // MP4 flagged multilayer: `lhvC` takes the layer; under `hvc1` not of an
  // SPS alone. The packet road's track states a second layer first, an
  // `lhvC` of no unit refused (below).
  for (tag, hvc1) in [(b"hev1", false), (b"hvc1", true)] {
    hevc_layer_law(
      &corpus,
      &format!("l141-multilayer-{}", u8::from(hvc1)),
      &[(ContainerName::MP4, "mp4")],
      (tag, true),
      (&layered, true),
      &annex_b(Some(hvc1)),
    );
    hevc_layer_law(
      &corpus,
      &format!("l141-multilayer-record-{}", u8::from(hvc1)),
      &[(ContainerName::MP4, "mp4")],
      (tag, true),
      (&record, true),
      &[("an hvcC of a second layer", record.clone(), false)],
    );
  }
  // Read back: the configuration holds the second layer's units, merged
  // from `lhvC`, and the stream is flagged multilayer.
  let target = output(&corpus, "l141-multilayer-0-0.mp4");
  let back = table_of(&target, TrackKind::Video).remove(0);
  assert!(back.disposition().contains(multilayer()), "flagged");
  let config = back.side_data().extradata().to_vec();
  let layer_one = hevc_pps_of(1);
  assert!(
    config
      .windows(layer_one.len())
      .any(|window| window == layer_one.as_slice()),
    "the second layer's PPS in the configuration read back"
  );
  // The flag alone, of no second layer: under `hvc1` `lhvC` is not written,
  // the flag lost, reported, the file read back unflagged; under `hev1` it
  // is written of no array, which FFmpeg's reader refuses, the file with it
  // (`mov.c` 8907–8914, 2248–2253): refused.
  let slice = start_coded(&[&escaped(&hevc_header(19, 0), &[0xAF, 0x88, 0x42])]);
  let (row, frames) = recoded_fixture(&corpus, &slice);
  let flagged = |tag: &[u8; 4]| {
    let row = restated(
      &row,
      ffmpeg_next::ffi::AVCodecID::AV_CODEC_ID_HEVC,
      &base,
      Some(tag),
      None,
    );
    let flags = row.disposition() | multilayer();
    row.with_disposition(flags)
  };
  let target = output(&corpus, "l141-flag.mp4");
  let mut muxer = muxer_into(&target, ContainerName::MP4, vec![flagged(b"hvc1")]);
  let dropped = lines(
    muxer.report(),
    |line| matches!(line, Uncarried::Disposition { flags, .. } if flags.contains(multilayer())),
  );
  assert_eq!(dropped.len(), 1, "{dropped:?}");
  for frame in &frames {
    muxer
      .write_video(VideoTrackPacket::new(TrackIndex::new(0), frame.clone()))
      .map_err(|error| error.map_packet(drop))
      .expect("a frame");
  }
  muxer.finish().expect("finished");
  assert!(
    !table_of(&target, TrackKind::Video)
      .remove(0)
      .disposition()
      .contains(multilayer()),
    "read back unflagged"
  );
  let opened = opened_in(
    &corpus,
    "l141-flag-empty.mp4",
    "mp4",
    vec![flagged(b"hev1")],
  );
  assert!(
    matches!(opened, Err(MuxOpenError::ConfigurationInvalid { track, .. }) if track.get() == 0),
    "{opened:?}"
  );
}

/// **L142 — Matroska's `A_MS/ACM` road is not reached** (R11; `mkv_query_codec`,
/// `matroskaenc.c` 3699–3713; `mkv_write_track`, 2025–2035, 2125–2127):
/// Matroska writes an audio codec it names not behind `A_MS/ACM`, in
/// RIFF's format, and its query admits such a codec only below the normal
/// compliance, which the context keeps. Every audio codec libavcodec
/// describes, its ticket a PCM track's recoded, is opened into Matroska:
/// refused, or its header written under a codec id of Matroska's own —
/// none behind `A_MS/ACM`. A release whose query admits one turns it red,
/// for the road's audit.
///
/// PLANT: the codec query's answer unread, a codec Matroska names not
/// admitted.
#[test]
fn l142_matroskas_acm_road_is_not_reached() {
  let Some(corpus) = Corpus::new() else { return };
  let pcm = pcm_mkv(&corpus);
  let mut opened = 0;
  let mut descriptor = core::ptr::null();
  loop {
    // SAFETY: `avcodec_descriptor_next` walks libavcodec's static table,
    // each entry live for the program; its `id` and `type_` are the
    // table's own values.
    let (id, kind) = unsafe {
      descriptor = ffmpeg_next::ffi::avcodec_descriptor_next(descriptor);
      if descriptor.is_null() {
        break;
      }
      ((*descriptor).id, (*descriptor).type_)
    };
    if kind != ffmpeg_next::ffi::AVMediaType::AVMEDIA_TYPE_AUDIO {
      continue;
    }
    let target = output(&corpus, &format!("l142-{}.mkv", id as i32));
    let result = FfmpegOwnedMuxer::open(
      File::create(&target).expect("create"),
      ContainerName::MATROSKA,
      vec![recoded(table_of(&pcm, TrackKind::Audio).remove(0), id, &[])],
      &[],
      PacketLimits::default(),
    );
    if let Ok((muxer, _)) = result {
      drop(muxer);
      opened += 1;
      let header = std::fs::read(&target).expect("the header");
      assert!(
        !header.windows(8).any(|window| window == b"A_MS/ACM"),
        "{id:?}: written behind A_MS/ACM"
      );
    }
  }
  assert!(
    opened > 10,
    "the premise: Matroska's own audio codecs open, {opened}"
  );
}
