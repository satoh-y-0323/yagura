//! The blocking-free waiting primitive behind the `wait_for_status` MCP tool.
//!
//! Only the waiting loops live here; task spawning, per-connection concurrency
//! limits and abort-on-disconnect belong to `ipc.rs`. A waiter holds nothing
//! but a status receiver, never a reference back to the `WindowEntry` that
//! produced it.

use std::future::Future;
use std::time::Duration;

use tokio::sync::watch;
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
    mut rx: watch::Receiver<StatusAt>,
    targets: &[WindowStatus],
    timeout: Duration,
) -> impl Future<Output = WaitOutcome> + Send + 'static {
    let targets = targets.to_vec();
    async move {
        let awaited = tokio::time::timeout(timeout, async {
            loop {
                let observed = *rx.borrow_and_update();
                match settle(observed.status, &targets) {
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
