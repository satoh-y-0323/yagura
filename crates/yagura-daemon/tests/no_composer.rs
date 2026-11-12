#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: what the daemon sends of its own accord (a handoff, a
//! task's instruction) is never typed into a window. It reaches a window whose
//! profile declares delivery through that CLI's own doors, or as the launch
//! prompt of a window it opens; a window that declares no delivery is handed
//! nothing, whatever its detection definition declares, and a launch whose
//! profile declares no `launch_prompt` is refused before anything starts.

use crate::common;

use std::path::Path;
use std::time::Duration;

use common::store_fixtures::{last_opened_seq, opened_seq, wire_window_table};
use common::{
    Harness, READY_MARKER, VALID_TOKEN, approver_base_config, call_list_windows, call_open_window,
    call_send_handoff_from_profile, composer_section_toml, detection_config, empty_lookup,
    expect_error, expect_handoff_sent, handoff_profile_config, identify, policy, read_response,
    register_approver, send, unique_temp_dir, wait_until_status, write_profile,
};
use yagura_daemon::DaemonConfig;
use yagura_protocol::{
    ClientMessage, DaemonMessage, PlanInfo, PlanTaskInfo, PlanTaskSpec, ReceivedReport,
    ReportOutcome, WindowStatus,
};
use yagura_test_support::detection::idle_definition_toml;
use yagura_test_support::ipc::Connection;
use yagura_test_support::process::{echo_child_command, ready_child_profile_source_with_composer};

/// Definition stem whose windows reach `idle` but declare no composer.
const SILENT: &str = "silent-shell";

/// Definition stem identical to [`SILENT`] except that it declares a composer.
const DRAWN: &str = "drawn-shell";

/// Writes a line of the test's own into `window_id` and waits for it to show
/// in the child's output. Bytes reach the child in the order they are
/// written, so anything written into the window before it shows there by
/// then.
async fn write_marker_and_see_it(h: &Harness, window_id: &str, marker: &str) {
    h.write_input(window_id, format!("{marker}\r").as_bytes())
        .await;
    h.wait_for_scrollback(window_id, marker.as_bytes()).await;
}

fn write_definitions(dir: &Path) {
    std::fs::write(
        dir.join(format!("{SILENT}.toml")),
        idle_definition_toml(SILENT, "", ""),
    )
    .expect("write the definition without a composer");
    std::fs::write(
        dir.join(format!("{DRAWN}.toml")),
        idle_definition_toml(DRAWN, "", &composer_section_toml()),
    )
    .expect("write the definition with a composer");
}

async fn start(tag: &str) -> Harness {
    start_with_base(tag, common::opening_config()).await
}

/// [`start`], with `base` supplying every field but the detection layer and
/// the status token: a case that needs a non-default limit (the handoff slot
/// cap, for instance) sets it on `base` rather than restating the whole
/// fixture.
async fn start_with_base(tag: &str, base: DaemonConfig) -> Harness {
    let dir = unique_temp_dir(tag);
    write_definitions(&dir);
    Harness::start(
        tag,
        DaemonConfig {
            detection_dir: Some(dir),
            status_token_override: Some(VALID_TOKEN.into()),
            ..base
        },
    )
    .await
}

/// Opens an `echo_child` window under `detection` and observes it reach
/// `idle`, so a decline cannot be put down to a window that was not ready.
async fn open_ready(h: &Harness, detection: &str) -> String {
    let mut client = h.connect().await;
    let window_id = h.open(echo_child_command(), Some(detection)).await;
    h.write_input(&window_id, READY_MARKER).await;
    wait_until_status(&mut client, &window_id, WindowStatus::Idle).await;
    window_id
}

/// [`open_ready`], for a window whose profile declares delivery with no way to wake
/// its CLI.
async fn open_ready_delivering(h: &Harness, detection: &str) -> String {
    let mut client = h.connect().await;
    let window_id = h
        .registry
        .open_with_extra_env(
            yagura_daemon::OpenWindowRequest {
                command: echo_child_command(),
                cwd: None,
                name: None,
                vendor: None,
                detection: Some(detection.to_string()),
                on_name_taken: Default::default(),
            },
            Vec::new(),
            Vec::new(),
            yagura_daemon::window::WindowProvenance {
                delivery: Some(yagura_daemon::profile_delivery::ProfileDelivery {
                    output: yagura_daemon::inbox::DoorOutputs::all(
                        &yagura_daemon::inbox::OutputForm::Plain,
                    ),
                    max_bytes: yagura_daemon::inbox::DEFAULT_MAX_BYTES,
                    wake: yagura_daemon::profile_delivery::WakeMode::None,
                    wake_stream: yagura_protocol::DeliveryStream::Stderr,
                    wake_exit: yagura_daemon::inbox::DEFAULT_WAKE_EXIT,
                    launch_prompt: None,
                    launch_context: None,
                }),
                ..yagura_daemon::window::WindowProvenance::default()
            },
        )
        .await
        .expect("registry.open_with_extra_env should spawn the child")
        .window_id;
    h.write_input(&window_id, READY_MARKER).await;
    wait_until_status(&mut client, &window_id, WindowStatus::Idle).await;
    window_id
}

/// A handoff's answer as its wire string.
fn delivery_wire(message: DaemonMessage) -> String {
    let (_, _, delivery) = expect_handoff_sent(message);
    serde_json::to_value(delivery)
        .expect("a delivery encodes")
        .as_str()
        .expect("a delivery is a string on the wire")
        .to_string()
}

fn handoff_to(window_id: &str) -> ClientMessage {
    ClientMessage::SendHandoff {
        window_id: Some(window_id.to_string()),
        profile: None,
        cwd: None,
        name: None,
        model: None,
        effort: None,
        reference: "src/foo.rs".to_string(),
        note: "please look at the error handling".to_string(),
    }
}

#[tokio::test]
async fn send_handoff_to_a_window_that_declares_no_delivery_is_undelivered_and_frees_its_slot() {
    let h = start_with_base(
        "no-composer-entry-handoff",
        DaemonConfig {
            max_open_handoffs_per_conn: 1,
            ..common::opening_config()
        },
    )
    .await;
    // A window whose definition declares a composer, so that is no way in either.
    let window_id = open_ready(&h, DRAWN).await;
    let entry = h.registry.get(&window_id).expect("drawn window");
    let seq_before: u64 = entry.write_seq();
    let mut client = h.connect().await;

    // With the per-connection slot cap set to one, a second answer that still
    // reads undelivered (rather than resource_exhausted) proves the first
    // answer released the slot it reserved.
    for attempt in 1..=2 {
        send(&mut client, &handoff_to(&window_id)).await;
        assert_eq!(
            delivery_wire(read_response(&mut client).await),
            "undelivered_no_delivery",
            "attempt {attempt}: a window opened from a raw argv declares no way to deliver, \
             whatever its definition declares"
        );
    }
    write_marker_and_see_it(&h, &window_id, "undelivered-marker").await;
    assert_eq!(
        entry.write_seq(),
        seq_before + 1,
        "nothing is typed into a window a handoff cannot be delivered to"
    );

    h.registry.terminate_all().await;
}

#[tokio::test]
async fn a_window_with_no_definition_refuses_send_message_as_unknown_and_a_handoff_as_undeliverable()
 {
    let h = start("no-composer-entry-undefined").await;
    let window_id = h
        .open(echo_child_command(), Some("absent-definition"))
        .await;
    let mut client = h.connect().await;

    send(
        &mut client,
        &ClientMessage::SendText {
            window_id: window_id.clone(),
            text: "hello\r".to_string(),
        },
    )
    .await;
    expect_error(read_response(&mut client).await, "status_unknown");

    send(&mut client, &handoff_to(&window_id)).await;
    assert_eq!(
        delivery_wire(read_response(&mut client).await),
        "undelivered_no_delivery",
        "a handoff is never typed: a window that declares no delivery is answered so, \
         whatever its status"
    );

    h.registry.terminate_all().await;
}

// =====================================================================
// Before a child is started: a profile-addressed handoff, and a task
// window whose task carries an instruction, are refused from the launch's
// profile alone when it declares no launch_prompt, so a request that is
// already known to be refused never spawns anything.
// =====================================================================

/// Definition stem whose windows reach `idle` on the ready fixture's own
/// startup line but declare no composer; the launch-side counterpart of
/// [`SILENT`].
const SILENT_LAUNCH: &str = "silent-launch";

fn write_launch_definitions(dir: &Path) {
    std::fs::write(
        dir.join(format!("{SILENT_LAUNCH}.toml")),
        idle_definition_toml(SILENT_LAUNCH, "", ""),
    )
    .expect("write the launch definition");
}

/// A profile launching the ready fixture (which prints the idle marker on
/// its own and paints a composer) under `detection`, with no `[delivery]`
/// table and so no launch_prompt.
fn ready_profile_source_with_detection(display_name: &str, detection: &str) -> String {
    let source = ready_child_profile_source_with_composer(display_name, 0);
    let with_detection = source.replacen(
        "\n\n[command]",
        &format!("\ndetection = \"{detection}\"\n\n[command]"),
        1,
    );
    assert_ne!(
        with_detection, source,
        "the profile source must contain the command table header once"
    );
    with_detection
}

/// A `hook_child` profile under [`SILENT_LAUNCH`] (no composer) that hands a first
/// request over as `--prompt <request>`: a launch that needs no composer at all.
fn prompted_profile_source(logs: &Path) -> String {
    let command = [
        std::path::PathBuf::from(env!("CARGO_BIN_EXE_hook_child"))
            .display()
            .to_string(),
        yagura_test_support::process::COMPOSER_MARKER.to_string(),
        yagura_test_support::process::COMPOSER_SEPARATOR.to_string(),
        yagura_test_support::process::COMPOSER_CONTINUATION_PREFIX.to_string(),
        "0".to_string(),
        "-".to_string(),
        "--log".to_string(),
        logs.display().to_string(),
    ]
    .iter()
    .map(|arg| format!("'{arg}'"))
    .collect::<Vec<_>>()
    .join(", ");
    format!(
        "schema = 1\ndisplay_name = \"Prompted\"\ndetection = \"{SILENT_LAUNCH}\"\n\n\
         [command]\ndefault = [{command}]\n\n[delivery]\nwake = \"none\"\n\
         launch_prompt = [\"--prompt\", \"{{prompt}}\"]\n"
    )
}

fn write_launch_profiles(profiles_dir: &Path) {
    let logs = profiles_dir.join("prompted-logs");
    std::fs::create_dir_all(&logs).expect("create the prompted profile's log directory");
    write_profile(
        profiles_dir,
        "prompted-profile",
        &prompted_profile_source(&logs),
    );
    write_profile(
        profiles_dir,
        "silent-profile",
        &ready_profile_source_with_detection("Silent", SILENT_LAUNCH),
    );
}

fn assert_no_launch_prompt_guidance(message: &str, context: &str) {
    assert!(
        message.contains("launch_prompt") && message.contains("open_window"),
        "{context}: the refusal must name the missing launch_prompt and open_window as the \
         alternative, got {message:?}"
    );
}

/// Starts a daemon whose profiles name the launch definitions, with
/// `playbook` / `approval_policy` deciding what the policy half of the gate
/// would answer.
async fn start_launch(
    tag: &str,
    playbook: Option<std::path::PathBuf>,
    approval_policy: Option<yagura_daemon::control::ApprovalPolicy>,
) -> (Harness, std::path::PathBuf) {
    let dir = unique_temp_dir(tag);
    let profiles_dir = dir.join("profiles");
    let detection_dir = dir.join("detection");
    std::fs::create_dir_all(&profiles_dir).expect("create profiles dir");
    std::fs::create_dir_all(&detection_dir).expect("create detection dir");
    write_launch_definitions(&detection_dir);
    write_launch_profiles(&profiles_dir);
    let config = handoff_profile_config(
        Some(profiles_dir),
        empty_lookup(),
        &detection_dir,
        playbook,
        approval_policy,
        30,
    );
    (Harness::start(tag, config).await, dir)
}

async fn window_count(client: &mut Connection) -> usize {
    call_list_windows(client).await.len()
}

#[tokio::test]
async fn send_handoff_to_a_profile_that_declares_no_launch_prompt_starts_no_child() {
    let (h, dir) = start_launch(
        "no-composer-pre-open-handoff",
        None,
        Some(policy(&["send_handoff"])),
    )
    .await;
    let db = wire_window_table(&h.registry, &dir);
    let mut client = h.connect().await;

    let before: usize = window_count(&mut client).await;
    let last_before = last_opened_seq(&db);
    let msg =
        call_send_handoff_from_profile(&mut client, "silent-profile", None, None, "r", "t").await;
    let message = expect_error(msg, "no_composer");
    assert_no_launch_prompt_guidance(&message, "a profile-addressed handoff");
    assert_eq!(
        window_count(&mut client).await,
        before,
        "a handoff known to be refused must not have opened a window for it, even one it \
         closes again before answering"
    );

    let control =
        call_send_handoff_from_profile(&mut client, "prompted-profile", None, None, "r", "t").await;
    let (control_id, opened, delivery) = expect_handoff_sent(control);
    assert!(
        opened,
        "a launch whose profile hands the envelope over at launch must open, composer or not"
    );
    assert_eq!(
        opened_seq(&db, &control_id),
        last_before + 1,
        "the control must be the first open started after the refusal: a launch started for \
         the refused handoff, even one left running behind the answer, would have come first"
    );
    assert_eq!(
        serde_json::to_value(delivery).expect("a delivery encodes"),
        "delivered",
        "handed over as the launch prompt, the envelope is taken once the window started"
    );
    assert_eq!(
        window_count(&mut client).await,
        before + 1,
        "the control must have opened exactly one window, or the unchanged count proves nothing"
    );

    h.registry.terminate_all().await;
}

#[tokio::test]
async fn the_launch_prompt_refusal_follows_the_launch_content_check_and_precedes_the_playbook_check()
 {
    let tag = "no-composer-pre-open-order";
    let playbook_dir = unique_temp_dir(&format!("{tag}-playbook"));
    // A configured playbook with no loaded policy is how a playbook that
    // failed to load at startup is represented; the file itself is never read.
    let playbook = playbook_dir.join("unused-playbook.toml");
    std::fs::write(&playbook, "").expect("write the playbook placeholder");
    let (h, dir) = start_launch(tag, Some(playbook), None).await;
    let db = wire_window_table(&h.registry, &dir);
    let mut client = h.connect().await;
    let before: usize = window_count(&mut client).await;
    let last_before = last_opened_seq(&db);

    let missing_cwd = dir.join("cwd-that-does-not-exist");
    let missing_cwd = missing_cwd.to_string_lossy().into_owned();
    let msg = call_send_handoff_from_profile(
        &mut client,
        "silent-profile",
        Some(&missing_cwd),
        None,
        "r",
        "t",
    )
    .await;
    expect_error(msg, "invalid_argument");

    let msg =
        call_send_handoff_from_profile(&mut client, "silent-profile", None, None, "r", "t").await;
    let message = expect_error(msg, "no_composer");
    assert_no_launch_prompt_guidance(&message, "a handoff under a broken playbook");

    let control =
        call_send_handoff_from_profile(&mut client, "prompted-profile", None, None, "r", "t").await;
    let control_message = expect_error(control, "approval_required");
    assert!(
        control_message.contains("the configured playbook failed to load"),
        "the broken playbook must still refuse a launch that can be handed the envelope, or \
         the refusal above was never tested against it: {control_message:?}"
    );

    assert_eq!(
        window_count(&mut client).await,
        before,
        "none of these refusals may have opened a window"
    );
    let marker = h.open(echo_child_command(), None).await;
    assert_eq!(
        opened_seq(&db, &marker),
        last_before + 1,
        "no refusal may have started an open, even one left running behind its answer"
    );
    h.registry.terminate_all().await;
}

// --- open_task_window --------------------------------------------------------

/// A daemon with the plan ledger enabled whose profiles name the launch
/// definitions. The plan review gate is off so a created plan's tasks can be
/// opened at once.
async fn start_ledger(tag: &str) -> Harness {
    let dir = unique_temp_dir(tag);
    let profiles_dir = dir.join("profiles");
    let detection_dir = dir.join("detection");
    std::fs::create_dir_all(&profiles_dir).expect("create profiles dir");
    std::fs::create_dir_all(&detection_dir).expect("create detection dir");
    write_launch_profiles(&profiles_dir);
    let config = DaemonConfig {
        store_db_path_resolved: Some(dir.join("plan.db")),
        profiles_dir: Some(profiles_dir),
        approval_policy: Some(policy(&["open_window"])),
        plan_approval_required: false,
        ..detection_config(&detection_dir, approver_base_config())
    };
    write_launch_definitions(&detection_dir);
    let (h, _locks) = Harness::start_with_ledger(tag, config).await;
    h
}

/// Reads the next response frame, skipping `plan_changed` pushes.
async fn read_ledger_reply(client: &mut Connection) -> DaemonMessage {
    let deadline = tokio::time::Instant::now() + yagura_test_support::HANG_GUARD;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(!remaining.is_zero(), "timed out waiting for a reply");
        let frame = read_response(client).await;
        if !matches!(frame, DaemonMessage::PlanChanged { .. }) {
            return frame;
        }
    }
}

fn task_spec(task_id: &str, profile: &str, instruction: Option<&str>) -> PlanTaskSpec {
    PlanTaskSpec {
        task_id: task_id.to_string(),
        title: format!("task {task_id}"),
        role: None,
        profile: Some(profile.to_string()),
        model: None,
        effort: None,
        depends_on: Vec::new(),
        writes: vec![format!("{task_id}.txt")],
        instruction: instruction.map(str::to_string),
    }
}

/// Opens a plain window and identifies `owner` as it: only a window may own
/// a plan and open its task windows.
async fn become_plan_owner(h: &Harness, owner: &mut Connection) {
    let window_id = h.open(echo_child_command(), None).await;
    identify(owner, &window_id).await;
}

async fn create_plan(owner: &mut Connection, tasks: Vec<PlanTaskSpec>) -> PlanInfo {
    send(
        owner,
        &ClientMessage::CreatePlan {
            title: "plan".to_string(),
            tasks,
            ticket: None,
        },
    )
    .await;
    match read_ledger_reply(owner).await {
        DaemonMessage::PlanDetail { plan } => plan,
        other => panic!("expected plan_detail, got {other:?}"),
    }
}

async fn stored_task(owner: &mut Connection, plan_id: &str, task_id: &str) -> PlanTaskInfo {
    send(
        owner,
        &ClientMessage::GetPlan {
            plan_id: plan_id.to_string(),
        },
    )
    .await;
    let plan = match read_ledger_reply(owner).await {
        DaemonMessage::PlanDetail { plan } => plan,
        other => panic!("expected plan_detail, got {other:?}"),
    };
    plan.tasks
        .into_iter()
        .find(|task| task.task_id == task_id)
        .unwrap_or_else(|| panic!("task {task_id} must be in plan {plan_id}"))
}

async fn open_task_window(
    owner: &mut Connection,
    plan_id: &str,
    task_id: &str,
    name: &str,
) -> DaemonMessage {
    send(
        owner,
        &ClientMessage::OpenTaskWindow {
            plan_id: plan_id.to_string(),
            task_id: task_id.to_string(),
            name: Some(name.to_string()),
        },
    )
    .await;
    read_ledger_reply(owner).await
}

fn expect_task_window_opened(msg: DaemonMessage, context: &str) -> String {
    match msg {
        DaemonMessage::TaskWindowOpened { window_id, .. } => window_id,
        other => panic!("{context}: expected task_window_opened, got {other:?}"),
    }
}

async fn windows_named(client: &mut Connection, name: &str) -> usize {
    call_list_windows(client)
        .await
        .into_iter()
        .filter(|window| window.name == name)
        .count()
}

#[tokio::test]
async fn open_task_window_refuses_an_instructed_task_whose_profile_declares_no_launch_prompt_before_starting_a_child()
 {
    let h = start_ledger("no-composer-pre-open-task").await;
    let db = wire_window_table(
        &h.registry,
        &unique_temp_dir("no-composer-pre-open-task-table"),
    );
    let mut owner = h.connect().await;
    become_plan_owner(&h, &mut owner).await;
    let plan = create_plan(
        &mut owner,
        vec![
            task_spec("instructed", "silent-profile", Some("do the thing")),
            task_spec("bare", "silent-profile", None),
            task_spec("prompted", "prompted-profile", Some("do the thing")),
        ],
    )
    .await;

    let before: usize = window_count(&mut owner).await;
    let last_before = last_opened_seq(&db);
    let msg = open_task_window(&mut owner, &plan.plan_id, "instructed", "instructed-window").await;
    let message = expect_error(msg, "no_composer");
    assert_no_launch_prompt_guidance(&message, "an instructed task window");
    assert_eq!(
        window_count(&mut owner).await,
        before,
        "an instructed task whose window could never be told its instruction must not start a \
         child"
    );

    let refused = stored_task(&mut owner, &plan.plan_id, "instructed").await;
    assert_eq!(
        refused.window_id, None,
        "a refused task must not be bound to any window"
    );
    assert_eq!(
        refused.instruction_notice, None,
        "a refused task must not carry a notice that was never going to be delivered"
    );

    let bare = open_task_window(&mut owner, &plan.plan_id, "bare", "bare-window").await;
    let bare_window: String = expect_task_window_opened(
        bare,
        "the same profile with nothing to tell the window must open",
    );
    assert_eq!(
        opened_seq(&db, &bare_window),
        last_before + 1,
        "the control must be the first open started after the refusal: a child started for the \
         refused task, even one left running behind the answer, would have come first"
    );
    assert_eq!(windows_named(&mut owner, "instructed-window").await, 0);
    let prompted = open_task_window(&mut owner, &plan.plan_id, "prompted", "prompted-window").await;
    let prompted_window: String = expect_task_window_opened(
        prompted,
        "an instructed task whose profile hands its notice over at launch must open, \
         composer or not",
    );
    assert_eq!(
        stored_task(&mut owner, &plan.plan_id, "bare")
            .await
            .window_id,
        Some(bare_window),
        "the control must be bound, or an unbound refused task proves nothing"
    );
    assert_eq!(
        stored_task(&mut owner, &plan.plan_id, "prompted")
            .await
            .window_id,
        Some(prompted_window)
    );

    h.registry.terminate_all().await;
}

// =====================================================================
// A window's own report inbox is discarded once the window ends.
// =====================================================================

async fn call_send_report(client: &mut Connection, summary: &str) -> DaemonMessage {
    send(
        client,
        &ClientMessage::SendReport {
            outcome: ReportOutcome::Succeeded,
            summary: summary.to_string(),
            path: None,
        },
    )
    .await;
    read_response(client).await
}

fn expect_report_sent(msg: DaemonMessage) {
    assert!(
        matches!(msg, DaemonMessage::ReportSent { .. }),
        "expected report_sent, got {msg:?}"
    );
}

async fn call_wait_for_report(client: &mut Connection, timeout_sec: u64) -> Vec<ReceivedReport> {
    send(client, &ClientMessage::WaitForReport { timeout_sec }).await;
    match read_response(client).await {
        DaemonMessage::Reports { reports, .. } => reports,
        other => panic!("expected reports, got {other:?}"),
    }
}

/// Opens `detection`'s parent window, opens a second window as its child
/// (identifying the parent first, so `call_open_window` attributes the child
/// to it), and has the child `send_report` to it, landing in the parent's
/// inbox.
async fn open_parent_with_reporting_child(h: &Harness, detection: &str) -> String {
    let parent_id = open_ready_delivering(h, detection).await;
    let mut parent_conn = h.connect().await;
    identify(&mut parent_conn, &parent_id).await;
    let child_id = call_open_window(&mut parent_conn, echo_child_command()).await;
    let mut child_conn = h.connect().await;
    identify(&mut child_conn, &child_id).await;
    expect_report_sent(call_send_report(&mut child_conn, "child done").await);
    parent_id
}

#[tokio::test]
async fn a_windows_report_inbox_is_discarded_once_it_ends() {
    let h = start_with_base(
        "no-composer-notice-inbox",
        DaemonConfig {
            approval_policy: Some(policy(&["close_window", "open_window"])),
            ..approver_base_config()
        },
    )
    .await;
    let silent_id = open_parent_with_reporting_child(&h, SILENT).await;

    let mut approver = h.connect().await;
    register_approver(&mut approver).await;
    send(
        &mut approver,
        &ClientMessage::CloseWindow {
            window_id: silent_id.clone(),
            force: None,
        },
    )
    .await;
    match read_response(&mut approver).await {
        DaemonMessage::WindowClosed { window_id } => assert_eq!(window_id, silent_id),
        other => panic!("expected window_closed, got {other:?}"),
    }
    h.wait_until_child_exited(&silent_id).await;

    let deadline = tokio::time::Instant::now() + yagura_test_support::HANG_GUARD;
    loop {
        let mut probe = h.connect().await;
        identify(&mut probe, &silent_id).await;
        let reports = call_wait_for_report(&mut probe, 0).await;
        if reports.is_empty() {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for a parent's inbox to be discarded once it ended, \
             still had {} report(s)",
            reports.len()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
