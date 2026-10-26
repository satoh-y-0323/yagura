#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The ticket tools end to end: the compiled `yagura-mcp` binary, spoken to
//! over MCP stdio the way a client does, against a real in-process daemon
//! (`IpcServer` + `WindowRegistry`, with the plan ledger and the ticket store
//! on one temporary SQLite file) that reads the repository's reference
//! `tickets.toml`. The bridge claims a real window opened in a temporary
//! repository, so the daemon sees it exactly as it sees an agent in a window.
//!
//! The daemon serves a named instance reserved for this file, so the spawned
//! bridge can only ever reach it, never a resident daemon.
//!
//! `cargo build -p yagura-daemon --bins` must have run first: the `echo_child`
//! binary is located next to the test executable.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use yagura_daemon::{
    DaemonConfig, IpcServer, OpenWindowRequest, RequestedCwd, WindowRegistry,
    ipc::{plan::PlanRuntime, ticket::TicketRuntime},
};
use yagura_os::{PeerAuth, PlatformIpc};
use yagura_protocol::{INSTANCE_ENV, STATUS_TOKEN_ENV, WINDOW_ID_ENV};
use yagura_test_support::ipc::connect_with_retry;
use yagura_test_support::paths::unique_temp_dir;
use yagura_test_support::process::echo_child_command;

mod common;

use common::{McpChild, tool_reply};

const INSTANCE: &str = "mcpticketflow";

/// The only shape `StatusToken::from_override` accepts; it lets the bridge
/// hold a valid token for the window the harness opened.
const TOKEN: &str = "0123456789abcdef0123456789abcdef";

const READY_TIMEOUT: Duration = Duration::from_secs(10);

/// A daemon whose workspace is a temporary directory with a bare `.git`
/// marker, and whose board rules are the repository's reference file.
struct Daemon {
    window_id: String,
    server: tokio::task::JoinHandle<Result<(), yagura_daemon::IpcError>>,
    // Keeps the registry, and so the window, alive for the whole test.
    _registry: Arc<WindowRegistry>,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        self.server.abort();
    }
}

fn reference_tickets_toml() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tickets.toml")
}

async fn start_daemon() -> Daemon {
    let dir = unique_temp_dir("yagura-mcp-ticket-e2e", "flow");
    let repo = dir.join("repo");
    std::fs::create_dir_all(repo.join(".git")).expect("create the workspace with a .git marker");
    let tickets_path = dir.join("tickets.toml");
    std::fs::copy(reference_tickets_toml(), &tickets_path)
        .expect("copy the reference tickets.toml");

    let config = DaemonConfig {
        status_token_override: Some(TOKEN.into()),
        store_db_path_resolved: Some(dir.join("yagura.db")),
        tickets_path: Some(tickets_path),
        ..DaemonConfig::default()
    };
    let registry = Arc::new(WindowRegistry::new(config.clone()));
    let ledger = Arc::new(
        tokio::task::spawn_blocking({
            let config = config.clone();
            move || PlanRuntime::from_config(&config)
        })
        .await
        .expect("PlanRuntime::from_config must not panic"),
    );
    let tickets = Arc::new(
        tokio::task::spawn_blocking({
            let config = config.clone();
            move || TicketRuntime::from_config(&config)
        })
        .await
        .expect("TicketRuntime::from_config must not panic"),
    );

    let namespace = yagura_os::resolve_instance_from(Some(OsStr::new(INSTANCE)))
        .expect("the instance id must be valid");
    let user_id = PlatformIpc::default()
        .current_user_id()
        .expect("current_user_id must resolve in a normal test-runner session");
    let endpoint = yagura_os::default_endpoint_owning(&user_id, &namespace);
    let server = Arc::new(
        IpcServer::new_with_ledger(registry.clone(), endpoint.clone(), config, ledger)
            .with_tickets(tickets),
    );
    let server = tokio::spawn(server.run());
    drop(connect_with_retry(&endpoint, READY_TIMEOUT).await);

    let window_id = registry
        .open(OpenWindowRequest {
            command: echo_child_command(),
            cwd: Some(RequestedCwd::from_caller(repo)),
            name: None,
            vendor: None,
            detection: None,
            on_name_taken: Default::default(),
        })
        .await
        .expect("the window must open")
        .window_id;
    Daemon {
        window_id,
        server,
        _registry: registry,
    }
}

/// Calls `tool` and returns its answer as JSON, failing if the tool failed.
async fn succeeds(bridge: &mut McpChild, tool: &str, arguments: Value) -> Value {
    let response = bridge.call_tool(tool, arguments).await;
    let (is_error, text) = tool_reply(&response);
    assert!(!is_error, "{tool} must succeed, got {text}");
    serde_json::from_str(&text)
        .unwrap_or_else(|err| panic!("{tool} must answer in JSON, got {text:?}: {err}"))
}

/// Calls `tool` and returns the text of its refusal, failing if it succeeded.
async fn refused(bridge: &mut McpChild, tool: &str, arguments: Value) -> String {
    let response = bridge.call_tool(tool, arguments).await;
    let (is_error, text) = tool_reply(&response);
    assert!(is_error, "{tool} must be refused, got {text}");
    text
}

#[tokio::test]
async fn an_ai_in_a_window_takes_a_ticket_from_todo_to_done_and_makes_a_plan_from_it() {
    let daemon = start_daemon().await;
    let mut bridge = McpChild::spawn(&[
        (INSTANCE_ENV, INSTANCE),
        (WINDOW_ID_ENV, &daemon.window_id),
        (STATUS_TOKEN_ENV, TOKEN),
    ]);
    bridge.initialize().await;

    // The rules come first: the user's states in board order, and no ticket yet.
    let board = succeeds(&mut bridge, "list_tickets", json!({})).await;
    let states: Vec<&str> = board["config"]["states"]
        .as_array()
        .expect("the board lists its states")
        .iter()
        .map(|state| state["name"].as_str().expect("a state has a name"))
        .collect();
    assert_eq!(
        states,
        [
            "todo",
            "in-progress",
            "done",
            "considering",
            "on-hold",
            "cancelled"
        ],
        "{board}"
    );
    assert!(
        board["config"]["guide"]
            .as_str()
            .is_some_and(|guide| !guide.is_empty()),
        "the guide must reach the AI: {board}"
    );
    assert_eq!(board["tickets"], json!([]), "{board}");

    // A new ticket starts in the first state, and its CRLF body arrives as LF.
    let created = succeeds(
        &mut bridge,
        "create_ticket",
        json!({ "title": "Show the board", "body": "first line\r\nsecond line" }),
    )
    .await;
    assert_eq!(created["number"], json!(1), "{created}");
    assert_eq!(created["state"], json!("todo"), "{created}");
    assert_eq!(
        created["body"],
        json!("first line\nsecond line"),
        "{created}"
    );

    let started = succeeds(
        &mut bridge,
        "update_ticket",
        json!({ "ticket": 1, "state": "in-progress" }),
    )
    .await;
    assert_eq!(started["state"], json!("in-progress"), "{started}");

    // A plan made from the ticket names it, and the ticket will name the plan.
    let plan = succeeds(
        &mut bridge,
        "create_plan",
        json!({
            "title": "Show the board",
            "tasks": [{ "task_id": "a", "title": "A", "depends_on": [], "writes": ["a.txt"] }],
            "ticket": 1,
        }),
    )
    .await;
    assert_eq!(plan["ticket"], json!(1), "{plan}");
    let plan_id = plan["plan_id"]
        .as_str()
        .expect("a plan has an id")
        .to_string();

    let with_result = succeeds(
        &mut bridge,
        "add_ticket_result",
        json!({ "ticket": 1, "text": "merged as abc123" }),
    )
    .await;
    assert_eq!(with_result["results"].as_array().map(Vec::len), Some(1));

    // The board refuses a pause that does not say what resumes it, and the
    // ticket stays where it was.
    let refusal = refused(
        &mut bridge,
        "update_ticket",
        json!({ "ticket": 1, "state": "on-hold" }),
    )
    .await;
    assert!(
        refusal.starts_with("invalid argument") && refusal.contains("resume_trigger"),
        "the refusal must be invalid_argument and name the missing field, got {refusal}"
    );
    let unchanged = succeeds(&mut bridge, "get_ticket", json!({ "ticket": 1 })).await;
    assert_eq!(unchanged["state"], json!("in-progress"), "{unchanged}");

    let finished = succeeds(
        &mut bridge,
        "update_ticket",
        json!({ "ticket": 1, "state": "done" }),
    )
    .await;
    assert_eq!(finished["state"], json!("done"), "{finished}");

    let last = succeeds(&mut bridge, "get_ticket", json!({ "ticket": 1 })).await;
    assert_eq!(last["state"], json!("done"), "{last}");
    assert_eq!(last["results"].as_array().map(Vec::len), Some(1), "{last}");
    assert_eq!(last["results"][0]["text"], json!("merged as abc123"));
    assert_eq!(last["plans"], json!([plan_id]), "{last}");

    bridge.kill().await;
}
