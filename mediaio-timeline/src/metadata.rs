//! A document's own notes, and other readers' own metadata.

use std::collections::BTreeMap;

use smol_bytes::Utf8Bytes;

/// The key under which this crate's own words ride in an object's
/// OpenTimelineIO `metadata`: no reader's namespace takes it.
pub(crate) const OURS: &str = "mediaio";

/// OpenTimelineIO's schema word: its reader reads a dictionary carrying it
/// as a key as an object of the schema it names.
pub(crate) const SCHEMA_WORD: &str = "OTIO_SCHEMA";

/// An ordered map from string words to string values: notes a document
/// carries for its own readers.
///
/// The model never interprets an entry. Entries iterate in key order, so a
/// map writes the same bytes however it was built. On the wire it is a plain
/// object of strings, and an object naming one key twice is refused by name
/// rather than letting the last value win.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct Metadata(BTreeMap<Utf8Bytes, Utf8Bytes>);

impl Metadata {
  /// An empty map.
  pub const fn new() -> Self {
    Self(BTreeMap::new())
  }

  /// The value under `key`, if any.
  pub fn get(&self, key: &str) -> Option<&str> {
    self.0.get(key).map(Utf8Bytes::as_str)
  }

  /// Sets `key` to `value`, returning the value it replaced.
  pub fn insert(
    &mut self,
    key: impl Into<Utf8Bytes>,
    value: impl Into<Utf8Bytes>,
  ) -> Option<Utf8Bytes> {
    self.0.insert(key.into(), value.into())
  }

  /// Sets `key` to `value` (consuming builder).
  #[must_use]
  pub fn with(mut self, key: impl Into<Utf8Bytes>, value: impl Into<Utf8Bytes>) -> Self {
    self.insert(key, value);
    self
  }

  /// Removes `key`, returning its value.
  pub fn remove(&mut self, key: &str) -> Option<Utf8Bytes> {
    self.0.remove(key)
  }

  /// The entries, in key order.
  pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
    self.0.iter().map(|(k, v)| (k.as_str(), v.as_str()))
  }

  /// How many entries the map holds.
  pub fn len(&self) -> usize {
    self.0.len()
  }

  /// Whether the map holds no entry.
  pub fn is_empty(&self) -> bool {
    self.0.is_empty()
  }
}

impl<K: Into<Utf8Bytes>, V: Into<Utf8Bytes>> FromIterator<(K, V)> for Metadata {
  /// Collects the pairs; a later pair under a key already seen replaces it.
  fn from_iter<I: IntoIterator<Item = (K, V)>>(iter: I) -> Self {
    Self(
      iter
        .into_iter()
        .map(|(k, v)| (k.into(), v.into()))
        .collect(),
    )
  }
}

/// Other readers' own metadata on an object: under each reader's own key —
/// its namespace — a [`Metadata`] of that reader's words, which the
/// OpenTimelineIO export writes in the object's `metadata`, beside this
/// crate's own words (`mediaio`), and [`from_otio`](crate::otio::from_otio)
/// reads back. A reader reads its namespace there: DaVinci Resolve 21.1, for
/// one, reads a marker's note from `Resolve_OTIO`'s `Note` alone and not
/// from OpenTimelineIO's `comment` (findit-studio/application#265).
///
/// A namespace holds strings alone, keys and values: on the wire it is an
/// object of strings, and `from_otio` refuses by name any other value inside
/// one, so no number crosses into it. Namespaces iterate in key order, and
/// their keys in theirs, so a map writes the same bytes however it was
/// built. The model never interprets an entry.
/// [`validate`](fn@crate::validate) refuses a namespace named `mediaio`,
/// the crate's own, or `OTIO_SCHEMA`, and a key `OTIO_SCHEMA` inside one:
/// OpenTimelineIO reads a dictionary carrying `OTIO_SCHEMA` as an object of
/// the schema it names (`deserialization.cpp` 561–573 in 0.17.0), which
/// would turn the object's metadata, or the namespace, into one.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct ReaderMetadata(BTreeMap<Utf8Bytes, Metadata>);

impl ReaderMetadata {
  /// No namespace.
  pub const fn new() -> Self {
    Self(BTreeMap::new())
  }

  /// The words under `namespace`, if it is there.
  pub fn get(&self, namespace: &str) -> Option<&Metadata> {
    self.0.get(namespace)
  }

  /// The words under `namespace`, for editing in place, if it is there.
  pub fn get_mut(&mut self, namespace: &str) -> Option<&mut Metadata> {
    self.0.get_mut(namespace)
  }

  /// Sets `namespace` to `words`, returning the words it replaced.
  pub fn insert(&mut self, namespace: impl Into<Utf8Bytes>, words: Metadata) -> Option<Metadata> {
    self.0.insert(namespace.into(), words)
  }

  /// Sets `namespace` to `words` (consuming builder).
  #[must_use]
  pub fn with(mut self, namespace: impl Into<Utf8Bytes>, words: Metadata) -> Self {
    self.insert(namespace, words);
    self
  }

  /// Removes `namespace`, returning its words.
  pub fn remove(&mut self, namespace: &str) -> Option<Metadata> {
    self.0.remove(namespace)
  }

  /// The namespaces and their words, in key order.
  pub fn iter(&self) -> impl Iterator<Item = (&str, &Metadata)> {
    self.0.iter().map(|(k, v)| (k.as_str(), v))
  }

  /// How many namespaces the map holds.
  pub fn len(&self) -> usize {
    self.0.len()
  }

  /// Whether the map holds no namespace.
  pub fn is_empty(&self) -> bool {
    self.0.is_empty()
  }
}

impl<K: Into<Utf8Bytes>> FromIterator<(K, Metadata)> for ReaderMetadata {
  /// Collects the namespaces; a later one under a key already seen replaces
  /// it.
  fn from_iter<I: IntoIterator<Item = (K, Metadata)>>(iter: I) -> Self {
    Self(iter.into_iter().map(|(k, v)| (k.into(), v)).collect())
  }
}

#[cfg(feature = "serde")]
const _: () = {
  use core::fmt;
  use std::collections::btree_map::Entry;

  use serde::{
    Deserialize, Deserializer, Serialize, Serializer,
    de::{self, MapAccess, Visitor},
  };

  impl Serialize for Metadata {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
      self.0.serialize(serializer)
    }
  }

  impl Serialize for ReaderMetadata {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
      self.0.serialize(serializer)
    }
  }

  /// A plain object of namespaces, each an object of string values; one
  /// naming a namespace twice is refused by name rather than letting the
  /// last win.
  impl<'de> Deserialize<'de> for ReaderMetadata {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
      deserializer.deserialize_map(ReaderMetadataVisitor)
    }
  }

  struct ReaderMetadataVisitor;

  impl<'de> Visitor<'de> for ReaderMetadataVisitor {
    type Value = ReaderMetadata;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
      f.write_str("an object of namespaces, each an object of string values")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<ReaderMetadata, A::Error> {
      let mut namespaces = BTreeMap::new();
      while let Some((namespace, words)) = map.next_entry::<Utf8Bytes, Metadata>()? {
        match namespaces.entry(namespace) {
          Entry::Vacant(slot) => {
            slot.insert(words);
          }
          Entry::Occupied(slot) => {
            return Err(de::Error::custom(format_args!(
              "duplicate reader namespace `{}`",
              slot.key()
            )));
          }
        }
      }
      Ok(ReaderMetadata(namespaces))
    }
  }

  impl<'de> Deserialize<'de> for Metadata {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
      deserializer.deserialize_map(MetadataVisitor)
    }
  }

  struct MetadataVisitor;

  impl<'de> Visitor<'de> for MetadataVisitor {
    type Value = Metadata;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
      f.write_str("an object of string values")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Metadata, A::Error> {
      let mut entries = BTreeMap::new();
      while let Some((key, value)) = map.next_entry::<Utf8Bytes, Utf8Bytes>()? {
        match entries.entry(key) {
          Entry::Vacant(slot) => {
            slot.insert(value);
          }
          Entry::Occupied(slot) => {
            return Err(de::Error::custom(format_args!(
              "duplicate metadata key `{}`",
              slot.key()
            )));
          }
        }
      }
      Ok(Metadata(entries))
    }
  }
};
