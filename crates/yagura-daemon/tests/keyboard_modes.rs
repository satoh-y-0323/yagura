#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The keyboard protocols a child asks the window's terminal model for: the Kitty
//! keyboard protocol's flag stack and Win32 Input Mode (DECSET 9001). Bytes go straight
//! into a `VtScreen`, without a daemon or a PTY host in between.

use yagura_daemon::detector::{ScreenIntegrity, VtScreen};

fn screen() -> VtScreen {
    VtScreen::new(80, 24)
}

#[track_caller]
fn feed(screen: &mut VtScreen, bytes: &[u8]) {
    assert_eq!(
        screen.feed(bytes),
        ScreenIntegrity::Intact,
        "feeding {:?} must not rebuild the model",
        String::from_utf8_lossy(&bytes[..bytes.len().min(32)])
    );
}

/// The reply the model queues to a Kitty flags query.
#[track_caller]
fn query_flags(screen: &mut VtScreen) -> Vec<u8> {
    screen.take_replies();
    feed(screen, b"\x1b[?u");
    screen.take_replies().0
}

#[test]
fn a_kitty_push_takes_effect_as_the_supported_flags_and_a_pop_restores_none() {
    let mut s = screen();
    // 11 = disambiguate | report event types | report all keys; only 1 and 8 are supported.
    feed(&mut s, b"\x1b[>11u");
    assert_eq!(query_flags(&mut s), b"\x1b[?9u");
    assert_eq!(s.input_modes().kitty_flags, 9);

    feed(&mut s, b"\x1b[<u");
    assert_eq!(query_flags(&mut s), b"\x1b[?0u");
    assert_eq!(s.input_modes().kitty_flags, 0);
}

#[test]
fn the_kitty_stack_is_bounded_and_drops_its_oldest_entries() {
    let mut s = screen();
    let mut flood = b"\x1b[>1u".to_vec();
    for _ in 0..10_000 {
        flood.extend_from_slice(b"\x1b[>8u");
    }
    feed(&mut s, &flood);
    assert_eq!(s.input_modes().kitty_flags, 8);

    for _ in 0..15 {
        feed(&mut s, b"\x1b[<u");
    }
    assert_eq!(
        s.input_modes().kitty_flags,
        8,
        "a stack of 16 still holds an 8 after 15 pops"
    );
    feed(&mut s, b"\x1b[<u");
    assert_eq!(
        s.input_modes().kitty_flags,
        0,
        "the first push (1) must have been dropped when the stack overflowed"
    );
}

#[test]
fn decset_9001_sets_win32_input_and_decrst_9001_clears_it() {
    let mut s = screen();
    assert!(!s.input_modes().win32_input);
    feed(&mut s, b"\x1b[?9001h");
    assert!(s.input_modes().win32_input);
    feed(&mut s, b"\x1b[?9001l");
    assert!(!s.input_modes().win32_input);
}

#[test]
fn a_full_reset_clears_the_kitty_flags_but_keeps_win32_input() {
    let mut s = screen();
    feed(&mut s, b"\x1b[>1u\x1b[?9001h");
    assert_eq!(s.input_modes().kitty_flags, 1);
    feed(&mut s, b"\x1bc");
    assert_eq!(s.input_modes().kitty_flags, 0);
    assert!(
        s.input_modes().win32_input,
        "9001 is the PTY host's own request, so a child's reset must not drop it"
    );
}
