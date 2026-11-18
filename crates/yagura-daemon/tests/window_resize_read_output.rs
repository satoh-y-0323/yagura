#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Resizing a real window reaches the model that `read_output` reads: a long
//! line the child printed still reads back as one line after the window was
//! narrowed and widened again.

use crate::common;

use std::time::{Duration, Instant};

use yagura_daemon::{DaemonConfig, PtySession};

fn long_line() -> String {
    let segments: Vec<String> = (0..12)
        .map(|n| format!("segment-{n:02}-abcdefghij"))
        .collect();
    format!("LONGLINE:{}", segments.join(";"))
}

async fn wait_for_text(session: &PtySession, needle: &str) {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if session
            .text_tail(100, 64 * 1024)
            .await
            .unwrap()
            .0
            .contains(needle)
        {
            return;
        }
        assert!(Instant::now() < deadline, "never saw {needle:?}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn a_long_line_reads_as_one_line_after_the_window_is_narrowed_and_widened() {
    let line = long_line();
    let child = common::bins::screen_load_child_path();
    let session = common::spawn_session(
        &[child.to_str().unwrap(), "text", line.as_str()],
        &DaemonConfig::default(),
    )
    .await
    .expect("spawn the load child");
    wait_for_text(&session, "segment-11").await;

    session.resize(20, 24).await.expect("narrow");
    session.resize(120, 24).await.expect("widen");

    // Read until the line shows whole and once; the deadline only bounds a hang.
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let (text, _) = session.text_tail(100, 64 * 1024).await.unwrap();
        let with_line: Vec<&str> = text.lines().filter(|l| l.contains("LONGLINE")).collect();
        if with_line == [line.as_str()] && text.matches("segment-00").count() == 1 {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the long line never read back whole and once, full text: {text:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
