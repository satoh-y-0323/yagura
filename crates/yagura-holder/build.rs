//! Build script: on Windows, checks the vendored pseudoconsole against its
//! pinned hashes and places it beside the holder's executable and the test
//! executables.

use std::process::ExitCode;

/// Directory of the vendored pseudoconsole, relative to this crate's manifest.
const VENDORED_CONPTY_DIR: &str = "../../vendor/conpty/x64";

fn main() -> ExitCode {
    match yagura_build_support::place_bundled_conpty(VENDORED_CONPTY_DIR) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
