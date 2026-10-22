//! The reader: what the export writes reads back exactly, and what the
//! timeline does not model is refused by name, with where it sits.
//!
//! Under Miri the laws run lighter, every branch they reach still reached:
//! Miri interprets every step, and on a 32-bit target the whole suite
//! shares one 4 GiB address space, no frame's addresses ever given back.
//! A law whose cases read alike for both targets reads one ([`TARGETS`]), a
//! table of spellings of one kind runs its first row ([`alike`]), and a
//! heavy law whose every branch the others reach does not run. Natively
//! every case runs.

use std::{string::String, vec, vec::Vec};

use smol_bytes::{Utf8Bytes, Utf8BytesMut};

use super::{
  edit,
  json::{self, Value},
};
use crate::{
  Clip, ClipAt, ClipId, Duration, Fade, FadeShape, Fades, Gain, IdClash, Marker, MarkerAt,
  MarkerColor, MediaRef, Metadata, MetadataAt, NamespaceAt, Rate, ReaderMetadata, Refusal,
  TimeRange, Timebase, Timeline, Timestamp, Track, TrackKind, Transition,
  otio::{
    Feature, Inexact, Invalid, OtioTarget, Refused, Shape, UnmodelledFeature, UnmodelledSchema,
    Unreadable, from_otio, to_otio, validate_json,
  },
};

/// One track at 24 fps: a 12-frame gap; `a` at [12, 36), fading in over 6
/// frames, at -3 dB, with a marker; a 2 + 2-frame dissolve at 36 into `b`
/// at [36, 60). The track carries a red marker over its first second.
fn sample() -> Timeline {
  let medium = |name: &str| {
    MediaRef::new(smol_bytes::format_utf8bytes!("file:///{name}.mov"))
      .with_available_range(Some(TimeRange::new(0, 100, edit())))
      .with_rate(Some(Rate::FPS_24))
  };
  let a = Clip::new(
    ClipId::new("a"),
    "a",
    medium("a"),
    TimeRange::new(10, 34, edit()),
    TimeRange::new(12, 36, edit()),
  )
  .with_gain(Gain::from_db(-3.0))
  .with_fades(Fades::new().with_in(Some(Fade::new(Duration::new(6, edit()), FadeShape::Linear))))
  .with_marker(Marker::new("n", TimeRange::new(10, 12, edit())));
  let b = Clip::new(
    ClipId::new("b"),
    "b",
    medium("b"),
    TimeRange::new(10, 34, edit()),
    TimeRange::new(36, 60, edit()),
  );
  Timeline::new("sample", Rate::FPS_24).with_track(
    Track::new(TrackKind::Video, "V")
      .with_clip(a)
      .with_clip(b)
      .with_transition(Transition::dissolve(
        Timestamp::new(36, edit()),
        Duration::new(2, edit()),
        Duration::new(2, edit()),
      ))
      .with_marker(Marker::new("m", TimeRange::new(0, 24, edit())).with_color(MarkerColor::Red)),
  )
}

/// The sample's first track, as the export lays it out:
/// `[gap, fade in, a, dissolve, b]`.
const TRACK: &str = "tracks.children.0";
const GAP: &str = "tracks.children.0.children.0";
const FADE_IN: &str = "tracks.children.0.children.1";
const A: &str = "tracks.children.0.children.2";
const DISSOLVE: &str = "tracks.children.0.children.3";
const B: &str = "tracks.children.0.children.4";
const A_MEDIA: &str = "tracks.children.0.children.2.media_references.DEFAULT_MEDIA";

/// The value at the dotted `path` inside `value`: object keys, and array
/// indices as numbers.
fn at<'a>(value: &'a mut Value, path: &str) -> &'a mut Value {
  path
    .split('.')
    .filter(|step| !step.is_empty())
    .fold(value, |value, step| match value {
      Value::Object(members) => {
        &mut members
          .iter_mut()
          .find(|(key, _)| key == step)
          .unwrap_or_else(|| panic!("no {step}"))
          .1
      }
      Value::Array(items) => &mut items[step.parse::<usize>().unwrap()],
      _ => panic!("{step} inside a scalar"),
    })
}

/// Sets `key` of the object at `path` to `new`, in place or appended.
fn set(value: &mut Value, path: &str, key: &str, new: Value) {
  let Value::Object(members) = at(value, path) else {
    panic!("{path} is no object")
  };
  match members.iter_mut().find(|(name, _)| name == key) {
    Some((_, old)) => *old = new,
    None => members.push((Utf8Bytes::from(key), new)),
  }
}

/// Takes `key` out of the object at `path`.
fn remove(value: &mut Value, path: &str, key: &str) {
  let Value::Object(members) = at(value, path) else {
    panic!("{path} is no object")
  };
  members.retain(|(name, _)| name != key);
}

fn text(value: &str) -> Value {
  Value::String(Utf8Bytes::from(value))
}

fn number(value: &str) -> Value {
  Value::Number(Utf8Bytes::from(value))
}

fn object(members: Vec<(&str, Value)>) -> Value {
  Value::Object(
    members
      .into_iter()
      .map(|(key, value)| (Utf8Bytes::from(key), value))
      .collect(),
  )
}

/// The sample as the export writes it for `target`, parsed: with `std`,
/// written and parsed once, for every law that edits a copy of it.
fn written(target: OtioTarget) -> Value {
  let write = || json::parse(&to_otio(&sample(), target).unwrap()).unwrap();
  #[cfg(feature = "std")]
  {
    static WRITTEN: [std::sync::OnceLock<Value>; 2] = [const { std::sync::OnceLock::new() }; 2];
    let slot = match target {
      OtioTarget::V0_15Plus => &WRITTEN[0],
      OtioTarget::Legacy => &WRITTEN[1],
    };
    slot.get_or_init(write).clone()
  }
  #[cfg(not(feature = "std"))]
  write()
}

/// The targets a law reads where its cases read alike for both: both, or
/// under Miri one (the module's note); `Legacy`'s own members are read by
/// the laws that name it.
const TARGETS: &[OtioTarget] = if cfg!(miri) {
  &[OtioTarget::V0_15Plus]
} else {
  &[OtioTarget::V0_15Plus, OtioTarget::Legacy]
};

/// The rows of a table whose rows reach the same branches, each spelling a
/// value of one kind: every one, or under Miri the first (the module's
/// note).
fn alike<T>(rows: impl IntoIterator<Item = T>) -> impl Iterator<Item = T> {
  rows
    .into_iter()
    .take(if cfg!(miri) { 1 } else { usize::MAX })
}

/// The sample written for `target`, edited, written out again and read.
fn read_edited(target: OtioTarget, edit: impl FnOnce(&mut Value)) -> Result<Timeline, Unreadable> {
  let mut root = written(target);
  edit(&mut root);
  let mut out = Utf8BytesMut::new();
  json::write_pretty(&root, &mut out);
  from_otio(&out.freeze_shared())
}

/// [`read_edited`] for `V0_15Plus` readers.
fn read(edit: impl FnOnce(&mut Value)) -> Result<Timeline, Unreadable> {
  read_edited(OtioTarget::V0_15Plus, edit)
}

/// `$.` and the dotted path, as the reader writes one.
fn path(dotted: &str) -> String {
  let mut path = String::from("$");
  for step in dotted.split('.').filter(|step| !step.is_empty()) {
    if step.bytes().all(|byte| byte.is_ascii_digit()) {
      path.push('[');
      path.push_str(step);
      path.push(']');
    } else {
      path.push('.');
      path.push_str(step);
    }
  }
  path
}

fn schema(name: &str, dotted: &str) -> Unreadable {
  Unreadable::Schema(UnmodelledSchema::new(
    Utf8Bytes::from(name),
    Utf8Bytes::from(path(dotted)),
  ))
}

fn feature(feature: Feature, dotted: &str) -> Unreadable {
  Unreadable::Feature(UnmodelledFeature::new(
    feature,
    Utf8Bytes::from(path(dotted)),
  ))
}

fn shape(dotted: &str, expected: &'static str) -> Unreadable {
  Unreadable::Shape(Shape {
    path: Utf8Bytes::from(path(dotted)),
    expected,
  })
}

fn inexact(dotted: &str, reason: &'static str) -> Unreadable {
  Unreadable::Inexact(Inexact::new(Utf8Bytes::from(path(dotted)), reason))
}

/// The reason the backstop gives.
const NOT_WRITTEN_BACK: &str = "a value the export does not write back as the document states it";

/// The backstop's refusal at the dotted `path`: the export writes `written`
/// there and the document states `stated`, each as JSON, or `nothing`.
fn differs(dotted: &str, written: &str, stated: &str) -> Unreadable {
  Unreadable::Inexact(Inexact::with_values(
    Utf8Bytes::from(path(dotted)),
    NOT_WRITTEN_BACK,
    Utf8Bytes::from(written),
    Utf8Bytes::from(stated),
  ))
}

#[test]
fn what_the_export_writes_reads_back_exactly() {
  let sample = sample();
  for target in [OtioTarget::V0_15Plus, OtioTarget::Legacy] {
    let text = to_otio(&sample, target).unwrap();
    assert_eq!(from_otio(&text), Ok(sample.clone()), "{target:?}");
  }
}

#[test]
#[cfg_attr(
  miri,
  ignore = "too heavy for Miri on a 32-bit target; the laws Miri runs reach its every branch"
)]
fn what_carries_nothing_reads_as_nothing() {
  // Round 10: a member reads as nothing only where OpenTimelineIO 0.17.0
  // holds it as it holds the member's absence and the export writes it back
  // so — left out where the export writes its default (the written
  // defaults), or a reference's image bounds stated `null` where `Legacy`
  // writes none (the stated defaults).
  let read = read(|root| {
    remove(root, &format!("{TRACK}.markers.0"), "comment");
    remove(root, A_MEDIA, "available_image_bounds");
    for item in ["tracks", TRACK, GAP, A, B] {
      remove(root, item, "effects");
      remove(root, item, "enabled");
    }
  });
  assert_eq!(read, Ok(sample()));
  // A marker with no color is OpenTimelineIO's default, green.
  let green = self::read(|root| remove(root, &format!("{TRACK}.markers.0"), "color")).unwrap();
  assert_eq!(green.tracks()[0].markers()[0].color(), MarkerColor::Green);
  let legacy = read_edited(OtioTarget::Legacy, |root| {
    set(
      root,
      &format!("{A}.media_reference"),
      "available_image_bounds",
      Value::Null,
    )
  });
  assert_eq!(legacy, Ok(sample()));
}

#[test]
fn a_member_opentimelineio_keeps_otherwise_than_the_export_writes_is_refused() {
  // Round 10. A `color` on an item and an `enabled` on a transition are
  // members OpenTimelineIO 0.17.0's schemas do not know, which it keeps as
  // dynamic fields and writes back (`serializableObject.cpp` 78–107):
  // refused by name, `null` and `true` among them.
  assert_eq!(
    read(|root| set(root, A, "color", Value::Null)),
    Err(feature(Feature::ItemColor, &format!("{A}.color"))),
  );
  assert_eq!(
    read(|root| set(root, DISSOLVE, "enabled", Value::Bool(true))),
    Err(feature(
      Feature::Key(Utf8Bytes::from("enabled")),
      &format!("{DISSOLVE}.enabled")
    )),
  );
  // The backstop refuses the rest, naming both values: the stack named
  // otherwise than the export names it, or not at all, and words the
  // timeline keeps none of, which OpenTimelineIO keeps.
  assert_eq!(
    read(|root| set(root, "tracks", "name", text(""))),
    Err(differs("tracks.name", "\"tracks\"", "\"\"")),
  );
  assert_eq!(
    read(|root| remove(root, "tracks", "name")),
    Err(differs("tracks.name", "\"tracks\"", "nothing")),
  );
  assert_eq!(
    read(|root| set(
      root,
      GAP,
      "metadata",
      object(vec![("mediaio", object(vec![]))]),
    )),
    Err(differs(&format!("{GAP}.metadata.mediaio"), "nothing", "{}")),
  );
}

#[test]
fn the_backstop_alone_refuses_a_string_or_a_boolean_written_back_otherwise() {
  // Round 10: the backstop by itself, every check before it bypassed — the
  // document altered after the timeline was read from it. Codex's case
  // through it: a marker's color `Green`, which the export writes `GREEN`.
  let mut timeline = sample();
  timeline.tracks_mut()[0].markers_mut()[0].set_color(MarkerColor::Green);
  for &target in TARGETS {
    let written = json::parse(&to_otio(&timeline, target).unwrap()).unwrap();
    let guarded = |edit: &dyn Fn(&mut Value)| {
      let mut root = written.clone();
      edit(&mut root);
      super::super::import::reproduced(&root, &timeline)
    };
    assert_eq!(guarded(&|_| {}), Ok(()), "{target:?}: the premise");
    let marker = format!("{TRACK}.markers.0");
    assert_eq!(
      guarded(&|root| set(root, &marker, "color", text("Green"))),
      Err(differs(
        &format!("{marker}.color"),
        "\"GREEN\"",
        "\"Green\""
      )),
      "{target:?}",
    );
    assert_eq!(
      guarded(&|root| set(root, A, "name", text("b"))),
      Err(differs(&format!("{A}.name"), "\"a\"", "\"b\"")),
      "{target:?}",
    );
    assert_eq!(
      guarded(&|root| set(root, TRACK, "enabled", Value::Bool(false))),
      Err(differs(&format!("{TRACK}.enabled"), "true", "false")),
      "{target:?}",
    );
  }
}

/// Every schema the export writes: an object carrying `OTIO_SCHEMA` is
/// one of these, or a reader would take a plain dictionary for an object.
const SCHEMAS: [&str; 11] = [
  "Timeline.1",
  "Stack.1",
  "Track.1",
  "Clip.1",
  "Clip.2",
  "Gap.1",
  "Transition.1",
  "ExternalReference.1",
  "Marker.2",
  "RationalTime.1",
  "TimeRange.1",
];

/// Whether every dictionary in `value` OpenTimelineIO reads as a plain one
/// is one: no object below a `metadata` member carries `OTIO_SCHEMA`, and
/// every object that does names a schema the export writes.
fn plain_where_it_should_be(value: &Value, in_metadata: bool) -> bool {
  match value {
    Value::Object(members) => {
      let schema = members
        .iter()
        .find(|(key, _)| key == "OTIO_SCHEMA")
        .map(|(_, schema)| schema);
      let schema_ok = match schema {
        None => true,
        Some(Value::String(name)) => !in_metadata && SCHEMAS.contains(&name.as_str()),
        Some(_) => false,
      };
      schema_ok
        && members
          .iter()
          .all(|(key, member)| plain_where_it_should_be(member, in_metadata || key == "metadata"))
    }
    Value::Array(items) => items
      .iter()
      .all(|item| plain_where_it_should_be(item, in_metadata)),
    _ => true,
  }
}

#[test]
#[cfg_attr(
  miri,
  ignore = "too heavy for Miri on a 32-bit target; the laws Miri runs reach its every branch"
)]
fn a_note_keyed_as_otio_keys_its_objects_is_written_as_a_pair_and_reads_back() {
  // The keys OpenTimelineIO's reader looks for (`deserialization.cpp`
  // 561–573, 627–629 in 0.17.0), the crate's own word names, and an empty
  // key: none may become a member of an object.
  let notes = || {
    Metadata::new()
      .with("OTIO_SCHEMA", "Marker.2")
      .with("OTIO_REF_ID", "a reference")
      .with("metadata", "a note")
      .with("mediaio", "another")
      .with("", "an empty key")
  };
  let mut timeline = sample().with_metadata(notes());
  *timeline.tracks_mut()[0].clips_mut()[0].metadata_mut() = notes();
  for target in [OtioTarget::V0_15Plus, OtioTarget::Legacy] {
    let written = to_otio(&timeline, target).unwrap();
    assert_eq!(validate_json(&written, target), Ok(()), "{target:?}");
    let root = json::parse(&written).unwrap();
    assert!(
      plain_where_it_should_be(&root, false),
      "{target:?}: a note keyed as a schema is an object: {written}"
    );
    assert_eq!(from_otio(&written), Ok(timeline.clone()), "{target:?}");
  }
  // The pairs, as written.
  let mut root = json::parse(&to_otio(&timeline, OtioTarget::V0_15Plus).unwrap()).unwrap();
  let pairs = at(&mut root, "metadata.mediaio.metadata");
  let Value::Array(pairs) = pairs else {
    panic!("the notes are an array: {pairs:?}")
  };
  assert!(pairs.contains(&Value::Array(vec![text("OTIO_SCHEMA"), text("Marker.2")])));
}

/// The sample written for `target`, edited, written out again and held to
/// the self-check: where it refuses, and what it expects there.
fn checked_edited(
  target: OtioTarget,
  edit: impl FnOnce(&mut Value),
) -> Result<(), (String, &'static str)> {
  let mut root = written(target);
  edit(&mut root);
  let mut out = Utf8BytesMut::new();
  json::write_pretty(&root, &mut out);
  validate_json(&out.freeze_shared(), target).map_err(|invalid| match invalid {
    Invalid::Shape(shape) => (String::from(shape.path()), shape.expected()),
    other => panic!("{other:?}"),
  })
}

#[test]
fn the_self_check_decodes_every_dictionary_as_a_reader_does() {
  // A reader decodes every dictionary as its parser closes it, wherever it
  // sits, one carrying `OTIO_SCHEMA` as an object of that schema
  // (`deserialization.cpp` 561–573 in 0.17.0), so the check walks them all.
  let refused = |path: &str, expected: &'static str| Err((String::from(path), expected));
  let schema = |name: &str, mut members: Vec<(&str, Value)>| {
    members.insert(0, ("OTIO_SCHEMA", text(name)));
    object(members)
  };
  let rational = || {
    schema(
      "RationalTime.1",
      vec![("rate", number("24.0")), ("value", number("1.0"))],
    )
  };
  let studio = |value: Value| move |root: &mut Value| set(root, "metadata", "studio", value);
  let clip = "$.tracks.children[0].children[2]";
  for &target in TARGETS {
    // Round 4's plant N1: the notes written as an object. One keyed
    // `OTIO_SCHEMA: Marker.2` is a marker to every reader, which refuses it
    // for the range it lacks.
    assert_eq!(
      checked_edited(target, |root| set(
        root,
        "metadata.mediaio",
        "metadata",
        schema("Marker.2", vec![])
      )),
      refused("$.metadata.mediaio.metadata.marked_range", "a required key"),
      "{target:?}",
    );
    // N1's document whole: its note keyed `metadata` is a string, which the
    // marker reads as its metadata.
    assert_eq!(
      checked_edited(target, |root| set(
        root,
        "metadata.mediaio",
        "metadata",
        object(vec![
          ("", text("an empty key")),
          ("OTIO_REF_ID", text("a reference")),
          ("OTIO_SCHEMA", text("Marker.2")),
          ("mediaio", text("another")),
          ("metadata", text("a note")),
        ])
      )),
      refused("$.metadata.mediaio.metadata.metadata", "an object"),
      "{target:?}",
    );
    // A plain dictionary passes however deep, and so does an object of a
    // schema the check knows, held to its rules.
    let plain = object(vec![
      (
        "crew",
        Value::Array(vec![
          object(vec![("name", text("x"))]),
          number("1"),
          Value::Null,
        ]),
      ),
      ("start", rational()),
    ]);
    assert_eq!(checked_edited(target, studio(plain)), Ok(()), "{target:?}");
    // A schema whose rules the check does not know is refused by name: one
    // no reader registers, which a reader keeps as an `UnknownSchema`; one
    // whose registered constructor is a fatal error (`typeRegistry.cpp`
    // 46–55 in 0.17.0); a version past the registered one, and a name
    // without a version, both of which a reader refuses too.
    for name in ["Studio.1", "UnknownSchema.1", "Marker.3", "Marker"] {
      assert_eq!(
        checked_edited(target, studio(schema(name, vec![]))),
        refused(
          "$.metadata.studio.OTIO_SCHEMA",
          "a schema whose rules the check knows"
        ),
        "{target:?} {name}",
      );
    }
    // Metadata is a dictionary, never a schema's object.
    assert_eq!(
      checked_edited(target, |root| set(
        root,
        "metadata",
        "OTIO_SCHEMA",
        text("Marker.2")
      )),
      refused(
        "$.metadata.OTIO_SCHEMA",
        "no schema: metadata is a dictionary"
      ),
      "{target:?}",
    );
    // In a clip's metadata, inside an array; and under a key no reader
    // reads, which a reader keeps as the clip's own.
    assert_eq!(
      checked_edited(target, |root| set(
        root,
        &format!("{A}.metadata"),
        "studio",
        Value::Array(vec![schema("TimeRange.1", vec![])])
      )),
      refused(
        &format!("{clip}.metadata.studio[0].duration"),
        "a required key"
      ),
      "{target:?}",
    );
    assert_eq!(
      checked_edited(target, |root| set(
        root,
        A,
        "studio",
        schema("Marker.2", vec![])
      )),
      refused(&format!("{clip}.studio.marked_range"), "a required key"),
      "{target:?}",
    );
    // An item holds its effects as `Effect`s, each with the keys its reader
    // requires.
    let effects =
      |entry: Value| move |root: &mut Value| set(root, A, "effects", Value::Array(vec![entry]));
    assert_eq!(
      checked_edited(target, effects(schema("Effect.1", vec![]))),
      refused(&format!("{clip}.effects[0].effect_name"), "a required key"),
      "{target:?}",
    );
    assert_eq!(
      checked_edited(
        target,
        effects(schema("LinearTimeWarp.1", vec![("effect_name", text("w"))]))
      ),
      refused(&format!("{clip}.effects[0].time_scalar"), "a required key"),
      "{target:?}",
    );
    assert_eq!(
      checked_edited(target, effects(schema("Marker.2", vec![]))),
      refused(
        &format!("{clip}.effects[0].OTIO_SCHEMA"),
        "an effect schema"
      ),
      "{target:?}",
    );
    let frozen = schema(
      "FreezeFrame.1",
      vec![
        ("effect_name", text("f")),
        ("time_scalar", number("0.0")),
        ("metadata", object(vec![("at", rational())])),
      ],
    );
    assert_eq!(
      checked_edited(target, effects(frozen)),
      Ok(()),
      "{target:?}"
    );
    // The value schemas a reader decodes, each with every key it fetches.
    let red = || {
      schema(
        "Color.1",
        vec![
          ("name", text("red")),
          ("r", number("1.0")),
          ("g", number("0")),
          ("b", number("0")),
          ("a", number("1")),
        ],
      )
    };
    let point = |keys: &[&str]| {
      schema(
        "V2d.1",
        keys.iter().map(|key| (*key, number("0.5"))).collect(),
      )
    };
    let warp = schema(
      "TimeTransform.1",
      vec![
        ("offset", rational()),
        ("rate", number("24.0")),
        ("scale", number("1.0")),
      ],
    );
    for value in [red(), point(&["x", "y"]), warp] {
      assert_eq!(checked_edited(target, studio(value)), Ok(()), "{target:?}");
    }
    let mut unlit = red();
    remove(&mut unlit, "", "a");
    assert_eq!(
      checked_edited(target, studio(unlit)),
      refused("$.metadata.studio.a", "a required key"),
      "{target:?}",
    );
    assert_eq!(
      checked_edited(target, studio(point(&["x"]))),
      refused("$.metadata.studio.y", "a required key"),
      "{target:?}",
    );
  }
  // Under Miri, where the cases above read for one target, `Legacy` reads
  // one of them: its `Clip.1` holds an item's effects as `Clip.2` does.
  if cfg!(miri) {
    assert_eq!(
      checked_edited(OtioTarget::Legacy, |root| set(
        root,
        A,
        "effects",
        Value::Array(vec![schema("Effect.1", vec![])])
      )),
      refused(&format!("{clip}.effects[0].effect_name"), "a required key"),
    );
  }
  // From 0.15 a reader reads a media reference's image bounds as a
  // `Box2d`; before, as a key it does not know.
  let bounds =
    |value: Value| move |root: &mut Value| set(root, A_MEDIA, "available_image_bounds", value);
  assert_eq!(
    checked_edited(OtioTarget::V0_15Plus, bounds(object(vec![]))),
    refused(
      &format!("{clip}.media_references.DEFAULT_MEDIA.available_image_bounds.OTIO_SCHEMA"),
      "a schema name"
    ),
  );
  let corner = || schema("V2d.1", vec![("x", number("0")), ("y", number("0"))]);
  let framed = schema("Box2d.1", vec![("min", corner()), ("max", corner())]);
  assert_eq!(
    checked_edited(OtioTarget::V0_15Plus, bounds(framed)),
    Ok(())
  );
  assert_eq!(
    checked_edited(OtioTarget::Legacy, |root| set(
      root,
      &format!("{A}.media_reference"),
      "available_image_bounds",
      object(vec![])
    )),
    Ok(()),
  );
}

/// Where the sample's clip `a` keeps its medium, for `target`.
fn a_media(target: OtioTarget) -> String {
  match target {
    OtioTarget::V0_15Plus => String::from(A_MEDIA),
    OtioTarget::Legacy => format!("{A}.media_reference"),
  }
}

/// The checker's path for the dotted `path`: `$.`, and each index in
/// brackets.
fn checked_path(path: &str) -> String {
  let mut out = String::from("$");
  for step in path.split('.').filter(|step| !step.is_empty()) {
    if step.bytes().all(|byte| byte.is_ascii_digit()) {
      out.push_str(&format!("[{step}]"));
    } else {
      out.push('.');
      out.push_str(step);
    }
  }
  out
}

#[test]
fn a_dictionary_of_media_references_carrying_otio_schema_is_refused_by_name() {
  // Round 5: a reader decodes `media_references` as it closes it, and a
  // member named `OTIO_SCHEMA` makes it an object of a schema, its
  // `OTIO_SCHEMA` fetched as a string — here an object, which it refuses.
  let external = object(vec![
    ("OTIO_SCHEMA", text("ExternalReference.1")),
    ("target_url", text("file:///other.mov")),
  ]);
  assert_eq!(
    checked_edited(OtioTarget::V0_15Plus, |root| set(
      root,
      &format!("{A}.media_references"),
      "OTIO_SCHEMA",
      external
    )),
    Err((
      checked_path(&format!("{A}.media_references.OTIO_SCHEMA")),
      "no schema: media_references is a dictionary"
    )),
  );
}

#[test]
fn a_generator_reference_is_held_to_its_readers_members() {
  // `generatorReference.cpp` 26–27 in 0.17.0: `generator_kind`, a string,
  // and `parameters`, a dictionary, both required.
  for &target in TARGETS {
    let media = a_media(target);
    let at = |key: &str| checked_path(&format!("{media}.{key}"));
    let generator = |members: Vec<(&str, Value)>| {
      let mut members = members;
      members.insert(0, ("OTIO_SCHEMA", text("GeneratorReference.1")));
      object(members)
    };
    let refused = |reference: Value| {
      checked_edited(target, |root| {
        let (parent, key) = media.rsplit_once('.').expect("a dotted path");
        set(root, parent, key, reference);
      })
    };
    // Round 5's counterexample: a generator on a reference's members alone.
    assert_eq!(
      refused(generator(vec![])),
      Err((at("generator_kind"), "a required key")),
      "{target:?}",
    );
    assert_eq!(
      refused(generator(vec![("generator_kind", text("solid"))])),
      Err((at("parameters"), "a required key")),
      "{target:?}",
    );
    assert_eq!(
      refused(generator(vec![
        ("generator_kind", number("1")),
        ("parameters", object(vec![])),
      ])),
      Err((at("generator_kind"), "a string")),
      "{target:?}",
    );
    assert_eq!(
      refused(generator(vec![
        ("generator_kind", text("solid")),
        ("parameters", text("red")),
      ])),
      Err((at("parameters"), "an object")),
      "{target:?}",
    );
    assert_eq!(
      refused(generator(vec![
        ("generator_kind", text("solid")),
        (
          "parameters",
          object(vec![("OTIO_SCHEMA", text("Marker.2"))]),
        ),
      ])),
      Err((
        at("parameters.OTIO_SCHEMA"),
        "no schema: parameters is a dictionary"
      )),
      "{target:?}",
    );
    // Its parameters are walked as any dictionary is.
    assert_eq!(
      refused(generator(vec![
        ("generator_kind", text("solid")),
        (
          "parameters",
          object(vec![(
            "fill",
            object(vec![("OTIO_SCHEMA", text("Marker.2"))]),
          )]),
        ),
      ])),
      Err((at("parameters.fill.marked_range"), "a required key")),
      "{target:?}",
    );
    assert_eq!(
      refused(generator(vec![
        ("generator_kind", text("solid")),
        ("parameters", object(vec![("color", text("red"))])),
      ])),
      Ok(()),
      "{target:?}",
    );
  }
}

#[test]
fn an_image_sequence_reference_is_held_to_its_readers_members() {
  // `imageSequenceReference.cpp` 195–233 in 0.17.0: three strings, three
  // `int64_t`s, a `double` and a policy of three, each required.
  let sequence = || {
    vec![
      ("OTIO_SCHEMA", text("ImageSequenceReference.1")),
      ("target_url_base", text("file:///frames/")),
      ("name_prefix", text("a.")),
      ("name_suffix", text(".exr")),
      ("start_frame", number("1001")),
      ("frame_step", number("1")),
      ("rate", number("24")),
      ("frame_zero_padding", number("4")),
      ("missing_frame_policy", text("hold")),
    ]
  };
  for &target in TARGETS {
    let media = a_media(target);
    let at = |key: &str| checked_path(&format!("{media}.{key}"));
    let checked = |members: Vec<(&str, Value)>| {
      checked_edited(target, |root| {
        let (parent, key) = media.rsplit_once('.').expect("a dotted path");
        set(root, parent, key, object(members));
      })
    };
    assert_eq!(checked(sequence()), Ok(()), "{target:?}");
    // Each required member, left out.
    for (index, (key, _)) in alike(sequence().into_iter().enumerate().skip(1)) {
      let mut members = sequence();
      members.remove(index);
      assert_eq!(
        checked(members),
        Err((at(key), "a required key")),
        "{target:?} {key}",
      );
    }
    // Each held to its type.
    let with = |key: &str, value: Value| {
      let mut members = sequence();
      for member in &mut members {
        if member.0 == key {
          member.1 = value.clone();
        }
      }
      checked(members)
    };
    let whole = "a whole number, without a fraction or an exponent";
    // An integer, as RapidJSON reads one: `-0` among them, and every one up
    // to 2^64 - 1, which the handler keeps cut to its low 63 bits — between
    // them each of its four integer callbacks, so every row runs under Miri
    // too.
    for spelling in [
      "-0",
      "0",
      "-9223372036854775808",
      "9223372036854775807",
      "9223372036854775808",
      "18446744073709551615",
    ] {
      assert_eq!(
        with("start_frame", number(spelling)),
        Ok(()),
        "{target:?} {spelling}",
      );
    }
    // A double, which the reader refuses there: a fraction, an exponent,
    // and an integer past 64 bits — each the one double callback.
    for spelling in alike([
      "1001.0",
      "1.001e3",
      "-0.0",
      "18446744073709551616",
      "-9223372036854775809",
      "NaN",
    ]) {
      assert_eq!(
        with("start_frame", number(spelling)),
        Err((at("start_frame"), whole)),
        "{target:?} {spelling}",
      );
    }
    assert_eq!(
      with("frame_step", text("1")),
      Err((at("frame_step"), whole)),
      "{target:?}",
    );
    assert_eq!(
      with("frame_zero_padding", number("4.5")),
      Err((at("frame_zero_padding"), whole)),
      "{target:?}",
    );
    assert_eq!(with("rate", number("23.976")), Ok(()), "{target:?}");
    assert_eq!(
      with("rate", text("24")),
      Err((at("rate"), "a number")),
      "{target:?}",
    );
    assert_eq!(
      with("name_prefix", number("5")),
      Err((at("name_prefix"), "a string")),
      "{target:?}",
    );
    for policy in alike(["error", "black", "hold"]) {
      assert_eq!(
        with("missing_frame_policy", text(policy)),
        Ok(()),
        "{target:?} {policy}"
      );
    }
    assert_eq!(
      with("missing_frame_policy", text("skip")),
      Err((at("missing_frame_policy"), "error, black or hold")),
      "{target:?}",
    );
  }
}

#[test]
fn a_missing_reference_reads_a_references_members_alone() {
  // `missingReference.cpp` 28 in 0.17.0: nothing of its own.
  for target in [OtioTarget::V0_15Plus, OtioTarget::Legacy] {
    let media = a_media(target);
    let checked = |members: Vec<(&str, Value)>| {
      checked_edited(target, |root| {
        let (parent, key) = media.rsplit_once('.').expect("a dotted path");
        set(root, parent, key, object(members));
      })
    };
    assert_eq!(
      checked(vec![("OTIO_SCHEMA", text("MissingReference.1"))]),
      Ok(()),
      "{target:?}",
    );
    assert_eq!(
      checked(vec![
        ("OTIO_SCHEMA", text("MissingReference.1")),
        ("name", number("5")),
      ]),
      Err((checked_path(&format!("{media}.name")), "a string")),
      "{target:?}",
    );
  }
}

#[test]
fn an_objects_reference_id_is_a_string_no_other_object_carries() {
  // Round 6: a reader fetches an object's `OTIO_REF_ID` as a string and
  // refuses a nonempty one an object it made before carries
  // (`deserialization.cpp` 624–642, 669–672 in 0.17.0); it makes the
  // stack before the timeline that holds it.
  let id = |root: &mut Value, path: &str, value: Value| set(root, path, "OTIO_REF_ID", value);
  for &target in TARGETS {
    assert_eq!(
      checked_edited(target, |root| id(root, "", number("7"))),
      Err((String::from("$.OTIO_REF_ID"), "a string")),
      "{target:?}: an integer id",
    );
    assert_eq!(
      checked_edited(target, |root| id(root, A, Value::Null)),
      Err((checked_path(&format!("{A}.OTIO_REF_ID")), "a string")),
      "{target:?}: a null id",
    );
    assert_eq!(
      checked_edited(target, |root| {
        id(&mut *root, "", text("one"));
        id(root, "tracks", text("one"));
      }),
      Err((
        String::from("$.OTIO_REF_ID"),
        "an id no other object carries"
      )),
      "{target:?}: the timeline's id, the stack's before it",
    );
    assert_eq!(
      checked_edited(target, |root| {
        for path in ["", "tracks", TRACK, A] {
          id(&mut *root, path, text(""));
        }
      }),
      Ok(()),
      "{target:?}: empty ids, none registered",
    );
    assert_eq!(
      checked_edited(target, |root| {
        for (path, name) in [("", "t"), ("tracks", "s"), (TRACK, "k"), (A, "a"), (B, "b")] {
          id(&mut *root, path, text(name));
        }
      }),
      Ok(()),
      "{target:?}: unique ids",
    );
    // A value schema's id is neither read nor registered.
    assert_eq!(
      checked_edited(target, |root| {
        id(&mut *root, "global_start_time", number("7"));
        id(&mut *root, &format!("{A}.source_range"), text("one"));
        id(root, "", text("one"));
      }),
      Ok(()),
      "{target:?}: ids on a RationalTime.1 and a TimeRange.1",
    );
  }
  // A schema a reader of the target decodes as a value and another as an
  // object is held as the object: `Color.1` before 0.18, `V2d.1` before
  // 0.15.
  let colored = |root: &mut Value| {
    set(
      root,
      "metadata",
      "studio",
      object(vec![
        ("OTIO_SCHEMA", text("Color.1")),
        ("name", text("red")),
        ("r", number("1")),
        ("g", number("0")),
        ("b", number("0")),
        ("a", number("1")),
        ("OTIO_REF_ID", number("7")),
      ]),
    )
  };
  assert_eq!(
    checked_edited(OtioTarget::V0_15Plus, colored),
    Err((String::from("$.metadata.studio.OTIO_REF_ID"), "a string")),
  );
  let point = |root: &mut Value| {
    set(
      root,
      "metadata",
      "studio",
      object(vec![
        ("OTIO_SCHEMA", text("V2d.1")),
        ("x", number("0")),
        ("y", number("0")),
        ("OTIO_REF_ID", number("7")),
      ]),
    )
  };
  assert_eq!(checked_edited(OtioTarget::V0_15Plus, point), Ok(()));
  assert_eq!(
    checked_edited(OtioTarget::Legacy, point),
    Err((String::from("$.metadata.studio.OTIO_REF_ID"), "a string")),
  );
}

#[test]
fn notes_that_are_not_pairs_of_strings_each_key_once_are_refused_by_name() {
  let notes = |root: &mut Value, value: Value| {
    set(root, "metadata.mediaio", "metadata", value);
  };
  let pair = |key: &str, value: &str| Value::Array(vec![text(key), text(value)]);
  assert_eq!(
    read(|root| notes(root, object(vec![("project", text("law"))]))),
    Err(shape("metadata.mediaio.metadata", "an array")),
  );
  assert_eq!(
    read(|root| notes(
      root,
      Value::Array(vec![Value::Array(vec![text("only a key")])])
    )),
    Err(shape(
      "metadata.mediaio.metadata.0",
      "a note: a key and a value, both strings"
    )),
  );
  assert_eq!(
    read(|root| {
      notes(
        root,
        Value::Array(vec![Value::Array(vec![text("key"), number("1")])]),
      )
    }),
    Err(shape(
      "metadata.mediaio.metadata.0",
      "a note: a key and a value, both strings"
    )),
  );
  assert_eq!(
    read(|root| notes(root, Value::Array(vec![pair("key", "a"), pair("key", "b")]))),
    Err(shape(
      "metadata.mediaio.metadata.1.0",
      "a key no earlier note names"
    )),
  );
  assert_eq!(
    read(|root| notes(root, Value::Array(vec![pair("key", "a")]))),
    Ok(sample().with_metadata(Metadata::new().with("key", "a"))),
  );
}

/// `num/den`, as a test spells a timebase.
fn timebase(num: i32, den: i32) -> Timebase {
  Timebase::new(num, core::num::NonZeroI32::new(den).expect("non-zero"))
}

/// The sample at a rate of `48/2` — 24 a second, spelled unreduced — whose
/// edit-side values spell its reciprocal `2/48` otherwise: the start in
/// `1/24`, `a`'s record in `4/96` and its fade in `3/72`, the dissolve's cut
/// in `1/24`, its offsets in `2/48` (the reciprocal's own spelling) and
/// `5/120`, and the track's marker in `1/24`.
fn respelled() -> Timeline {
  let rate = Rate::fps(48, core::num::NonZeroI32::new(2).expect("non-zero"));
  let mut timeline = sample();
  timeline.set_rate(rate);
  timeline.set_start(Timestamp::new(0, timebase(1, 24)));
  let track = &mut timeline.tracks_mut()[0];
  let a = &mut track.clips_mut()[0];
  a.set_record(TimeRange::new(12, 36, timebase(4, 96)));
  let fades = a.fades().with_in(Some(Fade::new(
    Duration::new(6, timebase(3, 72)),
    FadeShape::Linear,
  )));
  a.set_fades(fades);
  track.clips_mut()[1].set_record(TimeRange::new(36, 60, timebase(2, 48)));
  *track.transitions_mut() = vec![Transition::dissolve(
    Timestamp::new(36, timebase(1, 24)),
    Duration::new(2, timebase(2, 48)),
    Duration::new(2, timebase(5, 120)),
  )];
  track.markers_mut()[0].set_range(TimeRange::new(0, 24, timebase(1, 24)));
  timeline
}

/// A timebase's spelling, as `(num, den)`.
fn spelling(timebase: Timebase) -> (i32, i32) {
  (timebase.num(), timebase.den().get())
}

#[test]
fn an_edit_side_value_keeps_its_timebases_spelling() {
  let timeline = respelled();
  assert_eq!(
    crate::validate(&timeline),
    Ok(()),
    "the premise: it validates"
  );
  for &target in TARGETS {
    let written = to_otio(&timeline, target).unwrap();
    let read = from_otio(&written).unwrap();
    assert_eq!(read, timeline, "{target:?}");
    assert_eq!(spelling(read.start().timebase()), (1, 24), "the start");
    let track = &read.tracks()[0];
    let a = &track.clips()[0];
    assert_eq!(spelling(a.record().timebase()), (4, 96), "a's record");
    assert_eq!(
      spelling(a.fades().in_().expect("a's fade").duration().timebase()),
      (3, 72),
      "a's fade"
    );
    assert_eq!(
      spelling(track.clips()[1].record().timebase()),
      (2, 48),
      "b's record"
    );
    let dissolve = &track.transitions()[0];
    assert_eq!(spelling(dissolve.at().timebase()), (1, 24), "the cut");
    assert_eq!(spelling(dissolve.in_offset().timebase()), (2, 48), "in");
    assert_eq!(spelling(dissolve.out_offset().timebase()), (5, 120), "out");
    assert_eq!(
      spelling(track.markers()[0].range().timebase()),
      (1, 24),
      "the marker"
    );
    #[cfg(feature = "serde")]
    assert_eq!(
      serde_json::to_string(&read).unwrap(),
      serde_json::to_string(&timeline).unwrap(),
      "{target:?}: the stored document, byte for byte",
    );
  }
  // The words are written only where a spelling differs from the
  // reciprocal's.
  let mut root = json::parse(&to_otio(&timeline, OtioTarget::V0_15Plus).unwrap()).unwrap();
  assert_eq!(at(&mut root, "metadata.mediaio.start"), &text("1/24"));
  assert_eq!(
    at(&mut root, &format!("{A}.metadata.mediaio.record")),
    &text("4/96")
  );
  assert_eq!(
    at(&mut root, &format!("{FADE_IN}.metadata.mediaio.timebase")),
    &text("3/72")
  );
  let words = at(&mut root, &format!("{DISSOLVE}.metadata.mediaio"));
  let Value::Object(words) = words else {
    panic!("the dissolve's words: {words:?}")
  };
  let keys: Vec<&str> = words.iter().map(|(key, _)| key.as_str()).collect();
  assert_eq!(
    keys,
    ["at", "out_offset"],
    "in_offset spells the reciprocal"
  );
  let b_words = at(&mut root, &format!("{B}.metadata.mediaio"));
  let Value::Object(b_words) = b_words else {
    panic!("b's words: {b_words:?}")
  };
  assert!(
    b_words.iter().all(|(key, _)| key != "record"),
    "b's record spells the reciprocal"
  );
  assert_eq!(
    at(
      &mut root,
      &format!("{TRACK}.markers.0.metadata.mediaio.timebase")
    ),
    &text("1/24")
  );
}

#[test]
fn a_timebase_word_off_the_edit_rate_is_refused_by_name() {
  assert_eq!(
    read(|root| set(root, "metadata.mediaio", "start", text("1/25"))),
    Err(shape(
      "metadata.mediaio.start",
      "the edit rate's timebase, written num/den"
    )),
  );
  assert_eq!(
    read(|root| set(root, &format!("{A}.metadata.mediaio"), "record", text("x"))),
    Err(shape(
      &format!("{A}.metadata.mediaio.record"),
      "the edit rate's timebase, written num/den"
    )),
  );
  // An equal timebase spelled otherwise reads, in its own spelling.
  let equal = read(|root| {
    set(
      root,
      &format!("{A}.metadata.mediaio"),
      "record",
      text("2/48"),
    )
  })
  .expect("an equal timebase");
  assert_eq!(
    spelling(equal.tracks()[0].clips()[0].record().timebase()),
    (2, 48)
  );
  // A word of the other kind of transition.
  assert_eq!(
    read_edited(OtioTarget::V0_15Plus, |root| {
      set(
        &mut *root,
        &format!("{DISSOLVE}.metadata"),
        "mediaio",
        object(vec![]),
      );
      set(
        root,
        &format!("{DISSOLVE}.metadata.mediaio"),
        "timebase",
        text("1/24"),
      );
    }),
    Err(feature(
      Feature::Word("timebase".into()),
      &format!("{DISSOLVE}.metadata.mediaio.timebase")
    )),
  );
  assert_eq!(
    read(|root| set(
      root,
      &format!("{FADE_IN}.metadata.mediaio"),
      "at",
      text("1/24")
    )),
    Err(feature(
      Feature::Word("at".into()),
      &format!("{FADE_IN}.metadata.mediaio.at")
    )),
  );
}

// Members OpenTimelineIO 0.17.0's schema lets a document leave out read as
// its reader reads them: it builds each object with its default
// constructor (`typeRegistry.h` 57–65) and reads such a member only if
// present. One law per member.

#[test]
fn an_absent_name_reads_as_empty() {
  // `serializableObjectWithMetadata.cpp` 22: an empty name. The stack's,
  // which the export writes `tracks`, may not be left out (round 10).
  let (track_marker, clip_marker) = (format!("{TRACK}.markers.0"), format!("{A}.markers.0"));
  let read = read(|root| {
    for path in [
      "",
      TRACK,
      GAP,
      FADE_IN,
      A,
      DISSOLVE,
      B,
      A_MEDIA,
      track_marker.as_str(),
      clip_marker.as_str(),
    ] {
      remove(root, path, "name");
    }
  });
  let mut nameless = sample();
  nameless.set_name("");
  let track = &mut nameless.tracks_mut()[0];
  track.set_name("");
  track.markers_mut()[0].set_name("");
  for clip in track.clips_mut() {
    clip.set_name("");
  }
  track.clips_mut()[0].markers_mut()[0].set_name("");
  assert_eq!(read, Ok(nameless));
}

#[test]
fn absent_metadata_reads_as_no_note_and_a_needed_word_is_refused_by_name() {
  // `serializableObjectWithMetadata.cpp` 21: an empty dictionary — where
  // the timeline needs none of its words, nothing.
  let track_marker = format!("{TRACK}.markers.0");
  assert_eq!(
    read(|root| {
      for path in ["tracks", TRACK, GAP, DISSOLVE, track_marker.as_str()] {
        remove(root, path, "metadata");
      }
    }),
    Ok(sample()),
  );
  // Where it needs one, the word is refused by name.
  assert_eq!(
    read(|root| remove(root, "", "metadata")),
    Err(shape("metadata.mediaio.rate", "a required word")),
  );
  assert_eq!(
    read(|root| remove(root, A, "metadata")),
    Err(shape(
      &format!("{A}.metadata.mediaio.id"),
      "a required word"
    )),
  );
}

#[test]
fn an_items_absent_source_range_is_no_trim_on_a_track_or_refused_for_a_gap_or_a_clip() {
  // `item.cpp` 175: none — no trim, on the stack and a track.
  assert_eq!(
    read(|root| {
      remove(root, "tracks", "source_range");
      remove(root, TRACK, "source_range");
    }),
    Ok(sample()),
  );
  // A gap's is its constructor's `TimeRange()` (`gap.h` 23 in 0.17.0):
  // nothing, at rate 1 — a rate the timeline does not keep, which the
  // track's duration takes. Refused in every position: the sample's leading
  // gap of some length, and the gap of no length the export writes after a
  // fade-out at the track's end.
  assert_eq!(
    read(|root| remove(root, GAP, "source_range")),
    Err(shape(&format!("{GAP}.source_range"), "a required key")),
  );
  let mut faded = sample();
  let b = &mut faded.tracks_mut()[0].clips_mut()[1];
  let fades = b
    .fades()
    .with_out(Some(Fade::new(Duration::new(4, edit()), FadeShape::Linear)));
  b.set_fades(fades);
  let mut root = json::parse(&to_otio(&faded, OtioTarget::V0_15Plus).unwrap()).unwrap();
  let last = format!("{TRACK}.children.6");
  assert_eq!(
    at(&mut root, &format!("{last}.OTIO_SCHEMA")),
    &text("Gap.1"),
    "the premise: the gap after the fade-out"
  );
  remove(&mut root, &last, "source_range");
  let mut out = Utf8BytesMut::new();
  json::write_pretty(&root, &mut out);
  assert_eq!(
    from_otio(&out.freeze_shared()),
    Err(shape(&format!("{last}.source_range"), "a required key")),
  );
  // A clip's is none, which OpenTimelineIO plays as its medium's whole
  // available range — a range the document's words do not count.
  assert_eq!(
    read(|root| remove(root, A, "source_range")),
    Err(shape(&format!("{A}.source_range"), "a required key")),
  );
}

#[test]
fn absent_effects_and_markers_read_as_none() {
  // `item.cpp` 176–177: none.
  assert_eq!(
    read(|root| {
      for path in ["tracks", TRACK, GAP, A, B] {
        remove(root, path, "effects");
      }
      for path in ["tracks", GAP, B] {
        remove(root, path, "markers");
      }
    }),
    Ok(sample()),
  );
}

#[test]
fn an_items_absent_enabled_reads_as_enabled() {
  // `item.cpp` 178; `item.h` 33: `true`.
  assert_eq!(
    read(|root| {
      for path in ["tracks", TRACK, GAP, A, B] {
        remove(root, path, "enabled");
      }
    }),
    Ok(sample()),
  );
}

#[test]
fn a_markers_absent_color_reads_and_writes_back_as_green() {
  // `marker.cpp` 27 reads the color only if present, over the default
  // constructor's `Color::green` (`marker.h` 37–42). Python's binding
  // defaults its own constructor to RED, which no reader runs.
  let read = read(|root| {
    remove(root, &format!("{TRACK}.markers.0"), "color");
    remove(root, &format!("{A}.markers.0"), "color");
  })
  .unwrap();
  let track = &read.tracks()[0];
  assert_eq!(track.markers()[0].color(), MarkerColor::Green);
  assert_eq!(track.clips()[0].markers()[0].color(), MarkerColor::Green);
  let mut root = json::parse(&to_otio(&read, OtioTarget::V0_15Plus).unwrap()).unwrap();
  assert_eq!(
    at(&mut root, &format!("{TRACK}.markers.0.color")),
    &text("GREEN"),
  );
}

#[test]
fn a_markers_absent_comment_reads_as_empty() {
  // `marker.cpp` 29; `marker.h` 42: empty.
  let mut commented = sample();
  commented.tracks_mut()[0].markers_mut()[0].set_comment("a note");
  let mut root = json::parse(&to_otio(&commented, OtioTarget::V0_15Plus).unwrap()).unwrap();
  remove(&mut root, &format!("{TRACK}.markers.0"), "comment");
  let mut out = Utf8BytesMut::new();
  json::write_pretty(&root, &mut out);
  assert_eq!(from_otio(&out.freeze_shared()), Ok(sample()));
}

#[test]
fn a_references_absent_ranges_read_as_none() {
  // `mediaReference.cpp` 30–33: none — the available range, its word
  // with it, and the image bounds.
  let read = read(|root| {
    remove(root, A_MEDIA, "available_range");
    remove(root, &format!("{A_MEDIA}.metadata.mediaio"), "available");
    remove(root, A_MEDIA, "available_image_bounds");
  })
  .unwrap();
  let media = read.tracks()[0].clips()[0].media();
  assert_eq!(media.available_range(), None);
  let mut expected = sample();
  let a = &mut expected.tracks_mut()[0].clips_mut()[0];
  let medium = a.media().clone().with_available_range(None);
  a.set_media(medium);
  assert_eq!(read, expected);
}

#[test]
fn an_absent_global_start_time_is_refused_by_name() {
  // `timeline.cpp` 31; `timeline.h` 28: none — no time, where the
  // timeline always starts somewhere.
  assert_eq!(
    read(|root| remove(root, "", "global_start_time")),
    Err(shape("global_start_time", "a required key")),
  );
}

#[test]
fn a_clip_1_without_a_media_reference_is_a_clip_with_no_medium() {
  // OpenTimelineIO upgrades it to a `Clip.2` holding a `MissingReference`
  // (`typeRegistry.cpp` 99–117), as it does a `null` one.
  assert_eq!(
    read_edited(OtioTarget::Legacy, |root| remove(
      root,
      A,
      "media_reference"
    )),
    Err(feature(Feature::NoMedia, &format!("{A}.media_reference"))),
  );
}

#[test]
fn a_member_opentimelineio_requires_is_refused_by_name() {
  // What 0.17.0 reads with `reader.read` and so refuses a document
  // without: a track's kind (`track.cpp` 34), a transition's offsets and
  // type (`transition.cpp` 33–35), a marker's range (`marker.cpp` 28), a
  // stack's and a track's children (`composition.cpp` 191), a reference's
  // URL (`externalReference.cpp` 23), a `Clip.2`'s references
  // (`clip.cpp` 141–144), a time's rate and value and a range's start
  // and duration (`deserialization.cpp` 578, 585–586).
  let cases: Vec<(String, &str)> = vec![
    (TRACK.into(), "kind"),
    (DISSOLVE.into(), "in_offset"),
    (DISSOLVE.into(), "out_offset"),
    (DISSOLVE.into(), "transition_type"),
    (format!("{TRACK}.markers.0"), "marked_range"),
    ("tracks".into(), "children"),
    (TRACK.into(), "children"),
    (A_MEDIA.into(), "target_url"),
    (A.into(), "media_references"),
    (A.into(), "active_media_reference_key"),
    (format!("{GAP}.source_range.duration"), "rate"),
    (format!("{GAP}.source_range.duration"), "value"),
    (format!("{GAP}.source_range"), "start_time"),
    (format!("{GAP}.source_range"), "duration"),
  ];
  for (path, key) in cases {
    assert_eq!(
      read(|root| remove(root, &path, key)),
      Err(shape(&format!("{path}.{key}"), "a required key")),
      "{path}.{key}",
    );
  }
}

#[test]
fn a_schema_the_timeline_does_not_model_is_refused_with_where_it_sits() {
  let effect = |name: &str| object(vec![("OTIO_SCHEMA", text(name))]);
  for name in ["Effect.1", "LinearTimeWarp.1", "FreezeFrame.1"] {
    assert_eq!(
      read(|root| set(root, A, "effects", Value::Array(vec![effect(name)]))),
      Err(schema(name, &format!("{A}.effects.0"))),
    );
  }
  // A stack inside a track, a clip directly in the stack.
  let nested = object(vec![
    ("OTIO_SCHEMA", text("Stack.1")),
    ("children", Value::Array(vec![])),
  ]);
  assert_eq!(
    read(|root| {
      let Value::Array(children) = at(root, &format!("{TRACK}.children")) else {
        unreachable!()
      };
      children.push(nested);
    }),
    Err(schema("Stack.1", &format!("{TRACK}.children.5"))),
  );
  assert_eq!(
    read(|root| {
      let clip = at(root, A).clone();
      let Value::Array(tracks) = at(root, "tracks.children") else {
        unreachable!()
      };
      tracks.push(clip);
    }),
    Err(schema("Clip.2", "tracks.children.1")),
  );
  // A reference other than an external one, a marker of another version,
  // a marker where the timeline keeps none, and a root that is no timeline.
  assert_eq!(
    read(|root| set(
      root,
      &format!("{A}.media_references"),
      "DEFAULT_MEDIA",
      effect("MissingReference.1")
    )),
    Err(schema("MissingReference.1", A_MEDIA)),
  );
  assert_eq!(
    read(|root| set(
      root,
      &format!("{A}.markers.0"),
      "OTIO_SCHEMA",
      text("Marker.3")
    )),
    Err(schema("Marker.3", &format!("{A}.markers.0"))),
  );
  assert_eq!(
    read(|root| {
      let marker = at(root, &format!("{TRACK}.markers.0")).clone();
      set(root, GAP, "markers", Value::Array(vec![marker]));
    }),
    Err(schema("Marker.2", &format!("{GAP}.markers.0"))),
  );
  assert_eq!(
    read(|root| set(root, "", "OTIO_SCHEMA", text("SerializableCollection.1"))),
    Err(schema("SerializableCollection.1", "")),
  );
}

/// An edit of a written document, for a law that tries many.
type Edit = Box<dyn FnOnce(&mut Value)>;

#[test]
fn a_feature_the_timeline_has_no_word_for_is_refused_by_name() {
  let cases: Vec<(Edit, Unreadable)> = vec![
    (
      Box::new(|root| set(root, A, "speed", number("2.0"))),
      feature(Feature::Key("speed".into()), &format!("{A}.speed")),
    ),
    (
      // A gap keeps no reader's namespace (the namespaces' laws below).
      Box::new(|root| {
        set(
          root,
          &format!("{GAP}.metadata"),
          "Resolve_OTIO",
          object(vec![]),
        )
      }),
      feature(
        Feature::Metadata("Resolve_OTIO".into()),
        &format!("{GAP}.metadata.Resolve_OTIO"),
      ),
    ),
    (
      Box::new(|root| set(root, &format!("{A}.metadata.mediaio"), "speed", number("2"))),
      feature(
        Feature::Word("speed".into()),
        &format!("{A}.metadata.mediaio.speed"),
      ),
    ),
    (
      Box::new(|root| set(root, GAP, "name", text("black"))),
      feature(Feature::Name("black".into()), &format!("{GAP}.name")),
    ),
    (
      Box::new(|root| set(root, "tracks", "name", text("Video Tracks"))),
      feature(Feature::Name("Video Tracks".into()), "tracks.name"),
    ),
    (
      Box::new(|root| set(root, TRACK, "kind", text("Data"))),
      feature(Feature::TrackKind("Data".into()), &format!("{TRACK}.kind")),
    ),
    (
      Box::new(|root| set(root, DISSOLVE, "transition_type", text("SMPTE_Wipe"))),
      feature(
        Feature::TransitionType("SMPTE_Wipe".into()),
        &format!("{DISSOLVE}.transition_type"),
      ),
    ),
    (
      Box::new(|root| {
        let range = at(root, &format!("{GAP}.source_range")).clone();
        set(root, TRACK, "source_range", range);
      }),
      feature(Feature::Trim, &format!("{TRACK}.source_range")),
    ),
    (
      Box::new(|root| set(root, GAP, "enabled", Value::Bool(false))),
      feature(Feature::Disabled, &format!("{GAP}.enabled")),
    ),
    (
      // A transition has no `enabled` in OpenTimelineIO 0.17.0 (round 10).
      Box::new(|root| set(root, DISSOLVE, "enabled", Value::Bool(false))),
      feature(
        Feature::Key(Utf8Bytes::from("enabled")),
        &format!("{DISSOLVE}.enabled"),
      ),
    ),
    (
      Box::new(|root| set(root, A, "color", text("RED"))),
      feature(Feature::ItemColor, &format!("{A}.color")),
    ),
    (
      Box::new(|root| set(root, &format!("{TRACK}.markers.0"), "color", text("LILAC"))),
      feature(
        Feature::MarkerColor("LILAC".into()),
        &format!("{TRACK}.markers.0.color"),
      ),
    ),
    (
      Box::new(|root| {
        set(
          root,
          A_MEDIA,
          "available_image_bounds",
          object(vec![("OTIO_SCHEMA", text("Box2d.1"))]),
        )
      }),
      feature(
        Feature::ImageBounds,
        &format!("{A_MEDIA}.available_image_bounds"),
      ),
    ),
    (
      Box::new(|root| {
        let reference = at(root, A_MEDIA).clone();
        set(
          root,
          &format!("{A}.media_references"),
          "high_res",
          reference,
        );
      }),
      feature(Feature::MediaReferences, &format!("{A}.media_references")),
    ),
    (
      Box::new(|root| {
        set(
          root,
          &format!("{GAP}.source_range.start_time"),
          "value",
          number("5.0"),
        )
      }),
      feature(Feature::GapStart, &format!("{GAP}.source_range.start_time")),
    ),
    (
      // The fade's words on the dissolve: a fade between two clips.
      Box::new(|root| {
        let words = at(root, &format!("{FADE_IN}.metadata")).clone();
        set(root, DISSOLVE, "metadata", words);
      }),
      feature(Feature::Fade, DISSOLVE),
    ),
    (
      // The fade's words taken off: a dissolve from the gap.
      Box::new(|root| set(root, FADE_IN, "metadata", object(vec![]))),
      feature(Feature::DissolveBesideGap, FADE_IN),
    ),
  ];
  for (index, (edit, refused)) in cases.into_iter().enumerate() {
    assert_eq!(read(edit), Err(refused), "case {index}");
  }
  // A `Clip.1` with no reference at all.
  assert_eq!(
    read_edited(OtioTarget::Legacy, |root| set(
      root,
      A,
      "media_reference",
      Value::Null
    )),
    Err(feature(Feature::NoMedia, &format!("{A}.media_reference"))),
  );
}

#[test]
fn a_gap_of_some_length_after_the_last_clip_is_refused() {
  // The sample's leading gap, `length` frames long.
  let gap = |root: &mut Value, length: &str| {
    let mut gap = at(root, GAP).clone();
    set(&mut gap, "source_range.duration", "value", number(length));
    gap
  };
  let push = |root: &mut Value, dotted: &str, child: Value| {
    let Value::Array(children) = at(root, dotted) else {
      unreachable!()
    };
    children.push(child);
  };
  // Ten frames after `b`, the track's last clip, which OpenTimelineIO
  // counts in the track's duration.
  assert_eq!(
    read(|root| {
      let trailing = gap(root, "10.0");
      push(root, &format!("{TRACK}.children"), trailing);
    }),
    Err(feature(
      Feature::TrailingGap,
      &format!("{TRACK}.children.5")
    )),
  );
  // A track of one gap, ten frames long, where the timeline's track would
  // hold no time.
  assert_eq!(
    read(|root| {
      let mut track = at(root, TRACK).clone();
      let only = gap(root, "10.0");
      set(&mut track, "", "children", Value::Array(vec![only]));
      set(&mut track, "", "markers", Value::Array(vec![]));
      push(root, "tracks.children", track);
    }),
    Err(feature(
      Feature::TrailingGap,
      "tracks.children.1.children.0"
    )),
  );
  // A gap of no length there is no fade's: OpenTimelineIO still counts it.
  assert_eq!(
    read(|root| {
      let empty = gap(root, "0.0");
      push(root, &format!("{TRACK}.children"), empty);
    }),
    Err(feature(
      Feature::StandaloneGap,
      &format!("{TRACK}.children.5")
    )),
  );
  // The export writes one after a fade-out at a track's end, and reads it
  // back.
  let mut faded = sample();
  let b = &mut faded.tracks_mut()[0].clips_mut()[1];
  let fades = b.fades().with_out(Some(Fade::new(
    Duration::new(4, edit()),
    FadeShape::EqualPower,
  )));
  b.set_fades(fades);
  for &target in TARGETS {
    let written = to_otio(&faded, target).unwrap();
    let mut root = json::parse(&written).unwrap();
    // `[gap, fade in, a, dissolve, b, fade out, gap]`.
    let last = format!("{TRACK}.children.6");
    assert_eq!(
      at(&mut root, &format!("{last}.OTIO_SCHEMA")),
      &text("Gap.1"),
      "{target:?}"
    );
    assert_eq!(
      at(&mut root, &format!("{last}.source_range.duration.value")),
      &number("0.0"),
      "{target:?}"
    );
    assert_eq!(from_otio(&written), Ok(faded.clone()), "{target:?}");
  }
}

/// Two clips end to end from zero on one track, `a` at [0, 24) and `b` at
/// [24, 48), each fading as `fades` says: `a`'s in and out, then `b`'s.
fn abutting(fades: [bool; 4]) -> Timeline {
  let fade = |on: bool| on.then(|| Fade::new(Duration::new(6, edit()), FadeShape::Linear));
  let clip = |name: &str, start: i64, fade_in: bool, fade_out: bool| {
    Clip::new(
      ClipId::new(name),
      name,
      MediaRef::new(smol_bytes::format_utf8bytes!("file:///{name}.mov"))
        .with_available_range(Some(TimeRange::new(0, 100, edit())))
        .with_rate(Some(Rate::FPS_24)),
      TimeRange::new(10, 34, edit()),
      TimeRange::new(start, start + 24, edit()),
    )
    .with_fades(Fades::new().with_in(fade(fade_in)).with_out(fade(fade_out)))
  };
  Timeline::new("abutting", Rate::FPS_24).with_track(
    Track::new(TrackKind::Video, "V")
      .with_clip(clip("a", 0, fades[0], fades[1]))
      .with_clip(clip("b", 24, fades[2], fades[3])),
  )
}

#[test]
#[cfg_attr(
  miri,
  ignore = "too heavy for Miri on a 32-bit target; the laws Miri runs reach its every branch"
)]
fn a_gap_of_no_length_is_read_only_where_the_export_writes_one() {
  // Round 7: OpenTimelineIO counts a gap of no length all the same — its
  // range is the item's duration (`item.cpp` 43–46 in 0.17.0), and its rate
  // joins the track's sum, which starts at rate 1 and takes the higher of
  // two (`track.cpp` 111–141; `rationalTime.h` 283–294) — so one the
  // timeline has no place for is refused by name.
  //
  // Codex's input: a timeline at 4/3 starting 2^52 ticks in, one track
  // holding one such gap alone. Read, it would leave the track empty, which
  // the export derives at rate 1 and could not write back.
  let rate = Rate::fps(4, core::num::NonZeroI32::new(3).expect("non-zero"));
  let lone = Timeline::new("lone", rate).with_track(Track::new(TrackKind::Video, "V"));
  let mut root = json::parse(&to_otio(&lone, OtioTarget::V0_15Plus).unwrap()).unwrap();
  set(
    &mut root,
    "global_start_time",
    "value",
    number("4503599627370496.0"),
  );
  let edit_rate = at(&mut root, "global_start_time.rate").clone();
  let zero = || {
    object(vec![
      ("OTIO_SCHEMA", text("RationalTime.1")),
      ("rate", edit_rate.clone()),
      ("value", number("0.0")),
    ])
  };
  let gap = object(vec![
    ("OTIO_SCHEMA", text("Gap.1")),
    (
      "source_range",
      object(vec![
        ("OTIO_SCHEMA", text("TimeRange.1")),
        ("duration", zero()),
        ("start_time", zero()),
      ]),
    ),
  ]);
  set(
    &mut root,
    "tracks.children.0",
    "children",
    Value::Array(vec![gap]),
  );
  let mut out = Utf8BytesMut::new();
  json::write_pretty(&root, &mut out);
  assert_eq!(
    from_otio(&out.freeze_shared()),
    Err(feature(
      Feature::StandaloneGap,
      "tracks.children.0.children.0"
    )),
    "Codex's input",
  );

  // The sample's leading gap, of no length.
  let empty = |root: &mut Value| {
    let mut gap = at(root, GAP).clone();
    set(&mut gap, "source_range.duration", "value", number("0.0"));
    gap
  };
  // Alone, on a track with no clip.
  assert_eq!(
    read(|root| {
      let mut track = at(root, TRACK).clone();
      let only = empty(root);
      set(&mut track, "", "children", Value::Array(vec![only]));
      set(&mut track, "", "markers", Value::Array(vec![]));
      let Value::Array(tracks) = at(root, "tracks.children") else {
        unreachable!()
      };
      tracks.push(track);
    }),
    Err(feature(
      Feature::StandaloneGap,
      "tracks.children.1.children.0"
    )),
    "alone on a track",
  );
  // Mid-track, between `a` and `b`, where the dissolve was.
  assert_eq!(
    read(|root| {
      let gap = empty(root);
      let Value::Array(children) = at(root, &format!("{TRACK}.children")) else {
        unreachable!()
      };
      children[3] = gap;
    }),
    Err(feature(
      Feature::StandaloneGap,
      &format!("{TRACK}.children.3")
    )),
    "mid-track",
  );
  // On a fade's gap side, but beside another gap: the export writes no gap
  // of no length after one of some length.
  assert_eq!(
    read(|root| {
      let gap = empty(root);
      let Value::Array(children) = at(root, &format!("{TRACK}.children")) else {
        unreachable!()
      };
      children.insert(1, gap);
    }),
    Err(feature(
      Feature::StandaloneGap,
      &format!("{TRACK}.children.1")
    )),
    "beside another gap",
  );

  // The fade encoding reads back: a gap of no length before a fade-in at a
  // track's start, after a fade-out at its end, between a fade-out and a
  // fade-in, and before a fade-in after a clip that does not fade out.
  for (fades, children) in [
    ([true, false, false, false], 4),
    ([false, false, false, true], 4),
    ([false, true, true, false], 5),
    ([false, false, true, false], 4),
  ] {
    let timeline = abutting(fades);
    assert_eq!(crate::validate(&timeline), Ok(()), "{fades:?}: the premise");
    for target in [OtioTarget::V0_15Plus, OtioTarget::Legacy] {
      let written = to_otio(&timeline, target).unwrap();
      let mut root = json::parse(&written).unwrap();
      let Value::Array(items) = at(&mut root, &format!("{TRACK}.children")) else {
        unreachable!()
      };
      assert_eq!(items.len(), children, "{fades:?} {target:?}: the premise");
      let read = from_otio(&written);
      assert_eq!(read, Ok(timeline.clone()), "{fades:?} {target:?}");
      // Re-exported, each track lasts strictly as long as the document's
      // does, its rate — the edit rate — included.
      let again = to_otio(&read.unwrap(), target).unwrap();
      assert_eq!(
        track_durations(&written),
        vec![(48.0, 24.0)],
        "{fades:?} {target:?}: the premise"
      );
      assert_eq!(
        track_durations(&again),
        track_durations(&written),
        "{fades:?} {target:?}"
      );
    }
  }
}

/// Each track's duration as OpenTimelineIO 0.17.0 sums it from the document
/// `text` — `Track::available_range` (`track.cpp` 111–141): from
/// `RationalTime()`, nothing at rate 1 (`rationalTime.h` 43), each item's
/// `source_range.duration` — a gap's `TimeRange()` (`gap.h` 23) where it
/// states none — then a first transition's `in_offset` and a last one's
/// `out_offset`, each added by `operator+=` (`rationalTime.h` 283–294) —
/// as `(value, rate)`: what `RationalTime::strictly_equal` compares
/// (101–104).
fn track_durations(text: &str) -> Vec<(f64, f64)> {
  let root = json::parse(text).unwrap();
  let member = |value: &Value, key: &str| {
    value
      .as_object()
      .and_then(|members| members.iter().find(|(name, _)| name == key))
      .map(|(_, value)| value.clone())
  };
  let time = |value: &Value| {
    let number = |key| {
      member(value, key)
        .and_then(|number| number.as_f64())
        .unwrap()
    };
    (number("value"), number("rate"))
  };
  // `value_rescaled_to` (`rationalTime.h` 75–78), then `operator+=`.
  let rescaled = |(value, rate): (f64, f64), to: f64| {
    if to == rate {
      value
    } else {
      (value * to) / rate
    }
  };
  let add = |sum: (f64, f64), other: (f64, f64)| {
    if sum.1 < other.1 {
      (other.0 + rescaled(sum, other.1), other.1)
    } else {
      (sum.0 + rescaled(other, sum.1), sum.1)
    }
  };
  let transition = |child: &Value| {
    member(child, "OTIO_SCHEMA")
      .as_ref()
      .and_then(Value::as_str)
      == Some("Transition.1")
  };
  let tracks = member(&root, "tracks")
    .and_then(|stack| member(&stack, "children"))
    .unwrap();
  tracks
    .as_array()
    .unwrap()
    .iter()
    .map(|track| {
      let children = member(track, "children").unwrap();
      let children = children.as_array().unwrap();
      let mut sum = (0.0, 1.0);
      for child in children.iter().filter(|child| !transition(child)) {
        let duration = member(child, "source_range").map_or((0.0, 1.0), |range| {
          time(&member(&range, "duration").unwrap())
        });
        sum = add(sum, duration);
      }
      if let Some(first) = children.first().filter(|first| transition(first)) {
        sum = add(sum, time(&member(first, "in_offset").unwrap()));
      }
      if let Some(last) = children.last().filter(|last| transition(last)) {
        sum = add(sum, time(&member(last, "out_offset").unwrap()));
      }
      sum
    })
    .collect()
}

#[test]
#[cfg_attr(
  miri,
  ignore = "too heavy for Miri on a 32-bit target; the laws Miri runs reach its every branch"
)]
fn a_fade_side_gap_of_no_length_is_read_only_in_the_form_the_export_writes() {
  // Round 8: the timeline keeps a gap's length at the edit rate alone, so a
  // gap of no length is read only as the export writes one, its range stated
  // at the edit rate exactly; any other form loses its rate, which the
  // track's duration takes (`track.cpp` 111–141; `rationalTime.h` 283–294).
  //
  // Codex's case: one clip at 24 fps with a one-tick fade-in, its medium
  // counted at 1 fps, so its source range is written at rate 1.
  let mut a = Clip::new(
    ClipId::new("a"),
    "a",
    MediaRef::new(Utf8Bytes::from_static("file:///a.mov")).with_rate(Some(Rate::hz(1))),
    TimeRange::new(0, 1, timebase(1, 1)),
    TimeRange::new(0, 24, edit()),
  );
  a.set_fades(Fades::new().with_in(Some(Fade::new(Duration::new(1, edit()), FadeShape::Linear))));
  let timeline =
    Timeline::new("codex", Rate::FPS_24).with_track(Track::new(TrackKind::Video, "V").with_clip(a));
  assert_eq!(crate::validate(&timeline), Ok(()), "the premise");
  let written = to_otio(&timeline, OtioTarget::V0_15Plus).unwrap();
  assert_eq!(
    from_otio(&written),
    Ok(timeline.clone()),
    "the premise: the export reads back"
  );
  let edited = |edit: &dyn Fn(&mut Value)| {
    let mut root = json::parse(&written).unwrap();
    edit(&mut root);
    let mut out = Utf8BytesMut::new();
    json::write_pretty(&root, &mut out);
    out.freeze_shared()
  };
  let leading = format!("{TRACK}.children.0");

  // The range left out: OpenTimelineIO reads nothing at rate 1, and the
  // track lasts 1@1 where the export's lasts 24@24 — the same time, which
  // `strictly_equal` tells apart.
  let omitted = edited(&|root| remove(root, &leading, "source_range"));
  assert_eq!(track_durations(&written), vec![(24.0, 24.0)], "the premise");
  assert_eq!(track_durations(&omitted), vec![(1.0, 1.0)], "the premise");
  assert_eq!(
    from_otio(&omitted),
    Err(shape(&format!("{leading}.source_range"), "a required key")),
    "Codex's case",
  );

  // Stated, at rate 1: lost just the same.
  let off_rate = edited(&|root| {
    for key in ["start_time", "duration"] {
      set(
        root,
        &format!("{leading}.source_range.{key}"),
        "rate",
        number("1.0"),
      );
    }
  });
  assert_eq!(track_durations(&off_rate), vec![(1.0, 1.0)], "the premise");
  assert_eq!(
    from_otio(&off_rate),
    Err(inexact(
      &format!("{leading}.source_range.start_time.rate"),
      "a rate other than the f64 of the exact ruler it is counted in"
    )),
    "a range stated at another rate",
  );
}

/// `text`, edited by `edit`, read back.
fn read_text_edited(text: &str, edit: impl FnOnce(&mut Value)) -> Result<Timeline, Unreadable> {
  let mut root = json::parse(text).unwrap();
  edit(&mut root);
  let mut out = Utf8BytesMut::new();
  json::write_pretty(&root, &mut out);
  from_otio(&out.freeze_shared())
}

/// Recounts the range at `range` at 48 ticks a second — `start` and
/// `duration` its counts there — and names that ruler in its words at
/// `words`.
fn at_48(root: &mut Value, range: &str, words: &str, start: &str, duration: &str) {
  set(root, words, "ruler", text("48/1"));
  for (key, value) in [("start_time", start), ("duration", duration)] {
    let time = format!("{range}.{key}");
    set(root, &time, "rate", number("48.0"));
    set(root, &time, "value", number(value));
  }
}

#[test]
#[cfg_attr(
  miri,
  ignore = "too heavy for Miri on a 32-bit target; the laws Miri runs reach its every branch"
)]
fn a_media_range_read_in_a_ruler_the_export_does_not_write_is_refused() {
  // Round 9: the reader counts a media-side range in the ruler its words
  // declare and then in the model's timebase, which keeps no ruler; the
  // export plans its own, and writes another range. Codex's case: a 24 fps
  // clip whose source range is 0@48 + 48@48 under the ruler 48/1 read as
  // [0, 24) at 1/24, and was written back as 0@24 + 24@24 — its inclusive
  // end moved from 47@48 to 23@24, another instant (`timeRange.h` 48–65 in
  // 0.17.0). The backstop refuses it, and the same in an available range
  // and in a clip marker's range — since round 10 at the first value that
  // differs, the ruler the range's words name.
  let a = Clip::new(
    ClipId::new("a"),
    "a",
    MediaRef::new(Utf8Bytes::from_static("file:///a.mov"))
      .with_available_range(Some(TimeRange::new(0, 24, edit())))
      .with_rate(Some(Rate::FPS_24)),
    TimeRange::new(0, 24, edit()),
    TimeRange::new(0, 24, edit()),
  )
  .with_marker(Marker::new("half", TimeRange::new(0, 12, edit())));
  let timeline =
    Timeline::new("one", Rate::FPS_24).with_track(Track::new(TrackKind::Video, "V").with_clip(a));
  let written = to_otio(&timeline, OtioTarget::V0_15Plus).unwrap();
  assert_eq!(from_otio(&written), Ok(timeline.clone()), "the premise");
  let clip = "tracks.children.0.children.0";
  let media = format!("{clip}.media_references.DEFAULT_MEDIA");
  let cases: [(&str, String, String, &str); 3] = [
    (
      "the source range",
      format!("{clip}.source_range"),
      format!("{clip}.metadata.mediaio.source"),
      "48.0",
    ),
    (
      "the available range",
      format!("{media}.available_range"),
      format!("{media}.metadata.mediaio.available"),
      "48.0",
    ),
    (
      "the clip marker's range",
      format!("{clip}.markers.0.marked_range"),
      format!("{clip}.markers.0.metadata.mediaio.range"),
      "24.0",
    ),
  ];
  // Round 10 widened the backstop to every value: the first that differs
  // is the ruler the range's words name, ahead of its times.
  for (name, range, words, duration) in cases {
    assert_eq!(
      read_text_edited(&written, |root| at_48(
        root, &range, &words, "0.0", duration
      )),
      Err(differs(&format!("{words}.ruler"), "\"24/1\"", "\"48/1\"")),
      "{name}",
    );
  }
}

#[test]
fn every_time_the_document_states_is_the_one_the_export_writes_there() {
  // Round 9: a ruler the export would not write, injected in each family of
  // times. The edit side — the global start, a gap, a transition's offsets,
  // a track marker — is counted at the edit rate bit for bit, so its own
  // check refuses it by name; the media side declares its ruler, so the
  // backstop refuses it at its first value that differs, the ruler its
  // words name.
  let off_rate = "a rate other than the f64 of the exact ruler it is counted in";
  // The time at `time`, `value` ticks at 120 a second: five a frame.
  let at_120 = |root: &mut Value, time: &str, value: &str| {
    set(root, time, "rate", number("120.0"));
    set(root, time, "value", number(value));
  };
  // The range at `range` at 120 a second, its words at `words` naming it.
  let range_at_120 = move |root: &mut Value, range: &str, words: &str, start: &str, end: &str| {
    set(root, words, "ruler", text("120/1"));
    at_120(root, &format!("{range}.start_time"), start);
    at_120(root, &format!("{range}.duration"), end);
  };
  let a_marker = format!("{A}.markers.0");
  let cases: Vec<(&str, Edit, Unreadable)> = vec![
    (
      "the global start",
      Box::new(move |root| at_120(root, "global_start_time", "0.0")),
      inexact("global_start_time.rate", off_rate),
    ),
    (
      "a gap",
      Box::new(move |root| at_120(root, &format!("{GAP}.source_range.duration"), "60.0")),
      inexact(&format!("{GAP}.source_range.duration.rate"), off_rate),
    ),
    (
      "a transition's offset",
      Box::new(move |root| at_120(root, &format!("{DISSOLVE}.in_offset"), "10.0")),
      inexact(&format!("{DISSOLVE}.in_offset.rate"), off_rate),
    ),
    (
      "a track marker",
      Box::new(move |root| {
        at_120(
          root,
          &format!("{TRACK}.markers.0.marked_range.duration"),
          "120.0",
        )
      }),
      inexact(
        &format!("{TRACK}.markers.0.marked_range.duration.rate"),
        off_rate,
      ),
    ),
    (
      "a source range",
      Box::new(move |root| {
        range_at_120(
          root,
          &format!("{A}.source_range"),
          &format!("{A}.metadata.mediaio.source"),
          "50.0",
          "120.0",
        )
      }),
      differs(
        &format!("{A}.metadata.mediaio.source.ruler"),
        "\"24/1\"",
        "\"120/1\"",
      ),
    ),
    (
      "an available range",
      Box::new(move |root| {
        range_at_120(
          root,
          &format!("{A_MEDIA}.available_range"),
          &format!("{A_MEDIA}.metadata.mediaio.available"),
          "0.0",
          "500.0",
        )
      }),
      differs(
        &format!("{A_MEDIA}.metadata.mediaio.available.ruler"),
        "\"24/1\"",
        "\"120/1\"",
      ),
    ),
    (
      "a clip marker",
      Box::new({
        let marker = a_marker.clone();
        move |root| {
          range_at_120(
            root,
            &format!("{marker}.marked_range"),
            &format!("{marker}.metadata.mediaio.range"),
            "50.0",
            "10.0",
          )
        }
      }),
      differs(
        &format!("{a_marker}.metadata.mediaio.range.ruler"),
        "\"24/1\"",
        "\"120/1\"",
      ),
    ),
  ];
  for (name, edit, refused) in cases {
    assert_eq!(read(edit), Err(refused), "{name}");
  }
}

#[test]
fn a_count_or_rate_that_does_not_land_exactly_is_refused() {
  assert_eq!(
    read(|root| set(
      root,
      "global_start_time",
      "rate",
      number("24.000000000000004")
    )),
    Err(inexact(
      "global_start_time.rate",
      "a rate other than the f64 of the exact ruler it is counted in"
    )),
  );
  assert_eq!(
    read(|root| set(
      root,
      &format!("{GAP}.source_range.duration"),
      "value",
      number("12.5")
    )),
    Err(inexact(
      &format!("{GAP}.source_range.duration.value"),
      "a count that is no whole number"
    )),
  );
  assert_eq!(
    read(|root| set(
      root,
      &format!("{A}.source_range.start_time"),
      "value",
      number("9007199254740994.0")
    )),
    Err(inexact(
      &format!("{A}.source_range.start_time.value"),
      "a count past 2^53, which an f64 does not hold exactly"
    )),
  );
  // 47 ticks of 1/48 s: half an edit-rate tick short of 24.
  assert_eq!(
    read(|root| {
      set(
        root,
        &format!("{A}.metadata.mediaio"),
        "source",
        object(vec![("ruler", text("48/1")), ("timebase", text("1/48"))]),
      );
      for (key, value) in [("start_time", "20.0"), ("duration", "47.0")] {
        let time = format!("{A}.source_range.{key}");
        set(root, &time, "rate", number("48.0"));
        set(root, &time, "value", number(value));
      }
    }),
    Err(inexact(
      &format!("{A}.source_range"),
      "a source range that is no whole number of edit-rate ticks"
    )),
  );
}

/// 2^53: past it an `f64` holds only some whole counts.
const LIMIT: i64 = 1 << 53;

/// `value` as the export writes a count.
fn count(value: i64) -> Value {
  number(&format!("{value}.0"))
}

/// Sets the start and the duration of the range at `range`.
fn set_range(root: &mut Value, range: &str, start: i64, duration: i64) {
  set(root, &format!("{range}.start_time"), "value", count(start));
  set(root, &format!("{range}.duration"), "value", count(duration));
}

#[test]
fn a_range_whose_end_opentimelineio_rounds_is_refused() {
  const ROUNDS: &str = "a range whose end, or last tick, OpenTimelineIO's f64 arithmetic rounds";
  // A track's marker, at the edit rate: two ticks ending at 2^53 read; one
  // tick from 2^53, OpenTimelineIO's sum rounds 2^53 + 1 back to 2^53. A
  // start of 2^53 - 1, OpenTimelineIO 0.17's reader reads as 2^53 - 2, and
  // so does this one.
  let marker = |start: i64, length: i64| {
    read(move |root| {
      set_range(
        root,
        &format!("{TRACK}.markers.0.marked_range"),
        start,
        length,
      )
    })
  };
  assert_eq!(
    marker(LIMIT - 2, 2).unwrap().tracks()[0].markers()[0].range(),
    TimeRange::new(LIMIT - 2, LIMIT, edit()),
  );
  assert_eq!(
    marker(LIMIT - 1, 1).unwrap().tracks()[0].markers()[0].range(),
    TimeRange::new(LIMIT - 2, LIMIT - 1, edit()),
  );
  assert_eq!(
    marker(LIMIT, 1),
    Err(inexact(&format!("{TRACK}.markers.0.marked_range"), ROUNDS)),
  );
  // A clip's marker, in the ruler its word names.
  let clip_marker = |start: i64| {
    read(move |root| set_range(root, &format!("{A}.markers.0.marked_range"), start, 2))
  };
  assert_eq!(
    clip_marker(LIMIT - 2).unwrap().tracks()[0].clips()[0].markers()[0].range(),
    TimeRange::new(LIMIT - 2, LIMIT, edit()),
  );
  assert_eq!(
    clip_marker(LIMIT),
    Err(inexact(&format!("{A}.markers.0.marked_range"), ROUNDS)),
  );
  // `b`'s source range, its medium's available range left unknown so the
  // source may lie anywhere.
  let source = |start: i64| {
    read(move |root| {
      let media = format!("{B}.media_references.DEFAULT_MEDIA");
      set(root, &media, "available_range", Value::Null);
      remove(root, &format!("{media}.metadata.mediaio"), "available");
      set_range(root, &format!("{B}.source_range"), start, 24);
    })
  };
  assert_eq!(
    source(LIMIT - 24).unwrap().tracks()[0].clips()[1].source_range(),
    TimeRange::new(LIMIT - 24, LIMIT, edit()),
  );
  assert_eq!(
    source(LIMIT - 22),
    Err(inexact(&format!("{B}.source_range"), ROUNDS)),
  );
  // An available range.
  assert_eq!(
    read(|root| set_range(root, &format!("{A_MEDIA}.available_range"), LIMIT - 99, 100)),
    Err(inexact(&format!("{A_MEDIA}.available_range"), ROUNDS)),
  );
}

#[test]
fn what_opentimelineio_derives_from_a_track_is_held_as_the_export_holds_it() {
  // The leading gap so long that `b` ends at 2^53: read, every record
  // moved on with it.
  let gap =
    |length: i64| read(move |root| set_range(root, &format!("{GAP}.source_range"), 0, length));
  assert_eq!(
    gap(LIMIT - 48).unwrap().tracks()[0].clips()[1].record(),
    TimeRange::new(LIMIT - 24, LIMIT, edit()),
  );
  // Two frames longer, `b` ends at 2^53 + 2 on its track, past what the
  // walk holds. (One frame longer, 2^53 - 47, OpenTimelineIO 0.17's reader
  // reads as 2^53 - 46.)
  assert_eq!(
    gap(LIMIT - 46),
    Err(inexact(
      B,
      "a place on its track that OpenTimelineIO's f64 arithmetic rounds"
    )),
  );
  // `a`'s source so late that the dissolve's handle after it reaches past
  // 2^53: its visible range.
  assert_eq!(
    read(|root| {
      let start = format!("{A}.source_range.start_time");
      set(root, &start, "value", count(LIMIT - 24));
    }),
    Err(inexact(
      A,
      "a visible range that OpenTimelineIO's f64 arithmetic rounds"
    )),
  );
  // A global start so late that `b` ends past 2^53 from it.
  assert_eq!(
    read(|root| set(root, "global_start_time", "value", count(LIMIT - 50))),
    Err(inexact(
      B,
      "a place from the global start that OpenTimelineIO's f64 arithmetic rounds"
    )),
  );
}

#[test]
fn every_number_is_read_as_opentimelineio_reads_it_or_refused() {
  const GAIN: &str = "a gain that is not exactly the f32 it reads as";
  let gain_at = format!("{A}.metadata.mediaio.gain_db");
  let gain = |spelled: &'static str| {
    read(move |root| {
      set(
        root,
        &format!("{A}.metadata.mediaio"),
        "gain_db",
        number(spelled),
      )
    })
  };
  // Exactly an f32, and other spellings of the sample's own -3 dB, each a
  // double as the export writes one.
  assert_eq!(
    gain("16777216.0").unwrap().tracks()[0].clips()[0].gain(),
    Gain::from_db(16_777_216.0),
  );
  for spelled in alike(["-3e0", "-0.3E1", "-30e-1", "-3.00"]) {
    assert_eq!(gain(spelled), Ok(sample()), "{spelled}");
  }
  // A gain of -0.0 is one of its own: written back as -0.0.
  assert_eq!(
    gain("-0.0").unwrap().tracks()[0].clips()[0]
      .gain()
      .map(|gain| gain.db().to_bits()),
    Some((-0.0_f32).to_bits()),
  );
  // Round 10: an integer, which OpenTimelineIO's dictionary holds as an
  // `int64_t`, not as the double the export writes (`deserialization.cpp`
  // 62–73 in 0.17.0).
  assert_eq!(gain("-3"), Err(differs(&gain_at, "-3.0", "-3")));
  // Rounded to its nearest f32 (2^24 + 1 reads as 2^24), past the largest,
  // and below the smallest.
  for spelled in alike(["16777217", "-3.0000001", "1e39", "1e-50"]) {
    assert_eq!(gain(spelled), Err(inexact(&gain_at, GAIN)), "{spelled}");
  }
  // NaN and the infinities, which OpenTimelineIO reads, by name.
  for spelled in alike(["NaN", "-Infinity"]) {
    assert_eq!(
      gain(spelled),
      Err(inexact(
        &gain_at,
        "a gain of NaN or an infinity, which no gain holds"
      )),
      "{spelled}"
    );
  }

  // A RationalTime's rate, as OpenTimelineIO reads it, as a double: any
  // spelling of the f64 the export writes — `24.0000000000000001` among
  // them, whose digits past the 17th RapidJSON drops — and no other number.
  const RATE: &str = "a rate other than the f64 of the exact ruler it is counted in";
  let rate = |spelled: &'static str| {
    read(move |root| set(root, "global_start_time", "rate", number(spelled)))
  };
  for spelled in alike(["24", "2.4e1", "2400E-2", "24.000", "24.0000000000000001"]) {
    assert_eq!(rate(spelled), Ok(sample()), "{spelled}");
  }
  for spelled in alike(["24.000000000000004", "1e-400", "-0"]) {
    assert_eq!(
      rate(spelled),
      Err(inexact("global_start_time.rate", RATE)),
      "{spelled}"
    );
  }
  for spelled in alike(["NaN", "Infinity", "-Inf"]) {
    assert_eq!(
      rate(spelled),
      Err(inexact(
        "global_start_time.rate",
        "a rate of NaN or an infinity, which no ruler holds"
      )),
      "{spelled}"
    );
  }
  // A number RapidJSON refuses refuses the document, as syntax.
  let syntax = |result: Result<Timeline, Unreadable>| match result {
    Err(Unreadable::Syntax(syntax)) => syntax.reason(),
    other => panic!("{other:?}"),
  };
  assert_eq!(syntax(rate("1e309")), "a number too big for a double");
  assert_eq!(syntax(rate("0e309")), "a number too big for a double");

  // A RationalTime's value, as a double: the sample's start, zero, in any
  // spelling OpenTimelineIO reads as zero — `-0`, an integer, among them —
  // but not `-0.0`, a double of negative zero, whose sign no whole count
  // keeps; nor NaN or an infinity. Each by name.
  let start = |spelled: &'static str| {
    read(move |root| set(root, "global_start_time", "value", number(spelled)))
  };
  for spelled in alike(["0", "-0", "0e-400", "1e-400", "0.0E+5"]) {
    assert_eq!(start(spelled), Ok(sample()), "{spelled}");
  }
  for spelled in alike(["-0.0", "-0e0", "-1e-400"]) {
    assert_eq!(
      start(spelled),
      Err(inexact(
        "global_start_time.value",
        "a count of negative zero, whose sign no whole count keeps"
      )),
      "{spelled}"
    );
  }
  for spelled in alike(["NaN", "-Infinity"]) {
    assert_eq!(
      start(spelled),
      Err(inexact(
        "global_start_time.value",
        "a count of NaN or an infinity, which no whole count holds"
      )),
      "{spelled}"
    );
  }
}

#[test]
#[cfg_attr(
  miri,
  ignore = "too heavy for Miri on a 32-bit target; the laws Miri runs reach its every branch"
)]
fn the_backstop_holds_doubles_bit_for_bit_so_a_negative_zero_is_not_zero() {
  // Round 11: the backstop by itself, the document altered after the
  // timeline was read from it. A gain of 0 dB, written `0.0`, beside a
  // document's `-0.0`, in a dictionary; the global start, zero, beside
  // `-0.0`, a RationalTime's value read as a double. `-0`, the integer
  // zero, is the double `0.0` there, and the `int64_t` 0 in a dictionary.
  let mut timeline = sample();
  timeline.tracks_mut()[0].clips_mut()[0].set_gain(Some(Gain::UNITY));
  let written = || json::parse(&to_otio(&timeline, OtioTarget::V0_15Plus).unwrap()).unwrap();
  let guarded = |edit: &dyn Fn(&mut Value)| {
    let mut root = written();
    edit(&mut root);
    super::super::import::reproduced(&root, &timeline)
  };
  assert_eq!(guarded(&|_| {}), Ok(()), "the premise");
  let words = format!("{A}.metadata.mediaio");
  let gain_at = format!("{words}.gain_db");
  assert_eq!(
    guarded(&|root| set(root, &words, "gain_db", number("-0.0"))),
    Err(differs(&gain_at, "0.0", "-0.0")),
  );
  assert_eq!(
    guarded(&|root| set(root, &words, "gain_db", number("-0"))),
    Err(differs(&gain_at, "0.0", "-0")),
  );
  assert_eq!(
    guarded(&|root| set(root, "global_start_time", "value", number("-0.0"))),
    Err(differs("global_start_time.value", "0.0", "-0.0")),
  );
  for spelled in ["-0", "0", "0e-400"] {
    assert_eq!(
      guarded(&|root| set(root, "global_start_time", "value", number(spelled))),
      Ok(()),
      "{spelled}"
    );
  }
}

/// The rows of the number table (`json::number`'s), each in a document: in
/// a dictionary — the timeline's metadata, which every reader keeps as it
/// parses it — and as a `RationalTime`'s value, which a reader fetches as a
/// double and the check holds finite.
#[test]
fn every_row_of_the_number_table_reads_in_a_document_as_opentimelineio_reads_it() {
  #[derive(Debug, PartialEq)]
  enum Row {
    /// Read in both.
    Read,
    /// Read in a dictionary; not finite, so refused as a time's value.
    NotFinite,
    /// Refused as syntax, at the number, in both.
    Refused(&'static str),
  }
  const TOO_BIG: &str = "a number too big for a double";
  let integer_of_400 = "7".repeat(400);
  let rows = [
    ("0e309", Row::Refused(TOO_BIG)),
    ("0e-400", Row::Read),
    ("0.1e309", Row::Read),
    ("1e308", Row::Read),
    ("2e308", Row::Refused(TOO_BIG)),
    ("-0", Row::Read),
    ("-0.0", Row::Read),
    ("9223372036854775807", Row::Read),
    ("9223372036854775808", Row::Read),
    ("18446744073709551615", Row::Read),
    ("18446744073709551616", Row::Read),
    ("9007199254740992", Row::Read),
    ("9007199254740993", Row::Read),
    (integer_of_400.as_str(), Row::Refused(TOO_BIG)),
    ("9007199254740991.0", Row::Read),
    ("24.0000000000000001", Row::Read),
    (
      "0.90071992547409945",
      Row::Refused("a number OpenTimelineIO's builds read as different values"),
    ),
    ("NaN", Row::NotFinite),
    ("-Infinity", Row::NotFinite),
  ];
  let checked = |edit: &dyn Fn(&mut Value)| {
    let mut root = written(OtioTarget::V0_15Plus);
    edit(&mut root);
    let mut out = Utf8BytesMut::new();
    json::write_pretty(&root, &mut out);
    validate_json(&out.freeze_shared(), OtioTarget::V0_15Plus)
  };
  let mut wrong = Vec::new();
  let mut kinds: Vec<&Row> = Vec::new();
  for (text, row) in &rows {
    // Under Miri, the first row of each kind (the module's note).
    if cfg!(miri) && kinds.contains(&row) {
      continue;
    }
    kinds.push(row);
    let in_metadata = checked(&|root| set(root, "metadata", "n", number(text)));
    let as_value = checked(&|root| set(root, "global_start_time", "value", number(text)));
    let refused = |result: &Result<(), Invalid>, reason: &str| matches!(result, Err(Invalid::Syntax(syntax)) if syntax.reason() == reason);
    let held = match row {
      Row::Read => in_metadata.is_ok() && as_value.is_ok(),
      Row::NotFinite => {
        in_metadata.is_ok()
          && as_value
            == Err(Invalid::Shape(Shape {
              path: Utf8Bytes::from("$.global_start_time.value"),
              expected: "a finite number",
            }))
      }
      Row::Refused(reason) => refused(&in_metadata, reason) && refused(&as_value, reason),
    };
    if !held {
      wrong.push(format!(
        "{text}: {row:?}, read {in_metadata:?} and {as_value:?}"
      ));
    }
  }
  assert!(wrong.is_empty(), "rows read otherwise: {wrong:?}");
}

#[test]
fn a_word_missing_or_misshapen_is_refused_by_name() {
  assert_eq!(
    read(|root| remove(root, &format!("{A}.metadata.mediaio"), "id")),
    Err(shape(
      &format!("{A}.metadata.mediaio.id"),
      "a required word"
    )),
  );
  assert_eq!(
    read(|root| remove(root, "metadata.mediaio", "rate")),
    Err(shape("metadata.mediaio.rate", "a required word")),
  );
  assert_eq!(
    read(|root| set(root, "metadata.mediaio", "rate", text("23.976"))),
    Err(shape(
      "metadata.mediaio.rate",
      "a rate above zero, written num/den"
    )),
  );
  assert_eq!(
    read(|root| remove(root, &format!("{A_MEDIA}.metadata.mediaio"), "available")),
    Err(shape(
      &format!("{A_MEDIA}.metadata.mediaio.available"),
      "a required word"
    )),
  );
  // One dissolve may say its place, which is the first; no other place.
  // Since round 10 even the first is refused, by the backstop: the export
  // says a place only where the track keeps its dissolves out of their
  // cuts' order, and OpenTimelineIO would keep the word.
  assert_eq!(
    read(|root| set(
      root,
      &format!("{DISSOLVE}.metadata"),
      "mediaio",
      object(vec![("order", number("0"))])
    )),
    Err(differs(
      &format!("{DISSOLVE}.metadata.mediaio"),
      "nothing",
      "{\"order\": 0}"
    )),
  );
  assert_eq!(
    read(|root| set(
      root,
      &format!("{DISSOLVE}.metadata"),
      "mediaio",
      object(vec![("order", number("1"))])
    )),
    Err(shape(
      &format!("{DISSOLVE}.metadata.mediaio.order"),
      "a place among the track's transitions, said by every dissolve, each place once"
    )),
  );
  // A transition at a track's edge.
  assert_eq!(
    read(|root| {
      let Value::Array(children) = at(root, &format!("{TRACK}.children")) else {
        unreachable!()
      };
      children.remove(0);
    }),
    Err(shape(
      &format!("{TRACK}.children.0"),
      "a transition between two items"
    )),
  );
}

#[test]
fn text_that_is_not_json_and_a_timeline_that_does_not_validate_are_refused() {
  assert!(matches!(from_otio("{"), Err(Unreadable::Syntax(_))));
  // `b` under `a`'s id: read whole, then refused as the timeline is.
  assert_eq!(
    read(|root| set(root, &format!("{B}.metadata.mediaio"), "id", text("a"))),
    Err(Unreadable::Validation(vec![Refusal::DuplicateClipId(
      IdClash::new(ClipAt::new(0, 0), ClipAt::new(0, 1))
    )])),
  );
}

#[test]
fn reader_refusals_say_what_and_where() {
  let shown = |refused: Unreadable| std::string::ToString::to_string(&refused);
  assert_eq!(
    shown(schema("Effect.1", &format!("{A}.effects.0"))),
    "$.tracks.children[0].children[2].effects[0]: the schema Effect.1, which the timeline does not model there"
  );
  assert_eq!(
    shown(feature(
      Feature::TrackKind("Data".into()),
      &format!("{TRACK}.kind")
    )),
    "$.tracks.children[0].kind: the track kind Data, where the timeline knows Video and Audio"
  );
  assert_eq!(
    shown(inexact(
      "global_start_time.value",
      "a count that is no whole number"
    )),
    "$.global_start_time.value: a count that is no whole number"
  );
  assert_eq!(
    shown(Unreadable::Validation(vec![Refusal::RateUnstated])),
    "the timeline read does not validate: the edit rate is zero"
  );
}

#[test]
#[cfg_attr(
  miri,
  ignore = "too heavy for Miri on a 32-bit target; the laws Miri runs reach its every branch"
)]
fn every_marker_color_reads_back_as_its_name_in_capitals() {
  // Round 10: OpenTimelineIO keeps a marker's color as written (`marker.cpp`
  // 27 in 0.17.0), so `Green` is not `GREEN`; a name other than one of the
  // eleven, exactly, is refused by name. Codex's case: `Green` and `green`
  // read as GREEN, and were written back so.
  for target in [OtioTarget::V0_15Plus, OtioTarget::Legacy] {
    for color in MarkerColor::ALL {
      let mut timeline = sample();
      timeline.tracks_mut()[0].markers_mut()[0].set_color(color);
      let text = to_otio(&timeline, target).unwrap();
      assert_eq!(
        from_otio(&text).as_ref(),
        Ok(&timeline),
        "{color} {target:?}"
      );
    }
    let marker = format!("{TRACK}.markers.0");
    for spelled in ["Green", "green", "GREEn"] {
      assert_eq!(
        read_edited(target, |root| set(root, &marker, "color", text(spelled))),
        Err(feature(
          Feature::MarkerColor(Utf8Bytes::from(spelled)),
          &format!("{marker}.color")
        )),
        "{spelled} {target:?}",
      );
    }
  }
  assert_eq!(
    MarkerColor::from_otio_name("GREEN"),
    Some(MarkerColor::Green)
  );
  assert_eq!(MarkerColor::from_otio_name("Green"), None);
  assert_eq!(MarkerColor::from_otio_name("GRAY"), None);
}

#[test]
fn an_items_color_and_an_effects_or_transitions_enabled_are_held_to_the_types_newer_readers_read() {
  // OpenTimelineIO 0.18 reads an item's `color` as an optional `Color` — a
  // `Color.1`, or null — and an effect's `enabled` as a boolean (0.18.1
  // `item.cpp:181`, `effect.cpp:26`); `main` reads a transition's
  // `enabled` too (`transition.cpp:38` at `00c22fa`). A boolean member's
  // reader takes no null. `V0_15Plus`, whose readers run to `main`, holds
  // each to its type; the readers before 0.15 keep all three as keys they
  // do not know, which `Legacy` walks untyped.
  let color = |name: &str| {
    object(vec![
      ("OTIO_SCHEMA", text("Color.1")),
      ("name", text(name)),
      ("r", number("1.0")),
      ("g", number("0.0")),
      ("b", number("0.0")),
      ("a", number("1.0")),
    ])
  };
  let effects = |enabled: Value| {
    Value::Array(vec![object(vec![
      ("OTIO_SCHEMA", text("Effect.1")),
      ("effect_name", text("blur")),
      ("enabled", enabled),
    ])])
  };
  let edited = |target: OtioTarget, path: &str, key: &str, value: Value| {
    checked_edited(target, |root| set(root, path, key, value))
  };
  // What every reader takes.
  for &target in TARGETS {
    for (path, key, value) in [
      (TRACK, "color", Value::Null),
      (A, "color", color("red")),
      (GAP, "color", color("blue")),
      (DISSOLVE, "enabled", Value::Bool(false)),
      (A, "effects", effects(Value::Bool(true))),
    ] {
      assert_eq!(
        edited(target, path, key, value),
        Ok(()),
        "{target:?}: {path}.{key}"
      );
    }
  }
  // What a reader from 0.18 refuses, `V0_15Plus` refuses by name; a reader
  // before 0.15 keeps it.
  for (path, key, value, refused, expected) in [
    (TRACK, "color", text("red"), String::new(), "an object"),
    (A, "color", number("7"), String::new(), "an object"),
    (GAP, "color", Value::Bool(true), String::new(), "an object"),
    (DISSOLVE, "enabled", Value::Null, String::new(), "a boolean"),
    (
      DISSOLVE,
      "enabled",
      text("true"),
      String::new(),
      "a boolean",
    ),
    (
      A,
      "effects",
      effects(Value::Null),
      String::from(".effects[0].enabled"),
      "a boolean",
    ),
    (
      A,
      "effects",
      effects(number("1")),
      String::from(".effects[0].enabled"),
      "a boolean",
    ),
  ] {
    let at = if refused.is_empty() {
      format!("{}.{key}", checked_path(path))
    } else {
      format!("{}{refused}", checked_path(path))
    };
    assert_eq!(
      edited(OtioTarget::V0_15Plus, path, key, value.clone()),
      Err((at, expected)),
      "{path}.{key}"
    );
    if !cfg!(miri) {
      assert_eq!(
        edited(OtioTarget::Legacy, path, key, value),
        Ok(()),
        "Legacy: {path}.{key}"
      );
    }
  }
  // A dictionary of another schema where an item's color is, under
  // `V0_15Plus` held to the `Color.1` a reader from 0.18 reads there.
  assert_eq!(
    edited(
      OtioTarget::V0_15Plus,
      A,
      "color",
      object(vec![("OTIO_SCHEMA", text("Gap.1"))])
    ),
    Err((
      format!("{}.color.OTIO_SCHEMA", checked_path(A)),
      "the schema Color.1"
    )),
  );
}

/// A reader's namespace, as a writer puts a marker's note where DaVinci
/// Resolve reads it (findit-studio/application#265).
fn resolve(note: &str) -> ReaderMetadata {
  ReaderMetadata::new().with("Resolve_OTIO", Metadata::new().with("Note", note))
}

/// The keys of the object `value`, in the order written.
fn keys(value: &Value) -> Vec<String> {
  value
    .as_object()
    .expect("an object")
    .iter()
    .map(|(key, _)| String::from(key.as_str()))
    .collect()
}

/// The sample with readers' namespaces on every object that keeps them:
/// the timeline — beside `zeta`, which byte order puts after `mediaio`, its
/// keys given out of that order — its track, the track's marker, `a` and
/// `a`'s marker; and on `b` a namespace holding no entry.
fn with_readers() -> Timeline {
  let zeta = Metadata::new().with("b", "2").with("a", "3").with("B", "1");
  let mut timeline = sample().with_readers(resolve("the cut").with("zeta", zeta));
  let track = &mut timeline.tracks_mut()[0];
  *track.readers_mut() = resolve("picture");
  *track.markers_mut()[0].readers_mut() = resolve("act one");
  *track.clips_mut()[0].readers_mut() = resolve("a");
  *track.clips_mut()[0].markers_mut()[0].readers_mut() = resolve("n");
  *track.clips_mut()[1].readers_mut() = ReaderMetadata::new().with("Resolve_OTIO", Metadata::new());
  timeline
}

#[test]
fn other_readers_namespaces_are_written_beside_the_crates_words_and_read_back_exactly() {
  // Q752: each namespace in the object's `metadata`, beside `mediaio`,
  // where its reader reads it, and read back into the model whole.
  let timeline = with_readers();
  for target in [OtioTarget::V0_15Plus, OtioTarget::Legacy] {
    let written = to_otio(&timeline, target).unwrap();
    assert_eq!(validate_json(&written, target), Ok(()), "{target:?}");
    assert!(
      plain_where_it_should_be(&json::parse(&written).unwrap(), false),
      "{target:?}: {written}"
    );
    assert_eq!(from_otio(&written), Ok(timeline.clone()), "{target:?}");
  }
  // As written: every key of a metadata object in byte order, as
  // OpenTimelineIO's own writer orders a dictionary's — `Resolve_OTIO`,
  // `mediaio`, `zeta` — and every key of a namespace too.
  let mut root = json::parse(&to_otio(&timeline, OtioTarget::V0_15Plus).unwrap()).unwrap();
  assert_eq!(
    keys(at(&mut root, "metadata")),
    ["Resolve_OTIO", "mediaio", "zeta"]
  );
  assert_eq!(keys(at(&mut root, "metadata.zeta")), ["B", "a", "b"]);
  assert_eq!(
    *at(&mut root, "metadata.Resolve_OTIO"),
    object(vec![("Note", text("the cut"))])
  );
  // A track and a track's marker write no word of the crate's here: the
  // namespace alone.
  for (owner, note) in [
    (String::from(TRACK), "picture"),
    (format!("{TRACK}.markers.0"), "act one"),
  ] {
    assert_eq!(
      *at(&mut root, &format!("{owner}.metadata")),
      object(vec![("Resolve_OTIO", object(vec![("Note", text(note))]))]),
      "{owner}"
    );
  }
  // A clip and a clip's marker write the namespace before the crate's
  // words.
  for owner in [String::from(A), format!("{A}.markers.0")] {
    assert_eq!(
      keys(at(&mut root, &format!("{owner}.metadata"))),
      ["Resolve_OTIO", "mediaio"],
      "{owner}"
    );
  }
}

#[test]
fn an_empty_namespace_is_written_as_an_empty_object_and_reads_back_as_one() {
  // Q752, as ruled: an object with no namespace writes none — the sample's
  // track writes `{}` — while a namespace the model holds is written whole,
  // `{}` where it holds no entry, so `from_otio` reads it back: the exact
  // inverse both ways.
  let mut root = written(OtioTarget::V0_15Plus);
  assert_eq!(*at(&mut root, &format!("{TRACK}.metadata")), object(vec![]));
  let mut root = json::parse(&to_otio(&with_readers(), OtioTarget::V0_15Plus).unwrap()).unwrap();
  assert_eq!(
    *at(&mut root, &format!("{B}.metadata.Resolve_OTIO")),
    object(vec![])
  );
  // On each object that keeps namespaces, alone, for both targets.
  let empty = || ReaderMetadata::new().with("Avid", Metadata::new());
  let placed: [fn(&mut Timeline); 5] = [
    |timeline| *timeline.readers_mut() = ReaderMetadata::new().with("Avid", Metadata::new()),
    |timeline| {
      *timeline.tracks_mut()[0].readers_mut() = ReaderMetadata::new().with("Avid", Metadata::new());
    },
    |timeline| {
      *timeline.tracks_mut()[0].markers_mut()[0].readers_mut() =
        ReaderMetadata::new().with("Avid", Metadata::new());
    },
    |timeline| {
      *timeline.tracks_mut()[0].clips_mut()[1].readers_mut() =
        ReaderMetadata::new().with("Avid", Metadata::new());
    },
    |timeline| {
      *timeline.tracks_mut()[0].clips_mut()[0].markers_mut()[0].readers_mut() =
        ReaderMetadata::new().with("Avid", Metadata::new());
    },
  ];
  for (index, place) in placed.into_iter().enumerate() {
    let mut timeline = sample();
    place(&mut timeline);
    for &target in TARGETS {
      let written = to_otio(&timeline, target).unwrap();
      assert_eq!(
        from_otio(&written),
        Ok(timeline.clone()),
        "{index} {target:?}: {written}"
      );
    }
  }
  // A document stating one reads it as one, and writes it back.
  let mut expected = sample();
  *expected.tracks_mut()[0].readers_mut() = empty();
  assert_eq!(
    read(|root| set(root, &format!("{TRACK}.metadata"), "Avid", object(vec![]))),
    Ok(expected),
  );
}

#[test]
fn a_namespace_holding_anything_but_strings_is_refused_by_name() {
  // Q752, as ruled: a namespace holds strings, and any other value inside
  // one is refused by name — so no number reaches the backstop's
  // classifier there.
  let owners = [
    String::new(),
    String::from(TRACK),
    format!("{TRACK}.markers.0"),
    String::from(A),
    format!("{A}.markers.0"),
  ];
  let values = [
    number("3"),
    number("-0.0"),
    Value::Bool(true),
    Value::Null,
    Value::Array(vec![text("a")]),
    object(vec![("OTIO_SCHEMA", text("Marker.2"))]),
  ];
  for value in alike(values) {
    for owner in &owners {
      let namespace = object(vec![("Note", text("n")), ("Color", value.clone())]);
      assert_eq!(
        read(|root| set(
          root,
          &format!("{owner}.metadata"),
          "Resolve_OTIO",
          namespace
        )),
        Err(feature(
          Feature::NamespaceValue("Color".into()),
          &format!("{owner}.metadata.Resolve_OTIO.Color")
        )),
        "{owner}: {value:?}",
      );
    }
  }
  // A key beside `mediaio` whose value is no object is no namespace:
  // another writer's note, refused by name.
  for value in alike([
    text("a note"),
    number("3"),
    Value::Array(vec![]),
    Value::Null,
  ]) {
    assert_eq!(
      read(|root| set(
        root,
        &format!("{A}.metadata"),
        "Resolve_OTIO",
        value.clone()
      )),
      Err(feature(
        Feature::Metadata("Resolve_OTIO".into()),
        &format!("{A}.metadata.Resolve_OTIO")
      )),
      "{value:?}",
    );
  }
  // An object that keeps no namespace refuses any such key by name: the
  // stack, a gap, a fade, a dissolve and a reference.
  for owner in ["tracks", GAP, FADE_IN, DISSOLVE, A_MEDIA] {
    assert_eq!(
      read(|root| set(
        root,
        &format!("{owner}.metadata"),
        "Resolve_OTIO",
        object(vec![("Note", text("n"))])
      )),
      Err(feature(
        Feature::Metadata("Resolve_OTIO".into()),
        &format!("{owner}.metadata.Resolve_OTIO")
      )),
      "{owner}",
    );
  }
  assert_eq!(
    std::string::ToString::to_string(&feature(
      Feature::NamespaceValue("Color".into()),
      &format!("{A}.metadata.Resolve_OTIO.Color")
    )),
    "$.tracks.children[0].children[2].metadata.Resolve_OTIO.Color: the entry Color, which is no \
     string: a reader's namespace holds strings alone"
  );
}

#[test]
fn a_namespace_validation_refuses_is_read_and_refused_and_never_written() {
  // A namespace named `OTIO_SCHEMA`, or one holding that key — which
  // OpenTimelineIO reads as a schema word — reads, and the timeline read
  // does not validate; `mediaio` is the crate's own words, never one.
  let a = MetadataAt::Clip(ClipAt::new(0, 0));
  assert_eq!(
    read(|root| set(
      root,
      &format!("{A}.metadata"),
      "OTIO_SCHEMA",
      object(vec![])
    )),
    Err(Unreadable::Validation(vec![Refusal::ReservedNamespace(
      NamespaceAt::new(a, 0)
    )])),
  );
  assert_eq!(
    read(|root| set(
      root,
      &format!("{A}.metadata"),
      "Resolve_OTIO",
      object(vec![("OTIO_SCHEMA", text("Marker.2"))])
    )),
    Err(Unreadable::Validation(vec![Refusal::SchemaKeyInNamespace(
      NamespaceAt::new(a, 0)
    )])),
  );
  // And `to_otio` writes none of them, `mediaio` among them.
  let at = NamespaceAt::new(
    MetadataAt::Marker(MarkerAt::Track {
      track: 0,
      marker: 0,
    }),
    0,
  );
  for (readers, refusal) in [
    (
      ReaderMetadata::new().with("mediaio", Metadata::new()),
      Refusal::ReservedNamespace(at),
    ),
    (
      ReaderMetadata::new().with("OTIO_SCHEMA", Metadata::new()),
      Refusal::ReservedNamespace(at),
    ),
    (
      ReaderMetadata::new().with("Avid", Metadata::new().with("OTIO_SCHEMA", "Marker.2")),
      Refusal::SchemaKeyInNamespace(at),
    ),
  ] {
    let mut timeline = sample();
    *timeline.tracks_mut()[0].markers_mut()[0].readers_mut() = readers;
    assert_eq!(
      to_otio(&timeline, OtioTarget::V0_15Plus),
      Err(Refused::Validation(vec![refusal])),
    );
  }
}
