#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The wire form of the doors a CLI's hook uses to read a window's inbox:
//! `deliver_context`, `deliver_take`, `deliver_wait`, `deliver_done` and `report_session`,
//! and the replies `delivery`, `deliver_confirmed` and `session_recorded`. These are external contracts: a hook
//! shipped with one build talks to a daemon of another.

use serde_json::json;
use yagura_protocol::{
    ClientMessage, DELIVER_DONE_MIN_REVISION, DaemonMessage, DeliveryStream, MessageError,
    PROTOCOL_REVISION, client_message_wire_tags, daemon_message_wire_tags, decode_client_message,
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
                held: false,
            },
            "deliver_context",
        ),
        (
            ClientMessage::DeliverTake {
                window_id: "w-1".into(),
                token: "t".into(),
                held: false,
            },
            "deliver_take",
        ),
        (
            ClientMessage::DeliverWait {
                window_id: "w-1".into(),
                token: "t".into(),
                held: false,
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

/// Decodes `wire` as a daemon message and encodes it again: what a peer reads back.
fn daemon_round_trip(wire: &serde_json::Value) -> (DaemonMessage, serde_json::Value) {
    let msg = decode_daemon_message(wire.to_string().as_bytes())
        .unwrap_or_else(|err| panic!("{wire} must decode: {err:?}"));
    let again = serde_json::from_slice(&encode_daemon_message(&msg)).unwrap();
    (msg, again)
}

#[test]
fn delivery_names_the_text_the_stream_and_the_exit_code() {
    for (stream, name) in [
        (DeliveryStream::Stdout, "stdout"),
        (DeliveryStream::Stderr, "stderr"),
    ] {
        // An answer without the hook's deadline, as a daemon before revision 34 sends it.
        let wire = json!({
            "type": "delivery",
            "output": "hello\n\"quoted\"",
            "stream": name,
            "exit_code": 2
        });
        let (msg, again) = daemon_round_trip(&wire);
        match msg {
            DaemonMessage::Delivery {
                output,
                stream: decoded,
                exit_code,
                ..
            } => assert_eq!(
                (output.as_str(), decoded, exit_code),
                ("hello\n\"quoted\"", stream, 2)
            ),
            other => panic!("expected a delivery, got {other:?}"),
        }
        assert_eq!(
            again, wire,
            "an answer without a deadline stays without one"
        );
    }
}

/// The answer to a door's read names the time the CLI gives the hook, which the door
/// measures its own end against.
#[test]
fn a_delivery_carries_the_hooks_deadline_in_whole_seconds() {
    let wire = json!({
        "type": "delivery",
        "output": "x",
        "stream": "stdout",
        "exit_code": 0,
        "hook_timeout_sec": 86400
    });
    let (_, again) = daemon_round_trip(&wire);
    assert_eq!(again, wire, "the deadline must survive a round trip");
}

/// Revision 34's read names its door in one request: the three inbox doors by kind, and a
/// hook event by the profile and event, with the values read from the hook's input once
/// the daemon asked for them.
#[test]
fn a_doors_read_names_the_door_and_the_printed_notice_carries_nothing() {
    let reads = [
        json!({ "kind": "context" }),
        json!({ "kind": "take" }),
        json!({ "kind": "wait" }),
        json!({ "kind": "hook", "profile": "p", "name": "Stop" }),
        json!({
            "kind": "hook", "profile": "p", "name": "Stop",
            "input": ["absent", "present", { "session": "s-1" }]
        }),
    ];
    for door in reads {
        let wire = json!({ "type": "door_read", "window_id": "w-1", "token": "t", "door": door });
        let msg = decode_client_message(wire.to_string().as_bytes())
            .unwrap_or_else(|err| panic!("{wire} must decode: {err:?}"));
        assert_eq!(client_wire(&msg), wire);
    }
    let printed = json!({ "type": "deliver_printed" });
    let msg = decode_client_message(printed.to_string().as_bytes())
        .unwrap_or_else(|err| panic!("{printed} must decode: {err:?}"));
    assert_eq!(client_wire(&msg), printed);
    for body in [
        r#"{"type":"door_read","window_id":"w-1","door":{"kind":"take"}}"#,
        r#"{"type":"door_read","window_id":"w-1","token":"t"}"#,
        r#"{"type":"door_read","window_id":"w-1","token":"t","door":{"kind":"bogus"}}"#,
        r#"{"type":"door_read","window_id":"w-1","token":"t","door":{"kind":"hook","name":"Stop"}}"#,
    ] {
        assert_eq!(
            decode_client_message(body.as_bytes()),
            Err(MessageError::InvalidJson),
            "{body}"
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
        "door_read",
        "deliver_printed",
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
            held: false,
        },
        ClientMessage::DeliverTake {
            window_id: "w".into(),
            token: "t".into(),
            held: false,
        },
        ClientMessage::DeliverWait {
            window_id: "w".into(),
            token: "t".into(),
            held: false,
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

fn the_three_inbox_reads(held: bool) -> [(ClientMessage, &'static str); 3] {
    let (window_id, token) = (String::from("w-1"), String::from("t"));
    [
        (
            ClientMessage::DeliverContext {
                window_id: window_id.clone(),
                token: token.clone(),
                held,
            },
            "deliver_context",
        ),
        (
            ClientMessage::DeliverTake {
                window_id: window_id.clone(),
                token: token.clone(),
                held,
            },
            "deliver_take",
        ),
        (
            ClientMessage::DeliverWait {
                window_id,
                token,
                held,
            },
            "deliver_wait",
        ),
    ]
}

#[test]
fn an_inbox_read_that_waits_for_the_confirmation_says_so_and_one_that_does_not_is_unchanged() {
    for (msg, tag) in the_three_inbox_reads(true) {
        assert_eq!(
            client_wire(&msg),
            json!({ "type": tag, "window_id": "w-1", "token": "t", "held": true })
        );
    }
    for (msg, tag) in the_three_inbox_reads(false) {
        let body = json!({ "type": tag, "window_id": "w-1", "token": "t" }).to_string();
        assert_eq!(
            decode_client_message(body.as_bytes()),
            Ok(msg),
            "an older door's read, sent without held, must decode as one not held: {body}"
        );
    }
}

#[test]
fn deliver_done_carries_nothing_and_deliver_confirmed_says_whether_it_was_withdrawn() {
    let done = ClientMessage::DeliverDone;
    assert_eq!(client_wire(&done), json!({ "type": "deliver_done" }));
    assert!(client_message_wire_tags().contains(&"deliver_done"));
    for withdrawn in [true, false] {
        let msg = DaemonMessage::DeliverConfirmed { withdrawn };
        let body = encode_daemon_message(&msg);
        assert_eq!(decode_daemon_message(&body).as_ref(), Ok(&msg));
        let wire: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            wire,
            json!({ "type": "deliver_confirmed", "withdrawn": withdrawn })
        );
    }
    assert!(daemon_message_wire_tags().contains(&"deliver_confirmed"));
}

#[test]
fn a_held_read_and_deliver_done_need_the_confirmation_revision() {
    assert_eq!(DELIVER_DONE_MIN_REVISION, 33);
    assert_eq!(
        required_protocol_revision(&ClientMessage::DeliverDone),
        DELIVER_DONE_MIN_REVISION
    );
    for (msg, _) in the_three_inbox_reads(true) {
        assert_eq!(
            required_protocol_revision(&msg),
            DELIVER_DONE_MIN_REVISION,
            "{msg:?}"
        );
    }
    for (msg, _) in the_three_inbox_reads(false) {
        assert_eq!(required_protocol_revision(&msg), INBOX_REVISION, "{msg:?}");
    }
}

#[test]
fn a_hook_event_that_waits_for_the_confirmation_says_so_and_one_that_does_not_is_unchanged() {
    let run_hook = |held| ClientMessage::RunHook {
        window_id: "w-1".to_string(),
        token: "t".to_string(),
        profile: "p".to_string(),
        name: "Stop".to_string(),
        input: None,
        held,
        guest: None,
    };
    assert_eq!(
        client_wire(&run_hook(true)),
        json!({
            "type": "run_hook", "window_id": "w-1", "token": "t",
            "profile": "p", "name": "Stop", "held": true
        })
    );
    let unheld = client_wire(&run_hook(false));
    assert!(
        unheld.get("held").is_none(),
        "a hook not held must not put held on the wire: {unheld}"
    );
    assert_eq!(
        decode_client_message(unheld.to_string().as_bytes()),
        Ok(run_hook(false)),
        "a hook sent without held must decode as one not held"
    );
}
