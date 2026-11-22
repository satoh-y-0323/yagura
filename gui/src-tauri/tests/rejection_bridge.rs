#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! What a daemon rejection, and a refusal the GUI's Rust layer raised on its
//! own, still carry by the time they reach the Tauri command boundary.
//!
//! Every test below drives the pure items directly
//! ([`rejection_for`], [`CommandFailure`]'s `From<IpcError>` and
//! [`CommandFailure::from_reason`]). None of them construct an `AppHandle`, a
//! `State` or a live connection, so none of them reach `dispatch` or a
//! command function. A failure is read as the JSON the WebView receives.

use serde_json::{Value, json};
use yagura_gui::commands::CommandFailure;
use yagura_gui::ipc_client::IpcError;
use yagura_gui::message_dispatch::rejection_for;
use yagura_os::OsError;
use yagura_protocol::{DaemonMessage, client_message_wire_tags};

/// The rejection an error frame carrying these values turns into.
fn rejection_from_frame(
    code: String,
    message: String,
    rejected_argument: Option<String>,
) -> IpcError {
    rejection_for(DaemonMessage::Error {
        code,
        message,
        rejected_argument,
    })
    .expect("an error frame is a rejection")
}

/// A rejection the daemon answers an `open_window` request with when the
/// `cwd` the caller sent does not exist.
fn cwd_rejection() -> IpcError {
    rejection_from_frame(
        "invalid_argument".to_owned(),
        "cwd does not exist: /nonexistent/project".to_owned(),
        Some("cwd".to_owned()),
    )
}

/// The three parts of an [`IpcError::Rejected`], or a panic naming the
/// variant that stood there instead.
fn rejection_parts(error: &IpcError) -> (&str, &str, Option<&str>) {
    match error {
        IpcError::Rejected {
            code,
            message,
            rejected_argument,
        } => (code, message, rejected_argument.as_deref()),
        other => panic!("expected a rejection, got {other:?}"),
    }
}

#[test]
fn rejection_keeps_the_daemon_reason_text() {
    let error = cwd_rejection();
    let (_, message, _) = rejection_parts(&error);
    assert_eq!(message, "cwd does not exist: /nonexistent/project");
}

#[test]
fn rejection_keeps_the_name_of_the_argument_that_was_invalid() {
    let error = cwd_rejection();
    let (_, _, rejected_argument) = rejection_parts(&error);
    assert_eq!(rejected_argument, Some("cwd"));
}

#[test]
fn rejection_keeps_the_error_code() {
    let error = cwd_rejection();
    let (code, _, _) = rejection_parts(&error);
    assert_eq!(code, "invalid_argument");
}

#[test]
fn rejection_without_a_reason_or_an_argument_stays_a_rejection() {
    let error = rejection_from_frame("superseded".to_owned(), String::new(), None);
    let (code, message, rejected_argument) = rejection_parts(&error);
    assert_eq!(code, "superseded");
    assert_eq!(message, "");
    assert_eq!(rejected_argument, None);
}

#[test]
fn a_frame_that_is_not_an_error_is_handed_back_unchanged() {
    let handed_back = rejection_for(DaemonMessage::Pong).expect_err("a pong is not a rejection");
    assert!(
        matches!(*handed_back, DaemonMessage::Pong),
        "got {handed_back:?}"
    );
}

#[test]
fn rejection_display_includes_the_daemon_reason_text() {
    let rendered = cwd_rejection().to_string();
    assert!(
        rendered.contains("cwd does not exist: /nonexistent/project"),
        "the rendered rejection must carry the daemon's reason: {rendered:?}"
    );
}

#[test]
fn rejection_display_still_includes_the_error_code() {
    let rendered = cwd_rejection().to_string();
    assert!(
        rendered.contains("invalid_argument"),
        "the rendered rejection must keep naming the code: {rendered:?}"
    );
}

/// The JSON the WebView receives for the failure made from `error`.
fn wire_of(error: IpcError) -> Value {
    serde_json::to_value(CommandFailure::from(error)).unwrap()
}

/// One error of every [`IpcError`] variant. The match below names each
/// variant, so a new one does not compile until it is listed here too.
fn one_of_each_ipc_error() -> Vec<IpcError> {
    let errors = vec![
        IpcError::EndpointName(OsError::Security("no user id".to_owned())),
        IpcError::NotConnected,
        IpcError::ConnectionLost,
        IpcError::ConnectTimeout,
        cwd_rejection(),
        IpcError::UnexpectedReply,
        IpcError::Write,
        IpcError::DaemonNotFound,
        IpcError::DaemonSpawn,
        IpcError::InvalidArgument("cols"),
        IpcError::WindowNotAttached {
            window_id: "w1".to_owned(),
        },
        IpcError::ApproverNotRegistered,
        IpcError::Unsupported {
            request_type: refused_request_tag().to_owned(),
        },
    ];
    for error in &errors {
        match error {
            IpcError::EndpointName(_)
            | IpcError::NotConnected
            | IpcError::ConnectionLost
            | IpcError::ConnectTimeout
            | IpcError::Rejected { .. }
            | IpcError::UnexpectedReply
            | IpcError::Write
            | IpcError::DaemonNotFound
            | IpcError::DaemonSpawn
            | IpcError::InvalidArgument(_)
            | IpcError::WindowNotAttached { .. }
            | IpcError::ApproverNotRegistered
            | IpcError::Unsupported { .. } => {}
        }
    }
    errors
}

#[test]
fn every_failure_carries_the_sentence_of_the_error_it_was_made_from() {
    let sentences: Vec<String> = one_of_each_ipc_error()
        .iter()
        .map(ToString::to_string)
        .collect();
    let carried: Vec<Value> = one_of_each_ipc_error()
        .into_iter()
        .map(|error| wire_of(error)["text"].clone())
        .collect();
    let expected: Vec<Value> = sentences.into_iter().map(Value::String).collect();
    assert_eq!(carried, expected);
}

#[test]
fn a_rejection_without_a_reason_keeps_its_code_in_the_sentence() {
    let error = rejection_from_frame("superseded".to_owned(), String::new(), None);
    assert_eq!(
        wire_of(error),
        json!({
            "kind": "rejected",
            "code": "superseded",
            "message": "",
            "text": "the daemon rejected the request: superseded",
        })
    );
}

#[test]
fn a_gui_side_failure_reaches_the_webview_as_local_with_its_sentence_only() {
    assert_eq!(
        wire_of(IpcError::InvalidArgument("cols")),
        json!({
            "kind": "local",
            "text": "invalid argument: cols",
        })
    );
}

#[test]
fn serialized_rejection_carries_exactly_what_it_was_given() {
    assert_eq!(
        wire_of(cwd_rejection()),
        json!({
            "kind": "rejected",
            "code": "invalid_argument",
            "message": "cwd does not exist: /nonexistent/project",
            "rejected_argument": "cwd",
            "text": "the daemon rejected the request: invalid_argument: cwd does not exist: /nonexistent/project",
        })
    );
}

#[test]
fn serialized_rejection_omits_the_argument_key_when_none_is_named() {
    let error = rejection_from_frame("internal".to_owned(), "store write failed".to_owned(), None);
    assert_eq!(
        wire_of(error),
        json!({
            "kind": "rejected",
            "code": "internal",
            "message": "store write failed",
            "text": "the daemon rejected the request: internal: store write failed",
        })
    );
}

#[test]
fn serialized_local_failure_carries_exactly_what_it_was_given() {
    let serialized = serde_json::to_value(CommandFailure::from_reason(
        "cwd is not a directory".to_owned(),
    ))
    .unwrap();
    assert_eq!(
        serialized,
        json!({
            "kind": "local",
            "text": "cwd is not a directory",
        })
    );
}

/// The wire tag of a request the GUI refuses unwritten, read back from the
/// protocol crate's own table rather than introduced as a new word here.
fn refused_request_tag() -> &'static str {
    client_message_wire_tags()
        .iter()
        .copied()
        .find(|tag| *tag == "open_window_from_profile")
        .expect("the refused request's tag must be one the protocol already names")
}

#[test]
fn a_serialized_gate_refusal_carries_exactly_what_it_was_given() {
    let serialized = wire_of(IpcError::Unsupported {
        request_type: refused_request_tag().to_owned(),
    });
    assert_eq!(
        serialized,
        json!({
            "kind": "unsupported",
            "request_type": "open_window_from_profile",
            "text": "not sent, as the connected daemon speaks another wire revision: open_window_from_profile",
        })
    );
}
