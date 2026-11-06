//! A window's virtual terminal screen: the child's output in, the screen a
//! person sees out.
//!
//! The terminal itself is `screen_model::Model`, which keeps the child's
//! output inside bounds; this file guards it. The engine can panic on an input
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
use yagura_protocol::{
    InputModes, MouseTracking, ScreenCell, ScreenCopy, ScreenCursor, ScreenLine,
};

use crate::screen_model::{Model, clamp_size, has_content};

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

/// What the PTY host between the child and a screen does that the stream
/// itself does not tell. Whoever runs the PTY knows its host, so the screen is
/// told rather than guessing from the platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PtyHost {
    /// The host asks for Win32 Input Mode (DECSET 9001) itself, as ConPTY
    /// does, so a 9001 in the stream is honored; otherwise it is the child's
    /// and ignored.
    pub requests_win32_input: bool,
}

/// One window's screen, shared by its writer and its readers. The writer
/// releases the lock fairly and it is never poisoned, so a reader neither
/// starves while the child floods nor loses the screen to a panic elsewhere.
/// Waiting for it blocks, so it is taken only on threads that may block.
pub type SharedScreen = Arc<parking_lot::Mutex<VtScreen>>;

/// The count of writes to the child's input a screen follows; see
/// [`VtScreen::follow_writes`].
type WriteCount = Box<dyn Fn() -> u64 + Send + Sync>;

/// A virtual terminal screen that turns a byte stream into display lines.
///
/// The terminal engine is an implementation detail hidden behind this type.
pub struct VtScreen {
    model: Model,
    /// The PTY host the screen is behind; every rebuilt model keeps it.
    host: PtyHost,
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
    writes: WriteCount,
    /// Fault-injection countdown; see [`VtScreen::poison_after_calls`].
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
pub(crate) fn row_wraps(row: &Row<Cell>) -> bool {
    row.len()
        .checked_sub(1)
        .is_some_and(|last| row[Column(last)].flags.contains(Flags::WRAPLINE))
}

impl VtScreen {
    /// Creates a new blank screen of the given size, behind `host`.
    ///
    /// `cols`/`rows` are in `(cols, rows)` order. A zero dimension is
    /// degenerate and is replaced with the standard terminal default, and a
    /// size beyond the model's limits is cut to them; this constructor never
    /// panics.
    #[must_use]
    pub fn new(cols: u16, rows: u16, host: PtyHost) -> Self {
        Self::with_history(cols, rows, 0, host)
    }

    /// Creates a blank screen that keeps up to `history_rows` rows scrolled off
    /// its top, for reading back with [`VtScreen::text_tail`].
    ///
    /// Sized like [`VtScreen::new`]; never panics.
    #[must_use]
    pub fn with_history(cols: u16, rows: u16, history_rows: usize, host: PtyHost) -> Self {
        let (cols, rows) = clamp_size(cols, rows);
        Self {
            model: Model::new(cols, rows, history_rows, host.requests_win32_input),
            host,
            history_rows,
            generation: 0,
            outputs_fed: 0,
            rebuilt_by_read: false,
            writes: Box::new(|| 0),
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

    /// Follows the window's count of writes to its child's input, read
    /// through `writes`: a synchronized update holds the view only until the
    /// next write (see [`VtScreen::frame_held`]). A screen that follows none
    /// never sees one.
    pub fn follow_writes(&mut self, writes: impl Fn() -> u64 + Send + Sync + 'static) {
        self.writes = Box::new(writes);
    }

    /// Whether the program's synchronized update (`CSI ? 2026 h`) holds the
    /// window's view back: it is open and nothing has released it yet -- an
    /// input write, a resize, or more output than the screen's size allows.
    #[must_use]
    pub fn frame_held(&self) -> bool {
        self.model.frame_held((self.writes)())
    }

    /// Keeps the window client's theme colors, which the program's color
    /// queries (OSC 4/10/11/12) are answered from.
    pub fn set_palette(&mut self, palette: yagura_protocol::Palette) {
        self.model.set_palette(palette);
    }

    pub(crate) fn model(&self) -> &Model {
        &self.model
    }

    /// The screen as it stands, for a reader that has no terminal engine:
    /// every row of the live screen with the cells a program wrote on it, the
    /// cursor, the modes and the counts (see [`ScreenCopy`]).
    #[must_use]
    pub fn screen_copy(&self) -> ScreenCopy {
        let grid = self.model.term().grid();
        let cols = grid.columns();
        let lines = (0..grid.screen_lines())
            .filter_map(|row| Some((u16::try_from(row).ok()?, i32::try_from(row).ok()?)))
            .map(|(line, row)| ScreenLine {
                line,
                cells: copied_cells(&grid[Line(row)], cols),
            })
            .collect();
        let cursor = &grid.cursor;
        let (cols, rows) = self.size();
        ScreenCopy {
            cols,
            rows,
            lines,
            cursor: ScreenCursor {
                line: u16::try_from(cursor.point.line.0).unwrap_or(0),
                col: u16::try_from(cursor.point.column.0).unwrap_or(u16::MAX),
                input_needs_wrap: cursor.input_needs_wrap,
            },
            cursor_hidden: self.cursor_hidden(),
            sync_open: self.sync_open(),
            input_modes: self.input_modes(),
            generation: self.generation,
            outputs_fed: self.outputs_fed,
        }
    }

    /// Runs `op` on the model under the same guard as a feed; see
    /// [`VtScreen::guard_call`].
    pub(crate) fn update_model(&mut self, op: impl FnOnce(&mut Model)) -> ScreenIntegrity {
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
                self.rebuild_after_read_panic();
                None
            }
        }
    }

    /// What [`VtScreen::read_guarded`] does after its read panicked.
    fn rebuild_after_read_panic(&mut self) {
        let (cols, rows) = self.size();
        let win32_input = self.model.extras().win32_input;
        self.rebuild(cols, rows, win32_input);
        self.rebuilt_by_read = true;
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
            let text = row_text(&grid[Line(first)], cols);
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
        let writes = (self.writes)();
        self.guard_call(cols, rows, |model: &mut Model| model.feed(bytes, writes))
    }

    /// Takes the replies to the child's terminal queries that the feeds so
    /// far queued, to be written back to the child, and how many replies were
    /// dropped because the queue was full.
    pub fn take_replies(&mut self) -> (Vec<u8>, usize) {
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

    /// The modes the child set that change what input becomes.
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
            win32_input: self.model.extras().win32_input,
            kitty_flags: self
                .model
                .extras()
                .kitty_flags(mode.contains(TermMode::ALT_SCREEN)),
            // Set from the window's program by whoever runs it.
            key_records_flattened: false,
        }
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
        let poisoned = self.poison_is_due();
        let win32_input = self.model.extras().win32_input;

        let model = &mut self.model;
        // `AssertUnwindSafe`: the only state this closure reaches is `model`,
        // and the recovery below discards it wholesale instead of reading the
        // partial update a panic left behind, so no torn value is observable.
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            if poisoned {
                fire_poison_seam();
            }
            op(model);
        }));

        match outcome {
            Ok(()) if std::mem::take(&mut self.rebuilt_by_read) => ScreenIntegrity::Rebuilt,
            Ok(()) => ScreenIntegrity::Intact,
            Err(_) => {
                self.rebuild(cols, rows, win32_input);
                self.rebuilt_by_read = false;
                ScreenIntegrity::Rebuilt
            }
        }
    }

    /// Replaces the model with a blank one after a panic, keeping Win32
    /// Input Mode as it was before the call (see [`Model::keep_win32_input`]).
    fn rebuild(&mut self, cols: u16, rows: u16, win32_input: bool) {
        self.model = Model::new(
            cols,
            rows,
            self.history_rows,
            self.host.requests_win32_input,
        );
        self.model.keep_win32_input(win32_input);
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
    /// A test seam, so the recovery path can be exercised without depending on
    /// a specific engine version's panic conditions. It is always compiled, so
    /// a test fixture can reach it without a feature that workspace feature
    /// unification would carry into shipped binaries; nothing in a shipped
    /// build calls it.
    pub fn poison_after_calls(&mut self, calls: usize) {
        self.poison_countdown = Some(calls);
    }

    /// Advances the countdown by one guarded call; `true` when it is this
    /// call's turn to panic.
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

/// The seam's actual failure, kept in its own item so that the one `panic!`
/// is reached only through an armed seam.
#[allow(clippy::panic)] // the seam exists to make the guarded call panic
fn fire_poison_seam() {
    panic!("poison seam: simulated engine panic");
}

fn has_content_row(row: &Row<Cell>) -> bool {
    row.into_iter().any(has_content)
}

/// The cells of `row` a program wrote something to, as a copy carries them.
fn copied_cells(row: &Row<Cell>, cols: usize) -> Vec<ScreenCell> {
    (0..cols.min(row.len()))
        .filter_map(|col| {
            let cell = &row[Column(col)];
            if !has_content(cell) {
                return None;
            }
            Some(ScreenCell {
                col: u16::try_from(col).ok()?,
                c: cell.c,
                zerowidth: cell.zerowidth().map(<[char]>::to_vec).unwrap_or_default(),
                dim: cell.flags.contains(Flags::DIM),
                wide: cell.flags.contains(Flags::WIDE_CHAR),
            })
        })
        .collect()
}

/// The text of `row` as a person reads it, and nothing past its last
/// written cell. Cells nobody wrote are read as the spaces between the cells
/// that follow them, so a line has no trailing padding; a space a program
/// wrote stays.
fn row_text(row: &Row<Cell>, cols: usize) -> String {
    let mut text = String::new();
    let mut next_col = 0;
    for col in 0..cols.min(row.len()) {
        let cell = &row[Column(col)];
        if !has_content(cell) {
            continue;
        }
        for _ in next_col..col {
            text.push(' ');
        }
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
    text
}

#[cfg(test)]
mod tests {
    //! The guard helper and its poison seam.
    //!
    //! The default panic hook is left in place, so every test that arms the
    //! seam prints one panic message to stderr; that noise is expected.

    use super::*;
    use crate::screen_model::DEFAULT_COLS;

    const HOST: PtyHost = PtyHost {
        requests_win32_input: false,
    };

    #[test]
    fn a_poisoned_resize_recovers_at_the_clamped_target_size() {
        // The recovery size is the resize's own clamped target, decided
        // before the guarded call ran, never a size read back from the
        // model that panicked.
        let mut screen = VtScreen::new(80, 24, HOST);

        screen.poison_after_calls(0);
        assert_eq!(screen.resize(0, 7), ScreenIntegrity::Rebuilt);

        assert_eq!(screen.size(), (DEFAULT_COLS, 7));
    }

    #[test]
    fn the_poison_seam_is_consumed_by_a_single_call() {
        let mut screen = VtScreen::new(80, 24, HOST);
        screen.poison_after_calls(0);

        assert_eq!(screen.feed(b"first"), ScreenIntegrity::Rebuilt);
        assert_eq!(screen.feed(b"second"), ScreenIntegrity::Intact);
    }

    #[test]
    fn replies_are_queued_up_to_the_cap_and_one_past_it_is_dropped_and_counted() {
        const REPLY: &[u8] = b"\x1b[0n";
        let fit = 16 * 1024 / REPLY.len();
        let mut screen = VtScreen::new(80, 24, HOST);

        let integrity = screen.feed(&b"\x1b[5n".repeat(fit + 1));

        assert_eq!(integrity, ScreenIntegrity::Intact);
        assert_eq!(screen.take_replies(), (REPLY.repeat(fit), 1));
    }

    #[test]
    fn a_logical_line_grows_upwards_until_it_is_past_the_budget() {
        let mut screen = VtScreen::with_history(10, 4, 100, HOST);
        screen.feed("0123456789".repeat(3).as_bytes());
        screen.feed(b"01234");

        // Rows 0 to 3 are one line: 10 + 10 + 10 + 5 bytes, read from the bottom.
        let first_row = |budget| screen.logical_line_ending_at(3, 0, budget).1;
        assert_eq!(first_row(usize::MAX), 0);
        assert_eq!(first_row(14), 2, "15 bytes are past a budget of 14");
        assert_eq!(first_row(15), 1, "15 bytes are not past a budget of 15");
    }
}
