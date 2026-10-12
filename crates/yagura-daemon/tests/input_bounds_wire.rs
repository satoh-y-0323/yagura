#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The daemon refuses structured input past its bounds with the reason
//! `yagura_protocol::check_input_bounds` gives, so a sender that checks first (the GUI's
//! core) and the daemon say the same thing. A paste may be 1 MiB.

mod common;

use std::time::Duration;

use common::{Harness, attach, expect_error, hex_echo_path, read_response, send};
use yagura_daemon::DaemonConfig;
use yagura_protocol::{ClientMessage, check_input_bounds};

const ONE_MEBIBYTE: usize = 1024 * 1024;

async fn attached_connection(tag: &str) -> (Harness, yagura_test_support::ipc::Connection) {
    let h = Harness::start(tag, DaemonConfig::default()).await;
    let window_id = h
        .open(
            vec![
                hex_echo_path()
                    .to_str()
                    .expect("hex_echo path must be valid UTF-8")
                    .to_string(),
            ],
            None,
        )
        .await;
    let mut client = h.connect().await;
    attach(&mut client, &window_id).await;
    (h, client)
}

#[tokio::test]
async fn a_paste_past_one_mebibyte_is_refused_with_the_protocols_reason() {
    let (_h, mut client) = attached_connection("bounds-wire-paste").await;
    let paste = ClientMessage::Paste {
        text: "q".repeat(ONE_MEBIBYTE + 1),
    };
    let reason = check_input_bounds(&paste).expect_err("the protocol refuses this paste");

    send(&mut client, &paste).await;
    send(&mut client, &ClientMessage::Ping).await;
    let message = expect_error(
        read_response(&mut client, Duration::from_secs(10)).await,
        "invalid_argument",
    );
    assert_eq!(message, reason);
}

#[tokio::test]
async fn text_and_wheel_past_their_bounds_are_refused_with_the_protocols_reasons() {
    let (_h, mut client) = attached_connection("bounds-wire-others").await;
    let over = [
        ClientMessage::Text {
            text: "q".repeat(yagura_protocol::MAX_TEXT_INPUT_BYTES + 1),
        },
        ClientMessage::Wheel(yagura_protocol::WheelInput {
            delta: i32::from(yagura_protocol::MAX_WHEEL_LINES) + 1,
            cell: yagura_protocol::CellPosition { col: 0, row: 0 },
            mods: yagura_protocol::Modifiers::default(),
        }),
    ];
    for msg in over {
        let reason = check_input_bounds(&msg).expect_err("the protocol refuses this input");
        send(&mut client, &msg).await;
        let message = expect_error(
            read_response(&mut client, Duration::from_secs(10)).await,
            "invalid_argument",
        );
        assert_eq!(message, reason);
    }
}
