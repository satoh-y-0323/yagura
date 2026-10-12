//! Build script: embed bundled detection definitions and the shipped launch
//! profiles into the daemon binary.
//! Ensures version skew is impossible: the same *.toml files built in are used.
//!
//! On Windows it also checks the vendored pseudoconsole against its pinned hashes
//! and places it beside the daemon's executable and the test executables.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use yagura_build_support::{EmbedTomlDir, KeysConst, embed_toml_dir};

// The pinned hashes live in one file shared with `yagura-os`, so this check and
// the run-time check cannot disagree.
#[path = "../yagura-os/src/bundled_conpty_pins.rs"]
mod bundled_conpty_pins;

/// Included by `detector/definition.rs`.
const BUNDLED_DEFINITIONS: EmbedTomlDir<'static> = EmbedTomlDir {
    source_dir: "detection",
    out_file: "bundled_definitions.rs",
    const_name: "BUNDLED_DEFINITIONS",
    doc: &[
        "Every bundled detection definition, generated from this crate's",
        "`detection/` directory by `build.rs`. Names are file stems, validated",
        "at build time. Never empty (an empty directory fails the build).",
    ],
    keys_const: Some(KeysConst {
        name: "BUNDLED_NAMES",
        doc: &[
            "Every bundled definition name in byte order, extracted from",
            "BUNDLED_DEFINITIONS.",
        ],
    }),
};

/// Included by `profile.rs`. Generated rather than listed in the sources, so
/// no profile or vendor name is written into the daemon's code.
const BUNDLED_PROFILES: EmbedTomlDir<'static> = EmbedTomlDir {
    source_dir: "../../profiles",
    out_file: "bundled_profiles.rs",
    const_name: "BUNDLED_PROFILES",
    doc: &[
        "The launch profiles shipped with the application, generated from the",
        "repository's `profiles/` directory by `build.rs`, in byte order of id.",
    ],
    keys_const: None,
};

/// Directory of the vendored pseudoconsole, relative to this crate's manifest.
const VENDORED_CONPTY_DIR: &str = "../../vendor/conpty/x64";

/// Verifies the vendored pseudoconsole and copies it into the profile directory
/// (beside the daemon's executable) and `deps/` (beside the test executables).
fn place_bundled_conpty() -> Result<(), String> {
    println!("cargo:rerun-if-changed={VENDORED_CONPTY_DIR}");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return Ok(());
    }
    if std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() != Ok("x86_64") {
        return Err("the bundled ConPTY is vendored for x86_64 only".to_string());
    }
    let manifest_dir = std::env::var_os("CARGO_MANIFEST_DIR")
        .map(PathBuf::from)
        .ok_or("CARGO_MANIFEST_DIR is not set")?;
    let source = manifest_dir.join(VENDORED_CONPTY_DIR);
    bundled_conpty_pins::check_dir(&source)
        .map_err(|fault| format!("vendor/conpty/x64 does not match the pinned build: {fault}"))?;

    // OUT_DIR is <profile>/build/<package>-<hash>/out.
    let out_dir = std::env::var_os("OUT_DIR")
        .map(PathBuf::from)
        .ok_or("OUT_DIR is not set")?;
    let profile_dir = out_dir
        .ancestors()
        .nth(3)
        .ok_or("OUT_DIR is not inside a profile directory")?;
    for target in [profile_dir.to_path_buf(), profile_dir.join("deps")] {
        place(&source, &target)?;
    }
    Ok(())
}

/// Copies the pinned files into `target`, leaving a file alone when it already
/// has the pinned contents (a running daemon holds its copy open).
fn place(source: &Path, target: &Path) -> Result<(), String> {
    std::fs::create_dir_all(target)
        .map_err(|error| format!("cannot create {}: {error}", target.display()))?;
    if bundled_conpty_pins::check_dir(target).is_ok() {
        return Ok(());
    }
    for pinned in &bundled_conpty_pins::PINNED_FILES {
        std::fs::copy(source.join(pinned.name), target.join(pinned.name)).map_err(|error| {
            format!(
                "cannot copy {} into {}: {error}",
                pinned.name,
                target.display()
            )
        })?;
    }
    Ok(())
}

fn main() -> ExitCode {
    let result = embed_toml_dir(&BUNDLED_DEFINITIONS)
        .and_then(|()| embed_toml_dir(&BUNDLED_PROFILES))
        .map_err(|error| error.to_string())
        .and_then(|()| place_bundled_conpty());
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
