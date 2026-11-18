//! Where the executables these tests run are, and the argv and profile sources
//! built on them.
//!
//! A bin of this package comes from Cargo's `CARGO_BIN_EXE_<name>`. A bin of
//! another package (the holder's, the relay) is looked up in the profile
//! directory Cargo uplifts every final artifact to, found through
//! [`profile_dir`]. Nothing here follows the running test binary's own path,
//! whose place inside the build directory Cargo does not promise.
#![expect(
    dead_code,
    reason = "part of these helpers have no caller until the remaining tests move off yagura_test_support's lookups"
)]

use std::path::{Path, PathBuf};

use yagura_daemon::DaemonConfig;
use yagura_test_support::detection::CONTROL_READY_MARKER;
use yagura_test_support::process::{composer_arguments, profile_source};

/// The profile directory every bin of the workspace is uplifted to: the one
/// holding this package's `yagura-daemon`. Every path to another package's
/// bin goes through here.
///
/// # Panics
///
/// Panics if Cargo's path for `yagura-daemon` has no parent directory.
pub fn profile_dir() -> PathBuf {
    Path::new(env!("CARGO_BIN_EXE_yagura-daemon"))
        .parent()
        .expect("CARGO_BIN_EXE_yagura-daemon names a file inside the profile directory")
        .to_path_buf()
}

/// `name` from `package`, in [`profile_dir`].
///
/// # Panics
///
/// Panics if it is not built there; the message names the build command.
fn other_package_bin(package: &str, name: &str) -> PathBuf {
    let path = profile_dir().join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
    assert!(
        path.exists(),
        "{name} is missing at {}: run `cargo build -p {package} --bins` before these tests",
        path.display()
    );
    path
}

/// The `yagura-holder` a window of an in-process daemon runs in.
///
/// # Panics
///
/// Panics if it is not built; see [`other_package_bin`].
pub fn holder_path() -> PathBuf {
    other_package_bin("yagura-holder", "yagura-holder")
}

/// The `holder_fault` fixture: a holder that fails on purpose.
///
/// # Panics
///
/// Panics if it is not built; see [`other_package_bin`].
pub fn holder_fault_path() -> PathBuf {
    other_package_bin("yagura-holder", "holder_fault")
}

/// The `holder_child` fixture a holder can be told to run.
///
/// # Panics
///
/// Panics if it is not built; see [`other_package_bin`].
pub fn holder_child_path() -> PathBuf {
    other_package_bin("yagura-holder", "holder_child")
}

/// The `yagura-watcher` the holder starts beside itself.
///
/// # Panics
///
/// Panics if it is not built; see [`other_package_bin`].
#[cfg(unix)]
pub fn watcher_path() -> PathBuf {
    other_package_bin("yagura-holder", "yagura-watcher")
}

/// The `yagura-mcp` relay a window's MCP client runs.
///
/// # Panics
///
/// Panics if it is not built; see [`other_package_bin`].
pub fn relay_path() -> PathBuf {
    other_package_bin("yagura-mcp", "yagura-mcp")
}

/// The `yagura-daemon` a test starts as a real process. That daemon runs its
/// windows in the holder beside it, so the path is taken from [`profile_dir`]
/// like the holder's own.
///
/// # Panics
///
/// Panics under the same condition as [`profile_dir`].
pub fn daemon_exe() -> PathBuf {
    let exe = Path::new(env!("CARGO_BIN_EXE_yagura-daemon"));
    profile_dir().join(
        exe.file_name()
            .expect("CARGO_BIN_EXE_yagura-daemon names a file"),
    )
}

/// `base` with the holder of [`holder_path`] when it names none. A holder the
/// caller set is kept.
///
/// # Panics
///
/// Panics if `base` names no holder and none is built; see [`holder_path`].
pub fn config_with_holder(mut base: DaemonConfig) -> DaemonConfig {
    if base.holder_exe.is_none() {
        base.holder_exe = Some(holder_path());
    }
    base
}

/// [`config_with_holder`] over [`DaemonConfig::default`].
///
/// # Panics
///
/// Panics under the same condition as [`config_with_holder`].
pub fn default_config_with_holder() -> DaemonConfig {
    config_with_holder(DaemonConfig::default())
}

pub fn echo_child_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_echo_child"))
}

pub fn hex_echo_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_hex_echo"))
}

pub fn term_script_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_term_script"))
}

pub fn ready_child_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ready_child"))
}

pub fn env_probe_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_env_probe"))
}

pub fn argv_probe_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_argv_probe"))
}

pub fn capture_child_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_capture_child"))
}

pub fn screen_load_child_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_screen_load_child"))
}

pub fn mcp_cli_stub_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_mcp_cli_stub"))
}

pub fn hook_child_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_hook_child"))
}

pub fn hold_pty_child_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_hold_pty_child"))
}

/// `path` as one argv element.
///
/// # Panics
///
/// Panics if `path` is not valid UTF-8.
fn argv_element(path: &Path) -> String {
    path.to_str()
        .unwrap_or_else(|| panic!("{} must be valid UTF-8", path.display()))
        .to_string()
}

/// The argv of a window running `echo_child`: its path alone, with no shell
/// wrapping.
pub fn echo_child_command() -> Vec<String> {
    vec![argv_element(&echo_child_path())]
}

/// The argv of a window running `ready_child`, which prints `marker` on
/// startup.
pub fn ready_child_command(marker: &str) -> Vec<String> {
    vec![argv_element(&ready_child_path()), marker.to_string()]
}

/// [`echo_child_command`] in composer-draw mode, reflecting each write
/// `reflect_delay_ms` after it lands.
pub fn echo_child_command_with_composer(reflect_delay_ms: u64) -> Vec<String> {
    let mut command = echo_child_command();
    command.extend(composer_arguments(reflect_delay_ms));
    command
}

/// [`ready_child_command`] in composer-draw mode.
pub fn ready_child_command_with_composer(marker: &str, reflect_delay_ms: u64) -> Vec<String> {
    let mut command = ready_child_command(marker);
    command.extend(composer_arguments(reflect_delay_ms));
    command
}

/// A profile whose `[command]` runs [`ready_child_command`] with
/// [`CONTROL_READY_MARKER`], so its window reaches `Idle` under the control
/// definition without any input.
pub fn ready_child_profile_source(display_name: &str) -> String {
    profile_source(display_name, &ready_child_command(CONTROL_READY_MARKER))
}

/// [`ready_child_profile_source`] in composer-draw mode.
pub fn ready_child_profile_source_with_composer(
    display_name: &str,
    reflect_delay_ms: u64,
) -> String {
    profile_source(
        display_name,
        &ready_child_command_with_composer(CONTROL_READY_MARKER, reflect_delay_ms),
    )
}

/// A profile whose `[command]` runs `argv_probe` with no arguments of its own,
/// followed verbatim by `extra_tables`.
pub fn argv_probe_profile_source(display_name: &str, extra_tables: &str) -> String {
    format!(
        "{}{extra_tables}",
        profile_source(display_name, &[argv_element(&argv_probe_path())])
    )
}

/// The argv of a window whose direct child first starts a grandchild, waits
/// until it has written its process id to `pid_file`, lives
/// `child_lifetime_ms` more and exits `0`. No shell re-parses any value.
///
/// # Panics
///
/// Panics if `pid_file` is not valid UTF-8.
pub fn first_grandchild_command(pid_file: &Path, child_lifetime_ms: u64) -> Vec<String> {
    vec![
        argv_element(&capture_child_path()),
        "grandchild-first".to_string(),
        argv_element(pid_file),
        child_lifetime_ms.to_string(),
    ]
}

/// The argv of a window whose direct child starts a grandchild appending to
/// `tick_path` until something kills it, then exits on its own a few seconds
/// later: the natural-exit teardown route's fixture. The delay lets the
/// grandchild tick before the group is torn down at the child's exit.
pub fn grandchild_command(tick_path: &str) -> Vec<String> {
    grandchild_command_with_child_lifetime(tick_path, 4)
}

/// [`grandchild_command`] with a direct child that outlives any wait of the
/// test, for the `close_window` teardown route.
pub fn surviving_grandchild_command(tick_path: &str) -> Vec<String> {
    grandchild_command_with_child_lifetime(tick_path, 300)
}

/// `capture_child tick-grandchild`: the grandchild has no console window, so
/// it outlives the child unless the window's teardown unit reaches it.
#[cfg(windows)]
fn grandchild_command_with_child_lifetime(
    tick_path: &str,
    child_lifetime_secs: u64,
) -> Vec<String> {
    vec![
        argv_element(&capture_child_path()),
        "tick-grandchild".to_string(),
        tick_path.to_string(),
        (child_lifetime_secs * 1_000).to_string(),
    ]
}

/// A `nohup`'d grandchild ignores the session leader's `SIGHUP` yet stays in
/// the direct child's process group, as a real detached descendant does.
/// `tick_path` reaches the loop as `$0`, so it is never parsed as shell syntax.
#[cfg(not(windows))]
fn grandchild_command_with_child_lifetime(
    tick_path: &str,
    child_lifetime_secs: u64,
) -> Vec<String> {
    vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        format!(
            "nohup sh -c 'while :; do echo tick >> \"$0\"; sleep 0.2; done' {tick_path} \
             >/dev/null 2>&1 & sleep {child_lifetime_secs}"
        ),
    ]
}
