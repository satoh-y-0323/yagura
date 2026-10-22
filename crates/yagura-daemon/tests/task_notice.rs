#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: `open_task_window` hands a task's `instruction` over as a one-line
//! `[yagura task]` notice, the launch prompt the window's profile declares (cases TN-*).
//! The notice reaches the CLI as the request it starts with: nothing is typed into the
//! window, and the notice is recorded delivered once the window is open. Every window is
//! a `hook_child` launched from a profile, never a real vendor CLI; it logs every argument
//! it was started with and every byte typed into it.

mod common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use common::{
    CONTROL_INPUT_TABLE, Harness, approver_base_config, call_list_windows, composer_section_toml,
    detection_config, identify, policy, read_response, send, unique_temp_dir, wait_until_status,
    write_profile,
};

use yagura_daemon::DaemonConfig;
use yagura_protocol::{
    ClientMessage, DaemonMessage, InstructionNotice, InstructionNoticeState, PlanInfo,
    PlanTaskSpec, WindowInfo, WindowStatus,
};
use yagura_test_support::ipc::Connection;
use yagura_test_support::process::{
    COMPOSER_CONTINUATION_PREFIX, COMPOSER_MARKER, COMPOSER_SEPARATOR,
};

/// The fixed marker every task notice starts with.
const NOTICE_MARKER: &str = "[yagura task]";

/// How long a case waits for a reply, or for the window to log its start, that must
/// eventually arrive.
const REPLY_TIMEOUT: Duration = Duration::from_secs(15);

/// A detection definition the window reaches `waiting_input` under.
const READY_DETECTION: &str = "tn-ready";

/// A detection definition no screen ever matches: the window stays `Unknown`.
const NEVER_READY_DETECTION: &str = "tn-never-ready";

// --- config ---------------------------------------------------------------

fn ledger_config(dir: &Path, profiles_dir: PathBuf, detection_dir: &Path) -> DaemonConfig {
    DaemonConfig {
        store_db_path_resolved: Some(dir.join("plan.db")),
        profiles_dir: Some(profiles_dir),
        approval_policy: Some(policy(&["open_window"])),
        // This file measures the `[yagura task]` notice, not the review
        // gate: the plan created here must not start pending.
        plan_approval_required: false,
        ..detection_config(detection_dir, approver_base_config())
    }
}

fn write_definitions(detection_dir: &Path) {
    let tail = format!("{CONTROL_INPUT_TABLE}{}", composer_section_toml());
    std::fs::write(
        detection_dir.join(format!("{READY_DETECTION}.toml")),
        format!(
            "schema_version = 1\nname = \"{READY_DETECTION}\"\nbottom_line_count = 40\n\n\
             [debounce]\nconfirm_ms = 30\n\n[[rules]]\nstatus = \"waiting_input\"\n\
             pattern = 'HC-IDLE'\npriority = 10\nregion = \"bottom\"\n{tail}"
        ),
    )
    .expect("write the ready definition");
    std::fs::write(
        detection_dir.join(format!("{NEVER_READY_DETECTION}.toml")),
        format!(
            "schema_version = 1\nname = \"{NEVER_READY_DETECTION}\"\n\n[debounce]\n\
             confirm_ms = 30\n\n[[rules]]\nstatus = \"waiting_input\"\n\
             pattern = 'NEVER-PRINTED-XYZ'\npriority = 10\nregion = \"bottom\"\n{tail}"
        ),
    )
    .expect("write the never-ready definition");
}

/// A `hook_child` profile under `detection`, whose `[delivery]` table hands a first
/// request over as `--prompt <request>`. Returns the directory the window logs to.
fn write_task_profile(
    profiles_dir: &Path,
    logs_root: &Path,
    profile_id: &str,
    detection: &str,
) -> PathBuf {
    let logs = logs_root.join(profile_id);
    std::fs::create_dir_all(&logs).expect("create the log directory");
    let command = [
        PathBuf::from(env!("CARGO_BIN_EXE_hook_child"))
            .display()
            .to_string(),
        COMPOSER_MARKER.to_string(),
        COMPOSER_SEPARATOR.to_string(),
        COMPOSER_CONTINUATION_PREFIX.to_string(),
        "0".to_string(),
        "-".to_string(),
        "--log".to_string(),
        logs.display().to_string(),
    ]
    .iter()
    .map(|arg| format!("'{arg}'"))
    .collect::<Vec<_>>()
    .join(", ");
    write_profile(
        profiles_dir,
        profile_id,
        &format!(
            "schema = 1\ndisplay_name = \"{profile_id}\"\ndetection = \"{detection}\"\n\n\
             [command]\ndefault = [{command}]\n\n[delivery]\nwake = \"none\"\n\
             launch_prompt = [\"--prompt\", \"{{prompt}}\"]\n"
        ),
    );
    logs
}

struct Scene {
    h: Harness,
    owner: Connection,
    logs: PathBuf,
}

async fn scene(tag: &str, detection: &str) -> Scene {
    let dir = unique_temp_dir(tag);
    let profiles_dir = dir.join("profiles");
    let detection_dir = dir.join("detection");
    std::fs::create_dir_all(&profiles_dir).expect("create profiles dir");
    std::fs::create_dir_all(&detection_dir).expect("create detection dir");
    write_definitions(&detection_dir);
    let logs = write_task_profile(&profiles_dir, &dir.join("logs"), "tn-profile", detection);
    let (h, _locks) =
        Harness::start_with_ledger(tag, ledger_config(&dir, profiles_dir, &detection_dir)).await;
    let mut owner = h.connect().await;
    open_and_identify(&h, &mut owner).await;
    Scene { h, owner, logs }
}

// --- what the window was started with ---------------------------------------

/// The window's logged start: its argv and the prompt it was handed, once logged.
async fn started_with(logs: &Path) -> (Vec<String>, Option<String>) {
    let deadline = tokio::time::Instant::now() + REPLY_TIMEOUT;
    loop {
        let start = std::fs::read_to_string(logs.join("events.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .find(|line| line["event"] == "argv");
        if let Some(start) = start {
            let argv = start["args"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter_map(|arg| arg.as_str().map(str::to_string))
                .collect();
            return (argv, start["prompt"].as_str().map(str::to_string));
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for the window to log its start"
        );
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
}

fn typed(logs: &Path) -> String {
    String::from_utf8_lossy(&std::fs::read(logs.join("input.log")).unwrap_or_default()).into_owned()
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

fn expect_task_window_opened(msg: DaemonMessage) -> String {
    match msg {
        DaemonMessage::TaskWindowOpened { window_id, .. } => window_id,
        other => panic!("expected task_window_opened, got {other:?}"),
    }
}

async fn window_named(probe: &mut Connection, name: &str) -> Option<WindowInfo> {
    call_list_windows(probe)
        .await
        .into_iter()
        .find(|w| w.name == name)
}

/// The exact notice text, built from the fixed wording rather than taken from the
/// product, so the product cannot move the expectation along with itself.
fn expected_notice(plan_id: &str, task_id: &str) -> String {
    format!(
        "{NOTICE_MARKER} Task {task_id} of plan {plan_id} is assigned to this window. \
         Call get_plan with plan_id \"{plan_id}\" and task_id \"{task_id}\" to read its \
         instruction."
    )
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

// --- No instruction -> no launch prompt, nothing written --------------------

// A task with nothing to say is started with nothing.
#[tokio::test]
async fn a_task_with_no_instruction_is_started_with_no_launch_prompt_and_nothing_typed() {
    let Scene { h, mut owner, logs } = scene("tn1-no-instruction", READY_DETECTION).await;
    let plan = create_ok(&mut owner, "plan", vec![task("t1", "tn-profile", None)]).await;

    send_open_task_window(&mut owner, &plan.plan_id, "t1", "tn1-window").await;
    let window_id = expect_task_window_opened(read_ledger_reply(&mut owner, REPLY_TIMEOUT).await);

    let (argv, prompt) = started_with(&logs).await;
    assert_eq!(prompt, None, "{argv:?}");
    assert!(!argv.iter().any(|arg| arg == "--prompt"), "{argv:?}");
    wait_until_status(
        &mut owner,
        &window_id,
        WindowStatus::WaitingInput,
        REPLY_TIMEOUT,
    )
    .await;
    assert!(!typed(&logs).contains(NOTICE_MARKER));
    h.registry.terminate_all().await;
}

// --- An instruction is handed over as the fixed notice, at launch -----------

#[tokio::test]
async fn a_task_with_an_instruction_is_started_with_the_fixed_notice_never_the_body() {
    let Scene { h, mut owner, logs } = scene("tn2-launch-prompt", READY_DETECTION).await;
    let secret_token = "TN2INSTRUCTIONTOKEN";
    let instruction = format!("do the special thing involving {secret_token}");
    let plan = create_ok(
        &mut owner,
        "plan",
        vec![task("t1", "tn-profile", Some(&instruction))],
    )
    .await;

    send_open_task_window(&mut owner, &plan.plan_id, "t1", "tn2-window").await;
    let window_id = expect_task_window_opened(read_ledger_reply(&mut owner, REPLY_TIMEOUT).await);

    let (argv, prompt) = started_with(&logs).await;
    assert_eq!(
        prompt.as_deref(),
        Some(expected_notice(&plan.plan_id, "t1").as_str())
    );
    assert!(
        !argv.iter().any(|arg| arg.contains(secret_token)),
        "the instruction's own body must never reach the window: {argv:?}"
    );
    wait_until_status(
        &mut owner,
        &window_id,
        WindowStatus::WaitingInput,
        REPLY_TIMEOUT,
    )
    .await;
    assert!(
        !typed(&logs).contains(NOTICE_MARKER),
        "nothing is typed into a window started with its notice: {:?}",
        typed(&logs)
    );
    h.registry.terminate_all().await;
}

// --- A window that never becomes ready still starts with its notice ---------

#[tokio::test]
async fn a_task_whose_window_never_becomes_ready_still_starts_with_its_notice_and_stays_bound() {
    let Scene { h, mut owner, logs } = scene("tn3-not-ready", NEVER_READY_DETECTION).await;
    let mut probe = h.connect().await;
    let plan = create_ok(
        &mut owner,
        "plan",
        vec![task("t1", "tn-profile", Some("do the special thing"))],
    )
    .await;

    send_open_task_window(&mut owner, &plan.plan_id, "t1", "tn3-window").await;
    let window_id = expect_task_window_opened(read_ledger_reply(&mut owner, REPLY_TIMEOUT).await);

    let (_, prompt) = started_with(&logs).await;
    assert_eq!(
        prompt.as_deref(),
        Some(expected_notice(&plan.plan_id, "t1").as_str()),
        "the notice does not wait for the window to accept input"
    );
    assert_eq!(
        instruction_notice(&mut owner, &plan.plan_id, "t1")
            .await
            .map(|notice| notice.state),
        Some(InstructionNoticeState::Delivered)
    );
    let detail = get_plan(&mut owner, &plan.plan_id).await;
    let bound = detail
        .tasks
        .iter()
        .find(|t| t.task_id == "t1")
        .and_then(|t| t.window_id.clone());
    assert_eq!(bound.as_deref(), Some(window_id.as_str()));
    assert!(
        window_named(&mut probe, "tn3-window").await.is_some(),
        "the window is not closed for never becoming ready"
    );
    h.registry.terminate_all().await;
}

// --- The open is decoupled from the connection's frame loop -----------------

// The reply does not wait for the notice.
#[tokio::test]
async fn opening_a_task_window_with_an_instruction_does_not_block_other_requests_on_the_same_connection()
 {
    let Scene { h, mut owner, .. } = scene("tn4-decoupled", READY_DETECTION).await;
    let plan = create_ok(
        &mut owner,
        "plan",
        vec![task("t1", "tn-profile", Some("do the special thing"))],
    )
    .await;

    send_open_task_window(&mut owner, &plan.plan_id, "t1", "tn4-window").await;
    send(&mut owner, &ClientMessage::ListWindows).await;

    // Either order is fine: what must not happen is either reply going missing.
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

// --- The reply never waits for the window to accept input -------------------

// The reply does not wait for the window to accept input.
#[tokio::test]
async fn opening_a_task_window_with_an_instruction_replies_without_waiting_for_the_window() {
    let Scene { h, mut owner, .. } = scene("nq1-immediate-reply", NEVER_READY_DETECTION).await;
    let plan = create_ok(
        &mut owner,
        "plan",
        vec![task("t1", "tn-profile", Some("do the special thing"))],
    )
    .await;

    // The window never becomes ready, so a reply that waited for it to accept
    // input would never come.
    send_open_task_window(&mut owner, &plan.plan_id, "t1", "nq1-window").await;
    expect_task_window_opened(read_ledger_reply(&mut owner, REPLY_TIMEOUT).await);
    h.registry.terminate_all().await;
}

// --- The notice's delivery state in get_plan --------------------------------

#[tokio::test]
async fn a_task_notice_is_delivered_with_a_time_once_its_window_has_started() {
    let Scene { h, mut owner, logs } = scene("nq2-delivered", READY_DETECTION).await;
    let plan = create_ok(
        &mut owner,
        "plan",
        vec![task("t1", "tn-profile", Some("do the special thing"))],
    )
    .await;

    send_open_task_window(&mut owner, &plan.plan_id, "t1", "nq2-window").await;
    expect_task_window_opened(read_ledger_reply(&mut owner, REPLY_TIMEOUT).await);

    let notice = instruction_notice(&mut owner, &plan.plan_id, "t1").await;
    assert_eq!(
        notice.map(|notice| notice.state),
        Some(InstructionNoticeState::Delivered),
        "a notice handed over at launch is delivered by the time the open is answered"
    );
    assert!(
        notice.and_then(|notice| notice.at).is_some(),
        "a delivered notice carries the time it was handed over: {notice:?}"
    );
    started_with(&logs).await;
    h.registry.terminate_all().await;
}
