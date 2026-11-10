//! The doors a CLI's hook reads a window's inbox through: `deliver_context`,
//! `deliver_take`, `deliver_wait`, and `report_session`, which tells the
//! daemon the CLI's own session.
//!
//! They answer in any status, `Unknown` included: the CLI asks for what is
//! waiting for it, the daemon pushes nothing. Pushing is the wait's doing,
//! and it never pushes to a window whose status is `Unknown`
//! ([`crate::inbox::Inbox`]).
//!
//! A held read's parcels stay with the connection it was answered on until
//! the door sends `deliver_done` there; a read that is not held is settled
//! before it is answered.

use std::sync::{Arc, Mutex, PoisonError};

use yagura_protocol::DaemonMessage;

use crate::inbox::{DoorHold, DoorRead, Wait, silence};
use crate::window::WindowEntry;

use super::{ConnectionState, Outbound, error_msg, pending_waits_exhausted_msg};

/// A held read waiting for its door's `deliver_done`.
pub(super) struct HeldRead {
    hold: DoorHold,
    #[cfg(feature = "fault-injection")]
    faults: crate::fault_point::FaultPoints,
}

impl HeldRead {
    fn new(entry: &WindowEntry, hold: DoorHold) -> Self {
        #[cfg(not(feature = "fault-injection"))]
        let _ = entry;
        Self {
            hold,
            #[cfg(feature = "fault-injection")]
            faults: entry.faults().clone(),
        }
    }

    /// Commits the end of what the read took; whether it was withdrawn
    /// instead. `by_door` when its door confirmed it.
    async fn settle(self, by_door: bool) -> bool {
        #[cfg(feature = "fault-injection")]
        if self.hold.keeps_rows() {
            use crate::fault_point::FaultPoint;
            self.faults.reach(FaultPoint::ParcelEndHalfSettled);
            if by_door {
                self.faults.reach(FaultPoint::DoorBeforeSettle);
            }
        }
        #[cfg(not(feature = "fault-injection"))]
        let _ = by_door;
        self.hold.confirm().await
    }
}

/// Where a connection keeps its held read: the only place `deliver_done`
/// finds one, so no other connection can confirm it. A wait's task fills it
/// from outside the connection's loop.
pub(super) type HeldSlot = Arc<Mutex<Option<HeldRead>>>;

fn put_held(slot: &HeldSlot, held: HeldRead) {
    // The read it replaces goes back once the slot's lock is let go.
    let replaced = slot
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .replace(held);
    drop(replaced);
}

/// Answers a door with `read`: held in `held`, its parcels stay there for the
/// door's `deliver_done`; otherwise their end is committed first.
pub(super) async fn answer_read(
    entry: &WindowEntry,
    outbound: &Outbound,
    read: DoorRead,
    held: Option<&HeldSlot>,
) {
    let DoorRead { message, hold } = read;
    let read = HeldRead::new(entry, hold);
    if let Some(slot) = held {
        put_held(slot, read);
        outbound.reply(&message);
    } else if read.settle(false).await {
        outbound.reply(&silence());
    } else {
        outbound.reply(&message);
    }
}

/// Answers `deliver_context`.
pub(super) async fn handle_context(
    entry: &WindowEntry,
    outbound: &Outbound,
    intro: &str,
    held: Option<&HeldSlot>,
) {
    let read = entry.inbox().read_context(intro, None);
    answer_read(entry, outbound, read, held).await;
}

/// Answers `deliver_take`.
pub(super) async fn handle_take(entry: &WindowEntry, outbound: &Outbound, held: Option<&HeldSlot>) {
    let read = entry.inbox().read_take(None);
    answer_read(entry, outbound, read, held).await;
}

/// Answers `deliver_done` with whether the connection's held read was
/// withdrawn, once what it took is settled.
pub(super) async fn handle_done(outbound: &Outbound, slot: &HeldSlot) {
    let held = slot.lock().unwrap_or_else(PoisonError::into_inner).take();
    match held {
        Some(held) => {
            let withdrawn = held.settle(true).await;
            outbound.reply(&DaemonMessage::DeliverConfirmed { withdrawn });
        }
        None => outbound.reply(&error_msg(
            "invalid_argument",
            "deliver_done needs a held read answered on this connection",
        )),
    }
}

/// Answers `deliver_wait` at once when a parcel can be pushed, and otherwise
/// leaves the wait registered until a parcel arrives, a newer wait replaces it,
/// or the connection ends.
pub(super) async fn handle_wait(
    entry: &Arc<WindowEntry>,
    outbound: &Arc<Outbound>,
    state: &mut ConnectionState,
    max_waiters: usize,
    held: bool,
) {
    if state.live_waiter_count() >= max_waiters {
        outbound.reply(&pending_waits_exhausted_msg());
        return;
    }
    match entry.inbox().wait() {
        Wait::Answered(read) => {
            answer_read(entry, outbound, *read, held.then_some(&state.held_read)).await;
        }
        Wait::Registered { guard, answer } => {
            let entry = Arc::clone(entry);
            let outbound = Arc::clone(outbound);
            let slot = Arc::clone(&state.held_read);
            state.waiters.push(tokio::spawn(async move {
                // Dropped with the task, which the connection aborts when it ends.
                let _guard = guard;
                match answer.await {
                    Ok(read) => answer_read(&entry, &outbound, read, held.then_some(&slot)).await,
                    Err(_) => {
                        let silent = DoorRead::silent();
                        answer_read(&entry, &outbound, silent, held.then_some(&slot)).await;
                    }
                }
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
