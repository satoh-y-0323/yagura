#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// Integration-test target: clippy's `allow-unwrap-in-tests` /
// `allow-expect-in-tests` cover only `#[cfg(test)]` code, so a `tests/` file
// needs this header explicitly.

//! Contract tests for the per-process run directory that owns every
//! temporary directory handed out by `yagura_test_support::paths`.

use std::fs::{File, TryLockError};
use std::path::{Path, PathBuf};

use yagura_test_support::paths;

fn parent_of(dir: &Path) -> PathBuf {
    dir.parent()
        .unwrap_or_else(|| panic!("temp dir must have a parent: {dir:?}"))
        .to_path_buf()
}

fn run_dir() -> PathBuf {
    parent_of(&paths::unique_temp_dir("yagura-red-run", "probe"))
}

#[test]
fn every_temp_dir_constructor_places_its_directory_in_one_shared_run_dir() {
    let from_temp_dir = parent_of(&paths::unique_temp_dir("yagura-red-run", "temp"));
    let from_detection_dir = parent_of(&paths::unique_detection_dir(
        "yagura-red-run",
        "detection",
        "create detection dir",
    ));
    let fixture = paths::TempDir::new("yagura-red-run", "fixture");
    let from_fixture = parent_of(fixture.path());

    assert_eq!(
        from_temp_dir, from_detection_dir,
        "unique_temp_dir and unique_detection_dir must share one run dir"
    );
    assert_eq!(
        from_temp_dir, from_fixture,
        "unique_temp_dir and TempDir::new must share one run dir"
    );
    assert_eq!(
        parent_of(&from_temp_dir),
        std::env::temp_dir().join("yagura-tests"),
        "the run dir must sit directly under temp_dir()/yagura-tests"
    );
    assert!(
        from_temp_dir.join("owner.lock").is_file(),
        "the run dir must contain owner.lock: {from_temp_dir:?}"
    );
}

#[test]
fn run_dir_owner_lock_is_held_exclusively_while_the_process_lives() {
    let lock_path = run_dir().join("owner.lock");
    let other_handle = File::options()
        .read(true)
        .write(true)
        .open(&lock_path)
        .unwrap_or_else(|e| panic!("open {lock_path:?}: {e}"));

    // Both LockFileEx and flock lock per handle, so a second handle in this
    // same process must be refused while the owner handle is alive.
    match other_handle.try_lock() {
        Err(TryLockError::WouldBlock) => {}
        Err(TryLockError::Error(e)) => panic!("try_lock on {lock_path:?} failed: {e}"),
        Ok(()) => panic!("owner.lock must not be lockable by another handle: {lock_path:?}"),
    }
}
