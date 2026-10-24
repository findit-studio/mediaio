//! What OpenTimelineIO derives from an exported document: refused where its
//! arithmetic would round, written where it does not — and read back, in
//! that arithmetic written again here from OpenTimelineIO's C++, to prove
//! it.

use super::*;

/// OpenTimelineIO's `RationalTime` as it computes, for reading a document
/// back: `value_rescaled_to` (`opentime/rationalTime.h` 70–75), `operator+`
/// (316–326), `operator-` (329–339) and `TimeRange::end_time_exclusive`
/// (`timeRange.h` 105–108), at `main` `00c22fa`.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Read {
  value: f64,
  rate: f64,
}

impl Read {
  fn at(self, rate: f64) -> f64 {
    if rate == self.rate {
      self.value
    } else {
      self.value * rate / self.rate
    }
  }

  fn plus(self, other: Self) -> Self {
    if self.rate < other.rate {
      Self {
        value: self.at(other.rate) + other.value,
        rate: other.rate,
      }
    } else {
      Self {
        value: other.at(self.rate) + self.value,
        rate: self.rate,
      }
    }
  }

  fn minus(self, other: Self) -> Self {
    if self.rate < other.rate {
      Self {
        value: self.at(other.rate) - other.value,
        rate: other.rate,
      }
    } else {
      Self {
        value: self.value - other.at(self.rate),
        rate: self.rate,
      }
    }
  }

  fn end(start: Self, duration: Self) -> Self {
    duration.plus(Self {
      value: start.at(duration.rate),
      rate: duration.rate,
    })
  }

  /// `TimeRange::end_time_inclusive` (`timeRange.h` 88–102): past one
  /// tick, the end floored where the duration is not whole, else the end
  /// less one tick; else the start.
  fn end_inclusive(start: Self, duration: Self) -> Self {
    let end = Self::end(start, duration);
    let start_there = Self {
      value: start.at(duration.rate),
      rate: duration.rate,
    };
    if end.minus(start_there).value > 1.0 {
      if duration.value != duration.value.floor() {
        Self {
          value: end.value.floor(),
          rate: end.rate,
        }
      } else {
        end.minus(Self {
          value: 1.0,
          rate: duration.rate,
        })
      }
    } else {
      start
    }
  }
}

/// A `RationalTime.1` object, read.
fn read(time: &Value) -> Read {
  Read {
    value: member(time, "value").as_f64().unwrap(),
    rate: member(time, "rate").as_f64().unwrap(),
  }
}

/// One child of a track, read: an item's source range, or a transition's
/// offsets.
#[derive(Debug, Clone, Copy)]
enum Kid {
  Item { start: Read, duration: Read },
  Transition { in_offset: Read, out_offset: Read },
}

impl Kid {
  fn duration(self) -> Read {
    match self {
      Self::Item { duration, .. } => duration,
      Self::Transition {
        in_offset,
        out_offset,
      } => in_offset.plus(out_offset),
    }
  }
}

/// Track `track` of `text`, its children read.
fn kids(text: &str, track: usize) -> Vec<Kid> {
  let root = json::parse(text).unwrap();
  let tracks = member(&member(&root, "tracks"), "children");
  let track = tracks.as_array().unwrap()[track].clone();
  member(&track, "children")
    .as_array()
    .unwrap()
    .iter()
    .map(
      |child| match member(child, "OTIO_SCHEMA").as_str().unwrap() {
        "Transition.1" => Kid::Transition {
          in_offset: read(&member(child, "in_offset")),
          out_offset: read(&member(child, "out_offset")),
        },
        _ => {
          let range = member(child, "source_range");
          Kid::Item {
            start: read(&member(&range, "start_time")),
            duration: read(&member(&range, "duration")),
          }
        }
      },
    )
    .collect()
}

/// `Track::range_of_child_at_index` (`track.cpp` 51–92): from zero in the
/// child's own rate, every item before it added; a transition starts its
/// `in_offset` earlier. The child's start and end.
fn place(kids: &[Kid], index: usize) -> (Read, Read) {
  let length = kids[index].duration();
  let mut start = Read {
    value: 0.0,
    rate: length.rate,
  };
  for kid in &kids[..index] {
    if let Kid::Item { duration, .. } = kid {
      start = start.plus(*duration);
    }
  }
  if let Kid::Transition { in_offset, .. } = kids[index] {
    start = start.minus(in_offset);
  }
  (start, Read::end(start, length))
}

/// `Track::available_range` (`track.cpp` 117–148): from zero at rate 1,
/// every item added.
fn track_duration(kids: &[Kid]) -> Read {
  kids.iter().fold(
    Read {
      value: 0.0,
      rate: 1.0,
    },
    |sum, kid| match kid {
      Kid::Item { duration, .. } => sum.plus(*duration),
      Kid::Transition { .. } => sum,
    },
  )
}

/// `Item::visible_range` (`item.cpp` 60–85): the source range widened by
/// the `in_offset` of a transition before the item and the `out_offset` of
/// one after it. Its start and duration.
fn visible_range(kids: &[Kid], index: usize) -> (Read, Read) {
  let Kid::Item {
    mut start,
    mut duration,
  } = kids[index]
  else {
    panic!("child {index} is no item");
  };
  if let Some(Kid::Transition { in_offset, .. }) = index.checked_sub(1).map(|before| kids[before]) {
    start = start.minus(in_offset);
    duration = duration.plus(in_offset);
  }
  if let Some(Kid::Transition { out_offset, .. }) = kids.get(index + 1) {
    duration = duration.plus(*out_offset);
  }
  (start, duration)
}

/// The visible range's start and end.
fn visible(kids: &[Kid], index: usize) -> (Read, Read) {
  let (start, duration) = visible_range(kids, index);
  (start, Read::end(start, duration))
}

/// A clip `id` playing `source` at `record`, of a medium with no stated
/// rate: its source range is written in its own timebase's ticks, or a
/// ruler the export's search moves it to.
fn placed(id: &str, source: TimeRange, record: TimeRange) -> Clip {
  Clip::new(
    ClipId::new(id),
    id,
    MediaRef::new(format_utf8bytes!("file:///{id}")),
    source,
    record,
  )
}

/// Why `to_otio` refuses `timeline`: where, the count and its ruler.
fn refused(timeline: &Timeline) -> (Spot, i128, Rate) {
  match to_otio(timeline, OtioTarget::V0_15Plus) {
    Err(Refused::NotRepresentable(count)) => (count.at(), count.value(), count.rate()),
    other => panic!("{other:?}"),
  }
}

fn video(clips: impl IntoIterator<Item = Clip>) -> Track {
  clips
    .into_iter()
    .fold(Track::new(TrackKind::Video, "V"), Track::with_clip)
}

fn second() -> Timebase {
  tb(1, 1)
}

/// Codex round 3's first case, a second earlier: a timeline at one frame a
/// second, a gap of 2^53 - 2 seconds, then a clip two seconds long whose
/// medium counts thirds of a second, from `third` thirds. Codex's gap,
/// 2^53 - 1 seconds, is one OpenTimelineIO 0.17 reads as 2^53 - 2
/// (`a_count_opentimelineio_0_17_reads_as_another_is_refused_by_name`).
fn after_a_long_gap(third: i64) -> Timeline {
  Timeline::new("t", Rate::hz(1)).with_track(video([placed(
    "a",
    TimeRange::new(third, third + 6, tb(1, 3)),
    TimeRange::new(TWO_53 - 2, TWO_53, second()),
  )]))
}

#[test]
fn a_clip_whose_ruler_is_finer_than_the_track_before_it_is_placed_where_opentimelineio_places_it() {
  // OpenTimelineIO starts the clip's place at zero in the clip's own rate,
  // three a second, and adds the gap there: 3 · (2^53 - 2), which an f64
  // holds as two less — the clip two thirds of a second early. Every count
  // written is within 2^53. From a third past a second no whole ruler
  // coarser than thirds holds the clip: refused, at the clip, the track's
  // second child, with the count OpenTimelineIO would round.
  let thirds = 3 * (i128::from(TWO_53) - 2);
  assert_eq!(
    refused(&after_a_long_gap(4)),
    (Spot::TrackPosition(ChildAt::new(0, 1)), thirds, Rate::hz(3))
  );
  let gap = Read {
    value: (TWO_53 - 2) as f64,
    rate: 1.0,
  };
  let reads = Read {
    value: 0.0,
    rate: 3.0,
  }
  .plus(gap);
  assert_eq!(reads.value as i128, thirds - 2);
}

#[test]
fn a_clip_a_coarser_whole_ruler_holds_is_written_in_it_and_placed_exactly() {
  // From a whole second, whole seconds hold the clip: it is written in them,
  // the track is one ruler, and OpenTimelineIO places the clip at 2^53 - 2
  // and ends it at 2^53, exactly.
  let timeline = after_a_long_gap(3);
  let text = to_otio(&timeline, OtioTarget::V0_15Plus).unwrap();
  assert_eq!(validate_json(&text, OtioTarget::V0_15Plus), Ok(()));
  let kids = kids(&text, 0);
  let Kid::Item { start, duration } = kids[1] else {
    panic!("{kids:?}");
  };
  assert_eq!(
    (start, duration),
    (
      Read {
        value: 1.0,
        rate: 1.0
      },
      Read {
        value: 2.0,
        rate: 1.0
      }
    )
  );
  let seconds = |value: i64| Read {
    value: value as f64,
    rate: 1.0,
  };
  assert_eq!(place(&kids, 1), (seconds(TWO_53 - 2), seconds(TWO_53)));
  assert_eq!(track_duration(&kids), seconds(TWO_53));
}

#[test]
fn a_mixed_rate_track_whose_derived_places_are_whole_is_written_and_read_back_exactly() {
  // At 25 fps: a 10-frame gap; `a` in frames of its 50 fps medium; `b` in
  // 48 kHz samples; another 10-frame gap; `c` in milliseconds. OpenTimelineIO
  // carries each place in the highest rate before it, where each count is
  // whole: every clip lands on its record, exactly, and the track ends at
  // its last record.
  let edit = tb(1, 25);
  let a = placed(
    "a",
    TimeRange::new(100, 200, tb(1, 50)),
    TimeRange::new(10, 60, edit),
  );
  let mut a = a;
  a.media_mut().set_rate(Some(Rate::FPS_50));
  let b = placed(
    "b",
    TimeRange::new(48_000, 144_000, tb(1, 48_000)),
    TimeRange::new(60, 110, edit),
  );
  let c = placed(
    "c",
    TimeRange::new(1_000, 3_000, Timebase::MILLIS),
    TimeRange::new(120, 170, edit),
  );
  let timeline = Timeline::new("t", Rate::FPS_25)
    .with_start(Timestamp::new(90_000, edit))
    .with_track(video([a, b, c]));
  let text = to_otio(&timeline, OtioTarget::V0_15Plus).unwrap();
  let kids = kids(&text, 0);
  assert_eq!(kids.len(), 5);
  // Each clip's place, in the rate OpenTimelineIO carries it in: its record
  // counted there, with no remainder.
  for (child, rate, frames) in [(1, 50.0, 10), (2, 48_000.0, 60), (4, 48_000.0, 120)] {
    let (start, _) = place(&kids, child);
    assert_eq!(
      start,
      Read {
        value: frames as f64 * rate / 25.0,
        rate
      },
      "child {child}"
    );
  }
  assert_eq!(
    track_duration(&kids),
    Read {
      value: 170.0 * 1_920.0,
      rate: 48_000.0
    }
  );
}

#[test]
#[cfg_attr(
  miri,
  ignore = "too heavy for Miri on a 32-bit target; the laws Miri runs reach its every branch"
)]
fn the_goldens_read_back_put_every_clip_on_its_record() {
  // The law timeline, mixed 23.976 fps frames and 48 kHz samples: read back
  // as OpenTimelineIO reads it, `b` starts at frame 96 and `a-sound` at
  // sample 48 048, frame 24 — exactly.
  for golden in [
    include_str!("../../../tests/golden/law.v0_15.otio"),
    include_str!("../../../tests/golden/law.legacy.otio"),
  ] {
    let ntsc = Rate::FPS_23_976.as_f64();
    let video = kids(golden, 0);
    assert_eq!(
      place(&video, 2).0,
      Read {
        value: 96.0,
        rate: ntsc
      }
    );
    let audio = kids(golden, 1);
    assert_eq!(
      place(&audio, 2).0,
      Read {
        value: 48_048.0,
        rate: 48_000.0
      }
    );
    assert_eq!(
      track_duration(&audio),
      Read {
        value: 240_240.0,
        rate: 48_000.0
      }
    );
  }
}

#[test]
fn a_count_opentimelineio_rescales_a_tick_off_is_refused_though_within_2_53() {
  // At 3 fps, `a` plays v = 3w thirds of a second, then `b`, whose medium
  // counts sevenths. OpenTimelineIO places `b` at zero in sevenths and adds
  // `a` there: v · 7 / 3, its product past 2^54, where an f64 steps by four
  // — 7v rounds by two, and `b` lands a seventh of a second early. Every
  // count, the product's quotient 7w among them, is within 2^53, and no
  // ruler the timeline counts in holds both clips: `a` from a third of a
  // second is no whole number of sevenths, `b` from a seventh none of
  // thirds — and in 21sts `a` runs 7v, past 2^53.
  let w: i64 = 1_000_000_000_000_002;
  let v = 3 * w;
  let timeline = Timeline::new("t", Rate::hz(3)).with_track(video([
    placed(
      "a",
      TimeRange::new(1, 1 + v, tb(1, 3)),
      TimeRange::new(0, v, tb(1, 3)),
    ),
    placed(
      "b",
      TimeRange::new(1, 8, tb(1, 7)),
      TimeRange::new(v, v + 3, tb(1, 3)),
    ),
  ]));
  assert_eq!(
    refused(&timeline),
    (
      Spot::TrackPosition(ChildAt::new(0, 1)),
      7 * i128::from(w),
      Rate::hz(7)
    )
  );
  let reads = Read {
    value: 0.0,
    rate: 7.0,
  }
  .plus(Read {
    value: v as f64,
    rate: 3.0,
  });
  assert_eq!(reads.value as i128, 7 * i128::from(w) - 1);
}

/// A clip `a` one second long in thirds of a second, from a third past a
/// second — no coarser whole ruler holds it — then a gap of `gap` seconds and
/// a clip `b` of `seconds`, at one frame a second from `start`.
fn thirds_then_seconds(gap: i64, seconds: i64, start: i64) -> Timeline {
  let at = 1 + gap;
  Timeline::new("t", Rate::hz(1))
    .with_start(Timestamp::new(start, second()))
    .with_track(video([
      placed(
        "a",
        TimeRange::new(4, 7, tb(1, 3)),
        TimeRange::new(0, 1, second()),
      ),
      placed(
        "b",
        TimeRange::new(0, seconds, second()),
        TimeRange::new(at, at + seconds, second()),
      ),
    ]))
}

#[test]
fn a_track_whose_duration_opentimelineio_sums_past_2_53_is_refused() {
  // `a` in thirds puts the track's sum in thirds: the places are within
  // 2^53 — `b` starts at 3 + 3·g thirds — but the track's duration adds
  // `b`'s five seconds as fifteen thirds, past it.
  let gap = 3_002_399_751_580_329;
  assert_eq!(
    refused(&thirds_then_seconds(gap, 5, 0)),
    (
      Spot::TrackDuration(0),
      3 * i128::from(1 + gap + 5),
      Rate::hz(3)
    )
  );
}

/// Track `0` of length `first` thirds of a second and track `1` of
/// `second`, at 3 fps.
fn two_tracks(first: i64, second: i64) -> Timeline {
  let third = tb(1, 3);
  let track = |id: &str, length: i64| {
    video([placed(
      id,
      TimeRange::new(0, length, third),
      TimeRange::new(0, length, third),
    )])
  };
  Timeline::new("t", Rate::hz(3))
    .with_track(track("a", first))
    .with_track(track("b", second))
}

#[test]
fn a_stack_whose_longest_track_opentimelineio_cannot_tell_from_a_shorter_one_is_refused() {
  // Two tracks a third of a second apart near 2^51 seconds, where an f64
  // steps by halves: OpenTimelineIO compares their durations as seconds,
  // reads both as the same, and keeps the first — the shorter one, so the
  // timeline would end a third of a second early. The other way round it
  // keeps the longer, and the export is written.
  let j = 1_i64 << 51;
  let (shorter, longer) = (3 * j + 1, 3 * j + 2);
  assert_eq!(shorter as f64 / 3.0, longer as f64 / 3.0);
  assert_eq!(
    refused(&two_tracks(shorter, longer)),
    (Spot::StackDuration, i128::from(longer), Rate::hz(3))
  );
  assert!(to_otio(&two_tracks(longer, shorter), OtioTarget::V0_15Plus).is_ok());
}

/// Codex round 3's second case: at one frame a second, `a` plays one second
/// of `outgoing` and `b` one second of `incoming`, with a dissolve between
/// them reaching `into` before the cut and `out` after it; no available
/// range is known, so validation holds no handle.
fn handles(outgoing: i64, incoming: i64, into: u64, out: u64) -> Timeline {
  let second = second();
  Timeline::new("t", Rate::hz(1)).with_track(
    video([
      placed(
        "a",
        TimeRange::new(outgoing, outgoing + 1, second),
        TimeRange::new(0, 1, second),
      ),
      placed(
        "b",
        TimeRange::new(incoming, incoming + 1, second),
        TimeRange::new(1, 2, second),
      ),
    ])
    .with_transition(Transition::dissolve(
      Timestamp::new(1, second),
      Duration::new(into, second),
      Duration::new(out, second),
    )),
  )
}

#[test]
fn a_visible_range_a_transitions_handle_widens_past_2_53_is_refused() {
  // The outgoing source [2^53 - 1, 2^53) plays on a second past its end:
  // OpenTimelineIO's visible range of `a` ends at 2^53 + 1, read as 2^53 —
  // refused.
  let past = i128::from(TWO_53) + 1;
  assert_eq!(
    refused(&handles(TWO_53 - 1, 0, 0, 1)),
    (Spot::Visible(ChildAt::new(0, 0)), past, Rate::hz(1))
  );
  // The incoming source from -2^53 starts a second before it: -2^53 - 1.
  assert_eq!(
    refused(&handles(0, -TWO_53, 1, 0)),
    (Spot::Visible(ChildAt::new(0, 2)), -past, Rate::hz(1))
  );
}

#[test]
fn a_visible_range_inside_the_bound_is_written_and_read_back_exactly() {
  // One second earlier the handle ends `a`'s visible range at 2^53 itself,
  // and the incoming one starts at 1 - 2^53, its source from 2 - 2^53: both
  // read back exactly.
  let text = to_otio(&handles(TWO_53 - 2, 0, 0, 1), OtioTarget::V0_15Plus).unwrap();
  let seconds = |value: i64| Read {
    value: value as f64,
    rate: 1.0,
  };
  assert_eq!(
    visible(&kids(&text, 0), 0),
    (seconds(TWO_53 - 2), seconds(TWO_53))
  );
  let text = to_otio(&handles(0, 2 - TWO_53, 1, 0), OtioTarget::V0_15Plus).unwrap();
  assert_eq!(
    visible(&kids(&text, 0), 2),
    (seconds(1 - TWO_53), seconds(3 - TWO_53))
  );
}

/// Codex round 3's third case: at one frame a second from `start`, one
/// clip one second long.
fn one_second_from(start: i64) -> Timeline {
  seconds_long_from(start, 1)
}

/// At one frame a second from `start`, one clip `seconds` long.
fn seconds_long_from(start: i64, seconds: i64) -> Timeline {
  let second = second();
  Timeline::new("t", Rate::hz(1))
    .with_start(Timestamp::new(start, second))
    .with_track(video([placed(
      "a",
      TimeRange::new(0, seconds, second),
      TimeRange::new(0, seconds, second),
    )]))
}

#[test]
fn a_place_from_the_global_start_past_2_53_is_refused() {
  // From 2^53 the clip ends at 2^53 + 1 — the global start, written within
  // 2^53, plus the record, which an adapter adds up and rounds.
  assert_eq!(
    refused(&one_second_from(TWO_53)),
    (
      Spot::Absolute(ChildAt::new(0, 0)),
      i128::from(TWO_53) + 1,
      Rate::hz(1)
    )
  );
}

#[test]
fn a_global_start_whose_end_stays_within_2_53_is_written_and_read_back_exactly() {
  // From 2^53 - 2 a clip two seconds long ends at 2^53.
  let text = to_otio(&seconds_long_from(TWO_53 - 2, 2), OtioTarget::V0_15Plus).unwrap();
  let root = json::parse(&text).unwrap();
  let start = read(&member(&root, "global_start_time"));
  let kids = kids(&text, 0);
  let (_, ends) = place(&kids, 0);
  // `operator+`, and a range of the duration from the start, as toucan,
  // raven and the FCP XML adapter run one.
  let end = Read {
    value: TWO_53 as f64,
    rate: 1.0,
  };
  assert_eq!(start.plus(ends), end);
  assert_eq!(Read::end(start, track_duration(&kids)), end);
}

#[test]
fn a_tracks_end_from_the_global_start_past_2_53_is_refused() {
  // From one second, `a` in thirds then `b` until 2^53 - 2 thirds: every
  // place is within 2^53, and so is the track's duration — but a range of
  // it from the global start ends at 2^53 + 1 thirds.
  let seconds = 3_002_399_751_580_329;
  assert_eq!(
    refused(&thirds_then_seconds(0, seconds, 1)),
    (Spot::AbsoluteEnd(0), i128::from(TWO_53) + 1, Rate::hz(3))
  );
}

/// Codex round 4's first case: at 7 fps, `a`, then `b` — 360 287 970 189 200
/// frames of a 25 fps medium from frame 1 099 511 627 816 — then `c`, a
/// dissolve reaching `head` ticks into `a` before the cut into `b`, and one
/// reaching 1 099 511 627 815 ticks into `c` after the cut out of `b`. No
/// available range is known, so validation holds no handle; no clip has a
/// coarser ruler.
fn widened(head: u64) -> Timeline {
  let seven = tb(1, 7);
  let (a, c) = (1_125_899_906_843_277_i64, 1_099_511_627_815_i64);
  let (from, frames) = (1_099_511_627_816_i64, 360_287_970_189_200_i64);
  let b = frames * 7 / 25;
  Timeline::new("t", Rate::hz(7)).with_track(
    video([
      placed(
        "a",
        TimeRange::new(1, 1 + a, seven),
        TimeRange::new(0, a, seven),
      ),
      placed(
        "b",
        TimeRange::new(from, from + frames, tb(1, 25)),
        TimeRange::new(a, a + b, seven),
      ),
      placed(
        "c",
        TimeRange::new(1, 1 + c, seven),
        TimeRange::new(a + b, a + b + c, seven),
      ),
    ])
    .with_transition(Transition::dissolve(
      Timestamp::new(a, seven),
      Duration::new(head, seven),
      Duration::new(0, seven),
    ))
    .with_transition(Transition::dissolve(
      Timestamp::new(a + b, seven),
      Duration::new(0, seven),
      Duration::new(c.unsigned_abs(), seven),
    )),
  )
}

#[test]
fn a_last_tick_opentimelineio_derives_half_a_tick_off_is_refused() {
  // `b`'s visible range starts the first dissolve's reach before its source
  // and runs on through the second: OpenTimelineIO carries it in frames of
  // 25 fps, every sum rounded, its start, duration and end each within half
  // a frame of exact. But its duration rounds onto a whole number of frames,
  // which exactly it is not, so `end_time_inclusive` takes the end less one
  // frame instead of the end floored — half a frame early.
  let head = 1_125_899_906_843_277;
  assert_eq!(
    refused(&widened(head)),
    (
      Spot::Visible(ChildAt::new(0, 2)),
      365_314_309_059_212,
      Rate::hz(25)
    )
  );
  // OpenTimelineIO's arithmetic on the counts the export would write.
  let frames = |value: f64| Read { value, rate: 25.0 };
  let ticks = |value: f64| Read { value, rate: 7.0 };
  let start = frames(1_099_511_627_816.0).minus(ticks(head as f64));
  let duration = frames(360_287_970_189_200.0)
    .plus(ticks(head as f64))
    .plus(ticks(1_099_511_627_815.0));
  assert_eq!(duration.value, 4_385_285_893_300_243.0);
  assert_eq!(Read::end(start, duration).value, 365_314_309_059_212.5);
  assert_eq!(
    Read::end_inclusive(start, duration),
    frames(365_314_309_059_211.5)
  );
  // Exactly, the reach before the cut cancels: the range ends at
  // (7 · (from + frames) + 25 · out) / 7 frames, and its duration is no
  // whole number of frames, so its last is that end floored.
  let end = 7 * (1_099_511_627_816_i128 + 360_287_970_189_200) + 25 * 1_099_511_627_815;
  assert_eq!(end, 2_557_200_163_414_487);
  assert_eq!(end.div_euclid(7), 365_314_309_059_212);
}

#[test]
fn a_last_tick_one_tick_inside_is_written_and_read_back_exactly() {
  // A tick less reach before the cut, and OpenTimelineIO's visible duration
  // of `b` is no whole number of frames either: it floors the end, onto the
  // exact last frame.
  let text = to_otio(&widened(1_125_899_906_843_276), OtioTarget::V0_15Plus).unwrap();
  assert_eq!(validate_json(&text, OtioTarget::V0_15Plus), Ok(()));
  let (start, duration) = visible_range(&kids(&text, 0), 2);
  assert_eq!(duration.value, 4_385_285_893_300_239.5);
  assert_eq!(
    Read::end_inclusive(start, duration),
    Read {
      value: 365_314_309_059_212.0,
      rate: 25.0
    }
  );
}

/// The generator's seed 132, its track 1, alone: at 29.97 fps from frame
/// 106 560, an 18-frame gap, `a` in 48 kHz samples, a dissolve, then `b` in
/// ticks of 1/90 000 s — neither with a coarser ruler.
fn from_frame_106_560() -> Timeline {
  let edit = tb(1001, 30_000);
  Timeline::new("t", Rate::FPS_29_97)
    .with_start(Timestamp::new(106_560, edit))
    .with_track(
      video([
        placed(
          "a",
          TimeRange::new(69, 136_205, tb(1, 48_000)),
          TimeRange::new(18, 103, edit),
        ),
        placed(
          "b",
          TimeRange::new(45_196, 132_283, tb(1, 90_000)),
          TimeRange::new(103, 132, edit),
        ),
      ])
      .with_transition(Transition::dissolve(
        Timestamp::new(103, edit),
        Duration::new(15, edit),
        Duration::new(9, edit),
      )),
    )
}

#[test]
fn a_tracks_last_tick_from_the_global_start_opentimelineio_reads_late_is_refused() {
  // Exactly the track is 396 396 ticks of 1/90 000 s — a frame is 3003 of
  // them — but OpenTimelineIO adds its frames up in samples first, where
  // they are no whole number, and lands an ulp short of whole. So its range
  // from the global start, as OpenTimelineIO's tools form one, ends
  // inclusively at its end floored — and that end rounds up onto
  // 320 396 076, a tick past the last.
  assert_eq!(
    refused(&from_frame_106_560()),
    (Spot::AbsoluteEnd(0), 320_396_075, Rate::hz(90_000))
  );
  let frames = |value: f64| Read {
    value,
    rate: Rate::FPS_29_97.as_f64(),
  };
  let duration = [
    frames(18.0),
    Read {
      value: 136_136.0,
      rate: 48_000.0,
    },
    Read {
      value: 87_087.0,
      rate: 90_000.0,
    },
  ]
  .into_iter()
  .fold(
    Read {
      value: 0.0,
      rate: 1.0,
    },
    Read::plus,
  );
  assert_eq!(duration.value, f64::from_bits(396_396.0_f64.to_bits() - 1));
  assert_eq!(
    Read::end_inclusive(frames(106_560.0), duration),
    Read {
      value: 320_396_076.0,
      rate: 90_000.0
    }
  );
  // Exactly: from 106 560 · 3003 ticks, 396 396 long, a whole number —
  // the end less one.
  assert_eq!(106_560 * 3003 + 396_396 - 1, 320_396_075);
}

/// Codex round 4's `v`: thirds of a second, its triple a whole number of
/// ninths within 2^53, its ninefold past 2^54.
const V: i64 = 2_001_599_834_386_890;

/// Codex round 4's second case and its kin: at 3 fps, `a` plays `V` thirds
/// of a second from a third of a second into a medium counted in `a_rate`
/// ticks a second, then `b` a third of a second from a ninth into one
/// counted in `b_rate`.
fn shared(a_rate: i32, b_rate: i32) -> Timeline {
  let (a, b, third) = (i64::from(a_rate), i64::from(b_rate), tb(1, 3));
  Timeline::new("t", Rate::hz(3)).with_track(video([
    placed(
      "a",
      TimeRange::new(a / 3, a / 3 + V * a / 3, tb(1, a_rate)),
      TimeRange::new(0, V, third),
    ),
    placed(
      "b",
      TimeRange::new(b / 9, b / 9 + b / 3, tb(1, b_rate)),
      TimeRange::new(V, V + 1, third),
    ),
  ]))
}

/// `shared`'s track exported and read back: both clips written in ninths
/// of a second, and each where its record puts it, exactly.
fn in_ninths(timeline: &Timeline) {
  let text = to_otio(timeline, OtioTarget::V0_15Plus).unwrap();
  assert_eq!(validate_json(&text, OtioTarget::V0_15Plus), Ok(()));
  let kids = kids(&text, 0);
  let ninths = |value: i64| Read {
    value: value as f64,
    rate: 9.0,
  };
  let written: Vec<(Read, Read)> = kids
    .iter()
    .map(|kid| match *kid {
      Kid::Item { start, duration } => (start, duration),
      Kid::Transition { .. } => panic!("{kids:?}"),
    })
    .collect();
  assert_eq!(
    written,
    [(ninths(3), ninths(3 * V)), (ninths(1), ninths(3))]
  );
  assert_eq!(place(&kids, 1), (ninths(3 * V), ninths(3 * V + 3)));
  assert_eq!(track_duration(&kids), ninths(3 * V + 3));
}

#[test]
fn clips_held_only_in_a_ruler_they_share_are_written_in_it() {
  // Codex round 4's case: `a` in twelfths, `b` in 27ths. OpenTimelineIO
  // carries `b`'s place in 27ths, 9v, past 2^53; in their coarsest rulers,
  // thirds and ninths, it rescales v thirds into ninths as v · 9 / 3, a
  // product past 2^54 that rounds: a ninth short. Ninths hold both, and
  // `a` from a third of a second is a whole number of them too.
  in_ninths(&shared(12, 27));
}

#[test]
fn a_clip_only_a_ruler_between_its_own_and_its_coarsest_holds_is_written_in_it() {
  // `b` in ninths from the start: `a` in its own twelfths carries `b`'s end
  // through 36v / 12, in sixths and in its coarsest, thirds, through 18v / 6
  // and 9v / 3 — products past 2^54 that round. Only ninths, between its
  // own ruler and its coarsest, hold it.
  in_ninths(&shared(12, 9));
}

#[test]
fn a_clip_is_written_coarser_while_its_neighbour_keeps_its_own_ruler() {
  // `a` in ninths, `b` in 27ths: `b` in its coarsest, ninths, holds, and
  // `a` keeps its own ninths — written in its coarsest, thirds, too, it
  // would carry `b`'s place through 9v / 3 again.
  in_ninths(&shared(9, 27));
}

#[test]
fn a_track_no_ruler_holds_is_refused_with_the_last_plan_the_search_walked() {
  // At 1 fps, a gap of 2^53 seconds, then `a`, one second of a medium
  // counted in halves. In halves OpenTimelineIO places `a` 2^54 halves in,
  // past 2^53; in seconds, its coarsest ruler and its last, it starts at
  // 2^53 and ends past it — the refusal the search ends on.
  let timeline = Timeline::new("t", Rate::hz(1)).with_track(video([placed(
    "a",
    TimeRange::new(0, 2, tb(1, 2)),
    TimeRange::new(TWO_53, TWO_53 + 1, second()),
  )]));
  assert_eq!(
    refused(&timeline),
    (
      Spot::TrackPosition(ChildAt::new(0, 1)),
      i128::from(TWO_53) + 1,
      Rate::hz(1)
    )
  );
}

#[test]
fn a_clip_is_written_in_the_finer_ruler_its_neighbour_is_counted_in() {
  // `a` in thirds, then `b` in ninths: OpenTimelineIO places `b` at zero in
  // ninths and adds `a` there, v · 9 / 3, a product past 2^54 that rounds —
  // and `a`'s own thirds are its coarsest whole ruler. But `b`'s ninths,
  // finer than `a`'s own, hold `a` too, from three of them for 3v: written
  // in its neighbour's ruler, `a` is counted as `b` is, and nothing is
  // rescaled.
  in_ninths(&shared(3, 9));
}

/// Codex round 5's `N`: ticks of 1/127 s, of which 65 · N lie within 2^53.
const N: i64 = 138_572_296_126_847;

/// One clip `a`, `ticks` ticks of 1/127 s from zero, its medium stated at
/// (k + 1) · 127 fps, so planned in those frames, on a timeline at k · 127
/// fps from `start`.
fn a_frame_rate_above(k: i32, ticks: i64, start: i64) -> Timeline {
  let edit = tb(1, 127 * k);
  let mut a = placed(
    "a",
    TimeRange::new(0, ticks, tb(1, 127)),
    TimeRange::new(0, i64::from(k) * ticks, edit),
  );
  a.media_mut().set_rate(Some(Rate::hz(127 * (k + 1))));
  Timeline::new("t", Rate::hz(127 * k))
    .with_start(Timestamp::new(start, edit))
    .with_track(video([a]))
}

/// `a_frame_rate_above(k, ticks, start)` exported and read back: `a`
/// written in the edit rate's ticks, k · `ticks` of them, and its end from
/// the global start — by `operator+`, and as the end of a range of the
/// track's duration from it — at `start + k · ticks`, its last tick one
/// before, exactly. Answers the global start, read back.
fn in_the_edit_rate(k: i32, ticks: i64, start: i64) -> Read {
  let text = to_otio(&a_frame_rate_above(k, ticks, start), OtioTarget::V0_15Plus).unwrap();
  assert_eq!(validate_json(&text, OtioTarget::V0_15Plus), Ok(()));
  let rate = f64::from(127 * k);
  let edit = |value: i64| Read {
    value: value as f64,
    rate,
  };
  let root = json::parse(&text).unwrap();
  let global = read(&member(&root, "global_start_time"));
  assert_eq!(global, edit(start));
  let kids = kids(&text, 0);
  let length = i64::from(k) * ticks;
  let Kid::Item {
    start: from,
    duration,
  } = kids[0]
  else {
    panic!("{kids:?}");
  };
  assert_eq!((from, duration), (edit(0), edit(length)));
  let end = edit(start + length);
  assert_eq!(global.plus(place(&kids, 0).1), end);
  let track = track_duration(&kids);
  assert_eq!(Read::end(global, track), end);
  assert_eq!(Read::end_inclusive(global, track), edit(start + length - 1));
  global
}

#[test]
fn a_clip_only_the_edit_rate_holds_is_written_in_it() {
  // Codex round 5's case: at 8128 = 64 · 127 fps from 7 000 000, `a` runs N
  // ticks of 1/127 s, its medium stated at 8255 = 65 · 127 fps. In its
  // plan's 8255ths its end from the global start lies past 2^53. Below the
  // plan's lie 64 whole rulers, 127 · m: the 63 coarsest each rescale its
  // end into the edit rate a tick short, and the 64th is the edit rate's
  // own — the one ruler that holds it, a cap of the 63 coarsest would drop
  // it. Written in the edit rate, nothing is rescaled: read back exactly.
  let global = in_the_edit_rate(64, N, 7_000_000);
  // In OpenTimelineIO's arithmetic: in the plan's frames, the global start
  // rescaled to 7 109 375 of them, the end lies past 2^53; carried in the
  // edit rate from each of the 63 coarsest, a tick short of the exact
  // 64N + 7 000 000.
  let plan = Read {
    value: (65 * N) as f64,
    rate: 8255.0,
  };
  assert_eq!(global.plus(plan).value, 9_007_199_255_354_430.0);
  for m in 1..=63 {
    let ruler = Read {
      value: (N * m) as f64,
      rate: (127 * m) as f64,
    };
    assert_eq!(
      global.plus(ruler).value,
      8_868_626_959_118_207.0,
      "127 · {m}"
    );
  }
  assert_eq!(64 * N + 7_000_000, 8_868_626_959_118_208);
}

/// Codex round 5's construction at k = 100: ticks of 1/127 s, of which
/// 101 · M lie within 2^53, and whose 100 · M, rescaled from any of the 64
/// coarsest whole rulers below 12 827 fps into 12 700 fps, is read a tick
/// short.
const M: i64 = 88_628_115_813_744;

#[test]
fn the_edit_rate_is_tried_however_many_whole_rulers_lie_below_the_plans() {
  // At 12 700 = 100 · 127 fps from 10^14, `a` runs M ticks of 1/127 s, its
  // medium stated at 12 827 = 101 · 127 fps. A hundred whole rulers lie
  // below the plan's, 127 · m: the finest is the edit rate's, and the 99
  // others, free, fill both of the search's free bands — the 64 finest,
  // 127 · 36 … 127 · 99, and the 35 below them. Each of the 64 coarsest
  // rescales `a`'s end from the global start into the edit rate a tick
  // short (127 · 65, among the finest, reads it exactly). The edit rate is
  // an operand's ruler, never capped and tried before any free one:
  // written in it, `a` reads back exactly.
  let global = in_the_edit_rate(100, M, 100_000_000_000_000);
  let plan = Read {
    value: (101 * M) as f64,
    rate: 12_827.0,
  };
  assert_eq!(global.plus(plan).value, 9_052_439_697_188_144.0);
  for m in 1..=64 {
    let ruler = Read {
      value: (M * m) as f64,
      rate: (127 * m) as f64,
    };
    assert_eq!(
      global.plus(ruler).value,
      8_962_811_581_374_399.0,
      "127 · {m}"
    );
  }
  assert_eq!(100 * M + 100_000_000_000_000, 8_962_811_581_374_400);
}

/// How long `b` runs in [`beside_a_neighbour`]: 600 000 000 000 frames of
/// 127 fps.
const LONG: i64 = 600_000_000_000;

/// At 127 fps: `a`, M ticks of 1/127 s from zero, its medium stated at
/// 12 827 = 101 · 127 fps; then `b`, [`LONG`] frames, counted in its own
/// 1/12 700 s from one of them — so `b` has no other ruler.
fn beside_a_neighbour() -> Timeline {
  let edit = tb(1, 127);
  let mut a = placed("a", TimeRange::new(0, M, edit), TimeRange::new(0, M, edit));
  a.media_mut().set_rate(Some(Rate::hz(12_827)));
  let b = placed(
    "b",
    TimeRange::new(1, 1 + 100 * LONG, tb(1, 12_700)),
    TimeRange::new(M, M + LONG, edit),
  );
  Timeline::new("t", Rate::hz(127)).with_track(video([a, b]))
}

#[test]
fn a_clip_only_its_neighbours_ruler_holds_is_written_in_it() {
  // `a` again, a neighbour after it in place of the global start: in its
  // plan's 12 827ths the track's duration sums past 2^53, and OpenTimelineIO
  // places `b` at zero in 12 700ths and adds `a` there — from each of the 64
  // coarsest whole rulers below `a`'s plan's, a tick short. `b`'s 12 700ths,
  // another clip's ruler and not the edit rate's, hold `a`: written in them,
  // both clips are counted alike and the track reads back exactly.
  let text = to_otio(&beside_a_neighbour(), OtioTarget::V0_15Plus).unwrap();
  assert_eq!(validate_json(&text, OtioTarget::V0_15Plus), Ok(()));
  let kids = kids(&text, 0);
  let ticks = |value: i64| Read {
    value: value as f64,
    rate: 12_700.0,
  };
  let written: Vec<(Read, Read)> = kids
    .iter()
    .map(|kid| match *kid {
      Kid::Item { start, duration } => (start, duration),
      Kid::Transition { .. } => panic!("{kids:?}"),
    })
    .collect();
  assert_eq!(
    written,
    [(ticks(0), ticks(100 * M)), (ticks(1), ticks(100 * LONG))]
  );
  assert_eq!(place(&kids, 1), (ticks(100 * M), ticks(100 * (M + LONG))));
  assert_eq!(track_duration(&kids), ticks(100 * (M + LONG)));
  // In OpenTimelineIO's arithmetic: planned, the track's duration lies past
  // 2^53; from each of the 64 coarsest rulers, `b`'s place a tick short.
  let planned = [
    Read {
      value: (101 * M) as f64,
      rate: 12_827.0,
    },
    ticks(100 * LONG),
  ];
  let sum = planned.into_iter().fold(
    Read {
      value: 0.0,
      rate: 1.0,
    },
    Read::plus,
  );
  assert_eq!(
    sum,
    Read {
      value: 9_012_039_697_188_144.0,
      rate: 12_827.0
    }
  );
  for m in 1..=64 {
    let ruler = Read {
      value: (M * m) as f64,
      rate: (127 * m) as f64,
    };
    assert_eq!(
      ticks(0).plus(ruler).value,
      (100 * M - 1) as f64,
      "127 · {m}"
    );
  }
}

/// One clip `a`, `ticks` ticks of 1/127 s from the first — a multiple of
/// 127, so its record is a whole number of edit-rate ticks — its medium
/// stated at `stated` fps, so planned in those frames, on a timeline at
/// `edit` fps, prime to 127, from `start`. Neither the edit rate nor 1 holds
/// a 127th of a second, so `a` has no named ruler, and its coarsest whole
/// ruler is 127: its free rulers are 127 · m below `stated`.
fn from_a_tick(stated: i32, edit: i32, ticks: i64, start: i64) -> Timeline {
  let at = tb(1, edit);
  let mut a = placed(
    "a",
    TimeRange::new(1, 1 + ticks, tb(1, 127)),
    TimeRange::new(0, ticks / 127 * i64::from(edit), at),
  );
  a.media_mut().set_rate(Some(Rate::hz(stated)));
  Timeline::new("t", Rate::hz(edit))
    .with_start(Timestamp::new(start, at))
    .with_track(video([a]))
}

/// The end of [`from_a_tick`]'s clip from the global start, as
/// OpenTimelineIO's `operator+` reads it, were the clip written in 127 · `m`
/// fps: its `ticks · m` rescaled into the edit rate, `(value · rate) /
/// from`, and added.
fn end_from(global: Read, ticks: i64, m: i64) -> f64 {
  global
    .plus(Read {
      value: (ticks * m) as f64,
      rate: (127 * m) as f64,
    })
    .value
}

/// Asserts that OpenTimelineIO's `read`, a double of magnitude 2^52 or more
/// and so a whole number, lies less than half a tick from `num / den`
/// ticks: read to the nearest tick, it is the exact count.
fn within_half_a_tick(read: Read, num: i128, den: i128) {
  assert!(read.value.abs() >= 4_503_599_627_370_496.0, "{read:?}");
  let off = (read.value as i128 * den - num).unsigned_abs();
  assert!(2 * off < den.unsigned_abs(), "{read:?} for {num}/{den}");
}

/// Codex round 6's `M`: ticks of 1/127 s, a multiple of 127, whose 101-fold
/// lies within 2^53.
const M6: i64 = 89_058_727_916_224;

#[test]
fn a_clip_the_coarsest_free_rulers_misread_is_written_in_the_finest_band() {
  // Codex round 6's case: at 12 701 fps from 10^14, `a` runs M6 ticks of
  // 1/127 s from the first, its medium stated at 12 827 = 101 · 127 fps. In
  // its plan's frames its end from the global start lies past 2^53; its
  // free rulers are 127 · m, m = 1 … 100. From each of the 64 coarsest
  // OpenTimelineIO reads that end a tick late. The finest band, the 64 just
  // below its plan's, holds rulers that read it exactly: every odd m from
  // 65 — 8 255 = 65 · 127 fps, Codex's, among them — and the search, finest
  // first, writes `a` in the first it reaches, 12 573 = 99 · 127.
  let (edit, start) = (12_701, 100_000_000_000_000);
  let text = to_otio(&from_a_tick(12_827, edit, M6, start), OtioTarget::V0_15Plus).unwrap();
  assert_eq!(validate_json(&text, OtioTarget::V0_15Plus), Ok(()));
  let ruler = |value: i64| Read {
    value: value as f64,
    rate: 12_573.0,
  };
  let kids = kids(&text, 0);
  let Kid::Item {
    start: from,
    duration,
  } = kids[0]
  else {
    panic!("{kids:?}");
  };
  assert_eq!((from, duration), (ruler(99), ruler(99 * M6)));
  // Read back: its end from the global start by `operator+`, exactly; the
  // end of a range of the track's duration from it, carried in 12 573ths,
  // and that range's last tick, each to the nearest tick.
  let root = json::parse(&text).unwrap();
  let global = read(&member(&root, "global_start_time"));
  let exact = M6 / 127 * i64::from(edit) + start;
  assert_eq!(
    global.plus(place(&kids, 0).1),
    Read {
      value: exact as f64,
      rate: f64::from(edit)
    }
  );
  let track = track_duration(&kids);
  let (num, den) = (i128::from(exact) * 12_573, i128::from(edit));
  within_half_a_tick(Read::end(global, track), num, den);
  within_half_a_tick(Read::end_inclusive(global, track), num - den, den);
  // In OpenTimelineIO's arithmetic: in the plan's frames, past 2^53; from
  // each of the 64 coarsest rulers a tick late; from the finest, 12 700, a
  // tick late too; from 12 573 and from 8 255, exactly.
  let plan = global.plus(Read {
    value: (101 * M6) as f64,
    rate: 12_827.0,
  });
  assert_eq!(plan.value, 9_095_923_567_408_870.0);
  for m in 1..=64 {
    assert_eq!(end_from(global, M6, m), (exact + 1) as f64, "127 · {m}");
  }
  assert_eq!(end_from(global, M6, 100), (exact + 1) as f64);
  assert_eq!(end_from(global, M6, 99), exact as f64);
  assert_eq!(end_from(global, M6, 65), exact as f64);
  assert_eq!(exact, 9_006_574_041_448_512);
}

/// How long the clip between the bands runs: ticks of 1/127 s, a multiple
/// of 127, whose end from the global start below OpenTimelineIO reads a tick
/// short from every whole ruler 127 · m below 32 512 = 256 · 127 fps but
/// one, 16 383 = 129 · 127.
const BETWEEN: i64 = 35_183_954_771_904;

#[test]
fn a_clip_only_a_ruler_between_the_free_bands_holds_is_refused_by_the_bounded_search() {
  // At 32 511 fps from 2 · 10^11, `a` runs BETWEEN ticks of 1/127 s from
  // the first, its medium stated at 32 512 = 256 · 127 fps: in its plan's
  // frames its end from the global start lies past 2^53. Its free rulers are
  // 127 · m, m = 1 … 255 — the finest band 192 … 255, the coarsest 1 … 64 —
  // and from every one of them OpenTimelineIO reads that end a tick short.
  // Between the bands, 16 383 = 129 · 127 fps reads it exactly, and the walk
  // holds every value there: the timeline is representable, and the
  // contract refuses it, in the last of 1 + 64 + 64 plans, saying so.
  let (edit, start) = (32_511, 200_000_000_000);
  let exact = BETWEEN / 127 * i64::from(edit) + start;
  let refusal = match to_otio(
    &from_a_tick(32_512, edit, BETWEEN, start),
    OtioTarget::V0_15Plus,
  ) {
    Err(Refused::NotRepresentable(refusal)) => refusal,
    other => panic!("{other:?}"),
  };
  assert_eq!(
    (refusal.at(), refusal.value(), refusal.rate()),
    (
      Spot::Absolute(ChildAt::new(0, 0)),
      i128::from(exact),
      Rate::hz(edit)
    )
  );
  let searched = refusal.searched().unwrap();
  assert_eq!(searched.walks(), 1 + 64 + 64);
  assert_eq!(
    searched.bands(),
    [
      RulerBand::Operands,
      RulerBand::Finest(64),
      RulerBand::Coarsest(64)
    ]
  );
  assert!(
    refusal
      .to_string()
      .contains(", in the last of 129 plans the bounded search walked, "),
    "{refusal}"
  );
  // In OpenTimelineIO's arithmetic: in the plan's frames, past 2^53; from
  // every ruler of both bands, a tick short; between them, exactly from
  // 127 · 129 and from no other.
  let global = Read {
    value: start as f64,
    rate: f64::from(edit),
  };
  let plan = global.plus(Read {
    value: (256 * BETWEEN) as f64,
    rate: 32_512.0,
  });
  assert_eq!(plan.value, 9_007_292_427_759_188.0);
  for m in (1..=64).chain(192..=255) {
    assert_eq!(
      end_from(global, BETWEEN, m),
      (exact - 1) as f64,
      "127 · {m}"
    );
  }
  for m in 65..=191 {
    assert_eq!(
      end_from(global, BETWEEN, m) == exact as f64,
      m == 129,
      "127 · {m}"
    );
  }
  // Written in 127 · 129 fps, the walk the export makes holds the track.
  let ruler = derive::Ruler::new(Rate::hz(127 * 129));
  let a = derive::Child::Item {
    start: derive::Time::written(129, ruler),
    duration: derive::Time::written(129 * i128::from(BETWEEN), ruler),
  };
  let global = derive::Time::written(i128::from(start), derive::Ruler::new(Rate::hz(edit)));
  let duration = derive::track(0, &[a], global).unwrap();
  assert_eq!(derive::stack(&[duration], global), Ok(()));
  assert_eq!(exact, 9_007_015_382_593_472);
}

#[test]
fn a_ruler_the_timeline_counts_in_is_tried_before_a_free_one() {
  // At 127 fps from 10^12, `a` runs `ticks` ticks of 1/127 s from zero, its
  // medium stated at 12 827 = 101 · 127 fps: in its plan's frames its end
  // from the global start lies past 2^53. The edit rate holds it, a ruler
  // the timeline's operands are counted in, and so do the free 127 · m,
  // m = 2 … 100 — the finest, 12 700, reads that end exactly too. A clip's
  // list runs the operands' rulers before the free ones: `a` is written in
  // the edit rate, as the global start and the gaps are.
  let (ticks, start): (i64, i64) = (89_000_000_000_001, 1_000_000_000_000);
  let edit = tb(1, 127);
  let mut a = placed(
    "a",
    TimeRange::new(0, ticks, edit),
    TimeRange::new(0, ticks, edit),
  );
  a.media_mut().set_rate(Some(Rate::hz(12_827)));
  let timeline = Timeline::new("t", Rate::hz(127))
    .with_start(Timestamp::new(start, edit))
    .with_track(video([a]));
  let text = to_otio(&timeline, OtioTarget::V0_15Plus).unwrap();
  assert_eq!(validate_json(&text, OtioTarget::V0_15Plus), Ok(()));
  let at = |value: i64, rate: f64| Read {
    value: value as f64,
    rate,
  };
  let kids = kids(&text, 0);
  let Kid::Item {
    start: from,
    duration,
  } = kids[0]
  else {
    panic!("{kids:?}");
  };
  assert_eq!((from, duration), (at(0, 127.0), at(ticks, 127.0)));
  let global = read(&member(&json::parse(&text).unwrap(), "global_start_time"));
  assert_eq!(global.plus(place(&kids, 0).1), at(start + ticks, 127.0));
  // In OpenTimelineIO's arithmetic: in the plan's frames, past 2^53; in
  // 12 700ths, exactly — the order, not the arithmetic, chose the edit rate.
  assert_eq!(
    global.plus(at(101 * ticks, 12_827.0)).value,
    9_090_000_000_000_100.0
  );
  assert_eq!(
    global.plus(at(100 * ticks, 12_700.0)),
    at(100 * (start + ticks), 12_700.0)
  );
}

/// How long the clip whose two free bands are one runs: ticks of 1/127 s, a
/// multiple of 127, whose end from the global start below OpenTimelineIO
/// reads a tick short from every whole ruler 127 · m below 8 255 = 65 · 127
/// fps.
const BOTH: i64 = 138_571_922_804_800;

#[test]
fn a_ruler_in_both_free_bands_is_tried_once() {
  // At 8 129 fps from 10^14, `a` runs BOTH ticks of 1/127 s from the first,
  // its medium stated at 8 255 = 65 · 127 fps: in its plan's frames its end
  // from the global start lies past 2^53. Its free rulers are 127 · m,
  // m = 1 … 64 — the 64 finest and the 64 coarsest alike — and from each
  // OpenTimelineIO reads that end a tick short. Each is tried once: the
  // refusal comes in the last of 1 + 64 plans.
  let (edit, start) = (8_129, 100_000_000_000_000);
  let exact = BOTH / 127 * i64::from(edit) + start;
  let refusal = match to_otio(
    &from_a_tick(8_255, edit, BOTH, start),
    OtioTarget::V0_15Plus,
  ) {
    Err(Refused::NotRepresentable(refusal)) => refusal,
    other => panic!("{other:?}"),
  };
  assert_eq!(
    (refusal.at(), refusal.value(), refusal.rate()),
    (
      Spot::Absolute(ChildAt::new(0, 0)),
      i128::from(exact),
      Rate::hz(edit)
    )
  );
  assert_eq!(
    refusal.searched().map(|searched| searched.walks()),
    Some(1 + 64)
  );
  // In OpenTimelineIO's arithmetic: in the plan's frames, past 2^53; from
  // every one of the 64 rulers, a tick short.
  let global = Read {
    value: start as f64,
    rate: f64::from(edit),
  };
  let plan = global.plus(Read {
    value: (65 * BOTH) as f64,
    rate: 8_255.0,
  });
  assert_eq!(plan.value, 9_108_724_988_462_818.0);
  for m in 1..=64 {
    assert_eq!(end_from(global, BOTH, m), (exact - 1) as f64, "127 · {m}");
  }
  assert_eq!(exact, 8_969_694_177_009_600);
}

/// How long the clip only its coarse rulers hold runs: ticks of 1/127 s, a
/// multiple of 127, whose end from the global start below OpenTimelineIO
/// reads a tick off from every ruler of the finest band below 32 512 =
/// 256 · 127 fps, and exactly from 127 · 2^k, k = 0 … 6.
const COARSE: i64 = 35_184_364_667_071;

#[test]
fn a_clip_only_its_coarsest_free_rulers_hold_is_written_in_one() {
  // At 32 511 fps from 2 · 10^11, `a` runs COARSE ticks of 1/127 s from the
  // first, its medium stated at 32 512 = 256 · 127 fps: in its plan's frames
  // its end from the global start lies past 2^53. From every ruler of the
  // finest band, 127 · m for m = 192 … 255, OpenTimelineIO reads that end a
  // tick off; of the coarsest band, 127 · 2^k read it exactly. The search
  // reaches the coarsest band after the finest, and runs it finest first:
  // `a` is written in 8 128 = 64 · 127 fps.
  let (edit, start) = (32_511, 200_000_000_000);
  let text = to_otio(
    &from_a_tick(32_512, edit, COARSE, start),
    OtioTarget::V0_15Plus,
  )
  .unwrap();
  assert_eq!(validate_json(&text, OtioTarget::V0_15Plus), Ok(()));
  let ruler = |value: i64| Read {
    value: value as f64,
    rate: 8_128.0,
  };
  let kids = kids(&text, 0);
  let Kid::Item {
    start: from,
    duration,
  } = kids[0]
  else {
    panic!("{kids:?}");
  };
  assert_eq!((from, duration), (ruler(64), ruler(64 * COARSE)));
  let global = read(&member(&json::parse(&text).unwrap(), "global_start_time"));
  let exact = COARSE / 127 * i64::from(edit) + start;
  assert_eq!(
    global.plus(place(&kids, 0).1),
    Read {
      value: exact as f64,
      rate: f64::from(edit)
    }
  );
  // In OpenTimelineIO's arithmetic: in the plan's frames, past 2^53; from
  // every ruler of the finest band, a tick off; from 127 · 64, exactly.
  let plan = global.plus(Read {
    value: (256 * COARSE) as f64,
    rate: 32_512.0,
  });
  assert_eq!(plan.value, 9_007_397_360_921_940.0);
  for m in 192..=255 {
    assert_eq!(
      (end_from(global, COARSE, m) - exact as f64).abs(),
      1.0,
      "127 · {m}"
    );
  }
  assert_eq!(end_from(global, COARSE, 64), exact as f64);
  assert_eq!(exact, 9_007_120_312_528_703);
}

/// Codex round 7's first case: at 2 753 fps from `start`, a gap of 942 068
/// frames, then `a`, 6 759 seconds of a medium counted in ticks of 1/1 582 s
/// from zero — 10 692 738 of them, 18 607 527 frames — so planned in those
/// ticks.
fn after_a_gap_from(start: i64) -> Timeline {
  let edit = tb(1, 2753);
  let a = placed(
    "a",
    TimeRange::new(0, 10_692_738, tb(1, 1582)),
    TimeRange::new(942_068, 942_068 + 18_607_527, edit),
  );
  Timeline::new("t", Rate::hz(2753))
    .with_start(Timestamp::new(start, edit))
    .with_track(video([a]))
}

/// `after_a_gap_from`'s `a` ends, exactly, `(start + 942 068) · 1 582 +
/// 10 692 738 · 2 753` 2 753ths of a tick of 1/1 582 s from zero.
fn after_a_gap_ends(start: i64) -> i128 {
  i128::from(start + 942_068) * 1582 + 10_692_738 * 2753
}

#[test]
fn a_childs_range_from_the_global_start_opentimelineio_ends_more_than_half_a_tick_off_is_written_in_the_edit_rate()
 {
  // Codex round 7's case. From Codex's own start, -9 007 199 254 444 572,
  // which OpenTimelineIO 0.17's reader misreads written `<digits>.0`, the
  // export writes the start as its integer, which that reader reads exactly,
  // and the timeline reads back. Planned in 1 582ths, `a`'s place from the
  // global start by `operator+` is exact at both ends. But OpenTimelineIO
  // moves a child's range with its own duration kept (`composition.cpp`
  // 357–359), and ends it at that duration plus the moved start rescaled to
  // 1 582ths (`timeRange.h` 105–108): a product past 2^63, rounded, then
  // divided — 1 907/2 753 of a tick off. The edit rate, a ruler the timeline
  // counts in, holds `a`: written in it, nothing is rescaled, and the range
  // reads back exactly. 66 frames earlier, below, as from Codex's.
  let codex = -9_007_199_254_444_572;
  let text = to_otio(&after_a_gap_from(codex), OtioTarget::V0_15Plus).unwrap();
  assert!(text.contains("\"value\": -9007199254444572\n"), "{text}");
  assert_eq!(validate_json(&text, OtioTarget::V0_15Plus), Ok(()));
  assert_eq!(from_otio(&text), Ok(after_a_gap_from(codex)));
  let start = -9_007_199_254_444_638;
  let text = to_otio(&after_a_gap_from(start), OtioTarget::V0_15Plus).unwrap();
  assert_eq!(validate_json(&text, OtioTarget::V0_15Plus), Ok(()));
  let frames = |value: i64| Read {
    value: value as f64,
    rate: 2753.0,
  };
  let kids = kids(&text, 0);
  let Kid::Item {
    start: from,
    duration,
  } = kids[1]
  else {
    panic!("{kids:?}");
  };
  assert_eq!((from, duration), (frames(0), frames(18_607_527)));
  let global = read(&member(&json::parse(&text).unwrap(), "global_start_time"));
  let moved = global.plus(place(&kids, 1).0);
  let end = start + 942_068 + 18_607_527;
  assert_eq!(moved, frames(start + 942_068));
  assert_eq!(Read::end(moved, duration), frames(end));
  assert_eq!(Read::end_inclusive(moved, duration), frames(end - 1));
  // In OpenTimelineIO's arithmetic, planned in 1 582ths: the global start
  // added to either end of the place, exact; the range from the moved start
  // ending at -5 175 949 578 497 623 ticks, where exactly it ends at
  // -5 175 949 578 497 624 + 846/2 753, and its last tick a tick before, as
  // far off.
  let ticks = Read {
    value: 10_692_738.0,
    rate: 1582.0,
  };
  let place = Read {
    value: 0.0,
    rate: 1582.0,
  }
  .plus(frames(942_068));
  assert_eq!(global.plus(place), frames(start + 942_068));
  assert_eq!(global.plus(Read::end(place, ticks)), frames(end));
  let moved = global.plus(place);
  let exact = after_a_gap_ends(start);
  assert_eq!(exact, -14_249_389_189_603_958_026);
  assert_eq!(
    Read::end(moved, ticks),
    Read {
      value: -5_175_949_578_497_623.0,
      rate: 1582.0
    }
  );
  assert_eq!(Read::end(moved, ticks).value as i128 * 2753 - exact, 1907);
  assert_eq!(
    Read::end_inclusive(moved, ticks).value as i128 * 2753 - (exact - 2753),
    1907
  );
}

#[test]
fn a_childs_range_from_the_global_start_a_frame_later_is_written_in_its_own_ruler() {
  // A frame later the moved start rescales to the same double, and the
  // exact end moves on 1 582/2 753 of a tick: OpenTimelineIO ends the range
  // 325/2 753 of a tick from it, inside half a tick. `a` keeps its own
  // 1 582ths, and the range reads back to the nearest tick exactly.
  let start = -9_007_199_254_444_637;
  let text = to_otio(&after_a_gap_from(start), OtioTarget::V0_15Plus).unwrap();
  assert_eq!(validate_json(&text, OtioTarget::V0_15Plus), Ok(()));
  let ticks = |value: i64| Read {
    value: value as f64,
    rate: 1582.0,
  };
  let kids = kids(&text, 0);
  let Kid::Item {
    start: from,
    duration,
  } = kids[1]
  else {
    panic!("{kids:?}");
  };
  assert_eq!((from, duration), (ticks(0), ticks(10_692_738)));
  let global = read(&member(&json::parse(&text).unwrap(), "global_start_time"));
  let moved = global.plus(place(&kids, 1).0);
  let exact = after_a_gap_ends(start);
  within_half_a_tick(Read::end(moved, duration), exact, 2753);
  within_half_a_tick(Read::end_inclusive(moved, duration), exact - 2753, 2753);
  assert_eq!(Read::end(moved, duration).value as i128 * 2753 - exact, 325);
}

/// One frame every two seconds: the edit rate of Codex round 7's second and
/// third cases.
fn every_two_seconds() -> Rate {
  Rate::fps(1, NonZeroI32::new(2).unwrap())
}

/// At one frame every two seconds, `a` plays `seconds` from `from` of a
/// medium counted in whole seconds, from the timeline's zero.
fn seconds_from(from: i64, seconds: i64) -> Timeline {
  Timeline::new("t", every_two_seconds()).with_track(video([placed(
    "a",
    TimeRange::new(from, from + seconds, second()),
    TimeRange::new(0, seconds / 2, tb(2, 1)),
  )]))
}

#[test]
fn a_source_range_its_own_ruler_cannot_end_is_written_in_the_edit_rate() {
  // Codex round 7's second case: `a` plays [2^53, 2^53 + 2) seconds. In
  // seconds, its own ruler and its coarsest, its start and its length are
  // written within 2^53, but the end OpenTimelineIO derives from them
  // (`timeRange.h` 105–108) is 2^53 + 2, past it. The plan leaves that end
  // to the walk, which refuses it, and the search writes `a` in the edit
  // rate's frames — from 2^52, one long — whose end it holds: [2^53,
  // 2^53 + 2) seconds, read back exactly.
  let text = to_otio(&seconds_from(TWO_53, 2), OtioTarget::V0_15Plus).unwrap();
  assert_eq!(validate_json(&text, OtioTarget::V0_15Plus), Ok(()));
  let frames = |value: i64| Read {
    value: value as f64,
    rate: 0.5,
  };
  let kids = kids(&text, 0);
  let Kid::Item { start, duration } = kids[0] else {
    panic!("{kids:?}");
  };
  assert_eq!((start, duration), (frames(TWO_53 / 2), frames(1)));
  let end = Read::end(start, duration);
  assert_eq!(end, frames(TWO_53 / 2 + 1));
  assert_eq!(
    (start.at(1.0), end.at(1.0)),
    (TWO_53 as f64, (TWO_53 + 2) as f64)
  );
}

#[test]
fn a_source_range_no_ruler_ends_is_refused_after_the_search() {
  // [2^53 - 1, 2^53 + 3) seconds: in seconds it ends at 2^53 + 3, and no
  // other ruler holds it — the edit rate's frames do not land on its odd
  // start, and no whole rate is coarser than a second. A count
  // OpenTimelineIO derives: refused after the search, which says so.
  let refusal = match to_otio(&seconds_from(TWO_53 - 1, 4), OtioTarget::V0_15Plus) {
    Err(Refused::NotRepresentable(refusal)) => refusal,
    other => panic!("{other:?}"),
  };
  assert_eq!(
    (refusal.at(), refusal.value(), refusal.rate()),
    (
      Spot::Source(ClipAt::new(0, 0)),
      i128::from(TWO_53) + 3,
      Rate::hz(1)
    )
  );
  assert_eq!(refusal.searched().map(|searched| searched.walks()), Some(1));
  assert!(
    refusal
      .to_string()
      .contains(", in the one plan the bounded search walked, "),
    "{refusal}"
  );
}

#[test]
fn a_source_range_its_own_ruler_cannot_end_moves_its_own_clip_alone() {
  // At one frame every two seconds: `a`, two seconds counted in
  // milliseconds, then `b`, Codex's [2^53, 2^53 + 2) seconds. The walk
  // refuses `b`'s end in seconds; `a`'s ruler, finer, plays no part in it,
  // so `a` keeps its own milliseconds while `b` is written in the edit
  // rate's frames.
  let edit = tb(2, 1);
  let timeline = Timeline::new("t", every_two_seconds()).with_track(video([
    placed(
      "a",
      TimeRange::new(0, 2000, Timebase::MILLIS),
      TimeRange::new(0, 1, edit),
    ),
    placed(
      "b",
      TimeRange::new(TWO_53, TWO_53 + 2, second()),
      TimeRange::new(1, 2, edit),
    ),
  ]));
  let text = to_otio(&timeline, OtioTarget::V0_15Plus).unwrap();
  assert_eq!(validate_json(&text, OtioTarget::V0_15Plus), Ok(()));
  let at = |value: i64, rate: f64| Read {
    value: value as f64,
    rate,
  };
  let written: Vec<(Read, Read)> = kids(&text, 0)
    .iter()
    .map(|kid| match *kid {
      Kid::Item { start, duration } => (start, duration),
      Kid::Transition { .. } => panic!("{kid:?}"),
    })
    .collect();
  assert_eq!(
    written,
    [
      (at(0, 1000.0), at(2000, 1000.0)),
      (at(TWO_53 / 2, 0.5), at(1, 0.5))
    ]
  );
}

/// A timeline with no track at one frame every two seconds, from `frames`.
fn no_track_from(frames: i64) -> Timeline {
  Timeline::new("t", every_two_seconds()).with_start(Timestamp::new(frames, tb(2, 1)))
}

#[test]
fn a_timeline_with_no_track_whose_end_from_the_global_start_is_past_2_53_is_refused() {
  // Codex round 7's third case: from 2^53 frames, no track. OpenTimelineIO
  // gives the empty stack the range `TimeRange()` (`stack.cpp` 121–123),
  // no duration at rate 1, and the timeline its duration (`timeline.h`
  // 63–66): the timeline's range from the global start ends at that start
  // rescaled to rate 1, 2^54 seconds.
  let refusal = match to_otio(&no_track_from(TWO_53), OtioTarget::V0_15Plus) {
    Err(Refused::NotRepresentable(refusal)) => refusal,
    other => panic!("{other:?}"),
  };
  assert_eq!(
    (refusal.at(), refusal.value(), refusal.rate()),
    (Spot::TimelineEnd, 1 << 54, Rate::hz(1))
  );
  assert_eq!(refusal.searched().map(|searched| searched.walks()), Some(1));
  let global = Read {
    value: TWO_53 as f64,
    rate: 0.5,
  };
  let none = Read {
    value: 0.0,
    rate: 1.0,
  };
  assert_eq!(
    Read::end(global, none),
    Read {
      value: (1_i64 << 54) as f64,
      rate: 1.0
    }
  );
}

#[test]
fn a_timeline_with_no_track_whose_end_from_the_global_start_is_held_is_written() {
  // From 2^52 frames the range ends at 2^53 seconds: written, and read back
  // exactly. A frame later it ends at 2^53 + 2: refused.
  let text = to_otio(&no_track_from(TWO_53 / 2), OtioTarget::V0_15Plus).unwrap();
  assert_eq!(validate_json(&text, OtioTarget::V0_15Plus), Ok(()));
  let global = read(&member(&json::parse(&text).unwrap(), "global_start_time"));
  assert_eq!(
    global,
    Read {
      value: (TWO_53 / 2) as f64,
      rate: 0.5
    }
  );
  let none = Read {
    value: 0.0,
    rate: 1.0,
  };
  assert_eq!(
    Read::end(global, none),
    Read {
      value: TWO_53 as f64,
      rate: 1.0
    }
  );
  assert_eq!(
    refused(&no_track_from(TWO_53 / 2 + 1)),
    (Spot::TimelineEnd, i128::from(TWO_53) + 2, Rate::hz(1))
  );
}

#[test]
fn a_whole_rate_that_cannot_end_a_source_range_is_never_tried() {
  // At 29.97 fps, `a` plays `b` frames from frame `a` of a medium counted
  // in them, `a` = 1 500 · 4 499 100 526 843 and `b` = 1 500 ·
  // 4 499 100 526 847: each within 2^53, their sum — the end
  // OpenTimelineIO derives — past it. The coarsest whole rate that lands
  // on the range, 20 a second, counts it in 1 001/1 500 of the frames: the
  // start and the length within 2^53, the end 2 698 past. Every whole rate
  // that lands on it is a multiple of 20 and ends it further on, so none is
  // among the clip's rulers: refused in its own frames, in the one plan the
  // search walked.
  let (k1, k2) = (4_499_100_526_843_i64, 4_499_100_526_847_i64);
  let (a, b) = (1500 * k1, 1500 * k2);
  let frames = tb(1001, 30_000);
  let timeline = Timeline::new("t", Rate::FPS_29_97).with_track(video([placed(
    "a",
    TimeRange::new(a, a + b, frames),
    TimeRange::new(0, b, frames),
  )]));
  let refusal = match to_otio(&timeline, OtioTarget::V0_15Plus) {
    Err(Refused::NotRepresentable(refusal)) => refusal,
    other => panic!("{other:?}"),
  };
  assert_eq!(
    (refusal.at(), refusal.value(), refusal.rate()),
    (
      Spot::Source(ClipAt::new(0, 0)),
      i128::from(a) + i128::from(b),
      Rate::FPS_29_97
    )
  );
  assert_eq!(refusal.searched().map(|searched| searched.walks()), Some(1));
  let twentieths = |frames: i64| i128::from(frames) * 1001 / 1500;
  assert!(twentieths(a) <= i128::from(TWO_53) && twentieths(b) <= i128::from(TWO_53));
  assert_eq!(twentieths(a) + twentieths(b) - i128::from(TWO_53), 2698);
}

/// Track 0's items as `to_otio` writes `timeline`: each clip's source range,
/// its start and its duration read.
fn written_items(timeline: &Timeline) -> Vec<(Read, Read)> {
  let text = to_otio(timeline, OtioTarget::V0_15Plus).unwrap();
  assert_eq!(validate_json(&text, OtioTarget::V0_15Plus), Ok(()));
  kids(&text, 0)
    .iter()
    .map(|kid| match *kid {
      Kid::Item { start, duration } => (start, duration),
      Kid::Transition { .. } => panic!("{kid:?}"),
    })
    .collect()
}

/// Why `to_otio` refuses `timeline`, whole.
fn refusal(timeline: &Timeline) -> NotRepresentable {
  match to_otio(timeline, OtioTarget::V0_15Plus) {
    Err(Refused::NotRepresentable(refusal)) => refusal,
    other => panic!("{other:?}"),
  }
}

#[test]
fn a_source_range_none_of_its_own_rulers_writes_is_written_in_the_edit_rate() {
  // At one frame every two seconds, `a` plays [2^53 + 2, 2^53 + 4) seconds.
  // Its own rulers — seconds, its timebase's and its coarsest whole one —
  // count its start past 2^53, but the edit rate, a ruler the timeline's
  // operands are counted in, writes it: from 2^52 + 1, one long, ending at
  // 2^52 + 2. Planned there, it reads back as [2^53 + 2, 2^53 + 4) seconds
  // exactly.
  let frames = |value: i64| Read {
    value: value as f64,
    rate: 0.5,
  };
  let [(start, duration)] = written_items(&seconds_from(TWO_53 + 2, 2))[..] else {
    panic!("one item");
  };
  assert_eq!((start, duration), (frames(TWO_53 / 2 + 1), frames(1)));
  let end = Read::end(start, duration);
  assert_eq!(end, frames(TWO_53 / 2 + 2));
  assert_eq!(Read::end_inclusive(start, duration), start);
  assert_eq!(
    (start.at(1.0), end.at(1.0)),
    ((TWO_53 + 2) as f64, (TWO_53 + 4) as f64)
  );
}

#[test]
fn a_source_range_from_before_minus_2_53_none_of_its_own_rulers_writes_is_written_in_the_edit_rate()
{
  // `a` plays [-2^53 - 2, -2^53 + 2) seconds of a medium whose clock starts
  // before zero, at one frame every two seconds. Its own rulers count its
  // start past -2^53; the edit rate writes it, from -2^52 - 1, two long. The
  // walk derives only the end from a start, here within ±2^53 in seconds
  // too, so it is the plan that keeps the start written within ±2^53.
  let frames = |value: i64| Read {
    value: value as f64,
    rate: 0.5,
  };
  let [(start, duration)] = written_items(&seconds_from(-TWO_53 - 2, 4))[..] else {
    panic!("one item");
  };
  assert_eq!((start, duration), (frames(-TWO_53 / 2 - 1), frames(2)));
  let end = Read::end(start, duration);
  assert_eq!(end, frames(-TWO_53 / 2 + 1));
  assert_eq!(Read::end_inclusive(start, duration), frames(-TWO_53 / 2));
  assert_eq!(
    (start.at(1.0), end.at(1.0)),
    ((-TWO_53 - 2) as f64, (-TWO_53 + 2) as f64)
  );
}

#[test]
fn a_source_range_no_ruler_of_the_bands_writes_is_refused_before_any_walk() {
  // At one frame a second, [2^53 + 2, 2^53 + 4) seconds: its own rulers
  // count its start past 2^53, and so does the one ruler the timeline's
  // operands are counted in, seconds. Every whole rate that lands on it is
  // a multiple of a second, so no free ruler writes it either: no ruler of
  // the bands does, and there is no plan to walk. Refused as the timeline
  // holds it, with the bands the search tried and no walk.
  let at_one_fps = Timeline::new("t", Rate::hz(1)).with_track(video([placed(
    "a",
    TimeRange::new(TWO_53 + 2, TWO_53 + 4, second()),
    TimeRange::new(0, 2, second()),
  )]));
  let refused = refusal(&at_one_fps);
  assert_eq!(
    (refused.at(), refused.value(), refused.rate()),
    (
      Spot::Source(ClipAt::new(0, 0)),
      i128::from(TWO_53) + 2,
      Rate::hz(1)
    )
  );
  let searched = refused.searched().unwrap();
  assert_eq!(searched.walks(), 0);
  assert_eq!(
    searched.bands(),
    [
      RulerBand::Operands,
      RulerBand::Finest(64),
      RulerBand::Coarsest(64)
    ]
  );
  assert!(
    refused.to_string().ends_with(
      ", in any of the range's own rulers or of the bounded search's bands, so the search \
       walked no plan"
    ),
    "{refused}"
  );
  // At one frame every two seconds, [2^53 + 3, 2^53 + 5) seconds: the edit
  // rate's frames do not land on its odd start. Refused the same way.
  let refused = refusal(&seconds_from(TWO_53 + 3, 2));
  assert_eq!(
    (refused.at(), refused.value(), refused.rate()),
    (
      Spot::Source(ClipAt::new(0, 0)),
      i128::from(TWO_53) + 3,
      Rate::hz(1)
    )
  );
  assert_eq!(refused.searched().map(|searched| searched.walks()), Some(0));
}

#[test]
fn a_source_range_an_operand_ruler_writes_but_cannot_end_moves_its_own_clip_on() {
  // At one frame every two seconds: `m`, two seconds counted in
  // milliseconds; `q`, one tick of four seconds; then `a`, [2^54 - 4,
  // 2^54 + 4) seconds. None of `a`'s own rulers writes it. Of the
  // timeline's operands, finest first — 1 000, 1, the edit rate's 1/2,
  // `q`'s 1/4 — the first that writes it is the edit rate: from 2^53 - 2,
  // four long, ending at 2^53 + 2, past the bound. The walk refuses that end,
  // and the search moves `a` alone — `m`, finer, plays no part in it — to
  // the next ruler of its list, `q`'s, which ends it at 2^52 + 1.
  let edit = tb(2, 1);
  let timeline = Timeline::new("t", every_two_seconds()).with_track(video([
    placed(
      "m",
      TimeRange::new(0, 2000, Timebase::MILLIS),
      TimeRange::new(0, 1, edit),
    ),
    placed(
      "q",
      TimeRange::new(0, 1, tb(4, 1)),
      TimeRange::new(1, 3, edit),
    ),
    placed(
      "a",
      TimeRange::new(2 * TWO_53 - 4, 2 * TWO_53 + 4, second()),
      TimeRange::new(3, 7, edit),
    ),
  ]));
  let at = |value: i64, rate: f64| Read {
    value: value as f64,
    rate,
  };
  let written = written_items(&timeline);
  assert_eq!(
    written,
    [
      (at(0, 1000.0), at(2000, 1000.0)),
      (at(0, 0.25), at(1, 0.25)),
      (at(TWO_53 / 2 - 1, 0.25), at(2, 0.25)),
    ]
  );
  let (start, duration) = written[2];
  let end = Read::end(start, duration);
  assert_eq!(end, at(TWO_53 / 2 + 1, 0.25));
  assert_eq!(
    (start.at(1.0), end.at(1.0)),
    ((2 * TWO_53 - 4) as f64, (2 * TWO_53 + 4) as f64)
  );
}

#[test]
fn a_source_range_an_operand_ruler_writes_but_none_ends_is_refused_after_the_walk() {
  // `a` alone at one frame every two seconds, [2^54 - 4, 2^54 + 4) seconds:
  // planned in the edit rate's frames, from 2^53 - 2, four long, whose end,
  // 2^53 + 2, the walk refuses. No other ruler of its list holds it —
  // seconds count its start past 2^53 — so the search refuses it there,
  // after that one walk.
  let refused = refusal(&seconds_from(2 * TWO_53 - 4, 8));
  assert_eq!(
    (refused.at(), refused.value(), refused.rate()),
    (
      Spot::Source(ClipAt::new(0, 0)),
      i128::from(TWO_53) + 2,
      every_two_seconds()
    )
  );
  assert_eq!(refused.searched().map(|searched| searched.walks()), Some(1));
  assert!(
    refused.to_string().contains(
      ", in the one plan the bounded search walked, trying each clip in its plan's ruler, "
    ),
    "{refused}"
  );
}

/// `timeline`, the medium of its first track's first clip available from
/// `from` to `to` seconds.
fn available_from(mut timeline: Timeline, from: i64, to: i64) -> Timeline {
  timeline.tracks_mut()[0].clips_mut()[0]
    .media_mut()
    .set_available_range(Some(TimeRange::new(from, to, second())));
  timeline
}

/// The available range of the medium of track 0's first child, as `text`
/// writes it for OpenTimelineIO 0.15 and later: its start and its duration,
/// read.
fn available_read(text: &str) -> (Read, Read) {
  let root = json::parse(text).unwrap();
  let tracks = member(&member(&root, "tracks"), "children");
  let track = tracks.as_array().unwrap()[0].clone();
  let clip = member(&track, "children").as_array().unwrap()[0].clone();
  let reference = member(&member(&clip, "media_references"), "DEFAULT_MEDIA");
  let range = member(&reference, "available_range");
  (
    read(&member(&range, "start_time")),
    read(&member(&range, "duration")),
  )
}

#[test]
fn an_available_range_none_of_its_own_rulers_holds_is_written_in_the_edit_rate() {
  // At one frame every two seconds, `a` plays [2^53 + 2, 2^53 + 4) seconds
  // of a medium available over [2^53 + 2, 2^53 + 6) seconds. Its own rulers
  // — seconds, its timebase's and its coarsest whole one — count the
  // available range's start past 2^53. Of the rulers the timeline's operands
  // are counted in, finest first — seconds, then the edit rate's frames —
  // the edit rate holds it whole: from 2^52 + 1, two long, ending at
  // 2^52 + 3. Written there, beside the source range in the same frames, it
  // reads back as [2^53 + 2, 2^53 + 6) seconds exactly.
  let frames = |value: i64| Read {
    value: value as f64,
    rate: 0.5,
  };
  let timeline = available_from(seconds_from(TWO_53 + 2, 2), TWO_53 + 2, TWO_53 + 6);
  let text = to_otio(&timeline, OtioTarget::V0_15Plus).unwrap();
  assert_eq!(validate_json(&text, OtioTarget::V0_15Plus), Ok(()));
  let Kid::Item { start, duration } = kids(&text, 0)[0] else {
    panic!("{text}");
  };
  assert_eq!((start, duration), (frames(TWO_53 / 2 + 1), frames(1)));
  let (start, duration) = available_read(&text);
  assert_eq!((start, duration), (frames(TWO_53 / 2 + 1), frames(2)));
  let end = Read::end(start, duration);
  assert_eq!(end, frames(TWO_53 / 2 + 3));
  assert_eq!(Read::end_inclusive(start, duration), frames(TWO_53 / 2 + 2));
  assert_eq!(
    (start.at(1.0), end.at(1.0)),
    ((TWO_53 + 2) as f64, (TWO_53 + 6) as f64)
  );
}

#[test]
fn an_available_range_its_own_rulers_cannot_end_is_written_in_the_edit_rate_apart_from_its_source()
{
  // At one frame every two seconds, `a` plays [2^53 - 2, 2^53) seconds,
  // held whole in seconds, its own ruler; its medium is available over
  // [2^53 - 2, 2^53 + 2) seconds, which seconds start within 2^53 but end
  // past it, and no whole rate is coarser than a second. The edit rate
  // holds the available range whole: from 2^52 - 1, two long, ending at
  // 2^52 + 1. Each range is planned on its own — the source range keeps its
  // seconds, the available range is written in frames — and each reads
  // back exactly.
  let at = |value: i64, rate: f64| Read {
    value: value as f64,
    rate,
  };
  let timeline = available_from(seconds_from(TWO_53 - 2, 2), TWO_53 - 2, TWO_53 + 2);
  let text = to_otio(&timeline, OtioTarget::V0_15Plus).unwrap();
  assert_eq!(validate_json(&text, OtioTarget::V0_15Plus), Ok(()));
  let Kid::Item { start, duration } = kids(&text, 0)[0] else {
    panic!("{text}");
  };
  assert_eq!((start, duration), (at(TWO_53 - 2, 1.0), at(2, 1.0)));
  assert_eq!(Read::end(start, duration), at(TWO_53, 1.0));
  let (start, duration) = available_read(&text);
  assert_eq!((start, duration), (at(TWO_53 / 2 - 1, 0.5), at(2, 0.5)));
  let end = Read::end(start, duration);
  assert_eq!(end, at(TWO_53 / 2 + 1, 0.5));
  assert_eq!(
    (start.at(1.0), end.at(1.0)),
    ((TWO_53 - 2) as f64, (TWO_53 + 2) as f64)
  );
}

#[test]
fn an_available_range_no_ruler_of_the_bands_holds_is_refused_before_any_walk() {
  // At one frame every two seconds, `a` plays [2^53 + 2, 2^53 + 4) seconds
  // of a medium available over [2^53 + 1, 2^53 + 5) seconds: seconds count
  // the available range's start past 2^53, and the edit rate's frames do
  // not land on its odd start. Every whole rate that lands on it is a
  // multiple of a second, so no ruler of the bands holds it whole. The
  // search never moves an available range: refused as the timeline holds
  // it, with the bands tried and no walk.
  let refused = refusal(&available_from(
    seconds_from(TWO_53 + 2, 2),
    TWO_53 + 1,
    TWO_53 + 5,
  ));
  assert_eq!(
    (refused.at(), refused.value(), refused.rate()),
    (
      Spot::Available(ClipAt::new(0, 0)),
      i128::from(TWO_53) + 1,
      Rate::hz(1)
    )
  );
  let searched = refused.searched().unwrap();
  assert_eq!(searched.walks(), 0);
  assert_eq!(
    searched.bands(),
    [
      RulerBand::Operands,
      RulerBand::Finest(64),
      RulerBand::Coarsest(64)
    ]
  );
  assert!(
    refused.to_string().ends_with(
      ", in any of the range's own rulers or of the bounded search's bands, so the search \
       walked no plan"
    ),
    "{refused}"
  );
  // At one frame a second, `a` plays [2^53 - 2, 2^53) seconds of a medium
  // available over [2^53 - 2, 2^53 + 2): seconds, the one ruler the
  // timeline's operands are counted in, end it past 2^53. Refused the same
  // way, at its end — which the edit rate's frames hold at one frame every
  // two seconds.
  let at_one_fps = Timeline::new("t", Rate::hz(1)).with_track(video([placed(
    "a",
    TimeRange::new(TWO_53 - 2, TWO_53, second()),
    TimeRange::new(0, 2, second()),
  )]));
  let refused = refusal(&available_from(at_one_fps, TWO_53 - 2, TWO_53 + 2));
  assert_eq!(
    (refused.at(), refused.value(), refused.rate()),
    (
      Spot::Available(ClipAt::new(0, 0)),
      i128::from(TWO_53) + 2,
      Rate::hz(1)
    )
  );
  assert_eq!(refused.searched().map(|searched| searched.walks()), Some(0));
}

#[test]
fn an_available_range_an_operand_ruler_writes_but_cannot_end_is_refused_before_any_walk() {
  // At one frame every two seconds, `a` plays [2^53 + 2, 2^53 + 4) seconds
  // of a medium available over [2^53 + 2, 2^54 + 2) seconds. Seconds count
  // its start past 2^53; the edit rate's frames write its start and its
  // length — from 2^52 + 1, 2^52 long — but end it at 2^53 + 1, past the
  // bound. No walk reads an available range, so the plan holds its end
  // with its start and its length: no ruler of the bands holds it whole,
  // and it is refused before any walk.
  let refused = refusal(&available_from(
    seconds_from(TWO_53 + 2, 2),
    TWO_53 + 2,
    2 * TWO_53 + 2,
  ));
  assert_eq!(
    (refused.at(), refused.value(), refused.rate()),
    (
      Spot::Available(ClipAt::new(0, 0)),
      i128::from(TWO_53) + 2,
      Rate::hz(1)
    )
  );
  assert_eq!(refused.searched().map(|searched| searched.walks()), Some(0));
  // Written in those frames, OpenTimelineIO would read its end a frame
  // early: 2^52 + 1 and 2^52 added in its f64 make 2^53, not 2^53 + 1.
  let frames = |value: i64| Read {
    value: value as f64,
    rate: 0.5,
  };
  assert_eq!(
    Read::end(frames(TWO_53 / 2 + 1), frames(TWO_53 / 2)),
    frames(TWO_53)
  );
}
