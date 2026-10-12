#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: the daemon is the only terminal a window has, so it answers the
//! child's terminal queries (DA1, DA2, DSR, CPR, DECRQM) from its own model, with no GUI
//! attached.
//!
//! The child is `hex_echo --emit <query>...`: it writes the queries to its terminal and
//! prints every byte it then reads from stdin as hex, so the reply the child received is
//! read back from the window's raw output.

mod common;

use std::time::Duration;

use common::{Harness, assert_hex_echo_receives, contains_subsequence, hex_echo_path, hex_of};
use yagura_daemon::DaemonConfig;

const EMITTED_MARKER: &[u8] = b"QUERIES-EMITTED";
const FLOODED_MARKER: &[u8] = b"QUERIES-FLOODED";
const REPLY_TIMEOUT: Duration = Duration::from_secs(10);

const DA1: &[u8] = b"\x1b[c";
const DA2: &[u8] = b"\x1b[>c";
const DSR: &[u8] = b"\x1b[5n";
const CPR: &[u8] = b"\x1b[6n";

fn hex_echo_command(mode: &str, args: &[String]) -> Vec<String> {
    let mut command = vec![
        hex_echo_path()
            .to_str()
            .expect("hex_echo path must be valid UTF-8")
            .to_string(),
        mode.to_string(),
    ];
    command.extend(args.iter().cloned());
    command
}

/// Opens a `hex_echo` window that emits `queries` (in order) once it starts, and waits
/// until the child says it has written them all.
async fn open_emitting_window(h: &Harness, queries: &[&[u8]]) -> String {
    let args: Vec<String> = queries.iter().map(|query| hex_of(query)).collect();
    let window_id = h.open(hex_echo_command("--emit", &args), None).await;
    h.wait_for_scrollback(&window_id, EMITTED_MARKER, REPLY_TIMEOUT)
        .await;
    window_id
}

async fn assert_query_answered(tag: &str, queries: &[&[u8]], expected_reply: &[u8]) {
    let h = Harness::start(tag, DaemonConfig::default()).await;
    let window_id = open_emitting_window(&h, queries).await;
    assert_hex_echo_receives(
        &h,
        &window_id,
        EMITTED_MARKER,
        expected_reply,
        REPLY_TIMEOUT,
    )
    .await;
}

#[tokio::test]
async fn primary_device_attributes_are_answered_as_a_vt100_with_advanced_video() {
    assert_query_answered("query-da1", &[DA1], b"\x1b[?1;2c").await;
}

#[tokio::test]
async fn secondary_device_attributes_are_answered_with_the_terminal_identity() {
    assert_query_answered("query-da2", &[DA2], b"\x1b[>0;276;0c").await;
}

#[tokio::test]
async fn device_status_report_is_answered_as_ready() {
    assert_query_answered("query-dsr", &[DSR], b"\x1b[0n").await;
}

#[tokio::test]
async fn cursor_position_report_gives_the_models_cursor_one_based() {
    assert_query_answered("query-cpr", &[b"\x1b[4;7H", CPR], b"\x1b[4;7R").await;
}

#[tokio::test]
async fn cursor_position_report_follows_the_cursor_after_text() {
    // Row 1 is "CTRL-READY"; the cursor then sits at row 2, column 1 plus the text.
    assert_query_answered("query-cpr-text", &[b"abcde", CPR], b"\x1b[2;6R").await;
}

#[tokio::test]
async fn mode_report_says_set_for_the_alternate_screen_once_entered() {
    assert_query_answered(
        "query-decrqm-1049-set",
        &[b"\x1b[?1049h", b"\x1b[?1049$p"],
        b"\x1b[?1049;1$y",
    )
    .await;
}

#[tokio::test]
async fn mode_report_reads_an_ansi_mode_from_the_model() {
    assert_query_answered(
        "query-decrqm-insert",
        &[b"\x1b[4h", b"\x1b[4$p"],
        b"\x1b[4;1$y",
    )
    .await;
}

#[tokio::test]
async fn mode_report_says_reset_for_the_alternate_screen_before_it_is_entered() {
    assert_query_answered(
        "query-decrqm-1049-reset",
        &[b"\x1b[?1049$p"],
        b"\x1b[?1049;2$y",
    )
    .await;
}

#[tokio::test]
async fn mode_report_says_reset_for_the_alternate_screen_after_it_is_left() {
    assert_query_answered(
        "query-decrqm-1049-left",
        &[b"\x1b[?1049h", b"\x1b[?1049l", b"\x1b[?1049$p"],
        b"\x1b[?1049;2$y",
    )
    .await;
}

#[tokio::test]
async fn mode_report_reads_x10_mouse_from_the_model() {
    assert_query_answered(
        "query-decrqm-x10-set",
        &[b"\x1b[?9h", b"\x1b[?9$p"],
        b"\x1b[?9;1$y",
    )
    .await;
}

#[tokio::test]
async fn mode_report_says_reset_for_x10_mouse_never_set() {
    assert_query_answered("query-decrqm-x10-reset", &[b"\x1b[?9$p"], b"\x1b[?9;2$y").await;
}

#[tokio::test]
async fn mode_report_reads_mode_47_from_the_model() {
    assert_query_answered(
        "query-decrqm-47-set",
        &[b"\x1b[?47h", b"\x1b[?47$p"],
        b"\x1b[?47;1$y",
    )
    .await;
}

#[tokio::test]
async fn mode_report_reads_mode_1047_from_the_model() {
    assert_query_answered(
        "query-decrqm-1047-set",
        &[b"\x1b[?1047h", b"\x1b[?1047$p"],
        b"\x1b[?1047;1$y",
    )
    .await;
}

#[tokio::test]
async fn mode_report_reads_cursor_keys_mode_from_the_model() {
    assert_query_answered(
        "query-decrqm-decckm-set",
        &[b"\x1b[?1h", b"\x1b[?1$p"],
        b"\x1b[?1;1$y",
    )
    .await;
}

#[tokio::test]
async fn mode_report_says_not_recognized_for_an_unknown_mode() {
    assert_query_answered(
        "query-decrqm-unknown",
        &[b"\x1b[?7777$p"],
        b"\x1b[?7777;0$y",
    )
    .await;
}

#[tokio::test]
async fn replies_are_not_counted_as_the_users_writes() {
    let h = Harness::start("query-not-a-write", DaemonConfig::default()).await;
    let window_id = open_emitting_window(&h, &[DA1]).await;
    assert_hex_echo_receives(&h, &window_id, EMITTED_MARKER, b"\x1b[?1;2c", REPLY_TIMEOUT).await;
    let writes = h
        .registry
        .get(&window_id)
        .expect("window must exist in registry")
        .session()
        .writes_recorded();
    assert_eq!(writes, 0, "a reply to a query must not count as a write");
}

#[tokio::test]
async fn a_child_flooding_queries_without_reading_does_not_stop_the_reader() {
    // The replies far outgrow any PTY input buffer while the child is not reading, so a
    // reader that wrote them itself would block and the marker after the flood never
    // shows. Once the child reads again, the replies the bounded queue kept arrive.
    let h = Harness::start("query-flood", DaemonConfig::default()).await;
    let window_id = h
        .open(
            hex_echo_command("--flood", &[hex_of(DA1), "20000".to_string()]),
            None,
        )
        .await;
    // The echo of the replies that follows the marker outgrows the scrollback
    // tail, so the marker is looked for in everything the child wrote.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while !contains_subsequence(&h.captured_output(&window_id), FLOODED_MARKER) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the child's marker after the flood never showed"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_hex_echo_receives(&h, &window_id, FLOODED_MARKER, b"\x1b[?1;2c", REPLY_TIMEOUT).await;
}
