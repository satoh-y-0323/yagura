#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! What a daemon rejection, and a refusal the GUI's Rust layer raised on its
//! own, still carry by the time they reach the Tauri command boundary.
//!
//! Every test below drives the pure items directly
//! ([`rejection_for`], [`window_open_failure`],
//! [`window_open_failure_from_reason`]). None of them construct an
//! `AppHandle`, a `State` or a live connection, so none of them reach
//! `dispatch` or a command function.

use yagura_gui::commands::{
    WindowOpenFailure, window_open_failure, window_open_failure_from_reason,
};
use yagura_gui::ipc_client::IpcError;
use yagura_gui::message_dispatch::rejection_for;
use yagura_protocol::{DaemonMessage, validate_open_window_exclusivity, validate_window_name};

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

#[test]
fn window_open_failure_keeps_the_rejection_as_its_own_kind() {
    assert_eq!(
        window_open_failure(cwd_rejection()),
        WindowOpenFailure::Rejected {
            code: "invalid_argument".to_owned(),
            message: "cwd does not exist: /nonexistent/project".to_owned(),
            rejected_argument: Some("cwd".to_owned()),
        }
    );
}

#[test]
fn window_open_failure_keeps_a_rejection_that_names_no_argument() {
    let error = rejection_from_frame(
        "resource_exhausted".to_owned(),
        "window limit reached".to_owned(),
        None,
    );
    assert_eq!(
        window_open_failure(error),
        WindowOpenFailure::Rejected {
            code: "resource_exhausted".to_owned(),
            message: "window limit reached".to_owned(),
            rejected_argument: None,
        }
    );
}

#[test]
fn window_open_failure_reports_a_gui_side_failure_as_local() {
    assert_eq!(
        window_open_failure(IpcError::NotConnected),
        WindowOpenFailure::Local {
            message: "not connected to the yagura daemon".to_owned(),
        }
    );
}

#[test]
fn window_open_failure_from_reason_keeps_the_exclusivity_check_reason() {
    let reason = validate_open_window_exclusivity(true, Some("claude"), None, None, false)
        .expect_err("command plus profile must be refused")
        .to_string();
    assert_eq!(
        window_open_failure_from_reason(reason),
        WindowOpenFailure::Local {
            message: "command and profile are mutually exclusive".to_owned(),
        }
    );
}

#[test]
fn window_open_failure_from_reason_keeps_the_window_name_check_reason() {
    let reason = validate_window_name("")
        .expect_err("an empty window name must be refused")
        .to_string();
    assert_eq!(
        window_open_failure_from_reason(reason),
        WindowOpenFailure::Local {
            message: "window label must not be empty".to_owned(),
        }
    );
}

#[test]
fn serialized_rejection_carries_exactly_what_it_was_given() {
    let serialized = serde_json::to_value(window_open_failure(cwd_rejection())).unwrap();
    assert_eq!(
        serialized,
        serde_json::json!({
            "kind": "rejected",
            "code": "invalid_argument",
            "message": "cwd does not exist: /nonexistent/project",
            "rejected_argument": "cwd",
        })
    );
}

#[test]
fn serialized_rejection_omits_the_argument_key_when_none_is_named() {
    let error = rejection_from_frame("internal".to_owned(), "store write failed".to_owned(), None);
    let serialized = serde_json::to_value(window_open_failure(error)).unwrap();
    assert_eq!(
        serialized,
        serde_json::json!({
            "kind": "rejected",
            "code": "internal",
            "message": "store write failed",
        })
    );
}

#[test]
fn serialized_local_failure_carries_exactly_what_it_was_given() {
    let serialized = serde_json::to_value(window_open_failure_from_reason(
        "cwd is not a directory".to_owned(),
    ))
    .unwrap();
    assert_eq!(
        serialized,
        serde_json::json!({
            "kind": "local",
            "message": "cwd is not a directory",
        })
    );
}
