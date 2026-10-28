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
  AutomaticFilter, ContainerName, FfmpegMuxRefusal, FfmpegMuxer, FfmpegOwnedMuxer, MuxOpenError,
  MuxOptions, MuxStage, OutputError, OutputText, PayloadRule, Rewrite, SideDataFate, Uncarried,
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
  let description = table.remove(cover);
  let disposition = description.disposition();
  let (timebase, params, _, cover_ticket, _, file) = description.into_parts();
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
/// (`movenc.c` 6925–6928), and SRT drops the cue (`srtenc.c` 71–75):
/// refused, untouched, the file holding only what was admitted.
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
  for (name, container, table, cue) in [
    (
      "l11-cue.mp4",
      ContainerName::MP4,
      mov_table,
      cue_on(0, &subtitle_of(&mov_cues[0]), base, (0, -1)),
    ),
    (
      "l11-cue.srt",
      ContainerName::new("srt"),
      table_of(&source, TrackKind::Subtitle),
      subtitle_at(&cue, 0, -40),
    ),
  ] {
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
    // SRT writes nothing of its own; MP4's trailer writes the empty sample
    // of a `mov_text` track that took no cue (`movenc.c` 9025–9031). The
    // cue's bytes are nowhere.
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

/// **L16 — a packet of a codec of key frames alone, not marked one, is
/// refused by name** (table G), which libavformat would mark (`mux.c`
/// 820–821) over the packet's word that it is no seek point: MP3 and PCM
/// into MP4 and into Matroska. An MP3 frame whose bit reservoir starts in
/// the frame before decodes, where decoding starts at it, to silence
/// (`mpegaudiodec_template.c` 1316–1322): the mark would make a seek point
/// that does not decode. Matroska writes every subtitle block as a key
/// frame, so there a subtitle's flag is not kept either way, and the cue is
/// admitted.
///
/// PLANT: the refusal dropped takes the unmarked frames.
#[test]
fn l16_an_unmarked_key_frame_is_refused_by_name() {
  let Some(corpus) = Corpus::new() else { return };
  for (source, at, name) in [
    (pcm_mov(&corpus), 10, "pcm"),
    (audio_file(&corpus, "l16.mp3", "libmp3lame"), 24, "mp3"),
  ] {
    let (_, packets) = first_track(&source, TrackKind::Audio, 2);
    for (container, name) in [
      (ContainerName::MP4, format!("l16-{name}.mp4")),
      (ContainerName::MATROSKA, format!("l16-{name}.mkv")),
    ] {
      let mut muxer = muxer_into(
        &output(&corpus, &name),
        container,
        table_of(&source, TrackKind::Audio),
      );
      muxer
        .write_audio(audio_at(&audio_of(&packets[0]), 0))
        .expect("a key frame");
      let unmarked = AudioTrackPacket::new(
        TrackIndex::new(0),
        audio_of(&packets[1])
          .with_pts(Some(ms(at)))
          .with_dts(Some(ms(at)))
          .with_flags(PacketFlags::empty()),
      );
      let (refusal, back) = by_container(muxer.write_audio(unmarked.clone()));
      assert_eq!(
        refusal,
        FfmpegMuxRefusal::KeyFrameUnmarked {
          track: TrackIndex::new(0)
        },
        "{name}"
      );
      untouched!(unmarked, back);
      muxer.finish().expect("finished");
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
    .expect("Matroska writes a subtitle as a key frame regardless");
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
/// without one into MP4, AV1 without one into WebM. AAC's is refused only
/// with the automatic filters off (L29).
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
  let (av1, _) = first_track(&av1_mkv(&corpus), TrackKind::Video, 1);
  let av1 = av1
    .into_iter()
    .map(|row| row.with_extradata(none()))
    .collect();
  assert!(matches!(
    open("l26.webm", ContainerName::WEBM, av1),
    Err(MuxOpenError::ConfigurationMissing { codec, .. }) if codec == CodecId::AV1
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
/// as `ffmpeg -c copy` does, and the report names it for the track. Each
/// frame reads back as its raw AAC. With the filters off nothing makes the
/// configuration, and the track is refused at open by name, nothing
/// written.
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
    assert!(
      report.uncarried().contains(&Uncarried::Filtered {
        track,
        filter: AutomaticFilter::AacAdtsToAsc
      }),
      "{name}: {report:?}"
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
    let refused = FfmpegOwnedMuxer::open_with(
      File::create(&target).expect("create"),
      container,
      table_of(&ts, TrackKind::Audio),
      &[],
      filters(false),
    );
    assert!(
      matches!(
        refused,
        Err(MuxOpenError::ConfigurationMissing { track, codec })
          if track.get() == 0 && codec == CodecId::AAC
      ),
      "{name}"
    );
    assert_eq!(
      std::fs::metadata(&target).expect("the file").len(),
      0,
      "{name}: nothing was written"
    );
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
/// TTML's header strings from libavcodec's encoder cut short; and FLAC,
/// Opus and AV1 with none. Each track opens with its own.
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
  assert!(matches!(
    open(with(&flac, TrackKind::Audio, Some(&[]))),
    Err(MuxOpenError::ConfigurationMissing { codec, .. }) if codec == CodecId::FLAC
  ));

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

  assert!(matches!(
    open(with(&av1_mkv(&corpus), TrackKind::Video, Some(&[]))),
    Err(MuxOpenError::ConfigurationMissing { codec, .. }) if codec == CodecId::AV1
  ));

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
/// libavcodec does not parse, or that is larger than the space Matroska's
/// header reserved. Each container takes what it writes whole.
///
/// PLANT: without the check of new extradata, the Annex B on an `avcC`
/// track is taken and MP4 rewrites the packets after it.
#[test]
fn l43_new_extradata_a_container_would_refuse_is_refused() {
  use ffmpeg_next::ffi::AVPacketSideDataType as T;
  let Some(corpus) = Corpus::new() else { return };
  let new_extradata = |bytes: &[u8]| vec![side(T::AV_PKT_DATA_NEW_EXTRADATA, bytes)];
  let audio_with = |packet: &Audio, at: i64, entries: Vec<SideDataEntry>| {
    let mut inner = packet.clone().with_pts(Some(ms(at))).with_dts(Some(ms(at)));
    inner.extra_mut().set_side_data(entries);
    AudioTrackPacket::new(TrackIndex::new(0), inner)
  };

  let opus = opus_mkv(&corpus);
  let (table, frames) = first_track(&opus, TrackKind::Audio, 2);
  let own = table[0].side_data().extradata().to_vec();
  let mut muxer = muxer_into(&output(&corpus, "l43-opus.mp4"), ContainerName::MP4, table);
  let offered = audio_with(&audio_of(&frames[0]), 0, new_extradata(b"OpusHead"));
  let (refusal, back) = by_container(muxer.write_audio(offered.clone()));
  assert_eq!(refusal, payload(PayloadRule::NewExtradata));
  untouched!(offered, back);
  muxer
    .write_audio(audio_with(&audio_of(&frames[0]), 0, new_extradata(&own)))
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
  let offered = audio_with(&audio_of(&frames[0]), 0, new_extradata(&[0; 33]));
  let (refusal, back) = by_container(muxer.write_audio(offered.clone()));
  assert_eq!(refusal, payload(PayloadRule::FlacStreamInfo));
  untouched!(offered, back);

  let (table, frames) = first_track(&source, TrackKind::Audio, 2);
  let reserved = table[0].side_data().extradata().len();
  assert!(
    reserved >= 2,
    "the premise: the track's AudioSpecificConfig"
  );
  // AAC LC at 48 kHz, stereo, its bytes after the first two read as zeros.
  let config = |len: usize| {
    let mut bytes = vec![0x11, 0x90];
    bytes.resize(len, 0);
    bytes
  };
  let mut muxer = muxer_into(
    &output(&corpus, "l43-aac.mkv"),
    ContainerName::MATROSKA,
    table,
  );
  // Channel configuration 15, past libavcodec's table; and a valid one
  // longer than the space Matroska's header reserved, the track's own.
  for bytes in [vec![0x11, 0xF8], config(reserved + 1)] {
    let offered = audio_with(&audio_of(&frames[0]), 0, new_extradata(&bytes));
    let (refusal, back) = by_container(muxer.write_audio(offered.clone()));
    assert_eq!(refusal, payload(PayloadRule::NewExtradata), "{bytes:?}");
    untouched!(offered, back);
  }
  muxer
    .write_audio(audio_with(
      &audio_of(&frames[0]),
      0,
      new_extradata(&config(reserved)),
    ))
    .map_err(|error| error.map_packet(drop))
    .expect("an AudioSpecificConfig within the space");
  muxer.finish().expect("finished");
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
    &[Uncarried::Filtered {
      track,
      filter: AutomaticFilter::Vp9Superframe
    }],
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
/// first packet inserts no filter, for good: an ADTS packet after it is
/// bytes.
///
/// PLANT: FLV taken off AAC's filter sites admits the cut element.
#[test]
fn l50_aacs_filter_is_mirrored_in_another_container() {
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
  muxer
    .write_audio(audio_at(&audio_holding(&frame, &[0x21, 0x10, 0x05]), 0))
    .map_err(|error| error.map_packet(drop))
    .expect("a raw first packet");
  muxer
    .write_audio(audio_at(
      &audio_holding(&frame, &adts_with(&BARE_PCE[..5])),
      21,
    ))
    .map_err(|error| error.map_packet(drop))
    .expect("no filter: bytes");
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
/// are dropped, as the report says at open** (table C″; [`Rewrite::Ac3Parsed`]):
/// MP4 frees the run it holds without writing it (`mov_free`, `movenc.c`
/// 7951–7955). Two six-block frames and three one-block ones read back as
/// the two.
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
  muxer.finish().expect("finished");
  assert_eq!(packets(&target)[0].len(), 2, "the two six-block frames");
}

// ---------------------------------------------------------------------------
//  E″: the filters FFmpeg decides, named where it decides them.
// ---------------------------------------------------------------------------

/// The filter lines of `report`.
fn filtered(report: &mediaencode_ffmpeg::MuxReport) -> Vec<Uncarried> {
  lines(report, |line| matches!(line, Uncarried::Filtered { .. }))
}

/// **L62 — FLV's and LATM's AAC filter is named for its track at the
/// packet that inserts it, once, and never where none is** (E″ of the
/// census): their `check_bitstream` inserts `aac_adtstoasc` at a track's
/// first packet where it opens with an ADTS header (`flvenc.c` 1491–1493;
/// `latmenc.c` 255–257; `mux.c` 1063–1068). A transport stream's ADTS
/// frames: nothing is named at open, one line at the first frame's write,
/// none more at the second. A configured track's raw AAC: none.
///
/// PLANT: without the transition's line, the report names no filter for
/// the frames it converts.
#[test]
fn l62_flv_and_latm_name_aacs_filter_at_the_packet_that_inserts_it() {
  let Some(corpus) = Corpus::new() else { return };
  let ts = corpus.transport_stream();
  let (_, frames) = first_track(&ts, TrackKind::Audio, 2);
  let mkv = audio_file(&corpus, "aac.mkv", "aac");
  let (_, raw) = first_track(&mkv, TrackKind::Audio, 1);
  let named = vec![Uncarried::Filtered {
    track: TrackIndex::new(0),
    filter: AutomaticFilter::AacAdtsToAsc,
  }];
  for container in ["flv", "latm"] {
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
/// 51–52). An ADTS first frame is taken, the filter named at its write, and
/// reads back as its raw payload; raw and ADTS packets after it are taken.
/// Matroska rewrites the track's configuration with the one the filter
/// makes of the first frame, within the space its header reserved — the
/// stated configuration's length — and refuses one past it after taking
/// the packet (`matroskaenc.c` 3065–3082, 1334–1365): a first frame whose
/// channels its program config element gives makes one of eight bytes,
/// refused, by name, untouched; MP4 makes it a sample description, and
/// takes it.
///
/// PLANT: a configured track taking raw AAC alone, as before: the ADTS
/// first frame is refused.
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

/// **L68 — MP4 reports an AC-3 or E-AC-3 track's new extradata dropped**
/// (the side-data table): MP4 writes their sample entry's configuration,
/// `dac3` or `dec3`, from its own parse of the frames (`movenc.c` 418–462,
/// 612–666; 1084–1087, 1507–1510), so the sample description a packet's new
/// extradata opens (7044–7074) holds none of its bytes. Reported for the
/// track, by name, at the first packet carrying it; nothing reads back with
/// them.
///
/// PLANT: MP4's new extradata kept for every codec again leaves the line
/// out.
#[test]
fn l68_mp4_reports_ac3s_new_extradata_dropped() {
  use ffmpeg_next::ffi::AVPacketSideDataType as T;
  let Some(corpus) = Corpus::new() else { return };
  let added = [1, 2, 3, 4];
  for codec in ["ac3", "eac3"] {
    let source = audio_file(&corpus, &format!("{codec}.mkv"), codec);
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
