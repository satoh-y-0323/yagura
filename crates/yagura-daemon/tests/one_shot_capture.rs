#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// `yagura_os::spawn_captured` is called directly here: it is the unit under test.
#![allow(clippy::disallowed_methods)]

//! Integration test: the one-shot, non-PTY capture path
//! (`yagura_os::spawn_captured` / `CapturedChild::collect` /
//! `CapturedChild::collect_until`) and its daemon wrapper
//! (`yagura_daemon::pty::spawn_one_shot`).
//!
//! Every case spawns the `capture_child` fixture binary (see
//! `src/bin/capture_child.rs`), never a real vendor CLI. The only shell in
//! the way is the Unix-only launcher that puts one grandchild outside the
//! capture's process group.

use std::ffi::OsString;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use yagura_daemon::child_env::ChildBaseEnv;
use yagura_daemon::pty::CaptureChildEnv;
use yagura_os::{CaptureEnv, CaptureLimits, CaptureOutcome, MarkerOutcome};
use yagura_test_support::paths::TempDir;
use yagura_test_support::process::{assert_file_len_stable, wait_until_file_grows};

/// How long a control wait for the grandchild's first tick, or for a normal
/// exit, may take before the test gives up and fails loud.
const GROWTH_TIMEOUT: Duration = Duration::from_secs(10);

/// How long [`assert_file_len_stable`] keeps sampling for renewed growth,
/// after its own fixed settle wait for an in-flight teardown to land.
const OBSERVATION: Duration = Duration::from_secs(3);

/// Path to the fixture binary `name` that belongs to the test binary
/// currently running.
///
/// `cargo test` places a test binary in `target/{profile}/deps/`, while a
/// plain bin target lands in `target/{profile}/`, so the search steps up one
/// extra level when the parent directory is `deps`.
///
/// # Panics
///
/// Panics if the running executable's own path cannot be resolved, or if the
/// fixture binary is not present next to it -- build it with
/// `cargo build -p yagura-daemon --bin <name>` first.
fn fixture_path(name: &str) -> PathBuf {
    let mut dir =
        std::env::current_exe().expect("current_exe should resolve for a running test binary");
    dir.pop();
    if dir.file_name().and_then(|n| n.to_str()) == Some("deps") {
        dir.pop();
    }
    dir.push(if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    });
    assert!(
        dir.exists(),
        "the {name} fixture binary is missing at {}: run `cargo build -p yagura-daemon --bin \
         {name}` first",
        dir.display()
    );
    dir
}

fn capture_child_path() -> PathBuf {
    fixture_path("capture_child")
}

/// The argv for a window running `capture_child`: its own path followed by
/// `mode_args` verbatim, with no shell wrapping.
fn capture_child_argv(mode_args: &[&str]) -> Vec<String> {
    let mut argv = vec![
        capture_child_path()
            .to_str()
            .expect("capture_child path must be valid UTF-8")
            .to_string(),
    ];
    argv.extend(mode_args.iter().map(|s| (*s).to_string()));
    argv
}

fn generous_limits() -> CaptureLimits {
    CaptureLimits {
        timeout: GROWTH_TIMEOUT,
        max_output_bytes: 1 << 20,
    }
}

// ---------------------------------------------------------------------
// A normal exit returns its stdout bytes and its exit code unchanged.
// ---------------------------------------------------------------------

#[test]
fn a_normal_exit_returns_its_stdout_bytes_and_zero_code() {
    let argv = capture_child_argv(&["echo", "0", "hello capture"]);
    let child = yagura_os::spawn_captured(&argv, None, CaptureEnv::Inherit(&[]), None)
        .expect("spawn should succeed");
    let outcome = child.collect(generous_limits());
    match outcome {
        CaptureOutcome::Exited { code, stdout, .. } => {
            assert_eq!(code, Some(0));
            assert_eq!(stdout, b"hello capture");
        }
        other => panic!("expected Exited, got {other:?}"),
    }
}

#[test]
fn a_nonzero_exit_code_is_returned_unchanged() {
    let argv = capture_child_argv(&["echo", "7", "partial output"]);
    let child = yagura_os::spawn_captured(&argv, None, CaptureEnv::Inherit(&[]), None)
        .expect("spawn should succeed");
    let outcome = child.collect(generous_limits());
    match outcome {
        CaptureOutcome::Exited { code, stdout, .. } => {
            assert_eq!(code, Some(7));
            assert_eq!(stdout, b"partial output");
        }
        other => panic!("expected Exited, got {other:?}"),
    }
}

// ---------------------------------------------------------------------
// Stdout and stderr are captured into their own fields and never mixed
// with one another (superseding the old assumption that stderr was
// discarded: it is now kept, just apart from stdout).
// ---------------------------------------------------------------------

#[test]
fn stdout_and_stderr_are_captured_separately_and_never_mixed() {
    let argv = capture_child_argv(&[
        "echo",
        "0",
        "STDOUT-SENTINEL-VISIBLE",
        "STDERR-SENTINEL-VISIBLE",
    ]);
    let child = yagura_os::spawn_captured(&argv, None, CaptureEnv::Inherit(&[]), None)
        .expect("spawn should succeed");
    let outcome = child.collect(generous_limits());

    match outcome {
        CaptureOutcome::Exited { stdout, stderr, .. } => {
            assert_eq!(stdout, b"STDOUT-SENTINEL-VISIBLE");
            assert_eq!(stderr, b"STDERR-SENTINEL-VISIBLE");
        }
        other => panic!("expected Exited, got {other:?}"),
    }
}

#[test]
fn stderr_is_returned_in_full_for_a_child_that_writes_only_to_stderr() {
    let argv = capture_child_argv(&["echo", "0", "", "STDERR-ONLY-TEXT"]);
    let child = yagura_os::spawn_captured(&argv, None, CaptureEnv::Inherit(&[]), None)
        .expect("spawn should succeed");
    let outcome = child.collect(generous_limits());
    match outcome {
        CaptureOutcome::Exited {
            code,
            stdout,
            stderr,
        } => {
            assert_eq!(code, Some(0));
            assert_eq!(stdout, b"");
            assert_eq!(stderr, b"STDERR-ONLY-TEXT");
        }
        other => panic!("expected Exited, got {other:?}"),
    }
}

// ---------------------------------------------------------------------
// stderr past max_output_bytes is truncated and discarded rather than
// turning the outcome into OutputTooLarge: only stdout enforces that limit.
// stdout is fully returned and the child is not held up by a full stderr
// pipe.
// ---------------------------------------------------------------------

#[test]
fn stderr_over_the_output_limit_does_not_turn_the_outcome_into_output_too_large() {
    let argv = capture_child_argv(&["stderr-bytes", "8192", "stdout intact"]);
    let child = yagura_os::spawn_captured(&argv, None, CaptureEnv::Inherit(&[]), None)
        .expect("spawn should succeed");
    let timeout = Duration::from_secs(10);
    let limits = CaptureLimits {
        timeout,
        max_output_bytes: 1024,
    };
    let (outcome, _elapsed): (CaptureOutcome, Duration) =
        run_within(timeout + RETURN_SLACK, move || child.collect(limits));
    match outcome {
        CaptureOutcome::Exited {
            code,
            stdout,
            stderr,
        } => {
            assert_eq!(code, Some(0));
            assert_eq!(stdout, b"stdout intact");
            assert!(
                stderr.len() <= 1024,
                "stderr must be capped at max_output_bytes, got {} bytes",
                stderr.len()
            );
        }
        other => panic!("expected Exited, got {other:?}"),
    }
}

// ---------------------------------------------------------------------
// A timeout, and an output-size overrun, each end the whole descendant
// tree -- observed through a grandchild's heartbeat file, which must have
// been growing beforehand (the control) and must stop growing afterwards.
// ---------------------------------------------------------------------

#[tokio::test]
async fn a_timeout_returns_timed_out_and_ends_the_whole_descendant_tree() {
    let tmp = TempDir::new("yagura-one-shot-capture", "timeout");
    let tick_path = tmp.path().join("tick.log");

    let argv = capture_child_argv(&[
        "heartbeat",
        tick_path.to_str().expect("tick path must be valid UTF-8"),
        "hang",
    ]);
    let child = yagura_os::spawn_captured(&argv, None, CaptureEnv::Inherit(&[]), None)
        .expect("spawn should succeed");

    wait_until_file_grows(&tick_path, GROWTH_TIMEOUT).await;

    let outcome = child.collect(CaptureLimits {
        timeout: Duration::from_millis(500),
        max_output_bytes: 1 << 20,
    });
    assert!(
        matches!(outcome, CaptureOutcome::TimedOut),
        "expected TimedOut, got {outcome:?}"
    );

    assert_file_len_stable(&tick_path, OBSERVATION, "after a timeout").await;
}

// The direct child is itself gone when `collect` returns, not merely on its way
// out: ending its tree only starts the termination, and a caller that removes
// the child's files next must not find them still held.
#[test]
fn the_direct_child_has_ended_by_the_time_a_timed_out_collect_returns() {
    let tmp = TempDir::new("yagura-one-shot-capture", "timeout-reaps-child");
    let pid_path = tmp.path().join("child.pid");
    let argv = capture_child_argv(&[
        "pid-holder",
        pid_path.to_str().expect("pid path must be valid UTF-8"),
        "600000",
    ]);
    let child = spawn_inheriting(&argv);
    let pid = wait_for_published_pid(&pid_path);

    let outcome = child.collect(limits_with_timeout(Duration::from_millis(300)));

    assert!(
        matches!(outcome, CaptureOutcome::TimedOut),
        "expected TimedOut, got {outcome:?}"
    );
    assert!(
        process_ends_within(pid, Duration::ZERO),
        "the direct child (pid {pid}) was still running when collect returned"
    );
}

#[tokio::test]
async fn output_over_the_limit_returns_output_too_large_and_ends_the_whole_descendant_tree() {
    let tmp = TempDir::new("yagura-one-shot-capture", "output-too-large");
    let tick_path = tmp.path().join("tick.log");

    let argv = capture_child_argv(&[
        "heartbeat",
        tick_path.to_str().expect("tick path must be valid UTF-8"),
        "flood",
        "4096",
    ]);
    let child = yagura_os::spawn_captured(&argv, None, CaptureEnv::Inherit(&[]), None)
        .expect("spawn should succeed");

    wait_until_file_grows(&tick_path, GROWTH_TIMEOUT).await;

    let outcome = child.collect(CaptureLimits {
        timeout: Duration::from_secs(20),
        max_output_bytes: 1024,
    });
    assert!(
        matches!(outcome, CaptureOutcome::OutputTooLarge),
        "expected OutputTooLarge, got {outcome:?}"
    );

    assert_file_len_stable(&tick_path, OBSERVATION, "after output exceeded the limit").await;
}

// ---------------------------------------------------------------------
// Dropping a CapturedChild without ever calling collect() still ends
// the whole descendant tree.
// ---------------------------------------------------------------------

#[tokio::test]
async fn dropping_without_collecting_still_ends_the_whole_descendant_tree() {
    let tmp = TempDir::new("yagura-one-shot-capture", "drop-without-collect");
    let tick_path = tmp.path().join("tick.log");

    let argv = capture_child_argv(&[
        "heartbeat",
        tick_path.to_str().expect("tick path must be valid UTF-8"),
        "hang",
    ]);
    {
        let child = yagura_os::spawn_captured(&argv, None, CaptureEnv::Inherit(&[]), None)
            .expect("spawn should succeed");
        wait_until_file_grows(&tick_path, GROWTH_TIMEOUT).await;
        drop(child);
    }

    assert_file_len_stable(&tick_path, OBSERVATION, "after dropping without collecting").await;
}

// ---------------------------------------------------------------------
// A program that does not exist fails to spawn.
// ---------------------------------------------------------------------

#[test]
fn a_missing_program_fails_to_spawn() {
    // The error kind is not asserted: a PATH entry that cannot be searched
    // turns it into PermissionDenied (execvp's rule), and every caller treats
    // any spawn failure as "not installed" anyway.
    let argv = vec!["yagura-capture-child-does-not-exist-anywhere".to_string()];
    yagura_os::spawn_captured(&argv, None, CaptureEnv::Inherit(&[]), None)
        .expect_err("a missing program must fail to spawn");
}

// ---------------------------------------------------------------------
// (Windows only) a `.cmd` shim resolved through resolve_program /
// into_spawn_argv, spawned via spawn_captured, times out down to the
// fixture it starts and that fixture's own heartbeat grandchild.
// ---------------------------------------------------------------------

#[cfg(windows)]
#[tokio::test]
async fn a_cmd_shim_resolved_through_resolve_program_times_out_down_to_the_fixture_grandchild() {
    use yagura_os::program::{
        Platform, ProgramLookup, default_is_executable, into_spawn_argv, resolve_program,
    };

    let tmp = TempDir::new("yagura-one-shot-capture", "cmd-shim");
    let bin_dir = tmp.path().join("bin");
    std::fs::create_dir_all(&bin_dir).expect("bin dir should be creatable");
    let tick_path = tmp.path().join("tick.log");

    let shim_content = format!(
        "@echo off\r\n\"{}\" heartbeat \"{}\" hang\r\n",
        capture_child_path().display(),
        tick_path.display(),
    );
    std::fs::write(bin_dir.join("capshim.cmd"), shim_content)
        .expect("shim fixture should be writable");

    let lookup = ProgramLookup {
        path_dirs: vec![bin_dir],
        extra_bin_dirs: Vec::new(),
        windows_comspec: ProgramLookup::from_env().windows_comspec,
    };
    let argv = vec!["capshim".to_string()];
    let resolution = resolve_program(&argv, Platform::Windows, &lookup, &default_is_executable)
        .expect("resolution must not be rejected");
    let spawn_argv = into_spawn_argv(&resolution, &argv, Platform::Windows);

    let child = yagura_os::spawn_captured(&spawn_argv, None, CaptureEnv::Inherit(&[]), None)
        .expect("spawn should succeed");

    wait_until_file_grows(&tick_path, GROWTH_TIMEOUT).await;

    let outcome = child.collect(CaptureLimits {
        timeout: Duration::from_millis(500),
        max_output_bytes: 1 << 20,
    });
    assert!(
        matches!(outcome, CaptureOutcome::TimedOut),
        "expected TimedOut, got {outcome:?}"
    );

    assert_file_len_stable(
        &tick_path,
        OBSERVATION,
        "after a timeout through a .cmd shim",
    )
    .await;
}

// ---------------------------------------------------------------------
// The daemon's wrapper behaves like the direct path for a
// normal exit.
// ---------------------------------------------------------------------

#[tokio::test]
async fn spawn_one_shot_behaves_like_the_direct_path_for_a_normal_exit() {
    let argv = capture_child_argv(&["echo", "0", "via daemon lock"]);
    let child = yagura_daemon::pty::spawn_one_shot(
        argv,
        None,
        CaptureChildEnv::Based {
            base: ChildBaseEnv::default(),
            overlay: Vec::new(),
        },
        None,
    )
    .await
    .expect("spawn should succeed");
    let outcome = child.collect(generous_limits());
    match outcome {
        CaptureOutcome::Exited { code, stdout, .. } => {
            assert_eq!(code, Some(0));
            assert_eq!(stdout, b"via daemon lock");
        }
        other => panic!("expected Exited, got {other:?}"),
    }
}

// ---------------------------------------------------------------------
// Bounded returns and the end marker: a capture's own timeout bounds when
// it returns no matter what its descendants do with stdout, and a
// marker-driven read completes at the marker instead of waiting for every
// writer of the pipe to go away.
// ---------------------------------------------------------------------

/// Extra time, on top of a capture's own timeout, that tearing the tree down
/// and reaping the child may take before a return counts as late.
const RETURN_SLACK: Duration = Duration::from_secs(3);

/// How long a pipe-holding grandchild that a teardown is expected to kill may
/// live on its own: far beyond every timeout below, so only a teardown (or a
/// wait for end-of-file) can explain it going away within a case.
const HOLDER_LIFETIME: Duration = Duration::from_secs(60);

/// The end marker every marker-driven case looks for.
const END_MARKER: &str = "[capture-end-marker]";

fn limits_with_timeout(timeout: Duration) -> CaptureLimits {
    CaptureLimits {
        timeout,
        max_output_bytes: 1 << 20,
    }
}

fn spawn_inheriting(argv: &[String]) -> yagura_os::CapturedChild {
    yagura_os::spawn_captured(argv, None, CaptureEnv::Inherit(&[]), None)
        .expect("spawn should succeed")
}

/// Runs `run` on a detached thread and waits at most `outer` for it, so a
/// capture whose return is not bounded fails the case instead of hanging
/// the whole test binary. Returns the result and how long it took.
fn run_within<T: Send + 'static>(
    outer: Duration,
    run: impl FnOnce() -> T + Send + 'static,
) -> (T, Duration) {
    let (tx, rx) = std::sync::mpsc::channel::<T>();
    let started = Instant::now();
    std::thread::spawn(move || {
        let _ = tx.send(run());
    });
    match rx.recv_timeout(outer) {
        Ok(value) => (value, started.elapsed()),
        Err(_) => panic!(
            "the capture had not returned after {outer:?}: its return is not bounded by its own \
             timeout"
        ),
    }
}

/// `pipe-holder` argv: a `tick` grandchild targeting `tick_path` that holds
/// the capture pipe for `lifetime`, then `text` on stdout, then `behavior`
/// (`exit` or `hang`), with `launcher` in front of the grandchild when given.
fn pipe_holder_argv(
    tick_path: &std::path::Path,
    lifetime: Duration,
    text: &str,
    behavior: &str,
    launcher: &[&str],
) -> Vec<String> {
    let lifetime_ms = lifetime.as_millis().to_string();
    let mut mode: Vec<&str> = vec![
        "pipe-holder",
        tick_path.to_str().expect("tick path must be valid UTF-8"),
        &lifetime_ms,
        text,
        behavior,
    ];
    mode.extend_from_slice(launcher);
    capture_child_argv(&mode)
}

/// `pipe-holder-stderr` argv: the stderr-holding counterpart of
/// [`pipe_holder_argv`] -- the `tick` grandchild holds the capture's stderr
/// pipe instead of stdout, while the direct child writes `text` to stdout.
#[cfg(unix)]
fn pipe_holder_stderr_argv(
    tick_path: &std::path::Path,
    lifetime: Duration,
    text: &str,
    behavior: &str,
    launcher: &[&str],
) -> Vec<String> {
    let lifetime_ms = lifetime.as_millis().to_string();
    let mut mode: Vec<&str> = vec![
        "pipe-holder-stderr",
        tick_path.to_str().expect("tick path must be valid UTF-8"),
        &lifetime_ms,
        text,
        behavior,
    ];
    mode.extend_from_slice(launcher);
    capture_child_argv(&mode)
}

/// Starts the grandchild in a process group of its own: a non-interactive
/// bash with job control on puts every background job in a new group, the
/// way a daemon an rc file starts leaves the login shell's group.
#[cfg(unix)]
const OWN_GROUP_LAUNCHER: [&str; 3] = ["bash", "-c", "set -m; \"$0\" \"$@\" &"];

/// How long the grandchild outside the group lives. Nothing tears it down, so
/// it bounds how long it outlives its case; it must still outlast the case's
/// outer deadline and the survival check after it.
#[cfg(unix)]
const OUTSIDE_HOLDER_LIFETIME: Duration = Duration::from_secs(20);

#[cfg(unix)]
#[tokio::test]
async fn a_timeout_returns_within_its_bound_while_a_descendant_outside_the_group_holds_stdout() {
    let tmp = TempDir::new("yagura-one-shot-capture", "outside-holder");
    let tick_path = tmp.path().join("tick.log");
    let argv = pipe_holder_argv(
        &tick_path,
        OUTSIDE_HOLDER_LIFETIME,
        "outside holder started",
        "exit",
        &OWN_GROUP_LAUNCHER,
    );
    let child = spawn_inheriting(&argv);
    wait_until_file_grows(&tick_path, GROWTH_TIMEOUT).await;

    let timeout = Duration::from_secs(1);
    let (outcome, _elapsed): (CaptureOutcome, Duration) =
        run_within(timeout + RETURN_SLACK, move || {
            child.collect(limits_with_timeout(timeout))
        });
    assert!(
        matches!(outcome, CaptureOutcome::TimedOut),
        "expected TimedOut, got {outcome:?}"
    );

    // The case only means something if the holder really sat outside the
    // torn-down group: it must still be ticking after collect returned.
    wait_until_file_grows(&tick_path, GROWTH_TIMEOUT).await;
}

#[test]
fn a_timeout_bounded_run_with_no_descendant_holding_stdout_returns_exited() {
    let argv = capture_child_argv(&["echo", "0", "no holder"]);
    let child = spawn_inheriting(&argv);
    let timeout = Duration::from_secs(1);
    let (outcome, _elapsed): (CaptureOutcome, Duration) =
        run_within(timeout + RETURN_SLACK, move || {
            child.collect(limits_with_timeout(timeout))
        });
    match outcome {
        CaptureOutcome::Exited { code, stdout, .. } => {
            assert_eq!(code, Some(0));
            assert_eq!(stdout, b"no holder");
        }
        other => panic!("expected Exited, got {other:?}"),
    }
}

/// `capture_child` with `mode_args`, started with its stdout already closed:
/// the capture pipe reaches end of file while the direct child still runs.
#[cfg(unix)]
fn stdout_closed_argv(mode_args: &[&str]) -> Vec<String> {
    let mut argv: Vec<String> = ["sh", "-c", "exec >&-; exec \"$0\" \"$@\""]
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    argv.extend(capture_child_argv(mode_args));
    argv
}

#[cfg(unix)]
#[test]
fn collect_does_not_report_an_exit_for_a_child_that_closed_stdout_but_still_runs() {
    let child = spawn_inheriting(&stdout_closed_argv(&["sleep", "600000"]));
    let timeout = Duration::from_secs(1);
    let (outcome, _elapsed): (CaptureOutcome, Duration) =
        run_within(timeout + RETURN_SLACK, move || {
            child.collect(limits_with_timeout(timeout))
        });
    assert!(
        matches!(outcome, CaptureOutcome::TimedOut),
        "expected TimedOut, got {outcome:?}"
    );
}

#[cfg(unix)]
#[test]
fn collect_returns_the_own_exit_code_of_a_child_that_closed_stdout_before_exiting() {
    let tmp = TempDir::new("yagura-one-shot-capture", "closed-stdout-exit");
    let marker = tmp.path().join("started");
    let child = spawn_inheriting(&stdout_closed_argv(&[
        "touch",
        marker.to_str().expect("marker path must be valid UTF-8"),
        "exit",
        "7",
    ]));
    match child.collect(generous_limits()) {
        CaptureOutcome::Exited { code, stdout, .. } => {
            assert_eq!(code, Some(7));
            assert!(stdout.is_empty());
        }
        other => panic!("expected Exited, got {other:?}"),
    }
}

#[cfg(unix)]
#[test]
fn collect_until_does_not_report_an_exit_for_a_child_that_closed_stdout_but_still_runs() {
    let child = spawn_inheriting(&stdout_closed_argv(&["sleep", "600000"]));
    let timeout = Duration::from_secs(1);
    let (outcome, _elapsed): (MarkerOutcome, Duration) =
        run_within(timeout + RETURN_SLACK, move || {
            child.collect_until(limits_with_timeout(timeout), END_MARKER.as_bytes())
        });
    assert!(
        matches!(outcome, MarkerOutcome::TimedOut),
        "expected TimedOut, got {outcome:?}"
    );
}

#[test]
fn collect_until_completes_at_the_end_marker_while_a_descendant_in_the_group_holds_stdout() {
    let tmp = TempDir::new("yagura-one-shot-capture", "marker-in-group-holder");
    let text = format!("listing before the marker {END_MARKER}");
    let argv = pipe_holder_argv(
        &tmp.path().join("tick.log"),
        HOLDER_LIFETIME,
        &text,
        "exit",
        &[],
    );
    let child = spawn_inheriting(&argv);

    let timeout = Duration::from_secs(10);
    let (outcome, elapsed): (MarkerOutcome, Duration) =
        run_within(timeout + RETURN_SLACK, move || {
            child.collect_until(limits_with_timeout(timeout), END_MARKER.as_bytes())
        });
    match outcome {
        MarkerOutcome::Reached {
            stdout,
            child_exited,
            code,
            ..
        } => {
            let stdout_text = String::from_utf8_lossy(&stdout);
            assert!(
                stdout_text.contains(&text),
                "the output up to and including the marker must be kept: {stdout_text}"
            );
            assert!(child_exited, "the direct child had already exited");
            assert_eq!(code, Some(0));
        }
        other => panic!("expected Reached, got {other:?}"),
    }
    assert!(
        elapsed < timeout / 2,
        "collect_until returned after {elapsed:?}: it waited for the pipe to close instead of \
         completing at the marker"
    );
}

#[test]
fn collect_times_out_on_the_same_in_group_holder_that_collect_until_completes_on() {
    let tmp = TempDir::new("yagura-one-shot-capture", "collect-in-group-holder");
    let argv = pipe_holder_argv(
        &tmp.path().join("tick.log"),
        HOLDER_LIFETIME,
        &format!("listing before the marker {END_MARKER}"),
        "exit",
        &[],
    );
    let child = spawn_inheriting(&argv);

    let timeout = Duration::from_secs(1);
    let (outcome, _elapsed): (CaptureOutcome, Duration) =
        run_within(timeout + RETURN_SLACK, move || {
            child.collect(limits_with_timeout(timeout))
        });
    assert!(
        matches!(outcome, CaptureOutcome::TimedOut),
        "expected TimedOut while the holder keeps the pipe open, got {outcome:?}"
    );
}

#[test]
fn collect_until_waits_for_the_direct_child_to_finish_after_the_marker() {
    let tmp = TempDir::new("yagura-one-shot-capture", "marker-then-exit-work");
    let done_path = tmp.path().join("done.txt");
    let argv = capture_child_argv(&[
        "write-after",
        &format!("before {END_MARKER}"),
        "500",
        done_path.to_str().expect("done path must be valid UTF-8"),
    ]);
    let child = spawn_inheriting(&argv);

    let timeout = Duration::from_secs(10);
    let (outcome, _elapsed): (MarkerOutcome, Duration) =
        run_within(timeout + RETURN_SLACK, move || {
            child.collect_until(limits_with_timeout(timeout), END_MARKER.as_bytes())
        });
    // Checked before anything else: the child's own after-marker work must
    // have run to completion by the time collect_until hands back control.
    let done_exists = done_path.exists();
    match outcome {
        MarkerOutcome::Reached {
            child_exited, code, ..
        } => {
            assert!(
                child_exited,
                "the child finishes well inside the timeout and must be waited for"
            );
            assert_eq!(code, Some(0));
        }
        other => panic!("expected Reached, got {other:?}"),
    }
    assert!(
        done_exists,
        "the child was cut off before the work it does after the marker"
    );
}

#[tokio::test]
async fn collect_until_stops_waiting_for_a_child_that_outlives_the_timeout_and_ends_the_tree() {
    let tmp = TempDir::new("yagura-one-shot-capture", "marker-then-hang");
    let tick_path = tmp.path().join("tick.log");
    let text = format!("before {END_MARKER}");
    let argv = pipe_holder_argv(&tick_path, HOLDER_LIFETIME, &text, "hang", &[]);
    let child = spawn_inheriting(&argv);
    wait_until_file_grows(&tick_path, GROWTH_TIMEOUT).await;

    let timeout = Duration::from_secs(2);
    let (outcome, _elapsed): (MarkerOutcome, Duration) =
        run_within(timeout + RETURN_SLACK, move || {
            child.collect_until(limits_with_timeout(timeout), END_MARKER.as_bytes())
        });
    match outcome {
        MarkerOutcome::Reached {
            stdout,
            child_exited,
            ..
        } => {
            assert!(
                String::from_utf8_lossy(&stdout).contains(&text),
                "the output read before the timeout must still be returned"
            );
            assert!(
                !child_exited,
                "the child sleeps far beyond the timeout and cannot have exited"
            );
        }
        other => panic!("expected Reached, got {other:?}"),
    }
    assert_file_len_stable(
        &tick_path,
        OBSERVATION,
        "after collect_until gave up waiting for the child",
    )
    .await;
}

#[test]
fn output_written_after_the_end_marker_is_drained_so_the_child_exits_normally() {
    let tmp = TempDir::new("yagura-one-shot-capture", "marker-then-late-output");
    let done_path = tmp.path().join("done.txt");
    // Far more than a pipe buffer holds, so a reader that merely stops
    // reading blocks the child just as surely as one that closes the pipe
    // breaks its write.
    let argv = capture_child_argv(&[
        "write-after",
        END_MARKER,
        "200",
        done_path.to_str().expect("done path must be valid UTF-8"),
        "late output after the marker; ",
        "10000",
    ]);
    let child = spawn_inheriting(&argv);

    let timeout = Duration::from_secs(10);
    let (outcome, _elapsed): (MarkerOutcome, Duration) =
        run_within(timeout + RETURN_SLACK, move || {
            child.collect_until(limits_with_timeout(timeout), END_MARKER.as_bytes())
        });
    let done_exists = done_path.exists();
    match outcome {
        MarkerOutcome::Reached {
            child_exited, code, ..
        } => {
            assert!(
                child_exited,
                "a child writing after the marker must be able to finish"
            );
            assert_eq!(
                code,
                Some(0),
                "the child's write after the marker failed, as if the pipe had been closed"
            );
        }
        other => panic!("expected Reached, got {other:?}"),
    }
    assert!(
        done_exists,
        "the child never got past its write after the marker"
    );
}

#[test]
fn an_end_marker_split_across_two_writes_is_still_found() {
    let tmp = TempDir::new("yagura-one-shot-capture", "split-marker");
    let done_path = tmp.path().join("done.txt");
    let (first_half, second_half) = END_MARKER.split_at(END_MARKER.len() / 2);
    let argv = capture_child_argv(&[
        "write-after",
        &format!("split before {first_half}"),
        "300",
        done_path.to_str().expect("done path must be valid UTF-8"),
        second_half,
    ]);
    let child = spawn_inheriting(&argv);

    let timeout = Duration::from_secs(10);
    let (outcome, _elapsed): (MarkerOutcome, Duration) =
        run_within(timeout + RETURN_SLACK, move || {
            child.collect_until(limits_with_timeout(timeout), END_MARKER.as_bytes())
        });
    match outcome {
        MarkerOutcome::Reached { stdout, .. } => {
            let stdout_text = String::from_utf8_lossy(&stdout);
            assert!(
                stdout_text.contains(&format!("split before {END_MARKER}")),
                "the marker spanning two reads must be found and kept: {stdout_text}"
            );
        }
        other => panic!("expected Reached, got {other:?}"),
    }
}

#[test]
fn collect_until_reports_an_exit_without_the_marker_with_its_stdout_and_code() {
    let argv = capture_child_argv(&["echo", "3", "no marker in this output"]);
    let child = spawn_inheriting(&argv);
    let timeout = Duration::from_secs(10);
    let (outcome, _elapsed): (MarkerOutcome, Duration) =
        run_within(timeout + RETURN_SLACK, move || {
            child.collect_until(limits_with_timeout(timeout), END_MARKER.as_bytes())
        });
    match outcome {
        MarkerOutcome::Exited { code, stdout, .. } => {
            assert_eq!(code, Some(3));
            assert_eq!(stdout, b"no marker in this output");
        }
        other => panic!("expected Exited, got {other:?}"),
    }
}

#[test]
fn collect_until_reports_output_over_the_limit_before_any_marker() {
    let argv = capture_child_argv(&["bytes", "4096"]);
    let child = spawn_inheriting(&argv);
    let timeout = Duration::from_secs(10);
    let (outcome, _elapsed): (MarkerOutcome, Duration) =
        run_within(timeout + RETURN_SLACK, move || {
            child.collect_until(
                CaptureLimits {
                    timeout,
                    max_output_bytes: 1024,
                },
                END_MARKER.as_bytes(),
            )
        });
    assert!(
        matches!(outcome, MarkerOutcome::OutputTooLarge),
        "expected OutputTooLarge, got {outcome:?}"
    );
}

// The limit is the largest stdout accepted, so output of exactly that size is kept.
const LIMIT: usize = 1024;

fn limits_of_exactly(max_output_bytes: usize) -> CaptureLimits {
    CaptureLimits {
        timeout: GROWTH_TIMEOUT,
        max_output_bytes,
    }
}

#[test]
fn collect_keeps_stdout_of_exactly_the_output_limit() {
    let child = spawn_inheriting(&capture_child_argv(&["bytes", &LIMIT.to_string()]));
    match child.collect(limits_of_exactly(LIMIT)) {
        CaptureOutcome::Exited { code, stdout, .. } => {
            assert_eq!(code, Some(0));
            assert_eq!(stdout.len(), LIMIT);
        }
        other => panic!("expected Exited, got {other:?}"),
    }
}

#[test]
fn collect_until_keeps_stdout_of_exactly_the_output_limit() {
    let child = spawn_inheriting(&capture_child_argv(&["bytes", &LIMIT.to_string()]));
    match child.collect_until(limits_of_exactly(LIMIT), END_MARKER.as_bytes()) {
        MarkerOutcome::Exited { code, stdout, .. } => {
            assert_eq!(code, Some(0));
            assert_eq!(stdout.len(), LIMIT);
        }
        other => panic!("expected Exited, got {other:?}"),
    }
}

#[test]
fn collect_until_times_out_when_the_marker_never_comes() {
    let argv = capture_child_argv(&["sleep", "30000"]);
    let child = spawn_inheriting(&argv);
    let timeout = Duration::from_millis(500);
    let (outcome, _elapsed): (MarkerOutcome, Duration) =
        run_within(timeout + RETURN_SLACK, move || {
            child.collect_until(limits_with_timeout(timeout), END_MARKER.as_bytes())
        });
    assert!(
        matches!(outcome, MarkerOutcome::TimedOut),
        "expected TimedOut, got {outcome:?}"
    );
}

// ---------------------------------------------------------------------
// The environment a captured child starts with: replaced, or inherited with
// the given variables on top.
// ---------------------------------------------------------------------

/// A value the `Debug` renderings below must never show.
const SECRET_VALUE: &str = "capture-env-secret-sentinel-value";

fn os_pairs(pairs: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
    pairs
        .iter()
        .map(|(name, value)| (OsString::from(name), OsString::from(value)))
        .collect()
}

/// This test process's own `PATH`, which a child that inherits the process
/// environment reports back verbatim.
fn own_path_value() -> String {
    std::env::var_os("PATH")
        .expect("the test process must have a PATH to observe inheritance with")
        .to_string_lossy()
        .into_owned()
}

fn exited_stdout(outcome: CaptureOutcome) -> String {
    match outcome {
        CaptureOutcome::Exited { code, stdout, .. } => {
            assert_eq!(code, Some(0));
            String::from_utf8(stdout).expect("the env fixture writes UTF-8")
        }
        other => panic!("expected Exited, got {other:?}"),
    }
}

#[test]
fn replace_starts_the_child_from_only_the_given_variables() {
    // "not-set" below only shows a replaced environment when there was a
    // PATH to inherit in the first place.
    let _inherited_path = own_path_value();
    let vars: Vec<(OsString, OsString)> = os_pairs(&[("CAPTURE_REPLACE_ONLY_VAR", "replaced")]);
    let env: CaptureEnv<'_> = CaptureEnv::Replace(&vars);
    let argv = capture_child_argv(&["env", "PATH", "CAPTURE_REPLACE_ONLY_VAR"]);
    let child = yagura_os::spawn_captured(&argv, None, env, None).expect("spawn should succeed");
    assert_eq!(
        exited_stdout(child.collect(generous_limits())),
        "not-set\nreplaced\n",
        "a replaced environment must hold the given variables and nothing inherited"
    );
}

#[test]
fn inherit_keeps_the_process_environment_and_adds_the_given_variables() {
    let vars: Vec<(OsString, OsString)> = os_pairs(&[("CAPTURE_INHERIT_EXTRA_VAR", "added")]);
    let env: CaptureEnv<'_> = CaptureEnv::Inherit(&vars);
    let argv = capture_child_argv(&["env", "PATH", "CAPTURE_INHERIT_EXTRA_VAR"]);
    let child = yagura_os::spawn_captured(&argv, None, env, None).expect("spawn should succeed");
    assert_eq!(
        exited_stdout(child.collect(generous_limits())),
        format!("{}\nadded\n", own_path_value())
    );
}

#[test]
fn inherit_lets_a_later_given_variable_win_over_an_earlier_one_and_the_inherited_one() {
    // "not-set" below only shows a replaced environment when there was a
    // PATH to inherit in the first place.
    let _inherited_path = own_path_value();
    let vars: Vec<(OsString, OsString)> =
        os_pairs(&[("PATH", "first-given-path"), ("PATH", "second-given-path")]);
    let env: CaptureEnv<'_> = CaptureEnv::Inherit(&vars);
    let argv = capture_child_argv(&["env", "PATH"]);
    let child = yagura_os::spawn_captured(&argv, None, env, None).expect("spawn should succeed");
    assert_eq!(
        exited_stdout(child.collect(generous_limits())),
        "second-given-path\n"
    );
}

#[test]
fn capture_env_debug_never_shows_a_value() {
    let vars: Vec<(OsString, OsString)> = os_pairs(&[("CAPTURE_DEBUG_VAR", SECRET_VALUE)]);
    for (kind, env) in [
        ("Inherit", CaptureEnv::Inherit(&vars)),
        ("Replace", CaptureEnv::Replace(&vars)),
    ] {
        let debug_text = format!("{env:?}");
        assert!(
            !debug_text.contains(SECRET_VALUE),
            "a value leaked into CaptureEnv's Debug rendering: {debug_text}"
        );
        assert!(
            debug_text.contains(kind),
            "the Debug rendering must name the variant {kind}: {debug_text}"
        );
    }
}

#[tokio::test]
async fn spawn_one_shot_layers_the_overlay_over_the_base_over_the_inherited_env() {
    let base = ChildBaseEnv::from_pairs(os_pairs(&[
        ("CAPTURE_BASE_ONLY_VAR", "from-base"),
        ("CAPTURE_BASE_AND_OVERLAY_VAR", "from-base"),
    ]));
    let overlay: Vec<(OsString, OsString)> =
        os_pairs(&[("CAPTURE_BASE_AND_OVERLAY_VAR", "from-overlay")]);
    let argv = capture_child_argv(&[
        "env",
        "CAPTURE_BASE_ONLY_VAR",
        "CAPTURE_BASE_AND_OVERLAY_VAR",
        "PATH",
    ]);
    let child = yagura_daemon::pty::spawn_one_shot(
        argv,
        None,
        CaptureChildEnv::Based { base, overlay },
        None,
    )
    .await
    .expect("spawn should succeed");
    assert_eq!(
        exited_stdout(child.collect(generous_limits())),
        format!("from-base\nfrom-overlay\n{}\n", own_path_value())
    );
}

#[tokio::test]
async fn spawn_one_shot_replaced_env_inherits_nothing() {
    // "not-set" below only shows a replaced environment when there was a
    // PATH to inherit in the first place.
    let _inherited_path = own_path_value();
    let vars: Vec<(OsString, OsString)> = os_pairs(&[("CAPTURE_REPLACED_CHILD_VAR", "only")]);
    let argv = capture_child_argv(&["env", "PATH", "CAPTURE_REPLACED_CHILD_VAR"]);
    let child =
        yagura_daemon::pty::spawn_one_shot(argv, None, CaptureChildEnv::Replaced(vars), None)
            .await
            .expect("spawn should succeed");
    assert_eq!(
        exited_stdout(child.collect(generous_limits())),
        "not-set\nonly\n"
    );
}

#[test]
fn capture_child_env_debug_never_shows_a_value() {
    let based = CaptureChildEnv::Based {
        base: ChildBaseEnv::from_pairs(os_pairs(&[("CAPTURE_DEBUG_BASE_VAR", SECRET_VALUE)])),
        overlay: os_pairs(&[("CAPTURE_DEBUG_OVERLAY_VAR", SECRET_VALUE)]),
    };
    let replaced = CaptureChildEnv::Replaced(os_pairs(&[("CAPTURE_DEBUG_VAR", SECRET_VALUE)]));
    for env in [based, replaced] {
        let debug_text = format!("{env:?}");
        assert!(
            !debug_text.contains(SECRET_VALUE),
            "a value leaked into CaptureChildEnv's Debug rendering: {debug_text}"
        );
    }
}

// ---------------------------------------------------------------------
// stdin: bytes given to a captured child arrive in full, a child that
// ignores stdin is not held up by a large payload, and no payload means the
// child sees immediate end of file.
// ---------------------------------------------------------------------

#[test]
fn stdin_bytes_are_delivered_to_the_child_in_full_even_at_a_large_size() {
    let argv = capture_child_argv(&["cat-stdin"]);
    let payload = vec![b'y'; 1 << 20];
    let child =
        yagura_os::spawn_captured(&argv, None, CaptureEnv::Inherit(&[]), Some(payload.clone()))
            .expect("spawn should succeed");
    let timeout = Duration::from_secs(10);
    let (outcome, _elapsed): (CaptureOutcome, Duration) =
        run_within(timeout + RETURN_SLACK, move || {
            child.collect(limits_with_timeout(timeout))
        });
    match outcome {
        CaptureOutcome::Exited { code, stdout, .. } => {
            assert_eq!(code, Some(0));
            assert_eq!(stdout, payload);
        }
        other => panic!("expected Exited, got {other:?}"),
    }
}

#[test]
fn a_child_that_never_reads_stdin_is_not_held_up_by_a_large_stdin_payload() {
    let argv = capture_child_argv(&["echo", "0", "ignored stdin"]);
    let payload = vec![b'z'; 1 << 20];
    let child = yagura_os::spawn_captured(&argv, None, CaptureEnv::Inherit(&[]), Some(payload))
        .expect("spawn should succeed");
    let timeout = Duration::from_secs(5);
    let (outcome, _elapsed): (CaptureOutcome, Duration) =
        run_within(timeout + RETURN_SLACK, move || {
            child.collect(limits_with_timeout(timeout))
        });
    match outcome {
        CaptureOutcome::Exited { code, stdout, .. } => {
            assert_eq!(code, Some(0));
            assert_eq!(stdout, b"ignored stdin");
        }
        other => panic!("expected Exited, got {other:?}"),
    }
}

#[test]
fn no_stdin_given_means_the_child_sees_immediate_end_of_file() {
    let argv = capture_child_argv(&["cat-stdin"]);
    let child = yagura_os::spawn_captured(&argv, None, CaptureEnv::Inherit(&[]), None)
        .expect("spawn should succeed");
    let outcome = child.collect(generous_limits());
    match outcome {
        CaptureOutcome::Exited { code, stdout, .. } => {
            assert_eq!(code, Some(0));
            assert_eq!(stdout, b"");
        }
        other => panic!("expected Exited, got {other:?}"),
    }
}

// ---------------------------------------------------------------------
// stderr, mirroring the stdout-holder cases above: a descendant outside the
// process group holding stderr open bounds collect()'s return by the
// timeout instead of by the holder (unlike the stdout-holder case, which
// times out: stdout, not stderr, decides whether the outcome is Exited),
// and does not delay collect_until()'s return past the marker at all.
// ---------------------------------------------------------------------

#[cfg(unix)]
#[tokio::test]
async fn collect_returns_within_its_bound_while_a_descendant_outside_the_group_holds_stderr() {
    let tmp = TempDir::new("yagura-one-shot-capture", "stderr-outside-holder");
    let tick_path = tmp.path().join("tick.log");
    let argv = pipe_holder_stderr_argv(
        &tick_path,
        OUTSIDE_HOLDER_LIFETIME,
        "direct child stdout",
        "exit",
        &OWN_GROUP_LAUNCHER,
    );
    let child = spawn_inheriting(&argv);
    wait_until_file_grows(&tick_path, GROWTH_TIMEOUT).await;

    let timeout = Duration::from_secs(1);
    let (outcome, _elapsed): (CaptureOutcome, Duration) =
        run_within(timeout + RETURN_SLACK, move || {
            child.collect(limits_with_timeout(timeout))
        });
    match outcome {
        CaptureOutcome::Exited { stdout, .. } => {
            assert_eq!(stdout, b"direct child stdout");
        }
        other => panic!(
            "expected Exited (the direct child's own stdout closes right away; stderr, held \
             open by the grandchild, must not turn this into TimedOut), got {other:?}"
        ),
    }

    // The case only means something if the holder really sat outside the
    // torn-down group: it must still be ticking after collect returned.
    wait_until_file_grows(&tick_path, GROWTH_TIMEOUT).await;
}

#[cfg(unix)]
#[tokio::test]
async fn collect_until_completes_at_the_marker_while_a_descendant_outside_the_group_holds_stderr() {
    let tmp = TempDir::new("yagura-one-shot-capture", "stderr-marker-outside-holder");
    let tick_path = tmp.path().join("tick.log");
    let text = format!("listing before the marker {END_MARKER}");
    let argv = pipe_holder_stderr_argv(
        &tick_path,
        OUTSIDE_HOLDER_LIFETIME,
        &text,
        "exit",
        &OWN_GROUP_LAUNCHER,
    );
    let child = spawn_inheriting(&argv);
    wait_until_file_grows(&tick_path, GROWTH_TIMEOUT).await;

    let timeout = Duration::from_secs(10);
    let (outcome, elapsed): (MarkerOutcome, Duration) =
        run_within(timeout + RETURN_SLACK, move || {
            child.collect_until(limits_with_timeout(timeout), END_MARKER.as_bytes())
        });
    match outcome {
        MarkerOutcome::Reached { stdout, .. } => {
            let stdout_text = String::from_utf8_lossy(&stdout);
            assert!(
                stdout_text.contains(&text),
                "the output up to and including the marker must be kept: {stdout_text}"
            );
        }
        other => panic!("expected Reached, got {other:?}"),
    }
    assert!(
        elapsed < timeout / 2,
        "collect_until returned after {elapsed:?}: unlike collect, it must not wait on stderr \
         at all once the marker is found"
    );

    // The holder must still be outside the torn-down group.
    wait_until_file_grows(&tick_path, GROWTH_TIMEOUT).await;
}

// ---------------------------------------------------------------------
// spawn_one_shot (the daemon's wrapper) carries stderr and
// stdin the same way the direct path does.
// ---------------------------------------------------------------------

#[tokio::test]
async fn spawn_one_shot_captures_stderr_the_same_way_the_direct_path_does() {
    let argv = capture_child_argv(&["echo", "0", "", "stderr via daemon lock"]);
    let child = yagura_daemon::pty::spawn_one_shot(
        argv,
        None,
        CaptureChildEnv::Based {
            base: ChildBaseEnv::default(),
            overlay: Vec::new(),
        },
        None,
    )
    .await
    .expect("spawn should succeed");
    let outcome = child.collect(generous_limits());
    match outcome {
        CaptureOutcome::Exited {
            code,
            stdout,
            stderr,
        } => {
            assert_eq!(code, Some(0));
            assert_eq!(stdout, b"");
            assert_eq!(stderr, b"stderr via daemon lock");
        }
        other => panic!("expected Exited, got {other:?}"),
    }
}

#[tokio::test]
async fn spawn_one_shot_delivers_stdin_to_the_child() {
    let argv = capture_child_argv(&["cat-stdin"]);
    let payload = b"stdin payload via daemon lock".to_vec();
    let child = yagura_daemon::pty::spawn_one_shot(
        argv,
        None,
        CaptureChildEnv::Based {
            base: ChildBaseEnv::default(),
            overlay: Vec::new(),
        },
        Some(payload.clone()),
    )
    .await
    .expect("spawn should succeed");
    let outcome = child.collect(generous_limits());
    match outcome {
        CaptureOutcome::Exited { code, stdout, .. } => {
            assert_eq!(code, Some(0));
            assert_eq!(stdout, payload);
        }
        other => panic!("expected Exited, got {other:?}"),
    }
}

// ---------------------------------------------------------------------
// A grandchild the child starts as its very first action belongs to the
// capture's teardown unit, however quickly it is started: it is gone once
// `collect` has returned. The unit has to hold the child before the child
// runs, not be attached to it afterwards.
// ---------------------------------------------------------------------

/// Waits until `pid_path` holds a process id and returns it.
fn wait_for_published_pid(pid_path: &std::path::Path) -> u32 {
    let deadline = Instant::now() + GROWTH_TIMEOUT;
    loop {
        if let Some(pid) = std::fs::read_to_string(pid_path)
            .ok()
            .and_then(|text| text.trim().parse::<u32>().ok())
        {
            return pid;
        }
        assert!(
            Instant::now() < deadline,
            "the grandchild never published its process id at {}",
            pid_path.display()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Whether process `pid` has ended, waiting up to `bound` for it to.
#[cfg(windows)]
fn process_ends_within(pid: u32, bound: Duration) -> bool {
    use windows::Win32::Foundation::{CloseHandle, ERROR_INVALID_PARAMETER, WAIT_OBJECT_0};
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject,
    };
    // SAFETY: the handle is opened for waiting only, used on this thread, and
    // closed before returning.
    unsafe {
        match OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
            Err(error) if error.code() == ERROR_INVALID_PARAMETER.to_hresult() => true,
            Err(error) => panic!("OpenProcess({pid}) failed unexpectedly: {error}"),
            Ok(handle) => {
                let millis = u32::try_from(bound.as_millis()).unwrap_or(u32::MAX);
                let signaled = WaitForSingleObject(handle, millis) == WAIT_OBJECT_0;
                let _ = CloseHandle(handle);
                signaled
            }
        }
    }
}

/// Whether process `pid` has ended, waiting up to `bound` for it to.
#[cfg(unix)]
fn process_ends_within(pid: u32, bound: Duration) -> bool {
    let deadline = Instant::now() + bound;
    loop {
        let alive = std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stderr(std::process::Stdio::null())
            .status()
            .expect("kill should be runnable")
            .success();
        if !alive {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_grandchild_started_as_the_childs_first_action_is_gone_once_collect_returns() {
    let tmp = TempDir::new("yagura-one-shot-capture", "first-grandchild");
    let pid_path = tmp.path().join("grandchild.pid");
    let argv = capture_child_argv(&[
        "grandchild-first",
        pid_path.to_str().expect("pid path must be valid UTF-8"),
        "0",
    ]);
    let child = spawn_inheriting(&argv);
    match child.collect(generous_limits()) {
        CaptureOutcome::Exited { code, .. } => assert_eq!(code, Some(0)),
        other => panic!("expected Exited, got {other:?}"),
    }

    let grandchild = wait_for_published_pid(&pid_path);
    assert!(
        process_ends_within(grandchild, GROWTH_TIMEOUT),
        "the grandchild (pid {grandchild}) the child started first thing outlived collect: it \
         was started outside the capture's teardown unit"
    );
}

#[test]
fn the_first_grandchild_fixture_keeps_the_grandchild_alive_while_the_child_lives() {
    let tmp = TempDir::new("yagura-one-shot-capture", "first-grandchild-control");
    let pid_path = tmp.path().join("grandchild.pid");
    let argv = capture_child_argv(&[
        "grandchild-first",
        pid_path.to_str().expect("pid path must be valid UTF-8"),
        "600000",
    ]);
    let child = spawn_inheriting(&argv);

    let grandchild = wait_for_published_pid(&pid_path);
    let ended_while_child_lives = process_ends_within(grandchild, Duration::from_millis(500));
    drop(child);
    assert!(
        !ended_while_child_lives,
        "the fixture's grandchild (pid {grandchild}) must be running while its child is"
    );
    assert!(
        process_ends_within(grandchild, GROWTH_TIMEOUT),
        "dropping the capture must still end the grandchild (pid {grandchild})"
    );
}

// The fixture's contract: its grandchild has published its id before the
// fixture itself exits, so the id is readable at once, with no waiting.
#[test]
fn the_first_grandchild_fixture_has_the_grandchilds_id_published_before_it_exits() {
    let tmp = TempDir::new("yagura-one-shot-capture", "first-grandchild-published");
    let pid_path = tmp.path().join("grandchild.pid");
    let argv = capture_child_argv(&[
        "grandchild-first",
        pid_path.to_str().expect("pid path must be valid UTF-8"),
        "0",
    ]);
    match spawn_inheriting(&argv).collect(generous_limits()) {
        CaptureOutcome::Exited { code, .. } => assert_eq!(code, Some(0)),
        other => panic!("expected Exited, got {other:?}"),
    }

    let published = std::fs::read_to_string(&pid_path)
        .expect("the id must be published by the time the fixture exits");
    assert!(
        published.trim().parse::<u32>().is_ok(),
        "the published id must be whole, got {published:?}"
    );
}

// ---------------------------------------------------------------------
// (Windows only) a batch file is never handed to the OS as the program to
// run: `CreateProcess` would run it through `cmd.exe` implicitly, with
// argument metacharacters live. The resolver's wrapping is the only way in.
// ---------------------------------------------------------------------

#[cfg(windows)]
#[test]
fn a_batch_file_path_as_argv0_is_refused_and_never_started() {
    let tmp = TempDir::new("yagura-one-shot-capture", "batch-refused");
    for extension in ["cmd", "bat"] {
        let marker = tmp.path().join(format!("ran-{extension}.txt"));
        let batch = tmp.path().join(format!("shim.{extension}"));
        std::fs::write(
            &batch,
            format!("@echo off\r\necho ran> \"{}\"\r\n", marker.display()),
        )
        .expect("batch fixture should be writable");
        let argv = vec![
            batch
                .to_str()
                .expect("batch path must be valid UTF-8")
                .to_string(),
        ];

        let result = yagura_os::spawn_captured(&argv, None, CaptureEnv::Inherit(&[]), None);
        let started = result.is_ok();
        let kind = result.as_ref().err().map(std::io::Error::kind);
        if let Ok(child) = result {
            // Let an accepted batch finish, so it cannot outlive the case.
            let _ = child.collect(generous_limits());
        }
        assert!(
            !started,
            "a .{extension} path as argv[0] must be refused, not started"
        );
        assert_eq!(kind, Some(std::io::ErrorKind::InvalidInput));
        assert!(
            !marker.exists(),
            "the .{extension} file must never have run"
        );
    }
}

// ---------------------------------------------------------------------
// (Windows only) a captured child receives only the handles it is given: an
// inheritable handle the daemon happens to hold elsewhere is not carried into
// it. Observed through a pipe whose write end the daemon alone keeps: once
// the daemon drops it, its reader sees end of file unless the live child
// holds a copy.
// ---------------------------------------------------------------------

/// A pipe whose write end is inheritable. The read end is not.
#[cfg(windows)]
fn inheritable_write_pipe() -> (std::fs::File, std::fs::File) {
    use std::os::windows::io::{FromRawHandle, OwnedHandle};
    use windows::Win32::Foundation::{
        HANDLE, HANDLE_FLAG_INHERIT, HANDLE_FLAGS, SetHandleInformation,
    };
    use windows::Win32::Security::SECURITY_ATTRIBUTES;
    use windows::Win32::System::Pipes::CreatePipe;

    let attributes = SECURITY_ATTRIBUTES {
        nLength: u32::try_from(std::mem::size_of::<SECURITY_ATTRIBUTES>())
            .expect("SECURITY_ATTRIBUTES is small"),
        lpSecurityDescriptor: std::ptr::null_mut(),
        bInheritHandle: true.into(),
    };
    let mut read = HANDLE::default();
    let mut write = HANDLE::default();
    // SAFETY: both out-pointers are valid for the call; the returned handles
    // are owned by the `File`s below and closed exactly once.
    unsafe {
        CreatePipe(&mut read, &mut write, Some(&attributes), 0).expect("CreatePipe should succeed");
        SetHandleInformation(read, HANDLE_FLAG_INHERIT.0, HANDLE_FLAGS(0))
            .expect("the read end should become non-inheritable");
        (
            std::fs::File::from(OwnedHandle::from_raw_handle(read.0 as _)),
            std::fs::File::from(OwnedHandle::from_raw_handle(write.0 as _)),
        )
    }
}

#[cfg(windows)]
#[test]
fn a_captured_child_does_not_inherit_an_inheritable_handle_the_daemon_holds() {
    use std::io::Read;

    let (mut reader, writer) = inheritable_write_pipe();
    let argv = capture_child_argv(&["sleep", "60000"]);
    let child = spawn_inheriting(&argv);
    drop(writer);

    let (tx, rx) = std::sync::mpsc::channel::<std::io::Result<usize>>();
    std::thread::spawn(move || {
        let mut byte = [0u8; 1];
        let _ = tx.send(reader.read(&mut byte));
    });
    let seen = rx.recv_timeout(Duration::from_secs(5));
    // Ends the child, which also releases a copy of the write end if it held one.
    drop(child);

    match seen {
        Ok(Ok(0)) => {}
        Ok(Ok(n)) => panic!("read {n} unexpected bytes from a pipe nobody writes to"),
        // A broken pipe is how Windows reports end of file on a pipe read.
        Ok(Err(error)) if error.kind() == std::io::ErrorKind::BrokenPipe => {}
        Ok(Err(error)) => panic!("reading the pipe failed unexpectedly: {error}"),
        Err(_) => panic!(
            "the pipe's reader saw no end of file while the child lived: the child inherited the \
             daemon's write end"
        ),
    }
}

// ---------------------------------------------------------------------
// Arguments reach the child exactly as given, including the shapes
// command-line quoting gets wrong.
// ---------------------------------------------------------------------

#[test]
fn arguments_with_quoting_hazards_reach_the_child_unchanged() {
    let hazards = [
        "",
        "plain",
        "has space",
        "ends with backslash\\",
        "C:\\dir with space\\",
        "has \"quote\" inside",
        "backslash before quote \\\" end",
    ];
    let mut argv = vec![
        fixture_path("argv_probe")
            .to_str()
            .expect("argv_probe path must be valid UTF-8")
            .to_string(),
    ];
    argv.extend(hazards.iter().map(|h| (*h).to_string()));
    let child = spawn_inheriting(&argv);
    let stdout = match child.collect(generous_limits()) {
        CaptureOutcome::Exited { code, stdout, .. } => {
            assert_eq!(code, Some(0));
            String::from_utf8(stdout).expect("argv_probe writes UTF-8")
        }
        other => panic!("expected Exited, got {other:?}"),
    };
    let expected: String = hazards
        .iter()
        .enumerate()
        .map(|(index, value)| format!("<<argv[{}]={value}>>\n", index + 1))
        .collect();
    assert_eq!(stdout, expected);
}

// ---------------------------------------------------------------------
// One-shot captures started at the same time do not hold each other's
// pipes: short captures return `Exited` while long-lived ones are alive.
// ---------------------------------------------------------------------

#[test]
fn concurrent_one_shots_do_not_hold_each_others_pipes() {
    const LONG_LIVED: usize = 4;
    const SHORT_LIVED: usize = 4;
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(LONG_LIVED + SHORT_LIVED));

    let hangers: Vec<_> = (0..LONG_LIVED)
        .map(|_| {
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                spawn_inheriting(&capture_child_argv(&["sleep", "60000"]))
            })
        })
        .collect();
    let shorts: Vec<_> = (0..SHORT_LIVED)
        .map(|i| {
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                let text = format!("short-{i}");
                let child = spawn_inheriting(&capture_child_argv(&["echo", "0", &text]));
                let (outcome, _) = run_within(RETURN_SLACK, move || {
                    child.collect(limits_with_timeout(GROWTH_TIMEOUT))
                });
                (text, outcome)
            })
        })
        .collect();

    for short in shorts {
        let (text, outcome) = short.join().expect("short one-shot thread must not panic");
        match outcome {
            CaptureOutcome::Exited { code, stdout, .. } => {
                assert_eq!(code, Some(0));
                assert_eq!(stdout, text.as_bytes());
            }
            other => panic!("expected Exited while long-lived captures run, got {other:?}"),
        }
    }

    for hanger in hangers {
        let child = hanger
            .join()
            .expect("long-lived spawn thread must not panic");
        let outcome = child.collect(limits_with_timeout(Duration::from_millis(300)));
        assert!(
            matches!(outcome, CaptureOutcome::TimedOut),
            "the long-lived capture must have stayed alive until its own timeout, got {outcome:?}"
        );
    }
}
