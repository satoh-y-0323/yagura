#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: the specific launch profiles this repository ships
//! (`profiles/*.toml`) carry the `[mcp]` table their CLI needs to reach the
//! daemon's own MCP bridge, and the two WSL-routed profiles' Windows argv
//! execs the CLI directly rather than handing it to a shell that could
//! reinterpret what follows.
//!
//! Split from `profile_mcp.rs` (which measures the parser and renderer's
//! general rules against synthetic fixtures) because this file's subject is
//! the *specific* files this repository ships, following the same split
//! `launch_option_docs.rs` draws between the bundled launch profiles and
//! `profile_load.rs`'s general parsing rules.

use crate::common;

use common::docs::read_doc_verbatim;
use yagura_daemon::mcp_registration::{EntryVars, RegistrationTarget};
use yagura_daemon::profile::{
    InjectedPaths, JsonFilter, ModelChoicesSpec, OutputFormat, Profile, ProfileCommand, ProfileOs,
    argv_for_os, parse_profile, prepare_hooks_injection, render_hooks_args,
};
use yagura_daemon::profile_hook_events::{HookDeliver, HookEvent};
use yagura_os::program::HostOs;
use yagura_protocol::WindowStatus;

/// Reads and parses one of the repository's own shipped profiles by id
/// (`profiles/<id>.toml`, relative to this crate's manifest directory).
///
/// # Panics
///
/// Panics if the file cannot be read or does not parse as a valid profile: a
/// shipped profile that fails either is a contract violation this test must
/// surface loudly, not one it should silently skip.
fn parse_repo_profile(id: &str) -> Profile {
    let rel = format!("../../profiles/{id}.toml");
    let source = read_doc_verbatim(&rel);
    parse_profile(id, &source)
        .unwrap_or_else(|err| panic!("bundled profile {id} must parse as a valid profile: {err}"))
}

/// Decodes a JSON document embedded in a profile's own text (a
/// `config_file` string, or a WSL profile's literal `args` element),
/// returning the `mcpServers.{server}.command` field it must carry.
fn mcp_server_command(json: &str) -> String {
    let value: serde_json::Value = serde_json::from_str(json)
        .unwrap_or_else(|err| panic!("must be valid JSON: {err}, source: {json:?}"));
    value["mcpServers"]["{server}"]["command"]
        .as_str()
        .unwrap_or_else(|| panic!("mcpServers.{{server}}.command must be a string, got: {value}"))
        .to_string()
}

/// The Windows argv a bundled profile's `[command]` table names, resolved
/// the same way [`yagura_daemon::window`]'s spawn path would (`argv_for_os`
/// on the Windows key).
fn windows_argv(id: &str) -> Vec<String> {
    let profile = parse_repo_profile(id);
    let command: &ProfileCommand = profile
        .command
        .as_ref()
        .unwrap_or_else(|| panic!("{id} must have a [command] table"));
    argv_for_os(command, ProfileOs::Windows)
        .unwrap_or_else(|| panic!("{id} must define a windows argv"))
        .to_vec()
}

// ---------------------------------------------------------------------
// Each bundled profile's [mcp] table
// ---------------------------------------------------------------------

#[test]
fn claude_code_ships_a_config_file_referencing_the_bridge_variable() {
    let profile = parse_repo_profile("claude-code");
    let mcp = profile
        .mcp
        .as_ref()
        .expect("claude-code must ship an [mcp] table");

    assert_eq!(
        mcp.args,
        vec!["--mcp-config".to_string(), "{mcp_config_file}".to_string(),],
        "claude-code's [mcp] args must be exactly --mcp-config {{mcp_config_file}}"
    );
    let config_file = mcp
        .config_file
        .as_ref()
        .expect("claude-code's [mcp] table must carry a config_file");
    assert_eq!(
        mcp_server_command(config_file),
        "${YAGURA_MCP_BRIDGE}",
        "the config file's mcpServers.{{server}}.command must reference the bridge \
         environment variable, not a literal path"
    );
}

#[test]
fn wsl_claude_code_ships_the_mcp_json_directly_in_its_args() {
    let profile = parse_repo_profile("wsl-claude-code");
    let mcp = profile
        .mcp
        .as_ref()
        .expect("wsl-claude-code must ship an [mcp] table");

    assert_eq!(
        mcp.config_file, None,
        "wsl-claude-code has no config_file -- the JSON goes straight into args, which \
         wsl.exe (not a cmd shim) can carry unchanged"
    );
    assert_eq!(
        mcp.args.first().map(String::as_str),
        Some("--mcp-config"),
        "wsl-claude-code's [mcp] args must start with --mcp-config, got {:?}",
        mcp.args
    );
    let json = mcp.args.get(1).unwrap_or_else(|| {
        panic!(
            "wsl-claude-code's [mcp] args must carry the JSON as a second element, got {:?}",
            mcp.args
        )
    });
    assert_eq!(mcp_server_command(json), "${YAGURA_MCP_BRIDGE}");
}

#[test]
fn codex_cli_ships_bridge_toml_and_env_names_toml_placeholders() {
    let profile = parse_repo_profile("codex-cli");
    let mcp = profile
        .mcp
        .as_ref()
        .expect("codex-cli must ship an [mcp] table");

    let joined = mcp.args.join(" ");
    assert!(
        joined.contains("{bridge_toml}"),
        "codex-cli's [mcp] args must use {{bridge_toml}}, got {:?}",
        mcp.args
    );
    assert!(
        joined.contains("{env_names_toml}"),
        "codex-cli's [mcp] args must use {{env_names_toml}}, got {:?}",
        mcp.args
    );
}

#[test]
fn wsl_codex_cli_ships_a_bash_command_reading_the_bridge_variable_and_env_names_toml() {
    let profile = parse_repo_profile("wsl-codex-cli");
    let mcp = profile
        .mcp
        .as_ref()
        .expect("wsl-codex-cli must ship an [mcp] table");

    let joined = mcp.args.join(" ");
    assert!(
        joined.contains("command='bash'"),
        "wsl-codex-cli's [mcp] args must set the mcp server command to bash, got {:?}",
        mcp.args
    );
    assert!(
        joined.contains("exec \"$YAGURA_MCP_BRIDGE\""),
        "wsl-codex-cli's [mcp] args must exec the bridge through the environment \
         variable, got {:?}",
        mcp.args
    );
    assert!(
        joined.contains("{env_names_toml}"),
        "wsl-codex-cli's [mcp] args must use {{env_names_toml}}, got {:?}",
        mcp.args
    );
}

/// Codex CLI ends a tool call after its own `tool_timeout_sec` (60 seconds
/// unless set), well inside the daemon's own bounds on `wait_for_status`,
/// `wait_for_report` and `send_handoff`, so a bundled Codex profile must lift
/// it far enough that only the daemon's deadlines end a call.
#[test]
fn codex_profiles_lift_the_client_tool_timeout_above_the_daemon_deadlines() {
    const DAY_SECS: f64 = 86_400.0;
    for id in ["codex-cli", "wsl-codex-cli"] {
        let profile = parse_repo_profile(id);
        let mcp = profile
            .mcp
            .as_ref()
            .unwrap_or_else(|| panic!("{id} must ship an [mcp] table"));

        let timeout = mcp
            .args
            .windows(2)
            .filter(|pair| pair[0] == "-c")
            .find_map(|pair| pair[1].strip_prefix("mcp_servers.{server}.tool_timeout_sec="))
            .unwrap_or_else(|| {
                panic!(
                    "{id}'s [mcp] args must set mcp_servers.{{server}}.tool_timeout_sec, got {:?}",
                    mcp.args
                )
            });
        let seconds: f64 = timeout
            .parse()
            .unwrap_or_else(|err| panic!("{id}'s tool_timeout_sec {timeout:?}: {err}"));
        assert!(
            seconds >= DAY_SECS,
            "{id}'s tool_timeout_sec ({seconds}) must be at least a day so the daemon's own \
             deadlines, not Codex's, end a call"
        );
    }
}

#[test]
fn shell_profile_ships_no_mcp_table() {
    let profile = parse_repo_profile("shell");

    assert!(
        profile.mcp.is_none(),
        "the shell profile has no injection point, so it must ship no [mcp] table"
    );
}

#[test]
fn antigravity_cli_family_profiles_ship_no_mcp_table() {
    for id in ["antigravity-cli", "wsl-antigravity-cli"] {
        let profile = parse_repo_profile(id);

        assert!(
            profile.mcp.is_none(),
            "{id} has no per-launch MCP injection point (agy reads MCP servers only from \
             its own global registration), so it must ship no [mcp] table"
        );
    }
}

// ---------------------------------------------------------------------
// Both WSL profiles exec the CLI without going through a shell
// ---------------------------------------------------------------------

#[test]
fn wsl_claude_code_windows_argv_execs_the_cli_through_a_fixed_wrapper() {
    let argv = windows_argv("wsl-claude-code");

    assert_eq!(
        argv,
        vec!["wsl", "-e", "bash", "-lic", "exec \"$0\" \"$@\"", "claude"],
        "wsl-claude-code's windows argv must be the fixed exec wrapper, got {argv:?}"
    );
}

#[test]
fn wsl_codex_cli_windows_argv_execs_the_cli_through_a_fixed_wrapper() {
    let argv = windows_argv("wsl-codex-cli");

    assert_eq!(
        argv,
        vec!["wsl", "-e", "bash", "-lic", "exec \"$0\" \"$@\"", "codex"],
        "wsl-codex-cli's windows argv must be the fixed exec wrapper, got {argv:?}"
    );
}

#[test]
fn antigravity_cli_windows_argv_execs_agy_directly() {
    let argv = windows_argv("antigravity-cli");

    assert_eq!(
        argv,
        vec!["agy"],
        "antigravity-cli's windows argv must start agy directly, got {argv:?}"
    );
}

#[test]
fn wsl_antigravity_cli_windows_argv_execs_the_cli_through_a_fixed_wrapper() {
    let argv = windows_argv("wsl-antigravity-cli");

    assert_eq!(
        argv,
        vec!["wsl", "-e", "bash", "-lic", "exec \"$0\" \"$@\"", "agy"],
        "wsl-antigravity-cli's windows argv must be the fixed exec wrapper, got {argv:?}"
    );
}

// ---------------------------------------------------------------------
// Each bundled profile's [model.choices] table
// ---------------------------------------------------------------------

/// The ids of every `profiles/*.toml` this repository ships, sorted.
fn bundled_profile_ids() -> Vec<String> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../profiles");
    let mut ids: Vec<String> = std::fs::read_dir(&dir)
        .unwrap_or_else(|err| panic!("the bundled profiles directory must be readable: {err}"))
        .map(|entry| entry.expect("a directory entry must be readable").path())
        .filter(|path| path.extension().and_then(std::ffi::OsStr::to_str) == Some("toml"))
        .map(|path| {
            path.file_stem()
                .and_then(std::ffi::OsStr::to_str)
                .expect("a bundled profile's file name is UTF-8")
                .to_string()
        })
        .collect();
    ids.sort();
    ids
}

fn model_choices(profile: &Profile) -> &ModelChoicesSpec {
    profile
        .options
        .model_choices
        .as_ref()
        .unwrap_or_else(|| panic!("{} must ship a [model.choices] table", profile.id))
}

/// Bundled profiles with no source to offer model choices from: the shell
/// maps no model, and Copilot neither documents its model identifiers nor
/// lists them through a command.
const WITHOUT_MODEL_CHOICES: &[&str] = &["copilot-cli", "shell", "wsl-copilot-cli"];

#[test]
fn every_bundled_profile_with_a_model_list_source_ships_model_choices() {
    let ids = bundled_profile_ids();
    assert_eq!(ids.len(), 13, "the bundled set this test covers: {ids:?}");
    let mut with_choices = Vec::new();
    for id in &ids {
        let profile = parse_repo_profile(id);
        if profile.options.model_choices.is_some() {
            with_choices.push(id.clone());
        }
    }
    let expected: Vec<String> = ids
        .iter()
        .filter(|id| !WITHOUT_MODEL_CHOICES.contains(&id.as_str()))
        .cloned()
        .collect();
    assert_eq!(
        with_choices, expected,
        "every bundled profile whose models a CLI command or document lists ships where its \
         choices come from, and the others ship none"
    );
}

#[test]
fn the_claude_code_family_ships_the_values_its_aliases_already_point_at() {
    for id in ["claude-code", "wsl-claude-code"] {
        let profile = parse_repo_profile(id);
        let alias_values: Vec<String> = profile
            .options
            .model
            .as_ref()
            .unwrap_or_else(|| panic!("{id} must map the model axis"))
            .aliases
            .iter()
            .map(|(_, value)| value.clone())
            .collect();
        match model_choices(&profile) {
            ModelChoicesSpec::Values(values) => {
                assert_eq!(values.len(), 4, "{id} ships four fixed choices: {values:?}");
                assert_eq!(
                    values, &alias_values,
                    "{id}'s fixed choices are the values its own aliases point at"
                );
            }
            other => panic!("{id} ships a fixed list, got {other:?}"),
        }
    }
}

#[test]
fn the_antigravity_cli_family_lists_models_as_tab_separated_lines() {
    for id in ["antigravity-cli", "wsl-antigravity-cli"] {
        let profile = parse_repo_profile(id);
        match model_choices(&profile) {
            ModelChoicesSpec::Command {
                args,
                format: OutputFormat::Lines { separator },
            } => {
                assert_eq!(args, &vec!["models".to_string()], "{id}'s listing args");
                assert_eq!(separator, "\t", "{id}'s lines are tab separated");
            }
            other => panic!("{id} lists models through a line-by-line command, got {other:?}"),
        }
    }
}

#[test]
fn the_codex_cli_family_lists_models_as_json_filtered_to_the_listed_ones() {
    for id in ["codex-cli", "wsl-codex-cli"] {
        let profile = parse_repo_profile(id);
        match model_choices(&profile) {
            ModelChoicesSpec::Command {
                args,
                format:
                    OutputFormat::Json {
                        items,
                        id: id_pointer,
                        label,
                        filter,
                    },
            } => {
                assert_eq!(
                    args,
                    &vec!["debug".to_string(), "models".to_string()],
                    "{id}'s listing args"
                );
                assert_eq!(items, "/models", "{id}'s items pointer");
                assert_eq!(id_pointer, "/slug", "{id}'s id pointer");
                assert_eq!(
                    label.as_deref(),
                    Some("/display_name"),
                    "{id}'s label pointer"
                );
                match filter {
                    Some(JsonFilter { pointer, equals }) => {
                        assert_eq!(pointer, "/visibility", "{id}'s filter pointer");
                        assert_eq!(equals, "list", "{id}'s filter value");
                    }
                    None => panic!("{id} must keep hidden entries out of its choices"),
                }
            }
            other => panic!("{id} lists models through a json command, got {other:?}"),
        }
    }
}

// ---------------------------------------------------------------------
// Hooks that outlive a launch call `--hook <profile-id> <Event>`
// ---------------------------------------------------------------------

/// The profiles whose hook text stays in a CLI's settings (a registration's
/// `json_entry`, or Codex's trust record) and so must not change with a version.
const HOOK_DOOR_PROFILES: [&str; 7] = [
    "antigravity-cli",
    "wsl-antigravity-cli",
    "grok-build",
    "wsl-grok-build",
    "muse-code",
    "codex-cli",
    "wsl-codex-cli",
];

/// Flags of the doors that each named one handling, before `--hook` replaced them.
const OLD_DOOR_FLAGS: [&str; 5] = [
    "--emit-status",
    "--deliver-",
    "--report-session",
    "--skip-if",
    "--source",
];

/// One hook command line with the event it runs for.
type HookLine = (String, String);

/// Every hook a profile ships, as one list of command lines per way it is written: the
/// registration's `json_entry` rendered for each OS, and the `[hooks]` arguments.
fn shipped_hook_commands(profile: &Profile) -> Vec<Vec<HookLine>> {
    let mut renderings = Vec::new();
    if let Some(spec) = profile
        .mcp_registration
        .as_ref()
        .and_then(|spec| spec.json_entry())
    {
        let target = RegistrationTarget::for_instance(None);
        for os in [HostOs::Windows, HostOs::MacOs, HostOs::Linux] {
            let vars = EntryVars {
                target: &target,
                os,
                daemon: Some("/yagura/yagura-daemon"),
                bridge: Some("/yagura/yagura-mcp"),
            };
            let mut lines = Vec::new();
            for file in spec.files() {
                for (_, value) in file.entries(&vars).expect("the entries render") {
                    // An entry that maps events to handlers; the MCP server entry does not.
                    let Some(events) = value
                        .as_object()
                        .filter(|map| map.values().all(serde_json::Value::is_array))
                    else {
                        continue;
                    };
                    for (event, groups) in events {
                        collect_command_lines(event, groups, &mut lines);
                    }
                }
            }
            renderings.push(lines);
        }
    }
    if let Some(spec) = &profile.hooks {
        let mut lines = Vec::new();
        for arg in render_hooks_args(spec, InjectedPaths::default()) {
            if !arg.starts_with("hooks.") {
                continue;
            }
            let document: toml::Table = arg
                .parse()
                .unwrap_or_else(|err| panic!("{arg} must decode as TOML: {err}"));
            for (event, groups) in document["hooks"].as_table().expect("a hooks table") {
                let groups = serde_json::to_value(groups).expect("groups are data");
                collect_command_lines(event, &groups, &mut lines);
            }
        }
        renderings.push(lines);
    }
    renderings
}

/// Adds the command line (command, then `args`) of every handler under `value`.
fn collect_command_lines(event: &str, value: &serde_json::Value, out: &mut Vec<HookLine>) {
    match value {
        serde_json::Value::Array(items) => {
            for item in items {
                collect_command_lines(event, item, out);
            }
        }
        serde_json::Value::Object(map) => {
            if let Some(command) = map.get("command").and_then(serde_json::Value::as_str) {
                let mut words = vec![command];
                if let Some(args) = map.get("args").and_then(serde_json::Value::as_array) {
                    words.extend(args.iter().filter_map(serde_json::Value::as_str));
                }
                out.push((event.to_string(), words.join(" ")));
            }
            if let Some(inner) = map.get("hooks") {
                collect_command_lines(event, inner, out);
            }
        }
        _ => {}
    }
}

#[test]
fn every_hook_a_cli_keeps_calls_the_hook_door_of_its_own_profile_with_one_handler_per_event() {
    for id in HOOK_DOOR_PROFILES {
        let profile = parse_repo_profile(id);
        let events = profile
            .hook_events
            .as_ref()
            .unwrap_or_else(|| panic!("{id} must ship a [hook_events] table"));
        let renderings = shipped_hook_commands(&profile);
        assert!(!renderings.is_empty(), "{id} ships hook commands");
        for lines in &renderings {
            assert!(!lines.is_empty(), "{id}: a rendering with no hook commands");
            let mut per_event = std::collections::BTreeMap::<&str, usize>::new();
            for (event, line) in lines {
                *per_event.entry(event.as_str()).or_default() += 1;
                assert!(
                    events.get(event).is_some(),
                    "{id}: {event} is hooked but has no [hook_events] entry"
                );
                let tokens: Vec<&str> = line
                    .split(|c: char| c.is_whitespace() || c == ')')
                    .filter(|token| !token.is_empty())
                    .collect();
                let at = tokens
                    .iter()
                    .position(|token| *token == "--hook")
                    .unwrap_or_else(|| panic!("{id}/{event}: no --hook door in {line:?}"));
                assert_eq!(
                    tokens[at + 1..],
                    [id, event.as_str()],
                    "{id}/{event}: the door names this profile and this event: {line:?}"
                );
                for flag in OLD_DOOR_FLAGS {
                    assert!(
                        !line.contains(flag),
                        "{id}/{event}: {flag} belongs to the older doors: {line:?}"
                    );
                }
            }
            assert!(
                per_event.values().all(|count| *count == 1),
                "{id}: an event's handlings are one handler, got {per_event:?}"
            );
        }
    }
}

fn event(
    status: Option<WindowStatus>,
    deliver: Option<HookDeliver>,
    session: Option<&str>,
    skip_if: Option<&str>,
) -> HookEvent {
    HookEvent {
        status,
        deliver,
        session: session.map(str::to_string),
        skip_if: skip_if.map(str::to_string),
    }
}

/// Each event's handling, summed from the flags of the handlers that ran for it:
/// `--emit-status X` is `status`, `--deliver-take` / `--deliver-context` is `deliver`,
/// `--report-session P` is `session`, `--skip-if P` is `skip_if`.
fn expected_hook_events(id: &str) -> Vec<(&'static str, HookEvent)> {
    use HookDeliver::{Context, Take};
    use WindowStatus::{Running, WaitingApproval, WaitingInput};
    match id {
        "antigravity-cli" | "wsl-antigravity-cli" => vec![
            (
                "PreInvocation",
                event(Some(Running), Some(Take), None, None),
            ),
            ("SessionStart", event(None, Some(Context), None, None)),
            ("PostToolUse", event(Some(Running), None, None, None)),
        ],
        "grok-build" | "wsl-grok-build" => {
            let skip = Some("/subagentType");
            vec![
                ("UserPromptSubmit", event(Some(Running), None, None, skip)),
                ("PreToolUse", event(Some(Running), None, None, skip)),
                ("PostToolUse", event(Some(Running), Some(Take), None, skip)),
                (
                    "PostToolUseFailure",
                    event(Some(Running), Some(Take), None, skip),
                ),
                (
                    "Notification",
                    event(Some(WaitingApproval), None, None, skip),
                ),
                ("Stop", event(Some(WaitingInput), None, None, skip)),
                ("StopFailure", event(Some(WaitingInput), None, None, skip)),
                ("StopCancelled", event(Some(WaitingInput), None, None, skip)),
            ]
        }
        "muse-code" => vec![
            ("SessionStart", event(None, Some(Context), None, None)),
            (
                "UserPromptSubmit",
                event(Some(Running), Some(Take), None, None),
            ),
            (
                "PermissionRequest",
                event(Some(WaitingApproval), None, None, None),
            ),
            ("Stop", event(Some(WaitingInput), None, None, None)),
            ("StopFailure", event(Some(WaitingInput), None, None, None)),
            ("Interrupt", event(Some(WaitingInput), None, None, None)),
        ],
        "codex-cli" | "wsl-codex-cli" => vec![
            ("SessionStart", event(None, Some(Context), None, None)),
            (
                "UserPromptSubmit",
                event(Some(Running), Some(Take), Some("/session_id"), None),
            ),
            ("PreToolUse", event(Some(Running), None, None, None)),
            ("PostToolUse", event(Some(Running), None, None, None)),
            (
                "PermissionRequest",
                event(Some(WaitingApproval), None, None, None),
            ),
            ("Stop", event(Some(WaitingInput), None, None, None)),
            ("Interrupt", event(Some(WaitingInput), None, None, None)),
        ],
        other => panic!("{other} is not one of the profiles that moved to the hook door"),
    }
}

#[test]
fn each_hook_events_table_is_the_sum_of_what_the_handlers_of_that_event_did() {
    for id in HOOK_DOOR_PROFILES {
        let profile = parse_repo_profile(id);
        let table = profile
            .hook_events
            .as_ref()
            .unwrap_or_else(|| panic!("{id} must ship a [hook_events] table"));
        let expected = expected_hook_events(id);
        for (name, handling) in &expected {
            assert_eq!(table.get(name), Some(handling), "{id}/{name}");
        }
        // No event beyond these: the raw table's keys are exactly the expected names.
        let raw: toml::Table = read_doc_verbatim(&format!("../../profiles/{id}.toml"))
            .parse()
            .expect("a shipped profile is TOML");
        let mut names: Vec<&str> = raw
            .get("hook_events")
            .and_then(toml::Value::as_table)
            .unwrap_or_else(|| panic!("{id} must ship a [hook_events] table"))
            .keys()
            .map(String::as_str)
            .collect();
        names.sort_unstable();
        let mut wanted: Vec<&str> = expected.iter().map(|(name, _)| *name).collect();
        wanted.sort_unstable();
        assert_eq!(names, wanted, "{id}: the events the table names");
    }
}

// ---------------------------------------------------------------------
// The codex family's launch-only argv
// ---------------------------------------------------------------------

/// A fixture keeping `id`'s shipped `[command]` table except its argv keys, which run
/// `hook_child` (logging its argv under `logs`) instead, plus the shipped `[delivery]`
/// table and the shipped `wake_args`.
fn launch_only_fixture(id: &str, logs: &std::path::Path) -> (String, Vec<String>) {
    let source = read_doc_verbatim(&format!("../../profiles/{id}.toml"));
    let shipped: toml::Table = toml::from_str(&source).unwrap();
    let mut command = shipped["command"].as_table().unwrap().clone();
    for key in ["default", "windows", "macos", "linux"] {
        command.remove(key);
    }
    let child = [
        env!("CARGO_BIN_EXE_hook_child").to_string(),
        yagura_test_support::process::COMPOSER_MARKER.to_string(),
        yagura_test_support::process::COMPOSER_SEPARATOR.to_string(),
        yagura_test_support::process::COMPOSER_CONTINUATION_PREFIX.to_string(),
        "0".to_string(),
        "-".to_string(),
        "--log".to_string(),
        logs.display().to_string(),
    ];
    command.insert(
        "default".to_string(),
        toml::Value::Array(child.into_iter().map(toml::Value::String).collect()),
    );
    let delivery = shipped["delivery"].clone();
    let wake_args = delivery["wake_args"]
        .as_array()
        .unwrap_or_else(|| panic!("{id} must wake by a command with wake_args"))
        .iter()
        .map(|arg| arg.as_str().unwrap().to_string())
        .collect();
    let mut fixture = toml::Table::new();
    fixture.insert("schema".to_string(), shipped["schema"].clone());
    fixture.insert("display_name".to_string(), shipped["display_name"].clone());
    fixture.insert("command".to_string(), toml::Value::Table(command));
    fixture.insert("delivery".to_string(), delivery);
    (toml::to_string(&fixture).unwrap(), wake_args)
}

#[tokio::test]
async fn the_codex_cli_family_spawns_with_no_daemon_but_wakes_without_it() {
    for id in ["codex-cli", "wsl-codex-cli"] {
        let tree = yagura_test_support::paths::TempDir::new("yagura-cli-test", "bundled-no-daemon");
        let profiles_dir = tree.make_dir("profiles");
        let logs = tree.make_dir("logs");
        let (source, wake_args) = launch_only_fixture(id, &logs);
        common::write_profile(&profiles_dir, id, &source);
        let (home, registration_cwd) = common::registration_dirs("bundled-no-daemon");
        let config = yagura_daemon::DaemonConfig {
            home_dir: Some(home),
            registration_cwd,
            profiles_dir: Some(profiles_dir),
            host_os: ProfileOs::host(),
            program_lookup: common::empty_lookup(),
            program_is_executable: std::path::Path::is_file,
            default_shell: None,
            ..common::approver_base_config()
        };
        let h = common::Harness::start("bundled-no-daemon", config).await;
        let mut client = h.connect().await;
        common::send(
            &mut client,
            &yagura_protocol::ClientMessage::OpenWindowFromProfile {
                profile: id.to_string(),
                cwd: None,
                name: None,
                model: None,
                effort: None,
                on_name_taken: Default::default(),
            },
        )
        .await;
        let window_id = match common::read_response(&mut client).await {
            yagura_protocol::DaemonMessage::WindowOpened { window_id, .. } => window_id,
            other => panic!("{id}: expected window_opened, got {other:?}"),
        };

        let args = common::launch_args_logged_in(&logs).await;
        assert!(
            args.iter().any(|arg| arg == "--no-daemon"),
            "{id}: the window's CLI is spawned with --no-daemon; argv: {args:?}"
        );
        let entry = h.registry.get(&window_id).unwrap();
        let wake = entry.wake_command().expect("the profile wakes by command");
        let woken: Vec<&String> = wake.argv.iter().chain(&wake_args).collect();
        assert!(
            !woken.iter().any(|arg| *arg == "--no-daemon"),
            "{id}: the wake command, which the CLI refuses with --no-daemon, lacks it; got {woken:?}"
        );
        h.registry.terminate_all().await;
    }
}

// ---------------------------------------------------------------------
// The time a CLI gives a hook: one source, the profile's data
// ---------------------------------------------------------------------

/// Every text a profile's hooks are written into a CLI's settings with: each
/// registration `json_entry` rendered for every OS, and the `[hooks]` injection (its
/// arguments, with the paths a launch picks put back as placeholders, and the content of
/// its settings file and directory).
fn hook_registration_text(profile: &Profile) -> String {
    let mut text = String::new();
    let target = RegistrationTarget::for_instance(None);
    if let Some(spec) = profile
        .mcp_registration
        .as_ref()
        .and_then(|spec| spec.json_entry())
    {
        for os in [HostOs::Windows, HostOs::MacOs, HostOs::Linux] {
            let vars = EntryVars {
                target: &target,
                os,
                daemon: Some("/yagura/yagura-daemon"),
                bridge: Some("/yagura/yagura-mcp"),
            };
            for file in spec.files() {
                for (key, value) in file.entries(&vars).expect("the entries render") {
                    text.push_str(&format!("{os:?} {key} {value}\n"));
                }
            }
        }
    }
    if let Some(spec) = &profile.hooks {
        let injection = prepare_hooks_injection(spec, Some(std::path::Path::new("cfg")))
            .expect("a shipped [hooks] table renders");
        let file_path = injection
            .config_file
            .as_ref()
            .map(|file| file.path().display().to_string());
        let dir_path = injection
            .config_dir
            .as_ref()
            .map(|dir| dir.path().display().to_string());
        for arg in &injection.args {
            let mut arg = arg.clone();
            for (path, placeholder) in [(&file_path, "{file}"), (&dir_path, "{dir}")] {
                if let Some(path) = path {
                    arg = arg.replace(path.as_str(), placeholder);
                }
            }
            text.push_str(&format!("arg {arg}\n"));
        }
        if let Some(file) = &injection.config_file {
            text.push_str(&format!("file {}\n", file.content));
        }
        if let Some(dir) = &injection.config_dir {
            for (name, content) in &dir.files {
                text.push_str(&format!("dir {name} {content}\n"));
            }
        }
    }
    text
}

/// The sha256 of [`hook_registration_text`] for every shipped profile that writes hooks, as
/// the build before the time a hook has moved into the profile's data rendered it.
/// A registered hook whose text changes is one a CLI no longer recognizes (Codex keeps a
/// hash of each, and the JSON registrations are compared with the CLI's settings), so the
/// data must render the same text.
const HOOK_REGISTRATION_TEXT_SHA256: &[(&str, &str)] = &[
    (
        "antigravity-cli",
        "1bffedb10965434acd8cf5d42b24cecb613c38b16059756935826b20b8d11939",
    ),
    (
        "claude-code",
        "7eb52cfd916af6d83b44317f043a352b64270ef43bef78ddd3670cca4e9953fb",
    ),
    (
        "codex-cli",
        "be0e13bf5cd2a5cd9cf8f00c7af76f40ca5cfb82e41de165f67c7d3a389c96f2",
    ),
    (
        "copilot-cli",
        "3d8ffcd33ed225b69eaaa2aad4377af8acbc342602a5368720b57a216f427add",
    ),
    (
        "grok-build",
        "fb311bce0a32936f98185b5a8703cd9ca8146c316e414bb6c6c91d1b434314ac",
    ),
    (
        "muse-code",
        "17c850dac4eaa24c95fd3488397832d8267da9357d7c7b6d540b9a6314c8fd7e",
    ),
    (
        "wsl-antigravity-cli",
        "1492c21f0632ddc2af88dd426ab097a8a33b0b3509358d17abdad942cf376adb",
    ),
    (
        "wsl-claude-code",
        "5a9ab51f54a14f0d39b8f25a0d77fcf9544c64aff8debb35a81402c27b1cfc9c",
    ),
    (
        "wsl-codex-cli",
        "def25e055f30196298e30101e9a53142079a8000ab775d7c69544ee76489a720",
    ),
    (
        "wsl-copilot-cli",
        "5fe4e1cee9a1c629d568f47f3fec237d0302c047d894bf34507cd92e79549a3e",
    ),
    (
        "wsl-grok-build",
        "912215a46894603ebe5decb1df569458806728cbf9bb91c97241b7db6f449497",
    ),
    (
        "wsl-muse-code",
        "8b4adac523b2017d2001c0f7c526b408ed44ee711c4c098e8d580f7b8977b29c",
    ),
];

fn sha256_hex(text: &str) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(text.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[test]
fn the_hook_registrations_a_cli_keeps_render_the_same_text_from_the_profiles_data() {
    let measured: Vec<(String, String)> = bundled_profile_ids()
        .into_iter()
        .filter_map(|id| {
            let text = hook_registration_text(&parse_repo_profile(&id));
            (!text.is_empty()).then(|| (id, sha256_hex(&text)))
        })
        .collect();
    let expected: Vec<(String, String)> = HOOK_REGISTRATION_TEXT_SHA256
        .iter()
        .map(|(id, hash)| ((*id).to_string(), (*hash).to_string()))
        .collect();
    assert_eq!(
        measured, expected,
        "the hook texts a CLI keeps must not change with where their values come from"
    );
}

/// Adds `(event, timeout)` for every handler under `value` whose command runs the hook
/// door of `profile`; `timeout` is the one the handler declares.
fn collect_hook_timeouts(
    profile: &str,
    event: &str,
    value: &serde_json::Value,
    out: &mut Vec<(String, Option<u64>)>,
) {
    match value {
        serde_json::Value::Array(items) => {
            for item in items {
                collect_hook_timeouts(profile, event, item, out);
            }
        }
        serde_json::Value::Object(map) => {
            if let Some(command) = map.get("command").and_then(serde_json::Value::as_str) {
                let mut words = vec![command];
                if let Some(args) = map.get("args").and_then(serde_json::Value::as_array) {
                    words.extend(args.iter().filter_map(serde_json::Value::as_str));
                }
                let line = words.join(" ");
                if line.contains(&format!("--hook {profile} {event}")) {
                    let timeout = map.get("timeout").and_then(serde_json::Value::as_u64);
                    let entry = (event.to_string(), timeout);
                    if !out.contains(&entry) {
                        out.push(entry);
                    }
                }
            }
            if let Some(inner) = map.get("hooks") {
                collect_hook_timeouts(profile, event, inner, out);
            }
        }
        _ => {}
    }
}

/// Every `--hook` door a profile registers, with the timeout its text gives the CLI.
fn registered_hook_timeouts(id: &str) -> Vec<(String, Option<u64>)> {
    let profile = parse_repo_profile(id);
    let mut out = Vec::new();
    if let Some(spec) = profile
        .mcp_registration
        .as_ref()
        .and_then(|spec| spec.json_entry())
    {
        let target = RegistrationTarget::for_instance(None);
        for os in [HostOs::Windows, HostOs::MacOs, HostOs::Linux] {
            let vars = EntryVars {
                target: &target,
                os,
                daemon: Some("/yagura/yagura-daemon"),
                bridge: Some("/yagura/yagura-mcp"),
            };
            for file in spec.files() {
                for (_, value) in file.entries(&vars).expect("the entries render") {
                    let Some(events) = value
                        .as_object()
                        .filter(|map| map.values().all(serde_json::Value::is_array))
                    else {
                        continue;
                    };
                    for (event, groups) in events {
                        collect_hook_timeouts(id, event, groups, &mut out);
                    }
                }
            }
        }
    }
    if let Some(spec) = &profile.hooks {
        for arg in render_hooks_args(spec, InjectedPaths::default()) {
            if !arg.starts_with("hooks.") {
                continue;
            }
            let document: toml::Table = arg
                .parse()
                .unwrap_or_else(|err| panic!("{arg} must decode as TOML: {err}"));
            for (event, groups) in document["hooks"].as_table().expect("a hooks table") {
                let groups = serde_json::to_value(groups).expect("groups are data");
                collect_hook_timeouts(id, event, &groups, &mut out);
            }
        }
    }
    out
}

/// What the daemon answers a door's read of `--hook <profile> <event>` with, as its wire
/// form: the values of the hook's input it asks for are all reported absent.
async fn hook_door_answer(
    h: &common::Harness,
    window_id: &str,
    profile: &str,
    event: &str,
) -> serde_json::Value {
    use yagura_protocol::{ClientMessage, DaemonMessage};
    let read = |input: Option<Vec<serde_json::Value>>| -> ClientMessage {
        let mut door = serde_json::json!({ "kind": "hook", "profile": profile, "name": event });
        if let Some(input) = input {
            door["input"] = serde_json::Value::Array(input);
        }
        let wire = serde_json::json!({
            "type": "door_read",
            "window_id": window_id,
            "token": common::VALID_TOKEN,
            "door": door,
        });
        serde_json::from_value(wire.clone())
            .unwrap_or_else(|err| panic!("the wire must know a door's read {wire}: {err}"))
    };
    let mut client = h.connect().await;
    let mut answer = common::call_door(&mut client, &read(None)).await;
    if let DaemonMessage::HookInputWanted { pointers } = &answer {
        let input = vec![serde_json::json!("absent"); pointers.len()];
        answer = common::call_door(&mut client, &read(Some(input))).await;
    }
    serde_json::to_value(&answer).expect("a daemon message serializes")
}

/// The time the daemon names to a `--hook` door is the one the profile's registration
/// gives the CLI for that hook, so the two come from one value. A registration that names
/// no time (Codex's `-c` handlers) leaves the CLI's own, which the daemon names too.
#[tokio::test]
async fn the_daemon_names_each_hook_door_the_time_its_registration_gives_the_cli() {
    let dir = common::unique_temp_dir("bundled-hook-deadline");
    let mut config = common::detection_config(&dir, common::approver_base_config());
    config.bundled_profiles = true;
    let h = common::Harness::start("bundled-hook-deadline", config).await;
    let mut client = h.connect().await;
    let window_id = h.open_idle_window(&mut client).await;
    for id in ["antigravity-cli", "grok-build", "muse-code", "codex-cli"] {
        let hooks = registered_hook_timeouts(id);
        assert!(!hooks.is_empty(), "{id} registers --hook doors");
        for (event, registered) in hooks {
            let answer = hook_door_answer(&h, &window_id, id, &event).await;
            assert_eq!(
                answer["type"], "delivery",
                "{id}/{event}: the read is answered with a delivery: {answer}"
            );
            let named = answer["hook_timeout_sec"].as_u64();
            match registered {
                Some(seconds) => assert_eq!(
                    named,
                    Some(seconds),
                    "{id}/{event}: the daemon must name the time the registration gives: {answer}"
                ),
                None => assert!(
                    named.is_some_and(|seconds| seconds > 0),
                    "{id}/{event}: the daemon must name the CLI's own time: {answer}"
                ),
            }
        }
    }
}
