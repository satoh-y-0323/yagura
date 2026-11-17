#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: the relay contract v1 frames between a window's relay and
//! the daemon. A frame is the IPC framing (`frame_encode`: a `u32` LE length
//! and a JSON object body) whose body carries a `type`. The contract is
//! external: a relay and a daemon of any two versions read each other, so the
//! type names, field names and shapes are pinned here.
//!
//! The Rust names under `yagura_protocol::relay` are used inside each test so
//! that a name the implementation chooses differently fails only that test.

use serde_json::{Value, json};
use yagura_protocol::{frame_decode, frame_encode};

/// The body of a frame as a JSON value, after the IPC framing is removed.
fn body_of(framed: &[u8]) -> Value {
    let (body, consumed) = frame_decode(framed)
        .expect("a well-formed frame")
        .expect("a complete frame");
    assert_eq!(consumed, framed.len());
    serde_json::from_slice(body).expect("the body is JSON")
}

#[test]
fn every_relay_frame_has_the_catalogued_type_and_field_names() {
    use yagura_protocol::Declaration;
    use yagura_protocol::relay::{RelayFrame, RelayRefusedKind, RelayResume};

    let cases: Vec<(RelayFrame, Value)> = vec![
        (
            RelayFrame::Hello {
                contract: 1,
                window_id: Some("w1".to_owned()),
                token: Some("tok".to_owned()),
                resume: Some(RelayResume {
                    opening: Some(vec!["{\"id\":1}".to_owned(), "{\"result\":{}}".to_owned()]),
                    tools: "summary".to_owned(),
                }),
                declaration: None,
            },
            json!({
                "type": "relay_hello",
                "contract": 1,
                "window_id": "w1",
                "token": "tok",
                "resume": {"opening": ["{\"id\":1}", "{\"result\":{}}"], "tools": "summary"},
            }),
        ),
        (
            RelayFrame::Hello {
                contract: 1,
                window_id: None,
                token: None,
                resume: None,
                declaration: None,
            },
            json!({"type": "relay_hello", "contract": 1}),
        ),
        (
            RelayFrame::Hello {
                contract: 1,
                window_id: Some("w1".to_owned()),
                token: Some("tok".to_owned()),
                resume: None,
                declaration: Some(Declaration {
                    profile: "declared-profile".to_owned(),
                }),
            },
            json!({
                "type": "relay_hello",
                "contract": 1,
                "window_id": "w1",
                "token": "tok",
                "declaration": {"profile": "declared-profile"},
            }),
        ),
        (
            RelayFrame::Hello {
                contract: 1,
                window_id: None,
                token: None,
                resume: Some(RelayResume {
                    opening: None,
                    tools: "summary".to_owned(),
                }),
                declaration: None,
            },
            json!({"type": "relay_hello", "contract": 1, "resume": {"tools": "summary"}}),
        ),
        (
            RelayFrame::Welcome {
                contract: 1,
                tools: "summary".to_owned(),
            },
            json!({"type": "relay_welcome", "contract": 1, "tools": "summary"}),
        ),
        (
            RelayFrame::Refused {
                kind: RelayRefusedKind::Contract,
                contracts: vec![1],
                reason: "unsupported contract".to_owned(),
            },
            json!({
                "type": "relay_refused",
                "kind": "contract",
                "contracts": [1],
                "reason": "unsupported contract",
            }),
        ),
        (
            RelayFrame::In {
                line: "{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"tools/list\"}".to_owned(),
                seq: Some(9),
            },
            json!({
                "type": "relay_in",
                "line": "{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"tools/list\"}",
                "seq": 9,
            }),
        ),
        (
            RelayFrame::In {
                line: "{\"method\":\"notifications/initialized\"}".to_owned(),
                seq: None,
            },
            json!({"type": "relay_in", "line": "{\"method\":\"notifications/initialized\"}"}),
        ),
        (
            RelayFrame::Out {
                line: "{\"id\":3,\"result\":{}}".to_owned(),
                seq: Some(9),
                receipt: Some(true),
            },
            json!({"type": "relay_out", "line": "{\"id\":3,\"result\":{}}", "seq": 9, "receipt": true}),
        ),
        (
            RelayFrame::Out {
                line: "{\"method\":\"notifications/tools/list_changed\"}".to_owned(),
                seq: None,
                receipt: None,
            },
            json!({"type": "relay_out", "line": "{\"method\":\"notifications/tools/list_changed\"}"}),
        ),
        (
            RelayFrame::Done { seq: 9 },
            json!({"type": "relay_done", "seq": 9}),
        ),
        (
            RelayFrame::Dropped { seq: 9 },
            json!({"type": "relay_dropped", "seq": 9}),
        ),
    ];

    for (frame, wire) in cases {
        // The frame writes exactly the catalogued JSON (no absent optional
        // field appears), and the catalogued JSON reads back to the frame.
        assert_eq!(serde_json::to_value(&frame).unwrap(), wire, "{frame:?}");
        assert_eq!(
            serde_json::from_value::<RelayFrame>(wire.clone()).unwrap(),
            frame
        );

        // On the wire the body rides in the IPC framing.
        let framed = frame_encode(&serde_json::to_vec(&frame).unwrap());
        assert_eq!(body_of(&framed), wire);
    }
}

#[test]
fn a_reader_ignores_fields_it_does_not_know() {
    use yagura_protocol::relay::RelayFrame;

    let newer_out = json!({
        "type": "relay_out",
        "line": "{}",
        "seq": 4,
        "receipt": true,
        "added_later": {"nested": [1, 2]},
    });
    assert_eq!(
        serde_json::from_value::<RelayFrame>(newer_out).unwrap(),
        RelayFrame::Out {
            line: "{}".to_owned(),
            seq: Some(4),
            receipt: Some(true),
        }
    );

    let newer_hello = json!({
        "type": "relay_hello",
        "contract": 1,
        "added_later": "x",
        "resume": {"tools": "t", "added_later": 1},
    });
    assert!(serde_json::from_value::<RelayFrame>(newer_hello).is_ok());

    // A declaration a later relay adds to is still read for what it holds.
    let newer_declaration = json!({
        "type": "relay_hello",
        "contract": 1,
        "declaration": {"profile": "declared-profile", "added_later": {"nested": true}},
    });
    match serde_json::from_value::<RelayFrame>(newer_declaration).unwrap() {
        RelayFrame::Hello { declaration, .. } => assert_eq!(
            declaration
                .map(|declaration| declaration.profile)
                .as_deref(),
            Some("declared-profile")
        ),
        other => panic!("expected relay_hello, got {other:?}"),
    }

    let newer_refused = json!({
        "type": "relay_refused",
        "kind": "limit",
        "contracts": [1],
        "reason": "r",
        "added_later": true,
    });
    assert!(serde_json::from_value::<RelayFrame>(newer_refused).is_ok());
}

#[test]
fn a_body_that_is_not_a_known_relay_frame_is_an_error_the_reader_can_skip() {
    use yagura_protocol::relay::RelayFrame;

    // The IPC framing still delimits an unknown frame, so a reader can skip it
    // by its length; only reading the body as a relay frame fails.
    let unknown = frame_encode(br#"{"type":"relay_from_the_future","seq":1}"#);
    let (body, consumed) = frame_decode(&unknown).unwrap().unwrap();
    assert_eq!(consumed, unknown.len());
    assert!(serde_json::from_slice::<RelayFrame>(body).is_err());

    // A frame of the IPC (no relay type), a body without a type, a body that is
    // not an object and a known type missing a required field are not relay
    // frames either.
    for bad in [
        &br#"{"type":"ping"}"#[..],
        &br#"{"seq":1}"#[..],
        &br#"[1,2]"#[..],
        &br#"{"type":"relay_in","seq":1}"#[..],
        &br#"{"type":"relay_done"}"#[..],
        &br#"{"type":"relay_hello"}"#[..],
        &br#"{"type":"relay_refused","kind":"limit","contracts":[1]}"#[..],
    ] {
        assert!(
            serde_json::from_slice::<RelayFrame>(bad).is_err(),
            "{}",
            String::from_utf8_lossy(bad)
        );
    }
}

#[test]
fn relay_refused_has_exactly_three_kinds_and_the_daemon_speaks_contract_one() {
    use yagura_protocol::relay::{RelayFrame, RelayRefusedKind, SUPPORTED_CONTRACTS};

    for (kind, word) in [
        (RelayRefusedKind::Contract, "contract"),
        (RelayRefusedKind::Limit, "limit"),
        (RelayRefusedKind::SessionMismatch, "session_mismatch"),
    ] {
        let frame = RelayFrame::Refused {
            kind,
            contracts: vec![1],
            reason: "why".to_owned(),
        };
        assert_eq!(serde_json::to_value(&frame).unwrap()["kind"], word);
        assert_eq!(
            serde_json::from_value::<RelayFrame>(
                json!({"type": "relay_refused", "kind": word, "contracts": [1], "reason": "why"})
            )
            .unwrap(),
            frame
        );
    }

    // A kind outside the three is not a refusal a relay can act on.
    assert!(
        serde_json::from_value::<RelayFrame>(
            json!({"type": "relay_refused", "kind": "other", "contracts": [1], "reason": "why"})
        )
        .is_err()
    );

    // Contract 1 is the first version, so the daemon speaks only that one.
    assert_eq!(SUPPORTED_CONTRACTS, [1]);
}

const ERROR_WORDS: [&str; 5] = [
    "daemon_restarted",
    "daemon_unavailable",
    "resource_exhausted",
    "invalid_request",
    "relay_refused",
];

#[test]
fn a_tools_call_is_answered_with_an_is_error_result_that_leads_with_the_word() {
    use yagura_protocol::relay::tool_call_error_line;

    for word in ERROR_WORDS {
        for id in [json!(7), json!("req-1")] {
            let line = tool_call_error_line(&id, word, "The relay could not finish this call.");
            assert!(!line.contains('\n'), "a line must stay one line");
            let reply: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(reply["jsonrpc"], "2.0");
            assert_eq!(reply["id"], id);
            assert!(reply.get("error").is_none(), "a tool error is a result");
            assert_eq!(reply["result"]["isError"], true);
            let content = reply["result"]["content"].as_array().unwrap();
            assert_eq!(content.len(), 1);
            assert_eq!(content[0]["type"], "text");
            let text = content[0]["text"].as_str().unwrap();
            assert!(text.starts_with(word), "{text}");
            assert!(text.contains("The relay could not finish this call."));
        }
    }
}

#[test]
fn any_other_request_is_answered_with_a_json_rpc_error_code_32010_and_the_word() {
    use yagura_protocol::relay::rpc_error_line;

    for word in ERROR_WORDS {
        for id in [json!(8), json!("req-2")] {
            let line = rpc_error_line(&id, word, "The relay could not finish this request.");
            assert!(!line.contains('\n'), "a line must stay one line");
            let reply: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(reply["jsonrpc"], "2.0");
            assert_eq!(reply["id"], id);
            assert!(reply.get("result").is_none(), "a request error is an error");
            assert_eq!(reply["error"]["code"], -32010);
            let message = reply["error"]["message"].as_str().unwrap();
            assert!(message.starts_with(word), "{message}");
            assert!(message.contains("The relay could not finish this request."));
            assert_eq!(reply["error"]["data"]["code"], word);
        }
    }
}
