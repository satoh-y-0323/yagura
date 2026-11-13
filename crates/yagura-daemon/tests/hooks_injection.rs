#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration tests: a launch profile's `[hooks]` table is rendered into the
//! child's argv and a settings file on disk when a window opens, next to (and
//! independent of) the `[mcp]` injection.
//!
//! Every property is measured through a real spawned child (`argv_probe`):
//! the argv the child received and the file its argv names are what a CLI
//! would see. The table's keys are `args` (required), `config_file`
//! (optional) and the placeholder `{hooks_config_file}`. A `launch` window's open also
//! leaves the table's `answers` beside its credentials file, with its wake path.

use crate::common;

use std::path::{Path, PathBuf};

use common::{
    Harness, VALID_TOKEN, approver_base_config, call_open_window, empty_lookup, read_response,
    registration_dirs, send, write_profile,
};
use yagura_daemon::DaemonConfig;
use yagura_daemon::profile::ProfileOs;
use yagura_protocol::{ClientMessage, DaemonMessage};
use yagura_test_support::ipc::Connection;
use yagura_test_support::paths::TempDir;
use yagura_test_support::process::{argv_probe_path, argv_probe_profile_source, ready_child_path};

const HOOKS_JSON: &str = "{\"hooks\":{\"Stop\":[{\"hooks\":[{\"type\":\"command\",\"command\":\"yagura-daemon --emit-status waiting_input --source claude-code\"}]}]}}";

const MCP_JSON: &str =
    "{\"mcpServers\":{\"yagura\":{\"command\":\"${YAGURA_MCP_BRIDGE}\",\"args\":[]}}}";

fn hooks_file_table(config_file_json: &str) -> String {
    format!(
        "\n[hooks]\nconfig_file = '{config_file_json}'\nargs = [\"--settings\", \
         \"{{hooks_config_file}}\"]\n"
    )
}

fn mcp_file_table(config_file_json: &str) -> String {
    format!(
        "\n[mcp]\nconfig_file = '{config_file_json}'\nargs = [\"--mcp-config\", \
         \"{{mcp_config_file}}\"]\n"
    )
}

const LAUNCH_OPTIONS_TABLE: &str = "\n[model]\nargs = [\"--tier\", \"{value}\"]\n\n\
     [model.aliases]\nalpha = \"tier-a\"\n";

fn probe_is_executable(path: &Path) -> bool {
    path.is_file()
}

fn daemon_config(
    profiles_dir: PathBuf,
    config_dir: Option<&Path>,
    bridge: Option<PathBuf>,
) -> DaemonConfig {
    let (home, registration_cwd) = registration_dirs("hooks-injection");
    DaemonConfig {
        profiles_dir: Some(profiles_dir),
        home_dir: Some(home),
        registration_cwd,
        host_os: ProfileOs::host(),
        program_lookup: empty_lookup(),
        program_is_executable: probe_is_executable,
        default_shell: None,
        mcp_bridge: bridge,
        mcp_config_dir: config_dir.map(Path::to_path_buf),
        ..approver_base_config()
    }
}

async fn open_from_profile(client: &mut Connection, profile: &str, model: Option<&str>) -> String {
    send(
        client,
        &ClientMessage::OpenWindowFromProfile {
            profile: profile.to_string(),
            cwd: None,
            name: None,
            model: model.map(str::to_string),
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

fn sentinel(text: &str, name: &str) -> Option<String> {
    let prefix = format!("<<{name}=");
    let start = text.rfind(&prefix)? + prefix.len();
    let end = text[start..].find(">>")? + start;
    Some(text[start..end].to_string())
}

// =============================================================================
// Argv and file content
// =============================================================================

#[tokio::test]
async fn a_hooks_table_appends_its_args_after_launch_options_and_mcp_with_the_file_written_verbatim()
 {
    let tree = TempDir::new("yagura-cli-test", "hooks-inject-order");
    let profiles_dir = tree.make_dir("profiles");
    let config_dir = tree.make_dir("mcp");
    let tables = format!(
        "{LAUNCH_OPTIONS_TABLE}{}{}",
        mcp_file_table(MCP_JSON),
        hooks_file_table(HOOKS_JSON)
    );
    write_profile(
        &profiles_dir,
        "argv-probe",
        &argv_probe_profile_source("Argv Probe", &tables),
    );
    let bridge = tree.make_dir("bridge").join("yagura-mcp.exe");
    let h = Harness::start(
        "hooks-inject-order",
        daemon_config(profiles_dir, Some(&config_dir), Some(bridge)),
    )
    .await;
    let mut client = h.connect().await;

    let window_id = open_from_profile(&mut client, "argv-probe", Some("alpha")).await;
    h.wait_for_scrollback(&window_id, b"<<argv[6]=").await;
    let text = String::from_utf8_lossy(&h.scrollback(&window_id)).into_owned();

    assert_eq!(
        sentinel(&text, "argv[1]").as_deref(),
        Some("--tier"),
        "{text:?}"
    );
    assert_eq!(
        sentinel(&text, "argv[2]").as_deref(),
        Some("tier-a"),
        "{text:?}"
    );
    assert_eq!(
        sentinel(&text, "argv[3]").as_deref(),
        Some("--mcp-config"),
        "{text:?}"
    );
    assert_eq!(
        sentinel(&text, "argv[5]").as_deref(),
        Some("--settings"),
        "hooks args must follow the launch options and the MCP args; scrollback: {text:?}"
    );
    let mcp_path = PathBuf::from(sentinel(&text, "argv[4]").expect("mcp file path"));
    let hooks_path = PathBuf::from(sentinel(&text, "argv[6]").expect("hooks file path"));
    assert_ne!(hooks_path, mcp_path);
    assert!(
        hooks_path.starts_with(&config_dir),
        "the hooks file must live under the same directory as the MCP file ({config_dir:?}); \
         got {hooks_path:?}"
    );
    let written = std::fs::read_to_string(&hooks_path)
        .unwrap_or_else(|err| panic!("the hooks file must exist: {err}"));
    assert_eq!(written, HOOKS_JSON, "config_file must be written verbatim");
    assert!(
        !written.contains(VALID_TOKEN) && !text.contains(VALID_TOKEN),
        "neither the file nor the argv may carry the window's status token"
    );
}

#[tokio::test]
async fn a_hooks_table_is_injected_without_an_mcp_bridge_and_equal_content_maps_to_one_file_name() {
    let tree = TempDir::new("yagura-cli-test", "hooks-inject-no-bridge");
    let profiles_dir = tree.make_dir("profiles");
    let config_dir = tree.make_dir("mcp");
    write_profile(
        &profiles_dir,
        "argv-probe",
        &argv_probe_profile_source("Argv Probe", &hooks_file_table(HOOKS_JSON)),
    );
    let other = HOOKS_JSON.replace("waiting_input", "running");
    write_profile(
        &profiles_dir,
        "other",
        &argv_probe_profile_source("Other", &hooks_file_table(&other)),
    );
    let h = Harness::start(
        "hooks-inject-no-bridge",
        daemon_config(profiles_dir, Some(&config_dir), None),
    )
    .await;
    let mut client = h.connect().await;

    let mut paths = Vec::new();
    for profile in ["argv-probe", "argv-probe", "other"] {
        let window_id = open_from_profile(&mut client, profile, None).await;
        h.wait_for_scrollback(&window_id, b"<<argv[2]=").await;
        let text = String::from_utf8_lossy(&h.scrollback(&window_id)).into_owned();
        assert_eq!(
            sentinel(&text, "argv[1]").as_deref(),
            Some("--settings"),
            "{text:?}"
        );
        paths.push(sentinel(&text, "argv[2]").expect("hooks file path"));
    }
    assert_eq!(
        paths[0], paths[1],
        "the same content must map to the same file name"
    );
    assert_ne!(
        paths[0], paths[2],
        "different content must map to a different file name"
    );
    assert_eq!(std::fs::read_to_string(&paths[2]).unwrap(), other);
}

#[tokio::test]
async fn a_hooks_config_dir_is_written_as_a_directory_of_exactly_the_declared_files_and_argv_names_it()
 {
    const PLUGIN_JSON: &str = "{\"name\":\"yagura-status\",\"version\":\"0.0.0\"}";
    let tree = TempDir::new("yagura-cli-test", "hooks-inject-config-dir");
    let profiles_dir = tree.make_dir("profiles");
    let config_dir = tree.make_dir("mcp");
    let tables = format!(
        "\n[hooks]\nargs = [\"--plugin-dir\", \"{{hooks_config_dir}}\"]\n\n\
         [hooks.config_dir]\n\"plugin.json\" = '{PLUGIN_JSON}'\n\"hooks.json\" = '{HOOKS_JSON}'\n"
    );
    write_profile(
        &profiles_dir,
        "argv-probe",
        &argv_probe_profile_source("Argv Probe", &tables),
    );
    let h = Harness::start(
        "hooks-inject-config-dir",
        daemon_config(profiles_dir, Some(&config_dir), None),
    )
    .await;
    let mut client = h.connect().await;

    let window_id = open_from_profile(&mut client, "argv-probe", None).await;
    h.wait_for_scrollback(&window_id, b"<<argv[2]=").await;
    let text = String::from_utf8_lossy(&h.scrollback(&window_id)).into_owned();

    assert_eq!(
        sentinel(&text, "argv[1]").as_deref(),
        Some("--plugin-dir"),
        "{text:?}"
    );
    let dir = PathBuf::from(sentinel(&text, "argv[2]").expect("hooks directory path"));
    assert!(
        dir.is_dir(),
        "the argv must name an existing directory; scrollback: {text:?}"
    );
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(
        names,
        ["hooks.json", "plugin.json"],
        "the directory must hold exactly the declared files"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("plugin.json")).unwrap(),
        PLUGIN_JSON,
        "each declared file must be written verbatim"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("hooks.json")).unwrap(),
        HOOKS_JSON,
        "each declared file must be written verbatim"
    );
}

#[tokio::test]
async fn hooks_args_without_a_config_file_are_passed_through_as_written() {
    let tree = TempDir::new("yagura-cli-test", "hooks-inject-args-only");
    let profiles_dir = tree.make_dir("profiles");
    let config_dir = tree.make_dir("mcp");
    write_profile(
        &profiles_dir,
        "argv-probe",
        &argv_probe_profile_source(
            "Argv Probe",
            "\n[hooks]\nargs = [\"--settings\", '{\"hooks\":{}}']\n",
        ),
    );
    let h = Harness::start(
        "hooks-inject-args-only",
        daemon_config(profiles_dir, Some(&config_dir), None),
    )
    .await;
    let mut client = h.connect().await;

    let window_id = open_from_profile(&mut client, "argv-probe", None).await;
    h.wait_for_scrollback(&window_id, b"<<argv[2]=").await;
    let text = String::from_utf8_lossy(&h.scrollback(&window_id)).into_owned();
    assert_eq!(
        sentinel(&text, "argv[1]").as_deref(),
        Some("--settings"),
        "{text:?}"
    );
    assert_eq!(
        sentinel(&text, "argv[2]").as_deref(),
        Some("{\"hooks\":{}}"),
        "{text:?}"
    );
}

#[tokio::test]
async fn a_hooks_table_needing_a_file_opens_the_window_without_hooks_when_no_directory_is_configured()
 {
    let tree = TempDir::new("yagura-cli-test", "hooks-inject-no-dir");
    let profiles_dir = tree.make_dir("profiles");
    write_profile(
        &profiles_dir,
        "argv-probe",
        &argv_probe_profile_source("Argv Probe", &hooks_file_table(HOOKS_JSON)),
    );
    let h = Harness::start(
        "hooks-inject-no-dir",
        daemon_config(profiles_dir, None, None),
    )
    .await;
    let mut client = h.connect().await;

    let window_id = open_from_profile(&mut client, "argv-probe", None).await;
    h.wait_until_child_exited(&window_id).await;
    let text = String::from_utf8_lossy(&h.scrollback(&window_id)).into_owned();
    assert!(
        !text.contains("<<argv[1]="),
        "with nowhere to write the file the window must open without hooks; scrollback: {text:?}"
    );
}

// =============================================================================
// Nothing changes without a [hooks] table
// =============================================================================

#[tokio::test]
async fn a_profile_without_a_hooks_table_gets_no_hooks_argv_even_when_mcp_is_injected() {
    let tree = TempDir::new("yagura-cli-test", "hooks-inject-absent");
    let profiles_dir = tree.make_dir("profiles");
    let config_dir = tree.make_dir("mcp");
    write_profile(
        &profiles_dir,
        "argv-probe",
        &argv_probe_profile_source("Argv Probe", &mcp_file_table(MCP_JSON)),
    );
    let bridge = tree.make_dir("bridge").join("yagura-mcp.exe");
    let h = Harness::start(
        "hooks-inject-absent",
        daemon_config(profiles_dir, Some(&config_dir), Some(bridge)),
    )
    .await;
    let mut client = h.connect().await;

    let window_id = open_from_profile(&mut client, "argv-probe", None).await;
    h.wait_for_scrollback(&window_id, b"<<argv[2]=").await;
    let text = String::from_utf8_lossy(&h.scrollback(&window_id)).into_owned();
    assert!(
        !text.contains("<<argv[3]="),
        "only the MCP args may be appended; scrollback: {text:?}"
    );
}

#[tokio::test]
async fn a_raw_open_window_gets_no_hooks_argv() {
    let tree = TempDir::new("yagura-cli-test", "hooks-inject-raw");
    let profiles_dir = tree.make_dir("profiles");
    let config_dir = tree.make_dir("mcp");
    write_profile(
        &profiles_dir,
        "argv-probe",
        &argv_probe_profile_source("Argv Probe", &hooks_file_table(HOOKS_JSON)),
    );
    let h = Harness::start(
        "hooks-inject-raw",
        daemon_config(profiles_dir, Some(&config_dir), None),
    )
    .await;
    let mut client = h.connect().await;

    let window_id = call_open_window(
        &mut client,
        vec![
            argv_probe_path().to_str().unwrap().to_string(),
            "own".to_string(),
        ],
    )
    .await;
    h.wait_for_scrollback(&window_id, b"<<argv[1]=own>>").await;
    let text = String::from_utf8_lossy(&h.scrollback(&window_id)).into_owned();
    assert!(!text.contains("<<argv[2]="), "{text:?}");
}

// =============================================================================
// Status reports are expected exactly when hooks were injected
// =============================================================================

/// A profile that keeps its child alive (the detector, and with it the
/// expectation, is dropped once a window has exited) and ignores the
/// injected arguments.
fn long_lived_profile(tables: &str) -> String {
    let program = ready_child_path();
    format!(
        "schema = 1
display_name = \"Ready\"

[command]
default = ['{}', 'ready']
{tables}",
        program
            .to_str()
            .expect("ready_child path must be valid UTF-8")
    )
}

#[tokio::test]
async fn a_window_launched_with_hooks_expects_status_reports_and_one_without_does_not() {
    let tree = TempDir::new("yagura-cli-test", "hooks-expect-reports");
    let profiles_dir = tree.make_dir("profiles");
    let config_dir = tree.make_dir("mcp");
    write_profile(
        &profiles_dir,
        "with-hooks",
        &long_lived_profile(&hooks_file_table(HOOKS_JSON)),
    );
    write_profile(&profiles_dir, "without-hooks", &long_lived_profile(""));
    let h = Harness::start(
        "hooks-expect-reports",
        daemon_config(profiles_dir, Some(&config_dir), None),
    )
    .await;
    let mut client = h.connect().await;

    let with = open_from_profile(&mut client, "with-hooks", None).await;
    let without = open_from_profile(&mut client, "without-hooks", None).await;

    let expects = |window_id: &str| h.registry.get(window_id).unwrap().reports_expected();
    assert!(expects(&with), "hooks were injected");
    assert!(!expects(&without), "no hooks were injected");
}

#[tokio::test]
async fn a_window_launched_with_only_an_mcp_injection_does_not_expect_status_reports() {
    let tree = TempDir::new("yagura-cli-test", "hooks-expect-mcp-only");
    let profiles_dir = tree.make_dir("profiles");
    let config_dir = tree.make_dir("mcp");
    write_profile(
        &profiles_dir,
        "mcp-only",
        &long_lived_profile(&mcp_file_table(MCP_JSON)),
    );
    let bridge = tree.make_dir("bridge").join("yagura-mcp.exe");
    let h = Harness::start(
        "hooks-expect-mcp-only",
        daemon_config(profiles_dir, Some(&config_dir), Some(bridge)),
    )
    .await;
    let mut client = h.connect().await;

    let window_id = open_from_profile(&mut client, "mcp-only", None).await;

    let written = std::fs::read_dir(&config_dir)
        .expect("the MCP config directory must be readable")
        .count();
    assert!(
        written > 0,
        "the MCP injection must have been carried out, writing its settings file"
    );
    let entry = h.registry.get(&window_id).unwrap();
    assert!(
        entry.has_detector(),
        "the window must still be running, or it expects nothing whatever was injected"
    );
    assert!(
        !entry.reports_expected(),
        "only MCP was injected, so no status reports are expected"
    );
}

// =============================================================================
// A profile's [host_paths] converts the paths a hooks injection hands the CLI
// =============================================================================

const CONVERTED_HOOKS_PATH: &str = "/converted/hooks-settings.json";

/// Top-level `[host_paths]` whose converter is the `mcp_cli_stub` `probe`
/// subcommand, plus the `[env]` that makes it print `CONVERTED_HOOKS_PATH`
/// and exit with `exit_code`.
fn converting_tables(exit_code: i32) -> String {
    let stub = common::mcp_cli_stub_path();
    format!(
        "\n[host_paths]\nconvert = ['{}', \"probe\", \"{{path}}\"]\n\n\
         [env]\nMCP_CLI_STUB_PROBE_OUTPUT = \"{CONVERTED_HOOKS_PATH}\"\n\
         MCP_CLI_STUB_PROBE_EXIT_CODE = \"{exit_code}\"\n\n\
         [delivery]\n",
        stub.display()
    )
}

#[tokio::test]
async fn a_host_paths_converter_replaces_the_daemon_side_hooks_path_in_the_argv() {
    let tree = TempDir::new("yagura-cli-test", "hooks-host-paths-convert");
    let profiles_dir = tree.make_dir("profiles");
    let config_dir = tree.make_dir("mcp");
    let tables = format!("{}{}", hooks_file_table(HOOKS_JSON), converting_tables(0));
    write_profile(
        &profiles_dir,
        "converted",
        &argv_probe_profile_source("Converted", &tables),
    );
    let h = Harness::start(
        "hooks-host-paths-convert",
        daemon_config(profiles_dir, Some(&config_dir), None),
    )
    .await;
    let mut client = h.connect().await;

    let window_id = open_from_profile(&mut client, "converted", None).await;
    h.wait_for_scrollback(&window_id, b"<<argv[2]=").await;
    let text = String::from_utf8_lossy(&h.scrollback(&window_id)).into_owned();

    assert_eq!(
        sentinel(&text, "argv[1]").as_deref(),
        Some("--settings"),
        "{text:?}"
    );
    assert_eq!(
        sentinel(&text, "argv[2]").as_deref(),
        Some(CONVERTED_HOOKS_PATH),
        "the argv must carry the converter's path, not the daemon-side path; scrollback: {text:?}"
    );
    assert!(
        h.registry.get(&window_id).unwrap().delivers(),
        "a window whose hooks reached the CLI takes part in delivery"
    );
}

#[tokio::test]
async fn a_failing_host_paths_converter_leaves_hooks_out_and_the_window_neither_reports_nor_delivers()
 {
    let tree = TempDir::new("yagura-cli-test", "hooks-host-paths-fail");
    let profiles_dir = tree.make_dir("profiles");
    let config_dir = tree.make_dir("mcp");
    let tables = format!("{}{}", hooks_file_table(HOOKS_JSON), converting_tables(1));
    write_profile(&profiles_dir, "unconverted", &long_lived_profile(&tables));
    let h = Harness::start(
        "hooks-host-paths-fail",
        daemon_config(profiles_dir, Some(&config_dir), None),
    )
    .await;
    let mut client = h.connect().await;

    let window_id = open_from_profile(&mut client, "unconverted", None).await;

    let entry = h.registry.get(&window_id).unwrap();
    assert!(
        entry.has_detector(),
        "the window must still be running, or it expects nothing whatever was injected"
    );
    assert!(
        !entry.reports_expected(),
        "the hooks were left out, so no status reports are expected"
    );
    assert!(
        !entry.delivers(),
        "a window whose hooks never reached the CLI must not take part in delivery"
    );
}

// =============================================================================
// A command-interpreter shim drops only what it would reinterpret
// =============================================================================

#[cfg(windows)]
mod shim {
    use super::*;

    /// A profile whose program is a `.cmd` shim forwarding to `argv_probe`,
    /// with `own` as its first argument so a report proves the shim ran.
    fn shim_profile(tree: &TempDir, tables: &str) -> String {
        let shim = tree.make_dir("shim").join("hookshim.cmd");
        std::fs::write(
            &shim,
            format!("@echo off\r\n\"{}\" %*\r\n", argv_probe_path().display()),
        )
        .expect("shim fixture should be writable");
        format!(
            "schema = 1\ndisplay_name = \"Shim\"\n\n[command]\ndefault = ['hookshim', 'own']\n{tables}"
        )
    }

    fn shim_config(
        tree: &TempDir,
        profiles_dir: PathBuf,
        config_dir: &Path,
        bridge: Option<PathBuf>,
    ) -> DaemonConfig {
        let comspec = PathBuf::from(std::env::var("SystemRoot").expect("SystemRoot"))
            .join("System32")
            .join("cmd.exe");
        DaemonConfig {
            program_lookup: yagura_os::program::ProgramLookup {
                path_dirs: vec![tree.make_dir("shim")],
                extra_bin_dirs: Vec::new(),
                windows_comspec: Some(comspec),
            },
            ..daemon_config(profiles_dir, Some(config_dir), bridge)
        }
    }

    const INLINE_JSON_HOOKS: &str =
        "\n[hooks]\nargs = [\"--settings\", '{\"hooks\":{\"Stop\":[]}}']\n";

    #[tokio::test]
    async fn hooks_with_a_cmd_metacharacter_are_dropped_while_the_window_and_mcp_injection_remain()
    {
        let tree = TempDir::new("yagura-cli-test", "hooks-shim-drop");
        let profiles_dir = tree.make_dir("profiles");
        let config_dir = tree.make_dir("mcp");
        write_profile(
            &profiles_dir,
            "shim",
            &shim_profile(
                &tree,
                &format!("{}{INLINE_JSON_HOOKS}", mcp_file_table(MCP_JSON)),
            ),
        );
        let bridge = tree.make_dir("bridge").join("yagura-mcp.exe");
        let h = Harness::start(
            "hooks-shim-drop",
            shim_config(&tree, profiles_dir, &config_dir, Some(bridge)),
        )
        .await;
        let mut client = h.connect().await;

        let window_id = open_from_profile(&mut client, "shim", None).await;
        h.wait_for_scrollback(&window_id, b"<<argv[3]=").await;
        let text = String::from_utf8_lossy(&h.scrollback(&window_id)).into_owned();
        assert_eq!(
            sentinel(&text, "argv[1]").as_deref(),
            Some("own"),
            "{text:?}"
        );
        assert_eq!(
            sentinel(&text, "argv[2]").as_deref(),
            Some("--mcp-config"),
            "{text:?}"
        );
        assert!(
            !text.contains("--settings") && !text.contains("<<argv[5]="),
            "the hooks injection alone must be dropped; scrollback: {text:?}"
        );
    }

    #[tokio::test]
    async fn mcp_args_with_a_cmd_metacharacter_are_dropped_while_a_safe_hooks_injection_remains() {
        let tree = TempDir::new("yagura-cli-test", "hooks-shim-keep");
        let profiles_dir = tree.make_dir("profiles");
        let config_dir = tree.make_dir("mcp");
        let unsafe_mcp = "\n[mcp]\nargs = [\"--mcp-config\", '{\"a\":1}']\n";
        write_profile(
            &profiles_dir,
            "shim",
            &shim_profile(
                &tree,
                &format!("{unsafe_mcp}{}", hooks_file_table(HOOKS_JSON)),
            ),
        );
        let bridge = tree.make_dir("bridge").join("yagura-mcp.exe");
        let h = Harness::start(
            "hooks-shim-keep",
            shim_config(&tree, profiles_dir, &config_dir, Some(bridge)),
        )
        .await;
        let mut client = h.connect().await;

        let window_id = open_from_profile(&mut client, "shim", None).await;
        h.wait_for_scrollback(&window_id, b"<<argv[3]=").await;
        let text = String::from_utf8_lossy(&h.scrollback(&window_id)).into_owned();
        assert_eq!(
            sentinel(&text, "argv[1]").as_deref(),
            Some("own"),
            "{text:?}"
        );
        assert_eq!(
            sentinel(&text, "argv[2]").as_deref(),
            Some("--settings"),
            "{text:?}"
        );
        assert!(!text.contains("--mcp-config"), "{text:?}");
    }
}

// =============================================================================
// The answers a window's open leaves for its hooks, and its wake path
// =============================================================================

/// The answer claude's `SessionStart` hook prints: the paths the CLI watches to wake.
pub(crate) const WAKE_ANSWER: &str = "{\"hookSpecificOutput\":{\"hookEventName\":\
     \"SessionStart\",\"watchPaths\":{wake_paths}}}";

/// An answer naming no placeholder, left as written.
pub(crate) const PLAIN_ANSWER: &str = "a plain answer, \"quoted\" as written";

/// The second line a WSL guest's `wslpath -w` prints for its state base.
pub(crate) const UNC_BASE: &str = "\\\\wsl.localhost\\yagura-no-such-distro\\home\\u\\state";

/// How a guest profile's `[host_paths] state_base` prints the guest's state base: the
/// `mcp_cli_stub` `probe` subcommand, printing `printed` and ending with `exit_code`, after
/// sleeping `delay_ms` when given.
pub(crate) struct StateBase {
    pub(crate) printed: String,
    pub(crate) exit_code: i32,
    pub(crate) delay_ms: Option<u64>,
}

impl StateBase {
    /// A tool printing `printed`, its lines, and ending with 0.
    pub(crate) fn lines(printed: impl Into<String>) -> Self {
        Self {
            printed: printed.into(),
            exit_code: 0,
            delay_ms: None,
        }
    }
}

/// The profile `id` of a `launch` window running `cli`, whose `[hooks]` hands the CLI its
/// credentials file and declares `answers`. `guest` makes its CLI one in another
/// environment: `[host_paths]` with a `convert` that changes nothing, and `state_base` when
/// one is given.
pub(crate) fn wake_profile(
    id: &str,
    cli: &[String],
    answers: &[(&str, &str)],
    guest: Option<Option<StateBase>>,
) -> String {
    use toml::Value;
    let strings =
        |items: &[String]| Value::Array(items.iter().cloned().map(Value::String).collect());
    let table = |pairs: Vec<(&str, Value)>| {
        Value::Table(
            pairs
                .into_iter()
                .map(|(key, value)| (key.to_string(), value))
                .collect(),
        )
    };
    let mut document = toml::Table::new();
    document.insert("schema".into(), Value::Integer(1));
    document.insert("display_name".into(), Value::String(id.to_string()));
    document.insert("command".into(), table(vec![("default", strings(cli))]));
    document.insert(
        "credentials".into(),
        table(vec![("mode", Value::String("launch".into()))]),
    );
    let answers = answers
        .iter()
        .map(|(name, text)| (*name, Value::String((*text).to_string())))
        .collect();
    document.insert(
        "hooks".into(),
        table(vec![
            (
                "args",
                strings(&["--creds".to_string(), "{credentials_file}".to_string()]),
            ),
            ("answers", table(answers)),
        ]),
    );
    if let Some(state_base) = guest {
        let mut host_paths = vec![(
            "convert",
            strings(&crate::credentials_launch::unchanged_convert()),
        )];
        let mut env = Vec::new();
        if let Some(state_base) = state_base {
            host_paths.push((
                "state_base",
                strings(&[
                    common::mcp_cli_stub_path().display().to_string(),
                    "probe".to_string(),
                    "state".to_string(),
                ]),
            ));
            env.push((
                "MCP_CLI_STUB_PROBE_OUTPUT",
                Value::String(state_base.printed),
            ));
            env.push((
                "MCP_CLI_STUB_PROBE_EXIT_CODE",
                Value::String(state_base.exit_code.to_string()),
            ));
            if let Some(delay_ms) = state_base.delay_ms {
                env.push(("MCP_CLI_STUB_DELAY_MS", Value::String(delay_ms.to_string())));
            }
        }
        document.insert("host_paths".into(), table(host_paths));
        document.insert("env".into(), table(env));
    }
    toml::to_string(&document).expect("serialize the profile")
}

/// Blocking: the folder under `root` (at any depth) of the credentials file naming
/// `window_id`, found by its shape (`window_credentials` in `yagura-protocol`) rather
/// than its name.
pub(crate) fn credentials_folder_of(root: &Path, window_id: &str) -> Option<PathBuf> {
    let entries = std::fs::read_dir(root).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            if let Some(found) = credentials_folder_of(&path, window_id) {
                return Some(found);
            }
        } else if kind.is_file()
            && entry.metadata().is_ok_and(|meta| meta.len() <= 4096)
            && std::fs::read(&path)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
                .is_some_and(|value| {
                    // The shape of a credentials file: these two fields and nothing else.
                    value.as_object().is_some_and(|fields| fields.len() == 2)
                        && value["window_id"] == window_id
                        && value["token"].is_string()
                })
        {
            return path.parent().map(Path::to_path_buf);
        }
    }
    None
}

/// The answer `name` the open of `window_id` left beside its credentials file under
/// `root`; `None` when it left none.
pub(crate) fn answer_of(root: &Path, window_id: &str, name: &str) -> Option<String> {
    let folder = credentials_folder_of(root, window_id)
        .unwrap_or_else(|| panic!("no credentials file names {window_id} under {root:?}"));
    std::fs::read_to_string(folder.join(format!("answer-{name}"))).ok()
}

/// The wake paths the `SessionStart` answer ([`WAKE_ANSWER`]) of `window_id` hands its
/// CLI; `None` when its open left no such answer.
pub(crate) fn wake_paths_of(root: &Path, window_id: &str) -> Option<Vec<String>> {
    let answer = answer_of(root, window_id, "SessionStart")?;
    let value: serde_json::Value = serde_json::from_str(&answer)
        .unwrap_or_else(|err| panic!("the answer must read as JSON ({err}): {answer:?}"));
    let paths = value["hookSpecificOutput"]["watchPaths"]
        .as_array()
        .unwrap_or_else(|| panic!("the wake paths must be a JSON array: {answer:?}"));
    Some(
        paths
            .iter()
            .map(|path| {
                path.as_str()
                    .unwrap_or_else(|| panic!("each wake path is a string: {answer:?}"))
                    .to_string()
            })
            .collect(),
    )
}

/// The CLI's path of the wake file in the slot `slot` under the guest state base `linux`,
/// for the instance `instance` (`None`: the default one).
pub(crate) fn guest_wake_path(linux: &str, instance: Option<&str>, slot: u32) -> String {
    let namespace = instance.map_or_else(|| "yagura".to_string(), |id| format!("yagura-{id}"));
    format!("{linux}/{namespace}/host-wake/{slot}/yagura_wake")
}

/// Waits until the folder `folder` is gone, as a window's credentials folder is once its
/// child has ended.
pub(crate) async fn until_gone(folder: &Path) {
    let deadline = tokio::time::Instant::now() + yagura_test_support::HANG_GUARD;
    while folder.exists() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "{folder:?} outlived its window"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

/// A config for the wake cases: [`daemon_config`] whose playbook also lets a window close.
fn wake_config(profiles_dir: PathBuf, config_dir: &Path) -> DaemonConfig {
    DaemonConfig {
        approval_policy: Some(common::policy(&["open_window", "close_window"])),
        ..daemon_config(profiles_dir, Some(config_dir), None)
    }
}

async fn close(client: &mut Connection, window_id: &str) {
    send(
        client,
        &ClientMessage::CloseWindow {
            window_id: window_id.to_string(),
            force: None,
        },
    )
    .await;
    match read_response(client).await {
        DaemonMessage::WindowClosed { .. } => {}
        other => panic!("expected window_closed, got {other:?}"),
    }
}

/// A `launch` window's open leaves each answer of its `[hooks]` beside its credentials file,
/// a text naming no placeholder as written, and `{wake_paths}` filled once with the JSON
/// array of its wake path: `yagura_wake` in its credentials folder, named as its
/// credentials file is (with `/` on Windows), whose characters (a space and the
/// placeholder's own spelling here) stay as they are. The open makes no wake file itself.
#[tokio::test]
async fn a_launch_windows_answers_are_left_beside_its_credentials_with_its_wake_path_as_json() {
    let tree = TempDir::new("yagura-cli-test", "wake-answers");
    let profiles_dir = tree.make_dir("profiles");
    let config_dir = tree.make_dir("mcp {wake_paths} x");
    let cli = crate::credentials_launch::quiet_cli(&tree.make_dir("logs"));
    write_profile(
        &profiles_dir,
        "wake-native",
        &wake_profile(
            "wake-native",
            &cli,
            &[("SessionStart", WAKE_ANSWER), ("Plain", PLAIN_ANSWER)],
            None,
        ),
    );
    let h = Harness::start("wake-answers", wake_config(profiles_dir, &config_dir)).await;
    let mut client = h.connect().await;

    let window_id = open_from_profile(&mut client, "wake-native", None).await;
    let folder = credentials_folder_of(&config_dir, &window_id)
        .expect("a launch window has its credentials folder");
    assert_eq!(
        answer_of(&config_dir, &window_id, "Plain").as_deref(),
        Some(PLAIN_ANSWER),
        "an answer naming no placeholder is left as written"
    );
    let folder_arg = folder.to_str().expect("a UTF-8 folder");
    let folder_arg = if cfg!(windows) {
        folder_arg.replace('\\', "/")
    } else {
        folder_arg.to_string()
    };
    assert_eq!(
        wake_paths_of(&config_dir, &window_id),
        Some(vec![format!("{folder_arg}/yagura_wake")]),
        "{{wake_paths}} is filled with the wake path in the window's credentials folder"
    );
    assert!(
        !folder.join("yagura_wake").exists(),
        "the open names the wake file and makes none"
    );
    h.registry.terminate_all().await;
}

/// A window whose CLI runs in another environment has its wake file in a slot under the
/// guest's state base its `[host_paths] state_base` prints (`<line 1>/yagura/host-wake/
/// <slot>/yagura_wake` for the CLI): the lowest slot no living or opening window holds. A
/// second window takes the next one; a window opened once the first has ended takes the
/// first's again; an open that fails half-way gives its slot back. Characters a JSON string
/// escapes stay as they are in the path the CLI is handed.
#[tokio::test]
async fn a_guest_windows_wake_path_is_in_the_lowest_slot_no_living_window_holds() {
    const LINUX_BASE: &str = "/home/u \"q\" \\b {wake_paths}/state";
    let tree = TempDir::new("yagura-cli-test", "wake-slots");
    let profiles_dir = tree.make_dir("profiles");
    let config_dir = tree.make_dir("mcp");
    let cli = crate::credentials_launch::quiet_cli(&tree.make_dir("logs"));
    let printed = format!("{LINUX_BASE}\n{UNC_BASE}");
    let answers = [("SessionStart", WAKE_ANSWER)];
    write_profile(
        &profiles_dir,
        "wake-guest",
        &wake_profile(
            "wake-guest",
            &cli,
            &answers,
            Some(Some(StateBase::lines(printed.clone()))),
        ),
    );
    let broken = tree.make_dir("broken").join(if cfg!(windows) {
        "broken.exe"
    } else {
        "broken"
    });
    std::fs::write(&broken, b"this is not an executable image\n").unwrap();
    write_profile(
        &profiles_dir,
        "wake-broken",
        &wake_profile(
            "wake-broken",
            &[broken.display().to_string()],
            &answers,
            Some(Some(StateBase::lines(printed))),
        ),
    );
    let h = Harness::start("wake-slots", wake_config(profiles_dir, &config_dir)).await;
    let mut client = h.connect().await;
    let slot = |n| Some(vec![guest_wake_path(LINUX_BASE, None, n)]);

    let first = open_from_profile(&mut client, "wake-guest", None).await;
    assert_eq!(
        wake_paths_of(&config_dir, &first),
        slot(0),
        "the first window"
    );
    let second = open_from_profile(&mut client, "wake-guest", None).await;
    assert_eq!(
        wake_paths_of(&config_dir, &second),
        slot(1),
        "a second window while the first lives"
    );

    let first_folder = credentials_folder_of(&config_dir, &first).unwrap();
    close(&mut client, &first).await;
    until_gone(&first_folder).await;
    let third = open_from_profile(&mut client, "wake-guest", None).await;
    assert_eq!(
        wake_paths_of(&config_dir, &third),
        slot(0),
        "a window opened once the first has ended takes its slot again"
    );

    send(
        &mut client,
        &ClientMessage::OpenWindowFromProfile {
            profile: "wake-broken".to_string(),
            cwd: None,
            name: None,
            model: None,
            effort: None,
            on_name_taken: Default::default(),
        },
    )
    .await;
    let failed = read_response(&mut client).await;
    assert!(
        !matches!(failed, DaemonMessage::WindowOpened { .. }),
        "a window whose program cannot run fails to open, got {failed:?}"
    );
    let fourth = open_from_profile(&mut client, "wake-guest", None).await;
    assert_eq!(
        wake_paths_of(&config_dir, &fourth),
        slot(2),
        "an open that failed half-way gave its slot back"
    );
    h.registry.terminate_all().await;
}

/// A window whose CLI runs in another environment has no wake record, and so no
/// `{wake_paths}` answer and no slot, when its state base cannot be had: the second line
/// is no `\\wsl.localhost\` or `\\wsl$\` path, the first does not start with `/`, a line
/// holds a control character, the tool fails or outlasts the open's bound, or the profile
/// has no `state_base`; nor when no answer names `{wake_paths}`. Each window opens all the
/// same, with its other answers. A `\\wsl$\` base is one.
#[tokio::test]
async fn a_guest_window_whose_state_base_cannot_be_had_opens_with_no_wake_record() {
    let usable = format!("/home/u/state\n{UNC_BASE}");
    let cases: Vec<(&str, Option<StateBase>, bool)> = vec![
        (
            "base-not-unc",
            Some(StateBase::lines("/home/u/state\nD:\\wsl\\state")),
            true,
        ),
        (
            "base-not-absolute",
            Some(StateBase::lines(format!("home/u/state\n{UNC_BASE}"))),
            true,
        ),
        (
            "base-control-linux",
            Some(StateBase::lines(format!("/home/u\u{1}/state\n{UNC_BASE}"))),
            true,
        ),
        (
            "base-control-unc",
            Some(StateBase::lines(format!("/home/u/state\n{UNC_BASE}\u{1}"))),
            true,
        ),
        (
            "base-fails",
            Some(StateBase {
                printed: usable.clone(),
                exit_code: 1,
                delay_ms: None,
            }),
            true,
        ),
        ("base-absent", None, true),
        (
            "wake-unnamed",
            Some(StateBase::lines(usable.clone())),
            false,
        ),
    ];
    let tree = TempDir::new("yagura-cli-test", "wake-unusable");
    let profiles_dir = tree.make_dir("profiles");
    let config_dir = tree.make_dir("mcp");
    let cli = crate::credentials_launch::quiet_cli(&tree.make_dir("logs"));
    let named = [("SessionStart", WAKE_ANSWER), ("Plain", PLAIN_ANSWER)];
    let unnamed = [("Plain", PLAIN_ANSWER)];
    let mut ids = Vec::new();
    for (id, state_base, names_wake) in cases {
        let answers: &[(&str, &str)] = if names_wake { &named } else { &unnamed };
        write_profile(
            &profiles_dir,
            id,
            &wake_profile(id, &cli, answers, Some(state_base)),
        );
        ids.push(id);
    }
    let wsl_dollar = UNC_BASE.replace("wsl.localhost", "wsl$");
    for (id, printed) in [
        ("wake-dollar", format!("/home/u/dollar\n{wsl_dollar}")),
        ("wake-usable", usable.clone()),
    ] {
        write_profile(
            &profiles_dir,
            id,
            &wake_profile(id, &cli, &named, Some(Some(StateBase::lines(printed)))),
        );
    }
    let h = Harness::start(
        "wake-unusable",
        wake_config(profiles_dir.clone(), &config_dir),
    )
    .await;
    let mut client = h.connect().await;

    let dollar = open_from_profile(&mut client, "wake-dollar", None).await;
    assert_eq!(
        wake_paths_of(&config_dir, &dollar),
        Some(vec![guest_wake_path("/home/u/dollar", None, 0)]),
        "a wsl$ base is one"
    );
    let dollar_folder = credentials_folder_of(&config_dir, &dollar).unwrap();
    close(&mut client, &dollar).await;
    until_gone(&dollar_folder).await;
    for id in &ids {
        let window_id = open_from_profile(&mut client, id, None).await;
        assert_eq!(
            answer_of(&config_dir, &window_id, "Plain").as_deref(),
            Some(PLAIN_ANSWER),
            "{id}: the window opens with its other answers"
        );
        assert_eq!(
            wake_paths_of(&config_dir, &window_id),
            None,
            "{id}: a window without a wake record is left no {{wake_paths}} answer"
        );
    }
    let usable_window = open_from_profile(&mut client, "wake-usable", None).await;
    assert_eq!(
        wake_paths_of(&config_dir, &usable_window),
        Some(vec![guest_wake_path("/home/u/state", None, 0)]),
        "no window above took a slot"
    );
    h.registry.terminate_all().await;

    // A tool outlasting the open's bound, with a daemon whose bound is short. A delay rather
    // than a hold: the file releasing a hold goes with this test's tree, which may be gone
    // before the tool looks for it.
    write_profile(
        &profiles_dir,
        "base-held",
        &wake_profile(
            "base-held",
            &cli,
            &named,
            Some(Some(StateBase {
                printed: usable,
                exit_code: 0,
                delay_ms: Some(6000),
            })),
        ),
    );
    let mut config = wake_config(profiles_dir, &config_dir);
    config.mcp_listing_timeout = std::time::Duration::from_millis(2000);
    let h = Harness::start("wake-unusable-held", config).await;
    let mut client = h.connect().await;
    let held = open_from_profile(&mut client, "base-held", None).await;
    assert_eq!(
        answer_of(&config_dir, &held, "Plain").as_deref(),
        Some(PLAIN_ANSWER),
        "base-held: the window opens with its other answers"
    );
    assert_eq!(
        wake_paths_of(&config_dir, &held),
        None,
        "base-held: a state base outlasting the bound gives no wake record"
    );
    h.registry.terminate_all().await;
}
