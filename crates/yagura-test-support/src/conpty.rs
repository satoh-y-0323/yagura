//! Places the bundled pseudoconsole pair beside the executables a test starts.
//!
//! A Windows holder loads `conpty.dll` and `OpenConsole.exe` only from beside
//! its own executable. No build script puts them there, so a test does, from
//! `vendor/conpty/x64`, before it starts anything that loads them.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use yagura_os::bundled_conpty::{file_names, verify_beside};

/// Makes `dir` hold the pinned pair. A pair already there with the pinned
/// contents is kept; anything else is replaced from the vendored copy, which
/// is checked against the pins first and never copied when it differs. Done
/// once per directory per process: concurrent callers for the same directory
/// wait for the first one and share its outcome. Does nothing off Windows.
///
/// # Panics
///
/// Panics if the vendored pair differs from the pins, or if `dir` does not
/// hold the pinned pair after placing it, naming why.
pub fn place_bundled_conpty(dir: &Path) {
    if !cfg!(windows) {
        return;
    }
    if let Err(why) = placed_once(dir).get_or_init(|| place(dir)) {
        panic!("{why}");
    }
}

/// The outcome of placing the pair in one directory, set by its first caller.
type Placement = Arc<OnceLock<Result<(), String>>>;

/// The outcome cell of `dir`, shared by every caller in this process.
fn placed_once(dir: &Path) -> Placement {
    static PLACED: OnceLock<Mutex<HashMap<PathBuf, Placement>>> = OnceLock::new();
    let mut placed = PLACED
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    Arc::clone(placed.entry(dir.to_path_buf()).or_default())
}

fn place(dir: &Path) -> Result<(), String> {
    if verify_beside(dir).is_ok() {
        return Ok(());
    }
    let vendored = vendored_dir();
    verify_beside(&vendored).map_err(|fault| {
        format!(
            "the vendored bundled ConPTY in {} does not match the pins: {fault}",
            vendored.display()
        )
    })?;
    let mut copy_errors = Vec::new();
    for name in file_names() {
        if let Err(error) = replace_with_copy(&vendored.join(name), &dir.join(name)) {
            copy_errors.push(format!("{name}: {error}"));
        }
    }
    // A copy that could not be renamed over a file a running process holds
    // still passes here when the held file is already the pinned one.
    verify_beside(dir).map_err(|fault| {
        format!(
            "{} does not hold the pinned bundled ConPTY after placing it from {}: {fault} \
             (copy errors: {copy_errors:?})",
            dir.display(),
            vendored.display()
        )
    })
}

/// Copies `source` to a name unique to this call, then renames it over
/// `destination`, so no reader ever sees a partial file.
fn replace_with_copy(source: &Path, destination: &Path) -> std::io::Result<()> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let mut temporary = destination.as_os_str().to_owned();
    temporary.push(format!(
        ".{}.{}.tmp",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let temporary = PathBuf::from(temporary);
    let placed =
        std::fs::copy(source, &temporary).and_then(|_| std::fs::rename(&temporary, destination));
    if placed.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    placed
}

fn vendored_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../vendor/conpty/x64")
}
