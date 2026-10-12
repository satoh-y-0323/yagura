#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: an attached client sends input as structure (keys, committed text,
//! pastes, pointer and wheel events, focus) and the daemon writes the bytes the child's
//! current terminal modes call for.
//!
//! The child is `hex_echo --emit <mode>...`: it sets the modes, then prints every byte it
//! reads from stdin as hex, so what it received is read back from its output.

mod common;

use std::time::Duration;

use common::{
    Harness, assert_hex_echo_receives, attach, contains_subsequence, expect_error, hex_echo_path,
    hex_of, output_after_marker, read_response, send, wait_until_model_shows,
};
use yagura_daemon::DaemonConfig;
use yagura_protocol::{
    CellPosition, ClientMessage, DaemonMessage, Key, KeyInput, MAX_PASTE_BYTES,
    MAX_TEXT_INPUT_BYTES, MAX_WHEEL_LINES, Modifiers, NamedKey, PointerButton, PointerInput,
    PointerKind, WheelInput,
};
use yagura_test_support::ipc::Connection;

const EMITTED_MARKER: &[u8] = b"QUERIES-EMITTED";
const RECEIVE_TIMEOUT: Duration = Duration::from_secs(10);

const DECCKM: &[u8] = b"\x1b[?1h";
const BRACKETED_PASTE: &[u8] = b"\x1b[?2004h";
const CLICK_TRACKING: &[u8] = b"\x1b[?1000h";
const SGR_MOUSE: &[u8] = b"\x1b[?1006h";
const MOTION_TRACKING: &[u8] = b"\x1b[?1003h";
const FOCUS_REPORTS: &[u8] = b"\x1b[?1004h";
const ALT_SCREEN: &[u8] = b"\x1b[?1049h";
const ALTERNATE_SCROLL: &[u8] = b"\x1b[?1007h";

/// A `hex_echo` window that set `modes`, and a connection attached to it.
async fn attached_window(tag: &str, modes: &[&[u8]]) -> (Harness, String, Connection) {
    let h = Harness::start(tag, DaemonConfig::default()).await;
    let mut command = vec![
        hex_echo_path()
            .to_str()
            .expect("hex_echo path must be valid UTF-8")
            .to_string(),
        "--emit".to_string(),
    ];
    command.extend(modes.iter().map(|mode| hex_of(mode)));
    let window_id = h.open(command, None).await;
    h.wait_for_scrollback(&window_id, EMITTED_MARKER, RECEIVE_TIMEOUT)
        .await;
    wait_until_model_shows(&h, &window_id, EMITTED_MARKER, RECEIVE_TIMEOUT).await;
    let mut client = h.connect().await;
    attach(&mut client, &window_id).await;
    (h, window_id, client)
}

async fn assert_child_receives(h: &Harness, window_id: &str, expected: &[u8]) {
    assert_hex_echo_receives(h, window_id, EMITTED_MARKER, expected, RECEIVE_TIMEOUT).await;
}

fn key(named: NamedKey) -> ClientMessage {
    ClientMessage::Key(KeyInput {
        key: Key::Named(named),
        mods: Modifiers::default(),
    })
}

/// Sends `Ping` and expects `Pong` next: an input frame that drew an error would have
/// answered first.
async fn assert_no_error_before_pong(client: &mut Connection) {
    send(client, &ClientMessage::Ping).await;
    match read_response(client, Duration::from_secs(5)).await {
        DaemonMessage::Pong => {}
        other => panic!("expected the input to be taken silently, got {other:?}"),
    }
}

/// Sends a sentinel text after a refused input and asserts the child got the sentinel
/// but not `refused_hex`.
async fn assert_child_received_nothing_before_sentinel(
    h: &Harness,
    window_id: &str,
    client: &mut Connection,
    refused_hex: &str,
) {
    send(
        client,
        &ClientMessage::Text {
            text: "Z".to_string(),
        },
    )
    .await;
    assert_child_receives(h, window_id, b"Z").await;
    let received = output_after_marker(h, window_id, EMITTED_MARKER);
    assert!(
        !contains_subsequence(&received, refused_hex.as_bytes()),
        "a refused input reached the child: {:?}",
        String::from_utf8_lossy(&received)
    );
}

#[tokio::test]
async fn up_reaches_the_child_as_csi_while_cursor_keys_mode_is_reset() {
    let (h, window_id, mut client) = attached_window("si-up-normal", &[]).await;
    send(&mut client, &key(NamedKey::ArrowUp)).await;
    assert_child_receives(&h, &window_id, b"\x1b[A").await;
}

#[tokio::test]
async fn up_reaches_the_child_as_ss3_once_it_set_cursor_keys_mode() {
    let (h, window_id, mut client) = attached_window("si-up-app", &[DECCKM]).await;
    send(&mut client, &key(NamedKey::ArrowUp)).await;
    assert_child_receives(&h, &window_id, b"\x1bOA").await;
}

#[tokio::test]
async fn ctrl_c_reaches_the_child_as_etx() {
    let (h, window_id, mut client) = attached_window("si-ctrl-c", &[]).await;
    send(
        &mut client,
        &ClientMessage::Key(KeyInput {
            key: Key::Char('c'),
            mods: Modifiers {
                shift: false,
                alt: false,
                ctrl: true,
            },
        }),
    )
    .await;
    assert_child_receives(&h, &window_id, b"\x03").await;
}

#[tokio::test]
async fn committed_text_reaches_the_child_as_utf8() {
    let (h, window_id, mut client) = attached_window("si-text", &[]).await;
    let text = "h\u{e9}\u{65e5}";
    send(
        &mut client,
        &ClientMessage::Text {
            text: text.to_string(),
        },
    )
    .await;
    assert_child_receives(&h, &window_id, text.as_bytes()).await;
}

#[tokio::test]
async fn paste_reaches_the_child_with_line_breaks_as_cr() {
    let (h, window_id, mut client) = attached_window("si-paste-plain", &[]).await;
    send(
        &mut client,
        &ClientMessage::Paste {
            text: "ab\r\ncd\nef".to_string(),
        },
    )
    .await;
    assert_child_receives(&h, &window_id, b"ab\rcd\ref").await;
}

#[tokio::test]
async fn paste_reaches_the_child_bracketed_once_it_set_bracketed_paste() {
    let (h, window_id, mut client) =
        attached_window("si-paste-bracketed", &[BRACKETED_PASTE]).await;
    send(
        &mut client,
        &ClientMessage::Paste {
            text: "ab\ncd".to_string(),
        },
    )
    .await;
    assert_child_receives(&h, &window_id, b"\x1b[200~ab\rcd\x1b[201~").await;
}

#[tokio::test]
async fn a_bracketed_paste_cannot_end_the_bracket_early() {
    let (h, window_id, mut client) = attached_window("si-paste-marker", &[BRACKETED_PASTE]).await;
    send(
        &mut client,
        &ClientMessage::Paste {
            text: "x\x1b[201~y".to_string(),
        },
    )
    .await;
    assert_child_receives(&h, &window_id, b"\x1b[200~xy\x1b[201~").await;
}

#[tokio::test]
async fn a_press_reaches_the_child_as_an_sgr_report_once_it_tracks_the_mouse_in_sgr() {
    let (h, window_id, mut client) =
        attached_window("si-pointer-sgr", &[CLICK_TRACKING, SGR_MOUSE]).await;
    send(
        &mut client,
        &ClientMessage::Pointer(PointerInput {
            kind: PointerKind::Down,
            button: PointerButton::Left,
            cell: CellPosition { col: 2, row: 1 },
            mods: Modifiers::default(),
            clicks: 1,
        }),
    )
    .await;
    assert_child_receives(&h, &window_id, b"\x1b[<0;3;2M").await;
}

#[tokio::test]
async fn a_press_off_the_screen_writes_nothing_to_a_child_tracking_the_mouse() {
    let (h, window_id, mut client) =
        attached_window("si-pointer-off-screen", &[CLICK_TRACKING, SGR_MOUSE]).await;
    send(
        &mut client,
        &ClientMessage::Pointer(PointerInput {
            kind: PointerKind::Down,
            button: PointerButton::Left,
            cell: CellPosition {
                col: u16::MAX,
                row: 0,
            },
            mods: Modifiers::default(),
            clicks: 1,
        }),
    )
    .await;
    assert_child_received_nothing_before_sentinel(&h, &window_id, &mut client, "1b5b3c").await;
}

#[tokio::test]
async fn a_press_reaches_the_child_in_the_default_encoding_without_sgr() {
    let (h, window_id, mut client) = attached_window("si-pointer-default", &[CLICK_TRACKING]).await;
    send(
        &mut client,
        &ClientMessage::Pointer(PointerInput {
            kind: PointerKind::Down,
            button: PointerButton::Left,
            cell: CellPosition { col: 2, row: 1 },
            mods: Modifiers::default(),
            clicks: 1,
        }),
    )
    .await;
    assert_child_receives(&h, &window_id, b"\x1b[M #\"").await;
}

#[tokio::test]
async fn focus_reaches_the_child_once_it_asked_for_focus_reports() {
    let (h, window_id, mut client) = attached_window("si-focus", &[FOCUS_REPORTS]).await;
    send(&mut client, &ClientMessage::Focus { focused: true }).await;
    assert_child_receives(&h, &window_id, b"\x1b[I").await;
}

#[tokio::test]
async fn a_focus_report_reaches_the_child_without_counting_as_a_write() {
    let (h, window_id, mut client) = attached_window("si-focus-uncounted", &[FOCUS_REPORTS]).await;
    let entry = h.registry.get(&window_id).expect("window must exist");
    let before = entry.write_seq();
    send(&mut client, &ClientMessage::Focus { focused: false }).await;
    assert_child_receives(&h, &window_id, b"\x1b[O").await;
    assert_eq!(
        entry.write_seq(),
        before,
        "a focus report changes nothing in the composer, so a delivery waiting on the window \
         must not see it as another writer"
    );
}

fn pointer(kind: PointerKind, button: PointerButton) -> ClientMessage {
    ClientMessage::Pointer(PointerInput {
        kind,
        button,
        cell: CellPosition { col: 2, row: 1 },
        mods: Modifiers::default(),
        clicks: 1,
    })
}

#[tokio::test]
async fn a_move_with_no_button_held_reaches_the_child_without_counting_as_a_write() {
    let (h, window_id, mut client) =
        attached_window("si-hover-uncounted", &[MOTION_TRACKING, SGR_MOUSE]).await;
    let entry = h.registry.get(&window_id).expect("window must exist");
    let before = entry.write_seq();
    send(
        &mut client,
        &pointer(PointerKind::Move, PointerButton::None),
    )
    .await;
    assert_child_receives(&h, &window_id, b"\x1b[<35;3;2M").await;
    assert_eq!(
        entry.write_seq(),
        before,
        "a move with no button held cannot change the composer, so a delivery waiting on the \
         window must not see it as another writer"
    );
}

#[tokio::test]
async fn a_press_still_counts_as_a_write() {
    let (h, window_id, mut client) =
        attached_window("si-press-counted", &[MOTION_TRACKING, SGR_MOUSE]).await;
    let entry = h.registry.get(&window_id).expect("window must exist");
    let before = entry.write_seq();
    send(
        &mut client,
        &pointer(PointerKind::Down, PointerButton::Right),
    )
    .await;
    assert_child_receives(&h, &window_id, b"\x1b[<2;3;2M").await;
    assert!(
        entry.write_seq() > before,
        "a press can change the composer (a right press pastes in some programs), so it must \
         count as another writer"
    );
}

#[tokio::test]
async fn wheel_reaches_a_child_on_the_alternate_screen_as_arrow_keys() {
    let (h, window_id, mut client) =
        attached_window("si-wheel-alt", &[ALT_SCREEN, ALTERNATE_SCROLL]).await;
    send(
        &mut client,
        &ClientMessage::Wheel(WheelInput {
            delta: -2,
            cell: CellPosition { col: 0, row: 0 },
            mods: Modifiers::default(),
        }),
    )
    .await;
    assert_child_receives(&h, &window_id, b"\x1b[A\x1b[A").await;
}

#[tokio::test]
async fn wheel_off_the_screen_writes_nothing_to_a_child_on_the_alternate_screen() {
    let (h, window_id, mut client) =
        attached_window("si-wheel-off-screen", &[ALT_SCREEN, ALTERNATE_SCROLL]).await;
    send(
        &mut client,
        &ClientMessage::Wheel(WheelInput {
            delta: -2,
            cell: CellPosition {
                col: u16::MAX,
                row: 0,
            },
            mods: Modifiers::default(),
        }),
    )
    .await;
    assert_child_received_nothing_before_sentinel(&h, &window_id, &mut client, "1b5b").await;
}

#[tokio::test]
async fn structured_input_without_an_attachment_is_refused() {
    let h = Harness::start("si-not-attached", DaemonConfig::default()).await;
    let mut client = h.connect().await;
    for msg in [
        key(NamedKey::Enter),
        ClientMessage::Text {
            text: "a".to_string(),
        },
        ClientMessage::Paste {
            text: "a".to_string(),
        },
        ClientMessage::Focus { focused: true },
    ] {
        send(&mut client, &msg).await;
        expect_error(
            read_response(&mut client, Duration::from_secs(5)).await,
            "not_attached",
        );
    }
}

#[tokio::test]
async fn text_at_the_limit_is_taken() {
    let (_h, _window_id, mut client) = attached_window("si-text-limit", &[]).await;
    send(
        &mut client,
        &ClientMessage::Text {
            text: "q".repeat(MAX_TEXT_INPUT_BYTES),
        },
    )
    .await;
    assert_no_error_before_pong(&mut client).await;
}

#[tokio::test]
async fn text_over_the_limit_is_refused_and_writes_nothing() {
    let (h, window_id, mut client) = attached_window("si-text-over", &[]).await;
    send(
        &mut client,
        &ClientMessage::Text {
            text: "q".repeat(MAX_TEXT_INPUT_BYTES + 1),
        },
    )
    .await;
    expect_error(
        read_response(&mut client, Duration::from_secs(5)).await,
        "invalid_argument",
    );
    assert_child_received_nothing_before_sentinel(&h, &window_id, &mut client, "71717171").await;
}

#[tokio::test]
async fn paste_over_the_limit_is_refused_and_writes_nothing() {
    let (h, window_id, mut client) = attached_window("si-paste-over", &[]).await;
    send(
        &mut client,
        &ClientMessage::Paste {
            text: "q".repeat(MAX_PASTE_BYTES + 1),
        },
    )
    .await;
    expect_error(
        read_response(&mut client, Duration::from_secs(10)).await,
        "invalid_argument",
    );
    assert_child_received_nothing_before_sentinel(&h, &window_id, &mut client, "71717171").await;
}

#[tokio::test]
async fn wheel_over_the_line_limit_is_refused_and_writes_nothing() {
    let (h, window_id, mut client) =
        attached_window("si-wheel-over", &[ALT_SCREEN, ALTERNATE_SCROLL]).await;
    let over = i32::from(MAX_WHEEL_LINES) + 1;
    for delta in [over, -over] {
        send(
            &mut client,
            &ClientMessage::Wheel(WheelInput {
                delta,
                cell: CellPosition { col: 0, row: 0 },
                mods: Modifiers::default(),
            }),
        )
        .await;
        expect_error(
            read_response(&mut client, Duration::from_secs(5)).await,
            "invalid_argument",
        );
    }
    assert_child_received_nothing_before_sentinel(&h, &window_id, &mut client, "1b5b").await;
}
