//! The doors a CLI's hook reads a window's inbox through: `door_read`, which
//! names the door (context, take, wait, or a hook event), and `report_session`,
//! which tells the daemon the CLI's own session.
//!
//! They answer in any status, `Unknown` included: the CLI asks for what is
//! waiting for it, the daemon pushes nothing. Pushing is the wait's doing,
//! and it never pushes to a window whose status is `Unknown`
//! ([`crate::inbox::Inbox`]).
//!
//! A read's parcels stay with the connection it was answered on until the door
//! sends `deliver_printed` there; a connection that ends first gives them back.
//! The reads and the confirmation of a door older than
//! [`yagura_protocol::DOOR_READ_MIN_REVISION`] are answered with nothing to
//! print and take nothing: such a door prints after it took.

use std::sync::{Arc, Mutex, PoisonError, Weak};

use yagura_protocol::{DaemonMessage, Door, GuestLineage, WindowStatus};

use crate::inbox::{DoorHold, DoorRead, Wait, silence};
use crate::window::WindowEntry;

use super::{ConnectionState, IpcServer, Outbound, hook::HookRead, pending_waits_exhausted_msg};

/// A read waiting for its door's `deliver_printed`.
pub(super) struct HeldRead {
    // Before `hold`, so it is dropped first: the status is decided before the
    // parcels go back, and whoever sees them back sees the status decided.
    end: Option<StopEnd>,
    // Before `hold` for the same reason: the turn the parcels started is
    // closed before they go back.
    wake: Option<WakeTurn>,
    hold: DoorHold,
    #[cfg(feature = "fault-injection")]
    faults: crate::fault_point::FaultPoints,
}

impl HeldRead {
    fn new(entry: &WindowEntry, hold: DoorHold) -> Self {
        #[cfg(not(feature = "fault-injection"))]
        let _ = entry;
        Self {
            end: None,
            wake: None,
            hold,
            #[cfg(feature = "fault-injection")]
            faults: entry.faults().clone(),
        }
    }

    /// Commits the end of what the read took, its door having printed it.
    async fn settle(mut self) {
        if let Some(end) = self.end.take() {
            end.printed();
        }
        if let Some(wake) = self.wake.take() {
            wake.printed();
        }
        #[cfg(feature = "fault-injection")]
        if self.hold.keeps_rows() {
            self.faults
                .reach(crate::fault_point::FaultPoint::DoorBeforeSettle);
        }
        self.hold.confirm().await;
    }
}

/// Where a stop's block decides its window's status: `running` once the door
/// says it printed, since the CLI then goes on; otherwise, when the read ends,
/// the stop's own status, unless a status was reported after the door began
/// (the window's count of reported statuses tells, not a clock).
pub(super) struct StopEnd {
    entry: Weak<WindowEntry>,
    seen: u64,
    status: WindowStatus,
    printed: bool,
}

impl StopEnd {
    /// The end of a stop's door that began with `entry` as it is now.
    pub(super) fn new(entry: &Arc<WindowEntry>, status: WindowStatus) -> Self {
        Self {
            entry: Arc::downgrade(entry),
            seen: entry.status_event_count(),
            status,
            printed: false,
        }
    }

    fn printed(mut self) {
        self.printed = true;
        if let Some(entry) = self.entry.upgrade() {
            // Before `Running`, so the report finds a press's hold ended.
            entry.note_cli_moved_on();
            entry.record_status_event(WindowStatus::Running);
        }
    }
}

impl Drop for StopEnd {
    fn drop(&mut self) {
        if !self.printed
            && let Some(entry) = self.entry.upgrade()
        {
            entry.record_status_event_unless_newer(self.status, self.seen);
        }
    }
}

/// The turn a waiting door's parcel started by waking its CLI: the CLI was
/// never woken if the door ends without saying it printed, so the turn is
/// closed again then.
struct WakeTurn {
    entry: Weak<WindowEntry>,
    number: u64,
    printed: bool,
}

impl WakeTurn {
    fn printed(mut self) {
        self.printed = true;
        if let Some(entry) = self.entry.upgrade() {
            entry.note_cli_moved_on();
        }
    }
}

impl Drop for WakeTurn {
    fn drop(&mut self) {
        if !self.printed
            && let Some(entry) = self.entry.upgrade()
        {
            entry.turn().wake_unprinted(self.number);
        }
    }
}

/// Where a connection keeps its held read: the only place `deliver_printed`
/// finds one, so no other connection can say a read was printed. A wait's task
/// fills it from outside the connection's loop.
pub(super) type HeldSlot = Arc<Mutex<Option<HeldRead>>>;

fn put_held(slot: &HeldSlot, held: HeldRead) {
    // The read it replaces goes back once the slot's lock is let go.
    let replaced = slot
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .replace(held);
    drop(replaced);
}

/// Answers a door with `read`, naming `hook_timeout_sec`, the time its CLI
/// gives the hook; the parcels the read took stay held in `slot` for the door's
/// `deliver_printed`.
pub(super) fn answer_read(
    entry: &WindowEntry,
    outbound: &Outbound,
    read: DoorRead,
    slot: &HeldSlot,
    hook_timeout_sec: Option<u64>,
) {
    answer_read_ending(entry, outbound, read, slot, hook_timeout_sec, None);
}

/// [`answer_read`], with `end` deciding the window's status where the read
/// ends.
pub(super) fn answer_read_ending(
    entry: &WindowEntry,
    outbound: &Outbound,
    read: DoorRead,
    slot: &HeldSlot,
    hook_timeout_sec: Option<u64>,
    end: Option<StopEnd>,
) {
    let DoorRead { message, hold } = read;
    let mut held = HeldRead::new(entry, hold);
    held.end = end;
    put_held(slot, held);
    outbound.reply(&timed(message, hook_timeout_sec));
}

/// `message`, a delivery, as it is sent with the time its CLI gives the hook.
fn timed(message: DaemonMessage, hook_timeout_sec: Option<u64>) -> DaemonMessage {
    match message {
        DaemonMessage::Delivery {
            output,
            stream,
            exit_code,
            ..
        } => DaemonMessage::Delivery {
            output,
            stream,
            exit_code,
            hook_timeout_sec,
        },
        other => other,
    }
}

/// Answers `deliver_printed`: the connection's held read, if it has one, ends
/// as taken once what it took is settled. There is no answer; the door has
/// ended or ends without waiting for one.
pub(super) async fn handle_printed(slot: &HeldSlot) {
    let held = slot.lock().unwrap_or_else(PoisonError::into_inner).take();
    if let Some(held) = held {
        held.settle().await;
    }
}

/// Answers a read of a door older than
/// [`yagura_protocol::DOOR_READ_MIN_REVISION`]: it prints after it took, so
/// letting it take would lose what it took.
pub(super) fn handle_older_read(outbound: &Outbound) {
    outbound.reply(&silence());
}

/// Answers the `deliver_done` of a door older than
/// [`yagura_protocol::DOOR_READ_MIN_REVISION`] as withdrawn, which ends such a
/// door without printing.
pub(super) fn handle_older_done(outbound: &Outbound) {
    outbound.reply(&DaemonMessage::DeliverConfirmed { withdrawn: true });
}

impl IpcServer {
    /// Answers `door_read`; `guest` is what the door collected of its
    /// lineage in a WSL guest.
    pub(super) async fn handle_door_read(
        &self,
        outbound: &Arc<Outbound>,
        state: &mut ConnectionState,
        (window_id, token): (&str, &str),
        door: Door,
        guest: Option<&GuestLineage>,
    ) {
        if let Door::Hook {
            profile,
            name,
            input,
        } = door
        {
            let request = super::hook::HookRequest {
                window_id,
                token,
                profile_id: &profile,
                name: &name,
                input,
                peer: state.peer_process_id,
                guest,
            };
            self.handle_run_hook(outbound, request, HookRead::Door(&state.held_read))
                .await;
            return;
        }
        let Some(entry) = self
            .authenticate_window(
                outbound,
                super::HookDoor::new("door_read", state.peer_process_id)
                    .with_guest(guest)
                    .registering_wait(matches!(door, Door::Wait)),
                token,
                Some(window_id),
            )
            .await
        else {
            return;
        };
        let hook_timeout_sec = Self::window_hook_timeout(&entry, &door);
        match door {
            Door::Context => {
                let intro = self.chat.intro_for(&entry);
                let read = entry.inbox().read_context(intro, None);
                answer_read(&entry, outbound, read, &state.held_read, hook_timeout_sec);
            }
            Door::Take => {
                let read = entry.inbox().read_take(None);
                answer_read(&entry, outbound, read, &state.held_read, hook_timeout_sec);
            }
            Door::Wait => {
                let max_waiters = self.config.max_waiters_per_conn;
                handle_wait(&entry, outbound, state, max_waiters, hook_timeout_sec);
            }
            Door::Hook { .. } => {}
        }
    }
}

/// Answers a wait at once when a parcel can be pushed, and otherwise leaves the
/// wait registered until a parcel arrives, a newer wait replaces it, or the
/// connection ends.
fn handle_wait(
    entry: &Arc<WindowEntry>,
    outbound: &Arc<Outbound>,
    state: &mut ConnectionState,
    max_waiters: usize,
    hook_timeout_sec: Option<u64>,
) {
    if state.live_waiter_count() >= max_waiters {
        outbound.reply(&pending_waits_exhausted_msg());
        return;
    }
    match entry.inbox().wait() {
        Wait::Answered(read) => {
            answer_wait(entry, outbound, *read, &state.held_read, hook_timeout_sec);
        }
        Wait::Registered { guard, answer } => {
            let entry = Arc::clone(entry);
            let outbound = Arc::clone(outbound);
            let slot = Arc::clone(&state.held_read);
            state.waiters.push(tokio::spawn(async move {
                // Dropped with the task, which the connection aborts when it ends.
                let _guard = guard;
                let read = answer.await.unwrap_or_else(|_| DoorRead::silent());
                answer_wait(&entry, &outbound, read, &slot, hook_timeout_sec);
            }));
        }
    }
}

/// [`answer_read`] for a wait. Handing a waiting door a parcel wakes its CLI,
/// which starts a turn now: before the door prints, so before every hook of
/// the turn it wakes.
fn answer_wait(
    entry: &Arc<WindowEntry>,
    outbound: &Outbound,
    read: DoorRead,
    slot: &HeldSlot,
    hook_timeout_sec: Option<u64>,
) {
    let wake = if read.hold.holds() {
        entry.turn().wake_started().map(|number| WakeTurn {
            entry: Arc::downgrade(entry),
            number,
            printed: false,
        })
    } else {
        None
    };
    let DoorRead { message, hold } = read;
    let mut held = HeldRead::new(entry, hold);
    held.wake = wake;
    put_held(slot, held);
    outbound.reply(&timed(message, hook_timeout_sec));
}

/// Answers `report_session`. The value is neither logged nor echoed.
pub(super) fn handle_report_session(entry: &WindowEntry, outbound: &Outbound, session: &str) {
    outbound.reply(&DaemonMessage::SessionRecorded {
        held: entry.record_session_id(session),
    });
}
