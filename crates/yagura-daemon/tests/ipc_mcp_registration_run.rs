#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: the daemon IPC surface that registers, updates or
//! removes one profile's MCP registration -- `run_mcp_registration`,
//! answered by `mcp_registration_requested` and one `mcp_registration_result`
//! push.
//!
//! Every registration command spawns the `mcp_cli_stub` fixture binary (see
//! `src/bin/mcp_cli_stub.rs`), never a real vendor CLI. Every case points
//! `home_dir` at its own temporary directory, so no case reads or writes the
//! user's own home.

mod common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use common::{
    Harness, McpCliStubFixture, approver_base_config, echo_child_path, expect_error, identify,
    mcp_cli_stub_path, mcp_cli_stub_profile_source, mcp_cli_stub_profile_with_table,
    mcp_cli_stub_toml_literal, read_response, register_approver, registration_cwd_in, send,
    unique_temp_dir, write_profile,
};
use serde_json::{Value, json};
use tracing_test::traced_test;
use yagura_daemon::DaemonConfig;
use yagura_daemon::mcp_registration::{RenderError, STEP_OUTPUT_MAX_BYTES};
use yagura_daemon::profile::ProfileOs;
use yagura_protocol::{
    ClientMessage, DaemonMessage, McpRegistrationAction, McpRegistrationState,
    McpRegistrationStatusInfo, McpRegistrationStep,
};
use yagura_test_support::ipc::Connection;

// --- timing -------------------------------------------------------------------

/// How long any single reply or push may take before a case gives up. An
/// operation may run several child processes in sequence (probe, add, a
/// settings-file edit, a status re-read), so this is generous.
const REPLY_TIMEOUT: Duration = Duration::from_secs(20);

/// How long a case that must observe "no child ran" waits.
const NEGATIVE_OBSERVATION: Duration = Duration::from_millis(2000);

// --- fixture layout -------------------------------------------------------------

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

    fn home_dir_str(&self) -> String {
        self.home_dir
            .to_str()
            .expect("home dir is UTF-8")
            .to_string()
    }
}

fn registration_config(layout: &Layout, bridge: Option<PathBuf>) -> DaemonConfig {
    DaemonConfig {
        profiles_dir: Some(layout.profiles_dir.clone()),
        home_dir: Some(layout.home_dir.clone()),
        registration_cwd: registration_cwd_in(&layout.root),
        instance_id: None,
        mcp_bridge: bridge,
        mcp_registration_timeout: Duration::from_secs(20),
        mcp_registration_max_output_bytes: 1 << 20,
        mcp_registration_status_max_concurrent: 2,
        ..approver_base_config()
    }
}

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

/// The `[env]` pair pointing the fixture at `layout`'s state directory.
fn state_env(layout: &Layout) -> (&'static str, String) {
    ("MCP_CLI_STUB_STATE_DIR", layout.state_dir_str())
}

// --- registration tables -------------------------------------------------------

/// A host row whose status is read in the line form (`Command:` / `Scope:`),
/// with no settings-file edit.
const LINES_TABLE: &str = "[mcp_registration]\n\
add = [\"mcp\", \"add\", \"{server}\", \"{env_args}\", \"--\", \"{bridge}\"]\n\
remove = [\"mcp\", \"remove\", \"{server}\"]\n\
env_arg = [\"--env\", \"{env}\"]\n\
\n\
[mcp_registration.status]\n\
args = [\"mcp\", \"get\", \"{server}\"]\n\
format = \"lines\"\n\
command = \"Command:\"\n\
absent = \"No MCP server named\"\n";

/// [`LINES_TABLE`] with no `env_arg`: a table that cannot carry a named
/// instance's environment.
const NO_ENV_ARG_TABLE: &str = "[mcp_registration]\n\
add = [\"mcp\", \"add\", \"{server}\", \"--\", \"{bridge}\"]\n\
remove = [\"mcp\", \"remove\", \"{server}\"]\n\
\n\
[mcp_registration.status]\n\
args = [\"mcp\", \"get\", \"{server}\"]\n\
format = \"lines\"\n\
command = \"Command:\"\n\
absent = \"No MCP server named\"\n";

/// A home-relative TOML settings file a host row edits after `add`.
const HOST_TOML_FILE: &str = ".stub/config.toml";

/// A home-relative TOML settings file the wrong-file case declares but that never gets
/// written by the fixture, standing in for "the CLI wrote somewhere else".
const HOST_WRONG_TOML_FILE: &str = ".stub/wrong-config.toml";

/// A home-relative TOML settings file a foreign-environment (WSL) row edits
/// through its own `files.read` / `files.replace` tools.
const WSL_TOML_FILE: &str = ".stub/wsl-config.toml";

/// A home-relative TOML settings file the WSL absent-table case declares but
/// that is never created.
const WSL_MISSING_TOML_FILE: &str = ".stub/wsl-missing.toml";

/// A sibling file in the same directory as [`HOST_TOML_FILE`], used to prove
/// a registration edit never touches an unrelated file.
const SIBLING_FILE: &str = ".stub/sibling.txt";
const SIBLING_CONTENT: &str = "unrelated content, left alone\n";

/// A host row (JSON status, `toml_set`), the shape [`mcp_cli_stub_profile_source`]
/// already builds: `add` rewrites `[mcp_servers.<name>]` from scratch, then
/// the daemon's own `toml_set` step sets `tool_timeout_sec` in the same
/// table.
fn host_toml_set_profile(
    layout: &Layout,
    display_name: &str,
    extra_env: &[(&str, &str)],
) -> String {
    let fixture = McpCliStubFixture {
        state_dir: layout.state_dir.as_path(),
        toml_table_path: Some(HOST_TOML_FILE),
        on_duplicate: None,
        log_path: None,
        home_dir: Some(layout.home_dir.as_path()),
    };
    let mut source = mcp_cli_stub_profile_source(display_name, &fixture);
    // `mcp_cli_stub_profile_source` already writes its own `[env]` table
    // (with `MCP_CLI_STUB_STATE_DIR` and `MCP_CLI_STUB_TOML_TABLE_PATH`);
    // append any extra pairs the case needs (e.g. an injected failure), which
    // continue that same, still-open table.
    for (extra_key, extra_value) in extra_env {
        source.push_str(&format!("{extra_key} = '{extra_value}'\n"));
    }
    source
}

/// A row whose registration table has no `toml_set` at all (the lines-form
/// [`LINES_TABLE`]), for cases that only care about `add` / `remove`.
fn lines_profile(layout: &Layout, display_name: &str, extra_env: &[(&str, &str)]) -> String {
    let (key, value) = state_env(layout);
    let mut env: Vec<(&str, &str)> = vec![(key, value.as_str())];
    env.extend_from_slice(extra_env);
    mcp_cli_stub_profile_with_table(display_name, "", LINES_TABLE, &env)
}

/// [`NO_ENV_ARG_TABLE`] with the fixture wired in.
fn no_env_arg_profile(layout: &Layout, display_name: &str) -> String {
    let (key, value) = state_env(layout);
    mcp_cli_stub_profile_with_table(display_name, "", NO_ENV_ARG_TABLE, &[(key, value.as_str())])
}

/// A profile with a registration table but no `[command]` this test writes
/// to disk (used only for its id, never opened): stands in for "a profile
/// with no registration table at all" (the `invalid_argument` case).
fn plain_profile_source(display_name: &str) -> String {
    format!(
        "schema = 1\ndisplay_name = \"{display_name}\"\n\n[command]\ndefault = [{}]\n",
        mcp_cli_stub_toml_literal()
    )
}

/// A foreign-environment (WSL) row: `[host_paths]` converts the bridge path,
/// and a `toml_set` edit is carried out through `files.read` /
/// `files.replace` rather than a direct host read/write.
fn wsl_files_toml_set_table(home: &str, toml_set_file: &str) -> String {
    let stub = mcp_cli_stub_toml_literal();
    format!(
        "[host_paths]\n\
         convert = [{stub}, \"probe\", \"{{path}}\"]\n\
         \n\
         [mcp_registration]\n\
         group = \"WSL\"\n\
         add = [\"mcp\", \"add\", \"{{server}}\", \"{{env_args}}\", \"--\", \"{{bridge}}\"]\n\
         remove = [\"mcp\", \"remove\", \"{{server}}\"]\n\
         env_arg = [\"--env\", \"{{env}}\"]\n\
         files = {{ read = [{stub}, \"file-read\", '{home}/{{file}}'], replace = [{stub}, \
         \"file-replace\", '{home}/{{file}}'], absent_exit = 3 }}\n\
         \n\
         [mcp_registration.status]\n\
         args = [\"mcp\", \"get\", \"{{server}}\", \"--json\"]\n\
         format = \"json\"\n\
         command = \"/transport/command\"\n\
         absent = \"No MCP server named\"\n\
         \n\
         [mcp_registration.toml_set]\n\
         file = \"{toml_set_file}\"\n\
         table = [\"mcp_servers\", \"{{server}}\"]\n\
         values = {{ tool_timeout_sec = 86400 }}\n\
         confirm = {{ tool_timeout_sec = \"/tool_timeout_sec\" }}\n"
    )
}

// --- IPC round trips ------------------------------------------------------------

async fn approver_connection(h: &Harness) -> Connection {
    let mut client = h.connect().await;
    register_approver(&mut client).await;
    client
}

async fn send_run(
    client: &mut Connection,
    profile: &str,
    action: McpRegistrationAction,
    request_id: u64,
) {
    send(
        client,
        &ClientMessage::RunMcpRegistration {
            profile: profile.to_string(),
            action,
            request_id,
        },
    )
    .await;
}

/// The immediate answer to an operation: the request id, profile and action
/// it accepted.
struct Requested {
    #[allow(dead_code)]
    request_id: u64,
    profile: String,
    action: McpRegistrationAction,
}

/// The push carrying an operation's outcome.
#[derive(Debug)]
struct RunResult {
    #[allow(dead_code)]
    request_id: u64,
    #[allow(dead_code)]
    profile: String,
    #[allow(dead_code)]
    action: McpRegistrationAction,
    steps: Vec<McpRegistrationStep>,
    error: Option<String>,
    status: McpRegistrationStatusInfo,
}

fn expect_requested(reply: DaemonMessage, expected_request_id: u64) -> Requested {
    match reply {
        DaemonMessage::McpRegistrationRequested {
            request_id,
            profile,
            action,
        } => {
            assert_eq!(
                request_id, expected_request_id,
                "the answer must echo the request id it answers"
            );
            Requested {
                request_id,
                profile,
                action,
            }
        }
        other => panic!("expected mcp_registration_requested, got {other:?}"),
    }
}

fn expect_result(push: DaemonMessage, expected_request_id: u64) -> RunResult {
    match push {
        DaemonMessage::McpRegistrationResult {
            request_id,
            profile,
            action,
            steps,
            error,
            status,
        } => {
            assert_eq!(
                request_id, expected_request_id,
                "the result push must carry the request's own id"
            );
            RunResult {
                request_id,
                profile,
                action,
                steps,
                error,
                status,
            }
        }
        other => panic!("expected mcp_registration_result, got {other:?}"),
    }
}

/// Sends `run_mcp_registration` and reads both the immediate answer and the
/// result push that follows.
async fn run_and_collect(
    client: &mut Connection,
    profile: &str,
    action: McpRegistrationAction,
    request_id: u64,
) -> (Requested, RunResult) {
    send_run(client, profile, action, request_id).await;
    let requested = expect_requested(read_response(client, REPLY_TIMEOUT).await, request_id);
    let result = expect_result(read_response(client, REPLY_TIMEOUT).await, request_id);
    (requested, result)
}

/// The argv of every step, as `&str` slices, for a terser assertion.
fn step_argvs(steps: &[McpRegistrationStep]) -> Vec<Vec<&str>> {
    steps
        .iter()
        .map(|step| step.argv.iter().map(String::as_str).collect())
        .collect()
}

fn stub_str() -> String {
    mcp_cli_stub_path()
        .to_str()
        .expect("stub path is UTF-8")
        .to_string()
}

// =============================================================================
// Register is one step, and the fixture's own registry gains one entry.
// A named instance carries its environment in that one step.
// =============================================================================

#[tokio::test]
async fn register_runs_one_step_and_reaches_registered() {
    let layout = Layout::new("mcpreg-run-x1");
    let stub = stub_str();
    let bridge = layout.bridge_str();
    seed_registry(&layout.state_dir, &[]);
    write_profile(
        &layout.profiles_dir,
        "alpha",
        &lines_profile(&layout, "Alpha", &[]),
    );
    let h = Harness::start(
        "mcpreg-run-x1",
        registration_config(&layout, Some(layout.bridge.clone())),
    )
    .await;
    let mut approver = approver_connection(&h).await;

    let (requested, result) =
        run_and_collect(&mut approver, "alpha", McpRegistrationAction::Register, 1).await;
    assert_eq!(requested.profile, "alpha");
    assert_eq!(requested.action, McpRegistrationAction::Register);

    assert_eq!(
        step_argvs(&result.steps),
        vec![vec![
            stub.as_str(),
            "mcp",
            "add",
            "yagura",
            "--",
            bridge.as_str()
        ]]
    );
    assert_eq!(result.steps[0].exit_code, Some(0));
    assert!(result.error.is_none());
    assert_eq!(result.status.state, McpRegistrationState::Registered);

    let registry: serde_json::Value = serde_json::from_slice(
        &std::fs::read(layout.state_dir.join("registry.json")).expect("read fixture registry"),
    )
    .expect("registry is JSON");
    assert_eq!(
        registry.as_object().expect("registry is an object").len(),
        1,
        "the fixture's own registry gained exactly one entry: {registry}"
    );
}

#[tokio::test]
async fn register_for_a_named_instance_carries_its_environment_in_that_one_step() {
    let layout = Layout::new("mcpreg-run-x1-instance");
    let stub = stub_str();
    let bridge = layout.bridge_str();
    seed_registry(&layout.state_dir, &[]);
    write_profile(
        &layout.profiles_dir,
        "alpha",
        &lines_profile(&layout, "Alpha", &[]),
    );
    let config = DaemonConfig {
        instance_id: Some("dev".to_string()),
        ..registration_config(&layout, Some(layout.bridge.clone()))
    };
    let h = Harness::start("mcpreg-run-x1-instance", config).await;
    let mut approver = approver_connection(&h).await;

    let (_, result) =
        run_and_collect(&mut approver, "alpha", McpRegistrationAction::Register, 1).await;
    assert_eq!(
        step_argvs(&result.steps),
        vec![vec![
            stub.as_str(),
            "mcp",
            "add",
            "yagura-dev",
            "--env",
            "YAGURA_INSTANCE=dev",
            "--",
            bridge.as_str()
        ]]
    );
    assert_eq!(result.status.state, McpRegistrationState::Registered);
}

// =============================================================================
// A settings-file edit changes only its own key, leaves a sibling file
// alone, and (Unix) leaves the file's and its parent's mode alone.
// =============================================================================

#[tokio::test]
async fn register_edits_only_its_own_key_and_leaves_a_sibling_file_and_mode_alone() {
    let layout = Layout::new("mcpreg-run-x2");
    seed_registry(&layout.state_dir, &[]);
    let sibling = layout.home_dir.join(SIBLING_FILE);
    std::fs::create_dir_all(sibling.parent().expect("sibling has a parent"))
        .expect("create sibling dir");
    std::fs::write(&sibling, SIBLING_CONTENT).expect("write sibling file");

    #[cfg(unix)]
    let parent_dir = sibling
        .parent()
        .expect("sibling has a parent")
        .to_path_buf();
    #[cfg(unix)]
    let parent_mode_before = unix_mode(&parent_dir);
    // The settings file exists with an owner-only mode before the CLI's add
    // rewrites it in place, so the mode the edit must keep does not depend on
    // the test runner's umask.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let settings = layout.home_dir.join(HOST_TOML_FILE);
        std::fs::create_dir_all(settings.parent().expect("settings has a parent"))
            .expect("create settings dir");
        std::fs::write(&settings, "").expect("seed the settings file");
        std::fs::set_permissions(&settings, std::fs::Permissions::from_mode(0o600))
            .expect("make the settings file owner-only");
    }

    write_profile(
        &layout.profiles_dir,
        "bravo",
        &host_toml_set_profile(&layout, "Bravo", &[]),
    );
    let h = Harness::start(
        "mcpreg-run-x2",
        registration_config(&layout, Some(layout.bridge.clone())),
    )
    .await;
    let mut approver = approver_connection(&h).await;

    let (_, result) =
        run_and_collect(&mut approver, "bravo", McpRegistrationAction::Register, 1).await;
    assert!(result.error.is_none(), "{result:?}");
    assert_eq!(result.status.state, McpRegistrationState::Registered);

    // Exactly one step carries an empty argv: the daemon's own settings-file
    // edit, run directly (no `files` table on this host row).
    let file_steps: Vec<&McpRegistrationStep> = result
        .steps
        .iter()
        .filter(|step| step.argv.is_empty())
        .collect();
    assert_eq!(
        file_steps.len(),
        1,
        "exactly one step is the daemon's own file edit: {:?}",
        result.steps
    );
    let file_step = file_steps[0];
    assert_eq!(file_step.program_at, 0);
    assert_eq!(file_step.exit_code, None);
    assert!(
        !file_step.output.contains("86400"),
        "a file step's output never carries the value it wrote: {file_step:?}"
    );

    let edited = std::fs::read_to_string(layout.home_dir.join(HOST_TOML_FILE))
        .expect("read the edited settings file");
    let document: toml::Value = edited.parse().expect("the edited file parses as TOML");
    let timeout = document
        .get("mcp_servers")
        .and_then(|table| table.get("yagura"))
        .and_then(|table| table.get("tool_timeout_sec"))
        .expect("tool_timeout_sec was written");
    assert_eq!(timeout.as_integer(), Some(86400));

    assert_eq!(
        std::fs::read_to_string(&sibling).expect("read sibling file"),
        SIBLING_CONTENT,
        "a sibling file in the same directory is untouched"
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let edited_path = layout.home_dir.join(HOST_TOML_FILE);
        let mode = std::fs::metadata(&edited_path)
            .expect("stat the edited file")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            mode, 0o600,
            "the edited file's mode is unchanged from what the CLI's own write left it at"
        );
        assert_eq!(
            unix_mode(&parent_dir),
            parent_mode_before,
            "the parent directory's mode is never touched"
        );
    }
}

#[cfg(unix)]
fn unix_mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .expect("stat path")
        .permissions()
        .mode()
        & 0o777
}

// =============================================================================
// The settings file the profile names never gets the expected table;
// the operation fails without creating or touching that file, `add` is not
// undone, and the row ends up `differs` on the unset key.
// =============================================================================

#[tokio::test]
async fn register_fails_without_writing_when_the_declared_table_is_never_found() {
    let layout = Layout::new("mcpreg-run-x3");
    seed_registry(&layout.state_dir, &[]);
    let wrong_path = layout.home_dir.join(HOST_WRONG_TOML_FILE);
    std::fs::create_dir_all(wrong_path.parent().expect("has a parent"))
        .expect("create settings dir");
    let seeded = "[other]\nfoo = 1\n";
    std::fs::write(&wrong_path, seeded).expect("seed the wrong settings file");

    // `toml_set.file` names a settings file the fixture's own `add` never
    // writes to (it writes `HOST_TOML_FILE` instead, per its own
    // `MCP_CLI_STUB_TOML_TABLE_PATH`), modelling "the CLI reads a different
    // file than this row declares".
    let fixture = McpCliStubFixture {
        state_dir: layout.state_dir.as_path(),
        toml_table_path: Some(HOST_TOML_FILE),
        on_duplicate: None,
        log_path: None,
        home_dir: Some(layout.home_dir.as_path()),
    };
    let mut source = mcp_cli_stub_profile_source("Charlie", &fixture);
    source = source.replacen(
        &format!("file = '{HOST_TOML_FILE}'\n"),
        &format!("file = '{HOST_WRONG_TOML_FILE}'\n"),
        1,
    );
    write_profile(&layout.profiles_dir, "charlie", &source);
    let h = Harness::start(
        "mcpreg-run-x3",
        registration_config(&layout, Some(layout.bridge.clone())),
    )
    .await;
    let mut approver = approver_connection(&h).await;

    let (_, result) =
        run_and_collect(&mut approver, "charlie", McpRegistrationAction::Register, 1).await;

    let error = result
        .error
        .as_deref()
        .expect("the operation fails when the declared table is never found");
    assert!(
        error.contains("the table was not found"),
        "the fixed reason names the table: {error}"
    );
    assert!(
        error.contains(HOST_WRONG_TOML_FILE),
        "the fixed reason names the file, home-relative: {error}"
    );

    assert_eq!(
        std::fs::read_to_string(&wrong_path).expect("read the declared settings file"),
        seeded,
        "the declared settings file is byte-for-byte unchanged: nothing was written"
    );

    // `add` succeeded and is not undone: the row is registered except for
    // the one key the daemon could never confirm.
    assert_eq!(result.status.state, McpRegistrationState::Differs);
    assert_eq!(
        result.status.differs_in,
        vec!["tool_timeout_sec".to_string()]
    );
}

// =============================================================================
// A failing step stops the operation; no later step runs, and the
// failed step's own sanitized output is carried in the result.
// =============================================================================

#[tokio::test]
async fn a_failing_step_stops_the_operation_and_no_later_step_runs() {
    let layout = Layout::new("mcpreg-run-x4");
    seed_registry(&layout.state_dir, &[]);
    let log = layout.root.join("stub-log.jsonl");
    write_profile(
        &layout.profiles_dir,
        "delta",
        &lines_profile(
            &layout,
            "Delta",
            &[
                ("MCP_CLI_STUB_FAIL_SUBCOMMAND", "add"),
                ("MCP_CLI_STUB_FAIL_EXIT_CODE", "5"),
                ("MCP_CLI_STUB_FAIL_STDERR", "boom-x4-stderr"),
                ("MCP_CLI_STUB_LOG", log.to_str().expect("log path is UTF-8")),
            ],
        ),
    );
    let h = Harness::start(
        "mcpreg-run-x4",
        registration_config(&layout, Some(layout.bridge.clone())),
    )
    .await;
    let mut approver = approver_connection(&h).await;

    let (_, result) =
        run_and_collect(&mut approver, "delta", McpRegistrationAction::Register, 1).await;

    assert_eq!(result.steps.len(), 1, "no step after the failed one runs");
    assert_eq!(result.steps[0].exit_code, Some(5));
    assert!(result.steps[0].output.contains("boom-x4-stderr"));
    assert!(result.error.is_some());
    assert_eq!(result.status.state, McpRegistrationState::NotRegistered);

    let calls = std::fs::read_to_string(&log).expect("the fixture logged its calls");
    let add_calls = calls
        .lines()
        .filter(|line| line.contains("\"add\""))
        .count();
    assert_eq!(add_calls, 1, "add is attempted exactly once: {calls}");
}

// =============================================================================
// Update is remove-then-add; Remove is remove alone.
// =============================================================================

#[tokio::test]
async fn update_removes_then_adds_and_reaches_registered() {
    let layout = Layout::new("mcpreg-run-x5");
    let stub = stub_str();
    let bridge = layout.bridge_str();
    let other_bridge = layout.root.join("elsewhere").join("yagura-mcp");
    let other_bridge = other_bridge.to_str().expect("path is UTF-8").to_string();
    seed_registry(&layout.state_dir, &[("yagura", &other_bridge, &[])]);
    write_profile(
        &layout.profiles_dir,
        "echo",
        &lines_profile(&layout, "Echo", &[]),
    );
    let h = Harness::start(
        "mcpreg-run-x5",
        registration_config(&layout, Some(layout.bridge.clone())),
    )
    .await;
    let mut approver = approver_connection(&h).await;

    let (_, result) =
        run_and_collect(&mut approver, "echo", McpRegistrationAction::Update, 1).await;
    assert_eq!(
        step_argvs(&result.steps),
        vec![
            vec![stub.as_str(), "mcp", "remove", "yagura"],
            vec![stub.as_str(), "mcp", "add", "yagura", "--", bridge.as_str()],
        ]
    );
    assert!(result.steps.iter().all(|step| step.exit_code == Some(0)));
    assert!(result.error.is_none());
    assert_eq!(result.status.state, McpRegistrationState::Registered);
}

#[tokio::test]
async fn remove_runs_remove_alone_and_reaches_not_registered() {
    let layout = Layout::new("mcpreg-run-x6");
    let stub = stub_str();
    let bridge = layout.bridge_str();
    seed_registry(&layout.state_dir, &[("yagura", &bridge, &[])]);
    write_profile(
        &layout.profiles_dir,
        "foxtrot",
        &lines_profile(&layout, "Foxtrot", &[]),
    );
    let h = Harness::start(
        "mcpreg-run-x6",
        registration_config(&layout, Some(layout.bridge.clone())),
    )
    .await;
    let mut approver = approver_connection(&h).await;

    let (_, result) =
        run_and_collect(&mut approver, "foxtrot", McpRegistrationAction::Remove, 1).await;
    assert_eq!(
        step_argvs(&result.steps),
        vec![vec![stub.as_str(), "mcp", "remove", "yagura"]]
    );
    assert_eq!(result.steps[0].exit_code, Some(0));
    assert!(result.error.is_none());
    assert_eq!(result.status.state, McpRegistrationState::NotRegistered);
}

#[tokio::test]
async fn remove_of_a_settings_edit_that_owns_no_entries_runs_remove_alone() {
    let layout = Layout::new("mcpreg-run-no-entries");
    let stub = stub_str();
    let bridge = layout.bridge_str();
    seed_registry(&layout.state_dir, &[("yagura", &bridge, &[])]);
    write_profile(
        &layout.profiles_dir,
        "golf",
        &host_toml_set_profile(&layout, "Golf", &[]),
    );
    let h = Harness::start(
        "mcpreg-run-no-entries",
        registration_config(&layout, Some(layout.bridge.clone())),
    )
    .await;
    let mut approver = approver_connection(&h).await;

    let (_, result) =
        run_and_collect(&mut approver, "golf", McpRegistrationAction::Remove, 1).await;
    assert_eq!(
        step_argvs(&result.steps),
        vec![vec![stub.as_str(), "mcp", "remove", "yagura"]],
        "a toml_set without entries has nothing of its own to take out: {:?}",
        result.steps
    );
    assert!(result.error.is_none());
}

#[tokio::test]
async fn a_host_settings_edit_step_names_the_key_the_table_and_the_file() {
    let layout = Layout::new("mcpreg-run-describe");
    seed_registry(&layout.state_dir, &[]);
    write_profile(
        &layout.profiles_dir,
        "hotel",
        &host_toml_set_profile(&layout, "Hotel", &[]),
    );
    let h = Harness::start(
        "mcpreg-run-describe",
        registration_config(&layout, Some(layout.bridge.clone())),
    )
    .await;
    let mut approver = approver_connection(&h).await;

    let (_, result) =
        run_and_collect(&mut approver, "hotel", McpRegistrationAction::Register, 1).await;
    assert!(result.error.is_none(), "{result:?}");
    let file_step = result
        .steps
        .iter()
        .find(|step| step.argv.is_empty())
        .expect("the daemon's own file edit is one of the steps");
    assert_eq!(
        file_step.output,
        format!("set tool_timeout_sec in [mcp_servers.yagura] of {HOST_TOML_FILE}")
    );
}

/// The steps of a foreign row's `Register` that finds its settings file
/// absent of the value: probe the bridge, add, read the file, replace it.
fn foreign_register_steps<'a>(
    stub: &'a str,
    bridge: &'a str,
    probed: &'a str,
    file_path: &'a str,
) -> Vec<Vec<&'a str>> {
    vec![
        vec![stub, "probe", bridge],
        vec![stub, "mcp", "add", "yagura", "--", probed],
        vec![stub, "file-read", file_path],
        vec![stub, "file-replace", file_path],
    ]
}

#[tokio::test]
async fn a_foreign_row_naming_no_daemon_probes_only_its_bridge_even_with_a_report_folder() {
    let layout = Layout::new("mcpreg-run-no-daemon-probe");
    let stub = stub_str();
    let bridge = layout.bridge_str();
    let home = layout.home_dir_str();
    seed_registry(&layout.state_dir, &[]);
    let (state_key, state_value) = state_env(&layout);
    write_profile(
        &layout.profiles_dir,
        "india",
        &mcp_cli_stub_profile_with_table(
            "India",
            "",
            &wsl_files_toml_set_table(&home, WSL_TOML_FILE),
            &[
                (state_key, state_value.as_str()),
                ("MCP_CLI_STUB_TOML_TABLE_PATH", WSL_TOML_FILE),
                ("HOME", home.as_str()),
                ("USERPROFILE", home.as_str()),
            ],
        ),
    );
    // A report folder gives the daemon a hook path; a row whose settings never
    // name the daemon must still not convert it.
    let config = DaemonConfig {
        report_dir: Some(layout.root.join("report")),
        ..registration_config(&layout, Some(layout.bridge.clone()))
    };
    let h = Harness::start("mcpreg-run-no-daemon-probe", config).await;
    let mut approver = approver_connection(&h).await;

    let (_, result) =
        run_and_collect(&mut approver, "india", McpRegistrationAction::Register, 1).await;
    assert!(result.error.is_none(), "{result:?}");
    let probed = format!("/probed{}", bridge.replace('\\', "/"));
    let file_path = format!("{home}/{WSL_TOML_FILE}");
    assert_eq!(
        step_argvs(&result.steps),
        foreign_register_steps(&stub, &bridge, &probed, &file_path)
    );
}

#[tokio::test]
async fn a_foreign_row_whose_settings_file_already_holds_the_value_is_not_rewritten() {
    const HELD: &str = "[mcp_servers.yagura]\ntool_timeout_sec = 86400\n";
    let layout = Layout::new("mcpreg-run-unchanged-file");
    let stub = stub_str();
    let bridge = layout.bridge_str();
    let home = layout.home_dir_str();
    seed_registry(&layout.state_dir, &[]);
    let settings = layout.home_dir.join(WSL_TOML_FILE);
    std::fs::create_dir_all(settings.parent().expect("settings has a parent"))
        .expect("create settings dir");
    std::fs::write(&settings, HELD).expect("seed the settings file");
    // Without the stub's own TOML path its `add` leaves the settings file alone,
    // so the daemon's edit finds its value already in place.
    let (state_key, state_value) = state_env(&layout);
    write_profile(
        &layout.profiles_dir,
        "juliet",
        &mcp_cli_stub_profile_with_table(
            "Juliet",
            "",
            &wsl_files_toml_set_table(&home, WSL_TOML_FILE),
            &[(state_key, state_value.as_str())],
        ),
    );
    let h = Harness::start(
        "mcpreg-run-unchanged-file",
        registration_config(&layout, Some(layout.bridge.clone())),
    )
    .await;
    let mut approver = approver_connection(&h).await;

    let (_, result) =
        run_and_collect(&mut approver, "juliet", McpRegistrationAction::Register, 1).await;
    let probed = format!("/probed{}", bridge.replace('\\', "/"));
    let file_path = format!("{home}/{WSL_TOML_FILE}");
    let mut expected = foreign_register_steps(&stub, &bridge, &probed, &file_path);
    expected.pop();
    assert_eq!(
        step_argvs(&result.steps),
        expected,
        "a file that already holds the value is read but never replaced: {result:?}"
    );
    assert_eq!(
        std::fs::read_to_string(&settings).expect("read the settings file"),
        HELD
    );
}

// =============================================================================
// A foreign-environment (WSL) row edits its settings file through
// `files.read` / `files.replace` rather than a direct host write, and an
// absent table there fails the same way a host row's does.
// =============================================================================

#[tokio::test]
async fn a_foreign_row_edits_its_settings_file_through_its_own_read_and_replace_tools() {
    let layout = Layout::new("mcpreg-run-x7");
    let stub = stub_str();
    let bridge = layout.bridge_str();
    let home = layout.home_dir_str();
    seed_registry(&layout.state_dir, &[]);
    let (state_key, state_value) = state_env(&layout);
    write_profile(
        &layout.profiles_dir,
        "golf",
        &mcp_cli_stub_profile_with_table(
            "Golf",
            "",
            &wsl_files_toml_set_table(&home, WSL_TOML_FILE),
            &[
                (state_key, state_value.as_str()),
                ("MCP_CLI_STUB_TOML_TABLE_PATH", WSL_TOML_FILE),
                ("HOME", home.as_str()),
                ("USERPROFILE", home.as_str()),
            ],
        ),
    );
    let h = Harness::start(
        "mcpreg-run-x7",
        registration_config(&layout, Some(layout.bridge.clone())),
    )
    .await;
    let mut approver = approver_connection(&h).await;

    let (_, result) =
        run_and_collect(&mut approver, "golf", McpRegistrationAction::Register, 1).await;
    assert!(result.error.is_none(), "{result:?}");
    assert_eq!(result.status.state, McpRegistrationState::Registered);

    let probed = format!("/probed{}", bridge.replace('\\', "/"));
    let file_path = format!("{home}/{WSL_TOML_FILE}");
    let read_argv = vec![stub.as_str(), "file-read", file_path.as_str()];
    let replace_argv = vec![stub.as_str(), "file-replace", file_path.as_str()];
    assert_eq!(
        step_argvs(&result.steps),
        vec![
            vec![stub.as_str(), "probe", bridge.as_str()],
            vec![stub.as_str(), "mcp", "add", "yagura", "--", probed.as_str()],
            read_argv,
            replace_argv,
        ]
    );

    // Only the probe and add steps carry visible output; the file steps
    // never carry the file's content.
    assert!(!result.steps[0].output.is_empty());
    assert!(!result.steps[1].output.is_empty());
    assert!(result.steps[2].output.is_empty());
    assert!(result.steps[3].output.is_empty());

    let edited =
        std::fs::read_to_string(layout.home_dir.join(WSL_TOML_FILE)).expect("read edited file");
    let document: toml::Value = edited.parse().expect("the edited file parses as TOML");
    assert_eq!(
        document
            .get("mcp_servers")
            .and_then(|table| table.get("yagura"))
            .and_then(|table| table.get("tool_timeout_sec"))
            .and_then(toml::Value::as_integer),
        Some(86400)
    );
}

#[tokio::test]
async fn a_foreign_row_fails_the_same_way_a_host_row_does_when_its_table_is_never_found() {
    let layout = Layout::new("mcpreg-run-x7-absent");
    let home = layout.home_dir_str();
    seed_registry(&layout.state_dir, &[]);
    let (state_key, state_value) = state_env(&layout);
    write_profile(
        &layout.profiles_dir,
        "hotel",
        &mcp_cli_stub_profile_with_table(
            "Hotel",
            "",
            &wsl_files_toml_set_table(&home, WSL_MISSING_TOML_FILE),
            &[(state_key, state_value.as_str())],
        ),
    );
    let h = Harness::start(
        "mcpreg-run-x7-absent",
        registration_config(&layout, Some(layout.bridge.clone())),
    )
    .await;
    let mut approver = approver_connection(&h).await;

    let (_, result) =
        run_and_collect(&mut approver, "hotel", McpRegistrationAction::Register, 1).await;

    let error = result
        .error
        .as_deref()
        .expect("the absent-exit read fails the operation");
    assert!(error.contains(WSL_MISSING_TOML_FILE), "{error}");
    assert!(
        !layout.home_dir.join(WSL_MISSING_TOML_FILE).exists(),
        "nothing was created at the declared, never-written file"
    );
    assert_eq!(result.status.state, McpRegistrationState::Differs);
    assert_eq!(
        result.status.differs_in,
        vec!["tool_timeout_sec".to_string()]
    );
}

// =============================================================================
// A row with no `env_arg` cannot carry a named instance's environment;
// Register / Update fail before running any step, but Remove still runs.
// =============================================================================

#[tokio::test]
async fn a_row_that_cannot_carry_the_instance_fails_register_and_update_before_any_step_but_removes()
 {
    let layout = Layout::new("mcpreg-run-x8");
    seed_registry(&layout.state_dir, &[]);
    write_profile(
        &layout.profiles_dir,
        "india",
        &no_env_arg_profile(&layout, "India"),
    );
    let config = DaemonConfig {
        instance_id: Some("dev".to_string()),
        ..registration_config(&layout, Some(layout.bridge.clone()))
    };
    let h = Harness::start("mcpreg-run-x8", config).await;
    let mut approver = approver_connection(&h).await;

    let (_, register_result) =
        run_and_collect(&mut approver, "india", McpRegistrationAction::Register, 1).await;
    assert!(
        register_result.steps.is_empty(),
        "no step runs before the instance-carrying check fails: {:?}",
        register_result.steps
    );
    assert!(
        register_result
            .error
            .as_deref()
            .expect("register fails")
            .contains("cannot carry the instance name")
    );

    let (_, update_result) =
        run_and_collect(&mut approver, "india", McpRegistrationAction::Update, 2).await;
    assert!(update_result.error.is_some());

    let (_, remove_result) =
        run_and_collect(&mut approver, "india", McpRegistrationAction::Remove, 3).await;
    assert!(remove_result.error.is_none(), "{remove_result:?}");
    assert_eq!(remove_result.steps.len(), 1);
}

// =============================================================================
// refusals. Only an approver may run an operation; an unknown or
// table-less profile is `invalid_argument`; a second change while one runs
// is `resource_exhausted`; no bridge is `invalid_state` for Register/Update.
// =============================================================================

#[tokio::test]
async fn a_window_connection_and_an_unidentified_connection_are_refused_and_start_no_child() {
    let layout = Layout::new("mcpreg-run-x9-auth");
    let log = layout.root.join("stub-log.jsonl");
    seed_registry(&layout.state_dir, &[]);
    write_profile(
        &layout.profiles_dir,
        "juliet",
        &lines_profile(
            &layout,
            "Juliet",
            &[("MCP_CLI_STUB_LOG", log.to_str().expect("UTF-8"))],
        ),
    );
    let h = Harness::start(
        "mcpreg-run-x9-auth",
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
    send_run(
        &mut window_client,
        "juliet",
        McpRegistrationAction::Register,
        1,
    )
    .await;
    expect_error(
        read_response(&mut window_client, REPLY_TIMEOUT).await,
        "auth_failed",
    );

    let mut unidentified = h.connect().await;
    send_run(
        &mut unidentified,
        "juliet",
        McpRegistrationAction::Register,
        2,
    )
    .await;
    expect_error(
        read_response(&mut unidentified, REPLY_TIMEOUT).await,
        "auth_failed",
    );

    tokio::time::sleep(NEGATIVE_OBSERVATION).await;
    assert!(!log.exists(), "a refused operation must not start a child");

    h.registry.terminate_all().await;
}

#[tokio::test]
async fn an_unknown_or_table_less_profile_is_invalid_argument() {
    let layout = Layout::new("mcpreg-run-x9-unknown");
    seed_registry(&layout.state_dir, &[]);
    write_profile(&layout.profiles_dir, "kilo", &plain_profile_source("Kilo"));
    let h = Harness::start(
        "mcpreg-run-x9-unknown",
        registration_config(&layout, Some(layout.bridge.clone())),
    )
    .await;
    let mut approver = approver_connection(&h).await;

    send_run(
        &mut approver,
        "does-not-exist",
        McpRegistrationAction::Register,
        1,
    )
    .await;
    expect_error(
        read_response(&mut approver, REPLY_TIMEOUT).await,
        "invalid_argument",
    );

    send_run(&mut approver, "kilo", McpRegistrationAction::Register, 2).await;
    expect_error(
        read_response(&mut approver, REPLY_TIMEOUT).await,
        "invalid_argument",
    );
}

#[tokio::test]
async fn a_second_change_while_one_runs_is_resource_exhausted_and_the_slot_frees_afterwards() {
    let layout = Layout::new("mcpreg-run-x9-concurrent");
    seed_registry(&layout.state_dir, &[]);
    // Every fixture call stays in flight until this file exists.
    let release = layout.root.join("release");
    let release_str = release.to_str().expect("UTF-8").to_string();
    write_profile(
        &layout.profiles_dir,
        "lima",
        &lines_profile(
            &layout,
            "Lima",
            &[("MCP_CLI_STUB_HOLD_UNTIL", release_str.as_str())],
        ),
    );
    let h = Harness::start(
        "mcpreg-run-x9-concurrent",
        registration_config(&layout, Some(layout.bridge.clone())),
    )
    .await;
    let mut approver = approver_connection(&h).await;

    send_run(&mut approver, "lima", McpRegistrationAction::Register, 1).await;
    expect_requested(read_response(&mut approver, REPLY_TIMEOUT).await, 1);
    send_run(&mut approver, "lima", McpRegistrationAction::Register, 2).await;

    // The first operation is held, so the second one's answer comes first.
    expect_error(
        read_response(&mut approver, REPLY_TIMEOUT).await,
        "resource_exhausted",
    );
    std::fs::write(&release, b"").expect("release the held fixture calls");
    let _ = expect_result(read_response(&mut approver, REPLY_TIMEOUT).await, 1);

    let deadline = tokio::time::Instant::now() + REPLY_TIMEOUT;
    let mut request_id = 3;
    loop {
        send_run(
            &mut approver,
            "lima",
            McpRegistrationAction::Register,
            request_id,
        )
        .await;
        let reply: DaemonMessage = read_response(&mut approver, REPLY_TIMEOUT).await;
        match reply {
            DaemonMessage::McpRegistrationRequested { .. } => {
                expect_requested(reply, request_id);
                let _ = expect_result(
                    read_response(&mut approver, REPLY_TIMEOUT).await,
                    request_id,
                );
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

#[tokio::test]
async fn no_bridge_makes_register_and_update_invalid_state() {
    let layout = Layout::new("mcpreg-run-x9-no-bridge");
    seed_registry(&layout.state_dir, &[]);
    write_profile(
        &layout.profiles_dir,
        "mike",
        &lines_profile(&layout, "Mike", &[]),
    );
    let h = Harness::start(
        "mcpreg-run-x9-no-bridge",
        registration_config(&layout, None),
    )
    .await;
    let mut approver = approver_connection(&h).await;

    send_run(&mut approver, "mike", McpRegistrationAction::Register, 1).await;
    expect_error(
        read_response(&mut approver, REPLY_TIMEOUT).await,
        "invalid_state",
    );

    send_run(&mut approver, "mike", McpRegistrationAction::Update, 2).await;
    expect_error(
        read_response(&mut approver, REPLY_TIMEOUT).await,
        "invalid_state",
    );
}

// =============================================================================
// A step's own output is sanitized and bounded; a settings-file step
// carries none of it; none of it reaches the daemon's log.
// =============================================================================

const SECRET: &str = "run-output-secret-sentinel";

#[tokio::test]
#[traced_test]
async fn a_steps_output_is_sanitized_and_bounded_and_never_reaches_the_log() {
    let layout = Layout::new("mcpreg-run-x10");
    seed_registry(&layout.state_dir, &[]);
    let ansi_and_bidi = format!("\u{1b}[31m{SECRET}\u{202e}\u{1b}[0m");
    write_profile(
        &layout.profiles_dir,
        "november",
        &lines_profile(
            &layout,
            "November",
            &[
                ("MCP_CLI_STUB_FAIL_SUBCOMMAND", "add"),
                ("MCP_CLI_STUB_FAIL_EXIT_CODE", "0"),
                ("MCP_CLI_STUB_FAIL_STDOUT", &ansi_and_bidi),
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
        "mcpreg-run-x10",
        registration_config(&layout, Some(layout.bridge.clone())),
    )
    .await;
    let mut approver = approver_connection(&h).await;

    send_run(
        &mut approver,
        "november",
        McpRegistrationAction::Register,
        1,
    )
    .await;
    expect_requested(read_response(&mut approver, REPLY_TIMEOUT).await, 1);
    let result_push: DaemonMessage = read_response(&mut approver, REPLY_TIMEOUT).await;
    let result = expect_result(result_push, 1);

    // The add step's own output is shown to the caller who ran the
    // operation (unlike a status read's output, which never leaves the
    // daemon): sanitizing strips ANSI/bidi, but the visible text -- the
    // sentinel here -- still comes through.
    let step = &result.steps[0];
    assert!(
        !step.output.contains('\u{1b}'),
        "ANSI escapes are stripped: {:?}",
        step.output
    );
    assert!(
        !step.output.contains('\u{202e}'),
        "bidi control characters are stripped: {:?}",
        step.output
    );
    assert!(step.output.len() <= STEP_OUTPUT_MAX_BYTES);
    assert!(
        step.output.contains(SECRET),
        "sanitizing strips control characters, not the visible text: {:?}",
        step.output
    );

    // `list_profiles` reads every profile at least once at startup, in an
    // unspecified order: trigger the positive control's warn again, from
    // this connection, rather than depending on that first pass.
    send(&mut approver, &ClientMessage::ListProfiles).await;
    let _ = read_response(&mut approver, REPLY_TIMEOUT).await;

    assert!(
        captured_log_contains("logcontrolsentinel"),
        "the daemon's own log must be captured in this case"
    );
    assert!(
        !captured_log_contains(SECRET),
        "the operation's own output must never reach the daemon's log, even though it is \
         shown to the caller"
    );
}

/// Whether any captured log line, from any thread, contains `needle`.
///
/// `logs_contain` only matches events inside this test's own span, which a
/// read running on a blocking thread is not; the sentinel here is unique to
/// this case, so scanning every captured line is both wider and safe.
fn captured_log_contains(needle: &str) -> bool {
    let buffer = tracing_test::internal::global_buf()
        .lock()
        .expect("the captured log buffer is not poisoned");
    String::from_utf8_lossy(&buffer).contains(needle)
}

// =============================================================================
// The settings-file edit is written, but the CLI's own status still
// does not report the value (confirm disagrees): the operation fails.
// =============================================================================

#[tokio::test]
async fn register_fails_when_the_cli_does_not_report_the_value_that_was_written() {
    let layout = Layout::new("mcpreg-run-x11");
    seed_registry(&layout.state_dir, &[]);
    // The declared file has the table, so the daemon's own edit succeeds;
    // the fixture's `mcp get --json` reads `HOST_TOML_FILE` instead, which
    // never carries the key.
    let declared_path = layout.home_dir.join(HOST_WRONG_TOML_FILE);
    std::fs::create_dir_all(declared_path.parent().expect("has a parent"))
        .expect("create settings dir");
    std::fs::write(&declared_path, "[mcp_servers.yagura]\n").expect("seed the declared file");

    let fixture = McpCliStubFixture {
        state_dir: layout.state_dir.as_path(),
        toml_table_path: Some(HOST_TOML_FILE),
        on_duplicate: None,
        log_path: None,
        home_dir: Some(layout.home_dir.as_path()),
    };
    let source = mcp_cli_stub_profile_source("Oscar", &fixture).replacen(
        &format!("file = '{HOST_TOML_FILE}'\n"),
        &format!("file = '{HOST_WRONG_TOML_FILE}'\n"),
        1,
    );
    assert!(
        source.contains(HOST_WRONG_TOML_FILE),
        "the fixture's toml_set must name the declared file: {source}"
    );
    write_profile(&layout.profiles_dir, "oscar", &source);
    let h = Harness::start(
        "mcpreg-run-x11",
        registration_config(&layout, Some(layout.bridge.clone())),
    )
    .await;
    let mut approver = approver_connection(&h).await;

    let (_, result) =
        run_and_collect(&mut approver, "oscar", McpRegistrationAction::Register, 1).await;

    let written: toml::Value = std::fs::read_to_string(&declared_path)
        .expect("read the declared file")
        .parse()
        .expect("the declared file parses as TOML");
    assert_eq!(
        written
            .get("mcp_servers")
            .and_then(|table| table.get("yagura"))
            .and_then(|table| table.get("tool_timeout_sec"))
            .and_then(toml::Value::as_integer),
        Some(86400),
        "the daemon's own edit was written, so this is not the table-not-found failure"
    );
    assert_eq!(result.status.state, McpRegistrationState::Differs);
    assert_eq!(
        result.status.differs_in,
        vec!["tool_timeout_sec".to_string()]
    );
    let error = result
        .error
        .as_deref()
        .expect("a value the CLI does not report back fails the operation");
    assert!(
        !error.contains("the table was not found"),
        "the failure is the confirm mismatch, not a missing table: {error}"
    );
}

// =============================================================================
// (Windows) a registration child is started through the same program
// resolution a window uses -- a `.cmd` shim runs through the command
// interpreter, and an argument the interpreter would reinterpret is refused
// before anything starts.
// =============================================================================

/// Writes `<dir>/stubcli.cmd`: appends one line to `marker` whenever it runs,
/// then forwards every argument to the `mcp_cli_stub` fixture.
#[cfg(windows)]
fn write_stub_shim(dir: &Path, marker: &Path) {
    let content = format!(
        "@echo off\r\necho called>>\"{}\"\r\n\"{}\" %*\r\nexit /b %ERRORLEVEL%\r\n",
        marker.display(),
        stub_str()
    );
    std::fs::write(dir.join("stubcli.cmd"), content).expect("write the shim fixture");
}

/// A lines-form row whose `[command]` names `program` (a shim on `PATH`)
/// rather than the fixture's absolute path.
#[cfg(windows)]
fn shim_profile(
    layout: &Layout,
    display_name: &str,
    program: &str,
    env: &[(&str, &str)],
) -> String {
    let (key, value) = state_env(layout);
    let mut all_env: Vec<(&str, &str)> = vec![(key, value.as_str())];
    all_env.extend_from_slice(env);
    let source = mcp_cli_stub_profile_with_table(display_name, "", LINES_TABLE, &all_env);
    let direct = format!("default = [{}]", mcp_cli_stub_toml_literal());
    assert!(
        source.contains(&direct),
        "unexpected profile shape: {source}"
    );
    source.replacen(&direct, &format!("windows = [\"{program}\"]"), 1)
}

#[cfg(windows)]
fn shim_lookup(bin: &Path) -> yagura_os::program::ProgramLookup {
    yagura_os::program::ProgramLookup {
        path_dirs: vec![bin.to_path_buf()],
        extra_bin_dirs: Vec::new(),
        windows_comspec: yagura_os::program::ProgramLookup::from_env().windows_comspec,
    }
}

#[cfg(windows)]
#[tokio::test]
async fn register_starts_a_cmd_shim_cli_through_the_command_interpreter() {
    let layout = Layout::new("mcpreg-run-x12-shim");
    seed_registry(&layout.state_dir, &[]);
    let bin = layout.root.join("bin");
    std::fs::create_dir_all(&bin).expect("create shim dir");
    let marker = layout.root.join("shim-calls.txt");
    write_stub_shim(&bin, &marker);
    write_profile(
        &layout.profiles_dir,
        "papa",
        &shim_profile(&layout, "Papa", "stubcli", &[]),
    );
    let config = DaemonConfig {
        program_lookup: shim_lookup(&bin),
        ..registration_config(&layout, Some(layout.bridge.clone()))
    };
    let h = Harness::start("mcpreg-run-x12-shim", config).await;
    let mut approver = approver_connection(&h).await;

    let (_, result) =
        run_and_collect(&mut approver, "papa", McpRegistrationAction::Register, 1).await;

    assert!(
        result.error.is_none(),
        "a CLI installed as a .cmd shim registers: {result:?}"
    );
    assert_eq!(result.steps.len(), 1, "{result:?}");
    assert_eq!(result.steps[0].exit_code, Some(0), "{result:?}");
    assert_eq!(result.status.state, McpRegistrationState::Registered);
    assert!(marker.exists(), "the shim itself ran");
    let registry: serde_json::Value = serde_json::from_slice(
        &std::fs::read(layout.state_dir.join("registry.json")).expect("read fixture registry"),
    )
    .expect("registry is JSON");
    assert_eq!(
        registry.as_object().expect("registry is an object").len(),
        1,
        "the fixture's own registry gained exactly one entry: {registry}"
    );
}

#[cfg(windows)]
#[tokio::test]
async fn register_refuses_a_metacharacter_argument_for_a_cmd_shim_before_anything_starts() {
    let layout = Layout::new("mcpreg-run-x12-metachar");
    seed_registry(&layout.state_dir, &[]);
    let bin = layout.root.join("bin");
    std::fs::create_dir_all(&bin).expect("create shim dir");
    let marker = layout.root.join("shim-calls.txt");
    write_stub_shim(&bin, &marker);
    // The shim is also on the child's own `PATH`, so a launch that skipped
    // the daemon's program resolution would still find and run it.
    let mut child_path = std::ffi::OsString::from(bin.as_os_str());
    if let Some(inherited) = std::env::var_os("PATH") {
        child_path.push(";");
        child_path.push(inherited);
    }
    let child_path = child_path.into_string().expect("PATH is UTF-8");
    write_profile(
        &layout.profiles_dir,
        "quebec",
        &shim_profile(
            &layout,
            "Quebec",
            "stubcli.cmd",
            &[("PATH", child_path.as_str())],
        ),
    );
    // `{server}` and `{env}` carry the instance id into every command the
    // row runs (add, remove and the status read).
    let config = DaemonConfig {
        instance_id: Some("a&b".to_string()),
        program_lookup: shim_lookup(&bin),
        ..registration_config(&layout, Some(layout.bridge.clone()))
    };
    let h = Harness::start("mcpreg-run-x12-metachar", config).await;
    let mut approver = approver_connection(&h).await;

    let (_, result) =
        run_and_collect(&mut approver, "quebec", McpRegistrationAction::Register, 1).await;

    assert!(
        result.error.is_some(),
        "a refused argument fails the operation: {result:?}"
    );
    assert!(
        result.steps.iter().all(|step| step.exit_code.is_none()),
        "no step exited, because none started: {result:?}"
    );
    let refused_step = result.steps.last().expect("the refused step is reported");
    assert_eq!(
        result.error.as_deref(),
        Some(refused_step.output.as_str()),
        "the operation fails with the refusal's own reason, the one its step shows: {result:?}"
    );
    assert!(
        !marker.exists(),
        "the shim never ran: {:?}",
        std::fs::read_to_string(&marker).ok()
    );
}

// =============================================================================
// A registration with no add / remove command: its CLI reads the bridge
// from a settings file, and hooks from a second file that settings file
// names (the claim). The shared entries serve every instance at once.
// =============================================================================

const SETTINGS_FILE: &str = ".neutral/settings.json";
const MANAGED_HOOKS_FILE: &str = ".neutral/managed-hooks.json";
const CLAIM: &str = "/managed_hooks_path";
const OWN_CLAIM_VALUE: &str = "managed-hooks.json";

/// The settings file's own content before any registration: one entry of the
/// user's that every operation must leave as it is.
const USER_SETTINGS: &str = "{\n  \"theme\": \"dark\"\n}\n";

/// A command-less row over two files. The element holding the claim comes
/// first, so writing in the declared order would write the referring file
/// before the file it refers to.
const COMMANDLESS_TABLE: &str = "[mcp_registration.status]\n\
file = \".neutral/settings.json\"\n\
format = \"json\"\n\
command = \"/mcpServers/{server}/command\"\n\
\n\
[[mcp_registration.json_entry]]\n\
file = \".neutral/settings.json\"\n\
claim = \"/managed_hooks_path\"\n\
[mcp_registration.json_entry.entries]\n\
\"/mcpServers/{server}\" = { command = \"{bridge}\", args = [], env = \"{env_refs}\" }\n\
\"/managed_hooks_path\" = \"managed-hooks.json\"\n\
\n\
[[mcp_registration.json_entry]]\n\
file = \".neutral/managed-hooks.json\"\n\
[mcp_registration.json_entry.by_os.windows]\n\
\"/hooks\" = { Stop = \"windows-hook\" }\n\
[mcp_registration.json_entry.by_os.default]\n\
\"/hooks\" = { Stop = \"default-hook\" }\n";

fn commandless_profile(display_name: &str, table: &str) -> String {
    mcp_cli_stub_profile_with_table(display_name, "", table, &[])
}

fn home_file(layout: &Layout, file: &str) -> PathBuf {
    layout.home_dir.join(file)
}

fn seed_home_file(layout: &Layout, file: &str, content: &str) {
    let path = home_file(layout, file);
    std::fs::create_dir_all(path.parent().expect("has a parent")).expect("create settings dir");
    std::fs::write(path, content).expect("seed the settings file");
}

fn read_home_json(layout: &Layout, file: &str) -> Value {
    let text = std::fs::read_to_string(home_file(layout, file))
        .unwrap_or_else(|err| panic!("read {file}: {err}"));
    serde_json::from_str(&text).unwrap_or_else(|err| panic!("{file} is JSON ({err}): {text}"))
}

/// What `{env_refs}` writes for an instance: the window's identity as a
/// reference that is empty outside a window, and the `dev` instance by value.
fn env_refs(named: bool) -> Value {
    let mut refs = json!({
        "YAGURA_WINDOW_ID": "${YAGURA_WINDOW_ID:-}",
        "YAGURA_STATUS_TOKEN": "${YAGURA_STATUS_TOKEN:-}",
    });
    if named {
        refs["YAGURA_INSTANCE"] = json!("dev");
    }
    refs
}

fn assert_ran_no_command(result: &RunResult, context: &str) {
    assert!(
        result.steps.iter().all(|step| step.argv.is_empty()),
        "{context}: a row without add / remove runs no command, only file edits: {:?}",
        result.steps
    );
}

#[tokio::test]
async fn register_without_commands_writes_every_entry_with_the_set_for_its_os() {
    for (os, tag, expected_hook) in [
        (ProfileOs::Windows, "mcpreg-run-cl-win", "windows-hook"),
        (ProfileOs::Linux, "mcpreg-run-cl-linux", "default-hook"),
    ] {
        let layout = Layout::new(tag);
        let bridge = layout.bridge_str();
        seed_home_file(&layout, SETTINGS_FILE, USER_SETTINGS);
        write_profile(
            &layout.profiles_dir,
            "romeo",
            &commandless_profile("Romeo", COMMANDLESS_TABLE),
        );
        let config = DaemonConfig {
            host_os: os,
            ..registration_config(&layout, Some(layout.bridge.clone()))
        };
        // No report folder: there is no daemon path to write, and none of
        // these entries names one.
        assert!(config.report_dir.is_none());
        let h = Harness::start(tag, config).await;
        let mut approver = approver_connection(&h).await;

        let (_, result) =
            run_and_collect(&mut approver, "romeo", McpRegistrationAction::Register, 1).await;
        assert!(result.error.is_none(), "{os:?}: {result:?}");
        assert_ran_no_command(&result, &format!("{os:?}"));
        assert_eq!(
            result.status.state,
            McpRegistrationState::Registered,
            "{os:?}: {result:?}"
        );
        assert_eq!(result.status.refused_by_claim, None, "{os:?}");

        let settings = read_home_json(&layout, SETTINGS_FILE);
        assert_eq!(
            settings.pointer("/mcpServers/yagura"),
            Some(&json!({ "command": bridge, "args": [], "env": env_refs(false) })),
            "{os:?}: {settings}"
        );
        assert_eq!(settings.pointer(CLAIM), Some(&json!(OWN_CLAIM_VALUE)));
        assert_eq!(settings.pointer("/theme"), Some(&json!("dark")));
        let hooks = read_home_json(&layout, MANAGED_HOOKS_FILE);
        assert_eq!(
            hooks.pointer("/hooks"),
            Some(&json!({ "Stop": expected_hook })),
            "{os:?}: the set chosen for the host, or default without one: {hooks}"
        );
    }
}

#[tokio::test]
async fn a_single_json_entry_table_writes_what_a_one_element_array_writes() {
    let layout = Layout::new("mcpreg-run-cl-single");
    let bridge = layout.bridge_str();
    let table = |file: &str, header: &str| {
        format!(
            "[mcp_registration.status]\nfile = \"{file}\"\nformat = \"json\"\n\
             command = \"/{{server}}/command\"\n\n{header}\nfile = \"{file}\"\n\
             [mcp_registration.json_entry.entries]\n\
             \"{{server}}\" = {{ command = \"{{bridge}}\", env = \"{{env_refs}}\" }}\n"
        )
    };
    let single_file = ".neutral/single.json";
    let array_file = ".neutral/array.json";
    for file in [single_file, array_file] {
        seed_home_file(&layout, file, USER_SETTINGS);
    }
    write_profile(
        &layout.profiles_dir,
        "single",
        &commandless_profile(
            "Single",
            &table(single_file, "[mcp_registration.json_entry]"),
        ),
    );
    write_profile(
        &layout.profiles_dir,
        "array",
        &commandless_profile(
            "Array",
            &table(array_file, "[[mcp_registration.json_entry]]"),
        ),
    );
    let h = Harness::start(
        "mcpreg-run-cl-single",
        registration_config(&layout, Some(layout.bridge.clone())),
    )
    .await;
    let mut approver = approver_connection(&h).await;

    for (request_id, profile) in [(1, "single"), (2, "array")] {
        let (_, result) = run_and_collect(
            &mut approver,
            profile,
            McpRegistrationAction::Register,
            request_id,
        )
        .await;
        assert!(result.error.is_none(), "{profile}: {result:?}");
        assert_eq!(result.status.state, McpRegistrationState::Registered);
    }
    let single = std::fs::read(home_file(&layout, single_file)).expect("read single");
    let array = std::fs::read(home_file(&layout, array_file)).expect("read array");
    assert_eq!(
        String::from_utf8_lossy(&single),
        String::from_utf8_lossy(&array)
    );
    // A key without a leading `/` names a top-level entry.
    assert_eq!(
        read_home_json(&layout, single_file).pointer("/yagura"),
        Some(&json!({ "command": bridge, "env": env_refs(false) }))
    );
}

#[tokio::test]
async fn a_named_instance_whose_bridge_entry_lacks_env_refs_is_refused_before_any_write() {
    let layout = Layout::new("mcpreg-run-cl-no-refs");
    let table = "[mcp_registration.status]\n\
                 file = \".neutral/settings.json\"\n\
                 format = \"json\"\n\
                 command = \"/mcpServers/{server}/command\"\n\
                 \n\
                 [mcp_registration.json_entry]\n\
                 file = \".neutral/settings.json\"\n\
                 [mcp_registration.json_entry.entries]\n\
                 \"/mcpServers/{server}\" = { command = \"{bridge}\" }\n";
    write_profile(
        &layout.profiles_dir,
        "sierra",
        &commandless_profile("Sierra", table),
    );
    let config = DaemonConfig {
        instance_id: Some("dev".to_string()),
        ..registration_config(&layout, Some(layout.bridge.clone()))
    };
    let h = Harness::start("mcpreg-run-cl-no-refs", config).await;
    let mut approver = approver_connection(&h).await;

    for (request_id, action) in [
        (1, McpRegistrationAction::Register),
        (2, McpRegistrationAction::Update),
    ] {
        let (_, result) = run_and_collect(&mut approver, "sierra", action, request_id).await;
        assert_eq!(
            result.error,
            Some(RenderError::InstanceNotCarried.to_string()),
            "{action:?}: {result:?}"
        );
        assert!(result.steps.is_empty(), "{action:?}: {result:?}");
        assert!(
            !home_file(&layout, SETTINGS_FILE).exists(),
            "{action:?}: nothing was written"
        );
    }
}

#[tokio::test]
async fn a_claim_holding_another_value_refuses_register_and_update_and_changes_no_byte() {
    let layout = Layout::new("mcpreg-run-cl-claim");
    let theirs_settings =
        "{\n  \"managed_hooks_path\": \"theirs.json\",\n  \"theme\": \"dark\"\n}\n";
    let theirs_hooks = "{ \"hooks\": { \"Stop\": \"theirs\" } }\n";
    seed_home_file(&layout, SETTINGS_FILE, theirs_settings);
    seed_home_file(&layout, MANAGED_HOOKS_FILE, theirs_hooks);
    write_profile(
        &layout.profiles_dir,
        "tango",
        &commandless_profile("Tango", COMMANDLESS_TABLE),
    );
    let h = Harness::start(
        "mcpreg-run-cl-claim",
        registration_config(&layout, Some(layout.bridge.clone())),
    )
    .await;
    let mut approver = approver_connection(&h).await;

    for (request_id, action) in [
        (1, McpRegistrationAction::Register),
        (2, McpRegistrationAction::Update),
    ] {
        let (_, result) = run_and_collect(&mut approver, "tango", action, request_id).await;
        let error = result
            .error
            .as_deref()
            .unwrap_or_else(|| panic!("{action:?}: a claim held by another value refuses"));
        assert!(error.contains(CLAIM), "{action:?}: {error}");
        assert_eq!(
            std::fs::read_to_string(home_file(&layout, SETTINGS_FILE)).expect("read settings"),
            theirs_settings,
            "{action:?}"
        );
        assert_eq!(
            std::fs::read_to_string(home_file(&layout, MANAGED_HOOKS_FILE)).expect("read hooks"),
            theirs_hooks,
            "{action:?}"
        );
        assert_eq!(
            result.status.refused_by_claim.as_deref(),
            Some(CLAIM),
            "{action:?}: the status says Register and Update are refused, and by which claim: \
             {:?}",
            result.status
        );
    }
}

#[tokio::test]
async fn two_instances_keep_the_shared_entries_until_the_last_one_is_removed() {
    let layout = Layout::new("mcpreg-run-cl-two");
    let bridge = layout.bridge_str();
    seed_home_file(&layout, SETTINGS_FILE, USER_SETTINGS);
    write_profile(
        &layout.profiles_dir,
        "uniform",
        &commandless_profile("Uniform", COMMANDLESS_TABLE),
    );
    let default_daemon = Harness::start(
        "mcpreg-run-cl-two-default",
        registration_config(&layout, Some(layout.bridge.clone())),
    )
    .await;
    let dev_daemon = Harness::start(
        "mcpreg-run-cl-two-dev",
        DaemonConfig {
            instance_id: Some("dev".to_string()),
            ..registration_config(&layout, Some(layout.bridge.clone()))
        },
    )
    .await;
    let mut default_approver = approver_connection(&default_daemon).await;
    let mut dev_approver = approver_connection(&dev_daemon).await;

    for (approver, context) in [
        (&mut default_approver, "default"),
        (&mut dev_approver, "dev"),
    ] {
        let (_, result) =
            run_and_collect(approver, "uniform", McpRegistrationAction::Register, 1).await;
        assert!(result.error.is_none(), "{context}: {result:?}");
        assert_eq!(result.status.state, McpRegistrationState::Registered);
    }
    let settings = read_home_json(&layout, SETTINGS_FILE);
    assert_eq!(
        settings.pointer("/mcpServers/yagura-dev"),
        Some(&json!({ "command": bridge, "args": [], "env": env_refs(true) })),
        "{settings}"
    );

    let (_, removed) = run_and_collect(
        &mut default_approver,
        "uniform",
        McpRegistrationAction::Remove,
        2,
    )
    .await;
    assert!(removed.error.is_none(), "{removed:?}");
    assert_ran_no_command(&removed, "remove");
    assert_eq!(removed.status.state, McpRegistrationState::NotRegistered);
    let settings = read_home_json(&layout, SETTINGS_FILE);
    assert_eq!(settings.pointer("/mcpServers/yagura"), None, "{settings}");
    assert!(settings.pointer("/mcpServers/yagura-dev").is_some());
    assert_eq!(
        settings.pointer(CLAIM),
        Some(&json!(OWN_CLAIM_VALUE)),
        "another instance still needs the shared entries: {settings}"
    );
    assert!(
        read_home_json(&layout, MANAGED_HOOKS_FILE)
            .pointer("/hooks")
            .is_some()
    );

    let (_, removed) = run_and_collect(
        &mut dev_approver,
        "uniform",
        McpRegistrationAction::Remove,
        2,
    )
    .await;
    assert!(removed.error.is_none(), "{removed:?}");
    assert_eq!(
        read_home_json(&layout, SETTINGS_FILE),
        json!({ "theme": "dark", "mcpServers": {} }),
        "only the registration's own entries are gone; the emptied parent stays"
    );
    assert_eq!(read_home_json(&layout, MANAGED_HOOKS_FILE), json!({}));
}

#[tokio::test]
async fn a_hook_file_that_cannot_be_written_leaves_the_file_referring_to_it_unchanged() {
    let layout = Layout::new("mcpreg-run-cl-order");
    seed_home_file(&layout, SETTINGS_FILE, USER_SETTINGS);
    // A directory where the hook file belongs: it can be neither read nor
    // replaced as a file.
    std::fs::create_dir_all(home_file(&layout, MANAGED_HOOKS_FILE)).expect("block the hook file");
    write_profile(
        &layout.profiles_dir,
        "victor",
        &commandless_profile("Victor", COMMANDLESS_TABLE),
    );
    let h = Harness::start(
        "mcpreg-run-cl-order",
        registration_config(&layout, Some(layout.bridge.clone())),
    )
    .await;
    let mut approver = approver_connection(&h).await;

    let (_, result) =
        run_and_collect(&mut approver, "victor", McpRegistrationAction::Register, 1).await;
    assert!(result.error.is_some(), "{result:?}");
    assert_eq!(
        std::fs::read_to_string(home_file(&layout, SETTINGS_FILE)).expect("read settings"),
        USER_SETTINGS,
        "the settings file must not point at a hook file that was never written"
    );
    assert_ne!(result.status.state, McpRegistrationState::Registered);
}
