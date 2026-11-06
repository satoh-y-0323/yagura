//! A screen's copies as a reader is sent them: the first one whole, every one
//! after it as the rows that differ from the copy before.

use yagura_protocol::{SCREEN_ROWS_BUDGET, ScreenCopy, ScreenLine, ScreenPush, ScreenRows};

/// Turns one screen's successive copies into what is sent for each: the rows
/// in frames of at most [`SCREEN_ROWS_BUDGET`] bytes, then the close.
///
/// The copy last sent is kept whole and compared cell by cell, so a row is
/// left out only when it is exactly what the reader already holds.
#[derive(Debug, Default)]
pub struct Projector {
    sent: Option<ScreenCopy>,
    number: u64,
}

impl Projector {
    /// A projector that has sent nothing, so its first copy goes whole.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// What to send for `copy`: whole when nothing was sent before, else the
    /// rows that differ from the copy last sent, cut to `copy`'s size the way
    /// the reader cuts it ([`ScreenCopy::lines_resized`]). Every call takes a
    /// new number, and `copy` becomes the copy last sent.
    pub fn project(&mut self, copy: ScreenCopy) -> (Vec<ScreenRows>, ScreenPush) {
        self.number = self.number.wrapping_add(1);
        let full = self.sent.is_none();
        let changed: Vec<ScreenLine> = match &self.sent {
            None => copy
                .lines
                .iter()
                .filter(|line| !line.cells.is_empty())
                .cloned()
                .collect(),
            Some(sent) => {
                let base = sent.lines_resized(copy.cols, copy.rows);
                copy.lines
                    .iter()
                    .filter(|line| {
                        base.get(usize::from(line.line))
                            .is_none_or(|held| held.cells != line.cells)
                    })
                    .cloned()
                    .collect()
            }
        };
        let rows = into_frames(self.number, changed);
        let push = ScreenPush {
            number: self.number,
            full,
            cols: copy.cols,
            rows: copy.rows,
            cursor: copy.cursor,
            cursor_hidden: copy.cursor_hidden,
            sync_open: copy.sync_open,
            input_modes: copy.input_modes,
            generation: copy.generation,
            outputs_fed: copy.outputs_fed,
        };
        self.sent = Some(copy);
        (rows, push)
    }

    /// Forgets the copy last sent, for a reader that refused it: the next
    /// copy goes whole, under the next number.
    pub fn resync(&mut self) {
        self.sent = None;
    }
}

/// `lines` in frames, each filled until the next row would take it past
/// [`SCREEN_ROWS_BUDGET`]. A row always fits a frame by itself (see
/// [`yagura_protocol::MAX_SCREEN_LINE_BYTES`]).
fn into_frames(number: u64, lines: Vec<ScreenLine>) -> Vec<ScreenRows> {
    let mut frames = Vec::new();
    let mut current = Vec::new();
    let mut bytes = 0usize;
    for line in lines {
        let bound = line.json_bound();
        if !current.is_empty() && bytes + bound > SCREEN_ROWS_BUDGET {
            frames.push(ScreenRows {
                number,
                lines: std::mem::take(&mut current),
            });
            bytes = 0;
        }
        bytes += bound;
        current.push(line);
    }
    if !current.is_empty() {
        frames.push(ScreenRows {
            number,
            lines: current,
        });
    }
    frames
}
