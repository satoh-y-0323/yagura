#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: what a judged write leaves in the composer once it ends
//! without seeing its own probes, when a dialog took one of them or another
//! writer ended the call.
//!
//! The receiver is a `submit_child` that holds a pasted body unpainted (and
//! everything typed behind it) until a `clear` byte arrives, which it then
//! takes as nothing but the release: the composer reads empty while it holds
//! text, so whether a probe arrived can only be told from what is left once
//! the user's own keystroke paints it.

use crate::common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use common::{
    HANG_GUARD, Harness, SUBMIT_CHILD_PROBE, approver_base_config, composer_section_toml,
    read_composer_now, unique_temp_dir, wait_until_composer_satisfies, wait_until_status,
};
use yagura_daemon::DaemonConfig;
use yagura_daemon::detector::reading::ComposerReadout;
use yagura_daemon::input::ComposerLine;
use yagura_daemon::window::{
    ComposerResidue, InputOrigin, SendStage, SubmitJudgment, SubmitPlacement, WindowEntry,
};
use yagura_protocol::WindowStatus;
use yagura_test_support::process;

const MARKER: &str = "probes-left-ready";
const HEADING: &str = "PROBE-APPROVAL";
/// What the user types once a call has ended: it paints what the composer
/// holds, which the user then writes behind.
const USER_KEYS: &[u8] = b"\x15Q";

fn write_definition(dir: &Path, name: &str, probe: &str) {
    let toml = format!(
        r#"schema_version = 1
name = "{name}"
bottom_line_count = 10

[debounce]
confirm_ms = 30

[[rules]]
status = "waiting_approval"
pattern = '{HEADING}'
priority = 30
region = "bottom"

[[rules]]
status = "waiting_input"
pattern = '{MARKER}'
priority = 10
region = "bottom"

[input]
bracketed_paste = true
ready_hold_ms = 0
submit_delay_ms = 120
clear = ""
delivery_confirm_ms = 0
resend_max = 0
probe = "{probe}"
"#
    );
    let source = format!("{toml}{}", composer_section_toml());
    if let Err(error) = yagura_daemon::detector::definition::parse_definition(&source) {
        panic!("the test's detection definition (probe {probe:?}) must load: {error}");
    }
    std::fs::write(dir.join(format!("{name}.toml")), source)
        .expect("write detection definition file");
}

struct Receiver {
    h: Harness,
    dir: PathBuf,
    window_id: String,
    entry: Arc<WindowEntry>,
    input_log: PathBuf,
}

impl Receiver {
    /// A receiver whose definition declares `probe` and whose child holds input
    /// as `clear_scope` says and draws a dialog as `overlay`, given the
    /// receiver's directory, says.
    async fn start(
        tag: &str,
        probe: &str,
        clear_scope: &str,
        overlay: impl FnOnce(&Path) -> String,
    ) -> Self {
        let dir = unique_temp_dir(tag);
        let input_log = dir.join("input.log");
        let h = Harness::start(
            tag,
            DaemonConfig {
                detection_dir: Some(dir.clone()),
                ..approver_base_config()
            },
        )
        .await;
        write_definition(&dir, "receiver", probe);
        let mut command =
            process::submit_child_command_with_clear_scope(MARKER, 0, 0, 0, 0, clear_scope);
        command.push(overlay(&dir));
        command.push(HEADING.to_string());
        command.push(
            input_log
                .to_str()
                .expect("the input log path must be valid UTF-8")
                .to_string(),
        );
        let window_id = h.open(command, Some("receiver")).await;
        let mut observer = h.connect().await;
        wait_until_status(&mut observer, &window_id, WindowStatus::WaitingInput).await;
        let entry = h.registry.get(&window_id).expect("the receiver must exist");
        Self {
            h,
            dir,
            window_id,
            entry,
            input_log,
        }
    }

    /// Pastes `text` as an earlier writer would and waits until the child has
    /// read it, while the composer goes on reading empty.
    async fn paste_unshown(&self, text: &str) {
        self.entry
            .write_pty_input(
                format!("\x1b[200~{text}\x1b[201~").as_bytes(),
                InputOrigin::Message,
            )
            .await
            .expect("the paste must be written");
        self.h
            .wait_for_scrollback(&self.window_id, b"PASTE 1")
            .await;
        assert_eq!(
            read_composer_now(&self.h, &self.window_id).await,
            ComposerReadout::Empty,
            "the fixture must hold the pasted text unpainted"
        );
    }

    /// The file whose creation removes an `until|` dialog.
    fn dialog_gone(dir: &Path) -> PathBuf {
        dir.join("dialog-gone")
    }

    fn remove_dialog(&self) {
        std::fs::write(Self::dialog_gone(&self.dir), b"").expect("remove the dialog");
    }

    async fn wait_for_screen(&self, needle: &str) {
        self.h
            .wait_for_scrollback(&self.window_id, needle.as_bytes())
            .await;
    }

    async fn wait_for_status(&self, status: WindowStatus) {
        let mut observer = self.h.connect().await;
        wait_until_status(&mut observer, &self.window_id, status).await;
    }

    /// Waits until the ledger has reached `seq`: the call has written that far.
    async fn wait_for_writes(&self, seq: u64) {
        let deadline = tokio::time::Instant::now() + HANG_GUARD;
        while self.entry.write_seq() < seq {
            assert!(
                tokio::time::Instant::now() < deadline,
                "the call never wrote up to {seq}; the child read {:?}",
                self.child_read()
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    fn child_read(&self) -> String {
        String::from_utf8_lossy(&std::fs::read(&self.input_log).unwrap_or_default()).into_owned()
    }

    /// The composer once the user has typed [`USER_KEYS`] into it.
    async fn after_user_keys(&self) -> String {
        self.h.write_input(&self.window_id, USER_KEYS).await;
        match wait_until_composer_satisfies(
            &self.h,
            &self.window_id,
            "the user's key painted",
            |readout| matches!(readout, ComposerReadout::Body(text) if text.ends_with('Q')),
        )
        .await
        {
            ComposerReadout::Body(text) => text,
            other => panic!("expected the composer's text, got {other:?}"),
        }
    }

    async fn end(self) {
        self.h.registry.terminate_all().await;
    }
}

fn spawn_submit(
    entry: &Arc<WindowEntry>,
    body: &'static str,
    residue_seq: Option<u64>,
    requester: Option<tokio::sync::watch::Receiver<bool>>,
) -> tokio::task::JoinHandle<yagura_daemon::pty::PtyResult<SubmitJudgment>> {
    let entry = Arc::clone(entry);
    tokio::spawn(async move {
        let residue = residue_seq.map(|write_seq| ComposerResidue { body, write_seq });
        entry
            .submit_message(body, residue, SendStage::BeforeSubmitKey, requester)
            .await
    })
}

async fn declined(call: tokio::task::JoinHandle<yagura_daemon::pty::PtyResult<SubmitJudgment>>) {
    let placement = call
        .await
        .expect("the call must not panic")
        .expect("a declined write is not a PTY error")
        .placement;
    assert!(
        matches!(placement, SubmitPlacement::NotPlaced(_)),
        "a call that never saw its probes must not submit, got {placement:?}"
    );
}

/// An earlier attempt's envelope sits unpainted in the composer, so the resend
/// types a probe behind it. A dialog takes that probe as its own input, the
/// resend types the next one once the dialog is gone, and the requester leaves
/// before either shows: the envelope must stay whole.
#[tokio::test]
async fn a_requester_leaving_after_a_dialog_took_a_probe_leaves_the_envelope_whole() {
    const BODY: &str = "envelope-BODY";
    let first_probe = SUBMIT_CHILD_PROBE.as_bytes()[0];
    let receiver = Receiver::start(
        "probes-left-envelope",
        &format!("{SUBMIT_CHILD_PROBE}z"),
        "held:ignore-clear",
        |dir| {
            format!(
                "paste-byte={first_probe:02x}:hidden:until|{}",
                Receiver::dialog_gone(dir)
                    .to_str()
                    .expect("the dialog path must be valid UTF-8")
            )
        },
    )
    .await;
    let line = ComposerLine::new(BODY);
    receiver.paste_unshown(line.as_str()).await;
    let seeded = receiver.entry.write_seq();

    let (leave, requester) = tokio::sync::watch::channel(false);
    let call = spawn_submit(&receiver.entry, BODY, Some(seeded), Some(requester));
    receiver.wait_for_screen("DIALOG-INPUT").await;
    receiver
        .wait_for_status(WindowStatus::WaitingApproval)
        .await;
    receiver.remove_dialog();
    // The first probe, and the one typed once the dialog is gone.
    receiver.wait_for_writes(seeded + 2).await;
    leave
        .send(true)
        .expect("the call still listens for its requester");
    declined(call).await;

    let text = receiver.after_user_keys().await;
    let read = receiver.child_read();
    receiver.end().await;
    assert!(
        text.starts_with(line.as_str()),
        "the envelope the requester left must stay whole, trailing space included; the \
         composer reads {text:?} and the child read {read:?}"
    );
}

/// An earlier call left a probe unanswered behind a held body the receiver
/// then dropped, and this call types the next probe behind it. The user's
/// keystroke ends the call and shows both probes alone in the composer: the
/// call must not leave them in front of what the user types next.
#[tokio::test]
async fn probes_typed_behind_an_unanswered_one_are_not_left_once_another_writer_ends_the_call() {
    let receiver = Receiver::start(
        "probes-left-unanswered",
        &format!("{SUBMIT_CHILD_PROBE}zv"),
        "takes-paste-on-typing:held:ignore-clear",
        |_| format!("byte={:02x}:hidden:until-byte|15", b'z'),
    )
    .await;
    receiver.paste_unshown("dropped-draft").await;

    let (leave, requester) = tokio::sync::watch::channel(false);
    let first_at = receiver.entry.write_seq();
    let first = spawn_submit(&receiver.entry, "first-call", None, Some(requester));
    receiver.wait_for_writes(first_at + 1).await;
    leave
        .send(true)
        .expect("the call still listens for its requester");
    declined(first).await;

    let second = spawn_submit(&receiver.entry, "second-call", None, None);
    receiver
        .wait_for_status(WindowStatus::WaitingApproval)
        .await;
    receiver
        .entry
        .write_pty_input(b"\x15", InputOrigin::Interactive)
        .await
        .expect("the user's keystroke must be written");
    declined(second).await;

    let text = receiver.after_user_keys().await;
    let read = receiver.child_read();
    receiver.end().await;
    assert_eq!(
        text, "Q",
        "the call's probes must not stay in front of what the user types; the child read {read:?}"
    );
}
