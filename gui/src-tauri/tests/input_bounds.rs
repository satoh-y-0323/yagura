#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The core refuses input the WebView sends past its bounds before it reaches the daemon,
//! for the reason `yagura_protocol::check_input_bounds` gives -- the check the daemon
//! applies too -- and accepts what the daemon would take. A paste may be 1 MiB.

use yagura_gui::commands::validate_window_input;
use yagura_protocol::{
    CellPosition, ClientMessage, Key, KeyInput, MAX_TEXT_INPUT_BYTES, MAX_WHEEL_LINES, Modifiers,
    NamedKey, WheelInput, check_input_bounds,
};

const ONE_MEBIBYTE: usize = 1024 * 1024;

fn paste(len: usize) -> ClientMessage {
    ClientMessage::Paste {
        text: "q".repeat(len),
    }
}

fn wheel(delta: i32) -> ClientMessage {
    ClientMessage::Wheel(WheelInput {
        delta,
        cell: CellPosition { col: 0, row: 0 },
        mods: Modifiers::default(),
    })
}

#[test]
fn a_paste_of_one_mebibyte_is_taken() {
    assert!(validate_window_input(&paste(ONE_MEBIBYTE)).is_ok());
}

#[test]
fn a_paste_past_one_mebibyte_is_refused_for_the_protocols_reason() {
    let over = paste(ONE_MEBIBYTE + 1);
    let reason = check_input_bounds(&over).expect_err("the protocol refuses this paste");
    let error = validate_window_input(&over).expect_err("the core refuses this paste");
    assert!(
        error.to_string().contains(reason),
        "`{error}` should carry the protocol's reason `{reason}`"
    );
}

#[test]
fn text_and_wheel_past_their_bounds_are_refused_for_the_protocols_reasons() {
    let over = [
        ClientMessage::Text {
            text: "q".repeat(MAX_TEXT_INPUT_BYTES + 1),
        },
        wheel(i32::from(MAX_WHEEL_LINES) + 1),
        wheel(-i32::from(MAX_WHEEL_LINES) - 1),
    ];
    for msg in over {
        let reason = check_input_bounds(&msg).expect_err("the protocol refuses this input");
        let error = validate_window_input(&msg).expect_err("the core refuses this input");
        assert!(
            error.to_string().contains(reason),
            "`{error}` should carry the protocol's reason `{reason}`"
        );
    }
}

#[test]
fn the_input_messages_within_their_bounds_are_taken() {
    let taken = [
        ClientMessage::Key(KeyInput {
            key: Key::Named(NamedKey::Enter),
            mods: Modifiers::default(),
        }),
        ClientMessage::Text {
            text: "q".repeat(MAX_TEXT_INPUT_BYTES),
        },
        paste(1),
        wheel(i32::from(MAX_WHEEL_LINES)),
        ClientMessage::Focus { focused: true },
    ];
    for msg in taken {
        assert!(validate_window_input(&msg).is_ok(), "{msg:?}");
    }
}

#[test]
fn a_message_that_is_not_input_is_refused() {
    assert!(validate_window_input(&ClientMessage::Ping).is_err());
    assert!(validate_window_input(&ClientMessage::Resize { cols: 80, rows: 24 }).is_err());
}
