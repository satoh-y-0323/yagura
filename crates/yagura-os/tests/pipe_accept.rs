#![cfg(windows)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: a client that hangs up before the named-pipe listener
//! takes its connection up does not end the accept loop. The instance it
//! leaves behind is handed out as a connection that is already closed, and
//! the next client is still accepted.

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use yagura_os::platform_windows::WindowsIpc;
use yagura_os::{IpcTransport, PeerAuth};
use yagura_test_support::ipc::unique_endpoint_name;

/// Endpoint-name prefix for this file, kept distinct from the daemon's own so
/// a pipe created here can never be mistaken for a resident daemon's.
const PIPE_PREFIX: &str = "yagura-test-accept";

#[tokio::test]
async fn a_client_that_hangs_up_before_being_accepted_leaves_the_next_client_acceptable() {
    let ipc = WindowsIpc;
    let security = ipc
        .endpoint_security()
        .expect("endpoint_security should succeed");
    let name = unique_endpoint_name(PIPE_PREFIX, "hangs-up-first");
    let mut listener = ipc
        .bind(&name, security)
        .await
        .expect("bind should succeed");

    drop(
        ipc.connect(&name)
            .await
            .expect("the first client must reach the pending instance"),
    );
    let mut left_behind = ipc
        .accept(&mut listener)
        .await
        .expect("a client that hung up first must not end the accept loop");
    let mut byte = [0u8; 1];
    assert_eq!(
        left_behind
            .read(&mut byte)
            .await
            .expect("reading a closed instance reports its end"),
        0,
        "the instance the first client left behind must read as closed"
    );

    let (accepted, second) = tokio::join!(ipc.accept(&mut listener), ipc.connect(&name));
    let mut accepted = accepted.expect("the next client must be accepted");
    let mut second = second.expect("the next client must connect");
    second
        .write_all(b"x")
        .await
        .expect("the second client writes");
    accepted
        .read_exact(&mut byte)
        .await
        .expect("the accepted connection reads");
    assert_eq!(
        &byte, b"x",
        "the accepted connection must be the second client's"
    );
}

#[tokio::test]
async fn a_client_arriving_while_the_only_instance_is_taken_waits_for_the_next_one() {
    let ipc = WindowsIpc;
    let security = ipc
        .endpoint_security()
        .expect("endpoint_security should succeed");
    let name = unique_endpoint_name(PIPE_PREFIX, "all-taken");
    let mut listener = ipc
        .bind(&name, security)
        .await
        .expect("bind should succeed");
    // Takes the instance `bind` created; none is offered again until `accept` runs, as
    // while a daemon is still starting up.
    let _first = ipc
        .connect(&name)
        .await
        .expect("the first client must reach the pending instance");

    let (second, accepted) = tokio::join!(ipc.connect(&name), ipc.accept(&mut listener));
    accepted.expect("the first client must be accepted");
    second.expect("a client that finds every instance taken must wait for the next one");
}
