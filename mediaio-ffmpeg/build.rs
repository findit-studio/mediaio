use std::env::{self, var};

fn main() {
  // Don't rerun this on changes other than build.rs, as we only depend on
  // the rustc version.
  println!("cargo:rerun-if-changed=build.rs");

  // Check for `--features=tarpaulin`.
  let tarpaulin = var("CARGO_FEATURE_TARPAULIN").is_ok();

  if tarpaulin {
    use_feature("tarpaulin");
  } else {
    // Always rerun if these env vars change.
    println!("cargo:rerun-if-env-changed=CARGO_TARPAULIN");
    println!("cargo:rerun-if-env-changed=CARGO_CFG_TARPAULIN");

    // Detect tarpaulin by environment variable
    if env::var("CARGO_TARPAULIN").is_ok() || env::var("CARGO_CFG_TARPAULIN").is_ok() {
      use_feature("tarpaulin");
    }
  }

  // Rerun this script if any of our features or configuration flags change,
  // or if the toolchain we used for feature detection changes.
  println!("cargo:rerun-if-env-changed=CARGO_FEATURE_TARPAULIN");

  require_ffmpeg_9();
}

/// The FFmpeg releases `ffmpeg-sys-next` names in its `links` metadata, each
/// with the flag it reports for it: every one but the floor is here only to
/// name an FFmpeg older than it.
const FFMPEG_RELEASES: [(&str, &str); 19] = [
  ("ffmpeg_3_0", "3.0"),
  ("ffmpeg_3_1", "3.1"),
  ("ffmpeg_3_2", "3.2"),
  ("ffmpeg_3_3", "3.3"),
  ("ffmpeg_3_4", "3.4"),
  ("ffmpeg_4_0", "4.0"),
  ("ffmpeg_4_1", "4.1"),
  ("ffmpeg_4_2", "4.2"),
  ("ffmpeg_4_3", "4.3"),
  ("ffmpeg_4_4", "4.4"),
  ("ffmpeg_5_0", "5.0"),
  ("ffmpeg_5_1", "5.1"),
  ("ffmpeg_6_0", "6.0"),
  ("ffmpeg_6_1", "6.1"),
  ("ffmpeg_7_0", "7.0"),
  ("ffmpeg_7_1", "7.1"),
  ("ffmpeg_8_0", "8.0"),
  ("ffmpeg_8_1", "8.1"),
  ("ffmpeg_9_0", "9.0"),
];

/// **FFmpeg 9 is the floor**, for this crate and every crate of the family
/// built on it, and an older FFmpeg is refused here, by name, before a line
/// of Rust is compiled against headers that lack what the family names.
///
/// # How the release is read
///
/// `ffmpeg-sys-next` reads the linked release from the installed headers —
/// `LIBAVCODEC_VERSION_MAJOR` and `_MINOR` — and reports it in its `links`
/// metadata as one flag per release the headers are, or are newer than:
/// `cargo:ffmpeg_9_0=true` from libavcodec 63.1, FFmpeg 9.0's. Cargo hands
/// that metadata to the build script of a crate that depends on
/// `ffmpeg-sys-next` directly, as `DEP_FFMPEG_FFMPEG_9_0`, which is why this
/// crate does, for that alone. The newest flag that is set names the release
/// an older FFmpeg is.
fn require_ffmpeg_9() {
  let set = |flag: &str| {
    let name = format!("DEP_FFMPEG_{}", flag.to_uppercase());
    println!("cargo:rerun-if-env-changed={name}");
    env::var(name).as_deref() == Ok("true")
  };
  let linked = FFMPEG_RELEASES
    .iter()
    .rev()
    .find(|(flag, _)| set(flag))
    .map(|&(_, release)| release);
  if linked != Some("9.0") {
    let linked = linked.map_or_else(
      || "older than FFmpeg 3.0, the oldest ffmpeg-sys-next names".to_string(),
      |release| format!("FFmpeg {release}"),
    );
    println!(
      "cargo::error=the mediaio FFmpeg crates need FFmpeg 9 or newer, and the FFmpeg they are \
       being built against is {linked}, as ffmpeg-sys-next reads its installed headers: install \
       FFmpeg 9's libraries and headers, and point pkg-config (PKG_CONFIG_PATH) or FFMPEG_DIR at \
       them"
    );
  }
}

fn use_feature(feature: &str) {
  println!("cargo:rustc-cfg={}", feature);
}
