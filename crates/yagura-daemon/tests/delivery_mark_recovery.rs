#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! A delivery cut short by a killed daemon leaves no text of yagura's in the window's
//! composer: the delivery's body is kept as a mark from its start to its end, and the next
//! daemon of the instance, once it has taken the window back, takes that body back out of
//! the composer before it types any other delivery into it. A window with no mark keeps
//! the draft typed into it, and a composer holding more than the body is left as it is,
//! the way a take-back declines when the daemon's own text does not stand alone.
//!
//! Each case runs real daemon processes one after the other in one sandbox instance,
//! against `submit_child` windows that draw a composer and log every byte they read.

use crate::common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use common::real_daemon::{RunningDaemon, Sandbox};
use common::store_fixtures::store_holds;
use common::{
    CLEAR_CTRL_U, PASTE_END, SubmitDefinitionInput, line_ends_outside_paste, read_response, send,
    wait_until_input_log_contains, wait_until_status, write_profile, write_submit_definition,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use yagura_os::{IpcTransport, PeerAuth, PlatformIpc};
use yagura_protocol::holder::{
    HOLDER_PROTOCOL, HolderFrame, HolderFrameBuffer, encode_holder_frame, read_holder_record,
};
use yagura_protocol::{ClientMessage, DaemonMessage, WindowStatus};
use yagura_test_support::HANG_GUARD;
use yagura_test_support::ipc::{Connection, connect_with_retry};
use yagura_test_support::process::{
    COMPOSER_CONTINUATION_PREFIX, COMPOSER_MARKER, COMPOSER_SEPARATOR, submit_child_path,
};

/// The detection definition every window of these cases runs under.
const DEFINITION: &str = "delivery-mark";
/// The line `submit_child` prints at startup and after a paste: `waiting_input`.
const READY: &str = "DMARK-READY";
const PASTE_START: &[u8] = b"\x1b[200~";
/// The probe the definition names, typed behind a body: [`common::SUBMIT_CHILD_PROBE`].
const PROBE: u8 = b'x';
const CLEAR: u8 = 0x15;
const DEL: u8 = 0x7f;
const BS: u8 = 0x08;

fn utf8(path: &Path) -> &str {
    path.to_str().expect("a sandbox path must be UTF-8")
}

fn write_definition(dir: &Path) {
    write_submit_definition(
        dir,
        DEFINITION,
        READY,
        SubmitDefinitionInput {
            bracketed_paste: true,
            ready_hold_ms: 0,
            submit_delay_ms: 120,
            clear: CLEAR_CTRL_U,
            delivery_confirm_ms: 2000,
            resend_max: 0,
        },
    );
}

/// A profile whose `submit_child` draws its composer, reflecting writes as `reflect`
/// says (`0`, or `pause|<path>` to hold every repaint while the file exists), appends
/// every byte it reads to `log`, and takes lines as `receive_mode` says (`-` for none).
fn write_window_profile(
    sandbox: &Sandbox,
    id: &str,
    log: &Path,
    reflect: &str,
    receive_mode: &str,
) {
    let child = submit_child_path();
    let argv = [
        utf8(&child),
        READY,
        "0",
        "0",
        "0",
        "now",
        COMPOSER_MARKER,
        COMPOSER_SEPARATOR,
        COMPOSER_CONTINUATION_PREFIX,
        reflect,
        "-",
        "-",
        "-",
        "-",
        utf8(log),
        "-",
        receive_mode,
    ];
    let argv: Vec<String> = argv.iter().map(|arg| format!("'{arg}'")).collect();
    write_profile(
        &sandbox.profiles_dir,
        id,
        &format!(
            "schema = 1\ndisplay_name = \"{id}\"\ndetection = \"{DEFINITION}\"\n\n[command]\n\
             default = [{}]\n",
            argv.join(", ")
        ),
    );
}

/// The environment every daemon of a case starts with: the definitions in `detections`
/// and a playbook that lets `send_message` through without a human.
struct World {
    detections: PathBuf,
    playbook: PathBuf,
}

impl World {
    fn new(sandbox: &Sandbox) -> Self {
        let detections = sandbox.tree.make_dir("delivery-mark-detections");
        write_definition(&detections);
        let playbook = sandbox
            .tree
            .make_dir("delivery-mark-playbook")
            .join("playbook.toml");
        std::fs::write(
            &playbook,
            "[approval]\nauto_approve = [\"open_window\", \"close_window\", \"send_message\"]\n",
        )
        .unwrap();
        Self {
            detections,
            playbook,
        }
    }

    async fn start(&self, sandbox: &Sandbox) -> RunningDaemon {
        sandbox
            .start_from(&[
                ("YAGURA_DETECTION_DIR", utf8(&self.detections)),
                ("YAGURA_PLAYBOOK", utf8(&self.playbook)),
            ])
            .await
    }
}

async fn connect(sandbox: &Sandbox) -> Connection {
    let user = PlatformIpc::default()
        .current_user_id()
        .expect("the current user id must resolve");
    connect_with_retry(&yagura_os::default_endpoint(&user, &sandbox.namespace())).await
}

/// A connection attached to `window_id`, through which text is typed as a person types.
async fn viewer(sandbox: &Sandbox, window_id: &str) -> Connection {
    let mut client = connect(sandbox).await;
    send(
        &mut client,
        &ClientMessage::AttachView {
            window_id: window_id.to_owned(),
            size: None,
        },
    )
    .await;
    match read_response(&mut client).await {
        DaemonMessage::Attached { .. } => client,
        other => panic!("expected attached, got {other:?}"),
    }
}

async fn type_text(viewer: &mut Connection, text: &str) {
    send(
        viewer,
        &ClientMessage::Text {
            text: text.to_owned(),
        },
    )
    .await;
}

/// Sends `text` to `window_id` on a connection of its own, whose answer is never read.
async fn send_text_from(sandbox: &Sandbox, window_id: &str, text: &str) -> Connection {
    let mut requester = connect(sandbox).await;
    send(
        &mut requester,
        &ClientMessage::SendText {
            window_id: window_id.to_owned(),
            text: text.to_owned(),
        },
    )
    .await;
    requester
}

fn position(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn count(haystack: &[u8], needle: &[u8]) -> usize {
    haystack
        .windows(needle.len())
        .filter(|window| *window == needle)
        .count()
}

/// The start of `body`'s paste: a judged line is pasted with a space behind it.
fn paste_of(body: &str) -> Vec<u8> {
    [PASTE_START, body.as_bytes()].concat()
}

/// Where in `read` the bytes after the probe typed behind `body`'s paste begin.
fn after_the_probe(read: &[u8], body: &str) -> Option<usize> {
    let start = position(read, &paste_of(body))?;
    let end = start + position(&read[start..], PASTE_END)? + PASTE_END.len();
    let probe = read[end..].iter().position(|byte| *byte == PROBE)?;
    Some(end + probe + 1)
}

/// Waits until the child has read `body`'s paste and the probe typed behind it: the
/// delivery is past its body and waits for that probe to show.
async fn wait_until_probe_read(log: &Path, body: &str) {
    let deadline = tokio::time::Instant::now() + HANG_GUARD;
    loop {
        let read = std::fs::read(log).unwrap_or_default();
        if after_the_probe(&read, body).is_some() {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the child never read the paste of {body:?} and a probe behind it; it read {:?}",
            String::from_utf8_lossy(&read)
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Everything the child read before `sentinel`, typed through `viewer` behind whatever
/// was written to the window before this call.
async fn read_before(viewer: &mut Connection, log: &Path, sentinel: &str) -> Vec<u8> {
    type_text(viewer, sentinel).await;
    let mut read = wait_until_input_log_contains(log, sentinel.as_bytes()).await;
    let at = position(&read, sentinel.as_bytes()).expect("the log holds the sentinel");
    read.truncate(at);
    read
}

/// The text the composer region of `screen` holds, its wrapped lines joined: empty when
/// it shows the marker alone.
fn composer_of(screen: &str) -> Option<String> {
    let lines: Vec<&str> = screen.lines().collect();
    let start = lines
        .iter()
        .rposition(|line| line.trim_start().starts_with(COMPOSER_MARKER))?;
    let first = lines[start].trim_start()[COMPOSER_MARKER.len()..].trim_end();
    let mut text = first
        .strip_prefix(COMPOSER_SEPARATOR)
        .unwrap_or(first)
        .to_owned();
    for line in &lines[start + 1..] {
        let Some(rest) = line.strip_prefix(COMPOSER_CONTINUATION_PREFIX) else {
            break;
        };
        text.push_str(rest.trim_end());
    }
    Some(text)
}

/// The lines `submit_child` printed for the lines it took, without the prefix.
fn submitted(screen: &str) -> Vec<String> {
    screen
        .lines()
        .filter_map(|line| line.trim().strip_prefix("SUBMITTED"))
        .map(|body| body.trim().to_owned())
        .collect()
}

fn store_db(sandbox: &Sandbox) -> PathBuf {
    sandbox.data_root().join("yagura.db")
}

/// Waits until the session store holds `needle` (`kept`) or no longer does.
async fn until_store(sandbox: &Sandbox, needle: &str, kept: bool, what: &str) {
    let db = store_db(sandbox);
    let deadline = tokio::time::Instant::now() + HANG_GUARD;
    while store_holds(&db, needle) != kept {
        assert!(
            tokio::time::Instant::now() < deadline,
            "{what}: the session store {} {needle:?}; daemon log:\n{}",
            if kept { "never held" } else { "still holds" },
            sandbox.daemon_log()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// A body is the user's text: the daemon's log carries none of `bodies`.
fn assert_body_kept_out_of_the_log(sandbox: &Sandbox, bodies: &[&str]) {
    let log = sandbox.daemon_log();
    for body in bodies {
        assert!(
            !log.contains(body),
            "the daemon log must not carry the text {body:?}:
{log}"
        );
    }
}

/// A delivery whose daemon is killed once its body is in the composer leaves a mark;
/// the next daemon takes the body (and the probe behind it) back out of the composer
/// before it types the next delivery, which the window then takes once and alone.
#[tokio::test]
async fn a_delivery_cut_by_a_killed_daemon_is_taken_back_before_the_next_one_is_typed() {
    const BODY: &str = "CUT-BODY-3R7";
    const NEXT: &str = "NEXT-BODY-6M2";
    let sandbox = Sandbox::new("delivery-mark-cut");
    let world = World::new(&sandbox);
    let log = sandbox.tree.path().join("cut-input.log");
    let probe_may_be_read = sandbox.tree.path().join("cut-probe-may-be-read");
    write_window_profile(
        &sandbox,
        "mark-cut",
        &log,
        "0",
        &format!("hold-probe|{}", utf8(&probe_may_be_read)),
    );

    let mut first = world.start(&sandbox).await;
    let window_id = first.open_profile_window(&sandbox, "mark-cut").await;
    wait_until_status(&mut first.client, &window_id, WindowStatus::WaitingInput).await;
    let _requester = send_text_from(&sandbox, &window_id, &format!("{BODY}\r")).await;
    // The child draws the body and holds the probe behind it unread, so the delivery
    // waits for that probe for as long as the daemon lives.
    wait_until_probe_read(&log, BODY).await;
    until_store(
        &sandbox,
        BODY,
        true,
        "the delivery under way must have its body kept as a mark",
    )
    .await;
    first.kill_keeping_windows();
    std::fs::write(&probe_may_be_read, b"").unwrap();

    let mut next = world.start(&sandbox).await;
    wait_until_status(&mut next.client, &window_id, WindowStatus::WaitingInput).await;
    send(
        &mut next.client,
        &ClientMessage::SendText {
            window_id: window_id.clone(),
            text: format!("{NEXT}\r"),
        },
    )
    .await;
    match read_response(&mut next.client).await {
        DaemonMessage::TextSent { window_id: sent } => assert_eq!(sent, window_id),
        other => panic!(
            "the delivery after the restart must be typed, got {other:?}; daemon log:\n{}",
            sandbox.daemon_log()
        ),
    }
    let mut typing = viewer(&sandbox, &window_id).await;
    let read = read_before(&mut typing, &log, "SENT-CUT.").await;
    let screen = next
        .output_until(&window_id, "a line taken by the window", |text| {
            !submitted(text).is_empty()
        })
        .await;
    assert_eq!(
        submitted(&screen),
        [NEXT],
        "the window must take the next delivery once, alone, with nothing of the body cut \
         by the kill in front of it; screen:\n{screen}\ndaemon log:\n{}",
        sandbox.daemon_log()
    );
    assert_eq!(
        count(&read, &paste_of(NEXT)),
        1,
        "the next delivery's body must be written once; the child read {:?}",
        String::from_utf8_lossy(&read)
    );
    assert_eq!(
        line_ends_outside_paste(&read),
        1,
        "only the next delivery may be submitted; the child read {:?}",
        String::from_utf8_lossy(&read)
    );
    until_store(
        &sandbox,
        BODY,
        false,
        "the mark of the delivery taken back must be gone",
    )
    .await;
    until_store(
        &sandbox,
        NEXT,
        false,
        "the mark of a delivery that ended must be gone",
    )
    .await;
    assert_body_kept_out_of_the_log(&sandbox, &[BODY, NEXT]);
}

async fn send_holder_frame<C: AsyncWrite + Unpin>(connection: &mut C, frame: HolderFrame) {
    connection
        .write_all(&encode_holder_frame(&frame))
        .await
        .expect("write a frame to the holder");
}

async fn next_holder_frame<C: AsyncRead + Unpin>(
    connection: &mut C,
    frames: &mut HolderFrameBuffer,
) -> HolderFrame {
    let mut chunk = vec![0u8; 64 * 1024];
    loop {
        if let Some(frame) = frames.next_frame().expect("a frame from the holder") {
            return frame;
        }
        let read = tokio::time::timeout(HANG_GUARD, connection.read(&mut chunk))
            .await
            .expect("a frame from the holder within HANG_GUARD")
            .expect("read from the holder");
        assert!(read > 0, "the holder closed the connection");
        frames.push(&chunk[..read]);
    }
}

/// Types `text` into `window_id` through its holder while no daemon of the instance runs,
/// and waits until the holder's screen shows a composer `shown` accepts. This process
/// serves the instance's daemon endpoint meanwhile, as a daemon does, since the holder
/// lets in only the process serving it; both ends are closed before this returns.
async fn type_while_no_daemon(
    sandbox: &Sandbox,
    window_id: &str,
    text: &str,
    shown: impl Fn(&str) -> bool,
) {
    let ipc = PlatformIpc::default();
    let user = ipc
        .current_user_id()
        .expect("the current user id must resolve");
    let daemon_endpoint = yagura_os::default_endpoint(&user, &sandbox.namespace());
    let mut listener = ipc
        .bind(&daemon_endpoint, ipc.endpoint_security().unwrap())
        .await
        .expect("serve the instance's daemon endpoint while no daemon runs");
    let serving = tokio::spawn(async move {
        let ipc = PlatformIpc::default();
        // Kept open: the holder asks the system who serves the other end.
        let mut held = Vec::new();
        loop {
            match ipc.accept(&mut listener).await {
                Ok(connection) => held.push(connection),
                Err(_) => tokio::time::sleep(Duration::from_millis(20)).await,
            }
        }
    });

    let record = read_holder_record(&sandbox.holders_dir(), window_id)
        .expect("the window's holder keeps its record");
    let mut connection = ipc
        .connect(&record.endpoint)
        .await
        .expect("connect to the window's holder");
    let mut frames = HolderFrameBuffer::new();
    send_holder_frame(
        &mut connection,
        HolderFrame::Hello {
            protocol: HOLDER_PROTOCOL,
            daemon_boot: "delivery-mark-stand-in".to_owned(),
        },
    )
    .await;
    loop {
        match next_holder_frame(&mut connection, &mut frames).await {
            HolderFrame::Welcome { .. } => break,
            HolderFrame::ScreenRows(_) | HolderFrame::ScreenPush(_) => {}
            other => panic!("the holder must welcome the stand-in, got {other:?}"),
        }
    }
    send_holder_frame(
        &mut connection,
        HolderFrame::Write {
            id: 1,
            bytes: text.as_bytes().to_vec(),
        },
    )
    .await;
    let deadline = tokio::time::Instant::now() + HANG_GUARD;
    let mut written = false;
    for id in 2.. {
        send_holder_frame(
            &mut connection,
            HolderFrame::ReadTail {
                id,
                max_lines: 50,
                max_bytes: 64 * 1024,
            },
        )
        .await;
        let tail = loop {
            match next_holder_frame(&mut connection, &mut frames).await {
                HolderFrame::Written { id: 1 } => written = true,
                HolderFrame::WriteFailed { reason, .. } => {
                    panic!("the holder did not write the stand-in's text: {reason}")
                }
                HolderFrame::Tail {
                    id: answered, text, ..
                } if answered == id => break text,
                _ => {}
            }
        };
        if written && composer_of(&tail).is_some_and(|composer| shown(&composer)) {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the holder's screen never showed the expected composer; it showed:\n{tail}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    drop(connection);
    serving.abort();
    let _ = serving.await;
}

/// The next daemon's start leaves alone a draft typed into a window that had no
/// delivery under way, and a composer in which the body of a cut delivery has more text
/// behind it: that body does not stand alone, so it is not taken back, and its mark goes.
#[tokio::test]
async fn the_start_leaves_a_draft_alone_and_a_body_that_does_not_stand_alone() {
    const DRAFT: &str = "DRAFT-ALONE-7K1";
    const BODY: &str = "BESIDE-BODY-9W5";
    const APPENDED: &str = "APPENDED-4P8";
    const SENT_ALONE: &str = "SENT-ALONE.";
    const SENT_BESIDE: &str = "SENT-BESIDE.";
    let sandbox = Sandbox::new("delivery-mark-keep");
    let world = World::new(&sandbox);
    let alone_log = sandbox.tree.path().join("alone-input.log");
    let beside_log = sandbox.tree.path().join("beside-input.log");
    let probe_may_be_read = sandbox.tree.path().join("beside-probe-may-be-read");
    write_window_profile(&sandbox, "mark-alone", &alone_log, "0", "-");
    write_window_profile(
        &sandbox,
        "mark-beside",
        &beside_log,
        "0",
        &format!("hold-probe|{}", utf8(&probe_may_be_read)),
    );

    let mut first = world.start(&sandbox).await;
    let alone = first.open_profile_window(&sandbox, "mark-alone").await;
    let beside = first.open_profile_window(&sandbox, "mark-beside").await;
    wait_until_status(&mut first.client, &alone, WindowStatus::WaitingInput).await;
    wait_until_status(&mut first.client, &beside, WindowStatus::WaitingInput).await;

    let mut typing_alone = viewer(&sandbox, &alone).await;
    type_text(&mut typing_alone, DRAFT).await;
    first
        .output_until(&alone, "the draft in the composer", |text| {
            composer_of(text).as_deref() == Some(DRAFT)
        })
        .await;

    let _requester = send_text_from(&sandbox, &beside, &format!("{BODY}\r")).await;
    wait_until_probe_read(&beside_log, BODY).await;
    until_store(
        &sandbox,
        BODY,
        true,
        "the delivery under way must have its body kept as a mark",
    )
    .await;
    first.kill_keeping_windows();
    drop(typing_alone);
    std::fs::write(&probe_may_be_read, b"").unwrap();
    // Text no daemon saw stands behind the body and its probe when the next one starts.
    type_while_no_daemon(&sandbox, &beside, APPENDED, |composer| {
        composer.starts_with(BODY) && composer.ends_with(APPENDED)
    })
    .await;

    let mut next = world.start(&sandbox).await;
    wait_until_status(&mut next.client, &alone, WindowStatus::WaitingInput).await;
    wait_until_status(&mut next.client, &beside, WindowStatus::WaitingInput).await;
    until_store(
        &sandbox,
        BODY,
        false,
        "the mark of a body that does not stand alone must go once it is declined",
    )
    .await;

    let mut typing_alone = viewer(&sandbox, &alone).await;
    let alone_read = read_before(&mut typing_alone, &alone_log, SENT_ALONE).await;
    let mut typing_beside = viewer(&sandbox, &beside).await;
    let beside_read = read_before(&mut typing_beside, &beside_log, SENT_BESIDE).await;

    let alone_screen = next
        .output_until(&alone, "the sentinel in the composer", |text| {
            composer_of(text).is_some_and(|composer| composer.ends_with(SENT_ALONE))
        })
        .await;
    assert_eq!(
        composer_of(&alone_screen).as_deref(),
        Some(format!("{DRAFT}{SENT_ALONE}").as_str()),
        "a window with no delivery under way must keep its draft; screen:\n{alone_screen}"
    );
    let after_draft =
        position(&alone_read, DRAFT.as_bytes()).expect("the child read the draft") + DRAFT.len();
    assert!(
        !alone_read[after_draft..]
            .iter()
            .any(|byte| matches!(*byte, CLEAR | DEL | BS)),
        "nothing may be erased from a window with no delivery under way; after the draft \
         the child read {:?}",
        String::from_utf8_lossy(&alone_read[after_draft..])
    );

    let beside_screen = next
        .output_until(&beside, "the sentinel in the composer", |text| {
            composer_of(text).is_some_and(|composer| composer.ends_with(SENT_BESIDE))
        })
        .await;
    let beside_composer = composer_of(&beside_screen).unwrap_or_default();
    assert!(
        beside_composer.starts_with(BODY) && beside_composer.contains(APPENDED),
        "a body with more text behind it must not be taken back; composer \
         {beside_composer:?}; daemon log:\n{}",
        sandbox.daemon_log()
    );
    let after_probe =
        after_the_probe(&beside_read, BODY).expect("the child read the body and its probe");
    assert!(
        !beside_read[after_probe..].contains(&CLEAR),
        "no take-back key may be typed into a composer whose body does not stand alone; \
         after the probe the child read {:?}",
        String::from_utf8_lossy(&beside_read[after_probe..])
    );
    assert_body_kept_out_of_the_log(&sandbox, &[BODY, APPENDED]);
}
