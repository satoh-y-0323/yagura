#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: a launch profile's top-level `[mcp_registration]` table
//! -- the data that tells the daemon how to register its own MCP bridge with
//! a CLI -- is read, validated, and falls back to the shipped profile's table
//! when the placed profile has none.
//!
//! Profiles here use a neutral CLI name: the mechanism must not know any
//! vendor, so nothing in these fixtures names one.

use yagura_daemon::mcp_registration::{McpRegistrationSpec, RegistrationTarget, StatusPlan};
use yagura_daemon::profile::{Profile, ProfileParseError, parse_profile};

const HEADER: &str =
    "schema = 1\ndisplay_name = \"Neutral Cli\"\n\n[command]\ndefault = [\"neutral-cli\"]\n\n";

/// A host row whose status is a JSON document, with a settings-file edit.
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

/// A host row whose status is a set of `Prefix: value` lines.
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

/// A host row whose status is read from a JSON settings file, not a command.
const FILE_TABLE: &str = r#"[mcp_registration]
add = ["mcp", "add", "{server}", "{env_args}", "{bridge}"]
remove = ["mcp", "remove", "{server}"]
env_arg = ["--env", "{env}"]

[mcp_registration.status]
file = ".neutral/mcp_config.json"
format = "json"
command = "/mcpServers/{server}/command"
"#;

/// A row inside another environment: the bridge path comes from a probe,
/// files are read and written through that environment's own tools, and the
/// instance variable's name is forwarded.
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

[mcp_registration.toml_set]
file = ".neutral/config.toml"
table = ["mcp_servers", "{server}"]
values = { tool_timeout_sec = 86400 }
confirm = { tool_timeout_sec = "/tool_timeout_sec" }
"#;

const BRIDGE: &str = r"C:\Tools\yagura\yagura-mcp.exe";

fn document(table: &str) -> String {
    let mut source = HEADER.to_string();
    source.push_str(table);
    source
}

fn parse(id: &str, table: &str) -> Result<Profile, ProfileParseError> {
    parse_profile(id, &document(table))
}

fn spec_of(id: &str, table: &str) -> McpRegistrationSpec {
    parse(id, table)
        .unwrap_or_else(|err| panic!("{id} must parse: {err:?}"))
        .mcp_registration
        .unwrap_or_else(|| panic!("{id} must carry a registration table"))
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| (*item).to_string()).collect()
}

fn default_target() -> RegistrationTarget {
    RegistrationTarget::for_instance(None)
}

fn assert_rejected(id: &str, table: &str) {
    let result = parse(id, table);
    assert!(
        matches!(result, Err(ProfileParseError::InvalidMcpRegistration(_))),
        "{id}: a broken registration table must exclude the profile with the registration \
         variant, got {result:?}"
    );
}

// ---------------------------------------------------------------------
// Reading the table
// ---------------------------------------------------------------------

#[test]
fn a_profile_without_a_registration_table_has_none() {
    let profile = parse("no-registration", "").expect("a plain profile must parse");
    assert!(profile.mcp_registration.is_none());
}

#[test]
fn a_json_status_table_with_a_settings_edit_is_read_into_the_spec() {
    let spec = spec_of("json-row", JSON_TABLE);
    let target = default_target();

    assert_eq!(spec.group(), None, "a host row names no group");
    assert_eq!(
        spec.render_add(&target, BRIDGE).expect("default instance"),
        strings(&["mcp", "add", "yagura", "--", BRIDGE])
    );
    let status: StatusPlan = spec.render_status(&target);
    match status {
        StatusPlan::Run(args) => {
            assert_eq!(args, strings(&["mcp", "get", "yagura", "--json"]));
        }
        other => panic!("a status with args must run a command, got {other:?}"),
    }
    let toml_set = spec
        .toml_set()
        .expect("the settings edit table must be kept");
    assert_eq!(toml_set.file(), ".neutral/config.toml");
    assert_eq!(toml_set.table(&target), strings(&["mcp_servers", "yagura"]));
    assert_eq!(
        toml_set.values().get("tool_timeout_sec"),
        Some(&toml::Value::Integer(86400))
    );
    assert_eq!(spec.render_probe(BRIDGE), None, "a host row has no probe");
    assert_eq!(spec.render_file_read(".neutral/config.toml"), None);
}

#[test]
fn a_lines_status_table_is_read_into_the_spec() {
    let spec = spec_of("lines-row", LINES_TABLE);
    let target = default_target();
    assert_eq!(
        spec.render_remove(&target),
        strings(&["mcp", "remove", "--scope", "user", "yagura"])
    );
    assert!(spec.toml_set().is_none(), "no settings edit was written");
}

#[test]
fn a_file_status_table_is_read_as_a_file_to_read_rather_than_a_command() {
    let spec = spec_of("file-row", FILE_TABLE);
    let status: StatusPlan = spec.render_status(&default_target());
    match status {
        StatusPlan::ReadFile { file } => assert_eq!(file, ".neutral/mcp_config.json"),
        other => panic!("a status with a file must read that file, got {other:?}"),
    }
}

#[test]
fn a_foreign_environment_table_keeps_its_group_probe_and_file_tools() {
    let spec = spec_of("foreign-row", FOREIGN_TABLE);
    assert_eq!(spec.group(), Some("WSL"));
    assert_eq!(
        spec.render_probe(BRIDGE),
        Some(strings(&["wsl", "-e", "wslpath", "-u", BRIDGE]))
    );
    let read: Vec<String> = spec
        .render_file_read(".neutral/config.toml")
        .expect("the table has a read tool");
    assert_eq!(
        read.last().map(String::as_str),
        Some(".neutral/config.toml")
    );
    assert_eq!(
        read[..read.len() - 1],
        strings(&[
            "wsl",
            "-e",
            "sh",
            "-c",
            "[ -e \"$HOME/$1\" ] || exit 3; cat \"$HOME/$1\"",
            "sh"
        ])[..],
        "the file goes in as a positional argument, never into the script text"
    );
    let replace: Vec<String> = spec
        .render_file_replace(".neutral/config.toml")
        .expect("the table has a replace tool");
    assert_eq!(
        replace.last().map(String::as_str),
        Some(".neutral/config.toml")
    );
}

#[test]
fn a_foreign_file_table_may_have_a_read_tool_without_a_replace_tool() {
    let table = FILE_TABLE.replace(
        "env_arg = [\"--env\", \"{env}\"]\n",
        "env_arg = [\"--env\", \"{env}\"]\ngroup = \"WSL\"\nenv_forward = { name = \
         \"WSLENV\", separator = \":\" }\nfiles = { read = [\"wsl\", \"-e\", \"sh\", \"-c\", \
         \"cat \\\"$HOME/$1\\\"\", \"sh\", \"{file}\"], absent_exit = 3 }\n",
    );
    let table = format!(
        "[host_paths]\nconvert = [\"wsl\", \"-e\", \"wslpath\", \"-u\", \"{{path}}\"]\n\n{table}"
    );
    let spec = spec_of("foreign-file-row", &table);
    assert!(spec.render_file_read(".neutral/mcp_config.json").is_some());
    assert_eq!(spec.render_file_replace(".neutral/mcp_config.json"), None);
}

#[test]
fn a_cli_side_environment_reference_passes_through_verbatim() {
    let table = JSON_TABLE.replace("\"--\", \"{bridge}\"]", "\"${HOME}/bin\", \"{bridge}\"]");
    let spec = spec_of("dollar-brace", &table);
    assert_eq!(
        spec.render_add(&default_target(), BRIDGE)
            .expect("default instance"),
        strings(&["mcp", "add", "yagura", "${HOME}/bin", BRIDGE])
    );
}

#[test]
fn registration_keys_placed_inside_the_mcp_table_still_exclude_the_profile() {
    let result = parse(
        "mcp-with-add",
        "[mcp]\nargs = [\"--flag\"]\nadd = [\"mcp\", \"add\", \"{server}\"]\n",
    );
    assert!(
        matches!(result, Err(ProfileParseError::Toml(_))),
        "the [mcp] table keeps refusing a key it does not know, got {result:?}"
    );
}

// ---------------------------------------------------------------------
// Excluding a broken table
// ---------------------------------------------------------------------

#[test]
fn a_table_missing_add_remove_or_status_is_rejected() {
    assert_rejected(
        "missing-add",
        &JSON_TABLE.replace(
            "add = [\"mcp\", \"add\", \"{server}\", \"{env_args}\", \"--\", \"{bridge}\"]\n",
            "",
        ),
    );
    assert_rejected(
        "missing-remove",
        &JSON_TABLE.replace("remove = [\"mcp\", \"remove\", \"{server}\"]\n", ""),
    );
    assert_rejected(
        "missing-status",
        "[mcp_registration]\nadd = [\"mcp\", \"add\", \"{server}\", \"{bridge}\"]\nremove = \
         [\"mcp\", \"remove\", \"{server}\"]\n",
    );
}

#[test]
fn a_status_format_the_mechanism_does_not_read_is_rejected() {
    assert_rejected(
        "unknown-format",
        &JSON_TABLE.replace("format = \"json\"", "format = \"yaml\""),
    );
}

#[test]
fn a_pointer_outside_the_json_pointer_syntax_is_rejected() {
    assert_rejected(
        "bad-command-pointer",
        &JSON_TABLE.replace(
            "command = \"/transport/command\"",
            "command = \"transport/command\"",
        ),
    );
    assert_rejected(
        "bad-args-pointer",
        &JSON_TABLE.replace(
            "args_pointer = \"/transport/args\"",
            "args_pointer = \"transport/args\"",
        ),
    );
    assert_rejected(
        "bad-confirm-pointer",
        &JSON_TABLE.replace(
            "confirm = { tool_timeout_sec = \"/tool_timeout_sec\" }",
            "confirm = { tool_timeout_sec = \"tool_timeout_sec\" }",
        ),
    );
}

#[test]
fn a_placeholder_the_mechanism_does_not_render_is_rejected() {
    assert_rejected(
        "unknown-in-add",
        &JSON_TABLE.replace("\"--\", \"{bridge}\"]", "\"{nope}\", \"{bridge}\"]"),
    );
    assert_rejected(
        "unknown-in-status",
        &JSON_TABLE.replace(
            "args = [\"mcp\", \"get\", \"{server}\", \"--json\"]",
            "args = [\"mcp\", \"get\", \"{nope}\", \"--json\"]",
        ),
    );
}

#[test]
fn env_args_written_inside_a_larger_element_is_rejected() {
    // It replaces a whole element; spliced into text it would render nothing
    // sensible.
    assert_rejected(
        "partial-env-args",
        &JSON_TABLE.replace("\"{env_args}\", \"--\"", "\"x{env_args}\", \"--\""),
    );
}

#[test]
fn env_outside_env_arg_is_rejected() {
    assert_rejected(
        "env-in-add",
        &JSON_TABLE.replace("\"--\", \"{bridge}\"]", "\"{env}\", \"{bridge}\"]"),
    );
}

#[test]
fn file_outside_the_file_tools_is_rejected() {
    assert_rejected(
        "file-in-add",
        &JSON_TABLE.replace("\"--\", \"{bridge}\"]", "\"{file}\", \"{bridge}\"]"),
    );
}

#[test]
fn a_settings_file_that_climbs_out_of_home_or_is_absolute_is_rejected() {
    for (id, file) in [
        ("parent-file", "../.neutral/config.toml"),
        ("nested-parent-file", ".neutral/../../config.toml"),
        ("unix-absolute-file", "/etc/neutral/config.toml"),
        (
            "windows-absolute-file",
            "C:\\\\Users\\\\someone\\\\config.toml",
        ),
    ] {
        assert_rejected(
            id,
            &JSON_TABLE.replace(
                "file = \".neutral/config.toml\"",
                &format!("file = \"{file}\""),
            ),
        );
    }
    assert_rejected(
        "parent-status-file",
        &FILE_TABLE.replace(
            "file = \".neutral/mcp_config.json\"",
            "file = \"../mcp_config.json\"",
        ),
    );
}

/// [`FILE_TABLE`] with a JSON settings-file entry whose value names the
/// daemon several levels down: an object holding an array of objects.
fn json_entry_table(file: &str, command: &str) -> String {
    format!(
        "{FILE_TABLE}\n[mcp_registration.json_entry]\nfile = \"{file}\"\n\n\
         [mcp_registration.json_entry.entries.\"{{server}}\"]\n\
         Before = [{{ type = \"command\", command = \"{command}\" }}]\n"
    )
}

#[test]
fn a_json_entry_naming_only_the_daemon_inside_a_home_relative_file_is_read() {
    let spec = spec_of(
        "json-entry",
        &json_entry_table(".neutral/hooks.json", "{daemon} --emit-status running"),
    );
    assert!(spec.json_entry().is_some());
}

#[test]
fn a_json_entry_value_naming_a_placeholder_it_does_not_know_is_rejected() {
    // Nested inside an object and an array, so every level of the walk is crossed.
    assert_rejected(
        "json-entry-unknown",
        &json_entry_table(".neutral/hooks.json", "{nosuch} --emit-status running"),
    );
}

#[test]
fn a_json_entry_file_that_climbs_out_of_home_is_rejected() {
    assert_rejected(
        "json-entry-parent-file",
        &json_entry_table("../hooks.json", "{daemon} --emit-status running"),
    );
}

#[test]
fn a_group_heading_carrying_a_control_or_bidi_character_is_rejected() {
    for (id, group) in [
        ("control-group", "WS\\u0007L"),
        ("bidi-group", "WS\\u202EL"),
    ] {
        assert_rejected(
            id,
            &FOREIGN_TABLE.replace("group = \"WSL\"", &format!("group = \"{group}\"")),
        );
    }
}

#[test]
fn an_unknown_key_anywhere_in_the_table_is_rejected() {
    // A misspelt key must not silently mean "not set".
    assert_rejected(
        "unknown-top-key",
        &JSON_TABLE.replace("[mcp_registration]\n", "[mcp_registration]\nbogus = 1\n"),
    );
    assert_rejected(
        "unknown-status-key",
        &JSON_TABLE.replace(
            "[mcp_registration.status]\n",
            "[mcp_registration.status]\nbogus = 1\n",
        ),
    );
    assert_rejected(
        "unknown-toml-set-key",
        &JSON_TABLE.replace(
            "[mcp_registration.toml_set]\n",
            "[mcp_registration.toml_set]\nbogus = 1\n",
        ),
    );
    assert_rejected(
        "unknown-files-key",
        &FOREIGN_TABLE.replace("absent_exit = 3 }", "absent_exit = 3, bogus = 1 }"),
    );
    assert_rejected(
        "unknown-env-forward-key",
        &FOREIGN_TABLE.replace("separator = \":\" }", "separator = \":\", bogus = 1 }"),
    );
}

#[test]
fn the_rejection_reads_as_a_fixed_phrase_that_quotes_nothing_from_the_document() {
    let table = JSON_TABLE.replace(
        "\"--\", \"{bridge}\"]",
        "\"{nope}\", \"SECRET-TOKEN-VALUE\", \"{bridge}\"]",
    );
    let err = parse("secret-row", &table).expect_err("an unknown placeholder must be refused");
    assert!(matches!(err, ProfileParseError::InvalidMcpRegistration(_)));
    assert_eq!(err.to_string(), "invalid mcp registration table");
}
