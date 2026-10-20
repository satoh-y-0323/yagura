#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// Seeds and reads the database with raw SQL, which only tests may do.
#![allow(clippy::disallowed_methods)]

//! Integration tests: the daemon injects the MCP channel into a window's
//! environment and, through a launch profile's `[mcp]` table, into its argv
//! and a settings file on disk -- across every path that opens a
//! profile-addressed window -- and never lets that injection leak into the
//! session snapshot or the child's argv as the window's own status token.
//!
//! Every property here is measured through a real spawned child
//! (`argv_probe` / `env_probe`), never a fake transport: the injection is
//! wired into `WindowRegistry`'s real spawn argv and environment.
//! `YAGURA_INSTANCE`'s absence from a child's environment is covered by
//! `tests/child_env.rs`, which checks the composed environment directly and
//! so does not depend on this test process's own environment.

mod common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use common::{
    Harness, VALID_TOKEN, call_open_window, call_send_handoff_from_profile, detection_config,
    empty_lookup, expect_handoff_sent, identify, policy, read_response, send, write_profile,
};
use yagura_daemon::DaemonConfig;
use yagura_daemon::profile::ProfileOs;
use yagura_daemon::store;
use yagura_protocol::{ClientMessage, DaemonMessage, HandoffDelivery, PlanInfo, PlanTaskSpec};
use yagura_test_support::ipc::Connection;
use yagura_test_support::paths::TempDir;
use yagura_test_support::process::{argv_probe_path, argv_probe_profile_source, env_probe_path};

// --- fixtures --------------------------------------------------------------

/// A profile's `[mcp]` table using the Windows-hosted Claude Code shape
/// -- a settings file plus a
/// `{mcp_config_file}` argument.
fn mcp_config_file_table(config_file_json: &str) -> String {
    format!(
        "\n[mcp]\nconfig_file = '{config_file_json}'\nargs = [\"--mcp-config\", \
         \"{{mcp_config_file}}\"]\n"
    )
}

/// The fixed settings-file content every `mcp_config_file_table` test writes:
/// no placeholder of its own, so its round trip through disk is a literal
/// comparison.
const MCP_CONFIG_FILE_JSON: &str =
    "{\"mcpServers\":{\"yagura\":{\"command\":\"${YAGURA_MCP_BRIDGE}\",\"args\":[]}}}";

/// A profile's `[mcp]` table using the Codex CLI shape: the bridge path and
/// the channel environment-variable names rendered directly into argv, with
/// no settings file.
const MCP_BRIDGE_TOML_TABLE: &str = "\n[mcp]\nargs = [\"-c\", \
     \"mcp_servers.yagura.command={bridge_toml}\", \"-c\", \
     \"mcp_servers.yagura.env_vars={env_names_toml}\"]\n";

/// A profile's `[model]` / `[effort]` tables, mapping both axes: the same
/// shape `ipc_profile.rs`'s `BOTH_AXES_TABLES` uses, restated here because
/// integration-test binaries cannot import each other's private items.
const LAUNCH_OPTIONS_TABLE: &str = "\n[model]\nargs = [\"--tier\", \"{value}\"]\n\n\
     [model.aliases]\nalpha = \"tier-a\"\n\n[effort]\nargs = [\"-c\", \"tier={value}\"]\n";

/// The TOML source of a profile whose `[command]` launches `env_probe
/// <var_name>`, followed verbatim by `env_table` (the profile's own `[env]`
/// table, or `""` for none).
fn env_probe_profile_source(display_name: &str, var_name: &str, env_table: &str) -> String {
    let program = env_probe_path()
        .to_str()
        .expect("env_probe path must be valid UTF-8")
        .to_string();
    format!(
        "schema = 1\ndisplay_name = \"{display_name}\"\n\n[command]\ndefault = ['{program}', \
         '{var_name}']\n{env_table}"
    )
}

/// The profile's own `WSLENV` base a merge test checks survives the daemon's
/// own additions.
const WSLENV_BASE_TABLE: &str = "\n[env]\nWSLENV = \"FOO/l\"\n";

fn probe_is_executable(path: &Path) -> bool {
    path.is_file()
}

/// A `DaemonConfig` resolving profiles from `profiles_dir`, with the two
/// fields the daemon's MCP-channel injection reads: `mcp_bridge` (the
/// resolved bridge executable, in its external form) and `mcp_config_dir`
/// (where a profile's settings file is written).
fn mcp_daemon_config(
    profiles_dir: PathBuf,
    mcp_config_dir: &Path,
    bridge: Option<PathBuf>,
) -> DaemonConfig {
    DaemonConfig {
        profiles_dir: Some(profiles_dir),
        host_os: ProfileOs::host(),
        program_lookup: empty_lookup(),
        program_is_executable: probe_is_executable,
        default_shell: None,
        mcp_bridge: bridge,
        mcp_config_dir: Some(mcp_config_dir.to_path_buf()),
        ..common::approver_base_config()
    }
}

// --- IPC round trips ---------------------------------------------------------

async fn call_open_window_from_profile(client: &mut Connection, profile: &str) -> DaemonMessage {
    call_open_window_from_profile_with_options(client, profile, None, None).await
}

async fn call_open_window_from_profile_with_options(
    client: &mut Connection,
    profile: &str,
    model: Option<&str>,
    effort: Option<&str>,
) -> DaemonMessage {
    send(
        client,
        &ClientMessage::OpenWindowFromProfile {
            profile: profile.to_string(),
            cwd: None,
            name: None,
            model: model.map(str::to_string),
            effort: effort.map(str::to_string),
            on_name_taken: Default::default(),
        },
    )
    .await;
    read_response(client, Duration::from_secs(5)).await
}

fn expect_window_opened(msg: DaemonMessage) -> String {
    match msg {
        DaemonMessage::WindowOpened { window_id, .. } => window_id,
        other => panic!("expected window_opened, got {other:?}"),
    }
}

/// Extracts the value of the last `<<name=value>>` sentinel line in `text`
/// (an `argv_probe` / `env_probe` report), or `None` if absent. Last, not
/// first: a resync can repaint an already-written line, and the repaint is
/// always identical, so this is only ever a robustness margin.
fn extract_sentinel_value(text: &str, name: &str) -> Option<String> {
    let prefix = format!("<<{name}=");
    let start = text.rfind(&prefix)? + prefix.len();
    let end = text[start..].find(">>")? + start;
    Some(text[start..end].to_string())
}

// --- plan ledger round trips (open_task_window path) ------------------------

async fn read_ledger_reply(client: &mut Connection, timeout: Duration) -> DaemonMessage {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(!remaining.is_zero(), "timed out waiting for a reply");
        let frame = read_response(client, remaining).await;
        if !matches!(frame, DaemonMessage::PlanChanged { .. }) {
            return frame;
        }
    }
}

fn task_with_profile(task_id: &str, profile: &str) -> PlanTaskSpec {
    PlanTaskSpec {
        task_id: task_id.to_string(),
        title: "do the thing".to_string(),
        role: None,
        profile: Some(profile.to_string()),
        model: None,
        effort: None,
        depends_on: Vec::new(),
        writes: vec![format!("{task_id}.txt")],
        instruction: None,
    }
}

async fn create_plan_ok(
    client: &mut Connection,
    title: &str,
    tasks: Vec<PlanTaskSpec>,
) -> PlanInfo {
    send(
        client,
        &ClientMessage::CreatePlan {
            title: title.to_string(),
            tasks,
        },
    )
    .await;
    match read_ledger_reply(client, Duration::from_secs(5)).await {
        DaemonMessage::PlanDetail { plan } => plan,
        other => panic!("expected plan_detail, got {other:?}"),
    }
}

async fn open_task_window(client: &mut Connection, plan_id: &str, task_id: &str) -> DaemonMessage {
    send(
        client,
        &ClientMessage::OpenTaskWindow {
            plan_id: plan_id.to_string(),
            task_id: task_id.to_string(),
            name: None,
        },
    )
    .await;
    read_ledger_reply(client, Duration::from_secs(10)).await
}

fn expect_task_window_opened(msg: DaemonMessage) -> String {
    match msg {
        DaemonMessage::TaskWindowOpened { window_id, .. } => window_id,
        other => panic!("expected task_window_opened, got {other:?}"),
    }
}

// --- Session-snapshot polling -------------------------------

/// Polls the current slot until `predicate` holds, then returns the rows
/// that satisfied it. Duplicated locally the way `ipc_restore.rs` and
/// `store_snapshot.rs` each keep their own copy: an integration-test binary
/// cannot import another's private items.
async fn wait_for_current_snapshot(
    db_path: &Path,
    label: &str,
    predicate: impl Fn(&[store::SnapshotWindow]) -> bool,
) -> Vec<store::SnapshotWindow> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut last: Option<Vec<store::SnapshotWindow>> = None;
    loop {
        if let Ok(conn) = store::open_readonly(db_path)
            && let Ok(rows) = store::read_snapshot_conn(&conn)
        {
            if predicate(&rows) {
                return rows;
            }
            last = Some(rows);
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("timed out waiting for {label}; last observed snapshot: {last:?}");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

// =============================================================================
// The child's environment carries YAGURA_MCP_BRIDGE exactly when a
// bridge is configured
// =============================================================================

#[tokio::test]
async fn open_window_from_profile_injects_the_mcp_bridge_path_when_a_bridge_is_configured() {
    let tree = TempDir::new("yagura-cli-test", "mcp-inject-bridge-present");
    let profiles_dir = tree.make_dir("profiles");
    let mcp_dir = tree.make_dir("mcp");
    write_profile(
        &profiles_dir,
        "env-probe",
        &env_probe_profile_source("Env Probe", yagura_protocol::MCP_BRIDGE_ENV, ""),
    );
    let bridge = tree.make_dir("bridge").join("yagura-mcp.exe");
    let config = mcp_daemon_config(profiles_dir, &mcp_dir, Some(bridge.clone()));
    let h = Harness::start("mcp-inject-bridge-present", config).await;
    let mut client = h.connect().await;

    let window_id =
        expect_window_opened(call_open_window_from_profile(&mut client, "env-probe").await);
    let needle = format!(
        "<<{}={}>>",
        yagura_protocol::MCP_BRIDGE_ENV,
        bridge.display()
    );
    h.wait_for_scrollback(&window_id, needle.as_bytes(), Duration::from_secs(10))
        .await;
}

#[tokio::test]
async fn open_window_from_profile_leaves_the_mcp_bridge_variable_unset_when_no_bridge_is_configured()
 {
    let tree = TempDir::new("yagura-cli-test", "mcp-inject-bridge-absent");
    let profiles_dir = tree.make_dir("profiles");
    let mcp_dir = tree.make_dir("mcp");
    write_profile(
        &profiles_dir,
        "env-probe",
        &env_probe_profile_source("Env Probe", yagura_protocol::MCP_BRIDGE_ENV, ""),
    );
    let config = mcp_daemon_config(profiles_dir, &mcp_dir, None);
    let h = Harness::start("mcp-inject-bridge-absent", config).await;
    let mut client = h.connect().await;

    let window_id =
        expect_window_opened(call_open_window_from_profile(&mut client, "env-probe").await);
    let needle = format!("<<{} unset>>", yagura_protocol::MCP_BRIDGE_ENV);
    h.wait_for_scrollback(&window_id, needle.as_bytes(), Duration::from_secs(10))
        .await;
}

// =============================================================================
// On Windows, the child's WSLENV gains the MCP-channel names on top of
// the profile's own base; on Unix the daemon never touches WSLENV
// =============================================================================

#[cfg(windows)]
#[tokio::test]
async fn open_window_from_profile_extends_the_childs_wslenv_with_the_mcp_channel_names_on_windows()
{
    let tree = TempDir::new("yagura-cli-test", "mcp-inject-wslenv-with-bridge");
    let profiles_dir = tree.make_dir("profiles");
    let mcp_dir = tree.make_dir("mcp");
    write_profile(
        &profiles_dir,
        "env-probe",
        &env_probe_profile_source("Env Probe", "WSLENV", WSLENV_BASE_TABLE),
    );
    let bridge = tree.make_dir("bridge").join("yagura-mcp.exe");
    let config = mcp_daemon_config(profiles_dir, &mcp_dir, Some(bridge));
    let h = Harness::start("mcp-inject-wslenv-with-bridge", config).await;
    let mut client = h.connect().await;

    let window_id =
        expect_window_opened(call_open_window_from_profile(&mut client, "env-probe").await);
    h.wait_for_scrollback(&window_id, b"<<WSLENV=", Duration::from_secs(10))
        .await;
    let text = String::from_utf8_lossy(&h.scrollback(&window_id)).into_owned();
    let value = extract_sentinel_value(&text, "WSLENV")
        .expect("the child must report a WSLENV value, not unset");
    for expected in [
        "FOO/l",
        "YAGURA_WINDOW_ID",
        "YAGURA_STATUS_TOKEN",
        "YAGURA_MCP_BRIDGE/p",
    ] {
        assert!(
            value.contains(expected),
            "the child's WSLENV ({value:?}) must contain {expected:?}"
        );
    }
}

#[cfg(windows)]
#[tokio::test]
async fn open_window_from_profile_extends_wslenv_without_the_bridge_flag_when_no_bridge_is_configured()
 {
    let tree = TempDir::new("yagura-cli-test", "mcp-inject-wslenv-no-bridge");
    let profiles_dir = tree.make_dir("profiles");
    let mcp_dir = tree.make_dir("mcp");
    write_profile(
        &profiles_dir,
        "env-probe",
        &env_probe_profile_source("Env Probe", "WSLENV", WSLENV_BASE_TABLE),
    );
    let config = mcp_daemon_config(profiles_dir, &mcp_dir, None);
    let h = Harness::start("mcp-inject-wslenv-no-bridge", config).await;
    let mut client = h.connect().await;

    let window_id =
        expect_window_opened(call_open_window_from_profile(&mut client, "env-probe").await);
    h.wait_for_scrollback(&window_id, b"<<WSLENV=", Duration::from_secs(10))
        .await;
    let text = String::from_utf8_lossy(&h.scrollback(&window_id)).into_owned();
    let value = extract_sentinel_value(&text, "WSLENV")
        .expect("the child must report a WSLENV value, not unset");
    for expected in ["FOO/l", "YAGURA_WINDOW_ID", "YAGURA_STATUS_TOKEN"] {
        assert!(
            value.contains(expected),
            "the child's WSLENV ({value:?}) must contain {expected:?}"
        );
    }
    assert!(
        !value.contains("YAGURA_MCP_BRIDGE"),
        "with no bridge configured, WSLENV ({value:?}) must not carry the bridge flag"
    );
}

/// Compiled only on this Windows development machine, never executed here:
/// the property it measures only applies to a Unix host. Kept so the
/// contract exists once Unix rejoins the CI matrix (`coding-standards.md`'s
/// OS-matrix note).
#[cfg(unix)]
#[tokio::test]
async fn open_window_from_profile_does_not_touch_wslenv_on_unix() {
    let tree = TempDir::new("yagura-cli-test", "mcp-inject-wslenv-unix");
    let profiles_dir = tree.make_dir("profiles");
    let mcp_dir = tree.make_dir("mcp");
    write_profile(
        &profiles_dir,
        "env-probe",
        &env_probe_profile_source("Env Probe", "WSLENV", WSLENV_BASE_TABLE),
    );
    let bridge = tree.make_dir("bridge").join("yagura-mcp");
    let config = mcp_daemon_config(profiles_dir, &mcp_dir, Some(bridge));
    let h = Harness::start("mcp-inject-wslenv-unix", config).await;
    let mut client = h.connect().await;

    let window_id =
        expect_window_opened(call_open_window_from_profile(&mut client, "env-probe").await);
    h.wait_for_scrollback(&window_id, b"<<WSLENV=FOO/l>>", Duration::from_secs(10))
        .await;
}

// =============================================================================
// The `[mcp]` argv injection reaches the real spawn argv on
// every path that opens a profile, a settings file is written where
// promised and never carries the window's status token, and neither does
// any argv element
// =============================================================================

#[tokio::test]
async fn open_window_from_profile_appends_the_rendered_mcp_config_file_argument_to_the_childs_argv()
{
    let tree = TempDir::new("yagura-cli-test", "mcp-inject-config-file");
    let profiles_dir = tree.make_dir("profiles");
    let mcp_dir = tree.make_dir("mcp");
    write_profile(
        &profiles_dir,
        "argv-probe",
        &argv_probe_profile_source("Argv Probe", &mcp_config_file_table(MCP_CONFIG_FILE_JSON)),
    );
    let bridge = tree.make_dir("bridge").join("yagura-mcp.exe");
    let config = mcp_daemon_config(profiles_dir, &mcp_dir, Some(bridge));
    let h = Harness::start("mcp-inject-config-file", config).await;
    let mut client = h.connect().await;

    let window_id =
        expect_window_opened(call_open_window_from_profile(&mut client, "argv-probe").await);
    h.wait_for_scrollback(&window_id, b"<<argv[2]=", Duration::from_secs(10))
        .await;
    let text = String::from_utf8_lossy(&h.scrollback(&window_id)).into_owned();
    assert!(
        text.contains("<<argv[1]=--mcp-config>>"),
        "the profile's own args element must reach the child unchanged; scrollback: {text:?}"
    );
    let config_file_path = extract_sentinel_value(&text, "argv[2]")
        .expect("argv[2] must report the rendered settings-file path");
    let config_file_path = PathBuf::from(config_file_path);
    assert!(
        config_file_path.starts_with(&mcp_dir),
        "the settings file must live under DaemonConfig::mcp_config_dir ({mcp_dir:?}); got \
         {config_file_path:?}"
    );
    let written = std::fs::read_to_string(&config_file_path)
        .unwrap_or_else(|err| panic!("the rendered settings file must exist: {err}"));
    assert_eq!(
        written, MCP_CONFIG_FILE_JSON,
        "the written settings file must carry the profile's config_file verbatim"
    );
    assert!(
        !written.contains(VALID_TOKEN),
        "the settings file must never carry the window's status token"
    );
    assert!(
        !text.contains(VALID_TOKEN),
        "the child's argv must never carry the window's status token"
    );
}

#[tokio::test]
async fn open_window_from_profile_renders_the_bridge_toml_and_env_names_placeholders_into_argv() {
    let tree = TempDir::new("yagura-cli-test", "mcp-inject-bridge-toml");
    let profiles_dir = tree.make_dir("profiles");
    let mcp_dir = tree.make_dir("mcp");
    write_profile(
        &profiles_dir,
        "argv-probe",
        &argv_probe_profile_source("Argv Probe", MCP_BRIDGE_TOML_TABLE),
    );
    let bridge = tree.make_dir("bridge").join("yagura-mcp.exe");
    let config = mcp_daemon_config(profiles_dir, &mcp_dir, Some(bridge.clone()));
    let h = Harness::start("mcp-inject-bridge-toml", config).await;
    let mut client = h.connect().await;

    let window_id =
        expect_window_opened(call_open_window_from_profile(&mut client, "argv-probe").await);
    h.wait_for_scrollback(&window_id, b"<<argv[4]=", Duration::from_secs(10))
        .await;
    let text = String::from_utf8_lossy(&h.scrollback(&window_id)).into_owned();

    let command_arg = extract_sentinel_value(&text, "argv[2]")
        .expect("argv[2] must report the rendered {bridge_toml} element");
    assert!(
        command_arg.contains(&bridge.display().to_string()),
        "argv[2] ({command_arg:?}) must render the bridge path as a TOML literal string"
    );

    let env_vars_arg = extract_sentinel_value(&text, "argv[4]")
        .expect("argv[4] must report the rendered {env_names_toml} element");
    for name in [
        yagura_protocol::WINDOW_ID_ENV,
        yagura_protocol::STATUS_TOKEN_ENV,
        yagura_protocol::MCP_BRIDGE_ENV,
    ] {
        assert!(
            env_vars_arg.contains(name),
            "argv[4] ({env_vars_arg:?}) must list {name:?}"
        );
    }

    assert!(
        !text.contains(VALID_TOKEN),
        "the child's argv must never carry the window's status token, only its name"
    );
}

#[tokio::test]
async fn open_window_from_profile_names_the_injected_mcp_server_after_the_named_instances_registration()
 {
    let tree = TempDir::new("yagura-cli-test", "mcp-inject-server-name");
    let profiles_dir = tree.make_dir("profiles");
    let mcp_dir = tree.make_dir("mcp");
    let table = "\n[mcp]\nconfig_file = '{\"mcpServers\":{\"{server}\":{\"command\":\"x\"}}}'\n\
                 args = [\"--mcp-config\", \"{mcp_config_file}\", \"-c\", \
                 \"mcp_servers.{server}.command={bridge_toml}\"]\n";
    write_profile(
        &profiles_dir,
        "argv-probe",
        &argv_probe_profile_source("Argv Probe", table),
    );
    let bridge = tree.make_dir("bridge").join("yagura-mcp.exe");
    let mut config = mcp_daemon_config(profiles_dir, &mcp_dir, Some(bridge));
    config.instance_id = Some("t-named".to_string());
    let registered =
        yagura_daemon::mcp_registration::RegistrationTarget::for_instance(Some("t-named"))
            .server()
            .to_string();
    let h = Harness::start("mcp-inject-server-name", config).await;
    let mut client = h.connect().await;

    let window_id =
        expect_window_opened(call_open_window_from_profile(&mut client, "argv-probe").await);
    h.wait_for_scrollback(&window_id, b"<<argv[4]=", Duration::from_secs(10))
        .await;
    let text = String::from_utf8_lossy(&h.scrollback(&window_id)).into_owned();

    let command_arg = extract_sentinel_value(&text, "argv[4]")
        .expect("argv[4] must report the rendered -c element");
    assert!(
        command_arg.starts_with(&format!("mcp_servers.{registered}.command=")),
        "the -c override must name the registered server {registered:?}; got {command_arg:?}"
    );
    let config_file_path = extract_sentinel_value(&text, "argv[2]")
        .expect("argv[2] must report the rendered settings-file path");
    let written = std::fs::read_to_string(&config_file_path)
        .unwrap_or_else(|err| panic!("the rendered settings file must exist: {err}"));
    let json: serde_json::Value = serde_json::from_str(&written)
        .unwrap_or_else(|err| panic!("the settings file must be JSON: {err}; got {written:?}"));
    assert!(
        json["mcpServers"].get(&registered).is_some(),
        "the settings file must key mcpServers by {registered:?}; got {written:?}"
    );
}

#[tokio::test]
async fn open_window_from_profile_without_an_mcp_table_injects_nothing_into_argv() {
    let tree = TempDir::new("yagura-cli-test", "mcp-inject-no-table");
    let profiles_dir = tree.make_dir("profiles");
    let mcp_dir = tree.make_dir("mcp");
    write_profile(
        &profiles_dir,
        "argv-probe",
        &argv_probe_profile_source("Argv Probe", ""),
    );
    let bridge = tree.make_dir("bridge").join("yagura-mcp.exe");
    let config = mcp_daemon_config(profiles_dir, &mcp_dir, Some(bridge));
    let h = Harness::start("mcp-inject-no-table", config).await;
    let mut client = h.connect().await;

    let window_id =
        expect_window_opened(call_open_window_from_profile(&mut client, "argv-probe").await);
    h.wait_until_child_exited(&window_id, Duration::from_secs(10))
        .await;
    let text = String::from_utf8_lossy(&h.scrollback(&window_id)).into_owned();
    assert!(
        !text.contains("<<argv[1]="),
        "a profile with no [mcp] table must launch with no extra argv element; scrollback: \
         {text:?}"
    );
}

#[tokio::test]
async fn open_window_from_profile_injects_nothing_when_no_bridge_is_configured_even_with_an_mcp_table()
 {
    let tree = TempDir::new("yagura-cli-test", "mcp-inject-no-bridge-argv");
    let profiles_dir = tree.make_dir("profiles");
    let mcp_dir = tree.make_dir("mcp");
    write_profile(
        &profiles_dir,
        "argv-probe",
        &argv_probe_profile_source("Argv Probe", &mcp_config_file_table(MCP_CONFIG_FILE_JSON)),
    );
    let config = mcp_daemon_config(profiles_dir, &mcp_dir, None);
    let h = Harness::start("mcp-inject-no-bridge-argv", config).await;
    let mut client = h.connect().await;

    let window_id =
        expect_window_opened(call_open_window_from_profile(&mut client, "argv-probe").await);
    h.wait_until_child_exited(&window_id, Duration::from_secs(10))
        .await;
    let text = String::from_utf8_lossy(&h.scrollback(&window_id)).into_owned();
    assert!(
        !text.contains("<<argv[1]="),
        "with no bridge resolved, an [mcp] table must inject nothing; scrollback: {text:?}"
    );
}

#[tokio::test]
async fn send_handoff_from_a_profile_carries_the_same_mcp_injection_into_the_childs_argv() {
    let tree = TempDir::new("yagura-cli-test", "mcp-inject-handoff");
    let profiles_dir = tree.make_dir("profiles");
    let mcp_dir = tree.make_dir("mcp");
    let detection_dir = tree.make_dir("detection");
    write_profile(
        &profiles_dir,
        "argv-probe",
        &argv_probe_profile_source(
            "Argv Probe",
            &format!(
                "{}{}",
                mcp_config_file_table(MCP_CONFIG_FILE_JSON),
                common::LAUNCH_PROMPT_TABLE
            ),
        ),
    );
    let bridge = tree.make_dir("bridge").join("yagura-mcp.exe");
    let mut config = detection_config(
        &detection_dir,
        mcp_daemon_config(profiles_dir, &mcp_dir, Some(bridge)),
    );
    config.approval_policy = Some(policy(&["send_handoff", "open_window"]));
    let h = Harness::start("mcp-inject-handoff", config).await;
    let mut client = h.connect().await;

    let msg = call_send_handoff_from_profile(
        &mut client,
        "argv-probe",
        None,
        None,
        "src/foo.rs",
        "look here",
    )
    .await;
    // The window is started with the envelope, so the handoff is taken at
    // launch; `argv_probe` then writes its argv and exits.
    let (window_id, opened, delivery) = expect_handoff_sent(msg);
    assert!(opened);
    assert_eq!(delivery, HandoffDelivery::Taken);

    h.wait_for_scrollback(&window_id, b"<<argv[2]=", Duration::from_secs(10))
        .await;
    let text = String::from_utf8_lossy(&h.scrollback(&window_id)).into_owned();
    assert!(
        text.contains("<<argv[1]=--mcp-config>>"),
        "a handoff-opened window must carry the same MCP injection; scrollback: {text:?}"
    );
}

#[tokio::test]
async fn open_task_window_carries_the_same_mcp_injection_into_the_childs_argv() {
    let tree = TempDir::new("yagura-cli-test", "mcp-inject-task-window");
    let profiles_dir = tree.make_dir("profiles");
    let mcp_dir = tree.make_dir("mcp");
    write_profile(
        &profiles_dir,
        "argv-probe",
        &argv_probe_profile_source("Argv Probe", &mcp_config_file_table(MCP_CONFIG_FILE_JSON)),
    );
    let bridge = tree.make_dir("bridge").join("yagura-mcp.exe");
    let mut config = mcp_daemon_config(profiles_dir, &mcp_dir, Some(bridge));
    config.store_db_path_resolved = Some(tree.make_dir("store").join("yagura.db"));
    config.roles_path = None;
    // This file measures MCP injection, not the review gate: the plan
    // created here must not start pending.
    config.plan_approval_required = false;
    let (h, _locks) = Harness::start_with_ledger("mcp-inject-task-window", config).await;

    let mut owner = h.connect().await;
    let owner_window_id = h
        .open(yagura_test_support::process::echo_child_command(), None)
        .await;
    identify(&mut owner, &owner_window_id).await;

    let plan = create_plan_ok(
        &mut owner,
        "plan",
        vec![task_with_profile("t1", "argv-probe")],
    )
    .await;
    let window_id =
        expect_task_window_opened(open_task_window(&mut owner, &plan.plan_id, "t1").await);

    h.wait_for_scrollback(&window_id, b"<<argv[2]=", Duration::from_secs(10))
        .await;
    let text = String::from_utf8_lossy(&h.scrollback(&window_id)).into_owned();
    assert!(
        text.contains("<<argv[1]=--mcp-config>>"),
        "a task window must carry the same MCP injection; scrollback: {text:?}"
    );
}

#[tokio::test]
async fn open_window_from_profile_with_mcp_injection_excludes_the_injection_from_the_snapshot_command()
 {
    let tree = TempDir::new("yagura-cli-test", "mcp-inject-snapshot-command");
    let profiles_dir = tree.make_dir("profiles");
    let mcp_dir = tree.make_dir("mcp");
    write_profile(
        &profiles_dir,
        "argv-probe",
        &argv_probe_profile_source("Argv Probe", &mcp_config_file_table(MCP_CONFIG_FILE_JSON)),
    );
    let bridge = tree.make_dir("bridge").join("yagura-mcp.exe");
    let db_path = tree.make_dir("store").join("yagura.db");
    let config = mcp_daemon_config(profiles_dir, &mcp_dir, Some(bridge));
    let h = Harness::start("mcp-inject-snapshot-command", config).await;
    store::wire_into(&h.registry, Some(db_path.clone()));
    let mut client = h.connect().await;

    let window_id =
        expect_window_opened(call_open_window_from_profile(&mut client, "argv-probe").await);
    // Prove the injection did reach the real spawn argv before checking that
    // it never reached the stored one.
    h.wait_for_scrollback(&window_id, b"<<argv[2]=", Duration::from_secs(10))
        .await;

    let rows = wait_for_current_snapshot(
        &db_path,
        "the profile-launched window to be recorded",
        |rows| rows.len() == 1,
    )
    .await;
    assert_eq!(
        rows[0].command,
        vec![
            argv_probe_path()
                .to_str()
                .expect("argv_probe path must be valid UTF-8")
                .to_string()
        ],
        "the stored command must be the profile's own argv, never the injected --mcp-config \
         argument or settings-file path"
    );
}

// =============================================================================
// A profile launch's identity (profile id, model, effort) is stored
// on the window and read back as `None` for a raw OpenWindow, or for a row
// that predates profile tracking
// =============================================================================

#[test]
fn legacy_snapshot_rows_read_profile_model_effort_as_none_after_migrating_forward() {
    let tree = TempDir::new("yagura-cli-test", "mcp-migration-legacy");
    let db_path = tree.make_dir("store").join("yagura.db");
    let mut conn = rusqlite::Connection::open(&db_path).expect("open a fresh sqlite connection");
    // Every migration before the one that adds profile tracking to snapshot rows.
    let before_profile_tracking = &store::MIGRATIONS[..10];
    store::apply_migrations(&mut conn, before_profile_tracking)
        .expect("every migration before profile tracking must apply");
    conn.execute(
        "INSERT INTO snapshot_windows (slot, position, command_json, cwd, name, vendor, \
         detection) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![
            store::SLOT_CURRENT,
            0i64,
            "[\"cmd.exe\"]",
            "C:/work",
            "legacy",
            None::<String>,
            None::<String>,
        ],
    )
    .expect("seed a row written before profile tracking existed");
    store::apply_migrations(&mut conn, store::MIGRATIONS)
        .expect("the full migration series must apply forward from any earlier version");

    let rows = store::read_snapshot_conn(&conn).expect("read_snapshot_conn must succeed");
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].profile, None,
        "a row migrated forward from before profile tracking must read profile as None, never a \
         default value"
    );
    assert_eq!(rows[0].model, None);
    assert_eq!(rows[0].effort, None);
}

#[test]
fn snapshot_rows_written_with_profile_launch_metadata_round_trip_through_read_snapshot_conn() {
    let tree = TempDir::new("yagura-cli-test", "mcp-migration-fresh");
    let db_path = tree.make_dir("store").join("yagura.db");
    store::open_store(&db_path).expect("open_store must create a freshly migrated database");
    let conn = rusqlite::Connection::open(&db_path).expect("open the freshly migrated database");
    conn.execute(
        "INSERT INTO snapshot_windows (slot, position, command_json, cwd, name, vendor, \
         detection, profile, model, effort) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        rusqlite::params![
            store::SLOT_CURRENT,
            0i64,
            "[\"argv_probe\"]",
            "C:/work",
            "with-options",
            Some("with-options"),
            None::<String>,
            Some("with-options"),
            Some("alpha"),
            Some("tier-b"),
        ],
    )
    .expect("seed a row carrying profile launch metadata");

    let rows = store::read_snapshot_conn(&conn).expect("read_snapshot_conn must succeed");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].profile.as_deref(), Some("with-options"));
    assert_eq!(rows[0].model.as_deref(), Some("alpha"));
    assert_eq!(rows[0].effort.as_deref(), Some("tier-b"));
}

#[tokio::test]
async fn open_window_from_profile_with_launch_options_persists_profile_model_and_effort_into_the_current_snapshot_row_while_a_raw_open_window_leaves_them_none()
 {
    let tree = TempDir::new("yagura-cli-test", "mcp-inject-snapshot-metadata");
    let profiles_dir = tree.make_dir("profiles");
    let mcp_dir = tree.make_dir("mcp");
    write_profile(
        &profiles_dir,
        "with-options",
        &argv_probe_profile_source("With Options", LAUNCH_OPTIONS_TABLE),
    );
    let db_path = tree.make_dir("store").join("yagura.db");
    let config = mcp_daemon_config(profiles_dir, &mcp_dir, None);
    let h = Harness::start("mcp-inject-snapshot-metadata", config).await;
    store::wire_into(&h.registry, Some(db_path.clone()));
    let mut client = h.connect().await;

    expect_window_opened(
        call_open_window_from_profile_with_options(
            &mut client,
            "with-options",
            Some("alpha"),
            Some("tier-b"),
        )
        .await,
    );
    let _raw_window_id = call_open_window(
        &mut client,
        yagura_test_support::process::echo_child_command(),
    )
    .await;

    let rows = wait_for_current_snapshot(&db_path, "both windows to be recorded", |rows| {
        rows.len() == 2
    })
    .await;

    let profiled = rows
        .iter()
        .find(|row| row.name == "With Options")
        .expect("the profile-launched row must be present");
    assert_eq!(profiled.profile.as_deref(), Some("with-options"));
    assert_eq!(profiled.model.as_deref(), Some("alpha"));
    assert_eq!(profiled.effort.as_deref(), Some("tier-b"));

    let raw = rows
        .iter()
        .find(|row| row.name != "With Options")
        .expect("the raw OpenWindow row must be present");
    assert_eq!(
        raw.profile, None,
        "a raw OpenWindow row must never carry a profile identity"
    );
    assert_eq!(raw.model, None);
    assert_eq!(raw.effort, None);
}
