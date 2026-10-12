#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! `get_daemon_health` against a real `IpcServer` over a real endpoint.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{Harness, read_response, send, unique_pipe_name};
use yagura_daemon::{DaemonConfig, IpcServer, WindowRegistry};
use yagura_protocol::{ClientMessage, DaemonMessage, DaemonProblemKind};

#[tokio::test]
async fn a_healthy_daemon_reports_no_problems_to_an_unidentified_connection() {
    let harness = Harness::start("daemon-health-healthy", DaemonConfig::default()).await;
    let mut client = harness.connect().await;

    send(&mut client, &ClientMessage::GetDaemonHealth).await;
    match read_response(&mut client, Duration::from_secs(5)).await {
        DaemonMessage::DaemonHealth { problems } => assert!(problems.is_empty(), "{problems:?}"),
        other => panic!("expected daemon_health, got {other:?}"),
    }

    send(&mut client, &ClientMessage::Ping).await;
    assert!(matches!(
        read_response(&mut client, Duration::from_secs(5)).await,
        DaemonMessage::Pong
    ));
}

#[tokio::test]
async fn a_recorded_initial_window_failure_is_reported_with_its_reason() {
    let config = DaemonConfig::default();
    let registry = Arc::new(WindowRegistry::new(config.clone()));
    let endpoint = unique_pipe_name("daemon-health-initial-failure");
    let server = Arc::new(
        IpcServer::new(registry, endpoint.clone(), config)
            .with_initial_window_failure("spawn refused"),
    );
    let _task = tokio::spawn(async move {
        let _ = server.run().await;
    });
    let mut client =
        yagura_test_support::ipc::connect_with_retry(&endpoint, Duration::from_secs(5)).await;

    send(&mut client, &ClientMessage::GetDaemonHealth).await;
    match read_response(&mut client, Duration::from_secs(5)).await {
        DaemonMessage::DaemonHealth { problems } => {
            assert_eq!(problems.len(), 1, "{problems:?}");
            assert_eq!(problems[0].kind, DaemonProblemKind::InitialWindowFailed);
            assert_eq!(problems[0].detail, "spawn refused");
        }
        other => panic!("expected daemon_health, got {other:?}"),
    }
}
