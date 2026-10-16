//! State detection state machine and its async driver.
//!
//! The pipeline is `raw judgement -> debounce -> confirmed status`:
//!
//! - the *raw judgement* is what the current evidence says right now. It is
//!   re-derived on every event and is allowed to flap (spinners, partial
//!   repaints);
//! - the *confirmed status* only follows a raw judgement that has held for
//!   `confirm_ms`, and is the sole thing published to the rest of the daemon.
//!
//! [`DetectionEngine`] is a synchronous state machine driven by explicit
//! calls: it owns no task and reads the clock only through
//! `tokio::time::Instant`, so its time-dependent behavior stays reproducible
//! under `tokio::time::pause()`. It does not parse output itself: it reads the
//! window's own terminal model ([`SharedScreen`]), and is told when output
//! has reached it. [`DetectorHandle`] is the thin async wrapper
//! that owns one engine in a driver task, sleeps until the engine's next
//! deadline and republishes confirmed-status changes.
//!
//! Every judgement is fail-safe: anything the evidence does not positively
//! explain resolves to [`WindowStatus::Unknown`] rather than an optimistic
//! `Idle`/`Running` guess.

use std::cmp::Reverse;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;
use tracing::Instrument;
use yagura_protocol::{DetectedStatus, UnknownReason, WindowStatus};

use crate::DaemonConfig;
use crate::detector::definition::{Definition, Region};
use crate::detector::screen::{ComposerReadout, SharedScreen};
use crate::detector::source::TurnReport;
use crate::handoff;

/// The outcome of one raw-judgement evaluation.
enum RawJudgement {
    /// The evidence resolves to this status.
    Status(DetectedStatus),
    /// The evidence produces nothing new, so the previous raw judgement (and
    /// the debounce progress it accumulated) is kept. Only the
    /// `window_ms <= t < after_ms` region of a definition that opted into
    /// *both* time-based judgements does this, which is what bridges activity
    /// to quiescence without passing through `Unknown`.
    Hold,
}

/// What the detector should assume about the output an input write produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputEcho {
    /// The write is interactive line editing: it is expected to produce
    /// nothing but its own echo.
    EchoOnly,
    /// The write may start work (a submitted line, an injected message, an
    /// interrupt): its output stays activity evidence.
    MayStartWork,
}

/// How long output must stop before a turn a `Running` report opened counts
/// as over. Measured output gaps inside a turn whose screen shows no footer of
/// its own peak near 2.5 s (Codex); the longer gaps belong to screens that
/// carry a `Working` footer the rules classify directly. 5 s is about twice
/// that peak, so a quiet stretch inside a turn does not close it while the
/// return to the screen after an interrupt no report announces stays short.
pub const TURN_ACTIVE_QUIET: Duration = Duration::from_secs(5);

/// How long a `Running` turn may stay open with no report at all, however
/// much output keeps arriving. Every CLI with hooks reports during a turn (at
/// least a tool call, or the end), so a turn this old whose closing report
/// never came is one a periodic redraw is holding open; an approval report is
/// not subject to it because it waits for the user.
pub const TURN_REPORT_STALE: Duration = Duration::from_secs(120);

/// How long after a screen rule first confirms `Running` a window whose CLI
/// should report its turns, and has sent no report since it started, is
/// marked. The first report of a turn is sent when the prompt is submitted,
/// so a window still without one this long after its screen first showed a
/// running turn means the hooks are not firing. The wait is measured from
/// that first confirmation, whether or not the screen is still running when
/// it ends; a turn a report opened never starts it.
pub const REPORT_GAP_GRACE: Duration = Duration::from_secs(10);

/// What the engine and its handle share about status reports: whether they
/// are expected, and whether their absence is currently marked. Atomics so the
/// daemon can read both without a round trip to the driver task.
#[derive(Debug, Default)]
struct ReportWatch {
    expected: AtomicBool,
    gap: AtomicBool,
}

/// Attribution state for output the terminal produces on its own, which must
/// not be counted as evidence of work: an input's own echo and a resize's own
/// redraw. Kept for every window, whatever its definition declares, because a
/// turn a report opened measures its recent output the same way.
#[derive(Debug, Clone, Copy)]
struct OutputAttribution {
    /// `DaemonConfig::input_echo_window_ms`.
    echo_window: Duration,
    /// `DaemonConfig::resize_redraw_window_ms`.
    resize_window: Duration,
    /// Instant of the most recent `InputEcho::EchoOnly` write, or `None` when
    /// disarmed. Starts disarmed: nothing has been written yet.
    last_input_at: Option<Instant>,
    /// Instant of the most recent `on_resize_started` call, or `None` when
    /// disarmed. Starts disarmed: no resize has been requested yet.
    last_resize_at: Option<Instant>,
}

impl OutputAttribution {
    /// Whether output arriving at `now` is close enough to a recent
    /// `EchoOnly` write, or a recent resize arm, to be attributed to it
    /// rather than counted as evidence of work. Attribution is by time
    /// proximity, never by comparing byte sequences, and every consumer
    /// shares this one predicate.
    fn attributes_output(&self, now: Instant) -> bool {
        close_in_time(self.last_input_at, self.echo_window, now)
            || close_in_time(self.last_resize_at, self.resize_window, now)
    }
}

/// The activity band's clock. `Some(..)` exactly when the definition opted
/// into both `[activity]` and `[quiescence]`; the band does not exist outside
/// that envelope.
#[derive(Debug, Clone, Copy)]
struct AttributionGate {
    /// Instant the activity band measures from -- the most recent output chunk
    /// that was *not* attributed to an echo or a resize redraw -- or `None`
    /// while the band is closed. Starts at the engine's construction instant
    /// so a window that has produced no output at all keeps the timeline it
    /// had before attribution existed.
    activity_output_at: Option<Instant>,
}

/// Shared time-proximity predicate behind [`OutputAttribution::attributes_output`]:
/// whether `at` is set and less than `window` before `now`.
fn close_in_time(at: Option<Instant>, window: Duration, now: Instant) -> bool {
    at.is_some_and(|at| now.saturating_duration_since(at) < window)
}

/// The composer's content, paired with what the detection definition's rules
/// alone declare about the same screen -- the answer to
/// [`DetectionEngine::query_composer`] / [`DetectorHandle::query_composer`].
///
/// The two halves are read from the same `self` in the same call, so they are
/// guaranteed to describe the same instant: a caller deciding whether it may
/// write must never pair a composer reading with a rule verdict taken a
/// moment later, since a status change in between is exactly the gap that
/// lets a submit key land on an approval prompt the confirmed status has not
/// caught up with yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposerObservation {
    /// The composer's content as of this call.
    pub readout: ComposerReadout,
    /// What the definition's rules alone say about the screen right now,
    /// ignoring the structured source and every time-based judgement. `None`
    /// while no rule matches this screen.
    pub screen_rule: Option<DetectedStatus>,
    /// Whether the program had finished painting this screen: the cursor is
    /// shown and no synchronized update is open. Always `false` for a screen
    /// that is not trusted to reflect what the program drew.
    pub painted: bool,
}

impl ComposerObservation {
    /// The fail-safe answer for a query nobody could actually put to a live
    /// screen (the driver task has ended): the
    /// composer is unreadable, and the rule verdict closes the write gate
    /// rather than leaving it open, since a caller that cannot ask must never
    /// write on the strength of silence.
    pub(crate) fn closed_unobserved() -> Self {
        Self {
            readout: ComposerReadout::NotObserved,
            screen_rule: Some(DetectedStatus::Unknown(UnknownReason::DeclaredUnknown)),
            painted: false,
        }
    }
}

/// What one evaluation reads off the window's model. Taken under one lock, so
/// the cursor, the rule verdict and the composer all describe the same screen.
#[derive(Debug)]
struct ScreenReading {
    /// The model was replaced since the engine last read it, so the screen
    /// it judged before is gone.
    rebuilt: bool,
    generation: u64,
    cursor_hidden: bool,
    /// The program has a synchronized update open.
    sync_open: bool,
    /// What the definition's rules say about this screen.
    rule: Option<WindowStatus>,
    /// The composer as this screen shows it; `None` when not asked for, or
    /// when the definition declares no composer.
    composer: Option<ComposerReadout>,
}

/// Synchronous state machine turning PTY output, child-exit facts and
/// structured-source events into a confirmed [`WindowStatus`].
///
/// Every mutating method returns `Some(status)` exactly when the *confirmed*
/// status changed, and `None` otherwise; that return value is the only event
/// source behind `DaemonMessage::StatusChanged`.
#[derive(Debug)]
pub struct DetectionEngine {
    definition: Option<Definition>,
    screen: SharedScreen,
    /// The model's [`generation`](crate::detector::VtScreen::generation) as
    /// of the latest reading.
    screen_generation: u64,
    /// What the latest hook report says about the current turn.
    turn: TurnReport,
    /// The most recent output that was not an echo or a resize redraw; an open
    /// turn is over once it is [`TURN_ACTIVE_QUIET`] old.
    turn_output_at: Instant,
    /// When the latest report arrived; a `Running` turn is over once it is
    /// [`TURN_REPORT_STALE`] old.
    turn_report_at: Instant,
    /// Whether the rules have read an approval dialog since the latest
    /// report; only a dialog seen can be seen to give way.
    approval_dialog_seen: bool,
    attribution: OutputAttribution,
    confirm: Duration,
    bottom_line_count: usize,
    confirmed: DetectedStatus,
    raw: DetectedStatus,
    /// When `raw` became the current judgement; the debounce runs from here.
    raw_since: Instant,
    /// When the evidence behind `raw` appeared: `raw_since`, except for a
    /// judgement held back until its screen was finished, which dates from
    /// when that screen started to be drawn. Right after a confirmation,
    /// `raw` is the confirmed status, so this is its evidence instant.
    raw_evidence_at: Instant,
    last_output_at: Instant,
    attribution_gate: Option<AttributionGate>,
    exited: bool,
    /// Whether the screen model still reflects what the child drew. Cleared
    /// for as long as a screen loss stands, so screen-derived readings can
    /// tell "the screen is gone" apart from "the definition matched nothing".
    screen_trusted: bool,
    /// When an evaluation or a composer query was first held back because
    /// the screen was still being drawn; `None` while nothing is held back.
    held_since: Option<Instant>,
    /// Composer queries held back until the screen is finished.
    held_queries: Vec<oneshot::Sender<ComposerObservation>>,
    report_watch: Arc<ReportWatch>,
    /// Whether any status report has reached this engine.
    reports_seen: bool,
    /// When a rule of the screen first confirmed `Running`; a turn a report
    /// opened does not count.
    first_screen_running_at: Option<Instant>,
}

/// How long output must pause, with the cursor still hidden, before the
/// screen counts as finished: CLIs hide the cursor while they paint and show
/// it again at the end, so a hidden cursor that stays put this long belongs
/// to a screen the user is actually looking at. Whether the daemon may write
/// never rests on this clock: that is [`ComposerObservation::painted`].
pub const SETTLE_QUIET: Duration = Duration::from_millis(50);

/// The longest a judgement or a composer query is held back for a screen
/// that never finishes, so output that keeps the cursor hidden is still
/// judged. Whether the daemon may write never rests on this clock: that is
/// [`ComposerObservation::painted`].
pub const SETTLE_CEILING: Duration = Duration::from_millis(250);

impl DetectionEngine {
    /// Builds an engine that judges `screen`, the window's own terminal
    /// model, so it reads the very screen the window shows. Whoever feeds the
    /// model reports each arrival with [`DetectionEngine::on_output`].
    ///
    /// The engine's readings wait on the model's lock, so async code drives
    /// it only through [`DetectorHandle`], which runs it on blocking threads.
    ///
    /// `definition == None` means no usable definition was loaded: the engine
    /// then stays `Unknown` for its whole life instead of failing, keeping the
    /// daemon usable without detection.
    ///
    /// Values the definition leaves open (`confirm_ms`, `bottom_line_count`)
    /// are substituted from `config` here, since the definition loader applies
    /// no defaults of its own.
    #[must_use]
    pub fn new(
        definition: Option<Definition>,
        config: &DaemonConfig,
        screen: SharedScreen,
    ) -> Self {
        let confirm_ms = definition
            .as_ref()
            .and_then(|definition| definition.debounce)
            .map_or(config.confirm_ms, |debounce| debounce.confirm_ms);
        let bottom_line_count = definition
            .as_ref()
            .and_then(|definition| definition.bottom_line_count)
            .unwrap_or(config.bottom_line_count);

        let mut definition = definition;
        if let Some(definition) = definition.as_mut() {
            // Highest priority first, so arbitration is a plain scan and ties
            // keep declaration order (stable sort).
            definition.rules.sort_by_key(|rule| Reverse(rule.priority));
        }

        let now = Instant::now();
        // Attribution exists only for definitions that opted into both
        // time-based judgements, so suppressing activity evidence can at most
        // move an evaluation into the hold region, never create a new status.
        let attribution_gate = definition.as_ref().and_then(|definition| {
            (definition.activity.is_some() && definition.quiescence.is_some()).then_some(
                AttributionGate {
                    activity_output_at: Some(now),
                },
            )
        });
        // The initial reason reflects only whether a definition
        // was loaded, never a time-derived judgement -- a definition with an
        // activity-only table would otherwise report a working status before
        // a single byte of output has arrived.
        let initial = match definition.as_ref() {
            Some(_) => DetectedStatus::Unknown(UnknownReason::NotYetJudged),
            None => DetectedStatus::Unknown(UnknownReason::NoDefinition),
        };
        Self {
            definition,
            screen,
            // Every model starts at generation 0, so a rebuild before the
            // first reading is a loss too, and building takes no lock.
            screen_generation: 0,
            turn: TurnReport::Unreported,
            turn_output_at: now,
            turn_report_at: now,
            approval_dialog_seen: false,
            attribution: OutputAttribution {
                echo_window: Duration::from_millis(config.input_echo_window_ms),
                resize_window: Duration::from_millis(config.resize_redraw_window_ms),
                last_input_at: None,
                last_resize_at: None,
            },
            confirm: Duration::from_millis(confirm_ms),
            bottom_line_count: usize::from(bottom_line_count),
            confirmed: initial,
            raw: initial,
            raw_since: now,
            raw_evidence_at: now,
            last_output_at: now,
            attribution_gate,
            exited: false,
            screen_trusted: true,
            held_since: None,
            held_queries: Vec::new(),
            report_watch: Arc::new(ReportWatch::default()),
            reports_seen: false,
            first_screen_running_at: None,
        }
    }

    /// Declares whether this window's CLI is expected to report its turns. A
    /// window that is expected to, and whose screen shows a running turn for
    /// [`REPORT_GAP_GRACE`] with no report, is marked (see
    /// [`DetectionEngine::report_gap`]). Never changes the confirmed status.
    pub fn expect_status_reports(&mut self, expected: bool) {
        self.report_watch.expected.store(expected, Ordering::SeqCst);
    }

    /// Whether reports are expected, were never received, and the screen has
    /// shown a running turn for [`REPORT_GAP_GRACE`]. A report clears the mark
    /// for good.
    #[must_use]
    pub fn report_gap(&self) -> bool {
        self.report_gap_at(Instant::now())
    }

    fn report_gap_at(&self, now: Instant) -> bool {
        self.report_watch.expected.load(Ordering::SeqCst)
            && !self.reports_seen
            && self
                .report_gap_deadline()
                .is_some_and(|deadline| now >= deadline)
    }

    fn report_gap_deadline(&self) -> Option<Instant> {
        self.first_screen_running_at
            .and_then(|at| at.checked_add(REPORT_GAP_GRACE))
    }

    /// Publishes the mark to the handle, warning once when it is raised. The
    /// mark can only rise once: a report ends the condition for good.
    fn publish_report_gap(&mut self) {
        let gap = self.report_gap();
        let was = self.report_watch.gap.swap(gap, Ordering::SeqCst);
        if gap && !was {
            tracing::warn!(
                definition = self
                    .definition
                    .as_ref()
                    .map_or("", |definition| definition.name.as_str()),
                "the screen shows a running turn but no status report has arrived; the CLI's hooks may not be firing"
            );
        }
    }

    /// Records that output has reached the model, and re-evaluates the
    /// screen as the model now shows it. One call may stand for any number
    /// of chunks: the model holds all of them.
    ///
    /// The arrival instant is what the opt-in time-based judgements measure
    /// their elapsed time from -- except that output attributed to an
    /// input's echo or a resize's own redraw leaves the activity band's own
    /// clock untouched, since neither a terminal echoing a keystroke nor a
    /// redraw following a terminal resize is evidence of work. Rule matching
    /// reads the screen either way.
    ///
    /// A model that was rebuilt since the last reading (see
    /// [`generation`](crate::detector::VtScreen::generation)) is evidence of
    /// nothing: the call degrades instead of evaluating (see
    /// [`DetectionEngine::degrade_after_screen_loss`]), leaving both the
    /// quiescence clock and the attribution timeline where they were.
    ///
    /// Rules are matched only against a finished screen (see
    /// [`DetectionEngine::screen_finished`]); output that ends mid-paint is
    /// judged once the paint finishes, which
    /// [`DetectionEngine::next_deadline`] schedules.
    pub fn on_output(&mut self) -> Option<DetectedStatus> {
        let now = Instant::now();
        // Before this arrival refreshes the clock: a turn already silent for
        // the quiet time stays closed however soon output resumes.
        self.close_quiet_turn(now);
        let reading = self.read_screen(self.wants_composer());
        if reading.rebuilt {
            return self.degrade_after_screen_loss(&reading);
        }
        self.last_output_at = now;
        if !self.attribution.attributes_output(now) {
            self.turn_output_at = now;
            if let Some(gate) = self.attribution_gate.as_mut() {
                gate.activity_output_at = Some(now);
            }
        }
        self.evaluate(now, &reading)
    }

    /// Records that input was written to the PTY, before the write itself.
    ///
    /// Only a definition that opted into *both* time-based judgements has an
    /// activity band to close; every window still attributes the echo. Typing
    /// never closes an open turn: nothing a keystroke does says the turn ended.
    /// Never changes the confirmed status, so it returns nothing.
    pub fn on_input(&mut self, echo: InputEcho) {
        match echo {
            // A human editing a line is direct evidence the window is not
            // working, so the band closes at once rather than waiting out a
            // boundary an earlier real output left open.
            InputEcho::EchoOnly => {
                self.attribution.last_input_at = Some(Instant::now());
                if let Some(gate) = self.attribution_gate.as_mut() {
                    gate.activity_output_at = None;
                }
            }
            // Disarms both attribution sources, so a submitted line's own
            // output counts as activity right away even if it follows closely
            // on an armed resize.
            InputEcho::MayStartWork => {
                self.attribution.last_input_at = None;
                self.attribution.last_resize_at = None;
            }
        }
    }

    /// Records that a PTY resize was requested, before the resize itself runs
    /// and before the redrawn output it produces arrives.
    ///
    /// Unlike `InputEcho::EchoOnly`, arming a resize never touches the
    /// activity band itself, so a window already mid-work keeps its `Running`
    /// status across a resize. The output chunk that arrives less than
    /// `resize_redraw_window_ms` after this call is not counted as activity
    /// evidence, but it still reaches the virtual screen so rule matching is
    /// unaffected. Calling this again before the previous window elapses
    /// re-arms it, with no upper bound. Never changes the confirmed status,
    /// so it returns nothing.
    pub fn on_resize_started(&mut self) {
        self.attribution.last_resize_at = Some(Instant::now());
    }

    /// Re-evaluates at the current instant, without any new evidence.
    ///
    /// The driver calls this once a deadline returned by
    /// [`DetectionEngine::next_deadline`] expires.
    pub fn on_tick(&mut self) -> Option<DetectedStatus> {
        self.read_and_evaluate(Instant::now())
    }

    /// Records that the model was resized. The size itself is the model's;
    /// this only checks that the model survived the resize, and degrades at
    /// once when it had to be rebuilt (see
    /// [`DetectionEngine::degrade_after_screen_loss`]). Returns `None` for
    /// every ordinary resize.
    pub fn on_resized(&mut self) -> Option<DetectedStatus> {
        let reading = self.read_screen(self.wants_composer());
        if reading.rebuilt {
            return self.degrade_after_screen_loss(&reading);
        }
        None
    }

    /// The degradation behind a screen the model had to rebuild, noticed by
    /// any reading through a changed
    /// [`generation`](crate::detector::VtScreen::generation).
    ///
    /// Confirms `Unknown(ScreenLost)` at once, without waiting out
    /// `confirm_ms` and regardless of what the raw judgement would otherwise
    /// say, rather than letting a status the evidence no
    /// longer supports keep accumulating hold time. `Exited` outranks it, and
    /// structured-source events survive it. Neither the quiescence clock
    /// (`last_output_at`) nor the attribution timeline moves here: the event
    /// destroyed evidence, it did not produce any.
    fn degrade_after_screen_loss(&mut self, reading: &ScreenReading) -> Option<DetectedStatus> {
        self.screen_trusted = false;
        self.release_held(reading);
        if self.exited {
            return None;
        }
        let unknown = DetectedStatus::Unknown(UnknownReason::ScreenLost);
        self.raw = unknown;
        self.raw_since = Instant::now();
        self.raw_evidence_at = self.raw_since;
        self.confirm_now(unknown, reading.rule)
    }

    /// Handles the child-exit notification: `Exited` is a mechanical fact, so
    /// it is confirmed immediately (a debounce-exempt transition) and outranks
    /// every later judgement for the rest of the engine's life.
    pub fn on_exit(&mut self) -> Option<DetectedStatus> {
        self.exited = true;
        if !self.held_queries.is_empty() {
            let reading = self.read_screen(true);
            self.release_held(&reading);
        }
        self.raw = DetectedStatus::Exited;
        self.raw_since = Instant::now();
        self.raw_evidence_at = self.raw_since;
        self.confirm_now(DetectedStatus::Exited, None)
    }

    /// Records a structured-source report and re-evaluates.
    ///
    /// A report says where a turn began or ended (see [`TurnReport`]) and has
    /// no expiry; it does not override the screen, only fills in what the
    /// screen cannot show. The transition it causes is debounced like any
    /// other.
    pub fn on_status_event(&mut self, status: WindowStatus) -> Option<DetectedStatus> {
        let now = Instant::now();
        self.reports_seen = true;
        self.turn = TurnReport::after_report(status);
        self.turn_report_at = now;
        self.approval_dialog_seen = false;
        // A turn opened after a silence longer than the quiet time would be
        // over before it began, so its clock restarts at the report.
        if self.turn.closes_on_silence()
            && now.saturating_duration_since(self.turn_output_at) >= TURN_ACTIVE_QUIET
        {
            self.turn_output_at = now;
        }
        self.read_and_evaluate(now)
    }

    /// The window's terminal model this engine reads.
    #[must_use]
    pub fn screen(&self) -> &SharedScreen {
        &self.screen
    }

    /// Arms the model's fault-injection seam (see
    /// [`VtScreen::poison_after_calls`](crate::detector::VtScreen::poison_after_calls)).
    #[cfg(any(test, feature = "fault-injection"))]
    pub fn poison_screen_after_calls(&mut self, calls: usize) {
        self.screen.lock().poison_after_calls(calls);
    }

    /// Returns the confirmed status, projected to [`WindowStatus`] (see
    /// [`DetectionEngine::detected`] for the reason behind an `Unknown`).
    #[must_use]
    pub fn status(&self) -> WindowStatus {
        self.confirmed.status()
    }

    /// Returns the confirmed status together with why it is `Unknown`, when
    /// it is.
    #[must_use]
    pub fn detected(&self) -> DetectedStatus {
        self.confirmed
    }

    /// Reads the composer's content off the screen as it stands at this call,
    /// together with what the definition's rules alone say about the exact
    /// same screen (see [`ComposerObservation`]).
    ///
    /// The reading never fails: everything that leaves the composer unreadable
    /// resolves to [`ComposerReadout::NotObserved`] -- a definition without a
    /// `[composer]` table (answered without looking at the screen at all), no
    /// definition, a marker the screen does not show (unless the body's end
    /// still fills the screen, which is [`ComposerReadout::Tail`]), and a
    /// screen loss that still stands. An empty composer is
    /// [`ComposerReadout::Empty`], which is a reading, not a failure.
    ///
    /// The rule verdict ignores the structured source and every time-based
    /// judgement -- only [`DetectionEngine::match_rules`], applied to the
    /// screen this same call reads the composer from -- so it never lags the
    /// screen the way the confirmed status does (that one waits out
    /// `confirm_ms`). `None` when no rule matches; a definition with no rules
    /// at all, or no definition, answers `None` the same way.
    ///
    /// Synchronous and free of I/O; waits for the model's lock for at most
    /// one slice of output being fed, so it belongs on a blocking thread.
    #[must_use]
    pub fn query_composer(&self) -> ComposerObservation {
        self.observe(&self.take_reading(true))
    }

    /// The answer to a composer query from one reading. A model rebuilt
    /// since the engine last looked is as untrusted as a loss it has seen.
    fn observe(&self, reading: &ScreenReading) -> ComposerObservation {
        let trusted = self.screen_trusted && !reading.rebuilt;
        let readout = match reading.composer.as_ref() {
            Some(readout) if trusted => readout.clone(),
            _ => ComposerReadout::NotObserved,
        };
        ComposerObservation {
            readout,
            screen_rule: reading.rule.map(declare_status),
            painted: trusted && !reading.cursor_hidden && !reading.sync_open,
        }
    }

    /// Reads the model once (see [`ScreenReading`]), without recording what
    /// was read.
    fn take_reading(&self, with_composer: bool) -> ScreenReading {
        let composer = self
            .definition
            .as_ref()
            .and_then(|definition| definition.composer.as_ref());
        let has_rules = self
            .definition
            .as_ref()
            .is_some_and(|definition| !definition.rules.is_empty());
        let (read, generation) = {
            let mut model = self.screen.lock();
            let read = model.read_guarded(|screen| {
                let bottom = has_rules.then(|| match composer {
                    Some(section) => {
                        screen.bottom_lines_without_composer(self.bottom_line_count, section)
                    }
                    None => screen.bottom_lines(self.bottom_line_count),
                });
                let readout = composer
                    .filter(|_| with_composer)
                    .map(|section| screen.read_composer(section));
                (
                    (screen.cursor_hidden(), screen.sync_open()),
                    bottom,
                    readout,
                )
            });
            (read, model.generation())
        };
        let ((cursor_hidden, sync_open), bottom, composer) = read.unwrap_or_default();
        ScreenReading {
            // A read that panicked rebuilt the model, which moved its generation.
            rebuilt: generation != self.screen_generation,
            generation,
            cursor_hidden,
            sync_open,
            // Matched after the lock is released, so the reader thread does
            // not wait on the regular expressions.
            rule: bottom.and_then(|bottom| self.match_rules(&bottom)),
            composer,
        }
    }

    /// Reads the model once and remembers its generation, so the next
    /// reading only reports a rebuild that happened after this one.
    fn read_screen(&mut self, with_composer: bool) -> ScreenReading {
        let reading = self.take_reading(with_composer);
        self.screen_generation = reading.generation;
        reading
    }

    /// Composer queries are held, so a reading must cover the composer too.
    fn wants_composer(&self) -> bool {
        !self.held_queries.is_empty()
    }

    /// Reads the model once and evaluates that screen, or degrades when the
    /// model was rebuilt since the last reading.
    fn read_and_evaluate(&mut self, now: Instant) -> Option<DetectedStatus> {
        let reading = self.read_screen(self.wants_composer());
        if reading.rebuilt {
            return self.degrade_after_screen_loss(&reading);
        }
        self.evaluate(now, &reading)
    }

    /// Returns the earliest instant at which [`DetectionEngine::on_tick`]
    /// could change something -- a pending debounce, a `window_ms` / `after_ms`
    /// boundary or the end of an open turn's quiet time -- or `None` when nothing is
    /// scheduled.
    #[must_use]
    pub fn next_deadline(&self) -> Option<Instant> {
        if self.exited {
            return None;
        }

        let now = Instant::now();
        let mut earliest: Option<Instant> = None;

        if self.raw != self.confirmed {
            earliest = min_deadline(earliest, self.raw_since.checked_add(self.confirm));
        }
        if let Some(held_since) = self.held_since {
            earliest = min_deadline(
                earliest,
                min_deadline(
                    self.last_output_at.checked_add(SETTLE_QUIET),
                    held_since.checked_add(SETTLE_CEILING),
                ),
            );
        }
        if let Some(definition) = self.definition.as_ref() {
            if let Some(activity) = definition.activity {
                // A closed activity band contributes no wakeup, but the other terms
                // must survive that: `?` here would return from `next_deadline` and
                // leave the driver sleeping forever on `wait_until(None)`.
                earliest = min_deadline(
                    earliest,
                    self.activity_output_at()
                        .and_then(|at| boundary(at, activity.window_ms))
                        .filter(|deadline| *deadline > now),
                );
            }
            if let Some(quiescence) = definition.quiescence {
                earliest = min_deadline(
                    earliest,
                    boundary(self.last_output_at, quiescence.after_ms)
                        .filter(|deadline| *deadline > now),
                );
            }
        }
        if self.turn.closes_on_silence() {
            earliest = min_deadline(
                earliest,
                self.turn_output_at
                    .checked_add(TURN_ACTIVE_QUIET)
                    .filter(|deadline| *deadline > now),
            );
        }
        if self.turn == TurnReport::Running {
            earliest = min_deadline(
                earliest,
                self.turn_report_at
                    .checked_add(TURN_REPORT_STALE)
                    .filter(|deadline| *deadline > now),
            );
        }
        if self.report_watch.expected.load(Ordering::SeqCst) && !self.reports_seen {
            earliest = min_deadline(
                earliest,
                self.report_gap_deadline()
                    .filter(|deadline| *deadline > now),
            );
        }

        earliest
    }

    /// The instant the activity band measures from, or `None` when the band is
    /// closed. Falls back to `last_output_at` when no gate exists.
    ///
    /// `None` means the activity table contributes nothing at this instant --
    /// neither a judgement nor a deadline. Callers must treat it as one absent
    /// term among several, not as an absent result.
    fn activity_output_at(&self) -> Option<Instant> {
        match self.attribution_gate {
            Some(gate) => gate.activity_output_at,
            None => Some(self.last_output_at),
        }
    }

    /// A judgement read off the screen is made only on a finished screen and
    /// dated from when that screen started to be drawn, so a paint begun
    /// before a write never counts as a reaction to it. An open turn with
    /// recent output reads no screen and is judged at once. A judgement made
    /// earlier still matures through the debounce while the next paint is
    /// under way.
    fn evaluate(&mut self, now: Instant, reading: &ScreenReading) -> Option<DetectedStatus> {
        self.close_quiet_turn(now);
        let judged = if self.screen_finished(now, reading.cursor_hidden) {
            let evidence_at = self.held_since.unwrap_or(now);
            self.release_held(reading);
            self.settle_answered_approval(now, reading.rule);
            Some((self.judge(now, reading.rule), evidence_at))
        } else {
            self.held_since.get_or_insert(now);
            self.running_turn_status()
                .map(|status| (RawJudgement::Status(status), now))
        };
        if let Some((RawJudgement::Status(raw), evidence_at)) = judged
            && raw != self.raw
        {
            self.raw = raw;
            self.raw_since = now;
            self.raw_evidence_at = evidence_at;
        }

        if self.raw != self.confirmed
            && now.saturating_duration_since(self.raw_since) >= self.confirm
        {
            return self.confirm_now(self.raw, reading.rule);
        }
        None
    }

    /// Ends a turn a `Running` report opened once no output has come for
    /// [`TURN_ACTIVE_QUIET`], or no report for [`TURN_REPORT_STALE`]. It stays
    /// closed -- later output does not reopen it -- until the next opening
    /// report, so a redraw long after an interrupt no report announced cannot
    /// bring `Running` back.
    fn close_quiet_turn(&mut self, now: Instant) {
        let quiet = self.turn.closes_on_silence()
            && now.saturating_duration_since(self.turn_output_at) >= TURN_ACTIVE_QUIET;
        let stale = self.turn == TurnReport::Running
            && now.saturating_duration_since(self.turn_report_at) >= TURN_REPORT_STALE;
        if quiet || stale {
            self.turn = TurnReport::Closed;
        }
    }

    /// Ends an approval the CLI need not report the answer to (Claude Code's
    /// next report is the tool's end): once the dialog has been read, a
    /// running screen in its place means it was granted and the tool's run
    /// is a running turn; a screen at rest means it was refused.
    fn settle_answered_approval(&mut self, now: Instant, rule: Option<WindowStatus>) {
        if self.turn != TurnReport::AwaitingApproval || !self.screen_trusted {
            return;
        }
        match rule {
            Some(WindowStatus::WaitingApproval) => self.approval_dialog_seen = true,
            Some(WindowStatus::Running) if self.approval_dialog_seen => {
                self.turn = TurnReport::Running;
                self.turn_output_at = now;
                self.turn_report_at = now;
            }
            Some(WindowStatus::WaitingInput | WindowStatus::Idle | WindowStatus::Interrupted)
                if self.approval_dialog_seen =>
            {
                self.turn = TurnReport::Closed;
            }
            _ => {}
        }
    }

    /// `Running` while a report-opened turn is in flight, without reading the
    /// screen. A lost screen is never rescued by a report.
    fn running_turn_status(&self) -> Option<DetectedStatus> {
        (self.screen_trusted && self.turn == TurnReport::Running).then_some(DetectedStatus::Running)
    }

    /// What an open turn adds to a screen the rules do not positively
    /// classify (`rule` is `None`, or `WaitingInput`). An approval report
    /// stands only over a screen that shows nothing the rules know; a screen
    /// that plainly waits for input is the state.
    fn open_turn_status(&self, rule: Option<WindowStatus>) -> Option<DetectedStatus> {
        if !self.screen_trusted {
            return None;
        }
        match self.turn {
            TurnReport::Running => Some(DetectedStatus::Running),
            TurnReport::AwaitingApproval if rule.is_none() => Some(DetectedStatus::WaitingApproval),
            _ => None,
        }
    }

    /// Whether the screen may be read now: the cursor is shown (the CLI
    /// finished its paint), or it stays hidden but output has paused for
    /// [`SETTLE_QUIET`], or reading has already been held back for
    /// [`SETTLE_CEILING`]. Always true once nothing screen-derived is judged
    /// any more (the child exited, or no definition was loaded).
    fn screen_finished(&self, now: Instant, cursor_hidden: bool) -> bool {
        self.exited
            || self.definition.is_none()
            || !cursor_hidden
            || now.saturating_duration_since(self.last_output_at) >= SETTLE_QUIET
            || self
                .held_since
                .is_some_and(|since| now.saturating_duration_since(since) >= SETTLE_CEILING)
    }

    /// Ends a hold: the held evaluation is dropped (the caller evaluates now,
    /// or the event ending the hold made it moot), and held composer queries
    /// are answered from `reading`, the screen as it stands.
    fn release_held(&mut self, reading: &ScreenReading) {
        self.held_since = None;
        for respond in std::mem::take(&mut self.held_queries) {
            let _ = respond.send(self.observe(reading));
        }
    }

    /// Answers a composer query once the screen is finished, holding it back
    /// until then (see [`DetectionEngine::screen_finished`]). A judgement
    /// still held back is made first, so it is dated before anything the
    /// caller writes on the strength of the answer.
    fn answer_query(
        &mut self,
        respond: oneshot::Sender<ComposerObservation>,
    ) -> Option<DetectedStatus> {
        let now = Instant::now();
        let reading = self.read_screen(true);
        if reading.rebuilt {
            let changed = self.degrade_after_screen_loss(&reading);
            let _ = respond.send(self.observe(&reading));
            return changed;
        }
        if !self.screen_finished(now, reading.cursor_hidden) {
            self.held_since.get_or_insert(now);
            self.held_queries.push(respond);
            return None;
        }
        let changed = if self.held_since.is_some() {
            self.evaluate(now, &reading)
        } else {
            None
        };
        let _ = respond.send(self.observe(&reading));
        changed
    }

    /// The only place the confirmed status is assigned, so the screen-trust
    /// state has a single point to be restored from: confirming a status the
    /// window accepts input in ([`handoff::is_ready`]) means the child
    /// repainted its own prompt, which is what a trustworthy screen looks
    /// like after a loss. `screen_rule` is what the rules say about the
    /// screen the status is confirmed on.
    fn confirm_now(
        &mut self,
        status: DetectedStatus,
        screen_rule: Option<WindowStatus>,
    ) -> Option<DetectedStatus> {
        if self.confirmed == status {
            return None;
        }
        self.confirmed = status;
        if status == DetectedStatus::Running
            && self.first_screen_running_at.is_none()
            && screen_rule == Some(WindowStatus::Running)
        {
            self.first_screen_running_at = Some(Instant::now());
        }
        if handoff::is_ready(status.status()) {
            self.screen_trusted = true;
        }
        Some(status)
    }

    fn judge(&self, now: Instant, rule: Option<WindowStatus>) -> RawJudgement {
        if self.exited {
            return RawJudgement::Status(DetectedStatus::Exited);
        }
        let Some(definition) = self.definition.as_ref() else {
            return RawJudgement::Status(DetectedStatus::Unknown(UnknownReason::NoDefinition));
        };
        if let Some(status) = rule
            && status != WindowStatus::WaitingInput
        {
            return RawJudgement::Status(declare_status(status));
        }
        if let Some(status) = self.open_turn_status(rule) {
            return RawJudgement::Status(status);
        }
        if let Some(status) = rule {
            return RawJudgement::Status(declare_status(status));
        }
        self.judge_by_time(definition, now)
    }

    /// The highest-priority rule matching `bottom`, the bottom lines of the
    /// screen with the composer's body taken out. Runs after the model's lock
    /// is released.
    fn match_rules(&self, bottom: &[String]) -> Option<WindowStatus> {
        let definition = self.definition.as_ref()?;
        let matched = definition
            .rules
            .iter()
            .find(|rule| match rule.region {
                Region::Bottom => bottom.iter().any(|line| rule.pattern.is_match(line)),
            })
            .map(|rule| rule.status);
        // Diagnostic aid for definition authors: at debug level the daemon
        // logs what the engine matched against, so a pattern mismatch can be
        // diagnosed from the log alone.
        tracing::debug!(matched = ?matched, lines = ?bottom, "detector match window");
        matched
    }

    /// Time-based judgements are opt-in per definition, and their semantics
    /// differ by configuration: only the two-sided configuration has a hold
    /// region, because holding in a one-sided one would keep a vanished
    /// `WaitingApproval` alive and defeat the fail-safe default.
    fn judge_by_time(&self, definition: &Definition, now: Instant) -> RawJudgement {
        let elapsed = now.saturating_duration_since(self.last_output_at);
        match (definition.activity, definition.quiescence) {
            // Only this branch reads the attribution-aware clock; the
            // one-sided branches below stay on `last_output_at`, which keeps
            // attribution inside its envelope however the gate was built.
            (Some(activity), Some(quiescence)) => {
                // `in_activity_band` alone answers "did output arrive recently",
                // not "is that arrival still trustworthy": a screen loss leaves
                // `activity_output_at` untouched (see `degrade_after_screen_loss`), so
                // without the trust check a stale open band would report activity
                // purely from elapsed time right after a forced `ScreenLost`. An
                // untrusted screen falls through as if nothing new arrived (Hold,
                // or the quiescence status once `after_ms` matures) -- quiescence
                // itself stays trust-independent.
                let in_activity_band = self.screen_trusted
                    && self.activity_output_at().is_some_and(|at| {
                        now.saturating_duration_since(at)
                            < Duration::from_millis(activity.window_ms)
                    });
                if in_activity_band {
                    RawJudgement::Status(declare_status(activity.status))
                } else if elapsed < Duration::from_millis(quiescence.after_ms) {
                    RawJudgement::Hold
                } else {
                    RawJudgement::Status(declare_status(quiescence.status))
                }
            }
            (Some(activity), None) => {
                if self.screen_trusted && elapsed < Duration::from_millis(activity.window_ms) {
                    RawJudgement::Status(declare_status(activity.status))
                } else {
                    RawJudgement::Status(self.unknown_from_screen_trust())
                }
            }
            (None, Some(quiescence)) => {
                if elapsed < Duration::from_millis(quiescence.after_ms) {
                    RawJudgement::Status(self.unknown_from_screen_trust())
                } else {
                    RawJudgement::Status(declare_status(quiescence.status))
                }
            }
            (None, None) => RawJudgement::Status(self.unknown_from_screen_trust()),
        }
    }

    /// The `Unknown` reason [`DetectionEngine::judge_by_time`]'s three
    /// data-less branches report (no time table at all, an activity-only
    /// definition past its window, or a quiescence-only definition still
    /// waiting): [`UnknownReason::ScreenLost`] while the screen is not
    /// trusted, [`UnknownReason::NoRuleMatched`] otherwise.
    fn unknown_from_screen_trust(&self) -> DetectedStatus {
        DetectedStatus::Unknown(if self.screen_trusted {
            UnknownReason::NoRuleMatched
        } else {
            UnknownReason::ScreenLost
        })
    }

    fn apply(&mut self, command: DetectorCommand) -> Option<DetectedStatus> {
        match command {
            DetectorCommand::Output => self.on_output(),
            DetectorCommand::Input(echo) => {
                self.on_input(echo);
                None
            }
            DetectorCommand::Exited => self.on_exit(),
            DetectorCommand::StatusEvent(status) => self.on_status_event(status),
            DetectorCommand::ExpectStatusReports(expected) => {
                self.expect_status_reports(expected);
                None
            }
            DetectorCommand::Resized => self.on_resized(),
            DetectorCommand::ResizeStarted => {
                self.on_resize_started();
                None
            }
            // A caller that stopped waiting leaves a closed channel, which is
            // not an error here.
            DetectorCommand::QueryComposer(respond) => self.answer_query(respond),
            #[cfg(any(test, feature = "fault-injection"))]
            DetectorCommand::InjectPanic => fire_driver_panic_seam(),
        }
    }
}

/// The driver seam's failure, kept in its own item so that no `panic!` exists
/// in a build without the seam.
#[cfg(any(test, feature = "fault-injection"))]
#[allow(clippy::panic)] // the seam exists to make the driver's step panic
fn fire_driver_panic_seam() -> Option<DetectedStatus> {
    panic!("driver seam: simulated detection step panic");
}

/// Turns a bare [`WindowStatus`] read off a definition (a rule, `[activity]`,
/// `[quiescence]`) into a [`DetectedStatus`]: a
/// definition author may declare `unknown`
/// itself, and that declaration is [`UnknownReason::DeclaredUnknown`], not an
/// unreasoned value this crate has no way to build.
fn declare_status(status: WindowStatus) -> DetectedStatus {
    DetectedStatus::known(status).unwrap_or(DetectedStatus::Unknown(UnknownReason::DeclaredUnknown))
}

/// Shared boundary arithmetic for `next_deadline`.
fn boundary(from: Instant, offset_ms: u64) -> Option<Instant> {
    from.checked_add(Duration::from_millis(offset_ms))
}

fn min_deadline(current: Option<Instant>, candidate: Option<Instant>) -> Option<Instant> {
    match (current, candidate) {
        (Some(current), Some(candidate)) => Some(current.min(candidate)),
        (current, None) => current,
        (None, candidate) => candidate,
    }
}

/// A message for the driver task owning a [`DetectionEngine`].
#[derive(Debug)]
enum DetectorCommand {
    /// Output reached the model; any number of arrivals collapse into one
    /// pending command (see [`DetectorHandle::notify_output`]).
    Output,
    Input(InputEcho),
    Exited,
    StatusEvent(WindowStatus),
    ExpectStatusReports(bool),
    Resized,
    ResizeStarted,
    QueryComposer(oneshot::Sender<ComposerObservation>),
    /// Makes the step that takes it panic outside the screen's own guard,
    /// which ends the driver task; see [`DetectorHandle::inject_driver_panic`].
    #[cfg(any(test, feature = "fault-injection"))]
    InjectPanic,
}

/// One confirmed-status change as the driver task publishes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConfirmedStatus {
    /// The status just confirmed.
    pub status: DetectedStatus,
    /// When the engine first judged `status` from the evidence it was
    /// confirmed on, before any debounce held it back: the instant a
    /// reaction is attributed to. On the engine's Tokio clock, the same clock
    /// [`crate::waiter::StatusAt`] carries.
    pub evidence_since: Instant,
}

/// Async front end to a [`DetectionEngine`] running in its own task.
///
/// The notifiers are fire-and-forget and infallible: they are called from the
/// PTY read path, which must never fail or block because the detector task has
/// gone away. [`DetectorHandle::query_composer`] is the one round trip; it
/// answers once the driver task takes it up, or at once when that task ends.
#[derive(Debug)]
pub struct DetectorHandle {
    commands: mpsc::UnboundedSender<DetectorCommand>,
    /// An output arrival is queued and not yet taken up by the driver task.
    output_pending: Arc<AtomicBool>,
    report_watch: Arc<ReportWatch>,
}

impl DetectorHandle {
    /// Starts the driver task for `engine` and returns the handle together
    /// with the stream of confirmed-status changes, each carrying when its
    /// evidence first appeared.
    ///
    /// Each event is applied on a blocking thread, one at a time, so waiting
    /// for the model's lock never holds up the async runtime.
    ///
    /// The task waits on the engine's next deadline, so a paused-clock test
    /// observes no spurious wakeups. It stops when the handle is dropped or
    /// the receiver is closed. The task keeps the span current at the call, so
    /// its log lines carry whatever identifies the window.
    pub fn spawn(engine: DetectionEngine) -> (Self, mpsc::UnboundedReceiver<ConfirmedStatus>) {
        let (command_tx, mut command_rx) = mpsc::unbounded_channel();
        let (status_tx, status_rx) = mpsc::unbounded_channel();
        let report_watch = Arc::clone(&engine.report_watch);
        let output_pending = Arc::new(AtomicBool::new(false));

        let mut engine = engine;
        let pending = Arc::clone(&output_pending);
        tokio::spawn(
            async move {
                loop {
                    let deadline = engine.next_deadline();
                    let step = tokio::select! {
                        command = command_rx.recv() => match command {
                            Some(command) => {
                                // Cleared before the model is read, so an
                                // arrival after this point queues a new command.
                                if matches!(command, DetectorCommand::Output) {
                                    pending.store(false, Ordering::SeqCst);
                                }
                                Some(command)
                            }
                            None => break,
                        },
                        () = wait_until(deadline) => None,
                    };
                    let Some((stepped, changed)) = step_off_runtime(engine, step).await else {
                        break;
                    };
                    engine = stepped;
                    engine.publish_report_gap();
                    if let Some(status) = changed
                        && status_tx
                            .send(ConfirmedStatus {
                                status,
                                evidence_since: engine.raw_evidence_at,
                            })
                            .is_err()
                    {
                        break;
                    }
                }
            }
            .instrument(tracing::Span::current()),
        );

        (
            Self {
                commands: command_tx,
                output_pending,
                report_watch,
            },
            status_rx,
        )
    }

    /// Reports that output has reached the window's model (see
    /// [`DetectionEngine::on_output`]). Arrivals the driver task has not yet
    /// taken up collapse into one, so a flood queues no backlog.
    pub fn notify_output(&self) {
        if !self.output_pending.swap(true, Ordering::SeqCst) {
            self.send(DetectorCommand::Output);
        }
    }

    /// Reports an input write to the engine (see [`DetectionEngine::on_input`]).
    pub fn notify_input(&self, echo: InputEcho) {
        self.send(DetectorCommand::Input(echo));
    }

    /// Reports that a PTY resize was requested (see
    /// [`DetectionEngine::on_resize_started`]). Must be sent before the PTY
    /// resize itself so its effect is queued ahead of the redraw output it
    /// explains.
    pub fn notify_resize_started(&self) {
        self.send(DetectorCommand::ResizeStarted);
    }

    /// Reports that the child process exited (see
    /// [`DetectionEngine::on_exit`]).
    pub fn notify_exited(&self) {
        self.send(DetectorCommand::Exited);
    }

    /// Reports a structured-source event (see
    /// [`DetectionEngine::on_status_event`]).
    pub fn record_status_event(&self, status: WindowStatus) {
        self.send(DetectorCommand::StatusEvent(status));
    }

    /// Declares whether the window's CLI is expected to report its turns (see
    /// [`DetectionEngine::expect_status_reports`]). Readable at once through
    /// [`DetectorHandle::reports_expected`]; the driver task is woken so its
    /// deadlines follow.
    pub fn expect_status_reports(&self, expected: bool) {
        self.report_watch.expected.store(expected, Ordering::SeqCst);
        self.send(DetectorCommand::ExpectStatusReports(expected));
    }

    /// Whether the window's CLI is expected to report its turns.
    #[must_use]
    pub fn reports_expected(&self) -> bool {
        self.report_watch.expected.load(Ordering::SeqCst)
    }

    /// Whether the absence of expected status reports is marked right now (see
    /// [`DetectionEngine::report_gap`]). Raised by the driver task at its own
    /// deadline, so it needs no event to rise.
    #[must_use]
    pub fn report_gap(&self) -> bool {
        self.report_watch.gap.load(Ordering::SeqCst)
    }

    /// Reports that the window's model was resized (see
    /// [`DetectionEngine::on_resized`]).
    pub fn notify_resized(&self) {
        self.send(DetectorCommand::Resized);
    }

    /// Makes the driver task's next step panic, which ends the task the way
    /// an engine fault outside the screen's guard would. Compiled for tests
    /// and behind the `fault-injection` feature only.
    #[cfg(any(test, feature = "fault-injection"))]
    pub fn inject_driver_panic(&self) {
        self.send(DetectorCommand::InjectPanic);
    }

    /// Asks the engine for the composer's content, and the rule verdict on
    /// the same screen, as of the first finished screen once the driver task
    /// picks the request up (see [`DetectionEngine::query_composer`]; a
    /// screen mid-paint is waited out for at most [`SETTLE_CEILING`]).
    ///
    /// Never fails, and has no time limit of its own: it waits for as long as
    /// the driver task takes to pick the request up (a held model holds it
    /// up), so a caller that must not wait on it races it against what ends
    /// that caller's wait. An engine whose task has ended resolves to
    /// [`ComposerObservation::closed_unobserved`].
    pub async fn query_composer(&self) -> ComposerObservation {
        let (respond, reply) = oneshot::channel();
        if self
            .commands
            .send(DetectorCommand::QueryComposer(respond))
            .is_err()
        {
            return ComposerObservation::closed_unobserved();
        }
        reply
            .await
            .unwrap_or_else(|_| ComposerObservation::closed_unobserved())
    }

    fn send(&self, command: DetectorCommand) {
        let _ = self.commands.send(command);
    }
}

/// Applies `command` to `engine` (a tick when `None`) on a blocking thread,
/// since reading the model waits on its lock while the window's output is
/// being fed. Hands the engine back with what the step confirmed; `None` when
/// the step could not run to completion (the runtime is shutting down).
async fn step_off_runtime(
    mut engine: DetectionEngine,
    command: Option<DetectorCommand>,
) -> Option<(DetectionEngine, Option<DetectedStatus>)> {
    let span = tracing::Span::current();
    tokio::task::spawn_blocking(move || {
        let changed = span.in_scope(|| match command {
            Some(command) => engine.apply(command),
            None => engine.on_tick(),
        });
        (engine, changed)
    })
    .await
    .ok()
}

async fn wait_until(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

#[cfg(test)]
mod tests {
    //! A resize that rebuilds the model degrades the confirmed status
    //! immediately. The fixtures are local because `tests/common/mod.rs` is
    //! not visible from `src`.
    //!
    //! The seam panics through the default panic hook, so this module prints
    //! one panic message to stderr per armed test; that noise is expected.

    use super::*;
    use crate::detector::VtScreen;
    use crate::detector::definition::parse_definition;

    /// Rules-only definition: with neither `[activity]` nor `[quiescence]`,
    /// the confirmed status is decided by rule matching alone, so reaching a
    /// non-`Unknown` confirmed state takes exactly two steps (one matching
    /// chunk, then `confirm_ms` of paused time) with no time band to race.
    const RULES_ONLY_DEF: &str = r#"
schema_version = 1
name = "test-rules-only"

[debounce]
confirm_ms = 300

[[rules]]
status = "idle"
pattern = '[$#>]\s*$'
priority = 1
region = "bottom"
"#;

    /// `RULES_ONLY_DEF`'s `[debounce] confirm_ms`.
    const CONFIRM_MS: u64 = 300;

    /// The rule-matching prompt that drives the engine to a confirmed `Idle`.
    const PROMPT: &[u8] = b"PS C:\\work>";

    #[tokio::test]
    async fn a_resize_that_rebuilds_the_screen_confirms_unknown_immediately() {
        tokio::time::pause();
        let config = DaemonConfig::default();
        let definition = parse_definition(RULES_ONLY_DEF).expect("the fixture definition parses");
        let mut engine = DetectionEngine::new(
            Some(definition),
            &config,
            VtScreen::new(80, 24).into_shared(),
        );

        engine.screen().lock().feed(PROMPT);
        engine.on_output();
        tokio::time::advance(Duration::from_millis(CONFIRM_MS)).await;
        assert_eq!(
            engine.on_tick(),
            Some(DetectedStatus::Idle),
            "the starting state must be a confirmed non-Unknown status"
        );

        engine.poison_screen_after_calls(0);
        engine.screen().lock().resize(60, 20);
        assert_eq!(
            engine.on_resized(),
            Some(DetectedStatus::Unknown(UnknownReason::ScreenLost)),
            "a rebuilt screen must drop the confirmed status to Unknown at once, and resize must \
             report that change to its caller"
        );
        assert_eq!(engine.status(), WindowStatus::Unknown);
    }
}
