use super::*;

fn n(text: &str) -> Value {
  Value::Number(Utf8Bytes::from(text))
}

fn s(text: &str) -> Value {
  Value::String(Utf8Bytes::from(text))
}

fn pretty(value: &Value) -> Utf8Bytes {
  let mut out = Utf8BytesMut::new();
  write_pretty(value, &mut out);
  out.freeze_shared()
}

fn refusal(text: &str) -> (usize, &'static str) {
  let syntax = parse(text).unwrap_err();
  (syntax.offset(), syntax.reason())
}

#[test]
fn the_writer_indents_four_spaces_a_level_and_keeps_empty_containers_inline() {
  let value = Value::Object(vec![
    (Utf8Bytes::from("a"), n("1.0")),
    (
      Utf8Bytes::from("b"),
      Value::Array(vec![Value::Null, Value::Bool(true), s("x")]),
    ),
    (Utf8Bytes::from("c"), Value::Object(vec![])),
    (Utf8Bytes::from("d"), Value::Array(vec![])),
  ]);
  assert_eq!(
    pretty(&value),
    "{\n    \"a\": 1.0,\n    \"b\": [\n        null,\n        true,\n        \"x\"\n    ],\n    \"c\": {},\n    \"d\": []\n}"
  );
}

#[test]
fn strings_escape_the_quote_the_backslash_and_control_characters_only() {
  let text = "q\"b\\n\nr\rt\tb\u{8}f\u{c}u\u{1}é😀/";
  let written = pretty(&s(text));
  assert_eq!(written, "\"q\\\"b\\\\n\\nr\\rt\\tb\\bf\\fu\\u0001é😀/\"");
  assert_eq!(parse(&written), Ok(s(text)));
}

#[test]
fn what_is_written_reads_back() {
  let value = Value::Object(vec![
    (Utf8Bytes::from("n"), n("-0.5e-3")),
    (
      Utf8Bytes::from("deep"),
      Value::Array(vec![Value::Object(vec![(
        Utf8Bytes::from("k"),
        Value::Null,
      )])]),
    ),
  ]);
  assert_eq!(parse(&pretty(&value)), Ok(value));
}

#[test]
fn escapes_read_as_the_characters_they_name() {
  assert_eq!(parse(r#""é😀\/\"""#), Ok(s("é😀/\"")));
}

#[test]
fn numbers_keep_their_spelling() {
  assert_eq!(parse("86400.0"), Ok(n("86400.0")));
  assert_eq!(parse("-0"), Ok(n("-0")));
  assert_eq!(parse("1E+2"), Ok(n("1E+2")));
  // As OpenTimelineIO 0.17.0's parser reads them, under
  // `kParseNanAndInfFlag`.
  assert_eq!(parse("NaN"), Ok(n("NaN")));
  assert_eq!(
    parse("[-Infinity, Inf]"),
    Ok(Value::Array(vec![n("-Infinity"), n("Inf")]))
  );
  assert_eq!(n("23.976023976023978").as_f64(), Some(24_000.0 / 1001.0));
}

#[test]
fn what_the_grammar_does_not_admit_is_refused_where_it_breaks() {
  assert_eq!(refusal(""), (0, "expected a value"));
  assert_eq!(refusal("01"), (1, "text after the document"));
  assert_eq!(refusal("1."), (2, "expected a digit after `.`"));
  assert_eq!(refusal(".5"), (0, "expected a value"));
  assert_eq!(refusal("-"), (1, "expected a number"));
  assert_eq!(refusal("Infinit"), (7, "expected a number"));
  assert_eq!(refusal("nan"), (0, "expected `true`, `false` or `null`"));
  // What RapidJSON refuses, at the number's first byte, and what
  // OpenTimelineIO's builds read as different values.
  assert_eq!(refusal("0e309"), (0, "a number too big for a double"));
  assert_eq!(refusal("[1, 2e308]"), (4, "a number too big for a double"));
  assert_eq!(
    refusal("[0.90071992547409945]"),
    (
      1,
      "a number OpenTimelineIO's builds read as different values"
    )
  );
  assert_eq!(refusal("1e"), (2, "expected a digit in the exponent"));
  assert_eq!(refusal("nul"), (0, "expected `true`, `false` or `null`"));
  assert_eq!(refusal("[1,]"), (3, "expected a value"));
  assert_eq!(refusal("[1 2]"), (3, "expected `,` or `]`"));
  assert_eq!(refusal("{\"a\" 1}"), (5, "expected `:` after a key"));
  assert_eq!(refusal("{1: 2}"), (1, "expected a key"));
  assert_eq!(refusal("{\"a\": 1 \"b\": 2}"), (8, "expected `,` or `}`"));
  assert_eq!(refusal("\"open"), (5, "a string left open"));
  assert_eq!(
    refusal("\"a\nb\""),
    (2, "a control character inside a string")
  );
  assert_eq!(refusal(r#""\x""#), (2, "an unknown escape"));
  assert_eq!(refusal(r#""\u12""#), (5, "expected four hex digits"));
  assert_eq!(refusal(r#""\ud83d""#), (7, "a lone surrogate"));
  assert_eq!(refusal(r#""\ude00""#), (7, "a lone surrogate"));
  assert_eq!(refusal("{} {}"), (3, "text after the document"));
}

#[test]
fn a_key_named_twice_in_one_object_is_refused_at_the_second() {
  assert_eq!(
    refusal(r#"{"a": 1, "a": 2}"#),
    (9, "a key named twice in one object")
  );
  // The same key in two objects is two keys.
  assert!(parse(r#"[{"a": 1}, {"a": 2}]"#).is_ok());
}

#[test]
fn nesting_past_the_limit_is_refused_rather_than_recursed_into() {
  let deep = |levels: usize| {
    let mut text = Utf8BytesMut::new();
    for _ in 0..levels {
      text.push('[');
    }
    for _ in 0..levels {
      text.push(']');
    }
    text
  };
  assert!(parse(&deep(MAX_DEPTH)).is_ok());
  assert_eq!(
    refusal(&deep(MAX_DEPTH + 1)),
    (MAX_DEPTH, "nested too deeply")
  );
}

/// RapidJSON's verdict on `text` — the callback and its value, or the error
/// — and, where it reads a number, the value a dictionary keeps and the
/// `double` a member read as one takes, each as `{:?}` writes it.
fn verdict(
  text: &str,
) -> (
  std::string::String,
  Option<std::string::String>,
  Option<std::string::String>,
) {
  (
    std::format!("{:?}", number(text)),
    held(text).map(|held| std::format!("{held:?}")),
    double(text).map(|double| std::format!("{double:?}")),
  )
}

#[test]
fn the_classifier_gives_rapidjsons_verdict_row_by_row() {
  let integer_of_400 = "7".repeat(400);
  let read = |callback: &str, held: &str, double: &str| {
    (
      std::string::String::from(callback),
      Some(std::string::String::from(held)),
      Some(std::string::String::from(double)),
    )
  };
  let refused = |error: &str| (std::string::String::from(error), None, None);
  // The table in `number`'s docs, row by row: the callback, a dictionary's
  // value, and a `double`'s.
  let rows = [
    ("0e309", refused("Err(TooBig)")),
    ("0e-400", read("Ok(Double(0.0))", "Double(0.0)", "0.0")),
    (
      "0.1e309",
      read("Ok(Double(1e308))", "Double(1e308)", "1e308"),
    ),
    ("1e308", read("Ok(Double(1e308))", "Double(1e308)", "1e308")),
    ("2e308", refused("Err(TooBig)")),
    ("-0", read("Ok(Int(0))", "Integer(0)", "0.0")),
    ("-0.0", read("Ok(Double(-0.0))", "Double(-0.0)", "-0.0")),
    (
      "9223372036854775807",
      read(
        "Ok(Uint64(9223372036854775807))",
        "Integer(9223372036854775807)",
        "9.223372036854776e18",
      ),
    ),
    (
      "9223372036854775808",
      read("Ok(Uint64(9223372036854775808))", "Integer(0)", "0.0"),
    ),
    (
      "18446744073709551615",
      read(
        "Ok(Uint64(18446744073709551615))",
        "Integer(9223372036854775807)",
        "9.223372036854776e18",
      ),
    ),
    (
      "18446744073709551616",
      read(
        "Ok(Double(1.8446744073709552e19))",
        "Double(1.8446744073709552e19)",
        "1.8446744073709552e19",
      ),
    ),
    (
      "9007199254740992",
      read(
        "Ok(Uint64(9007199254740992))",
        "Integer(9007199254740992)",
        "9007199254740992.0",
      ),
    ),
    (
      "9007199254740993",
      read(
        "Ok(Uint64(9007199254740993))",
        "Integer(9007199254740993)",
        "9007199254740992.0",
      ),
    ),
    (integer_of_400.as_str(), refused("Err(TooBig)")),
    (
      "9007199254740991.0",
      read(
        "Ok(Double(9007199254740990.0))",
        "Double(9007199254740990.0)",
        "9007199254740990.0",
      ),
    ),
    (
      "24.0000000000000001",
      read("Ok(Double(24.0))", "Double(24.0)", "24.0"),
    ),
    ("0.90071992547409945", refused("Err(Unsettled)")),
    ("NaN", read("Ok(Double(NaN))", "Double(NaN)", "NaN")),
    (
      "-Infinity",
      read("Ok(Double(-inf))", "Double(-inf)", "-inf"),
    ),
  ];
  let wrong: Vec<&str> = rows
    .iter()
    .filter(|(text, expected)| verdict(text) != *expected)
    .map(|(text, _)| *text)
    .collect();
  assert!(wrong.is_empty(), "rows read otherwise: {wrong:?}");
}

#[test]
fn a_build_that_fuses_its_multiply_add_reads_some_long_numbers_otherwise() {
  // Its 17th digit is taken `d * 10 + digit` past 2^53: rounded twice, then
  // once (RapidJSON 06d58b9e compiled with `-ffp-contract=off` and `=on`).
  let doubles =
    readings("0.90071992547409945").map(|reading| reading.map(|number| number.double().to_bits()));
  assert_eq!(
    doubles,
    [Ok(0x3FEC_D2B2_97D8_89BE), Ok(0x3FEC_D2B2_97D8_89BF)]
  );
  // A significand that never reaches the step reads alike.
  let alike = readings("23.976023976023978").map(|reading| reading.map(Number::double));
  assert_eq!(alike, [Ok(24_000.0 / 1001.0); 2]);
}

/// RapidJSON's own readings: the reader at `06d58b9e` compiled twice and
/// fed each number — the text, then a reading by a build that rounds
/// `d * 10 + digit` twice, then by one that fuses it, as the file's header
/// says.
const RAPIDJSON: &str = include_str!("rapidjson.tsv");

#[test]
fn every_number_is_read_as_rapidjson_compiled_either_way_reads_it() {
  // A reading as the file writes one: the callback and its value, a double
  // as its bits; or RapidJSON's error code and where it reports it, 7 for a
  // number that ends before its text does.
  let shown = |scanned: &number::Scanned, build: usize, length: usize| match scanned.readings[build]
  {
    Ok(_) if scanned.end != length => std::format!("Error 7 {}", scanned.end),
    Ok(Number::Int(value)) => std::format!("Int {value}"),
    Ok(Number::Uint(value)) => std::format!("Uint {value}"),
    Ok(Number::Int64(value)) => std::format!("Int64 {value}"),
    Ok(Number::Uint64(value)) => std::format!("Uint64 {value}"),
    Ok(Number::Double(value)) => std::format!("Double {:016x}", value.to_bits()),
    Err(error) => {
      let code = match error {
        number::NumberError::Invalid => 3,
        number::NumberError::TooBig => 13,
        number::NumberError::MissFraction => 14,
        number::NumberError::MissExponent => 15,
        // No code of RapidJSON's: never in the file.
        number::NumberError::Unsettled => 0,
      };
      let at = if error.at_start() { 0 } else { scanned.end };
      std::format!("Error {code} {at}")
    }
  };
  let mut rows = 0;
  let mut wrong = Vec::new();
  for line in RAPIDJSON.lines().filter(|line| !line.starts_with('#')) {
    let mut columns = line.split('\t');
    let (Some(text), Some(twice), Some(fused)) = (columns.next(), columns.next(), columns.next())
    else {
      panic!("a row of three columns: {line}");
    };
    let scanned = number::scan(text.as_bytes());
    rows += 1;
    if [
      shown(&scanned, 0, text.len()),
      shown(&scanned, 1, text.len()),
    ] != [twice, fused]
    {
      wrong.push(text);
    }
  }
  assert_eq!(rows, 225);
  assert!(wrong.is_empty(), "read otherwise than RapidJSON: {wrong:?}");
}

#[test]
fn an_int_operation_of_the_reader_that_overflows_is_unsettled_where_rapidjson_makes_it() {
  use number::{
    NumberError::{TooBig, Unsettled},
    power,
  };
  // Each `expFrac` below is a fraction of that many leading zeros: too long
  // a text for a law, so the exponent is read beside it. Every case was
  // checked against RapidJSON 06d58b9e itself, built with UBSan's
  // signed-overflow check, both builds.
  //
  // `int p = exp + expFrac` (1713): -98 meets INT_MIN beside 2147483550
  // places, -99 passes it, and so does -1 beside 2^31.
  assert_eq!(power(b"e-98", -2_147_483_550), Ok((4, i32::MIN)));
  assert_eq!(power(b"e-99", -2_147_483_550), Err((0, Unsettled)));
  assert_eq!(power(b"e-0", i32::MIN), Ok((3, i32::MIN)));
  assert_eq!(power(b"e-1", i32::MIN), Err((0, Unsettled)));
  // Past `maxExp` a negative exponent's digits are consumed, not added
  // (1660–1663): `e-100` beside a bound of 8 is 10.
  assert_eq!(power(b"e-100", -2_147_483_550), Ok((5, -2_147_483_560)));
  // `308 - expFrac` (1667): INT_MAX at -2147483339, past it below.
  assert_eq!(power(b"e0", -2_147_483_339), Ok((2, -2_147_483_339)));
  assert_eq!(power(b"e2147483647", -2_147_483_339), Ok((11, 308)));
  assert_eq!(power(b"e0", -2_147_483_340), Err((0, Unsettled)));
  assert_eq!(power(b"e0", i32::MIN), Err((0, Unsettled)));
  // `exp * 10 + digit` (1669) under a bound of 214748364: 2147483647 passes
  // the bound without overflowing, 2147483648 overflows first. And the long
  // run alone is no refusal: `0.` and 214748056 zeros, then `e0`, reads.
  assert_eq!(power(b"e0", -214_748_056), Ok((2, -214_748_056)));
  assert_eq!(power(b"e214748364", -214_748_056), Ok((10, 308)));
  assert_eq!(power(b"e214748365", -214_748_056), Err((10, TooBig)));
  assert_eq!(power(b"e2147483647", -214_748_056), Err((11, TooBig)));
  assert_eq!(power(b"e2147483648", -214_748_056), Err((0, Unsettled)));
  // A short text at the `int`'s edges reads, or is refused as too big, with
  // no overflow on the way.
  for (text, read) in [
    ("1e2147483647", "Err(TooBig)"),
    ("0e2147483648", "Err(TooBig)"),
    ("1e-2147483648", "Ok(Double(0.0))"),
    ("1e-2147483649", "Ok(Double(0.0))"),
    ("-1e-2147483647", "Ok(Double(-0.0))"),
    ("1.5e-2147483648", "Ok(Double(0.0))"),
  ] {
    assert_eq!(std::format!("{:?}", number(text)), read, "{text}");
  }
}

#[test]
fn a_whole_number_is_the_integer_or_the_whole_double_a_dictionary_keeps() {
  for (text, value) in [
    ("86400.0", Some(86_400)),
    ("86400", Some(86_400)),
    ("-24.000", Some(-24)),
    ("0", Some(0)),
    ("-0.0", Some(0)),
    ("0e7", Some(0)),
    ("8.64e4", Some(86_400)),
    ("8640000e-2", Some(86_400)),
    ("1E+3", Some(1_000)),
    // An integer, kept exactly; past 2^63, cut to its low 63 bits.
    ("9007199254740993", Some(9_007_199_254_740_993)),
    ("9223372036854775808", Some(0)),
    // A double RapidJSON reads in its normal precision: 2^53 + 1 as 2^53,
    // and 2^52 + 0.5 as 2^52 + 1, a whole number.
    ("9007199254740993.0", Some(9_007_199_254_740_992)),
    ("4503599627370496.5", Some(4_503_599_627_370_497)),
    ("24.5", None),
    ("1e-1", None),
    ("8640001e-2", None),
    // Past an i128.
    ("1e39", None),
    // An exponent RapidJSON refuses, one it reads down to zero, and one of
    // leading zeros, which is its value.
    ("1e9999999999", None),
    ("1e-9223372036854775809", Some(0)),
    ("1e+00000000000000000000000000000000000000002", Some(100)),
  ] {
    assert_eq!(whole(text), value, "{text}");
  }
}

#[test]
fn a_double_is_the_one_opentimelineio_reads_and_a_float_the_one_its_spelling_reads_back() {
  let f64s = [
    // The shortest spelling of a double, as the export writes one.
    ("24.0", Some(24.0)),
    ("23.976023976023978", Some(24_000.0 / 1001.0)),
    ("0.1", Some(0.1)),
    ("-0.0", Some(-0.0)),
    ("5e-324", Some(f64::from_bits(1))),
    ("1.7976931348623157e308", Some(f64::MAX)),
    // Other spellings of the same value.
    ("24", Some(24.0)),
    ("2.4e1", Some(24.0)),
    ("2400e-2", Some(24.0)),
    ("0.24E+2", Some(24.0)),
    // An integer, cast to the nearest double: `-0` as zero, 2^53 + 1 as
    // 2^53.
    ("-0", Some(0.0)),
    ("9007199254740993", Some(9_007_199_254_740_992.0)),
    // RapidJSON's normal precision: the digits past the 17th dropped, a
    // longer decimal of 0.1 read as 0.1, an 18-digit spelling of 24000/1001
    // read as a neighbour of its `f64`.
    ("24.0000000000000001", Some(24.0)),
    ("0.1000000000000000055511151231257827", Some(0.1)),
    (
      "2.39760239760239780e1",
      Some(f64::from_bits(0x4037_F9DC_B511_2286)),
    ),
    // Below the smallest double, to it or to zero; past the largest,
    // refused.
    ("3e-324", Some(f64::from_bits(1))),
    ("2e-324", Some(0.0)),
    ("1e-400", Some(0.0)),
    ("1e309", None),
    ("-1.8e308", None),
    // NaN and the infinities, under `kParseNanAndInfFlag`.
    ("NaN", Some(f64::NAN)),
    ("-Infinity", Some(f64::NEG_INFINITY)),
  ];
  for (text, value) in f64s {
    assert_eq!(
      double(text).map(f64::to_bits),
      value.map(f64::to_bits),
      "{text}"
    );
  }
  let f32s = [
    ("16777216", Some(16_777_216.0)),
    ("1.6777216e7", Some(16_777_216.0)),
    ("-3.0", Some(-3.0)),
    ("-3e0", Some(-3.0)),
    ("-0.3E1", Some(-3.0)),
    ("0.1", Some(0.1)),
    ("1e-45", Some(f32::from_bits(1))),
    ("3.4028235e38", Some(f32::MAX)),
    // 2^24 + 1, which no f32 holds; the f32 0.1's own value, whose spelling
    // `0.1` reads as another double; a longer decimal of it.
    ("16777217", None),
    ("0.10000000149011612", None),
    ("0.100000001", None),
    // Past the largest f32, and below the smallest.
    ("1e39", None),
    ("3.5e38", None),
    ("1e-46", None),
  ];
  for (text, value) in f32s {
    assert_eq!(
      float(text).map(f32::to_bits),
      value.map(f32::to_bits),
      "{text}"
    );
  }
}

// The count is kept per thread, which needs `std`: without it the crate's
// `std` is `alloc`, which has no thread-locals.
#[cfg(feature = "std")]
std::thread_local! {
  /// How many times this thread's duplicate checks compared two keys.
  static COMPARISONS: core::cell::Cell<usize> = const { core::cell::Cell::new(0) };
}

/// Counts one comparison of two keys, for the law below.
#[cfg(feature = "std")]
pub(super) fn compared() {
  COMPARISONS.with(|count| count.set(count.get() + 1));
}

#[cfg(feature = "std")]
#[test]
fn a_large_flat_object_is_checked_for_a_twice_named_key_in_n_log_n_comparisons() {
  use core::fmt::Write as _;
  // Fewer keys under Miri, which interprets slowly: at a thousand, a scan's
  // n(n - 1)/2 = 499,500 comparisons is still over six times the bound.
  let n: usize = if cfg!(miri) { 1_000 } else { 20_000 };
  let mut text = std::string::String::from("{");
  for index in 0..n {
    if index > 0 {
      text.push(',');
    }
    write!(text, "\"key{index:05}\":{index}").expect("a string takes it");
  }
  text.push('}');
  COMPARISONS.with(|count| count.set(0));
  let value = parse(&text).expect("a flat object");
  let comparisons = COMPARISONS.with(core::cell::Cell::get);
  assert_eq!(value.as_object().map(<[_]>::len), Some(n));
  // Each key is compared on its way into the set — at least once — and a
  // search tree compares it a bounded number of times on each of O(log n)
  // levels: the standard library's B-tree scans at most eleven keys a
  // node, on at most 1 + log6(n) levels, so at most 66 times a key here,
  // within the 8 log2(n) = 120 the law allows. A scan of the keys before
  // each would cost n(n - 1)/2 = 2·10^8, over 80 times the bound.
  let log2 = usize::BITS - n.leading_zeros();
  assert!(
    comparisons >= n - 1,
    "the check compares every key: {comparisons}"
  );
  assert!(
    comparisons <= 8 * n * log2 as usize,
    "{comparisons} comparisons for {n} keys",
  );
}
