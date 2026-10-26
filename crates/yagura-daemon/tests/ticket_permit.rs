#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Contracts for `yagura_daemon::ticket_permit`: who may read and write a workspace's
//! tickets. Every permit here comes from a real `WindowRegistry` with real windows
//! opened in it, and is used against a real `TicketStore` on a temporary database, so
//! what is measured is what a permit lets a caller reach.

mod common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::time::sleep;

use yagura_daemon::DaemonConfig;
use yagura_daemon::control::RequestOrigin;
use yagura_daemon::ticket::{TicketConfig, TicketState};
use yagura_daemon::ticket_permit::{self, TicketPermitError};
use yagura_daemon::ticket_store::{NewTicket, TicketQuery, TicketStore};
use yagura_daemon::window::{OpenWindowRequest, RequestedCwd, WindowRegistry};
use yagura_protocol::WindowStatus;
use yagura_test_support::process::{echo_child_path, sleepy_command};

fn db_path(tag: &str) -> PathBuf {
    common::unique_temp_dir(tag).join("ticket.db")
}

fn repo_dir(tag: &str) -> PathBuf {
    let dir = common::unique_temp_dir(tag);
    std::fs::create_dir(dir.join(".git")).expect("create a bare .git marker directory");
    dir
}

fn echo_request(cwd: &Path) -> OpenWindowRequest {
    let child = echo_child_path();
    OpenWindowRequest {
        command: vec![
            child
                .to_str()
                .expect("echo_child path must be valid UTF-8")
                .to_string(),
        ],
        cwd: Some(RequestedCwd::from_caller(cwd.to_path_buf())),
        name: None,
        vendor: None,
        detection: None,
        on_name_taken: Default::default(),
    }
}

async fn open_window_in(registry: &WindowRegistry, cwd: &Path) -> String {
    registry
        .open(echo_request(cwd))
        .await
        .expect("open should succeed")
        .window_id
}

async fn open_short_lived_window_in(registry: &WindowRegistry, cwd: &Path) -> String {
    registry
        .open(OpenWindowRequest {
            command: sleepy_command("3"),
            cwd: Some(RequestedCwd::from_caller(cwd.to_path_buf())),
            name: None,
            vendor: None,
            detection: None,
            on_name_taken: Default::default(),
        })
        .await
        .expect("open should succeed")
        .window_id
}

async fn wait_for_exited(registry: &WindowRegistry, window_id: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let entry = registry
            .get(window_id)
            .expect("window must still be registered while waiting for it to exit");
        if entry.current_status() == WindowStatus::Exited {
            return;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!(
                "timed out waiting for the window to become Exited: current status {:?}",
                entry.current_status()
            );
        }
        sleep(Duration::from_millis(50)).await;
    }
}

fn one_state_config() -> TicketConfig {
    TicketConfig {
        guide: String::new(),
        states: vec![TicketState {
            name: "todo".to_string(),
            description: String::new(),
            requires: Vec::new(),
        }],
        fields: Vec::new(),
    }
}

fn new_ticket(title: &str) -> NewTicket {
    NewTicket {
        title: title.to_string(),
        body: "body".to_string(),
        milestone: None,
        state: None,
        fields: Default::default(),
    }
}

#[tokio::test]
async fn only_an_alive_window_gets_a_write_permit_and_it_writes_its_own_workspace() {
    let registry = WindowRegistry::new(DaemonConfig::default());
    let repo = repo_dir("ticket-permit-write");
    let mut store = TicketStore::open(&db_path("ticket-permit-write-db")).expect("open store");

    let alive = open_window_in(&registry, &repo).await;
    let permit = ticket_permit::issue_write(&RequestOrigin::Window(alive.clone()), &registry)
        .expect("a window with a running child must get a write permit");
    let written = store
        .create_ticket(&permit, &one_state_config(), &new_ticket("first"))
        .expect("the window's write permit must license a create in its own workspace");
    assert_eq!(written.ticket.number, 1);

    let read = ticket_permit::issue_read(&RequestOrigin::Window(alive), &registry, None)
        .expect("the same window must get a read permit for its own workspace");
    let listing = store
        .list_tickets(&read, &TicketQuery::default())
        .expect("reading the window's own workspace must succeed");
    assert_eq!(
        listing
            .tickets
            .iter()
            .map(|summary| summary.number)
            .collect::<Vec<_>>(),
        vec![1],
        "the write permit must have written into the workspace the window's read permit reads"
    );

    let dying = open_short_lived_window_in(&registry, &repo).await;
    wait_for_exited(&registry, &dying).await;
    assert!(
        matches!(
            ticket_permit::issue_write(&RequestOrigin::Window(dying), &registry),
            Err(TicketPermitError::InvalidState)
        ),
        "a window whose child has exited must not get a write permit"
    );
    assert!(
        matches!(
            ticket_permit::issue_write(
                &RequestOrigin::Window("w999999-never".to_string()),
                &registry
            ),
            Err(TicketPermitError::InvalidState)
        ),
        "an id this registry never opened must be refused the same way a dead window is"
    );
}

#[tokio::test]
async fn an_external_connection_gets_no_ticket_permit_at_all() {
    let registry = WindowRegistry::new(DaemonConfig::default());
    assert!(matches!(
        ticket_permit::issue_write(&RequestOrigin::External, &registry),
        Err(TicketPermitError::AuthFailed)
    ));
    assert!(matches!(
        ticket_permit::issue_read(&RequestOrigin::External, &registry, None),
        Err(TicketPermitError::AuthFailed)
    ));
    assert!(matches!(
        ticket_permit::issue_read(&RequestOrigin::External, &registry, Some(1)),
        Err(TicketPermitError::AuthFailed)
    ));
    assert!(matches!(
        ticket_permit::issue_list_workspaces(&RequestOrigin::External),
        Err(TicketPermitError::AuthFailed)
    ));
}

#[tokio::test]
async fn an_approver_reads_any_named_workspace_but_never_writes() {
    let registry = WindowRegistry::new(DaemonConfig::default());
    let mut store = TicketStore::open(&db_path("ticket-permit-approver-db")).expect("open store");
    let repo_a = repo_dir("ticket-permit-approver-a");
    let repo_b = repo_dir("ticket-permit-approver-b");

    let window_a = open_window_in(&registry, &repo_a).await;
    let window_b = open_window_in(&registry, &repo_b).await;
    let permit_a = ticket_permit::issue_write(&RequestOrigin::Window(window_a), &registry)
        .expect("window A must get a write permit");
    let permit_b = ticket_permit::issue_write(&RequestOrigin::Window(window_b), &registry)
        .expect("window B must get a write permit");
    let in_a = store
        .create_ticket(&permit_a, &one_state_config(), &new_ticket("in a"))
        .expect("create in A");
    let in_b = store
        .create_ticket(&permit_b, &one_state_config(), &new_ticket("in b"))
        .expect("create in B");

    assert!(
        matches!(
            ticket_permit::issue_write(&RequestOrigin::Approver, &registry),
            Err(TicketPermitError::AuthFailed)
        ),
        "an approver must not get a write permit"
    );

    let list_permit = ticket_permit::issue_list_workspaces(&RequestOrigin::Approver)
        .expect("an approver must be able to list workspaces");
    let mut ids: Vec<i64> = store
        .list_workspaces(&list_permit)
        .expect("listing workspaces must succeed")
        .into_iter()
        .map(|workspace| workspace.workspace_id)
        .collect();
    ids.sort_unstable();
    let mut expected = vec![in_a.workspace_id, in_b.workspace_id];
    expected.sort_unstable();
    assert_eq!(ids, expected);

    for (workspace_id, title) in [(in_a.workspace_id, "in a"), (in_b.workspace_id, "in b")] {
        let read =
            ticket_permit::issue_read(&RequestOrigin::Approver, &registry, Some(workspace_id))
                .expect("an approver must get a read permit for a named workspace");
        let ticket = store
            .get_ticket(&read, 1)
            .expect("an approver must read ticket 1 of the named workspace");
        assert_eq!(ticket.title, title);
    }

    assert!(
        matches!(
            ticket_permit::issue_read(&RequestOrigin::Approver, &registry, None),
            Err(TicketPermitError::WorkspaceRequired)
        ),
        "an approver has no workspace of its own, so it must name one to read"
    );
}

#[tokio::test]
async fn a_window_reads_only_its_own_workspace_and_cannot_name_another() {
    let registry = WindowRegistry::new(DaemonConfig::default());
    let repo = repo_dir("ticket-permit-window-read");
    let window = open_window_in(&registry, &repo).await;

    assert!(
        matches!(
            ticket_permit::issue_read(&RequestOrigin::Window(window.clone()), &registry, Some(1)),
            Err(TicketPermitError::WorkspaceNotAllowed)
        ),
        "a window's workspace comes from its registration, never from an argument"
    );
    assert!(
        matches!(
            ticket_permit::issue_list_workspaces(&RequestOrigin::Window(window)),
            Err(TicketPermitError::AuthFailed)
        ),
        "listing every workspace is the approver's alone"
    );
}
