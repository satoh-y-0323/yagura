//! The copy of a window's screen that the daemon reads: what the terminal
//! model shows, carried as plain data so that its reader needs no terminal
//! engine.
//!
//! A copy holds every value the daemon's reading of a screen looks at, and
//! nothing a reading does not: the visible rows of the live screen (not the
//! scrolled view a client is shown), the cursor, the modes that change what
//! input becomes, and the counts that tell a reader what happened to the
//! screen between two copies.
//!
//! A copy travels as [`ScreenRows`] followed by one [`ScreenPush`], all under
//! one copy number. The first copy is whole ([`ScreenPush::full`]); after it,
//! only the rows that differ from the copy before are sent, laid over that
//! copy cut to the new size ([`ScreenCopy::lines_resized`]).

use serde::{Deserialize, Serialize};

use crate::InputModes;

/// Combining characters a cell carries; a terminal model keeps no more, and a
/// copy with more is refused.
pub const MAX_ZEROWIDTH_PER_CELL: usize = 16;

/// The most JSON bytes one [`ScreenRows`] body carries its rows in: rows are
/// put into a frame until the next one would go past this.
pub const SCREEN_ROWS_BUDGET: usize = 1024 * 1024;

/// The most JSON bytes one character takes in a cell: a control character is
/// escaped as six bytes, between two quotes, with a separating comma; twelve
/// leaves room to spare.
const CHAR_BOUND: usize = 12;

/// The most JSON bytes a cell takes apart from its characters: the keys, the
/// largest column, both flags set and the braces and comma around it.
const CELL_FIXED_BOUND: usize = 64;

/// The most JSON bytes a row takes apart from its cells.
const LINE_FIXED_BOUND: usize = 48;

/// The most JSON bytes a [`ScreenRows`] takes apart from its rows.
const ROWS_FIXED_BOUND: usize = 64;

/// The most JSON bytes one row of the widest screen takes, every cell
/// carrying the most combining characters.
pub const MAX_SCREEN_LINE_BYTES: usize = LINE_FIXED_BOUND
    + crate::MAX_WINDOW_DIMENSION as usize
        * (CELL_FIXED_BOUND + CHAR_BOUND * (1 + MAX_ZEROWIDTH_PER_CELL));

// Any one row fits in a frame by itself, and a frame of rows fits under the
// holder wire's frame limit, so the size of a copy never fails the stream.
const _: () = assert!(MAX_SCREEN_LINE_BYTES <= SCREEN_ROWS_BUDGET);
const _: () =
    assert!(ROWS_FIXED_BOUND + SCREEN_ROWS_BUDGET <= crate::holder::MAX_HOLDER_FRAME_LEN - 2);

/// One cell a program wrote something to. A cell nobody wrote, a tab's
/// padding and the second half of a wide character are not carried; a space
/// a program wrote is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScreenCell {
    /// The cell's column, counted from 0.
    pub col: u16,
    /// The character as written (a tab stays a tab).
    pub c: char,
    /// The combining characters on the cell, at most
    /// [`MAX_ZEROWIDTH_PER_CELL`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub zerowidth: Vec<char>,
    /// The cell is drawn dimmed.
    #[serde(default, skip_serializing_if = "is_false")]
    pub dim: bool,
    /// The cell holds a wide character, which takes this column and the next.
    #[serde(default, skip_serializing_if = "is_false")]
    pub wide: bool,
}

impl ScreenCell {
    /// The most JSON bytes this cell takes, its separating comma included.
    #[must_use]
    pub fn json_bound(&self) -> usize {
        CELL_FIXED_BOUND + CHAR_BOUND * (1 + self.zerowidth.len())
    }
}

/// One row of the screen: the cells written on it, left to right.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScreenLine {
    /// The row, counted from 0 at the top of the live screen.
    pub line: u16,
    /// The written cells, in increasing column order.
    pub cells: Vec<ScreenCell>,
}

impl ScreenLine {
    /// The most JSON bytes this row takes, its separating comma included.
    #[must_use]
    pub fn json_bound(&self) -> usize {
        LINE_FIXED_BOUND + self.cells.iter().map(ScreenCell::json_bound).sum::<usize>()
    }
}

/// Where the cursor stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ScreenCursor {
    /// The cursor's row.
    pub line: u16,
    /// The cursor's column.
    pub col: u16,
    /// The cursor stands on the last column with a character written there,
    /// so the next character goes to the start of the next row.
    pub input_needs_wrap: bool,
}

/// A window's screen as a terminal model shows it at one moment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScreenCopy {
    /// Columns.
    pub cols: u16,
    /// Rows.
    pub rows: u16,
    /// Every row of the live screen, top to bottom: `lines[i].line == i`.
    pub lines: Vec<ScreenLine>,
    /// The cursor.
    pub cursor: ScreenCursor,
    /// The cursor is hidden (`CSI ? 25 l`).
    pub cursor_hidden: bool,
    /// The program has a synchronized update open (`CSI ? 2026 h`).
    pub sync_open: bool,
    /// The modes the child set that change what input becomes;
    /// [`InputModes::key_records_flattened`] is never set here.
    #[serde(with = "modes_as_bits")]
    pub input_modes: InputModes,
    /// How many times the model was rebuilt blank, losing what it showed.
    pub generation: u64,
    /// How many chunks of the child's output the model has taken in.
    pub outputs_fed: u64,
}

impl ScreenCopy {
    /// This copy's rows cut to `cols` x `rows`: rows below the new bottom and
    /// cells right of the new edge are gone, and rows below the old bottom
    /// are empty. A difference is laid over these.
    #[must_use]
    pub fn lines_resized(&self, cols: u16, rows: u16) -> Vec<ScreenLine> {
        (0..rows)
            .map(|line| ScreenLine {
                line,
                cells: self
                    .lines
                    .get(usize::from(line))
                    .map(|kept| {
                        kept.cells
                            .iter()
                            .filter(|cell| cell.col < cols)
                            .cloned()
                            .collect()
                    })
                    .unwrap_or_default(),
            })
            .collect()
    }
}

/// Rows of one copy (h->d). A copy's rows may take several of these, each
/// within [`SCREEN_ROWS_BUDGET`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScreenRows {
    /// The copy these rows belong to.
    pub number: u64,
    /// Rows that replace the ones of the same number.
    pub lines: Vec<ScreenLine>,
}

/// The close of one copy (h->d): everything but the rows. The copy is applied
/// once this arrives, so a reader only ever sees whole copies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScreenPush {
    /// The copy's number: one more than the copy before, unless the copy is
    /// whole.
    pub number: u64,
    /// The copy is whole: its rows are laid over an empty screen rather than
    /// over the copy before.
    pub full: bool,
    /// See [`ScreenCopy::cols`].
    pub cols: u16,
    /// See [`ScreenCopy::rows`].
    pub rows: u16,
    /// See [`ScreenCopy::cursor`].
    pub cursor: ScreenCursor,
    /// See [`ScreenCopy::cursor_hidden`].
    pub cursor_hidden: bool,
    /// See [`ScreenCopy::sync_open`].
    pub sync_open: bool,
    /// See [`ScreenCopy::input_modes`].
    #[serde(with = "modes_as_bits")]
    pub input_modes: InputModes,
    /// See [`ScreenCopy::generation`].
    pub generation: u64,
    /// See [`ScreenCopy::outputs_fed`].
    pub outputs_fed: u64,
}

// `serde` hands a field to `skip_serializing_if` by reference.
fn is_false(value: &bool) -> bool {
    !*value
}

/// The modes travel as [`InputModes::to_bits`], which leaves out what the
/// window decides rather than its child.
mod modes_as_bits {
    use serde::{Deserialize, Deserializer, Serializer};

    use crate::InputModes;

    pub(super) fn serialize<S: Serializer>(
        modes: &InputModes,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_u16(modes.to_bits())
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<InputModes, D::Error> {
        u16::deserialize(deserializer).map(InputModes::from_bits)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    /// The cell that takes the most JSON: the largest column, a character and
    /// every combining character escaped, both flags set.
    fn widest_cell() -> ScreenCell {
        ScreenCell {
            col: u16::MAX,
            c: '\u{1}',
            zerowidth: vec!['\u{1}'; MAX_ZEROWIDTH_PER_CELL],
            dim: true,
            wide: true,
        }
    }

    #[test]
    fn the_widest_row_serializes_within_its_bound() {
        let cell = widest_cell();
        let cell_json = serde_json::to_vec(&cell).unwrap();
        assert!(
            cell_json.len() < cell.json_bound(),
            "a cell took {} bytes, past its bound of {}",
            cell_json.len(),
            cell.json_bound()
        );

        let line = ScreenLine {
            line: u16::MAX,
            cells: vec![cell; usize::from(crate::MAX_WINDOW_DIMENSION)],
        };
        let rows = ScreenRows {
            number: u64::MAX,
            lines: vec![line],
        };
        let rows_json = serde_json::to_vec(&rows).unwrap();
        assert!(
            rows_json.len() <= ROWS_FIXED_BOUND + MAX_SCREEN_LINE_BYTES,
            "the widest row took {} bytes, past {}",
            rows_json.len(),
            ROWS_FIXED_BOUND + MAX_SCREEN_LINE_BYTES
        );
    }
}
