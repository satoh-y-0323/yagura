#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! A window whose CLI should report its turns, but whose screen has shown a
//! running turn for the grace time without a single report, is marked: hooks
//! that silently stopped firing (a CLI update, a lost trust entry) would
//! otherwise go unnoticed because detection just falls back to the screen.
//!
//! The mark never changes the confirmed status.

mod common;

use common::ScreenFeed;
use std::time::Duration;

use tokio::time::Instant;
use tracing_test::traced_test;
use yagura_daemon::DaemonConfig;
use yagura_daemon::detector::SharedScreen;
use yagura_daemon::detector::engine::{DetectionEngine, DetectorHandle, REPORT_GAP_GRACE};
use yagura_protocol::{DetectedStatus, WindowStatus};

const DEF: &str = r#"
schema_version = 1
name = "test-report-gap"

[debounce]
confirm_ms = 5

[[rules]]
status = "running"
pattern = 'esc to interrupt'
priority = 20
region = "bottom"

[[rules]]
status = "waiting_input"
pattern = '^›'
priority = 10
region = "bottom"
"#;

const GRACE_MS: u64 = REPORT_GAP_GRACE.as_secs() * 1_000;

fn screen(text: &str) -> Vec<u8> {
    format!("\x1b[?25l\x1b[2J\x1b[H{text}\x1b[?25h").into_bytes()
}
fn running() -> Vec<u8> {
    screen("esc to interrupt")
}
fn idle() -> Vec<u8> {
    screen("› ")
}

struct Driver {
    start: Instant,
    engine: DetectionEngine,
    samples: Vec<(u64, DetectedStatus)>,
}

impl Driver {
    fn new(expect_reports: bool) -> Self {
        let config = DaemonConfig::default();
        let mut engine = common::new_engine_sized(DEF, &config, 60, 8);
        engine.expect_status_reports(expect_reports);
        Self {
            start: Instant::now(),
            engine,
            samples: Vec::new(),
        }
    }

    async fn advance_to(&mut self, target_ms: u64) {
        let target = self.start + Duration::from_millis(target_ms);
        for _ in 0..100_000 {
            match self.engine.next_deadline() {
                Some(deadline) if deadline <= target => {
                    let now = Instant::now();
                    if deadline > now {
                        tokio::time::advance(deadline - now).await;
                    }
                    self.engine.on_tick();
                    self.samples.push((self.ms(), self.engine.detected()));
                }
                _ => break,
            }
        }
        let now = Instant::now();
        if target > now {
            tokio::time::advance(target - now).await;
            self.engine.on_tick();
            self.samples.push((self.ms(), self.engine.detected()));
        }
    }

    fn ms(&self) -> u64 {
        u64::try_from((Instant::now() - self.start).as_millis()).unwrap()
    }

    async fn feed_at(&mut self, ms: u64, bytes: &[u8]) {
        self.advance_to(ms).await;
        self.engine.feed(bytes);
    }

    async fn report_at(&mut self, ms: u64, status: WindowStatus) {
        self.advance_to(ms).await;
        self.engine.on_status_event(status);
    }

    async fn gap_at(&mut self, ms: u64) -> bool {
        self.advance_to(ms).await;
        self.engine.report_gap()
    }
}

#[tokio::test(start_paused = true)]
async fn a_window_expecting_reports_is_marked_after_the_grace_of_running_without_one() {
    let mut driver = Driver::new(true);
    driver.feed_at(0, &running()).await;
    assert!(!driver.gap_at(GRACE_MS - 500).await, "inside the grace");
    assert!(driver.gap_at(GRACE_MS + 600).await, "grace elapsed");
}

#[tokio::test(start_paused = true)]
async fn the_grace_runs_from_the_first_running_the_screen_confirmed() {
    let mut driver = Driver::new(true);
    driver.feed_at(0, &idle()).await;
    assert!(!driver.gap_at(GRACE_MS).await, "idle time does not count");
    driver.feed_at(GRACE_MS + 100, &running()).await;
    assert!(!driver.gap_at(GRACE_MS + 100 + GRACE_MS - 500).await);
    assert!(driver.gap_at(GRACE_MS + 100 + GRACE_MS + 600).await);
}

/// A later running screen does not restart the grace: it runs from the first
/// one, or the mark would blink off and on with every new turn.
#[tokio::test(start_paused = true)]
async fn a_later_running_screen_does_not_restart_the_grace() {
    let mut driver = Driver::new(true);
    driver.feed_at(0, &running()).await;
    driver.feed_at(3_000, &idle()).await;
    driver.feed_at(6_000, &running()).await;
    assert!(!driver.gap_at(GRACE_MS - 500).await, "inside the grace");
    assert!(driver.gap_at(GRACE_MS + 600).await, "grace from the first");
}

#[tokio::test(start_paused = true)]
async fn a_report_before_the_grace_ends_prevents_the_mark() {
    let mut driver = Driver::new(true);
    driver.feed_at(0, &running()).await;
    driver
        .report_at(GRACE_MS - 1_000, WindowStatus::Running)
        .await;
    assert!(!driver.gap_at(GRACE_MS * 3).await);
}

#[tokio::test(start_paused = true)]
async fn a_report_after_the_mark_clears_it() {
    let mut driver = Driver::new(true);
    driver.feed_at(0, &running()).await;
    assert!(driver.gap_at(GRACE_MS + 600).await);
    driver
        .report_at(GRACE_MS + 1_000, WindowStatus::Running)
        .await;
    assert!(!driver.gap_at(GRACE_MS + 1_500).await);
}

#[tokio::test(start_paused = true)]
async fn a_window_not_expecting_reports_is_never_marked() {
    let mut driver = Driver::new(false);
    driver.feed_at(0, &running()).await;
    assert!(!driver.gap_at(GRACE_MS * 3).await);
}

#[tokio::test(start_paused = true)]
async fn a_window_whose_screen_never_confirms_running_is_never_marked() {
    let mut driver = Driver::new(true);
    driver.feed_at(0, &idle()).await;
    assert!(!driver.gap_at(GRACE_MS * 6).await, "waiting for input");

    // A report that opens a turn is not the screen confirming running.
    let mut driver = Driver::new(true);
    driver.feed_at(0, &idle()).await;
    driver.report_at(200, WindowStatus::Running).await;
    assert!(!driver.gap_at(GRACE_MS * 3).await);
}

#[tokio::test(start_paused = true)]
async fn the_mark_does_not_change_the_confirmed_statuses() {
    // Each driver measures from its own start, so the two runs share the
    // paused clock without the second one starting in the past. Only the
    // points where the confirmed status changes are compared: the mark's own
    // deadline adds a wake-up, and with it a sample, that changes nothing.
    async fn run(expect_reports: bool) -> Vec<(u64, DetectedStatus)> {
        let mut driver = Driver::new(expect_reports);
        driver.feed_at(0, &running()).await;
        driver.feed_at(GRACE_MS + 2_000, &idle()).await;
        driver.advance_to(GRACE_MS + 4_000).await;
        let mut changes: Vec<(u64, DetectedStatus)> = Vec::new();
        for (at, status) in driver.samples {
            if changes.last().is_none_or(|(_, last)| *last != status) {
                changes.push((at, status));
            }
        }
        changes
    }
    let with = run(true).await;
    let without = run(false).await;
    assert_eq!(with, without);
    assert!(with.iter().any(|(_, s)| *s == DetectedStatus::Running));
}

async fn drain(screen: &SharedScreen, handle: &DetectorHandle, expect: bool) {
    handle.expect_status_reports(expect);
    common::feed_handle(screen, handle, &running());
    // No event arrives after this, so only the engine's own deadline can
    // raise the mark.
    tokio::time::sleep(Duration::from_millis(GRACE_MS * 3)).await;
}

fn warn_lines(lines: &[&str]) -> usize {
    lines.iter().filter(|line| line.contains("WARN")).count()
}

#[traced_test]
#[tokio::test(start_paused = true)]
async fn the_driver_warns_once_when_the_mark_is_raised_by_its_own_deadline() {
    let config = DaemonConfig::default();
    let engine = common::new_engine_sized(DEF, &config, 60, 8);
    let screen = engine.screen().clone();
    let (handle, _statuses) = DetectorHandle::spawn(engine);
    drain(&screen, &handle, true).await;
    logs_assert(|lines: &[&str]| match warn_lines(lines) {
        1 => Ok(()),
        n => Err(format!("expected exactly one warning, saw {n}: {lines:?}")),
    });
}

#[traced_test]
#[tokio::test(start_paused = true)]
async fn the_driver_does_not_warn_for_a_window_not_expecting_reports() {
    let config = DaemonConfig::default();
    let engine = common::new_engine_sized(DEF, &config, 60, 8);
    let screen = engine.screen().clone();
    let (handle, _statuses) = DetectorHandle::spawn(engine);
    drain(&screen, &handle, false).await;
    logs_assert(|lines: &[&str]| match warn_lines(lines) {
        0 => Ok(()),
        n => Err(format!("expected no warning, saw {n}: {lines:?}")),
    });
}
