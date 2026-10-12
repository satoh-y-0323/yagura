#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! A status report from a CLI's hooks says where a turn began and ended, not
//! what the screen shows right now.
//!
//! `Running` / `WaitingApproval` open a turn; `WaitingInput` / `Idle` /
//! `Interrupted` close it; `Unknown` / `Exited` withdraw the report. A report
//! has no expiry. While a turn is open and output is still arriving, a screen
//! that looks idle (or shows nothing the rules know) is a turn in flight, but
//! once output has been quiet for the turn-quiet time the turn counts as
//! closed and only the next `Running` / `WaitingApproval` report opens it
//! again. A screen the rules positively classify always wins, and a screen the
//! daemon has lost is never rescued by a report.

mod common;

use common::ScreenFeed;
use std::path::Path;
use std::time::Duration;

use tokio::time::Instant;
use yagura_daemon::DaemonConfig;
use yagura_daemon::detector::engine::{
    DetectionEngine, DetectorHandle, InputEcho, TURN_ACTIVE_QUIET, TURN_REPORT_STALE,
};
use yagura_protocol::{DetectedStatus, UnknownReason, WindowStatus};

/// Silence after which an open turn counts as closed, in whole milliseconds.
const TURN_QUIET_MS: u64 = TURN_ACTIVE_QUIET.as_secs() * 1_000;

/// Time with no report after which an open `Running` turn counts as closed.
const TURN_STALE_MS: u64 = TURN_REPORT_STALE.as_secs() * 1_000;

const COLS: u16 = 60;
const ROWS: u16 = 8;

const SCREEN_DEF: &str = r#"
schema_version = 1
name = "test-turn-reports"

[debounce]
confirm_ms = 5

[[rules]]
status = "unknown"
pattern = 'DECLARED-UNKNOWN'
priority = 40
region = "bottom"

[[rules]]
status = "waiting_approval"
pattern = 'Allow\?'
priority = 30
region = "bottom"

[[rules]]
status = "interrupted"
pattern = 'INTERRUPTED-BY-USER'
priority = 25
region = "bottom"

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

/// The same rules plus both time-based judgements, to show that a report's
/// turn does not depend on whether the definition has time tables.
fn tabled_def() -> String {
    format!(
        "{SCREEN_DEF}\n[activity]\nstatus = \"running\"\nwindow_ms = 500\n\n[quiescence]\nstatus = \"idle\"\nafter_ms = 2000\n"
    )
}

/// Repaints the whole screen with `text` and shows the cursor again.
fn screen(text: &str) -> Vec<u8> {
    format!("\x1b[?25l\x1b[2J\x1b[H{text}\x1b[?25h").into_bytes()
}

/// A no-op chunk: it only shows the cursor, so it is output that changes
/// nothing on the screen.
const CHUNK: &[u8] = b"\x1b[?25h";

fn idle() -> Vec<u8> {
    screen("› ")
}
fn unmatched() -> Vec<u8> {
    screen("some text no rule knows")
}
fn approval() -> Vec<u8> {
    screen("Allow? (y/n)")
}
fn interrupted_screen() -> Vec<u8> {
    screen("INTERRUPTED-BY-USER")
}
fn declared_unknown() -> Vec<u8> {
    screen("DECLARED-UNKNOWN")
}
fn running_footer() -> Vec<u8> {
    screen("esc to interrupt")
}

const NO_RULE: DetectedStatus = DetectedStatus::Unknown(UnknownReason::NoRuleMatched);

/// Drives an engine the way the detector's driver task does: whenever the
/// engine's next deadline falls due, it is ticked.
struct Driver {
    start: Instant,
    engine: DetectionEngine,
    samples: Vec<(u64, DetectedStatus)>,
}

impl Driver {
    fn new(engine: DetectionEngine) -> Self {
        Self {
            start: Instant::now(),
            engine,
            samples: Vec::new(),
        }
    }

    fn synthetic(definition: &str) -> Self {
        let config = DaemonConfig::default();
        Self::new(common::new_engine_sized(definition, &config, COLS, ROWS))
    }

    fn elapsed_ms(&self) -> u64 {
        u64::try_from((Instant::now() - self.start).as_millis()).unwrap()
    }

    fn sample(&mut self) {
        let sample = (self.elapsed_ms(), self.engine.detected());
        self.samples.push(sample);
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
                    self.sample();
                }
                _ => break,
            }
        }
        let now = Instant::now();
        if target > now {
            tokio::time::advance(target - now).await;
            self.engine.on_tick();
            self.sample();
        }
    }

    async fn feed_at(&mut self, ms: u64, bytes: &[u8]) {
        self.advance_to(ms).await;
        self.engine.feed(bytes);
        self.sample();
    }

    async fn report_at(&mut self, ms: u64, status: WindowStatus) {
        self.advance_to(ms).await;
        self.engine.on_status_event(status);
        self.sample();
    }

    /// Advances to `ms` and returns what is confirmed there.
    async fn detected_at(&mut self, ms: u64) -> DetectedStatus {
        self.advance_to(ms).await;
        self.engine.detected()
    }

    /// The first sample in `from_ms..=to_ms` that `is_ok` rejects.
    fn first_rejected(
        &self,
        from_ms: u64,
        to_ms: u64,
        is_ok: impl Fn(DetectedStatus) -> bool,
    ) -> Option<(u64, DetectedStatus)> {
        self.samples
            .iter()
            .copied()
            .find(|(ms, seen)| (from_ms..=to_ms).contains(ms) && !is_ok(*seen))
    }
}

/// One starting screen a turn can be open over, with the status the rules
/// alone give it.
struct Case {
    name: &'static str,
    definition: String,
    cols: u16,
    rows: u16,
    bytes: Vec<u8>,
    screen_only: DetectedStatus,
    /// Milliseconds after which the first confirmation has certainly happened.
    settle_ms: u64,
}

impl Case {
    fn driver(&self) -> Driver {
        let config = DaemonConfig::default();
        Driver::new(common::new_engine_sized(
            &self.definition,
            &config,
            self.cols,
            self.rows,
        ))
    }

    /// Feeds the screen at 0 and opens a turn just after the screen has
    /// settled.
    async fn open_turn(&self) -> (Driver, u64) {
        let mut driver = self.driver();
        driver.feed_at(0, &self.bytes).await;
        assert_eq!(
            driver.detected_at(self.settle_ms).await,
            self.screen_only,
            "{}: the screen alone is judged before any report",
            self.name
        );
        let report_ms = self.settle_ms + 100;
        driver.report_at(report_ms, WindowStatus::Running).await;
        (driver, report_ms)
    }
}

fn codex_fixture(name: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/codex-cli/running")
        .join(format!("{name}.bin"));
    std::fs::read(&path).unwrap_or_else(|err| panic!("cannot read {}: {err}", path.display()))
}

/// Screens a Codex answer streams over while the turn is in flight and the
/// screen shows no footer of its own.
fn cases_over_a_quiet_looking_screen() -> Vec<Case> {
    vec![
        Case {
            name: "synthetic idle screen",
            definition: SCREEN_DEF.to_string(),
            cols: COLS,
            rows: ROWS,
            bytes: idle(),
            screen_only: DetectedStatus::WaitingInput,
            settle_ms: 100,
        },
        Case {
            name: "synthetic screen no rule matches",
            definition: SCREEN_DEF.to_string(),
            cols: COLS,
            rows: ROWS,
            bytes: unmatched(),
            screen_only: NO_RULE,
            settle_ms: 100,
        },
        Case {
            name: "recorded Codex screen after an answer",
            definition: common::repo_definition_text("codex-cli.toml"),
            cols: 100,
            rows: 30,
            bytes: codex_fixture("100x30-idle-after-answer"),
            screen_only: DetectedStatus::WaitingInput,
            settle_ms: 600,
        },
    ]
}

// =====================================================================
// Without any report
// =====================================================================

/// Control: a window that never gets a report is judged by its screen alone,
/// however much output arrives.
#[tokio::test(start_paused = true)]
async fn without_any_report_the_screen_alone_decides() {
    let mut driver = Driver::synthetic(SCREEN_DEF);
    driver.feed_at(0, &idle()).await;
    for second in 1..=10 {
        driver.feed_at(second * 1_000, CHUNK).await;
    }
    assert_eq!(
        driver.first_rejected(200, 10_000, |d| d == DetectedStatus::WaitingInput),
        None
    );

    driver.feed_at(11_000, &unmatched()).await;
    assert_eq!(driver.detected_at(11_300).await, NO_RULE);
    driver.feed_at(12_000, &approval()).await;
    assert_eq!(
        driver.detected_at(12_300).await,
        DetectedStatus::WaitingApproval
    );
}

// =====================================================================
// An open turn with output still arriving
// =====================================================================

/// A long answer keeps the turn running for as long as output keeps coming,
/// however long the turn has lasted in total.
#[tokio::test(start_paused = true)]
async fn an_open_turn_is_running_for_as_long_as_output_keeps_coming() {
    for case in cases_over_a_quiet_looking_screen() {
        let (mut driver, report_ms) = case.open_turn().await;
        for second in 1..=40 {
            driver.feed_at(second * 1_000, CHUNK).await;
        }
        driver.advance_to(40_500).await;
        assert_eq!(
            driver.first_rejected(report_ms + 500, 40_500, |d| d == DetectedStatus::Running),
            None,
            "{}: the turn stopped being running while output was still arriving",
            case.name
        );
    }
}

// =====================================================================
// A turn no report ever closes
// =====================================================================

/// Output every 4 s (never quiet for the turn-quiet time) is a periodic redraw,
/// not an answer; with no report for the stale time the screen decides again.
#[tokio::test(start_paused = true)]
async fn a_running_turn_with_no_report_for_two_minutes_closes_though_output_continues() {
    for case in cases_over_a_quiet_looking_screen() {
        let (mut driver, report_ms) = case.open_turn().await;
        let mut at = report_ms;
        while at + 4_000 < report_ms + TURN_STALE_MS + 600 {
            at += 4_000;
            driver.feed_at(at, CHUNK).await;
        }
        assert_eq!(
            driver.first_rejected(report_ms + 500, report_ms + TURN_STALE_MS - 1_000, |d| d
                == DetectedStatus::Running),
            None,
            "{}: the turn closed before the stale time",
            case.name
        );
        assert_eq!(
            driver.detected_at(report_ms + TURN_STALE_MS + 700).await,
            case.screen_only,
            "{}: no report for the stale time, so the screen decides",
            case.name
        );
    }
}

/// A report in the middle of the turn restarts the stale time.
#[tokio::test(start_paused = true)]
async fn a_report_during_the_turn_extends_the_stale_time() {
    let case = cases_over_a_quiet_looking_screen().remove(0);
    let (mut driver, report_ms) = case.open_turn().await;
    let mid = report_ms + TURN_STALE_MS - 20_000;
    let mut at = report_ms;
    while at < mid {
        at += 4_000;
        driver.feed_at(at, CHUNK).await;
    }
    driver.report_at(at, WindowStatus::Running).await;
    let second = at;
    while at < second + TURN_STALE_MS - 4_000 {
        at += 4_000;
        driver.feed_at(at, CHUNK).await;
    }
    assert_eq!(
        driver.first_rejected(report_ms + 500, second + TURN_STALE_MS - 1_000, |d| d
            == DetectedStatus::Running),
        None,
        "the later report restarted the stale time"
    );
    while at + 4_000 < second + TURN_STALE_MS + 600 {
        at += 4_000;
        driver.feed_at(at, CHUNK).await;
    }
    assert_eq!(
        driver.detected_at(second + TURN_STALE_MS + 700).await,
        case.screen_only
    );
    // The next opening report opens it again.
    let closed_at = second + TURN_STALE_MS + 700;
    driver
        .report_at(closed_at + 200, WindowStatus::Running)
        .await;
    assert_eq!(
        driver.detected_at(closed_at + 800).await,
        DetectedStatus::Running
    );
}

/// An approval waits for the user, so no amount of time closes it.
#[tokio::test(start_paused = true)]
async fn an_approval_report_is_not_closed_by_the_stale_time() {
    let mut driver = Driver::synthetic(SCREEN_DEF);
    driver.feed_at(0, &unmatched()).await;
    driver.report_at(200, WindowStatus::WaitingApproval).await;
    let mut at = 200;
    while at < TURN_STALE_MS + 20_000 {
        at += 4_000;
        driver.feed_at(at, CHUNK).await;
    }
    assert_eq!(
        driver.detected_at(TURN_STALE_MS + 21_000).await,
        DetectedStatus::WaitingApproval
    );
}

/// The engine names the stale instant as a deadline, so a driver sleeping until
/// the next deadline is woken for it even when the last output was recent.
#[tokio::test(start_paused = true)]
async fn the_engine_schedules_a_wakeup_for_the_stale_time() {
    let mut driver = Driver::synthetic(SCREEN_DEF);
    driver.feed_at(0, &idle()).await;
    driver.report_at(100, WindowStatus::Running).await;
    let mut at = 100;
    while at < 100 + TURN_STALE_MS - 1_000 {
        at += 4_000;
        driver
            .feed_at(at.min(100 + TURN_STALE_MS - 1_000), CHUNK)
            .await;
    }
    assert_eq!(
        driver.engine.next_deadline(),
        Some(driver.start + Duration::from_millis(100 + TURN_STALE_MS))
    );
}

// =====================================================================
// The turn goes quiet
// =====================================================================

/// Once output has stopped for the turn-quiet time the screen decides again,
/// and one later redraw of the input area does not bring `Running` back.
#[tokio::test(start_paused = true)]
async fn a_turn_quiet_for_five_seconds_returns_to_the_screen_and_a_lone_redraw_does_not_reopen_it()
{
    for case in cases_over_a_quiet_looking_screen() {
        let (mut driver, _) = case.open_turn().await;
        assert_eq!(
            driver.detected_at(TURN_QUIET_MS - 500).await,
            DetectedStatus::Running,
            "{}: still inside the quiet time",
            case.name
        );
        assert_eq!(
            driver.detected_at(TURN_QUIET_MS + 600).await,
            case.screen_only,
            "{}: output stopped for the quiet time",
            case.name
        );
        driver.feed_at(8_000, CHUNK).await;
        driver.advance_to(9_500).await;
        assert_eq!(
            driver.first_rejected(TURN_QUIET_MS + 600, 9_500, |d| d == case.screen_only),
            None,
            "{}: a single output after the silence must not reopen the turn",
            case.name
        );
    }
}

/// The return to the screen is scheduled by the engine's own deadline, not by
/// a tick some other event happens to cause.
#[tokio::test(start_paused = true)]
async fn the_driver_task_wakes_at_the_end_of_the_quiet_time() {
    let config = DaemonConfig::default();
    let engine = common::new_engine_sized(SCREEN_DEF, &config, COLS, ROWS);
    let screen = engine.screen().clone();
    let (handle, mut statuses) = DetectorHandle::spawn(engine);
    common::feed_handle(&screen, &handle, &idle());
    tokio::time::sleep(Duration::from_millis(100)).await;
    handle.record_status_event(WindowStatus::Running);
    tokio::time::sleep(Duration::from_millis(200)).await;
    tokio::time::sleep(Duration::from_millis(TURN_QUIET_MS + 1_000)).await;

    let mut confirmed = Vec::new();
    while let Ok(change) = statuses.try_recv() {
        confirmed.push(change.status);
    }
    assert!(
        confirmed.contains(&DetectedStatus::Running),
        "the report opened a turn: {confirmed:?}"
    );
    assert_eq!(
        confirmed.last(),
        Some(&DetectedStatus::WaitingInput),
        "no event arrived after the silence began, so only a deadline can have moved the status: {confirmed:?}"
    );
}

/// A silent turn stays closed until the next `Running` report, which opens it
/// again; the turn then goes quiet again the same way.
#[tokio::test(start_paused = true)]
async fn the_next_running_report_opens_a_turn_that_went_quiet() {
    let (mut driver, _) = cases_over_a_quiet_looking_screen()
        .remove(0)
        .open_turn()
        .await;
    assert_eq!(
        driver.detected_at(TURN_QUIET_MS + 600).await,
        DetectedStatus::WaitingInput
    );
    driver.feed_at(8_000, CHUNK).await;
    driver.report_at(9_000, WindowStatus::Running).await;
    assert_eq!(driver.detected_at(9_500).await, DetectedStatus::Running);
    assert_eq!(
        driver.detected_at(8_000 + TURN_QUIET_MS + 600).await,
        DetectedStatus::WaitingInput,
        "the reopened turn goes quiet like any other"
    );
}

/// A `WaitingApproval` report also opens a turn that went quiet: with no
/// dialog readable on the screen the approval stands.
#[tokio::test(start_paused = true)]
async fn an_approval_report_opens_a_turn_that_went_quiet() {
    let (mut driver, _) = cases_over_a_quiet_looking_screen()
        .remove(0)
        .open_turn()
        .await;
    assert_eq!(
        driver.detected_at(TURN_QUIET_MS + 600).await,
        DetectedStatus::WaitingInput
    );
    driver.feed_at(7_000, &unmatched()).await;
    driver.report_at(7_100, WindowStatus::WaitingApproval).await;
    assert_eq!(
        driver.detected_at(7_600).await,
        DetectedStatus::WaitingApproval
    );
}

// =====================================================================
// Closing reports
// =====================================================================

/// A report that the turn ended closes it at once, even while output still
/// arrives, and it stays closed: only a later opening report reopens it.
#[tokio::test(start_paused = true)]
async fn a_closing_report_ends_the_turn_while_output_keeps_arriving() {
    for closing in [
        WindowStatus::WaitingInput,
        WindowStatus::Idle,
        WindowStatus::Interrupted,
    ] {
        let mut driver = Driver::synthetic(SCREEN_DEF);
        driver.feed_at(0, &unmatched()).await;
        driver.report_at(200, WindowStatus::Running).await;
        for second in 1..=2 {
            driver.feed_at(second * 1_000, CHUNK).await;
        }
        assert_eq!(
            driver.detected_at(2_500).await,
            DetectedStatus::Running,
            "{closing:?}: the turn is open before the closing report"
        );
        driver.report_at(3_000, closing).await;
        for second in 4..=10 {
            driver.feed_at(second * 1_000, CHUNK).await;
        }
        assert_eq!(
            driver.first_rejected(3_500, 10_500, |d| d == NO_RULE),
            None,
            "{closing:?}: after the closing report an unreadable screen stays Unknown"
        );
    }
}

/// A closed turn on a screen no rule matches is `Unknown`, not
/// `WaitingInput` -- for as long as it lasts, since a report never expires.
#[tokio::test(start_paused = true)]
async fn a_closed_turn_on_an_unmatched_screen_stays_unknown_without_expiry() {
    let mut driver = Driver::synthetic(SCREEN_DEF);
    driver.feed_at(0, &unmatched()).await;
    driver.report_at(200, WindowStatus::WaitingInput).await;
    assert_eq!(driver.detected_at(1_000).await, NO_RULE);
    assert_eq!(driver.detected_at(60_000).await, NO_RULE);
    assert_eq!(driver.detected_at(600_000).await, NO_RULE);
}

/// A report that withdraws itself returns the window to what the screen says,
/// including any turn the earlier report had opened.
#[tokio::test(start_paused = true)]
async fn an_unknown_or_exited_report_returns_to_the_screen_alone() {
    for withdrawing in [WindowStatus::Unknown, WindowStatus::Exited] {
        let mut driver = Driver::synthetic(SCREEN_DEF);
        driver.feed_at(0, &idle()).await;
        driver.report_at(200, WindowStatus::Running).await;
        assert_eq!(driver.detected_at(1_000).await, DetectedStatus::Running);
        driver.report_at(1_500, withdrawing).await;
        driver.feed_at(1_800, CHUNK).await;
        assert_eq!(
            driver.detected_at(2_200).await,
            DetectedStatus::WaitingInput,
            "{withdrawing:?}: the open turn is gone and the screen is idle"
        );
    }
}

// =====================================================================
// The screen wins
// =====================================================================

/// A screen the rules positively classify is the state, whether the last
/// report opened or closed the turn.
#[tokio::test(start_paused = true)]
async fn a_screen_the_rules_classify_outranks_any_report() {
    let screens: [(&str, Vec<u8>, DetectedStatus); 4] = [
        (
            "approval dialog",
            approval(),
            DetectedStatus::WaitingApproval,
        ),
        (
            "interrupted screen",
            interrupted_screen(),
            DetectedStatus::Interrupted,
        ),
        (
            "declared unknown",
            declared_unknown(),
            DetectedStatus::Unknown(UnknownReason::DeclaredUnknown),
        ),
        ("running footer", running_footer(), DetectedStatus::Running),
    ];
    for report in [
        WindowStatus::Running,
        WindowStatus::WaitingApproval,
        WindowStatus::WaitingInput,
        WindowStatus::Interrupted,
    ] {
        for (name, bytes, expected) in &screens {
            let mut driver = Driver::synthetic(SCREEN_DEF);
            driver.feed_at(0, &idle()).await;
            driver.report_at(200, report).await;
            driver.feed_at(1_000, bytes).await;
            assert_eq!(
                driver.detected_at(1_500).await,
                *expected,
                "report {report:?} over the {name}"
            );
        }
    }
}

/// Control: the child's exit outranks every report.
#[tokio::test(start_paused = true)]
async fn the_childs_exit_outranks_an_open_turn() {
    let mut driver = Driver::synthetic(SCREEN_DEF);
    driver.feed_at(0, &idle()).await;
    driver.report_at(200, WindowStatus::Running).await;
    driver.advance_to(500).await;
    driver.engine.on_exit();
    driver.feed_at(600, CHUNK).await;
    assert_eq!(driver.detected_at(1_000).await, DetectedStatus::Exited);
}

/// A paint under way after the child exited does not hold the evaluation back,
/// so the open turn's `Running` never overwrites `Exited`. Only a definition
/// with `confirm_ms` below the settle ceiling (here 5 ms) can reach this.
#[tokio::test(start_paused = true)]
async fn the_childs_exit_outranks_an_open_turn_while_a_paint_is_under_way() {
    let mut driver = Driver::synthetic(SCREEN_DEF);
    driver.feed_at(0, &idle()).await;
    driver.report_at(200, WindowStatus::Running).await;
    driver.advance_to(500).await;
    driver.engine.on_exit();
    driver.feed_at(600, HALF_DRAWN).await;
    driver.feed_at(610, HALF_DRAWN).await;
    driver.advance_to(1_000).await;
    assert_eq!(
        driver.first_rejected(600, 1_000, |d| d == DetectedStatus::Exited),
        None
    );
}

/// An approval is settled by the screen only after the dialog itself was read
/// while the approval report stood: a running footer or a screen at rest seen
/// before that, or with no approval report at all, settles nothing.
#[tokio::test(start_paused = true)]
async fn the_screen_settles_an_approval_only_after_its_dialog_was_read() {
    // No report: a dialog followed by a running footer opens no turn, so the
    // unreadable screen after it stays unknown.
    let mut driver = Driver::synthetic(SCREEN_DEF);
    driver.feed_at(0, &approval()).await;
    driver.feed_at(500, &running_footer()).await;
    driver.feed_at(1_000, &unmatched()).await;
    assert_eq!(driver.detected_at(1_500).await, NO_RULE, "no report");

    // The report arrives over a running footer: that footer is not the
    // answer to a dialog nobody has seen, so the approval stands.
    let mut driver = Driver::synthetic(SCREEN_DEF);
    driver.feed_at(0, &running_footer()).await;
    driver.report_at(500, WindowStatus::WaitingApproval).await;
    driver.feed_at(1_000, &unmatched()).await;
    assert_eq!(
        driver.detected_at(1_500).await,
        DetectedStatus::WaitingApproval,
        "report over a running footer"
    );

    // The same over a screen at rest.
    let mut driver = Driver::synthetic(SCREEN_DEF);
    driver.feed_at(0, &idle()).await;
    driver.report_at(500, WindowStatus::WaitingApproval).await;
    driver.feed_at(1_000, &unmatched()).await;
    assert_eq!(
        driver.detected_at(1_500).await,
        DetectedStatus::WaitingApproval,
        "report over a screen at rest"
    );
}

/// An approval report does not hide a screen that plainly waits for input.
#[tokio::test(start_paused = true)]
async fn an_approval_report_yields_to_a_screen_that_plainly_waits_for_input() {
    let mut driver = Driver::synthetic(SCREEN_DEF);
    driver.feed_at(0, &idle()).await;
    driver.report_at(200, WindowStatus::WaitingApproval).await;
    driver.feed_at(800, CHUNK).await;
    assert_eq!(
        driver.detected_at(1_000).await,
        DetectedStatus::WaitingInput
    );
}

/// Control: when the dialog cannot be read at all (a tile too small for it)
/// the approval report stands.
#[tokio::test(start_paused = true)]
async fn an_approval_report_stands_while_the_screen_shows_nothing_the_rules_know() {
    let mut driver = Driver::synthetic(SCREEN_DEF);
    driver.feed_at(0, &unmatched()).await;
    driver.report_at(200, WindowStatus::WaitingApproval).await;
    assert_eq!(
        driver.detected_at(1_000).await,
        DetectedStatus::WaitingApproval
    );
}

/// A CLI need not report that an approval was granted (Claude Code's next
/// report is the tool's end), so the screen moving from the dialog to a
/// running footer ends the approval: the tool's run is a running turn, and a
/// screen the rules then cannot read keeps it running, not awaiting approval.
#[tokio::test(start_paused = true)]
async fn an_approval_the_screen_shows_granted_continues_as_a_running_turn() {
    let mut driver = Driver::synthetic(SCREEN_DEF);
    driver.feed_at(0, &approval()).await;
    driver.report_at(200, WindowStatus::WaitingApproval).await;
    assert_eq!(
        driver.detected_at(1_000).await,
        DetectedStatus::WaitingApproval
    );
    driver.feed_at(1_500, &running_footer()).await;
    assert_eq!(driver.detected_at(2_000).await, DetectedStatus::Running);

    driver.feed_at(2_500, &unmatched()).await;
    for ms in (3_000..=6_000).step_by(500) {
        driver.feed_at(ms, CHUNK).await;
    }
    assert_eq!(
        driver.first_rejected(3_000, 6_000, |seen| seen == DetectedStatus::Running),
        None,
        "the granted approval's tool run must stay running over an unreadable screen"
    );
}

/// The dialog giving way to a screen that waits for input means the approval
/// was refused: nothing is left of the turn, so an unreadable screen after it
/// is unknown, not awaiting approval.
#[tokio::test(start_paused = true)]
async fn an_approval_the_screen_shows_refused_ends_the_turn() {
    let mut driver = Driver::synthetic(SCREEN_DEF);
    driver.feed_at(0, &approval()).await;
    driver.report_at(200, WindowStatus::WaitingApproval).await;
    driver.feed_at(1_500, &idle()).await;
    assert_eq!(
        driver.detected_at(2_000).await,
        DetectedStatus::WaitingInput
    );
    driver.feed_at(2_500, &unmatched()).await;
    assert_eq!(driver.detected_at(3_000).await, NO_RULE);
}

// =====================================================================
// Typing and resizing are not output
// =====================================================================

/// The echo of a keystroke neither closes the turn nor counts as output that
/// keeps it running, with and without time tables in the definition.
#[tokio::test(start_paused = true)]
async fn a_keystroke_and_its_echo_neither_close_the_turn_nor_extend_it() {
    for definition in [SCREEN_DEF.to_string(), tabled_def()] {
        let mut driver = Driver::synthetic(&definition);
        driver.feed_at(0, &idle()).await;
        driver.report_at(200, WindowStatus::Running).await;

        driver.advance_to(1_000).await;
        driver.engine.on_input(InputEcho::EchoOnly);
        driver.feed_at(1_020, b"a").await;
        assert_eq!(
            driver.detected_at(3_000).await,
            DetectedStatus::Running,
            "typing must not close an open turn"
        );

        driver.advance_to(4_000).await;
        driver.engine.on_input(InputEcho::EchoOnly);
        driver.feed_at(4_020, b"b").await;
        assert_eq!(
            driver.detected_at(TURN_QUIET_MS + 600).await,
            DetectedStatus::WaitingInput,
            "an echo must not count as recent output"
        );
    }
}

/// The redraw that follows a resize is not output that keeps a turn open.
#[tokio::test(start_paused = true)]
async fn a_resize_redraw_does_not_extend_the_turn() {
    for definition in [SCREEN_DEF.to_string(), tabled_def()] {
        let mut driver = Driver::synthetic(&definition);
        driver.feed_at(0, &idle()).await;
        driver.report_at(200, WindowStatus::Running).await;

        driver.advance_to(4_000).await;
        driver.engine.on_resize_started();
        driver.engine.screen().lock().resize(COLS, ROWS);
        driver.engine.on_resized();
        driver.feed_at(4_040, &idle()).await;
        assert_eq!(
            driver.detected_at(TURN_QUIET_MS + 600).await,
            DetectedStatus::WaitingInput,
            "a resize redraw must not count as recent output"
        );
    }
}

// =====================================================================
// A screen that was lost is not rescued by a report
// =====================================================================

/// After a screen loss the window stays `Unknown` for as long as the screen
/// is gone, whether the last report opened or closed the turn.
#[tokio::test(start_paused = true)]
async fn a_report_does_not_restore_a_lost_screen() {
    for report in [WindowStatus::Running, WindowStatus::WaitingInput] {
        let mut driver = Driver::synthetic(SCREEN_DEF);
        driver.feed_at(0, &idle()).await;
        driver.report_at(200, report).await;
        driver.advance_to(1_000).await;
        driver.engine.lose_screen();
        let lost = DetectedStatus::Unknown(UnknownReason::ScreenLost);
        assert_eq!(driver.engine.detected(), lost, "{report:?}: lost at once");
        assert_eq!(driver.detected_at(1_500).await, lost, "{report:?}");
        driver.feed_at(2_000, CHUNK).await;
        assert_eq!(
            driver.detected_at(2_500).await,
            lost,
            "{report:?}: after more output"
        );
        driver.report_at(3_000, WindowStatus::Running).await;
        assert_eq!(
            driver.detected_at(3_500).await,
            lost,
            "{report:?}: after another report"
        );
    }
}

// =====================================================================
// A paint under way
// =====================================================================

const HALF_DRAWN: &[u8] = "\x1b[?25l\x1b[2J\x1b[3;1H  partial".as_bytes();
const REST_OF_PAINT: &[u8] = "\x1b[2;1H› \x1b[?25h".as_bytes();

/// An opening report with output just arrived needs no screen, so a paint
/// under way does not hold it back.
#[tokio::test(start_paused = true)]
async fn an_opening_report_is_judged_without_waiting_for_the_paint_to_finish() {
    let mut driver = Driver::synthetic(SCREEN_DEF);
    driver.feed_at(0, &idle()).await;
    driver.advance_to(100).await;
    driver.feed_at(200, HALF_DRAWN).await;
    driver.report_at(202, WindowStatus::Running).await;
    assert_eq!(driver.detected_at(215).await, DetectedStatus::Running);
}

/// A closing report is read against the screen, so it waits for the paint to
/// finish and then yields to the finished screen.
#[tokio::test(start_paused = true)]
async fn a_closing_report_waits_for_the_paint_to_finish() {
    let mut driver = Driver::synthetic(SCREEN_DEF);
    driver.feed_at(0, &idle()).await;
    driver.advance_to(100).await;
    driver.feed_at(200, HALF_DRAWN).await;
    driver.report_at(202, WindowStatus::Interrupted).await;
    driver.advance_to(215).await;
    driver.feed_at(220, REST_OF_PAINT).await;
    driver.advance_to(400).await;
    assert_eq!(
        driver.first_rejected(200, 400, |d| d == DetectedStatus::WaitingInput),
        None,
        "a closing report must not pin a status before the finished screen is read"
    );
}
