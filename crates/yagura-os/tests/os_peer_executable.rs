#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: `PeerAuth::peer_executable`, the peer-executable-path
//! probe the daemon uses to verify an approver's registration.
//!
//! cfg-free by design: `PlatformIpc` resolves to the platform under
//! compilation, so this file exercises whichever implementation that is
//! without a `#[cfg(...)]` split of its own. The accept/connect shape is
//! the same one `os_unix.rs::peer_id_resolves_the_connecting_uid` uses;
//! `os_security.rs` is not a usable template here because it is
//! `#![cfg(windows)]` and never establishes a connection.

use yagura_os::{IpcTransport, PeerAuth, PlatformIpc};
use yagura_test_support::ipc::unique_endpoint_name;
use yagura_test_support::process::own_canonical_exe;

/// Endpoint-name prefix for this file, kept distinct from the daemon's own so
/// an endpoint created here can never be mistaken for a resident daemon's.
const ENDPOINT_PREFIX: &str = "yagura-test-peer-executable";

#[tokio::test]
async fn peer_executable_resolves_to_the_connecting_process_canonicalized() {
    let ipc = PlatformIpc::default();
    let security = ipc
        .endpoint_security()
        .expect("endpoint_security should succeed");
    let endpoint = unique_endpoint_name(ENDPOINT_PREFIX, "canonicalized");
    let mut listener = ipc
        .bind(&endpoint, security)
        .await
        .expect("bind should succeed");

    let (accepted, connected) = tokio::join!(ipc.accept(&mut listener), ipc.connect(&endpoint));
    let accepted = accepted.expect("accept should succeed");
    let _connected = connected.expect("connect should succeed");

    let raw = ipc
        .peer_executable(&accepted)
        .expect("peer_executable should resolve for a same-process connecting peer");
    let actual = raw
        .canonicalize()
        .unwrap_or_else(|err| panic!("canonicalize peer_executable path {raw:?}: {err}"));
    let expected = own_canonical_exe();

    // Raw equality does not hold on Windows: `canonicalize` prefixes `\\?\`
    // while `QueryFullProcessImageNameW` does not, so both sides are
    // canonicalized before comparison.
    assert_eq!(
        actual, expected,
        "peer_executable must resolve to this test binary's own executable path"
    );
}

#[tokio::test]
async fn peer_executable_raw_value_is_an_existing_absolute_path() {
    let ipc = PlatformIpc::default();
    let security = ipc
        .endpoint_security()
        .expect("endpoint_security should succeed");
    let endpoint = unique_endpoint_name(ENDPOINT_PREFIX, "raw-absolute");
    let mut listener = ipc
        .bind(&endpoint, security)
        .await
        .expect("bind should succeed");

    let (accepted, connected) = tokio::join!(ipc.accept(&mut listener), ipc.connect(&endpoint));
    let accepted = accepted.expect("accept should succeed");
    let _connected = connected.expect("connect should succeed");

    let raw = ipc
        .peer_executable(&accepted)
        .expect("peer_executable should resolve for a same-process connecting peer");
    assert!(
        raw.is_absolute(),
        "peer_executable's raw (pre-canonicalize) value must be an absolute path, got {raw:?}"
    );
    assert!(
        raw.exists(),
        "peer_executable's raw (pre-canonicalize) value must name an existing file, got {raw:?}"
    );
}
