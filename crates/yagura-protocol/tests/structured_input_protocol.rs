#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The wire form of the GUI's structured input: keys, committed text, pastes,
//! pointer and wheel events and focus changes travel as structure, and the
//! daemon turns them into bytes with the window's terminal modes.

use serde_json::{Value, json};
use yagura_protocol::{
    CellPosition, ClientMessage, FRAME_MIN_REVISION, Key, KeyInput, MessageError, Modifiers,
    NamedKey, PROTOCOL_REVISION, PointerButton, PointerInput, PointerKind,
    STRUCTURED_INPUT_MIN_REVISION, WheelInput, decode_client_message, encode_client_message,
    required_protocol_revision,
};

fn wire(msg: &ClientMessage) -> Value {
    serde_json::from_slice(&encode_client_message(msg)).expect("a frame body is JSON")
}

fn round_trips(msg: &ClientMessage) {
    assert_eq!(
        decode_client_message(&encode_client_message(msg)),
        Ok(msg.clone())
    );
}

fn ctrl_shift() -> Modifiers {
    Modifiers {
        shift: true,
        alt: false,
        ctrl: true,
    }
}

fn key_message() -> ClientMessage {
    ClientMessage::Key(KeyInput {
        key: Key::Named(NamedKey::ArrowUp),
        mods: ctrl_shift(),
    })
}

fn pointer_message() -> ClientMessage {
    ClientMessage::Pointer(PointerInput {
        kind: PointerKind::Down,
        button: PointerButton::Left,
        cell: CellPosition { col: 4, row: 2 },
        mods: Modifiers::default(),
        clicks: 2,
    })
}

fn wheel_message() -> ClientMessage {
    ClientMessage::Wheel(WheelInput {
        delta: -3,
        cell: CellPosition { col: 0, row: 7 },
        mods: Modifiers::default(),
    })
}

fn every_structured_input() -> Vec<ClientMessage> {
    vec![
        key_message(),
        ClientMessage::Text {
            text: "a".to_string(),
        },
        ClientMessage::Paste {
            text: "a\nb".to_string(),
        },
        pointer_message(),
        wheel_message(),
        ClientMessage::Focus { focused: true },
    ]
}

#[test]
fn a_named_key_carries_its_name_and_the_three_modifiers() {
    let msg = key_message();
    let value = wire(&msg);
    assert_eq!(value["type"], "key");
    assert_eq!(value["key"], json!({ "named": "arrow_up" }));
    assert_eq!(
        value["mods"],
        json!({ "shift": true, "alt": false, "ctrl": true })
    );
    round_trips(&msg);
}

#[test]
fn a_character_key_carries_one_character() {
    let msg = ClientMessage::Key(KeyInput {
        key: Key::Char('c'),
        mods: Modifiers {
            shift: false,
            alt: false,
            ctrl: true,
        },
    });
    let value = wire(&msg);
    assert_eq!(value["key"], json!({ "char": "c" }));
    round_trips(&msg);
}

#[test]
fn a_character_key_of_more_than_one_character_does_not_decode() {
    assert_eq!(
        decode_client_message(br#"{"type":"key","key":{"char":"ab"}}"#),
        Err(MessageError::InvalidJson)
    );
}

#[test]
fn every_named_key_has_a_snake_case_wire_name() {
    let names = [
        (NamedKey::Enter, "enter"),
        (NamedKey::Tab, "tab"),
        (NamedKey::Backspace, "backspace"),
        (NamedKey::Escape, "escape"),
        (NamedKey::ArrowUp, "arrow_up"),
        (NamedKey::ArrowDown, "arrow_down"),
        (NamedKey::ArrowLeft, "arrow_left"),
        (NamedKey::ArrowRight, "arrow_right"),
        (NamedKey::Home, "home"),
        (NamedKey::End, "end"),
        (NamedKey::PageUp, "page_up"),
        (NamedKey::PageDown, "page_down"),
        (NamedKey::Insert, "insert"),
        (NamedKey::Delete, "delete"),
        (NamedKey::F1, "f1"),
        (NamedKey::F2, "f2"),
        (NamedKey::F3, "f3"),
        (NamedKey::F4, "f4"),
        (NamedKey::F5, "f5"),
        (NamedKey::F6, "f6"),
        (NamedKey::F7, "f7"),
        (NamedKey::F8, "f8"),
        (NamedKey::F9, "f9"),
        (NamedKey::F10, "f10"),
        (NamedKey::F11, "f11"),
        (NamedKey::F12, "f12"),
    ];
    for (named, expected) in names {
        let msg = ClientMessage::Key(KeyInput {
            key: Key::Named(named),
            mods: Modifiers::default(),
        });
        assert_eq!(wire(&msg)["key"], json!({ "named": expected }), "{named:?}");
        round_trips(&msg);
    }
}

#[test]
fn a_key_without_modifiers_on_the_wire_decodes_with_none_held() {
    assert_eq!(
        decode_client_message(br#"{"type":"key","key":{"named":"enter"}}"#),
        Ok(ClientMessage::Key(KeyInput {
            key: Key::Named(NamedKey::Enter),
            mods: Modifiers::default(),
        }))
    );
}

#[test]
fn modifiers_carry_no_meta() {
    let value = wire(&key_message());
    let mods = value["mods"].as_object().expect("mods is an object");
    let mut keys: Vec<&str> = mods.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(keys, ["alt", "ctrl", "shift"]);
}

#[test]
fn text_and_paste_carry_their_text() {
    let text = ClientMessage::Text {
        text: "h\u{e9}".to_string(),
    };
    assert_eq!(wire(&text), json!({ "type": "text", "text": "h\u{e9}" }));
    round_trips(&text);

    let paste = ClientMessage::Paste {
        text: "a\r\nb".to_string(),
    };
    assert_eq!(wire(&paste), json!({ "type": "paste", "text": "a\r\nb" }));
    round_trips(&paste);
}

#[test]
fn a_pointer_event_carries_kind_button_cell_modifiers_and_clicks() {
    let msg = pointer_message();
    let value = wire(&msg);
    assert_eq!(value["type"], "pointer");
    assert_eq!(value["kind"], "down");
    assert_eq!(value["button"], "left");
    assert_eq!(value["cell"], json!({ "col": 4, "row": 2 }));
    assert_eq!(
        value["mods"],
        json!({ "shift": false, "alt": false, "ctrl": false })
    );
    assert_eq!(value["clicks"], 2);
    round_trips(&msg);
}

#[test]
fn pointer_kinds_and_buttons_have_snake_case_wire_names() {
    for (kind, expected) in [
        (PointerKind::Down, "down"),
        (PointerKind::Up, "up"),
        (PointerKind::Move, "move"),
    ] {
        for (button, button_name) in [
            (PointerButton::Left, "left"),
            (PointerButton::Middle, "middle"),
            (PointerButton::Right, "right"),
            (PointerButton::None, "none"),
        ] {
            let msg = ClientMessage::Pointer(PointerInput {
                kind,
                button,
                cell: CellPosition { col: 0, row: 0 },
                mods: Modifiers::default(),
                clicks: 1,
            });
            let value = wire(&msg);
            assert_eq!(value["kind"], expected);
            assert_eq!(value["button"], button_name);
            round_trips(&msg);
        }
    }
}

#[test]
fn a_pointer_event_without_modifiers_or_clicks_decodes_as_a_single_unmodified_click() {
    assert_eq!(
        decode_client_message(
            br#"{"type":"pointer","kind":"up","button":"right","cell":{"col":1,"row":9}}"#
        ),
        Ok(ClientMessage::Pointer(PointerInput {
            kind: PointerKind::Up,
            button: PointerButton::Right,
            cell: CellPosition { col: 1, row: 9 },
            mods: Modifiers::default(),
            clicks: 1,
        }))
    );
}

#[test]
fn a_wheel_event_carries_a_signed_line_count_the_cell_and_modifiers() {
    let msg = wheel_message();
    let value = wire(&msg);
    assert_eq!(value["type"], "wheel");
    assert_eq!(value["delta"], -3);
    assert_eq!(value["cell"], json!({ "col": 0, "row": 7 }));
    round_trips(&msg);
}

#[test]
fn focus_carries_whether_the_window_gained_it() {
    for focused in [true, false] {
        let msg = ClientMessage::Focus { focused };
        assert_eq!(wire(&msg), json!({ "type": "focus", "focused": focused }));
        round_trips(&msg);
    }
}

#[test]
fn every_structured_input_needs_the_structured_input_revision() {
    for msg in every_structured_input() {
        assert_eq!(
            required_protocol_revision(&msg),
            STRUCTURED_INPUT_MIN_REVISION,
            "{msg:?}"
        );
    }
}

#[test]
fn the_structured_input_revision_is_older_than_the_frame_revision_and_implemented_here() {
    let required = required_protocol_revision(&key_message());
    assert!(required < FRAME_MIN_REVISION, "{required}");
    assert!(required <= PROTOCOL_REVISION, "{required}");
}
