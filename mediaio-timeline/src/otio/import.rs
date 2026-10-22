//! OpenTimelineIO read back into a timeline: the exact inverse of the
//! export, and a refusal by name of everything the timeline does not model.
//!
//! The reader walks the document the export writes — `Timeline.1`, one
//! `Stack.1`, its `Track.1`s, their `Gap.1`s, `Clip.1`s or `Clip.2`s and
//! `Transition.1`s, each clip's `ExternalReference.1`, and `Marker.2`s on
//! the stack, tracks and clips — and nothing else: a schema the timeline
//! does not model, or models elsewhere, is refused with its name and where
//! it sits ([`Unreadable::Schema`]); a key, a value or a note of a modelled
//! schema the timeline has no word for is refused by name too
//! ([`Unreadable::Feature`]). On the timeline, a track, a clip and a
//! marker, each key of the `metadata` beside `mediaio` is another reader's
//! namespace, read whole into the object's [`ReaderMetadata`]: an object
//! of strings, any other value inside one refused by name
//! ([`Feature::NamespaceValue`]); on every other object, and where its
//! value is no object, such a key is another writer's note, refused by
//! name ([`Feature::Metadata`]). Nothing is dropped: a member is read only
//! where the timeline keeps it, or where OpenTimelineIO 0.17.0 holds it as
//! it holds the member's absence — and the export writes it back so (the
//! backstop, below). A member OpenTimelineIO keeps and the timeline does not
//! — a `color` on an item, an `enabled` on a transition, which it keeps as
//! dynamic fields — is refused by name, `null` and `true` among them.
//!
//! A member OpenTimelineIO's schema lets a document leave out reads as
//! OpenTimelineIO 0.17.0 reads it — its reader builds each object with its
//! default constructor and reads such a member only if present — so an
//! absent name reads as empty, an item's absent `enabled` as `true`, and a
//! marker's absent color as GREEN and its comment as empty; a `Clip.1` with
//! no media reference is a clip with no medium, refused by name as a `null`
//! one is. A member whose default is no value at all, where the timeline
//! needs one — the timeline's `global_start_time`, a clip's `source_range`
//! — is refused by name, and so is a gap's absent `source_range`, whose
//! default, `TimeRange()`, is nothing at rate 1: a rate the timeline does
//! not keep, which OpenTimelineIO's sum of the track's duration takes.
//!
//! Every number is read as OpenTimelineIO 0.17.0's parser reads it —
//! RapidJSON at the commit the release pins, whose verdict on a number's
//! text [`json::number`] gives, a number its builds read differently
//! refused — and held to the model exactly, or refused as
//! [`Unreadable::Inexact`]: a count as the whole number the `double` it
//! reads as is ([`json::double`]), a dissolve's place as the whole number a
//! dictionary keeps ([`json::whole`]), a gain as the `f32` whose shortest
//! spelling reads as the same `double` ([`json::float`]). So `2.4e1` reads
//! as `24.0` does, and `24.0000000000000001` too — RapidJSON keeps 17
//! significant digits — while `16777217` reads as no `f32`, and a count of
//! NaN, an infinity or `-0.0`, whose sign no whole count keeps, is refused
//! by name, as is a rate of NaN or an infinity. Counts are read where the
//! export writes them, and held to the exact words it
//! writes beside them ([`export`](super::export)): each `RationalTime`'s
//! rate must be the `f64` of the exact ruler its word names, bit for bit,
//! and its value a whole count within ±2^53, which an `f64` holds exactly;
//! a media-side range is counted again in the timebase the model counts it
//! in, exactly. An edit-side value is counted in the timebase its word
//! spells, which must be the edit rate's reciprocal by value, and without a
//! word in that reciprocal; the model's notes are pairs, each key once.
//! What OpenTimelineIO derives from the document is held as the export
//! holds it ([`derive`](super::derive)), and refused as
//! [`Unreadable::Inexact`] where its `f64` arithmetic rounds: both ends of
//! every range read, and each track's walk and the stack's.
//! Records are not written: like OpenTimelineIO, the reader lays a track's
//! items end to end from zero, each clip as long as its source range at the
//! edit rate. A track's time ends with its last clip, so a gap of some
//! length after it — which OpenTimelineIO counts in the track's duration —
//! is refused by name ([`Feature::TrailingGap`]). A gap of no length is
//! read only where the export writes one, on a fade's gap side, and as it
//! writes one, its range stated at the edit rate; anywhere else
//! OpenTimelineIO still counts it — its range in its duration, its rate in
//! its track's sum — so it is refused by name too
//! ([`Feature::StandaloneGap`]).
//!
//! Last comes the backstop ([`reproduced`]): the export writes the timeline
//! read, and every value it writes must be the one the document states at
//! the same path, as OpenTimelineIO 0.17.0 holds it — a string byte for
//! byte, a boolean exactly, a `RationalTime`'s numbers as doubles, a
//! dictionary's as the integers or doubles RapidJSON reads them as, two
//! doubles bit for bit — or the read
//! is refused as inexact there, naming both values. A member the export
//! writes may be missing from the document only where OpenTimelineIO reads
//! its absence as that value ([`WRITTEN_DEFAULTS`]), and one the document
//! states may go unwritten only where OpenTimelineIO holds it as its
//! absence ([`STATED_DEFAULTS`]). The checks before it refuse by name what
//! they know; the backstop closes the class of what is read but would be
//! written otherwise — a medium's range in a ruler the export would not
//! choose, a marker's color in another case — so the reader accepts exactly
//! the documents the export reproduces.

use core::str::FromStr;
use std::{boxed::Box, vec, vec::Vec};

use mediatime::{Duration, Rate, Rounding, TimeRange, Timebase, Timestamp};
use smol_bytes::{Utf8Bytes, Utf8BytesMut, format_utf8bytes};

use super::{
  ChildAt, Feature, Inexact, NotRepresentable, OtioTarget, Shape, Spot, UnmodelledFeature,
  UnmodelledSchema, Unreadable,
  derive::{self, Ruler, Time},
  export::{self, DEFAULT_MEDIA, OURS},
  json::{self, Held, Value},
};
use crate::{
  Clip, ClipId, Edge, Fade, FadeShape, Gain, Marker, MarkerColor, MediaRef, Metadata,
  ReaderMetadata, Timeline, Track, TrackKind, Transition, layout::layout_valid,
};

type Members = [(Utf8Bytes, Value)];

/// Where a value sits, as a path from the document's root:
/// `$.tracks.children[0]`.
#[derive(Clone)]
struct At(Utf8Bytes);

impl At {
  fn root() -> Self {
    Self(Utf8Bytes::from_static("$"))
  }

  fn key(&self, key: &str) -> Self {
    Self(format_utf8bytes!("{}.{key}", self.0))
  }

  fn index(&self, index: usize) -> Self {
    Self(format_utf8bytes!("{}[{index}]", self.0))
  }
}

fn shape(at: &At, expected: &'static str) -> Unreadable {
  Unreadable::Shape(Shape {
    path: at.0.clone(),
    expected,
  })
}

fn unmodelled(at: &At, name: &str) -> Unreadable {
  Unreadable::Schema(UnmodelledSchema::new(Utf8Bytes::from(name), at.0.clone()))
}

fn feature(at: &At, feature: Feature) -> Unreadable {
  Unreadable::Feature(UnmodelledFeature::new(feature, at.0.clone()))
}

fn inexact(at: &At, reason: &'static str) -> Unreadable {
  Unreadable::Inexact(Inexact::new(at.0.clone(), reason))
}

fn array<'a>(value: &'a Value, at: &At) -> Result<&'a [Value], Unreadable> {
  value.as_array().ok_or_else(|| shape(at, "an array"))
}

fn number<'a>(value: &'a Value, at: &At) -> Result<&'a str, Unreadable> {
  match value {
    Value::Number(text) => Ok(text.as_str()),
    _ => Err(shape(at, "a number")),
  }
}

/// An object of the document, and where it sits.
struct Object<'a> {
  members: &'a Members,
  at: At,
}

impl<'a> Object<'a> {
  fn of(value: &'a Value, at: At) -> Result<Self, Unreadable> {
    match value.as_object() {
      Some(members) => Ok(Self { members, at }),
      None => Err(shape(&at, "an object")),
    }
  }

  fn get(&self, key: &str) -> Option<&'a Value> {
    self
      .members
      .iter()
      .find(|(name, _)| name == key)
      .map(|(_, value)| value)
  }

  fn required(&self, key: &str) -> Result<&'a Value, Unreadable> {
    self
      .get(key)
      .ok_or_else(|| shape(&self.at.key(key), "a required key"))
  }

  /// The object's schema, refused by name where it is not one of `known`.
  fn schema(&self, known: &[&str]) -> Result<&'a str, Unreadable> {
    let name = self
      .get("OTIO_SCHEMA")
      .and_then(Value::as_str)
      .ok_or_else(|| shape(&self.at.key("OTIO_SCHEMA"), "a schema name"))?;
    if known.contains(&name) {
      Ok(name)
    } else {
      Err(unmodelled(&self.at, name))
    }
  }

  /// Refuses the first key not among `known`, by name.
  fn keys(&self, known: &[&str]) -> Result<(), Unreadable> {
    match self
      .members
      .iter()
      .find(|(key, _)| !known.contains(&key.as_str()))
    {
      Some((key, _)) => Err(feature(&self.at.key(key), Feature::Key(key.clone()))),
      None => Ok(()),
    }
  }

  fn string(&self, key: &str) -> Result<Option<&'a str>, Unreadable> {
    match self.get(key) {
      None => Ok(None),
      Some(value) => value
        .as_str()
        .map(Some)
        .ok_or_else(|| shape(&self.at.key(key), "a string")),
    }
  }

  fn boolean(&self, key: &str) -> Result<Option<bool>, Unreadable> {
    match self.get(key) {
      None => Ok(None),
      Some(Value::Bool(flag)) => Ok(Some(*flag)),
      Some(_) => Err(shape(&self.at.key(key), "a boolean")),
    }
  }

  /// The object's name, where the timeline keeps it; absent reads as empty.
  fn name(&self) -> Result<&'a str, Unreadable> {
    Ok(self.string("name")?.unwrap_or(""))
  }

  /// A name where the timeline keeps none: empty, or one of `also`.
  fn no_name(&self, also: &[&str]) -> Result<(), Unreadable> {
    match self.string("name")? {
      None | Some("") => Ok(()),
      Some(name) if also.contains(&name) => Ok(()),
      Some(name) => Err(feature(
        &self.at.key("name"),
        Feature::Name(Utf8Bytes::from(name)),
      )),
    }
  }

  /// Whether the item plays: `enabled`, absent reading as `true`.
  fn enabled(&self) -> Result<bool, Unreadable> {
    Ok(self.boolean("enabled")?.unwrap_or(true))
  }

  /// An item the timeline cannot disable — the stack, a gap, a transition:
  /// `enabled` absent or `true`.
  fn never_disabled(&self) -> Result<(), Unreadable> {
    if self.enabled()? {
      Ok(())
    } else {
      Err(feature(&self.at.key("enabled"), Feature::Disabled))
    }
  }

  /// An item's `color`, a newer reader's word the timeline has none for:
  /// absent or `null`.
  /// No `color` on an item: OpenTimelineIO 0.17.0's items have none, and
  /// keep one stated — `null` among them — as a dynamic field they write
  /// back (`serializableObject.cpp` 78–107), which the timeline would drop.
  fn no_color(&self) -> Result<(), Unreadable> {
    match self.get("color") {
      None => Ok(()),
      Some(_) => Err(feature(&self.at.key("color"), Feature::ItemColor)),
    }
  }

  /// A trim on the stack or a track, which the timeline has no word for:
  /// `source_range` absent or `null`.
  fn no_trim(&self) -> Result<(), Unreadable> {
    match self.get("source_range") {
      None | Some(Value::Null) => Ok(()),
      Some(_) => Err(feature(&self.at.key("source_range"), Feature::Trim)),
    }
  }

  /// An item's effects: none. An effect of any schema is one the timeline
  /// does not model, refused with its name.
  fn no_effects(&self) -> Result<(), Unreadable> {
    self.no_schemas("effects")
  }

  /// Markers where the timeline keeps none — on a gap: none, each refused
  /// as a schema the timeline does not model there.
  fn no_markers(&self) -> Result<(), Unreadable> {
    self.no_schemas("markers")
  }

  fn no_schemas(&self, key: &str) -> Result<(), Unreadable> {
    let Some(value) = self.get(key) else {
      return Ok(());
    };
    let at = self.at.key(key);
    match array(value, &at)?.first() {
      None => Ok(()),
      Some(first) => {
        let at = at.index(0);
        let entry = Object::of(first, at.clone())?;
        let name = entry
          .get("OTIO_SCHEMA")
          .and_then(Value::as_str)
          .ok_or_else(|| shape(&at.key("OTIO_SCHEMA"), "a schema name"))?;
        Err(unmodelled(&at, name))
      }
    }
  }
}

/// This crate's words in an object's `metadata`: the `mediaio` object. Any
/// other key of the metadata is another reader's namespace, on the objects
/// that keep one ([`Words::with_readers`]), and otherwise another writer's
/// note, which the timeline does not keep, refused by name; so is a word
/// this reader does not know.
struct Words<'a> {
  words: Option<Object<'a>>,
  at: At,
}

impl<'a> Words<'a> {
  /// The words of `owner`, an object that keeps no reader's namespace,
  /// refusing any but `known`, and every other key of its metadata by name.
  fn of(owner: &Object<'a>, known: &[&str]) -> Result<Self, Unreadable> {
    let metadata = metadata(owner)?;
    if let Some(metadata) = &metadata
      && let Some((key, _)) = metadata.members.iter().find(|(key, _)| key != OURS)
    {
      return Err(feature(
        &metadata.at.key(key),
        Feature::Metadata(key.clone()),
      ));
    }
    Self::ours(owner, metadata.as_ref(), known)
  }

  /// The words of `owner`, refusing any but `known`, and every other key of
  /// its metadata as another reader's namespace, whole: an object of
  /// strings — any other value inside one refused by name
  /// ([`Feature::NamespaceValue`]) — and, as a key whose value is no
  /// object, another writer's note ([`Feature::Metadata`]). An empty
  /// object is an empty namespace, as the export writes one.
  fn with_readers(
    owner: &Object<'a>,
    known: &[&str],
  ) -> Result<(Self, ReaderMetadata), Unreadable> {
    let metadata = metadata(owner)?;
    let mut readers = ReaderMetadata::new();
    if let Some(metadata) = &metadata {
      for (namespace, value) in metadata.members.iter().filter(|(key, _)| key != OURS) {
        let at = metadata.at.key(namespace);
        let Some(entries) = value.as_object() else {
          return Err(feature(&at, Feature::Metadata(namespace.clone())));
        };
        let mut words = Metadata::new();
        for (key, value) in entries {
          let Value::String(entry) = value else {
            return Err(feature(&at.key(key), Feature::NamespaceValue(key.clone())));
          };
          // The parser refuses an object naming a key twice.
          words.insert(key.clone(), entry.clone());
        }
        readers.insert(namespace.clone(), words);
      }
    }
    Ok((Self::ours(owner, metadata.as_ref(), known)?, readers))
  }

  /// The `mediaio` object of `owner`'s `metadata`, refusing any word but
  /// `known`.
  fn ours(
    owner: &Object<'a>,
    metadata: Option<&Object<'a>>,
    known: &[&str],
  ) -> Result<Self, Unreadable> {
    let ours = owner.at.key("metadata").key(OURS);
    let Some(words) = metadata.and_then(|metadata| metadata.get(OURS)) else {
      return Ok(Self {
        words: None,
        at: ours,
      });
    };
    let words = Object::of(words, ours.clone())?;
    if let Some((key, _)) = words
      .members
      .iter()
      .find(|(key, _)| !known.contains(&key.as_str()))
    {
      return Err(feature(&ours.key(key), Feature::Word(key.clone())));
    }
    Ok(Self {
      words: Some(words),
      at: ours,
    })
  }

  fn get(&self, word: &str) -> Option<&'a Value> {
    self.words.as_ref().and_then(|words| words.get(word))
  }

  fn text(&self, word: &str) -> Result<Option<&'a str>, Unreadable> {
    match self.get(word) {
      None => Ok(None),
      Some(value) => value
        .as_str()
        .map(Some)
        .ok_or_else(|| shape(&self.at.key(word), "a string")),
    }
  }

  fn required_text(&self, word: &str) -> Result<&'a str, Unreadable> {
    self
      .text(word)?
      .ok_or_else(|| shape(&self.at.key(word), "a required word"))
  }

  /// A rate written `num/den`, above zero.
  fn rate(&self, word: &str) -> Result<Option<Rate>, Unreadable> {
    self
      .text(word)?
      .map(|text| rate(text, &self.at.key(word)))
      .transpose()
  }

  /// A gain in decibels: the `f32` whose shortest spelling reads back as
  /// the `double` its number reads as ([`json::float`]), finite so; NaN and
  /// an infinity refused by name.
  fn gain(&self, word: &str) -> Result<Option<Gain>, Unreadable> {
    let Some(value) = self.get(word) else {
      return Ok(None);
    };
    let at = self.at.key(word);
    let text = number(value, &at)?;
    if json::double(text).is_some_and(|read| !read.is_finite()) {
      return Err(inexact(
        &at,
        "a gain of NaN or an infinity, which no gain holds",
      ));
    }
    json::float(text)
      .and_then(Gain::from_db)
      .map(Some)
      .ok_or_else(|| inexact(&at, "a gain that is not exactly the f32 it reads as"))
  }

  /// The exact words of a media-side range: the ruler its counts are
  /// written in and the timebase the model counts it in.
  fn exact(&self, word: &str) -> Result<Option<Exact>, Unreadable> {
    let Some(value) = self.get(word) else {
      return Ok(None);
    };
    let at = self.at.key(word);
    let object = Object::of(value, at.clone())?;
    if let Some((key, _)) = object
      .members
      .iter()
      .find(|(key, _)| key != "ruler" && key != "timebase")
    {
      return Err(feature(&at.key(key), Feature::Word(key.clone())));
    }
    let ruler = object
      .string("ruler")?
      .ok_or_else(|| shape(&at.key("ruler"), "a required word"))?;
    let timebase = object
      .string("timebase")?
      .ok_or_else(|| shape(&at.key("timebase"), "a required word"))?;
    let timebase_at = at.key("timebase");
    Ok(Some(Exact {
      ruler: rate(ruler, &at.key("ruler"))?,
      timebase: Timebase::from_str(timebase)
        .ok()
        .filter(|timebase| timebase.num() > 0)
        .ok_or_else(|| shape(&timebase_at, "a timebase above zero, written num/den"))?,
    }))
  }

  fn required_exact(&self, word: &str) -> Result<Exact, Unreadable> {
    self
      .exact(word)?
      .ok_or_else(|| shape(&self.at.key(word), "a required word"))
  }

  /// The model's notes, under `metadata`: an array of `[key, value]`
  /// pairs of strings, each key once — never an object, whose keys a
  /// reader would take for schema words (see the export).
  fn notes(&self) -> Result<Metadata, Unreadable> {
    let Some(value) = self.get("metadata") else {
      return Ok(Metadata::new());
    };
    let at = self.at.key("metadata");
    let mut notes = Metadata::new();
    for (index, pair) in array(value, &at)?.iter().enumerate() {
      let at = at.index(index);
      let Some([Value::String(key), Value::String(note)]) = pair.as_array() else {
        return Err(shape(&at, "a note: a key and a value, both strings"));
      };
      if notes.insert(key.clone(), note.clone()).is_some() {
        return Err(shape(&at.index(0), "a key no earlier note names"));
      }
    }
    Ok(notes)
  }

  /// The timebase an edit-side value is counted in: the one its word
  /// spells — the edit rate's reciprocal by value, spelled otherwise, as
  /// the export writes it — or, with no word, the reciprocal `ticks`
  /// itself. Either counts the same ticks.
  fn timebase(&self, word: &str, ticks: Timebase) -> Result<Timebase, Unreadable> {
    let Some(text) = self.text(word)? else {
      return Ok(ticks);
    };
    Timebase::from_str(text)
      .ok()
      .filter(|timebase| *timebase == ticks)
      .ok_or_else(|| {
        shape(
          &self.at.key(word),
          "the edit rate's timebase, written num/den",
        )
      })
  }
}

/// `owner`'s `metadata`, an object, if it states one.
fn metadata<'a>(owner: &Object<'a>) -> Result<Option<Object<'a>>, Unreadable> {
  owner
    .get("metadata")
    .map(|metadata| Object::of(metadata, owner.at.key("metadata")))
    .transpose()
}

/// A rate written `num/den`, above zero.
fn rate(text: &str, at: &At) -> Result<Rate, Unreadable> {
  Rate::from_str(text)
    .ok()
    .filter(|rate| rate.num() > 0)
    .ok_or_else(|| shape(at, "a rate above zero, written num/den"))
}

/// The exact ruler a media-side range is written in, and the timebase the
/// model counts it in.
#[derive(Clone, Copy)]
struct Exact {
  ruler: Rate,
  timebase: Timebase,
}

/// A `RationalTime.1` counted in `ruler`, its two numbers read as
/// OpenTimelineIO 0.17.0 reads them, as `double`s ([`json::double`]): its
/// rate the `f64` of the exact ruler, bit for bit; its value a whole count
/// within ±2^53. NaN and an infinity are refused by name, in either, and a
/// count of `-0.0`, whose sign no whole count keeps.
fn count(value: &Value, at: &At, ruler: Ruler) -> Result<i64, Unreadable> {
  /// 2^53.
  const EXACT: f64 = 9_007_199_254_740_992.0;
  let object = Object::of(value, at.clone())?;
  object.schema(&["RationalTime.1"])?;
  object.keys(&["OTIO_SCHEMA", "rate", "value"])?;
  let rate_at = at.key("rate");
  let rate = double(object.required("rate")?, &rate_at)?;
  if !rate.is_finite() {
    return Err(inexact(
      &rate_at,
      "a rate of NaN or an infinity, which no ruler holds",
    ));
  }
  if rate.to_bits() != ruler.otio().to_bits() {
    return Err(inexact(
      &rate_at,
      "a rate other than the f64 of the exact ruler it is counted in",
    ));
  }
  let value_at = at.key("value");
  let count = double(object.required("value")?, &value_at)?;
  if !count.is_finite() {
    return Err(inexact(
      &value_at,
      "a count of NaN or an infinity, which no whole count holds",
    ));
  }
  if count.to_bits() == (-0.0_f64).to_bits() {
    return Err(inexact(
      &value_at,
      "a count of negative zero, whose sign no whole count keeps",
    ));
  }
  if !(-EXACT..=EXACT).contains(&count) {
    return Err(inexact(
      &value_at,
      "a count past 2^53, which an f64 does not hold exactly",
    ));
  }
  let whole = json::whole_double(count)
    .ok_or_else(|| inexact(&value_at, "a count that is no whole number"))?;
  // Within ±2^53, so within `i64`.
  Ok(whole as i64)
}

/// The `double` OpenTimelineIO 0.17.0 reads a number as where it reads a
/// member as one ([`json::double`]).
fn double(value: &Value, at: &At) -> Result<f64, Unreadable> {
  json::double(number(value, at)?)
    .ok_or_else(|| inexact(at, "a number OpenTimelineIO 0.17.0 does not read"))
}

/// A count that measures a length: zero or more.
fn length(value: &Value, at: &At, ruler: Ruler) -> Result<u64, Unreadable> {
  u64::try_from(count(value, at, ruler)?)
    .map_err(|_| shape(&at.key("value"), "a length of zero or more"))
}

/// A `TimeRange.1` counted in `ruler`: its start and its length, and both
/// ends OpenTimelineIO derives from them held ([`derive::range_held`]) —
/// its end, the two added in `f64`, and its last tick — as the export holds
/// every range it writes.
fn counts(value: &Value, at: &At, ruler: Ruler) -> Result<(i64, u64), Unreadable> {
  let object = Object::of(value, at.clone())?;
  object.schema(&["TimeRange.1"])?;
  object.keys(&["OTIO_SCHEMA", "duration", "start_time"])?;
  let start = count(object.required("start_time")?, &at.key("start_time"), ruler)?;
  let length = length(object.required("duration")?, &at.key("duration"), ruler)?;
  if !derive::range_held(written(start, ruler), written(length, ruler)) {
    return Err(inexact(
      at,
      "a range whose end, or last tick, OpenTimelineIO's f64 arithmetic rounds",
    ));
  }
  Ok((start, length))
}

/// `count` ticks of `ruler`, as the document writes them, for the
/// arithmetic OpenTimelineIO does on them ([`Time`]).
fn written(count: impl Into<i128>, ruler: Ruler) -> Time {
  Time::written(count.into(), ruler)
}

/// What OpenTimelineIO's `f64` arithmetic rounds in the walk of a track
/// or of the stack at `stack` ([`derive::track`], [`derive::stack`]),
/// refused where it is derived: at a child of a track, at a track, or at
/// the stack.
fn rounded(refusal: &NotRepresentable, stack: &At) -> Unreadable {
  let track = |index: usize| stack.key("children").index(index);
  let child = |at: ChildAt| track(at.track()).key("children").index(at.child());
  match refusal.at() {
    Spot::TrackPosition(at) => inexact(
      &child(at),
      "a place on its track that OpenTimelineIO's f64 arithmetic rounds",
    ),
    Spot::Visible(at) => inexact(
      &child(at),
      "a visible range that OpenTimelineIO's f64 arithmetic rounds",
    ),
    Spot::Absolute(at) => inexact(
      &child(at),
      "a place from the global start that OpenTimelineIO's f64 arithmetic rounds",
    ),
    Spot::TrackDuration(index) => inexact(
      &track(index),
      "a track duration that OpenTimelineIO's f64 arithmetic rounds",
    ),
    Spot::AbsoluteEnd(index) => inexact(
      &track(index),
      "a track's end from the global start that OpenTimelineIO's f64 arithmetic rounds",
    ),
    Spot::StackDuration => inexact(
      stack,
      "tracks whose longest OpenTimelineIO's f64 comparison does not pick",
    ),
    Spot::TimelineEnd => inexact(
      stack,
      "a timeline end from the global start that OpenTimelineIO's f64 arithmetic rounds",
    ),
    // Counts the document writes, which a walk does not refuse.
    Spot::Start
    | Spot::Record(_)
    | Spot::Source(_)
    | Spot::Available(_)
    | Spot::Transition(_)
    | Spot::Fade(_)
    | Spot::Marker(_) => inexact(stack, "a time OpenTimelineIO's f64 arithmetic rounds"),
  }
}

/// The range `start` and `length` ticks of `exact.ruler` make, counted
/// exactly in `exact.timebase`.
fn exact_range(start: i64, length: u64, exact: Exact, at: &At) -> Result<TimeRange, Unreadable> {
  // A rate above zero has a timebase.
  let written = exact.ruler.checked_to_timebase().unwrap_or_default();
  let start = Timestamp::new(start, written)
    .checked_rescale_with(exact.timebase, Rounding::Exact)
    .ok_or_else(|| inexact(at, "a start its timebase does not count exactly"))?;
  let length = Duration::new(length, written)
    .checked_rescale_with(exact.timebase, Rounding::Exact)
    .ok_or_else(|| inexact(at, "a length its timebase does not count exactly"))?;
  let end = i64::try_from(length.ticks())
    .ok()
    .and_then(|ticks| start.pts().checked_add(ticks))
    .ok_or_else(|| inexact(at, "an end past what its timebase counts in an i64"))?;
  Ok(TimeRange::new(start.pts(), end, exact.timebase))
}

/// The range `start` and `length` edit-rate ticks make, from the
/// timeline's zero.
fn edit_range(start: i64, length: u64, edit: Timebase) -> TimeRange {
  // Both lie within ±2^53, so their sum within `i64`.
  TimeRange::new(start, start + length as i64, edit)
}

/// Reads the timeline `root` holds — not yet validated.
pub(crate) fn timeline(root: &Value) -> Result<Timeline, Unreadable> {
  let object = Object::of(root, At::root())?;
  object.schema(&["Timeline.1"])?;
  object.keys(&[
    "OTIO_SCHEMA",
    "metadata",
    "name",
    "global_start_time",
    "tracks",
  ])?;
  let (words, readers) = Words::with_readers(&object, &["rate", "start", "metadata"])?;
  let rate = words
    .rate("rate")?
    .ok_or_else(|| shape(&words.at.key("rate"), "a required word"))?;
  let edit = Ruler::new(rate);
  let ticks = rate.checked_to_timebase().unwrap_or_default();
  let start_timebase = words.timebase("start", ticks)?;
  let start = count(
    object.required("global_start_time")?,
    &object.at.key("global_start_time"),
    edit,
  )?;
  let stack = Object::of(object.required("tracks")?, object.at.key("tracks"))?;
  let (tracks, markers) = self::stack(&stack, edit, ticks, written(start, edit))?;
  let mut timeline = Timeline::new(object.name()?, rate)
    .with_start(Timestamp::new(start, start_timebase))
    .with_metadata(words.notes()?)
    .with_tracks(tracks)
    .with_readers(readers);
  *timeline.markers_mut() = markers;
  Ok(timeline)
}

/// The one `Stack.1`, named `tracks` as OpenTimelineIO names it, holding
/// the tracks and the timeline's markers — read as a track's are, at the
/// edit rate from the stack's zero — and nothing the timeline does not
/// keep. Each track is walked as the export walks it, from the global start
/// `global`, and then the stack ([`derive::track`], [`derive::stack`]):
/// what OpenTimelineIO's `f64` arithmetic rounds there is refused
/// ([`rounded`]).
fn stack(
  stack: &Object<'_>,
  edit: Ruler,
  ticks: Timebase,
  global: Time,
) -> Result<(Vec<Track>, Vec<Marker>), Unreadable> {
  stack.schema(&["Stack.1"])?;
  stack.keys(&[
    "OTIO_SCHEMA",
    "metadata",
    "name",
    "source_range",
    "effects",
    "markers",
    "enabled",
    "color",
    "children",
  ])?;
  Words::of(stack, &[])?;
  stack.no_name(&["tracks"])?;
  stack.no_trim()?;
  stack.no_effects()?;
  let markers = markers(stack, Placed::Edit(edit, ticks))?;
  stack.never_disabled()?;
  stack.no_color()?;
  let at = stack.at.key("children");
  let children = array(stack.required("children")?, &at)?;
  let mut tracks = Vec::with_capacity(children.len());
  let mut durations = Vec::with_capacity(children.len());
  for (index, child) in children.iter().enumerate() {
    let (track, walk) = track(&Object::of(child, at.index(index))?, edit, ticks)?;
    let duration =
      derive::track(index, &walk, global).map_err(|refusal| rounded(&refusal, &stack.at))?;
    tracks.push(track);
    durations.push(duration);
  }
  derive::stack(&durations, global).map_err(|refusal| rounded(&refusal, &stack.at))?;
  Ok((tracks, markers))
}

/// A `Track.1`, and its children as OpenTimelineIO's walk reads them.
fn track(
  object: &Object<'_>,
  edit: Ruler,
  ticks: Timebase,
) -> Result<(Track, Vec<derive::Child>), Unreadable> {
  object.schema(&["Track.1"])?;
  object.keys(&[
    "OTIO_SCHEMA",
    "metadata",
    "name",
    "source_range",
    "effects",
    "markers",
    "enabled",
    "color",
    "children",
    "kind",
  ])?;
  let (_, readers) = Words::with_readers(object, &[])?;
  object.no_trim()?;
  object.no_effects()?;
  object.no_color()?;
  let kind = match object.string("kind")? {
    Some("Video") => TrackKind::Video,
    Some("Audio") => TrackKind::Audio,
    Some(other) => {
      return Err(feature(
        &object.at.key("kind"),
        Feature::TrackKind(Utf8Bytes::from(other)),
      ));
    }
    None => return Err(shape(&object.at.key("kind"), "a required key")),
  };
  let mut track = Track::new(kind, object.name()?)
    .with_enabled(object.enabled()?)
    .with_readers(readers);
  let Children {
    clips,
    transitions,
    walk,
  } = children(object, edit, ticks)?;
  *track.clips_mut() = clips;
  *track.transitions_mut() = transitions;
  *track.markers_mut() = markers(object, Placed::Edit(edit, ticks))?;
  Ok((track, walk))
}

/// A track's children as read: its clips, each at its record, its
/// transitions, and the children as OpenTimelineIO's walk reads them.
struct Children {
  clips: Vec<Clip>,
  transitions: Vec<Transition>,
  walk: Vec<derive::Child>,
}

/// One child of a track as read, before the track places it.
enum Child {
  /// A gap, so many edit-rate ticks long.
  Gap(u64),
  /// A clip, and its length at the edit rate.
  Clip(Box<Clip>, u64),
  /// A fade at one edge of the clip beside it.
  Fade(Edge, Fade),
  /// A dissolve: its offsets, the timebase of its cut, and its index
  /// among its track's transitions where the document says it.
  Dissolve {
    in_offset: Duration,
    out_offset: Duration,
    at: Timebase,
    order: Option<usize>,
  },
}

/// What kind of child sits at a place of a track: what a transition's
/// neighbours are judged by.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
  Gap,
  Clip,
  Transition,
}

impl Child {
  const fn kind(&self) -> Kind {
    match self {
      Self::Gap(_) => Kind::Gap,
      Self::Clip(..) => Kind::Clip,
      Self::Fade(..) | Self::Dissolve { .. } => Kind::Transition,
    }
  }
}

/// A track's clips, each at its record, and its transitions: its items
/// laid end to end from zero, as OpenTimelineIO places them, each fade on
/// the clip beside it and each dissolve on the cut it straddles. The track
/// ends with its last clip: a gap of some length after it is refused by
/// name, every gap of a track with no clip among them. A gap of no length
/// is read only where the export writes one — on a fade's gap side, beside
/// no other gap — and in the form it writes, its range stated at the edit
/// rate ([`gap`]); it is refused by name anywhere else, as OpenTimelineIO
/// counts it all the same. Answers the children as OpenTimelineIO's walk
/// reads them too, each counted as the document writes it.
fn children(object: &Object<'_>, edit: Ruler, ticks: Timebase) -> Result<Children, Unreadable> {
  let at = object.at.key("children");
  let values = array(object.required("children")?, &at)?;
  let mut read = Vec::with_capacity(values.len());
  let mut walk = Vec::with_capacity(values.len());
  for (index, value) in values.iter().enumerate() {
    let child = Object::of(value, at.index(index))?;
    let (read_as, walked) = match child.schema(&["Gap.1", "Clip.1", "Clip.2", "Transition.1"])? {
      "Gap.1" => {
        let length = gap(&child, edit)?;
        let walked = derive::Child::Item {
          start: written(0, edit),
          duration: written(length, edit),
        };
        (Child::Gap(length), walked)
      }
      "Transition.1" => transition(&child, edit, ticks)?,
      schema => {
        let (clip, length, walked) = clip(&child, schema, ticks)?;
        (Child::Clip(Box::new(clip), length), walked)
      }
    };
    read.push(read_as);
    walk.push(walked);
  }

  let kinds: Vec<Kind> = read.iter().map(Child::kind).collect();
  let fades: Vec<Option<Edge>> = read
    .iter()
    .map(|child| match child {
      Child::Fade(edge, _) => Some(*edge),
      _ => None,
    })
    .collect();
  let last_clip = kinds.iter().rposition(|&kind| kind == Kind::Clip);
  let mut cursor: i64 = 0;
  let mut clips: Vec<Clip> = Vec::new();
  let mut dissolves = Vec::new();
  let mut fade_in = None;
  for (index, child) in read.into_iter().enumerate() {
    let here = at.index(index);
    let before = index
      .checked_sub(1)
      .and_then(|before| kinds.get(before).copied());
    let after = kinds.get(index + 1).copied();
    if kinds[index] == Kind::Transition
      && [before, after]
        .iter()
        .any(|neighbour| neighbour.is_none_or(|kind| kind == Kind::Transition))
    {
      return Err(shape(&here, "a transition between two items"));
    }
    match child {
      Child::Gap(length) => {
        if length > 0 && last_clip.is_none_or(|last| index > last) {
          return Err(feature(&here, Feature::TrailingGap));
        }
        // A gap of no length is read only where the export writes one: on
        // a fade's gap side — before a fade-in, after a fade-out — and
        // beside no other gap. Anywhere else OpenTimelineIO still counts
        // it, while the timeline has nothing there.
        if length == 0 {
          let fade = |at: Option<usize>| at.and_then(|at| fades.get(at).copied().flatten());
          let fade_side = fade(Some(index + 1)) == Some(Edge::In)
            || fade(index.checked_sub(1)) == Some(Edge::Out);
          if !fade_side || before == Some(Kind::Gap) || after == Some(Kind::Gap) {
            return Err(feature(&here, Feature::StandaloneGap));
          }
        }
        cursor = advance(cursor, length, &here)?;
      }
      Child::Clip(mut clip, length) => {
        let end = advance(cursor, length, &here)?;
        // The record's timebase is the one `clip` set: its word's, or the
        // edit rate's reciprocal.
        let timebase = clip.record().timebase();
        clip.set_record(TimeRange::new(cursor, end, timebase));
        if let Some(fade) = fade_in.take() {
          let fades = clip.fades().with_in(Some(fade));
          clip.set_fades(fades);
        }
        clips.push(*clip);
        cursor = end;
      }
      Child::Fade(edge, fade) => {
        // A fade runs between a gap and the clip it fades: a fade-in from
        // the gap before its clip, a fade-out to the gap after it.
        let (gap_side, clip_side) = match edge {
          Edge::In => (before, after),
          Edge::Out => (after, before),
        };
        if gap_side != Some(Kind::Gap) || clip_side != Some(Kind::Clip) {
          return Err(feature(&here, Feature::Fade));
        }
        match edge {
          Edge::In => fade_in = Some(fade),
          Edge::Out => {
            if let Some(clip) = clips.last_mut() {
              let fades = clip.fades().with_out(Some(fade));
              clip.set_fades(fades);
            }
          }
        }
      }
      Child::Dissolve {
        in_offset,
        out_offset,
        at,
        order,
      } => {
        if before != Some(Kind::Clip) || after != Some(Kind::Clip) {
          return Err(feature(&here, Feature::DissolveBesideGap));
        }
        let transition = Transition::dissolve(Timestamp::new(cursor, at), in_offset, out_offset);
        dissolves.push((transition, order, here));
      }
    }
  }
  Ok(Children {
    clips,
    transitions: ordered(dissolves)?,
    walk,
  })
}

/// `cursor` moved on `length` edit-rate ticks.
fn advance(cursor: i64, length: u64, at: &At) -> Result<i64, Unreadable> {
  i64::try_from(length)
    .ok()
    .and_then(|length| cursor.checked_add(length))
    .ok_or_else(|| inexact(at, "a place on its track past i64 edit-rate ticks"))
}

/// A track's dissolves in the order the track keeps them: their cuts'
/// order, where none says its place; else each at the place it says, every
/// place once.
fn ordered(dissolves: Vec<(Transition, Option<usize>, At)>) -> Result<Vec<Transition>, Unreadable> {
  if dissolves.iter().all(|(_, order, _)| order.is_none()) {
    return Ok(
      dissolves
        .into_iter()
        .map(|(dissolve, ..)| dissolve)
        .collect(),
    );
  }
  let mut places = vec![None; dissolves.len()];
  for (dissolve, order, at) in dissolves {
    let place = order
      .filter(|&place| places.get(place).is_some_and(Option::is_none))
      .ok_or_else(|| {
        shape(
          &at.key("metadata").key(OURS).key("order"),
          "a place among the track's transitions, said by every dissolve, each place once",
        )
      })?;
    places[place] = Some(dissolve);
  }
  Ok(places.into_iter().flatten().collect())
}

/// A `Gap.1`'s length at the edit rate: a gap carries nothing else.
///
/// Its `source_range` is required, as a clip's is, and counted at the edit
/// rate exactly — the form the export writes — as every range is
/// ([`counts`]). Without one OpenTimelineIO reads a gap as its
/// constructor's `TimeRange()` (`gap.h` 23 in 0.17.0; `item.cpp` 175 reads
/// the range only if present): nothing, at rate 1. The timeline keeps a
/// gap's length at the edit rate alone, so that rate would be lost, and it
/// joins the track's sum, which carries the higher of two rates
/// (`track.cpp` 111–141; `rationalTime.h` 283–294): a re-export, writing
/// the gap at the edit rate, could change the track's duration strictly.
fn gap(object: &Object<'_>, edit: Ruler) -> Result<u64, Unreadable> {
  object.keys(&[
    "OTIO_SCHEMA",
    "metadata",
    "name",
    "source_range",
    "effects",
    "markers",
    "enabled",
    "color",
  ])?;
  Words::of(object, &[])?;
  object.no_name(&[])?;
  object.no_effects()?;
  object.no_markers()?;
  object.never_disabled()?;
  object.no_color()?;
  let at = object.at.key("source_range");
  let (start, length) = counts(object.required("source_range")?, &at, edit)?;
  if start != 0 {
    return Err(feature(&at.key("start_time"), Feature::GapStart));
  }
  Ok(length)
}

/// A `Transition.1`: a fade, by this crate's words, or a dissolve — and
/// the transition as OpenTimelineIO's walk reads it.
fn transition(
  object: &Object<'_>,
  edit: Ruler,
  ticks: Timebase,
) -> Result<(Child, derive::Child), Unreadable> {
  // No `enabled`: OpenTimelineIO 0.17.0's transitions have none, and keep
  // one stated as a dynamic field they write back (`transition.cpp` 33–35;
  // `serializableObject.cpp` 78–107), which the timeline would drop.
  object.keys(&[
    "OTIO_SCHEMA",
    "metadata",
    "name",
    "in_offset",
    "out_offset",
    "transition_type",
  ])?;
  object.no_name(&[])?;
  match object.string("transition_type")? {
    Some("SMPTE_Dissolve") => {}
    Some(other) => {
      return Err(feature(
        &object.at.key("transition_type"),
        Feature::TransitionType(Utf8Bytes::from(other)),
      ));
    }
    None => return Err(shape(&object.at.key("transition_type"), "a required key")),
  }
  let in_offset = length(
    object.required("in_offset")?,
    &object.at.key("in_offset"),
    edit,
  )?;
  let out_offset = length(
    object.required("out_offset")?,
    &object.at.key("out_offset"),
    edit,
  )?;
  let walked = derive::Child::Transition {
    in_offset: written(in_offset, edit),
    out_offset: written(out_offset, edit),
  };
  let words = Words::of(
    object,
    &[
      "fade",
      "shape",
      "timebase",
      "order",
      "at",
      "in_offset",
      "out_offset",
    ],
  )?;
  // A word of the other kind of transition is one this reader does not
  // know there.
  let foreign = |names: &[&'static str]| -> Result<(), Unreadable> {
    match names.iter().find(|name| words.get(name).is_some()) {
      Some(name) => Err(feature(
        &words.at.key(name),
        Feature::Word(Utf8Bytes::from_static(name)),
      )),
      None => Ok(()),
    }
  };
  let Some(fade) = words.text("fade")? else {
    foreign(&["shape", "timebase"])?;
    let order = words
      .get("order")
      .map(|value| {
        let at = words.at.key("order");
        json::whole(number(value, &at)?)
          .and_then(|order| usize::try_from(order).ok())
          .ok_or_else(|| shape(&at, "a place among the track's transitions"))
      })
      .transpose()?;
    let dissolve = Child::Dissolve {
      in_offset: Duration::new(in_offset, words.timebase("in_offset", ticks)?),
      out_offset: Duration::new(out_offset, words.timebase("out_offset", ticks)?),
      at: words.timebase("at", ticks)?,
      order,
    };
    return Ok((dissolve, walked));
  };
  foreign(&["order", "at", "in_offset", "out_offset"])?;
  let edge = match fade {
    "in" => Edge::In,
    "out" => Edge::Out,
    _ => return Err(shape(&words.at.key("fade"), "in or out")),
  };
  let shape_word = words.required_text("shape")?;
  let curve = match shape_word {
    "linear" => FadeShape::Linear,
    "equal_power" => FadeShape::EqualPower,
    _ => return Err(shape(&words.at.key("shape"), "linear or equal_power")),
  };
  // A fade runs inside its clip: it plays nothing of the gap's side.
  let (inside, gap_side) = match edge {
    Edge::In => (out_offset, in_offset),
    Edge::Out => (in_offset, out_offset),
  };
  if gap_side != 0 {
    return Err(feature(&object.at, Feature::Fade));
  }
  let timebase = words.timebase("timebase", ticks)?;
  Ok((
    Child::Fade(edge, Fade::new(Duration::new(inside, timebase), curve)),
    walked,
  ))
}

/// A `Clip.1` or a `Clip.2`, its length at the edit rate — its record is
/// the track's to place — and the clip as OpenTimelineIO's walk reads it:
/// its source range, counted in the ruler the document writes it in.
fn clip(
  object: &Object<'_>,
  schema: &str,
  ticks: Timebase,
) -> Result<(Clip, u64, derive::Child), Unreadable> {
  let references: &[&str] = match schema {
    "Clip.2" => &["media_references", "active_media_reference_key"],
    _ => &["media_reference"],
  };
  let mut known = vec![
    "OTIO_SCHEMA",
    "metadata",
    "name",
    "source_range",
    "effects",
    "markers",
    "enabled",
    "color",
  ];
  known.extend_from_slice(references);
  object.keys(&known)?;
  let (words, readers) =
    Words::with_readers(object, &["id", "gain_db", "source", "record", "metadata"])?;
  let id = words.required_text("id")?;
  let record = words.timebase("record", ticks)?;
  let exact = words.required_exact("source")?;
  object.no_effects()?;
  object.no_color()?;
  let at = object.at.key("source_range");
  let ruler = Ruler::new(exact.ruler);
  let (start, length) = counts(object.required("source_range")?, &at, ruler)?;
  let walked = derive::Child::Item {
    start: written(start, ruler),
    duration: written(length, ruler),
  };
  let source = exact_range(start, length, exact, &at)?;
  // With no time-warp, a clip runs exactly as long as its source.
  let length = source
    .span()
    .checked_rescale_with(ticks, Rounding::Exact)
    .ok_or_else(|| {
      inexact(
        &at,
        "a source range that is no whole number of edit-rate ticks",
      )
    })?
    .ticks();
  let mut clip = Clip::new(
    ClipId::new(id),
    object.name()?,
    media(object, schema)?,
    source,
    // The track places the record; its timebase is the clip's own.
    TimeRange::new(0, 0, record),
  )
  .with_enabled(object.enabled()?)
  .with_gain(words.gain("gain_db")?)
  .with_metadata(words.notes()?)
  .with_readers(readers);
  *clip.markers_mut() = markers(object, Placed::Exact)?;
  Ok((clip, length, walked))
}

/// The one medium a clip reads: `Clip.1`'s `media_reference`, or the one
/// reference of a `Clip.2`, under `DEFAULT_MEDIA` and active.
fn media(object: &Object<'_>, schema: &str) -> Result<MediaRef, Unreadable> {
  if schema != "Clip.2" {
    // OpenTimelineIO upgrades a `Clip.1` to a `Clip.2`, and one whose
    // `media_reference` is absent or `null` to a `MissingReference`
    // (`typeRegistry.cpp` 99–117 in 0.17.0): a clip with no medium.
    let at = object.at.key("media_reference");
    return match object.get("media_reference") {
      None | Some(Value::Null) => Err(feature(&at, Feature::NoMedia)),
      Some(value) => reference(value, at),
    };
  }
  let at = object.at.key("media_references");
  let references = Object::of(object.required("media_references")?, at.clone())?;
  let active = object
    .string("active_media_reference_key")?
    .ok_or_else(|| {
      shape(
        &object.at.key("active_media_reference_key"),
        "a required key",
      )
    })?;
  match references.members {
    [] => Err(feature(&at, Feature::NoMedia)),
    [(key, value)] if key == DEFAULT_MEDIA && active == DEFAULT_MEDIA => {
      reference(value, at.key(DEFAULT_MEDIA))
    }
    _ => Err(feature(&at, Feature::MediaReferences)),
  }
}

/// An `ExternalReference.1`: the medium's locator, and what is known of it.
fn reference(value: &Value, at: At) -> Result<MediaRef, Unreadable> {
  let object = Object::of(value, at)?;
  object.schema(&["ExternalReference.1"])?;
  object.keys(&[
    "OTIO_SCHEMA",
    "metadata",
    "name",
    "available_range",
    "available_image_bounds",
    "target_url",
  ])?;
  let words = Words::of(&object, &["rate", "reel", "available"])?;
  object.no_name(&[])?;
  match object.get("available_image_bounds") {
    None | Some(Value::Null) => {}
    Some(_) => {
      return Err(feature(
        &object.at.key("available_image_bounds"),
        Feature::ImageBounds,
      ));
    }
  }
  let locator = object
    .string("target_url")?
    .ok_or_else(|| shape(&object.at.key("target_url"), "a required key"))?;
  let at = object.at.key("available_range");
  let available = match object.get("available_range") {
    None | Some(Value::Null) => {
      if words.get("available").is_some() {
        return Err(shape(
          &words.at.key("available"),
          "no available word, as the reference has no available range",
        ));
      }
      None
    }
    Some(range) => {
      let exact = words.required_exact("available")?;
      let (start, length) = counts(range, &at, Ruler::new(exact.ruler))?;
      Some(exact_range(start, length, exact, &at)?)
    }
  };
  Ok(
    MediaRef::new(locator)
      .with_available_range(available)
      .with_rate(words.rate("rate")?)
      .with_reel(words.text("reel")?.map(Utf8Bytes::from)),
  )
}

/// Where a marker's range is counted.
#[derive(Clone, Copy)]
enum Placed {
  /// The stack's or a track's: at the edit rate, whose exact rate and
  /// timebase are these.
  Edit(Ruler, Timebase),
  /// A clip's: in the exact ruler its words name, then in its timebase.
  Exact,
}

/// The `Marker.2`s of an item: the stack's — the timeline's own — a
/// track's, or a clip's.
fn markers(object: &Object<'_>, placed: Placed) -> Result<Vec<Marker>, Unreadable> {
  let Some(value) = object.get("markers") else {
    return Ok(Vec::new());
  };
  let at = object.at.key("markers");
  array(value, &at)?
    .iter()
    .enumerate()
    .map(|(index, entry)| {
      let marker = Object::of(entry, at.index(index))?;
      marker.schema(&["Marker.2"])?;
      marker.keys(&[
        "OTIO_SCHEMA",
        "metadata",
        "name",
        "color",
        "marked_range",
        "comment",
      ])?;
      let (words, readers) = Words::with_readers(
        &marker,
        match placed {
          Placed::Edit(..) => &["timebase"],
          Placed::Exact => &["range"],
        },
      )?;
      let color = match marker.string("color")? {
        None => MarkerColor::Green,
        Some(name) => MarkerColor::from_otio_name(name).ok_or_else(|| {
          feature(
            &marker.at.key("color"),
            Feature::MarkerColor(Utf8Bytes::from(name)),
          )
        })?,
      };
      let range_at = marker.at.key("marked_range");
      let range = marker.required("marked_range")?;
      let range = match placed {
        Placed::Edit(edit, ticks) => {
          let (start, length) = counts(range, &range_at, edit)?;
          edit_range(start, length, words.timebase("timebase", ticks)?)
        }
        Placed::Exact => {
          let exact = words.required_exact("range")?;
          let (start, length) = counts(range, &range_at, Ruler::new(exact.ruler))?;
          exact_range(start, length, exact, &range_at)?
        }
      };
      Ok(
        Marker::new(marker.name()?, range)
          .with_color(color)
          .with_comment(marker.string("comment")?.unwrap_or(""))
          .with_readers(readers),
      )
    })
    .collect()
}

/// The reader's backstop: every value the document states is one the
/// export writes back, where and as the export writes it for the timeline
/// read from it.
///
/// The export writes `timeline` again, for the target the document's clips
/// are written for — `Legacy` where they are `Clip.1`, else `V0_15Plus` —
/// and its tree is walked beside the document's, path by path, an object's
/// keys in any order. Every scalar the export writes must be the one the
/// document states at the same path, as OpenTimelineIO 0.17.0's reader holds
/// it ([`same_scalar`]). A member the export writes and the document leaves
/// out is admitted only where OpenTimelineIO reads its absence as that very
/// value ([`WRITTEN_DEFAULTS`]); a member the document states and the export
/// does not write, only where OpenTimelineIO holds it as it holds its absence
/// ([`STATED_DEFAULTS`]). The first value that differs is refused as inexact
/// there, naming both; a timeline the export cannot write, at the document's
/// root.
///
/// The checks before it refuse by name what they know. This closes the class
/// none of them names — a value read, then written back otherwise: a time in
/// another ruler, a string in another case — so the reader accepts exactly
/// the documents the export reproduces, value for value, as OpenTimelineIO
/// reads them.
pub(crate) fn reproduced(root: &Value, timeline: &Timeline) -> Result<(), Unreadable> {
  let target = if clip_schema(root) == Some("Clip.1") {
    OtioTarget::Legacy
  } else {
    OtioTarget::V0_15Plus
  };
  let written = export::timeline(timeline, &layout_valid(timeline), target)
    .map_err(|_| inexact(&At::root(), "a timeline the export cannot write back"))?;
  same(&written, root, &At::root(), Numbers::AsWritten)
}

/// How OpenTimelineIO 0.17.0's reader holds the numbers at a place.
#[derive(Clone, Copy)]
enum Numbers {
  /// A member it fetches as a `double` — a `RationalTime`'s rate and value —
  /// which takes any number, an integer cast to the nearest `double`
  /// (`deserialization.cpp` 418–454 in 0.17.0).
  Doubles,
  /// A value it keeps as it parses it — in a dictionary: an integer as an
  /// `int64_t`, a `Uint64` cut to its low 63 bits, any other number as a
  /// `double` (62–73).
  AsWritten,
}

/// The tree `written` beside `stated`, both at `at`: each object's members
/// by key, in any order; each array's items by place; each scalar held as
/// OpenTimelineIO holds it ([`same_scalar`]), its numbers as `numbers` says
/// ([`reproduced`]).
fn same(written: &Value, stated: &Value, at: &At, numbers: Numbers) -> Result<(), Unreadable> {
  match (written, stated) {
    (Value::Object(members), Value::Object(stated_members)) => {
      let schema = schema_of(written);
      let numbers = if schema == Some("RationalTime.1") {
        Numbers::Doubles
      } else {
        Numbers::AsWritten
      };
      for (key, value) in members {
        let at = at.key(key);
        match find(stated_members, key) {
          Some(stated) => same(value, stated, &at, numbers)?,
          None if admitted(&WRITTEN_DEFAULTS, schema, key, value) => {}
          None => return Err(differs(&at, Some(value), None)),
        }
      }
      for (key, value) in stated_members {
        if find(members, key).is_none() && !admitted(&STATED_DEFAULTS, schema, key, value) {
          return Err(differs(&at.key(key), None, Some(value)));
        }
      }
      Ok(())
    }
    (Value::Array(items), Value::Array(stated_items)) => {
      for (index, (item, stated)) in items.iter().zip(stated_items).enumerate() {
        same(item, stated, &at.index(index), numbers)?;
      }
      let shorter = items.len().min(stated_items.len());
      if items.len() == stated_items.len() {
        Ok(())
      } else {
        Err(differs(
          &at.index(shorter),
          items.get(shorter),
          stated_items.get(shorter),
        ))
      }
    }
    _ if same_scalar(written, stated, numbers) => Ok(()),
    _ => Err(differs(at, Some(written), Some(stated))),
  }
}

/// Whether two scalars are one value as OpenTimelineIO 0.17.0's reader holds
/// it: two strings byte for byte, two booleans or two nulls alike, and two
/// numbers as `numbers` reads them ([`held`]), two doubles bit for bit — so
/// `24` and `24.0` are one `RationalTime` rate, both read as the `double`
/// 24.0, while in a dictionary `-3` is an `int64_t` and `-3.0` a `double`,
/// and `-0.0` is not `0.0` anywhere.
fn same_scalar(written: &Value, stated: &Value, numbers: Numbers) -> bool {
  match (written, stated) {
    (Value::Null, Value::Null) => true,
    (Value::Bool(written), Value::Bool(stated)) => written == stated,
    (Value::String(written), Value::String(stated)) => written == stated,
    (Value::Number(written), Value::Number(stated)) => {
      held(written, numbers).is_some_and(|written| Some(written) == held(stated, numbers))
    }
    _ => false,
  }
}

/// The number `text` spells, as OpenTimelineIO 0.17.0's reader holds it
/// where it reads `numbers` — RapidJSON's verdict ([`json::number`]): as a
/// `double` ([`json::double`]), or as its handler keeps it, an integer as an
/// `int64_t`, a `Uint64` cut to its low 63 bits, any other number as a
/// `double` ([`json::held`]). `None` for a number RapidJSON refuses, or one
/// OpenTimelineIO's builds read as different values.
fn held(text: &str, numbers: Numbers) -> Option<Held> {
  match numbers {
    Numbers::Doubles => json::double(text).map(Held::Double),
    Numbers::AsWritten => json::held(text),
  }
}

/// A value the export writes, or the document states, at `at`, and the other
/// side's — `None` where one leaves the member out — refused as inexact,
/// naming both.
fn differs(at: &At, written: Option<&Value>, stated: Option<&Value>) -> Unreadable {
  let shown = |value: Option<&Value>| match value {
    Some(value) => {
      let mut out = Utf8BytesMut::new();
      json::write_compact(value, &mut out);
      out.freeze_shared()
    }
    None => Utf8Bytes::from_static("nothing"),
  };
  Unreadable::Inexact(Inexact::with_values(
    at.0.clone(),
    "a value the export does not write back as the document states it",
    shown(written),
    shown(stated),
  ))
}

/// A value OpenTimelineIO 0.17.0 holds a member at, which a defaults table
/// names.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Plain {
  Null,
  True,
  Text(&'static str),
  EmptyObject,
  EmptyArray,
}

impl Plain {
  fn is(self, value: &Value) -> bool {
    match (self, value) {
      (Self::Null, Value::Null) | (Self::True, Value::Bool(true)) => true,
      (Self::Text(expected), Value::String(text)) => text == expected,
      (Self::EmptyObject, Value::Object(members)) => members.is_empty(),
      (Self::EmptyArray, Value::Array(items)) => items.is_empty(),
      _ => false,
    }
  }
}

/// One row of a defaults table: the member `key`, at `value`, of an object
/// of one of `schemas`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Row {
  schemas: &'static [&'static str],
  key: &'static str,
  value: Plain,
}

/// Whether one of `rows` admits the member `key`, at `value`, of an object
/// of `schema`.
fn admitted(rows: &[Row], schema: Option<&str>, key: &str, value: &Value) -> bool {
  schema.is_some_and(|schema| {
    rows
      .iter()
      .any(|row| row.key == key && row.schemas.contains(&schema) && row.value.is(value))
  })
}

/// The schemas whose objects carry a name and metadata
/// (`SerializableObjectWithMetadata`).
const WITH_METADATA: &[&str] = &[
  "Timeline.1",
  "Stack.1",
  "Track.1",
  "Gap.1",
  "Clip.1",
  "Clip.2",
  "Transition.1",
  "ExternalReference.1",
  "Marker.2",
];

/// The schemas whose objects are items (`Item`).
const ITEMS: &[&str] = &["Stack.1", "Track.1", "Gap.1", "Clip.1", "Clip.2"];

/// **The written defaults**: the members the export writes that a document
/// may leave out, as OpenTimelineIO 0.17.0 reads each one's absence as the
/// value written — it builds every object by its default constructor and
/// reads such a member only if present. Each row cites the export's write
/// (the function of `export.rs` that writes it), the read and the default
/// (`src/opentimelineio/` at `v0.17.0`). Nothing else the export writes may
/// be missing.
pub(crate) const WRITTEN_DEFAULTS: [Row; 10] = [
  // `metadata` `{}`: the stack's and a gap's, and any object's with neither
  // words nor a reader's namespace (`timeline`, `gap`, `metadata`); read
  // only if present (`serializableObjectWithMetadata.cpp` 21) over
  // `AnyDictionary()` (`serializableObjectWithMetadata.h` 24, each schema's
  // constructor).
  Row {
    schemas: WITH_METADATA,
    key: "metadata",
    value: Plain::EmptyObject,
  },
  // `name` `""`: a gap's, a transition's, a reference's, and whatever the
  // timeline names nothing (`gap`, `transition`, `external_reference`;
  // `timeline`, `track`, `clip`, `marker`); read only if present
  // (`serializableObjectWithMetadata.cpp` 22) over `std::string()` (`.h`
  // 23).
  Row {
    schemas: WITH_METADATA,
    key: "name",
    value: Plain::Text(""),
  },
  // `source_range` `null` on the stack and a track (`timeline`, `track`):
  // read only if present (`item.cpp` 175) over `std::nullopt` (`stack.h`
  // 26, `track.h` 38).
  Row {
    schemas: &["Stack.1", "Track.1"],
    key: "source_range",
    value: Plain::Null,
  },
  // `effects` `[]` (`timeline`, `track`, `clip`, `gap`): `item.cpp` 176
  // over `item.h` 31.
  Row {
    schemas: ITEMS,
    key: "effects",
    value: Plain::EmptyArray,
  },
  // `markers` `[]` where an item has none (`timeline`, `track`, `clip`,
  // `gap`): `item.cpp` 177 over `item.h` 32.
  Row {
    schemas: ITEMS,
    key: "markers",
    value: Plain::EmptyArray,
  },
  // `enabled` `true` (`timeline`, `track`, `clip`, `gap`): `item.cpp` 178
  // over `item.h` 33.
  Row {
    schemas: ITEMS,
    key: "enabled",
    value: Plain::True,
  },
  // A reference's `available_range` `null` where it states none
  // (`external_reference`): `mediaReference.cpp` 30 over `std::nullopt`
  // (`externalReference.h` 24).
  Row {
    schemas: &["ExternalReference.1"],
    key: "available_range",
    value: Plain::Null,
  },
  // A reference's `available_image_bounds` `null`, for `V0_15Plus`
  // (`external_reference`): `mediaReference.cpp` 31–33 over `std::nullopt`
  // (`externalReference.h` 26–27).
  Row {
    schemas: &["ExternalReference.1"],
    key: "available_image_bounds",
    value: Plain::Null,
  },
  // A marker's `color` `GREEN` (`marker`): `marker.cpp` 27 over
  // `Color::green` (`marker.h` 40), which is `"GREEN"` (20).
  Row {
    schemas: &["Marker.2"],
    key: "color",
    value: Plain::Text("GREEN"),
  },
  // A marker's `comment` `""` (`marker`): `marker.cpp` 29 over
  // `std::string()` (`marker.h` 42).
  Row {
    schemas: &["Marker.2"],
    key: "comment",
    value: Plain::Text(""),
  },
];

/// **The stated defaults**: the members a document may state that the export
/// does not write, as OpenTimelineIO 0.17.0 holds each one's stated value as
/// it holds the member's absence. Nothing else a document states may go
/// unwritten: OpenTimelineIO keeps every member it reads, and one its schema
/// does not know as a dynamic field it writes back (`serializableObject.cpp`
/// 78–107 at `v0.17.0`) — a `color` on an item and an `enabled` on a
/// transition among them.
pub(crate) const STATED_DEFAULTS: [Row; 1] = [
  // A reference's `available_image_bounds` `null`, which the `Legacy` export
  // does not write (`export.rs`, `external_reference`): read into its
  // optional as `std::nullopt` (`mediaReference.cpp` 31–33), its value when
  // absent (`externalReference.h` 26–27).
  Row {
    schemas: &["ExternalReference.1"],
    key: "available_image_bounds",
    value: Plain::Null,
  },
];

/// The schema of the first clip `value` holds, depth first: what target the
/// document is written for.
fn clip_schema(value: &Value) -> Option<&str> {
  match value {
    Value::Object(members) => match schema_of(value) {
      Some(schema @ ("Clip.1" | "Clip.2")) => Some(schema),
      _ => members.iter().find_map(|(_, value)| clip_schema(value)),
    },
    Value::Array(items) => items.iter().find_map(clip_schema),
    _ => None,
  }
}

/// The schema an object names, if it is one that names one.
fn schema_of(value: &Value) -> Option<&str> {
  member(value, "OTIO_SCHEMA").and_then(Value::as_str)
}

/// The member `key` of `value`, if it is an object that has one.
fn member<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
  value.as_object().and_then(|members| find(members, key))
}

/// The member `key` among `members`.
fn find<'a>(members: &'a Members, key: &str) -> Option<&'a Value> {
  members
    .iter()
    .find(|(name, _)| name == key)
    .map(|(_, value)| value)
}
