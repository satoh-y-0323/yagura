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
use yagura_daemon::config::OwnedEmptyDir;
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

/// The project settings the stub reads from its working directory
/// (`MCP_CLI_STUB_PROJECT_REGISTRY`).
const PROJECT_REGISTRY_FILE: &str = ".stub-project.json";

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
/// (`<event>:<group>:<handler>`), for the commands `yagura-daemon --hook codex-cli <Event>`.
/// Every event is one group with one handler, so each key ends in `:0:0`.
const HOST_HASHES: [(&str, &str); 7] = [
    (
        "user_prompt_submit:0:0",
        "sha256:02032e0879a2fbddb9f0e2690072eceeb3b16c73f658405259f457079129f2e7",
    ),
    (
        "pre_tool_use:0:0",
        "sha256:31260d38950fe330199fa666df200a3f53c54d4e4da1a0d1b9d84053b7f42466",
    ),
    (
        "post_tool_use:0:0",
        "sha256:1be9973fde53b9993f850a41917392dbd9bb2402c950fccf16794427600925ee",
    ),
    (
        "permission_request:0:0",
        "sha256:3fb04fe1cc776d675b8e4a998207258698706caa29c9cd303df61a060c9b1983",
    ),
    (
        "stop:0:0",
        "sha256:eedecbdbf094978df724e83fa53dead35ccd572bf17413bd4d22a3dcb908870e",
    ),
    (
        "interrupt:0:0",
        "sha256:a3859bc8069fc031b1f0565f704b051613089fa979fb50c20779080ac9ca4489",
    ),
    (
        "session_start:0:0",
        "sha256:24fdd95ac846f2f040eb9080e25cf31fffec6f98ab4203fec6fa7142bf825446",
    ),
];

/// The same, for `yagura-daemon.exe --hook wsl-codex-cli <Event>` (the WSL profile calls the
/// Windows executable).
const WSL_HASHES: [(&str, &str); 7] = [
    (
        "user_prompt_submit:0:0",
        "sha256:e08ce961764ed6b0c8da9f54e00e60f6f1ab0d7d97b6dc7a8a8eb1abb3e721cd",
    ),
    (
        "pre_tool_use:0:0",
        "sha256:91b667eb99eb41cedd5915bda9bd464b3818125b37599aee2b081687770da216",
    ),
    (
        "post_tool_use:0:0",
        "sha256:284a3acc9795611b0649fe54fc08ba6b1d1bc2a671fa08986cdd5639a0f8fac9",
    ),
    (
        "permission_request:0:0",
        "sha256:aca0c79a6f0619696a82f59c4742cee11bddb16d9a6852cf339bc57cae946889",
    ),
    (
        "stop:0:0",
        "sha256:b597adc0cd753aa5905f72cc77136b041b7f37d82b385f22c446db0f63371794",
    ),
    (
        "interrupt:0:0",
        "sha256:aff135859be7c027a03d207700edc6580980129e7b7585cefe226a0210660bc0",
    ),
    (
        "session_start:0:0",
        "sha256:a115dbf2b1fe741824096dce087ed090c8feb2348a8ec2876302e9249a15d649",
    ),
];

/// The keys an older yagura wrote for the handlers `UserPromptSubmit` no longer has. A
/// registration takes them away; they are not part of the trust a window needs.
const RETIRED_SUFFIXES: [&str; 2] = ["user_prompt_submit:0:1", "user_prompt_submit:0:2"];

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
fn trust_root(profile: &str) -> &'static str {
    if profile == WSL_PROFILE || !cfg!(windows) {
        "/<session-flags>/config.toml"
    } else {
        "C:\\<session-flags>\\config.toml"
    }
}

fn expected_trust(profile: &str) -> BTreeMap<String, String> {
    let root = trust_root(profile);
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

/// The handlers each event carries: one call of the profile's own `--hook` door, and the
/// context limit on the two events whose output reaches the model.
fn expected_commands(profile: &str) -> BTreeMap<String, Vec<Handler>> {
    let program = program_of(profile);
    let mut events: Vec<&str> = EVENTS.iter().map(|(event, _, _)| *event).collect();
    events.push("SessionStart");
    events
        .into_iter()
        .map(|event| {
            let limit =
                matches!(event, "SessionStart" | "UserPromptSubmit").then_some(CONTEXT_LIMIT);
            (
                event.to_string(),
                vec![(format!("{program} --hook {profile} {event}"), limit)],
            )
        })
        .collect()
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

/// The `-c` value each event is injected with. Codex derives its trust hash from this text,
/// so the hashes above are only right while it is exactly this.
fn injected_text(profile: &str, event: &str) -> String {
    let program = program_of(profile);
    let limit = if matches!(event, "SessionStart" | "UserPromptSubmit") {
        ",additionalContextLimit=10000"
    } else {
        ""
    };
    format!(
        "hooks.{event}=[{{hooks=[{{type='command',command='{program} --hook {profile} {event}'{limit}}}]}}]"
    )
}

#[test]
fn the_injected_hook_text_is_the_fixed_text_the_trust_hashes_were_taken_from() {
    for profile in [HOST_PROFILE, WSL_PROFILE] {
        let spec = parse_profile(profile, &shipped_source(profile))
            .unwrap_or_else(|err| panic!("{profile} must parse: {err}"))
            .hooks
            .unwrap_or_else(|| panic!("{profile} must ship a [hooks] table"));
        let assignments = hook_assignments(&render_hooks_args(&spec, InjectedPaths::default()));
        let expected: BTreeMap<String, String> = expected_commands(profile)
            .into_keys()
            .map(|event| {
                let text = injected_text(profile, &event);
                (event, text)
            })
            .collect();
        assert_eq!(assignments, expected, "{profile}");
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
    root: PathBuf,
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
            root: root.clone(),
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
        ("MCP_CLI_STUB_PROJECT_REGISTRY", PROJECT_REGISTRY_FILE),
        // The home the stub resolves `CONFIG_FILE` against, as the CLI would.
        ("HOME", home),
        ("USERPROFILE", home),
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
        registration_cwd: Some(OwnedEmptyDir::new(
            layout.root.clone(),
            layout.root.join("state").join("empty"),
        )),
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

/// The retired keys of `profile`, in the layer its trust keys use.
fn retired_keys(profile: &str) -> Vec<String> {
    RETIRED_SUFFIXES
        .iter()
        .map(|suffix| format!("{}:{suffix}", trust_root(profile)))
        .collect()
}

async fn retired_keys_case(profile: &str, tag: &str) {
    let layout = Layout::new(tag);
    let retired = retired_keys(profile);
    let mut seed = String::from(SEED_CONFIG);
    for key in &retired {
        seed.push_str(&format!(
            "\n[hooks.state.\"{}\"]\ntrusted_hash = \"sha256:old\"\n",
            key.replace('\\', "\\\\")
        ));
    }
    layout.write_config(&seed);
    let h = start(tag, profile, &layout).await;
    let mut approver = approver_connection(&h).await;
    assert!(
        retired
            .iter()
            .all(|key| hooks_state(&layout.read_config()).contains_key(key)),
        "the seed holds the retired keys"
    );

    let (error, status) = run_action(&mut approver, McpRegistrationAction::Register, 1).await;
    assert_eq!(error, None);
    assert_eq!(status.state, McpRegistrationState::Registered, "{status:?}");
    assert_eq!(
        hooks_state(&layout.read_config()),
        with_user_key(expected_trust(profile)),
        "registering takes the retired keys away and writes the current ones"
    );

    let mut readded = layout.read_config();
    for key in &retired {
        readded.push_str(&format!(
            "\n[hooks.state.\"{}\"]\ntrusted_hash = \"sha256:old\"\n",
            key.replace('\\', "\\\\")
        ));
    }
    layout.write_config(&readded);
    let (error, _) = run_action(&mut approver, McpRegistrationAction::Remove, 2).await;
    assert_eq!(error, None);
    assert_eq!(
        hooks_state(&layout.read_config()),
        with_user_key(BTreeMap::new()),
        "removing takes the retired keys as well, and only yagura's"
    );
}

#[tokio::test]
async fn registering_and_removing_the_host_codex_row_take_the_retired_trust_keys_away() {
    retired_keys_case(HOST_PROFILE, "codex-retired-host").await;
}

#[tokio::test]
async fn registering_and_removing_the_wsl_codex_row_take_the_retired_trust_keys_away() {
    retired_keys_case(WSL_PROFILE, "codex-retired-wsl").await;
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
    // The server's own table is this instance's key, so incomplete trust is rewritten.
    for (round, (what, edit)) in (3u64..).zip(edits) {
        set_state(&layout, |state| edit(state));
        let state = listed_state(&mut approver, round * 10).await;
        assert_eq!(
            hooks_state(&layout.read_config()),
            with_user_key(expected_trust(profile)),
            "{what}: the listing rewrites yagura's trust and keeps the user's own"
        );
        assert_eq!(
            state,
            McpRegistrationState::Registered,
            "{what}: the rewritten trust holds, so the row is registered"
        );
        layout.write_config(&registered);
    }

    // Without that table there is nothing the user agreed to have rewritten.
    without_server_table_and_one_trust_key(&layout);
    let unagreed = layout.read_config();
    assert_ne!(
        listed_state(&mut approver, 100).await,
        McpRegistrationState::Registered,
        "the trust is incomplete, so the row is not registered"
    );
    assert_eq!(
        layout.read_config(),
        unagreed,
        "a file without this instance's key is not written"
    );
}

/// Takes away the server's own table (what `codex mcp remove` takes) and one of yagura's
/// trust keys, leaving the rest of the trust as it was.
fn without_server_table_and_one_trust_key(layout: &Layout) {
    let mut document = parse(&layout.read_config());
    document
        .get_mut("mcp_servers")
        .and_then(toml::Value::as_table_mut)
        .and_then(|servers| servers.remove("yagura"))
        .expect("the registration wrote the server's table");
    layout.write_config(&toml::to_string(&document).expect("serialize"));
    set_state(layout, |state| {
        let key = state
            .keys()
            .find(|key| key.contains("<session-flags>"))
            .cloned()
            .expect("a yagura trust key");
        state.remove(&key);
    });
}

#[tokio::test]
async fn the_host_codex_row_is_registered_while_its_trust_holds_or_the_listing_rewrites_it() {
    state_follows_trust_case(HOST_PROFILE, "codex-state-host").await;
}

#[tokio::test]
async fn the_wsl_codex_row_is_registered_while_its_trust_holds_or_the_listing_rewrites_it() {
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
    open_in_and_read_launch(client, layout, expected_launches, None).await
}

async fn open_in_and_read_launch(
    client: &mut Connection,
    layout: &Layout,
    expected_launches: usize,
    cwd: Option<&Path>,
) -> Vec<String> {
    send(
        client,
        &ClientMessage::OpenWindowFromProfile {
            profile: "codex".to_string(),
            cwd: cwd.map(|cwd| cwd.to_str().expect("UTF-8").to_string()),
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
    let rewritten = open_and_read_launch(&mut client, &layout, 3).await;
    assert_eq!(
        hooks_state(&layout.read_config()),
        with_user_key(expected_trust(profile)),
        "the launch rewrites the incomplete trust of this instance"
    );
    assert_eq!(
        hook_commands(&hook_overrides(&rewritten)),
        expected_commands(profile),
        "a row whose incomplete trust was rewritten keeps every hook"
    );

    without_server_table_and_one_trust_key(&layout);
    let unagreed = layout.read_config();
    let broken = open_and_read_launch(&mut client, &layout, 4).await;
    assert_eq!(
        hook_overrides(&broken),
        Vec::<String>::new(),
        "a row whose trust is incomplete and cannot be rewritten gets no hooks"
    );
    assert_eq!(
        layout.read_config(),
        unagreed,
        "a file without this instance's key is not written"
    );

    layout.write_config(&registered_text);
    let restored = open_and_read_launch(&mut client, &layout, 5).await;
    assert_eq!(
        hook_commands(&hook_overrides(&restored)),
        expected_commands(profile),
        "restoring the trust restores the hooks"
    );
}

#[tokio::test]
async fn a_host_codex_window_gets_its_hooks_while_the_registration_holds_or_is_rewritten() {
    gating_case(HOST_PROFILE, "codex-gate-host").await;
}

#[tokio::test]
async fn a_wsl_codex_window_gets_its_hooks_while_the_registration_holds_or_is_rewritten() {
    gating_case(WSL_PROFILE, "codex-gate-wsl").await;
}

const LEFT_OUT: &str = "hooks were left out of this launch";

fn left_out_lines(lines: &[&str]) -> usize {
    lines.iter().filter(|line| line.contains(LEFT_OUT)).count()
}

#[traced_test]
#[tokio::test]
async fn a_launch_says_once_with_the_reason_only_when_it_loses_its_hooks_to_stale_trust() {
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

    // Stale trust of this instance is rewritten, so the launch loses nothing.
    set_state(&layout, |state| {
        let key = state
            .keys()
            .find(|key| key.contains("<session-flags>"))
            .cloned()
            .expect("a yagura trust key");
        state.remove(&key);
    });
    open_and_read_launch(&mut client, &layout, 2).await;
    logs_assert(|lines: &[&str]| match left_out_lines(lines) {
        0 => Ok(()),
        n => Err(format!("a rewritten registration logged {n} lines")),
    });

    without_server_table_and_one_trust_key(&layout);
    open_and_read_launch(&mut client, &layout, 3).await;
    logs_assert(|lines: &[&str]| {
        let found: Vec<&&str> = lines.iter().filter(|l| l.contains(LEFT_OUT)).collect();
        match found.as_slice() {
            [line] if line.contains("reason=") && line.contains("profile=\"codex\"") => Ok(()),
            other => Err(format!("expected one line naming the reason: {other:?}")),
        }
    });

    layout.write_config(&registered_text);
    open_and_read_launch(&mut client, &layout, 4).await;
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

// =============================================================================
// Where the registration commands and a window's listing run
// =============================================================================

/// Writes a project's registrations, in the stub's registry form, to `dir`.
fn write_project_registry(dir: &Path, registry: &serde_json::Value) {
    std::fs::write(dir.join(PROJECT_REGISTRY_FILE), registry.to_string())
        .expect("write project registry");
}

#[tokio::test]
async fn a_registration_status_is_not_read_from_project_settings_in_the_home_directory() {
    let layout = Layout::new("codex-status-no-project");
    let server = RegistrationTarget::for_instance(None).server().to_string();
    let bridge = layout.bridge.to_str().unwrap().to_string();
    write_project_registry(
        &layout.home_dir,
        &serde_json::json!({ server.as_str(): { "command": bridge, "args": [] } }),
    );
    let h = start("codex-status-no-project", HOST_PROFILE, &layout).await;
    let mut approver = approver_connection(&h).await;

    assert_eq!(
        listed_state(&mut approver, 1).await,
        McpRegistrationState::NotRegistered,
        "a project's settings in the home directory are not the CLI's registration"
    );
}

#[tokio::test]
async fn a_window_turns_off_another_instances_bridge_registered_in_its_own_project() {
    let layout = Layout::new("codex-disable-project");
    let default_name = RegistrationTarget::for_instance(None).server().to_string();
    let bridge_name = layout.bridge.file_name().unwrap().to_str().unwrap();
    let project = layout.root.join("project");
    std::fs::create_dir_all(&project).expect("create project dir");
    write_project_registry(
        &project,
        &serde_json::json!({
            default_name.as_str(): { "command": format!("/default/{bridge_name}"), "args": [] },
        }),
    );
    let h = start_as("codex-disable-project", HOST_PROFILE, &layout, Some("work")).await;
    let mut client = approver_connection(&h).await;

    let launch = open_in_and_read_launch(&mut client, &layout, 1, Some(&project)).await;

    assert_eq!(
        disabled_servers(&launch),
        vec![default_name],
        "the registration the window's project carries is turned off too: {launch:?}"
    );
}
