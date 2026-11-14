//! What the GUI remembers about one window across the connections it makes to
//! it: the state the user asked for, the state of the link, and the input
//! typed while the link was down.
//!
//! A record outlives every connection of its window, so the state a connection
//! that is gone was told is restored by the next one, from this one place.
//! [`crate::connection_pool::ConnectionPool`] owns the records; a window's
//! connection only borrows its record.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::sync::{Mutex, MutexGuard};
use yagura_protocol::{ClientMessage, MAX_PASTE_BYTES, Palette, TerminalSize};

use crate::events::{EVENT_WINDOW_LINK, LinkLostReason, WindowLinkPayload, WindowLinkState};
use crate::host::GuiHost;
use crate::ipc_client::IpcError;

/// The most input, by [`input_weight`], a window holds while it is not
/// attached: one largest paste, with as much again to spare for the keys and
/// pointer reports of the longest gap a reconnect can bridge.
pub const MAX_PENDING_INPUT_BYTES: usize = 2 * MAX_PASTE_BYTES;

/// The epoch of the latest link report of any window. Process-wide, so a
/// report always carries a larger epoch than every report before it.
static EPOCH: AtomicU64 = AtomicU64::new(0);

/// The state of a window's link to the daemon, as far as this GUI knows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkState {
    /// Not attached yet, or attaching again after the link went down.
    Connecting,
    /// Attached to a live child.
    Attached,
    /// The window could not be reached; the next window-list poll tries again.
    Lost(LinkLostReason),
    /// Another GUI took the window over. Final.
    Superseded,
    /// An attach found the child already exited. Final.
    Exited,
}

impl LinkState {
    fn is_final(self) -> bool {
        matches!(self, Self::Superseded | Self::Exited)
    }
}

/// The size a connection was last given, and which connection it was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SentSize {
    pub(crate) size: Option<TerminalSize>,
    /// The connection, as [`crate::ipc_client::IpcClient::connection_id`].
    pub(crate) connection: (u64, u64),
}

/// The contents of a [`WindowRecord`], read and changed under its lock.
pub(crate) struct RecordState {
    /// The size the view asked for, already through the size gate.
    pub(crate) size: Option<TerminalSize>,
    /// Whether the view is shown; `None` while it has only ever been shown,
    /// which the daemon assumes.
    pub(crate) visible: Option<bool>,
    pub(crate) palette: Option<Palette>,
    pub(crate) sent: Option<SentSize>,
    pub(crate) link: LinkState,
    epoch: u64,
    pending: VecDeque<ClientMessage>,
    pending_bytes: usize,
    /// Set once an input did not fit; every later one is refused until the
    /// next attach, so what is delivered is an unbroken run from the start.
    past_bound: bool,
}

// By hand, so a debug print never carries what the user typed: the held
// input shows as its count and weight only.
impl std::fmt::Debug for RecordState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RecordState")
            .field("size", &self.size)
            .field("visible", &self.visible)
            .field("palette", &self.palette)
            .field("sent", &self.sent)
            .field("link", &self.link)
            .field("epoch", &self.epoch)
            .field("pending", &self.pending.len())
            .field("pending_bytes", &self.pending_bytes)
            .field("past_bound", &self.past_bound)
            .finish()
    }
}

/// One window's record. See the module documentation.
#[derive(Debug)]
pub struct WindowRecord {
    state: Mutex<RecordState>,
    /// The serial of the connection that may change the record. Written when
    /// a connection is built for the record, before it can act; read under
    /// the lock by every change a connection makes.
    owner: AtomicU64,
}

impl Default for WindowRecord {
    fn default() -> Self {
        Self::new()
    }
}

impl WindowRecord {
    /// A record of a window nothing has been asked of yet, about to attach.
    pub fn new() -> Self {
        Self {
            state: Mutex::new(RecordState {
                size: None,
                visible: None,
                palette: None,
                sent: None,
                link: LinkState::Connecting,
                epoch: 0,
                pending: VecDeque::new(),
                pending_bytes: 0,
                past_bound: false,
            }),
            owner: AtomicU64::new(0),
        }
    }

    pub(crate) async fn lock(&self) -> MutexGuard<'_, RecordState> {
        self.state.lock().await
    }

    pub(crate) fn owner(&self) -> u64 {
        self.owner.load(Ordering::SeqCst)
    }

    pub(crate) fn set_owner(&self, serial: u64) {
        self.owner.store(serial, Ordering::SeqCst);
    }
}

impl RecordState {
    /// Moves the link to `link` and reports it, unless the link has already
    /// reached a final state. Leaving for a state that cannot deliver drops
    /// the held input.
    pub(crate) fn move_to(&mut self, app: &impl GuiHost, window_id: &str, link: LinkState) {
        if self.link.is_final() {
            return;
        }
        self.link = link;
        if matches!(
            link,
            LinkState::Lost(_) | LinkState::Superseded | LinkState::Exited
        ) {
            self.drop_pending();
        }
        self.report(app, window_id);
    }

    /// Reports the current link again with a new epoch, even when it has not
    /// changed: every attach reports its outcome, and the WebView discards a
    /// report that is not newer than the last it applied.
    pub(crate) fn report(&mut self, app: &impl GuiHost, window_id: &str) {
        self.epoch = EPOCH.fetch_add(1, Ordering::SeqCst) + 1;
        let (state, reason) = match self.link {
            LinkState::Connecting => (WindowLinkState::Connecting, None),
            LinkState::Attached => (WindowLinkState::Attached, None),
            LinkState::Lost(reason) => (WindowLinkState::Lost, Some(reason)),
            LinkState::Superseded => (WindowLinkState::Superseded, None),
            // An exit is reported through `EVENT_EXIT`, by the attach that
            // found it.
            LinkState::Exited => return,
        };
        app.emit_event(
            EVENT_WINDOW_LINK,
            WindowLinkPayload {
                window_id: window_id.to_owned(),
                state,
                reason,
                epoch: self.epoch,
            },
        );
    }

    /// Holds `input` until the next attach.
    ///
    /// # Errors
    /// Returns [`IpcError::InvalidArgument`] when the held input would pass
    /// [`MAX_PENDING_INPUT_BYTES`], and for every input after one that did,
    /// until the next attach.
    pub(crate) fn hold(&mut self, input: &ClientMessage) -> Result<(), IpcError> {
        let weight = input_weight(input);
        if self.past_bound || self.pending_bytes + weight > MAX_PENDING_INPUT_BYTES {
            self.past_bound = true;
            return Err(IpcError::InvalidArgument(
                "input held while the window reconnects is past its bound",
            ));
        }
        self.pending.push_back(input.clone());
        self.pending_bytes += weight;
        Ok(())
    }

    pub(crate) fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    /// The oldest held input, left in place until [`Self::sent_first`].
    pub(crate) fn first_pending(&self) -> Option<&ClientMessage> {
        self.pending.front()
    }

    /// Forgets the oldest held input once it has been written.
    pub(crate) fn sent_first(&mut self) {
        if let Some(sent) = self.pending.pop_front() {
            self.pending_bytes -= input_weight(&sent);
        }
    }

    /// Lowers the bound's mark once everything held has been written.
    pub(crate) fn release_bound_if_drained(&mut self) {
        if self.pending.is_empty() {
            self.past_bound = false;
        }
    }

    fn drop_pending(&mut self) {
        self.pending.clear();
        self.pending_bytes = 0;
        self.past_bound = false;
    }
}

/// What holding `input` costs in memory: the message itself and the text it
/// carries.
fn input_weight(input: &ClientMessage) -> usize {
    let text = match input {
        ClientMessage::Text { text } | ClientMessage::Paste { text } => text.len(),
        _ => 0,
    };
    std::mem::size_of::<ClientMessage>() + text
}
