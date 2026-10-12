#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: the daemon IPC surface that lists each profile's MCP
//! registration row and pushes the state read from the profile's own CLI --
//! `list_mcp_registrations`, answered by `mcp_registrations_listed` and one
//! `mcp_registration_status` push per row.
//!
//! Every registration command spawns the `mcp_cli_stub` fixture binary (see
//! `src/bin/mcp_cli_stub.rs`), never a real vendor CLI. Every case points
//! `home_dir` at its own temporary directory, so no case reads or writes the
//! user's own home.

mod common;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use common::{
    Harness, McpCliStubFixture, approver_base_config, echo_child_path, expect_error, identify,
    mcp_cli_stub_path, mcp_cli_stub_profile_source, mcp_cli_stub_profile_with_table,
    mcp_cli_stub_toml_literal, read_response, register_approver, send, unique_temp_dir,
    write_profile,
};
use tracing_test::traced_test;
use yagura_daemon::DaemonConfig;
use yagura_daemon::child_env::ChildBaseEnv;
use yagura_daemon::mcp_registration::{
    DerivedStatus, RegistrationTarget, StatusObservation, derive_status,
};
use yagura_daemon::profile::parse_profile;
use yagura_os::program::Platform;
use yagura_protocol::{
    ClientMessage, DaemonMessage, McpRegistrationCommand, McpRegistrationRow, McpRegistrationState,
    McpRegistrationStatusInfo, McpRegistrationTomlSet, McpRegistrationTomlValue, ProfileInfo,
};
use yagura_test_support::ipc::{Connection, try_read_daemon_message};

// --- timing -------------------------------------------------------------------

/// How long any single reply or push may take before a case gives up. Far
/// above a fixture spawn, so it only bounds a hang.
const REPLY_TIMEOUT: Duration = Duration::from_secs(20);

/// How long a case that must observe "no push arrives" waits. Its positive
/// control shows that the same row's push does arrive once a bridge is set.
const NEGATIVE_OBSERVATION: Duration = Duration::from_millis(2000);

// --- fixture layout -----------------------------------------------------------

/// A launcher that no real executable is named, so a row whose launch argv
/// starts with it is never runnable here.
const NONEXISTENT_LAUNCHER: &str = "yagura-mcp-registration-test-nonexistent-launcher";

/// A CLI name that no real executable is named.
const NONEXISTENT_CLI: &str = "yagura-mcp-registration-test-nonexistent-cli";

/// A home-relative TOML settings file the fixture edits and reads back
/// `tool_timeout_sec` from; resolved against the registration child's own
/// working directory, which is the configured home.
const STUB_TOML_FILE: &str = ".stub/config.toml";

/// A home-relative JSON settings file a file-form status reads.
const STUB_JSON_FILE: &str = ".stub/mcp.json";

/// One case's directories: the profiles directory, the home the daemon
/// registers from, a state directory the fixture keeps its registry in, and
/// the bridge path this daemon would register.
struct Layout {
    root: PathBuf,
    profiles_dir: PathBuf,
    home_dir: PathBuf,
    state_dir: PathBuf,
    bridge: PathBuf,
}

impl Layout {
    fn new(tag: &str) -> Self {
        let root = unique_temp_dir(tag);
        let profiles_dir = root.join("profiles");
        let home_dir = root.join("home");
        let state_dir = root.join("stub-state");
        std::fs::create_dir_all(&profiles_dir).expect("create profiles dir");
        std::fs::create_dir_all(&home_dir).expect("create home dir");
        std::fs::create_dir_all(&state_dir).expect("create state dir");
        let bridge = root.join("bridge").join(if cfg!(windows) {
            "yagura-mcp.exe"
        } else {
            "yagura-mcp"
        });
        Self {
            root,
            profiles_dir,
            home_dir,
            state_dir,
            bridge,
        }
    }

    fn bridge_str(&self) -> String {
        self.bridge
            .to_str()
            .expect("bridge path is UTF-8")
            .to_string()
    }

    fn state_dir_str(&self) -> String {
        self.state_dir
            .to_str()
            .expect("state dir is UTF-8")
            .to_string()
    }
}

fn registration_config(layout: &Layout, bridge: Option<PathBuf>) -> DaemonConfig {
    DaemonConfig {
        profiles_dir: Some(layout.profiles_dir.clone()),
        home_dir: Some(layout.home_dir.clone()),
        instance_id: None,
        mcp_bridge: bridge,
        mcp_registration_timeout: Duration::from_secs(20),
        mcp_registration_max_output_bytes: 1 << 20,
        mcp_registration_status_max_concurrent: 2,
        ..approver_base_config()
    }
}

// --- fixture state ------------------------------------------------------------

/// Writes the fixture's own registry (`registry.json` in `state_dir`) as if
/// `mcp add` had registered each `(name, command, args)`.
fn seed_registry(state_dir: &Path, entries: &[(&str, &str, &[&str])]) {
    let mut registry = serde_json::Map::new();
    for (name, command, args) in entries {
        registry.insert(
            (*name).to_string(),
            serde_json::json!({ "command": command, "args": args, "env": {} }),
        );
    }
    std::fs::create_dir_all(state_dir).expect("create state dir");
    std::fs::write(
        state_dir.join("registry.json"),
        serde_json::to_vec_pretty(&serde_json::Value::Object(registry)).expect("encode registry"),
    )
    .expect("write registry");
}

/// Writes the TOML settings file the fixture reads `tool_timeout_sec` from,
/// with `[mcp_servers.<server>]` carrying `tool_timeout_literal` verbatim.
fn seed_toml_settings(home_dir: &Path, server: &str, command: &str, tool_timeout_literal: &str) {
    let path = home_dir.join(STUB_TOML_FILE);
    std::fs::create_dir_all(path.parent().expect("settings file has a parent"))
        .expect("create settings dir");
    std::fs::write(
        &path,
        format!(
            "[mcp_servers.{server}]\ncommand = '{command}'\ntool_timeout_sec = \
             {tool_timeout_literal}\n"
        ),
    )
    .expect("write TOML settings");
}

/// Writes the JSON settings file a file-form status reads.
fn seed_json_settings(home_dir: &Path, server: &str, command: &str) {
    let path = home_dir.join(STUB_JSON_FILE);
    std::fs::create_dir_all(path.parent().expect("settings file has a parent"))
        .expect("create settings dir");
    let body = serde_json::json!({ "mcpServers": { server: { "command": command, "args": [] } } });
    std::fs::write(
        &path,
        serde_json::to_vec_pretty(&body).expect("encode settings"),
    )
    .expect("write JSON settings");
}

// --- registration tables ------------------------------------------------------

/// A host row whose status is read in the line form (`Command:` / `Scope:`).
const LINES_TABLE: &str = "[mcp_registration]\n\
add = [\"mcp\", \"add\", \"{server}\", \"{env_args}\", \"--\", \"{bridge}\"]\n\
remove = [\"mcp\", \"remove\", \"{server}\"]\n\
env_arg = [\"--env\", \"{env}\"]\n\
\n\
[mcp_registration.status]\n\
args = [\"mcp\", \"get\", \"{server}\"]\n\
format = \"lines\"\n\
command = \"Command:\"\n\
require = { \"Scope:\" = \"User\" }\n\
absent = \"No MCP server named\"\n";

/// A host row whose status is read directly from a JSON settings file under
/// the home directory.
fn host_file_table() -> String {
    format!(
        "[mcp_registration]\n\
         add = [\"mcp\", \"add\", \"{{server}}\", \"{{env_args}}\", \"--\", \"{{bridge}}\"]\n\
         remove = [\"mcp\", \"remove\", \"{{server}}\"]\n\
         env_arg = [\"--env\", \"{{env}}\"]\n\
         \n\
         [mcp_registration.status]\n\
         file = \"{STUB_JSON_FILE}\"\n\
         format = \"json\"\n\
         command = \"/mcpServers/{{server}}/command\"\n"
    )
}

/// A foreign-environment row: the bridge path is turned into that
/// environment's own path by a probe, and `WSLENV` forwards the instance
/// variable. Its status is read in the line form.
fn probed_lines_table() -> String {
    let stub = mcp_cli_stub_toml_literal();
    format!(
        "[mcp_registration]\n\
         group = \"WSL\"\n\
         add = [\"mcp\", \"add\", \"{{server}}\", \"{{env_args}}\", \"--\", \"{{bridge}}\"]\n\
         remove = [\"mcp\", \"remove\", \"{{server}}\"]\n\
         env_arg = [\"--env\", \"{{env}}\"]\n\
         bridge_probe = [{stub}, \"probe\", \"{{bridge}}\"]\n\
         env_forward = {{ name = \"WSLENV\", separator = \":\" }}\n\
         \n\
         [mcp_registration.status]\n\
         args = [\"mcp\", \"get\", \"{{server}}\"]\n\
         format = \"lines\"\n\
         command = \"Command:\"\n\
         absent = \"No MCP server named\"\n"
    )
}

/// A foreign-environment row whose status reads a JSON settings file
/// through the environment's own read tool (`files.read`), which exits `3`
/// for a missing file. `home` prefixes `{file}` so the read does not depend
/// on the child's working directory.
fn probed_file_table(home: &str) -> String {
    let stub = mcp_cli_stub_toml_literal();
    format!(
        "[mcp_registration]\n\
         group = \"WSL\"\n\
         add = [\"mcp\", \"add\", \"{{server}}\", \"{{env_args}}\", \"--\", \"{{bridge}}\"]\n\
         remove = [\"mcp\", \"remove\", \"{{server}}\"]\n\
         env_arg = [\"--env\", \"{{env}}\"]\n\
         bridge_probe = [{stub}, \"probe\", \"{{bridge}}\"]\n\
         files = {{ read = [{stub}, \"file-read\", '{home}/{{file}}'], absent_exit = 3 }}\n\
         \n\
         [mcp_registration.status]\n\
         file = \"{STUB_JSON_FILE}\"\n\
         format = \"json\"\n\
         command = \"/mcpServers/{{server}}/command\"\n"
    )
}

/// A profile whose launch argv is a launcher, one flag, then the CLI, none
/// of which exists: its row is listed, its CLI starts at index 2, and its
/// state is never runnable.
fn launcher_profile_source(display_name: &str) -> String {
    format!(
        "schema = 1\ndisplay_name = \"{display_name}\"\n\n[command]\ndefault = \
         [\"{NONEXISTENT_LAUNCHER}\", \"--launcher-flag\", \"{NONEXISTENT_CLI}\"]\n\n\
         {LINES_TABLE}"
    )
}

/// A profile with no registration table at all.
fn plain_profile_source(display_name: &str) -> String {
    format!(
        "schema = 1\ndisplay_name = \"{display_name}\"\n\n[command]\ndefault = [{}]\n",
        mcp_cli_stub_toml_literal()
    )
}

/// A profile with a registration table but no argv for this host.
fn other_os_profile_source(display_name: &str) -> String {
    let other_os_keys = if cfg!(windows) {
        "macos = [\"macos-only\"]\nlinux = [\"linux-only\"]\n"
    } else if cfg!(target_os = "macos") {
        "windows = [\"windows-only\"]\nlinux = [\"linux-only\"]\n"
    } else {
        "windows = [\"windows-only\"]\nmacos = [\"macos-only\"]\n"
    };
    format!(
        "schema = 1\ndisplay_name = \"{display_name}\"\n\n[command]\n{other_os_keys}\n{LINES_TABLE}"
    )
}

/// The `[env]` pair pointing the fixture at `layout`'s state directory.
fn state_env(layout: &Layout) -> (&'static str, String) {
    ("MCP_CLI_STUB_STATE_DIR", layout.state_dir_str())
}

fn lines_profile(layout: &Layout, display_name: &str, extra_env: &[(&str, &str)]) -> String {
    let (key, value) = state_env(layout);
    let mut env: Vec<(&str, &str)> = vec![(key, value.as_str())];
    env.extend_from_slice(extra_env);
    mcp_cli_stub_profile_with_table(display_name, "", LINES_TABLE, &env)
}

// --- IPC round trips ----------------------------------------------------------

async fn approver_connection(h: &Harness) -> Connection {
    let mut client = h.connect().await;
    register_approver(&mut client).await;
    client
}

async fn send_list(client: &mut Connection, request_id: u64) {
    send(client, &ClientMessage::ListMcpRegistrations { request_id }).await;
}

/// The immediate answer to a listing: its bridge and rows.
struct Listed {
    bridge: Option<String>,
    rows: Vec<McpRegistrationRow>,
}

fn expect_listed(reply: DaemonMessage, expected_request_id: u64) -> Listed {
    match reply {
        DaemonMessage::McpRegistrationsListed {
            request_id,
            bridge,
            rows,
        } => {
            assert_eq!(
                request_id, expected_request_id,
                "the answer must echo the request id it answers"
            );
            Listed { bridge, rows }
        }
        other => panic!("expected mcp_registrations_listed, got {other:?}"),
    }
}

/// Reads `count` status pushes for `request_id`, keyed by profile.
async fn collect_statuses(
    client: &mut Connection,
    request_id: u64,
    count: usize,
) -> BTreeMap<String, McpRegistrationStatusInfo> {
    let mut statuses: BTreeMap<String, McpRegistrationStatusInfo> = BTreeMap::new();
    while statuses.len() < count {
        let push: DaemonMessage = read_response(client, REPLY_TIMEOUT).await;
        match push {
            DaemonMessage::McpRegistrationStatus {
                request_id: pushed_for,
                status,
            } => {
                assert_eq!(
                    pushed_for, request_id,
                    "a status push must carry the listing's own request id"
                );
                let previous = statuses.insert(status.profile.clone(), status);
                assert!(
                    previous.is_none(),
                    "each row's state must be pushed exactly once"
                );
            }
            other => panic!("expected mcp_registration_status, got {other:?}"),
        }
    }
    statuses
}

/// Lists, then reads one push per row.
async fn list_and_collect(
    client: &mut Connection,
    request_id: u64,
) -> (Listed, BTreeMap<String, McpRegistrationStatusInfo>) {
    send_list(client, request_id).await;
    let reply: DaemonMessage = read_response(client, REPLY_TIMEOUT).await;
    let listed = expect_listed(reply, request_id);
    let count = listed.rows.len();
    let statuses = collect_statuses(client, request_id, count).await;
    (listed, statuses)
}

async fn list_profile_ids(client: &mut Connection) -> Vec<String> {
    send(client, &ClientMessage::ListProfiles).await;
    let reply: DaemonMessage = read_response(client, REPLY_TIMEOUT).await;
    match reply {
        DaemonMessage::ProfileList { profiles, .. } => profiles
            .into_iter()
            .map(|profile: ProfileInfo| profile.id)
            .collect(),
        other => panic!("expected profile_list, got {other:?}"),
    }
}

fn status_of<'a>(
    statuses: &'a BTreeMap<String, McpRegistrationStatusInfo>,
    profile: &str,
) -> &'a McpRegistrationStatusInfo {
    statuses
        .get(profile)
        .unwrap_or_else(|| panic!("no status was pushed for {profile}: {statuses:?}"))
}

fn row_of<'a>(rows: &'a [McpRegistrationRow], profile: &str) -> &'a McpRegistrationRow {
    rows.iter()
        .find(|row| row.profile == profile)
        .unwrap_or_else(|| panic!("no row was listed for {profile}: {rows:?}"))
}

/// The state the pure derivation gives `observed` for the profile `source`
/// describes, so a pushed reason is compared against the one fixed phrase
/// the daemon's own rule produces rather than a copy of its wording.
fn derived_for(source: &str, observed: &StatusObservation, bridge: &str) -> DerivedStatus {
    let profile = parse_profile("derived", source).expect("the profile parses");
    let spec = profile
        .mcp_registration
        .expect("the profile carries a registration table");
    derive_status(
        &spec,
        observed,
        &RegistrationTarget::for_instance(None),
        bridge,
        Platform::host(),
    )
}

fn stub_str() -> String {
    mcp_cli_stub_path()
        .to_str()
        .expect("stub path is UTF-8")
        .to_string()
}

fn command(argv: &[&str], program_at: u32) -> McpRegistrationCommand {
    McpRegistrationCommand {
        argv: argv.iter().map(|arg| (*arg).to_string()).collect(),
        program_at,
    }
}

// =============================================================================
// Only an approver may list, and a refused listing starts no child.
// =============================================================================

#[tokio::test]
async fn a_window_connection_and_an_unidentified_connection_are_refused_and_start_no_child() {
    let layout = Layout::new("mcpreg-auth");
    let log = layout.root.join("stub-log.jsonl");
    seed_registry(&layout.state_dir, &[]);
    write_profile(
        &layout.profiles_dir,
        "alpha",
        &mcp_cli_stub_profile_source(
            "Alpha",
            &McpCliStubFixture {
                state_dir: &layout.state_dir,
                toml_table_path: Some(STUB_TOML_FILE),
                on_duplicate: None,
                log_path: Some(&log),
            },
        ),
    );
    let h = Harness::start(
        "mcpreg-auth",
        registration_config(&layout, Some(layout.bridge.clone())),
    )
    .await;

    let window_id = h
        .open(
            vec![
                echo_child_path()
                    .to_str()
                    .expect("echo_child path is UTF-8")
                    .to_string(),
            ],
            None,
        )
        .await;
    let mut window_client = h.connect().await;
    identify(&mut window_client, &window_id).await;
    send_list(&mut window_client, 1).await;
    expect_error(
        read_response(&mut window_client, REPLY_TIMEOUT).await,
        "auth_failed",
    );

    let mut unidentified = h.connect().await;
    send_list(&mut unidentified, 2).await;
    expect_error(
        read_response(&mut unidentified, REPLY_TIMEOUT).await,
        "auth_failed",
    );

    tokio::time::sleep(NEGATIVE_OBSERVATION).await;
    assert!(
        !log.exists(),
        "a refused listing must not start the registration CLI"
    );

    // Control: an approver's listing of the same profile does start it.
    let mut approver = approver_connection(&h).await;
    let (listed, statuses) = list_and_collect(&mut approver, 3).await;
    assert_eq!(listed.rows.len(), 1);
    assert_eq!(statuses.len(), 1);
    assert!(
        log.exists(),
        "the approver's listing must have run the fixture"
    );

    h.registry.terminate_all().await;
}

// =============================================================================
// The rows: only listed profiles with a registration table, in the profile
// list's order, with the full argv and where the CLI starts in it.
// =============================================================================

#[tokio::test]
async fn rows_cover_listed_profiles_with_a_table_in_listing_order_with_full_argv() {
    let layout = Layout::new("mcpreg-rows");
    let bridge = layout.bridge_str();
    let stub = stub_str();
    let (state_key, state_value) = state_env(&layout);
    seed_registry(&layout.state_dir, &[]);

    write_profile(
        &layout.profiles_dir,
        "alpha",
        &lines_profile(&layout, "Alpha", &[]),
    );
    write_profile(
        &layout.profiles_dir,
        "bravo",
        &mcp_cli_stub_profile_source(
            "Bravo",
            &McpCliStubFixture {
                state_dir: &layout.state_dir,
                toml_table_path: Some(STUB_TOML_FILE),
                on_duplicate: None,
                log_path: None,
            },
        )
        .replacen(
            "display_name = \"Bravo\"\n",
            "display_name = \"Bravo\"\nenabled = false\n",
            1,
        ),
    );
    write_profile(
        &layout.profiles_dir,
        "charlie",
        &mcp_cli_stub_profile_with_table(
            "Charlie",
            "",
            &probed_lines_table(),
            &[(state_key, state_value.as_str())],
        ),
    );
    write_profile(
        &layout.profiles_dir,
        "delta",
        &plain_profile_source("Delta"),
    );
    write_profile(
        &layout.profiles_dir,
        "foxtrot",
        &other_os_profile_source("Foxtrot"),
    );
    write_profile(
        &layout.profiles_dir,
        "golf",
        &launcher_profile_source("Golf"),
    );

    let h = Harness::start(
        "mcpreg-rows",
        registration_config(&layout, Some(layout.bridge.clone())),
    )
    .await;
    let mut approver = approver_connection(&h).await;

    let expected_ids = ["alpha", "bravo", "charlie", "golf"];
    let listing_order: Vec<String> = list_profile_ids(&mut approver)
        .await
        .into_iter()
        .filter(|id| expected_ids.contains(&id.as_str()))
        .collect();

    send_list(&mut approver, 7).await;
    let listed = expect_listed(read_response(&mut approver, REPLY_TIMEOUT).await, 7);
    assert_eq!(listed.bridge.as_deref(), Some(bridge.as_str()));

    let row_ids: Vec<String> = listed.rows.iter().map(|row| row.profile.clone()).collect();
    assert_eq!(
        row_ids, listing_order,
        "rows are the listed profiles that carry a registration table (a disabled one \
         included), in the profile list's order"
    );

    let alpha = row_of(&listed.rows, "alpha");
    assert_eq!(alpha.display_name, "Alpha");
    assert_eq!(alpha.group, None);
    assert_eq!(alpha.toml_set, None);
    assert_eq!(
        alpha.add,
        command(&[&stub, "mcp", "add", "yagura", "--", &bridge], 0),
        "a host row's add is rendered with the bridge, after the launch argv"
    );
    assert_eq!(
        alpha.remove,
        command(&[&stub, "mcp", "remove", "yagura"], 0)
    );

    let bravo = row_of(&listed.rows, "bravo");
    assert_eq!(bravo.display_name, "Bravo");
    assert_eq!(
        bravo.toml_set,
        Some(McpRegistrationTomlSet {
            file: STUB_TOML_FILE.to_string(),
            table: vec!["mcp_servers".to_string(), "yagura".to_string()],
            values: vec![McpRegistrationTomlValue {
                key: "tool_timeout_sec".to_string(),
                value: "86400".to_string(),
            }],
        })
    );

    let charlie = row_of(&listed.rows, "charlie");
    assert_eq!(charlie.group.as_deref(), Some("WSL"));
    assert_eq!(
        charlie.add,
        command(&[&stub, "mcp", "add", "yagura", "--", "{bridge}"], 0),
        "a probed row's bridge is not known before its probe runs, so the listing keeps the \
         placeholder"
    );

    let golf = row_of(&listed.rows, "golf");
    assert_eq!(
        golf.add,
        command(
            &[
                NONEXISTENT_LAUNCHER,
                "--launcher-flag",
                NONEXISTENT_CLI,
                "mcp",
                "add",
                "yagura",
                "--",
                &bridge,
            ],
            2
        ),
        "the CLI starts at the launch argv's last element"
    );
    assert_eq!(
        golf.remove,
        command(
            &[
                NONEXISTENT_LAUNCHER,
                "--launcher-flag",
                NONEXISTENT_CLI,
                "mcp",
                "remove",
                "yagura",
            ],
            2
        )
    );

    let statuses = collect_statuses(&mut approver, 7, listed.rows.len()).await;
    assert_eq!(statuses.len(), expected_ids.len());
}

// =============================================================================
// One push per row, each carrying the state read from the CLI itself.
// =============================================================================

#[tokio::test]
async fn a_row_registered_with_the_same_bridge_is_registered() {
    let layout = Layout::new("mcpreg-registered");
    let bridge = layout.bridge_str();
    let stub = stub_str();
    seed_registry(&layout.state_dir, &[("yagura", &bridge, &[])]);
    write_profile(
        &layout.profiles_dir,
        "alpha",
        &lines_profile(&layout, "Alpha", &[]),
    );
    let h = Harness::start(
        "mcpreg-registered",
        registration_config(&layout, Some(layout.bridge.clone())),
    )
    .await;
    let mut approver = approver_connection(&h).await;

    let (_, statuses) = list_and_collect(&mut approver, 11).await;
    let status = status_of(&statuses, "alpha");
    assert_eq!(status.state, McpRegistrationState::Registered);
    assert!(status.differs_in.is_empty());
    assert_eq!(status.reason, None);
    assert_eq!(
        status.add,
        Some(command(&[&stub, "mcp", "add", "yagura", "--", &bridge], 0))
    );
    assert_eq!(
        status.remove,
        Some(command(&[&stub, "mcp", "remove", "yagura"], 0))
    );
}

#[tokio::test]
async fn a_row_registered_with_another_bridge_differs_in_its_command() {
    let layout = Layout::new("mcpreg-differs-command");
    let other_bridge = layout.root.join("elsewhere").join("yagura-mcp");
    let other_bridge = other_bridge.to_str().expect("path is UTF-8").to_string();
    seed_registry(&layout.state_dir, &[("yagura", &other_bridge, &[])]);
    write_profile(
        &layout.profiles_dir,
        "alpha",
        &lines_profile(&layout, "Alpha", &[]),
    );
    let h = Harness::start(
        "mcpreg-differs-command",
        registration_config(&layout, Some(layout.bridge.clone())),
    )
    .await;
    let mut approver = approver_connection(&h).await;

    let (_, statuses) = list_and_collect(&mut approver, 12).await;
    let status = status_of(&statuses, "alpha");
    assert_eq!(status.state, McpRegistrationState::Differs);
    assert_eq!(status.differs_in, vec!["command".to_string()]);
}

#[tokio::test]
async fn a_settings_value_read_back_as_a_float_compares_by_number_and_a_different_one_differs() {
    let layout = Layout::new("mcpreg-tool-timeout");
    let bridge = layout.bridge_str();
    seed_registry(&layout.state_dir, &[("yagura", &bridge, &[])]);
    let fixture = McpCliStubFixture {
        state_dir: &layout.state_dir,
        toml_table_path: Some(STUB_TOML_FILE),
        on_duplicate: None,
        log_path: None,
    };
    write_profile(
        &layout.profiles_dir,
        "bravo",
        &mcp_cli_stub_profile_source("Bravo", &fixture),
    );
    let h = Harness::start(
        "mcpreg-tool-timeout",
        registration_config(&layout, Some(layout.bridge.clone())),
    )
    .await;
    let mut approver = approver_connection(&h).await;

    // The CLI reports the integer the profile declares as 86400.0.
    seed_toml_settings(&layout.home_dir, "yagura", &bridge, "86400.0");
    let (_, statuses) = list_and_collect(&mut approver, 13).await;
    let status = status_of(&statuses, "bravo");
    assert_eq!(
        status.state,
        McpRegistrationState::Registered,
        "86400.0 read back is the same number as the declared 86400: {status:?}"
    );
    assert!(status.differs_in.is_empty());

    seed_toml_settings(&layout.home_dir, "yagura", &bridge, "60");
    let (_, statuses) = list_and_collect(&mut approver, 14).await;
    let status = status_of(&statuses, "bravo");
    assert_eq!(status.state, McpRegistrationState::Differs);
    assert_eq!(status.differs_in, vec!["tool_timeout_sec".to_string()]);
}

#[tokio::test]
async fn owned_settings_entries_missing_from_the_file_differ_under_their_dotted_path() {
    const CONFIRM_LINE: &str = "confirm = { tool_timeout_sec = \"/tool_timeout_sec\" }\n";
    const OWNED_ENTRIES: &str = "\n[mcp_registration.toml_set.entries]\n\
                                 under = [\"hooks\", \"state\"]\n\n\
                                 [mcp_registration.toml_set.entries.default]\n\
                                 'yagura-hook' = { trusted_hash = \"sha256:0\" }\n";
    let layout = Layout::new("mcpreg-owned-entries");
    let bridge = layout.bridge_str();
    seed_registry(&layout.state_dir, &[("yagura", &bridge, &[])]);
    let fixture = McpCliStubFixture {
        state_dir: &layout.state_dir,
        toml_table_path: Some(STUB_TOML_FILE),
        on_duplicate: None,
        log_path: None,
    };
    let source = mcp_cli_stub_profile_source("Bravo", &fixture)
        .replace(CONFIRM_LINE, &format!("{CONFIRM_LINE}{OWNED_ENTRIES}"));
    assert!(
        source.contains(OWNED_ENTRIES),
        "the fixture must carry the owned entries: {source}"
    );
    write_profile(&layout.profiles_dir, "bravo", &source);
    let h = Harness::start(
        "mcpreg-owned-entries",
        registration_config(&layout, Some(layout.bridge.clone())),
    )
    .await;
    let mut approver = approver_connection(&h).await;

    // The CLI's own key holds; only the entries the row owns are absent.
    seed_toml_settings(&layout.home_dir, "yagura", &bridge, "86400");
    let (_, statuses) = list_and_collect(&mut approver, 15).await;
    let status = status_of(&statuses, "bravo");
    assert_eq!(status.state, McpRegistrationState::Differs, "{status:?}");
    assert_eq!(status.differs_in, vec!["hooks.state".to_string()]);
}

#[tokio::test]
async fn owned_settings_entries_for_another_operating_system_are_not_read_back_on_this_one() {
    const CONFIRM_LINE: &str = "confirm = { tool_timeout_sec = \"/tool_timeout_sec\" }\n";
    let other_os = if cfg!(windows) { "macos" } else { "windows" };
    let owned_entries = format!(
        "\n[mcp_registration.toml_set.entries]\nunder = [\"hooks\", \"state\"]\n\n\
         [mcp_registration.toml_set.entries.{other_os}]\n\
         'yagura-hook' = {{ trusted_hash = \"sha256:0\" }}\n"
    );
    let layout = Layout::new("mcpreg-owned-entries-other-os");
    let bridge = layout.bridge_str();
    seed_registry(&layout.state_dir, &[("yagura", &bridge, &[])]);
    let fixture = McpCliStubFixture {
        state_dir: &layout.state_dir,
        toml_table_path: Some(STUB_TOML_FILE),
        on_duplicate: None,
        log_path: None,
    };
    let source = mcp_cli_stub_profile_source("Tango", &fixture)
        .replace(CONFIRM_LINE, &format!("{CONFIRM_LINE}{owned_entries}"));
    assert!(
        source.contains(&owned_entries),
        "the fixture must carry the owned entries: {source}"
    );
    write_profile(&layout.profiles_dir, "tango", &source);
    let h = Harness::start(
        "mcpreg-owned-entries-other-os",
        registration_config(&layout, Some(layout.bridge.clone())),
    )
    .await;
    let mut approver = approver_connection(&h).await;

    // No settings file at all: only the CLI's own key is reported missing,
    // since the entries do not apply to this operating system.
    let (_, statuses) = list_and_collect(&mut approver, 16).await;
    let status = status_of(&statuses, "tango");
    assert_eq!(status.state, McpRegistrationState::Differs, "{status:?}");
    assert_eq!(status.differs_in, vec!["tool_timeout_sec".to_string()]);
}

#[tokio::test]
async fn a_row_the_cli_reports_absent_on_stderr_is_not_registered() {
    let layout = Layout::new("mcpreg-not-registered");
    seed_registry(&layout.state_dir, &[]);
    write_profile(
        &layout.profiles_dir,
        "alpha",
        &lines_profile(&layout, "Alpha", &[]),
    );
    let h = Harness::start(
        "mcpreg-not-registered",
        registration_config(&layout, Some(layout.bridge.clone())),
    )
    .await;
    let mut approver = approver_connection(&h).await;

    let (_, statuses) = list_and_collect(&mut approver, 15).await;
    let status = status_of(&statuses, "alpha");
    assert_eq!(status.state, McpRegistrationState::NotRegistered);
    assert!(status.differs_in.is_empty());
}

#[tokio::test]
async fn a_missing_executable_and_an_exit_127_are_not_installed() {
    let layout = Layout::new("mcpreg-not-installed");
    let bridge = layout.bridge_str();
    seed_registry(&layout.state_dir, &[("yagura", &bridge, &[])]);
    write_profile(
        &layout.profiles_dir,
        "golf",
        &launcher_profile_source("Golf"),
    );
    write_profile(
        &layout.profiles_dir,
        "hotel",
        &lines_profile(
            &layout,
            "Hotel",
            &[
                ("MCP_CLI_STUB_FAIL_SUBCOMMAND", "get"),
                ("MCP_CLI_STUB_FAIL_EXIT_CODE", "127"),
            ],
        ),
    );
    // Control: the same fixture and registry without the injected exit.
    write_profile(
        &layout.profiles_dir,
        "india",
        &lines_profile(&layout, "India", &[]),
    );
    let h = Harness::start(
        "mcpreg-not-installed",
        registration_config(&layout, Some(layout.bridge.clone())),
    )
    .await;
    let mut approver = approver_connection(&h).await;

    let (_, statuses) = list_and_collect(&mut approver, 16).await;
    let expected_reason = derived_for(
        &launcher_profile_source("Golf"),
        &StatusObservation::NotInstalled,
        &bridge,
    )
    .reason;
    for profile in ["golf", "hotel"] {
        let status = status_of(&statuses, profile);
        assert_eq!(
            status.state,
            McpRegistrationState::NotInstalled,
            "{profile}: {status:?}"
        );
        assert_eq!(status.reason, expected_reason, "{profile}");
    }
    assert_eq!(
        status_of(&statuses, "india").state,
        McpRegistrationState::Registered
    );
}

#[tokio::test]
async fn unreadable_output_and_a_timeout_are_unknown_with_a_fixed_reason() {
    let layout = Layout::new("mcpreg-unknown");
    let bridge = layout.bridge_str();
    seed_registry(&layout.state_dir, &[("yagura", &bridge, &[])]);
    let fixture = McpCliStubFixture {
        state_dir: &layout.state_dir,
        toml_table_path: Some(STUB_TOML_FILE),
        on_duplicate: None,
        log_path: None,
    };
    let broken_source = format!(
        "{}MCP_CLI_STUB_FAIL_SUBCOMMAND = 'get'\nMCP_CLI_STUB_FAIL_EXIT_CODE = '0'\n\
         MCP_CLI_STUB_FAIL_STDOUT = '{{not json'\n",
        mcp_cli_stub_profile_source("Juliet", &fixture)
    );
    write_profile(&layout.profiles_dir, "juliet", &broken_source);
    write_profile(
        &layout.profiles_dir,
        "kilo",
        &lines_profile(&layout, "Kilo", &[("MCP_CLI_STUB_DELAY_MS", "4000")]),
    );
    let config = DaemonConfig {
        mcp_registration_timeout: Duration::from_secs(1),
        ..registration_config(&layout, Some(layout.bridge.clone()))
    };
    let h = Harness::start("mcpreg-unknown", config).await;
    let mut approver = approver_connection(&h).await;

    let (_, statuses) = list_and_collect(&mut approver, 17).await;

    let broken = status_of(&statuses, "juliet");
    assert_eq!(broken.state, McpRegistrationState::Unknown);
    assert_eq!(
        broken.reason,
        derived_for(
            &broken_source,
            &StatusObservation::Exited {
                code: Some(0),
                stdout: b"{not json".to_vec(),
                stderr: Vec::new(),
            },
            &bridge,
        )
        .reason
    );

    let slow = status_of(&statuses, "kilo");
    assert_eq!(slow.state, McpRegistrationState::Unknown);
    assert_eq!(
        slow.reason,
        derived_for(
            &lines_profile(&layout, "Kilo", &[]),
            &StatusObservation::TimedOut,
            &bridge,
        )
        .reason
    );
    assert!(slow.reason.is_some(), "an unknown state always says why");
}

// =============================================================================
// A probed (foreign-environment) row and a file-form status.
// =============================================================================

#[tokio::test]
async fn a_probed_row_compares_against_the_probe_output_exactly_and_pushes_the_probed_argv() {
    let layout = Layout::new("mcpreg-probe");
    let bridge = layout.bridge_str();
    let stub = stub_str();
    let (state_key, state_value) = state_env(&layout);
    // The fixture's own probe transform: forward slashes, a `/probed` prefix.
    let probed = format!("/probed{}", bridge.replace('\\', "/"));

    let probed_state = layout.root.join("probed-state");
    let unprobed_state = layout.root.join("unprobed-state");
    seed_registry(&probed_state, &[("yagura", &probed, &[])]);
    seed_registry(&unprobed_state, &[("yagura", &bridge, &[])]);
    let probed_state = probed_state.to_str().expect("UTF-8").to_string();
    let unprobed_state = unprobed_state.to_str().expect("UTF-8").to_string();

    let table = probed_lines_table();
    write_profile(
        &layout.profiles_dir,
        "lima",
        &mcp_cli_stub_profile_with_table("Lima", "", &table, &[(state_key, probed_state.as_str())]),
    );
    // Registered with the unconverted host path: not what this environment
    // would register.
    write_profile(
        &layout.profiles_dir,
        "mike",
        &mcp_cli_stub_profile_with_table(
            "Mike",
            "",
            &table,
            &[(state_key, unprobed_state.as_str())],
        ),
    );
    write_profile(
        &layout.profiles_dir,
        "november",
        &mcp_cli_stub_profile_with_table(
            "November",
            "",
            &table,
            &[
                (state_key, state_value.as_str()),
                ("MCP_CLI_STUB_PROBE_OUTPUT", "relative/not-a-path"),
            ],
        ),
    );
    write_profile(
        &layout.profiles_dir,
        "oscar",
        &mcp_cli_stub_profile_with_table(
            "Oscar",
            "",
            &table,
            &[
                (state_key, state_value.as_str()),
                ("MCP_CLI_STUB_PROBE_EXIT_CODE", "1"),
            ],
        ),
    );
    let h = Harness::start(
        "mcpreg-probe",
        registration_config(&layout, Some(layout.bridge.clone())),
    )
    .await;
    let mut approver = approver_connection(&h).await;

    let (_, statuses) = list_and_collect(&mut approver, 21).await;

    let lima = status_of(&statuses, "lima");
    assert_eq!(lima.state, McpRegistrationState::Registered, "{lima:?}");
    assert_eq!(
        lima.add,
        Some(command(&[&stub, "mcp", "add", "yagura", "--", &probed], 0)),
        "the push carries the add argv redrawn with the probe's output"
    );

    let mike = status_of(&statuses, "mike");
    assert_eq!(mike.state, McpRegistrationState::Differs, "{mike:?}");
    assert_eq!(mike.differs_in, vec!["command".to_string()]);

    let november = status_of(&statuses, "november");
    assert_eq!(november.state, McpRegistrationState::Unknown);
    assert!(november.reason.is_some());

    let oscar = status_of(&statuses, "oscar");
    assert_eq!(oscar.state, McpRegistrationState::NotInstalled);
}

#[tokio::test]
async fn a_probed_file_status_reads_through_the_read_tool_and_its_absent_exit_is_not_registered() {
    let layout = Layout::new("mcpreg-probe-file");
    let bridge = layout.bridge_str();
    let probed = format!("/probed{}", bridge.replace('\\', "/"));
    let (state_key, state_value) = state_env(&layout);

    let present_home = layout.root.join("present-home");
    let absent_home = layout.root.join("absent-home");
    std::fs::create_dir_all(&absent_home).expect("create absent home");
    seed_json_settings(&present_home, "yagura", &probed);
    let present_home = present_home.to_str().expect("UTF-8").to_string();
    let absent_home = absent_home.to_str().expect("UTF-8").to_string();
    let papa_log = layout.root.join("stub-log-papa.jsonl");
    let quebec_log = layout.root.join("stub-log-quebec.jsonl");
    let papa_log_str = papa_log.to_str().expect("UTF-8").to_string();
    let quebec_log_str = quebec_log.to_str().expect("UTF-8").to_string();

    write_profile(
        &layout.profiles_dir,
        "papa",
        &mcp_cli_stub_profile_with_table(
            "Papa",
            "",
            &probed_file_table(&present_home),
            &[
                (state_key, state_value.as_str()),
                ("MCP_CLI_STUB_LOG", papa_log_str.as_str()),
            ],
        ),
    );
    write_profile(
        &layout.profiles_dir,
        "quebec",
        &mcp_cli_stub_profile_with_table(
            "Quebec",
            "",
            &probed_file_table(&absent_home),
            &[
                (state_key, state_value.as_str()),
                ("MCP_CLI_STUB_LOG", quebec_log_str.as_str()),
            ],
        ),
    );
    let h = Harness::start(
        "mcpreg-probe-file",
        registration_config(&layout, Some(layout.bridge.clone())),
    )
    .await;
    let mut approver = approver_connection(&h).await;

    let (_, statuses) = list_and_collect(&mut approver, 22).await;
    assert_eq!(
        status_of(&statuses, "papa").state,
        McpRegistrationState::Registered
    );
    assert_eq!(
        status_of(&statuses, "quebec").state,
        McpRegistrationState::NotRegistered,
        "the read tool's absent exit means no registration"
    );

    for log in [&papa_log, &quebec_log] {
        let read_calls = std::fs::read_to_string(log)
            .expect("the fixture logged its calls")
            .lines()
            .filter(|line| line.contains("\"file-read\""))
            .count();
        assert_eq!(
            read_calls,
            1,
            "a foreign row's settings file is read through its own read tool: {}",
            log.display()
        );
    }
}

#[tokio::test]
async fn a_host_file_status_reads_under_home_and_a_missing_file_is_not_registered() {
    let layout = Layout::new("mcpreg-host-file");
    let bridge = layout.bridge_str();
    let (state_key, state_value) = state_env(&layout);
    write_profile(
        &layout.profiles_dir,
        "romeo",
        &mcp_cli_stub_profile_with_table(
            "Romeo",
            "",
            &host_file_table(),
            &[(state_key, state_value.as_str())],
        ),
    );
    let h = Harness::start(
        "mcpreg-host-file",
        registration_config(&layout, Some(layout.bridge.clone())),
    )
    .await;
    let mut approver = approver_connection(&h).await;

    let (_, statuses) = list_and_collect(&mut approver, 23).await;
    assert_eq!(
        status_of(&statuses, "romeo").state,
        McpRegistrationState::NotRegistered,
        "no settings file means no registration"
    );

    seed_json_settings(&layout.home_dir, "yagura", &bridge);
    let (_, statuses) = list_and_collect(&mut approver, 24).await;
    assert_eq!(
        status_of(&statuses, "romeo").state,
        McpRegistrationState::Registered,
        "the settings file is read under the configured home"
    );
}

#[tokio::test]
async fn a_host_status_file_that_exists_but_cannot_be_read_is_unknown() {
    let layout = Layout::new("mcpreg-host-file-unreadable");
    let (state_key, state_value) = state_env(&layout);
    write_profile(
        &layout.profiles_dir,
        "sierra",
        &mcp_cli_stub_profile_with_table(
            "Sierra",
            "",
            &host_file_table(),
            &[(state_key, state_value.as_str())],
        ),
    );
    // A directory where the file belongs: present, but not readable as a file.
    std::fs::create_dir_all(layout.home_dir.join(STUB_JSON_FILE))
        .expect("create a directory at the settings path");
    let h = Harness::start(
        "mcpreg-host-file-unreadable",
        registration_config(&layout, Some(layout.bridge.clone())),
    )
    .await;
    let mut approver = approver_connection(&h).await;

    let (_, statuses) = list_and_collect(&mut approver, 25).await;
    assert_eq!(
        status_of(&statuses, "sierra").state,
        McpRegistrationState::Unknown,
        "a settings file that cannot be read is not the same as no settings file"
    );
}

/// Writes `contents` verbatim as the JSON settings file under `home_dir`.
fn write_json_settings_raw(home_dir: &Path, contents: &str) {
    let path = home_dir.join(STUB_JSON_FILE);
    std::fs::create_dir_all(path.parent().expect("settings file has a parent"))
        .expect("create settings dir");
    std::fs::write(&path, contents).expect("write raw settings");
}

#[tokio::test]
async fn an_empty_or_whitespace_only_host_settings_file_is_not_registered_and_garbage_is_unknown() {
    let layout = Layout::new("mcpreg-host-blank");
    let (state_key, state_value) = state_env(&layout);
    write_profile(
        &layout.profiles_dir,
        "sierra",
        &mcp_cli_stub_profile_with_table(
            "Sierra",
            "",
            &host_file_table(),
            &[(state_key, state_value.as_str())],
        ),
    );
    let h = Harness::start(
        "mcpreg-host-blank",
        registration_config(&layout, Some(layout.bridge.clone())),
    )
    .await;
    let mut approver = approver_connection(&h).await;

    let cases: [(&str, McpRegistrationState); 4] = [
        ("not json", McpRegistrationState::Unknown),
        ("", McpRegistrationState::NotRegistered),
        (" \t\r\n \n", McpRegistrationState::NotRegistered),
        ("\n", McpRegistrationState::NotRegistered),
    ];
    for (request_id, (contents, expected)) in (40_u64..).zip(cases) {
        write_json_settings_raw(&layout.home_dir, contents);
        let (_, statuses) = list_and_collect(&mut approver, request_id).await;
        assert_eq!(
            status_of(&statuses, "sierra").state,
            expected,
            "host settings file {contents:?}"
        );
    }
}

#[tokio::test]
async fn an_empty_or_whitespace_only_file_through_the_read_tool_is_not_registered_and_garbage_is_unknown()
 {
    let layout = Layout::new("mcpreg-probe-blank");
    let (state_key, state_value) = state_env(&layout);
    let cases: [(&str, &str, McpRegistrationState); 3] = [
        ("victor", "not json", McpRegistrationState::Unknown),
        ("tango", "", McpRegistrationState::NotRegistered),
        ("uniform", " \t\r\n \n", McpRegistrationState::NotRegistered),
    ];
    for (profile, contents, _) in &cases {
        let home = layout.root.join(format!("{profile}-home"));
        write_json_settings_raw(&home, contents);
        let home = home.to_str().expect("UTF-8").to_string();
        write_profile(
            &layout.profiles_dir,
            profile,
            &mcp_cli_stub_profile_with_table(
                profile,
                "",
                &probed_file_table(&home),
                &[(state_key, state_value.as_str())],
            ),
        );
    }
    let h = Harness::start(
        "mcpreg-probe-blank",
        registration_config(&layout, Some(layout.bridge.clone())),
    )
    .await;
    let mut approver = approver_connection(&h).await;

    let (_, statuses) = list_and_collect(&mut approver, 50).await;
    for (profile, contents, expected) in &cases {
        assert_eq!(
            status_of(&statuses, profile).state,
            *expected,
            "settings file {contents:?} read through the read tool"
        );
    }
}

// =============================================================================
// No bridge: rows but no state. No profiles directory: unsupported.
// =============================================================================

#[tokio::test]
async fn without_a_bridge_rows_are_listed_with_placeholders_and_no_state_is_pushed() {
    let layout = Layout::new("mcpreg-no-bridge");
    let stub = stub_str();
    seed_registry(&layout.state_dir, &[]);
    write_profile(
        &layout.profiles_dir,
        "alpha",
        &lines_profile(&layout, "Alpha", &[]),
    );
    let h = Harness::start("mcpreg-no-bridge", registration_config(&layout, None)).await;
    let mut approver = approver_connection(&h).await;

    send_list(&mut approver, 31).await;
    let listed = expect_listed(read_response(&mut approver, REPLY_TIMEOUT).await, 31);
    assert_eq!(listed.bridge, None);
    assert_eq!(
        row_of(&listed.rows, "alpha").add,
        command(&[&stub, "mcp", "add", "yagura", "--", "{bridge}"], 0)
    );
    let unexpected: Option<DaemonMessage> =
        try_read_daemon_message(&mut approver, NEGATIVE_OBSERVATION).await;
    assert!(
        unexpected.is_none(),
        "no state is read without a bridge: {unexpected:?}"
    );

    // Control: the same profile with a bridge gets its push. The push is an
    // event, so it is waited for up to the reply ceiling.
    let with_bridge = Harness::start(
        "mcpreg-no-bridge-control",
        registration_config(&layout, Some(layout.bridge.clone())),
    )
    .await;
    let mut control = approver_connection(&with_bridge).await;
    send_list(&mut control, 32).await;
    expect_listed(read_response(&mut control, REPLY_TIMEOUT).await, 32);
    let push: Option<DaemonMessage> = try_read_daemon_message(&mut control, REPLY_TIMEOUT).await;
    assert!(
        matches!(push, Some(DaemonMessage::McpRegistrationStatus { .. })),
        "with a bridge the state push arrives: {push:?}"
    );
}

#[tokio::test]
async fn without_a_profiles_directory_the_listing_is_unsupported() {
    let layout = Layout::new("mcpreg-no-profiles");
    write_profile(
        &layout.profiles_dir,
        "alpha",
        &lines_profile(&layout, "Alpha", &[]),
    );
    // Control: with the directory, the same daemon lists.
    let with_dir = Harness::start(
        "mcpreg-with-profiles",
        registration_config(&layout, Some(layout.bridge.clone())),
    )
    .await;
    let mut control = approver_connection(&with_dir).await;
    send_list(&mut control, 41).await;
    expect_listed(read_response(&mut control, REPLY_TIMEOUT).await, 41);

    let config = DaemonConfig {
        profiles_dir: None,
        ..registration_config(&layout, Some(layout.bridge.clone()))
    };
    let h = Harness::start("mcpreg-no-profiles", config).await;
    let mut approver = approver_connection(&h).await;
    send_list(&mut approver, 42).await;
    expect_error(
        read_response(&mut approver, REPLY_TIMEOUT).await,
        "unsupported",
    );
}

// =============================================================================
// A named instance registers under its own name and carries its own
// environment.
// =============================================================================

#[tokio::test]
async fn a_named_instance_renders_its_own_name_and_environment_and_reads_its_own_name() {
    let layout = Layout::new("mcpreg-instance");
    let bridge = layout.bridge_str();
    let stub = stub_str();
    let (state_key, _) = state_env(&layout);
    let named_state = layout.root.join("named-state");
    let default_only_state = layout.root.join("default-only-state");
    seed_registry(&named_state, &[("yagura-dev", &bridge, &[])]);
    seed_registry(&default_only_state, &[("yagura", &bridge, &[])]);
    let named_state = named_state.to_str().expect("UTF-8").to_string();
    let default_only_state = default_only_state.to_str().expect("UTF-8").to_string();

    write_profile(
        &layout.profiles_dir,
        "sierra",
        &mcp_cli_stub_profile_with_table(
            "Sierra",
            "",
            LINES_TABLE,
            &[(state_key, named_state.as_str())],
        ),
    );
    write_profile(
        &layout.profiles_dir,
        "tango",
        &mcp_cli_stub_profile_with_table(
            "Tango",
            "",
            LINES_TABLE,
            &[(state_key, default_only_state.as_str())],
        ),
    );
    write_profile(
        &layout.profiles_dir,
        "uniform",
        &mcp_cli_stub_profile_with_table(
            "Uniform",
            "",
            &probed_lines_table(),
            &[(state_key, named_state.as_str())],
        ),
    );
    let config = DaemonConfig {
        instance_id: Some("dev".to_string()),
        ..registration_config(&layout, Some(layout.bridge.clone()))
    };
    let h = Harness::start("mcpreg-instance", config).await;
    let mut approver = approver_connection(&h).await;

    let (listed, statuses) = list_and_collect(&mut approver, 51).await;

    assert_eq!(
        row_of(&listed.rows, "sierra").add,
        command(
            &[
                &stub,
                "mcp",
                "add",
                "yagura-dev",
                "--env",
                "YAGURA_INSTANCE=dev",
                "--",
                &bridge,
            ],
            0
        )
    );
    assert_eq!(
        row_of(&listed.rows, "sierra").remove,
        command(&[&stub, "mcp", "remove", "yagura-dev"], 0)
    );
    assert_eq!(
        row_of(&listed.rows, "uniform").add,
        command(
            &[
                &stub,
                "mcp",
                "add",
                "yagura-dev",
                "--env",
                "YAGURA_INSTANCE=dev",
                "--env",
                "WSLENV=YAGURA_INSTANCE",
                "--",
                "{bridge}",
            ],
            0
        ),
        "a row forwarding the environment also carries the forwarded names"
    );

    assert_eq!(
        status_of(&statuses, "sierra").state,
        McpRegistrationState::Registered,
        "the status reads the instance's own registration name"
    );
    assert_eq!(
        status_of(&statuses, "tango").state,
        McpRegistrationState::NotRegistered,
        "the default instance's registration is not this instance's"
    );
}

// =============================================================================
// Concurrency: one listing at a time; row reads bounded and waited for.
// =============================================================================

#[tokio::test]
async fn a_second_listing_while_one_runs_is_resource_exhausted_and_the_slot_frees_afterwards() {
    let layout = Layout::new("mcpreg-one-listing");
    seed_registry(&layout.state_dir, &[]);
    // Every fixture call stays in flight until this file exists.
    let release = layout.root.join("release");
    let release_str = release.to_str().expect("UTF-8").to_string();
    write_profile(
        &layout.profiles_dir,
        "alpha",
        &lines_profile(
            &layout,
            "Alpha",
            &[("MCP_CLI_STUB_HOLD_UNTIL", release_str.as_str())],
        ),
    );
    let h = Harness::start(
        "mcpreg-one-listing",
        registration_config(&layout, Some(layout.bridge.clone())),
    )
    .await;
    let mut approver = approver_connection(&h).await;

    send_list(&mut approver, 61).await;
    expect_listed(read_response(&mut approver, REPLY_TIMEOUT).await, 61);
    send_list(&mut approver, 62).await;

    // The first listing's row read is held, so the second listing's answer
    // comes first.
    expect_error(
        read_response(&mut approver, REPLY_TIMEOUT).await,
        "resource_exhausted",
    );
    std::fs::write(&release, b"").expect("release the held fixture calls");
    match read_response(&mut approver, REPLY_TIMEOUT).await {
        DaemonMessage::McpRegistrationStatus { request_id, .. } => assert_eq!(request_id, 61),
        other => panic!("expected the first listing's push, got {other:?}"),
    }

    // Control: once the first listing has finished, a new one is admitted.
    let deadline = tokio::time::Instant::now() + REPLY_TIMEOUT;
    let mut request_id = 63;
    loop {
        send_list(&mut approver, request_id).await;
        let reply: DaemonMessage = read_response(&mut approver, REPLY_TIMEOUT).await;
        match reply {
            DaemonMessage::McpRegistrationsListed { .. } => {
                expect_listed(reply, request_id);
                break;
            }
            DaemonMessage::Error { ref code, .. }
                if code == "resource_exhausted" && tokio::time::Instant::now() < deadline =>
            {
                tokio::time::sleep(Duration::from_millis(100)).await;
                request_id += 1;
            }
            other => panic!("expected the slot to be freed, got {other:?}"),
        }
    }
}

/// One fixture invocation's start and finish, from its call log.
struct Call {
    started_at_ms: u128,
    finished_at_ms: u128,
}

fn read_calls(log: &Path) -> Vec<Call> {
    std::fs::read_to_string(log)
        .expect("the fixture logged its calls")
        .lines()
        .map(|line| {
            let record: serde_json::Value = serde_json::from_str(line).expect("a JSON log line");
            Call {
                started_at_ms: u128::from(record["started_at_ms"].as_u64().expect("start")),
                finished_at_ms: u128::from(record["finished_at_ms"].as_u64().expect("finish")),
            }
        })
        .collect()
}

/// The largest number of calls running at one instant.
fn peak_overlap(calls: &[Call]) -> usize {
    calls
        .iter()
        .map(|probe| {
            calls
                .iter()
                .filter(|call| {
                    call.started_at_ms <= probe.started_at_ms
                        && probe.started_at_ms < call.finished_at_ms
                })
                .count()
        })
        .max()
        .unwrap_or(0)
}

#[tokio::test]
async fn row_reads_never_exceed_the_configured_limit_and_every_row_is_still_pushed() {
    let layout = Layout::new("mcpreg-row-limit");
    seed_registry(&layout.state_dir, &[]);
    let ids = ["row-a", "row-b", "row-c", "row-d", "row-e"];
    // One log per row: concurrent appends from separate processes to one
    // file may interleave within a line.
    let logs: Vec<PathBuf> = ids
        .iter()
        .map(|id| layout.root.join(format!("stub-log-{id}.jsonl")))
        .collect();
    for (id, log) in ids.iter().zip(&logs) {
        let log_str = log.to_str().expect("UTF-8");
        write_profile(
            &layout.profiles_dir,
            id,
            &lines_profile(
                &layout,
                id,
                &[
                    ("MCP_CLI_STUB_DELAY_MS", "700"),
                    ("MCP_CLI_STUB_LOG", log_str),
                ],
            ),
        );
    }
    let limit: usize = 2;
    let config = DaemonConfig {
        mcp_registration_status_max_concurrent: limit,
        ..registration_config(&layout, Some(layout.bridge.clone()))
    };
    let h = Harness::start("mcpreg-row-limit", config).await;
    let mut approver = approver_connection(&h).await;

    let (_, statuses) = list_and_collect(&mut approver, 71).await;
    assert_eq!(
        statuses.len(),
        ids.len(),
        "row reads wait for a slot rather than being dropped"
    );

    let calls: Vec<Call> = logs.iter().flat_map(|log| read_calls(log)).collect();
    assert_eq!(calls.len(), ids.len());
    let peak = peak_overlap(&calls);
    assert!(
        peak <= limit,
        "at most {limit} row reads run at once, saw {peak}"
    );
    // Control: the log does observe overlap, so the bound above is not
    // trivially met by reads that never overlap.
    assert_eq!(
        peak, limit,
        "the reads use every slot the limit allows, saw {peak}"
    );
}

// =============================================================================
// The status output is never sent or logged.
// =============================================================================

const SECRET: &str = "status-output-secret-sentinel";

#[tokio::test]
#[traced_test]
async fn the_status_output_reaches_neither_a_push_nor_the_log() {
    let layout = Layout::new("mcpreg-no-leak");
    let bridge = layout.bridge_str();
    seed_registry(&layout.state_dir, &[("yagura", &bridge, &[SECRET])]);
    write_profile(
        &layout.profiles_dir,
        "victor",
        &lines_profile(&layout, "Victor", &[]),
    );
    let garbage = format!("{SECRET} is not a line this profile reads");
    write_profile(
        &layout.profiles_dir,
        "whiskey",
        &lines_profile(
            &layout,
            "Whiskey",
            &[
                ("MCP_CLI_STUB_FAIL_SUBCOMMAND", "get"),
                ("MCP_CLI_STUB_FAIL_EXIT_CODE", "0"),
                ("MCP_CLI_STUB_FAIL_STDOUT", garbage.as_str()),
                ("MCP_CLI_STUB_FAIL_STDERR", SECRET),
            ],
        ),
    );
    // A broken profile file whose name the daemon's own listing warns about:
    // the positive control that this case captures the daemon's log at all.
    std::fs::write(
        layout.profiles_dir.join("logcontrolsentinel.toml"),
        "schema = [unterminated",
    )
    .expect("write broken profile");

    let h = Harness::start(
        "mcpreg-no-leak",
        registration_config(&layout, Some(layout.bridge.clone())),
    )
    .await;
    let mut approver = approver_connection(&h).await;

    send_list(&mut approver, 81).await;
    let listed_reply: DaemonMessage = read_response(&mut approver, REPLY_TIMEOUT).await;
    let listed_text = String::from_utf8(yagura_protocol::encode_daemon_message(&listed_reply))
        .expect("frames are UTF-8");
    let listed = expect_listed(listed_reply, 81);
    assert!(!listed_text.contains(SECRET));

    let mut pushes: Vec<McpRegistrationStatusInfo> = Vec::new();
    while pushes.len() < listed.rows.len() {
        let push: DaemonMessage = read_response(&mut approver, REPLY_TIMEOUT).await;
        let text = String::from_utf8(yagura_protocol::encode_daemon_message(&push))
            .expect("frames are UTF-8");
        assert!(
            !text.contains(SECRET),
            "a status push must never carry the status output: {text}"
        );
        match push {
            DaemonMessage::McpRegistrationStatus { status, .. } => pushes.push(status),
            other => panic!("expected mcp_registration_status, got {other:?}"),
        }
    }

    let victor = pushes
        .iter()
        .find(|status| status.profile == "victor")
        .expect("victor pushed");
    assert_eq!(victor.state, McpRegistrationState::Registered);
    let whiskey = pushes
        .iter()
        .find(|status| status.profile == "whiskey")
        .expect("whiskey pushed");
    assert_eq!(whiskey.state, McpRegistrationState::Unknown);
    assert!(whiskey.reason.is_some());

    assert!(
        captured_log_contains("logcontrolsentinel"),
        "the daemon's own log must be captured in this case"
    );
    assert!(
        !captured_log_contains(SECRET),
        "the status output must never reach the daemon's log"
    );
}

/// Whether any captured log line, from any thread, contains `needle`.
///
/// `logs_contain` only matches events inside this test's own span, which a
/// read running on a blocking thread is not; the sentinels here are unique
/// to this case, so scanning every captured line is both wider and safe.
fn captured_log_contains(needle: &str) -> bool {
    let buffer = tracing_test::internal::global_buf()
        .lock()
        .expect("the captured log buffer is not poisoned");
    String::from_utf8_lossy(&buffer).contains(needle)
}

// =============================================================================
// A registration child runs in the home directory with the base environment
// plus the profile's own `[env]`.
// =============================================================================

#[tokio::test]
async fn a_registration_child_runs_in_the_configured_home() {
    let layout = Layout::new("mcpreg-cwd");
    let bridge = layout.bridge_str();
    // A relative state directory: the fixture resolves it against its own
    // working directory, so only a child started in `home_dir` finds this
    // registry.
    seed_registry(
        &layout.home_dir.join("relative-state"),
        &[("yagura", &bridge, &[])],
    );
    write_profile(
        &layout.profiles_dir,
        "xray",
        &mcp_cli_stub_profile_with_table(
            "Xray",
            "",
            LINES_TABLE,
            &[("MCP_CLI_STUB_STATE_DIR", "relative-state")],
        ),
    );
    let h = Harness::start(
        "mcpreg-cwd",
        registration_config(&layout, Some(layout.bridge.clone())),
    )
    .await;
    let mut approver = approver_connection(&h).await;
    let (_, statuses) = list_and_collect(&mut approver, 91).await;
    assert_eq!(
        status_of(&statuses, "xray").state,
        McpRegistrationState::Registered,
        "the child must run in the configured home"
    );

    // Control: the same profiles against another, empty home.
    let other_home = layout.root.join("other-home");
    std::fs::create_dir_all(&other_home).expect("create other home");
    let config = DaemonConfig {
        home_dir: Some(other_home),
        ..registration_config(&layout, Some(layout.bridge.clone()))
    };
    let other = Harness::start("mcpreg-cwd-control", config).await;
    let mut control = approver_connection(&other).await;
    let (_, statuses) = list_and_collect(&mut control, 92).await;
    assert_eq!(
        status_of(&statuses, "xray").state,
        McpRegistrationState::NotRegistered
    );
}

#[tokio::test]
async fn a_registration_child_sees_the_base_environment_under_the_profile_env() {
    let layout = Layout::new("mcpreg-env");
    let bridge = layout.bridge_str();
    let registered_state = layout.root.join("registered-state");
    let empty_state = layout.root.join("empty-state");
    seed_registry(&registered_state, &[("yagura", &bridge, &[])]);
    seed_registry(&empty_state, &[]);
    let registered_state = registered_state.to_str().expect("UTF-8").to_string();

    // Reads its state directory from the base environment alone.
    write_profile(
        &layout.profiles_dir,
        "yankee",
        &mcp_cli_stub_profile_with_table("Yankee", "", LINES_TABLE, &[]),
    );
    // Overrides the base environment's state directory with its own.
    write_profile(
        &layout.profiles_dir,
        "zulu",
        &mcp_cli_stub_profile_with_table(
            "Zulu",
            "",
            LINES_TABLE,
            &[("MCP_CLI_STUB_STATE_DIR", registered_state.as_str())],
        ),
    );
    let base_for = |state: &Path| -> ChildBaseEnv {
        ChildBaseEnv::from_pairs(vec![(
            OsString::from("MCP_CLI_STUB_STATE_DIR"),
            state.as_os_str().to_os_string(),
        )])
    };

    let config = DaemonConfig {
        child_base_env: base_for(Path::new(&registered_state)),
        ..registration_config(&layout, Some(layout.bridge.clone()))
    };
    let h = Harness::start("mcpreg-env", config).await;
    let mut approver = approver_connection(&h).await;
    let (_, statuses) = list_and_collect(&mut approver, 93).await;
    assert_eq!(
        status_of(&statuses, "yankee").state,
        McpRegistrationState::Registered,
        "the base environment reaches the child"
    );
    assert_eq!(
        status_of(&statuses, "zulu").state,
        McpRegistrationState::Registered
    );

    // Control: a base environment naming an empty registry changes the
    // profile without its own entry, but not the one whose `[env]` wins.
    let config = DaemonConfig {
        child_base_env: base_for(&empty_state),
        ..registration_config(&layout, Some(layout.bridge.clone()))
    };
    let other = Harness::start("mcpreg-env-control", config).await;
    let mut control = approver_connection(&other).await;
    let (_, statuses) = list_and_collect(&mut control, 94).await;
    assert_eq!(
        status_of(&statuses, "yankee").state,
        McpRegistrationState::NotRegistered
    );
    assert_eq!(
        status_of(&statuses, "zulu").state,
        McpRegistrationState::Registered,
        "the profile's own [env] is layered over the base environment"
    );
}
