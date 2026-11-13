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
use yagura_daemon::mcp_registration::{
    EntryVars, RegistrationState, RegistrationTarget, StatusObservation, derive_status,
};
use yagura_daemon::profile::{
    InjectedPaths, JsonFilter, ModelChoicesSpec, OutputFormat, Profile, ProfileCommand, ProfileOs,
    argv_for_os, parse_profile, prepare_hooks_injection, prepare_mcp_injection, render_hooks_args,
};
use yagura_daemon::profile_hook_events::{HookDeliver, HookEvent};
use yagura_os::program::{HostOs, Platform};
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
// The WSL profiles exec the CLI through a fixed wrapper, no value spliced into it
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

/// The WSL agy profile's `bash -c` runs one fixed statement before the CLI -- it puts the
/// CLI's root in its environment -- and the statement ends by handing every argument the
/// daemon appends on to the CLI through the same login-shell wrapper the other WSL profiles
/// use. The argv is still fixed: no value of a launch is spliced into the statement.
#[test]
fn wsl_antigravity_cli_windows_argv_execs_the_cli_through_a_fixed_wrapper() {
    let argv = windows_argv("wsl-antigravity-cli");

    assert_eq!(
        argv.len(),
        6,
        "wsl-antigravity-cli's windows argv is `wsl -e bash -c <statement> agy`, got {argv:?}"
    );
    assert_eq!(argv[..4], ["wsl", "-e", "bash", "-c"], "{argv:?}");
    assert_eq!(argv[5], "agy", "{argv:?}");
    assert!(
        argv[4]
            .trim_end()
            .ends_with("exec bash -lic 'exec \"$0\" \"$@\"' \"$0\" \"$@\""),
        "the statement ends by handing $0 and the daemon's arguments to the CLI: {:?}",
        argv[4]
    );
}

/// What the WSL agy profile's launch statement does when it runs, as bash in a WSL
/// distribution runs it: the arguments the daemon appends reach the CLI as they were given
/// (a model and a launch prompt with a space), and the CLI starts with a root in its
/// environment that names its own process id. Only a Linux has the files it reads.
#[test]
fn the_wsl_agy_launch_statement_hands_its_arguments_on_and_puts_a_root_in_the_environment() {
    if !cfg!(target_os = "linux") {
        return;
    }
    let probe = yagura_test_support::process::argv_probe_path();
    let shown = crate::hook_events_door::run_guest_statement(
        &probe,
        &["--model", "gemini-model", "-i", "a prompt with spaces"],
    );
    for expected in [
        "<<argv[1]=--model>>",
        "<<argv[2]=gemini-model>>",
        "<<argv[3]=-i>>",
        "<<argv[4]=a prompt with spaces>>",
    ] {
        assert!(
            shown.contains(expected),
            "the CLI must be started with the arguments the daemon appended; missing \n             {expected} in {shown:?}"
        );
    }

    let (pid, added) = crate::hook_events_door::guest_root_variables();
    let root = added
        .iter()
        .find(|(name, _)| name == crate::hook_events_door::GUEST_ROOT_VAR)
        .map(|(_, value)| value.clone())
        .unwrap_or_else(|| {
            panic!(
                "the statement must export {} for the CLI it starts; it added {added:?}",
                crate::hook_events_door::GUEST_ROOT_VAR
            )
        });
    let boot_id =
        std::fs::read_to_string("/proc/sys/kernel/random/boot_id").expect("a Linux has a boot id");
    let parts: Vec<&str> = root.split(':').collect();
    assert_eq!(
        parts.len(),
        3,
        "the root is <pid>:<boot_id>:<starttime>, got {root:?}"
    );
    assert_eq!(
        parts[0], pid,
        "the root names the CLI's own process: {root:?}"
    );
    assert_eq!(
        parts[1],
        boot_id.trim(),
        "the root names this boot: {root:?}"
    );
    assert!(
        !parts[2].is_empty() && parts[2].chars().all(|c| c.is_ascii_digit()),
        "the root ends in the CLI's start time: {root:?}"
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

/// The profiles whose every hook calls the `--hook` door: those whose hook text stays in
/// a CLI's settings (a registration's `json_entry`, or Codex's trust record) and so must
/// not change with a version, and Copilot's, whose events the door's table tells apart.
const HOOK_DOOR_PROFILES: [&str; 9] = [
    "antigravity-cli",
    "wsl-antigravity-cli",
    "grok-build",
    "wsl-grok-build",
    "muse-code",
    "codex-cli",
    "wsl-codex-cli",
    "copilot-cli",
    "wsl-copilot-cli",
];

/// The keys a hook handler of Copilot's `hooks.json` names its command line under, one
/// per shell it runs in.
const SHELL_KEYS: [&str; 2] = ["bash", "powershell"];

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
/// registration's `json_entry` rendered for each OS, the `[hooks]` arguments, and each
/// shell's command lines of a `[hooks]` directory's `hooks.json`.
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
            // The hooks a launch on `os` hands its CLI in a file, when the profile
            // chooses them per operating system.
            if profile.hooks.is_none()
                && let Some(file) = profile
                    .launch_tables
                    .for_os(os)
                    .hooks
                    .as_ref()
                    .and_then(|spec| spec.config_file.as_deref())
            {
                let document: serde_json::Value =
                    serde_json::from_str(file).expect("a hooks file is JSON");
                for (event, groups) in document["hooks"].as_object().into_iter().flatten() {
                    collect_command_lines(event, groups, &mut lines);
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
        if !lines.is_empty() {
            renderings.push(lines);
        }
        if let Some(hooks_json) = spec
            .config_dir
            .as_ref()
            .and_then(|dir| dir.get("hooks.json"))
        {
            let document: serde_json::Value =
                serde_json::from_str(hooks_json).expect("hooks.json is JSON");
            for shell in SHELL_KEYS {
                let mut lines = Vec::new();
                for (event, handlers) in document["hooks"].as_object().expect("a hooks object") {
                    for handler in handlers.as_array().expect("an array of handlers") {
                        if let Some(line) = handler[shell].as_str() {
                            lines.push((event.clone(), line.to_string()));
                        }
                    }
                }
                if !lines.is_empty() {
                    renderings.push(lines);
                }
            }
        }
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
                if id == "wsl-antigravity-cli" {
                    // The guest's command hands the door what it collected after the pair.
                    assert_eq!(
                        tokens[at + 1..].get(..2),
                        Some(&[id, event.as_str()][..]),
                        "{id}/{event}: the door names this profile and this event first: \n                         {line:?}"
                    );
                } else {
                    assert_eq!(
                        tokens[at + 1..],
                        [id, event.as_str()],
                        "{id}/{event}: the door names this profile and this event: {line:?}"
                    );
                }
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
) -> HookEvent {
    HookEvent {
        status,
        deliver,
        session: session.map(str::to_string),
        when: std::collections::BTreeMap::new(),
    }
}

/// A stop whose door hands what waits over as a block, when the hook's input holds `when`.
fn blocking_stop(when: &[(&str, serde_json::Value)]) -> HookEvent {
    HookEvent {
        when: when
            .iter()
            .map(|(pointer, value)| ((*pointer).to_string(), value.clone()))
            .collect(),
        ..event(
            Some(WindowStatus::WaitingInput),
            Some(HookDeliver::Block),
            None,
        )
    }
}

/// The pointer each profile's hooks were skipped at by the older doors' `--skip-if P`, now
/// the profile's one `subagent_pointer`.
fn expected_subagent_pointer(id: &str) -> Option<&'static str> {
    match id {
        "copilot-cli" | "wsl-copilot-cli" => Some("/agentId"),
        "grok-build" | "wsl-grok-build" => Some("/subagentType"),
        "codex-cli" | "wsl-codex-cli" => Some("/agent_id"),
        _ => None,
    }
}

/// Each event's handling: what the handlers of the older doors that ran for it did
/// (`--emit-status X` is `status`, `--deliver-take` / `--deliver-context` is `deliver`,
/// `--report-session P` is `session`), with a stop's block where the CLI takes one.
fn expected_hook_events(id: &str) -> Vec<(&'static str, HookEvent)> {
    use HookDeliver::{Context, Take};
    use WindowStatus::{Running, WaitingApproval, WaitingInput};
    let not_a_continuation = ("/stop_hook_active", serde_json::json!(false));
    match id {
        "antigravity-cli" | "wsl-antigravity-cli" => vec![
            ("PreInvocation", event(Some(Running), Some(Take), None)),
            ("SessionStart", event(None, Some(Context), None)),
            ("PostToolUse", event(Some(Running), None, None)),
            (
                "Stop",
                blocking_stop(&[
                    ("/terminationReason", serde_json::json!("NO_TOOL_CALL")),
                    ("/executionNum", serde_json::json!(0)),
                ]),
            ),
        ],
        "copilot-cli" | "wsl-copilot-cli" => {
            vec![
                ("sessionStart", event(None, Some(Context), None)),
                ("userPromptSubmitted", event(Some(Running), None, None)),
                ("preToolUse", event(Some(Running), None, None)),
                ("postToolUse", event(Some(Running), Some(Take), None)),
                ("postToolUseFailure", event(Some(Running), None, None)),
                ("agentStop", blocking_stop(&[not_a_continuation])),
            ]
        }
        "grok-build" | "wsl-grok-build" => {
            vec![
                ("UserPromptSubmit", event(Some(Running), None, None)),
                ("PreToolUse", event(Some(Running), None, None)),
                ("PostToolUse", event(Some(Running), Some(Take), None)),
                ("PostToolUseFailure", event(Some(Running), Some(Take), None)),
                ("Notification", event(Some(WaitingApproval), None, None)),
                (
                    "Stop",
                    blocking_stop(&[
                        not_a_continuation,
                        ("/stopHookActive", serde_json::json!(false)),
                    ]),
                ),
                ("StopFailure", event(Some(WaitingInput), None, None)),
                ("StopCancelled", event(Some(WaitingInput), None, None)),
            ]
        }
        "muse-code" => vec![
            ("SessionStart", event(None, Some(Context), None)),
            ("UserPromptSubmit", event(Some(Running), Some(Take), None)),
            (
                "PermissionRequest",
                event(Some(WaitingApproval), None, None),
            ),
            ("Stop", blocking_stop(&[not_a_continuation])),
            ("StopFailure", event(Some(WaitingInput), None, None)),
            ("Interrupt", event(Some(WaitingInput), None, None)),
            // Declared only to be stopped at: [interrupt] answers there.
            ("PostToolUse", event(None, None, None)),
            ("PostToolUseFailure", event(None, None, None)),
        ],
        "codex-cli" | "wsl-codex-cli" => vec![
            ("SessionStart", event(None, Some(Context), None)),
            (
                "UserPromptSubmit",
                event(Some(Running), Some(Take), Some("/session_id")),
            ),
            ("PreToolUse", event(Some(Running), None, None)),
            ("PostToolUse", event(Some(Running), None, None)),
            (
                "PermissionRequest",
                event(Some(WaitingApproval), None, None),
            ),
            ("Stop", event(Some(WaitingInput), None, None)),
            ("Interrupt", event(Some(WaitingInput), None, None)),
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
        assert_eq!(
            table.subagent_pointer(),
            expected_subagent_pointer(id),
            "{id}: the pointer that tells a sub-agent's hook apart"
        );
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
        text.push_str(&hooks_injection_text(spec));
    }
    // The hooks a profile chooses per operating system, each with its own.
    if profile.hooks.is_none() {
        for os in [HostOs::Windows, HostOs::MacOs, HostOs::Linux] {
            if let Some(spec) = &profile.launch_tables.for_os(os).hooks {
                for line in hooks_injection_text(spec).lines() {
                    text.push_str(&format!("{os:?} {line}\n"));
                }
            }
        }
    }
    text
}

/// The text of a `[hooks]` injection: its arguments, with the paths a launch picks put
/// back as placeholders, and the content of its settings file and directory.
fn hooks_injection_text(spec: &yagura_daemon::profile::InjectionSpec) -> String {
    let mut text = String::new();
    {
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

/// The sha256 of [`hook_registration_text`] for every shipped profile that writes hooks: the
/// text the profile's data renders, which changes only when its hooks are changed on purpose.
/// A registered hook whose text changes is one a CLI no longer recognizes (Codex keeps a
/// hash of each, and the JSON registrations are compared with the CLI's settings), so the
/// data must render the same text wherever its values come from.
const HOOK_REGISTRATION_TEXT_SHA256: &[(&str, &str)] = &[
    (
        "antigravity-cli",
        "3a8f276af1e57b4d9305ccd60e49ac136c2c57ba9733be79e589b43c20a791f5",
    ),
    (
        "claude-code",
        "283e7e1326367cb0b4af9a3630ee6972a0b437986e44b079002e5f9e8dd227d6",
    ),
    (
        "codex-cli",
        "be0e13bf5cd2a5cd9cf8f00c7af76f40ca5cfb82e41de165f67c7d3a389c96f2",
    ),
    (
        "copilot-cli",
        "4b09a16ed9044b35100217ce218de472727930719f43362ff02bf01425258efa",
    ),
    (
        "grok-build",
        "fb311bce0a32936f98185b5a8703cd9ca8146c316e414bb6c6c91d1b434314ac",
    ),
    (
        "muse-code",
        "4ebfd0a9fc6e6e8f82b2fd493e04244b95cbaa6ad9929a434bb1ce15f5d9d33c",
    ),
    (
        "wsl-claude-code",
        "bb3116aa73f739d9cb1c2b94859a7abb58627f1eb4c3d199e8e37090297d5ce2",
    ),
    (
        "wsl-codex-cli",
        "def25e055f30196298e30101e9a53142079a8000ab775d7c69544ee76489a720",
    ),
    (
        "wsl-copilot-cli",
        "1b99a109c1739a3f1eb3021f92fb7d795f2cbde04c6e89b9c3f6e30fb414bd34",
    ),
    (
        "wsl-grok-build",
        "912215a46894603ebe5decb1df569458806728cbf9bb91c97241b7db6f449497",
    ),
    (
        "wsl-muse-code",
        "a584d593af635c51f028dceb4456c154ed5db904562e3d300a252fd24239a9a4",
    ),
];

/// The sha256 of the hook text of `wsl-antigravity-cli` before its command collected what the
/// daemon judges a guest's hook by. That command is written into the guest's `hooks.json`
/// and rewritten by the daemon at each launch, so it is not one a CLI keeps by hash; it is
/// no longer pinned to this text, and must not render it again.
const GUEST_HOOK_TEXT_BEFORE_COLLECTION_SHA256: &str =
    "d48fe04b10130a122f70876bfcb11ed08555ed1dcf070f66bfe81745a07e27b8";

fn sha256_hex(text: &str) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(text.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[test]
fn the_hook_registrations_a_cli_keeps_render_the_same_text_from_the_profiles_data() {
    let guest = hook_registration_text(&parse_repo_profile("wsl-antigravity-cli"));
    assert_ne!(
        sha256_hex(&guest),
        GUEST_HOOK_TEXT_BEFORE_COLLECTION_SHA256,
        "wsl-antigravity-cli's hook command must collect what the daemon judges a guest's \
         hook by, so its text is no longer the one it had without"
    );
    let measured: Vec<(String, String)> = bundled_profile_ids()
        .into_iter()
        .filter(|id| id != "wsl-antigravity-cli")
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

/// The time the daemon names to a `--hook` door of a window opened from a profile is the
/// one the profile's registration gives the CLI for that hook, so the two come from one
/// value. A registration that names no time (Codex's `-c` handlers) leaves the CLI's own,
/// which the daemon names too. Each window runs a quiet stand-in for its CLI, laid over the
/// shipped profile; a profile that declares its CLI's process image declares the stand-in's,
/// whose own reads are the window's.
#[tokio::test]
async fn the_daemon_names_each_hook_door_the_time_its_registration_gives_the_cli() {
    use crate::hook_events_door::{LineageCli, LineageImage, door_answer, open_from_profile};
    const DECLARES_ITS_IMAGE: &str = "antigravity-cli";
    let instance = crate::hook_events_door::unique_instance();
    let profiles_dir = common::unique_temp_dir("bundled-hook-deadline");
    std::fs::create_dir_all(&profiles_dir).expect("create the profiles directory");
    let config = yagura_daemon::DaemonConfig {
        profiles_dir: Some(profiles_dir.clone()),
        bundled_profiles: true,
        ..common::approver_base_config()
    };
    let h = common::Harness::start_on_instance(&instance, config).await;
    let image = LineageImage::install("bundled-hook-deadline");
    let mut stand_in: Option<LineageCli> = None;
    for id in ["antigravity-cli", "grok-build", "muse-code", "codex-cli"] {
        if id == DECLARES_ITS_IMAGE {
            stand_in = Some(image.write_layer(&profiles_dir, id, &instance));
        } else {
            common::write_profile(
                &profiles_dir,
                id,
                &format!(
                    "[command]\ndefault = ['{}']\n",
                    common::echo_child_path().display()
                ),
            );
        }
        let window_id = open_from_profile(&h, id).await;
        let hooks = registered_hook_timeouts(id);
        assert!(!hooks.is_empty(), "{id} registers --hook doors");
        for (event, registered) in hooks {
            let door = serde_json::json!({ "kind": "hook", "profile": id, "name": event });
            let answer = match stand_in.as_mut().filter(|_| id == DECLARES_ITS_IMAGE) {
                Some(cli) => cli.read(&h.endpoint, door).await,
                None => door_answer(&h.endpoint, &window_id, common::VALID_TOKEN, door).await,
            };
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
    if let Some(mut cli) = stand_in {
        cli.exit().await;
    }
}

// ---------------------------------------------------------------------
// The profile id each bundled bridge is started with
// ---------------------------------------------------------------------

/// The bridge path the daemon is taken to render into each profile's tables.
const RENDERED_BRIDGE: &str = "/yagura/yagura-mcp";

/// Whether `args` names the profile `id` the way a relay reads it.
fn names_profile(args: &[String], id: &str) -> bool {
    args.windows(2)
        .any(|pair| pair[0] == "--profile" && pair[1] == id)
}

fn json_strings(value: &serde_json::Value) -> Option<Vec<String>> {
    value
        .as_array()?
        .iter()
        .map(|item| item.as_str().map(str::to_string))
        .collect()
}

/// The bridge's argument lists `piece` -- one argument of a rendered `[mcp]` table, or the
/// settings file it names -- hands the CLI for the server `server`.
fn bridge_argument_lists(piece: &str, server: &str) -> Vec<Vec<String>> {
    // `{env_refs}` is filled in only at spawn, from the window's environment.
    let piece = piece.replace("{env_refs}", "{}");
    if let Ok(document) = serde_json::from_str::<serde_json::Value>(&piece) {
        return json_strings(&document["mcpServers"][server]["args"])
            .into_iter()
            .collect();
    }
    let Some((key, value)) = piece.split_once('=') else {
        return Vec::new();
    };
    if key == format!("mcp_servers.{server}.args") {
        let table: toml::Table = format!("args = {value}")
            .parse()
            .unwrap_or_else(|err| panic!("{piece} must hold a TOML array: {err}"));
        let args = table["args"].as_array().expect("an array");
        return vec![
            args.iter()
                .map(|arg| arg.as_str().expect("a string").to_string())
                .collect(),
        ];
    }
    if key == format!("mcpServers.{server}") {
        let entry: serde_json::Value = serde_json::from_str(value)
            .unwrap_or_else(|err| panic!("{piece} must hold a JSON entry: {err}"));
        return json_strings(&entry["args"]).into_iter().collect();
    }
    Vec::new()
}

/// The arguments the bridge is registered with for `profile`, as the daemon renders the
/// registration: what follows the bridge in `add`, or the arguments of the settings entry
/// that runs the bridge.
fn registered_bridge_args(profile: &Profile) -> Vec<String> {
    let spec = profile
        .mcp_registration
        .as_ref()
        .expect("a registration table");
    let target = RegistrationTarget::for_instance(None);
    let add = spec
        .render_add(&target, RENDERED_BRIDGE)
        .expect("add renders for the default instance");
    if let Some(at) = add.iter().position(|arg| arg == RENDERED_BRIDGE) {
        return add[at + 1..].to_vec();
    }
    let vars = EntryVars {
        target: &target,
        os: HostOs::Windows,
        daemon: Some("/yagura/yagura-daemon"),
        bridge: Some(RENDERED_BRIDGE),
    };
    spec.json_entry()
        .into_iter()
        .flat_map(|entries| entries.files())
        .flat_map(|file| file.entries(&vars).expect("the entries render"))
        .find(|(_, value)| value["command"] == RENDERED_BRIDGE)
        .and_then(|(_, value)| json_strings(&value["args"]))
        .expect("an entry of the registration runs the bridge")
}

/// The argv the registration has the CLI run as its server, whole: what follows `--` in
/// `add` (or, without one, `add` from the bridge on), else the bridge entry's command and
/// arguments. A server that is a shell wrapping the bridge is one argv with the shell first.
fn registered_server_argv(profile: &Profile) -> Vec<String> {
    let spec = profile
        .mcp_registration
        .as_ref()
        .expect("a registration table");
    let target = RegistrationTarget::for_instance(None);
    let add = spec
        .render_add(&target, RENDERED_BRIDGE)
        .expect("add renders for the default instance");
    if let Some(at) = add.iter().position(|arg| arg == "--") {
        return add[at + 1..].to_vec();
    }
    if let Some(at) = add.iter().position(|arg| arg.contains(RENDERED_BRIDGE)) {
        return add[at..].to_vec();
    }
    let mut argv = vec![RENDERED_BRIDGE.to_string()];
    argv.extend(registered_bridge_args(profile));
    argv
}

/// Every bundled profile starts the bridge a window's CLI runs -- the `[mcp]` injection
/// when it has one, the registration otherwise -- with the profile's own id, which is
/// how the daemon tells the window's bridge from one a nested CLI of another profile runs.
#[test]
fn every_bundled_bridge_a_window_runs_names_its_own_profile() {
    let server = RegistrationTarget::for_instance(None).server().to_string();
    let config_dir = std::env::temp_dir().join("yagura-bundled-mcp-render");
    let mut checked = Vec::new();
    for id in bundled_profile_ids() {
        let profile = parse_repo_profile(&id);
        let lists = if let Some(spec) = &profile.mcp {
            let injection = prepare_mcp_injection(
                spec,
                &server,
                std::path::Path::new(RENDERED_BRIDGE),
                Some(&config_dir),
            )
            .unwrap_or_else(|| panic!("{id}'s [mcp] table renders"));
            let mut pieces = injection.args.clone();
            pieces.extend(injection.config_file.map(|file| file.content));
            pieces
                .iter()
                .flat_map(|piece| bridge_argument_lists(piece, &server))
                .collect::<Vec<_>>()
        } else if profile.mcp_registration.is_some() {
            vec![registered_bridge_args(&profile)]
        } else {
            continue;
        };
        assert!(!lists.is_empty(), "{id}: no bridge arguments were found");
        for args in &lists {
            assert!(
                names_profile(args, &id),
                "{id}: the bridge must be started with `--profile {id}`, got {args:?}"
            );
        }
        checked.push(id);
    }
    assert!(
        checked.len() > 1,
        "the bundled profiles that start a bridge were checked: {checked:?}"
    );
}

/// The status output a bundled profile's CLI prints (or the settings file it keeps) for
/// `yagura` registered as the server `argv` (a program, then its arguments), laid out the
/// way the profile's `[mcp_registration.status]` table reads it: in JSON the arguments are
/// the sibling of the command, in lines they are the `Args:` line joined by spaces.
fn status_output(status: &toml::Table, toml_set: Option<&toml::Table>, argv: &[String]) -> String {
    let (program, args) = argv.split_first().expect("a server has a program");
    let server = "yagura";
    let read = |key: &str| status.get(key).and_then(toml::Value::as_str);
    let command = read("command").expect("the status names the command");
    if read("format") == Some("lines") {
        let mut lines = vec![format!("{server}:")];
        for (label, word) in status
            .get("require")
            .and_then(toml::Value::as_table)
            .into_iter()
            .flatten()
        {
            lines.push(format!("  {label} {}", word.as_str().expect("a word")));
        }
        lines.push(format!("  {command} {program}"));
        lines.push(format!("  Args: {}", args.join(" ")));
        return lines.join("\n") + "\n";
    }
    fn set(document: &mut serde_json::Value, pointer: &str, value: serde_json::Value) {
        let mut at = document;
        let segments: Vec<&str> = pointer.trim_start_matches('/').split('/').collect();
        for segment in &segments[..segments.len() - 1] {
            at = &mut at[*segment];
        }
        at[*segments.last().expect("a segment")] = value;
    }
    let command = command.replace("{server}", server);
    let args_at = command
        .strip_suffix("/command")
        .map(|parent| format!("{parent}/args"))
        .expect("the command's pointer ends in /command");
    let mut element = serde_json::json!({});
    set(&mut element, &command, serde_json::json!(program));
    set(&mut element, &args_at, serde_json::json!(args));
    if let Some(toml_set) = toml_set {
        for (key, pointer) in toml_set
            .get("confirm")
            .and_then(toml::Value::as_table)
            .into_iter()
            .flatten()
        {
            let value = serde_json::to_value(&toml_set["values"][key.as_str()]).expect("a value");
            set(&mut element, pointer.as_str().expect("a pointer"), value);
        }
    }
    let document = match (read("items"), read("name")) {
        (Some(items), Some(name)) => {
            set(&mut element, name, serde_json::json!(server));
            let mut document = serde_json::json!({});
            if items.is_empty() {
                document = serde_json::json!([element]);
            } else {
                set(&mut document, items, serde_json::json!([element]));
            }
            document
        }
        _ => element,
    };
    document.to_string()
}

/// A registration made before the bridge named its profile reads as differing, so the
/// Integrations row asks for an Update; the one the daemon registers now reads as
/// registered.
#[test]
fn an_older_registration_of_each_bundled_profile_reads_as_differing() {
    let target = RegistrationTarget::for_instance(None);
    let mut checked = Vec::new();
    for id in bundled_profile_ids() {
        let profile = parse_repo_profile(&id);
        let Some(spec) = &profile.mcp_registration else {
            continue;
        };
        let source: toml::Table = read_doc_verbatim(&format!("../../profiles/{id}.toml"))
            .parse()
            .expect("a bundled profile is TOML");
        let registration = source["mcp_registration"]
            .as_table()
            .expect("a registration table");
        let status = registration["status"].as_table().expect("a status table");
        let toml_set = registration.get("toml_set").and_then(toml::Value::as_table);
        let registered = registered_server_argv(&profile);
        assert!(
            names_profile(&registered, &id),
            "{id}: the registration names its profile, got {registered:?}"
        );
        let derive = |args: &[String]| {
            derive_status(
                spec,
                &StatusObservation::Exited {
                    code: Some(0),
                    stdout: status_output(status, toml_set, args).into_bytes(),
                    stderr: Vec::new(),
                },
                &target,
                RENDERED_BRIDGE,
                Platform::Windows,
            )
        };

        let current = derive(&registered);
        assert_eq!(
            current.state,
            RegistrationState::Registered,
            "{id}: what the daemon registers reads as registered: {current:?}"
        );
        let older = derive(&[RENDERED_BRIDGE.to_string()]);
        assert_eq!(
            older.state,
            RegistrationState::Differs,
            "{id}: a registration without the profile's id reads as differing: {older:?}"
        );
        checked.push(id);
    }
    assert!(
        checked.len() > 1,
        "the bundled profiles with a registration were checked: {checked:?}"
    );
}

#[cfg(unix)]
fn make_executable(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[cfg(not(unix))]
fn make_executable(_: &std::path::Path) {}

/// The shipped `wsl-grok-build` statement starts grok only with the overlay its door
/// printed: a door that ends non-zero, or ends zero having printed nothing, ends the
/// statement with 1 and one line on stderr that holds nothing the door printed, and grok
/// does not start. A door that printed an overlay hands it to grok in `GROK_CONFIG`, and
/// grok gets the arguments after the door's path. The guest is this machine when it runs
/// Linux; elsewhere the case returns without a run.
#[test]
// The shipped statement is run under bash as a guest runs it.
#[allow(clippy::disallowed_methods)]
fn wsl_grok_builds_statement_starts_grok_only_with_the_overlay_a_door_that_succeeded_printed() {
    if !cfg!(target_os = "linux") {
        return;
    }
    let document: toml::Table = read_doc_verbatim("../../profiles/wsl-grok-build.toml")
        .parse()
        .expect("the shipped WSL grok profile is TOML");
    let argv: Vec<String> = document["command"]["windows"]
        .as_array()
        .expect("the shipped WSL grok profile has a Windows argv")
        .iter()
        .map(|arg| arg.as_str().expect("an argument").to_string())
        .collect();
    let at = argv
        .iter()
        .position(|arg| arg == "-lic")
        .expect("the shipped profile starts grok through `bash -lic <statement>`");
    let statement = &argv[at + 1];

    let tree = yagura_test_support::paths::TempDir::new("yagura-grok-statement", "run");
    let home = tree.make_dir("home");
    let bin = tree.make_dir("bin");
    let record = tree.path().join("grok.log");
    let grok = bin.join("grok");
    std::fs::write(
        &grok,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"GROK_CONFIG=$GROK_CONFIG\" \"$@\" > '{}'\n",
            record.display()
        ),
    )
    .unwrap();
    make_executable(&grok);
    const OVERLAY: &str = r#"{"a":1}"#;
    for (what, door_body, started) in [
        (
            "a door that ends non-zero",
            "cat > /dev/null; printf 'SECRET-OUT'; exit 3",
            false,
        ),
        (
            "a door that prints nothing",
            "cat > /dev/null; exit 0",
            false,
        ),
        (
            "a door that prints an overlay",
            "cat > /dev/null; printf '%s' '{\"a\":1}'",
            true,
        ),
    ] {
        let door = bin.join("door");
        std::fs::write(&door, format!("#!/bin/sh\n{door_body}\n")).unwrap();
        make_executable(&door);
        let _ = std::fs::remove_file(&record);
        let output = std::process::Command::new(crate::hook_events_door::bash_path())
            .args(["-c", statement])
            .arg(&grok)
            .arg(&door)
            .args(["--model", "m"])
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &home)
            .stdin(std::process::Stdio::null())
            .output()
            .expect("run the statement");
        let stderr = String::from_utf8_lossy(&output.stderr);
        if started {
            assert_eq!(
                std::fs::read_to_string(&record).ok().as_deref(),
                Some(format!("GROK_CONFIG={OVERLAY}\n--model\nm\n").as_str()),
                "{what}: grok starts with the overlay and its own arguments; stderr: {stderr}"
            );
        } else {
            assert_eq!(
                output.status.code(),
                Some(1),
                "{what}: the statement ends with 1; stderr: {stderr}"
            );
            assert_eq!(stderr.lines().count(), 1, "{what}: one line: {stderr:?}");
            assert!(!stderr.contains("SECRET-OUT"), "{what}: {stderr:?}");
            assert!(!record.exists(), "{what}: grok must not start");
        }
    }
}
