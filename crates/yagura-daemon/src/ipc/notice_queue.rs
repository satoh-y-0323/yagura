//! Every window's notice delivery agent: one task per window that writes the
//! notices queued on it (the chat intro, a plan task's `[yagura task]`
//! notice) and then its report notice, one at a time, whenever the window
//! accepts input.
//!
//! A queued notice is removed only once a delivery wrote it. One the window
//! declined stays where it is and is tried again when the window's status
//! moves on or, failing that, after [`NOTICE_RETRY_INTERVAL`], so a window
//! that keeps accepting input while refusing the write is not retried in a
//! tight loop. A notice declined because someone else's text sits in the
//! composer is not retried on a clock: that text stays until somebody empties
//! it, so it waits for the composer to read empty or the status to move on.
//! Whatever is still queued when the window ends is settled then:
//! an intro as abandoned, a task notice as `window_closed` in the ledger.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use tokio::sync::watch;
use tokio::time::Instant;
use yagura_protocol::{DaemonMessage, HandoffDelivery, InstructionNoticeState, WindowStatus};

use crate::approval::ApprovalBroker;
use crate::config::DaemonConfig;
use crate::handoff::{READY_STATUSES, compose_plan_approved_notice, compose_task_notice, is_ready};
use crate::plan_permit::{self, PermitError};
use crate::store::StoreError;
use crate::waiter::{StatusAt, WaitOutcome, wait_for_any_held};
use crate::window::{ChatIntroOutcome, QueuedNotice, WindowEntry, WindowLiveness, WindowRegistry};

use super::chat::ChatRuntime;
use super::plan::{self, PlanRuntime};
use super::report::ReportInboxes;
use super::{CarriedResidue, DeliveryEnd, DeliveryText, Scarce, SharedBudget, run_delivery};

/// How long one wait for the window to accept input runs before its liveness
/// is looked at again; a notice itself has no deadline.
const NOTICE_WAIT_SLICE: Duration = Duration::from_secs(60);

/// How long a declined notice waits before it is tried again while the
/// window's status stays where it was.
const NOTICE_RETRY_INTERVAL: Duration = Duration::from_secs(2);

/// What every window's delivery agent delivers with. Held strongly by the
/// IPC server and by each running agent; the registry's launcher holds it
/// weakly, so the registry never keeps it alive.
pub(super) struct NoticeAgents {
    registry: Arc<WindowRegistry>,
    reports: Arc<ReportInboxes>,
    chat: Arc<ChatRuntime>,
    ledger: Arc<PlanRuntime>,
    broker: Arc<ApprovalBroker>,
    config: DaemonConfig,
}

impl NoticeAgents {
    /// Installs the launcher on `registry`, which also starts an agent for
    /// every window that already has notices queued.
    pub(super) fn install(
        registry: &Arc<WindowRegistry>,
        reports: Arc<ReportInboxes>,
        chat: Arc<ChatRuntime>,
        ledger: Arc<PlanRuntime>,
        broker: Arc<ApprovalBroker>,
    ) -> Arc<Self> {
        let agents = Arc::new(Self {
            registry: Arc::clone(registry),
            reports,
            chat,
            ledger,
            broker,
            config: registry.config().clone(),
        });
        let launcher_agents = Arc::downgrade(&agents);
        registry.set_notice_launcher(Box::new(move |entry: &Arc<WindowEntry>| {
            if let Some(agents) = launcher_agents.upgrade() {
                launch(&agents, entry);
            }
        }));
        agents
    }
}

/// Starts `entry`'s agent, or wakes the one already running.
fn launch(agents: &Arc<NoticeAgents>, entry: &Arc<WindowEntry>) {
    // Only unreachable outside a runtime (a drop during shutdown), where
    // nothing could deliver anyway.
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        return;
    };
    if entry.claim_notice_agent() {
        runtime.spawn(run_agent(Arc::clone(agents), Arc::clone(entry)));
    } else {
        entry.wake_notice_agent();
    }
}

/// The notice an agent tries next.
enum Next {
    Queued(QueuedNotice),
    Report,
}

impl Next {
    fn kind(&self) -> &'static str {
        match self {
            Self::Queued(QueuedNotice::Intro { .. }) => "intro",
            Self::Queued(QueuedNotice::Task { .. }) => "task",
            Self::Queued(QueuedNotice::PlanApproved { .. }) => "plan_approved",
            Self::Report => "report",
        }
    }
}

/// How one delivery attempt ended, with the verdict or reason it is logged
/// under.
#[derive(Clone, Copy)]
enum Attempt {
    Written(&'static str),
    Declined(&'static str),
    /// Declined because someone else's text is in the composer.
    DeclinedForeign,
}

/// When the last attempt was declined, and on which status generation.
struct Declined {
    generation: u64,
    at: Instant,
}

async fn run_agent(agents: Arc<NoticeAgents>, entry: Arc<WindowEntry>) {
    let window_id = entry.window_id().to_string();
    let mut status = entry.watch_status();
    let mut declined: Option<Declined> = None;
    let mut retries: u32 = 0;
    // Whether the last attempt was declined for someone else's text.
    let mut foreign_draft = false;
    // What a declined attempt left in the composer, handed to the next one so
    // it finishes its own text instead of declining it as someone else's.
    let mut residue: Option<CarriedResidue> = None;
    while matches!(
        agents.registry.liveness(&window_id),
        WindowLiveness::Alive(_)
    ) {
        let Some(next) = next_notice(&agents, &entry) else {
            declined = None;
            foreign_draft = false;
            retries = 0;
            residue = None;
            if !idle(&entry, &mut status).await {
                break;
            }
            continue;
        };
        if let Some(last) = &declined {
            let released = if foreign_draft {
                wait_for_draft_change(&entry, &mut status, last).await
            } else {
                wait_to_retry(&mut status, last).await
            };
            if !released {
                break;
            }
        }
        let ready_hold = entry.input_profile().ready_hold;
        match wait_for_any_held(
            status.clone(),
            READY_STATUSES,
            ready_hold,
            NOTICE_WAIT_SLICE,
        )
        .await
        {
            WaitOutcome::Reached => {}
            WaitOutcome::Timeout { .. } => continue,
            WaitOutcome::ChildExited { .. } | WaitOutcome::Detached => break,
        }
        entry.note_left_startup_phase();
        let attempt = attempt_notice(&agents, &entry, &next, &mut residue).await;
        entry.note_notice_attempt();
        let outcome = match attempt {
            Attempt::Written(outcome) | Attempt::Declined(outcome) => outcome,
            Attempt::DeclinedForeign => "foreign_draft",
        };
        tracing::info!(
            window_id,
            kind = next.kind(),
            outcome,
            retries,
            "notice delivery attempt finished"
        );
        match attempt {
            Attempt::Written(_) => {
                declined = None;
                foreign_draft = false;
                retries = 0;
            }
            Attempt::Declined(_) | Attempt::DeclinedForeign => {
                declined = Some(Declined {
                    generation: status.borrow().generation,
                    at: Instant::now(),
                });
                foreign_draft = matches!(attempt, Attempt::DeclinedForeign);
                retries = retries.saturating_add(1);
            }
        }
    }
    finish(&agents, &entry).await;
}

/// The queue's head, else the report notice when one is due. `None` also
/// while an intro still waits for its room: it holds everything behind it.
fn next_notice(agents: &NoticeAgents, entry: &WindowEntry) -> Option<Next> {
    // A notice is a judged delivery, so a window the daemon cannot judge can
    // never take one: the agent idles rather than retrying, and still runs so
    // that `finish` settles whatever was queued and discards the report inbox
    // once the window ends.
    if !entry.composer_access().is_judged() {
        return None;
    }
    match entry.front_notice() {
        Some(QueuedNotice::Intro {
            room_settled: false,
        }) => None,
        Some(queued) => Some(Next::Queued(queued)),
        None => agents
            .reports
            .notice_due(entry.window_id())
            .then_some(Next::Report),
    }
}

/// Waits for something that may give the agent work. `false` once the
/// window's status stream is gone.
async fn idle(entry: &WindowEntry, status: &mut watch::Receiver<StatusAt>) -> bool {
    status.mark_unchanged();
    tokio::select! {
        changed = status.changed() => changed.is_ok(),
        () = entry.notice_woken() => true,
    }
}

/// Holds a retry back until the status generation moves past the declined
/// attempt's or the retry interval has passed. `false` once the window's
/// status stream is gone.
async fn wait_to_retry(status: &mut watch::Receiver<StatusAt>, last: &Declined) -> bool {
    let deadline = last.at + NOTICE_RETRY_INTERVAL;
    loop {
        if status.borrow_and_update().generation > last.generation {
            return true;
        }
        tokio::select! {
            () = tokio::time::sleep_until(deadline) => return true,
            changed = status.changed() => {
                if changed.is_err() {
                    return false;
                }
            }
        }
    }
}

/// Holds a retry back after a decline for someone else's text, which stays
/// until somebody empties the composer: released once a screen that accepts a
/// write shows the composer empty, or the status generation moves past the
/// declined attempt's. Writes by others do not release it, since a draft being
/// typed moves the ledger on every key. `false` once the status stream is gone.
async fn wait_for_draft_change(
    entry: &WindowEntry,
    status: &mut watch::Receiver<StatusAt>,
    last: &Declined,
) -> bool {
    loop {
        if status.borrow_and_update().generation > last.generation {
            return true;
        }
        tokio::select! {
            changed = status.changed() => {
                if changed.is_err() {
                    return false;
                }
            }
            () = entry.wait_for_composer_empty() => return true,
        }
    }
}

/// Composed under the delivery lock: nothing is written to a window whose
/// status is `Unknown` or no longer accepts input.
fn accepts_input(entry: &WindowEntry) -> bool {
    let status = entry.current_status();
    status != WindowStatus::Unknown && is_ready(status)
}

async fn attempt_notice(
    agents: &NoticeAgents,
    entry: &WindowEntry,
    next: &Next,
    residue: &mut Option<CarriedResidue>,
) -> Attempt {
    match next {
        Next::Queued(notice @ QueuedNotice::Intro { .. }) => {
            let intro = agents.chat.intro_text();
            // Only when the intro was queued through another chat runtime
            // than this server's: there is nothing to write.
            if intro.is_empty() {
                entry.remove_notice(notice);
                return Attempt::Declined("no_intro_configured");
            }
            let compose = || accepts_input(entry).then(|| intro.to_string());
            let attempt = deliver(
                entry,
                &compose,
                &agents.config,
                residue,
                agents.chat.delivery_budget(),
            )
            .await;
            if let Attempt::Written(_) = attempt {
                entry.remove_notice(notice);
                entry.set_chat_intro_outcome(ChatIntroOutcome::Sent);
            }
            attempt
        }
        Next::Queued(notice @ QueuedNotice::Task { plan_id, task_id }) => {
            let compose = || accepts_input(entry).then(|| compose_task_notice(plan_id, task_id));
            let attempt = deliver(
                entry,
                &compose,
                &agents.config,
                residue,
                SharedBudget::NotNeeded,
            )
            .await;
            if let Attempt::Written(_) = attempt {
                entry.remove_notice(notice);
                record_task_notice(
                    agents,
                    entry.window_id(),
                    plan_id,
                    task_id,
                    InstructionNoticeState::Delivered,
                )
                .await;
            }
            attempt
        }
        Next::Queued(notice @ QueuedNotice::PlanApproved { plan_id }) => {
            let compose = || accepts_input(entry).then(|| compose_plan_approved_notice(plan_id));
            let attempt = deliver(
                entry,
                &compose,
                &agents.config,
                residue,
                SharedBudget::NotNeeded,
            )
            .await;
            if let Attempt::Written(_) = attempt {
                entry.remove_notice(notice);
            }
            attempt
        }
        Next::Report => {
            let window_id = entry.window_id();
            let stretch = OnceLock::new();
            let compose = || {
                if !accepts_input(entry) {
                    return None;
                }
                let (text, due) = agents.reports.due_notice(window_id)?;
                let _ = stretch.set(due);
                Some(text)
            };
            let attempt = deliver(
                entry,
                &compose,
                &agents.config,
                residue,
                SharedBudget::NotNeeded,
            )
            .await;
            if let (Attempt::Written(_), Some(due)) = (&attempt, stretch.get()) {
                agents.reports.confirm_notice(window_id, *due);
            }
            attempt
        }
    }
}

async fn deliver(
    entry: &WindowEntry,
    compose: &(dyn Fn() -> Option<String> + Sync),
    config: &DaemonConfig,
    residue: &mut Option<CarriedResidue>,
    budget: SharedBudget,
) -> Attempt {
    let delivered = run_delivery(
        entry,
        DeliveryText::Guarded(compose),
        config,
        None,
        0,
        residue.clone(),
        budget,
    )
    .await;
    match delivered {
        DeliveryEnd::Finished {
            verdict,
            residue: left,
            foreign_draft,
            ..
        } => match verdict.delivery {
            HandoffDelivery::NotWritten => {
                *residue = left;
                if foreign_draft {
                    Attempt::DeclinedForeign
                } else {
                    Attempt::Declined("not_written")
                }
            }
            HandoffDelivery::Confirmed => {
                *residue = None;
                Attempt::Written("confirmed")
            }
            HandoffDelivery::Unconfirmed => {
                *residue = None;
                Attempt::Written("unconfirmed")
            }
            HandoffDelivery::NotObserved => {
                *residue = None;
                Attempt::Written("not_observed")
            }
        },
        DeliveryEnd::WriteFailed(err, guard) => {
            *residue = None;
            drop(guard);
            tracing::warn!(window_id = entry.window_id(), error = %err, "notice write failed");
            Attempt::Declined("write_failed")
        }
        DeliveryEnd::Skipped => Attempt::Declined("skipped"),
        DeliveryEnd::Exhausted(Scarce::TurnWaiters) => Attempt::Declined("turn_waiters_exhausted"),
        DeliveryEnd::Exhausted(Scarce::SharedBudget) => Attempt::Declined("chat_budget_exhausted"),
        // Never produced: this passes `closed: None`.
        DeliveryEnd::Aborted => Attempt::Declined("aborted"),
    }
}

/// Settles what the window never got once it has ended, then drops its
/// report inbox: window ids are never reused, so nobody could read it again.
async fn finish(agents: &NoticeAgents, entry: &WindowEntry) {
    let window_id = entry.window_id();
    for notice in entry.release_notice_agent() {
        let kind = match &notice {
            QueuedNotice::Intro { .. } => {
                entry.set_chat_intro_outcome(ChatIntroOutcome::Abandoned);
                "intro"
            }
            QueuedNotice::Task { plan_id, task_id } => {
                record_task_notice(
                    agents,
                    window_id,
                    plan_id,
                    task_id,
                    InstructionNoticeState::WindowClosed,
                )
                .await;
                "task"
            }
            // No ledger state tracks a plan-approval notice, unlike a task's
            // instruction notice: nothing else here reads whether the owner
            // ever saw it.
            QueuedNotice::PlanApproved { .. } => "plan_approved",
        };
        tracing::info!(window_id, kind, "notice dropped: the window ended");
    }
    agents.reports.discard(window_id);
}

/// Why a task notice state was not recorded under the plan's lock.
enum NoticeRefusal {
    Permit(PermitError),
    Write(StoreError),
}

/// Records `state` for `task_id` while it is still bound to `window_id`, and
/// tells the approvers the plan changed. A task rebound elsewhere since is
/// left to its new window.
async fn record_task_notice(
    agents: &NoticeAgents,
    window_id: &str,
    plan_id: &str,
    task_id: &str,
    state: InstructionNoticeState,
) {
    // Checked before locking: locking an id creates its lock-table entry.
    if !matches!(plan::plan_exists(&agents.ledger, plan_id).await, Ok(true)) {
        return;
    }
    let guard = agents.ledger.locks().lock(plan_id).await;
    let window_id = window_id.to_string();
    let task_id = task_id.to_string();
    let recorded = agents
        .ledger
        .with_store(move |store| {
            let permit = plan_permit::issue_notice(&window_id, &task_id, guard, store)
                .map_err(NoticeRefusal::Permit)?;
            store
                .write_instruction_notice(&permit, state)
                .map_err(NoticeRefusal::Write)
        })
        .await;
    match recorded {
        Ok(Ok(())) => {
            agents.broker.notify_approvers(&DaemonMessage::PlanChanged {
                plan_id: plan_id.to_string(),
            });
        }
        Ok(Err(NoticeRefusal::Permit(PermitError::PlanNotFound))) => {
            // Deleted between the existence check and the lock.
            agents.ledger.locks().remove(plan_id);
        }
        Ok(Err(NoticeRefusal::Permit(_))) => {}
        Ok(Err(NoticeRefusal::Write(err))) => {
            tracing::warn!(error = %err, "task notice state write failed");
        }
        Err(_) => {
            tracing::warn!("the plan store became unavailable while recording a task notice");
        }
    }
}

#[cfg(test)]
mod tests {
    use yagura_protocol::DetectedStatus;

    use super::*;

    fn status_at(generation: u64) -> StatusAt {
        let now = Instant::now();
        StatusAt {
            status: DetectedStatus::Idle,
            changed_at: now,
            evidence_since: now,
            generation,
        }
    }

    fn declined_now(generation: u64) -> Declined {
        Declined {
            generation,
            at: Instant::now(),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_retry_is_held_back_for_the_whole_interval_while_the_status_stays() {
        let (_tx, mut rx) = watch::channel(status_at(3));
        let last = declined_now(3);
        let start = Instant::now();
        let waiting = wait_to_retry(&mut rx, &last);
        tokio::pin!(waiting);

        let early = tokio::time::timeout(
            NOTICE_RETRY_INTERVAL - Duration::from_millis(1),
            &mut waiting,
        )
        .await;
        assert!(
            early.is_err(),
            "the retry must not be released before the interval"
        );

        assert!(waiting.await);
        assert_eq!(start.elapsed(), NOTICE_RETRY_INTERVAL);
    }

    #[tokio::test(start_paused = true)]
    async fn a_new_status_generation_releases_the_retry_without_waiting_out_the_interval() {
        let (tx, mut rx) = watch::channel(status_at(3));
        let last = declined_now(3);
        let start = Instant::now();
        let waiting = wait_to_retry(&mut rx, &last);
        tokio::pin!(waiting);

        let early = tokio::time::timeout(Duration::from_millis(500), &mut waiting).await;
        assert!(early.is_err());
        tx.send(status_at(4)).expect("the waiter holds a receiver");

        assert!(waiting.await);
        assert_eq!(start.elapsed(), Duration::from_millis(500));
    }

    #[tokio::test(start_paused = true)]
    async fn a_value_with_the_declined_generation_does_not_release_the_retry() {
        let (tx, mut rx) = watch::channel(status_at(3));
        let last = declined_now(3);
        let start = Instant::now();
        let waiting = wait_to_retry(&mut rx, &last);
        tokio::pin!(waiting);

        let early = tokio::time::timeout(Duration::from_millis(500), &mut waiting).await;
        assert!(early.is_err());
        tx.send(status_at(3)).expect("the waiter holds a receiver");

        assert!(waiting.await);
        assert_eq!(start.elapsed(), NOTICE_RETRY_INTERVAL);
    }

    #[tokio::test(start_paused = true)]
    async fn a_generation_already_past_the_declined_one_releases_the_retry_at_once() {
        let (_tx, mut rx) = watch::channel(status_at(5));
        let last = declined_now(3);
        let start = Instant::now();

        assert!(wait_to_retry(&mut rx, &last).await);
        assert_eq!(start.elapsed(), Duration::ZERO);
    }

    #[tokio::test(start_paused = true)]
    async fn a_dropped_status_sender_ends_the_wait_with_false() {
        let (tx, mut rx) = watch::channel(status_at(3));
        let last = declined_now(3);
        drop(tx);

        assert!(!wait_to_retry(&mut rx, &last).await);
    }
}
