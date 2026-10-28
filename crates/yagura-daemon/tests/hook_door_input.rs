#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// Starts the compiled `yagura-daemon` binary as a real child process, the way a CLI runs
// its hook: what the CLI sees is whether its write of the hook input succeeded, the
// hook's output and its exit code.
#![allow(clippy::disallowed_methods)]

//! Integration test: the hook doors (`--emit-status`, `--deliver-context`,
//! `--deliver-take`, `--deliver-wait`, `--report-session`) and the input a CLI writes to
//! them on stdin.
//!
//! A registered hook also runs outside any window and inside sessions that are not the
//! window's own (a subagent), and a copy of the binary older than the hook file may not
//! know every argument in it. Every spawn sets `YAGURA_INSTANCE` to an identifier of this
//! test's own and points the data, state and config base directories into a temporary
//! directory, so it never reaches a running daemon of the user.

mod common;

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

use yagura_daemon::inbox::Parcel;
use yagura_test_support::paths::TempDir;

use common::{Harness, VALID_TOKEN, approver_base_config, detection_config, unique_temp_dir};

const WINDOW_ID_VAR: &str = "YAGURA_WINDOW_ID";
const STATUS_TOKEN_VAR: &str = "YAGURA_STATUS_TOKEN";
const SKIP_IF_SUBAGENT: [&str; 2] = ["--skip-if", "/subagentType"];
const LARGE_INPUT_BYTES: usize = 1024 * 1024;

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn daemon_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_yagura-daemon"))
}

fn unique_instance() -> String {
    format!(
        "door{:x}{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

/// Every door as a hook file names it, with the arguments it takes.
fn every_door() -> Vec<Vec<&'static str>> {
    vec![
        vec!["--emit-status", "running", "--source", "hook"],
        vec!["--deliver-context"],
        vec!["--deliver-take"],
        vec!["--deliver-wait"],
        vec!["--report-session", "/session_id"],
    ]
}

fn door_command(
    args: &[&str],
    instance: &str,
    bases: &TempDir,
    window: Option<(&str, &str)>,
) -> Command {
    let mut command = Command::new(daemon_bin());
    command
        .args(args)
        .env("YAGURA_INSTANCE", instance)
        .env_remove("YAGURA_STORE_DB")
        .env_remove(WINDOW_ID_VAR)
        .env_remove(STATUS_TOKEN_VAR)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if cfg!(windows) {
        command
            .env("LOCALAPPDATA", bases.make_dir("local"))
            .env("APPDATA", bases.make_dir("roaming"));
    } else {
        command
            .env("XDG_DATA_HOME", bases.make_dir("data"))
            .env("XDG_STATE_HOME", bases.make_dir("state"))
            .env("XDG_CONFIG_HOME", bases.make_dir("config"))
            .env("HOME", bases.make_dir("home"));
    }
    if let Some((window_id, token)) = window {
        command
            .env(WINDOW_ID_VAR, window_id)
            .env(STATUS_TOKEN_VAR, token);
    }
    command
}

/// Runs `command` with `input` written on its stdin from a thread of its own, as a CLI
/// writes the hook input; the returned result is that write's.
fn run_with_input(mut command: Command, input: Vec<u8>) -> (Output, std::io::Result<()>) {
    let mut child = command
        .stdin(Stdio::piped())
        .spawn()
        .expect("spawn the yagura-daemon binary");
    let mut pipe = child.stdin.take().expect("stdin");
    let writer = std::thread::spawn(move || pipe.write_all(&input).and_then(|()| pipe.flush()));
    let output = child.wait_with_output().expect("wait for the child");
    let written = writer.join().expect("writer thread");
    (output, written)
}

/// A JSON object of about [`LARGE_INPUT_BYTES`] with no `subagentType`.
fn large_input() -> Vec<u8> {
    format!(r#"{{"pad":"{}"}}"#, "x".repeat(LARGE_INPUT_BYTES)).into_bytes()
}

fn describe(output: &Output) -> String {
    format!(
        "exit {:?}; stdout: {:?}; stderr: {:?}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn assert_silent_success(output: &Output, situation: &str) {
    assert!(
        output.status.success() && output.stdout.is_empty() && output.stderr.is_empty(),
        "{situation}: the hook must print nothing and exit 0; {}",
        describe(output)
    );
}

// --- outside a window ---------------------------------------------------------

#[test]
fn a_door_outside_a_window_ignores_an_argument_this_copy_does_not_know() {
    let instance = unique_instance();
    let bases = TempDir::new("yagura-door", "bases");
    for door in every_door() {
        let mut args = door.clone();
        args.extend(["--an-argument-from-a-newer-copy", "/x"]);
        let output = door_command(&args, &instance, &bases, None)
            .stdin(Stdio::null())
            .output()
            .expect("spawn");
        assert_silent_success(&output, &format!("{args:?} outside a window"));
    }
}

#[test]
fn a_door_outside_a_window_reads_all_of_a_large_input() {
    let instance = unique_instance();
    let bases = TempDir::new("yagura-door", "bases");
    let mut doors = every_door();
    doors.push(vec![
        "--deliver-take",
        SKIP_IF_SUBAGENT[0],
        SKIP_IF_SUBAGENT[1],
    ]);
    for args in doors {
        let (output, written) =
            run_with_input(door_command(&args, &instance, &bases, None), large_input());
        assert!(
            written.is_ok(),
            "{args:?} outside a window: the CLI's write of the hook input must succeed, got {written:?}"
        );
        assert_silent_success(&output, &format!("{args:?} outside a window"));
    }
}

// --- inside a window ------------------------------------------------------------

struct Setup {
    h: Harness,
    instance: String,
    bases: TempDir,
    window_id: String,
}

impl Setup {
    async fn start() -> Self {
        let instance = unique_instance();
        let config = detection_config(&unique_temp_dir("hook-door"), approver_base_config());
        let h = Harness::start_on_instance(&instance, config).await;
        // An Unknown window is never pushed to, so the doors are measured on a window
        // whose transition to idle has been observed.
        let mut client = h.connect().await;
        let window_id = h.open_idle_window(&mut client).await;
        Self {
            h,
            instance,
            bases: TempDir::new("yagura-door", "bases"),
            window_id,
        }
    }

    fn entry(&self) -> std::sync::Arc<yagura_daemon::WindowEntry> {
        self.h.registry.get(&self.window_id).expect("window exists")
    }

    async fn run(&self, args: &[&str], input: Vec<u8>) -> (Output, std::io::Result<()>) {
        let command = door_command(
            args,
            &self.instance,
            &self.bases,
            Some((&self.window_id, VALID_TOKEN)),
        );
        tokio::task::spawn_blocking(move || run_with_input(command, input))
            .await
            .expect("join")
    }
}

#[tokio::test]
async fn a_door_inside_a_window_reads_all_of_a_large_input() {
    let setup = Setup::start().await;
    for args in [
        &["--emit-status", "running", "--source", "hook"][..],
        &["--deliver-take"],
    ] {
        let (output, written) = setup.run(args, large_input()).await;
        assert!(
            written.is_ok(),
            "{args:?} inside a window: the CLI's write of the hook input must succeed, got {written:?}"
        );
        assert_eq!(
            output.status.code(),
            Some(0),
            "{args:?} inside a window: {}",
            describe(&output)
        );
    }
}

#[tokio::test]
async fn a_take_whose_input_has_the_skip_pointer_leaves_the_parcels_for_the_next_take() {
    let setup = Setup::start().await;
    setup
        .entry()
        .inbox()
        .deposit(Parcel::new("a", "PARCEL-FOR-THE-MAIN-SESSION"));
    let mut take = vec!["--deliver-take"];
    take.extend(SKIP_IF_SUBAGENT);

    let (skipped, written) = setup
        .run(&take, br#"{"subagentType":"explore"}"#.to_vec())
        .await;
    assert!(written.is_ok(), "{written:?}");
    assert_silent_success(&skipped, "a take from a subagent's hook");
    assert_eq!(
        setup.entry().inbox().pending_count(),
        1,
        "a take from a subagent's hook must leave the parcel in the inbox"
    );

    let (taken, written) = setup.run(&take, br#"{"session_id":"s-1"}"#.to_vec()).await;
    assert!(written.is_ok(), "{written:?}");
    assert_eq!(taken.status.code(), Some(0), "{}", describe(&taken));
    assert!(
        String::from_utf8_lossy(&taken.stdout).contains("PARCEL-FOR-THE-MAIN-SESSION"),
        "a take whose input has no value at the pointer must print the parcel; {}",
        describe(&taken)
    );
    assert_eq!(setup.entry().inbox().pending_count(), 0);
}

/// Grok Build writes `subagentType` after a tool input of up to 128 KiB.
#[tokio::test]
async fn a_take_finds_the_skip_pointer_after_a_large_tool_input() {
    let setup = Setup::start().await;
    setup
        .entry()
        .inbox()
        .deposit(Parcel::new("a", "PARCEL-FOR-THE-MAIN-SESSION"));
    let mut take = vec!["--deliver-take"];
    take.extend(SKIP_IF_SUBAGENT);
    let input = format!(
        r#"{{"toolInput":"{}","subagentType":"explore"}}"#,
        "x".repeat(128 * 1024)
    );

    let (skipped, written) = setup.run(&take, input.into_bytes()).await;
    assert!(written.is_ok(), "{written:?}");
    assert_silent_success(&skipped, "a take from a subagent's hook");
    assert_eq!(
        setup.entry().inbox().pending_count(),
        1,
        "a take from a subagent's hook must leave the parcel in the inbox"
    );
}

#[tokio::test]
async fn a_session_report_finds_the_session_after_a_large_field() {
    let setup = Setup::start().await;
    let input = format!(
        r#"{{"pad":"{}","session_id":"late-2"}}"#,
        "p".repeat(128 * 1024)
    );

    let (output, written) = setup
        .run(&["--report-session", "/session_id"], input.into_bytes())
        .await;
    assert!(written.is_ok(), "{written:?}");
    assert_eq!(output.status.code(), Some(0), "{}", describe(&output));
    assert_eq!(setup.entry().session_id().as_deref(), Some("late-2"));
}

/// A status report has no answer to wait for, so whether the door sent one is measured
/// where sending fails: inside a window whose daemon is not listening, a report that is
/// sent ends with an error and one that is skipped ends silently with 0.
#[test]
fn a_status_whose_input_has_the_skip_pointer_sends_nothing_to_the_daemon() {
    let instance = unique_instance();
    let bases = TempDir::new("yagura-door", "bases");
    let mut args = vec!["--emit-status", "waiting_input", "--source", "hook"];
    args.extend(SKIP_IF_SUBAGENT);
    let in_window = Some(("w-1", VALID_TOKEN));

    let (skipped, written) = run_with_input(
        door_command(&args, &instance, &bases, in_window),
        br#"{"subagentType":"explore"}"#.to_vec(),
    );
    assert!(written.is_ok(), "{written:?}");
    assert_silent_success(&skipped, "a status from a subagent's hook");

    let (sent, written) = run_with_input(
        door_command(&args, &instance, &bases, in_window),
        br#"{"session_id":"s-1"}"#.to_vec(),
    );
    assert!(written.is_ok(), "{written:?}");
    let code = sent.status.code();
    assert!(
        code.is_some_and(|code| code != 0 && code != 2),
        "a status whose input has no value at the pointer must be sent, and fail with no \
         daemon listening; {}",
        describe(&sent)
    );
}
