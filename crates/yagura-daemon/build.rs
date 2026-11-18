//! Build script: embed bundled detection definitions and the shipped launch
//! profiles into the daemon binary.
//! Ensures version skew is impossible: the same *.toml files built in are used.
//!
//! On Windows it also checks the vendored pseudoconsole against its pinned
//! hashes; it does not copy it anywhere.

use std::process::ExitCode;

use yagura_build_support::{EmbedTomlDir, KeysConst, check_bundled_conpty, embed_toml_dir};

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

fn main() -> ExitCode {
    let result = embed_toml_dir(&BUNDLED_DEFINITIONS)
        .and_then(|()| embed_toml_dir(&BUNDLED_PROFILES))
        .map_err(|error| error.to_string())
        .and_then(|()| check_bundled_conpty(VENDORED_CONPTY_DIR));
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
