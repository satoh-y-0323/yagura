#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! `[composer] right_border`: a CLI that draws its composer inside a box with
//! a right edge that is not dimmed can still have its draft read.
//!
//! Screens are synthetic but use the box layout and border colors a boxed
//! composer is drawn with (two-column indent, `│ > ` marker row, `│   `
//! continuation rows, a truecolor `│` right edge).

use yagura_daemon::detector::VtScreen;
use yagura_daemon::detector::definition::{ComposerSection, DefinitionError, parse_definition};
use yagura_daemon::detector::screen::ComposerReadout;

const COLS: u16 = 40;
const ROWS: u16 = 8;
/// The SGR a boxed composer's right edge is drawn with: colored, not DIM.
const BORDER: &str = "\x1b[38;2;69;69;69;48;2;20;20;20m│\x1b[0m";
const BOX_TOP: &str = "  ╭──────────────────────────────────╮  ";
const BOX_BOTTOM: &str = "  ╰──────────────────────────────────╯  ";
const FOOTER: &str = "  Enter:send  │  Shift+Tab:mode";

fn definition_source(extra_composer_keys: &str) -> String {
    format!(
        r#"schema_version = 1
name = "boxed"

[[rules]]
status = "waiting_input"
pattern = 'Enter:send'
priority = 10
region = "bottom"

[composer]
marker = '^\s*│ [>❯]'
separator = " "
continuation_prefix = "  │   "
{extra_composer_keys}
"#
    )
}

fn composer(extra_composer_keys: &str) -> ComposerSection {
    parse_definition(&definition_source(extra_composer_keys))
        .expect("definition must load")
        .composer
        .expect("definition must declare a composer")
}

/// A box row: `inner` padded to the box's inner width, then the right edge.
fn box_row(inner: &str) -> String {
    let width = BOX_TOP.chars().count() - 3;
    let pad = width - inner.chars().count();
    format!("{inner}{}{BORDER}  ", " ".repeat(pad))
}

/// Feeds `rows` top to bottom and leaves the cursor at 0-based (`row`, `col`).
fn screen(rows: &[String], row: u16, col: u16) -> VtScreen {
    let mut screen = VtScreen::new(COLS, ROWS);
    screen.feed(rows.join("\r\n").as_bytes());
    screen.feed(format!("\x1b[{};{}H", row + 1, col + 1).as_bytes());
    screen
}

/// A two-row draft whose first row is filled up to the padding before the
/// edge, so the body has no space at the fold.
fn boxed_draft() -> (Vec<String>, u16, u16) {
    let first = "  │ > HEADABCDEFGHIJKLMNOPQRSTUVW";
    let second = "  │   XYZTAIL";
    let rows = vec![
        String::new(),
        BOX_TOP.to_string(),
        box_row(first),
        box_row(second),
        BOX_BOTTOM.to_string(),
        FOOTER.to_string(),
    ];
    (rows, 3, second.chars().count() as u16)
}

#[test]
fn a_right_border_is_dropped_from_every_composer_row_and_only_when_declared() {
    let (rows, cursor_row, cursor_col) = boxed_draft();
    let draft = screen(&rows, cursor_row, cursor_col);

    match draft.read_composer(&composer("")) {
        ComposerReadout::NotObserved => {}
        other => {
            panic!("without right_border the box edge must keep the draft unread, got {other:?}")
        }
    }

    let bordered = composer(r"right_border = '│\s*$'");
    match draft.read_composer(&bordered) {
        ComposerReadout::Body(text) => assert_eq!(text, "HEADABCDEFGHIJKLMNOPQRSTUVWXYZTAIL"),
        other => panic!("expected the draft without the box edge, got {other:?}"),
    }

    // Typed text right of the cursor is still text the reader cannot place.
    let mid_draft = screen(&rows, cursor_row, cursor_col - 4);
    match mid_draft.read_composer(&bordered) {
        ComposerReadout::NotObserved => {}
        other => panic!("typed text right of the cursor must stay unread, got {other:?}"),
    }
}

#[test]
fn a_boxed_composer_holding_only_its_border_or_its_hint_reads_as_empty() {
    let marker = "  │ > ";
    let col = marker.chars().count() as u16;
    let empty_rows = |inner: &str| {
        vec![
            String::new(),
            BOX_TOP.to_string(),
            box_row(inner),
            BOX_BOTTOM.to_string(),
            FOOTER.to_string(),
        ]
    };

    let bordered = composer(r"right_border = '│\s*$'");
    match screen(&empty_rows(marker), 2, col).read_composer(&bordered) {
        ComposerReadout::Empty => {}
        other => panic!("a box with only its edge must read as empty, got {other:?}"),
    }

    let hinted = composer("right_border = '│\\s*$'\nplaceholder = '^Build anything$'");
    let hint = format!("{marker}Build anything");
    match screen(&empty_rows(&hint), 2, col).read_composer(&hinted) {
        ComposerReadout::Empty => {}
        other => panic!("the hint is judged after the edge is dropped, got {other:?}"),
    }
}

#[test]
fn an_invalid_right_border_pattern_rejects_the_definition() {
    match parse_definition(&definition_source("right_border = '('")) {
        Err(DefinitionError::InvalidPattern { pattern, .. }) => assert_eq!(pattern, "("),
        other => panic!("expected the right_border pattern to be rejected, got {other:?}"),
    }
}
