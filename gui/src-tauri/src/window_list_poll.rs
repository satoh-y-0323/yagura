//! The window-list poll loop.
//!
//! Held apart from the pool it drives: the pool answers requests it is
//! given, whereas this loop is the one place that asks the daemon for its
//! window list on a timer and pushes the result to the WebView.

use std::sync::Arc;

use crate::connection_pool::{ConnectionPool, ControlConnect};
use crate::constants::WINDOW_LIST_POLL_INTERVAL;
use crate::events::{EVENT_WINDOW_LIST, WindowListPayload};
use crate::host::GuiHost;

/// Polls the daemon for its window list every [`WINDOW_LIST_POLL_INTERVAL`],
/// pushes it to the WebView, and keeps the pool in step with it.
///
/// The loop lives in Tauri core because the interval is a Rust-side constant.
/// The daemon is never started from here (a poll that spawned it would retry
/// every tick when the start keeps failing); explicit commands do that.
pub async fn run_window_list_poll(pool: Arc<ConnectionPool>, app: impl GuiHost) {
    poll_every(WINDOW_LIST_POLL_INTERVAL, &pool, &app).await;
}

/// The loop of [`run_window_list_poll`] with its interval given, so a test
/// can run it without waiting for the production one.
async fn poll_every(interval: std::time::Duration, pool: &ConnectionPool, app: &impl GuiHost) {
    loop {
        tokio::time::sleep(interval).await;
        poll_window_list(pool, app).await;
    }
}

/// One tick of [`run_window_list_poll`].
async fn poll_window_list(pool: &ConnectionPool, app: &impl GuiHost) {
    if pool
        .control_connected(app, ControlConnect::Existing)
        .await
        .is_err()
    {
        return;
    }
    // An already-connected control returns early without reaching the
    // registration retry, so this tick is what retries a generation that
    // is neither registered nor refused; a refused generation stays
    // latched until a reconnect advances it.
    pool.control_connection()
        .ensure_approver_registered(app)
        .await;
    pool.control_connection().ensure_protocol_revision().await;
    // A round that fails emits nothing, so the WebView keeps the list it
    // has instead of seeing every window disappear.
    let Ok(windows) = pool.control_connection().list_windows().await else {
        return;
    };
    app.emit_event(
        EVENT_WINDOW_LIST,
        WindowListPayload {
            windows: windows.clone(),
            protocol_revision: pool.control_connection().protocol_revision(),
        },
    );
    pool.reconcile(app, &windows).await;
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use yagura_protocol::{ClientMessage, DaemonMessage, PROTOCOL_REVISION, WindowStatus};

    use super::*;
    use crate::host::fake::{FakeHost, Reaction, ScriptedDaemon, window_info};

    /// A daemon with a running window and an exited one, that lets windows
    /// attach.
    fn listing_daemon() -> ScriptedDaemon {
        ScriptedDaemon::new(|request| match request {
            ClientMessage::ListWindows => Reaction::reply(DaemonMessage::WindowList {
                windows: vec![
                    window_info("w1", WindowStatus::Running),
                    window_info("w2", WindowStatus::Exited),
                ],
            }),
            ClientMessage::AttachView { .. } => Reaction::reply(DaemonMessage::Attached {
                child_exited: false,
                exit_code: None,
            }),
            _ => None,
        })
    }

    fn listed_windows() -> serde_json::Value {
        json!([
            window_info("w1", WindowStatus::Running),
            window_info("w2", WindowStatus::Exited)
        ])
    }

    #[tokio::test]
    async fn a_poll_never_starts_a_daemon_and_publishes_nothing_when_none_runs() {
        let host = FakeHost::new("poll-no-daemon");

        poll_window_list(&ConnectionPool::new(), &host).await;

        assert_eq!(host.spawn_count(), 0);
        assert!(host.take_events().is_empty());
    }

    #[tokio::test]
    async fn a_poll_publishes_the_window_list_and_connects_the_windows_that_are_alive() {
        let daemon = listing_daemon();
        let host = FakeHost::served_by("poll-lists", &daemon).await;
        let pool = ConnectionPool::new();

        poll_window_list(&pool, &host).await;

        assert_eq!(
            host.next_event(EVENT_WINDOW_LIST).await,
            json!({ "windows": listed_windows(), "protocol_revision": PROTOCOL_REVISION })
        );
        assert_eq!(pool.window_ids().await, ["w1"]);
    }

    #[tokio::test]
    async fn the_poll_loop_publishes_the_list_on_its_interval() {
        let daemon = listing_daemon();
        let host = FakeHost::served_by("poll-loop", &daemon).await;
        let pool = Arc::new(ConnectionPool::new());
        let polling = tokio::spawn({
            let host = host.clone();
            async move { poll_every(std::time::Duration::ZERO, &pool, &host).await }
        });

        let published = host.next_event(EVENT_WINDOW_LIST).await;

        assert_eq!(published["windows"], listed_windows());
        polling.abort();
    }
}
