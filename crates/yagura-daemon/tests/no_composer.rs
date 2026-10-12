#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: the daemon writes text of its own composition only into
//! a window whose detection definition declares a `[composer]` table, and a
//! line whose showing it must judge only where the definition also names
//! `[input].probe`.
//!
//! On a window without one, nothing on screen tells the daemon whether a
//! written body would land in an input line or in a running program's
//! stdin, so both writing entry points (`WindowEntry::submit_message` and
//! `WindowEntry::submit_raw_gated`) must decline before reading or writing
//! anything. Every case drives a real `WindowRegistry` against `echo_child`,
//! so "nothing written" is checked against what reached the child, and every
//! negative observation is paired with the same write succeeding on a window
//! whose definition does declare a composer.

mod common;

use std::path::Path;
use std::time::Duration;

use common::{
    CONTROL_INPUT_TABLE, ChatPlaybook, Harness, READY_MARKER, SUBMIT_CHILD_PROBE, VALID_TOKEN,
    approver_base_config, call_list_windows, call_open_window, call_send_handoff_from_profile,
    call_send_handoff_from_profile_within, chat_config, composer_section_toml, detection_config,
    empty_lookup, expect_approval_requested, expect_error, expect_handoff_sent,
    handoff_profile_config, identify, policy, read_response, register_approver, send,
    unique_temp_dir, wait_until_status, write_profile,
};
use tracing_test::traced_test;
use yagura_daemon::DaemonConfig;
use yagura_daemon::control::ComposerAccess;
use yagura_daemon::input::plan_raw_write;
use yagura_daemon::window::{
    NotPlaced, NotPlacedReason, SubmitJudgment, SubmitPlacement, WindowParticipation,
};
use yagura_protocol::{
    ClientMessage, DaemonMessage, InstructionNoticeState, PlanInfo, PlanTaskInfo, PlanTaskSpec,
    ReceivedReport, ReportOutcome, WindowStatus,
};
use yagura_test_support::detection::idle_definition_toml;
use yagura_test_support::ipc::Connection;
use yagura_test_support::process::{
    echo_child_command, echo_child_command_with_composer, ready_child_profile_source_with_composer,
};

/// Definition stem whose windows reach `idle` but declare no composer.
const SILENT: &str = "silent-shell";

/// Definition stem identical to [`SILENT`] except that it declares a composer
/// and a probe.
const DRAWN: &str = "drawn-shell";

/// Definition stem identical to [`DRAWN`] except that it names no probe: the
/// daemon may type into its composer but cannot judge whether a line showed.
const RAW_ONLY: &str = "raw-only-shell";

/// How long a body that must never be written is watched for in the child's
/// output. The positive control on a composer-declaring window shows an
/// echoed body well within this, so an absence over this span is not merely
/// a slow echo.
const ABSENCE_OBSERVATION: Duration = Duration::from_millis(1500);

/// Upper bound for a body that must be written to show up in the child's
/// output.
const PRESENCE_TIMEOUT: Duration = Duration::from_secs(10);

fn write_definitions(dir: &Path) {
    std::fs::write(
        dir.join(format!("{SILENT}.toml")),
        idle_definition_toml(SILENT, "", ""),
    )
    .expect("write the definition without a composer");
    std::fs::write(
        dir.join(format!("{DRAWN}.toml")),
        idle_definition_toml(
            DRAWN,
            "",
            &format!("{}{}", probe_input_table(), composer_section_toml()),
        ),
    )
    .expect("write the definition with a composer");
    std::fs::write(
        dir.join(format!("{RAW_ONLY}.toml")),
        idle_definition_toml(RAW_ONLY, "", &composer_section_toml()),
    )
    .expect("write the definition with a composer but no probe");
}

/// The `[input]` table [`DRAWN`] carries: the probe a judged delivery needs.
fn probe_input_table() -> String {
    format!("\n[input]\nprobe = \"{SUBMIT_CHILD_PROBE}\"\n")
}

async fn start(tag: &str) -> Harness {
    start_with_base(tag, common::opening_config()).await
}

/// [`start`], with `base` supplying every field but the detection layer and
/// the status token: a case that needs a non-default limit (the handoff slot
/// cap, for instance) sets it on `base` rather than restating the whole
/// fixture.
async fn start_with_base(tag: &str, base: DaemonConfig) -> Harness {
    let dir = unique_temp_dir(tag);
    write_definitions(&dir);
    Harness::start(
        tag,
        DaemonConfig {
            detection_dir: Some(dir),
            status_token_override: Some(VALID_TOKEN.into()),
            ..base
        },
    )
    .await
}

/// Opens an `echo_child` window under `detection` and observes it reach
/// `idle`, so a decline cannot be put down to a window that was not ready.
async fn open_ready(h: &Harness, detection: &str) -> String {
    open_ready_running(h, detection, echo_child_command()).await
}

/// [`open_ready`] with `command` as the window's child.
async fn open_ready_running(h: &Harness, detection: &str, command: Vec<String>) -> String {
    let mut client = h.connect().await;
    let window_id = h.open(command, Some(detection)).await;
    h.write_input(&window_id, READY_MARKER).await;
    wait_until_status(
        &mut client,
        &window_id,
        WindowStatus::Idle,
        Duration::from_secs(10),
    )
    .await;
    window_id
}

// Reads the registry's participant list directly to check a window's membership.
#[allow(clippy::disallowed_methods)]
fn participation_of(h: &Harness, window_id: &str) -> WindowParticipation {
    h.registry
        .list_participation()
        .into_iter()
        .find(|participation| participation.window_id == window_id)
        .expect("an open window must be listed")
}

fn scrollback_contains(h: &Harness, window_id: &str, needle: &str) -> bool {
    common::contains_subsequence(&h.scrollback(window_id), needle.as_bytes())
}

#[tokio::test]
async fn neither_writing_entry_point_writes_into_a_window_that_declares_no_composer() {
    let h = start("no-composer-writes").await;
    let silent_id = open_ready(&h, SILENT).await;
    let drawn_id = open_ready(&h, DRAWN).await;
    let silent = h.registry.get(&silent_id).expect("silent window");
    let drawn = h.registry.get(&drawn_id).expect("drawn window");
    let declined = SubmitPlacement::NotPlaced(NotPlaced {
        envelope_remains: false,
        reason: NotPlacedReason::NoComposer,
    });

    let seq_before: u64 = silent.write_seq();
    let judged: SubmitJudgment = silent
        .submit_message("silent-judged-body", None, None)
        .await
        .expect("a declined write is not a PTY error");
    let raw: SubmitJudgment = silent
        .submit_raw_gated(&plan_raw_write("silent-raw-body"), None)
        .await
        .expect("a declined write is not a PTY error");

    assert_eq!(
        judged.placement, declined,
        "a judged line must be declined for the missing composer itself"
    );
    assert_eq!(judged.residue_write_seq, None);
    assert_eq!(
        raw.placement, declined,
        "a raw body must be declined for the missing composer itself"
    );
    assert_eq!(raw.residue_write_seq, None);
    assert_eq!(
        silent.write_seq(),
        seq_before,
        "a declined window must not have taken a single write"
    );

    let control_seq_before: u64 = drawn.write_seq();
    let control: SubmitJudgment = drawn
        .submit_raw_gated(&plan_raw_write("drawn-raw-body"), None)
        .await
        .expect("the control write must not fail");
    assert!(
        matches!(control.placement, SubmitPlacement::Placed(_)),
        "the same raw body must be written into a window that declares a composer, got {:?}",
        control.placement
    );
    assert!(
        drawn.write_seq() > control_seq_before,
        "the control write must be counted, or an unchanged count proves nothing"
    );
    h.wait_for_scrollback(&drawn_id, b"drawn-raw-body", PRESENCE_TIMEOUT)
        .await;

    tokio::time::sleep(ABSENCE_OBSERVATION).await;
    assert!(
        !scrollback_contains(&h, &silent_id, "silent-judged-body"),
        "the judged body must never reach the child"
    );
    assert!(
        !scrollback_contains(&h, &silent_id, "silent-raw-body"),
        "the raw body must never reach the child"
    );
    assert_eq!(silent.write_seq(), seq_before);

    h.registry.terminate_all().await;
}

#[tokio::test]
async fn a_window_records_whether_its_definition_declares_a_composer() {
    let h = start("no-composer-record").await;
    let silent_id = open_ready(&h, SILENT).await;
    let drawn_id = open_ready(&h, DRAWN).await;
    let undefined_id = h
        .open(echo_child_command(), Some("absent-definition"))
        .await;

    let raw_id = open_ready(&h, RAW_ONLY).await;

    let cases: [(&str, &str, ComposerAccess); 4] = [
        (silent_id.as_str(), "no composer", ComposerAccess::Absent),
        (
            raw_id.as_str(),
            "a composer but no probe",
            ComposerAccess::RawOnly,
        ),
        (
            drawn_id.as_str(),
            "a composer and a probe",
            ComposerAccess::Judged {
                probe: probe_char(),
            },
        ),
        (
            undefined_id.as_str(),
            "no definition at all",
            ComposerAccess::Absent,
        ),
    ];
    for (window_id, label, expected) in cases {
        let entry = h.registry.get(window_id).expect("open window");
        let access: ComposerAccess = entry.composer_access();
        assert_eq!(
            access, expected,
            "a window whose definition declares {label} must report {expected:?}"
        );
        let listed: ComposerAccess = participation_of(&h, window_id).composer_access;
        assert_eq!(
            listed, access,
            "the participation snapshot must agree with the window itself ({label})"
        );
    }

    h.registry.terminate_all().await;
}

/// The bundled and directory layers stacked the way a config root stacks
/// them, with no explicit `detection`, so a window resolves the default name.
fn layered_config(dir: &Path) -> DaemonConfig {
    DaemonConfig {
        detection_dir: Some(dir.to_path_buf()),
        bundled_definitions: true,
        ..common::opening_config()
    }
}

#[tokio::test]
async fn the_bundled_default_shell_definition_declares_no_composer() {
    let dir = unique_temp_dir("no-composer-bundled");
    let h = Harness::start("no-composer-bundled", layered_config(&dir)).await;
    let window_id = h.open(echo_child_command(), None).await;
    let entry = h.registry.get(&window_id).expect("open window");
    assert_eq!(
        entry.detection_definition(),
        Some("generic-shell"),
        "a window with no definition would read false too, so it must resolve the bundled one"
    );

    let access: ComposerAccess = entry.composer_access();
    assert_eq!(
        access,
        ComposerAccess::Absent,
        "a plain shell's screen cannot tell its prompt from program output, so the bundled \
         default must not declare a composer"
    );

    h.registry.terminate_all().await;
}

#[tokio::test]
async fn a_user_layer_definition_of_the_same_name_that_declares_a_composer_is_written_to() {
    let dir = unique_temp_dir("no-composer-user-layer");
    std::fs::write(
        dir.join("generic-shell.toml"),
        idle_definition_toml("generic-shell", "", &composer_section_toml()),
    )
    .expect("write the user layer definition");
    let h = Harness::start("no-composer-user-layer", layered_config(&dir)).await;
    let window_id = h.open(echo_child_command(), None).await;
    let entry = h.registry.get(&window_id).expect("open window");

    let access: ComposerAccess = entry.composer_access();
    assert_eq!(
        access,
        ComposerAccess::RawOnly,
        "the gate follows what the resolved definition declares, not its name"
    );
    let written: SubmitJudgment = entry
        .submit_raw_gated(&plan_raw_write("user-layer-raw-body"), None)
        .await
        .expect("the write must not fail");
    assert!(
        matches!(written.placement, SubmitPlacement::Placed(_)),
        "a window whose definition declares a composer must take the body, got {:?}",
        written.placement
    );
    h.wait_for_scrollback(&window_id, b"user-layer-raw-body", PRESENCE_TIMEOUT)
        .await;

    h.registry.terminate_all().await;
}

// =====================================================================
// The entry gate (`check_destructive`'s composer gate): a
// request that is already known to be refused is turned away before it
// reaches any identity or approval relief, and with the same guidance
// `no_composer.rs`'s own window-layer tests observe (open_window's command,
// send_keys).
// =====================================================================

#[tokio::test]
async fn send_message_to_a_composerless_window_is_refused_with_no_composer_from_any_origin() {
    let h = start("no-composer-entry-send").await;
    let window_id = open_ready(&h, SILENT).await;
    let entry = h.registry.get(&window_id).expect("open window");

    let mut owner = h.connect().await;
    identify(&mut owner, &window_id).await;
    let mut external = h.connect().await;

    for (form, text) in [
        ("a terminated line", "hello\r"),
        ("no terminator", "hello"),
        ("a bare terminator", "\r"),
    ] {
        for (origin_label, client) in [
            ("an external caller", &mut external),
            ("the window identified as itself", &mut owner),
        ] {
            let seq_before = entry.write_seq();
            send(
                client,
                &ClientMessage::SendText {
                    window_id: window_id.clone(),
                    text: text.to_string(),
                },
            )
            .await;
            let message = expect_error(
                read_response(client, Duration::from_secs(5)).await,
                "no_composer",
            );
            assert!(
                message.contains("send_keys") && message.contains("open_window"),
                "the refusal for {form} from {origin_label} must name send_keys and \
                 open_window as the alternatives, got {message:?}"
            );
            assert!(
                !message.contains("detection"),
                "the refusal must not read as an invitation to reopen the window with a \
                 detection definition, got {message:?}"
            );
            assert_eq!(
                entry.write_seq(),
                seq_before,
                "a refused send_message ({form} from {origin_label}) must not have written \
                 anything, own-window relief included"
            );
        }
    }

    tokio::time::sleep(ABSENCE_OBSERVATION).await;
    assert!(
        !scrollback_contains(&h, &window_id, "hello"),
        "no form of the refused body may have reached the child"
    );

    h.registry.terminate_all().await;
}

#[tokio::test]
async fn send_handoff_to_a_composerless_window_id_is_refused_with_no_composer_and_frees_its_slot() {
    let h = start_with_base(
        "no-composer-entry-handoff",
        DaemonConfig {
            max_open_handoffs_per_conn: 1,
            ..common::opening_config()
        },
    )
    .await;
    let window_id = open_ready(&h, SILENT).await;
    let mut client = h.connect().await;

    // With the per-connection slot cap set to one, a second refusal that
    // still reports no_composer (rather than resource_exhausted) proves the
    // first refusal released the slot it reserved.
    for attempt in 1..=2 {
        send(
            &mut client,
            &ClientMessage::SendHandoff {
                window_id: Some(window_id.clone()),
                profile: None,
                cwd: None,
                name: None,
                model: None,
                effort: None,
                reference: "src/foo.rs".to_string(),
                note: "please look at the error handling".to_string(),
            },
        )
        .await;
        let message = expect_error(
            read_response(&mut client, Duration::from_secs(5)).await,
            "no_composer",
        );
        assert!(
            message.contains("send_keys") && message.contains("open_window"),
            "attempt {attempt} must carry the same guidance as send_message's refusal, \
             got {message:?}"
        );
    }

    h.registry.terminate_all().await;
}

#[tokio::test]
async fn a_window_with_no_definition_answers_send_message_and_send_handoff_alike() {
    let h = start("no-composer-entry-undefined").await;
    let window_id = h
        .open(echo_child_command(), Some("absent-definition"))
        .await;
    let mut client = h.connect().await;

    send(
        &mut client,
        &ClientMessage::SendText {
            window_id: window_id.clone(),
            text: "hello\r".to_string(),
        },
    )
    .await;
    let send_text_message = expect_error(
        read_response(&mut client, Duration::from_secs(5)).await,
        "status_unknown",
    );

    send(
        &mut client,
        &ClientMessage::SendHandoff {
            window_id: Some(window_id.clone()),
            profile: None,
            cwd: None,
            name: None,
            model: None,
            effort: None,
            reference: "src/foo.rs".to_string(),
            note: "please look at the error handling".to_string(),
        },
    )
    .await;
    let send_handoff_message = expect_error(
        read_response(&mut client, Duration::from_secs(5)).await,
        "status_unknown",
    );

    assert_eq!(
        send_text_message, send_handoff_message,
        "a window with no definition at all must read status_unknown the same way \
         whichever writing tool asked, since the composer gate never reasons about a \
         missing definition"
    );

    h.registry.terminate_all().await;
}

// =====================================================================
// Before a child is started: a profile-addressed handoff, and a task
// window whose task carries an instruction, are refused from the launch's
// resolved definition alone, so a request that is already known to be
// refused never spawns anything.
// =====================================================================

/// Definition stem whose windows reach `idle` on the ready fixture's own
/// startup line but declare no composer; the launch-side counterpart of
/// [`SILENT`].
const SILENT_LAUNCH: &str = "silent-launch";

/// Identical to [`SILENT_LAUNCH`] except that it declares a composer.
const DRAWN_LAUNCH: &str = "drawn-launch";

/// Declares a composer, but its `[input]` table names no probe.
const RAW_LAUNCH: &str = "raw-launch";

/// [`CONTROL_INPUT_TABLE`] without its probe.
const RAW_LAUNCH_INPUT_TABLE: &str = "\n[input]\nsubmit_delay_ms = 40\ndelivery_confirm_ms = 0\n";

/// A detection name no layer defines, so the definition cannot be read
/// before the window opens.
const UNDEFINED_LAUNCH: &str = "undefined-launch";

/// Upper bound for a reply that is expected to arrive once a window opened
/// for it reaches readiness.
const LAUNCH_REPLY_TIMEOUT: Duration = Duration::from_secs(15);

/// The two launch definitions differ only in the composer table; both keep
/// the confirm-free `[input]` table so the positive control answers as soon
/// as its envelope is written.
fn launch_definition_toml(name: &str, with_composer: bool) -> String {
    let composer = if with_composer {
        composer_section_toml()
    } else {
        String::new()
    };
    idle_definition_toml(name, "", &format!("{composer}{CONTROL_INPUT_TABLE}"))
}

fn write_launch_definitions(dir: &Path) {
    for (name, with_composer) in [(SILENT_LAUNCH, false), (DRAWN_LAUNCH, true)] {
        std::fs::write(
            dir.join(format!("{name}.toml")),
            launch_definition_toml(name, with_composer),
        )
        .expect("write a launch definition");
    }
    std::fs::write(
        dir.join(format!("{RAW_LAUNCH}.toml")),
        idle_definition_toml(
            RAW_LAUNCH,
            "",
            &format!("{}{RAW_LAUNCH_INPUT_TABLE}", composer_section_toml()),
        ),
    )
    .expect("write the launch definition without a probe");
}

/// A profile launching the ready fixture (which prints the idle marker on
/// its own and paints a composer) under `detection`, so every profile here
/// runs the very same child and only the named definition differs.
fn ready_profile_source_with_detection(display_name: &str, detection: &str) -> String {
    let source = ready_child_profile_source_with_composer(display_name, 0);
    let with_detection = source.replacen(
        "\n\n[command]",
        &format!("\ndetection = \"{detection}\"\n\n[command]"),
        1,
    );
    assert_ne!(
        with_detection, source,
        "the profile source must contain the command table header once"
    );
    with_detection
}

fn write_launch_profiles(profiles_dir: &Path) {
    for (id, display_name, detection) in [
        ("silent-profile", "Silent", SILENT_LAUNCH),
        ("drawn-profile", "Drawn", DRAWN_LAUNCH),
        ("undefined-profile", "Undefined", UNDEFINED_LAUNCH),
        ("raw-profile", "Raw", RAW_LAUNCH),
    ] {
        write_profile(
            profiles_dir,
            id,
            &ready_profile_source_with_detection(display_name, detection),
        );
    }
}

fn assert_no_composer_guidance(message: &str, context: &str) {
    assert!(
        message.contains("send_keys") && message.contains("open_window"),
        "{context}: the refusal must carry the same guidance as send_message's, got {message:?}"
    );
}

/// Starts a daemon whose profiles name the launch definitions, with
/// `playbook` / `approval_policy` deciding what the policy half of the gate
/// would answer.
async fn start_launch(
    tag: &str,
    playbook: Option<std::path::PathBuf>,
    approval_policy: Option<yagura_daemon::control::ApprovalPolicy>,
) -> (Harness, std::path::PathBuf) {
    let dir = unique_temp_dir(tag);
    let profiles_dir = dir.join("profiles");
    let detection_dir = dir.join("detection");
    std::fs::create_dir_all(&profiles_dir).expect("create profiles dir");
    std::fs::create_dir_all(&detection_dir).expect("create detection dir");
    write_launch_definitions(&detection_dir);
    write_launch_profiles(&profiles_dir);
    let config = handoff_profile_config(
        Some(profiles_dir),
        empty_lookup(),
        &detection_dir,
        playbook,
        approval_policy,
        30,
    );
    (Harness::start(tag, config).await, dir)
}

async fn window_count(client: &mut Connection) -> usize {
    call_list_windows(client).await.len()
}

#[tokio::test]
async fn send_handoff_to_a_profile_whose_definition_declares_no_composer_starts_no_child() {
    let (h, _dir) = start_launch(
        "no-composer-pre-open-handoff",
        None,
        Some(policy(&["send_handoff"])),
    )
    .await;
    let mut client = h.connect().await;

    let before: usize = window_count(&mut client).await;
    let msg =
        call_send_handoff_from_profile(&mut client, "silent-profile", None, None, "r", "t").await;
    let message = expect_error(msg, "no_composer");
    assert_no_composer_guidance(&message, "a profile-addressed handoff");
    assert_eq!(
        window_count(&mut client).await,
        before,
        "a handoff known to be refused must not have opened a window for it, even one it \
         closes again before answering"
    );
    // Watched past the reply, so a launch left running behind an early
    // answer would still be seen.
    tokio::time::sleep(ABSENCE_OBSERVATION).await;
    assert_eq!(window_count(&mut client).await, before);

    let control = call_send_handoff_from_profile_within(
        &mut client,
        "drawn-profile",
        None,
        None,
        "r",
        "t",
        LAUNCH_REPLY_TIMEOUT,
    )
    .await;
    let (_window_id, opened, _delivery) = expect_handoff_sent(control);
    assert!(
        opened,
        "the same launch under a definition that declares a composer must open and deliver"
    );
    assert_eq!(
        window_count(&mut client).await,
        before + 1,
        "the control must have opened exactly one window, or the unchanged count proves nothing"
    );

    h.registry.terminate_all().await;
}

#[tokio::test]
async fn the_composer_refusal_follows_the_launch_content_check_and_precedes_the_playbook_check() {
    let tag = "no-composer-pre-open-order";
    let playbook_dir = unique_temp_dir(&format!("{tag}-playbook"));
    // A configured playbook with no loaded policy is how a playbook that
    // failed to load at startup is represented; the file itself is never read.
    let playbook = playbook_dir.join("unused-playbook.toml");
    std::fs::write(&playbook, "").expect("write the playbook placeholder");
    let (h, dir) = start_launch(tag, Some(playbook), None).await;
    let mut client = h.connect().await;
    let before: usize = window_count(&mut client).await;

    let missing_cwd = dir.join("cwd-that-does-not-exist");
    let missing_cwd = missing_cwd.to_string_lossy().into_owned();
    let msg = call_send_handoff_from_profile(
        &mut client,
        "silent-profile",
        Some(&missing_cwd),
        None,
        "r",
        "t",
    )
    .await;
    expect_error(msg, "invalid_argument");

    let msg =
        call_send_handoff_from_profile(&mut client, "silent-profile", None, None, "r", "t").await;
    let message = expect_error(msg, "no_composer");
    assert_no_composer_guidance(&message, "a handoff under a broken playbook");

    let control =
        call_send_handoff_from_profile(&mut client, "drawn-profile", None, None, "r", "t").await;
    let control_message = expect_error(control, "approval_required");
    assert!(
        control_message.contains("the configured playbook failed to load"),
        "the broken playbook must still refuse a composer-declaring launch, or the composer \
         refusal above was never tested against it: {control_message:?}"
    );

    assert_eq!(
        window_count(&mut client).await,
        before,
        "none of these refusals may have opened a window"
    );
}

// --- open_task_window --------------------------------------------------------

/// A daemon with the plan ledger enabled whose profiles name the launch
/// definitions. The plan review gate is off so a created plan's tasks can be
/// opened at once.
async fn start_ledger(tag: &str) -> Harness {
    let dir = unique_temp_dir(tag);
    let profiles_dir = dir.join("profiles");
    let detection_dir = dir.join("detection");
    std::fs::create_dir_all(&profiles_dir).expect("create profiles dir");
    std::fs::create_dir_all(&detection_dir).expect("create detection dir");
    write_launch_profiles(&profiles_dir);
    let config = DaemonConfig {
        store_db_path_resolved: Some(dir.join("plan.db")),
        profiles_dir: Some(profiles_dir),
        approval_policy: Some(policy(&["open_window"])),
        plan_approval_required: false,
        ..detection_config(&detection_dir, approver_base_config())
    };
    write_launch_definitions(&detection_dir);
    let (h, _locks) = Harness::start_with_ledger(tag, config).await;
    h
}

/// Reads the next response frame, skipping `plan_changed` pushes.
async fn read_ledger_reply(client: &mut Connection, timeout: Duration) -> DaemonMessage {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(!remaining.is_zero(), "timed out waiting for a reply");
        let frame = read_response(client, remaining).await;
        if !matches!(frame, DaemonMessage::PlanChanged { .. }) {
            return frame;
        }
    }
}

fn task_spec(task_id: &str, profile: &str, instruction: Option<&str>) -> PlanTaskSpec {
    PlanTaskSpec {
        task_id: task_id.to_string(),
        title: format!("task {task_id}"),
        role: None,
        profile: Some(profile.to_string()),
        model: None,
        effort: None,
        depends_on: Vec::new(),
        writes: vec![format!("{task_id}.txt")],
        instruction: instruction.map(str::to_string),
    }
}

/// Opens a plain window and identifies `owner` as it: only a window may own
/// a plan and open its task windows.
async fn become_plan_owner(h: &Harness, owner: &mut Connection) {
    let window_id = h.open(echo_child_command(), None).await;
    identify(owner, &window_id).await;
}

async fn create_plan(owner: &mut Connection, tasks: Vec<PlanTaskSpec>) -> PlanInfo {
    send(
        owner,
        &ClientMessage::CreatePlan {
            title: "plan".to_string(),
            tasks,
        },
    )
    .await;
    match read_ledger_reply(owner, Duration::from_secs(5)).await {
        DaemonMessage::PlanDetail { plan } => plan,
        other => panic!("expected plan_detail, got {other:?}"),
    }
}

async fn stored_task(owner: &mut Connection, plan_id: &str, task_id: &str) -> PlanTaskInfo {
    send(
        owner,
        &ClientMessage::GetPlan {
            plan_id: plan_id.to_string(),
        },
    )
    .await;
    let plan = match read_ledger_reply(owner, Duration::from_secs(5)).await {
        DaemonMessage::PlanDetail { plan } => plan,
        other => panic!("expected plan_detail, got {other:?}"),
    };
    plan.tasks
        .into_iter()
        .find(|task| task.task_id == task_id)
        .unwrap_or_else(|| panic!("task {task_id} must be in plan {plan_id}"))
}

async fn open_task_window(
    owner: &mut Connection,
    plan_id: &str,
    task_id: &str,
    name: &str,
) -> DaemonMessage {
    send(
        owner,
        &ClientMessage::OpenTaskWindow {
            plan_id: plan_id.to_string(),
            task_id: task_id.to_string(),
            name: Some(name.to_string()),
        },
    )
    .await;
    read_ledger_reply(owner, LAUNCH_REPLY_TIMEOUT).await
}

fn expect_task_window_opened(msg: DaemonMessage, context: &str) -> String {
    match msg {
        DaemonMessage::TaskWindowOpened { window_id, .. } => window_id,
        other => panic!("{context}: expected task_window_opened, got {other:?}"),
    }
}

async fn windows_named(client: &mut Connection, name: &str) -> usize {
    call_list_windows(client)
        .await
        .into_iter()
        .filter(|window| window.name == name)
        .count()
}

#[tokio::test]
async fn open_task_window_refuses_an_instructed_task_whose_profile_declares_no_composer_before_starting_a_child()
 {
    let h = start_ledger("no-composer-pre-open-task").await;
    let mut owner = h.connect().await;
    become_plan_owner(&h, &mut owner).await;
    let plan = create_plan(
        &mut owner,
        vec![
            task_spec("instructed", "silent-profile", Some("do the thing")),
            task_spec("bare", "silent-profile", None),
            task_spec("drawn", "drawn-profile", Some("do the thing")),
        ],
    )
    .await;

    let before: usize = window_count(&mut owner).await;
    let msg = open_task_window(&mut owner, &plan.plan_id, "instructed", "instructed-window").await;
    let message = expect_error(msg, "no_composer");
    assert_no_composer_guidance(&message, "an instructed task window");
    assert_eq!(
        window_count(&mut owner).await,
        before,
        "an instructed task whose window could never be told its instruction must not start a \
         child"
    );
    tokio::time::sleep(ABSENCE_OBSERVATION).await;
    assert_eq!(windows_named(&mut owner, "instructed-window").await, 0);

    let refused = stored_task(&mut owner, &plan.plan_id, "instructed").await;
    assert_eq!(
        refused.window_id, None,
        "a refused task must not be bound to any window"
    );
    assert_eq!(
        refused.instruction_notice, None,
        "a refused task must not carry a notice that was never going to be delivered"
    );

    let bare = open_task_window(&mut owner, &plan.plan_id, "bare", "bare-window").await;
    let bare_window: String = expect_task_window_opened(
        bare,
        "the same profile with nothing to tell the window must open",
    );
    let drawn = open_task_window(&mut owner, &plan.plan_id, "drawn", "drawn-window").await;
    let drawn_window: String = expect_task_window_opened(
        drawn,
        "an instructed task whose definition declares a composer must open",
    );
    assert_eq!(
        stored_task(&mut owner, &plan.plan_id, "bare")
            .await
            .window_id,
        Some(bare_window),
        "the control must be bound, or an unbound refused task proves nothing"
    );
    assert_eq!(
        stored_task(&mut owner, &plan.plan_id, "drawn")
            .await
            .window_id,
        Some(drawn_window)
    );

    h.registry.terminate_all().await;
}

#[tokio::test]
async fn an_instructed_task_whose_definition_cannot_be_read_leaves_no_window_running_and_no_pending_notice()
 {
    let h = start_ledger("no-composer-pre-open-undefined").await;
    let mut owner = h.connect().await;
    become_plan_owner(&h, &mut owner).await;
    let plan = create_plan(
        &mut owner,
        vec![
            task_spec("instructed", "undefined-profile", Some("do the thing")),
            task_spec("bare", "undefined-profile", None),
        ],
    )
    .await;

    let msg = open_task_window(&mut owner, &plan.plan_id, "instructed", "instructed-window").await;
    let message = expect_error(msg, "no_composer");
    assert_no_composer_guidance(&message, "an instructed task with an unreadable definition");

    // A definition that cannot be read before opening is only known once the
    // window exists, so a window may have been started; it must be gone.
    let started: Vec<String> = call_list_windows(&mut owner)
        .await
        .into_iter()
        .filter(|window| window.name == "instructed-window")
        .map(|window| window.window_id)
        .collect();
    for window_id in &started {
        h.wait_until_child_exited(window_id, LAUNCH_REPLY_TIMEOUT)
            .await;
    }

    let deadline = tokio::time::Instant::now() + LAUNCH_REPLY_TIMEOUT;
    loop {
        let notice = stored_task(&mut owner, &plan.plan_id, "instructed")
            .await
            .instruction_notice;
        let state: Option<InstructionNoticeState> = notice.map(|notice| notice.state);
        if state != Some(InstructionNoticeState::Pending) {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "a notice for a window that was closed unannounced must not stay pending"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let bare = open_task_window(&mut owner, &plan.plan_id, "bare", "bare-window").await;
    let bare_window: String = expect_task_window_opened(
        bare,
        "the same profile with nothing to tell the window must open",
    );
    assert_eq!(
        stored_task(&mut owner, &plan.plan_id, "bare")
            .await
            .window_id,
        Some(bare_window),
        "the control must be bound, or the refusal above may be any failure to open"
    );

    h.registry.terminate_all().await;
}

// =====================================================================
// The notice queue: a composerless window's own report inbox raises no
// attempt to type a notice into it, and its inbox is still discarded once it
// ends -- the delivery agent that discards it still runs (it is only the
// notice, not the agent, that a missing composer withholds).
// =====================================================================

/// The delivery agent's own per-attempt log line (`notice_queue.rs`),
/// present only once `attempt_notice` was actually dispatched for a window.
const NOTICE_ATTEMPT_LOG: &str = "notice delivery attempt finished";

/// How long the delivery agent is watched for a periodic attempt on a
/// composerless window. Several times the agent's own retry spacing, so an
/// agent that spins on the composer check rather than going idle at
/// `next_notice` would have logged at least once within this span.
const NOTICE_SPIN_OBSERVATION: Duration = Duration::from_secs(6);

async fn call_send_report(client: &mut Connection, summary: &str) -> DaemonMessage {
    send(
        client,
        &ClientMessage::SendReport {
            outcome: ReportOutcome::Succeeded,
            summary: summary.to_string(),
            path: None,
        },
    )
    .await;
    read_response(client, Duration::from_secs(5)).await
}

fn expect_report_sent(msg: DaemonMessage) {
    assert!(
        matches!(msg, DaemonMessage::ReportSent { .. }),
        "expected report_sent, got {msg:?}"
    );
}

async fn call_wait_for_report(client: &mut Connection, timeout_sec: u64) -> Vec<ReceivedReport> {
    send(client, &ClientMessage::WaitForReport { timeout_sec }).await;
    match read_response(client, Duration::from_secs(5)).await {
        DaemonMessage::Reports { reports, .. } => reports,
        other => panic!("expected reports, got {other:?}"),
    }
}

/// Opens `detection`'s parent window, opens a second window as its child
/// (identifying the parent first, so `call_open_window` attributes the child
/// to it), and has the child `send_report` to it -- the report landing in
/// the parent's inbox is what makes a notice fall due. A parent under a
/// composer-declaring definition runs `echo_child` in composer-draw mode, so
/// a notice written into it can be seen to land.
async fn open_parent_with_reporting_child(h: &Harness, detection: &str) -> String {
    let parent_command = if detection == DRAWN {
        echo_child_command_with_composer(0)
    } else {
        echo_child_command()
    };
    let parent_id = open_ready_running(h, detection, parent_command).await;
    let mut parent_conn = h.connect().await;
    identify(&mut parent_conn, &parent_id).await;
    let child_id = call_open_window(&mut parent_conn, echo_child_command()).await;
    let mut child_conn = h.connect().await;
    identify(&mut child_conn, &child_id).await;
    expect_report_sent(call_send_report(&mut child_conn, "child done").await);
    parent_id
}

#[tokio::test]
#[traced_test]
async fn a_composerless_windows_report_notice_is_never_attempted() {
    let h = start("no-composer-notice-spin").await;
    let silent_id = open_parent_with_reporting_child(&h, SILENT).await;
    let drawn_id = open_parent_with_reporting_child(&h, DRAWN).await;
    let silent_entry = h.registry.get(&silent_id).expect("silent parent");
    let drawn_entry = h.registry.get(&drawn_id).expect("drawn parent");

    // Control: the composer-declaring parent's report notice must actually
    // be dispatched, or the negative assertions below would prove nothing.
    let deadline = tokio::time::Instant::now() + PRESENCE_TIMEOUT;
    while drawn_entry.notice_attempts() == 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the composer-declaring parent's report notice was never attempted"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let drawn_log = format!("{NOTICE_ATTEMPT_LOG} window_id=\"{drawn_id}\"");
    // An attempt is counted as it finishes and logged just after.
    while !logs_contain(&drawn_log) && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        logs_contain(&drawn_log),
        "the log substring this test relies on must match the composer-declaring parent's own \
         attempt, or the negative assertion below proves nothing"
    );

    tokio::time::sleep(NOTICE_SPIN_OBSERVATION).await;
    assert_eq!(
        silent_entry.notice_attempts(),
        0,
        "a composerless window's report notice must never be attempted"
    );
    let silent_log = format!("{NOTICE_ATTEMPT_LOG} window_id=\"{silent_id}\"");
    assert!(
        !logs_contain(&silent_log),
        "the composer check must sit in next_notice, ahead of the delivery agent even \
         considering the notice -- not only inside a dispatched attempt, which would still \
         spin the agent on the retry interval and log every cycle"
    );

    let mut silent_owner = h.connect().await;
    identify(&mut silent_owner, &silent_id).await;
    let reports = call_wait_for_report(&mut silent_owner, 0).await;
    assert_eq!(
        reports.len(),
        1,
        "the report must still be readable through wait_for_report even though no notice was \
         ever typed for it"
    );
    assert_eq!(reports[0].summary, "child done");

    h.registry.terminate_all().await;
}

#[tokio::test]
async fn a_composerless_windows_report_inbox_is_discarded_once_it_ends() {
    let h = start_with_base(
        "no-composer-notice-inbox",
        DaemonConfig {
            approval_policy: Some(policy(&["close_window", "open_window"])),
            ..approver_base_config()
        },
    )
    .await;
    let silent_id = open_parent_with_reporting_child(&h, SILENT).await;

    let mut approver = h.connect().await;
    register_approver(&mut approver).await;
    send(
        &mut approver,
        &ClientMessage::CloseWindow {
            window_id: silent_id.clone(),
            force: None,
        },
    )
    .await;
    match read_response(&mut approver, Duration::from_secs(10)).await {
        DaemonMessage::WindowClosed { window_id } => assert_eq!(window_id, silent_id),
        other => panic!("expected window_closed, got {other:?}"),
    }
    h.wait_until_child_exited(&silent_id, Duration::from_secs(10))
        .await;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let mut probe = h.connect().await;
        identify(&mut probe, &silent_id).await;
        let reports = call_wait_for_report(&mut probe, 0).await;
        if reports.is_empty() {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for a composerless parent's inbox to be discarded once it ended, \
             still had {} report(s)",
            reports.len()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

// =====================================================================
// A composer without a probe: the daemon may type into it, but cannot
// judge whether a line it wrote showed there. Every judged write -- one
// terminated send_message line, a handoff, a chat post, an intro or a
// notice -- is refused at the same points a missing composer is, while a
// write with nothing to judge still goes through.
// =====================================================================

fn probe_char() -> char {
    SUBMIT_CHILD_PROBE
        .chars()
        .next()
        .expect("the probe is one character")
}

async fn send_text(client: &mut Connection, window_id: &str, text: &str) -> DaemonMessage {
    send(
        client,
        &ClientMessage::SendText {
            window_id: window_id.to_string(),
            text: text.to_string(),
        },
    )
    .await;
    read_response(client, Duration::from_secs(5)).await
}

#[tokio::test]
async fn a_judged_line_is_declined_before_writing_into_a_composer_without_a_probe_while_a_raw_body_goes_through()
 {
    let h = start("no-probe-entry").await;
    let window_id = open_ready(&h, RAW_ONLY).await;
    let entry = h.registry.get(&window_id).expect("raw-only window");
    let seq_before: u64 = entry.write_seq();

    let judged: SubmitJudgment = entry
        .submit_message("raw-only-judged-body", None, None)
        .await
        .expect("a declined write is not a PTY error");
    assert_eq!(
        judged.placement,
        SubmitPlacement::NotPlaced(NotPlaced {
            envelope_remains: false,
            reason: NotPlacedReason::NoComposer,
        }),
        "a line whose showing cannot be judged must be declined for that before anything is \
         read or written"
    );
    assert_eq!(judged.residue_write_seq, None);
    assert_eq!(
        entry.write_seq(),
        seq_before,
        "the declined line must not have taken a single write, a probe included"
    );

    let raw: SubmitJudgment = entry
        .submit_raw_gated(&plan_raw_write("raw-only-raw-body"), None)
        .await
        .expect("the raw write must not fail");
    assert!(
        matches!(raw.placement, SubmitPlacement::Placed(_)),
        "a body with nothing to judge must still be written into a composer without a probe, \
         got {:?}",
        raw.placement
    );
    h.wait_for_scrollback(&window_id, b"raw-only-raw-body", PRESENCE_TIMEOUT)
        .await;
    // Written after the declined call returned: anything that call had written
    // would already be in the child's output ahead of this body.
    assert!(
        !scrollback_contains(&h, &window_id, "raw-only-judged-body"),
        "the declined line must never reach the child"
    );

    h.registry.terminate_all().await;
}

#[tokio::test]
async fn a_one_line_send_message_to_a_composer_without_a_probe_is_refused_before_any_approval() {
    let h = start_with_base("no-probe-line", approver_base_config()).await;
    let raw_id = open_ready(&h, RAW_ONLY).await;
    let drawn_id = open_ready(&h, DRAWN).await;
    let raw = h.registry.get(&raw_id).expect("raw-only window");
    assert_eq!(raw.detection_definition(), Some(RAW_ONLY));
    assert_eq!(
        raw.detection_warning(),
        None,
        "a definition without a probe must still load: only the writes it cannot judge are \
         refused"
    );
    let seq_before: u64 = raw.write_seq();

    // No approver yet: a request the gate let through to approval would be
    // refused for the missing approver instead.
    let mut external = h.connect().await;
    let message = expect_error(
        send_text(&mut external, &raw_id, "hello\r").await,
        "no_composer",
    );
    assert_no_composer_guidance(&message, "a one-line send_message without a probe");

    let mut approver = h.connect().await;
    assert!(register_approver(&mut approver).await.is_empty());
    expect_error(
        send_text(&mut external, &raw_id, "hello\r").await,
        "no_composer",
    );

    // Control: the same line to a window with a probe is put before a human,
    // so this playbook does queue such a request.
    let mut other = h.connect().await;
    send(
        &mut other,
        &ClientMessage::SendText {
            window_id: drawn_id.clone(),
            text: "hello\r".to_string(),
        },
    )
    .await;
    let request = expect_approval_requested(&mut approver).await;
    assert_eq!(
        request.target.map(|target| target.window_id),
        Some(drawn_id.clone())
    );

    let mut late = h.connect().await;
    let pending: Vec<Option<String>> = register_approver(&mut late)
        .await
        .into_iter()
        .map(|request| request.target.map(|target| target.window_id))
        .collect();
    assert_eq!(
        pending,
        vec![Some(drawn_id.clone())],
        "the refused lines must not have left a request pending"
    );
    assert_eq!(
        raw.write_seq(),
        seq_before,
        "a refused line must not have written anything"
    );

    h.registry.terminate_all().await;
}

#[tokio::test]
async fn a_send_message_with_no_line_to_judge_is_written_into_a_composer_without_a_probe() {
    let h = start("no-probe-raw").await;
    let window_id = open_ready(&h, RAW_ONLY).await;
    let mut owner = h.connect().await;
    identify(&mut owner, &window_id).await;

    for (form, text) in [
        ("no terminator", "raw-only-unterminated"),
        ("a bare terminator", "\r"),
    ] {
        match send_text(&mut owner, &window_id, text).await {
            DaemonMessage::TextSent { window_id: sent_to } => assert_eq!(sent_to, window_id),
            other => panic!(
                "a send_message with {form} judges nothing on screen and must be written into \
                 a composer without a probe, got {other:?}"
            ),
        }
    }
    h.wait_for_scrollback(&window_id, b"raw-only-unterminated", PRESENCE_TIMEOUT)
        .await;

    h.registry.terminate_all().await;
}

#[tokio::test]
async fn send_handoff_to_a_composer_without_a_probe_is_refused_with_no_composer_even_when_approved()
{
    let h = start_with_base(
        "no-probe-handoff",
        DaemonConfig {
            approval_policy: Some(policy(&["send_handoff", "open_window"])),
            ..common::opening_config()
        },
    )
    .await;
    let window_id = open_ready(&h, RAW_ONLY).await;
    let entry = h.registry.get(&window_id).expect("raw-only window");
    let seq_before: u64 = entry.write_seq();
    let mut client = h.connect().await;

    send(
        &mut client,
        &ClientMessage::SendHandoff {
            window_id: Some(window_id.clone()),
            profile: None,
            cwd: None,
            name: None,
            model: None,
            effort: None,
            reference: "src/foo.rs".to_string(),
            note: "please look at the error handling".to_string(),
        },
    )
    .await;
    let message = expect_error(
        read_response(&mut client, LAUNCH_REPLY_TIMEOUT).await,
        "no_composer",
    );
    assert_no_composer_guidance(&message, "a window-addressed handoff without a probe");
    assert_eq!(
        entry.write_seq(),
        seq_before,
        "a refused handoff must not have written anything"
    );

    h.registry.terminate_all().await;
}

#[tokio::test]
async fn send_handoff_to_a_profile_whose_definition_names_no_probe_starts_no_child() {
    let (h, _dir) = start_launch(
        "no-probe-pre-open-handoff",
        None,
        Some(policy(&["send_handoff"])),
    )
    .await;
    let mut client = h.connect().await;

    let before: usize = window_count(&mut client).await;
    let msg = call_send_handoff_from_profile_within(
        &mut client,
        "raw-profile",
        None,
        None,
        "r",
        "t",
        LAUNCH_REPLY_TIMEOUT,
    )
    .await;
    let message = expect_error(msg, "no_composer");
    assert_no_composer_guidance(&message, "a profile-addressed handoff without a probe");
    assert_eq!(
        window_count(&mut client).await,
        before,
        "a handoff known to be refused must not have opened a window for it"
    );

    h.registry.terminate_all().await;
}

/// Reads the next reply, skipping any `chat_changed` push that overtakes it.
async fn read_chat_reply(client: &mut Connection) -> DaemonMessage {
    loop {
        let frame = read_response(client, Duration::from_secs(5)).await;
        if !matches!(frame, DaemonMessage::ChatChanged { .. }) {
            return frame;
        }
    }
}

/// A chat-enabled daemon over this file's definitions, with nothing excluded
/// from the rooms so the composer access is the only thing that can keep a
/// window out of them.
async fn start_chat(tag: &str, intro: Option<&str>) -> Harness {
    let dir = unique_temp_dir(tag);
    write_definitions(&dir);
    Harness::start_with_chat(
        tag,
        chat_config(
            &dir,
            &ChatPlaybook {
                exclude_detections: Some(&[]),
                intro,
                ..ChatPlaybook::default()
            },
        ),
    )
    .await
}

#[tokio::test]
async fn a_window_with_a_composer_but_no_probe_is_never_a_chat_participant() {
    let h = start_chat("no-probe-chat", None).await;
    let mut client = h.connect().await;
    let raw_id = h.open(echo_child_command(), Some(RAW_ONLY)).await;
    let drawn_id = h.open(echo_child_command(), Some(DRAWN)).await;
    let poster_id = h.open_idle_window(&mut client).await;
    identify(&mut client, &poster_id).await;

    send(
        &mut client,
        &ClientMessage::SendChat {
            room_id: None,
            body: format!("hey @{raw_id} and @{drawn_id}"),
        },
    )
    .await;
    match read_chat_reply(&mut client).await {
        DaemonMessage::ChatPosted {
            targets,
            unresolved,
            ..
        } => {
            assert_eq!(
                targets,
                vec![drawn_id.clone()],
                "only the mention whose window can judge a delivered line may resolve"
            );
            assert_eq!(unresolved, vec![raw_id.clone()]);
        }
        other => panic!("expected chat_posted, got {other:?}"),
    }

    send(
        &mut client,
        &ClientMessage::ReadChat {
            room_id: None,
            after_seq: Some(0),
            limit: None,
        },
    )
    .await;
    match read_chat_reply(&mut client).await {
        DaemonMessage::ChatMessages { roster, .. } => {
            assert!(
                !roster.iter().any(|p| p.window_id == raw_id),
                "a window with a composer but no probe must not appear in the roster, got \
                 {roster:?}"
            );
            assert!(roster.iter().any(|p| p.window_id == drawn_id));
        }
        other => panic!("expected chat_messages, got {other:?}"),
    }

    h.registry.terminate_all().await;
}

#[tokio::test]
async fn open_task_window_refuses_an_instructed_task_whose_profile_names_no_probe_before_starting_a_child()
 {
    let h = start_ledger("no-probe-pre-open-task").await;
    let mut owner = h.connect().await;
    become_plan_owner(&h, &mut owner).await;
    let plan = create_plan(
        &mut owner,
        vec![
            task_spec("instructed", "raw-profile", Some("do the thing")),
            task_spec("bare", "raw-profile", None),
        ],
    )
    .await;

    let before: usize = window_count(&mut owner).await;
    let msg = open_task_window(&mut owner, &plan.plan_id, "instructed", "instructed-window").await;
    let message = expect_error(msg, "no_composer");
    assert_no_composer_guidance(&message, "an instructed task window without a probe");
    assert_eq!(
        window_count(&mut owner).await,
        before,
        "an instructed task whose instruction could never be judged delivered must not start a \
         child"
    );
    let refused = stored_task(&mut owner, &plan.plan_id, "instructed").await;
    assert_eq!(refused.window_id, None);
    assert_eq!(refused.instruction_notice, None);

    let bare = open_task_window(&mut owner, &plan.plan_id, "bare", "bare-window").await;
    let bare_window: String = expect_task_window_opened(
        bare,
        "the same profile with nothing to tell the window must open",
    );
    assert_eq!(
        stored_task(&mut owner, &plan.plan_id, "bare")
            .await
            .window_id,
        Some(bare_window),
        "the control must be bound, or the refusal above may be any failure to open"
    );

    h.registry.terminate_all().await;
}

#[tokio::test]
async fn the_no_composer_refusal_tells_that_a_definition_naming_a_probe_is_written_to() {
    let h = start("no-composer-probe-guidance").await;
    let silent_id = open_ready(&h, SILENT).await;
    let raw_id = open_ready(&h, RAW_ONLY).await;
    let mut silent_owner = h.connect().await;
    identify(&mut silent_owner, &silent_id).await;
    let mut raw_owner = h.connect().await;
    identify(&mut raw_owner, &raw_id).await;

    let silent_message = expect_error(
        send_text(&mut silent_owner, &silent_id, "hello\r").await,
        "no_composer",
    );
    assert!(
        silent_message.contains("probe"),
        "the refusal must tell that a definition naming a probe is written to, got \
         {silent_message:?}"
    );
    assert_no_composer_guidance(&silent_message, "the refusal naming the probe");

    let raw_message = expect_error(
        send_text(&mut raw_owner, &raw_id, "hello\r").await,
        "no_composer",
    );
    assert_eq!(
        raw_message, silent_message,
        "a missing composer and a missing probe are refused with one wording"
    );

    h.registry.terminate_all().await;
}
