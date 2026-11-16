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

use crate::common;

use common::{CopiedEngine, ScreenFeed};
use std::path::Path;
use std::time::Duration;

use tokio::time::Instant;
use yagura_daemon::DaemonConfig;
use yagura_daemon::detector::engine::{
    DetectorHandle, InputEcho, TURN_ACTIVE_QUIET, TURN_REPORT_STALE,
};
use yagura_daemon::input_encode::NamedPress;
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

/// The same rules plus a key whose press ends the turn once the running
/// footer has named it since the last submit.
fn press_def() -> String {
    format!("{SCREEN_DEF}\n[turn_end_key]\nkey = \"escape\"\nshown = 'esc to interrupt'\n")
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
    engine: CopiedEngine,
    samples: Vec<(u64, DetectedStatus)>,
}

impl Driver {
    fn new(engine: CopiedEngine) -> Self {
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

    async fn input_at(&mut self, ms: u64, echo: InputEcho) {
        self.advance_to(ms).await;
        self.engine.on_input(echo);
        self.sample();
    }

    async fn press_at(&mut self, ms: u64, press: NamedPress) {
        self.advance_to(ms).await;
        let _ = self.engine.on_press(press);
        self.sample();
    }

    /// An interrupt's write of `press`: its input notification, then the
    /// press it carries, in the order the window sends them.
    async fn interrupt_at(&mut self, ms: u64, press: NamedPress) {
        self.advance_to(ms).await;
        self.engine.on_interrupt_input(InputEcho::MayStartWork);
        let _ = self.engine.on_press(press);
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

    // With no press there is no hold, so a confirmed running screen opens
    // no turn and an unreadable screen after it stays unknown.
    let mut failures: Vec<String> = Vec::new();
    driver.feed_at(13_000, &running_footer()).await;
    driver.feed_at(13_500, &unmatched()).await;
    for ms in [14_000, 15_000, 16_000] {
        driver.feed_at(ms, CHUNK).await;
    }
    check_window(
        &mut failures,
        &driver,
        "without a hold a confirmed running screen opens no turn",
        13_605,
        16_000,
        |d| d == NO_RULE,
    );

    // In a window that has had reports, with no press there is still no
    // hold, so a confirmed running screen after a closing report opens no turn.
    let mut driver = Driver::synthetic(&press_def());
    driver.feed_at(0, &running_footer()).await;
    driver.report_at(100, WindowStatus::Running).await;
    driver.report_at(500, WindowStatus::WaitingInput).await;
    driver.feed_at(600, &idle()).await;
    driver.feed_at(1_000, &running_footer()).await;
    driver.feed_at(1_100, &unmatched()).await;
    for ms in [1_500, 2_000] {
        driver.feed_at(ms, CHUNK).await;
    }
    check_window(
        &mut failures,
        &driver,
        "without a hold a confirmed running screen after a closing report opens no turn",
        1_205,
        2_000,
        |d| d == NO_RULE,
    );
    assert!(
        failures.is_empty(),
        "without a press's hold a confirmed running screen opened a turn:\n{}",
        failures.join("\n")
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

    // A press that ends nothing leaves the open turn running.
    let mut failures: Vec<String> = Vec::new();

    // A definition without the table ends no turn on any key.
    let mut driver = Driver::synthetic(SCREEN_DEF);
    driver.feed_at(0, &running_footer()).await;
    driver.report_at(100, WindowStatus::Running).await;
    driver.feed_at(1_000, &idle()).await;
    driver.press_at(1_500, NamedPress::Escape).await;
    driver.press_at(2_000, NamedPress::CtrlC).await;
    for ms in (2_500..=5_000).step_by(1_000) {
        driver.feed_at(ms, CHUNK).await;
    }
    driver.advance_to(5_000).await;
    check_window(
        &mut failures,
        &driver,
        "a definition without the table ends no turn on a press",
        1_600,
        5_000,
        |d| d == DetectedStatus::Running,
    );

    // A screen that never named the key since the last submit is not
    // ended by its press.
    let mut driver = Driver::synthetic(&press_def());
    driver.feed_at(0, &idle()).await;
    driver.report_at(100, WindowStatus::Running).await;
    driver.feed_at(200, &unmatched()).await;
    driver.feed_at(600, CHUNK).await;
    driver.press_at(1_000, NamedPress::Escape).await;
    for ms in (2_000..=4_000).step_by(1_000) {
        driver.feed_at(ms, CHUNK).await;
    }
    check_window(
        &mut failures,
        &driver,
        "a screen that never showed the key is not ended by its press",
        1_100,
        4_000,
        |d| d == DetectedStatus::Running,
    );

    // A key other than the declared one ends nothing.
    let mut driver = Driver::synthetic(&press_def());
    driver.feed_at(0, &running_footer()).await;
    driver.report_at(100, WindowStatus::Running).await;
    driver.feed_at(1_000, &idle()).await;
    driver.press_at(1_500, NamedPress::CtrlC).await;
    for ms in (2_500..=5_000).step_by(1_000) {
        driver.feed_at(ms, CHUNK).await;
    }
    driver.advance_to(5_000).await;
    check_window(
        &mut failures,
        &driver,
        "a key other than the declared one ends no turn",
        1_600,
        5_000,
        |d| d == DetectedStatus::Running,
    );

    assert!(
        failures.is_empty(),
        "a press that must end nothing ended the turn:\n{}",
        failures.join("\n")
    );
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
    let (handle, mut statuses) = DetectorHandle::spawn(engine.into_engine());
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
    drop(handle);

    // Through the handle, with the table declared: the daemon moving the CLI
    // on ends the press's hold, so only the report after it opens the turn.
    // Each step lands well inside the quiet time and the changes are read
    // before it runs out, so the last change is the report's own.
    let mut failures: Vec<String> = Vec::new();
    let engine = common::new_engine_sized(&press_def(), &config, COLS, ROWS);
    let screen = engine.screen().clone();
    let (handle, mut statuses) = DetectorHandle::spawn(engine.into_engine());
    let step = Duration::from_millis(100);
    common::feed_handle(&screen, &handle, &running_footer());
    tokio::time::sleep(step).await;
    handle.record_status_event(WindowStatus::Running);
    tokio::time::sleep(step).await;
    common::feed_handle(&screen, &handle, &idle());
    tokio::time::sleep(step).await;
    handle.notify_press(NamedPress::Escape);
    tokio::time::sleep(step).await;
    handle.record_status_event(WindowStatus::Running);
    tokio::time::sleep(step).await;
    let mut before_moved_on = Vec::new();
    while let Ok(change) = statuses.try_recv() {
        before_moved_on.push(change.status);
    }
    if before_moved_on.last() != Some(&DetectedStatus::WaitingInput) {
        failures.push(format!(
            "a report between the press and the daemon moving the CLI on was published: {before_moved_on:?}"
        ));
    }
    handle.notify_cli_moved_on();
    tokio::time::sleep(step).await;
    handle.record_status_event(WindowStatus::Running);
    tokio::time::sleep(step).await;
    let mut after_moved_on = Vec::new();
    while let Ok(change) = statuses.try_recv() {
        after_moved_on.push(change.status);
    }
    if after_moved_on.last() != Some(&DetectedStatus::Running) {
        failures.push(format!(
            "the daemon moving the CLI on did not let the next report open the turn: {after_moved_on:?}"
        ));
    }
    assert!(
        failures.is_empty(),
        "the daemon moving the CLI on did not end the press's hold through the handle:\n{}",
        failures.join("\n")
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

    // After a press, which later facts open the next turn and which do not.
    let mut failures: Vec<String> = Vec::new();

    // With the turn closed by a report and nothing submitted since, a press
    // holds nothing back, so the next opening report opens a turn.
    let mut driver = Driver::synthetic(&press_def());
    driver.input_at(0, InputEcho::MayStartWork).await;
    driver.feed_at(50, &running_footer()).await;
    driver.report_at(100, WindowStatus::Running).await;
    driver.report_at(500, WindowStatus::WaitingInput).await;
    driver.feed_at(600, &idle()).await;
    driver.press_at(1_000, NamedPress::Escape).await;
    driver.report_at(1_500, WindowStatus::Running).await;
    driver.feed_at(1_600, &unmatched()).await;
    let seen = driver.detected_at(1_705).await;
    if seen != DetectedStatus::Running {
        failures.push(format!(
            "a press after a closed turn with nothing submitted held back the next report: {seen:?} at 1705 ms"
        ));
    }

    // On the same window: an interrupt into a turn a report closed starts
    // nothing and ends nothing, so the next opening report opens a turn.
    driver.report_at(2_000, WindowStatus::WaitingInput).await;
    driver.feed_at(2_100, &idle()).await;
    driver.interrupt_at(2_500, NamedPress::Escape).await;
    driver.report_at(3_000, WindowStatus::Running).await;
    driver.feed_at(3_100, &unmatched()).await;
    let seen = driver.detected_at(3_205).await;
    if seen != DetectedStatus::Running {
        failures.push(format!(
            "an interrupt into a turn a report closed held back the next report: {seen:?} at 3205 ms"
        ));
    }

    // A press just after a submit, before the opening report arrives,
    // ends the turn the submit began, and the late report does not reopen it.
    let mut driver = Driver::synthetic(&press_def());
    driver.feed_at(0, &running_footer()).await;
    driver.feed_at(100, &idle()).await;
    driver.input_at(1_000, InputEcho::MayStartWork).await;
    driver.feed_at(1_100, &running_footer()).await;
    driver.press_at(2_000, NamedPress::Escape).await;
    driver.feed_at(2_100, &idle()).await;
    driver.report_at(2_700, WindowStatus::Running).await;
    for ms in (3_000..=8_000).step_by(1_000) {
        driver.feed_at(ms, CHUNK).await;
    }
    check_window(
        &mut failures,
        &driver,
        "a press just after a submit ends the turn before its report",
        2_805,
        8_000,
        |d| d == DetectedStatus::WaitingInput,
    );

    // A press after the turn went quiet still ends what the submit
    // began, so the late report does not reopen it.
    let mut driver = Driver::synthetic(&press_def());
    driver.input_at(0, InputEcho::MayStartWork).await;
    driver.feed_at(50, &running_footer()).await;
    driver.report_at(100, WindowStatus::Running).await;
    driver.feed_at(200, &unmatched()).await;
    driver.press_at(6_000, NamedPress::Escape).await;
    driver.report_at(6_500, WindowStatus::Running).await;
    for ms in (7_000..=10_000).step_by(1_000) {
        driver.feed_at(ms, CHUNK).await;
    }
    check_window(
        &mut failures,
        &driver,
        "a press after the turn went quiet still holds back the late report",
        6_605,
        10_000,
        |d| d == NO_RULE,
    );

    // While the press holds the turn back, the screen confirming running
    // opens it again.
    let mut driver = Driver::synthetic(&press_def());
    driver.feed_at(0, &running_footer()).await;
    driver.report_at(100, WindowStatus::Running).await;
    driver.feed_at(1_000, &idle()).await;
    driver.press_at(1_500, NamedPress::Escape).await;
    driver.feed_at(2_000, &running_footer()).await;
    driver.feed_at(2_100, &unmatched()).await;
    for ms in [2_500, 3_000] {
        driver.feed_at(ms, CHUNK).await;
    }
    check_window(
        &mut failures,
        &driver,
        "a confirmed running screen during the hold opens the turn",
        2_205,
        3_000,
        |d| d == DetectedStatus::Running,
    );

    // In a window that never had a report, the confirmed running screen
    // releases the hold but opens no turn, so an unreadable screen stays unknown.
    let mut driver = Driver::synthetic(&press_def());
    driver.input_at(0, InputEcho::MayStartWork).await;
    driver.feed_at(50, &running_footer()).await;
    driver.feed_at(1_000, &idle()).await;
    driver.press_at(1_500, NamedPress::Escape).await;
    driver.feed_at(2_000, &running_footer()).await;
    driver.feed_at(2_100, &unmatched()).await;
    for ms in [2_500, 3_000] {
        driver.feed_at(ms, CHUNK).await;
    }
    check_window(
        &mut failures,
        &driver,
        "a window with no report opens no turn when the hold is released",
        2_205,
        3_000,
        |d| d == NO_RULE,
    );

    // A running screen gone before its confirmation does not open the
    // turn, so the report during the hold is still held back.
    let mut driver = Driver::synthetic(&press_def());
    driver.feed_at(0, &running_footer()).await;
    driver.report_at(100, WindowStatus::Running).await;
    driver.feed_at(1_000, &idle()).await;
    driver.press_at(1_500, NamedPress::Escape).await;
    driver.feed_at(2_000, &running_footer()).await;
    driver.feed_at(2_002, &idle()).await;
    driver.report_at(2_100, WindowStatus::Running).await;
    driver.feed_at(2_200, &unmatched()).await;
    for ms in (2_500..=4_000).step_by(500) {
        driver.feed_at(ms, CHUNK).await;
    }
    check_window(
        &mut failures,
        &driver,
        "an unconfirmed running screen does not open the held turn",
        2_305,
        4_000,
        |d| d == NO_RULE,
    );

    // A submit since the screen last named the key withdraws that
    // evidence, so the press ends nothing until the screen names it again.
    let mut driver = Driver::synthetic(&press_def());
    driver.feed_at(0, &running_footer()).await;
    driver.report_at(100, WindowStatus::Running).await;
    driver.report_at(500, WindowStatus::WaitingInput).await;
    driver.feed_at(600, &unmatched()).await;
    driver.input_at(1_000, InputEcho::MayStartWork).await;
    driver.report_at(1_100, WindowStatus::Running).await;
    driver.feed_at(1_500, CHUNK).await;
    driver.press_at(2_000, NamedPress::Escape).await;
    for ms in (3_000..=5_000).step_by(1_000) {
        driver.feed_at(ms, CHUNK).await;
    }
    check_window(
        &mut failures,
        &driver,
        "a press after a submit with no footer since ends no turn",
        2_100,
        5_000,
        |d| d == DetectedStatus::Running,
    );

    assert!(
        failures.is_empty(),
        "the turn after a press opened or stayed closed against the facts:\n{}",
        failures.join("\n")
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

    // The declared key's press ends the turn the way a closing report does.
    let mut failures: Vec<String> = Vec::new();

    // The next three checks run on one window.
    let mut driver = Driver::synthetic(&press_def());
    driver.feed_at(0, &running_footer()).await;
    driver.report_at(100, WindowStatus::Running).await;
    driver.feed_at(1_000, &unmatched()).await;
    driver.feed_at(2_000, CHUNK).await;
    driver.feed_at(2_500, &idle()).await;
    driver.press_at(3_000, NamedPress::Escape).await;
    for ms in (4_000..=10_000).step_by(1_000) {
        driver.feed_at(ms, CHUNK).await;
    }
    check_window(
        &mut failures,
        &driver,
        "the press ends the turn while output keeps arriving",
        3_105,
        10_000,
        |d| d == DetectedStatus::WaitingInput,
    );
    driver.report_at(11_000, WindowStatus::Running).await;
    for ms in (12_000..=15_000).step_by(1_000) {
        driver.feed_at(ms, CHUNK).await;
    }
    check_window(
        &mut failures,
        &driver,
        "an opening report after the press does not reopen the turn",
        11_105,
        15_000,
        |d| d == DetectedStatus::WaitingInput,
    );
    driver.input_at(16_000, InputEcho::MayStartWork).await;
    driver.report_at(16_500, WindowStatus::Running).await;
    driver.feed_at(17_000, &idle()).await;
    driver.feed_at(18_000, CHUNK).await;
    driver.advance_to(18_500).await;
    check_window(
        &mut failures,
        &driver,
        "a submit releases the hold and the next report opens the turn",
        16_605,
        18_500,
        |d| d == DetectedStatus::Running,
    );

    // The press is judged at once, with no output after it.
    let mut driver = Driver::synthetic(&press_def());
    driver.feed_at(0, &running_footer()).await;
    driver.report_at(100, WindowStatus::Running).await;
    driver.feed_at(1_000, &idle()).await;
    driver.press_at(2_000, NamedPress::Escape).await;
    let seen = driver.detected_at(2_105).await;
    if seen != DetectedStatus::WaitingInput {
        failures.push(format!(
            "the press was not judged at once: {seen:?} at 2105 ms"
        ));
    }

    // A closing report after the press releases the hold, so the next
    // opening report opens a turn.
    let mut driver = Driver::synthetic(&press_def());
    driver.feed_at(0, &running_footer()).await;
    driver.report_at(100, WindowStatus::Running).await;
    driver.feed_at(1_000, &idle()).await;
    driver.press_at(1_500, NamedPress::Escape).await;
    driver.report_at(2_000, WindowStatus::WaitingInput).await;
    driver.report_at(3_000, WindowStatus::Running).await;
    driver.feed_at(3_100, &unmatched()).await;
    for ms in [3_500, 4_000] {
        driver.feed_at(ms, CHUNK).await;
    }
    check_window(
        &mut failures,
        &driver,
        "a closing report releases the hold",
        3_205,
        4_000,
        |d| d == DetectedStatus::Running,
    );

    // On the same window: an interrupt carrying the key ends the turn the
    // report opened, and an interrupt after the turn is closed starts nothing,
    // so it leaves the hold in place and a late report does not reopen the turn.
    driver.feed_at(4_500, &idle()).await;
    driver.interrupt_at(5_000, NamedPress::Escape).await;
    driver.report_at(5_500, WindowStatus::Running).await;
    driver.interrupt_at(6_000, NamedPress::Escape).await;
    driver.report_at(6_500, WindowStatus::Running).await;
    for ms in [7_000, 7_500, 8_000] {
        driver.feed_at(ms, CHUNK).await;
    }
    check_window(
        &mut failures,
        &driver,
        "an interrupt after the press closed the turn does not release the hold",
        5_105,
        8_000,
        |d| d == DetectedStatus::WaitingInput,
    );

    assert!(
        failures.is_empty(),
        "the declared key's press did not end the turn as a closing report does:\n{}",
        failures.join("\n")
    );
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

    // The declared key's press ends a turn that waits on an approval too.
    let mut failures: Vec<String> = Vec::new();
    let mut driver = Driver::synthetic(&press_def());
    driver.feed_at(0, &running_footer()).await;
    driver.report_at(100, WindowStatus::WaitingApproval).await;
    driver.feed_at(200, &unmatched()).await;
    driver.press_at(1_000, NamedPress::Escape).await;
    let seen = driver.detected_at(1_105).await;
    if seen != NO_RULE {
        failures.push(format!(
            "the press did not end the approval's turn: {seen:?} at 1105 ms"
        ));
    }
    assert!(
        failures.is_empty(),
        "an approval's turn outlived the press:\n{}",
        failures.join("\n")
    );
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

    // Typing after the press is not the next turn's start, so a late
    // opening report stays held back.
    let mut failures: Vec<String> = Vec::new();
    let mut driver = Driver::synthetic(&press_def());
    driver.feed_at(0, &running_footer()).await;
    driver.report_at(100, WindowStatus::Running).await;
    driver.feed_at(1_000, &idle()).await;
    driver.press_at(1_500, NamedPress::Escape).await;
    driver.input_at(2_000, InputEcho::EchoOnly).await;
    driver.report_at(2_100, WindowStatus::Running).await;
    for ms in [3_000, 4_000] {
        driver.feed_at(ms, CHUNK).await;
    }
    check_window(
        &mut failures,
        &driver,
        "typing after the press does not release the hold",
        2_205,
        4_000,
        |d| d == DetectedStatus::WaitingInput,
    );
    assert!(
        failures.is_empty(),
        "a keystroke released the press's hold:\n{}",
        failures.join("\n")
    );
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

    // The press ends the turn while the screen is lost too, so the screen
    // that comes back is judged with the turn closed.
    let mut failures: Vec<String> = Vec::new();
    let mut driver = Driver::synthetic(&press_def());
    driver.feed_at(0, &running_footer()).await;
    driver.report_at(100, WindowStatus::Running).await;
    driver.advance_to(1_000).await;
    driver.engine.lose_screen();
    driver.sample();
    driver.press_at(1_500, NamedPress::Escape).await;
    driver.feed_at(2_000, &idle()).await;
    for ms in (2_500..=14_500).step_by(1_000) {
        driver.feed_at(ms, CHUNK).await;
    }
    driver.advance_to(15_000).await;
    check_window(
        &mut failures,
        &driver,
        "the press ends the turn while the screen is lost",
        2_105,
        15_000,
        |d| d == DetectedStatus::WaitingInput,
    );
    assert!(
        failures.is_empty(),
        "a lost screen kept the press from ending the turn:\n{}",
        failures.join("\n")
    );
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

// =====================================================================
// A screen that waits on the user stops both turn clocks
// =====================================================================

/// Records one failure per window whose samples break `is_ok`, and one when the
/// window holds no sample at all (a window nothing was measured in proves
/// nothing).
fn check_window(
    failures: &mut Vec<String>,
    driver: &Driver,
    what: &str,
    from_ms: u64,
    to_ms: u64,
    is_ok: impl Fn(DetectedStatus) -> bool,
) {
    let seen = driver
        .samples
        .iter()
        .filter(|(ms, _)| (from_ms..=to_ms).contains(ms))
        .count();
    if seen == 0 {
        failures.push(format!("{what}: no sample in {from_ms}..={to_ms} ms"));
    } else if let Some((ms, status)) = driver.first_rejected(from_ms, to_ms, is_ok) {
        failures.push(format!("{what}: {status:?} at {ms} ms"));
    }
}

/// How one retreat from a screen that waits on the user is driven.
struct Retreat {
    stage: &'static str,
    /// The screen that waits on the user, and what it is confirmed as.
    hold_screen: Vec<u8>,
    hold_status: DetectedStatus,
    /// Milliseconds at which the screen retreats to a prompt.
    retreat_ms: u64,
    /// A keystroke echo just before the retreat output: the output is
    /// attributed to it and does not move the turn's output time.
    echo: bool,
    /// A `Running` report while the screen waits (a post-tool report during an
    /// approval).
    mid_report: bool,
    /// Also check the engine's wake-up instants (stage 3).
    deadlines: bool,
}

impl Retreat {
    /// A report opened the turn; a screen that waits on the user sits over it for
    /// longer than either clock allows; then the screen retreats. The turn is
    /// the one the report opened, so it is still running.
    async fn run(self, failures: &mut Vec<String>) {
        let stage = self.stage;
        let t = self.retreat_ms;
        let mut driver = Driver::synthetic(SCREEN_DEF);
        driver.feed_at(0, &idle()).await;
        driver.report_at(200, WindowStatus::Running).await;
        driver.feed_at(1_000, &self.hold_screen).await;
        if self.mid_report {
            driver.report_at(1_100, WindowStatus::Running).await;
        }
        if self.deadlines {
            driver.advance_to(1_200).await;
            let during = driver.engine.next_deadline();
            if during.is_some() {
                failures.push(format!(
                    "stage 3 (during the hold): deadline {during:?} while the screen waits on the user"
                ));
            }
        }
        if self.echo {
            driver.advance_to(t - 50).await;
            driver.engine.on_input(InputEcho::EchoOnly);
        }
        driver.feed_at(t, &idle()).await;
        if self.deadlines {
            driver.advance_to(t + 100).await;
            let expected = Some(driver.start + Duration::from_millis(t + TURN_QUIET_MS));
            let after = driver.engine.next_deadline();
            if after != expected {
                failures.push(format!(
                    "stage 3 (after the hold): next deadline {after:?}, expected {expected:?}"
                ));
            }
        }
        driver.advance_to(t + TURN_QUIET_MS - 500).await;

        let hold = self.hold_status;
        if self.mid_report {
            check_window(
                failures,
                &driver,
                &format!("{stage} (i) during the hold"),
                1_100,
                t - 1,
                |d| d == hold,
            );
        }
        check_window(
            failures,
            &driver,
            &format!("{stage} (ii) wide window after the retreat"),
            t,
            t + TURN_QUIET_MS - 500,
            |d| d == hold || d == DetectedStatus::Running,
        );
        check_window(
            failures,
            &driver,
            &format!("{stage} (iii) narrow window after the retreat"),
            t + 100,
            t + TURN_QUIET_MS - 500,
            |d| d == DetectedStatus::Running,
        );
        let later = driver.detected_at(t + TURN_QUIET_MS + 600).await;
        if later != DetectedStatus::WaitingInput {
            failures.push(format!(
                "{stage} (iv) after the quiet time: {later:?}, expected WaitingInput"
            ));
        }
    }
}

/// Time a screen spends waiting on the user (an approval box, or a screen the
/// definition declares unreadable) is the user's, not the CLI's silence: neither
/// the quiet time nor the stale time counts it, whichever way raw got there or
/// left, and the clocks resume where they stopped.
#[tokio::test(start_paused = true)]
async fn a_screen_waiting_on_the_user_is_not_counted_as_the_turns_silence() {
    let mut failures: Vec<String> = Vec::new();
    let past_the_quiet_time = 1_000 + TURN_QUIET_MS + 700;
    let approval_status = DetectedStatus::WaitingApproval;
    let declared = DetectedStatus::Unknown(UnknownReason::DeclaredUnknown);

    // Stages 1 and 3: the shape of a real approval, with a report in the middle.
    Retreat {
        stage: "stage 1",
        hold_screen: approval(),
        hold_status: approval_status,
        retreat_ms: past_the_quiet_time,
        echo: false,
        mid_report: true,
        deadlines: true,
    }
    .run(&mut failures)
    .await;
    // Stage 1b: the answering keystroke's echo is not output.
    Retreat {
        stage: "stage 1b",
        hold_screen: approval(),
        hold_status: approval_status,
        retreat_ms: past_the_quiet_time,
        echo: true,
        mid_report: true,
        deadlines: false,
    }
    .run(&mut failures)
    .await;
    // Stage 2: the report-age clock stops too.
    Retreat {
        stage: "stage 2",
        hold_screen: approval(),
        hold_status: approval_status,
        retreat_ms: TURN_STALE_MS + 2_000,
        echo: false,
        mid_report: false,
        deadlines: false,
    }
    .run(&mut failures)
    .await;
    // Stage 6: a screen the definition declares unreadable waits just the same.
    Retreat {
        stage: "stage 6",
        hold_screen: declared_unknown(),
        hold_status: declared,
        retreat_ms: past_the_quiet_time,
        echo: false,
        mid_report: true,
        deadlines: false,
    }
    .run(&mut failures)
    .await;

    stage_4_holds_do_not_reset_the_stale_clock(&mut failures).await;
    stage_5_a_lost_screen_inside_the_hold(&mut failures).await;
    stage_5b_a_screen_lost_for_longer_than_the_quiet_time(&mut failures).await;

    assert!(
        failures.is_empty(),
        "the screen waiting on the user was counted as silence:\n{}",
        failures.join("\n")
    );
}

/// Stage 4: time not spent waiting piles up toward the stale time across a hold
/// instead of restarting after it.
async fn stage_4_holds_do_not_reset_the_stale_clock(failures: &mut Vec<String>) {
    let mut driver = Driver::synthetic(SCREEN_DEF);
    driver.feed_at(0, &idle()).await;
    driver.report_at(200, WindowStatus::Running).await;
    let approval_at = 200 + TURN_STALE_MS - 10_000;
    let mut at = 200;
    while at + 4_000 < approval_at {
        at += 4_000;
        driver.feed_at(at, CHUNK).await;
    }
    driver.feed_at(approval_at, &approval()).await;
    let left = approval_at + 20_000;
    driver.feed_at(left, &idle()).await;
    let mut at = left;
    while at < left + 8_000 {
        at += 4_000;
        driver.feed_at(at, CHUNK).await;
    }
    driver.advance_to(left + 9_000).await;
    check_window(
        failures,
        &driver,
        "stage 4 narrow window after the retreat",
        left + 100,
        left + 9_000,
        |d| d == DetectedStatus::Running,
    );
    let later = driver.detected_at(left + 10_700).await;
    if later != DetectedStatus::WaitingInput {
        failures.push(format!(
            "stage 4: {later:?} once the time not spent waiting reached the stale time, expected WaitingInput"
        ));
    }
}

/// Stage 5: the screen is lost while it waits on the user. The end of the wait
/// is the same fact whichever way raw changes, so the turn is not closed by the
/// time spent waiting, and it still closes later.
async fn stage_5_a_lost_screen_inside_the_hold(failures: &mut Vec<String>) {
    let mut driver = Driver::synthetic(SCREEN_DEF);
    driver.feed_at(0, &idle()).await;
    driver.report_at(200, WindowStatus::Running).await;
    driver.feed_at(1_000, &approval()).await;
    let lost_at = 1_000 + TURN_QUIET_MS + 700;
    driver.advance_to(lost_at).await;
    driver.engine.lose_screen();
    driver.sample();
    if driver.engine.detected() != DetectedStatus::Unknown(UnknownReason::ScreenLost) {
        failures.push("stage 5 precondition: the screen was not lost at once".to_string());
    }
    let back_at = lost_at + 1_000;
    driver.advance_to(back_at - 1).await;
    driver.feed_at(back_at, &idle()).await;
    // The prompt's repaint restores trust through its confirmation.
    let restored = driver.detected_at(back_at + 50).await;
    if restored != DetectedStatus::WaitingInput {
        failures.push(format!(
            "stage 5 precondition: the repaint did not restore trust, {restored:?} at +50 ms"
        ));
    }
    // One more event after that confirmation judges the turn again.
    let event_at = back_at + 100;
    driver.feed_at(event_at, CHUNK).await;
    driver.advance_to(back_at + TURN_QUIET_MS - 500).await;
    check_window(
        failures,
        &driver,
        "stage 5 (i) while the screen is lost",
        lost_at + 1,
        back_at - 1,
        |d| d == DetectedStatus::Unknown(UnknownReason::ScreenLost),
    );
    check_window(
        failures,
        &driver,
        "stage 5 (ii) after the screen came back",
        event_at + 100,
        back_at + TURN_QUIET_MS - 500,
        |d| d == DetectedStatus::Running,
    );
    let later = driver.detected_at(event_at + TURN_QUIET_MS + 600).await;
    if later != DetectedStatus::WaitingInput {
        failures.push(format!(
            "stage 5 (iii) after the quiet time: {later:?}, expected WaitingInput"
        ));
    }
}

/// Stage 5b: a lost screen is not a wait on the user, so the silence it spans
/// is counted. Lost for longer than the quiet time, the turn is over by the
/// time the screen is back, and later output does not bring `Running` back.
async fn stage_5b_a_screen_lost_for_longer_than_the_quiet_time(failures: &mut Vec<String>) {
    let mut driver = Driver::synthetic(SCREEN_DEF);
    driver.feed_at(0, &idle()).await;
    driver.report_at(200, WindowStatus::Running).await;
    driver.feed_at(1_000, &approval()).await;
    let lost_at = 1_000 + TURN_QUIET_MS + 700;
    driver.advance_to(lost_at).await;
    driver.engine.lose_screen();
    driver.sample();
    if driver.engine.detected() != DetectedStatus::Unknown(UnknownReason::ScreenLost) {
        failures.push("stage 5b precondition: the screen was not lost at once".to_string());
    }
    let back_at = lost_at + TURN_QUIET_MS + 700;
    driver.advance_to(back_at - 1).await;
    driver.feed_at(back_at, &idle()).await;
    let restored = driver.detected_at(back_at + 50).await;
    if restored != DetectedStatus::WaitingInput {
        failures.push(format!(
            "stage 5b precondition: the repaint did not restore trust, {restored:?} at +50 ms"
        ));
    }
    let event_at = back_at + 100;
    driver.feed_at(event_at, CHUNK).await;
    driver.advance_to(back_at + TURN_QUIET_MS - 500).await;
    check_window(
        failures,
        &driver,
        "stage 5b (i) while the screen is lost",
        lost_at + 1,
        back_at - 1,
        |d| d == DetectedStatus::Unknown(UnknownReason::ScreenLost),
    );
    check_window(
        failures,
        &driver,
        "stage 5b (ii) the silence while lost closed the turn",
        event_at + 100,
        back_at + TURN_QUIET_MS - 500,
        |d| d == DetectedStatus::WaitingInput,
    );
}
