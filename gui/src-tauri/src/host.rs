//! The seam between the GUI's decisions and the Tauri application handle.
//!
//! Connecting, reconnecting, registering as an approver, deciding whether to
//! start the daemon and routing what the daemon sends all branch on state
//! this crate owns. What they need from the running application is the five
//! operations of [`GuiHost`]; the production implementation is the
//! application handle, and a test supplies one that records instead.

use std::future::Future;
use std::sync::Arc;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

use crate::daemon_launch;
use crate::events::EVENT_DAEMON_SPAWN_WARNING;
use crate::frame_mailbox::FrameMailbox;
use crate::instance::StartupContext;
use crate::ipc_client::{IpcError, current_endpoint_with};

#[cfg(test)]
pub(crate) mod fake;

/// What the GUI's connection, dispatch and supervision logic asks of the
/// running application.
///
/// Every method is a plain delegation: a decision belongs in the caller, so
/// that a host which records can stand in for the application without
/// changing what is decided.
pub trait GuiHost: Clone + Send + Sync + 'static {
    /// The daemon endpoint of the instance this process resolved to.
    ///
    /// # Errors
    /// Returns [`IpcError::NotConnected`] when start-up resolved no instance,
    /// and [`IpcError::EndpointName`] when the current user's identifier is
    /// unavailable.
    fn endpoint(&self) -> Result<String, IpcError>;

    /// Starts the daemon detached.
    ///
    /// # Errors
    /// Returns [`IpcError::DaemonNotFound`] or [`IpcError::DaemonSpawn`] when
    /// the daemon cannot be located or started.
    fn spawn_daemon(&self) -> Result<(), IpcError>;

    /// Emits `event` to the WebView, ignoring the (window-less) failure case.
    ///
    /// Emission failures are not actionable here: the GUI has no logging
    /// channel of its own and a missing window means there is nothing to
    /// notify.
    fn emit_event<P: Serialize + Clone>(&self, event: &str, payload: P);

    /// The mailbox the WebView pulls frames from, `None` when the
    /// application manages none.
    fn frame_mailbox(&self) -> Option<&Arc<FrameMailbox>>;

    /// Runs `task` on the application's async runtime, detached.
    fn spawn_task<F>(&self, task: F)
    where
        F: Future<Output = ()> + Send + 'static;
}

impl GuiHost for AppHandle {
    fn endpoint(&self) -> Result<String, IpcError> {
        current_endpoint_with(self.state::<StartupContext>().resolution())
    }

    fn spawn_daemon(&self) -> Result<(), IpcError> {
        let startup = self.state::<StartupContext>();
        daemon_launch::spawn_daemon(&startup, |warning| {
            self.emit_event(EVENT_DAEMON_SPAWN_WARNING, warning);
        })
    }

    fn emit_event<P: Serialize + Clone>(&self, event: &str, payload: P) {
        let _ = self.emit(event, payload);
    }

    fn frame_mailbox(&self) -> Option<&Arc<FrameMailbox>> {
        self.try_state::<Arc<FrameMailbox>>()
            .map(|mailbox| mailbox.inner())
    }

    fn spawn_task<F>(&self, task: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        tauri::async_runtime::spawn(task);
    }
}
