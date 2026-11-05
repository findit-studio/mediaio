//! A remux written against the two faces alone — `mediadecode`'s
//! `Demuxer` and `mediaencode`'s `Muxer` — run over this crate's
//! demuxer on a real container.
//!
//! The write side's track conversion,
//! `TrackDescription::from_track_info`, used to take a row by value,
//! while `Demuxer::tracks` hands out shared handles on rows the session
//! keeps: a generic remux could not build its table from a live demuxer
//! at all. It borrows now, and this lane is the proof, written as a user
//! writes it: no backend type appears in [`remux`].
//!
//! The muxer here is [`Ledger`], a `Muxer` over this crate's adapter
//! that keeps the faces' refusals and records what it is given, so the
//! lane can say what reached it. The same remux ([`remux`], in the
//! shared support module) runs over the FFmpeg muxer in
//! `mediaencode-ffmpeg`'s `mux` lane.

mod support;

use std::{cell::RefCell, rc::Rc};

use mediadecode::{
  Timestamp,
  demuxer::{
    AudioTrackPacket, DataTrackPacket, DemuxedPacket, Demuxer, SubtitleTrackPacket, TrackIndex,
    TrackKind, VideoTrackPacket,
  },
};
use mediadecode_ffmpeg::{Ffmpeg, FfmpegBuffer, FfmpegDemuxer};
use mediaencode::{
  MuxError, MuxRefusal, Muxer, check_decode_order, check_presentation_order,
  mediaspine::track::TrackDescription,
};
use support::{Corpus, remux};

/// A text carrier's contents, owned — read through its `AsRef<str>`
/// alone.
fn owned_text(text: &impl AsRef<str>) -> String {
  text.as_ref().to_owned()
}

/// One attachment track's file, as the muxer was handed it.
#[derive(Debug, PartialEq)]
struct FileSeen {
  track: usize,
  filename: Option<String>,
  mime_type: Option<String>,
  payload: Vec<u8>,
}

/// What reached the muxer: the table's kinds, codecs and languages, the
/// attachment files, and the timed packets written on each track.
#[derive(Debug, Default)]
struct Seen {
  kinds: Vec<TrackKind>,
  codecs: Vec<mediadecode_ffmpeg::CodecId>,
  languages: Vec<Option<String>>,
  files: Vec<FileSeen>,
  written: Vec<usize>,
  finished: bool,
}

/// A muxer that records: the table it was opened with, and every packet
/// it is asked to write, refusing what every muxer refuses on the face —
/// a track it does not have, a packet of another kind, a missing decode
/// time, one that goes back ([`check_decode_order`], the portable rule,
/// which admits a repeated one), and a presentation before its decode
/// ([`check_presentation_order`]).
struct Ledger<B> {
  kinds: Vec<TrackKind>,
  last: Vec<Option<Timestamp>>,
  seen: Rc<RefCell<Seen>>,
  _buffer: core::marker::PhantomData<B>,
}

impl<B: AsRef<[u8]>> Ledger<B> {
  fn open(table: Vec<TrackDescription<Ffmpeg, B, ()>>, seen: Rc<RefCell<Seen>>) -> Self {
    let kinds: Vec<TrackKind> = table.iter().map(TrackDescription::kind).collect();
    {
      let mut record = seen.borrow_mut();
      record.kinds = kinds.clone();
      record.codecs = table.iter().map(TrackDescription::codec).collect();
      record.languages = table
        .iter()
        .map(|description| description.language().map(owned_text))
        .collect();
      record.files = table
        .iter()
        .enumerate()
        .filter_map(|(track, description)| {
          description.file().map(|file| FileSeen {
            track,
            filename: file.filename().map(owned_text),
            mime_type: file.mime_type().map(owned_text),
            payload: file.payload().data().as_ref().to_vec(),
          })
        })
        .collect();
      record.written = vec![0; kinds.len()];
    }
    let last = vec![None; kinds.len()];
    Self {
      kinds,
      last,
      seen,
      _buffer: core::marker::PhantomData,
    }
  }

  /// The refusal for a packet of `offered` kind on `track` presented at
  /// `pts` and decoded at `dts`, if any; otherwise the packet is counted.
  fn admit(
    &mut self,
    track: TrackIndex,
    offered: TrackKind,
    (pts, dts): (Option<Timestamp>, Option<Timestamp>),
  ) -> Option<MuxRefusal> {
    let Some(&expected) = self.kinds.get(track.get()) else {
      return Some(MuxRefusal::UnknownTrack { track });
    };
    if expected != offered {
      return Some(MuxRefusal::KindMismatch {
        track,
        expected,
        offered,
      });
    }
    let Some(dts) = dts else {
      return Some(MuxRefusal::MissingDts { track });
    };
    if let Err(refusal) = check_decode_order(track, self.last[track.get()], dts) {
      return Some(refusal);
    }
    if let Err(refusal) = check_presentation_order(track, pts, dts) {
      return Some(refusal);
    }
    self.last[track.get()] = Some(dts);
    self.seen.borrow_mut().written[track.get()] += 1;
    None
  }
}

impl<B: AsRef<[u8]>> Muxer for Ledger<B> {
  type Adapter = Ffmpeg;
  type Buffer = B;
  // The Ledger keeps the face's rules and no container's.
  type Refusal = core::convert::Infallible;
  type Error = core::convert::Infallible;

  fn write_video(
    &mut self,
    packet: VideoTrackPacket<Ffmpeg, B>,
  ) -> Result<(), MuxError<Self::Error, Self::Refusal, VideoTrackPacket<Ffmpeg, B>>> {
    let times = (packet.packet().pts(), packet.packet().dts());
    match self.admit(packet.track(), TrackKind::Video, times) {
      Some(refusal) => Err(MuxError::Refused { refusal, packet }),
      None => Ok(()),
    }
  }

  fn write_audio(
    &mut self,
    packet: AudioTrackPacket<Ffmpeg, B>,
  ) -> Result<(), MuxError<Self::Error, Self::Refusal, AudioTrackPacket<Ffmpeg, B>>> {
    let times = (packet.packet().pts(), packet.packet().dts());
    match self.admit(packet.track(), TrackKind::Audio, times) {
      Some(refusal) => Err(MuxError::Refused { refusal, packet }),
      None => Ok(()),
    }
  }

  fn write_subtitle(
    &mut self,
    packet: SubtitleTrackPacket<Ffmpeg, B>,
  ) -> Result<(), MuxError<Self::Error, Self::Refusal, SubtitleTrackPacket<Ffmpeg, B>>> {
    // A subtitle or data packet's decode time is its presentation time.
    let pts = packet.packet().pts();
    match self.admit(packet.track(), TrackKind::Subtitle, (pts, pts)) {
      Some(refusal) => Err(MuxError::Refused { refusal, packet }),
      None => Ok(()),
    }
  }

  fn write_data(
    &mut self,
    packet: DataTrackPacket<Ffmpeg, B>,
  ) -> Result<(), MuxError<Self::Error, Self::Refusal, DataTrackPacket<Ffmpeg, B>>> {
    let pts = packet.packet().pts();
    match self.admit(packet.track(), TrackKind::Data, (pts, pts)) {
      Some(refusal) => Err(MuxError::Refused { refusal, packet }),
      None => Ok(()),
    }
  }

  fn finish(self) -> Result<(), Self::Error> {
    self.seen.borrow_mut().finished = true;
    Ok(())
  }
}

/// **A generic remux builds its table from a live demuxer.** [`remux`]
/// names neither backend: it reads the attachments, describes every
/// track from the handle the demuxer keeps for it, opens the muxer and
/// writes the timed packets. Over this crate's demuxer and a Matroska
/// file of four tracks, the muxer is opened with all four — the
/// attachment track carrying its font's bytes under its filename and
/// MIME type — and every timed packet reaches it. The demuxer keeps its
/// rows throughout: a conversion that consumed them could not have been
/// called on a shared handle at all.
#[test]
fn a_generic_remux_builds_its_table_from_a_live_demuxer() {
  let Some(corpus) = Corpus::new() else { return };
  let path = corpus.multi_track_mkv();

  let seen = Rc::new(RefCell::new(Seen::default()));
  let mut demuxer = FfmpegDemuxer::open(&path).expect("open mkv");
  let rows_before: Vec<(TrackKind, mediadecode_ffmpeg::CodecId)> = demuxer
    .tracks()
    .iter()
    .map(|row| (row.kind(), row.params().codec()))
    .collect();
  let languages_before: Vec<Option<String>> = demuxer
    .tracks()
    .iter()
    .map(|row| row.language().map(owned_text))
    .collect();

  let ledger_seen = Rc::clone(&seen);
  remux(
    &mut demuxer,
    |_| (),
    move |table, _chapters| Ledger::<FfmpegBuffer>::open(table, ledger_seen),
  );

  let seen = seen.borrow();
  assert!(seen.finished, "the container was finished");
  assert_eq!(
    seen.kinds,
    vec![
      TrackKind::Video,
      TrackKind::Audio,
      TrackKind::Subtitle,
      TrackKind::Attachment,
    ],
    "the muxer was opened with all four tracks",
  );
  let described: Vec<(TrackKind, mediadecode_ffmpeg::CodecId)> = seen
    .kinds
    .iter()
    .copied()
    .zip(seen.codecs.iter().copied())
    .collect();
  assert_eq!(described, rows_before, "each description is its row's");
  assert_eq!(seen.languages, languages_before, "each row's language");
  assert_eq!(
    seen.files,
    vec![FileSeen {
      track: 3,
      filename: Some("font.ttf".to_owned()),
      mime_type: Some("application/x-truetype-font".to_owned()),
      payload: support::FONT_PAYLOAD.to_vec(),
    }],
    "the attachment track's one file: the delivered bytes, under the row's identity",
  );

  // Every timed packet reached the muxer: the same count a second
  // session delivers per track.
  let mut recount = FfmpegDemuxer::open(&path).expect("reopen mkv");
  let mut expected = vec![0usize; seen.kinds.len()];
  while let Some(packet) = recount.next_packet().expect("a packet or the end") {
    if !matches!(packet, DemuxedPacket::Attachment(_)) {
      expected[packet.track().get()] += 1;
    }
  }
  assert_eq!(seen.written, expected, "every timed packet was written");
  assert!(
    seen.written[..3].iter().all(|&count| count > 0),
    "each timed track carried packets"
  );

  // The session kept its rows: the table borrowed them.
  let rows_after: Vec<(TrackKind, mediadecode_ffmpeg::CodecId)> = demuxer
    .tracks()
    .iter()
    .map(|row| (row.kind(), row.params().codec()))
    .collect();
  assert_eq!(rows_after, rows_before, "the demuxer keeps every row");
  assert_eq!(
    demuxer.tracks()[3].filename().map(owned_text).as_deref(),
    Some("font.ttf"),
    "and the attachment row its identity",
  );
}

/// **Two cues at one instant remux whole.** A subtitle track whose cues
/// share a start time is ordinary input — two speakers at once — and the
/// portable decode-order rule admits the second: decode times may repeat
/// on the face, and only go back is refused. Under a face that demanded
/// strictly increasing decode times this remux failed on the second cue.
#[test]
fn a_generic_remux_writes_cues_that_share_an_instant() {
  let Some(corpus) = Corpus::new() else { return };
  let path = corpus.simultaneous_cues_mkv();

  // The premise, read off the file: two subtitle packets at one instant.
  let mut source = FfmpegDemuxer::open(&path).expect("open mkv");
  let mut cue_times = Vec::new();
  while let Some(packet) = source.next_packet().expect("a packet or the end") {
    if let DemuxedPacket::Subtitle(cue) = &packet {
      cue_times.push(cue.packet().pts().expect("a cue has a time"));
    }
  }
  assert_eq!(cue_times.len(), 2, "the fixture carries two cues");
  assert_eq!(cue_times[0], cue_times[1], "at one instant");

  let seen = Rc::new(RefCell::new(Seen::default()));
  let mut demuxer = FfmpegDemuxer::open(&path).expect("open mkv");
  let ledger_seen = Rc::clone(&seen);
  remux(
    &mut demuxer,
    |_| (),
    move |table, _chapters| Ledger::<FfmpegBuffer>::open(table, ledger_seen),
  );

  let seen = seen.borrow();
  assert!(seen.finished, "the container was finished");
  assert_eq!(seen.kinds, vec![TrackKind::Video, TrackKind::Subtitle]);
  assert_eq!(seen.written[1], 2, "both cues were written");
}
