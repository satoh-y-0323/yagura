#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// `ChatStore::ensure_room` is called directly here: it is the unit under test.
#![allow(clippy::disallowed_methods)]

//! Contracts for `yagura_daemon::chat_store::ChatStore`, the room's persistence port.
//! Nothing here spawns a PTY, so the file is OS-independent.
//!
//! Every database lives under `common::unique_temp_dir`. Verification reads that do not
//! go through `ChatStore` itself use `store::open_readonly` and raw SQL against the chat
//! tables, the same pattern the session-store tests use for `snapshot_windows`.

mod common;

use std::path::{Path, PathBuf};

use common::store_fixtures::{capture_warn_events, count_warns};
use rusqlite::OptionalExtension;
use yagura_daemon::chat_store::{ChatStore, EnsureRoom, NewChatMessage};
use yagura_daemon::room::{ChatDeliveryState, ChatLimits, DeliveryOutcome, HaltReason};
use yagura_daemon::store;
use yagura_daemon::workspace::WorkspaceKind;
use yagura_protocol::UnknownReason;

const ROOM_ID: &str = "main";

fn db_path(tag: &str) -> PathBuf {
    common::unique_temp_dir(tag).join("chat.db")
}

fn open_ro(path: &Path) -> rusqlite::Connection {
    store::open_readonly(path).expect("open_readonly must succeed")
}

fn chat_message_seqs(path: &Path) -> Vec<i64> {
    let conn = open_ro(path);
    let mut stmt = conn
        .prepare("SELECT seq FROM chat_message WHERE room_id = ?1 ORDER BY seq")
        .expect("the chat_message query must prepare");
    stmt.query_map(rusqlite::params![ROOM_ID], |row| row.get(0))
        .expect("the chat_message query must run")
        .map(|row| row.expect("chat_message row must decode"))
        .collect()
}

fn chat_delivery_seqs(path: &Path) -> Vec<i64> {
    let conn = open_ro(path);
    let mut stmt = conn
        .prepare("SELECT DISTINCT seq FROM chat_delivery WHERE room_id = ?1 ORDER BY seq")
        .expect("the chat_delivery query must prepare");
    stmt.query_map(rusqlite::params![ROOM_ID], |row| row.get(0))
        .expect("the chat_delivery query must run")
        .map(|row| row.expect("chat_delivery row must decode"))
        .collect()
}

fn delivery_state(path: &Path, seq: i64, window_id: &str) -> Option<String> {
    let conn = open_ro(path);
    conn.query_row(
        "SELECT state FROM chat_delivery WHERE room_id = ?1 AND seq = ?2 AND window_id = ?3",
        rusqlite::params![ROOM_ID, seq, window_id],
        |row| row.get(0),
    )
    .optional()
    .expect("the chat_delivery lookup must not error")
}

/// The `unknown_reason` column exactly as stored, without decoding it.
fn delivery_reason_column(path: &Path, seq: i64, window_id: &str) -> Option<String> {
    let conn = open_ro(path);
    conn.query_row(
        "SELECT unknown_reason FROM chat_delivery \
         WHERE room_id = ?1 AND seq = ?2 AND window_id = ?3",
        rusqlite::params![ROOM_ID, seq, window_id],
        |row| row.get(0),
    )
    .expect("the chat_delivery row must exist")
}

fn silence_noted_for(path: &Path, seq: i64) -> bool {
    let conn = open_ro(path);
    let value: i64 = conn
        .query_row(
            "SELECT silence_noted FROM chat_message WHERE room_id = ?1 AND seq = ?2",
            rusqlite::params![ROOM_ID, seq],
            |row| row.get(0),
        )
        .expect("the chat_message row must exist");
    value != 0
}

/// `(halted_reason, halted_at_ms, needs_user)` of the `main` room.
fn chat_room_state(path: &Path) -> (Option<String>, Option<i64>, bool) {
    let conn = open_ro(path);
    conn.query_row(
        "SELECT halted_reason, halted_at_ms, needs_user FROM chat_room WHERE room_id = ?1",
        rusqlite::params![ROOM_ID],
        |row| Ok((row.get(0)?, row.get(1)?, row.get::<_, i64>(2)? != 0)),
    )
    .expect("the chat_room row for main must exist")
}

fn generous_limits() -> ChatLimits {
    ChatLimits {
        repetition_count: 1000,
        repetition_window: 1000,
        repetition_distance: 6,
        monologue_max: 1000,
        turn_message_max: 1000,
        silence_timeout_secs: 300,
        history_max: 5000,
    }
}

fn limits_with_history_max(history_max: u64) -> ChatLimits {
    ChatLimits {
        history_max,
        ..generous_limits()
    }
}

fn limits_with_monologue_max(monologue_max: u64) -> ChatLimits {
    ChatLimits {
        monologue_max,
        ..generous_limits()
    }
}

fn window_message(window_id: &str, name: &str, body: &str, at_ms: i64) -> NewChatMessage {
    NewChatMessage {
        room_id: ROOM_ID.to_string(),
        at_ms,
        from_kind: "window".to_string(),
        from_window_id: Some(window_id.to_string()),
        from_name: name.to_string(),
        body: body.to_string(),
        mentions: Vec::new(),
        mentions_user: false,
        broadcast: false,
    }
}

// `ChatStore::open` must lay down the schema on its own -- `open_store` is never
// called first here.
#[test]
fn open_creates_the_schema_without_calling_open_store_first() {
    let path = db_path("chat-open-fresh");
    let mut chat =
        ChatStore::open(&path).expect("ChatStore::open must create the schema on a brand-new path");

    let version: i64 = {
        let conn = open_ro(&path);
        conn.query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("user_version must be readable")
    };
    assert_eq!(
        version,
        store::MIGRATIONS.len() as i64,
        "ChatStore::open must migrate an untouched path all the way to the latest schema"
    );

    let appended = chat
        .append_message(window_message("w-a", "Alpha", "hello", 1_000), &generous_limits())
        .expect("append_message must succeed right after ChatStore::open, with no separate open_store call");
    assert_eq!(
        appended.seq, 1,
        "the first message ever posted to a fresh room must get seq 1"
    );
}

#[test]
fn append_message_assigns_seq_starting_at_one_and_increasing() {
    let path = db_path("chat-seq");
    let mut chat = ChatStore::open(&path).expect("ChatStore::open must succeed");
    let limits = generous_limits();

    let seqs: Vec<i64> = (0..3i64)
        .map(|i| {
            chat.append_message(
                window_message("w-a", "Alpha", &format!("body-{i}"), 1_000 + i),
                &limits,
            )
            .expect("append_message must succeed")
            .seq
        })
        .collect();

    assert_eq!(
        seqs,
        vec![1, 2, 3],
        "seq must be assigned 1, 2, 3 in the order the messages were appended"
    );
}

#[test]
fn append_message_evicts_the_oldest_rows_past_history_max_with_their_deliveries() {
    let path = db_path("chat-history-max");
    let mut chat = ChatStore::open(&path).expect("ChatStore::open must succeed");
    let limits = limits_with_history_max(3);

    for i in 0..5i64 {
        let appended = chat
            .append_message(
                window_message("w-a", "Alpha", &format!("body-{i}"), 1_000 + i),
                &limits,
            )
            .expect("append_message must succeed");
        chat.set_delivery(
            ROOM_ID,
            appended.seq,
            "w-b",
            DeliveryOutcome::Taken,
            2_000 + i,
        )
        .expect("set_delivery must succeed");
    }

    assert_eq!(
        chat_message_seqs(&path),
        vec![3, 4, 5],
        "only the most recent history_max rows may remain in chat_message"
    );
    assert_eq!(
        chat_delivery_seqs(&path),
        vec![3, 4, 5],
        "a message's chat_delivery rows must be evicted together with the message itself"
    );
}

#[test]
fn each_write_method_commits_on_its_own_and_round_trips() {
    let path = db_path("chat-roundtrip");
    let mut chat = ChatStore::open(&path).expect("ChatStore::open must succeed");
    let limits = generous_limits();

    let appended = chat
        .append_message(window_message("w-a", "Alpha", "hi", 1_000), &limits)
        .expect("append_message must succeed");

    chat.set_delivery(
        ROOM_ID,
        appended.seq,
        "w-b",
        DeliveryOutcome::Pending,
        2_000,
    )
    .expect("set_delivery must succeed");
    assert_eq!(
        delivery_state(&path, appended.seq, "w-b"),
        Some("pending".to_string())
    );

    chat.set_delivery(ROOM_ID, appended.seq, "w-b", DeliveryOutcome::Taken, 2_100)
        .expect("a second set_delivery for the same key must succeed");
    assert_eq!(
        delivery_state(&path, appended.seq, "w-b"),
        Some("taken".to_string()),
        "a second set_delivery for the same (room, seq, window) must update the row, not \
         duplicate it"
    );

    chat.mark_silence_noted(ROOM_ID, appended.seq)
        .expect("mark_silence_noted must succeed");
    assert!(silence_noted_for(&path, appended.seq));

    chat.set_halt(ROOM_ID, HaltReason::Repetition, 3_000)
        .expect("set_halt must succeed on its own, without a prior append_message halt");
    let (reason, at_ms, needs_user) = chat_room_state(&path);
    assert_eq!(reason.as_deref(), Some("repetition"));
    assert!(at_ms.is_some());
    assert!(
        needs_user,
        "set_halt must also raise needs_user, in the same transaction"
    );

    chat.set_needs_user(ROOM_ID, false)
        .expect("set_needs_user must succeed");
    let (_, _, needs_user_after) = chat_room_state(&path);
    assert!(
        !needs_user_after,
        "set_needs_user must be independently settable"
    );

    chat.clear_halt_and_needs_user(ROOM_ID)
        .expect("clear_halt_and_needs_user must succeed");
    let (reason_after, at_ms_after, needs_user_final) = chat_room_state(&path);
    assert!(reason_after.is_none());
    assert!(at_ms_after.is_none());
    assert!(!needs_user_final);
}

// The store takes and returns one delivery outcome, and what it writes
// into the `state` column must be the string `room::ChatDeliveryState`
// produces (and reads back through), for every one of the six delivery
// states -- not a value only the caller's own literal happens to agree with
// `chat.rs`'s.
#[test]
fn set_delivery_round_trips_every_delivery_outcome_through_the_stored_wire_string() {
    let path = db_path("chat-delivery-state-round-trip");
    let mut chat = ChatStore::open(&path).expect("ChatStore::open must succeed");
    let limits = generous_limits();

    let appended = chat
        .append_message(window_message("w-a", "Alpha", "hi @beta", 1_000), &limits)
        .expect("append_message must succeed");

    for outcome in [
        DeliveryOutcome::Pending,
        DeliveryOutcome::Taken,
        DeliveryOutcome::UndeliveredUnknown(None),
        DeliveryOutcome::UndeliveredUnknown(Some(UnknownReason::DeclaredUnknown)),
        DeliveryOutcome::UndeliveredHalted,
        DeliveryOutcome::UndeliveredExhausted,
        DeliveryOutcome::UndeliveredRestart,
    ] {
        chat.set_delivery(ROOM_ID, appended.seq, "w-b", outcome, 2_000)
            .expect("set_delivery must accept every delivery outcome");
        let wire = delivery_state(&path, appended.seq, "w-b")
            .expect("w-b must have a delivery row after set_delivery");
        assert_eq!(
            ChatDeliveryState::parse(&wire),
            Some(outcome.state()),
            "the string chat_store persisted for {outcome:?} must parse back to the state \
             it projects to"
        );
        assert_eq!(
            deliveries_sorted(&chat, appended.seq),
            vec![("w-b".to_string(), outcome)],
            "reading the row back must rebuild the very outcome that was recorded"
        );
    }
}

#[test]
fn read_after_returns_ascending_entries_past_the_cursor_and_the_next_cursor() {
    let path = db_path("chat-read-after");
    let mut chat = ChatStore::open(&path).expect("ChatStore::open must succeed");
    let limits = generous_limits();
    for i in 0..5i64 {
        chat.append_message(
            window_message("w-a", "Alpha", &format!("body-{i}"), 1_000 + i),
            &limits,
        )
        .expect("append_message must succeed");
    }

    let page = chat
        .read_after(ROOM_ID, 2, 10)
        .expect("read_after must succeed");
    let seqs: Vec<i64> = page.messages.iter().map(|m| m.seq).collect();
    assert_eq!(seqs, vec![3, 4, 5]);
    assert_eq!(page.next_after_seq, 5);

    let empty = chat
        .read_after(ROOM_ID, 5, 10)
        .expect("read_after must succeed with zero results");
    assert!(empty.messages.is_empty());
    assert_eq!(
        empty.next_after_seq, 5,
        "with zero results, next_after_seq must stay at the cursor that was passed in"
    );
}

#[test]
fn read_tail_returns_the_most_recent_entries_in_ascending_order() {
    let path = db_path("chat-read-tail");
    let mut chat = ChatStore::open(&path).expect("ChatStore::open must succeed");
    let limits = generous_limits();
    for i in 0..5i64 {
        chat.append_message(
            window_message("w-a", "Alpha", &format!("body-{i}"), 1_000 + i),
            &limits,
        )
        .expect("append_message must succeed");
    }

    let tail = chat.read_tail(ROOM_ID, 2).expect("read_tail must succeed");
    let seqs: Vec<i64> = tail.iter().map(|m| m.seq).collect();
    assert_eq!(
        seqs,
        vec![4, 5],
        "read_tail must return the last `limit` rows, oldest to newest"
    );
}

#[test]
fn last_window_seq_reports_a_windows_own_most_recent_post_or_none() {
    let path = db_path("chat-last-window-seq");
    let mut chat = ChatStore::open(&path).expect("ChatStore::open must succeed");
    let limits = generous_limits();

    chat.append_message(window_message("w-a", "Alpha", "one", 1_000), &limits)
        .expect("append_message must succeed");
    chat.append_message(window_message("w-b", "Beta", "two", 1_001), &limits)
        .expect("append_message must succeed");
    let last_a = chat
        .append_message(window_message("w-a", "Alpha", "three", 1_002), &limits)
        .expect("append_message must succeed");

    assert_eq!(
        chat.last_window_seq(ROOM_ID, "w-a")
            .expect("last_window_seq must succeed"),
        Some(last_a.seq)
    );
    assert_eq!(
        chat.last_window_seq(ROOM_ID, "w-c")
            .expect("last_window_seq must succeed for a window that never posted"),
        None,
        "a window that never posted must read back as None, not 0"
    );
}

// A real store-thread snapshot writer hammers the same file while chat appends
// run on a second connection.
#[test]
fn append_survives_a_concurrent_snapshot_writer_on_the_same_database_file() {
    let path = db_path("chat-concurrent-snapshot");
    let mut chat = ChatStore::open(&path).expect("ChatStore::open must succeed");

    let snapshot_store = store::open_store(&path)
        .expect("open_store must reuse the schema ChatStore::open already migrated");
    let tx = store::spawn_store_thread(snapshot_store);

    let driver_tx = tx.clone();
    let driver = std::thread::spawn(move || {
        for i in 0..200i64 {
            let _ = driver_tx.send(vec![store::SnapshotWindow {
                position: 0,
                command: vec!["cmd.exe".to_string()],
                cwd: "C:/work".to_string(),
                name: format!("w{i}"),
                vendor: None,
                detection: None,
                layout_json: None,
                profile: None,
                model: None,
                effort: None,
            }]);
        }
    });

    let limits = generous_limits();
    let mut errors = 0usize;
    for i in 0..50i64 {
        if chat
            .append_message(
                window_message("w-a", "Alpha", &format!("body-{i}"), 1_000 + i),
                &limits,
            )
            .is_err()
        {
            errors += 1;
        }
    }

    driver
        .join()
        .expect("the snapshot-driving thread must not panic");
    drop(tx);

    assert_eq!(
        errors, 0,
        "no append_message call may fail while a concurrent snapshot writer holds the file"
    );
    assert_eq!(
        chat_message_seqs(&path).len(),
        50,
        "all 50 chat appends must have landed in chat_message"
    );
}

// `write_snapshot_or_warn` is called directly from the test thread (not through
// `spawn_store_thread`), so its warns can actually be counted, while a separate
// thread keeps `ChatStore::append_message` busy on the same file.
#[test]
fn concurrent_write_snapshot_or_warn_calls_do_not_warn_while_chat_appends() {
    let path = db_path("chat-concurrent-warngate");
    let mut chat = ChatStore::open(&path).expect("ChatStore::open must succeed");

    // This test measures lock coordination between two IMMEDIATE writers, not the
    // production 5s busy_timeout budget, so give it headroom against wall-clock jitter.
    let mut snapshot_conn =
        rusqlite::Connection::open(&path).expect("a second connection must open");
    snapshot_conn
        .pragma_update(None, "busy_timeout", 120_000i64)
        .expect("busy_timeout must apply to the second connection");

    let limits = generous_limits();
    let chat_driver = std::thread::spawn(move || {
        for i in 0..50i64 {
            let _ = chat.append_message(
                window_message("w-a", "Alpha", &format!("body-{i}"), 1_000 + i),
                &limits,
            );
        }
    });

    let gate = store::WarnGate::new();
    let rows = vec![store::SnapshotWindow {
        position: 0,
        command: vec!["cmd.exe".to_string()],
        cwd: "C:/work".to_string(),
        name: "final".to_string(),
        vendor: None,
        detection: None,
        layout_json: None,
        profile: None,
        model: None,
        effort: None,
    }];
    let (warns, ()) = count_warns(|| {
        for _ in 0..20 {
            store::write_snapshot_or_warn(&gate, &mut snapshot_conn, &rows);
        }
    });

    chat_driver
        .join()
        .expect("the chat-appending thread must not panic");

    assert_eq!(
        warns, 0,
        "write_snapshot_or_warn must not warn while a second connection writes chat \
         appends concurrently"
    );
    let names: Vec<String> = {
        let conn = open_ro(&path);
        let mut stmt = conn
            .prepare("SELECT name FROM snapshot_windows ORDER BY position")
            .expect("the snapshot_windows query must prepare");
        stmt.query_map([], |row| row.get(0))
            .expect("the snapshot_windows query must run")
            .map(|row| row.expect("snapshot_windows row must decode"))
            .collect()
    };
    assert_eq!(
        names,
        vec!["final".to_string()],
        "the last written snapshot configuration must be the one that reads back"
    );
}

#[test]
fn open_refuses_a_future_schema_version() {
    let path = db_path("chat-future-version");
    {
        let conn = rusqlite::Connection::open(&path).expect("a seed connection must open");
        conn.execute_batch("PRAGMA user_version = 99;")
            .expect("seeding a future-version database must succeed");
    }

    let err = ChatStore::open(&path)
        .expect_err("ChatStore::open must refuse a database from a future schema version");
    assert!(
        matches!(
            err,
            store::StoreError::FutureSchemaVersion { found: 99, .. }
        ),
        "the refusal must carry the future version, got: {err:?}"
    );
}

#[test]
fn append_message_halts_atomically_with_the_system_notice_and_needs_user() {
    let path = db_path("chat-halt-atomic");
    let mut chat = ChatStore::open(&path).expect("ChatStore::open must succeed");
    let limits = limits_with_monologue_max(1);

    let appended = chat
        .append_message(
            window_message("w-a", "Alpha", "only one message needed", 1_000),
            &limits,
        )
        .expect("append_message must succeed even though it halts the room");
    assert_eq!(
        appended.halt,
        Some(HaltReason::Monologue),
        "a single post must already trip monologue_max = 1"
    );

    let (reason, at_ms, needs_user) = chat_room_state(&path);
    assert_eq!(reason.as_deref(), Some("monologue"));
    assert!(at_ms.is_some());
    assert!(
        needs_user,
        "the halt must raise needs_user in the same transaction as the triggering post"
    );

    let tail = chat.read_tail(ROOM_ID, 10).expect("read_tail must succeed");
    let system_row = tail
        .iter()
        .find(|m| m.from_kind == "system")
        .expect("a system notice must be inserted in the same transaction as the halt");
    assert!(
        system_row.mentions_user,
        "the system notice must mention @user"
    );
    assert_eq!(
        system_row.seq,
        appended.seq + 1,
        "the system notice must be the very next entry after the triggering message, from \
         the same seq counter"
    );
}

/// [`window_message`] addressed at `mentions`: the resolved window ids the
/// post is delivered to.
fn window_message_to(
    window_id: &str,
    name: &str,
    body: &str,
    at_ms: i64,
    mentions: &[&str],
) -> NewChatMessage {
    NewChatMessage {
        mentions: mentions.iter().map(|id| (*id).to_string()).collect(),
        ..window_message(window_id, name, body, at_ms)
    }
}

/// Every persisted delivery for `seq`, rebuilt into its outcome, sorted by window id.
fn deliveries_sorted(chat: &ChatStore, seq: i64) -> Vec<(String, DeliveryOutcome)> {
    let mut rows = chat
        .deliveries_for_seq(ROOM_ID, seq)
        .expect("deliveries_for_seq must succeed");
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    rows
}

/// The same deliveries as seen by the whole-room read the history assembly uses, narrowed
/// to `seq` and sorted by window id. Both reads go through one reconstruction point, so a
/// row either survives in both or in neither.
fn deliveries_in_room_sorted(chat: &ChatStore, seq: i64) -> Vec<(String, DeliveryOutcome)> {
    let mut by_seq = chat
        .deliveries_by_room(ROOM_ID)
        .expect("deliveries_by_room must succeed");
    let mut rows = by_seq.remove(&seq).unwrap_or_default();
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    rows
}

#[test]
fn append_message_records_every_destination_as_pending_in_the_posts_own_transaction() {
    let path = db_path("chat-append-pending");
    let mut chat = ChatStore::open(&path).expect("ChatStore::open must succeed");

    let appended = chat
        .append_message(
            window_message_to("w-a", "Alpha", "hello both", 1_000, &["w-b", "w-c"]),
            &generous_limits(),
        )
        .expect("append_message must succeed");

    assert_eq!(
        deliveries_sorted(&chat, appended.seq),
        vec![
            ("w-b".to_string(), DeliveryOutcome::Pending),
            ("w-c".to_string(), DeliveryOutcome::Pending),
        ],
        "every addressed destination must already carry a pending row when append_message \
         returns, so the first read after a post shows the post as being delivered"
    );
    assert_eq!(
        delivery_state(&path, appended.seq, "w-b").as_deref(),
        Some("pending"),
        "the pending rows must be committed with the post itself, not held in memory"
    );
}

#[test]
fn append_message_records_no_delivery_for_a_post_that_addresses_nobody() {
    let path = db_path("chat-append-no-destination");
    let mut chat = ChatStore::open(&path).expect("ChatStore::open must succeed");

    let appended = chat
        .append_message(
            window_message_to("w-a", "Alpha", "thinking out loud", 1_000, &[]),
            &generous_limits(),
        )
        .expect("append_message must succeed");

    assert!(
        deliveries_sorted(&chat, appended.seq).is_empty(),
        "a post nobody is addressed by must carry no delivery row at all"
    );
}

#[test]
fn append_message_records_a_destination_once_even_when_it_is_named_twice() {
    let path = db_path("chat-append-repeated-destination");
    let mut chat = ChatStore::open(&path).expect("ChatStore::open must succeed");

    let appended = chat
        .append_message(
            window_message_to("w-a", "Alpha", "hello again", 1_000, &["w-b", "w-b"]),
            &generous_limits(),
        )
        .expect("append_message must not fail on a destination named twice");

    assert_eq!(
        deliveries_sorted(&chat, appended.seq),
        vec![("w-b".to_string(), DeliveryOutcome::Pending)],
        "one destination is one row, however many times the body named it"
    );
}

#[test]
fn append_message_records_pending_for_a_post_that_halts_the_room() {
    let path = db_path("chat-append-pending-halt");
    let mut chat = ChatStore::open(&path).expect("ChatStore::open must succeed");

    let appended = chat
        .append_message(
            window_message_to("w-a", "Alpha", "only one message needed", 1_000, &["w-b"]),
            &limits_with_monologue_max(1),
        )
        .expect("append_message must succeed even though it halts the room");
    assert_eq!(appended.halt, Some(HaltReason::Monologue));

    assert_eq!(
        deliveries_sorted(&chat, appended.seq),
        vec![("w-b".to_string(), DeliveryOutcome::Pending)],
        "a halting post writes its destinations as pending like any other; the halted \
         outcome is written over them afterwards"
    );
    assert!(
        deliveries_sorted(&chat, appended.seq + 1).is_empty(),
        "the system notice addresses no window, so it must carry no delivery row"
    );
}

#[test]
fn set_delivery_replaces_the_pending_row_instead_of_adding_one() {
    let path = db_path("chat-pending-settles");
    let mut chat = ChatStore::open(&path).expect("ChatStore::open must succeed");

    let appended = chat
        .append_message(
            window_message_to("w-a", "Alpha", "settle this", 1_000, &["w-b"]),
            &generous_limits(),
        )
        .expect("append_message must succeed");

    chat.set_delivery(ROOM_ID, appended.seq, "w-b", DeliveryOutcome::Taken, 2_000)
        .expect("set_delivery must succeed over a pending row");

    assert_eq!(
        deliveries_sorted(&chat, appended.seq),
        vec![("w-b".to_string(), DeliveryOutcome::Taken)],
        "settling a delivery must update the row the post wrote, never add a second one"
    );
}

// ---------------------------------------------------------------------------------------
// The reason a destination's delivery was recorded as status-unknown. Recording and reading a delivery are one API: `set_delivery`
// itself takes the reason alongside the state, and `deliveries_for_seq` itself reads it back,
// decoding the stored wire string through `UnknownReason::from_wire_str` so a value this
// build does not recognize reads back as `None` without dropping the row.
// ---------------------------------------------------------------------------------------

#[test]
fn set_delivery_persists_and_reads_back_the_reason() {
    let path = db_path("chat-delivery-reason-persist");
    let mut chat = ChatStore::open(&path).expect("ChatStore::open must succeed");

    let appended = chat
        .append_message(
            window_message_to("w-a", "Alpha", "hi @b", 1_000, &["w-b"]),
            &generous_limits(),
        )
        .expect("append_message must succeed");

    chat.set_delivery(
        ROOM_ID,
        appended.seq,
        "w-b",
        DeliveryOutcome::UndeliveredUnknown(Some(UnknownReason::NoRuleMatched)),
        2_000,
    )
    .expect("set_delivery must succeed");

    assert_eq!(
        deliveries_sorted(&chat, appended.seq),
        vec![(
            "w-b".to_string(),
            DeliveryOutcome::UndeliveredUnknown(Some(UnknownReason::NoRuleMatched))
        )],
        "the reason carried by the outcome must round-trip through deliveries_for_seq"
    );
    assert_eq!(
        delivery_state(&path, appended.seq, "w-b").as_deref(),
        Some("undelivered_unknown"),
        "the stored state column must still hold the projected wire string"
    );
    assert_eq!(
        delivery_reason_column(&path, appended.seq, "w-b").as_deref(),
        Some(UnknownReason::NoRuleMatched.as_wire()),
        "the stored reason column must still hold the reason's own wire string"
    );
}

#[test]
fn a_delivery_recorded_without_a_reason_reads_back_as_none() {
    let path = db_path("chat-delivery-reason-none");
    let mut chat = ChatStore::open(&path).expect("ChatStore::open must succeed");

    let appended = chat
        .append_message(
            window_message_to("w-a", "Alpha", "hi @b", 1_000, &["w-b"]),
            &generous_limits(),
        )
        .expect("append_message must succeed");

    chat.set_delivery(ROOM_ID, appended.seq, "w-b", DeliveryOutcome::Taken, 2_000)
        .expect("set_delivery must succeed");

    assert_eq!(
        deliveries_sorted(&chat, appended.seq),
        vec![("w-b".to_string(), DeliveryOutcome::Taken)],
        "a delivery settled with an outcome that carries no reason must read back that way, \
         and every outcome other than the status-unknown one carries none"
    );
}

#[test]
fn a_reason_value_this_build_does_not_recognize_reads_back_as_none_but_the_row_survives() {
    let path = db_path("chat-delivery-reason-unknown-value");
    let mut chat = ChatStore::open(&path).expect("ChatStore::open must succeed");

    let appended = chat
        .append_message(
            window_message_to("w-a", "Alpha", "hi @b", 1_000, &["w-b"]),
            &generous_limits(),
        )
        .expect("append_message must succeed");

    {
        let conn = rusqlite::Connection::open(&path).expect("a raw write connection must open");
        conn.execute(
            "UPDATE chat_delivery SET state = 'undelivered_unknown', \
             unknown_reason = 'a_future_reason_this_build_does_not_know' \
             WHERE room_id = ?1 AND seq = ?2 AND window_id = ?3",
            rusqlite::params![ROOM_ID, appended.seq, "w-b"],
        )
        .expect("seeding an unrecognized reason value directly must succeed");
    }

    assert_eq!(
        deliveries_sorted(&chat, appended.seq),
        vec![("w-b".to_string(), DeliveryOutcome::UndeliveredUnknown(None))],
        "a reason string this build does not recognize must be dropped, not the whole row"
    );
}

#[test]
fn a_destination_that_was_never_observed_is_recorded_without_a_reason() {
    let path = db_path("chat-delivery-unknown-without-reason");
    let mut chat = ChatStore::open(&path).expect("ChatStore::open must succeed");

    let appended = chat
        .append_message(
            window_message_to("w-a", "Alpha", "hi @b", 1_000, &["w-b"]),
            &generous_limits(),
        )
        .expect("append_message must succeed");

    chat.set_delivery(
        ROOM_ID,
        appended.seq,
        "w-b",
        DeliveryOutcome::UndeliveredUnknown(None),
        2_000,
    )
    .expect("set_delivery must accept a status-unknown outcome that carries no reason");

    assert_eq!(
        deliveries_sorted(&chat, appended.seq),
        vec![("w-b".to_string(), DeliveryOutcome::UndeliveredUnknown(None))],
        "a destination nothing was observed about must read back as the reason-less form, \
         not as a guessed reason"
    );
    assert_eq!(
        delivery_state(&path, appended.seq, "w-b").as_deref(),
        Some("undelivered_unknown"),
        "the reason-less form must still be stored under the same state string"
    );
    assert_eq!(
        delivery_reason_column(&path, appended.seq, "w-b"),
        None,
        "no reason was observed, so the reason column must stay empty"
    );
}

#[test]
fn settling_a_delivery_again_clears_a_reason_the_new_outcome_cannot_carry() {
    let path = db_path("chat-delivery-reason-cleared-on-resettle");
    let mut chat = ChatStore::open(&path).expect("ChatStore::open must succeed");

    let appended = chat
        .append_message(
            window_message_to("w-a", "Alpha", "hi @b", 1_000, &["w-b"]),
            &generous_limits(),
        )
        .expect("append_message must succeed");

    chat.set_delivery(
        ROOM_ID,
        appended.seq,
        "w-b",
        DeliveryOutcome::UndeliveredUnknown(Some(UnknownReason::DetectorStopped)),
        2_000,
    )
    .expect("set_delivery must succeed");

    chat.set_delivery(ROOM_ID, appended.seq, "w-b", DeliveryOutcome::Taken, 2_100)
        .expect("re-settling the same destination must succeed");

    assert_eq!(
        deliveries_sorted(&chat, appended.seq),
        vec![("w-b".to_string(), DeliveryOutcome::Taken)],
        "the second outcome carries no reason, so the row must no longer carry one either"
    );
    assert_eq!(
        delivery_reason_column(&path, appended.seq, "w-b"),
        None,
        "the reason recorded by the first settle must be cleared, never left behind next to \
         a state that cannot carry it"
    );
}

#[test]
fn a_state_value_this_build_does_not_recognize_drops_the_row_from_both_reads_and_warns() {
    let path = db_path("chat-delivery-state-unknown-value");
    let mut chat = ChatStore::open(&path).expect("ChatStore::open must succeed");

    let appended = chat
        .append_message(
            window_message_to("w-a", "Alpha", "hi @b @c", 1_000, &["w-b", "w-c"]),
            &generous_limits(),
        )
        .expect("append_message must succeed");

    {
        let conn = rusqlite::Connection::open(&path).expect("a raw write connection must open");
        // The schema refuses this state, so only a hand-edited database holds one.
        conn.pragma_update(None, "ignore_check_constraints", true)
            .expect("the check bypass must apply");
        conn.execute(
            "UPDATE chat_delivery SET state = 'a_future_state_this_build_does_not_know' \
             WHERE room_id = ?1 AND seq = ?2 AND window_id = ?3",
            rusqlite::params![ROOM_ID, appended.seq, "w-b"],
        )
        .expect("seeding an unrecognized state value directly must succeed");
    }

    let (warns, rows) = count_warns(|| deliveries_sorted(&chat, appended.seq));
    assert_eq!(
        rows,
        vec![("w-c".to_string(), DeliveryOutcome::Pending)],
        "the delivery state is the record itself, not a value that can be dropped off a \
         surviving row, and it is never guessed at either: the unreadable row is skipped \
         while the other destination of the same message is still returned"
    );
    assert_eq!(
        warns, 1,
        "skipping a stored delivery must be announced once, never done in silence"
    );

    let (room_warns, room_rows) = count_warns(|| deliveries_in_room_sorted(&chat, appended.seq));
    assert_eq!(
        room_rows,
        vec![("w-c".to_string(), DeliveryOutcome::Pending)],
        "the whole-room read rebuilds rows through the same point, so it must skip the same \
         row and keep the same one"
    );
    assert_eq!(
        room_warns, 1,
        "the whole-room read must announce the skip as well"
    );

    assert_eq!(
        delivery_state(&path, appended.seq, "w-b").as_deref(),
        Some("a_future_state_this_build_does_not_know"),
        "skipping happens on the way out of the store; the read must never delete the row \
         a build that understands it could still use"
    );
}

/// Appends one message with two destinations and then writes a `state` value this build
/// does not recognize onto one of them: the stored situation both delivery reads have to
/// skip past, and the only one that makes either of them warn.
fn store_with_one_unreadable_delivery_row(tag: &str) -> (ChatStore, i64) {
    let path = db_path(tag);
    let mut chat = ChatStore::open(&path).expect("ChatStore::open must succeed");

    let appended = chat
        .append_message(
            window_message_to("w-a", "Alpha", "hi @b @c", 1_000, &["w-b", "w-c"]),
            &generous_limits(),
        )
        .expect("append_message must succeed");

    {
        let conn = rusqlite::Connection::open(&path).expect("a raw write connection must open");
        // The schema refuses this state, so only a hand-edited database holds one.
        conn.pragma_update(None, "ignore_check_constraints", true)
            .expect("the check bypass must apply");
        conn.execute(
            "UPDATE chat_delivery SET state = 'a_future_state_this_build_does_not_know' \
             WHERE room_id = ?1 AND seq = ?2 AND window_id = ?3",
            rusqlite::params![ROOM_ID, appended.seq, "w-b"],
        )
        .expect("seeding an unrecognized state value directly must succeed");
    }

    (chat, appended.seq)
}

/// The skip warning each read emits for the store `store_with_one_unreadable_delivery_row`
/// built, captured one read at a time so the two are never confused for each other.
fn skip_warnings_of_both_reads(
    chat: &ChatStore,
    seq: i64,
) -> (
    common::store_fixtures::WarnEvent,
    common::store_fixtures::WarnEvent,
) {
    let (mut per_seq, ()) = capture_warn_events(|| {
        chat.deliveries_for_seq(ROOM_ID, seq)
            .expect("deliveries_for_seq must succeed");
    });
    let (mut per_room, ()) = capture_warn_events(|| {
        chat.deliveries_by_room(ROOM_ID)
            .expect("deliveries_by_room must succeed");
    });
    assert_eq!(
        per_seq.len(),
        1,
        "the per-seq read must announce the skip exactly once"
    );
    assert_eq!(
        per_room.len(),
        1,
        "the whole-room read must announce the skip exactly once"
    );
    (per_seq.remove(0), per_room.remove(0))
}

#[test]
fn only_the_per_seq_delivery_read_names_the_seq_of_the_rows_it_skipped() {
    let (chat, seq) = store_with_one_unreadable_delivery_row("chat-delivery-skip-warning-fields");
    let (per_seq, per_room) = skip_warnings_of_both_reads(&chat, seq);

    let per_seq_keys: Vec<&str> = per_seq.fields.keys().map(String::as_str).collect();
    assert_eq!(
        per_seq_keys,
        vec!["room_id", "seq"],
        "a read of one message can say which (room, message) pair the skipped rows belonged \
         to, and that is the only place that information exists: losing the seq would leave \
         a reader unable to find the rows the warning is about"
    );
    assert_eq!(
        per_seq.fields.get("room_id"),
        Some(&ROOM_ID.to_string()),
        "the room the skip happened in must be the one that was read"
    );
    assert_eq!(
        per_seq.fields.get("seq"),
        Some(&seq.to_string()),
        "the seq the skip happened under must be the one that was read"
    );

    let per_room_keys: Vec<&str> = per_room.fields.keys().map(String::as_str).collect();
    assert_eq!(
        per_room_keys,
        vec!["room_id"],
        "a whole-room read can skip rows under several seqs at once, so it names none: \
         attaching one seq to a warning that covers many would state something untrue"
    );
    assert_eq!(
        per_room.fields.get("room_id"),
        Some(&ROOM_ID.to_string()),
        "the room the skip happened in must be the one that was read"
    );
}

#[test]
fn both_delivery_reads_word_the_skip_warning_the_same() {
    let (chat, seq) = store_with_one_unreadable_delivery_row("chat-delivery-skip-warning-wording");
    let (per_seq, per_room) = skip_warnings_of_both_reads(&chat, seq);

    assert!(
        !per_seq.message.is_empty(),
        "a skip warning with no message text would make the rest of this contract vacuous"
    );
    assert_eq!(
        per_seq.message, per_room.message,
        "both reads skip a row for the same reason, so they must say so in the same words"
    );
}

#[test]
fn an_unreadable_reason_keeps_its_row_while_an_unreadable_state_removes_it() {
    let path = db_path("chat-delivery-unreadable-reason-vs-state");
    let mut chat = ChatStore::open(&path).expect("ChatStore::open must succeed");

    let appended = chat
        .append_message(
            window_message_to("w-a", "Alpha", "hi @b @c @d", 1_000, &["w-b", "w-c", "w-d"]),
            &generous_limits(),
        )
        .expect("append_message must succeed");

    {
        let conn = rusqlite::Connection::open(&path).expect("a raw write connection must open");
        // The schema refuses the unknown state set below; only a hand-edited database holds one.
        conn.pragma_update(None, "ignore_check_constraints", true)
            .expect("the check bypass must apply");
        conn.execute(
            "UPDATE chat_delivery SET state = 'undelivered_unknown', \
             unknown_reason = 'a_future_reason_this_build_does_not_know' \
             WHERE room_id = ?1 AND seq = ?2 AND window_id = ?3",
            rusqlite::params![ROOM_ID, appended.seq, "w-b"],
        )
        .expect("seeding an unrecognized reason value directly must succeed");
        conn.execute(
            "UPDATE chat_delivery SET state = 'a_future_state_this_build_does_not_know' \
             WHERE room_id = ?1 AND seq = ?2 AND window_id = ?3",
            rusqlite::params![ROOM_ID, appended.seq, "w-c"],
        )
        .expect("seeding an unrecognized state value directly must succeed");
    }

    assert_eq!(
        deliveries_sorted(&chat, appended.seq),
        vec![
            ("w-b".to_string(), DeliveryOutcome::UndeliveredUnknown(None)),
            ("w-d".to_string(), DeliveryOutcome::Pending),
        ],
        "an unreadable reason is a value the row can do without, so the row stays with the \
         reason dropped; an unreadable state leaves nothing to record, so that row goes"
    );
}

// ---------------------------------------------------------------------------------------
// Schema v4 (`ensure_room` / `find_room` / `list_rooms`) and the v3 -> v4 history
// preservation contract (the migration keeps every existing chat row). Every helper
// below writes only through parameterized queries against fixed string literals, per the
// project's test-only SQL-concatenation allowance.
// ---------------------------------------------------------------------------------------

/// Builds a schema-v3-only database (migrations 001-003, never touching 004) at `path`,
/// then seeds two `main`-room chat_message rows and one chat_delivery row directly with
/// raw SQL mirroring `chat_store.rs`'s own insert shape (`mentions_json` as
/// `{"targets":[...],"user":bool}`), and raises `main`'s `needs_user` flag. Mimics a
/// database written by a build that predates the chat schema changes, before `ChatStore::open` (which
/// always migrates to the latest schema) ever touches it.
fn seed_v3_history_with_needs_user(path: &Path) {
    let mut conn = rusqlite::Connection::open(path).expect("a v3 seed connection must open");
    store::apply_migrations(&mut conn, &store::MIGRATIONS[..3])
        .expect("applying the first three migrations must reach schema v3");

    conn.execute(
        "INSERT INTO chat_message \
         (room_id, seq, at_ms, from_kind, from_window_id, from_name, body, mentions_json, \
          broadcast, silence_noted) \
         VALUES ('main', 1, 1000, 'window', 'w-a', 'Alpha', 'pre-migration one', \
                 '{\"targets\":[],\"user\":false}', 0, 0)",
        [],
    )
    .expect("seeding the first v3-era chat_message row must succeed");
    conn.execute(
        "INSERT INTO chat_message \
         (room_id, seq, at_ms, from_kind, from_window_id, from_name, body, mentions_json, \
          broadcast, silence_noted) \
         VALUES ('main', 2, 1100, 'window', 'w-a', 'Alpha', 'pre-migration two @beta', \
                 '{\"targets\":[\"w-b\"],\"user\":false}', 0, 0)",
        [],
    )
    .expect("seeding the second v3-era chat_message row must succeed");
    conn.execute(
        "INSERT INTO chat_delivery (room_id, seq, window_id, state, at_ms) \
         VALUES ('main', 2, 'w-b', 'pending', 1150)",
        [],
    )
    .expect("seeding the v3-era chat_delivery row must succeed");
    conn.execute(
        "UPDATE chat_room SET needs_user = 1 WHERE room_id = 'main'",
        [],
    )
    .expect("raising main's needs_user in the v3-era database must succeed");
}

// S-1 (requirement success condition 3): a v3 database with chat history, migrated to v4
// by nothing more than `ChatStore::open`, must gain the new workspace columns on `main`
// while its existing message and delivery rows read back byte-for-byte the same. There is
// no `display_name` column to backfill: the room's display name is a read-time value the
// disambiguation function derives from `(workspace_kind, workspace_key)`.
#[test]
fn migrating_a_v3_database_with_history_to_v4_preserves_main_and_its_history() {
    let path = db_path("chat-v3-to-v4-history");
    seed_v3_history_with_needs_user(&path);

    let chat = ChatStore::open(&path)
        .expect("ChatStore::open must migrate an existing v3-era database to the latest schema");

    let version: i64 = {
        let conn = open_ro(&path);
        conn.query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("user_version must be readable")
    };
    assert_eq!(
        version,
        store::MIGRATIONS.len() as i64,
        "opening a v3-era database must carry it all the way to the latest schema, not stop \
         partway"
    );

    let main_room = chat
        .find_room("main")
        .expect("find_room must succeed")
        .expect("the main room row seeded by 003 must still exist after migrating to v4");
    assert_eq!(
        main_room.kind, None,
        "main has no workspace kind before or after the migration"
    );
    assert_eq!(
        main_room.key, None,
        "main has no workspace key before or after the migration"
    );
    assert!(
        main_room.needs_user,
        "needs_user, set before the migration ran, must survive it unchanged"
    );

    let after = chat
        .read_after("main", 0, 10)
        .expect("read_after must succeed after migrating to v4");
    let bodies: Vec<(i64, String, Vec<String>)> = after
        .messages
        .iter()
        .map(|m| (m.seq, m.body.clone(), m.mentions.clone()))
        .collect();
    assert_eq!(
        bodies,
        vec![
            (1, "pre-migration one".to_string(), Vec::new()),
            (
                2,
                "pre-migration two @beta".to_string(),
                vec!["w-b".to_string()]
            ),
        ],
        "the two chat_message rows written before the migration must read back with the \
         exact same seq, body and mentions after it"
    );

    let deliveries = chat
        .deliveries_for_seq("main", 2)
        .expect("deliveries_for_seq must succeed after migrating to v4");
    assert_eq!(
        deliveries,
        vec![("w-b".to_string(), DeliveryOutcome::Pending)],
        "the chat_delivery row written before the migration must read back unchanged, with \
         no reason (the row predates the reason column)"
    );
}

/// A `WorkspaceKind` / key pair for `ensure_room`, distinct per call site.
fn git_key(path: &str) -> (WorkspaceKind, &str) {
    (WorkspaceKind::Git, path)
}

// ensure_room: no row for room_id yet -> Created, and the row it wrote is readable
// through find_room with every field it was given.
#[test]
fn ensure_room_creates_a_new_room_row_when_none_exists() {
    let path = db_path("chat-ensure-create");
    let mut chat = ChatStore::open(&path).expect("ChatStore::open must succeed");
    let (kind, key) = git_key("/repo/alpha");

    let outcome = chat
        .ensure_room("r-alpha0000000001", kind, key, 1_000)
        .expect("ensure_room must succeed for a brand-new room_id");
    assert_eq!(
        outcome,
        EnsureRoom::Created,
        "a room_id with no existing row must report Created"
    );

    let row = chat
        .find_room("r-alpha0000000001")
        .expect("find_room must succeed")
        .expect("the row ensure_room just created must be readable through find_room");
    assert_eq!(row.kind, Some(WorkspaceKind::Git));
    assert_eq!(row.key.as_deref(), Some("/repo/alpha"));
    assert_eq!(row.created_at_ms, 1_000);
    assert!(
        !row.needs_user,
        "a freshly created room must not start with needs_user set"
    );
    assert_eq!(
        row.halted, None,
        "a freshly created room must not start halted"
    );
    assert_eq!(
        row.last_at_ms, None,
        "a room with no posted messages yet must report no last_at_ms"
    );
}

// ensure_room: same room_id, same (kind, key) on a second call -> Existing, and the
// original created_at_ms is not overwritten by the second call's now_ms.
#[test]
fn ensure_room_is_idempotent_for_the_same_kind_and_key_and_does_not_overwrite_the_row() {
    let path = db_path("chat-ensure-existing");
    let mut chat = ChatStore::open(&path).expect("ChatStore::open must succeed");
    let (kind, key) = git_key("/repo/beta");

    chat.ensure_room("r-beta00000000001", kind, key, 1_000)
        .expect("the first ensure_room call must succeed");
    let outcome = chat
        .ensure_room("r-beta00000000001", kind, key, 9_999)
        .expect("a second ensure_room call with the same (kind, key) must succeed");
    assert_eq!(
        outcome,
        EnsureRoom::Existing,
        "the same room_id with a matching (kind, key) must report Existing"
    );

    let row = chat
        .find_room("r-beta00000000001")
        .expect("find_room must succeed")
        .expect("the row must still exist");
    assert_eq!(
        row.created_at_ms, 1_000,
        "the Existing branch must not overwrite the original created_at_ms with the second \
         call's now_ms"
    );
}

// ensure_room: same room_id, a different key on the second call -> Err(RoomKeyMismatch),
// covering both a changed key and a changed kind (either half of the (kind, key) pair
// diverging must be treated as a mismatch, not silently accepted).
#[test]
fn ensure_room_reports_room_key_mismatch_when_the_same_room_id_gets_a_different_key() {
    let path = db_path("chat-ensure-mismatch-key");
    let mut chat = ChatStore::open(&path).expect("ChatStore::open must succeed");

    chat.ensure_room(
        "r-gamma0000000001",
        WorkspaceKind::Git,
        "/repo/gamma",
        1_000,
    )
    .expect("the first ensure_room call must succeed");

    let err = chat
        .ensure_room(
            "r-gamma0000000001",
            WorkspaceKind::Git,
            "/repo/gamma-moved",
            2_000,
        )
        .expect_err("a second call with a different key for the same room_id must fail");
    assert!(
        matches!(err, store::StoreError::RoomKeyMismatch { .. }),
        "a changed key must be reported as RoomKeyMismatch, got: {err:?}"
    );

    let err_kind = chat
        .ensure_room(
            "r-gamma0000000001",
            WorkspaceKind::Directory,
            "/repo/gamma",
            3_000,
        )
        .expect_err("a second call with a different kind for the same room_id must fail");
    assert!(
        matches!(err_kind, store::StoreError::RoomKeyMismatch { .. }),
        "a changed kind (same key) must also be reported as RoomKeyMismatch, got: {err_kind:?}"
    );

    let row = chat
        .find_room("r-gamma0000000001")
        .expect("find_room must succeed")
        .expect("the original row must be untouched by the rejected calls");
    assert_eq!(
        row.key.as_deref(),
        Some("/repo/gamma"),
        "a rejected mismatch call must not overwrite the original row"
    );
}

// ensure_room: a different room_id sharing the same (kind, key) as an existing row hits
// the partial UNIQUE INDEX -- a distinct error path from RoomKeyMismatch.
#[test]
fn ensure_room_reports_a_unique_violation_when_a_different_room_id_shares_the_same_key() {
    let path = db_path("chat-ensure-unique-violation");
    let mut chat = ChatStore::open(&path).expect("ChatStore::open must succeed");
    let (kind, key) = git_key("/repo/delta");

    chat.ensure_room("r-delta0000000001", kind, key, 1_000)
        .expect("the first ensure_room call must succeed");

    let err = chat
        .ensure_room("r-delta0000000002", kind, key, 2_000)
        .expect_err("a different room_id for the same (kind, key) must fail");
    assert!(
        matches!(err, store::StoreError::Sqlite(_)),
        "a duplicate (kind, key) under a different room_id must surface as the partial \
         UNIQUE INDEX's own constraint failure, not a hand-rolled variant, got: {err:?}"
    );
    assert!(
        !matches!(err, store::StoreError::RoomKeyMismatch { .. }),
        "the UNIQUE-index collision must be distinguishable from the same-room_id \
         RoomKeyMismatch case, got: {err:?}"
    );

    assert!(
        chat.find_room("r-delta0000000002")
            .expect("find_room must succeed")
            .is_none(),
        "the rejected INSERT must not have left a row behind under the second room_id"
    );
}

// find_room: an unknown room_id reads back as None, not an error.
#[test]
fn find_room_returns_none_for_an_unknown_room_id() {
    let path = db_path("chat-find-room-unknown");
    let chat = ChatStore::open(&path).expect("ChatStore::open must succeed");

    assert_eq!(
        chat.find_room("r-does-not-exist-1")
            .expect("find_room must succeed for an unknown id"),
        None
    );
}

// find_room: the pre-seeded main row has no workspace kind or key, on a fresh v4 database
// with no history at all.
#[test]
fn find_room_for_main_on_a_fresh_database_has_no_kind_or_key() {
    let path = db_path("chat-find-room-main-fresh");
    let chat = ChatStore::open(&path).expect("ChatStore::open must succeed");

    let main_room = chat
        .find_room("main")
        .expect("find_room must succeed")
        .expect("the main room row seeded by migration 003 must exist on a fresh database");
    assert_eq!(main_room.room_id, "main");
    assert_eq!(main_room.kind, None);
    assert_eq!(main_room.key, None);
}

// list_rooms: main sorts first regardless of its own timestamps, the rest sort by
// last_at_ms descending, falling back to created_at_ms for a room with no messages yet;
// needs_user and halted are read per room, independent of one another; and every row
// created earlier in the test is still present in the final call (this module's public
// surface has no method that removes a chat_room row).
#[test]
fn list_rooms_sorts_main_first_then_by_last_at_ms_descending_falling_back_to_created_at_ms() {
    let path = db_path("chat-list-rooms-order");
    let mut chat = ChatStore::open(&path).expect("ChatStore::open must succeed");
    let limits = generous_limits();

    // room_c: created earliest (created_at_ms = 50) but posted to most recently
    // (at_ms = 9000) -- must sort ahead of room_a on last_at_ms alone.
    chat.ensure_room("r-roomc0000000001", WorkspaceKind::Git, "/repo/c", 50)
        .expect("ensure_room for room_c must succeed");
    chat.append_message(
        NewChatMessage {
            room_id: "r-roomc0000000001".to_string(),
            ..window_message("w-a", "Alpha", "in room c", 9_000)
        },
        &limits,
    )
    .expect("posting to room_c must succeed");

    // room_a: created after room_c (created_at_ms = 100), posted to less recently
    // (at_ms = 5000) than room_c.
    chat.ensure_room(
        "r-rooma0000000001",
        WorkspaceKind::Directory,
        "/repo/a",
        100,
    )
    .expect("ensure_room for room_a must succeed");
    chat.append_message(
        NewChatMessage {
            room_id: "r-rooma0000000001".to_string(),
            ..window_message("w-a", "Alpha", "in room a", 5_000)
        },
        &limits,
    )
    .expect("posting to room_a must succeed");

    // room_b: never posted to, so its sort key falls back to created_at_ms = 200 --
    // below both room_c's and room_a's last_at_ms, so it sorts last.
    chat.ensure_room("r-roomb0000000001", WorkspaceKind::Git, "/repo/b", 200)
        .expect("ensure_room for room_b must succeed");
    chat.set_needs_user("r-roomb0000000001", true)
        .expect("set_needs_user for room_b must succeed");
    chat.set_halt("r-roomc0000000001", HaltReason::Repetition, 9_500)
        .expect("set_halt for room_c must succeed");

    let rooms = chat.list_rooms().expect("list_rooms must succeed");
    let ids: Vec<String> = rooms.iter().map(|r| r.room_id.clone()).collect();
    assert_eq!(
        ids,
        vec![
            "main".to_string(),
            "r-roomc0000000001".to_string(),
            "r-rooma0000000001".to_string(),
            "r-roomb0000000001".to_string(),
        ],
        "main must sort first; the rest must sort by last_at_ms descending (room_c's 9000 \
         ahead of room_a's 5000), falling back to created_at_ms for room_b, which has no \
         messages"
    );

    let room_b = rooms
        .iter()
        .find(|r| r.room_id == "r-roomb0000000001")
        .expect("room_b must be present");
    assert!(
        room_b.needs_user,
        "needs_user must be read per room from list_rooms, independent of the other rooms"
    );
    assert_eq!(
        room_b.halted, None,
        "room_b was never halted and must not pick up room_c's halted state"
    );
    assert_eq!(
        room_b.last_at_ms, None,
        "room_b, which was never posted to, must report no last_at_ms"
    );

    let room_c = rooms
        .iter()
        .find(|r| r.room_id == "r-roomc0000000001")
        .expect("room_c must be present");
    assert_eq!(
        room_c.halted.map(|(reason, _)| reason),
        Some(HaltReason::Repetition),
        "room_c's own halted state must be visible in list_rooms"
    );
    assert!(
        room_c.needs_user,
        "set_halt raises needs_user for the room it halts, so room_c (halted above) must \
         have needs_user set"
    );

    let room_a = rooms
        .iter()
        .find(|r| r.room_id == "r-rooma0000000001")
        .expect("room_a must be present");
    assert!(
        !room_a.needs_user,
        "room_a had neither set_halt nor set_needs_user called on it, so it must not pick up \
         room_b's or room_c's needs_user flag (each room's flags are independent)"
    );
    assert_eq!(
        room_a.halted, None,
        "room_a was never halted and must not pick up room_c's halted state"
    );

    // Calling list_rooms again must not have caused any row to disappear: no delete path
    // exists anywhere in this module's contract (ensure_room / find_room / list_rooms are
    // its only room-table entry points), so every room created above must still be there.
    let rooms_again = chat
        .list_rooms()
        .expect("a second list_rooms call must succeed");
    let ids_again: Vec<String> = rooms_again.iter().map(|r| r.room_id.clone()).collect();
    assert_eq!(
        ids_again, ids,
        "a second list_rooms call must return the exact same set of rooms as the first"
    );
}

#[test]
fn a_directory_keyed_room_reads_back_with_its_directory_kind() {
    let path = db_path("chat-ensure-directory");
    let mut chat = ChatStore::open(&path).expect("ChatStore::open must succeed");

    chat.ensure_room(
        "r-dir0000000000001",
        WorkspaceKind::Directory,
        "/work/plain",
        1_000,
    )
    .expect("ensure_room must succeed for a directory-keyed room");

    let row = chat
        .find_room("r-dir0000000000001")
        .expect("find_room must succeed")
        .expect("the room just created must be readable");
    assert_eq!(row.kind, Some(WorkspaceKind::Directory));
    assert_eq!(row.key.as_deref(), Some("/work/plain"));
}

#[test]
fn a_user_post_resets_the_turn_budget_for_the_posts_after_it() {
    let path = db_path("chat-user-resets-turn");
    let mut chat = ChatStore::open(&path).expect("ChatStore::open must succeed");
    let limits = ChatLimits {
        turn_message_max: 2,
        ..generous_limits()
    };
    let user_post = NewChatMessage {
        from_kind: "user".to_string(),
        from_window_id: None,
        from_name: "user".to_string(),
        ..window_message("unused", "unused", "keep going", 2_000)
    };

    chat.append_message(window_message("w-a", "Alpha", "first", 1_000), &limits)
        .expect("append_message must succeed");
    chat.append_message(user_post, &limits)
        .expect("append_message must succeed");
    let after_user = chat
        .append_message(window_message("w-a", "Alpha", "second", 3_000), &limits)
        .expect("append_message must succeed");

    assert_eq!(
        after_user.halt, None,
        "only one AI post has landed since the user spoke, below turn_message_max = 2"
    );
}

#[test]
fn consecutive_external_posts_trip_the_monologue_halt() {
    let path = db_path("chat-external-monologue");
    let mut chat = ChatStore::open(&path).expect("ChatStore::open must succeed");
    let limits = limits_with_monologue_max(2);
    let external_post = |body: &str, at_ms: i64| NewChatMessage {
        from_kind: "external".to_string(),
        from_window_id: None,
        from_name: "external".to_string(),
        ..window_message("unused", "unused", body, at_ms)
    };

    let first = chat
        .append_message(external_post("first from outside", 1_000), &limits)
        .expect("append_message must succeed");
    assert_eq!(first.halt, None);
    let second = chat
        .append_message(external_post("second from outside", 2_000), &limits)
        .expect("append_message must succeed");

    assert_eq!(
        second.halt,
        Some(HaltReason::Monologue),
        "an external caller is a speaker like any window, so its run counts toward monologue_max"
    );
}

#[test]
fn the_broadcast_flag_reads_back_as_it_was_posted() {
    let path = db_path("chat-broadcast-flag");
    let mut chat = ChatStore::open(&path).expect("ChatStore::open must succeed");

    let broadcast = chat
        .append_message(
            NewChatMessage {
                broadcast: true,
                ..window_message("w-a", "Alpha", "to everyone", 1_000)
            },
            &generous_limits(),
        )
        .expect("append_message must succeed");
    let addressed = chat
        .append_message(
            window_message("w-a", "Alpha", "to nobody in particular", 2_000),
            &generous_limits(),
        )
        .expect("append_message must succeed");

    let tail = chat.read_tail(ROOM_ID, 10).expect("read_tail must succeed");
    let flag_of = |seq: i64| {
        tail.iter()
            .find(|m| m.seq == seq)
            .map(|m| m.broadcast)
            .expect("the posted row must be in the tail")
    };
    assert!(flag_of(broadcast.seq));
    assert!(!flag_of(addressed.seq));
}
