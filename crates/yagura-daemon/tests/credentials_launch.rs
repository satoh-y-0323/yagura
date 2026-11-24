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

use crate::common;
use crate::common::bins::{self, argv_probe_path, env_probe_path};

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use common::door_relay::{Printed, relay_door_with, unique_instance};
use common::real_daemon::{RunningDaemon, Sandbox, daemon_bin};
use common::{Harness, VALID_TOKEN, launch_args_logged_in, read_response, send};
use yagura_daemon::DaemonConfig;
use yagura_daemon::child_env::ChildBaseEnv;
use yagura_daemon::profile::ProfileOs;
use yagura_protocol::{ClientMessage, DaemonMessage, Declaration, WindowStatus};
use yagura_test_support::ipc::Connection;
use yagura_test_support::paths::TempDir;
use yagura_test_support::process::{
    COMPOSER_CONTINUATION_PREFIX, COMPOSER_MARKER, COMPOSER_SEPARATOR,
};

const WINDOW_ID_VAR: &str = yagura_protocol::WINDOW_ID_ENV;
const TOKEN_VAR: &str = yagura_protocol::STATUS_TOKEN_ENV;

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
    open_reply_on(&mut daemon.client, profile, name, on_name_taken).await
}

async fn open_reply_on(
    client: &mut Connection,
    profile: &str,
    name: Option<&str>,
    on_name_taken: yagura_protocol::OnNameTaken,
) -> DaemonMessage {
    send(
        client,
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
    read_response(client).await
}

async fn open(daemon: &mut RunningDaemon, profile: &str) -> String {
    opened(
        open_reply(daemon, profile, None, Default::default()).await,
        profile,
    )
}

async fn open_on(client: &mut Connection, profile: &str) -> String {
    opened(
        open_reply_on(client, profile, None, Default::default()).await,
        profile,
    )
}

fn opened(reply: DaemonMessage, profile: &str) -> String {
    match reply {
        DaemonMessage::WindowOpened { window_id, .. } => window_id,
        other => panic!("expected window_opened for {profile}, got {other:?}"),
    }
}

async fn until(what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + yagura_test_support::HANG_GUARD;
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
    // The window was opened from `launch`, whose hooks the daemon arranged, so only a
    // claim that declares that profile is the window's; and this test process is not of
    // the window's tree, so even that claim is refused for its lineage.
    let declaring = |profile: &str| {
        Some(Declaration {
            profile: profile.to_string(),
        })
    };
    for (what, candidate, declaration, refused_as) in [
        (
            "its token, declaring its profile, from outside the window's tree",
            token.as_str(),
            declaring("launch"),
            Some("lineage_refused"),
        ),
        ("another token", VALID_TOKEN, declaring("launch"), None),
        (
            "its token, declaring another profile",
            token.as_str(),
            declaring("other"),
            None,
        ),
        (
            "its token, declaring no profile",
            token.as_str(),
            None,
            None,
        ),
    ] {
        let mut client = yagura_test_support::ipc::connect_with_retry(sandbox.endpoint()).await;
        send(
            &mut client,
            &ClientMessage::Identify {
                window_id: window_id.clone(),
                token: candidate.to_string(),
                declaration,
            },
        )
        .await;
        let reply = read_response(&mut client).await;
        assert!(
            !matches!(reply, DaemonMessage::Identified { .. }),
            "{what}: the claim must not be the window's; reply {reply:?}"
        );
        if let Some(expected) = refused_as {
            match &reply {
                DaemonMessage::Error { code, .. } => {
                    assert_eq!(code, expected, "{what}: reply {reply:?}");
                }
                other => panic!("{what}: expected the error {expected}, got {other:?}"),
            }
        }
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
// (c) a window's own hook claims it
// =============================================================================

/// The window's own hook door, handed `--credentials`, reports into the window. A CLI
/// started inside the window cannot: what it inherits is measured by
/// `a_launch_windows_child_has_no_window_credentials_even_when_the_daemon_inherited_them`,
/// and an event without the window's credentials is refused by
/// `a_status_event_without_a_window_id_is_rejected_and_touches_no_window` and
/// `status_event_with_a_wrong_token_is_rejected_and_leaves_the_status_untouched`
/// (`ipc_control.rs`).
#[tokio::test]
async fn the_windows_own_hook_reports_into_it() {
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
    // A door reads its window only from the environment or `--credentials`, and
    // the nested one is never handed `--credentials`; that the environment holds
    // neither variable is measured on the child itself by
    // `a_launch_windows_child_has_no_window_credentials_even_when_the_daemon_inherited_them`.
    // A door without a window ends before it reaches the daemon, so no later
    // event of this daemon is ordered after it.
    assert_eq!(
        nested["code"], 0,
        "a door without a window exits 0: {nested}"
    );

    std::fs::write(&gate, b"go").unwrap();
    common::wait_until_status(
        &mut daemon.client,
        &window_id,
        WindowStatus::WaitingApproval,
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
    let reply = read_response(&mut daemon.client).await;
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
/// On Unix it has no execute bit, since `execvp` would hand an executable text file to `/bin/sh`.
fn write_unrunnable_program(sandbox: &Sandbox) -> PathBuf {
    let name = if cfg!(windows) {
        "broken.exe"
    } else {
        "broken"
    };
    let path = sandbox.tree.make_dir("broken").join(name);
    std::fs::write(&path, b"this is not an executable image\n").unwrap();
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

    // Killed, the daemon neither creates nor removes anything more, so the
    // tree is final: whatever an open left behind, however late it would
    // have gone, is still there.
    drop(daemon);
    let after = tree_of(&mcp_dir(&sandbox));
    assert!(
        before.iter().all(|path| after.contains(path)),
        "the live window's credentials must survive"
    );
    let new_folders: Vec<_> = top_level(&after)
        .into_iter()
        .filter(|path| !before.contains(path))
        .collect();
    assert_eq!(
        new_folders.len(),
        1,
        "only the window opened in between keeps a folder; an open that failed must leave \
         nothing under {:?}; found {new_folders:?}",
        mcp_dir(&sandbox)
    );
}

/// The entries of `tree` directly under the folder it was read from.
fn top_level(tree: &[PathBuf]) -> Vec<&PathBuf> {
    tree.iter()
        .filter(|path| {
            !tree
                .iter()
                .any(|other| path.starts_with(other) && path != &other)
        })
        .collect()
}

// =============================================================================
// (e) the names a bridge's `env` lists
// =============================================================================

/// A copy of the built daemon in a folder of its own, beside a stand-in MCP bridge, the
/// holder and, on Windows, the pseudoconsole pair.
fn install_daemon_with_bridge(sandbox: &Sandbox) -> PathBuf {
    let dir = sandbox.tree.make_dir("install");
    bins::install_window_runtime(&dir);
    let suffix = std::env::consts::EXE_SUFFIX;
    let bin = dir.join(format!("yagura-daemon{suffix}"));
    yagura_test_support::process::install_exe(&daemon_bin(), &bin);
    yagura_test_support::process::install_exe(
        &env_probe_path(),
        &dir.join(format!("yagura-mcp{suffix}")),
    );
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
    )
    .await;
    let emitted = logged(&logs, "emit_start").expect("the report was made");
    // `--emit-status` ends with 0 only on the daemon's pong, so the report has been handled.
    assert_eq!(emitted["code"], 0, "{emitted}");
}

// =============================================================================
// the overlay that keeps the credentials of a window in the environment from its tools
// =============================================================================

/// The variable a window run directly from a profile declaring an overlay is handed it in.
pub(crate) const OVERLAY_VAR: &str = "GROK_CONFIG";

/// What every overlay drops from the window's tools, beside what the user drops: the
/// window's credentials and the overlay itself, which can hold what was only in the
/// user's files.
pub(crate) const OVERLAY_DROPS: [&str; 4] =
    [WINDOW_ID_VAR, TOKEN_VAR, "GROK_CONFIG", "GROK_CONFIG_PATH"];

/// The shipped profile `shipped` as the TOML of the profile `id`: its `[command]` replaced
/// by `argv` on every OS, its display name by `id`, with `convert` its
/// `[host_paths].convert` replaced, and `env` added to its `[env]`. Every other table
/// stays as shipped, so whatever the profile declares for its overlay comes with it.
pub(crate) fn shipped_with_command(
    id: &str,
    shipped: &str,
    argv: &[String],
    convert: Option<&[String]>,
    env: &[(&str, &str)],
) -> String {
    let strings = |items: &[String]| {
        toml::Value::Array(items.iter().cloned().map(toml::Value::String).collect())
    };
    let text = common::docs::read_doc_verbatim(&format!("../../profiles/{shipped}.toml"));
    let mut document: toml::Table = text
        .parse()
        .unwrap_or_else(|err| panic!("the shipped profile {shipped} is TOML: {err}"));
    document.insert("display_name".into(), toml::Value::String(id.to_string()));
    let mut command = toml::Table::new();
    command.insert("default".into(), strings(argv));
    document.insert("command".into(), toml::Value::Table(command));
    if let Some(convert) = convert {
        document
            .entry("host_paths")
            .or_insert_with(|| toml::Value::Table(toml::Table::new()))
            .as_table_mut()
            .expect("[host_paths] is a table")
            .insert("convert".into(), strings(convert));
    }
    if !env.is_empty() {
        let table = document
            .entry("env")
            .or_insert_with(|| toml::Value::Table(toml::Table::new()))
            .as_table_mut()
            .expect("[env] is a table");
        for (name, value) in env {
            table.insert(
                (*name).to_string(),
                toml::Value::String((*value).to_string()),
            );
        }
    }
    toml::to_string(&document).expect("serialize the profile")
}

/// A `[host_paths].convert` that hands every path over unchanged, as a guest that sees the
/// daemon's paths as they are would.
pub(crate) fn unchanged_convert() -> Vec<String> {
    vec![
        bins::capture_child_path().display().to_string(),
        "echo".to_string(),
        "0".to_string(),
        "{path}".to_string(),
    ]
}

/// The text of a window's output with its line breaks removed, so a value the terminal
/// wrapped reads as one run.
fn flat(text: &str) -> String {
    text.chars().filter(|c| !matches!(c, '\r' | '\n')).collect()
}

/// Whether `flat_text` holds the whole of the overlay report an `env_probe` window prints.
fn overlay_report_done(flat_text: &str) -> bool {
    let marker = format!("<<{OVERLAY_VAR}");
    flat_text
        .rfind(&marker)
        .is_some_and(|start| flat_text[start..].contains(">>"))
}

/// The overlay the report in `flat_text` carries, as JSON; `context` says what to show of the
/// daemon when the window was handed none.
fn parse_overlay_report(flat_text: &str, context: impl FnOnce() -> String) -> serde_json::Value {
    let marker = format!("<<{OVERLAY_VAR}");
    let rest = &flat_text[flat_text.rfind(&marker).unwrap() + marker.len()..];
    let value = rest.strip_prefix('=').unwrap_or_else(|| {
        panic!(
            "the window's CLI was handed no {OVERLAY_VAR}: {rest:?}; daemon log:\n{}",
            context()
        )
    });
    let value = &value[..value.find(">>").unwrap()];
    serde_json::from_str(value)
        .unwrap_or_else(|err| panic!("{OVERLAY_VAR} must be a JSON object: {err}; got {value:?}"))
}

/// The overlay the `env_probe` window `window_id` reports in [`OVERLAY_VAR`], as JSON.
pub(crate) async fn reported_overlay(
    daemon: &mut RunningDaemon,
    sandbox: &Sandbox,
    window_id: &str,
) -> serde_json::Value {
    let text = daemon
        .output_until(window_id, "the overlay report", |text| {
            overlay_report_done(&flat(text))
        })
        .await;
    parse_overlay_report(&flat(&text), || sandbox.daemon_log())
}

/// [`reported_overlay`] for a window of an in-process daemon.
async fn reported_overlay_in(h: &Harness, window_id: &str) -> serde_json::Value {
    until("the overlay report", || {
        overlay_report_done(&h.flattened_scrollback(window_id))
    })
    .await;
    parse_overlay_report(&h.flattened_scrollback(window_id), String::new)
}

/// The names the overlay drops from the window's tools.
pub(crate) fn overlay_drops(overlay: &serde_json::Value) -> BTreeSet<String> {
    overlay["shell_environment_policy"]["exclude"]
        .as_array()
        .unwrap_or_else(|| panic!("the overlay names what it drops: {overlay}"))
        .iter()
        .map(|name| name.as_str().expect("a name").to_string())
        .collect()
}

/// [`OVERLAY_DROPS`] and `extra`.
pub(crate) fn drops_with(extra: &[&str]) -> BTreeSet<String> {
    OVERLAY_DROPS
        .iter()
        .chain(extra)
        .map(|name| (*name).to_string())
        .collect()
}

/// One way the user may have set up what grok's tools see, and what a window run directly
/// must be handed for it.
struct OverlayCase {
    what: &'static str,
    /// The user's `~/.grok/config.toml`.
    config_toml: Option<&'static str>,
    /// The user's `GROK_CONFIG`.
    inline: Option<&'static str>,
    /// The user's `GROK_CONFIG_PATH` file, as `(file name, text)`.
    file: Option<(&'static str, &'static str)>,
    /// What the user drops from the tools; `None` when the overlay need only drop at
    /// least what every overlay drops.
    drops: Option<&'static [&'static str]>,
    /// `(table, key, value)` of the user's overlay the window keeps as the user wrote it.
    kept: &'static [(&'static str, &'static str, &'static str)],
}

const OVERLAY_CASES: [OverlayCase; 4] = [
    OverlayCase {
        what: "nothing of the user's",
        config_toml: None,
        inline: None,
        file: None,
        drops: Some(&[]),
        kept: &[],
    },
    OverlayCase {
        what: "the user's config.toml and inline overlay",
        config_toml: Some("[shell_environment_policy]\nexclude = [\"ACME_*\"]\n"),
        inline: Some(
            r#"{"models":{"default_reasoning_effort":"high"},"shell_environment_policy":{"exclude":["BETA_*"],"inherit":"core"}}"#,
        ),
        file: None,
        drops: Some(&["ACME_*", "BETA_*"]),
        kept: &[
            ("models", "default_reasoning_effort", "high"),
            ("shell_environment_policy", "inherit", "core"),
        ],
    },
    OverlayCase {
        what: "the user's overlay file",
        config_toml: None,
        inline: None,
        file: Some((
            "overlay.toml",
            "[shell_environment_policy]\nexclude = [\"GAMMA_*\"]\n\n[models]\n\
             default_reasoning_effort = \"low\"\n",
        )),
        drops: Some(&["GAMMA_*"]),
        kept: &[("models", "default_reasoning_effort", "low")],
    },
    OverlayCase {
        what: "an inline overlay that is not JSON",
        config_toml: None,
        inline: Some("{not json"),
        file: None,
        drops: None,
        kept: &[],
    },
];

/// A window run directly from a profile declaring an overlay is handed, in its
/// environment, an overlay that drops the union of what the user drops (their
/// `config.toml` and their own overlay) and what every overlay drops, and keeps every
/// other key of the user's overlay as the user wrote it. One the daemon cannot read still
/// leaves the window's credentials dropped.
#[tokio::test]
async fn a_window_run_directly_is_handed_the_union_of_every_drop_and_keeps_the_users_overlay() {
    for case in OVERLAY_CASES {
        let sandbox = Sandbox::new("cred-overlay-direct");
        let home = sandbox.tree.make_dir("home");
        if let Some(text) = case.config_toml {
            let dir = home.join(".grok");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("config.toml"), text).unwrap();
        }
        let file = case.file.map(|(name, text)| {
            let path = sandbox.tree.make_dir("user").join(name);
            std::fs::write(&path, text).unwrap();
            path.display().to_string()
        });
        let home_text = home.display().to_string();
        let mut env: Vec<(&str, &str)> = Vec::new();
        if cfg!(windows) {
            env.push(("USERPROFILE", &home_text));
        }
        if let Some(inline) = case.inline {
            env.push(("GROK_CONFIG", inline));
        }
        if let Some(file) = &file {
            env.push(("GROK_CONFIG_PATH", file));
        }
        let probe = env_probe_path().display().to_string();
        common::write_profile(
            &sandbox.profiles_dir,
            "overlay-direct",
            &shipped_with_command(
                "overlay-direct",
                "grok-build",
                &[probe, OVERLAY_VAR.to_string()],
                None,
                &[],
            ),
        );
        let mut daemon = sandbox.start_from(&env).await;

        let window_id = open(&mut daemon, "overlay-direct").await;
        let overlay = reported_overlay(&mut daemon, &sandbox, &window_id).await;
        let dropped = overlay_drops(&overlay);
        match case.drops {
            Some(user) => assert_eq!(
                dropped,
                drops_with(user),
                "{}: the overlay drops exactly the union; got {overlay}",
                case.what
            ),
            None => assert!(
                dropped.is_superset(&drops_with(&[])),
                "{}: the window's credentials are dropped even so; got {overlay}",
                case.what
            ),
        }
        for (table, key, value) in case.kept {
            assert_eq!(
                overlay[*table][*key], *value,
                "{}: the user's {table}.{key} stays as the user wrote it; got {overlay}",
                case.what
            );
        }
    }
}

/// [`claims`] on a new connection to the daemon serving `endpoint`.
async fn claims_at(
    endpoint: &str,
    window_id: &str,
    profile: &str,
    token: &str,
    times: usize,
) -> (Vec<DaemonMessage>, bool) {
    let mut client = yagura_test_support::ipc::connect_with_retry(endpoint).await;
    let mut answers = Vec::new();
    for _ in 0..times {
        send(
            &mut client,
            &ClientMessage::Identify {
                window_id: window_id.to_string(),
                token: token.to_string(),
                declaration: Some(Declaration {
                    profile: profile.to_string(),
                }),
            },
        )
        .await;
        answers.push(read_response(&mut client).await);
    }
    send(&mut client, &ClientMessage::Ping).await;
    let ponged = matches!(read_response(&mut client).await, DaemonMessage::Pong);
    (answers, ponged)
}

/// What a door taking the inbox of `window_id` with `token` is answered.
async fn door_take(endpoint: &str, window_id: &str, token: &str) -> DaemonMessage {
    let mut client = yagura_test_support::ipc::connect_with_retry(endpoint).await;
    send(
        &mut client,
        &ClientMessage::DoorRead {
            window_id: window_id.to_string(),
            token: token.to_string(),
            door: yagura_protocol::Door::Take,
        },
    )
    .await;
    let answer = read_response(&mut client).await;
    if matches!(answer, DaemonMessage::Delivery { .. }) {
        send(&mut client, &ClientMessage::DeliverPrinted).await;
    }
    answer
}

/// The code a status event or a claim is refused with while the window's overlay has not
/// been made.
pub(crate) const OVERLAY_PENDING: &str = "launch_overlay_pending";

/// Whether `answer` refuses because the window's overlay has not been made.
pub(crate) fn refused_as_pending(answer: &DaemonMessage) -> bool {
    matches!(answer, DaemonMessage::Error { code, .. } if code == OVERLAY_PENDING)
}

/// Whether a door's answer is the silence a door is refused with: nothing to print, exit 0.
fn silent(answer: &DaemonMessage) -> bool {
    matches!(
        answer,
        DaemonMessage::Delivery { output, exit_code: 0, .. } if output.is_empty()
    )
}

/// The argv of a CLI that keeps its window open, asks nothing of the daemon, and logs its
/// argv under `logs` once it has started.
pub(crate) fn quiet_cli(logs: &Path) -> Vec<String> {
    vec![
        env!("CARGO_BIN_EXE_hook_child").to_string(),
        COMPOSER_MARKER.to_string(),
        COMPOSER_SEPARATOR.to_string(),
        COMPOSER_CONTINUATION_PREFIX.to_string(),
        "0".to_string(),
        "-".to_string(),
        "--log".to_string(),
        logs.display().to_string(),
    ]
}

/// The windows the overlay cases open: one run directly from a profile declaring an
/// overlay, one in another environment from such a profile, and one in another
/// environment from a profile declaring none.
const DIRECT: &str = "overlay-gate-direct";
const GUEST: &str = "overlay-gate-guest";
const PLAIN: &str = "overlay-gate-plain";

/// The tree and profiles directory of the profiles [`DIRECT`], [`GUEST`] and [`PLAIN`],
/// whose CLIs keep their windows open.
fn overlay_profiles(tag: &str) -> (TempDir, PathBuf) {
    let tree = TempDir::new("yagura-cli-test", tag);
    let profiles_dir = tree.make_dir("profiles");
    let convert = unchanged_convert();
    for (id, shipped, convert) in [
        (DIRECT, "grok-build", None),
        (GUEST, "wsl-grok-build", Some(convert.as_slice())),
    ] {
        let cli = quiet_cli(&tree.make_dir(id));
        common::write_profile(
            &profiles_dir,
            id,
            &shipped_with_command(id, shipped, &cli, convert, &[]),
        );
    }
    let quoted = |items: Vec<String>| {
        items
            .iter()
            .map(|arg| format!("'{arg}'"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    common::write_profile(
        &profiles_dir,
        PLAIN,
        &format!(
            "schema = 1\ndisplay_name = \"{PLAIN}\"\n\n\
             [command]\ndefault = [{}]\n\n[host_paths]\nconvert = [{}]\n",
            quoted(quiet_cli(&tree.make_dir(PLAIN))),
            quoted(convert)
        ),
    );
    (tree, profiles_dir)
}

/// An in-process daemon serving `profiles_dir`, whose windows' children see `env` beside the
/// test's own and whose user home is an empty directory of `tree`.
async fn start_overlay_daemon(
    tag: &str,
    tree: &TempDir,
    profiles_dir: PathBuf,
    env: &[(&str, &str)],
) -> Harness {
    Harness::start(
        tag,
        DaemonConfig {
            profiles_dir: Some(profiles_dir),
            home_dir: Some(tree.make_dir("home")),
            child_base_env: ChildBaseEnv::from_pairs(
                env.iter()
                    .map(|(name, value)| ((*name).into(), (*value).into()))
                    .collect(),
            ),
            host_os: ProfileOs::host(),
            program_lookup: common::empty_lookup(),
            program_is_executable: |path| path.is_file(),
            default_shell: None,
            ..common::approver_base_config()
        },
    )
    .await
}

/// In a window whose profile declares an overlay and whose CLI runs in another
/// environment, the window's credentials speak for it only once its overlay was made:
/// before that, a claim and a status event are refused as pending, the claims not counted
/// against the connection, and a door is answered with silence. A window
/// run directly has its overlay from its open, and a window whose profile declares none is
/// spoken for as before.
#[tokio::test]
async fn a_window_declaring_an_overlay_is_not_spoken_for_before_its_overlay_is_made() {
    // More claims than `IDENTIFY_FAILURE_LIMIT` (`ipc.rs`) lets one connection fail.
    const CLAIMS: usize = 5;
    let (tree, profiles_dir) = overlay_profiles("cred-overlay-gate");
    let h = start_overlay_daemon("cred-overlay-gate", &tree, profiles_dir, &[]).await;
    let mut client = h.connect().await;
    let direct = open_on(&mut client, DIRECT).await;
    let guest = open_on(&mut client, GUEST).await;
    let plain = open_on(&mut client, PLAIN).await;

    for (window_id, profile) in [(&direct, DIRECT), (&plain, PLAIN)] {
        let (answers, _) = claims_at(&h.endpoint, window_id, profile, VALID_TOKEN, 1).await;
        assert!(
            matches!(answers[0], DaemonMessage::Identified { .. }),
            "{profile}: the window is spoken for by its credentials, got {:?}",
            answers[0]
        );
        let refusal = common::status_event_refusal(&mut client, window_id, VALID_TOKEN).await;
        assert!(refusal.is_none(), "{profile}: got {refusal:?}");
        let door = door_take(&h.endpoint, window_id, VALID_TOKEN).await;
        assert!(
            matches!(door, DaemonMessage::Delivery { .. }),
            "{profile}: the door is answered, got {door:?}"
        );
    }

    let (answers, ponged) = claims_at(&h.endpoint, &guest, GUEST, VALID_TOKEN, CLAIMS).await;
    for answer in &answers {
        assert!(
            refused_as_pending(answer),
            "a claim before the overlay is refused as pending, got {answer:?}"
        );
    }
    assert!(
        ponged,
        "claims refused for a missing overlay are not counted against the connection"
    );
    let refusal = common::status_event_refusal(&mut client, &guest, VALID_TOKEN).await;
    assert!(
        refusal.as_ref().is_some_and(refused_as_pending),
        "a status event before the overlay is refused as pending, got {refusal:?}"
    );
    let door = door_take(&h.endpoint, &guest, VALID_TOKEN).await;
    assert!(
        silent(&door),
        "a door before the overlay is answered with silence, got {door:?}"
    );
    h.registry.terminate_all().await;
}

/// The wire form of a request for the overlay of `window_id`, with the guest's facts.
pub(crate) fn build_launch_overlay(
    window_id: &str,
    token: &str,
    env: serde_json::Value,
) -> ClientMessage {
    let wire = serde_json::json!({
        "type": "build_launch_overlay",
        "window_id": window_id,
        "token": token,
        "env": env,
        "files": {},
    });
    serde_json::from_value(wire.clone())
        .unwrap_or_else(|err| panic!("the decoder must accept {wire}: {err}"))
}

/// The overlay a `launch_overlay` answer carries, as JSON; `None` for any other answer.
fn answered_overlay(answer: &DaemonMessage) -> Option<serde_json::Value> {
    let wire = serde_json::to_value(answer).expect("a daemon message serializes");
    if wire["type"] != "launch_overlay" {
        return None;
    }
    let text = wire["overlay"].as_str().expect("the overlay is a string");
    Some(
        serde_json::from_str(text)
            .unwrap_or_else(|err| panic!("the overlay is a JSON object: {err}; got {text:?}")),
    )
}

/// What the daemon answers a request for the overlay of `window_id` on a new connection.
async fn ask_overlay(endpoint: &str, message: &ClientMessage) -> DaemonMessage {
    let mut client = yagura_test_support::ipc::connect_with_retry(endpoint).await;
    send(&mut client, message).await;
    read_response(&mut client).await
}

/// The overlay the daemon builds for a window in another environment, from the facts its
/// guest sends with the window's credentials, is the one a window run directly gets for
/// the same facts. A request without the window's credentials is refused and builds
/// nothing; once the overlay was built, the window is spoken for.
#[tokio::test]
async fn the_overlay_built_for_a_guest_is_the_one_a_direct_window_gets_for_the_same_facts() {
    const INLINE: &str = r#"{"models":{"default_reasoning_effort":"high"},"shell_environment_policy":{"exclude":["BETA_*"]}}"#;
    const PROBE: &str = "overlay-ipc-direct";
    let (tree, profiles_dir) = overlay_profiles("cred-overlay-ipc");
    let probe = env_probe_path().display().to_string();
    common::write_profile(
        &profiles_dir,
        PROBE,
        &shipped_with_command(
            PROBE,
            "grok-build",
            &[probe, OVERLAY_VAR.to_string()],
            None,
            &[],
        ),
    );
    let h = start_overlay_daemon(
        "cred-overlay-ipc",
        &tree,
        profiles_dir,
        &[("GROK_CONFIG", INLINE)],
    )
    .await;
    let mut client = h.connect().await;
    let direct = open_on(&mut client, PROBE).await;
    let expected = reported_overlay_in(&h, &direct).await;
    let guest = open_on(&mut client, GUEST).await;
    let facts = serde_json::json!({ "GROK_CONFIG": INLINE });

    let refused = ask_overlay(
        &h.endpoint,
        &build_launch_overlay(&guest, &"f".repeat(32), facts.clone()),
    )
    .await;
    assert!(
        matches!(refused, DaemonMessage::Error { .. }),
        "a request without the window's credentials is refused, got {refused:?}"
    );
    let (answers, _) = claims_at(&h.endpoint, &guest, GUEST, VALID_TOKEN, 1).await;
    assert!(
        refused_as_pending(&answers[0]),
        "a refused request builds nothing, got {:?}",
        answers[0]
    );

    let answer = ask_overlay(
        &h.endpoint,
        &build_launch_overlay(&guest, VALID_TOKEN, facts),
    )
    .await;
    let built = answered_overlay(&answer)
        .unwrap_or_else(|| panic!("the daemon answers with the overlay, got {answer:?}"));
    assert_eq!(
        built, expected,
        "the guest's overlay is the one a direct window gets for the same facts"
    );
    let (answers, _) = claims_at(&h.endpoint, &guest, GUEST, VALID_TOKEN, 1).await;
    assert!(
        matches!(answers[0], DaemonMessage::Identified { .. }),
        "the window is spoken for once its overlay was built, got {:?}",
        answers[0]
    );
    h.registry.terminate_all().await;
}

/// Runs the overlay door with the credentials `credentials` in its environment and `input`
/// on its stdin, through a relay to `h`'s daemon.
async fn run_overlay_door(
    h: &Harness,
    credentials: (&str, &str),
    input: &[u8],
) -> std::process::Output {
    relay_door_with(
        h,
        &unique_instance(),
        Some(credentials),
        &["--launch-overlay"],
        input.to_vec(),
        Printed::Forward,
    )
    .await
    .finish_dropping_upstream()
    .await
    .0
}

/// Whether the door failed the way a statement can tell: a non-zero end, nothing on
/// stdout, and one line on stderr that holds no credential.
pub(crate) fn assert_door_failed(output: &std::process::Output, what: &str, token: &str) {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "{what}: the door must end non-zero; stdout: {stdout:?} stderr: {stderr:?}"
    );
    assert!(stdout.is_empty(), "{what}: nothing on stdout: {stdout:?}");
    assert_eq!(stderr.lines().count(), 1, "{what}: one line: {stderr:?}");
    assert!(!stderr.contains(token), "{what}: no credential: {stderr:?}");
}

/// The overlay door ends non-zero with one line on stderr when its daemon answers with an
/// error (credentials it does not take) and when its stdin holds more than the door reads
/// (more than an IPC frame may carry): neither builds the window's overlay, which stays
/// refused as pending.
#[tokio::test]
async fn the_overlay_door_fails_loudly_on_a_refusal_and_on_too_much_input_and_builds_nothing() {
    let (tree, profiles_dir) = overlay_profiles("cred-overlay-door");
    let h = start_overlay_daemon("cred-overlay-door", &tree, profiles_dir, &[]).await;
    let mut client = h.connect().await;
    let guest = open_on(&mut client, GUEST).await;
    let wrong = "f".repeat(32);

    let refused = run_overlay_door(&h, (&guest, &wrong), b"").await;
    assert_door_failed(&refused, "a daemon that refuses the credentials", &wrong);
    let too_much = vec![b'x'; yagura_protocol::MAX_FRAME_BODY_LEN + 1];
    let overflowed = run_overlay_door(&h, (&guest, VALID_TOKEN), &too_much).await;
    assert_door_failed(&overflowed, "more input than the door reads", VALID_TOKEN);

    let (answers, _) = claims_at(&h.endpoint, &guest, GUEST, VALID_TOKEN, 1).await;
    assert!(
        refused_as_pending(&answers[0]),
        "neither door built the overlay, got {:?}",
        answers[0]
    );
    h.registry.terminate_all().await;
}
