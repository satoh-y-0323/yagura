#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Wire revision 14 as the daemon answers it: the mark that a window's status
//! reports stopped arriving (`WindowInfo.status_reports_missing` on
//! `list_windows`) and the hook entries a registration will write
//! (`McpRegistrationRow.hook_entries`).
//!
//! Both are driven through a real `Harness` over a real IPC endpoint. The
//! mark is produced the way it happens in use: a window launched with hooks
//! whose screen shows a running turn for the grace time without one report.

mod common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use common::docs::read_doc_verbatim;
use common::{
    Harness, approver_base_config, call_list_windows, empty_lookup, mcp_cli_stub_path,
    read_response, register_approver, send, unique_temp_dir, wait_until_status, write_profile,
};
use yagura_daemon::DaemonConfig;
use yagura_daemon::detector::engine::REPORT_GAP_GRACE;
use yagura_daemon::profile::ProfileOs;
use yagura_protocol::{
    ClientMessage, DaemonMessage, McpRegistrationHookEntry, McpRegistrationRow, WindowInfo,
    WindowStatus,
};
use yagura_test_support::ipc::Connection;
use yagura_test_support::process::ready_child_path;

const RUNNING_MARKER: &str = "GAP-RUNNING";
const DETECTION_NAME: &str = "gap-test";

const DEFINITION: &str = r#"
schema_version = 1
name = "gap-test"

[debounce]
confirm_ms = 30

[[rules]]
status = "running"
pattern = 'GAP-RUNNING'
priority = 10
region = "bottom"
"#;

fn is_file(path: &Path) -> bool {
    path.is_file()
}

fn profile_source(hooks: bool) -> String {
    let child = ready_child_path();
    let child = child.to_str().expect("ready_child path is UTF-8");
    let hooks_table = if hooks {
        "\n[hooks]\nargs = [\"--hooks-on\"]\n"
    } else {
        ""
    };
    format!(
        "schema = 1\ndisplay_name = \"Gap\"\ndetection = \"{DETECTION_NAME}\"\n\n\
         [command]\ndefault = ['{child}', '{RUNNING_MARKER}']\n{hooks_table}"
    )
}

async fn open_from_profile(client: &mut Connection, profile: &str) -> String {
    send(
        client,
        &ClientMessage::OpenWindowFromProfile {
            profile: profile.to_string(),
            cwd: None,
            name: None,
            model: None,
            effort: None,
            on_name_taken: Default::default(),
        },
    )
    .await;
    match read_response(client, Duration::from_secs(5)).await {
        DaemonMessage::WindowOpened { window_id, .. } => window_id,
        other => panic!("expected window_opened, got {other:?}"),
    }
}

async fn listed(client: &mut Connection, window_id: &str) -> WindowInfo {
    call_list_windows(client)
        .await
        .into_iter()
        .find(|window| window.window_id == window_id)
        .unwrap_or_else(|| panic!("window {window_id} is missing from list_windows"))
}

#[tokio::test]
async fn list_windows_marks_only_the_window_whose_expected_reports_never_came() {
    let root = unique_temp_dir("report-gap-mark");
    let profiles_dir = root.join("profiles");
    let detection_dir = root.join("detection");
    let config_dir = root.join("mcp");
    for dir in [&profiles_dir, &detection_dir, &config_dir] {
        std::fs::create_dir_all(dir).expect("create fixture dir");
    }
    std::fs::write(
        detection_dir.join(format!("{DETECTION_NAME}.toml")),
        DEFINITION,
    )
    .expect("write the detection definition");
    write_profile(&profiles_dir, "hooked", &profile_source(true));
    write_profile(&profiles_dir, "plain", &profile_source(false));

    let config = DaemonConfig {
        profiles_dir: Some(profiles_dir),
        detection_dir: Some(detection_dir),
        mcp_config_dir: Some(config_dir),
        host_os: ProfileOs::host(),
        program_lookup: empty_lookup(),
        program_is_executable: is_file,
        default_shell: None,
        ..approver_base_config()
    };
    let h = Harness::start("report-gap-mark", config).await;
    let mut client = h.connect().await;

    let hooked = open_from_profile(&mut client, "hooked").await;
    let plain = open_from_profile(&mut client, "plain").await;
    for id in [&hooked, &plain] {
        wait_until_status(
            &mut client,
            id,
            WindowStatus::Running,
            Duration::from_secs(10),
        )
        .await;
    }

    // Inside the grace nothing is missing yet, for either window.
    assert_eq!(
        listed(&mut client, &hooked).await.status_reports_missing,
        Some(false)
    );
    assert_eq!(
        listed(&mut client, &plain).await.status_reports_missing,
        Some(false)
    );

    let deadline = tokio::time::Instant::now() + REPORT_GAP_GRACE + Duration::from_secs(15);
    loop {
        if listed(&mut client, &hooked).await.status_reports_missing == Some(true) {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "a window launched with hooks that showed a running turn past the grace without a \
             report must be listed with status_reports_missing = Some(true)"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    let hooked_info = listed(&mut client, &hooked).await;
    let plain_info = listed(&mut client, &plain).await;
    assert_eq!(
        plain_info.status_reports_missing,
        Some(false),
        "a window not launched with hooks expects no reports, so it is never marked"
    );
    assert_eq!(
        hooked_info.status,
        WindowStatus::Running,
        "the mark never changes the confirmed status"
    );
}

// =============================================================================
// Hook entries in the registration listing
// =============================================================================

fn str_array(items: &[&str]) -> toml::Value {
    toml::Value::Array(
        items
            .iter()
            .map(|item| toml::Value::String((*item).to_string()))
            .collect(),
    )
}

/// The shipped `profile` with only its launch command swapped for the stub
/// fixture, so the row is listed without any real CLI present.
fn shipped_profile_with_stub_command(profile: &str) -> String {
    let stub = mcp_cli_stub_path();
    let stub = stub.to_str().expect("stub path is UTF-8");
    let mut root: toml::Table = read_doc_verbatim(&format!("../../profiles/{profile}.toml"))
        .parse()
        .expect("the shipped profile is TOML");
    let mut command = toml::Table::new();
    command.insert("default".into(), str_array(&[stub]));
    root.insert("command".into(), toml::Value::Table(command));
    toml::to_string(&root).expect("serialize the fixture profile")
}

async fn listed_rows(tag: &str, profiles: &[&str]) -> Vec<McpRegistrationRow> {
    let root = unique_temp_dir(tag);
    let profiles_dir = root.join("profiles");
    let home_dir = root.join("home");
    let report_dir: PathBuf = root.join("state").join("bin");
    for dir in [&profiles_dir, &home_dir] {
        std::fs::create_dir_all(dir).expect("create fixture dir");
    }
    for profile in profiles {
        write_profile(
            &profiles_dir,
            profile,
            &shipped_profile_with_stub_command(profile),
        );
    }
    let config = DaemonConfig {
        profiles_dir: Some(profiles_dir),
        home_dir: Some(home_dir),
        report_dir: Some(report_dir),
        mcp_bridge: Some(root.join("bridge").join("yagura-mcp")),
        host_os: ProfileOs::host(),
        program_lookup: empty_lookup(),
        program_is_executable: is_file,
        default_shell: None,
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
    match read_response(&mut client, Duration::from_secs(20)).await {
        DaemonMessage::McpRegistrationsListed { rows, .. } => rows,
        other => panic!("expected mcp_registrations_listed, got {other:?}"),
    }
}

fn row<'a>(rows: &'a [McpRegistrationRow], profile: &str) -> &'a McpRegistrationRow {
    rows.iter()
        .find(|row| row.profile == profile)
        .unwrap_or_else(|| panic!("the listing has no row for {profile}"))
}

#[tokio::test]
async fn the_antigravity_row_previews_the_hooks_entry_it_will_add() {
    let rows = listed_rows("hook-preview-agy", &["antigravity-cli"]).await;
    let entries = &row(&rows, "antigravity-cli").hook_entries;

    assert_eq!(
        entries.len(),
        1,
        "one top-level entry is written: {entries:?}"
    );
    let entry = &entries[0];
    assert_eq!(entry.file, ".gemini/config/hooks.json");
    assert_eq!(entry.name, "yagura");
    let content: serde_json::Value =
        serde_json::from_str(&entry.content).expect("the preview content is the entry's JSON");
    assert!(
        content.get("PreInvocation").is_some() && content.get("PostToolUse").is_some(),
        "the preview must show the events the entry reports on: {content}"
    );
    assert!(
        entry
            .content
            .contains("--emit-status running --source antigravity-cli"),
        "the preview must show the command the hook runs: {}",
        entry.content
    );
}

#[tokio::test]
async fn the_codex_row_previews_the_trust_entries_next_to_its_settings_preview() {
    let rows = listed_rows("hook-preview-codex", &["codex-cli"]).await;
    let codex = row(&rows, "codex-cli");

    let mut events: Vec<&str> = codex
        .hook_entries
        .iter()
        .map(|entry: &McpRegistrationHookEntry| {
            assert_eq!(entry.file, ".codex/config.toml");
            assert!(
                entry.content.contains("trusted_hash") && entry.content.contains("sha256:"),
                "each entry carries the hash Codex checks: {}",
                entry.content
            );
            let mut parts = entry.name.rsplitn(3, ':');
            let is_index =
                |part: Option<&str>| part.is_some_and(|text| text.parse::<u32>().is_ok());
            let (handler, group) = (parts.next(), parts.next());
            assert!(
                is_index(handler) && is_index(group),
                "an entry ends in the hook's group and handler index: {entry:?}"
            );
            parts
                .next()
                .and_then(|head| head.rsplit(':').next())
                .unwrap_or_else(|| {
                    panic!("an entry names one hook of the command-line layer: {entry:?}")
                })
        })
        .collect();
    events.sort_unstable();
    assert_eq!(
        events,
        [
            "interrupt",
            "permission_request",
            "post_tool_use",
            "pre_tool_use",
            "session_start",
            "stop",
            "user_prompt_submit",
            "user_prompt_submit",
            "user_prompt_submit",
        ],
        "one trust entry per hook the profile injects"
    );

    let toml_set = codex
        .toml_set
        .as_ref()
        .expect("the existing settings preview stays where it was");
    assert_eq!(toml_set.file, ".codex/config.toml");
    assert!(
        toml_set
            .values
            .iter()
            .any(|value| value.key == "tool_timeout_sec"),
        "the existing settings preview is unchanged: {toml_set:?}"
    );
}

#[tokio::test]
async fn a_row_that_writes_no_hook_entries_lists_none() {
    // Claude Code receives its hooks at launch, so registration writes none.
    let rows = listed_rows("hook-preview-none", &["claude-code"]).await;
    assert!(
        row(&rows, "claude-code").hook_entries.is_empty(),
        "a profile that injects its hooks at launch has nothing to preview"
    );
}
