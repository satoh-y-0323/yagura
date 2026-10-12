//! The daemon-recovery state machine.
//!
//! Every decision the supervisor takes -- whether to start the daemon, how
//! long to wait, when to give up, what to publish -- is made here, by code
//! that touches neither the application handle nor the connection pool. The
//! loop that drives it only carries out what it returns.

use std::time::Duration;

use serde::Serialize;

use crate::client_config::{ClientConfigFailure, ClientConfigLoadResult, ReconnectConfig};
use crate::constants::{MAX_RECONNECT_DELAY, TICK_INTERVAL};
use crate::instance::StartupEnvFailure;

/// How far daemon recovery has got. Serializes as a `snake_case` string, and
/// the WebView names the same three values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryStatus {
    /// There is nothing to recover from.
    Idle,
    /// The daemon is being started again.
    Retrying,
    /// The attempt limit was reached without the daemon coming back.
    Exhausted,
}

/// What a tick found before the state machine ran.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecoveryObservation {
    /// Whether the control connection is established.
    pub control_connected: bool,
    /// Whether the pool still holds at least one window.
    pub holds_windows: bool,
}

/// A status change to publish, carrying the generation that orders it
/// against every other delivery of a status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecoveryUpdate {
    /// The status the supervisor has moved to.
    pub status: RecoveryStatus,
    /// The generation of this change; it only ever grows.
    pub rev: u64,
}

/// What the supervisor must carry out for the tick that produced it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecoveryAction {
    /// Whether to run the daemon-start entry point.
    pub spawn: bool,
    /// How long to sleep before taking the next observation.
    pub wait: Duration,
    /// The status change to publish, if this tick produced one.
    pub emit: Option<RecoveryUpdate>,
}

/// What the WebView reads when it initializes, so that a status change
/// published before it subscribed is not lost.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct RecoveryStatusReport {
    /// The status as of the last tick.
    pub status: RecoveryStatus,
    /// The generation of that status.
    pub rev: u64,
    /// Why the client settings could not be loaded, if they could not be.
    pub config_failure: Option<ClientConfigFailure>,
    /// Why start-up could not resolve this process's instance, if it
    /// could not.
    pub startup_env_failure: Option<StartupEnvFailure>,
}

/// The recovery state a supervisor carries from one tick to the next.
///
/// It is plain data: no lock, no handle, no connection. The supervisor is
/// what puts it behind a mutex.
#[derive(Debug, Clone)]
pub struct RecoveryState {
    config: ReconnectConfig,
    config_failure: Option<ClientConfigFailure>,
    startup_env_failure: Option<StartupEnvFailure>,
    /// Whether a connection has been established at least once since the
    /// GUI started. Recovery is for a daemon that went away, so a process
    /// that was never reached is left to the paths a user action drives.
    ever_connected: bool,
    /// How many attempts have been made since the budget was last put back.
    attempts: u32,
    status: RecoveryStatus,
    rev: u64,
}

impl RecoveryState {
    /// Builds the state a supervisor starts from, keeping the settings and
    /// the reason they could not be loaded, if there is one, alongside the
    /// reason start-up could not resolve this process's instance, if there
    /// is one.
    pub fn new(
        config: ClientConfigLoadResult,
        startup_env_failure: Option<StartupEnvFailure>,
    ) -> Self {
        Self {
            config: config.reconnect,
            config_failure: config.failure,
            startup_env_failure,
            ever_connected: false,
            attempts: 0,
            status: RecoveryStatus::Idle,
            rev: 0,
        }
    }

    /// Folds one observation into the state and returns what the tick must
    /// do. Never panics: an attempt count past the limit yields the
    /// give-up outcome rather than overflowing.
    pub fn next_action(&mut self, observation: RecoveryObservation) -> RecoveryAction {
        if observation.control_connected {
            return self.observe_connection();
        }
        if self.may_attempt(observation) {
            return self.make_attempt();
        }
        self.observe_continued_outage(observation)
    }

    /// Reads the current status without changing it.
    pub fn snapshot(&self) -> RecoveryStatusReport {
        RecoveryStatusReport {
            status: self.status,
            rev: self.rev,
            config_failure: self.config_failure,
            startup_env_failure: self.startup_env_failure,
        }
    }

    /// The daemon answers: stop trying and put the budget back, so a later
    /// outage gets a full cycle of its own.
    fn observe_connection(&mut self) -> RecoveryAction {
        self.ever_connected = true;
        let was_recovering = matches!(
            self.status,
            RecoveryStatus::Retrying | RecoveryStatus::Exhausted
        );
        let emit = was_recovering.then(|| {
            self.attempts = 0;
            self.publish(RecoveryStatus::Idle)
        });
        RecoveryAction {
            spawn: false,
            wait: TICK_INTERVAL,
            emit,
        }
    }

    /// Spends one attempt from the budget.
    fn make_attempt(&mut self) -> RecoveryAction {
        self.attempts = self.attempts.saturating_add(1);
        let emit = (self.status != RecoveryStatus::Retrying)
            .then(|| self.publish(RecoveryStatus::Retrying));
        RecoveryAction {
            spawn: true,
            wait: self.attempt_wait(self.attempts),
            emit,
        }
    }

    /// The daemon is still unreachable and no attempt is due, either
    /// because activation stopped holding or because the budget ran out.
    /// The first ends the cycle at `Idle` with a fresh budget, since a
    /// `Retrying` that can no longer attempt anything is not trying; it
    /// takes precedence over the second, which is where the give-up
    /// becomes final (the tick that spent the last attempt only started
    /// it). `Exhausted` stands either way: it reports a cycle that ran.
    fn observe_continued_outage(&mut self, observation: RecoveryObservation) -> RecoveryAction {
        let retrying = self.status == RecoveryStatus::Retrying;
        let disengaged = retrying && !self.activation_holds(observation);
        let budget_spent = retrying && self.attempts >= self.config.attempt_limit;
        let emit = if disengaged {
            self.attempts = 0;
            Some(self.publish(RecoveryStatus::Idle))
        } else {
            budget_spent.then(|| self.publish(RecoveryStatus::Exhausted))
        };
        RecoveryAction {
            spawn: false,
            wait: TICK_INTERVAL,
            emit,
        }
    }

    /// Whether recovery has anything to act on: the settings allow it and
    /// the two activation conditions hold.
    fn activation_holds(&self, observation: RecoveryObservation) -> bool {
        self.config.auto_respawn && self.ever_connected && observation.holds_windows
    }

    /// Whether this tick may start the daemon: recovery is activated and
    /// the budget has something left.
    fn may_attempt(&self, observation: RecoveryObservation) -> bool {
        self.activation_holds(observation) && self.attempts < self.config.attempt_limit
    }

    /// The wait that follows the `attempt_number`-th attempt (1-based):
    /// the initial delay multiplied by the backoff factor once per earlier
    /// attempt, saturated at the ceiling so no configuration can grow it
    /// without bound.
    fn attempt_wait(&self, attempt_number: u32) -> Duration {
        let mut wait = Duration::from_millis(self.config.initial_delay_ms);
        for _ in 1..attempt_number {
            if wait >= MAX_RECONNECT_DELAY {
                break;
            }
            wait = wait.saturating_mul(self.config.backoff_factor);
        }
        wait.min(MAX_RECONNECT_DELAY)
    }

    /// Moves to `status` and hands back the change to publish, carrying the
    /// generation that orders it against every other one.
    fn publish(&mut self, status: RecoveryStatus) -> RecoveryUpdate {
        self.status = status;
        self.rev = self.rev.saturating_add(1);
        RecoveryUpdate {
            status,
            rev: self.rev,
        }
    }
}

/// Puts the attempt budget back, so a state that had given up can try again.
///
/// This is what an explicit user request goes through; the loop itself does
/// the same thing when it observes the daemon back. The published status is
/// left alone on purpose: a status only ever changes on an observation, so
/// the next tick is what moves it.
pub fn rearm_recovery_attempts(state: &mut RecoveryState) {
    state.attempts = 0;
}
