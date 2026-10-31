#![cfg(windows)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: a key press the legacy form cannot carry reaches a console child
//! of a daemon-opened ConPTY window with its modifiers, as a console input record.
//!
//! The child is this test binary started again as `key_record_child`: it reads console
//! input records with `ReadConsoleInputW` and prints each key-down's virtual key and
//! control key state.

mod common;

use std::io::Write;
use std::time::Duration;

use common::{Harness, attach, send, wait_until_model_shows};
use yagura_daemon::DaemonConfig;
use yagura_protocol::{ClientMessage, Key, KeyInput, Modifiers, NamedKey};

const CHILD: &str = "key_record_child";
const READY: &str = "KEY-RECORDS-READY";
const TAB_RECORD: &str = "KEYDOWN-VK-0009-CTRL-";
const TIMEOUT: Duration = Duration::from_secs(15);
const LEFT_CTRL_PRESSED: u32 = 0x0008;

/// Runs `child` from this binary as a window's command.
fn child_command(child: &str) -> Vec<String> {
    let exe = std::env::current_exe().expect("current_exe");
    vec![
        exe.to_string_lossy().into_owned(),
        child.to_string(),
        "--exact".to_string(),
        "--ignored".to_string(),
        "--nocapture".to_string(),
        "--test-threads=1".to_string(),
    ]
}

/// Whether this process was started by [`child_command`] to be `child`.
fn running_as(child: &str) -> bool {
    let args: Vec<String> = std::env::args().collect();
    args.iter().any(|a| a == "--exact") && args.iter().any(|a| a == child)
}

#[test]
#[ignore = "the child of the test below, started by child_command"]
fn key_record_child() {
    use windows::Win32::System::Console::{
        GetStdHandle, INPUT_RECORD, KEY_EVENT, ReadConsoleInputW, STD_INPUT_HANDLE,
    };
    if !running_as(CHILD) {
        return;
    }
    // A child left waiting on input after a failed test must not outlive it for long.
    std::thread::spawn(|| {
        std::thread::sleep(Duration::from_secs(60));
        std::process::exit(2);
    });
    // SAFETY: GetStdHandle only reads this process's standard handle table.
    let input = unsafe { GetStdHandle(STD_INPUT_HANDLE) }.expect("console input handle");
    let mut out = std::io::stdout().lock();
    write!(out, "{READY}\r\n").unwrap();
    out.flush().unwrap();
    let mut records = [INPUT_RECORD::default(); 16];
    loop {
        let mut read = 0u32;
        // SAFETY: `input` is the console input handle and `records` is a valid buffer.
        unsafe { ReadConsoleInputW(input, &mut records, &mut read) }.expect("ReadConsoleInputW");
        for record in &records[..read as usize] {
            if u32::from(record.EventType) != KEY_EVENT {
                continue;
            }
            // SAFETY: EventType is KEY_EVENT, so the union holds a KEY_EVENT_RECORD.
            let key = unsafe { record.Event.KeyEvent };
            if !key.bKeyDown.as_bool() {
                continue;
            }
            write!(
                out,
                "KEYDOWN-VK-{:04X}-CTRL-{:08X}-END\r\n",
                key.wVirtualKeyCode, key.dwControlKeyState
            )
            .unwrap();
            out.flush().unwrap();
            if key.wVirtualKeyCode == 0x09 {
                std::process::exit(0);
            }
        }
    }
}

/// The control key state the child printed for its first Tab key-down.
fn tab_control_state(text: &str) -> Option<u32> {
    let start = text.find(TAB_RECORD)? + TAB_RECORD.len();
    u32::from_str_radix(text.get(start..start + 8)?, 16).ok()
}

#[tokio::test]
async fn ctrl_tab_reaches_a_console_child_as_vk_tab_with_left_ctrl() {
    let h = Harness::start("conpty-key-records", DaemonConfig::default()).await;
    let window_id = h.open(child_command(CHILD), None).await;
    h.wait_for_scrollback(&window_id, READY.as_bytes(), TIMEOUT)
        .await;
    wait_until_model_shows(&h, &window_id, READY.as_bytes(), TIMEOUT).await;

    // The PTY host asks for Win32 Input Mode before the child prints anything.
    let session = h
        .registry
        .get(&window_id)
        .expect("window must exist in registry")
        .session()
        .clone();
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    while !session.input_state().modes.win32_input {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the window's published input modes never carried the PTY host's DECSET 9001"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let mut client = h.connect().await;
    attach(&mut client, &window_id).await;
    send(
        &mut client,
        &ClientMessage::Key(KeyInput {
            key: Key::Named(NamedKey::Tab),
            mods: Modifiers {
                shift: false,
                alt: false,
                ctrl: true,
            },
        }),
    )
    .await;
    h.wait_for_scrollback(&window_id, TAB_RECORD.as_bytes(), TIMEOUT)
        .await;
    let text = h.flattened_scrollback(&window_id);
    let state = tab_control_state(&text)
        .unwrap_or_else(|| panic!("no control key state for the Tab key-down in {text:?}"));
    assert_ne!(
        state & LEFT_CTRL_PRESSED,
        0,
        "the child read Ctrl+Tab as a Tab without Ctrl (state {state:#010x})"
    );
}
