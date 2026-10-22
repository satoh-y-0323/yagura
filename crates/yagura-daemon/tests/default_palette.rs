#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! A window no view is attached to answers its program's color queries from
//! the default palettes the GUI last sent (`set_default_palettes`): the one
//! assigned to the window's vendor, else the app's; before any were sent, from
//! the built-in dark palette. The answer does not depend on whether a view
//! sent `set_palette` before the program asked.
//!
//! The child is `hex_echo --emit`: right after starting it writes the queries,
//! then a device status report as a sentinel (answered by the daemon in query
//! order), and prints every byte it reads as hex. No input is ever sent, so the
//! queries race the window's creation the way a real CLI's do.

mod common;

use std::time::Duration;

use common::{
    Harness, assert_hex_echo_receives, contains_subsequence, hex_echo_path, hex_of,
    output_after_marker, read_message, send,
};
use tokio::io::AsyncWriteExt;
use yagura_daemon::{DaemonConfig, OpenWindowRequest};
use yagura_protocol::{ClientMessage, DaemonMessage, Palette};

const EMITTED_MARKER: &[u8] = b"QUERIES-EMITTED";
const REPLY_TIMEOUT: Duration = Duration::from_secs(10);

const QUERY_BACKGROUND: &[u8] = b"\x1b]11;?\x1b\\";
const QUERY_FOREGROUND: &[u8] = b"\x1b]10;?\x1b\\";
const DEVICE_STATUS: &[u8] = b"\x1b[5n";
const DEVICE_STATUS_REPLY: &[u8] = b"\x1b[0n";

const ASSIGNED_VENDOR: &str = "palette-assigned";
const UNASSIGNED_VENDOR: &str = "palette-unassigned";

fn palette(foreground: [u8; 3], background: [u8; 3]) -> Palette {
    Palette {
        foreground,
        background,
        cursor: [0x12, 0x34, 0x56],
        ansi: (0..16u8).map(|i| [i * 16, 255 - i * 8, i]).collect(),
    }
}

fn app_palette() -> Palette {
    palette([0x31, 0x32, 0x33], [0xfa, 0xfb, 0xfc])
}

fn vendor_palette() -> Palette {
    palette([0x41, 0x42, 0x43], [0x0a, 0x0b, 0x2c])
}

/// An OSC color reply as xterm.js wrote it: 16-bit channels, ST terminator.
fn reply(slot: &str, rgb: [u8; 3]) -> Vec<u8> {
    format!(
        "\x1b]{slot};rgb:{0:02x}{0:02x}/{1:02x}{1:02x}/{2:02x}{2:02x}\x1b\\",
        rgb[0], rgb[1], rgb[2]
    )
    .into_bytes()
}

/// Sends the default palettes as the GUI does, on a connection with no view,
/// and returns once a ping sent after them came back: the daemon has handled
/// the notice before any window below is opened.
async fn send_default_palettes(h: &Harness, app: &Palette, vendors: &[(&str, &Palette)]) {
    let vendors: serde_json::Map<String, serde_json::Value> = vendors
        .iter()
        .map(|(vendor, palette)| {
            (
                (*vendor).to_string(),
                serde_json::to_value(palette).unwrap(),
            )
        })
        .collect();
    let body = serde_json::json!({
        "type": "set_default_palettes",
        "app": app,
        "vendors": vendors,
    });
    let mut client = h.connect().await;
    let frame = yagura_protocol::frame_encode(&serde_json::to_vec(&body).unwrap());
    client
        .write_all(&frame)
        .await
        .expect("write the default palettes frame");
    send(&mut client, &ClientMessage::Ping).await;
    loop {
        match read_message(&mut client, REPLY_TIMEOUT).await {
            DaemonMessage::Pong => return,
            DaemonMessage::Error { code, message, .. } => {
                panic!("the daemon refused the default palettes: {code}: {message}")
            }
            _ => continue,
        }
    }
}

/// Opens a `hex_echo --emit` window of `vendor` with no view attached and
/// waits for the reply to its status report, the last of its queries.
async fn window_that_asks_at_start(h: &Harness, vendor: Option<&str>) -> String {
    let argv = vec![
        hex_echo_path().to_str().unwrap().to_string(),
        "--emit".to_string(),
        hex_of(QUERY_BACKGROUND),
        hex_of(QUERY_FOREGROUND),
        hex_of(DEVICE_STATUS),
    ];
    let window_id = h
        .registry
        .open(OpenWindowRequest {
            command: argv,
            cwd: None,
            name: None,
            vendor: vendor.map(str::to_string),
            detection: None,
            on_name_taken: Default::default(),
        })
        .await
        .expect("registry.open should spawn the child")
        .window_id;
    assert_hex_echo_receives(
        h,
        &window_id,
        EMITTED_MARKER,
        DEVICE_STATUS_REPLY,
        REPLY_TIMEOUT,
    )
    .await;
    window_id
}

/// The replies arrive in query order, so once the status report's reply has
/// come, the color replies that belong before it have come too or never will.
fn assert_answered_with(h: &Harness, window_id: &str, palette: &Palette, what: &str) {
    let expected = [
        reply("11", palette.background),
        reply("10", palette.foreground),
        DEVICE_STATUS_REPLY.to_vec(),
    ]
    .concat();
    let received = output_after_marker(h, window_id, EMITTED_MARKER);
    assert!(
        contains_subsequence(&received, hex_of(&expected).as_bytes()),
        "{what}: expected the replies {:?} (hex {}); the child printed after the marker: {:?}",
        String::from_utf8_lossy(&expected),
        hex_of(&expected),
        String::from_utf8_lossy(&received)
    );
}

#[tokio::test]
async fn a_window_with_no_view_answers_from_its_vendors_default_palette() {
    let h = Harness::start("default-palette-vendor", DaemonConfig::default()).await;
    let vendor = vendor_palette();
    send_default_palettes(&h, &app_palette(), &[(ASSIGNED_VENDOR, &vendor)]).await;

    let window_id = window_that_asks_at_start(&h, Some(ASSIGNED_VENDOR)).await;

    assert_answered_with(
        &h,
        &window_id,
        &vendor,
        "a window of the assigned vendor must answer with that vendor's palette",
    );
}

#[tokio::test]
async fn a_window_whose_vendor_has_no_assignment_answers_from_the_app_palette() {
    let h = Harness::start("default-palette-app", DaemonConfig::default()).await;
    let app = app_palette();
    send_default_palettes(&h, &app, &[(ASSIGNED_VENDOR, &vendor_palette())]).await;

    let unassigned = window_that_asks_at_start(&h, Some(UNASSIGNED_VENDOR)).await;
    let without_vendor = window_that_asks_at_start(&h, None).await;

    assert_answered_with(
        &h,
        &unassigned,
        &app,
        "a window of a vendor with no assignment must answer with the app palette",
    );
    assert_answered_with(
        &h,
        &without_vendor,
        &app,
        "a window with no vendor must answer with the app palette",
    );
}

#[tokio::test]
async fn before_any_default_palettes_were_sent_a_window_answers_from_the_built_in_dark_palette() {
    let h = Harness::start("default-palette-built-in", DaemonConfig::default()).await;
    let built_in = yagura_protocol::built_in_dark_palette();
    assert_ne!(
        built_in.background,
        app_palette().background,
        "the fixture's app palette must differ from the built-in one"
    );

    let window_id = window_that_asks_at_start(&h, Some(ASSIGNED_VENDOR)).await;

    assert_answered_with(
        &h,
        &window_id,
        &built_in,
        "with no default palettes sent, a window must answer with the built-in dark palette",
    );
}
