#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: the chat mention `@parent` resolves once a message
//! actually reaches the daemon over IPC. Reserved-
//! word parsing itself is `tests/room.rs`'s job; this file drives
//! `post_chat`'s resolution against a real `Harness`: an opener-derived
//! parent, a task-bound window's plan-owner-derived parent (surviving an
//! owner nomination), room membership scoping, mixing `@parent` with an
//! ordinary `@name` mention, a speaker with no parent at all, and the
//! default chat intro's wording.

mod common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use common::{
    ChatPlaybook, Harness, call_open_window, chat_config, identify, read_response,
    register_approver, second_workspace_dir, send, unique_temp_dir, write_profile,
};
use yagura_daemon::{DaemonConfig, OpenWindowRequest};
use yagura_protocol::{ClientMessage, DaemonMessage, PlanInfo, PlanTaskSpec};
use yagura_test_support::ipc::Connection;
use yagura_test_support::process::{echo_child_command, ready_child_profile_source};

// --- config builders ---------------------------------------------------------

/// A chat-only config: `send_chat` auto-approved, intro disabled (the shared
/// suite convention -- see `common::ChatPlaybook`'s own doc comment), the
/// baseline for every case that does not need the plan ledger.
fn parent_config(dir: &Path) -> DaemonConfig {
    chat_config(dir, &ChatPlaybook::default())
}

/// [`parent_config`] plus a resolved `profiles_dir`, for the task-bound
/// window, which `Harness::start_with_chat_and_ledger` serves alongside chat.
/// `plan_approval_required` is off: this file measures `@parent` resolution,
/// not the review gate, so `open_task_window` must not be refused for it.
fn parent_ledger_config(dir: &Path, profiles_dir: PathBuf) -> DaemonConfig {
    DaemonConfig {
        profiles_dir: Some(profiles_dir),
        plan_approval_required: false,
        ..chat_config(dir, &ChatPlaybook::default())
    }
}

// --- connection / message helpers ---------------------------------------------

/// Reads the next reply, skipping a `chat_changed` / `plan_changed` push an
/// approver-registered connection may also receive while its own request is
/// in flight (mirroring `ipc_chat.rs`'s and `ipc_report.rs`'s own
/// `read_reply` helpers).
async fn read_reply(client: &mut Connection, timeout: Duration) -> DaemonMessage {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(!remaining.is_zero(), "timed out waiting for a reply");
        let frame = read_response(client, remaining).await;
        if !matches!(
            frame,
            DaemonMessage::ChatChanged { .. } | DaemonMessage::PlanChanged { .. }
        ) {
            return frame;
        }
    }
}

async fn send_chat_msg(
    client: &mut Connection,
    room_id: Option<&str>,
    body: &str,
) -> DaemonMessage {
    send(
        client,
        &ClientMessage::SendChat {
            room_id: room_id.map(str::to_string),
            body: body.to_string(),
        },
    )
    .await;
    read_reply(client, Duration::from_secs(5)).await
}

struct ChatPostedResult {
    targets: Vec<String>,
    unresolved: Vec<String>,
}

fn expect_chat_posted(msg: DaemonMessage) -> ChatPostedResult {
    match msg {
        DaemonMessage::ChatPosted {
            targets,
            unresolved,
            ..
        } => ChatPostedResult {
            targets,
            unresolved,
        },
        other => panic!("expected chat_posted, got {other:?}"),
    }
}

/// The `[delivery]` table of a profile with no way to wake its CLI while it waits: every
/// window here declares it, since only such a window takes part in chat.
fn wake_none_delivery() -> yagura_daemon::profile_delivery::ProfileDelivery {
    yagura_daemon::profile_delivery::ProfileDelivery {
        output: yagura_daemon::inbox::OutputForm::Plain,
        max_chars: yagura_daemon::inbox::DEFAULT_MAX_CHARS,
        wake: yagura_daemon::profile_delivery::WakeMode::None,
        wake_stream: yagura_protocol::DeliveryStream::Stderr,
        wake_exit: yagura_daemon::inbox::DEFAULT_WAKE_EXIT,
        launch_prompt: None,
        shim_guidance: None,
    }
}

/// Opens an `echo_child` window directly through the registry (bypassing IPC, like
/// `Harness::open`), declaring delivery, with an explicit display `name` when given -- the
/// `@name` case needs a plain `@name` mention target alongside `@parent`.
async fn open_delivering(h: &Harness, name: Option<&str>) -> String {
    h.registry
        .open_with_extra_env(
            OpenWindowRequest {
                command: echo_child_command(),
                cwd: None,
                name: name.map(str::to_string),
                vendor: None,
                detection: None,
            },
            Vec::new(),
            Vec::new(),
            yagura_daemon::window::WindowProvenance {
                delivery: Some(wake_none_delivery()),
                ..yagura_daemon::window::WindowProvenance::default()
            },
        )
        .await
        .expect("registry.open_with_extra_env should spawn the child")
        .window_id
}

async fn open_named(h: &Harness, name: &str) -> String {
    open_delivering(h, Some(name)).await
}

/// Opens a plain `echo_child` window through the registry directly
/// (bypassing IPC, like `Harness::open`) and identifies `client` as it.
async fn open_and_identify(h: &Harness, client: &mut Connection) -> String {
    let window_id = open_delivering(h, None).await;
    identify(client, &window_id).await;
    window_id
}

// --- plan/task fixtures ----------------------------------------------

fn task_with_profile(task_id: &str, profile: &str) -> PlanTaskSpec {
    PlanTaskSpec {
        task_id: task_id.to_string(),
        title: "do the thing".to_string(),
        role: None,
        profile: Some(profile.to_string()),
        model: None,
        effort: None,
        depends_on: Vec::new(),
        writes: vec![format!("{task_id}.txt")],
        instruction: None,
    }
}

/// A launch profile whose `[command]` is `ready_child`, absolute-pathed (no
/// program-lookup resolution needed) and never self-exiting, so a task
/// window bound to it stays alive for the whole test (mirroring
/// `ipc_report.rs`'s own `write_plain_profile`).
fn write_plain_profile(dir: &Path, id: &str, display_name: &str) {
    write_profile(dir, id, &ready_child_profile_source(display_name));
}

async fn create_plan(client: &mut Connection, title: &str, tasks: Vec<PlanTaskSpec>) -> PlanInfo {
    send(
        client,
        &ClientMessage::CreatePlan {
            title: title.to_string(),
            tasks,
        },
    )
    .await;
    match read_reply(client, Duration::from_secs(5)).await {
        DaemonMessage::PlanDetail { plan } => plan,
        other => panic!("expected plan_detail, got {other:?}"),
    }
}

async fn open_task_window(client: &mut Connection, plan_id: &str, task_id: &str) -> String {
    send(
        client,
        &ClientMessage::OpenTaskWindow {
            plan_id: plan_id.to_string(),
            task_id: task_id.to_string(),
            name: None,
        },
    )
    .await;
    match read_reply(client, Duration::from_secs(5)).await {
        DaemonMessage::TaskWindowOpened { window_id, .. } => window_id,
        other => panic!("expected task_window_opened, got {other:?}"),
    }
}

async fn set_plan_owner(client: &mut Connection, plan_id: &str, window_id: &str) {
    send(
        client,
        &ClientMessage::SetPlanOwner {
            plan_id: plan_id.to_string(),
            window_id: window_id.to_string(),
        },
    )
    .await;
    match read_reply(client, Duration::from_secs(5)).await {
        DaemonMessage::PlanDetail { .. } => {}
        other => panic!("expected plan_detail, got {other:?}"),
    }
}

// --- Opener-derived parent, delivered ---------------------------------

#[tokio::test]
async fn at_parent_from_a_child_reaches_its_opener_in_their_shared_room() {
    let h = Harness::start_with_chat(
        "chat-parent-opener",
        parent_config(&unique_temp_dir("chat-parent-opener")),
    )
    .await;

    // The parent must actually be able to *receive* a delivery (not merely
    // exist), so it is driven to `Idle` in composer-draw mode -- the same
    // discipline `ipc_chat.rs` uses for every delivery target.
    let mut setup = h.connect().await;
    let parent_window = h.open_idle_window_with_composer(&mut setup).await;
    let mut parent_conn = h.connect().await;
    identify(&mut parent_conn, &parent_window).await;

    let child_window = call_open_window(&mut parent_conn, echo_child_command()).await;
    let mut child_conn = h.connect().await;
    identify(&mut child_conn, &child_window).await;

    let posted =
        expect_chat_posted(send_chat_msg(&mut child_conn, None, "@parent are you there?").await);
    assert_eq!(posted.targets, vec![parent_window.clone()]);
    assert!(posted.unresolved.is_empty());

    h.wait_for_scrollback(&parent_window, b"are you there?", Duration::from_secs(5))
        .await;
}

// --- Task-bound window follows the plan's live owner, not opener -----

#[tokio::test]
async fn at_parent_from_a_task_bound_window_resolves_to_the_plans_live_owner() {
    let dir = unique_temp_dir("chat-parent-task-owner");
    let profiles_dir = dir.join("profiles");
    std::fs::create_dir_all(&profiles_dir).expect("create profiles dir");
    write_plain_profile(&profiles_dir, "chat-parent-profile", "Chat Parent Profile");
    let (h, _locks) = Harness::start_with_chat_and_ledger(
        "chat-parent-task-owner",
        parent_ledger_config(&dir, profiles_dir),
    )
    .await;

    let mut owner_a = h.connect().await;
    let window_a = open_and_identify(&h, &mut owner_a).await;
    let plan = create_plan(
        &mut owner_a,
        "plan",
        vec![task_with_profile("t1", "chat-parent-profile")],
    )
    .await;
    let bound_window = open_task_window(&mut owner_a, &plan.plan_id, "t1").await;
    let mut bound_conn = h.connect().await;
    identify(&mut bound_conn, &bound_window).await;

    let first =
        expect_chat_posted(send_chat_msg(&mut bound_conn, None, "@parent status update").await);
    assert_eq!(first.targets, vec![window_a.clone()]);
    assert!(first.unresolved.is_empty());

    // window_a is both the task's opener and the plan's owner so far, so the
    // assertion above alone cannot tell the two rules apart. Reassigning the
    // owner to a window that never opened anything settles it.
    let mut owner_b = h.connect().await;
    let window_b = open_and_identify(&h, &mut owner_b).await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;
    set_plan_owner(&mut approver, &plan.plan_id, &window_b).await;

    let second =
        expect_chat_posted(send_chat_msg(&mut bound_conn, None, "@parent status update 2").await);
    assert_eq!(
        second.targets,
        vec![window_b.clone()],
        "the resolved parent must follow the plan's live owner, not the task window's own \
         opener (window_a, unchanged since it opened the task window)"
    );
    assert!(!second.targets.contains(&window_a));
}

#[tokio::test]
async fn at_parent_from_a_task_window_dropped_from_its_plan_is_unresolved() {
    let dir = unique_temp_dir("chat-parent-task-dropped");
    let profiles_dir = dir.join("profiles");
    std::fs::create_dir_all(&profiles_dir).expect("create profiles dir");
    write_plain_profile(&profiles_dir, "chat-parent-profile", "Chat Parent Profile");
    let (h, _locks) = Harness::start_with_chat_and_ledger(
        "chat-parent-task-dropped",
        parent_ledger_config(&dir, profiles_dir),
    )
    .await;

    let mut owner = h.connect().await;
    let opener = open_and_identify(&h, &mut owner).await;
    let plan = create_plan(
        &mut owner,
        "plan",
        vec![task_with_profile("t1", "chat-parent-profile")],
    )
    .await;
    let task_window = open_task_window(&mut owner, &plan.plan_id, "t1").await;
    let mut task_conn = h.connect().await;
    identify(&mut task_conn, &task_window).await;

    send(
        &mut owner,
        &ClientMessage::UpdatePlan {
            plan_id: plan.plan_id.clone(),
            title: None,
            tasks: vec![task_with_profile("t2", "chat-parent-profile")],
        },
    )
    .await;
    match read_reply(&mut owner, Duration::from_secs(5)).await {
        DaemonMessage::PlanDetail { .. } => {}
        other => panic!("expected plan_detail, got {other:?}"),
    }

    let posted =
        expect_chat_posted(send_chat_msg(&mut task_conn, None, "@parent are you there?").await);
    assert!(
        posted.targets.is_empty(),
        "a task window no longer bound to its task has no parent, not its opener {opener}"
    );
    assert_eq!(posted.unresolved, vec!["parent".to_string()]);
}

// --- `@parent` is scoped to the room the parent actually shares ------

#[tokio::test]
async fn at_parent_is_unresolved_outside_the_parents_room_but_resolves_in_main() {
    let h = Harness::start_with_chat(
        "chat-parent-room-scope",
        parent_config(&unique_temp_dir("chat-parent-room-scope")),
    )
    .await;

    let second_ws = second_workspace_dir("chat-parent-room-scope");
    let parent_window = h
        .open_delivering_in(echo_child_command(), None, Some(&second_ws))
        .await;
    let mut parent_conn = h.connect().await;
    identify(&mut parent_conn, &parent_window).await;

    // Opened over IPC with no `cwd` override, so the child lands in the
    // daemon's own default room -- a different one than the parent's.
    let child_window = call_open_window(&mut parent_conn, echo_child_command()).await;
    let mut child_conn = h.connect().await;
    identify(&mut child_conn, &child_window).await;

    let own_room =
        expect_chat_posted(send_chat_msg(&mut child_conn, None, "@parent are you there?").await);
    assert!(
        own_room.targets.is_empty(),
        "the parent lives in a different workspace's room, so it is not a member of the \
         child's own room"
    );
    assert_eq!(own_room.unresolved, vec!["parent".to_string()]);

    let main_room = expect_chat_posted(
        send_chat_msg(&mut child_conn, Some("main"), "@parent are you there?").await,
    );
    assert_eq!(
        main_room.targets,
        vec![parent_window],
        "\"main\" holds every participant regardless of workspace"
    );
    assert!(main_room.unresolved.is_empty());
}

// --- A speaker with no parent at all -----

#[tokio::test]
async fn at_parent_from_a_speaker_with_no_parent_is_unresolved() {
    let h = Harness::start_with_chat(
        "chat-parent-no-parent",
        parent_config(&unique_temp_dir("chat-parent-no-parent")),
    )
    .await;

    let mut approver = h.connect().await;
    register_approver(&mut approver).await;
    let from_approver =
        expect_chat_posted(send_chat_msg(&mut approver, None, "@parent are you there?").await);
    assert!(
        from_approver.targets.is_empty(),
        "the GUI's own approver connection is not a window, so it has no parent"
    );
    assert_eq!(from_approver.unresolved, vec!["parent".to_string()]);

    let opener_less_window = open_delivering(&h, None).await;
    let mut conn = h.connect().await;
    identify(&mut conn, &opener_less_window).await;
    let from_opener_less =
        expect_chat_posted(send_chat_msg(&mut conn, None, "@parent are you there?").await);
    assert!(
        from_opener_less.targets.is_empty(),
        "a window opened with no recorded opener has no parent either"
    );
    assert_eq!(from_opener_less.unresolved, vec!["parent".to_string()]);
}

#[tokio::test]
async fn an_unresolved_parent_is_reported_alongside_another_unresolved_mention() {
    let h = Harness::start_with_chat(
        "chat-parent-two-unresolved",
        parent_config(&unique_temp_dir("chat-parent-two-unresolved")),
    )
    .await;

    let mut approver = h.connect().await;
    register_approver(&mut approver).await;
    let posted = expect_chat_posted(
        send_chat_msg(&mut approver, None, "@nobody @parent are you there?").await,
    );
    assert!(posted.targets.is_empty());
    assert_eq!(
        posted.unresolved,
        vec!["nobody".to_string(), "parent".to_string()],
        "an unresolved @parent must be reported even when another mention is already unresolved"
    );
}

// --- A task window nominated as its own plan's owner -----

#[tokio::test]
async fn at_parent_from_a_task_window_that_owns_its_own_plan_addresses_nobody() {
    let dir = unique_temp_dir("chat-parent-self-owner");
    let profiles_dir = dir.join("profiles");
    std::fs::create_dir_all(&profiles_dir).expect("create profiles dir");
    write_plain_profile(&profiles_dir, "chat-parent-profile", "Chat Parent Profile");
    let (h, _locks) = Harness::start_with_chat_and_ledger(
        "chat-parent-self-owner",
        parent_ledger_config(&dir, profiles_dir),
    )
    .await;

    let mut owner = h.connect().await;
    let _opener = open_and_identify(&h, &mut owner).await;
    let plan = create_plan(
        &mut owner,
        "plan",
        vec![task_with_profile("t1", "chat-parent-profile")],
    )
    .await;
    let bound_window = open_task_window(&mut owner, &plan.plan_id, "t1").await;
    let mut bound_conn = h.connect().await;
    identify(&mut bound_conn, &bound_window).await;

    let mut approver = h.connect().await;
    register_approver(&mut approver).await;
    set_plan_owner(&mut approver, &plan.plan_id, &bound_window).await;

    let posted =
        expect_chat_posted(send_chat_msg(&mut bound_conn, None, "@parent status update").await);
    assert!(
        posted.targets.is_empty(),
        "a window that is its own parent must not be addressed by its own post: {:?}",
        posted.targets
    );
    assert!(
        posted.unresolved.is_empty(),
        "a self-addressed @parent is dropped, not reported as unresolved: {:?}",
        posted.unresolved
    );
}

// --- `@parent` does not crowd out an ordinary `@name` mention -------

#[tokio::test]
async fn at_parent_and_at_name_in_the_same_message_both_become_targets() {
    let h = Harness::start_with_chat(
        "chat-parent-plus-name",
        parent_config(&unique_temp_dir("chat-parent-plus-name")),
    )
    .await;

    let parent_window = open_delivering(&h, None).await;
    let mut parent_conn = h.connect().await;
    identify(&mut parent_conn, &parent_window).await;

    let child_window = call_open_window(&mut parent_conn, echo_child_command()).await;
    let mut child_conn = h.connect().await;
    identify(&mut child_conn, &child_window).await;

    let bob_window = open_named(&h, "Bob").await;

    let posted = expect_chat_posted(
        send_chat_msg(&mut child_conn, None, "@parent @Bob status update").await,
    );
    assert_eq!(
        posted.targets.len(),
        2,
        "@parent must not crowd out an ordinary @name mention in the same message: {:?}",
        posted.targets
    );
    assert!(posted.targets.contains(&parent_window));
    assert!(posted.targets.contains(&bob_window));
    assert!(posted.unresolved.is_empty());
}

// --- The default intro names @parent without narrowing chat's use ---

#[test]
fn default_chat_intro_names_parent_alongside_user_and_main() {
    let intro = DaemonConfig::default().chat_intro;
    assert!(
        intro.contains("@parent"),
        "the default intro must name @parent as a reachable destination, got: {intro:?}"
    );
    assert!(
        intro.contains("@user"),
        "adding @parent must not drop the existing @user guidance, got: {intro:?}"
    );
    assert!(
        intro.contains("main"),
        "adding @parent must not drop the existing \"main\" room guidance, got: {intro:?}"
    );
}
