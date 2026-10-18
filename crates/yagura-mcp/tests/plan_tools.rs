#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The plan tools of the MCP bridge against a scripted transport: which
//! `ClientMessage` each tool sends, which required arguments it refuses
//! before any round trip, that it never re-implements the daemon's plan
//! validation, and how the daemon's replies and error codes reach the caller.
//!
//! Arguments are built from JSON, the shape an MCP client actually sends, so
//! these tests fix the external argument names rather than the Rust types
//! the bridge happens to deserialize them into.

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::{Value, json};
use yagura_mcp::plan_tools::{
    CreatePlanArgs, DeletePlanArgs, GetPlanArgs, OpenTaskWindowArgs, UpdatePlanArgs, create_plan,
    delete_plan, get_plan, list_plans, list_roles, open_task_window, update_plan,
};
use yagura_mcp::tools::ToolError;
use yagura_mcp::transport::{DaemonTransport, RequesterIdentity, TransportError};
use yagura_protocol::{
    AssignmentSource, ClientMessage, DaemonMessage, InstructionNotice, InstructionNoticeState,
    PlanInfo, PlanSummary, PlanTaskInfo, PlanTaskSpec, ResolvedAssignment, RoleEntry,
    TaskAssignment,
};

/// Replays queued responses in order and records every request, so a test
/// can assert both what came back and whether the daemon was contacted.
struct MockTransport {
    responses: Mutex<Vec<Result<DaemonMessage, TransportError>>>,
    sent: Mutex<Vec<ClientMessage>>,
    call_count: AtomicUsize,
}

impl MockTransport {
    /// A transport whose `round_trip` panics, so a missing short-circuit
    /// fails loudly instead of passing silently.
    fn never_called() -> Self {
        Self {
            responses: Mutex::new(Vec::new()),
            sent: Mutex::new(Vec::new()),
            call_count: AtomicUsize::new(0),
        }
    }

    fn with_response(response: Result<DaemonMessage, TransportError>) -> Self {
        Self {
            responses: Mutex::new(vec![response]),
            sent: Mutex::new(Vec::new()),
            call_count: AtomicUsize::new(0),
        }
    }

    fn call_count(&self) -> usize {
        self.call_count.load(Ordering::SeqCst)
    }

    fn last_sent(&self) -> ClientMessage {
        self.sent
            .lock()
            .expect("sent log mutex must not be poisoned")
            .last()
            .expect("round_trip must have been called at least once")
            .clone()
    }
}

impl DaemonTransport for MockTransport {
    fn requester_identity(&self) -> RequesterIdentity {
        RequesterIdentity::Window("w-owner".to_string())
    }

    async fn round_trip(&self, msg: ClientMessage) -> Result<DaemonMessage, TransportError> {
        self.call_count.fetch_add(1, Ordering::SeqCst);
        self.sent
            .lock()
            .expect("sent log mutex must not be poisoned")
            .push(msg);
        let mut responses = self
            .responses
            .lock()
            .expect("responses mutex must not be poisoned");
        if responses.is_empty() {
            panic!("MockTransport::round_trip called with no scripted response queued");
        }
        responses.remove(0)
    }
}

fn daemon_error(code: &str, message: &str) -> DaemonMessage {
    DaemonMessage::Error {
        code: code.to_string(),
        message: message.to_string(),
        rejected_argument: None,
    }
}

fn create_args(value: Value) -> CreatePlanArgs {
    serde_json::from_value(value).expect("create_plan arguments must deserialize")
}

fn update_args(value: Value) -> UpdatePlanArgs {
    serde_json::from_value(value).expect("update_plan arguments must deserialize")
}

fn get_args(value: Value) -> GetPlanArgs {
    serde_json::from_value(value).expect("get_plan arguments must deserialize")
}

fn delete_args(value: Value) -> DeletePlanArgs {
    serde_json::from_value(value).expect("delete_plan arguments must deserialize")
}

fn open_args(value: Value) -> OpenTaskWindowArgs {
    serde_json::from_value(value).expect("open_task_window arguments must deserialize")
}

fn full_task_json() -> Value {
    json!({
        "task_id": "build-parser",
        "title": "Build the parser",
        "role": "heavy",
        "profile": "claude-code",
        "model": "opus",
        "effort": "high",
        "depends_on": ["lay-out-types"],
        "writes": ["src/parser.rs", "tests/parser.rs"],
    })
}

fn full_task_spec() -> PlanTaskSpec {
    PlanTaskSpec {
        task_id: "build-parser".to_string(),
        title: "Build the parser".to_string(),
        role: Some("heavy".to_string()),
        profile: Some("claude-code".to_string()),
        model: Some("opus".to_string()),
        effort: Some("high".to_string()),
        depends_on: vec!["lay-out-types".to_string()],
        writes: vec!["src/parser.rs".to_string(), "tests/parser.rs".to_string()],
        instruction: None,
    }
}

fn bare_task_json() -> Value {
    json!({
        "task_id": "lay-out-types",
        "title": "Lay out the types",
        "depends_on": [],
        "writes": ["src/types.rs"],
    })
}

fn bare_task_spec() -> PlanTaskSpec {
    PlanTaskSpec {
        task_id: "lay-out-types".to_string(),
        title: "Lay out the types".to_string(),
        role: None,
        profile: None,
        model: None,
        effort: None,
        depends_on: Vec::new(),
        writes: vec!["src/types.rs".to_string()],
        instruction: None,
    }
}

fn assignment(profile: &str, model: &str, effort: &str) -> TaskAssignment {
    TaskAssignment {
        profile: profile.to_string(),
        model: Some(model.to_string()),
        effort: Some(effort.to_string()),
    }
}

/// A plan with every optional field populated, so each field's appearance in
/// the reply is observable without depending on how an absent one is shown.
fn sample_plan() -> PlanInfo {
    PlanInfo {
        plan_id: "plan-7".to_string(),
        title: "Parser rework".to_string(),
        cwd: "/work/repo".to_string(),
        owner_window: Some("w-owner".to_string()),
        waves: vec![
            vec!["lay-out-types".to_string()],
            vec!["build-parser".to_string()],
        ],
        tasks: vec![PlanTaskInfo {
            task_id: "build-parser".to_string(),
            title: "Build the parser".to_string(),
            role: Some("heavy".to_string()),
            depends_on: vec!["lay-out-types".to_string()],
            writes: vec!["src/parser.rs".to_string()],
            wave: 1,
            parent: Some(assignment("claude-code", "sonnet", "medium")),
            user: Some(assignment("codex-cli", "gpt-5", "high")),
            resolved: ResolvedAssignment {
                source: AssignmentSource::User,
                assignment: Some(assignment("codex-cli", "gpt-5", "high")),
            },
            window_id: Some("w-task".to_string()),
            report: None,
            instruction: None,
            instruction_notice: None,
            review_mark: None,
        }],
        roles_warning: Some("roles.toml could not be parsed".to_string()),
        review: None,
        version: 0,
    }
}

/// Asserts that every field of [`sample_plan`] reaches the caller's JSON.
fn assert_plan_detail_json(result: &Value) {
    assert_eq!(result["plan_id"], json!("plan-7"), "plan_id: {result}");
    assert_eq!(result["title"], json!("Parser rework"), "title: {result}");
    assert_eq!(result["cwd"], json!("/work/repo"), "cwd: {result}");
    assert_eq!(
        result["owner_window"],
        json!("w-owner"),
        "owner_window: {result}"
    );
    assert_eq!(
        result["waves"],
        json!([["lay-out-types"], ["build-parser"]]),
        "waves: {result}"
    );
    assert_eq!(
        result["roles_warning"],
        json!("roles.toml could not be parsed"),
        "roles_warning: {result}"
    );

    let tasks = result["tasks"]
        .as_array()
        .unwrap_or_else(|| panic!("the reply must carry a tasks array: {result}"));
    assert_eq!(tasks.len(), 1, "tasks: {result}");
    let task = &tasks[0];
    assert_eq!(task["task_id"], json!("build-parser"), "task_id: {task}");
    assert_eq!(
        task["title"],
        json!("Build the parser"),
        "task title: {task}"
    );
    assert_eq!(task["role"], json!("heavy"), "role: {task}");
    assert_eq!(
        task["depends_on"],
        json!(["lay-out-types"]),
        "depends_on: {task}"
    );
    assert_eq!(task["writes"], json!(["src/parser.rs"]), "writes: {task}");
    assert_eq!(task["wave"], json!(1), "wave: {task}");
    assert_eq!(task["window_id"], json!("w-task"), "window_id: {task}");
    assert_eq!(
        task["parent"]["profile"],
        json!("claude-code"),
        "parent: {task}"
    );
    assert_eq!(
        task["parent"]["model"],
        json!("sonnet"),
        "parent model: {task}"
    );
    assert_eq!(
        task["parent"]["effort"],
        json!("medium"),
        "parent effort: {task}"
    );
    assert_eq!(task["user"]["profile"], json!("codex-cli"), "user: {task}");
    assert_eq!(
        task["resolved"]["source"],
        json!("user"),
        "resolved source: {task}"
    );
    assert_eq!(
        task["resolved"]["assignment"]["profile"],
        json!("codex-cli"),
        "resolved profile: {task}"
    );
    assert_eq!(
        task["resolved"]["assignment"]["model"],
        json!("gpt-5"),
        "resolved model: {task}"
    );
    assert_eq!(
        task["resolved"]["assignment"]["effort"],
        json!("high"),
        "resolved effort: {task}"
    );
}

/// The seven plan tools, each called with a well-formed argument set, so a
/// property that must hold for every tool is checked against all of them.
#[derive(Debug, Clone, Copy)]
enum PlanTool {
    CreatePlan,
    UpdatePlan,
    GetPlan,
    ListPlans,
    DeletePlan,
    ListRoles,
    OpenTaskWindow,
}

const ALL_PLAN_TOOLS: [PlanTool; 7] = [
    PlanTool::CreatePlan,
    PlanTool::UpdatePlan,
    PlanTool::GetPlan,
    PlanTool::ListPlans,
    PlanTool::DeletePlan,
    PlanTool::ListRoles,
    PlanTool::OpenTaskWindow,
];

/// The tools that address one existing plan by `plan_id`, the only ones the
/// daemon can answer with `plan_not_found` or `not_plan_owner`.
const PLAN_ADDRESSING_TOOLS: [PlanTool; 4] = [
    PlanTool::UpdatePlan,
    PlanTool::GetPlan,
    PlanTool::DeletePlan,
    PlanTool::OpenTaskWindow,
];

async fn call(tool: PlanTool, transport: &MockTransport) -> Result<Value, ToolError> {
    match tool {
        PlanTool::CreatePlan => {
            create_plan(
                transport,
                create_args(json!({ "title": "Parser rework", "tasks": [bare_task_json()] })),
            )
            .await
        }
        PlanTool::UpdatePlan => {
            update_plan(
                transport,
                update_args(json!({ "plan_id": "plan-7", "tasks": [bare_task_json()] })),
            )
            .await
        }
        PlanTool::GetPlan => get_plan(transport, get_args(json!({ "plan_id": "plan-7" }))).await,
        PlanTool::ListPlans => list_plans(transport).await,
        PlanTool::DeletePlan => {
            delete_plan(transport, delete_args(json!({ "plan_id": "plan-7" }))).await
        }
        PlanTool::ListRoles => list_roles(transport).await,
        PlanTool::OpenTaskWindow => {
            open_task_window(
                transport,
                open_args(json!({ "plan_id": "plan-7", "task_id": "build-parser" })),
            )
            .await
        }
    }
}

/// A reply of a kind no plan tool asks for, so it is a contract violation for
/// every one of them.
fn reply_no_plan_tool_expects() -> DaemonMessage {
    DaemonMessage::WindowClosed {
        window_id: "w-1".to_string(),
    }
}

// ---------------------------------------------------------------------
// Which ClientMessage each tool sends
// ---------------------------------------------------------------------

#[tokio::test]
async fn create_plan_sends_the_title_and_every_task_field_verbatim() {
    let transport = MockTransport::with_response(Ok(DaemonMessage::PlanDetail {
        plan: sample_plan(),
    }));

    create_plan(
        &transport,
        create_args(json!({
            "title": "Parser rework",
            "tasks": [bare_task_json(), full_task_json()],
        })),
    )
    .await
    .expect("create_plan must succeed");

    assert_eq!(transport.call_count(), 1);
    assert_eq!(
        transport.last_sent(),
        ClientMessage::CreatePlan {
            title: "Parser rework".to_string(),
            tasks: vec![bare_task_spec(), full_task_spec()],
        }
    );
}

#[tokio::test]
async fn update_plan_sends_the_plan_id_the_new_title_and_the_full_task_list() {
    let transport = MockTransport::with_response(Ok(DaemonMessage::PlanDetail {
        plan: sample_plan(),
    }));

    update_plan(
        &transport,
        update_args(json!({
            "plan_id": "plan-7",
            "title": "Parser rework, second pass",
            "tasks": [bare_task_json(), full_task_json()],
        })),
    )
    .await
    .expect("update_plan must succeed");

    assert_eq!(transport.call_count(), 1);
    assert_eq!(
        transport.last_sent(),
        ClientMessage::UpdatePlan {
            plan_id: "plan-7".to_string(),
            title: Some("Parser rework, second pass".to_string()),
            tasks: vec![bare_task_spec(), full_task_spec()],
        }
    );
}

#[tokio::test]
async fn update_plan_without_a_title_sends_none_so_the_stored_title_is_kept() {
    let transport = MockTransport::with_response(Ok(DaemonMessage::PlanDetail {
        plan: sample_plan(),
    }));

    update_plan(
        &transport,
        update_args(json!({ "plan_id": "plan-7", "tasks": [bare_task_json()] })),
    )
    .await
    .expect("update_plan must succeed");

    assert_eq!(
        transport.last_sent(),
        ClientMessage::UpdatePlan {
            plan_id: "plan-7".to_string(),
            title: None,
            tasks: vec![bare_task_spec()],
        }
    );
}

#[tokio::test]
async fn get_plan_sends_the_plan_id() {
    let transport = MockTransport::with_response(Ok(DaemonMessage::PlanDetail {
        plan: sample_plan(),
    }));

    get_plan(&transport, get_args(json!({ "plan_id": "plan-7" })))
        .await
        .expect("get_plan must succeed");

    assert_eq!(transport.call_count(), 1);
    assert_eq!(
        transport.last_sent(),
        ClientMessage::GetPlan {
            plan_id: "plan-7".to_string(),
        }
    );
}

#[tokio::test]
async fn list_plans_sends_list_plans() {
    let transport = MockTransport::with_response(Ok(DaemonMessage::PlanList { plans: Vec::new() }));

    list_plans(&transport)
        .await
        .expect("list_plans must succeed");

    assert_eq!(transport.call_count(), 1);
    assert_eq!(transport.last_sent(), ClientMessage::ListPlans);
}

#[tokio::test]
async fn delete_plan_sends_the_plan_id() {
    let transport = MockTransport::with_response(Ok(DaemonMessage::PlanDeleted {
        plan_id: "plan-7".to_string(),
    }));

    delete_plan(&transport, delete_args(json!({ "plan_id": "plan-7" })))
        .await
        .expect("delete_plan must succeed");

    assert_eq!(transport.call_count(), 1);
    assert_eq!(
        transport.last_sent(),
        ClientMessage::DeletePlan {
            plan_id: "plan-7".to_string(),
        }
    );
}

#[tokio::test]
async fn list_roles_sends_list_roles() {
    let transport = MockTransport::with_response(Ok(DaemonMessage::RoleList {
        roles: Vec::new(),
        default_role: None,
    }));

    list_roles(&transport)
        .await
        .expect("list_roles must succeed");

    assert_eq!(transport.call_count(), 1);
    assert_eq!(transport.last_sent(), ClientMessage::ListRoles);
}

#[tokio::test]
async fn open_task_window_sends_the_plan_id_the_task_id_and_the_name() {
    let transport = MockTransport::with_response(Ok(DaemonMessage::TaskWindowOpened {
        plan_id: "plan-7".to_string(),
        task_id: "build-parser".to_string(),
        window_id: "w-task".to_string(),
        assignment: ResolvedAssignment {
            source: AssignmentSource::Role,
            assignment: Some(assignment("claude-code", "opus", "high")),
        },
        name: None,
    }));

    open_task_window(
        &transport,
        open_args(json!({
            "plan_id": "plan-7",
            "task_id": "build-parser",
            "name": "parser",
        })),
    )
    .await
    .expect("open_task_window must succeed");

    assert_eq!(transport.call_count(), 1);
    assert_eq!(
        transport.last_sent(),
        ClientMessage::OpenTaskWindow {
            plan_id: "plan-7".to_string(),
            task_id: "build-parser".to_string(),
            name: Some("parser".to_string()),
        }
    );
}

#[tokio::test]
async fn open_task_window_without_a_name_sends_none() {
    let transport = MockTransport::with_response(Ok(DaemonMessage::TaskWindowOpened {
        plan_id: "plan-7".to_string(),
        task_id: "build-parser".to_string(),
        window_id: "w-task".to_string(),
        assignment: ResolvedAssignment {
            source: AssignmentSource::Parent,
            assignment: Some(assignment("claude-code", "opus", "high")),
        },
        name: None,
    }));

    open_task_window(
        &transport,
        open_args(json!({ "plan_id": "plan-7", "task_id": "build-parser" })),
    )
    .await
    .expect("open_task_window must succeed");

    assert_eq!(
        transport.last_sent(),
        ClientMessage::OpenTaskWindow {
            plan_id: "plan-7".to_string(),
            task_id: "build-parser".to_string(),
            name: None,
        }
    );
}

// ---------------------------------------------------------------------
// Required arguments are refused before any round trip
// ---------------------------------------------------------------------

fn assert_refused_locally(result: Result<Value, ToolError>, transport: &MockTransport, case: &str) {
    let err = result.expect_err(case);
    assert_eq!(err.code(), "invalid_argument", "{case}: got {err:?}");
    assert_eq!(
        transport.call_count(),
        0,
        "{case}: a missing or empty required argument must be refused before any round trip"
    );
}

#[tokio::test]
async fn create_plan_refuses_a_missing_or_empty_title_before_any_round_trip() {
    for (case, args) in [
        ("missing title", json!({ "tasks": [bare_task_json()] })),
        (
            "empty title",
            json!({ "title": "", "tasks": [bare_task_json()] }),
        ),
    ] {
        let transport = MockTransport::never_called();
        let result = create_plan(&transport, create_args(args)).await;
        assert_refused_locally(result, &transport, case);
    }
}

#[tokio::test]
async fn create_plan_refuses_missing_tasks_before_any_round_trip() {
    let transport = MockTransport::never_called();
    let result = create_plan(&transport, create_args(json!({ "title": "Parser rework" }))).await;
    assert_refused_locally(result, &transport, "missing tasks");
}

#[tokio::test]
async fn update_plan_refuses_a_missing_or_empty_plan_id_before_any_round_trip() {
    for (case, args) in [
        ("missing plan_id", json!({ "tasks": [bare_task_json()] })),
        (
            "empty plan_id",
            json!({ "plan_id": "", "tasks": [bare_task_json()] }),
        ),
    ] {
        let transport = MockTransport::never_called();
        let result = update_plan(&transport, update_args(args)).await;
        assert_refused_locally(result, &transport, case);
    }
}

#[tokio::test]
async fn update_plan_refuses_missing_tasks_before_any_round_trip() {
    let transport = MockTransport::never_called();
    let result = update_plan(&transport, update_args(json!({ "plan_id": "plan-7" }))).await;
    assert_refused_locally(result, &transport, "missing tasks");
}

#[tokio::test]
async fn get_plan_refuses_a_missing_or_empty_plan_id_before_any_round_trip() {
    for (case, args) in [
        ("missing plan_id", json!({})),
        ("empty plan_id", json!({ "plan_id": "" })),
    ] {
        let transport = MockTransport::never_called();
        let result = get_plan(&transport, get_args(args)).await;
        assert_refused_locally(result, &transport, case);
    }
}

#[tokio::test]
async fn delete_plan_refuses_a_missing_or_empty_plan_id_before_any_round_trip() {
    for (case, args) in [
        ("missing plan_id", json!({})),
        ("empty plan_id", json!({ "plan_id": "" })),
    ] {
        let transport = MockTransport::never_called();
        let result = delete_plan(&transport, delete_args(args)).await;
        assert_refused_locally(result, &transport, case);
    }
}

#[tokio::test]
async fn open_task_window_refuses_a_missing_or_empty_plan_id_before_any_round_trip() {
    for (case, args) in [
        ("missing plan_id", json!({ "task_id": "build-parser" })),
        (
            "empty plan_id",
            json!({ "plan_id": "", "task_id": "build-parser" }),
        ),
    ] {
        let transport = MockTransport::never_called();
        let result = open_task_window(&transport, open_args(args)).await;
        assert_refused_locally(result, &transport, case);
    }
}

#[tokio::test]
async fn open_task_window_refuses_a_missing_or_empty_task_id_before_any_round_trip() {
    for (case, args) in [
        ("missing task_id", json!({ "plan_id": "plan-7" })),
        (
            "empty task_id",
            json!({ "plan_id": "plan-7", "task_id": "" }),
        ),
    ] {
        let transport = MockTransport::never_called();
        let result = open_task_window(&transport, open_args(args)).await;
        assert_refused_locally(result, &transport, case);
    }
}

// ---------------------------------------------------------------------
// Plan validation belongs to the daemon alone
// ---------------------------------------------------------------------

fn task_json(task_id: &str, depends_on: &[&str], writes: &[&str]) -> Value {
    json!({
        "task_id": task_id,
        "title": "A task",
        "depends_on": depends_on,
        "writes": writes,
    })
}

fn task_spec(task_id: &str, depends_on: &[&str], writes: &[&str]) -> PlanTaskSpec {
    PlanTaskSpec {
        task_id: task_id.to_string(),
        title: "A task".to_string(),
        role: None,
        profile: None,
        model: None,
        effort: None,
        depends_on: depends_on.iter().map(|s| s.to_string()).collect(),
        writes: writes.iter().map(|s| s.to_string()).collect(),
        instruction: None,
    }
}

/// Task lists the daemon refuses, each paired with the specs the bridge must
/// forward unchanged: a cycle, a self-dependency, a duplicate task_id, an
/// unknown dependency, two unordered tasks writing the same file, a path
/// outside the plan's directory, an empty task_id, a task list over the
/// daemon's size limit, and an empty task list.
fn plans_the_daemon_refuses() -> Vec<(&'static str, Value, Vec<PlanTaskSpec>)> {
    let oversized: Vec<(Value, PlanTaskSpec)> = (0..300)
        .map(|index| {
            let id = format!("t{index}");
            (task_json(&id, &[], &[]), task_spec(&id, &[], &[]))
        })
        .collect();
    vec![
        (
            "dependency cycle",
            json!([task_json("a", &["b"], &[]), task_json("b", &["a"], &[])]),
            vec![task_spec("a", &["b"], &[]), task_spec("b", &["a"], &[])],
        ),
        (
            "self-dependency",
            json!([task_json("a", &["a"], &[])]),
            vec![task_spec("a", &["a"], &[])],
        ),
        (
            "duplicate task_id",
            json!([task_json("a", &[], &[]), task_json("a", &[], &[])]),
            vec![task_spec("a", &[], &[]), task_spec("a", &[], &[])],
        ),
        (
            "unknown dependency",
            json!([task_json("a", &["missing"], &[])]),
            vec![task_spec("a", &["missing"], &[])],
        ),
        (
            "unordered tasks writing the same file",
            json!([
                task_json("a", &[], &["src/lib.rs"]),
                task_json("b", &[], &["SRC/lib.rs"])
            ]),
            vec![
                task_spec("a", &[], &["src/lib.rs"]),
                task_spec("b", &[], &["SRC/lib.rs"]),
            ],
        ),
        (
            "path escaping the plan's directory",
            json!([task_json("a", &[], &["../outside.rs"])]),
            vec![task_spec("a", &[], &["../outside.rs"])],
        ),
        (
            "empty task_id",
            json!([task_json("", &[], &[])]),
            vec![task_spec("", &[], &[])],
        ),
        (
            "task list over the size limit",
            Value::Array(oversized.iter().map(|(json, _)| json.clone()).collect()),
            oversized.into_iter().map(|(_, spec)| spec).collect(),
        ),
        ("empty task list", json!([]), Vec::new()),
    ]
}

const DAEMON_VALIDATION_MESSAGE: &str = "plan rejected by the daemon's own validation";

#[tokio::test]
async fn create_plan_forwards_every_plan_the_daemon_refuses_and_relays_its_invalid_argument() {
    for (case, tasks_json, expected_tasks) in plans_the_daemon_refuses() {
        let transport = MockTransport::with_response(Ok(daemon_error(
            "invalid_argument",
            DAEMON_VALIDATION_MESSAGE,
        )));

        let err = create_plan(
            &transport,
            create_args(json!({ "title": "Parser rework", "tasks": tasks_json })),
        )
        .await
        .expect_err(case);

        assert_eq!(
            transport.call_count(),
            1,
            "{case}: the bridge must not validate the plan itself; the daemon must see it"
        );
        assert_eq!(
            transport.last_sent(),
            ClientMessage::CreatePlan {
                title: "Parser rework".to_string(),
                tasks: expected_tasks,
            },
            "{case}: the task list must reach the daemon unchanged"
        );
        assert_eq!(err.code(), "invalid_argument", "{case}: got {err:?}");
        assert!(
            err.to_string().contains(DAEMON_VALIDATION_MESSAGE),
            "{case}: the daemon's own explanation must reach the caller, got {err}"
        );
    }
}

#[tokio::test]
async fn update_plan_forwards_every_plan_the_daemon_refuses_and_relays_its_invalid_argument() {
    for (case, tasks_json, expected_tasks) in plans_the_daemon_refuses() {
        let transport = MockTransport::with_response(Ok(daemon_error(
            "invalid_argument",
            DAEMON_VALIDATION_MESSAGE,
        )));

        let err = update_plan(
            &transport,
            update_args(json!({ "plan_id": "plan-7", "tasks": tasks_json })),
        )
        .await
        .expect_err(case);

        assert_eq!(
            transport.call_count(),
            1,
            "{case}: the bridge must not validate the plan itself; the daemon must see it"
        );
        assert_eq!(
            transport.last_sent(),
            ClientMessage::UpdatePlan {
                plan_id: "plan-7".to_string(),
                title: None,
                tasks: expected_tasks,
            },
            "{case}: the task list must reach the daemon unchanged"
        );
        assert_eq!(err.code(), "invalid_argument", "{case}: got {err:?}");
        assert!(
            err.to_string().contains(DAEMON_VALIDATION_MESSAGE),
            "{case}: the daemon's own explanation must reach the caller, got {err}"
        );
    }
}

// ---------------------------------------------------------------------
// How each reply reaches the caller
// ---------------------------------------------------------------------

#[tokio::test]
async fn create_plan_reports_every_field_of_the_plan_detail_reply() {
    let transport = MockTransport::with_response(Ok(DaemonMessage::PlanDetail {
        plan: sample_plan(),
    }));

    let result = create_plan(
        &transport,
        create_args(json!({ "title": "Parser rework", "tasks": [bare_task_json()] })),
    )
    .await
    .expect("create_plan must succeed");

    assert_plan_detail_json(&result);
}

#[tokio::test]
async fn update_plan_reports_every_field_of_the_plan_detail_reply() {
    let transport = MockTransport::with_response(Ok(DaemonMessage::PlanDetail {
        plan: sample_plan(),
    }));

    let result = update_plan(
        &transport,
        update_args(json!({ "plan_id": "plan-7", "tasks": [bare_task_json()] })),
    )
    .await
    .expect("update_plan must succeed");

    assert_plan_detail_json(&result);
}

#[tokio::test]
async fn get_plan_reports_every_field_of_the_plan_detail_reply() {
    let transport = MockTransport::with_response(Ok(DaemonMessage::PlanDetail {
        plan: sample_plan(),
    }));

    let result = get_plan(&transport, get_args(json!({ "plan_id": "plan-7" })))
        .await
        .expect("get_plan must succeed");

    assert_plan_detail_json(&result);
}

#[tokio::test]
async fn list_plans_reports_every_field_of_each_plan_summary() {
    let transport = MockTransport::with_response(Ok(DaemonMessage::PlanList {
        plans: vec![
            PlanSummary {
                plan_id: "plan-7".to_string(),
                title: "Parser rework".to_string(),
                owner_window: Some("w-owner".to_string()),
                task_count: 5,
                review: None,
                unseen_changes: false,
            },
            PlanSummary {
                plan_id: "plan-8".to_string(),
                title: "Docs pass".to_string(),
                owner_window: Some("w-other".to_string()),
                task_count: 2,
                // Same note as above.
                review: None,
                unseen_changes: false,
            },
        ],
    }));

    let result = list_plans(&transport)
        .await
        .expect("list_plans must succeed");

    let plans = result["plans"]
        .as_array()
        .unwrap_or_else(|| panic!("the reply must carry a plans array: {result}"));
    assert_eq!(plans.len(), 2, "plans: {result}");
    assert_eq!(plans[0]["plan_id"], json!("plan-7"), "{result}");
    assert_eq!(plans[0]["title"], json!("Parser rework"), "{result}");
    assert_eq!(plans[0]["owner_window"], json!("w-owner"), "{result}");
    assert_eq!(plans[0]["task_count"], json!(5), "{result}");
    assert_eq!(plans[1]["plan_id"], json!("plan-8"), "{result}");
    assert_eq!(plans[1]["task_count"], json!(2), "{result}");
}

#[tokio::test]
async fn delete_plan_reports_the_deleted_plan_id() {
    let transport = MockTransport::with_response(Ok(DaemonMessage::PlanDeleted {
        plan_id: "plan-7".to_string(),
    }));

    let result = delete_plan(&transport, delete_args(json!({ "plan_id": "plan-7" })))
        .await
        .expect("delete_plan must succeed");

    assert_eq!(result["plan_id"], json!("plan-7"), "{result}");
}

#[tokio::test]
async fn list_roles_reports_every_field_of_each_role_row() {
    let transport = MockTransport::with_response(Ok(DaemonMessage::RoleList {
        roles: vec![
            RoleEntry {
                name: "heavy".to_string(),
                profile: "claude-code".to_string(),
                model: Some("opus".to_string()),
                effort: Some("high".to_string()),
            },
            RoleEntry {
                name: "light".to_string(),
                profile: "codex-cli".to_string(),
                model: Some("gpt-5-mini".to_string()),
                effort: Some("low".to_string()),
            },
        ],
        default_role: None,
    }));

    let result = list_roles(&transport)
        .await
        .expect("list_roles must succeed");

    let roles = result["roles"]
        .as_array()
        .unwrap_or_else(|| panic!("the reply must carry a roles array: {result}"));
    assert_eq!(roles.len(), 2, "roles: {result}");
    assert_eq!(roles[0]["name"], json!("heavy"), "{result}");
    assert_eq!(roles[0]["profile"], json!("claude-code"), "{result}");
    assert_eq!(roles[0]["model"], json!("opus"), "{result}");
    assert_eq!(roles[0]["effort"], json!("high"), "{result}");
    assert_eq!(roles[1]["name"], json!("light"), "{result}");
    assert_eq!(roles[1]["profile"], json!("codex-cli"), "{result}");
}

#[tokio::test]
async fn open_task_window_reports_the_window_and_the_assignment_with_its_source() {
    let transport = MockTransport::with_response(Ok(DaemonMessage::TaskWindowOpened {
        plan_id: "plan-7".to_string(),
        task_id: "build-parser".to_string(),
        window_id: "w-task".to_string(),
        assignment: ResolvedAssignment {
            source: AssignmentSource::Role,
            assignment: Some(assignment("claude-code", "opus", "high")),
        },
        name: None,
    }));

    let result = open_task_window(
        &transport,
        open_args(json!({ "plan_id": "plan-7", "task_id": "build-parser" })),
    )
    .await
    .expect("open_task_window must succeed");

    assert_eq!(result["plan_id"], json!("plan-7"), "{result}");
    assert_eq!(result["task_id"], json!("build-parser"), "{result}");
    assert_eq!(result["window_id"], json!("w-task"), "{result}");
    assert_eq!(result["assignment"]["source"], json!("role"), "{result}");
    assert_eq!(
        result["assignment"]["assignment"]["profile"],
        json!("claude-code"),
        "{result}"
    );
    assert_eq!(
        result["assignment"]["assignment"]["model"],
        json!("opus"),
        "{result}"
    );
    assert_eq!(
        result["assignment"]["assignment"]["effort"],
        json!("high"),
        "{result}"
    );
}

#[tokio::test]
async fn every_plan_tool_treats_a_reply_of_another_kind_as_an_internal_error() {
    for tool in ALL_PLAN_TOOLS {
        let transport = MockTransport::with_response(Ok(reply_no_plan_tool_expects()));

        let err = call(tool, &transport)
            .await
            .expect_err("a reply of a kind the tool did not ask for is not a success");

        assert_eq!(err.code(), "internal", "{tool:?}: got {err:?}");
    }
}

// ---------------------------------------------------------------------
// Error codes
// ---------------------------------------------------------------------

#[tokio::test]
async fn every_plan_addressing_tool_reports_the_daemons_plan_not_found_under_the_same_code() {
    for tool in PLAN_ADDRESSING_TOOLS {
        let transport = MockTransport::with_response(Ok(daemon_error(
            "plan_not_found",
            "no plan with that id is visible to this window",
        )));

        let err = call(tool, &transport).await.expect_err("must fail");

        assert_eq!(err.code(), "plan_not_found", "{tool:?}: got {err:?}");
        assert!(
            err.to_string()
                .contains("no plan with that id is visible to this window"),
            "{tool:?}: the daemon's message must reach the caller, got {err}"
        );
    }
}

#[tokio::test]
async fn every_plan_addressing_tool_reports_the_daemons_not_plan_owner_under_the_same_code() {
    for tool in PLAN_ADDRESSING_TOOLS {
        let transport = MockTransport::with_response(Ok(daemon_error(
            "not_plan_owner",
            "only the plan's living owner window may change it",
        )));

        let err = call(tool, &transport).await.expect_err("must fail");

        assert_eq!(err.code(), "not_plan_owner", "{tool:?}: got {err:?}");
        assert!(
            err.to_string()
                .contains("only the plan's living owner window may change it"),
            "{tool:?}: the daemon's message must reach the caller, got {err}"
        );
    }
}

/// The daemon codes a plan tool can meet that existed before the plan tools
/// did, each of which must keep reaching the caller under its own name.
const PRE_EXISTING_DAEMON_CODES: &[&str] = &[
    "invalid_argument",
    "window_not_found",
    "unsupported",
    "child_exited",
    "resource_exhausted",
    "spawn_failed",
    "auth_failed",
    "invalid_state",
];

#[tokio::test]
async fn plan_tools_keep_every_pre_existing_daemon_code_under_its_own_name() {
    for tool in ALL_PLAN_TOOLS {
        for code in PRE_EXISTING_DAEMON_CODES {
            let transport =
                MockTransport::with_response(Ok(daemon_error(code, "refused by the daemon")));

            let err = call(tool, &transport).await.expect_err("must fail");

            assert_eq!(
                err.code(),
                *code,
                "{tool:?} with daemon code {code}: got {err:?}"
            );
        }
    }
}

#[tokio::test]
async fn plan_tools_treat_an_unrecognized_daemon_code_as_internal() {
    for tool in ALL_PLAN_TOOLS {
        let transport = MockTransport::with_response(Ok(daemon_error(
            "no_such_code",
            "a code the bridge has never heard of",
        )));

        let err = call(tool, &transport).await.expect_err("must fail");

        assert_eq!(err.code(), "internal", "{tool:?}: got {err:?}");
    }
}

#[tokio::test]
async fn every_plan_tool_reports_an_older_daemons_unknown_type_refusal_as_unsupported() {
    for tool in ALL_PLAN_TOOLS {
        let transport = MockTransport::with_response(Ok(daemon_error(
            "unknown_type",
            "the daemon does not recognize this message type",
        )));

        let err = call(tool, &transport).await.expect_err("must fail");

        assert_eq!(err.code(), "unsupported", "{tool:?}: got {err:?}");
    }
}

#[tokio::test]
async fn every_plan_tool_reports_a_transport_level_revision_refusal_as_unsupported() {
    for tool in ALL_PLAN_TOOLS {
        let transport = MockTransport::with_response(Err(TransportError::Unsupported(
            "the running daemon is too old for this request".to_string(),
        )));

        let err = call(tool, &transport).await.expect_err("must fail");

        assert_eq!(err.code(), "unsupported", "{tool:?}: got {err:?}");
    }
}

#[tokio::test]
async fn every_plan_tool_reports_an_unreachable_daemon_as_daemon_unavailable() {
    for tool in ALL_PLAN_TOOLS {
        let transport = MockTransport::with_response(Err(TransportError::Unavailable(
            "pipe not connected".to_string(),
        )));

        let err = call(tool, &transport).await.expect_err("must fail");

        assert_eq!(err.code(), "daemon_unavailable", "{tool:?}: got {err:?}");
    }
}

// ---------------------------------------------------------------------
// get_plan's task_id narrowing, and the
// instruction CRLF/CR normalization create_plan / update_plan apply before
// sending. Not implemented yet.
// ---------------------------------------------------------------------

/// A plan with two tasks, distinguishable by `task_id`, for the `task_id`
/// narrowing tests below.
fn two_task_plan() -> PlanInfo {
    let mut plan = sample_plan();
    plan.tasks.push(PlanTaskInfo {
        task_id: "lay-out-types".to_string(),
        title: "Lay out the types".to_string(),
        role: None,
        depends_on: Vec::new(),
        writes: vec!["src/types.rs".to_string()],
        wave: 0,
        parent: None,
        user: None,
        resolved: ResolvedAssignment {
            source: AssignmentSource::Parent,
            assignment: None,
        },
        window_id: None,
        report: None,
        instruction: None,
        instruction_notice: None,
        review_mark: None,
    });
    plan
}

#[tokio::test]
async fn get_plan_with_a_task_id_narrows_the_reply_to_that_one_task() {
    let transport = MockTransport::with_response(Ok(DaemonMessage::PlanDetail {
        plan: two_task_plan(),
    }));

    let result = get_plan(
        &transport,
        get_args(json!({ "plan_id": "plan-7", "task_id": "lay-out-types" })),
    )
    .await
    .expect("get_plan must succeed");

    // The wire itself carries no task_id: the bridge
    // narrows the reply after the same round trip a plain get_plan makes.
    assert_eq!(
        transport.last_sent(),
        ClientMessage::GetPlan {
            plan_id: "plan-7".to_string(),
        }
    );
    let tasks = result["tasks"]
        .as_array()
        .unwrap_or_else(|| panic!("the reply must carry a tasks array: {result}"));
    assert_eq!(
        tasks.len(),
        1,
        "task_id must narrow the reply to the one named task: {result}"
    );
    assert_eq!(tasks[0]["task_id"], json!("lay-out-types"), "{result}");
}

#[tokio::test]
async fn get_plan_with_an_unknown_task_id_refuses_invalid_argument_without_a_second_round_trip() {
    let transport = MockTransport::with_response(Ok(DaemonMessage::PlanDetail {
        plan: two_task_plan(),
    }));

    let err = get_plan(
        &transport,
        get_args(json!({ "plan_id": "plan-7", "task_id": "does-not-exist" })),
    )
    .await
    .expect_err("an unknown task_id must be refused");

    assert_eq!(err.code(), "invalid_argument", "got {err:?}");
    assert_eq!(
        transport.call_count(),
        1,
        "the narrowing check must not spend a second round trip"
    );
}

#[tokio::test]
async fn get_plan_without_a_task_id_keeps_every_task() {
    let transport = MockTransport::with_response(Ok(DaemonMessage::PlanDetail {
        plan: two_task_plan(),
    }));

    let result = get_plan(&transport, get_args(json!({ "plan_id": "plan-7" })))
        .await
        .expect("get_plan must succeed");

    let tasks = result["tasks"]
        .as_array()
        .unwrap_or_else(|| panic!("the reply must carry a tasks array: {result}"));
    assert_eq!(
        tasks.len(),
        2,
        "omitting task_id must not narrow the reply: {result}"
    );
}

#[tokio::test]
async fn create_plan_normalizes_crlf_and_lone_cr_in_task_instructions_to_lf() {
    let transport = MockTransport::with_response(Ok(DaemonMessage::PlanDetail {
        plan: sample_plan(),
    }));

    let mut task = bare_task_json();
    task["instruction"] = json!("line1\r\nline2\rline3\nline4");

    create_plan(
        &transport,
        create_args(json!({ "title": "Parser rework", "tasks": [task] })),
    )
    .await
    .expect("create_plan must succeed");

    match transport.last_sent() {
        ClientMessage::CreatePlan { tasks, .. } => {
            assert_eq!(
                tasks[0].instruction.as_deref(),
                Some("line1\nline2\nline3\nline4"),
                "CRLF and lone CR must normalize to LF before the instruction is sent"
            );
        }
        other => panic!("expected create_plan, got {other:?}"),
    }
}

#[tokio::test]
async fn update_plan_normalizes_crlf_and_lone_cr_in_task_instructions_to_lf() {
    let transport = MockTransport::with_response(Ok(DaemonMessage::PlanDetail {
        plan: sample_plan(),
    }));

    let mut task = bare_task_json();
    task["instruction"] = json!("a\r\nb\rc");

    update_plan(
        &transport,
        update_args(json!({ "plan_id": "plan-7", "tasks": [task] })),
    )
    .await
    .expect("update_plan must succeed");

    match transport.last_sent() {
        ClientMessage::UpdatePlan { tasks, .. } => {
            assert_eq!(tasks[0].instruction.as_deref(), Some("a\nb\nc"));
        }
        other => panic!("expected update_plan, got {other:?}"),
    }
}

// ---------------------------------------------------------------------
// A task's instruction_notice reaches get_plan's JSON only when the
// daemon reported one.
// ---------------------------------------------------------------------

#[tokio::test]
async fn get_plan_carries_instruction_notice_only_for_a_task_that_has_one() {
    let mut plan = two_task_plan();
    plan.tasks[0].instruction = Some("build it".to_string());
    plan.tasks[0].instruction_notice = Some(InstructionNotice {
        state: InstructionNoticeState::Delivered,
        at: Some(1_790_000_000),
    });
    let transport = MockTransport::with_response(Ok(DaemonMessage::PlanDetail { plan }));

    let result = get_plan(&transport, get_args(json!({ "plan_id": "plan-7" })))
        .await
        .expect("get_plan must succeed");

    let tasks = result["tasks"]
        .as_array()
        .unwrap_or_else(|| panic!("the reply must carry a tasks array: {result}"));
    let with_notice = tasks
        .iter()
        .find(|task| task["task_id"] == json!("build-parser"))
        .unwrap_or_else(|| panic!("build-parser must be in the reply: {result}"));
    assert_eq!(
        with_notice["instruction_notice"],
        json!({ "state": "delivered", "at": 1_790_000_000 }),
        "{result}"
    );
    let without_notice = tasks
        .iter()
        .find(|task| task["task_id"] == json!("lay-out-types"))
        .unwrap_or_else(|| panic!("lay-out-types must be in the reply: {result}"));
    assert!(
        without_notice
            .as_object()
            .is_some_and(|task| !task.contains_key("instruction_notice")),
        "a task with no notice state must not carry the key at all: {result}"
    );
}
