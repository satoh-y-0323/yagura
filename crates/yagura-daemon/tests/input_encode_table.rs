#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The daemon's input encoder: structured input plus the window's terminal modes in,
//! the bytes a terminal would write to the PTY out. The expected sequences are those
//! xterm.js 6.0.0 wrote (`Keyboard.ts` `evaluateKeyboardEvent`, `Clipboard.ts`, the
//! mouse encoders), plus yagura's own Shift+Enter.

use yagura_daemon::input_encode::{
    InputModes, MouseTracking, PointerOutcome, WheelOutcome, encode_control_key, encode_focus,
    encode_key, encode_paste, encode_pointer, encode_text, encode_wheel,
};
use yagura_protocol::{
    CellPosition, ControlKey, Key, KeyInput, Modifiers, NamedKey, PointerButton, PointerInput,
    PointerKind, WheelInput,
};

const NONE: Modifiers = Modifiers {
    shift: false,
    alt: false,
    ctrl: false,
};
const SHIFT: Modifiers = Modifiers {
    shift: true,
    alt: false,
    ctrl: false,
};
const ALT: Modifiers = Modifiers {
    shift: false,
    alt: true,
    ctrl: false,
};
const CTRL: Modifiers = Modifiers {
    shift: false,
    alt: false,
    ctrl: true,
};
const CTRL_SHIFT: Modifiers = Modifiers {
    shift: true,
    alt: false,
    ctrl: true,
};
const ALT_SHIFT: Modifiers = Modifiers {
    shift: true,
    alt: true,
    ctrl: false,
};
const CTRL_ALT: Modifiers = Modifiers {
    shift: false,
    alt: true,
    ctrl: true,
};
const ALL: Modifiers = Modifiers {
    shift: true,
    alt: true,
    ctrl: true,
};

fn plain() -> InputModes {
    InputModes::default()
}

fn app_cursor() -> InputModes {
    InputModes {
        app_cursor: true,
        ..InputModes::default()
    }
}

fn named(key: NamedKey, mods: Modifiers, modes: InputModes) -> Vec<u8> {
    encode_key(
        &KeyInput {
            key: Key::Named(key),
            mods,
        },
        modes,
    )
}

fn chr(c: char, mods: Modifiers) -> Vec<u8> {
    encode_key(
        &KeyInput {
            key: Key::Char(c),
            mods,
        },
        plain(),
    )
}

#[track_caller]
fn assert_bytes(actual: Vec<u8>, expected: &[u8], case: &str) {
    assert_eq!(
        actual,
        expected,
        "{case}: got {:?}, want {:?}",
        String::from_utf8_lossy(&actual),
        String::from_utf8_lossy(expected)
    );
}

// --- named keys -------------------------------------------------------------------

#[test]
fn enter_is_cr_and_alt_or_shift_alone_prefix_escape() {
    let cases: [(Modifiers, &[u8]); 6] = [
        (NONE, b"\r"),
        (ALT, b"\x1b\r"),
        (SHIFT, b"\x1b\r"),
        (CTRL, b"\r"),
        (CTRL_SHIFT, b"\r"),
        (ALT_SHIFT, b"\x1b\r"),
    ];
    for (mods, expected) in cases {
        assert_bytes(
            named(NamedKey::Enter, mods, plain()),
            expected,
            &format!("{mods:?}"),
        );
    }
}

#[test]
fn tab_is_ht_and_back_tab_with_shift() {
    let cases: [(Modifiers, &[u8]); 5] = [
        (NONE, b"\t"),
        (SHIFT, b"\x1b[Z"),
        (CTRL, b"\t"),
        (ALT, b"\t"),
        (CTRL_SHIFT, b"\x1b[Z"),
    ];
    for (mods, expected) in cases {
        assert_bytes(
            named(NamedKey::Tab, mods, plain()),
            expected,
            &format!("{mods:?}"),
        );
    }
}

#[test]
fn backspace_is_del_or_bs_with_ctrl_and_alt_prefixes_escape() {
    let cases: [(Modifiers, &[u8]); 5] = [
        (NONE, b"\x7f"),
        (SHIFT, b"\x7f"),
        (CTRL, b"\x08"),
        (ALT, b"\x1b\x7f"),
        (CTRL_ALT, b"\x1b\x08"),
    ];
    for (mods, expected) in cases {
        assert_bytes(
            named(NamedKey::Backspace, mods, plain()),
            expected,
            &format!("{mods:?}"),
        );
    }
}

#[test]
fn escape_is_esc_and_alt_doubles_it() {
    assert_bytes(named(NamedKey::Escape, NONE, plain()), b"\x1b", "none");
    assert_bytes(named(NamedKey::Escape, SHIFT, plain()), b"\x1b", "shift");
    assert_bytes(named(NamedKey::Escape, ALT, plain()), b"\x1b\x1b", "alt");
}

#[test]
fn arrows_are_csi_without_cursor_keys_mode_and_ss3_with_it() {
    let arrows = [
        (NamedKey::ArrowUp, b'A'),
        (NamedKey::ArrowDown, b'B'),
        (NamedKey::ArrowRight, b'C'),
        (NamedKey::ArrowLeft, b'D'),
    ];
    for (key, letter) in arrows {
        assert_bytes(
            named(key, NONE, plain()),
            &[0x1b, b'[', letter],
            &format!("{key:?} normal"),
        );
        assert_bytes(
            named(key, NONE, app_cursor()),
            &[0x1b, b'O', letter],
            &format!("{key:?} application"),
        );
    }
}

#[test]
fn modified_arrows_carry_the_modifier_parameter_in_either_cursor_keys_mode() {
    let mods_and_param: [(Modifiers, u8); 7] = [
        (SHIFT, b'2'),
        (ALT, b'3'),
        (ALT_SHIFT, b'4'),
        (CTRL, b'5'),
        (CTRL_SHIFT, b'6'),
        (CTRL_ALT, b'7'),
        (ALL, b'8'),
    ];
    let arrows = [
        (NamedKey::ArrowUp, b'A'),
        (NamedKey::ArrowDown, b'B'),
        (NamedKey::ArrowRight, b'C'),
        (NamedKey::ArrowLeft, b'D'),
    ];
    for (key, letter) in arrows {
        for (mods, param) in mods_and_param {
            let expected = [0x1b, b'[', b'1', b';', param, letter];
            assert_bytes(
                named(key, mods, plain()),
                &expected,
                &format!("{key:?} {mods:?}"),
            );
            assert_bytes(
                named(key, mods, app_cursor()),
                &expected,
                &format!("{key:?} {mods:?} application"),
            );
        }
    }
}

#[test]
fn home_and_end_follow_cursor_keys_mode_and_take_modifiers() {
    assert_bytes(named(NamedKey::Home, NONE, plain()), b"\x1b[H", "home");
    assert_bytes(named(NamedKey::End, NONE, plain()), b"\x1b[F", "end");
    assert_bytes(
        named(NamedKey::Home, NONE, app_cursor()),
        b"\x1bOH",
        "home app",
    );
    assert_bytes(
        named(NamedKey::End, NONE, app_cursor()),
        b"\x1bOF",
        "end app",
    );
    assert_bytes(
        named(NamedKey::Home, CTRL, plain()),
        b"\x1b[1;5H",
        "ctrl home",
    );
    assert_bytes(
        named(NamedKey::End, SHIFT, app_cursor()),
        b"\x1b[1;2F",
        "shift end app",
    );
}

#[test]
fn page_keys_take_a_modifier_parameter_only_with_ctrl() {
    assert_bytes(named(NamedKey::PageUp, NONE, plain()), b"\x1b[5~", "pgup");
    assert_bytes(named(NamedKey::PageDown, NONE, plain()), b"\x1b[6~", "pgdn");
    assert_bytes(
        named(NamedKey::PageUp, CTRL, plain()),
        b"\x1b[5;5~",
        "ctrl pgup",
    );
    assert_bytes(
        named(NamedKey::PageDown, CTRL_ALT, plain()),
        b"\x1b[6;7~",
        "ctrl alt pgdn",
    );
    assert_bytes(
        named(NamedKey::PageUp, ALT, plain()),
        b"\x1b[5~",
        "alt pgup",
    );
}

#[test]
fn shift_page_keys_write_nothing_to_the_child() {
    assert_bytes(named(NamedKey::PageUp, SHIFT, plain()), b"", "shift pgup");
    assert_bytes(named(NamedKey::PageDown, SHIFT, plain()), b"", "shift pgdn");
}

#[test]
fn insert_is_written_only_without_shift_or_ctrl() {
    assert_bytes(named(NamedKey::Insert, NONE, plain()), b"\x1b[2~", "insert");
    assert_bytes(
        named(NamedKey::Insert, ALT, plain()),
        b"\x1b[2~",
        "alt insert",
    );
    assert_bytes(named(NamedKey::Insert, SHIFT, plain()), b"", "shift insert");
    assert_bytes(named(NamedKey::Insert, CTRL, plain()), b"", "ctrl insert");
}

#[test]
fn delete_takes_modifiers() {
    assert_bytes(named(NamedKey::Delete, NONE, plain()), b"\x1b[3~", "delete");
    assert_bytes(
        named(NamedKey::Delete, SHIFT, plain()),
        b"\x1b[3;2~",
        "shift delete",
    );
    assert_bytes(
        named(NamedKey::Delete, CTRL, plain()),
        b"\x1b[3;5~",
        "ctrl delete",
    );
}

#[test]
fn function_keys_without_modifiers() {
    let table: [(NamedKey, &[u8]); 12] = [
        (NamedKey::F1, b"\x1bOP"),
        (NamedKey::F2, b"\x1bOQ"),
        (NamedKey::F3, b"\x1bOR"),
        (NamedKey::F4, b"\x1bOS"),
        (NamedKey::F5, b"\x1b[15~"),
        (NamedKey::F6, b"\x1b[17~"),
        (NamedKey::F7, b"\x1b[18~"),
        (NamedKey::F8, b"\x1b[19~"),
        (NamedKey::F9, b"\x1b[20~"),
        (NamedKey::F10, b"\x1b[21~"),
        (NamedKey::F11, b"\x1b[23~"),
        (NamedKey::F12, b"\x1b[24~"),
    ];
    for (key, expected) in table {
        assert_bytes(named(key, NONE, plain()), expected, &format!("{key:?}"));
        assert_bytes(
            named(key, NONE, app_cursor()),
            expected,
            &format!("{key:?} application"),
        );
    }
}

#[test]
fn function_keys_with_modifiers() {
    let table: [(NamedKey, Modifiers, &[u8]); 12] = [
        (NamedKey::F1, SHIFT, b"\x1b[1;2P"),
        (NamedKey::F2, ALT, b"\x1b[1;3Q"),
        (NamedKey::F3, CTRL, b"\x1b[1;5R"),
        (NamedKey::F4, CTRL_SHIFT, b"\x1b[1;6S"),
        (NamedKey::F5, SHIFT, b"\x1b[15;2~"),
        (NamedKey::F6, ALT, b"\x1b[17;3~"),
        (NamedKey::F7, CTRL, b"\x1b[18;5~"),
        (NamedKey::F8, CTRL_ALT, b"\x1b[19;7~"),
        (NamedKey::F9, ALL, b"\x1b[20;8~"),
        (NamedKey::F10, SHIFT, b"\x1b[21;2~"),
        (NamedKey::F11, CTRL, b"\x1b[23;5~"),
        (NamedKey::F12, ALT_SHIFT, b"\x1b[24;4~"),
    ];
    for (key, mods, expected) in table {
        assert_bytes(
            named(key, mods, plain()),
            expected,
            &format!("{key:?} {mods:?}"),
        );
    }
}

// --- character keys ---------------------------------------------------------------

#[test]
fn ctrl_letters_are_c0_controls_whatever_their_case() {
    assert_bytes(chr('a', CTRL), b"\x01", "ctrl a");
    assert_bytes(chr('A', CTRL), b"\x01", "ctrl A (caps lock)");
    assert_bytes(chr('c', CTRL), b"\x03", "ctrl c");
    assert_bytes(chr('z', CTRL), b"\x1a", "ctrl z");
}

#[test]
fn ctrl_symbols_follow_xterm_js() {
    let table: [(char, &[u8]); 10] = [
        (' ', b"\x00"),
        ('[', b"\x1b"),
        ('\\', b"\x1c"),
        (']', b"\x1d"),
        ('/', b"\x1f"),
        ('3', b"\x1b"),
        ('4', b"\x1c"),
        ('5', b"\x1d"),
        ('7', b"\x1f"),
        ('8', b"\x7f"),
    ];
    for (c, expected) in table {
        assert_bytes(chr(c, CTRL), expected, &format!("ctrl {c:?}"));
    }
}

#[test]
fn ctrl_with_a_character_xterm_js_does_not_map_writes_nothing() {
    assert_bytes(chr('1', CTRL), b"", "ctrl 1");
    assert_bytes(chr('A', CTRL_SHIFT), b"", "ctrl shift A");
}

#[test]
fn ctrl_shift_symbols_follow_xterm_js() {
    assert_bytes(chr('_', CTRL_SHIFT), b"\x1f", "ctrl shift _");
    assert_bytes(chr('@', CTRL_SHIFT), b"\x00", "ctrl shift @");
    assert_bytes(chr('^', CTRL_SHIFT), b"\x1e", "ctrl shift ^");
}

#[test]
fn alt_prefixes_the_character_with_escape() {
    let table: [(char, Modifiers, &[u8]); 7] = [
        ('x', ALT, b"\x1bx"),
        ('X', ALT_SHIFT, b"\x1bX"),
        ('1', ALT, b"\x1b1"),
        ('!', ALT_SHIFT, b"\x1b!"),
        (';', ALT, b"\x1b;"),
        (' ', ALT, b"\x1b "),
        ('.', ALT, b"\x1b."),
    ];
    for (c, mods, expected) in table {
        assert_bytes(chr(c, mods), expected, &format!("{mods:?} {c:?}"));
    }
}

#[test]
fn ctrl_alt_prefixes_the_control_character_with_escape() {
    assert_bytes(chr('a', CTRL_ALT), b"\x1b\x01", "ctrl alt a");
    assert_bytes(chr(' ', CTRL_ALT), b"\x1b\x00", "ctrl alt space");
}

#[test]
fn an_unmodified_character_key_is_its_utf8() {
    assert_bytes(chr('a', NONE), b"a", "a");
    assert_bytes(chr('A', SHIFT), b"A", "shift A");
    assert_bytes(chr('\u{e9}', NONE), "\u{e9}".as_bytes(), "e acute");
}

// --- text and paste ---------------------------------------------------------------

#[test]
fn committed_text_is_written_as_utf8_unchanged() {
    let text = "h\u{e9}\u{65e5}\u{672c}\u{1f600}";
    assert_bytes(encode_text(text), text.as_bytes(), "text");
}

#[test]
fn paste_turns_every_line_break_into_cr() {
    assert_bytes(
        encode_paste("a\r\nb\nc\rd", plain()),
        b"a\rb\rc\rd",
        "unbracketed",
    );
}

#[test]
fn paste_is_wrapped_in_bracketed_paste_markers_when_the_mode_is_set() {
    let modes = InputModes {
        bracketed_paste: true,
        ..InputModes::default()
    };
    assert_bytes(
        encode_paste("ab\r\ncd\n", modes),
        b"\x1b[200~ab\rcd\r\x1b[201~",
        "bracketed",
    );
    let text = "\u{65e5}\u{672c}";
    let mut expected = b"\x1b[200~".to_vec();
    expected.extend_from_slice(text.as_bytes());
    expected.extend_from_slice(b"\x1b[201~");
    assert_bytes(encode_paste(text, modes), &expected, "utf8 bracketed");
}

#[test]
fn bracketed_paste_drops_markers_found_in_the_text() {
    let modes = InputModes {
        bracketed_paste: true,
        ..InputModes::default()
    };
    assert_bytes(
        encode_paste("x\x1b[201~y\x1b[200~z", modes),
        b"\x1b[200~xyz\x1b[201~",
        "markers in the text",
    );
}

#[test]
fn bracketed_paste_leaves_no_marker_that_dropping_one_would_rebuild() {
    let modes = InputModes {
        bracketed_paste: true,
        ..InputModes::default()
    };
    for text in [
        "\x1b[20\x1b[201~1~rm -rf",
        "\x1b[2\x1b[200~01~rm -rf",
        "\x1b[\x1b[201~\x1b[200~201~rm -rf",
    ] {
        let encoded: Vec<u8> = encode_paste(text, modes);
        let body = encoded
            .strip_prefix(b"\x1b[200~".as_slice())
            .and_then(|rest: &[u8]| rest.strip_suffix(b"\x1b[201~".as_slice()))
            .unwrap_or_else(|| panic!("{text:?} is not wrapped: {encoded:?}"));
        for marker in [b"\x1b[200~".as_slice(), b"\x1b[201~".as_slice()] {
            assert!(
                !body.windows(marker.len()).any(|window| window == marker),
                "{text:?} left a marker in the pasted body: {:?}",
                String::from_utf8_lossy(body)
            );
        }
        assert!(
            body.windows(6).any(|window| window == b"rm -rf"),
            "{text:?} lost its text: {:?}",
            String::from_utf8_lossy(body)
        );
    }
}

// --- pointer ----------------------------------------------------------------------

fn pointer(
    kind: PointerKind,
    button: PointerButton,
    col: u16,
    row: u16,
    mods: Modifiers,
) -> PointerInput {
    PointerInput {
        kind,
        button,
        cell: CellPosition { col, row },
        mods,
        clicks: 1,
    }
}

fn tracking(mouse: MouseTracking, sgr: bool) -> InputModes {
    InputModes {
        mouse,
        sgr_mouse: sgr,
        ..InputModes::default()
    }
}

fn report(bytes: &[u8]) -> PointerOutcome {
    PointerOutcome::Report(bytes.to_vec())
}

const DOWN: PointerKind = PointerKind::Down;
const UP: PointerKind = PointerKind::Up;
const MOVE: PointerKind = PointerKind::Move;
const LEFT: PointerButton = PointerButton::Left;
const MIDDLE: PointerButton = PointerButton::Middle;
const RIGHT: PointerButton = PointerButton::Right;
const NO_BUTTON: PointerButton = PointerButton::None;

#[test]
fn click_tracking_reports_presses_and_releases_in_the_default_encoding() {
    let modes = tracking(MouseTracking::Click, false);
    assert_eq!(
        encode_pointer(&pointer(DOWN, LEFT, 2, 1, NONE), modes),
        report(b"\x1b[M #\"")
    );
    assert_eq!(
        encode_pointer(&pointer(DOWN, MIDDLE, 2, 1, NONE), modes),
        report(b"\x1b[M!#\"")
    );
    assert_eq!(
        encode_pointer(&pointer(DOWN, RIGHT, 2, 1, NONE), modes),
        report(b"\x1b[M\"#\"")
    );
    assert_eq!(
        encode_pointer(&pointer(UP, LEFT, 2, 1, NONE), modes),
        report(b"\x1b[M##\"")
    );
}

// xterm ctlseqs: the default encoding cannot name the released button, so every
// release reports button 3 with the modifiers kept.
#[test]
fn the_default_encoding_reports_every_release_as_button_three() {
    let modes = tracking(MouseTracking::Click, false);
    assert_eq!(
        encode_pointer(&pointer(UP, MIDDLE, 2, 1, NONE), modes),
        report(b"\x1b[M##\""),
        "middle"
    );
    assert_eq!(
        encode_pointer(&pointer(UP, RIGHT, 2, 1, NONE), modes),
        report(b"\x1b[M##\""),
        "right"
    );
    assert_eq!(
        encode_pointer(&pointer(UP, RIGHT, 2, 1, CTRL), modes),
        report(b"\x1b[M3#\""),
        "ctrl right"
    );
}

#[test]
fn click_tracking_reports_presses_and_releases_in_sgr() {
    let modes = tracking(MouseTracking::Click, true);
    assert_eq!(
        encode_pointer(&pointer(DOWN, LEFT, 2, 1, NONE), modes),
        report(b"\x1b[<0;3;2M")
    );
    assert_eq!(
        encode_pointer(&pointer(DOWN, RIGHT, 2, 1, NONE), modes),
        report(b"\x1b[<2;3;2M")
    );
    assert_eq!(
        encode_pointer(&pointer(UP, LEFT, 2, 1, NONE), modes),
        report(b"\x1b[<0;3;2m")
    );
    assert_eq!(
        encode_pointer(&pointer(UP, RIGHT, 2, 1, NONE), modes),
        report(b"\x1b[<2;3;2m")
    );
}

#[test]
fn click_tracking_ignores_motion() {
    for sgr in [false, true] {
        let modes = tracking(MouseTracking::Click, sgr);
        assert_eq!(
            encode_pointer(&pointer(MOVE, LEFT, 2, 1, NONE), modes),
            PointerOutcome::Ignore,
            "drag, sgr {sgr}"
        );
        assert_eq!(
            encode_pointer(&pointer(MOVE, NO_BUTTON, 2, 1, NONE), modes),
            PointerOutcome::Ignore,
            "hover, sgr {sgr}"
        );
    }
}

#[test]
fn drag_tracking_reports_motion_with_a_button_held() {
    assert_eq!(
        encode_pointer(
            &pointer(MOVE, LEFT, 3, 1, NONE),
            tracking(MouseTracking::Drag, true)
        ),
        report(b"\x1b[<32;4;2M")
    );
    assert_eq!(
        encode_pointer(
            &pointer(MOVE, LEFT, 2, 1, NONE),
            tracking(MouseTracking::Drag, false)
        ),
        report(b"\x1b[M@#\"")
    );
    assert_eq!(
        encode_pointer(
            &pointer(DOWN, LEFT, 2, 1, NONE),
            tracking(MouseTracking::Drag, true)
        ),
        report(b"\x1b[<0;3;2M")
    );
}

#[test]
fn drag_tracking_ignores_motion_without_a_button() {
    assert_eq!(
        encode_pointer(
            &pointer(MOVE, NO_BUTTON, 2, 1, NONE),
            tracking(MouseTracking::Drag, true)
        ),
        PointerOutcome::Ignore
    );
}

#[test]
fn motion_tracking_reports_motion_without_a_button() {
    assert_eq!(
        encode_pointer(
            &pointer(MOVE, NO_BUTTON, 4, 2, NONE),
            tracking(MouseTracking::Motion, true)
        ),
        report(b"\x1b[<35;5;3M")
    );
    assert_eq!(
        encode_pointer(
            &pointer(MOVE, NO_BUTTON, 2, 1, NONE),
            tracking(MouseTracking::Motion, false)
        ),
        report(b"\x1b[MC#\"")
    );
    assert_eq!(
        encode_pointer(
            &pointer(MOVE, LEFT, 4, 2, NONE),
            tracking(MouseTracking::Motion, true)
        ),
        report(b"\x1b[<32;5;3M")
    );
}

#[test]
fn reports_carry_ctrl_and_alt() {
    let modes = tracking(MouseTracking::Click, true);
    assert_eq!(
        encode_pointer(&pointer(DOWN, LEFT, 2, 1, CTRL), modes),
        report(b"\x1b[<16;3;2M")
    );
    assert_eq!(
        encode_pointer(&pointer(DOWN, LEFT, 2, 1, ALT), modes),
        report(b"\x1b[<8;3;2M")
    );
    assert_eq!(
        encode_pointer(&pointer(DOWN, LEFT, 2, 1, CTRL_ALT), modes),
        report(b"\x1b[<24;3;2M")
    );
}

#[test]
fn x10_reports_presses_only_and_without_modifiers() {
    let modes = tracking(MouseTracking::X10, false);
    assert_eq!(
        encode_pointer(&pointer(DOWN, LEFT, 2, 1, NONE), modes),
        report(b"\x1b[M #\"")
    );
    assert_eq!(
        encode_pointer(&pointer(DOWN, LEFT, 2, 1, CTRL_ALT), modes),
        report(b"\x1b[M #\"")
    );
    assert_eq!(
        encode_pointer(&pointer(UP, LEFT, 2, 1, NONE), modes),
        PointerOutcome::Ignore
    );
    assert_eq!(
        encode_pointer(&pointer(MOVE, LEFT, 2, 1, NONE), modes),
        PointerOutcome::Ignore
    );
    assert_eq!(
        encode_pointer(
            &pointer(DOWN, LEFT, 2, 1, NONE),
            tracking(MouseTracking::X10, true)
        ),
        report(b"\x1b[<0;3;2M")
    );
}

#[test]
fn a_press_without_a_button_is_never_reported() {
    assert_eq!(
        encode_pointer(
            &pointer(DOWN, NO_BUTTON, 2, 1, NONE),
            tracking(MouseTracking::Motion, true)
        ),
        PointerOutcome::Ignore
    );
}

#[test]
fn the_default_encoding_writes_coordinates_as_raw_bytes_up_to_255() {
    let modes = tracking(MouseTracking::Click, false);
    assert_eq!(
        encode_pointer(&pointer(DOWN, LEFT, 200, 0, NONE), modes),
        report(&[0x1b, b'[', b'M', 32, 233, 33])
    );
    assert_eq!(
        encode_pointer(&pointer(DOWN, LEFT, 222, 222, NONE), modes),
        report(&[0x1b, b'[', b'M', 32, 255, 255])
    );
    assert_eq!(
        encode_pointer(&pointer(DOWN, LEFT, 223, 0, NONE), modes),
        PointerOutcome::Ignore
    );
}

#[test]
fn sgr_has_no_coordinate_limit() {
    assert_eq!(
        encode_pointer(
            &pointer(DOWN, LEFT, 999, 499, NONE),
            tracking(MouseTracking::Click, true)
        ),
        report(b"\x1b[<0;1000;500M")
    );
}

#[test]
fn without_mouse_tracking_the_pointer_selects() {
    for event in [
        pointer(DOWN, LEFT, 2, 1, NONE),
        pointer(MOVE, LEFT, 5, 1, NONE),
        pointer(UP, LEFT, 5, 1, NONE),
    ] {
        assert_eq!(
            encode_pointer(&event, InputModes::default()),
            PointerOutcome::Select,
            "{event:?}"
        );
        assert_eq!(
            encode_pointer(
                &event,
                InputModes {
                    sgr_mouse: true,
                    ..InputModes::default()
                }
            ),
            PointerOutcome::Select,
            "{event:?} with sgr set but no tracking"
        );
    }
}

#[test]
fn shift_selects_even_while_the_child_tracks_the_mouse() {
    for mouse in [
        MouseTracking::X10,
        MouseTracking::Click,
        MouseTracking::Drag,
        MouseTracking::Motion,
    ] {
        for event in [
            pointer(DOWN, LEFT, 2, 1, SHIFT),
            pointer(MOVE, LEFT, 5, 1, SHIFT),
            pointer(UP, LEFT, 5, 1, SHIFT),
        ] {
            assert_eq!(
                encode_pointer(&event, tracking(mouse, true)),
                PointerOutcome::Select,
                "{mouse:?} {event:?}"
            );
        }
    }
}

// --- wheel ------------------------------------------------------------------------

fn wheel(delta: i32, mods: Modifiers) -> WheelInput {
    WheelInput {
        delta,
        cell: CellPosition { col: 2, row: 1 },
        mods,
    }
}

#[test]
fn wheel_is_reported_while_the_child_tracks_clicks_drags_or_motion() {
    for mouse in [
        MouseTracking::Click,
        MouseTracking::Drag,
        MouseTracking::Motion,
    ] {
        assert_eq!(
            encode_wheel(&wheel(-1, NONE), tracking(mouse, true)),
            WheelOutcome::Write(b"\x1b[<64;3;2M".to_vec()),
            "{mouse:?} up"
        );
        assert_eq!(
            encode_wheel(&wheel(1, NONE), tracking(mouse, true)),
            WheelOutcome::Write(b"\x1b[<65;3;2M".to_vec()),
            "{mouse:?} down"
        );
    }
    assert_eq!(
        encode_wheel(&wheel(-1, NONE), tracking(MouseTracking::Click, false)),
        WheelOutcome::Write(b"\x1b[M`#\"".to_vec()),
        "default encoding up"
    );
    assert_eq!(
        encode_wheel(&wheel(1, NONE), tracking(MouseTracking::Click, false)),
        WheelOutcome::Write(b"\x1b[Ma#\"".to_vec()),
        "default encoding down"
    );
}

#[test]
fn wheel_reports_one_event_per_line_and_carry_modifiers() {
    assert_eq!(
        encode_wheel(&wheel(-2, NONE), tracking(MouseTracking::Click, true)),
        WheelOutcome::Write(b"\x1b[<64;3;2M\x1b[<64;3;2M".to_vec())
    );
    assert_eq!(
        encode_wheel(&wheel(1, CTRL), tracking(MouseTracking::Click, true)),
        WheelOutcome::Write(b"\x1b[<81;3;2M".to_vec())
    );
}

// Unlike a click, Shift does not take the wheel away from the child: it is reported
// as xterm's Shift bit.
#[test]
fn wheel_reports_carry_shift() {
    assert_eq!(
        encode_wheel(&wheel(1, SHIFT), tracking(MouseTracking::Click, true)),
        WheelOutcome::Write(b"\x1b[<69;3;2M".to_vec())
    );
    assert_eq!(
        encode_wheel(&wheel(-1, SHIFT), tracking(MouseTracking::Click, false)),
        WheelOutcome::Write(b"\x1b[Md#\"".to_vec())
    );
}

#[test]
fn wheel_on_the_alternate_screen_with_alternate_scroll_is_arrow_keys() {
    let modes = InputModes {
        alt_screen: true,
        alternate_scroll: true,
        ..InputModes::default()
    };
    assert_eq!(
        encode_wheel(&wheel(-3, NONE), modes),
        WheelOutcome::Write(b"\x1b[A\x1b[A\x1b[A".to_vec())
    );
    assert_eq!(
        encode_wheel(&wheel(2, NONE), modes),
        WheelOutcome::Write(b"\x1b[B\x1b[B".to_vec())
    );
    let application = InputModes {
        app_cursor: true,
        ..modes
    };
    assert_eq!(
        encode_wheel(&wheel(-1, NONE), application),
        WheelOutcome::Write(b"\x1bOA".to_vec())
    );
    assert_eq!(
        encode_wheel(&wheel(1, NONE), application),
        WheelOutcome::Write(b"\x1bOB".to_vec())
    );
}

#[test]
fn mouse_tracking_takes_the_wheel_before_alternate_scroll() {
    let modes = InputModes {
        alt_screen: true,
        alternate_scroll: true,
        mouse: MouseTracking::Click,
        sgr_mouse: true,
        ..InputModes::default()
    };
    assert_eq!(
        encode_wheel(&wheel(-1, NONE), modes),
        WheelOutcome::Write(b"\x1b[<64;3;2M".to_vec())
    );
}

#[test]
fn wheel_otherwise_scrolls_the_view() {
    let main_screen = InputModes {
        alternate_scroll: true,
        ..InputModes::default()
    };
    assert_eq!(
        encode_wheel(&wheel(-3, NONE), main_screen),
        WheelOutcome::ScrollView { lines: -3 }
    );
    let alt_without_alternate_scroll = InputModes {
        alt_screen: true,
        ..InputModes::default()
    };
    assert_eq!(
        encode_wheel(&wheel(2, NONE), alt_without_alternate_scroll),
        WheelOutcome::ScrollView { lines: 2 }
    );
    let x10 = InputModes {
        mouse: MouseTracking::X10,
        ..InputModes::default()
    };
    assert_eq!(
        encode_wheel(&wheel(-1, NONE), x10),
        WheelOutcome::ScrollView { lines: -1 },
        "x10 does not report the wheel"
    );
}

// --- focus ------------------------------------------------------------------------

#[test]
fn focus_is_reported_only_while_the_child_asked_for_it() {
    let on = InputModes {
        focus_reports: true,
        ..InputModes::default()
    };
    assert_bytes(encode_focus(true, on), b"\x1b[I", "in");
    assert_bytes(encode_focus(false, on), b"\x1b[O", "out");
    assert_bytes(encode_focus(true, plain()), b"", "in, off");
    assert_bytes(encode_focus(false, plain()), b"", "out, off");
}

// --- send_keys --------------------------------------------------------------------

#[test]
fn control_keys_encode_like_the_matching_keys() {
    let table: [(ControlKey, &[u8], &[u8]); 6] = [
        (ControlKey::Escape, b"\x1b", b"\x1b"),
        (ControlKey::Backspace, b"\x7f", b"\x7f"),
        (ControlKey::CtrlU, b"\x15", b"\x15"),
        (ControlKey::Enter, b"\r", b"\r"),
        (ControlKey::Up, b"\x1b[A", b"\x1bOA"),
        (ControlKey::Down, b"\x1b[B", b"\x1bOB"),
    ];
    for (key, normal, application) in table {
        assert_eq!(
            encode_control_key(key, plain()).as_deref(),
            Some(normal),
            "{key:?}"
        );
        assert_eq!(
            encode_control_key(key, app_cursor()).as_deref(),
            Some(application),
            "{key:?} application"
        );
    }
}

#[test]
fn an_unknown_control_key_encodes_to_nothing() {
    assert_eq!(encode_control_key(ControlKey::Unknown, plain()), None);
}
