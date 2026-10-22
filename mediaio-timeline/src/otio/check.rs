//! OpenTimelineIO's schema shape, checked over a value the crate's own
//! reader produced.
//!
//! # Every schema the check accepts
//!
//! Each schema's `read_from` in OpenTimelineIO's C++ core decides what a
//! document must carry and of which type: `reader.read` fails on a missing
//! key and `reader.read_if_present` does not, and each takes exactly the
//! type of the member it fills (`deserialization.cpp:376–415` in 0.17.0) —
//! a `double` any number (418–454), an `int64_t` an integer (457–487), a
//! string or an optional value `null` as well, read as empty or as none.
//! A number is held as OpenTimelineIO's parser reads it — RapidJSON at the
//! commit 0.17.0 pins, whose verdict on each number's text the crate's
//! reader gives ([`json::number`]): an integer written without a fraction
//! or an exponent, `-0` among them and every one up to 2^64 − 1, which the
//! handler keeps cut to its low 63 bits (`deserialization.cpp:62–73`), and
//! any other number a `double`, `NaN` and the infinities among them. A
//! number RapidJSON refuses — `0e309`, `2e308` — refuses the document as
//! syntax, and so does one OpenTimelineIO's builds read as different
//! values. The table is OpenTimelineIO 0.17.0's, with each
//! reader's lines under `src/opentimelineio/` at `v0.17.0`; the check holds
//! every schema it accepts to its row, and in places to more (`*`, see
//! below). A schema not in the table is refused by name.
//!
//! | schema | required | optional | reader | the check |
//! |---|---|---|---|---|
//! | every schema with metadata | — | `metadata`: a dictionary; `name`: a string | `serializableObjectWithMetadata.cpp:21–22` | metadata a plain dictionary, walked |
//! | `Timeline.1` | `tracks`: a `Stack.1` | `global_start_time`: a `RationalTime.1`, or null | `timeline.cpp:30–31` | as read |
//! | every item | — | `source_range`: a `TimeRange.1`, or null; `effects`: effects; `markers`: markers; `enabled`: a boolean | `item.cpp:175–178` | each marker a `Marker.2`* |
//! | `Stack.1` | `children`: composables | an item's | `stack.cpp:32`, `composition.cpp:191` | children tracks, stacks, clips and gaps* |
//! | `Track.1` | `kind`: a string; `children`: composables | an item's | `track.cpp:34`, `composition.cpp:191` | each transition between two items* |
//! | `Clip.2` | `media_references`: a dictionary of media references; `active_media_reference_key`: a string | an item's | `clip.cpp:141–144` | the references a plain dictionary, the active key one of them* |
//! | `Clip.1` | — | `media_reference` | upgraded to a `Clip.2`, anything but a media reference read as missing (`typeRegistry.cpp:99–117`) | the reference null or a media reference* |
//! | `Gap.1` | — | an item's | `gap.cpp:43` | as read |
//! | `Transition.1` | `in_offset`, `out_offset`: `RationalTime.1`; `transition_type`: a string | metadata and name | `transition.cpp:33–35` | offsets of zero or more* |
//! | `Marker.2` | `marked_range`: a `TimeRange.1` | `color`, `comment`: strings | `marker.cpp:27–29` | as read |
//! | every media reference | — | `available_range`: a `TimeRange.1`, or null; `available_image_bounds`: a `Box2d.1`, or null | `mediaReference.cpp:30–33` | as read |
//! | `ExternalReference.1` | `target_url`: a string | a reference's | `externalReference.cpp:23` | as read |
//! | `MissingReference.1` | — | a reference's | `missingReference.cpp:28` | as read |
//! | `GeneratorReference.1` | `generator_kind`: a string; `parameters`: a dictionary | a reference's | `generatorReference.cpp:26–27` | the parameters a plain dictionary, walked |
//! | `ImageSequenceReference.1` | `target_url_base`, `name_prefix`, `name_suffix`: strings; `start_frame`, `frame_step`, `frame_zero_padding`: whole numbers; `rate`: a number; `missing_frame_policy`: `error`, `black` or `hold` | a reference's | `imageSequenceReference.cpp:195–233` | as read |
//! | `Effect.1`, `TimeEffect.1` | `effect_name`: a string | metadata and name | `effect.cpp:23` | as read |
//! | `LinearTimeWarp.1`, `FreezeFrame.1` | `time_scalar`: a number; an effect's | an effect's | `linearTimeWarp.cpp:23` | as read |
//! | `RationalTime.1` | `rate`, `value`: numbers | — | `deserialization.cpp:575–581` | a positive, finite rate; a finite value* |
//! | `TimeRange.1` | `start_time`, `duration`: `RationalTime.1` | — | `deserialization.cpp:582–589` | a duration of zero or more* |
//! | `TimeTransform.1` | `offset`: a `RationalTime.1`; `rate`, `scale`: numbers | — | `deserialization.cpp:590–598` | as read |
//! | `V2d.1` | `x`, `y`: numbers | — | `deserialization.cpp:609–615` | as read |
//! | `Box2d.1` | `min`, `max`: `V2d.1` | — | `deserialization.cpp:616–623` | as read |
//! | `Color.1` | `name`: a string; `r`, `g`, `b`, `a`: numbers | — | 0.18.1 `deserialization.cpp:591–598`; 0.17.0 keeps it as an unknown schema | as 0.18.1 reads it |
//! | every object a reader registers | — | `OTIO_REF_ID`: a string, nonempty ones unique in the document | `deserialization.cpp:624–642`, `669–672` | as read (below) |
//!
//! The check requires exactly the keys of every reader of its target —
//! releases 0.15 to 0.18.1 and `main` (`00c22fa`) for
//! [`OtioTarget::V0_15Plus`]; for [`OtioTarget::Legacy`], the C++ readers of
//! 0.12 to 0.14.1 (the Python ones before them require less). Where another
//! reader differs from 0.17.0's, the check follows the stricter:
//!
//! - A `Clip.1` must carry its `media_reference` under `Legacy` (0.14.1
//!   `clip.cpp:34`, 0.12 `clip.cpp:28`); `Clip.2` is unknown there, and
//!   refused.
//! - `kind` is `track.cpp:31` in 0.14.1; `marked_range` is `marker.cpp:26`
//!   in 0.15, `:23` in 0.14.1 and `:20` in 0.12; `effect_name` is
//!   `effect.cpp:25` at `main`, `:20` in 0.14.1 and `:17` in 0.12;
//!   `time_scalar` is `linearTimeWarp.cpp:20` in 0.14.1 and `:17` in 0.12.
//! - A reader before 0.15 does not decode a `V2d.1` or a `Box2d.1`, nor one
//!   before 0.18 a `Color.1`: it keeps the dictionary as an object of a
//!   schema it does not know (below). The check holds it to the newer
//!   readers' rules for every target. A media reference's image bounds, which
//!   no reader before 0.15 reads, must be an object under `Legacy`, walked
//!   as any other.
//! - A marker's `comment` is read from 0.16 (readers before keep it as a
//!   field they do not know, and write it back); an item's `color`, and an
//!   effect's `enabled`, from 0.18 (0.18.1 `item.cpp:181`; `effect.cpp:26`
//!   at `main`).
//!
//! # Every dictionary
//!
//! A reader converts each dictionary as its parser closes it, wherever it
//! sits (`deserialization.cpp:156–190` at `main`): one without `OTIO_SCHEMA`
//! stays a dictionary, and one carrying it becomes an object of the schema
//! it names (`:564–568`), which that schema's own `read_from` then reads
//! (`serializableObject.h:222–235`). So a dictionary in metadata, under a
//! key no reader reads, or nested inside either, refuses the document when
//! its schema's reader would. The check walks every dictionary alike and
//! holds each one carrying `OTIO_SCHEMA` to that schema's rules, wherever it
//! sits. A dictionary a reader reads as one — metadata, a clip's media
//! references, a generator's parameters — is never a schema's object: a
//! member named `OTIO_SCHEMA` there is refused by name.
//!
//! A reader registers each object it makes by its `OTIO_REF_ID`: it fetches
//! the key, where present, as a string, and refuses a nonempty one an
//! object it made before carries (`deserialization.cpp:624–642`, `669–672`
//! in 0.17.0). It makes an object of every dictionary carrying
//! `OTIO_SCHEMA` but the value schemas it decodes before (`:575–623`), whose
//! `OTIO_REF_ID` it neither reads nor registers: `RationalTime.1`,
//! `TimeRange.1`, `TimeTransform.1` and `SerializableObjectRef.1` for every
//! reader, `V2d.1` and `Box2d.1` from 0.15, `Color.1` from 0.18. The check
//! holds every other schema's id so, in the order a reader makes them — each
//! dictionary's contents before itself — and, for a schema a reader of the
//! target decodes as a value and another as an object, as the latter does.
//!
//! A schema whose rules the check does not know is refused by name, at its
//! `OTIO_SCHEMA`. In one case that is stricter than a reader: a name no
//! reader registers becomes an `UnknownSchema` (`typeRegistry.cpp:260–290`
//! in 0.17.0), which a reader keeps where the document leaves the type
//! open. The check does not vouch for an object it cannot read, and the
//! export writes none.
//!
//! # What a value is held to
//!
//! A value present is held to the type its reader takes, and where the
//! table marks it (`*`) to more: a string where a reader would take `null`
//! as empty, a positive, finite rate, a length of zero or more, an active
//! media key among the references, every transition between two items,
//! every marker a `Marker.2` — the one marker schema the readers 0.12 to
//! 0.18.1 know — and a `Clip.1`'s reference a media reference or null,
//! where a reader from 0.15 reads anything else as missing. Keys no reader
//! of the target requires and this crate never writes are walked but not
//! typed: a newer reader's `color` on an item or `enabled` on a transition
//! or an effect.

use std::{collections::BTreeSet, format};

use smol_bytes::{Utf8Bytes, format_utf8bytes};

use super::{
  OtioTarget, Shape,
  json::{self, Held, Value},
};

type Members = [(Utf8Bytes, Value)];

/// The keys every schema with metadata reads.
const NAMED: &[&str] = &["metadata", "name"];

/// The keys every item reads, beside [`NAMED`].
const ITEM: &[&str] = &["source_range", "effects", "markers", "enabled"];

const MEDIA_REFERENCES: &[&str] = &[
  "ExternalReference.1",
  "MissingReference.1",
  "GeneratorReference.1",
  "ImageSequenceReference.1",
];

const EFFECTS: &[&str] = &[
  "Effect.1",
  "TimeEffect.1",
  "LinearTimeWarp.1",
  "FreezeFrame.1",
];

pub(crate) fn timeline(root: &Value, target: OtioTarget) -> Result<(), Shape> {
  timeline_at(root, "$", target)?;
  reference_ids(root, "$", target, &mut BTreeSet::new())
}

/// Whether every reader of `target` decodes `name` as a value schema, whose
/// `OTIO_REF_ID` it neither reads nor registers: the branches of
/// `SerializableObject::Reader::_decode` before its last
/// (`deserialization.cpp:575–623` in 0.17.0; 0.12's and 0.14.1's are the
/// first four, and `Color.1` is one only from 0.18, after the oldest reader
/// of either target).
fn value_schema(name: &str, target: OtioTarget) -> bool {
  match name {
    "RationalTime.1" | "TimeRange.1" | "TimeTransform.1" | "SerializableObjectRef.1" => true,
    "V2d.1" | "Box2d.1" => target == OtioTarget::V0_15Plus,
    _ => false,
  }
}

/// Every object a reader registers carries its `OTIO_REF_ID`, where it has
/// one, as a string, and no nonempty one an object registered before it
/// carries (`deserialization.cpp:624–642`, `669–672` in 0.17.0). A reader
/// makes an object of each dictionary carrying `OTIO_SCHEMA` of no value
/// schema ([`value_schema`]), wherever it sits, as its parser closes the
/// dictionary — after everything the dictionary holds — so the walk takes
/// each dictionary's contents before the dictionary, and names the id a
/// reader would refuse.
fn reference_ids<'a>(
  value: &'a Value,
  path: &str,
  target: OtioTarget,
  seen: &mut BTreeSet<&'a str>,
) -> Result<(), Shape> {
  match value {
    Value::Array(entries) => {
      for (index, entry) in entries.iter().enumerate() {
        reference_ids(entry, &format!("{path}[{index}]"), target, seen)?;
      }
    }
    Value::Object(members) => {
      for (key, entry) in members {
        reference_ids(entry, &format!("{path}.{key}"), target, seen)?;
      }
      let registered = member(members, "OTIO_SCHEMA")
        .and_then(Value::as_str)
        .is_some_and(|name| !value_schema(name, target));
      if registered && let Some(id) = member(members, "OTIO_REF_ID") {
        let at = format!("{path}.OTIO_REF_ID");
        let id = string_value(id, &at)?;
        if !id.is_empty() && !seen.insert(id) {
          return Err(shape(&at, "an id no other object carries"));
        }
      }
    }
    _ => {}
  }
  Ok(())
}

fn timeline_at(value: &Value, path: &str, target: OtioTarget) -> Result<(), Shape> {
  let members = schema(value, path, &["Timeline.1"], "the schema Timeline.1")?;
  named(members, path, target)?;
  optional(members, path, "global_start_time", |value, at| {
    nullable(value, at, |value, at| rational_time(value, at, target))
  })?;
  let (tracks, at) = required(members, path, "tracks")?;
  stack(tracks, &at, target)?;
  others(
    members,
    path,
    target,
    &[NAMED, &["global_start_time", "tracks"]],
  )
}

fn stack(value: &Value, path: &str, target: OtioTarget) -> Result<(), Shape> {
  let members = schema(value, path, &["Stack.1"], "the schema Stack.1")?;
  item(members, path, target)?;
  let (children, at) = required(members, path, "children")?;
  for (index, child) in array(children, &at)?.iter().enumerate() {
    composable(child, &format!("{at}[{index}]"), target, false)?;
  }
  others(members, path, target, &[NAMED, ITEM, &["children"]])
}

fn track(value: &Value, path: &str, target: OtioTarget) -> Result<(), Shape> {
  let members = schema(value, path, &["Track.1"], "the schema Track.1")?;
  item(members, path, target)?;
  let (kind, at) = required(members, path, "kind")?;
  string(kind, &at)?;
  let (children, at) = required(members, path, "children")?;
  let children = array(children, &at)?;
  let is_transition = |index: usize| {
    children
      .get(index)
      .and_then(Value::as_object)
      .and_then(|members| member(members, "OTIO_SCHEMA"))
      .and_then(Value::as_str)
      == Some("Transition.1")
  };
  for (index, child) in children.iter().enumerate() {
    let here = format!("{at}[{index}]");
    composable(child, &here, target, true)?;
    if is_transition(index)
      && (index == 0
        || index + 1 == children.len()
        || is_transition(index - 1)
        || is_transition(index + 1))
    {
      return Err(shape(&here, "a transition between two items"));
    }
  }
  others(members, path, target, &[NAMED, ITEM, &["kind", "children"]])
}

fn composable(value: &Value, path: &str, target: OtioTarget, in_track: bool) -> Result<(), Shape> {
  let members = object(value, path)?;
  let name = schema_name(members, path)?;
  match name {
    "Track.1" => track(value, path, target),
    "Stack.1" => stack(value, path, target),
    "Gap.1" => {
      item(members, path, target)?;
      others(members, path, target, &[NAMED, ITEM])
    }
    "Clip.1" => clip1(members, path, target),
    "Clip.2" if target == OtioTarget::V0_15Plus => clip2(members, path, target),
    "Clip.2" => Err(shape(
      &format!("{path}.OTIO_SCHEMA"),
      "Clip.1, the only clip readers before OpenTimelineIO 0.15 know",
    )),
    "Transition.1" if in_track => transition(members, path, target),
    _ => Err(shape(
      &format!("{path}.OTIO_SCHEMA"),
      if in_track {
        "a schema a track holds"
      } else {
        "a schema a stack holds"
      },
    )),
  }
}

fn clip1(members: &Members, path: &str, target: OtioTarget) -> Result<(), Shape> {
  item(members, path, target)?;
  let reference =
    |value: &Value, at: &str| nullable(value, at, |value, at| media_reference(value, at, target));
  match target {
    OtioTarget::V0_15Plus => optional(members, path, "media_reference", reference)?,
    OtioTarget::Legacy => {
      let (value, at) = required(members, path, "media_reference")?;
      reference(value, &at)?;
    }
  }
  others(members, path, target, &[NAMED, ITEM, &["media_reference"]])
}

fn clip2(members: &Members, path: &str, target: OtioTarget) -> Result<(), Shape> {
  item(members, path, target)?;
  let (references, at) = required(members, path, "media_references")?;
  let references = plain(
    references,
    &at,
    "no schema: media_references is a dictionary",
  )?;
  for (key, reference) in references {
    media_reference(reference, &format!("{at}.{key}"), target)?;
  }
  let (active, at) = required(members, path, "active_media_reference_key")?;
  let active = string_value(active, &at)?;
  if member(references, active).is_none() {
    return Err(shape(&at, "a key of media_references"));
  }
  others(
    members,
    path,
    target,
    &[
      NAMED,
      ITEM,
      &["media_references", "active_media_reference_key"],
    ],
  )
}

fn media_reference(value: &Value, path: &str, target: OtioTarget) -> Result<(), Shape> {
  let members = schema(value, path, MEDIA_REFERENCES, "a media reference schema")?;
  named(members, path, target)?;
  optional(members, path, "available_range", |value, at| {
    nullable(value, at, |value, at| time_range(value, at, target))
  })?;
  // Read from 0.15 as a `Box2d`; kept before as a key the reader does not
  // know.
  optional(members, path, "available_image_bounds", |value, at| {
    nullable(value, at, |value, at| match target {
      OtioTarget::V0_15Plus => box2d(value, at, target),
      OtioTarget::Legacy => {
        object(value, at)?;
        contents(value, at, target)
      }
    })
  })?;
  // Each reference's own members, as its reader reads them; a
  // `MissingReference.1` reads none.
  let own: &[&str] = match schema_name(members, path)? {
    "ExternalReference.1" => {
      let (url, at) = required(members, path, "target_url")?;
      string(url, &at)?;
      &["target_url"]
    }
    "GeneratorReference.1" => {
      let (kind, at) = required(members, path, "generator_kind")?;
      string(kind, &at)?;
      let (parameters, at) = required(members, path, "parameters")?;
      plain(parameters, &at, "no schema: parameters is a dictionary")?;
      contents(parameters, &at, target)?;
      &["generator_kind", "parameters"]
    }
    "ImageSequenceReference.1" => {
      image_sequence(members, path)?;
      IMAGE_SEQUENCE
    }
    _ => &[],
  };
  others(
    members,
    path,
    target,
    &[NAMED, &["available_range", "available_image_bounds"], own],
  )
}

/// The members an `ImageSequenceReference.1`'s reader requires, in its
/// order (`imageSequenceReference.cpp:195–233` in 0.17.0).
const IMAGE_SEQUENCE: &[&str] = &[
  "target_url_base",
  "name_prefix",
  "name_suffix",
  "start_frame",
  "frame_step",
  "rate",
  "frame_zero_padding",
  "missing_frame_policy",
];

/// An `ImageSequenceReference.1`'s own members: the URL's base, the name's
/// prefix and suffix as strings, the first frame, the step and the zero
/// padding as whole numbers, the rate as a number, and a missing frame's
/// policy, one of the three the reader knows (`:214–233`).
fn image_sequence(members: &Members, path: &str) -> Result<(), Shape> {
  for key in IMAGE_SEQUENCE {
    let (value, at) = required(members, path, key)?;
    match *key {
      "start_frame" | "frame_step" | "frame_zero_padding" => integer(value, &at)?,
      "rate" => number(value, &at)?,
      "missing_frame_policy" => match string_value(value, &at)? {
        "error" | "black" | "hold" => {}
        _ => return Err(shape(&at, "error, black or hold")),
      },
      _ => string(value, &at)?,
    }
  }
  Ok(())
}

fn transition(members: &Members, path: &str, target: OtioTarget) -> Result<(), Shape> {
  named(members, path, target)?;
  let (in_offset, at) = required(members, path, "in_offset")?;
  length(in_offset, &at, target)?;
  let (out_offset, at) = required(members, path, "out_offset")?;
  length(out_offset, &at, target)?;
  let (kind, at) = required(members, path, "transition_type")?;
  string(kind, &at)?;
  others(
    members,
    path,
    target,
    &[NAMED, &["in_offset", "out_offset", "transition_type"]],
  )
}

/// The fields every item carries: a name and metadata, a trim, effects,
/// markers, and whether it plays.
fn item(members: &Members, path: &str, target: OtioTarget) -> Result<(), Shape> {
  named(members, path, target)?;
  optional(members, path, "source_range", |value, at| {
    nullable(value, at, |value, at| time_range(value, at, target))
  })?;
  optional(members, path, "effects", |value, at| {
    for (index, entry) in array(value, at)?.iter().enumerate() {
      effect(entry, &format!("{at}[{index}]"), target)?;
    }
    Ok(())
  })?;
  optional(members, path, "markers", |value, at| {
    for (index, entry) in array(value, at)?.iter().enumerate() {
      marker(entry, &format!("{at}[{index}]"), target)?;
    }
    Ok(())
  })?;
  optional(members, path, "enabled", |value, at| match value {
    Value::Bool(_) => Ok(()),
    _ => Err(shape(at, "a boolean")),
  })
}

/// A `Marker.2`: its name and metadata, its color and comment strings, and
/// the range it marks, which every reader requires.
fn marker(value: &Value, path: &str, target: OtioTarget) -> Result<(), Shape> {
  let members = schema(value, path, &["Marker.2"], "the schema Marker.2")?;
  named(members, path, target)?;
  optional(members, path, "color", string)?;
  let (range, at) = required(members, path, "marked_range")?;
  time_range(range, &at, target)?;
  optional(members, path, "comment", string)?;
  others(
    members,
    path,
    target,
    &[NAMED, &["color", "marked_range", "comment"]],
  )
}

/// An effect: its name and metadata, the `effect_name` every reader
/// requires, and a time warp's `time_scalar`. An item holds its effects as
/// `Effect`s, so every reader refuses another schema there too.
fn effect(value: &Value, path: &str, target: OtioTarget) -> Result<(), Shape> {
  let members = schema(value, path, EFFECTS, "an effect schema")?;
  named(members, path, target)?;
  let (name, at) = required(members, path, "effect_name")?;
  string(name, &at)?;
  let warp = matches!(
    schema_name(members, path)?,
    "LinearTimeWarp.1" | "FreezeFrame.1"
  );
  if warp {
    let (scalar, at) = required(members, path, "time_scalar")?;
    number(scalar, &at)?;
  }
  let scalar: &[&str] = if warp { &["time_scalar"] } else { &[] };
  others(members, path, target, &[NAMED, &["effect_name"], scalar])
}

fn named(members: &Members, path: &str, target: OtioTarget) -> Result<(), Shape> {
  optional(members, path, "metadata", |value, at| {
    dictionary(value, at, target)
  })?;
  optional(members, path, "name", string)
}

/// Metadata: a dictionary every reader takes as one, never a schema's
/// object ([`plain`]). Its values are walked as any others are
/// ([`contents`]).
fn dictionary(value: &Value, path: &str, target: OtioTarget) -> Result<(), Shape> {
  plain(value, path, "no schema: metadata is a dictionary")?;
  contents(value, path, target)
}

/// A dictionary a reader reads as one — metadata, a clip's media
/// references, a generator's parameters — and so never a schema's object:
/// one carrying `OTIO_SCHEMA` is decoded as an object, its `OTIO_SCHEMA`
/// fetched as a string (`deserialization.cpp:561–573` in 0.17.0), and the
/// reader refuses it where it reads a dictionary. Refused here at that
/// member, as `expected` names it.
fn plain<'a>(value: &'a Value, path: &str, expected: &'static str) -> Result<&'a Members, Shape> {
  let members = object(value, path)?;
  if member(members, "OTIO_SCHEMA").is_some() {
    return Err(shape(&format!("{path}.OTIO_SCHEMA"), expected));
  }
  Ok(members)
}

/// A value where the document leaves the type open — metadata, a key no
/// reader reads, and what either holds. A reader decodes every dictionary
/// there all the same: a plain one stays a dictionary, whose values are
/// walked in turn, and one carrying `OTIO_SCHEMA` is an object of that
/// schema ([`any_schema`]).
fn contents(value: &Value, path: &str, target: OtioTarget) -> Result<(), Shape> {
  match value {
    Value::Array(entries) => {
      for (index, entry) in entries.iter().enumerate() {
        contents(entry, &format!("{path}[{index}]"), target)?;
      }
      Ok(())
    }
    Value::Object(members) => {
      if member(members, "OTIO_SCHEMA").is_some() {
        return any_schema(value, path, target);
      }
      for (key, entry) in members {
        contents(entry, &format!("{path}.{key}"), target)?;
      }
      Ok(())
    }
    _ => Ok(()),
  }
}

/// A dictionary carrying `OTIO_SCHEMA` where the document leaves the type
/// open: held to that schema's rules, or refused by name where the check
/// does not know them.
fn any_schema(value: &Value, path: &str, target: OtioTarget) -> Result<(), Shape> {
  let members = object(value, path)?;
  match schema_name(members, path)? {
    "Timeline.1" => timeline_at(value, path, target),
    "Stack.1" | "Track.1" | "Gap.1" | "Clip.1" | "Clip.2" => composable(value, path, target, false),
    "Transition.1" => transition(members, path, target),
    "Marker.2" => marker(value, path, target),
    name if MEDIA_REFERENCES.contains(&name) => media_reference(value, path, target),
    name if EFFECTS.contains(&name) => effect(value, path, target),
    "RationalTime.1" => rational_time(value, path, target),
    "TimeRange.1" => time_range(value, path, target),
    "TimeTransform.1" => time_transform(value, path, target),
    "V2d.1" => v2d(value, path, target),
    "Box2d.1" => box2d(value, path, target),
    "Color.1" => color(value, path, target),
    _ => Err(shape(
      &format!("{path}.OTIO_SCHEMA"),
      "a schema whose rules the check knows",
    )),
  }
}

/// The members a schema's reader leaves unread, given the keys it reads: a
/// reader keeps them as the object's own, decoded as any others are
/// ([`contents`]).
fn others(
  members: &Members,
  path: &str,
  target: OtioTarget,
  read: &[&[&str]],
) -> Result<(), Shape> {
  for (key, value) in members {
    let key = key.as_str();
    if key != "OTIO_SCHEMA" && !read.iter().any(|keys| keys.contains(&key)) {
      contents(value, &format!("{path}.{key}"), target)?;
    }
  }
  Ok(())
}

fn time_range(value: &Value, path: &str, target: OtioTarget) -> Result<(), Shape> {
  let members = schema(value, path, &["TimeRange.1"], "the schema TimeRange.1")?;
  let (duration, at) = required(members, path, "duration")?;
  length(duration, &at, target)?;
  let (start, at) = required(members, path, "start_time")?;
  rational_time(start, &at, target)?;
  others(members, path, target, &[&["duration", "start_time"]])
}

/// A `RationalTime.1` that measures a length: no shorter than nothing.
fn length(value: &Value, path: &str, target: OtioTarget) -> Result<(), Shape> {
  rational_time(value, path, target)?;
  let members = object(value, path)?;
  match member(members, "value").and_then(Value::as_f64) {
    Some(value) if value >= 0.0 => Ok(()),
    _ => Err(shape(&format!("{path}.value"), "a length of zero or more")),
  }
}

fn rational_time(value: &Value, path: &str, target: OtioTarget) -> Result<(), Shape> {
  let members = schema(
    value,
    path,
    &["RationalTime.1"],
    "the schema RationalTime.1",
  )?;
  let (rate, at) = required(members, path, "rate")?;
  match rate.as_f64() {
    Some(rate) if rate.is_finite() && rate > 0.0 => {}
    _ => return Err(shape(&at, "a positive, finite rate")),
  }
  let (count, at) = required(members, path, "value")?;
  match count.as_f64() {
    Some(count) if count.is_finite() => {}
    _ => return Err(shape(&at, "a finite number")),
  }
  others(members, path, target, &[&["rate", "value"]])
}

fn time_transform(value: &Value, path: &str, target: OtioTarget) -> Result<(), Shape> {
  let members = schema(
    value,
    path,
    &["TimeTransform.1"],
    "the schema TimeTransform.1",
  )?;
  let (offset, at) = required(members, path, "offset")?;
  rational_time(offset, &at, target)?;
  for key in ["rate", "scale"] {
    let (value, at) = required(members, path, key)?;
    number(value, &at)?;
  }
  others(members, path, target, &[&["offset", "rate", "scale"]])
}

fn v2d(value: &Value, path: &str, target: OtioTarget) -> Result<(), Shape> {
  let members = schema(value, path, &["V2d.1"], "the schema V2d.1")?;
  for key in ["x", "y"] {
    let (value, at) = required(members, path, key)?;
    number(value, &at)?;
  }
  others(members, path, target, &[&["x", "y"]])
}

fn box2d(value: &Value, path: &str, target: OtioTarget) -> Result<(), Shape> {
  let members = schema(value, path, &["Box2d.1"], "the schema Box2d.1")?;
  for key in ["min", "max"] {
    let (corner, at) = required(members, path, key)?;
    v2d(corner, &at, target)?;
  }
  others(members, path, target, &[&["min", "max"]])
}

fn color(value: &Value, path: &str, target: OtioTarget) -> Result<(), Shape> {
  let members = schema(value, path, &["Color.1"], "the schema Color.1")?;
  let (name, at) = required(members, path, "name")?;
  string(name, &at)?;
  for key in ["r", "g", "b", "a"] {
    let (value, at) = required(members, path, key)?;
    number(value, &at)?;
  }
  others(members, path, target, &[&["name", "r", "g", "b", "a"]])
}

fn schema<'a>(
  value: &'a Value,
  path: &str,
  allowed: &[&str],
  expected: &'static str,
) -> Result<&'a Members, Shape> {
  let members = object(value, path)?;
  let name = schema_name(members, path)?;
  if allowed.contains(&name) {
    Ok(members)
  } else {
    Err(shape(&format!("{path}.OTIO_SCHEMA"), expected))
  }
}

fn schema_name<'a>(members: &'a Members, path: &str) -> Result<&'a str, Shape> {
  member(members, "OTIO_SCHEMA")
    .and_then(Value::as_str)
    .ok_or_else(|| shape(&format!("{path}.OTIO_SCHEMA"), "a schema name"))
}

fn required<'a>(
  members: &'a Members,
  path: &str,
  key: &str,
) -> Result<(&'a Value, Utf8Bytes), Shape> {
  let at = format_utf8bytes!("{path}.{key}");
  match member(members, key) {
    Some(value) => Ok((value, at)),
    None => Err(shape(&at, "a required key")),
  }
}

fn optional(
  members: &Members,
  path: &str,
  key: &str,
  check: impl Fn(&Value, &str) -> Result<(), Shape>,
) -> Result<(), Shape> {
  match member(members, key) {
    Some(value) => check(value, &format!("{path}.{key}")),
    None => Ok(()),
  }
}

fn nullable(
  value: &Value,
  path: &str,
  check: impl Fn(&Value, &str) -> Result<(), Shape>,
) -> Result<(), Shape> {
  match value {
    Value::Null => Ok(()),
    value => check(value, path),
  }
}

fn member<'a>(members: &'a Members, key: &str) -> Option<&'a Value> {
  members
    .iter()
    .find(|(name, _)| name == key)
    .map(|(_, value)| value)
}

fn object<'a>(value: &'a Value, path: &str) -> Result<&'a Members, Shape> {
  value.as_object().ok_or_else(|| shape(path, "an object"))
}

fn array<'a>(value: &'a Value, path: &str) -> Result<&'a [Value], Shape> {
  value.as_array().ok_or_else(|| shape(path, "an array"))
}

fn string(value: &Value, path: &str) -> Result<(), Shape> {
  string_value(value, path).map(drop)
}

fn string_value<'a>(value: &'a Value, path: &str) -> Result<&'a str, Shape> {
  value.as_str().ok_or_else(|| shape(path, "a string"))
}

/// A whole number, what a reader takes for an `int64_t`: one RapidJSON
/// reads as an integer — written without a fraction or an exponent, `-0`
/// among them, and up to 2^64 − 1, which the handler keeps cut to its low 63
/// bits (`deserialization.cpp:62–73` in 0.17.0) — not one it reads as a
/// `double`, which the reader refuses there (`:457–487`).
fn integer(value: &Value, path: &str) -> Result<(), Shape> {
  let whole = match value {
    Value::Number(text) => matches!(json::held(text), Some(Held::Integer(_))),
    _ => false,
  };
  if whole {
    Ok(())
  } else {
    Err(shape(
      path,
      "a whole number, without a fraction or an exponent",
    ))
  }
}

/// A number: what a reader takes for a `double` — any RapidJSON reads,
/// `NaN` and the infinities among them.
fn number(value: &Value, path: &str) -> Result<(), Shape> {
  match value.as_f64() {
    Some(_) => Ok(()),
    None => Err(shape(path, "a number")),
  }
}

fn shape(path: &str, expected: &'static str) -> Shape {
  Shape {
    path: Utf8Bytes::from(path),
    expected,
  }
}
