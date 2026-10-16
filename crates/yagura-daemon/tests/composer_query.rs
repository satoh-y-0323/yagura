#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The request/response path a caller uses to ask the screen-owning
//! `DetectionEngine` for the composer's current content,
//! and the persistent "screen untrustworthy" state that path must respect.
//!
//! Two layers are covered:
//!
//! - `DetectionEngine::query_composer` -- a synchronous method on the engine
//!   itself, which owns the `VtScreen` and can slice it without any I/O, reachable without a
//!   background task.
//! - `DetectorHandle::query_composer` -- the async round trip a caller outside
//!   the engine's task actually uses, which must resolve even when the engine
//!   task can no longer answer.
//!
//! Both reuse `ComposerReadout` (`Body` / `Empty` / `NotObserved`) rather than
//! inventing a second three-way type: "screen lost", "no composer section" and
//! "marker not found" are all the same answer to a caller -- the composer
//! cannot be trusted right now -- so they all collapse to `NotObserved`.
//!
//! Trust is withdrawn on a screen loss and restored once the engine confirms
//! a status the window can actually accept input in -- the existing single
//! source of truth for that, `handoff::is_ready` / `handoff::READY_STATUSES`,
//! rather than a set invented here. `WaitingInput` alone would strand a plain
//! shell forever: a bare shell prompt confirms `Idle`, never `WaitingInput`
//! (see `detection/generic-shell.toml`), so a screen loss on that kind of
//! window would never clear.
//!
//! Placement: the withdrawn-trust contracts live here rather
//! than in `detector_failsafe_api.rs`. That file's own module doc scopes it
//! narrowly to the `ScreenIntegrity` type (the two-layer defence against a
//! screen model panic); the composer trust state is a different mechanism
//! that happens to share its trigger with that defence (a rebuilt model),
//! and every one of its properties reads more
//! naturally next to the request/response API it protects. No existing
//! assertion in `detector_engine.rs` or `detector_failsafe_api.rs` names
//! `query_composer`, so nothing there needed to change for this file to exist.

mod common;

use common::ScreenFeed;
use std::time::Duration;

use common::{
    SCREEN_COLS, SCREEN_ROWS, feed_handle, new_engine, new_engine_sized, repaint, screen_of,
    tick_after_ms,
};
use yagura_daemon::DaemonConfig;
use yagura_daemon::detector::engine::{DetectionEngine, DetectorHandle};
use yagura_daemon::detector::screen::ComposerReadout;
use yagura_daemon::handoff;
use yagura_protocol::{DetectedStatus, UnknownReason, WindowStatus};

/// A rule and a composer section that share one marker, so a single fed line
/// both confirms `waiting_input` and identifies the composer region.
const COMPOSER_DEF: &str = r#"
schema_version = 1
name = "test-composer-query"

[debounce]
confirm_ms = 300

[[rules]]
status = "waiting_input"
pattern = 'QPROMPT>'
priority = 10
region = "bottom"

[composer]
marker = 'QPROMPT>'
separator = " "
continuation_prefix = ""
"#;

/// Same shape as [`COMPOSER_DEF`], but the rule status is `idle`: a plain
/// shell's prompt confirms `Idle` rather than `WaitingInput` (see
/// `detection/generic-shell.toml`), and `Idle` is also a member of
/// [`handoff::READY_STATUSES`].
const COMPOSER_IDLE_DEF: &str = r#"
schema_version = 1
name = "test-composer-query-idle"

[debounce]
confirm_ms = 300

[[rules]]
status = "idle"
pattern = 'QPROMPT>'
priority = 10
region = "bottom"

[composer]
marker = 'QPROMPT>'
separator = " "
continuation_prefix = ""
"#;

/// Same shape as [`COMPOSER_DEF`], but the rule status is `running`, which is
/// outside [`handoff::READY_STATUSES`].
const COMPOSER_RUNNING_DEF: &str = r#"
schema_version = 1
name = "test-composer-query-running"

[debounce]
confirm_ms = 300

[[rules]]
status = "running"
pattern = 'QPROMPT>'
priority = 10
region = "bottom"

[composer]
marker = 'QPROMPT>'
separator = " "
continuation_prefix = ""
"#;

/// Same rule as [`COMPOSER_DEF`], but the definition never declares
/// `[composer]` at all.
const NO_COMPOSER_DEF: &str = r#"
schema_version = 1
name = "test-no-composer-section"

[debounce]
confirm_ms = 300

[[rules]]
status = "waiting_input"
pattern = 'QPROMPT>'
priority = 10
region = "bottom"
"#;

/// `COMPOSER_DEF`'s `[debounce] confirm_ms`.
const CONFIRM_MS: u64 = 300;

/// Canonical rebuild stimulus (see `detector_failsafe.rs`): fed twice, then a
/// degenerate resize, then fed once more with the fault-injection seam armed,
/// forces the model to panic and `VtScreen` to rebuild blank.
const RESIZE_CHUNK: &[u8] = b"redrawn screen chunk";

/// A well-formed `COMPOSER_DEF` composer line carrying `body`, written as a
/// full repaint so successive calls never leave stray bytes from a shorter
/// previous body on the same row.
fn composer_line(body: &str) -> Vec<u8> {
    repaint(&format!("QPROMPT> {body}"))
}

// --- The query answers with the value as of the call --------------------

#[tokio::test]
async fn query_reflects_the_screen_as_of_the_call_not_a_previously_pushed_value() {
    let config = DaemonConfig::default();
    let mut engine = new_engine(COMPOSER_DEF, &config);

    engine.feed(&composer_line("first"));
    assert_eq!(
        engine.query_composer().readout,
        ComposerReadout::Body("first".to_string())
    );

    engine.feed(&composer_line("second"));
    assert_eq!(
        engine.query_composer().readout,
        ComposerReadout::Body("second".to_string()),
        "the query must answer with the screen as it is now, not a value captured earlier"
    );
}

// --- A busy model delays the answer; no clock turns it into "cannot tell" ---

/// A model whose lock is held elsewhere (a flood of output being fed, a read of
/// another request) answers late, and a late answer is still the answer: the
/// caller is released by the engine ending or by the caller itself, never by a
/// bound the query carries. The clock is paused, so every duration a bound
/// could have been is spent before the model is let go.
#[tokio::test(start_paused = true)]
async fn a_query_on_a_busy_model_waits_for_the_model_instead_of_giving_up_on_a_clock() {
    let config = DaemonConfig::default();
    let mut engine = new_engine(COMPOSER_DEF, &config);
    engine.feed(&composer_line("draft"));
    let screen = engine.screen().clone();
    let (handle, _statuses) = DetectorHandle::spawn(engine);
    let handle = std::sync::Arc::new(handle);

    let (held_tx, held_rx) = std::sync::mpsc::channel::<()>();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let holder = std::thread::spawn(move || {
        let _model = screen.lock();
        let _ = held_tx.send(());
        let _ = release_rx.recv();
    });
    held_rx.recv().expect("the model must be held");

    let query = tokio::spawn({
        let handle = std::sync::Arc::clone(&handle);
        async move { handle.query_composer().await }
    });
    // The query is registered with the engine before the clock moves.
    tokio::task::yield_now().await;
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(3600)).await;
    tokio::task::yield_now().await;
    tokio::task::yield_now().await;
    assert!(
        !query.is_finished(),
        "a query on a busy model must wait for the model, not resolve once a clock bound passes"
    );

    release_tx.send(()).expect("release the model");
    let observation = query.await.expect("the query task must not panic");
    assert_eq!(
        observation.readout,
        ComposerReadout::Body("draft".to_string()),
        "the answer after the model is let go is the screen as it stands, not a gave-up reading"
    );
    holder.join().expect("the holder thread must not panic");
}

#[tokio::test]
async fn detector_handle_query_composer_returns_promptly_after_the_engine_task_has_already_ended() {
    let config = DaemonConfig::default();
    let engine = new_engine(COMPOSER_DEF, &config);
    let (handle, statuses) = DetectorHandle::spawn(engine);
    // Nobody is left to receive a confirmed-status change, so the driver
    // task's next publish attempt fails and it ends -- the engine really has
    // stopped, not merely gone quiet.
    drop(statuses);

    // `Exited` is a debounce-exempt transition, published as soon as this
    // command is applied.
    handle.notify_exited();

    let query = handle.query_composer();
    let observation = tokio::time::timeout(Duration::from_secs(5), query)
        .await
        .expect(
            "query_composer must resolve on its own once the engine task is gone, not rely on \
             this test's outer timeout to end the wait",
        );
    assert_eq!(
        observation.readout,
        ComposerReadout::NotObserved,
        "an engine that can no longer answer must report the composer as unobservable, not hang"
    );
}

// --- The screen slice is synchronous, not I/O -----------------------------

#[tokio::test]
async fn query_composer_needs_no_await_because_the_screen_slice_is_synchronous() {
    let config = DaemonConfig::default();
    let mut engine = new_engine(COMPOSER_DEF, &config);
    engine.feed(&composer_line("draft"));

    // A plain (non-`Future`) return type is what makes this compile without
    // `.await`: if the screen slice ever became I/O-bound and `async`, this
    // assignment would stop type-checking.
    let observation = engine.query_composer();
    assert_eq!(
        observation.readout,
        ComposerReadout::Body("draft".to_string())
    );
}

// --- An unconfirmed Unknown alone is not a lost screen --------------------

#[tokio::test]
async fn an_unconfirmed_status_does_not_by_itself_withhold_the_composer() {
    let config = DaemonConfig::default();
    let mut engine = new_engine(COMPOSER_DEF, &config);

    // Freshly constructed: the confirmed status is Unknown, and the screen has
    // never been lost.
    assert_eq!(engine.status(), WindowStatus::Unknown);

    engine.feed(&composer_line("draft"));
    // Still Unknown: confirm_ms has not elapsed yet. This is ordinary
    // debounce, not a lost screen.
    assert_eq!(engine.status(), WindowStatus::Unknown);

    assert_eq!(
        engine.query_composer().readout,
        ComposerReadout::Body("draft".to_string()),
        "a confirmed status of Unknown must not by itself withhold the composer -- only an \
         actual screen loss does"
    );
}

// --- The two screen-loss events converge on one withdrawal ----------

#[tokio::test]
async fn a_screen_loss_withdraws_composer_trust_until_a_new_confirmed_status_arrives() {
    tokio::time::pause();
    let config = DaemonConfig::default();
    let mut engine = new_engine(COMPOSER_DEF, &config);
    assert!(handoff::is_ready(WindowStatus::WaitingInput));

    engine.feed(&composer_line("draft one"));
    tick_after_ms(&mut engine, CONFIRM_MS).await;
    assert_eq!(engine.status(), WindowStatus::WaitingInput);
    assert_eq!(
        engine.query_composer().readout,
        ComposerReadout::Body("draft one".to_string())
    );

    engine.lose_screen();
    assert_eq!(engine.status(), WindowStatus::Unknown);

    // The exact same well-formed composer reappears, but the window has not
    // yet re-confirmed a status.
    engine.feed(&composer_line("draft one"));
    assert_eq!(
        engine.query_composer().readout,
        ComposerReadout::NotObserved,
        "trust must stay withdrawn even though the repainted screen looks identical to before \
         the loss, because no new confirmed status has been reached yet"
    );

    tick_after_ms(&mut engine, CONFIRM_MS).await;
    assert_eq!(engine.status(), WindowStatus::WaitingInput);
    assert_eq!(
        engine.query_composer().readout,
        ComposerReadout::Body("draft one".to_string()),
        "trust is restored once a new confirmed status is reached"
    );
}

#[tokio::test]
async fn an_idle_status_also_restores_composer_trust_after_a_screen_loss() {
    tokio::time::pause();
    let config = DaemonConfig::default();
    let mut engine = new_engine(COMPOSER_IDLE_DEF, &config);
    assert!(
        handoff::is_ready(WindowStatus::Idle),
        "this case only exercises something a plain-shell window can actually reach"
    );

    engine.lose_screen();
    assert_eq!(engine.status(), WindowStatus::Unknown);

    engine.feed(&composer_line("draft"));
    assert_eq!(
        engine.query_composer().readout,
        ComposerReadout::NotObserved,
        "trust must stay withdrawn until a new status is confirmed"
    );

    tick_after_ms(&mut engine, CONFIRM_MS).await;
    assert_eq!(engine.status(), WindowStatus::Idle);
    assert_eq!(
        engine.query_composer().readout,
        ComposerReadout::Body("draft".to_string()),
        "Idle is a member of handoff::READY_STATUSES, so reaching it after a screen loss must \
         restore trust too -- a plain-shell window never confirms WaitingInput at its prompt"
    );
}

#[tokio::test]
async fn a_status_outside_ready_statuses_does_not_restore_composer_trust() {
    tokio::time::pause();
    let config = DaemonConfig::default();
    let mut engine = new_engine(COMPOSER_RUNNING_DEF, &config);
    assert!(!handoff::is_ready(WindowStatus::Running));

    engine.lose_screen();
    assert_eq!(engine.status(), WindowStatus::Unknown);

    engine.feed(&composer_line("draft"));
    assert_eq!(
        engine.query_composer().readout,
        ComposerReadout::NotObserved
    );

    tick_after_ms(&mut engine, CONFIRM_MS).await;
    assert_eq!(
        engine.status(),
        WindowStatus::Running,
        "a new confirmed status is reached"
    );
    assert_eq!(
        engine.query_composer().readout,
        ComposerReadout::NotObserved,
        "Running is outside handoff::READY_STATUSES, so reaching it after a screen loss must not \
         restore composer trust -- confirming *any* new status is not the condition"
    );
}

#[tokio::test]
async fn a_rebuilt_screen_withdraws_composer_trust_from_a_status_a_report_confirmed() {
    tokio::time::pause();
    let config = DaemonConfig::default();
    let mut engine = new_engine_sized(COMPOSER_DEF, &config, 80, 24);
    assert!(handoff::is_ready(WindowStatus::WaitingInput));

    // Reach a confirmed non-Unknown status through an open-turn report,
    // which never touches the screen, so the panic recipe below starts from
    // the exact preconditions it needs: a screen that has received nothing
    // yet.
    engine.on_status_event(WindowStatus::Running);
    tick_after_ms(&mut engine, CONFIRM_MS).await;
    assert_eq!(engine.status(), WindowStatus::Running);

    engine.feed(RESIZE_CHUNK);
    engine.feed(RESIZE_CHUNK);
    engine.screen().lock().resize(46, 1);
    engine.poison_screen_after_calls(0);
    assert_eq!(
        engine.feed(RESIZE_CHUNK),
        Some(DetectedStatus::Unknown(UnknownReason::ScreenLost)),
        "the canonical rebuild sequence must still confirm Unknown immediately"
    );

    engine.feed(&composer_line("draft two"));
    assert_eq!(
        engine.query_composer().readout,
        ComposerReadout::NotObserved,
        "a rebuilt screen must withdraw composer trust"
    );

    tick_after_ms(&mut engine, CONFIRM_MS).await;
    assert_eq!(engine.status(), WindowStatus::WaitingInput);
    assert_eq!(
        engine.query_composer().readout,
        ComposerReadout::Body("draft two".to_string()),
        "trust is restored once a new confirmed status is reached after the rebuild"
    );
}

// --- No composer section, or no definition at all, never observes -------

#[tokio::test]
async fn a_definition_without_a_composer_section_never_observes_one() {
    tokio::time::pause();
    let config = DaemonConfig::default();
    let mut engine = new_engine(NO_COMPOSER_DEF, &config);

    // The fed line looks exactly like a well-formed composer, but the
    // definition never declared how to recognize one.
    engine.feed(&composer_line("draft"));
    tick_after_ms(&mut engine, CONFIRM_MS).await;

    assert_eq!(
        engine.query_composer().readout,
        ComposerReadout::NotObserved
    );
}

#[tokio::test]
async fn an_engine_with_no_definition_at_all_never_observes_a_composer() {
    let config = DaemonConfig::default();
    let mut engine = DetectionEngine::new(None, &config, screen_of(SCREEN_COLS, SCREEN_ROWS));

    engine.feed(&composer_line("draft"));

    assert_eq!(
        engine.query_composer().readout,
        ComposerReadout::NotObserved
    );
}

// --- The answer says whether the screen was finished painting -------------

/// A CLI hides the cursor while it repaints, or opens a synchronized update
/// around the repaint with the cursor still shown: either way the screen read
/// is not finished, and the answer read from it says so.
#[tokio::test]
async fn the_answer_says_whether_the_screen_it_was_read_from_was_finished_painting() {
    let config = DaemonConfig::default();
    let engine = new_engine(COMPOSER_DEF, &config);
    let screen = engine.screen().clone();
    let (handle, _statuses) = DetectorHandle::spawn(engine);
    feed_handle(&screen, &handle, &composer_line("draft"));

    let steps: [(&[u8], bool, &str); 4] = [
        (
            b"\x1b[?25l",
            false,
            "a hidden cursor marks a paint under way",
        ),
        (
            b"\x1b[?25h",
            true,
            "a shown cursor marks the paint finished",
        ),
        (
            b"\x1b[?2026h",
            false,
            "an open synchronized update marks a paint under way, the cursor shown or not",
        ),
        (
            b"\x1b[?2026l",
            true,
            "a closed synchronized update with the cursor shown marks the paint finished",
        ),
    ];
    for (bytes, painted, what) in steps {
        feed_handle(&screen, &handle, bytes);
        assert_eq!(handle.query_composer().await.painted, painted, "{what}");
    }
}
