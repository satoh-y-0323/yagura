#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: a `[mcp_registration] notice` reaches the registration
//! list a daemon answers, verbatim, and a notice that is not one visible line
//! makes the table invalid (the `[shim_guidance] text` rule).

mod common;

use std::time::Duration;

use common::{
    Harness, approver_base_config, mcp_cli_stub_profile_with_table, read_response,
    register_approver, send, unique_temp_dir, write_profile,
};
use yagura_daemon::DaemonConfig;
use yagura_daemon::profile::{ProfileParseError, parse_profile};
use yagura_protocol::{ClientMessage, DaemonMessage, McpRegistrationRow};

const REPLY_TIMEOUT: Duration = Duration::from_secs(20);

fn table(notice_line: &str) -> String {
    format!(
        "[mcp_registration]\n\
         add = [\"mcp\", \"add\", \"{{server}}\", \"--\", \"{{bridge}}\"]\n\
         remove = [\"mcp\", \"remove\", \"{{server}}\"]\n\
         {notice_line}\n\
         [mcp_registration.status]\n\
         args = [\"mcp\", \"get\", \"{{server}}\"]\n\
         format = \"lines\"\n\
         command = \"Command:\"\n\
         absent = \"No MCP server named\"\n"
    )
}

fn profile_source(notice_line: &str) -> String {
    mcp_cli_stub_profile_with_table("Neutral Cli", "", &table(notice_line), &[])
}

async fn list_rows(profile_sources: &[(&str, String)]) -> Vec<McpRegistrationRow> {
    let root = unique_temp_dir("mcpreg-notice");
    let profiles_dir = root.join("profiles");
    let home_dir = root.join("home");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::create_dir_all(&home_dir).unwrap();
    for (id, source) in profile_sources {
        write_profile(&profiles_dir, id, source);
    }
    let config = DaemonConfig {
        profiles_dir: Some(profiles_dir),
        home_dir: Some(home_dir),
        instance_id: None,
        mcp_bridge: Some(root.join("bridge").join("yagura-mcp")),
        mcp_registration_timeout: Duration::from_secs(20),
        mcp_registration_max_output_bytes: 1 << 20,
        mcp_registration_status_max_concurrent: 2,
        ..approver_base_config()
    };
    let h = Harness::start("mcpreg-notice", config).await;
    let mut client = h.connect().await;
    register_approver(&mut client).await;
    send(
        &mut client,
        &ClientMessage::ListMcpRegistrations { request_id: 1 },
    )
    .await;
    let reply: DaemonMessage = read_response(&mut client, REPLY_TIMEOUT).await;
    match reply {
        DaemonMessage::McpRegistrationsListed { rows, .. } => rows,
        other => panic!("expected mcp_registrations_listed, got {other:?}"),
    }
}

fn notice_of(rows: &[McpRegistrationRow], profile: &str) -> Option<serde_json::Value> {
    let row = rows
        .iter()
        .find(|row| row.profile == profile)
        .unwrap_or_else(|| panic!("no row was listed for {profile}: {rows:?}"));
    // Read through the wire form so the test names no struct field.
    serde_json::to_value(row).unwrap().get("notice").cloned()
}

#[tokio::test]
async fn a_notice_is_listed_verbatim_and_a_profile_without_one_lists_none() {
    let sentence = "Answering yes also lets that folder's own hooks run.";
    let rows = list_rows(&[
        (
            "with-notice",
            profile_source(&format!("notice = \"{sentence}\"")),
        ),
        ("without-notice", profile_source("")),
    ])
    .await;
    assert_eq!(
        notice_of(&rows, "with-notice"),
        Some(serde_json::json!(sentence))
    );
    assert_eq!(
        notice_of(&rows, "without-notice"),
        Some(serde_json::Value::Null)
    );
}

#[test]
fn a_notice_that_is_not_one_visible_line_makes_the_table_invalid() {
    assert!(
        parse_profile("ok", &profile_source("notice = \"One visible line.\"")).is_ok(),
        "a one-line notice must be accepted"
    );
    for bad in [
        "notice = \"\"",
        "notice = \"   \"",
        "notice = \"two\\nlines\"",
        "notice = \"tab\there\"",
        "notice = 7",
    ] {
        let result = parse_profile("bad", &profile_source(bad));
        assert!(
            matches!(result, Err(ProfileParseError::InvalidMcpRegistration(_))),
            "{bad} must make the registration table invalid, got {result:?}"
        );
    }
}
