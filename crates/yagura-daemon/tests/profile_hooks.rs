#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration tests: what a profile's `[hooks]` table may say, and what the
//! profiles this repository ships say.
//!
//! An invalid table takes its profile out of the listing with one warning,
//! the same rule `[mcp]` follows. The shipped Claude Code profiles must map
//! Claude Code's hook events to status reports as the CLI's settings format
//! expects: that is an external contract, so the shipped files are read as
//! data and their JSON is decoded.

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use common::docs::read_doc_verbatim;
use common::{Harness, empty_lookup, read_response, send, write_profile};
use yagura_daemon::DaemonConfig;
use yagura_daemon::profile::{ProfileOs, parse_profile};
use yagura_protocol::{ClientMessage, DaemonMessage, ProfileInfo};
use yagura_test_support::ipc::Connection;
use yagura_test_support::paths::TempDir;
use yagura_test_support::process::argv_probe_profile_source;

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
    match read_response(client, Duration::from_secs(5)).await {
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

fn shipped_hooks_table(id: &str) -> toml::Table {
    let source = read_doc_verbatim(&format!("../../profiles/{id}.toml"));
    parse_profile(id, &source)
        .unwrap_or_else(|err| panic!("bundled profile {id} must still parse: {err}"));
    let mut root: toml::Table = source.parse().expect("bundled profile is TOML");
    match root.remove("hooks") {
        Some(toml::Value::Table(table)) => table,
        other => panic!("{id} must ship a [hooks] table, got {other:?}"),
    }
}

/// Each event's hook commands in a Claude Code `hooks` settings document,
/// checking the document's shape on the way.
fn commands_by_event(document: &str) -> BTreeMap<String, Vec<String>> {
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
        let mut commands = Vec::new();
        for group in groups
            .as_array()
            .expect("an event maps to an array of groups")
        {
            for hook in group["hooks"].as_array().expect("a group carries hooks") {
                assert_eq!(hook["type"], "command", "{event}: {hook}");
                commands.push(
                    hook["command"]
                        .as_str()
                        .expect("a command string")
                        .to_string(),
                );
            }
        }
        out.insert(event.clone(), commands);
    }
    out
}

fn expected_commands(program: &str) -> BTreeMap<String, Vec<String>> {
    [
        ("UserPromptSubmit", "running"),
        ("PreToolUse", "running"),
        ("PostToolUse", "running"),
        ("PermissionRequest", "waiting_approval"),
        ("Stop", "waiting_input"),
        ("StopFailure", "waiting_input"),
    ]
    .into_iter()
    .map(|(event, status)| {
        (
            event.to_string(),
            vec![format!(
                "{program} --emit-status {status} --source claude-code"
            )],
        )
    })
    .collect()
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
        expected_commands("yagura-daemon")
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
        expected_commands("yagura-daemon.exe")
    );
}

#[tokio::test]
async fn the_shipped_claude_code_hooks_table_is_written_out_when_a_window_opens() {
    let tree = TempDir::new("yagura-cli-test", "profile-hooks-shipped-live");
    let profiles_dir = tree.make_dir("profiles");
    let config_dir = tree.make_dir("mcp");
    let hooks = shipped_hooks_table("claude-code");
    let mut root = toml::Table::new();
    root.insert("hooks".to_string(), toml::Value::Table(hooks));
    write_profile(
        &profiles_dir,
        "argv-probe",
        &argv_probe_profile_source(
            "Argv Probe",
            &format!("\n{}", toml::to_string(&root).unwrap()),
        ),
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
            profile: "argv-probe".to_string(),
            cwd: None,
            name: None,
            model: None,
            effort: None,
        },
    )
    .await;
    let window_id = match read_response(&mut client, Duration::from_secs(5)).await {
        DaemonMessage::WindowOpened { window_id, .. } => window_id,
        other => panic!("expected window_opened, got {other:?}"),
    };
    h.wait_for_scrollback(&window_id, b"<<argv[2]=", Duration::from_secs(10))
        .await;
    let text = String::from_utf8_lossy(&h.scrollback(&window_id)).into_owned();
    assert!(text.contains("<<argv[1]=--settings>>"), "{text:?}");
    let start = text.rfind("<<argv[2]=").unwrap() + "<<argv[2]=".len();
    let end = text[start..].find(">>").unwrap() + start;
    let written = std::fs::read_to_string(&text[start..end])
        .unwrap_or_else(|err| panic!("the settings file must exist: {err}"));
    assert_eq!(
        commands_by_event(&written),
        expected_commands("yagura-daemon")
    );
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
