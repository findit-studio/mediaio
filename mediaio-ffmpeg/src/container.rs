//! [`ContainerFormat`] — what libavformat decided the bytes are wrapped
//! in, surfaced as the demuxer's own word — and [`DemuxerName`], a
//! demuxer the linked libavformat registers, named by a caller.
//!
//! Read once from `AVFormatContext.iformat` while the session is opened
//! and kept for its life: libavformat picks the demuxer during
//! `avformat_open_input` and never changes it, so re-reading could only
//! answer the same thing more expensively.

use core::{fmt, str::FromStr};

use smol_bytes::Utf8Bytes;

/// Upper bound on the NUL search over an `AVInputFormat`'s strings.
///
/// The longest name in FFmpeg's demuxer table is a comma list a few
/// dozen bytes long and the longest description a couple of hundred;
/// the cap is generous for both and exists only so that a
/// version-skewed table cannot turn the walk into an unbounded read.
const FORMAT_TEXT_MAX_BYTES: usize = 1024;

/// The container identification libavformat made for a session.
///
/// # This is the DEMUXER's identity, not a filename's
///
/// It is `AVInputFormat`'s own words — what libavformat concluded from
/// the *bytes*, having probed them. That is precisely what a
/// content-addressed row wants and what an extension cannot give: the
/// same bytes at `movie.mov` and `movie.mp4` are one content, and this
/// answers identically for both because it never looked at the path.
///
/// # One demuxer, several words — and [`name`](Self::name) is the list
///
/// FFmpeg registers one demuxer per *family*, so its name is a
/// comma-separated list of the short names that family handles:
/// `"mov,mp4,m4a,3gp,3g2,mj2"` for the ISOBMFF demuxer, `"matroska,webm"`
/// for Matroska. [`name`](Self::name) is that string verbatim and
/// [`names`](Self::names) walks it.
///
/// **The list does not narrow to one word, and this type does not
/// pretend it does.** libavformat identified the *demuxer*; which brand
/// inside that family a file is — an `.mp4` against an `.m4a` — is a
/// question it did not answer and one nothing here can answer for it.
/// A door that returned a single word would have had to pick, and a
/// pick is the guess this whole design exists to avoid.
///
/// # Crossing into a typed vocabulary
///
/// The words are FFmpeg's own slugs, which is what makes them
/// crossable: a consumer that wants
/// [`mediaframe::container::Format`](https://docs.rs/mediaframe) tries
/// [`names`](Self::names) against its `FromStr` and takes what it
/// recognises, deciding for itself what to do when a family offers
/// several. This crate stays out of that decision — the fold belongs to
/// whoever owns the vocabulary, and doing it here would put a second
/// one somewhere a caller cannot see it.
///
/// # An open vocabulary
///
/// Never an enum. A container FFmpeg learns to demux in its next
/// release names itself here with nothing to change on this side, which
/// is the whole reason the identity is carried as text.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ContainerFormat {
  name: Utf8Bytes,
  long_name: Option<Utf8Bytes>,
}

impl ContainerFormat {
  /// Constructs a `ContainerFormat` from a demuxer's short-name list and
  /// its optional description.
  ///
  /// Public so a consumer can build the value in a test or a fake; a
  /// session builds its own from `AVFormatContext.iformat`.
  #[inline]
  pub const fn new(name: Utf8Bytes, long_name: Option<Utf8Bytes>) -> Self {
    Self { name, long_name }
  }

  /// The demuxer's short-name list, verbatim — `"mov,mp4,m4a,3gp,3g2,mj2"`.
  ///
  /// See [`names`](Self::names) for the words in it.
  #[inline]
  pub fn name(&self) -> &str {
    self.name.as_str()
  }

  /// The demuxer's human description — `"QuickTime / MOV"` — or `None`
  /// where the table carries none.
  ///
  /// FFmpeg's prose, for display. It is not stable across releases and
  /// nothing should key on it.
  #[inline]
  pub fn long_name(&self) -> Option<&str> {
    self.long_name.as_deref()
  }

  /// The short names in [`name`](Self::name), in the order the demuxer
  /// lists them.
  ///
  /// Every FFmpeg name is a non-empty word or a comma list of them, so
  /// this yields at least one item for any format a session opened
  /// with.
  pub fn names(&self) -> impl Iterator<Item = &str> {
    self.name.split(',').filter(|word| !word.is_empty())
  }

  /// Whether this is the demuxer `demuxer`: its [`name`](Self::name) is
  /// `demuxer`'s, whole and exactly.
  #[inline]
  pub fn is(&self, demuxer: DemuxerName) -> bool {
    self.name() == demuxer.as_str()
  }

  /// Reads the format out of a live `AVFormatContext`, or `None` where
  /// it carries no `iformat` or the table's name is not readable text.
  ///
  /// The name is the identity: a format that cannot be named is no
  /// answer at all, so it is `None` rather than a value with an empty
  /// word. A missing *description* is not the same thing and keeps the
  /// value.
  ///
  /// # Safety
  ///
  /// `context` must be null — answered `None` — or point at a live,
  /// aligned `AVFormatContext` that meets [the `AVFormatContext`
  /// contract](crate::contracts#avformatcontext) for the duration of the
  /// call: its `iformat` is read, and through it the `name` and `long_name`
  /// strings, each up to its NUL or for 1,024 bytes. Those strings are
  /// libavformat's static tables, so the text returned borrows memory that
  /// lives for the process.
  #[doc(hidden)]
  pub unsafe fn from_context(context: *const ffmpeg_next::ffi::AVFormatContext) -> Option<Self> {
    if context.is_null() {
      return None;
    }
    // SAFETY: `context` is live per the contract; `iformat` is a public
    // field holding a pointer into libavformat's own demuxer table (or
    // null before a successful open).
    let iformat = unsafe { (*context).iformat };
    if iformat.is_null() {
      return None;
    }
    // SAFETY: a non-null `iformat` points at a `static const`
    // `AVInputFormat` compiled into libavformat — live for the process
    // and never written — so both string fields may be read, and each
    // is independently nullable.
    let (name, long_name) = unsafe { ((*iformat).name, (*iformat).long_name) };
    // SAFETY: both are null or NUL-terminated string literals in that
    // same static table; the reader answers `None` for null.
    // `from_static` rather than `from`: the reader borrows out of
    // libavformat's static table, and this stores that borrow instead
    // of copying it. FFmpeg has long names past the inline window — the
    // SER demuxer's is sixty-five bytes — so the copy was a real
    // allocation on an ordinary open, for a string the process already
    // owns.
    let name =
      Utf8Bytes::from_static(unsafe { crate::ffi::table_text(name, FORMAT_TEXT_MAX_BYTES) }?);
    let long_name = unsafe { crate::ffi::table_text(long_name, FORMAT_TEXT_MAX_BYTES) }
      .map(Utf8Bytes::from_static);
    Some(Self::new(name, long_name))
  }
}

/// A demuxer the linked libavformat registers, by its whole name: an
/// `AVInputFormat.name` exactly as libavformat spells it — the comma list
/// where FFmpeg registers one demuxer for a family,
/// `"mov,mp4,m4a,3gp,3g2,mj2"`, and never a word of it.
///
/// **Made only from a name the linked libavformat registers**
/// ([`Self::registered`], and [`FromStr`] with it): a name no demuxer goes
/// by, whole, is refused by name ([`UnregisteredDemuxer`]). So a value
/// always names a demuxer that can open an input, and it compares with a
/// session's [`ContainerFormat`] exactly ([`ContainerFormat::is`]).
///
/// The text is libavformat's own, borrowed from its static demuxer table,
/// which lives for the process: never copied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DemuxerName(&'static str);

impl DemuxerName {
  /// The demuxer the linked libavformat registers as `name`, whole.
  ///
  /// The registry is walked with `av_demuxer_iterate`, and `name`
  /// compared with each demuxer's `AVInputFormat.name` exactly, case and
  /// all.
  ///
  /// # Errors
  ///
  /// [`UnregisteredDemuxer`], naming `name`, where no demuxer is named
  /// `name` whole: a name FFmpeg does not register, one this build of
  /// libavformat leaves out, and a word of a family's comma list, such as
  /// `mov`, whose whole name [`UnregisteredDemuxer::word_of`] gives.
  pub fn registered(name: &str) -> Result<Self, UnregisteredDemuxer> {
    let mut word_of = None;
    for registered in registered_demuxers() {
      if registered == name {
        return Ok(Self(registered));
      }
      if word_of.is_none() && registered.split(',').any(|word| word == name) {
        word_of = Some(Self(registered));
      }
    }
    Err(UnregisteredDemuxer {
      name: Utf8Bytes::from(name),
      word_of,
    })
  }

  /// The demuxer's whole name, as libavformat spells it.
  #[inline]
  pub const fn as_str(&self) -> &'static str {
    self.0
  }
}

impl fmt::Display for DemuxerName {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(self.0)
  }
}

impl AsRef<str> for DemuxerName {
  #[inline]
  fn as_ref(&self) -> &str {
    self.0
  }
}

impl FromStr for DemuxerName {
  type Err = UnregisteredDemuxer;

  /// [`DemuxerName::registered`].
  ///
  /// # Errors
  ///
  /// [`UnregisteredDemuxer`], as [`DemuxerName::registered`] refuses.
  fn from_str(name: &str) -> Result<Self, Self::Err> {
    Self::registered(name)
  }
}

/// Why a name is no [`DemuxerName`]: no demuxer the linked libavformat
/// registers goes by it, whole. Made by [`DemuxerName::registered`] alone.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct UnregisteredDemuxer {
  name: Utf8Bytes,
  word_of: Option<DemuxerName>,
}

impl UnregisteredDemuxer {
  /// The name refused, as it was given.
  #[inline]
  pub fn name(&self) -> &str {
    self.name.as_str()
  }

  /// The demuxer whose comma-list name holds the refused one as a word,
  /// where one does: `mov,mp4,m4a,3gp,3g2,mj2` for `mov`. Its whole name
  /// is the one to give.
  #[inline]
  pub const fn word_of(&self) -> Option<DemuxerName> {
    self.word_of
  }
}

impl fmt::Display for UnregisteredDemuxer {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(
      f,
      "no demuxer the linked FFmpeg registers is named {:?}",
      self.name()
    )?;
    match self.word_of {
      Some(whole) => write!(
        f,
        ": it is a word of {:?}, the demuxer's whole name",
        whole.0
      ),
      None => Ok(()),
    }
  }
}

impl core::error::Error for UnregisteredDemuxer {}

/// Every name a demuxer of the linked libavformat goes by, whole, in the
/// order `av_demuxer_iterate` answers them. A demuxer whose name is not
/// readable text, which FFmpeg's table holds none of, is passed over.
fn registered_demuxers() -> impl Iterator<Item = &'static str> {
  let mut opaque = core::ptr::null_mut();
  core::iter::from_fn(move || {
    loop {
      // SAFETY: `opaque` is the iteration state `av_demuxer_iterate` keeps,
      // started null and handed back as it left it; each demuxer it answers
      // is a `static const` entry of libavformat's own table.
      let demuxer = unsafe { ffmpeg_next::ffi::av_demuxer_iterate(&mut opaque) };
      if demuxer.is_null() {
        return None;
      }
      // SAFETY: a non-null entry lives for the process, its `name` a
      // NUL-terminated string literal of that table, or null, which the
      // reader answers `None`.
      if let Some(name) = unsafe { crate::ffi::table_text((*demuxer).name, FORMAT_TEXT_MAX_BYTES) }
      {
        return Some(name);
      }
    }
  })
}

#[cfg(test)]
mod tests;
