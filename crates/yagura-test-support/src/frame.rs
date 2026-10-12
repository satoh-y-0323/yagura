//! The reference applier for [`Frame`]s: turns the parts a daemon sends into
//! the grid a renderer draws.
//!
//! The GUI renderer (TypeScript) implements the same rules; this is the
//! specification it is held to. The applier interprets nothing: every cell,
//! width, attribute, cursor and flag is copied from the frame. A frame that
//! breaks a rule of the wire (a row past the grid, a size change without a
//! reset, a wide index with no cell) is an [`ApplyError`], never repaired.

use yagura_protocol::{
    CellAttr, CellFlag, ColorOverride, CursorShape, CursorState, Frame, FrameLine, ViewState,
};

/// A breach of the frame wire rules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyError {
    /// A part of another frame arrived before the open frame's last part.
    InterleavedFrames { open: u64, got: u64 },
    /// A part other than the first set `reset`.
    ResetInsideFrame { seq: u64 },
    /// The first part of a frame changed the size without `reset`, or there
    /// was no grid yet.
    SizeChangedWithoutReset { seq: u64 },
    /// A later part changed the size the first part set.
    SizeChangedInsideFrame { seq: u64 },
    /// A line named a row outside the grid.
    RowOutOfRange { row: u16 },
    /// A run named an attribute index outside `attrs`.
    AttrOutOfRange { index: u16 },
    /// A run's `wide` named a cell that is not in the run, or was not ascending.
    WideIndexInvalid { row: u16 },
    /// A row's cells are wider than the grid.
    RowOverflow { row: u16 },
    /// The same row was named twice in one part.
    DuplicateRow { row: u16 },
}

/// One cell of the grid. A wide character occupies two columns: the first
/// holds the text with `width` 2, the second is a spacer with `width` 0.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GridCell {
    /// The base character and its combining characters; empty for a spacer.
    pub text: String,
    /// 1, 2, or 0 for the spacer behind a wide cell.
    pub width: u8,
    /// The cell's attribute.
    pub attr: CellAttr,
}

impl GridCell {
    fn blank() -> Self {
        Self {
            text: " ".to_string(),
            width: 1,
            attr: CellAttr::default(),
        }
    }
}

/// One row of the grid: exactly `cols` cells.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GridLine {
    /// The line goes on in the next row.
    pub wrapped: bool,
    /// One entry per column.
    pub cells: Vec<GridCell>,
}

/// The state a renderer keeps between frames.
#[derive(Debug, Clone)]
pub struct Grid {
    /// Columns.
    pub cols: u16,
    /// Rows.
    pub rows: u16,
    /// The rows, top to bottom.
    pub lines: Vec<GridLine>,
    /// The cursor of the last applied part.
    pub cursor: CursorState,
    /// The view state of the last applied part.
    pub view: ViewState,
    /// The color overrides of the last applied part.
    pub overrides: Vec<ColorOverride>,
    /// The sequence number of the last completed frame.
    pub last_seq: Option<u64>,
    open: Option<u64>,
    open_size: (u16, u16),
}

impl Default for Grid {
    fn default() -> Self {
        Self::new()
    }
}

impl Grid {
    /// An empty grid that accepts only a `reset` frame.
    #[must_use]
    pub fn new() -> Self {
        Self {
            cols: 0,
            rows: 0,
            lines: Vec::new(),
            cursor: CursorState {
                row: 0,
                col: 0,
                visible: false,
                shape: CursorShape::Block,
            },
            view: ViewState {
                display_offset: 0,
                history_rows: 0,
                mouse_reporting: false,
                alt_screen: false,
            },
            overrides: Vec::new(),
            last_seq: None,
            open: None,
            open_size: (0, 0),
        }
    }

    /// Applies one part of a frame. Returns `true` when it was the last part
    /// (the frame is complete and is to be acknowledged).
    ///
    /// # Errors
    ///
    /// Returns the [`ApplyError`] naming the rule the frame broke; the grid
    /// is then in an unspecified state.
    pub fn apply(&mut self, frame: &Frame) -> Result<bool, ApplyError> {
        match self.open {
            Some(open) if open != frame.seq => {
                return Err(ApplyError::InterleavedFrames {
                    open,
                    got: frame.seq,
                });
            }
            Some(_) => {
                if frame.reset {
                    return Err(ApplyError::ResetInsideFrame { seq: frame.seq });
                }
                if (frame.cols, frame.rows) != self.open_size {
                    return Err(ApplyError::SizeChangedInsideFrame { seq: frame.seq });
                }
            }
            None => {
                if frame.reset {
                    self.cols = frame.cols;
                    self.rows = frame.rows;
                    self.lines = (0..frame.rows)
                        .map(|_| GridLine {
                            wrapped: false,
                            cells: vec![GridCell::blank(); usize::from(frame.cols)],
                        })
                        .collect();
                } else if (frame.cols, frame.rows) != (self.cols, self.rows) || self.rows == 0 {
                    return Err(ApplyError::SizeChangedWithoutReset { seq: frame.seq });
                }
                self.open_size = (frame.cols, frame.rows);
            }
        }

        let mut seen_rows = Vec::new();
        for line in &frame.lines {
            if seen_rows.contains(&line.row) {
                return Err(ApplyError::DuplicateRow { row: line.row });
            }
            seen_rows.push(line.row);
            let built = self.build_line(frame, line)?;
            let slot = self
                .lines
                .get_mut(usize::from(line.row))
                .ok_or(ApplyError::RowOutOfRange { row: line.row })?;
            *slot = built;
        }

        self.cursor = frame.cursor;
        self.view = frame.view;
        self.overrides.clone_from(&frame.overrides);

        if frame.last {
            self.open = None;
            self.last_seq = Some(frame.seq);
            Ok(true)
        } else {
            self.open = Some(frame.seq);
            Ok(false)
        }
    }

    fn build_line(&self, frame: &Frame, line: &FrameLine) -> Result<GridLine, ApplyError> {
        if line.row >= self.rows {
            return Err(ApplyError::RowOutOfRange { row: line.row });
        }
        let mut cells: Vec<GridCell> = Vec::with_capacity(usize::from(self.cols));
        for run in &line.runs {
            let attr = frame
                .attrs
                .get(usize::from(run.attr))
                .ok_or(ApplyError::AttrOutOfRange { index: run.attr })?;
            let mut last_wide: Option<u16> = None;
            for index in &run.wide {
                if usize::from(*index) >= run.cells.len() || last_wide.is_some_and(|l| l >= *index)
                {
                    return Err(ApplyError::WideIndexInvalid { row: line.row });
                }
                last_wide = Some(*index);
            }
            for (index, text) in run.cells.iter().enumerate() {
                let wide = u16::try_from(index).is_ok_and(|i| run.wide.contains(&i));
                cells.push(GridCell {
                    text: text.clone(),
                    width: if wide { 2 } else { 1 },
                    attr: attr.clone(),
                });
                if wide {
                    cells.push(GridCell {
                        text: String::new(),
                        width: 0,
                        attr: attr.clone(),
                    });
                }
            }
        }
        if cells.len() > usize::from(self.cols) {
            return Err(ApplyError::RowOverflow { row: line.row });
        }
        cells.resize(usize::from(self.cols), GridCell::blank());
        Ok(GridLine {
            wrapped: line.wrapped,
            cells,
        })
    }

    /// The text of row `row`: the cells joined, a wide cell once, without
    /// trailing spaces.
    ///
    /// # Panics
    ///
    /// Panics if `row` is outside the grid.
    #[must_use]
    pub fn row_text(&self, row: u16) -> String {
        let line = &self.lines[usize::from(row)];
        let text: String = line
            .cells
            .iter()
            .filter(|cell| cell.width != 0)
            .map(|cell| cell.text.as_str())
            .collect();
        text.trim_end_matches(' ').to_string()
    }

    /// Every row's [`Grid::row_text`], top to bottom.
    #[must_use]
    pub fn screen_text(&self) -> Vec<String> {
        (0..self.rows).map(|row| self.row_text(row)).collect()
    }

    /// The screen as one string, a row per line, for failure messages.
    #[must_use]
    pub fn dump(&self) -> String {
        let mut out = format!(
            "grid {}x{} cursor {:?} view {:?}\n",
            self.cols, self.rows, self.cursor, self.view
        );
        for (row, text) in self.screen_text().iter().enumerate() {
            out.push_str(&format!("{row:>3}| {text}\n"));
        }
        out
    }

    /// Whether any row's text contains `needle`.
    #[must_use]
    pub fn contains_text(&self, needle: &str) -> bool {
        self.screen_text().iter().any(|row| row.contains(needle))
    }

    /// The `(row, col)` of the first cell whose text is exactly `text`.
    #[must_use]
    pub fn find_cell(&self, text: &str) -> Option<(u16, u16)> {
        self.lines.iter().enumerate().find_map(|(row, line)| {
            line.cells
                .iter()
                .position(|cell| cell.width != 0 && cell.text == text)
                .and_then(|col| Some((u16::try_from(row).ok()?, u16::try_from(col).ok()?)))
        })
    }

    /// The cell at `(row, col)`.
    ///
    /// # Panics
    ///
    /// Panics if the position is outside the grid.
    #[must_use]
    pub fn cell(&self, row: u16, col: u16) -> &GridCell {
        &self.lines[usize::from(row)].cells[usize::from(col)]
    }

    /// Every `(row, col)` whose attribute has [`CellFlag::Selected`].
    #[must_use]
    pub fn selected_cells(&self) -> Vec<(u16, u16)> {
        let mut out = Vec::new();
        for (row, line) in self.lines.iter().enumerate() {
            for (col, cell) in line.cells.iter().enumerate() {
                if cell.attr.flags.contains(&CellFlag::Selected)
                    && let (Ok(row), Ok(col)) = (u16::try_from(row), u16::try_from(col))
                {
                    out.push((row, col));
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    //! The applier is held to the same sample file the renderer's tests read
    //! (`yagura-protocol/tests/fixtures/frame-sample.json`).

    use std::path::Path;

    use serde_json::Value;
    use yagura_protocol::{
        CellAttr, CellColor, CursorShape, CursorState, DaemonMessage, Frame, FrameLine, FrameRun,
        ViewState, decode_daemon_message,
    };

    use super::{ApplyError, Grid};

    fn color(value: &Value) -> CellColor {
        serde_json::from_value(value.clone()).expect("a color of the sample")
    }

    fn part(seq: u64, (cols, rows): (u16, u16), reset: bool, last: bool) -> Frame {
        Frame {
            seq,
            cols,
            rows,
            reset,
            last,
            attrs: vec![CellAttr::default()],
            lines: Vec::new(),
            cursor: CursorState {
                row: 0,
                col: 0,
                visible: true,
                shape: CursorShape::Block,
            },
            view: ViewState {
                display_offset: 0,
                history_rows: 0,
                mouse_reporting: false,
                alt_screen: false,
            },
            overrides: Vec::new(),
        }
    }

    fn line(row: u16, cells: &[&str], wide: &[u16]) -> FrameLine {
        FrameLine {
            row,
            wrapped: false,
            runs: vec![FrameRun {
                attr: 0,
                cells: cells.iter().map(|text| (*text).to_string()).collect(),
                wide: wide.to_vec(),
            }],
        }
    }

    /// A grid that has applied one complete 4x2 frame.
    fn sized_grid() -> Grid {
        let mut grid = Grid::new();
        assert_eq!(grid.apply(&part(1, (4, 2), true, true)), Ok(true));
        grid
    }

    fn rejects(grid: &mut Grid, frame: &Frame) -> ApplyError {
        grid.apply(frame)
            .expect_err("the frame breaks a wire rule and must be refused")
    }

    #[test]
    fn a_first_frame_without_reset_is_refused() {
        let mut grid = Grid::new();
        assert_eq!(
            rejects(&mut grid, &part(1, (4, 2), false, true)),
            ApplyError::SizeChangedWithoutReset { seq: 1 }
        );
    }

    #[test]
    fn a_new_size_without_reset_is_refused() {
        let mut grid = sized_grid();
        assert_eq!(
            rejects(&mut grid, &part(2, (5, 2), false, true)),
            ApplyError::SizeChangedWithoutReset { seq: 2 }
        );
    }

    #[test]
    fn a_part_of_another_frame_before_the_open_one_ends_is_refused() {
        let mut grid = sized_grid();
        assert_eq!(grid.apply(&part(2, (4, 2), false, false)), Ok(false));
        assert_eq!(
            rejects(&mut grid, &part(3, (4, 2), false, true)),
            ApplyError::InterleavedFrames { open: 2, got: 3 }
        );
    }

    #[test]
    fn a_later_part_may_not_reset_or_resize() {
        let mut grid = sized_grid();
        assert_eq!(grid.apply(&part(2, (4, 2), false, false)), Ok(false));
        assert_eq!(
            rejects(&mut grid, &part(2, (4, 2), true, true)),
            ApplyError::ResetInsideFrame { seq: 2 }
        );

        let mut grid = sized_grid();
        assert_eq!(grid.apply(&part(2, (4, 2), false, false)), Ok(false));
        assert_eq!(
            rejects(&mut grid, &part(2, (4, 3), false, true)),
            ApplyError::SizeChangedInsideFrame { seq: 2 }
        );
    }

    #[test]
    fn the_last_part_of_a_frame_closes_it() {
        let mut grid = sized_grid();
        assert_eq!(grid.apply(&part(2, (4, 2), false, false)), Ok(false));
        assert_eq!(grid.apply(&part(2, (4, 2), false, true)), Ok(true));
        assert_eq!(grid.apply(&part(3, (4, 2), false, true)), Ok(true));
    }

    #[test]
    fn a_row_past_the_grid_or_named_twice_is_refused() {
        let mut frame = part(2, (4, 2), false, true);
        frame.lines = vec![line(2, &["a"], &[])];
        assert_eq!(
            rejects(&mut sized_grid(), &frame),
            ApplyError::RowOutOfRange { row: 2 }
        );

        frame.lines = vec![line(1, &["a"], &[]), line(1, &["b"], &[])];
        assert_eq!(
            rejects(&mut sized_grid(), &frame),
            ApplyError::DuplicateRow { row: 1 }
        );
    }

    #[test]
    fn an_attribute_index_past_attrs_is_refused() {
        let mut frame = part(2, (4, 2), false, true);
        let mut bad = line(0, &["a"], &[]);
        bad.runs[0].attr = 1;
        frame.lines = vec![bad];
        assert_eq!(
            rejects(&mut sized_grid(), &frame),
            ApplyError::AttrOutOfRange { index: 1 }
        );
    }

    #[test]
    fn a_wide_index_with_no_cell_or_out_of_order_is_refused() {
        for wide in [&[2][..], &[1, 0][..], &[1, 1][..]] {
            let mut frame = part(2, (4, 2), false, true);
            frame.lines = vec![line(0, &["a", "b"], wide)];
            assert_eq!(
                rejects(&mut sized_grid(), &frame),
                ApplyError::WideIndexInvalid { row: 0 },
                "wide {wide:?}"
            );
        }
    }

    #[test]
    fn cells_wider_than_the_grid_are_refused() {
        let mut frame = part(2, (4, 2), false, true);
        frame.lines = vec![line(0, &["a", "b", "c", "d", "e"], &[])];
        assert_eq!(
            rejects(&mut sized_grid(), &frame),
            ApplyError::RowOverflow { row: 0 }
        );

        // A wide cell takes two columns, so four cells with one wide is five.
        frame.lines = vec![line(0, &["a", "b", "c", "d"], &[0])];
        assert_eq!(
            rejects(&mut sized_grid(), &frame),
            ApplyError::RowOverflow { row: 0 }
        );
    }

    #[test]
    fn a_wide_cell_is_followed_by_its_spacer() {
        let mut grid = sized_grid();
        let mut frame = part(2, (4, 2), false, true);
        frame.lines = vec![line(1, &["\u{4e00}", "x"], &[0])];
        assert_eq!(grid.apply(&frame), Ok(true));

        assert_eq!(grid.cell(1, 0).width, 2);
        assert_eq!(grid.cell(1, 1).width, 0);
        assert_eq!(grid.cell(1, 2).text, "x");
        assert_eq!(grid.row_text(1), "\u{4e00}x");
    }

    #[test]
    fn the_applier_leaves_the_grid_the_sample_says() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../yagura-protocol/tests/fixtures/frame-sample.json");
        let sample: Value = serde_json::from_str(
            &std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("{path:?}: {err}")),
        )
        .unwrap();

        let mut grid = Grid::new();
        for step in sample["steps"].as_array().unwrap() {
            let name = step["name"].as_str().unwrap();
            for message in step["messages"].as_array().unwrap() {
                let body = serde_json::to_vec(message).unwrap();
                let DaemonMessage::Frame(frame) = decode_daemon_message(&body).unwrap() else {
                    panic!("{name}: not a frame");
                };
                grid.apply(&frame)
                    .unwrap_or_else(|error| panic!("{name}: {error:?}"));
            }

            let expected = &step["expected"];
            assert_eq!(
                u64::from(grid.cols),
                expected["cols"].as_u64().unwrap(),
                "{name}"
            );
            assert_eq!(
                u64::from(grid.rows),
                expected["rows"].as_u64().unwrap(),
                "{name}"
            );
            let screen_text: Vec<String> =
                serde_json::from_value(expected["screen_text"].clone()).unwrap();
            assert_eq!(grid.screen_text(), screen_text, "{name}");
            let wrapped: Vec<usize> =
                serde_json::from_value(expected["wrapped_rows"].clone()).unwrap();
            let got_wrapped: Vec<usize> = grid
                .lines
                .iter()
                .enumerate()
                .filter(|(_, line)| line.wrapped)
                .map(|(row, _)| row)
                .collect();
            assert_eq!(got_wrapped, wrapped, "{name}");
            assert_eq!(
                serde_json::to_value(grid.cursor).unwrap(),
                expected["cursor"],
                "{name}"
            );
            assert_eq!(
                serde_json::to_value(grid.view).unwrap(),
                expected["view"],
                "{name}"
            );
            assert_eq!(
                serde_json::to_value(&grid.overrides).unwrap(),
                expected["overrides"],
                "{name}"
            );
            for cell in expected["cells"].as_array().unwrap() {
                let row = u16::try_from(cell["row"].as_u64().unwrap()).unwrap();
                let col = u16::try_from(cell["col"].as_u64().unwrap()).unwrap();
                let got = grid.cell(row, col);
                let at = format!("{name}: cell ({row},{col})");
                assert_eq!(got.text, cell["text"].as_str().unwrap(), "{at}");
                assert_eq!(
                    u64::from(got.width),
                    cell["width"].as_u64().unwrap(),
                    "{at}"
                );
                assert_eq!(got.attr.fg, color(&cell["fg"]), "{at}");
                assert_eq!(got.attr.bg, color(&cell["bg"]), "{at}");
                assert_eq!(
                    serde_json::to_value(&got.attr.flags).unwrap(),
                    cell["flags"],
                    "{at}"
                );
            }
        }
    }
}
