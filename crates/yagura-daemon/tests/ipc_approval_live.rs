#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: an approval setting changed over IPC takes effect on the
//! very next decision the running daemon makes, without a restart.
//!
//! Every case boots the daemon from a real playbook file (see
//! [`common::live_playbook_config`]) and changes it only through an
//! approver's `set_auto_approve` / `set_plan_approval_required`. A request
//! already waiting for approval when its tool is switched to automatic stays
//! waiting: the switch applies to requests that arrive after it.
//!
//! Every negative observation (nothing arrives, no request is created) is
//! read behind a later answer that has to come after it, and is paired with a
//! positive control under the same conditions.

use crate::common;
use crate::common::bins::ready_child_profile_source;

use std::path::{Path, PathBuf};
use std::time::Duration;

use common::{
    ChatPlaybook, Harness, approver_base_config, call_send_handoff_from_profile,
    chat_config_with_playbook_file, detection_config, empty_lookup,
    expect_approval_requested_skipping_pushes, expect_error, expect_handoff_sent,
    handoff_profile_config, identify, live_playbook_config, read_approver_reply, read_response,
    register_approver, reported_auto_approve, send, send_handoff_from_profile, set_auto_approve,
    set_plan_approval_required, unique_temp_dir, write_profile,
};
use yagura_daemon::DaemonConfig;
use yagura_protocol::{
    ApprovalRequest, ApprovalSettingsInfo, ClientMessage, DaemonMessage, PlanInfo, PlanReview,
    PlanTaskSpec, WithdrawReason,
};
use yagura_test_support::ipc::{
    Connection, read_correlated_daemon_frame, send_correlated_client_message, send_raw_frame,
};
use yagura_test_support::paths::TempDir;
use yagura_test_support::process::profile_source;

const NO_AUTO_APPROVE_PLAYBOOK: &str = "[approval]\nauto_approve = []\n";

// --- shared steps -------------------------------------------------------------

/// A daemon booted from a playbook that auto-approves nothing, with an
/// approver registered on its own connection.
async fn start_with_approver(tag: &str, source: &str) -> (Harness, Connection) {
    let dir = unique_temp_dir(tag);
    let config: DaemonConfig =
        live_playbook_config(&dir, detection_config(&dir, approver_base_config()), source);
    let h = Harness::start(tag, config).await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;
    (h, approver)
}

/// Opens an idle window and identifies a fresh connection as it.
async fn identified_window(h: &Harness) -> (Connection, String) {
    let mut client = h.connect().await;
    let window_id = h.open_idle_window(&mut client).await;
    identify(&mut client, &window_id).await;
    (client, window_id)
}

async fn send_text(client: &mut Connection, window_id: &str, text: &str) {
    send(
        client,
        &ClientMessage::SendText {
            window_id: window_id.to_string(),
            text: text.to_string(),
        },
    )
    .await;
}

fn expect_text_sent(reply: DaemonMessage, window_id: &str) {
    match reply {
        DaemonMessage::TextSent {
            window_id: acked, ..
        } => assert_eq!(acked, window_id),
        other => panic!("expected text_sent, got {other:?}"),
    }
}

/// Sends a denial for `request_id` and waits for its resolution.
async fn decide_denied(approver: &mut Connection, request_id: &str) {
    send(
        approver,
        &ClientMessage::ApprovalDecision {
            request_id: request_id.to_string(),
            approved: false,
        },
    )
    .await;
    let reply: DaemonMessage = read_approver_reply(approver).await;
    match reply {
        DaemonMessage::ApprovalResolved {
            request_id: resolved,
        } => assert_eq!(resolved, request_id),
        other => panic!("expected approval_resolved, got {other:?}"),
    }
}

/// Denies `request` and asserts its requester is told so.
async fn deny(approver: &mut Connection, requester: &mut Connection, request: &ApprovalRequest) {
    decide_denied(approver, &request.request_id).await;
    let reply: DaemonMessage = read_response(requester).await;
    expect_error(reply, "approval_denied");
}

/// Re-registers `approver` and returns the request ids still pending.
async fn pending_request_ids(approver: &mut Connection) -> Vec<String> {
    send(approver, &ClientMessage::RegisterApprover).await;
    let reply: DaemonMessage = read_approver_reply(approver).await;
    let pending: Vec<ApprovalRequest> = match reply {
        DaemonMessage::ApproverRegistered { pending } => pending,
        other => panic!("expected approver_registered, got {other:?}"),
    };
    pending
        .into_iter()
        .map(|request: ApprovalRequest| request.request_id)
        .collect()
}

async fn switch_tool(approver: &mut Connection, tool: &str, auto_approve: bool) {
    let settings: ApprovalSettingsInfo = set_auto_approve(approver, tool, auto_approve).await;
    assert_eq!(
        reported_auto_approve(&settings, tool),
        Some(auto_approve),
        "the answer must carry the daemon's value after the switch, got {settings:?}"
    );
}

// --- correlation ids ----------------------------------------------------------

const REQUESTER_CORRELATION_ID: u64 = 7_001;
const BYSTANDER_CORRELATION_ID: u64 = 7_003;
// The top of the range: the daemon must hand the value back whole.
const DECIDER_CORRELATION_ID: u64 = u64::MAX;

fn assert_carries(actual: Option<u64>, expected: Option<u64>, frame: &str) {
    assert_eq!(actual, expected, "correlation_id on {frame}");
}

/// Reads the next frame that is not a push a connection may be sent at any
/// moment, asserting that every push it skips carries no `correlation_id`.
async fn read_correlated_reply(client: &mut Connection) -> (Option<u64>, DaemonMessage) {
    loop {
        let (correlation_id, message) = read_correlated_daemon_frame(client)
            .await
            .expect("the connection closed while a reply was awaited");
        match message {
            DaemonMessage::Frame(_)
            | DaemonMessage::StatusChanged { .. }
            | DaemonMessage::ChatParticipantsChanged
            | DaemonMessage::ChatChanged { .. }
            | DaemonMessage::PlanChanged { .. } => {
                assert_carries(correlation_id, None, &format!("the push {message:?}"));
            }
            other => return (correlation_id, other),
        }
    }
}

/// Sends `msg` with `correlation_id` and returns its answer, asserting that the
/// answer carries the same id.
async fn correlated_request(
    client: &mut Connection,
    msg: &ClientMessage,
    correlation_id: u64,
) -> DaemonMessage {
    send_correlated_client_message(client, msg, Some(correlation_id)).await;
    let (carried, reply) = read_correlated_reply(client).await;
    assert_carries(
        carried,
        Some(correlation_id),
        &format!("the answer {reply:?}"),
    );
    reply
}

async fn expect_uncorrelated_approval_requested(client: &mut Connection) -> ApprovalRequest {
    let (correlation_id, reply) = read_correlated_reply(client).await;
    let DaemonMessage::ApprovalRequested { request } = reply else {
        panic!("expected approval_requested, got {reply:?}");
    };
    assert_carries(correlation_id, None, "the approval_requested push");
    request
}

// --- send_message -------------------------------------------------------------

#[tokio::test]
async fn switching_send_message_to_automatic_and_back_takes_effect_on_the_next_request() {
    let (h, mut approver) =
        start_with_approver("live-send-message", NO_AUTO_APPROVE_PLAYBOOK).await;
    let (mut source, _window_a) = identified_window(&h).await;
    let window_b = h.open_idle_window(&mut source).await;

    send_text(&mut source, &window_b, "before the switch").await;
    let request: ApprovalRequest = expect_approval_requested_skipping_pushes(&mut approver).await;
    assert_eq!(request.tool, "send_message");
    deny(&mut approver, &mut source, &request).await;

    switch_tool(&mut approver, "send_message", true).await;
    send_text(&mut source, &window_b, "after switching on").await;
    let reply: DaemonMessage = read_response(&mut source).await;
    expect_text_sent(reply, &window_b);
    assert_eq!(
        pending_request_ids(&mut approver).await,
        Vec::<String>::new(),
        "a request made while send_message is automatic must not wait for approval"
    );

    switch_tool(&mut approver, "send_message", false).await;
    send_text(&mut source, &window_b, "after switching off").await;
    let request: ApprovalRequest = expect_approval_requested_skipping_pushes(&mut approver).await;
    assert_eq!(request.tool, "send_message");
    deny(&mut approver, &mut source, &request).await;

    h.registry.terminate_all().await;
}

// --- send_chat ----------------------------------------------------------------

async fn post_chat(client: &mut Connection, body: &str) {
    send(
        client,
        &ClientMessage::SendChat {
            room_id: None,
            body: body.to_string(),
        },
    )
    .await;
}

/// Reads a poster's reply, skipping `chat_changed` pushes.
async fn read_poster_reply(client: &mut Connection) -> DaemonMessage {
    read_approver_reply(client).await
}

#[tokio::test]
async fn switching_send_chat_to_automatic_and_back_takes_effect_on_the_next_post() {
    let dir = unique_temp_dir("live-send-chat");
    let config: DaemonConfig = chat_config_with_playbook_file(
        &dir,
        &ChatPlaybook {
            auto_approve: &[],
            ..ChatPlaybook::default()
        },
    );
    let h = Harness::start_with_chat("live-send-chat", config).await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;
    let (mut poster, _poster_id) = identified_window(&h).await;

    post_chat(&mut poster, "first post, before the switch").await;
    let request: ApprovalRequest = expect_approval_requested_skipping_pushes(&mut approver).await;
    assert_eq!(request.tool, "send_chat");
    decide_denied(&mut approver, &request.request_id).await;
    let reply: DaemonMessage = read_poster_reply(&mut poster).await;
    expect_error(reply, "approval_denied");

    switch_tool(&mut approver, "send_chat", true).await;
    post_chat(&mut poster, "second post, after switching on").await;
    let reply: DaemonMessage = read_poster_reply(&mut poster).await;
    match reply {
        DaemonMessage::ChatPosted { .. } => {}
        other => panic!("expected chat_posted without approval, got {other:?}"),
    }
    assert_eq!(
        pending_request_ids(&mut approver).await,
        Vec::<String>::new(),
        "a post made while send_chat is automatic must not wait for approval"
    );

    switch_tool(&mut approver, "send_chat", false).await;
    post_chat(&mut poster, "third post, after switching off").await;
    let request: ApprovalRequest = expect_approval_requested_skipping_pushes(&mut approver).await;
    assert_eq!(request.tool, "send_chat");
    decide_denied(&mut approver, &request.request_id).await;
    let reply: DaemonMessage = read_poster_reply(&mut poster).await;
    expect_error(reply, "approval_denied");

    h.registry.terminate_all().await;
}

// --- broadcast_chat -----------------------------------------------------------

async fn broadcast(client: &mut Connection, body: &str) {
    send(
        client,
        &ClientMessage::BroadcastChat {
            room_id: None,
            body: body.to_string(),
        },
    )
    .await;
}

#[tokio::test]
async fn switching_broadcast_chat_to_automatic_and_back_takes_effect_on_the_next_post() {
    let dir = unique_temp_dir("live-broadcast-chat");
    let config: DaemonConfig = chat_config_with_playbook_file(
        &dir,
        &ChatPlaybook {
            auto_approve: &[],
            ..ChatPlaybook::default()
        },
    );
    let h = Harness::start_with_chat("live-broadcast-chat", config).await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;
    let (mut poster, _poster_id) = identified_window(&h).await;

    broadcast(&mut poster, "first broadcast, before the switch").await;
    let request: ApprovalRequest = expect_approval_requested_skipping_pushes(&mut approver).await;
    assert_eq!(request.tool, "broadcast_chat");
    decide_denied(&mut approver, &request.request_id).await;
    let reply: DaemonMessage = read_poster_reply(&mut poster).await;
    expect_error(reply, "approval_denied");

    switch_tool(&mut approver, "broadcast_chat", true).await;
    broadcast(&mut poster, "second broadcast, after switching on").await;
    let reply: DaemonMessage = read_poster_reply(&mut poster).await;
    match reply {
        DaemonMessage::ChatPosted { .. } => {}
        other => panic!("expected chat_posted without approval, got {other:?}"),
    }
    assert_eq!(
        pending_request_ids(&mut approver).await,
        Vec::<String>::new(),
        "a broadcast made while broadcast_chat is automatic must not wait for approval"
    );

    switch_tool(&mut approver, "broadcast_chat", false).await;
    broadcast(&mut poster, "third broadcast, after switching off").await;
    let request: ApprovalRequest = expect_approval_requested_skipping_pushes(&mut approver).await;
    assert_eq!(request.tool, "broadcast_chat");
    decide_denied(&mut approver, &request.request_id).await;
    let reply: DaemonMessage = read_poster_reply(&mut poster).await;
    expect_error(reply, "approval_denied");

    h.registry.terminate_all().await;
}

// --- interrupt_window ---------------------------------------------------------

async fn request_interrupt(client: &mut Connection, window_id: &str) {
    send(
        client,
        &ClientMessage::InterruptWindow {
            window_id: window_id.to_string(),
        },
    )
    .await;
}

#[tokio::test]
async fn switching_interrupt_window_to_automatic_and_back_takes_effect_on_the_next_request() {
    let (h, mut approver) =
        start_with_approver("live-interrupt-window", NO_AUTO_APPROVE_PLAYBOOK).await;
    let mut requester = h.connect().await;
    let first = h.open_idle_window(&mut requester).await;
    let second = h.open_idle_window(&mut requester).await;

    request_interrupt(&mut requester, &first).await;
    let request: ApprovalRequest = expect_approval_requested_skipping_pushes(&mut approver).await;
    assert_eq!(request.tool, "interrupt_window");
    deny(&mut approver, &mut requester, &request).await;

    switch_tool(&mut approver, "interrupt_window", true).await;
    request_interrupt(&mut requester, &first).await;
    let reply: DaemonMessage = read_response(&mut requester).await;
    match reply {
        DaemonMessage::WindowInterrupted { window_id, .. } => assert_eq!(window_id, first),
        other => panic!("expected window_interrupted without approval, got {other:?}"),
    }
    assert_eq!(
        pending_request_ids(&mut approver).await,
        Vec::<String>::new(),
        "a request made while interrupt_window is automatic must not wait for approval"
    );

    // The interrupted window may have left `idle`; the untouched one has not.
    switch_tool(&mut approver, "interrupt_window", false).await;
    request_interrupt(&mut requester, &second).await;
    let request: ApprovalRequest = expect_approval_requested_skipping_pushes(&mut approver).await;
    assert_eq!(request.tool, "interrupt_window");
    deny(&mut approver, &mut requester, &request).await;

    h.registry.terminate_all().await;
}

// --- close_window -------------------------------------------------------------

fn close_window(window_id: &str) -> ClientMessage {
    ClientMessage::CloseWindow {
        window_id: window_id.to_string(),
        force: None,
    }
}

async fn request_close(client: &mut Connection, window_id: &str) {
    send(client, &close_window(window_id)).await;
}

/// Also measures, on the first decision, which frames carry the
/// `correlation_id` of the request they answer: the decider's
/// `approval_resolved` and the requester's final answer do; the
/// `approval_requested` and `approval_withdrawn` pushes do not, nor does the
/// answer to a request sent without one.
#[tokio::test]
async fn switching_close_window_to_automatic_and_back_takes_effect_on_the_next_request() {
    let (h, mut approver) =
        start_with_approver("live-close-window", NO_AUTO_APPROVE_PLAYBOOK).await;
    let mut bystander = h.connect().await;
    let registered: DaemonMessage = correlated_request(
        &mut bystander,
        &ClientMessage::RegisterApprover,
        BYSTANDER_CORRELATION_ID,
    )
    .await;
    assert!(
        matches!(registered, DaemonMessage::ApproverRegistered { .. }),
        "expected approver_registered, got {registered:?}"
    );
    let mut requester = h.connect().await;
    let first = h.open_idle_window(&mut requester).await;
    let second = h.open_idle_window(&mut requester).await;

    send_correlated_client_message(
        &mut requester,
        &close_window(&first),
        Some(REQUESTER_CORRELATION_ID),
    )
    .await;
    let request: ApprovalRequest = expect_uncorrelated_approval_requested(&mut approver).await;
    assert_eq!(request.tool, "close_window");
    // The bystander's last request carried an id: a push that took the
    // connection's last id would show it here.
    let seen: ApprovalRequest = expect_uncorrelated_approval_requested(&mut bystander).await;
    assert_eq!(seen.request_id, request.request_id);

    let resolved: DaemonMessage = correlated_request(
        &mut approver,
        &ClientMessage::ApprovalDecision {
            request_id: request.request_id.clone(),
            approved: false,
        },
        DECIDER_CORRELATION_ID,
    )
    .await;
    match resolved {
        DaemonMessage::ApprovalResolved { request_id } => {
            assert_eq!(request_id, request.request_id);
        }
        other => panic!("expected approval_resolved, got {other:?}"),
    }
    let (correlation_id, withdrawn) = read_correlated_reply(&mut bystander).await;
    match withdrawn {
        DaemonMessage::ApprovalWithdrawn { request_id, reason } => {
            assert_eq!(request_id, request.request_id);
            assert_eq!(reason, WithdrawReason::Resolved);
        }
        other => panic!("expected approval_withdrawn, got {other:?}"),
    }
    assert_carries(
        correlation_id,
        None,
        "the approval_withdrawn push to another approver",
    );
    let (correlation_id, denied) = read_correlated_reply(&mut requester).await;
    expect_error(denied, "approval_denied");
    assert_carries(
        correlation_id,
        Some(REQUESTER_CORRELATION_ID),
        "the requester's answer after the decision",
    );

    switch_tool(&mut approver, "close_window", true).await;
    request_close(&mut requester, &first).await;
    let (correlation_id, reply) = read_correlated_reply(&mut requester).await;
    match reply {
        DaemonMessage::WindowClosed { window_id } => assert_eq!(window_id, first),
        other => panic!("expected window_closed without approval, got {other:?}"),
    }
    assert_carries(
        correlation_id,
        None,
        "the answer to a request sent without a correlation_id",
    );
    assert_eq!(
        pending_request_ids(&mut approver).await,
        Vec::<String>::new(),
        "a request made while close_window is automatic must not wait for approval"
    );

    switch_tool(&mut approver, "close_window", false).await;
    request_close(&mut requester, &second).await;
    let request: ApprovalRequest = expect_approval_requested_skipping_pushes(&mut approver).await;
    assert_eq!(request.tool, "close_window");
    deny(&mut approver, &mut requester, &request).await;

    h.registry.terminate_all().await;
}

// --- send_handoff to a profile ------------------------------------------------

const READY_PROFILE: &str = "ready";

/// `hook_child`, handed a first request at launch as `--prompt <request>`.
fn launch_profile_source(logs: &Path) -> String {
    let command: Vec<String> = vec![
        PathBuf::from(env!("CARGO_BIN_EXE_hook_child"))
            .display()
            .to_string(),
        yagura_test_support::process::COMPOSER_MARKER.to_string(),
        yagura_test_support::process::COMPOSER_SEPARATOR.to_string(),
        yagura_test_support::process::COMPOSER_CONTINUATION_PREFIX.to_string(),
        "0".to_string(),
        "-".to_string(),
        "--log".to_string(),
        logs.display().to_string(),
    ];
    format!(
        "{}{}",
        profile_source("Ready", &command),
        "\n[delivery]\nwake = \"none\"\nlaunch_prompt = [\"--prompt\", \"{prompt}\"]\n"
    )
}

/// A daemon that can open windows from [`READY_PROFILE`], booted from a playbook
/// whose `[approval]` table is `playbook`.
async fn start_profile_daemon(tag: &str, playbook: &str) -> (Harness, TempDir) {
    // Starting the window is not what these cases measure: it is auto-approved
    // on top of the playbook given.
    let playbook = playbook.replace("auto_approve = [", "auto_approve = [\"open_window\", ");
    let playbook = playbook.as_str();
    let tree = TempDir::new("yagura-cli-test", tag);
    let profiles_dir: PathBuf = tree.make_dir("profiles");
    let detection_dir: PathBuf = tree.make_dir("detection");
    let playbook_dir: PathBuf = tree.make_dir("playbook");
    write_profile(
        &profiles_dir,
        READY_PROFILE,
        &launch_profile_source(&tree.make_dir("ready-logs")),
    );
    let base: DaemonConfig = handoff_profile_config(
        Some(profiles_dir),
        empty_lookup(),
        &detection_dir,
        None,
        None,
        30,
    );
    let config: DaemonConfig = live_playbook_config(&playbook_dir, base, playbook);
    let h = Harness::start(tag, config).await;
    (h, tree)
}

/// Waits until the daemon has dropped every approver: an approver's
/// connection closing unregisters it asynchronously. Probes with a
/// `close_window` that is not automatic, followed by a `Ping`: the handler
/// refuses the close before it answers the `Ping` only when nobody could
/// approve it, and otherwise leaves the request waiting for approval, which
/// dropping the probe's connection withdraws.
async fn wait_until_no_approver_is_registered(h: &Harness, probe_window: &str) {
    let deadline = tokio::time::Instant::now() + common::HANG_GUARD;
    loop {
        let mut probe = h.connect().await;
        request_close(&mut probe, probe_window).await;
        send(&mut probe, &ClientMessage::Ping).await;
        let reply: DaemonMessage = read_response(&mut probe).await;
        if !matches!(reply, DaemonMessage::Pong) {
            let message: String = expect_error(reply, "approval_required");
            assert!(
                message.contains("no approver is connected"),
                "the probe must be refused for want of an approver, got {message:?}"
            );
            return;
        }
        drop(probe);
        assert!(
            tokio::time::Instant::now() < deadline,
            "the closed approver connection was never unregistered"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Switches `tool` on from an approver that then disconnects, leaving the
/// daemon with no approver at all.
async fn switch_on_and_leave_no_approver(h: &Harness, tool: &str) {
    let mut probe_owner = h.connect().await;
    let probe_window = h.open_idle_window(&mut probe_owner).await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;
    switch_tool(&mut approver, tool, true).await;
    drop(approver);
    wait_until_no_approver_is_registered(h, &probe_window).await;
}

#[tokio::test]
async fn a_profile_handoff_pre_launch_judgement_follows_send_handoff_switched_to_automatic() {
    let (h, _tree) = start_profile_daemon("live-handoff-pre", NO_AUTO_APPROVE_PLAYBOOK).await;
    switch_on_and_leave_no_approver(&h, "send_handoff").await;

    let mut requester = h.connect().await;
    let reply: DaemonMessage = call_send_handoff_from_profile(
        &mut requester,
        READY_PROFILE,
        None,
        None,
        "src/foo.rs",
        "look",
    )
    .await;
    let (_window_id, opened, _delivery) = expect_handoff_sent(reply);
    assert!(opened, "a profile target must report opened: true");

    h.registry.terminate_all().await;
}

#[tokio::test]
async fn a_profile_handoff_pre_launch_judgement_still_refuses_when_only_another_tool_was_switched()
{
    let (h, _tree) =
        start_profile_daemon("live-handoff-pre-control", NO_AUTO_APPROVE_PLAYBOOK).await;
    switch_on_and_leave_no_approver(&h, "interrupt_window").await;

    let mut requester = h.connect().await;
    let before: usize = h.registry.list().len();
    let reply: DaemonMessage = call_send_handoff_from_profile(
        &mut requester,
        READY_PROFILE,
        None,
        None,
        "src/foo.rs",
        "look",
    )
    .await;
    let message: String = expect_error(reply, "approval_required");
    assert!(
        message.contains("no approver is connected"),
        "message must state that no approver is connected, got {message:?}"
    );
    // Opening a handoff's window runs inside the request's handler, which
    // has returned once this connection answers a later `Ping`.
    send(&mut requester, &ClientMessage::Ping).await;
    assert!(matches!(
        read_response(&mut requester).await,
        DaemonMessage::Pong
    ));
    assert_eq!(
        h.registry.list().len(),
        before,
        "the pre-launch judgement must refuse before opening a window"
    );

    h.registry.terminate_all().await;
}

#[tokio::test]
async fn a_profile_handoff_with_an_approver_connected_is_not_refused_before_launch() {
    let (h, _tree) =
        start_profile_daemon("live-handoff-pre-approver", NO_AUTO_APPROVE_PLAYBOOK).await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;
    let mut requester = h.connect().await;

    send_handoff_from_profile(
        &mut requester,
        READY_PROFILE,
        None,
        None,
        "src/foo.rs",
        "look",
    )
    .await;

    let request: ApprovalRequest = expect_approval_requested_skipping_pushes(&mut approver).await;
    assert_eq!(request.tool, "send_handoff");
    deny(&mut approver, &mut requester, &request).await;

    h.registry.terminate_all().await;
}

// --- plans --------------------------------------------------------------------

const TASK_PROFILE: &str = "live-plan-task";

fn task(task_id: &str) -> PlanTaskSpec {
    PlanTaskSpec {
        task_id: task_id.to_string(),
        title: format!("task {task_id}"),
        role: None,
        profile: Some(TASK_PROFILE.to_string()),
        model: None,
        effort: None,
        depends_on: Vec::new(),
        writes: vec![format!("{task_id}.txt")],
        instruction: None,
    }
}

fn expect_plan_detail(reply: DaemonMessage) -> PlanInfo {
    match reply {
        DaemonMessage::PlanDetail { plan } => plan,
        other => panic!("expected plan_detail, got {other:?}"),
    }
}

async fn create_plan(owner: &mut Connection, task_id: &str) -> PlanInfo {
    send(
        owner,
        &ClientMessage::CreatePlan {
            title: format!("plan with {task_id}"),
            tasks: vec![task(task_id)],
            ticket: None,
        },
    )
    .await;
    let reply: DaemonMessage = read_approver_reply(owner).await;
    expect_plan_detail(reply)
}

async fn get_plan(client: &mut Connection, plan_id: &str) -> PlanInfo {
    send(
        client,
        &ClientMessage::GetPlan {
            plan_id: plan_id.to_string(),
        },
    )
    .await;
    let reply: DaemonMessage = read_approver_reply(client).await;
    expect_plan_detail(reply)
}

async fn switch_plan_review(approver: &mut Connection, required: bool) {
    let settings: ApprovalSettingsInfo = set_plan_approval_required(approver, required).await;
    assert_eq!(
        settings.plans_require_approval, required,
        "the answer must carry the daemon's value after the switch, got {settings:?}"
    );
}

#[tokio::test]
async fn switching_plan_review_off_and_on_applies_to_plans_created_afterwards_only() {
    let tag = "live-plan-review";
    let dir = unique_temp_dir(tag);
    let profiles_dir = dir.join("profiles");
    std::fs::create_dir_all(&profiles_dir).expect("create profiles dir");
    write_profile(
        &profiles_dir,
        TASK_PROFILE,
        &ready_child_profile_source("Live plan task"),
    );
    let mut base: DaemonConfig = detection_config(&dir, approver_base_config());
    base.store_db_path_resolved = Some(dir.join("plan.db"));
    base.profiles_dir = Some(profiles_dir);
    let config: DaemonConfig = live_playbook_config(
        &dir,
        base,
        "[approval]\nauto_approve = []\n\n[plans]\nrequire_approval = true\n",
    );
    let (h, _locks) = Harness::start_with_ledger(tag, config).await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;
    let (mut owner, _owner_id) = identified_window(&h).await;

    let before_off: PlanInfo = create_plan(&mut owner, "t1").await;
    assert_eq!(before_off.review, Some(PlanReview::Pending));

    switch_plan_review(&mut approver, false).await;
    let while_off: PlanInfo = create_plan(&mut owner, "t2").await;
    assert_eq!(
        while_off.review, None,
        "a plan created after review was switched off must not wait for review"
    );
    assert_eq!(
        get_plan(&mut owner, &before_off.plan_id).await.review,
        Some(PlanReview::Pending),
        "a plan created before the switch keeps waiting for review"
    );

    switch_plan_review(&mut approver, true).await;
    let after_on: PlanInfo = create_plan(&mut owner, "t3").await;
    assert_eq!(
        after_on.review,
        Some(PlanReview::Pending),
        "a plan created after review was switched back on must wait for review"
    );

    h.registry.terminate_all().await;
}

// --- a request already waiting for approval -----------------------------------

#[tokio::test]
async fn a_request_already_waiting_for_approval_keeps_waiting_after_its_tool_is_switched_to_automatic()
 {
    let (h, mut approver) = start_with_approver("live-pending", NO_AUTO_APPROVE_PLAYBOOK).await;
    let (mut waiting_source, _window_a) = identified_window(&h).await;
    let (mut later_source, _window_c) = identified_window(&h).await;
    let window_b = h.open_idle_window(&mut waiting_source).await;

    send_text(&mut waiting_source, &window_b, "sent before the switch").await;
    let request: ApprovalRequest = expect_approval_requested_skipping_pushes(&mut approver).await;
    assert_eq!(request.tool, "send_message");

    switch_tool(&mut approver, "send_message", true).await;

    // Control: a request made after the switch goes through at once.
    send_text(&mut later_source, &window_b, "sent after the switch").await;
    let reply: DaemonMessage = read_response(&mut later_source).await;
    expect_text_sent(reply, &window_b);

    assert_eq!(
        pending_request_ids(&mut approver).await,
        vec![request.request_id.clone()],
        "the request must still be listed as waiting for approval"
    );

    // The waiting request's only answer comes from the task that waits for its
    // decision, so the first answer `waiting_source` reads must be the denial:
    // an answer the switch had caused would come ahead of it.
    deny(&mut approver, &mut waiting_source, &request).await;
    h.registry.terminate_all().await;
}

// --- a frame that does not decode ---------------------------------------------

#[tokio::test]
async fn a_frame_that_does_not_decode_is_answered_with_the_correlation_id_it_carries() {
    let h = Harness::start("live-correlation-undecodable", common::opening_config()).await;

    // A value that is not an unsigned 64-bit integer makes the frame malformed:
    // its error names no request, and the connection closes.
    for value in ["\"42\"", "-1", "4.5", "null"] {
        let mut client = h.connect().await;
        let body = format!(r#"{{"type":"ping","correlation_id":{value}}}"#);
        send_raw_frame(&mut client, body.as_bytes()).await;
        let (correlation_id, reply) = read_correlated_reply(&mut client).await;
        expect_error(reply, "invalid_json");
        assert_carries(
            correlation_id,
            None,
            &format!("the error for a correlation_id of {value}"),
        );
        assert!(
            read_correlated_daemon_frame(&mut client).await.is_none(),
            "a correlation_id of {value} must close the connection"
        );
    }

    let mut client = h.connect().await;
    send_raw_frame(
        &mut client,
        br#"{"type":"no_such_type","correlation_id":41}"#,
    )
    .await;
    let (correlation_id, reply) = read_correlated_reply(&mut client).await;
    expect_error(reply, "unknown_type");
    assert_carries(correlation_id, Some(41), "the unknown_type error");
    send(&mut client, &ClientMessage::Ping).await;
    let (correlation_id, reply) = read_correlated_reply(&mut client).await;
    assert!(
        matches!(reply, DaemonMessage::Pong),
        "the connection must go on after unknown_type, got {reply:?}"
    );
    assert_carries(correlation_id, None, "the pong to a ping without one");

    // A known type without its required field.
    send_raw_frame(
        &mut client,
        br#"{"type":"close_window","correlation_id":42}"#,
    )
    .await;
    let (correlation_id, reply) = read_correlated_reply(&mut client).await;
    expect_error(reply, "invalid_json");
    assert_carries(correlation_id, Some(42), "the invalid_json error");
    assert!(
        read_correlated_daemon_frame(&mut client).await.is_none(),
        "invalid_json must close the connection"
    );

    h.registry.terminate_all().await;
}
