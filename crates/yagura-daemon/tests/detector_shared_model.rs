#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Detection reads the copy of the window's own terminal model, so it judges the
//! screen the window actually shows: a burst it did not follow chunk by chunk
//! cannot leave it with a lost screen.

mod common;

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::{
    CONTROL_DETECTION_NAME, HANG_GUARD, Harness, READY_MARKER, read_response, send,
    unique_temp_dir, write_control_definition_with_bottom_line_count,
};
use yagura_daemon::{DaemonConfig, OpenWindowRequest, PtySession, WindowEntry, WindowRegistry};
use yagura_protocol::{ClientMessage, DaemonMessage, DetectedStatus, UnknownReason, WindowStatus};

/// Tall enough that every line the lag scenario prints stays on screen.
const TALL_COLS: u16 = 200;
const TALL_ROWS: u16 = 1000;
/// Large enough that the marker stays inside the matched bottom region after
/// the filler lines that follow it.
const TALL_BOTTOM_LINES: u16 = 950;
const FILLER_LINES: usize = 120;
const FILLER_LINE_BYTES: usize = 100;
const FILLER_END: &str = "FILLER-END";

fn config_with(dir: &Path, bottom_line_count: Option<u16>, base: DaemonConfig) -> DaemonConfig {
    write_control_definition_with_bottom_line_count(dir, bottom_line_count);
    DaemonConfig {
        detection_dir: Some(dir.to_path_buf()),
        detection_name: CONTROL_DETECTION_NAME.to_string(),
        ..base
    }
}

/// The marker line, then enough filler that the output reaches the model in
/// more than one chunk.
fn marker_then_filler() -> Vec<u8> {
    let mut payload = READY_MARKER.to_vec();
    for _ in 0..FILLER_LINES {
        payload.extend(std::iter::repeat_n(b'0', FILLER_LINE_BYTES));
        payload.extend_from_slice(b"\r\n");
    }
    payload.extend_from_slice(FILLER_END.as_bytes());
    payload.extend_from_slice(b"\r\n");
    payload
}

async fn resize_window(h: &Harness, window_id: &str, cols: u16, rows: u16) {
    let entry = h.registry.get(window_id).unwrap();
    entry.notify_resize_started();
    entry.session().resize(cols, rows).await.expect("resize");
    entry.propagate_resize();
}

/// Writes `payload` from a thread with its own runtime and blocks the calling
/// thread until the window's model shows `until`, so nothing on the caller's
/// runtime follows the output meanwhile.
fn write_while_runtime_blocked(session: Arc<PtySession>, payload: Vec<u8>, until: &str) {
    let until = until.to_string();
    let worker = std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                session.write_input(&payload).await.expect("write_input");
                let deadline = Instant::now() + HANG_GUARD;
                while !session
                    .text_tail(10_000, 1 << 20)
                    .await
                    .unwrap()
                    .0
                    .contains(&until)
                {
                    assert!(
                        Instant::now() < deadline,
                        "the window never showed {until:?}"
                    );
                    std::thread::sleep(Duration::from_millis(50));
                }
            });
    });
    if let Err(panic) = worker.join() {
        std::panic::resume_unwind(panic);
    }
}

/// Waits for the window's confirmed status to be `expected`; on reaching
/// [`HANG_GUARD`] first, the status it is at.
async fn wait_for_status(
    h: &Harness,
    window_id: &str,
    expected: WindowStatus,
) -> Result<(), WindowStatus> {
    let entry = h.registry.get(window_id).unwrap();
    let mut status = entry.watch_status();
    let reached = tokio::time::timeout(
        HANG_GUARD,
        status.wait_for(|at| at.status.status() == expected),
    )
    .await
    .is_ok_and(|seen| seen.is_ok());
    if reached {
        Ok(())
    } else {
        Err(entry.current_status())
    }
}

async fn read_output_text(h: &Harness, window_id: &str) -> String {
    let mut client = h.connect().await;
    send(
        &mut client,
        &ClientMessage::ReadOutput {
            window_id: window_id.to_string(),
            lines: Some(1000),
        },
    )
    .await;
    match read_response(&mut client).await {
        DaemonMessage::WindowOutput { text, .. } => text,
        other => panic!("expected window_output, got {other:?}"),
    }
}

#[tokio::test]
async fn detection_judges_the_screen_the_window_shows_after_a_burst_it_did_not_follow() {
    let dir = unique_temp_dir("shared-model-lag");
    let config = config_with(&dir, Some(TALL_BOTTOM_LINES), DaemonConfig::default());
    let h = Harness::start("shared-model-lag", config).await;
    let window_id = h
        .open(
            yagura_test_support::process::echo_child_command(),
            Some(CONTROL_DETECTION_NAME),
        )
        .await;
    resize_window(&h, &window_id, TALL_COLS, TALL_ROWS).await;

    let session = h.registry.get(&window_id).unwrap().session().clone();
    let mut arrivals = session.output_arrivals();
    let before = *arrivals.borrow_and_update();
    write_while_runtime_blocked(session.clone(), marker_then_filler(), FILLER_END);
    // An arrival is announced just after the model takes it in, so the count is
    // awaited rather than read.
    let in_chunks = tokio::time::timeout(
        HANG_GUARD,
        arrivals.wait_for(|arrived| arrived.wrapping_sub(before) >= 2),
    )
    .await
    .is_ok_and(|seen| seen.is_ok());
    assert!(
        in_chunks,
        "the output burst must reach the model in more than one chunk, got {}",
        arrivals.borrow().wrapping_sub(before)
    );

    let marker = std::str::from_utf8(READY_MARKER).unwrap().trim_end();
    let shown = read_output_text(&h, &window_id).await;
    assert!(
        shown.contains(marker),
        "the window's text must still show the marker: {shown:?}"
    );
    let reached = wait_for_status(&h, &window_id, WindowStatus::Idle).await;
    assert!(
        reached.is_ok(),
        "the window shows {marker:?}, but detection stayed at {:?} after a burst it did not follow",
        reached.unwrap_err()
    );
    h.registry.terminate_all().await;
}

/// The same screen without a blocked runtime: the marker is inside the matched
/// region, so a failure of the burst case is the burst's doing, not the screen's.
#[tokio::test]
async fn detection_judges_the_same_tall_screen_when_nothing_lagged() {
    let dir = unique_temp_dir("shared-model-no-lag");
    let config = config_with(&dir, Some(TALL_BOTTOM_LINES), DaemonConfig::default());
    let h = Harness::start("shared-model-no-lag", config).await;
    let window_id = h
        .open(
            yagura_test_support::process::echo_child_command(),
            Some(CONTROL_DETECTION_NAME),
        )
        .await;
    resize_window(&h, &window_id, TALL_COLS, TALL_ROWS).await;

    h.write_input(&window_id, &marker_then_filler()).await;
    let deadline = Instant::now() + HANG_GUARD;
    while !read_output_text(&h, &window_id).await.contains(FILLER_END) {
        assert!(
            Instant::now() < deadline,
            "the window never showed the filler"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let reached = wait_for_status(&h, &window_id, WindowStatus::Idle).await;
    assert!(
        reached.is_ok(),
        "detection stayed at {:?} on a screen showing the marker",
        reached.unwrap_err()
    );
    h.registry.terminate_all().await;
}

/// No time table, so nothing but output or a resize notice makes the detector
/// read the model.
const RULES_ONLY_DETECTION_NAME: &str = "rules-only-test";

async fn wait_for_detected(entry: &WindowEntry, expected: DetectedStatus) {
    let mut status = entry.watch_status();
    let reached = tokio::time::timeout(HANG_GUARD, status.wait_for(|at| at.status == expected))
        .await
        .is_ok_and(|seen| seen.is_ok());
    assert!(
        reached,
        "the window never reached {expected:?}; it is at {:?}",
        entry.current_detected()
    );
}

#[tokio::test]
async fn a_resize_notice_degrades_a_quiet_window_whose_screen_read_was_lost() {
    let dir = unique_temp_dir("shared-model-resize-rebuilt");
    std::fs::write(
        dir.join(format!("{RULES_ONLY_DETECTION_NAME}.toml")),
        yagura_test_support::detection::idle_definition_toml(RULES_ONLY_DETECTION_NAME, "", ""),
    )
    .unwrap();
    let registry = WindowRegistry::new(DaemonConfig {
        detection_dir: Some(dir),
        detection_name: RULES_ONLY_DETECTION_NAME.to_string(),
        ..DaemonConfig::default()
    });
    let entry = registry
        .open(OpenWindowRequest {
            command: yagura_test_support::process::echo_child_command(),
            cwd: None,
            name: None,
            vendor: None,
            detection: Some(RULES_ONLY_DETECTION_NAME.to_string()),
            on_name_taken: Default::default(),
        })
        .await
        .expect("an echo_child window must open")
        .entry;
    entry.session().write_input(READY_MARKER).await.unwrap();
    wait_for_detected(&entry, DetectedStatus::Idle).await;

    // A read of the window's copy that panics is lost; the detector learns
    // of it only when it next reads the copy.
    let read = entry
        .session()
        .read_screen(|_| -> bool { panic!("lose this read") })
        .await;
    assert!(
        read.is_none(),
        "the panicking read must have been contained"
    );

    // Sent without a PTY resize, so no redraw output reaches the detector.
    entry.propagate_resize();
    wait_for_detected(&entry, DetectedStatus::Unknown(UnknownReason::ScreenLost)).await;
    registry.terminate_all().await;
}
