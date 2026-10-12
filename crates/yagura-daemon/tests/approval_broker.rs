#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The pending-approval ledger (`approval.rs`): who is told about a request,
//! who may settle it, and the guarantee that every pending request ends.
//!
//! The ledger is exercised through a recording [`ApproverSink`] double rather
//! than a real connection, which is what keeps these properties testable
//! without an IPC server: the sink records what it was pushed and can be told
//! to refuse a push, standing in for an approver whose outbound queue is full.
//!
//! Delivery to the *requester* is observed through the ticket's receiver, so
//! "the decision reached the waiting caller" is asserted rather than inferred
//! from the ledger's own bookkeeping.

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use yagura_daemon::approval::{
    ApprovalBroker, ApprovalDraft, ApprovalLimits, ApprovalRoomRef, ApproverSink, DecideError,
    SubmitError, Ticket, WithdrawReason,
};
use yagura_daemon::control::DestructiveTool;
use yagura_protocol::{
    ApprovalRequest, ApprovalRequester, ApprovalRequesterKind, ApprovalTarget, DaemonMessage,
    RoomRef, WindowStatus,
};

/// The boot nonce every request id in these tests is minted against.
const NONCE: &str = "k3f9xa";

/// Generous limits, so a test that is not about the caps never trips one.
const ROOMY: ApprovalLimits = ApprovalLimits {
    max_pending: 32,
    max_pending_per_conn: 4,
};

/// An approver that records everything pushed to it and can be told to start
/// refusing pushes, which is how a full outbound queue looks to the ledger.
struct RecordingSink {
    accepts: AtomicBool,
    pushed: Mutex<Vec<DaemonMessage>>,
}

impl RecordingSink {
    fn accepting() -> Arc<Self> {
        Arc::new(Self {
            accepts: AtomicBool::new(true),
            pushed: Mutex::new(Vec::new()),
        })
    }

    fn refusing() -> Arc<Self> {
        Arc::new(Self {
            accepts: AtomicBool::new(false),
            pushed: Mutex::new(Vec::new()),
        })
    }

    fn messages(&self) -> Vec<DaemonMessage> {
        self.pushed
            .lock()
            .expect("the sink lock must not be poisoned")
            .clone()
    }

    /// The requests this sink was asked to approve, in push order.
    fn requested(&self) -> Vec<ApprovalRequest> {
        self.messages()
            .into_iter()
            .filter_map(|msg| match msg {
                DaemonMessage::ApprovalRequested { request } => Some(request),
                _ => None,
            })
            .collect()
    }

    /// The `(request_id, reason)` pairs this sink was told about, in push order.
    fn withdrawn(&self) -> Vec<(String, WithdrawReason)> {
        self.messages()
            .into_iter()
            .filter_map(|msg| match msg {
                DaemonMessage::ApprovalWithdrawn { request_id, reason } => {
                    Some((request_id, reason))
                }
                _ => None,
            })
            .collect()
    }
}

impl ApproverSink for RecordingSink {
    fn push(&self, msg: &DaemonMessage) -> bool {
        self.pushed
            .lock()
            .expect("the sink lock must not be poisoned")
            .push(msg.clone());
        self.accepts.load(Ordering::SeqCst)
    }
}

fn broker() -> ApprovalBroker {
    ApprovalBroker::new(NONCE)
}

/// A `close_window` request against `w2`, asked for by window `w1`.
fn draft() -> ApprovalDraft {
    ApprovalDraft {
        tool: DestructiveTool::CloseWindow,
        requester: ApprovalRequester {
            kind: ApprovalRequesterKind::Window,
            window_id: Some("w1-abc".to_string()),
            name: Some("agent A".to_string()),
        },
        target: Some(ApprovalTarget {
            window_id: "w2-abc".to_string(),
            name: "agent B".to_string(),
            status: WindowStatus::Idle,
        }),
        text: None,
        // close_window is not addressed to a chat room.
        room: None,
    }
}

/// A `send_chat` request with no target window: the room itself is the
/// target, so [`ApprovalDraft::target`] is `None`.
fn chat_draft() -> ApprovalDraft {
    ApprovalDraft {
        tool: DestructiveTool::SendChat,
        requester: ApprovalRequester {
            kind: ApprovalRequesterKind::Window,
            window_id: Some("w1-abc".to_string()),
            name: Some("agent A".to_string()),
        },
        target: None,
        text: Some("[yagura chat] {\"from\":\"w1-abc\"}".to_string()),
        room: None,
    }
}

/// [`chat_draft`], with a resolved room attached: the shape
/// `resolve_room` + `disambiguate` hand `post_chat` once a chat post's room
/// and display name are both known.
fn chat_draft_with_room() -> ApprovalDraft {
    ApprovalDraft {
        room: Some(ApprovalRoomRef {
            room_id: "r-0123456789abcdef".to_string(),
            name: "yagura".to_string(),
        }),
        ..chat_draft()
    }
}

// =============================================================================
// Submitting
// =============================================================================

#[test]
fn submitting_with_no_approver_registered_is_refused() {
    // Nobody is waited for. With no approver connected the request
    // is refused immediately rather than parked until it times out.
    let broker = broker();
    let result = broker.submit(draft(), 1, ROOMY);
    assert!(
        matches!(result, Err(SubmitError::NoApprover)),
        "a submit with no registered approver must fail with NoApprover, got \
         {result:?}"
    );
}

#[test]
fn submitting_pushes_the_request_to_every_registered_approver() {
    let broker = broker();
    let first = RecordingSink::accepting();
    let second = RecordingSink::accepting();
    broker.register_approver(10, first.clone());
    broker.register_approver(11, second.clone());

    let ticket = broker
        .submit(draft(), 1, ROOMY)
        .expect("a submit with two approvers must be accepted");

    for (label, sink) in [("first", &first), ("second", &second)] {
        let requested = sink.requested();
        assert_eq!(
            requested.len(),
            1,
            "the {label} approver must be asked exactly once, got {requested:?}"
        );
        assert_eq!(
            requested[0].request_id,
            ticket.request_id(),
            "every approver must be asked about the same request id as the \
             ticket the requester holds"
        );
    }
}

#[test]
fn the_submitted_request_carries_the_draft_verbatim() {
    let broker = broker();
    let sink = RecordingSink::accepting();
    broker.register_approver(10, sink.clone());

    let mut draft = draft();
    draft.tool = DestructiveTool::SendMessage;
    draft.text = Some("please run the tests".to_string());
    let _ticket = broker
        .submit(draft, 1, ROOMY)
        .expect("a submit with an approver must be accepted");

    let requested = sink.requested();
    let request = requested.first().expect("the approver must be asked");
    assert_eq!(
        request.tool, "send_message",
        "the tool must reach the approver under its external name"
    );
    assert_eq!(
        request.text.as_deref(),
        Some("please run the tests"),
        "the approver decides on the actual body, so it is carried in full"
    );
    assert_eq!(request.requester.kind, ApprovalRequesterKind::Window);
    assert_eq!(request.requester.window_id.as_deref(), Some("w1-abc"));
    assert_eq!(
        request.target.as_ref().map(|target| target.status),
        Some(WindowStatus::Idle),
        "the target status is the snapshot taken at submit time"
    );
}

#[test]
fn a_draft_with_no_target_submits_and_reaches_the_approver_with_no_target() {
    // A chat post is not addressed to any one window, so the
    // request it produces carries no target at all -- not a placeholder one.
    let broker = broker();
    let sink = RecordingSink::accepting();
    broker.register_approver(10, sink.clone());

    let _ticket = broker
        .submit(chat_draft(), 1, ROOMY)
        .expect("a targetless draft must still submit");

    let requested = sink.requested();
    let request = requested.first().expect("the approver must be asked");
    assert_eq!(request.tool, "send_chat");
    assert_eq!(
        request.target, None,
        "a chat request must carry no target, not a synthesized placeholder"
    );
    assert_eq!(
        request.room, None,
        "a chat draft with no room attached must reach the approver with none either"
    );
}

// The wire ApprovalRequest.room is built from the daemon-internal
// ApprovalDraft.room verbatim (same room_id and name), so an approver's dialog
// can name the room a chat post is addressed to.
#[test]
fn a_draft_with_a_room_reaches_the_approver_carrying_the_same_room() {
    let broker = broker();
    let sink = RecordingSink::accepting();
    broker.register_approver(10, sink.clone());

    let _ticket = broker
        .submit(chat_draft_with_room(), 1, ROOMY)
        .expect("a draft with a room must still submit");

    let requested = sink.requested();
    let request = requested.first().expect("the approver must be asked");
    assert_eq!(
        request.room,
        Some(RoomRef {
            room_id: "r-0123456789abcdef".to_string(),
            name: "yagura".to_string(),
        })
    );
}

#[test]
fn an_approver_that_refuses_the_push_is_dropped_and_the_rest_still_get_it() {
    let broker = broker();
    let dead = RecordingSink::refusing();
    let live = RecordingSink::accepting();
    broker.register_approver(10, dead.clone());
    broker.register_approver(11, live.clone());

    let ticket = broker
        .submit(draft(), 1, ROOMY)
        .expect("one reachable approver is enough for a submit to be accepted");
    assert_eq!(
        live.requested().len(),
        1,
        "the reachable approver must still be asked"
    );

    // The refusing approver was evicted, so it is no longer entitled to decide.
    let result = broker.decide(10, &ticket.request_id(), true);
    assert!(
        matches!(result, Err(DecideError::NotPending)),
        "an approver dropped for refusing a push must no longer be able to \
         settle the request, got {result:?}"
    );
}

#[test]
fn a_lone_approver_that_refuses_the_push_leaves_no_pending_entry() {
    // An approver that cannot be reached is no approver. The
    // request must not stay in the ledger where a later `register_approver`
    // would resurrect it.
    let broker = broker();
    let dead = RecordingSink::refusing();
    broker.register_approver(10, dead.clone());

    let result = broker.submit(draft(), 1, ROOMY);
    assert!(
        matches!(result, Err(SubmitError::NoApprover)),
        "a submit whose only approver refuses the push must fail with \
         NoApprover, got {result:?}"
    );

    let fresh = RecordingSink::accepting();
    let pending = broker.register_approver(11, fresh);
    assert!(
        pending.is_empty(),
        "the refused request must not have been recorded as pending, got \
         {pending:?}"
    );
}

// =============================================================================
// Registering
// =============================================================================

#[test]
fn registering_returns_the_requests_already_pending() {
    let broker = broker();
    let first = RecordingSink::accepting();
    broker.register_approver(10, first);
    let ticket = broker
        .submit(draft(), 1, ROOMY)
        .expect("a submit with an approver must be accepted");

    let latecomer = RecordingSink::accepting();
    let pending = broker.register_approver(11, latecomer);
    assert_eq!(
        pending.len(),
        1,
        "an approver that connects while a request is pending must be handed \
         it, or a GUI restart would leave the request invisible, got {pending:?}"
    );
    assert_eq!(pending[0].request_id, ticket.request_id());
}

#[test]
fn re_registering_the_same_connection_is_idempotent() {
    let broker = broker();
    let sink = RecordingSink::accepting();
    broker.register_approver(10, sink.clone());
    let ticket = broker
        .submit(draft(), 1, ROOMY)
        .expect("a submit with an approver must be accepted");

    let pending = broker.register_approver(10, sink.clone());
    assert_eq!(
        pending.len(),
        1,
        "re-registering the same connection must succeed and return the current \
         pending list, got {pending:?}"
    );

    let second = broker
        .submit(draft(), 2, ROOMY)
        .expect("the second submit must be accepted");
    let asked: Vec<String> = sink
        .requested()
        .into_iter()
        .map(|request| request.request_id)
        .collect();
    assert_eq!(
        asked,
        vec![ticket.request_id(), second.request_id()],
        "a re-registered connection must be held once, not twice: a duplicate \
         entry would push every request to it twice"
    );
}

#[test]
fn unregistering_the_only_approver_leaves_no_one_to_ask() {
    let broker = broker();
    let sink = RecordingSink::accepting();
    broker.register_approver(10, sink);
    broker.unregister_approver(10);

    let result = broker.submit(draft(), 1, ROOMY);
    assert!(
        matches!(result, Err(SubmitError::NoApprover)),
        "after the last approver disconnects a submit must be refused, got \
         {result:?}"
    );
}

#[test]
fn registering_returns_pending_requests_in_submission_order() {
    // The pending list handed to an approver must reflect
    // submission order (oldest first), not the ledger's internal iteration
    // order. `HashMap`'s iteration order changes from run to run, so a set
    // comparison alone would pass by coincidence; this asserts the exact
    // sequence, twice, over enough entries that a coincidental match across
    // both calls is not plausible.
    let broker = broker();
    let sink = RecordingSink::accepting();
    broker.register_approver(10, sink.clone());

    let limits = ApprovalLimits {
        max_pending: 32,
        max_pending_per_conn: 6,
    };
    let tickets: Vec<_> = (0..6)
        .map(|_| {
            broker
                .submit(draft(), 1, limits)
                .expect("a submit with an approver registered must be accepted")
        })
        .collect();
    let submitted: Vec<String> = tickets.iter().map(Ticket::request_id).collect();

    let first = broker.register_approver(10, sink.clone());
    let second = broker.register_approver(10, sink);

    let first_ids: Vec<String> = first
        .into_iter()
        .map(|request| request.request_id)
        .collect();
    let second_ids: Vec<String> = second
        .into_iter()
        .map(|request| request.request_id)
        .collect();

    assert_eq!(
        first_ids, submitted,
        "a re-registration must return the pending requests in submission \
         order, got {first_ids:?}, wanted {submitted:?}"
    );
    assert_eq!(
        second_ids, submitted,
        "a second re-registration on the same connection must return the \
         same submission order, got {second_ids:?}, wanted {submitted:?}"
    );
}

// =============================================================================
// Deciding
// =============================================================================

#[test]
fn a_decision_reaches_the_requester_and_withdraws_the_request_from_the_others() {
    let broker = broker();
    let decider = RecordingSink::accepting();
    let other = RecordingSink::accepting();
    broker.register_approver(10, decider.clone());
    broker.register_approver(11, other.clone());
    let mut ticket = broker
        .submit(draft(), 1, ROOMY)
        .expect("a submit with approvers must be accepted");
    let request_id = ticket.request_id();

    broker
        .decide(10, &request_id, true)
        .expect("the registered approver must be allowed to decide");

    assert_eq!(
        ticket.rx.try_recv(),
        Ok(true),
        "the approval must reach the waiting requester"
    );
    assert_eq!(
        other.withdrawn(),
        vec![(request_id.clone(), WithdrawReason::Resolved)],
        "the approvers that did not decide must be told the dialog is over, \
         with the reason `resolved`"
    );
    assert!(
        decider.withdrawn().is_empty(),
        "the approver that decided already knows; a withdrawal to it would ask \
         the GUI to close a dialog it closed itself"
    );
}

#[test]
fn a_denial_reaches_the_requester_as_a_denial() {
    let broker = broker();
    let sink = RecordingSink::accepting();
    broker.register_approver(10, sink);
    let mut ticket = broker
        .submit(draft(), 1, ROOMY)
        .expect("a submit with an approver must be accepted");

    broker
        .decide(10, &ticket.request_id(), false)
        .expect("a denial must be accepted from a registered approver");

    assert_eq!(
        ticket.rx.try_recv(),
        Ok(false),
        "a denial must reach the requester as `false`, not as a silence that \
         would be indistinguishable from a timeout"
    );
}

#[test]
fn only_the_first_decision_settles_a_request() {
    // One request, one decision, first come first served.
    let broker = broker();
    let first = RecordingSink::accepting();
    let second = RecordingSink::accepting();
    broker.register_approver(10, first);
    broker.register_approver(11, second);
    let ticket = broker
        .submit(draft(), 1, ROOMY)
        .expect("a submit with approvers must be accepted");
    let request_id = ticket.request_id();

    broker
        .decide(10, &request_id, true)
        .expect("the first decision must be accepted");
    let result = broker.decide(11, &request_id, false);
    assert!(
        matches!(result, Err(DecideError::NotPending)),
        "a second decision on a settled request must be refused, so a race \
         between two approvers cannot flip an approval into a denial, got \
         {result:?}"
    );
}

#[test]
fn a_decision_from_an_unregistered_connection_is_refused() {
    let broker = broker();
    let sink = RecordingSink::accepting();
    broker.register_approver(10, sink);
    let ticket = broker
        .submit(draft(), 1, ROOMY)
        .expect("a submit with an approver must be accepted");

    let result = broker.decide(99, &ticket.request_id(), true);
    assert!(
        matches!(result, Err(DecideError::NotPending)),
        "a connection that never registered as an approver must not be able to \
         settle a request, got {result:?}"
    );
}

#[test]
fn a_decision_on_an_unknown_request_id_is_refused() {
    let broker = broker();
    let sink = RecordingSink::accepting();
    broker.register_approver(10, sink);

    let result = broker.decide(10, "a999-nope", true);
    assert!(
        matches!(result, Err(DecideError::NotPending)),
        "an id that is not pending must be refused rather than silently \
         accepted, got {result:?}"
    );
}

// =============================================================================
// Termination: every pending request ends
// =============================================================================

#[test]
fn dropping_the_ticket_withdraws_the_request_as_requester_disconnected() {
    let broker = broker();
    let sink = RecordingSink::accepting();
    broker.register_approver(10, sink.clone());
    let ticket = broker
        .submit(draft(), 1, ROOMY)
        .expect("a submit with an approver must be accepted");
    let request_id = ticket.request_id();

    drop(ticket);

    assert_eq!(
        sink.withdrawn(),
        vec![(request_id.clone(), WithdrawReason::RequesterDisconnected)],
        "when the requester goes away the approver's dialog must be taken down, \
         naming the default reason"
    );
    let result = broker.decide(10, &request_id, true);
    assert!(
        matches!(result, Err(DecideError::NotPending)),
        "a withdrawn request must be gone from the ledger, got {result:?}"
    );
}

#[test]
fn withdrawing_with_a_reason_wins_over_the_drop_default() {
    // The timeout path withdraws explicitly and then lets the ticket drop; the
    // approver must be told once, and told `timeout` rather than the default.
    let broker = broker();
    let sink = RecordingSink::accepting();
    broker.register_approver(10, sink.clone());
    let ticket = broker
        .submit(draft(), 1, ROOMY)
        .expect("a submit with an approver must be accepted");
    let request_id = ticket.request_id();

    ticket.withdraw(WithdrawReason::Timeout);
    drop(ticket);

    assert_eq!(
        sink.withdrawn(),
        vec![(request_id, WithdrawReason::Timeout)],
        "an explicit withdrawal must be reported once, with its own reason: a \
         second notification from Drop would reopen a closed dialog"
    );
}

// =============================================================================
// Caps
// =============================================================================

#[test]
fn the_daemon_wide_cap_refuses_further_submits() {
    let broker = broker();
    let sink = RecordingSink::accepting();
    broker.register_approver(10, sink);
    let limits = ApprovalLimits {
        max_pending: 2,
        max_pending_per_conn: 2,
    };

    let _held = [
        broker
            .submit(draft(), 1, limits)
            .expect("the first submit is within both caps"),
        broker
            .submit(draft(), 2, limits)
            .expect("the second submit is within both caps"),
    ];

    let result = broker.submit(draft(), 3, limits);
    assert!(
        matches!(result, Err(SubmitError::TooManyPending)),
        "a third pending request from a third connection must hit the \
         daemon-wide cap, got {result:?}"
    );
}

#[test]
fn the_per_connection_cap_refuses_further_submits_from_that_connection() {
    let broker = broker();
    let sink = RecordingSink::accepting();
    broker.register_approver(10, sink);
    let limits = ApprovalLimits {
        max_pending: 32,
        max_pending_per_conn: 1,
    };

    let _held = broker
        .submit(draft(), 1, limits)
        .expect("the first submit is within both caps");

    let result = broker.submit(draft(), 1, limits);
    assert!(
        matches!(result, Err(SubmitError::TooManyPendingForConn)),
        "a second pending request from the same connection must hit the \
         per-connection cap, distinctly from the daemon-wide one, got {result:?}"
    );

    let other = broker.submit(draft(), 2, limits);
    assert!(
        other.is_ok(),
        "the per-connection cap must not spill onto another connection, got \
         {other:?}"
    );
}

#[test]
fn a_settled_request_frees_its_place_in_the_caps() {
    let broker = broker();
    let sink = RecordingSink::accepting();
    broker.register_approver(10, sink);
    let limits = ApprovalLimits {
        max_pending: 1,
        max_pending_per_conn: 1,
    };

    let ticket = broker
        .submit(draft(), 1, limits)
        .expect("the first submit is within both caps");
    drop(ticket);

    let result = broker.submit(draft(), 1, limits);
    assert!(
        result.is_ok(),
        "a cap counts what is pending, not what has ever been submitted, got \
         {result:?}"
    );
}

// =============================================================================
// Request ids
// =============================================================================

#[test]
fn request_ids_carry_the_boot_nonce_and_increase() {
    // Same shape as a window id (`w{sequence}-{boot nonce}`): the nonce keeps a
    // stale id from a previous daemon from resolving here, and the sequence
    // makes ids unique within this one.
    let broker = broker();
    let sink = RecordingSink::accepting();
    broker.register_approver(10, sink);

    let first = broker
        .submit(draft(), 1, ROOMY)
        .expect("the first submit must be accepted");
    let second = broker
        .submit(draft(), 1, ROOMY)
        .expect("the second submit must be accepted");

    assert_eq!(first.request_id(), format!("a1-{NONCE}"));
    assert_eq!(second.request_id(), format!("a2-{NONCE}"));
    assert!(
        first.request_id().contains(NONCE) && second.request_id().contains(NONCE),
        "every request id must carry this daemon's boot nonce"
    );
}

#[test]
fn request_ids_do_not_repeat_after_a_request_is_settled() {
    let broker = broker();
    let sink = RecordingSink::accepting();
    broker.register_approver(10, sink);

    let first = broker
        .submit(draft(), 1, ROOMY)
        .expect("the first submit must be accepted");
    let first_id = first.request_id();
    drop(first);

    let second = broker
        .submit(draft(), 1, ROOMY)
        .expect("the second submit must be accepted");
    assert_ne!(
        second.request_id(),
        first_id,
        "the sequence must not be reused once a request leaves the ledger, or a \
         late decision could settle the wrong request"
    );
}

// =============================================================================
// Reaching the approvers
// =============================================================================

fn chat_changed() -> DaemonMessage {
    DaemonMessage::ChatChanged {
        room_id: "r1".to_string(),
        seq: 1,
        needs_user: false,
    }
}

#[test]
fn has_approvers_follows_registration_and_unregistration() {
    let broker = broker();
    assert!(!broker.has_approvers(), "a new ledger has no approver");

    broker.register_approver(10, RecordingSink::accepting());
    assert!(
        broker.has_approvers(),
        "a registered approver must be reported, so a gated request is not refused as unapprovable"
    );

    broker.unregister_approver(10);
    assert!(!broker.has_approvers(), "the last approver left");
}

#[test]
fn notifying_delivers_the_message_to_every_accepting_approver() {
    let broker = broker();
    let first = RecordingSink::accepting();
    let second = RecordingSink::accepting();
    broker.register_approver(10, first.clone());
    broker.register_approver(11, second.clone());

    broker.notify_approvers(&chat_changed());
    assert_eq!(first.messages(), vec![chat_changed()]);
    assert_eq!(second.messages(), vec![chat_changed()]);
}

#[test]
fn an_approver_that_refuses_a_notification_is_no_longer_an_approver() {
    let broker = broker();
    broker.register_approver(10, RecordingSink::refusing());

    broker.notify_approvers(&chat_changed());
    assert!(
        !broker.has_approvers(),
        "an approver that refuses a push is no approver"
    );
}
