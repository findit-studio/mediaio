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

  configure_videotoolbox();
  configure_ffmpeg_release();
}

fn use_feature(feature: &str) {
  println!("cargo:rustc-cfg={}", feature);
}

/// The FFmpeg releases after 5.1, the oldest this crate builds against,
/// that added a packet side-data kind `src/demuxer.rs` names, each with the
/// cfg that gates its kinds: `ffmpeg-sys-next`'s own flag for the release,
/// the gate `ffmpeg-next` puts on the same kinds
/// (`src/codec/packet/side_data.rs`).
const FFMPEG_RELEASES: [(&str, (u32, u32)); 5] = [
  ("ffmpeg_7_0", (7, 0)),
  ("ffmpeg_7_1", (7, 1)),
  ("ffmpeg_8_0", (8, 0)),
  ("ffmpeg_8_1", (8, 1)),
  ("ffmpeg_9_0", (9, 0)),
];

/// Gate each packet side-data kind a release after 5.1 added on the linked
/// FFmpeg being that release or newer.
///
/// # The question is the headers'
///
/// `ffmpeg-sys-next` generates its bindings from the installed headers, so
/// a kind those headers do not name is no name at all, and naming one fails
/// the build. Its build script reads the release from the same headers
/// (`LIBAVCODEC_VERSION_MAJOR` and `_MINOR`) and reports it in its `links`
/// metadata — `cargo:ffmpeg_7_0=true` and on — which Cargo hands this
/// script as `DEP_FFMPEG_FFMPEG_7_0`, this crate depending on it directly
/// for that alone. A kind the linked headers do not name cannot arrive from
/// that library either, so leaving it out loses nothing, and at run time a
/// kind the tables do not name persists, its packet never disposable.
///
/// # The one switch
///
/// `MEDIADECODE_FFMPEG_GATE_CEILING=<major>.<minor>` gates as though the
/// linked FFmpeg were no newer than that release — what a build against
/// that release's headers names, against the headers at hand — and sets
/// `ffmpeg_gate_ceiling` where it lowers a gate. The local gate builds so
/// to check that the tables and their laws stand with the newer kinds left
/// out.
fn configure_ffmpeg_release() {
  println!("cargo:rerun-if-env-changed=MEDIADECODE_FFMPEG_GATE_CEILING");
  let ceiling = env::var("MEDIADECODE_FFMPEG_GATE_CEILING")
    .ok()
    .map(|value| {
      let release = value.split_once('.').and_then(|(major, minor)| {
        Some((
          major.trim().parse::<u32>().ok()?,
          minor.trim().parse::<u32>().ok()?,
        ))
      });
      match release {
        Some(release) => (value, release),
        None => {
          panic!("MEDIADECODE_FFMPEG_GATE_CEILING={value:?} names no <major>.<minor> release")
        }
      }
    });
  let mut lowered = false;
  for (flag, release) in FFMPEG_RELEASES {
    let metadata = format!("DEP_FFMPEG_{}", flag.to_uppercase());
    println!("cargo:rerun-if-env-changed={metadata}");
    if env::var(&metadata).as_deref() != Ok("true") {
      continue;
    }
    if ceiling
      .as_ref()
      .is_some_and(|(_, ceiling)| release > *ceiling)
    {
      lowered = true;
      continue;
    }
    use_feature(flag);
  }
  if let Some((value, _)) = ceiling.filter(|_| lowered) {
    use_feature("ffmpeg_gate_ceiling");
    println!(
      "cargo:warning=mediadecode-ffmpeg: MEDIADECODE_FFMPEG_GATE_CEILING={value} gates the packet \
       side-data kinds as for that FFmpeg release: the kinds a newer one added are left out, and \
       a packet carrying one is never DISPOSABLE"
    );
  }
}

/// Decide whether `src/vtscale`'s VideoToolbox body can be compiled,
/// and link what it calls when it can.
///
/// # One question, and it is about the target
///
/// `VTPixelTransferSession` is `API_UNAVAILABLE(watchos)` and only
/// `API_AVAILABLE(ios(16.0), tvos(16.0))` — an availability window Rust
/// can express neither as a guard nor as a weak import, so an
/// older-deployment-target iOS or tvOS binary would carry a strong
/// import of a symbol its runtime does not export and die in the
/// dynamic loader at launch. macOS 10.8 and visionOS 1.0 sit below
/// every deployment target Rust supports there, so those two are the
/// whole list.
///
/// # And deliberately nothing about the FFmpeg
///
/// This decision does **not** probe the linked FFmpeg, and that is the
/// point rather than an omission. `src/vtscale`'s body names only Apple
/// system-framework symbols — `VTPixelTransferSession*`,
/// `CVPixelBufferGet*`, `CFRelease` — and no FFmpeg symbol whose
/// presence depends on how FFmpeg was configured. So there is nothing
/// here that a `--disable-videotoolbox` build, an include overlay, a
/// cross sysroot, `BINDGEN_EXTRA_CLANG_ARGS` or a vendored source build
/// could make absent, and therefore nothing for a build script to
/// predict about `bindgen`'s output and be wrong about.
///
/// An FFmpeg without VideoToolbox is handled where it belongs, at run
/// time: no VideoToolbox device opens, the decoder takes another
/// backend or software, and the stage never sees a frame. That is the
/// same stand-down every other condition it cannot honor takes.
///
/// # The one switch
///
/// `MEDIADECODE_FFMPEG_NO_VIDEOTOOLBOX` compiles the stage out by
/// request, for a build that would rather not link the frameworks or
/// carry the code at all.
fn configure_videotoolbox() {
  println!("cargo:rerun-if-env-changed=CARGO_CFG_TARGET_OS");
  println!("cargo:rerun-if-env-changed=MEDIADECODE_FFMPEG_NO_VIDEOTOOLBOX");

  let Ok(os) = var("CARGO_CFG_TARGET_OS") else {
    return;
  };
  if !matches!(os.as_str(), "macos" | "visionos") {
    return;
  }
  if env::var_os("MEDIADECODE_FFMPEG_NO_VIDEOTOOLBOX").is_some() {
    println!(
      "cargo:warning=mediadecode-ffmpeg: MEDIADECODE_FFMPEG_NO_VIDEOTOOLBOX is set, so the \
       VideoToolbox scaled-output stage was compiled out by request (scaled_output_capability \
       will answer Unsupported)"
    );
    return;
  }

  use_feature("ffmpeg_videotoolbox");
  // `VTPixelTransferSession*` is VideoToolbox, `CVPixelBufferGet*` is
  // CoreVideo, and `CFRelease` is CoreFoundation. VideoToolbox pulls the
  // other two in transitively on every Apple SDK, but naming each one
  // this crate actually calls keeps the link line a statement of what
  // the code does rather than a bet on a transitive edge.
  for framework in ["VideoToolbox", "CoreVideo", "CoreFoundation"] {
    println!("cargo:rustc-link-lib=framework={framework}");
  }
}
