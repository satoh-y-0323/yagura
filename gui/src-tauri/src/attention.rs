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
/// at startup. It holds no per-observation state.
#[derive(Debug, Clone)]
pub struct AttentionState {
    stem: String,
    id: Option<String>,
}

impl AttentionState {
    /// Captures the title stem and instance id.
    #[must_use]
    pub fn new(stem: &str, id: Option<&str>) -> Self {
        Self {
            stem: stem.to_owned(),
            id: id.map(str::to_owned),
        }
    }

    /// The title for `total` waiting things, always built from the startup
    /// stem so earlier counts never accumulate in it.
    #[must_use]
    pub fn title(&self, total: u32) -> String {
        compose_window_title(&self.stem, self.id.as_deref(), total)
    }

    /// Decides what to show for one summary from the view.
    ///
    /// The title and badge follow the sum of the counts. Attention is
    /// requested only when the view reports that something `arrived` and the
    /// main window lacks focus; a rising total alone is not an arrival.
    #[must_use]
    pub fn observe(
        &self,
        needs_you: u32,
        pending_permissions: u32,
        arrived: bool,
        focused: bool,
    ) -> AttentionUpdate {
        let total = needs_you.saturating_add(pending_permissions);
        AttentionUpdate {
            title: self.title(total),
            total,
            request_attention: arrived && !focused,
        }
    }
}
