//! Virtual terminal screen model used by the detector to match patterns
//! against "what a human sees on screen". Pattern definitions are written
//! against display text rather than raw byte streams or ANSI-stripped text,
//! because full-screen TUI agents rewrite the screen and move the cursor to
//! redraw spinners; only a real VT parser reproduces that faithfully.
//!
//! The terminal itself is [`yagura_term::VtScreen`]; this file reads it with
//! the detection definitions' policy. `VtScreen` here holds that terminal and
//! adds the reads, for as long as the daemon reads the terminal itself rather
//! than a copy of its screen.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::Line;
use yagura_os::program::HostOs;
use yagura_term::PtyHost;
use yagura_term::screen::{cells_text, cells_text_mapped};

pub use yagura_term::ScreenIntegrity;

/// The result of reading a composer from the screen: body recovered, only the
/// body's end in view, empty (marker present but no text), or not observed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComposerReadout {
    /// The composer contains a non-empty body of text.
    Body(String),
    /// The marker row has scrolled off the top, but every row from the top
    /// down to the cursor is a continuation row: only the body's end is in
    /// view, and this is that non-empty end. A frame whose composer rows were
    /// just erased never takes this shape, since erased rows do not fill the
    /// screen up to the top with continuation rows.
    Tail(String),
    /// The composer is present but empty.
    Empty,
    /// The composer could not be observed (marker not found or not defined).
    NotObserved,
}

/// Where the upward walk from the cursor ended (see `VtScreen::walk_to_marker`).
enum ComposerWalk {
    /// On the marker row.
    Marker(u16),
    /// Off the top, every row on the way a continuation row.
    ReachedTop,
    /// On a row that is neither a continuation row nor the marker.
    Absent,
}

/// One window's screen, shared by everything that reads it: the PTY reader
/// feeds it, and detection and `read_output` read it. The feeder releases the
/// lock fairly and it is never poisoned, so a reader neither starves while the
/// child floods nor loses the screen to a panic elsewhere. Waiting for it
/// blocks, so it is taken only on threads that may block: `PtySession` and
/// `DetectorHandle` take it on blocking threads for async callers.
pub type SharedScreen = Arc<parking_lot::Mutex<VtScreen>>;

/// A window's terminal ([`yagura_term::VtScreen`], reached through `Deref`)
/// with the detector's reads of it.
///
/// This is the only public surface the detector's pattern matcher touches.
pub struct VtScreen {
    term: yagura_term::VtScreen,
}

impl std::fmt::Debug for VtScreen {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.term.fmt(f)
    }
}

impl std::ops::Deref for VtScreen {
    type Target = yagura_term::VtScreen;

    fn deref(&self) -> &Self::Target {
        &self.term
    }
}

impl std::ops::DerefMut for VtScreen {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.term
    }
}

impl VtScreen {
    /// Creates a new blank screen of the given size, behind this OS's PTY
    /// host; see [`yagura_term::VtScreen::new`].
    ///
    /// `cols`/`rows` follow the same `(cols, rows)` order as
    /// `PtySession::size()`. Never panics.
    #[must_use]
    pub fn new(cols: u16, rows: u16) -> Self {
        Self::with_history(cols, rows, 0)
    }

    /// Creates a blank screen that keeps up to `history_rows` rows scrolled
    /// off its top; see [`yagura_term::VtScreen::with_history`]. Never panics.
    #[must_use]
    pub fn with_history(cols: u16, rows: u16, history_rows: usize) -> Self {
        let host = PtyHost {
            requests_win32_input: HostOs::host().pty_host_requests_win32_input(),
        };
        Self {
            term: yagura_term::VtScreen::with_history(cols, rows, history_rows, host),
        }
    }

    /// Wraps this screen for sharing between its writer and its readers.
    #[must_use]
    pub fn into_shared(self) -> SharedScreen {
        Arc::new(parking_lot::Mutex::new(self))
    }

    /// Runs `read` against this screen and returns what it returns.
    ///
    /// Never panics: a panic from inside the engine rebuilds the screen
    /// blank at its current size, advances
    /// [`generation`](yagura_term::VtScreen::generation) and returns `None`;
    /// the next feed or resize reports [`ScreenIntegrity::Rebuilt`].
    pub fn read_guarded<R>(&mut self, read: impl FnOnce(&Self) -> R) -> Option<R> {
        // `AssertUnwindSafe`: `read` borrows the screen shared, so a panic
        // cannot leave a half-updated value behind.
        match catch_unwind(AssertUnwindSafe(|| read(&*self))) {
            Ok(value) => Some(value),
            Err(_) => {
                self.term.rebuild_after_read_panic();
                None
            }
        }
    }

    /// Returns the bottom `n` display lines as plain text, top-to-bottom
    /// (the last element is the bottom-most line).
    ///
    /// The window ends at the last row holding a visible (non-whitespace)
    /// character, not merely a non-empty row: a full-screen TUI that clears
    /// and repaints often paints unused rows with whitespace instead of
    /// leaving them untouched, and such a row must not count as the
    /// window's tail. A line-oriented CLI that has printed two lines into a
    /// 24-row terminal shows them at the top, and "the bottom of the
    /// screen" a human reads is those lines -- not the blank padding below
    /// them; anchoring on the viewport instead would make every `region =
    /// "bottom"` rule unmatchable until the screen first scrolls. A
    /// full-screen TUI that fills the viewport with visible content puts
    /// the window at the viewport's bottom edge, so both readings coincide;
    /// one that pads unused rows with whitespace instead puts the window at
    /// the last row holding visible content, above that padding. The window
    /// never starts above row `n - 1`, so the blank padding of a
    /// nearly-empty screen still forms part of the result.
    ///
    /// Cells that were never written do not appear at the end of a line;
    /// whitespace the CLI itself wrote is returned as-is. Never returns
    /// more entries than the current row count, even if `n` exceeds it.
    #[must_use]
    pub fn bottom_lines(&self, n: usize) -> Vec<String> {
        self.window_of(self.screen_rows(), n)
    }

    /// [`VtScreen::bottom_lines`] over the screen with the composer's body
    /// taken out first, so a rule sees the CLI's drawing and not the text
    /// being typed.
    ///
    /// The marker row is cut after the marker and its separator (after the
    /// marker alone when there is no separator), and the continuation rows
    /// down to the cursor row are dropped, closing the gap. The window is
    /// taken after that, so a long body cannot push the CLI's own lines out
    /// of it. Nothing is taken out when the cursor is hidden or the composer
    /// cannot be located: the screen then cannot be told apart from a dialog
    /// drawn over the composer's shape.
    ///
    /// The composer is located from the same column as in
    /// [`VtScreen::read_composer`], and a left panel's part of the marker row
    /// is kept for the rules.
    #[must_use]
    pub fn bottom_lines_without_composer(
        &self,
        n: usize,
        section: &crate::detector::definition::ComposerSection,
    ) -> Vec<String> {
        self.window_of(self.lines_without_composer(section), n)
    }

    /// Every row of the screen, top-to-bottom, with the composer's body taken
    /// out the way [`VtScreen::bottom_lines_without_composer`] takes it out,
    /// before any window is cut.
    #[must_use]
    pub(crate) fn lines_without_composer(
        &self,
        section: &crate::detector::definition::ComposerSection,
    ) -> Vec<String> {
        let mut lines = self.screen_rows();
        let origin = self.composer_origin(section);
        if !self.cursor_hidden()
            && let ComposerWalk::Marker(marker_row) = self.walk_to_marker(section, origin)
        {
            let cursor_row = self.cursor_position().0;
            let marker_start = if origin == 0 {
                Some(0)
            } else {
                self.row_text_columns(marker_row)
                    .and_then(|(_, columns)| byte_at_column(&columns, origin))
            };
            let (marker_row, cursor_row) = (usize::from(marker_row), usize::from(cursor_row));
            if let Some(line) = lines.get_mut(marker_row)
                && let Some(start) = marker_start
                && let Some(marker_match) =
                    line.get(start..).and_then(|from| section.marker.find(from))
            {
                let after_marker = start + marker_match.end();
                let keep = match line[after_marker..].find(&section.separator) {
                    Some(rel) => after_marker + rel + section.separator.len(),
                    None => after_marker,
                };
                line.truncate(keep);
            }
            if cursor_row > marker_row && cursor_row < lines.len() {
                lines.drain(marker_row + 1..=cursor_row);
            }
        }
        lines
    }

    /// Every row of the screen, top-to-bottom, as [`VtScreen::bottom_lines`]
    /// reads them before cutting its window.
    #[must_use]
    pub(crate) fn all_lines(&self) -> Vec<String> {
        self.screen_rows()
    }

    /// The window [`VtScreen::bottom_lines`] cuts from `lines`, copied out so
    /// the caller keeps `lines` whole.
    #[must_use]
    pub(crate) fn bottom_window(&self, lines: &[String], n: usize) -> Vec<String> {
        lines[self.window_range(lines, n)].to_vec()
    }

    /// The same window as [`VtScreen::bottom_window`], cut from `lines` in place.
    #[must_use]
    pub(crate) fn window_of(&self, mut lines: Vec<String>, n: usize) -> Vec<String> {
        let range = self.window_range(&lines, n);
        lines.truncate(range.end);
        lines.drain(..range.start);
        lines
    }

    fn window_range(&self, lines: &[String], n: usize) -> std::ops::Range<usize> {
        let (_cols, rows) = self.size();
        let take = n.min(usize::from(rows));
        let last_written = lines
            .iter()
            .rposition(|line| line.chars().any(|c| !c.is_whitespace()))
            .unwrap_or(0);
        let skip = (last_written + 1).saturating_sub(take);
        skip.min(lines.len())..(skip + take).min(lines.len())
    }

    /// Returns the row index (0-based) where the cursor is currently positioned.
    #[must_use]
    pub fn cursor_row(&self) -> u16 {
        self.cursor_position().0
    }

    /// The cursor as `(row, col)`. A cursor parked after the last column,
    /// waiting to wrap, is at column `cols`: that is where the next
    /// character would go, and where a reader of the row sees it.
    fn cursor_position(&self) -> (u16, u16) {
        let cursor = &self.term.term().grid().cursor;
        let row = u16::try_from(cursor.point.line.0).unwrap_or(0);
        let col = u16::try_from(cursor.point.column.0 + usize::from(cursor.input_needs_wrap))
            .unwrap_or(u16::MAX);
        (row, col)
    }

    /// Every screen row as text, covering all contents (dimmed ones too).
    fn screen_rows(&self) -> Vec<String> {
        let grid = self.term.term().grid();
        let cols = grid.columns();
        (0..grid.screen_lines())
            .filter_map(|row| i32::try_from(row).ok())
            .map(|row| cells_text(&grid[Line(row)], 0, cols, false).0)
            .collect()
    }

    /// Reads the composer region from the screen according to the given section.
    ///
    /// The composer region is identified by:
    /// 1. Finding the cursor row
    /// 2. Scanning upward from the cursor row to find the first (closest) row
    ///    that matches the section's marker pattern, following continuation lines
    ///    (lines starting with the section's continuation prefix)
    /// 3. Extracting the region from the marker line up to the cursor
    ///
    /// The cursor bounds the body in both directions: rows below it are
    /// outside the region, and on its own row the body ends at its column.
    /// Columns the cursor has moved past count as blanks there, written or
    /// not, so a marker's trailing space may be drawn by a cursor move alone.
    /// What stands to the right of it is the CLI's own drawing, told apart
    /// from typed text by dimming rather than by any text it contains (see
    /// [`VtScreen::visible_text`]).
    ///
    /// Returns:
    /// - `NotObserved` if the marker is not found and the walk stopped below
    ///   the top, if the section has no continuation prefix to walk by, or if
    ///   anything not dimmed stands to the right of the cursor -- which is
    ///   also how a cursor parked in the middle of a body avoids reporting a
    ///   shortened one
    /// - `Empty` if the marker is found and nothing but dimmed drawing is in
    ///   the composer, or nothing is typed and the rest of the marker row is
    ///   the section's `placeholder`
    /// - `Body(text)` if body text is successfully extracted and normalized
    /// - `Tail(text)` if continuation rows reach the top row without the
    ///   marker, and joining them (normalized as below) leaves text; with no
    ///   text left it is `NotObserved`, since an empty tail ends every body
    ///
    /// The normalization process:
    /// - Remove marker and separator from the first line
    /// - Remove continuation prefix from continuation lines (if not empty string)
    /// - Trim trailing whitespace from all lines
    /// - Join all lines into a single string
    ///
    /// Every row is read from the composer's origin: the right edge of the
    /// section's `left_panel` when it matches every row holding a visible
    /// character and all its matches end at one column, else column 0. A
    /// cursor left of the origin is outside the composer: `NotObserved`.
    ///
    /// Every row of the region, the cursor's included, ends where the
    /// section's `right_border` starts, so the edge is neither body text nor
    /// something standing right of the cursor.
    pub fn read_composer(
        &self,
        section: &crate::detector::definition::ComposerSection,
    ) -> ComposerReadout {
        let (cols, _rows) = self.size();
        let origin = self.composer_origin(section);
        let (cursor, cursor_col) = self.cursor_position();
        let cursor_col = cursor_col.min(cols);
        let cursor_end = self.composer_row_end(section, cursor, origin);
        let cursor_line = self.text_left_of_cursor(cursor, origin, cursor_col.min(cursor_end));

        let marker_row = match self.walk_to_marker(section, origin) {
            ComposerWalk::Marker(row) => Some(row),
            ComposerWalk::ReachedTop => None,
            ComposerWalk::Absent => return ComposerReadout::NotObserved,
        };

        // Text the CLI left to the right of the insertion point is either its
        // own drawing (dimmed, and already dropped) or something this section
        // cannot name: refuse to judge rather than fold it into the body or
        // call the composer empty.
        let right_of_cursor = self.visible_text(cursor, cursor_col, cursor_end);
        if !right_of_cursor.trim().is_empty() {
            let untyped_hint = marker_row == Some(cursor)
                && is_untyped_hint(section, &cursor_line, &right_of_cursor);
            return if untyped_hint {
                ComposerReadout::Empty
            } else {
                ComposerReadout::NotObserved
            };
        }

        // Lines from marker to cursor, each cut at the cursor's column on the
        // cursor's own row.
        let region_line = |row: u16| {
            if row == cursor {
                cursor_line.clone()
            } else {
                self.visible_text(row, origin, self.composer_row_end(section, row, origin))
            }
        };
        let continuation_text = |row: u16| {
            let line = region_line(row);
            let continued = line
                .strip_prefix(&section.continuation_prefix)
                .unwrap_or(&line);
            continued.trim_end().to_string()
        };

        let Some(marker_row) = marker_row else {
            let tail: String = (0..=cursor).map(continuation_text).collect();
            return if tail.is_empty() {
                ComposerReadout::NotObserved
            } else {
                ComposerReadout::Tail(tail)
            };
        };

        // Normalize:
        // 1. First line: drop the marker match, then the separator's first
        //    occurrence *after* it, not the separator's first
        //    occurrence in the whole line -- a marker class like `[$#>]` can
        //    coincidentally match earlier text (e.g. a path segment).
        let first_line = region_line(marker_row);
        let Some(marker_match) = section.marker.find(&first_line) else {
            // The upward walk already matched this exact line, so this is
            // unreachable in practice; treat it as "not observed" rather
            // than panicking on an assumption about regex behavior.
            return ComposerReadout::NotObserved;
        };

        let after_marker = marker_match.end();
        let body_start = match first_line[after_marker..].find(&section.separator) {
            Some(rel_pos) => after_marker + rel_pos + section.separator.len(),
            // Without a separator the body has no start to bound it. Anything
            // visible after the marker is text this section cannot place, so
            // refuse to call the composer empty.
            None if first_line[after_marker..].trim().is_empty() => {
                return ComposerReadout::Empty;
            }
            None => return ComposerReadout::NotObserved,
        };

        let mut normalized = String::new();

        // Add the text after separator from the first line (trimming trailing whitespace)
        let first_text = first_line[body_start..].trim_end();
        if !first_text.is_empty() {
            normalized.push_str(first_text);
        }

        // 2. Continuation lines: remove prefix, trim trailing whitespace, concatenate
        for row in marker_row.saturating_add(1)..=cursor {
            normalized.push_str(&continuation_text(row));
        }

        if normalized.is_empty() {
            ComposerReadout::Empty
        } else {
            ComposerReadout::Body(normalized)
        }
    }

    /// Locates the composer by walking up from the cursor row: a row starting
    /// with the continuation prefix is skipped, and the first row that is not
    /// one must match the marker, else the structure is not there. With
    /// `blank_lines_in_body`, a blank row above a shown cursor is skipped too:
    /// a hidden cursor is how a dialog is told from a composer, and the cursor
    /// row itself never is, since typing puts the cursor past the prefix.
    ///
    /// Returns `(marker_row, cursor_row)`, or `None` when the walk reaches a
    /// non-continuation row that is not the marker, or runs off the top of the
    /// screen (the marker row has scrolled out of view). A marker row must
    /// therefore not itself start with the continuation prefix.
    ///
    /// Rows are read from the same origin as in [`VtScreen::read_composer`],
    /// and a cursor left of it locates nothing.
    #[must_use]
    pub fn locate_composer(
        &self,
        section: &crate::detector::definition::ComposerSection,
    ) -> Option<(u16, u16)> {
        match self.walk_to_marker(section, self.composer_origin(section)) {
            ComposerWalk::Marker(row) => Some((row, self.cursor_position().0)),
            ComposerWalk::ReachedTop | ComposerWalk::Absent => None,
        }
    }

    /// The column the composer is read from (see [`VtScreen::read_composer`]).
    /// The panel is matched against each row from column 0, dimmed cells
    /// included, and its end is counted in cells: a wide character takes two
    /// columns, and a match ending inside a cell (before its combining marks)
    /// has no column, so the origin falls back to 0.
    fn composer_origin(&self, section: &crate::detector::definition::ComposerSection) -> u16 {
        let Some(panel) = &section.left_panel else {
            return 0;
        };
        let (_cols, rows) = self.size();
        let mut origin = None;
        for row in 0..rows {
            let Some((text, columns)) = self.row_text_columns(row) else {
                return 0;
            };
            if text.chars().all(char::is_whitespace) {
                continue;
            }
            let Some(edge) = panel
                .find(&text)
                .filter(|found| found.start() == 0)
                .and_then(|found| column_at_byte(&columns, found.end()))
            else {
                return 0;
            };
            match origin {
                None => origin = Some(edge),
                Some(seen) if seen == edge => {}
                Some(_) => return 0,
            }
        }
        origin.unwrap_or(0)
    }

    /// The upward walk behind [`VtScreen::locate_composer`], telling a walk
    /// that ran off the top through continuation rows from one that stopped
    /// on a row that is neither a continuation nor the marker. Rows are read
    /// from column `origin`.
    fn walk_to_marker(
        &self,
        section: &crate::detector::definition::ComposerSection,
        origin: u16,
    ) -> ComposerWalk {
        let (cols, _rows) = self.size();
        let (cursor, cursor_col) = self.cursor_position();
        let cursor_col = cursor_col.min(cols);
        if cursor_col < origin {
            return ComposerWalk::Absent;
        }
        let cursor_line = self.text_left_of_cursor(cursor, origin, cursor_col);
        let crosses_blank_rows = section.blank_lines_in_body && !self.cursor_hidden();
        let mut row = cursor;
        loop {
            let line = if row == cursor {
                cursor_line.clone()
            } else {
                match self.row_text(row, origin, cols, false) {
                    Some((text, _)) => text,
                    None => return ComposerWalk::Absent,
                }
            };
            let is_continuation = !section.continuation_prefix.is_empty()
                && (line.starts_with(&section.continuation_prefix)
                    || (crosses_blank_rows && row != cursor && line.trim().is_empty()));
            if !is_continuation {
                return if section.marker.is_match(&line) {
                    ComposerWalk::Marker(row)
                } else {
                    ComposerWalk::Absent
                };
            }
            match row.checked_sub(1) {
                Some(above) => row = above,
                None => return ComposerWalk::ReachedTop,
            }
        }
    }

    /// The text of `row` between columns `start` (inclusive) and `end`
    /// (exclusive) as a human reads it, minus every cell drawn dimmed.
    ///
    /// Dimming is how a CLI separates what it drew itself -- the hint filling
    /// an otherwise empty input area -- from what the user typed, and unlike
    /// the hint's wording it carries no vendor's vocabulary. A terminal that
    /// renders no dim attribute therefore reads such a hint as a body, which
    /// is the safe direction: the composer is treated as occupied.
    fn visible_text(&self, row: u16, start: u16, end: u16) -> String {
        self.row_text(row, start, end, true)
            .map(|(text, _)| text)
            .unwrap_or_default()
    }

    /// The column a composer row read from `start` ends at: where the
    /// section's `right_border` match ending at the row's last visible
    /// character begins, else the screen's width.
    fn composer_row_end(
        &self,
        section: &crate::detector::definition::ComposerSection,
        row: u16,
        start: u16,
    ) -> u16 {
        let (cols, _rows) = self.size();
        let Some(border) = &section.right_border else {
            return cols;
        };
        let grid = self.term.term().grid();
        if usize::from(row) >= grid.screen_lines() {
            return cols;
        }
        let mut columns = Vec::new();
        let (text, _) = cells_text_mapped(
            &grid[Line(i32::from(row))],
            usize::from(start),
            usize::from(cols),
            true,
            |byte, col| columns.push((byte, col)),
        );
        border
            .find_iter(&text)
            .find(|found| found.end() == text.len())
            .and_then(|found| column_at_byte(&columns, found.start()))
            .unwrap_or(cols)
    }

    /// The cursor row from column `start` up to the cursor's column, with
    /// every column the cursor has moved past read as blank even if nothing
    /// was written there: a terminal may draw a space by moving the cursor
    /// alone (ConPTY does after a PowerShell prompt), and a reader sees it as
    /// one.
    fn text_left_of_cursor(&self, cursor_row: u16, start: u16, cursor_col: u16) -> String {
        let (mut text, next_col) = self
            .row_text(cursor_row, start, cursor_col, true)
            .unwrap_or_default();
        for _ in next_col..cursor_col {
            text.push(' ');
        }
        text
    }

    /// [`cells_text`] of a screen row, or `None` below the last row.
    fn row_text(&self, row: u16, start: u16, end: u16, skip_dim: bool) -> Option<(String, u16)> {
        let grid = self.term.term().grid();
        if usize::from(row) >= grid.screen_lines() {
            return None;
        }
        Some(cells_text(
            &grid[Line(i32::from(row))],
            usize::from(start),
            usize::from(end),
            skip_dim,
        ))
    }

    /// A whole screen row as [`cells_text`] reads it with dimmed cells
    /// included, with the `(byte, column)` where each column's text starts,
    /// ending with the text's length and the column past it; `None` below the
    /// last row.
    fn row_text_columns(&self, row: u16) -> Option<(String, Vec<(usize, usize)>)> {
        let grid = self.term.term().grid();
        if usize::from(row) >= grid.screen_lines() {
            return None;
        }
        let mut columns = Vec::new();
        let (text, next_col) = cells_text_mapped(
            &grid[Line(i32::from(row))],
            0,
            grid.columns(),
            false,
            |byte, col| columns.push((byte, col)),
        );
        columns.push((text.len(), usize::from(next_col)));
        Some((text, columns))
    }
}

/// Whether the marker row holds only the section's `placeholder`: nothing but
/// the marker and blanks left of the cursor, and one match spanning the whole
/// visible text right of it, so a hint followed by anything else is not one.
fn is_untyped_hint(
    section: &crate::detector::definition::ComposerSection,
    cursor_line: &str,
    right_of_cursor: &str,
) -> bool {
    let Some(placeholder) = &section.placeholder else {
        return false;
    };
    let Some(marker_match) = section.marker.find(cursor_line) else {
        return false;
    };
    let after_marker = &cursor_line[marker_match.end()..];
    let typed = match after_marker.find(&section.separator) {
        Some(pos) => &after_marker[pos + section.separator.len()..],
        None => after_marker,
    };
    if !typed.trim().is_empty() {
        return false;
    }
    let hint = right_of_cursor.trim();
    placeholder
        .find(hint)
        .is_some_and(|found| found.start() == 0 && found.end() == hint.len())
}

/// The column whose text starts at `byte` in a [`VtScreen::row_text_columns`]
/// map, or `None` when `byte` falls inside a cell.
fn column_at_byte(columns: &[(usize, usize)], byte: usize) -> Option<u16> {
    columns
        .iter()
        .find(|&&(at, _)| at == byte)
        .and_then(|&(_, col)| u16::try_from(col).ok())
}

/// The byte where column `col` starts in a [`VtScreen::row_text_columns`] map.
fn byte_at_column(columns: &[(usize, usize)], col: u16) -> Option<usize> {
    columns
        .iter()
        .find(|&&(_, at)| at == usize::from(col))
        .map(|&(byte, _)| byte)
}

#[cfg(test)]
mod tests {
    //! The guard as the detector's reads see it.
    //!
    //! The default panic hook is left in place, so every test that arms the
    //! seam prints one panic message to stderr; that noise is expected.

    use super::*;

    #[test]
    fn an_ordinary_feed_and_resize_report_intact() {
        let mut screen = VtScreen::new(80, 24);

        assert_eq!(screen.feed(b"ordinary output"), ScreenIntegrity::Intact);
        assert_eq!(screen.resize(60, 20), ScreenIntegrity::Intact);
        assert_eq!(screen.size(), (60, 20));
        assert!(
            screen
                .bottom_lines(1)
                .iter()
                .any(|line| line.contains("ordinary output")),
            "an Intact resize must keep the screen contents readable"
        );
    }

    #[test]
    fn a_poisoned_feed_reports_rebuilt_and_recovers_blank_at_the_current_size() {
        let mut screen = VtScreen::new(80, 24);
        screen.feed(b"before the panic");

        screen.poison_after_calls(0);
        assert_eq!(screen.feed(b"during the panic"), ScreenIntegrity::Rebuilt);

        assert_eq!(screen.size(), (80, 24));
        assert!(
            screen.bottom_lines(24).iter().all(String::is_empty),
            "a rebuilt screen must be blank: everything fed before the panic is lost"
        );
    }

    #[test]
    fn the_screen_stays_usable_after_a_rebuild() {
        let mut screen = VtScreen::new(80, 24);
        screen.poison_after_calls(0);
        screen.feed(b"during the panic");

        assert_eq!(screen.feed(b"still alive"), ScreenIntegrity::Intact);
        assert!(
            screen
                .bottom_lines(1)
                .iter()
                .any(|line| line.contains("still alive")),
            "output fed after a rebuild must reach the new screen"
        );
    }
}
