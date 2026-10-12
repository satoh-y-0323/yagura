#![cfg(windows)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! A Windows window runs under the bundled ConPTY, which sends nothing
//! downstream on a resize and learns the cursor from the daemon's answer to a
//! cursor position report. With a real PowerShell child, the daemon's model
//! must then follow resizes the way it does on Unix: nothing is duplicated
//! however fast the window is resized, the frames a view applies show the
//! model, and the cursor the child reads back is the model's.

mod common;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use common::Harness;
use common::views::{CATCH_UP, assert_converges_to_model};
use yagura_daemon::{DaemonConfig, PtySession};
use yagura_protocol::{ClientMessage, TerminalSize};
use yagura_test_support::view::ViewClient;

const START: TerminalSize = TerminalSize { cols: 80, rows: 24 };

fn powershell_path() -> String {
    let root = std::env::var("SystemRoot").expect("SystemRoot");
    PathBuf::from(root)
        .join(r"System32\WindowsPowerShell\v1.0\powershell.exe")
        .to_str()
        .expect("utf-8 path")
        .to_string()
}

fn powershell_argv(script: &str) -> Vec<String> {
    vec![
        powershell_path(),
        "-NoLogo".into(),
        "-NoProfile".into(),
        "-NonInteractive".into(),
        "-Command".into(),
        script.into(),
    ]
}

async fn open_attached(
    tag: &str,
    script: &str,
) -> (Harness, String, std::sync::Arc<PtySession>, ViewClient) {
    let h = Harness::start(tag, DaemonConfig::default()).await;
    let window_id = h.open(powershell_argv(script), None).await;
    let session = h.registry.get(&window_id).unwrap().session().clone();
    let view = ViewClient::attach(h.connect().await, &window_id, Some(START)).await;
    (h, window_id, session, view)
}

async fn read_text(session: &PtySession) -> String {
    session.text_tail(100, 64 * 1024).await.unwrap().0
}

async fn wait_for_text(session: &PtySession, expected: impl Fn(&str) -> bool, what: &str) {
    let deadline = Instant::now() + CATCH_UP;
    loop {
        let text = read_text(session).await;
        if expected(&text) {
            return;
        }
        assert!(Instant::now() < deadline, "{what}; the text is {text:?}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Thirty lines, one long line that wraps at every width used here, a short one.
fn child_lines() -> Vec<String> {
    let mut lines: Vec<String> = (1..=30).map(|n| format!("history line {n}")).collect();
    let numbers: Vec<String> = (1..=80).map(|n| format!("{n:03}")).collect();
    lines.push(numbers.join("."));
    lines.push("after-seq".to_string());
    lines
}

const PRINT_ALL_THEN_WAIT: &str = "1..30|%{'history line '+$_}; \
     (1..80|%{'{0:d3}' -f $_}) -join '.'; 'after-seq'; [void][Console]::ReadKey($true)";

/// Narrows and shortens by two columns and one row at a time, then widens and
/// grows the same way: the sizes a dragged window edge passes through.
fn drag_path() -> Vec<(u16, u16)> {
    let mut path = Vec::new();
    let (mut cols, mut rows) = (80u16, 24u16);
    while (cols, rows) != (40, 12) {
        cols = (cols - 2).max(40);
        rows = (rows - 1).max(12);
        path.push((cols, rows));
    }
    while (cols, rows) != (110, 30) {
        cols = (cols + 2).min(110);
        rows = (rows + 1).min(30);
        path.push((cols, rows));
    }
    path
}

async fn drag_the_window_edge(tag: &str, pause: Duration) {
    let (h, window_id, session, mut view) = open_attached(tag, PRINT_ALL_THEN_WAIT).await;
    let expected = child_lines().join("\n");
    wait_for_text(&session, |text| text == expected, "the child's lines").await;

    for (cols, rows) in drag_path() {
        // Through the view, the way the GUI resizes, so the view hears of it.
        view.send(&ClientMessage::Resize { cols, rows }).await;
        if !pause.is_zero() {
            tokio::time::sleep(pause).await;
        }
    }
    view.sync().await;

    wait_for_text(
        &session,
        |text| text == expected,
        "every row reads once after the drag",
    )
    .await;
    view.pump_until("the dragged size", CATCH_UP, |grid| {
        (grid.cols, grid.rows) == (110, 30)
    })
    .await;
    assert_converges_to_model(&mut view, &h, &window_id, "after the drag", CATCH_UP).await;
}

#[tokio::test]
async fn back_to_back_resizes_leave_no_repeated_row_and_the_view_matches_the_model() {
    drag_the_window_edge("bundled-drag-0", Duration::ZERO).await;
}

#[tokio::test]
async fn resizes_a_few_milliseconds_apart_leave_no_repeated_row_and_the_view_matches_the_model() {
    drag_the_window_edge("bundled-drag-3", Duration::from_millis(3)).await;
}

/// Prints a long prompt (it wraps once the window is narrow), then for every
/// key it reads reports the cursor and size the console API gives it.
const REPORT_CURSOR_ON_KEY: &str = "1..30|%{'history line '+$_}; \
     $p='P'+('x'*44)+'> '; \
     while($true){ [Console]::Write($p); [void][Console]::ReadKey($true); \
     $t=[Console]::CursorTop; $l=[Console]::CursorLeft; \
     $w=[Console]::WindowWidth; $h=[Console]::WindowHeight; \
     [Console]::Write([Environment]::NewLine+'CUR='+$t+','+$l+';SZ='+$w+','+$h+';'+[Environment]::NewLine) }";

#[derive(Debug, PartialEq, Eq)]
struct Report {
    row: u16,
    col: u16,
    cols: u16,
    rows: u16,
}

fn reports_in(text: &str) -> Vec<Report> {
    text.split("CUR=")
        .skip(1)
        .filter_map(|rest| {
            let (cursor, rest) = rest.split_once(";SZ=")?;
            let (size, _) = rest.split_once(';')?;
            let (row, col) = cursor.split_once(',')?;
            let (cols, rows) = size.split_once(',')?;
            Some(Report {
                row: row.parse().ok()?,
                col: col.parse().ok()?,
                cols: cols.parse().ok()?,
                rows: rows.parse().ok()?,
            })
        })
        .collect()
}

#[tokio::test]
async fn the_cursor_the_child_reads_after_a_resize_is_the_models() {
    let (h, window_id, session, mut view) =
        open_attached("bundled-cursor", REPORT_CURSOR_ON_KEY).await;
    wait_for_text(&session, |text| text.contains("x>"), "the prompt").await;

    for (cols, rows) in [(40u16, 12u16), (100, 30), (25, 10), (80, 24)] {
        view.send(&ClientMessage::Resize { cols, rows }).await;
        view.pump_until("the resized grid", CATCH_UP, |grid| {
            (grid.cols, grid.rows) == (cols, rows)
        })
        .await;

        // The console may not have taken the new size yet when a key arrives;
        // a report that still shows the old size says nothing, so ask again.
        let mut asked = 0;
        loop {
            assert!(
                asked < 40,
                "the console never reported {cols}x{rows}; reports: {:?}",
                reports_in(&read_text(&session).await)
            );
            asked += 1;
            // The child prints its prompt after each report; the model's
            // cursor is only the one the child will read once that has landed.
            wait_for_text(
                &session,
                |text| text.trim_end().ends_with("x>"),
                "the prompt",
            )
            .await;
            view.sync().await;
            assert_converges_to_model(&mut view, &h, &window_id, "before the key", CATCH_UP).await;
            let model_cursor = (view.grid.cursor.row, view.grid.cursor.col);
            let before = reports_in(&read_text(&session).await).len();
            h.write_input(&window_id, b"k").await;
            wait_for_text(
                &session,
                |text| reports_in(text).len() > before,
                "a report for the key",
            )
            .await;
            let reports = reports_in(&read_text(&session).await);
            let report = reports.last().unwrap();
            if (report.cols, report.rows) != (cols, rows) {
                continue;
            }
            assert_eq!(
                (report.row, report.col),
                model_cursor,
                "after resizing to {cols}x{rows}: the cursor the child read vs the model's"
            );
            break;
        }
    }
}
