#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The daemon's `unknown_type` refusal -- the answer a
//! peer built before a given `ClientMessage` variant existed gives for it --
//! must reach the caller as "this feature is not supported", not as an
//! opaque internal error.
//!
//! Duplicates a minimal `MockTransport` rather than depending on `tools.rs`'s
//! private one (this crate's `tests/` binaries do not share a `common`
//! module).

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use yagura_mcp::tools::{ToolError, list_windows};
use yagura_mcp::transport::{DaemonTransport, RequesterIdentity, TransportError};
use yagura_protocol::{ClientMessage, DaemonMessage};

/// A transport that answers exactly one `round_trip` call with a scripted
/// response, regardless of the message sent.
struct MockTransport {
    response: Mutex<Option<Result<DaemonMessage, TransportError>>>,
    call_count: AtomicUsize,
}

impl MockTransport {
    fn with_response(response: Result<DaemonMessage, TransportError>) -> Self {
        Self {
            response: Mutex::new(Some(response)),
            call_count: AtomicUsize::new(0),
        }
    }
}

impl DaemonTransport for MockTransport {
    fn requester_identity(&self) -> RequesterIdentity {
        RequesterIdentity::External
    }

    async fn round_trip(&self, _msg: ClientMessage) -> Result<DaemonMessage, TransportError> {
        self.call_count.fetch_add(1, Ordering::SeqCst);
        self.response
            .lock()
            .expect("response mutex must not be poisoned")
            .take()
            .expect("round_trip called more than once against a single-response mock")
    }
}

/// An `"unknown_type"` refusal is the shape a daemon built before a
/// given `ClientMessage` variant existed gives for it; this
/// bridge must map it onto the caller-facing vocabulary for "not supported",
/// not the catch-all for a daemon contract violation.
#[tokio::test]
async fn an_unknown_type_refusal_maps_to_unsupported_not_internal() {
    let transport = MockTransport::with_response(Ok(DaemonMessage::Error {
        code: "unknown_type".to_string(),
        message: "the daemon does not recognize this message type".to_string(),
        rejected_argument: None,
    }));

    let result = list_windows(&transport).await;

    match result {
        Err(ToolError::Unsupported(_)) => {}
        other => panic!(
            "an \"unknown_type\" refusal from the daemon must surface as \"unsupported\", not \
             \"internal\", got {other:?}"
        ),
    }
}
