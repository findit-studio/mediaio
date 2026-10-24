//! The law timeline, shared by the integration laws.
#![allow(dead_code)]

use core::num::NonZeroI32;

use mediaio_timeline::{
  Clip, ClipId, Duration, Fade, FadeShape, Fades, Gain, Marker, MarkerColor, MediaRef, Metadata,
  Rate, ReaderMetadata, TimeRange, Timebase, Timeline, Timestamp, Track, TrackKind, Transition,
};

pub fn tb(num: i32, den: i32) -> Timebase {
  Timebase::new(num, NonZeroI32::new(den).unwrap())
}

/// Two tracks, three clips, a dissolve, two fades and three markers, at
/// 23.976 fps from 01:00:00:00. The clips' ids are `clip-1` to `clip-3`, in
/// that order.
///
/// - The timeline: a blue `b` marks frames 96–144, the second shot, its
///   comment `the second shot` noted again where DaVinci Resolve reads a
///   marker's note (`Resolve_OTIO`'s `Note`) — on the stack, whose markers
///   are the ones Resolve imports as its timeline's.
/// - V1: `a` at [0, 96) plays frames 24–120 of a 240-frame movie (reel
///   A001), a green `slate` marking its frames 48–72; a 12 + 12-frame
///   dissolve at 96 into `b` at [96, 144), frames 48–96 of a 480-frame
///   movie, which fades out over its last 24 frames at the track's end.
///   The track's red `act one` marks its first 48 frames, commented `the
///   opening`.
/// - A1: a 24-frame gap, then `a-sound` at [24, 120): 1.001–5.005 s of a
///   48 kHz recording, at -6 dB, fading in over 12 frames, equal power.
pub fn law() -> Timeline {
  let edit = tb(1001, 24_000);
  let movie = tb(1, 24_000);
  let sound = tb(1, 48_000);
  let frames = |n: u64| Duration::new(n, edit);
  Timeline::new("law", Rate::FPS_23_976)
    .with_start(Timestamp::new(86_400, edit))
    .with_metadata(Metadata::new().with("project", "law"))
    .with_track(
      Track::new(TrackKind::Video, "V1")
        .with_clip(
          Clip::new(
            ClipId::new("clip-1"),
            "a",
            MediaRef::new("file:///media/a.mov")
              .with_available_range(Some(TimeRange::new(0, 240_240, movie)))
              .with_rate(Some(Rate::FPS_23_976))
              .with_reel(Some("A001".into())),
            TimeRange::new(24_024, 120_120, movie),
            TimeRange::new(0, 96, edit),
          )
          .with_marker(Marker::new("slate", TimeRange::new(48_048, 72_072, movie))),
        )
        .with_clip(
          Clip::new(
            ClipId::new("clip-2"),
            "b",
            MediaRef::new("file:///media/b.mov")
              .with_available_range(Some(TimeRange::new(0, 480_480, movie)))
              .with_rate(Some(Rate::FPS_23_976)),
            TimeRange::new(48_048, 96_096, movie),
            TimeRange::new(96, 144, edit),
          )
          .with_fades(Fades::new().with_out(Some(Fade::new(frames(24), FadeShape::Linear)))),
        )
        .with_transition(Transition::dissolve(
          Timestamp::new(96, edit),
          frames(12),
          frames(12),
        ))
        .with_marker(
          Marker::new("act one", TimeRange::new(0, 48, edit))
            .with_color(MarkerColor::Red)
            .with_comment("the opening"),
        ),
    )
    .with_track(
      Track::new(TrackKind::Audio, "A1").with_clip(
        Clip::new(
          ClipId::new("clip-3"),
          "a-sound",
          MediaRef::new("file:///media/a.wav")
            .with_available_range(Some(TimeRange::new(0, 480_480, sound)))
            .with_rate(Some(Rate::hz(48_000))),
          TimeRange::new(48_048, 240_240, sound),
          TimeRange::new(24, 120, edit),
        )
        .with_gain(Gain::from_db(-6.0))
        .with_fades(Fades::new().with_in(Some(Fade::new(frames(12), FadeShape::EqualPower)))),
      ),
    )
    .with_marker(
      Marker::new("b", TimeRange::new(96, 144, edit))
        .with_color(MarkerColor::Blue)
        .with_comment("the second shot")
        .with_readers(ReaderMetadata::new().with(
          "Resolve_OTIO",
          Metadata::new().with("Note", "the second shot"),
        )),
    )
}
