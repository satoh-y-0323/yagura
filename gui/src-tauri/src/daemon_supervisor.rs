//! The subject that keeps the daemon alive.
//!
//! It owns the recovery state, observes the control connection once per
//! tick, and carries out what `recovery_state` decides. It is the only
//! module that starts the daemon on the application's own initiative; every
//! other start is a consequence of a user action.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use tauri::{AppHandle, Manager};

use crate::client_config::{
    ClientConfigLoadResult, client_config_without_base_dir, load_client_config,
};
use crate::connection_pool::ConnectionPool;
use crate::events::{DaemonRecoveryPayload, EVENT_DAEMON_RECOVERY};
use crate::host::GuiHost;
use crate::instance::StartupContext;
use crate::recovery_state::{
    RecoveryObservation, RecoveryState, RecoveryStatusReport, RecoveryUpdate,
    rearm_recovery_attempts,
};

/// Name of the variable that overrides automatic respawning.
const AUTO_RESPAWN_ENV: &str = "YAGURA_GUI_AUTO_RESPAWN";

/// The recovery state as the application holds it: shared with the tick loop
/// and reachable from the commands through Tauri's managed state.
///
/// A blocking mutex on purpose: holding its guard across an `await` does not
/// compile, so "decide, then act" is enforced by the compiler rather than by
/// review.
pub(crate) type RecoverySupervisorState = Arc<Mutex<RecoveryState>>;

/// Installs the supervisor: resolves the client settings, registers the
/// recovery state as managed state, and starts the tick loop.
///
/// This is the only entry point the binary target calls.
///
/// The loop runs whether or not automatic respawning is enabled: the setting
/// is honoured in the one place that decides whether to start the daemon, so
/// there is no second switch here that could disagree with it. It is not
/// started at all when `startup` failed to resolve an instance:
/// this is an optimization against pointless retries
/// against a daemon this process must not spawn, not the guarantee that a
/// resolution failure stays inert -- that guarantee is the two type-level
/// choke points `startup` itself is threaded through.
///
/// Never returns `Err` and never panics: settings that cannot be read leave
/// the defaults in force and are reported to the WebView instead.
pub fn install(app: &AppHandle, pool: Arc<ConnectionPool>, startup: &StartupContext) {
    let startup_env_failure = startup.resolution().as_ref().err().copied();
    let state: RecoverySupervisorState = Arc::new(Mutex::new(RecoveryState::new(
        resolve_config(startup),
        startup_env_failure,
    )));
    app.manage(Arc::clone(&state));
    if startup.resolution().is_ok() {
        tauri::async_runtime::spawn(run_recovery_loop(app.clone(), pool, state));
    }
}

/// Starts the daemon at the user's explicit request, putting the attempt
/// budget back first so the automatic loop resumes afterwards.
///
/// Never returns `Err` and never panics: a start that does not take is seen
/// by the next tick rather than reported here.
pub(crate) async fn trigger_manual_reconnect(
    app: &impl GuiHost,
    pool: &Arc<ConnectionPool>,
    state: &RecoverySupervisorState,
) {
    rearm_recovery_attempts(&mut lock_state(state));
    let _ = pool.ensure_daemon_running(app).await;
}

/// The recovery status as it stands, for a WebView that has just started
/// listening and so may have missed the change that produced it.
pub(crate) fn recovery_snapshot(state: &RecoverySupervisorState) -> RecoveryStatusReport {
    lock_state(state).snapshot()
}

/// Observes the daemon once per tick and carries out what the state machine
/// returns, in the order it prescribes: publish what was observed, then act
/// on it, then wait.
///
/// The result of starting the daemon is deliberately dropped: whether it
/// took is a question the next observation answers, and a start that
/// reported success is no evidence the connection is back.
async fn run_recovery_loop<H: GuiHost>(
    app: H,
    pool: Arc<ConnectionPool>,
    state: RecoverySupervisorState,
) {
    loop {
        let observation = observe(&pool).await;
        let action = lock_state(&state).next_action(observation);
        if let Some(update) = action.emit {
            publish(&app, update);
        }
        if action.spawn {
            let _ = pool.ensure_daemon_running(&app).await;
        }
        tokio::time::sleep(action.wait).await;
    }
}

/// What this tick found: whether the daemon answers, and whether there is
/// anything left to recover for.
async fn observe(pool: &ConnectionPool) -> RecoveryObservation {
    RecoveryObservation {
        control_connected: pool.has_live_control_connection().await,
        holds_windows: !pool.window_ids().await.is_empty(),
    }
}

/// Pushes a status change to the WebView.
fn publish(app: &impl GuiHost, update: RecoveryUpdate) {
    app.emit_event(
        EVENT_DAEMON_RECOVERY,
        DaemonRecoveryPayload {
            status: update.status,
            rev: update.rev,
        },
    );
}

/// Reads the settings once, at startup, from the environment and then
/// `startup`'s configuration root.
///
/// Whether the variable's name matches case-insensitively is left to the
/// host: [`std::env::var`] looks it up the way the platform stores it, and
/// no branch here second-guesses that.
///
/// A root that cannot be resolved -- whether because `startup` failed to
/// resolve an instance or because the configuration base directory itself is
/// unavailable -- is the same situation as the file being absent: the
/// defaults stand, and the environment value still decides whether the
/// supervisor may act.
fn resolve_config(startup: &StartupContext) -> ClientConfigLoadResult {
    let auto_respawn_env = std::env::var(AUTO_RESPAWN_ENV).ok();
    match startup.config_root() {
        Some(root) => load_client_config(&root, auto_respawn_env.as_deref()),
        None => client_config_without_base_dir(auto_respawn_env.as_deref()),
    }
}

/// Takes the state lock, carrying on through a poisoned one.
///
/// What the mutex guards is plain data with no invariant a panicking holder
/// could have left half-applied, so refusing to look at it would only turn
/// one lost tick into a supervisor that never observes anything again.
fn lock_state(state: &Mutex<RecoveryState>) -> MutexGuard<'_, RecoveryState> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::events::EVENT_DAEMON_RECOVERY;
    use crate::host::fake::FakeHost;
    use crate::ipc_client::{IpcClient, IpcError};

    /// A recovery state that has already seen the daemon answer, as an earlier
    /// tick would have left it: recovery is for a daemon that went away.
    fn state_that_saw_the_daemon() -> RecoverySupervisorState {
        let state: RecoverySupervisorState = Arc::new(Mutex::new(RecoveryState::new(
            client_config_without_base_dir(None),
            None,
        )));
        let _ = lock_state(&state).next_action(RecoveryObservation {
            control_connected: true,
            holds_windows: true,
        });
        state
    }

    async fn pool_holding_a_window() -> Arc<ConnectionPool> {
        let pool = Arc::new(ConnectionPool::new());
        let failed = pool
            .connect_windows(&["a".to_owned()], |_id: String| async move {
                Ok::<_, IpcError>(Arc::new(IpcClient::new()))
            })
            .await;
        assert!(failed.is_empty());
        pool
    }

    #[tokio::test]
    async fn a_daemon_that_went_away_with_windows_open_is_started_and_the_recovery_published() {
        let host = FakeHost::new("recovery");
        let recovery = tokio::spawn(run_recovery_loop(
            host.clone(),
            pool_holding_a_window().await,
            state_that_saw_the_daemon(),
        ));

        assert_eq!(
            host.next_event(EVENT_DAEMON_RECOVERY).await,
            json!({ "status": "retrying", "rev": 1 })
        );
        host.wait_for_spawns(1).await;
        recovery.abort();
    }

    #[tokio::test]
    async fn a_manual_reconnect_puts_the_attempt_budget_back_and_starts_the_daemon() {
        let host = FakeHost::new("manual-reconnect");
        let pool = Arc::new(ConnectionPool::new());
        let state = state_that_saw_the_daemon();
        let outage = RecoveryObservation {
            control_connected: false,
            holds_windows: true,
        };
        while lock_state(&state).next_action(outage).spawn {}
        assert!(!lock_state(&state).next_action(outage).spawn);

        trigger_manual_reconnect(&host, &pool, &state).await;

        assert_eq!(host.spawn_count(), 1);
        assert!(lock_state(&state).next_action(outage).spawn);
    }

    #[tokio::test]
    async fn an_empty_pool_is_observed_holding_no_windows() {
        let observation = observe(&ConnectionPool::new()).await;

        assert!(!observation.holds_windows);
    }

    #[tokio::test]
    async fn a_pool_with_a_window_is_observed_holding_windows() {
        let pool = ConnectionPool::new();
        let failed = pool
            .connect_windows(&["a".to_owned()], |_id: String| async move {
                Ok::<_, IpcError>(Arc::new(IpcClient::new()))
            })
            .await;
        assert!(failed.is_empty());

        assert!(observe(&pool).await.holds_windows);
    }
}
