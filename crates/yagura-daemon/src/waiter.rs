//! The blocking-free waiting primitives: `wait_for_status` behind the
//! `wait_for_status` MCP tool, and `wait_for_reaction` behind a handoff's
//! delivery confirmation.
//!
//! Only the waiting loops live here; task spawning, per-connection concurrency
//! limits and abort-on-disconnect belong to `ipc.rs`. A waiter holds nothing
//! but a status receiver, never a reference back to the `WindowEntry` that
//! produced it.

use std::future::Future;
use std::time::Duration;

use tokio::sync::{broadcast, watch};
use tokio::time::Instant;
use yagura_protocol::{DetectedStatus, WindowStatus};

/// A confirmed status together with the instant it became the confirmed
/// status, published as one value so no observer can pair a new status with
/// the previous change's age.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatusAt {
    /// The confirmed status, together with why it is `Unknown` when it is.
    pub status: DetectedStatus,
    /// When `status` became the confirmed status. A `tokio::time::Instant`,
    /// the clock the detection engine and the window's status publisher run
    /// on, so a paused test clock moves it along with every waiter that reads
    /// it.
    pub changed_at: Instant,
    /// When the evidence `status` was confirmed on first appeared, before any
    /// debounce held it back; never later than `changed_at`, and on the same
    /// clock. What a reaction is attributed to: a change whose evidence
    /// predates an input was not caused by it, however late it was confirmed.
    pub evidence_since: Instant,
    /// How many confirmed transitions this window has published, counting
    /// this one: increased by one every time the window's publisher confirms
    /// a status, including a return to a status already seen. Monotonic per
    /// window and meaningless across windows, so a reader can order two
    /// observations of the *same* window and discard the older one.
    pub generation: u64,
}

impl StatusAt {
    /// The age of this pair at `now`, in whole milliseconds: how long
    /// [`StatusAt::status`] has been the confirmed status. `now` must be read
    /// from the same clock as [`StatusAt::changed_at`].
    ///
    /// The single conversion point from the pair's `Instant` to the
    /// millisecond count the protocol carries, so every answer saturates the
    /// same way.
    ///
    /// # Panics
    ///
    /// Never panics and never fails: a `now` at or before `changed_at` gives
    /// `0`, and an age too large for `u64` milliseconds gives `u64::MAX`.
    ///
    /// ```
    /// use std::time::Duration;
    /// use tokio::time::Instant;
    ///
    /// use yagura_daemon::waiter::StatusAt;
    /// use yagura_protocol::DetectedStatus;
    ///
    /// let changed_at = Instant::now();
    /// let at = StatusAt {
    ///     status: DetectedStatus::Idle,
    ///     changed_at,
    ///     evidence_since: changed_at,
    ///     generation: 0,
    /// };
    ///
    /// assert_eq!(at.since_ms(changed_at + Duration::from_millis(1500)), 1500);
    /// assert_eq!(at.since_ms(changed_at), 0);
    /// ```
    pub fn since_ms(&self, now: Instant) -> u64 {
        u64::try_from(now.saturating_duration_since(self.changed_at).as_millis())
            .unwrap_or(u64::MAX)
    }
}

/// The result of a [`wait_for_status`] call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitOutcome {
    /// The target status was observed.
    Reached,
    /// The deadline elapsed before the target status was observed. Carries
    /// the pair actually observed -- status and, when that status is
    /// `Unknown`, the reason for it -- never a guess.
    Timeout { current: DetectedStatus },
    /// The window exited while waiting for a target status other than
    /// `Exited` itself, so the target can never be reached. Carries the
    /// observed pair, exactly like [`WaitOutcome::Timeout`].
    ChildExited { current: DetectedStatus },
    /// The status sender was dropped (the window was evicted from the
    /// registry) while waiting. Unreachable through a live `WindowEntry` under
    /// the current eviction policy, because a waiter observes `Exited` before
    /// the eviction that drops the sender.
    ///
    /// Carries no pair: this outcome ends a wait that observed nothing, and
    /// naming a status here would invent an observation that never happened.
    Detached,
}

/// Waits on `rx` until its value matches `target`, the window exits with a
/// different target still pending, the status sender is dropped, or
/// `timeout` elapses -- whichever happens first.
///
/// Exactly [`wait_for_any`] with a one-element target list, which is where
/// the waiting rules -- including `Unknown` never settling a wait, even when
/// it is the target -- are documented and implemented.
pub async fn wait_for_status(
    rx: watch::Receiver<StatusAt>,
    target: WindowStatus,
    timeout: Duration,
) -> WaitOutcome {
    wait_for_any(rx, &[target], timeout).await
}

/// Waits on `rx` until its value matches any of `targets`, the window exits
/// with none of them reached, the status sender is dropped, or `timeout`
/// elapses -- whichever happens first.
///
/// The zero-hold form of [`wait_for_any_held`], which is where the waiting
/// rules are implemented.
///
/// `Unknown` is a fail-safe sink: while the observed status is `Unknown`, no
/// target is ever reported as reached, *including* `Unknown` itself when a
/// caller names it. An undetectable window is never treated as
/// ready for input. Passing through `Unknown` does not disarm the wait -- a
/// later transition into a target is still reported as `Reached`.
///
/// `targets` is copied before the returned future is created, so the future
/// owns everything it needs and can be spawned as its own task.
///
/// An empty `targets` never reaches: the wait then only ends on the window's
/// exit, a dropped sender or the deadline.
///
/// # Examples
///
/// ```
/// use std::time::Duration;
/// use tokio::time::Instant;
/// use tokio::sync::watch;
/// use yagura_daemon::handoff::READY_STATUSES;
/// use yagura_daemon::waiter::{StatusAt, WaitOutcome, wait_for_any};
/// use yagura_protocol::DetectedStatus;
///
/// let (_tx, rx) = watch::channel(StatusAt {
///     status: DetectedStatus::Idle,
///     changed_at: Instant::now(),
///     evidence_since: Instant::now(),
///     generation: 0,
/// });
/// // The future owns its copy of the targets, so it can be held, moved or
/// // spawned as a task of its own.
/// let waiting = wait_for_any(rx, READY_STATUSES, Duration::from_secs(5));
/// let outcome = tokio::runtime::Builder::new_current_thread()
///     .enable_all()
///     .build()
///     .expect("a current-thread runtime")
///     .block_on(waiting);
/// assert_eq!(outcome, WaitOutcome::Reached);
/// ```
pub fn wait_for_any(
    rx: watch::Receiver<StatusAt>,
    targets: &[WindowStatus],
    timeout: Duration,
) -> impl Future<Output = WaitOutcome> + Send + 'static {
    wait_for_any_held(rx, targets, Duration::ZERO, timeout)
}

/// Waits like [`wait_for_any`], and settles only when the observed status is
/// in `targets` **and** has been so continuously for `hold`, which it reads
/// from the observed value's own `changed_at` rather than from a parameter.
///
/// Any change restarts the streak, because a change is what `changed_at`
/// records. The condition is evaluated before the timeout is armed, so a
/// `timeout` of zero still settles a condition that already holds. A `hold`
/// of zero makes this exactly [`wait_for_any`].
///
/// # Examples
///
/// ```
/// use std::time::Duration;
/// use tokio::time::Instant;
/// use tokio::sync::watch;
/// use yagura_daemon::handoff::READY_STATUSES;
/// use yagura_daemon::waiter::{StatusAt, WaitOutcome, wait_for_any_held};
/// use yagura_protocol::DetectedStatus;
///
/// let changed_at = Instant::now()
///     .checked_sub(Duration::from_secs(10))
///     .expect("a clock that supports a 10s offset into the past");
/// let (_tx, rx) = watch::channel(StatusAt {
///     status: DetectedStatus::Idle,
///     changed_at,
///     evidence_since: changed_at,
///     generation: 0,
/// });
/// // The pair already carries more than the hold, so nothing is waited out.
/// let waiting = wait_for_any_held(
///     rx,
///     READY_STATUSES,
///     Duration::from_secs(3),
///     Duration::from_secs(5),
/// );
/// let outcome = tokio::runtime::Builder::new_current_thread()
///     .enable_all()
///     .build()
///     .expect("a current-thread runtime")
///     .block_on(waiting);
/// assert_eq!(outcome, WaitOutcome::Reached);
/// ```
pub fn wait_for_any_held(
    mut rx: watch::Receiver<StatusAt>,
    targets: &[WindowStatus],
    hold: Duration,
    timeout: Duration,
) -> impl Future<Output = WaitOutcome> + Send + 'static {
    let targets = targets.to_vec();
    async move {
        let awaited = tokio::time::timeout(timeout, async {
            loop {
                let observed = *rx.borrow_and_update();
                match settle(observed.status, &targets) {
                    Some(WaitOutcome::Reached) => {
                        let held = Instant::now().saturating_duration_since(observed.changed_at);
                        if held >= hold {
                            return WaitOutcome::Reached;
                        }
                        let remaining = hold.saturating_sub(held);
                        // The streak is only as good as the value that carries
                        // it, so a change arriving first replaces it entirely.
                        tokio::select! {
                            changed = rx.changed() => {
                                if changed.is_err() {
                                    return WaitOutcome::Detached;
                                }
                            }
                            () = tokio::time::sleep(remaining) => {}
                        }
                    }
                    Some(outcome) => return outcome,
                    None => {
                        if rx.changed().await.is_err() {
                            return WaitOutcome::Detached;
                        }
                    }
                }
            }
        })
        .await;

        match awaited {
            Ok(outcome) => outcome,
            Err(_) => WaitOutcome::Timeout {
                current: rx.borrow().status,
            },
        }
    }
}

/// The result of a [`wait_for_reaction`] call.
///
/// # Examples
///
/// ```
/// use yagura_daemon::waiter::ReactionOutcome;
/// use yagura_protocol::WindowStatus;
///
/// // Watching a window that stayed put and losing sight of it are
/// // different answers, and only the first says anything about the target.
/// assert_ne!(ReactionOutcome::Quiet, ReactionOutcome::Unobservable);
/// assert_eq!(
///     ReactionOutcome::Reacted(WindowStatus::Running),
///     ReactionOutcome::Reacted(WindowStatus::Running)
/// );
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReactionOutcome {
    /// One of the accepted statuses was observed. Carries the status that
    /// settled the wait.
    Reacted(WindowStatus),
    /// The window exited while waiting.
    Exited,
    /// The deadline elapsed with none of the above observed. Statuses that
    /// are neither accepted nor `Exited` (`Unknown` included) pass through
    /// without settling the wait.
    Quiet,
    /// Status changes were dropped before they could be read, so no answer
    /// about them can be given.
    Unobservable,
}

/// Waits on `rx` for one of `accept`, the window's exit, or `timeout`,
/// whichever happens first, counting only status changes whose evidence
/// appeared at or after `since` -- the instant the caller's submit key was
/// written.
///
/// `rx` is borrowed rather than consumed, so one subscription taken before
/// the first write can be reused across retries: subscribing per attempt
/// would leave a later attempt's subscription starting after its own write,
/// which is exactly where a real reaction would be missed.
///
/// Events already queued on `rx` are read first. Every change, queued or
/// not, whose [`StatusAt::evidence_since`] is earlier than `since` is
/// discarded, so a status change caused by the body echoing into the
/// receiver's editor cannot be read as evidence that the submit key was
/// taken, however late its debounce confirmed it. The rest count, so a
/// reaction confirmed while the caller was still settling after its own
/// write is not lost. `since` must come from the same clock as
/// `evidence_since` (`tokio::time::Instant`).
///
/// A lagged receiver is treated differently in the two phases, and the
/// difference is the point: while reading the backlog, a `Lagged` only
/// means the oldest queued events were dropped, so reading continues and the
/// wait proceeds normally; while waiting, a `Lagged` means a change that may
/// have been the reaction was lost, which is
/// [`ReactionOutcome::Unobservable`]. A backlog `Lagged` can cost a
/// confirmation the caller could have had, and never manufactures one it
/// could not.
///
/// A dropped sender resolves to [`ReactionOutcome::Unobservable`] as well: a
/// caller holding the window it subscribed to cannot reach it, and there is
/// nothing left to observe if it ever did.
///
/// # Examples
///
/// ```
/// use std::time::Duration;
/// use tokio::time::Instant;
/// use tokio::sync::broadcast;
/// use yagura_daemon::waiter::{ReactionOutcome, StatusAt, wait_for_reaction};
/// use yagura_protocol::{DetectedStatus, WindowStatus};
///
/// let (tx, mut rx) = broadcast::channel(4);
/// // Confirmed before the submit key was written, so it is discarded
/// // rather than counted as a reaction.
/// tx.send(StatusAt {
///     status: DetectedStatus::Running,
///     changed_at: Instant::now(),
///     evidence_since: Instant::now(),
///     generation: 1,
/// })
/// .expect("the receiver is alive");
/// let submitted_at = Instant::now() + Duration::from_millis(1);
///
/// let outcome = tokio::runtime::Builder::new_current_thread()
///     .enable_all()
///     .build()
///     .expect("a current-thread runtime")
///     .block_on(wait_for_reaction(
///         &mut rx,
///         &[WindowStatus::Running],
///         submitted_at,
///         Duration::from_millis(20),
///     ));
/// assert_eq!(outcome, ReactionOutcome::Quiet);
/// ```
pub async fn wait_for_reaction(
    rx: &mut broadcast::Receiver<StatusAt>,
    accept: &[WindowStatus],
    since: Instant,
    timeout: Duration,
) -> ReactionOutcome {
    if let Some(outcome) = react_to_backlog(rx, accept, since) {
        return outcome;
    }

    let awaited = tokio::time::timeout(timeout, async {
        loop {
            match rx.recv().await {
                Ok(at) => {
                    if let Some(outcome) = reaction_to(at, accept, since) {
                        return outcome;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    return ReactionOutcome::Unobservable;
                }
                Err(broadcast::error::RecvError::Closed) => {
                    return ReactionOutcome::Unobservable;
                }
            }
        }
    })
    .await;

    match awaited {
        Ok(outcome) => outcome,
        Err(_) => ReactionOutcome::Quiet,
    }
}

/// What one change seen by [`wait_for_reaction`] settles, if anything. A
/// change whose evidence predates `since` settles nothing.
fn reaction_to(at: StatusAt, accept: &[WindowStatus], since: Instant) -> Option<ReactionOutcome> {
    if at.evidence_since < since {
        return None;
    }
    let status = at.status.status();
    if accept.contains(&status) {
        Some(ReactionOutcome::Reacted(status))
    } else if status == WindowStatus::Exited {
        Some(ReactionOutcome::Exited)
    } else {
        None
    }
}

/// Reads whatever `rx` already holds without waiting and without consulting
/// a clock, judging each change by [`reaction_to`]. A `Lagged` here is not an outcome: it dropped
/// the oldest queued changes, and the rest are still read.
fn react_to_backlog(
    rx: &mut broadcast::Receiver<StatusAt>,
    accept: &[WindowStatus],
    since: Instant,
) -> Option<ReactionOutcome> {
    loop {
        match rx.try_recv() {
            Ok(at) => {
                if let Some(outcome) = reaction_to(at, accept, since) {
                    return Some(outcome);
                }
            }
            Err(broadcast::error::TryRecvError::Lagged(_)) => {}
            Err(broadcast::error::TryRecvError::Empty | broadcast::error::TryRecvError::Closed) => {
                return None;
            }
        }
    }
}

/// Whether `current` settles the wait for any of `targets`, and if so, how.
/// `None` means the loop must keep waiting for the next change.
fn settle(current: DetectedStatus, targets: &[WindowStatus]) -> Option<WaitOutcome> {
    let status = current.status();
    if status == WindowStatus::Unknown {
        // Fail-safe: never settle while Unknown, even when a target itself is
        // Unknown.
        return None;
    }
    if targets.contains(&status) {
        return Some(WaitOutcome::Reached);
    }
    if status == WindowStatus::Exited {
        return Some(WaitOutcome::ChildExited { current });
    }
    None
}
