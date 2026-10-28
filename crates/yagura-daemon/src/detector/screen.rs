//! Virtual terminal screen model used by the detector to match patterns
//! against "what a human sees on screen". Pattern definitions are written
//! against display text rather than raw byte streams or ANSI-stripped text,
//! because full-screen TUI agents rewrite the screen and move the cursor to
//! redraw spinners; only a real VT parser reproduces that faithfully.
//!
//! The terminal itself is `screen_model::Model`, which keeps the child's
//! output inside bounds; this file reads it. The engine can panic on an input
//! a size check alone cannot rule out, so `VtScreen` defends on two fronts: it
//! clamps degenerate dimensions (see `screen_model::clamp_size`), and every
//! call that mutates or reads the model runs under a guard that turns an
//! escaping panic into a blank rebuilt screen plus a
//! [`ScreenIntegrity::Rebuilt`] answer instead of unwinding into the caller.
//! `size()` therefore always returns cols/rows `>= 1`.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

use alacritty_terminal::grid::{Dimensions, Row};
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::TermMode;
use alacritty_terminal::term::cell::{Cell, Flags};

use super::screen_model::{Model, clamp_size, has_content};
use crate::input_encode::{InputModes, MouseTracking};

/// Whether a screen-mutating call left the screen model intact, or had to
/// rebuild it from scratch to survive a panic inside the terminal engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenIntegrity {
    /// The call completed normally. Everything fed so far is still on screen.
    Intact,
    /// The call panicked inside the engine and the screen was replaced
    /// with a blank one. Everything fed before it is lost, so screen-derived
    /// judgements must start over rather than trust what they read next.
    Rebuilt,
}

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

/// A virtual terminal screen that turns a byte stream into display lines.
///
/// This is the only public surface the detector's pattern matcher touches;
/// the terminal engine is an implementation detail hidden behind this type.
pub struct VtScreen {
    model: Model,
    /// Rows kept after they scroll off the top; every rebuilt model keeps it.
    history_rows: usize,
    /// See [`VtScreen::generation`].
    generation: u64,
    /// Chunks of output taken in; see [`VtScreen::note_output`].
    outputs_fed: u64,
    /// A read panicked and the model was rebuilt; the next feed or resize
    /// reports it, since a read has no way to.
    rebuilt_by_read: bool,
    /// The window's count of input writes; see [`VtScreen::follow_writes`].
    writes: tokio::sync::watch::Receiver<u64>,
    /// Fault-injection countdown; see [`VtScreen::poison_after_calls`].
    #[cfg(any(test, feature = "fault-injection"))]
    poison_countdown: Option<usize>,
}

impl std::fmt::Debug for VtScreen {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (cols, rows) = self.size();
        f.debug_struct("VtScreen")
            .field("cols", &cols)
            .field("rows", &rows)
            .finish()
    }
}

/// The row's last cell carries the engine's mark that the line goes on in the
/// next row.
pub(super) fn row_wraps(row: &Row<Cell>) -> bool {
    row.len()
        .checked_sub(1)
        .is_some_and(|last| row[Column(last)].flags.contains(Flags::WRAPLINE))
}

impl VtScreen {
    /// Creates a new blank screen of the given size.
    ///
    /// `cols`/`rows` follow the same `(cols, rows)` order as
    /// `PtySession::size()`. A zero dimension is degenerate and is replaced
    /// with the standard terminal default, and a size beyond the model's
    /// limits is cut to them; this constructor never panics.
    #[must_use]
    pub fn new(cols: u16, rows: u16) -> Self {
        Self::with_history(cols, rows, 0)
    }

    /// Creates a blank screen that keeps up to `history_rows` rows scrolled off
    /// its top, for reading back with [`VtScreen::text_tail`].
    ///
    /// Sized like [`VtScreen::new`]; never panics.
    #[must_use]
    pub fn with_history(cols: u16, rows: u16, history_rows: usize) -> Self {
        let (cols, rows) = clamp_size(cols, rows);
        Self {
            model: Model::new(cols, rows, history_rows),
            history_rows,
            generation: 0,
            outputs_fed: 0,
            rebuilt_by_read: false,
            writes: tokio::sync::watch::channel(0).1,
            #[cfg(any(test, feature = "fault-injection"))]
            poison_countdown: None,
        }
    }

    /// The text a person sees in this window and could scroll back to, as its
    /// last `max_lines` logical lines: a row the window wrapped joins the next
    /// one, so only a line break the program wrote ends a line. While a
    /// full-screen program has the alternate screen up, that screen is all
    /// there is, as in the GUI. Trailing blank rows, and the spaces a
    /// terminal paints after a line's last character, are left out.
    ///
    /// The text is cut to its last `max_bytes` bytes on a character boundary.
    /// The flag is `true` when lines before the returned ones exist, the byte
    /// limit cut the text, or the history is full (rows may then have been
    /// dropped from it, whether or not the rows that remain show any text).
    ///
    /// Never panics: a panic from inside the engine rebuilds the screen blank
    /// and returns empty text; the next feed or resize reports
    /// [`ScreenIntegrity::Rebuilt`].
    pub fn text_tail(&mut self, max_lines: usize, max_bytes: usize) -> (String, bool) {
        self.read_guarded(|screen| screen.read_tail(max_lines, max_bytes))
            .unwrap_or_default()
    }

    /// Follows the window's count of writes to its child's input: a
    /// synchronized update holds the view only until the next write (see
    /// [`VtScreen::frame_held`]). A screen that follows none never sees one.
    pub(crate) fn follow_writes(&mut self, writes: tokio::sync::watch::Receiver<u64>) {
        self.writes = writes;
    }

    /// Whether the program's synchronized update (`CSI ? 2026 h`) holds the
    /// window's view back: it is open and nothing has released it yet -- an
    /// input write, a resize, or more output than the screen's size allows.
    #[must_use]
    pub fn frame_held(&self) -> bool {
        self.model.frame_held(*self.writes.borrow())
    }

    /// Keeps the window client's theme colors, which the program's color
    /// queries (OSC 4/10/11/12) are answered from.
    pub fn set_palette(&mut self, palette: yagura_protocol::Palette) {
        self.model.set_palette(palette);
    }

    pub(super) fn model(&self) -> &Model {
        &self.model
    }

    /// Runs `op` on the model under the same guard as a feed; see
    /// [`VtScreen::guard_call`].
    pub(super) fn update_model(&mut self, op: impl FnOnce(&mut Model)) -> ScreenIntegrity {
        let (cols, rows) = self.size();
        self.guard_call(cols, rows, op)
    }

    /// Wraps this screen for sharing between its writer and its readers.
    #[must_use]
    pub fn into_shared(self) -> SharedScreen {
        Arc::new(parking_lot::Mutex::new(self))
    }

    /// Counts one chunk of the child's output as taken in, returning the new
    /// count. Called by whoever feeds the screen, once per chunk, while it
    /// still holds the screen.
    pub fn note_output(&mut self) -> u64 {
        self.outputs_fed = self.outputs_fed.wrapping_add(1);
        self.outputs_fed
    }

    /// How many times the model has been rebuilt after a panic and lost what
    /// it showed. A reader that keeps the
    /// value it last saw can tell that the screen it judged is gone.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Runs `read` against this screen and returns what it returns.
    ///
    /// Never panics: a panic from inside the engine rebuilds the screen
    /// blank at its current size, advances [`VtScreen::generation`] and
    /// returns `None`; the next feed or resize reports
    /// [`ScreenIntegrity::Rebuilt`].
    pub fn read_guarded<R>(&mut self, read: impl FnOnce(&Self) -> R) -> Option<R> {
        // `AssertUnwindSafe`: `read` borrows the screen shared, so a panic
        // cannot leave a half-updated value behind.
        match catch_unwind(AssertUnwindSafe(|| read(&*self))) {
            Ok(value) => Some(value),
            Err(_) => {
                let (cols, rows) = self.size();
                self.rebuild(cols, rows);
                self.rebuilt_by_read = true;
                None
            }
        }
    }

    /// Reads logical lines from the last non-blank row upwards, so the work
    /// follows what is asked for and not the length of the history.
    fn read_tail(&self, max_lines: usize, max_bytes: usize) -> (String, bool) {
        let grid = self.model.term().grid();
        let top = grid.topmost_line().0;
        let mut last = grid.bottommost_line().0;
        while last >= top && !has_content_row(&grid[Line(last)]) && !row_wraps(&grid[Line(last)]) {
            last -= 1;
        }

        let mut lines_rev: Vec<String> = Vec::new();
        let mut total = 0usize;
        let mut truncated = false;
        let mut row = last;
        while row >= top {
            if lines_rev.len() >= max_lines || total > max_bytes {
                truncated = true;
                break;
            }
            let (line, first_row) = self.logical_line_ending_at(row, top, max_bytes);
            total = total.saturating_add(line.len() + usize::from(!lines_rev.is_empty()));
            lines_rev.push(line);
            row = first_row - 1;
        }
        if row < top && self.history_rows > 0 {
            truncated |= grid.history_size() >= self.history_rows;
        }

        lines_rev.reverse();
        let text = lines_rev.join("\n");
        if text.len() <= max_bytes {
            return (text, truncated);
        }
        let mut start = text.len() - max_bytes;
        while !text.is_char_boundary(start) {
            start += 1;
        }
        (text[start..].to_string(), true)
    }

    /// The logical line whose last row is `row`, and the grid line of its
    /// first row. Rows are added upwards and stop once the line alone is past
    /// `byte_budget`, so one very long line cannot be assembled in full.
    ///
    /// Only the bytes the line keeps count against the budget: the blank rows
    /// at its end are trimmed away, so stopping inside them would end the line
    /// short of its start, and the caller would read the rest as another line.
    fn logical_line_ending_at(&self, row: i32, top: i32, byte_budget: usize) -> (String, i32) {
        let grid = self.model.term().grid();
        let cols = grid.columns();
        let mut segments = Vec::new();
        let mut len = 0usize;
        let mut first = row;
        loop {
            let (text, _) = cells_text(&grid[Line(first)], 0, cols, false);
            if segments.is_empty() {
                let kept = text.trim_end_matches(' ');
                if !kept.is_empty() {
                    len += kept.len();
                    segments.push(kept.to_string());
                }
            } else {
                len += text.len();
                segments.push(text);
            }
            if len > byte_budget || first <= top || !row_wraps(&grid[Line(first - 1)]) {
                break;
            }
            first -= 1;
        }
        let joined: String = segments.iter().rev().map(String::as_str).collect();
        (joined.trim_end_matches(' ').to_string(), first)
    }

    /// Feeds a chunk of raw PTY output into the screen.
    ///
    /// Input may be split at arbitrary byte boundaries (escape sequences and
    /// multi-byte UTF-8 characters included): the parser retains state
    /// across calls and reassembles split sequences correctly. Malformed
    /// escape sequences and invalid UTF-8 are tolerated.
    ///
    /// Never panics: a panic from inside the engine is caught and the
    /// screen is rebuilt blank at its current size, which this reports as
    /// [`ScreenIntegrity::Rebuilt`] (see [`VtScreen::guard_call`]). The chunk
    /// that triggered the rebuild is lost along with everything fed before it.
    pub fn feed(&mut self, bytes: &[u8]) -> ScreenIntegrity {
        let (cols, rows) = self.size();
        let writes = *self.writes.borrow();
        self.guard_call(cols, rows, |model: &mut Model| model.feed(bytes, writes))
    }

    /// Takes the replies to the child's terminal queries that the feeds so
    /// far queued, to be written back to the child, and how many replies were
    /// dropped because the queue was full.
    pub(crate) fn take_replies(&mut self) -> (Vec<u8>, usize) {
        self.model.take_replies()
    }

    /// Returns the current screen size as `(cols, rows)`.
    #[must_use]
    pub fn size(&self) -> (u16, u16) {
        let term = self.model.term();
        (
            u16::try_from(term.columns()).unwrap_or(u16::MAX),
            u16::try_from(term.screen_lines()).unwrap_or(u16::MAX),
        )
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

    /// Resizes the virtual screen, keeping its contents readable afterwards.
    ///
    /// A zero dimension is degenerate and is replaced the same way as in
    /// [`VtScreen::new`].
    ///
    /// Never panics: a panic from inside the engine is caught and the
    /// screen is rebuilt blank at the clamped target size, which this reports
    /// as [`ScreenIntegrity::Rebuilt`] (see [`VtScreen::guard_call`]). The
    /// resize still takes effect in that case; only the contents are lost.
    pub fn resize(&mut self, cols: u16, rows: u16) -> ScreenIntegrity {
        let (cols, rows) = clamp_size(cols, rows);
        self.guard_call(cols, rows, |model: &mut Model| model.resize(cols, rows))
    }

    /// Whether the cursor is currently hidden (`ESC[?25l`), which CLIs and
    /// ConPTY use to bracket a repaint in progress.
    #[must_use]
    pub fn cursor_hidden(&self) -> bool {
        !self.model.term().mode().contains(TermMode::SHOW_CURSOR)
    }

    /// Whether the program has a synchronized update (`CSI ? 2026 h`) open,
    /// so the frame it brackets is not finished yet. Unlike
    /// [`VtScreen::frame_held`], only the program closing it (or a reset)
    /// ends it.
    #[must_use]
    pub fn sync_open(&self) -> bool {
        self.model.sync_open()
    }

    /// The modes the child set that change what input becomes (see
    /// [`crate::input_encode`]).
    #[must_use]
    pub fn input_modes(&self) -> InputModes {
        let mode = self.model.term().mode();
        let mouse = if mode.contains(TermMode::MOUSE_MOTION) {
            MouseTracking::Motion
        } else if mode.contains(TermMode::MOUSE_DRAG) {
            MouseTracking::Drag
        } else if mode.contains(TermMode::MOUSE_REPORT_CLICK) {
            MouseTracking::Click
        } else if self.model.extras().x10_mouse {
            MouseTracking::X10
        } else {
            MouseTracking::Off
        };
        InputModes {
            app_cursor: mode.contains(TermMode::APP_CURSOR),
            bracketed_paste: mode.contains(TermMode::BRACKETED_PASTE),
            mouse,
            sgr_mouse: mode.contains(TermMode::SGR_MOUSE),
            alternate_scroll: mode.contains(TermMode::ALTERNATE_SCROLL),
            alt_screen: mode.contains(TermMode::ALT_SCREEN),
            focus_reports: mode.contains(TermMode::FOCUS_IN_OUT),
        }
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
        let cursor = &self.model.term().grid().cursor;
        let row = u16::try_from(cursor.point.line.0).unwrap_or(0);
        let col = u16::try_from(cursor.point.column.0 + usize::from(cursor.input_needs_wrap))
            .unwrap_or(u16::MAX);
        (row, col)
    }

    /// Every screen row as text, covering all contents (dimmed ones too).
    fn screen_rows(&self) -> Vec<String> {
        let grid = self.model.term().grid();
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
        let grid = self.model.term().grid();
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
        let grid = self.model.term().grid();
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
        let grid = self.model.term().grid();
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

    /// Runs `op` -- a call that mutates the model -- and contains any panic it
    /// raises by replacing the model with a blank `(cols, rows)` screen.
    ///
    /// Invariant: every call that mutates the model goes through this helper.
    /// Construction ([`VtScreen::with_history`]) builds a fresh model instead.
    ///
    /// Invariant: `cols` / `rows` are decided by the caller *before* `op`
    /// runs. A model that panicked mid-update is left in an unspecified state,
    /// so nothing may be read back from it afterwards -- not even its size.
    ///
    /// The recovery is reported, never logged with the panic payload or the
    /// bytes that triggered it: the log line carries the new size and nothing
    /// else, so PTY output cannot leak into it.
    fn guard_call(&mut self, cols: u16, rows: u16, op: impl FnOnce(&mut Model)) -> ScreenIntegrity {
        #[cfg(any(test, feature = "fault-injection"))]
        let poisoned = self.poison_is_due();

        let model = &mut self.model;
        // `AssertUnwindSafe`: the only state this closure reaches is `model`,
        // and the recovery below discards it wholesale instead of reading the
        // partial update a panic left behind, so no torn value is observable.
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            #[cfg(any(test, feature = "fault-injection"))]
            if poisoned {
                fire_poison_seam();
            }
            op(model);
        }));

        match outcome {
            Ok(()) if std::mem::take(&mut self.rebuilt_by_read) => ScreenIntegrity::Rebuilt,
            Ok(()) => ScreenIntegrity::Intact,
            Err(_) => {
                self.rebuild(cols, rows);
                self.rebuilt_by_read = false;
                ScreenIntegrity::Rebuilt
            }
        }
    }

    /// Replaces the model with a blank one after a panic.
    fn rebuild(&mut self, cols: u16, rows: u16) {
        self.model = Model::new(cols, rows, self.history_rows);
        self.generation = self.generation.wrapping_add(1);
        tracing::warn!(
            cols,
            rows,
            "the virtual terminal screen model panicked and was rebuilt blank; \
             screen-derived detection restarts from an empty screen"
        );
    }

    /// Arms the fault-injection seam: after `calls` more calls through
    /// [`VtScreen::guard_call`] have completed, the next one panics from inside
    /// the guarded call itself, after the recovery size has been decided. The
    /// seam is consumed by that one call.
    ///
    /// Exists so the recovery path can be exercised without depending on a
    /// specific engine version's panic conditions. Compiled for tests and
    /// behind the `fault-injection` feature only.
    #[cfg(any(test, feature = "fault-injection"))]
    pub fn poison_after_calls(&mut self, calls: usize) {
        self.poison_countdown = Some(calls);
    }

    /// Advances the countdown by one guarded call; `true` when it is this
    /// call's turn to panic.
    #[cfg(any(test, feature = "fault-injection"))]
    fn poison_is_due(&mut self) -> bool {
        match self.poison_countdown {
            Some(0) => {
                self.poison_countdown = None;
                true
            }
            Some(remaining) => {
                self.poison_countdown = Some(remaining - 1);
                false
            }
            None => false,
        }
    }
}

/// The seam's actual failure, kept in its own item so that no `panic!` exists
/// in a build without the seam.
#[cfg(any(test, feature = "fault-injection"))]
#[allow(clippy::panic)] // the seam exists to make the guarded call panic
fn fire_poison_seam() {
    panic!("poison seam: simulated engine panic");
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

fn has_content_row(row: &Row<Cell>) -> bool {
    row.into_iter().any(has_content)
}

/// The text of `row` between columns `start` (inclusive) and `end`
/// (exclusive), and the column just past the last cell it took. Cells nobody
/// wrote are skipped and read as the spaces between the cells that follow
/// them, so a line has no trailing padding; a space a program wrote stays.
/// With `skip_dim`, dimmed cells are skipped like unwritten ones.
fn cells_text(row: &Row<Cell>, start: usize, end: usize, skip_dim: bool) -> (String, u16) {
    cells_text_mapped(row, start, end, skip_dim, |_, _| {})
}

/// [`cells_text`], calling `column_starts(byte, col)` where the text of each
/// column begins. A combining mark adds bytes but no column, and a wide
/// character's second column has no start of its own.
fn cells_text_mapped(
    row: &Row<Cell>,
    start: usize,
    end: usize,
    skip_dim: bool,
    mut column_starts: impl FnMut(usize, usize),
) -> (String, u16) {
    let mut text = String::new();
    let mut next_col = start;
    for col in start..end.min(row.len()) {
        let cell = &row[Column(col)];
        if !has_content(cell) || (skip_dim && cell.flags.contains(Flags::DIM)) {
            continue;
        }
        for blank in next_col..col {
            column_starts(text.len(), blank);
            text.push(' ');
        }
        column_starts(text.len(), col);
        text.push(if cell.c == '\t' { ' ' } else { cell.c });
        if let Some(marks) = cell.zerowidth() {
            text.extend(marks);
        }
        next_col = col
            + if cell.flags.contains(Flags::WIDE_CHAR) {
                2
            } else {
                1
            };
    }
    (text, u16::try_from(next_col).unwrap_or(u16::MAX))
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
    //! The guard helper and its `#[cfg(test)]` poison seam.
    //!
    //! The default panic hook is left in place, so every test that arms the
    //! seam prints one panic message to stderr; that noise is expected.

    use super::super::screen_model::DEFAULT_COLS;
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
    fn a_poisoned_resize_recovers_at_the_clamped_target_size() {
        // The recovery size is the resize's own clamped target, decided
        // before the guarded call ran, never a size read back from the
        // model that panicked.
        let mut screen = VtScreen::new(80, 24);

        screen.poison_after_calls(0);
        assert_eq!(screen.resize(0, 7), ScreenIntegrity::Rebuilt);

        assert_eq!(screen.size(), (DEFAULT_COLS, 7));
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

    #[test]
    fn the_poison_seam_is_consumed_by_a_single_call() {
        let mut screen = VtScreen::new(80, 24);
        screen.poison_after_calls(0);

        assert_eq!(screen.feed(b"first"), ScreenIntegrity::Rebuilt);
        assert_eq!(screen.feed(b"second"), ScreenIntegrity::Intact);
    }

    #[test]
    fn replies_are_queued_up_to_the_cap_and_one_past_it_is_dropped_and_counted() {
        const REPLY: &[u8] = b"\x1b[0n";
        let fit = 16 * 1024 / REPLY.len();
        let mut screen = VtScreen::new(80, 24);

        let integrity = screen.feed(&b"\x1b[5n".repeat(fit + 1));

        assert_eq!(integrity, ScreenIntegrity::Intact);
        assert_eq!(screen.take_replies(), (REPLY.repeat(fit), 1));
    }

    #[test]
    fn a_logical_line_grows_upwards_until_it_is_past_the_budget() {
        let mut screen = VtScreen::with_history(10, 4, 100);
        screen.feed("0123456789".repeat(3).as_bytes());
        screen.feed(b"01234");

        // Rows 0 to 3 are one line: 10 + 10 + 10 + 5 bytes, read from the bottom.
        let first_row = |budget| screen.logical_line_ending_at(3, 0, budget).1;
        assert_eq!(first_row(usize::MAX), 0);
        assert_eq!(first_row(14), 2, "15 bytes are past a budget of 14");
        assert_eq!(first_row(15), 1, "15 bytes are not past a budget of 15");
    }
}
