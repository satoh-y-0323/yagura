//! The doors a CLI's hook reads a window's inbox through: `deliver_context`,
//! `deliver_take`, `deliver_wait`, and `report_session`, which tells the
//! daemon the CLI's own session.
//!
//! They answer in any status, `Unknown` included: the CLI asks for what is
//! waiting for it, the daemon pushes nothing. Pushing is the wait's doing,
//! and it never pushes to a window whose status is `Unknown`
//! ([`crate::inbox::Inbox`]).

use std::sync::Arc;

use yagura_protocol::DaemonMessage;

use crate::inbox::{DeliverySpec, Wait, silence};
use crate::window::WindowEntry;

use super::{ConnectionState, Outbound, pending_waits_exhausted_msg};

/// The answer to `deliver_context`: the intro and what is waiting behind it,
/// printed as `spec` says, or as the window's own delivery says when `None`.
pub(super) fn context_message(
    entry: &WindowEntry,
    intro: &str,
    spec: Option<&DeliverySpec>,
) -> DaemonMessage {
    match spec {
        Some(spec) => entry.inbox().context_by(intro, spec),
        None => entry.inbox().context(intro),
    }
}

/// The answer to `deliver_take`: what is waiting, printed as in
/// [`context_message`].
pub(super) fn take_message(entry: &WindowEntry, spec: Option<&DeliverySpec>) -> DaemonMessage {
    match spec {
        Some(spec) => entry.inbox().take_by(spec),
        None => entry.inbox().take(),
    }
}

/// Answers `deliver_context`.
pub(super) fn handle_context(entry: &WindowEntry, outbound: &Outbound, intro: &str) {
    outbound.reply(&context_message(entry, intro, None));
}

/// Answers `deliver_take`.
pub(super) fn handle_take(entry: &WindowEntry, outbound: &Outbound) {
    outbound.reply(&take_message(entry, None));
}

/// Answers `deliver_wait` at once when a parcel can be pushed, and otherwise
/// leaves the wait registered until a parcel arrives, a newer wait replaces it,
/// or the connection ends.
pub(super) fn handle_wait(
    entry: &WindowEntry,
    outbound: &Arc<Outbound>,
    state: &mut ConnectionState,
    max_waiters: usize,
) {
    if state.live_waiter_count() >= max_waiters {
        outbound.reply(&pending_waits_exhausted_msg());
        return;
    }
    match entry.inbox().wait() {
        Wait::Answered(message) => outbound.reply(&message),
        Wait::Registered { guard, answer } => {
            let outbound = Arc::clone(outbound);
            state.waiters.push(tokio::spawn(async move {
                // Dropped with the task, which the connection aborts when it ends.
                let _guard = guard;
                let message = answer.await.unwrap_or_else(|_| silence());
                outbound.reply(&message);
            }));
        }
    }
}

/// Answers `report_session`. The value is neither logged nor echoed.
pub(super) fn handle_report_session(entry: &WindowEntry, outbound: &Outbound, session: &str) {
    outbound.reply(&DaemonMessage::SessionRecorded {
        held: entry.record_session_id(session),
    });
}
