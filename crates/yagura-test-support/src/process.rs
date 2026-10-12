//! Child-process command fixtures, the running test binary's own path, and
//! output-file growth assertions.

use std::path::{Path, PathBuf};
use std::time::Duration;

/// The size of `path` in bytes, or `0` when it cannot be read.
///
/// Absence reads as zero on purpose: the growth helpers below start polling
/// before the child has created its output file.
#[must_use]
pub fn file_len(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

/// Stage 1 of the grandchild observation: the descendant is demonstrably
/// alive, because the file it appends to keeps growing.
///
/// # Panics
///
/// Panics if the file has not grown within `timeout`, which means the fixture
/// never established its precondition.
pub async fn wait_until_file_grows(path: &Path, timeout: Duration) {
    let deadline = tokio::time::Instant::now() + timeout;
    let start = file_len(path);
    loop {
        if file_len(path) > start {
            return;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!(
                "grandchild never appended to {} (fixture precondition failed)",
                path.display()
            );
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Stage 2 of the grandchild observation: after teardown, the file's size must
/// stay put. Size is the signal (not mtime), sampled by polling over a
/// generous window.
///
/// `context` names the teardown route the caller exercised and is reported
/// verbatim on failure, so the failure text says which route leaked.
///
/// # Panics
///
/// Panics if the file grows during the observation window, which means an
/// orphaned descendant survived the teardown.
pub async fn assert_file_len_stable(path: &Path, observation: Duration, context: &str) {
    // One in-flight iteration may still land right after the kill.
    tokio::time::sleep(Duration::from_millis(1000)).await;
    let baseline = file_len(path);
    let deadline = tokio::time::Instant::now() + observation;
    while tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(200)).await;
        let current = file_len(path);
        assert_eq!(
            current,
            baseline,
            "{} kept growing {context}: an orphaned descendant survived",
            path.display()
        );
    }
}

/// Command whose direct child starts a grandchild that keeps appending to
/// `tick_path` until something kills it, then exits **on its own** a few
/// seconds later -- the fixture for the natural-exit teardown route.
///
/// The child's short delay is what makes the fixture observable: that route
/// tears the process group down as soon as the direct child exits, so an
/// immediately exiting child would kill the grandchild before its first tick.
/// The grandchild has no console window, so it outlives the child unless the
/// window's teardown unit reaches it; see [`tick_grandchild_command`].
#[cfg(windows)]
#[must_use]
pub fn grandchild_command(tick_path: &str) -> Vec<String> {
    tick_grandchild_command(tick_path, 4_000)
}

/// Same grandchild as [`grandchild_command`], but the direct child is kept
/// alive far longer -- this is the `close_window` teardown route's fixture,
/// where the test needs to observe the direct child still running right up
/// until the close is sent.
///
/// The long delay comfortably clears the [`wait_until_file_grows`] budget its
/// callers use, and `close_window` kills the whole tree regardless of how much
/// of the delay is left.
#[cfg(windows)]
#[must_use]
pub fn surviving_grandchild_command(tick_path: &str) -> Vec<String> {
    tick_grandchild_command(tick_path, 300_000)
}

/// The `capture_child tick-grandchild` argv behind both Windows grandchild
/// fixtures; they differ only in `child_lifetime_ms`. No shell is involved, so
/// `tick_path` is never re-parsed.
#[cfg(windows)]
fn tick_grandchild_command(tick_path: &str, child_lifetime_ms: u64) -> Vec<String> {
    vec![
        capture_child_path()
            .to_str()
            .expect("capture_child path must be valid UTF-8")
            .to_string(),
        "tick-grandchild".to_string(),
        tick_path.to_string(),
        child_lifetime_ms.to_string(),
    ]
}

/// A fixed command (not a real vendor CLI) whose direct child stays alive for
/// roughly `ping_count - 1` seconds and then exits on its own. ConPTY spawns
/// its `conhost.exe --headless` host for any command regardless of what it is,
/// so this is enough to reproduce a host-process leak without depending on an
/// installed agent CLI.
///
/// The delay uses `ping -n` rather than `timeout /t`, which fails outright
/// when stdin is redirected (as it is under a PTY).
#[cfg(windows)]
#[must_use]
pub fn sleepy_command(ping_count: &str) -> Vec<String> {
    vec![
        "cmd.exe".to_string(),
        "/c".to_string(),
        "ping".to_string(),
        "-n".to_string(),
        ping_count.to_string(),
        "127.0.0.1".to_string(),
        ">nul".to_string(),
    ]
}

/// Unix counterpart of [`grandchild_command`]: launches a `nohup`'d grandchild
/// that keeps appending to `tick_path` until something kills it, while the
/// direct child (`/bin/sh`) exits on its own after a few seconds.
///
/// `nohup` makes the grandchild ignore the `SIGHUP` its session leader would
/// otherwise send it, while it still inherits the direct child's process group
/// (neither backgrounding nor `nohup` starts a new one), so a group-wide signal
/// reaches it as it would a real detached descendant. The inner loop script
/// receives `tick_path` as `$0` rather than folded into the script text, so an
/// interpolated path is never re-parsed as shell syntax.
#[cfg(not(windows))]
#[must_use]
pub fn grandchild_command(tick_path: &str) -> Vec<String> {
    grandchild_command_with_child_lifetime(tick_path, "4")
}

/// Unix counterpart of [`surviving_grandchild_command`]: same shape as
/// [`grandchild_command`], but the direct child stays alive far longer.
#[cfg(not(windows))]
#[must_use]
pub fn surviving_grandchild_command(tick_path: &str) -> Vec<String> {
    grandchild_command_with_child_lifetime(tick_path, "300")
}

/// The single argv shape behind both Unix grandchild fixtures; they differ
/// only in `direct_child_lifetime_secs`, the number of seconds the direct
/// child sleeps before exiting on its own.
#[cfg(not(windows))]
fn grandchild_command_with_child_lifetime(
    tick_path: &str,
    direct_child_lifetime_secs: &str,
) -> Vec<String> {
    vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        format!(
            "nohup sh -c 'while :; do echo tick >> \"$0\"; sleep 0.2; done' {tick_path} \
             >/dev/null 2>&1 & sleep {direct_child_lifetime_secs}"
        ),
    ]
}

/// Unix counterpart of [`sleepy_command`]: a direct child that sleeps and
/// then exits on its own, spawning no grandchild. `ping_count` keeps the same
/// unit as the Windows fixture (a count of `ping -n`-style probes); it is
/// mapped to `max(ping_count - 1, 1)` seconds here so callers pass the same
/// numbers on both platforms.
#[cfg(not(windows))]
#[must_use]
pub fn sleepy_command(ping_count: &str) -> Vec<String> {
    let count: u64 = ping_count
        .parse()
        .expect("ping_count must be a valid non-negative integer");
    let secs = count.saturating_sub(1).max(1);
    vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        format!("sleep {secs}"),
    ]
}

/// A fixed command whose direct child exits immediately with `code` (a
/// decimal string): the shared fixture behind every window a test needs to
/// already be `Exited` by the time it looks.
#[cfg(windows)]
#[must_use]
pub fn exit_with_code_command(code: &str) -> Vec<String> {
    vec![
        "cmd.exe".to_string(),
        "/c".to_string(),
        "exit".to_string(),
        code.to_string(),
    ]
}

/// Unix counterpart of [`exit_with_code_command`].
#[cfg(not(windows))]
#[must_use]
pub fn exit_with_code_command(code: &str) -> Vec<String> {
    vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        format!("exit {code}"),
    ]
}

/// Shared resolution logic behind [`echo_child_path`] and [`env_probe_path`],
/// parameterized on the fixture's `[[bin]]` name since the two only differ in
/// which binary they point at.
fn fixture_bin_path(stem: &str) -> PathBuf {
    let exe =
        std::env::current_exe().expect("current_exe should resolve for a running test binary");
    let mut dir = fixture_bin_dir(&exe);
    let name = if cfg!(windows) {
        format!("{stem}.exe")
    } else {
        stem.to_string()
    };
    dir.push(name);
    assert!(
        dir.exists(),
        "the {stem} fixture binary is missing at {}: run `cargo build -p yagura-daemon --bins` \
         before the tests that open a window on it",
        dir.display()
    );
    dir
}

/// The directory holding the plain bin targets of the profile `exe` was
/// built in: `exe`'s own directory, or the one above it when that is `deps`.
fn fixture_bin_dir(exe: &Path) -> PathBuf {
    let mut dir = exe.to_path_buf();
    dir.pop();
    if dir.file_name().and_then(|n| n.to_str()) == Some("deps") {
        dir.pop();
    }
    dir
}

/// Path to the `echo_child` fixture binary that belongs to the test binary
/// currently running.
///
/// `cargo test` places a test binary in `target/{profile}/deps/`, while a
/// plain bin target lands in `target/{profile}/`, so the search steps up one
/// extra level when the parent directory is `deps`.
///
/// Existence is asserted here rather than left to the spawn, where an unbuilt
/// fixture surfaces only as an opaque spawn failure far from its cause.
///
/// # Panics
///
/// Panics if the running executable's own path cannot be resolved, or if the
/// fixture binary is not present next to it -- build it with
/// `cargo build -p yagura-daemon --bins` first.
#[must_use]
pub fn echo_child_path() -> PathBuf {
    fixture_bin_path("echo_child")
}

/// Path to the `hex_echo` fixture binary that belongs to the test binary
/// currently running. The fixture enters raw console mode and prints received
/// stdin bytes as lowercase hexadecimal text.
#[must_use]
pub fn hex_echo_path() -> PathBuf {
    fixture_bin_path("hex_echo")
}

/// Path to the `term_script` fixture binary that belongs to the test binary
/// currently running: a child that writes exactly the bytes a test sends it
/// as hex commands, or paints a per-cell attribute grid.
///
/// # Panics
///
/// Panics under the same conditions as [`hex_echo_path`].
#[must_use]
pub fn term_script_path() -> PathBuf {
    fixture_bin_path("term_script")
}

/// Path to the `ready_child` fixture binary that belongs to the test binary
/// currently running.
///
/// Resolved the same way as [`echo_child_path`], including the existence
/// check, and for the same reason.
///
/// # Panics
///
/// Panics if the running executable's own path cannot be resolved, or if the
/// fixture binary is not present next to it -- build it with
/// `cargo build -p yagura-daemon --bins` first.
#[must_use]
pub fn ready_child_path() -> PathBuf {
    fixture_bin_path("ready_child")
}

/// Path to the `submit_child` fixture binary that belongs to the test
/// binary currently running.
///
/// Resolved the same way as [`echo_child_path`], including the existence
/// check, and for the same reason.
///
/// # Panics
///
/// Panics if the running executable's own path cannot be resolved, or if the
/// fixture binary is not present next to it -- build it with
/// `cargo build -p yagura-daemon --bins` first.
#[must_use]
pub fn submit_child_path() -> PathBuf {
    fixture_bin_path("submit_child")
}

/// Path to the `env_probe` fixture binary that belongs to the test binary
/// currently running.
///
/// `cargo test` places a test binary in `target/{profile}/deps/`, while a
/// plain bin target lands in `target/{profile}/`, so the search steps up one
/// extra level when the parent directory is `deps`.
///
/// Existence is asserted here rather than left to the spawn, where an unbuilt
/// fixture surfaces only as an opaque spawn failure far from its cause.
///
/// # Panics
///
/// Panics if the running executable's own path cannot be resolved, or if the
/// fixture binary is not present next to it -- build it with
/// `cargo build -p yagura-daemon --bins` first.
#[must_use]
pub fn env_probe_path() -> PathBuf {
    fixture_bin_path("env_probe")
}

/// Path to the `argv_probe` fixture binary that belongs to the test binary
/// currently running.
///
/// Resolved the same way as [`echo_child_path`], including the existence
/// check, and for the same reason.
///
/// # Panics
///
/// Panics if the running executable's own path cannot be resolved, or if the
/// fixture binary is not present next to it -- build it with
/// `cargo build -p yagura-daemon --bins` first.
#[must_use]
pub fn argv_probe_path() -> PathBuf {
    fixture_bin_path("argv_probe")
}

/// Path to the `capture_child` fixture binary that belongs to the test binary
/// currently running.
///
/// # Panics
///
/// Panics under the same conditions as [`echo_child_path`].
#[must_use]
pub fn capture_child_path() -> PathBuf {
    fixture_bin_path("capture_child")
}

/// The argv for a window whose direct child starts a grandchild as its very
/// first action, waits until that grandchild has written its own process id to
/// `pid_file`, lives `child_lifetime_ms` more (`0` = exits at once) and exits
/// `0`.
///
/// The grandchild holds none of the child's standard handles and, on Windows,
/// has no console window; it outlives the child unless the window's teardown
/// unit reaches it. No shell is involved, so no value is re-parsed.
///
/// # Panics
///
/// Panics under the same conditions as [`capture_child_path`], or if a path is
/// not valid UTF-8.
#[must_use]
pub fn first_grandchild_command(pid_file: &Path, child_lifetime_ms: u64) -> Vec<String> {
    vec![
        capture_child_path()
            .to_str()
            .expect("capture_child path must be valid UTF-8")
            .to_string(),
        "grandchild-first".to_string(),
        pid_file
            .to_str()
            .expect("pid file path must be valid UTF-8")
            .to_string(),
        child_lifetime_ms.to_string(),
    ]
}

/// The canonicalized path of the test binary that is currently running.
///
/// The result is canonicalized because `current_exe()` alone is not: on
/// Windows it lacks the extended-length prefix that `canonicalize` adds, so
/// comparing it against a canonicalized path would fail there.
///
/// # Panics
///
/// Panics if the running executable's own path cannot be resolved, or if that
/// path cannot be canonicalized.
#[must_use]
pub fn own_canonical_exe() -> PathBuf {
    std::env::current_exe()
        .expect("current_exe should resolve for a running test binary")
        .canonicalize()
        .expect("canonicalize current_exe")
}

/// The command array for a window running the `echo_child` fixture: the
/// executable path as a single argv element, with no shell wrapping.
///
/// # Panics
///
/// Panics under the same conditions as [`echo_child_path`], or if that path is
/// not valid UTF-8.
#[must_use]
pub fn echo_child_command() -> Vec<String> {
    vec![
        echo_child_path()
            .to_str()
            .expect("echo_child path must be valid UTF-8")
            .to_string(),
    ]
}

/// The command array for a window running the `ready_child` fixture: the
/// executable path followed by `marker`, the line the fixture prints on
/// startup, with no shell wrapping.
///
/// # Panics
///
/// Panics under the same conditions as [`ready_child_path`], or if that path
/// is not valid UTF-8.
#[must_use]
pub fn ready_child_command(marker: &str) -> Vec<String> {
    vec![
        ready_child_path()
            .to_str()
            .expect("ready_child path must be valid UTF-8")
            .to_string(),
        marker.to_string(),
    ]
}

/// The command array for a window running the `submit_child` fixture: the
/// executable path followed by its five positional settings, with no shell
/// wrapping and no parsing beyond writing each number in decimal.
/// Slot 5 is the startup slot and holds `now`; a caller that must hold the
/// marker until a file exists writes `until|<path>` over it.
///
/// # Panics
///
/// Panics under the same conditions as [`submit_child_path`], or if that
/// path is not valid UTF-8.
#[must_use]
pub fn submit_child_command(
    marker: &str,
    discard_reads: u64,
    paste_guard_keys: u64,
    react_on_body: u8,
) -> Vec<String> {
    vec![
        submit_child_path()
            .to_str()
            .expect("submit_child path must be valid UTF-8")
            .to_string(),
        marker.to_string(),
        discard_reads.to_string(),
        paste_guard_keys.to_string(),
        react_on_body.to_string(),
        "now".to_string(),
    ]
}

/// The TOML source of a profile with `display_name` whose `[command]`
/// default is `command`, each element written as a TOML literal string.
///
/// One construction behind every profile-source helper here, so a window
/// opened from a profile always runs the very argv its direct-command
/// counterpart builds.
pub fn profile_source(display_name: &str, command: &[String]) -> String {
    let elements: Vec<String> = command
        .iter()
        .map(|element| format!("'{element}'"))
        .collect();
    format!(
        "schema = 1\ndisplay_name = \"{display_name}\"\n\n[command]\ndefault = [{}]\n",
        elements.join(", ")
    )
}

/// The TOML source of a profile whose `[command]` launches the `ready_child`
/// fixture with [`crate::detection::CONTROL_READY_MARKER`] as its marker
/// argument: it prints that line at startup, which the control definition's
/// `idle` rule reacts to,
/// so a window opened from this profile reaches `Idle` without the caller
/// having to write anything to its input.
///
/// # Panics
///
/// Panics under the same conditions as [`ready_child_command`].
#[must_use]
pub fn ready_child_profile_source(display_name: &str) -> String {
    profile_source(
        display_name,
        &ready_child_command(crate::detection::CONTROL_READY_MARKER),
    )
}

// --- composer-draw mode ------------------------------------------------------
//
// Composer-draw mode is a second entry point per fixture rather than an
// argument on the existing ones: the fixtures are launched from many call
// sites that must keep their argv, and a fixture only draws when the four
// trailing arguments below are present.

/// The marker a fixture in composer-draw mode paints at the start of its
/// composer line.
///
/// Published here because a detection definition's `[composer] marker` must
/// hold the same value, as a regex: a caller writes this constant into the
/// definition rather than restating the fixture's own string, so the two can
/// never drift. It therefore carries no regex metacharacter, and no quote,
/// backslash or newline that a TOML string would have to escape.
pub const COMPOSER_MARKER: &str = "YAGURA-COMPOSER";

/// What a fixture in composer-draw mode paints between the marker and the
/// body, and what a definition's `[composer] separator` must hold.
///
/// Absent from the painted line while the composer is empty, which is what
/// tells an empty composer apart from an unobservable one.
pub const COMPOSER_SEPARATOR: &str = ">>";

/// What a fixture in composer-draw mode paints at the start of every wrapped
/// line of a body, and what a definition's `[composer] continuation_prefix`
/// must hold.
pub const COMPOSER_CONTINUATION_PREFIX: &str = "::";

/// The four argv elements that put a fixture into composer-draw mode: the
/// shared composer triple, then `reflect_delay_ms` (`0` reflects a write as
/// soon as it arrives).
fn composer_arguments(reflect_delay_ms: u64) -> [String; 4] {
    [
        COMPOSER_MARKER.to_string(),
        COMPOSER_SEPARATOR.to_string(),
        COMPOSER_CONTINUATION_PREFIX.to_string(),
        reflect_delay_ms.to_string(),
    ]
}

/// [`echo_child_command`] in composer-draw mode: the fixture paints what is
/// written to it as a composer region and echoes a line once a submit key
/// arrives, reflecting each write `reflect_delay_ms` after it lands.
///
/// # Panics
///
/// Panics under the same conditions as [`echo_child_command`].
#[must_use]
pub fn echo_child_command_with_composer(reflect_delay_ms: u64) -> Vec<String> {
    let mut command = echo_child_command();
    command.extend(composer_arguments(reflect_delay_ms));
    command
}

/// [`ready_child_command`] in composer-draw mode: the fixture still prints
/// `marker` on startup, then behaves as
/// [`echo_child_command_with_composer`] describes.
///
/// # Panics
///
/// Panics under the same conditions as [`ready_child_command`].
#[must_use]
pub fn ready_child_command_with_composer(marker: &str, reflect_delay_ms: u64) -> Vec<String> {
    let mut command = ready_child_command(marker);
    command.extend(composer_arguments(reflect_delay_ms));
    command
}

/// [`submit_child_command`] in composer-draw mode: the fixture keeps every
/// behavior its five positional settings select and additionally paints its
/// unsubmitted input as a composer region, reflecting each write
/// `reflect_delay_ms` after it lands.
///
/// # Panics
///
/// Panics under the same conditions as [`submit_child_command`].
#[must_use]
pub fn submit_child_command_with_composer(
    marker: &str,
    discard_reads: u64,
    paste_guard_keys: u64,
    react_on_body: u8,
    reflect_delay_ms: u64,
) -> Vec<String> {
    let mut command = submit_child_command(marker, discard_reads, paste_guard_keys, react_on_body);
    command.extend(composer_arguments(reflect_delay_ms));
    command
}

/// [`submit_child_command_with_composer`] with the composer folded: the
/// fixture paints `placeholder` wherever a non-empty body would stand, so its
/// region stays observable while what was written into it cannot be read back
/// off the screen. An empty `placeholder` paints the body as written.
///
/// `placeholder` is the caller's own value rather than a shared constant:
/// unlike the composer triple, no detection definition holds it, so nothing
/// reading the screen may depend on what it says.
///
/// # Panics
///
/// Panics under the same conditions as [`submit_child_command`].
#[must_use]
pub fn submit_child_command_with_folded_composer(
    marker: &str,
    discard_reads: u64,
    paste_guard_keys: u64,
    react_on_body: u8,
    reflect_delay_ms: u64,
    placeholder: &str,
) -> Vec<String> {
    let mut command = submit_child_command_with_composer(
        marker,
        discard_reads,
        paste_guard_keys,
        react_on_body,
        reflect_delay_ms,
    );
    command.push(placeholder.to_string());
    command
}

/// [`ready_child_profile_source`] in composer-draw mode, with the same fixed
/// marker ([`crate::detection::CONTROL_READY_MARKER`]).
///
/// # Panics
///
/// Panics under the same conditions as [`ready_child_command`].
#[must_use]
pub fn ready_child_profile_source_with_composer(
    display_name: &str,
    reflect_delay_ms: u64,
) -> String {
    profile_source(
        display_name,
        &ready_child_command_with_composer(
            crate::detection::CONTROL_READY_MARKER,
            reflect_delay_ms,
        ),
    )
}

/// The TOML source of a profile whose `[command]` launches the `submit_child`
/// fixture in composer-draw mode (see [`submit_child_command_with_composer`]).
///
/// # Panics
///
/// Panics under the same conditions as [`submit_child_command`].
#[must_use]
pub fn submit_child_profile_source_with_composer(
    display_name: &str,
    marker: &str,
    discard_reads: u64,
    paste_guard_keys: u64,
    react_on_body: u8,
    reflect_delay_ms: u64,
) -> String {
    profile_source(
        display_name,
        &submit_child_command_with_composer(
            marker,
            discard_reads,
            paste_guard_keys,
            react_on_body,
            reflect_delay_ms,
        ),
    )
}

/// [`submit_child_command_with_composer`] with `clear_scope` appended after
/// the (always-empty, "no folding") `fold_placeholder` slot: selects what the
/// fixture's `clear` byte (`0x15`) does, or leaves it at its ordinary
/// behavior when `clear_scope` is empty. See the fixture's module doc for the
/// named values (`"hold-until-clear"`, `"literal-clear"`, `"partial-clear"`).
///
/// # Panics
///
/// Panics under the same conditions as [`submit_child_command`].
#[must_use]
pub fn submit_child_command_with_clear_scope(
    marker: &str,
    discard_reads: u64,
    paste_guard_keys: u64,
    react_on_body: u8,
    reflect_delay_ms: u64,
    clear_scope: &str,
) -> Vec<String> {
    let mut command = submit_child_command_with_composer(
        marker,
        discard_reads,
        paste_guard_keys,
        react_on_body,
        reflect_delay_ms,
    );
    command.push(String::new()); // fold_placeholder: none
    command.push(clear_scope.to_string());
    command
}

/// The TOML source of a profile whose `[command]` launches the `argv_probe`
/// fixture with no arguments of its own, followed verbatim by
/// `extra_tables`: the `[model]` / `[effort]` TOML a test wants the profile
/// to map, so the elements a launch appends show up in the probe's output.
///
/// # Panics
///
/// Panics under the same conditions as [`argv_probe_path`], or if that path
/// is not valid UTF-8.
#[must_use]
pub fn argv_probe_profile_source(display_name: &str, extra_tables: &str) -> String {
    let program = argv_probe_path()
        .to_str()
        .expect("argv_probe path must be valid UTF-8")
        .to_string();
    format!("{}{extra_tables}", profile_source(display_name, &[program]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_test_binary_in_deps_finds_the_fixtures_one_level_up() {
        let profile = Path::new("target").join("debug");

        assert_eq!(
            fixture_bin_dir(&profile.join("deps").join("ipc_control-0123")),
            profile
        );
    }

    #[test]
    fn a_binary_outside_deps_finds_the_fixtures_beside_it() {
        let profile = Path::new("target").join("debug");

        assert_eq!(fixture_bin_dir(&profile.join("yagura-daemon")), profile);
    }
}
