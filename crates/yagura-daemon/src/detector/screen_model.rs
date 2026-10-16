//! The terminal model under [`super::screen::VtScreen`]: a single
//! `alacritty_terminal` state fed from a child's output, which is untrusted.
//!
//! The engine is not changed; every bound lives here, in front of it: the
//! number of characters one feed may write, the cells it may rewrite, every
//! count capped where a larger one changes nothing, no synchronized-output
//! buffering in the parser (an open update is only recorded, see
//! [`SyncHold`]), a cap on combining characters per cell, no window titles, a cap
//! on OSC length, and a ceiling on the size of the screen. The model also keeps
//! what the engine does not model or hides (see [`Extras`]). Nothing outside
//! this file, `screen.rs` and `screen_view.rs` names an
//! engine type.

use alacritty_terminal::event::VoidListener;
use alacritty_terminal::grid::{Dimensions, Grid};
use alacritty_terminal::index::Column;
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::term::{Config, MIN_COLUMNS, Term, TermMode};
use alacritty_terminal::vte::ansi::{
    Attr, CharsetIndex, ClearMode, CursorStyle, Handler, LineClearMode, Mode, NamedMode,
    NamedPrivateMode, PrivateMode, Processor, Rgb, StandardCharset, TabulationClearMode, Timeout,
};
use unicode_width::UnicodeWidthChar;
use yagura_protocol::Palette;

/// Column count substituted when a caller passes `cols == 0`.
pub(super) const DEFAULT_COLS: u16 = 80;

/// Row count substituted when a caller passes `rows == 0`.
pub(super) const DEFAULT_ROWS: u16 = 24;

/// The widest and tallest screen the model accepts; larger requests are cut to
/// it. The same limit the daemon enforces on every window resize.
const MAX_SIDE: u16 = yagura_protocol::MAX_WINDOW_DIMENSION;

/// The narrowest and shortest screen the model accepts, as the daemon enforces
/// on every window resize; never below what the engine can hold.
fn min_size() -> (u16, u16) {
    let engine_min_cols = u16::try_from(MIN_COLUMNS).unwrap_or(u16::MAX);
    (
        yagura_protocol::MIN_WINDOW_COLS.max(engine_min_cols),
        yagura_protocol::MIN_WINDOW_ROWS,
    )
}

/// Combining characters kept on one cell; later ones are dropped.
const MAX_ZEROWIDTH_PER_CELL: usize = 16;

/// OSC payload bytes passed on before the string is cut off.
const MAX_OSC_BYTES: usize = 64 * 1024;

/// Reply bytes held until the caller takes them; a reply that would go past
/// this is dropped whole.
const MAX_PENDING_REPLY_BYTES: usize = 16 * 1024;

/// Bytes a synchronized update may take per cell of the screen before the
/// view shows it anyway: a whole repaint with a color change on every cell
/// fits, a program that never ends the update does not hold the view forever.
const SYNC_HOLD_BYTES_PER_CELL: usize = 64;

/// Palette entries kept from a client's palette; OSC 4 names no more.
const MAX_PALETTE_ENTRIES: usize = 256;

/// The primary device attributes answered to a query, as xterm.js answers
/// them: a VT100 with advanced video.
const PRIMARY_DEVICE_ATTRIBUTES: &[u8] = b"\x1b[?1;2c";

/// The secondary device attributes answered to a query, as xterm.js answers them.
const SECONDARY_DEVICE_ATTRIBUTES: &[u8] = b"\x1b[>0;276;0c";

/// A DECRQM answer, as DECRPM encodes it.
#[derive(Clone, Copy)]
enum ModeState {
    NotRecognized = 0,
    Set = 1,
    Reset = 2,
}

impl From<bool> for ModeState {
    fn from(set: bool) -> Self {
        if set { Self::Set } else { Self::Reset }
    }
}

/// Marks a cell a program wrote a space to, which the engine cannot tell from
/// a cell nobody wrote (both are a space with default attributes). The engine
/// uses the bits below this one.
pub(super) const WRITTEN_BLANK: Flags = Flags::from_bits_retain(1 << 15);

/// Mouse tracking modes of the engine; at most one is on at a time.
const MOUSE_PROTOCOLS: [NamedPrivateMode; 3] = [
    NamedPrivateMode::ReportMouseClicks,
    NamedPrivateMode::ReportCellMouseMotion,
    NamedPrivateMode::ReportAllMouseMotion,
];

const ESC: u8 = 0x1b;
const BEL: u8 = 0x07;
const CAN: u8 = 0x18;
const SUB: u8 = 0x1a;

/// Clamps a requested size to what the model supports: a zero dimension takes
/// the standard default, and the result stays within the window size limits
/// (see [`min_size`] and [`MAX_SIDE`]).
pub(super) fn clamp_size(cols: u16, rows: u16) -> (u16, u16) {
    let cols = if cols == 0 { DEFAULT_COLS } else { cols };
    let rows = if rows == 0 { DEFAULT_ROWS } else { rows };
    let (min_cols, min_rows) = min_size();
    (
        cols.clamp(min_cols, MAX_SIDE),
        rows.clamp(min_rows, MAX_SIDE),
    )
}

/// Whether a program put something on this cell, as opposed to the cell being
/// blank: an unwritten cell, a tab's padding, or the half of a wide character.
pub(super) fn has_content(cell: &Cell) -> bool {
    if cell
        .flags
        .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
    {
        return false;
    }
    !matches!(cell.c, ' ' | '\t')
        || cell.flags.contains(WRITTEN_BLANK)
        || cell.zerowidth().is_some_and(|marks| !marks.is_empty())
}

/// A synchronized update (`CSI ? 2026 h`) the program has open. The view
/// sends no frame while it holds; see [`Model::frame_held`].
#[derive(Debug, Clone, Copy)]
pub(super) struct SyncHold {
    /// Bytes fed since it opened, counted by whole feeds: the feed that opens
    /// it counts in full.
    bytes: usize,
    /// The window's count of input writes when it opened; any later write
    /// releases it.
    writes_at_open: u64,
    /// A resize released it.
    released: bool,
}

/// What the model keeps because the engine hides it or does not model it.
pub(super) struct Extras {
    /// Mouse mode 9 (X10 reporting), which the engine does not know. It is
    /// exclusive with the engine's own mouse modes, as in the GUI's terminal.
    pub(super) x10_mouse: bool,
    /// Reverse wrap-around (mode 45), which the engine does not know.
    pub(super) reverse_wrap: bool,
    /// The main screen while the alternate one is up. The engine keeps it
    /// where nothing can read it, so it is held here instead and handed back
    /// when the alternate screen is left.
    pub(super) parked_main: Option<Grid<Cell>>,
    history_rows: usize,
    /// Replies to the child's queries, waiting to be written back to it.
    replies: Vec<u8>,
    /// Replies dropped since the last take because `replies` was full.
    dropped_replies: usize,
    /// The synchronized update in progress, if any.
    sync: Option<SyncHold>,
    /// The theme colors of the window's client, which color queries are
    /// answered from; `None` until a client sent them. Kept across a reset:
    /// they are the client's, not the program's.
    palette: Option<Palette>,
}

impl Extras {
    fn new(history_rows: usize) -> Self {
        Self {
            x10_mouse: false,
            reverse_wrap: false,
            parked_main: None,
            history_rows,
            replies: Vec::new(),
            dropped_replies: 0,
            sync: None,
            palette: None,
        }
    }

    fn queue_reply(&mut self, reply: &[u8]) {
        if self.replies.len() + reply.len() > MAX_PENDING_REPLY_BYTES {
            self.dropped_replies += 1;
        } else {
            self.replies.extend_from_slice(reply);
        }
    }
}

/// The screen's dimensions, in the form the engine takes them.
struct Size {
    cols: usize,
    rows: usize,
}

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.rows
    }

    fn screen_lines(&self) -> usize {
        self.rows
    }

    fn columns(&self) -> usize {
        self.cols
    }
}

/// A synchronized-output timeout that is never pending, so the parser applies
/// output as it arrives instead of holding it back for a frame.
#[derive(Default)]
struct NoSyncTimeout;

impl Timeout for NoSyncTimeout {
    fn set_timeout(&mut self, _duration: std::time::Duration) {}

    fn clear_timeout(&mut self) {}

    fn pending_timeout(&self) -> bool {
        false
    }
}

#[derive(Clone, Copy, Default)]
enum OscState {
    /// Outside an escape sequence.
    #[default]
    Other,
    /// Just after an ESC.
    Escape,
    /// Inside an OSC string, with this many payload bytes passed on.
    Body(usize),
    /// Inside an OSC string that went over the limit; the rest is dropped.
    Discarding,
}

/// Cuts an OSC string that runs past [`MAX_OSC_BYTES`]: the parser buffers a
/// string's bytes without bound, so a CAN is put in at the limit and the
/// remainder is dropped up to the string's end. The parser takes that CAN as
/// the string's end and applies what it has, so a longer string takes effect
/// truncated. State carries across chunks.
#[derive(Default)]
struct OscLimiter {
    state: OscState,
}

impl OscLimiter {
    fn filter(&mut self, input: &[u8], out: &mut Vec<u8>) {
        let mut rest = input;
        while let Some(&first) = rest.first() {
            match self.state {
                OscState::Other => match rest.iter().position(|&b| b == ESC) {
                    Some(esc) => {
                        out.extend_from_slice(&rest[..=esc]);
                        rest = &rest[esc + 1..];
                        self.state = OscState::Escape;
                    }
                    None => {
                        out.extend_from_slice(rest);
                        return;
                    }
                },
                OscState::Escape => {
                    out.push(first);
                    rest = &rest[1..];
                    self.state = match first {
                        b']' => OscState::Body(0),
                        ESC => OscState::Escape,
                        _ => OscState::Other,
                    };
                }
                OscState::Body(passed) => {
                    let end = rest.iter().position(|&b| is_osc_end(b));
                    let until_end = end.unwrap_or(rest.len());
                    let take = until_end.min(MAX_OSC_BYTES - passed);
                    out.extend_from_slice(&rest[..take]);
                    rest = &rest[take..];
                    if take < until_end {
                        out.push(CAN);
                        self.state = OscState::Discarding;
                    } else if let Some(&end_byte) = rest.first() {
                        out.push(end_byte);
                        rest = &rest[1..];
                        self.state = if end_byte == ESC {
                            OscState::Escape
                        } else {
                            OscState::Other
                        };
                    } else {
                        self.state = OscState::Body(passed + take);
                    }
                }
                OscState::Discarding => {
                    let Some(end) = rest.iter().position(|&b| is_osc_end(b)) else {
                        return;
                    };
                    rest = &rest[end..];
                    if rest.first() == Some(&ESC) {
                        out.push(ESC);
                        self.state = OscState::Escape;
                    } else {
                        self.state = OscState::Other;
                    }
                    rest = &rest[1..];
                }
            }
        }
    }
}

fn is_osc_end(byte: u8) -> bool {
    matches!(byte, BEL | ESC | CAN | SUB)
}

/// Forwards each listed [`Handler`] method to the engine unchanged.
macro_rules! forward {
    ($(fn $name:ident(&mut self $(, $arg:ident: $ty:ty)*);)*) => {
        $(fn $name(&mut self $(, $arg: $ty)*) {
            self.term.$name($($arg),*);
        })*
    };
}

/// Forwards each listed [`Handler`] method to the engine after charging the
/// worst-case number of cells it touches (the expression, given the handler)
/// to the work budget; a call the budget cannot pay for is dropped.
macro_rules! charged {
    ($(fn $name:ident(&mut self $(, $arg:ident: $ty:ty)*) => $cost:expr;)*) => {
        $(fn $name(&mut self $(, $arg: $ty)*) {
            let cost: usize = ($cost)(&*self);
            if self.spend(cost) {
                self.term.$name($($arg),*);
            }
        })*
    };
}

/// Forwards each listed [`Handler`] method that takes a count. The count is
/// first capped at its reach (a handler method): the largest count whose
/// outcome a larger one could not change, so a count never drives more engine
/// work than the screen can show. The cells the engine walks for the capped
/// count (another handler method) are then charged as in [`charged!`]. A
/// count whose every unit changes what is drawn, such as a repeat, is not a
/// count of this kind.
macro_rules! counted {
    ($(fn $name:ident(&mut self, $count:ident: $ty:ty) reach $reach:ident, cost $cost:ident;)*) => {
        $(fn $name(&mut self, $count: $ty) {
            let $count = capped($count, self.$reach());
            if self.spend(self.$cost($count.into())) {
                self.term.$name($count);
            }
        })*
    };
}

/// `count`, or `reach` when that is smaller.
fn capped<T: Copy + Into<usize> + TryFrom<usize>>(count: T, reach: usize) -> T {
    T::try_from(count.into().min(reach)).unwrap_or(count)
}

/// The parser's handler: the engine behind the bounds that need to see each
/// call. The daemon is the window's only terminal, so the handler answers the
/// child's queries itself from the engine's modes and [`Extras`], and queues
/// the replies; the engine's own events are discarded. Anything neither
/// forwarded nor answered is something the model has no use for (titles,
/// clipboard, hyperlinks, keyboard protocols).
struct BoundedHandler<'a> {
    term: &'a mut Term<VoidListener>,
    /// The window's count of input writes, for a synchronized update opened
    /// by this feed.
    writes: u64,
    /// Characters this feed may still write.
    budget: usize,
    /// Cells this feed may still rewrite; see [`BoundedHandler::spend`].
    work: usize,
    extras: &'a mut Extras,
}

impl BoundedHandler<'_> {
    fn cols(&self) -> usize {
        self.term.columns()
    }

    fn rows(&self) -> usize {
        self.term.screen_lines()
    }

    fn area(&self) -> usize {
        self.term.columns() * self.term.screen_lines()
    }

    /// The cost of a count that only moves the cursor.
    fn free(&self, _count: usize) -> usize {
        0
    }

    /// The cost of a count that rewrites at most one row.
    fn one_row(&self, _count: usize) -> usize {
        self.cols()
    }

    /// The cost of a count that walks a whole row per unit.
    fn row_per_unit(&self, count: usize) -> usize {
        count * self.cols()
    }

    /// The reach of an upward move: the engine adds the origin-mode offset
    /// after subtracting the count, so only a count past twice the height is
    /// sure to land on the top row.
    fn upward_reach(&self) -> usize {
        2 * self.term.screen_lines()
    }

    /// Takes `cells` from the work budget. When it cannot pay, the budget is
    /// emptied (so nothing later in the feed rewrites the screen either) and
    /// the call is to be dropped. Cursor moves and mode changes cost nothing
    /// unless the engine walks a row to make them.
    fn spend(&mut self, cells: usize) -> bool {
        match self.work.checked_sub(cells) {
            Some(left) => {
                self.work = left;
                true
            }
            None => {
                self.work = 0;
                false
            }
        }
    }

    /// Whether one more combining character fits on the cell it would attach to.
    fn combining_fits(&self) -> bool {
        let grid = self.term.grid();
        let cursor = &grid.cursor;
        let line = cursor.point.line;
        let mut column = cursor.point.column;
        if !cursor.input_needs_wrap {
            column = Column(column.0.saturating_sub(1));
        }
        if grid[line][column].flags.contains(Flags::WIDE_CHAR_SPACER) {
            column = Column(column.0.saturating_sub(1));
        }
        grid[line][column]
            .zerowidth()
            .is_none_or(|marks| marks.len() < MAX_ZEROWIDTH_PER_CELL)
    }

    /// Marks the cell a space was just written to; see [`WRITTEN_BLANK`].
    fn mark_written_blank(&mut self) {
        let grid = self.term.grid_mut();
        let line = grid.cursor.point.line;
        let mut column = grid.cursor.point.column;
        if !grid.cursor.input_needs_wrap {
            column = Column(column.0.saturating_sub(1));
        }
        grid[line][column].flags.insert(WRITTEN_BLANK);
    }

    fn in_alt_screen(&self) -> bool {
        self.term.mode().contains(TermMode::ALT_SCREEN)
    }

    /// Switches to the alternate screen, parking the main screen in the model.
    /// `saves_cursor` is the 1049 form, which also keeps the cursor for the
    /// way back; 47 and 1047 leave the saved cursor as it was.
    fn enter_alt_screen(&mut self, saves_cursor: bool) {
        if self.in_alt_screen() {
            return;
        }
        let blank = Grid::new(
            self.term.screen_lines(),
            self.term.columns(),
            self.extras.history_rows,
        );
        let mut main = std::mem::replace(self.term.grid_mut(), blank);
        // The engine's switch copies the shown screen's cursor to the
        // alternate one, so the stand-in carries the main cursor.
        self.term.grid_mut().cursor = main.cursor.clone();
        if saves_cursor {
            main.saved_cursor = main.cursor.clone();
        }
        self.term.swap_alt();
        self.extras.parked_main = Some(main);
    }

    /// Queues `reply` for the child, as one unit of the work budget.
    fn reply(&mut self, reply: &[u8]) {
        if self.spend(1) {
            self.extras.queue_reply(reply);
        }
    }

    /// A private mode's state as the model holds it: the engine's modes, and
    /// [`Extras`] for the ones the engine does not know.
    fn private_mode_state(&self, mode: PrivateMode) -> ModeState {
        let modes = self.term.mode();
        let named = match mode {
            PrivateMode::Named(named) => named,
            // 47 and 1047 are the alternate screen without the cursor save.
            PrivateMode::Unknown(47 | 1047) => return self.in_alt_screen().into(),
            PrivateMode::Unknown(9) => return self.extras.x10_mouse.into(),
            PrivateMode::Unknown(45) => return self.extras.reverse_wrap.into(),
            PrivateMode::Unknown(66) => return modes.contains(TermMode::APP_KEYPAD).into(),
            PrivateMode::Unknown(_) => return ModeState::NotRecognized,
        };
        let flag = match named {
            NamedPrivateMode::CursorKeys => TermMode::APP_CURSOR,
            NamedPrivateMode::Origin => TermMode::ORIGIN,
            NamedPrivateMode::LineWrap => TermMode::LINE_WRAP,
            NamedPrivateMode::ShowCursor => TermMode::SHOW_CURSOR,
            NamedPrivateMode::ReportMouseClicks => TermMode::MOUSE_REPORT_CLICK,
            NamedPrivateMode::ReportCellMouseMotion => TermMode::MOUSE_DRAG,
            NamedPrivateMode::ReportAllMouseMotion => TermMode::MOUSE_MOTION,
            NamedPrivateMode::ReportFocusInOut => TermMode::FOCUS_IN_OUT,
            NamedPrivateMode::Utf8Mouse => TermMode::UTF8_MOUSE,
            NamedPrivateMode::SgrMouse => TermMode::SGR_MOUSE,
            NamedPrivateMode::AlternateScroll => TermMode::ALTERNATE_SCROLL,
            NamedPrivateMode::UrgencyHints => TermMode::URGENCY_HINTS,
            NamedPrivateMode::SwapScreenAndSetRestoreCursor => TermMode::ALT_SCREEN,
            NamedPrivateMode::BracketedPaste => TermMode::BRACKETED_PASTE,
            NamedPrivateMode::BlinkingCursor => {
                return self.term.cursor_style().blinking.into();
            }
            NamedPrivateMode::SyncUpdate => return self.extras.sync.is_some().into(),
            // The engine keeps no 132-column state.
            NamedPrivateMode::ColumnMode => return ModeState::NotRecognized,
        };
        modes.contains(flag).into()
    }

    /// Leaves the alternate screen and puts the parked main screen back.
    fn leave_alt_screen(&mut self) {
        if !self.in_alt_screen() {
            return;
        }
        self.term.swap_alt();
        if let Some(main) = self.extras.parked_main.take() {
            *self.term.grid_mut() = main;
        }
    }
}

impl Handler for BoundedHandler<'_> {
    forward! {
        fn goto(&mut self, line: i32, col: usize);
        fn goto_line(&mut self, line: i32);
        fn goto_col(&mut self, col: usize);
        fn backspace(&mut self);
        fn carriage_return(&mut self);
        fn substitute(&mut self);
        fn set_horizontal_tabstop(&mut self);
        fn save_cursor_position(&mut self);
        fn restore_cursor_position(&mut self);
        fn clear_tabs(&mut self, mode: TabulationClearMode);
        fn set_tabs(&mut self, interval: u16);
        fn terminal_attribute(&mut self, attr: Attr);
        fn set_mode(&mut self, mode: Mode);
        fn unset_mode(&mut self, mode: Mode);
        fn set_keypad_application_mode(&mut self);
        fn unset_keypad_application_mode(&mut self);
        fn set_active_charset(&mut self, index: CharsetIndex);
        fn configure_charset(&mut self, index: CharsetIndex, charset: StandardCharset);
        fn set_scrolling_region(&mut self, top: usize, bottom: Option<usize>);
    }

    charged! {
        fn linefeed(&mut self) => |h: &Self| h.cols();
        fn newline(&mut self) => |h: &Self| h.cols();
        fn reverse_index(&mut self) => |h: &Self| h.cols();
        fn clear_line(&mut self, mode: LineClearMode) => |h: &Self| h.cols();
        fn decaln(&mut self) => |h: &Self| h.area();
    }

    counted! {
        fn move_up(&mut self, lines: usize) reach upward_reach, cost free;
        fn move_up_and_cr(&mut self, lines: usize) reach upward_reach, cost free;
        fn move_down(&mut self, lines: usize) reach rows, cost free;
        fn move_down_and_cr(&mut self, lines: usize) reach rows, cost free;
        fn move_forward(&mut self, cols: usize) reach cols, cost free;
        fn move_backward(&mut self, cols: usize) reach cols, cost free;
        // Every step moves on at least one column or stops, so all of them
        // together walk one row.
        fn move_forward_tabs(&mut self, count: u16) reach cols, cost one_row;
        // Every step scans back from the cursor, which stays put when the row
        // has no stop.
        fn move_backward_tabs(&mut self, count: u16) reach cols, cost row_per_unit;
        fn put_tab(&mut self, count: u16) reach cols, cost one_row;
        fn insert_blank(&mut self, count: usize) reach cols, cost one_row;
        fn erase_chars(&mut self, count: usize) reach cols, cost one_row;
        fn delete_chars(&mut self, count: usize) reach cols, cost one_row;
        fn scroll_up(&mut self, lines: usize) reach rows, cost row_per_unit;
        fn scroll_down(&mut self, lines: usize) reach rows, cost row_per_unit;
        fn insert_blank_lines(&mut self, lines: usize) reach rows, cost row_per_unit;
        fn delete_lines(&mut self, lines: usize) reach rows, cost row_per_unit;
    }

    fn reset_state(&mut self) {
        if self.spend(self.area()) {
            self.term.reset_state();
            // Replies to queries earlier in this feed still go out.
            let replies = std::mem::take(&mut self.extras.replies);
            let dropped = self.extras.dropped_replies;
            let palette = self.extras.palette.take();
            *self.extras = Extras::new(self.extras.history_rows);
            self.extras.replies = replies;
            self.extras.dropped_replies = dropped;
            self.extras.palette = palette;
        }
    }

    fn identify_terminal(&mut self, intermediate: Option<char>) {
        match intermediate {
            None => self.reply(PRIMARY_DEVICE_ATTRIBUTES),
            Some('>') => self.reply(SECONDARY_DEVICE_ATTRIBUTES),
            Some(_) => {}
        }
    }

    fn device_status(&mut self, arg: usize) {
        match arg {
            5 => self.reply(b"\x1b[0n"),
            6 => {
                let point = self.term.grid().cursor.point;
                let report = format!("\x1b[{};{}R", point.line.0 + 1, point.column.0 + 1);
                self.reply(report.as_bytes());
            }
            _ => {}
        }
    }

    fn report_mode(&mut self, mode: Mode) {
        let modes = self.term.mode();
        let state = match mode {
            Mode::Named(NamedMode::Insert) => modes.contains(TermMode::INSERT).into(),
            Mode::Named(NamedMode::LineFeedNewLine) => {
                modes.contains(TermMode::LINE_FEED_NEW_LINE).into()
            }
            Mode::Unknown(_) => ModeState::NotRecognized,
        };
        let report = format!("\x1b[{};{}$y", mode.raw(), state as u8);
        self.reply(report.as_bytes());
    }

    fn report_private_mode(&mut self, mode: PrivateMode) {
        let state = self.private_mode_state(mode);
        let report = format!("\x1b[?{};{}$y", mode.raw(), state as u8);
        self.reply(report.as_bytes());
    }

    fn set_cursor_style(&mut self, style: Option<CursorStyle>) {
        self.term.set_cursor_style(style);
    }

    /// Answers a color query (OSC 4/10/11/12 `?`) the way xterm.js does: the
    /// color the program set, else the client's palette; nothing before a
    /// client sent one. The reply always ends with ST.
    fn dynamic_color_sequence(&mut self, prefix: String, index: usize, _terminator: &str) {
        let Some(palette) = self.extras.palette.as_ref() else {
            return;
        };
        let set = self.term.colors()[index].map(|color| [color.r, color.g, color.b]);
        let Some([r, g, b]) = set.or_else(|| palette_color(palette, index)) else {
            return;
        };
        let reply = format!("\x1b]{prefix};rgb:{r:02x}{r:02x}/{g:02x}{g:02x}/{b:02x}{b:02x}\x1b\\");
        self.reply(reply.as_bytes());
    }

    fn set_color(&mut self, index: usize, color: Rgb) {
        if self.spend(1) {
            self.term.set_color(index, color);
        }
    }

    fn reset_color(&mut self, index: usize) {
        if self.spend(1) {
            self.term.reset_color(index);
        }
    }

    fn input(&mut self, c: char) {
        if self.budget == 0 {
            return;
        }
        self.budget -= 1;
        // An insert shifts the rest of the row.
        let insert_mode = self.term.mode().contains(TermMode::INSERT);
        if !self.spend(if insert_mode { self.cols() } else { 1 }) {
            return;
        }
        if c.width() == Some(0) {
            if self.combining_fits() {
                self.term.input(c);
            }
            return;
        }
        self.term.input(c);
        if c == ' ' {
            self.mark_written_blank();
        }
    }

    /// Erases the screen the way the GUI's terminal does. The engine would
    /// push a main screen's rows into its history first, filling it with
    /// every frame a repainting program clears.
    fn clear_screen(&mut self, mode: ClearMode) {
        match mode {
            ClearMode::All => {
                if self.spend(self.area()) {
                    self.term.grid_mut().reset_region(..);
                }
            }
            other => {
                if self.spend(self.area()) {
                    self.term.clear_screen(other);
                }
            }
        }
    }

    fn set_private_mode(&mut self, mode: PrivateMode) {
        if clears_screen(mode) && !self.spend(self.area()) {
            return;
        }
        match mode {
            PrivateMode::Named(NamedPrivateMode::SwapScreenAndSetRestoreCursor) => {
                self.enter_alt_screen(true);
            }
            PrivateMode::Unknown(47 | 1047) => self.enter_alt_screen(false),
            PrivateMode::Unknown(9) => {
                self.extras.x10_mouse = true;
                for protocol in MOUSE_PROTOCOLS {
                    self.term.unset_private_mode(PrivateMode::Named(protocol));
                }
            }
            PrivateMode::Unknown(45) => self.extras.reverse_wrap = true,
            // DECNKM is the keypad mode `ESC =` sets.
            PrivateMode::Unknown(66) => self.term.set_keypad_application_mode(),
            PrivateMode::Named(NamedPrivateMode::SyncUpdate) => {
                let writes = self.writes;
                self.extras.sync.get_or_insert(SyncHold {
                    bytes: 0,
                    writes_at_open: writes,
                    released: false,
                });
            }
            PrivateMode::Named(named) => {
                if MOUSE_PROTOCOLS.contains(&named) {
                    self.extras.x10_mouse = false;
                }
                self.term.set_private_mode(mode);
            }
            PrivateMode::Unknown(_) => self.term.set_private_mode(mode),
        }
    }

    fn unset_private_mode(&mut self, mode: PrivateMode) {
        if clears_screen(mode) && !self.spend(self.area()) {
            return;
        }
        match mode {
            PrivateMode::Named(NamedPrivateMode::SwapScreenAndSetRestoreCursor)
            | PrivateMode::Unknown(47 | 1047) => self.leave_alt_screen(),
            PrivateMode::Unknown(9) => self.extras.x10_mouse = false,
            PrivateMode::Unknown(45) => self.extras.reverse_wrap = false,
            PrivateMode::Unknown(66) => self.term.unset_keypad_application_mode(),
            PrivateMode::Named(NamedPrivateMode::SyncUpdate) => self.extras.sync = None,
            PrivateMode::Named(named) => {
                if MOUSE_PROTOCOLS.contains(&named) {
                    self.extras.x10_mouse = false;
                }
                self.term.unset_private_mode(mode);
            }
            PrivateMode::Unknown(_) => self.term.unset_private_mode(mode),
        }
    }
}

/// The client's color for `index` (256, 257 and 258 are the foreground,
/// background and cursor): its own entry, else xterm's standard value for
/// 16-255.
fn palette_color(palette: &Palette, index: usize) -> Option<[u8; 3]> {
    match index {
        256 => Some(palette.foreground),
        257 => Some(palette.background),
        258 => Some(palette.cursor),
        _ => palette
            .ansi
            .get(index)
            .copied()
            .or_else(|| standard_color(index)),
    }
}

/// xterm's color `index` of the 6x6x6 cube (16-231) or the gray ramp
/// (232-255).
fn standard_color(index: usize) -> Option<[u8; 3]> {
    let level = |step: usize| u8::try_from(if step == 0 { 0 } else { 55 + step * 40 }).ok();
    match index {
        16..=231 => {
            let cube = index - 16;
            Some([level(cube / 36)?, level(cube / 6 % 6)?, level(cube % 6)?])
        }
        232..=255 => {
            let gray = u8::try_from(8 + (index - 232) * 10).ok()?;
            Some([gray; 3])
        }
        _ => None,
    }
}

/// Whether switching this private mode rewrites the screen: the 80/132 column
/// switch clears it, and the alternate screens swap whole grids.
fn clears_screen(mode: PrivateMode) -> bool {
    matches!(
        mode,
        PrivateMode::Named(
            NamedPrivateMode::ColumnMode | NamedPrivateMode::SwapScreenAndSetRestoreCursor
        ) | PrivateMode::Unknown(47 | 1047)
    )
}

/// One window's terminal: the engine, the parser around it, and the bounds.
pub(super) struct Model {
    term: Term<VoidListener>,
    processor: Processor<NoSyncTimeout>,
    osc: OscLimiter,
    extras: Extras,
    /// The filtered bytes of the feed in progress, reused between feeds.
    filtered: Vec<u8>,
}

impl Model {
    /// A blank model of an already clamped size (see [`clamp_size`]).
    pub(super) fn new(cols: u16, rows: u16, history_rows: usize) -> Self {
        let config = Config {
            scrolling_history: history_rows,
            ..Config::default()
        };
        let size = Size {
            cols: usize::from(cols),
            rows: usize::from(rows),
        };
        Self {
            term: Term::new(config, &size, VoidListener),
            processor: Processor::new(),
            osc: OscLimiter::default(),
            extras: Extras::new(history_rows),
            filtered: Vec::new(),
        }
    }

    pub(super) fn term(&self) -> &Term<VoidListener> {
        &self.term
    }

    pub(super) fn term_mut(&mut self) -> &mut Term<VoidListener> {
        &mut self.term
    }

    /// Keeps `palette` as the client's theme colors (see
    /// [`Extras::palette`]), at most [`MAX_PALETTE_ENTRIES`] of them.
    pub(super) fn set_palette(&mut self, mut palette: Palette) {
        palette.ansi.truncate(MAX_PALETTE_ENTRIES);
        self.extras.palette = Some(palette);
    }

    /// Whether a synchronized update holds the view back: it is open, no
    /// input was written since (the window's count is `writes`), no resize
    /// released it, and it has not grown past its byte bound.
    pub(super) fn frame_held(&self, writes: u64) -> bool {
        let area = self.term.columns() * self.term.screen_lines();
        self.extras.sync.is_some_and(|hold| {
            !hold.released
                && hold.writes_at_open == writes
                && hold.bytes <= area.saturating_mul(SYNC_HOLD_BYTES_PER_CELL)
        })
    }

    /// Whether the program has a synchronized update open, whatever has
    /// released the view's hold on it.
    pub(super) fn sync_open(&self) -> bool {
        self.extras.sync.is_some()
    }

    pub(super) fn extras(&self) -> &Extras {
        &self.extras
    }

    /// Takes the replies queued for the child so far, and how many were
    /// dropped because the queue was full.
    pub(super) fn take_replies(&mut self) -> (Vec<u8>, usize) {
        let replies = std::mem::take(&mut self.extras.replies);
        let dropped = std::mem::take(&mut self.extras.dropped_replies);
        (replies, dropped)
    }

    /// Applies `bytes` to the terminal. One call writes at most the number of
    /// bytes plus two screens of characters, and rewrites at most a row of
    /// cells per byte plus four screens: an honest stream never reaches
    /// either, and a few bytes can not make one call do unbounded work.
    /// `writes` is the window's count of input writes, recorded by a
    /// synchronized update this feed opens.
    pub(super) fn feed(&mut self, bytes: &[u8], writes: u64) {
        let budget = bytes
            .len()
            .saturating_add(2 * self.term.columns() * self.term.screen_lines());
        let cols = self.term.columns();
        let work = bytes
            .len()
            .saturating_mul(cols)
            .saturating_add(4 * cols * self.term.screen_lines());
        self.filtered.clear();
        self.osc.filter(bytes, &mut self.filtered);
        let mut handler = BoundedHandler {
            term: &mut self.term,
            writes,
            budget,
            work,
            extras: &mut self.extras,
        };
        self.processor.advance(&mut handler, &self.filtered);
        if let Some(hold) = self.extras.sync.as_mut() {
            hold.bytes = hold.bytes.saturating_add(bytes.len());
        }
    }

    /// Resizes to an already clamped size (see [`clamp_size`]): the cursor
    /// stays in view and rows move between history and screen, as a Unix
    /// pseudo terminal's terminal does.
    pub(super) fn resize(&mut self, cols: u16, rows: u16) {
        let (cols, rows) = (usize::from(cols), usize::from(rows));
        if (cols, rows) == (self.term.columns(), self.term.screen_lines()) {
            return;
        }
        self.term.resize(Size { cols, rows });
        if let Some(main) = self.extras.parked_main.as_mut() {
            main.resize(true, rows, cols);
        }
        if let Some(hold) = self.extras.sync.as_mut() {
            hold.released = true;
        }
    }
}
