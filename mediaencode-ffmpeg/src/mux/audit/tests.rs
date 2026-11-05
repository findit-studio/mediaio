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
    mov_verdict(&audit, [expected(&[4, 5, 4])].into_iter()),
    None
  );
  assert_eq!(
    mov_verdict(&audit, [expected(&[4, 6, 4])].into_iter()),
    Some(WrittenOtherwise::new(
      Some(TrackIndex::new(0)),
      WrittenPart::SampleSizes,
      Found::Length(Counts::new(13, 14))
    ))
  );
  assert_eq!(
    mov_verdict(&audit, [expected(&[5, 4, 4])].into_iter()),
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
    mov_verdict(&audit, [expected(&[4, 4])].into_iter()),
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
    samples: SampleMirror::default(),
  };
  assert_eq!(
    mov_verdict(&audit, [expected(&[4, 4]), chapters].into_iter()),
    Some(WrittenOtherwise::new(
      None,
      WrittenPart::ChapterTrack,
      Found::Omitted
    ))
  );
  assert_eq!(
    mov_verdict(&audit, core::iter::empty()),
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
    mov_verdict(&audit, [expected(&[36, 36])].into_iter()),
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
