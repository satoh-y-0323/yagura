#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! A whole screen that does not fit one frame's byte budget is sent as
//! several parts split by row range. The screen here is the largest a window
//! can be (`MAX_WINDOW_DIMENSION` both ways) with a different pair of colors
//! in every neighbouring cell, so every cell is its own run.

mod common;

use std::time::Duration;

use common::Harness;
use common::views::{grid_differs_from_model, model_screen};
use tokio::time::Instant;
use yagura_daemon::DaemonConfig;
use yagura_protocol::{
    CellColor, DaemonMessage, FRAME_BUDGET_BYTES, MAX_WINDOW_DIMENSION, TerminalSize,
    encode_daemon_message,
};
use yagura_test_support::view::ViewClient;

/// The color `term_script attr-grid` gives cell (`row`, `col`) as foreground.
fn fg_of(row: usize, col: usize) -> u8 {
    u8::try_from((row + 3 * col) % 16).unwrap()
}

/// The color `term_script attr-grid` gives cell (`row`, `col`) as background.
fn bg_of(row: usize, col: usize) -> u8 {
    u8::try_from((5 * row + col) % 16).unwrap()
}

#[tokio::test]
async fn a_full_frame_over_the_byte_budget_arrives_in_parts_that_rebuild_the_screen() {
    let side = MAX_WINDOW_DIMENSION;
    let size = TerminalSize {
        cols: side,
        rows: side,
    };
    let h = Harness::start("view-split", DaemonConfig::default()).await;
    let window_id = h
        .open(
            vec![
                yagura_test_support::process::term_script_path()
                    .to_str()
                    .unwrap()
                    .to_string(),
                "attr-grid".to_string(),
                side.to_string(),
                side.to_string(),
            ],
            None,
        )
        .await;

    // Size the window first, then let the child paint: it paints at the first
    // input byte, so it paints the size the attach asked for.
    let first = ViewClient::attach_unacknowledged(h.connect().await, &window_id, Some(size)).await;
    assert_eq!((first.grid.cols, first.grid.rows), (side, side));
    h.write_input(&window_id, b"g").await;

    let painted = Instant::now() + Duration::from_secs(180);
    loop {
        let model = model_screen(&h, &window_id).await;
        let last_row = model.rows.last().expect("the model has rows");
        if last_row.len() == usize::from(side) && last_row.chars().all(|c| c == 'x') {
            break;
        }
        assert!(
            Instant::now() < painted,
            "the child never finished painting"
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    drop(first);

    // A new attach sends the whole screen again.
    let view = ViewClient::attach_unacknowledged(h.connect().await, &window_id, None).await;
    let frame = view
        .completed
        .last()
        .expect("the attach frame completed")
        .clone();
    assert!(frame.reset);
    assert!(
        frame.parts >= 2,
        "{} cells of one run each cannot fit {FRAME_BUDGET_BYTES} bytes, so the frame is split; \
         it came in {} part(s)",
        u32::from(side) * u32::from(side),
        frame.parts
    );
    assert_eq!(
        frame.rows_carried,
        usize::from(side),
        "every row exactly once"
    );

    let parts = &view.last_frame_parts;
    assert_eq!(parts.len(), frame.parts);
    assert!(parts.iter().all(|part| part.seq == parts[0].seq));
    assert!(parts[0].reset, "the first part replaces the grid");
    assert!(parts[1..].iter().all(|part| !part.reset));
    assert!(
        parts[..parts.len() - 1].iter().all(|part| !part.last),
        "only the last part is marked as the last"
    );
    assert!(parts[parts.len() - 1].last);
    for part in parts {
        let size = encode_daemon_message(&DaemonMessage::Frame(part.clone())).len();
        assert!(
            size <= FRAME_BUDGET_BYTES,
            "a part is {size} bytes, over the budget of {FRAME_BUDGET_BYTES}"
        );
    }
    let mut rows: Vec<u16> = parts
        .iter()
        .flat_map(|part| part.lines.iter().map(|line| line.row))
        .collect();
    rows.sort_unstable();
    assert_eq!(
        rows,
        (0..side).collect::<Vec<u16>>(),
        "the parts split the rows"
    );

    let model = model_screen(&h, &window_id).await;
    assert_eq!(
        grid_differs_from_model(&view.grid, &model),
        None,
        "the rebuilt grid is the model's picture"
    );
    for row in 0..side {
        for col in 0..side {
            let cell = view.grid.cell(row, col);
            let (r, c) = (usize::from(row), usize::from(col));
            assert_eq!(cell.text, "x", "cell ({row},{col})");
            assert_eq!(
                cell.attr.fg,
                CellColor::Indexed(fg_of(r, c)),
                "foreground of ({row},{col})"
            );
            assert_eq!(
                cell.attr.bg,
                CellColor::Indexed(bg_of(r, c)),
                "background of ({row},{col})"
            );
            assert!(cell.attr.flags.is_empty(), "flags of ({row},{col})");
        }
    }
}
