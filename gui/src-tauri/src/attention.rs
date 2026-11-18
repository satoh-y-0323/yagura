//! The outward signal that windows are waiting for the user: the count in the
//! main window's title and the request for the user's attention.
//!
//! The decision is a pure state machine ([`AttentionState::observe`]) so it can
//! be driven without a window; [`crate::commands::set_attention_summary`]
//! applies its result to the main window.

use crate::instance::compose_window_title;

/// Label of the application's single main window (`tauri.conf.json`).
pub const MAIN_WINDOW_LABEL: &str = "main";

/// What one observation asks the main window to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttentionUpdate {
    /// The window title, composed from the stem captured at startup.
    pub title: String,
    /// The number of things waiting for the user that the title shows.
    pub total: u32,
    /// Whether to request the user's attention for this observation.
    pub request_attention: bool,
}

/// The attention decision: remembers the title stem and instance id captured
/// at startup and the previously observed waiting total.
#[derive(Debug, Clone)]
pub struct AttentionState {
    stem: String,
    id: Option<String>,
    last_total: Option<u32>,
}

impl AttentionState {
    /// Starts with no baseline, so the first observation only records its total.
    #[must_use]
    pub fn new(stem: &str, id: Option<&str>) -> Self {
        Self {
            stem: stem.to_owned(),
            id: id.map(str::to_owned),
            last_total: None,
        }
    }

    /// The title for `total` waiting things, always built from the startup
    /// stem so earlier counts never accumulate in it.
    #[must_use]
    pub fn title(&self, total: u32) -> String {
        compose_window_title(&self.stem, self.id.as_deref(), total)
    }

    /// Records one summary from the view and decides what to show.
    ///
    /// Attention is requested only when the total rises above a recorded
    /// baseline while the main window lacks focus. `baseline` drops the
    /// baseline (the view is about to reload its whole list, e.g. after a
    /// reconnect), so the next observation only records its total.
    pub fn observe(
        &mut self,
        needs_you: u32,
        pending_permissions: u32,
        baseline: bool,
        focused: bool,
    ) -> AttentionUpdate {
        let total = needs_you.saturating_add(pending_permissions);
        let request_attention = if baseline {
            self.last_total = None;
            false
        } else {
            let rose = self.last_total.is_some_and(|last| total > last);
            self.last_total = Some(total);
            rose && !focused
        };
        AttentionUpdate {
            title: self.title(total),
            total,
            request_attention,
        }
    }
}
