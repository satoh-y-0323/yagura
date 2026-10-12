#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! `get_daemon_health` against a real `IpcServer` over a real endpoint.

mod common;

use std::time::Duration;

use common::{Harness, read_response, send};
use yagura_daemon::DaemonConfig;
use yagura_protocol::{ClientMessage, DaemonMessage};

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
