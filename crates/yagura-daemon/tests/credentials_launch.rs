#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// Starts the compiled `yagura-daemon` binary as a real child process: what a window's
// child sees in its environment and argv is decided only by the real startup path.
#![allow(clippy::disallowed_methods)]

//! Integration test: a window opened from a profile whose `[credentials]` mode is
//! `launch` does not carry its credentials in its environment. They are in a file only
//! the window's own hook doors and bridge are handed (`{credentials_file}`), so a CLI
//! started by a tool of the window inherits nothing it could claim the window with.
//!
//! Every window is a real child of a real daemon: `env_probe` and `argv_probe` report
//! what they received, and `hook_child` plays the CLI whose hook doors report into the
//! window. The daemon runs under an instance of its own, so a daemon of the user is
//! never reached.

mod common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use common::real_daemon::{RunningDaemon, Sandbox, daemon_bin};
use common::{VALID_TOKEN, launch_args_logged_in, query_status, read_response, send};
use yagura_protocol::{ClientMessage, DaemonMessage, WindowStatus};
use yagura_test_support::process::{
    COMPOSER_CONTINUATION_PREFIX, COMPOSER_MARKER, COMPOSER_SEPARATOR, argv_probe_path,
    env_probe_path,
};

const WINDOW_ID_VAR: &str = yagura_protocol::WINDOW_ID_ENV;
const TOKEN_VAR: &str = yagura_protocol::STATUS_TOKEN_ENV;
const TIMEOUT: Duration = Duration::from_secs(20);

/// The tables of a `launch` profile whose hook doors are given the credentials file.
const LAUNCH_HOOKS: &str = "
[credentials]
mode = \"launch\"

[hooks]
args = [\"--credentials\", \"{credentials_file}\"]
";

/// Writes a profile that runs `hook_child` with `flags`, logging under `logs`, then
/// `tables`.
fn write_hook_child_profile(
    sandbox: &Sandbox,
    id: &str,
    logs: &Path,
    flags: &[&str],
    tables: &str,
) {
    let mut command = vec![
        env!("CARGO_BIN_EXE_hook_child").to_string(),
        COMPOSER_MARKER.to_string(),
        COMPOSER_SEPARATOR.to_string(),
        COMPOSER_CONTINUATION_PREFIX.to_string(),
        "0".to_string(),
        "-".to_string(),
        "--daemon".to_string(),
        daemon_bin().display().to_string(),
        "--log".to_string(),
        logs.display().to_string(),
    ];
    command.extend(flags.iter().map(ToString::to_string));
    let elements = command
        .iter()
        .map(|element| format!("'{element}'"))
        .collect::<Vec<_>>()
        .join(", ");
    let source = format!(
        "schema = 1\ndisplay_name = \"{id}\"\n\n[command]\ndefault = [{elements}]\n{tables}"
    );
    common::write_profile(&sandbox.profiles_dir, id, &source);
}

fn logs_dir(sandbox: &Sandbox, name: &str) -> PathBuf {
    sandbox.tree.make_dir(name)
}

/// The directory a window's credentials folder lives under.
fn mcp_dir(sandbox: &Sandbox) -> PathBuf {
    sandbox.state_root().join("mcp")
}

/// The credentials file `hook_child` was handed through `--credentials`.
async fn credentials_path(logs: &Path) -> PathBuf {
    let args = launch_args_logged_in(logs).await;
    let at = args
        .iter()
        .position(|arg| arg == "--credentials")
        .unwrap_or_else(|| panic!("the window was not handed --credentials; argv: {args:?}"));
    PathBuf::from(&args[at + 1])
}

async fn open_reply(
    daemon: &mut RunningDaemon,
    profile: &str,
    name: Option<&str>,
    on_name_taken: yagura_protocol::OnNameTaken,
) -> DaemonMessage {
    send(
        &mut daemon.client,
        &ClientMessage::OpenWindowFromProfile {
            profile: profile.to_string(),
            cwd: None,
            name: name.map(str::to_string),
            model: None,
            effort: None,
            on_name_taken,
        },
    )
    .await;
    read_response(&mut daemon.client, Duration::from_secs(15)).await
}

async fn open(daemon: &mut RunningDaemon, profile: &str) -> String {
    match open_reply(daemon, profile, None, Default::default()).await {
        DaemonMessage::WindowOpened { window_id, .. } => window_id,
        other => panic!("expected window_opened for {profile}, got {other:?}"),
    }
}

async fn until(what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    while !condition() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn logged(logs: &Path, event: &str) -> Option<serde_json::Value> {
    std::fs::read_to_string(logs.join("events.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find(|line| line["event"] == event)
}

/// Every path under `dir`, files and folders, in a stable order.
fn tree_of(dir: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, found: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            found.push(entry.path());
            if entry.path().is_dir() {
                walk(&entry.path(), found);
            }
        }
    }
    let mut found = Vec::new();
    walk(dir, &mut found);
    found.sort();
    found
}

/// The value `env_probe` reports for `name`; `None` when it is unset.
async fn reported_var(daemon: &mut RunningDaemon, window_id: &str, name: &str) -> Option<String> {
    let marker = format!("<<{name}");
    let text = daemon
        .output_until(window_id, "the variable report", |text| {
            text.rfind(&marker)
                .is_some_and(|start| text[start..].contains(">>"))
        })
        .await;
    let start = text.rfind(&marker)? + marker.len();
    text[start..]
        .strip_prefix('=')
        .and_then(|rest| rest.split_once(">>"))
        .map(|(value, _)| value.to_string())
}

fn write_var_probe(sandbox: &Sandbox, id: &str, name: &str, tables: &str) {
    let probe = env_probe_path();
    let probe = probe.to_str().expect("env_probe path must be UTF-8");
    sandbox.write_probe_profile(id, &[probe, name], tables);
}

// =============================================================================
// (a) the child's environment
// =============================================================================

#[tokio::test]
async fn a_launch_windows_child_has_no_window_credentials_even_when_the_daemon_inherited_them() {
    let sandbox = Sandbox::new("cred-launch-env");
    for (id, name) in [("id-launch", WINDOW_ID_VAR), ("token-launch", TOKEN_VAR)] {
        write_var_probe(&sandbox, id, name, LAUNCH_HOOKS);
    }
    write_var_probe(&sandbox, "id-env", WINDOW_ID_VAR, "");
    let mut daemon = sandbox
        .start_from(&[(WINDOW_ID_VAR, "w-inherited"), (TOKEN_VAR, VALID_TOKEN)])
        .await;

    // The same daemon's `env` window is the control: it gets its own id.
    let env_window = open(&mut daemon, "id-env").await;
    let own = reported_var(&mut daemon, &env_window, WINDOW_ID_VAR).await;
    assert_eq!(
        own.as_deref(),
        Some(env_window.as_str()),
        "control: an env window's child must get its own window id"
    );

    for (id, name) in [("id-launch", WINDOW_ID_VAR), ("token-launch", TOKEN_VAR)] {
        let window = open(&mut daemon, id).await;
        let value = reported_var(&mut daemon, &window, name).await;
        assert_eq!(
            value,
            None,
            "a launch window's child must not have {name}; daemon log:\n{}",
            sandbox.daemon_log()
        );
    }
}

// =============================================================================
// (b) the credentials file
// =============================================================================

#[tokio::test]
async fn the_credentials_file_is_an_absolute_path_naming_the_window_and_a_token_it_accepts() {
    let sandbox = Sandbox::new("cred-launch-file");
    let logs = logs_dir(&sandbox, "logs");
    write_hook_child_profile(&sandbox, "launch", &logs, &[], LAUNCH_HOOKS);
    let mut daemon = sandbox.start().await;

    let window_id = open(&mut daemon, "launch").await;
    let path = credentials_path(&logs).await;

    assert!(
        path.is_absolute(),
        "{{credentials_file}} must be an absolute path; got {path:?}"
    );
    if cfg!(windows) {
        assert!(
            !path.to_string_lossy().contains('\\'),
            "on Windows the path must use / separators; got {path:?}"
        );
    }
    assert!(
        path.is_file(),
        "the window's credentials file must exist while its child runs: {path:?}"
    );
    let folder = path.parent().expect("a file has a folder");
    assert!(
        folder.starts_with(mcp_dir(&sandbox)) && folder != mcp_dir(&sandbox),
        "the file must sit in a folder of its own under {:?}; got {path:?}",
        mcp_dir(&sandbox)
    );

    let contents: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(contents["window_id"], window_id);
    let token = contents["token"].as_str().unwrap_or_default().to_string();
    for (candidate, accepted) in [(token.as_str(), true), (VALID_TOKEN, false)] {
        send(
            &mut daemon.client,
            &ClientMessage::Identify {
                window_id: window_id.clone(),
                token: candidate.to_string(),
            },
        )
        .await;
        let reply = read_response(&mut daemon.client, Duration::from_secs(5)).await;
        assert_eq!(
            matches!(reply, DaemonMessage::Identified { .. }),
            accepted,
            "the token {candidate:?} must {}be the window's own; reply {reply:?}",
            if accepted { "" } else { "not " }
        );
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for owned in [path.as_path(), folder] {
            let mode = std::fs::metadata(owned).unwrap().permissions().mode();
            assert_eq!(
                mode & 0o077,
                0,
                "{owned:?} must be readable by its owner only"
            );
        }
    }
}

// =============================================================================
// (c) a window's own hook claims it, a nested CLI cannot
// =============================================================================

#[tokio::test]
async fn the_windows_own_hook_reports_into_it_and_a_cli_started_inside_it_cannot() {
    let sandbox = Sandbox::new("cred-launch-nested");
    let logs = logs_dir(&sandbox, "logs");
    let gate = sandbox.tree.make_dir("gate").join("go");
    let gate_arg = gate.display().to_string();
    write_hook_child_profile(
        &sandbox,
        "launch",
        &logs,
        &[
            "--nested-emit",
            "waiting_approval",
            "--gate",
            &gate_arg,
            "--emit-start",
            "waiting_approval",
        ],
        LAUNCH_HOOKS,
    );
    let mut daemon = sandbox.start().await;
    let window_id = open(&mut daemon, "launch").await;

    // The nested door has run to its end before the window's own hook is let go.
    until("the nested door to finish", || {
        logged(&logs, "nested_emit").is_some()
    })
    .await;
    let nested = logged(&logs, "nested_emit").unwrap();
    assert_eq!(
        nested["code"], 0,
        "a door without a window exits 0: {nested}"
    );
    let sampled_until = tokio::time::Instant::now() + Duration::from_millis(1500);
    while tokio::time::Instant::now() < sampled_until {
        assert_ne!(
            query_status(&mut daemon.client, &window_id).await,
            WindowStatus::WaitingApproval,
            "a report from a CLI that only inherited the window's environment must not \
             reach the window; daemon log:\n{}",
            sandbox.daemon_log()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    std::fs::write(&gate, b"go").unwrap();
    common::wait_until_status(
        &mut daemon.client,
        &window_id,
        WindowStatus::WaitingApproval,
        TIMEOUT,
    )
    .await;
}

// =============================================================================
// (d) the credentials folder is the window's, and goes with it
// =============================================================================

#[tokio::test]
async fn a_windows_credentials_are_gone_when_its_child_ends_or_it_is_closed() {
    let sandbox = Sandbox::new("cred-launch-gone");
    let ends_logs = logs_dir(&sandbox, "ends");
    let closed_logs = logs_dir(&sandbox, "closed");
    let exit = sandbox.tree.make_dir("exit").join("now");
    let exit_arg = exit.display().to_string();
    write_hook_child_profile(
        &sandbox,
        "ends",
        &ends_logs,
        &["--exit-on", &exit_arg],
        LAUNCH_HOOKS,
    );
    write_hook_child_profile(&sandbox, "closed", &closed_logs, &[], LAUNCH_HOOKS);
    // Closing a window over IPC needs approval, so this daemon's playbook allows it.
    let playbook = sandbox
        .tree
        .make_dir("close-playbook")
        .join("playbook.toml");
    std::fs::write(
        &playbook,
        "[approval]
auto_approve = [\"open_window\", \"close_window\"]
",
    )
    .unwrap();
    let mut daemon = sandbox
        .start_with_args(&["--playbook", playbook.to_str().unwrap()])
        .await;

    open(&mut daemon, "ends").await;
    let ends = credentials_path(&ends_logs).await;
    assert!(ends.is_file(), "the file must exist while the child runs");
    std::fs::write(&exit, b"now").unwrap();
    until("the credentials of an ended child to go", || {
        !ends.exists() && ends.parent().is_some_and(|folder| !folder.exists())
    })
    .await;

    let closed_window = open(&mut daemon, "closed").await;
    let closed = credentials_path(&closed_logs).await;
    assert!(closed.is_file(), "the file must exist while the child runs");
    send(
        &mut daemon.client,
        &ClientMessage::CloseWindow {
            window_id: closed_window,
            force: None,
        },
    )
    .await;
    let reply = read_response(&mut daemon.client, Duration::from_secs(15)).await;
    assert!(
        matches!(reply, DaemonMessage::WindowClosed { .. }),
        "got {reply:?}"
    );
    until("the credentials of a closed window to go", || {
        !closed.exists() && closed.parent().is_some_and(|folder| !folder.exists())
    })
    .await;
}

/// A file the platform will not run: a program the profile resolves but the spawn refuses.
fn write_unrunnable_program(sandbox: &Sandbox) -> PathBuf {
    let name = if cfg!(windows) {
        "broken.exe"
    } else {
        "broken"
    };
    let path = sandbox.tree.make_dir("broken").join(name);
    std::fs::write(&path, b"this is not an executable image\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    path
}

#[tokio::test]
async fn an_open_that_ends_early_leaves_no_credentials_behind() {
    let sandbox = Sandbox::new("cred-launch-early");
    let logs = logs_dir(&sandbox, "logs");
    write_hook_child_profile(&sandbox, "launch", &logs, &[], LAUNCH_HOOKS);
    let broken = write_unrunnable_program(&sandbox);
    sandbox.write_probe_profile(
        "broken",
        &[broken.to_str().expect("path must be UTF-8")],
        LAUNCH_HOOKS,
    );
    let mut daemon = sandbox.start().await;

    // A live window's credentials are what the others are measured against.
    open(&mut daemon, "launch").await;
    let live = credentials_path(&logs).await;
    assert!(live.is_file(), "the live window's credentials must exist");
    let before = tree_of(&mcp_dir(&sandbox));

    let spawn_failed = open_reply(&mut daemon, "broken", None, Default::default()).await;
    assert!(
        matches!(spawn_failed, DaemonMessage::Error { .. }),
        "a program that cannot be spawned must fail the open; got {spawn_failed:?}"
    );
    let named = open_reply(
        &mut daemon,
        "launch",
        Some("taken"),
        yagura_protocol::OnNameTaken::Refuse,
    )
    .await;
    assert!(
        matches!(named, DaemonMessage::WindowOpened { .. }),
        "{named:?}"
    );
    let before_refused = tree_of(&mcp_dir(&sandbox));
    let name_taken = open_reply(
        &mut daemon,
        "launch",
        Some("taken"),
        yagura_protocol::OnNameTaken::Refuse,
    )
    .await;
    assert!(
        matches!(name_taken, DaemonMessage::Error { .. }),
        "a taken name must refuse the open; got {name_taken:?}"
    );

    // Give a leaked guard no excuse to be dropped late.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let after = tree_of(&mcp_dir(&sandbox));
    assert!(
        before.iter().all(|path| after.contains(path)),
        "the live window's credentials must survive"
    );
    let unexpected_spawn: Vec<_> = after
        .iter()
        .filter(|path| !before_refused.contains(path) && !before.contains(path))
        .collect();
    assert!(
        unexpected_spawn.is_empty(),
        "an open that failed must leave nothing under {:?}; found {unexpected_spawn:?}",
        mcp_dir(&sandbox)
    );
}

// =============================================================================
// (e) the names a bridge's `env` lists
// =============================================================================

/// A copy of the built daemon in a folder of its own, beside a stand-in MCP bridge (and,
/// on Windows, the vendored pseudoconsole pair it needs).
fn install_daemon_with_bridge(sandbox: &Sandbox) -> PathBuf {
    let dir = sandbox.tree.make_dir("install");
    #[cfg(windows)]
    {
        let vendored = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../vendor/conpty/x64");
        for name in ["conpty.dll", "OpenConsole.exe"] {
            std::fs::copy(vendored.join(name), dir.join(name)).unwrap();
        }
    }
    let suffix = std::env::consts::EXE_SUFFIX;
    let bin = dir.join(format!("yagura-daemon{suffix}"));
    std::fs::copy(daemon_bin(), &bin).unwrap();
    std::fs::copy(env_probe_path(), dir.join(format!("yagura-mcp{suffix}"))).unwrap();
    bin
}

fn bridge_entry_table(bridge_args: &str) -> String {
    format!(
        "[mcp]\nargs = ['-c', 'mcpServers.{{server}}={{\"transport\":\"stdio\",\
         \"command\":{{bridge_json}},\"args\":{bridge_args},\"env\":{{env_refs}}}}']\n"
    )
}

/// The variable names the bridge entry of the `argv_probe` window `window_id` lists.
async fn listed_env_names(
    daemon: &mut RunningDaemon,
    window_id: &str,
) -> std::collections::BTreeSet<String> {
    let flat = |text: &str| {
        text.chars()
            .filter(|c| !matches!(c, '\r' | '\n'))
            .collect::<String>()
    };
    let text = daemon
        .output_until(window_id, "the argv report", |text| {
            let text = flat(text);
            text.rfind("<<argv[2]=")
                .is_some_and(|start| text[start..].contains(">>"))
        })
        .await;
    let text = flat(&text);
    let start = text.rfind("<<argv[2]=").unwrap() + "<<argv[2]=".len();
    let argv2 = &text[start..text[start..].find(">>").unwrap() + start];
    let (_, json) = argv2.split_once('=').expect("argv[2] must be key=json");
    let entry: serde_json::Value = serde_json::from_str(json)
        .unwrap_or_else(|err| panic!("the bridge entry must be JSON: {err}; got {json:?}"));
    entry["env"]
        .as_object()
        .unwrap_or_else(|| panic!("`env` must be an object; got {entry}"))
        .keys()
        .cloned()
        .collect()
}

#[tokio::test]
async fn a_bridges_env_names_the_window_credentials_only_in_an_env_window() {
    let sandbox = Sandbox::new("cred-launch-env-refs");
    let bin = install_daemon_with_bridge(&sandbox);
    let argv_probe = argv_probe_path();
    let argv_probe = argv_probe.to_str().expect("argv_probe path must be UTF-8");
    sandbox.write_probe_profile("env-window", &[argv_probe], &bridge_entry_table("[]"));
    sandbox.write_probe_profile(
        "launch-window",
        &[argv_probe],
        &format!(
            "\n[credentials]\nmode = \"launch\"\n\n{}",
            bridge_entry_table("[\"--credentials\",\"{credentials_file}\"]")
        ),
    );
    let mut daemon = sandbox.start_bin(&bin).await;

    let env_window = open(&mut daemon, "env-window").await;
    let env_names = listed_env_names(&mut daemon, &env_window).await;
    let launch_window = open(&mut daemon, "launch-window").await;
    let launch_names = listed_env_names(&mut daemon, &launch_window).await;

    for name in [WINDOW_ID_VAR, TOKEN_VAR] {
        assert!(
            env_names.contains(name),
            "an env window's bridge must still be handed {name}; got {env_names:?}"
        );
        assert!(
            !launch_names.contains(name),
            "a launch window's bridge env must not name {name}; got {launch_names:?}; daemon \
             log:\n{}",
            sandbox.daemon_log()
        );
    }
    assert!(
        launch_names.contains(yagura_protocol::INSTANCE_ENV),
        "the instance variable is still handed on; got {launch_names:?}"
    );
}

// =============================================================================
// a launch profile whose own settings carry no credentials file
// =============================================================================

#[tokio::test]
async fn a_launch_profile_with_settings_that_ignore_the_credentials_file_still_reports() {
    let sandbox = Sandbox::new("cred-launch-fallback");
    let logs = logs_dir(&sandbox, "logs");
    write_hook_child_profile(
        &sandbox,
        "own-hooks",
        &logs,
        &["--emit-start", "waiting_approval"],
        "\n[credentials]\nmode = \"launch\"\n\n[hooks]\nargs = [\"--own-hooks\", \"x\"]\n",
    );
    let mut daemon = sandbox.start().await;

    let window_id = open(&mut daemon, "own-hooks").await;

    common::wait_until_status(
        &mut daemon.client,
        &window_id,
        WindowStatus::WaitingApproval,
        TIMEOUT,
    )
    .await;
    let emitted = logged(&logs, "emit_start").expect("the report was made");
    assert_eq!(emitted["code"], 0, "{emitted}");
}
