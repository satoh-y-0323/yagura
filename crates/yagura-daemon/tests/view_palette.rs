#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! A program asking for the terminal's colors (OSC 10, 11, 4) is answered by
//! the daemon from the palette the attached GUI sent (`set_palette`) and from
//! what the program itself set over it; before any palette was sent the
//! daemon does not answer.
//!
//! The child is `hex_echo --emit-on-input`: on its first input byte it writes
//! the queries, then a device status report as a sentinel (answered by the
//! daemon in query order), and prints every byte it reads as hex. A reply
//! missing before the sentinel's reply therefore proves it was never sent.

mod common;

use std::time::Duration;

use common::{
    Harness, assert_hex_echo_receives, contains_subsequence, hex_echo_path, hex_of,
    output_after_marker,
};
use yagura_daemon::DaemonConfig;
use yagura_protocol::{ClientMessage, Palette, TerminalSize};
use yagura_test_support::view::ViewClient;

const EMITTED_MARKER: &[u8] = b"QUERIES-EMITTED";
const REPLY_TIMEOUT: Duration = Duration::from_secs(10);
const SIZE: TerminalSize = TerminalSize { cols: 80, rows: 24 };

const QUERY_BACKGROUND: &[u8] = b"\x1b]11;?\x1b\\";
const QUERY_FOREGROUND: &[u8] = b"\x1b]10;?\x1b\\";
const QUERY_ANSI_1: &[u8] = b"\x1b]4;1;?\x1b\\";
const QUERY_CURSOR: &[u8] = b"\x1b]12;?\x1b\\";
const SET_BACKGROUND_RED: &[u8] = b"\x1b]11;rgb:ff/00/00\x1b\\";
const RESET_BACKGROUND: &[u8] = b"\x1b]111\x1b\\";
const DEVICE_STATUS: &[u8] = b"\x1b[5n";
const DEVICE_STATUS_REPLY: &[u8] = b"\x1b[0n";

fn palette() -> Palette {
    Palette {
        foreground: [0xc0, 0xc1, 0xc2],
        background: [0x1e, 0x1f, 0x2e],
        cursor: [0xff, 0xee, 0xdd],
        ansi: (0..16u8).map(|i| [i * 16, 255 - i * 8, i]).collect(),
    }
}

/// An OSC color reply as xterm.js wrote it: 16-bit channels, ST terminator.
fn reply(slot: &str, rgb: [u8; 3]) -> Vec<u8> {
    format!(
        "\x1b]{slot};rgb:{0:02x}{0:02x}/{1:02x}{1:02x}/{2:02x}{2:02x}\x1b\\",
        rgb[0], rgb[1], rgb[2]
    )
    .into_bytes()
}

/// A `hex_echo` window that emits `queries` at its first input byte, with a
/// view attached to it (and the palette sent when `with_palette`).
async fn window_that_asks(
    tag: &str,
    queries: &[&[u8]],
    with_palette: bool,
) -> (Harness, String, ViewClient) {
    let h = Harness::start(tag, DaemonConfig::default()).await;
    let mut argv = vec![
        hex_echo_path().to_str().unwrap().to_string(),
        "--emit-on-input".to_string(),
    ];
    argv.extend(queries.iter().map(|query| hex_of(query)));
    let window_id = h.open(argv, None).await;
    h.wait_for_scrollback(&window_id, b"CTRL-READY", REPLY_TIMEOUT)
        .await;
    let mut view = ViewClient::attach(h.connect().await, &window_id, Some(SIZE)).await;
    if with_palette {
        view.send(&ClientMessage::SetPalette(palette())).await;
        view.sync().await;
    }
    view.send_text("a").await;
    (h, window_id, view)
}

#[tokio::test]
async fn a_color_query_is_not_answered_before_the_gui_sent_a_palette() {
    let (h, window_id, _view) = window_that_asks(
        "palette-none",
        &[QUERY_BACKGROUND, QUERY_FOREGROUND, DEVICE_STATUS],
        false,
    )
    .await;

    assert_hex_echo_receives(
        &h,
        &window_id,
        EMITTED_MARKER,
        DEVICE_STATUS_REPLY,
        REPLY_TIMEOUT,
    )
    .await;
    let received = output_after_marker(&h, &window_id, EMITTED_MARKER);
    assert!(
        !contains_subsequence(&received, hex_of(b"\x1b]1").as_bytes()),
        "a color reply came before the status report's: {:?}",
        String::from_utf8_lossy(&received)
    );
}

#[tokio::test]
async fn color_queries_are_answered_from_the_palette_the_gui_sent() {
    let palette = palette();
    let (h, window_id, _view) = window_that_asks(
        "palette-set",
        &[
            QUERY_BACKGROUND,
            QUERY_FOREGROUND,
            QUERY_CURSOR,
            QUERY_ANSI_1,
            b"\x1b]4;16;?\x1b\\",
            b"\x1b]4;17;?\x1b\\",
            b"\x1b]4;22;?\x1b\\",
            b"\x1b]4;232;?\x1b\\",
            b"\x1b]4;233;?\x1b\\",
            DEVICE_STATUS,
        ],
        true,
    )
    .await;

    // Past the client's own entries, xterm's color cube and gray ramp.
    let expected = [
        reply("11", palette.background),
        reply("10", palette.foreground),
        reply("12", palette.cursor),
        reply("4;1", palette.ansi[1]),
        reply("4;16", [0x00, 0x00, 0x00]),
        reply("4;17", [0x00, 0x00, 0x5f]),
        reply("4;22", [0x00, 0x5f, 0x00]),
        reply("4;232", [0x08, 0x08, 0x08]),
        reply("4;233", [0x12, 0x12, 0x12]),
        DEVICE_STATUS_REPLY.to_vec(),
    ]
    .concat();
    assert_hex_echo_receives(&h, &window_id, EMITTED_MARKER, &expected, REPLY_TIMEOUT).await;
}

#[tokio::test]
async fn a_color_the_program_set_is_answered_in_place_of_the_palette_until_it_resets_it() {
    let palette = palette();
    let (h, window_id, _view) = window_that_asks(
        "palette-override",
        &[
            SET_BACKGROUND_RED,
            QUERY_BACKGROUND,
            RESET_BACKGROUND,
            QUERY_BACKGROUND,
            DEVICE_STATUS,
        ],
        true,
    )
    .await;

    let expected = [
        reply("11", [255, 0, 0]),
        reply("11", palette.background),
        DEVICE_STATUS_REPLY.to_vec(),
    ]
    .concat();
    assert_hex_echo_receives(&h, &window_id, EMITTED_MARKER, &expected, REPLY_TIMEOUT).await;
}
