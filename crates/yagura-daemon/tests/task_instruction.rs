#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// Seeds and reads the database with raw SQL, which only tests may do.
#![allow(clippy::disallowed_methods)]

//! Integration test: a plan task's `instruction` on the plan ledger -- stored by
//! `create_plan`, replaced or removed by `update_plan`, validated by
//! `yagura_protocol::validate_task_instruction`, and read back by `get_plan`
//! (the owner, an approver, and a window bound to the task by
//! `open_task_window`). Driven through `Harness::start_with_ledger`; children
//! are `echo_child` or `ready_child` via a launch profile, never a real vendor
//! CLI.
//!
//! Every task that gets a window here carries no instruction at the moment
//! `open_task_window` runs: delivering an instruction as a notice is a
//! separate contract, and this file only measures the ledger.

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use common::{
    Harness, approver_base_config, expect_error, identify, read_response, register_approver, send,
    unique_temp_dir, write_profile,
};
use yagura_daemon::DaemonConfig;
use yagura_daemon::plan_permit::LockTable;
use yagura_daemon::store;
use yagura_protocol::{
    ClientMessage, DaemonMessage, PlanInfo, PlanTaskInfo, PlanTaskSpec, ReportOutcome,
    TASK_INSTRUCTION_MAX_BYTES,
};
use yagura_test_support::ipc::Connection;
use yagura_test_support::process::{echo_child_command, ready_child_profile_source};

// --- fixtures ----------------------------------------------------------

const PROFILE_ID: &str = "instruction-profile";

/// Multi-line prose with a TAB and non-ASCII text: the byte-exact round trip
/// must keep every LF and TAB and every UTF-8 byte.
const INSTRUCTION_A: &str =
    "Step 1:\tread src/plan.rs\nStep 2:\twrite a test for caf\u{e9} \u{2014} na\u{ef}ve\n\nDone.";
const INSTRUCTION_B: &str = "Second task.\n\tIndented line\nlast line without a terminator";
const INSTRUCTION_C: &str = "Replaced instruction for t1.";

fn ledger_config(dir: &Path, profiles_dir: Option<PathBuf>, auto_approve: &[&str]) -> DaemonConfig {
    let mut config = approver_base_config();
    config.approval_policy = Some(common::policy_opening(auto_approve));
    config.store_db_path_resolved = Some(dir.join("plan.db"));
    config.profiles_dir = profiles_dir;
    // This file measures instruction delivery, not the review gate: the
    // plan created here must not start pending.
    config.plan_approval_required = false;
    config
}

/// A config with a `ready_child` launch profile under `dir`, for the cases
/// that bind a task to a window.
fn ledger_config_with_profile(dir: &Path) -> DaemonConfig {
    let profiles_dir = dir.join("profiles");
    std::fs::create_dir_all(&profiles_dir).expect("create profiles dir");
    write_profile(
        &profiles_dir,
        PROFILE_ID,
        &ready_child_profile_source("Instruction Profile"),
    );
    ledger_config(dir, Some(profiles_dir), &[])
}

async fn start_ledger(tag: &str, config: DaemonConfig) -> (Harness, Arc<LockTable>) {
    Harness::start_with_ledger(tag, config).await
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

fn task_with_instruction(task_id: &str, instruction: &str) -> PlanTaskSpec {
    PlanTaskSpec {
        instruction: Some(instruction.to_string()),
        ..task(task_id)
    }
}

fn with_profile(spec: PlanTaskSpec) -> PlanTaskSpec {
    PlanTaskSpec {
        profile: Some(PROFILE_ID.to_string()),
        ..spec
    }
}

// --- connection helpers ------------------------------------------------

async fn open_and_identify(h: &Harness, client: &mut Connection) -> String {
    let window_id = h.open(echo_child_command(), None).await;
    identify(client, &window_id).await;
    window_id
}

async fn approver_connection(h: &Harness) -> Connection {
    let mut client = h.connect().await;
    register_approver(&mut client).await;
    client
}

/// Reads the next *response* frame, skipping `plan_changed` pushes: a
/// connection that is also a registered approver may be pushed to while its
/// own request is in flight.
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

async fn create_plan(
    client: &mut Connection,
    title: &str,
    plan_tasks: Vec<PlanTaskSpec>,
) -> DaemonMessage {
    send(
        client,
        &ClientMessage::CreatePlan {
            title: title.to_string(),
            tasks: plan_tasks,
        },
    )
    .await;
    read_ledger_reply(client, Duration::from_secs(5)).await
}

async fn update_plan(
    client: &mut Connection,
    plan_id: &str,
    plan_tasks: Vec<PlanTaskSpec>,
) -> DaemonMessage {
    send(
        client,
        &ClientMessage::UpdatePlan {
            plan_id: plan_id.to_string(),
            title: None,
            tasks: plan_tasks,
        },
    )
    .await;
    read_ledger_reply(client, Duration::from_secs(5)).await
}

async fn get_plan(client: &mut Connection, plan_id: &str) -> DaemonMessage {
    send(
        client,
        &ClientMessage::GetPlan {
            plan_id: plan_id.to_string(),
        },
    )
    .await;
    read_ledger_reply(client, Duration::from_secs(5)).await
}

async fn list_plans(client: &mut Connection) -> DaemonMessage {
    send(client, &ClientMessage::ListPlans).await;
    read_ledger_reply(client, Duration::from_secs(5)).await
}

async fn open_task_window(client: &mut Connection, plan_id: &str, task_id: &str) -> DaemonMessage {
    send(
        client,
        &ClientMessage::OpenTaskWindow {
            plan_id: plan_id.to_string(),
            task_id: task_id.to_string(),
            name: None,
        },
    )
    .await;
    read_ledger_reply(client, Duration::from_secs(10)).await
}

async fn send_report(client: &mut Connection, summary: &str) -> DaemonMessage {
    send(
        client,
        &ClientMessage::SendReport {
            outcome: ReportOutcome::Succeeded,
            summary: summary.to_string(),
            path: None,
        },
    )
    .await;
    read_response(client, Duration::from_secs(5)).await
}

fn expect_plan_detail(msg: DaemonMessage) -> PlanInfo {
    match msg {
        DaemonMessage::PlanDetail { plan } => plan,
        other => panic!("expected plan_detail, got {other:?}"),
    }
}

fn expect_bound_window(msg: DaemonMessage) -> String {
    match msg {
        DaemonMessage::TaskWindowOpened { window_id, .. } => window_id,
        other => panic!("expected task_window_opened, got {other:?}"),
    }
}

fn task_info<'a>(plan: &'a PlanInfo, task_id: &str) -> &'a PlanTaskInfo {
    plan.tasks
        .iter()
        .find(|t| t.task_id == task_id)
        .unwrap_or_else(|| panic!("task {task_id:?} not found in plan {plan:?}"))
}

fn instruction_of<'a>(plan: &'a PlanInfo, task_id: &str) -> Option<&'a str> {
    task_info(plan, task_id).instruction.as_deref()
}

async fn plan_count(client: &mut Connection) -> usize {
    match list_plans(client).await {
        DaemonMessage::PlanList { plans } => plans.len(),
        other => panic!("expected plan_list, got {other:?}"),
    }
}

// --- create_plan stores, get_plan returns ----------------------

#[tokio::test]
async fn lg_s_1_create_plan_stores_each_tasks_instruction_and_get_plan_returns_it_byte_exact() {
    let dir = unique_temp_dir("task-instruction-create");
    let config = ledger_config(&dir, None, &[]);

    let plan_id;
    {
        let (h, _locks) = start_ledger("task-instruction-create-a", config.clone()).await;
        let mut owner = h.connect().await;
        open_and_identify(&h, &mut owner).await;

        let created = expect_plan_detail(
            create_plan(
                &mut owner,
                "plan",
                vec![
                    task_with_instruction("t1", INSTRUCTION_A),
                    task("t2"),
                    task_with_instruction("t3", INSTRUCTION_B),
                ],
            )
            .await,
        );
        plan_id = created.plan_id.clone();

        let read = expect_plan_detail(get_plan(&mut owner, &plan_id).await);
        assert_eq!(
            instruction_of(&read, "t1").map(str::as_bytes),
            Some(INSTRUCTION_A.as_bytes()),
            "get_plan must return t1's instruction byte for byte, LF and TAB included"
        );
        assert_eq!(
            instruction_of(&read, "t2"),
            None,
            "a task created without an instruction must read back as having none"
        );
        assert_eq!(
            instruction_of(&read, "t3").map(str::as_bytes),
            Some(INSTRUCTION_B.as_bytes()),
            "get_plan must return t3's instruction byte for byte"
        );
    }

    // A fresh daemon on the same database: the instruction was persisted,
    // not merely held in memory by the first daemon.
    let (h, _locks) = start_ledger("task-instruction-create-b", config).await;
    let mut approver = approver_connection(&h).await;
    let reread = expect_plan_detail(get_plan(&mut approver, &plan_id).await);
    assert_eq!(
        instruction_of(&reread, "t1").map(str::as_bytes),
        Some(INSTRUCTION_A.as_bytes()),
        "t1's instruction must survive a daemon restart against the same database"
    );
    assert_eq!(instruction_of(&reread, "t2"), None);
    assert_eq!(
        instruction_of(&reread, "t3").map(str::as_bytes),
        Some(INSTRUCTION_B.as_bytes())
    );
}

// --- update_plan replaces / removes; binding and report survive -

#[tokio::test]
async fn lg_s_2_update_plan_replaces_and_removes_instructions_and_keeps_the_binding_and_report() {
    let dir = unique_temp_dir("task-instruction-update");
    let (h, _locks) =
        start_ledger("task-instruction-update", ledger_config_with_profile(&dir)).await;

    let mut owner = h.connect().await;
    open_and_identify(&h, &mut owner).await;
    let plan = expect_plan_detail(
        create_plan(
            &mut owner,
            "plan",
            vec![
                with_profile(task("t1")),
                task_with_instruction("t2", INSTRUCTION_B),
            ],
        )
        .await,
    );
    let plan_id = plan.plan_id.clone();

    let bound_window = expect_bound_window(open_task_window(&mut owner, &plan_id, "t1").await);
    let mut bound = h.connect().await;
    identify(&mut bound, &bound_window).await;
    match send_report(&mut bound, "t1 finished").await {
        DaemonMessage::ReportSent { task, .. } => assert_eq!(
            task.map(|t| t.task_id),
            Some("t1".to_string()),
            "the bound window's report must be recorded against t1"
        ),
        other => panic!("expected report_sent, got {other:?}"),
    }

    let before = expect_plan_detail(get_plan(&mut owner, &plan_id).await);
    let report_before = task_info(&before, "t1")
        .report
        .clone()
        .expect("t1 must carry the recorded report before any update");
    assert_eq!(
        task_info(&before, "t1").window_id.as_deref(),
        Some(bound_window.as_str())
    );

    let assert_survivors = |plan: &PlanInfo, step: &str| {
        let t1 = task_info(plan, "t1");
        assert_eq!(
            t1.window_id.as_deref(),
            Some(bound_window.as_str()),
            "{step}: update_plan must keep t1 bound to its window"
        );
        assert_eq!(
            t1.report.as_ref(),
            Some(&report_before),
            "{step}: update_plan must keep t1's recorded report"
        );
    };

    // Step 1: give t1 an instruction, take t2's away.
    let step1 = vec![
        with_profile(task_with_instruction("t1", INSTRUCTION_A)),
        task("t2"),
    ];
    expect_plan_detail(update_plan(&mut owner, &plan_id, step1).await);
    let after1 = expect_plan_detail(get_plan(&mut owner, &plan_id).await);
    assert_eq!(
        instruction_of(&after1, "t1"),
        Some(INSTRUCTION_A),
        "update_plan must store the instruction the new spec gives t1"
    );
    assert_eq!(
        instruction_of(&after1, "t2"),
        None,
        "a new spec without an instruction must remove t2's previous one, not inherit it"
    );
    assert_survivors(&after1, "adding an instruction");

    // Step 2: replace t1's instruction.
    let step2 = vec![
        with_profile(task_with_instruction("t1", INSTRUCTION_C)),
        task("t2"),
    ];
    expect_plan_detail(update_plan(&mut owner, &plan_id, step2).await);
    let after2 = expect_plan_detail(get_plan(&mut owner, &plan_id).await);
    assert_eq!(
        instruction_of(&after2, "t1"),
        Some(INSTRUCTION_C),
        "update_plan must replace t1's instruction with the new spec's value"
    );
    assert_survivors(&after2, "replacing an instruction");

    // Step 3: remove t1's instruction while it stays bound.
    let step3 = vec![with_profile(task("t1")), task("t2")];
    expect_plan_detail(update_plan(&mut owner, &plan_id, step3).await);
    let after3 = expect_plan_detail(get_plan(&mut owner, &plan_id).await);
    assert_eq!(
        instruction_of(&after3, "t1"),
        None,
        "a new spec without an instruction must remove the bound t1's previous one"
    );
    assert_survivors(&after3, "removing an instruction");
}

// --- Invalid instructions are refused and change nothing -------

/// Each rejected instruction, with a distinctive marker so a reply that
/// echoes the value back can be detected.
fn invalid_instructions() -> Vec<(&'static str, String)> {
    let mut too_long = String::from("leak-marker-long ");
    too_long.push_str(&"x".repeat(TASK_INSTRUCTION_MAX_BYTES + 1 - too_long.len()));
    vec![
        ("empty", String::new()),
        ("over the byte limit", too_long),
        (
            "carriage return",
            "leak-marker-cr first\rsecond".to_string(),
        ),
        ("escape", "leak-marker-esc \u{1b}[31m red".to_string()),
        ("bell", "leak-marker-bel \u{7} ring".to_string()),
        ("NUL", "leak-marker-nul \u{0} end".to_string()),
    ]
}

fn assert_invalid_argument_without_echo(reply: DaemonMessage, case: &str) {
    let message = match reply {
        DaemonMessage::Error { code, message, .. } => {
            assert_eq!(
                code, "invalid_argument",
                "{case}: an invalid instruction must be refused as invalid_argument"
            );
            message
        }
        other => panic!("{case}: expected an invalid_argument error, got {other:?}"),
    };
    assert!(
        !message.contains("leak-marker"),
        "{case}: the refusal must not echo the rejected instruction, got {message:?}"
    );
}

#[tokio::test]
async fn lg_v_1_create_plan_refuses_an_invalid_instruction_and_stores_no_plan() {
    let dir = unique_temp_dir("task-instruction-create-invalid");
    let (h, _locks) = start_ledger(
        "task-instruction-create-invalid",
        ledger_config(&dir, None, &[]),
    )
    .await;

    let mut owner = h.connect().await;
    open_and_identify(&h, &mut owner).await;

    for (case, instruction) in invalid_instructions() {
        let reply = create_plan(
            &mut owner,
            "plan",
            vec![task("t1"), task_with_instruction("t2", &instruction)],
        )
        .await;
        assert_invalid_argument_without_echo(reply, case);
        assert_eq!(
            plan_count(&mut owner).await,
            0,
            "{case}: a refused create_plan must not store a plan"
        );
    }

    // The limit is inclusive: exactly the maximum is accepted.
    let at_limit = "y".repeat(TASK_INSTRUCTION_MAX_BYTES);
    let plan = expect_plan_detail(
        create_plan(
            &mut owner,
            "plan",
            vec![task_with_instruction("t1", &at_limit)],
        )
        .await,
    );
    let read = expect_plan_detail(get_plan(&mut owner, &plan.plan_id).await);
    assert_eq!(
        instruction_of(&read, "t1").map(str::len),
        Some(TASK_INSTRUCTION_MAX_BYTES),
        "an instruction of exactly the byte limit must be accepted and stored whole"
    );
}

#[tokio::test]
async fn lg_v_1_update_plan_refuses_an_invalid_instruction_and_leaves_the_plan_unchanged() {
    let dir = unique_temp_dir("task-instruction-update-invalid");
    let (h, _locks) = start_ledger(
        "task-instruction-update-invalid",
        ledger_config(&dir, None, &[]),
    )
    .await;

    let mut owner = h.connect().await;
    open_and_identify(&h, &mut owner).await;
    let plan = expect_plan_detail(
        create_plan(
            &mut owner,
            "plan",
            vec![task_with_instruction("t1", INSTRUCTION_A), task("t2")],
        )
        .await,
    );
    let before = expect_plan_detail(get_plan(&mut owner, &plan.plan_id).await);

    for (case, instruction) in invalid_instructions() {
        let reply = update_plan(
            &mut owner,
            &plan.plan_id,
            vec![
                task_with_instruction("t1", INSTRUCTION_C),
                task_with_instruction("t2", &instruction),
                task("t3"),
            ],
        )
        .await;
        assert_invalid_argument_without_echo(reply, case);
        let after = expect_plan_detail(get_plan(&mut owner, &plan.plan_id).await);
        assert_eq!(
            after, before,
            "{case}: a refused update_plan must leave the stored plan exactly as it was"
        );
    }
}

// --- The bound child reads its own task's instruction ----------

#[tokio::test]
async fn lg_p_1_a_bound_task_window_reads_its_instruction_and_an_outside_window_cannot() {
    let dir = unique_temp_dir("task-instruction-bound-read");
    let (h, _locks) = start_ledger(
        "task-instruction-bound-read",
        ledger_config_with_profile(&dir),
    )
    .await;

    let mut owner = h.connect().await;
    open_and_identify(&h, &mut owner).await;
    let plan =
        expect_plan_detail(create_plan(&mut owner, "plan", vec![with_profile(task("t1"))]).await);
    let bound_window = expect_bound_window(open_task_window(&mut owner, &plan.plan_id, "t1").await);
    expect_plan_detail(
        update_plan(
            &mut owner,
            &plan.plan_id,
            vec![with_profile(task_with_instruction("t1", INSTRUCTION_A))],
        )
        .await,
    );

    let mut bound = h.connect().await;
    identify(&mut bound, &bound_window).await;
    let read = expect_plan_detail(get_plan(&mut bound, &plan.plan_id).await);
    assert_eq!(
        task_info(&read, "t1").window_id.as_deref(),
        Some(bound_window.as_str()),
        "the reader must be the window bound to t1"
    );
    assert_eq!(
        instruction_of(&read, "t1"),
        Some(INSTRUCTION_A),
        "the window bound to t1 must read t1's instruction through get_plan"
    );

    let mut outsider = h.connect().await;
    open_and_identify(&h, &mut outsider).await;
    expect_error(
        get_plan(&mut outsider, &plan.plan_id).await,
        "plan_not_found",
    );
}

// --- A task row written before migration 009 reads as None -----

#[tokio::test]
async fn lg_m_1_a_task_stored_before_the_instruction_column_existed_reads_back_with_none() {
    let dir = unique_temp_dir("task-instruction-legacy-row");
    let config = ledger_config(&dir, None, &[]);
    let db_path = dir.join("plan.db");
    {
        let mut conn = rusqlite::Connection::open(&db_path).expect("a seed connection must open");
        store::apply_migrations(&mut conn, &store::MIGRATIONS[..8])
            .expect("the first eight migrations must apply");
        conn.execute_batch(
            "INSERT INTO plan (plan_id, title, cwd, owner_window, created_at, updated_at) \
             VALUES ('legacy-plan', 'legacy', 'C:/work', NULL, 1000, 1000); \
             INSERT INTO plan_task (plan_id, task_id, seq, title, report_outcome, \
             report_summary, report_path, reported_at) \
             VALUES ('legacy-plan', 't1', 0, 'old task', 'succeeded', 'old summary', NULL, 1000);",
        )
        .expect("seeding a v8 plan must succeed");
    }

    let (h, _locks) = start_ledger("task-instruction-legacy-row", config).await;
    let mut approver = approver_connection(&h).await;
    let read = expect_plan_detail(get_plan(&mut approver, "legacy-plan").await);
    let t1 = task_info(&read, "t1");
    assert_eq!(
        t1.instruction, None,
        "a task row written before the instruction column existed must read as having none"
    );
    assert_eq!(
        t1.report.as_ref().map(|r| r.summary.as_str()),
        Some("old summary"),
        "the rest of the pre-existing row must still read back"
    );
}
