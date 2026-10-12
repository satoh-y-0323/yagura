#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The daemon sends each attached connection what its window's terminal model
//! shows, as cells in `frame` messages. These tests drive a real window
//! (`term_script`) through a real daemon, attach over the real IPC endpoint,
//! apply the frames the way a renderer does (`yagura_test_support::frame::Grid`)
//! and compare the grid with the model: wide and combining characters, resize
//! on the main and the alternate screen, scrolling output, colors and flags,
//! the cursor, and that a small change costs a small frame.

mod common;

use std::collections::BTreeSet;
use std::time::Duration;

use common::Harness;
use common::views::{
    CATCH_UP, assert_converges_to_model, numbered_lines, open_script_window, script_write,
    script_write_and_wait,
};
use yagura_daemon::DaemonConfig;
use yagura_protocol::{CellColor, CellFlag, ClientMessage, CursorShape, PaletteSlot, TerminalSize};
use yagura_test_support::view::ViewClient;

const SIZE: TerminalSize = TerminalSize { cols: 80, rows: 24 };
const CLEAR: &[u8] = b"\x1b[H\x1b[2J";

/// A script window with a view attached to it at 80x24 and the first frame
/// acknowledged.
async fn attached_script_window(tag: &str) -> (Harness, String, ViewClient) {
    let h = Harness::start(tag, DaemonConfig::default()).await;
    let window_id = open_script_window(&h).await;
    let view = ViewClient::attach(h.connect().await, &window_id, Some(SIZE)).await;
    (h, window_id, view)
}

fn bytes_of(parts: &[&[u8]]) -> Vec<u8> {
    parts.concat()
}

#[tokio::test]
async fn wide_emoji_and_combining_characters_arrive_as_cells_of_the_widths_the_daemon_decided() {
    let (h, window_id, mut view) = attached_script_window("view-wide").await;

    let line = "ZW \u{5168}\u{89d2}ABC\u{1F600}e\u{301}\u{ff71} END-1\r\n";
    script_write_and_wait(
        &h,
        &window_id,
        &bytes_of(&[CLEAR, line.as_bytes()]),
        "END-1",
    )
    .await;
    view.pump_until("the wide line", CATCH_UP, |grid| {
        grid.contains_text("END-1")
    })
    .await;
    assert_converges_to_model(&mut view, &h, &window_id, "wide line", CATCH_UP).await;

    let grid = &view.grid;
    for (name, text) in [("CJK", "\u{5168}"), ("emoji", "\u{1F600}")] {
        let (row, col) = grid
            .find_cell(text)
            .unwrap_or_else(|| panic!("the {name} cell is missing:\n{}", grid.dump()));
        assert_eq!(grid.cell(row, col).width, 2, "the {name} cell is two wide");
        assert_eq!(
            grid.cell(row, col + 1).width,
            0,
            "the {name} cell is followed by its spacer"
        );
    }
    let (row, col) = grid
        .find_cell("e\u{301}")
        .unwrap_or_else(|| panic!("the combining cell is missing:\n{}", grid.dump()));
    assert_eq!(
        grid.cell(row, col).width,
        1,
        "base and combining character are one cell"
    );
    let (row, col) = grid.find_cell("\u{ff71}").expect("the half-width cell");
    assert_eq!(grid.cell(row, col).width, 1);
    for line in &grid.lines {
        for cell in &line.cells {
            assert_ne!(
                cell.text, "\u{301}",
                "a combining character never occupies a cell of its own"
            );
        }
    }
}

#[tokio::test]
async fn a_wide_character_that_does_not_fit_the_last_column_wraps_and_leaves_a_blank_cell() {
    let (h, window_id, mut view) = attached_script_window("view-wide-wrap").await;

    let line = format!("{}\u{5168}TAIL-1\r\n", "y".repeat(79));
    script_write_and_wait(
        &h,
        &window_id,
        &bytes_of(&[CLEAR, line.as_bytes()]),
        "TAIL-1",
    )
    .await;
    view.pump_until("the wrapped wide line", CATCH_UP, |grid| {
        grid.contains_text("TAIL-1")
    })
    .await;
    assert_converges_to_model(&mut view, &h, &window_id, "wrapped wide", CATCH_UP).await;

    let grid = &view.grid;
    let (row, col) = grid.find_cell("\u{5168}").expect("the wide cell");
    assert_eq!(
        (row, col),
        (1, 0),
        "the wide character moved to the next row"
    );
    assert!(
        grid.lines[0].wrapped,
        "the row it left goes on in the next row"
    );
    let last = grid.cell(0, 79);
    assert_eq!(last.width, 1, "the vacated last column is a one-wide cell");
    assert_eq!(last.text.trim(), "", "and it is blank");
}

#[tokio::test]
async fn a_resize_on_the_main_screen_is_followed_by_a_full_frame_that_matches_the_model() {
    let (h, window_id, mut view) = attached_script_window("view-resize-main").await;

    // The second line has no line break, so the cursor sits on a line the
    // window wrapped; the narrow and the wide resize both re-lay it out.
    let long = format!("{}END-A", "0123456789".repeat(10));
    script_write_and_wait(
        &h,
        &window_id,
        &bytes_of(&[CLEAR, b"FIRST-LINE\r\n", long.as_bytes()]),
        "END-A",
    )
    .await;
    view.pump_until("the long line", CATCH_UP, |grid| {
        grid.contains_text("END-A")
    })
    .await;
    assert_converges_to_model(&mut view, &h, &window_id, "before resize", CATCH_UP).await;

    for (cols, rows) in [(40u16, 24u16), (120, 30), (80, 24), (50, 10)] {
        view.send(&ClientMessage::Resize { cols, rows }).await;
        view.pump_until("the resized grid", CATCH_UP, |grid| {
            (grid.cols, grid.rows) == (cols, rows)
        })
        .await;
        assert_converges_to_model(
            &mut view,
            &h,
            &window_id,
            &format!("after resize to {cols}x{rows}"),
            CATCH_UP,
        )
        .await;
    }
}

#[tokio::test]
async fn a_resize_on_the_alternate_screen_and_the_return_to_the_main_screen_match_the_model() {
    let (h, window_id, mut view) = attached_script_window("view-resize-alt").await;

    script_write_and_wait(
        &h,
        &window_id,
        &bytes_of(&[CLEAR, b"MAIN-TEXT\r\n"]),
        "MAIN-TEXT",
    )
    .await;
    script_write_and_wait(
        &h,
        &window_id,
        &bytes_of(&[
            b"\x1b[?1049h",
            CLEAR,
            b"ALT-TOP\x1b[10;5HALT-MID\x1b[24;70HALT-END",
        ]),
        "ALT-END",
    )
    .await;
    view.pump_until("the alternate screen", CATCH_UP, |grid| {
        grid.view.alt_screen && grid.contains_text("ALT-END")
    })
    .await;
    assert_converges_to_model(&mut view, &h, &window_id, "alternate screen", CATCH_UP).await;

    view.send(&ClientMessage::Resize {
        cols: 100,
        rows: 30,
    })
    .await;
    view.pump_until("the resized alternate screen", CATCH_UP, |grid| {
        (grid.cols, grid.rows) == (100, 30)
    })
    .await;
    assert_converges_to_model(&mut view, &h, &window_id, "alt after resize", CATCH_UP).await;

    script_write_and_wait(
        &h,
        &window_id,
        &bytes_of(&[b"\x1b[?1049l", b"\x1b[H BACK-MAIN"]),
        "BACK-MAIN",
    )
    .await;
    view.pump_until("the main screen again", CATCH_UP, |grid| {
        !grid.view.alt_screen && grid.contains_text("BACK-MAIN")
    })
    .await;
    assert_converges_to_model(&mut view, &h, &window_id, "back on main", CATCH_UP).await;
}

#[tokio::test]
async fn output_that_scrolls_the_screen_is_followed_row_for_row() {
    let (h, window_id, mut view) = attached_script_window("view-scroll").await;

    script_write_and_wait(
        &h,
        &window_id,
        &bytes_of(&[CLEAR, &numbered_lines(1, 100), b"SCROLL-END"]),
        "SCROLL-END",
    )
    .await;
    view.pump_until("the scrolled screen", CATCH_UP, |grid| {
        grid.contains_text("SCROLL-END")
    })
    .await;
    assert_converges_to_model(&mut view, &h, &window_id, "scrolled", CATCH_UP).await;
    assert!(
        view.grid.contains_text("LINE-0100"),
        "the newest line is on screen:\n{}",
        view.grid.dump()
    );
    assert!(
        !view.grid.contains_text("LINE-0001"),
        "the oldest lines scrolled off the view:\n{}",
        view.grid.dump()
    );
}

#[tokio::test]
async fn several_bursts_written_back_to_back_end_in_the_models_picture() {
    let (h, window_id, mut view) = attached_script_window("view-bursts").await;

    script_write(&h, &window_id, CLEAR).await;
    for burst in 0..5usize {
        let mut bytes = numbered_lines(burst * 40 + 1, 40);
        bytes.extend_from_slice(format!("BURST-END-{burst}").as_bytes());
        script_write(&h, &window_id, &bytes).await;
    }
    common::wait_until_model_shows(&h, &window_id, b"BURST-END-4", CATCH_UP).await;
    view.pump_until("the last burst", CATCH_UP, |grid| {
        grid.contains_text("BURST-END-4")
    })
    .await;
    assert_converges_to_model(&mut view, &h, &window_id, "bursts", CATCH_UP).await;
}

#[tokio::test]
async fn a_change_confined_to_one_row_sends_only_that_row() {
    let (h, window_id, mut view) = attached_script_window("view-diff").await;

    script_write_and_wait(
        &h,
        &window_id,
        &bytes_of(&[CLEAR, b"ROW-A\r\nROW-B\r\nROW-C\r\n", b"SETTLED"]),
        "SETTLED",
    )
    .await;
    view.pump_until("the settled screen", CATCH_UP, |grid| {
        grid.contains_text("SETTLED")
    })
    .await;
    let frames_before = view.completed.len();

    script_write_and_wait(&h, &window_id, b"\x1b[2;1HROW-X", "ROW-X").await;
    view.pump_until("the changed row", CATCH_UP, |grid| {
        grid.contains_text("ROW-X")
    })
    .await;

    let later = &view.completed[frames_before..];
    assert!(!later.is_empty());
    for frame in later {
        assert!(
            !frame.reset,
            "a one-row change must not replace the whole grid: {frame:?}"
        );
        assert!(
            frame.rows_carried <= 1,
            "only the changed row travels: {frame:?}"
        );
    }
    assert_converges_to_model(&mut view, &h, &window_id, "after the diff", CATCH_UP).await;
}

#[tokio::test]
async fn colors_and_flags_are_carried_as_the_model_holds_them() {
    let (h, window_id, mut view) = attached_script_window("view-attrs").await;

    let styled: &[u8] = b"\x1b[31mR\x1b[0m\x1b[92mG\x1b[0m\x1b[38;5;196mS\x1b[0m\
\x1b[48;5;21mB\x1b[0m\x1b[38;2;10;20;30mT\x1b[0m\x1b[1;3;4;7mF\x1b[0m\
\x1b[2;9;8mH\x1b[0mD END-C";
    script_write_and_wait(&h, &window_id, &bytes_of(&[CLEAR, styled]), "END-C").await;
    view.pump_until("the styled line", CATCH_UP, |grid| {
        grid.contains_text("END-C")
    })
    .await;

    let grid = &view.grid;
    let attr_of = |text: &str| {
        let (row, col) = grid
            .find_cell(text)
            .unwrap_or_else(|| panic!("no cell {text:?}:\n{}", grid.dump()));
        grid.cell(row, col).attr.clone()
    };
    let red = attr_of("R");
    assert_eq!(red.fg, CellColor::Indexed(1), "SGR 31 is palette color 1");
    assert_eq!(red.bg, CellColor::Default);
    assert!(red.flags.is_empty());
    assert_eq!(
        attr_of("G").fg,
        CellColor::Indexed(10),
        "SGR 92 is palette color 10"
    );
    assert_eq!(attr_of("S").fg, CellColor::Indexed(196));
    assert_eq!(attr_of("B").bg, CellColor::Indexed(21));
    assert_eq!(attr_of("T").fg, CellColor::Rgb([10, 20, 30]));
    assert_eq!(
        attr_of("F").flags,
        [
            CellFlag::Bold,
            CellFlag::Italic,
            CellFlag::Underline,
            CellFlag::Inverse
        ]
        .into_iter()
        .collect::<BTreeSet<_>>(),
    );
    assert_eq!(
        attr_of("H").flags,
        [CellFlag::Dim, CellFlag::Strikeout, CellFlag::Hidden]
            .into_iter()
            .collect::<BTreeSet<_>>(),
    );
    assert_eq!(attr_of("D"), yagura_protocol::CellAttr::default());
}

#[tokio::test]
async fn the_cursor_position_visibility_and_shape_follow_the_model() {
    let (h, window_id, mut view) = attached_script_window("view-cursor").await;

    script_write_and_wait(&h, &window_id, &bytes_of(&[CLEAR, b"abc"]), "abc").await;
    view.pump_until("the cursor after abc", CATCH_UP, |grid| {
        grid.contains_text("abc") && grid.cursor.col == 3
    })
    .await;
    assert_eq!(view.grid.cursor.row, 0);
    assert!(view.grid.cursor.visible);

    script_write_and_wait(&h, &window_id, b"\x1b[?25lHIDDEN", "HIDDEN").await;
    view.pump_until("the hidden cursor", CATCH_UP, |grid| !grid.cursor.visible)
        .await;

    script_write_and_wait(&h, &window_id, b"\x1b[?25h\x1b[4 qUNDERLINE", "UNDERLINE").await;
    view.pump_until("the underline cursor", CATCH_UP, |grid| {
        grid.cursor.visible && grid.cursor.shape == CursorShape::Underline
    })
    .await;

    script_write_and_wait(&h, &window_id, b"\x1b[6 qBAR", "BAR").await;
    view.pump_until("the bar cursor", CATCH_UP, |grid| {
        grid.cursor.shape == CursorShape::Bar
    })
    .await;
    assert_converges_to_model(&mut view, &h, &window_id, "cursor", CATCH_UP).await;
}

#[tokio::test]
async fn a_color_the_program_sets_over_the_theme_travels_until_the_program_resets_it() {
    let (h, window_id, mut view) = attached_script_window("view-overrides").await;

    script_write(
        &h,
        &window_id,
        b"\x1b]11;rgb:ff/00/00\x1b\\\x1b]4;3;rgb:01/02/03\x1b\\\x1b]10;rgb:04/05/06\x1b\\\x1b]12;rgb:07/08/09\x1b\\",
    )
    .await;
    view.pump_until("the overrides", CATCH_UP, |grid| grid.overrides.len() == 4)
        .await;
    let find = |slot: PaletteSlot| {
        view.grid
            .overrides
            .iter()
            .find(|entry| entry.slot == slot)
            .map(|entry| entry.rgb)
    };
    assert_eq!(find(PaletteSlot::Background), Some([255, 0, 0]));
    assert_eq!(find(PaletteSlot::Indexed(3)), Some([1, 2, 3]));
    assert_eq!(find(PaletteSlot::Foreground), Some([4, 5, 6]));
    assert_eq!(find(PaletteSlot::Cursor), Some([7, 8, 9]));

    script_write(
        &h,
        &window_id,
        b"\x1b]111\x1b\\\x1b]104;3\x1b\\\x1b]110\x1b\\\x1b]112\x1b\\",
    )
    .await;
    view.pump_until("the overrides gone", CATCH_UP, |grid| {
        grid.overrides.is_empty()
    })
    .await;
}

#[tokio::test]
async fn nothing_but_a_ping_comes_back_while_the_screen_does_not_change() {
    let (h, window_id, mut view) = attached_script_window("view-quiet").await;

    script_write_and_wait(&h, &window_id, &bytes_of(&[CLEAR, b"STILL"]), "STILL").await;
    view.pump_until("the screen", CATCH_UP, |grid| grid.contains_text("STILL"))
        .await;
    view.assert_no_frame(
        "nothing was written since the last frame",
        Duration::from_millis(300),
    )
    .await;
}

#[tokio::test]
async fn attr_grid_cells_carry_the_colors_listed_by_hand() {
    let size = TerminalSize { cols: 12, rows: 8 };
    let h = Harness::start("view-attr-grid", DaemonConfig::default()).await;
    let window_id = h
        .open(
            vec![
                yagura_test_support::process::term_script_path()
                    .to_str()
                    .unwrap()
                    .to_string(),
                "attr-grid".to_string(),
                size.cols.to_string(),
                size.rows.to_string(),
            ],
            None,
        )
        .await;
    let mut view = ViewClient::attach(h.connect().await, &window_id, Some(size)).await;
    view.send_text("g").await;
    view.pump_until("the last cell painted", CATCH_UP, |grid| {
        grid.cell(size.rows - 1, size.cols - 1).text == "x"
    })
    .await;

    // (row, col, foreground, background): picked so that both halves of the
    // 16 colors, the boundary colors 7 and 8, and the wrap past 15 all occur.
    let expected: [(u16, u16, u8, u8); 10] = [
        (0, 0, 0, 0),
        (0, 1, 3, 1),
        (1, 0, 1, 5),
        (1, 1, 4, 6),
        (1, 3, 10, 8),
        (1, 5, 0, 10),
        (2, 2, 8, 12),
        (3, 3, 12, 2),
        (7, 0, 7, 3),
        (0, 7, 5, 7),
    ];
    for (row, col, fg, bg) in expected {
        let cell = view.grid.cell(row, col);
        assert_eq!(cell.text, "x", "cell ({row},{col})");
        assert_eq!(cell.attr.fg, CellColor::Indexed(fg), "fg of ({row},{col})");
        assert_eq!(cell.attr.bg, CellColor::Indexed(bg), "bg of ({row},{col})");
    }

    view.reattach_unacknowledged(&window_id, None).await;
    assert!(!view.child_exited, "the painting child is still alive");
}

#[tokio::test]
async fn a_bang_inside_a_script_command_is_ignored_and_does_not_end_the_child() {
    let (h, window_id, _view) = attached_script_window("view-script-bang").await;

    let mut input = b"<!".to_vec();
    input.extend(common::hex_of(b"BANG-OK").into_bytes());
    input.push(b'>');
    h.write_input(&window_id, &input).await;
    common::wait_until_model_shows(&h, &window_id, b"BANG-OK", CATCH_UP).await;
}
