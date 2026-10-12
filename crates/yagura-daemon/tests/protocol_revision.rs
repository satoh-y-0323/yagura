#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The daemon's version-probe endpoint (`get_protocol_revision` /
//! `protocol_revision`): a real `IpcServer` is driven over a real endpoint,
//! never a mock, so what is asserted is what a peer actually receives.
//!
//! Four properties are measured here:
//!
//! - The daemon answers with the wire revision this build implements.
//! - The answer comes back from a connection that never sent `identify` or
//!   `register_approver` -- the probe carries no permission gate.
//! - The connection stays open and usable after the probe responds.
//! - The three revision constants this build exposes sit in the relative
//!   order the probe's contract depends on, checked as a relation rather than
//!   against a pinned literal.
//!
//! A fifth property -- that an unrecognized `type` still leaves the
//! connection open for a following request -- is not re-tested here: `
//! unknown_type_continues_the_connection` in `tests/ipc_server.rs` already
//! drives exactly that sequence against a real `IpcServer`, and this file's
//! probe does not change that path.

mod common;

use std::time::Duration;

use common::{Harness, read_response, send};
use yagura_daemon::DaemonConfig;
use yagura_protocol::{ClientMessage, DaemonMessage};

#[tokio::test]
async fn probe_reports_this_builds_own_wire_revision() {
    let harness = Harness::start("protocol-revision-value", DaemonConfig::default()).await;
    let mut client = harness.connect().await;

    send(&mut client, &ClientMessage::GetProtocolRevision).await;
    match read_response(&mut client, Duration::from_secs(5)).await {
        DaemonMessage::ProtocolRevision { revision } => {
            assert_eq!(revision, yagura_protocol::PROTOCOL_REVISION);
        }
        other => panic!("expected protocol_revision, got {other:?}"),
    }
}

#[tokio::test]
async fn probe_answers_every_connection_with_no_identification_or_approver_registration() {
    let harness = Harness::start("protocol-revision-any-connection", DaemonConfig::default()).await;

    // Two independent, freshly connected clients. Neither ever sends
    // `identify` or `register_approver` before probing.
    let mut first = harness.connect().await;
    let mut second = harness.connect().await;

    for client in [&mut first, &mut second] {
        send(client, &ClientMessage::GetProtocolRevision).await;
        match read_response(client, Duration::from_secs(5)).await {
            DaemonMessage::ProtocolRevision { .. } => {}
            other => panic!(
                "expected protocol_revision from an unidentified, unregistered connection, got {other:?}"
            ),
        }
    }
}

#[tokio::test]
async fn connection_stays_usable_after_the_probe_responds() {
    let harness = Harness::start("protocol-revision-survives", DaemonConfig::default()).await;
    let mut client = harness.connect().await;

    send(&mut client, &ClientMessage::GetProtocolRevision).await;
    match read_response(&mut client, Duration::from_secs(5)).await {
        DaemonMessage::ProtocolRevision { .. } => {}
        other => panic!("expected protocol_revision, got {other:?}"),
    }

    send(&mut client, &ClientMessage::Ping).await;
    match read_response(&mut client, Duration::from_secs(5)).await {
        DaemonMessage::Pong => {}
        other => panic!("expected pong on the same connection after the probe, got {other:?}"),
    }
}

// The relation being measured holds between three constants, so clippy sees
// assertions whose value is decided at compile time. That is what this test is
// for: it fails the build's own constants, not a runtime value.
#[allow(clippy::assertions_on_constants)]
#[test]
fn revision_relationship_holds_without_pinning_a_literal() {
    assert!(
        yagura_protocol::PROTOCOL_REVISION_BASELINE < yagura_protocol::LAUNCH_OPTIONS_MIN_REVISION,
        "the launch-option threshold must sit strictly above the baseline revision"
    );
    assert!(
        yagura_protocol::LAUNCH_OPTIONS_MIN_REVISION <= yagura_protocol::PROTOCOL_REVISION,
        "this build must implement at least the wire revision the launch-option threshold names"
    );
}
