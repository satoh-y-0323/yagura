#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// Integration-test target: clippy's `allow-unwrap-in-tests` /
// `allow-expect-in-tests` cover only `#[cfg(test)]` code, so a `tests/` file
// needs this header explicitly.

//! Contract tests for `yagura_test_support::paths::sweep_dead_runs`.
//!
//! Kept apart from `run_dir_contract.rs` so a missing sweep API cannot stop
//! the run-dir contract from compiling.

use std::fs::File;
use std::path::{Path, PathBuf};

use yagura_test_support::paths::{self, sweep_dead_runs};

const CONTENT_FILE: &str = "content.txt";

/// Fake run directories built under a base this test owns, so the sweep
/// never touches the real `yagura-tests` tree of this or another process.
struct FakeRuns {
    base: PathBuf,
    live: PathBuf,
    dead: PathBuf,
    // Held for the whole test: the live run's owner is "alive".
    _live_owner: File,
}

fn make_run(base: &Path, name: &str) -> PathBuf {
    let run = base.join(name);
    std::fs::create_dir_all(&run).expect("create fake run dir");
    std::fs::write(run.join("owner.lock"), b"").expect("write owner.lock");
    std::fs::write(run.join(CONTENT_FILE), b"payload").expect("write content");
    run
}

fn fake_runs(tag: &str) -> FakeRuns {
    let base = paths::unique_temp_dir("yagura-red-sweep", tag);
    let live = make_run(&base, "1-live");
    let dead = make_run(&base, "2-dead");
    let live_owner = File::options()
        .read(true)
        .write(true)
        .open(live.join("owner.lock"))
        .expect("open live owner.lock");
    live_owner
        .try_lock()
        .expect("lock live owner.lock before the sweep");
    FakeRuns {
        base,
        live,
        dead,
        _live_owner: live_owner,
    }
}

#[test]
fn sweep_removes_a_run_dir_whose_owner_lock_is_free() {
    let runs = fake_runs("dead");

    sweep_dead_runs(&runs.base);

    assert!(
        !runs.dead.exists(),
        "a run dir with a free owner.lock must be swept: {:?}",
        runs.dead
    );
}

#[cfg(windows)]
#[test]
fn sweep_keeps_the_owner_lock_of_a_dead_run_dir_it_cannot_empty() {
    use std::os::windows::fs::OpenOptionsExt;

    let runs = fake_runs("stuck");
    // Opened without sharing, so the sweep cannot remove it while it is held.
    let _held = File::options()
        .read(true)
        .share_mode(0)
        .open(runs.dead.join(CONTENT_FILE))
        .expect("hold the dead run's content");

    sweep_dead_runs(&runs.base);

    assert!(
        runs.dead.join("owner.lock").is_file(),
        "a dead run dir the sweep could not empty must keep its owner.lock so the next sweep \
         still recognises it: {:?}",
        runs.dead
    );
}

#[test]
fn sweep_leaves_a_run_dir_whose_owner_lock_is_held_intact() {
    let runs = fake_runs("live");

    sweep_dead_runs(&runs.base);

    assert!(
        runs.live.join("owner.lock").is_file(),
        "a held owner.lock must survive the sweep: {:?}",
        runs.live
    );
    assert_eq!(
        std::fs::read(runs.live.join(CONTENT_FILE)).expect("read live content"),
        b"payload",
        "the contents of a live run dir must survive the sweep: {:?}",
        runs.live
    );
}
