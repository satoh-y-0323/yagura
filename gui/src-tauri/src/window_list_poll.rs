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
    // A failed round emits nothing, so no window disappears. The records follow
    // this list, though a later list may reach the WebView before it.
    let Ok(windows) = pool.listed_windows().await else {
        return;
    };
    app.emit_event(
        EVENT_WINDOW_LIST,
        WindowListPayload {
            windows: windows.clone(),
        },
    );
    pool.reconcile(app, &windows).await;
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use yagura_protocol::{
        ClientMessage, DaemonMessage, InterruptOutcome, PROTOCOL_REVISION, WindowStatus,
    };

    use super::*;
    use crate::host::fake::{
        FakeHost, Reaction, ScriptedDaemon, attached, protocol_revision_at, window_info,
    };
    use crate::ipc_client::IpcError;

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
            json!({ "windows": listed_windows() })
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

    #[tokio::test]
    async fn a_poll_of_a_daemon_of_another_wire_revision_lists_no_windows_and_writes_nothing_but_the_probe()
     {
        let daemon = ScriptedDaemon::new(|request| match request {
            ClientMessage::GetProtocolRevision => {
                Reaction::reply(protocol_revision_at(PROTOCOL_REVISION + 1, None))
            }
            ClientMessage::ListWindows => Reaction::reply(DaemonMessage::WindowList {
                windows: vec![window_info("w1", WindowStatus::Running)],
            }),
            ClientMessage::AttachView { .. } => Reaction::reply(attached(false)),
            ClientMessage::CloseWindow { window_id, .. } => {
                Reaction::reply(DaemonMessage::WindowClosed {
                    window_id: window_id.clone(),
                })
            }
            ClientMessage::InterruptWindow { window_id } => {
                Reaction::reply(DaemonMessage::WindowInterrupted {
                    window_id: window_id.clone(),
                    outcome: InterruptOutcome::Interrupted,
                })
            }
            _ => None,
        });
        let host = FakeHost::served_by("poll-incompatible", &daemon).await;
        let pool = ConnectionPool::new();
        pool.track_window_list(&[window_info("w1", WindowStatus::Running)])
            .await;

        poll_window_list(&pool, &host).await;

        // The window recorded before belongs to no daemon this build can talk
        // to, so the list it is shown is empty and its record goes.
        let published = host.next_event(EVENT_WINDOW_LIST).await;
        let connected = pool.window_ids().await;
        let attach = pool.attach_window(&host, "w1", None).await;
        // A close or interrupt fails for the daemon's wire revision, not for
        // a registration it was never asked for.
        let closed = pool.close_window(&host, "w1", None).await;
        let interrupted = pool.interrupt_window(&host, "w1").await;
        assert_eq!(
            (
                published["windows"].clone(),
                connected,
                matches!(&attach, Err(IpcError::WindowNotAttached { window_id }) if window_id == "w1"),
                matches!(closed, Err(IpcError::Unsupported { .. })),
                matches!(interrupted, Err(IpcError::Unsupported { .. })),
                daemon.received(),
            ),
            (
                json!([]),
                Vec::<String>::new(),
                true,
                true,
                true,
                vec![ClientMessage::GetProtocolRevision],
            ),
            "attach: {attach:?}, closed: {closed:?}, interrupted: {interrupted:?}"
        );
    }
}
