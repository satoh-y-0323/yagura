#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: the I/O-free rules behind registering the daemon's MCP
//! bridge with a CLI -- the name and environment an instance registers
//! under, how a profile's registration table renders into argv, how a status
//! reading becomes one of five states, and how probe output and step output
//! are checked before they are used or shown.
//!
//! Profiles here use a neutral CLI name; output shapes follow what real CLIs
//! print, without naming them.

use serde_json::json;
use yagura_daemon::mcp_registration::{
    DerivedStatus, McpRegistrationSpec, RegistrationState, RegistrationTarget, RenderError,
    STEP_OUTPUT_MAX_BYTES, StatusObservation, derive_status, json_value_matches, sanitize_output,
    validate_probe_output,
};
use yagura_daemon::profile::{ProfileParseError, parse_profile};
use yagura_os::program::Platform;

const HEADER: &str =
    "schema = 1\ndisplay_name = \"Neutral Cli\"\n\n[command]\ndefault = [\"neutral-cli\"]\n\n";

const JSON_TABLE: &str = r#"[mcp_registration]
add = ["mcp", "add", "{server}", "{env_args}", "--", "{bridge}"]
remove = ["mcp", "remove", "{server}"]
env_arg = ["--env", "{env}"]

[mcp_registration.status]
args = ["mcp", "get", "{server}", "--json"]
format = "json"
command = "/transport/command"
args_pointer = "/transport/args"
absent = "No MCP server named"

[mcp_registration.toml_set]
file = ".neutral/config.toml"
table = ["mcp_servers", "{server}"]
values = { tool_timeout_sec = 86400 }
confirm = { tool_timeout_sec = "/tool_timeout_sec" }
"#;

const LINES_TABLE: &str = r#"[mcp_registration]
add = ["mcp", "add", "--scope", "user", "{server}", "{env_args}", "--", "{bridge}"]
remove = ["mcp", "remove", "--scope", "user", "{server}"]
env_arg = ["-e", "{env}"]

[mcp_registration.status]
args = ["mcp", "get", "{server}"]
format = "lines"
command = "Command:"
require = { "Scope:" = "User" }
absent = "No MCP server named"
"#;

const FILE_TABLE: &str = r#"[mcp_registration]
add = ["mcp", "add", "{server}", "{env_args}", "{bridge}"]
remove = ["mcp", "remove", "{server}"]
env_arg = ["--env", "{env}"]

[mcp_registration.status]
file = ".neutral/mcp_config.json"
format = "json"
command = "/mcpServers/{server}/command"
"#;

const FOREIGN_TABLE: &str = r#"[host_paths]
convert = ["wsl", "-e", "wslpath", "-u", "{path}"]

[mcp_registration]
group = "WSL"
add = ["mcp", "add", "{server}", "{env_args}", "--", "{bridge}"]
remove = ["mcp", "remove", "{server}"]
env_arg = ["--env", "{env}"]
env_forward = { name = "WSLENV", separator = ":" }
files = { read = ["wsl", "-e", "sh", "-c", "[ -e \"$HOME/$1\" ] || exit 3; cat \"$HOME/$1\"", "sh", "{file}"], replace = ["wsl", "-e", "sh", "-c", "cat > \"$HOME/$1\"", "sh", "{file}"], absent_exit = 3 }

[mcp_registration.status]
args = ["mcp", "get", "{server}", "--json"]
format = "json"
command = "/transport/command"
absent = "No MCP server named"
"#;

const FOREIGN_FILE_TABLE: &str = r#"[host_paths]
convert = ["wsl", "-e", "wslpath", "-u", "{path}"]

[mcp_registration]
group = "WSL"
add = ["mcp", "add", "{server}", "{env_args}", "{bridge}"]
remove = ["mcp", "remove", "{server}"]
env_arg = ["--env", "{env}"]
env_forward = { name = "WSLENV", separator = ":" }
files = { read = ["wsl", "-e", "sh", "-c", "[ -e \"$HOME/$1\" ] || exit 3; cat \"$HOME/$1\"", "sh", "{file}"], absent_exit = 3 }

[mcp_registration.status]
file = ".neutral/mcp_config.json"
format = "json"
command = "/mcpServers/{server}/command"
"#;

/// A table whose CLI has no flag to set an environment variable.
const NO_ENV_ARG_TABLE: &str = r#"[mcp_registration]
add = ["mcp", "add", "{server}", "{env_args}", "{bridge}"]
remove = ["mcp", "remove", "{server}"]

[mcp_registration.status]
args = ["mcp", "get", "{server}"]
format = "json"
command = "/command"
absent = "not found"
"#;

const BRIDGE: &str = r"C:\Tools\yagura\yagura-mcp.exe";
const PROBED_BRIDGE: &str = "/mnt/c/Tools/yagura/yagura-mcp.exe";
const ABSENT_ON_STDERR: &str = "Error: No MCP server named 'yagura' found.\n";

fn spec_of(table: &str) -> McpRegistrationSpec {
    let mut source = HEADER.to_string();
    source.push_str(table);
    parse_profile("neutral-cli", &source)
        .unwrap_or_else(|err| panic!("the fixture profile must parse: {err:?}"))
        .mcp_registration
        .expect("the fixture profile carries a registration table")
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| (*item).to_string()).collect()
}

fn default_target() -> RegistrationTarget {
    RegistrationTarget::for_instance(None)
}

fn dev_target() -> RegistrationTarget {
    RegistrationTarget::for_instance(Some("dev"))
}

fn exited(code: i32, stdout: &str, stderr: &str) -> StatusObservation {
    StatusObservation::Exited {
        code: Some(code),
        stdout: stdout.as_bytes().to_vec(),
        stderr: stderr.as_bytes().to_vec(),
    }
}

/// A JSON status document as the host CLI prints it for a registered server.
fn json_status(command: &str, args: serde_json::Value, timeout: serde_json::Value) -> String {
    json!({
        "name": "yagura",
        "enabled": true,
        "transport": { "type": "stdio", "command": command, "args": args, "env": {} },
        "startup_timeout_sec": null,
        "tool_timeout_sec": timeout,
    })
    .to_string()
}

/// A line-shaped status as the host CLI prints it for a registered server.
fn lines_status(scope: &str, command: &str, newline: &str) -> String {
    [
        "yagura:".to_string(),
        format!("  Scope: {scope}"),
        "  Status: \u{2714} Connected".to_string(),
        "  Type: stdio".to_string(),
        format!("  Command: {command}"),
        "  Args: ".to_string(),
        "  Environment:".to_string(),
        String::new(),
    ]
    .join(newline)
}

fn derive(
    table: &str,
    observed: &StatusObservation,
    expected_bridge: &str,
    platform: Platform,
) -> DerivedStatus {
    let spec = spec_of(table);
    derive_status(
        &spec,
        observed,
        &default_target(),
        expected_bridge,
        platform,
    )
}

fn assert_state(derived: &DerivedStatus, state: RegistrationState, context: &str) {
    assert_eq!(derived.state, state, "{context}: got {derived:?}");
}

fn assert_unknown_with_a_fixed_reason(derived: &DerivedStatus, leaked: &[&str], context: &str) {
    assert_state(derived, RegistrationState::Unknown, context);
    let reason = derived
        .reason
        .as_deref()
        .unwrap_or_else(|| panic!("{context}: an unknown state must say why"));
    for text in leaked {
        assert!(
            !reason.contains(text),
            "{context}: the reason must not quote paths or output, got {reason:?}"
        );
    }
}

// ---------------------------------------------------------------------
// The name and environment an instance registers under
// ---------------------------------------------------------------------

#[test]
fn the_default_instance_registers_as_yagura_with_no_environment() {
    let target = default_target();
    assert_eq!(target.server(), "yagura");
    assert!(target.env().is_empty());
}

#[test]
fn a_named_instance_registers_under_its_own_name_and_carries_it_in_the_environment() {
    // A shared name would overwrite the user's own registration and reach the
    // default daemon.
    let target = dev_target();
    assert_eq!(target.server(), "yagura-dev");
    assert_eq!(
        target.env(),
        &[("YAGURA_INSTANCE".to_string(), "dev".to_string())][..]
    );
}

// ---------------------------------------------------------------------
// Rendering argv
// ---------------------------------------------------------------------

#[test]
fn env_args_vanishes_as_a_whole_element_when_there_is_no_environment() {
    let spec = spec_of(JSON_TABLE);
    let argv: Vec<String> = spec
        .render_add(&default_target(), BRIDGE)
        .expect("default instance");
    assert_eq!(argv, strings(&["mcp", "add", "yagura", "--", BRIDGE]));
}

#[test]
fn env_args_expands_env_arg_once_per_variable() {
    let spec = spec_of(JSON_TABLE);
    let argv: Vec<String> = spec.render_add(&dev_target(), BRIDGE).expect("dev");
    assert_eq!(
        argv,
        strings(&[
            "mcp",
            "add",
            "yagura-dev",
            "--env",
            "YAGURA_INSTANCE=dev",
            "--",
            BRIDGE
        ])
    );

    let lines = spec_of(LINES_TABLE);
    let argv: Vec<String> = lines.render_add(&dev_target(), BRIDGE).expect("dev");
    assert_eq!(
        argv,
        strings(&[
            "mcp",
            "add",
            "--scope",
            "user",
            "yagura-dev",
            "-e",
            "YAGURA_INSTANCE=dev",
            "--",
            BRIDGE
        ])
    );
}

#[test]
fn env_forward_adds_the_variable_names_only_for_a_named_instance() {
    // Without the names listed, the other environment does not hand the
    // variable to the bridge it starts.
    let spec = spec_of(FOREIGN_TABLE);
    let named: Vec<String> = spec.render_add(&dev_target(), PROBED_BRIDGE).expect("dev");
    assert_eq!(
        named,
        strings(&[
            "mcp",
            "add",
            "yagura-dev",
            "--env",
            "YAGURA_INSTANCE=dev",
            "--env",
            "WSLENV=YAGURA_INSTANCE",
            "--",
            PROBED_BRIDGE
        ])
    );
    let default: Vec<String> = spec
        .render_add(&default_target(), PROBED_BRIDGE)
        .expect("default instance");
    assert_eq!(
        default,
        strings(&["mcp", "add", "yagura", "--", PROBED_BRIDGE])
    );
}

#[test]
fn a_named_instance_cannot_be_carried_by_a_table_without_env_arg() {
    let spec = spec_of(NO_ENV_ARG_TABLE);
    let result = spec.render_add(&dev_target(), BRIDGE);
    assert!(
        matches!(result, Err(RenderError::InstanceNotCarried)),
        "registering a named instance without its variable would reach the default daemon, \
         got {result:?}"
    );
    let default: Vec<String> = spec
        .render_add(&default_target(), BRIDGE)
        .expect("the default instance needs no variable");
    assert_eq!(default, strings(&["mcp", "add", "yagura", BRIDGE]));
}

#[test]
fn remove_and_the_probe_render_their_placeholders() {
    let spec = spec_of(FOREIGN_TABLE);
    assert_eq!(
        spec.render_remove(&dev_target()),
        strings(&["mcp", "remove", "yagura-dev"])
    );
    assert_eq!(
        spec.render_probe(BRIDGE),
        Some(strings(&["wsl", "-e", "wslpath", "-u", BRIDGE]))
    );
    let replace: Vec<String> = spec
        .render_file_replace(".neutral/config.toml")
        .expect("the table has a replace tool");
    assert_eq!(
        replace,
        strings(&[
            "wsl",
            "-e",
            "sh",
            "-c",
            "cat > \"$HOME/$1\"",
            "sh",
            ".neutral/config.toml"
        ])
    );
}

// ---------------------------------------------------------------------
// Deriving the state
// ---------------------------------------------------------------------

#[test]
fn a_matching_json_status_is_registered() {
    let derived = derive(
        JSON_TABLE,
        &exited(0, &json_status(BRIDGE, json!([]), json!(86400.0)), ""),
        BRIDGE,
        Platform::Windows,
    );
    assert_state(&derived, RegistrationState::Registered, "json match");
    assert!(derived.differs_in.is_empty());
}

#[test]
fn a_different_bridge_or_leftover_args_differs_in_command() {
    let other = derive(
        JSON_TABLE,
        &exited(
            0,
            &json_status(r"D:\old\yagura-mcp.exe", json!([]), json!(86400.0)),
            "",
        ),
        BRIDGE,
        Platform::Windows,
    );
    assert_state(&other, RegistrationState::Differs, "other bridge");
    assert_eq!(other.differs_in, strings(&["command"]));

    let args = derive(
        JSON_TABLE,
        &exited(0, &json_status(BRIDGE, json!(["--x"]), json!(86400.0)), ""),
        BRIDGE,
        Platform::Windows,
    );
    assert_state(&args, RegistrationState::Differs, "leftover args");
    assert_eq!(args.differs_in, strings(&["command"]));
}

#[test]
fn a_confirm_value_that_is_off_or_missing_differs_in_that_key() {
    // Without this the row would read registered and offer no way to redo
    // the settings edit.
    for timeout in [json!(60.0), json!(null)] {
        let derived = derive(
            JSON_TABLE,
            &exited(0, &json_status(BRIDGE, json!([]), timeout), ""),
            BRIDGE,
            Platform::Windows,
        );
        assert_state(&derived, RegistrationState::Differs, "confirm");
        assert_eq!(derived.differs_in, strings(&["tool_timeout_sec"]));
    }
}

#[test]
fn every_differing_part_is_named() {
    let derived = derive(
        JSON_TABLE,
        &exited(
            0,
            &json_status(r"D:\old\yagura-mcp.exe", json!([]), json!(60)),
            "",
        ),
        BRIDGE,
        Platform::Windows,
    );
    assert_state(&derived, RegistrationState::Differs, "both");
    assert!(derived.differs_in.contains(&"command".to_string()));
    assert!(derived.differs_in.contains(&"tool_timeout_sec".to_string()));
}

#[test]
fn an_integer_confirm_value_matches_a_float_reported_by_the_cli() {
    let derived = derive(
        JSON_TABLE,
        &exited(0, &json_status(BRIDGE, json!([]), json!(86400.0)), ""),
        BRIDGE,
        Platform::Windows,
    );
    assert_state(&derived, RegistrationState::Registered, "86400 vs 86400.0");
}

#[test]
fn a_matching_lines_status_is_registered() {
    for newline in ["\n", "\r\n"] {
        let derived = derive(
            LINES_TABLE,
            &exited(
                0,
                &lines_status(
                    "User config (available in all your projects)",
                    BRIDGE,
                    newline,
                ),
                "",
            ),
            BRIDGE,
            Platform::Windows,
        );
        assert_state(&derived, RegistrationState::Registered, "lines match");
    }
}

#[test]
fn a_required_line_that_does_not_start_with_its_word_differs_in_that_key() {
    let derived = derive(
        LINES_TABLE,
        &exited(
            0,
            &lines_status("Project config (shared via .mcp.json)", BRIDGE, "\n"),
            "",
        ),
        BRIDGE,
        Platform::Windows,
    );
    assert_state(&derived, RegistrationState::Differs, "scope");
    assert_eq!(derived.differs_in, strings(&["Scope"]));
}

#[test]
fn absent_on_standard_error_alone_is_not_registered() {
    // Some CLIs report "no such server" only on standard error.
    let json = derive(
        JSON_TABLE,
        &exited(1, "", ABSENT_ON_STDERR),
        BRIDGE,
        Platform::Windows,
    );
    assert_state(&json, RegistrationState::NotRegistered, "json stderr");

    let lines = derive(
        LINES_TABLE,
        &exited(
            1,
            "",
            "No MCP server named \"yagura\". Run `mcp add` to add one.\n",
        ),
        BRIDGE,
        Platform::Windows,
    );
    assert_state(&lines, RegistrationState::NotRegistered, "lines stderr");
}

#[test]
fn absent_on_standard_output_is_not_registered() {
    let derived = derive(
        JSON_TABLE,
        &exited(1, ABSENT_ON_STDERR, ""),
        BRIDGE,
        Platform::Windows,
    );
    assert_state(&derived, RegistrationState::NotRegistered, "stdout");
}

#[test]
fn a_missing_program_is_not_installed() {
    let derived = derive(
        JSON_TABLE,
        &StatusObservation::NotInstalled,
        BRIDGE,
        Platform::Windows,
    );
    assert_state(&derived, RegistrationState::NotInstalled, "not installed");

    let shell = derive(
        JSON_TABLE,
        &exited(127, "", "sh: neutral-cli: not found\n"),
        BRIDGE,
        Platform::Windows,
    );
    assert_state(&shell, RegistrationState::NotInstalled, "exit 127");
}

#[test]
fn anything_that_cannot_be_read_falls_to_unknown_without_quoting_it() {
    let secret = "SECRET-OUTPUT-TEXT";
    let cases: Vec<(&str, StatusObservation)> = vec![
        (
            "unparsable json",
            exited(0, &format!("{secret} not json"), ""),
        ),
        (
            "missing command pointer",
            exited(0, &json!({ "name": secret }).to_string(), ""),
        ),
        (
            "command not a string",
            exited(
                0,
                &json!({ "transport": { "command": 7 }, "note": secret }).to_string(),
                "",
            ),
        ),
        (
            "other exit",
            exited(1, secret, &format!("{secret} failed at {BRIDGE}")),
        ),
        (
            "no exit code",
            StatusObservation::Exited {
                code: None,
                stdout: secret.as_bytes().to_vec(),
                stderr: Vec::new(),
            },
        ),
        ("timed out", StatusObservation::TimedOut),
        ("too large", StatusObservation::OutputTooLarge),
    ];
    for (context, observed) in cases {
        let derived = derive(JSON_TABLE, &observed, BRIDGE, Platform::Windows);
        assert_unknown_with_a_fixed_reason(&derived, &[secret, BRIDGE], context);
        assert!(derived.differs_in.is_empty(), "{context}");
    }

    let lines = derive(
        LINES_TABLE,
        &exited(
            0,
            &format!("yagura:\n  Scope: User\n  Note: {secret}\n"),
            "",
        ),
        BRIDGE,
        Platform::Windows,
    );
    assert_unknown_with_a_fixed_reason(&lines, &[secret], "lines without a command line");
}

#[test]
fn a_status_file_that_is_missing_or_lacks_the_server_is_not_registered() {
    let missing = derive(
        FILE_TABLE,
        &StatusObservation::FileMissing,
        BRIDGE,
        Platform::Windows,
    );
    assert_state(&missing, RegistrationState::NotRegistered, "file missing");

    let other_server = derive(
        FILE_TABLE,
        &exited(
            0,
            &json!({ "mcpServers": { "someone-else": { "command": BRIDGE } } }).to_string(),
            "",
        ),
        BRIDGE,
        Platform::Windows,
    );
    assert_state(
        &other_server,
        RegistrationState::NotRegistered,
        "pointer value absent",
    );
}

#[test]
fn a_lines_status_file_without_the_command_line_is_not_registered() {
    let lines_file_table = FILE_TABLE.replace(
        "format = \"json\"\ncommand = \"/mcpServers/{server}/command\"",
        "format = \"lines\"\ncommand = \"Command:\"",
    );
    let registered = derive(
        &lines_file_table,
        &exited(0, &lines_status("User", BRIDGE, "\n"), ""),
        BRIDGE,
        Platform::Windows,
    );
    assert_state(
        &registered,
        RegistrationState::Registered,
        "command line present",
    );

    let without_command = derive(
        &lines_file_table,
        &exited(0, "other-server:\n  Type: stdio\n", ""),
        BRIDGE,
        Platform::Windows,
    );
    assert_state(
        &without_command,
        RegistrationState::NotRegistered,
        "a settings file without the line has no registration",
    );
}

#[test]
fn a_status_file_that_names_the_bridge_is_registered() {
    let derived = derive(
        FILE_TABLE,
        &exited(
            0,
            &json!({ "mcpServers": { "yagura": { "command": BRIDGE, "env": {} } } }).to_string(),
            "",
        ),
        BRIDGE,
        Platform::Windows,
    );
    assert_state(&derived, RegistrationState::Registered, "file match");
}

#[test]
fn a_status_file_that_cannot_be_read_as_the_format_is_unknown() {
    let unparsable = derive(
        FILE_TABLE,
        &exited(0, "{ SECRET-OUTPUT-TEXT", ""),
        BRIDGE,
        Platform::Windows,
    );
    assert_unknown_with_a_fixed_reason(
        &unparsable,
        &["SECRET-OUTPUT-TEXT", ".neutral"],
        "file unparsable",
    );

    let not_string = derive(
        FILE_TABLE,
        &exited(
            0,
            &json!({ "mcpServers": { "yagura": { "command": ["x"] } } }).to_string(),
            "",
        ),
        BRIDGE,
        Platform::Windows,
    );
    assert_unknown_with_a_fixed_reason(&not_string, &[".neutral"], "file command not a string");
}

/// Contents a CLI leaves in a settings file that records no registration at
/// all: nothing, or whitespace only.
const BLANK_FILE_CONTENTS: [&str; 5] = ["", " ", "\n", "\r\n", " \t\r\n \n"];

#[test]
fn an_empty_or_whitespace_only_status_file_is_not_registered() {
    for contents in BLANK_FILE_CONTENTS {
        let host = derive(
            FILE_TABLE,
            &exited(0, contents, ""),
            BRIDGE,
            Platform::Windows,
        );
        assert_state(
            &host,
            RegistrationState::NotRegistered,
            &format!("host file {contents:?}"),
        );

        let foreign = derive(
            FOREIGN_FILE_TABLE,
            &exited(0, contents, ""),
            PROBED_BRIDGE,
            Platform::Windows,
        );
        assert_state(
            &foreign,
            RegistrationState::NotRegistered,
            &format!("read tool file {contents:?}"),
        );
    }
}

#[test]
fn a_status_file_with_non_blank_unreadable_contents_stays_unknown() {
    for contents in ["x", " x \n", "\n{\n", "SECRET-OUTPUT-TEXT"] {
        for (table, bridge, context) in [
            (FILE_TABLE, BRIDGE, "host file"),
            (FOREIGN_FILE_TABLE, PROBED_BRIDGE, "read tool file"),
        ] {
            let derived = derive(table, &exited(0, contents, ""), bridge, Platform::Windows);
            assert_unknown_with_a_fixed_reason(
                &derived,
                &["SECRET-OUTPUT-TEXT", ".neutral"],
                &format!("{context} {contents:?}"),
            );
        }
    }
}

#[test]
fn an_empty_command_output_stays_unknown_since_only_a_settings_file_reads_blank_as_absent() {
    for contents in BLANK_FILE_CONTENTS {
        let derived = derive(
            JSON_TABLE,
            &exited(0, contents, ""),
            BRIDGE,
            Platform::Windows,
        );
        assert_state(
            &derived,
            RegistrationState::Unknown,
            &format!("command output {contents:?}"),
        );
    }
}

#[test]
fn the_read_tool_s_absent_exit_is_not_registered_and_other_failures_keep_their_rules() {
    let absent = derive(
        FOREIGN_FILE_TABLE,
        &exited(3, "", ""),
        PROBED_BRIDGE,
        Platform::Windows,
    );
    assert_state(&absent, RegistrationState::NotRegistered, "absent exit");

    let failed = derive(
        FOREIGN_FILE_TABLE,
        &exited(1, "", "cat: permission denied\n"),
        PROBED_BRIDGE,
        Platform::Windows,
    );
    assert_state(&failed, RegistrationState::Unknown, "other exit");

    let missing = derive(
        FOREIGN_FILE_TABLE,
        &exited(127, "", ""),
        PROBED_BRIDGE,
        Platform::Windows,
    );
    assert_state(&missing, RegistrationState::NotInstalled, "exit 127");

    let registered = derive(
        FOREIGN_FILE_TABLE,
        &exited(
            0,
            &json!({ "mcpServers": { "yagura": { "command": PROBED_BRIDGE } } }).to_string(),
            "",
        ),
        PROBED_BRIDGE,
        Platform::Windows,
    );
    assert_state(&registered, RegistrationState::Registered, "read match");
}

#[test]
fn a_windows_host_path_compares_across_separators_and_case() {
    let observed = exited(
        0,
        &json_status("c:/tools/YAGURA/Yagura-MCP.exe", json!([]), json!(86400)),
        "",
    );
    let windows = derive(JSON_TABLE, &observed, BRIDGE, Platform::Windows);
    assert_state(&windows, RegistrationState::Registered, "windows host");

    let unix = derive(JSON_TABLE, &observed, BRIDGE, Platform::Unix);
    assert_state(&unix, RegistrationState::Differs, "unix host");
    assert_eq!(unix.differs_in, strings(&["command"]));
}

#[test]
fn a_probed_path_compares_exactly_even_on_a_windows_host() {
    let observed = exited(
        0,
        &json_status("/mnt/c/tools/yagura/yagura-mcp.exe", json!([]), json!(null)),
        "",
    );
    let derived = derive(FOREIGN_TABLE, &observed, PROBED_BRIDGE, Platform::Windows);
    assert_state(&derived, RegistrationState::Differs, "probed case differs");
    assert_eq!(derived.differs_in, strings(&["command"]));

    let exact = exited(0, &json_status(PROBED_BRIDGE, json!([]), json!(null)), "");
    let derived = derive(FOREIGN_TABLE, &exact, PROBED_BRIDGE, Platform::Windows);
    assert_state(&derived, RegistrationState::Registered, "probed exact");
}

// ---------------------------------------------------------------------
// Comparing values
// ---------------------------------------------------------------------

#[test]
fn numbers_compare_as_numbers_across_integer_and_float() {
    assert!(json_value_matches(
        &toml::Value::Integer(86400),
        &json!(86400.0)
    ));
    assert!(json_value_matches(
        &toml::Value::Integer(86400),
        &json!(86400)
    ));
    assert!(json_value_matches(&toml::Value::Float(1.5), &json!(1.5)));
    assert!(!json_value_matches(
        &toml::Value::Integer(86400),
        &json!(60.0)
    ));
    assert!(!json_value_matches(
        &toml::Value::Integer(86400),
        &json!(null)
    ));
}

#[test]
fn strings_and_booleans_compare_within_their_own_type() {
    assert!(json_value_matches(
        &toml::Value::String("a".to_string()),
        &json!("a")
    ));
    assert!(!json_value_matches(
        &toml::Value::String("1".to_string()),
        &json!(1)
    ));
    assert!(!json_value_matches(&toml::Value::Integer(1), &json!("1")));
    assert!(json_value_matches(
        &toml::Value::Boolean(true),
        &json!(true)
    ));
    assert!(!json_value_matches(
        &toml::Value::Boolean(true),
        &json!("true")
    ));
}

// ---------------------------------------------------------------------
// Checking probe output
// ---------------------------------------------------------------------

#[test]
fn a_single_absolute_line_is_accepted_without_its_newline() {
    assert_eq!(
        validate_probe_output(b"/mnt/c/Tools/yagura/yagura-mcp.exe\n"),
        Some(PROBED_BRIDGE.to_string())
    );
    assert_eq!(
        validate_probe_output(PROBED_BRIDGE.as_bytes()),
        Some(PROBED_BRIDGE.to_string())
    );
}

#[test]
fn probe_output_that_could_not_serve_as_a_path_is_refused() {
    // The value goes straight into the add argv.
    let too_long = format!("/{}", "a".repeat(4096));
    let cases: [(&str, &[u8]); 7] = [
        ("empty", b""),
        ("relative", b"mnt/c/yagura-mcp.exe\n"),
        ("two lines", b"/mnt/c/a\n/mnt/c/b\n"),
        ("nul", b"/mnt/c/a\0b\n"),
        ("control", b"/mnt/c/a\x1b[31mb\n"),
        ("carriage return inside", b"/mnt/c/a\rb\n"),
        ("too long", too_long.as_bytes()),
    ];
    for (context, stdout) in cases {
        assert_eq!(validate_probe_output(stdout), None, "{context}");
    }
    let at_limit = format!("/{}", "a".repeat(4095));
    assert_eq!(
        validate_probe_output(at_limit.as_bytes()),
        Some(at_limit),
        "4 KiB itself is accepted"
    );
}

// ---------------------------------------------------------------------
// Cleaning output for display
// ---------------------------------------------------------------------

#[test]
fn escapes_and_control_characters_are_dropped_but_newlines_and_tabs_kept() {
    let raw = "\u{1b}[31mred\u{1b}[0m plain\tcol\nnext\u{7}bell\u{91}c1\u{202e}rtl\u{2066}iso";
    assert_eq!(
        sanitize_output(raw.as_bytes(), STEP_OUTPUT_MAX_BYTES),
        "red plain\tcol\nnextbellc1rtliso"
    );
}

#[test]
fn invalid_utf8_is_replaced() {
    assert_eq!(sanitize_output(b"ok\xffok", 64), "ok\u{fffd}ok");
}

#[test]
fn output_is_cut_at_the_byte_limit_without_splitting_a_character() {
    assert_eq!(STEP_OUTPUT_MAX_BYTES, 16 * 1024);
    let long = "a".repeat(STEP_OUTPUT_MAX_BYTES + 100);
    assert_eq!(
        sanitize_output(long.as_bytes(), STEP_OUTPUT_MAX_BYTES).len(),
        STEP_OUTPUT_MAX_BYTES
    );
    // Three 3-byte characters under a 7-byte limit leave two.
    let euros = "\u{20ac}\u{20ac}\u{20ac}";
    assert_eq!(sanitize_output(euros.as_bytes(), 7), "\u{20ac}\u{20ac}");
}

// ---------------------------------------------------------------------
// A registration with no add / remove command: settings-file entries only
// ---------------------------------------------------------------------

const COMMANDLESS_STATUS: &str = "[mcp_registration.status]\n\
file = \".neutral/settings.json\"\n\
format = \"json\"\n\
command = \"/mcpServers/{server}/command\"\n";

/// The bridge entry a command-less registration needs: it names `{bridge}`
/// and its pointer ends in `{server}`.
const BRIDGE_ENTRY: &str =
    "\"/mcpServers/{server}\" = { command = \"{bridge}\", args = [], env = \"{env_refs}\" }\n";

/// A command-less table: `head` goes under `[mcp_registration]`, `elements`
/// after the status table.
fn commandless(head: &str, elements: &str) -> String {
    format!("[mcp_registration]\n{head}\n{COMMANDLESS_STATUS}\n{elements}")
}

/// One `[[mcp_registration.json_entry]]` element of `.neutral/settings.json`.
fn settings_element(extra_keys: &str, entries: &str) -> String {
    format!(
        "[[mcp_registration.json_entry]]\nfile = \".neutral/settings.json\"\n{extra_keys}\n\
         [mcp_registration.json_entry.entries]\n{entries}\n"
    )
}

fn parse_table(table: &str) -> Result<(), ProfileParseError> {
    let mut source = HEADER.to_string();
    source.push_str(table);
    parse_profile("neutral-cli", &source).map(|profile| {
        assert!(
            profile.mcp_registration.is_some(),
            "an accepted table must be carried: {table}"
        );
    })
}

fn assert_accepted(context: &str, table: &str) {
    if let Err(err) = parse_table(table) {
        panic!("{context}: the table must be accepted, got {err:?}\n{table}");
    }
}

fn assert_table_rejected(context: &str, table: &str) {
    let result = parse_table(table);
    assert!(
        matches!(result, Err(ProfileParseError::InvalidMcpRegistration(_))),
        "{context}: the table must be rejected as a registration table, got {result:?}\n{table}"
    );
}

#[test]
fn add_and_remove_may_both_be_omitted_when_a_bridge_entry_ends_in_the_server() {
    let bridge_only = settings_element("", BRIDGE_ENTRY);
    assert_accepted("both omitted", &commandless("", &bridge_only));

    let cases = [
        (
            "add alone",
            commandless(
                "add = [\"mcp\", \"add\", \"{server}\", \"{bridge}\"]",
                &bridge_only,
            ),
        ),
        (
            "remove alone",
            commandless("remove = [\"mcp\", \"remove\", \"{server}\"]", &bridge_only),
        ),
        (
            "env_arg without commands",
            commandless("env_arg = [\"--env\", \"{env}\"]", &bridge_only),
        ),
        (
            "env_forward without commands",
            commandless(
                "env_forward = { name = \"WSLENV\", separator = \":\" }",
                &bridge_only,
            ),
        ),
        (
            "no entry names the bridge",
            commandless(
                "",
                &settings_element(
                    "",
                    "\"/mcpServers/{server}\" = { command = \"x\", env = \"{env_refs}\" }\n",
                ),
            ),
        ),
        (
            "the bridge entry's pointer does not end in the server",
            commandless(
                "",
                &settings_element(
                    "",
                    "\"/mcpServers/{server}/inner\" = { command = \"{bridge}\", env = \
                     \"{env_refs}\" }\n",
                ),
            ),
        ),
    ];
    for (context, table) in cases {
        assert_table_rejected(context, &table);
    }
}

#[test]
fn json_entry_is_read_as_a_single_table_or_an_array_of_tables() {
    let single = commandless(
        "",
        &format!(
            "[mcp_registration.json_entry]\nfile = \".neutral/settings.json\"\n\n\
             [mcp_registration.json_entry.entries]\n{BRIDGE_ENTRY}"
        ),
    );
    assert_accepted("a single table", &single);

    let two_files = commandless(
        "",
        &format!(
            "{}{}",
            settings_element(
                "claim = \"/managed_hooks_path\"",
                &format!("{BRIDGE_ENTRY}\"/managed_hooks_path\" = \"managed-hooks.json\"\n"),
            ),
            "[[mcp_registration.json_entry]]\nfile = \".neutral/managed-hooks.json\"\n\n\
             [mcp_registration.json_entry.entries]\n\"/hooks\" = { Stop = \"run\" }\n"
        ),
    );
    assert_accepted("an array of two tables", &two_files);
}

#[test]
fn by_os_sets_are_read_and_a_set_may_not_repeat_a_pointer_of_the_entries() {
    let element = |by_os: &str| {
        commandless(
            "",
            &format!(
                "{}{by_os}",
                settings_element(
                    "claim = \"/managed_hooks_path\"",
                    &format!("{BRIDGE_ENTRY}\"/managed_hooks_path\" = \"managed-hooks.json\"\n"),
                )
            ),
        )
    };
    assert_accepted(
        "every operating-system key",
        &element(
            "[mcp_registration.json_entry.by_os.windows]\n\"/hooks\" = { Stop = \"win\" }\n\
             [mcp_registration.json_entry.by_os.macos]\n\"/hooks\" = { Stop = \"mac\" }\n\
             [mcp_registration.json_entry.by_os.linux]\n\"/hooks\" = { Stop = \"linux\" }\n\
             [mcp_registration.json_entry.by_os.default]\n\"/hooks\" = { Stop = \"any\" }\n",
        ),
    );
    // Only `default` is given, so it is the set chosen on every host.
    assert_table_rejected(
        "the chosen set repeats a pointer of the entries",
        &element(
            "[mcp_registration.json_entry.by_os.default]\n\
             \"/managed_hooks_path\" = \"other.json\"\n",
        ),
    );
}

#[test]
fn a_claim_is_required_by_a_shared_entry_and_at_most_one_is_allowed() {
    let shared = format!("{BRIDGE_ENTRY}\"/managed_hooks_path\" = \"managed-hooks.json\"\n");
    let hooks_element = |extra_keys: &str| {
        format!(
            "[[mcp_registration.json_entry]]\nfile = \".neutral/managed-hooks.json\"\n\
             {extra_keys}\n[mcp_registration.json_entry.entries]\n\"/hooks\" = {{ Stop = \"run\" \
             }}\n"
        )
    };
    assert_accepted(
        "one claim over the shared entries",
        &commandless(
            "",
            &format!(
                "{}{}",
                settings_element("claim = \"/managed_hooks_path\"", &shared),
                hooks_element("")
            ),
        ),
    );
    assert_accepted(
        "no shared entry needs no claim",
        &commandless("", &settings_element("", BRIDGE_ENTRY)),
    );
    assert_table_rejected(
        "a shared entry without a claim",
        &commandless(
            "",
            &format!("{}{}", settings_element("", &shared), hooks_element("")),
        ),
    );
    assert_table_rejected(
        "two claims",
        &commandless(
            "",
            &format!(
                "{}{}",
                settings_element("claim = \"/managed_hooks_path\"", &shared),
                hooks_element("claim = \"/hooks\"")
            ),
        ),
    );
}
