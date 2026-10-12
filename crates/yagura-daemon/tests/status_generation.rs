#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The window status generation counter: a `u64` carried alongside a
//! window's confirmed status pair, exposed to a caller through
//! [`yagura_daemon::waiter::StatusAt::generation`] and, on the wire, through
//! `status_rev` on `WindowInfo` and `DaemonMessage::StatusChanged`.
//!
//! Scope: the counter's own rules (monotonic, per-window, carried by the two
//! wire shapes that update a reader's registry) -- not the detection engine's
//! judgement logic (`tests/detector_engine.rs`) nor the waiting primitives
//! that consume a confirmed pair (`tests/waiter.rs`).

mod common;

use std::time::Duration;

use tokio::io::AsyncReadExt;

use yagura_daemon::waiter::StatusAt;
use yagura_daemon::{DaemonConfig, WindowEntry};
use yagura_protocol::{ClientMessage, DaemonMessage, DetectedStatus, UnknownReason, WindowStatus};
use yagura_test_support::ipc::{Connection, try_read_daemon_message};
use yagura_test_support::paths::{unique_detection_dir, write_definition};

/// Reads exactly one frame and decodes its body as a bare JSON value, so a
/// case can assert on the presence or absence of a wire key without going
/// through the typed [`DaemonMessage`] decode (which cannot distinguish "the
/// key is absent" from "the field decoded to `None`").
async fn read_raw_json_frame(client: &mut Connection, timeout: Duration) -> serde_json::Value {
    let mut len_buf = [0u8; 4];
    tokio::time::timeout(timeout, client.read_exact(&mut len_buf))
        .await
        .expect("timed out waiting for a frame length prefix")
        .expect("connection closed while reading a frame length prefix");
    let len = u32::from_le_bytes(len_buf) as usize;
    let mut body = vec![0u8; len];
    tokio::time::timeout(timeout, client.read_exact(&mut body))
        .await
        .expect("timed out waiting for a frame body")
        .expect("connection closed while reading a frame body");
    serde_json::from_slice(&body).expect("daemon must only ever emit well-formed JSON frames")
}

/// Two independent rules so a case can drive the window through a sequence
/// of *different* confirmed statuses (and back to one already seen) with a
/// low, deterministic debounce.
const TWO_RULE_DEFINITION: &str = r#"
schema_version = 1
name = "status-rev-test"

[debounce]
confirm_ms = 20

[[rules]]
status = "running"
pattern = 'RUN-MARK'
priority = 10
region = "bottom"

[[rules]]
status = "idle"
pattern = 'IDLE-MARK'
priority = 10
region = "bottom"
"#;

fn two_rule_config(tag: &str) -> DaemonConfig {
    let dir = unique_detection_dir("yagura-test-status-rev", tag, "create temp detection dir");
    write_definition(&dir, "status-rev-test", TWO_RULE_DEFINITION);
    DaemonConfig {
        detection_dir: Some(dir),
        ..DaemonConfig::default()
    }
}

/// Polls `entry`'s confirmed status until it matches `expected`, returning
/// the [`StatusAt`] pair observed at that point (carrying the generation the
/// transition into `expected` was assigned).
async fn wait_for_confirmed(
    entry: &WindowEntry,
    expected: WindowStatus,
    timeout: Duration,
) -> StatusAt {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let at = entry.status_snapshot();
        if at.status.status() == expected {
            return at;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {expected:?}; last observed {:?}",
            at.status
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Polls `entry`'s confirmed status until it matches the exact `expected`
/// pair (status and reason both), not merely its [`WindowStatus`]
/// projection: `Unknown(NotYetJudged)` and `Unknown(NoRuleMatched)` project
/// to the same `WindowStatus::Unknown`, so [`wait_for_confirmed`] could not
/// tell a window that has been judged from one that has not.
async fn wait_for_detected(
    entry: &WindowEntry,
    expected: DetectedStatus,
    timeout: Duration,
) -> StatusAt {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let at = entry.status_snapshot();
        if at.status == expected {
            return at;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {expected:?}; last observed {:?}",
            at.status
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

// =============================================================================
// Per-window monotonicity (a window's generation only ever increases,
// including across a return to a status already seen)
// =============================================================================

#[tokio::test]
async fn generation_increases_after_a_confirmed_transition() {
    let harness = common::Harness::start("status-rev-mono", two_rule_config("mono")).await;
    let opened = common::open_echo_window(&harness.registry, Some("status-rev-test")).await;
    let entry = opened.entry;

    let baseline = entry.status_snapshot().generation;

    common::write_window_input(&harness.registry, entry.window_id(), b"RUN-MARK\r\n").await;
    let after = wait_for_confirmed(&entry, WindowStatus::Running, Duration::from_secs(10)).await;

    assert!(
        after.generation > baseline,
        "a confirmed transition must strictly increase the generation: baseline {baseline}, \
         after {}",
        after.generation
    );

    harness.registry.terminate_all().await;
}

#[tokio::test]
async fn generation_increases_again_when_a_status_already_seen_recurs() {
    let harness = common::Harness::start("status-rev-recur", two_rule_config("recur")).await;
    let opened = common::open_echo_window(&harness.registry, Some("status-rev-test")).await;
    let entry = opened.entry;

    // The first Running comes from an open-turn report (the same path
    // `ClientMessage::StatusEvent` uses) over a screen that has shown nothing
    // yet. Idle then comes from its rule's marker, which outranks an open
    // turn. The last Running comes from its own marker: both rules share a
    // priority and region, and the one declared first wins once both markers
    // are visible, however the console echo places them.
    assert!(
        entry.record_status_event(WindowStatus::Running),
        "a living window must accept a structured-source status event"
    );
    let first_running =
        wait_for_confirmed(&entry, WindowStatus::Running, Duration::from_secs(10)).await;

    common::write_window_input(
        &harness.registry,
        entry.window_id(),
        b"IDLE-MARK
",
    )
    .await;
    let idle = wait_for_confirmed(&entry, WindowStatus::Idle, Duration::from_secs(10)).await;
    assert!(
        idle.generation > first_running.generation,
        "moving to idle must increase the generation past the first running: \
         running {}, idle {}",
        first_running.generation,
        idle.generation
    );

    common::write_window_input(
        &harness.registry,
        entry.window_id(),
        b"RUN-MARK
",
    )
    .await;
    let second_running =
        wait_for_confirmed(&entry, WindowStatus::Running, Duration::from_secs(10)).await;
    assert!(
        second_running.generation > idle.generation,
        "re-confirming a status already seen once must still strictly increase the \
         generation, not reuse the value assigned to it the first time: first running {}, \
         idle {}, second running {}",
        first_running.generation,
        idle.generation,
        second_running.generation
    );

    harness.registry.terminate_all().await;
}

// =============================================================================
// Per-window independence
// =============================================================================

#[tokio::test]
async fn generation_is_independent_across_windows() {
    let harness = common::Harness::start("status-rev-indep", two_rule_config("indep")).await;
    let a = common::open_echo_window(&harness.registry, Some("status-rev-test"))
        .await
        .entry;
    let b = common::open_echo_window(&harness.registry, Some("status-rev-test"))
        .await
        .entry;

    // b's own first judgement must land before its baseline is read, or the
    // assertion below would blame it on a. The echo child prints nothing until
    // it reads input and this definition has no time-based deadline, so b is
    // only ever judged once fed output.
    common::write_window_input(&harness.registry, b.window_id(), b"no-match-here\r\n").await;
    wait_for_detected(
        &b,
        DetectedStatus::Unknown(UnknownReason::NoRuleMatched),
        Duration::from_secs(10),
    )
    .await;
    let b_baseline = b.status_snapshot().generation;

    assert!(a.record_status_event(WindowStatus::Running));
    wait_for_confirmed(&a, WindowStatus::Running, Duration::from_secs(10)).await;

    assert_eq!(
        b.status_snapshot().generation,
        b_baseline,
        "a transition confirmed on window a must not move window b's own generation"
    );

    harness.registry.terminate_all().await;
}

// =============================================================================
// Wire carriers: WindowInfo.status_rev and StatusChanged.status_rev match the
// entry's own generation; the request/response status query carries none.
// =============================================================================

#[tokio::test]
async fn window_list_reports_the_status_rev_matching_the_entrys_generation() {
    let harness = common::Harness::start("status-rev-list", two_rule_config("list")).await;
    let opened = common::open_echo_window(&harness.registry, Some("status-rev-test")).await;
    let entry = opened.entry;
    let window_id = entry.window_id().to_string();

    common::write_window_input(&harness.registry, &window_id, b"RUN-MARK\r\n").await;
    let at = wait_for_confirmed(&entry, WindowStatus::Running, Duration::from_secs(10)).await;

    let mut client = harness.connect().await;
    let windows = common::call_list_windows(&mut client).await;
    let info = windows
        .iter()
        .find(|w| w.window_id == window_id)
        .unwrap_or_else(|| panic!("window {window_id:?} not found in window_list"));
    assert_eq!(
        info.status_rev,
        Some(at.generation),
        "window_list's status_rev must match the entry's own generation"
    );

    harness.registry.terminate_all().await;
}

#[tokio::test]
async fn status_changed_frames_carry_a_strictly_increasing_status_rev() {
    let harness = common::Harness::start("status-rev-changed", two_rule_config("changed")).await;
    let opened = common::open_echo_window(&harness.registry, Some("status-rev-test")).await;
    let entry = opened.entry;
    let window_id = entry.window_id().to_string();

    let mut client = harness.connect().await;
    common::attach(&mut client, &window_id).await;

    // See generation_increases_again_when_a_status_already_seen_recurs for
    // why the first transition comes from a report and the second from a
    // rule's marker.
    assert!(entry.record_status_event(WindowStatus::Running));
    let running = wait_for_confirmed(&entry, WindowStatus::Running, Duration::from_secs(10)).await;

    common::write_window_input(
        &harness.registry,
        &window_id,
        b"IDLE-MARK
",
    )
    .await;
    let idle = wait_for_confirmed(&entry, WindowStatus::Idle, Duration::from_secs(10)).await;

    // Drains every frame the daemon has already queued for this connection,
    // stopping once no frame arrives within the idle window (not once a
    // fixed count is reached): the exact number of StatusChanged frames
    // buffered ahead of the two transitions this case caused (the post-
    // attach snapshot, and possibly the two transitions themselves) is not
    // pinned by the contract, only their content is.
    let mut seen_revs = Vec::new();
    while let Some(msg) = try_read_daemon_message(&mut client, Duration::from_millis(500)).await {
        match msg {
            DaemonMessage::StatusChanged { status_rev, .. } => {
                if let Some(rev) = status_rev {
                    seen_revs.push(rev);
                }
            }
            DaemonMessage::Frame(_) => {}
            other => panic!("unexpected frame while draining StatusChanged: {other:?}"),
        }
    }

    assert!(
        seen_revs.len() >= 2,
        "expected at least two StatusChanged frames carrying status_rev, got {seen_revs:?}"
    );
    for pair in seen_revs.windows(2) {
        assert!(
            pair[1] > pair[0],
            "status_rev must be strictly increasing across StatusChanged frames, got {seen_revs:?}"
        );
    }
    let last = *seen_revs.last().expect("checked len >= 2 above");
    assert_eq!(
        last, idle.generation,
        "the last observed status_rev must match the entry's own generation once settled \
         (running generation was {}), proving the wire carries the pair's own generation \
         rather than re-deriving one at send time",
        running.generation
    );

    harness.registry.terminate_all().await;
}

#[tokio::test]
async fn get_window_status_response_carries_no_generation() {
    let harness =
        common::Harness::start("status-rev-getstatus", two_rule_config("getstatus")).await;
    let window_id = common::open_echo_window(&harness.registry, Some("status-rev-test"))
        .await
        .window_id;

    let mut client = harness.connect().await;
    common::send(
        &mut client,
        &ClientMessage::GetWindowStatus {
            window_id: window_id.clone(),
        },
    )
    .await;
    let value = loop {
        let value = read_raw_json_frame(&mut client, Duration::from_secs(5)).await;
        let kind = value.get("type").and_then(serde_json::Value::as_str);
        if kind == Some("window_status") {
            break value;
        }
        // Skip an interleaved output/status_changed notification, same as
        // read_response's typed skip.
    };
    assert!(
        value.get("status_rev").is_none(),
        "the request/response status query must never carry a generation, got: {value}"
    );

    harness.registry.terminate_all().await;
}

// =============================================================================
// Type-level proof: the fan-out payload itself carries the pair and the
// generation together, in one value, so a receiver never has to read the
// window back to build a StatusChanged-equivalent message. This is stronger
// than the wire-level tests above: those only show the two values agree on
// this run, not that the code holding the receiver was structurally unable
// to consult the window for anything else.
// =============================================================================

/// Exactly the three fields `DaemonMessage::StatusChanged` reports besides
/// `window_id`, built from nothing but one fan-out value. The signature is
/// the proof: it takes no `&WindowEntry` and no registry lookup, so nothing
/// this function does can read the window back -- only the recipient of one
/// `WindowEntry::subscribe_status()` value can supply its inputs.
fn status_changed_fields_from(at: StatusAt) -> (WindowStatus, Option<UnknownReason>, u64) {
    (
        at.status.status(),
        at.status.unknown_reason(),
        at.generation,
    )
}

#[tokio::test]
async fn subscribe_status_delivers_the_pair_and_generation_together_in_one_value() {
    let harness =
        common::Harness::start("status-rev-fanout-pair", two_rule_config("fanout-pair")).await;
    let opened = common::open_echo_window(&harness.registry, Some("status-rev-test")).await;
    let entry = opened.entry;
    let window_id = entry.window_id().to_string();

    let mut rx = entry.subscribe_status();

    common::write_window_input(&harness.registry, &window_id, b"RUN-MARK\r\n").await;
    let confirmed =
        wait_for_confirmed(&entry, WindowStatus::Running, Duration::from_secs(10)).await;

    // Read that transition's own value off the fan-out, picked by generation;
    // an earlier transition may still be queued ahead of it. Everything a
    // caller needs to build the outgoing StatusChanged message must already be
    // inside it.
    let received: StatusAt = loop {
        let value = rx
            .recv()
            .await
            .expect("a confirmed transition must publish on the fan-out this entry owns");
        if value.generation >= confirmed.generation {
            break value;
        }
    };

    assert_eq!(
        received.status.status(),
        WindowStatus::Running,
        "the fan-out value's own status must be the confirmed one, with no need to read \
         the window back"
    );
    assert_eq!(
        received.status.unknown_reason(),
        None,
        "a known status carries no reason, straight from the received value"
    );
    assert_eq!(
        received.generation,
        entry.status_snapshot().generation,
        "the generation carried by this single fan-out value must equal the one actually \
         assigned to this confirmed pair (the entry's own record), proving the value alone \
         -- not a value re-derived by reading the window at receive time -- is what a \
         receiver would report"
    );

    harness.registry.terminate_all().await;
}

#[tokio::test]
async fn status_changed_fields_are_derivable_from_the_fan_out_value_alone() {
    let harness =
        common::Harness::start("status-rev-fanout-derive", two_rule_config("fanout-derive")).await;
    let opened = common::open_echo_window(&harness.registry, Some("status-rev-test")).await;
    let entry = opened.entry;
    let window_id = entry.window_id().to_string();

    let mut rx = entry.subscribe_status();

    common::write_window_input(&harness.registry, &window_id, b"RUN-MARK\r\n").await;
    let ground_truth =
        wait_for_confirmed(&entry, WindowStatus::Running, Duration::from_secs(10)).await;

    // An earlier transition may still be queued ahead of the confirmed one.
    let received: StatusAt = loop {
        let value = rx
            .recv()
            .await
            .expect("a confirmed transition must publish on the fan-out this entry owns");
        if value.generation >= ground_truth.generation {
            break value;
        }
    };
    let (status, reason, status_rev) = status_changed_fields_from(received);

    assert_eq!(status, WindowStatus::Running);
    assert_eq!(reason, None);
    assert_eq!(
        status_rev, ground_truth.generation,
        "a message built from nothing but the received value must still match the ground \
         truth recorded for this transition"
    );

    harness.registry.terminate_all().await;
}
