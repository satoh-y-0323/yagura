#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: a window's child finds `yagura-daemon` by its bare name.
//!
//! A CLI's hook runs in the CLI's own shell, so it can only name the daemon's executable
//! the same way on every shell if `PATH` resolves it. The daemon therefore keeps a
//! folder of its own under the instance's state root (`bin/`) holding one link or copy of
//! its executable and puts only that folder at the front of each child's `PATH`; the
//! installation folder itself is never exposed (it holds an uninstaller).
//!
//! Every property is measured through a real daemon process
//! ([`common::real_daemon`]) and a real child (`env_probe`).

mod common;

use std::path::{Path, PathBuf};

use common::real_daemon::{RunningDaemon, Sandbox, daemon_base_dirs, daemon_bin};
use yagura_daemon::config::{REPORT_EXE_NAME, prepare_report_dir};
use yagura_test_support::paths::TempDir;

// --- fixtures --------------------------------------------------------------

const EXE_NAME: &str = if cfg!(windows) {
    "yagura-daemon.exe"
} else {
    "yagura-daemon"
};

fn profile_path_value() -> &'static str {
    if cfg!(windows) {
        r"C:\yprofile"
    } else {
        "/yprofile"
    }
}

impl Sandbox {
    fn write_path_probe_profile(&self, id: &str, env_table: &str) {
        self.write_var_probe_profile(id, "PATH", env_table);
    }
}

impl RunningDaemon {
    /// The `PATH` the profile's `env_probe` child reports, split into entries.
    async fn probed_path(&mut self, sandbox: &Sandbox, profile: &str) -> Vec<PathBuf> {
        let window_id = self.open_profile_window(sandbox, profile).await;
        let text = self
            .output_until(&window_id, "the PATH report", |text| {
                path_report(text).is_some()
            })
            .await;
        let value = path_report(&text).expect("the report was just seen");
        std::env::split_paths(&value).collect()
    }
}

/// The value between `<<PATH=` and `>>` once the whole report has arrived.
fn path_report(output: &str) -> Option<String> {
    let flat: String = output
        .chars()
        .filter(|c| !matches!(c, '\r' | '\n'))
        .collect();
    let start = flat.rfind("<<PATH=")? + "<<PATH=".len();
    let end = flat[start..].find(">>")? + start;
    Some(flat[start..end].to_string())
}

/// Windows paths compare without regard to case or a trailing separator.
fn same_path(a: &Path, b: &Path) -> bool {
    let normalize = |path: &Path| {
        let text = path.to_string_lossy().into_owned();
        let text = text.trim_end_matches(['\\', '/']).to_string();
        if cfg!(windows) {
            text.to_ascii_lowercase().replace('/', "\\")
        } else {
            text
        }
    };
    normalize(a) == normalize(b)
}

fn non_empty(entries: Vec<PathBuf>) -> Vec<PathBuf> {
    entries
        .into_iter()
        .filter(|entry| !entry.as_os_str().is_empty())
        .collect()
}

/// The `PATH` a child receives when the report folder cannot be made (a regular file
/// occupies its place), i.e. what children received before the folder existed. On Windows
/// that is not the daemon's own `PATH`, so it has to be observed. The blocking file is
/// left in place and the daemon is stopped before this returns.
async fn original_child_path(sandbox: &Sandbox) -> Vec<PathBuf> {
    std::fs::create_dir_all(sandbox.state_root()).unwrap();
    std::fs::write(sandbox.report_dir(), b"in the way").unwrap();
    let mut daemon = sandbox.start().await;
    non_empty(daemon.probed_path(sandbox, "probe").await)
}

fn assert_paths(actual: &[PathBuf], expected: &[PathBuf], what: &str) {
    let matches =
        actual.len() == expected.len() && actual.iter().zip(expected).all(|(a, e)| same_path(a, e));
    assert!(matches, "{what}: expected {expected:?}, got {actual:?}");
}

// =============================================================================
// The child's PATH leads with the report folder and keeps everything else
// =============================================================================

#[tokio::test]
async fn child_path_leads_with_the_report_folder_and_keeps_the_original_entries_in_order() {
    let sandbox = Sandbox::new("child-path-leads");
    sandbox.write_path_probe_profile("probe", "");
    let original = original_child_path(&sandbox).await;
    std::fs::remove_file(sandbox.report_dir()).unwrap();
    let mut daemon = sandbox.start().await;

    let entries = daemon.probed_path(&sandbox, "probe").await;

    let mut expected = vec![sandbox.report_dir()];
    expected.extend(original);
    assert_paths(
        &non_empty(entries),
        &non_empty(expected),
        "a window's PATH must be the report folder followed by the entries the child          received before",
    );
}

#[tokio::test]
async fn profile_env_path_replaces_the_whole_child_path() {
    let sandbox = Sandbox::new("child-path-profile");
    sandbox.write_path_probe_profile(
        "probe",
        &format!("\n[env]\nPATH = '{}'\n", profile_path_value()),
    );
    let mut daemon = sandbox.start().await;

    let entries = daemon.probed_path(&sandbox, "probe").await;

    assert_paths(
        &entries,
        &[PathBuf::from(profile_path_value())],
        "a profile's own PATH is the last layer and must win over the report folder",
    );
}

// =============================================================================
// The report folder holds the daemon's executable and nothing else
// =============================================================================

#[tokio::test]
async fn report_folder_holds_exactly_the_daemon_executable() {
    let sandbox = Sandbox::new("report-folder-content");
    sandbox.write_path_probe_profile("probe", "");
    let mut daemon = sandbox.start().await;
    // A window has opened, so the folder was prepared before any child needed it.
    daemon.probed_path(&sandbox, "probe").await;

    let names: Vec<String> = std::fs::read_dir(sandbox.report_dir())
        .unwrap_or_else(|err| {
            panic!(
                "the report folder {:?} must exist: {err}",
                sandbox.report_dir()
            )
        })
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        names,
        vec![EXE_NAME.to_string()],
        "the report folder must hold only the daemon executable"
    );
    let linked = std::fs::read(sandbox.report_dir().join(EXE_NAME)).unwrap();
    let original = std::fs::read(daemon_bin()).unwrap();
    assert!(
        linked == original,
        "the executable in the report folder must be the running daemon's own ({} bytes \
         expected, {} found)",
        original.len(),
        linked.len()
    );
}

#[tokio::test]
async fn a_stale_executable_in_the_report_folder_is_replaced_at_startup() {
    let sandbox = Sandbox::new("report-folder-stale");
    sandbox.write_path_probe_profile("probe", "");
    std::fs::create_dir_all(sandbox.report_dir()).unwrap();
    std::fs::write(
        sandbox.report_dir().join(EXE_NAME),
        b"left by an older version",
    )
    .unwrap();
    let mut daemon = sandbox.start().await;
    daemon.probed_path(&sandbox, "probe").await;

    let linked = std::fs::read(sandbox.report_dir().join(EXE_NAME)).unwrap();
    let original = std::fs::read(daemon_bin()).unwrap();
    assert!(
        linked == original,
        "an executable left by an earlier start must be replaced by the current one \
         ({} bytes expected, {} found)",
        original.len(),
        linked.len()
    );
}

/// A state root with an empty report folder in it and a stand-in executable beside it.
fn report_fixture(tag: &str) -> (TempDir, PathBuf, PathBuf, PathBuf) {
    let tree = TempDir::new("yagura-report-dir", tag);
    let state = tree.make_dir("state");
    let dir = state.join("bin");
    std::fs::create_dir_all(&dir).unwrap();
    let exe = tree.path().join("daemon-image");
    std::fs::write(&exe, b"the running daemon's image").unwrap();
    (tree, state, dir, exe)
}

#[test]
fn a_staging_file_an_interrupted_start_left_is_removed() {
    let (_tree, state, dir, exe) = report_fixture("stale-staging");
    std::fs::write(
        dir.join(format!(".{REPORT_EXE_NAME}.tmp-1")),
        b"half-written",
    )
    .unwrap();

    prepare_report_dir(&state, &dir, &exe).unwrap();

    let names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, vec![REPORT_EXE_NAME.to_string()]);
}

#[test]
fn a_report_executable_that_cannot_be_replaced_and_is_not_the_daemon_is_an_error() {
    let (_tree, state, dir, exe) = report_fixture("blocked-target");
    std::fs::create_dir(dir.join(REPORT_EXE_NAME)).unwrap();

    assert!(prepare_report_dir(&state, &dir, &exe).is_err());
}

// A running image refuses a rename over it; holding the file open without delete
// sharing reproduces that refusal.
#[cfg(windows)]
mod in_use {
    use std::os::windows::fs::OpenOptionsExt;

    use yagura_daemon::config::{REPORT_EXE_NAME, prepare_report_dir};

    use super::report_fixture;

    const FILE_SHARE_READ: u32 = 0x0000_0001;

    fn hold_open(path: &std::path::Path) -> std::fs::File {
        std::fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ)
            .open(path)
            .unwrap()
    }

    #[test]
    fn an_executable_in_use_that_is_already_the_daemon_is_kept() {
        let (_tree, state, dir, exe) = report_fixture("in-use-same");
        let target = dir.join(REPORT_EXE_NAME);
        std::fs::copy(&exe, &target).unwrap();
        let _held = hold_open(&target);

        assert_eq!(prepare_report_dir(&state, &dir, &exe), Ok(()));
        assert_eq!(
            std::fs::read(&target).unwrap(),
            std::fs::read(&exe).unwrap()
        );
    }

    #[test]
    fn an_executable_in_use_that_differs_from_the_daemon_is_an_error() {
        let (_tree, state, dir, exe) = report_fixture("in-use-other");
        let target = dir.join(REPORT_EXE_NAME);
        std::fs::write(&target, b"an older daemon").unwrap();
        let _held = hold_open(&target);

        assert!(prepare_report_dir(&state, &dir, &exe).is_err());
    }
}

// =============================================================================
// When the folder cannot be made the window still opens, PATH untouched
// =============================================================================

#[tokio::test]
async fn window_opens_without_the_report_folder_in_path_when_it_cannot_be_made() {
    let sandbox = Sandbox::new("report-folder-blocked");
    sandbox.write_path_probe_profile("probe", "");

    let entries = original_child_path(&sandbox).await;

    assert!(
        !entries
            .iter()
            .any(|entry| entry.starts_with(sandbox.state_root())),
        "no entry may point into the state root when the report folder does not exist; got          {entries:?}"
    );
    if cfg!(windows) {
        let system32 = Path::new(&std::env::var_os("SystemRoot").unwrap()).join("System32");
        assert!(
            entries.iter().any(|entry| same_path(entry, &system32)),
            "the child must still receive its usual PATH; got {entries:?}"
        );
    } else {
        assert_paths(
            &entries,
            &daemon_base_dirs(),
            "the child keeps the daemon's PATH",
        );
    }
}

// =============================================================================
// Windows environment names ignore case: PATH must not appear twice
// =============================================================================

#[cfg(windows)]
#[tokio::test]
async fn windows_child_environment_has_a_single_path_variable() {
    let sandbox = Sandbox::new("child-path-single");
    let cmd = common::real_daemon::shell_path();
    sandbox.write_probe_profile("dump", &[cmd.to_str().unwrap(), "/c", "set"], "");
    let mut daemon = sandbox.start().await;
    let window_id = daemon.open_profile_window(&sandbox, "dump").await;

    let text = daemon
        .output_until(&window_id, "the environment listing", |text| {
            text.to_ascii_lowercase().contains("windir=")
        })
        .await;

    let path_variables = text
        .lines()
        .filter(|line| {
            line.split_once('=')
                .is_some_and(|(name, _)| name.trim().eq_ignore_ascii_case("path"))
        })
        .count();
    assert_eq!(
        path_variables, 1,
        "the child must see exactly one PATH variable whatever its letter case; listing: {text}"
    );
}

// =============================================================================
// A window that reports through hooks does not let cmd run a program from its cwd
// =============================================================================

#[cfg(windows)]
mod cwd_search {
    use super::*;

    const VAR: &str = "NoDefaultCurrentDirectoryInExePath";

    #[tokio::test]
    async fn hooks_windows_turn_the_cwd_program_search_off_and_plain_windows_are_left_alone() {
        let sandbox = Sandbox::new("cwd-search");
        sandbox.write_var_probe_profile(
            "with-hooks",
            VAR,
            "
[hooks]
args = [\"--settings\", '{\"hooks\":{}}']
",
        );
        sandbox.write_var_probe_profile("plain", VAR, "");
        let mut daemon = sandbox.start().await;

        assert_eq!(
            daemon
                .probed_var(&sandbox, "with-hooks", VAR)
                .await
                .as_deref(),
            Some("1"),
            "a window that carries hooks must not search its cwd for programs"
        );
        assert_eq!(
            daemon.probed_var(&sandbox, "plain", VAR).await,
            None,
            "an ordinary shell window keeps the system's own search behaviour"
        );
    }
}

// =============================================================================
// A window's environment is refreshed from the registry, not the daemon's own
// =============================================================================

#[cfg(windows)]
#[tokio::test]
async fn windows_child_environment_takes_the_registrys_variables_over_a_stale_daemon_environment() {
    let sandbox = Sandbox::new("child-env-registry");
    // `OS` is stored as a plain string and `windir` as one to expand, and the machine
    // key's own `USERNAME` (SYSTEM) must not replace the user's.
    let names = ["OS", "windir", "USERNAME"];
    for name in names {
        sandbox.write_var_probe_profile(&name.to_ascii_lowercase(), name, "");
    }
    sandbox.write_path_probe_profile("path", "");
    let mut daemon = sandbox
        .start_from(&[("OS", "stale-os"), ("windir", "stale-windir")])
        .await;

    for name in names {
        let seen = daemon
            .probed_var(&sandbox, &name.to_ascii_lowercase(), name)
            .await
            .unwrap_or_default();
        let own = std::env::var(name).expect("the test process's own session sets it");
        assert!(
            seen.eq_ignore_ascii_case(&own),
            "a window's {name} must be the session's {own:?}, got {seen:?}"
        );
    }
    let system32 =
        Path::new(&std::env::var_os("SystemRoot").expect("SystemRoot is set")).join("System32");
    let path = daemon.probed_path(&sandbox, "path").await;
    assert!(
        path.iter().any(|entry| same_path(entry, &system32)),
        "a window's PATH must carry the machine PATH's {} although the daemon's lacks it: \
         {path:?}",
        system32.display()
    );
}
