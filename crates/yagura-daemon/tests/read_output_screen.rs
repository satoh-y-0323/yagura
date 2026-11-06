#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: `read_output` returns a long value exactly as the window's program
//! wrote it, on one line, even once it wraps and scrolls the window, and the status
//! summary is the end of the text the window's screen shows, read the same way.
//!
//! Measured through a real daemon process ([`common::real_daemon`]) and a real child
//! (`env_probe`) on the platform's own pseudo-terminal, whose repaint of wrapped rows is
//! what a reader of the raw output gets wrong.

use crate::common;

use common::real_daemon::Sandbox;
use common::{read_response, send};
use yagura_protocol::{ClientMessage, DaemonMessage};

/// Far more than a default 80x24 window holds, so the window wraps it and scrolls.
fn long_value() -> String {
    (0..90)
        .map(|n| format!("segment-{n:02}-abcdefghij"))
        .collect::<Vec<_>>()
        .join(";")
}

#[tokio::test]
async fn a_long_wrapped_value_reads_back_unchanged_on_one_line() {
    let value = long_value();
    let sandbox = Sandbox::new("read-output-long");
    sandbox.write_var_probe_profile(
        "probe",
        "LONG_VALUE",
        &format!("\n[env]\nLONG_VALUE = '{value}'\n"),
    );
    let mut daemon = sandbox.start().await;
    let window_id = daemon.open_profile_window(&sandbox, "probe").await;

    let expected = format!("<<LONG_VALUE={value}>>");
    let text = daemon
        .output_until(&window_id, "the whole report", |text| text.contains(">>"))
        .await;
    assert!(
        text.lines().any(|line| line == expected),
        "the report must come back as one unchanged line; got {text:?}"
    );
}

/// The report wraps over more of the screen than 500 characters, so the last 500
/// characters the screen shows are the report's last 500, its wrapped rows joined.
#[tokio::test]
async fn the_status_summary_is_the_last_500_characters_the_screen_shows() {
    let value = long_value();
    let sandbox = Sandbox::new("read-output-summary");
    sandbox.write_var_probe_profile(
        "probe",
        "LONG_VALUE",
        &format!(
            "
[env]
LONG_VALUE = '{value}'
"
        ),
    );
    let mut daemon = sandbox.start().await;
    let window_id = daemon.open_profile_window(&sandbox, "probe").await;
    daemon
        .output_until(&window_id, "the whole report", |text| text.contains(">>"))
        .await;

    send(
        &mut daemon.client,
        &ClientMessage::GetWindowStatus {
            window_id: window_id.clone(),
        },
    )
    .await;
    let summary = match read_response(&mut daemon.client).await {
        DaemonMessage::WindowStatus {
            last_output_summary,
            ..
        } => last_output_summary.expect("a window with output has a summary"),
        other => panic!("expected window_status, got {other:?}"),
    };
    let report = format!("<<LONG_VALUE={value}>>");
    assert_eq!(summary, report[report.len() - 500..]);
}

#[tokio::test]
async fn the_status_summary_counts_its_500_characters_not_bytes() {
    // One cell but three bytes per euro sign, so a byte budget sized for
    // ASCII would cut the summary short. The whole report fits on the screen.
    let value = (0..60)
        .map(|n| format!("{n:02}{}", "\u{20ac}".repeat(6)))
        .collect::<Vec<_>>()
        .join(";");
    let sandbox = Sandbox::new("read-output-summary-multibyte");
    sandbox.write_var_probe_profile(
        "probe",
        "LONG_VALUE",
        &format!("\n[env]\nLONG_VALUE = '{value}'\n"),
    );
    let mut daemon = sandbox.start().await;
    let window_id = daemon.open_profile_window(&sandbox, "probe").await;
    daemon
        .output_until(&window_id, "the whole report", |text| text.contains(">>"))
        .await;

    send(
        &mut daemon.client,
        &ClientMessage::GetWindowStatus {
            window_id: window_id.clone(),
        },
    )
    .await;
    let summary = match read_response(&mut daemon.client).await {
        DaemonMessage::WindowStatus {
            last_output_summary,
            ..
        } => last_output_summary.expect("a window with output has a summary"),
        other => panic!("expected window_status, got {other:?}"),
    };
    let report = format!("<<LONG_VALUE={value}>>");
    let mut tail: Vec<char> = report.chars().rev().take(500).collect();
    tail.reverse();
    assert_eq!(summary, tail.into_iter().collect::<String>());
}
