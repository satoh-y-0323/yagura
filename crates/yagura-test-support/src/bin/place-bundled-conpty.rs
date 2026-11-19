//! Makes a directory hold the pinned bundled ConPTY pair.
//!
//! Usage: `place-bundled-conpty <directory>`. Exits 0 when the directory holds
//! the pinned pair, non-zero with the reason on stderr otherwise.

use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1);
    let (Some(dir), None) = (args.next(), args.next()) else {
        eprintln!("usage: place-bundled-conpty <directory>");
        return ExitCode::from(2);
    };
    match yagura_test_support::conpty::try_place_bundled_conpty(&PathBuf::from(dir)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) => {
            eprintln!("place-bundled-conpty: {why}");
            ExitCode::FAILURE
        }
    }
}
