//! Every window's delivery agent: one task per window that pushes what the
//! window's inbox holds, when the window's profile gives the daemon a way to,
//! and settles whatever is left once the window ends.
//!
//! How it pushes is the profile's `[delivery]` `wake` -- data, never the name
//! of a CLI:
//!
//! - `wait`: answers the wait the CLI left registered, whenever the window can
//!   be pushed to again.
//! - `command`: runs the profile's own program with the wake arguments, once
//!   the CLI has reported its session. A failed run puts the parcels back and
//!   is not repeated until something changes.
//! - `none`: types the oldest parcel into the composer while the window waits
//!   for input, through the same judged send `send_message` uses.
//!
//! None of them pushes to a window whose status is `Unknown`. A parcel a push
//! carries is out of reach of the window's doors until the push ends, and goes
//! back in front of the inbox if nothing reached the CLI.
//!
//! A typed parcel the window declined is tried again when the window's status
//! moves on or, failing that, after [`NOTICE_RETRY_INTERVAL`]; one declined
//! because someone else's text sits in the composer waits for the composer to
//! read empty or the status to move on instead.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{oneshot, watch};
use tokio::time::Instant;
use yagura_protocol::WindowStatus;

use crate::config::DaemonConfig;
use crate::handoff::{READY_STATUSES, is_ready};
use crate::inbox::{Deposited, Parcel, ParcelEnd, Push, PushBudget, PushEnd};
use crate::profile_delivery::WakeMode;
use crate::pty;
use crate::waiter::{StatusAt, WaitOutcome, wait_for_any_held};
use crate::window::{WindowEntry, WindowLiveness, WindowRegistry};

use super::report::ReportInboxes;
use super::{CarriedResidue, CounterGuard, DeliveryEnd, Scarce, SharedBudget, TypedOutcome};

/// How long one wait for the window to accept input runs before its liveness
/// is looked at again; a parcel itself has no deadline.
const NOTICE_WAIT_SLICE: Duration = Duration::from_secs(60);

/// How long a declined parcel waits before it is typed again while the
/// window's status stays where it was.
const NOTICE_RETRY_INTERVAL: Duration = Duration::from_secs(2);

/// How long a wake command may run before it is given up on; only a hang
/// reaches it.
const WAKE_COMMAND_TIMEOUT: Duration = Duration::from_secs(30);

/// The most output a wake command's run is read for.
const WAKE_COMMAND_MAX_OUTPUT_BYTES: usize = 64 * 1024;

/// Where a wake argument takes the CLI's session identifier.
const SESSION_PLACEHOLDER: &str = "{session}";
/// Where a wake argument takes the text.
const TEXT_PLACEHOLDER: &str = "{text}";

/// What every window's delivery agent works with. Held strongly by the IPC
/// server and by each running agent; the registry's launcher holds it
/// weakly, so the registry never keeps it alive.
pub(super) struct NoticeAgents {
    registry: Arc<WindowRegistry>,
    reports: Arc<ReportInboxes>,
    config: DaemonConfig,
}

impl NoticeAgents {
    /// Installs the launcher on `registry`, which also starts an agent for
    /// every window whose inbox already holds something.
    pub(super) fn install(
        registry: &Arc<WindowRegistry>,
        reports: Arc<ReportInboxes>,
    ) -> Arc<Self> {
        let agents = Arc::new(Self {
            registry: Arc::clone(registry),
            reports,
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

/// Why [`deposit`] turned a parcel away at once.
pub(super) enum Refused {
    /// The window has ended, or is ending.
    Ended,
    /// The window's inbox has no room for it.
    Full,
}

/// Puts `parcel` in `entry`'s inbox and wakes the window's agent. `budget` is
/// what typing it draws on; `waiting_limit` turns it away rather than pushing
/// an older parcel out when that many already wait to be typed.
///
/// # Errors
///
/// [`Refused`] when the inbox would not take it.
pub(super) fn deposit(
    registry: &WindowRegistry,
    entry: &Arc<WindowEntry>,
    parcel: Parcel,
    budget: Option<PushBudget>,
    waiting_limit: Option<usize>,
) -> Result<oneshot::Receiver<ParcelEnd>, Refused> {
    let typed = matches!(
        entry.delivery().map(|delivery| &delivery.wake),
        Some(WakeMode::None)
    );
    let Deposited { turned_away, end } =
        entry
            .inbox()
            .deposit_tracked(parcel, budget, waiting_limit.filter(|_| typed));
    if !turned_away.is_empty() {
        tracing::info!(
            window_id = entry.window_id(),
            count = turned_away.len(),
            "parcels turned away by a full inbox"
        );
    }
    let Some(end) = end else {
        return Err(
            if matches!(
                registry.liveness(entry.window_id()),
                WindowLiveness::Alive(_)
            ) {
                Refused::Full
            } else {
                Refused::Ended
            },
        );
    };
    registry.wake_notices(entry);
    Ok(end)
}

/// How one typing attempt ended, with the verdict or reason it is logged
/// under.
#[derive(Clone, Copy)]
enum Attempt {
    Written(&'static str),
    Declined(&'static str),
    /// Declined because someone else's text is in the composer.
    DeclinedForeign,
    /// The budget it draws on was spent.
    Exhausted(&'static str),
}

/// When the last attempt was declined, and on which status generation.
struct Declined {
    generation: u64,
    at: Instant,
}

/// What a typing agent carries from one attempt to the next.
#[derive(Default)]
struct Typing {
    declined: Option<Declined>,
    /// Whether the last attempt was declined for someone else's text.
    foreign_draft: bool,
    retries: u32,
    /// What a declined attempt left in the composer, with the parcel it
    /// belongs to, handed to the next attempt at that same parcel so it
    /// finishes its own text instead of declining it as someone else's.
    residue: Option<(String, CarriedResidue)>,
}

impl Typing {
    fn reset(&mut self) {
        *self = Self::default();
    }
}

async fn run_agent(agents: Arc<NoticeAgents>, entry: Arc<WindowEntry>) {
    let window_id = entry.window_id().to_string();
    let mut status = entry.watch_status();
    let mut typing = Typing::default();
    // A failed wake command is not run again until something changes.
    let mut command_failed = false;
    while matches!(
        agents.registry.liveness(&window_id),
        WindowLiveness::Alive(_)
    ) {
        status.borrow_and_update();
        let wake = entry.delivery().map(|delivery| delivery.wake.clone());
        match wake {
            Some(WakeMode::Wait) => entry.inbox().wake_waiter(),
            Some(WakeMode::Command(args)) => {
                if !command_failed
                    && pushable(&entry)
                    && entry.session_id().is_some()
                    && entry.inbox().pending_count() > 0
                {
                    command_failed = !push_by_command(&agents, &entry, &args).await;
                    continue;
                }
            }
            Some(WakeMode::None) => {
                // A window the daemon cannot judge a typed line in is never
                // typed into: what waits for it stays for its doors.
                if entry.composer_access().is_judged() && entry.inbox().pending_count() > 0 {
                    match type_once(&agents, &entry, &mut status, &mut typing).await {
                        Some(()) => continue,
                        None => break,
                    }
                }
                typing.reset();
            }
            // Nothing is pushed to a window that declares no delivery; the
            // agent still runs so that its report inbox is discarded once it
            // ends.
            None => {}
        }
        if !idle(&entry, &mut status).await {
            break;
        }
        command_failed = false;
    }
    finish(&agents, &entry);
}

/// Whether the daemon may push to `entry` now.
fn pushable(entry: &WindowEntry) -> bool {
    !matches!(
        entry.current_status(),
        WindowStatus::Unknown | WindowStatus::Exited
    )
}

/// One pass of the typing agent: waits out a declined attempt's pacing and
/// the window's readiness, then types the oldest parcel. `None` once the
/// window's status stream is gone.
async fn type_once(
    agents: &NoticeAgents,
    entry: &WindowEntry,
    status: &mut watch::Receiver<StatusAt>,
    typing: &mut Typing,
) -> Option<()> {
    if let Some(last) = &typing.declined {
        let released = if typing.foreign_draft {
            wait_for_draft_change(entry, status, last).await
        } else {
            wait_to_retry(status, last).await
        };
        if !released {
            return None;
        }
    }
    // A budgeted parcel holds its unit from the moment it is next to be typed,
    // its wait for the window to be ready included; with none left it goes
    // straight to the push, which turns it away.
    let reserved = entry.inbox().next_budget().map(|budget| {
        CounterGuard::try_take(&budget.counter, budget.max).map(|unit| (budget, unit))
    });
    if !matches!(reserved, Some(None)) {
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
            WaitOutcome::Timeout { .. } => return Some(()),
            WaitOutcome::ChildExited { .. } | WaitOutcome::Detached => return None,
        }
        entry.note_left_startup_phase();
    }
    // Taken out only now: until the window is ready, a turn's own door may
    // still take it.
    let Some(push) = entry.inbox().begin_push(false) else {
        return Some(());
    };
    let carried = typing
        .residue
        .take()
        .filter(|(key, _)| *key == push.key)
        .map(|(_, residue)| residue);
    let held = reserved.flatten().filter(|(budget, _)| {
        push.budget
            .as_ref()
            .is_some_and(|drawn| Arc::ptr_eq(&drawn.counter, &budget.counter))
    });
    let (attempt, left) = type_push(entry, &push, &agents.config, carried, held.is_some()).await;
    drop(held);
    entry.note_notice_attempt();
    let outcome = match attempt {
        Attempt::Written(outcome) | Attempt::Declined(outcome) | Attempt::Exhausted(outcome) => {
            outcome
        }
        Attempt::DeclinedForeign => "foreign_draft",
    };
    tracing::info!(
        window_id = entry.window_id(),
        outcome,
        retries = typing.retries,
        "notice delivery attempt finished"
    );
    match attempt {
        Attempt::Written(_) => {
            entry.inbox().finish_push(push.id, PushEnd::Taken);
            typing.reset();
        }
        Attempt::Exhausted(_) => {
            entry.inbox().finish_push(push.id, PushEnd::TurnedAway);
            typing.reset();
        }
        Attempt::Declined(_) | Attempt::DeclinedForeign => {
            entry.inbox().finish_push(push.id, PushEnd::Failed);
            typing.declined = Some(Declined {
                generation: status.borrow().generation,
                at: Instant::now(),
            });
            typing.foreign_draft = matches!(attempt, Attempt::DeclinedForeign);
            typing.retries = typing.retries.saturating_add(1);
            typing.residue = left.map(|residue| (push.key.clone(), residue));
        }
    }
    Some(())
}

/// Waits for something that may give the agent work: a status change, or a
/// wake (a new parcel, a session reported). `false` once the window's status
/// stream is gone.
async fn idle(entry: &WindowEntry, status: &mut watch::Receiver<StatusAt>) -> bool {
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

/// Types `push`'s text, returning how the attempt ended and what a declined
/// one left in the composer.
async fn type_push(
    entry: &WindowEntry,
    push: &Push,
    config: &DaemonConfig,
    carried: Option<CarriedResidue>,
    budget_held: bool,
) -> (Attempt, Option<CarriedResidue>) {
    // A parcel withdrawn while this waited for the window's turn is not typed.
    let compose = || {
        (accepts_input(entry) && !entry.inbox().push_withdrawn(push.id)).then(|| push.text.clone())
    };
    let budget = match &push.budget {
        Some(_) if budget_held => SharedBudget::NotNeeded,
        Some(budget) => SharedBudget::Counter {
            counter: Arc::clone(&budget.counter),
            max: budget.max,
        },
        None => SharedBudget::NotNeeded,
    };
    // Withdrawing what is under way ends the typing the way a departed
    // requester does: before the first byte nothing is written, after it the
    // body is taken back if it can be.
    let delivered = super::run_delivery(
        entry,
        &compose,
        config,
        Some(push.withdrawn.clone()),
        0,
        carried,
        budget,
    )
    .await;
    match delivered {
        DeliveryEnd::Finished {
            verdict,
            residue,
            foreign_draft,
        } => {
            tracing::info!(
                window_id = entry.window_id(),
                attempts = verdict.attempts,
                elapsed_since_submitted_ms = verdict.submitted_at.elapsed().as_millis(),
                status_before_submit = %crate::window::window_status_wire(verdict.status_before_submit),
                ready_hold_wait_ms = verdict.ready_hold_wait_ms,
                delivery = ?verdict.delivery,
                "typed delivery finished"
            );
            match verdict.delivery {
                TypedOutcome::NotWritten if foreign_draft => (Attempt::DeclinedForeign, residue),
                TypedOutcome::NotWritten => (Attempt::Declined("not_written"), residue),
                TypedOutcome::Confirmed => (Attempt::Written("confirmed"), None),
                TypedOutcome::Unconfirmed => (Attempt::Written("unconfirmed"), None),
                TypedOutcome::NotObserved => (Attempt::Written("not_observed"), None),
            }
        }
        DeliveryEnd::WriteFailed(err, guard) => {
            drop(guard);
            tracing::warn!(window_id = entry.window_id(), error = %err, "notice write failed");
            (Attempt::Declined("write_failed"), None)
        }
        DeliveryEnd::Skipped => (Attempt::Declined("skipped"), None),
        DeliveryEnd::Exhausted(Scarce::TurnWaiters) => {
            (Attempt::Declined("turn_waiters_exhausted"), None)
        }
        DeliveryEnd::Exhausted(Scarce::SharedBudget) => {
            (Attempt::Exhausted("budget_exhausted"), None)
        }
        DeliveryEnd::Aborted => (Attempt::Declined("withdrawn"), None),
    }
}

/// Hands what waits to the window's CLI by running the profile's own program
/// with `args`. Returns whether it did.
async fn push_by_command(agents: &NoticeAgents, entry: &WindowEntry, args: &[String]) -> bool {
    let Some(session) = entry.session_id() else {
        return false;
    };
    let Some(wake) = entry.wake_command().cloned() else {
        tracing::warn!(
            window_id = entry.window_id(),
            "the window's profile wakes by command, but it was opened with no program to run"
        );
        return false;
    };
    let Some(push) = entry.inbox().begin_push(true) else {
        return true;
    };
    let mut argv = wake.argv;
    argv.extend(
        args.iter()
            .map(|arg| fill_wake_argument(arg, &session, &push.text)),
    );
    let ran = run_wake_command(&agents.config, argv, wake.env, PathBuf::from(entry.cwd())).await;
    tracing::info!(window_id = entry.window_id(), ran, "wake command finished");
    entry
        .inbox()
        .finish_push(push.id, if ran { PushEnd::Taken } else { PushEnd::Failed });
    ran
}

/// `template` with `{session}` and `{text}` filled in, in one pass: what was
/// filled in is never read again, so a `{text}` inside the text stays as
/// written.
fn fill_wake_argument(template: &str, session: &str, text: &str) -> String {
    let mut filled = String::with_capacity(template.len() + text.len());
    let mut rest = template;
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix(SESSION_PLACEHOLDER) {
            filled.push_str(session);
            rest = after;
        } else if let Some(after) = rest.strip_prefix(TEXT_PLACEHOLDER) {
            filled.push_str(text);
            rest = after;
        } else {
            let mut chars = rest.chars();
            if let Some(c) = chars.next() {
                filled.push(c);
            }
            rest = chars.as_str();
        }
    }
    filled
}

/// Runs `argv` once, with no shell, and returns whether it exited with 0.
/// A program that resolves to a Windows `.cmd` / `.bat` file is never run:
/// the command interpreter would read the text as its own syntax.
async fn run_wake_command(
    config: &DaemonConfig,
    argv: Vec<String>,
    env: Vec<(String, String)>,
    cwd: PathBuf,
) -> bool {
    let platform = config.host_os.platform();
    let predicate = crate::profile::is_executable_predicate(config);
    let resolution = match yagura_os::program::resolve_program(
        &argv,
        platform,
        &config.program_lookup,
        &predicate,
    ) {
        Ok(resolution) => resolution,
        Err(_) => {
            tracing::warn!("the wake command's program could not be resolved");
            return false;
        }
    };
    if matches!(resolution, yagura_os::program::Resolution::CmdShim { .. }) {
        tracing::warn!("the wake command's program is a .cmd / .bat file and is not run");
        return false;
    }
    let spawn_argv = yagura_os::program::into_spawn_argv(&resolution, &argv, platform);
    let overlay = env
        .into_iter()
        .map(|(name, value)| (name.into(), value.into()))
        .collect();
    let child = match pty::spawn_one_shot(
        spawn_argv,
        Some(cwd),
        pty::CaptureChildEnv::Based {
            base: config.child_base_env.clone(),
            overlay,
        },
        None,
    )
    .await
    {
        Ok(child) => child,
        Err(err) => {
            tracing::warn!(error = %err, "the wake command could not be started");
            return false;
        }
    };
    let limits = yagura_os::CaptureLimits {
        timeout: WAKE_COMMAND_TIMEOUT,
        max_output_bytes: WAKE_COMMAND_MAX_OUTPUT_BYTES,
    };
    let outcome = tokio::task::spawn_blocking(move || child.collect(limits)).await;
    matches!(
        outcome,
        Ok(yagura_os::CaptureOutcome::Exited { code: Some(0), .. })
    )
}

/// Settles what the window never got once it has ended, then drops its
/// report inbox: window ids are never reused, so nobody could read it again.
fn finish(agents: &NoticeAgents, entry: &WindowEntry) {
    // A daemon that is stopping leaves its posts for the next one to settle.
    let end = if agents.registry.is_shutting_down() {
        ParcelEnd::Stopped
    } else {
        ParcelEnd::Closed
    };
    entry.inbox().close(end);
    agents.reports.discard(entry.window_id());
    entry.release_notice_agent();
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

    #[test]
    fn a_wake_argument_is_filled_in_once_and_what_it_carries_is_not_read_again() {
        assert_eq!(
            fill_wake_argument("--thread={session}", "s-1", "x"),
            "--thread=s-1"
        );
        assert_eq!(
            fill_wake_argument("{text}", "s-1", "say {session} and {text}"),
            "say {session} and {text}"
        );
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
