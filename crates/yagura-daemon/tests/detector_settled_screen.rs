#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The detector may judge only a screen the CLI has finished drawing.
//!
//! A CLI wraps one repaint in `ESC[?25l` (hide the cursor) ... `ESC[?25h`
//! (show it), but a read from the PTY can end anywhere inside that span. A
//! rule applied to the half-drawn screen can read a status the finished screen
//! does not show, and the composer read off it is not what the user sees.
//!
//! A screen counts as finished once a chunk has been fed and the cursor is
//! visible, or once the cursor has stayed hidden while the output was quiet
//! for `SETTLE_QUIET` (50 ms). Output that never stops is judged at latest
//! `SETTLE_CEILING` (250 ms) after the first postponed chunk.
//!
//! The recorded stream lives under `tests/fixtures/codex-cli/settle/`; see the
//! README there for its origin.

mod common;

use common::{ScreenFeed, feed_handle};
use std::path::Path;
use std::time::Duration;

use tokio::sync::mpsc::UnboundedReceiver;
use tokio::time::Instant;
use yagura_daemon::DaemonConfig;
use yagura_daemon::detector::SharedScreen;
use yagura_daemon::detector::engine::{ConfirmedStatus, DetectionEngine, DetectorHandle};
use yagura_daemon::detector::screen::ComposerReadout;
use yagura_protocol::{DetectedStatus, UnknownReason, WindowStatus};

/// Small debounce so a judgement shows up as a confirmed status within a few
/// milliseconds of the screen it was read from.
const QUICK_DEF: &str = r#"
schema_version = 1
name = "test-settled-screen"

[debounce]
confirm_ms = 5

[[rules]]
status = "running"
pattern = 'tab to queue message'
priority = 20
region = "bottom"

[[rules]]
status = "waiting_input"
pattern = '^›'
priority = 10
region = "bottom"

[composer]
marker = '›'
separator = " "
continuation_prefix = "  "
"#;

const COLS: u16 = 60;
const ROWS: u16 = 8;

/// A finished idle screen: empty composer, cursor shown again.
const IDLE_SCREEN: &[u8] = "\x1b[?25l\x1b[2J\x1b[H› \x1b[?25h".as_bytes();

/// The first half of a repaint: the composer's continuation row is drawn, the
/// row with the marker above it is not yet. Read on its own, the row holds the
/// hint words of a turn in flight, though the finished screen has them inside
/// the composer's body.
const HALF_DRAWN: &[u8] = "\x1b[?25l\x1b[2J\x1b[3;1H  tab to queue message".as_bytes();

/// The rest of that repaint: the marker row, then the cursor on the last row
/// of the body and shown again.
const REST_OF_REPAINT: &[u8] = "\x1b[2;1H› draft body\x1b[3;23H\x1b[?25h".as_bytes();

/// Drives an engine the way the detector's driver task does: whenever the
/// engine's next deadline falls due, it is ticked.
struct Driver {
    start: Instant,
    engine: DetectionEngine,
    /// The confirmed status after every step, with the elapsed milliseconds.
    samples: Vec<(u64, WindowStatus)>,
}

impl Driver {
    fn new(engine: DetectionEngine) -> Self {
        Self {
            start: Instant::now(),
            engine,
            samples: Vec::new(),
        }
    }

    fn elapsed_ms(&self) -> u64 {
        u64::try_from((Instant::now() - self.start).as_millis()).unwrap()
    }

    fn sample(&mut self) {
        let sample = (self.elapsed_ms(), self.engine.status());
        self.samples.push(sample);
    }

    /// Lets time pass up to `target_ms`, ticking at every deadline on the way.
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
}

fn quick_driver() -> Driver {
    let config = DaemonConfig::default();
    Driver::new(common::new_engine_sized(QUICK_DEF, &config, COLS, ROWS))
}

/// A driver that has confirmed `WaitingInput` on the finished idle screen.
async fn idle_driver() -> Driver {
    let mut driver = quick_driver();
    driver.feed_at(0, IDLE_SCREEN).await;
    driver.advance_to(100).await;
    assert_eq!(driver.engine.status(), WindowStatus::WaitingInput);
    driver
}

fn ever_confirmed(driver: &Driver, status: WindowStatus) -> bool {
    driver.samples.iter().any(|(_, seen)| *seen == status)
}

// =====================================================================
// A recorded Codex session
// =====================================================================

fn fixture_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/codex-cli/settle")
}

/// Replays the recorded stream at the recorded times against the bundled
/// definition and returns the driver with everything it confirmed.
async fn replay(name: &str, cols: u16, rows: u16) -> Driver {
    let dir = fixture_dir();
    let bytes = std::fs::read(dir.join(format!("{name}.bin"))).unwrap();
    let table = std::fs::read_to_string(dir.join(format!("{name}.tsv"))).unwrap();
    let source = common::repo_definition_text("codex-cli.toml");
    let config = DaemonConfig::default();
    let mut driver = Driver::new(common::new_engine_sized(&source, &config, cols, rows));
    let mut at = 0;
    for line in table.lines().filter(|line| !line.trim().is_empty()) {
        let (ms, len) = line.split_once('\t').unwrap();
        let (ms, len): (u64, usize) = (ms.trim().parse().unwrap(), len.trim().parse().unwrap());
        driver.feed_at(ms, &bytes[at..at + len]).await;
        at += len;
    }
    assert_eq!(at, bytes.len(), "the time table covers the whole stream");
    driver.advance_to(driver.elapsed_ms() + 3000).await;
    driver
}

/// While the turn runs and the user types a body into the composer, Codex
/// repaints the screen many times a second, and each repaint passes through
/// half-drawn screens that read as an approval dialog. The turn is still in
/// flight the whole time, so the confirmed status must stay `Running`.
#[tokio::test(start_paused = true)]
async fn a_turn_stays_running_while_the_screen_is_repainted_around_a_typed_body() {
    let driver = replay("80x24-body-typed-during-turn", 80, 24).await;
    let window: Vec<_> = driver
        .samples
        .iter()
        .filter(|(ms, _)| (11_500..=14_500).contains(ms))
        .collect();
    assert!(!window.is_empty(), "the stream covers the checked window");
    if let Some((ms, status)) = window
        .iter()
        .find(|(_, status)| *status != WindowStatus::Running)
    {
        panic!("{status:?} was confirmed at {ms} ms, in the middle of the turn");
    }
}

/// A half-drawn dialog is never a dialog: no approval is confirmed anywhere
/// in a session that never asked for one.
#[tokio::test(start_paused = true)]
async fn no_approval_is_confirmed_in_a_session_that_never_asked_for_one() {
    let driver = replay("80x24-body-typed-during-turn", 80, 24).await;
    assert!(!ever_confirmed(&driver, WindowStatus::WaitingApproval));
}

// =====================================================================
// A screen that is still being drawn is not judged
// =====================================================================

/// The half-drawn screen reads as a turn in flight; the next chunk, 20 ms
/// later, shows the finished screen, which is waiting for input.
#[tokio::test(start_paused = true)]
async fn a_repaint_finished_within_the_quiet_time_is_judged_only_on_its_finished_screen() {
    let mut driver = idle_driver().await;
    driver.samples.clear();
    driver.feed_at(200, HALF_DRAWN).await;
    driver.feed_at(220, REST_OF_REPAINT).await;
    driver.advance_to(400).await;
    assert!(
        !ever_confirmed(&driver, WindowStatus::Running),
        "samples: {:?}",
        driver.samples
    );
    assert_eq!(driver.engine.status(), WindowStatus::WaitingInput);
}

/// Output that keeps the cursor hidden and never stops is not postponed for
/// ever: it is judged at latest 250 ms after the first postponed chunk.
#[tokio::test(start_paused = true)]
async fn output_that_never_settles_is_judged_at_the_ceiling() {
    let mut driver = idle_driver().await;
    let first = 200;
    driver.feed_at(first, HALF_DRAWN).await;
    for step in 1..=15 {
        driver.feed_at(first + 20 * step, b"\x1b[3;40H.").await;
        if step == 5 {
            assert_eq!(
                driver.engine.status(),
                WindowStatus::WaitingInput,
                "100 ms into a screen that is still being drawn nothing is judged yet"
            );
        }
    }
    assert_eq!(
        driver.engine.status(),
        WindowStatus::Running,
        "300 ms in, the screen has been judged and the judgement confirmed"
    );
}

// =====================================================================
// The composer is read off a finished screen
// =====================================================================

/// A query that arrives while the screen is being drawn waits for the screen
/// to finish, and reads the finished one.
#[tokio::test(start_paused = true)]
async fn a_composer_query_during_a_repaint_reads_the_finished_screen() {
    let config = DaemonConfig::default();
    let engine = common::new_engine_sized(QUICK_DEF, &config, COLS, ROWS);
    let screen = engine.screen().clone();
    let (handle, _statuses) = DetectorHandle::spawn(engine);
    feed_handle(&screen, &handle, IDLE_SCREEN);
    tokio::time::sleep(Duration::from_millis(50)).await;

    feed_handle(
        &screen,
        &handle,
        "\x1b[?25l\x1b[2J\x1b[H› old draft".as_bytes(),
    );
    let (observation, ()) = tokio::join!(handle.query_composer(), async {
        tokio::time::sleep(Duration::from_millis(20)).await;
        feed_handle(
            &screen,
            &handle,
            "\x1b[2J\x1b[H› new draft\x1b[?25h".as_bytes(),
        );
    });
    assert_eq!(
        observation.readout,
        ComposerReadout::Body("new draft".to_string())
    );
}

// =====================================================================
// Lost or ended evidence is not judged later
// =====================================================================

/// A screen loss drops the postponed judgement together with the screen it
/// was to be read from.
#[tokio::test(start_paused = true)]
async fn a_postponed_judgement_is_dropped_when_the_screen_is_lost() {
    let mut driver = idle_driver().await;
    driver.feed_at(200, HALF_DRAWN).await;
    driver.advance_to(202).await;
    driver.engine.lose_screen();
    driver.advance_to(1000).await;
    assert_eq!(
        driver.engine.detected(),
        DetectedStatus::Unknown(UnknownReason::ScreenLost)
    );
    assert!(!ever_confirmed(&driver, WindowStatus::Running));
}

/// The child's exit outranks a judgement that was still waiting for its
/// screen to finish.
#[tokio::test(start_paused = true)]
async fn a_postponed_judgement_is_dropped_when_the_child_exits() {
    let mut driver = idle_driver().await;
    driver.feed_at(200, HALF_DRAWN).await;
    driver.advance_to(202).await;
    driver.engine.on_exit();
    driver.advance_to(1000).await;
    assert_eq!(driver.engine.detected(), DetectedStatus::Exited);
    assert!(!ever_confirmed(&driver, WindowStatus::Running));
}

// =====================================================================
// Held composer queries are always answered
// =====================================================================

/// A spawned detector that has confirmed `WaitingInput` on the idle screen,
/// with the status receiver kept alive so the driver task keeps running.
async fn idle_handle() -> (
    DetectorHandle,
    UnboundedReceiver<ConfirmedStatus>,
    SharedScreen,
) {
    let config = DaemonConfig::default();
    let engine = common::new_engine_sized(QUICK_DEF, &config, COLS, ROWS);
    let screen = engine.screen().clone();
    let (handle, statuses) = DetectorHandle::spawn(engine);
    feed_handle(&screen, &handle, IDLE_SCREEN);
    tokio::time::sleep(Duration::from_millis(50)).await;
    (handle, statuses, screen)
}

/// A screen that keeps the cursor hidden once output stops (a dialog) is
/// read as soon as output has been quiet long enough.
#[tokio::test(start_paused = true)]
async fn a_query_held_on_a_hidden_cursor_is_answered_once_output_is_quiet() {
    let (handle, _statuses, screen) = idle_handle().await;
    feed_handle(&screen, &handle, "[?25l[2J[H› held draft".as_bytes());
    let observation = tokio::time::timeout(Duration::from_millis(100), handle.query_composer())
        .await
        .expect("the held query is answered once output has been quiet");
    assert_eq!(
        observation.readout,
        ComposerReadout::Body("held draft".to_string())
    );
}

/// A held query is answered at once when the screen is lost.
#[tokio::test(start_paused = true)]
async fn a_query_held_when_the_screen_is_lost_is_answered_at_once() {
    let (handle, _statuses, screen) = idle_handle().await;
    feed_handle(&screen, &handle, HALF_DRAWN);
    let (observation, ()) = tokio::join!(
        tokio::time::timeout(Duration::from_millis(20), handle.query_composer()),
        async {
            tokio::time::sleep(Duration::from_millis(5)).await;
            screen.lock().poison_after_calls(0);
            feed_handle(&screen, &handle, b"");
        }
    );
    let observation = observation.expect("the held query is answered when the screen is lost");
    assert_eq!(observation.readout, ComposerReadout::NotObserved);
}

/// A held query is answered at once when the child exits.
#[tokio::test(start_paused = true)]
async fn a_query_held_when_the_child_exits_is_answered_at_once() {
    let (handle, _statuses, screen) = idle_handle().await;
    feed_handle(&screen, &handle, HALF_DRAWN);
    let (observation, ()) = tokio::join!(
        tokio::time::timeout(Duration::from_millis(20), handle.query_composer()),
        async {
            tokio::time::sleep(Duration::from_millis(5)).await;
            handle.notify_exited();
        }
    );
    observation.expect("the held query is answered when the child exits");
}

// =====================================================================
// When the evidence appeared
// =====================================================================

/// A judgement held back for its screen to finish dates from when that
/// screen started to be drawn, not from when it was let through.
#[tokio::test(start_paused = true)]
async fn a_held_judgement_dates_from_when_its_screen_started_to_be_drawn() {
    let config = DaemonConfig::default();
    let engine = common::new_engine_sized(QUICK_DEF, &config, COLS, ROWS);
    let screen = engine.screen().clone();
    let (handle, mut statuses) = DetectorHandle::spawn(engine);
    feed_handle(&screen, &handle, IDLE_SCREEN);
    let idle = statuses.recv().await.expect("the idle screen is confirmed");
    assert_eq!(idle.status, DetectedStatus::WaitingInput);

    tokio::time::sleep(Duration::from_millis(100)).await;
    let started = Instant::now();
    feed_handle(
        &screen,
        &handle,
        "[?25l[2J[H  tab to queue message".as_bytes(),
    );
    tokio::time::sleep(Duration::from_millis(20)).await;
    feed_handle(&screen, &handle, b"[?25h");

    let running = statuses
        .recv()
        .await
        .expect("the finished screen is confirmed");
    assert_eq!(running.status, DetectedStatus::Running);
    assert_eq!(running.evidence_since, started);
}

/// A `Running` report with output just arrived reads no screen, so a paint
/// under way does not hold it back.
#[tokio::test(start_paused = true)]
async fn an_opening_status_event_is_not_held_back_by_a_paint_under_way() {
    let mut driver = idle_driver().await;
    driver.feed_at(200, HALF_DRAWN).await;
    driver.advance_to(202).await;
    driver.engine.on_status_event(WindowStatus::Running);
    driver.advance_to(215).await;
    assert_eq!(driver.engine.status(), WindowStatus::Running);
}
