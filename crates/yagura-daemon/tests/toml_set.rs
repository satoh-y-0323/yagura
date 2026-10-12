#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: setting keys inside an existing table of a user's TOML
//! settings file. The file is the user's own, so everything outside the named
//! keys -- other tables, other keys, comments, order and line endings -- must
//! come back byte for byte, and a table that is not there is never created.

use yagura_daemon::toml_set::{TomlSetError, apply};

fn table(path: &[&str]) -> Vec<String> {
    path.iter().map(|part| (*part).to_string()).collect()
}

fn values(source: &str) -> toml::Table {
    toml::from_str(source).expect("fixture values must parse")
}

fn timeout_of(document: &str, server: &str) -> Option<toml::Value> {
    let parsed: toml::Table = toml::from_str(document).expect("the result must stay TOML");
    parsed
        .get("mcp_servers")?
        .get(server)?
        .get("tool_timeout_sec")
        .cloned()
}

#[test]
fn replacing_a_value_changes_only_that_value_and_keeps_crlf_and_comments() {
    let source = "# Settings kept by hand\r\nmodel = \"m\"\r\n\r\n[mcp_servers.yagura-dev]\r\n\
                  command = 'C:\\bin\\yagura-mcp.exe'\r\nargs = []\r\n\
                  tool_timeout_sec = 60 # tuned by hand\r\nenabled = true\r\n\r\n\
                  # another server\r\n[mcp_servers.other]\r\ntool_timeout_sec = 5\r\n";
    let result: String = apply(
        source,
        &table(&["mcp_servers", "yagura-dev"]),
        &values("tool_timeout_sec = 86400"),
    )
    .expect("the table exists");
    assert_eq!(
        result,
        source.replace(
            "tool_timeout_sec = 60 # tuned by hand",
            "tool_timeout_sec = 86400 # tuned by hand"
        )
    );
}

#[test]
fn adding_a_key_inserts_one_line_and_leaves_every_other_byte() {
    let source = "model = \"m\"\n\n[mcp_servers.yagura]\ncommand = \"/bin/yagura-mcp\"\n\
                  args = [] # none\n\n[mcp_servers.other]\ncommand = \"x\"\ntool_timeout_sec = 5\n";
    let result: String = apply(
        source,
        &table(&["mcp_servers", "yagura"]),
        &values("tool_timeout_sec = 86400"),
    )
    .expect("the table exists");

    assert_eq!(
        timeout_of(&result, "yagura"),
        Some(toml::Value::Integer(86400))
    );
    assert_eq!(timeout_of(&result, "other"), Some(toml::Value::Integer(5)));
    assert_eq!(
        result.matches("tool_timeout_sec = 86400\n").count(),
        1,
        "exactly one new line: {result:?}"
    );
    assert_eq!(
        result.replacen("tool_timeout_sec = 86400\n", "", 1),
        source,
        "nothing but the new line may change"
    );
    let inserted_at = result.find("tool_timeout_sec = 86400").unwrap();
    let other_at = result.find("[mcp_servers.other]").unwrap();
    assert!(inserted_at < other_at, "the key belongs to its own table");
}

#[test]
fn a_crlf_file_gets_its_added_line_in_crlf() {
    let source = "[mcp_servers.yagura]\r\ncommand = \"/bin/yagura-mcp\"\r\n";
    let result: String = apply(
        source,
        &table(&["mcp_servers", "yagura"]),
        &values("tool_timeout_sec = 86400"),
    )
    .expect("the table exists");
    assert_eq!(
        result,
        "[mcp_servers.yagura]\r\ncommand = \"/bin/yagura-mcp\"\r\ntool_timeout_sec = 86400\r\n"
    );
}

#[test]
fn keys_not_named_in_values_are_never_removed() {
    let source = "[mcp_servers.yagura]\ncommand = \"/bin/yagura-mcp\"\nstartup_timeout_sec = 20\n\
                  \n[mcp_servers.yagura.env]\nYAGURA_INSTANCE = \"dev\"\n";
    let result: String = apply(
        source,
        &table(&["mcp_servers", "yagura"]),
        &values("tool_timeout_sec = 86400\nnote = \"kept\""),
    )
    .expect("the table exists");
    let parsed: toml::Table = toml::from_str(&result).expect("the result must stay TOML");
    let server = parsed["mcp_servers"]["yagura"].as_table().unwrap();
    assert_eq!(server["command"].as_str(), Some("/bin/yagura-mcp"));
    assert_eq!(server["startup_timeout_sec"].as_integer(), Some(20));
    assert_eq!(server["env"]["YAGURA_INSTANCE"].as_str(), Some("dev"));
    assert_eq!(server["tool_timeout_sec"].as_integer(), Some(86400));
    assert_eq!(server["note"].as_str(), Some("kept"));
}

#[test]
fn a_missing_table_is_refused_and_not_created() {
    // A missing table means the CLI reads another file; writing one here would
    // edit a file the CLI never reads.
    let source = "[mcp_servers.other]\ncommand = \"x\"\n";
    for path in [
        table(&["mcp_servers", "yagura"]),
        table(&["elsewhere", "yagura"]),
    ] {
        let result = apply(source, &path, &values("tool_timeout_sec = 86400"));
        assert!(
            matches!(result, Err(TomlSetError::TableNotFound)),
            "{path:?}: got {result:?}"
        );
    }
}

#[test]
fn an_unreadable_document_is_refused() {
    let result = apply(
        "[[[ not toml",
        &table(&["mcp_servers", "yagura"]),
        &values("tool_timeout_sec = 86400"),
    );
    assert!(
        matches!(result, Err(TomlSetError::Unparsable)),
        "got {result:?}"
    );
}

#[test]
fn a_table_name_holding_a_dot_is_one_key_not_two() {
    let source =
        "[mcp_servers.a.b]\ncommand = \"nested\"\n\n[mcp_servers.\"a.b\"]\ncommand = \"quoted\"\n";
    let result: String = apply(
        source,
        &table(&["mcp_servers", "a.b"]),
        &values("tool_timeout_sec = 86400"),
    )
    .expect("the quoted table exists");
    assert_eq!(
        result,
        "[mcp_servers.a.b]\ncommand = \"nested\"\n\n[mcp_servers.\"a.b\"]\ncommand = \"quoted\"\n\
         tool_timeout_sec = 86400\n"
    );
}
