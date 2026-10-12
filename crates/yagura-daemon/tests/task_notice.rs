#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: `open_task_window`'s delivery of a task's `instruction`
//! as a one-line `[yagura task]` notice (cases TN-*). Every window is a `submit_child` launched from a launch profile
//! (`open_task_window` never launches anything else), never a real vendor
//! CLI.

mod common;

use std::path::Path;
use std::time::Duration;

use common::{
    ChatPlaybook, Harness, SubmitDefinitionInput, VALID_TOKEN, approver_base_config,
    call_list_windows, chat_config, detection_config, identify, policy, read_response, send,
    submit_child_profile_source_with_detection_and_composer, unique_temp_dir, wait_until_status,
    write_profile, write_submit_definition, write_submit_definition_with_bottom_line_count,
};

use yagura_daemon::DaemonConfig;
use yagura_protocol::{
    ClientMessage, DaemonMessage, InstructionNotice, InstructionNoticeState, PlanInfo,
    PlanTaskSpec, WindowInfo, WindowStatus,
};
use yagura_test_support::ipc::Connection;
use yagura_test_support::process::ready_child_profile_source;

/// The fixed marker every task notice starts with.
const NOTICE_MARKER: &str = "[yagura task]";

/// How long a case waits for a notice, or a `task_window_opened` reply, that
/// must eventually arrive.
const REPLY_TIMEOUT: Duration = Duration::from_secs(15);

/// A readiness timeout far past [`REPLY_TIMEOUT`], for the cases that measure a
/// reply not waiting on the window: a reply that still waited for readiness
/// could arrive only after this, so it would miss [`REPLY_TIMEOUT`].
const READY_TIMEOUT_PAST_REPLY_SECS: u64 = REPLY_TIMEOUT.as_secs() * 4;

/// How long a case watches for something that must never happen (a notice
/// written, a second reply). Long enough to be well past every deadline a
/// case configures below.
const ABSENCE_WINDOW: Duration = Duration::from_secs(3);

/// Detection window wide enough to keep the startup marker line in view while
/// the composer paints a whole notice, which wraps across several rows.
const NOTICE_BOTTOM_LINE_COUNT: u16 = 20;

// --- config ---------------------------------------------------------------

fn ledger_config(
    dir: &Path,
    profiles_dir: std::path::PathBuf,
    detection_dir: &Path,
    handoff_ready_timeout_secs: u64,
) -> DaemonConfig {
    DaemonConfig {
        store_db_path_resolved: Some(dir.join("plan.db")),
        profiles_dir: Some(profiles_dir),
        approval_policy: Some(policy(&["open_window"])),
        handoff_ready_timeout_secs,
        // This file measures the `[yagura task]` notice, not the review
        // gate: the plan created here must not start pending.
        plan_approval_required: false,
        ..detection_config(detection_dir, approver_base_config())
    }
}

/// A `submit_child` profile in composer-draw mode, wired to a real detection
/// definition so it reaches `waiting_input` on its own startup line and
/// `running` once it echoes back what it was given (the
/// `submit_child_profile_source_with_detection_and_composer` template). It is
/// ready again after `common::redraw_screen`.
fn write_ready_submit_profile(
    profiles_dir: &Path,
    detection_dir: &Path,
    profile_id: &str,
    display_name: &str,
    tag: &str,
) {
    write_submit_definition_with_bottom_line_count(
        detection_dir,
        tag,
        &common::ready_pattern_with_redraw(tag),
        SubmitDefinitionInput {
            bracketed_paste: false,
            ready_hold_ms: 0,
            submit_delay_ms: 50,
            clear: "",
            delivery_confirm_ms: 3000,
            resend_max: 0,
        },
        NOTICE_BOTTOM_LINE_COUNT,
    );
    write_profile(
        profiles_dir,
        profile_id,
        &submit_child_profile_source_with_detection_and_composer(
            display_name,
            tag,
            0,
            0,
            0,
            tag,
            0,
        ),
    );
}

/// A `submit_child` profile in composer-draw mode (so its input box paints)
/// whose detection definition exists (a status report needs a live detector,
/// `WindowEntry::record_status_event`), but whose `waiting_input` pattern
/// names a marker the fixture's own runtime marker argument deliberately does
/// not match: the window stays `Unknown` until a case redraws its screen
/// (`common::redraw_screen`), so it is free to report `Running` first without
/// racing a real match.
fn write_never_ready_submit_profile(
    profiles_dir: &Path,
    detection_dir: &Path,
    profile_id: &str,
    display_name: &str,
    tag: &str,
) {
    write_submit_definition(
        detection_dir,
        tag,
        &common::ready_pattern_with_redraw("never-printed-marker-xyz"),
        SubmitDefinitionInput {
            bracketed_paste: false,
            ready_hold_ms: 0,
            submit_delay_ms: 50,
            clear: "",
            delivery_confirm_ms: 3000,
            resend_max: 0,
        },
    );
    write_profile(
        profiles_dir,
        profile_id,
        &submit_child_profile_source_with_detection_and_composer(
            display_name,
            "tn3-actual-marker",
            0,
            0,
            0,
            tag,
            0,
        ),
    );
}

// --- wire round trips -------------------------------------------------------

/// Reads the next *response* frame, skipping `plan_changed` pushes (the same
/// shape `ipc_plan_assign.rs`'s own `read_ledger_reply` uses).
async fn read_ledger_reply(client: &mut Connection, timeout: Duration) -> DaemonMessage {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(!remaining.is_zero(), "timed out waiting for a reply");
        let frame = read_response(client, remaining).await;
        if !matches!(frame, DaemonMessage::PlanChanged { .. }) {
            return frame;
        }
    }
}

fn task(task_id: &str, profile: &str, instruction: Option<&str>) -> PlanTaskSpec {
    PlanTaskSpec {
        task_id: task_id.to_string(),
        title: "do the thing".to_string(),
        role: None,
        profile: Some(profile.to_string()),
        model: None,
        effort: None,
        depends_on: Vec::new(),
        writes: vec![format!("{task_id}.txt")],
        instruction: instruction.map(str::to_string),
    }
}

/// Opens a plain `echo_child` window and identifies `client` as it, so the
/// connection becomes a plan owner (`create_plan` binds the requesting
/// window as `owner_window`, and the ledger refuses an unidentified
/// connection outright).
async fn open_and_identify(h: &Harness, client: &mut Connection) -> String {
    let window_id = h
        .open(yagura_test_support::process::echo_child_command(), None)
        .await;
    identify(client, &window_id).await;
    window_id
}

async fn create_ok(client: &mut Connection, title: &str, tasks: Vec<PlanTaskSpec>) -> PlanInfo {
    send(
        client,
        &ClientMessage::CreatePlan {
            title: title.to_string(),
            tasks,
        },
    )
    .await;
    match read_ledger_reply(client, Duration::from_secs(5)).await {
        DaemonMessage::PlanDetail { plan } => plan,
        other => panic!("expected plan_detail, got {other:?}"),
    }
}

async fn get_plan(client: &mut Connection, plan_id: &str) -> PlanInfo {
    send(
        client,
        &ClientMessage::GetPlan {
            plan_id: plan_id.to_string(),
        },
    )
    .await;
    match read_ledger_reply(client, Duration::from_secs(5)).await {
        DaemonMessage::PlanDetail { plan } => plan,
        other => panic!("expected plan_detail, got {other:?}"),
    }
}

async fn send_open_task_window(client: &mut Connection, plan_id: &str, task_id: &str, name: &str) {
    send(
        client,
        &ClientMessage::OpenTaskWindow {
            plan_id: plan_id.to_string(),
            task_id: task_id.to_string(),
            name: Some(name.to_string()),
        },
    )
    .await;
}

struct TaskWindowOpenedFields {
    window_id: String,
}

fn expect_task_window_opened(msg: DaemonMessage) -> TaskWindowOpenedFields {
    match msg {
        DaemonMessage::TaskWindowOpened { window_id, .. } => TaskWindowOpenedFields { window_id },
        other => panic!("expected task_window_opened, got {other:?}"),
    }
}

async fn wait_for_window_named(
    probe: &mut Connection,
    name: &str,
    timeout: Duration,
) -> WindowInfo {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let windows = call_list_windows(probe).await;
        if let Some(found) = windows.into_iter().find(|w| w.name == name) {
            return found;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for a window named {name:?} to appear"
        );
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
}

/// Sends a hook report for `window_id` and waits until the daemon reports
/// that status (the same technique `ipc_report_notice.rs`'s `pin_status`
/// uses).
async fn pin_status(probe: &mut Connection, window_id: &str, status: WindowStatus) {
    send(
        probe,
        &ClientMessage::StatusEvent {
            source: "task-notice-test".to_string(),
            status,
            window_id: Some(window_id.to_string()),
            token: VALID_TOKEN.to_string(),
        },
    )
    .await;
    wait_until_status(probe, window_id, status, Duration::from_secs(10)).await;
}

// --- reading what reached a window's composer -------------------------------

/// `window_id`'s scrollback with escape sequences and line breaks removed
/// (the same flattening `ipc_report_notice.rs` uses), so a wrapped or
/// redrawn line still reads as one run of text.
fn flattened_scrollback(h: &Harness, window_id: &str) -> String {
    let raw = String::from_utf8_lossy(&h.scrollback(window_id)).into_owned();
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => match chars.next() {
                Some('[') => {
                    for c in chars.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&c) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    while let Some(c) = chars.next() {
                        if c == '\u{7}' {
                            break;
                        }
                        if c == '\u{1b}' && chars.peek() == Some(&'\\') {
                            chars.next();
                            break;
                        }
                    }
                }
                _ => {}
            },
            '\r' | '\n' => {}
            other => out.push(other),
        }
    }
    out
}

fn tail(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let start = chars.len().saturating_sub(600);
    chars[start..].iter().collect()
}

/// The exact notice text, built from the fixed
/// wording rather than taken from the product, so the product cannot move
/// the expectation along with itself.
fn expected_notice(plan_id: &str, task_id: &str) -> String {
    format!(
        "{NOTICE_MARKER} Task {task_id} of plan {plan_id} is assigned to this window. \
         Call get_plan with plan_id \"{plan_id}\" and task_id \"{task_id}\" to read its \
         instruction."
    )
}

async fn wait_for_submitted(h: &Harness, window_id: &str, text: &str, timeout: Duration) {
    let deadline = tokio::time::Instant::now() + timeout;
    let needle = format!("SUBMITTED {text}");
    loop {
        if flattened_scrollback(h, window_id).contains(&needle) {
            return;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!(
                "timed out waiting for {window_id} to accept {text:?}; flattened scrollback \
                 tail: {:?}",
                tail(&flattened_scrollback(h, window_id)),
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Times `window_id` accepted exactly `text` as a submitted line.
fn submitted_count(h: &Harness, window_id: &str, text: &str) -> usize {
    flattened_scrollback(h, window_id)
        .matches(&format!("SUBMITTED {text}"))
        .count()
}

/// The stored delivery state of `task_id`'s instruction notice, as `get_plan`
/// reports it.
async fn instruction_notice(
    owner: &mut Connection,
    plan_id: &str,
    task_id: &str,
) -> Option<InstructionNotice> {
    get_plan(owner, plan_id)
        .await
        .tasks
        .into_iter()
        .find(|t| t.task_id == task_id)
        .unwrap_or_else(|| panic!("task {task_id} must be in plan {plan_id}"))
        .instruction_notice
}

/// Polls `get_plan` until `task_id`'s notice reaches `state`, returning it.
async fn wait_for_notice_state(
    owner: &mut Connection,
    plan_id: &str,
    task_id: &str,
    state: InstructionNoticeState,
) -> InstructionNotice {
    let deadline = tokio::time::Instant::now() + REPLY_TIMEOUT;
    loop {
        let notice = instruction_notice(owner, plan_id, task_id).await;
        if let Some(notice) = notice
            && notice.state == state
        {
            return notice;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for task {task_id}'s instruction_notice to become {state:?}; \
             last seen: {notice:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn assert_notice_never_written(h: &Harness, window_id: &str, wait: Duration) {
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        let flattened = flattened_scrollback(h, window_id);
        assert!(
            !flattened.contains(NOTICE_MARKER),
            "a task notice was written even though none was expected: {:?}",
            tail(&flattened),
        );
        if tokio::time::Instant::now() >= deadline {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

// --- No instruction -> immediate reply, nothing written --------------

#[tokio::test]
async fn a_task_with_no_instruction_opens_immediately_with_no_delivery_and_writes_nothing() {
    let dir = unique_temp_dir("tn1-no-instruction");
    let profiles_dir = dir.join("profiles");
    let detection_dir = dir.join("detection");
    std::fs::create_dir_all(&profiles_dir).expect("create profiles dir");
    std::fs::create_dir_all(&detection_dir).expect("create detection dir");
    write_profile(
        &profiles_dir,
        "tn1-profile",
        &ready_child_profile_source("TN1"),
    );

    let (h, _locks) = Harness::start_with_ledger(
        "tn1-no-instruction",
        ledger_config(
            &dir,
            profiles_dir,
            &detection_dir,
            READY_TIMEOUT_PAST_REPLY_SECS,
        ),
    )
    .await;
    let mut owner = h.connect().await;
    open_and_identify(&h, &mut owner).await;
    let plan = create_ok(&mut owner, "plan", vec![task("t1", "tn1-profile", None)]).await;

    // The window never reaches readiness (no detection), so a reply that waited
    // on it would miss `REPLY_TIMEOUT`.
    send_open_task_window(&mut owner, &plan.plan_id, "t1", "tn1-window").await;
    let opened = expect_task_window_opened(read_ledger_reply(&mut owner, REPLY_TIMEOUT).await);

    assert_notice_never_written(&h, &opened.window_id, ABSENCE_WINDOW).await;
    h.registry.terminate_all().await;
}

// --- An instruction is delivered as a notice, confirmed -------

#[tokio::test]
async fn a_task_with_an_instruction_is_announced_by_the_fixed_notice_line_never_the_body() {
    let dir = unique_temp_dir("tn2-confirmed");
    let profiles_dir = dir.join("profiles");
    let detection_dir = dir.join("detection");
    std::fs::create_dir_all(&profiles_dir).expect("create profiles dir");
    std::fs::create_dir_all(&detection_dir).expect("create detection dir");
    write_ready_submit_profile(
        &profiles_dir,
        &detection_dir,
        "tn2-profile",
        "TN2",
        "tn2-tag",
    );

    let (h, _locks) = Harness::start_with_ledger(
        "tn2-confirmed",
        ledger_config(&dir, profiles_dir, &detection_dir, 10),
    )
    .await;
    let mut owner = h.connect().await;
    open_and_identify(&h, &mut owner).await;
    let secret_token = "TN2INSTRUCTIONTOKEN";
    let instruction = format!("do the special thing involving {secret_token}");
    let plan = create_ok(
        &mut owner,
        "plan",
        vec![task("t1", "tn2-profile", Some(&instruction))],
    )
    .await;

    send_open_task_window(&mut owner, &plan.plan_id, "t1", "tn2-window").await;
    let opened = expect_task_window_opened(read_ledger_reply(&mut owner, REPLY_TIMEOUT).await);

    let expected = expected_notice(&plan.plan_id, "t1");
    wait_for_submitted(&h, &opened.window_id, &expected, REPLY_TIMEOUT).await;

    let flattened = flattened_scrollback(&h, &opened.window_id);
    assert!(
        !flattened.contains(secret_token),
        "the instruction's own body must never reach the window: {:?}",
        tail(&flattened),
    );

    h.registry.terminate_all().await;
}

// --- Readiness never reached -> not_written, window stays bound

#[tokio::test]
async fn a_task_whose_window_never_becomes_ready_is_not_written_and_stays_bound() {
    let dir = unique_temp_dir("tn3-not-ready");
    let profiles_dir = dir.join("profiles");
    let detection_dir = dir.join("detection");
    std::fs::create_dir_all(&profiles_dir).expect("create profiles dir");
    std::fs::create_dir_all(&detection_dir).expect("create detection dir");
    write_never_ready_submit_profile(
        &profiles_dir,
        &detection_dir,
        "tn3-profile",
        "TN3",
        "tn3-tag",
    );

    let (h, _locks) = Harness::start_with_ledger(
        "tn3-not-ready",
        ledger_config(&dir, profiles_dir, &detection_dir, 2),
    )
    .await;
    let mut owner = h.connect().await;
    open_and_identify(&h, &mut owner).await;
    let mut probe = h.connect().await;
    let instruction = "do the special thing";
    let plan = create_ok(
        &mut owner,
        "plan",
        vec![task("t1", "tn3-profile", Some(instruction))],
    )
    .await;

    send_open_task_window(&mut owner, &plan.plan_id, "t1", "tn3-window").await;
    let reply = expect_task_window_opened(read_ledger_reply(&mut owner, REPLY_TIMEOUT).await);
    let opened = wait_for_window_named(&mut probe, "tn3-window", Duration::from_secs(5)).await;
    assert_eq!(reply.window_id, opened.window_id);
    // An open turn: a status that never enters WaitingInput/Idle, with the
    // composer still painted (composer-draw mode): Running, not Unknown.
    pin_status(&mut probe, &opened.window_id, WindowStatus::Running).await;

    assert_notice_never_written(&h, &opened.window_id, ABSENCE_WINDOW).await;
    assert_eq!(
        instruction_notice(&mut owner, &plan.plan_id, "t1").await,
        Some(InstructionNotice {
            state: InstructionNoticeState::Pending,
            at: None,
        }),
        "a notice never written stays pending"
    );

    let detail = get_plan(&mut owner, &plan.plan_id).await;
    let bound = detail
        .tasks
        .iter()
        .find(|t| t.task_id == "t1")
        .and_then(|t| t.window_id.clone());
    assert_eq!(
        bound.as_deref(),
        Some(opened.window_id.as_str()),
        "the window must stay bound to the task even though its notice went unwritten"
    );
    let windows = call_list_windows(&mut probe).await;
    assert!(
        windows.iter().any(|w| w.window_id == opened.window_id),
        "the window must not have been closed after an undelivered notice"
    );

    h.registry.terminate_all().await;
}

// --- Delivery is decoupled from the connection's frame loop ----------

#[tokio::test]
async fn opening_a_task_window_with_an_instruction_does_not_block_other_requests_on_the_same_connection()
 {
    let dir = unique_temp_dir("tn4-decoupled");
    let profiles_dir = dir.join("profiles");
    let detection_dir = dir.join("detection");
    std::fs::create_dir_all(&profiles_dir).expect("create profiles dir");
    std::fs::create_dir_all(&detection_dir).expect("create detection dir");
    write_ready_submit_profile(
        &profiles_dir,
        &detection_dir,
        "tn4-profile",
        "TN4",
        "tn4-tag",
    );

    let (h, _locks) = Harness::start_with_ledger(
        "tn4-decoupled",
        ledger_config(&dir, profiles_dir, &detection_dir, 10),
    )
    .await;
    let mut owner = h.connect().await;
    open_and_identify(&h, &mut owner).await;
    let instruction = "do the special thing";
    let plan = create_ok(
        &mut owner,
        "plan",
        vec![task("t1", "tn4-profile", Some(instruction))],
    )
    .await;

    send_open_task_window(&mut owner, &plan.plan_id, "t1", "tn4-window").await;
    send(&mut owner, &ClientMessage::ListWindows).await;

    // Either order is fine: the open reply no longer waits on the delivery,
    // so it may come first; what must not happen is either reply going missing.
    let mut window_list = false;
    let mut task_window_opened = false;
    for _ in 0..2 {
        match read_ledger_reply(&mut owner, REPLY_TIMEOUT).await {
            DaemonMessage::WindowList { .. } if !window_list => window_list = true,
            DaemonMessage::TaskWindowOpened { .. } if !task_window_opened => {
                task_window_opened = true;
            }
            other => panic!("expected one window_list and one task_window_opened, got {other:?}"),
        }
    }

    h.registry.terminate_all().await;
}

// --- The reply never waits for the window to accept input -------------

#[tokio::test]
async fn opening_a_task_window_with_an_instruction_replies_without_waiting_for_the_window() {
    let dir = unique_temp_dir("nq1-immediate-reply");
    let profiles_dir = dir.join("profiles");
    let detection_dir = dir.join("detection");
    std::fs::create_dir_all(&profiles_dir).expect("create profiles dir");
    std::fs::create_dir_all(&detection_dir).expect("create detection dir");
    write_never_ready_submit_profile(
        &profiles_dir,
        &detection_dir,
        "nq1-profile",
        "NQ1",
        "nq1-tag",
    );

    let (h, _locks) = Harness::start_with_ledger(
        "nq1-immediate-reply",
        ledger_config(
            &dir,
            profiles_dir,
            &detection_dir,
            READY_TIMEOUT_PAST_REPLY_SECS,
        ),
    )
    .await;
    let mut owner = h.connect().await;
    open_and_identify(&h, &mut owner).await;
    let plan = create_ok(
        &mut owner,
        "plan",
        vec![task("t1", "nq1-profile", Some("do the special thing"))],
    )
    .await;

    // The window never becomes ready, so a reply that waited for it to accept
    // input would miss `REPLY_TIMEOUT`.
    send_open_task_window(&mut owner, &plan.plan_id, "t1", "nq1-window").await;
    expect_task_window_opened(read_ledger_reply(&mut owner, REPLY_TIMEOUT).await);
    h.registry.terminate_all().await;
}

// --- The notice's delivery state in get_plan ---------------------------

#[tokio::test]
async fn a_task_notice_is_pending_until_written_then_delivered_with_a_time() {
    let dir = unique_temp_dir("nq2-delivered");
    let profiles_dir = dir.join("profiles");
    let detection_dir = dir.join("detection");
    std::fs::create_dir_all(&profiles_dir).expect("create profiles dir");
    std::fs::create_dir_all(&detection_dir).expect("create detection dir");
    write_never_ready_submit_profile(
        &profiles_dir,
        &detection_dir,
        "nq2-profile",
        "NQ2",
        "nq2-tag",
    );

    let (h, _locks) = Harness::start_with_ledger(
        "nq2-delivered",
        ledger_config(&dir, profiles_dir, &detection_dir, 2),
    )
    .await;
    let mut owner = h.connect().await;
    open_and_identify(&h, &mut owner).await;
    let mut probe = h.connect().await;
    let plan = create_ok(
        &mut owner,
        "plan",
        vec![task("t1", "nq2-profile", Some("do the special thing"))],
    )
    .await;

    send_open_task_window(&mut owner, &plan.plan_id, "t1", "nq2-window").await;
    let opened = expect_task_window_opened(read_ledger_reply(&mut owner, REPLY_TIMEOUT).await);
    pin_status(&mut probe, &opened.window_id, WindowStatus::Running).await;
    assert_notice_never_written(&h, &opened.window_id, ABSENCE_WINDOW).await;
    assert_eq!(
        instruction_notice(&mut owner, &plan.plan_id, "t1").await,
        Some(InstructionNotice {
            state: InstructionNoticeState::Pending,
            at: None,
        }),
        "before the notice is written its state is pending"
    );

    // The turn ends: the prompt is redrawn and the hook reports it over.
    common::redraw_screen(&h, &opened.window_id, "tn3-actual-marker").await;
    pin_status(&mut probe, &opened.window_id, WindowStatus::WaitingInput).await;
    wait_for_submitted(
        &h,
        &opened.window_id,
        &expected_notice(&plan.plan_id, "t1"),
        REPLY_TIMEOUT,
    )
    .await;
    let notice = wait_for_notice_state(
        &mut owner,
        &plan.plan_id,
        "t1",
        InstructionNoticeState::Delivered,
    )
    .await;
    assert!(
        notice.at.is_some(),
        "a delivered notice carries the time it was written: {notice:?}"
    );
    h.registry.terminate_all().await;
}

#[tokio::test]
async fn a_task_notice_whose_window_closes_before_it_is_written_becomes_window_closed() {
    let dir = unique_temp_dir("nq2-window-closed");
    let profiles_dir = dir.join("profiles");
    let detection_dir = dir.join("detection");
    std::fs::create_dir_all(&profiles_dir).expect("create profiles dir");
    std::fs::create_dir_all(&detection_dir).expect("create detection dir");
    write_never_ready_submit_profile(
        &profiles_dir,
        &detection_dir,
        "nq2c-profile",
        "NQ2C",
        "nq2c-tag",
    );

    let config = DaemonConfig {
        approval_policy: Some(policy(&["close_window", "open_window"])),
        ..ledger_config(&dir, profiles_dir, &detection_dir, 2)
    };
    let (h, _locks) = Harness::start_with_ledger("nq2-window-closed", config).await;
    let mut owner = h.connect().await;
    open_and_identify(&h, &mut owner).await;
    let mut probe = h.connect().await;
    let plan = create_ok(
        &mut owner,
        "plan",
        vec![task("t1", "nq2c-profile", Some("do the special thing"))],
    )
    .await;

    send_open_task_window(&mut owner, &plan.plan_id, "t1", "nq2c-window").await;
    let opened = expect_task_window_opened(read_ledger_reply(&mut owner, REPLY_TIMEOUT).await);
    pin_status(&mut probe, &opened.window_id, WindowStatus::Running).await;
    assert_eq!(
        instruction_notice(&mut owner, &plan.plan_id, "t1")
            .await
            .map(|notice| notice.state),
        Some(InstructionNoticeState::Pending),
    );

    send(
        &mut probe,
        &ClientMessage::CloseWindow {
            window_id: opened.window_id.clone(),
            force: Some(true),
        },
    )
    .await;
    match read_response(&mut probe, Duration::from_secs(10)).await {
        DaemonMessage::WindowClosed { .. } => {}
        other => panic!("expected window_closed, got {other:?}"),
    }

    let notice = wait_for_notice_state(
        &mut owner,
        &plan.plan_id,
        "t1",
        InstructionNoticeState::WindowClosed,
    )
    .await;
    assert!(
        notice.at.is_some(),
        "window_closed carries the time the window closed: {notice:?}"
    );
    h.registry.terminate_all().await;
}

// --- The intro and the task notice on one window --------------

const NQ_INTRO: &str = "welcome to the room";

#[tokio::test]
async fn a_task_notice_queued_behind_the_intro_is_written_once_the_window_accepts_input_again() {
    let dir = unique_temp_dir("nqr1-intro-then-task");
    let profiles_dir = dir.join("profiles");
    std::fs::create_dir_all(&profiles_dir).expect("create profiles dir");
    // `chat_config` points the detection directory at `dir` itself.
    write_ready_submit_profile(&profiles_dir, &dir, "nqr1-profile", "NQR1", "nqr1-tag");
    let mut config = chat_config(
        &dir,
        &ChatPlaybook {
            intro: Some(NQ_INTRO),
            ..ChatPlaybook::default()
        },
    );
    config.profiles_dir = Some(profiles_dir);
    config.handoff_ready_timeout_secs = 10;
    // This file measures the `[yagura task]` notice, not the review gate:
    // the plan created here must not start pending.
    config.plan_approval_required = false;

    let (h, _locks) = Harness::start_with_chat_and_ledger("nqr1-intro-then-task", config).await;
    let mut owner = h.connect().await;
    open_and_identify(&h, &mut owner).await;
    let mut probe = h.connect().await;
    let plan = create_ok(
        &mut owner,
        "plan",
        vec![task("t1", "nqr1-profile", Some("do the special thing"))],
    )
    .await;

    send_open_task_window(&mut owner, &plan.plan_id, "t1", "nqr1-window").await;
    let opened = expect_task_window_opened(read_ledger_reply(&mut owner, REPLY_TIMEOUT).await);
    let window_id = opened.window_id;
    let task_line = expected_notice(&plan.plan_id, "t1");

    // The intro is written first; the child echoes it, which its running rule
    // matches, so the window stops accepting input.
    wait_for_submitted(&h, &window_id, NQ_INTRO, REPLY_TIMEOUT).await;
    wait_until_status(
        &mut probe,
        &window_id,
        WindowStatus::Running,
        Duration::from_secs(10),
    )
    .await;

    // The prompt is redrawn, so the window accepts input again and takes the
    // queued task notice.
    common::redraw_screen(&h, &window_id, "nqr1-tag").await;
    wait_for_submitted(&h, &window_id, &task_line, REPLY_TIMEOUT).await;

    let flattened = flattened_scrollback(&h, &window_id);
    let intro_at = flattened
        .find(&format!("SUBMITTED {NQ_INTRO}"))
        .expect("the intro was observed above");
    let task_at = flattened
        .find(&format!("SUBMITTED {task_line}"))
        .expect("the task notice was observed above");
    assert!(
        intro_at < task_at,
        "the intro must be written before the task notice: {:?}",
        tail(&flattened),
    );

    // Further ready stretches never write either notice again.
    for _ in 0..2 {
        pin_status(&mut probe, &window_id, WindowStatus::Running).await;
        common::redraw_screen(&h, &window_id, "nqr1-tag").await;
        pin_status(&mut probe, &window_id, WindowStatus::WaitingInput).await;
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let deadline = tokio::time::Instant::now() + ABSENCE_WINDOW;
    loop {
        assert_eq!(submitted_count(&h, &window_id, NQ_INTRO), 1, "intro count");
        assert_eq!(
            submitted_count(&h, &window_id, &task_line),
            1,
            "task notice count"
        );
        if tokio::time::Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    h.registry.terminate_all().await;
}

// --- A task notice declined by foreign input is kept, not dropped ----

/// Text standing in the task window's input box that nobody from the daemon wrote.
const FOREIGN_DRAFT: &str = "NQFOREIGNDRAFT";

#[tokio::test]
async fn a_task_notice_declined_by_foreign_text_in_the_input_is_written_once_the_input_clears() {
    let dir = unique_temp_dir("nqr3-foreign-then-clear");
    let profiles_dir = dir.join("profiles");
    let detection_dir = dir.join("detection");
    std::fs::create_dir_all(&profiles_dir).expect("create profiles dir");
    std::fs::create_dir_all(&detection_dir).expect("create detection dir");
    write_never_ready_submit_profile(
        &profiles_dir,
        &detection_dir,
        "nqr3-profile",
        "NQR3",
        "nqr3-tag",
    );

    let (h, _locks) = Harness::start_with_ledger(
        "nqr3-foreign-then-clear",
        ledger_config(&dir, profiles_dir, &detection_dir, 2),
    )
    .await;
    let mut owner = h.connect().await;
    open_and_identify(&h, &mut owner).await;
    let mut probe = h.connect().await;
    let plan = create_ok(
        &mut owner,
        "plan",
        vec![task("t1", "nqr3-profile", Some("do the special thing"))],
    )
    .await;

    send_open_task_window(&mut owner, &plan.plan_id, "t1", "nqr3-window").await;
    let opened = expect_task_window_opened(read_ledger_reply(&mut owner, REPLY_TIMEOUT).await);
    let window_id = opened.window_id;
    pin_status(&mut probe, &window_id, WindowStatus::Running).await;
    // The prompt is redrawn before the draft is typed: the byte that redraws it
    // would otherwise clear the draft. The open turn keeps the window busy.
    common::redraw_screen(&h, &window_id, "tn3-actual-marker").await;

    h.write_input(&window_id, FOREIGN_DRAFT.as_bytes()).await;
    let deadline = tokio::time::Instant::now() + REPLY_TIMEOUT;
    while !flattened_scrollback(&h, &window_id).contains(FOREIGN_DRAFT) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the foreign draft never painted in the task window's input"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // The window accepts input, but the notice must be declined rather than
    // typed over the foreign draft, and it must stay pending.
    pin_status(&mut probe, &window_id, WindowStatus::WaitingInput).await;
    assert_notice_never_written(&h, &window_id, ABSENCE_WINDOW).await;
    let entry = h.registry.get(&window_id).expect("task window entry");
    assert!(
        entry.notice_attempts() >= 1,
        "the notice must have been tried against the foreign draft, not merely left untried"
    );
    assert_eq!(
        instruction_notice(&mut owner, &plan.plan_id, "t1").await,
        Some(InstructionNotice {
            state: InstructionNoticeState::Pending,
            at: None,
        }),
        "a declined notice stays pending"
    );

    h.write_input(&window_id, b"\x15").await;
    wait_for_submitted(
        &h,
        &window_id,
        &expected_notice(&plan.plan_id, "t1"),
        REPLY_TIMEOUT,
    )
    .await;
    let notice = wait_for_notice_state(
        &mut owner,
        &plan.plan_id,
        "t1",
        InstructionNoticeState::Delivered,
    )
    .await;
    assert!(
        notice.at.is_some(),
        "a delivered notice carries the time it was written: {notice:?}"
    );
    h.registry.terminate_all().await;
}
