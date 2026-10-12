//! The pending-approval ledger: registered approvers, submitted requests and
//! the guarantee that every pending request eventually ends.
//!
//! Independent of `ipc.rs` by design: this module knows nothing about
//! connections, frames or the wire transport, only about [`ApproverSink`] (a
//! place to push a [`DaemonMessage`]) and the requester's own [`oneshot`]
//! channel. Every public method is synchronous and takes a lock only for the
//! duration of its own body, so none is ever held across an `.await`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use thiserror::Error;
use tokio::sync::oneshot;

use crate::control::DestructiveTool;
use yagura_protocol::{ApprovalRequest, ApprovalRequester, ApprovalTarget, DaemonMessage, RoomRef};

// The ledger and the wire name the same reasons, so the protocol's type is
// the only definition rather than a second one kept in step by hand.
pub use yagura_protocol::WithdrawReason;

/// A place a pending [`ApprovalRequest`] (and its resolution) can be pushed
/// to. Implemented by the daemon's real approver connection; a test double
/// records what it was pushed and can be told to refuse, standing in for an
/// approver whose outbound queue is full.
pub trait ApproverSink: Send + Sync {
    /// Pushes `msg` to this approver. A refusal (a full outbound queue)
    /// evicts this sink from the ledger, so it is never asked about
    /// anything again until it re-registers.
    fn push(&self, msg: &DaemonMessage) -> bool;
}

/// One request not yet submitted to the ledger: everything
/// [`ApprovalBroker::submit`] needs to build an [`ApprovalRequest`], minus
/// the `request_id` the broker mints itself.
#[derive(Debug, Clone)]
pub struct ApprovalDraft {
    /// The gated operation.
    pub tool: DestructiveTool,
    /// Who is asking.
    pub requester: ApprovalRequester,
    /// The window the operation would act on. `None` for a chat post,
    /// which is not addressed to any one window, and for `open_window`,
    /// whose window does not exist yet.
    pub target: Option<ApprovalTarget>,
    /// The `send_message` text, the `send_handoff` envelope or the lines
    /// describing what `open_window` would start; absent for the tools that
    /// carry no body.
    pub text: Option<String>,
    /// The chat room a post would go to, present only for `send_chat` /
    /// `broadcast_chat`.
    pub room: Option<ApprovalRoomRef>,
}

/// The room a chat post's [`ApprovalDraft`] names: the resolved room id and
/// the display name an approver is shown, never a workspace path.
#[derive(Debug, Clone)]
pub struct ApprovalRoomRef {
    /// The resolved room's identifier.
    pub room_id: String,
    /// The room's display name at the moment the draft was built.
    pub name: String,
}

/// The two caps [`ApprovalBroker::submit`] enforces. Injected rather than
/// read from [`crate::config::DaemonConfig`] directly, so the ledger carries
/// no configuration dependency of its own.
#[derive(Debug, Clone, Copy)]
pub struct ApprovalLimits {
    /// Daemon-wide cap on pending requests.
    pub max_pending: usize,
    /// Cap on pending requests held by a single requester connection.
    pub max_pending_per_conn: usize,
}

/// Why [`ApprovalBroker::submit`] refused a request.
#[derive(Debug, Error)]
pub enum SubmitError {
    /// No approver is currently reachable.
    #[error("no approver is connected")]
    NoApprover,
    /// The daemon-wide pending cap ([`ApprovalLimits::max_pending`]) is
    /// already reached.
    #[error("too many pending approval requests")]
    TooManyPending,
    /// The requester connection's own cap
    /// ([`ApprovalLimits::max_pending_per_conn`]) is already reached.
    #[error("too many pending approval requests for this connection")]
    TooManyPendingForConn,
}

/// Why [`ApprovalBroker::decide`] refused a decision.
#[derive(Debug, Error)]
pub enum DecideError {
    /// `request_id` names no request currently awaiting a decision from the
    /// given connection -- whether because it was never pending, has already
    /// settled, or the connection is not a currently registered approver.
    #[error("request is not pending")]
    NotPending,
}

/// One request's ledger entry.
struct Entry {
    /// Kept so [`ApprovalBroker::register_approver`] can hand a reconnecting
    /// or late-joining approver the requests already pending, and so
    /// per-connection caps can be counted without a separate index.
    request: ApprovalRequest,
    requester_conn: u64,
    /// Submission order, so the pending list can be handed back oldest
    /// first even though the ledger itself is keyed by id.
    sequence: u64,
    /// Resolves the requester's [`Ticket::rx`]. Consumed exactly once, by
    /// whichever of [`ApprovalBroker::decide`] or a withdrawal reaches this
    /// entry first (the entry is removed from the ledger in the same step,
    /// so only one of them ever can).
    settle: oneshot::Sender<bool>,
}

/// Locks `mutex`, recovering from poisoning rather than propagating it: a
/// panic in one caller (all of them short, lock-scoped closures) must not
/// take the whole ledger down for every other connection.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Ledger state shared between [`ApprovalBroker`] and every [`Ticket`] it
/// hands out, so a ticket can withdraw its own request on `Drop` without
/// borrowing back from the broker that created it.
struct Shared {
    nonce: String,
    next_id: Mutex<u64>,
    approvers: Mutex<HashMap<u64, Arc<dyn ApproverSink>>>,
    pending: Mutex<HashMap<String, Entry>>,
}

impl Shared {
    /// Mints the next request id: `a{sequence}-{boot nonce}`, the same shape
    /// as a window id. The sequence never repeats, even for a settled
    /// request, so a late decision can never resolve the wrong one. It is
    /// returned alongside the id so an [`Entry`] can record its submission
    /// order without parsing the id back apart.
    fn mint_request_id(&self) -> (u64, String) {
        let mut next_id = lock(&self.next_id);
        let sequence = *next_id;
        *next_id += 1;
        (sequence, format!("a{sequence}-{}", self.nonce))
    }

    /// Pushes `msg` to every currently registered approver, evicting any
    /// whose push refuses it. Returns whether at least one
    /// approver accepted it.
    fn push_to_every_approver(&self, msg: &DaemonMessage) -> bool {
        let mut approvers = lock(&self.approvers);
        let mut reached = false;
        approvers.retain(|_, sink| {
            let accepted = sink.push(msg);
            reached |= accepted;
            accepted
        });
        reached
    }

    /// Pushes `msg` to every currently registered approver except `exclude`
    /// (the one who just decided, when there is one). A refusal still evicts
    /// the approver, same as [`Shared::push_to_every_approver`].
    fn broadcast_except(&self, msg: &DaemonMessage, exclude: Option<u64>) {
        let mut approvers = lock(&self.approvers);
        approvers.retain(|conn, sink| Some(*conn) == exclude || sink.push(msg));
    }

    /// Removes `request_id` from the ledger, if still pending, and tells
    /// every approver the dialog is over. A no-op if the request already
    /// ended (settled, or already withdrawn), which is what lets both
    /// [`Ticket::withdraw`] and its `Drop` call this unconditionally.
    fn withdraw_pending(&self, request_id: &str, reason: WithdrawReason) {
        if lock(&self.pending).remove(request_id).is_none() {
            return;
        }
        let msg = DaemonMessage::ApprovalWithdrawn {
            request_id: request_id.to_string(),
            reason,
        };
        self.broadcast_except(&msg, None);
    }
}

/// The pending-approval ledger.
pub struct ApprovalBroker {
    shared: Arc<Shared>,
}

impl ApprovalBroker {
    /// Creates an empty ledger. `boot_nonce` seeds every request id this
    /// broker mints, the same nonce a window id carries, so a stale id from a
    /// previous daemon boot can never resolve here.
    #[must_use]
    pub fn new(boot_nonce: &str) -> Self {
        Self {
            shared: Arc::new(Shared {
                nonce: boot_nonce.to_string(),
                next_id: Mutex::new(1),
                approvers: Mutex::new(HashMap::new()),
                pending: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// Registers `conn` as an approver, replacing any previous sink held for
    /// the same connection (idempotent: re-registering never duplicates the
    /// entry, so a request is never pushed to the same connection twice).
    /// Returns every request currently pending, so a reconnecting or
    /// newly-joined approver's queue is restored rather than left silently
    /// empty. They come back in submission order (oldest first), the order an
    /// approver present from the start would have been pushed them in.
    pub fn register_approver(
        &self,
        conn: u64,
        sink: Arc<dyn ApproverSink>,
    ) -> Vec<ApprovalRequest> {
        lock(&self.shared.approvers).insert(conn, sink);
        let pending = lock(&self.shared.pending);
        // The ledger is a map keyed by id, whose iteration order says nothing
        // about submission order, so sort by sequence.
        let mut entries: Vec<&Entry> = pending.values().collect();
        entries.sort_by_key(|entry| entry.sequence);
        entries
            .into_iter()
            .map(|entry| entry.request.clone())
            .collect()
    }

    /// Whether at least one approver is registered.
    ///
    /// This is only the *sufficient* condition for [`ApprovalBroker::submit`]
    /// to fail with [`SubmitError::NoApprover`] (no approver at all means
    /// nobody can take the push), not an equivalent: `submit` also fails that
    /// way when every registered approver refuses the push. Use it to refuse
    /// early, never to skip `submit`.
    ///
    /// # Examples
    ///
    /// ```
    /// use yagura_daemon::approval::ApprovalBroker;
    ///
    /// let broker = ApprovalBroker::new("boot-nonce");
    /// assert!(!broker.has_approvers());
    /// ```
    #[must_use]
    pub fn has_approvers(&self) -> bool {
        !lock(&self.shared.approvers).is_empty()
    }

    /// Removes `conn` from the set of approvers. Requests it was asked about
    /// stay pending; they simply stop being decidable by this connection.
    pub fn unregister_approver(&self, conn: u64) {
        lock(&self.shared.approvers).remove(&conn);
    }

    /// Pushes an unsolicited message to every registered approver,
    /// e.g. [`DaemonMessage::ChatChanged`]. An approver that refuses the push
    /// is evicted, as it is when it refuses a request.
    pub fn notify_approvers(&self, msg: &DaemonMessage) {
        self.shared.push_to_every_approver(msg);
    }

    /// Submits a new request from `requester_conn`.
    ///
    /// The pushed [`ApprovalRequest`] carries a freshly minted id even when
    /// the submit is ultimately refused for a reason other than the caps
    /// (`NoApprover`): that id is discarded rather than leaked, since nothing
    /// outside this call ever observes it.
    ///
    /// # Errors
    ///
    /// Returns [`SubmitError::TooManyPending`] /
    /// [`SubmitError::TooManyPendingForConn`] when `limits` is already
    /// reached, and [`SubmitError::NoApprover`] when no approver accepts the
    /// push (including the case where every registered approver refuses it).
    pub fn submit(
        &self,
        draft: ApprovalDraft,
        requester_conn: u64,
        limits: ApprovalLimits,
    ) -> Result<Ticket, SubmitError> {
        {
            let pending = lock(&self.shared.pending);
            if pending.len() >= limits.max_pending {
                return Err(SubmitError::TooManyPending);
            }
            let per_conn = pending
                .values()
                .filter(|entry| entry.requester_conn == requester_conn)
                .count();
            if per_conn >= limits.max_pending_per_conn {
                return Err(SubmitError::TooManyPendingForConn);
            }
        }

        let (sequence, request_id) = self.shared.mint_request_id();
        let request = ApprovalRequest {
            request_id: request_id.clone(),
            tool: draft.tool.as_tool_name().to_string(),
            requester: draft.requester,
            target: draft.target,
            text: draft.text,
            room: draft.room.map(|room| RoomRef {
                room_id: room.room_id,
                name: room.name,
            }),
        };
        let msg = DaemonMessage::ApprovalRequested {
            request: request.clone(),
        };

        if !self.shared.push_to_every_approver(&msg) {
            return Err(SubmitError::NoApprover);
        }

        let (settle, rx) = oneshot::channel();
        lock(&self.shared.pending).insert(
            request_id.clone(),
            Entry {
                request,
                requester_conn,
                sequence,
                settle,
            },
        );

        Ok(Ticket {
            rx,
            request_id,
            shared: Arc::clone(&self.shared),
            withdrawn: AtomicBool::new(false),
        })
    }

    /// Settles `request_id` on behalf of `conn`.
    ///
    /// # Errors
    ///
    /// Returns [`DecideError::NotPending`] when `conn` is not a currently
    /// registered approver, when `request_id` is not pending, or when it has
    /// already been settled (first decision wins).
    pub fn decide(&self, conn: u64, request_id: &str, approved: bool) -> Result<(), DecideError> {
        if !lock(&self.shared.approvers).contains_key(&conn) {
            return Err(DecideError::NotPending);
        }
        let Some(entry) = lock(&self.shared.pending).remove(request_id) else {
            return Err(DecideError::NotPending);
        };
        // The requester may already be gone; a failed send just means
        // nobody is listening for a decision that no longer matters.
        let _ = entry.settle.send(approved);
        let msg = DaemonMessage::ApprovalWithdrawn {
            request_id: request_id.to_string(),
            reason: WithdrawReason::Resolved,
        };
        self.shared.broadcast_except(&msg, Some(conn));
        Ok(())
    }
}

/// A submitted request's handle, held by the requester.
///
/// Dropping it withdraws the request with reason
/// [`WithdrawReason::RequesterDisconnected`], unless [`Ticket::withdraw`] was
/// called first: every pending request ends exactly once, and
/// whichever of an explicit withdrawal or a decision reaches the ledger
/// first is the one that is reported.
pub struct Ticket {
    /// Resolves to the approver's decision (`true` = approved). Public so a
    /// caller can `select!` on it alongside a timeout and its own connection
    /// close signal.
    pub rx: oneshot::Receiver<bool>,
    request_id: String,
    shared: Arc<Shared>,
    withdrawn: AtomicBool,
}

impl Ticket {
    /// This ticket's request id.
    #[must_use]
    pub fn request_id(&self) -> String {
        self.request_id.clone()
    }

    /// Ends this request with `reason`, notifying every currently
    /// registered approver. A no-op if the request has already ended
    /// (decided, or withdrawn once already): only the first end is ever
    /// reported.
    pub fn withdraw(&self, reason: WithdrawReason) {
        if self.withdrawn.swap(true, Ordering::SeqCst) {
            return;
        }
        self.shared.withdraw_pending(&self.request_id, reason);
    }
}

impl Drop for Ticket {
    fn drop(&mut self) {
        if self.withdrawn.swap(true, Ordering::SeqCst) {
            return;
        }
        self.shared
            .withdraw_pending(&self.request_id, WithdrawReason::RequesterDisconnected);
    }
}

impl std::fmt::Debug for Ticket {
    // `Arc<dyn ApproverSink>` (reachable only indirectly, through `shared`)
    // has no `Debug` impl, so this cannot be derived; the request id is the
    // only field worth printing anyway.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ticket")
            .field("request_id", &self.request_id)
            .finish_non_exhaustive()
    }
}
