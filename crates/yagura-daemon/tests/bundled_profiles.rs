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

mod common;

use common::docs::read_doc_verbatim;
use yagura_daemon::profile::{
    JsonFilter, ModelChoicesSpec, OutputFormat, Profile, ProfileCommand, ProfileOs, argv_for_os,
    parse_profile,
};

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

#[test]
fn every_bundled_profile_but_the_shell_ships_model_choices() {
    let ids = bundled_profile_ids();
    assert_eq!(ids.len(), 7, "the bundled set this test covers: {ids:?}");
    let mut with_choices = Vec::new();
    for id in &ids {
        let profile = parse_repo_profile(id);
        if profile.options.model_choices.is_some() {
            with_choices.push(id.clone());
        }
    }
    let expected: Vec<String> = ids.iter().filter(|id| *id != "shell").cloned().collect();
    assert_eq!(
        with_choices, expected,
        "every bundled profile that maps a model ships where its choices come from, and the \
         shell, which maps none, ships none"
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
