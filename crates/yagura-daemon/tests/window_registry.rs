#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: `WindowRegistry` / `WindowEntry` contract -- public API
//! shape, dual resource limits, detection name validation, per-window detection
//! engine and Exited teardown.
//!
//! Children are echo_child or a per-OS shell (`cmd.exe` / `/bin/sh`), never a
//! real vendor CLI.

mod common;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::time::sleep;
use yagura_test_support::paths::{
    strip_extended_length_prefix, unique_detection_dir, unique_temp_dir, write_definition,
};
use yagura_test_support::process::echo_child_path;

use yagura_daemon::DaemonConfig;
use yagura_daemon::window::{
    OpenWindowRequest, RequestedCwd, WindowEntry, WindowOpenError, WindowRegistry,
};
use yagura_protocol::{RoomRef, WindowInfo, WindowStatus};

/// The minimal `OpenWindowRequest` most tests need: spawn echo_child with all
/// optional fields omitted.
fn echo_request() -> OpenWindowRequest {
    let child = echo_child_path();
    OpenWindowRequest {
        command: vec![
            child
                .to_str()
                .expect("echo_child path must be valid UTF-8")
                .to_string(),
        ],
        cwd: None,
        name: None,
        vendor: None,
        detection: None,
    }
}

/// [`echo_request`], opened against `cwd` instead of the daemon's own
/// (the room decoration under test is keyed by the window's resolved
/// working directory).
fn echo_request_in(cwd: PathBuf) -> OpenWindowRequest {
    OpenWindowRequest {
        cwd: Some(RequestedCwd::from_caller(cwd)),
        ..echo_request()
    }
}

/// A fresh temp directory holding a bare `.git` marker directory (never the
/// real `git` binary -- `resolve_key`'s walk only needs a directory named
/// `.git` to exist): a window opened against this `cwd` resolves to a
/// `WorkspaceKind::Git` key.
fn git_workspace_dir(tag: &str) -> PathBuf {
    let dir = unique_temp_dir("yagura-test-registry-room", tag);
    std::fs::create_dir(dir.join(".git")).expect("create a bare .git marker directory");
    dir
}

/// A `.git`-marked workspace directory nested as `{parent}/{leaf}` under a
/// fresh temp root: lets a test control the immediate parent name
/// `disambiguate` prepends when two workspaces share a leaf name.
fn git_workspace_dir_named(tag: &str, parent: &str, leaf: &str) -> PathBuf {
    let root = unique_temp_dir("yagura-test-registry-room", tag);
    let dir = root.join(parent).join(leaf);
    std::fs::create_dir_all(&dir).expect("create nested workspace dir");
    std::fs::create_dir(dir.join(".git")).expect("create a bare .git marker directory");
    dir
}

const KNOWN_DETECTION_DEFINITION: &str = r#"
schema_version = 1
name = "known-def"
rules = []

[debounce]
confirm_ms = 30
"#;

/// Polls until the window reports `Exited`; panics on timeout. Deterministic
/// polling instead of a fixed sleep keeps this robust on slow machines.
async fn wait_for_exited(entry: &WindowEntry) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if entry.current_status() == WindowStatus::Exited {
            return;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("timed out waiting for the window to become Exited");
        }
        sleep(Duration::from_millis(50)).await;
    }
}

/// Asserts the `w{sequence}-{nonce}` shape without pinning the nonce: tests
/// must consume ids returned by `open` / `list`, never a literal.
fn assert_window_id_shape(id: &str) {
    let rest = id
        .strip_prefix('w')
        .unwrap_or_else(|| panic!("window_id must start with 'w', got {id}"));
    let (sequence, nonce) = rest
        .split_once('-')
        .unwrap_or_else(|| panic!("window_id must contain a nonce suffix, got {id}"));
    assert!(
        !sequence.is_empty() && sequence.chars().all(|c| c.is_ascii_digit()),
        "window_id must carry a numeric sequence before the nonce, got {id}"
    );
    assert!(
        !nonce.is_empty() && nonce.chars().all(|c| c.is_ascii_alphanumeric()),
        "window_id must carry an alphanumeric nonce suffix, got {id}"
    );
}

#[tokio::test]
async fn registry_open_assigns_unique_window_ids_sequentially() {
    let registry = WindowRegistry::new(DaemonConfig::default());
    let id1 = registry
        .open(echo_request())
        .await
        .expect("first open should succeed")
        .window_id;
    let id2 = registry
        .open(echo_request())
        .await
        .expect("second open should succeed")
        .window_id;

    assert_ne!(id1, id2, "sequential opens must assign distinct window_ids");
    assert_window_id_shape(&id1);
    assert_window_id_shape(&id2);

    registry.terminate_all().await;
}

#[tokio::test]
async fn registry_window_ids_never_collide_across_registry_instances() {
    let first = WindowRegistry::new(DaemonConfig::default());
    let second = WindowRegistry::new(DaemonConfig::default());

    let id_from_first = first
        .open(echo_request())
        .await
        .expect("open on the first registry should succeed")
        .window_id;
    let id_from_second = second
        .open(echo_request())
        .await
        .expect("open on the second registry should succeed")
        .window_id;

    assert_ne!(
        id_from_first, id_from_second,
        "two registries must not mint the same window_id at the same sequence \
         position (a restarted daemon's ids must never match the old ones)"
    );
    assert!(
        first.get(&id_from_second).is_none(),
        "a window_id minted by another registry must not resolve here"
    );
    assert!(
        second.get(&id_from_first).is_none(),
        "a window_id minted by another registry must not resolve here"
    );

    first.terminate_all().await;
    second.terminate_all().await;
}

#[tokio::test]
async fn registry_concurrent_opens_never_produce_duplicate_ids() {
    let registry = Arc::new(WindowRegistry::new(DaemonConfig::default()));
    let mut handles = Vec::new();
    for _ in 0..8 {
        let registry = registry.clone();
        handles.push(tokio::spawn(async move {
            registry
                .open(echo_request())
                .await
                .expect("concurrent open should succeed")
                .window_id
        }));
    }

    let mut ids = Vec::new();
    for handle in handles {
        ids.push(handle.await.expect("open task must not panic"));
    }

    let mut unique = ids.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(
        unique.len(),
        ids.len(),
        "concurrent opens must never assign duplicate window_ids, got {ids:?}"
    );

    registry.terminate_all().await;
}

#[tokio::test]
async fn registry_list_returns_insertion_order() {
    let registry = WindowRegistry::new(DaemonConfig::default());
    let id1 = registry
        .open(echo_request())
        .await
        .expect("first open")
        .window_id;
    let id2 = registry
        .open(echo_request())
        .await
        .expect("second open")
        .window_id;
    let id3 = registry
        .open(echo_request())
        .await
        .expect("third open")
        .window_id;

    let list = registry.list();
    let ids: Vec<String> = list.iter().map(|w| w.window_id.clone()).collect();
    assert_eq!(
        ids,
        vec![id1, id2, id3],
        "list() must preserve insertion (registration) order"
    );

    registry.terminate_all().await;
}

#[tokio::test]
async fn registry_get_returns_window_entry_or_none() {
    let registry = WindowRegistry::new(DaemonConfig::default());
    let id = registry
        .open(echo_request())
        .await
        .expect("open should succeed")
        .window_id;

    let entry = registry.get(&id);
    assert!(
        entry.is_some(),
        "get() must return Some for a known window_id"
    );

    let missing = registry.get("w-does-not-exist");
    assert!(
        missing.is_none(),
        "get() must return None for an unknown window_id"
    );

    registry.terminate_all().await;
}

#[tokio::test]
async fn registry_get_returns_none_for_a_nonce_less_legacy_id() {
    let registry = WindowRegistry::new(DaemonConfig::default());
    let id = registry
        .open(echo_request())
        .await
        .expect("open should succeed")
        .window_id;

    assert!(
        registry.get("w1").is_none(),
        "a pre-nonce identifier must not resolve to the window occupying the \
         same sequence position, got a hit while {id} is open"
    );

    registry.terminate_all().await;
}

#[tokio::test]
async fn registry_open_supplies_default_name_from_command_file_stem() {
    let registry = WindowRegistry::new(DaemonConfig::default());
    let id = registry
        .open(echo_request())
        .await
        .expect("open should succeed")
        .window_id;

    let list = registry.list();
    let info = list
        .iter()
        .find(|w| w.window_id == id)
        .expect("window must be present in list");
    assert_eq!(
        info.name, "echo_child",
        "name must default to command[0]'s file_stem"
    );

    registry.terminate_all().await;
}

#[tokio::test]
async fn registry_cwd_is_reflected_in_child_process_working_directory() {
    let registry = WindowRegistry::new(DaemonConfig::default());
    let target_dir =
        unique_detection_dir("yagura-test-registry", "child-pwd", "create temp cwd dir");
    let expected = strip_extended_length_prefix(
        &target_dir
            .canonicalize()
            .expect("target dir must canonicalize"),
    )
    .to_string_lossy()
    .to_string();

    #[cfg(windows)]
    let command = vec!["cmd.exe".to_string(), "/c".to_string(), "cd".to_string()];
    #[cfg(unix)]
    let command = vec!["/bin/sh".to_string(), "-c".to_string(), "pwd".to_string()];

    let id = registry
        .open(OpenWindowRequest {
            command,
            cwd: Some(RequestedCwd::from_caller(target_dir)),
            name: None,
            vendor: None,
            detection: None,
        })
        .await
        .expect("open with a valid, existing cwd should succeed")
        .window_id;

    let entry = registry
        .get(&id)
        .expect("get should return the opened window");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let (text, _) = entry
            .session()
            .text_tail(usize::MAX, usize::MAX)
            .await
            .unwrap();
        if text.contains(&expected) {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!(
                "timed out waiting for the child's `cd` output to contain {expected:?};                  the window shows: {text:?}"
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    registry.terminate_all().await;
}

#[tokio::test]
async fn registry_window_info_cwd_has_no_extended_length_prefix() {
    let registry = WindowRegistry::new(DaemonConfig::default());
    let target_dir =
        unique_detection_dir("yagura-test-registry", "info-cwd", "create temp cwd dir");

    let id = registry
        .open(OpenWindowRequest {
            command: vec![
                echo_child_path()
                    .to_str()
                    .expect("echo_child path must be valid UTF-8")
                    .to_string(),
            ],
            cwd: Some(RequestedCwd::from_caller(target_dir)),
            name: None,
            vendor: None,
            detection: None,
        })
        .await
        .expect("open with a valid, existing cwd should succeed")
        .window_id;

    let list = registry.list();
    let info = list
        .iter()
        .find(|w| w.window_id == id)
        .expect("window must be present in list");
    assert!(
        !info.cwd.starts_with(r"\\?\"),
        "WindowInfo.cwd must not contain the \\?\\ prefix, got {}",
        info.cwd
    );

    registry.terminate_all().await;
}

#[tokio::test]
async fn registry_cwd_omitted_defaults_to_daemon_current_directory() {
    let registry = WindowRegistry::new(DaemonConfig::default());
    let id = registry
        .open(echo_request())
        .await
        .expect("open without cwd should succeed")
        .window_id;

    let list = registry.list();
    let info = list
        .iter()
        .find(|w| w.window_id == id)
        .expect("window must be present in list");
    let daemon_cwd = std::env::current_dir()
        .expect("daemon current_dir must resolve")
        .canonicalize()
        .expect("daemon current_dir must canonicalize");
    let expected = strip_extended_length_prefix(&daemon_cwd)
        .to_string_lossy()
        .to_string();
    assert_eq!(
        info.cwd, expected,
        "cwd must default to the daemon's resolved current directory when omitted"
    );

    registry.terminate_all().await;
}

#[tokio::test]
async fn registry_open_with_nonexistent_cwd_returns_invalid_argument_without_panicking() {
    let registry = WindowRegistry::new(DaemonConfig::default());
    let missing_cwd = common::guaranteed_nonexistent_exe_path();

    let result = registry
        .open(OpenWindowRequest {
            command: vec![
                echo_child_path()
                    .to_str()
                    .expect("echo_child path must be valid UTF-8")
                    .to_string(),
            ],
            cwd: Some(RequestedCwd::from_caller(missing_cwd)),
            name: None,
            vendor: None,
            detection: None,
        })
        .await;

    match result {
        Err(WindowOpenError::InvalidArgument { .. }) => {}
        other => panic!("expected InvalidArgument for a nonexistent cwd, got {other:?}"),
    }
}

#[tokio::test]
async fn registry_open_returns_spawn_failed_on_missing_command_without_panicking() {
    let registry = WindowRegistry::new(DaemonConfig::default());
    let missing_exe = common::guaranteed_nonexistent_exe_path()
        .to_str()
        .expect("nonexistent test path must be valid UTF-8")
        .to_string();

    let result = registry
        .open(OpenWindowRequest {
            command: vec![missing_exe],
            cwd: None,
            name: None,
            vendor: None,
            detection: None,
        })
        .await;

    match result {
        Err(WindowOpenError::SpawnFailed(_)) => {}
        other => panic!("expected SpawnFailed for a nonexistent executable, got {other:?}"),
    }
}

#[tokio::test]
async fn registry_detection_name_rejects_path_traversal() {
    let registry = WindowRegistry::new(DaemonConfig::default());
    let result = registry
        .open(OpenWindowRequest {
            command: vec![
                echo_child_path()
                    .to_str()
                    .expect("echo_child path must be valid UTF-8")
                    .to_string(),
            ],
            cwd: None,
            name: None,
            vendor: None,
            detection: Some("../escape".to_string()),
        })
        .await;

    match result {
        Err(WindowOpenError::InvalidArgument { .. }) => {}
        other => {
            panic!("expected InvalidArgument for a path-traversal detection name, got {other:?}")
        }
    }
}

#[tokio::test]
async fn registry_detection_name_rejects_path_separator() {
    let registry = WindowRegistry::new(DaemonConfig::default());
    let result = registry
        .open(OpenWindowRequest {
            command: vec![
                echo_child_path()
                    .to_str()
                    .expect("echo_child path must be valid UTF-8")
                    .to_string(),
            ],
            cwd: None,
            name: None,
            vendor: None,
            detection: Some("foo/bar".to_string()),
        })
        .await;

    match result {
        Err(WindowOpenError::InvalidArgument { .. }) => {}
        other => {
            panic!("expected InvalidArgument for a detection name containing '/', got {other:?}")
        }
    }
}

#[tokio::test]
async fn registry_detection_name_rejects_backslash() {
    let registry = WindowRegistry::new(DaemonConfig::default());
    let result = registry
        .open(OpenWindowRequest {
            command: vec![
                echo_child_path()
                    .to_str()
                    .expect("echo_child path must be valid UTF-8")
                    .to_string(),
            ],
            cwd: None,
            name: None,
            vendor: None,
            detection: Some(r"foo\bar".to_string()),
        })
        .await;

    match result {
        Err(WindowOpenError::InvalidArgument { .. }) => {}
        other => {
            panic!("expected InvalidArgument for a detection name containing '\\', got {other:?}")
        }
    }
}

#[tokio::test]
async fn registry_detection_name_with_valid_syntax_is_accepted() {
    let registry = WindowRegistry::new(DaemonConfig::default());
    let result = registry
        .open(OpenWindowRequest {
            command: vec![
                echo_child_path()
                    .to_str()
                    .expect("echo_child path must be valid UTF-8")
                    .to_string(),
            ],
            cwd: None,
            name: None,
            vendor: None,
            detection: Some("my-detection_1".to_string()),
        })
        .await;

    assert!(
        result.is_ok(),
        "a syntactically valid detection name must be accepted, got {result:?}"
    );

    registry.terminate_all().await;
}

#[tokio::test]
async fn registry_unknown_detection_definition_fails_safe_and_other_windows_unaffected() {
    let dir = unique_detection_dir(
        "yagura-test-registry-detection",
        "unknown-def",
        "create temp detection dir",
    );
    write_definition(&dir, "known-def", KNOWN_DETECTION_DEFINITION);
    let config = DaemonConfig {
        detection_dir: Some(dir),
        ..DaemonConfig::default()
    };
    let registry = WindowRegistry::new(config);

    let id1 = registry
        .open(OpenWindowRequest {
            command: vec![
                echo_child_path()
                    .to_str()
                    .expect("echo_child path must be valid UTF-8")
                    .to_string(),
            ],
            cwd: None,
            name: None,
            vendor: None,
            detection: Some("nonexistent-detection".to_string()),
        })
        .await
        .expect("open with a missing detection definition must not error or panic")
        .window_id;

    let entry1 = registry
        .get(&id1)
        .expect("window must be registered despite the missing detection definition");
    assert_eq!(
        entry1.current_status(),
        WindowStatus::Unknown,
        "a missing detection definition must fail safe to Unknown"
    );

    let id2 = registry
        .open(OpenWindowRequest {
            command: vec![
                echo_child_path()
                    .to_str()
                    .expect("echo_child path must be valid UTF-8")
                    .to_string(),
            ],
            cwd: None,
            name: None,
            vendor: None,
            detection: Some("known-def".to_string()),
        })
        .await
        .expect("second open with a valid detection definition should succeed")
        .window_id;
    let entry2 = registry.get(&id2).expect("second window must exist");
    assert!(
        entry2.has_detector(),
        "a window with a valid detection definition must have a detector attached, unaffected \
         by the first window's failure"
    );

    registry.terminate_all().await;
}

#[tokio::test]
async fn registry_living_window_limit_rejects_open_until_a_window_exits() {
    let config = DaemonConfig {
        max_windows: 1,
        ..DaemonConfig::default()
    };
    let registry = WindowRegistry::new(config);

    let id1 = registry
        .open(echo_request())
        .await
        .expect("first open within the living limit should succeed")
        .window_id;
    let entry1 = registry.get(&id1).expect("get id1");

    let err = registry.open(echo_request()).await;
    assert!(
        matches!(err, Err(WindowOpenError::ResourceExhausted)),
        "open beyond the living limit must return ResourceExhausted, got {err:?}"
    );

    registry.terminate_all().await;
    wait_for_exited(&entry1).await;

    let id2 = registry
        .open(echo_request())
        .await
        .expect("open after the living window exited should succeed")
        .window_id;
    assert_ne!(id1, id2);

    registry.terminate_all().await;
}

#[tokio::test]
async fn registry_total_entry_count_evicts_oldest_unattached_exited_window() {
    let config = DaemonConfig {
        max_windows: 1, // total cap = max_windows * 2 = 2
        ..DaemonConfig::default()
    };
    let registry = WindowRegistry::new(config);

    let id1 = registry
        .open(echo_request())
        .await
        .expect("first open")
        .window_id;
    let entry1 = registry.get(&id1).expect("get id1");
    registry.terminate_all().await;
    wait_for_exited(&entry1).await;

    let id2 = registry
        .open(echo_request())
        .await
        .expect("second open after id1 exited (living limit freed)")
        .window_id;
    let entry2 = registry.get(&id2).expect("get id2");
    registry.terminate_all().await;
    wait_for_exited(&entry2).await;

    // Total entries now 2 (== max_windows * 2); a third open must evict the
    // oldest unattached Exited entry (id1) rather than fail.
    let id3 = registry
        .open(echo_request())
        .await
        .expect("third open should evict id1, not fail")
        .window_id;

    let list = registry.list();
    assert!(
        !list.iter().any(|w| w.window_id == id1),
        "the oldest unattached Exited window must be evicted from list(), got {list:?}"
    );
    assert!(
        registry.get(&id1).is_none(),
        "the evicted window must no longer be retrievable via get()"
    );
    assert!(
        list.iter().any(|w| w.window_id == id2),
        "id2 must remain, got {list:?}"
    );
    assert!(
        list.iter().any(|w| w.window_id == id3),
        "id3 must be present, got {list:?}"
    );

    registry.terminate_all().await;
}

#[tokio::test]
async fn registry_total_entry_count_rejects_open_when_all_exited_entries_are_attached() {
    let config = DaemonConfig {
        max_windows: 1, // total cap = 2
        ..DaemonConfig::default()
    };
    let registry = WindowRegistry::new(config);

    let id1 = registry
        .open(echo_request())
        .await
        .expect("first open")
        .window_id;
    let entry1 = registry.get(&id1).expect("get id1");
    registry.terminate_all().await;
    wait_for_exited(&entry1).await;
    let (_gen1, _kick1) = entry1.register_attach().await;

    let id2 = registry
        .open(echo_request())
        .await
        .expect("second open after id1 exited (living limit freed)")
        .window_id;
    let entry2 = registry.get(&id2).expect("get id2");
    registry.terminate_all().await;
    wait_for_exited(&entry2).await;
    let (_gen2, _kick2) = entry2.register_attach().await;

    // Both Exited entries are attached, so no eviction candidate exists: the
    // registry must fail closed rather than evict an attached entry.
    let err = registry.open(echo_request()).await;
    assert!(
        matches!(err, Err(WindowOpenError::ResourceExhausted)),
        "open must fail-closed with ResourceExhausted when every Exited entry is attached, \
         got {err:?}"
    );
}

#[tokio::test]
async fn registry_evicts_an_exited_window_once_its_attach_holder_let_go() {
    let config = DaemonConfig {
        max_windows: 1, // total cap = 2
        ..DaemonConfig::default()
    };
    let registry = WindowRegistry::new(config);

    let id1 = registry
        .open(echo_request())
        .await
        .expect("first open")
        .window_id;
    let entry1 = registry.get(&id1).expect("get id1");
    registry.terminate_all().await;
    wait_for_exited(&entry1).await;
    let (gen1, _kick1) = entry1.register_attach().await;
    entry1.clear_attach_if(gen1).await;

    let id2 = registry
        .open(echo_request())
        .await
        .expect("second open after id1 exited (living limit freed)")
        .window_id;
    let entry2 = registry.get(&id2).expect("get id2");
    registry.terminate_all().await;
    wait_for_exited(&entry2).await;
    // Still held, so id1 is the only eviction candidate.
    let (_gen2, _kick2) = entry2.register_attach().await;

    let opened = registry.open(echo_request()).await;
    assert!(
        opened.is_ok(),
        "the holder of id1 let go, so id1 must be evicted to make room, got {opened:?}"
    );
    let list = registry.list();
    assert!(
        !list.iter().any(|w| w.window_id == id1),
        "the released Exited window must be the one evicted, got {list:?}"
    );
    assert!(
        list.iter().any(|w| w.window_id == id2),
        "the still-attached Exited window must be kept, got {list:?}"
    );

    registry.terminate_all().await;
}

#[tokio::test]
async fn registry_living_window_limit_rejects_exactly_one_of_two_concurrent_opens() {
    let config = DaemonConfig {
        max_windows: 1,
        ..DaemonConfig::default()
    };
    let registry = WindowRegistry::new(config);

    let (r1, r2) = tokio::join!(registry.open(echo_request()), registry.open(echo_request()));
    let results = [r1, r2];

    let ok_count = results.iter().filter(|r| r.is_ok()).count();
    assert_eq!(
        ok_count, 1,
        "exactly one of two opens raced against a max_windows: 1 registry must succeed, got \
         {results:?}"
    );
    let exhausted_count = results
        .iter()
        .filter(|r| matches!(r, Err(WindowOpenError::ResourceExhausted)))
        .count();
    assert_eq!(
        exhausted_count, 1,
        "the other of the two raced opens must be rejected with ResourceExhausted, got \
         {results:?}"
    );

    let living = registry
        .list()
        .into_iter()
        .filter(|w| w.status != WindowStatus::Exited)
        .count();
    assert!(
        living <= 1,
        "the registry must never hold more than max_windows (1) living windows even when \
         opens race, got {living} living"
    );

    registry.terminate_all().await;
}

#[tokio::test]
async fn registry_total_entry_count_evicts_exactly_one_candidate_across_two_concurrent_opens_that_exceed_the_cap()
 {
    let config = DaemonConfig {
        max_windows: 2, // total cap = max_windows * 2 = 4
        ..DaemonConfig::default()
    };
    let registry = WindowRegistry::new(config);

    // Three unattached Exited entries: with the total cap at 4, landing two
    // more windows on top of these three must evict exactly one of them.
    let mut evictable_ids = Vec::new();
    for _ in 0..3 {
        let id = registry
            .open(echo_request())
            .await
            .expect("setup open")
            .window_id;
        let entry = registry.get(&id).expect("get setup window");
        registry.terminate_all().await;
        wait_for_exited(&entry).await;
        evictable_ids.push(id);
    }

    let (r1, r2) = tokio::join!(registry.open(echo_request()), registry.open(echo_request()));
    let opened_ids: Vec<String> = [r1, r2]
        .into_iter()
        .map(|r| {
            r.expect(
                "both concurrent opens must succeed: the living limit (2) is not exceeded, and \
                 the total cap only requires evicting one of the three eviction candidates",
            )
            .window_id
        })
        .collect();

    let list = registry.list();
    assert!(
        list.len() <= 4,
        "total entries must never exceed the total cap (max_windows * 2 = 4) even when opens \
         race past a full set of eviction candidates, got {list:?}"
    );
    let evicted_count = evictable_ids
        .iter()
        .filter(|id| !list.iter().any(|w| &w.window_id == *id))
        .count();
    assert_eq!(
        evicted_count, 1,
        "landing two more windows against a total cap of 4 with three pre-existing unattached \
         Exited candidates must evict exactly one of them, got {list:?}"
    );
    for id in &opened_ids {
        assert!(
            list.iter().any(|w| &w.window_id == id),
            "both concurrently opened windows must be present in the final list, got {list:?}"
        );
    }

    registry.terminate_all().await;
}

#[tokio::test]
async fn registry_attach_slot_replacement_kicks_previous_holder() {
    let registry = WindowRegistry::new(DaemonConfig::default());
    let id = registry
        .open(echo_request())
        .await
        .expect("open should succeed")
        .window_id;
    let entry = registry.get(&id).expect("get should return entry");

    let (_gen1, mut rx1) = entry.register_attach().await;
    let (_gen2, _rx2) = entry.register_attach().await;

    tokio::select! {
        _ = &mut rx1 => {}
        _ = sleep(Duration::from_millis(500)) => {
            panic!("the first attach generation must be kicked when a second register_attach occurs");
        }
    }

    registry.terminate_all().await;
}

#[tokio::test]
async fn registry_exited_window_status_is_fixed_and_detector_task_converges_to_stopped() {
    let registry = WindowRegistry::new(DaemonConfig::default());
    let id = registry
        .open(echo_request())
        .await
        .expect("open should succeed")
        .window_id;
    let entry = registry.get(&id).expect("get should return entry");
    assert!(
        entry.detector_task_alive(),
        "a live window's detector handle must still be held"
    );

    registry.terminate_all().await;
    wait_for_exited(&entry).await;

    assert_eq!(
        entry.current_status(),
        WindowStatus::Exited,
        "status must be fixed to Exited after the child exits"
    );

    let list = registry.list();
    assert!(
        list.iter().any(|w| w.window_id == id),
        "list() must keep succeeding and reporting an Exited window"
    );
    assert!(
        registry.get(&id).is_some(),
        "get() must keep succeeding for an Exited window"
    );

    assert!(
        !entry.has_detector(),
        "an Exited window must no longer expose a detector"
    );

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if !entry.detector_task_alive() {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("detector_task_alive() did not converge to false within the timeout");
        }
        sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn registry_record_status_event_returns_false_after_exit() {
    let registry = WindowRegistry::new(DaemonConfig::default());
    let id = registry
        .open(echo_request())
        .await
        .expect("open should succeed")
        .window_id;
    let entry = registry.get(&id).expect("get should return entry");

    assert!(
        entry.record_status_event(WindowStatus::Running),
        "a living window must accept record_status_event"
    );

    registry.terminate_all().await;
    wait_for_exited(&entry).await;

    assert!(
        !entry.record_status_event(WindowStatus::Unknown),
        "an Exited window must reject record_status_event"
    );
}

#[tokio::test]
async fn registry_notify_resize_started_returns_true_while_alive_and_false_after_exit() {
    let registry = WindowRegistry::new(DaemonConfig::default());
    let id = registry
        .open(echo_request())
        .await
        .expect("open should succeed")
        .window_id;
    let entry = registry.get(&id).expect("get should return entry");

    assert!(
        entry.notify_resize_started(),
        "a living window must accept notify_resize_started"
    );

    registry.terminate_all().await;
    wait_for_exited(&entry).await;

    assert!(
        !entry.notify_resize_started(),
        "an Exited window must reject notify_resize_started without panicking"
    );
}

#[tokio::test]
async fn registry_terminate_all_exits_all_windows_with_observable_exit_codes() {
    let registry = WindowRegistry::new(DaemonConfig::default());
    let id1 = registry
        .open(echo_request())
        .await
        .expect("first open")
        .window_id;
    let id2 = registry
        .open(echo_request())
        .await
        .expect("second open")
        .window_id;
    let entry1 = registry.get(&id1).expect("get id1");
    let entry2 = registry.get(&id2).expect("get id2");

    registry.terminate_all().await;
    wait_for_exited(&entry1).await;
    wait_for_exited(&entry2).await;

    let list = registry.list();
    for info in &list {
        assert_eq!(
            info.status,
            WindowStatus::Exited,
            "every window must be Exited after terminate_all(), got {list:?}"
        );
        assert!(
            info.exit_code.is_some(),
            "an Exited window's exit_code must be observable, got {list:?}"
        );
    }
}

// =====================================================================
// list() room decoration: every non-Exited window's `room` is
// derived from its resolved workspace.
// =====================================================================

/// Looks up `window_id` in a snapshot returned by [`WindowRegistry::list`]
/// and returns its room, panicking (naming `window_id`) if the window is
/// absent or carries no room. Callers that need to assert the absence of a
/// room instead read `.room` directly and must not use this helper.
fn room_of(list: &[WindowInfo], window_id: &str) -> RoomRef {
    list.iter()
        .find(|w| w.window_id == window_id)
        .unwrap_or_else(|| panic!("window {window_id} must be listed"))
        .room
        .clone()
        .unwrap_or_else(|| panic!("window {window_id} must have a room"))
}

#[tokio::test]
async fn registry_list_gives_two_distinct_git_workspaces_distinct_room_ids() {
    let registry = WindowRegistry::new(DaemonConfig::default());
    let dir_a = git_workspace_dir("distinct-a");
    let dir_b = git_workspace_dir("distinct-b");
    let id_a = registry
        .open(echo_request_in(dir_a))
        .await
        .expect("open a")
        .window_id;
    let id_b = registry
        .open(echo_request_in(dir_b))
        .await
        .expect("open b")
        .window_id;

    let list = registry.list();
    let room_a = room_of(&list, &id_a);
    let room_b = room_of(&list, &id_b);
    assert_ne!(
        room_a.room_id, room_b.room_id,
        "two distinct git workspaces must resolve to distinct room ids"
    );

    registry.terminate_all().await;
}

#[tokio::test]
async fn registry_list_gives_a_workspace_and_its_subdirectory_the_same_room_id() {
    let registry = WindowRegistry::new(DaemonConfig::default());
    let dir_top = git_workspace_dir("same-room-parent");
    let dir_sub = dir_top.join("child");
    std::fs::create_dir_all(&dir_sub).expect("create subdirectory of the workspace");

    let id_top = registry
        .open(echo_request_in(dir_top))
        .await
        .expect("open at the workspace root")
        .window_id;
    let id_sub = registry
        .open(echo_request_in(dir_sub))
        .await
        .expect("open in the subdirectory")
        .window_id;

    let list = registry.list();
    let room_top = room_of(&list, &id_top);
    let room_sub = room_of(&list, &id_sub);
    assert_eq!(
        room_top.room_id, room_sub.room_id,
        "a window opened in a subdirectory of a git workspace must resolve to the same room as \
         the workspace root"
    );

    registry.terminate_all().await;
}

#[tokio::test]
async fn registry_list_gives_a_room_to_a_window_with_no_git_marker() {
    let registry = WindowRegistry::new(DaemonConfig::default());
    let dir = unique_temp_dir("yagura-test-registry-room", "no-git");
    let id = registry
        .open(echo_request_in(dir))
        .await
        .expect("open without a .git marker")
        .window_id;

    let list = registry.list();
    let info = list
        .iter()
        .find(|w| w.window_id == id)
        .expect("window must be listed");
    assert!(
        info.room.is_some(),
        "a window opened outside any git workspace must still get a room (cwd-keyed), got \
         {info:?}"
    );

    registry.terminate_all().await;
}

#[tokio::test]
async fn registry_list_names_a_colliding_leaf_with_its_parent_segment() {
    let registry = WindowRegistry::new(DaemonConfig::default());
    let dir_x = git_workspace_dir_named("collide-x", "x", "proj");
    let dir_y = git_workspace_dir_named("collide-y", "y", "proj");

    let id_x = registry
        .open(echo_request_in(dir_x))
        .await
        .expect("open x/proj")
        .window_id;
    let id_y = registry
        .open(echo_request_in(dir_y))
        .await
        .expect("open y/proj")
        .window_id;

    let list = registry.list();
    let name_x = room_of(&list, &id_x).name;
    let name_y = room_of(&list, &id_y).name;

    let sep = std::path::MAIN_SEPARATOR;
    assert_eq!(
        name_x,
        format!("x{sep}proj"),
        "a colliding leaf name must grow to include its parent segment"
    );
    assert_eq!(
        name_y,
        format!("y{sep}proj"),
        "a colliding leaf name must grow to include its parent segment"
    );

    registry.terminate_all().await;
}

#[tokio::test]
// Reads the registry's participant list directly to check a window's membership.
#[allow(clippy::disallowed_methods)]
async fn registry_list_room_id_matches_workspace_room_id_for_and_is_a_valid_token() {
    let registry = WindowRegistry::new(DaemonConfig::default());
    let dir = git_workspace_dir("id-matches");
    let id = registry
        .open(echo_request_in(dir))
        .await
        .expect("open")
        .window_id;

    let participation = registry
        .list_participation()
        .into_iter()
        .find(|p| p.window_id == id)
        .expect("window must be present in list_participation");
    let expected_room_id = yagura_daemon::workspace::room_id_for(&participation.workspace);

    let list = registry.list();
    let room = room_of(&list, &id);

    assert_eq!(
        room.room_id, expected_room_id,
        "list()'s room_id must match workspace::room_id_for the same window's workspace"
    );
    assert!(
        yagura_protocol::is_valid_token(&room.room_id),
        "room_id must satisfy the shared [a-z0-9_-]{{1,64}} token rule, got {}",
        room.room_id
    );

    registry.terminate_all().await;
}

#[tokio::test]
async fn registry_list_keeps_matching_room_ids_for_two_living_windows_around_an_exited_one() {
    let registry = WindowRegistry::new(DaemonConfig::default());
    let dir_top = git_workspace_dir("sandwich-parent");
    let dir_sub = dir_top.join("child");
    std::fs::create_dir_all(&dir_sub).expect("create subdirectory of the workspace");
    let dir_middle = unique_temp_dir("yagura-test-registry-room", "sandwich-middle");

    let id_first = registry
        .open(echo_request_in(dir_top))
        .await
        .expect("open the first living window")
        .window_id;

    #[cfg(windows)]
    let short_lived_command = vec!["cmd.exe".to_string(), "/c".to_string(), "cd".to_string()];
    #[cfg(unix)]
    let short_lived_command = vec!["/bin/sh".to_string(), "-c".to_string(), "pwd".to_string()];
    let id_middle = registry
        .open(OpenWindowRequest {
            command: short_lived_command,
            cwd: Some(RequestedCwd::from_caller(dir_middle)),
            name: None,
            vendor: None,
            detection: None,
        })
        .await
        .expect("open the window that exits on its own")
        .window_id;
    let entry_middle = registry.get(&id_middle).expect("get the middle window");

    let id_last = registry
        .open(echo_request_in(dir_sub))
        .await
        .expect("open the second living window, in a subdirectory of the first")
        .window_id;

    wait_for_exited(&entry_middle).await;

    let list = registry.list();
    let room_first = room_of(&list, &id_first);
    let room_middle = list
        .iter()
        .find(|w| w.window_id == id_middle)
        .expect("the exited window must still be listed")
        .room
        .clone();
    let room_last = room_of(&list, &id_last);

    assert_eq!(
        room_middle, None,
        "an exited window sandwiched between two living windows must still report no room"
    );
    assert_eq!(
        room_first.room_id, room_last.room_id,
        "the two living windows sharing a workspace must keep matching room ids even with an \
         exited window between them"
    );
    assert_eq!(
        room_first.name, room_last.name,
        "the two living windows sharing a workspace must keep matching room names even with an \
         exited window between them"
    );

    registry.terminate_all().await;
}

#[tokio::test]
async fn registry_list_gives_no_room_to_an_exited_window() {
    let registry = WindowRegistry::new(DaemonConfig::default());
    let id = registry
        .open(echo_request())
        .await
        .expect("open should succeed")
        .window_id;
    let entry = registry.get(&id).expect("get should return entry");

    registry.terminate_all().await;
    wait_for_exited(&entry).await;

    let list = registry.list();
    let info = list
        .iter()
        .find(|w| w.window_id == id)
        .expect("an Exited window must still be listed");
    assert_eq!(
        info.room, None,
        "an Exited window must have left its room, got {info:?}"
    );
}
