//! A small JSON value: written pretty for the export, and read as
//! OpenTimelineIO 0.17.0's parser reads it — its numbers by RapidJSON's
//! verdict ([`number`]).

use core::{cmp::Ordering, fmt::Write as _};
use smol_bytes::{Utf8Bytes, Utf8BytesMut};
use std::{collections::BTreeSet, vec::Vec};

use super::Syntax;

mod number;

pub(crate) use number::{Held, Number, number, readings};

/// A JSON value. A number is kept as the text that spells it, so the writer
/// puts down exactly the digits it was handed and the reader keeps exactly
/// the digits it read.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Value {
  Null,
  Bool(bool),
  Number(Utf8Bytes),
  String(Utf8Bytes),
  Array(Vec<Value>),
  Object(Vec<(Utf8Bytes, Value)>),
}

impl Value {
  pub(crate) fn as_object(&self) -> Option<&[(Utf8Bytes, Value)]> {
    match self {
      Self::Object(members) => Some(members),
      _ => None,
    }
  }

  pub(crate) fn as_array(&self) -> Option<&[Value]> {
    match self {
      Self::Array(items) => Some(items),
      _ => None,
    }
  }

  pub(crate) fn as_str(&self) -> Option<&str> {
    match self {
      Self::String(text) => Some(text.as_str()),
      _ => None,
    }
  }

  /// The number's value as a member read as a `double` takes it
  /// ([`double`]).
  pub(crate) fn as_f64(&self) -> Option<f64> {
    match self {
      Self::Number(text) => double(text),
      _ => None,
    }
  }
}

/// Writes `value` with four spaces of indent per level and `": "` after a
/// key — the layout OpenTimelineIO's own writer uses — without a final
/// newline.
pub(crate) fn write_pretty(value: &Value, out: &mut Utf8BytesMut) {
  write_value(value, 0, out);
}

/// Writes `value` on one line, `", "` between members and items and `": "`
/// after a key: a value named in a refusal.
pub(crate) fn write_compact(value: &Value, out: &mut Utf8BytesMut) {
  match value {
    Value::Array(items) => {
      out.push('[');
      for (index, item) in items.iter().enumerate() {
        if index > 0 {
          out.push_str(", ");
        }
        write_compact(item, out);
      }
      out.push(']');
    }
    Value::Object(members) => {
      out.push('{');
      for (index, (key, item)) in members.iter().enumerate() {
        if index > 0 {
          out.push_str(", ");
        }
        write_string(key, out);
        out.push_str(": ");
        write_compact(item, out);
      }
      out.push('}');
    }
    scalar => write_value(scalar, 0, out),
  }
}

fn write_value(value: &Value, depth: usize, out: &mut Utf8BytesMut) {
  match value {
    Value::Null => out.push_str("null"),
    Value::Bool(true) => out.push_str("true"),
    Value::Bool(false) => out.push_str("false"),
    Value::Number(text) => out.push_str(text),
    Value::String(text) => write_string(text, out),
    Value::Array(items) if items.is_empty() => out.push_str("[]"),
    Value::Array(items) => {
      out.push('[');
      for (index, item) in items.iter().enumerate() {
        if index > 0 {
          out.push(',');
        }
        newline(depth + 1, out);
        write_value(item, depth + 1, out);
      }
      newline(depth, out);
      out.push(']');
    }
    Value::Object(members) if members.is_empty() => out.push_str("{}"),
    Value::Object(members) => {
      out.push('{');
      for (index, (key, item)) in members.iter().enumerate() {
        if index > 0 {
          out.push(',');
        }
        newline(depth + 1, out);
        write_string(key, out);
        out.push_str(": ");
        write_value(item, depth + 1, out);
      }
      newline(depth, out);
      out.push('}');
    }
  }
}

fn newline(depth: usize, out: &mut Utf8BytesMut) {
  out.push('\n');
  for _ in 0..depth {
    out.push_str("    ");
  }
}

/// Writes `text` as a JSON string: the quote, the backslash and the control
/// characters escaped, everything else as it is.
fn write_string(text: &str, out: &mut Utf8BytesMut) {
  out.push('"');
  for c in text.chars() {
    match c {
      '"' => out.push_str("\\\""),
      '\\' => out.push_str("\\\\"),
      '\n' => out.push_str("\\n"),
      '\r' => out.push_str("\\r"),
      '\t' => out.push_str("\\t"),
      '\u{8}' => out.push_str("\\b"),
      '\u{c}' => out.push_str("\\f"),
      c if u32::from(c) < 0x20 => {
        let _ = write!(out, "\\u{:04x}", u32::from(c));
      }
      c => out.push(c),
    }
  }
  out.push('"');
}

/// The `double` OpenTimelineIO 0.17.0 reads `text` as where it fetches a
/// member as one — a `RationalTime`'s rate and value among them: the
/// number RapidJSON reads ([`number`]), an integer cast to the nearest
/// double ([`Number::double`]) — or `None` where it reads no number, or its
/// builds read different ones.
pub(crate) fn double(text: &str) -> Option<f64> {
  number(text).ok().map(Number::double)
}

/// The number `text` spells as OpenTimelineIO 0.17.0's handler keeps it,
/// in a dictionary: an `int64_t` or a `double` ([`Number::held`]) — or
/// `None` where RapidJSON reads no number, or its builds read different
/// ones.
pub(crate) fn held(text: &str) -> Option<Held> {
  number(text).ok().map(Number::held)
}

/// The whole number OpenTimelineIO 0.17.0 keeps for `text` in a dictionary
/// ([`held`]): its integer, or its double where that is a whole number
/// within ±2^127 — `None` for a fraction, NaN or an infinity, and for a
/// number RapidJSON refuses.
pub(crate) fn whole(text: &str) -> Option<i128> {
  match held(text)? {
    Held::Integer(value) => Some(i128::from(value)),
    Held::Double(value) => whole_double(value),
  }
}

/// `value` as a whole number, where it is one within ±2^127: `-0.0` as
/// zero, a fraction, NaN and an infinity `None`.
pub(crate) fn whole_double(value: f64) -> Option<i128> {
  /// 2^127.
  const LIMIT: f64 = 170_141_183_460_469_231_731_687_303_715_884_105_728.0;
  // NaN is within neither bound.
  if !(value > -LIMIT && value < LIMIT) {
    return None;
  }
  let whole = value as i128;
  // Exact both ways for a whole number; a fraction truncated is another.
  (whole as f64 == value).then_some(whole)
}

/// The `f32` the export writes back as `text` reads: the `f32` nearest the
/// double OpenTimelineIO 0.17.0 reads `text` as ([`double`]), where it is
/// finite and its shortest spelling — `{:?}`, as the export writes every
/// `f32` — reads as that very double; else `None`. So `-3.0` and `-3e0`
/// read as -3.0, `16777217`, which no `f32` holds, is `None`, and so is
/// `0.10000000149011612`, the `f32` 0.1's own value, whose spelling `0.1`
/// reads as another double.
pub(crate) fn float(text: &str) -> Option<f32> {
  let read = double(text)?;
  let nearest = read as f32;
  if !nearest.is_finite() {
    return None;
  }
  let mut written = Spelled::default();
  core::fmt::write(&mut written, format_args!("{nearest:?}")).ok()?;
  double(written.as_str())
    .filter(|back| back.to_bits() == read.to_bits())
    .map(|_| nearest)
}

/// A float's shortest spelling, written on the stack: an `f32`'s is at
/// most a minus, nine significant digits, a point and an exponent such as
/// `e-38`, 15 bytes. Longer text is refused as an error, which no float
/// writes.
#[derive(Default)]
struct Spelled {
  bytes: [u8; 32],
  len: usize,
}

impl Spelled {
  fn as_str(&self) -> &str {
    // Only whole `&str`s are copied in.
    core::str::from_utf8(&self.bytes[..self.len]).unwrap_or("")
  }
}

impl core::fmt::Write for Spelled {
  fn write_str(&mut self, text: &str) -> core::fmt::Result {
    let end = self.len + text.len();
    let slot = self.bytes.get_mut(self.len..end).ok_or(core::fmt::Error)?;
    slot.copy_from_slice(text.as_bytes());
    self.len = end;
    Ok(())
  }
}

/// How deep arrays and objects may nest before the reader refuses: deep
/// enough for any timeline this crate writes, shallow enough that a hostile
/// document cannot exhaust the stack.
const MAX_DEPTH: usize = 128;

/// An object's key as the duplicate check orders it: by its text — and,
/// in the laws with `std`, counted each time two are compared.
#[derive(PartialEq, Eq)]
struct Key(Utf8Bytes);

impl PartialOrd for Key {
  fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
    Some(self.cmp(other))
  }
}

impl Ord for Key {
  fn cmp(&self, other: &Self) -> Ordering {
    #[cfg(all(test, feature = "std"))]
    tests::compared();
    self.0.as_str().cmp(other.0.as_str())
  }
}

/// Reads `text` as one JSON value (RFC 8259) — and, as OpenTimelineIO
/// 0.17.0's parser does under `kParseNanAndInfFlag`, `NaN`, `Inf` and
/// `Infinity` as numbers — refusing what the grammar does not admit, a
/// number RapidJSON refuses or OpenTimelineIO's builds read as different
/// values ([`number::scan`]), an object naming a key twice, and nesting past
/// [`MAX_DEPTH`].
pub(crate) fn parse(text: &str) -> Result<Value, Syntax> {
  let mut reader = Reader {
    text,
    bytes: text.as_bytes(),
    at: 0,
    depth: 0,
  };
  reader.skip_space();
  let value = reader.value()?;
  reader.skip_space();
  if reader.at != reader.bytes.len() {
    return Err(reader.refuse("text after the document"));
  }
  Ok(value)
}

struct Reader<'a> {
  text: &'a str,
  bytes: &'a [u8],
  at: usize,
  depth: usize,
}

impl Reader<'_> {
  fn refuse(&self, reason: &'static str) -> Syntax {
    Syntax {
      offset: self.at,
      reason,
    }
  }

  fn peek(&self) -> Option<u8> {
    self.bytes.get(self.at).copied()
  }

  fn skip_space(&mut self) {
    while let Some(b' ' | b'\t' | b'\n' | b'\r') = self.peek() {
      self.at += 1;
    }
  }

  fn expect(&mut self, byte: u8, reason: &'static str) -> Result<(), Syntax> {
    if self.peek() == Some(byte) {
      self.at += 1;
      Ok(())
    } else {
      Err(self.refuse(reason))
    }
  }

  fn value(&mut self) -> Result<Value, Syntax> {
    match self.peek() {
      Some(b'{') => self.nested(Self::object),
      Some(b'[') => self.nested(Self::array),
      Some(b'"') => self.string().map(Value::String),
      Some(b't') => self.literal("true", Value::Bool(true)),
      Some(b'f') => self.literal("false", Value::Bool(false)),
      Some(b'n') => self.literal("null", Value::Null),
      Some(b'-' | b'0'..=b'9' | b'N' | b'I') => self.number(),
      _ => Err(self.refuse("expected a value")),
    }
  }

  fn nested(&mut self, read: fn(&mut Self) -> Result<Value, Syntax>) -> Result<Value, Syntax> {
    if self.depth == MAX_DEPTH {
      return Err(self.refuse("nested too deeply"));
    }
    self.depth += 1;
    let value = read(self);
    self.depth -= 1;
    value
  }

  fn object(&mut self) -> Result<Value, Syntax> {
    self.at += 1;
    let mut members: Vec<(Utf8Bytes, Value)> = Vec::new();
    // The keys read so far, for the check that none is named twice: in a
    // set, so an object of n keys costs O(n log n) comparisons — a scan of
    // the members before each key would cost n²/2.
    let mut keys: BTreeSet<Key> = BTreeSet::new();
    self.skip_space();
    if self.peek() == Some(b'}') {
      self.at += 1;
      return Ok(Value::Object(members));
    }
    loop {
      self.skip_space();
      if self.peek() != Some(b'"') {
        return Err(self.refuse("expected a key"));
      }
      let key_at = self.at;
      let key = self.string()?;
      if !keys.insert(Key(key.clone())) {
        return Err(Syntax {
          offset: key_at,
          reason: "a key named twice in one object",
        });
      }
      self.skip_space();
      self.expect(b':', "expected `:` after a key")?;
      self.skip_space();
      let value = self.value()?;
      members.push((key, value));
      self.skip_space();
      match self.peek() {
        Some(b',') => self.at += 1,
        Some(b'}') => {
          self.at += 1;
          return Ok(Value::Object(members));
        }
        _ => return Err(self.refuse("expected `,` or `}`")),
      }
    }
  }

  fn array(&mut self) -> Result<Value, Syntax> {
    self.at += 1;
    let mut items = Vec::new();
    self.skip_space();
    if self.peek() == Some(b']') {
      self.at += 1;
      return Ok(Value::Array(items));
    }
    loop {
      self.skip_space();
      items.push(self.value()?);
      self.skip_space();
      match self.peek() {
        Some(b',') => self.at += 1,
        Some(b']') => {
          self.at += 1;
          return Ok(Value::Array(items));
        }
        _ => return Err(self.refuse("expected `,` or `]`")),
      }
    }
  }

  fn literal(&mut self, word: &'static str, value: Value) -> Result<Value, Syntax> {
    if self.bytes[self.at..].starts_with(word.as_bytes()) {
      self.at += word.len();
      Ok(value)
    } else {
      Err(self.refuse("expected `true`, `false` or `null`"))
    }
  }

  /// A number, as far as RapidJSON reads one, kept as its text — or
  /// RapidJSON's refusal where it stopped, at the number's first byte for
  /// one too big or one the builds read differently ([`number::scan`]).
  fn number(&mut self) -> Result<Value, Syntax> {
    let start = self.at;
    let scanned = number::scan(&self.bytes[start..]);
    match scanned.settled() {
      Ok(_) => {
        self.at = start + scanned.end;
        // A number's bytes are ASCII, so its ends are a character's.
        Ok(Value::Number(Utf8Bytes::from(&self.text[start..self.at])))
      }
      Err(error) => Err(Syntax {
        offset: if error.at_start() {
          start
        } else {
          start + scanned.end
        },
        reason: error.reason(),
      }),
    }
  }

  fn string(&mut self) -> Result<Utf8Bytes, Syntax> {
    self.at += 1;
    let mut out = Utf8BytesMut::new();
    loop {
      let run = self.at;
      while let Some(b) = self.peek()
        && b != b'"'
        && b != b'\\'
        && b >= 0x20
      {
        self.at += 1;
      }
      // The run ends on an ASCII byte or at the end of the text, so it is a
      // whole number of UTF-8 sequences.
      out.push_str(&self.text[run..self.at]);
      match self.peek() {
        Some(b'"') => {
          self.at += 1;
          return Ok(out.freeze_shared());
        }
        Some(b'\\') => {
          self.at += 1;
          self.escape(&mut out)?;
        }
        Some(_) => return Err(self.refuse("a control character inside a string")),
        None => return Err(self.refuse("a string left open")),
      }
    }
  }

  fn escape(&mut self, out: &mut Utf8BytesMut) -> Result<(), Syntax> {
    let c = match self.peek() {
      Some(b'"') => '"',
      Some(b'\\') => '\\',
      Some(b'/') => '/',
      Some(b'b') => '\u{8}',
      Some(b'f') => '\u{c}',
      Some(b'n') => '\n',
      Some(b'r') => '\r',
      Some(b't') => '\t',
      Some(b'u') => {
        self.at += 1;
        let unit = self.hex4()?;
        let code = match unit {
          0xD800..=0xDBFF => {
            if !self.bytes[self.at..].starts_with(b"\\u") {
              return Err(self.refuse("a lone surrogate"));
            }
            self.at += 2;
            let low = self.hex4()?;
            if !(0xDC00..=0xDFFF).contains(&low) {
              return Err(self.refuse("a lone surrogate"));
            }
            0x10000 + ((unit - 0xD800) << 10) + (low - 0xDC00)
          }
          0xDC00..=0xDFFF => return Err(self.refuse("a lone surrogate")),
          unit => unit,
        };
        let c = char::from_u32(code).ok_or_else(|| self.refuse("not a character"))?;
        out.push(c);
        return Ok(());
      }
      _ => return Err(self.refuse("an unknown escape")),
    };
    self.at += 1;
    out.push(c);
    Ok(())
  }

  fn hex4(&mut self) -> Result<u32, Syntax> {
    let mut unit = 0;
    for _ in 0..4 {
      let digit = match self.peek() {
        Some(b @ b'0'..=b'9') => b - b'0',
        Some(b @ b'a'..=b'f') => b - b'a' + 10,
        Some(b @ b'A'..=b'F') => b - b'A' + 10,
        _ => return Err(self.refuse("expected four hex digits")),
      };
      unit = unit * 16 + u32::from(digit);
      self.at += 1;
    }
    Ok(unit)
  }
}

#[cfg(test)]
mod tests;
