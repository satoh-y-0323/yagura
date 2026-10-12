#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The plan review gate as an MCP caller sees it through the plan tools: the
//! daemon's `plan_not_approved` refusal keeps its own code, and the review
//! state, the plan version, the per-task marks, the default role and the
//! `default` assignment source all reach the caller's JSON.

use std::sync::Mutex;

use serde_json::{Value, json};
use yagura_mcp::plan_tools::{
    CreatePlanArgs, GetPlanArgs, OpenTaskWindowArgs, UpdatePlanArgs, create_plan, get_plan,
    list_plans, list_roles, open_task_window, update_plan,
};
use yagura_mcp::tools::ToolError;
use yagura_mcp::transport::{DaemonTransport, RequesterIdentity, TransportError};
use yagura_protocol::{
    AssignmentSource, ClientMessage, DaemonMessage, PlanInfo, PlanReview, PlanSummary,
    PlanTaskInfo, ResolvedAssignment, ReviewMark, RoleEntry, TaskAssignment,
};

/// Answers the single round trip a plan tool makes with one scripted reply.
struct ScriptedTransport {
    reply: Mutex<Option<DaemonMessage>>,
}

impl ScriptedTransport {
    fn replying(reply: DaemonMessage) -> Self {
        Self {
            reply: Mutex::new(Some(reply)),
        }
    }
}

impl DaemonTransport for ScriptedTransport {
    fn requester_identity(&self) -> RequesterIdentity {
        RequesterIdentity::Window("w-owner".to_string())
    }

    async fn round_trip(&self, _msg: ClientMessage) -> Result<DaemonMessage, TransportError> {
        Ok(self
            .reply
            .lock()
            .expect("reply mutex must not be poisoned")
            .take()
            .expect("a plan tool makes exactly one round trip"))
    }
}

fn daemon_error(code: &str, message: &str) -> DaemonMessage {
    DaemonMessage::Error {
        code: code.to_string(),
        message: message.to_string(),
        rejected_argument: None,
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

fn assignment(profile: &str) -> TaskAssignment {
    TaskAssignment {
        profile: profile.to_string(),
        model: Some("opus".to_string()),
        effort: Some("high".to_string()),
    }
}

fn task(task_id: &str, review_mark: Option<ReviewMark>) -> PlanTaskInfo {
    PlanTaskInfo {
        task_id: task_id.to_string(),
        title: format!("Task {task_id}"),
        role: None,
        depends_on: Vec::new(),
        writes: vec![format!("src/{task_id}.rs")],
        wave: 0,
        parent: None,
        user: None,
        resolved: ResolvedAssignment {
            source: AssignmentSource::Default,
            assignment: Some(assignment("claude-code")),
        },
        window_id: None,
        report: None,
        instruction: None,
        instruction_notice: None,
        review_mark,
    }
}

fn plan(review: Option<PlanReview>, version: u64, tasks: Vec<PlanTaskInfo>) -> PlanInfo {
    PlanInfo {
        plan_id: "plan-7".to_string(),
        title: "Parser rework".to_string(),
        cwd: "/work/repo".to_string(),
        owner_window: Some("w-owner".to_string()),
        waves: vec![tasks.iter().map(|t| t.task_id.clone()).collect()],
        tasks,
        roles_warning: None,
        review,
        version,
    }
}

/// An approved plan whose tasks carry each mark and none, so every mark's
/// spelling and its omission are observable in one reply.
fn approved_plan_with_marks() -> PlanInfo {
    plan(
        Some(PlanReview::Approved),
        3,
        vec![
            task("added", Some(ReviewMark::New)),
            task("edited", Some(ReviewMark::Changed)),
            task("untouched", None),
        ],
    )
}

/// The three tools that reply with a whole plan.
#[derive(Debug, Clone, Copy)]
enum DetailTool {
    Create,
    Update,
    Get,
}

const DETAIL_TOOLS: [DetailTool; 3] = [DetailTool::Create, DetailTool::Update, DetailTool::Get];

async fn call_detail(tool: DetailTool, reply: PlanInfo) -> Value {
    let transport = ScriptedTransport::replying(DaemonMessage::PlanDetail { plan: reply });
    let result = match tool {
        DetailTool::Create => {
            let args: CreatePlanArgs = serde_json::from_value(
                json!({ "title": "Parser rework", "tasks": [bare_task_json()] }),
            )
            .expect("create_plan arguments must deserialize");
            create_plan(&transport, args).await
        }
        DetailTool::Update => {
            let args: UpdatePlanArgs =
                serde_json::from_value(json!({ "plan_id": "plan-7", "tasks": [bare_task_json()] }))
                    .expect("update_plan arguments must deserialize");
            update_plan(&transport, args).await
        }
        DetailTool::Get => {
            let args: GetPlanArgs = serde_json::from_value(json!({ "plan_id": "plan-7" }))
                .expect("get_plan arguments must deserialize");
            get_plan(&transport, args).await
        }
    };
    result.unwrap_or_else(|err| panic!("{tool:?} must succeed, got {err:?}"))
}

async fn open_task(transport: &ScriptedTransport) -> Result<Value, ToolError> {
    let args: OpenTaskWindowArgs =
        serde_json::from_value(json!({ "plan_id": "plan-7", "task_id": "added" }))
            .expect("open_task_window arguments must deserialize");
    open_task_window(transport, args).await
}

// ---------------------------------------------------------------------
// The refusal while a plan waits for review
// ---------------------------------------------------------------------

#[tokio::test]
async fn open_task_window_reports_the_daemons_plan_not_approved_under_the_same_code() {
    let transport = ScriptedTransport::replying(daemon_error(
        "plan_not_approved",
        "the operator has not approved this plan yet",
    ));

    let err = open_task(&transport)
        .await
        .expect_err("a refused open must not succeed");

    assert_eq!(err.code(), "plan_not_approved", "got {err:?}");
    assert!(
        err.to_string()
            .contains("the operator has not approved this plan yet"),
        "the daemon's message must reach the caller, got {err}"
    );
}

// ---------------------------------------------------------------------
// Review state in the plan replies
// ---------------------------------------------------------------------

#[tokio::test]
async fn plan_detail_replies_report_the_review_state_and_the_version() {
    for tool in DETAIL_TOOLS {
        for (review, spelled) in [
            (PlanReview::Pending, "pending"),
            (PlanReview::Approved, "approved"),
        ] {
            let result = call_detail(tool, plan(Some(review), 4, vec![task("added", None)])).await;

            assert_eq!(result["review"], json!(spelled), "{tool:?}: {result}");
            assert_eq!(result["version"], json!(4), "{tool:?}: {result}");
        }
    }
}

#[tokio::test]
async fn plan_detail_replies_omit_the_review_of_a_plan_outside_the_review_gate() {
    for tool in DETAIL_TOOLS {
        let result = call_detail(tool, plan(None, 0, vec![task("added", None)])).await;

        let object = result
            .as_object()
            .unwrap_or_else(|| panic!("{tool:?}: the reply must be an object: {result}"));
        assert!(
            !object.contains_key("review"),
            "{tool:?}: a plan with no review must not carry the key: {result}"
        );
        assert_eq!(result["version"], json!(0), "{tool:?}: {result}");
    }
}

#[tokio::test]
async fn plan_detail_replies_report_each_tasks_review_mark_and_omit_an_absent_one() {
    for tool in DETAIL_TOOLS {
        let result = call_detail(tool, approved_plan_with_marks()).await;

        let tasks = result["tasks"]
            .as_array()
            .unwrap_or_else(|| panic!("{tool:?}: the reply must carry a tasks array: {result}"));
        assert_eq!(tasks.len(), 3, "{tool:?}: {result}");
        assert_eq!(tasks[0]["review_mark"], json!("new"), "{tool:?}: {result}");
        assert_eq!(
            tasks[1]["review_mark"],
            json!("changed"),
            "{tool:?}: {result}"
        );
        let unmarked = tasks[2]
            .as_object()
            .unwrap_or_else(|| panic!("{tool:?}: a task must be an object: {result}"));
        assert!(
            !unmarked.contains_key("review_mark"),
            "{tool:?}: a task with no mark must not carry the key: {result}"
        );
    }
}

#[tokio::test]
async fn list_plans_reports_each_plans_review_state_and_unseen_changes() {
    let transport = ScriptedTransport::replying(DaemonMessage::PlanList {
        plans: vec![
            PlanSummary {
                plan_id: "plan-7".to_string(),
                title: "Parser rework".to_string(),
                owner_window: Some("w-owner".to_string()),
                task_count: 3,
                review: Some(PlanReview::Pending),
                unseen_changes: false,
            },
            PlanSummary {
                plan_id: "plan-8".to_string(),
                title: "Docs pass".to_string(),
                owner_window: Some("w-owner".to_string()),
                task_count: 2,
                review: Some(PlanReview::Approved),
                unseen_changes: true,
            },
            PlanSummary {
                plan_id: "plan-9".to_string(),
                title: "Older plan".to_string(),
                owner_window: Some("w-owner".to_string()),
                task_count: 1,
                review: None,
                unseen_changes: false,
            },
        ],
    });

    let result = list_plans(&transport)
        .await
        .expect("list_plans must succeed");

    let plans = result["plans"]
        .as_array()
        .unwrap_or_else(|| panic!("the reply must carry a plans array: {result}"));
    assert_eq!(plans.len(), 3, "{result}");
    assert_eq!(plans[0]["review"], json!("pending"), "{result}");
    assert_eq!(plans[0]["unseen_changes"], json!(false), "{result}");
    assert_eq!(plans[1]["review"], json!("approved"), "{result}");
    assert_eq!(plans[1]["unseen_changes"], json!(true), "{result}");
    let older = plans[2]
        .as_object()
        .unwrap_or_else(|| panic!("a plan summary must be an object: {result}"));
    assert!(
        !older.contains_key("review"),
        "a plan with no review must not carry the key: {result}"
    );
    assert_eq!(plans[2]["unseen_changes"], json!(false), "{result}");
}

// ---------------------------------------------------------------------
// The default role
// ---------------------------------------------------------------------

fn role_rows() -> Vec<RoleEntry> {
    vec![RoleEntry {
        name: "heavy".to_string(),
        profile: "claude-code".to_string(),
        model: Some("opus".to_string()),
        effort: Some("high".to_string()),
    }]
}

#[tokio::test]
async fn list_roles_reports_the_default_role() {
    let transport = ScriptedTransport::replying(DaemonMessage::RoleList {
        roles: role_rows(),
        default_role: Some("heavy".to_string()),
    });

    let result = list_roles(&transport)
        .await
        .expect("list_roles must succeed");

    assert_eq!(result["default_role"], json!("heavy"), "{result}");
    assert_eq!(result["roles"][0]["name"], json!("heavy"), "{result}");
}

#[tokio::test]
async fn list_roles_without_a_default_role_names_none() {
    let transport = ScriptedTransport::replying(DaemonMessage::RoleList {
        roles: role_rows(),
        default_role: None,
    });

    let result = list_roles(&transport)
        .await
        .expect("list_roles must succeed");

    assert!(
        result.get("default_role").is_none_or(Value::is_null),
        "with no default role the reply must not name one: {result}"
    );
}

#[tokio::test]
async fn open_task_window_reports_an_assignment_taken_from_the_default_role() {
    let transport = ScriptedTransport::replying(DaemonMessage::TaskWindowOpened {
        plan_id: "plan-7".to_string(),
        task_id: "added".to_string(),
        window_id: "w-task".to_string(),
        assignment: ResolvedAssignment {
            source: AssignmentSource::Default,
            assignment: Some(assignment("claude-code")),
        },
    });

    let result = open_task(&transport)
        .await
        .expect("open_task_window must succeed");

    assert_eq!(result["assignment"]["source"], json!("default"), "{result}");
    assert_eq!(
        result["assignment"]["assignment"]["profile"],
        json!("claude-code"),
        "{result}"
    );
}
