#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The argument name the daemon may put on an error frame does not turn an
//! `invalid_argument` refusal into an `Internal` one on the bridge.

use std::sync::Mutex;

use yagura_mcp::tools::{OpenWindowArgs, open_window};
use yagura_mcp::transport::{DaemonTransport, RequesterIdentity, TransportError};
use yagura_protocol::{ClientMessage, DaemonMessage};

/// A transport that answers any request with one scripted response.
struct ScriptedTransport {
    response: Mutex<Option<Result<DaemonMessage, TransportError>>>,
}

impl ScriptedTransport {
    fn new(response: Result<DaemonMessage, TransportError>) -> Self {
        Self {
            response: Mutex::new(Some(response)),
        }
    }
}

impl DaemonTransport for ScriptedTransport {
    fn requester_identity(&self) -> RequesterIdentity {
        RequesterIdentity::External
    }

    async fn round_trip(&self, _msg: ClientMessage) -> Result<DaemonMessage, TransportError> {
        self.response
            .lock()
            .expect("scripted response mutex must not be poisoned")
            .take()
            .expect("round_trip must be called at most once")
    }
}

fn open_window_args(cwd: Option<&str>) -> OpenWindowArgs {
    OpenWindowArgs {
        command: vec!["probe".to_string()],
        cwd: cwd.map(str::to_string),
        name: None,
        vendor: None,
        detection: None,
        profile: None,
        model: None,
        effort: None,
    }
}

#[tokio::test]
async fn a_refusal_naming_an_argument_still_reaches_the_caller_as_invalid_argument() {
    let transport = ScriptedTransport::new(Ok(DaemonMessage::Error {
        code: "invalid_argument".to_string(),
        message: "cwd does not exist or cannot be resolved: C:\\nope".to_string(),
        rejected_argument: Some("cwd".to_string()),
    }));

    let err = open_window(&transport, open_window_args(Some("C:\\nope")))
        .await
        .expect_err("a refused open must not report success");

    assert_eq!(
        err.code(),
        "invalid_argument",
        "a named argument must not change the code the bridge reports"
    );
    assert!(
        err.to_string().contains("C:\\nope"),
        "the daemon's reason must still reach the caller, got: {err}"
    );
}

/// The same refusal through the real daemon and the real frame transport, so
/// the new wire key is decoded rather than scripted.
///
/// Windows-only for the same reason as the sibling `e2e.rs`: this directory's
/// real-daemon coverage is not run on other hosts.
#[cfg(windows)]
mod real_daemon {
    use std::sync::Arc;
    use std::time::Duration;

    use yagura_daemon::{DaemonConfig, IpcServer, WindowRegistry};
    use yagura_mcp::tools::{list_windows, open_window};
    use yagura_mcp::transport::EndpointTransport;
    use yagura_test_support::ipc::{reply_within, unique_endpoint_name};

    use super::open_window_args;

    const READY_TIMEOUT: Duration = Duration::from_secs(10);

    #[tokio::test]
    async fn a_nonexistent_cwd_is_invalid_argument_end_to_end() {
        let config = DaemonConfig {
            approval_policy: Some(yagura_daemon::control::ApprovalPolicy {
                approval: Some(yagura_daemon::control::ApprovalSection {
                    auto_approve: vec!["open_window".to_string()],
                }),
                ..yagura_daemon::control::ApprovalPolicy::default()
            }),
            ..DaemonConfig::default()
        };
        let registry = Arc::new(WindowRegistry::new(config.clone()));
        let endpoint = unique_endpoint_name("yagura-mcp-rejected-arg", "cwd");
        let server = Arc::new(IpcServer::new(
            registry.clone(),
            endpoint.clone(),
            config.clone(),
        ));
        let server_task = tokio::spawn(server.run());

        let transport = EndpointTransport::new(endpoint, None);
        // `EndpointTransport` connects lazily and does not retry, so the first
        // asserted call must not race the accept loop coming up.
        let deadline = tokio::time::Instant::now() + READY_TIMEOUT;
        while reply_within(READY_TIMEOUT, "list_windows", list_windows(&transport))
            .await
            .is_err()
        {
            assert!(
                tokio::time::Instant::now() < deadline,
                "the daemon endpoint never became reachable"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        let missing = r"C:\yagura-nonexistent-test-path-should-never-exist-12345";
        let err = reply_within(
            READY_TIMEOUT,
            "open_window",
            open_window(&transport, open_window_args(Some(missing))),
        )
        .await
        .expect_err("a nonexistent cwd must be refused");

        assert_eq!(
            err.code(),
            "invalid_argument",
            "the bridge must keep reporting a refused cwd as invalid_argument"
        );
        assert!(
            err.to_string().contains(missing),
            "the daemon's reason must reach the caller, got: {err}"
        );

        server_task.abort();
    }
}
