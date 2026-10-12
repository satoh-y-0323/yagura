#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Boundary-value passthrough probes for the chat tools that `tools.rs` does
//! not already cover: `read_chat`'s `room_id` / `after_seq` / `limit`
//! arguments are forwarded verbatim (membership -- whether the caller
//! belongs to the named room at all -- is validated by the
//! daemon, not this layer -- see `chat_boundary_probe.rs` in `yagura-daemon`
//! for the daemon side of these same boundaries), exercised here at the
//! extreme values `after_seq = u64::MAX`, `limit = 0` and `limit` one past
//! the documented maximum, plus a `room_id` naming a room other than the
//! caller's own default.
//!
//! Duplicates a minimal `MockTransport` rather than depending on `tools.rs`'s
//! private one (this crate's `tests/` binaries do not share a `common`
//! module).

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use yagura_mcp::tools::{ReadChatArgs, read_chat};
use yagura_mcp::transport::{DaemonTransport, RequesterIdentity, TransportError};
use yagura_protocol::{ClientMessage, DaemonMessage};

struct MockTransport {
    response: Mutex<Option<Result<DaemonMessage, TransportError>>>,
    sent: Mutex<Vec<ClientMessage>>,
    call_count: AtomicUsize,
}

impl MockTransport {
    fn with_response(response: Result<DaemonMessage, TransportError>) -> Self {
        Self {
            response: Mutex::new(Some(response)),
            sent: Mutex::new(Vec::new()),
            call_count: AtomicUsize::new(0),
        }
    }

    fn last_sent(&self) -> ClientMessage {
        self.sent
            .lock()
            .expect("sent log mutex must not be poisoned")
            .last()
            .expect("round_trip must have been called at least once")
            .clone()
    }
}

impl DaemonTransport for MockTransport {
    fn requester_identity(&self) -> RequesterIdentity {
        RequesterIdentity::External
    }

    async fn round_trip(&self, msg: ClientMessage) -> Result<DaemonMessage, TransportError> {
        self.call_count.fetch_add(1, Ordering::SeqCst);
        self.sent
            .lock()
            .expect("sent log mutex must not be poisoned")
            .push(msg);
        self.response
            .lock()
            .expect("response mutex must not be poisoned")
            .take()
            .expect("round_trip called more than once against a single-response mock")
    }
}

fn sample_chat_messages_response() -> DaemonMessage {
    DaemonMessage::ChatMessages {
        room_id: "main".to_string(),
        messages: Vec::new(),
        next_after_seq: 0,
        halted: None,
        needs_user: false,
        roster: Vec::new(),
    }
}

#[tokio::test]
async fn read_chat_forwards_after_seq_of_u64_max_verbatim() {
    let transport = MockTransport::with_response(Ok(sample_chat_messages_response()));
    let args = ReadChatArgs {
        room_id: None,
        after_seq: Some(u64::MAX),
        limit: None,
    };

    read_chat(&transport, args).await.expect("must succeed");

    assert_eq!(
        transport.last_sent(),
        ClientMessage::ReadChat {
            room_id: None,
            after_seq: Some(u64::MAX),
            limit: None,
        }
    );
}

#[tokio::test]
async fn read_chat_forwards_a_limit_of_zero_verbatim_leaving_rejection_to_the_daemon() {
    let transport = MockTransport::with_response(Ok(sample_chat_messages_response()));
    let args = ReadChatArgs {
        room_id: None,
        after_seq: Some(0),
        limit: Some(0),
    };

    read_chat(&transport, args).await.expect("must succeed");

    assert_eq!(
        transport.last_sent(),
        ClientMessage::ReadChat {
            room_id: None,
            after_seq: Some(0),
            limit: Some(0),
        }
    );
}

#[tokio::test]
async fn read_chat_forwards_a_limit_one_past_the_documented_maximum_verbatim() {
    let transport = MockTransport::with_response(Ok(sample_chat_messages_response()));
    let args = ReadChatArgs {
        room_id: None,
        after_seq: Some(0),
        limit: Some(yagura_protocol::CHAT_READ_LIMIT_MAX + 1),
    };

    read_chat(&transport, args).await.expect("must succeed");

    assert_eq!(
        transport.last_sent(),
        ClientMessage::ReadChat {
            room_id: None,
            after_seq: Some(0),
            limit: Some(yagura_protocol::CHAT_READ_LIMIT_MAX + 1),
        }
    );
}

#[tokio::test]
async fn read_chat_forwards_a_room_id_other_than_main_verbatim() {
    let transport = MockTransport::with_response(Ok(sample_chat_messages_response()));
    let args = ReadChatArgs {
        room_id: Some("side-room".to_string()),
        after_seq: Some(0),
        limit: None,
    };

    read_chat(&transport, args).await.expect("must succeed");

    assert_eq!(
        transport.last_sent(),
        ClientMessage::ReadChat {
            room_id: Some("side-room".to_string()),
            after_seq: Some(0),
            limit: None,
        }
    );
}
