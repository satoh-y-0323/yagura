#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: the single "is this window alive" predicate the
//! registry exposes (`WindowRegistry::liveness`), covering a window with a
//! running child, a window whose child exited on its own while its entry
//! stays registered, and an id the registry never opened.
//!
//! Children are echo_child or a fixed self-exiting command, never a real
//! vendor CLI.

use std::path::PathBuf;
use std::time::Duration;

use tokio::time::sleep;
use yagura_test_support::paths::{strip_extended_length_prefix, unique_temp_dir};
use yagura_test_support::process::{echo_child_path, exit_with_code_command};

use yagura_daemon::DaemonConfig;
use yagura_daemon::window::{OpenWindowRequest, RequestedCwd, WindowLiveness, WindowRegistry};
use yagura_protocol::WindowStatus;

/// The minimal `OpenWindowRequest` most tests need: spawn echo_child with all
/// optional fields omitted.
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
    }
}

/// [`echo_request`], opened against `cwd` instead of the daemon's own.
fn echo_request_in(cwd: PathBuf) -> OpenWindowRequest {
    OpenWindowRequest {
        cwd: Some(RequestedCwd::from_caller(cwd)),
        ..echo_request()
    }
}

/// A request whose direct child exits immediately on its own, with no
/// explicit termination ever sent to it: the fixture behind "the child
/// exited by itself" as opposed to a caller-initiated close.
fn self_exiting_request() -> OpenWindowRequest {
    OpenWindowRequest {
        command: exit_with_code_command("0"),
        cwd: None,
        name: None,
        vendor: None,
        detection: None,
    }
}

/// Polls a registry-held entry until it reports `Exited`; panics on timeout.
/// Deterministic polling instead of a fixed sleep keeps this robust on slow
/// machines.
async fn wait_for_exited(registry: &WindowRegistry, window_id: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let entry = registry
            .get(window_id)
            .expect("window must still be registered while waiting for it to exit");
        if entry.current_status() == WindowStatus::Exited {
            return;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("timed out waiting for the window to become Exited");
        }
        sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn a_window_with_a_running_child_is_alive_and_carries_its_open_time_cwd() {
    let registry = WindowRegistry::new(DaemonConfig::default());
    let dir = unique_temp_dir("yagura-test-window-liveness", "alive");
    let expected_cwd =
        strip_extended_length_prefix(&dir.canonicalize().expect("temp dir must canonicalize"))
            .to_string_lossy()
            .into_owned();

    let window_id = registry
        .open(echo_request_in(dir))
        .await
        .expect("open should succeed")
        .window_id;

    match registry.liveness(&window_id) {
        WindowLiveness::Alive(entry) => {
            assert_eq!(
                entry.cwd(),
                expected_cwd,
                "a live window's entry must carry the cwd it was opened with"
            );
        }
        other => panic!("a window with a running child must be reported alive, got {other:?}"),
    }

    registry.terminate_all().await;
}

#[tokio::test]
async fn a_window_whose_child_exited_on_its_own_is_not_alive_but_stays_registered() {
    let registry = WindowRegistry::new(DaemonConfig::default());

    let window_id = registry
        .open(self_exiting_request())
        .await
        .expect("open should succeed")
        .window_id;
    wait_for_exited(&registry, &window_id).await;

    assert!(
        registry.get(&window_id).is_some(),
        "an Exited window's entry must stay registered: being in the registry and being alive \
         are different questions"
    );
    assert!(
        matches!(registry.liveness(&window_id), WindowLiveness::Dead),
        "an Exited window must not be reported alive even though it is still registered"
    );
}

#[tokio::test]
async fn a_window_id_that_was_never_opened_is_not_found() {
    let registry = WindowRegistry::new(DaemonConfig::default());

    assert!(
        matches!(
            registry.liveness("w999999-neveropened"),
            WindowLiveness::NotFound
        ),
        "an id this registry never opened must be reported not found, not merely not alive"
    );
}
