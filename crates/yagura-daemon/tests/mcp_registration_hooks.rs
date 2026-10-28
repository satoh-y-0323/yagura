#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration tests: registering the Antigravity CLI also writes yagura's
//! status-report hooks into the user's `~/.gemini/config/hooks.json`.
//!
//! The CLI takes no hooks at launch, so the registration owns one top-level
//! entry, `yagura`, of that file: it is added by `Register`, taken out by
//! `Remove`, read back by the status check, and a window is expected to
//! report only while the entry is complete. Every case runs the shipped
//! profile with its launch command swapped for the `mcp_cli_stub` fixture and
//! `home_dir` at a temporary directory, so the user's own `~/.gemini` is never
//! read or written.

mod common;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use common::docs::read_doc_verbatim;
use common::{
    Harness, approver_base_config, empty_lookup, mcp_cli_stub_path, read_response,
    register_approver, send, unique_temp_dir, write_profile,
};
use serde_json::{Value, json};
use yagura_daemon::DaemonConfig;
use yagura_daemon::config::REPORT_EXE_NAME;
use yagura_daemon::profile::ProfileOs;
use yagura_os::program::contains_cmd_metacharacter;
use yagura_protocol::{
    ClientMessage, DaemonMessage, McpRegistrationAction, McpRegistrationState,
    McpRegistrationStatusInfo,
};
use yagura_test_support::ipc::Connection;

const REPLY_TIMEOUT: Duration = Duration::from_secs(20);

const HOST_PROFILE: &str = "antigravity-cli";
const WSL_PROFILE: &str = "wsl-antigravity-cli";

const HOOKS_FILE: &str = ".gemini/config/hooks.json";

/// What every door of the entry carries after the daemon path: the default instance's
/// registration name, which the cases register under.
const FOR_INSTANCE: &str = " --for-instance yagura";
const MCP_FILE: &str = ".gemini/config/mcp_config.json";

/// How long a stubbed CLI stays alive when a case needs a live window: a
/// window that has exited no longer has a detector to ask.
const WINDOW_ALIVE_MS: &str = "8000";

/// The vocabulary of yagura's own entry. A user's entries in the seeds below
/// never use these names, so they can be told apart by key alone.
const YAGURA_KEYS: [&str; 10] = [
    "yagura",
    "PreInvocation",
    "SessionStart",
    "PostToolUse",
    "matcher",
    "hooks",
    "type",
    "command",
    "timeout",
    "enabled",
];

// =============================================================================
// Fixture: the shipped profile with its launch command swapped for the stub
// =============================================================================

struct Layout {
    profiles_dir: PathBuf,
    home_dir: PathBuf,
    state_dir: PathBuf,
    mcp_config_dir: PathBuf,
    report_dir: PathBuf,
    bridge: PathBuf,
}

impl Layout {
    fn new(tag: &str) -> Self {
        let root = unique_temp_dir(tag);
        let layout = Self {
            profiles_dir: root.join("profiles"),
            home_dir: root.join("home"),
            state_dir: root.join("stub-state"),
            mcp_config_dir: root.join("mcp"),
            report_dir: root.join("state").join("bin"),
            bridge: root.join("bridge").join(if cfg!(windows) {
                "yagura-mcp.exe"
            } else {
                "yagura-mcp"
            }),
        };
        for dir in [
            &layout.profiles_dir,
            &layout.home_dir,
            &layout.state_dir,
            &layout.mcp_config_dir,
        ] {
            std::fs::create_dir_all(dir).expect("create fixture dir");
        }
        std::fs::write(layout.state_dir.join("registry.json"), "{}").expect("seed registry");
        layout
    }

    fn hooks_path(&self) -> PathBuf {
        self.home_dir.join(HOOKS_FILE)
    }

    fn mcp_path(&self) -> PathBuf {
        self.home_dir.join(MCP_FILE)
    }

    fn write_hooks(&self, text: &str) {
        let path = self.hooks_path();
        std::fs::create_dir_all(path.parent().unwrap()).expect("create config dir");
        std::fs::write(path, text).expect("write hooks file");
    }

    fn read_hooks(&self) -> String {
        std::fs::read_to_string(self.hooks_path()).expect("read hooks file")
    }

    fn hooks_value(&self) -> Value {
        parse(&self.read_hooks())
    }

    /// The command the daemon is expected to write for `profile`: the report
    /// folder's daemon, as the CLI's own operating system sees the path.
    fn expected_command(&self, profile: &str) -> String {
        let path = self.report_dir.join(REPORT_EXE_NAME);
        let path = path.to_str().expect("report path is UTF-8");
        let path = if profile == WSL_PROFILE {
            // The stub's `probe` stands in for `wslpath -u`.
            format!("/probed{}", path.replace('\\', "/"))
        } else {
            path.to_string()
        };
        format!("{path}{FOR_INSTANCE} --emit-status running --source antigravity-cli")
    }
}

/// The daemon path `command` (a running report) starts, to build the commands of the doors
/// the same entry reads the inbox through.
fn door_command(command: &str, door: &str) -> String {
    let program = command
        .strip_suffix(" --emit-status running --source antigravity-cli")
        .unwrap_or_else(|| panic!("{command:?} is the running report"));
    format!("{program} --deliver-{door}")
}

fn str_array(items: &[&str]) -> toml::Value {
    toml::Value::Array(
        items
            .iter()
            .map(|item| toml::Value::String((*item).to_string()))
            .collect(),
    )
}

/// The shipped `profile` with only the parts that would reach the real CLI
/// replaced: the launch command, and for the WSL row the path conversion and file
/// tools. `alive_ms` keeps every stub call (and so a launched window) alive
/// that long before it does anything.
fn fixture_profile(profile: &str, layout: &Layout, alive_ms: Option<&str>) -> String {
    let stub = mcp_cli_stub_path();
    let stub = stub.to_str().expect("stub path is UTF-8");
    let home = layout.home_dir.to_str().expect("home is UTF-8");
    let mut root: toml::Table = read_doc_verbatim(&format!("../../profiles/{profile}.toml"))
        .parse()
        .expect("the shipped profile is TOML");
    let mut command = toml::Table::new();
    command.insert("default".into(), str_array(&[stub]));
    root.insert("command".into(), toml::Value::Table(command));

    if profile == WSL_PROFILE {
        root.get("mcp_registration")
            .and_then(toml::Value::as_table)
            .expect("the shipped profile carries a registration table");
        let shipped_tools = root
            .get("host_paths")
            .and_then(toml::Value::as_table)
            .filter(|tools| tools.contains_key("read"))
            .expect("the WSL row edits its settings through file tools");
        let mut host_paths = toml::Table::new();
        if let Some(absent_exit) = shipped_tools.get("absent_exit") {
            host_paths.insert("absent_exit".into(), absent_exit.clone());
        }
        host_paths.insert("convert".into(), str_array(&[stub, "probe", "{path}"]));
        let file_arg = format!("{home}/{{file}}");
        host_paths.insert(
            "read".into(),
            str_array(&[stub, "file-read", file_arg.as_str()]),
        );
        host_paths.insert(
            "replace".into(),
            str_array(&[stub, "file-replace", file_arg.as_str()]),
        );
        root.insert("host_paths".into(), toml::Value::Table(host_paths));
    }

    let mut env = toml::Table::new();
    let mcp_path = layout.mcp_path();
    let mut pairs = vec![
        (
            "MCP_CLI_STUB_STATE_DIR",
            layout.state_dir.to_str().expect("UTF-8").to_string(),
        ),
        (
            "MCP_CLI_STUB_JSON_CONFIG_PATH",
            mcp_path.to_str().expect("UTF-8").to_string(),
        ),
    ];
    if let Some(ms) = alive_ms {
        pairs.push(("MCP_CLI_STUB_DELAY_MS", ms.to_string()));
    }
    for (key, value) in pairs {
        env.insert(key.into(), toml::Value::String(value));
    }
    root.insert("env".into(), toml::Value::Table(env));
    toml::to_string(&root).expect("serialize the fixture profile")
}

fn is_file(path: &Path) -> bool {
    path.is_file()
}

fn place_profile(profile: &str, layout: &Layout, alive_ms: Option<&str>) {
    write_profile(
        &layout.profiles_dir,
        "agy",
        &fixture_profile(profile, layout, alive_ms),
    );
}

async fn start(tag: &str, profile: &str, layout: &Layout) -> Harness {
    start_with(tag, profile, layout, hooks_config(layout)).await
}

async fn start_with(tag: &str, profile: &str, layout: &Layout, config: DaemonConfig) -> Harness {
    place_profile(profile, layout, None);
    Harness::start(tag, config).await
}

fn hooks_config(layout: &Layout) -> DaemonConfig {
    DaemonConfig {
        profiles_dir: Some(layout.profiles_dir.clone()),
        home_dir: Some(layout.home_dir.clone()),
        instance_id: None,
        mcp_bridge: Some(layout.bridge.clone()),
        mcp_config_dir: Some(layout.mcp_config_dir.clone()),
        report_dir: Some(layout.report_dir.clone()),
        mcp_registration_timeout: Duration::from_secs(20),
        mcp_registration_max_output_bytes: 1 << 20,
        mcp_registration_status_max_concurrent: 2,
        host_os: ProfileOs::host(),
        program_lookup: empty_lookup(),
        program_is_executable: is_file,
        default_shell: None,
        ..approver_base_config()
    }
}

async fn approver_connection(h: &Harness) -> Connection {
    let mut client = h.connect().await;
    register_approver(&mut client).await;
    client
}

/// Runs a registration change and returns its result's error and status.
async fn run_action(
    client: &mut Connection,
    action: McpRegistrationAction,
    request_id: u64,
) -> (Option<String>, McpRegistrationStatusInfo) {
    send(
        client,
        &ClientMessage::RunMcpRegistration {
            profile: "agy".to_string(),
            action,
            request_id,
        },
    )
    .await;
    match read_response(client, REPLY_TIMEOUT).await {
        DaemonMessage::McpRegistrationRequested { .. } => {}
        other => panic!("expected mcp_registration_requested, got {other:?}"),
    }
    match read_response(client, REPLY_TIMEOUT).await {
        DaemonMessage::McpRegistrationResult { error, status, .. } => (error, status),
        other => panic!("expected mcp_registration_result, got {other:?}"),
    }
}

/// The state a fresh listing reports for the one profile.
async fn listed_state(client: &mut Connection, request_id: u64) -> McpRegistrationState {
    listed_status(client, request_id).await.state
}

/// The status a fresh listing pushes for the one profile.
async fn listed_status(client: &mut Connection, request_id: u64) -> McpRegistrationStatusInfo {
    send(client, &ClientMessage::ListMcpRegistrations { request_id }).await;
    match read_response(client, REPLY_TIMEOUT).await {
        DaemonMessage::McpRegistrationsListed { rows, .. } => {
            assert_eq!(rows.len(), 1, "{rows:?}");
        }
        other => panic!("expected mcp_registrations_listed, got {other:?}"),
    }
    match read_response(client, REPLY_TIMEOUT).await {
        DaemonMessage::McpRegistrationStatus { status, .. } => status,
        other => panic!("expected mcp_registration_status, got {other:?}"),
    }
}

// =============================================================================
// Reading the edited file
// =============================================================================

/// A user's hooks file: keys deliberately neither sorted nor grouped, so an
/// edit that rebuilds the document through a sorted map is visible.
const SEED_HOOKS: &str = "{\n\
  \"zeta\": {\"on_stop\": [{\"run\": \"/bin/zeta\", \"wait\": 5}], \"active\": false},\n\
  \"alpha\": {\"b_second\": 1, \"a_first\": [3, 2, 1]},\n\
  \"mid\": \"plain\"\n\
}\n";

fn parse(text: &str) -> Value {
    serde_json::from_str(text)
        .unwrap_or_else(|err| panic!("the file must stay valid JSON: {err}\n{text}"))
}

/// Every object key of `text`, in the order they are written. serde_json's
/// map here is sorted, so only the text itself can show an order.
fn key_sequence(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut keys = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '"' {
            i += 1;
            continue;
        }
        let mut literal = String::new();
        i += 1;
        while i < chars.len() && chars[i] != '"' {
            if chars[i] == '\\' && i + 1 < chars.len() {
                i += 1;
            }
            literal.push(chars[i]);
            i += 1;
        }
        i += 1;
        let mut j = i;
        while j < chars.len() && chars[j].is_whitespace() {
            j += 1;
        }
        if chars.get(j) == Some(&':') {
            keys.push(literal);
        }
    }
    keys
}

fn without_yagura_vocabulary(keys: Vec<String>) -> Vec<String> {
    keys.into_iter()
        .filter(|key| !YAGURA_KEYS.contains(&key.as_str()))
        .collect()
}

fn object_without_yagura(value: &Value) -> Value {
    let mut copy = value.clone();
    copy.as_object_mut().expect("an object").remove("yagura");
    copy
}

fn keys_of(value: &Value) -> BTreeSet<String> {
    value
        .as_object()
        .expect("an object")
        .keys()
        .cloned()
        .collect()
}

fn set_of(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|item| (*item).to_string()).collect()
}

fn assert_handler(handler: &Value, command: &str, context: &str) {
    assert_eq!(
        keys_of(handler),
        set_of(&["type", "command", "timeout"]),
        "{context}: handler keys: {handler}"
    );
    assert_eq!(handler["type"], "command", "{context}");
    assert_eq!(handler["command"], command, "{context}");
    assert!(
        handler["timeout"].as_u64().is_some_and(|secs| secs > 0),
        "{context}: a positive timeout in seconds: {handler}"
    );
}

/// The shape agy's file takes: a running report on a new call and after each tool, and the
/// doors the CLI reads its inbox through -- the start-up context when a session starts, and the
/// start of a turn beside the report on each new call. Nothing else is written, and not the
/// events whose empty output agy may treat as a decision.
fn assert_yagura_entry(file: &Value, command: &str) {
    let entry = file
        .get("yagura")
        .unwrap_or_else(|| panic!("the file has a yagura entry: {file}"));
    assert_eq!(
        keys_of(entry),
        set_of(&["PreInvocation", "PostToolUse", "SessionStart"]),
        "the running events and the start-up door are used: {entry}"
    );
    let pre = entry["PreInvocation"].as_array().expect("an array");
    assert_eq!(pre.len(), 2, "{entry}");
    assert_handler(&pre[0], command, "PreInvocation");
    assert_handler(&pre[1], &door_command(command, "take"), "PreInvocation");

    let start = entry["SessionStart"].as_array().expect("an array");
    assert_eq!(start.len(), 1, "{entry}");
    assert_handler(&start[0], &door_command(command, "context"), "SessionStart");

    let post = entry["PostToolUse"].as_array().expect("an array");
    assert_eq!(post.len(), 1, "{entry}");
    assert_eq!(
        keys_of(&post[0]),
        set_of(&["matcher", "hooks"]),
        "PostToolUse is wrapped in a matcher group: {entry}"
    );
    assert_eq!(post[0]["matcher"], "*", "{entry}");
    let handlers = post[0]["hooks"].as_array().expect("an array");
    assert_eq!(handlers.len(), 1, "{entry}");
    assert_handler(&handlers[0], command, "PostToolUse");
}

fn write_value(layout: &Layout, value: &Value) {
    layout.write_hooks(&serde_json::to_string_pretty(value).expect("serialize"));
}

// =============================================================================
// Registration writes, keeps and removes the entry
// =============================================================================

async fn register_then_remove_case(profile: &str, tag: &str) {
    let layout = Layout::new(tag);
    layout.write_hooks(SEED_HOOKS);
    let h = start(tag, profile, &layout).await;
    let mut approver = approver_connection(&h).await;
    let original = parse(SEED_HOOKS);
    let original_keys = key_sequence(SEED_HOOKS);

    let (error, status) = run_action(&mut approver, McpRegistrationAction::Register, 1).await;
    assert_eq!(error, None);
    assert_eq!(status.state, McpRegistrationState::Registered, "{status:?}");

    let registered_text = layout.read_hooks();
    let registered = parse(&registered_text);
    assert_yagura_entry(&registered, &layout.expected_command(profile));
    assert_eq!(
        object_without_yagura(&registered),
        original,
        "the user's own entries keep their content"
    );
    assert_eq!(
        without_yagura_vocabulary(key_sequence(&registered_text)),
        original_keys,
        "the user's own entries keep the order they were written in:\n{registered_text}"
    );
    assert_eq!(
        key_sequence(&registered_text)
            .iter()
            .filter(|key| key.as_str() == "yagura")
            .count(),
        1
    );

    let (error, status) = run_action(&mut approver, McpRegistrationAction::Remove, 2).await;
    assert_eq!(error, None);
    assert_eq!(
        status.state,
        McpRegistrationState::NotRegistered,
        "{status:?}"
    );

    let removed_text = layout.read_hooks();
    assert_eq!(parse(&removed_text), original, "only yagura's entry goes");
    assert_eq!(
        key_sequence(&removed_text),
        original_keys,
        "the user's order is still as written after removal:\n{removed_text}"
    );
}

#[tokio::test]
async fn registering_the_host_row_adds_only_its_entry_and_removing_takes_only_that_away() {
    register_then_remove_case(HOST_PROFILE, "agy-hooks-host").await;
}

#[tokio::test]
async fn registering_the_wsl_row_adds_only_its_entry_and_removing_takes_only_that_away() {
    register_then_remove_case(WSL_PROFILE, "agy-hooks-wsl").await;
}

async fn creates_the_file_case(profile: &str, tag: &str) {
    let layout = Layout::new(tag);
    assert!(!layout.hooks_path().exists(), "the case starts without it");
    let h = start(tag, profile, &layout).await;
    let mut approver = approver_connection(&h).await;

    let (error, status) = run_action(&mut approver, McpRegistrationAction::Register, 1).await;

    assert_eq!(error, None);
    assert_eq!(status.state, McpRegistrationState::Registered, "{status:?}");
    let created = layout.hooks_value();
    assert_eq!(keys_of(&created), set_of(&["yagura"]));
    assert_yagura_entry(&created, &layout.expected_command(profile));

    let (error, _) = run_action(&mut approver, McpRegistrationAction::Remove, 2).await;
    assert_eq!(error, None);
    if layout.hooks_path().exists() {
        assert!(
            layout
                .hooks_value()
                .as_object()
                .is_some_and(serde_json::Map::is_empty),
            "nothing of yagura's is left behind"
        );
    }
}

#[tokio::test]
async fn registering_the_host_row_creates_a_missing_hooks_file() {
    creates_the_file_case(HOST_PROFILE, "agy-hooks-create-host").await;
}

/// The file holds commands a CLI will run, so a new one is the owner's alone.
#[cfg(unix)]
#[tokio::test]
async fn a_newly_created_hooks_file_is_readable_and_writable_by_its_owner_only() {
    use std::os::unix::fs::PermissionsExt;

    let layout = Layout::new("agy-hooks-create-mode");
    let h = start("agy-hooks-create-mode", HOST_PROFILE, &layout).await;
    let mut approver = approver_connection(&h).await;

    let (error, _) = run_action(&mut approver, McpRegistrationAction::Register, 1).await;

    assert_eq!(error, None);
    let mode = std::fs::metadata(layout.hooks_path())
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600, "mode was {mode:o}");
}

#[tokio::test]
async fn registering_the_wsl_row_creates_a_missing_hooks_file() {
    creates_the_file_case(WSL_PROFILE, "agy-hooks-create-wsl").await;
}

async fn replaces_a_stale_entry_case(profile: &str, tag: &str) {
    let layout = Layout::new(tag);
    let mut seed = parse(SEED_HOOKS);
    seed["yagura"] = json!({
        "PreInvocation": [{"type": "command", "command": "/old/place/yagura-daemon", "timeout": 1}]
    });
    write_value(&layout, &seed);
    let h = start(tag, profile, &layout).await;
    let mut approver = approver_connection(&h).await;

    let (error, status) = run_action(&mut approver, McpRegistrationAction::Register, 1).await;

    assert_eq!(error, None);
    assert_eq!(status.state, McpRegistrationState::Registered, "{status:?}");
    let registered = layout.hooks_value();
    assert_yagura_entry(&registered, &layout.expected_command(profile));
    assert_eq!(object_without_yagura(&registered), parse(SEED_HOOKS));
}

#[tokio::test]
async fn registering_replaces_an_older_yagura_entry_of_the_host_row() {
    replaces_a_stale_entry_case(HOST_PROFILE, "agy-hooks-stale-host").await;
}

#[tokio::test]
async fn registering_replaces_an_older_yagura_entry_of_the_wsl_row() {
    replaces_a_stale_entry_case(WSL_PROFILE, "agy-hooks-stale-wsl").await;
}

async fn broken_file_case(profile: &str, tag: &str) {
    let layout = Layout::new(tag);
    let broken = "{ \"zeta\": [1, 2,\n  not json at all";
    layout.write_hooks(broken);
    let h = start(tag, profile, &layout).await;
    let mut approver = approver_connection(&h).await;

    let (error, status) = run_action(&mut approver, McpRegistrationAction::Register, 1).await;

    assert!(
        error.is_some(),
        "a hooks file that is not JSON must fail the registration: {status:?}"
    );
    assert_ne!(status.state, McpRegistrationState::Registered, "{status:?}");
    assert_eq!(
        layout.read_hooks(),
        broken,
        "a file that cannot be edited safely is left byte for byte as it was"
    );
}

#[tokio::test]
async fn a_hooks_file_that_is_not_json_fails_the_host_registration_and_is_left_as_it_was() {
    broken_file_case(HOST_PROFILE, "agy-hooks-broken-host").await;
}

#[tokio::test]
async fn a_hooks_file_that_is_not_json_fails_the_wsl_registration_and_is_left_as_it_was() {
    broken_file_case(WSL_PROFILE, "agy-hooks-broken-wsl").await;
}

// =============================================================================
// The command is the report folder's daemon, written so no shell reinterprets it
// =============================================================================

#[tokio::test]
async fn the_entry_runs_the_report_folders_daemon_by_an_absolute_path_free_of_shell_characters() {
    for (profile, tag) in [
        (HOST_PROFILE, "agy-hooks-cmd-host"),
        (WSL_PROFILE, "agy-hooks-cmd-wsl"),
    ] {
        let layout = Layout::new(tag);
        let h = start(tag, profile, &layout).await;
        let mut approver = approver_connection(&h).await;
        let (error, _) = run_action(&mut approver, McpRegistrationAction::Register, 1).await;
        assert_eq!(error, None, "{profile}");

        let file = layout.hooks_value();
        let command = file["yagura"]["PreInvocation"][0]["command"]
            .as_str()
            .expect("a command string")
            .to_string();
        assert_eq!(command, layout.expected_command(profile), "{profile}");
        let doors = [
            file["yagura"]["PreInvocation"][1]["command"].clone(),
            file["yagura"]["SessionStart"][0]["command"].clone(),
        ];
        for door in doors {
            let door = door.as_str().expect("a command string");
            assert!(
                door.starts_with(command.split(" --").next().unwrap())
                    && !contains_cmd_metacharacter(door)
                    && !door.contains('"'),
                "{profile}: {door:?} runs the same daemon and needs no quoting"
            );
        }
        assert!(
            !contains_cmd_metacharacter(&command) && !command.contains('"'),
            "{profile}: {command:?} must not need quoting"
        );
        if profile == WSL_PROFILE {
            assert!(
                command.starts_with('/'),
                "{profile}: a path on the Linux side: {command:?}"
            );
        } else {
            assert!(
                Path::new(&command).is_absolute(),
                "{profile}: {command:?} must not depend on PATH"
            );
        }
        let raw = layout.read_hooks();
        if cfg!(windows) && profile == HOST_PROFILE {
            assert!(
                raw.contains("\\\\"),
                "backslashes are escaped in the JSON text: {raw}"
            );
        }
    }
}

// =============================================================================
// The registration state includes the entry
// =============================================================================

type Tamper = Box<dyn Fn(&Layout, &mut Value)>;

async fn state_follows_entry_case(profile: &str, tag: &str) {
    let layout = Layout::new(tag);
    layout.write_hooks(SEED_HOOKS);
    let h = start(tag, profile, &layout).await;
    let mut approver = approver_connection(&h).await;
    let (error, status) = run_action(&mut approver, McpRegistrationAction::Register, 1).await;
    assert_eq!(error, None);
    assert_eq!(status.state, McpRegistrationState::Registered, "{status:?}");
    let registered_text = layout.read_hooks();
    assert_eq!(
        listed_state(&mut approver, 2).await,
        McpRegistrationState::Registered
    );

    let tampers: Vec<(&str, Tamper)> = vec![
        (
            "the yagura entry is gone",
            Box::new(|_, file| {
                file.as_object_mut().unwrap().remove("yagura");
            }),
        ),
        (
            "the command points at another daemon",
            Box::new(|_, file| {
                let other = "/elsewhere/yagura-daemon";
                file["yagura"]["PreInvocation"][0]["command"] = json!(other);
                file["yagura"]["PostToolUse"][0]["hooks"][0]["command"] = json!(other);
            }),
        ),
        (
            "PreInvocation is missing",
            Box::new(|_, file| {
                file["yagura"]
                    .as_object_mut()
                    .unwrap()
                    .remove("PreInvocation");
            }),
        ),
        (
            "PostToolUse is missing",
            Box::new(|_, file| {
                file["yagura"]
                    .as_object_mut()
                    .unwrap()
                    .remove("PostToolUse");
            }),
        ),
        (
            "SessionStart is missing",
            Box::new(|_, file| {
                file["yagura"]
                    .as_object_mut()
                    .unwrap()
                    .remove("SessionStart");
            }),
        ),
        (
            "the inbox door beside the running report is gone",
            Box::new(|_, file| {
                file["yagura"]["PreInvocation"]
                    .as_array_mut()
                    .unwrap()
                    .remove(1);
            }),
        ),
        (
            "the start-up door points at another daemon",
            Box::new(|_, file| {
                file["yagura"]["SessionStart"][0]["command"] =
                    json!("/elsewhere/yagura-daemon --deliver-context");
            }),
        ),
        (
            "only one of the two commands differs",
            Box::new(|_, file| {
                file["yagura"]["PostToolUse"][0]["hooks"][0]["command"] =
                    json!("/elsewhere/yagura-daemon");
            }),
        ),
    ];
    for (round, (what, tamper)) in (3u64..).zip(tampers) {
        let mut file = parse(&registered_text);
        tamper(&layout, &mut file);
        write_value(&layout, &file);
        assert_ne!(
            listed_state(&mut approver, round * 10).await,
            McpRegistrationState::Registered,
            "{what}: the entry is incomplete, so the row is not registered"
        );
        layout.write_hooks(&registered_text);
        assert_eq!(
            listed_state(&mut approver, round * 10 + 1).await,
            McpRegistrationState::Registered,
            "{what}: restoring the file restores the state"
        );
    }

    std::fs::remove_file(layout.hooks_path()).expect("remove the hooks file");
    assert_ne!(
        listed_state(&mut approver, 100).await,
        McpRegistrationState::Registered,
        "no hooks file"
    );
    layout.write_hooks(&registered_text);
    layout.write_hooks("not json");
    assert_ne!(
        listed_state(&mut approver, 101).await,
        McpRegistrationState::Registered,
        "a hooks file that is not JSON"
    );
}

#[tokio::test]
async fn the_host_row_is_registered_only_while_its_hooks_entry_holds() {
    state_follows_entry_case(HOST_PROFILE, "agy-state-host").await;
}

#[tokio::test]
async fn the_wsl_row_is_registered_only_while_its_hooks_entry_holds() {
    state_follows_entry_case(WSL_PROFILE, "agy-state-wsl").await;
}

async fn missing_hooks_file_case(profile: &str, tag: &str) {
    let layout = Layout::new(tag);
    layout.write_hooks(SEED_HOOKS);
    let h = start(tag, profile, &layout).await;
    let mut approver = approver_connection(&h).await;
    let (error, status) = run_action(&mut approver, McpRegistrationAction::Register, 1).await;
    assert_eq!(error, None);
    assert_eq!(status.state, McpRegistrationState::Registered, "{status:?}");

    std::fs::remove_file(layout.hooks_path()).expect("remove the hooks file");
    let status = listed_status(&mut approver, 2).await;
    assert_eq!(
        status.state,
        McpRegistrationState::Differs,
        "{profile}: a missing hooks file is a missing entry, not an unreadable file: {status:?}"
    );
    assert_eq!(status.differs_in, vec!["yagura".to_string()], "{profile}");
}

#[tokio::test]
async fn the_host_row_differs_in_its_entry_once_the_hooks_file_is_gone() {
    missing_hooks_file_case(HOST_PROFILE, "agy-missing-hooks-host").await;
}

#[tokio::test]
async fn the_wsl_row_differs_in_its_entry_once_the_read_tool_reports_the_hooks_file_absent() {
    missing_hooks_file_case(WSL_PROFILE, "agy-missing-hooks-wsl").await;
}

#[tokio::test]
async fn without_a_report_folder_the_entry_cannot_be_written_and_the_row_differs_in_it() {
    let tag = "agy-hooks-no-report-folder";
    let layout = Layout::new(tag);
    layout.write_hooks(SEED_HOOKS);
    let config = DaemonConfig {
        report_dir: None,
        ..hooks_config(&layout)
    };
    let h = start_with(tag, HOST_PROFILE, &layout, config).await;
    let mut approver = approver_connection(&h).await;

    let (error, _) = run_action(&mut approver, McpRegistrationAction::Register, 1).await;
    assert!(
        error.is_some(),
        "no daemon path can be written into the hooks file"
    );
    assert_eq!(
        layout.read_hooks(),
        SEED_HOOKS,
        "the hooks file is left as it was"
    );
    let status = listed_status(&mut approver, 2).await;
    assert_eq!(
        status.state,
        McpRegistrationState::Differs,
        "the CLI holds the bridge but the entry can never hold: {status:?}"
    );
    assert_eq!(status.differs_in, vec!["yagura".to_string()]);
}

#[tokio::test]
async fn a_report_folder_whose_path_holds_a_space_is_written_by_a_spaceless_name_or_refused() {
    let tag = "agy-hooks-report-folder-space";
    let mut layout = Layout::new(tag);
    // A space would split the command a shell runs, and the CLI escapes any quotes.
    layout.report_dir = layout.home_dir.with_file_name("report folder").join("bin");
    std::fs::create_dir_all(&layout.report_dir).expect("create the report folder");
    let daemon = layout.report_dir.join(REPORT_EXE_NAME);
    std::fs::write(&daemon, b"stand-in daemon").expect("place the report daemon");
    layout.write_hooks(SEED_HOOKS);
    let h = start(tag, HOST_PROFILE, &layout).await;
    let mut approver = approver_connection(&h).await;

    let (error, status) = run_action(&mut approver, McpRegistrationAction::Register, 1).await;
    match yagura_os::spaceless_path(&daemon) {
        Ok(spaceless) => {
            assert_eq!(error, None, "a daemon with a spaceless name is registered");
            let command = layout.hooks_value()["yagura"]["PreInvocation"][0]["command"]
                .as_str()
                .expect("a command string")
                .to_string();
            assert_eq!(
                command,
                format!("{spaceless}{FOR_INSTANCE} --emit-status running --source antigravity-cli")
            );
            assert!(!spaceless.contains(' '), "{spaceless:?}");
            assert_eq!(
                status.state,
                McpRegistrationState::Registered,
                "the entry is read back by the name it was written with: {status:?}"
            );
            assert_eq!(
                listed_state(&mut approver, 2).await,
                McpRegistrationState::Registered
            );
        }
        Err(refusal) => {
            let error = error.expect("a daemon path with no spaceless name is refused");
            assert!(
                error.contains(&refusal.to_string()),
                "the refusal says why: {error:?}"
            );
            assert_eq!(
                layout.read_hooks(),
                SEED_HOOKS,
                "the hooks file is left as it was"
            );
        }
    }
}

#[tokio::test]
async fn removing_fails_when_the_hooks_file_exists_but_cannot_be_read() {
    let tag = "agy-hooks-remove-unreadable";
    let layout = Layout::new(tag);
    layout.write_hooks(SEED_HOOKS);
    let h = start(tag, HOST_PROFILE, &layout).await;
    let mut approver = approver_connection(&h).await;
    let (error, _) = run_action(&mut approver, McpRegistrationAction::Register, 1).await;
    assert_eq!(error, None);

    // A directory where the file belongs: present, but not readable as a file.
    std::fs::remove_file(layout.hooks_path()).expect("remove the hooks file");
    std::fs::create_dir(layout.hooks_path()).expect("create a directory at the hooks path");
    let (error, _) = run_action(&mut approver, McpRegistrationAction::Remove, 2).await;
    assert!(
        error.is_some(),
        "an unreadable hooks file is not a file that is already gone"
    );
    assert!(layout.hooks_path().is_dir());
}

// =============================================================================
// A window is expected to report exactly while the entry is complete
// =============================================================================

async fn open_window(client: &mut Connection) -> String {
    send(
        client,
        &ClientMessage::OpenWindowFromProfile {
            profile: "agy".to_string(),
            cwd: None,
            name: None,
            model: None,
            effort: None,
            on_name_taken: Default::default(),
        },
    )
    .await;
    match read_response(client, REPLY_TIMEOUT).await {
        DaemonMessage::WindowOpened { window_id, .. } => window_id,
        other => panic!("expected window_opened, got {other:?}"),
    }
}

/// Opens a window from the profile with a CLI that stays alive for a while,
/// and reports whether the daemon expects it to send status reports. An
/// expectation set after the open is waited for only when it is claimed.
async fn window_expects_reports(
    h: &Harness,
    client: &mut Connection,
    profile: &str,
    layout: &Layout,
    claimed: bool,
) -> bool {
    place_profile(profile, layout, Some(WINDOW_ALIVE_MS));
    let window_id = open_window(client).await;
    let entry = h.registry.get(&window_id).expect("the window is listed");
    let deadline = Instant::now() + Duration::from_secs(3);
    let expected = loop {
        if entry.reports_expected() || !claimed || Instant::now() >= deadline {
            break entry.reports_expected();
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    if !claimed {
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(
            h.registry.get(&window_id).is_some(),
            "the window must still be open for the answer to mean anything"
        );
    }
    place_profile(profile, layout, None);
    expected
}

async fn expectation_follows_entry_case(profile: &str, tag: &str) {
    let layout = Layout::new(tag);
    layout.write_hooks(SEED_HOOKS);
    let h = start(tag, profile, &layout).await;
    let mut client = approver_connection(&h).await;

    assert!(
        !window_expects_reports(&h, &mut client, profile, &layout, false).await,
        "nothing is registered, so no report is to be expected"
    );

    let (error, status) = run_action(&mut client, McpRegistrationAction::Register, 1).await;
    assert_eq!(error, None);
    assert_eq!(status.state, McpRegistrationState::Registered, "{status:?}");
    let registered_text = layout.read_hooks();
    assert!(
        window_expects_reports(&h, &mut client, profile, &layout, true).await,
        "the entry is in place, so the CLI will run it"
    );

    let mut incomplete = parse(&registered_text);
    incomplete["yagura"]
        .as_object_mut()
        .unwrap()
        .remove("PostToolUse");
    write_value(&layout, &incomplete);
    assert!(
        !window_expects_reports(&h, &mut client, profile, &layout, false).await,
        "an incomplete entry is not one to expect reports from"
    );

    layout.write_hooks(&registered_text);
    assert!(
        window_expects_reports(&h, &mut client, profile, &layout, true).await,
        "restoring the entry restores the expectation"
    );

    // The expectation rests on the hooks entry alone: the MCP registration
    // being gone does not take it away.
    std::fs::remove_file(layout.mcp_path()).expect("remove the MCP settings file");
    assert!(
        window_expects_reports(&h, &mut client, profile, &layout, true).await,
        "the hooks entry is complete whatever the MCP registration says"
    );

    std::fs::remove_file(layout.hooks_path()).expect("remove the hooks file");
    assert!(
        !window_expects_reports(&h, &mut client, profile, &layout, false).await,
        "without the file there is no hook to wait for"
    );
}

#[tokio::test]
async fn a_host_agy_window_is_expected_to_report_only_while_the_entry_is_complete() {
    expectation_follows_entry_case(HOST_PROFILE, "agy-expect-host").await;
}

#[tokio::test]
async fn a_wsl_agy_window_is_expected_to_report_only_while_the_entry_is_complete() {
    expectation_follows_entry_case(WSL_PROFILE, "agy-expect-wsl").await;
}
