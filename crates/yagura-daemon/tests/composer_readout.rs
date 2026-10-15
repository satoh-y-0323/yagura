#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Reconstructing the composer's body text from a screen.
//!
//! `VtScreen::read_composer` is a pure function of a `ComposerSection` (the
//! marker / separator / continuation-prefix triple a detection definition can
//! declare) and the screen's own lines and cursor position: no window, no PTY,
//! no clock. These tests build `ComposerSection` fixtures directly rather than
//! through the TOML loader (covered separately in `detector_definition.rs`),
//! and screens directly through `VtScreen::feed`, so the readout algorithm can
//! be exercised in isolation.
//!
//! Region determination itself (the cursor-bound walk that finds where a
//! composer's text starts) is covered in `detector_screen.rs`, next to
//! `cursor_row`; this file covers the result
//! (`Body` / `Tail` / `Empty` / `NotObserved`) and the restore rule that turns
//! a region's raw lines into recovered body text.

use regex::Regex;
use yagura_daemon::detector::VtScreen;
use yagura_daemon::detector::definition::ComposerSection;
use yagura_daemon::detector::screen::ComposerReadout;

/// A composer-identifying section built directly, without going through the
/// TOML loader.
fn composer(marker: &str, separator: &str, continuation_prefix: &str) -> ComposerSection {
    ComposerSection {
        marker: Regex::new(marker).expect("test fixture pattern must compile"),
        separator: separator.to_string(),
        continuation_prefix: continuation_prefix.to_string(),
        blank_lines_in_body: false,
    }
}

/// A screen exactly `rows.len()` rows tall, `cols` wide, with `rows` fed in as
/// display lines joined by CRLF. The cursor is left wherever ordinary output
/// would leave it (the end of the last row fed).
fn feed_rows(cols: u16, rows: &[&str]) -> VtScreen {
    let mut screen = VtScreen::new(cols, rows.len() as u16);
    screen.feed(rows.join("\r\n").as_bytes());
    screen
}

const LONG_BODY_LEN: usize = 246;

/// A deterministic body of exactly [`LONG_BODY_LEN`] characters with `HEAD`
/// and `TAIL` sentinels at each end, so a reconstruction bug that drops or
/// reorders a row shows up as a readable mismatch instead of a length-only
/// diff.
fn long_body() -> String {
    let mut body = String::from("HEAD");
    while body.len() < LONG_BODY_LEN - 4 {
        let i = body.len();
        body.push((b'A' + (i % 26) as u8) as char);
    }
    body.push_str("TAIL");
    body
}

// =====================================================================
// Both bundled marker forms round-trip an empty and a typed
// composer.
// =====================================================================

#[test]
fn claude_code_marker_distinguishes_empty_from_a_typed_body() {
    let section = composer("^\u{276f}", "\u{a0}", "  ");

    match feed_rows(30, &["\u{276f}"]).read_composer(&section) {
        ComposerReadout::Empty => {}
        other => panic!("expected Empty, got {other:?}"),
    }

    match feed_rows(30, &["\u{276f}\u{a0}PROBEMARKER123"]).read_composer(&section) {
        ComposerReadout::Body(text) => assert_eq!(text, "PROBEMARKER123"),
        other => panic!("expected Body, got {other:?}"),
    }
}

#[test]
fn codex_marker_distinguishes_empty_from_a_typed_body() {
    let section = composer("^\u{203a}", " ", "  ");

    match feed_rows(30, &["\u{203a}"]).read_composer(&section) {
        ComposerReadout::Empty => {}
        other => panic!("expected Empty, got {other:?}"),
    }

    match feed_rows(30, &["\u{203a} PROBEMARKER123"]).read_composer(&section) {
        ComposerReadout::Body(text) => assert_eq!(text, "PROBEMARKER123"),
        other => panic!("expected Body, got {other:?}"),
    }
}

#[test]
fn shell_form_with_an_empty_continuation_prefix_reconstructs_a_short_body() {
    let section = composer(r"[$#>]", " ", "");

    match feed_rows(40, &["user@host:/repo$ "]).read_composer(&section) {
        ComposerReadout::Empty => {}
        other => panic!("expected Empty, got {other:?}"),
    }

    match feed_rows(40, &["user@host:/repo$ zzShortDraft"]).read_composer(&section) {
        ComposerReadout::Body(text) => assert_eq!(text, "zzShortDraft"),
        other => panic!("expected Body, got {other:?}"),
    }
}

// =====================================================================
// No marker line anywhere on screen must report NotObserved, never
// Empty and never a guessed row.
// =====================================================================

#[test]
fn screens_with_no_composer_at_all_fall_back_to_not_observed() {
    let section = composer("^\u{276f}", "\u{a0}", "  ");

    let startup = feed_rows(40, &["Welcome to Claude Code", "Loading workspace..."]);
    match startup.read_composer(&section) {
        ComposerReadout::NotObserved => {}
        other => panic!("expected NotObserved for a startup screen, got {other:?}"),
    }

    let trust_dialog = feed_rows(
        40,
        &[
            "Do you trust the files in this folder?",
            "1. Yes, proceed",
            "2. No, exit",
        ],
    );
    match trust_dialog.read_composer(&section) {
        ComposerReadout::NotObserved => {}
        other => panic!("expected NotObserved for a trust dialog, got {other:?}"),
    }

    let update_prompt = feed_rows(
        40,
        &["A new version is available.", "1. Update now", "2. Skip"],
    );
    match update_prompt.read_composer(&section) {
        ComposerReadout::NotObserved => {}
        other => panic!("expected NotObserved for an update-check prompt, got {other:?}"),
    }
}

#[test]
fn an_empty_composer_is_distinguishable_from_no_composer_at_all() {
    let section = composer("^\u{276f}", "\u{a0}", "  ");

    match feed_rows(40, &["\u{276f}"]).read_composer(&section) {
        ComposerReadout::Empty => {}
        other => panic!("expected Empty, got {other:?}"),
    }

    match feed_rows(40, &["no marker on this screen at all"]).read_composer(&section) {
        ComposerReadout::NotObserved => {}
        other => panic!("expected NotObserved, got {other:?}"),
    }
}

// =====================================================================
// Restore rule: a body wrapped across several continuation lines,
// including a line with real trailing whitespace written by the CLI itself
// (not unwritten padding `bottom_lines` already trims), reconstructs
// exactly.
// =====================================================================

#[test]
fn a_long_body_across_three_rows_reconstructs_exactly_with_trailing_padding_trimmed() {
    let section = composer("^\u{276f}", "\u{a0}", "  ");
    let body = long_body();
    assert_eq!(body.len(), LONG_BODY_LEN, "fixture precondition");

    let chunk1 = &body[0..84];
    let chunk2 = &body[84..168];
    let chunk3 = &body[168..246];

    let row0 = format!("\u{276f}\u{a0}{chunk1}");
    // Real written trailing spaces on a continuation row, distinct from the
    // unwritten cells `bottom_lines` already trims.
    let row1 = format!("  {chunk2}    ");
    let row2 = format!("  {chunk3}");

    let screen = feed_rows(90, &[&row0, &row1, &row2]);

    match screen.read_composer(&section) {
        ComposerReadout::Body(text) => assert_eq!(text, body),
        other => panic!("expected Body, got {other:?}"),
    }
}

// =====================================================================
// Further edge cases beyond the basic round trips above.
// =====================================================================

#[test]
fn claude_code_composer_bordered_by_rule_lines_does_not_pick_up_lines_outside_the_region() {
    let section = composer("^\u{276f}", "\u{a0}", "  ");
    let border = "-".repeat(40);
    let mut screen = feed_rows(
        40,
        &[
            "some earlier output line",
            &border,
            "\u{276f}\u{a0}insideTheBorder",
            &border,
        ],
    );
    // The cursor sits on the composer's own row, between the two border
    // lines, not wherever the bottom border (fed last) left it.
    screen.feed(b"\x1b[3;18H");

    match screen.read_composer(&section) {
        ComposerReadout::Body(text) => assert_eq!(text, "insideTheBorder"),
        other => panic!("expected Body, got {other:?}"),
    }
}

#[test]
fn shell_output_row_that_looks_like_a_prompt_is_not_mistaken_for_the_composer() {
    // The cursor -- not content matching -- is what tells a finished
    // command's output apart from the live composer, even when the output
    // happens to end in the same shape a prompt does.
    let section = composer(r"[$#>]", " ", "");
    let mut screen = feed_rows(
        50,
        &[
            "previous command reported: balance$ 12",
            "user@host:/repo$ realDraftBody",
        ],
    );
    screen.feed(b"\x1b[2;31H");

    match screen.read_composer(&section) {
        ComposerReadout::Body(text) => assert_eq!(text, "realDraftBody"),
        other => panic!("expected Body, got {other:?}"),
    }
}

#[test]
fn a_folded_placeholder_reconstructs_to_something_other_than_the_original_body() {
    // Claude Code folds a large paste into a placeholder line ("[Pasted text
    // #1]"). Reconstruction must never claim the recovered text equals the
    // (much longer) original body just because a composer row is present, and
    // no vendor-specific placeholder pattern is added to the definition to
    // detect this case -- the contract is simply that the two must not
    // report as equal.
    let section = composer("^\u{276f}", "\u{a0}", "  ");
    let original_body = long_body();

    let screen = feed_rows(40, &["\u{276f}\u{a0}[Pasted text #1]"]);

    match screen.read_composer(&section) {
        ComposerReadout::Body(text) => assert_ne!(
            text, original_body,
            "a folded placeholder must never be reported as matching the original body"
        ),
        other => panic!("expected Body (the placeholder text itself), got {other:?}"),
    }
}

// =====================================================================
// A generic-shell prompt whose PS1 has a separator occurrence
// before the marker (e.g. PowerShell's "PS C:\...> ") must split at the
// marker's own position, not at the first separator anywhere on the line.
// =====================================================================

#[test]
fn a_prompt_with_a_separator_before_the_marker_reads_as_empty_when_untyped() {
    let section = composer(r"[$#>]", " ", "");

    let screen = feed_rows(60, &[r"PS C:\Users\alice\projects\yagura> "]);

    match screen.read_composer(&section) {
        ComposerReadout::Empty => {}
        other => panic!("expected Empty, got {other:?}"),
    }
}

#[test]
fn a_prompt_with_a_separator_before_the_marker_reads_only_the_draft() {
    let section = composer(r"[$#>]", " ", "");

    let screen = feed_rows(60, &[r"PS C:\Users\alice\projects\yagura> myDraftText"]);

    match screen.read_composer(&section) {
        ComposerReadout::Body(text) => assert_eq!(text, "myDraftText"),
        other => panic!("expected Body, got {other:?}"),
    }
}

// =====================================================================
// A blank the terminal draws only by moving the cursor, rather than by
// writing a space cell (a separator drawn this way still reads as absent).
// =====================================================================

#[test]
fn a_separator_space_drawn_as_a_cursor_move_still_reads_empty() {
    let section = composer(">", " ", "");

    match feed_rows(20, &[">\x1b[1C"]).read_composer(&section) {
        ComposerReadout::Empty => {}
        other => panic!("expected Empty, got {other:?}"),
    }
    match feed_rows(20, &[">\x1b[1Chi"]).read_composer(&section) {
        ComposerReadout::Body(text) => assert_eq!(text, "hi"),
        other => panic!("expected Body, got {other:?}"),
    }
}

// =====================================================================
// The body is everything left of the cursor column; a
// non-empty remainder to the right of the cursor is a body-vs-decoration
// question the marker/separator/continuation-prefix triple cannot answer,
// so it is resolved by the SGR "dim" attribute (the cell's dim flag)
// instead of a vendor-specific placeholder string.
// =====================================================================

/// Positions the cursor with a raw CUP escape (1-based row/col), the same
/// mechanism the existing border/prompt tests above already use.
fn move_cursor(screen: &mut VtScreen, row: u16, col: u16) {
    screen.feed(format!("\x1b[{};{}H", row + 1, col + 1).as_bytes());
}

#[test]
fn codex_dim_placeholder_reads_as_empty_and_a_typed_body_reads_as_itself() {
    let section = composer("^\u{203a}", " ", "  ");

    // Idle: marker + separator, then the hint text dimmed -- Codex leaves the
    // cursor right after the separator while it draws the hint.
    let mut idle = feed_rows(80, &["\u{203a} \x1b[2mAsk Codex to do anything\x1b[0m"]);
    move_cursor(&mut idle, 0, 2);
    match idle.read_composer(&section) {
        ComposerReadout::Empty => {}
        other => panic!("expected Empty for a dim placeholder, got {other:?}"),
    }

    // Typed: the placeholder is fully replaced, nothing dim remains, and the
    // cursor sits at the end of the typed text as ordinary printing leaves it.
    let typed = feed_rows(80, &["\u{203a} zzDraft"]);
    match typed.read_composer(&section) {
        ComposerReadout::Body(text) => assert_eq!(text, "zzDraft"),
        other => panic!("expected Body, got {other:?}"),
    }
}

#[test]
fn claude_code_dim_placeholder_reads_as_empty_and_a_typed_body_reads_as_itself() {
    let section = composer("^\u{276f}", "\u{a0}", "  ");

    let mut idle = feed_rows(
        80,
        &["\u{276f}\u{a0}\x1b[2mTry \"write a test for <filepath>\"\x1b[0m"],
    );
    move_cursor(&mut idle, 0, 2);
    match idle.read_composer(&section) {
        ComposerReadout::Empty => {}
        other => panic!("expected Empty for a dim placeholder, got {other:?}"),
    }

    let typed = feed_rows(80, &["\u{276f}\u{a0}zzDraft"]);
    match typed.read_composer(&section) {
        ComposerReadout::Body(text) => assert_eq!(text, "zzDraft"),
        other => panic!("expected Body, got {other:?}"),
    }
}

#[test]
fn a_non_dim_remainder_to_the_right_of_the_cursor_falls_back_to_not_observed() {
    let section = composer("^\u{203a}", " ", "  ");

    // Nothing before the cursor, but the remainder is not dim: this is the
    // degraded case (e.g. a `NO_COLOR` terminal draws no placeholder
    // attribute at all), and it must not be read as an empty composer.
    let mut nothing_typed_but_not_dim = feed_rows(80, &["\u{203a} STILLTEXT"]);
    move_cursor(&mut nothing_typed_but_not_dim, 0, 2);
    match nothing_typed_but_not_dim.read_composer(&section) {
        ComposerReadout::NotObserved => {}
        other => panic!("expected NotObserved for a non-dim remainder, got {other:?}"),
    }

    // A real body, plus non-dim text to the right of the cursor on the same
    // row: this must not be concatenated onto the body as if it were the
    // user's own text.
    let mut body_with_trailing_text = feed_rows(80, &["\u{203a} zzDraft EXTRA"]);
    move_cursor(&mut body_with_trailing_text, 0, 9);
    match body_with_trailing_text.read_composer(&section) {
        ComposerReadout::NotObserved => {}
        other => panic!("expected NotObserved for a trailing non-dim remainder, got {other:?}"),
    }
}

#[test]
fn a_cursor_left_in_the_middle_of_a_wrapped_body_falls_back_to_not_observed_instead_of_shortening_it()
 {
    let section = composer("^\u{276f}", "\u{a0}", "  ");
    // Three rows of a wrapped draft, as if typed in full and then the cursor
    // was moved back (Home / Left) into the second row, mid-word.
    let mut screen = feed_rows(40, &["\u{276f}\u{a0}HEADPART", "  MIDPART", "  TAILPART"]);
    move_cursor(&mut screen, 1, 5);

    match screen.read_composer(&section) {
        ComposerReadout::NotObserved => {}
        other => panic!(
            "expected NotObserved for a cursor stopped mid-body, got {other:?} \
             (a shortened Body would silently drop the row below the cursor)"
        ),
    }
}

// =====================================================================
// Real screens: raw PTY byte streams captured from the three bundled CLIs,
// fed to a real `VtScreen` and read with the shipped definition's composer
// section (`fixtures/<cli>/composer/`, see each README).
// =====================================================================

use yagura_daemon::detector::definition::{bundled_definition_source, parse_definition};

fn shipped_composer(definition: &str) -> ComposerSection {
    let source = bundled_definition_source(definition).expect("bundled definition must exist");
    parse_definition(source)
        .expect("bundled definition must parse")
        .composer
        .expect("bundled definition must declare a composer")
}

fn captured_screen(cli: &str, name: &str, cols: u16, rows: u16) -> VtScreen {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(cli)
        .join("composer")
        .join(format!("{name}.bin"));
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let mut screen = VtScreen::new(cols, rows);
    screen.feed(&bytes);
    screen
}

/// Body text with every whitespace character removed, so a comparison does
/// not depend on where the CLI folded its rows.
fn squeezed(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace()).collect()
}

const HANDOFF_BODY: &str = concat!(
    r#"[yagura handoff] {"from":{"window_id":"w4-80d4k1","name":"Codex CLI","cwd":"C:\\demo\\todo-app"},"#,
    r#""reference":"src/todo.js","note":"Review complete. Definite bug at line 2: addTodo assigns "#,
    r#"list.length + 1, so removing an item or receiving a non-contiguous/preexisting ID list can "#,
    r#"reuse an existing ID (e.g. [{id:1},{id:3}] -> new id 3). This can make completeTodo update "#,
    r#"multiple todos sharing that ID. completeTodo otherwise maps immutably and is a no-op for "#,
    r#"unknown IDs; pendingTodos correctly filters unfinished todos under the boolean done "#,
    r#"contract. Existing tests were not runnable: npm.ps1 is blocked by PowerShell policy; "#,
    r#"npm.cmd test reaches Node but fails spawning the test worker with EPERM. No edits "#,
    r#"made.","reply":{"tool":"send_handoff","window_id":"w4-80d4k1"}}"#
);

#[test]
fn claude_wrapped_body_containing_the_marker_glyph_and_an_arrow_reads_as_the_body() {
    let section = shipped_composer("claude-code");
    let screen = captured_screen("claude-code", "100x30-wrapped-body-with-arrow", 100, 30);

    match screen.read_composer(&section) {
        ComposerReadout::Body(text) => assert_eq!(squeezed(&text), squeezed(HANDOFF_BODY)),
        other => panic!("expected the whole wrapped body, got {other:?}"),
    }
}

#[test]
fn claude_body_left_partly_after_ctrl_u_reads_as_the_remaining_part() {
    let section = shipped_composer("claude-code");
    let screen = captured_screen("claude-code", "100x30-partial-body-after-ctrl-u", 100, 30);

    match screen.read_composer(&section) {
        ComposerReadout::Body(text) => assert_eq!(
            squeezed(&text),
            squeezed(r#"[yagura handoff] {"from":{"window_id":"w4-80d4k1","name":"Codex"#)
        ),
        other => panic!("expected the remaining first row as Body, got {other:?}"),
    }
}

#[test]
fn claude_empty_composer_with_a_dim_hint_reads_as_empty() {
    let section = shipped_composer("claude-code");
    let screen = captured_screen("claude-code", "100x30-empty", 100, 30);

    match screen.read_composer(&section) {
        ComposerReadout::Empty => {}
        other => panic!("expected Empty, got {other:?}"),
    }
}

#[test]
fn claude_screen_whose_marker_row_scrolled_off_reads_as_the_tail() {
    let section = shipped_composer("claude-code");
    let screen = captured_screen("claude-code", "80x24-marker-scrolled-off", 80, 24);
    // Two bodies were typed; the top row starts inside the first one, at the
    // word after "Codex".
    let top = HANDOFF_BODY
        .find(r#"CLI","cwd""#)
        .expect("fixture precondition: the body names the CLI");
    let visible = format!("{}{HANDOFF_BODY}", &HANDOFF_BODY[top..]);

    match screen.read_composer(&section) {
        ComposerReadout::Tail(text) => {
            assert!(
                squeezed(&text).ends_with(&squeezed(r#""window_id":"w4-80d4k1"}}"#)),
                "the tail must end where the composer ends, got {text:?}"
            );
            assert_eq!(squeezed(&text), squeezed(&visible));
        }
        other => panic!("expected Tail, got {other:?}"),
    }
}

#[test]
fn claude_scrolled_off_screen_with_the_cursor_moved_back_into_the_body_is_not_observed() {
    let section = shipped_composer("claude-code");
    let mut screen = captured_screen("claude-code", "80x24-marker-scrolled-off", 80, 24);
    move_cursor(&mut screen, 20, 10);

    match screen.read_composer(&section) {
        ComposerReadout::NotObserved => {}
        other => panic!("expected NotObserved, got {other:?}"),
    }
}

// =====================================================================
// Without the marker on screen, continuation rows that reach the top row
// are the end of a body; anything short of that is a frame the CLI has not
// finished drawing.
// =====================================================================

#[test]
fn continuation_rows_reaching_the_top_read_as_the_tail_without_their_prefix() {
    let section = shipped_composer("claude-code");

    match feed_rows(30, &["  first", "  second", "  third"]).read_composer(&section) {
        ComposerReadout::Tail(text) => assert_eq!(text, "firstsecondthird"),
        other => panic!("expected Tail, got {other:?}"),
    }
}

#[test]
fn a_frame_whose_composer_rows_were_just_erased_is_not_observed() {
    let section = shipped_composer("claude-code");

    // The cursor waits on an erased row under the transcript.
    let mut under_transcript = feed_rows(40, &["\u{23fa} an answer", "  wrapped answer", "", ""]);
    move_cursor(&mut under_transcript, 2, 2);
    match under_transcript.read_composer(&section) {
        ComposerReadout::NotObserved => {}
        other => panic!("expected NotObserved under the transcript, got {other:?}"),
    }

    // Everything above the cursor was erased too.
    let mut all_erased = feed_rows(40, &["", "", ""]);
    move_cursor(&mut all_erased, 2, 2);
    match all_erased.read_composer(&section) {
        ComposerReadout::NotObserved => {}
        other => panic!("expected NotObserved on an erased screen, got {other:?}"),
    }
}

#[test]
fn continuation_rows_that_stop_below_the_top_are_not_observed() {
    let section = shipped_composer("claude-code");

    match feed_rows(30, &["\u{23fa} an answer", "  second", "  third"]).read_composer(&section) {
        ComposerReadout::NotObserved => {}
        other => panic!("expected NotObserved, got {other:?}"),
    }
}

#[test]
fn continuation_rows_reaching_the_top_with_no_text_are_not_observed() {
    // An empty tail would end every body alike.
    let section = shipped_composer("claude-code");

    match feed_rows(10, &["  ", "  ", "  "]).read_composer(&section) {
        ComposerReadout::NotObserved => {}
        other => panic!("expected NotObserved, got {other:?}"),
    }
}

#[test]
fn continuation_rows_reaching_the_top_with_text_right_of_the_cursor_are_not_observed() {
    let section = shipped_composer("claude-code");
    let mut screen = feed_rows(30, &["  first", "  second EXTRA"]);
    move_cursor(&mut screen, 1, 8);

    match screen.read_composer(&section) {
        ComposerReadout::NotObserved => {}
        other => panic!("expected NotObserved, got {other:?}"),
    }
}

#[test]
fn codex_and_antigravity_captured_composers_keep_reading_as_before() {
    for (cli, definition) in [
        ("codex-cli", "codex-cli"),
        ("antigravity-cli", "antigravity-cli"),
    ] {
        let section = shipped_composer(definition);

        let body = captured_screen(cli, "100x30-wrapped-body-with-arrow", 100, 30);
        match body.read_composer(&section) {
            ComposerReadout::Body(text) => {
                assert_eq!(squeezed(&text), squeezed(HANDOFF_BODY), "{cli} body");
            }
            other => panic!("{cli}: expected Body, got {other:?}"),
        }

        let empty = captured_screen(cli, "100x30-empty", 100, 30);
        match empty.read_composer(&section) {
            ComposerReadout::Empty => {}
            other => panic!("{cli}: expected Empty, got {other:?}"),
        }
    }
}

#[test]
fn visible_text_right_after_the_marker_without_a_separator_is_not_observed() {
    // "\u{203a}abc" is not a composer row of this definition: the separator is
    // missing, so the text cannot be told apart from the CLI's own drawing.
    let section = composer("^\u{203a}", " ", "  ");

    match feed_rows(30, &["\u{203a}abc"]).read_composer(&section) {
        ComposerReadout::NotObserved => {}
        other => panic!("expected NotObserved, got {other:?}"),
    }
}

// =====================================================================
// A blank line inside a multi-line body is drawn as nothing at all, not as
// the continuation prefix: a definition that declares `blank_lines_in_body`
// walks across it, and one that does not stops there.
// =====================================================================

fn composer_with_blank_lines() -> ComposerSection {
    ComposerSection {
        blank_lines_in_body: true,
        ..composer("^>", " ", "  ")
    }
}

#[test]
fn a_blank_line_inside_the_body_reaches_the_marker_when_declared() {
    match feed_rows(30, &["> first", "", "  third"]).read_composer(&composer_with_blank_lines()) {
        ComposerReadout::Body(text) => assert_eq!(text, "firstthird"),
        other => panic!("expected Body, got {other:?}"),
    }
}

#[test]
fn a_blank_line_ends_the_walk_when_not_declared() {
    match feed_rows(30, &["> first", "", "  third"]).read_composer(&composer("^>", " ", "  ")) {
        ComposerReadout::NotObserved => {}
        other => panic!("expected NotObserved, got {other:?}"),
    }
}

#[test]
fn blank_lines_do_not_reach_past_a_line_that_is_neither_marker_nor_continuation() {
    let rows = ["> history", "output", "", "  third"];
    match feed_rows(30, &rows).read_composer(&composer_with_blank_lines()) {
        ComposerReadout::NotObserved => {}
        other => panic!("expected NotObserved, got {other:?}"),
    }
}

#[test]
fn a_cursor_at_the_start_of_a_blank_row_is_not_in_the_composer() {
    match feed_rows(30, &["> first", "  second", ""]).read_composer(&composer_with_blank_lines()) {
        ComposerReadout::NotObserved => {}
        other => panic!("expected NotObserved, got {other:?}"),
    }
}

#[test]
fn blank_lines_are_not_crossed_while_the_cursor_is_hidden() {
    let mut screen = feed_rows(30, &["> first", "", "  third"]);
    screen.feed(b"\x1b[?25l");
    match screen.read_composer(&composer_with_blank_lines()) {
        ComposerReadout::NotObserved => {}
        other => panic!("expected NotObserved, got {other:?}"),
    }
}

#[test]
fn a_marker_character_inside_the_prompt_text_does_not_move_the_body_start() {
    // `#` in the path matches the marker class before the real prompt does, so the
    // body starts after the first separator that follows the match.
    let section = composer(r"[$#>]", " ", "");
    let screen = feed_rows(60, &[r"PS C:\a#b> myDraft"]);

    match screen.read_composer(&section) {
        ComposerReadout::Body(text) => assert_eq!(text, "myDraft"),
        other => panic!("expected Body, got {other:?}"),
    }
    assert_eq!(
        screen.bottom_lines_without_composer(1, &section),
        vec![r"PS C:\a#b> ".to_string()]
    );
}

#[test]
fn a_body_that_fills_the_row_exactly_is_still_read() {
    // The cursor waits to wrap past the last column; the body is left of it.
    let section = composer(">", " ", "");
    let screen = feed_rows(10, &["> 12345678"]);

    match screen.read_composer(&section) {
        ComposerReadout::Body(text) => assert_eq!(text, "12345678"),
        other => panic!("expected Body, got {other:?}"),
    }
}
