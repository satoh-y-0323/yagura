#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: the per-window notice queue and its single delivery
//! agent (NQ-*). Covers what no single notice kind's own suite can: the agent
//! retrying a notice the window's input declined, and a notice
//! queued before the IPC layer that delivers it exists. Every window is a
//! `submit_child` or `echo_child` fixture, never a real vendor CLI.

mod common;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use common::{
    ChatPlaybook, Harness, SubmitDefinitionInput, VALID_TOKEN, approver_base_config,
    call_open_window, chat_config, identify, open_submit_child_window_with_composer, policy,
    read_response, send, unique_pipe_name, unique_temp_dir, wait_until_status,
    write_submit_definition,
};

use yagura_daemon::window::ChatIntroOutcome;
use yagura_daemon::{DaemonConfig, IpcServer, OpenWindowRequest, WindowRegistry};
use yagura_protocol::{ClientMessage, DaemonMessage, ReportOutcome, WindowStatus};
use yagura_test_support::ipc::Connection;
use yagura_test_support::process::echo_child_command;

const NOTICE_TIMEOUT: Duration = Duration::from_secs(10);

const FOREIGN_DRAFT: &str = "NQFOREIGNDRAFT";

fn expected_report_notice(pending: usize) -> String {
    format!("[yagura report] {pending} pending report(s). Call wait_for_report to read them.")
}

// --- reading what reached a window's input ------------------------------------

fn flattened_scrollback(h: &Harness, window_id: &str) -> String {
    let raw = String::from_utf8_lossy(&h.scrollback(window_id)).into_owned();
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => match chars.next() {
                Some('[') => {
                    for c in chars.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&c) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    while let Some(c) = chars.next() {
                        if c == '\u{7}' {
                            break;
                        }
                        if c == '\u{1b}' && chars.peek() == Some(&'\\') {
                            chars.next();
                            break;
                        }
                    }
                }
                _ => {}
            },
            '\r' | '\n' => {}
            other => out.push(other),
        }
    }
    out
}

fn submitted_lines(h: &Harness, window_id: &str) -> usize {
    flattened_scrollback(h, window_id)
        .matches("SUBMITTED ")
        .count()
}

async fn wait_for_flattened(h: &Harness, window_id: &str, needle: &str, timeout: Duration) {
    let deadline = tokio::time::Instant::now() + timeout;
    while !flattened_scrollback(h, window_id).contains(needle) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {needle:?} in {window_id}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn pin_status(probe: &mut Connection, window_id: &str, status: WindowStatus) {
    send(
        probe,
        &ClientMessage::StatusEvent {
            source: "notice-queue-test".to_string(),
            status,
            window_id: Some(window_id.to_string()),
            token: VALID_TOKEN.to_string(),
        },
    )
    .await;
    wait_until_status(probe, window_id, status, Duration::from_secs(10)).await;
}

// --- A declined notice is retried without typing over the draft ---------

fn report_config(dir: &Path) -> DaemonConfig {
    DaemonConfig {
        detection_dir: Some(dir.to_path_buf()),
        approval_policy: Some(policy(&["open_window"])),
        ..approver_base_config()
    }
}

#[tokio::test]
async fn a_declined_notice_is_retried_and_lands_only_once_the_draft_is_cleared() {
    let dir = unique_temp_dir("nq5-retry-pace");
    let h = Harness::start("nq5-retry-pace", report_config(&dir)).await;
    let mut probe = h.connect().await;
    let parent = open_submit_child_window_with_composer(
        &h,
        &mut probe,
        &dir,
        "nq5-parent",
        "nq5-parent-ready",
        0,
        0,
        0,
        SubmitDefinitionInput {
            bracketed_paste: false,
            ready_hold_ms: 200,
            submit_delay_ms: 50,
            clear: "",
            delivery_confirm_ms: 0,
            resend_max: 0,
        },
        0,
    )
    .await;
    let mut parent_conn = h.connect().await;
    identify(&mut parent_conn, &parent).await;
    let child = call_open_window(&mut parent_conn, echo_child_command()).await;
    let mut child_conn = h.connect().await;
    identify(&mut child_conn, &child).await;
    pin_status(&mut probe, &parent, WindowStatus::WaitingInput).await;

    h.write_input(&parent, FOREIGN_DRAFT.as_bytes()).await;
    wait_for_flattened(&h, &parent, FOREIGN_DRAFT, NOTICE_TIMEOUT).await;

    send(
        &mut child_conn,
        &ClientMessage::SendReport {
            outcome: ReportOutcome::Succeeded,
            summary: "done".to_string(),
            path: None,
        },
    )
    .await;
    match read_response(&mut child_conn, Duration::from_secs(5)).await {
        DaemonMessage::ReportSent { .. } => {}
        other => panic!("expected report_sent, got {other:?}"),
    }

    let entry = h.registry.get(&parent).expect("parent window entry");
    let deadline = tokio::time::Instant::now() + NOTICE_TIMEOUT;
    while entry.notice_attempts() == 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the notice was never attempted"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // The window keeps accepting input and its status never changes, so only
    // the retry interval may trigger another attempt. Its spacing is measured
    // on a paused clock by `notice_queue`'s unit tests.
    let before = entry.notice_attempts();
    let deadline = tokio::time::Instant::now() + NOTICE_TIMEOUT;
    while entry.notice_attempts() == before {
        assert!(
            tokio::time::Instant::now() < deadline,
            "a declined notice must still be retried while the window keeps accepting input"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        submitted_lines(&h, &parent),
        0,
        "no notice may be typed over the foreign draft"
    );

    h.write_input(&parent, b"\x15").await;
    wait_for_flattened(
        &h,
        &parent,
        &format!("SUBMITTED {}", expected_report_notice(1)),
        NOTICE_TIMEOUT,
    )
    .await;
    h.registry.terminate_all().await;
}

// --- An intro queued before the IPC layer exists -------------------------

/// A daemon wired the way `main.rs` boots one: the intro hook, then the first
/// window, then the IPC server. The window is observed accepting input before
/// the server is built, so an intro that only the IPC layer's delivery agent
/// writes must still reach it once that server exists.
struct BootedDaemon {
    registry: Arc<WindowRegistry>,
    _server: tokio::task::JoinHandle<()>,
}

#[tokio::test]
async fn the_first_windows_intro_is_delivered_when_it_was_queued_before_the_ipc_server_existed() {
    let dir = unique_temp_dir("nq7-boot-order");
    let config = chat_config(
        &dir,
        &ChatPlaybook {
            intro: Some("welcome to the room"),
            ..ChatPlaybook::default()
        },
    );
    write_submit_definition(
        &dir,
        "nq7-first",
        "nq7-first-marker",
        SubmitDefinitionInput {
            bracketed_paste: false,
            ready_hold_ms: 0,
            submit_delay_ms: 50,
            clear: "",
            delivery_confirm_ms: 0,
            resend_max: 0,
        },
    );

    let registry = Arc::new(WindowRegistry::new(config.clone()));
    let chat = {
        let config = config.clone();
        Arc::new(
            tokio::task::spawn_blocking(move || {
                yagura_daemon::ipc::chat::ChatRuntime::from_config(&config)
            })
            .await
            .expect("ChatRuntime::from_config must not panic"),
        )
    };
    yagura_daemon::ipc::chat::install_intro_hook(&registry, chat.clone());

    let window_id = registry
        .open(OpenWindowRequest {
            command: yagura_test_support::process::submit_child_command_with_composer(
                "nq7-first-marker",
                0,
                0,
                0,
                0,
            ),
            cwd: None,
            name: None,
            vendor: None,
            detection: Some("nq7-first".to_string()),
        })
        .await
        .expect("the first window must open")
        .window_id;
    let entry = registry.get(&window_id).expect("first window entry");
    let deadline = tokio::time::Instant::now() + NOTICE_TIMEOUT;
    while entry.current_status() != WindowStatus::WaitingInput {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the first window never accepted input"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let ledger = Arc::new(
        tokio::task::spawn_blocking({
            let config = config.clone();
            move || yagura_daemon::ipc::plan::PlanRuntime::from_config(&config)
        })
        .await
        .expect("PlanRuntime::from_config must not panic"),
    );
    let server = Arc::new(
        IpcServer::new_with_chat(
            registry.clone(),
            unique_pipe_name("nq7-boot-order"),
            config.clone(),
            chat,
        )
        .with_ledger(ledger),
    );
    let daemon = BootedDaemon {
        registry,
        _server: tokio::spawn(async move {
            let _ = server.run().await;
        }),
    };

    let deadline = tokio::time::Instant::now() + NOTICE_TIMEOUT;
    while entry.chat_intro_outcome().is_none() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for the first window's intro"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(entry.chat_intro_outcome(), Some(ChatIntroOutcome::Sent));
    // `Sent` covers a submit key whose effect the fixture has yet to print.
    let deadline = tokio::time::Instant::now() + NOTICE_TIMEOUT;
    loop {
        let scrollback = entry
            .session()
            .text_tail(usize::MAX, 256 * 1024)
            .await
            .unwrap()
            .0;
        if scrollback.contains("SUBMITTED welcome to the room") {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the intro must reach the first window: {scrollback:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    daemon.registry.terminate_all().await;
}
