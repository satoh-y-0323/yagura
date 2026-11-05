#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Second check on the effective debounce a definition's `[input]` table
//! must outlast, run at window-open time rather than at load time.
//!
//! `detector::definition::validate_input_section` (the definition load's
//! check) can only compare `input.submit_delay_ms` against a
//! definition's own `[debounce].confirm_ms`: when a definition omits
//! `[debounce]`, the effective `confirm_ms` a window actually runs under is
//! `DaemonConfig::confirm_ms` (the daemon-wide default), a value the loader
//! never reads. This file's own
//! check runs where both values are actually known --
//! `WindowRegistry::initialize_detection_for_window` -- and treats a
//! violation the same way every other load failure is treated: no
//! definition, `Unknown` forever, and a caller-facing warning.
//!
//! Both cases below share one definition (`submit_delay_ms = 400`,
//! `delivery_confirm_ms = 800`, no `[debounce]`) and differ only in the
//! daemon's own `confirm_ms`, so the daemon setting is the only variable
//! under test.

mod common;

use std::path::Path;
use std::time::Duration;

use tokio::time::sleep;

use yagura_daemon::{DaemonConfig, WindowRegistry};
use yagura_protocol::WindowStatus;
use yagura_test_support::paths::{unique_detection_dir, write_definition};

/// Pattern this file's definition maps to `waiting_input`.
const MARKER: &str = "SECOND-CHECK-MARK";

/// Bytes written to the window's PTY to trigger [`MARKER`]'s rule.
const MARKER_LINE: &[u8] = b"SECOND-CHECK-MARK\r\n";

/// Name this file's definition is written under.
const DETECTION_NAME: &str = "e3-second-check";

/// A definition with `[input] submit_delay_ms = 400 / delivery_confirm_ms =
/// 800` and **no `[debounce]` table**, so the definition-load-time check
/// cannot see it: the effective `confirm_ms` a window
/// opened against this definition runs under is whatever `DaemonConfig`
/// supplies.
fn definition_without_debounce() -> String {
    format!(
        r#"
schema_version = 1
name = "{DETECTION_NAME}"

[[rules]]
status = "waiting_input"
pattern = '{MARKER}'
priority = 10
region = "bottom"

[input]
submit_delay_ms = 400
delivery_confirm_ms = 800
"#
    )
}

/// Writes [`definition_without_debounce`] to `dir` and returns a
/// `DaemonConfig` pointing at it with `confirm_ms` set to `daemon_confirm_ms`.
fn config_with_daemon_confirm(dir: &Path, daemon_confirm_ms: u64) -> DaemonConfig {
    write_definition(dir, DETECTION_NAME, &definition_without_debounce());
    DaemonConfig {
        detection_dir: Some(dir.to_path_buf()),
        confirm_ms: daemon_confirm_ms,
        ..DaemonConfig::default()
    }
}

/// Asserts that `window_id` runs no detection definition: the engine it was
/// given has no rule to leave `Unknown` with, so it stays `Unknown` whatever
/// the window prints (the engine's own
/// `missing_definition_keeps_confirmed_status_unknown`).
fn assert_runs_no_definition(registry: &WindowRegistry, window_id: &str) {
    let entry = registry.get(window_id).expect("window must exist");
    assert_eq!(
        entry.detection_definition(),
        None,
        "window {window_id} must run no definition: a definition whose \
         input.submit_delay_ms does not outlast the effective confirm_ms must be treated \
         as a load failure, same as an unparseable one"
    );
    assert_eq!(entry.current_status(), WindowStatus::Unknown);
}

/// Polls `entry.current_status()` until it reports `target`.
async fn wait_for_status(registry: &WindowRegistry, window_id: &str, target: WindowStatus) {
    let deadline = tokio::time::Instant::now() + yagura_test_support::HANG_GUARD;
    loop {
        let entry = registry.get(window_id).expect("window must exist");
        if entry.current_status() == target {
            return;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!(
                "timed out waiting for window {window_id} to reach {target:?}; status is {:?}",
                entry.current_status()
            );
        }
        sleep(Duration::from_millis(50)).await;
    }
}

/// The daemon default `confirm_ms` (`DaemonConfig::default().confirm_ms`) is
/// 300ms, shorter than this file's `submit_delay_ms = 400`: the front
/// half of the required inequality (`submit_delay_ms > effective
/// confirm_ms`) holds, so opening a window against the definition with the
/// daemon left at its own default must behave exactly as if the definition
/// had declared `[debounce] confirm_ms = 300` itself -- no warning, and the
/// window reaches `waiting_input` normally. This is the contrast case: the
/// only thing that changes between this test and the one below is the
/// daemon's own `confirm_ms`.
#[tokio::test]
async fn a_definition_without_debounce_reaches_waiting_input_when_the_daemon_default_confirm_is_shorter_than_submit_delay()
 {
    let dir = unique_detection_dir(
        "e3-second-check-pass",
        "case",
        "create detection dir for the passing case",
    );
    let registry = WindowRegistry::new(config_with_daemon_confirm(&dir, 300));

    let opened = common::open_echo_window(&registry, Some(DETECTION_NAME)).await;
    assert_eq!(
        opened.detection_warning, None,
        "submit_delay_ms (400) outlasts the daemon's default confirm_ms (300), so the \
         definition must load and run with no warning, got: {:?}",
        opened.detection_warning
    );

    assert!(
        registry
            .get(&opened.window_id)
            .expect("window must exist")
            .detection_definition()
            .is_some(),
        "the definition loads, so the window runs it"
    );

    common::write_window_input(&registry, &opened.window_id, MARKER_LINE).await;
    wait_for_status(&registry, &opened.window_id, WindowStatus::WaitingInput).await;
}

/// The failing case: the daemon's own `confirm_ms`
/// is raised to 500ms, which the definition's `submit_delay_ms = 400` no
/// longer outlasts. `validate_input_section` (load time) cannot see this --
/// it never reads `DaemonConfig` -- so the gap
/// the load-time check leaves is exactly what is exercised here: the definition still loads,
/// and nothing today stops the body's own echo (matched by the very rule
/// this test writes to the PTY) from being read as a confirmed reaction to
/// a submit key that was never meaningfully separated from it in time.
///
/// The second check -- run where a window's detection setup
/// resolves its loaded definition, the one place the definition's own
/// (absent) `[debounce]` and the daemon's own default `confirm_ms` are both
/// visible together -- must treat this the same as a load failure: no
/// definition, `Unknown` forever, and a `detection_warning` naming both
/// `input.submit_delay_ms` and the fact that the effective `confirm_ms` it
/// was judged against came from the daemon's own default (there being no
/// `[debounce]` table in the definition to name instead).
#[tokio::test]
async fn a_definition_without_debounce_stays_unknown_when_the_daemon_default_confirm_is_not_outlasted_by_submit_delay()
 {
    let dir = unique_detection_dir(
        "e3-second-check-fail",
        "case",
        "create detection dir for the failing case",
    );
    let registry = WindowRegistry::new(config_with_daemon_confirm(&dir, 500));

    let opened = common::open_echo_window(&registry, Some(DETECTION_NAME)).await;

    let warning = opened.detection_warning.clone().unwrap_or_else(|| {
        panic!(
            "input.submit_delay_ms (400) does not outlast the daemon's default confirm_ms \
             (500); the window-open second check must reject this the same way an \
             unparseable definition is rejected, and report a detection_warning -- got \
             detection_warning: None"
        )
    });
    assert!(
        warning.contains("input.submit_delay_ms"),
        "the warning must name the input key it judged, got: {warning}"
    );
    assert!(
        warning.contains("default") && warning.to_lowercase().contains("confirm_ms"),
        "the warning must identify that the effective confirm_ms it judged against came \
         from the daemon's own default (this definition has no [debounce] table to name \
         instead), got: {warning}"
    );

    assert_runs_no_definition(&registry, &opened.window_id);
}
