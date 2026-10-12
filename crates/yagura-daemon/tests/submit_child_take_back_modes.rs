#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Self-check for `submit_child`'s take-back modes (`clear_scope`): pins
//! what the fixture itself does, independent of the window-layer judgment
//! that builds on top of it (`ADR` naming and every other non-test-fixture
//! concern lives there, not here).
//!
//! Every case drives a real `WindowRegistry` and writes bytes straight
//! through `WindowEntry::write_pty_input`, then reads the composer back off
//! the child's own screen (`VtScreen::read_composer`), the same way
//! `composer_gate.rs` verifies the window layer's writes.

mod common;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use common::{
    Harness, approver_base_config, open_submit_child_window_with_clear_scope,
    paste_buffered_values, unique_temp_dir, wait_until_composer_is,
};
use yagura_daemon::DaemonConfig;
use yagura_daemon::detector::screen::ComposerReadout;
use yagura_daemon::window::{InputOrigin, WindowEntry};

/// A [`DaemonConfig`] whose detection directory is empty: every case writes
/// its own definition under it through
/// [`common::open_submit_child_window_with_clear_scope`].
fn detection_config(dir: &Path) -> DaemonConfig {
    DaemonConfig {
        detection_dir: Some(dir.to_path_buf()),
        ..approver_base_config()
    }
}

/// The `[input]` table every case here writes: `submit_delay_ms` clears the
/// definition loader's own requirement of outlasting `[debounce].confirm_ms`
/// (30, baked into [`common::write_submit_definition_with_input_toml`]).
fn input_toml(clear: &str) -> String {
    format!(
        "bracketed_paste = false\n\
         submit_delay_ms = 120\n\
         clear = \"{clear}\"\n\
         delivery_confirm_ms = 0\n\
         resend_max = 0\n"
    )
}

fn entry(h: &Harness, window_id: &str) -> Arc<WindowEntry> {
    h.registry
        .get(window_id)
        .expect("window must exist in registry")
}

async fn write(h: &Harness, window_id: &str, bytes: &[u8]) {
    entry(h, window_id)
        .write_pty_input(bytes, InputOrigin::Message)
        .await
        .expect("write_pty_input must not fail at the PTY level");
}

// =====================================================================
// clear_scope: what the clear byte (0x15) does.
// =====================================================================

#[tokio::test]
async fn literal_clear_scope_types_caret_u_instead_of_clearing() {
    let dir = unique_temp_dir("takeback-literal-clear");
    let h = Harness::start("takeback-literal-clear", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let literal = open_submit_child_window_with_clear_scope(
        &h,
        &mut client,
        &dir,
        "literal",
        "literal-marker",
        &input_toml("\\u0015"),
        0,
        "literal-clear",
    )
    .await;
    let ordinary = open_submit_child_window_with_clear_scope(
        &h,
        &mut client,
        &dir,
        "ordinary",
        "ordinary-marker",
        &input_toml("\\u0015"),
        0,
        "",
    )
    .await;

    for window_id in [&literal, &ordinary] {
        write(&h, window_id, b"abc").await;
        wait_until_composer_is(
            &h,
            window_id,
            &ComposerReadout::Body("abc".to_string()),
            Duration::from_secs(3),
        )
        .await;
        write(&h, window_id, &[0x15]).await;
    }

    wait_until_composer_is(
        &h,
        &literal,
        &ComposerReadout::Body("abc^U".to_string()),
        Duration::from_secs(3),
    )
    .await;
    wait_until_composer_is(
        &h,
        &ordinary,
        &ComposerReadout::Empty,
        Duration::from_secs(3),
    )
    .await;
}

/// `take_back_line_clear.rs` only proves anything if one clear byte really
/// leaves the other display lines standing.
#[tokio::test]
async fn line_clear_scope_erases_one_display_line_per_clear_byte() {
    let dir = unique_temp_dir("takeback-line-clear");
    let h = Harness::start("takeback-line-clear", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let window_id = open_submit_child_window_with_clear_scope(
        &h,
        &mut client,
        &dir,
        "line",
        "line-marker",
        &input_toml("\\u0015"),
        0,
        "line-clear",
    )
    .await;
    // 70 characters without a space: display lines of 32, 32 and 6.
    let body = "abcdefghij".repeat(7);

    write(&h, &window_id, body.as_bytes()).await;
    for remaining in [70, 64, 32] {
        wait_until_composer_is(
            &h,
            &window_id,
            &ComposerReadout::Body(body[..remaining].to_string()),
            Duration::from_secs(3),
        )
        .await;
        write(&h, &window_id, &[0x15]).await;
    }
    wait_until_composer_is(
        &h,
        &window_id,
        &ComposerReadout::Empty,
        Duration::from_secs(3),
    )
    .await;
}

/// `held:<scope>` holds a pasted body unpainted, and its next clear byte
/// acts as `<scope>` says -- unlike `hold-until-clear`, whose clear byte
/// only releases the body.
#[tokio::test]
async fn held_scope_applies_its_scope_on_the_clear_byte_that_ends_the_hold() {
    let dir = unique_temp_dir("takeback-held");
    let h = Harness::start("takeback-held", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let window_id = open_submit_child_window_with_clear_scope(
        &h,
        &mut client,
        &dir,
        "held",
        "held-marker",
        &input_toml("\\u0015"),
        0,
        "held:",
    )
    .await;

    write(&h, &window_id, b"\x1b[200~BODY\x1b[201~").await;
    write(&h, &window_id, &[0x15]).await;
    write(&h, &window_id, b"z").await;
    wait_until_composer_is(
        &h,
        &window_id,
        &ComposerReadout::Body("z".to_string()),
        Duration::from_secs(3),
    )
    .await;
}

#[tokio::test]
async fn partial_clear_scope_erases_only_the_last_unit() {
    let dir = unique_temp_dir("takeback-partial-clear");
    let h = Harness::start("takeback-partial-clear", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let window_id = open_submit_child_window_with_clear_scope(
        &h,
        &mut client,
        &dir,
        "partial",
        "partial-marker",
        &input_toml("\\u0015"),
        0,
        "partial-clear",
    )
    .await;

    write(&h, &window_id, b"abc").await;
    wait_until_composer_is(
        &h,
        &window_id,
        &ComposerReadout::Body("abc".to_string()),
        Duration::from_secs(3),
    )
    .await;

    write(&h, &window_id, &[0x15]).await;
    wait_until_composer_is(
        &h,
        &window_id,
        &ComposerReadout::Body("ab".to_string()),
        Duration::from_secs(3),
    )
    .await;
}

/// `discard-paste:<scope>` is the receiver the window layer reads a probe
/// behind a discarded body from: the pasted content never reaches the buffer,
/// and what is typed after it paints as usual.
#[tokio::test]
async fn discard_paste_scope_drops_a_pasted_body_and_paints_what_is_typed() {
    let dir = unique_temp_dir("takeback-discard-paste");
    let h = Harness::start("takeback-discard-paste", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let window_id = open_submit_child_window_with_clear_scope(
        &h,
        &mut client,
        &dir,
        "discard",
        "discard-marker",
        &input_toml("\\u0015"),
        0,
        "discard-paste:",
    )
    .await;

    write(&h, &window_id, b"\x1b[200~BODY\x1b[201~x").await;
    wait_until_composer_is(
        &h,
        &window_id,
        &ComposerReadout::Body("x".to_string()),
        Duration::from_secs(3),
    )
    .await;
    assert_eq!(
        paste_buffered_values(&h, &window_id),
        vec![0],
        "the paste reached the fixture and was dropped whole"
    );

    write(&h, &window_id, b"\x1b[200~MORE\x1b[201~yz").await;
    wait_until_composer_is(
        &h,
        &window_id,
        &ComposerReadout::Body("xyz".to_string()),
        Duration::from_secs(3),
    )
    .await;
}

/// `discard-first-paste:<scope>` is a CLI still starting up: only the first
/// body it is sent is lost.
#[tokio::test]
async fn discard_first_paste_scope_drops_only_the_first_pasted_body() {
    let dir = unique_temp_dir("takeback-discard-first-paste");
    let h = Harness::start("takeback-discard-first-paste", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let window_id = open_submit_child_window_with_clear_scope(
        &h,
        &mut client,
        &dir,
        "discardfirst",
        "discardfirst-marker",
        &input_toml("\\u0015"),
        0,
        "discard-first-paste:",
    )
    .await;

    write(&h, &window_id, b"\x1b[200~FIRST\x1b[201~").await;
    write(&h, &window_id, b"\x1b[200~SECOND\x1b[201~").await;
    wait_until_composer_is(
        &h,
        &window_id,
        &ComposerReadout::Body("SECOND".to_string()),
        Duration::from_secs(3),
    )
    .await;
    assert_eq!(
        paste_buffered_values(&h, &window_id),
        vec![0, "SECOND".len()],
        "the first paste was dropped whole and the second one kept"
    );
}

/// `drain:<scope>` drops every byte until a Ctrl-C reaches it; the Ctrl-C and
/// everything after it behave as usual.
#[tokio::test]
async fn drain_scope_drops_everything_until_the_first_ctrl_c() {
    let dir = unique_temp_dir("takeback-drain");
    let h = Harness::start("takeback-drain", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let window_id = open_submit_child_window_with_clear_scope(
        &h,
        &mut client,
        &dir,
        "drain",
        "drain-marker",
        &input_toml("\\u0015"),
        0,
        "drain:",
    )
    .await;

    write(&h, &window_id, b"\x1b[200~LOST\x1b[201~lost").await;
    write(&h, &window_id, &[0x03]).await;
    write(&h, &window_id, b"\x1b[200~KEPT\x1b[201~k").await;
    wait_until_composer_is(
        &h,
        &window_id,
        &ComposerReadout::Body("KEPTk".to_string()),
        Duration::from_secs(3),
    )
    .await;
    assert_eq!(
        paste_buffered_values(&h, &window_id),
        vec!["KEPT".len()],
        "a paste read before the first Ctrl-C never reached the fixture's state"
    );
}

/// `stall-until|<path>|<scope>` announces that it has stopped reading, so a
/// case knows the receiver is stuck before it acts, and announces when it
/// goes on; what queued meanwhile is processed in order.
#[tokio::test]
async fn stall_until_announces_when_it_stops_and_when_it_goes_on() {
    let dir = unique_temp_dir("takeback-stall-announced");
    let resume = dir.join("resume");
    let h = Harness::start("takeback-stall-announced", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let window_id = open_submit_child_window_with_clear_scope(
        &h,
        &mut client,
        &dir,
        "stall",
        "stall-marker",
        &input_toml("\\u0015"),
        0,
        &format!(
            "stall-until|{}|",
            resume
                .to_str()
                .expect("the resume path must be valid UTF-8")
        ),
    )
    .await;

    write(&h, &window_id, b"\x1b[200~BODY\x1b[201~").await;
    write(&h, &window_id, b"z").await;
    h.wait_for_scrollback(&window_id, b"STALL-BEGAN", Duration::from_secs(10))
        .await;
    assert_eq!(
        common::count_marker(&h, &window_id, "STALL-ENDED"),
        0,
        "the receiver has not gone on while the file is missing"
    );

    std::fs::write(&resume, b"").expect("let the receiver resume");
    wait_until_composer_is(
        &h,
        &window_id,
        &ComposerReadout::Body("BODYz".to_string()),
        Duration::from_secs(10),
    )
    .await;
    assert_eq!(common::count_marker(&h, &window_id, "STALL-ENDED"), 1);
}
