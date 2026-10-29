#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration tests: a window opened from a profile whose `[credentials]` mode is
//! `launch` gets the settings its CLI reads written with the window's own credentials
//! file named inside (`{credentials_file}`), and `[credentials] launch_args` reach the
//! argv the window is spawned with and nothing else.
//!
//! Every window is a real child (`hook_child`, which logs the argv it received) of an
//! in-process daemon, so what a CLI would read from disk and from its argv is what is
//! measured.

mod common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use common::{
    Harness, approver_base_config, call_send_handoff_from_profile_within, empty_lookup,
    expect_handoff_sent, handoff_profile_config, launch_args_logged_in, mcp_cli_stub_path, policy,
    read_response, registration_dirs, send, write_profile,
};
use yagura_daemon::DaemonConfig;
use yagura_daemon::profile::ProfileOs;
use yagura_protocol::{ClientMessage, DaemonMessage};
use yagura_test_support::ipc::Connection;
use yagura_test_support::paths::TempDir;
use yagura_test_support::process::{
    COMPOSER_CONTINUATION_PREFIX, COMPOSER_MARKER, COMPOSER_SEPARATOR,
};

const TIMEOUT: Duration = Duration::from_secs(20);

/// What a launch window's hook settings hold: the credentials file inside a command,
/// and a `${...}` reference the CLI itself expands, which must stay as written.
const HOOKS_JSON: &str = "{\"hooks\":{\"Stop\":[{\"hooks\":[{\"type\":\"command\",\"command\":\
     \"yagura-daemon --credentials '{credentials_file}' --emit-status waiting_input\",\
     \"timeout\":\"${HOOK_REF}\"}]}]}}";

const MCP_JSON: &str = "{\"mcpServers\":{\"yagura\":{\"command\":\"${MCP_BRIDGE_REF}\",\
     \"args\":[\"--credentials\",\"{credentials_file}\"]}}}";

const LAUNCH_ARGS: &str = "launch_args = [\"--launch-arg\", \"one\"]";

/// The `[hooks]` table that writes `HOOKS_JSON` as one file and hands the CLI its path
/// and the credentials file's.
fn hooks_file_tables() -> String {
    format!(
        "\n[credentials]\nmode = \"launch\"\n\n[hooks]\n\
         args = [\"--hooks-settings\", \"{{hooks_config_file}}\", \"--creds\", \
         \"{{credentials_file}}\"]\nconfig_file = '''{HOOKS_JSON}'''\n"
    )
}

/// A profile that runs `hook_child` (logging its argv under `logs`) with `flags`, then
/// `tables`.
fn write_hook_child_profile(
    profiles_dir: &Path,
    id: &str,
    logs: &Path,
    flags: &[&str],
    tables: &str,
) {
    let mut command = vec![
        env!("CARGO_BIN_EXE_hook_child").to_string(),
        COMPOSER_MARKER.to_string(),
        COMPOSER_SEPARATOR.to_string(),
        COMPOSER_CONTINUATION_PREFIX.to_string(),
        "0".to_string(),
        "-".to_string(),
        "--log".to_string(),
        logs.display().to_string(),
    ];
    command.extend(flags.iter().map(ToString::to_string));
    let elements = command
        .iter()
        .map(|element| format!("'{element}'"))
        .collect::<Vec<_>>()
        .join(", ");
    let source = format!(
        "schema = 1\ndisplay_name = \"{id}\"\n\n[command]\ndefault = [{elements}]\n{tables}"
    );
    write_profile(profiles_dir, id, &source);
}

/// A daemon config that resolves absolute programs and writes a window's settings under
/// `mcp_dir`; a bridge path makes `[mcp]` tables count.
fn config(profiles_dir: PathBuf, mcp_dir: &Path, bridge: Option<PathBuf>) -> DaemonConfig {
    // A `[host_paths]` command needs a home and an empty directory to run in.
    let (home, registration_cwd) = registration_dirs("cred-inject");
    DaemonConfig {
        home_dir: Some(home),
        registration_cwd,
        profiles_dir: Some(profiles_dir),
        host_os: ProfileOs::host(),
        program_lookup: empty_lookup(),
        program_is_executable: Path::is_file,
        default_shell: None,
        mcp_bridge: bridge,
        mcp_config_dir: Some(mcp_dir.to_path_buf()),
        ..approver_base_config()
    }
}

async fn open(client: &mut Connection, profile: &str) -> String {
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
    match read_response(client, Duration::from_secs(15)).await {
        DaemonMessage::WindowOpened { window_id, .. } => window_id,
        other => panic!("expected window_opened for {profile}, got {other:?}"),
    }
}

async fn until(what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    while !condition() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// The argument after `flag` in the argv `hook_child` logged.
fn value_after(args: &[String], flag: &str) -> String {
    let at = args
        .iter()
        .position(|arg| arg == flag)
        .unwrap_or_else(|| panic!("the window was not handed {flag}; argv: {args:?}"));
    args[at + 1].clone()
}

fn read(path: &str) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|err| {
        panic!("{path} must be a readable file the window's CLI can use: {err}")
    })
}

/// Every file under `dir`, recursively.
fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(files_under(&path));
        } else {
            found.push(path);
        }
    }
    found
}

// =============================================================================
// (a) a settings file is the window's own and goes with it
// =============================================================================

#[tokio::test]
async fn a_launch_windows_hook_settings_name_its_own_credentials_file_and_go_with_it() {
    let tree = TempDir::new("yagura-cli-test", "cred-inject-file");
    let profiles_dir = tree.make_dir("profiles");
    let mcp_dir = tree.make_dir("mcp");
    let gates = tree.make_dir("gates");
    let mut logs = Vec::new();
    for id in ["first", "second"] {
        let log = tree.make_dir(&format!("logs-{id}"));
        let gate = gates.join(id).display().to_string();
        write_hook_child_profile(
            &profiles_dir,
            id,
            &log,
            &["--exit-on", &gate],
            &hooks_file_tables(),
        );
        logs.push(log);
    }
    let h = Harness::start("cred-inject-file", config(profiles_dir, &mcp_dir, None)).await;
    let mut client = h.connect().await;

    let mut windows = Vec::new();
    for (id, log) in ["first", "second"].into_iter().zip(&logs) {
        open(&mut client, id).await;
        let args = launch_args_logged_in(log).await;
        windows.push((
            value_after(&args, "--hooks-settings"),
            value_after(&args, "--creds"),
        ));
    }

    for (settings, credentials) in &windows {
        let written = read(settings);
        assert!(
            written.contains(&format!("'{credentials}'")),
            "the settings must name the window's credentials file {credentials}; got {written}"
        );
        assert!(
            !written.contains("{credentials_file}"),
            "no placeholder may be left in the settings; got {written}"
        );
        assert!(
            written.contains("${HOOK_REF}"),
            "a reference the CLI expands must stay as written; got {written}"
        );
    }
    assert_ne!(
        windows[0].0, windows[1].0,
        "two launch windows must not share one settings file"
    );

    std::fs::write(gates.join("first"), b"go").unwrap();
    until("the first window's settings to go with its child", || {
        !Path::new(&windows[0].0).exists()
    })
    .await;
    assert!(
        Path::new(&windows[1].0).is_file() && Path::new(&windows[1].1).is_file(),
        "the other window's settings and credentials must stay"
    );
    h.registry.terminate_all().await;
}

// =============================================================================
// (b) a settings directory and an MCP settings file are rendered the same way
// =============================================================================

#[tokio::test]
async fn a_hooks_directory_and_an_mcp_file_name_the_credentials_file_and_keep_their_references() {
    let tree = TempDir::new("yagura-cli-test", "cred-inject-dir");
    let profiles_dir = tree.make_dir("profiles");
    let mcp_dir = tree.make_dir("mcp");
    let logs = tree.make_dir("logs");
    let hooks_json = HOOKS_JSON;
    let tables = format!(
        "\n[credentials]\nmode = \"launch\"\n\n[hooks]\n\
         args = [\"--hooks-dir\", \"{{hooks_config_dir}}\", \"--creds\", \"{{credentials_file}}\"]\n\n\
         [hooks.config_dir]\n\"hooks.json\" = '''{hooks_json}'''\n\n\
         [mcp]\nargs = [\"--mcp-config\", \"{{mcp_config_file}}\"]\n\
         config_file = '''{MCP_JSON}'''\n"
    );
    write_hook_child_profile(&profiles_dir, "both", &logs, &[], &tables);
    let bridge = tree.make_dir("bridge").join("yagura-mcp.exe");
    let h = Harness::start(
        "cred-inject-dir",
        config(profiles_dir, &mcp_dir, Some(bridge)),
    )
    .await;
    let mut client = h.connect().await;

    open(&mut client, "both").await;
    let args = launch_args_logged_in(&logs).await;
    let credentials = value_after(&args, "--creds");
    let hooks = read(&format!("{}/hooks.json", value_after(&args, "--hooks-dir")));
    let mcp = read(&value_after(&args, "--mcp-config"));

    for (what, written, reference) in [
        ("the hooks directory's file", &hooks, "${HOOK_REF}"),
        ("the MCP settings file", &mcp, "${MCP_BRIDGE_REF}"),
    ] {
        assert!(
            written.contains(&credentials),
            "{what} must name the window's credentials file {credentials}; got {written}"
        );
        assert!(
            !written.contains("{credentials_file}"),
            "{what} must have no placeholder left; got {written}"
        );
        assert!(
            written.contains(reference),
            "{what} must keep {reference} as written; got {written}"
        );
    }
    h.registry.terminate_all().await;
}

// =============================================================================
// (c) a host-path conversion reaches the settings' location only
// =============================================================================

const CONVERTED_SETTINGS_PATH: &str = "/converted/hooks-settings.json";

#[tokio::test]
async fn a_host_paths_conversion_changes_the_settings_location_but_not_the_credentials_file() {
    let tree = TempDir::new("yagura-cli-test", "cred-inject-host-paths");
    let profiles_dir = tree.make_dir("profiles");
    let mcp_dir = tree.make_dir("mcp");
    let logs = tree.make_dir("logs");
    let tables = format!(
        "{}\n[host_paths]\nconvert = ['{}', \"probe\", \"{{path}}\"]\n\n\
         [env]\nMCP_CLI_STUB_PROBE_OUTPUT = \"{CONVERTED_SETTINGS_PATH}\"\n\
         MCP_CLI_STUB_PROBE_EXIT_CODE = \"0\"\n",
        hooks_file_tables(),
        mcp_cli_stub_path().display()
    );
    write_hook_child_profile(&profiles_dir, "converted", &logs, &[], &tables);
    let h = Harness::start(
        "cred-inject-host-paths",
        config(profiles_dir, &mcp_dir, None),
    )
    .await;
    let mut client = h.connect().await;

    open(&mut client, "converted").await;
    let args = launch_args_logged_in(&logs).await;
    let credentials = value_after(&args, "--creds");

    assert_eq!(
        value_after(&args, "--hooks-settings"),
        CONVERTED_SETTINGS_PATH,
        "the CLI is told the converted location of its settings"
    );
    assert!(
        Path::new(&credentials).is_file(),
        "the credentials file is named where the daemon keeps it, not converted; got {credentials}"
    );
    let settings: Vec<String> = files_under(&mcp_dir)
        .into_iter()
        .filter_map(|path| std::fs::read_to_string(path).ok())
        .filter(|text| text.contains("--emit-status waiting_input"))
        .collect();
    assert_eq!(settings.len(), 1, "one settings file was written");
    assert!(
        settings[0].contains(&credentials),
        "the settings name the daemon-side credentials file {credentials}; got {}",
        settings[0]
    );
    assert!(
        !settings[0].contains("/converted/"),
        "the settings' content is not converted; got {}",
        settings[0]
    );
    h.registry.terminate_all().await;
}

// =============================================================================
// (d) a credentials file whose location cannot be written safely
// =============================================================================

/// Opens a launch window whose settings are kept under a directory called `dir_name`;
/// returns the argv its child logged and whether the daemon expects its CLI's reports
/// and takes part in delivery.
async fn open_launch_window_under(dir_name: &str) -> (Vec<String>, bool, bool) {
    let tree = TempDir::new("yagura-cli-test", "cred-inject-quote");
    let profiles_dir = tree.make_dir("profiles");
    let mcp_dir = tree.make_dir(dir_name);
    let logs = tree.make_dir("logs");
    let tables = format!(
        "\n[credentials]\nmode = \"launch\"\n{LAUNCH_ARGS}\n\n[hooks]\n\
         args = [\"--hooks-settings\", \"{{hooks_config_file}}\", \"--creds\", \
         \"{{credentials_file}}\"]\nconfig_file = '''{HOOKS_JSON}'''\n\n\
         [mcp]\nargs = [\"--mcp-config\", \"{{mcp_config_file}}\"]\n\
         config_file = '''{MCP_JSON}'''\n\n[delivery]\nwake = \"none\"\n"
    );
    write_hook_child_profile(&profiles_dir, "launch", &logs, &[], &tables);
    let bridge = tree.make_dir("bridge").join("yagura-mcp.exe");
    let h = Harness::start(
        "cred-inject-quote",
        config(profiles_dir, &mcp_dir, Some(bridge)),
    )
    .await;
    let mut client = h.connect().await;

    let window_id = open(&mut client, "launch").await;
    let args = launch_args_logged_in(&logs).await;
    let entry = h.registry.get(&window_id).unwrap();
    assert!(entry.has_detector(), "the window must still be running");
    let seen = (args, entry.reports_expected(), entry.delivers());
    h.registry.terminate_all().await;
    seen
}

#[tokio::test]
async fn a_location_with_a_quote_opens_the_window_without_hooks_and_the_bridge_but_with_launch_args()
 {
    let (control, expects, delivers) = open_launch_window_under("mcp").await;
    assert!(
        expects && delivers,
        "control: with a plain location the window's hooks are in place"
    );
    assert!(
        control.iter().any(|arg| arg == "--creds"),
        "control: the credentials file is named; argv: {control:?}"
    );

    let (args, expects, delivers) = open_launch_window_under("it's mcp").await;
    assert!(
        !expects && !delivers,
        "a window whose hooks were left out neither reports nor takes delivery"
    );
    for arg in &args {
        assert!(
            !arg.contains("{credentials_file}") && !arg.contains("it's"),
            "neither the placeholder nor the unusable location may reach the CLI; argv: {args:?}"
        );
    }
    assert!(
        !args.iter().any(|arg| arg == "--hooks-settings"),
        "the hooks injection is left out; argv: {args:?}"
    );
    assert_eq!(
        value_after(&args, "--launch-arg"),
        "one",
        "launch_args are added whether or not the injections are"
    );
}

// =============================================================================
// (e) launch_args are for the spawn's argv only
// =============================================================================

/// The command lines the daemon recorded for its windows, once it has written them.
async fn recorded_commands(db: &Path) -> Vec<Vec<String>> {
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    loop {
        if let Ok(conn) = yagura_daemon::store::open_readonly(db)
            && let Ok(rows) = yagura_daemon::store::read_snapshot_conn(&conn)
            && !rows.is_empty()
        {
            return rows.into_iter().map(|row| row.command).collect();
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the window's command was never recorded"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn launch_args_come_before_the_launch_prompt_and_stay_out_of_every_other_command() {
    let tree = TempDir::new("yagura-cli-test", "cred-inject-launch-args");
    let profiles_dir = tree.make_dir("profiles");
    let detection_dir = tree.make_dir("detection");
    let logs = tree.make_dir("logs");
    let tables = format!(
        "\n[credentials]\n{LAUNCH_ARGS}\n\n[delivery]\nwake = \"command\"\n\
         wake_args = [\"--woken\", \"{{session}}\", \"{{text}}\"]\n\
         launch_prompt = [\"--prompt\", \"{{prompt}}\"]\n"
    );
    write_hook_child_profile(&profiles_dir, "args", &logs, &[], &tables);
    let config = handoff_profile_config(
        Some(profiles_dir),
        empty_lookup(),
        &detection_dir,
        None,
        Some(policy(&["send_handoff"])),
        30,
    );
    let h = Harness::start("cred-inject-launch-args", config).await;
    let db = tree.make_dir("store").join("yagura.db");
    yagura_daemon::store::wire_into(&h.registry, Some(db.clone()));
    let mut client = h.connect().await;

    let reply = call_send_handoff_from_profile_within(
        &mut client,
        "args",
        None,
        None,
        "src/foo.rs",
        "look here",
        Duration::from_secs(10),
    )
    .await;
    let (window_id, _, _) = expect_handoff_sent(reply);
    let args = launch_args_logged_in(&logs).await;

    let at = args
        .iter()
        .position(|arg| arg == "--launch-arg")
        .unwrap_or_else(|| panic!("launch_args never reached the spawn's argv; argv: {args:?}"));
    assert_eq!(args[at + 1], "one", "argv: {args:?}");
    let prompt_at = value_position(&args, "--prompt");
    assert!(
        at < prompt_at && prompt_at + 2 == args.len(),
        "launch_args come before the launch prompt, which stays the last argument; argv: {args:?}"
    );

    let entry = h.registry.get(&window_id).unwrap();
    let wake = entry.wake_command().expect("the profile wakes by command");
    assert!(
        !wake.argv.iter().any(|arg| arg == "--launch-arg"),
        "launch_args are not part of the wake command; got {:?}",
        wake.argv
    );
    for command in recorded_commands(&db).await {
        assert!(
            !command.iter().any(|arg| arg == "--launch-arg"),
            "launch_args are not part of the recorded command; got {command:?}"
        );
    }
    h.registry.terminate_all().await;
}

fn value_position(args: &[String], flag: &str) -> usize {
    args.iter()
        .position(|arg| arg == flag)
        .unwrap_or_else(|| panic!("{flag} is missing; argv: {args:?}"))
}
