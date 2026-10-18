#![allow(missing_docs, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use yagura_daemon::window::RejectedArgument;
use yagura_daemon::{DaemonConfig, WindowRegistry};
use yagura_test_support::process::echo_child_path;

/// Caller sends nonexistent cwd: argument is named.
#[tokio::test]
async fn rejected_argument_wire_identifies_caller_cwd_when_missing() {
    let registry = WindowRegistry::new(DaemonConfig::default());
    let missing_cwd = common::guaranteed_nonexistent_exe_path();

    let result = registry
        .open(yagura_daemon::OpenWindowRequest {
            command: vec![
                echo_child_path()
                    .to_str()
                    .expect("echo_child path must be valid UTF-8")
                    .to_string(),
            ],
            cwd: Some(yagura_daemon::RequestedCwd::from_caller(missing_cwd)),
            name: None,
            vendor: None,
            detection: None,
            on_name_taken: Default::default(),
        })
        .await;

    match result {
        Err(yagura_daemon::WindowOpenError::InvalidArgument { argument, .. }) => {
            assert_eq!(
                argument,
                Some(RejectedArgument::Cwd),
                "caller-sent cwd should be named when it does not exist"
            );
        }
        other => panic!("expected InvalidArgument with cwd argument, got {other:?}"),
    }
}

/// Caller sends cwd that is not a directory: argument is named.
#[tokio::test]
async fn rejected_argument_wire_identifies_caller_cwd_when_not_directory() {
    let registry = WindowRegistry::new(DaemonConfig::default());
    let not_a_dir = echo_child_path();

    let result = registry
        .open(yagura_daemon::OpenWindowRequest {
            command: vec![
                echo_child_path()
                    .to_str()
                    .expect("echo_child path must be valid UTF-8")
                    .to_string(),
            ],
            cwd: Some(yagura_daemon::RequestedCwd::from_caller(not_a_dir)),
            name: None,
            vendor: None,
            detection: None,
            on_name_taken: Default::default(),
        })
        .await;

    match result {
        Err(yagura_daemon::WindowOpenError::InvalidArgument { argument, .. }) => {
            assert_eq!(
                argument,
                Some(RejectedArgument::Cwd),
                "caller-sent cwd should be named when it is not a directory"
            );
        }
        other => panic!("expected InvalidArgument with cwd argument, got {other:?}"),
    }
}

/// Empty command is rejected but does not name cwd.
#[tokio::test]
async fn rejected_argument_wire_omits_cwd_for_empty_command() {
    let registry = WindowRegistry::new(DaemonConfig::default());

    let result = registry
        .open(yagura_daemon::OpenWindowRequest {
            command: vec![],
            cwd: None,
            name: None,
            vendor: None,
            detection: None,
            on_name_taken: Default::default(),
        })
        .await;

    match result {
        Err(yagura_daemon::WindowOpenError::InvalidArgument { argument, .. }) => {
            assert_eq!(
                argument, None,
                "empty command should not name cwd as the rejected argument"
            );
        }
        other => panic!("expected InvalidArgument without cwd argument, got {other:?}"),
    }
}

/// Reason text contains the path.
#[tokio::test]
async fn rejected_argument_wire_includes_path_in_reason() {
    let registry = WindowRegistry::new(DaemonConfig::default());
    let missing_cwd = common::guaranteed_nonexistent_exe_path();
    let missing_str = missing_cwd.to_string_lossy().to_string();

    let result = registry
        .open(yagura_daemon::OpenWindowRequest {
            command: vec![
                echo_child_path()
                    .to_str()
                    .expect("echo_child path must be valid UTF-8")
                    .to_string(),
            ],
            cwd: Some(yagura_daemon::RequestedCwd::from_caller(missing_cwd)),
            name: None,
            vendor: None,
            detection: None,
            on_name_taken: Default::default(),
        })
        .await;

    match result {
        Err(yagura_daemon::WindowOpenError::InvalidArgument { reason, .. }) => {
            assert!(
                reason.contains(&missing_str),
                "reason should contain the path, got: {reason}"
            );
        }
        other => panic!("expected InvalidArgument, got {other:?}"),
    }
}

/// wire_name() returns the correct name.
#[test]
fn rejected_argument_wire_name_is_correct() {
    assert_eq!(RejectedArgument::Cwd.wire_name(), "cwd");
}
