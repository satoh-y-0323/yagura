#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration tests: the status-report hooks the shipped Codex profiles
//! inject, and the trust Codex needs before it runs them.
//!
//! Codex runs a command-line `-c hooks.<Event>=...` hook only when the hook's
//! key has a matching `trusted_hash` under `hooks.state` in the user's
//! `~/.codex/config.toml`. The keys and hashes below are what a real Codex
//! (`codex app-server`, `hooks/list`) reports for the exact `-c` values the
//! shipped profiles must carry; they are external data, not recomputed here.
//! The trust is written, removed and read back together with the MCP
//! registration, so every registration case runs the shipped profile with its
//! launch command swapped for the `mcp_cli_stub` fixture and `home_dir` at a
//! temporary directory: the user's own `~/.codex/config.toml` is never read.

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use common::docs::read_doc_verbatim;
use common::{
    Harness, approver_base_config, empty_lookup, mcp_cli_stub_path, read_response,
    register_approver, send, unique_temp_dir, write_profile,
};
use tracing_test::traced_test;
use yagura_daemon::DaemonConfig;
use yagura_daemon::mcp_registration::RegistrationTarget;
use yagura_daemon::profile::{InjectedPaths, ProfileOs, parse_profile, render_hooks_args};
use yagura_os::program::contains_cmd_metacharacter;
use yagura_protocol::{
    ClientMessage, DaemonMessage, McpRegistrationAction, McpRegistrationState,
    McpRegistrationStatusInfo,
};
use yagura_test_support::ipc::Connection;

const REPLY_TIMEOUT: Duration = Duration::from_secs(20);

const HOST_PROFILE: &str = "codex-cli";
const WSL_PROFILE: &str = "wsl-codex-cli";

const CONFIG_FILE: &str = ".codex/config.toml";

/// Codex event name, the snake_case form its trust keys use, and the status
/// the hook reports.
const EVENTS: [(&str, &str, &str); 6] = [
    ("UserPromptSubmit", "user_prompt_submit", "running"),
    ("PreToolUse", "pre_tool_use", "running"),
    ("PostToolUse", "post_tool_use", "running"),
    (
        "PermissionRequest",
        "permission_request",
        "waiting_approval",
    ),
    ("Stop", "stop", "waiting_input"),
    ("Interrupt", "interrupt", "waiting_input"),
];

/// `trusted_hash` per hook, keyed by the part of its trust key after the layer path
/// (`<event>:<group>:<handler>`), for the commands
/// `yagura-daemon --emit-status <status> --source codex-cli` and the three doors a window's CLI
/// reads its inbox through. The first handler of `UserPromptSubmit` is the status report; the
/// doors are the handlers added next to it and the new `SessionStart` event.
const HOST_HASHES: [(&str, &str); 9] = [
    (
        "user_prompt_submit:0:0",
        "sha256:bcdd1a4e04c2adf8d261c27dd14730f9096c08fe0345e1eb69aa716bbdc78ec2",
    ),
    (
        "pre_tool_use:0:0",
        "sha256:6af0ccec70ae43c3185b536b05b462d419e571eabb1d4b94b1a2edaea259e379",
    ),
    (
        "post_tool_use:0:0",
        "sha256:5efd19a45a144926108b1fc429a7263dc6ac6b0b75a8e207d2560ce5e98d544f",
    ),
    (
        "permission_request:0:0",
        "sha256:5efe27044d7f6270223f101a93a3888d13b9357a16b8ab0e9873479b8b34e4a7",
    ),
    (
        "stop:0:0",
        "sha256:4c2cb00ead159dc4581d064a3b1e5773e87ba45b1cd2f52000e3eac60af0e807",
    ),
    (
        "interrupt:0:0",
        "sha256:f922670c2371572c137477fd40f9c9d860e4e25a06d5130bd52f39344c928ac9",
    ),
    (
        "session_start:0:0",
        "sha256:cf21a391f2cb6943a687fb780e9176839d9f98dd552d3c4a73aeceee2bb128b4",
    ),
    (
        "user_prompt_submit:0:1",
        "sha256:fef1c207b9b998e56af2f77afef068a25d39d76b097dcfbbc339f5caafc3105f",
    ),
    (
        "user_prompt_submit:0:2",
        "sha256:15779906e60d795a1816f8cbb6360e2c807012f52098613d09e4d72fb089319a",
    ),
];

/// The same, for `yagura-daemon.exe ...` (the WSL profile calls the Windows executable).
const WSL_HASHES: [(&str, &str); 9] = [
    (
        "user_prompt_submit:0:0",
        "sha256:83f84d8a25f526a1ada8a56b0bf9fc47f4994145ed7427c23932aa675dd79aa4",
    ),
    (
        "pre_tool_use:0:0",
        "sha256:98dabe6d33c4c6d7ab045cd4bd62f35293a7bc6d2dfbd925ead971024f6feb25",
    ),
    (
        "post_tool_use:0:0",
        "sha256:eaa6e89082c12ced1b86bb9d2a230c3918e7943c03cf54f2ad713ed0b1edea91",
    ),
    (
        "permission_request:0:0",
        "sha256:44f1ce78ae1ed855ad7b9bf53ef66a9a9b12d6b3a03c1eb63c9ac361e9a577a2",
    ),
    (
        "stop:0:0",
        "sha256:8918ebfbb7a4fd5c7aae8798514ca10d115b5ef648ef3704a11afcfc7840f666",
    ),
    (
        "interrupt:0:0",
        "sha256:19a4ee2712cd73414b2c13f19a8183731238195ddd118946ee8fb9d210193f2b",
    ),
    (
        "session_start:0:0",
        "sha256:4797ec0bdc9f2dd9beb2dc55e20215a4609c6f6d15ff49f8c2f2e493e6bb6dc5",
    ),
    (
        "user_prompt_submit:0:1",
        "sha256:5be51c65610812ad8b436954dbec8df94f5845db00dfbd5baa17ec8b8092dd00",
    ),
    (
        "user_prompt_submit:0:2",
        "sha256:dab1ea0140be7aef663bdd6dc37130d16134a4b43755295d4d043eaf78b1f4b4",
    ),
];

/// What a hook's `additionalContextLimit` must be: the profile's `max_chars`.
const CONTEXT_LIMIT: i64 = 10_000;

fn program_of(profile: &str) -> &'static str {
    if profile == WSL_PROFILE {
        "yagura-daemon.exe"
    } else {
        "yagura-daemon"
    }
}

/// The trust keys Codex derives for `profile`'s hooks: the synthetic layer
/// path of the command-line flags depends on the operating system of the
/// Codex process, which is Linux inside WSL and the host otherwise.
fn expected_trust(profile: &str) -> BTreeMap<String, String> {
    let root = if profile == WSL_PROFILE || !cfg!(windows) {
        "/<session-flags>/config.toml"
    } else {
        "C:\\<session-flags>\\config.toml"
    };
    let hashes = if profile == WSL_PROFILE {
        WSL_HASHES
    } else {
        HOST_HASHES
    };
    hashes
        .iter()
        .map(|(suffix, hash)| (format!("{root}:{suffix}"), (*hash).to_string()))
        .collect()
}

fn shipped_source(profile: &str) -> String {
    read_doc_verbatim(&format!("../../profiles/{profile}.toml"))
}

// =============================================================================
// The injected arguments
// =============================================================================

/// One handler of an event: its command and the `additionalContextLimit` it carries.
type Handler = (String, Option<i64>);

/// Each event's `-c hooks.<Event>=...` assignment text, as written in the arguments.
fn hook_assignments(args: &[String]) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let mut iter = args.iter();
    while let Some(flag) = iter.next() {
        assert_eq!(flag, "-c", "hooks args are -c overrides only: {args:?}");
        let assignment = iter.next().expect("-c carries an assignment");
        let event = assignment
            .strip_prefix("hooks.")
            .and_then(|rest| rest.split('=').next())
            .unwrap_or_else(|| panic!("unexpected override: {assignment}"));
        assert!(
            out.insert(event.to_string(), assignment.clone()).is_none(),
            "{event} appears twice"
        );
    }
    out
}

/// Each event's handlers, in order, in the `-c hooks.<Event>=...` arguments of `args`,
/// decoding every value as TOML on the way. Every event is one override with one group.
fn hook_commands(args: &[String]) -> BTreeMap<String, Vec<Handler>> {
    let mut out = BTreeMap::new();
    for (event_name, assignment) in hook_assignments(args) {
        let document: toml::Table = assignment
            .parse()
            .unwrap_or_else(|err| panic!("{assignment} must decode as TOML: {err}"));
        let hooks = document["hooks"].as_table().expect("a hooks table");
        assert_eq!(hooks.len(), 1, "one event per override: {assignment}");
        let (event, groups) = hooks.iter().next().unwrap();
        assert_eq!(*event, event_name);
        let groups = groups.as_array().expect("an array of groups");
        assert_eq!(groups.len(), 1, "{event}: one group");
        let handlers = groups[0]["hooks"].as_array().expect("group hooks");
        let decoded: Vec<Handler> = handlers
            .iter()
            .map(|handler| {
                assert_eq!(handler["type"].as_str(), Some("command"), "{event}");
                (
                    handler["command"]
                        .as_str()
                        .expect("a command string")
                        .to_string(),
                    handler
                        .get("additionalContextLimit")
                        .map(|limit| limit.as_integer().expect("an integer limit")),
                )
            })
            .collect();
        out.insert(event.clone(), decoded);
    }
    out
}

/// The handlers each event carries: the status report alone for the events that only report,
/// and for `UserPromptSubmit` the report first, then the door that reads the inbox and the one
/// that tells the daemon the session; `SessionStart` is the door for the start-up context.
fn expected_commands(profile: &str) -> BTreeMap<String, Vec<Handler>> {
    let program = program_of(profile);
    let mut expected: BTreeMap<String, Vec<Handler>> = EVENTS
        .iter()
        .map(|(event, _, status)| {
            (
                (*event).to_string(),
                vec![(
                    format!("{program} --emit-status {status} --source codex-cli"),
                    None,
                )],
            )
        })
        .collect();
    expected
        .get_mut("UserPromptSubmit")
        .expect("the status events include UserPromptSubmit")
        .extend([
            (format!("{program} --deliver-take"), Some(CONTEXT_LIMIT)),
            (format!("{program} --report-session /session_id"), None),
        ]);
    expected.insert(
        "SessionStart".to_string(),
        vec![(format!("{program} --deliver-context"), Some(CONTEXT_LIMIT))],
    );
    expected
}

#[test]
fn the_shipped_codex_profiles_inject_their_hooks_as_plain_overrides_one_event_each() {
    for profile in [HOST_PROFILE, WSL_PROFILE] {
        let parsed = parse_profile(profile, &shipped_source(profile))
            .unwrap_or_else(|err| panic!("{profile} must parse: {err}"));
        let spec = parsed
            .hooks
            .unwrap_or_else(|| panic!("{profile} must ship a [hooks] table"));
        assert!(
            spec.config_file.is_none(),
            "{profile}: the hooks are command-line overrides, not a settings file"
        );
        let args = render_hooks_args(&spec, InjectedPaths::default());
        assert_eq!(args.len(), (EVENTS.len() + 1) * 2, "{profile}: {args:?}");
        assert_eq!(
            hook_commands(&args),
            expected_commands(profile),
            "{profile}"
        );
        assert!(
            args.iter().all(|arg| !arg.contains("hooks.state")),
            "{profile}: trust lives in the user's config file, not on the command line: {args:?}"
        );
    }
}

#[test]
fn the_doors_are_added_to_the_user_prompt_group_without_dropping_the_status_report() {
    for profile in [HOST_PROFILE, WSL_PROFILE] {
        let program = program_of(profile);
        let spec = parse_profile(profile, &shipped_source(profile))
            .unwrap_or_else(|err| panic!("{profile} must parse: {err}"))
            .hooks
            .unwrap_or_else(|| panic!("{profile} must ship a [hooks] table"));
        let assignments = hook_assignments(&render_hooks_args(&spec, InjectedPaths::default()));

        let submit = &assignments["UserPromptSubmit"];
        let running = format!(
            "{{type='command',command='{program} --emit-status running --source codex-cli'}}"
        );
        let take = format!(
            "{{type='command',command='{program} --deliver-take',additionalContextLimit=10000}}"
        );
        let report = format!("{{type='command',command='{program} --report-session /session_id'}}");
        let positions: Vec<usize> = [&running, &take, &report]
            .iter()
            .map(|handler| {
                submit
                    .find(handler.as_str())
                    .unwrap_or_else(|| panic!("{profile}: {handler} is missing from {submit}"))
            })
            .collect();
        assert!(
            positions.windows(2).all(|pair| pair[0] < pair[1]),
            "{profile}: the handlers keep their indexes (status report 0, inbox 1, session 2), \
             which the trust keys name: {submit}"
        );
        let start = format!(
            "{{type='command',command='{program} --deliver-context',additionalContextLimit=10000}}"
        );
        assert!(
            assignments["SessionStart"].contains(&start),
            "{profile}: {}",
            assignments["SessionStart"]
        );
    }
}

#[test]
fn the_injected_codex_arguments_pass_the_command_interpreters_metacharacter_gate() {
    for profile in [HOST_PROFILE, WSL_PROFILE] {
        let spec = parse_profile(profile, &shipped_source(profile))
            .unwrap_or_else(|err| panic!("{profile} must parse: {err}"))
            .hooks
            .unwrap_or_else(|| panic!("{profile} must ship a [hooks] table"));
        let args = render_hooks_args(&spec, InjectedPaths::default());
        assert!(!args.is_empty(), "{profile}: no hooks were injected");
        for arg in &args {
            assert!(
                !contains_cmd_metacharacter(arg),
                "{profile}: a .cmd shim launch would be refused for {arg:?}"
            );
        }
    }
}

// =============================================================================
// Fixture: the shipped profile with its launch command swapped for the stub
// =============================================================================

struct Layout {
    profiles_dir: PathBuf,
    home_dir: PathBuf,
    state_dir: PathBuf,
    mcp_config_dir: PathBuf,
    log: PathBuf,
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
            log: root.join("stub.log"),
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

    fn config_path(&self) -> PathBuf {
        self.home_dir.join(CONFIG_FILE)
    }

    fn write_config(&self, text: &str) {
        let path = self.config_path();
        std::fs::create_dir_all(path.parent().unwrap()).expect("create config dir");
        std::fs::write(path, text).expect("write config");
    }

    fn read_config(&self) -> String {
        std::fs::read_to_string(self.config_path()).expect("read config")
    }
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
/// tools.
/// Everything else, including whatever the shipped registration table says
/// about trust, is kept as shipped.
fn fixture_profile(profile: &str, layout: &Layout) -> String {
    let stub = mcp_cli_stub_path();
    let stub = stub.to_str().expect("stub path is UTF-8");
    let home = layout.home_dir.to_str().expect("home is UTF-8");
    let mut root: toml::Table = shipped_source(profile)
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
    for (key, value) in [
        (
            "MCP_CLI_STUB_STATE_DIR",
            layout.state_dir.to_str().expect("UTF-8"),
        ),
        ("MCP_CLI_STUB_TOML_TABLE_PATH", CONFIG_FILE),
        ("MCP_CLI_STUB_LOG", layout.log.to_str().expect("UTF-8")),
    ] {
        env.insert(key.into(), toml::Value::String(value.to_string()));
    }
    root.insert("env".into(), toml::Value::Table(env));
    toml::to_string(&root).expect("serialize the fixture profile")
}

fn is_file(path: &Path) -> bool {
    path.is_file()
}

async fn start(tag: &str, profile: &str, layout: &Layout) -> Harness {
    start_as(tag, profile, layout, None).await
}

async fn start_as(tag: &str, profile: &str, layout: &Layout, instance: Option<&str>) -> Harness {
    write_profile(
        &layout.profiles_dir,
        "codex",
        &fixture_profile(profile, layout),
    );
    let config = DaemonConfig {
        profiles_dir: Some(layout.profiles_dir.clone()),
        home_dir: Some(layout.home_dir.clone()),
        instance_id: instance.map(str::to_string),
        mcp_bridge: Some(layout.bridge.clone()),
        mcp_config_dir: Some(layout.mcp_config_dir.clone()),
        mcp_registration_timeout: Duration::from_secs(20),
        mcp_registration_max_output_bytes: 1 << 20,
        mcp_registration_status_max_concurrent: 2,
        host_os: ProfileOs::host(),
        program_lookup: empty_lookup(),
        program_is_executable: is_file,
        default_shell: None,
        ..approver_base_config()
    };
    Harness::start(tag, config).await
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
            profile: "codex".to_string(),
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
    send(client, &ClientMessage::ListMcpRegistrations { request_id }).await;
    match read_response(client, REPLY_TIMEOUT).await {
        DaemonMessage::McpRegistrationsListed { rows, .. } => {
            assert_eq!(rows.len(), 1, "{rows:?}");
        }
        other => panic!("expected mcp_registrations_listed, got {other:?}"),
    }
    match read_response(client, REPLY_TIMEOUT).await {
        DaemonMessage::McpRegistrationStatus { status, .. } => status.state,
        other => panic!("expected mcp_registration_status, got {other:?}"),
    }
}

// --- reading the edited file ----------------------------------------------------

const USER_KEY: &str = "user-own:pre_tool_use:0:0";

/// A user's settings with comments, unrelated tables and a trust entry of
/// their own for a hook yagura knows nothing about.
const SEED_CONFIG: &str = "# my codex settings\n\
model = \"gpt-5.6-terra\" # keep me\n\
\n\
[projects.'C:\\work']\n\
trust_level = \"trusted\"\n\
\n\
[hooks.state.\"user-own:pre_tool_use:0:0\"]\n\
trusted_hash = \"sha256:0000\"\n";

fn parse(text: &str) -> toml::Table {
    text.parse()
        .unwrap_or_else(|err| panic!("the file must stay valid TOML: {err}\n{text}"))
}

fn hooks_state(text: &str) -> BTreeMap<String, String> {
    let document = parse(text);
    let Some(state) = document
        .get("hooks")
        .and_then(|hooks| hooks.get("state"))
        .and_then(toml::Value::as_table)
    else {
        return BTreeMap::new();
    };
    state
        .iter()
        .map(|(key, entry)| {
            let hash = entry
                .get("trusted_hash")
                .and_then(toml::Value::as_str)
                .unwrap_or_else(|| panic!("{key} must carry a trusted_hash: {entry:?}"));
            (key.clone(), hash.to_string())
        })
        .collect()
}

/// Every non-blank line of `original` still appears in `edited`, in order:
/// nothing was rewritten, only added to.
fn lines_kept_in_order(original: &str, edited: &str) -> bool {
    let mut remaining = edited.lines();
    original
        .lines()
        .filter(|line| !line.trim().is_empty())
        .all(|line| remaining.any(|candidate| candidate == line))
}

fn with_user_key(mut expected: BTreeMap<String, String>) -> BTreeMap<String, String> {
    expected.insert(USER_KEY.to_string(), "sha256:0000".to_string());
    expected
}

// =============================================================================
// Registration writes, keeps and removes the trust
// =============================================================================

async fn register_then_remove_case(profile: &str, tag: &str) {
    let layout = Layout::new(tag);
    layout.write_config(SEED_CONFIG);
    let h = start(tag, profile, &layout).await;
    let mut approver = approver_connection(&h).await;

    let (error, status) = run_action(&mut approver, McpRegistrationAction::Register, 1).await;
    assert_eq!(error, None);
    assert_eq!(status.state, McpRegistrationState::Registered, "{status:?}");

    let registered = layout.read_config();
    assert_eq!(
        hooks_state(&registered),
        with_user_key(expected_trust(profile)),
        "exactly yagura's keys are added next to the user's own"
    );
    assert!(
        lines_kept_in_order(SEED_CONFIG, &registered),
        "the user's lines (comments, tables, their own trust) stay as written:\n{registered}"
    );

    let (error, status) = run_action(&mut approver, McpRegistrationAction::Remove, 2).await;
    assert_eq!(error, None);
    assert_eq!(
        status.state,
        McpRegistrationState::NotRegistered,
        "{status:?}"
    );

    let removed = layout.read_config();
    assert_eq!(
        hooks_state(&removed),
        with_user_key(BTreeMap::new()),
        "only yagura's keys are removed"
    );
    assert!(
        !removed.contains("<session-flags>"),
        "no trace of yagura's keys is left:\n{removed}"
    );
    assert!(
        lines_kept_in_order(SEED_CONFIG, &removed),
        "the user's lines are still as written after removal:\n{removed}"
    );
}

#[tokio::test]
async fn registering_the_host_codex_row_adds_its_trust_keys_and_removing_takes_them_away() {
    register_then_remove_case(HOST_PROFILE, "codex-trust-host").await;
}

#[tokio::test]
async fn registering_the_wsl_codex_row_adds_its_trust_keys_and_removing_takes_them_away() {
    register_then_remove_case(WSL_PROFILE, "codex-trust-wsl").await;
}

#[tokio::test]
async fn a_hooks_entry_that_is_not_a_table_fails_the_registration_and_is_left_as_it_was() {
    let layout = Layout::new("codex-trust-blocked");
    layout.write_config("hooks = 1\n");
    let h = start("codex-trust-blocked", HOST_PROFILE, &layout).await;
    let mut approver = approver_connection(&h).await;

    let (error, status) = run_action(&mut approver, McpRegistrationAction::Register, 1).await;

    assert!(
        error.is_some(),
        "trust that cannot be written must fail the registration: {status:?}"
    );
    assert_ne!(status.state, McpRegistrationState::Registered, "{status:?}");
    let document = parse(&layout.read_config());
    assert_eq!(
        document.get("hooks").and_then(toml::Value::as_integer),
        Some(1),
        "the user's own value is not replaced"
    );
}

// =============================================================================
// The registration state includes the trust
// =============================================================================

fn set_state(layout: &Layout, edit: impl FnOnce(&mut toml::Table)) {
    let mut document = parse(&layout.read_config());
    let state = document
        .get_mut("hooks")
        .and_then(toml::Value::as_table_mut)
        .and_then(|hooks| hooks.get_mut("state"))
        .and_then(toml::Value::as_table_mut)
        .expect("the registration wrote hooks.state");
    edit(state);
    layout.write_config(&toml::to_string(&document).expect("serialize"));
}

async fn state_follows_trust_case(profile: &str, tag: &str) {
    let layout = Layout::new(tag);
    layout.write_config(SEED_CONFIG);
    let h = start(tag, profile, &layout).await;
    let mut approver = approver_connection(&h).await;
    let (error, status) = run_action(&mut approver, McpRegistrationAction::Register, 1).await;
    assert_eq!(error, None);
    assert_eq!(status.state, McpRegistrationState::Registered, "{status:?}");
    let registered = layout.read_config();
    assert_eq!(
        listed_state(&mut approver, 2).await,
        McpRegistrationState::Registered
    );

    let first_key = expected_trust(profile).into_keys().next().unwrap();
    type Edit = Box<dyn Fn(&mut toml::Table)>;
    let key_for_hash = first_key.clone();
    let edits: Vec<(&str, Edit)> = vec![
        (
            "one key missing",
            Box::new(move |state: &mut toml::Table| {
                state.remove(&first_key);
            }),
        ),
        (
            "one hash differs",
            Box::new(move |state: &mut toml::Table| {
                state
                    .get_mut(&key_for_hash)
                    .and_then(toml::Value::as_table_mut)
                    .expect("the key entry is a table")
                    .insert(
                        "trusted_hash".into(),
                        toml::Value::String("sha256:dead".into()),
                    );
            }),
        ),
        (
            "all of yagura's keys missing",
            Box::new(|state: &mut toml::Table| {
                state.retain(|key, _| !key.contains("<session-flags>"));
            }),
        ),
    ];
    for (round, (what, edit)) in (3u64..).zip(edits) {
        set_state(&layout, |state| edit(state));
        assert_ne!(
            listed_state(&mut approver, round * 10).await,
            McpRegistrationState::Registered,
            "{what}: the trust is incomplete, so the row is not registered"
        );
        layout.write_config(&registered);
        assert_eq!(
            listed_state(&mut approver, round * 10 + 1).await,
            McpRegistrationState::Registered,
            "{what}: restoring the file restores the state"
        );
    }
}

#[tokio::test]
async fn the_host_codex_row_is_registered_only_while_all_of_its_trust_keys_hold() {
    state_follows_trust_case(HOST_PROFILE, "codex-state-host").await;
}

#[tokio::test]
async fn the_wsl_codex_row_is_registered_only_while_all_of_its_trust_keys_hold() {
    state_follows_trust_case(WSL_PROFILE, "codex-state-wsl").await;
}

// =============================================================================
// A window gets the hooks only when the registration holds
// =============================================================================

/// The argv of every launch of the stub as a CLI: the `-c` overrides come
/// first, unlike the registration subcommands (`mcp`, `probe`, `file-*`).
fn launches(log: &Path) -> Vec<Vec<String>> {
    let Ok(text) = std::fs::read_to_string(log) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| {
            let record: serde_json::Value = serde_json::from_str(line).ok()?;
            let argv: Vec<String> = record["argv"]
                .as_array()?
                .iter()
                .map(|value| value.as_str().unwrap_or_default().to_string())
                .collect();
            (argv.first().map(String::as_str) == Some("-c")).then_some(argv)
        })
        .collect()
}

async fn open_and_read_launch(
    client: &mut Connection,
    layout: &Layout,
    expected_launches: usize,
) -> Vec<String> {
    send(
        client,
        &ClientMessage::OpenWindowFromProfile {
            profile: "codex".to_string(),
            cwd: None,
            name: None,
            model: None,
            effort: None,
            on_name_taken: Default::default(),
        },
    )
    .await;
    match read_response(client, REPLY_TIMEOUT).await {
        DaemonMessage::WindowOpened { .. } => {}
        other => panic!("expected window_opened, got {other:?}"),
    }
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let all = launches(&layout.log);
        if all.len() >= expected_launches {
            return all[expected_launches - 1].clone();
        }
        assert!(
            Instant::now() < deadline,
            "the window's child never started: {all:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn hook_overrides(launch: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for pair in launch.windows(2) {
        if pair[0] == "-c" && pair[1].starts_with("hooks.") {
            out.push("-c".to_string());
            out.push(pair[1].clone());
        }
    }
    out
}

async fn gating_case(profile: &str, tag: &str) {
    let layout = Layout::new(tag);
    layout.write_config(SEED_CONFIG);
    let h = start(tag, profile, &layout).await;
    let mut client = approver_connection(&h).await;

    let unregistered = open_and_read_launch(&mut client, &layout, 1).await;
    assert!(
        unregistered
            .iter()
            .any(|arg| arg.starts_with("mcp_servers.yagura.command=")),
        "the launch reached the CLI with its MCP injection: {unregistered:?}"
    );
    assert_eq!(
        hook_overrides(&unregistered),
        Vec::<String>::new(),
        "an unregistered row gets no hooks"
    );

    let (error, status) = run_action(&mut client, McpRegistrationAction::Register, 1).await;
    assert_eq!(error, None);
    assert_eq!(status.state, McpRegistrationState::Registered, "{status:?}");
    let registered_text = layout.read_config();

    let registered = open_and_read_launch(&mut client, &layout, 2).await;
    assert_eq!(
        hook_commands(&hook_overrides(&registered)),
        expected_commands(profile),
        "a registered row gets every hook: {registered:?}"
    );
    assert!(
        registered.iter().all(|arg| !arg.contains("hooks.state")),
        "trust is never passed on the command line: {registered:?}"
    );

    set_state(&layout, |state| {
        let key = state
            .keys()
            .find(|key| key.contains("<session-flags>"))
            .cloned()
            .expect("a yagura trust key");
        state.remove(&key);
    });
    let broken = open_and_read_launch(&mut client, &layout, 3).await;
    assert_eq!(
        hook_overrides(&broken),
        Vec::<String>::new(),
        "a row whose trust is incomplete gets no hooks"
    );

    layout.write_config(&registered_text);
    let restored = open_and_read_launch(&mut client, &layout, 4).await;
    assert_eq!(
        hook_commands(&hook_overrides(&restored)),
        expected_commands(profile),
        "restoring the trust restores the hooks"
    );
}

#[tokio::test]
async fn a_host_codex_window_gets_its_hooks_only_while_the_registration_holds() {
    gating_case(HOST_PROFILE, "codex-gate-host").await;
}

#[tokio::test]
async fn a_wsl_codex_window_gets_its_hooks_only_while_the_registration_holds() {
    gating_case(WSL_PROFILE, "codex-gate-wsl").await;
}

const LEFT_OUT: &str = "hooks were left out of this launch";

fn left_out_lines(lines: &[&str]) -> usize {
    lines.iter().filter(|line| line.contains(LEFT_OUT)).count()
}

#[traced_test]
#[tokio::test]
async fn a_launch_that_loses_its_hooks_to_stale_trust_says_so_once_with_the_reason() {
    let layout = Layout::new("codex-gate-warn");
    layout.write_config(SEED_CONFIG);
    let h = start("codex-gate-warn", HOST_PROFILE, &layout).await;
    let mut client = approver_connection(&h).await;
    let (error, _) = run_action(&mut client, McpRegistrationAction::Register, 1).await;
    assert_eq!(error, None);
    let registered_text = layout.read_config();

    open_and_read_launch(&mut client, &layout, 1).await;
    logs_assert(|lines: &[&str]| match left_out_lines(lines) {
        0 => Ok(()),
        n => Err(format!("a complete registration logged {n} lines")),
    });

    set_state(&layout, |state| {
        let key = state
            .keys()
            .find(|key| key.contains("<session-flags>"))
            .cloned()
            .expect("a yagura trust key");
        state.remove(&key);
    });
    open_and_read_launch(&mut client, &layout, 2).await;
    logs_assert(|lines: &[&str]| {
        let found: Vec<&&str> = lines.iter().filter(|l| l.contains(LEFT_OUT)).collect();
        match found.as_slice() {
            [line] if line.contains("out of date") && line.contains("profile=\"codex\"") => Ok(()),
            other => Err(format!("expected one line naming the reason: {other:?}")),
        }
    });

    layout.write_config(&registered_text);
    open_and_read_launch(&mut client, &layout, 3).await;
    logs_assert(|lines: &[&str]| match left_out_lines(lines) {
        1 => Ok(()),
        n => Err(format!("restoring the trust must add no line, found {n}")),
    });
}

// =============================================================================
// A window disables the other instances' bridge registrations
// =============================================================================

fn disabled_servers(launch: &[String]) -> Vec<String> {
    launch
        .windows(2)
        .filter(|pair| pair[0] == "-c")
        .filter_map(|pair| {
            let rest = pair[1].strip_prefix("mcp_servers.")?;
            rest.strip_suffix(".enabled=false").map(str::to_string)
        })
        .collect()
}

#[tokio::test]
async fn a_named_instance_window_disables_only_the_default_bridge_registration() {
    let layout = Layout::new("codex-disable-others");
    let default_name = RegistrationTarget::for_instance(None).server().to_string();
    let own_name = RegistrationTarget::for_instance(Some("work"))
        .server()
        .to_string();
    let bridge_name = layout.bridge.file_name().unwrap().to_str().unwrap();
    let bridge_path = |dir: &str| format!("{dir}/{bridge_name}");
    let registry = serde_json::json!({
        default_name.as_str(): { "command": bridge_path("/default"), "args": [] },
        own_name.as_str(): { "command": bridge_path("/work"), "args": [] },
        "my-own-tool": { "command": "/tools/my-own-tool", "args": [] },
    });
    std::fs::write(layout.state_dir.join("registry.json"), registry.to_string())
        .expect("seed registry");
    let h = start_as("codex-disable-others", HOST_PROFILE, &layout, Some("work")).await;
    let mut client = approver_connection(&h).await;

    let launch = open_and_read_launch(&mut client, &layout, 1).await;

    assert_eq!(
        disabled_servers(&launch),
        vec![default_name],
        "only the bridge registration of another instance is disabled: {launch:?}"
    );
}
