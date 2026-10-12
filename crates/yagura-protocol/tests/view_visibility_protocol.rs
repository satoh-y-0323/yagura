#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The wire form of telling the daemon whether a window's view is on screen:
//! `set_visible`. While the view is hidden the daemon sends no frames; when it is shown
//! again the daemon sends every row.

use serde_json::json;
use yagura_protocol::{
    ClientMessage, FRAME_MIN_REVISION, MessageError, client_message_wire_tags,
    decode_client_message, encode_client_message, required_protocol_revision,
};

#[test]
fn set_visible_carries_whether_the_view_is_on_screen() {
    for visible in [true, false] {
        let msg = ClientMessage::SetVisible { visible };
        let body = encode_client_message(&msg);
        assert_eq!(decode_client_message(&body).as_ref(), Ok(&msg));
        let wire: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(wire, json!({ "type": "set_visible", "visible": visible }));
    }
}

#[test]
fn set_visible_without_the_flag_is_invalid_json() {
    assert_eq!(
        decode_client_message(br#"{"type":"set_visible"}"#),
        Err(MessageError::InvalidJson)
    );
}

#[test]
fn set_visible_is_a_known_client_message_type() {
    assert!(client_message_wire_tags().contains(&"set_visible"));
}

#[test]
fn set_visible_needs_the_frame_revision() {
    assert_eq!(
        required_protocol_revision(&ClientMessage::SetVisible { visible: false }),
        FRAME_MIN_REVISION
    );
}
