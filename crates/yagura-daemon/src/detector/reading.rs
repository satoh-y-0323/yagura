//! The detector's reading of a window's screen. Pattern definitions are
//! written against display text rather than raw byte streams or ANSI-stripped
//! text, because full-screen TUI agents rewrite the screen and move the cursor
//! to redraw spinners; only a real VT parser reproduces that faithfully.
//!
//! The parser runs in the terminal model; what is read here is the model's
//! copy ([`ScreenCopy`]), which the window's holder sends, so every read is a
//! pure function of a copy and the definitions' policy. [`HeldCopy`] checks
//! and applies the copies a holder sends, and [`SharedCopy`] is the one a
//! window's readers share.

use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

use yagura_protocol::{
    MAX_ZEROWIDTH_PER_CELL, ScreenCopy, ScreenLine, ScreenPush, ScreenRows,
    window_size_within_limit,
};

use crate::detector::definition::ComposerSection;

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

/// Where the upward walk from the cursor ended (see [`walk_to_marker`]).
enum ComposerWalk {
    /// On the marker row.
    Marker(u16),
    /// Off the top, every row on the way a continuation row.
    ReachedTop,
    /// On a row that is neither a continuation row nor the marker.
    Absent,
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
/// more entries than the copy's row count, even if `n` exceeds it.
#[must_use]
pub fn bottom_lines(copy: &ScreenCopy, n: usize) -> Vec<String> {
    window_of(copy, screen_rows(copy), n)
}

/// [`bottom_lines`] over the screen with the composer's body taken out
/// first, so a rule sees the CLI's drawing and not the text being typed.
///
/// The marker row is cut after the marker and its separator (after the
/// marker alone when there is no separator), and the continuation rows
/// down to the cursor row are dropped, closing the gap. The window is
/// taken after that, so a long body cannot push the CLI's own lines out
/// of it. Nothing is taken out when the cursor is hidden or the composer
/// cannot be located: the screen then cannot be told apart from a dialog
/// drawn over the composer's shape.
///
/// The composer is located from the same column as in [`read_composer`],
/// and a left panel's part of the marker row is kept for the rules.
#[must_use]
pub fn bottom_lines_without_composer(
    copy: &ScreenCopy,
    n: usize,
    section: &ComposerSection,
) -> Vec<String> {
    window_of(copy, lines_without_composer(copy, section), n)
}

/// Every row of the screen, top-to-bottom, with the composer's body taken
/// out the way [`bottom_lines_without_composer`] takes it out, before any
/// window is cut.
#[must_use]
pub(crate) fn lines_without_composer(copy: &ScreenCopy, section: &ComposerSection) -> Vec<String> {
    let mut lines = screen_rows(copy);
    let origin = composer_origin(copy, section);
    if !copy.cursor_hidden
        && let ComposerWalk::Marker(marker_row) = walk_to_marker(copy, section, origin)
    {
        let cursor_row = cursor_position(copy).0;
        let marker_start = if origin == 0 {
            Some(0)
        } else {
            row_text_columns(copy, marker_row)
                .and_then(|(_, columns)| byte_at_column(&columns, origin))
        };
        let (marker_row, cursor_row) = (usize::from(marker_row), usize::from(cursor_row));
        if let Some(line) = lines.get_mut(marker_row)
            && let Some(start) = marker_start
            && let Some(marker_match) = line.get(start..).and_then(|from| section.marker.find(from))
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

/// Every row of the screen, top-to-bottom, as [`bottom_lines`] reads them
/// before cutting its window.
#[must_use]
pub(crate) fn all_lines(copy: &ScreenCopy) -> Vec<String> {
    screen_rows(copy)
}

/// The window [`bottom_lines`] cuts from `lines`, copied out so the caller
/// keeps `lines` whole.
#[must_use]
pub(crate) fn bottom_window(copy: &ScreenCopy, lines: &[String], n: usize) -> Vec<String> {
    lines[window_range(copy, lines, n)].to_vec()
}

/// The same window as [`bottom_window`], cut from `lines` in place.
#[must_use]
pub(crate) fn window_of(copy: &ScreenCopy, mut lines: Vec<String>, n: usize) -> Vec<String> {
    let range = window_range(copy, &lines, n);
    lines.truncate(range.end);
    lines.drain(..range.start);
    lines
}

fn window_range(copy: &ScreenCopy, lines: &[String], n: usize) -> std::ops::Range<usize> {
    let take = n.min(usize::from(copy.rows));
    let last_written = lines
        .iter()
        .rposition(|line| line.chars().any(|c| !c.is_whitespace()))
        .unwrap_or(0);
    let skip = (last_written + 1).saturating_sub(take);
    skip.min(lines.len())..(skip + take).min(lines.len())
}

/// Returns the row index (0-based) where the cursor is currently positioned.
#[must_use]
pub fn cursor_row(copy: &ScreenCopy) -> u16 {
    cursor_position(copy).0
}

/// The cursor as `(row, col)`. A cursor parked after the last column,
/// waiting to wrap, is at column `cols`: that is where the next character
/// would go, and where a reader of the row sees it.
fn cursor_position(copy: &ScreenCopy) -> (u16, u16) {
    let cursor = copy.cursor;
    (
        cursor.line,
        cursor
            .col
            .saturating_add(u16::from(cursor.input_needs_wrap)),
    )
}

/// The text the screen shows, in logical lines: a row the terminal wrapped
/// ([`ScreenLine::wraps`]) joins the next one, so only a line break the
/// program wrote ends a line. The spaces after a line's last character and
/// the blank rows at the bottom are left out. This is the holder's reading of
/// its history, applied to the visible screen alone.
#[must_use]
pub fn screen_text(copy: &ScreenCopy) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();
    for row in &copy.lines {
        line.push_str(&cells_text(row, 0, usize::from(copy.cols), false).0);
        if !row.wraps {
            lines.push(line.trim_end_matches(' ').to_owned());
            line.clear();
        }
    }
    if !line.is_empty() {
        lines.push(line.trim_end_matches(' ').to_owned());
    }
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    lines.join("\n")
}

/// Every screen row as text, covering all contents (dimmed ones too).
fn screen_rows(copy: &ScreenCopy) -> Vec<String> {
    copy.lines
        .iter()
        .map(|line| cells_text(line, 0, usize::from(copy.cols), false).0)
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
/// [`visible_text`]).
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
#[must_use]
pub fn read_composer(copy: &ScreenCopy, section: &ComposerSection) -> ComposerReadout {
    let origin = composer_origin(copy, section);
    let (cursor, cursor_col) = cursor_position(copy);
    let cursor_col = cursor_col.min(copy.cols);
    let cursor_end = composer_row_end(copy, section, cursor, origin);
    let cursor_line = text_left_of_cursor(copy, cursor, origin, cursor_col.min(cursor_end));

    let marker_row = match walk_to_marker(copy, section, origin) {
        ComposerWalk::Marker(row) => Some(row),
        ComposerWalk::ReachedTop => None,
        ComposerWalk::Absent => return ComposerReadout::NotObserved,
    };

    // Text the CLI left to the right of the insertion point is either its
    // own drawing (dimmed, and already dropped) or something this section
    // cannot name: refuse to judge rather than fold it into the body or
    // call the composer empty.
    let right_of_cursor = visible_text(copy, cursor, cursor_col, cursor_end);
    if !right_of_cursor.trim().is_empty() {
        let untyped_hint =
            marker_row == Some(cursor) && is_untyped_hint(section, &cursor_line, &right_of_cursor);
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
            visible_text(
                copy,
                row,
                origin,
                composer_row_end(copy, section, row, origin),
            )
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
/// Rows are read from the same origin as in [`read_composer`], and a cursor
/// left of it locates nothing.
#[must_use]
pub fn locate_composer(copy: &ScreenCopy, section: &ComposerSection) -> Option<(u16, u16)> {
    match walk_to_marker(copy, section, composer_origin(copy, section)) {
        ComposerWalk::Marker(row) => Some((row, cursor_position(copy).0)),
        ComposerWalk::ReachedTop | ComposerWalk::Absent => None,
    }
}

/// The column the composer is read from (see [`read_composer`]). The panel
/// is matched against each row from column 0, dimmed cells included, and its
/// end is counted in cells: a wide character takes two columns, and a match
/// ending inside a cell (before its combining marks) has no column, so the
/// origin falls back to 0.
fn composer_origin(copy: &ScreenCopy, section: &ComposerSection) -> u16 {
    let Some(panel) = &section.left_panel else {
        return 0;
    };
    let mut origin = None;
    for row in 0..copy.rows {
        let Some((text, columns)) = row_text_columns(copy, row) else {
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

/// The upward walk behind [`locate_composer`], telling a walk that ran off
/// the top through continuation rows from one that stopped on a row that is
/// neither a continuation nor the marker. Rows are read from column
/// `origin`.
fn walk_to_marker(copy: &ScreenCopy, section: &ComposerSection, origin: u16) -> ComposerWalk {
    let cols = copy.cols;
    let (cursor, cursor_col) = cursor_position(copy);
    let cursor_col = cursor_col.min(cols);
    if cursor_col < origin {
        return ComposerWalk::Absent;
    }
    let cursor_line = text_left_of_cursor(copy, cursor, origin, cursor_col);
    let crosses_blank_rows = section.blank_lines_in_body && !copy.cursor_hidden;
    let mut row = cursor;
    loop {
        let line = if row == cursor {
            cursor_line.clone()
        } else {
            match row_text(copy, row, origin, cols, false) {
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
fn visible_text(copy: &ScreenCopy, row: u16, start: u16, end: u16) -> String {
    row_text(copy, row, start, end, true)
        .map(|(text, _)| text)
        .unwrap_or_default()
}

/// The column a composer row read from `start` ends at: where the section's
/// `right_border` match ending at the row's last visible character begins,
/// else the screen's width.
fn composer_row_end(copy: &ScreenCopy, section: &ComposerSection, row: u16, start: u16) -> u16 {
    let cols = copy.cols;
    let Some(border) = &section.right_border else {
        return cols;
    };
    let Some(line) = copy.lines.get(usize::from(row)) else {
        return cols;
    };
    let mut columns = Vec::new();
    let (text, _) = cells_text_mapped(
        line,
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

/// The cursor row from column `start` up to the cursor's column, with every
/// column the cursor has moved past read as blank even if nothing was
/// written there: a terminal may draw a space by moving the cursor alone
/// (ConPTY does after a PowerShell prompt), and a reader sees it as one.
fn text_left_of_cursor(copy: &ScreenCopy, cursor_row: u16, start: u16, cursor_col: u16) -> String {
    let (mut text, next_col) =
        row_text(copy, cursor_row, start, cursor_col, true).unwrap_or_default();
    for _ in next_col..cursor_col {
        text.push(' ');
    }
    text
}

/// [`cells_text`] of a screen row, or `None` below the last row.
fn row_text(
    copy: &ScreenCopy,
    row: u16,
    start: u16,
    end: u16,
    skip_dim: bool,
) -> Option<(String, u16)> {
    let line = copy.lines.get(usize::from(row))?;
    Some(cells_text(
        line,
        usize::from(start),
        usize::from(end),
        skip_dim,
    ))
}

/// A whole screen row as [`cells_text`] reads it with dimmed cells included,
/// with the `(byte, column)` where each column's text starts, ending with the
/// text's length and the column past it; `None` below the last row.
fn row_text_columns(copy: &ScreenCopy, row: u16) -> Option<(String, Vec<(usize, usize)>)> {
    let line = copy.lines.get(usize::from(row))?;
    let mut columns = Vec::new();
    let (text, next_col) =
        cells_text_mapped(line, 0, usize::from(copy.cols), false, |byte, col| {
            columns.push((byte, col))
        });
    columns.push((text.len(), usize::from(next_col)));
    Some((text, columns))
}

/// The text of `line` between columns `start` (inclusive) and `end`
/// (exclusive), and the column just past the last cell it took. A copy holds
/// only written cells, so the columns between them read as spaces and a line
/// has no trailing padding; a space a program wrote stays. With `skip_dim`,
/// dimmed cells are skipped like unwritten ones.
fn cells_text(line: &ScreenLine, start: usize, end: usize, skip_dim: bool) -> (String, u16) {
    cells_text_mapped(line, start, end, skip_dim, |_, _| {})
}

/// [`cells_text`], calling `column_starts(byte, col)` where the text of each
/// column begins. A combining mark adds bytes but no column, and a wide
/// character's second column has no start of its own.
fn cells_text_mapped(
    line: &ScreenLine,
    start: usize,
    end: usize,
    skip_dim: bool,
    mut column_starts: impl FnMut(usize, usize),
) -> (String, u16) {
    let mut text = String::new();
    let mut next_col = start;
    for cell in &line.cells {
        let col = usize::from(cell.col);
        if col >= end {
            break;
        }
        if col < start || (skip_dim && cell.dim) {
            continue;
        }
        for blank in next_col..col {
            column_starts(text.len(), blank);
            text.push(' ');
        }
        column_starts(text.len(), col);
        text.push(if cell.c == '\t' { ' ' } else { cell.c });
        text.extend(&cell.zerowidth);
        next_col = col + if cell.wide { 2 } else { 1 };
    }
    (text, u16::try_from(next_col).unwrap_or(u16::MAX))
}

/// Whether the marker row holds only the section's `placeholder`: nothing but
/// the marker and blanks left of the cursor, and one match spanning the whole
/// visible text right of it, so a hint followed by anything else is not one.
fn is_untyped_hint(section: &ComposerSection, cursor_line: &str, right_of_cursor: &str) -> bool {
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

/// The column whose text starts at `byte` in a [`row_text_columns`] map, or
/// `None` when `byte` falls inside a cell.
fn column_at_byte(columns: &[(usize, usize)], byte: usize) -> Option<u16> {
    columns
        .iter()
        .find(|&&(at, _)| at == byte)
        .and_then(|&(_, col)| u16::try_from(col).ok())
}

/// The byte where column `col` starts in a [`row_text_columns`] map.
fn byte_at_column(columns: &[(usize, usize)], col: u16) -> Option<usize> {
    columns
        .iter()
        .find(|&&(_, at)| at == usize::from(col))
        .map(|&(byte, _)| byte)
}

/// Why a copy was not applied. The copy held before stays as it was.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CopyRefused {
    /// Some of the copy's rows carry another copy's number.
    #[error("the copy's rows belong to another copy")]
    OtherCopy,
    /// A difference does not follow the copy held, or a whole copy is not
    /// newer than it.
    #[error("the copy does not follow the copy held")]
    OutOfOrder,
    /// A difference arrived with no copy to lay it over.
    #[error("a difference arrived before any whole copy")]
    NothingHeld,
    /// The copy's size is outside the window size limits.
    #[error("the copy's size is outside the window size limits")]
    SizeOutOfRange,
    /// A row is below the copy's bottom, or arrives twice.
    #[error("a row is outside the copy or arrives twice")]
    BadRow,
    /// A cell is right of the copy's edge, or out of column order.
    #[error("a cell is outside the copy or out of order")]
    BadCell,
    /// A cell carries more combining characters than
    /// [`MAX_ZEROWIDTH_PER_CELL`].
    #[error("a cell carries too many combining characters")]
    TooManyMarks,
}

/// The copy of a window's screen the daemon holds, applied only once it has
/// been checked whole.
#[derive(Default)]
pub struct HeldCopy {
    copy: Option<Arc<ScreenCopy>>,
    number: u64,
}

impl fmt::Debug for HeldCopy {
    // The rows are the child's output, which stays out of any log.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HeldCopy")
            .field("number", &self.number)
            .field("held", &self.copy.is_some())
            .finish()
    }
}

impl HeldCopy {
    /// Holds no copy yet; the first one applied must be whole.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The copy held, or `None` before the first whole one.
    #[must_use]
    pub fn copy(&self) -> Option<&ScreenCopy> {
        self.copy.as_deref()
    }

    /// Lays `rows` over the copy held (over an empty screen when `push` is
    /// whole) and holds the result.
    ///
    /// # Errors
    ///
    /// A [`CopyRefused`] when the copy breaks the order of copies or any part
    /// of it is outside its own size or the limits; the copy held stays as
    /// it was.
    pub fn apply(&mut self, rows: Vec<ScreenRows>, push: ScreenPush) -> Result<(), CopyRefused> {
        if rows.iter().any(|frame| frame.number != push.number) {
            return Err(CopyRefused::OtherCopy);
        }
        if !window_size_within_limit(push.cols, push.rows) {
            return Err(CopyRefused::SizeOutOfRange);
        }
        let mut lines = match (self.copy.as_deref(), push.full) {
            (Some(_), true) if push.number <= self.number => {
                return Err(CopyRefused::OutOfOrder);
            }
            (_, true) => blank_lines(push.rows),
            (Some(held), false) if push.number == self.number.wrapping_add(1) => {
                held.lines_resized(push.cols, push.rows)
            }
            (Some(_), false) => return Err(CopyRefused::OutOfOrder),
            (None, false) => return Err(CopyRefused::NothingHeld),
        };
        let mut replaced = vec![false; lines.len()];
        for line in rows.into_iter().flat_map(|frame| frame.lines) {
            let index = usize::from(line.line);
            match replaced.get_mut(index) {
                Some(seen) if !*seen => *seen = true,
                _ => return Err(CopyRefused::BadRow),
            }
            check_cells(&line, push.cols)?;
            lines[index] = line;
        }
        self.copy = Some(Arc::new(ScreenCopy {
            cols: push.cols,
            rows: push.rows,
            lines,
            cursor: push.cursor,
            cursor_hidden: push.cursor_hidden,
            sync_open: push.sync_open,
            input_modes: push.input_modes,
            generation: push.generation,
            outputs_fed: push.outputs_fed,
        }));
        self.number = push.number;
        Ok(())
    }
}

fn blank_lines(rows: u16) -> Vec<ScreenLine> {
    (0..rows)
        .map(|line| ScreenLine {
            line,
            cells: Vec::new(),
            wraps: false,
        })
        .collect()
}

/// Every cell of `line` is left of `cols`, in increasing column order, and
/// within the combining-character limit.
fn check_cells(line: &ScreenLine, cols: u16) -> Result<(), CopyRefused> {
    let mut next = 0u16;
    for cell in &line.cells {
        if cell.col < next || cell.col >= cols {
            return Err(CopyRefused::BadCell);
        }
        if cell.zerowidth.len() > MAX_ZEROWIDTH_PER_CELL {
            return Err(CopyRefused::TooManyMarks);
        }
        next = cell.col.saturating_add(1);
    }
    Ok(())
}

/// One read of a [`SharedCopy`].
#[derive(Debug)]
pub struct CopyRead<R> {
    /// What the read returned; `None` when no copy is held yet or the read
    /// panicked.
    pub value: Option<R>,
    /// The held copy's [`ScreenCopy::generation`] (0 before any copy).
    pub generation: u64,
    /// How many reads of this copy have panicked, this one included.
    pub read_losses: u64,
    /// How many copies were refused (see [`SharedCopy::apply`]).
    pub refused: u64,
}

/// What tells a reader that the screen it read before is gone: the held
/// copy's rebuild count, the reads that panicked and the copies refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CopyCounts {
    /// See [`CopyRead::generation`].
    pub generation: u64,
    /// See [`CopyRead::read_losses`].
    pub read_losses: u64,
    /// See [`CopyRead::refused`].
    pub refused: u64,
}

/// The copy of a window's screen, shared by the one that applies copies and
/// every reader. The lock is held only to apply a copy or to take the one
/// held, never while reading it.
#[derive(Debug, Clone, Default)]
pub struct SharedCopy(Arc<parking_lot::Mutex<CopySlot>>);

#[derive(Debug, Default)]
struct CopySlot {
    held: HeldCopy,
    /// Reads that panicked. Kept apart from the copy's own rebuild count, so
    /// a reader can tell a lost read from a lost screen and neither hides
    /// the other.
    read_losses: u64,
    /// Copies that were refused. The copy held stays, but it no longer shows
    /// what the model shows, so a reader treats it as lost until a whole copy
    /// replaces it.
    refused: u64,
}

impl SharedCopy {
    /// Applies a copy; see [`HeldCopy::apply`]. A refused copy is counted,
    /// so a reader learns the screen it holds is out of date.
    ///
    /// # Errors
    ///
    /// As [`HeldCopy::apply`].
    pub fn apply(&self, rows: Vec<ScreenRows>, push: ScreenPush) -> Result<(), CopyRefused> {
        let mut slot = self.0.lock();
        let applied = slot.held.apply(rows, push);
        if applied.is_err() {
            slot.refused = slot.refused.wrapping_add(1);
        }
        applied
    }

    /// Counts a copy refused before it reached [`SharedCopy::apply`].
    pub fn refuse(&self) {
        let mut slot = self.0.lock();
        slot.refused = slot.refused.wrapping_add(1);
    }

    /// The counts as they stand, without reading the copy.
    #[must_use]
    pub fn counts(&self) -> CopyCounts {
        let slot = self.0.lock();
        CopyCounts {
            generation: slot.held.copy().map_or(0, |copy| copy.generation),
            read_losses: slot.read_losses,
            refused: slot.refused,
        }
    }

    /// Runs `read` on the copy held.
    ///
    /// Never panics: a panic from `read` is contained, counted in
    /// [`CopyRead::read_losses`], and leaves the copy as it was.
    pub fn read<R>(&self, read: impl FnOnce(&ScreenCopy) -> R) -> CopyRead<R> {
        let (copy, read_losses, refused) = {
            let slot = self.0.lock();
            (slot.held.copy.clone(), slot.read_losses, slot.refused)
        };
        let Some(copy) = copy else {
            return CopyRead {
                value: None,
                generation: 0,
                read_losses,
                refused,
            };
        };
        // `AssertUnwindSafe`: `read` only borrows the copy, which nothing
        // else changes, so a panic leaves no half-updated value behind.
        match catch_unwind(AssertUnwindSafe(|| read(&copy))) {
            Ok(value) => CopyRead {
                value: Some(value),
                generation: copy.generation,
                read_losses,
                refused,
            },
            Err(_) => {
                let mut slot = self.0.lock();
                slot.read_losses = slot.read_losses.wrapping_add(1);
                tracing::warn!(
                    "a read of the window's screen copy panicked; the read is lost and \
                     screen-derived detection restarts"
                );
                CopyRead {
                    value: None,
                    generation: copy.generation,
                    read_losses: slot.read_losses,
                    refused,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use yagura_protocol::{InputModes, ScreenCell, ScreenCursor};

    use super::*;

    fn row(line: u16, text: &str, wraps: bool) -> ScreenLine {
        ScreenLine {
            line,
            cells: text
                .chars()
                .enumerate()
                .map(|(col, c)| ScreenCell {
                    col: u16::try_from(col).unwrap_or(u16::MAX),
                    c,
                    zerowidth: Vec::new(),
                    dim: false,
                    wide: false,
                })
                .collect(),
            wraps,
        }
    }

    #[test]
    fn a_space_written_where_the_line_wrapped_stays_between_its_words() {
        let copy = ScreenCopy {
            cols: 10,
            rows: 3,
            lines: vec![
                row(0, "AAAAAAAAA ", true),
                row(1, "BBB  ", false),
                row(2, "", false),
            ],
            cursor: ScreenCursor::default(),
            cursor_hidden: false,
            sync_open: false,
            input_modes: InputModes::default(),
            generation: 0,
            outputs_fed: 0,
        };
        assert_eq!(screen_text(&copy), "AAAAAAAAA BBB");
    }
}
