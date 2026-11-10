//! Reports from a window to its parent: who the parent is
//! ([`resolve_parent`]), every window's in-memory inbox ([`ReportInboxes`]),
//! and the `send_report` / `wait_for_report` handlers.
//!
//! A task window's parent is its plan's live owner while the window is still
//! bound to the task it was opened for, and it has none once it is not; any
//! other window's parent is the live window that opened it.
//! A task window's report lands both in the plan ledger and in the parent's
//! inbox, or in neither: the inbox slot is reserved before the ledger write,
//! and the report is queued only once that write has succeeded.
//!
//! An inbox with a waiter parked on it is always empty: queueing hands a
//! report straight to the oldest waiter, under the same lock that drains.
//!
//! A report that stays in an empty inbox makes one notice due: a fixed line
//! put in the parent window's own inbox for its CLI, and not again until the
//! report inbox has been emptied. Its count follows the reports that arrive
//! while it waits; emptying the report inbox withdraws it.
//!
//! Each report inbox is the copy of what the store keeps for its window
//! ([`crate::parcel_store`]): a report queued is kept, and a take forgets it
//! once the take is committed ([`TakenReports`]), so a next daemon hands out
//! the reports this one accepted and nobody received. A client of its own
//! commits as it answers; a relay session commits on the relay's receipt. A report
//! sent is shown to no waiter and no door until its row is written, and the
//! reports show in the order they were sent: `send_report` answers for one
//! whose row could not be written by refusing it, and it never shows.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use tokio::sync::oneshot;
use yagura_protocol::{
    AwaitingWindow, DaemonMessage, REPORT_PATH_MAX_BYTES, REPORT_SUMMARY_MAX_BYTES, ReceivedReport,
    ReportOutcome, ReportTaskRef, WindowStatus, validate_handoff_text,
};

use crate::approval::ApprovalBroker;
use crate::config::DaemonConfig;
use crate::control::RequestOrigin;
use crate::inbox::{SaveOutcome, Saving};
use crate::parcel_store::{KeptReports, Written};
use crate::plan_permit::{self, PermitError, TaskBinding};
use crate::plan_store::ParentReportRow;
use crate::pty::lock_recover;
use crate::store::StoreError;
use crate::window::{TaskOrigin, WindowEntry, WindowLiveness, WindowRegistry};

use super::notice_queue;
use super::plan::{self, PlanRuntime};
use super::{ConnectionState, Outbound, error_msg, now_ms};
use super::{RequestWatch, request_left};

/// How many reports one window's inbox holds before `send_report` to it is
/// refused with `resource_exhausted`.
pub const REPORT_INBOX_MAX: usize = 64;

/// The key a report notice is held under in its window's inbox.
const REPORT_NOTICE_KEY: &str = "report";

/// What one `send_report` request carries.
pub(super) struct ReportBody {
    pub(super) outcome: ReportOutcome,
    pub(super) summary: String,
    pub(super) path: Option<String>,
}

/// Every window's inbox of received reports, together with the
/// `wait_for_report` waiters parked on an empty one. The queued reports are
/// kept in the registry's store when it has one.
pub struct ReportInboxes {
    registry: Arc<WindowRegistry>,
    /// Held across every change to a notice's state and the matching deposit,
    /// revision or withdrawal in the window's own inbox, so the two never
    /// disagree. Taken before that inbox's lock, which never calls back here.
    inboxes: Mutex<HashMap<String, Inbox>>,
    next_waiter_id: AtomicU64,
    /// Numbers every inbox's stretches. Kept outside the inboxes because an
    /// idle inbox is dropped and recreated, which would reuse its numbers.
    next_stretch: AtomicU64,
    /// Each window's places in the store, kept outside the inboxes for the
    /// same reason: a place given once is never given again while the
    /// window's entry lives, so a row is only ever written and forgotten for
    /// one report. Taken under the inboxes' lock, and takes no other.
    places: Mutex<HashMap<String, Places>>,
    /// What the store kept for each window taken back as the daemon starts,
    /// until the window's entry is built ([`Self::bind_taken_back`]). Taken
    /// under the inboxes' lock, before the places' lock.
    unbound: Mutex<HashMap<String, KeptReports>>,
}

#[derive(Default)]
struct Inbox {
    /// The reports shown, oldest first, each at its place in the store.
    reports: Vec<(i64, ReceivedReport)>,
    /// The reports sent whose rows are being written, by place: each shows
    /// once it and every older one here are settled.
    writing: BTreeMap<i64, Writing>,
    /// Slots promised to a `send_report` whose ledger write is still running.
    reserved: usize,
    /// Reports taken and neither committed nor put back yet. Their rows and
    /// places are still theirs.
    out: usize,
    waiters: VecDeque<(u64, oneshot::Sender<TakenReports>)>,
    notice: Notice,
    /// The current non-empty stretch's number, unique across the inbox being
    /// recreated, so a notice written for one stretch is never recorded
    /// against a later one.
    stretch: u64,
    /// The stretch whose notice was put in a window entry's inbox, and that
    /// entry: a window built again over the same holder has an inbox of its
    /// own, which the notice is put in once more.
    noticed: Option<(u64, Weak<WindowEntry>)>,
}

impl Inbox {
    /// Whether this stretch's notice is yet to be put in `entry`'s inbox.
    fn notice_owed_to(&self, entry: &Arc<WindowEntry>) -> bool {
        self.notice == Notice::Due
            && !self.noticed.as_ref().is_some_and(|(stretch, noticed)| {
                *stretch == self.stretch && std::ptr::eq(noticed.as_ptr(), Arc::as_ptr(entry))
            })
    }

    /// Puts this stretch's notice in `entry`'s inbox.
    fn notice(&mut self, entry: &Arc<WindowEntry>) -> (crate::inbox::Deposited, u64) {
        self.noticed = Some((self.stretch, Arc::downgrade(entry)));
        (deposit_notice(entry, self.reports.len()), self.stretch)
    }

    fn is_idle(&self) -> bool {
        self.reports.is_empty()
            && self.writing.is_empty()
            && self.reserved == 0
            && self.out == 0
            && self.waiters.is_empty()
    }
}

/// The places one window's reports have been given in the store.
struct Places {
    /// The window they number, whose entry going lets them go: its id is
    /// never reused, and no row is written for a window without one.
    entry: Weak<WindowEntry>,
    /// The lowest place given, which a report put ahead goes below.
    first_seq: i64,
    /// The place the next report queued behind the others takes.
    next_seq: i64,
}

impl Places {
    /// `count` places ahead of every one given when `older`, behind them
    /// otherwise.
    fn give(&mut self, count: usize, older: bool) -> Vec<i64> {
        let count = i64::try_from(count).unwrap_or(i64::MAX);
        let start = if older {
            self.first_seq = self.first_seq.saturating_sub(count);
            self.first_seq
        } else {
            let start = self.next_seq;
            self.next_seq = self.next_seq.saturating_add(count);
            start
        };
        (0..count)
            .map(|offset| start.saturating_add(offset))
            .collect()
    }
}

/// Where the inbox's current non-empty stretch stands on its notice.
#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum Notice {
    /// The inbox is empty.
    #[default]
    Lowered,
    /// Reports are waiting and no notice has been typed for them yet.
    Due,
    /// This stretch's one notice has been written; none until it is emptied.
    Typed,
}

/// A report whose row is being written.
struct Writing {
    report: ReceivedReport,
    /// How its write came out, once told.
    outcome: Option<SaveOutcome>,
}

/// What [`ReportInboxes::take_or_wait`] decided.
enum Taken {
    Ready(TakenReports),
    Waiting(WaitRegistration),
    NoWaiterBudget,
}

impl ReportInboxes {
    /// Empty inboxes for the windows of `registry`.
    #[must_use]
    pub fn new(registry: Arc<WindowRegistry>) -> Self {
        Self {
            registry,
            inboxes: Mutex::new(HashMap::new()),
            next_waiter_id: AtomicU64::new(0),
            next_stretch: AtomicU64::new(0),
            places: Mutex::new(HashMap::new()),
            unbound: Mutex::new(HashMap::new()),
        }
    }

    /// The places `count` reports for `entry`'s window take in the store:
    /// ahead of every place given when `older`, behind them otherwise. A
    /// window whose entry is gone keeps nothing, so it is numbered from
    /// nothing. Called under the inboxes' lock.
    fn places(
        &self,
        window_id: &str,
        entry: Option<&Arc<WindowEntry>>,
        count: usize,
        older: bool,
    ) -> Vec<i64> {
        let Some(entry) = entry else {
            let mut unkept = Places {
                entry: Weak::new(),
                first_seq: 0,
                next_seq: 0,
            };
            return unkept.give(count, older);
        };
        let mut places = lock_recover(&self.places);
        if !places.contains_key(window_id) {
            places.retain(|_, places| places.entry.strong_count() > 0);
        }
        places
            .entry(window_id.to_owned())
            .or_insert_with(|| Places {
                entry: Arc::downgrade(entry),
                first_seq: 0,
                next_seq: 0,
            })
            .give(count, older)
    }

    /// Promises `window_id` one more report, or `None` when its inbox is
    /// already full.
    fn reserve(self: &Arc<Self>, window_id: &str) -> Option<InboxSlot> {
        let mut inboxes = lock_recover(&self.inboxes);
        let inbox = inboxes.entry(window_id.to_string()).or_default();
        if inbox.reports.len() + inbox.writing.len() + inbox.reserved + inbox.out
            >= REPORT_INBOX_MAX
        {
            return None;
        }
        inbox.reserved += 1;
        Some(InboxSlot {
            inboxes: Arc::clone(self),
            window_id: window_id.to_string(),
            held: true,
        })
    }

    /// Hands `reports` to `window_id`'s oldest waiter, or queues them, at
    /// once: they were accepted before (a waiter that could not take them
    /// after all hands them back), or the inbox keeps nothing. `older` puts
    /// them ahead of what is already queued.
    fn put(
        self: &Arc<Self>,
        window_id: &str,
        reports: Vec<ReceivedReport>,
        from_slot: bool,
        older: bool,
    ) {
        // Looked up before the inboxes' lock, which the registry's is never
        // taken under.
        let entry = self.registry.get(window_id);
        let store = self.registry.parcel_store().filter(|_| entry.is_some());
        let deposited = {
            let mut inboxes = lock_recover(&self.inboxes);
            let inbox = inboxes.entry(window_id.to_string()).or_default();
            if from_slot {
                inbox.reserved = inbox.reserved.saturating_sub(1);
            }
            let places = self.places(window_id, entry.as_ref(), reports.len(), older);
            // Kept under the same lock a take forgets them under, so the
            // store follows the inbox's order. Kept even for a waiter, whose
            // take may not be committed. Shown whatever the write says:
            // nothing waits on it.
            if let Some(store) = &store {
                let kept: Vec<(i64, &ReceivedReport)> =
                    places.iter().copied().zip(&reports).collect();
                drop(store.keep_reports(window_id, &kept));
            }
            let placed = places.into_iter().zip(reports).collect();
            let Some(reports) = self.hand_to_waiter(inbox, window_id, placed) else {
                return;
            };
            self.queue(inbox, entry.as_ref(), reports)
        };
        self.after_queue(window_id, entry, deposited);
    }

    /// Gives back the slot reserved for `report`, gives it the next place and
    /// holds it in `writing` under that place. `keep` runs under the same lock
    /// with the place, before the report is held.
    fn hold_place<R>(
        &self,
        window_id: &str,
        entry: Option<&Arc<WindowEntry>>,
        report: ReceivedReport,
        keep: impl FnOnce(i64, &ReceivedReport) -> R,
    ) -> (i64, R) {
        let mut inboxes = lock_recover(&self.inboxes);
        let inbox = inboxes.entry(window_id.to_string()).or_default();
        inbox.reserved = inbox.reserved.saturating_sub(1);
        let seq = self
            .places(window_id, entry, 1, false)
            .first()
            .copied()
            .unwrap_or_default();
        let kept = keep(seq, &report);
        inbox.writing.insert(
            seq,
            Writing {
                report,
                outcome: None,
            },
        );
        (seq, kept)
    }

    /// Puts `report`, the one a slot was reserved for, in `window_id`'s
    /// inbox behind the others. With a store it shows only once its row is
    /// written, and only after every report sent before it is settled; the
    /// returned [`Saving`] tells its sender how the write came out.
    fn put_saved(self: &Arc<Self>, window_id: &str, report: ReceivedReport) -> Saving {
        let entry = self.registry.get(window_id);
        let Some(store) = self.registry.parcel_store().filter(|_| entry.is_some()) else {
            self.put(window_id, vec![report], true, false);
            return Saving::ready();
        };
        let (seq, written) = self.hold_place(window_id, entry.as_ref(), report, |seq, report| {
            store.keep_reports(window_id, &[(seq, report)])
        });
        let (told, outcome) = oneshot::channel();
        let inboxes = Arc::clone(self);
        let window_id = window_id.to_owned();
        // Settled by a task of its own, so a sender that goes away cannot
        // leave the reports behind this one held back.
        tokio::spawn(async move {
            let outcome = SaveOutcome::of(written.await.unwrap_or(Written::Failed));
            inboxes.settle_write(&window_id, seq, outcome);
            let _ = told.send(outcome);
        });
        Saving::of(outcome)
    }

    /// [`Self::put_saved`] for a caller that writes the row itself: decides
    /// the report's place and holds it back, until [`Placing::finish`].
    fn place_saved(self: &Arc<Self>, window_id: &str, report: ReceivedReport) -> Placing {
        let entry = self.registry.get(window_id);
        if self
            .registry
            .parcel_store()
            .filter(|_| entry.is_some())
            .is_none()
        {
            return Placing::Memory {
                inboxes: Arc::clone(self),
                window_id: window_id.to_owned(),
                report: Some(report),
            };
        }
        let report_json = serde_json::to_string(&report).ok();
        let (seq, ()) = self.hold_place(window_id, entry.as_ref(), report, |_, _| ());
        Placing::Kept {
            inboxes: Arc::clone(self),
            window_id: window_id.to_owned(),
            seq,
            report_json,
            settled: false,
        }
    }

    /// Records how the write of the report at `seq` came out, and shows every
    /// report whose turn that settles: a report whose row was not written
    /// leaves, and holds nothing back.
    fn settle_write(self: &Arc<Self>, window_id: &str, seq: i64, outcome: SaveOutcome) {
        let entry = self.registry.get(window_id);
        let deposited = {
            let mut inboxes = lock_recover(&self.inboxes);
            let Some(inbox) = inboxes.get_mut(window_id) else {
                return;
            };
            if let Some(writing) = inbox.writing.get_mut(&seq) {
                writing.outcome = Some(outcome);
            }
            let mut settled = Vec::new();
            while let Some(oldest) = inbox.writing.first_entry() {
                let Some(outcome) = oldest.get().outcome else {
                    break;
                };
                let (seq, writing) = oldest.remove_entry();
                if outcome == SaveOutcome::Saved {
                    settled.push((seq, writing.report));
                }
            }
            if settled.is_empty() {
                if inbox.is_idle() {
                    inboxes.remove(window_id);
                }
                return;
            }
            let Some(reports) = self.hand_to_waiter(inbox, window_id, settled) else {
                return;
            };
            self.queue(inbox, entry.as_ref(), reports)
        };
        self.after_queue(window_id, entry, deposited);
    }

    /// Queues `reports` in `inbox`, each in the order of its place, and makes
    /// or revises the notice of them. Called under the inboxes' lock, so the
    /// notice and the notice state never disagree.
    fn queue(
        &self,
        inbox: &mut Inbox,
        entry: Option<&Arc<WindowEntry>>,
        reports: Vec<(i64, ReceivedReport)>,
    ) -> Option<(crate::inbox::Deposited, u64)> {
        for (seq, report) in reports {
            let at = inbox.reports.partition_point(|(queued, _)| *queued < seq);
            inbox.reports.insert(at, (seq, report));
        }
        let fell_due = inbox.notice == Notice::Lowered && !inbox.reports.is_empty();
        if fell_due {
            inbox.notice = Notice::Due;
            inbox.stretch = self.next_stretch.fetch_add(1, Ordering::Relaxed);
        }
        let pending = inbox.reports.len();
        // A window that declares no delivery is handed no notice: its
        // reports stay readable through `wait_for_report`.
        match entry.filter(|entry| entry.delivers()) {
            Some(entry) if inbox.notice_owed_to(entry) => Some(inbox.notice(entry)),
            Some(entry) if inbox.notice == Notice::Due => {
                // The notice still waiting tells the count as it is now.
                let _ = entry
                    .inbox()
                    .revise(REPORT_NOTICE_KEY, notice_text(pending));
                None
            }
            _ => None,
        }
    }

    /// What follows queueing reports for `window_id`, outside the inboxes'
    /// lock: a notice deposited is confirmed once taken, and the window's
    /// agent is woken.
    fn after_queue(
        self: &Arc<Self>,
        window_id: &str,
        entry: Option<Arc<WindowEntry>>,
        deposited: Option<(crate::inbox::Deposited, u64)>,
    ) {
        let Some(entry) = entry else {
            // Window ids are never reused, so nobody could read it again.
            self.discard(window_id);
            return;
        };
        if let Some((deposited, stretch)) = deposited {
            self.confirm_when_taken(&entry, deposited, stretch);
        }
        // Woken even for a window that is handed no notice: its agent
        // discards this inbox once the window ends.
        self.registry.wake_notices(&entry);
    }

    /// Records `stretch`'s notice as delivered once the CLI took it.
    fn confirm_when_taken(
        self: &Arc<Self>,
        entry: &Arc<WindowEntry>,
        deposited: crate::inbox::Deposited,
        stretch: u64,
    ) {
        let Ok(end) = notice_queue::after_deposit(&self.registry, entry, deposited) else {
            return;
        };
        let inboxes = Arc::clone(self);
        let window_id = entry.window_id().to_string();
        tokio::spawn(async move {
            if end.await == Ok(crate::inbox::ParcelEnd::Taken) {
                inboxes.confirm_notice(&window_id, stretch);
            }
        });
    }

    /// Gives back a reservation that was never used.
    fn release(&self, window_id: &str) {
        let mut inboxes = lock_recover(&self.inboxes);
        if let Some(inbox) = inboxes.get_mut(window_id) {
            inbox.reserved = inbox.reserved.saturating_sub(1);
            if inbox.is_idle() {
                inboxes.remove(window_id);
            }
        }
    }

    /// Takes what `window_id`'s inbox queues, uncommitted ([`TakenReports`]),
    /// or -- when it is empty, waiting is asked
    /// for and the connection may hold one more wait -- parks a waiter on it.
    /// One lock covers both, so a report can never slip into the inbox
    /// between the emptiness check and the waiter's registration.
    fn take_or_wait(self: &Arc<Self>, window_id: &str, may_wait: bool, budget_left: bool) -> Taken {
        let entry = self.registry.get(window_id);
        let mut inboxes = lock_recover(&self.inboxes);
        if let Some(inbox) = inboxes.get_mut(window_id)
            && !inbox.reports.is_empty()
        {
            let reports = std::mem::take(&mut inbox.reports);
            inbox.out += reports.len();
            inbox.notice = Notice::Lowered;
            // The reports it announced were read.
            if let Some(entry) = &entry {
                let _ = entry.inbox().withdraw(REPORT_NOTICE_KEY);
            }
            return Taken::Ready(self.taken(window_id, reports));
        }
        if !may_wait {
            return Taken::Ready(self.taken(window_id, Vec::new()));
        }
        if !budget_left {
            return Taken::NoWaiterBudget;
        }
        let id = self.next_waiter_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        inboxes
            .entry(window_id.to_string())
            .or_default()
            .waiters
            .push_back((id, tx));
        Taken::Waiting(WaitRegistration {
            inboxes: Arc::clone(self),
            window_id: window_id.to_string(),
            id,
            rx,
            settled: false,
        })
    }

    /// `reports`, counted out of `window_id`'s inbox, as a take.
    fn taken(
        self: &Arc<Self>,
        window_id: &str,
        reports: Vec<(i64, ReceivedReport)>,
    ) -> TakenReports {
        TakenReports {
            inboxes: Arc::clone(self),
            window_id: window_id.to_owned(),
            reports,
        }
    }

    /// Offers `reports` to the waiters in arrival order, returning them when
    /// no waiter is still listening. Called under the inboxes' lock.
    fn hand_to_waiter(
        self: &Arc<Self>,
        inbox: &mut Inbox,
        window_id: &str,
        mut reports: Vec<(i64, ReceivedReport)>,
    ) -> Option<Vec<(i64, ReceivedReport)>> {
        while let Some((_, waiter)) = inbox.waiters.pop_front() {
            let count = reports.len();
            inbox.out += count;
            match waiter.send(self.taken(window_id, reports)) {
                Ok(()) => return None,
                Err(mut returned) => {
                    // Disarmed here: dropping it would take this same lock.
                    inbox.out = inbox.out.saturating_sub(count);
                    reports = returned.disarm();
                }
            }
        }
        Some(reports)
    }

    /// Puts `reports`, taken and not committed, back in `window_id`'s inbox
    /// at their own places: ahead of every report that came after them, and
    /// their rows untouched.
    fn put_back(self: &Arc<Self>, window_id: &str, reports: Vec<(i64, ReceivedReport)>) {
        let entry = self.registry.get(window_id);
        let deposited = {
            let mut inboxes = lock_recover(&self.inboxes);
            let inbox = inboxes.entry(window_id.to_string()).or_default();
            inbox.out = inbox.out.saturating_sub(reports.len());
            let Some(reports) = self.hand_to_waiter(inbox, window_id, reports) else {
                return;
            };
            self.queue(inbox, entry.as_ref(), reports)
        };
        self.after_queue(window_id, entry, deposited);
    }

    /// Forgets the rows of the reports a committed take carried.
    fn commit_taken(&self, window_id: &str, seqs: Vec<i64>) {
        // Sent once the rows were written, as a report shows only then; and
        // no place is given twice, so this never reaches another report's row.
        if let Some(store) = self.registry.parcel_store() {
            store.forget_reports(window_id, seqs.clone());
        }
        let mut inboxes = lock_recover(&self.inboxes);
        if let Some(inbox) = inboxes.get_mut(window_id) {
            inbox.out = inbox.out.saturating_sub(seqs.len());
            if inbox.is_idle() {
                inboxes.remove(window_id);
            }
        }
    }

    /// Drops everything queued for `window_id`, whose window can no longer
    /// read it. What the store keeps goes with the window's row: a window
    /// whose holder was lost, or whose daemon stopped, keeps it.
    pub(super) fn discard(&self, window_id: &str) {
        let mut inboxes = lock_recover(&self.inboxes);
        if let Some(inbox) = inboxes.get_mut(window_id) {
            inbox.reports.clear();
            inbox.writing.clear();
            inbox.notice = Notice::Lowered;
            if inbox.is_idle() {
                inboxes.remove(window_id);
            }
        }
    }

    /// Holds what the store kept for the windows taken back as the daemon
    /// starts, each until its window's entry is built.
    pub(super) fn keep_unbound(&self, kept: Vec<(String, KeptReports)>) {
        lock_recover(&self.unbound).extend(kept);
    }

    /// Ties `entry`, a window taken back just built, to its reports: its
    /// places follow it, the reports the store kept for it are queued the
    /// first time, and a window that is handed notices gets one for them.
    /// The notice is not kept: it is derived from the reports, as when they
    /// first arrived. A window built again over the same holder (a
    /// placeholder made whole) is tied again, keeping every place given.
    pub(super) fn bind_taken_back(self: &Arc<Self>, entry: &Arc<WindowEntry>) {
        let window_id = entry.window_id();
        let deposited = {
            let mut inboxes = lock_recover(&self.inboxes);
            let kept = lock_recover(&self.unbound).remove(window_id);
            {
                let mut places = lock_recover(&self.places);
                match (
                    places.get_mut(window_id),
                    kept.as_ref().and_then(|kept| kept.places),
                ) {
                    (Some(places), _) => places.entry = Arc::downgrade(entry),
                    (None, Some((first_seq, last_seq))) => {
                        places.insert(
                            window_id.to_owned(),
                            Places {
                                entry: Arc::downgrade(entry),
                                first_seq,
                                next_seq: last_seq.saturating_add(1),
                            },
                        );
                    }
                    (None, None) => {}
                }
            }
            if let Some(kept) =
                kept.filter(|kept| kept.places.is_some() && !kept.reports.is_empty())
            {
                let inbox = inboxes.entry(window_id.to_owned()).or_default();
                if inbox.reports.is_empty() {
                    inbox.reports = kept
                        .reports
                        .into_iter()
                        .map(|kept| (kept.seq, kept.report))
                        .collect();
                    inbox.notice = Notice::Due;
                    inbox.stretch = self.next_stretch.fetch_add(1, Ordering::Relaxed);
                }
            }
            // A placeholder delivers nothing; the window built from its
            // values is handed the notice.
            inboxes
                .get_mut(window_id)
                .filter(|inbox| entry.delivers() && inbox.notice_owed_to(entry))
                .map(|inbox| inbox.notice(entry))
        };
        if let Some((deposited, stretch)) = deposited {
            self.confirm_when_taken(entry, deposited, stretch);
        }
    }

    #[cfg(test)]
    fn notice_due(&self, window_id: &str) -> bool {
        lock_recover(&self.inboxes)
            .get(window_id)
            .is_some_and(|inbox| inbox.notice == Notice::Due)
    }

    /// `window_id`'s notice state, `Some(None)` when it has no inbox, or
    /// `None` while another thread holds the inboxes' lock.
    #[cfg(test)]
    fn try_notice(&self, window_id: &str) -> Option<Option<Notice>> {
        let inboxes = match self.inboxes.try_lock() {
            Ok(inboxes) => inboxes,
            Err(std::sync::TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => return None,
        };
        Some(inboxes.get(window_id).map(|inbox| inbox.notice))
    }

    /// The text of `window_id`'s due notice and the stretch it belongs to,
    /// leaving it due; `None` when none is due or the inbox is empty.
    #[cfg(test)]
    fn due_notice(&self, window_id: &str) -> Option<(String, u64)> {
        let inboxes = lock_recover(&self.inboxes);
        let inbox = inboxes.get(window_id)?;
        if inbox.notice != Notice::Due || inbox.reports.is_empty() {
            return None;
        }
        Some((notice_text(inbox.reports.len()), inbox.stretch))
    }

    /// Records that `stretch`'s notice was written, so that stretch gets no
    /// other. A no-op once the inbox has been emptied since.
    fn confirm_notice(&self, window_id: &str, stretch: u64) {
        let mut inboxes = lock_recover(&self.inboxes);
        if let Some(inbox) = inboxes.get_mut(window_id)
            && inbox.notice == Notice::Due
            && inbox.stretch == stretch
        {
            inbox.notice = Notice::Typed;
        }
    }
}

impl std::fmt::Debug for ReportInboxes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReportInboxes")
            .field("inboxes", &lock_recover(&self.inboxes).len())
            .finish_non_exhaustive()
    }
}

/// Puts the notice of `pending` reports in `entry`'s inbox. Called under the
/// report inboxes' lock, so the notice and the notice state never disagree.
fn deposit_notice(entry: &WindowEntry, pending: usize) -> crate::inbox::Deposited {
    // Derived from the reports, which are kept, so the notice itself is not.
    let parcel = crate::inbox::Parcel::notice(REPORT_NOTICE_KEY, notice_text(pending)).unsaved();
    entry.inbox().deposit_tracked(parcel, None, None)
}

/// The one line handed to a parent with `pending` reports waiting. Only the
/// count varies, so nothing a child wrote ever reaches the parent's CLI.
fn notice_text(pending: usize) -> String {
    format!("[yagura report] {pending} pending report(s). Call wait_for_report to read them.")
}

/// One reserved place in a parent's inbox. Dropped unused, it gives the
/// place back.
struct InboxSlot {
    inboxes: Arc<ReportInboxes>,
    window_id: String,
    held: bool,
}

impl InboxSlot {
    /// Puts `report` in the reserved place; it shows once its row is
    /// written, which the returned [`Saving`] tells.
    fn deliver(mut self, report: ReceivedReport) -> Saving {
        self.held = false;
        self.inboxes.put_saved(&self.window_id, report)
    }

    /// Decides the place `report` takes behind the others, for a caller that
    /// writes the report's row itself, in the transaction that writes what
    /// the report is for.
    fn place(mut self, report: ReceivedReport) -> Placing {
        self.held = false;
        self.inboxes.place_saved(&self.window_id, report)
    }
}

impl Drop for InboxSlot {
    fn drop(&mut self) {
        if self.held {
            self.inboxes.release(&self.window_id);
        }
    }
}

/// A report on its way into its parent's inbox, whose row the caller writes
/// itself. Dropped before [`Placing::finish`], it counts as a write that
/// failed: the report never shows.
enum Placing {
    /// The inbox keeps its reports: the report holds its place until its row
    /// is written.
    Kept {
        inboxes: Arc<ReportInboxes>,
        window_id: String,
        seq: i64,
        /// `None` when the report could not be encoded to keep.
        report_json: Option<String>,
        settled: bool,
    },
    /// The inbox keeps nothing: the report shows at once.
    Memory {
        inboxes: Arc<ReportInboxes>,
        window_id: String,
        report: Option<ReceivedReport>,
    },
}

impl Placing {
    /// The row to write, when the report is kept.
    fn row(&self) -> Option<ParentReportRow<'_>> {
        match self {
            Self::Kept {
                window_id,
                seq,
                report_json: Some(report_json),
                ..
            } => Some(ParentReportRow {
                window_id,
                seq: *seq,
                report_json,
            }),
            _ => None,
        }
    }

    /// Tells how many rows the report's row came to once written: the report
    /// shows, or, its parent having no row to keep it with, leaves.
    fn finish(mut self, rows: usize) -> SaveOutcome {
        match &mut self {
            Self::Kept {
                inboxes,
                window_id,
                seq,
                settled,
                ..
            } => {
                *settled = true;
                let outcome = SaveOutcome::of(Written::Rows(rows));
                inboxes.settle_write(window_id, *seq, outcome);
                outcome
            }
            Self::Memory {
                inboxes,
                window_id,
                report,
            } => {
                if let Some(report) = report.take() {
                    inboxes.put(window_id, vec![report], true, false);
                }
                SaveOutcome::Saved
            }
        }
    }
}

impl Drop for Placing {
    fn drop(&mut self) {
        match self {
            Self::Kept {
                inboxes,
                window_id,
                seq,
                settled: false,
                ..
            } => inboxes.settle_write(window_id, *seq, SaveOutcome::Failed),
            Self::Memory {
                inboxes,
                window_id,
                report: Some(_),
            } => inboxes.release(window_id),
            _ => {}
        }
    }
}

/// Reports taken out of a window's inbox whose rows are still kept.
/// Committed ([`TakenReports::commit`]), their rows go; dropped, they go back
/// to the inbox at their own places, so a take that reaches nobody loses
/// nothing.
pub(crate) struct TakenReports {
    inboxes: Arc<ReportInboxes>,
    window_id: String,
    reports: Vec<(i64, ReceivedReport)>,
}

impl TakenReports {
    fn is_empty(&self) -> bool {
        self.reports.is_empty()
    }

    /// The reports, as an answer carries them.
    fn reports(&self) -> Vec<ReceivedReport> {
        self.reports
            .iter()
            .map(|(_, report)| report.clone())
            .collect()
    }

    /// Forgets the reports' rows: they were received.
    pub(crate) fn commit(mut self) {
        let seqs = self.disarm().into_iter().map(|(seq, _)| seq).collect();
        self.inboxes.commit_taken(&self.window_id, seqs);
    }

    /// [`Self::commit`], returning the reports.
    fn into_committed(mut self) -> Vec<ReceivedReport> {
        let (seqs, reports) = self.disarm().into_iter().unzip();
        self.inboxes.commit_taken(&self.window_id, seqs);
        reports
    }

    /// Takes the reports out, so dropping this puts nothing back.
    fn disarm(&mut self) -> Vec<(i64, ReceivedReport)> {
        std::mem::take(&mut self.reports)
    }
}

impl Drop for TakenReports {
    fn drop(&mut self) {
        if !self.reports.is_empty() {
            let reports = self.disarm();
            self.inboxes.put_back(&self.window_id, reports);
        }
    }
}

/// Where a relay session's `wait_for_report` hands the reports it took: to
/// the session's request, uncommitted, for the relay's receipt to commit.
pub(crate) struct ReportReceipt(oneshot::Sender<TakenReports>);

impl ReportReceipt {
    /// A receipt and the end its request reads the take from.
    pub(crate) fn channel() -> (Self, oneshot::Receiver<TakenReports>) {
        let (tx, rx) = oneshot::channel();
        (Self(tx), rx)
    }

    /// Hands `taken` to the request; put back when the request is gone.
    fn hand(self, taken: TakenReports) {
        drop(self.0.send(taken));
    }
}

/// A `wait_for_report` parked on an empty inbox. Dropped before it settles
/// (its task aborted because the connection ended), it withdraws itself and
/// puts back anything already handed to it, so an abort racing a hand-off
/// never loses a report.
struct WaitRegistration {
    inboxes: Arc<ReportInboxes>,
    window_id: String,
    id: u64,
    rx: oneshot::Receiver<TakenReports>,
    settled: bool,
}

impl WaitRegistration {
    async fn wait(mut self, timeout: Duration) -> Option<TakenReports> {
        let received = tokio::time::timeout(timeout, &mut self.rx).await;
        self.settled = true;
        match received {
            Ok(Ok(taken)) => Some(taken),
            Ok(Err(_)) | Err(_) => self.withdraw(),
        }
    }

    /// Leaves the waiter queue, returning whatever was handed over before
    /// it did. Read under the inbox lock, so no hand-off can land after it.
    fn withdraw(&mut self) -> Option<TakenReports> {
        let mut inboxes = lock_recover(&self.inboxes.inboxes);
        if let Some(inbox) = inboxes.get_mut(&self.window_id) {
            inbox.waiters.retain(|(id, _)| *id != self.id);
            if inbox.is_idle() {
                inboxes.remove(&self.window_id);
            }
        }
        self.rx.try_recv().ok()
    }
}

impl Drop for WaitRegistration {
    fn drop(&mut self) {
        if !self.settled {
            // Dropped after the inboxes' lock is left, which puts it back.
            drop(self.withdraw());
        }
    }
}

/// The window `window_id` reports to right now. A task window reports to
/// its plan's live owner while it is still bound to the task it was opened
/// for ([`plan_permit::check_task_binding`],
/// [`plan_permit::resolve_owner_parent`]) and to no one once it is not, the
/// same way `send_report` refuses its report; any other window reports to
/// the live window that opened it. `None` when there is no such window.
///
/// Reads the ledger without the plan's lock, so the answer can be stale by
/// the time it is used; `send_report` re-decides under the lock instead.
pub async fn resolve_parent(
    ledger: &PlanRuntime,
    registry: &Arc<WindowRegistry>,
    window_id: &str,
) -> Option<String> {
    let entry = registry.get(window_id)?;
    let Some(origin) = entry.task_origin() else {
        return live_opener(registry, &entry);
    };
    let plan_id = origin.plan_id.clone();
    let task_id = origin.task_id.clone();
    let reporter = window_id.to_string();
    let registry = Arc::clone(registry);
    ledger
        .with_store(move |store| {
            match plan_permit::check_task_binding(store, &plan_id, &task_id, &reporter) {
                TaskBinding::Bound => plan_permit::resolve_owner_parent(store, &registry, &plan_id),
                TaskBinding::NotBound | TaskBinding::PlanNotFound => None,
            }
        })
        .await
        .ok()
        .flatten()
}

fn live_opener(registry: &WindowRegistry, entry: &WindowEntry) -> Option<String> {
    entry
        .opener()
        .filter(|opener| matches!(registry.liveness(opener), WindowLiveness::Alive(_)))
        .map(str::to_string)
}

/// Answers `send_report`: origin, then the arguments, then delivery to the
/// parent (and, for a task window, the ledger) as the module doc describes.
pub(super) async fn handle_send_report(
    inboxes: &Arc<ReportInboxes>,
    ledger: &PlanRuntime,
    registry: &Arc<WindowRegistry>,
    broker: &Arc<ApprovalBroker>,
    outbound: &Outbound,
    state: &ConnectionState,
    body: ReportBody,
) {
    let RequestOrigin::Window(window_id) = &state.origin else {
        outbound.reply(&not_a_window_msg());
        return;
    };
    if let Err(message) = validate_body(&body) {
        outbound.reply(&message);
        return;
    }
    let Some(entry) = registry.get(window_id) else {
        outbound.reply(&no_parent_msg());
        return;
    };
    let reply = match entry.task_origin().cloned() {
        Some(origin) => {
            send_task_report(inboxes, ledger, registry, broker, &entry, &origin, body).await
        }
        None => send_opener_report(inboxes, registry, &entry, &body).await,
    };
    outbound.reply(&reply);
}

// The error is the wire reply itself, as in `IpcServer::plan_from_profile`.
#[allow(clippy::result_large_err)]
fn validate_body(body: &ReportBody) -> Result<(), DaemonMessage> {
    validate_handoff_text(&body.summary, REPORT_SUMMARY_MAX_BYTES)
        .map_err(|err| error_msg("invalid_argument", &format!("summary: {err}")))?;
    if let Some(path) = body.path.as_deref() {
        validate_handoff_text(path, REPORT_PATH_MAX_BYTES)
            .map_err(|err| error_msg("invalid_argument", &format!("path: {err}")))?;
    }
    Ok(())
}

async fn send_opener_report(
    inboxes: &Arc<ReportInboxes>,
    registry: &WindowRegistry,
    entry: &WindowEntry,
    body: &ReportBody,
) -> DaemonMessage {
    let Some(parent) = live_opener(registry, entry) else {
        return no_parent_msg();
    };
    let Some(slot) = inboxes.reserve(&parent) else {
        return inbox_full_msg();
    };
    let saving = slot.deliver(received_report(entry, body, None));
    match saving.outcome().await {
        SaveOutcome::Saved => DaemonMessage::ReportSent {
            delivered_to: Some(parent),
            task: None,
        },
        // The parent's row is gone: it is closing.
        SaveOutcome::NoRow => no_parent_msg(),
        SaveOutcome::Failed => {
            tracing::warn!("a report's row could not be written; it is refused");
            plan::internal_msg()
        }
    }
}

/// Why a task window's report was refused under the plan's lock.
enum TaskReportRefusal {
    Permit(PermitError),
    InboxFull,
    Write(StoreError),
}

async fn send_task_report(
    inboxes: &Arc<ReportInboxes>,
    ledger: &PlanRuntime,
    registry: &Arc<WindowRegistry>,
    broker: &Arc<ApprovalBroker>,
    entry: &WindowEntry,
    origin: &TaskOrigin,
    body: ReportBody,
) -> DaemonMessage {
    let plan_id = origin.plan_id.clone();
    // Checked before locking: locking an id creates its lock-table entry.
    match plan::plan_exists(ledger, &plan_id).await {
        Ok(true) => {}
        Ok(false) => return unbound_msg(),
        Err(message) => return message,
    }

    let task = ReportTaskRef {
        plan_id: plan_id.clone(),
        task_id: origin.task_id.clone(),
    };
    let report = received_report(entry, &body, Some(task.clone()));
    let guard = ledger.locks().lock(&plan_id).await;
    let reporter = RequestOrigin::Window(entry.window_id().to_string());
    let task_id = task.task_id.clone();
    let registry_in_store = Arc::clone(registry);
    let inboxes_in_store = Arc::clone(inboxes);
    let ReportBody {
        outcome,
        summary,
        path,
    } = body;
    let decided = ledger
        .with_store(move |store| {
            let permit = plan_permit::issue_report(&reporter, &task_id, guard, store)
                .map_err(TaskReportRefusal::Permit)?;
            let parent =
                plan_permit::resolve_owner_parent(store, &registry_in_store, permit.plan_id());
            let placing = match parent.as_deref() {
                Some(parent) => Some(
                    inboxes_in_store
                        .reserve(parent)
                        .ok_or(TaskReportRefusal::InboxFull)?
                        .place(report),
                ),
                None => None,
            };
            // The report and its parent's row are one transaction: a daemon
            // that ends between them holds both or neither.
            let rows = store
                .write_task_report_kept(
                    &permit,
                    outcome,
                    &summary,
                    path.as_deref(),
                    placing.as_ref().and_then(Placing::row),
                    || {
                        #[cfg(feature = "fault-injection")]
                        registry_in_store
                            .config()
                            .faults
                            .reach(crate::fault_point::FaultPoint::TaskReportHalfWritten);
                    },
                )
                .map_err(TaskReportRefusal::Write)?;
            let parent = match placing.map(|placing| placing.finish(rows)) {
                // The parent's row is gone: the ledger alone holds it.
                Some(SaveOutcome::NoRow) | None => None,
                Some(_) => parent,
            };
            Ok(parent)
        })
        .await;

    match decided {
        Ok(Ok(parent)) => {
            broker.notify_approvers(&DaemonMessage::PlanChanged {
                plan_id: plan_id.clone(),
            });
            DaemonMessage::ReportSent {
                delivered_to: parent,
                task: Some(task),
            }
        }
        Ok(Err(TaskReportRefusal::Permit(PermitError::PlanNotFound))) => {
            // Deleted between the existence check and the lock.
            ledger.locks().remove(&plan_id);
            unbound_msg()
        }
        Ok(Err(TaskReportRefusal::Permit(PermitError::InvalidState))) => unbound_msg(),
        Ok(Err(TaskReportRefusal::Permit(err))) => plan::permit_error_msg(err),
        Ok(Err(TaskReportRefusal::InboxFull)) => inbox_full_msg(),
        Ok(Err(TaskReportRefusal::Write(err))) => {
            tracing::warn!(error = %err, "task report write failed");
            plan::internal_msg()
        }
        Err(access_err) => plan::map_access_error(access_err),
    }
}

fn received_report(
    entry: &WindowEntry,
    body: &ReportBody,
    task: Option<ReportTaskRef>,
) -> ReceivedReport {
    ReceivedReport {
        from_window_id: entry.window_id().to_string(),
        from_name: entry.window_info().name,
        outcome: body.outcome,
        summary: body.summary.clone(),
        path: body.path.clone(),
        task,
        at_ms: now_ms(),
    }
}

/// Answers `wait_for_report`: every queued report at once, or -- on an
/// empty inbox with a non-zero `timeout_sec` -- a spawned wait that shares
/// the connection's waiter budget with `wait_for_status` and is aborted with
/// the connection.
pub(super) async fn handle_wait_for_report(
    inboxes: &Arc<ReportInboxes>,
    ledger: &Arc<PlanRuntime>,
    config: &DaemonConfig,
    outbound: &Arc<Outbound>,
    state: &mut ConnectionState,
    timeout_sec: u64,
) {
    let RequestOrigin::Window(window_id) = &state.origin else {
        outbound.reply(&not_a_window_msg());
        return;
    };
    let window_id = window_id.clone();
    if timeout_sec > config.wait_timeout_max_secs {
        outbound.reply(&error_msg(
            "invalid_argument",
            "timeout_sec must not exceed the configured maximum",
        ));
        return;
    }
    let budget_left = state.live_waiter_count() < config.max_waiters_per_conn;
    let receipt = state.report_receipt.take();
    match inboxes.take_or_wait(&window_id, timeout_sec > 0, budget_left) {
        Taken::Ready(taken) => {
            answer_taken(inboxes, ledger, outbound, &window_id, Some(taken), receipt).await
        }
        Taken::NoWaiterBudget => outbound.reply(&error_msg(
            "resource_exhausted",
            "this connection already holds the maximum number of pending waits",
        )),
        Taken::Waiting(registration) => {
            let RequestWatch { outbound, mut left } =
                RequestWatch::quiet(&inboxes.registry, outbound, state);
            let inboxes = Arc::clone(inboxes);
            let ledger = Arc::clone(ledger);
            let timeout = Duration::from_secs(timeout_sec);
            state.waiters.push(tokio::spawn(async move {
                // A wait its requester's CLI has dropped takes nothing: the
                // dropped registration puts back what reached it.
                let taken = tokio::select! {
                    taken = registration.wait(timeout) => taken,
                    () = request_left(&mut left) => None,
                };
                answer_taken(&inboxes, &ledger, &outbound, &window_id, taken, receipt).await;
            }));
        }
    }
}

/// Answers with what a take took. Through a relay session's `receipt` the
/// take goes to the session's request uncommitted, and comes back to the
/// inbox when that request is gone; otherwise it is committed and answered.
async fn answer_taken(
    inboxes: &Arc<ReportInboxes>,
    ledger: &PlanRuntime,
    outbound: &Outbound,
    window_id: &str,
    taken: Option<TakenReports>,
    receipt: Option<ReportReceipt>,
) {
    match (taken.filter(|taken| !taken.is_empty()), receipt) {
        (Some(taken), Some(receipt)) => {
            #[cfg(feature = "fault-injection")]
            inboxes.registry.config().faults.hold_report_taken().await;
            let reports = taken.reports();
            // Handed before the answer, so the request has it once answered.
            receipt.hand(taken);
            outbound.reply(&DaemonMessage::Reports {
                reports,
                awaiting: None,
            });
        }
        (taken, _) => {
            let reports = taken.map(TakenReports::into_committed).unwrap_or_default();
            reply_reports(inboxes, ledger, outbound, window_id, reports).await;
        }
    }
}

/// Replies with `reports`, putting them back when the connection can no
/// longer take the reply: a drained report must reach someone. An empty
/// answer also lists who the caller is still waiting on.
async fn reply_reports(
    inboxes: &Arc<ReportInboxes>,
    ledger: &PlanRuntime,
    outbound: &Outbound,
    window_id: &str,
    reports: Vec<ReceivedReport>,
) {
    // Awaited only when there is nothing drained, so an abort at this point
    // can never lose a report.
    let awaiting = if reports.is_empty() {
        Some(awaiting_windows(ledger, &inboxes.registry, window_id).await)
    } else {
        None
    };
    outbound.reply(&DaemonMessage::Reports {
        reports: reports.clone(),
        awaiting,
    });
    if outbound.is_broken() && !reports.is_empty() {
        inboxes.put(window_id, reports, false, true);
    }
}

/// Every window not yet `Exited` whose report would be delivered to
/// `parent` right now, decided by [`resolve_parent`] so it can never
/// disagree with where a report actually goes.
async fn awaiting_windows(
    ledger: &PlanRuntime,
    registry: &Arc<WindowRegistry>,
    parent: &str,
) -> Vec<AwaitingWindow> {
    let mut awaiting = Vec::new();
    for info in registry.list() {
        if info.status == WindowStatus::Exited {
            continue;
        }
        if resolve_parent(ledger, registry, &info.window_id)
            .await
            .as_deref()
            != Some(parent)
        {
            continue;
        }
        awaiting.push(AwaitingWindow {
            window_id: info.window_id,
            name: info.name,
            status: info.status,
            mcp_connected: info.mcp_connected,
        });
    }
    awaiting
}

fn not_a_window_msg() -> DaemonMessage {
    error_msg("auth_failed", "only a window can send or wait for reports")
}

fn no_parent_msg() -> DaemonMessage {
    error_msg("invalid_state", "this window has no parent to report to")
}

fn unbound_msg() -> DaemonMessage {
    error_msg(
        "invalid_state",
        "this window is no longer bound to its task",
    )
}

fn inbox_full_msg() -> DaemonMessage {
    error_msg(
        "resource_exhausted",
        "the parent window's report inbox is full",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::window::{OpenWindowRequest, WindowProvenance};

    fn report(summary: &str) -> ReceivedReport {
        ReceivedReport {
            from_window_id: "w-child".to_string(),
            from_name: "child".to_string(),
            outcome: ReportOutcome::Succeeded,
            summary: summary.to_string(),
            path: None,
            task: None,
            at_ms: 0,
        }
    }

    #[cfg(windows)]
    fn idle_command() -> Vec<String> {
        vec!["cmd.exe".into(), "/c".into(), "exit".into()]
    }

    #[cfg(not(windows))]
    fn idle_command() -> Vec<String> {
        vec!["sh".into(), "-c".into(), "true".into()]
    }

    #[tokio::test]
    async fn a_stale_stretchs_confirmation_does_not_consume_a_later_stretchs_notice() {
        let registry = Arc::new(WindowRegistry::new(DaemonConfig::default()));
        let opened = registry
            .open_with_extra_env(
                OpenWindowRequest {
                    command: idle_command(),
                    cwd: None,
                    name: None,
                    vendor: None,
                    detection: None,
                    on_name_taken: Default::default(),
                },
                Vec::new(),
                Vec::new(),
                WindowProvenance::default(),
            )
            .await
            .expect("open a registered window");
        let window = opened.window_id.as_str();
        let inboxes = Arc::new(ReportInboxes::new(registry));

        inboxes.put(window, vec![report("first")], false, false);
        let (_, first_stretch) = inboxes.due_notice(window).expect("first stretch is due");

        assert_eq!(drained(inboxes.take_or_wait(window, false, true)).len(), 1);

        inboxes.put(window, vec![report("second")], false, false);
        assert!(inboxes.due_notice(window).is_some());

        inboxes.confirm_notice(window, first_stretch);

        assert!(
            inboxes.notice_due(window),
            "a confirmation for an earlier stretch must leave the new stretch's notice due"
        );
    }

    async fn registered_window() -> (Arc<ReportInboxes>, String) {
        let registry = Arc::new(WindowRegistry::new(DaemonConfig::default()));
        let opened = registry
            .open_with_extra_env(
                OpenWindowRequest {
                    command: idle_command(),
                    cwd: None,
                    name: None,
                    vendor: None,
                    detection: None,
                    on_name_taken: Default::default(),
                },
                Vec::new(),
                Vec::new(),
                WindowProvenance::default(),
            )
            .await
            .expect("open a registered window");
        (Arc::new(ReportInboxes::new(registry)), opened.window_id)
    }

    async fn delivering_window() -> (Arc<ReportInboxes>, Arc<WindowEntry>) {
        let registry = Arc::new(WindowRegistry::new(DaemonConfig::default()));
        let delivery = crate::profile_delivery::ProfileDelivery {
            output: crate::inbox::DoorOutputs::all(&crate::inbox::OutputForm::Plain),
            max_chars: crate::inbox::DEFAULT_MAX_CHARS,
            wake: crate::profile_delivery::WakeMode::Wait,
            wake_stream: yagura_protocol::DeliveryStream::Stderr,
            wake_exit: crate::inbox::DEFAULT_WAKE_EXIT,
            launch_prompt: None,
            launch_context: None,
        };
        let opened = registry
            .open_with_extra_env(
                OpenWindowRequest {
                    command: idle_command(),
                    cwd: None,
                    name: None,
                    vendor: None,
                    detection: None,
                    on_name_taken: Default::default(),
                },
                Vec::new(),
                Vec::new(),
                WindowProvenance {
                    delivery: Some(delivery),
                    ..WindowProvenance::default()
                },
            )
            .await
            .expect("open a registered window");
        let entry = registry.get(&opened.window_id).expect("registered");
        (Arc::new(ReportInboxes::new(registry)), entry)
    }

    /// A report put while `wait_for_report` is taking the notice of the
    /// reports it drained back out of the window's inbox must leave its own
    /// notice there: the drain withdraws the old notice, never the new one.
    #[tokio::test]
    async fn a_report_put_while_a_drain_withdraws_its_notice_keeps_a_notice_of_its_own() {
        let (inboxes, entry) = delivering_window().await;
        let window = entry.window_id().to_string();
        inboxes.put(&window, vec![report("first")], false, false);
        assert!(entry.inbox().holds(REPORT_NOTICE_KEY));
        let late: Arc<Mutex<Option<std::thread::JoinHandle<()>>>> = Arc::default();
        entry.inbox().before_next_withdraw(Box::new({
            let inboxes = Arc::clone(&inboxes);
            let window = window.clone();
            let late = Arc::clone(&late);
            let runtime = tokio::runtime::Handle::current();
            move || {
                // It lands in the gap when the drain leaves the inboxes' lock
                // before withdrawing, and right after the drain otherwise.
                let gap = inboxes.try_notice(&window).is_some();
                let put = move || {
                    let _runtime = runtime.enter();
                    inboxes.put(&window, vec![report("second")], false, false);
                };
                if gap {
                    put();
                } else {
                    *lock_recover(&late) = Some(std::thread::spawn(put));
                }
            }
        }));

        assert_eq!(drained(inboxes.take_or_wait(&window, false, true)).len(), 1);
        if let Some(putting) = lock_recover(&late).take() {
            putting.join().expect("the put finishes");
        }

        assert!(inboxes.notice_due(&window), "the second report is unread");
        assert!(
            entry.inbox().holds(REPORT_NOTICE_KEY),
            "a due notice must be waiting in the window's inbox"
        );
    }

    fn drained(taken: Taken) -> Vec<ReceivedReport> {
        match taken {
            Taken::Ready(taken) => taken.into_committed(),
            _ => panic!("expected the inbox to answer at once"),
        }
    }

    fn has_inbox(inboxes: &ReportInboxes, window: &str) -> bool {
        lock_recover(&inboxes.inboxes).contains_key(window)
    }

    #[tokio::test]
    async fn returning_an_unused_reservation_keeps_queued_reports_and_drops_an_empty_inbox() {
        let (inboxes, window) = registered_window().await;
        let window = window.as_str();

        inboxes.put(window, vec![report("queued")], false, false);
        drop(inboxes.reserve(window).expect("room for a reservation"));
        let reports = drained(inboxes.take_or_wait(window, false, true));
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].summary, "queued");
        assert!(!has_inbox(&inboxes, window), "a drained inbox is dropped");

        drop(inboxes.reserve(window).expect("room for a reservation"));
        assert!(!has_inbox(&inboxes, window), "an idle inbox is dropped");
    }

    #[tokio::test]
    async fn reservations_count_against_the_inbox_cap_until_delivered_or_returned() {
        let (inboxes, window) = registered_window().await;
        let window = window.as_str();

        let held = inboxes.reserve(window).expect("room for a reservation");
        for n in 1..REPORT_INBOX_MAX {
            inboxes
                .reserve(window)
                .expect("room below the cap")
                .deliver(report(&n.to_string()));
        }
        assert!(
            inboxes.reserve(window).is_none(),
            "queued reports plus the held reservation fill the inbox"
        );

        drop(held);
        assert!(
            inboxes.reserve(window).is_some(),
            "a returned reservation frees its place"
        );
    }

    #[tokio::test]
    async fn discarding_an_inbox_drops_its_reports_and_its_due_notice() {
        let (inboxes, window) = registered_window().await;
        let window = window.as_str();

        inboxes.put(window, vec![report("unread")], false, false);
        assert!(inboxes.notice_due(window));

        inboxes.discard(window);

        assert!(!inboxes.notice_due(window));
        assert!(!has_inbox(&inboxes, window));
        assert!(drained(inboxes.take_or_wait(window, false, true)).is_empty());
    }

    #[tokio::test]
    async fn a_wait_abandoned_after_a_hand_off_puts_the_report_back_and_spares_other_waiters() {
        let (inboxes, window) = registered_window().await;
        let window = window.as_str();
        let waiting = |taken: Taken| match taken {
            Taken::Waiting(registration) => registration,
            _ => panic!("expected a parked waiter"),
        };

        let first = waiting(inboxes.take_or_wait(window, true, true));
        inboxes.put(window, vec![report("handed")], false, false);
        drop(first);
        let reports = drained(inboxes.take_or_wait(window, false, true));
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].summary, "handed");

        let leaving = waiting(inboxes.take_or_wait(window, true, true));
        let staying = waiting(inboxes.take_or_wait(window, true, true));
        drop(leaving);
        inboxes.put(window, vec![report("for the other")], false, false);
        let reports = staying
            .wait(Duration::from_secs(30))
            .await
            .map(TakenReports::into_committed)
            .unwrap_or_default();
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].summary, "for the other");
    }

    #[tokio::test]
    async fn a_reply_that_cannot_be_sent_puts_the_drained_reports_back() {
        let (inboxes, window) = registered_window().await;
        let window = window.as_str();
        let (tx, rx) = tokio::sync::mpsc::channel(1);
        drop(rx);
        let outbound = Outbound::new(tx);

        reply_reports(
            &inboxes,
            &PlanRuntime::disabled(),
            &outbound,
            window,
            vec![report("r")],
        )
        .await;

        let reports = drained(inboxes.take_or_wait(window, false, true));
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].summary, "r");
    }
}
