#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Contract for the Unknown-reason surface over IPC: `WindowStatus` /
//! `WindowList` (via `WindowInfo`), `StatusChanged`, the destructive-tool
//! refusal text and the profile-handoff "not ready" message, plus the confirmed-transition
//! log line.
//!
//! Every case drives a real `WindowRegistry` + `IpcServer` pair over a real
//! IPC endpoint through `common::Harness`, the same convention every other
//! file in this suite uses.

mod common;

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::Duration;

use common::{
    CONTROL_DETECTION_NAME, Harness, approval_config, call_list_windows, decide, detection_config,
    expect_approval_requested, expect_error, query_status, read_response, register_approver, send,
    unique_temp_dir, wait_until_status, write_window_input,
};
use yagura_daemon::DaemonConfig;
use yagura_daemon::detector::layers::LayerError;
use yagura_protocol::{ClientMessage, DaemonMessage, UnknownReason, WindowStatus};
use yagura_test_support::ipc::{Connection, ReadOutcome, read_daemon_frame, send_client_message};
use yagura_test_support::paths::TempDir;
use yagura_test_support::process::echo_child_command;

/// Six lines of plain text, none containing common::control_definition's `CTRL-READY`
/// marker, long enough to scroll a prior `CTRL-READY` line out of the
/// matched region (`bottom_line_count` defaults to 5) through a real PTY
/// round trip.
///
/// An ANSI clear+home sequence (`common::repaint`) written the same way
/// `write_window_input` writes `READY_MARKER` does not survive that round
/// trip the way it does when fed straight into `DetectionEngine::on_output`:
/// `echo_child`'s default mode never raw-modes its own console, so the
/// console's own cooked line-editing input mode intercepts or drops the
/// escape bytes rather than passing them through, and the prior `CTRL-READY`
/// line stays visible and matching. Plain, `\r\n`-terminated lines are what
/// cooked mode is for, so they survive the round trip reliably instead
/// (confirmed against this suite's own `Harness` by inspecting scrollback
/// directly before choosing this fixture).
const SCROLL_CTRL_READY_OUT: &[u8] = b"filler line one\r\nfiller line two\r\nfiller line three\r\n\
                                       filler line four\r\nfiller line five\r\nfiller line six\r\n";

/// One `GetWindowStatus` round trip, returning every field this task adds
/// alongside the existing `status`.
async fn full_window_status(
    client: &mut Connection,
    window_id: &str,
) -> (WindowStatus, Option<UnknownReason>, Option<String>) {
    send(
        client,
        &ClientMessage::GetWindowStatus {
            window_id: window_id.to_string(),
        },
    )
    .await;
    match read_response(client, Duration::from_secs(5)).await {
        DaemonMessage::WindowStatus {
            status,
            unknown_reason,
            detection_warning,
            ..
        } => (status, unknown_reason, detection_warning),
        other => panic!("expected window_status, got {other:?}"),
    }
}

// =============================================================================
// IPC surface
// =============================================================================

#[tokio::test]
async fn a_configured_directory_layer_missing_the_named_definition_reports_no_definition_with_a_warning()
 {
    let dir = unique_temp_dir("t4a-dir-missing-file");
    // No file written: the default detection_name's .toml is absent from this
    // configured directory layer.
    let config = DaemonConfig {
        detection_dir: Some(dir),
        ..DaemonConfig::default()
    };
    let h = Harness::start("t4a-dir-missing-file", config).await;
    let mut client = h.connect().await;
    let window_id = h.open(echo_child_command(), None).await;

    // Read without giving the window a single byte of output: a
    // definition-less engine never publishes a confirmed transition, so the
    // initial value is the whole answer.
    let (status, unknown_reason, detection_warning) =
        full_window_status(&mut client, &window_id).await;
    assert_eq!(status, WindowStatus::Unknown);
    assert_eq!(unknown_reason, Some(UnknownReason::NoDefinition));
    assert!(
        detection_warning.is_some_and(|warning| !warning.is_empty()),
        "a configured-but-missing directory layer must still carry a non-empty warning"
    );
}

#[tokio::test]
async fn neither_detection_layer_configured_reports_no_definition_with_the_missing_layer_warning() {
    let config = DaemonConfig::default();
    let h = Harness::start("t4a2-no-layer", config).await;
    let mut client = h.connect().await;
    let window_id = h.open(echo_child_command(), None).await;

    let (status, unknown_reason, detection_warning) =
        full_window_status(&mut client, &window_id).await;
    assert_eq!(status, WindowStatus::Unknown);
    assert_eq!(
        unknown_reason,
        Some(UnknownReason::NoDefinition),
        "the reason must be no_definition regardless of whether a warning exists"
    );
    // No layer configured at all states that fact instead of staying silent.
    assert_eq!(
        detection_warning,
        Some(LayerError::Disabled.to_string()),
        "no layer configured at all now states that fact instead of staying silent, got {:?}",
        detection_warning
    );
}

#[tokio::test]
async fn a_definition_present_but_unmatched_reports_no_rule_matched_without_a_warning() {
    let dir = unique_temp_dir("t4b-no-rule-matched");
    let config = detection_config(&dir, DaemonConfig::default());
    let h = Harness::start("t4b-no-rule-matched", config).await;
    let mut client = h.connect().await;
    let window_id = h
        .open(echo_child_command(), Some(CONTROL_DETECTION_NAME))
        .await;

    write_window_input(&h.registry, &window_id, SCROLL_CTRL_READY_OUT).await;

    // Waits for the engine's first judgment of that output, whatever it is.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let (status, unknown_reason, detection_warning) = loop {
        let reading = full_window_status(&mut client, &window_id).await;
        if reading.1 != Some(UnknownReason::NotYetJudged) {
            break reading;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the engine never judged the output written to {window_id}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(status, WindowStatus::Unknown);
    assert_eq!(unknown_reason, Some(UnknownReason::NoRuleMatched));
    assert_eq!(detection_warning, None);
}

#[tokio::test]
async fn a_confirmed_status_carries_neither_key() {
    let dir = unique_temp_dir("t4c-confirmed");
    let config = detection_config(&dir, DaemonConfig::default());
    let h = Harness::start("t4c-confirmed", config).await;
    let mut client = h.connect().await;
    let window_id = h.open_idle_window(&mut client).await;

    let (status, unknown_reason, detection_warning) =
        full_window_status(&mut client, &window_id).await;
    assert_eq!(status, WindowStatus::Idle);
    assert_eq!(unknown_reason, None);
    assert_eq!(detection_warning, None);
}

#[tokio::test]
async fn a_definition_present_before_the_first_confirmation_reports_not_yet_judged() {
    let dir = unique_temp_dir("t4d-not-yet-judged");
    let config = detection_config(&dir, DaemonConfig::default());
    let h = Harness::start("t4d-not-yet-judged", config).await;
    let mut client = h.connect().await;
    let window_id = h
        .open(echo_child_command(), Some(CONTROL_DETECTION_NAME))
        .await;

    // No output at all yet: this is what distinguishes (d) from (b); the
    // definition being present (unlike a/a2) is what distinguishes it from
    // no_definition.
    let (status, unknown_reason, detection_warning) =
        full_window_status(&mut client, &window_id).await;
    assert_eq!(status, WindowStatus::Unknown);
    assert_eq!(unknown_reason, Some(UnknownReason::NotYetJudged));
    assert_eq!(detection_warning, None);
}

#[tokio::test]
async fn a_status_changed_notification_carries_the_reason() {
    let dir = unique_temp_dir("t4e-status-changed-reason");
    let config = detection_config(&dir, DaemonConfig::default());
    let h = Harness::start("t4e-status-changed-reason", config).await;
    let mut client = h.connect().await;
    let window_id = h.open_idle_window(&mut client).await;

    send_client_message(
        &mut client,
        &ClientMessage::AttachView {
            window_id: window_id.clone(),
            size: None,
        },
    )
    .await;
    loop {
        match read_daemon_frame(&mut client, Duration::from_secs(5)).await {
            ReadOutcome::Message(DaemonMessage::Attached { .. }) => break,
            ReadOutcome::Message(_) => continue,
            ReadOutcome::Closed => panic!("connection closed while waiting for attach"),
        }
    }

    // Scroll CTRL-READY out of the matched region: a real Idle -> Unknown
    // transition through the real production path, driven directly (the
    // same PTY-input bypass drive_window_idle-style helpers in this suite
    // use for stimulating a window out of band).
    write_window_input(&h.registry, &window_id, SCROLL_CTRL_READY_OUT).await;

    // The attach handler above answers with one StatusChanged of its own
    // (echoing the current, still-Idle status) before the real transition
    // this test is about; only the Unknown one is the assertion target.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(
            !remaining.is_zero(),
            "timed out waiting for the Unknown StatusChanged push"
        );
        match read_daemon_frame(&mut client, remaining).await {
            ReadOutcome::Message(DaemonMessage::StatusChanged {
                window_id: id,
                status,
                unknown_reason,
                ..
            }) if id == window_id && status == WindowStatus::Unknown => {
                assert_eq!(unknown_reason, Some(UnknownReason::NoRuleMatched));
                break;
            }
            ReadOutcome::Message(_) => continue,
            ReadOutcome::Closed => panic!("connection closed while waiting for StatusChanged"),
        }
    }
}

#[tokio::test]
async fn interrupt_window_approved_after_falling_back_to_unknown_reports_the_reason_in_the_refusal()
{
    // The post-approval re-judgement path: the decision was
    // made against an old, non-Unknown status; by the time it resolves the
    // window has fallen back to Unknown, and the refusal must still name why.
    let dir = unique_temp_dir("t4f-post-approval-recheck-reason");
    let config = approval_config(&dir, 30);
    let h = Harness::start("t4f-post-approval-recheck-reason", config).await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;

    let mut probe = h.connect().await;
    let window_id = h.open_unknown_window().await;
    assert_eq!(
        query_status(&mut probe, &window_id).await,
        WindowStatus::Unknown,
        "fixture precondition: a window opened with no per-window override falls back to \
         common::control_definition's own unmatched-rule Unknown"
    );

    // Force a short-lived non-Unknown status by opening a turn with a
    // report, and submit the interrupt while it is still in effect.
    send(
        &mut probe,
        &ClientMessage::StatusEvent {
            source: "unknown-reason-recheck-probe".to_string(),
            status: WindowStatus::Running,
            window_id: Some(window_id.clone()),
            token: common::VALID_TOKEN.to_string(),
        },
    )
    .await;
    wait_until_status(
        &mut probe,
        &window_id,
        WindowStatus::Running,
        Duration::from_secs(5),
    )
    .await;

    let mut requester = h.connect().await;
    send(
        &mut requester,
        &ClientMessage::InterruptWindow {
            window_id: window_id.clone(),
        },
    )
    .await;
    let request = expect_approval_requested(&mut approver).await;
    assert_eq!(
        request.target.as_ref().map(|target| target.status),
        Some(WindowStatus::Running),
        "the request must have been submitted while the window was Running, not Unknown, or this \
         test would prove nothing about the re-check at decision time"
    );

    // Let the open turn go quiet, so the window falls back to Unknown before
    // the decision is made.
    wait_until_status(
        &mut probe,
        &window_id,
        WindowStatus::Unknown,
        Duration::from_secs(15),
    )
    .await;

    decide(&mut approver, &request.request_id, true).await;

    let msg = read_response(&mut requester, Duration::from_secs(5)).await;
    let message = expect_error(msg, "status_unknown");
    assert!(
        message.contains(UnknownReason::NoRuleMatched.phrase()),
        "the refusal after a post-approval re-check must still name the reason: {message:?}"
    );
}

#[tokio::test]
async fn send_text_to_an_unknown_window_with_no_definition_names_the_reason_in_the_entry_gate_refusal()
 {
    // The entry gate (check_destructive, reached for every ordinary send_message
    // / send_handoff / interrupt_window that never becomes a pending approval)
    // is a call site distinct from the post-approval re-judgement tested above.
    // This window is Unknown(NoDefinition), not Unknown(NoRuleMatched): a wrong
    // implementation that hardcodes one reason at either call site, or
    // reconstructs the reason from current_status() alone (the pair
    // reconstruction is forbidden), must diverge from one of the two
    // tests.
    let h = Harness::start("t4f-entry-gate-reason", DaemonConfig::default()).await;
    let mut client = h.connect().await;
    let window_id = h.open_unknown_window().await;
    assert_eq!(
        query_status(&mut client, &window_id).await,
        WindowStatus::Unknown,
        "fixture precondition: a window with neither detection layer configured stays \
         Unknown(NoDefinition)"
    );

    // No approval policy at all: status_gate denies before check_destructive
    // ever reaches the approval-needed branch, so this exercises the entry
    // gate alone, with no approval decision pending or made.
    send(
        &mut client,
        &ClientMessage::SendText {
            window_id: window_id.clone(),
            text: "hello".to_string(),
        },
    )
    .await;

    let msg = read_response(&mut client, Duration::from_secs(5)).await;
    let message = expect_error(msg, "status_unknown");
    assert_eq!(
        message,
        format!(
            "the window's status could not be detected ({}), so this operation is refused",
            UnknownReason::NoDefinition.phrase()
        ),
        "the entry gate's refusal must be the reason-carrying refusal text verbatim"
    );
    assert!(
        !message.contains(UnknownReason::NoRuleMatched.phrase()),
        "the entry gate must not report a different window's reason (no_rule_matched), which \
         would indicate a hardcoded or misrouted value: {message:?}"
    );
}

#[tokio::test]
async fn send_handoff_from_a_profile_whose_detection_never_resolves_names_the_reason_in_the_not_ready_message()
 {
    // Only the "last status: {status}" fragment carries the reason; the
    // detection-warning section, the ready-hold section and their order are
    // the same as for any other not-ready status.
    let tree = TempDir::new("yagura-cli-test", "t4g-not-ready-reason");
    let profiles_dir = tree.make_dir("profiles");
    let detection_dir = tree.make_dir("detection");
    // A detection name nothing in `detection_dir` defines: the window stays
    // Unknown(NoDefinition) forever, the same fail-safe fixture
    // `open_unknown_window` uses.
    common::write_profile(
        &profiles_dir,
        "unresolvable",
        &echo_child_profile_source_with_detection("Unresolvable", "does-not-exist-anywhere"),
    );

    let mut config = common::handoff_profile_config(
        Some(profiles_dir),
        common::empty_lookup(),
        &detection_dir,
        None,
        Some(common::policy(&["send_handoff"])),
        30,
    );
    config.handoff_ready_timeout_secs = 1;
    let h = Harness::start("t4g-not-ready-reason", config).await;
    let mut client = h.connect().await;

    let msg = common::call_send_handoff_from_profile(
        &mut client,
        "unresolvable",
        None,
        None,
        "src/foo.rs",
        "look",
    )
    .await;
    let message = expect_error(msg, "window_not_ready");

    assert!(
        message.starts_with(
            "the window opened for this handoff did not become ready within 1s (last status: \
             unknown ("
        ),
        "the fragment must be replaced with the reason-carrying form: {message:?}"
    );
    assert!(
        message.contains(&format!(
            "last status: unknown ({})",
            UnknownReason::NoDefinition.phrase()
        )),
        "the phrase named must be no_definition's, since the profile names a detection nothing \
         resolves: {message:?}"
    );
    assert!(
        message.contains("; detection warning: "),
        "the detection-warning section must survive unchanged: {message:?}"
    );
    assert!(
        !message.contains("the definition asks this window to hold"),
        "echo_child's profile carries no [input] table, so ready_hold is zero and the \
         ready-hold section must stay absent: {message:?}"
    );
}

fn echo_child_profile_source_with_detection(display_name: &str, detection: &str) -> String {
    let path = yagura_test_support::process::echo_child_path();
    format!(
        "schema = 1\ndisplay_name = \"{display_name}\"\ndetection = \"{detection}\"\n\n\
         [command]\ndefault = ['{}']\n",
        path.display()
    )
}

// =============================================================================
// The confirmed-transition log line
// =============================================================================

/// One captured `tracing` event: level, and every recorded field normalized
/// to a string. A field recorded through `record_str`/`record_debug` of a
/// `&str`/`String` value comes back wrapped in `Debug`-style quotes; those
/// are stripped here so a field recorded via `%field` (no quotes) and one
/// recorded via a plain string field (quoted) compare the same way.
#[derive(Debug, Clone)]
struct CapturedEvent {
    level: tracing::Level,
    fields: HashMap<String, String>,
}

impl CapturedEvent {
    fn message(&self) -> Option<&str> {
        self.fields.get("message").map(String::as_str)
    }
}

#[derive(Default)]
struct FieldVisitor {
    fields: HashMap<String, String>,
}

impl tracing::field::Visit for FieldVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        let text = format!("{value:?}");
        let normalized = text
            .strip_prefix('"')
            .and_then(|rest| rest.strip_suffix('"'))
            .map(str::to_string)
            .unwrap_or(text);
        self.fields.insert(field.name().to_string(), normalized);
    }
}

/// A process-wide `tracing` subscriber, installed exactly once for this
/// whole test binary via [`install_capture`].
///
/// A global default rather than a scoped `tracing::subscriber::with_default`:
/// the confirmed-transition log line this file tests fires inside
/// `spawn_status_forwarder`'s own spawned task, not the caller's -- the same
/// situation `src/window.rs`'s own `before_spawn_snapshot_failure_emits_a_warning`
/// test documents (it uses `tracing_test` instead of a scoped subscriber for
/// exactly this reason: `tracing`'s per-callsite `Interest` cache is
/// process-global, so a scoped, thread-local subscriber can lose the race to
/// another thread that touches the same call site first with no subscriber
/// active, permanently caching "not interested" for it). A single global
/// default sidesteps that race outright (`enabled` always returns `true`,
/// and `tracing::subscriber::set_global_default` forces every call site's
/// cached interest to be re-evaluated); per-test isolation comes from
/// filtering the shared buffer by each test's own unique `window_id` rather
/// than from thread scoping.
struct GlobalCapture {
    events: std::sync::Arc<Mutex<Vec<CapturedEvent>>>,
}

impl tracing::Subscriber for GlobalCapture {
    fn enabled(&self, _metadata: &tracing::Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }

    fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}

    fn event(&self, event: &tracing::Event<'_>) {
        let mut visitor = FieldVisitor::default();
        event.record(&mut visitor);
        self.events
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(CapturedEvent {
                level: *event.metadata().level(),
                fields: visitor.fields,
            });
    }

    fn enter(&self, _span: &tracing::span::Id) {}

    fn exit(&self, _span: &tracing::span::Id) {}
}

static CAPTURE_EVENTS: OnceLock<std::sync::Arc<Mutex<Vec<CapturedEvent>>>> = OnceLock::new();

/// Installs [`GlobalCapture`] as the process-wide default exactly once (via
/// `OnceLock`) and returns the shared event buffer every call, including the
/// first, can read from.
fn install_capture() -> std::sync::Arc<Mutex<Vec<CapturedEvent>>> {
    CAPTURE_EVENTS
        .get_or_init(|| {
            let events = std::sync::Arc::new(Mutex::new(Vec::new()));
            let subscriber = GlobalCapture {
                events: std::sync::Arc::clone(&events),
            };
            // A second call in the same process never happens (OnceLock runs
            // this closure at most once), but set_global_default's Err is
            // ignored on principle rather than unwrapped.
            let _ = tracing::subscriber::set_global_default(subscriber);
            events
        })
        .clone()
}

fn captured_events(events: &Mutex<Vec<CapturedEvent>>) -> Vec<CapturedEvent> {
    events
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

/// Self-verification of the capture harness against a synthetic event fired
/// from a spawned background `tokio` task, independent of any production
/// code path -- proof that a subscriber installed this way is not
/// thread-scoped, which is the property the real log-line assertions below depend
/// on (production's own confirmed-transition log line will fire from
/// `spawn_status_forwarder`'s own spawned task, not this test's).
#[tokio::test]
async fn capture_harness_observes_an_event_fired_from_a_spawned_background_task() {
    let events = install_capture();
    let marker = format!("t5-self-check-{}-{}", std::process::id(), line!());

    let task_marker = marker.clone();
    tokio::spawn(async move {
        tracing::info!(window_id = %task_marker, status = "idle", "window status confirmed");
    })
    .await
    .expect("the spawned task must not panic");

    let matching: Vec<_> = captured_events(&events)
        .into_iter()
        .filter(|event| {
            event.level == tracing::Level::INFO
                && event.message() == Some("window status confirmed")
                && event.fields.get("window_id").map(String::as_str) == Some(marker.as_str())
        })
        .collect();
    assert_eq!(
        matching.len(),
        1,
        "the harness must observe exactly the one synthetic event fired from a spawned tokio \
         task, proving capture is not thread-scoped, found: {matching:?}"
    );
    assert_eq!(
        matching[0].fields.get("status").map(String::as_str),
        Some("idle")
    );
}

#[tokio::test]
async fn a_confirmed_transition_logs_exactly_one_info_event_naming_the_status_and_reason() {
    let events = install_capture();
    let dir = unique_temp_dir("t5-confirmed-log");
    let config = detection_config(&dir, DaemonConfig::default());
    let h = Harness::start("t5-confirmed-log", config).await;
    let mut client = h.connect().await;

    let window_id = h.open_idle_window(&mut client).await;
    write_window_input(&h.registry, &window_id, SCROLL_CTRL_READY_OUT).await;
    wait_until_status(
        &mut client,
        &window_id,
        WindowStatus::Unknown,
        Duration::from_secs(5),
    )
    .await;

    // The blank screen a window starts on may itself be confirmed as no_rule_matched before
    // the ready marker paints, so the transition count is the daemon's, not an assumed one.
    let transitions = call_list_windows(&mut client)
        .await
        .into_iter()
        .find(|window| window.window_id == window_id)
        .and_then(|window| window.status_rev)
        .expect("the listed window must carry its status_rev");

    let matching: Vec<_> = captured_events(&events)
        .into_iter()
        .filter(|event| {
            event.level == tracing::Level::INFO
                && event.message() == Some("window status confirmed")
                && event.fields.get("window_id").map(String::as_str) == Some(window_id.as_str())
        })
        .collect();

    assert_eq!(
        u64::try_from(matching.len()).unwrap(),
        transitions,
        "exactly one info event per confirmed transition, no more: the daemon counts \
         {transitions} confirmed transitions for this window, so the total must be exactly that \
         -- not 0 (unimplemented), not more (a duplicate), found: {matching:?}"
    );

    let [.., idle_event, unknown_event] = matching.as_slice() else {
        panic!("the window must have confirmed Idle and then Unknown, found: {matching:?}");
    };
    assert_eq!(
        idle_event.fields.get("status").map(String::as_str),
        Some("idle")
    );
    assert!(
        !idle_event.fields.contains_key("unknown_reason"),
        "a non-Unknown confirmation must carry no unknown_reason field at all: {idle_event:?}"
    );

    assert_eq!(
        unknown_event.fields.get("status").map(String::as_str),
        Some("unknown")
    );
    assert_eq!(
        unknown_event
            .fields
            .get("unknown_reason")
            .map(String::as_str),
        Some("no_rule_matched")
    );
}
