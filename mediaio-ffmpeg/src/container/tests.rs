use super::*;

/// **The comma list is a list**, which is the whole reason
/// [`ContainerFormat::names`] exists: the raw name of the ISOBMFF
/// demuxer answers six words, and a consumer that read only
/// [`ContainerFormat::name`] would store a string no vocabulary parses.
#[test]
fn a_family_demuxer_walks_every_word_it_handles() {
  let format = ContainerFormat::new(Utf8Bytes::from("mov,mp4,m4a,3gp,3g2,mj2"), None);

  assert_eq!(format.name(), "mov,mp4,m4a,3gp,3g2,mj2");
  assert_eq!(
    format.names().collect::<Vec<_>>(),
    ["mov", "mp4", "m4a", "3gp", "3g2", "mj2"],
  );
}

/// A demuxer that handles exactly one format still walks — one word, and
/// no caller needs a second road for it.
#[test]
fn a_single_format_demuxer_walks_one_word() {
  let format = ContainerFormat::new(Utf8Bytes::from("flac"), Some(Utf8Bytes::from("raw FLAC")));

  assert_eq!(format.names().collect::<Vec<_>>(), ["flac"]);
  assert_eq!(format.long_name(), Some("raw FLAC"));
}

/// A description is optional and its absence is not the format's: the
/// name is the identity, and a nameless format is no value at all —
/// which is what [`ContainerFormat::from_context`] enforces at the door.
#[test]
fn a_format_without_a_description_is_still_a_format() {
  let format = ContainerFormat::new(Utf8Bytes::from("matroska,webm"), None);

  assert_eq!(format.long_name(), None);
  assert_eq!(format.names().collect::<Vec<_>>(), ["matroska", "webm"]);
}

/// Empty words are dropped rather than yielded. FFmpeg writes no such
/// name, and a walk that answered `""` would hand a consumer a word to
/// try against a vocabulary that can only refuse it.
#[test]
fn empty_words_are_not_names() {
  let format = ContainerFormat::new(Utf8Bytes::from("mp4,,mov,"), None);

  assert_eq!(format.names().collect::<Vec<_>>(), ["mp4", "mov"]);
}

/// The crossing seam #42 opens, exercised the way a consumer uses it:
/// the words are FFmpeg's own slugs, so a typed vocabulary's `FromStr`
/// meets them directly. Spelled here with a stand-in matcher rather than
/// a real vocabulary, because which crate a consumer crosses into is
/// none of this crate's business — what it owes is words that *can* be
/// crossed.
#[test]
fn the_words_are_what_a_typed_vocabulary_parses() {
  let format = ContainerFormat::new(Utf8Bytes::from("mov,mp4,m4a,3gp,3g2,mj2"), None);

  let recognised: Vec<_> = format
    .names()
    .filter(|word| matches!(*word, "mov" | "mp4" | "mkv" | "webm"))
    .collect();

  assert_eq!(
    recognised,
    ["mov", "mp4"],
    "a family demuxer can offer several words a vocabulary knows, and choosing between them \
     is the consumer's call rather than this crate's",
  );
}

/// A null context has no format — the shape a caller reaching this
/// helper before a successful open would be in.
#[test]
fn a_null_context_has_no_format() {
  // SAFETY: the null case is exactly what the contract admits, and the
  // helper answers it without a dereference.
  assert_eq!(
    unsafe { ContainerFormat::from_context(std::ptr::null()) },
    None,
  );
}

/// LAW: **a name no demuxer of the linked libavformat goes by, whole, is no
/// [`DemuxerName`], and is refused by name** — when the option is made,
/// before any input is opened. A demuxer's whole comma-list name is one,
/// and so is a single word that is a demuxer's whole name; a word of a
/// comma list is not, and the refusal names the whole name to give; nor is
/// a name FFmpeg has no demuxer for, a whole name in another case, or a
/// part of one. [`FromStr`] refuses as [`DemuxerName::registered`] does.
///
/// PLANT: a name taken as given, unchecked, turns this red at "refused by
/// name".
#[test]
fn a_name_no_registered_demuxer_goes_by_is_refused_by_name() {
  let mov = DemuxerName::registered("mov,mp4,m4a,3gp,3g2,mj2").expect("the ISOBMFF demuxer");
  assert_eq!(mov.as_str(), "mov,mp4,m4a,3gp,3g2,mj2");
  assert_eq!(mov.to_string(), "mov,mp4,m4a,3gp,3g2,mj2");
  let h264 = "h264"
    .parse::<DemuxerName>()
    .expect("the raw H.264 demuxer");
  assert_eq!(h264.as_str(), "h264");
  let format = ContainerFormat::new(Utf8Bytes::from("mov,mp4,m4a,3gp,3g2,mj2"), None);
  assert!(format.is(mov), "a session's format is its demuxer");
  assert!(!format.is(h264), "and no other");

  let word = DemuxerName::registered("mov").expect_err("refused by name: a word of a comma list");
  assert_eq!(word.name(), "mov");
  assert_eq!(word.word_of(), Some(mov), "the whole name to give");
  assert_eq!(
    word.to_string(),
    "no demuxer the linked FFmpeg registers is named \"mov\": it is a word of \
     \"mov,mp4,m4a,3gp,3g2,mj2\", the demuxer's whole name"
  );
  for name in ["not a demuxer", "MOV,MP4,M4A,3GP,3G2,MJ2", "mov,mp4", ""] {
    let refused = name.parse::<DemuxerName>().expect_err("refused by name");
    assert_eq!(refused.name(), name, "refused by name");
    assert_eq!(
      refused.word_of(),
      None,
      "{name:?}: a word of no demuxer's name"
    );
  }
}
