//! Sending the daemon copies of the screen: one ordinary copy per
//! `PushAck`, made from the screen as it stands when the copy goes out, and
//! the copies an exit or a resize owes sent at once.

use yagura_protocol::holder::HolderFrame;
use yagura_protocol::{SCREEN_ROWS_BUDGET, ScreenCopy};
use yagura_term::Projector;

/// What a copy was made from: the terminal thread's count of changes to the
/// screen (output taken in, resizes) and the model's rebuild count. A copy
/// is owed whenever this moves past the one last sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Stamp {
    pub(crate) changes: u64,
    pub(crate) generation: u64,
}

/// The copies sent on one connection.
#[derive(Default)]
pub(crate) struct Push {
    projector: Projector,
    /// The number of the last copy sent, until its `PushAck`.
    awaiting: Option<u64>,
    /// What the last copy sent was made from.
    sent: Option<Stamp>,
}

impl Push {
    /// Starts over for a new connection: the next copy goes whole.
    pub(crate) fn reset(&mut self) {
        *self = Self::default();
    }

    /// The daemon applied copy `number`.
    pub(crate) fn acknowledge(&mut self, number: u64) {
        if self.awaiting == Some(number) {
            self.awaiting = None;
        }
    }

    /// The daemon refused the last copy: the next one goes whole, and is
    /// made once the screen changes again.
    pub(crate) fn resync(&mut self) {
        self.projector.resync();
        self.awaiting = None;
    }

    /// Whether an ordinary copy is due for a screen made from `stamp`.
    pub(crate) fn due(&self, stamp: Stamp) -> bool {
        self.awaiting.is_none() && self.sent != Some(stamp)
    }

    /// Whether the last copy sent was made from `stamp`.
    pub(crate) fn holds(&self, stamp: Stamp) -> bool {
        self.sent == Some(stamp)
    }

    /// The frames of `copy`, made from `stamp`, and the copy's number, which
    /// the next ordinary copy waits on. `None`, with nothing recorded as
    /// sent, for a copy with a row too large for a frame of its own: that is
    /// a defect of the model's bounds, so the copy is logged and dropped
    /// rather than sent past the wire's limits.
    pub(crate) fn frames(&mut self, copy: ScreenCopy, stamp: Stamp) -> Option<Vec<HolderFrame>> {
        if let Some(line) = copy
            .lines
            .iter()
            .find(|line| line.json_bound() > SCREEN_ROWS_BUDGET)
        {
            crate::log(format_args!(
                "a screen copy was not sent: row {} may take {} bytes, past a frame's {}",
                line.line,
                line.json_bound(),
                SCREEN_ROWS_BUDGET
            ));
            return None;
        }
        let (rows, push) = self.projector.project(copy);
        self.awaiting = Some(push.number);
        self.sent = Some(stamp);
        let mut frames: Vec<HolderFrame> = rows.into_iter().map(HolderFrame::ScreenRows).collect();
        frames.push(HolderFrame::ScreenPush(push));
        Some(frames)
    }
}
