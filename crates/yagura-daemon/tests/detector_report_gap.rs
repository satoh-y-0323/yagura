#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! A window whose CLI should report its turns, but whose screen has shown a
//! running turn for the grace time without a single report, is marked: hooks
//! that silently stopped firing (a CLI update, a lost trust entry) would
//! otherwise go unnoticed because detection just falls back to the screen.
//!
//! The mark never changes the confirmed status.

use crate::common;

use common::{CopiedEngine, ScreenFeed, SharedLocalScreen};
use std::time::Duration;

use tokio::time::Instant;
use tracing_test::traced_test;
use yagura_daemon::DaemonConfig;
use yagura_daemon::detector::engine::{
    DetectorHandle, InputEcho, REPORT_GAP_GRACE, ReportsExpected, TURN_ACTIVE_QUIET,
    TURN_REPORT_STALE,
};
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

[[rules]]
status = "waiting_approval"
pattern = 'Allow this command'
priority = 30
region = "bottom"
"#;

const GRACE_MS: u64 = REPORT_GAP_GRACE.as_secs() * 1_000;
const QUIET_MS: u64 = TURN_ACTIVE_QUIET.as_secs() * 1_000;
const STALE_MS: u64 = TURN_REPORT_STALE.as_secs() * 1_000;

fn screen(text: &str) -> Vec<u8> {
    format!("\x1b[?25l\x1b[2J\x1b[H{text}\x1b[?25h").into_bytes()
}
fn running() -> Vec<u8> {
    screen("esc to interrupt")
}
fn idle() -> Vec<u8> {
    screen("› ")
}
fn approval() -> Vec<u8> {
    screen("Allow this command")
}

struct Driver {
    start: Instant,
    engine: CopiedEngine,
    samples: Vec<(u64, DetectedStatus)>,
}

impl Driver {
    fn new(expect_reports: Option<ReportsExpected>) -> Self {
        let config = DaemonConfig::default();
        let mut engine = common::new_engine_sized(DEF, &config, 60, 8);
        if let Some(expected) = expect_reports {
            engine.expect_status_reports(expected);
        }
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

    async fn input_at(&mut self, ms: u64, echo: InputEcho) {
        self.advance_to(ms).await;
        self.engine.on_input(echo);
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
    let mut driver = Driver::new(Some(ReportsExpected::FromLaunch));
    driver.feed_at(0, &running()).await;
    assert!(!driver.gap_at(GRACE_MS - 500).await, "inside the grace");
    assert!(driver.gap_at(GRACE_MS + 600).await, "grace elapsed");
}

#[tokio::test(start_paused = true)]
async fn the_grace_runs_from_the_first_running_the_screen_confirmed() {
    let mut driver = Driver::new(Some(ReportsExpected::FromLaunch));
    driver.feed_at(0, &idle()).await;
    assert!(!driver.gap_at(GRACE_MS).await, "idle time does not count");
    driver.feed_at(GRACE_MS + 100, &running()).await;
    assert!(!driver.gap_at(GRACE_MS + 100 + GRACE_MS - 500).await);
    assert!(driver.gap_at(GRACE_MS + 100 + GRACE_MS + 600).await);

    // A taken-back engine was born inside a turn the previous daemon already
    // reported, and cannot tell from the screen where that turn ends: it
    // estimates the turn's end from its own evidence instead.
    // Stage 1 and 2 share one driver; each later stage has its own.
    let mut a = Driver::new(Some(ReportsExpected::FromTakeBack));
    for at in [0, 1_000] {
        a.feed_at(at, &running()).await;
    }
    for at in [2_000, 3_000, 4_000] {
        a.feed_at(at, &idle()).await;
    }
    let mut last_output = 0;
    for at in (5_000..=5_000 + GRACE_MS).step_by(1_000) {
        a.feed_at(at, &running()).await;
        last_output = at;
    }
    assert!(
        !a.gap_at(5_000 + GRACE_MS + 600).await,
        "a codex answer streaming across idle-looking screens is not marked"
    );

    let idle_at = last_output + QUIET_MS + 1_000;
    a.feed_at(idle_at, &idle()).await;
    let running_at = idle_at + 100;
    a.feed_at(running_at, &running()).await;
    assert!(
        !a.gap_at(running_at + GRACE_MS - 500).await,
        "a new turn after a quiet gap is inside its grace"
    );
    assert!(
        a.gap_at(running_at + GRACE_MS + 600).await,
        "a new turn after a quiet gap is marked once its grace elapses"
    );

    let mut b = Driver::new(Some(ReportsExpected::FromTakeBack));
    for at in (0..=STALE_MS + 1_000).step_by(1_000) {
        b.feed_at(at, &idle()).await;
    }
    let running_at = STALE_MS + 2_000;
    b.feed_at(running_at, &running()).await;
    assert!(
        b.gap_at(running_at + GRACE_MS + 600).await,
        "an estimate that never saw a report closes once it is stale"
    );

    let mut c = Driver::new(Some(ReportsExpected::FromTakeBack));
    c.feed_at(0, &approval()).await;
    let at = 2 * QUIET_MS;
    c.feed_at(at, &running()).await;
    for offset in (1_000..=GRACE_MS).step_by(1_000) {
        c.feed_at(at + offset, &running()).await;
    }
    assert!(
        !c.gap_at(at + GRACE_MS + 600).await,
        "a quiet wait for approval does not close the estimate"
    );

    let mut d = Driver::new(Some(ReportsExpected::FromTakeBack));
    d.feed_at(0, &approval()).await;
    let at = 2 * QUIET_MS;
    d.input_at(at, InputEcho::EchoOnly).await;
    d.feed_at(at + 50, &running()).await;
    for offset in (1_000..=GRACE_MS).step_by(1_000) {
        d.feed_at(at + offset, &running()).await;
    }
    assert!(
        !d.gap_at(at + GRACE_MS + 600).await,
        "an answered approval re-anchors the estimate at the answer"
    );
}

/// A later running screen does not restart the grace: it runs from the first
/// one, or the mark would blink off and on with every new turn.
#[tokio::test(start_paused = true)]
async fn a_later_running_screen_does_not_restart_the_grace() {
    let mut driver = Driver::new(Some(ReportsExpected::FromLaunch));
    driver.feed_at(0, &running()).await;
    driver.feed_at(3_000, &idle()).await;
    driver.feed_at(6_000, &running()).await;
    assert!(!driver.gap_at(GRACE_MS - 500).await, "inside the grace");
    assert!(driver.gap_at(GRACE_MS + 600).await, "grace from the first");
}

#[tokio::test(start_paused = true)]
async fn a_report_before_the_grace_ends_prevents_the_mark() {
    let mut driver = Driver::new(Some(ReportsExpected::FromLaunch));
    driver.feed_at(0, &running()).await;
    driver
        .report_at(GRACE_MS - 1_000, WindowStatus::Running)
        .await;
    assert!(!driver.gap_at(GRACE_MS * 3).await);
}

#[tokio::test(start_paused = true)]
async fn a_report_after_the_mark_clears_it() {
    let mut driver = Driver::new(Some(ReportsExpected::FromLaunch));
    driver.feed_at(0, &running()).await;
    assert!(driver.gap_at(GRACE_MS + 600).await);
    driver
        .report_at(GRACE_MS + 1_000, WindowStatus::Running)
        .await;
    assert!(!driver.gap_at(GRACE_MS + 1_500).await);
}

#[tokio::test(start_paused = true)]
async fn a_window_not_expecting_reports_is_never_marked() {
    let mut driver = Driver::new(None);
    driver.feed_at(0, &running()).await;
    assert!(!driver.gap_at(GRACE_MS * 3).await);
}

#[tokio::test(start_paused = true)]
async fn a_window_whose_screen_never_confirms_running_is_never_marked() {
    let mut driver = Driver::new(Some(ReportsExpected::FromLaunch));
    driver.feed_at(0, &idle()).await;
    assert!(!driver.gap_at(GRACE_MS * 6).await, "waiting for input");

    // A report that opens a turn is not the screen confirming running.
    let mut driver = Driver::new(Some(ReportsExpected::FromLaunch));
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
    async fn run(expect_reports: Option<ReportsExpected>) -> Vec<(u64, DetectedStatus)> {
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
    let with = run(Some(ReportsExpected::FromLaunch)).await;
    let without = run(None).await;
    assert_eq!(with, without);
    assert!(with.iter().any(|(_, s)| *s == DetectedStatus::Running));
}

async fn drain(
    screen: &SharedLocalScreen,
    handle: &DetectorHandle,
    expect: Option<ReportsExpected>,
) {
    if let Some(expected) = expect {
        handle.expect_status_reports(expected);
    }
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
    let (handle, _statuses) = DetectorHandle::spawn(engine.into_engine());
    drain(&screen, &handle, Some(ReportsExpected::FromLaunch)).await;
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
    let (handle, _statuses) = DetectorHandle::spawn(engine.into_engine());
    drain(&screen, &handle, None).await;
    logs_assert(|lines: &[&str]| match warn_lines(lines) {
        0 => Ok(()),
        n => Err(format!("expected no warning, saw {n}: {lines:?}")),
    });
}
