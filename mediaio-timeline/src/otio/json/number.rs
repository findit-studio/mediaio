//! A number's text as OpenTimelineIO 0.17.0 reads it: its JSON parser's
//! verdict.
//!
//! OpenTimelineIO 0.17.0 parses a document with RapidJSON's `Reader` at the
//! commit its `src/deps/rapidjson` submodule pins, `06d58b9e`, under one
//! flag, `kParseNanAndInfFlag` (`deserialization.cpp:864` and `:931` at
//! `v0.17.0`). Without `kParseFullPrecisionFlag`, a number with a fraction
//! or an exponent is read in RapidJSON's normal precision, which does not
//! always round to the nearest double. [`scan`] is that reader's
//! `ParseNumber` (`include/rapidjson/reader.h:1467–1748` at `06d58b9e`) step
//! for step, as a 64-bit build runs it — `RAPIDJSON_64BIT`, which every LP64
//! and every 64-bit Windows build defines (`rapidjson.h:288–294`): the
//! callback it makes, with the value it carries, or the parse error it
//! raises. Every number the crate reads goes through it — the import's, the
//! self-check's, the backstop's — and the export holds every number it
//! writes to it.
//!
//! The reader takes a minus, then `0` or a run of digits: into an
//! `unsigned` while it stays within 2^31 with a minus or 2^32 − 1 without
//! (`reader.h:1483–1522`), then into a `uint64_t` while it stays within
//! 2^63 or 2^64 − 1 (1550–1575), then into a `double`, a digit at a time,
//! `d * 10 + digit` (1577–1582). With `kParseNanAndInfFlag` it takes `NaN`,
//! `Inf` or `Infinity` in place of the digits (1523–1548). A fraction's
//! digits join the 64-bit significand while it is at most 2^53 − 1, one
//! place of `expFrac` each (1593–1608); the significand is converted to a
//! `double` (1610), and the next digits are taken `d * 10 + digit` while
//! fewer than 17 significant digits are counted, the rest dropped
//! (1618–1627). A negative exponent of any length is read, the digits past
//! what an `int` can add consumed (1648–1665); a positive one past
//! `308 − expFrac` is refused, whatever the digits before it (1666–1673).
//! The significand is then scaled by `10^(exp + expFrac)` from a table of
//! the doubles nearest the powers of ten — one multiplication or division,
//! two below 10^−308 (`StrtodNormalPrecision`, `strtod.h:28–46`;
//! `pow10.h:28–50`) — and a result past the largest double is refused
//! (`reader.h:1712–1724`). A fraction, an exponent, a number past the
//! integers, `NaN` and an infinity call `Double`; a 64-bit integer
//! `Int64` or `Uint64`, and one within 32 bits `Int` or `Uint`
//! (1726–1742).
//!
//! OpenTimelineIO's handler keeps every integer as an `int64_t` — a
//! `Uint64` cut to its low 63 bits — and every double as a `double`
//! (`deserialization.cpp:62–73`): so a dictionary holds a number
//! ([`Number::held`]). A member read as a `double` takes either, the
//! integer cast to the nearest double (`deserialization.cpp:418–454`;
//! [`Number::double`]), and one read as an `int64_t` the integer alone
//! (`:457–487`).
//!
//! # Two builds
//!
//! C++ lets a compiler fuse `d * 10 + digit` into one rounding — clang's
//! default `-ffp-contract=on` does, and GCC's `fast`, wherever the processor
//! has a fused multiply-add, as every 64-bit ARM has — and the reader's two
//! such steps (`reader.h:1580`, `:1620`) then read some long numbers
//! otherwise than a build that rounds twice. [`scan`] reads every number
//! both ways, and one the two read differently is
//! [`NumberError::Unsettled`]: refused, never claimed as either. Only a
//! significand of 17 digits or more whose first 16 reach 2^53, or an
//! integer past 64 bits, can differ.
//!
//! # The reader's integer arithmetic
//!
//! `ParseNumber` counts in C++ `unsigned`, `uint64_t` and `int`. The
//! unsigned types wrap, which is defined, and RapidJSON's own cuts keep
//! every one of their sums from wrapping. An `int` that overflows is
//! undefined behaviour, after which no build's reading can be claimed. So
//! [`scan`] makes every `int` operation as a checked 32-bit one, at the
//! place RapidJSON makes it, and an overflow is [`NumberError::Unsettled`],
//! never a reading. The operations, at `06d58b9e`:
//!
//! | `reader.h` | operation | C++ type | overflows? | [`scan`] |
//! |---|---|---|---|---|
//! | 1496, 1507, 1519 | `i = i * 10 + digit` | `unsigned` | no: cut at 214748364 with a minus, 429496729 without, first (1500, 1512) | `u32`, under the same cuts |
//! | 1508, 1520, 1562, 1573, 1606, 1623 | `significandDigit++` | `int` | no: at most 20 digits of an integer, and 17 counted in a fraction | checked |
//! | 1561, 1572, 1603 | `i64 = i64 * 10 + digit` | `uint64_t` | no: cut at 2^63 with a minus, 2^64 − 1 without (1555, 1566), and at 2^53 − 1 in a fraction (1600), first | `u64`, under the same cuts |
//! | 1604, 1621 | `--expFrac` | `int` | yes: a fraction's leading zeros keep the significand at 0 (1600), so its 2^31 + 1st place passes `INT_MIN` | checked: `Unsettled` |
//! | 1647 | `exp = s.Take() - '0'` | `int` | no: one digit, before either loop's guard | — |
//! | 1656 | `(expFrac + 2147483639) / 10` | `int` | no: `expFrac` is at most 0 | checked |
//! | 1659 | `exp = exp * 10 + digit`, a negative exponent | `int` | no: past `maxExp`, the digits are consumed, not added (1660–1663) | checked |
//! | 1667 | `308 - expFrac` | `int` | yes: for `expFrac` below −2147483339 | checked: `Unsettled` |
//! | 1669 | `exp = exp * 10 + digit`, a positive exponent | `int` | yes: where `308 − expFrac` reaches 214748364, the exponent's digits can pass `INT_MAX` before they pass `maxExp` (1670) | checked: `Unsettled` |
//! | 1679 | `exp = -exp` | `int` | no: `exp` is at most 2147483639 | checked |
//! | 1713 | `int p = exp + expFrac` | `int` | yes: below `INT_MIN`, a negative exponent beside a fraction's long run of zeros | checked: `Unsettled` |
//! | `strtod.h` 34, 41 | `-exp`, `p + 308` | `int` | no: `exp` is within −308..−1 there, and `p` below −308 | — |
//! | 1734, 1740 | `~i64 + 1`, `~i + 1` | `uint64_t`, `unsigned` | no: wraps, which is defined, before the conversion | `wrapping_neg` |
//!
//! # The table
//!
//! | text | RapidJSON at `06d58b9e` (`reader.h`) | callback | in a dictionary | read as a `double` |
//! |---|---|---|---|---|
//! | `0e309` | a positive exponent past `308 − expFrac` is refused whatever the digits before it (1666–1673) | `kParseErrorNumberTooBig` | refused | refused |
//! | `0e-400` | a negative exponent of any length is read (1648–1665): 0 × 10^−400 | `Double(0.0)` | `0.0` | `0.0` |
//! | `0.1e309` | the fraction's one digit makes `expFrac` −1, so the bound is 309 and the exponent within it (1667): 1 × 10^(309 − 1) | `Double(1e308)` | `1e308` | `1e308` |
//! | `1e308` | 1 × 10^308 (1717) | `Double(1e308)` | `1e308` | `1e308` |
//! | `2e308` | 2 × 10^308 overflows past the largest double (1720–1723) | `kParseErrorNumberTooBig` | refused | refused |
//! | `-0` | an integer, its minus negating zero (1740) | `Int(0)` | `0` | `0.0` |
//! | `-0.0` | a fraction, the double negated (1726) | `Double(-0.0)` | `-0.0` | `-0.0` |
//! | `9223372036854775807` (`i64::MAX`) | past 2^32 − 1, without a minus (1736) | `Uint64(2^63 − 1)` | `2^63 − 1` | `2^63`, the cast rounding |
//! | `9223372036854775808` (`i64::MAX` + 1) | the same (1736) | `Uint64(2^63)` | `0`, its low 63 bits | `0.0` |
//! | `18446744073709551615` (`u64::MAX`) | the same (1736) | `Uint64(2^64 − 1)` | `2^63 − 1` | `2^63` |
//! | `18446744073709551616` (`u64::MAX` + 1) | past `uint64_t`, a double digit by digit (1565–1582) | `Double(2^64)` | `2^64` | `2^64` |
//! | `9007199254740992` (2^53) | (1736) | `Uint64(2^53)` | `2^53` | `2^53` |
//! | `9007199254740993` (2^53 + 1) | (1736) | `Uint64(2^53 + 1)` | `2^53 + 1` | `2^53`, the cast rounding to even |
//! | a 400-digit integer | past `uint64_t` the double overflows to an infinity (1577–1582, 1720–1723) | `kParseErrorNumberTooBig` | refused | refused |
//! | `9007199254740991.0` | the fraction's `0` joins the significand: 90071992547409910, converted to 90071992547409904 (1599–1610), then divided by ten | `Double(9007199254740990.0)` | `9007199254740990.0` | `9007199254740990.0` |
//! | `24.0000000000000001` | 17 significant digits kept, the rest dropped (1618–1627) | `Double(24.0)` | `24.0` | `24.0` |
//! | `0.90071992547409945` | its 17th digit taken `d * 10 + digit` (1620): `0.9007199254740994` rounded twice, `0.9007199254740995` fused | [`NumberError::Unsettled`] | refused | refused |
//! | `NaN`, `-Infinity` | `kParseNanAndInfFlag` (1523–1548) | `Double(NaN)`, `Double(−∞)` | NaN, −∞ | NaN, −∞ |

/// The callback RapidJSON makes for a number, and the value it carries
/// (`reader.h:1726–1742` at `06d58b9e`).
#[derive(Debug, Clone, Copy)]
pub(crate) enum Number {
  /// An integer with a minus, down to −2^31 — zero among them, `-0`.
  Int(i32),
  /// An integer without a minus, up to 2^32 − 1.
  Uint(u32),
  /// An integer with a minus, below −2^31 and down to −2^63.
  Int64(i64),
  /// An integer without a minus, from 2^32 up to 2^64 − 1.
  Uint64(u64),
  /// Any other number: a fraction, an exponent, an integer past those,
  /// `NaN` or an infinity.
  Double(f64),
}

impl Number {
  /// The number as OpenTimelineIO's handler keeps it
  /// (`deserialization.cpp:62–73` at `v0.17.0`): every integer as an
  /// `int64_t`, a `Uint64` cut to its low 63 bits, and a double as a
  /// `double` — as a dictionary holds it.
  pub(crate) const fn held(self) -> Held {
    match self {
      Self::Int(value) => Held::Integer(value as i64),
      Self::Uint(value) => Held::Integer(value as i64),
      Self::Int64(value) => Held::Integer(value),
      // Within 63 bits, so within `i64`.
      Self::Uint64(value) => Held::Integer((value & 0x7FFF_FFFF_FFFF_FFFF) as i64),
      Self::Double(value) => Held::Double(value),
    }
  }

  /// The number as a member read as a `double` takes it
  /// (`deserialization.cpp:418–454`): the integer the handler kept, cast to
  /// the nearest double, ties to even; a double as it is.
  pub(crate) const fn double(self) -> f64 {
    match self.held() {
      Held::Integer(value) => value as f64,
      Held::Double(value) => value,
    }
  }

  /// Whether the two are one callback with one value, a double's bit for
  /// bit.
  const fn same(self, other: Self) -> bool {
    match (self, other) {
      (Self::Int(a), Self::Int(b)) => a == b,
      (Self::Uint(a), Self::Uint(b)) => a == b,
      (Self::Int64(a), Self::Int64(b)) => a == b,
      (Self::Uint64(a), Self::Uint64(b)) => a == b,
      (Self::Double(a), Self::Double(b)) => a.to_bits() == b.to_bits(),
      _ => false,
    }
  }
}

/// A number as OpenTimelineIO 0.17.0's handler keeps it: an `int64_t` or a
/// `double` ([`Number::held`]). Two are one value where they are of one kind
/// and the same — two doubles bit for bit, so `-0.0` is not `0.0`, and a
/// NaN is the NaN of its bits.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Held {
  Integer(i64),
  Double(f64),
}

impl PartialEq for Held {
  fn eq(&self, other: &Self) -> bool {
    match (self, other) {
      (Self::Integer(a), Self::Integer(b)) => a == b,
      (Self::Double(a), Self::Double(b)) => a.to_bits() == b.to_bits(),
      _ => false,
    }
  }
}

impl Eq for Held {}

/// Why RapidJSON reads no number, or why none can be claimed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NumberError {
  /// `kParseErrorValueInvalid`: no digit where one must be, or a word that
  /// is not `NaN`, `Inf` or `Infinity`.
  Invalid,
  /// `kParseErrorNumberMissFraction`: a `.` with no digit after it.
  MissFraction,
  /// `kParseErrorNumberMissExponent`: an exponent with no digit.
  MissExponent,
  /// `kParseErrorNumberTooBig`: a positive exponent past `308 − expFrac`,
  /// or a value past the largest double.
  TooBig,
  /// Not RapidJSON's: a number a build that fuses its multiply-add reads
  /// otherwise than one that does not, or one on which an `int` operation
  /// of RapidJSON's overflows, which is undefined behaviour (the module's
  /// table of the reader's integer arithmetic) — no build's reading can be
  /// claimed.
  Unsettled,
}

impl NumberError {
  /// What a refusal of the document says.
  pub(crate) const fn reason(self) -> &'static str {
    match self {
      Self::Invalid => "expected a number",
      Self::MissFraction => "expected a digit after `.`",
      Self::MissExponent => "expected a digit in the exponent",
      Self::TooBig => "a number too big for a double",
      Self::Unsettled => "a number OpenTimelineIO's builds read as different values",
    }
  }

  /// Whether RapidJSON reports it at the number's first byte, as it does
  /// `kParseErrorNumberTooBig` (`reader.h:1671`, `:1723`) — and the crate
  /// an unsettled number — rather than where it stopped.
  pub(crate) const fn at_start(self) -> bool {
    matches!(self, Self::TooBig | Self::Unsettled)
  }
}

/// What [`scan`] read: how far, and each build's verdict.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Scanned {
  /// The bytes read — where the number ends, or where a refusal stopped.
  pub(crate) end: usize,
  /// The verdict of a build that rounds `d * 10 + digit` twice, and of one
  /// that fuses it.
  pub(crate) readings: [Result<Number, NumberError>; 2],
}

impl Scanned {
  /// The verdict both builds give; [`NumberError::Unsettled`] where they
  /// differ.
  pub(crate) fn settled(&self) -> Result<Number, NumberError> {
    match self.readings {
      [Ok(a), Ok(b)] if a.same(b) => Ok(a),
      [Err(a), Err(b)] if a == b => Err(a),
      _ => Err(NumberError::Unsettled),
    }
  }

  const fn refused(end: usize, error: NumberError) -> Self {
    Self {
      end,
      readings: [Err(error); 2],
    }
  }
}

/// The verdict on `text`, a number and nothing after it, that both kinds of
/// build give ([`Scanned::settled`]).
pub(crate) fn number(text: &str) -> Result<Number, NumberError> {
  let scanned = scan(text.as_bytes());
  match scanned.settled() {
    Ok(_) if scanned.end != text.len() => Err(NumberError::Invalid),
    verdict => verdict,
  }
}

/// Each kind of build's verdict on `text`, a number and nothing after it:
/// the one that rounds `d * 10 + digit` twice, then the one that fuses it.
pub(crate) fn readings(text: &str) -> [Result<Number, NumberError>; 2] {
  let scanned = scan(text.as_bytes());
  if scanned.end == text.len() {
    scanned.readings
  } else {
    [Err(NumberError::Invalid); 2]
  }
}

/// Reads the number at the start of `bytes` as RapidJSON's `ParseNumber`
/// does at `06d58b9e` (`reader.h:1467–1748`), in a 64-bit build, under
/// `kParseNanAndInfFlag`: as far as it reads, and the callback each kind of
/// build makes ([`Scanned`]).
pub(crate) fn scan(bytes: &[u8]) -> Scanned {
  // One of `ParseNumber`'s C++ `int` operations, checked where RapidJSON
  // makes it: past an `int` it is undefined behaviour, and no build's
  // reading can be claimed (the module's table of the reader's integer
  // arithmetic).
  macro_rules! int {
    ($operation:expr) => {
      match $operation {
        Some(value) => value,
        None => return Scanned::refused(0, NumberError::Unsettled),
      }
    };
  }
  let peek = |at: usize| bytes.get(at).copied().unwrap_or(0);
  let digit = |at: usize| peek(at).is_ascii_digit();
  let value = |at: usize| peek(at).wrapping_sub(b'0');
  let mut at = 0;

  // The minus and the integer (1483–1522).
  let minus = peek(at) == b'-';
  if minus {
    at += 1;
  }
  let mut i: u32 = 0;
  let mut i64: u64 = 0;
  let mut use_64 = false;
  let mut significant: i32 = 0;
  // The significand as a double: a build's that rounds `d * 10 + digit`
  // twice, and one's that fuses it.
  let mut d = [0.0_f64; 2];
  let mut nan_or_inf = false;
  match peek(at) {
    b'0' => at += 1,
    b'1'..=b'9' => {
      i = u32::from(value(at));
      at += 1;
      // 2^31 = 2147483648 with a minus, 2^32 − 1 = 4294967295 without.
      let (cut, last) = if minus {
        (214_748_364, b'8')
      } else {
        (429_496_729, b'5')
      };
      while digit(at) {
        if i >= cut && (i != cut || peek(at) > last) {
          i64 = u64::from(i);
          use_64 = true;
          break;
        }
        i = i * 10 + u32::from(value(at));
        at += 1;
        significant = int!(significant.checked_add(1));
      }
    }
    // NaN and the infinities (1523–1548).
    b'N' => {
      at += 1;
      if peek(at) == b'a' {
        at += 1;
        if peek(at) == b'N' {
          at += 1;
          d = [f64::NAN; 2];
          nan_or_inf = true;
        }
      }
      if !nan_or_inf {
        return Scanned::refused(at, NumberError::Invalid);
      }
    }
    b'I' => {
      at += 1;
      if peek(at) == b'n' {
        at += 1;
        if peek(at) == b'f' {
          at += 1;
          let infinity = if minus {
            f64::NEG_INFINITY
          } else {
            f64::INFINITY
          };
          d = [infinity; 2];
          nan_or_inf = true;
          if peek(at) == b'i' {
            for letter in *b"inity" {
              if peek(at) != letter {
                return Scanned::refused(at, NumberError::Invalid);
              }
              at += 1;
            }
          }
        }
      }
      if !nan_or_inf {
        return Scanned::refused(at, NumberError::Invalid);
      }
    }
    _ => return Scanned::refused(at, NumberError::Invalid),
  }

  // The 64-bit integer (1550–1575), and past it a double (1577–1582).
  let mut use_double = false;
  if use_64 {
    // 2^63 = 9223372036854775808 with a minus, 2^64 − 1 =
    // 18446744073709551615 without.
    let (cut, last) = if minus {
      (0x0CCC_CCCC_CCCC_CCCC, b'8')
    } else {
      (0x1999_9999_9999_9999, b'5')
    };
    while digit(at) {
      if i64 >= cut && (i64 != cut || peek(at) > last) {
        d = [i64 as f64; 2];
        use_double = true;
        break;
      }
      i64 = i64 * 10 + u64::from(value(at));
      at += 1;
      significant = int!(significant.checked_add(1));
    }
  }
  if use_double {
    while digit(at) {
      d = tenfold(d, value(at));
      at += 1;
    }
  }

  // The fraction (1584–1630).
  let mut exp_frac: i32 = 0;
  if peek(at) == b'.' {
    at += 1;
    if !digit(at) {
      return Scanned::refused(at, NumberError::MissFraction);
    }
    if !use_double {
      if !use_64 {
        i64 = u64::from(i);
      }
      while digit(at) {
        // 2^53 − 1.
        if i64 > 0x001F_FFFF_FFFF_FFFF {
          break;
        }
        i64 = i64 * 10 + u64::from(value(at));
        at += 1;
        exp_frac = int!(exp_frac.checked_sub(1));
        if i64 != 0 {
          significant = int!(significant.checked_add(1));
        }
      }
      d = [i64 as f64; 2];
      use_double = true;
    }
    while digit(at) {
      if significant < 17 {
        d = tenfold(d, value(at));
        exp_frac = int!(exp_frac.checked_sub(1));
        // Both builds' significands are zero alike, until a digit is not.
        if d[0] > 0.0 {
          significant = int!(significant.checked_add(1));
        }
      }
      at += 1;
    }
  }

  // The exponent (1632–1680), and the power of ten it makes with the
  // fraction's places. Without one `exp` is 0, and `exp + expFrac` (1713)
  // is `expFrac`.
  let mut p = exp_frac;
  if matches!(peek(at), b'e' | b'E') {
    if !use_double {
      d = [(if use_64 { i64 } else { u64::from(i) }) as f64; 2];
      use_double = true;
    }
    match power(&bytes[at..], exp_frac) {
      Ok((read, power)) => {
        at += read;
        p = power;
      }
      Err((_, NumberError::Unsettled)) => return Scanned::refused(0, NumberError::Unsettled),
      Err((stopped, error)) => return Scanned::refused(at + stopped, error),
    }
  }

  // The callback (1682–1748).
  let readings = if use_double {
    d.map(|d| {
      let d = normal_precision(d, p);
      if d > f64::MAX {
        Err(NumberError::TooBig)
      } else {
        Ok(Number::Double(if minus { -d } else { d }))
      }
    })
  } else if nan_or_inf {
    [Ok(Number::Double(d[0])); 2]
  } else {
    let number = match (use_64, minus) {
      (true, true) => Number::Int64(i64.wrapping_neg() as i64),
      (true, false) => Number::Uint64(i64),
      (false, true) => Number::Int(i.wrapping_neg() as i32),
      (false, false) => Number::Uint(i),
    };
    [Ok(number); 2]
  };
  Scanned { end: at, readings }
}

/// The exponent at the start of `bytes`, its `e` or `E` included
/// (`reader.h:1632–1680` at `06d58b9e`), and the power of ten it makes with
/// a fraction of `exp_frac` places, `exp + expFrac` (1713). Each of the
/// reader's `int` operations is checked where it makes it, and an overflow is
/// [`NumberError::Unsettled`] (the module's table of the reader's integer
/// arithmetic). Returns the bytes read and the power, or the refusal and
/// where it stopped.
pub(super) fn power(bytes: &[u8], exp_frac: i32) -> Result<(usize, i32), (usize, NumberError)> {
  const UNSETTLED: (usize, NumberError) = (0, NumberError::Unsettled);
  let peek = |at: usize| bytes.get(at).copied().unwrap_or(0);
  let digit = |at: usize| peek(at).is_ascii_digit();
  let value = |at: usize| i32::from(peek(at).wrapping_sub(b'0'));
  // `exp = exp * 10 + digit` (1659, 1669).
  let more = |exp: i32, at: usize| {
    exp
      .checked_mul(10)
      .and_then(|exp| exp.checked_add(value(at)))
      .ok_or(UNSETTLED)
  };
  // Past the `e` or `E`.
  let mut at = 1;
  let minus = match peek(at) {
    b'+' => {
      at += 1;
      false
    }
    b'-' => {
      at += 1;
      true
    }
    _ => false,
  };
  if !digit(at) {
    return Err((at, NumberError::MissExponent));
  }
  // The first digit, before either loop's guard (1647).
  let mut exp = value(at);
  at += 1;
  if minus {
    // `(expFrac + 2147483639) / 10` (1656), an `int` division, which
    // truncates toward zero as an `i32`'s does.
    let max_exp = exp_frac.checked_add(2_147_483_639).ok_or(UNSETTLED)? / 10;
    while digit(at) {
      exp = more(exp, at)?;
      at += 1;
      if exp > max_exp {
        while digit(at) {
          at += 1;
        }
      }
    }
    exp = exp.checked_neg().ok_or(UNSETTLED)?;
  } else {
    // `308 - expFrac` (1667).
    let max_exp = 308_i32.checked_sub(exp_frac).ok_or(UNSETTLED)?;
    while digit(at) {
      exp = more(exp, at)?;
      at += 1;
      if exp > max_exp {
        return Err((at, NumberError::TooBig));
      }
    }
  }
  // `int p = exp + expFrac` (1713).
  let p = exp.checked_add(exp_frac).ok_or(UNSETTLED)?;
  Ok((at, p))
}

/// `d * 10 + digit`, each build's way: rounded twice, and fused.
fn tenfold(d: [f64; 2], digit: u8) -> [f64; 2] {
  [d[0] * 10.0 + f64::from(digit), fused(d[1], digit)]
}

/// `10 · d + digit` rounded once, to nearest, ties to even, as a build
/// that fuses `d * 10 + digit` computes it — `d` a whole number of zero or
/// more, or an infinity, as every significand the reader multiplies is.
fn fused(d: f64, digit: u8) -> f64 {
  /// 2^64.
  const BELOW: f64 = 18_446_744_073_709_551_616.0;
  if d < BELOW {
    // Below 2^64 a whole `d` and the sum are exact in a `u128`, which the
    // cast rounds once.
    return ((d as u128) * 10 + u128::from(digit)) as f64;
  }
  if d.is_infinite() {
    return d;
  }
  // d = m · 2^e, m of 53 bits and e at least 12: the digit lies wholly below
  // the last place of 10 · m · 2^e, so it only breaks a tie.
  let bits = d.to_bits();
  let e = ((bits >> 52) & 0x7FF) as i32 - 1075;
  let m = (bits & 0x000F_FFFF_FFFF_FFFF) | 0x0010_0000_0000_0000;
  // Below 2^57: of 56 bits or 57.
  let product = m * 10;
  let shift = 64 - product.leading_zeros() - 53;
  let mut kept = product >> shift;
  let rest = product & ((1 << shift) - 1);
  let half = 1 << (shift - 1);
  if rest > half || (rest == half && (digit > 0 || (kept & 1) == 1)) {
    kept += 1;
  }
  let mut e = e + shift as i32;
  if kept == 1 << 53 {
    kept >>= 1;
    e += 1;
  }
  // kept · 2^e, kept of 53 bits: its exponent field is e + 52 + 1023.
  let field = e + 52 + 1023;
  if field >= 0x7FF {
    return f64::INFINITY;
  }
  f64::from_bits(((field as u64) << 52) | (kept & 0x000F_FFFF_FFFF_FFFF))
}

/// `StrtodNormalPrecision` (`strtod.h:37–46` at `06d58b9e`): `d` scaled by
/// `10^p`, through 10^−308 first below it.
fn normal_precision(d: f64, p: i32) -> f64 {
  if p < -308 {
    fast_path(fast_path(d, -308), p + 308)
  } else {
    fast_path(d, p)
  }
}

/// `FastPath` (`strtod.h:28–35`): one multiplication or division by the
/// double nearest `10^exp`, or zero below 10^−308.
fn fast_path(significand: f64, exp: i32) -> f64 {
  let power = |n: i32| {
    usize::try_from(n)
      .ok()
      .and_then(|n| POW10.get(n).copied())
      .unwrap_or(f64::INFINITY)
  };
  if exp < -308 {
    0.0
  } else if exp >= 0 {
    significand * power(exp)
  } else {
    significand / power(-exp)
  }
}

/// `Pow10`'s table (`pow10.h:28–50`): the double nearest each power of ten
/// from 10^0 to 10^308, as a compiler reads each literal.
static POW10: [f64; 309] = [
  1e0, 1e1, 1e2, 1e3, 1e4, 1e5, 1e6, 1e7, 1e8, 1e9, 1e10, 1e11, 1e12, 1e13, 1e14, 1e15, 1e16, 1e17,
  1e18, 1e19, 1e20, 1e21, 1e22, 1e23, 1e24, 1e25, 1e26, 1e27, 1e28, 1e29, 1e30, 1e31, 1e32, 1e33,
  1e34, 1e35, 1e36, 1e37, 1e38, 1e39, 1e40, 1e41, 1e42, 1e43, 1e44, 1e45, 1e46, 1e47, 1e48, 1e49,
  1e50, 1e51, 1e52, 1e53, 1e54, 1e55, 1e56, 1e57, 1e58, 1e59, 1e60, 1e61, 1e62, 1e63, 1e64, 1e65,
  1e66, 1e67, 1e68, 1e69, 1e70, 1e71, 1e72, 1e73, 1e74, 1e75, 1e76, 1e77, 1e78, 1e79, 1e80, 1e81,
  1e82, 1e83, 1e84, 1e85, 1e86, 1e87, 1e88, 1e89, 1e90, 1e91, 1e92, 1e93, 1e94, 1e95, 1e96, 1e97,
  1e98, 1e99, 1e100, 1e101, 1e102, 1e103, 1e104, 1e105, 1e106, 1e107, 1e108, 1e109, 1e110, 1e111,
  1e112, 1e113, 1e114, 1e115, 1e116, 1e117, 1e118, 1e119, 1e120, 1e121, 1e122, 1e123, 1e124, 1e125,
  1e126, 1e127, 1e128, 1e129, 1e130, 1e131, 1e132, 1e133, 1e134, 1e135, 1e136, 1e137, 1e138, 1e139,
  1e140, 1e141, 1e142, 1e143, 1e144, 1e145, 1e146, 1e147, 1e148, 1e149, 1e150, 1e151, 1e152, 1e153,
  1e154, 1e155, 1e156, 1e157, 1e158, 1e159, 1e160, 1e161, 1e162, 1e163, 1e164, 1e165, 1e166, 1e167,
  1e168, 1e169, 1e170, 1e171, 1e172, 1e173, 1e174, 1e175, 1e176, 1e177, 1e178, 1e179, 1e180, 1e181,
  1e182, 1e183, 1e184, 1e185, 1e186, 1e187, 1e188, 1e189, 1e190, 1e191, 1e192, 1e193, 1e194, 1e195,
  1e196, 1e197, 1e198, 1e199, 1e200, 1e201, 1e202, 1e203, 1e204, 1e205, 1e206, 1e207, 1e208, 1e209,
  1e210, 1e211, 1e212, 1e213, 1e214, 1e215, 1e216, 1e217, 1e218, 1e219, 1e220, 1e221, 1e222, 1e223,
  1e224, 1e225, 1e226, 1e227, 1e228, 1e229, 1e230, 1e231, 1e232, 1e233, 1e234, 1e235, 1e236, 1e237,
  1e238, 1e239, 1e240, 1e241, 1e242, 1e243, 1e244, 1e245, 1e246, 1e247, 1e248, 1e249, 1e250, 1e251,
  1e252, 1e253, 1e254, 1e255, 1e256, 1e257, 1e258, 1e259, 1e260, 1e261, 1e262, 1e263, 1e264, 1e265,
  1e266, 1e267, 1e268, 1e269, 1e270, 1e271, 1e272, 1e273, 1e274, 1e275, 1e276, 1e277, 1e278, 1e279,
  1e280, 1e281, 1e282, 1e283, 1e284, 1e285, 1e286, 1e287, 1e288, 1e289, 1e290, 1e291, 1e292, 1e293,
  1e294, 1e295, 1e296, 1e297, 1e298, 1e299, 1e300, 1e301, 1e302, 1e303, 1e304, 1e305, 1e306, 1e307,
  1e308,
];
