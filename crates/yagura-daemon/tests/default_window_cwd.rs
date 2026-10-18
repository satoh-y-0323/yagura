#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: the daemon's start-up default for a window whose
//! `cwd` is omitted -- the pure rule (`resolve_default_window_cwd`) and its
//! wiring into `DaemonConfig::default_window_cwd` / the `cwd: None` branch
//! `WindowRegistry::open` resolves through.
//!
//! The default is the user's home directory whatever the host OS and however
//! the daemon was started; only a home that is missing or not an existing
//! directory defers to the daemon's own current directory (`None`).
//!
//! `DaemonConfig::default_window_cwd` itself defaults to `None`, so every
//! existing `..DaemonConfig::default()` literal -- including
//! `registry_cwd_omitted_defaults_to_daemon_current_directory` in
//! `tests/window_registry.rs` -- keeps resolving to the daemon's current
//! directory unless a caller sets the field explicitly.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use yagura_daemon::DaemonConfig;
use yagura_daemon::config::resolve_default_window_cwd;
use yagura_daemon::window::{OpenWindowRequest, RequestedCwd, WindowRegistry};
use yagura_test_support::paths::{strip_extended_length_prefix, unique_temp_dir};
use yagura_test_support::process::echo_child_path;

fn exists_paths_among(paths: Vec<&Path>) -> impl Fn(&Path) -> bool + use<> {
    let owned: Vec<PathBuf> = paths.into_iter().map(Path::to_path_buf).collect();
    move |candidate: &Path| owned.iter().any(|known| known == candidate)
}

fn echo_request() -> OpenWindowRequest {
    let child = echo_child_path();
    OpenWindowRequest {
        command: vec![
            child
                .to_str()
                .expect("echo_child path must be valid UTF-8")
                .to_string(),
        ],
        cwd: None,
        name: None,
        vendor: None,
        detection: None,
        on_name_taken: Default::default(),
    }
}

// =====================================================================
// resolve_default_window_cwd: the pure rule
// =====================================================================

#[test]
fn the_home_directory_is_the_default_cwd_when_it_is_an_existing_directory() {
    let home = PathBuf::from("home-that-exists");
    let exists = exists_paths_among(vec![&home]);

    let result = resolve_default_window_cwd(Some(&home), &exists);

    assert_eq!(result, Some(home));
}

#[test]
fn a_home_that_is_not_an_existing_directory_defers_to_the_daemon_cwd() {
    let home = PathBuf::from("home-that-does-not-exist");
    let exists = exists_paths_among(vec![]);

    let result = resolve_default_window_cwd(Some(&home), &exists);

    assert_eq!(result, None);
}

#[test]
fn an_empty_home_defers_to_the_daemon_cwd() {
    let empty = PathBuf::new();
    let exists = |_: &Path| true;

    let result = resolve_default_window_cwd(Some(&empty), &exists);

    assert_eq!(result, None);
}

#[test]
fn no_home_defers_to_the_daemon_cwd() {
    let exists = |_: &Path| true;

    let result = resolve_default_window_cwd(None, &exists);

    assert_eq!(result, None);
}

// =====================================================================
// DaemonConfig / WindowRegistry: the `cwd: None` branch wired to the
// configured default
// =====================================================================

#[test]
fn default_window_cwd_field_is_none_on_daemonconfig_default_so_the_daemon_cwd_still_wins() {
    assert_eq!(DaemonConfig::default().default_window_cwd, None);
}

#[tokio::test]
async fn open_window_without_a_cwd_uses_the_configured_default_when_one_is_set() {
    let default_dir = unique_temp_dir("default-window-cwd", "default");
    std::fs::create_dir_all(&default_dir).expect("create the default cwd directory");
    let expected = strip_extended_length_prefix(
        &default_dir
            .canonicalize()
            .expect("default cwd directory must canonicalize"),
    )
    .to_string_lossy()
    .to_string();

    let config = DaemonConfig {
        default_window_cwd: Some(default_dir),
        ..DaemonConfig::default()
    };
    let registry = WindowRegistry::new(config);

    let id = registry
        .open(echo_request())
        .await
        .expect("open without cwd should succeed")
        .window_id;
    let list = registry.list();
    let info = list
        .iter()
        .find(|w| w.window_id == id)
        .expect("window must be present in list");

    assert_eq!(
        info.cwd, expected,
        "cwd must default to the configured default_window_cwd when the caller omits one"
    );

    registry.terminate_all().await;
}

// =====================================================================
// The child itself starts in the window's cwd
// =====================================================================

/// A command whose child writes the directory it is running in to `report`.
fn cwd_report_command(report: &Path) -> Vec<String> {
    let report = report.to_str().expect("report path must be valid UTF-8");
    if cfg!(windows) {
        let root = std::env::var_os("SystemRoot").unwrap_or_else(|| OsString::from("C:/Windows"));
        let cmd = Path::new(&root).join("System32").join("cmd.exe");
        vec![
            cmd.to_str()
                .expect("cmd path must be valid UTF-8")
                .to_string(),
            "/d".to_string(),
            "/c".to_string(),
            format!("cd>{report}"),
        ]
    } else {
        vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "pwd -P > \"$1\"".to_string(),
            "sh".to_string(),
            report.to_string(),
        ]
    }
}

fn canonical(path: &Path) -> PathBuf {
    strip_extended_length_prefix(
        &path
            .canonicalize()
            .unwrap_or_else(|e| panic!("{} must canonicalize: {e}", path.display())),
    )
}

#[tokio::test]
async fn a_window_child_runs_in_the_cwd_the_window_was_opened_with() {
    let cwd = unique_temp_dir("default-window-cwd", "child-cwd");
    std::fs::create_dir_all(&cwd).expect("create the window's cwd");
    let report_dir = unique_temp_dir("default-window-cwd", "child-cwd-report");
    std::fs::create_dir_all(&report_dir).expect("create the report directory");
    let report = report_dir.join("cwd.txt");
    let registry = WindowRegistry::new(DaemonConfig::default());

    registry
        .open(OpenWindowRequest {
            cwd: Some(RequestedCwd::from_caller(cwd.clone())),
            command: cwd_report_command(&report),
            ..echo_request()
        })
        .await
        .expect("open with an explicit cwd should succeed");

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
    let reported = loop {
        let text = std::fs::read_to_string(&report).unwrap_or_default();
        if text.ends_with('\n') {
            break text.trim().to_string();
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the window's child never reported its cwd"
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    };
    registry.terminate_all().await;

    let reported = canonical(Path::new(&reported));
    let expected = canonical(&cwd);
    let same = if cfg!(windows) {
        reported
            .to_string_lossy()
            .eq_ignore_ascii_case(&expected.to_string_lossy())
    } else {
        reported == expected
    };
    assert!(
        same,
        "the child must run in {}, but it ran in {}",
        expected.display(),
        reported.display()
    );
}
