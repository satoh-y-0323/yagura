#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration tests: a `json_entry` file named by `{server}`.
//!
//! A CLI whose hook files have no per-name container (one file per
//! registration) needs one file per instance: instances sharing one file
//! would overwrite each other's entry. `file` may therefore carry `{server}`,
//! and such an entry belongs to its instance without `{server}` in the key.
//! Each case runs a stubbed row over a temporary `home_dir`.

mod common;

use std::path::PathBuf;
use std::time::Duration;

use common::{
    Harness, approver_base_config, mcp_cli_stub_profile_with_table, read_response,
    register_approver, registration_cwd_in, send, unique_temp_dir, write_profile,
};
use serde_json::{Value, json};
use yagura_daemon::DaemonConfig;
use yagura_protocol::{
    ClientMessage, DaemonMessage, McpRegistrationAction, McpRegistrationState,
    McpRegistrationStatusInfo,
};
use yagura_test_support::ipc::Connection;

const REPLY_TIMEOUT: Duration = Duration::from_secs(20);

const SETTINGS_FILE: &str = ".neutral/settings.json";
const DEFAULT_HOOKS_FILE: &str = ".neutral/hooks/yagura.json";
const DEV_HOOKS_FILE: &str = ".neutral/hooks/yagura-dev.json";

/// The bridge entry lives in a shared settings file under an instance's own
/// key; the hooks live in a file of their own whose name carries `{server}`.
fn table_with_hooks_file(hooks_file: &str) -> String {
    format!(
        "[mcp_registration.status]\n\
         file = \"{SETTINGS_FILE}\"\n\
         format = \"json\"\n\
         command = \"/mcpServers/{{server}}/command\"\n\
         \n\
         [[mcp_registration.json_entry]]\n\
         file = \"{SETTINGS_FILE}\"\n\
         [mcp_registration.json_entry.entries]\n\
         \"/mcpServers/{{server}}\" = {{ command = \"{{bridge}}\", args = [], env = \"{{env_refs}}\" }}\n\
         \n\
         [[mcp_registration.json_entry]]\n\
         file = \"{hooks_file}\"\n\
         [mcp_registration.json_entry.entries]\n\
         \"/hooks\" = {{ Stop = [{{ type = \"command\", command = \"run-hook\" }}] }}\n"
    )
}

struct Layout {
    profiles_dir: PathBuf,
    home_dir: PathBuf,
    bridge: PathBuf,
}

impl Layout {
    fn new(tag: &str) -> Self {
        let root = unique_temp_dir(tag);
        let layout = Self {
            profiles_dir: root.join("profiles"),
            home_dir: root.join("home"),
            bridge: root.join("bridge").join(if cfg!(windows) {
                "yagura-mcp.exe"
            } else {
                "yagura-mcp"
            }),
        };
        std::fs::create_dir_all(&layout.profiles_dir).expect("create profiles dir");
        std::fs::create_dir_all(&layout.home_dir).expect("create home dir");
        layout
    }

    fn config(&self, instance_id: Option<&str>) -> DaemonConfig {
        DaemonConfig {
            profiles_dir: Some(self.profiles_dir.clone()),
            home_dir: Some(self.home_dir.clone()),
            registration_cwd: registration_cwd_in(
                self.home_dir.parent().expect("home has a parent"),
            ),
            instance_id: instance_id.map(str::to_string),
            mcp_bridge: Some(self.bridge.clone()),
            mcp_registration_timeout: Duration::from_secs(20),
            mcp_registration_max_output_bytes: 1 << 20,
            mcp_registration_status_max_concurrent: 2,
            ..approver_base_config()
        }
    }

    fn home_file(&self, file: &str) -> PathBuf {
        self.home_dir.join(file)
    }

    fn read_json(&self, file: &str) -> Option<Value> {
        let text = std::fs::read_to_string(self.home_file(file)).ok()?;
        Some(serde_json::from_str(&text).unwrap_or_else(|err| panic!("{file}: {err}: {text}")))
    }

    fn write_row(&self, id: &str, table: &str) {
        write_profile(
            &self.profiles_dir,
            id,
            &mcp_cli_stub_profile_with_table(id, "", table, &[]),
        );
    }
}

async fn approver_connection(h: &Harness) -> Connection {
    let mut client = h.connect().await;
    register_approver(&mut client).await;
    client
}

/// The profile ids the daemon lists as registrable, and the status pushes
/// that follow the listing (drained so the connection stays in step).
async fn list_rows(client: &mut Connection, request_id: u64) -> Vec<(String, Vec<String>)> {
    send(client, &ClientMessage::ListMcpRegistrations { request_id }).await;
    let rows = match read_response(client, REPLY_TIMEOUT).await {
        DaemonMessage::McpRegistrationsListed { rows, .. } => rows,
        other => panic!("expected mcp_registrations_listed, got {other:?}"),
    };
    for _ in &rows {
        match read_response(client, REPLY_TIMEOUT).await {
            DaemonMessage::McpRegistrationStatus { .. } => {}
            other => panic!("expected mcp_registration_status, got {other:?}"),
        }
    }
    rows.into_iter()
        .map(|row| {
            let files = row.entries.into_iter().map(|entry| entry.file).collect();
            (row.profile, files)
        })
        .collect()
}

async fn status_of(
    client: &mut Connection,
    profile: &str,
    request_id: u64,
) -> McpRegistrationStatusInfo {
    send(client, &ClientMessage::ListMcpRegistrations { request_id }).await;
    let rows = match read_response(client, REPLY_TIMEOUT).await {
        DaemonMessage::McpRegistrationsListed { rows, .. } => rows.len(),
        other => panic!("expected mcp_registrations_listed, got {other:?}"),
    };
    let mut found = None;
    for _ in 0..rows {
        match read_response(client, REPLY_TIMEOUT).await {
            DaemonMessage::McpRegistrationStatus { status, .. } if status.profile == profile => {
                found = Some(status);
            }
            DaemonMessage::McpRegistrationStatus { .. } => {}
            other => panic!("expected mcp_registration_status, got {other:?}"),
        }
    }
    found.unwrap_or_else(|| panic!("no status was pushed for {profile}"))
}

async fn run(
    client: &mut Connection,
    profile: &str,
    action: McpRegistrationAction,
    request_id: u64,
) -> (Option<String>, McpRegistrationStatusInfo) {
    send(
        client,
        &ClientMessage::RunMcpRegistration {
            profile: profile.to_string(),
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
        DaemonMessage::McpRegistrationResult { error, status, .. } => (error, status),
        other => panic!("expected mcp_registration_result, got {other:?}"),
    }
}

/// A control row that is valid today, so a row missing from the listing is the
/// row's own doing and not a harness that listed nothing.
const CONTROL_TABLE: &str = "[mcp_registration.status]\n\
    file = \".neutral/control.json\"\n\
    format = \"json\"\n\
    command = \"/mcpServers/{server}/command\"\n\
    \n\
    [mcp_registration.json_entry]\n\
    file = \".neutral/control.json\"\n\
    [mcp_registration.json_entry.entries]\n\
    \"/mcpServers/{server}\" = { command = \"{bridge}\", args = [], env = \"{env_refs}\" }\n";

#[tokio::test]
async fn a_server_named_file_with_an_unkeyed_entry_is_accepted_and_listed_by_its_own_name() {
    let layout = Layout::new("mcpreg-server-file-accept");
    layout.write_row("control", CONTROL_TABLE);
    layout.write_row(
        "hooked",
        &table_with_hooks_file(".neutral/hooks/{server}.json"),
    );
    let h = Harness::start("mcpreg-server-file-accept", layout.config(None)).await;
    let mut approver = approver_connection(&h).await;

    let rows = list_rows(&mut approver, 1).await;

    assert!(rows.iter().any(|(id, _)| id == "control"), "{rows:?}");
    let (_, files) = rows
        .iter()
        .find(|(id, _)| id == "hooked")
        .unwrap_or_else(|| {
            panic!("a {{server}} file is a per-instance entry, not a shared one: {rows:?}")
        });
    assert!(
        files.iter().any(|file| file == DEFAULT_HOOKS_FILE),
        "the row shows the file this instance writes, not the template: {files:?}"
    );
    assert!(
        files.iter().all(|file| !file.contains("{server}")),
        "{files:?}"
    );
}

#[tokio::test]
async fn two_instances_write_separate_hook_files_and_removing_one_leaves_the_other_registered() {
    let layout = Layout::new("mcpreg-server-file-two");
    layout.write_row(
        "hooked",
        &table_with_hooks_file(".neutral/hooks/{server}.json"),
    );
    let default_daemon =
        Harness::start("mcpreg-server-file-two-default", layout.config(None)).await;
    let dev_daemon = Harness::start("mcpreg-server-file-two-dev", layout.config(Some("dev"))).await;
    let mut default_approver = approver_connection(&default_daemon).await;
    let mut dev_approver = approver_connection(&dev_daemon).await;

    for (approver, context) in [
        (&mut default_approver, "default"),
        (&mut dev_approver, "dev"),
    ] {
        let (error, status) = run(approver, "hooked", McpRegistrationAction::Register, 1).await;
        assert_eq!(error, None, "{context}");
        assert_eq!(
            status.state,
            McpRegistrationState::Registered,
            "{context}: {status:?}"
        );
    }
    let default_file = layout.read_json(DEFAULT_HOOKS_FILE);
    let dev_file = layout.read_json(DEV_HOOKS_FILE);
    assert!(
        default_file
            .as_ref()
            .is_some_and(|file| file.pointer("/hooks").is_some()),
        "the default instance's own file: {default_file:?}"
    );
    assert!(
        dev_file
            .as_ref()
            .is_some_and(|file| file.pointer("/hooks").is_some()),
        "the dev instance's own file: {dev_file:?}"
    );
    let default_before = std::fs::read(layout.home_file(DEFAULT_HOOKS_FILE)).expect("read");

    let (error, status) = run(
        &mut dev_approver,
        "hooked",
        McpRegistrationAction::Remove,
        2,
    )
    .await;
    assert_eq!(error, None);
    assert_eq!(
        status.state,
        McpRegistrationState::NotRegistered,
        "{status:?}"
    );

    assert_eq!(
        std::fs::read(layout.home_file(DEFAULT_HOOKS_FILE)).expect("read"),
        default_before,
        "removing dev's registration leaves the default instance's file byte for byte"
    );
    assert_eq!(
        layout
            .read_json(DEV_HOOKS_FILE)
            .and_then(|file| file.pointer("/hooks").cloned()),
        None,
        "dev's own entry is gone"
    );
    let default_status = status_of(&mut default_approver, "hooked", 3).await;
    assert_eq!(
        default_status.state,
        McpRegistrationState::Registered,
        "{default_status:?}"
    );
    assert_eq!(
        layout
            .read_json(SETTINGS_FILE)
            .and_then(|file| file.pointer("/mcpServers/yagura").cloned())
            .map(|entry| entry.get("command").cloned()),
        Some(Some(json!(layout.bridge.to_str().expect("UTF-8")))),
        "the default instance's bridge entry stays"
    );
}

#[tokio::test]
async fn a_file_with_a_placeholder_other_than_server_is_not_accepted() {
    let layout = Layout::new("mcpreg-server-file-reject");
    layout.write_row("control", CONTROL_TABLE);
    layout.write_row(
        "valid",
        &table_with_hooks_file(".neutral/hooks/{server}.json"),
    );
    layout.write_row(
        "daemon",
        &table_with_hooks_file(".neutral/hooks/{daemon}.json"),
    );
    layout.write_row(
        "bridge",
        &table_with_hooks_file(".neutral/hooks/{bridge}.json"),
    );
    let h = Harness::start("mcpreg-server-file-reject", layout.config(None)).await;
    let mut approver = approver_connection(&h).await;

    let rows = list_rows(&mut approver, 1).await;

    let ids: Vec<&str> = rows.iter().map(|(id, _)| id.as_str()).collect();
    assert!(ids.contains(&"control"), "{ids:?}");
    assert!(
        ids.contains(&"valid"),
        "the {{server}} file is accepted: {ids:?}"
    );
    assert!(
        !ids.contains(&"daemon") && !ids.contains(&"bridge"),
        "only {{server}} may stand in a file name: {ids:?}"
    );
}
