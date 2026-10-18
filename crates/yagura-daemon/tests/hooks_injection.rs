#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration tests: a launch profile's `[hooks]` table is rendered into the
//! child's argv and a settings file on disk when a window opens, next to (and
//! independent of) the `[mcp]` injection.
//!
//! Every property is measured through a real spawned child (`argv_probe`):
//! the argv the child received and the file its argv names are what a CLI
//! would see. The table's keys are `args` (required), `config_file`
//! (optional) and the placeholder `{hooks_config_file}`.

mod common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use common::{
    Harness, VALID_TOKEN, approver_base_config, call_open_window, empty_lookup, read_response,
    send, write_profile,
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
    DaemonConfig {
        profiles_dir: Some(profiles_dir),
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
    match read_response(client, Duration::from_secs(5)).await {
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
    h.wait_for_scrollback(&window_id, b"<<argv[6]=", Duration::from_secs(10))
        .await;
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
        h.wait_for_scrollback(&window_id, b"<<argv[2]=", Duration::from_secs(10))
            .await;
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
    h.wait_for_scrollback(&window_id, b"<<argv[2]=", Duration::from_secs(10))
        .await;
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
    h.wait_until_child_exited(&window_id, Duration::from_secs(10))
        .await;
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
    h.wait_for_scrollback(&window_id, b"<<argv[2]=", Duration::from_secs(10))
        .await;
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
    h.wait_for_scrollback(&window_id, b"<<argv[1]=own>>", Duration::from_secs(10))
        .await;
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
        h.wait_for_scrollback(&window_id, b"<<argv[3]=", Duration::from_secs(10))
            .await;
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
        h.wait_for_scrollback(&window_id, b"<<argv[3]=", Duration::from_secs(10))
            .await;
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
