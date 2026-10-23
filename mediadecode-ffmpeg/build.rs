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
}

fn use_feature(feature: &str) {
  println!("cargo:rustc-cfg={}", feature);
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
