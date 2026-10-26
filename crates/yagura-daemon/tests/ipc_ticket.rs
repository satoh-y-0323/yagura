#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: the ticket board's IPC surface (`list_ticket_workspaces`
//! / `list_tickets` / `get_ticket` / `create_ticket` / `update_ticket` /
//! `add_ticket_result` / `set_milestone`), driven through
//! `Harness::start_with_ledger_and_tickets` -- a real `WindowRegistry` +
//! `IpcServer` pair whose plan ledger and ticket store share one temporary
//! SQLite file, and whose `tickets.toml` is a file the case writes and
//! rewrites between requests. Each workspace is a temporary directory with a
//! bare `.git` marker; children are `echo_child`, never a real vendor CLI.

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use common::{
    Harness, approver_base_config, expect_error, identify, read_response, register_approver,
    second_workspace_dir, send, unique_temp_dir,
};
use yagura_daemon::DaemonConfig;
use yagura_protocol::{
    ClientMessage, DaemonMessage, MilestoneInfo, PlanTaskSpec, TICKET_BODY_MAX_BYTES,
    TICKET_FIELD_VALUE_MAX_BYTES, TICKET_RESULT_MAX_BYTES, TicketConfigInfo, TicketInfo,
    TicketSummary, TicketWorkspaceInfo,
};
use yagura_test_support::ipc::Connection;
use yagura_test_support::process::echo_child_path;

/// Fails a request that never gets its reply; not a speed bound.
const REPLY_GUARD: Duration = Duration::from_secs(30);

// --- fixtures ----------------------------------------------------------

fn echo_command() -> Vec<String> {
    vec![
        echo_child_path()
            .to_str()
            .expect("echo_child path must be valid UTF-8")
            .to_string(),
    ]
}

/// A config whose plan ledger and ticket store live in `dir`, reading the
/// ticket rules from `dir/tickets.toml` (which the case writes, or not).
fn ticket_config(dir: &Path) -> DaemonConfig {
    let mut config = approver_base_config();
    config.store_db_path_resolved = Some(dir.join("yagura.db"));
    config.tickets_path = Some(tickets_path(dir));
    config
}

fn tickets_path(dir: &Path) -> PathBuf {
    dir.join("tickets.toml")
}

/// The reference `tickets.toml` shipped at the repository root.
fn reference_tickets_toml() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tickets.toml");
    std::fs::read_to_string(&path).expect("read the reference tickets.toml")
}

fn write_reference_config(dir: &Path) {
    std::fs::write(tickets_path(dir), reference_tickets_toml()).expect("write tickets.toml");
}

async fn start(tag: &str, config: DaemonConfig) -> Harness {
    let (h, _locks) = Harness::start_with_ledger_and_tickets(tag, config).await;
    h
}

/// Opens an `echo_child` window in `cwd` and identifies a new connection as it.
async fn window_in(h: &Harness, cwd: &Path) -> Connection {
    let window_id = h.open_in_cwd(echo_command(), None, cwd).await;
    let mut client = h.connect().await;
    identify(&mut client, &window_id).await;
    client
}

async fn approver(h: &Harness) -> Connection {
    let mut client = h.connect().await;
    let pending = register_approver(&mut client).await;
    assert!(
        pending.is_empty(),
        "no approval may be pending: {pending:?}"
    );
    client
}

fn now_ms() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after the epoch")
            .as_millis(),
    )
    .expect("milliseconds fit u64")
}

// --- requests ----------------------------------------------------------

/// Sends `msg` and returns the next frame that is not a view frame or status
/// push. A `ticket_changed` is *not* skipped: a window must never be sent one.
async fn request(client: &mut Connection, msg: ClientMessage) -> DaemonMessage {
    send(client, &msg).await;
    read_response(client, REPLY_GUARD).await
}

/// [`request`] on an approver connection, skipping the pushes an approver is
/// sent while its own request is in flight.
async fn approver_request(client: &mut Connection, msg: ClientMessage) -> DaemonMessage {
    send(client, &msg).await;
    loop {
        match read_response(client, REPLY_GUARD).await {
            DaemonMessage::TicketChanged { .. }
            | DaemonMessage::PlanChanged { .. }
            | DaemonMessage::ChatChanged { .. } => continue,
            other => return other,
        }
    }
}

/// The next `ticket_changed` an approver is sent, skipping plan and chat
/// pushes. Anything else (an approval request in particular) fails the case.
async fn next_ticket_changed(approver: &mut Connection) -> i64 {
    loop {
        match read_response(approver, REPLY_GUARD).await {
            DaemonMessage::TicketChanged { workspace_id } => return workspace_id,
            DaemonMessage::PlanChanged { .. } | DaemonMessage::ChatChanged { .. } => continue,
            other => panic!("expected ticket_changed, got {other:?}"),
        }
    }
}

fn create(title: &str, body: &str) -> ClientMessage {
    ClientMessage::CreateTicket {
        title: title.to_string(),
        body: body.to_string(),
        milestone: None,
        state: None,
        fields: None,
    }
}

fn create_in(title: &str, state: &str, fields: &[(&str, &str)]) -> ClientMessage {
    ClientMessage::CreateTicket {
        title: title.to_string(),
        body: "body".to_string(),
        milestone: None,
        state: Some(state.to_string()),
        fields: Some(field_map(fields)),
    }
}

fn update(ticket: u32, state: Option<&str>, fields: &[(&str, &str)]) -> ClientMessage {
    ClientMessage::UpdateTicket {
        ticket,
        title: None,
        body: None,
        milestone: None,
        state: state.map(str::to_string),
        fields: (!fields.is_empty()).then(|| field_map(fields)),
    }
}

fn field_map(fields: &[(&str, &str)]) -> BTreeMap<String, String> {
    fields
        .iter()
        .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
        .collect()
}

fn get(ticket: u32) -> ClientMessage {
    ClientMessage::GetTicket {
        workspace_id: None,
        ticket,
    }
}

fn get_in(workspace_id: i64, ticket: u32) -> ClientMessage {
    ClientMessage::GetTicket {
        workspace_id: Some(workspace_id),
        ticket,
    }
}

fn list() -> ClientMessage {
    list_limited(None)
}

fn list_limited(limit: Option<u32>) -> ClientMessage {
    ClientMessage::ListTickets {
        workspace_id: None,
        state: None,
        milestone: None,
        limit,
    }
}

fn list_in(workspace_id: i64) -> ClientMessage {
    ClientMessage::ListTickets {
        workspace_id: Some(workspace_id),
        state: None,
        milestone: None,
        limit: None,
    }
}

fn add_result(ticket: u32, text: &str) -> ClientMessage {
    ClientMessage::AddTicketResult {
        ticket,
        text: text.to_string(),
    }
}

fn set_milestone(name: &str, title: &str) -> ClientMessage {
    ClientMessage::SetMilestone {
        name: name.to_string(),
        title: title.to_string(),
        description: None,
        position: 1,
    }
}

fn expect_detail(msg: DaemonMessage) -> TicketInfo {
    match msg {
        DaemonMessage::TicketDetail { ticket } => ticket,
        other => panic!("expected ticket_detail, got {other:?}"),
    }
}

fn expect_milestone(msg: DaemonMessage) -> MilestoneInfo {
    match msg {
        DaemonMessage::MilestoneSet { milestone } => milestone,
        other => panic!("expected milestone_set, got {other:?}"),
    }
}

struct Listing {
    config: Option<TicketConfigInfo>,
    config_error: Option<String>,
    tickets: Vec<TicketSummary>,
    truncated: bool,
}

fn expect_list(msg: DaemonMessage) -> Listing {
    match msg {
        DaemonMessage::TicketList {
            config,
            config_error,
            tickets,
            truncated,
            ..
        } => Listing {
            config,
            config_error,
            tickets,
            truncated,
        },
        other => panic!("expected ticket_list, got {other:?}"),
    }
}

fn expect_workspaces(msg: DaemonMessage) -> Vec<TicketWorkspaceInfo> {
    match msg {
        DaemonMessage::TicketWorkspaceList { workspaces } => workspaces,
        other => panic!("expected ticket_workspace_list, got {other:?}"),
    }
}

/// The id of the listed workspace rooted at `dir` (matched by its unique
/// directory name, since the daemon reports the canonical path).
fn workspace_id_of(workspaces: &[TicketWorkspaceInfo], dir: &Path) -> i64 {
    let name = dir.file_name().expect("a named temporary directory");
    workspaces
        .iter()
        .find(|info| Path::new(&info.path).file_name() == Some(name))
        .unwrap_or_else(|| panic!("no workspace for {}: {workspaces:?}", dir.display()))
        .workspace_id
}

// --- 1. workspace scope ------------------------------------------------

#[tokio::test]
async fn a_ticket_belongs_to_the_window_repository_and_another_repository_cannot_see_it() {
    let dir = unique_temp_dir("ipc-ticket-scope");
    write_reference_config(&dir);
    let repo_a = second_workspace_dir("ipc-ticket-scope-a");
    let repo_b = second_workspace_dir("ipc-ticket-scope-b");
    let sub_a = repo_a.join("nested").join("deeper");
    std::fs::create_dir_all(&sub_a).expect("create a subdirectory of repository A");
    let h = start("ipc-ticket-scope", ticket_config(&dir)).await;

    let mut a = window_in(&h, &repo_a).await;
    let created = expect_detail(request(&mut a, create("first in a", "body of a")).await);
    assert_eq!(created.number, 1);

    let mut a_sub = window_in(&h, &sub_a).await;
    let listed = expect_list(request(&mut a_sub, list()).await);
    let numbers: Vec<u32> = listed.tickets.iter().map(|t| t.number).collect();
    assert_eq!(numbers, vec![1]);
    let seen = expect_detail(request(&mut a_sub, get(1)).await);
    assert_eq!(seen.title, "first in a");
    assert_eq!(seen.body, "body of a");

    let mut b = window_in(&h, &repo_b).await;
    let from_b = expect_error(request(&mut b, get(1)).await, "ticket_not_found");
    let absent_in_b = expect_error(request(&mut b, get(999)).await, "ticket_not_found");
    let absent_in_a = expect_error(request(&mut a, get(999)).await, "ticket_not_found");
    assert_eq!(from_b, absent_in_b);
    assert_eq!(from_b, absent_in_a);
    let update_from_b = expect_error(
        request(&mut b, update(1, Some("in-progress"), &[])).await,
        "ticket_not_found",
    );
    assert_eq!(update_from_b, absent_in_a);
    assert!(
        expect_list(request(&mut b, list()).await)
            .tickets
            .is_empty()
    );

    let in_b = expect_detail(request(&mut b, create("first in b", "body of b")).await);
    assert_eq!(in_b.number, 1, "numbering is per workspace");
    let still_a = expect_detail(request(&mut a, get(1)).await);
    assert_eq!(still_a.title, "first in a");
}

// --- 2. who may read and write -----------------------------------------

#[tokio::test]
async fn external_is_refused_and_an_approver_reads_every_workspace_but_never_writes() {
    let dir = unique_temp_dir("ipc-ticket-origin");
    write_reference_config(&dir);
    let repo_a = second_workspace_dir("ipc-ticket-origin-a");
    let repo_b = second_workspace_dir("ipc-ticket-origin-b");
    let h = start("ipc-ticket-origin", ticket_config(&dir)).await;

    let mut a = window_in(&h, &repo_a).await;
    expect_detail(request(&mut a, create("ticket of a", "body")).await);
    let mut b = window_in(&h, &repo_b).await;
    expect_detail(request(&mut b, create("ticket of b", "body")).await);

    let mut external = h.connect().await;
    for msg in [
        ClientMessage::ListTicketWorkspaces,
        list(),
        list_in(1),
        get(1),
        get_in(1, 1),
        create("t", "b"),
        update(1, Some("in-progress"), &[]),
        add_result(1, "r"),
        set_milestone("m1", "M1"),
    ] {
        let label = format!("{msg:?}");
        match request(&mut external, msg).await {
            DaemonMessage::Error { code, .. } => {
                assert_eq!(code, "auth_failed", "external {label}");
            }
            other => panic!("external {label}: expected auth_failed, got {other:?}"),
        }
    }

    let mut appr = approver(&h).await;
    let workspaces =
        expect_workspaces(approver_request(&mut appr, ClientMessage::ListTicketWorkspaces).await);
    let id_a = workspace_id_of(&workspaces, &repo_a);
    let id_b = workspace_id_of(&workspaces, &repo_b);
    assert_ne!(id_a, id_b);
    assert!(workspaces.iter().all(|w| w.kind == "git"), "{workspaces:?}");

    let listed_a = expect_list(approver_request(&mut appr, list_in(id_a)).await);
    assert_eq!(listed_a.tickets.len(), 1);
    assert_eq!(listed_a.tickets[0].title, "ticket of a");
    let listed_b = expect_list(approver_request(&mut appr, list_in(id_b)).await);
    assert_eq!(listed_b.tickets[0].title, "ticket of b");
    assert_eq!(
        expect_detail(approver_request(&mut appr, get_in(id_a, 1)).await).title,
        "ticket of a"
    );
    assert_eq!(
        expect_detail(approver_request(&mut appr, get_in(id_b, 1)).await).title,
        "ticket of b"
    );

    for msg in [
        create("t", "b"),
        update(1, Some("in-progress"), &[]),
        add_result(1, "r"),
        set_milestone("m1", "M1"),
    ] {
        let label = format!("{msg:?}");
        match approver_request(&mut appr, msg).await {
            DaemonMessage::Error { code, .. } => {
                assert_eq!(code, "auth_failed", "approver {label}");
            }
            other => panic!("approver {label}: expected auth_failed, got {other:?}"),
        }
    }
    assert_eq!(
        expect_detail(approver_request(&mut appr, get_in(id_a, 1)).await).state,
        "todo",
        "the approver's refused update must not have changed the ticket"
    );

    // A window reads only its own workspace and may not name one.
    expect_error(request(&mut a, list_in(id_b)).await, "invalid_argument");
    expect_error(request(&mut a, get_in(id_b, 1)).await, "invalid_argument");
    expect_error(request(&mut a, list_in(id_a)).await, "invalid_argument");
}

// --- 3. settings file missing, broken, and rewritten -------------------

#[tokio::test]
async fn missing_or_broken_tickets_toml_refuses_writes_keeps_reads_and_rewrites_apply_live() {
    let dir = unique_temp_dir("ipc-ticket-config-file");
    let repo = second_workspace_dir("ipc-ticket-config-file-a");
    let h = start("ipc-ticket-config-file", ticket_config(&dir)).await;
    let mut a = window_in(&h, &repo).await;

    expect_error(
        request(&mut a, create("t", "b")).await,
        "tickets_not_configured",
    );
    let unconfigured = expect_list(request(&mut a, list()).await);
    assert!(unconfigured.config.is_none());
    assert!(
        unconfigured
            .config_error
            .as_deref()
            .is_some_and(|reason| !reason.is_empty()),
        "a listing without settings names why"
    );
    assert!(unconfigured.tickets.is_empty());

    write_reference_config(&dir);
    let created = expect_detail(request(&mut a, create("made after the file", "b")).await);
    assert_eq!(created.number, 1);
    let configured = expect_list(request(&mut a, list()).await);
    assert!(configured.config_error.is_none());
    let config = configured
        .config
        .expect("settings are read once the file exists");
    assert_eq!(config.states[0].name, "todo");

    let leak = "zq7filecontent";
    std::fs::write(
        tickets_path(&dir),
        format!("schema = 1\n[[state]\nname = \"{leak}\"\n"),
    )
    .expect("write a broken tickets.toml");
    expect_error(
        request(&mut a, create("t", "b")).await,
        "tickets_config_invalid",
    );
    expect_error(
        request(&mut a, update(1, Some("in-progress"), &[])).await,
        "tickets_config_invalid",
    );
    let broken = expect_list(request(&mut a, list()).await);
    assert!(broken.config.is_none());
    let reason = broken
        .config_error
        .expect("a listing with broken settings names why");
    assert!(!reason.is_empty());
    assert!(
        !reason.contains(leak),
        "the reason quoted the file: {reason}"
    );
    let numbers: Vec<u32> = broken.tickets.iter().map(|t| t.number).collect();
    assert_eq!(numbers, vec![1], "tickets are still listed");
    assert_eq!(
        expect_detail(request(&mut a, get(1)).await).state,
        "todo",
        "nothing changed while the settings were broken"
    );

    std::fs::remove_file(tickets_path(&dir)).expect("remove tickets.toml");
    expect_error(
        request(&mut a, update(1, Some("in-progress"), &[])).await,
        "tickets_not_configured",
    );
    assert_eq!(
        expect_detail(request(&mut a, get(1)).await).title,
        "made after the file"
    );

    write_reference_config(&dir);
    let moved = expect_detail(request(&mut a, update(1, Some("in-progress"), &[])).await);
    assert_eq!(moved.state, "in-progress");
}

// --- 4. required fields of the reference settings ----------------------

#[tokio::test]
async fn reference_settings_require_the_state_fields_and_a_refusal_writes_nothing() {
    let dir = unique_temp_dir("ipc-ticket-required");
    write_reference_config(&dir);
    let repo = second_workspace_dir("ipc-ticket-required-a");
    let h = start("ipc-ticket-required", ticket_config(&dir)).await;
    let mut a = window_in(&h, &repo).await;

    let created = expect_detail(request(&mut a, create("pause me", "b")).await);
    assert_eq!(created.state, "todo", "no state given starts in the first");
    let before = expect_detail(request(&mut a, get(created.number)).await);

    let message = expect_error(
        request(&mut a, update(created.number, Some("on-hold"), &[])).await,
        "invalid_argument",
    );
    assert!(
        message.contains("resume_trigger"),
        "names the missing field: {message}"
    );
    assert_eq!(
        expect_detail(request(&mut a, get(created.number)).await),
        before,
        "the refused update changed the ticket"
    );

    let held = expect_detail(
        request(
            &mut a,
            update(
                created.number,
                Some("on-hold"),
                &[("resume_trigger", "after the release")],
            ),
        )
        .await,
    );
    assert_eq!(held.state, "on-hold");
    assert_eq!(
        held.fields.get("resume_trigger").map(String::as_str),
        Some("after the release")
    );

    // Clearing the required field while staying in the state is checked too.
    let message = expect_error(
        request(
            &mut a,
            update(created.number, None, &[("resume_trigger", "")]),
        )
        .await,
        "invalid_argument",
    );
    assert!(message.contains("resume_trigger"), "{message}");
    assert_eq!(
        expect_detail(request(&mut a, get(created.number)).await),
        held
    );

    let message = expect_error(
        request(&mut a, create_in("never", "cancelled", &[])).await,
        "invalid_argument",
    );
    assert!(
        message.contains("reason"),
        "names the missing field: {message}"
    );
    let numbers: Vec<u32> = expect_list(request(&mut a, list()).await)
        .tickets
        .iter()
        .map(|t| t.number)
        .collect();
    assert_eq!(
        numbers,
        vec![created.number],
        "the refused create wrote a ticket"
    );

    let cancelled = expect_detail(
        request(
            &mut a,
            create_in("dropped", "cancelled", &[("reason", "out of scope")]),
        )
        .await,
    );
    assert_eq!(cancelled.state, "cancelled");
}

// --- 5. names the settings do not declare ------------------------------

#[tokio::test]
async fn an_undeclared_state_or_field_is_invalid_argument() {
    let dir = unique_temp_dir("ipc-ticket-undeclared");
    write_reference_config(&dir);
    let repo = second_workspace_dir("ipc-ticket-undeclared-a");
    let h = start("ipc-ticket-undeclared", ticket_config(&dir)).await;
    let mut a = window_in(&h, &repo).await;

    expect_error(
        request(&mut a, create_in("t", "no-such-state", &[])).await,
        "invalid_argument",
    );
    expect_error(
        request(&mut a, create_in("t", "todo", &[("no_such_field", "v")])).await,
        "invalid_argument",
    );
    let created = expect_detail(request(&mut a, create("t", "b")).await);
    expect_error(
        request(&mut a, update(created.number, Some("no-such-state"), &[])).await,
        "invalid_argument",
    );
    expect_error(
        request(
            &mut a,
            update(created.number, None, &[("no_such_field", "v")]),
        )
        .await,
        "invalid_argument",
    );
    assert_eq!(
        expect_detail(request(&mut a, get(created.number)).await),
        created
    );
}

// --- 6. results -------------------------------------------------------

#[tokio::test]
async fn results_are_appended_in_order_with_the_daemon_clock() {
    let dir = unique_temp_dir("ipc-ticket-results");
    write_reference_config(&dir);
    let repo = second_workspace_dir("ipc-ticket-results-a");
    let h = start("ipc-ticket-results", ticket_config(&dir)).await;
    let mut a = window_in(&h, &repo).await;
    let created = expect_detail(request(&mut a, create("t", "b")).await);
    assert!(created.results.is_empty());

    let before = now_ms();
    expect_detail(request(&mut a, add_result(created.number, "measured 12 ms")).await);
    let after_second = expect_detail(request(&mut a, add_result(created.number, "merged")).await);
    let after = now_ms();

    let read = expect_detail(request(&mut a, get(created.number)).await);
    assert_eq!(read.results, after_second.results);
    let texts: Vec<&str> = read.results.iter().map(|r| r.text.as_str()).collect();
    assert_eq!(texts, vec!["measured 12 ms", "merged"]);
    let seqs: Vec<u64> = read.results.iter().map(|r| r.seq).collect();
    assert_eq!(seqs, vec![1, 2]);
    for result in &read.results {
        assert!(
            (before..=after).contains(&result.at),
            "result time {} outside the request window {before}..={after}",
            result.at
        );
    }
    assert!(read.results[0].at <= read.results[1].at);
}

// --- 7. ticket_changed -------------------------------------------------

#[tokio::test]
async fn every_write_pushes_ticket_changed_to_approvers_and_never_to_windows() {
    let dir = unique_temp_dir("ipc-ticket-push");
    write_reference_config(&dir);
    let repo_a = second_workspace_dir("ipc-ticket-push-a");
    let repo_b = second_workspace_dir("ipc-ticket-push-b");
    let h = start("ipc-ticket-push", ticket_config(&dir)).await;
    let mut appr = approver(&h).await;
    let mut a = window_in(&h, &repo_a).await;
    let mut b = window_in(&h, &repo_b).await;

    let created = expect_detail(request(&mut a, create("t", "b")).await);
    let id_a = next_ticket_changed(&mut appr).await;
    expect_detail(request(&mut a, update(created.number, Some("in-progress"), &[])).await);
    assert_eq!(next_ticket_changed(&mut appr).await, id_a);
    expect_detail(request(&mut a, add_result(created.number, "r")).await);
    assert_eq!(next_ticket_changed(&mut appr).await, id_a);
    expect_milestone(request(&mut a, set_milestone("m1", "Milestone one")).await);
    assert_eq!(next_ticket_changed(&mut appr).await, id_a);

    expect_detail(request(&mut b, create("t", "b")).await);
    let id_b = next_ticket_changed(&mut appr).await;
    assert_ne!(id_a, id_b, "the push names the workspace that changed");

    let workspaces =
        expect_workspaces(approver_request(&mut appr, ClientMessage::ListTicketWorkspaces).await);
    assert_eq!(workspace_id_of(&workspaces, &repo_a), id_a);
    assert_eq!(workspace_id_of(&workspaces, &repo_b), id_b);

    // A window's next reply is its own answer, not a queued push.
    expect_list(request(&mut a, list()).await);
    expect_list(request(&mut b, list()).await);
}

// --- 8. refusals do not echo -------------------------------------------

#[tokio::test]
async fn refusals_never_quote_the_callers_text() {
    let dir = unique_temp_dir("ipc-ticket-echo");
    write_reference_config(&dir);
    let repo_a = second_workspace_dir("ipc-ticket-echo-a");
    let repo_b = second_workspace_dir("ipc-ticket-echo-b");
    let h = start("ipc-ticket-echo", ticket_config(&dir)).await;
    let mut a = window_in(&h, &repo_a).await;
    let mut b = window_in(&h, &repo_b).await;
    let created = expect_detail(request(&mut a, create("t", "b")).await);

    let title = "zq7title";
    let body = "zq7body";
    let value = "zq7value";
    let name = "zq7name";
    let refusals = [
        ClientMessage::CreateTicket {
            title: title.to_string(),
            body: body.to_string(),
            milestone: None,
            state: Some(name.to_string()),
            fields: Some(field_map(&[("reason", value)])),
        },
        ClientMessage::CreateTicket {
            title: title.to_string(),
            body: body.to_string(),
            milestone: None,
            state: None,
            fields: Some(field_map(&[(name, value)])),
        },
        ClientMessage::CreateTicket {
            title: format!("{title}\u{7}"),
            body: body.to_string(),
            milestone: None,
            state: None,
            fields: None,
        },
        ClientMessage::CreateTicket {
            title: title.to_string(),
            body: body.repeat(TICKET_BODY_MAX_BYTES / body.len() + 1),
            milestone: None,
            state: None,
            fields: None,
        },
        ClientMessage::CreateTicket {
            title: title.to_string(),
            body: body.to_string(),
            milestone: Some(name.to_string()),
            state: None,
            fields: None,
        },
        ClientMessage::UpdateTicket {
            ticket: created.number,
            title: Some(title.to_string()),
            body: Some(body.to_string()),
            milestone: None,
            state: Some("on-hold".to_string()),
            fields: Some(field_map(&[("reason", value)])),
        },
        ClientMessage::UpdateTicket {
            ticket: created.number,
            title: None,
            body: None,
            milestone: None,
            state: None,
            fields: Some(field_map(&[("reason", &value.repeat(1024))])),
        },
        add_result(999, value),
        set_milestone(&format!("{name} Bad"), title),
    ];
    for msg in refusals {
        let label = format!("{msg:?}");
        match request(&mut a, msg).await {
            DaemonMessage::Error { message, .. } => {
                for leak in [title, body, value, name] {
                    assert!(
                        !message.contains(leak),
                        "{label} refused with a message quoting {leak}: {message}"
                    );
                }
            }
            other => panic!("{label}: expected a refusal, got {other:?}"),
        }
    }
    let message = expect_error(
        request(
            &mut b,
            ClientMessage::UpdateTicket {
                ticket: created.number,
                title: Some(title.to_string()),
                body: Some(body.to_string()),
                milestone: None,
                state: None,
                fields: Some(field_map(&[("reason", value)])),
            },
        )
        .await,
        "ticket_not_found",
    );
    for leak in [title, body, value] {
        assert!(!message.contains(leak), "{message}");
    }
    assert_eq!(
        expect_detail(request(&mut a, get(created.number)).await),
        created
    );
}

// --- 9. size and listing limits ----------------------------------------

#[tokio::test]
async fn text_sizes_and_listing_limits_are_bounded() {
    let dir = unique_temp_dir("ipc-ticket-limits");
    write_reference_config(&dir);
    let repo = second_workspace_dir("ipc-ticket-limits-a");
    let h = start("ipc-ticket-limits", ticket_config(&dir)).await;
    let mut a = window_in(&h, &repo).await;

    let at_body = "b".repeat(TICKET_BODY_MAX_BYTES);
    let first = expect_detail(request(&mut a, create("max body", &at_body)).await);
    assert_eq!(first.body.len(), TICKET_BODY_MAX_BYTES);
    let over_body = "b".repeat(TICKET_BODY_MAX_BYTES + 1);
    expect_error(
        request(&mut a, create("long body", &over_body)).await,
        "invalid_argument",
    );

    let at_value = "v".repeat(TICKET_FIELD_VALUE_MAX_BYTES);
    expect_detail(
        request(
            &mut a,
            update(first.number, None, &[("start_condition", &at_value)]),
        )
        .await,
    );
    let over_value = "v".repeat(TICKET_FIELD_VALUE_MAX_BYTES + 1);
    expect_error(
        request(
            &mut a,
            update(first.number, None, &[("start_condition", &over_value)]),
        )
        .await,
        "invalid_argument",
    );

    let at_result = "r".repeat(TICKET_RESULT_MAX_BYTES);
    expect_detail(request(&mut a, add_result(first.number, &at_result)).await);
    let over_result = "r".repeat(TICKET_RESULT_MAX_BYTES + 1);
    expect_error(
        request(&mut a, add_result(first.number, &over_result)).await,
        "invalid_argument",
    );
    let stored = expect_detail(request(&mut a, get(first.number)).await);
    assert_eq!(stored.results.len(), 1, "the refused result was stored");
    assert_eq!(
        stored.fields.get("start_condition").map(String::len),
        Some(TICKET_FIELD_VALUE_MAX_BYTES),
        "the refused field value was stored"
    );

    for n in 2..=201 {
        expect_detail(request(&mut a, create(&format!("ticket {n}"), "b")).await);
    }
    let by_default = expect_list(request(&mut a, list()).await);
    assert_eq!(by_default.tickets.len(), 200);
    assert!(by_default.truncated);
    let at_max = expect_list(request(&mut a, list_limited(Some(1_000))).await);
    assert_eq!(at_max.tickets.len(), 201);
    assert!(!at_max.truncated);
    expect_error(
        request(&mut a, list_limited(Some(1_001))).await,
        "invalid_argument",
    );
}

// --- 10. no approval for ticket writes ---------------------------------

#[tokio::test]
async fn ticket_writes_never_ask_for_approval_even_when_the_playbook_approves_nothing() {
    let dir = unique_temp_dir("ipc-ticket-no-approval");
    write_reference_config(&dir);
    let repo = second_workspace_dir("ipc-ticket-no-approval-a");
    let mut config = ticket_config(&dir);
    config.playbook = None;
    config.approval_policy = None;
    let h = start("ipc-ticket-no-approval", config).await;
    let mut appr = approver(&h).await;
    let mut a = window_in(&h, &repo).await;

    // The approver's next frame after each write is the push, never a request.
    send(&mut a, &create("t", "b")).await;
    next_ticket_changed(&mut appr).await;
    let created = expect_detail(read_response(&mut a, REPLY_GUARD).await);
    send(&mut a, &update(created.number, Some("in-progress"), &[])).await;
    next_ticket_changed(&mut appr).await;
    expect_detail(read_response(&mut a, REPLY_GUARD).await);
    send(&mut a, &add_result(created.number, "r")).await;
    next_ticket_changed(&mut appr).await;
    expect_detail(read_response(&mut a, REPLY_GUARD).await);
    send(&mut a, &set_milestone("m1", "Milestone one")).await;
    next_ticket_changed(&mut appr).await;
    expect_milestone(read_response(&mut a, REPLY_GUARD).await);
}

// --- 11. what a summary carries for the board card ---------------------

fn create_plan_from(ticket: u32) -> ClientMessage {
    ClientMessage::CreatePlan {
        title: format!("from T-{ticket}"),
        tasks: vec![PlanTaskSpec {
            task_id: "a".to_string(),
            title: "do the thing".to_string(),
            role: None,
            profile: None,
            model: None,
            effort: None,
            depends_on: Vec::new(),
            writes: vec!["a.txt".to_string()],
            instruction: None,
        }],
        ticket: Some(ticket),
    }
}

/// The summary of `number` as it travels on the wire.
fn summary_wire(listing: &Listing, number: u32) -> serde_json::Value {
    let summary = listing
        .tickets
        .iter()
        .find(|t| t.number == number)
        .unwrap_or_else(|| panic!("no summary for T-{number}"));
    serde_json::to_value(summary).expect("a summary encodes")
}

#[tokio::test]
async fn a_summary_carries_only_its_states_required_field_values_and_its_plan_count() {
    let dir = unique_temp_dir("ipc-ticket-summary");
    write_reference_config(&dir);
    let repo = second_workspace_dir("ipc-ticket-summary-a");
    let h = start("ipc-ticket-summary", ticket_config(&dir)).await;
    let mut a = window_in(&h, &repo).await;

    let held = expect_detail(
        request(
            &mut a,
            create_in(
                "paused",
                "on-hold",
                &[("resume_trigger", "after the release"), ("reason", "kept")],
            ),
        )
        .await,
    );
    let waiting = expect_detail(
        request(
            &mut a,
            create_in("queued", "todo", &[("start_condition", "the wire lands")]),
        )
        .await,
    );
    for _ in 0..2 {
        match approver_request(&mut a, create_plan_from(waiting.number)).await {
            DaemonMessage::PlanDetail { .. } => {}
            other => panic!("expected plan_detail, got {other:?}"),
        }
    }

    let listing = expect_list(request(&mut a, list()).await);
    let held_wire = summary_wire(&listing, held.number);
    let waiting_wire = summary_wire(&listing, waiting.number);

    assert_eq!(
        held_wire["fields"],
        serde_json::json!({ "resume_trigger": "after the release" }),
        "an on-hold summary carries its state's required field and nothing else"
    );
    assert_eq!(
        waiting_wire["fields"],
        serde_json::json!({}),
        "a todo summary carries no field values, even ones the ticket has"
    );
    assert_eq!(
        waiting_wire["plan_count"],
        serde_json::json!(2),
        "two plans were made from the ticket"
    );
    assert_eq!(held_wire["plan_count"], serde_json::json!(0));
}

// --- 12. a summary's field values are card excerpts --------------------

/// Code points the board card shows of a field value in a summary.
const SUMMARY_FIELD_EXCERPT_CHARS: usize = 256;

#[tokio::test]
async fn a_summary_cuts_a_long_field_value_to_an_excerpt_and_get_ticket_keeps_it_whole() {
    let dir = unique_temp_dir("ipc-ticket-excerpt");
    write_reference_config(&dir);
    let repo = second_workspace_dir("ipc-ticket-excerpt-a");
    let h = start("ipc-ticket-excerpt", ticket_config(&dir)).await;
    let mut a = window_in(&h, &repo).await;

    // A four-byte code point, so a cut by bytes or UTF-16 units lands elsewhere.
    let at_limit = "\u{1D11E}".repeat(SUMMARY_FIELD_EXCERPT_CHARS);
    let one_over = "\u{1D11E}".repeat(SUMMARY_FIELD_EXCERPT_CHARS + 1);
    let well_over = format!(
        "{}{}",
        "\u{3042}".repeat(SUMMARY_FIELD_EXCERPT_CHARS),
        "z".repeat(200)
    );
    let excerpt_of_one_over = format!("{}\u{2026}", at_limit);
    let excerpt_of_well_over =
        format!("{}\u{2026}", "\u{3042}".repeat(SUMMARY_FIELD_EXCERPT_CHARS));

    let mut numbers = Vec::new();
    for value in [&at_limit, &one_over, &well_over] {
        let ticket = expect_detail(
            request(
                &mut a,
                create_in("paused", "on-hold", &[("resume_trigger", value)]),
            )
            .await,
        );
        numbers.push(ticket.number);
    }

    let listing = expect_list(request(&mut a, list()).await);
    let expected = [&at_limit, &excerpt_of_one_over, &excerpt_of_well_over];
    for (number, excerpt) in numbers.iter().zip(expected) {
        assert_eq!(
            summary_wire(&listing, *number)["fields"],
            serde_json::json!({ "resume_trigger": excerpt }),
            "T-{number}'s summary carries the first {SUMMARY_FIELD_EXCERPT_CHARS} code points"
        );
    }

    for (number, whole) in numbers.iter().zip([&at_limit, &one_over, &well_over]) {
        let detail = expect_detail(request(&mut a, get(*number)).await);
        assert_eq!(
            detail.fields.get("resume_trigger"),
            Some(whole),
            "T-{number}'s get_ticket keeps the whole value"
        );
    }
}
