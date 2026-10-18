#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Which rejected arguments an `invalid_argument` error frame names, measured on the wire.
//!
//! Frames are read as raw JSON rather than through `decode_daemon_message`,
//! because an omitted key and a `null` one decode to the same `Option::None`.

mod common;

use std::path::Path;
use std::time::Duration;

use serde_json::{Map, Value};
use tokio::io::AsyncReadExt;

use common::{
    Harness, empty_lookup, guaranteed_nonexistent_exe_path, handoff_profile_config, send,
    write_profile,
};
use yagura_protocol::ClientMessage;
use yagura_test_support::ipc::Connection;
use yagura_test_support::paths::TempDir;
use yagura_test_support::process::echo_child_path;

const FRAME_TIMEOUT: Duration = Duration::from_secs(10);

/// The key an error frame carries to name the argument it rejected.
const ARGUMENT_KEY: &str = "rejected_argument";

fn echo_child_argv() -> Vec<String> {
    vec![
        echo_child_path()
            .to_str()
            .expect("echo_child path must be valid UTF-8")
            .to_string(),
    ]
}

/// Reads one frame and returns its body parsed as JSON.
async fn read_frame_json(client: &mut Connection) -> Value {
    let mut len_buf = [0u8; 4];
    tokio::time::timeout(FRAME_TIMEOUT, client.read_exact(&mut len_buf))
        .await
        .expect("timed out waiting for a frame length prefix")
        .expect("connection closed while reading a frame length prefix");
    let mut body = vec![0u8; u32::from_le_bytes(len_buf) as usize];
    tokio::time::timeout(FRAME_TIMEOUT, client.read_exact(&mut body))
        .await
        .expect("timed out waiting for a frame body")
        .expect("connection closed while reading a frame body");
    serde_json::from_slice(&body).expect("the daemon must only ever emit well-formed JSON frames")
}

/// Reads one frame and asserts it is an `invalid_argument` error,
/// returning the whole object so the caller can inspect its keys.
async fn read_invalid_argument_frame(client: &mut Connection) -> Map<String, Value> {
    let frame = read_frame_json(client).await;
    let object = frame
        .as_object()
        .unwrap_or_else(|| panic!("a daemon frame must be a JSON object, got {frame}"))
        .clone();
    assert_eq!(
        object.get("type").and_then(Value::as_str),
        Some("error"),
        "expected an error frame, got {frame}"
    );
    assert_eq!(
        object.get("code").and_then(Value::as_str),
        Some("invalid_argument"),
        "the rejection code must stay invalid_argument, got {frame}"
    );
    object
}

fn message_of(frame: &Map<String, Value>) -> &str {
    frame
        .get("message")
        .and_then(Value::as_str)
        .expect("an error frame must carry a message")
}

/// A profile whose `[command]` names a program nothing resolves: every case
/// here is refused before a spawn is ever attempted.
fn cwd_probe_profile_source(declared_cwd: Option<&Path>) -> String {
    let cwd_line = match declared_cwd {
        Some(path) => format!("cwd = '{}'\n", path.display()),
        None => String::new(),
    };
    format!(
        "schema = 1\ndisplay_name = \"Cwd Probe\"\n{cwd_line}\n[command]\ndefault = \
         ['rejected-argument-probe']\n"
    )
}

async fn profile_harness(tag: &str, tree: &TempDir, declared_cwd: Option<&Path>) -> Harness {
    let profiles_dir = tree.make_dir("profiles");
    let detection_dir = tree.make_dir("detection");
    write_profile(
        &profiles_dir,
        "cwd-probe",
        &cwd_probe_profile_source(declared_cwd),
    );
    let config = handoff_profile_config(
        Some(profiles_dir),
        empty_lookup(),
        &detection_dir,
        None,
        None,
        30,
    );
    Harness::start(tag, config).await
}

/// A cwd the caller sent is named by the wire key.
#[tokio::test]
async fn open_window_with_a_nonexistent_caller_cwd_names_cwd_on_the_wire() {
    let h = Harness::start("rejected-arg-caller-cwd", common::opening_config()).await;
    let mut client = h.connect().await;

    send(
        &mut client,
        &ClientMessage::OpenWindow {
            command: echo_child_argv(),
            cwd: Some(
                guaranteed_nonexistent_exe_path()
                    .to_string_lossy()
                    .into_owned(),
            ),
            name: None,
            vendor: None,
            detection: None,
            on_name_taken: Default::default(),
        },
    )
    .await;

    let frame = read_invalid_argument_frame(&mut client).await;
    assert_eq!(
        frame.get(ARGUMENT_KEY).and_then(Value::as_str),
        Some("cwd"),
        "a cwd the caller sent must be named on the wire"
    );
}

/// A caller-sent cwd is named even when a profile declared one of
/// its own (which this case makes resolvable, so only the caller's value can
/// be the one that failed).
#[tokio::test]
async fn open_window_from_profile_with_a_nonexistent_caller_cwd_names_cwd_on_the_wire() {
    let tree = TempDir::new("yagura-cli-test", "rejected-arg-profile-caller-cwd");
    let declared = tree.make_dir("declared-cwd");
    let h = profile_harness(
        "rejected-arg-profile-caller-cwd",
        &tree,
        Some(declared.as_path()),
    )
    .await;
    let mut client = h.connect().await;

    let missing = guaranteed_nonexistent_exe_path();
    send(
        &mut client,
        &ClientMessage::OpenWindowFromProfile {
            profile: "cwd-probe".to_string(),
            cwd: Some(missing.to_string_lossy().into_owned()),
            name: None,
            model: None,
            effort: None,
            on_name_taken: Default::default(),
        },
    )
    .await;

    let frame = read_invalid_argument_frame(&mut client).await;
    assert_eq!(
        frame.get(ARGUMENT_KEY).and_then(Value::as_str),
        Some("cwd"),
        "a caller-sent cwd must be named even on the profile route"
    );
    assert!(
        message_of(&frame).contains(&missing.to_string_lossy().into_owned()),
        "the reason must carry the path, got: {}",
        message_of(&frame)
    );
}

/// A cwd only the profile declared is refused with the
/// path in the reason, and the argument key is omitted rather than sent as
/// `null`.
#[tokio::test]
async fn open_window_from_profile_with_a_declared_cwd_omits_the_argument_key() {
    let tree = TempDir::new("yagura-cli-test", "rejected-arg-profile-declared-cwd");
    let missing = tree.make_dir("declared-parent").join("declared-but-absent");
    let h = profile_harness(
        "rejected-arg-profile-declared-cwd",
        &tree,
        Some(missing.as_path()),
    )
    .await;
    let mut client = h.connect().await;

    send(
        &mut client,
        &ClientMessage::OpenWindowFromProfile {
            profile: "cwd-probe".to_string(),
            cwd: None,
            name: None,
            model: None,
            effort: None,
            on_name_taken: Default::default(),
        },
    )
    .await;

    let frame = read_invalid_argument_frame(&mut client).await;
    assert!(
        !frame.contains_key(ARGUMENT_KEY),
        "a cwd the caller never sent must leave the key off the wire entirely, got {:?}",
        frame.get(ARGUMENT_KEY)
    );
    assert!(
        message_of(&frame).contains(&missing.to_string_lossy().into_owned()),
        "the reason must carry the path even when no argument is named, got: {}",
        message_of(&frame)
    );
}

/// An empty command is refused without naming cwd, and
/// the key is omitted rather than sent as `null`.
#[tokio::test]
async fn open_window_with_an_empty_command_omits_the_argument_key_on_the_wire() {
    let h = Harness::start("rejected-arg-empty-command", common::opening_config()).await;
    let mut client = h.connect().await;

    send(
        &mut client,
        &ClientMessage::OpenWindow {
            command: Vec::new(),
            cwd: None,
            name: None,
            vendor: None,
            detection: None,
            on_name_taken: Default::default(),
        },
    )
    .await;

    let frame = read_invalid_argument_frame(&mut client).await;
    assert!(
        !frame.contains_key(ARGUMENT_KEY),
        "an empty command must leave the key off the wire entirely, got {:?}",
        frame.get(ARGUMENT_KEY)
    );
}

/// A refused vendor label does not name cwd.
#[tokio::test]
async fn open_window_with_an_invalid_vendor_omits_the_argument_key_on_the_wire() {
    let h = Harness::start("rejected-arg-bad-vendor", common::opening_config()).await;
    let mut client = h.connect().await;

    send(
        &mut client,
        &ClientMessage::OpenWindow {
            command: echo_child_argv(),
            cwd: None,
            name: None,
            vendor: Some("bad\u{0}vendor".to_string()),
            detection: None,
            on_name_taken: Default::default(),
        },
    )
    .await;

    let frame = read_invalid_argument_frame(&mut client).await;
    assert!(
        !frame.contains_key(ARGUMENT_KEY),
        "a refused vendor must not name cwd, got {:?}",
        frame.get(ARGUMENT_KEY)
    );
}

/// A refused detection name does not name cwd.
#[tokio::test]
async fn open_window_with_an_invalid_detection_name_omits_the_argument_key_on_the_wire() {
    let h = Harness::start("rejected-arg-bad-detection", common::opening_config()).await;
    let mut client = h.connect().await;

    send(
        &mut client,
        &ClientMessage::OpenWindow {
            command: echo_child_argv(),
            cwd: None,
            name: None,
            vendor: None,
            detection: Some("../escape".to_string()),
            on_name_taken: Default::default(),
        },
    )
    .await;

    let frame = read_invalid_argument_frame(&mut client).await;
    assert!(
        !frame.contains_key(ARGUMENT_KEY),
        "a refused detection name must not name cwd, got {:?}",
        frame.get(ARGUMENT_KEY)
    );
}
