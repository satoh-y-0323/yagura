#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The wire form of the doors a CLI's hook uses to read a window's inbox:
//! `deliver_context`, `deliver_take`, `deliver_wait` and `report_session`, and the
//! replies `delivery` and `session_recorded`. These are external contracts: a hook
//! shipped with one build talks to a daemon of another.

use serde_json::json;
use yagura_protocol::{
    ClientMessage, DaemonMessage, DeliveryStream, MessageError, PROTOCOL_REVISION,
    client_message_wire_tags, daemon_message_wire_tags, decode_client_message,
    decode_daemon_message, encode_client_message, encode_daemon_message,
    required_protocol_revision,
};

const INBOX_REVISION: u32 = 22;

fn client_wire(msg: &ClientMessage) -> serde_json::Value {
    let body = encode_client_message(msg);
    assert_eq!(decode_client_message(&body).as_ref(), Ok(msg));
    serde_json::from_slice(&body).unwrap()
}

#[test]
fn the_three_inbox_reads_carry_the_window_and_its_token() {
    let cases = [
        (
            ClientMessage::DeliverContext {
                window_id: "w-1".into(),
                token: "t".into(),
            },
            "deliver_context",
        ),
        (
            ClientMessage::DeliverTake {
                window_id: "w-1".into(),
                token: "t".into(),
            },
            "deliver_take",
        ),
        (
            ClientMessage::DeliverWait {
                window_id: "w-1".into(),
                token: "t".into(),
            },
            "deliver_wait",
        ),
    ];
    for (msg, tag) in cases {
        assert_eq!(
            client_wire(&msg),
            json!({ "type": tag, "window_id": "w-1", "token": "t" })
        );
    }
}

#[test]
fn report_session_carries_the_value_the_hook_read() {
    let msg = ClientMessage::ReportSession {
        window_id: "w-1".into(),
        token: "t".into(),
        session: "abc-123".into(),
    };
    assert_eq!(
        client_wire(&msg),
        json!({ "type": "report_session", "window_id": "w-1", "token": "t", "session": "abc-123" })
    );
}

#[test]
fn an_inbox_read_without_its_token_is_invalid_json() {
    for body in [
        r#"{"type":"deliver_context","window_id":"w-1"}"#,
        r#"{"type":"deliver_take","window_id":"w-1"}"#,
        r#"{"type":"deliver_wait","window_id":"w-1"}"#,
        r#"{"type":"report_session","window_id":"w-1","session":"s"}"#,
        r#"{"type":"report_session","window_id":"w-1","token":"t"}"#,
    ] {
        assert_eq!(
            decode_client_message(body.as_bytes()),
            Err(MessageError::InvalidJson),
            "{body}"
        );
    }
}

#[test]
fn delivery_names_the_text_the_stream_and_the_exit_code() {
    for (stream, name) in [
        (DeliveryStream::Stdout, "stdout"),
        (DeliveryStream::Stderr, "stderr"),
    ] {
        let msg = DaemonMessage::Delivery {
            output: "hello\n\"quoted\"".into(),
            stream,
            exit_code: 2,
        };
        let body = encode_daemon_message(&msg);
        assert_eq!(decode_daemon_message(&body).as_ref(), Ok(&msg));
        let wire: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            wire,
            json!({
                "type": "delivery",
                "output": "hello\n\"quoted\"",
                "stream": name,
                "exit_code": 2
            })
        );
    }
}

#[test]
fn session_recorded_says_whether_the_daemon_kept_the_value() {
    for held in [true, false] {
        let msg = DaemonMessage::SessionRecorded { held };
        let body = encode_daemon_message(&msg);
        assert_eq!(decode_daemon_message(&body).as_ref(), Ok(&msg));
        let wire: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(wire, json!({ "type": "session_recorded", "held": held }));
    }
}

#[test]
fn an_unknown_delivery_stream_is_not_decoded() {
    let body = br#"{"type":"delivery","output":"","stream":"stdin","exit_code":0}"#;
    assert_eq!(decode_daemon_message(body), Err(MessageError::InvalidJson));
}

#[test]
fn the_new_messages_are_known_wire_tags() {
    for tag in [
        "deliver_context",
        "deliver_take",
        "deliver_wait",
        "report_session",
    ] {
        assert!(client_message_wire_tags().contains(&tag), "{tag}");
    }
    for tag in ["delivery", "session_recorded"] {
        assert!(daemon_message_wire_tags().contains(&tag), "{tag}");
    }
}

#[test]
fn the_inbox_reads_need_the_inbox_revision() {
    const { assert!(PROTOCOL_REVISION >= INBOX_REVISION) };
    for msg in [
        ClientMessage::DeliverContext {
            window_id: "w".into(),
            token: "t".into(),
        },
        ClientMessage::DeliverTake {
            window_id: "w".into(),
            token: "t".into(),
        },
        ClientMessage::DeliverWait {
            window_id: "w".into(),
            token: "t".into(),
        },
        ClientMessage::ReportSession {
            window_id: "w".into(),
            token: "t".into(),
            session: "s".into(),
        },
    ] {
        assert_eq!(required_protocol_revision(&msg), INBOX_REVISION);
    }
}
