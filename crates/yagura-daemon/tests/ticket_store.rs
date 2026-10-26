#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// Seeds the database up to a limit with raw SQL, which only tests may do.
#![allow(clippy::disallowed_methods)]

//! Contracts for `yagura_daemon::ticket_store::TicketStore`: per-workspace numbering,
//! the required-field gate, append-only results, the resource limits and the board
//! listing. Every write goes through a permit issued by `ticket_permit` for a real
//! window opened in a real `WindowRegistry`; `tests/ticket_permit.rs` is the contract
//! for who gets which permit.

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rusqlite::params;

use yagura_daemon::DaemonConfig;
use yagura_daemon::control::RequestOrigin;
use yagura_daemon::ticket::{TicketConfig, TicketField, TicketState};
use yagura_daemon::ticket_permit::{self, TicketReadPermit, TicketWritePermit};
use yagura_daemon::ticket_store::{
    NewTicket, TicketChange, TicketLimitKind, TicketQuery, TicketStore, TicketStoreError,
};
use yagura_daemon::window::{OpenWindowRequest, RequestedCwd, WindowRegistry};
use yagura_protocol::{MilestoneInfo, TicketInfo};
use yagura_test_support::process::echo_child_path;

fn db_path(tag: &str) -> PathBuf {
    common::unique_temp_dir(tag).join("ticket.db")
}

fn repo_dir(tag: &str) -> PathBuf {
    let dir = common::unique_temp_dir(tag);
    std::fs::create_dir(dir.join(".git")).expect("create a bare .git marker directory");
    dir
}

fn raw(path: &Path) -> rusqlite::Connection {
    let conn = rusqlite::Connection::open(path).expect("a raw connection must open");
    conn.busy_timeout(std::time::Duration::from_secs(5))
        .expect("busy_timeout must be settable");
    conn
}

fn count(path: &Path, sql: &str) -> i64 {
    raw(path)
        .query_row(sql, [], |row| row.get(0))
        .expect("the count query must run")
}

async fn open_window_in(registry: &WindowRegistry, cwd: &Path) -> String {
    let child = echo_child_path();
    registry
        .open(OpenWindowRequest {
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
        })
        .await
        .expect("open should succeed")
        .window_id
}

/// A window opened in `cwd`, with its write and read permits.
struct Writer {
    write: TicketWritePermit,
    read: TicketReadPermit,
}

async fn writer_in(registry: &WindowRegistry, cwd: &Path) -> Writer {
    let window = open_window_in(registry, cwd).await;
    let origin = RequestOrigin::Window(window);
    Writer {
        write: ticket_permit::issue_write(&origin, registry).expect("write permit"),
        read: ticket_permit::issue_read(&origin, registry, None).expect("read permit"),
    }
}

fn state(name: &str, requires: &[&str]) -> TicketState {
    TicketState {
        name: name.to_string(),
        description: String::new(),
        requires: requires.iter().map(|name| (*name).to_string()).collect(),
    }
}

fn field(name: &str) -> TicketField {
    TicketField {
        name: name.to_string(),
        description: String::new(),
    }
}

/// The first state is deliberately not `todo`, so a default taken from anywhere but the
/// config's first state shows.
fn board_config() -> TicketConfig {
    TicketConfig {
        guide: String::new(),
        states: vec![
            state("triage", &[]),
            state("todo", &[]),
            state("in-progress", &[]),
            state("on-hold", &["resume_trigger"]),
            state("cancelled", &["reason"]),
        ],
        fields: vec![field("resume_trigger"), field("reason"), field("note")],
    }
}

fn new_ticket(title: &str) -> NewTicket {
    NewTicket {
        title: title.to_string(),
        body: format!("body of {title}"),
        milestone: None,
        state: None,
        fields: BTreeMap::new(),
    }
}

fn fields(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
        .collect()
}

fn numbers(store: &TicketStore, read: &TicketReadPermit, query: &TicketQuery) -> Vec<u32> {
    store
        .list_tickets(read, query)
        .expect("list_tickets must succeed")
        .tickets
        .iter()
        .map(|summary| summary.number)
        .collect()
}

fn now_millis() -> u64 {
    u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after epoch")
            .as_millis(),
    )
    .expect("millis fit u64")
}

fn milestone(name: &str, position: i64) -> MilestoneInfo {
    MilestoneInfo {
        name: name.to_string(),
        position,
        title: format!("Milestone {name}"),
        description: None,
    }
}

// Property 2: numbering per workspace.

#[tokio::test]
async fn numbers_start_at_one_per_workspace_and_a_subdirectory_shares_its_repository() {
    let path = db_path("ticket-store-numbering");
    let mut store = TicketStore::open(&path).expect("open store");
    let registry = WindowRegistry::new(DaemonConfig::default());
    let repo_a = repo_dir("ticket-store-numbering-a");
    let sub_a = repo_a.join("crates").join("inner");
    std::fs::create_dir_all(&sub_a).expect("create a subdirectory");
    let repo_b = repo_dir("ticket-store-numbering-b");
    let config = board_config();

    let a = writer_in(&registry, &repo_a).await;
    let a_sub = writer_in(&registry, &sub_a).await;
    let b = writer_in(&registry, &repo_b).await;

    let first = store
        .create_ticket(&a.write, &config, &new_ticket("a1"))
        .expect("create a1");
    let second = store
        .create_ticket(&a_sub.write, &config, &new_ticket("a2"))
        .expect("create a2 from the subdirectory");
    let other = store
        .create_ticket(&b.write, &config, &new_ticket("b1"))
        .expect("create b1");

    assert_eq!(first.ticket.number, 1);
    assert_eq!(
        second.ticket.number, 2,
        "a window in a subdirectory of the same repository must number in the same workspace"
    );
    assert_eq!(first.workspace_id, second.workspace_id);
    assert_eq!(
        other.ticket.number, 1,
        "another repository numbers from 1 on its own"
    );
    assert_ne!(other.workspace_id, first.workspace_id);

    assert_eq!(
        numbers(&store, &a.read, &TicketQuery::default()),
        vec![1, 2]
    );
    assert_eq!(
        numbers(&store, &a_sub.read, &TicketQuery::default()),
        vec![1, 2]
    );
    assert_eq!(numbers(&store, &b.read, &TicketQuery::default()), vec![1]);
    assert_eq!(
        store
            .get_ticket(&b.read, 1)
            .expect("b reads its own 1")
            .title,
        "b1",
        "ticket 1 of B is B's own, never A's"
    );
    assert!(
        matches!(
            store.get_ticket(&b.read, 2),
            Err(TicketStoreError::TicketNotFound)
        ),
        "A's ticket 2 must not be visible from B"
    );
}

#[tokio::test]
async fn the_same_path_as_a_directory_and_as_a_repository_are_two_workspaces() {
    let path = db_path("ticket-store-kind");
    let mut store = TicketStore::open(&path).expect("open store");
    let registry = WindowRegistry::new(DaemonConfig::default());
    let config = board_config();
    let dir = common::unique_temp_dir("ticket-store-kind-dir");

    let as_directory = writer_in(&registry, &dir).await;
    let in_directory = store
        .create_ticket(&as_directory.write, &config, &new_ticket("plain"))
        .expect("create in the directory workspace");

    std::fs::create_dir(dir.join(".git")).expect("turn the directory into a repository");
    let as_repository = writer_in(&registry, &dir).await;
    let in_repository = store
        .create_ticket(&as_repository.write, &config, &new_ticket("repo"))
        .expect("create in the repository workspace");

    assert_eq!(in_directory.ticket.number, 1);
    assert_eq!(in_repository.ticket.number, 1);
    assert_ne!(in_directory.workspace_id, in_repository.workspace_id);

    let list = ticket_permit::issue_list_workspaces(&RequestOrigin::Approver).expect("list");
    let mut kinds: Vec<String> = store
        .list_workspaces(&list)
        .expect("list workspaces")
        .into_iter()
        .map(|workspace| workspace.kind)
        .collect();
    kinds.sort();
    assert_eq!(kinds, vec!["directory".to_string(), "git".to_string()]);
}

#[tokio::test]
async fn reading_an_empty_workspace_returns_nothing_and_creates_no_workspace() {
    let path = db_path("ticket-store-read-only");
    let store = TicketStore::open(&path).expect("open store");
    let registry = WindowRegistry::new(DaemonConfig::default());
    let reader = writer_in(&registry, &repo_dir("ticket-store-read-only-repo")).await;

    let listing = store
        .list_tickets(&reader.read, &TicketQuery::default())
        .expect("listing an empty workspace must succeed");
    assert!(listing.tickets.is_empty());
    assert!(listing.milestones.is_empty());
    assert!(!listing.truncated);
    assert!(matches!(
        store.get_ticket(&reader.read, 1),
        Err(TicketStoreError::TicketNotFound)
    ));
    let list = ticket_permit::issue_list_workspaces(&RequestOrigin::Approver).expect("list");
    assert!(
        store
            .list_workspaces(&list)
            .expect("list workspaces")
            .is_empty(),
        "a read must not create the workspace's row"
    );
}

// Property 3: the default state and the required-field gate.

#[tokio::test]
async fn a_ticket_without_a_state_starts_in_the_configs_first_state() {
    let path = db_path("ticket-store-default-state");
    let mut store = TicketStore::open(&path).expect("open store");
    let registry = WindowRegistry::new(DaemonConfig::default());
    let w = writer_in(&registry, &repo_dir("ticket-store-default-state-repo")).await;

    let created = store
        .create_ticket(&w.write, &board_config(), &new_ticket("t"))
        .expect("create");
    assert_eq!(created.ticket.state, "triage");
    assert_eq!(created.ticket.body, "body of t");
    assert!(created.ticket.results.is_empty());
    assert!(created.ticket.plans.is_empty());
}

#[tokio::test]
async fn creating_into_a_state_with_an_empty_required_field_is_refused_and_writes_nothing() {
    let path = db_path("ticket-store-create-gate");
    let mut store = TicketStore::open(&path).expect("open store");
    let registry = WindowRegistry::new(DaemonConfig::default());
    let w = writer_in(&registry, &repo_dir("ticket-store-create-gate-repo")).await;
    let config = board_config();

    for value in [None, Some(""), Some("   ")] {
        let mut ticket = new_ticket("held");
        ticket.state = Some("on-hold".to_string());
        if let Some(value) = value {
            ticket.fields = fields(&[("resume_trigger", value)]);
        }
        let refused = store.create_ticket(&w.write, &config, &ticket);
        assert!(
            matches!(
                &refused,
                Err(TicketStoreError::MissingRequiredFields(names))
                    if names.as_slice() == ["resume_trigger"]
            ),
            "on-hold without a resume_trigger ({value:?}) must be refused naming the field, \
             got {refused:?}"
        );
    }
    assert_eq!(count(&path, "SELECT COUNT(*) FROM ticket"), 0);
    assert_eq!(count(&path, "SELECT COUNT(*) FROM ticket_field"), 0);
    assert_eq!(
        count(&path, "SELECT COUNT(*) FROM ticket_workspace"),
        0,
        "a refused first write must not leave the workspace's row behind"
    );

    let mut ticket = new_ticket("held");
    ticket.state = Some("on-hold".to_string());
    ticket.fields = fields(&[("resume_trigger", "after the release")]);
    let created = store
        .create_ticket(&w.write, &config, &ticket)
        .expect("on-hold with its resume_trigger must be accepted");
    assert_eq!(
        created.ticket.number, 1,
        "refused creates must not consume a number"
    );
    assert_eq!(created.ticket.state, "on-hold");
    assert_eq!(
        created.ticket.fields,
        fields(&[("resume_trigger", "after the release")])
    );
}

#[tokio::test]
async fn an_update_is_gated_on_the_fields_after_the_change() {
    let path = db_path("ticket-store-update-gate");
    let mut store = TicketStore::open(&path).expect("open store");
    let registry = WindowRegistry::new(DaemonConfig::default());
    let w = writer_in(&registry, &repo_dir("ticket-store-update-gate-repo")).await;
    let config = board_config();
    store
        .create_ticket(&w.write, &config, &new_ticket("t"))
        .expect("create");

    // Moving into on-hold without the field: refused, nothing changes.
    let before = store.get_ticket(&w.read, 1).expect("read");
    let refused = store.update_ticket(
        &w.write,
        &config,
        1,
        &TicketChange {
            state: Some("on-hold".to_string()),
            title: Some("renamed".to_string()),
            ..Default::default()
        },
    );
    assert!(
        matches!(
            &refused,
            Err(TicketStoreError::MissingRequiredFields(names))
                if names.as_slice() == ["resume_trigger"]
        ),
        "got {refused:?}"
    );
    assert_unchanged(&store, &w.read, &before);

    // Moving in with the field in the same update: accepted.
    let held = store
        .update_ticket(
            &w.write,
            &config,
            1,
            &TicketChange {
                state: Some("on-hold".to_string()),
                fields: fields(&[("resume_trigger", "when CI is green")]),
                ..Default::default()
            },
        )
        .expect("on-hold with its field in the same update must be accepted");
    assert_eq!(held.ticket.state, "on-hold");

    // Clearing the field while staying in on-hold: refused (the gate looks at fields too).
    let before = store.get_ticket(&w.read, 1).expect("read");
    let refused = store.update_ticket(
        &w.write,
        &config,
        1,
        &TicketChange {
            fields: fields(&[("resume_trigger", "")]),
            ..Default::default()
        },
    );
    assert!(
        matches!(&refused, Err(TicketStoreError::MissingRequiredFields(_))),
        "emptying a field the current state requires must be refused, got {refused:?}"
    );
    assert_unchanged(&store, &w.read, &before);

    // Leaving on-hold while clearing it: accepted, and "" removes the field.
    let resumed = store
        .update_ticket(
            &w.write,
            &config,
            1,
            &TicketChange {
                state: Some("todo".to_string()),
                fields: fields(&[("resume_trigger", "")]),
                ..Default::default()
            },
        )
        .expect("leaving on-hold while clearing its field must be accepted");
    assert_eq!(resumed.ticket.state, "todo");
    assert!(
        resumed.ticket.fields.is_empty(),
        "a field set to \"\" must be removed, got {:?}",
        resumed.ticket.fields
    );
}

#[tokio::test]
async fn title_body_and_milestone_updates_do_not_consult_the_required_fields() {
    let path = db_path("ticket-store-update-no-gate");
    let mut store = TicketStore::open(&path).expect("open store");
    let registry = WindowRegistry::new(DaemonConfig::default());
    let w = writer_in(&registry, &repo_dir("ticket-store-update-no-gate-repo")).await;

    // The ticket enters on-hold while on-hold requires nothing; the user then adds a
    // requirement.
    let lenient = TicketConfig {
        states: vec![state("todo", &[]), state("on-hold", &[])],
        ..board_config()
    };
    let mut ticket = new_ticket("t");
    ticket.state = Some("on-hold".to_string());
    store
        .create_ticket(&w.write, &lenient, &ticket)
        .expect("create under the lenient config");
    store
        .set_milestone(&w.write, &milestone("m1", 0))
        .expect("set milestone");

    let strict = board_config();
    let updated = store
        .update_ticket(
            &w.write,
            &strict,
            1,
            &TicketChange {
                title: Some("renamed".to_string()),
                body: Some("new body".to_string()),
                milestone: Some("m1".to_string()),
                ..Default::default()
            },
        )
        .expect("an update that touches neither state nor fields must not be gated");
    assert_eq!(updated.ticket.title, "renamed");
    assert_eq!(updated.ticket.body, "new body");
    assert_eq!(updated.ticket.milestone.as_deref(), Some("m1"));

    let cleared = store
        .update_ticket(
            &w.write,
            &strict,
            1,
            &TicketChange {
                milestone: Some(String::new()),
                ..Default::default()
            },
        )
        .expect("milestone \"\" must take the ticket out of its milestone");
    assert_eq!(cleared.ticket.milestone, None);
}

#[tokio::test]
async fn a_state_or_field_the_config_does_not_declare_is_refused() {
    let path = db_path("ticket-store-unknown-names");
    let mut store = TicketStore::open(&path).expect("open store");
    let registry = WindowRegistry::new(DaemonConfig::default());
    let w = writer_in(&registry, &repo_dir("ticket-store-unknown-names-repo")).await;
    let config = board_config();

    let mut ticket = new_ticket("t");
    ticket.state = Some("archived".to_string());
    assert!(matches!(
        store.create_ticket(&w.write, &config, &ticket),
        Err(TicketStoreError::UnknownState)
    ));
    let mut ticket = new_ticket("t");
    ticket.fields = fields(&[("priority", "high")]);
    assert!(matches!(
        store.create_ticket(&w.write, &config, &ticket),
        Err(TicketStoreError::UnknownField)
    ));
    assert_eq!(count(&path, "SELECT COUNT(*) FROM ticket"), 0);

    store
        .create_ticket(&w.write, &config, &new_ticket("t"))
        .expect("create");
    let before = store.get_ticket(&w.read, 1).expect("read");
    assert!(matches!(
        store.update_ticket(
            &w.write,
            &config,
            1,
            &TicketChange {
                state: Some("archived".to_string()),
                ..Default::default()
            }
        ),
        Err(TicketStoreError::UnknownState)
    ));
    assert!(matches!(
        store.update_ticket(
            &w.write,
            &config,
            1,
            &TicketChange {
                fields: fields(&[("priority", "high")]),
                ..Default::default()
            }
        ),
        Err(TicketStoreError::UnknownField)
    ));
    assert_unchanged(&store, &w.read, &before);
    assert!(matches!(
        store.update_ticket(
            &w.write,
            &config,
            7,
            &TicketChange {
                title: Some("x".to_string()),
                ..Default::default()
            }
        ),
        Err(TicketStoreError::TicketNotFound)
    ));
}

fn assert_unchanged(store: &TicketStore, read: &TicketReadPermit, before: &TicketInfo) {
    let after = store.get_ticket(read, before.number).expect("read after");
    assert_eq!(
        &after, before,
        "a refused update must leave the ticket (state, fields, updated_at) as it was"
    );
}

// Property 4: append-only results, timed by the store.

#[tokio::test]
async fn results_are_appended_in_order_and_timed_by_the_store() {
    let path = db_path("ticket-store-results");
    let mut store = TicketStore::open(&path).expect("open store");
    let registry = WindowRegistry::new(DaemonConfig::default());
    let w = writer_in(&registry, &repo_dir("ticket-store-results-repo")).await;
    store
        .create_ticket(&w.write, &board_config(), &new_ticket("t"))
        .expect("create");

    let before = now_millis();
    for text in ["measured 12ms", "merged as abc123", "pushed"] {
        store
            .add_result(&w.write, 1, text)
            .expect("adding a result must succeed");
    }
    let after = now_millis();

    let ticket = store.get_ticket(&w.read, 1).expect("read");
    let seqs: Vec<u64> = ticket.results.iter().map(|result| result.seq).collect();
    let texts: Vec<&str> = ticket
        .results
        .iter()
        .map(|result| result.text.as_str())
        .collect();
    assert_eq!(seqs, vec![1, 2, 3]);
    assert_eq!(texts, vec!["measured 12ms", "merged as abc123", "pushed"]);
    for result in &ticket.results {
        assert!(
            (before..=after).contains(&result.at),
            "a result's time must be the store's clock in milliseconds ({before}..={after}), \
             got {}",
            result.at
        );
    }
    assert!(matches!(
        store.add_result(&w.write, 2, "nowhere"),
        Err(TicketStoreError::TicketNotFound)
    ));
}

// Property 5 and B-8: resource limits.

#[tokio::test]
async fn a_workspace_holds_at_most_ten_thousand_tickets() {
    let path = db_path("ticket-store-ticket-limit");
    let mut store = TicketStore::open(&path).expect("open store");
    let registry = WindowRegistry::new(DaemonConfig::default());
    let w = writer_in(&registry, &repo_dir("ticket-store-ticket-limit-repo")).await;
    let config = board_config();
    let first = store
        .create_ticket(&w.write, &config, &new_ticket("t1"))
        .expect("create");

    {
        let mut conn = raw(&path);
        let tx = conn.transaction().expect("begin");
        for number in 2..=9_999_i64 {
            tx.execute(
                "INSERT INTO ticket (workspace_id, number, title, body, state, milestone, \
                 created_at, updated_at) VALUES (?1, ?2, 'seed', 'seed', 'todo', NULL, 0, 0)",
                params![first.workspace_id, number],
            )
            .expect("seed a ticket");
        }
        tx.execute(
            "UPDATE ticket_workspace SET next_number = 10000 WHERE workspace_id = ?1",
            params![first.workspace_id],
        )
        .expect("advance the counter");
        tx.commit().expect("commit");
    }

    let last = store
        .create_ticket(&w.write, &config, &new_ticket("t10000"))
        .expect("the 10,000th ticket must be accepted");
    assert_eq!(last.ticket.number, 10_000);
    let refused = store.create_ticket(&w.write, &config, &new_ticket("over"));
    assert!(
        matches!(
            refused,
            Err(TicketStoreError::ResourceExhausted(
                TicketLimitKind::Tickets
            ))
        ),
        "the 10,001st ticket must be refused, got {refused:?}"
    );
    assert_eq!(count(&path, "SELECT COUNT(*) FROM ticket"), 10_000);
}

#[tokio::test]
async fn a_ticket_holds_at_most_256_results() {
    let path = db_path("ticket-store-result-limit");
    let mut store = TicketStore::open(&path).expect("open store");
    let registry = WindowRegistry::new(DaemonConfig::default());
    let w = writer_in(&registry, &repo_dir("ticket-store-result-limit-repo")).await;
    let created = store
        .create_ticket(&w.write, &board_config(), &new_ticket("t"))
        .expect("create");

    {
        let mut conn = raw(&path);
        let tx = conn.transaction().expect("begin");
        for seq in 1..=255_i64 {
            tx.execute(
                "INSERT INTO ticket_result (workspace_id, number, seq, at, text) \
                 VALUES (?1, 1, ?2, 0, 'seed')",
                params![created.workspace_id, seq],
            )
            .expect("seed a result");
        }
        tx.commit().expect("commit");
    }

    let at_cap = store
        .add_result(&w.write, 1, "the 256th")
        .expect("the 256th result must be accepted");
    assert_eq!(
        at_cap.ticket.results.last().map(|result| result.seq),
        Some(256)
    );
    let before = store.get_ticket(&w.read, 1).expect("read");
    let refused = store.add_result(&w.write, 1, "the 257th");
    assert!(
        matches!(
            refused,
            Err(TicketStoreError::ResourceExhausted(
                TicketLimitKind::Results
            ))
        ),
        "the 257th result must be refused, got {refused:?}"
    );
    assert_unchanged(&store, &w.read, &before);
}

#[tokio::test]
async fn a_workspace_holds_at_most_64_milestones() {
    let path = db_path("ticket-store-milestone-limit");
    let mut store = TicketStore::open(&path).expect("open store");
    let registry = WindowRegistry::new(DaemonConfig::default());
    let w = writer_in(&registry, &repo_dir("ticket-store-milestone-limit-repo")).await;
    let first = store
        .set_milestone(&w.write, &milestone("m0", 0))
        .expect("set the first milestone");

    {
        let mut conn = raw(&path);
        let tx = conn.transaction().expect("begin");
        for i in 1..=62_i64 {
            tx.execute(
                "INSERT INTO ticket_milestone (workspace_id, name, position, title, description) \
                 VALUES (?1, ?2, ?3, 'seed', NULL)",
                params![first.workspace_id, format!("m{i}"), i],
            )
            .expect("seed a milestone");
        }
        tx.commit().expect("commit");
    }

    store
        .set_milestone(&w.write, &milestone("m63", 63))
        .expect("the 64th milestone must be accepted");
    let refused = store.set_milestone(&w.write, &milestone("m64", 64));
    assert!(
        matches!(
            refused,
            Err(TicketStoreError::ResourceExhausted(
                TicketLimitKind::Milestones
            ))
        ),
        "the 65th milestone must be refused, got {refused:?}"
    );
    assert_eq!(count(&path, "SELECT COUNT(*) FROM ticket_milestone"), 64);
    let changed = store
        .set_milestone(&w.write, &milestone("m0", 100))
        .expect("changing an existing milestone at the limit adds nothing and must be accepted");
    assert_eq!(changed.milestone.position, 100);
}

#[tokio::test]
async fn at_most_256_workspaces_exist_and_a_further_new_one_is_refused() {
    let path = db_path("ticket-store-workspace-limit");
    let mut store = TicketStore::open(&path).expect("open store");
    let registry = WindowRegistry::new(DaemonConfig::default());
    let config = board_config();
    let w = writer_in(&registry, &repo_dir("ticket-store-workspace-limit-a")).await;
    store
        .create_ticket(&w.write, &config, &new_ticket("first"))
        .expect("create in the first workspace");

    {
        let mut conn = raw(&path);
        let tx = conn.transaction().expect("begin");
        for i in 0..254_i64 {
            tx.execute(
                "INSERT INTO ticket_workspace (kind, key, next_number) VALUES ('directory', ?1, 1)",
                params![format!("/seeded/workspace-{i}")],
            )
            .expect("seed a workspace");
        }
        tx.commit().expect("commit");
    }

    let at_cap = writer_in(&registry, &repo_dir("ticket-store-workspace-limit-b")).await;
    store
        .create_ticket(&at_cap.write, &config, &new_ticket("256th"))
        .expect("the 256th workspace must be accepted");
    assert_eq!(count(&path, "SELECT COUNT(*) FROM ticket_workspace"), 256);

    let over = writer_in(&registry, &repo_dir("ticket-store-workspace-limit-c")).await;
    let refused = store.create_ticket(&over.write, &config, &new_ticket("257th"));
    assert!(
        matches!(refused, Err(TicketStoreError::TooManyWorkspaces)),
        "a 257th workspace must be refused, got {refused:?}"
    );
    let refused = store.set_milestone(&over.write, &milestone("m", 0));
    assert!(
        matches!(refused, Err(TicketStoreError::TooManyWorkspaces)),
        "a milestone is a first write too, got {refused:?}"
    );
    assert_eq!(count(&path, "SELECT COUNT(*) FROM ticket_workspace"), 256);
    assert_eq!(count(&path, "SELECT COUNT(*) FROM ticket"), 2);

    store
        .create_ticket(&w.write, &config, &new_ticket("second"))
        .expect("an existing workspace keeps writing at the limit");
}

// Property 6: listing.

#[tokio::test]
async fn the_listing_filters_orders_by_milestone_then_number_and_reports_truncation() {
    let path = db_path("ticket-store-listing");
    let mut store = TicketStore::open(&path).expect("open store");
    let registry = WindowRegistry::new(DaemonConfig::default());
    let w = writer_in(&registry, &repo_dir("ticket-store-listing-repo")).await;
    let config = board_config();

    // Equal positions sort by name: m-a before m-b; m-c sorts first by position.
    store
        .set_milestone(&w.write, &milestone("m-b", 1))
        .expect("m-b");
    store
        .set_milestone(&w.write, &milestone("m-c", 0))
        .expect("m-c");
    store
        .set_milestone(&w.write, &milestone("m-a", 1))
        .expect("m-a");

    let plan: [(Option<&str>, &str); 5] = [
        (None, "todo"),
        (Some("m-b"), "in-progress"),
        (Some("m-c"), "todo"),
        (Some("m-a"), "in-progress"),
        (Some("m-c"), "in-progress"),
    ];
    for (i, (milestone_name, state_name)) in plan.iter().enumerate() {
        let mut ticket = new_ticket(&format!("t{}", i + 1));
        ticket.milestone = milestone_name.map(str::to_string);
        ticket.state = Some((*state_name).to_string());
        store
            .create_ticket(&w.write, &config, &ticket)
            .expect("create");
    }

    let all = store
        .list_tickets(&w.read, &TicketQuery::default())
        .expect("list");
    assert_eq!(
        all.tickets
            .iter()
            .map(|summary| summary.number)
            .collect::<Vec<_>>(),
        vec![3, 5, 4, 2, 1],
        "board order is milestone position (then name), tickets without one last, then number"
    );
    assert_eq!(
        all.milestones
            .iter()
            .map(|m| m.name.as_str())
            .collect::<Vec<_>>(),
        vec!["m-c", "m-a", "m-b"]
    );
    assert!(!all.truncated);
    assert_eq!(all.tickets[0].title, "t3");
    assert_eq!(all.tickets[0].milestone.as_deref(), Some("m-c"));

    let in_progress = TicketQuery {
        state: Some("in-progress".to_string()),
        ..Default::default()
    };
    assert_eq!(numbers(&store, &w.read, &in_progress), vec![5, 4, 2]);
    let in_m_c = TicketQuery {
        milestone: Some("m-c".to_string()),
        ..Default::default()
    };
    assert_eq!(numbers(&store, &w.read, &in_m_c), vec![3, 5]);

    let cut = store
        .list_tickets(
            &w.read,
            &TicketQuery {
                limit: Some(2),
                ..Default::default()
            },
        )
        .expect("list with limit");
    assert_eq!(
        cut.tickets
            .iter()
            .map(|summary| summary.number)
            .collect::<Vec<_>>(),
        vec![3, 5]
    );
    assert!(
        cut.truncated,
        "more matched than the limit, so the listing must say it was cut"
    );
    let exact = store
        .list_tickets(
            &w.read,
            &TicketQuery {
                limit: Some(5),
                ..Default::default()
            },
        )
        .expect("list with an exact limit");
    assert_eq!(exact.tickets.len(), 5);
    assert!(
        !exact.truncated,
        "exactly the limit matched, so nothing was cut"
    );
}

// Property 7: milestones.

#[tokio::test]
async fn a_ticket_may_only_name_a_milestone_of_its_own_workspace() {
    let path = db_path("ticket-store-milestones");
    let mut store = TicketStore::open(&path).expect("open store");
    let registry = WindowRegistry::new(DaemonConfig::default());
    let config = board_config();
    let a = writer_in(&registry, &repo_dir("ticket-store-milestones-a")).await;
    let b = writer_in(&registry, &repo_dir("ticket-store-milestones-b")).await;

    let set = store
        .set_milestone(
            &a.write,
            &MilestoneInfo {
                name: "v1".to_string(),
                position: 2,
                title: "Version 1".to_string(),
                description: Some("first release".to_string()),
            },
        )
        .expect("create a milestone");
    assert_eq!(set.milestone.title, "Version 1");
    let changed = store
        .set_milestone(
            &a.write,
            &MilestoneInfo {
                name: "v1".to_string(),
                position: 5,
                title: "Version one".to_string(),
                description: None,
            },
        )
        .expect("update the milestone");
    assert_eq!(changed.workspace_id, set.workspace_id);
    let listing = store
        .list_tickets(&a.read, &TicketQuery::default())
        .expect("list");
    assert_eq!(
        listing.milestones,
        vec![MilestoneInfo {
            name: "v1".to_string(),
            position: 5,
            title: "Version one".to_string(),
            description: None,
        }],
        "setting an existing name changes it in place"
    );

    let mut in_v1 = new_ticket("t");
    in_v1.milestone = Some("v1".to_string());
    let created = store
        .create_ticket(&a.write, &config, &in_v1)
        .expect("a ticket may name its workspace's milestone");
    assert_eq!(created.ticket.milestone.as_deref(), Some("v1"));

    assert!(
        matches!(
            store.create_ticket(&b.write, &config, &in_v1),
            Err(TicketStoreError::UnknownMilestone)
        ),
        "B has no milestone v1, even though A does"
    );
    let mut missing = new_ticket("t");
    missing.milestone = Some("v9".to_string());
    assert!(matches!(
        store.create_ticket(&a.write, &config, &missing),
        Err(TicketStoreError::UnknownMilestone)
    ));
    assert!(matches!(
        store.update_ticket(
            &a.write,
            &config,
            1,
            &TicketChange {
                milestone: Some("v9".to_string()),
                ..Default::default()
            }
        ),
        Err(TicketStoreError::UnknownMilestone)
    ));
    assert_eq!(
        store
            .get_ticket(&a.read, 1)
            .expect("read")
            .milestone
            .as_deref(),
        Some("v1")
    );
}

// Property 8: a state the config no longer declares.

#[tokio::test]
async fn a_ticket_in_a_state_the_config_dropped_is_still_read_as_stored() {
    let path = db_path("ticket-store-dropped-state");
    let mut store = TicketStore::open(&path).expect("open store");
    let registry = WindowRegistry::new(DaemonConfig::default());
    let w = writer_in(&registry, &repo_dir("ticket-store-dropped-state-repo")).await;

    let with_review = TicketConfig {
        states: vec![state("todo", &[]), state("review", &[])],
        ..board_config()
    };
    let mut ticket = new_ticket("t");
    ticket.state = Some("review".to_string());
    ticket.fields = fields(&[("note", "kept")]);
    store
        .create_ticket(&w.write, &with_review, &ticket)
        .expect("create in review");

    // The config without `review` and without the `note` field is now in force.
    let without_review = TicketConfig {
        states: vec![state("todo", &[])],
        fields: Vec::new(),
        ..board_config()
    };
    let renamed = store
        .update_ticket(
            &w.write,
            &without_review,
            1,
            &TicketChange {
                title: Some("renamed".to_string()),
                ..Default::default()
            },
        )
        .expect("a title update must not trip over a state or field the config dropped");
    assert_eq!(renamed.ticket.state, "review");

    let ticket = store.get_ticket(&w.read, 1).expect("read");
    assert_eq!(ticket.state, "review");
    assert_eq!(ticket.fields, fields(&[("note", "kept")]));
    let listing = store
        .list_tickets(&w.read, &TicketQuery::default())
        .expect("list");
    assert_eq!(listing.tickets[0].state, "review");
}
