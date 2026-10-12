#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: the IPC surface of launch profiles -- `ListProfiles` /
//! `OpenWindowFromProfile` -- plus the existing `OpenWindow` contract changes
//! that resolving the executable through `yagura-os` brings with it,
//! including the fixture for npm's extensionless POSIX shim next to a
//! same-named `.cmd` file (`resolve_program`'s Windows extension probe).
//!
//! The daemon-side profile module (`yagura_daemon::profile`) and the new
//! protocol messages (`ClientMessage::ListProfiles` /
//! `ClientMessage::OpenWindowFromProfile` / `DaemonMessage::ProfileList` /
//! `ProfileInfo`) are fixed here as public contracts: this file is itself
//! the one place their exact names and shapes are specified, so nothing
//! outside it needs to restate them.
//!
//! Every fixture profile is written by the test itself rather than living
//! under `tests/fixtures/profiles/` (that directory is `profile_load.rs`'s
//! own unit-level fixture set), the same
//! way `profile_load.rs` writes its two properties no shared fixture can hold
//! (`write_absolute_cwd_profile`, the metacharacter-argument profile).
//!
//! Availability is always decided against an injected `program_lookup`, never
//! against what happens to be installed on the machine running the suite
//! (same discipline as `profile_load.rs`): every program name a profile names
//! is deliberately unlikely to exist anywhere.

mod common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use common::{
    Harness, SubmitDefinitionInput, call_list_windows, call_send_handoff_from_profile,
    call_send_handoff_from_profile_with_options, call_send_handoff_from_profile_within,
    count_marker, decide, empty_lookup, expect_approval_requested_within, expect_error,
    expect_handoff_sent, handoff_profile_config, identify, policy, read_response,
    register_approver, send, send_handoff_from_profile,
    submit_child_profile_source_with_detection_and_composer,
    wait_for_accepted_handoff_envelope_json, write_profile, write_submit_definition,
};

use yagura_daemon::profile::ProfileOs;
use yagura_daemon::{DaemonConfig, OpenWindowRequest, RequestedCwd};
use yagura_os::program::ProgramLookup;
use yagura_protocol::{
    ClientMessage, DaemonMessage, HANDOFF_NOTE_MAX_BYTES, HANDOFF_REFERENCE_MAX_BYTES,
    HandoffDelivery, ProfileInfo, WindowInfo,
};
use yagura_test_support::ipc::Connection;
use yagura_test_support::paths::TempDir;
use yagura_test_support::paths::strip_extended_length_prefix;
use yagura_test_support::process::{
    argv_probe_profile_source, echo_child_command, echo_child_path, ready_child_path,
    ready_child_profile_source_with_composer,
};
// `file_len` / `wait_until_file_grows` back the Windows-only shim-argument
// fixtures below (npm-style extensionless shim, cmd.exe verbatim-argument
// delivery); every call site is under `#[cfg(windows)]`, so a plain,
// unconditional `use` is an unused import on every other target.
#[cfg(windows)]
use yagura_test_support::process::{file_len, wait_until_file_grows};

// --- fixtures: profiles are written ad hoc, not read from a shared directory ----

/// Writes a program under `dir` this host actually resolves for the bare
/// name `name`: a `.exe` file on Windows (`Resolution::Direct` -- a
/// same-name extensionless file is never probed by `resolve_program`) and
/// an extensionless file with the execute bit set on Unix.
#[cfg(windows)]
fn write_available_program(dir: &Path, name: &str) {
    std::fs::write(dir.join(format!("{name}.exe")), [])
        .expect("program fixture should be writable");
}

#[cfg(not(windows))]
fn write_available_program(dir: &Path, name: &str) {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    std::fs::write(&path, []).expect("program fixture should be writable");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .expect("program fixture should be made executable");
}

/// A profile whose `[command]` launches the real `echo_child` fixture binary
/// by absolute path, so opening it actually spawns a well-behaved child no
/// resolution lookup is needed for (an absolute path is left untouched by
/// `resolve_program`).
fn echo_child_profile_source(display_name: &str) -> String {
    let path = echo_child_path();
    format!(
        "schema = 1\ndisplay_name = \"{display_name}\"\n\n[command]\ndefault = ['{}']\n",
        path.display()
    )
}

fn echo_child_profile_source_with_cwd(display_name: &str, cwd: &Path) -> String {
    let path = echo_child_path();
    format!(
        "schema = 1\ndisplay_name = \"{display_name}\"\ncwd = '{}'\n\n[command]\ndefault = ['{}']\n",
        cwd.display(),
        path.display()
    )
}

fn echo_child_profile_source_with_detection(display_name: &str, detection: &str) -> String {
    let path = echo_child_path();
    format!(
        "schema = 1\ndisplay_name = \"{display_name}\"\ndetection = \"{detection}\"\n\n[command]\ndefault = ['{}']\n",
        path.display()
    )
}

/// Same as [`ready_child_profile_source_with_composer`], with a `cwd` default
/// the launch carries unless the caller overrides it.
fn ready_child_profile_source_with_cwd(display_name: &str, cwd: &Path) -> String {
    let path = ready_child_path();
    format!(
        "schema = 1\ndisplay_name = \"{display_name}\"\ncwd = '{}'\n\n[command]\ndefault = \
         ['{}', '{}', '{}', \"{}\", \"{}\", '0']\n",
        cwd.display(),
        path.display(),
        yagura_test_support::detection::CONTROL_READY_MARKER,
        yagura_test_support::process::COMPOSER_MARKER,
        yagura_test_support::process::COMPOSER_SEPARATOR,
        yagura_test_support::process::COMPOSER_CONTINUATION_PREFIX,
    )
}

/// A profile whose `[command]` names no program at all for the host
/// operating system: every *other* operating system's key is present (so a
/// selection bug that ignores the per-OS keys would still find an argv), but
/// the host's own key, and `default`, are both absent.
fn profile_source_with_no_argv_for_host() -> String {
    let other_os_keys = if cfg!(windows) {
        "macos = [\"macos-only\"]\nlinux = [\"linux-only\"]\n"
    } else if cfg!(target_os = "macos") {
        "windows = [\"windows-only\"]\nlinux = [\"linux-only\"]\n"
    } else {
        "windows = [\"windows-only\"]\nmacos = [\"macos-only\"]\n"
    };
    format!("schema = 1\ndisplay_name = \"No Host Argv\"\n\n[command]\n{other_os_keys}")
}

/// A profile whose `[command]` prints the environment the child actually
/// received, via the shell's own `echo` (the same deviation `status_token.rs`
/// documents: it is the only way to observe a real child's environment).
/// `YAGURA_WINDOW_ID` is reserved and must be dropped by the daemon before
/// the child ever sees it; `KEPT` is not reserved and must survive.
fn reserved_env_profile_source() -> String {
    let command = if cfg!(windows) {
        "windows = [\"cmd.exe\", \"/c\", \"echo %YAGURA_WINDOW_ID% [%KEPT%]\"]\n"
    } else {
        "linux = [\"/bin/sh\", \"-c\", \"echo $YAGURA_WINDOW_ID [$KEPT]\"]\n\
         macos = [\"/bin/sh\", \"-c\", \"echo $YAGURA_WINDOW_ID [$KEPT]\"]\n"
    };
    format!(
        "schema = 1\ndisplay_name = \"Reserved Env Launch\"\n\n[command]\n{command}\n[env]\n\
         YAGURA_WINDOW_ID = \"hijacked\"\nKEPT = \"kept-value\"\n"
    )
}

// --- injected resolution environment (mirrors profile_load.rs) -------------

fn file_exists(path: &Path) -> bool {
    path.is_file()
}

fn lookup_in(dir: &Path) -> ProgramLookup {
    ProgramLookup {
        path_dirs: vec![dir.to_path_buf()],
        extra_bin_dirs: Vec::new(),
        windows_comspec: Some(PathBuf::from("comspec-placeholder").join("cmd.exe")),
    }
}

fn config_with(profiles_dir: Option<PathBuf>, lookup: ProgramLookup) -> DaemonConfig {
    DaemonConfig {
        profiles_dir,
        host_os: ProfileOs::host(),
        program_lookup: lookup,
        program_is_executable: file_exists,
        default_shell: None,
        detection_dir: None,
        ..common::opening_config()
    }
}

// --- IPC round trips ---------------------------------------------------------

async fn call_list_profiles(client: &mut Connection) -> (Vec<ProfileInfo>, Vec<String>) {
    send(client, &ClientMessage::ListProfiles).await;
    match read_response(client, Duration::from_secs(5)).await {
        DaemonMessage::ProfileList { profiles, warnings } => (profiles, warnings),
        other => panic!("expected profile_list, got {other:?}"),
    }
}

async fn call_open_window_from_profile(
    client: &mut Connection,
    profile: &str,
    cwd: Option<&str>,
    name: Option<&str>,
) -> DaemonMessage {
    call_open_window_from_profile_with_options(client, profile, cwd, name, None, None).await
}

/// [`call_open_window_from_profile`] carrying the two launch options only the
/// profile form of the request accepts.
async fn call_open_window_from_profile_with_options(
    client: &mut Connection,
    profile: &str,
    cwd: Option<&str>,
    name: Option<&str>,
    model: Option<&str>,
    effort: Option<&str>,
) -> DaemonMessage {
    send(
        client,
        &ClientMessage::OpenWindowFromProfile {
            profile: profile.to_string(),
            cwd: cwd.map(str::to_string),
            name: name.map(str::to_string),
            model: model.map(str::to_string),
            effort: effort.map(str::to_string),
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

fn find_profile<'a>(profiles: &'a [ProfileInfo], id: &str) -> &'a ProfileInfo {
    profiles
        .iter()
        .find(|p| p.id == id)
        .unwrap_or_else(|| panic!("profile {id:?} not found in listing: {profiles:?}"))
}

fn find_window<'a>(windows: &'a [WindowInfo], id: &str) -> &'a WindowInfo {
    windows
        .iter()
        .find(|w| w.window_id == id)
        .unwrap_or_else(|| panic!("window {id:?} not found in window_list: {windows:?}"))
}

/// Polls `list_windows` on `client` until a window with the given `vendor`
/// (the profile identifier) appears, and returns it. Used to observe
/// that a profile-addressed handoff has already opened its window --
/// synchronously, before the readiness wait -- without knowing the new
/// window's id ahead of time, and without racing the connection's own
/// still-pending handoff response (which, for every fixture this helper is
/// used with, never arrives during the polling window).
async fn wait_for_window_with_vendor(
    client: &mut Connection,
    vendor: &str,
    timeout: Duration,
) -> WindowInfo {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let windows = call_list_windows(client).await;
        if let Some(found) = windows
            .into_iter()
            .find(|w| w.vendor.as_deref() == Some(vendor))
        {
            return found;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("timed out waiting for a window with vendor {vendor:?} to appear");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

// =============================================================================
// Listing: availability, reason, warnings, re-read on every call
// =============================================================================

#[tokio::test]
async fn list_profiles_reports_availability_and_a_reason_per_entry() {
    let tree = TempDir::new("yagura-cli-test", "ipc-profile-list-availability");
    let bin = tree.make_dir("bin");
    write_available_program(&bin, "avail-program");
    let profiles_dir = tree.make_dir("profiles");
    write_profile(
        &profiles_dir,
        "avail",
        "schema = 1\ndisplay_name = \"Available\"\n\n[command]\ndefault = [\"avail-program\"]\n",
    );
    write_profile(
        &profiles_dir,
        "unavail",
        "schema = 1\ndisplay_name = \"Unavailable\"\n\n[command]\ndefault = \
         [\"definitely-not-present-program\"]\n",
    );

    let config = config_with(Some(profiles_dir), lookup_in(&bin));
    let harness = Harness::start("ipc-profile-list-availability", config).await;
    let mut client = harness.connect().await;

    let (profiles, _warnings): (Vec<ProfileInfo>, Vec<String>) =
        call_list_profiles(&mut client).await;

    let avail = find_profile(&profiles, "avail");
    assert!(
        avail.available,
        "a program the injected lookup finds must be reported available: {avail:?}"
    );
    assert_eq!(
        avail.unavailable_reason, None,
        "an available entry must carry no reason: {avail:?}"
    );

    let unavail = find_profile(&profiles, "unavail");
    assert!(
        !unavail.available,
        "a program the injected lookup does not find must be reported unavailable: {unavail:?}"
    );
    assert!(
        unavail.unavailable_reason.is_some(),
        "an unavailable entry must carry a reason: {unavail:?}"
    );
}

#[tokio::test]
async fn list_profiles_warns_about_an_excluded_file_and_leaves_it_out_of_the_listing() {
    let tree = TempDir::new("yagura-cli-test", "ipc-profile-list-warnings");
    let profiles_dir = tree.make_dir("profiles");
    write_profile(
        &profiles_dir,
        "valid",
        "schema = 1\ndisplay_name = \"Valid\"\n\n[command]\ndefault = [\"valid-program\"]\n",
    );
    write_profile(
        &profiles_dir,
        "unsupported-schema",
        "schema = 2\ndisplay_name = \"Unsupported\"\n\n[command]\ndefault = [\"x\"]\n",
    );

    let config = config_with(Some(profiles_dir), empty_lookup());
    let harness = Harness::start("ipc-profile-list-warnings", config).await;
    let mut client = harness.connect().await;

    let (profiles, warnings): (Vec<ProfileInfo>, Vec<String>) =
        call_list_profiles(&mut client).await;

    assert!(
        profiles.iter().any(|p| p.id == "valid"),
        "the valid file must still be listed: {profiles:?}"
    );
    assert!(
        !profiles.iter().any(|p| p.id == "unsupported-schema"),
        "an excluded file must not appear in the listing: {profiles:?}"
    );
    assert!(
        !warnings.is_empty(),
        "an excluded file must produce at least one warning"
    );
}

#[tokio::test]
async fn list_profiles_never_leaks_a_secret_shaped_value_from_a_syntax_error_over_ipc() {
    // Written by the test itself rather than read from `tests/fixtures/profiles/`: this
    // file's own module doc reserves that directory for `profile_load.rs`. The value is
    // a deliberately fake placeholder, never a realistic credential.
    let tree = TempDir::new("yagura-cli-test", "ipc-profile-list-syntax-error-secret");
    let profiles_dir = tree.make_dir("profiles");
    write_profile(
        &profiles_dir,
        "syntax-error-with-secret",
        "schema = 1\ndisplay_name = \"Syntax Error\"\n\n[env]\nAPI_KEY = not-a-real-token-0000\n",
    );

    let config = config_with(Some(profiles_dir), empty_lookup());
    let harness = Harness::start("ipc-profile-list-syntax-error-secret", config).await;
    let mut client = harness.connect().await;

    let (profiles, warnings): (Vec<ProfileInfo>, Vec<String>) =
        call_list_profiles(&mut client).await;

    assert!(
        !profiles.iter().any(|p| p.id == "syntax-error-with-secret"),
        "a file with a syntax error must not be listed: {profiles:?}"
    );
    assert!(
        !warnings.is_empty(),
        "a syntax error must still produce a warning naming the file"
    );
    for warning in &warnings {
        assert!(
            !warning.contains("not-a-real-token-0000"),
            "the same secret-shaped value that never leaves the daemon-side API must not \
             reach an MCP or GUI client either: {warning:?}"
        );
    }
}

#[tokio::test]
async fn list_profiles_reflects_a_file_added_between_two_calls() {
    let tree = TempDir::new("yagura-cli-test", "ipc-profile-list-reread");
    let profiles_dir = tree.make_dir("profiles");
    write_profile(
        &profiles_dir,
        "first",
        "schema = 1\ndisplay_name = \"First\"\n\n[command]\ndefault = [\"first-program\"]\n",
    );

    let config = config_with(Some(profiles_dir.clone()), empty_lookup());
    let harness = Harness::start("ipc-profile-list-reread", config).await;
    let mut client = harness.connect().await;

    let (first_call, _): (Vec<ProfileInfo>, Vec<String>) = call_list_profiles(&mut client).await;
    let first_ids: Vec<&str> = first_call.iter().map(|p| p.id.as_str()).collect();
    assert_eq!(
        first_ids,
        vec!["first"],
        "the first call must see only the file present at that time"
    );

    write_profile(
        &profiles_dir,
        "second",
        "schema = 1\ndisplay_name = \"Second\"\n\n[command]\ndefault = [\"second-program\"]\n",
    );

    let (second_call, _): (Vec<ProfileInfo>, Vec<String>) = call_list_profiles(&mut client).await;
    assert!(
        second_call.iter().any(|p| p.id == "second"),
        "a file added after the first call must appear in the next listing without restarting \
         the daemon: {second_call:?}"
    );
}

// =============================================================================
// The `command` a listing surfaces: the same unresolved argv a launch would
// carry, applying the operating-system selection
// =============================================================================

#[tokio::test]
async fn list_profiles_reports_the_argv_a_launch_of_it_would_carry_as_command() {
    let tree = TempDir::new("yagura-cli-test", "ipc-profile-list-command");
    let profiles_dir = tree.make_dir("profiles");
    write_profile(
        &profiles_dir,
        "with-command",
        "schema = 1\ndisplay_name = \"With Command\"\n\n[command]\ndefault = [\"codex\", \
         \"--flag\"]\n",
    );

    let config = config_with(Some(profiles_dir), empty_lookup());
    let harness = Harness::start("ipc-profile-list-command", config).await;
    let mut client = harness.connect().await;

    let (profiles, _warnings): (Vec<ProfileInfo>, Vec<String>) =
        call_list_profiles(&mut client).await;

    let entry = find_profile(&profiles, "with-command");
    assert_eq!(
        entry.command,
        vec!["codex".to_string(), "--flag".to_string()],
        "the listing carries the same unresolved argv a launch would carry: {entry:?}"
    );
}

#[tokio::test]
async fn list_profiles_leaves_out_a_profile_with_no_argv_for_this_operating_system() {
    let tree = TempDir::new("yagura-cli-test", "ipc-profile-list-no-host-argv");
    let profiles_dir = tree.make_dir("profiles");
    write_profile(
        &profiles_dir,
        "no-host-argv",
        &profile_source_with_no_argv_for_host(),
    );
    write_profile(
        &profiles_dir,
        "with-command",
        "schema = 1
display_name = \"With Command\"

[command]
default = [\"codex\"]
",
    );

    let config = config_with(Some(profiles_dir), empty_lookup());
    let harness = Harness::start("ipc-profile-list-no-host-argv", config).await;
    let mut client = harness.connect().await;

    let (profiles, warnings): (Vec<ProfileInfo>, Vec<String>) =
        call_list_profiles(&mut client).await;

    let ids: Vec<&str> = profiles.iter().map(|p| p.id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["with-command"],
        "a profile naming no argv for this operating system has nothing to launch here, so          it is left out rather than listed with an empty command: {profiles:?}"
    );
    assert!(
        warnings.is_empty(),
        "leaving out a profile this operating system cannot launch is not a problem to          report: {warnings:?}"
    );
}

// =============================================================================
// Profile launch content: vendor / name default / cwd+name override /
// detection carry / reserved environment key dropped
// =============================================================================

#[tokio::test]
async fn open_window_from_profile_carries_the_identifier_as_vendor_and_the_display_name_as_the_default_name()
 {
    let tree = TempDir::new("yagura-cli-test", "ipc-profile-launch-vendor-name");
    let profiles_dir = tree.make_dir("profiles");
    write_profile(
        &profiles_dir,
        "vendor-check",
        &echo_child_profile_source("Vendor Check"),
    );

    let config = config_with(Some(profiles_dir), empty_lookup());
    let harness = Harness::start("ipc-profile-launch-vendor-name", config).await;
    let mut client = harness.connect().await;

    let window_id = expect_window_opened(
        call_open_window_from_profile(&mut client, "vendor-check", None, None).await,
    );
    let windows = call_list_windows(&mut client).await;
    let info = find_window(&windows, &window_id);

    assert_eq!(
        info.vendor.as_deref(),
        Some("vendor-check"),
        "vendor must be the profile identifier: {info:?}"
    );
    assert_eq!(
        info.name, "Vendor Check",
        "name must default to the profile's display_name when the caller supplies none: {info:?}"
    );

    harness.registry.terminate_all().await;
}

#[tokio::test]
async fn open_window_from_profile_prefers_the_requested_cwd_and_name_over_the_profile_defaults() {
    let tree = TempDir::new("yagura-cli-test", "ipc-profile-launch-override");
    let profile_default_cwd = tree.make_dir("profile-default-cwd");
    let requested_cwd = tree.make_dir("requested-cwd");
    let profiles_dir = tree.make_dir("profiles");
    write_profile(
        &profiles_dir,
        "override-check",
        &echo_child_profile_source_with_cwd("Override Check", &profile_default_cwd),
    );

    let config = config_with(Some(profiles_dir), empty_lookup());
    let harness = Harness::start("ipc-profile-launch-override", config).await;
    let mut client = harness.connect().await;

    let requested_cwd_str = requested_cwd.to_string_lossy().into_owned();
    let window_id = expect_window_opened(
        call_open_window_from_profile(
            &mut client,
            "override-check",
            Some(&requested_cwd_str),
            Some("Custom Name"),
        )
        .await,
    );
    let windows = call_list_windows(&mut client).await;
    let info = find_window(&windows, &window_id);

    assert_eq!(
        info.name, "Custom Name",
        "a caller-supplied name must win over the profile's display_name: {info:?}"
    );
    let expected_cwd = strip_extended_length_prefix(
        &requested_cwd
            .canonicalize()
            .expect("requested cwd must canonicalize"),
    )
    .to_string_lossy()
    .into_owned();
    assert_eq!(
        info.cwd, expected_cwd,
        "a caller-supplied cwd must win over the profile's own default cwd: {info:?}"
    );

    harness.registry.terminate_all().await;
}

#[tokio::test]
async fn open_window_from_profile_carries_the_profile_detection_value() {
    let tree = TempDir::new("yagura-cli-test", "ipc-profile-launch-detection");
    let profiles_dir = tree.make_dir("profiles");
    write_profile(
        &profiles_dir,
        "detection-check",
        &echo_child_profile_source_with_detection("Detection Check", "profile-detection-xyz"),
    );

    let config = config_with(Some(profiles_dir), empty_lookup());
    let harness = Harness::start("ipc-profile-launch-detection", config).await;
    let mut client = harness.connect().await;

    let window_id = expect_window_opened(
        call_open_window_from_profile(&mut client, "detection-check", None, None).await,
    );
    let entry = harness
        .registry
        .get(&window_id)
        .expect("the opened window must be in the registry");

    assert_eq!(
        entry.detection(),
        Some("profile-detection-xyz"),
        "the profile's detection value must be carried into the opened window verbatim"
    );

    harness.registry.terminate_all().await;
}

#[tokio::test]
async fn open_window_from_profile_reserved_environment_key_never_reaches_the_child() {
    let tree = TempDir::new("yagura-cli-test", "ipc-profile-launch-reserved-env");
    let profiles_dir = tree.make_dir("profiles");
    write_profile(
        &profiles_dir,
        "reserved-env",
        &reserved_env_profile_source(),
    );

    let config = config_with(Some(profiles_dir), empty_lookup());
    let harness = Harness::start("ipc-profile-launch-reserved-env", config).await;
    let mut client = harness.connect().await;

    let window_id = expect_window_opened(
        call_open_window_from_profile(&mut client, "reserved-env", None, None).await,
    );

    harness
        .wait_for_scrollback(&window_id, b"kept-value", Duration::from_secs(10))
        .await;
    let scrollback = harness.scrollback(&window_id);

    assert!(
        common::contains_subsequence(&scrollback, window_id.as_bytes()),
        "the child must observe the daemon's own YAGURA_WINDOW_ID (the real window id), not \
         the reserved key the profile tried to inject; scrollback: {:?}",
        String::from_utf8_lossy(&scrollback)
    );
    assert!(
        !common::contains_subsequence(&scrollback, b"hijacked"),
        "the reserved YAGURA_WINDOW_ID key in the profile's [env] must never reach the child; \
         scrollback: {:?}",
        String::from_utf8_lossy(&scrollback)
    );
    assert!(
        common::contains_subsequence(&scrollback, b"kept-value"),
        "a non-reserved key in the profile's [env] must still reach the child; scrollback: {:?}",
        String::from_utf8_lossy(&scrollback)
    );

    harness.registry.terminate_all().await;
}

// =============================================================================
// Unknown / invalid identifiers, and an identifier that resolves to no argv
// on the host operating system -- all invalid_argument
// =============================================================================

#[tokio::test]
async fn open_window_from_profile_with_an_unknown_identifier_is_rejected() {
    let tree = TempDir::new("yagura-cli-test", "ipc-profile-unknown-id");
    let profiles_dir = tree.make_dir("profiles");
    write_profile(
        &profiles_dir,
        "the-only-profile",
        "schema = 1\ndisplay_name = \"Only\"\n\n[command]\ndefault = [\"only-program\"]\n",
    );

    let config = config_with(Some(profiles_dir), empty_lookup());
    let harness = Harness::start("ipc-profile-unknown-id", config).await;
    let mut client = harness.connect().await;

    let response =
        call_open_window_from_profile(&mut client, "does-not-exist-anywhere", None, None).await;
    expect_error(response, "invalid_argument");
}

#[tokio::test]
async fn open_window_from_profile_with_a_path_traversal_identifier_is_rejected_without_reading_outside_the_profiles_directory()
 {
    let tree = TempDir::new("yagura-cli-test", "ipc-profile-traversal");
    let profiles_dir = tree.make_dir("profiles");
    let outside_dir = tree.make_dir("outside");
    // A decoy the daemon must never read: if `..` traversal worked, this
    // valid, launchable profile would make the open succeed instead of
    // being rejected.
    write_profile(
        &outside_dir,
        "trap",
        "schema = 1\ndisplay_name = \"Trap\"\n\n[command]\ndefault = [\"trap-program\"]\n",
    );

    let config = config_with(Some(profiles_dir), empty_lookup());
    let harness = Harness::start("ipc-profile-traversal", config).await;
    let mut client = harness.connect().await;

    for traversal_id in ["../outside/trap", "..\\outside\\trap"] {
        let response = call_open_window_from_profile(&mut client, traversal_id, None, None).await;
        expect_error(response, "invalid_argument");
    }
}

#[tokio::test]
async fn open_window_from_profile_whose_operating_system_has_no_argv_is_rejected() {
    let tree = TempDir::new("yagura-cli-test", "ipc-profile-no-host-argv");
    let profiles_dir = tree.make_dir("profiles");
    write_profile(
        &profiles_dir,
        "no-host-argv",
        &profile_source_with_no_argv_for_host(),
    );

    let config = config_with(Some(profiles_dir), empty_lookup());
    let harness = Harness::start("ipc-profile-no-host-argv", config).await;
    let mut client = harness.connect().await;

    let response = call_open_window_from_profile(&mut client, "no-host-argv", None, None).await;
    expect_error(response, "invalid_argument");
}

// =============================================================================
// The existing OpenWindow contract, unchanged in shape but now resolving the
// executable through yagura-os: "not found" and "rejected" must remain
// distinguishable, same as `ipc_v2.rs`'s existing
// `open_window_with_nonexistent_executable_returns_spawn_failed`.
// =============================================================================

#[tokio::test]
async fn open_window_with_a_bare_name_missing_from_the_injected_lookup_returns_spawn_failed() {
    let config = config_with(None, empty_lookup());
    let harness = Harness::start("ipc-openwindow-not-found", config).await;
    let mut client = harness.connect().await;

    send(
        &mut client,
        &ClientMessage::OpenWindow {
            command: vec!["yagura-test-definitely-not-installed-cli-9f3k2".to_string()],
            cwd: None,
            name: None,
            vendor: None,
            detection: None,
        },
    )
    .await;
    let response = read_response(&mut client, Duration::from_secs(5)).await;
    expect_error(response, "spawn_failed");
}

/// A NUL byte survives JSON decoding (it is a well-formed JSON string
/// character) and can never become an OS argument, so `validate_open_request`
/// (`window.rs`) rejects it for both `OpenWindow` and `OpenWindowFromProfile`
/// through `validate_argv_nul_free` (`yagura-protocol`). An *empty* element
/// is a profile-authoring rule enforced by `parse_profile` (`profile.rs`)
/// alone and is accepted on this path.
///
/// The NUL sits in a trailing argument, not `command[0]`, and `name` is
/// given explicitly: `command[0]`'s file stem is what an omitted `name`
/// derives from, and that derived name is separately validated against the
/// window-label rule (which also forbids C0 controls, NUL included) --
/// without this care, the assertion below would pass for that unrelated
/// reason instead of exercising the argv-element check this test is about.
#[tokio::test]
async fn open_window_with_a_nul_byte_in_an_argv_element_is_rejected() {
    let config = config_with(None, empty_lookup());
    let harness = Harness::start("ipc-openwindow-nul-arg", config).await;
    let mut client = harness.connect().await;

    send(
        &mut client,
        &ClientMessage::OpenWindow {
            command: vec!["prog".to_string(), "a\u{0}b".to_string()],
            cwd: None,
            name: Some("explicit-name".to_string()),
            vendor: None,
            detection: None,
        },
    )
    .await;
    let response = read_response(&mut client, Duration::from_secs(5)).await;
    expect_error(response, "invalid_argument");
}

#[cfg(windows)]
#[tokio::test]
async fn open_window_with_a_metacharacter_argument_against_a_shim_returns_invalid_argument() {
    let tree = TempDir::new("yagura-cli-test", "ipc-openwindow-metachar-reject");
    let bin = tree.make_dir("bin");
    tree.write_file("bin/codex.cmd", "");

    let config = config_with(None, lookup_in(&bin));
    let harness = Harness::start("ipc-openwindow-metachar-reject", config).await;
    let mut client = harness.connect().await;

    send(
        &mut client,
        &ClientMessage::OpenWindow {
            command: vec!["codex".to_string(), "%PATH%".to_string()],
            cwd: None,
            name: None,
            vendor: None,
            detection: None,
        },
    )
    .await;
    let response = read_response(&mut client, Duration::from_secs(5)).await;
    expect_error(response, "invalid_argument");
}

// =============================================================================
// Windows only: the real spawn path delivers a whitespace-containing argument
// verbatim through the cmd.exe shim wrapping, in a shim directory with no
// spaces and in one that has spaces. The acceptance judgement belongs on the
// daemon side, not in `yagura-os`, which stops at the resolved argv.
// =============================================================================

#[cfg(windows)]
fn write_arg_dump_cmd(dir: &Path, output_file: &Path) {
    let content = format!("@echo off\r\necho %~1>>\"{}\"\r\n", output_file.display());
    std::fs::write(dir.join("codex.cmd"), content).expect("shim fixture should be writable");
}

#[cfg(windows)]
fn real_windows_comspec() -> Option<PathBuf> {
    yagura_os::program::ProgramLookup::from_env().windows_comspec
}

// =============================================================================
// Windows only: npm's exact directory layout, where an extensionless POSIX
// shell shim sits beside a `.cmd` shim and a `.ps1` script under the same
// name. An `OpenWindow` naming the bare (extensionless) name must open
// through the `.cmd` shim, not spawn the extensionless file, which fails
// with `spawn_failed`.
// =============================================================================

#[cfg(windows)]
#[tokio::test]
async fn open_window_with_an_extensionless_argv0_opens_through_the_cmd_shim_in_an_npm_style_fixture()
 {
    let tree = TempDir::new("yagura-cli-test", "ipc-openwindow-npm-style-fixture");
    let bin = tree.make_dir("bin");
    let output_file = tree.path().join("out.txt");
    write_arg_dump_cmd(&bin, &output_file);
    // The extensionless file npm places alongside the `.cmd` shim: real
    // content (a POSIX shell shebang), so a bug that resolved it `Direct`
    // would actually attempt to spawn it rather than merely fail to find an
    // executable.
    std::fs::write(bin.join("codex"), "#!/bin/sh\nexec codex.js \"$@\"\n")
        .expect("extensionless shim fixture should be writable");
    // Never a probed extension: a bug that widened the extension list would
    // wrongly try this one too.
    std::fs::write(bin.join("codex.ps1"), "# codex launcher\n")
        .expect("ps1 shim fixture should be writable");

    let lookup = ProgramLookup {
        path_dirs: vec![bin.clone()],
        extra_bin_dirs: Vec::new(),
        windows_comspec: real_windows_comspec(),
    };
    let config = config_with(None, lookup);
    let harness = Harness::start("ipc-openwindow-npm-style-fixture", config).await;
    let mut client = harness.connect().await;

    send(
        &mut client,
        &ClientMessage::OpenWindow {
            command: vec!["codex".to_string(), "hello".to_string()],
            cwd: None,
            name: None,
            vendor: None,
            detection: None,
        },
    )
    .await;
    let response = read_response(&mut client, Duration::from_secs(5)).await;
    let window_id = expect_window_opened(response);

    wait_until_file_grows(&output_file, Duration::from_secs(10)).await;
    // One in-flight write may still be landing; the shim writes exactly one line.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let content = std::fs::read_to_string(&output_file).expect("output file must be readable");
    assert_eq!(
        content.trim(),
        "hello",
        "an extensionless argv0 must resolve through the .cmd shim, not the extensionless file \
         next to it (window {window_id})"
    );
    assert!(
        file_len(&output_file) > 0,
        "the shim must have written something"
    );

    harness.registry.terminate_all().await;
}

// A shim named by absolute path goes through the same wrapping and
// metacharacter check as a bare name, rather than reaching `CreateProcess`
// where Windows would run it through cmd.exe unchecked.

#[cfg(windows)]
async fn assert_absolute_shim_delivers(dir_name: &str, argument: &str, tag: &str) {
    let tree = TempDir::new("yagura-cli-test", tag);
    let bin = tree.make_dir(dir_name);
    let output_file = tree.path().join("out.txt");
    write_arg_dump_cmd(&bin, &output_file);
    let shim = bin.join("codex.cmd");

    let lookup = ProgramLookup {
        path_dirs: Vec::new(),
        extra_bin_dirs: Vec::new(),
        windows_comspec: real_windows_comspec(),
    };
    let harness = Harness::start(tag, config_with(None, lookup)).await;
    let mut client = harness.connect().await;

    send(
        &mut client,
        &ClientMessage::OpenWindow {
            command: vec![shim.to_string_lossy().into_owned(), argument.to_string()],
            cwd: None,
            name: Some("abs-shim".to_string()),
            vendor: None,
            detection: None,
        },
    )
    .await;
    let response = read_response(&mut client, Duration::from_secs(5)).await;
    let window_id = expect_window_opened(response);

    wait_until_file_grows(&output_file, Duration::from_secs(10)).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let content = std::fs::read_to_string(&output_file).expect("output file must be readable");
    assert_eq!(
        content.trim(),
        argument,
        "the argument must reach the child exactly as written (window {window_id})"
    );

    harness.registry.terminate_all().await;
}

#[cfg(windows)]
#[tokio::test]
async fn open_window_with_an_absolute_cmd_shim_path_delivers_the_argument_verbatim() {
    assert_absolute_shim_delivers("bin", "hello", "ipc-openwindow-abs-shim-ok").await;
}

#[cfg(windows)]
#[tokio::test]
async fn open_window_with_an_absolute_cmd_shim_path_in_a_spaced_directory_delivers_a_spaced_argument_verbatim()
 {
    assert_absolute_shim_delivers(
        "bin with spaces",
        "hello world",
        "ipc-openwindow-abs-shim-spaced",
    )
    .await;
}

#[cfg(windows)]
#[tokio::test]
async fn open_window_with_an_absolute_cmd_shim_path_and_a_metacharacter_argument_is_rejected_before_spawn()
 {
    let tree = TempDir::new("yagura-cli-test", "ipc-openwindow-abs-shim-reject");
    let bin = tree.make_dir("bin");
    let output_file = tree.path().join("out.txt");
    write_arg_dump_cmd(&bin, &output_file);
    let shim = bin.join("codex.cmd");

    let lookup = ProgramLookup {
        path_dirs: Vec::new(),
        extra_bin_dirs: Vec::new(),
        windows_comspec: real_windows_comspec(),
    };
    let harness = Harness::start("ipc-openwindow-abs-shim-reject", config_with(None, lookup)).await;
    let mut client = harness.connect().await;

    send(
        &mut client,
        &ClientMessage::OpenWindow {
            command: vec![shim.to_string_lossy().into_owned(), "%PATH%".to_string()],
            cwd: None,
            name: Some("abs-shim".to_string()),
            vendor: None,
            detection: None,
        },
    )
    .await;
    let response = read_response(&mut client, Duration::from_secs(5)).await;
    harness.registry.terminate_all().await;
    expect_error(response, "invalid_argument");

    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        !output_file.exists(),
        "a rejected request must never start the shim"
    );
}

#[cfg(windows)]
async fn assert_argument_delivered_verbatim(bin_dir: &Path, tag: &str) {
    let tree = TempDir::new("yagura-cli-test", tag);
    let output_file = tree.path().join("out.txt");
    write_arg_dump_cmd(bin_dir, &output_file);

    let profiles_dir = tree.make_dir("profiles");
    write_profile(
        &profiles_dir,
        "verbatim",
        "schema = 1\ndisplay_name = \"Verbatim\"\n\n[command]\nwindows = [\"codex\", \"hello \
         world\"]\n",
    );

    let lookup = ProgramLookup {
        path_dirs: vec![bin_dir.to_path_buf()],
        extra_bin_dirs: Vec::new(),
        windows_comspec: real_windows_comspec(),
    };
    let config = config_with(Some(profiles_dir), lookup);
    let harness = Harness::start(tag, config).await;
    let mut client = harness.connect().await;

    let window_id = expect_window_opened(
        call_open_window_from_profile(&mut client, "verbatim", None, None).await,
    );

    wait_until_file_grows(&output_file, Duration::from_secs(10)).await;
    // One in-flight write may still be landing; the shim writes exactly one line.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let content = std::fs::read_to_string(&output_file).expect("output file must be readable");
    assert_eq!(
        content.trim(),
        "hello world",
        "the argument must arrive at the child exactly as written, including its internal \
         space (window {window_id})"
    );
    assert!(
        file_len(&output_file) > 0,
        "the shim must have written something"
    );

    harness.registry.terminate_all().await;
}

#[cfg(windows)]
#[tokio::test]
async fn profile_launch_delivers_a_whitespace_argument_verbatim_through_a_shim_in_a_plain_directory()
 {
    let tree = TempDir::new("yagura-cli-test", "ipc-profile-verbatim-plain-bin");
    let bin = tree.make_dir("bin");
    assert_argument_delivered_verbatim(&bin, "ipc-profile-verbatim-plain").await;
}

#[cfg(windows)]
#[tokio::test]
async fn profile_launch_delivers_a_whitespace_argument_verbatim_through_a_shim_in_a_directory_with_spaces()
 {
    let tree = TempDir::new("yagura-cli-test", "ipc-profile-verbatim-spaced-bin");
    let bin = tree.make_dir("bin with spaces");
    assert_argument_delivered_verbatim(&bin, "ipc-profile-verbatim-spaced").await;
}

// =============================================================================
// send_handoff: profile-addressed delivery.
//
// Approval prerequisite: a profile-addressed request is pre-judged by
// policy before any window opens, so every case below either injects
// `auto_approve = ["send_handoff"]` (via `policy`) or registers an approver.
// =============================================================================

#[tokio::test]
async fn send_handoff_from_a_profile_opens_a_new_window_and_delivers_the_envelope_once_ready() {
    let tree = TempDir::new("yagura-cli-test", "handoff-profile-basic");
    let profiles_dir = tree.make_dir("profiles");
    let detection_dir = tree.make_dir("detection");
    write_profile(
        &profiles_dir,
        "ready",
        &ready_child_profile_source_with_composer("Ready", 0),
    );

    // auto_approve injected: no approver needed.
    let config = handoff_profile_config(
        Some(profiles_dir),
        empty_lookup(),
        &detection_dir,
        None,
        Some(policy(&["send_handoff"])),
        30,
    );
    let h = Harness::start("handoff-profile-basic", config).await;
    let mut client = h.connect().await;

    let msg = call_send_handoff_from_profile_within(
        &mut client,
        "ready",
        None,
        None,
        "src/foo.rs",
        "look here",
        Duration::from_secs(10),
    )
    .await;
    let (window_id, opened, delivery) = expect_handoff_sent(msg);
    assert_eq!(delivery, HandoffDelivery::NotObserved);
    assert!(opened, "a profile target must always report opened: true");

    let json =
        wait_for_accepted_handoff_envelope_json(&h, &window_id, Duration::from_secs(10)).await;
    assert_eq!(
        json["from"], "external",
        "an unidentified connection's envelope must carry \"from\":\"external\": {json}"
    );
    assert!(json.get("reply").is_none());
    assert_eq!(json["reference"], "src/foo.rs");
    assert_eq!(json["note"], "look here");

    h.registry.terminate_all().await;
}

#[tokio::test]
async fn send_handoff_from_a_profile_uses_the_explicit_cwd_argument_over_everything_else() {
    let tree = TempDir::new("yagura-cli-test", "handoff-cwd-explicit");
    let profiles_dir = tree.make_dir("profiles");
    let detection_dir = tree.make_dir("detection");
    let requester_cwd = tree.make_dir("requester-cwd");
    let profile_default_cwd = tree.make_dir("profile-default-cwd");
    let explicit_cwd = tree.make_dir("explicit-cwd");
    write_profile(
        &profiles_dir,
        "ready",
        &ready_child_profile_source_with_cwd("Ready", &profile_default_cwd),
    );

    let config = handoff_profile_config(
        Some(profiles_dir),
        empty_lookup(),
        &detection_dir,
        None,
        Some(policy(&["send_handoff"])),
        30,
    );
    let h = Harness::start("handoff-cwd-explicit", config).await;

    // The requester's own window, opened directly through the registry
    // (`Harness::open` fixes `cwd: None`) with an explicit cwd, so the
    // priority order (argument > requester's window cwd > profile default)
    // can be told apart.
    let requester_window_id = h
        .registry
        .open(OpenWindowRequest {
            command: echo_child_command(),
            cwd: Some(RequestedCwd::from_caller(requester_cwd)),
            name: None,
            vendor: None,
            detection: None,
        })
        .await
        .expect("requester window should spawn")
        .window_id;

    let mut requester = h.connect().await;
    identify(&mut requester, &requester_window_id).await;

    let explicit_cwd_str = explicit_cwd.to_string_lossy().into_owned();
    let msg = call_send_handoff_from_profile_within(
        &mut requester,
        "ready",
        Some(&explicit_cwd_str),
        None,
        "src/foo.rs",
        "look here",
        Duration::from_secs(10),
    )
    .await;
    let (window_id, opened, delivery) = expect_handoff_sent(msg);
    assert_eq!(delivery, HandoffDelivery::NotObserved);
    assert!(opened);

    let windows = call_list_windows(&mut requester).await;
    let info = find_window(&windows, &window_id);
    let expected_cwd = strip_extended_length_prefix(
        &explicit_cwd
            .canonicalize()
            .expect("explicit cwd must canonicalize"),
    )
    .to_string_lossy()
    .into_owned();
    assert_eq!(
        info.cwd, expected_cwd,
        "the cwd argument must win over both the requester's own cwd and the profile default"
    );

    h.registry.terminate_all().await;
}

#[tokio::test]
async fn send_handoff_from_a_profile_with_no_cwd_argument_uses_the_requesters_own_window_cwd() {
    let tree = TempDir::new("yagura-cli-test", "handoff-cwd-requester-default");
    let profiles_dir = tree.make_dir("profiles");
    let detection_dir = tree.make_dir("detection");
    let requester_cwd = tree.make_dir("requester-cwd");
    let profile_default_cwd = tree.make_dir("profile-default-cwd");
    write_profile(
        &profiles_dir,
        "ready",
        &ready_child_profile_source_with_cwd("Ready", &profile_default_cwd),
    );

    let config = handoff_profile_config(
        Some(profiles_dir),
        empty_lookup(),
        &detection_dir,
        None,
        Some(policy(&["send_handoff"])),
        30,
    );
    let h = Harness::start("handoff-cwd-requester-default", config).await;

    let requester_window_id = h
        .registry
        .open(OpenWindowRequest {
            command: echo_child_command(),
            cwd: Some(RequestedCwd::from_caller(requester_cwd.clone())),
            name: None,
            vendor: None,
            detection: None,
        })
        .await
        .expect("requester window should spawn")
        .window_id;

    let mut requester = h.connect().await;
    identify(&mut requester, &requester_window_id).await;

    let msg = call_send_handoff_from_profile_within(
        &mut requester,
        "ready",
        None,
        None,
        "src/foo.rs",
        "look",
        Duration::from_secs(10),
    )
    .await;
    let (window_id, opened, delivery) = expect_handoff_sent(msg);
    assert_eq!(delivery, HandoffDelivery::NotObserved);
    assert!(opened);

    let windows = call_list_windows(&mut requester).await;
    let info = find_window(&windows, &window_id);
    let expected_requester_cwd = strip_extended_length_prefix(
        &requester_cwd
            .canonicalize()
            .expect("requester cwd must canonicalize"),
    )
    .to_string_lossy()
    .into_owned();
    let profile_default_cwd_canonical = strip_extended_length_prefix(
        &profile_default_cwd
            .canonicalize()
            .expect("profile default cwd must canonicalize"),
    )
    .to_string_lossy()
    .into_owned();
    assert_eq!(
        info.cwd, expected_requester_cwd,
        "with no cwd argument, the new window's cwd must be the requester's own window cwd"
    );
    assert_ne!(
        info.cwd, profile_default_cwd_canonical,
        "the requester's own cwd must win over the profile's default cwd"
    );

    h.registry.terminate_all().await;
}

#[tokio::test]
async fn send_handoff_from_a_profile_with_no_cwd_argument_and_an_external_requester_uses_the_profiles_own_default_cwd()
 {
    let tree = TempDir::new("yagura-cli-test", "handoff-cwd-profile-default");
    let profiles_dir = tree.make_dir("profiles");
    let detection_dir = tree.make_dir("detection");
    let profile_default_cwd = tree.make_dir("profile-default-cwd");
    write_profile(
        &profiles_dir,
        "ready",
        &ready_child_profile_source_with_cwd("Ready", &profile_default_cwd),
    );

    let config = handoff_profile_config(
        Some(profiles_dir),
        empty_lookup(),
        &detection_dir,
        None,
        Some(policy(&["send_handoff"])),
        30,
    );
    let h = Harness::start("handoff-cwd-profile-default", config).await;
    // Never identified: an External requester has no window cwd to fall
    // back to, so the profile's own default must be used.
    let mut client = h.connect().await;

    let msg = call_send_handoff_from_profile_within(
        &mut client,
        "ready",
        None,
        None,
        "src/foo.rs",
        "look",
        Duration::from_secs(10),
    )
    .await;
    let (window_id, opened, delivery) = expect_handoff_sent(msg);
    assert_eq!(delivery, HandoffDelivery::NotObserved);
    assert!(opened);

    let windows = call_list_windows(&mut client).await;
    let info = find_window(&windows, &window_id);
    let expected_cwd = strip_extended_length_prefix(
        &profile_default_cwd
            .canonicalize()
            .expect("profile default cwd must canonicalize"),
    )
    .to_string_lossy()
    .into_owned();
    assert_eq!(info.cwd, expected_cwd);

    h.registry.terminate_all().await;
}

#[tokio::test]
async fn send_handoff_from_a_profile_with_a_nonexistent_cwd_is_invalid_argument_without_any_approver()
 {
    let tree = TempDir::new("yagura-cli-test", "handoff-cwd-nonexistent");
    let profiles_dir = tree.make_dir("profiles");
    let detection_dir = tree.make_dir("detection");
    write_profile(
        &profiles_dir,
        "ready",
        &ready_child_profile_source_with_composer("Ready", 0),
    );
    let nonexistent = tree.make_dir("cwd-parent").join("does-not-exist-anywhere");

    // No playbook, no approval_policy: PolicySource::NotConfigured. The cwd
    // check must reject before the policy pre-judgement
    // ever runs, so no approver is needed for this to fail closed.
    let config = handoff_profile_config(
        Some(profiles_dir),
        empty_lookup(),
        &detection_dir,
        None,
        None,
        30,
    );
    let h = Harness::start("handoff-cwd-nonexistent", config).await;
    let mut client = h.connect().await;

    let before = call_list_windows(&mut client).await.len();
    let nonexistent_str = nonexistent.to_string_lossy().into_owned();
    let msg = call_send_handoff_from_profile(
        &mut client,
        "ready",
        Some(&nonexistent_str),
        None,
        "src/foo.rs",
        "look",
    )
    .await;
    expect_error(msg, "invalid_argument");
    let after = call_list_windows(&mut client).await.len();
    assert_eq!(before, after, "a rejected handoff must not open a window");
}

#[tokio::test]
async fn send_handoff_from_a_profile_with_a_control_character_in_name_is_invalid_argument_without_any_approver()
 {
    let tree = TempDir::new("yagura-cli-test", "handoff-name-control-char");
    let profiles_dir = tree.make_dir("profiles");
    let detection_dir = tree.make_dir("detection");
    write_profile(
        &profiles_dir,
        "ready",
        &ready_child_profile_source_with_composer("Ready", 0),
    );

    let config = handoff_profile_config(
        Some(profiles_dir),
        empty_lookup(),
        &detection_dir,
        None,
        None,
        30,
    );
    let h = Harness::start("handoff-name-control-char", config).await;
    let mut client = h.connect().await;

    let before = call_list_windows(&mut client).await.len();
    let msg = call_send_handoff_from_profile(
        &mut client,
        "ready",
        None,
        Some("bad\u{0}name"),
        "src/foo.rs",
        "look",
    )
    .await;
    expect_error(msg, "invalid_argument");
    let after = call_list_windows(&mut client).await.len();
    assert_eq!(before, after, "a rejected handoff must not open a window");
}

#[tokio::test]
async fn send_handoff_from_a_profile_that_is_denied_closes_the_window_it_opened() {
    let tree = TempDir::new("yagura-cli-test", "handoff-profile-denied");
    let profiles_dir = tree.make_dir("profiles");
    let detection_dir = tree.make_dir("detection");
    write_profile(
        &profiles_dir,
        "ready",
        &ready_child_profile_source_with_composer("Ready", 0),
    );

    // No auto_approve: a human decision is required.
    let config = handoff_profile_config(
        Some(profiles_dir),
        empty_lookup(),
        &detection_dir,
        None,
        None,
        30,
    );
    let h = Harness::start("handoff-profile-denied", config).await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;

    let mut requester = h.connect().await;
    send_handoff_from_profile(&mut requester, "ready", None, None, "src/foo.rs", "look").await;

    // Same rationale as the slot-pending test below: this request just
    // launched a fresh profile window, so the wait covers a process spawn
    // plus detection, not just an IPC round trip.
    let request = expect_approval_requested_within(&mut approver, Duration::from_secs(10)).await;
    assert_eq!(request.tool, "send_handoff");
    let window_id = request
        .target
        .as_ref()
        .expect("send_handoff must carry a target")
        .window_id
        .clone();

    decide(&mut approver, &request.request_id, false).await;
    let msg = read_response(&mut requester, Duration::from_secs(5)).await;
    expect_error(msg, "approval_denied");

    h.wait_until_child_exited(&window_id, Duration::from_secs(10))
        .await;
}

/// A `ready_hold_ms` of 2000 can never be satisfied within a
/// `handoff_ready_timeout_secs` of 1 (`hold_is_reachable_within` is false
/// for this pair), so the readiness wait exhausts the timeout and
/// `window_not_ready`'s message must name both the hold the definition
/// asked for and the fact that this window's timeout can never satisfy it.
/// The definition itself still loads successfully -- the window reports
/// `waiting_input`, never `Unknown` -- which is what distinguishes this
/// case from an out-of-range `ready_hold_ms`.
#[tokio::test]
async fn send_handoff_from_a_profile_whose_hold_can_never_fit_the_readiness_timeout_is_window_not_ready()
 {
    let tree = TempDir::new("yagura-cli-test", "handoff-a6-hold-unreachable");
    let profiles_dir = tree.make_dir("profiles");
    let detection_dir = tree.make_dir("detection");
    write_submit_definition(
        &detection_dir,
        "handoff-a6",
        "handoff-a6-marker",
        SubmitDefinitionInput {
            bracketed_paste: true,
            ready_hold_ms: 2000,
            submit_delay_ms: 900,
            clear: "",
            delivery_confirm_ms: 5000,
            resend_max: 0,
        },
    );
    write_profile(
        &profiles_dir,
        "submit-a6",
        &submit_child_profile_source_with_detection_and_composer(
            "Submit A6",
            "handoff-a6-marker",
            0,
            0,
            0,
            "handoff-a6",
            0,
        ),
    );

    let mut config = handoff_profile_config(
        Some(profiles_dir),
        empty_lookup(),
        &detection_dir,
        None,
        Some(policy(&["send_handoff"])),
        30,
    );
    config.handoff_ready_timeout_secs = 1;
    let h = Harness::start("handoff-a6-hold-unreachable", config).await;
    let mut client = h.connect().await;

    let msg = call_send_handoff_from_profile_within(
        &mut client,
        "submit-a6",
        None,
        None,
        "src/foo.rs",
        "look here",
        Duration::from_secs(10),
    )
    .await;
    let message = expect_error(msg, "window_not_ready");
    assert!(
        message.contains("2000ms"),
        "message must name the hold the definition asked for, in the milliseconds the \
         definition wrote it in: {message:?}"
    );
    assert!(
        message.contains("can never satisfy"),
        "message must say this window's readiness timeout can never satisfy that hold: {message:?}"
    );

    let opened =
        wait_for_window_with_vendor(&mut client, "submit-a6", Duration::from_secs(5)).await;
    h.wait_until_child_exited(&opened.window_id, Duration::from_secs(10))
        .await;
}

#[tokio::test]
async fn send_handoff_from_a_profile_whose_detection_never_resolves_is_window_not_ready() {
    let tree = TempDir::new("yagura-cli-test", "handoff-profile-not-ready");
    let profiles_dir = tree.make_dir("profiles");
    let detection_dir = tree.make_dir("detection");
    // A detection name nothing in `detection_dir` defines: the window stays
    // Unknown forever, the same fail-safe fixture `open_unknown_window` uses.
    write_profile(
        &profiles_dir,
        "unresolvable",
        &echo_child_profile_source_with_detection("Unresolvable", "does-not-exist-anywhere"),
    );

    let mut config = handoff_profile_config(
        Some(profiles_dir),
        empty_lookup(),
        &detection_dir,
        None,
        Some(policy(&["send_handoff"])),
        30,
    );
    config.handoff_ready_timeout_secs = 1;
    let h = Harness::start("handoff-profile-not-ready", config).await;
    let mut client = h.connect().await;

    let msg = call_send_handoff_from_profile(
        &mut client,
        "unresolvable",
        None,
        None,
        "src/foo.rs",
        "look",
    )
    .await;
    let message = expect_error(msg, "window_not_ready");
    assert!(
        message.contains("unknown"),
        "message must carry the last observed status (wire spelling): {message:?}"
    );

    let opened =
        wait_for_window_with_vendor(&mut client, "unresolvable", Duration::from_secs(5)).await;
    h.wait_until_child_exited(&opened.window_id, Duration::from_secs(10))
        .await;
}

#[tokio::test]
async fn send_handoff_from_a_profile_closes_the_window_if_the_requester_disconnects_while_waiting_for_readiness()
 {
    let tree = TempDir::new("yagura-cli-test", "handoff-profile-requester-gone");
    let profiles_dir = tree.make_dir("profiles");
    let detection_dir = tree.make_dir("detection");
    // echo_child prints nothing on startup, so this window never reaches
    // WaitingInput / Idle on its own.
    write_profile(
        &profiles_dir,
        "never-ready",
        &echo_child_profile_source("Never Ready"),
    );

    let config = handoff_profile_config(
        Some(profiles_dir),
        empty_lookup(),
        &detection_dir,
        None,
        Some(policy(&["send_handoff"])),
        30,
    );
    let h = Harness::start("handoff-profile-requester-gone", config).await;
    let mut requester = h.connect().await;

    send_handoff_from_profile(
        &mut requester,
        "never-ready",
        None,
        None,
        "src/foo.rs",
        "look",
    )
    .await;
    let opened =
        wait_for_window_with_vendor(&mut requester, "never-ready", Duration::from_secs(5)).await;
    drop(requester);

    h.wait_until_child_exited(&opened.window_id, Duration::from_secs(10))
        .await;
    // A requester disconnect while still waiting for readiness must abort
    // before the first byte of the submit plan is ever written. Scrollback
    // is not empty by itself: ConPTY/conhost writes its own session-startup
    // negotiation sequence before either side of the daemon ever touches the
    // child's input, so an `is_empty()` check is unsatisfiable regardless of
    // whether the daemon wrote anything (confirmed by direct measurement).
    // Checking for the absence of the daemon's own vocabulary -- the
    // envelope prefix, and `submit_child`'s explicit `PASTE ` / `SUBMITTED `
    // output -- stays a decisive, non-vacuous proof that no submit plan step
    // reached the window, without depending on that startup sequence.
    let text = String::from_utf8_lossy(&h.scrollback(&opened.window_id)).into_owned();
    for needle in ["[yagura handoff] ", "PASTE ", "SUBMITTED "] {
        assert!(
            !text.contains(needle),
            "a requester disconnect before readiness must never write anything into the \
             window, found {needle:?} in scrollback: {text:?}"
        );
    }
}

#[tokio::test]
async fn send_handoff_with_an_unknown_profile_identifier_is_invalid_argument_without_any_approver()
{
    let tree = TempDir::new("yagura-cli-test", "handoff-unknown-profile");
    let profiles_dir = tree.make_dir("profiles");
    let detection_dir = tree.make_dir("detection");
    write_profile(
        &profiles_dir,
        "the-only-profile",
        &ready_child_profile_source_with_composer("Only", 0),
    );

    let config = handoff_profile_config(
        Some(profiles_dir),
        empty_lookup(),
        &detection_dir,
        None,
        None,
        30,
    );
    let h = Harness::start("handoff-unknown-profile", config).await;
    let mut client = h.connect().await;

    let before = call_list_windows(&mut client).await.len();
    let msg = call_send_handoff_from_profile(
        &mut client,
        "does-not-exist-anywhere",
        None,
        None,
        "src/foo.rs",
        "look",
    )
    .await;
    expect_error(msg, "invalid_argument");
    let after = call_list_windows(&mut client).await.len();
    assert_eq!(before, after);
}

#[tokio::test]
async fn send_handoff_from_a_profile_with_an_oversized_envelope_is_invalid_argument_without_opening_a_window()
 {
    let tree = TempDir::new("yagura-cli-test", "handoff-envelope-oversize");
    let profiles_dir = tree.make_dir("profiles");
    let detection_dir = tree.make_dir("detection");
    write_profile(
        &profiles_dir,
        "ready",
        &ready_child_profile_source_with_composer("Ready", 0),
    );

    let config = handoff_profile_config(
        Some(profiles_dir),
        empty_lookup(),
        &detection_dir,
        None,
        Some(policy(&["send_handoff"])),
        30,
    );
    let h = Harness::start("handoff-envelope-oversize", config).await;

    let reference = "a".repeat(HANDOFF_REFERENCE_MAX_BYTES);
    let note = "b".repeat(HANDOFF_NOTE_MAX_BYTES);
    let max_name = "m".repeat(yagura_protocol::WINDOW_NAME_MAX_BYTES);

    // How many JSON-encoded bytes the cwd alone must contribute to push the
    // envelope past HANDOFF_ENVELOPE_MAX_BYTES on its own, computed with the
    // same encoder the daemon uses rather than assumed. "w0-000000" (9
    // bytes) matches this requester's real id shape: the first window a
    // fresh registry allocates is always "w1-" followed by a fixed 6-byte
    // nonce. The fixture precondition below (checked against the real id
    // and cwd after opening) fails loud if that shape ever changes, instead
    // of this estimate silently drifting.
    let min_cwd_content_len = {
        let baseline = yagura_daemon::handoff::compose_envelope(
            &yagura_daemon::handoff::EnvelopeFrom::Window {
                id: "w0-000000".to_string(),
                name: max_name.clone(),
                cwd: String::new(),
            },
            &reference,
            &note,
        );
        yagura_protocol::HANDOFF_ENVELOPE_MAX_BYTES.saturating_sub(baseline.text.len()) + 4
    };

    // Grows the requester's cwd one small segment at a time, re-checking its
    // real JSON-encoded length after each step, instead of assuming a fixed
    // path length: a nested temp directory costs a different number of bytes
    // depending on how long the local temp root is and whether its
    // separators need JSON-escaping (backslashes on Windows). The loop stops
    // as soon as it has proven enough, so it never asks for more path than
    // the local temp root happens to require (the envelope-wide
    // cap must be provably exceeded independently of the per-field caps).
    let mut relative_cwd = String::from("requester-cwd");
    let mut nested_cwd = tree.make_dir(&relative_cwd);
    let mut grown_segments = 0u32;
    loop {
        let probe = yagura_daemon::handoff::compose_envelope(
            &yagura_daemon::handoff::EnvelopeFrom::Window {
                id: String::new(),
                name: max_name.clone(),
                cwd: nested_cwd.to_string_lossy().into_owned(),
            },
            &reference,
            &note,
        );
        if probe.parts.cwd >= min_cwd_content_len {
            break;
        }
        grown_segments += 1;
        assert!(
            grown_segments < 40,
            "fixture precondition: could not grow the requester cwd past {min_cwd_content_len} \
             encoded bytes within a sane number of path segments"
        );
        relative_cwd.push_str("/abcd");
        nested_cwd = tree.make_dir(&relative_cwd);
    }
    let requester_window_id = h
        .registry
        .open(OpenWindowRequest {
            command: echo_child_command(),
            cwd: Some(RequestedCwd::from_caller(nested_cwd)),
            name: None,
            vendor: None,
            detection: None,
        })
        .await
        .expect("requester window should spawn")
        .window_id;
    let mut requester = h.connect().await;
    identify(&mut requester, &requester_window_id).await;
    send(
        &mut requester,
        &ClientMessage::RenameWindow {
            window_id: requester_window_id.clone(),
            name: max_name.clone(),
        },
    )
    .await;
    match read_response(&mut requester, Duration::from_secs(5)).await {
        DaemonMessage::WindowRenamed { .. } => {}
        other => panic!("expected window_renamed, got {other:?}"),
    }

    // Fixture precondition, checked with the same pure function the daemon
    // calls (`yagura_daemon::handoff`): this exact combination really is
    // over the cap, so the byte budget above can never silently stop
    // proving the intended condition.
    let windows = call_list_windows(&mut requester).await;
    let requester_info = find_window(&windows, &requester_window_id);
    let probe_envelope = yagura_daemon::handoff::compose_envelope(
        &yagura_daemon::handoff::EnvelopeFrom::Window {
            id: requester_window_id.clone(),
            name: requester_info.name.clone(),
            cwd: requester_info.cwd.clone(),
        },
        &reference,
        &note,
    );
    assert!(
        yagura_daemon::handoff::check_envelope_len(&probe_envelope).is_err(),
        "fixture precondition: this requester's real name/cwd plus reference/note at their own \
         per-field caps must already exceed HANDOFF_ENVELOPE_MAX_BYTES, or this test proves \
         nothing: envelope was {} bytes",
        probe_envelope.text.len()
    );

    let before = windows.len();
    let msg =
        call_send_handoff_from_profile(&mut requester, "ready", None, None, &reference, &note)
            .await;
    expect_error(msg, "invalid_argument");
    let after = call_list_windows(&mut requester).await.len();
    assert_eq!(
        before, after,
        "an envelope rejected for its overall size must not open a window"
    );
}

#[tokio::test]
async fn send_handoff_to_a_profile_with_no_approver_connected_is_approval_required_before_any_window_opens()
 {
    let tree = TempDir::new("yagura-cli-test", "handoff-profile-no-approver");
    let profiles_dir = tree.make_dir("profiles");
    let detection_dir = tree.make_dir("detection");
    write_profile(
        &profiles_dir,
        "ready",
        &ready_child_profile_source_with_composer("Ready", 0),
    );

    let config = handoff_profile_config(
        Some(profiles_dir),
        empty_lookup(),
        &detection_dir,
        None,
        None,
        30,
    );
    let h = Harness::start("handoff-profile-no-approver", config).await;
    let mut client = h.connect().await;

    let before = call_list_windows(&mut client).await.len();
    let msg =
        call_send_handoff_from_profile(&mut client, "ready", None, None, "src/foo.rs", "look")
            .await;
    let message = expect_error(msg, "approval_required");
    assert!(
        message.contains("no approver is connected"),
        "message must state that no approver is connected, got {message:?}"
    );
    let after = call_list_windows(&mut client).await.len();
    assert_eq!(
        before, after,
        "the pre-launch policy judgement must reject before opening a window"
    );
}

#[tokio::test]
async fn send_handoff_to_a_profile_with_a_playbook_that_failed_to_load_is_approval_required_before_any_window_opens()
 {
    let tree = TempDir::new("yagura-cli-test", "handoff-profile-playbook-broken");
    let profiles_dir = tree.make_dir("profiles");
    let detection_dir = tree.make_dir("detection");
    write_profile(
        &profiles_dir,
        "ready",
        &ready_child_profile_source_with_composer("Ready", 0),
    );

    // `playbook: Some(_)` with no `approval_policy` is exactly how a
    // playbook that was configured but failed to load at startup is
    // represented -- the daemon never re-reads this path, so the file need
    // not actually contain a valid playbook.
    let config = handoff_profile_config(
        Some(profiles_dir),
        empty_lookup(),
        &detection_dir,
        Some(tree.write_file("unused-playbook.toml", "")),
        None,
        30,
    );
    let h = Harness::start("handoff-profile-playbook-broken", config).await;
    let mut client = h.connect().await;

    let before = call_list_windows(&mut client).await.len();
    let msg =
        call_send_handoff_from_profile(&mut client, "ready", None, None, "src/foo.rs", "look")
            .await;
    let message = expect_error(msg, "approval_required");
    assert!(
        message.contains("the configured playbook failed to load"),
        "message must state that the playbook failed to load, got {message:?}"
    );
    let after = call_list_windows(&mut client).await.len();
    assert_eq!(before, after);
}

// -----------------------------------------------------------------------
// Handoff-slot accounting: handoffs this
// build has accepted and not yet answered, bounded per-connection and
// daemon-wide.
// -----------------------------------------------------------------------

#[tokio::test]
async fn a_second_profile_handoff_on_the_same_connection_while_the_first_awaits_readiness_is_resource_exhausted_per_connection()
 {
    let tree = TempDir::new("yagura-cli-test", "handoff-slot-per-conn");
    let profiles_dir = tree.make_dir("profiles");
    let detection_dir = tree.make_dir("detection");
    write_profile(
        &profiles_dir,
        "never-ready",
        &echo_child_profile_source("Never Ready"),
    );

    let mut config = handoff_profile_config(
        Some(profiles_dir),
        empty_lookup(),
        &detection_dir,
        None,
        Some(policy(&["send_handoff"])),
        30,
    );
    config.max_open_handoffs_per_conn = 1;
    let h = Harness::start("handoff-slot-per-conn", config).await;
    let mut client = h.connect().await;

    send_handoff_from_profile(&mut client, "never-ready", None, None, "r", "t").await;
    let first =
        wait_for_window_with_vendor(&mut client, "never-ready", Duration::from_secs(5)).await;

    send_handoff_from_profile(&mut client, "never-ready", None, None, "r", "t").await;
    let msg = read_response(&mut client, Duration::from_secs(5)).await;
    let message = expect_error(msg, "resource_exhausted");
    assert!(
        message.contains("per-connection"),
        "message must name the per-connection limit: {message:?}"
    );

    // Only the first request's window was opened.
    let windows = call_list_windows(&mut client).await;
    assert_eq!(
        windows
            .iter()
            .filter(|w| w.vendor.as_deref() == Some("never-ready"))
            .count(),
        1,
        "the refused second request must not have opened a second window"
    );

    // Clean up: disconnecting the requester lets the daemon close the
    // window the first request is still holding open.
    drop(client);
    h.wait_until_child_exited(&first.window_id, Duration::from_secs(10))
        .await;
}

#[tokio::test]
async fn a_second_profile_handoff_on_a_different_connection_while_the_first_awaits_readiness_is_resource_exhausted_daemon_wide()
 {
    let tree = TempDir::new("yagura-cli-test", "handoff-slot-daemon-wide");
    let profiles_dir = tree.make_dir("profiles");
    let detection_dir = tree.make_dir("detection");
    write_profile(
        &profiles_dir,
        "never-ready",
        &echo_child_profile_source("Never Ready"),
    );

    let mut config = handoff_profile_config(
        Some(profiles_dir),
        empty_lookup(),
        &detection_dir,
        None,
        Some(policy(&["send_handoff"])),
        30,
    );
    config.max_open_handoffs = 1;
    config.max_open_handoffs_per_conn = 10;
    let h = Harness::start("handoff-slot-daemon-wide", config).await;

    let mut first_conn = h.connect().await;
    send_handoff_from_profile(&mut first_conn, "never-ready", None, None, "r", "t").await;
    let first =
        wait_for_window_with_vendor(&mut first_conn, "never-ready", Duration::from_secs(5)).await;

    let mut second_conn = h.connect().await;
    send_handoff_from_profile(&mut second_conn, "never-ready", None, None, "r", "t").await;
    let msg = read_response(&mut second_conn, Duration::from_secs(5)).await;
    let message = expect_error(msg, "resource_exhausted");
    assert!(
        message.contains("daemon-wide"),
        "message must name the daemon-wide limit: {message:?}"
    );

    drop(first_conn);
    h.wait_until_child_exited(&first.window_id, Duration::from_secs(10))
        .await;
}

#[tokio::test]
async fn a_pending_profile_handoff_holds_its_slot_until_decided_and_releases_it_before_the_response()
 {
    let tree = TempDir::new("yagura-cli-test", "handoff-slot-pending");
    let profiles_dir = tree.make_dir("profiles");
    let detection_dir = tree.make_dir("detection");
    write_profile(
        &profiles_dir,
        "ready",
        &ready_child_profile_source_with_composer("Ready", 0),
    );

    // No auto_approve: the first request reaches readiness and then waits
    // for a human decision, holding its slot the whole time.
    let mut config = handoff_profile_config(
        Some(profiles_dir),
        empty_lookup(),
        &detection_dir,
        None,
        None,
        30,
    );
    config.max_open_handoffs_per_conn = 1;
    let h = Harness::start("handoff-slot-pending", config).await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;

    let mut requester = h.connect().await;
    send_handoff_from_profile(&mut requester, "ready", None, None, "r", "t").await;
    // A profile-addressed launch spawns a fresh OS process and waits for the
    // status-detection engine's poll/debounce cycle before the gate ever
    // runs, the same category of work this suite already budgets 10s for
    // elsewhere (`Harness::wait_until_child_exited`, and the profile-launch
    // `wait_for_scrollback` above); a plain 5s read races with that spawn
    // whenever other test binaries in the same `cargo test` invocation are
    // competing for OS process-spawn capacity (reproduced directly: two
    // fresh timeouts landed here, never past it, across 18 full-suite runs).
    let first_request =
        expect_approval_requested_within(&mut approver, Duration::from_secs(10)).await;
    assert_eq!(first_request.tool, "send_handoff");

    // A second request on the same connection must be refused while the
    // first is still pending: the slot is held for the whole approval wait,
    // not just the readiness wait.
    send_handoff_from_profile(&mut requester, "ready", None, None, "r", "t").await;
    let msg = read_response(&mut requester, Duration::from_secs(5)).await;
    let message = expect_error(msg, "resource_exhausted");
    assert!(message.contains("per-connection"));

    decide(&mut approver, &first_request.request_id, false).await;
    let msg = read_response(&mut requester, Duration::from_secs(5)).await;
    expect_error(msg, "approval_denied");

    // The slot is released before that reply is sent, so
    // a third request must be accepted into the pending state again.
    send_handoff_from_profile(&mut requester, "ready", None, None, "r", "t").await;
    let third_request =
        expect_approval_requested_within(&mut approver, Duration::from_secs(10)).await;
    assert_eq!(third_request.tool, "send_handoff");
    decide(&mut approver, &third_request.request_id, false).await;
    let msg = read_response(&mut requester, Duration::from_secs(5)).await;
    expect_error(msg, "approval_denied");
}

#[tokio::test]
async fn a_second_stage_slot_failure_rolls_back_the_first_stage_reservation() {
    let tree = TempDir::new("yagura-cli-test", "handoff-slot-rollback");
    let profiles_dir = tree.make_dir("profiles");
    let detection_dir = tree.make_dir("detection");
    write_profile(
        &profiles_dir,
        "never-ready",
        &echo_child_profile_source("Never Ready"),
    );
    write_profile(
        &profiles_dir,
        "ready",
        &ready_child_profile_source_with_composer("Ready", 0),
    );

    let mut config = handoff_profile_config(
        Some(profiles_dir),
        empty_lookup(),
        &detection_dir,
        None,
        Some(policy(&["send_handoff"])),
        30,
    );
    config.max_open_handoffs = 1;
    config.max_open_handoffs_per_conn = 1;
    let h = Harness::start("handoff-slot-rollback", config).await;

    // Connection X holds the only daemon-wide slot.
    let mut x = h.connect().await;
    send_handoff_from_profile(&mut x, "never-ready", None, None, "r", "t").await;
    let held = wait_for_window_with_vendor(&mut x, "never-ready", Duration::from_secs(5)).await;

    // Connection Y's first attempt fails on the daemon-wide limit. If the
    // two-stage acquisition (`HandoffSlot::try_acquire`) failed to roll back
    // Y's own per-connection reservation after the daemon-wide stage
    // refused it, Y's *next* attempt below would wrongly fail with the
    // per-connection wording instead of succeeding.
    let mut y = h.connect().await;
    send_handoff_from_profile(&mut y, "ready", None, None, "r", "t").await;
    let msg = read_response(&mut y, Duration::from_secs(5)).await;
    let message = expect_error(msg, "resource_exhausted");
    assert!(
        message.contains("daemon-wide"),
        "Y's first attempt must fail on the daemon-wide limit, not the per-connection one: \
         {message:?}"
    );

    // Freeing X's slot: disconnect lets the daemon close X's held window.
    drop(x);
    h.wait_until_child_exited(&held.window_id, Duration::from_secs(10))
        .await;

    // Y's next attempt must now succeed.
    send_handoff_from_profile(&mut y, "ready", None, None, "r", "t").await;
    match read_response(&mut y, Duration::from_secs(10)).await {
        DaemonMessage::HandoffSent { opened, .. } => assert!(opened),
        DaemonMessage::Error { code, message, .. } => panic!(
            "Y's retry must succeed once X's slot is freed, got error({code}): {message} -- a \
             per-connection failure here means Y's own counter was never rolled back after its \
             first, daemon-wide-refused attempt"
        ),
        other => panic!("expected handoff_sent, got {other:?}"),
    }
}

#[tokio::test]
async fn send_handoff_from_a_profile_approved_by_a_registered_approver_delivers_the_envelope_and_keeps_the_window()
 {
    let tree = TempDir::new("yagura-cli-test", "handoff-profile-approved");
    let profiles_dir = tree.make_dir("profiles");
    let detection_dir = tree.make_dir("detection");
    write_profile(
        &profiles_dir,
        "ready",
        &ready_child_profile_source_with_composer("Ready", 0),
    );

    // No auto_approve: a human decision is required.
    let config = handoff_profile_config(
        Some(profiles_dir),
        empty_lookup(),
        &detection_dir,
        None,
        None,
        30,
    );
    let h = Harness::start("handoff-profile-approved", config).await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;

    let mut requester = h.connect().await;
    send_handoff_from_profile(
        &mut requester,
        "ready",
        None,
        None,
        "src/foo.rs",
        "look here",
    )
    .await;

    // Same rationale as the denied/pending profile cases above: this
    // request just launched a fresh profile window, so the wait covers a
    // process spawn plus detection, not just an IPC round trip.
    let request = expect_approval_requested_within(&mut approver, Duration::from_secs(10)).await;
    assert_eq!(request.tool, "send_handoff");
    let window_id = request
        .target
        .as_ref()
        .expect("send_handoff must carry a target")
        .window_id
        .clone();

    decide(&mut approver, &request.request_id, true).await;

    let msg = read_response(&mut requester, Duration::from_secs(5)).await;
    let (acked, opened, delivery) = expect_handoff_sent(msg);
    assert_eq!(delivery, HandoffDelivery::NotObserved);
    assert_eq!(acked, window_id);
    assert!(opened, "a profile target must always report opened: true");

    let json =
        wait_for_accepted_handoff_envelope_json(&h, &window_id, Duration::from_secs(10)).await;
    assert_eq!(
        json["from"], "external",
        "an unidentified connection's envelope must carry \"from\":\"external\": {json}"
    );
    assert!(json.get("reply").is_none());
    assert_eq!(json["reference"], "src/foo.rs");
    assert_eq!(json["note"], "look here");

    let windows = call_list_windows(&mut requester).await;
    let info = find_window(&windows, &window_id);
    assert!(
        info.exit_code.is_none(),
        "an approved handoff must leave the profile-opened window running, got exit_code {:?}",
        info.exit_code
    );

    h.registry.terminate_all().await;
}

/// `open_planned(..)` can fail *after* `HandoffSlot::try_acquire`
/// already reserved a slot (here, because the daemon's separate
/// `max_windows` limit is full), and that refusal must not leak the slot --
/// it must be back in the per-connection budget by the time the caller
/// receives *that* reply, not merely by the time some later reply arrives.
#[tokio::test]
async fn a_profile_handoff_refused_by_the_window_limit_after_acquiring_its_slot_releases_the_slot_before_the_response()
 {
    let tree = TempDir::new("yagura-cli-test", "handoff-slot-open-planned-failure");
    let profiles_dir = tree.make_dir("profiles");
    let detection_dir = tree.make_dir("detection");
    write_profile(
        &profiles_dir,
        "ready",
        &ready_child_profile_source_with_composer("Ready", 0),
    );

    let mut config = handoff_profile_config(
        Some(profiles_dir),
        empty_lookup(),
        &detection_dir,
        None,
        Some(policy(&["send_handoff", "close_window"])),
        30,
    );
    config.max_windows = 1;
    config.max_open_handoffs_per_conn = 1;
    let h = Harness::start("handoff-slot-open-planned-failure", config).await;
    let mut client = h.connect().await;

    // Fill the daemon's one-window budget with a plain window, so the
    // handoff slot below is acquired (both the per-connection and
    // daemon-wide handoff budgets have room) before the window it would open
    // is refused by the separate `max_windows` limit inside `open_planned`.
    send(
        &mut client,
        &ClientMessage::OpenWindow {
            command: echo_child_command(),
            cwd: None,
            name: None,
            vendor: None,
            detection: None,
        },
    )
    .await;
    let requester_window_id =
        expect_window_opened(read_response(&mut client, Duration::from_secs(5)).await);

    send_handoff_from_profile(&mut client, "ready", None, None, "r", "t").await;
    let msg = read_response(&mut client, Duration::from_secs(5)).await;
    let message = expect_error(msg, "resource_exhausted");
    assert!(
        message.contains("maximum number of windows"),
        "the refusal must name the window limit (max_windows), the one open_planned's own \
         open_with_extra_env enforces, not the handoff slot budget: {message:?}"
    );
    assert!(
        !message.contains("per-connection") && !message.contains("daemon-wide"),
        "this refusal comes from the window limit, not from HandoffSlot::try_acquire, so it \
         must not carry the handoff-slot wording: {message:?}"
    );

    // Free the window limit so the next attempt can actually open a window.
    send(
        &mut client,
        &ClientMessage::CloseWindow {
            window_id: requester_window_id.clone(),
            force: Some(true),
        },
    )
    .await;
    match read_response(&mut client, Duration::from_secs(10)).await {
        DaemonMessage::WindowClosed { window_id: acked } => {
            assert_eq!(acked, requester_window_id);
        }
        other => panic!("expected window_closed, got {other:?}"),
    }

    // If the first attempt's handoff slot was never released, this
    // connection's per-connection budget (max_open_handoffs_per_conn = 1) is
    // still exhausted and this request fails with the per-connection
    // wording instead of succeeding.
    send_handoff_from_profile(&mut client, "ready", None, None, "r2", "t2").await;
    match read_response(&mut client, Duration::from_secs(10)).await {
        DaemonMessage::HandoffSent { opened, .. } => {
            assert!(opened, "a profile target must always report opened: true");
        }
        DaemonMessage::Error { code, message, .. } => panic!(
            "the second attempt must succeed once the window limit is freed, got \
             error({code}): {message} -- a per-connection resource_exhausted here means the \
             handoff slot acquired by the first, window-limit-refused attempt was never \
             released"
        ),
        other => panic!("expected handoff_sent, got {other:?}"),
    }
}

// =============================================================================
// send_handoff: submit-plan / delivery-confirmation contract, profile-
// addressed.
// =============================================================================

/// The window a profile handoff has opened, once the first attempt has written
/// into it; then another writer's keystroke, typed and erased in one write so
/// the composer is as empty as before, ends that attempt's wait.
async fn end_the_first_attempts_wait_with_a_keystroke(h: &Harness) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let window = loop {
        if let Some(info) = h.registry.list().into_iter().next()
            && let Some(window) = h.registry.get(&info.window_id)
            && window.write_seq() > 0
        {
            break window;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the handoff never wrote into its window"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    window
        .write_pty_input(b"Z\x7f", yagura_daemon::window::InputOrigin::Interactive)
        .await
        .expect("the keystroke must not fail");
}

#[tokio::test]
async fn send_handoff_from_a_profile_waits_on_a_probe_the_startup_drain_dropped_and_resends_once_another_writer_ends_the_wait()
 {
    // (j), defect B: the fixture drops its first read whole -- the first
    // attempt's probe -- so that attempt waits for a probe that never shows,
    // with no clock to end it. Another writer's keystroke does, and only a
    // later attempt gets as far as the body.
    let tree = TempDir::new("yagura-cli-test", "handoff-j-defect-b");
    let profiles_dir = tree.make_dir("profiles");
    let detection_dir = tree.make_dir("detection");
    write_submit_definition(
        &detection_dir,
        "handoff-j",
        "handoff-j-marker",
        SubmitDefinitionInput {
            bracketed_paste: true,
            ready_hold_ms: 0,
            submit_delay_ms: 900,
            clear: "\\u0015",
            delivery_confirm_ms: 800,
            resend_max: 4,
        },
    );
    write_profile(
        &profiles_dir,
        "submit-j",
        &submit_child_profile_source_with_detection_and_composer(
            "Submit J",
            "handoff-j-marker",
            1,
            0,
            0,
            "handoff-j",
            0,
        ),
    );

    let config = handoff_profile_config(
        Some(profiles_dir),
        empty_lookup(),
        &detection_dir,
        None,
        Some(policy(&["send_handoff"])),
        30,
    );
    let h = Harness::start("handoff-j-defect-b", config).await;
    let mut client = h.connect().await;

    let (msg, ()) = tokio::join!(
        call_send_handoff_from_profile_within(
            &mut client,
            "submit-j",
            None,
            None,
            "src/foo.rs",
            "look here",
            Duration::from_secs(40),
        ),
        end_the_first_attempts_wait_with_a_keystroke(&h),
    );
    let (window_id, opened, delivery) = expect_handoff_sent(msg);
    assert!(opened);
    assert_eq!(delivery, HandoffDelivery::Confirmed);

    assert_eq!(
        count_marker(&h, &window_id, "SUBMITTED "),
        1,
        "the body is accepted exactly once"
    );
    assert_eq!(
        count_marker(&h, &window_id, "PASTE "),
        1,
        "the body is written once: the attempt the startup discard swallowed never got as far \
         as writing it"
    );

    h.registry.terminate_all().await;
}

/// Control for the test above: the same fixture, but `resend_max = 0` -- the
/// first (and only) attempt's probe is dropped and its wait ended by the
/// keystroke, with nothing left to retry it.
#[tokio::test]
async fn send_handoff_from_a_profile_with_no_resend_budget_never_recovers_from_the_startup_discard_window()
 {
    let tree = TempDir::new("yagura-cli-test", "handoff-j-control");
    let profiles_dir = tree.make_dir("profiles");
    let detection_dir = tree.make_dir("detection");
    write_submit_definition(
        &detection_dir,
        "handoff-j-control",
        "handoff-j-control-marker",
        SubmitDefinitionInput {
            bracketed_paste: true,
            ready_hold_ms: 0,
            submit_delay_ms: 900,
            clear: "\\u0015",
            delivery_confirm_ms: 800,
            resend_max: 0,
        },
    );
    write_profile(
        &profiles_dir,
        "submit-j-control",
        &submit_child_profile_source_with_detection_and_composer(
            "Submit J Control",
            "handoff-j-control-marker",
            1,
            0,
            0,
            "handoff-j-control",
            0,
        ),
    );

    let config = handoff_profile_config(
        Some(profiles_dir),
        empty_lookup(),
        &detection_dir,
        None,
        Some(policy(&["send_handoff"])),
        30,
    );
    let h = Harness::start("handoff-j-control", config).await;
    let mut client = h.connect().await;

    let (msg, ()) = tokio::join!(
        call_send_handoff_from_profile_within(
            &mut client,
            "submit-j-control",
            None,
            None,
            "src/foo.rs",
            "look here",
            Duration::from_secs(15),
        ),
        end_the_first_attempts_wait_with_a_keystroke(&h),
    );
    let (window_id, opened, delivery) = expect_handoff_sent(msg);
    assert!(opened);
    assert_eq!(
        delivery,
        HandoffDelivery::NotWritten,
        "the sole attempt's probe is swallowed by the startup discard and its wait ended by \
         another writer, so the delivery outcome itself must report that no body was ever \
         written"
    );

    assert_eq!(count_marker(&h, &window_id, "SUBMITTED "), 0);
    assert_eq!(
        count_marker(&h, &window_id, "PASTE "),
        0,
        "the only attempt never gets as far as writing its body"
    );

    h.registry.terminate_all().await;
}

// =============================================================================
// Launch options: the two optional values the profile form of a request may
// carry, measured where they end up -- in the child process's own argv.
// =============================================================================

/// The launch-option tables an `argv_probe` fixture profile carries when it
/// maps both axes. The alias table is written in an order sorting would
/// visibly change, so a listing that sorted it would fail rather than pass.
const BOTH_AXES_TABLES: &str = "\n[model]\nargs = [\"--tier\", \"{value}\"]\n\n\
     [model.aliases]\ngamma = \"tier-c\"\nalpha = \"tier-a\"\nbeta = \"tier-b\"\n\n\
     [effort]\nargs = [\"-c\", \"tier={value}\"]\n";

/// The same, mapping the model axis alone: a profile that accepts nothing for
/// the other axis.
const MODEL_AXIS_ONLY_TABLES: &str =
    "\n[model]\nargs = [\"--tier\", \"{value}\"]\n\n[model.aliases]\nalpha = \"tier-a\"\n";

/// Writes a frame the way a client that predates a field would: as bytes,
/// without going through the current `ClientMessage`. Local to this file on
/// purpose (the same way `ipc_server.rs` keeps its own), because a shared
/// helper would invite tests to bypass the typed messages by default.
async fn send_raw_frame(client: &mut Connection, body: &[u8]) {
    use tokio::io::AsyncWriteExt;

    let frame = yagura_protocol::frame_encode(body);
    client
        .write_all(&frame)
        .await
        .expect("write_all raw frame to the endpoint");
}

/// Asserts `needles` appear in `scrollback` in the order given: what is
/// measured is the sequence the argv elements were appended in, which mere
/// presence would not catch.
fn assert_in_order(scrollback: &[u8], needles: &[&str]) {
    let text = String::from_utf8_lossy(scrollback);
    let mut from = 0usize;
    for needle in needles {
        let at = text[from..].find(needle).unwrap_or_else(|| {
            panic!(
                "{needle:?} must appear after byte offset {from} of the child's output, in the \
                 order the elements were appended; output was {text:?}"
            )
        });
        from += at + needle.len();
    }
}

#[tokio::test]
async fn open_window_from_profile_carries_both_supplied_launch_options_into_the_child_argv() {
    let tree = TempDir::new("yagura-cli-test", "ipc-profile-launch-options");
    let profiles_dir = tree.make_dir("profiles");
    write_profile(
        &profiles_dir,
        "with-options",
        &argv_probe_profile_source("With Options", BOTH_AXES_TABLES),
    );

    let config = config_with(Some(profiles_dir), empty_lookup());
    let harness = Harness::start("ipc-profile-launch-options", config).await;
    let mut client = harness.connect().await;

    let window_id = expect_window_opened(
        call_open_window_from_profile_with_options(
            &mut client,
            "with-options",
            None,
            None,
            Some("alpha"),
            Some("tier-b"),
        )
        .await,
    );

    harness
        .wait_for_scrollback(
            &window_id,
            b"<<argv[4]=tier=tier-b>>",
            Duration::from_secs(10),
        )
        .await;
    assert_in_order(
        &harness.scrollback(&window_id),
        &[
            "<<argv[1]=--tier>>",
            "<<argv[2]=tier-a>>",
            "<<argv[3]=-c>>",
            "<<argv[4]=tier=tier-b>>",
        ],
    );

    harness.registry.terminate_all().await;
}

#[tokio::test]
async fn open_window_from_profile_refuses_an_axis_the_profile_does_not_map_and_opens_nothing() {
    let tree = TempDir::new("yagura-cli-test", "ipc-profile-launch-option-unsupported");
    let profiles_dir = tree.make_dir("profiles");
    write_profile(
        &profiles_dir,
        "one-axis",
        &argv_probe_profile_source("One Axis", MODEL_AXIS_ONLY_TABLES),
    );

    let config = config_with(Some(profiles_dir), empty_lookup());
    let harness = Harness::start("ipc-profile-launch-option-unsupported", config).await;
    let mut client = harness.connect().await;

    let before = call_list_windows(&mut client).await.len();
    let message = expect_error(
        call_open_window_from_profile_with_options(
            &mut client,
            "one-axis",
            None,
            None,
            None,
            Some("tier-b"),
        )
        .await,
        "invalid_argument",
    );
    assert!(
        message.contains("does not map the effort option"),
        "the refusal names the axis the profile has no mapping for: {message:?}"
    );
    assert_eq!(
        call_list_windows(&mut client).await.len(),
        before,
        "the refusal happens before anything is spawned, so no child outlives it"
    );
}

#[tokio::test]
async fn open_window_from_profile_refuses_a_value_outside_the_argv_token_set_and_names_its_axis() {
    let tree = TempDir::new("yagura-cli-test", "ipc-profile-launch-option-bad-value");
    let profiles_dir = tree.make_dir("profiles");
    write_profile(
        &profiles_dir,
        "with-options",
        &argv_probe_profile_source("With Options", BOTH_AXES_TABLES),
    );

    let config = config_with(Some(profiles_dir), empty_lookup());
    let harness = Harness::start("ipc-profile-launch-option-bad-value", config).await;
    let mut client = harness.connect().await;

    let message = expect_error(
        call_open_window_from_profile_with_options(
            &mut client,
            "with-options",
            None,
            None,
            Some("a\nb"),
            None,
        )
        .await,
        "invalid_argument",
    );
    assert!(
        message.starts_with("model:"),
        "a caller supplying two values needs to know which one was refused, so \
         the axis leads the message: {message:?}"
    );

    let message = expect_error(
        call_open_window_from_profile_with_options(
            &mut client,
            "with-options",
            None,
            None,
            None,
            Some("a\u{202E}b"),
        )
        .await,
        "invalid_argument",
    );
    assert!(message.starts_with("effort:"), "{message:?}");

    assert!(
        call_list_windows(&mut client).await.is_empty(),
        "a value outside the launch option value rule never reaches a spawn"
    );
}

/// Values a launcher on some spawn path could hand to a shell for
/// reinterpretation (a command substitution, a statement separator, a pipe,
/// a percent-expansion, a flag). None of these ever reach a spawn: the argv
/// token rule refuses every one of them before `resolve_launch` runs.
const REINTERPRETABLE_VALUES: [&str; 5] = ["x$(id)y", "a;b", "a|b", "a%b", "--flag"];

#[tokio::test]
async fn open_window_from_profile_refuses_a_value_a_launcher_could_reinterpret() {
    let tree = TempDir::new(
        "yagura-cli-test",
        "ipc-profile-launch-option-reinterpretable",
    );
    let profiles_dir = tree.make_dir("profiles");
    write_profile(
        &profiles_dir,
        "with-options",
        &argv_probe_profile_source("With Options", BOTH_AXES_TABLES),
    );

    let config = config_with(Some(profiles_dir), empty_lookup());
    let harness = Harness::start("ipc-profile-launch-option-reinterpretable", config).await;
    let mut client = harness.connect().await;

    for value in REINTERPRETABLE_VALUES {
        let message = expect_error(
            call_open_window_from_profile_with_options(
                &mut client,
                "with-options",
                None,
                None,
                Some(value),
                None,
            )
            .await,
            "invalid_argument",
        );
        assert!(
            message.starts_with("model:"),
            "a value a launcher could hand to a shell must be refused before a \
             spawn: {value:?} -> {message:?}"
        );
    }

    assert!(
        call_list_windows(&mut client).await.is_empty(),
        "none of the refused values reach a spawn"
    );
}

#[tokio::test]
async fn send_handoff_from_a_profile_refuses_a_value_a_launcher_could_reinterpret() {
    let tree = TempDir::new(
        "yagura-cli-test",
        "handoff-profile-launch-option-reinterpretable",
    );
    let profiles_dir = tree.make_dir("profiles");
    let detection_dir = tree.make_dir("detection");
    write_profile(
        &profiles_dir,
        "with-options",
        &argv_probe_profile_source("With Options", BOTH_AXES_TABLES),
    );

    let config = handoff_profile_config(
        Some(profiles_dir),
        empty_lookup(),
        &detection_dir,
        None,
        Some(policy(&["send_handoff"])),
        30,
    );
    let h = Harness::start("handoff-profile-launch-option-reinterpretable", config).await;
    let mut client = h.connect().await;

    for value in REINTERPRETABLE_VALUES {
        let message = expect_error(
            call_send_handoff_from_profile_with_options(
                &mut client,
                "with-options",
                None,
                None,
                "ref",
                "note",
                Some(value),
                None,
            )
            .await,
            "invalid_argument",
        );
        assert!(
            message.starts_with("model:"),
            "a value a launcher could hand to a shell must be refused before a \
             spawn: {value:?} -> {message:?}"
        );
    }

    assert!(
        call_list_windows(&mut client).await.is_empty(),
        "none of the refused values reach a spawn"
    );
}

#[tokio::test]
async fn open_window_from_profile_refuses_the_value_before_it_looks_the_profile_up() {
    let tree = TempDir::new("yagura-cli-test", "ipc-profile-launch-option-order");
    let profiles_dir = tree.make_dir("profiles");

    let config = config_with(Some(profiles_dir), empty_lookup());
    let harness = Harness::start("ipc-profile-launch-option-order", config).await;
    let mut client = harness.connect().await;

    let message = expect_error(
        call_open_window_from_profile_with_options(
            &mut client,
            "no-such-profile",
            None,
            None,
            Some("a\nb"),
            None,
        )
        .await,
        "invalid_argument",
    );
    assert!(
        message.starts_with("model:"),
        "the value check is the cheapest and needs no directory read, so it \
         answers first even when the profile is unknown too: {message:?}"
    );
}

#[tokio::test]
async fn open_window_from_profile_still_opens_from_a_frame_that_predates_the_launch_options() {
    let tree = TempDir::new("yagura-cli-test", "ipc-profile-launch-option-old-frame");
    let profiles_dir = tree.make_dir("profiles");
    write_profile(&profiles_dir, "plain", &echo_child_profile_source("Plain"));

    let config = config_with(Some(profiles_dir), empty_lookup());
    let harness = Harness::start("ipc-profile-launch-option-old-frame", config).await;
    let mut client = harness.connect().await;

    send_raw_frame(
        &mut client,
        br#"{"type":"open_window_from_profile","profile":"plain","cwd":null,"name":null}"#,
    )
    .await;
    let window_id = expect_window_opened(read_response(&mut client, Duration::from_secs(5)).await);

    let windows = call_list_windows(&mut client).await;
    assert_eq!(
        find_window(&windows, &window_id).vendor.as_deref(),
        Some("plain"),
        "a client built before the two keys existed keeps working unchanged"
    );

    harness.registry.terminate_all().await;
}

#[tokio::test]
async fn send_handoff_from_a_profile_carries_a_supplied_launch_option_into_the_child_argv() {
    let tree = TempDir::new("yagura-cli-test", "handoff-profile-launch-options");
    let profiles_dir = tree.make_dir("profiles");
    let detection_dir = tree.make_dir("detection");
    write_profile(
        &profiles_dir,
        "with-options",
        &argv_probe_profile_source("With Options", BOTH_AXES_TABLES),
    );

    let mut config = handoff_profile_config(
        Some(profiles_dir),
        empty_lookup(),
        &detection_dir,
        None,
        Some(policy(&["send_handoff"])),
        30,
    );
    config.handoff_ready_timeout_secs = 1;
    let h = Harness::start("handoff-profile-launch-options", config).await;
    let mut client = h.connect().await;

    let msg = call_send_handoff_from_profile_with_options(
        &mut client,
        "with-options",
        None,
        None,
        "src/foo.rs",
        "look",
        Some("alpha"),
        Some("tier-b"),
    )
    .await;
    // The probe writes its argv and exits at once, so it never reaches a
    // ready state and the handoff is abandoned rather than delivered. The
    // window it opened is retired, but its entry and scrollback stay.
    expect_error(msg, "window_not_ready");

    let opened =
        wait_for_window_with_vendor(&mut client, "with-options", Duration::from_secs(10)).await;
    h.wait_for_scrollback(
        &opened.window_id,
        b"<<argv[4]=tier=tier-b>>",
        Duration::from_secs(10),
    )
    .await;
    assert_in_order(
        &h.scrollback(&opened.window_id),
        &[
            "<<argv[1]=--tier>>",
            "<<argv[2]=tier-a>>",
            "<<argv[3]=-c>>",
            "<<argv[4]=tier=tier-b>>",
        ],
    );
}

#[tokio::test]
async fn send_handoff_addressed_to_an_existing_window_refuses_a_launch_option() {
    let tree = TempDir::new("yagura-cli-test", "handoff-window-launch-option");
    let profiles_dir = tree.make_dir("profiles");
    let detection_dir = tree.make_dir("detection");
    write_profile(&profiles_dir, "plain", &echo_child_profile_source("Plain"));

    let config = handoff_profile_config(
        Some(profiles_dir),
        empty_lookup(),
        &detection_dir,
        None,
        Some(policy(&["send_handoff"])),
        30,
    );
    let h = Harness::start("handoff-window-launch-option", config).await;
    let mut client = h.connect().await;

    let window_id =
        expect_window_opened(call_open_window_from_profile(&mut client, "plain", None, None).await);

    send(
        &mut client,
        &ClientMessage::SendHandoff {
            window_id: Some(window_id.clone()),
            profile: None,
            cwd: None,
            name: None,
            model: Some("alpha".to_string()),
            effort: None,
            reference: "handoff-reference-marker".to_string(),
            note: "handoff-note-marker".to_string(),
        },
    )
    .await;
    let message = expect_error(
        read_response(&mut client, Duration::from_secs(5)).await,
        "invalid_argument",
    );
    assert_eq!(
        message, "model and effort cannot be specified with window_id",
        "a window that is already running was launched with whatever it was \
         launched with, so this is refused rather than quietly dropped"
    );

    tokio::time::sleep(Duration::from_millis(200)).await;
    let scrollback = h.scrollback(&window_id);
    assert!(
        !common::contains_subsequence(&scrollback, b"handoff-reference-marker"),
        "a refused handoff writes nothing into the window it named: {:?}",
        String::from_utf8_lossy(&scrollback)
    );

    h.registry.terminate_all().await;
}

#[tokio::test]
async fn list_profiles_reports_the_alias_table_in_file_order_and_nothing_for_an_unmapped_axis() {
    let tree = TempDir::new("yagura-cli-test", "ipc-profile-launch-option-listing");
    let profiles_dir = tree.make_dir("profiles");
    write_profile(
        &profiles_dir,
        "with-options",
        &argv_probe_profile_source("With Options", BOTH_AXES_TABLES),
    );
    write_profile(
        &profiles_dir,
        "one-axis",
        &argv_probe_profile_source("One Axis", MODEL_AXIS_ONLY_TABLES),
    );

    let config = config_with(Some(profiles_dir), empty_lookup());
    let harness = Harness::start("ipc-profile-launch-option-listing", config).await;
    let mut client = harness.connect().await;

    let (profiles, warnings) = call_list_profiles(&mut client).await;
    assert!(warnings.is_empty(), "{warnings:?}");

    let both = find_profile(&profiles, "with-options");
    let model = both
        .model
        .as_ref()
        .expect("this profile maps the model axis");
    assert_eq!(
        model
            .aliases
            .iter()
            .map(|entry| (entry.alias.as_str(), entry.value.as_str()))
            .collect::<Vec<_>>(),
        vec![("gamma", "tier-c"), ("alpha", "tier-a"), ("beta", "tier-b"),],
        "what a dialog offers is the author's own order, so the wire must not \
         sort it on the way out"
    );
    assert!(
        both.effort
            .as_ref()
            .expect("this profile maps the effort axis")
            .aliases
            .is_empty(),
        "an axis mapped without aliases accepts anything, which is a different \
         answer from not accepting the axis"
    );

    let one = find_profile(&profiles, "one-axis");
    assert!(one.model.is_some());
    assert_eq!(
        one.effort, None,
        "an axis the profile does not map is reported as absent, which is what \
         lets a caller stop offering it"
    );
}
