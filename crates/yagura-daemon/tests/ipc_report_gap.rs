#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Wire revision 14 as the daemon answers it: the mark that a window's status
//! reports stopped arriving (`WindowInfo.status_reports_missing` on
//! `list_windows`) and the hook entries a registration will write
//! (`McpRegistrationRow.entries`).
//!
//! Both are driven through a real `Harness` over a real IPC endpoint. The
//! mark is produced the way it happens in use: a window launched with hooks
//! whose screen shows a running turn for the grace time without one report.
//! The grace is passed on the test's clock once both children have shown all
//! they will; its length and the mark's other rules are measured on a paused
//! clock in `detector_report_gap.rs`.

use crate::common;
use crate::common::bins::ready_child_path;

use std::path::{Path, PathBuf};

use common::bins::mcp_cli_stub_path;
use common::docs::read_doc_verbatim;
use common::{
    Harness, approver_base_config, call_list_windows, empty_lookup, read_response,
    register_approver, registration_cwd_in, send, unique_temp_dir, wait_until_status,
    write_profile,
};
use yagura_daemon::DaemonConfig;
use yagura_daemon::profile::ProfileOs;
use yagura_protocol::{
    ClientMessage, DaemonMessage, McpRegistrationEntry, McpRegistrationRow, WindowInfo,
    WindowStatus,
};
use yagura_test_support::ipc::Connection;

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
    match read_response(client).await {
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
        wait_until_status(&mut client, id, WindowStatus::Running).await;
    }

    // Both children printed their only line and now wait for input, so nothing
    // is on its way: every clock bound, the grace among them, can run out.
    common::let_every_clock_bound_run_out().await;

    assert_eq!(
        listed(&mut client, &hooked).await.status_reports_missing,
        Some(true),
        "a window launched with hooks that showed a running turn past the grace without a \
         report must be listed with status_reports_missing = Some(true)"
    );
    assert_eq!(
        listed(&mut client, &plain).await.status_reports_missing,
        Some(false),
        "a window not launched with hooks expects no reports, so it is never marked"
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
    with_stub_command(&read_doc_verbatim(&format!(
        "../../profiles/{profile}.toml"
    )))
}

/// The fixture profile `name` under `tests/fixtures/profiles` with only its launch
/// command swapped for the stub fixture.
fn fixture_profile_with_stub_command(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/profiles")
        .join(format!("{name}.toml"));
    let source = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("read the fixture {}: {err}", path.display()));
    with_stub_command(&source)
}

fn with_stub_command(source: &str) -> String {
    let stub = mcp_cli_stub_path();
    let stub = stub.to_str().expect("stub path is UTF-8");
    let mut root: toml::Table = source.parse().expect("the profile is TOML");
    let mut command = toml::Table::new();
    command.insert("default".into(), str_array(&[stub]));
    root.insert("command".into(), toml::Value::Table(command));
    toml::to_string(&root).expect("serialize the fixture profile")
}

async fn listed_rows(tag: &str, profiles: &[&str]) -> Vec<McpRegistrationRow> {
    let placed: Vec<(&str, String)> = profiles
        .iter()
        .map(|profile| (*profile, shipped_profile_with_stub_command(profile)))
        .collect();
    listed_rows_of(tag, &placed, None).await
}

/// The rows listed by a daemon of `instance_id` whose profiles are `placed`
/// (identifier, source).
async fn listed_rows_of(
    tag: &str,
    placed: &[(&str, String)],
    instance_id: Option<&str>,
) -> Vec<McpRegistrationRow> {
    let root = unique_temp_dir(tag);
    let profiles_dir = root.join("profiles");
    let home_dir = root.join("home");
    let report_dir: PathBuf = root.join("state").join("bin");
    for dir in [&profiles_dir, &home_dir] {
        std::fs::create_dir_all(dir).expect("create fixture dir");
    }
    for (id, source) in placed {
        write_profile(&profiles_dir, id, source);
    }
    let config = DaemonConfig {
        profiles_dir: Some(profiles_dir),
        home_dir: Some(home_dir),
        registration_cwd: registration_cwd_in(&root),
        instance_id: instance_id.map(str::to_string),
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
    match read_response(&mut client).await {
        DaemonMessage::McpRegistrationsListed { rows, .. } => rows,
        other => panic!("expected mcp_registrations_listed, got {other:?}"),
    }
}

fn row<'a>(rows: &'a [McpRegistrationRow], profile: &str) -> &'a McpRegistrationRow {
    rows.iter()
        .find(|row| row.profile == profile)
        .unwrap_or_else(|| panic!("the listing has no row for {profile}"))
}

/// A row whose registration owns one key, named after its server, of a JSON file it
/// shares with the user's own entries previews that one key under the name the
/// instance registers as.
#[tokio::test]
async fn a_shared_file_row_previews_the_one_key_it_will_add_under_its_server_name() {
    const FIXTURE: &str = "json-entry-shared-file";
    for (instance_id, server) in [(None, "yagura"), (Some("dev"), "yagura-dev")] {
        let placed = [(FIXTURE, fixture_profile_with_stub_command(FIXTURE))];
        let rows = listed_rows_of("hook-preview-shared-file", &placed, instance_id).await;
        let entries = &row(&rows, FIXTURE).entries;

        assert_eq!(
            entries.len(),
            1,
            "{server}: one top-level entry is written: {entries:?}"
        );
        let entry = &entries[0];
        assert_eq!(entry.file, ".example-cli/hooks.json", "{server}");
        assert_eq!(entry.name, server, "the key is the server's own name");
        let content: serde_json::Value =
            serde_json::from_str(&entry.content).expect("the preview content is the entry's JSON");
        assert!(
            content.get("BeforeTurn").is_some() && content.get("AfterTool").is_some(),
            "{server}: the preview must show the events the entry reports on: {content}"
        );
        assert!(
            entry.content.contains(&format!(
                "--for-instance {server} --hook {FIXTURE} BeforeTurn"
            )),
            "{server}: the preview must show the command the hook runs: {}",
            entry.content
        );
    }
}

#[tokio::test]
async fn the_codex_row_previews_the_trust_entries_next_to_its_settings_preview() {
    let rows = listed_rows("hook-preview-codex", &["codex-cli"]).await;
    let codex = row(&rows, "codex-cli");

    let mut events: Vec<&str> = codex
        .entries
        .iter()
        .map(|entry: &McpRegistrationEntry| {
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
        row(&rows, "claude-code").entries.is_empty(),
        "a profile that injects its hooks at launch has nothing to preview"
    );
}
