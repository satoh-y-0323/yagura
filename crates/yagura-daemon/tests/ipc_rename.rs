#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The `ClientMessage::RenameWindow` / `DaemonMessage::WindowRenamed` IPC contract.
//!
//! The validation rules themselves live in `yagura-protocol` (`validate_window_name`);
//! this file only pins how the daemon wires that validation into its IPC surface. Both
//! `RenameWindow` and `OpenWindow.name` must pass through it, including the name
//! *derived* from `command[0]`: that string is fully caller-controlled, so validating
//! only an explicitly supplied `name` would close the front door and leave the back one
//! open.
//!
//! A rename changes a display label and never touches the child process, so it must not
//! share `send_text`'s `child_exited` gate. The echoed `WindowRenamed.name` is the value
//! read back from storage, not a pass-through of the request.

mod common;

use std::sync::Arc;
use std::time::Duration;

use yagura_test_support::ipc::{
    Connection, connect_with_retry, read_status_skipping_message, send_client_message,
    unique_endpoint_name,
};
use yagura_test_support::process::echo_child_path;

use yagura_daemon::{IpcServer, WindowRegistry};
use yagura_protocol::{ClientMessage, DaemonMessage, WINDOW_NAME_MAX_BYTES};

use common::open_exited_window_via_ipc;

// Every read below goes through `read_status_skipping_message`, which drops unsolicited
// `StatusChanged` pushes: the calls in this file race the detection engine's own status
// broadcasts on the same connection.

/// Starts a fresh daemon (`WindowRegistry` + `IpcServer`) on a unique endpoint and
/// returns a connected client.
async fn start_daemon_and_connect(tag: &str) -> Connection {
    let config = common::opening_config();
    let registry = Arc::new(WindowRegistry::new(config.clone()));
    let endpoint = unique_endpoint_name("yagura-test-rename", tag);
    let server = Arc::new(IpcServer::new(registry, endpoint.clone(), config));
    let _server_task = tokio::spawn(server.run());
    connect_with_retry(&endpoint, Duration::from_secs(5)).await
}

/// Opens an `echo_child` window via IPC `OpenWindow` and returns its `window_id`.
async fn open_window_via_ipc(client: &mut Connection) -> String {
    let echo_exe = echo_child_path();
    let echo_str = echo_exe
        .to_str()
        .expect("echo_child path must be valid UTF-8");
    send_client_message(
        client,
        &ClientMessage::OpenWindow {
            command: vec![echo_str.to_string()],
            cwd: None,
            name: None,
            vendor: None,
            detection: None,
        },
    )
    .await;
    let response = read_status_skipping_message(client, Duration::from_secs(5))
        .await
        .expect_message("WindowOpened response");
    match response {
        DaemonMessage::WindowOpened { window_id, .. } => window_id,
        other => panic!("expected WindowOpened, got {other:?}"),
    }
}

/// Sends `ListWindows` and returns the full window list.
async fn list_windows_via_ipc(client: &mut Connection) -> Vec<yagura_protocol::WindowInfo> {
    send_client_message(client, &ClientMessage::ListWindows).await;
    let response = read_status_skipping_message(client, Duration::from_secs(5))
        .await
        .expect_message("WindowList response");
    match response {
        DaemonMessage::WindowList { windows } => windows,
        other => panic!("expected WindowList, got {other:?}"),
    }
}

/// A name of exactly `WINDOW_NAME_MAX_BYTES + 1` ASCII bytes, derived from that constant
/// rather than hard-coding a byte count here.
fn name_one_byte_over_max() -> String {
    "a".repeat(WINDOW_NAME_MAX_BYTES + 1)
}

/// U+202E RIGHT-TO-LEFT OVERRIDE, one of the characters
/// `is_forbidden_window_name_char` rejects (the bidirectional embedding / override
/// block).
const BIDI_OVERRIDE: char = '\u{202E}';

/// A `command[0]` whose file stem carries [`BIDI_OVERRIDE`], so
/// `default_name_from_command` (`window.rs`) derives a display name
/// `validate_window_name` forbids.
///
/// The path deliberately does not exist: the derived name is a pure function of
/// caller-controlled data, so the check belongs before the spawn attempt. Expecting
/// `invalid_argument` rather than `spawn_failed` is what pins that ordering.
const COMMAND_WITH_FORBIDDEN_STEM: &str = "C:\\yagura-no-such-dir\\ev\u{202E}il.exe";

#[tokio::test]
async fn rename_of_unknown_window_id_returns_window_not_found() {
    let mut client = start_daemon_and_connect("unknown").await;

    send_client_message(
        &mut client,
        &ClientMessage::RenameWindow {
            window_id: "w-does-not-exist".to_string(),
            name: "new name".to_string(),
        },
    )
    .await;

    let response = read_status_skipping_message(&mut client, Duration::from_secs(5))
        .await
        .expect_message("rename_window error response for an unknown window_id");
    match response {
        DaemonMessage::Error { code, .. } => {
            assert_eq!(
                code, "window_not_found",
                "rename_window on an unknown window_id must return window_not_found \
                 (existing daemon spelling, see window_not_found_msg() in src/ipc.rs)"
            );
        }
        other => panic!("expected error(window_not_found), got {other:?}"),
    }
}

#[tokio::test]
async fn rename_success_is_reflected_in_list_windows_name() {
    let mut client = start_daemon_and_connect("success").await;
    let window_id = open_window_via_ipc(&mut client).await;

    send_client_message(
        &mut client,
        &ClientMessage::RenameWindow {
            window_id: window_id.clone(),
            name: "renamed window".to_string(),
        },
    )
    .await;

    let response = read_status_skipping_message(&mut client, Duration::from_secs(5))
        .await
        .expect_message("rename_window success response");
    let renamed_name = match response {
        DaemonMessage::WindowRenamed {
            window_id: id,
            name,
        } => {
            assert_eq!(
                id, window_id,
                "WindowRenamed must echo the target window_id"
            );
            name
        }
        other => panic!("expected WindowRenamed, got {other:?}"),
    };
    assert_eq!(
        renamed_name, "renamed window",
        "single-writer round trip: the stored value read back after the write \
         must equal what was just written"
    );

    let windows = list_windows_via_ipc(&mut client).await;
    let info = windows
        .iter()
        .find(|w| w.window_id == window_id)
        .expect("renamed window must still be present in list_windows");
    assert_eq!(
        info.name, "renamed window",
        "list_windows must reflect the renamed value, not the name the window \
         was opened with"
    );
    assert_eq!(
        info.name, renamed_name,
        "list_windows's name must match what WindowRenamed reported: both read \
         from the same stored value, not two independent echoes of the request"
    );
}

#[tokio::test]
async fn rename_of_exited_window_succeeds_without_child_exited_error() {
    let mut client = start_daemon_and_connect("exited").await;
    let window_id = open_exited_window_via_ipc(&mut client).await;

    send_client_message(
        &mut client,
        &ClientMessage::RenameWindow {
            window_id: window_id.clone(),
            name: "post exit name".to_string(),
        },
    )
    .await;

    let response = read_status_skipping_message(&mut client, Duration::from_secs(5))
        .await
        .expect_message("rename_window response for an Exited window");
    match response {
        DaemonMessage::WindowRenamed {
            window_id: id,
            name,
        } => {
            assert_eq!(id, window_id);
            assert_eq!(name, "post exit name");
        }
        DaemonMessage::Error { code, .. } if code == "child_exited" => panic!(
            "rename_window must NOT apply the child_exited gate: renaming is a \
             display-label change that never touches the child process"
        ),
        other => panic!("expected WindowRenamed, got {other:?}"),
    }

    let windows = list_windows_via_ipc(&mut client).await;
    let info = windows
        .iter()
        .find(|w| w.window_id == window_id)
        .expect("exited window must still be present in list_windows");
    assert_eq!(info.name, "post exit name");
}

#[tokio::test]
async fn rename_rejects_empty_name_as_invalid_argument() {
    let mut client = start_daemon_and_connect("empty").await;
    let window_id = open_window_via_ipc(&mut client).await;

    send_client_message(
        &mut client,
        &ClientMessage::RenameWindow {
            window_id,
            name: String::new(),
        },
    )
    .await;

    let response = read_status_skipping_message(&mut client, Duration::from_secs(5))
        .await
        .expect_message("rename_window error response for an empty name");
    match response {
        DaemonMessage::Error { code, .. } => {
            assert_eq!(code, "invalid_argument", "an empty name must be rejected")
        }
        other => panic!("expected error(invalid_argument), got {other:?}"),
    }
}

#[tokio::test]
async fn rename_rejects_name_exceeding_max_bytes_as_invalid_argument() {
    let mut client = start_daemon_and_connect("toolong").await;
    let window_id = open_window_via_ipc(&mut client).await;

    send_client_message(
        &mut client,
        &ClientMessage::RenameWindow {
            window_id,
            name: name_one_byte_over_max(),
        },
    )
    .await;

    let response = read_status_skipping_message(&mut client, Duration::from_secs(5))
        .await
        .expect_message("rename_window error response for an over-length name");
    match response {
        DaemonMessage::Error { code, .. } => assert_eq!(
            code, "invalid_argument",
            "a name exceeding WINDOW_NAME_MAX_BYTES must be rejected"
        ),
        other => panic!("expected error(invalid_argument), got {other:?}"),
    }
}

#[tokio::test]
async fn rename_rejects_forbidden_control_char_as_invalid_argument() {
    let mut client = start_daemon_and_connect("forbidden").await;
    let window_id = open_window_via_ipc(&mut client).await;

    send_client_message(
        &mut client,
        &ClientMessage::RenameWindow {
            window_id,
            name: "bad\nname".to_string(),
        },
    )
    .await;

    let response = read_status_skipping_message(&mut client, Duration::from_secs(5))
        .await
        .expect_message("rename_window error response for a name containing a control char");
    match response {
        DaemonMessage::Error { code, .. } => assert_eq!(
            code, "invalid_argument",
            "a name containing a forbidden control character (LF) must be rejected"
        ),
        other => panic!("expected error(invalid_argument), got {other:?}"),
    }
}

#[tokio::test]
async fn open_window_rejects_forbidden_char_name_as_invalid_argument() {
    let mut client = start_daemon_and_connect("open-forbidden").await;
    let echo_exe = echo_child_path();
    let echo_str = echo_exe
        .to_str()
        .expect("echo_child path must be valid UTF-8");

    send_client_message(
        &mut client,
        &ClientMessage::OpenWindow {
            command: vec![echo_str.to_string()],
            cwd: None,
            name: Some("bad\nname".to_string()),
            vendor: None,
            detection: None,
        },
    )
    .await;

    let response = read_status_skipping_message(&mut client, Duration::from_secs(5))
        .await
        .expect_message("open_window error response for a name containing a control char");
    match response {
        DaemonMessage::Error { code, .. } => assert_eq!(
            code, "invalid_argument",
            "open_window's name must be validated by the same SSOT as rename_window \
             (validate_window_name); an unvalidated open_window.name would let a \
             forbidden-char name reach list_windows without ever going through rename"
        ),
        other => panic!("expected error(invalid_argument), got {other:?}"),
    }
}

#[tokio::test]
async fn open_window_without_name_still_defaults_to_command_file_stem() {
    // Non-regression: adding validation to open_window's path must not disturb the
    // pre-existing default-name derivation (`default_name_from_command` in `window.rs`).
    let mut client = start_daemon_and_connect("open-default").await;
    let window_id = open_window_via_ipc(&mut client).await;

    let windows = list_windows_via_ipc(&mut client).await;
    let info = windows
        .iter()
        .find(|w| w.window_id == window_id)
        .expect("window must be present in list_windows");
    assert_eq!(
        info.name, "echo_child",
        "name must still default to command[0]'s file_stem when omitted"
    );
}

#[tokio::test]
async fn open_window_without_name_rejects_forbidden_char_in_command_file_stem() {
    let mut client = start_daemon_and_connect("open-default-forbidden").await;

    send_client_message(
        &mut client,
        &ClientMessage::OpenWindow {
            command: vec![COMMAND_WITH_FORBIDDEN_STEM.to_string()],
            cwd: None,
            name: None,
            vendor: None,
            detection: None,
        },
    )
    .await;

    let response = read_status_skipping_message(&mut client, Duration::from_secs(5))
        .await
        .expect_message("open_window error response for a forbidden-char default name");
    match response {
        DaemonMessage::Error { code, .. } => assert_eq!(
            code, "invalid_argument",
            "the name derived from command[0]'s file stem must go through the same \
             validate_window_name gate as an explicitly supplied name: command[0] is \
             caller-controlled, so validating only the explicit `name` lets a \
             bidi-override-laced stem become the window's display name. \
             `spawn_failed` here means the derived name is still checked after (or \
             never), not before, PtySession::spawn_*"
        ),
        other => panic!("expected error(invalid_argument), got {other:?}"),
    }

    // The rejected open must leave nothing behind: no window carrying the tainted name
    // may appear in list_windows. Sanitizing the stem instead of rejecting it would also
    // trip the assertion above, but this pins the property the gate exists for.
    let windows = list_windows_via_ipc(&mut client).await;
    assert!(
        !windows.iter().any(|w| w.name.contains(BIDI_OVERRIDE)),
        "a rejected open must not leave a window whose display name carries the \
         bidi-override character: {windows:?}"
    );
}
