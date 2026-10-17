#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// Starts the compiled `yagura-daemon` binary as a real child process against a daemon
// served in this process: what a CLI's hook sees is its stdout, its stderr and its exit
// code.
#![allow(clippy::disallowed_methods)]

//! Integration test: the `yagura-daemon` commands a CLI's hook runs to read a window's
//! inbox (`--deliver-context`, `--deliver-take`, `--deliver-wait`) and to tell the daemon
//! the CLI's session (`--report-session <pointer>`).
//!
//! Every spawn sets `YAGURA_INSTANCE` to an identifier of this test's own, so the endpoint
//! it connects to is the in-process daemon's and never a running daemon of the user, and
//! points the data, state and config base directories into a temporary directory.

mod common;

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use yagura_daemon::DaemonConfig;
use yagura_daemon::inbox::Parcel;
use yagura_test_support::paths::TempDir;

use common::{
    Harness, VALID_TOKEN, approver_base_config, detection_config, echo_child_path, unique_temp_dir,
};

const WINDOW_ID_VAR: &str = "YAGURA_WINDOW_ID";
const STATUS_TOKEN_VAR: &str = "YAGURA_STATUS_TOKEN";

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn daemon_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_yagura-daemon"))
}

struct Setup {
    h: Harness,
    instance: String,
    bases: TempDir,
}

impl Setup {
    async fn start(tweak: impl FnOnce(&mut DaemonConfig)) -> Self {
        let instance = format!(
            "dlv{:x}{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        );
        let mut config = detection_config(&unique_temp_dir("deliver-cli"), approver_base_config());
        tweak(&mut config);
        let h = Harness::start_on_instance(&instance, config).await;
        Self {
            h,
            instance,
            bases: TempDir::new("yagura-deliver", "bases"),
        }
    }

    // An Unknown window is never pushed to, so the doors are measured on a
    // window whose transition to idle has been observed.
    async fn window(&self) -> String {
        let mut client = self.h.connect().await;
        self.h.open_idle_window(&mut client).await
    }

    fn command(&self, args: &[&str], window_id: Option<&str>, token: Option<&str>) -> Command {
        let mut command = Command::new(daemon_bin());
        command
            .args(args)
            .env("YAGURA_INSTANCE", &self.instance)
            .env("YAGURA_LOG", "trace")
            .env_remove("YAGURA_STORE_DB")
            .env_remove(WINDOW_ID_VAR)
            .env_remove(STATUS_TOKEN_VAR);
        if cfg!(windows) {
            command
                .env("LOCALAPPDATA", self.bases.make_dir("local"))
                .env("APPDATA", self.bases.make_dir("roaming"));
        } else {
            command
                .env("XDG_DATA_HOME", self.bases.make_dir("data"))
                .env("XDG_STATE_HOME", self.bases.make_dir("state"))
                .env("XDG_CONFIG_HOME", self.bases.make_dir("config"))
                .env("HOME", self.bases.make_dir("home"));
        }
        if let Some(window_id) = window_id {
            command.env(WINDOW_ID_VAR, window_id);
        }
        if let Some(token) = token {
            command.env(STATUS_TOKEN_VAR, token);
        }
        command
    }

    fn spawn(&self, args: &[&str], window_id: &str, token: &str) -> Child {
        self.command(args, Some(window_id), Some(token))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn the yagura-daemon binary")
    }

    async fn run(&self, args: &[&str], window_id: &str, token: &str) -> Output {
        finish(self.spawn(args, window_id, token)).await
    }

    fn entry(&self, window_id: &str) -> std::sync::Arc<yagura_daemon::WindowEntry> {
        self.h.registry.get(window_id).expect("window exists")
    }

    fn deposit(&self, window_id: &str, key: &str, text: &str) {
        self.entry(window_id)
            .inbox()
            .deposit(Parcel::new(key, text));
    }

    async fn until(&self, what: &str, mut condition: impl FnMut() -> bool) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        while !condition() {
            assert!(
                tokio::time::Instant::now() < deadline,
                "timed out waiting for {what}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

async fn finish(child: Child) -> Output {
    tokio::task::spawn_blocking(move || child.wait_with_output().expect("wait for the child"))
        .await
        .expect("join")
}

fn out(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn err(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

// --- I5: outside a window -----------------------------------------------------

#[tokio::test]
async fn every_inbox_command_is_silent_and_succeeds_outside_a_window() {
    let setup = Setup::start(|_| {}).await;
    let token = "00112233445566778899aabbccddeeff";
    for args in [
        &["--deliver-context"][..],
        &["--deliver-take"],
        &["--deliver-wait"],
        &["--report-session", "/session_id"],
    ] {
        for (window_id, token) in [(None, None), (Some("w-1"), None), (None, Some(token))] {
            let output = setup
                .command(args, window_id, token)
                .stdin(Stdio::null())
                .output()
                .expect("spawn");
            assert!(
                output.status.success(),
                "{args:?} {window_id:?}: {:?}",
                output.status
            );
            assert!(
                output.stdout.is_empty() && output.stderr.is_empty(),
                "{args:?} must print nothing: {} / {}",
                out(&output),
                err(&output)
            );
        }
    }
}

// --- I1 / I3 / I4 through the process ------------------------------------------

#[tokio::test]
async fn take_prints_the_parcels_on_stdout_and_exits_zero() {
    let setup = Setup::start(|_| {}).await;
    let id = setup.window().await;
    setup.deposit(&id, "a", "PARCEL-ALPHA");
    let output = setup.run(&["--deliver-take"], &id, VALID_TOKEN).await;
    assert_eq!(output.status.code(), Some(0), "{}", err(&output));
    assert!(out(&output).contains("PARCEL-ALPHA"));
    assert!(output.stderr.is_empty());
    assert_eq!(setup.entry(&id).inbox().pending_count(), 0);

    let again = setup.run(&["--deliver-take"], &id, VALID_TOKEN).await;
    assert_eq!(again.status.code(), Some(0));
    assert!(again.stdout.is_empty() && again.stderr.is_empty());
}

#[tokio::test]
async fn context_prints_the_intro_and_the_pending_parcels() {
    let setup = Setup::start(|config| config.chat_intro = "WELCOME-INTRO".into()).await;
    let id = setup.window().await;
    setup.deposit(&id, "a", "PARCEL-ALPHA");
    let output = setup.run(&["--deliver-context"], &id, VALID_TOKEN).await;
    assert_eq!(output.status.code(), Some(0), "{}", err(&output));
    let text = out(&output);
    assert!(text.contains("WELCOME-INTRO") && text.contains("PARCEL-ALPHA"));
    assert_eq!(setup.entry(&id).inbox().pending_count(), 0);
}

#[tokio::test]
async fn a_wrong_token_prints_no_parcel_and_is_never_the_wake_exit_code() {
    let setup = Setup::start(|_| {}).await;
    let id = setup.window().await;
    setup.deposit(&id, "a", "PARCEL-ALPHA");
    for args in [&["--deliver-context"][..], &["--deliver-take"]] {
        let output = setup
            .run(args, &id, "ffffffffffffffffffffffffffffffff")
            .await;
        assert!(!out(&output).contains("PARCEL-ALPHA"));
        assert!(!err(&output).contains("PARCEL-ALPHA"));
        assert_ne!(output.status.code(), Some(2), "2 means wake in a hook");
    }
    assert_eq!(setup.entry(&id).inbox().pending_count(), 1);
}

// --- I6 / I7 through the process ------------------------------------------------

#[tokio::test]
async fn wait_ends_with_the_declared_stream_and_exit_code_when_a_parcel_arrives() {
    let setup = Setup::start(|_| {}).await;
    let id = setup.window().await;
    let child = setup.spawn(&["--deliver-wait"], &id, VALID_TOKEN);
    setup
        .until("the wait to be registered", || {
            setup.entry(&id).inbox().has_waiter()
        })
        .await;
    setup.deposit(&id, "a", "PARCEL-ALPHA");
    let output = finish(child).await;
    assert_eq!(output.status.code(), Some(2));
    assert!(err(&output).contains("PARCEL-ALPHA"), "{}", err(&output));
    assert!(
        output.stdout.is_empty(),
        "the text goes to the declared stream only"
    );
}

#[tokio::test]
async fn a_second_wait_ends_the_first_silently_with_zero() {
    let setup = Setup::start(|_| {}).await;
    let id = setup.window().await;
    let first = setup.spawn(&["--deliver-wait"], &id, VALID_TOKEN);
    setup
        .until("the first wait", || setup.entry(&id).inbox().has_waiter())
        .await;
    let second = setup.spawn(&["--deliver-wait"], &id, VALID_TOKEN);

    let first = finish(first).await;
    assert_eq!(first.status.code(), Some(0));
    assert!(first.stdout.is_empty() && first.stderr.is_empty());
    assert!(setup.entry(&id).inbox().has_waiter(), "the second stays");

    setup.deposit(&id, "a", "PARCEL-ALPHA");
    let second = finish(second).await;
    assert_eq!(second.status.code(), Some(2));
    assert!(err(&second).contains("PARCEL-ALPHA"));
}

#[tokio::test]
async fn a_wait_killed_by_the_cli_is_dropped_by_the_daemon() {
    let setup = Setup::start(|_| {}).await;
    let id = setup.window().await;
    let mut child = setup.spawn(&["--deliver-wait"], &id, VALID_TOKEN);
    setup
        .until("the wait to be registered", || {
            setup.entry(&id).inbox().has_waiter()
        })
        .await;
    child.kill().expect("kill the wait");
    let _ = finish(child).await;
    setup
        .until("the killed wait to be dropped", || {
            !setup.entry(&id).inbox().has_waiter()
        })
        .await;
}

#[tokio::test]
async fn a_wait_in_an_unknown_window_prints_nothing_while_the_window_cannot_be_pushed_to() {
    let dir = unique_temp_dir("deliver-cli-unknown");
    let setup = Setup::start({
        let dir = dir.clone();
        move |config| *config = detection_config(&dir, approver_base_config())
    })
    .await;
    // A definition is loaded but nothing matched: the window is Unknown.
    let id = setup
        .h
        .open(
            vec![echo_child_path().to_str().unwrap().into()],
            Some(common::CONTROL_DETECTION_NAME),
        )
        .await;
    setup.deposit(&id, "a", "PARCEL-ALPHA");
    let mut child = setup.spawn(&["--deliver-wait"], &id, VALID_TOKEN);
    setup
        .until("the wait to be registered", || {
            setup.entry(&id).inbox().has_waiter()
        })
        .await;
    assert_eq!(setup.entry(&id).inbox().pending_count(), 1);
    assert!(child.try_wait().unwrap().is_none(), "still waiting");
    child.kill().expect("kill the wait");
    let output = finish(child).await;
    assert!(!out(&output).contains("PARCEL-ALPHA"));
    assert!(!err(&output).contains("PARCEL-ALPHA"));
}

// --- I8 through the process ------------------------------------------------------

/// Runs `--report-session pointer` with `stdin` written from a thread of its own, so a
/// child that stopped reading shows up as a failed write.
async fn report(setup: &Setup, id: &str, pointer: &str, stdin: Vec<u8>) -> Output {
    let mut child = setup
        .command(&["--report-session", pointer], Some(id), Some(VALID_TOKEN))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");
    let mut pipe = child.stdin.take().expect("stdin");
    let writer = std::thread::spawn(move || pipe.write_all(&stdin).and_then(|()| pipe.flush()));
    let output = finish(child).await;
    writer
        .join()
        .expect("writer thread")
        .expect("the hook's CLI must be able to write all of its stdin");
    output
}

#[tokio::test]
async fn report_session_keeps_the_string_at_the_pointer() {
    let setup = Setup::start(|_| {}).await;
    let id = setup.window().await;
    let output = report(
        &setup,
        &id,
        "/session_id",
        br#"{"session_id":"abc-123","x":1}"#.to_vec(),
    )
    .await;
    assert_eq!(output.status.code(), Some(0), "{}", err(&output));
    assert!(output.stdout.is_empty() && output.stderr.is_empty());
    assert_eq!(setup.entry(&id).session_id().as_deref(), Some("abc-123"));

    let output = report(&setup, &id, "/a/b", br#"{"a":{"b":"nested.7"}}"#.to_vec()).await;
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(setup.entry(&id).session_id().as_deref(), Some("nested.7"));
}

#[tokio::test]
async fn report_session_keeps_nothing_for_input_without_a_usable_value() {
    let setup = Setup::start(|_| {}).await;
    let id = setup.window().await;
    let too_long = format!(r#"{{"session_id":"{}"}}"#, "a".repeat(129));
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("not JSON", b"this is not json".to_vec()),
        ("empty", Vec::new()),
        ("no value at the pointer", br#"{"other":"abc"}"#.to_vec()),
        ("a number", br#"{"session_id":12345}"#.to_vec()),
        ("a space", br#"{"session_id":"has space"}"#.to_vec()),
        ("leading dash", br#"{"session_id":"-rf"}"#.to_vec()),
        ("a slash", br#"{"session_id":"a/b"}"#.to_vec()),
        ("too long", too_long.into_bytes()),
        ("empty string", br#"{"session_id":""}"#.to_vec()),
    ];
    for (what, stdin) in cases {
        let output = report(&setup, &id, "/session_id", stdin).await;
        assert_eq!(output.status.code(), Some(0), "{what}: {}", err(&output));
        assert!(
            output.stdout.is_empty() && output.stderr.is_empty(),
            "{what}: a hook prints nothing"
        );
        assert_eq!(setup.entry(&id).session_id(), None, "{what}");
    }
}

#[tokio::test]
async fn report_session_reads_all_of_stdin_but_parses_only_the_first_64_kib() {
    let setup = Setup::start(|_| {}).await;
    let id = setup.window().await;
    let padding = "p".repeat(1024 * 1024);

    let early = format!(r#"{{"session_id":"early-1","pad":"{padding}"}}"#);
    let output = report(&setup, &id, "/session_id", early.into_bytes()).await;
    assert_eq!(output.status.code(), Some(0), "{}", err(&output));
    assert_eq!(setup.entry(&id).session_id().as_deref(), Some("early-1"));

    // The value sits past the first 64 KiB: it is never seen.
    let late = format!(
        r#"{{"pad":"{}","session_id":"late-2"}}"#,
        "p".repeat(100 * 1024)
    );
    let output = report(&setup, &id, "/session_id", late.into_bytes()).await;
    assert_eq!(output.status.code(), Some(0), "{}", err(&output));
    assert_eq!(
        setup.entry(&id).session_id().as_deref(),
        Some("early-1"),
        "a value beyond the parsed prefix is not kept"
    );
}

#[tokio::test]
async fn neither_stdin_nor_the_session_value_is_logged_by_the_command() {
    let setup = Setup::start(|_| {}).await;
    let id = setup.window().await;
    let stdin =
        br#"{"session_id":"LOGPROBE-SESSION-1","prompt":"LOGPROBE-PROMPT-SECRET"}"#.to_vec();
    let output = report(&setup, &id, "/session_id", stdin).await;
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        setup.entry(&id).session_id().as_deref(),
        Some("LOGPROBE-SESSION-1")
    );
    let mut files = Vec::new();
    collect_files(setup.bases.path(), &mut files);
    assert!(
        files.iter().any(|path| path.ends_with("daemon.log")),
        "the command writes a log at trace level, so the scan below can see one: {files:?}"
    );
    for path in files {
        let mut bytes = Vec::new();
        std::fs::File::open(&path)
            .and_then(|mut file| file.read_to_end(&mut bytes))
            .expect("read a file the command wrote");
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            !text.contains("LOGPROBE"),
            "{} logged a hook input",
            path.display()
        );
    }
}

fn collect_files(dir: &Path, found: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, found);
        } else {
            found.push(path);
        }
    }
}

// --- the doors answer as the window's profile says ------------------------------------
//
// The same doors, on a window opened from a profile that declares a `[delivery]` table:
// the profile's output form, limit, wake stream and wake exit replace the daemon's own
// defaults, which the windows above are measured against.

const PROFILE_ID: &str = "delivery-test";

impl Setup {
    /// A daemon whose only profile is `delivery-test`: `echo_child` bound to the control
    /// detection, with `delivery_lines` as its `[delivery]` table.
    async fn start_with_profile(
        delivery_lines: &str,
        tweak: impl FnOnce(&mut DaemonConfig),
    ) -> Self {
        let profiles_dir = unique_temp_dir("deliver-cli-profiles");
        std::fs::create_dir_all(&profiles_dir).expect("create the profiles directory");
        common::write_profile(
            &profiles_dir,
            PROFILE_ID,
            &format!(
                "schema = 1\ndisplay_name = \"Delivery Test\"\ndetection = \"{}\"\n\n\
                 [command]\ndefault = ['{}']\n\n[delivery]\n{delivery_lines}",
                common::CONTROL_DETECTION_NAME,
                echo_child_path().display(),
            ),
        );
        Self::start(move |config| {
            config.profiles_dir = Some(profiles_dir);
            config.host_os = yagura_daemon::profile::ProfileOs::host();
            config.program_lookup = common::empty_lookup();
            config.program_is_executable = |path| path.is_file();
            config.default_shell = None;
            tweak(config);
        })
        .await
    }

    /// Opens a window from the profile and returns it once it is idle, so that a push to it
    /// is allowed.
    async fn profile_window(&self) -> String {
        let mut client = self.h.connect().await;
        common::send(
            &mut client,
            &yagura_protocol::ClientMessage::OpenWindowFromProfile {
                profile: PROFILE_ID.to_string(),
                cwd: None,
                name: None,
                model: None,
                effort: None,
            },
        )
        .await;
        let id = match common::read_response(&mut client, Duration::from_secs(5)).await {
            yagura_protocol::DaemonMessage::WindowOpened { window_id, .. } => window_id,
            other => panic!("expected window_opened, got {other:?}"),
        };
        self.h.write_input(&id, common::READY_MARKER).await;
        common::wait_until_status(
            &mut client,
            &id,
            yagura_protocol::WindowStatus::Idle,
            Duration::from_secs(10),
        )
        .await;
        id
    }
}

fn json_out(output: &Output) -> serde_json::Value {
    serde_json::from_str(out(output).trim())
        .unwrap_or_else(|err| panic!("stdout must be one JSON document ({err}): {}", out(output)))
}

const SYSTEM_MESSAGE_TEMPLATE: &str =
    r#"output = '{"injectSteps":[{"systemMessage":{"systemMessage":{text}}}]}'"#;

#[tokio::test]
async fn a_json_template_in_the_profile_shapes_what_take_prints() {
    let setup = Setup::start_with_profile(SYSTEM_MESSAGE_TEMPLATE, |_| {}).await;
    let id = setup.profile_window().await;
    // Quotes, a backslash, a line break and the template's own slot names: none of it is
    // special to the embedding.
    let text = "say \"hi\"\\ now\nline two {text} {session}";
    setup.deposit(&id, "a", text);

    let output = setup.run(&["--deliver-take"], &id, VALID_TOKEN).await;

    assert_eq!(output.status.code(), Some(0), "{}", err(&output));
    assert_eq!(
        json_out(&output),
        serde_json::json!({"injectSteps": [{"systemMessage": {"systemMessage": text}}]})
    );
    assert!(output.stderr.is_empty());
}

#[tokio::test]
async fn a_json_template_in_the_profile_shapes_what_context_prints() {
    let setup = Setup::start_with_profile(SYSTEM_MESSAGE_TEMPLATE, |config| {
        config.chat_intro = "WELCOME-INTRO".into();
    })
    .await;
    let id = setup.profile_window().await;
    setup.deposit(&id, "a", "PARCEL-ALPHA");

    let output = setup.run(&["--deliver-context"], &id, VALID_TOKEN).await;

    assert_eq!(output.status.code(), Some(0), "{}", err(&output));
    let document = json_out(&output);
    let text = document["injectSteps"][0]["systemMessage"]["systemMessage"]
        .as_str()
        .unwrap_or_else(|| panic!("the text is embedded as a JSON string: {document}"));
    assert!(
        text.starts_with("WELCOME-INTRO") && text.contains("PARCEL-ALPHA"),
        "{text:?}"
    );
}

#[tokio::test]
async fn the_profiles_limit_caps_one_answer_and_the_rest_comes_with_the_next() {
    let setup = Setup::start_with_profile("max_chars = 20", |_| {}).await;
    let id = setup.profile_window().await;
    setup.deposit(&id, "a", "AAAAAAAAAAAAAAA");
    setup.deposit(&id, "b", "BBBBBBBBBBBBBBB");

    let first = setup.run(&["--deliver-take"], &id, VALID_TOKEN).await;
    assert_eq!(first.status.code(), Some(0), "{}", err(&first));
    assert!(out(&first).contains("AAAAAAAAAAAAAAA"), "{}", out(&first));
    assert!(
        !out(&first).contains('B'),
        "the second parcel does not fit within the profile's 20 characters: {}",
        out(&first)
    );

    let second = setup.run(&["--deliver-take"], &id, VALID_TOKEN).await;
    assert!(out(&second).contains("BBBBBBBBBBBBBBB"), "{}", out(&second));
    assert!(!out(&second).contains('A'));
}

#[tokio::test]
async fn a_parcel_longer_than_the_profiles_limit_is_turned_away_at_the_inbox() {
    let setup = Setup::start_with_profile("max_chars = 20", |_| {}).await;
    let id = setup.profile_window().await;

    let turned_away = setup
        .entry(&id)
        .inbox()
        .deposit(Parcel::new("long", "L".repeat(21)));

    assert_eq!(
        turned_away.len(),
        1,
        "the profile's limit, not the daemon's"
    );
    assert_eq!(setup.entry(&id).inbox().pending_count(), 0);
}

#[tokio::test]
async fn the_profiles_wake_stream_and_exit_code_end_a_wait() {
    let setup = Setup::start_with_profile(
        "wake = \"wait\"\nwake_stream = \"stdout\"\nwake_exit = 3\n",
        |_| {},
    )
    .await;
    let id = setup.profile_window().await;
    let child = setup.spawn(&["--deliver-wait"], &id, VALID_TOKEN);
    setup
        .until("the wait to be registered", || {
            setup.entry(&id).inbox().has_waiter()
        })
        .await;

    setup.deposit(&id, "a", "PARCEL-ALPHA");

    let output = finish(child).await;
    assert_eq!(output.status.code(), Some(3), "{}", err(&output));
    assert!(out(&output).contains("PARCEL-ALPHA"), "{}", out(&output));
    assert!(
        output.stderr.is_empty(),
        "the text goes to the stream the profile names only: {}",
        err(&output)
    );
}

#[tokio::test]
async fn a_window_the_profile_did_not_open_keeps_the_daemons_defaults() {
    let setup = Setup::start_with_profile("max_chars = 20\nwake_exit = 3\n", |_| {}).await;
    // Opened by argv, not from the profile: the profile's table does not apply to it.
    let id = setup.window().await;
    setup.deposit(&id, "a", "AAAAAAAAAAAAAAA");
    setup.deposit(&id, "b", "BBBBBBBBBBBBBBB");

    let output = setup.run(&["--deliver-take"], &id, VALID_TOKEN).await;

    assert!(
        out(&output).contains("AAAAAAAAAAAAAAA") && out(&output).contains("BBBBBBBBBBBBBBB"),
        "{}",
        out(&output)
    );
}
