#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: the `[mcp_registration]` tables this repository ships
//! in `profiles/*.toml` -- read from the real files, not from fixtures --
//! render each CLI's own registration command, read back each CLI's own
//! status output shape, and reach the daemon's registration listing even
//! when a placed profile predates the table.
//!
//! No real CLI is started here. The status outputs below are the shapes the
//! CLIs themselves print (captured against throwaway configuration
//! directories), fed to the pure derivation; the Grok Build rows are also run
//! end to end with their launch command swapped for the `mcp_cli_stub` fixture.

mod common;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use common::docs::read_doc_verbatim;
use common::{
    Harness, approver_base_config, empty_lookup, mcp_cli_stub_path, read_response,
    register_approver, send, unique_temp_dir, write_profile,
};
use serde_json::json;
use yagura_daemon::DaemonConfig;
use yagura_daemon::config::REPORT_EXE_NAME;
use yagura_daemon::mcp_registration::{
    McpRegistrationSpec, RegistrationState, RegistrationTarget, StatusObservation, StatusPlan,
    derive_status,
};
use yagura_daemon::profile::parse_profile;
use yagura_os::program::Platform;
use yagura_protocol::{
    ClientMessage, DaemonMessage, McpRegistrationAction, McpRegistrationCommand,
    McpRegistrationRow, McpRegistrationState, McpRegistrationTomlSet, McpRegistrationTomlValue,
};

// --- the shipped rows -----------------------------------------------------------

/// The bridge path a Windows host daemon would register.
const HOST_BRIDGE: &str = r"C:\Users\someone\yagura\target\yagura-mcp.exe";

/// The same bridge as WSL's own path conversion reports it.
const WSL_BRIDGE: &str = "/mnt/c/Users/someone/yagura/target/yagura-mcp.exe";

/// A bridge that is not this daemon's own.
const OTHER_HOST_BRIDGE: &str = r"C:\Users\someone\elsewhere\yagura-mcp.exe";

/// The instance a named-instance case registers for.
const INSTANCE: &str = "dev";

/// How one CLI's registration command is spelled.
#[derive(Clone, Copy)]
struct CliForm {
    /// Arguments between `mcp add` / `mcp remove` and the server name.
    scope: &'static [&'static str],
    /// Options only `mcp add` takes, after `scope`.
    add_options: &'static [&'static str],
    /// The flag that carries one `NAME=value` environment pair.
    env_flag: &'static str,
    /// How the CLI reports a registration's state.
    status: StatusForm,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum StatusForm {
    /// `mcp get <server> --json`, plus a settings-file edit.
    JsonCommandWithTomlSet,
    /// `mcp get <server> --json`, nothing else.
    JsonCommand,
    /// `mcp get <server>`, printed as `Prefix: value` lines.
    Lines,
    /// A JSON settings file read directly.
    JsonFile,
    /// `mcp list --json`, the server picked out of the array by name, plus a
    /// settings-file edit.
    JsonListWithTomlSet,
}

impl StatusForm {
    /// Whether the row edits its CLI's TOML settings file after `add`.
    fn edits_toml(self) -> bool {
        matches!(
            self,
            Self::JsonCommandWithTomlSet | Self::JsonListWithTomlSet
        )
    }
}

const CLAUDE_FORM: CliForm = CliForm {
    scope: &["--scope", "user"],
    add_options: &[],
    env_flag: "--env",
    status: StatusForm::Lines,
};

const CODEX_FORM: CliForm = CliForm {
    scope: &[],
    add_options: &[],
    env_flag: "--env",
    status: StatusForm::JsonCommandWithTomlSet,
};

const AGY_FORM: CliForm = CliForm {
    scope: &[],
    add_options: &[],
    env_flag: "--env",
    status: StatusForm::JsonFile,
};

const COPILOT_FORM: CliForm = CliForm {
    scope: &[],
    // Copilot's own 180-second default would end a call before the daemon's deadlines do.
    add_options: &["--timeout", "86400000"],
    env_flag: "--env",
    status: StatusForm::JsonCommand,
};

const GROK_FORM: CliForm = CliForm {
    scope: &[],
    add_options: &[],
    env_flag: "--env",
    status: StatusForm::JsonListWithTomlSet,
};

/// Every shipped profile expected to carry a registration table: its id,
/// its CLI's form, and whether it runs inside WSL.
const SHIPPED_ROWS: &[(&str, CliForm, bool)] = &[
    ("claude-code", CLAUDE_FORM, false),
    ("codex-cli", CODEX_FORM, false),
    ("antigravity-cli", AGY_FORM, false),
    ("copilot-cli", COPILOT_FORM, false),
    ("grok-build", GROK_FORM, false),
    ("wsl-claude-code", CLAUDE_FORM, true),
    ("wsl-codex-cli", CODEX_FORM, true),
    ("wsl-antigravity-cli", AGY_FORM, true),
    ("wsl-copilot-cli", COPILOT_FORM, true),
    ("wsl-grok-build", GROK_FORM, true),
];

/// Shipped profiles whose CLI has no registration command, so registering
/// only writes its settings files.
const SETTINGS_ONLY_ROWS: &[&str] = &["muse-code"];

/// Shipped profiles that carry no registration and inject the bridge and the
/// hooks on every launch instead.
const INJECTED_PER_LAUNCH: &[&str] = &["wsl-muse-code"];

/// The settings file the JSON-file status form reads.
const AGY_SETTINGS_FILE: &str = ".gemini/config/mcp_config.json";

/// The settings file the settings-file edit targets.
const CODEX_SETTINGS_FILE: &str = ".codex/config.toml";

fn shipped_source(id: &str) -> String {
    read_doc_verbatim(&format!("../../profiles/{id}.toml"))
}

fn shipped_spec(id: &str) -> McpRegistrationSpec {
    parse_profile(id, &shipped_source(id))
        .unwrap_or_else(|_| panic!("shipped profile {id} must parse as a valid profile"))
        .mcp_registration
        .unwrap_or_else(|| panic!("shipped profile {id} must carry a [mcp_registration] table"))
}

/// The shipped profile's own `[mcp_registration]` table, untyped, for the
/// keys the parsed spec keeps private.
fn raw_registration_table(id: &str) -> toml::Table {
    let document: toml::Table =
        toml::from_str(&shipped_source(id)).expect("shipped profile must be valid TOML");
    match document.get("mcp_registration") {
        Some(toml::Value::Table(table)) => table.clone(),
        other => {
            panic!("shipped profile {id} must carry a [mcp_registration] table, got {other:?}")
        }
    }
}

/// The shipped profile's `[host_paths]` table, untyped, when it carries the
/// file tools (`read`, and `replace` / `absent_exit` beside it).
fn host_file_tools(id: &str) -> Option<toml::Table> {
    let document: toml::Table =
        toml::from_str(&shipped_source(id)).expect("shipped profile must be valid TOML");
    match document.get("host_paths") {
        Some(toml::Value::Table(table)) if table.contains_key("read") => Some(table.clone()),
        _ => None,
    }
}

/// Every id under `profiles/`, derived from the directory itself so a newly
/// shipped profile is judged without editing this test.
fn every_shipped_profile_id() -> Vec<String> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../profiles");
    let mut ids: Vec<String> = std::fs::read_dir(&dir)
        .expect("profiles directory must be readable")
        .map(|entry| entry.expect("profiles directory entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "toml"))
        .map(|path| {
            path.file_stem()
                .and_then(|stem| stem.to_str())
                .expect("profile file name is UTF-8")
                .to_string()
        })
        .collect();
    ids.sort();
    ids
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| (*item).to_string()).collect()
}

fn default_target() -> RegistrationTarget {
    RegistrationTarget::for_instance(None)
}

fn named_target() -> RegistrationTarget {
    RegistrationTarget::for_instance(Some(INSTANCE))
}

fn bridge_for(is_wsl: bool) -> &'static str {
    if is_wsl { WSL_BRIDGE } else { HOST_BRIDGE }
}

/// Whether a row reads or writes a settings file of its CLI's own, as
/// opposed to reading every state from the CLI's own output.
fn reads_files(spec: &McpRegistrationSpec, form: CliForm) -> bool {
    form.status == StatusForm::JsonFile || spec.toml_set().is_some() || spec.json_entry().is_some()
}

// =============================================================================
// Which shipped profiles carry a table, and the keys each group's table has.
// =============================================================================

#[test]
fn every_shipped_profile_but_the_shell_and_those_injecting_per_launch_carries_a_registration_table()
{
    let ids = every_shipped_profile_id();
    assert!(
        ids.iter().any(|id| id == "shell"),
        "the shell profile must still be shipped, found {ids:?}"
    );
    for id in INJECTED_PER_LAUNCH {
        assert!(
            ids.iter().any(|shipped| shipped == id),
            "{id} must be shipped, found {ids:?}"
        );
    }
    for id in &ids {
        let profile = parse_profile(id, &shipped_source(id))
            .unwrap_or_else(|_| panic!("shipped profile {id} must parse as a valid profile"));
        if id == "shell" {
            assert!(
                profile.mcp_registration.is_none(),
                "the shell profile launches no CLI with an MCP registry of its own"
            );
        } else if INJECTED_PER_LAUNCH.contains(&id.as_str()) {
            assert!(
                profile.mcp_registration.is_none(),
                "{id} must not register: its CLI would refuse to start over a same-named server"
            );
            assert!(
                profile.mcp.is_some() && profile.hooks.is_some(),
                "{id} carries no registration, so every launch must inject the bridge and the \
                 hooks itself"
            );
        } else {
            assert!(
                profile.mcp_registration.is_some(),
                "shipped profile {id} launches a CLI that registers MCP servers, so it must carry \
                 a [mcp_registration] table"
            );
        }
    }

    let expected: BTreeSet<&str> = SHIPPED_ROWS
        .iter()
        .map(|(id, _, _)| *id)
        .chain(SETTINGS_ONLY_ROWS.iter().copied())
        .collect();
    let actual: BTreeSet<&str> = ids
        .iter()
        .map(String::as_str)
        .filter(|id| *id != "shell" && !INJECTED_PER_LAUNCH.contains(id))
        .collect();
    assert_eq!(
        actual, expected,
        "every shipped CLI profile must be judged by this file's per-row expectations"
    );
}

#[test]
fn host_rows_have_no_group_no_probe_and_no_file_tools() {
    for (id, form, is_wsl) in SHIPPED_ROWS {
        if *is_wsl {
            continue;
        }
        let spec = shipped_spec(id);
        assert_eq!(
            spec.group(),
            None,
            "{id} runs on the host, so it belongs to the host's own group"
        );
        assert!(
            !spec.converts_paths(),
            "{id} runs on the host, which reaches the bridge by its own path"
        );
        assert_eq!(
            spec.render_file_read(CODEX_SETTINGS_FILE),
            None,
            "{id} reads host settings files directly, not through a tool"
        );
        assert_eq!(spec.render_file_replace(CODEX_SETTINGS_FILE), None);
        let raw = raw_registration_table(id);
        assert!(
            !raw.contains_key("env_forward"),
            "{id}'s bridge runs in the same environment as the CLI, so nothing needs forwarding"
        );
        assert_eq!(
            spec.toml_set().is_some(),
            form.status.edits_toml(),
            "{id}: only the CLI whose add drops hand-written keys edits its settings file"
        );
    }
}

#[test]
fn wsl_rows_carry_the_group_the_path_probe_the_env_forward_and_the_read_tool() {
    for (id, form, is_wsl) in SHIPPED_ROWS {
        if !*is_wsl {
            continue;
        }
        let spec = shipped_spec(id);
        assert_eq!(
            spec.group(),
            Some("WSL"),
            "{id} must be shown in the WSL group"
        );
        assert_eq!(
            spec.render_probe(HOST_BRIDGE),
            Some(strings(&["wsl", "-e", "wslpath", "-u", HOST_BRIDGE])),
            "{id} must convert the bridge path with WSL's own tool, not a daemon-side imitation"
        );

        let raw = raw_registration_table(id);
        let forward = match raw.get("env_forward") {
            Some(toml::Value::Table(forward)) => forward.clone(),
            other => panic!("{id} must carry env_forward, got {other:?}"),
        };
        assert_eq!(
            forward.get("name").and_then(toml::Value::as_str),
            Some("WSLENV"),
            "{id}: a variable reaches a Windows program started from WSL only when WSLENV names it"
        );
        assert_eq!(
            forward.get("separator").and_then(toml::Value::as_str),
            Some(":"),
            "{id}: WSLENV separates names with a colon"
        );

        // A row that reads no file of its own has no use for the file tools.
        let tools = host_file_tools(id);
        if !reads_files(&spec, *form) && tools.is_none() {
            continue;
        }
        let Some(files) = tools else {
            panic!("{id} reads settings files, so its [host_paths] must carry the read tool")
        };
        assert_eq!(
            files.get("absent_exit").and_then(toml::Value::as_integer),
            Some(3),
            "{id}: the read tool's missing-file exit must be declared"
        );
        assert!(
            spec.render_file_read(AGY_SETTINGS_FILE).is_some(),
            "{id} must read files inside WSL with WSL's own tools"
        );
        assert_eq!(
            spec.render_file_replace(CODEX_SETTINGS_FILE).is_some(),
            spec.toml_set().is_some() || spec.json_entry().is_some(),
            "{id}: only the row that edits a settings file needs the replace tool"
        );
        assert_eq!(spec.toml_set().is_some(), form.status.edits_toml());
    }
}

#[test]
fn wsl_file_tools_run_inside_wsl_and_take_the_file_as_one_positional_argument() {
    // A name no script text would contain, with shell metacharacters: it must
    // arrive as its own argv element, never spliced into a script string.
    let file = ".probe dir/it's $(not run);`x`.json";
    for (id, form, is_wsl) in SHIPPED_ROWS {
        if !*is_wsl {
            continue;
        }
        let spec = shipped_spec(id);
        let Some(read) = spec.render_file_read(file) else {
            assert!(
                !reads_files(&spec, *form),
                "{id} reads settings files, so it must carry a read tool"
            );
            continue;
        };
        let mut rendered = vec![("read", read)];
        if let Some(replace) = spec.render_file_replace(file) {
            rendered.push(("replace", replace));
        }
        for (tool, argv) in rendered {
            assert_eq!(
                argv.first().map(String::as_str),
                Some("wsl"),
                "{id}'s {tool} tool must run inside WSL: {argv:?}"
            );
            let whole: Vec<usize> = argv
                .iter()
                .enumerate()
                .filter(|(_, arg)| arg.as_str() == file)
                .map(|(index, _)| index)
                .collect();
            assert_eq!(
                whole.len(),
                1,
                "{id}'s {tool} tool must pass the file as exactly one whole argument: {argv:?}"
            );
            let spliced = argv
                .iter()
                .filter(|arg| arg.as_str() != file && arg.contains(file))
                .count();
            assert_eq!(
                spliced, 0,
                "{id}'s {tool} tool must never splice the file into another argument: {argv:?}"
            );
        }
    }
}

// =============================================================================
// Rendering: each CLI's own registration command, for the default instance and
// a named one.
// =============================================================================

/// The canonical `add` for `form`: `mcp add [scope] [--env=pair ...] <server>
/// -- <bridge>`. Every flag precedes the name, as each CLI's usage line has it
/// (`agy` rejects a flag after the name), and carries its value in the same
/// argument, since `claude`'s variadic `-e <value>` would take the name too.
fn expected_add(form: CliForm, server: &str, env_pairs: &[&str], bridge: &str) -> Vec<String> {
    let mut argv = strings(&["mcp", "add"]);
    argv.extend(strings(form.scope));
    argv.extend(strings(form.add_options));
    for pair in env_pairs {
        argv.push(format!("{}={pair}", form.env_flag));
    }
    argv.push(server.to_string());
    argv.push("--".to_string());
    argv.push(bridge.to_string());
    argv
}

fn expected_remove(form: CliForm, server: &str) -> Vec<String> {
    let mut argv = strings(&["mcp", "remove"]);
    argv.extend(strings(form.scope));
    argv.push(server.to_string());
    argv
}

#[test]
fn the_default_instance_renders_each_clis_canonical_add_and_remove() {
    for (id, form, is_wsl) in SHIPPED_ROWS {
        let spec = shipped_spec(id);
        let bridge = bridge_for(*is_wsl);
        let add = spec
            .render_add(&default_target(), bridge)
            .unwrap_or_else(|err| panic!("{id} must render add for the default instance: {err}"));
        assert_eq!(
            add,
            expected_add(*form, "yagura", &[], bridge),
            "{id}: the default instance registers as `yagura` with no environment"
        );
        assert_eq!(
            spec.render_remove(&default_target()),
            expected_remove(*form, "yagura"),
            "{id}: remove must name the same scope add registered in"
        );
    }
}

#[test]
fn a_named_instance_carries_its_instance_variable_and_wsl_rows_also_forward_it() {
    let instance_pair = format!("YAGURA_INSTANCE={INSTANCE}");
    let server = format!("yagura-{INSTANCE}");
    for (id, form, is_wsl) in SHIPPED_ROWS {
        let spec = shipped_spec(id);
        let bridge = bridge_for(*is_wsl);
        let add = spec
            .render_add(&named_target(), bridge)
            .unwrap_or_else(|err| panic!("{id} must be able to carry the instance name: {err}"));
        let pairs: Vec<&str> = if *is_wsl {
            vec![instance_pair.as_str(), "WSLENV=YAGURA_INSTANCE"]
        } else {
            vec![instance_pair.as_str()]
        };
        assert_eq!(
            add,
            expected_add(*form, &server, &pairs, bridge),
            "{id}: a named instance must reach its own daemon, not the default one"
        );
        assert_eq!(
            spec.render_remove(&named_target()),
            expected_remove(*form, &server)
        );
    }
}

#[test]
fn each_row_reads_its_status_the_way_its_cli_reports_it() {
    for (id, form, _is_wsl) in SHIPPED_ROWS {
        let spec = shipped_spec(id);
        let expected = match form.status {
            StatusForm::JsonCommandWithTomlSet | StatusForm::JsonCommand => {
                StatusPlan::Run(strings(&["mcp", "get", "yagura", "--json"]))
            }
            StatusForm::Lines => StatusPlan::Run(strings(&["mcp", "get", "yagura"])),
            StatusForm::JsonFile => StatusPlan::ReadFile {
                file: AGY_SETTINGS_FILE.to_string(),
            },
            StatusForm::JsonListWithTomlSet => StatusPlan::Run(strings(&["mcp", "list", "--json"])),
        };
        assert_eq!(spec.render_status(&default_target()), expected, "{id}");
    }
}

#[test]
fn the_settings_file_edit_sets_the_tool_timeout_in_the_servers_own_table() {
    for (id, form, _is_wsl) in SHIPPED_ROWS {
        if form.status != StatusForm::JsonCommandWithTomlSet {
            continue;
        }
        let spec = shipped_spec(id);
        let toml_set = spec
            .toml_set()
            .unwrap_or_else(|| panic!("{id} must edit its settings file after add"));
        assert_eq!(toml_set.file(), CODEX_SETTINGS_FILE, "{id}");
        assert_eq!(
            toml_set.table(&named_target()),
            strings(&["mcp_servers", "yagura-dev"]),
            "{id}: the edit targets the table add just created for this server"
        );
        let mut expected = toml::Table::new();
        expected.insert("tool_timeout_sec".to_string(), toml::Value::Integer(86400));
        assert_eq!(
            toml_set.values(),
            &expected,
            "{id}: the CLI's own 60-second default would end a call before the daemon's \
             deadlines do"
        );
    }
}

// =============================================================================
// Reading back: the CLIs' own status output shapes derive the right state.
// =============================================================================

fn exited(code: i32, stdout: &str, stderr: &str) -> StatusObservation {
    StatusObservation::Exited {
        code: Some(code),
        stdout: stdout.as_bytes().to_vec(),
        stderr: stderr.as_bytes().to_vec(),
    }
}

/// What `mcp get <server> --json` prints for a registered server.
fn json_command_status(command: &str, tool_timeout: serde_json::Value) -> String {
    json!({
        "name": "yagura",
        "enabled": true,
        "disabled_reason": null,
        "transport": {
            "type": "stdio",
            "command": command,
            "args": [],
            "env": null,
            "env_vars": [],
            "cwd": null
        },
        "startup_timeout_sec": null,
        "tool_timeout_sec": tool_timeout
    })
    .to_string()
}

/// What `mcp get <server>` prints for a registered server, line by line. It
/// exits 0 even when the server cannot be reached.
fn lines_status(scope: &str, command: &str) -> String {
    format!(
        "yagura:\n  Scope: {scope}\n  Status: \u{2718} Failed to connect\n  Type: stdio\n  \
         Command: {command}\n  Args: \n  Environment:\n\nTo remove this server, run: mcp remove \
         \"yagura\" -s user\n"
    )
}

/// The JSON settings file listing registered servers.
fn json_settings(server: &str, command: &str) -> String {
    json!({ "mcpServers": { server: { "command": command, "args": [], "env": {} } } }).to_string()
}

struct Derived {
    state: RegistrationState,
    differs_in: Vec<String>,
}

fn derive(id: &str, observed: &StatusObservation, bridge: &str) -> Derived {
    let derived = derive_status(
        &shipped_spec(id),
        observed,
        &default_target(),
        bridge,
        Platform::Windows,
    );
    Derived {
        state: derived.state,
        differs_in: derived.differs_in,
    }
}

fn assert_state(id: &str, case: &str, derived: &Derived, expected: RegistrationState) {
    assert_eq!(
        derived.state, expected,
        "{id} ({case}): differs_in = {:?}",
        derived.differs_in
    );
}

#[test]
fn the_json_command_form_reads_not_registered_registered_and_differs() {
    for id in ["codex-cli", "wsl-codex-cli"] {
        let is_wsl = id.starts_with("wsl-");
        let bridge = bridge_for(is_wsl);
        let other = if is_wsl {
            "/mnt/c/Users/someone/elsewhere/yagura-mcp.exe"
        } else {
            OTHER_HOST_BRIDGE
        };

        let absent = exited(1, "", "Error: No MCP server named 'yagura' found.\n");
        assert_state(
            id,
            "not registered",
            &derive(id, &absent, bridge),
            RegistrationState::NotRegistered,
        );

        let registered = exited(0, &json_command_status(bridge, json!(86400.0)), "");
        assert_state(
            id,
            "registered, the timeout read back as a float",
            &derive(id, &registered, bridge),
            RegistrationState::Registered,
        );

        let other_bridge = exited(0, &json_command_status(other, json!(86400.0)), "");
        let derived = derive(id, &other_bridge, bridge);
        assert_state(id, "another bridge", &derived, RegistrationState::Differs);
        assert_eq!(derived.differs_in, strings(&["command"]), "{id}");

        let default_timeout = exited(0, &json_command_status(bridge, json!(60.0)), "");
        let derived = derive(id, &default_timeout, bridge);
        assert_state(
            id,
            "the CLI's default timeout",
            &derived,
            RegistrationState::Differs,
        );
        assert_eq!(derived.differs_in, strings(&["tool_timeout_sec"]), "{id}");
    }
}

#[test]
fn the_lines_form_reads_not_registered_registered_and_differs() {
    for id in ["claude-code", "wsl-claude-code"] {
        let is_wsl = id.starts_with("wsl-");
        let bridge = bridge_for(is_wsl);
        let other = if is_wsl {
            "/mnt/c/Users/someone/elsewhere/yagura-mcp.exe"
        } else {
            OTHER_HOST_BRIDGE
        };
        let user_scope = "User config (available in all your projects)";

        let absent = exited(
            1,
            "",
            "No MCP server named \"yagura\". Run `claude mcp add` to add one.\n",
        );
        assert_state(
            id,
            "not registered",
            &derive(id, &absent, bridge),
            RegistrationState::NotRegistered,
        );

        let registered = exited(0, &lines_status(user_scope, bridge), "");
        assert_state(
            id,
            "registered in the user scope, unreachable or not",
            &derive(id, &registered, bridge),
            RegistrationState::Registered,
        );

        let other_bridge = exited(0, &lines_status(user_scope, other), "");
        let derived = derive(id, &other_bridge, bridge);
        assert_state(id, "another bridge", &derived, RegistrationState::Differs);
        assert_eq!(derived.differs_in, strings(&["command"]), "{id}");

        let project_scope = exited(
            0,
            &lines_status("Project config (shared via .mcp.json)", bridge),
            "",
        );
        let derived = derive(id, &project_scope, bridge);
        assert_state(
            id,
            "registered in another scope",
            &derived,
            RegistrationState::Differs,
        );
        assert_eq!(derived.differs_in, strings(&["Scope"]), "{id}");
    }
}

#[test]
fn the_json_file_form_reads_not_registered_registered_and_differs() {
    for id in ["antigravity-cli", "wsl-antigravity-cli"] {
        let is_wsl = id.starts_with("wsl-");
        let bridge = bridge_for(is_wsl);
        let other = if is_wsl {
            "/mnt/c/Users/someone/elsewhere/yagura-mcp.exe"
        } else {
            OTHER_HOST_BRIDGE
        };

        // A host row reads the file itself; a WSL row reads it through its
        // read tool, which reports a missing file by its declared exit.
        let missing = if is_wsl {
            exited(3, "", "")
        } else {
            StatusObservation::FileMissing
        };
        assert_state(
            id,
            "no settings file",
            &derive(id, &missing, bridge),
            RegistrationState::NotRegistered,
        );

        let other_server = exited(0, &json_settings("some-other-server", bridge), "");
        assert_state(
            id,
            "a settings file without this server",
            &derive(id, &other_server, bridge),
            RegistrationState::NotRegistered,
        );

        let registered = exited(0, &json_settings("yagura", bridge), "");
        assert_state(
            id,
            "registered",
            &derive(id, &registered, bridge),
            RegistrationState::Registered,
        );

        let other_bridge = exited(0, &json_settings("yagura", other), "");
        let derived = derive(id, &other_bridge, bridge);
        assert_state(id, "another bridge", &derived, RegistrationState::Differs);
        assert_eq!(derived.differs_in, strings(&["command"]), "{id}");
    }
}

#[test]
fn a_wsl_read_tool_failure_other_than_the_missing_file_exit_is_not_read_as_unregistered() {
    // Control for the missing-file exit above: another failure of the same
    // tool must not be mistaken for "not registered".
    let derived = derive("wsl-antigravity-cli", &exited(1, "", ""), WSL_BRIDGE);
    assert_state(
        "wsl-antigravity-cli",
        "the read tool failed for another reason",
        &derived,
        RegistrationState::Unknown,
    );
}

// =============================================================================
// A placed profile that predates the table still gets its row, from the
// shipped table of the same id, launched the way the placed file says.
// =============================================================================

/// How long the listing may take before the case gives up.
const REPLY_TIMEOUT: Duration = Duration::from_secs(20);

/// A launcher no real executable is named, so no row here can ever run.
const PLACED_LAUNCHER: &str = "yagura-bundled-registration-test-nonexistent-launcher";

/// A placed profile with the given id's launch argv of its own and no
/// registration table. `[command]` is laid over the shipped one key by key,
/// so it names every key a shipped profile may launch with on the host.
fn placed_profile_without_table(display_name: &str) -> String {
    format!(
        "schema = 1\ndisplay_name = \"{display_name}\"\n\n[command]\ndefault = \
         [\"{PLACED_LAUNCHER}\"]\nwindows = [\"{PLACED_LAUNCHER}\"]\n"
    )
}

fn placed_argv(rendered: &[&str]) -> Vec<String> {
    let mut argv = strings(&[PLACED_LAUNCHER]);
    argv.extend(strings(rendered));
    argv
}

fn row_of<'a>(rows: &'a [McpRegistrationRow], profile: &str) -> &'a McpRegistrationRow {
    rows.iter()
        .find(|row| row.profile == profile)
        .unwrap_or_else(|| panic!("no row was listed for {profile}: {rows:?}"))
}

#[tokio::test]
async fn a_placed_profile_without_the_table_is_listed_with_the_shipped_table_and_its_own_launch() {
    let root = unique_temp_dir("bundled-mcpreg-placed");
    let profiles_dir = root.join("profiles");
    let home_dir = root.join("home");
    std::fs::create_dir_all(&profiles_dir).expect("create profiles dir");
    std::fs::create_dir_all(&home_dir).expect("create home dir");
    write_profile(
        &profiles_dir,
        "codex-cli",
        &placed_profile_without_table("Placed Host Row"),
    );
    write_profile(
        &profiles_dir,
        "wsl-antigravity-cli",
        &placed_profile_without_table("Placed Wsl Row"),
    );
    // Control: a placed profile no shipped profile shares an id with gets
    // no row, so the rows above can only have come from the shipped tables.
    write_profile(
        &profiles_dir,
        "not-a-shipped-id",
        &placed_profile_without_table("Unshipped Row"),
    );

    // No bridge: the listing reads no status, so no child is started.
    let config = DaemonConfig {
        profiles_dir: Some(profiles_dir),
        bundled_profiles: true,
        home_dir: Some(home_dir),
        instance_id: None,
        mcp_bridge: None,
        ..approver_base_config()
    };
    let h = Harness::start("bundled-mcpreg-placed", config).await;
    let mut client = h.connect().await;
    register_approver(&mut client).await;
    send(
        &mut client,
        &ClientMessage::ListMcpRegistrations { request_id: 7 },
    )
    .await;
    let rows = match read_response(&mut client, REPLY_TIMEOUT).await {
        DaemonMessage::McpRegistrationsListed {
            request_id,
            bridge,
            rows,
        } => {
            assert_eq!(request_id, 7);
            assert_eq!(bridge, None);
            rows
        }
        other => panic!("expected mcp_registrations_listed, got {other:?}"),
    };

    assert!(
        rows.iter().all(|row| row.profile != "not-a-shipped-id"),
        "a placed profile with no table and no shipped counterpart has nothing to register: \
         {rows:?}"
    );

    let host = row_of(&rows, "codex-cli");
    assert_eq!(host.display_name, "Placed Host Row");
    assert_eq!(host.group, None);
    assert_eq!(
        host.add,
        Some(McpRegistrationCommand {
            argv: placed_argv(&["mcp", "add", "yagura", "--", "{bridge}"]),
            program_at: 0,
        }),
        "the shipped table renders after the placed profile's own launch argv"
    );
    assert_eq!(
        host.remove,
        Some(McpRegistrationCommand {
            argv: placed_argv(&["mcp", "remove", "yagura"]),
            program_at: 0,
        })
    );
    assert_eq!(
        host.toml_set,
        Some(McpRegistrationTomlSet {
            file: CODEX_SETTINGS_FILE.to_string(),
            table: strings(&["mcp_servers", "yagura"]),
            values: vec![McpRegistrationTomlValue {
                key: "tool_timeout_sec".to_string(),
                value: "86400".to_string(),
            }],
        })
    );

    let wsl = row_of(&rows, "wsl-antigravity-cli");
    assert_eq!(wsl.display_name, "Placed Wsl Row");
    assert_eq!(wsl.group.as_deref(), Some("WSL"));
    assert_eq!(
        wsl.add.as_ref().map(|add| add.argv.clone()),
        Some(placed_argv(&["mcp", "add", "yagura", "--", "{bridge}"]))
    );
    assert_eq!(wsl.toml_set, None);

    h.registry.terminate_all().await;
}

// =============================================================================
// Muse Code: no registration command; registering writes the bridge and the
// hooks into a test home's settings files, the hooks acting only inside a
// window.
// =============================================================================

const MUSE_SETTINGS_FILE: &str = ".config/muse/settings.json";
const MUSE_HOOKS_FILE: &str = ".config/muse/yagura-managed-hooks.json";

/// Every string held under a `command` key anywhere in `value`.
fn hook_commands(value: &serde_json::Value, found: &mut Vec<String>) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, item) in map {
                match item {
                    serde_json::Value::String(command) if key == "command" => {
                        found.push(command.clone());
                    }
                    other => hook_commands(other, found),
                }
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                hook_commands(item, found);
            }
        }
        _ => {}
    }
}

fn read_json(path: &std::path::Path) -> serde_json::Value {
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
    serde_json::from_str(&text)
        .unwrap_or_else(|err| panic!("{} is JSON ({err}): {text}", path.display()))
}

#[test]
fn muse_code_reads_its_status_from_its_settings_file_directly() {
    let spec = shipped_spec("muse-code");
    assert_eq!(
        spec.render_status(&default_target()),
        StatusPlan::ReadFile {
            file: MUSE_SETTINGS_FILE.to_string()
        }
    );
    assert_eq!(
        spec.render_file_read(MUSE_SETTINGS_FILE),
        None,
        "muse-code runs on the host and reads the file itself"
    );
}

#[tokio::test]
async fn registering_muse_code_writes_the_bridge_and_window_only_hooks_into_the_home() {
    for (os, tag, prefix, suffix) in [
        (
            yagura_daemon::profile::ProfileOs::Windows,
            "bundled-mcpreg-muse-win",
            "if defined YAGURA_WINDOW_ID (set NoDefaultCurrentDirectoryInExePath=1& ",
            ")",
        ),
        (
            yagura_daemon::profile::ProfileOs::Linux,
            "bundled-mcpreg-muse-linux",
            "[ -z \"$YAGURA_WINDOW_ID\" ] || exec ",
            "",
        ),
    ] {
        let root = unique_temp_dir(tag);
        let profiles_dir = root.join("profiles");
        let home_dir = root.join("home");
        std::fs::create_dir_all(&profiles_dir).expect("create profiles dir");
        std::fs::create_dir_all(&home_dir).expect("create home dir");
        let bridge = root.join("bridge").join("yagura-mcp.exe");
        let bridge_str = bridge.to_str().expect("bridge path is UTF-8").to_string();
        let config = DaemonConfig {
            profiles_dir: Some(profiles_dir),
            bundled_profiles: true,
            home_dir: Some(home_dir.clone()),
            instance_id: None,
            mcp_bridge: Some(bridge),
            report_dir: Some(root.join("report")),
            host_os: os,
            ..approver_base_config()
        };
        let h = Harness::start(tag, config).await;
        let mut client = h.connect().await;
        register_approver(&mut client).await;

        send(
            &mut client,
            &ClientMessage::ListMcpRegistrations { request_id: 1 },
        )
        .await;
        let rows = match read_response(&mut client, REPLY_TIMEOUT).await {
            DaemonMessage::McpRegistrationsListed { rows, .. } => rows,
            other => panic!("expected mcp_registrations_listed, got {other:?}"),
        };
        // The listing is followed by one status push per row; drain them so
        // the next read is the registration's own answer.
        for _ in 0..rows.len() {
            match read_response(&mut client, REPLY_TIMEOUT).await {
                DaemonMessage::McpRegistrationStatus { request_id: 1, .. } => {}
                other => panic!("expected mcp_registration_status, got {other:?}"),
            }
        }
        let row = row_of(&rows, "muse-code");
        assert_eq!(row.group, None, "{os:?}: muse-code runs on the host");
        assert_eq!(row.add, None, "{os:?}: Muse has no registration command");
        assert_eq!(row.remove, None, "{os:?}");
        assert!(
            rows.iter().all(|row| row.profile != "wsl-muse-code"),
            "{os:?}: wsl-muse-code injects per launch and has nothing to register: {rows:?}"
        );

        send(
            &mut client,
            &ClientMessage::RunMcpRegistration {
                profile: "muse-code".to_string(),
                action: McpRegistrationAction::Register,
                request_id: 2,
            },
        )
        .await;
        match read_response(&mut client, REPLY_TIMEOUT).await {
            DaemonMessage::McpRegistrationRequested { request_id: 2, .. } => {}
            other => panic!("expected mcp_registration_requested, got {other:?}"),
        }
        let (steps, error, status) = match read_response(&mut client, REPLY_TIMEOUT).await {
            DaemonMessage::McpRegistrationResult {
                request_id: 2,
                steps,
                error,
                status,
                ..
            } => (steps, error, status),
            other => panic!("expected mcp_registration_result, got {other:?}"),
        };
        assert_eq!(error, None, "{os:?}: {steps:?}");
        assert!(
            steps.iter().all(|step| step.argv.is_empty()),
            "{os:?}: registering runs no command, only file edits: {steps:?}"
        );
        assert_eq!(
            status.state,
            McpRegistrationState::Registered,
            "{os:?}: {status:?}"
        );

        let settings = read_json(&home_dir.join(MUSE_SETTINGS_FILE));
        assert_eq!(
            settings
                .pointer("/mcpServers/yagura/command")
                .and_then(serde_json::Value::as_str),
            Some(bridge_str.as_str()),
            "{os:?}: {settings}"
        );
        assert_eq!(
            settings.pointer("/managed_hooks_path"),
            Some(&json!("yagura-managed-hooks.json")),
            "{os:?}: {settings}"
        );
        assert_eq!(
            settings.pointer("/managed_hooks_env_vars"),
            Some(&json!([
                "YAGURA_WINDOW_ID",
                "YAGURA_STATUS_TOKEN",
                "YAGURA_INSTANCE"
            ])),
            "{os:?}: the hooks receive the window's variables and nothing else: {settings}"
        );

        let hooks = read_json(&home_dir.join(MUSE_HOOKS_FILE));
        let mut commands = Vec::new();
        hook_commands(
            hooks
                .pointer("/hooks")
                .unwrap_or_else(|| panic!("{os:?}: no /hooks in {hooks}")),
            &mut commands,
        );
        assert!(!commands.is_empty(), "{os:?}: {hooks}");
        for command in &commands {
            assert!(
                command.starts_with(prefix) && command.ends_with(suffix),
                "{os:?}: outside a window the hook must do nothing, before any program is \
                 looked up: {command}"
            );
        }

        h.registry.terminate_all().await;
    }
}

// =============================================================================
// Grok Build: the shipped tables run end to end against the `mcp_cli_stub`
// fixture standing in for `grok`, so a wrong pointer or path in the profile
// shows as a registration that does not read back as registered.
// =============================================================================

const GROK_SETTINGS_FILE: &str = ".grok/config.toml";
const GROK_HOOKS_FILE: &str = ".grok/hooks/yagura.json";

/// The id the fixture profile is placed under, so no shipped table is laid
/// under it.
const GROK_PLACED_ID: &str = "grok";

fn string_array(items: &[&str]) -> toml::Value {
    toml::Value::Array(
        items
            .iter()
            .map(|item| toml::Value::String((*item).to_string()))
            .collect(),
    )
}

/// The shipped `profile` with only what would reach the real CLI replaced:
/// the launch command, the environment (the stub's own variables), and for
/// the WSL row the path conversion and file tools.
fn grok_fixture_profile(profile: &str, home: &Path, state: &Path) -> String {
    let stub = mcp_cli_stub_path();
    let stub = stub.to_str().expect("stub path is UTF-8");
    let home_str = home.to_str().expect("home is UTF-8");
    let mut root: toml::Table = shipped_source(profile)
        .parse()
        .expect("the shipped profile is TOML");
    let mut command = toml::Table::new();
    command.insert("default".into(), string_array(&[stub]));
    root.insert("command".into(), toml::Value::Table(command));

    if profile.starts_with("wsl-") {
        let absent_exit = root
            .get("host_paths")
            .and_then(|tools| tools.get("absent_exit"))
            .cloned()
            .expect("the WSL row declares its read tool's missing-file exit");
        let file_arg = format!("{home_str}/{{file}}");
        let mut host_paths = toml::Table::new();
        host_paths.insert("absent_exit".into(), absent_exit);
        host_paths.insert("convert".into(), string_array(&[stub, "probe", "{path}"]));
        host_paths.insert("read".into(), string_array(&[stub, "file-read", &file_arg]));
        host_paths.insert(
            "replace".into(),
            string_array(&[stub, "file-replace", &file_arg]),
        );
        root.insert("host_paths".into(), toml::Value::Table(host_paths));
    }

    let settings = home.join(GROK_SETTINGS_FILE);
    let mut env = toml::Table::new();
    for (key, value) in [
        ("MCP_CLI_STUB_STATE_DIR", state.to_str().expect("UTF-8")),
        (
            "MCP_CLI_STUB_TOML_TABLE_PATH",
            settings.to_str().expect("UTF-8"),
        ),
        ("MCP_CLI_STUB_LIST_SHAPE", "flat"),
    ] {
        env.insert(key.into(), toml::Value::String(value.to_string()));
    }
    root.insert("env".into(), toml::Value::Table(env));
    toml::to_string(&root).expect("serialize the fixture profile")
}

fn is_file(path: &Path) -> bool {
    path.is_file()
}

/// Runs a registration change and returns the state its result reads back.
async fn run_grok_action(
    client: &mut yagura_test_support::ipc::Connection,
    action: McpRegistrationAction,
    request_id: u64,
) -> McpRegistrationState {
    send(
        client,
        &ClientMessage::RunMcpRegistration {
            profile: GROK_PLACED_ID.to_string(),
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
        DaemonMessage::McpRegistrationResult {
            error,
            status,
            steps,
            ..
        } => {
            assert_eq!(error, None, "{steps:?}");
            status.state
        }
        other => panic!("expected mcp_registration_result, got {other:?}"),
    }
}

async fn grok_register_then_remove_case(profile: &str, tag: &str) {
    let root = unique_temp_dir(tag);
    let profiles_dir = root.join("profiles");
    let home = root.join("home");
    let state = root.join("stub-state");
    let report_dir = root.join("state").join("bin");
    for dir in [&profiles_dir, &home, &state] {
        std::fs::create_dir_all(dir).expect("create fixture dir");
    }
    std::fs::write(state.join("registry.json"), "{}").expect("seed registry");
    write_profile(
        &profiles_dir,
        GROK_PLACED_ID,
        &grok_fixture_profile(profile, &home, &state),
    );

    let config = DaemonConfig {
        profiles_dir: Some(profiles_dir),
        home_dir: Some(home.clone()),
        instance_id: None,
        mcp_bridge: Some(root.join("bridge").join("yagura-mcp.exe")),
        report_dir: Some(report_dir.clone()),
        mcp_registration_timeout: REPLY_TIMEOUT,
        host_os: yagura_daemon::profile::ProfileOs::host(),
        program_lookup: empty_lookup(),
        program_is_executable: is_file,
        default_shell: None,
        ..approver_base_config()
    };
    let h = Harness::start(tag, config).await;
    let mut client = h.connect().await;
    register_approver(&mut client).await;

    assert_eq!(
        run_grok_action(&mut client, McpRegistrationAction::Register, 1).await,
        McpRegistrationState::Registered,
        "{profile}: the shipped status, settings edit and hooks file must read back as registered"
    );

    let daemon = report_dir.join(REPORT_EXE_NAME);
    let daemon = daemon.to_str().expect("report path is UTF-8");
    let daemon = if profile.starts_with("wsl-") {
        // The stub's `probe` stands in for `wslpath -u`.
        format!("/probed{}", daemon.replace('\\', "/"))
    } else {
        daemon.to_string()
    };
    let hooks = read_json(&home.join(GROK_HOOKS_FILE));
    let mut commands = Vec::new();
    hook_commands(
        hooks
            .pointer("/hooks")
            .unwrap_or_else(|| panic!("{profile}: no /hooks in {hooks}")),
        &mut commands,
    );
    assert!(!commands.is_empty(), "{profile}: {hooks}");
    assert!(
        commands.iter().all(|command| *command == daemon),
        "{profile}: every handler runs this daemon by its absolute path: {commands:?}"
    );
    let settings: toml::Table = std::fs::read_to_string(home.join(GROK_SETTINGS_FILE))
        .expect("read the settings file")
        .parse()
        .expect("the settings file stays TOML");
    assert_eq!(
        settings
            .get("mcp_servers")
            .and_then(|servers| servers.get("yagura"))
            .and_then(|server| server.get("tool_timeout_sec")),
        Some(&toml::Value::Integer(86400)),
        "{profile}: {settings}"
    );

    assert_eq!(
        run_grok_action(&mut client, McpRegistrationAction::Remove, 2).await,
        McpRegistrationState::NotRegistered,
        "{profile}"
    );
    let hooks = read_json(&home.join(GROK_HOOKS_FILE));
    assert_eq!(
        hooks.pointer("/hooks"),
        None,
        "{profile}: removing takes the hooks away: {hooks}"
    );

    h.registry.terminate_all().await;
}

#[tokio::test]
async fn registering_grok_build_reads_back_registered_and_removing_reads_back_not_registered() {
    grok_register_then_remove_case("grok-build", "bundled-mcpreg-grok-host").await;
}

#[tokio::test]
async fn registering_wsl_grok_build_reads_back_registered_and_removing_reads_back_not_registered() {
    grok_register_then_remove_case("wsl-grok-build", "bundled-mcpreg-grok-wsl").await;
}

// =============================================================================
// The WSL file tools themselves: run on a Unix host with the leading `wsl`
// invocation dropped, against a temporary home.
// =============================================================================

// The WSL rows' shell pieces only ever run inside WSL, on Linux.
#[cfg(target_os = "linux")]
mod wsl_file_tools {
    use std::ffi::OsString;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::path::Path;
    use std::time::Duration;

    use yagura_daemon::child_env::ChildBaseEnv;
    use yagura_daemon::pty::{CaptureChildEnv, spawn_one_shot};
    use yagura_os::{CaptureLimits, CaptureOutcome};

    use super::{AGY_SETTINGS_FILE, CODEX_SETTINGS_FILE, host_file_tools, shipped_spec};
    use crate::common::unique_temp_dir;

    /// Far above a shell snippet's run time, so it only bounds a hang.
    const TOOL_TIMEOUT: Duration = Duration::from_secs(20);

    /// `argv` without its leading `wsl` invocation (`wsl`, then `-e`,
    /// `--exec` or `--` when present): what WSL itself runs inside the
    /// distribution.
    fn inside_wsl(argv: Vec<String>) -> Vec<String> {
        assert_eq!(argv.first().map(String::as_str), Some("wsl"), "{argv:?}");
        let mut rest: Vec<String> = argv.into_iter().skip(1).collect();
        if matches!(
            rest.first().map(String::as_str),
            Some("-e" | "--exec" | "--")
        ) {
            rest.remove(0);
        }
        rest
    }

    struct Ran {
        code: Option<i32>,
        stdout: Vec<u8>,
    }

    async fn run_with_home(argv: Vec<String>, home: &Path, stdin: Option<Vec<u8>>) -> Ran {
        let overlay: Vec<(OsString, OsString)> =
            vec![(OsString::from("HOME"), home.as_os_str().to_os_string())];
        let child = spawn_one_shot(
            inside_wsl(argv),
            None,
            CaptureChildEnv::Based {
                base: ChildBaseEnv::default(),
                overlay,
            },
            stdin,
        )
        .await
        .expect("the tool's shell must start");
        match child.collect(CaptureLimits {
            timeout: TOOL_TIMEOUT,
            max_output_bytes: 1 << 20,
        }) {
            CaptureOutcome::Exited { code, stdout, .. } => Ran { code, stdout },
            other => panic!("the tool must exit on its own, got {other:?}"),
        }
    }

    fn absent_exit(id: &str) -> i32 {
        let exit = host_file_tools(id)
            .and_then(|tools| tools.get("absent_exit").and_then(toml::Value::as_integer))
            .unwrap_or_else(|| panic!("{id} must declare host_paths.absent_exit"));
        i32::try_from(exit).expect("an exit code fits in i32")
    }

    /// The WSL rows that carry file tools; a row that reads every state from
    /// its CLI's own output has none.
    fn wsl_ids() -> Vec<&'static str> {
        super::SHIPPED_ROWS
            .iter()
            .filter(|(id, _, is_wsl)| {
                *is_wsl
                    && shipped_spec(id)
                        .render_file_read(AGY_SETTINGS_FILE)
                        .is_some()
            })
            .map(|(id, _, _)| *id)
            .collect()
    }

    fn entries(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .expect("read dir")
            .map(|entry| {
                entry
                    .expect("dir entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        names.sort();
        names
    }

    #[tokio::test]
    async fn the_read_tool_prints_a_home_relative_file_and_exits_with_the_declared_code_when_it_is_missing()
     {
        for id in wsl_ids() {
            let spec = shipped_spec(id);
            let home = unique_temp_dir("bundled-mcpreg-read");
            let argv = spec
                .render_file_read(AGY_SETTINGS_FILE)
                .expect("a WSL row carries a read tool");

            let missing = run_with_home(argv.clone(), &home, None).await;
            assert_eq!(
                missing.code,
                Some(absent_exit(id)),
                "{id}: a missing file must be told apart from every other failure"
            );

            let path = home.join(AGY_SETTINGS_FILE);
            std::fs::create_dir_all(path.parent().expect("parent")).expect("create dir");
            let content = b"{\"mcpServers\":{}}\n\xe2\x9c\x94 kept byte for byte\r\n";
            std::fs::write(&path, content).expect("write settings");
            let present = run_with_home(argv, &home, None).await;
            assert_eq!(present.code, Some(0), "{id}");
            assert_eq!(
                present.stdout, content,
                "{id}: the read tool must print the file resolved against $HOME, unchanged"
            );
        }
    }

    #[tokio::test]
    async fn the_replace_tool_keeps_the_mode_leaves_no_temporary_file_and_follows_a_symlink() {
        let id = "wsl-codex-cli";
        let spec = shipped_spec(id);
        let argv = spec
            .render_file_replace(CODEX_SETTINGS_FILE)
            .expect("the settings-editing WSL row carries a replace tool");

        // A regular file, readable by its owner alone.
        let home = unique_temp_dir("bundled-mcpreg-replace");
        let path = home.join(CODEX_SETTINGS_FILE);
        let dir = path.parent().expect("parent").to_path_buf();
        std::fs::create_dir_all(&dir).expect("create dir");
        std::fs::write(&path, "old = 1\n").expect("write settings");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("chmod");
        let ran = run_with_home(argv.clone(), &home, Some(b"new = 2\n".to_vec())).await;
        assert_eq!(ran.code, Some(0), "{id}");
        assert_eq!(std::fs::read(&path).expect("read back"), b"new = 2\n");
        assert_eq!(
            std::fs::metadata(&path).expect("stat").permissions().mode() & 0o777,
            0o600,
            "{id}: other servers' secrets may live in this file, so its mode must not widen"
        );
        assert_eq!(
            entries(&dir),
            vec!["config.toml".to_string()],
            "{id}: the temporary file must be renamed over the original, not left behind"
        );

        // A symlink into a dotfiles directory.
        let home = unique_temp_dir("bundled-mcpreg-replace-link");
        let real_dir = home.join("dotfiles");
        std::fs::create_dir_all(&real_dir).expect("create dotfiles");
        let real = real_dir.join("codex-config.toml");
        std::fs::write(&real, "old = 1\n").expect("write real");
        // Neither a fresh temporary file's mode nor the default umask's, so
        // only copying the original's mode keeps it.
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o640)).expect("chmod");
        let link = home.join(CODEX_SETTINGS_FILE);
        std::fs::create_dir_all(link.parent().expect("parent")).expect("create dir");
        symlink(&real, &link).expect("symlink");
        let ran = run_with_home(argv.clone(), &home, Some(b"new = 2\n".to_vec())).await;
        assert_eq!(ran.code, Some(0), "{id}");
        assert!(
            std::fs::symlink_metadata(&link)
                .expect("lstat")
                .file_type()
                .is_symlink(),
            "{id}: a dotfiles-managed link must stay a link"
        );
        assert_eq!(std::fs::read_link(&link).expect("readlink"), real);
        assert_eq!(std::fs::read(&real).expect("read real"), b"new = 2\n");
        assert_eq!(
            std::fs::metadata(&real).expect("stat").permissions().mode() & 0o777,
            0o640,
            "{id}: the link's target keeps its own mode"
        );
        assert_eq!(entries(&real_dir), vec!["codex-config.toml".to_string()]);

        // No file: nothing is created.
        let home = unique_temp_dir("bundled-mcpreg-replace-none");
        // The directory exists, so only the tool itself can refuse.
        std::fs::create_dir_all(home.join(CODEX_SETTINGS_FILE).parent().expect("parent"))
            .expect("create dir");
        let ran = run_with_home(argv, &home, Some(b"new = 2\n".to_vec())).await;
        assert_ne!(
            ran.code,
            Some(0),
            "{id}: a missing settings file means the CLI reads another one, so it must not be \
             created"
        );
        assert!(
            !home.join(CODEX_SETTINGS_FILE).exists(),
            "{id}: the replace tool must not create the settings file"
        );
    }
}
