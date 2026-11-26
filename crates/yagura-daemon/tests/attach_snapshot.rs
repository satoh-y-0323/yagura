#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! A GUI that (re)connects to a window gets the window's screen model as a full frame
//! right after `attached`: the cells of the window's size show what the window shows,
//! however much the child wrote before, and at the size the GUI asked for.
//!
//! The windows run this test binary itself as their child (see [`common::own_test_child_command`]), so
//! the child's output is exact on every platform without a shell in the way.

use crate::common;
use crate::common::bins;

use std::io::{Read, Write};
use std::time::Duration;

use common::views::{cr_burst_payload, grid_differs_from_model, model_screen};
use common::{Harness, expect_error, read_response, send};
use yagura_daemon::{DaemonConfig, PtySession};
use yagura_protocol::{CellColor, CellFlag, ClientMessage, TerminalSize};
use yagura_test_support::HANG_GUARD;
use yagura_test_support::frame::Grid;
use yagura_test_support::view::ViewClient;

const NO_LIMIT: usize = usize::MAX;
const ALTERNATE_CHILD: &str = "alternate_screen_child";
const EXITING_CHILD: &str = "exiting_child";
const MARKER: &str = "ALT-SNAPSHOT-MARKER";
/// What [`alternate_screen_child`] writes last.
const LAST_TEXT: &str = "second row";
const EXIT_MARKER: &str = "EXITING-CHILD-MARKER";
/// More than the 2 MiB the daemon used to keep of a window's raw output.
const FILLER_BYTES: usize = 5 * 1024 * 1024 / 2;

fn wait_for_end_of_input() {
    let mut buf = [0u8; 64];
    while std::io::stdin().lock().read(&mut buf).unwrap_or(0) != 0 {}
}

#[test]
#[ignore = "the child of the tests below, started by common::own_test_child_command"]
fn alternate_screen_child() {
    if !common::running_as_own_test(module_path!(), ALTERNATE_CHILD) {
        return;
    }
    let mut out = b"main before 1\r\nmain before 2\r\n\x1b[?1049h".to_vec();
    let mut n = 0usize;
    while out.len() < FILLER_BYTES {
        out.extend_from_slice(
            format!("filler {n:08} ----------------------------------\r\n").as_bytes(),
        );
        n += 1;
    }
    out.extend_from_slice(b"\x1b[H\x1b[2J\x1b[1;35m");
    out.extend_from_slice(MARKER.as_bytes());
    // The modes come before the last text, so a model that shows the last
    // text already holds everything this child writes.
    out.extend_from_slice(b"\x1b[0m\x1b[?2004h\x1b[?25l\x1b[3;4H");
    out.extend_from_slice(LAST_TEXT.as_bytes());
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(&out).unwrap();
    stdout.flush().unwrap();
    drop(stdout);
    wait_for_end_of_input();
}

#[test]
#[ignore = "the child of the tests below, started by common::own_test_child_command"]
fn exiting_child() {
    if !common::running_as_own_test(module_path!(), EXITING_CHILD) {
        return;
    }
    let mut stdout = std::io::stdout().lock();
    stdout
        .write_all(format!("\x1b[1;33m{EXIT_MARKER}\x1b[0m\r\n").as_bytes())
        .unwrap();
    stdout.flush().unwrap();
    drop(stdout);
    let mut buf = [0u8; 1];
    let _ = std::io::stdin().lock().read(&mut buf);
    std::process::exit(0);
}

fn session(h: &Harness, window_id: &str) -> std::sync::Arc<PtySession> {
    h.registry
        .get(window_id)
        .expect("window must exist in registry")
        .session()
        .clone()
}

/// Waits until the window's text is `expected`. The text only moves towards
/// what the child last wrote, so reaching `expected` is an event.
async fn wait_until_window_text_is(session: &PtySession, expected: &str) {
    let deadline = tokio::time::Instant::now() + HANG_GUARD;
    loop {
        let now = session.text_tail(NO_LIMIT, NO_LIMIT).await.unwrap().0;
        if now == expected {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the window's text never became the grid's; last text ends {:?}",
            now.chars().rev().take(200).collect::<String>()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// The grid's rows as the model's `text_tail` writes a screen with nothing
/// scrolled off: trailing blank rows dropped, rows joined by a line feed.
fn text_of(grid: &Grid) -> String {
    let mut rows = grid.screen_text();
    while rows.last().is_some_and(String::is_empty) {
        rows.pop();
    }
    rows.join("\n")
}

async fn open_alternate_screen_window(h: &Harness) -> (String, std::sync::Arc<PtySession>) {
    let window_id = h
        .open(
            common::own_test_child_command(module_path!(), ALTERNATE_CHILD),
            None,
        )
        .await;
    let session = session(h, &window_id);
    common::views::wait_until_copy_shows(h, &window_id, LAST_TEXT.as_bytes()).await;
    (window_id, session)
}

#[tokio::test]
async fn attaching_without_a_size_shows_what_the_window_shows_and_keeps_its_size() {
    let h = Harness::start("attach-snapshot-same-size", DaemonConfig::default()).await;
    let (window_id, session) = open_alternate_screen_window(&h).await;
    let size_before = session.size().await.unwrap();
    let shown = session.text_tail(NO_LIMIT, NO_LIMIT).await.unwrap();
    assert!(shown.0.contains(MARKER), "{shown:?}");

    let view = ViewClient::attach(h.connect().await, &window_id, None).await;
    assert!(!view.child_exited);
    assert_eq!(
        session.size().await.unwrap(),
        size_before,
        "no size asked, so none may change"
    );

    let grid = &view.grid;
    assert_eq!((grid.cols, grid.rows), size_before);
    assert_eq!(text_of(grid), shown.0);
    assert_eq!(session.text_tail(NO_LIMIT, NO_LIMIT).await.unwrap(), shown);
    assert!(!grid.cursor.visible, "the hidden cursor must stay hidden");
    assert!(grid.view.alt_screen);
    assert_eq!(grid.row_text(2), format!("   {LAST_TEXT}"));

    let marker_cell = grid.cell(0, 0);
    assert_eq!(marker_cell.text, "A");
    assert_eq!(marker_cell.attr.fg, CellColor::Indexed(5));
    assert!(marker_cell.attr.flags.contains(&CellFlag::Bold));
    assert_eq!(
        grid_differs_from_model(grid, &model_screen(&h, &window_id).await),
        None
    );
}

#[tokio::test]
async fn attaching_with_a_size_resizes_the_window_before_drawing_it() {
    let h = Harness::start("attach-snapshot-resize", DaemonConfig::default()).await;
    let (window_id, session) = open_alternate_screen_window(&h).await;
    let asked = TerminalSize {
        cols: 101,
        rows: 31,
    };
    assert_ne!(session.size().await.unwrap(), (asked.cols, asked.rows));

    let view = ViewClient::attach(h.connect().await, &window_id, Some(asked)).await;
    assert!(!view.child_exited);
    assert_eq!(session.size().await.unwrap(), (asked.cols, asked.rows));

    let grid = &view.grid;
    assert_eq!((grid.cols, grid.rows), (asked.cols, asked.rows));
    assert!(grid.contains_text(MARKER), "{}", grid.dump());
    wait_until_window_text_is(&session, &text_of(grid)).await;
    assert!(!grid.cursor.visible);
}

#[tokio::test]
async fn attaching_to_an_exited_window_keeps_its_size_and_still_draws_it() {
    let h = Harness::start("attach-snapshot-exited", DaemonConfig::default()).await;
    let window_id = h
        .open(
            common::own_test_child_command(module_path!(), EXITING_CHILD),
            None,
        )
        .await;
    let session = session(&h, &window_id);
    common::views::wait_until_copy_shows(&h, &window_id, EXIT_MARKER.as_bytes()).await;
    h.write_input(&window_id, b"q\r").await;
    h.wait_until_child_exited(&window_id).await;
    let size_before = session.size().await.unwrap();

    let view = ViewClient::attach(
        h.connect().await,
        &window_id,
        Some(TerminalSize {
            cols: size_before.0 + 7,
            rows: size_before.1 + 3,
        }),
    )
    .await;
    assert!(view.child_exited);
    assert_eq!(view.exit_code, Some(0));
    assert_eq!(
        session.size().await.unwrap(),
        size_before,
        "an exited window is not resized"
    );
    assert_eq!((view.grid.cols, view.grid.rows), size_before);
    assert!(view.grid.contains_text(EXIT_MARKER), "{}", view.grid.dump());
}

#[tokio::test]
async fn attaching_at_a_size_out_of_range_is_invalid_argument_and_changes_nothing() {
    let h = Harness::start("attach-snapshot-out-of-range", DaemonConfig::default()).await;
    let window_id = h.open(bins::echo_child_command(), None).await;
    let session = session(&h, &window_id);
    let size_before = session.size().await.unwrap();
    let too_large = yagura_protocol::MAX_WINDOW_DIMENSION + 1;

    for (cols, rows) in [(0, 24), (80, 0), (too_large, 24), (80, too_large)] {
        let mut client = h.connect().await;
        send(
            &mut client,
            &ClientMessage::AttachView {
                window_id: window_id.clone(),
                size: Some(TerminalSize { cols, rows }),
            },
        )
        .await;
        let reply = read_response(&mut client).await;
        expect_error(reply, "invalid_argument");
        assert_eq!(
            session.size().await.unwrap(),
            size_before,
            "{cols}x{rows} must change nothing"
        );
    }
}

#[tokio::test]
async fn a_burst_that_outruns_the_acknowledgements_ends_on_the_latest_screen() {
    let h = Harness::start("attach-snapshot-burst", DaemonConfig::default()).await;
    let window_id = h.open(bins::echo_child_command(), None).await;
    let mut view = ViewClient::attach_unacknowledged(h.connect().await, &window_id, None).await;
    let first = view.grid.last_seq.expect("the first frame completed");

    view.send(&ClientMessage::Paste {
        text: String::from_utf8(cr_burst_payload("burst-tail;")).unwrap(),
    })
    .await;
    common::views::wait_until_copy_shows(&h, &window_id, b"burst-tail;").await;
    view.assert_no_frame("the first frame is unacknowledged however much the window wrote")
        .await;

    view.ack(first).await;
    view.next_complete_frame().await;
    assert!(
        view.grid.contains_text("burst-tail;"),
        "{}",
        view.grid.dump()
    );
    assert_eq!(
        grid_differs_from_model(&view.grid, &model_screen(&h, &window_id).await),
        None,
        "the first frame after the acknowledgement is the latest screen"
    );
    assert_eq!(view.completed.len(), 2);
}

#[tokio::test]
async fn a_failed_attach_leaves_both_connections_and_the_window_as_they_were() {
    let h = Harness::start("attach-snapshot-failed-attach", DaemonConfig::default()).await;
    let held = h.open(bins::echo_child_command(), None).await;
    let other = h.open(bins::echo_child_command(), None).await;
    let held_session = session(&h, &held);
    let size_before = held_session.size().await.unwrap();

    let mut holder = ViewClient::attach(h.connect().await, &held, None).await;
    let mut mover = ViewClient::attach(h.connect().await, &other, None).await;

    held_session.fail_next_resize();
    let asked = TerminalSize {
        cols: size_before.0 + 9,
        rows: size_before.1 + 4,
    };
    send(
        &mut mover.conn,
        &ClientMessage::AttachView {
            window_id: held.clone(),
            size: Some(asked),
        },
    )
    .await;
    expect_error(read_response(&mut mover.conn).await, "internal");
    assert_eq!(
        held_session.size().await.unwrap(),
        size_before,
        "the refused size must not stay"
    );

    // The holder is not superseded and still receives its window's output.
    h.write_input(&held, b"to-held\r").await;
    holder
        .pump_until("the holder's window output", |grid| {
            grid.contains_text("to-held")
        })
        .await;

    // The mover is still attached to its own window: its input goes there and only
    // that window's output reaches it.
    mover
        .send(&ClientMessage::Text {
            text: "from-mover\r".to_string(),
        })
        .await;
    mover
        .pump_until("the mover's window output", |grid| {
            grid.contains_text("from-mover")
        })
        .await;
    assert!(
        !mover.grid.contains_text("to-held"),
        "{}",
        mover.grid.dump()
    );
    assert!(
        !held_session
            .text_tail(NO_LIMIT, NO_LIMIT)
            .await
            .unwrap()
            .0
            .contains("from-mover"),
        "the mover's input must not reach the window it failed to attach to"
    );
}
