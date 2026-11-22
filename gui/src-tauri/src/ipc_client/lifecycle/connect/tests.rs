//! The connect functions an attempt uses, against a real endpoint.

use yagura_os::PeerAuth;

use super::*;

const ENDPOINT_PREFIX: &str = "yagura-test-gui-ipc-client";

async fn listening(tag: &str) -> (PlatformIpc, <PlatformIpc as IpcTransport>::Listener, String) {
    let ipc = PlatformIpc::default();
    let endpoint = yagura_test_support::ipc::unique_endpoint_name(ENDPOINT_PREFIX, tag);
    let security = ipc.endpoint_security().expect("endpoint security");
    let listener = ipc.bind(&endpoint, security).await.expect("bind");
    (ipc, listener, endpoint)
}

#[tokio::test]
async fn every_connect_form_reaches_a_listening_endpoint() {
    let (ipc, mut listener, endpoint) = listening("connect-once").await;
    let (accepted, reached) = tokio::join!(ipc.accept(&mut listener), connect_once(&endpoint));
    assert!(accepted.is_ok() && reached.is_some(), "connect_once");

    let (ipc, mut listener, endpoint) = listening("connect-limited").await;
    let (accepted, reached) = tokio::join!(
        ipc.accept(&mut listener),
        connect_retrying_with_limit(&endpoint, 1)
    );
    assert!(
        accepted.is_ok() && reached.is_some(),
        "connect_retrying_with_limit"
    );

    let (ipc, mut listener, endpoint) = listening("connect-retrying").await;
    let (accepted, reached) = tokio::join!(ipc.accept(&mut listener), connect_retrying(&endpoint));
    assert!(accepted.is_ok() && reached.is_some(), "connect_retrying");
}

// The paused clock moves only when the retry loop sleeps, and only to that
// sleep's deadline, so the elapsed time counts the sleeps exactly: one
// interval between each pair of attempts and none before the first.
#[tokio::test(start_paused = true)]
async fn a_refused_endpoint_is_retried_an_interval_apart() {
    let endpoint = yagura_test_support::ipc::unique_endpoint_name(ENDPOINT_PREFIX, "nobody");

    let started = tokio::time::Instant::now();
    assert!(connect_retrying_with_limit(&endpoint, 1).await.is_none());
    assert_eq!(
        started.elapsed(),
        std::time::Duration::ZERO,
        "a single attempt waits"
    );

    let started = tokio::time::Instant::now();
    assert!(connect_retrying_with_limit(&endpoint, 3).await.is_none());
    assert_eq!(started.elapsed(), CONNECT_RETRY_INTERVAL * 2);
}
