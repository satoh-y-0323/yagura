#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration tests: what a profile's `[hooks]` table may say, and what the
//! profiles this repository ships say.
//!
//! An invalid table takes its profile out of the listing with one warning,
//! the same rule `[mcp]` follows. The shipped Claude Code profiles must map
//! Claude Code's hook events to status reports as the CLI's settings format
//! expects, each door handed the window's credentials file: that is an
//! external contract, so the shipped files are read as data and their JSON is
//! decoded.

use crate::common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use common::docs::read_doc_verbatim;
use common::{Harness, empty_lookup, launch_args_logged_in, read_response, send, write_profile};
use yagura_daemon::DaemonConfig;
use yagura_daemon::profile::{ProfileOs, parse_profile};
use yagura_protocol::{ClientMessage, DaemonMessage, ProfileInfo};
use yagura_test_support::ipc::Connection;
use yagura_test_support::paths::TempDir;
use yagura_test_support::process::{
    COMPOSER_CONTINUATION_PREFIX, COMPOSER_MARKER, COMPOSER_SEPARATOR,
};

fn is_file(path: &Path) -> bool {
    path.is_file()
}

fn daemon_config(profiles_dir: PathBuf, config_dir: Option<&Path>) -> DaemonConfig {
    DaemonConfig {
        profiles_dir: Some(profiles_dir),
        host_os: ProfileOs::host(),
        program_lookup: empty_lookup(),
        program_is_executable: is_file,
        default_shell: None,
        mcp_config_dir: config_dir.map(Path::to_path_buf),
        ..common::approver_base_config()
    }
}

async fn call_list_profiles(client: &mut Connection) -> (Vec<ProfileInfo>, Vec<String>) {
    send(client, &ClientMessage::ListProfiles).await;
    match read_response(client).await {
        DaemonMessage::ProfileList { profiles, warnings } => (profiles, warnings),
        other => panic!("expected profile_list, got {other:?}"),
    }
}

// =============================================================================
// Rejection of an invalid [hooks] table
// =============================================================================

#[tokio::test]
async fn an_invalid_hooks_table_takes_its_profile_out_of_the_listing_with_one_warning() {
    let tree = TempDir::new("yagura-cli-test", "profile-hooks-invalid");
    let profiles_dir = tree.make_dir("profiles");
    let head = "schema = 1\ndisplay_name = \"X\"\n\n[command]\ndefault = [\"some-program\"]\n";
    write_profile(
        &profiles_dir,
        "hooks-ok",
        &format!(
            "{head}\n[hooks]\nconfig_file = \"{{}}\"\nargs = [\"--s\", \"{{hooks_config_file}}\"]\n"
        ),
    );
    let invalid = [
        ("hooks-no-args", "[hooks]\nconfig_file = \"x\"\n"),
        (
            "hooks-unknown-key",
            "[hooks]\nargs = [\"--x\"]\nbogus = 1\n",
        ),
        (
            "hooks-unknown-placeholder",
            "[hooks]\nargs = [\"--x\", \"{nope}\"]\n",
        ),
        (
            "hooks-file-placeholder-without-file",
            "[hooks]\nargs = [\"--s\", \"{hooks_config_file}\"]\n",
        ),
    ];
    for (id, table) in invalid {
        write_profile(&profiles_dir, id, &format!("{head}\n{table}"));
    }
    let h = Harness::start("profile-hooks-invalid", daemon_config(profiles_dir, None)).await;
    let mut client = h.connect().await;

    let (profiles, warnings) = call_list_profiles(&mut client).await;

    assert!(
        profiles.iter().any(|p| p.id == "hooks-ok"),
        "a valid [hooks] table must leave its profile listed: {profiles:?}"
    );
    for (id, _) in invalid {
        assert!(
            !profiles.iter().any(|p| p.id == id),
            "{id} must be left out of the listing: {profiles:?}"
        );
        let naming: Vec<_> = warnings.iter().filter(|w| w.contains(id)).collect();
        assert_eq!(
            naming.len(),
            1,
            "{id} must produce exactly one warning: {warnings:?}"
        );
    }
}

// =============================================================================
// The shipped Claude Code profiles
// =============================================================================

fn shipped_table(id: &str, name: &str) -> toml::Table {
    let source = read_doc_verbatim(&format!("../../profiles/{id}.toml"));
    let profile = parse_profile(id, &source)
        .unwrap_or_else(|err| panic!("bundled profile {id} must still parse: {err}"));
    let mut root: toml::Table = source.parse().expect("bundled profile is TOML");
    let mut table = match root.remove(name) {
        Some(toml::Value::Table(table)) => table,
        other => panic!("{id} must ship a [{name}] table, got {other:?}"),
    };
    // The texts as the profile resolves them (its time placeholders filled), not as written.
    if let (Some(spec), "hooks") = (&profile.hooks, name) {
        table.insert(
            "args".to_string(),
            toml::Value::Array(spec.args.iter().cloned().map(toml::Value::String).collect()),
        );
        if let Some(content) = &spec.config_file {
            table.insert(
                "config_file".to_string(),
                toml::Value::String(content.clone()),
            );
        }
    }
    table
}

fn shipped_hooks_table(id: &str) -> toml::Table {
    shipped_table(id, "hooks")
}

/// The placeholder the shipped hook commands carry for the window's credentials file.
const CREDENTIALS_FILE: &str = "{credentials_file}";

/// A hook command running `program` with `door`, handed the credentials file
/// single-quoted, since the CLI runs the command through a shell.
fn hook_command(program: &str, credentials: &str, door: &str) -> String {
    format!("{program} --credentials '{credentials}' {door}")
}

/// Every hook of an event in a Claude Code `hooks` settings document, across all of its
/// groups, checking the document's shape on the way.
fn hooks_by_event(document: &str) -> BTreeMap<String, Vec<serde_json::Value>> {
    let value: serde_json::Value =
        serde_json::from_str(document).expect("the hooks settings must be valid JSON");
    let hooks = value["hooks"]
        .as_object()
        .expect("a top-level hooks object");
    assert_eq!(
        value.as_object().unwrap().len(),
        1,
        "only the hooks key: {value}"
    );
    let mut out = BTreeMap::new();
    for (event, groups) in hooks {
        let mut found = Vec::new();
        for group in groups
            .as_array()
            .expect("an event maps to an array of groups")
        {
            for hook in group["hooks"].as_array().expect("a group carries hooks") {
                assert_eq!(hook["type"], "command", "{event}: {hook}");
                assert!(hook["command"].is_string(), "{event}: {hook}");
                found.push(hook.clone());
            }
        }
        out.insert(event.clone(), found);
    }
    out
}

/// Each event's hook commands, sorted so the order the groups are written in is not part of
/// the contract.
fn commands_by_event(document: &str) -> BTreeMap<String, Vec<String>> {
    hooks_by_event(document)
        .into_iter()
        .map(|(event, hooks)| {
            let mut commands: Vec<String> = hooks
                .iter()
                .map(|hook| hook["command"].as_str().unwrap().to_string())
                .collect();
            commands.sort();
            (event, commands)
        })
        .collect()
}

/// The hooks the shipped profiles inject: the status reports, the three doors a window's
/// CLI reads its inbox through (start-up context, the start of a turn, and a wait that the
/// daemon answers when something arrives for a waiting CLI), the wait its wake file starts
/// again, and the answers that hand the CLI the wake file to watch without asking the
/// daemon (at start-up, and again whenever the CLI changes directory).
fn expected_commands(program: &str, credentials: &str) -> BTreeMap<String, Vec<String>> {
    let report = |status: &str| {
        hook_command(
            program,
            credentials,
            &format!("--emit-status {status} --source claude-code --skip-if /agent_id"),
        )
    };
    let door = |name: &str| hook_command(program, credentials, &format!("--deliver-{name}"));
    let answer =
        |event: &str| hook_command(program, credentials, &format!("--print-answer {event}"));
    let mut expected: BTreeMap<String, Vec<String>> = [
        ("PreToolUse", vec![report("running")]),
        ("PostToolUse", vec![report("running")]),
        ("PermissionRequest", vec![report("waiting_approval")]),
        ("StopFailure", vec![report("waiting_input")]),
        ("UserPromptSubmit", vec![report("running"), door("take")]),
        (
            "SessionStart",
            vec![door("context"), door("wait"), answer("SessionStart")],
        ),
        ("CwdChanged", vec![answer("CwdChanged")]),
        ("FileChanged", vec![door("wait")]),
        ("Stop", vec![report("waiting_input"), door("wait")]),
    ]
    .into_iter()
    .map(|(event, commands)| (event.to_string(), commands))
    .collect();
    for commands in expected.values_mut() {
        commands.sort();
    }
    expected
}

#[test]
fn claude_code_ships_hooks_that_report_turn_start_approval_and_end() {
    let hooks = shipped_hooks_table("claude-code");
    let args: Vec<&str> = hooks["args"]
        .as_array()
        .expect("args")
        .iter()
        .map(|v| v.as_str().expect("string arg"))
        .collect();
    assert_eq!(args, ["--settings", "{hooks_config_file}"]);
    let document = hooks["config_file"].as_str().expect("config_file");
    assert_eq!(
        commands_by_event(document),
        expected_commands("yagura-daemon", CREDENTIALS_FILE)
    );
}

#[test]
fn wsl_claude_code_ships_the_same_mapping_calling_the_windows_executable() {
    let hooks = shipped_hooks_table("wsl-claude-code");
    assert!(
        hooks.get("config_file").is_none(),
        "a Windows path is unreadable from the WSL CLI, so the JSON must be inline"
    );
    let args: Vec<&str> = hooks["args"]
        .as_array()
        .expect("args")
        .iter()
        .map(|v| v.as_str().expect("string arg"))
        .collect();
    assert_eq!(args.len(), 2, "{args:?}");
    assert_eq!(args[0], "--settings");
    assert_eq!(
        commands_by_event(args[1]),
        expected_commands("yagura-daemon.exe", CREDENTIALS_FILE)
    );
}

/// The settings document each shipped Claude Code profile injects: a file for the host
/// profile, inline in the arguments for the WSL one.
fn shipped_documents() -> [(&'static str, &'static str, String); 2] {
    let host = shipped_hooks_table("claude-code");
    let wsl = shipped_hooks_table("wsl-claude-code");
    [
        (
            "claude-code",
            "yagura-daemon",
            host["config_file"]
                .as_str()
                .expect("config_file")
                .to_string(),
        ),
        (
            "wsl-claude-code",
            "yagura-daemon.exe",
            wsl["args"].as_array().expect("args")[1]
                .as_str()
                .expect("inline settings")
                .to_string(),
        ),
    ]
}

/// The profile's wake timeout, as the hook that waits must carry it.
const WAKE_TIMEOUT_SECS: u64 = 86_400;

/// The file the daemon writes to ask a window's CLI to wait again: the group that starts the
/// wait on its change matches it alone.
const WAKE_FILE_MATCHER: &str = "yagura_wake";

#[test]
fn the_hook_that_waits_for_the_daemon_is_asynchronous_and_outlives_a_long_idle() {
    for (id, program, document) in shipped_documents() {
        let wait = hook_command(program, CREDENTIALS_FILE, "--deliver-wait");
        let by_event = hooks_by_event(&document);
        for event in ["SessionStart", "Stop", "FileChanged"] {
            let hooks = by_event
                .get(event)
                .unwrap_or_else(|| panic!("{id}: {event} ships no hook: {by_event:?}"));
            let waits: Vec<_> = hooks
                .iter()
                .filter(|hook| hook["command"] == wait.as_str())
                .collect();
            assert_eq!(waits.len(), 1, "{id}: {event} carries one wait: {hooks:?}");
            let hook = waits[0];
            assert_eq!(
                hook["asyncRewake"],
                serde_json::Value::Bool(true),
                "{id}: {event}: Claude Code wakes an idle session from a background hook that                  exits with 2: {hook}"
            );
            assert_eq!(
                hook["timeout"].as_u64(),
                Some(WAKE_TIMEOUT_SECS),
                "{id}: {event}: the hook outlives the default timeout: {hook}"
            );
        }
        let settings: serde_json::Value =
            serde_json::from_str(&document).expect("the hooks settings must be valid JSON");
        let matchers: Vec<&serde_json::Value> = settings["hooks"]["FileChanged"]
            .as_array()
            .unwrap_or_else(|| panic!("{id}: FileChanged maps to an array of groups"))
            .iter()
            .filter(|group| {
                group["hooks"]
                    .as_array()
                    .is_some_and(|hooks| hooks.iter().any(|hook| hook["command"] == wait.as_str()))
            })
            .map(|group| &group["matcher"])
            .collect();
        assert_eq!(
            matchers,
            [&serde_json::Value::String(WAKE_FILE_MATCHER.to_string())],
            "{id}: the wait a changed file starts is started by the wake file alone"
        );
    }
}

#[test]
fn the_hooks_that_answer_at_once_are_not_asynchronous() {
    for (id, program, document) in shipped_documents() {
        let wait = hook_command(program, CREDENTIALS_FILE, "--deliver-wait");
        let doors = [
            hook_command(program, CREDENTIALS_FILE, "--deliver-context"),
            hook_command(program, CREDENTIALS_FILE, "--deliver-take"),
        ];
        let mut doors_seen = 0;
        for (event, hooks) in hooks_by_event(&document) {
            for hook in hooks {
                if hook["command"] == wait.as_str() {
                    continue;
                }
                doors_seen += doors
                    .iter()
                    .filter(|door| hook["command"] == door.as_str())
                    .count();
                assert!(
                    hook.get("asyncRewake").is_none()
                        && hook.get("async") != Some(&serde_json::Value::Bool(true)),
                    "{id}: {event}: a hook whose output Claude Code reads must run in the                      foreground: {hook}"
                );
            }
        }
        assert_eq!(
            doors_seen, 2,
            "{id}: both doors that print into the session are shipped"
        );
    }
}

/// A profile running `hook_child`, which logs its argv under `logs` and stays alive (a
/// launch window's settings go with its child), followed by `tables`.
fn hook_child_profile_source(logs: &Path, tables: &str) -> String {
    let command = [
        env!("CARGO_BIN_EXE_hook_child").to_string(),
        COMPOSER_MARKER.to_string(),
        COMPOSER_SEPARATOR.to_string(),
        COMPOSER_CONTINUATION_PREFIX.to_string(),
        "0".to_string(),
        "-".to_string(),
        "--log".to_string(),
        logs.display().to_string(),
    ]
    .iter()
    .map(|element| format!("'{element}'"))
    .collect::<Vec<_>>()
    .join(", ");
    format!(
        "schema = 1\ndisplay_name = \"Hook Child\"\n\n[command]\ndefault = [{command}]\n{tables}"
    )
}

/// The credentials file `command` hands its door, single-quoted after the program.
fn quoted_credentials_file(command: &str) -> &str {
    command
        .strip_prefix("yagura-daemon --credentials '")
        .and_then(|rest| rest.split_once('\''))
        .map(|(location, _)| location)
        .unwrap_or_else(|| {
            panic!("the command must hand a single-quoted credentials file: {command}")
        })
}

#[tokio::test]
async fn the_shipped_claude_code_hooks_table_is_written_out_when_a_window_opens() {
    let tree = TempDir::new("yagura-cli-test", "profile-hooks-shipped-live");
    let profiles_dir = tree.make_dir("profiles");
    let config_dir = tree.make_dir("mcp");
    let logs = tree.make_dir("logs");
    let mut root = toml::Table::new();
    root.insert(
        "credentials".to_string(),
        toml::Value::Table(shipped_table("claude-code", "credentials")),
    );
    root.insert(
        "hooks".to_string(),
        toml::Value::Table(shipped_hooks_table("claude-code")),
    );
    write_profile(
        &profiles_dir,
        "hook-child",
        &hook_child_profile_source(&logs, &format!("\n{}", toml::to_string(&root).unwrap())),
    );
    let h = Harness::start(
        "profile-hooks-shipped-live",
        daemon_config(profiles_dir, Some(&config_dir)),
    )
    .await;
    let mut client = h.connect().await;
    send(
        &mut client,
        &ClientMessage::OpenWindowFromProfile {
            profile: "hook-child".to_string(),
            cwd: None,
            name: None,
            model: None,
            effort: None,
            on_name_taken: Default::default(),
        },
    )
    .await;
    match read_response(&mut client).await {
        DaemonMessage::WindowOpened { .. } => {}
        other => panic!("expected window_opened, got {other:?}"),
    }
    let args = launch_args_logged_in(&logs).await;
    let at = args
        .iter()
        .position(|arg| arg == "--settings")
        .unwrap_or_else(|| panic!("the window was not handed --settings; argv: {args:?}"));
    let written = std::fs::read_to_string(&args[at + 1])
        .unwrap_or_else(|err| panic!("the settings file must exist: {err}"));

    let by_event = commands_by_event(&written);
    let mut locations: Vec<&str> = by_event
        .values()
        .flatten()
        .map(|command| quoted_credentials_file(command))
        .collect();
    locations.sort_unstable();
    locations.dedup();
    assert_eq!(
        locations.len(),
        1,
        "every command must name the window's one credentials file: {locations:?}"
    );
    assert!(
        Path::new(locations[0]).is_file(),
        "the credentials file the commands name must exist: {}",
        locations[0]
    );
    assert_eq!(by_event, expected_commands("yagura-daemon", locations[0]));
    h.registry.terminate_all().await;
}

#[test]
fn profiles_without_a_hooks_table_keep_none() {
    for id in ["shell", "antigravity-cli", "wsl-antigravity-cli"] {
        let source = read_doc_verbatim(&format!("../../profiles/{id}.toml"));
        let root: toml::Table = source.parse().expect("bundled profile is TOML");
        assert!(
            root.get("hooks").is_none(),
            "{id} must not ship a [hooks] table"
        );
    }
}
