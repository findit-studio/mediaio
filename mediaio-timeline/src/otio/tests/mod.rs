use core::num::NonZeroI32;
use std::{format, vec::Vec};

use smol_bytes::{Utf8Bytes, Utf8BytesMut, format_utf8bytes};

use super::{json::Value, *};
use crate::{
  Clip, ClipId, Duration, Fade, FadeShape, Fades, Marker, MarkerAt, MarkerColor, MediaRef, Rate,
  TimeRange, Timebase, Timestamp, Track, TrackKind, Transition,
};

fn tb(num: i32, den: i32) -> Timebase {
  Timebase::new(num, NonZeroI32::new(den).unwrap())
}

fn edit() -> Timebase {
  tb(1, 24)
}

/// The clip `name`, its id its name too.
fn clip(name: &str, start: i64, end: i64) -> Clip {
  Clip::new(
    ClipId::new(name),
    name,
    MediaRef::new(format_utf8bytes!("file:///{name}.mov")),
    TimeRange::new(0, end - start, edit()),
    TimeRange::new(start, end, edit()),
  )
}

fn fade(frames: u64) -> Option<Fade> {
  Some(Fade::new(Duration::new(frames, edit()), FadeShape::Linear))
}

fn one_track(track: Track) -> Timeline {
  Timeline::new("t", Rate::FPS_24).with_track(track)
}

/// The schema names of the first track's children, in order, with each
/// gap's and transition's lengths: `Gap(0)`, `In(0, 6)`, `Clip`.
fn children(timeline: &Timeline) -> Vec<String> {
  let text = to_otio(timeline, OtioTarget::V0_15Plus).unwrap();
  validate_json(&text, OtioTarget::V0_15Plus).unwrap();
  let root = json::parse(&text).unwrap();
  let get = |value: &Value, key: &str| -> Value {
    value
      .as_object()
      .unwrap()
      .iter()
      .find(|(k, _)| k == key)
      .unwrap()
      .1
      .clone()
  };
  let frames = |time: &Value| get(time, "value").as_f64().unwrap();
  let tracks = get(&root, "tracks");
  let first = get(&tracks, "children").as_array().unwrap()[0].clone();
  get(&first, "children")
    .as_array()
    .unwrap()
    .iter()
    .map(|child| match get(child, "OTIO_SCHEMA").as_str().unwrap() {
      "Gap.1" => format!(
        "Gap({})",
        frames(&get(&get(child, "source_range"), "duration"))
      ),
      "Transition.1" => format!(
        "Dissolve({}, {})",
        frames(&get(child, "in_offset")),
        frames(&get(child, "out_offset"))
      ),
      _ => String::from(get(child, "name").as_str().unwrap()),
    })
    .collect()
}

#[test]
fn a_fade_out_at_a_tracks_end_dissolves_to_a_gap_of_no_length() {
  let timeline = one_track(
    Track::new(TrackKind::Video, "V")
      .with_clip(clip("a", 0, 24).with_fades(Fades::new().with_out(fade(6)))),
  );
  assert_eq!(children(&timeline), ["a", "Dissolve(6, 0)", "Gap(0)"]);
}

#[test]
fn a_fade_in_at_a_tracks_start_dissolves_from_a_gap_of_no_length() {
  let timeline = one_track(
    Track::new(TrackKind::Video, "V")
      .with_clip(clip("a", 0, 24).with_fades(Fades::new().with_in(fade(6)))),
  );
  assert_eq!(children(&timeline), ["Gap(0)", "Dissolve(0, 6)", "a"]);
}

#[test]
fn a_fade_beside_a_derived_gap_dissolves_against_that_gap() {
  let timeline = one_track(
    Track::new(TrackKind::Video, "V")
      .with_clip(clip("a", 0, 24).with_fades(Fades::new().with_out(fade(6))))
      .with_clip(clip("b", 30, 54).with_fades(Fades::new().with_in(fade(6)))),
  );
  assert_eq!(
    children(&timeline),
    ["a", "Dissolve(6, 0)", "Gap(6)", "Dissolve(0, 6)", "b"]
  );
}

#[test]
fn a_fade_out_meeting_a_fade_in_shares_one_gap_of_no_length() {
  let timeline = one_track(
    Track::new(TrackKind::Video, "V")
      .with_clip(clip("a", 0, 24).with_fades(Fades::new().with_out(fade(6))))
      .with_clip(clip("b", 24, 48).with_fades(Fades::new().with_in(fade(6)))),
  );
  assert_eq!(
    children(&timeline),
    ["a", "Dissolve(6, 0)", "Gap(0)", "Dissolve(0, 6)", "b"]
  );
}

#[test]
fn a_dissolve_sits_between_the_two_clips_it_joins() {
  // `b` plays from the start of its medium, so the dissolve's four frames
  // before the cut have no head handle until its source moves on.
  let mut a = clip("a", 0, 24);
  a.set_source_range(TimeRange::new(10, 34, edit()));
  let mut b = clip("b", 24, 48).with_enabled(false);
  b.media_mut()
    .set_available_range(Some(TimeRange::new(0, 100, edit())));
  let mut timeline = one_track(
    Track::new(TrackKind::Video, "V")
      .with_clip(a)
      .with_clip(b)
      .with_clip(clip("c", 48, 72))
      .with_transition(Transition::dissolve(
        crate::Timestamp::new(24, edit()),
        Duration::new(4, edit()),
        Duration::new(2, edit()),
      )),
  );
  assert_eq!(
    to_otio(&timeline, OtioTarget::V0_15Plus),
    Err(Refused::Validation(std::vec![Refusal::HandleMissing(
      crate::EdgeAt::new(crate::ClipAt::new(0, 1), crate::Edge::In)
    )]))
  );
  timeline.tracks_mut()[0].clips_mut()[1].set_source_range(TimeRange::new(10, 34, edit()));
  assert_eq!(children(&timeline), ["a", "Dissolve(4, 2)", "b", "c"]);
}

#[test]
fn a_cut_back_exports_each_placement_under_its_id() {
  // `a`, `b`, then `a` again from further into its medium: one medium placed
  // twice, two clips.
  let mut again = clip("a", 48, 72);
  again.set_id(ClipId::new("a, again"));
  again.set_source_range(TimeRange::new(24, 48, edit()));
  let timeline = one_track(
    Track::new(TrackKind::Video, "V")
      .with_clip(clip("a", 0, 24))
      .with_clip(clip("b", 24, 48))
      .with_clip(again),
  );
  assert_eq!(validate(&timeline), Ok(()));
  assert_eq!(children(&timeline), ["a", "b", "a"]);
  // Each clip's id rides in its metadata, so a reader tells the two apart.
  let text = to_otio(&timeline, OtioTarget::V0_15Plus).unwrap();
  let root = json::parse(&text).unwrap();
  let track = member(&member(&root, "tracks"), "children")
    .as_array()
    .unwrap()[0]
    .clone();
  let ids: Vec<_> = member(&track, "children")
    .as_array()
    .unwrap()
    .iter()
    .map(|clip| {
      let ours = member(&member(clip, "metadata"), "mediaio");
      String::from(member(&ours, "id").as_str().unwrap())
    })
    .collect();
  assert_eq!(ids, ["a", "b", "a, again"]);
}

#[test]
fn a_timeline_that_does_not_validate_is_refused_with_its_refusals() {
  let timeline = one_track(
    Track::new(TrackKind::Video, "V")
      .with_clip(clip("a", 0, 24))
      .with_clip(clip("b", 12, 36)),
  );
  assert_eq!(
    to_otio(&timeline, OtioTarget::Legacy),
    Err(Refused::Validation(std::vec![Refusal::Overlap(
      crate::ClipPair::new(0, 0, 1)
    )]))
  );
}

fn member(value: &Value, key: &str) -> Value {
  value
    .as_object()
    .unwrap()
    .iter()
    .find(|(k, _)| k == key)
    .unwrap()
    .1
    .clone()
}

/// The first clip's `source_range` as written: `(rate, start, duration)`,
/// after checking that its start and its duration share that rate.
fn source_range(timeline: &Timeline) -> (f64, f64, f64) {
  rate_start_duration(&member(&first_clip(timeline), "source_range"))
}

/// The first clip's medium's `available_range` as written, as
/// [`source_range`] reads one.
fn available_range(timeline: &Timeline) -> (f64, f64, f64) {
  let reference = member(&first_clip(timeline), "media_reference");
  rate_start_duration(&member(&reference, "available_range"))
}

/// The first track's first clip, written for `Legacy` readers.
fn first_clip(timeline: &Timeline) -> Value {
  let text = to_otio(timeline, OtioTarget::Legacy).unwrap();
  let root = json::parse(&text).unwrap();
  let track = member(&member(&root, "tracks"), "children")
    .as_array()
    .unwrap()[0]
    .clone();
  member(&track, "children").as_array().unwrap()[0].clone()
}

fn rate_start_duration(range: &Value) -> (f64, f64, f64) {
  let (start, duration) = (member(range, "start_time"), member(range, "duration"));
  let rate = member(&duration, "rate").as_f64().unwrap();
  assert_eq!(
    member(&start, "rate").as_f64(),
    Some(rate),
    "a range written in two rates"
  );
  (
    rate,
    member(&start, "value").as_f64().unwrap(),
    member(&duration, "value").as_f64().unwrap(),
  )
}

/// One clip `a` playing `source` of a medium at `rate`, at the record
/// [0, `frames`) of a timeline at `edit`.
fn one_source(edit: Rate, frames: i64, source: TimeRange, rate: Option<Rate>) -> Timeline {
  let mut a = clip("a", 0, frames);
  a.set_source_range(source);
  a.set_record(TimeRange::new(
    0,
    frames,
    edit.checked_to_timebase().unwrap(),
  ));
  a.media_mut().set_rate(rate);
  Timeline::new("t", edit).with_track(Track::new(TrackKind::Video, "V").with_clip(a))
}

#[test]
fn a_source_range_is_written_whole_in_the_mediums_ruler() {
  // A second of 48 kHz sound from sample 1000, under 24 fps: the start and
  // the length in samples, so the range OpenTimelineIO reads ends at 49 000,
  // where the stored range ends — not at a length counted in frames.
  let sound = one_source(
    Rate::FPS_24,
    24,
    TimeRange::new(1000, 49_000, tb(1, 48_000)),
    Some(Rate::hz(48_000)),
  );
  assert_eq!(source_range(&sound), (48_000.0, 1000.0, 48_000.0));
  // On frames of the medium's stated rate: both in frames.
  let movie = one_source(
    Rate::FPS_23_976,
    24,
    TimeRange::new(1001, 1001 + 24_024, tb(1, 24_000)),
    Some(Rate::FPS_23_976),
  );
  assert_eq!(source_range(&movie), (24_000.0 / 1001.0, 1.0, 24.0));
}

#[test]
fn a_source_range_off_the_mediums_frames_is_written_in_ticks_start_and_length() {
  // The start is half a frame in at 24 fps: both in ticks.
  let between = one_source(
    Rate::FPS_24,
    24,
    TimeRange::new(500, 500 + 24_000, tb(1, 24_000)),
    Some(Rate::FPS_24),
  );
  assert_eq!(source_range(&between), (24_000.0, 500.0, 24_000.0));
  // The start lands on a frame of the medium's 25 fps, the length — three
  // frames at 24, 3.125 at 25 — does not: both in ticks, never one of each.
  let start_only = one_source(
    Rate::FPS_24,
    3,
    TimeRange::new(3600, 3600 + 11_250, tb(1, 90_000)),
    Some(Rate::FPS_25),
  );
  assert_eq!(source_range(&start_only), (90_000.0, 3600.0, 11_250.0));
  // No stated rate: ticks.
  let unstated = one_source(
    Rate::FPS_24,
    24,
    TimeRange::new(1000, 1000 + 24_000, tb(1, 24_000)),
    None,
  );
  assert_eq!(source_range(&unstated), (24_000.0, 1000.0, 24_000.0));
}

#[test]
fn a_source_off_the_edit_rate_is_not_exported() {
  // [0, 1) at 48 per second is half a frame at 24 fps: a record of one frame
  // would select twice the stored media, so the timeline is refused.
  let half = one_source(Rate::FPS_24, 1, TimeRange::new(0, 1, tb(1, 48)), None);
  assert_eq!(
    to_otio(&half, OtioTarget::V0_15Plus),
    Err(Refused::Validation(std::vec![Refusal::SourceOffEditRate(
      crate::ClipAt::new(0, 0)
    )]))
  );
}

const TWO_53: i64 = 1 << 53;

/// Where `to_otio` refuses `timeline` as not representable, and the count.
fn not_representable(timeline: &Timeline) -> (Spot, i128) {
  match to_otio(timeline, OtioTarget::V0_15Plus) {
    Err(Refused::NotRepresentable(count)) => (count.at(), count.value()),
    other => panic!("{other:?}"),
  }
}

/// One clip `a` at the record [`start`, `end`) of a timeline at one frame a
/// second, playing as long a stretch of a medium counted in whole seconds.
fn at_one_fps(start: i64, end: i64) -> Timeline {
  let second = tb(1, 1);
  one_track_at(
    Rate::hz(1),
    Clip::new(
      ClipId::new("a"),
      "a",
      MediaRef::new("file:///a.mov"),
      TimeRange::new(0, end - start, second),
      TimeRange::new(start, end, second),
    ),
  )
}

fn one_track_at(rate: Rate, clip: Clip) -> Timeline {
  Timeline::new("t", rate).with_track(Track::new(TrackKind::Video, "V").with_clip(clip))
}

#[test]
fn a_count_past_2_53_is_refused_rather_than_read_rounded() {
  // An f64 holds every whole number up to 2^53 and only some past it:
  // OpenTimelineIO reads 2^53 + 1 as 2^53.
  assert_eq!(
    "9007199254740993.0".parse::<f64>(),
    Ok(9_007_199_254_740_992.0)
  );
  let a = Spot::Record(ClipAt::new(0, 0));
  let past = i128::from(TWO_53) + 1;
  // Codex's case: a clip at [2^53 + 1, 2^53 + 2) on a timeline at one frame
  // a second. Its leading gap of 2^53 + 1 frames would read as 2^53, placing
  // the clip a second early.
  assert_eq!(
    not_representable(&at_one_fps(TWO_53 + 1, TWO_53 + 2)),
    (a, past)
  );
  // A gap of 2^53 and a clip of one: each count is held, but the clip ends
  // at their sum, 2^53 + 1, where OpenTimelineIO adds them up — the place
  // of the clip, the track's second child.
  assert_eq!(
    not_representable(&at_one_fps(TWO_53, TWO_53 + 1)),
    (Spot::TrackPosition(ChildAt::new(0, 1)), past)
  );
  // The start, at the edit rate.
  let early = at_one_fps(0, 1).with_start(Timestamp::new(-TWO_53 - 1, tb(1, 1)));
  assert_eq!(not_representable(&early), (Spot::Start, -past));
  // Within 2^53 a count OpenTimelineIO 0.17's reader reads back exactly is
  // written, and reads back whole: 2^53 - 2. (2^53 - 1 it misreads, below.)
  assert_eq!(
    children(&at_one_fps(TWO_53 - 2, TWO_53 - 1)),
    ["Gap(9007199254740990)", "a"]
  );
}

/// Where `to_otio` refuses `timeline` because OpenTimelineIO 0.17's reader
/// reads a number it would write as another value, and the refusal.
fn misread(timeline: &Timeline) -> (NotRepresentable, Misread) {
  match to_otio(timeline, OtioTarget::V0_15Plus) {
    Err(Refused::NotRepresentable(refusal)) => match refusal.misread() {
      Some(misread) => (refusal, misread),
      None => panic!("{refusal}"),
    },
    other => panic!("{other:?}"),
  }
}

#[test]
fn a_count_opentimelineio_0_17_reads_as_another_is_refused_by_name() {
  // OpenTimelineIO 0.17's reader takes `<digits>.0`'s zero into the
  // significand, which converts to a double rounded past 2^53 before it is
  // divided by ten: every count up to 1 801 439 850 948 198, ⌊2^54 / 10⌋,
  // reads back exactly — the last the export writes before the first it
  // refuses.
  const LAST: i64 = 1_801_439_850_948_198;
  assert_eq!(
    children(&at_one_fps(LAST, LAST + 1)),
    ["Gap(1801439850948198)", "a"]
  );
  // The next it reads as 1 801 439 850 948 199.25: refused by name, with no
  // search, the place and both values named.
  let (refusal, read) = misread(&at_one_fps(LAST + 1, LAST + 2));
  assert_eq!(
    (refusal.at(), refusal.value(), refusal.searched()),
    (Spot::Record(ClipAt::new(0, 0)), i128::from(LAST + 1), None)
  );
  assert_eq!(read.number(), MisreadNumber::Count);
  assert_eq!(read.meant(), (LAST + 1) as f64);
  // A quarter more: the doubles there are a quarter apart.
  assert_eq!(read.read(), [(LAST + 1) as f64 + 0.25; 2]);
  assert_eq!(
    std::string::ToString::to_string(&refusal),
    "the gap before track 0, clip 0 counts 1801439850948199 ticks of 1/1 s, written \
     1801439850948199.0: OpenTimelineIO 0.17 reads this spelling as another value, \
     1801439850948199.3"
  );
  // 2^53 - 1 it reads as 2^53 - 2, a whole number: a reader would place the
  // clip a second early, and say nothing.
  let (refusal, read) = misread(&at_one_fps(TWO_53 - 1, TWO_53));
  assert_eq!(refusal.value(), i128::from(TWO_53 - 1));
  assert_eq!(read.read(), [(TWO_53 - 2) as f64; 2]);
  // At the start, at the edit rate, as much.
  let start = at_one_fps(0, 1).with_start(Timestamp::new(TWO_53 - 1, tb(1, 1)));
  assert_eq!(misread(&start).0.at(), Spot::Start);
}

#[test]
fn a_rate_opentimelineio_0_17_reads_as_another_is_refused_by_name() {
  let at = |num: i32| {
    let rate = Rate::fps(num, core::num::NonZeroI32::new(1001).unwrap());
    let frame = rate.checked_to_timebase().unwrap();
    one_track_at(
      rate,
      Clip::new(
        ClipId::new("a"),
        "a",
        MediaRef::new("file:///a.mov"),
        TimeRange::new(0, 24, frame),
        TimeRange::new(0, 24, frame),
      ),
    )
  };
  // 96000/1001's f64 is written 95.90409590409591, which OpenTimelineIO
  // 0.17's reader reads as the next f64 up: refused where it is first
  // written, at the start.
  let (refusal, read) = misread(&at(96_000));
  assert_eq!((refusal.at(), refusal.value()), (Spot::Start, 0));
  assert_eq!(read.number(), MisreadNumber::Rate);
  assert_eq!(read.meant(), 96_000.0 / 1001.0);
  assert_eq!(
    read.read().map(f64::to_bits),
    [0x4057_F9DC_B511_2288; 2],
    "the next f64 up from {:#x}",
    (96_000.0_f64 / 1001.0).to_bits()
  );
  assert_eq!(
    std::string::ToString::to_string(&refusal),
    "the start counts 0 ticks of 1001/96000 s, its rate written 95.90409590409591: \
     OpenTimelineIO 0.17 reads this spelling as another value, 95.90409590409593"
  );
  // 24000/1001's, written 23.976023976023978, reads back exactly: written,
  // and read back as the very timeline.
  let ntsc = at(24_000);
  let text = to_otio(&ntsc, OtioTarget::V0_15Plus).unwrap();
  assert_eq!(from_otio(&text), Ok(ntsc));
}

/// SplitMix64: a fixed seed gives a fixed sequence, on every platform.
struct Seeded(u64);

impl Seeded {
  fn next(&mut self) -> u64 {
    self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = self.0;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
  }

  /// A number in `low..=high`.
  fn within(&mut self, low: u64, high: u64) -> u64 {
    low + self.next() % (high - low + 1)
  }
}

#[test]
fn every_number_an_accepted_export_writes_reads_back_exactly_in_either_build() {
  // Rates common and exotic, and gaps from a few frames to 2^53, from fixed
  // seeds. Whatever `to_otio` writes, OpenTimelineIO 0.17's reader reads
  // back as the value meant — each count as its whole number, each rate as
  // the f64 its shortest spelling names — in a build that rounds RapidJSON's
  // multiply-adds twice and in one that fuses them; whatever it refuses as
  // misread, either build does read otherwise.
  const COMMON: [(i32, i32); 12] = [
    (24, 1),
    (25, 1),
    (30, 1),
    (48, 1),
    (50, 1),
    (60, 1),
    (24_000, 1001),
    (30_000, 1001),
    (48_000, 1001),
    (60_000, 1001),
    (96_000, 1001),
    (25, 2),
  ];
  let reads_back = |text: &str, meant: f64| {
    json::readings(text)
      .iter()
      .all(|reading| reading.is_ok_and(|number| number.double().to_bits() == meant.to_bits()))
  };
  // Fewer under Miri, which interprets slowly: the first eight seeds, which
  // between them draw every kind of rate and of gap, written, misread and
  // refused.
  let (seeds, many) = if cfg!(miri) {
    (8, (0, 0))
  } else {
    (256, (64, 16))
  };
  let (mut written, mut misread) = (0, 0);
  for seed in 0..seeds {
    let mut draw = Seeded(seed);
    let (num, den) = match draw.within(0, 2) {
      0 => COMMON[draw.within(0, COMMON.len() as u64 - 1) as usize],
      1 => (draw.within(1, 240_000) as i32, 1001),
      _ => (draw.within(1, 240_000) as i32, draw.within(1, 1001) as i32),
    };
    let rate = Rate::fps(num, core::num::NonZeroI32::new(den).unwrap());
    let Some(frame) = rate.checked_to_timebase() else {
      continue;
    };
    let gap = match draw.within(0, 3) {
      0 => draw.within(0, 1000),
      1 => 1 << draw.within(0, 53),
      2 => draw.within(1_801_439_850_948_190, 1_801_439_850_948_210),
      _ => draw.within((1 << 53) - 30, 1 << 53),
    } as i64;
    let length = draw.within(1, 48) as i64;
    let timeline = one_track_at(
      rate,
      Clip::new(
        ClipId::new("a"),
        "a",
        MediaRef::new("file:///a.mov"),
        TimeRange::new(0, length, frame),
        TimeRange::new(gap, gap + length, frame),
      ),
    );
    match to_otio(&timeline, OtioTarget::V0_15Plus) {
      Ok(text) => {
        written += 1;
        let root = json::parse(&text).unwrap();
        let mut times = Vec::new();
        named(&root, "RationalTime.1", &mut times);
        for time in times {
          let value = spelled(time, "value");
          let count: i128 = value.strip_suffix(".0").unwrap().parse().unwrap();
          assert!(reads_back(value, count as f64), "seed {seed}: {value}");
          let rate = spelled(time, "rate");
          assert!(
            reads_back(rate, rate.parse().unwrap()),
            "seed {seed}: {rate}"
          );
        }
      }
      Err(Refused::NotRepresentable(refusal)) => {
        if let Some(read) = refusal.misread() {
          misread += 1;
          let meant = read.meant().to_bits();
          assert!(
            read.read().iter().any(|read| read.to_bits() != meant),
            "seed {seed}: {refusal}"
          );
        }
      }
      Err(other) => panic!("seed {seed}: {other}"),
    }
  }
  // Both sides drawn, many times; under Miri, at least once.
  assert!(
    written > many.0 && misread > many.1,
    "{written} written, {misread} misread"
  );
}

#[test]
fn a_count_the_export_writes_is_refused_before_any_search() {
  // The gap before `a`, 2^53 + 1 frames at the edit rate: no ruler the
  // search tries changes a record-side count, so none is walked, and the
  // refusal names no search.
  let refusal = match to_otio(&at_one_fps(TWO_53 + 1, TWO_53 + 2), OtioTarget::V0_15Plus) {
    Err(Refused::NotRepresentable(refusal)) => refusal,
    other => panic!("{other:?}"),
  };
  assert_eq!(refusal.searched(), None);
  assert!(
    std::string::ToString::to_string(&refusal).ends_with("does not hold exactly"),
    "{refusal}"
  );
}

#[test]
fn a_media_range_whose_end_is_past_2_53_is_refused() {
  // The source range [2^53 - 1, 2^53 + 1): its start and its length are
  // held, the end OpenTimelineIO adds up from them is not, and in whole
  // seconds no coarser whole ruler holds it. The end is derived: the walk
  // refuses it, after a search with no other ruler to try.
  let second = tb(1, 1);
  let searched = |timeline: &Timeline| match to_otio(timeline, OtioTarget::V0_15Plus) {
    Err(Refused::NotRepresentable(count)) => count.searched().map(|search| search.walks()),
    other => panic!("{other:?}"),
  };
  let mut timeline = at_one_fps(0, 2);
  timeline.tracks_mut()[0].clips_mut()[0].set_source_range(TimeRange::new(
    TWO_53 - 1,
    TWO_53 + 1,
    second,
  ));
  assert_eq!(
    not_representable(&timeline),
    (Spot::Source(ClipAt::new(0, 0)), i128::from(TWO_53) + 1)
  );
  assert_eq!(searched(&timeline), Some(1));
  // An available range 2^53 + 1 seconds long: none of its own rulers holds
  // it whole, nor does seconds, the one ruler the timeline's operands are
  // counted in, and every whole rate that lands on it is a multiple of a
  // second. Refused before any walk, the search's bands tried and no plan
  // walked: the search never moves an available range.
  let mut timeline = at_one_fps(0, 2);
  timeline.tracks_mut()[0].clips_mut()[0]
    .media_mut()
    .set_available_range(Some(TimeRange::new(0, TWO_53 + 1, second)));
  assert_eq!(
    not_representable(&timeline),
    (Spot::Available(ClipAt::new(0, 0)), i128::from(TWO_53) + 1)
  );
  assert_eq!(searched(&timeline), Some(0));
  // The available range [2^53 - 3, 2^53 + 1), around a source range that
  // ends within 2^53: its start and its length are held, its end is not —
  // held with them as it is planned, in seconds as in every ruler of the
  // bands, so refused before any walk too.
  let mut timeline = at_one_fps(0, 2);
  let clip = &mut timeline.tracks_mut()[0].clips_mut()[0];
  clip.set_source_range(TimeRange::new(TWO_53 - 3, TWO_53 - 1, second));
  clip
    .media_mut()
    .set_available_range(Some(TimeRange::new(TWO_53 - 3, TWO_53 + 1, second)));
  assert_eq!(
    not_representable(&timeline),
    (Spot::Available(ClipAt::new(0, 0)), i128::from(TWO_53) + 1)
  );
  assert_eq!(searched(&timeline), Some(0));
}

#[test]
fn a_media_range_its_ticks_cannot_hold_is_written_in_the_coarsest_whole_ruler_that_can() {
  // A medium stamped in nanoseconds since 1970: one second of it from
  // 1 700 000 000 s starts at tick 1.7e18, past 2^53, and has no stated rate
  // to count frames in. Whole seconds hold it.
  let nanos = Timebase::NANOS;
  let second = 1_000_000_000;
  let epoch = 1_700_000_000 * second;
  let mut timeline = one_source(
    Rate::FPS_25,
    25,
    TimeRange::new(epoch, epoch + second, nanos),
    None,
  );
  timeline.tracks_mut()[0].clips_mut()[0]
    .media_mut()
    .set_available_range(Some(TimeRange::new(epoch, epoch + 10 * second, nanos)));
  assert_eq!(source_range(&timeline), (1.0, 1_700_000_000.0, 1.0));
  assert_eq!(available_range(&timeline), (1.0, 1_700_000_000.0, 10.0));
  // Starting between seconds, on a millisecond: milliseconds.
  let off = epoch + 123_000_000;
  let mut timeline = one_source(
    Rate::FPS_25,
    25,
    TimeRange::new(off, off + second, nanos),
    None,
  );
  assert_eq!(
    source_range(&timeline),
    (1000.0, 1_700_000_000_123.0, 1000.0)
  );
  // Read back, the counts land on the stored ends, exactly.
  let stored = timeline.tracks()[0].clips()[0].source_range();
  let (rate, start, duration) = source_range(&timeline);
  let nanoseconds = |count: f64| count as i128 * i128::from(second) / rate as i128;
  assert_eq!(nanoseconds(start), i128::from(stored.start_pts()));
  assert_eq!(nanoseconds(start + duration), i128::from(stored.end_pts()));
  // A stated rate whose frames it does not land on changes nothing.
  timeline.tracks_mut()[0].clips_mut()[0]
    .media_mut()
    .set_rate(Some(Rate::FPS_29_97));
  assert_eq!(
    source_range(&timeline),
    (1000.0, 1_700_000_000_123.0, 1000.0)
  );
}

#[test]
fn a_media_range_no_ruler_holds_is_refused() {
  // One nanosecond off a whole microsecond: no whole ruler coarser than its
  // own holds the start, and its own counts it past 2^53.
  let start = 1_700_000_000_123_456_789;
  let timeline = one_source(
    Rate::FPS_25,
    25,
    TimeRange::new(start, start + 1_000_000_000, Timebase::NANOS),
    None,
  );
  assert_eq!(
    not_representable(&timeline),
    (Spot::Source(ClipAt::new(0, 0)), i128::from(start))
  );
  // Nor does a ruler the timeline's operands are counted in — 25 fps or
  // seconds: no ruler of the search's bands writes it, so there is no plan
  // to walk, and the refusal says so.
  let Err(Refused::NotRepresentable(refusal)) = to_otio(&timeline, OtioTarget::V0_15Plus) else {
    panic!("exported");
  };
  assert_eq!(refusal.searched().map(|search| search.walks()), Some(0));
}

/// Every object inside `value` naming `schema`.
fn named<'a>(value: &'a Value, schema: &str, out: &mut Vec<&'a [(Utf8Bytes, Value)]>) {
  match value {
    Value::Object(members) => {
      if members
        .iter()
        .any(|(key, name)| key == "OTIO_SCHEMA" && name.as_str() == Some(schema))
      {
        out.push(members);
      }
      for (_, member) in members {
        named(member, schema, out);
      }
    }
    Value::Array(items) => items.iter().for_each(|item| named(item, schema, out)),
    _ => {}
  }
}

/// The text a JSON number was written as.
fn spelled<'a>(members: &'a [(Utf8Bytes, Value)], key: &str) -> &'a str {
  match members.iter().find(|(k, _)| k == key) {
    Some((_, Value::Number(text))) => text,
    other => panic!("{key}: {other:?}"),
  }
}

#[test]
fn every_count_in_the_goldens_is_one_an_f64_holds_exactly() {
  for golden in [
    include_str!("../../../tests/golden/law.v0_15.otio"),
    include_str!("../../../tests/golden/law.legacy.otio"),
  ] {
    let root = json::parse(golden).unwrap();
    let mut times = Vec::new();
    named(&root, "RationalTime.1", &mut times);
    // The start; per clip its source and available ranges; per gap its range;
    // per transition its two offsets; per marker its range.
    assert_eq!(times.len(), 1 + 3 * 4 + 2 * 2 + 3 * 2 + 2 * 2);
    for time in &times {
      // A whole count within ±2^53, written as `<digits>.0`, which an f64
      // reads back as exactly that count — and OpenTimelineIO 0.17's reader
      // too, in either build.
      let value = spelled(time, "value");
      let digits = value.strip_suffix(".0").unwrap();
      let count: i128 = digits.parse().unwrap();
      assert!(count.unsigned_abs() <= 1 << 53, "{value}");
      assert_eq!(value.parse::<f64>().unwrap() as i128, count, "{value}");
      let both = |text: &str, meant: f64| {
        json::readings(text).map(|reading| reading.map(|number| number.double().to_bits()))
          == [Ok(meant.to_bits()); 2]
      };
      assert!(both(value, count as f64), "{value}");
      // The rate spelled as the shortest text of one f64, which reads back as
      // that very f64, in either build of OpenTimelineIO 0.17's reader too.
      let rate = spelled(time, "rate");
      let read = rate.parse::<f64>().unwrap();
      assert!(read.is_finite() && read > 0.0, "{rate}");
      assert_eq!(format!("{read:?}"), rate);
      assert!(both(rate, read), "{rate}");
    }
    let mut ranges = Vec::new();
    named(&root, "TimeRange.1", &mut ranges);
    assert_eq!(ranges.len(), 3 * 2 + 2 + 2);
    for range in ranges {
      let time = |key: &str| match range.iter().find(|(k, _)| k == key) {
        Some((_, Value::Object(members))) => members.as_slice(),
        other => panic!("{key}: {other:?}"),
      };
      let (start, duration) = (time("start_time"), time("duration"));
      // One ruler, so the end is the two counts added, within 2^53 too.
      assert_eq!(spelled(start, "rate"), spelled(duration, "rate"));
      let count = |members| -> i128 {
        spelled(members, "value")
          .strip_suffix(".0")
          .unwrap()
          .parse()
          .unwrap()
      };
      assert!((count(start) + count(duration)).unsigned_abs() <= 1 << 53);
    }
  }
}

#[test]
fn a_refusal_to_export_says_why() {
  let shown = |refused: Refused| std::string::ToString::to_string(&refused);
  // A count the export would write: refused before any walk, no search
  // made for it.
  assert_eq!(
    shown(Refused::NotRepresentable(NotRepresentable {
      at: Spot::Record(ClipAt::new(0, 0)),
      value: 9_007_199_254_740_993,
      rate: Rate::hz(1),
      searched: None,
      misread: None,
    })),
    "the gap before track 0, clip 0 counts 9007199254740993 ticks of 1/1 s, which \
     OpenTimelineIO's f64 does not hold exactly"
  );
  // A count OpenTimelineIO derives: refused after the search, which says it
  // was bounded and how.
  assert_eq!(
    shown(Refused::NotRepresentable(NotRepresentable {
      at: Spot::TrackPosition(ChildAt::new(0, 1)),
      value: 27_021_597_764_222_973,
      rate: Rate::hz(3),
      searched: Some(RulerSearch { walks: 3 }),
      misread: None,
    })),
    "the place of track 0, child 1 counts 27021597764222973 ticks of 1/3 s, which \
     OpenTimelineIO's f64 does not hold exactly, in the last of 3 plans the bounded search \
     walked, trying each clip in its plan's ruler, every ruler the timeline's operands are \
     counted in that holds it, the 64 finest whole rates below its plan's that hold it, and the \
     64 coarsest whole rates below its plan's that hold it"
  );
  assert_eq!(
    std::string::ToString::to_string(&RulerSearch { walks: 1 }),
    "the one plan the bounded search walked, trying each clip in its plan's ruler, every ruler \
     the timeline's operands are counted in that holds it, the 64 finest whole rates below its \
     plan's that hold it, and the 64 coarsest whole rates below its plan's that hold it"
  );
  // A source range no ruler of the search's bands writes, or an available
  // range none of them holds whole: refused as the timeline holds it, the
  // bands tried and no plan walked.
  assert_eq!(
    shown(Refused::NotRepresentable(NotRepresentable {
      at: Spot::Source(ClipAt::new(0, 0)),
      value: 9_007_199_254_740_994,
      rate: Rate::hz(1),
      searched: Some(RulerSearch { walks: 0 }),
      misread: None,
    })),
    "the source range of track 0, clip 0 counts 9007199254740994 ticks of 1/1 s, which \
     OpenTimelineIO's f64 does not hold exactly, in any of the range's own rulers or of the \
     bounded search's bands, so the search walked no plan"
  );
  assert_eq!(
    shown(Refused::NotRepresentable(NotRepresentable {
      at: Spot::Available(ClipAt::new(0, 0)),
      value: 9_007_199_254_740_993,
      rate: Rate::hz(1),
      searched: Some(RulerSearch { walks: 0 }),
      misread: None,
    })),
    "the available range of track 0, clip 0 counts 9007199254740993 ticks of 1/1 s, which \
     OpenTimelineIO's f64 does not hold exactly, in any of the range's own rulers or of the \
     bounded search's bands, so the search walked no plan"
  );
  assert_eq!(
    shown(Refused::Validation(std::vec![
      Refusal::RateUnstated,
      Refusal::EmptyRecord(ClipAt::new(0, 1)),
    ])),
    "the timeline does not validate: the edit rate is zero; track 0, clip 1: the record covers \
     no time"
  );
  let spot = |at: Spot| std::string::ToString::to_string(&at);
  assert_eq!(spot(Spot::Start), "the start");
  assert_eq!(
    spot(Spot::Available(ClipAt::new(1, 2))),
    "the available range of track 1, clip 2"
  );
  assert_eq!(
    spot(Spot::Fade(EdgeAt::new(ClipAt::new(0, 3), crate::Edge::Out))),
    "the fade at the end of track 0, clip 3"
  );
  assert_eq!(
    spot(Spot::Transition(TransitionAt::new(0, 1))),
    "track 0, transition 1"
  );
  assert_eq!(
    spot(Spot::TimelineEnd),
    "the end of the timeline, which has no track, from the global start"
  );
}

#[test]
fn names_with_quotes_and_control_characters_are_escaped() {
  let timeline = one_track(Track::new(TrackKind::Video, "V \"1\"\n").with_clip(clip("a\\b", 0, 1)));
  let text = to_otio(&timeline, OtioTarget::V0_15Plus).unwrap();
  assert!(text.contains(r#""name": "V \"1\"\n""#), "{text}");
  assert!(text.contains(r#""name": "a\\b""#), "{text}");
  assert_eq!(validate_json(&text, OtioTarget::V0_15Plus), Ok(()));
}

#[test]
fn an_empty_timeline_exports_an_empty_stack() {
  let text = to_otio(&Timeline::new("empty", Rate::FPS_25), OtioTarget::V0_15Plus).unwrap();
  assert!(text.contains(r#""children": []"#), "{text}");
  assert_eq!(validate_json(&text, OtioTarget::V0_15Plus), Ok(()));
  assert!(text.ends_with("}\n"));
}

fn shape_refusal(text: &str, target: OtioTarget) -> (String, &'static str) {
  match validate_json(text, target) {
    Err(Invalid::Shape(shape)) => (String::from(shape.path()), shape.expected()),
    other => panic!("{other:?}"),
  }
}

/// A one-clip document, valid for both targets once `clip` is filled in.
fn document(clip: &str) -> String {
  format!(
    r#"{{"OTIO_SCHEMA": "Timeline.1", "tracks": {{"OTIO_SCHEMA": "Stack.1", "children": [
      {{"OTIO_SCHEMA": "Track.1", "kind": "Video", "children": [{clip}]}}]}}}}"#
  )
}

const RT: &str = r#"{"OTIO_SCHEMA": "RationalTime.1", "rate": 24.0, "value": 1.0}"#;

fn gap() -> String {
  format!(
    r#"{{"OTIO_SCHEMA": "Gap.1", "source_range": {{"OTIO_SCHEMA": "TimeRange.1", "duration": {RT}, "start_time": {RT}}}}}"#
  )
}

fn dissolve() -> String {
  format!(
    r#"{{"OTIO_SCHEMA": "Transition.1", "in_offset": {RT}, "out_offset": {RT}, "transition_type": "SMPTE_Dissolve"}}"#
  )
}

#[test]
fn the_self_check_accepts_the_minimal_shapes() {
  assert_eq!(validate_json(&document(&gap()), OtioTarget::Legacy), Ok(()));
  let clip1 = r#"{"OTIO_SCHEMA": "Clip.1", "media_reference": {"OTIO_SCHEMA": "ExternalReference.1", "target_url": "a.mov"}}"#;
  assert_eq!(validate_json(&document(clip1), OtioTarget::Legacy), Ok(()));
  assert_eq!(
    validate_json(&document(clip1), OtioTarget::V0_15Plus),
    Ok(())
  );
  let between = format!("{}, {}, {}", gap(), dissolve(), gap());
  assert_eq!(
    validate_json(&document(&between), OtioTarget::Legacy),
    Ok(())
  );
}

#[test]
fn the_self_check_refuses_by_path() {
  let target = OtioTarget::V0_15Plus;
  assert_eq!(
    shape_refusal(r#"{"OTIO_SCHEMA": "Stack.1"}"#, target),
    (String::from("$.OTIO_SCHEMA"), "the schema Timeline.1")
  );
  assert_eq!(
    shape_refusal(r#"{"OTIO_SCHEMA": "Timeline.1"}"#, target),
    (String::from("$.tracks"), "a required key")
  );
  assert_eq!(
    shape_refusal(
      &document(r#"{"OTIO_SCHEMA": "Gap.1", "enabled": "yes"}"#),
      target
    ),
    (
      String::from("$.tracks.children[0].children[0].enabled"),
      "a boolean"
    )
  );
  let rate_zero = gap().replace("\"rate\": 24.0", "\"rate\": 0.0");
  assert_eq!(
    shape_refusal(&document(&rate_zero), target),
    (
      String::from("$.tracks.children[0].children[0].source_range.duration.rate"),
      "a positive, finite rate"
    )
  );
  let negative = gap().replacen("\"value\": 1.0", "\"value\": -1.0", 1);
  assert_eq!(
    shape_refusal(&document(&negative), target),
    (
      String::from("$.tracks.children[0].children[0].source_range.duration.value"),
      "a length of zero or more"
    )
  );
  assert_eq!(
    shape_refusal(&document(r#"{"OTIO_SCHEMA": "Clip.3"}"#), target),
    (
      String::from("$.tracks.children[0].children[0].OTIO_SCHEMA"),
      "a schema a track holds"
    )
  );
}

#[test]
fn a_track_without_kind_is_refused_for_both_targets() {
  // Every reader takes `kind` with `reader.read`, which fails on a key left
  // out (track.cpp).
  let kindless = document(&gap()).replace(r#""kind": "Video", "#, "");
  assert!(!kindless.contains("kind"));
  for target in [OtioTarget::V0_15Plus, OtioTarget::Legacy] {
    assert_eq!(
      shape_refusal(&kindless, target),
      (String::from("$.tracks.children[0].kind"), "a required key")
    );
  }
}

#[test]
fn a_clip_1_without_its_media_reference_is_refused_for_legacy_readers_only() {
  // Readers before 0.15 read `media_reference` with `reader.read`; later
  // ones upgrade a `Clip.1` and take a reference left out as missing.
  let bare = document(r#"{"OTIO_SCHEMA": "Clip.1"}"#);
  assert_eq!(
    shape_refusal(&bare, OtioTarget::Legacy),
    (
      String::from("$.tracks.children[0].children[0].media_reference"),
      "a required key"
    )
  );
  assert_eq!(validate_json(&bare, OtioTarget::V0_15Plus), Ok(()));
  // A null reference is read, as an empty one.
  let null = document(r#"{"OTIO_SCHEMA": "Clip.1", "media_reference": null}"#);
  assert_eq!(validate_json(&null, OtioTarget::Legacy), Ok(()));
}

/// The keys each schema's reader requires, per target: the audit in
/// `check`'s docs, transcribed.
fn required_by_readers(schema: &str, target: OtioTarget) -> &'static [&'static str] {
  match (schema, target) {
    ("Timeline.1", _) => &["tracks"],
    ("Stack.1", _) => &["children"],
    ("Track.1", _) => &["kind", "children"],
    ("Clip.2", _) => &["media_references", "active_media_reference_key"],
    ("Clip.1", OtioTarget::Legacy) => &["media_reference"],
    ("Clip.1" | "Gap.1", _) => &[],
    ("Transition.1", _) => &["in_offset", "out_offset", "transition_type"],
    ("ExternalReference.1", _) => &["target_url"],
    ("Marker.2", _) => &["marked_range"],
    ("RationalTime.1", _) => &["rate", "value"],
    ("TimeRange.1", _) => &["start_time", "duration"],
    _ => panic!("no audit for {schema}"),
  }
}

/// One step from a value to one inside it.
#[derive(Clone)]
enum Step {
  Key(Utf8Bytes),
  At(usize),
}

/// Every object naming a schema inside `value`: its path as the check
/// writes one, the steps to it, its schema, and its keys.
fn schema_objects(
  value: &Value,
  path: &str,
  steps: &[Step],
  out: &mut Vec<(Utf8Bytes, Vec<Step>, Utf8Bytes, Vec<Utf8Bytes>)>,
) {
  match value {
    Value::Object(members) => {
      if let Some(schema) = members
        .iter()
        .find(|(key, _)| key == "OTIO_SCHEMA")
        .and_then(|(_, schema)| schema.as_str())
      {
        let keys = members
          .iter()
          .map(|(key, _)| key.clone())
          .filter(|key| key != "OTIO_SCHEMA")
          .collect();
        out.push((
          Utf8Bytes::from(path),
          steps.to_vec(),
          Utf8Bytes::from(schema),
          keys,
        ));
      }
      for (key, member) in members {
        let mut deeper = steps.to_vec();
        deeper.push(Step::Key(key.clone()));
        schema_objects(member, &format!("{path}.{key}"), &deeper, out);
      }
    }
    Value::Array(items) => {
      for (index, item) in items.iter().enumerate() {
        let mut deeper = steps.to_vec();
        deeper.push(Step::At(index));
        schema_objects(item, &format!("{path}[{index}]"), &deeper, out);
      }
    }
    _ => {}
  }
}

/// `root` with `key` taken out of the object `steps` lead to, written out.
fn without(root: &Value, steps: &[Step], key: &str) -> Utf8Bytes {
  let mut root = root.clone();
  let mut at = &mut root;
  for step in steps {
    at = match (step, at) {
      (Step::Key(name), Value::Object(members)) => {
        &mut members
          .iter_mut()
          .find(|(member, _)| member == name)
          .unwrap()
          .1
      }
      (Step::At(index), Value::Array(items)) => &mut items[*index],
      _ => unreachable!(),
    };
  }
  let Value::Object(members) = at else {
    unreachable!()
  };
  members.retain(|(member, _)| member != key);
  let mut text = Utf8BytesMut::new();
  json::write_pretty(&root, &mut text);
  text.freeze_shared()
}

#[test]
fn the_self_check_requires_exactly_what_the_targets_readers_require() {
  // Each key of each object of each golden taken out in turn: refused where
  // the target's readers require it, read where they do not. Fewer under
  // Miri, which interprets slowly.
  let every = if cfg!(miri) { 23 } else { 1 };
  for (target, golden) in [
    (
      OtioTarget::V0_15Plus,
      include_str!("../../../tests/golden/law.v0_15.otio"),
    ),
    (
      OtioTarget::Legacy,
      include_str!("../../../tests/golden/law.legacy.otio"),
    ),
  ] {
    let root = json::parse(golden).unwrap();
    let mut objects = Vec::new();
    schema_objects(&root, "$", &[], &mut objects);
    let cases = objects.iter().flat_map(|(path, steps, schema, keys)| {
      keys.iter().map(move |key| (path, steps, schema, key))
    });
    let mut checked = 0;
    for (path, steps, schema, key) in cases.step_by(every) {
      let verdict = validate_json(&without(&root, steps, key), target);
      if required_by_readers(schema, target).contains(&key.as_str()) {
        assert_eq!(
          verdict,
          Err(Invalid::Shape(Shape {
            path: format_utf8bytes!("{path}.{key}"),
            expected: "a required key",
          })),
          "{target:?}: {schema} at {path} without {key}"
        );
      } else {
        assert_eq!(
          verdict,
          Ok(()),
          "{target:?}: {schema} at {path} without {key}"
        );
      }
      checked += 1;
    }
    assert!(checked > 0);
  }
}

#[test]
fn the_self_check_holds_transitions_between_two_items() {
  let target = OtioTarget::Legacy;
  for (children, at) in [
    (format!("{}, {}", dissolve(), gap()), 0),
    (format!("{}, {}", gap(), dissolve()), 1),
    (
      format!("{}, {}, {}, {}", gap(), dissolve(), dissolve(), gap()),
      1,
    ),
  ] {
    assert_eq!(
      shape_refusal(&document(&children), target),
      (
        format!("$.tracks.children[0].children[{at}]"),
        "a transition between two items"
      )
    );
  }
}

#[test]
fn the_self_check_holds_the_targets_clip_schema() {
  let clip2 = r#"{"OTIO_SCHEMA": "Clip.2", "media_references": {"DEFAULT_MEDIA": {"OTIO_SCHEMA": "MissingReference.1"}}, "active_media_reference_key": "DEFAULT_MEDIA"}"#;
  assert_eq!(
    validate_json(&document(clip2), OtioTarget::V0_15Plus),
    Ok(())
  );
  assert_eq!(
    shape_refusal(&document(clip2), OtioTarget::Legacy),
    (
      String::from("$.tracks.children[0].children[0].OTIO_SCHEMA"),
      "Clip.1, the only clip readers before OpenTimelineIO 0.15 know"
    )
  );
  let dangling = clip2.replace(
    r#""active_media_reference_key": "DEFAULT_MEDIA""#,
    r#""active_media_reference_key": "OTHER""#,
  );
  assert_eq!(
    shape_refusal(&document(&dangling), OtioTarget::V0_15Plus),
    (
      String::from("$.tracks.children[0].children[0].active_media_reference_key"),
      "a key of media_references"
    )
  );
  let no_url = clip2.replace("MissingReference.1", "ExternalReference.1");
  assert_eq!(
    shape_refusal(&document(&no_url), OtioTarget::V0_15Plus),
    (
      String::from("$.tracks.children[0].children[0].media_references.DEFAULT_MEDIA.target_url"),
      "a required key"
    )
  );
}

#[test]
fn a_document_that_is_not_json_is_refused_as_syntax() {
  assert_eq!(
    validate_json("{\"OTIO_SCHEMA\": ", OtioTarget::V0_15Plus),
    Err(Invalid::Syntax(Syntax {
      offset: 16,
      reason: "expected a value"
    }))
  );
  assert_eq!(
    std::string::ToString::to_string(&validate_json("[", OtioTarget::Legacy).unwrap_err()),
    "byte 1: expected a value"
  );
}

/// The words under `metadata.mediaio` of `value`, as `(word, value)` text
/// pairs, a nested object written as JSON.
fn words(value: &Value) -> Vec<(String, String)> {
  let ours = member(&member(value, "metadata"), "mediaio");
  ours
    .as_object()
    .unwrap()
    .iter()
    .map(|(key, word)| {
      let mut text = Utf8BytesMut::new();
      json::write_pretty(word, &mut text);
      (
        String::from(key.as_str()),
        String::from(text.freeze_shared().as_str()),
      )
    })
    .collect()
}

/// A clip `m` at [0, 24) of a 24 fps timeline, playing a second of a
/// 48 kHz medium, with a marker on the clip and one on its track.
fn marked() -> Timeline {
  let sound = tb(1, 48_000);
  let mut m = clip("m", 0, 24);
  m.set_source_range(TimeRange::new(48_000, 96_000, sound));
  m.markers_mut().push(
    Marker::new("breath", TimeRange::new(60_000, 72_000, sound))
      .with_color(MarkerColor::Cyan)
      .with_comment("a \"quoted\" note"),
  );
  one_track(
    Track::new(TrackKind::Audio, "A")
      .with_clip(m)
      .with_marker(Marker::new("act one", TimeRange::new(6, 18, edit()))),
  )
}

#[test]
fn markers_are_written_as_marker_2_where_their_owners_count_time() {
  let timeline = marked();
  assert_eq!(validate(&timeline), Ok(()));
  for target in [OtioTarget::V0_15Plus, OtioTarget::Legacy] {
    let text = to_otio(&timeline, target).unwrap();
    assert_eq!(validate_json(&text, target), Ok(()), "{target:?}");
  }
  let text = to_otio(&timeline, OtioTarget::V0_15Plus).unwrap();
  let root = json::parse(&text).unwrap();
  let track = member(&member(&root, "tracks"), "children")
    .as_array()
    .unwrap()[0]
    .clone();

  // The track's: at the edit rate, from the timeline's zero, with no words.
  let on_track = member(&track, "markers").as_array().unwrap()[0].clone();
  assert_eq!(member(&on_track, "OTIO_SCHEMA").as_str(), Some("Marker.2"));
  assert_eq!(member(&on_track, "metadata"), Value::Object(Vec::new()));
  assert_eq!(member(&on_track, "name").as_str(), Some("act one"));
  assert_eq!(member(&on_track, "color").as_str(), Some("GREEN"));
  assert_eq!(member(&on_track, "comment").as_str(), Some(""));
  assert_eq!(
    rate_start_duration(&member(&on_track, "marked_range")),
    (24.0, 6.0, 12.0)
  );

  // The clip's: in its medium's ticks, the exact ruler and timebase beside.
  let clip = member(&track, "children").as_array().unwrap()[0].clone();
  let on_clip = member(&clip, "markers").as_array().unwrap()[0].clone();
  assert_eq!(member(&on_clip, "name").as_str(), Some("breath"));
  assert_eq!(member(&on_clip, "color").as_str(), Some("CYAN"));
  assert_eq!(
    member(&on_clip, "comment").as_str(),
    Some("a \"quoted\" note")
  );
  assert_eq!(
    rate_start_duration(&member(&on_clip, "marked_range")),
    (48_000.0, 60_000.0, 12_000.0)
  );
  let range = "{\n    \"ruler\": \"48000/1\",\n    \"timebase\": \"1/48000\"\n}";
  assert_eq!(
    words(&on_clip),
    [(String::from("range"), String::from(range))]
  );
}

#[test]
fn the_exact_words_name_the_edit_rate_and_each_ranges_rulers() {
  // At 30000/1001 the edit rate's f64 names many rationals: the timeline
  // says which. A source range in 1/90 000 s ticks of a 29.97 fps medium is
  // written in its frames: the clip says which ruler, and which timebase
  // the model counts it in; so does its reference, for its available range.
  let edit = Rate::FPS_29_97;
  let ticks = tb(1, 90_000);
  let mut timeline = one_source(
    edit,
    30,
    TimeRange::new(3003, 3003 * 31, ticks),
    Some(Rate::FPS_29_97),
  );
  timeline.tracks_mut()[0].clips_mut()[0]
    .media_mut()
    .set_available_range(Some(TimeRange::new(0, 3003 * 100, ticks)));
  let text = to_otio(&timeline, OtioTarget::Legacy).unwrap();
  let root = json::parse(&text).unwrap();
  assert_eq!(
    words(&root),
    [(String::from("rate"), String::from("\"30000/1001\""))]
  );
  let exact = "{\n    \"ruler\": \"30000/1001\",\n    \"timebase\": \"1/90000\"\n}";
  let clip = first_clip(&timeline);
  assert_eq!(
    words(&clip),
    [
      (String::from("id"), String::from("\"a\"")),
      (String::from("source"), String::from(exact)),
    ]
  );
  assert_eq!(
    words(&member(&clip, "media_reference")),
    [
      (String::from("rate"), String::from("\"30000/1001\"")),
      (String::from("available"), String::from(exact)),
    ]
  );
  assert_eq!(
    source_range(&timeline),
    (Rate::FPS_29_97.as_f64(), 1.0, 30.0)
  );
}

/// Three abutting clips with a dissolve at each of their two cuts, kept in
/// `order`.
fn two_dissolves(order: [i64; 2]) -> Timeline {
  let mut track = Track::new(TrackKind::Video, "V");
  for (name, start) in [("a", 0), ("b", 24), ("c", 48)] {
    let mut placed = clip(name, start, start + 24);
    placed.set_source_range(TimeRange::new(10, 34, edit()));
    placed
      .media_mut()
      .set_available_range(Some(TimeRange::new(0, 100, edit())));
    track.clips_mut().push(placed);
  }
  for cut in order {
    track.transitions_mut().push(Transition::dissolve(
      crate::Timestamp::new(cut, edit()),
      Duration::new(2, edit()),
      Duration::new(2, edit()),
    ));
  }
  one_track(track)
}

/// The `order` word of each dissolve the first track writes, in the order
/// they are written.
fn orders(timeline: &Timeline) -> Vec<Option<String>> {
  let text = to_otio(timeline, OtioTarget::V0_15Plus).unwrap();
  let root = json::parse(&text).unwrap();
  let track = member(&member(&root, "tracks"), "children")
    .as_array()
    .unwrap()[0]
    .clone();
  member(&track, "children")
    .as_array()
    .unwrap()
    .iter()
    .filter(|child| member(child, "OTIO_SCHEMA").as_str() == Some("Transition.1"))
    .map(|dissolve| {
      words_or_none(dissolve)
        .into_iter()
        .find(|(word, _)| word == "order")
        .map(|(_, value)| value)
    })
    .collect()
}

/// [`words`], or none where the object's metadata holds no `mediaio`.
fn words_or_none(value: &Value) -> Vec<(String, String)> {
  let metadata = member(value, "metadata");
  if metadata.as_object().unwrap().is_empty() {
    Vec::new()
  } else {
    words(value)
  }
}

#[test]
fn a_dissolve_says_its_place_only_where_its_track_keeps_transitions_out_of_order() {
  // Written in their cuts' order, the dissolves need no word…
  let ordered = two_dissolves([24, 48]);
  assert_eq!(validate(&ordered), Ok(()));
  assert_eq!(orders(&ordered), [None, None]);
  // …kept the other way round, each says its place among the track's.
  let reversed = two_dissolves([48, 24]);
  assert_eq!(validate(&reversed), Ok(()));
  assert_eq!(
    orders(&reversed),
    [Some(String::from("1")), Some(String::from("0"))]
  );
  assert_eq!(
    to_otio(&reversed, OtioTarget::Legacy).map(drop),
    Ok(()),
    "the same dissolves, in the same places"
  );
}

#[test]
fn a_marker_no_ruler_holds_whole_is_refused_by_name() {
  // A track's marker is counted at the edit rate: past 2^53 there, refused
  // with no search.
  let mut timeline = marked();
  timeline.tracks_mut()[0].markers_mut()[0].set_range(TimeRange::new(0, TWO_53 + 1, edit()));
  assert_eq!(
    not_representable(&timeline),
    (
      Spot::Marker(MarkerAt::Track {
        track: 0,
        marker: 0
      }),
      i128::from(TWO_53 + 1)
    )
  );
  // A clip's marker of 2^62 thirds of a second: its own ticks, and the
  // coarsest whole ruler that lands on it — the same thirds — count past
  // 2^53, and no operand's ruler holds it either.
  let mut timeline = marked();
  timeline.tracks_mut()[0].clips_mut()[0].markers_mut()[0].set_range(TimeRange::new(
    0,
    1 << 62,
    tb(1, 3),
  ));
  assert_eq!(validate(&timeline), Ok(()));
  let refused = match to_otio(&timeline, OtioTarget::V0_15Plus) {
    Err(Refused::NotRepresentable(count)) => count,
    other => panic!("{other:?}"),
  };
  assert_eq!(
    (refused.at(), refused.value(), refused.rate()),
    (
      Spot::Marker(MarkerAt::Clip {
        clip: crate::ClipAt::new(0, 0),
        marker: 0
      }),
      1_i128 << 62,
      Rate::hz(3)
    )
  );
  assert_eq!(refused.searched().map(|search| search.walks()), Some(0));
  assert!(
    std::string::ToString::to_string(&refused).starts_with(
      "the marked range of track 0, clip 0, marker 0 counts 4611686018427387904 ticks"
    ),
    "{refused}"
  );
}

mod derived;
mod import;
