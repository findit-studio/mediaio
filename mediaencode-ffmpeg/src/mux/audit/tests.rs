use super::*;

/// A box whose size field reads `size`, of `fourcc` and `payload`.
fn sized(size: u32, fourcc: &[u8; 4], payload: &[u8]) -> Vec<u8> {
  let mut bytes = size.to_be_bytes().to_vec();
  bytes.extend_from_slice(fourcc);
  bytes.extend_from_slice(payload);
  bytes
}

/// A box of its final size.
fn boxed(fourcc: &[u8; 4], payload: &[u8]) -> Vec<u8> {
  sized(8 + payload.len() as u32, fourcc, payload)
}

/// A box whose size is movenc's placeholder, 0.
fn placeholder(fourcc: &[u8; 4], payload: &[u8]) -> Vec<u8> {
  sized(0, fourcc, payload)
}

/// A file as the sink takes it: runs appended, and patches below the
/// frontier, each handed to the tap at its offset.
#[derive(Default)]
struct Sink {
  runs: Vec<(u64, Vec<u8>)>,
  frontier: u64,
}

impl Sink {
  /// `bytes` appended; answers their offset.
  fn append(&mut self, bytes: &[u8]) -> u64 {
    let at = self.frontier;
    self.runs.push((at, bytes.to_vec()));
    self.frontier += bytes.len() as u64;
    at
  }

  /// `bytes` written at `at`, below the frontier.
  fn patch(&mut self, at: u64, bytes: &[u8]) {
    self.runs.push((at, bytes.to_vec()));
  }

  /// A box's size patched, at `at`: what it spans to the frontier.
  fn close(&mut self, at: u64) {
    let size = u32::try_from(self.frontier - at).expect("a 32-bit size");
    self.patch(at, &size.to_be_bytes());
  }

  /// The MOV walk over the runs, of an MP4 file.
  fn walked(&self) -> Audit {
    let mut audit = Audit::mov(MovMode::Mp4, 4);
    for (at, bytes) in &self.runs {
      audit.took(*at, bytes);
    }
    audit
  }
}

/// The header movenc writes first: `ftyp`, `free`, and the `mdat` of a
/// placeholder size, flushed before any sample; answers the `mdat`'s
/// offset.
fn header(sink: &mut Sink) -> u64 {
  sink.append(&boxed(b"ftyp", b"isom\0\0\x02\0isomiso2mp41"));
  sink.append(&boxed(b"free", &[]));
  sink.append(&placeholder(b"mdat", &[]))
}

/// A track header of version 0, of track `id`.
fn tkhd(id: u32) -> Vec<u8> {
  let mut payload = vec![0; 84];
  payload[12..16].copy_from_slice(&id.to_be_bytes());
  boxed(b"tkhd", &payload)
}

/// A handler of `kind`.
fn hdlr(kind: &[u8; 4]) -> Vec<u8> {
  let mut payload = vec![0; 25];
  payload[8..12].copy_from_slice(kind);
  boxed(b"hdlr", &payload)
}

/// A time-to-sample table of `runs`, each its count and duration.
fn stts(runs: &[(u32, u32)]) -> Vec<u8> {
  let mut payload = vec![0, 0, 0, 0];
  payload.extend_from_slice(&(runs.len() as u32).to_be_bytes());
  for &(count, duration) in runs {
    payload.extend_from_slice(&count.to_be_bytes());
    payload.extend_from_slice(&duration.to_be_bytes());
  }
  boxed(b"stts", &payload)
}

/// A sample-size table listing `sizes`.
fn stsz(sizes: &[u32]) -> Vec<u8> {
  let mut payload = vec![0; 8];
  payload.extend_from_slice(&(sizes.len() as u32).to_be_bytes());
  for size in sizes {
    payload.extend_from_slice(&size.to_be_bytes());
  }
  boxed(b"stsz", &payload)
}

/// The `moov` of one audio track of `sizes`, each sample one tick.
fn moov(sizes: &[u32]) -> Vec<u8> {
  let stbl = [
    boxed(b"stsd", &[0; 8]),
    stts(&[(sizes.len() as u32, 1)]),
    stsz(sizes),
    boxed(b"stco", &[0; 8]),
  ]
  .concat();
  let minf = [boxed(b"smhd", &[0; 8]), boxed(b"stbl", &stbl)].concat();
  let mdia = [hdlr(b"soun"), boxed(b"minf", &minf)].concat();
  let trak = [tkhd(1), boxed(b"mdia", &mdia)].concat();
  boxed(b"moov", &boxed(b"trak", &trak))
}

/// A whole MP4 file of one audio track of `sizes`, written in one buffer
/// but for its `mdat`'s size: the runs the sink takes.
fn whole(sizes: &[u32]) -> Sink {
  let mut sink = Sink::default();
  let mdat = header(&mut sink);
  let payload: u32 = sizes.iter().sum();
  sink.append(&vec![0x21; payload as usize]);
  sink.close(mdat);
  sink.append(&moov(sizes));
  sink
}

/// The expectation of the file [`whole`] writes: one track, its samples of
/// `sizes`, one tick each.
fn expected(sizes: &[u32]) -> ExpectedTrak {
  let mut samples = SampleMirror::default();
  for (at, &size) in sizes.iter().enumerate() {
    samples.sample(at as i64, u64::from(size));
  }
  samples.end(1);
  ExpectedTrak {
    track: Some(TrackIndex::new(0)),
    id: 1,
    samples,
    ..ExpectedTrak::default()
  }
}

/// The movie's parts of an MP4 file of no `udta` and no `pssh`: the
/// synthetic files'.
fn mp4() -> ExpectedMovie {
  ExpectedMovie::default()
}

/// The movie's parts of an AVIF of `media` bytes of payload.
fn avif(media: u64) -> ExpectedMovie {
  ExpectedMovie {
    media: Some(media),
    ..ExpectedMovie::default()
  }
}

fn unfollowed(sink: &Sink) -> Option<Unfollowed> {
  sink.walked().unfollowed()
}

/// **A whole file passes, and its tables are judged count, sum, then
/// fold**: the file as written passes; one sample's size other than handed
/// is named by the sum, and two sizes swapped by the content.
#[test]
fn a_whole_file_passes_and_its_tables_are_judged() {
  let sink = whole(&[4, 5, 4]);
  assert_eq!(unfollowed(&sink), None);
  let audit = sink.walked();
  assert_eq!(
    mov_verdict(&audit, [expected(&[4, 5, 4])].into_iter(), &mp4()),
    None
  );
  assert_eq!(
    mov_verdict(&audit, [expected(&[4, 6, 4])].into_iter(), &mp4()),
    Some(WrittenOtherwise::new(
      Some(TrackIndex::new(0)),
      WrittenPart::SampleSizes,
      Found::Length(Counts::new(13, 14))
    ))
  );
  assert_eq!(
    mov_verdict(&audit, [expected(&[5, 4, 4])].into_iter(), &mp4()),
    Some(WrittenOtherwise::new(
      Some(TrackIndex::new(0)),
      WrittenPart::SampleSizes,
      Found::Content
    ))
  );
}

/// **A track's table left out is named, before the file's sizes it
/// shortens**: an `stbl` without its `stts` (A2) is the track's
/// time-to-sample table, omitted; a `trak` left out is the track, omitted;
/// one more than handed is the file's structure.
///
/// PLANT P609: the verdict skips `TimeToSample`.
#[test]
fn a_table_or_a_trak_left_out_is_named() {
  let mut sink = Sink::default();
  let mdat = header(&mut sink);
  sink.append(&[0x21; 8]);
  sink.close(mdat);
  let stbl = [boxed(b"stsd", &[0; 8]), stsz(&[4, 4])].concat();
  let mdia = [hdlr(b"soun"), boxed(b"minf", &boxed(b"stbl", &stbl))].concat();
  let trak = [tkhd(1), boxed(b"mdia", &mdia)].concat();
  sink.append(&boxed(b"moov", &boxed(b"trak", &trak)));
  let audit = sink.walked();
  assert_eq!(audit.unfollowed(), None);
  assert_eq!(
    mov_verdict(&audit, [expected(&[4, 4])].into_iter(), &mp4()),
    Some(WrittenOtherwise::new(
      Some(TrackIndex::new(0)),
      WrittenPart::TimeToSample,
      Found::Omitted
    ))
  );

  let audit = whole(&[4, 4]).walked();
  let chapters = ExpectedTrak {
    track: None,
    id: 2,
    ..ExpectedTrak::default()
  };
  assert_eq!(
    mov_verdict(&audit, [expected(&[4, 4]), chapters].into_iter(), &mp4()),
    Some(WrittenOtherwise::new(
      None,
      WrittenPart::ChapterTrack,
      Found::Omitted
    ))
  );
  assert_eq!(
    mov_verdict(&audit, core::iter::empty(), &mp4()),
    Some(WrittenOtherwise::new(
      None,
      WrittenPart::Structure,
      Found::Count(Counts::new(1, 0))
    ))
  );
}

/// **The `mdat`'s 32-bit size is judged against the payload the sink
/// took**: declared as taken, the file passes; declared short — A25's
/// `-64` — it is named as the media data's length, after every track's
/// tables.
///
/// PLANT P610: the `mdat` check removed.
#[test]
fn the_mdats_size_is_judged_against_the_payload_taken() {
  let audit = whole(&[4, 4]).walked();
  assert_eq!(audit.mov_walk().and_then(MovWalk::media_data), Some((8, 8)));
  let mut short = Sink::default();
  let mdat = header(&mut short);
  short.append(&[0x21; 72]);
  short.patch(mdat, &16u32.to_be_bytes());
  short.append(&moov(&[36, 36]));
  let audit = short.walked();
  assert_eq!(audit.unfollowed(), None, "followed: the size judged later");
  assert_eq!(
    mov_verdict(&audit, [expected(&[36, 36])].into_iter(), &mp4()),
    Some(WrittenOtherwise::new(
      None,
      WrittenPart::MediaData,
      Found::Length(Counts::new(8, 72))
    ))
  );
}

/// **The `mdat`'s 64-bit form overwrites the `free` before it**: `00000001
/// 'mdat'` and the size in 64 bits, over 16 bytes; anything else there is a
/// rewrite of no field.
#[test]
fn the_mdats_64_bit_form_overwrites_the_free_before_it() {
  let mut sink = Sink::default();
  let mdat = header(&mut sink);
  sink.append(&[0x21; 10]);
  let mut form = vec![0, 0, 0, 1];
  form.extend_from_slice(b"mdat");
  form.extend_from_slice(&26u64.to_be_bytes());
  sink.patch(mdat - 8, &form);
  sink.append(&boxed(b"moov", &[]));
  let audit = sink.walked();
  assert_eq!(audit.unfollowed(), None);
  assert_eq!(
    audit.mov_walk().and_then(MovWalk::media_data),
    Some((10, 10))
  );
  let mut wrong = Sink::default();
  let mdat = header(&mut wrong);
  wrong.append(&[0x21; 10]);
  let mut form = vec![0, 0, 0, 2];
  form.extend_from_slice(b"mdat");
  form.extend_from_slice(&26u64.to_be_bytes());
  wrong.patch(mdat - 8, &form);
  assert_eq!(
    unfollowed(&wrong),
    Some(Unfollowed::new(mdat - 8, Why::Rewrite))
  );
}

/// **Placeholders resolve innermost first, each after its last byte, and in
/// parts where the sink takes a write in parts**: a `moov` and a `trak` of
/// placeholder sizes, the `trak`'s patched first, in two parts, then the
/// `moov`'s; patched the other way round, the `moov`'s names a box that is
/// not the innermost.
#[test]
fn placeholders_resolve_innermost_first_and_in_parts() {
  let mut sink = Sink::default();
  let mdat = header(&mut sink);
  sink.close(mdat);
  let moov = sink.append(&placeholder(b"moov", &[]));
  let trak = sink.append(&placeholder(b"trak", &[]));
  sink.append(&tkhd(1));
  let size = (sink.frontier - trak) as u32;
  sink.patch(trak, &size.to_be_bytes()[..3]);
  sink.patch(trak + 3, &size.to_be_bytes()[3..]);
  sink.close(moov);
  assert_eq!(unfollowed(&sink), None);
  assert_eq!(sink.walked().mov_walk().map(MovWalk::traks_seen), Some(1));

  let mut wrong = Sink::default();
  let mdat = header(&mut wrong);
  wrong.close(mdat);
  let moov = wrong.append(&placeholder(b"moov", &[]));
  wrong.append(&placeholder(b"trak", &[]));
  wrong.close(moov);
  assert_eq!(
    unfollowed(&wrong),
    Some(Unfollowed::new(moov, Why::Structure))
  );
}

/// **A size patch must end its box at the frontier**: a value of one byte
/// less is no size movenc writes.
#[test]
fn a_size_patch_ends_its_box_at_the_frontier() {
  let mut sink = Sink::default();
  let mdat = header(&mut sink);
  sink.close(mdat);
  let moov = sink.append(&placeholder(b"moov", &boxed(b"mvhd", &[0; 8])));
  let size = (sink.frontier - moov - 1) as u32;
  sink.patch(moov, &size.to_be_bytes());
  assert_eq!(
    unfollowed(&sink),
    Some(Unfollowed::new(moov, Why::Structure))
  );
}

/// **A rewrite of no field, a run past the end, and a run across it are not
/// followed**: a byte of the `ftyp` rewritten — as `avio_flush`'s seek back
/// over bytes it had flushed, then a write there, would be (gate condition
/// 3), which no kept writer makes — is a rewrite; a run past the frontier a
/// gap; one starting below it and running past it a rewrite.
///
/// PLANT P612: a rewrite below the frontier accepted unregistered.
#[test]
fn a_rewrite_of_no_field_a_gap_and_a_run_across_the_end_are_named() {
  let mut sink = Sink::default();
  header(&mut sink);
  sink.patch(9, b"X");
  assert_eq!(unfollowed(&sink), Some(Unfollowed::new(9, Why::Rewrite)));

  let mut gap = Sink::default();
  header(&mut gap);
  let end = gap.frontier;
  gap.runs.push((end + 4, vec![0; 4]));
  assert_eq!(unfollowed(&gap), Some(Unfollowed::new(end, Why::Gap)));

  let mut across = Sink::default();
  header(&mut across);
  let end = across.frontier;
  across.runs.push((end - 2, vec![0; 4]));
  assert_eq!(
    unfollowed(&across),
    Some(Unfollowed::new(end - 2, Why::Rewrite))
  );
}

/// The boxes movenc opens around a track's `stbl`, each of a placeholder
/// size, and its handler of `kind`; answers their offsets, outermost first.
fn opened_to_stbl(sink: &mut Sink, kind: &[u8; 4]) -> [u64; 5] {
  let moov = sink.append(&placeholder(b"moov", &[]));
  let trak = sink.append(&placeholder(b"trak", &[]));
  let mdia = sink.append(&placeholder(b"mdia", &[]));
  sink.append(&hdlr(kind));
  let minf = sink.append(&placeholder(b"minf", &[]));
  let stbl = sink.append(&placeholder(b"stbl", &[]));
  [moov, trak, mdia, minf, stbl]
}

/// **The counts and the version movenc patches beside the sizes are
/// admitted where they lie**: `stsc`'s and `stss`'s counts at box offset 12
/// while their boxes are open; `stsd`'s version at box offset 8 right after
/// its size's patch, and not after a later byte.
#[test]
fn the_counts_and_the_version_are_admitted_where_they_lie() {
  let mut sink = Sink::default();
  let mdat = header(&mut sink);
  sink.close(mdat);
  let opened = opened_to_stbl(&mut sink, b"soun");
  let stsd = sink.append(&placeholder(b"stsd", &[0; 8]));
  sink.close(stsd);
  sink.patch(stsd + 8, &[1]);
  let stsc = sink.append(&placeholder(b"stsc", &[0; 8]));
  sink.patch(stsc + 12, &0u32.to_be_bytes());
  sink.close(stsc);
  let stss = sink.append(&placeholder(b"stss", &[0, 0, 0, 0, 0, 0, 0, 9, 0, 0, 0, 1]));
  sink.patch(stss + 12, &1u32.to_be_bytes());
  sink.close(stss);
  for at in opened.into_iter().rev() {
    sink.close(at);
  }
  assert_eq!(unfollowed(&sink), None);

  let mut late = Sink::default();
  let mdat = header(&mut late);
  late.close(mdat);
  opened_to_stbl(&mut late, b"soun");
  let stsd = late.append(&placeholder(b"stsd", &[0; 8]));
  late.close(stsd);
  late.append(&stts(&[]));
  late.patch(stsd + 8, &[1]);
  assert_eq!(
    unfollowed(&late),
    Some(Unfollowed::new(stsd + 8, Why::Rewrite))
  );
}

/// **A raw subtitle entry's `btrt` is patched where it falls**: its
/// configuration's bytes are not boxes, and its trailing `btrt`'s size, 20,
/// reaches the sink at the frontier less 20.
#[test]
fn a_raw_entrys_btrt_is_patched_where_it_falls() {
  let mut sink = Sink::default();
  let mdat = header(&mut sink);
  sink.close(mdat);
  let opened = opened_to_stbl(&mut sink, b"sbtl");
  let stsd = sink.append(&placeholder(b"stsd", &[0, 0, 0, 0, 0, 0, 0, 1]));
  let entry = sink.append(&placeholder(b"tx3g", &[0; 8 + 38]));
  let btrt = sink.append(&placeholder(b"btrt", &[0; 12]));
  sink.close(btrt);
  sink.close(entry);
  sink.close(stsd);
  for at in opened.into_iter().rev() {
    sink.close(at);
  }
  assert_eq!(unfollowed(&sink), None);
}

/// **A file that ends inside a box is truncated, and a field written in
/// part unresolved**: the `moov`'s size never patched; then its patch's
/// first bytes alone.
#[test]
fn a_file_ending_inside_a_box_or_a_field_is_named() {
  let mut sink = Sink::default();
  let mdat = header(&mut sink);
  sink.close(mdat);
  let moov = sink.append(&placeholder(b"moov", &boxed(b"mvhd", &[0; 8])));
  let end = sink.frontier;
  assert_eq!(
    unfollowed(&sink),
    Some(Unfollowed::new(end, Why::Truncated))
  );
  sink.patch(moov, &[0, 0]);
  assert_eq!(
    unfollowed(&sink),
    Some(Unfollowed::new(end, Why::Unresolved))
  );
}

/// **A position lost is named**: no byte after it can be placed.
#[test]
fn a_position_lost_is_named() {
  let mut sink = Sink::default();
  header(&mut sink);
  let end = sink.frontier;
  let mut audit = sink.walked();
  audit.lost();
  assert_eq!(
    audit.unfollowed(),
    Some(Unfollowed::new(end, Why::Position))
  );
}

/// The two tables R5 reads of `runs` and `sizes`.
fn cue_tables(runs: &[(u64, u64)], sizes: &[u64]) -> (Summary, Summary) {
  let mut tables = CueTables::default();
  for &(count, duration) in runs {
    tables.time(count, duration);
  }
  for &size in sizes {
    tables.size(size, 1);
  }
  (tables.times(), tables.sizes())
}

/// **R5: a `mov_text` track's tables read alike whatever end samples the
/// interleaving made**: the CLI's two shapes — beside video, an end sample
/// at each boundary but the first; alone, none — and a mixed one fold as the
/// cues and the trailer's end; a lost trailer's end, or a lost cue, does
/// not.
#[test]
fn r5_the_cue_tables_fold_alike_whatever_end_samples_the_interleaving_made() {
  let beside = cue_tables(
    &[(1, 1000), (1, 0), (1, 1000), (1, 0), (1, 1000), (1, 0)],
    &[7, 2, 8, 2, 7, 2],
  );
  let alone = cue_tables(&[(3, 1000), (1, 0)], &[7, 8, 7, 2]);
  let mixed = cue_tables(&[(2, 1000), (1, 0), (1, 1000), (1, 0)], &[7, 8, 2, 7, 2]);
  assert_eq!(beside, alone);
  assert_eq!(mixed, alone);
  let end_lost = cue_tables(&[(3, 1000)], &[7, 8, 7]);
  assert_eq!(
    end_lost.0.count + 1,
    alone.0.count,
    "the counted samples short"
  );
  assert_eq!(end_lost.1.count + 1, alone.1.count);
  let cue_lost = cue_tables(&[(2, 1000), (1, 0)], &[7, 7, 2]);
  assert_ne!(cue_lost.0.count, alone.0.count);
  assert_ne!(cue_lost.1.count, alone.1.count);
}

/// **The fold moves with every value**: zeros folded from the seed are not
/// the seed, and order matters.
#[test]
fn the_fold_is_order_sensitive_and_moved_by_zeros() {
  let mut one = Runs::default();
  one.push(0, 1);
  let mut two = Runs::default();
  two.push(0, 2);
  assert_ne!(one.summary().fold, Runs::default().summary().fold);
  assert_ne!(one.summary().fold, two.summary().fold);
  let mut ab = Runs::default();
  ab.push(1, 1);
  ab.push(2, 1);
  let mut ba = Runs::default();
  ba.push(2, 1);
  ba.push(1, 1);
  assert_ne!(ab.summary().fold, ba.summary().fold);
}

/// **R7: a patch at the head of a run that goes on past the frontier is
/// split there.** movenc's 64-bit `mdat` size over an empty payload, A25's
/// road (`mov_write_mdat_size`, `movenc.c` 8997–9010; the `uint64_t` size
/// wrapped by `-64`): the seek back to the `moov`'s offset is the patch's
/// very next byte, inside avio's buffer (`aviobuf.c` 272–280), so the
/// patch and the `moov` leave as one run from the `free` box across the
/// frontier — judged as the `mdat`'s patch, then the `moov` appended.
#[test]
fn r7_a_patch_at_the_head_of_a_run_past_the_frontier_is_split_there() {
  let mut sink = Sink::default();
  let mdat = header(&mut sink);
  let mut run = vec![0, 0, 0, 1];
  run.extend_from_slice(b"mdat");
  run.extend_from_slice(&u64::MAX.wrapping_sub(47).to_be_bytes());
  let moov = moov(&[]);
  run.extend_from_slice(&moov);
  sink.runs.push((mdat - 8, run));
  sink.frontier += moov.len() as u64;
  let audit = sink.walked();
  assert_eq!(audit.unfollowed(), None, "a patch, then an append");
  assert_eq!(
    audit.mov_walk().and_then(MovWalk::media_data),
    Some((u64::MAX - 63, 0)),
    "the size the patch declared, against the payload the sink took"
  );
  assert_eq!(audit.mov_walk().map(MovWalk::traks_seen), Some(1));
}

/// **R7's refusal side: a run across the frontier whose part below is no
/// field stays a rewrite.** Two bytes of the `mdat`'s type rewritten, and
/// two appended after them, in one run: the part below tiles no field the
/// walk registered.
#[test]
fn r7_a_run_across_the_frontier_whose_part_below_is_no_field_is_a_rewrite() {
  let mut sink = Sink::default();
  header(&mut sink);
  let end = sink.frontier;
  sink.runs.push((end - 2, vec![b'a', b't', 0, 0]));
  sink.frontier += 2;
  assert_eq!(
    unfollowed(&sink),
    Some(Unfollowed::new(end - 2, Why::Rewrite))
  );
}

// ---------------------------------------------------------------------------
//  The sample descriptions (R6): each entry judged as its last byte is read,
//  against what the muxer handed the tap of it.
// ---------------------------------------------------------------------------

/// `bytes` written at `at`, which may lie below the frontier and run past
/// it: the frontier moved to its end where it does.
impl Sink {
  fn write(&mut self, at: u64, bytes: &[u8]) {
    self.runs.push((at, bytes.to_vec()));
    self.frontier = self.frontier.max(at + bytes.len() as u64);
  }

  /// The MOV walk over the runs, of a file in `mode`, handed `handed`
  /// before any run, as the muxer hands it before the trailer.
  fn walked_with(&self, mode: MovMode, handed: Handed) -> Audit {
    let mut audit = Audit::mov(mode, 4);
    audit.hand(0, handed);
    for (at, bytes) in &self.runs {
      audit.took(*at, bytes);
    }
    audit
  }
}

/// A video sample entry `fourcc`: its 78 bytes, then `children`.
fn video_entry(fourcc: &[u8; 4], children: &[Vec<u8>]) -> Vec<u8> {
  boxed(fourcc, &[vec![0; 78], children.concat()].concat())
}

/// An `stsd` of `entries`.
fn stsd(entries: &[Vec<u8>]) -> Vec<u8> {
  let mut payload = vec![0; 4];
  payload.extend_from_slice(&(entries.len() as u32).to_be_bytes());
  payload.extend(entries.concat());
  boxed(b"stsd", &payload)
}

/// A whole file of one track of the handler `kind`, its `stsd` of
/// `entries`, of one sample of 4 bytes; the `moov` written in one buffer.
fn described(kind: &[u8; 4], entries: &[Vec<u8>]) -> Sink {
  let mut sink = Sink::default();
  let mdat = header(&mut sink);
  sink.append(&[0x21; 4]);
  sink.close(mdat);
  let stbl = [
    stsd(entries),
    stts(&[(1, 1)]),
    stsz(&[4]),
    boxed(b"stco", &[0; 8]),
  ]
  .concat();
  let minf = [boxed(b"vmhd", &[0; 12]), boxed(b"stbl", &stbl)].concat();
  let mdia = [hdlr(kind), boxed(b"minf", &minf)].concat();
  let trak = [tkhd(1), boxed(b"mdia", &mdia)].concat();
  sink.append(&boxed(b"moov", &boxed(b"trak", &trak)));
  sink
}

/// The verdict on `sink`, walked in `mode` and handed `handed`, against
/// the expectation of its one sample.
fn judged_with(sink: &Sink, mode: MovMode, handed: Handed) -> Option<WrittenOtherwise> {
  mov_verdict(
    &sink.walked_with(mode, handed),
    [expected(&[4])].into_iter(),
    &mp4(),
  )
}

/// An `avcC` configuration, as a record: written as it is
/// ([`records::avcc_written_len`]).
fn avcc_config(tag: u8) -> Vec<u8> {
  vec![
    1, 0x64, 0, 0x1F, 0xFF, 0xE1, 0, 4, 0x67, 0x64, 0, 0x1F, 1, 0, 2, 0x68, tag,
  ]
}

/// H.264's descriptions as handed, of `configs`.
fn avc_handed(configs: &[Vec<u8>]) -> Handed {
  Handed::new(
    Carried::Record(Writer::Avcc),
    configs.iter().cloned().map(Some).collect(),
    Vec::new(),
  )
}

fn named(part: WrittenPart, found: Found) -> Option<WrittenOtherwise> {
  Some(WrittenOtherwise::new(Some(TrackIndex::new(0)), part, found))
}

/// **A description judged whole passes, and each difference is named by
/// its index**: an `avcC` as its writer writes the configuration passes; one
/// cut, one empty — its placeholder patched to 8 bytes, as FFmpeg's helper
/// leaves it — and one left out are named against the configuration's
/// length; the second of two, by index 1; a count short or long, by the
/// first index past the shorter.
#[test]
fn a_description_judged_whole_passes_and_each_difference_is_named() {
  let config = avcc_config(0xEB);
  let len = config.len() as u64;
  let handed = || avc_handed(&[config.clone()]);
  let entry = |record: Vec<u8>| video_entry(b"avc1", &[record]);
  let whole = described(b"vide", &[entry(boxed(b"avcC", &config))]);
  assert_eq!(judged_with(&whole, MovMode::Mp4, handed()), None);
  let cut = described(b"vide", &[entry(boxed(b"avcC", &config[..10]))]);
  assert_eq!(
    judged_with(&cut, MovMode::Mp4, handed()),
    named(
      WrittenPart::Configuration(0),
      Found::Length(Counts::new(10, len))
    )
  );
  let left_out = described(b"vide", &[video_entry(b"avc1", &[boxed(b"pasp", &[0; 8])])]);
  assert_eq!(
    judged_with(&left_out, MovMode::Mp4, handed()),
    named(WrittenPart::Configuration(0), Found::Omitted)
  );
  // An empty record, its header flushed before its size's patch.
  let mut empty = Sink::default();
  let mdat = header(&mut empty);
  empty.append(&[0x21; 4]);
  empty.close(mdat);
  let [moov, trak, mdia, minf, stbl] = opened_to_stbl(&mut empty, b"vide");
  let stsd_at = empty.append(&placeholder(b"stsd", &[0, 0, 0, 0, 0, 0, 0, 1]));
  let avc1 = empty.append(&placeholder(b"avc1", &[0; 78]));
  let avcc = empty.append(&placeholder(b"avcC", &[]));
  empty.close(avcc);
  empty.close(avc1);
  empty.close(stsd_at);
  empty.append(&stts(&[(1, 1)]));
  empty.append(&stsz(&[4]));
  empty.append(&boxed(b"stco", &[0; 8]));
  for at in [stbl, minf, mdia] {
    empty.close(at);
  }
  // The track header after the media box, as no writer orders it, is
  // the walk's alone: it reads the id wherever the `trak` holds it.
  empty.append(&tkhd(1));
  empty.close(trak);
  empty.close(moov);
  assert_eq!(
    empty.walked().unfollowed(),
    None,
    "the empty record followed"
  );
  assert_eq!(
    judged_with(&empty, MovMode::Mp4, handed()),
    named(
      WrittenPart::Configuration(0),
      Found::Length(Counts::new(0, len))
    )
  );
  // The second of two descriptions, cut.
  let other = avcc_config(0xEC);
  let two = described(
    b"vide",
    &[
      entry(boxed(b"avcC", &config)),
      entry(boxed(b"avcC", &other[..12])),
    ],
  );
  assert_eq!(
    judged_with(
      &two,
      MovMode::Mp4,
      avc_handed(&[config.clone(), other.clone()])
    ),
    named(
      WrittenPart::Configuration(1),
      Found::Length(Counts::new(12, other.len() as u64))
    )
  );
  // A count short, then long.
  let one = described(b"vide", &[entry(boxed(b"avcC", &config))]);
  assert_eq!(
    judged_with(
      &one,
      MovMode::Mp4,
      avc_handed(&[config.clone(), other.clone()])
    ),
    named(
      WrittenPart::Configuration(1),
      Found::Count(Counts::new(1, 2))
    )
  );
  let both = described(
    b"vide",
    &[
      entry(boxed(b"avcC", &config)),
      entry(boxed(b"avcC", &other)),
    ],
  );
  assert_eq!(
    judged_with(&both, MovMode::Mp4, handed()),
    named(
      WrittenPart::Configuration(1),
      Found::Count(Counts::new(2, 1))
    )
  );
}

/// **R6 (d): a difference at the 1 024th description is named
/// `Configuration(1023)`**: 1 024 descriptions handed and written, the last
/// one's record cut.
#[test]
fn r6_the_1024th_description_is_named_by_its_index() {
  let configs: Vec<Vec<u8>> = (0..1024_u32)
    .map(|k| [avcc_config(0xEB), k.to_be_bytes().to_vec()].concat())
    .collect();
  let entries: Vec<Vec<u8>> = configs
    .iter()
    .enumerate()
    .map(|(index, config)| {
      let record = if index == 1023 {
        &config[..config.len() - 1]
      } else {
        &config[..]
      };
      video_entry(b"avc1", &[boxed(b"avcC", record)])
    })
    .collect();
  let sink = described(b"vide", &entries);
  let len = configs[1023].len() as u64;
  assert_eq!(
    judged_with(&sink, MovMode::Mp4, avc_handed(&configs)),
    named(
      WrittenPart::Configuration(1023),
      Found::Length(Counts::new(len - 1, len))
    )
  );
  let entries: Vec<Vec<u8>> = configs
    .iter()
    .map(|config| video_entry(b"avc1", &[boxed(b"avcC", config)]))
    .collect();
  assert_eq!(
    judged_with(
      &described(b"vide", &entries),
      MovMode::Mp4,
      avc_handed(&configs)
    ),
    None,
    "the 1 024 written whole"
  );
}

/// **Gate condition 1: a `vvcC` of its header alone is named left out**:
/// `vvcC`'s writer writes its record whole or not at all, after the box's
/// own version and flags (`mov_write_vvcc_tag`, `movenc.c` 1708–1723), so
/// a box of 12 bytes holds none; one byte past its header passes.
#[test]
fn a_vvcc_of_its_header_alone_is_named_left_out() {
  let handed = || {
    Handed::new(
      Carried::Record(Writer::Vvcc),
      vec![Some(vec![0, 0, 0, 1, 0x00, 0x79, 0x80])],
      Vec::new(),
    )
  };
  let empty = described(b"vide", &[video_entry(b"vvc1", &[boxed(b"vvcC", &[0; 4])])]);
  assert_eq!(boxed(b"vvcC", &[0; 4]).len(), 12);
  assert_eq!(
    judged_with(&empty, MovMode::Mp4, handed()),
    named(WrittenPart::Configuration(0), Found::Omitted)
  );
  let written = described(b"vide", &[video_entry(b"vvc1", &[boxed(b"vvcC", &[0; 5])])]);
  assert_eq!(judged_with(&written, MovMode::Mp4, handed()), None);
}

/// An HEVC configuration flagged multilayer: a VPS, the base layer's SPS,
/// layer 1's SPS, which `hvcC` skips and `lhvC` writes, and a PPS.
fn layered_hevc() -> Vec<u8> {
  use crate::mux::tests::{hevc_pps, hevc_sps, hevc_vps, start_coded};
  start_coded(&[
    &hevc_vps(0, None),
    &hevc_sps(0, 0, 1, None),
    &hevc_sps(1, 0, 0, None),
    &hevc_pps(),
  ])
}

/// **L223 and gate condition 3: `lhvC`'s rewind, both faces** (`movenc.c`
/// 1654–1674). Where the buffer absorbs it, the entry reads with its `hvcC`
/// and no `lhvC`: named left out, its configuration's `lhvC` expected.
/// Where its header was flushed first — its size's placeholder, then its
/// FourCC, then the next box written from its offset across the frontier
/// — the walk cannot follow: that box's size, written at the open `lhvC`'s
/// size field, does not end it at the frontier (the coordinator's ruling on
/// L223: `Structure`, by §2.3's closing rule and gate condition 2).
#[test]
fn l223_lhvcs_rewind_is_named_on_both_faces() {
  let config = layered_hevc();
  assert_eq!(
    records::lhvc(&config, false),
    records::Wrote::Whole,
    "the premise: an lhvC writes"
  );
  let hvcc_len = records::hvcc_written_len(&config) as usize;
  let lhvc_len = records::lhvc_written_len(&config) as usize;
  assert!(lhvc_len >= 8, "the premise: {lhvc_len}");
  let handed = || {
    Handed::new(
      Carried::Record(Writer::Hvcc {
        complete: false,
        lhvc: true,
      }),
      vec![Some(config.clone())],
      Vec::new(),
    )
  };
  let hvcc = boxed(b"hvcC", &vec![0; hvcc_len]);
  let lhvc = boxed(b"lhvC", &vec![0; lhvc_len]);
  let both = described(b"vide", &[video_entry(b"hvc1", &[hvcc.clone(), lhvc])]);
  assert_eq!(judged_with(&both, MovMode::Mp4, handed()), None);
  let absorbed = described(b"vide", &[video_entry(b"hvc1", &[hvcc.clone()])]);
  assert_eq!(
    judged_with(&absorbed, MovMode::Mp4, handed()),
    named(WrittenPart::Configuration(0), Found::Omitted)
  );
  // The flushed face.
  let mut flushed = Sink::default();
  let mdat = header(&mut flushed);
  flushed.append(&[0x21; 4]);
  flushed.close(mdat);
  opened_to_stbl(&mut flushed, b"vide");
  flushed.append(&placeholder(b"stsd", &[0, 0, 0, 0, 0, 0, 0, 1]));
  flushed.append(&placeholder(b"hvc1", &[0; 78]));
  flushed.append(&hvcc);
  let at = flushed.append(&[0, 0, 0, 0]);
  flushed.append(b"lhvC");
  flushed.write(at, &boxed(b"btrt", &[0; 12]));
  assert_eq!(
    flushed.walked().unfollowed(),
    Some(Unfollowed::new(at, Why::Structure))
  );
}

/// **Gate condition 3: `avio_flush`'s seek-back** (`aviobuf.c` 238–245): a
/// flush that leaves the sink behind the frontier — the writer having gone
/// back inside its buffer — then a write there, where no field lies: the
/// walk cannot follow it.
#[test]
fn a_write_behind_the_frontier_after_a_flush_is_a_rewrite() {
  let mut sink = Sink::default();
  let mdat = header(&mut sink);
  sink.append(&[0x21; 4]);
  sink.close(mdat);
  opened_to_stbl(&mut sink, b"vide");
  sink.append(&placeholder(b"stsd", &[0, 0, 0, 0, 0, 0, 0, 1]));
  sink.append(&placeholder(b"avc1", &[0; 78]));
  let pasp = sink.append(&boxed(b"pasp", &[0; 8]));
  // Behind the frontier, inside the closed `pasp`, and on past it.
  sink.write(pasp + 8, &[1; 16]);
  assert_eq!(
    sink.walked().unfollowed(),
    Some(Unfollowed::new(pasp + 8, Why::Rewrite))
  );
}

/// A MOV-mode audio entry of version 0, E-AC-3's: its 28 bytes, then a
/// `wave` of `frma`, `dec3` of `dec3`'s payload where given, and the
/// terminator (`mov_write_wave_tag`, `movenc.c` 1059–1100).
fn eac3_entry(dec3: Option<&[u8]>) -> Vec<u8> {
  let mut wave = boxed(b"frma", b"ec-3");
  if let Some(payload) = dec3 {
    wave.extend(boxed(b"dec3", payload));
  }
  wave.extend(sized(8, &[0; 4], &[]));
  boxed(b"ec-3", &[vec![0; 28], boxed(b"wave", &wave)].concat())
}

/// **A `dec3` is judged against its own fields** (`mov_write_eac3_tag`,
/// 612–666): one independent substream of no dependent ones, its five
/// bytes, passes; with the complexity index after them, seven; a substream
/// naming dependent ones, its four bytes; a byte past them, or a `dec3`
/// left out, is named.
#[test]
fn a_dec3_is_judged_against_its_own_fields() {
  let handed = || Handed::new(Carried::Dec3, vec![Some(Vec::new())], Vec::new());
  let audio = |dec3: Option<&[u8]>| described(b"soun", &[eac3_entry(dec3)]);
  let one = [0x0C, 0x00, 0x20, 0x0F, 0x00];
  assert_eq!(
    judged_with(&audio(Some(&one)), MovMode::Mov, handed()),
    None
  );
  let tail = [&one[..], &[0x01, 0x10]].concat();
  assert_eq!(
    judged_with(&audio(Some(&tail)), MovMode::Mov, handed()),
    None
  );
  // Two substreams, the second naming dependent ones: 2 + 3 + 4.
  let two = [0x0C, 0x01, 0x20, 0x0F, 0x00, 0x20, 0x0F, 0x02, 0x80];
  assert_eq!(
    judged_with(&audio(Some(&two)), MovMode::Mov, handed()),
    None
  );
  let past = [&one[..], &[0x00]].concat();
  assert_eq!(
    judged_with(&audio(Some(&past)), MovMode::Mov, handed()),
    named(
      WrittenPart::Configuration(0),
      Found::Length(Counts::new(6, 5))
    )
  );
  assert_eq!(
    judged_with(&audio(None), MovMode::Mov, handed()),
    named(WrittenPart::Configuration(0), Found::Omitted)
  );
}

/// An `mp4s` entry whose `esds` holds the decoder-specific information
/// `dsi` behind the tag `tag` (`mov_write_esds_tag`, 765–816): 30 bytes
/// before it, then its tag, its length in four bytes of seven bits, its
/// bytes, and the SL descriptor.
fn dvd_entry(tag: u8, dsi: &[u8]) -> Vec<u8> {
  let len = dsi.len() as u32;
  let mut esds = vec![0; 30];
  esds.push(tag);
  esds.extend([
    ((len >> 21) as u8) | 0x80,
    ((len >> 14) as u8) | 0x80,
    ((len >> 7) as u8) | 0x80,
    (len & 0x7F) as u8,
  ]);
  esds.extend_from_slice(dsi);
  esds.extend([0x06, 0x80, 0x80, 0x80, 0x01, 0x02]);
  boxed(b"mp4s", &[vec![0; 8], boxed(b"esds", &esds)].concat())
}

/// **DVD subtitles' palette is judged by its colours** (8000–8048): the
/// sixteen colours as movenc converts them pass; one other is named by
/// the content, a palette of another length by its length, and a
/// decoder-specific information left out — its tag not at offset 30 —
/// as omitted.
#[test]
fn a_dvd_palette_is_judged_by_its_colours() {
  let colours: [u32; 16] = core::array::from_fn(|at| 0x10_8080 + at as u32 * 0x0B_0000);
  let bytes: Vec<u8> = colours
    .iter()
    .flat_map(|colour| colour.to_be_bytes())
    .collect();
  let handed = || Handed::new(Carried::Palette(colours), vec![None], Vec::new());
  let subtitles = |entry: Vec<u8>| described(b"subp", &[entry]);
  assert_eq!(
    judged_with(&subtitles(dvd_entry(5, &bytes)), MovMode::Mp4, handed()),
    None
  );
  let mut other = bytes.clone();
  other[17] ^= 1;
  assert_eq!(
    judged_with(&subtitles(dvd_entry(5, &other)), MovMode::Mp4, handed()),
    named(WrittenPart::Palette(0), Found::Content)
  );
  assert_eq!(
    judged_with(
      &subtitles(dvd_entry(5, &bytes[..60])),
      MovMode::Mp4,
      handed()
    ),
    named(WrittenPart::Palette(0), Found::Length(Counts::new(60, 64)))
  );
  assert_eq!(
    judged_with(&subtitles(dvd_entry(7, &bytes)), MovMode::Mp4, handed()),
    named(WrittenPart::Palette(0), Found::Omitted)
  );
}

/// **R8: a record whose configuration rests on the trust model is judged
/// not at all, and its writer's record of a conforming configuration still
/// is**: for each writer whose mirror stops at the trust model's limit —
/// `hvcC` (an SPS of 65 short-term reference picture sets), `evcC` (an SPS
/// of id 16), `vvcC` (an SPS of eight sub-layers), `dvc1` (a main profile's
/// sequence header) — an entry of no record: of the conforming twin, named
/// left out, as on A4's road; of the trust stop's, no finding.
#[test]
fn r8_a_record_at_a_trust_stop_is_judged_not_at_all() {
  use crate::mux::tests::{
    evc_header, evc_sps, hevc_pps, hevc_sps, hevc_vps, length_prefixed_units, start_coded, vvc_unit,
  };
  let evc_pps = [&evc_header(25)[..], &[0x80]].concat();
  // An SPS of its ids' byte, then eight sub-layers less one in 3 bits.
  let vvc_eight = vec![0x00, (15 << 3) | 1, 0x00, 0xE0];
  let vc1 = |profile: u8| vec![0, 0, 1, 0x0F, (profile << 6) | 0x08, 0x0A];
  let hevc = |short_term: u32| {
    start_coded(&[
      &hevc_vps(0, None),
      &hevc_sps(0, 0, short_term, None),
      &hevc_pps(),
    ])
  };
  let cases = [
    (
      "hvcC",
      Writer::Hvcc {
        complete: false,
        lhvc: false,
      },
      hevc(1),
      hevc(65),
    ),
    (
      "evcC",
      Writer::Evcc,
      length_prefixed_units(&[&evc_sps(0, 1, 2, 2), &evc_pps]),
      length_prefixed_units(&[&evc_sps(16, 1, 2, 2), &evc_pps]),
    ),
    (
      "vvcC",
      Writer::Vvcc,
      start_coded(&[&vvc_unit(15), &vvc_unit(16)]),
      start_coded(&[&vvc_eight, &vvc_unit(16)]),
    ),
    ("dvc1", Writer::Dvc1, vc1(3), vc1(1)),
  ];
  for (name, writer, conforming, trusted) in cases {
    assert!(
      !records::rests_on_trust(writer, &conforming),
      "{name}: the conforming twin"
    );
    assert!(
      records::rests_on_trust(writer, &trusted),
      "{name}: the trust stop"
    );
    let handed = |config: &[u8]| {
      Handed::new(
        Carried::Record(writer),
        vec![Some(config.to_vec())],
        Vec::new(),
      )
    };
    let sink = described(b"vide", &[video_entry(b"avc1", &[boxed(b"pasp", &[0; 8])])]);
    assert_eq!(
      judged_with(&sink, MovMode::Mp4, handed(&conforming)),
      named(WrittenPart::Configuration(0), Found::Omitted),
      "{name}: a conforming record left out"
    );
    assert_eq!(
      judged_with(&sink, MovMode::Mp4, handed(&trusted)),
      None,
      "{name}: no claim at the trust stop"
    );
  }
}

// ---------------------------------------------------------------------------
//  AVIF (A13): its whole file written at the trailer.
// ---------------------------------------------------------------------------

/// An `iloc` of one item, its extent at `offset`, of `length` bytes
/// (`mov_write_iloc_tag`, `movenc.c` 3660–3681).
fn iloc(offset: u32, length: u32) -> Vec<u8> {
  let mut payload = vec![0, 0, 0, 0, 0x44, 0, 0, 1, 0, 1, 0, 0, 0, 1];
  payload.extend_from_slice(&offset.to_be_bytes());
  payload.extend_from_slice(&length.to_be_bytes());
  boxed(b"iloc", &payload)
}

/// AVIF's `meta`: its version and flags, then `hdlr`, `pitm`, `iloc` of
/// one item, `iinf`, and `iprp` of an `ipco` holding the item's `av1C` of
/// `av1c`; and the `iloc`'s extent offset's place in it, from the `meta`.
fn avif_meta(offset: u32, length: u32, av1c: &[u8]) -> (Vec<u8>, u64) {
  let head = [
    vec![0; 4],
    hdlr(b"pict"),
    boxed(b"pitm", &[0, 0, 0, 0, 0, 1]),
  ]
  .concat();
  let extent = 8 + head.len() as u64 + 8 + 14;
  let ipco = boxed(
    b"ipco",
    &[boxed(b"ispe", &[0; 12]), boxed(b"av1C", av1c)].concat(),
  );
  let payload = [
    head,
    iloc(offset, length),
    boxed(b"iinf", &[0; 6]),
    boxed(b"iprp", &[ipco, boxed(b"ipma", &[0; 11])].concat()),
  ]
  .concat();
  (boxed(b"meta", &payload), extent)
}

/// The walk of an AVIF file, handed `handed` for its track.
fn avif_walked(sink: &Sink, handed: Option<Handed>) -> Audit {
  let mut audit = Audit::mov(MovMode::Avif, 2);
  if let Some(handed) = handed {
    audit.hand(0, handed);
  }
  for (at, bytes) in &sink.runs {
    audit.took(*at, bytes);
  }
  audit
}

/// A still AVIF of a payload of `payload` bytes, its extent offset written
/// `offset` — `None`, the payload's place — and its `av1C` `av1c`: the
/// runs, and the payload's place.
fn still_avif(payload: usize, offset: Option<u32>, av1c: &[u8]) -> (Sink, u64) {
  let mut sink = Sink::default();
  sink.append(&boxed(b"ftyp", b"avifmif1miafMA1B"));
  let (probe, _) = avif_meta(0, payload as u32, av1c);
  let start = sink.frontier + probe.len() as u64 + 8;
  let (meta, _) = avif_meta(offset.unwrap_or(start as u32), payload as u32, av1c);
  sink.append(&meta);
  sink.append(&sized(8 + payload as u32, b"mdat", &vec![0x21; payload]));
  (sink, start)
}

/// **AVIF's file is walked whole, and its payload, extent and item record
/// judged** (`avif_write_trailer`, `movenc.c` 9154–9213): a still image
/// passes; a payload other than the samples' bytes is named by its length,
/// an extent offset other than the payload's place by its value, and the
/// item's `av1C` against its track's description.
#[test]
fn avifs_file_is_walked_and_its_parts_judged() {
  let av1c = [0x81, 0, 0, 0];
  let handed = || {
    Some(Handed::new(
      Carried::ItemAv1c,
      vec![Some(av1c.to_vec())],
      Vec::new(),
    ))
  };
  let (sink, start) = still_avif(27, None, &av1c);
  let audit = avif_walked(&sink, handed());
  assert_eq!(audit.unfollowed(), None);
  assert_eq!(mov_verdict(&audit, core::iter::empty(), &avif(27)), None);
  assert_eq!(
    mov_verdict(&audit, core::iter::empty(), &avif(28)),
    Some(WrittenOtherwise::new(
      None,
      WrittenPart::MediaData,
      Found::Length(Counts::new(27, 28))
    ))
  );
  let (moved, _) = still_avif(27, Some(start as u32 + 4), &av1c);
  assert_eq!(
    mov_verdict(
      &avif_walked(&moved, handed()),
      core::iter::empty(),
      &avif(27)
    ),
    Some(WrittenOtherwise::new(
      None,
      WrittenPart::ItemLocation(1),
      Found::Value(Counts::new(start + 4, start))
    ))
  );
  let (cut, _) = still_avif(27, None, &av1c[..2]);
  assert_eq!(
    mov_verdict(&avif_walked(&cut, handed()), core::iter::empty(), &avif(27)),
    named(
      WrittenPart::Configuration(0),
      Found::Length(Counts::new(2, 4))
    )
  );
}

/// **AVIF's extent offset, written 0 and patched once the payload is
/// placed, is admitted where it lies** (9200–9210): its `meta` flushed
/// before the payload, the patch a write of its own after it.
#[test]
fn avifs_extent_offset_is_patched_where_it_lies() {
  let av1c = [0x81, 0, 0, 0];
  let mut sink = Sink::default();
  sink.append(&boxed(b"ftyp", b"avifmif1miafMA1B"));
  let (meta, extent) = avif_meta(0, 27, &av1c);
  let at = sink.append(&meta);
  let start = sink.append(&sized(35, b"mdat", &[])) + 8;
  sink.append(&[0x21; 27]);
  sink.patch(at + extent, &(start as u32).to_be_bytes());
  let audit = avif_walked(&sink, None);
  assert_eq!(audit.unfollowed(), None);
  assert_eq!(mov_verdict(&audit, core::iter::empty(), &avif(27)), None);
  // In two parts, as a sink takes it.
  let mut parts = Sink::default();
  parts.append(&boxed(b"ftyp", b"avifmif1miafMA1B"));
  let at = parts.append(&meta);
  parts.append(&sized(35, b"mdat", &[]));
  parts.append(&[0x21; 27]);
  let word = (start as u32).to_be_bytes();
  parts.patch(at + extent, &word[..1]);
  parts.patch(at + extent + 1, &word[1..]);
  assert_eq!(avif_walked(&parts, None).unfollowed(), None);
}

/// **An animated AVIF's first chunk is its payload's first byte**
/// (9179–9181): a `moov` whose `stco` says so passes; one moved by
/// `get_moov_size`'s error, taken as the `moov`'s size, is named.
#[test]
fn an_animated_avifs_first_chunk_is_judged() {
  let moov_of = |first: u32| {
    let stbl = [
      stsd(&[]),
      stts(&[(2, 1)]),
      stsz(&[4, 4]),
      boxed(
        b"stco",
        &[[0, 0, 0, 0, 0, 0, 0, 1].as_slice(), &first.to_be_bytes()].concat(),
      ),
    ]
    .concat();
    let minf = [boxed(b"vmhd", &[0; 12]), boxed(b"stbl", &stbl)].concat();
    let mdia = [hdlr(b"pict"), boxed(b"minf", &minf)].concat();
    let trak = [tkhd(1), boxed(b"mdia", &mdia)].concat();
    boxed(b"moov", &boxed(b"trak", &trak))
  };
  let file = |first: Option<u32>| {
    let mut sink = Sink::default();
    sink.append(&boxed(b"ftyp", b"avifmif1miafMA1B"));
    let (probe, extent) = avif_meta(0, 4, &[0x81, 0, 0, 0]);
    let start = sink.frontier + probe.len() as u64 + moov_of(0).len() as u64 + 8;
    let at = sink.append(&probe);
    sink.append(&moov_of(first.unwrap_or(start as u32)));
    sink.append(&sized(16, b"mdat", &[0x21; 8]));
    sink.patch(at + extent, &(start as u32).to_be_bytes());
    (sink, start)
  };
  let mut samples = SampleMirror::default();
  samples.sample(0, 4);
  samples.sample(1, 4);
  samples.end(1);
  let expected = || {
    [ExpectedTrak {
      track: Some(TrackIndex::new(0)),
      id: 1,
      samples,
      ..ExpectedTrak::default()
    }]
    .into_iter()
  };
  let (whole, start) = file(None);
  let audit = avif_walked(&whole, None);
  assert_eq!(audit.unfollowed(), None);
  assert_eq!(mov_verdict(&audit, expected(), &avif(8)), None);
  let (moved, _) = file(Some(start as u32 - 20));
  assert_eq!(
    mov_verdict(&avif_walked(&moved, None), expected(), &avif(8)),
    named(
      WrittenPart::ChunkOffsets,
      Found::Value(Counts::new(start - 20, start))
    )
  );
}

// ---------------------------------------------------------------------------
//  The protection headers, the user data and the roll groups (A1, A6, A7,
//  A3).
// ---------------------------------------------------------------------------

/// An `sgpd` of roll `distances`, each written negated in 16 bits, of
/// version 1 and a default length of 2 (`movenc.c` 3373–3385).
fn sgpd(distances: &[u16]) -> Vec<u8> {
  let mut payload = vec![1, 0, 0, 0];
  payload.extend_from_slice(b"roll");
  payload.extend_from_slice(&2u32.to_be_bytes());
  payload.extend_from_slice(&(distances.len() as u32).to_be_bytes());
  for distance in distances {
    payload.extend_from_slice(&distance.wrapping_neg().to_be_bytes());
  }
  boxed(b"sgpd", &payload)
}

/// An `sbgp` of `runs`, each its samples and its group's index
/// (3387–3396).
fn sbgp(runs: &[(u32, u32)]) -> Vec<u8> {
  let mut payload = vec![0; 4];
  payload.extend_from_slice(b"roll");
  payload.extend_from_slice(&(runs.len() as u32).to_be_bytes());
  for &(samples, index) in runs {
    payload.extend_from_slice(&samples.to_be_bytes());
    payload.extend_from_slice(&index.to_be_bytes());
  }
  boxed(b"sbgp", &payload)
}

/// What a synthetic file holds beside one audio track's tables: its roll
/// groups' boxes, its `udta`'s payload, the movie's, and each `pssh`'s.
#[derive(Clone, Default)]
struct Parts {
  stss: Option<Vec<u32>>,
  sdtp: Option<Vec<u8>>,
  sgpd: Option<Vec<u16>>,
  sbgp: Option<Vec<(u32, u32)>>,
  track: Option<usize>,
  movie: Option<usize>,
  pssh: Vec<usize>,
}

/// A whole MP4 file of one audio track of `sizes`, each sample one tick, and
/// `parts`, in movenc's order: the roll groups last in the `stbl`, the
/// track's `udta` last in its `trak`, the movie's after the `trak`s, then
/// the `pssh` boxes (4380–4441, 5446–5451).
fn whole_with(sizes: &[u32], parts: &Parts) -> Sink {
  let mut sink = Sink::default();
  let mdat = header(&mut sink);
  let payload: u32 = sizes.iter().sum();
  sink.append(&vec![0x21; payload as usize]);
  sink.close(mdat);
  let mut stbl = [boxed(b"stsd", &[0; 8]), stts(&[(sizes.len() as u32, 1)])].concat();
  // The flags after `stts`, as movenc orders them (3410–3422).
  if let Some(indices) = &parts.stss {
    stbl.extend(sync_table(b"stss", indices));
  }
  if let Some(bytes) = &parts.sdtp {
    stbl.extend(boxed(b"sdtp", &[&[0, 0, 0, 0][..], bytes].concat()));
  }
  stbl.extend(stsz(sizes));
  stbl.extend(boxed(b"stco", &[0; 8]));
  if let Some(distances) = &parts.sgpd {
    stbl.extend(sgpd(distances));
  }
  if let Some(runs) = &parts.sbgp {
    stbl.extend(sbgp(runs));
  }
  let minf = [boxed(b"smhd", &[0; 8]), boxed(b"stbl", &stbl)].concat();
  let mdia = [hdlr(b"soun"), boxed(b"minf", &minf)].concat();
  let mut trak = [tkhd(1), boxed(b"mdia", &mdia)].concat();
  if let Some(len) = parts.track {
    trak.extend(boxed(b"udta", &vec![0x55; len]));
  }
  let mut moov = boxed(b"trak", &trak);
  if let Some(len) = parts.movie {
    moov.extend(boxed(b"udta", &vec![0x56; len]));
  }
  for &len in &parts.pssh {
    moov.extend(boxed(b"pssh", &vec![0x57; len]));
  }
  sink.append(&boxed(b"moov", &moov));
  sink
}

/// AAC's roll groups of `samples` samples, as the muxer counts them: one
/// group of a distance of 1.
fn aac_groups(samples: u64) -> Option<RollGroups> {
  let mut groups = RollMirror::default();
  groups.samples(1, samples);
  groups.written()
}

/// The movie's parts the muxer expects: MP4's `udta` of `user_data` bytes
/// and a `pssh` of each of `pssh`'s payload bytes.
fn movie_of(user_data: MovieUserData, pssh: &[u64]) -> ExpectedMovie {
  let mut protection = Runs::default();
  for &len in pssh {
    protection.push(len, 1);
  }
  ExpectedMovie {
    media: None,
    user_data,
    protection,
  }
}

/// **The roll groups, a track's and the movie's user data and the
/// protection headers are each judged, a part left out named** (A3, A6,
/// A7, A1): a file of AAC's roll groups, a track `udta` of a DASH role, the
/// movie's `udta` and two `pssh` boxes passes as handed; without the roll
/// groups, the track's roll groups are named, omitted; without its `udta`,
/// the track's user data; without the movie's, the movie's user data;
/// without a `pssh`, the protection headers by their count, and one of
/// another length by their bytes. A track's parts are named before the
/// movie's.
#[test]
fn the_roll_groups_user_data_and_protection_headers_are_judged() {
  let whole = Parts {
    sgpd: Some(vec![1]),
    sbgp: Some(vec![(3, 1)]),
    track: Some(44),
    movie: Some(100),
    pssh: vec![60, 70],
    ..Parts::default()
  };
  let trak = || ExpectedTrak {
    rolls: aac_groups(3),
    user_data: 44,
    ..expected(&[4, 5, 4])
  };
  let movie = movie_of(MovieUserData::Exact(100), &[60, 70]);
  let judged = |parts: &Parts| {
    let audit = whole_with(&[4, 5, 4], parts).walked();
    assert_eq!(audit.unfollowed(), None);
    mov_verdict(&audit, [trak()].into_iter(), &movie)
  };
  assert_eq!(judged(&whole), None);
  let without = |change: fn(&mut Parts)| {
    let mut parts = whole.clone();
    change(&mut parts);
    judged(&parts)
  };
  assert_eq!(
    without(|parts| (parts.sgpd, parts.sbgp) = (None, None)),
    named(WrittenPart::RollGroups, Found::Omitted)
  );
  assert_eq!(
    without(|parts| parts.track = None),
    named(WrittenPart::TrackUserData, Found::Omitted)
  );
  assert_eq!(
    without(|parts| (parts.track, parts.movie) = (None, None)),
    named(WrittenPart::TrackUserData, Found::Omitted),
    "the track's first"
  );
  let file = |part: WrittenPart, found: Found| Some(WrittenOtherwise::new(None, part, found));
  assert_eq!(
    without(|parts| parts.movie = None),
    file(WrittenPart::MovieUserData, Found::Omitted)
  );
  assert_eq!(
    without(|parts| parts.movie = Some(99)),
    file(
      WrittenPart::MovieUserData,
      Found::Length(Counts::new(99, 100))
    )
  );
  assert_eq!(
    without(|parts| parts.pssh = vec![60]),
    file(
      WrittenPart::ProtectionHeaders,
      Found::Count(Counts::new(1, 2))
    )
  );
  assert_eq!(
    without(|parts| parts.pssh = vec![60, 71]),
    file(
      WrittenPart::ProtectionHeaders,
      Found::Length(Counts::new(131, 130))
    )
  );
  assert_eq!(
    without(|parts| parts.pssh = vec![70, 60]),
    file(WrittenPart::ProtectionHeaders, Found::Content)
  );
}

/// **MOV's movie `udta` passes with libavformat's name or without** (the
/// owner's ruling on letter 1, (b)): of its Nero list alone, or of the list
/// and the name, it passes; of another length it is named against both;
/// left out, where the list is some, it is omitted; where the list is
/// none, no `udta` at all passes, as does one of the name alone.
#[test]
fn movs_movie_udta_passes_with_libavformats_name_or_without() {
  let judged = |movie: Option<usize>, len: u64| {
    let parts = Parts {
      movie,
      ..Parts::default()
    };
    let audit = whole_with(&[4], &parts).walked();
    let expected = movie_of(MovieUserData::NameOptional { len, name: 24 }, &[]);
    mov_verdict(&audit, [expected_one(4)].into_iter(), &expected)
  };
  assert_eq!(judged(Some(20), 20), None, "the Nero list alone");
  assert_eq!(judged(Some(44), 20), None, "the list and the name");
  assert_eq!(
    judged(Some(30), 20),
    Some(WrittenOtherwise::new(
      None,
      WrittenPart::MovieUserData,
      Found::Length(Counts::new(30, 44))
    ))
  );
  assert_eq!(
    judged(None, 20),
    Some(WrittenOtherwise::new(
      None,
      WrittenPart::MovieUserData,
      Found::Omitted
    ))
  );
  assert_eq!(judged(None, 0), None, "no list, no name: none");
  assert_eq!(judged(Some(24), 0), None, "the name alone");
}

/// The expectation of one sample of `size` bytes, as [`whole_with`] writes
/// it.
fn expected_one(size: u32) -> ExpectedTrak {
  expected(&[size])
}

/// **The roll groups are judged by their groups' count, their runs', then
/// their content** (A3): groups where the muxer expects none are named by
/// their count; one run moved into two, by the runs'; a group of another
/// distance, by the content; the `sgpd` without its `sbgp`, omitted; and
/// an `sgpd` stating more entries than its payload holds is a structure no
/// writer makes.
#[test]
fn the_roll_groups_are_judged_by_count_runs_then_content() {
  let judged = |parts: &Parts, rolls: Option<RollGroups>| {
    let audit = whole_with(&[4, 5, 4], parts).walked();
    assert_eq!(audit.unfollowed(), None);
    let trak = ExpectedTrak {
      rolls,
      ..expected(&[4, 5, 4])
    };
    mov_verdict(&audit, [trak].into_iter(), &mp4())
  };
  let aac = Parts {
    sgpd: Some(vec![1]),
    sbgp: Some(vec![(3, 1)]),
    ..Parts::default()
  };
  assert_eq!(judged(&aac, aac_groups(3)), None);
  assert_eq!(
    judged(&aac, None),
    named(WrittenPart::RollGroups, Found::Count(Counts::new(1, 0)))
  );
  let split = Parts {
    sbgp: Some(vec![(2, 1), (1, 1)]),
    ..aac.clone()
  };
  assert_eq!(
    judged(&split, aac_groups(3)),
    named(WrittenPart::RollGroups, Found::Count(Counts::new(2, 1)))
  );
  let other = Parts {
    sgpd: Some(vec![2]),
    ..aac.clone()
  };
  assert_eq!(
    judged(&other, aac_groups(3)),
    named(WrittenPart::RollGroups, Found::Content)
  );
  let lone = Parts {
    sbgp: None,
    ..aac.clone()
  };
  assert_eq!(
    judged(&lone, aac_groups(3)),
    named(WrittenPart::RollGroups, Found::Omitted)
  );
  // Opus's: distances 0, 4, 4, 3 and 4 again, a group a run of a distance
  // other than none, the second 4 its own.
  let mut opus = RollMirror::default();
  for distance in [0, 0, 4, 4, 3, 4] {
    opus.samples(distance, 1);
  }
  let parts = Parts {
    sgpd: Some(vec![4, 3, 4]),
    sbgp: Some(vec![(2, 0), (2, 1), (1, 2), (1, 3)]),
    ..Parts::default()
  };
  let audit = whole_with(&[4, 4, 4, 4, 4, 4], &parts).walked();
  let trak = ExpectedTrak {
    rolls: opus.written(),
    ..expected(&[4, 4, 4, 4, 4, 4])
  };
  assert_eq!(mov_verdict(&audit, [trak].into_iter(), &mp4()), None);
  // More entries stated than the payload holds.
  let mut short = sgpd(&[1, 1]);
  // The count's last byte: the box's header, then 15 bytes of fields.
  short[23] = 3;
  let mut sink = Sink::default();
  let mdat = header(&mut sink);
  sink.append(&[0x21; 4]);
  sink.close(mdat);
  let stbl = [boxed(b"stsd", &[0; 8]), stts(&[(1, 1)]), stsz(&[4]), short].concat();
  let mdia = [hdlr(b"soun"), boxed(b"minf", &boxed(b"stbl", &stbl))].concat();
  let trak = [tkhd(1), boxed(b"mdia", &mdia)].concat();
  sink.append(&boxed(b"moov", &boxed(b"trak", &trak)));
  assert_eq!(
    sink
      .walked()
      .unfollowed()
      .map(|unfollowed| unfollowed.why()),
    Some(Why::Structure)
  );
}

/// A sync sample table `fourcc` of `indices`, each from 1
/// (`mov_write_stss_tag`, `movenc.c` 292–313).
fn sync_table(fourcc: &[u8; 4], indices: &[u32]) -> Vec<u8> {
  let mut payload = vec![0; 4];
  payload.extend_from_slice(&(indices.len() as u32).to_be_bytes());
  for index in indices {
    payload.extend_from_slice(&index.to_be_bytes());
  }
  boxed(fourcc, &payload)
}

/// The flag tables the muxer counts of samples of `flags`, each whether it
/// is a sync sample and whether it is disposable, of a video track.
fn flag_tables(flags: &[(bool, bool)]) -> Option<FlagTables> {
  let mut mirror = FlagsMirror::default();
  for &(sync, disposable) in flags {
    mirror.sample(sync, disposable);
  }
  Some(mirror.written(true, true))
}

/// **The flags are judged by their entries' count, then their content**:
/// a video track of samples sync, disposable and sync again — its
/// `stss` of the first and third, its `sdtp` of each — passes as handed;
/// `stss` left out is the sync samples omitted, of one entry their count,
/// of another index their content; `sdtp` left out is the dependencies
/// omitted, of another byte their content; an `stss` where every sample is
/// a sync sample, which movenc writes none of, is named by its count; and a
/// track whose flags are unjudged, VC-1's, passes whatever it holds.
#[test]
fn the_flags_are_judged_by_count_then_content() {
  let (sync, neither) = (true, false);
  let sizes = [4, 5, 4];
  let whole = Parts {
    stss: Some(vec![1, 3]),
    sdtp: Some(vec![0x20, 0x18, 0x20]),
    ..Parts::default()
  };
  let judged = |parts: &Parts, flags: Option<FlagTables>| {
    let audit = whole_with(&sizes, parts).walked();
    assert_eq!(audit.unfollowed(), None);
    let trak = ExpectedTrak {
      flags,
      ..expected(&sizes)
    };
    mov_verdict(&audit, [trak].into_iter(), &mp4())
  };
  let handed = flag_tables(&[(sync, false), (neither, true), (sync, false)]);
  assert_eq!(judged(&whole, handed), None);
  let with = |change: fn(&mut Parts)| {
    let mut parts = whole.clone();
    change(&mut parts);
    judged(&parts, handed)
  };
  assert_eq!(
    with(|parts| parts.stss = None),
    named(WrittenPart::SyncSamples, Found::Omitted)
  );
  assert_eq!(
    with(|parts| parts.stss = Some(vec![1])),
    named(WrittenPart::SyncSamples, Found::Count(Counts::new(1, 2)))
  );
  assert_eq!(
    with(|parts| parts.stss = Some(vec![1, 2])),
    named(WrittenPart::SyncSamples, Found::Content)
  );
  assert_eq!(
    with(|parts| parts.sdtp = None),
    named(WrittenPart::Dependencies, Found::Omitted)
  );
  assert_eq!(
    with(|parts| parts.sdtp = Some(vec![0x20, 0x10, 0x20])),
    named(WrittenPart::Dependencies, Found::Content)
  );
  let every = flag_tables(&[(sync, false), (sync, false), (sync, false)]);
  let listed = Parts {
    stss: Some(vec![1, 2, 3]),
    ..Parts::default()
  };
  assert_eq!(
    judged(&listed, every),
    named(WrittenPart::SyncSamples, Found::Count(Counts::new(3, 0)))
  );
  assert_eq!(judged(&listed, None), None, "unjudged");
}

/// **An `stss` whose payload holds no whole number of entries is a
/// structure no writer makes**.
#[test]
fn an_stss_of_part_of_an_entry_is_a_structure() {
  let mut sink = Sink::default();
  let mdat = header(&mut sink);
  sink.append(&[0x21; 4]);
  sink.close(mdat);
  let mut cut = sync_table(b"stss", &[1]);
  cut.extend_from_slice(&[0, 0]);
  cut[3] += 2;
  let stbl = [boxed(b"stsd", &[0; 8]), stts(&[(1, 1)]), cut, stsz(&[4])].concat();
  let mdia = [hdlr(b"vide"), boxed(b"minf", &boxed(b"stbl", &stbl))].concat();
  let trak = [tkhd(1), boxed(b"mdia", &mdia)].concat();
  sink.append(&boxed(b"moov", &boxed(b"trak", &trak)));
  assert_eq!(
    sink
      .walked()
      .unfollowed()
      .map(|unfollowed| unfollowed.why()),
    Some(Why::Structure)
  );
}

// ---------------------------------------------------------------------------
//  FLV's walk (A22, A26; R10, R11, R12; the design gate's condition 6).
// ---------------------------------------------------------------------------

/// FLV's file header, of audio and video, and the first PreviousTagSize.
fn flv_header() -> Vec<u8> {
  vec![b'F', b'L', b'V', 1, 5, 0, 0, 0, 9, 0, 0, 0, 0]
}

/// A tag of `kind` whose header's size reads `size`, holding `data`, then
/// its PreviousTagSize, of its data and 11.
fn flv_tag(kind: u8, size: u32, data: &[u8]) -> Vec<u8> {
  let mut bytes = vec![kind];
  bytes.extend_from_slice(&size.to_be_bytes()[1..]);
  bytes.extend_from_slice(&[0; 7]);
  bytes.extend_from_slice(data);
  bytes.extend_from_slice(&(data.len() as u32 + 11).to_be_bytes());
  bytes
}

/// A tag of `kind` of its true size, holding `data`.
fn flv_whole(kind: u8, data: &[u8]) -> Vec<u8> {
  flv_tag(kind, data.len() as u32, data)
}

/// `onMetaData`'s data: an AMF string, then an array of no entry's count.
fn on_meta_data() -> Vec<u8> {
  let mut data = vec![2, 0, 10];
  data.extend_from_slice(b"onMetaData");
  data.extend_from_slice(&[8, 0, 0, 0, 0, 0, 0, 9]);
  data
}

/// One AAC track, as the walk is told it: legacy AAC, its sequence header
/// of 2 bytes before its record.
const AAC: FlvTrack = FlvTrack {
  kind: 8,
  index: 0,
  header: 2,
  emptiness: false,
};

/// The walk of an FLV file of `tracks` and `pre_tags`, over `sink`'s runs.
fn flv_walked(sink: &Sink, tracks: &[FlvTrack], pre_tags: usize) -> Audit {
  let mut audit = Audit::flv(tracks, pre_tags);
  for (at, bytes) in &sink.runs {
    audit.took(*at, bytes);
  }
  audit
}

/// AAC's track as the muxer expects it: one sequence header of `record`
/// bytes, and frames of `sizes`.
fn aac_expected(record: u64, sizes: &[u64]) -> ExpectedFlv {
  let mut sequences = Runs::default();
  sequences.push(record, 1);
  let mut frames = Runs::default();
  for &size in sizes {
    frames.push(size, 1);
  }
  ExpectedFlv {
    track: TrackIndex::new(0),
    sequences: sequences.summary(),
    records: true,
    frames: frames.summary(),
  }
}

/// An FLV file whole: its header, `onMetaData`, AAC's sequence header of a
/// 2-byte configuration, and frames of `payloads` bytes, each its 2 flags'
/// beside: the runs, every size final in the stream.
fn flv_whole_file(payloads: &[usize]) -> Sink {
  let mut sink = Sink::default();
  sink.append(&flv_header());
  sink.append(&flv_whole(18, &on_meta_data()));
  sink.append(&flv_whole(8, &[0xAF, 0, 0x12, 0x10]));
  for &payload in payloads {
    let mut data = vec![0xAF, 1];
    data.extend(core::iter::repeat_n(0x21, payload));
    sink.append(&flv_whole(8, &data));
  }
  sink
}

/// **An FLV file whole passes; its frames and sequence headers are judged**
/// (A26, A22): each frame's data size — its payload and its 2 flags — and
/// the sequence header's record; one frame of another size is named by the
/// frames' sum, one fewer by their count, the record by its length.
#[test]
fn an_flv_files_frames_and_sequence_headers_are_judged() {
  let audit = flv_walked(&flv_whole_file(&[10, 12]), &[AAC], 0);
  assert_eq!(audit.unfollowed(), None);
  let judged = |expected: ExpectedFlv| flv_verdict(&audit, [expected].into_iter());
  assert_eq!(judged(aac_expected(2, &[12, 14])), None);
  let named = |part, found| Some(WrittenOtherwise::new(Some(TrackIndex::new(0)), part, found));
  assert_eq!(
    judged(aac_expected(2, &[12, 15])),
    named(WrittenPart::Frames, Found::Length(Counts::new(26, 27)))
  );
  assert_eq!(
    judged(aac_expected(2, &[12, 14, 14])),
    named(WrittenPart::Frames, Found::Count(Counts::new(2, 3)))
  );
  assert_eq!(
    judged(aac_expected(5, &[12, 14])),
    named(
      WrittenPart::Configuration(0),
      Found::Length(Counts::new(2, 5))
    )
  );
}

/// **A placeholder size is closed by its rewrite at the frontier, before its
/// PreviousTagSize, in parts or whole**: AAC's sequence header written of
/// size 0, flushed, its data, then its size rewritten — in one write, or in
/// two — then its PreviousTagSize: followed; a rewrite of another size is a
/// structure no writer makes.
#[test]
fn a_placeholder_size_is_closed_by_its_rewrite() {
  let file = |parts: &[&[u8]]| {
    let mut sink = Sink::default();
    sink.append(&flv_header());
    let at = sink.append(&flv_tag(8, 0, &[])[..11]);
    sink.append(&[0xAF, 0, 0x12, 0x10]);
    let mut field = at + 1;
    for part in parts {
      sink.patch(field, part);
      field += part.len() as u64;
    }
    sink.append(&15u32.to_be_bytes());
    sink
  };
  let followed = |sink: &Sink| flv_walked(sink, &[AAC], 0).unfollowed();
  assert_eq!(followed(&file(&[&[0, 0, 4]])), None);
  assert_eq!(followed(&file(&[&[0], &[0, 4]])), None, "in two parts");
  assert_eq!(
    followed(&file(&[&[0, 0, 5]])).map(|unfollowed| unfollowed.why()),
    Some(Why::Structure)
  );
}

/// **A PreviousTagSize other than its tag's is a structure no writer makes**
/// (A26's road among them).
///
/// PLANT P618: the PreviousTagSize not read.
#[test]
fn a_previous_tag_size_off_by_one_is_a_structure() {
  let mut sink = Sink::default();
  sink.append(&flv_header());
  let mut tag = flv_whole(8, &[0xAF, 1, 0x21, 0x21]);
  let last = tag.len() - 1;
  tag[last] += 1;
  let at = sink.append(&tag);
  assert_eq!(
    flv_walked(&sink, &[AAC], 0).unfollowed(),
    Some(Unfollowed::new(at + tag.len() as u64 - 4, Why::Structure))
  );
}

/// **A file ending inside a tag is truncated at the tag's offset; a tag of
/// another type is a structure** — A26's road: a tag's size of 0xFFFFC5,
/// its flags and nothing after.
#[test]
fn an_flv_file_ending_inside_a_tag_is_truncated_at_it() {
  let mut sink = Sink::default();
  sink.append(&flv_header());
  let at = sink.append(&[9, 0xFF, 0xFF, 0xC5, 0, 0, 0, 0, 0, 0, 0, 0x17, 1, 0, 0, 0]);
  sink.append(&0xFFFF_FFD0_u32.to_be_bytes());
  assert_eq!(
    flv_walked(&sink, &[], 0).unfollowed(),
    Some(Unfollowed::new(at, Why::Truncated))
  );
  let mut other = Sink::default();
  other.append(&flv_header());
  let at = other.append(&flv_whole(7, &[1, 2, 3]));
  assert_eq!(
    flv_walked(&other, &[], 0).unfollowed(),
    Some(Unfollowed::new(at, Why::Structure))
  );
}

/// **R10 — HDR metadata's video tag of size 5 is a placeholder, of both
/// faces**: its size final in the stream (the in-buffer face) and its size
/// read as 5 then rewritten after its colorInfo (the flushed face) are each
/// followed; a video tag of size 5 opening on another byte is final, its
/// PreviousTagSize of 16 read.
///
/// PLANT P630: R10's predicate removed.
#[test]
fn r10_hdr_metadatas_size_of_5_is_a_placeholder() {
  let body = [
    0xD4, b'h', b'v', b'c', b'1', 2, 0, 9, b'c', b'o', b'l', b'o', b'r', b'I', b'n', b'f', b'o', 3,
    0, 0, 9,
  ];
  let mut whole = Sink::default();
  whole.append(&flv_header());
  whole.append(&flv_whole(9, &body));
  assert_eq!(flv_walked(&whole, &[], 0).unfollowed(), None, "in-buffer");
  let mut flushed = Sink::default();
  flushed.append(&flv_header());
  let at = flushed.append(&flv_tag(9, 5, &[])[..11]);
  flushed.append(&body);
  flushed.patch(at + 1, &(body.len() as u32).to_be_bytes()[1..]);
  flushed.append(&(body.len() as u32 + 11).to_be_bytes());
  assert_eq!(flv_walked(&flushed, &[], 0).unfollowed(), None, "flushed");
  let mut final_five = Sink::default();
  final_five.append(&flv_header());
  final_five.append(&flv_whole(9, &[0x27, 1, 0, 0, 0]));
  assert_eq!(flv_walked(&final_five, &[], 0).unfollowed(), None, "final");
}

/// **R11 — VP6A's pre-tags are read exactly as many as stated**: a header
/// of one and of two, the walk told so, followed; told one fewer, the
/// pre-tag is a structure at its offset; told one more, at the first tag's;
/// a placeholder audio tag opening on 0x00 — a pre-tag the walk was not
/// told of — a structure at its offset.
#[test]
fn r11_vp6as_pre_tags_are_read_as_many_as_stated() {
  let pre_tag = [8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 11];
  let file = |pre_tags: usize| {
    let mut sink = Sink::default();
    sink.append(&flv_header());
    for _ in 0..pre_tags {
      sink.append(&pre_tag);
    }
    sink.append(&flv_whole(18, &on_meta_data()));
    sink
  };
  for count in [1, 2] {
    assert_eq!(flv_walked(&file(count), &[], count).unfollowed(), None);
  }
  assert_eq!(
    flv_walked(&file(2), &[], 1).unfollowed(),
    Some(Unfollowed::new(13 + 15, Why::Structure)),
    "told one fewer"
  );
  assert_eq!(
    flv_walked(&file(1), &[], 2).unfollowed(),
    Some(Unfollowed::new(13 + 15, Why::Structure)),
    "told one more"
  );
  assert_eq!(
    flv_walked(&file(1), &[], 0).unfollowed(),
    Some(Unfollowed::new(13, Why::Structure)),
    "not told"
  );
}

/// **A script tag's provisional size, the design gate's condition 6**: a
/// TEXT cue's tag written of its payload's size and one, its AMF one to
/// three bytes longer — the rewrite arriving with fewer than 4 lookahead
/// bytes held — or shorter — the rewrite arriving before the provisional
/// end, a data stream's pass-through — each followed; a provisional size
/// that was the true one, its PreviousTagSize read after it, followed.
#[test]
fn a_script_tags_provisional_size_is_held_until_its_rewrite() {
  let file = |provisional: u32, data: &[u8], rewritten: bool| {
    let mut sink = Sink::default();
    sink.append(&flv_header());
    sink.append(&flv_whole(18, &on_meta_data()));
    let at = sink.append(&flv_tag(18, provisional, &[])[..11]);
    sink.append(data);
    if rewritten {
      sink.patch(at + 1, &(data.len() as u32).to_be_bytes()[1..]);
    }
    sink.append(&(data.len() as u32 + 11).to_be_bytes());
    sink
  };
  let amf = [
    2, 0, 10, b'o', b'n', b'T', b'e', b'x', b't', b'D', b'a', b't', b'a', 0, 0, 9,
  ];
  for longer in 1..=3 {
    let provisional = amf.len() as u32 - longer;
    assert_eq!(
      flv_walked(&file(provisional, &amf, true), &[], 0).unfollowed(),
      None,
      "the tail case, {longer} past"
    );
  }
  assert_eq!(
    flv_walked(&file(amf.len() as u32 + 1, &amf, true), &[], 0).unfollowed(),
    None,
    "pass-through"
  );
  assert_eq!(
    flv_walked(&file(amf.len() as u32, &amf, false), &[], 0).unfollowed(),
    None,
    "final"
  );
}

/// **`onMetaData`'s window**: its count written while it is open and its
/// duration and file size after it closes are each admitted; a write below
/// the frontier outside it is a rewrite of no field.
#[test]
fn on_meta_datas_window_admits_its_rewrites() {
  let mut sink = Sink::default();
  sink.append(&flv_header());
  let at = sink.append(&flv_tag(18, 0, &[])[..11]);
  let data = on_meta_data();
  sink.append(&data);
  // The array's count, while open.
  sink.patch(at + 11 + 14, &[0, 0, 0, 2]);
  sink.patch(at + 1, &(data.len() as u32).to_be_bytes()[1..]);
  sink.append(&(data.len() as u32 + 11).to_be_bytes());
  sink.append(&flv_whole(8, &[0xAF, 0, 0x12, 0x10]));
  // The trailer's duration, inside the closed window.
  sink.patch(at + 11 + 4, &[1, 2, 3, 4]);
  assert_eq!(flv_walked(&sink, &[AAC], 0).unfollowed(), None);
  let mut outside = sink;
  outside.patch(at + 11 + data.len() as u64 + 6, &[0]);
  assert_eq!(
    flv_walked(&outside, &[AAC], 0)
      .unfollowed()
      .map(|unfollowed| unfollowed.why()),
    Some(Why::Rewrite)
  );
}

// ---------------------------------------------------------------------------
//  Matroska's and WebM's walk (A21, A4's part).
// ---------------------------------------------------------------------------

/// An EBML element of `id` holding `payload`, its size in one byte.
fn ebml(id: &[u8], payload: &[u8]) -> Vec<u8> {
  let mut bytes = id.to_vec();
  bytes.push(0x80 | u8::try_from(payload.len()).expect("a one-byte size"));
  bytes.extend_from_slice(payload);
  bytes
}

/// The EBML element IDs the synthetic files use.
const EBML_ID: [u8; 4] = [0x1A, 0x45, 0xDF, 0xA3];
const SEGMENT_ID: [u8; 4] = [0x18, 0x53, 0x80, 0x67];
const INFO_ID: [u8; 4] = [0x15, 0x49, 0xA9, 0x66];
const TRACKS_ID: [u8; 4] = [0x16, 0x54, 0xAE, 0x6B];
const TAGS_ID: [u8; 4] = [0x12, 0x54, 0xC3, 0x67];
const CLUSTER_ID: [u8; 4] = [0x1F, 0x43, 0xB6, 0x75];

/// Tracks of one TrackEntry per CodecPrivate in `privates`.
fn tracks_of(privates: &[&[u8]]) -> Vec<u8> {
  let entries: Vec<u8> = privates
    .iter()
    .flat_map(|private| {
      ebml(
        &[0xAE],
        &[ebml(&[0xD7], &[1]), ebml(&[0x63, 0xA2], private)].concat(),
      )
    })
    .collect();
  ebml(&TRACKS_ID, &entries)
}

/// A Matroska file as its header and a packet write it: the EBML header;
/// the Segment of its unknown size; the SeekHead's Void, Info, `tracks`,
/// Tags; then a Cluster. Answers the sink and each window's offset.
fn mkv_file(tracks: &[u8]) -> (Sink, [u64; 4]) {
  let mut sink = Sink::default();
  sink.append(&ebml(&EBML_ID, &[0x42, 0x82, 0x81, 0x01]));
  sink.append(
    &[
      SEGMENT_ID.as_slice(),
      &[0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF],
    ]
    .concat(),
  );
  let void = sink.append(&ebml(&[0xEC], &[0; 20]));
  let info = sink.append(&ebml(&INFO_ID, &[0x2A, 0xD7, 0xB1, 0x83, 0x0F, 0x42, 0x40]));
  let tracks = sink.append(tracks);
  let tags = sink.append(&ebml(&TAGS_ID, &[0xEC, 0x82, 0, 0]));
  sink.append(&ebml(
    &CLUSTER_ID,
    &[0xE7, 0x81, 0x00, 0xA3, 0x84, 0x81, 0, 0, 0x80],
  ));
  (sink, [void, info, tracks, tags])
}

/// The trailer's rewrite of `file`, in one run from the Segment's size: its
/// size of `size` bytes, the SeekHead's Void, Info, `tracks` and Tags, each
/// as long as its window.
fn rewrite(sink: &mut Sink, windows: [u64; 4], size: u64, tracks: &[u8]) {
  let segment = windows[0];
  let mut run = vec![0x01];
  run.extend_from_slice(&size.to_be_bytes()[1..]);
  run.extend_from_slice(&ebml(&[0xEC], &[0; 20]));
  run.extend_from_slice(&ebml(&INFO_ID, &[0x2A, 0xD7, 0xB1, 0x83, 0x0F, 0x42, 0x40]));
  run.extend_from_slice(tracks);
  run.extend_from_slice(&ebml(&TAGS_ID, &[0xEC, 0x82, 0, 0]));
  sink.patch(segment - 8, &run);
}

/// A FLAC CodecPrivate as Matroska writes it: `fLaC`, a STREAMINFO block,
/// then a VORBIS_COMMENT block of a 9-byte vendor and, of a `count` or an
/// `entry`, the count and the entry; of neither, the vendor alone (A21's
/// road).
fn flac_private(count: u8, entry: &[u8]) -> Vec<u8> {
  let mut block = 9_u32.to_le_bytes().to_vec();
  block.extend_from_slice(b"Lavf62.3.");
  if count > 0 || !entry.is_empty() {
    block.extend_from_slice(&u32::from(count).to_le_bytes());
    block.extend_from_slice(&(entry.len() as u32).to_le_bytes());
    block.extend_from_slice(entry);
  }
  let mut private = b"fLaC\0\0\0\x22".to_vec();
  private.extend_from_slice(&[0x11; 34]);
  private.push(0x84);
  private.extend_from_slice(&(block.len() as u32).to_be_bytes()[1..]);
  private.extend_from_slice(&block);
  private
}

/// FLAC's mask comment entry of a 2.1 layout.
const MASK_ENTRY: &[u8] = b"WAVEFORMATEXTENSIBLE_CHANNEL_MASK=0xb";

/// The Matroska walk of `tracks`' fold offsets over `sink`'s runs.
fn mkv_walked(sink: &Sink, folds: &[Option<u64>]) -> Audit {
  let tracks: Vec<MkvTrack> = folds
    .iter()
    .map(|&fold_from| MkvTrack { fold_from })
    .collect();
  let mut audit = Audit::mkv(&tracks);
  for (at, bytes) in &sink.runs {
    audit.took(*at, bytes);
  }
  audit
}

/// The verdict of `audit` of one TrackEntry expected `private`.
fn mkv_judged(audit: &Audit, private: MkvPrivate) -> Option<WrittenOtherwise> {
  mkv_verdict(audit, [(TrackIndex::new(0), private)].into_iter(), 1)
}

/// **A Matroska file and its trailer's rewrite are followed, the rewrite's
/// Tracks the one judged**: the header's FLAC CodecPrivate of a count of
/// 0, the rewrite's of 1, in one run from the Segment's size across the
/// SeekHead's Void, Info, Tracks and Tags; the Segment's size its bytes.
#[test]
fn a_matroska_file_and_its_trailers_rewrite_are_followed() {
  let (header, rewritten) = (flac_private(0, MASK_ENTRY), flac_private(1, MASK_ENTRY));
  let (mut sink, windows) = mkv_file(&tracks_of(&[&header]));
  let taken = sink.frontier - windows[0];
  let expected = |private: &[u8]| MkvPrivate::Comment {
    len: private.len() as u64,
    fold: config_fold(CONFIG_SEED, &private[42..]),
  };
  assert_eq!(
    mkv_judged(
      &mkv_walked(&sink, &[Some(FLAC_HEADER)]),
      expected(&rewritten)
    ),
    Some(WrittenOtherwise::new(
      Some(TrackIndex::new(0)),
      WrittenPart::ChannelMask,
      Found::Content
    )),
    "the header's, before the rewrite"
  );
  rewrite(&mut sink, windows, taken, &tracks_of(&[&rewritten]));
  let audit = mkv_walked(&sink, &[Some(FLAC_HEADER)]);
  assert_eq!(audit.unfollowed(), None);
  assert_eq!(
    mkv_judged(&audit, expected(&rewritten)),
    None,
    "the rewrite's"
  );
  assert_eq!(
    mkv_judged(&audit, MkvPrivate::Length(header.len() as u64)),
    None
  );
}

/// **The Segment's size is judged against the bytes after its header**:
/// written one short, named; never written, the unknown size named.
#[test]
fn the_segments_size_is_judged() {
  let (sink, windows) = mkv_file(&tracks_of(&[&[1]]));
  let taken = sink.frontier - windows[0];
  let mut short = Sink {
    runs: sink.runs.clone(),
    frontier: sink.frontier,
  };
  rewrite(&mut short, windows, taken - 1, &tracks_of(&[&[1]]));
  assert_eq!(
    mkv_judged(&mkv_walked(&short, &[None]), MkvPrivate::Unjudged),
    Some(WrittenOtherwise::new(
      None,
      WrittenPart::Structure,
      Found::Length(Counts::new(taken - 1, taken))
    ))
  );
  // No trailer at all: the size stays unknown.
  assert_eq!(
    mkv_judged(&mkv_walked(&sink, &[None]), MkvPrivate::Unjudged),
    Some(WrittenOtherwise::new(
      None,
      WrittenPart::Structure,
      Found::Length(Counts::new((1 << 56) - 1, taken))
    ))
  );
}

/// **The windows are rewritten in order and once; nothing else is**: Info
/// rewritten a second time, a write inside the Cluster, and Tracks
/// rewritten part way at the end.
#[test]
fn a_matroska_window_is_rewritten_once_and_nothing_else_is() {
  let tracks = tracks_of(&[&[1, 2]]);
  let (base, windows) = mkv_file(&tracks);
  let taken = base.frontier - windows[0];
  let whole = || {
    let mut sink = Sink {
      runs: base.runs.clone(),
      frontier: base.frontier,
    };
    rewrite(&mut sink, windows, taken, &tracks);
    sink
  };
  let why = |sink: &Sink| mkv_walked(sink, &[None]).unfollowed();
  let mut twice = whole();
  twice.patch(
    windows[1],
    &ebml(&INFO_ID, &[0x2A, 0xD7, 0xB1, 0x83, 0x0F, 0x42, 0x40]),
  );
  assert_eq!(why(&twice), Some(Unfollowed::new(windows[1], Why::Rewrite)));
  let mut inside = whole();
  inside.patch(base.frontier - 3, &[0]);
  assert_eq!(
    why(&inside),
    Some(Unfollowed::new(base.frontier - 3, Why::Rewrite))
  );
  let mut part = Sink {
    runs: base.runs.clone(),
    frontier: base.frontier,
  };
  part.patch(windows[2], &tracks[..3]);
  assert_eq!(
    why(&part),
    Some(Unfollowed::new(windows[2], Why::Unresolved))
  );
}

/// **A Matroska file ending inside an element is truncated at it; an ID of
/// five bytes is no writer's.**
#[test]
fn a_matroska_file_ending_inside_a_cluster_is_truncated_at_it() {
  let (mut sink, _) = mkv_file(&tracks_of(&[&[1]]));
  let cluster = sink.append(&[CLUSTER_ID.as_slice(), &[0x88, 0xE7, 0x81]].concat());
  assert_eq!(
    mkv_walked(&sink, &[None]).unfollowed(),
    Some(Unfollowed::new(cluster, Why::Truncated))
  );
  let (mut five, _) = mkv_file(&tracks_of(&[&[1]]));
  let at = five.append(&[0x08, 1, 2, 3, 4, 0x80]);
  assert_eq!(
    mkv_walked(&five, &[None]).unfollowed(),
    Some(Unfollowed::new(at, Why::Structure))
  );
}

/// **FLAC's channel mask comment and AV1's `av1C` are judged from the final
/// Tracks** (A21, A4): the comment equal passes; its count 0, named by its
/// content; cut to the vendor alone, by its length; AV1's of another length
/// named; one TrackEntry where two streams are, by the count.
#[test]
fn flacs_comment_and_av1s_record_are_judged() {
  let written = |private: &[u8]| {
    let tracks = tracks_of(&[private]);
    let (mut sink, windows) = mkv_file(&tracks);
    let taken = sink.frontier - windows[0];
    rewrite(&mut sink, windows, taken, &tracks);
    mkv_walked(&sink, &[Some(FLAC_HEADER)])
  };
  let whole = flac_private(1, MASK_ENTRY);
  let expected = MkvPrivate::Comment {
    len: whole.len() as u64,
    fold: config_fold(CONFIG_SEED, &whole[42..]),
  };
  let named = |found| {
    Some(WrittenOtherwise::new(
      Some(TrackIndex::new(0)),
      WrittenPart::ChannelMask,
      found,
    ))
  };
  assert_eq!(mkv_judged(&written(&whole), expected), None);
  assert_eq!(
    mkv_judged(&written(&flac_private(0, MASK_ENTRY)), expected),
    named(Found::Content)
  );
  let cut = flac_private(0, &[]);
  assert_eq!(
    mkv_judged(&written(&cut), expected),
    named(Found::Length(Counts::new(
      cut.len() as u64,
      whole.len() as u64
    )))
  );
  // The Segment's size left unknown: the CodecPrivate's finding comes
  // first.
  let (sink, _) = mkv_file(&tracks_of(&[&[0x81, 0x00, 0x0C, 0x00, 1, 2]]));
  assert_eq!(
    mkv_judged(&mkv_walked(&sink, &[None]), MkvPrivate::Length(17)),
    Some(WrittenOtherwise::new(
      Some(TrackIndex::new(0)),
      WrittenPart::Configuration(0),
      Found::Length(Counts::new(6, 17))
    ))
  );
  assert_eq!(
    mkv_verdict(
      &mkv_walked(&sink, &[None, None]),
      [
        (TrackIndex::new(0), MkvPrivate::Unjudged),
        (TrackIndex::new(1), MkvPrivate::Unjudged),
      ]
      .into_iter(),
      2
    ),
    Some(WrittenOtherwise::new(
      None,
      WrittenPart::Structure,
      Found::Count(Counts::new(1, 2))
    ))
  );
}

// ---------------------------------------------------------------------------
//  Raw FLAC's walk (A23).
// ---------------------------------------------------------------------------

/// A metadata block of `kind`, `last` or not, holding `data`.
fn flac_block(kind: u8, last: bool, data: &[u8]) -> Vec<u8> {
  let mut bytes = vec![kind | if last { 0x80 } else { 0 }];
  bytes.extend_from_slice(&u32::try_from(data.len()).expect("a length").to_be_bytes()[1..]);
  bytes.extend_from_slice(data);
  bytes
}

/// A VORBIS_COMMENT's data: a 9-byte vendor and `entries`; of none, the
/// vendor alone, as an empty dictionary writes it.
fn vorbis_comment(entries: &[&[u8]]) -> Vec<u8> {
  let mut data = 9_u32.to_le_bytes().to_vec();
  data.extend_from_slice(b"Lavf62.3.");
  if !entries.is_empty() {
    data.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    for entry in entries {
      data.extend_from_slice(&(entry.len() as u32).to_le_bytes());
      data.extend_from_slice(entry);
    }
  }
  data
}

/// A raw FLAC file: `fLaC`, STREAMINFO, the VORBIS_COMMENT of `comment`,
/// PADDING last, then two frames.
fn flac_file(comment: &[u8]) -> Sink {
  let mut sink = Sink::default();
  sink.append(b"fLaC");
  sink.append(&flac_block(0, false, &[0x11; 34]));
  sink.append(&flac_block(4, false, comment));
  sink.append(&flac_block(1, true, &[0; 16]));
  sink.append(&[0xFF, 0xF8, 0x69, 0x08, 0x00, 0x00]);
  sink.append(&[0xFF, 0xF8, 0x69, 0x08, 0x00, 0x01]);
  sink
}

/// The raw FLAC walk over `sink`'s runs.
fn flac_walked(sink: &Sink) -> Audit {
  let mut audit = Audit::flac();
  for (at, bytes) in &sink.runs {
    audit.took(*at, bytes);
  }
  audit
}

/// The mask entry of a 2.1 layout, and what the muxer expects of it.
const MASK: &[u8] = b"WAVEFORMATEXTENSIBLE_CHANNEL_MASK=0xb";
fn mask_expected() -> (u64, u64) {
  (1, config_fold(CONFIG_SEED, MASK))
}

/// **A raw FLAC file whole is followed, its mask entry read** (A23): the
/// vendor, libavformat's `encoder` and the mask entry; STREAMINFO written
/// again once; twice, or a write in the comment, a rewrite of no field.
#[test]
fn a_raw_flac_file_and_its_mask_entry_are_followed() {
  let comment = vorbis_comment(&[b"encoder=Lavf62.3.100", MASK]);
  let mut sink = flac_file(&comment);
  let audit = flac_walked(&sink);
  assert_eq!(audit.unfollowed(), None);
  assert_eq!(
    flac_verdict(&audit, Some((TrackIndex::new(0), mask_expected()))),
    None
  );
  sink.patch(8, &[0x22; 20]);
  sink.patch(28, &[0x22; 14]);
  assert_eq!(flac_walked(&sink).unfollowed(), None, "STREAMINFO again");
  let mut twice = Sink {
    runs: sink.runs.clone(),
    frontier: sink.frontier,
  };
  twice.patch(8, &[0x33; 34]);
  assert_eq!(
    flac_walked(&twice).unfollowed(),
    Some(Unfollowed::new(8, Why::Rewrite))
  );
  let mut inside = flac_file(&comment);
  inside.patch(50, &[0]);
  assert_eq!(
    flac_walked(&inside).unfollowed(),
    Some(Unfollowed::new(50, Why::Rewrite))
  );
}

/// **The mask entry is judged by its absence, its count and its content**:
/// a comment of the vendor alone — A23's road, an empty dictionary —
/// omitted; two, counted; another mask, by its content.
#[test]
fn raw_flacs_mask_entry_is_judged() {
  let judged = |entries: &[&[u8]]| {
    flac_verdict(
      &flac_walked(&flac_file(&vorbis_comment(entries))),
      Some((TrackIndex::new(0), mask_expected())),
    )
  };
  let named = |found| {
    Some(WrittenOtherwise::new(
      Some(TrackIndex::new(0)),
      WrittenPart::ChannelMask,
      found,
    ))
  };
  assert_eq!(judged(&[]), named(Found::Omitted));
  assert_eq!(judged(&[b"encoder=Lavf62.3.100"]), named(Found::Omitted));
  assert_eq!(
    judged(&[MASK, MASK]),
    named(Found::Count(Counts::new(2, 1)))
  );
  assert_eq!(
    judged(&[b"WAVEFORMATEXTENSIBLE_CHANNEL_MASK=0xc"]),
    named(Found::Content)
  );
  assert_eq!(
    flac_verdict(
      &flac_walked(&flac_file(&vorbis_comment(&[]))),
      Some((TrackIndex::new(0), (0, CONFIG_SEED)))
    ),
    None,
    "none where none is expected"
  );
}

/// **A raw FLAC structure no writer makes, or a file ending inside its
/// metadata**: an entry's length past its block; the file ending inside
/// the comment, truncated at its header; a first block other than
/// STREAMINFO.
#[test]
fn a_raw_flac_structure_no_writer_makes_is_named() {
  let mut comment = vorbis_comment(&[MASK]);
  let at = comment.len() - MASK.len() - 4;
  comment[at] += 1;
  let sink = flac_file(&comment);
  assert_eq!(
    flac_walked(&sink)
      .unfollowed()
      .map(|unfollowed| unfollowed.why()),
    Some(Why::Structure)
  );
  let mut cut = Sink::default();
  cut.append(b"fLaC");
  cut.append(&flac_block(0, false, &[0x11; 34]));
  let header = cut.append(&flac_block(4, true, &vorbis_comment(&[MASK]))[..20]);
  assert_eq!(
    flac_walked(&cut).unfollowed(),
    Some(Unfollowed::new(header, Why::Truncated))
  );
  let mut first = Sink::default();
  first.append(b"fLaC");
  first.append(&flac_block(4, true, &vorbis_comment(&[MASK])));
  assert_eq!(
    flac_walked(&first).unfollowed(),
    Some(Unfollowed::new(4, Why::Structure))
  );
}

/// **A `gmhd`'s placeholder children are followed** (Codex R1 [2]): GoPro
/// metadata's `gmhd` — `gmin` and `text` of fixed sizes, then a `gpmd` of
/// a placeholder size, patched once its version is written
/// (`mov_write_gmhd_tag`, `movenc.c` 3488–3537) — its header and its child
/// flushed before their patches, each patch a write of its own closing its
/// box at the frontier, innermost first: followed; a `tmcd` child (3524–3529)
/// the same; and the in-buffer face, every size final, followed.
///
/// PLANT P635: `gmhd` kept opaque.
#[test]
fn a_gmhds_placeholder_children_are_followed() {
  let flushed = |child: &[u8; 4], payload: &[u8]| {
    let mut sink = Sink::default();
    let mdat = header(&mut sink);
    sink.close(mdat);
    let moov = sink.append(&placeholder(b"moov", &[]));
    let trak = sink.append(&placeholder(b"trak", &[]));
    let mdia = sink.append(&placeholder(b"mdia", &[]));
    sink.append(&hdlr(b"meta"));
    let minf = sink.append(&placeholder(b"minf", &[]));
    let gmhd = sink.append(&placeholder(b"gmhd", &[]));
    sink.append(&boxed(b"gmin", &[0; 16]));
    sink.append(&boxed(b"text", &[0; 36]));
    let at = sink.append(&placeholder(child, payload));
    sink.close(at);
    sink.close(gmhd);
    sink.append(&boxed(b"dinf", &boxed(b"dref", &[0; 8])));
    for open in [minf, mdia, trak, moov] {
      sink.close(open);
    }
    (sink, at)
  };
  let (gpmd, _) = flushed(b"gpmd", &[0; 4]);
  assert_eq!(unfollowed(&gpmd), None, "gpmd");
  let (tmcd, _) = flushed(b"tmcd", &boxed(b"tcmi", &[0; 12]));
  assert_eq!(unfollowed(&tmcd), None, "tmcd");
  let mut whole = Sink::default();
  let mdat = header(&mut whole);
  whole.close(mdat);
  let gmhd = boxed(
    b"gmhd",
    &[
      boxed(b"gmin", &[0; 16]),
      boxed(b"text", &[0; 36]),
      boxed(b"gpmd", &[0; 4]),
    ]
    .concat(),
  );
  let minf = boxed(b"minf", &gmhd);
  let mdia = boxed(b"mdia", &[hdlr(b"meta"), minf].concat());
  whole.append(&boxed(b"moov", &boxed(b"trak", &mdia)));
  assert_eq!(unfollowed(&whole), None, "in-buffer");
}

/// **A raw FLAC file of no audio track to judge is named, never passed**
/// (the R3 gate's condition 1): `flac_init` refuses such a context
/// (`flacenc.c` 205–244), so the road is the verdict's shape alone — a
/// structure no writer makes, at the frontier.
#[test]
fn a_raw_flac_file_of_no_audio_track_is_named() {
  let sink = flac_file(&vorbis_comment(&[MASK]));
  let audit = flac_walked(&sink);
  assert_eq!(
    flac_verdict(&audit, None),
    Some(WrittenOtherwise::new(
      None,
      WrittenPart::Structure,
      Found::Unfollowed(Unfollowed::new(sink.frontier, Why::Structure))
    ))
  );
}
