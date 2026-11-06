//! The window's one attached view: what its client is shown, one frame at a
//! time, each waiting for the client's acknowledgement of the one before.

use yagura_protocol::holder::{HolderFrame, HolderViewNote};
use yagura_term::VtScreen;
use yagura_term::screen_view::View;

/// The attach that holds the window's view, if any.
#[derive(Default)]
pub(crate) struct Attached {
    view: Option<(u64, View)>,
}

impl Attached {
    /// A client is shown the window as `generation`, superseding the attach
    /// before: every row goes in the first frame.
    pub(crate) fn attach(
        &mut self,
        generation: u64,
        screen: &mut VtScreen,
        child_exited: bool,
    ) -> Vec<HolderFrame> {
        let mut view = View::new();
        let capture = view.capture(screen, child_exited, true);
        self.view = Some((generation, view));
        frames_of(generation, capture)
    }

    /// Lets go of `generation` if it still holds the view.
    pub(crate) fn detach(&mut self, generation: u64) {
        if self
            .view
            .as_ref()
            .is_some_and(|(held, _)| *held == generation)
        {
            self.view = None;
        }
    }

    /// Lets go of whichever attach holds the view.
    pub(crate) fn clear(&mut self) {
        self.view = None;
    }

    /// What the client of `generation` told its view.
    pub(crate) fn note(&mut self, generation: u64, note: HolderViewNote) {
        let Some((held, view)) = self.view.as_mut() else {
            return;
        };
        if *held != generation {
            return;
        }
        match note {
            HolderViewNote::Ack { seq } => view.acknowledge(seq),
            HolderViewNote::Visible { visible } => view.set_visible(visible),
        }
    }

    /// Something the view shows may have changed.
    pub(crate) fn note_change(&mut self) {
        if let Some((_, view)) = self.view.as_mut() {
            view.note_change();
        }
    }

    /// A write to the child, which may end a synchronized update that holds
    /// the view.
    pub(crate) fn note_write(&mut self) {
        if let Some((_, view)) = self.view.as_mut()
            && view.is_held()
        {
            view.note_change();
        }
    }

    /// The view's next frame, if one is due.
    pub(crate) fn frames(&mut self, screen: &mut VtScreen, child_exited: bool) -> Vec<HolderFrame> {
        let Some((generation, view)) = self.view.as_mut() else {
            return Vec::new();
        };
        if !view.wants_frame() {
            return Vec::new();
        }
        let capture = view.capture(screen, child_exited, false);
        frames_of(*generation, capture)
    }
}

fn frames_of(
    generation: u64,
    capture: Option<yagura_term::screen_view::FrameCapture>,
) -> Vec<HolderFrame> {
    capture
        .map(|capture| {
            capture
                .into_parts()
                .into_iter()
                .map(|frame| HolderFrame::ViewFrame { generation, frame })
                .collect()
        })
        .unwrap_or_default()
}
