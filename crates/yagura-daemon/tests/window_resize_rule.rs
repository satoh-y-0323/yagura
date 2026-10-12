#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! A window's model follows a resize by one rule on every system: the one a
//! Unix terminal uses. A reference model fed the very bytes the window's child
//! wrote and resized the same way must show what the window's model shows.

mod common;

use common::Harness;
use common::views::{model_screen, numbered_lines, open_script_window, script_write_and_wait};
use yagura_daemon::DaemonConfig;
use yagura_daemon::detector::VtScreen;

const CLEAR: &[u8] = b"\x1b[H\x1b[2J";

struct Shown {
    rows: Vec<String>,
    cursor_row: u16,
}

fn reference_shown(reference: &VtScreen) -> Shown {
    let (_, rows) = reference.size();
    Shown {
        rows: reference
            .bottom_lines(usize::from(rows))
            .into_iter()
            .map(|line| line.trim_end_matches(' ').to_string())
            .collect(),
        cursor_row: reference.cursor_row(),
    }
}

#[tokio::test]
async fn a_resized_window_shows_what_a_model_with_the_unix_rule_shows() {
    let config = DaemonConfig::default();
    let h = Harness::start("resize-rule", config.clone()).await;
    let window_id = open_script_window(&h).await;
    let session = h.registry.get(&window_id).unwrap().session().clone();

    let mut bytes = CLEAR.to_vec();
    bytes.extend(numbered_lines(1, 40));
    bytes.extend_from_slice(b"TAIL-MARK");
    script_write_and_wait(&h, &window_id, &bytes, "TAIL-MARK").await;

    let captured = h.captured_output(&window_id);
    let mut reference = VtScreen::with_history(80, 24, config.read_output_max_lines);
    reference.feed(&captured);

    let before = model_screen(&h, &window_id).await;
    let shown = reference_shown(&reference);
    assert_eq!(
        (before.rows, before.cursor_row),
        (shown.rows, shown.cursor_row),
        "before any resize the reference shows what the window shows"
    );

    for (cols, rows) in [(80u16, 12u16), (80, 30), (40, 30), (100, 24), (30, 8)] {
        session.resize(cols, rows).await.expect("resize");
        reference.resize(cols, rows);

        let window = model_screen(&h, &window_id).await;
        let shown = reference_shown(&reference);
        assert_eq!(window.rows, shown.rows, "rows at {cols}x{rows}");
        assert_eq!(
            window.cursor_row, shown.cursor_row,
            "cursor row at {cols}x{rows}"
        );
        assert_eq!(
            session.text_tail(100, 64 * 1024).await.unwrap().0,
            reference.text_tail(100, 64 * 1024).0,
            "text at {cols}x{rows}"
        );
    }
}
