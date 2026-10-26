#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: a plan made from a ticket (`create_plan` with `ticket`),
//! driven through `Harness::start_with_ledger_and_tickets` -- a real
//! `WindowRegistry` + `IpcServer` pair whose plan ledger and ticket store
//! share one temporary SQLite file. The link must read the same from both
//! sides: the plan names its ticket, and the ticket lists its plans. Each
//! workspace is a temporary directory with a bare `.git` marker; children are
//! `echo_child`, never a real vendor CLI.

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use common::{
    Harness, approver_base_config, expect_error, identify, read_response, register_approver,
    second_workspace_dir, send, unique_temp_dir,
};
use yagura_daemon::DaemonConfig;
use yagura_protocol::{
    ClientMessage, DaemonMessage, PlanInfo, PlanSummary, PlanTaskSpec, TicketInfo,
};
use yagura_test_support::ipc::Connection;
use yagura_test_support::process::echo_child_path;

/// Fails a request that never gets its reply; not a speed bound.
const REPLY_GUARD: Duration = Duration::from_secs(60);

// --- fixtures ----------------------------------------------------------

fn echo_command() -> Vec<String> {
    vec![
        echo_child_path()
            .to_str()
            .expect("echo_child path must be valid UTF-8")
            .to_string(),
    ]
}

fn tickets_path(dir: &Path) -> PathBuf {
    dir.join("tickets.toml")
}

/// A config whose plan ledger and ticket store live in `dir`, reading the
/// ticket rules from the reference `tickets.toml` copied into `dir`.
fn link_config(dir: &Path) -> DaemonConfig {
    let reference = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tickets.toml");
    let text = std::fs::read_to_string(&reference).expect("read the reference tickets.toml");
    std::fs::write(tickets_path(dir), text).expect("write tickets.toml");
    let mut config = approver_base_config();
    config.store_db_path_resolved = Some(dir.join("yagura.db"));
    config.tickets_path = Some(tickets_path(dir));
    config
}

async fn start(tag: &str) -> Harness {
    let dir = unique_temp_dir(tag);
    let (h, _locks) = Harness::start_with_ledger_and_tickets(tag, link_config(&dir)).await;
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
    register_approver(&mut client).await;
    client
}

fn task(task_id: &str) -> PlanTaskSpec {
    PlanTaskSpec {
        task_id: task_id.to_string(),
        title: "do the thing".to_string(),
        role: None,
        profile: None,
        model: None,
        effort: None,
        depends_on: Vec::new(),
        writes: vec![format!("{task_id}.txt")],
        instruction: None,
    }
}

// --- requests ----------------------------------------------------------

/// Sends `msg` and returns the next reply, skipping the plan, ticket and chat
/// pushes an approver connection is sent while its request is in flight.
async fn request(client: &mut Connection, msg: ClientMessage) -> DaemonMessage {
    send(client, &msg).await;
    loop {
        match read_response(client, REPLY_GUARD).await {
            DaemonMessage::PlanChanged { .. }
            | DaemonMessage::TicketChanged { .. }
            | DaemonMessage::ChatChanged { .. } => continue,
            other => return other,
        }
    }
}

fn create_plan(title: &str, ticket: Option<u32>) -> ClientMessage {
    ClientMessage::CreatePlan {
        title: title.to_string(),
        tasks: vec![task("a")],
        ticket,
    }
}

fn create_ticket(title: &str) -> ClientMessage {
    ClientMessage::CreateTicket {
        title: title.to_string(),
        body: "body".to_string(),
        milestone: None,
        state: None,
        fields: None,
    }
}

fn get_ticket(ticket: u32) -> ClientMessage {
    ClientMessage::GetTicket {
        workspace_id: None,
        ticket,
    }
}

fn get_plan(plan_id: &str) -> ClientMessage {
    ClientMessage::GetPlan {
        plan_id: plan_id.to_string(),
    }
}

fn delete_plan(plan_id: &str) -> ClientMessage {
    ClientMessage::DeletePlan {
        plan_id: plan_id.to_string(),
    }
}

fn expect_plan(msg: DaemonMessage) -> PlanInfo {
    match msg {
        DaemonMessage::PlanDetail { plan } => plan,
        other => panic!("expected plan_detail, got {other:?}"),
    }
}

fn expect_plans(msg: DaemonMessage) -> Vec<PlanSummary> {
    match msg {
        DaemonMessage::PlanList { plans } => plans,
        other => panic!("expected plan_list, got {other:?}"),
    }
}

fn expect_ticket(msg: DaemonMessage) -> TicketInfo {
    match msg {
        DaemonMessage::TicketDetail { ticket } => ticket,
        other => panic!("expected ticket_detail, got {other:?}"),
    }
}

fn sorted(mut ids: Vec<String>) -> Vec<String> {
    ids.sort();
    ids
}

// --- 1 and 3. both directions of the link ------------------------------

#[tokio::test]
async fn a_plan_made_from_a_ticket_names_it_and_the_ticket_lists_every_plan_made_from_it() {
    let repo = second_workspace_dir("ipc-plan-ticket-link-repo");
    let h = start("ipc-plan-ticket-link").await;
    let mut w = window_in(&h, &repo).await;
    assert_eq!(
        expect_ticket(request(&mut w, create_ticket("first")).await).number,
        1
    );
    assert_eq!(
        expect_ticket(request(&mut w, create_ticket("second")).await).number,
        2
    );

    let first = expect_plan(request(&mut w, create_plan("from T-1", Some(1))).await);
    assert_eq!(first.ticket, Some(1));
    assert_eq!(
        expect_plan(request(&mut w, get_plan(&first.plan_id)).await).ticket,
        Some(1)
    );

    let plain = expect_plan(request(&mut w, create_plan("from no ticket", None)).await);
    assert_eq!(plain.ticket, None);
    assert_eq!(
        expect_plan(request(&mut w, get_plan(&plain.plan_id)).await).ticket,
        None
    );

    let again = expect_plan(request(&mut w, create_plan("from T-1 again", Some(1))).await);
    assert_eq!(again.ticket, Some(1));
    assert_ne!(again.plan_id, first.plan_id);

    let ticket = expect_ticket(request(&mut w, get_ticket(1)).await);
    assert_eq!(
        sorted(ticket.plans),
        sorted(vec![first.plan_id.clone(), again.plan_id.clone()]),
        "the ticket lists both plans made from it and not the plan made from none"
    );
    assert!(
        expect_ticket(request(&mut w, get_ticket(2)).await)
            .plans
            .is_empty()
    );

    let listed: Vec<String> = expect_plans(request(&mut w, ClientMessage::ListPlans).await)
        .into_iter()
        .map(|p| p.plan_id)
        .collect();
    assert_eq!(
        sorted(listed),
        sorted(vec![first.plan_id, plain.plan_id, again.plan_id])
    );
}

// --- 2. a ticket the window cannot point at ----------------------------

#[tokio::test]
async fn a_ticket_of_another_workspace_or_none_at_all_is_ticket_not_found_and_no_plan_is_made() {
    let repo_a = second_workspace_dir("ipc-plan-ticket-scope-a");
    let repo_b = second_workspace_dir("ipc-plan-ticket-scope-b");
    let h = start("ipc-plan-ticket-scope").await;
    let mut b = window_in(&h, &repo_b).await;
    expect_ticket(request(&mut b, create_ticket("b one")).await);
    assert_eq!(
        expect_ticket(request(&mut b, create_ticket("b two")).await).number,
        2
    );
    let mut a = window_in(&h, &repo_a).await;
    assert_eq!(
        expect_ticket(request(&mut a, create_ticket("a one")).await).number,
        1
    );
    let mut appr = approver(&h).await;

    let of_b = expect_error(
        request(&mut a, create_plan("points at b", Some(2))).await,
        "ticket_not_found",
    );
    let absent = expect_error(
        request(&mut a, create_plan("points at nothing", Some(999))).await,
        "ticket_not_found",
    );
    assert_eq!(
        of_b, absent,
        "another workspace's ticket must be refused exactly like a missing one"
    );

    assert!(expect_plans(request(&mut a, ClientMessage::ListPlans).await).is_empty());
    assert!(expect_plans(request(&mut appr, ClientMessage::ListPlans).await).is_empty());
    assert!(
        expect_ticket(request(&mut b, get_ticket(2)).await)
            .plans
            .is_empty()
    );
}

// --- 4. deleting a plan ------------------------------------------------

#[tokio::test]
async fn deleting_a_plan_drops_it_from_the_ticket_and_the_ticket_remains() {
    let repo = second_workspace_dir("ipc-plan-ticket-delete-repo");
    let h = start("ipc-plan-ticket-delete").await;
    let mut w = window_in(&h, &repo).await;
    expect_ticket(request(&mut w, create_ticket("kept")).await);
    let first = expect_plan(request(&mut w, create_plan("one", Some(1))).await);
    let second = expect_plan(request(&mut w, create_plan("two", Some(1))).await);

    match request(&mut w, delete_plan(&first.plan_id)).await {
        DaemonMessage::PlanDeleted { plan_id } => assert_eq!(plan_id, first.plan_id),
        other => panic!("expected plan_deleted, got {other:?}"),
    }
    let after_one = expect_ticket(request(&mut w, get_ticket(1)).await);
    assert_eq!(after_one.plans, vec![second.plan_id.clone()]);
    assert_eq!(after_one.title, "kept");

    match request(&mut w, delete_plan(&second.plan_id)).await {
        DaemonMessage::PlanDeleted { plan_id } => assert_eq!(plan_id, second.plan_id),
        other => panic!("expected plan_deleted, got {other:?}"),
    }
    let after_both = expect_ticket(request(&mut w, get_ticket(1)).await);
    assert!(after_both.plans.is_empty());
    assert_eq!(after_both.title, "kept");
}

// --- 5. a plan does not move its ticket --------------------------------

#[tokio::test]
async fn making_a_plan_from_a_ticket_leaves_its_state_fields_and_results_as_they_were() {
    let repo = second_workspace_dir("ipc-plan-ticket-state-repo");
    let h = start("ipc-plan-ticket-state").await;
    let mut w = window_in(&h, &repo).await;
    expect_ticket(request(&mut w, create_ticket("waiting to start")).await);
    let considering: BTreeMap<String, String> = [(
        "start_condition".to_string(),
        "after the release".to_string(),
    )]
    .into();
    expect_ticket(
        request(
            &mut w,
            ClientMessage::CreateTicket {
                title: "considered".to_string(),
                body: "body".to_string(),
                milestone: None,
                state: Some("considering".to_string()),
                fields: Some(considering),
            },
        )
        .await,
    );
    expect_ticket(
        request(
            &mut w,
            ClientMessage::AddTicketResult {
                ticket: 2,
                text: "measured".to_string(),
            },
        )
        .await,
    );

    for number in [1, 2] {
        let before = expect_ticket(request(&mut w, get_ticket(number)).await);
        let plan = expect_plan(request(&mut w, create_plan("work on it", Some(number))).await);
        let after = expect_ticket(request(&mut w, get_ticket(number)).await);
        assert_eq!(after.plans, vec![plan.plan_id]);
        assert_eq!(after.state, before.state, "T-{number}'s state");
        assert_eq!(after.fields, before.fields, "T-{number}'s fields");
        assert_eq!(after.results, before.results, "T-{number}'s results");
        assert_eq!(after.milestone, before.milestone, "T-{number}'s milestone");
    }
}

// --- 6. the ticket board hears of its plans ----------------------------

/// The `ticket_changed` workspaces an approver is sent up to and including
/// the one naming `sentinel`, which is left out. Plan and chat pushes are
/// skipped; anything else fails the case.
async fn ticket_pushes_until(approver: &mut Connection, sentinel: i64) -> Vec<i64> {
    let mut seen = Vec::new();
    loop {
        match read_response(approver, REPLY_GUARD).await {
            DaemonMessage::TicketChanged { workspace_id } if workspace_id == sentinel => {
                return seen;
            }
            DaemonMessage::TicketChanged { workspace_id } => seen.push(workspace_id),
            DaemonMessage::PlanChanged { .. } | DaemonMessage::ChatChanged { .. } => continue,
            other => panic!("expected ticket_changed, got {other:?}"),
        }
    }
}

/// A write in another workspace whose push marks where a step's pushes end.
async fn sentinel(
    window: &mut Connection,
    approver: &mut Connection,
    sentinel_id: i64,
) -> Vec<i64> {
    expect_ticket(request(window, create_ticket("sentinel")).await);
    ticket_pushes_until(approver, sentinel_id).await
}

#[tokio::test]
async fn making_or_deleting_a_plan_of_a_ticket_pushes_ticket_changed_and_a_plain_plan_does_not() {
    let repo_a = second_workspace_dir("ipc-plan-ticket-push-a");
    let repo_b = second_workspace_dir("ipc-plan-ticket-push-b");
    let h = start("ipc-plan-ticket-push").await;
    let mut appr = approver(&h).await;
    let mut a = window_in(&h, &repo_a).await;
    let mut b = window_in(&h, &repo_b).await;

    expect_ticket(request(&mut b, create_ticket("b one")).await);
    let id_b = match read_response(&mut appr, REPLY_GUARD).await {
        DaemonMessage::TicketChanged { workspace_id } => workspace_id,
        other => panic!("expected ticket_changed, got {other:?}"),
    };
    expect_ticket(request(&mut a, create_ticket("a one")).await);
    let pushed = sentinel(&mut b, &mut appr, id_b).await;
    assert_eq!(pushed.len(), 1, "creating a ticket pushes once: {pushed:?}");
    let id_a = pushed[0];
    assert_ne!(id_a, id_b);

    let linked = expect_plan(request(&mut a, create_plan("from T-1", Some(1))).await);
    assert_eq!(
        sentinel(&mut b, &mut appr, id_b).await,
        vec![id_a],
        "a plan made from a ticket changes that ticket's plan count"
    );

    let plain = expect_plan(request(&mut a, create_plan("from no ticket", None)).await);
    assert_eq!(
        sentinel(&mut b, &mut appr, id_b).await,
        Vec::<i64>::new(),
        "a plan made from no ticket changes no ticket"
    );

    match request(&mut a, delete_plan(&plain.plan_id)).await {
        DaemonMessage::PlanDeleted { .. } => {}
        other => panic!("expected plan_deleted, got {other:?}"),
    }
    assert_eq!(
        sentinel(&mut b, &mut appr, id_b).await,
        Vec::<i64>::new(),
        "deleting a plan of no ticket changes no ticket"
    );

    match request(&mut a, delete_plan(&linked.plan_id)).await {
        DaemonMessage::PlanDeleted { .. } => {}
        other => panic!("expected plan_deleted, got {other:?}"),
    }
    assert_eq!(
        sentinel(&mut b, &mut appr, id_b).await,
        vec![id_a],
        "deleting a plan of a ticket changes that ticket's plan count"
    );
}
