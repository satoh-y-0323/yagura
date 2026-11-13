#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: the window table in `yagura.db` and the window's token.
//!
//! A window's row is there before its holder starts its child, is `live` once the
//! window opened, carries the window's values as they change, and goes with the
//! window: a window that cannot open and a closed one leave no row. The token
//! lives in the holder's launch record only, never in a file.
//!
//! The table is read without naming its columns: the row of a window is the row
//! holding its id and one of the three states.

use crate::common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use rusqlite::types::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use yagura_os::{IpcTransport, PlatformIpc};
use yagura_protocol::holder::{
    HOLDER_PROTOCOL, HolderFrame, HolderFrameBuffer, encode_holder_frame, read_holder_record,
};
use yagura_protocol::{ClientMessage, DaemonMessage, WINDOW_ID_ENV};
use yagura_test_support::HANG_GUARD;
use yagura_test_support::process::echo_child_path;

use common::{Harness, policy, read_response, send};
use yagura_daemon::DaemonConfig;
use yagura_daemon::fault_point::{FaultPoint, FaultPoints};
use yagura_daemon::store;
use yagura_daemon::window::{
    HooksRoute, LaunchIdentity, OpenWindowRequest, RequestedCwd, TaskOrigin, WindowOpenError,
    WindowProvenance, WindowRegistry,
};

const STATES: [&str; 3] = ["opening", "live", "lost"];

const ROW_PROBE_CHILD: &str = "row_probe_child";
const PROBE_DB_ENV: &str = "WINDOW_TABLE_PROBE_DB";
const PROBE_OUT_ENV: &str = "WINDOW_TABLE_PROBE_OUT";
const ABSENT: &str = "absent";

const LAYOUT: &str = r#"{"window_table":"layout"}"#;
const SESSION_ID: &str = "window-table-session-1";

type Row = Vec<(String, Value)>;

/// Every row, in any table but the snapshot's, that holds one of [`STATES`].
// Reads a test-owned database read-only, naming tables from its own schema so the
// test does not fix the table's column names.
#[allow(clippy::disallowed_methods)]
fn state_rows(db: &Path) -> Vec<Row> {
    let Ok(conn) = store::open_readonly(db) else {
        return Vec::new();
    };
    let tables: Vec<String> = conn
        .prepare(
            "SELECT name FROM sqlite_master WHERE type = 'table' \
             AND name NOT LIKE 'sqlite_%' AND name NOT LIKE 'snapshot_%'",
        )
        .expect("list the tables")
        .query_map([], |row| row.get(0))
        .expect("list the tables")
        .collect::<Result<_, _>>()
        .expect("list the tables");
    let mut rows = Vec::new();
    for table in tables {
        let mut statement = conn
            .prepare(&format!("SELECT * FROM \"{table}\""))
            .expect("read a table");
        let names: Vec<String> = statement.column_names().map(str::to_owned).collect();
        let mut query = statement.query([]).expect("read a table");
        while let Some(row) = query.next().expect("read a row") {
            let cells: Row = names
                .iter()
                .enumerate()
                .map(|(index, name)| (name.clone(), row.get::<_, Value>(index).unwrap()))
                .collect();
            if state_of(&cells).is_some() {
                rows.push(cells);
            }
        }
    }
    rows
}

fn texts(row: &Row) -> impl Iterator<Item = &str> {
    row.iter().filter_map(|(_, value)| match value {
        Value::Text(text) => Some(text.as_str()),
        _ => None,
    })
}

fn has_text(row: &Row, wanted: &str) -> bool {
    texts(row).any(|text| text == wanted)
}

fn state_of(row: &Row) -> Option<&str> {
    texts(row).find(|text| STATES.contains(text))
}

/// The one row of `window_id`, if any.
fn row_of(rows: &[Row], window_id: &str) -> Option<Row> {
    let found: Vec<&Row> = rows.iter().filter(|row| has_text(row, window_id)).collect();
    assert!(
        found.len() <= 1,
        "a window has one row at most, {window_id} has {}: {found:?}",
        found.len()
    );
    found.first().map(|row| (*row).clone())
}

/// Waits until the row of `window_id` satisfies `holds`; returns it.
async fn wait_for_row(db: &Path, window_id: &str, what: &str, holds: impl Fn(&Row) -> bool) -> Row {
    let deadline = tokio::time::Instant::now() + HANG_GUARD;
    loop {
        let row = row_of(&state_rows(db), window_id);
        if let Some(row) = row.as_ref().filter(|row| holds(row)) {
            return row.clone();
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the row of {window_id} never came to {what}; last seen: {row:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Waits until the rows that hold a state belong to exactly `window_ids`.
async fn wait_for_rows_of(db: &Path, window_ids: &[&str], what: &str) {
    let deadline = tokio::time::Instant::now() + HANG_GUARD;
    loop {
        let rows = state_rows(db);
        let matches = rows.len() == window_ids.len()
            && window_ids.iter().all(|id| row_of(&rows, id).is_some());
        if matches {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "{what}: the table must hold the rows of {window_ids:?} only, got {rows:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn holds_argv(row: &Row, argv: &[String]) -> bool {
    texts(row).any(|text| serde_json::from_str::<Vec<String>>(text).is_ok_and(|v| v == argv))
}

fn echo_command() -> Vec<String> {
    vec![echo_child_path().to_str().unwrap().to_owned()]
}

fn request(command: Vec<String>, name: &str) -> OpenWindowRequest {
    OpenWindowRequest {
        command,
        cwd: None,
        name: Some(name.to_owned()),
        vendor: None,
        detection: None,
        on_name_taken: Default::default(),
    }
}

/// The window's child: reports the state of its window's row as it starts.
#[test]
#[ignore = "runs only as the child of a window a test opens"]
fn row_probe_child() {
    if !common::running_as_own_test(module_path!(), ROW_PROBE_CHILD) {
        return;
    }
    let db = PathBuf::from(std::env::var_os(PROBE_DB_ENV).expect("the probe's database"));
    let out = PathBuf::from(std::env::var_os(PROBE_OUT_ENV).expect("the probe's output"));
    let window_id = std::env::var(WINDOW_ID_ENV).expect("the window's id");
    let seen = row_of(&state_rows(&db), &window_id)
        .and_then(|row| state_of(&row).map(str::to_owned))
        .unwrap_or_else(|| ABSENT.to_owned());
    let staged = out.with_extension("staged");
    std::fs::write(&staged, seen).expect("write what the probe saw");
    std::fs::rename(&staged, &out).expect("publish what the probe saw");
    loop {
        std::thread::park();
    }
}

async fn wait_for_file(path: &Path) -> String {
    let deadline = tokio::time::Instant::now() + HANG_GUARD;
    loop {
        if let Ok(text) = std::fs::read_to_string(path) {
            return text;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the window's child never reported what it saw at {}",
            path.display()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn a_windows_row_comes_before_its_child_follows_its_values_and_goes_with_it() {
    let root = common::unique_temp_dir("window-table-row");
    let db = root.join("yagura.db");
    let config = DaemonConfig {
        approval_policy: Some(policy(&["close_window"])),
        ..DaemonConfig::default()
    };
    let h = Harness::start("window-table-row", config).await;
    store::wire_into(&h.registry, Some(db.clone()));

    let alpha_cwd = root.join("alpha");
    std::fs::create_dir_all(&alpha_cwd).unwrap();
    let probe_out = root.join("row-at-child-start.txt");
    let alpha_argv = common::own_test_child_command(module_path!(), ROW_PROBE_CHILD);
    let alpha = h
        .registry
        .open_with_extra_env(
            OpenWindowRequest {
                command: alpha_argv.clone(),
                cwd: Some(RequestedCwd::from_caller(alpha_cwd)),
                name: Some("wt-alpha".to_owned()),
                vendor: Some("wt-vendor".to_owned()),
                detection: Some("wt-detect".to_owned()),
                on_name_taken: Default::default(),
            },
            vec![
                (PROBE_DB_ENV.to_owned(), db.to_str().unwrap().to_owned()),
                (
                    PROBE_OUT_ENV.to_owned(),
                    probe_out.to_str().unwrap().to_owned(),
                ),
            ],
            Vec::new(),
            WindowProvenance {
                opener: Some("wopener01".to_owned()),
                task_origin: Some(TaskOrigin {
                    plan_id: "wt-plan-1".to_owned(),
                    task_id: "wt-task-1".to_owned(),
                }),
                launch: Some(LaunchIdentity {
                    profile: "wt-profile".to_owned(),
                    model: Some("wt-model".to_owned()),
                    effort: Some("wt-effort".to_owned()),
                }),
                ..WindowProvenance::default()
            },
        )
        .await
        .expect("the probing window must open");
    let alpha_id = alpha.window_id.clone();

    let seen = wait_for_file(&probe_out).await;
    assert!(
        seen == "opening" || seen == "live",
        "the window's row must be in the table before its holder starts the child, \
         the child saw {seen:?}"
    );

    let row = wait_for_row(&db, &alpha_id, "live", |row| state_of(row) == Some("live")).await;
    let entry = h.registry.get(&alpha_id).expect("the window is registered");
    for (value, what) in [
        ("wt-alpha", "the name"),
        (entry.cwd(), "the cwd"),
        ("wt-vendor", "the vendor"),
        ("wt-detect", "the detection"),
        ("wt-profile", "the profile"),
        ("wt-model", "the model"),
        ("wt-effort", "the effort"),
        ("wopener01", "the opener"),
        ("env", "the credentials' kind"),
    ] {
        assert!(
            has_text(&row, value),
            "the row must carry {what} {value:?}: {row:?}"
        );
    }
    assert!(
        holds_argv(&row, &alpha_argv),
        "the row must carry the argv: {row:?}"
    );
    for part in ["wt-plan-1", "wt-task-1"] {
        assert!(
            texts(&row).any(|text| text.contains(part)),
            "the row must carry the task origin's {part:?}: {row:?}"
        );
    }

    // The rename is answered only once its row has the name: while the row
    // writer stands before the write, the rename is still waiting.
    let paused = FaultPoints::arm(FaultPoint::WindowRowUpdatePaused);
    h.registry.arm_window_table(&paused);
    let mut rename = std::pin::pin!(h.registry.rename_window(&entry, "wt-alpha-renamed"));
    let first_poll =
        std::future::poll_fn(|cx| std::task::Poll::Ready(rename.as_mut().poll(cx))).await;
    let answered_early = first_poll.is_ready();
    paused.release();
    assert!(
        !answered_early,
        "a rename must not be answered before its row has the name"
    );
    let renamed = match first_poll {
        std::task::Poll::Ready(renamed) => renamed,
        std::task::Poll::Pending => tokio::time::timeout(HANG_GUARD, rename)
            .await
            .expect("the rename must be answered once the row writer goes on"),
    };
    renamed.expect("the rename must succeed");
    let row = row_of(&state_rows(&db), &alpha_id).expect("the window's row");
    assert!(
        has_text(&row, "wt-alpha-renamed"),
        "the row must have the new name once the rename is answered: {row:?}"
    );
    h.registry.set_window_layout(&entry, LAYOUT.to_owned());
    assert!(entry.record_session_id(SESSION_ID));
    wait_for_row(
        &db,
        &alpha_id,
        "the renamed name, layout and session id",
        |row| {
            has_text(row, "wt-alpha-renamed")
                && !has_text(row, "wt-alpha")
                && has_text(row, LAYOUT)
                && has_text(row, SESSION_ID)
        },
    )
    .await;

    let beta = h
        .registry
        .open(request(echo_command(), "wt-beta"))
        .await
        .expect("the second window must open");
    let beta_id = beta.window_id.clone();
    let beta_row = wait_for_row(&db, &beta_id, "live", |row| state_of(row) == Some("live")).await;
    let alpha_row = row_of(&state_rows(&db), &alpha_id).expect("the first window's row");
    let ordered = alpha_row.iter().any(|(name, first)| {
        beta_row.iter().any(|(other, second)| {
            other == name
                && matches!((first, second), (Value::Integer(a), Value::Integer(b)) if b > a)
        })
    });
    assert!(
        ordered,
        "the rows must carry the order the windows opened in: {alpha_row:?} then {beta_row:?}"
    );

    let unreachable = common::guaranteed_nonexistent_exe_path();
    let refused = h
        .registry
        .open(request(
            vec![unreachable.to_str().unwrap().to_owned()],
            "wt-no-child",
        ))
        .await;
    assert!(
        refused.is_err(),
        "a child that cannot be found must not open"
    );
    let holderless = Arc::new(WindowRegistry::new(DaemonConfig {
        holder_exe: Some(unreachable),
        ..DaemonConfig::default()
    }));
    store::wire_into(&holderless, Some(db.clone()));
    let refused = holderless
        .open(request(echo_command(), "wt-no-holder"))
        .await;
    assert!(
        refused.is_err(),
        "a window whose holder cannot start must not open"
    );
    // A row that takes no values refuses the open before any holder starts.
    let unwritable = Harness::start(
        "window-table-row-unwritable",
        DaemonConfig {
            faults: FaultPoints::arm(FaultPoint::WindowRowUpdateFails),
            ..DaemonConfig::default()
        },
    )
    .await;
    store::wire_into(&unwritable.registry, Some(db.clone()));
    let refused = unwritable
        .registry
        .open(request(echo_command(), "wt-row-unwritable"))
        .await;
    assert!(
        matches!(refused, Err(WindowOpenError::SpawnFailed(_))),
        "a window whose row cannot take its values must be refused as spawn_failed, got {:?}",
        refused.as_ref().map(|opened| &opened.window_id)
    );
    wait_for_rows_of(
        &db,
        &[&alpha_id, &beta_id],
        "after two windows failed to open",
    )
    .await;

    let mut client = h.connect().await;
    send(
        &mut client,
        &ClientMessage::CloseWindow {
            window_id: alpha_id.clone(),
            force: None,
        },
    )
    .await;
    match read_response(&mut client).await {
        DaemonMessage::WindowClosed { window_id } => assert_eq!(window_id, alpha_id),
        other => panic!("expected window_closed, got {other:?}"),
    }
    wait_for_rows_of(&db, &[&beta_id], "after the first window closed").await;

    // A shutdown waits for a window still opening, which it then ends with its
    // row, and refuses an open asked for once it began.
    let paused = FaultPoints::arm(FaultPoint::WindowRegistrationPaused);
    let stopping = Harness::start(
        "window-table-row-stopping",
        DaemonConfig {
            faults: paused.clone(),
            ..DaemonConfig::default()
        },
    )
    .await;
    let stopping_db = root.join("stopping.db");
    store::wire_into(&stopping.registry, Some(stopping_db.clone()));
    let registry = stopping.registry.clone();
    let opening =
        tokio::spawn(async move { registry.open(request(echo_command(), "wt-opening")).await });
    tokio::time::timeout(HANG_GUARD, paused.reached())
        .await
        .expect("the open must come to its registration");
    let mut shutdown = std::pin::pin!(stopping.registry.terminate_all());
    let first_poll =
        std::future::poll_fn(|cx| std::task::Poll::Ready(shutdown.as_mut().poll(cx))).await;
    let waited_for_the_opening = first_poll.is_pending();
    let registry = stopping.registry.clone();
    let later =
        tokio::spawn(async move { registry.open(request(echo_command(), "wt-later")).await });
    paused.release();
    if first_poll.is_pending() {
        tokio::time::timeout(HANG_GUARD, shutdown)
            .await
            .expect("the shutdown must end once the opening window goes on");
    }
    let later = tokio::time::timeout(HANG_GUARD, later)
        .await
        .expect("the later open must be answered")
        .expect("the later open's task");
    let later_seen = match &later {
        Ok(opened) => format!("opened {}", opened.window_id),
        Err(err) => format!("{err:?}"),
    };
    let opening = tokio::time::timeout(HANG_GUARD, opening)
        .await
        .expect("the paused open must be answered")
        .expect("the paused open's task");
    let opening_seen = opening.as_ref().map(|opened| opened.window_id.clone());
    let row_left = row_of(&state_rows(&stopping_db), "wt-opening");
    stopping.registry.terminate_all().await;
    assert!(
        waited_for_the_opening,
        "a shutdown must not end while a window is still opening"
    );
    assert!(
        later_seen.starts_with("ShuttingDown"),
        "an open asked for once the shutdown began must be refused as shutting down, got \
         {later_seen}"
    );
    assert!(
        row_left.is_none(),
        "the window that was opening when the shutdown began must be ended with its row by \
         the time the shutdown ends (its open answered {opening_seen:?}): {row_left:?}"
    );

    unwritable.registry.terminate_all().await;
    holderless.terminate_all().await;
    h.registry.terminate_all().await;
}

/// The spelling of each hook route in the table and the launch record: a
/// contract with the rows and records earlier daemons and holders wrote.
fn persisted_spelling(route: HooksRoute) -> &'static str {
    match route {
        HooksRoute::Unarranged => "unarranged",
        HooksRoute::Injection => "injection",
        HooksRoute::RegisteredEntries => "registered_entries",
        HooksRoute::LeftOut => "left_out",
    }
}

#[test]
fn every_hook_route_keeps_its_persisted_spelling_both_ways() {
    for route in [
        HooksRoute::Unarranged,
        HooksRoute::Injection,
        HooksRoute::RegisteredEntries,
        HooksRoute::LeftOut,
    ] {
        let spelling = persisted_spelling(route);
        assert_eq!(
            serde_json::to_value(route).unwrap(),
            serde_json::Value::String(spelling.to_owned()),
            "{route:?} must be written as {spelling:?}"
        );
        let read: HooksRoute =
            serde_json::from_value(serde_json::Value::String(spelling.to_owned()))
                .unwrap_or_else(|err| panic!("{spelling:?} must read back ({err})"));
        assert_eq!(read, route, "{spelling:?} must read back as {route:?}");
    }
}

/// The holders folder of this process's registry that keeps `window_id`'s record.
fn holders_dir_of(window_id: &str) -> PathBuf {
    let prefix = format!("yagura-holders-{}-", std::process::id());
    std::fs::read_dir(std::env::temp_dir())
        .expect("read the temp directory")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|dir| {
            dir.file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with(&prefix))
                && dir.join(format!("{window_id}.json")).is_file()
        })
        .unwrap_or_else(|| panic!("no holders folder keeps the record of {window_id}"))
}

/// Says `Hello` to the holder at `endpoint` as a next daemon would; returns the
/// launch record its `Welcome` carries.
async fn welcome_record(endpoint: &str) -> String {
    let mut connection = PlatformIpc::default()
        .connect(endpoint)
        .await
        .expect("connect to the window's holder");
    let hello = encode_holder_frame(&HolderFrame::Hello {
        protocol: HOLDER_PROTOCOL,
        daemon_boot: "window-table-next-daemon".to_owned(),
    });
    connection.write_all(&hello).await.expect("say hello");
    let mut frames = HolderFrameBuffer::new();
    let mut chunk = [0u8; 4096];
    tokio::time::timeout(HANG_GUARD, async {
        loop {
            if let Some(frame) = frames.next_frame().expect("a frame from the holder") {
                match frame {
                    HolderFrame::Welcome { record, .. } => return record,
                    other => panic!("the holder must answer hello with welcome, got {other:?}"),
                }
            }
            let read = connection
                .read(&mut chunk)
                .await
                .expect("read from the holder");
            assert!(
                read > 0,
                "the holder closed the connection before its welcome"
            );
            frames.push(&chunk[..read]);
        }
    })
    .await
    .expect("the holder's welcome within HANG_GUARD")
}

fn string_leaves(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::String(text) => out.push(text.clone()),
        serde_json::Value::Array(items) => items.iter().for_each(|item| string_leaves(item, out)),
        serde_json::Value::Object(map) => map.values().for_each(|item| string_leaves(item, out)),
        _ => {}
    }
}

fn files_under(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        match entry.file_type() {
            Ok(kind) if kind.is_dir() => files_under(&path, out),
            Ok(kind) if kind.is_file() => out.push(path),
            _ => {}
        }
    }
}

#[tokio::test]
async fn a_windows_token_is_in_its_holders_launch_record_and_in_no_file() {
    let root = common::unique_temp_dir("window-table-token");
    let db = root.join("yagura.db");
    let h = Harness::start("window-table-token", DaemonConfig::default()).await;
    store::wire_into(&h.registry, Some(db.clone()));

    let opened = h
        .registry
        .open(request(echo_command(), "wt-token"))
        .await
        .expect("the window must open");
    let window_id = opened.window_id.clone();
    let entry = h
        .registry
        .get(&window_id)
        .expect("the window is registered");
    h.registry
        .rename_window(&entry, "wt-token-renamed")
        .await
        .expect("the rename must succeed");
    h.registry.set_window_layout(&entry, LAYOUT.to_owned());
    assert!(entry.record_session_id(SESSION_ID));
    wait_for_row(&db, &window_id, "every value written", |row| {
        has_text(row, "wt-token-renamed") && has_text(row, LAYOUT) && has_text(row, SESSION_ID)
    })
    .await;

    let holders_dir = holders_dir_of(&window_id);
    let record = read_holder_record(&holders_dir, &window_id).expect("the holder's record");
    // Read before the Hello below displaces the daemon: a holder that awaits no successor
    // ends, removing its record, once that connection closes.
    #[cfg(target_os = "linux")]
    let record_file =
        yagura_protocol::holder::holder_record_path(&holders_dir, &window_id).unwrap();
    #[cfg(target_os = "linux")]
    let record_bytes = std::fs::read(record_file).unwrap();
    let launch_record = welcome_record(&record.endpoint).await;
    let mut leaves = Vec::new();
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&launch_record) {
        string_leaves(&value, &mut leaves);
    }
    let tokens: Vec<String> = leaves
        .into_iter()
        .filter(|leaf| entry.verify_status_token(leaf))
        .collect();
    assert!(
        !tokens.is_empty(),
        "the holder's launch record must carry the window's token, after its name, layout and \
         session id changed"
    );

    #[cfg(target_os = "linux")]
    {
        let is_scope_name = |name: &str| {
            name.strip_suffix(".scope").is_some_and(|stem| {
                !stem.is_empty()
                    && stem
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
            })
        };
        let row = row_of(&state_rows(&db), &window_id).expect("the window's row");
        let in_row: Vec<&str> = texts(&row).filter(|text| is_scope_name(text)).collect();
        let mut in_record = Vec::new();
        if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&record_bytes) {
            string_leaves(&value, &mut in_record);
        }
        assert!(
            in_row
                .iter()
                .any(|name| in_record.iter().any(|recorded| recorded == name)),
            "the window's row must carry the scope name its holder's record carries: row \
             {in_row:?}, record {in_record:?}"
        );
    }

    let mut files = Vec::new();
    files_under(&root, &mut files);
    files_under(&holders_dir, &mut files);
    assert!(
        files.iter().any(|file| file.starts_with(&root)),
        "the database must have been written: {files:?}"
    );
    for file in &files {
        let Ok(bytes) = std::fs::read(file) else {
            continue;
        };
        for token in &tokens {
            assert!(
                !bytes
                    .windows(token.len())
                    .any(|window| window == token.as_bytes()),
                "the window's token must never reach a file, it is in {}",
                file.display()
            );
        }
    }

    h.registry.terminate_all().await;
}
