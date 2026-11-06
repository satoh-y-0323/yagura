//! The view layer: what one client is shown of a window's model, sent as
//! frames of cells ([`Frame`]). The client draws the cells and interprets
//! nothing, so the daemon's model is the window's only terminal.
//!
//! A [`View`] keeps the rows it last sent and sends a row again only when it
//! changed, one frame at a time: the next frame waits for the client's
//! acknowledgement of the last one and is then built from the newest state.
//! Where the view looks into the history is the engine's display offset, so
//! the engine keeps it on the same rows while output arrives.

use std::collections::HashMap;

use alacritty_terminal::grid::{Dimensions, Row, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionRange, SelectionType};
use alacritty_terminal::term::TermMode;
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::vte::ansi::{Color, CursorShape as EngineCursorShape};
use yagura_protocol::{
    CellAttr, CellColor, CellFlag, ColorOverride, CursorShape, CursorState, DaemonMessage,
    FRAME_BUDGET_BYTES, Frame, FrameLine, FrameRun, PaletteSlot, PointerButton, PointerInput,
    PointerKind, ViewState,
};

use super::screen::VtScreen;
use super::screen_model::Model;

/// The cell flags a frame shows. The model's own marks (a written blank)
/// are not among them, so they never make a row look changed.
const SHOWN_FLAGS: Flags = Flags::INVERSE
    .union(Flags::BOLD)
    .union(Flags::ITALIC)
    .union(Flags::ALL_UNDERLINES)
    .union(Flags::WRAPLINE)
    .union(Flags::WIDE_CHAR)
    .union(Flags::WIDE_CHAR_SPACER)
    .union(Flags::DIM)
    .union(Flags::HIDDEN)
    .union(Flags::STRIKEOUT)
    .union(Flags::LEADING_WIDE_CHAR_SPACER);

/// The highest palette slot a program can set: the cursor color (OSC 12).
const LAST_COLOR_SLOT: usize = 258;

/// One cell as a frame shows it.
#[derive(Debug, Clone, PartialEq)]
struct ShownCell {
    c: char,
    marks: Option<Box<[char]>>,
    fg: Color,
    bg: Color,
    flags: Flags,
}

impl ShownCell {
    fn of(cell: &Cell) -> Self {
        Self {
            c: shown_char(cell.c),
            marks: cell
                .zerowidth()
                .filter(|marks| !marks.is_empty())
                .map(Box::from),
            fg: cell.fg,
            bg: cell.bg,
            flags: cell.flags & SHOWN_FLAGS,
        }
    }

    /// Whether `cell` shows as this one, without building it.
    fn shows_as(&self, cell: &Cell) -> bool {
        self.c == shown_char(cell.c)
            && self.fg == cell.fg
            && self.bg == cell.bg
            && self.flags == cell.flags & SHOWN_FLAGS
            && self.marks.as_deref().unwrap_or_default() == cell.zerowidth().unwrap_or_default()
    }
}

/// A tab's cell and a never written one read as a blank.
fn shown_char(c: char) -> char {
    if matches!(c, '\t' | '\0') { ' ' } else { c }
}

/// One row of the view as a frame shows it.
#[derive(Debug, Clone, PartialEq)]
struct ShownRow {
    cells: Vec<ShownCell>,
    /// The selected columns, first and last inclusive.
    selected: Option<(usize, usize)>,
}

impl ShownRow {
    fn of(row: &Row<Cell>, cols: usize, selected: Option<(usize, usize)>) -> Self {
        Self {
            cells: (0..cols.min(row.len()))
                .map(|col| ShownCell::of(&row[Column(col)]))
                .collect(),
            selected,
        }
    }

    fn shows_as(&self, row: &Row<Cell>, cols: usize, selected: Option<(usize, usize)>) -> bool {
        self.selected == selected
            && self.cells.len() == cols.min(row.len())
            && self
                .cells
                .iter()
                .enumerate()
                .all(|(col, cell)| cell.shows_as(&row[Column(col)]))
    }

    fn wrapped(&self) -> bool {
        self.cells
            .last()
            .is_some_and(|cell| cell.flags.contains(Flags::WRAPLINE))
    }

    fn is_blank(&self) -> bool {
        !self.wrapped()
            && self.selected.is_none()
            && self.cells.iter().all(|cell| {
                cell_attr(cell, false) == CellAttr::default()
                    && cell.c == ' '
                    && cell.marks.is_none()
                    && !cell.flags.contains(Flags::WIDE_CHAR)
            })
    }
}

/// What a view last sent.
struct Sent {
    cols: u16,
    rows: u16,
    generation: u64,
    shown: Vec<ShownRow>,
    cursor: CursorState,
    view: ViewState,
    overrides: Vec<ColorOverride>,
}

/// One frame read off the model, before it is laid out as wire parts by
/// [`FrameCapture::into_parts`] (away from the model's lock).
pub struct FrameCapture {
    seq: u64,
    cols: u16,
    rows: u16,
    reset: bool,
    lines: Vec<(u16, ShownRow)>,
    cursor: CursorState,
    view: ViewState,
    overrides: Vec<ColorOverride>,
}

/// One client's view of a window: what it was sent and whether it has
/// acknowledged it. Holds one screen of rows.
#[derive(Default)]
pub struct View {
    seq: u64,
    sent: Option<Sent>,
    /// The frame sent and not yet acknowledged.
    awaiting: Option<u64>,
    /// Something may have changed since the last frame.
    dirty: bool,
    /// The last attempt found a synchronized update holding the view.
    held: bool,
    /// The client reported the view as not on screen.
    hidden: bool,
}

impl View {
    /// A view that has sent nothing yet.
    #[must_use]
    pub fn new() -> Self {
        Self {
            dirty: true,
            ..Self::default()
        }
    }

    /// Something the view shows may have changed.
    pub fn note_change(&mut self) {
        self.dirty = true;
    }

    /// The client shows the view or no longer does. A hidden view wants no
    /// frame; showing it again sends every row, since what the client holds
    /// may be arbitrarily stale.
    pub fn set_visible(&mut self, visible: bool) {
        if self.hidden != visible {
            return;
        }
        self.hidden = !visible;
        if visible {
            self.sent = None;
            self.dirty = true;
        }
    }

    /// The client applied frame `seq`. An acknowledgement of any other frame
    /// changes nothing.
    pub fn acknowledge(&mut self, seq: u64) {
        if self.awaiting == Some(seq) {
            self.awaiting = None;
        }
    }

    /// Whether a frame may be due: the view is shown, something changed and
    /// nothing is awaiting its acknowledgement.
    #[must_use]
    pub fn wants_frame(&self) -> bool {
        !self.hidden && self.dirty && self.awaiting.is_none()
    }

    /// Whether the last attempt found the view held by a synchronized update.
    #[must_use]
    pub fn is_held(&self) -> bool {
        self.held
    }

    /// Reads the next frame off `screen`, or `None` when none is to be sent:
    /// a frame awaits its acknowledgement, a synchronized update holds the
    /// view (unless the child has exited), or nothing changed. `full` (an
    /// attach) sends every row whatever is awaited or held, and forgets
    /// what was sent before.
    pub fn capture(
        &mut self,
        screen: &mut VtScreen,
        child_exited: bool,
        full: bool,
    ) -> Option<FrameCapture> {
        if full {
            self.sent = None;
            self.awaiting = None;
        } else if self.awaiting.is_some() {
            return None;
        }
        // A held view waits for what can release it (output, a write, a
        // resize, the child's exit), each of which notes a change again.
        self.held = !full && !child_exited && screen.frame_held();
        if self.held {
            self.dirty = false;
            return None;
        }
        self.dirty = false;
        let generation = screen.generation();
        let read = screen.read_guarded(|screen| self.read(screen.model(), generation));
        match read {
            Some(capture) => capture,
            // The model was rebuilt blank; the next attempt sends it whole.
            None => {
                self.sent = None;
                self.dirty = true;
                None
            }
        }
    }

    fn read(&mut self, model: &Model, generation: u64) -> Option<FrameCapture> {
        let term = model.term();
        let grid = term.grid();
        let cols = grid.columns();
        let rows = grid.screen_lines();
        let (cols_u16, rows_u16) = (
            u16::try_from(cols).unwrap_or(u16::MAX),
            u16::try_from(rows).unwrap_or(u16::MAX),
        );
        let offset = grid.display_offset();
        let selection = term.selection.as_ref().and_then(|s| s.to_range(term));

        let mut sent = self.sent.take().filter(|sent| {
            (sent.cols, sent.rows, sent.generation) == (cols_u16, rows_u16, generation)
                && sent.shown.len() == rows
        });
        let reset = sent.is_none();
        let mut shown = sent
            .as_mut()
            .map(|sent| std::mem::take(&mut sent.shown))
            .unwrap_or_default();
        let mut lines = Vec::new();
        for row in 0..rows {
            let line = Line(i32::try_from(row).unwrap_or(i32::MAX) - offset_i32(offset));
            let selected = selection
                .as_ref()
                .and_then(|range| selected_columns(range, line, cols));
            let engine_row = &grid[line];
            if !reset
                && shown
                    .get(row)
                    .is_some_and(|old| old.shows_as(engine_row, cols, selected))
            {
                continue;
            }
            let new = ShownRow::of(engine_row, cols, selected);
            if !(reset && new.is_blank()) {
                lines.push((u16::try_from(row).unwrap_or(u16::MAX), new.clone()));
            }
            match shown.get_mut(row) {
                Some(slot) => *slot = new,
                None => shown.push(new),
            }
        }

        let cursor = cursor_state(model);
        let view = view_state(model);
        let overrides = overrides(model);
        let unchanged = sent.as_ref().is_some_and(|sent| {
            lines.is_empty()
                && sent.cursor == cursor
                && sent.view == view
                && sent.overrides == overrides
        });
        self.sent = Some(Sent {
            cols: cols_u16,
            rows: rows_u16,
            generation,
            shown,
            cursor,
            view,
            overrides: overrides.clone(),
        });
        if unchanged {
            return None;
        }
        self.seq += 1;
        self.awaiting = Some(self.seq);
        Some(FrameCapture {
            seq: self.seq,
            cols: cols_u16,
            rows: rows_u16,
            reset,
            lines,
            cursor,
            view,
            overrides,
        })
    }
}

fn offset_i32(offset: usize) -> i32 {
    i32::try_from(offset).unwrap_or(i32::MAX)
}

/// The columns of `line` inside `range`, first and last inclusive.
fn selected_columns(range: &SelectionRange, line: Line, cols: usize) -> Option<(usize, usize)> {
    if cols == 0 || line < range.start.line || line > range.end.line {
        return None;
    }
    let (first, last) = if range.is_block {
        (range.start.column.0, range.end.column.0)
    } else {
        (
            if line == range.start.line {
                range.start.column.0
            } else {
                0
            },
            if line == range.end.line {
                range.end.column.0
            } else {
                cols - 1
            },
        )
    };
    let last = last.min(cols - 1);
    (first <= last).then_some((first, last))
}

fn cursor_state(model: &Model) -> CursorState {
    let term = model.term();
    let cursor = &term.grid().cursor;
    CursorState {
        row: u16::try_from(cursor.point.line.0).unwrap_or(0),
        col: u16::try_from(cursor.point.column.0 + usize::from(cursor.input_needs_wrap))
            .unwrap_or(u16::MAX),
        visible: term.mode().contains(TermMode::SHOW_CURSOR),
        shape: match term.cursor_style().shape {
            EngineCursorShape::Underline => CursorShape::Underline,
            EngineCursorShape::Beam => CursorShape::Bar,
            EngineCursorShape::HollowBlock => CursorShape::HollowBlock,
            EngineCursorShape::Block | EngineCursorShape::Hidden => CursorShape::Block,
        },
    }
}

fn view_state(model: &Model) -> ViewState {
    let term = model.term();
    let grid = term.grid();
    let mode = term.mode();
    ViewState {
        display_offset: u32::try_from(grid.display_offset()).unwrap_or(u32::MAX),
        history_rows: u32::try_from(grid.history_size()).unwrap_or(u32::MAX),
        mouse_reporting: mode.intersects(
            TermMode::MOUSE_REPORT_CLICK | TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION,
        ) || model.extras().x10_mouse,
        alt_screen: mode.contains(TermMode::ALT_SCREEN),
    }
}

/// Every color the program set over the client's palette.
fn overrides(model: &Model) -> Vec<ColorOverride> {
    let colors = model.term().colors();
    (0..=LAST_COLOR_SLOT)
        .filter_map(|index| {
            let rgb = colors[index]?;
            let slot = match index {
                256 => PaletteSlot::Foreground,
                257 => PaletteSlot::Background,
                258 => PaletteSlot::Cursor,
                _ => PaletteSlot::Indexed(u8::try_from(index).ok()?),
            };
            Some(ColorOverride {
                slot,
                rgb: [rgb.r, rgb.g, rgb.b],
            })
        })
        .collect()
}

fn cell_color(color: Color) -> CellColor {
    match color {
        Color::Named(named) => u8::try_from(named as usize)
            .ok()
            .filter(|index| *index < 16)
            .map_or(CellColor::Default, CellColor::Indexed),
        Color::Spec(rgb) => CellColor::Rgb([rgb.r, rgb.g, rgb.b]),
        Color::Indexed(index) => CellColor::Indexed(index),
    }
}

fn cell_attr(cell: &ShownCell, selected: bool) -> CellAttr {
    let flags = cell.flags;
    let mut shown = std::collections::BTreeSet::new();
    for (flag, mark) in [
        (Flags::BOLD, CellFlag::Bold),
        (Flags::DIM, CellFlag::Dim),
        (Flags::ITALIC, CellFlag::Italic),
        (Flags::STRIKEOUT, CellFlag::Strikeout),
        (Flags::INVERSE, CellFlag::Inverse),
        (Flags::HIDDEN, CellFlag::Hidden),
    ] {
        if flags.contains(flag) {
            shown.insert(mark);
        }
    }
    if flags.intersects(Flags::ALL_UNDERLINES) {
        shown.insert(CellFlag::Underline);
    }
    if selected {
        shown.insert(CellFlag::Selected);
    }
    CellAttr {
        fg: cell_color(cell.fg),
        bg: cell_color(cell.bg),
        flags: shown,
    }
}

/// A run with its attribute itself, before a part's table numbers it.
struct AttrRun {
    attr: CellAttr,
    cells: Vec<String>,
    wide: Vec<u16>,
}

/// The runs of `row`: a wide cell once (its spacer left out), trailing
/// blanks with the default attribute left out.
fn row_runs(row: &ShownRow) -> Vec<AttrRun> {
    let mut cells: Vec<(CellAttr, String, bool)> = Vec::with_capacity(row.cells.len());
    for (col, cell) in row.cells.iter().enumerate() {
        if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
            continue;
        }
        let leading = cell.flags.contains(Flags::LEADING_WIDE_CHAR_SPACER);
        let mut text = String::new();
        if leading {
            text.push(' ');
        } else {
            text.push(cell.c);
            text.extend(cell.marks.iter().flat_map(|marks| marks.iter()));
        }
        let selected = row
            .selected
            .is_some_and(|(first, last)| (first..=last).contains(&col));
        let wide = cell.flags.contains(Flags::WIDE_CHAR) && !leading;
        cells.push((cell_attr(cell, selected), text, wide));
    }
    while cells
        .last()
        .is_some_and(|(attr, text, wide)| *attr == CellAttr::default() && text == " " && !wide)
    {
        cells.pop();
    }
    let mut runs: Vec<AttrRun> = Vec::new();
    for (attr, text, wide) in cells {
        if runs.last().is_none_or(|run| run.attr != attr) {
            runs.push(AttrRun {
                attr,
                cells: Vec::new(),
                wide: Vec::new(),
            });
        }
        let Some(run) = runs.last_mut() else {
            continue;
        };
        if wide {
            run.wide
                .push(u16::try_from(run.cells.len()).unwrap_or(u16::MAX));
        }
        run.cells.push(text);
    }
    runs
}

/// Counts the bytes written to it.
struct ByteCount(usize);

impl std::io::Write for ByteCount {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0 = self.0.saturating_add(buf.len());
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The JSON length of `value`; a value that does not serialize counts as
/// over any budget.
fn json_len(value: &impl serde::Serialize) -> usize {
    let mut count = ByteCount(0);
    match serde_json::to_writer(&mut count, value) {
        Ok(()) => count.0,
        Err(_) => usize::MAX,
    }
}

/// The part being filled: its attribute table and lines, and an upper bound
/// of its encoded size.
struct PartFill {
    attrs: Vec<CellAttr>,
    index: HashMap<CellAttr, u16>,
    lines: Vec<FrameLine>,
    bytes: usize,
}

impl PartFill {
    fn new(envelope: usize) -> Self {
        Self {
            attrs: Vec::new(),
            index: HashMap::new(),
            lines: Vec::new(),
            bytes: envelope,
        }
    }

    /// Adds `row`'s line, numbering its attributes in this part's table.
    /// Leaves the part as it was and returns `false` when the line does not
    /// fit the budget or the table.
    fn try_add(&mut self, row: u16, wrapped: bool, runs: &[AttrRun]) -> bool {
        let table_before = self.attrs.len();
        let mut added = 0usize;
        let mut frame_runs = Vec::with_capacity(runs.len());
        for run in runs {
            let attr = match self.index.get(&run.attr) {
                Some(index) => *index,
                None => {
                    let Ok(index) = u16::try_from(self.attrs.len()) else {
                        self.roll_back(table_before);
                        return false;
                    };
                    added = added.saturating_add(json_len(&run.attr).saturating_add(1));
                    self.attrs.push(run.attr.clone());
                    self.index.insert(run.attr.clone(), index);
                    index
                }
            };
            frame_runs.push(FrameRun {
                attr,
                cells: run.cells.clone(),
                wide: run.wide.clone(),
            });
        }
        let line = FrameLine {
            row,
            wrapped,
            runs: frame_runs,
        };
        let bytes = self
            .bytes
            .saturating_add(added)
            .saturating_add(json_len(&line).saturating_add(1));
        if bytes > FRAME_BUDGET_BYTES && !self.lines.is_empty() {
            self.roll_back(table_before);
            return false;
        }
        self.bytes = bytes;
        self.lines.push(line);
        true
    }

    fn roll_back(&mut self, table_len: usize) {
        for attr in self.attrs.drain(table_len..) {
            self.index.remove(&attr);
        }
    }
}

impl FrameCapture {
    /// The wire parts of this frame, each within [`FRAME_BUDGET_BYTES`]
    /// once encoded: rows split by range, only the first part resetting
    /// the grid, only the last marked `last`.
    #[must_use]
    pub fn into_parts(self) -> Vec<Frame> {
        let part = |reset: bool, last: bool, attrs: Vec<CellAttr>, lines: Vec<FrameLine>| Frame {
            seq: self.seq,
            cols: self.cols,
            rows: self.rows,
            reset,
            last,
            attrs,
            lines,
            cursor: self.cursor,
            view: self.view,
            overrides: self.overrides.clone(),
        };
        let envelope = json_len(&DaemonMessage::Frame(part(
            false,
            false,
            Vec::new(),
            Vec::new(),
        )));
        let mut parts: Vec<Frame> = Vec::new();
        let mut fill = PartFill::new(envelope);
        for (row, shown) in &self.lines {
            let runs = row_runs(shown);
            if !fill.try_add(*row, shown.wrapped(), &runs) {
                let full = std::mem::replace(&mut fill, PartFill::new(envelope));
                parts.push(part(
                    self.reset && parts.is_empty(),
                    false,
                    full.attrs,
                    full.lines,
                ));
                fill.try_add(*row, shown.wrapped(), &runs);
            }
        }
        parts.push(part(
            self.reset && parts.is_empty(),
            true,
            fill.attrs,
            fill.lines,
        ));
        parts
    }
}

/// Moves the view `lines` rows into the history (negative: back toward the
/// live screen), within the history.
pub fn scroll_view(screen: &mut VtScreen, lines: i32) {
    screen.update_model(|model| {
        let term = model.term_mut();
        let grid = term.grid();
        let offset = i64::try_from(grid.display_offset()).unwrap_or(i64::MAX);
        let history = i64::try_from(grid.history_size()).unwrap_or(i64::MAX);
        let target = offset.saturating_add(i64::from(lines)).clamp(0, history);
        if let Ok(delta) = i32::try_from(target - offset)
            && delta != 0
        {
            term.scroll_display(Scroll::Delta(delta));
        }
    });
}

/// Brings the view back to the live screen; whether it was scrolled back.
pub fn scroll_to_bottom(screen: &mut VtScreen) -> bool {
    let mut moved = false;
    screen.update_model(|model| {
        let term = model.term_mut();
        moved = term.grid().display_offset() != 0;
        if moved {
            term.scroll_display(Scroll::Bottom);
        }
    });
    moved
}

/// Moves the view one screen up into the history or back down.
pub fn page_view(screen: &mut VtScreen, up: bool) {
    screen.update_model(|model| {
        model
            .term_mut()
            .scroll_display(if up { Scroll::PageUp } else { Scroll::PageDown });
    });
}

/// Applies a pointer event that selects (not one reported to the child):
/// a press starts a selection (one click by cell, two by word, three by
/// line), a move with the left button held extends it to the cell's far
/// side. The cell is on the screen as the view shows it.
pub fn select(screen: &mut VtScreen, pointer: &PointerInput) {
    if pointer.button != PointerButton::Left {
        return;
    }
    screen.update_model(|model| {
        let term = model.term_mut();
        let offset = offset_i32(term.grid().display_offset());
        let point = Point::new(
            Line(i32::from(pointer.cell.row) - offset),
            Column(usize::from(pointer.cell.col)),
        );
        match pointer.kind {
            PointerKind::Down => {
                let ty = match pointer.clicks {
                    0 | 1 => SelectionType::Simple,
                    2 => SelectionType::Semantic,
                    _ => SelectionType::Lines,
                };
                term.selection = Some(Selection::new(ty, point, Side::Left));
            }
            PointerKind::Move => {
                if let Some(selection) = term.selection.as_mut() {
                    selection.update(point, Side::Right);
                }
            }
            PointerKind::Up => {}
        }
    });
}

/// Drops the selection.
pub fn clear_selection(screen: &mut VtScreen) {
    screen.update_model(|model| {
        model.term_mut().selection = None;
    });
}

/// The selected text: wrapped rows joined, rows separated by a line feed,
/// trailing blanks left out. `None` when nothing is selected.
pub fn selected_text(screen: &mut VtScreen) -> Option<String> {
    screen
        .read_guarded(|screen| screen.model().term().selection_to_string())
        .flatten()
}
