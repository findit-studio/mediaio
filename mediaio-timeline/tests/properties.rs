//! Seeded properties: valid timelines built at random, from fixed seeds, so
//! every run checks the same few hundred and a failure names its seed.
#![cfg(any(feature = "std", feature = "alloc"))]

mod common;

use mediaio_timeline::{
  ChangeKind, Clip, ClipId, Duration, Fade, FadeShape, Fades, Gain, Item, Marker, MarkerColor,
  MediaRef, Rate, TimeRange, Timebase, Timeline, Timestamp, Track, TrackKind, Transition, diff,
  layout,
  otio::{OtioTarget, Refused, Spot, from_otio, to_otio, validate_json},
  validate,
};
use mediatime::Rounding;
use smol_bytes::{Utf8Bytes, format_utf8bytes};

/// Seeds checked per property; fewer under Miri, which interprets slowly.
const SEEDS: u64 = if cfg!(miri) { 4 } else { 256 };

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

  /// `true` one time in `odds`.
  fn one_in(&mut self, odds: u64) -> bool {
    self.next().is_multiple_of(odds)
  }

  fn pick<T: Copy>(&mut self, items: &[T]) -> T {
    items[self.next() as usize % items.len()]
  }
}

const RATES: [Rate; 8] = [
  Rate::FPS_23_976,
  Rate::FPS_24,
  Rate::FPS_25,
  Rate::FPS_29_97,
  Rate::FPS_30,
  Rate::FPS_50,
  Rate::FPS_59_94,
  Rate::FPS_60,
];

/// The rulers media are counted in — each finer than any frame above. Not
/// every one counts every frame of every rate exactly (a frame at 23.976 fps
/// is 1839.3375 ticks at 44.1 kHz), so a clip's length is a whole number of
/// the shortest stretch of frames its ruler does count exactly.
fn media_rulers() -> [Timebase; 5] {
  [
    common::tb(1, 24_000),
    common::tb(1, 48_000),
    common::tb(1, 44_100),
    common::tb(1, 90_000),
    Timebase::MILLIS,
  ]
}

struct Planned {
  gap: u64,
  length: u64,
  /// The ruler the clip's medium is counted in.
  ruler: Timebase,
  /// The transition into this clip from the one before, `(in, out)`.
  entering: Option<(u64, u64)>,
}

/// A valid timeline, built from `seed`.
///
/// The words markers brought — markers on tracks and clips, notes, disabled
/// tracks, transitions kept out of their cuts' order — are drawn from a
/// second stream, `extra`, so the first draws every timeline before them
/// exactly as it did, and the seeds the laws below name are the same
/// timelines.
fn timeline(seed: u64) -> Timeline {
  let mut rng = Seeded(seed);
  let mut extra = Seeded(!seed);
  let rate = rng.pick(&RATES);
  let edit = rate.checked_to_timebase().unwrap();
  let frames = |n: u64| Duration::new(n, edit);
  let mut timeline = Timeline::new(format_utf8bytes!("seed {seed}"), rate)
    .with_start(Timestamp::new(rng.within(0, 200_000) as i64, edit));
  if extra.one_in(3) {
    timeline
      .metadata_mut()
      .insert("project", format_utf8bytes!("seed {seed}"));
  }
  for track_index in 0..rng.within(1, 3) {
    let kind = if rng.one_in(2) {
      TrackKind::Video
    } else {
      TrackKind::Audio
    };
    let count = rng.within(0, 6);
    let mut plan: Vec<Planned> = Vec::new();
    for index in 0..count {
      let ruler = rng.pick(&media_rulers());
      // Schema 1 has no time-warp: a source runs a whole number of frames,
      // counted exactly in its ruler. Six to ninety frames, in steps of the
      // shortest such stretch.
      let step = (1..=1000)
        .find(|&n| {
          frames(n)
            .checked_rescale_with(ruler, Rounding::Exact)
            .is_some()
        })
        .unwrap();
      let low = 6_u64.div_ceil(step);
      let length = step * rng.within(low, (90 / step).max(low));
      let gap = if rng.one_in(2) { 0 } else { rng.within(1, 40) };
      let entering = (index > 0 && gap == 0 && rng.one_in(2)).then(|| {
        let before = plan[index as usize - 1].length;
        (rng.within(0, before / 3), rng.within(0, length / 3))
      });
      plan.push(Planned {
        gap,
        length,
        ruler,
        entering,
      });
    }
    let mut track = Track::new(kind, format_utf8bytes!("T{track_index}"));
    let mut cursor = 0;
    for (index, planned) in plan.iter().enumerate() {
      let leaving = plan.get(index + 1).and_then(|next| next.entering);
      let start = cursor + planned.gap;
      cursor = start + planned.length;
      let ruler = planned.ruler;
      let ticks = |length: Duration, rounding| {
        length
          .checked_rescale_with(ruler, rounding)
          .unwrap()
          .ticks()
      };
      let source_length = ticks(frames(planned.length), Rounding::Exact);
      assert_eq!(
        Duration::new(source_length, ruler).checked_rescale_with(edit, Rounding::Exact),
        Some(frames(planned.length)),
        "seed {seed}: the source is no whole number of frames"
      );
      // Handles: as much media before and after the source range as the
      // transitions on either side play, and a little more.
      let head = planned
        .entering
        .map_or(0, |(into, _)| ticks(frames(into), Rounding::Ceil));
      let tail = leaving.map_or(0, |(_, out)| ticks(frames(out), Rounding::Ceil));
      let available_start = rng.within(0, 100) as i64;
      let source_start = available_start + head as i64 + rng.within(0, 500) as i64;
      let source_end = source_start + source_length as i64;
      let available_end = source_end + tail as i64 + rng.within(0, 500) as i64;
      // Now and then a cut back: the name and the medium of a clip placed
      // earlier on the track, under an id of its own.
      let placed = if index > 0 && rng.one_in(4) {
        rng.within(0, index as u64 - 1)
      } else {
        index as u64
      };
      let mut media = MediaRef::new(format_utf8bytes!("file:///{track_index}/{placed}"));
      if !rng.one_in(4) {
        media.set_available_range(Some(TimeRange::new(available_start, available_end, ruler)));
      }
      if rng.one_in(2) {
        media.set_rate(Some(rng.pick(&[rate, Rate::hz(48_000), Rate::FPS_25])));
      }
      if rng.one_in(3) {
        media.set_reel(Some(format_utf8bytes!("R{index}")));
      }
      let mut fades = Fades::new();
      if planned.entering.is_none() && rng.one_in(3) {
        fades.set_in(Some(Fade::new(
          frames(rng.within(1, planned.length / 3)),
          FadeShape::Linear,
        )));
      }
      if leaving.is_none() && rng.one_in(3) {
        fades.set_out(Some(Fade::new(
          frames(rng.within(1, planned.length / 3)),
          FadeShape::EqualPower,
        )));
      }
      let gain = rng
        .one_in(3)
        .then(|| Gain::from_db(-(rng.within(0, 240) as f32) / 10.0).unwrap());
      let mut clip = Clip::new(
        ClipId::new(format_utf8bytes!("{track_index}.{index}")),
        format_utf8bytes!("c{placed}"),
        media,
        TimeRange::new(source_start, source_end, ruler),
        TimeRange::new(start as i64, cursor as i64, edit),
      )
      .with_gain(gain)
      .with_fades(fades)
      .with_enabled(!rng.one_in(8));
      // Markers in the medium's own ticks, anywhere about the source range.
      for marker in 0..extra.within(0, 2) {
        let at = source_start + extra.within(0, source_length) as i64;
        let length = extra.within(0, source_length / 2) as i64;
        clip.markers_mut().push(
          Marker::new(
            format_utf8bytes!("m{marker}"),
            TimeRange::new(at, at + length, ruler),
          )
          .with_color(extra.pick(&MarkerColor::ALL)),
        );
      }
      if extra.one_in(4) {
        clip
          .metadata_mut()
          .insert("take", format_utf8bytes!("{}", extra.within(1, 9)));
      }
      track.clips_mut().push(clip);
      if let Some((into, out)) = planned.entering {
        track.transitions_mut().push(Transition::dissolve(
          Timestamp::new(start as i64, edit),
          frames(into),
          frames(out),
        ));
      }
    }
    // A track's own markers, at the edit rate from zero, now and then past
    // its last record; a disabled track; transitions kept the other way
    // round, which the export says by each dissolve's place.
    for marker in 0..extra.within(0, 2) {
      let at = extra.within(0, cursor + 48) as i64;
      // Now and then a point: a marker of no length.
      let length = if extra.one_in(4) {
        0
      } else {
        extra.within(1, 48) as i64
      };
      track.markers_mut().push(
        Marker::new(
          format_utf8bytes!("t{marker}"),
          TimeRange::new(at, at + length, edit),
        )
        .with_color(extra.pick(&MarkerColor::ALL))
        .with_comment(if extra.one_in(2) { "a note" } else { "" }),
      );
    }
    if extra.one_in(6) {
      track.set_enabled(false);
    }
    if track.transitions().len() > 1 && extra.one_in(2) {
      track.transitions_mut().reverse();
    }
    timeline.tracks_mut().push(track);
  }
  timeline
}

#[test]
fn validate_accepts_what_the_generator_builds() {
  for seed in 0..SEEDS {
    let timeline = timeline(seed);
    assert_eq!(validate(&timeline), Ok(()), "seed {seed}");
  }
}

#[test]
fn layout_tiles_each_track_from_zero_and_its_gaps_never_overlap() {
  for seed in 0..SEEDS {
    let timeline = timeline(seed);
    let laid = layout(&timeline).unwrap();
    for (track, laid) in timeline.tracks().iter().zip(laid.tracks()) {
      let mut covered_to = Timestamp::new(0, timeline.edit_timebase().unwrap());
      let mut clips = 0;
      for item in laid.items() {
        let record = item.record();
        // Each item starts where the one before it ends: nothing overlaps,
        // and no time is left out.
        assert_eq!(record.start(), covered_to, "seed {seed}");
        assert!(record.start() < record.end(), "seed {seed}: an empty item");
        if let Item::Clip(clip) = item {
          assert!(core::ptr::eq(*clip, &track.clips()[clips]), "seed {seed}");
          clips += 1;
        }
        covered_to = record.end();
      }
      assert_eq!(clips, track.clips().len(), "seed {seed}");
    }
  }
}

#[test]
fn a_timeline_against_itself_has_no_change() {
  for seed in 0..SEEDS {
    let timeline = timeline(seed);
    assert!(
      diff(&timeline, &timeline).unwrap().is_empty(),
      "seed {seed}"
    );
  }
}

#[test]
fn diff_names_exactly_the_change_made() {
  for seed in 0..SEEDS {
    let before = timeline(seed);
    let Some(track) = before.tracks().iter().position(|t| !t.clips().is_empty()) else {
      continue;
    };
    let clip = (seed as usize) % before.tracks()[track].clips().len();
    let mut after = before.clone();
    let edited = &mut after.tracks_mut()[track].clips_mut()[clip];
    let kind = match seed % 3 {
      0 => {
        let enabled = edited.enabled();
        edited.set_enabled(!enabled);
        ChangeKind::EnabledFlipped
      }
      1 => {
        let name = format_utf8bytes!("{}, renamed", edited.name());
        edited.set_name(name);
        ChangeKind::Renamed
      }
      _ => {
        let locator = format_utf8bytes!("{}, moved", edited.media().locator());
        edited.media_mut().set_locator(locator);
        ChangeKind::Relinked
      }
    };
    let changes = diff(&before, &after).unwrap();
    let changes: Vec<_> = changes
      .changes()
      .iter()
      .map(|change| {
        (
          change.track(),
          change.kind(),
          change.before(),
          change.after(),
        )
      })
      .collect();
    assert_eq!(
      changes,
      [(track, kind, Some(clip), Some(clip))],
      "seed {seed}"
    );
  }
}

#[cfg(feature = "serde")]
#[test]
fn a_document_round_trips() {
  for seed in 0..SEEDS {
    let timeline = timeline(seed);
    let text = serde_json::to_string(&timeline).unwrap();
    let back: Timeline = serde_json::from_str(&text).unwrap();
    assert_eq!(back, timeline, "seed {seed}");
  }
}

/// The one generated timeline the export refuses: seed 132's track 1, at
/// 29.97 fps from frame 106 560, is 396 396 ticks of 1/90 000 s long, which
/// OpenTimelineIO sums from frames and 48 kHz samples one ulp short — so
/// `end_time_inclusive` of its range from the global start floors an end
/// that rounds up to 320 396 076, a tick past its last, 320 396 075
/// (`src/otio/tests/derived.rs` re-derives it in OpenTimelineIO's
/// arithmetic).
const REFUSED: u64 = 132;

/// `timeline`, built from `seed`, as the export writes it for `target` —
/// `None` for [`REFUSED`], after checking it is refused exactly there.
fn exported(seed: u64, timeline: &Timeline, target: OtioTarget) -> Option<Utf8Bytes> {
  match to_otio(timeline, target) {
    Ok(text) if seed != REFUSED => Some(text),
    Err(Refused::NotRepresentable(count)) if seed == REFUSED => {
      assert_eq!(
        (count.at(), count.value(), count.rate()),
        (Spot::AbsoluteEnd(1), 320_396_075, Rate::hz(90_000))
      );
      None
    }
    other => panic!("seed {seed}: {other:?}"),
  }
}

/// **What the export writes reads back exactly**, for both targets:
/// `from_otio(to_otio(timeline))` is the timeline itself — every count,
/// timebase and rate as the model held it, the markers, the notes and the
/// transitions' order among them.
#[test]
fn every_generated_timeline_reads_back_from_its_opentimelineio() {
  for seed in 0..SEEDS {
    let timeline = timeline(seed);
    for target in [OtioTarget::V0_15Plus, OtioTarget::Legacy] {
      let Some(text) = exported(seed, &timeline, target) else {
        continue;
      };
      assert_eq!(
        from_otio(&text),
        Ok(timeline.clone()),
        "seed {seed}, {target:?}"
      );
    }
  }
}

#[test]
fn the_export_passes_the_self_check_for_both_targets() {
  for seed in 0..SEEDS {
    let timeline = timeline(seed);
    for target in [OtioTarget::V0_15Plus, OtioTarget::Legacy] {
      let Some(text) = exported(seed, &timeline, target) else {
        continue;
      };
      assert_eq!(validate_json(&text, target), Ok(()), "seed {seed}");
    }
  }
}

/// OpenTimelineIO places a track's items end to end, each as long as its
/// `source_range`'s duration. Every range is written in one ruler, so the end
/// OpenTimelineIO derives from a clip's is its stored source range's end;
/// and each duration, read in the ruler it is written in, is a whole number
/// of edit-rate frames — so walking the items lands every clip on its
/// record, exactly.
#[test]
fn the_export_places_every_clip_on_its_record() {
  for seed in 0..SEEDS {
    let timeline = timeline(seed);
    let edit = timeline.edit_timebase().unwrap();
    let Some(text) = exported(seed, &timeline, OtioTarget::V0_15Plus) else {
      continue;
    };
    let root: serde_json::Value = serde_json::from_str(&text).unwrap();
    let tracks = root["tracks"]["children"].as_array().unwrap();
    for (track, exported) in timeline.tracks().iter().zip(tracks) {
      let mut clips = track.clips().iter();
      let mut at = 0;
      for child in exported["children"].as_array().unwrap() {
        let schema = child["OTIO_SCHEMA"].as_str().unwrap();
        if schema == "Transition.1" {
          continue;
        }
        let range = &child["source_range"];
        let rate = range["duration"]["rate"].as_f64().unwrap();
        assert_eq!(
          range["start_time"]["rate"].as_f64(),
          Some(rate),
          "seed {seed}: a range in two rates"
        );
        let start = range["start_time"]["value"].as_f64().unwrap() as i64;
        let length = range["duration"]["value"].as_f64().unwrap() as i64;
        let ruler = if schema == "Gap.1" {
          assert_eq!(rate, timeline.rate().as_f64(), "seed {seed}");
          edit
        } else {
          let clip = clips.next().unwrap();
          assert_eq!(
            clip.record().start_pts(),
            at,
            "seed {seed}: {}",
            clip.name()
          );
          let ruler = ruler_written(clip, rate);
          let written = TimeRange::new(start, start + length, ruler);
          let stored = clip.source_range();
          assert!(
            written.start() == stored.start() && written.end() == stored.end(),
            "seed {seed}: {} written as {written:?}, stored as {stored:?}",
            clip.name()
          );
          ruler
        };
        let frames = Duration::new(length as u64, ruler)
          .checked_rescale_with(edit, Rounding::Exact)
          .unwrap_or_else(|| panic!("seed {seed}: a length between frames"));
        at += frames.ticks() as i64;
      }
      assert!(clips.next().is_none(), "seed {seed}: a clip not exported");
    }
  }
}

/// OpenTimelineIO's `RationalTime` sum, written again from its C++ for the
/// property below (`opentime/rationalTime.h` 70–75, 316–326): carried in the
/// higher rate, the other value rescaled to it as `value * rate / from`.
fn otio_plus(a: (f64, f64), b: (f64, f64)) -> (f64, f64) {
  let rescaled = |(value, from): (f64, f64), to: f64| {
    if to == from { value } else { value * to / from }
  };
  if a.1 < b.1 {
    (rescaled(a, b.1) + b.0, b.1)
  } else {
    (rescaled(b, a.1) + a.0, a.1)
  }
}

/// Read back as OpenTimelineIO reads it — each clip's place on its track
/// from zero in the clip's own rate, every item before it added
/// (`opentimelineio/track.cpp` 51–92) — every clip lands within a millionth
/// of a tick of its record, in whichever rate OpenTimelineIO carries it.
#[test]
fn opentimelineio_reads_every_clip_back_where_its_record_puts_it() {
  for seed in 0..SEEDS {
    let timeline = timeline(seed);
    let edit = timeline.rate();
    let Some(text) = exported(seed, &timeline, OtioTarget::V0_15Plus) else {
      continue;
    };
    let root: serde_json::Value = serde_json::from_str(&text).unwrap();
    let tracks = root["tracks"]["children"].as_array().unwrap();
    for (track, exported) in timeline.tracks().iter().zip(tracks) {
      let children = exported["children"].as_array().unwrap();
      // Each item's duration as `(value, rate)`; a transition overlaps its
      // neighbours and adds none.
      let durations: Vec<Option<(f64, f64)>> = children
        .iter()
        .map(|child| {
          let duration = &child["source_range"]["duration"];
          (child["OTIO_SCHEMA"] != "Transition.1").then(|| {
            (
              duration["value"].as_f64().unwrap(),
              duration["rate"].as_f64().unwrap(),
            )
          })
        })
        .collect();
      let mut clips = track.clips().iter();
      for (index, child) in children.iter().enumerate() {
        if !child["OTIO_SCHEMA"].as_str().unwrap().starts_with("Clip") {
          continue;
        }
        let clip = clips.next().unwrap();
        let rate = durations[index].unwrap().1;
        let (place, carried) = durations[..index]
          .iter()
          .flatten()
          .fold((0.0, rate), |sum, &duration| otio_plus(sum, duration));
        let seconds =
          clip.record().start_pts() as f64 * f64::from(edit.den().get()) / f64::from(edit.num());
        let record = seconds * carried;
        assert!(
          (place - record).abs() < 1e-6,
          "seed {seed}: {} read back at {place}, its record at {record}, {carried} a second",
          clip.name()
        );
      }
      assert!(clips.next().is_none(), "seed {seed}: a clip not exported");
    }
  }
}

/// The ruler a clip's range is written in: frames of its medium's stated
/// rate where the export used them, ticks of the source's timebase where it
/// used those, else a whole number of ticks a second the export's search
/// chose — coarser than the timebase's, as seed 208's `c0`, written in
/// ticks of 1/67 500 s rather than 1/90 000 s, where OpenTimelineIO derives
/// every value of its track exactly.
fn ruler_written(clip: &Clip, rate: f64) -> Timebase {
  match clip.media().rate() {
    Some(stated) if stated.as_f64() == rate => stated.checked_to_timebase().unwrap(),
    _ => {
      let ticks = clip.source_range().timebase();
      let own = Rate::checked_from_timebase(ticks).unwrap().as_f64();
      if own == rate {
        return ticks;
      }
      assert!(rate.fract() == 0.0 && rate < own, "{rate} against {own}");
      common::tb(1, rate as i32)
    }
  }
}

/// The properties above are only as strong as what the generator builds:
/// across the seeds it reaches every word of the model, often.
#[test]
fn the_generator_reaches_every_word() {
  #[derive(Default, Debug)]
  struct Seen {
    tracks: [usize; 2],
    clips: usize,
    gaps: usize,
    abutting: usize,
    transitions: usize,
    fades_in: usize,
    fades_out: usize,
    gains: usize,
    disabled: usize,
    unknown_available: usize,
    media_rates: usize,
    reels: usize,
    cut_backs: usize,
    rulers: std::collections::BTreeSet<String>,
    rates: std::collections::BTreeSet<String>,
    track_markers: usize,
    clip_markers: usize,
    marker_points: usize,
    comments: usize,
    colors: std::collections::BTreeSet<String>,
    notes: usize,
    disabled_tracks: usize,
    reordered: usize,
  }
  let mut seen = Seen::default();
  for seed in 0..SEEDS {
    let timeline = timeline(seed);
    seen.rates.insert(timeline.rate().to_string());
    seen.notes += usize::from(!timeline.metadata().is_empty());
    for (track, laid) in timeline
      .tracks()
      .iter()
      .zip(layout(&timeline).unwrap().tracks())
    {
      seen.tracks[usize::from(track.kind() == TrackKind::Audio)] += 1;
      seen.transitions += track.transitions().len();
      seen.disabled_tracks += usize::from(!track.enabled());
      seen.reordered += usize::from(
        track
          .transitions()
          .windows(2)
          .any(|pair| pair[1].at() < pair[0].at()),
      );
      let markers = track
        .markers()
        .iter()
        .chain(track.clips().iter().flat_map(Clip::markers));
      for marker in markers {
        seen.colors.insert(marker.color().to_string());
        seen.comments += usize::from(!marker.comment().is_empty());
        seen.marker_points += usize::from(marker.range().start() == marker.range().end());
      }
      seen.track_markers += track.markers().len();
      seen.gaps += laid
        .items()
        .iter()
        .filter(|item| matches!(item, Item::Gap(_)))
        .count();
      seen.abutting += track
        .clips()
        .windows(2)
        .filter(|pair| pair[0].record().end() == pair[1].record().start())
        .count();
      seen.cut_backs += (0..track.clips().len())
        .filter(|&index| {
          let clip = &track.clips()[index];
          track.clips()[..index].iter().any(|earlier| {
            earlier.name() == clip.name() && earlier.media().locator() == clip.media().locator()
          })
        })
        .count();
      for clip in track.clips() {
        seen.clips += 1;
        seen.fades_in += usize::from(clip.fades().in_().is_some());
        seen.fades_out += usize::from(clip.fades().out().is_some());
        seen.gains += usize::from(clip.gain().is_some());
        seen.disabled += usize::from(!clip.enabled());
        seen.unknown_available += usize::from(clip.media().available_range().is_none());
        seen.media_rates += usize::from(clip.media().rate().is_some());
        seen.reels += usize::from(clip.media().reel().is_some());
        seen.clip_markers += clip.markers().len();
        seen.notes += usize::from(!clip.metadata().is_empty());
        seen
          .rulers
          .insert(clip.source_range().timebase().to_string());
      }
    }
  }
  if cfg!(miri) {
    return;
  }
  let enough = |count: usize, what: &str| assert!(count >= 40, "{what}: {count} in {seen:?}");
  enough(seen.tracks[0], "video tracks");
  enough(seen.tracks[1], "audio tracks");
  enough(seen.clips, "clips");
  enough(seen.gaps, "gaps");
  enough(seen.abutting, "abutting clips");
  enough(seen.transitions, "transitions");
  enough(seen.fades_in, "fades in");
  enough(seen.fades_out, "fades out");
  enough(seen.gains, "gains");
  enough(seen.disabled, "disabled clips");
  enough(seen.unknown_available, "unknown available ranges");
  enough(seen.media_rates, "media rates");
  enough(seen.reels, "reels");
  enough(seen.cut_backs, "cut-backs");
  enough(seen.track_markers, "track markers");
  enough(seen.clip_markers, "clip markers");
  enough(seen.marker_points, "point markers");
  enough(seen.comments, "marker comments");
  enough(seen.notes, "notes");
  enough(seen.disabled_tracks, "disabled tracks");
  assert!(seen.reordered >= 10, "reordered tracks: {seen:?}");
  assert_eq!(seen.colors.len(), MarkerColor::ALL.len(), "{seen:?}");
  assert_eq!(seen.rulers.len(), media_rulers().len(), "{seen:?}");
  assert_eq!(seen.rates.len(), RATES.len(), "{seen:?}");
}
