#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The one check of how large structured input may be, shared by every sender and the
//! daemon: `check_input_bounds` returns the reason the input is refused, and that text
//! is what each side reports.

use yagura_protocol::{
    CellPosition, ClientMessage, Key, KeyInput, MAX_PASTE_BYTES, MAX_TEXT_INPUT_BYTES,
    MAX_WHEEL_LINES, Modifiers, NamedKey, WheelInput, check_input_bounds,
};

const ONE_MEBIBYTE: usize = 1024 * 1024;

fn paste(len: usize) -> ClientMessage {
    ClientMessage::Paste {
        text: "q".repeat(len),
    }
}

fn text(len: usize) -> ClientMessage {
    ClientMessage::Text {
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
fn a_paste_may_be_one_mebibyte() {
    assert_eq!(MAX_PASTE_BYTES, ONE_MEBIBYTE);
}

#[test]
fn a_paste_up_to_the_limit_is_taken_and_one_byte_more_is_refused() {
    assert_eq!(check_input_bounds(&paste(0)), Ok(()));
    assert_eq!(check_input_bounds(&paste(ONE_MEBIBYTE)), Ok(()));
    assert!(check_input_bounds(&paste(ONE_MEBIBYTE + 1)).is_err());
}

#[test]
fn text_up_to_the_limit_is_taken_and_one_byte_more_is_refused() {
    assert_eq!(check_input_bounds(&text(MAX_TEXT_INPUT_BYTES)), Ok(()));
    assert!(check_input_bounds(&text(MAX_TEXT_INPUT_BYTES + 1)).is_err());
}

#[test]
fn a_wheel_up_to_the_line_limit_is_taken_either_way_and_one_line_more_is_refused() {
    let limit = i32::from(MAX_WHEEL_LINES);
    assert_eq!(check_input_bounds(&wheel(limit)), Ok(()));
    assert_eq!(check_input_bounds(&wheel(-limit)), Ok(()));
    assert!(check_input_bounds(&wheel(limit + 1)).is_err());
    assert!(check_input_bounds(&wheel(-limit - 1)).is_err());
    assert!(check_input_bounds(&wheel(i32::MIN)).is_err());
}

#[test]
fn the_three_refusals_give_three_different_reasons() {
    let reasons = [
        check_input_bounds(&paste(ONE_MEBIBYTE + 1)).unwrap_err(),
        check_input_bounds(&text(MAX_TEXT_INPUT_BYTES + 1)).unwrap_err(),
        check_input_bounds(&wheel(i32::MAX)).unwrap_err(),
    ];
    assert!(reasons.iter().all(|reason| !reason.is_empty()));
    assert_ne!(reasons[0], reasons[1]);
    assert_ne!(reasons[0], reasons[2]);
    assert_ne!(reasons[1], reasons[2]);
}

#[test]
fn a_message_with_no_bound_to_check_is_not_refused() {
    let key = ClientMessage::Key(KeyInput {
        key: Key::Named(NamedKey::Enter),
        mods: Modifiers::default(),
    });
    assert_eq!(check_input_bounds(&key), Ok(()));
    assert_eq!(check_input_bounds(&ClientMessage::Ping), Ok(()));
    assert_eq!(
        check_input_bounds(&ClientMessage::Focus { focused: true }),
        Ok(())
    );
}
