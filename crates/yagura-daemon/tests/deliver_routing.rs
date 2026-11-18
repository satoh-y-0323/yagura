#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: what the daemon sends of its own accord -- a chat post, the intro, a
//! handoff, a task, plan or report notice -- reaches a window whose profile declares a
//! `[delivery]` table through that CLI's own doors (start-up, the start of a turn, a
//! registered wait, the profile's wake command, the launch prompt), and is never typed into
//! its composer. Typing remains only for `send_message` and for a profile that declares no
//! way to wake a waiting CLI, while that CLI waits.
//!
//! Every window is a `hook_child`: it calls the real `yagura-daemon` doors from inside the
//! window, as a CLI's hooks do, and logs what each door printed, every argument it was
//! launched with and every byte typed into it. Its detection definition declares a composer
//! and a probe, so the daemon could type into it: "nothing was typed" is a result, not a
//! property of the fixture.
//!
//! The daemon runs in this process on an instance endpoint of this test's own, so the doors
//! reach it and never a daemon of the user's.

use crate::common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

#[cfg(windows)]
use common::call_list_windows;
use common::{
    VALID_TOKEN, approver_base_config, composer_section_toml, detection_config, empty_lookup,
    identify, read_response, register_approver, send, unique_temp_dir, wait_until_status,
    write_profile,
};
use yagura_daemon::profile::ProfileOs;
use yagura_daemon::{IpcServer, OpenWindowRequest, WindowRegistry};
use yagura_protocol::{
    ChatMessage, ClientMessage, DaemonMessage, InstructionNoticeState, PlanInfo, PlanTaskSpec,
    ReportOutcome, TaskReportMark, WindowStatus,
};
use yagura_test_support::ipc::Connection;
use yagura_test_support::process::{
    COMPOSER_CONTINUATION_PREFIX, COMPOSER_MARKER, COMPOSER_SEPARATOR, echo_child_command,
};

/// The detection definition every `hook_child` window is opened under.
const HOOK_DETECTION: &str = "hook-child";

/// The playbook's `[chat] intro`.
const INTRO: &str = "INTRO-7Q-STARTUP-TEXT";

/// The session id a command-waking fixture reports.
const SESSION: &str = "sess-hook-7";

/// The fixed task notice, built from its wording rather than taken from the product.
fn expected_task_notice(plan_id: &str, task_id: &str) -> String {
    format!(
        "[yagura task] Task {task_id} of plan {plan_id} is assigned to this window. Call \
         get_plan with plan_id \"{plan_id}\" and task_id \"{task_id}\" to read its instruction."
    )
}

fn expected_plan_notice(plan_id: &str) -> String {
    format!("[yagura plan] Plan {plan_id} was approved. You can open its tasks now.")
}

fn expected_report_notice(pending: usize) -> String {
    format!(
        "[yagura report] {pending} pending report(s). Call wait_for_report with timeout_sec: 0 to read them."
    )
}

/// Every chat delivery state a profile that declares `[delivery]` never shows: they
/// describe what the composer did, and nothing is typed there.
const COMPOSER_STATES: [&str; 4] = ["confirmed", "unconfirmed", "not_observed", "not_written"];

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn hook_child_exe() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_hook_child"))
}

fn daemon_exe() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_yagura-daemon"))
}

#[cfg(windows)]
fn toml_path(path: &Path) -> String {
    format!("'{}'", path.display())
}

// --- the daemon --------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct Opts<'a> {
    intro: &'a str,
    /// Lines under the playbook's `[limits]`.
    limits: &'a str,
    plan_review: bool,
    /// `None` keeps the daemon's own bound on a connection's outstanding handoffs.
    handoffs_per_conn: Option<usize>,
}

impl Default for Opts<'_> {
    fn default() -> Self {
        Self {
            intro: INTRO,
            limits: "",
            plan_review: false,
            handoffs_per_conn: None,
        }
    }
}

fn playbook_toml(opts: &Opts<'_>) -> String {
    format!(
        "[approval]\nauto_approve = [\"send_chat\", \"broadcast_chat\", \"send_handoff\", \
         \"open_window\", \"send_message\", \"close_window\"]\n\n[chat]\nintro = \"{}\"\n\n\
         [limits]\n{}\n",
        opts.intro, opts.limits
    )
}

/// A chat-, plan- and handoff-enabled daemon built the way `main.rs` builds one, on the
/// endpoint of an instance of this test's own.
struct Scene {
    registry: Arc<WindowRegistry>,
    endpoint: String,
    instance: String,
    root: PathBuf,
    bases: PathBuf,
    _server: tokio::task::JoinHandle<()>,
}

impl Scene {
    async fn start(tag: &str, opts: Opts<'_>) -> Self {
        let root = unique_temp_dir(&format!("deliver-routing-{tag}"));
        let instance = format!(
            "dr{:x}{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        );
        let bases = root.join("bases");
        std::fs::create_dir_all(root.join("profiles")).expect("create the profiles directory");
        std::fs::create_dir_all(&bases).expect("create the base directories");

        let source = playbook_toml(&opts);
        let policy = yagura_daemon::control::parse_policy(&source).unwrap_or_else(|err| {
            panic!(
                "the playbook must load ({err}); every key in it is part of the contract:\n{source}"
            )
        });
        let mut config = detection_config(&root, approver_base_config());
        yagura_daemon::control::apply_limits(&mut config, policy.limits());
        yagura_daemon::control::apply_chat_section(&mut config, policy.chat.as_ref());
        config.approval_policy = Some(policy);
        config.playbook = Some(root.join("playbook.toml"));
        config.store_db_path_resolved = Some(root.join("yagura.db"));
        config.profiles_dir = Some(root.join("profiles"));
        config.host_os = ProfileOs::host();
        // No PATH of the machine's, but its own command interpreter, which a `.cmd` needs.
        config.program_lookup = yagura_os::program::ProgramLookup {
            windows_comspec: yagura_os::program::ProgramLookup::from_env().windows_comspec,
            ..empty_lookup()
        };
        config.program_is_executable = |path| path.is_file();
        config.default_shell = None;
        config.plan_approval_required = opts.plan_review;
        if let Some(bound) = opts.handoffs_per_conn {
            config.max_open_handoffs_per_caller = bound;
        }
        write_hook_definition(&root);

        let user = yagura_os::PeerAuth::current_user_id(&yagura_os::PlatformIpc::default())
            .expect("the current user id must resolve");
        let namespace = yagura_os::resolve_instance_from(Some(std::ffi::OsStr::new(&instance)))
            .expect("the instance identifier must be valid");
        let endpoint = yagura_os::default_endpoint_owning(&user, &namespace);

        let registry = Arc::new(WindowRegistry::new(config.clone()));
        yagura_daemon::store::wire_into(&registry, Some(root.join("yagura.db")));
        let chat = {
            let config = config.clone();
            Arc::new(
                tokio::task::spawn_blocking(move || {
                    yagura_daemon::ipc::chat::ChatRuntime::from_config(&config, &[])
                })
                .await
                .expect("ChatRuntime::from_config must not panic"),
            )
        };
        yagura_daemon::ipc::chat::install_intro_hook(&registry, chat.clone());
        let ledger = Arc::new(
            tokio::task::spawn_blocking({
                let config = config.clone();
                move || yagura_daemon::ipc::plan::PlanRuntime::from_config(&config, &[])
            })
            .await
            .expect("PlanRuntime::from_config must not panic"),
        );
        let server = Arc::new(
            IpcServer::new_with_chat(registry.clone(), endpoint.clone(), config, chat)
                .with_ledger(ledger),
        );
        let handle = tokio::spawn(
            yagura_test_support::ipc::InstanceEndpoint::new(&instance).released_after(async move {
                let _ = server.run().await;
            }),
        );
        Self {
            registry,
            endpoint,
            instance,
            root,
            bases,
            _server: handle,
        }
    }

    async fn connect(&self) -> Connection {
        yagura_test_support::ipc::connect_with_retry(&self.endpoint).await
    }

    /// A connection registered as an approver: it posts as the user, to `main`, which every
    /// participant belongs to.
    async fn user(&self) -> Connection {
        let mut client = self.connect().await;
        register_approver(&mut client).await;
        client
    }

    fn entry(&self, window_id: &str) -> Arc<yagura_daemon::WindowEntry> {
        self.registry.get(window_id).expect("the window exists")
    }

    async fn type_line(&self, window_id: &str, line: &str) {
        self.entry(window_id)
            .session()
            .write_input(format!("{line}\r").as_bytes())
            .await
            .expect("write into the window");
    }

    /// The `hook_child` argv for a window whose logs go to `logs`.
    fn hook_command(&self, logs: &Path, timing: &str, flags: &[&str]) -> Vec<String> {
        std::fs::create_dir_all(logs).expect("create the log directory");
        let mut command = vec![
            hook_child_exe().display().to_string(),
            COMPOSER_MARKER.to_string(),
            COMPOSER_SEPARATOR.to_string(),
            COMPOSER_CONTINUATION_PREFIX.to_string(),
            timing.to_string(),
            "-".to_string(),
            "--daemon".to_string(),
            daemon_exe().display().to_string(),
            "--instance".to_string(),
            self.instance.clone(),
            "--bases".to_string(),
            self.bases.display().to_string(),
            "--log".to_string(),
            logs.display().to_string(),
        ];
        command.extend(flags.iter().map(|flag| (*flag).to_string()));
        command
    }

    /// Writes profile `id`: a `hook_child` launched with `flags`, and `delivery` as its
    /// whole `[delivery]` table (`None` writes no table). Returns where its window logs.
    fn profile(&self, id: &str, flags: &[&str], delivery: Option<&str>) -> Logs {
        self.profile_with(id, "0", flags, delivery, "")
    }

    fn profile_with(
        &self,
        id: &str,
        timing: &str,
        flags: &[&str],
        delivery: Option<&str>,
        extra_tables: &str,
    ) -> Logs {
        let logs = self.root.join("logs").join(id);
        let command = self
            .hook_command(&logs, timing, flags)
            .iter()
            .map(|arg| format!("'{arg}'"))
            .collect::<Vec<_>>()
            .join(", ");
        let delivery =
            delivery.map_or_else(String::new, |table| format!("\n[delivery]\n{table}\n"));
        write_profile(
            &self.root.join("profiles"),
            id,
            &format!(
                "schema = 1\ndisplay_name = \"{id}\"\ndetection = \"{HOOK_DETECTION}\"\n\n\
                 [command]\ndefault = [{command}]\n{delivery}{extra_tables}"
            ),
        );
        Logs { dir: logs }
    }

    /// Opens a window from `profile` and returns it once its screen waits for input.
    async fn open_profile(&self, client: &mut Connection, profile: &str) -> String {
        send(
            client,
            &ClientMessage::OpenWindowFromProfile {
                profile: profile.to_string(),
                cwd: None,
                name: None,
                model: None,
                effort: None,
                on_name_taken: Default::default(),
            },
        )
        .await;
        let id = match read_reply(client).await {
            DaemonMessage::WindowOpened { window_id, .. } => window_id,
            other => panic!("expected window_opened for {profile}, got {other:?}"),
        };
        wait_until_status(client, &id, WindowStatus::WaitingInput).await;
        id
    }

    /// Opens a `hook_child` window from a raw argv, the way a restore or `open_window` does.
    async fn open_argv(&self, client: &mut Connection, logs: &Path, flags: &[&str]) -> String {
        let id = self
            .registry
            .open(OpenWindowRequest {
                command: self.hook_command(logs, "0", flags),
                cwd: None,
                name: None,
                vendor: None,
                detection: Some(HOOK_DETECTION.to_string()),
                on_name_taken: Default::default(),
            })
            .await
            .expect("open the window")
            .window_id;
        wait_until_status(client, &id, WindowStatus::WaitingInput).await;
        id
    }

    async fn start_turn(&self, client: &mut Connection, logs: &Logs, window_id: &str) {
        let before = logs.count_events("turn_started");
        self.type_line(window_id, "TURN").await;
        logs.until("the turn to start", |logs| {
            logs.count_events("turn_started") > before
        })
        .await;
        wait_until_status(client, window_id, WindowStatus::Running).await;
    }

    async fn end_turn(&self, client: &mut Connection, logs: &Logs, window_id: &str) {
        let before = logs.count_events("turn_ended");
        self.type_line(window_id, "END").await;
        logs.until("the turn to end", |logs| {
            logs.count_events("turn_ended") > before
        })
        .await;
        wait_until_status(client, window_id, WindowStatus::WaitingInput).await;
    }

    async fn until(&self, what: &str, mut condition: impl FnMut() -> bool) {
        let deadline = tokio::time::Instant::now() + yagura_test_support::HANG_GUARD;
        while !condition() {
            assert!(
                tokio::time::Instant::now() < deadline,
                "timed out waiting for {what}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    async fn until_waiting(&self, window_id: &str) {
        self.until("the window's wait to be registered", || {
            self.entry(window_id).inbox().has_waiter()
        })
        .await;
    }
}

/// The definition `hook_child` is read under: its status line says waiting or running,
/// and it declares the composer it paints and the probe a typed delivery needs.
fn write_hook_definition(dir: &Path) {
    std::fs::write(
        dir.join(format!("{HOOK_DETECTION}.toml")),
        format!(
            "schema_version = 1\nname = \"{HOOK_DETECTION}\"\nbottom_line_count = 40\n\n\
             [debounce]\nconfirm_ms = 30\n\n\
             [[rules]]\nstatus = \"waiting_input\"\npattern = 'HC-IDLE'\npriority = 10\n\
             region = \"bottom\"\n\n\
             [[rules]]\nstatus = \"running\"\npattern = 'HC-BUSY'\npriority = 10\n\
             region = \"bottom\"\n{}",
            composer_section_toml()
        ),
    )
    .expect("write the hook_child definition");
}

/// The `[delivery]` table of a profile whose CLI leaves a wait registered.
const WAIT_DELIVERY: &str = "wake = \"wait\"\nlaunch_prompt = [\"--prompt\", \"{prompt}\"]";
/// ... of a profile whose CLI is woken by running its own program.
const COMMAND_DELIVERY: &str = "wake = \"command\"\nwake_args = [\"--woken\", \"{session}\", \
                                \"{text}\"]\nlaunch_prompt = [\"--prompt\", \"{prompt}\"]";
/// ... of a profile with no way to wake its CLI while it waits.
const NONE_DELIVERY: &str = "wake = \"none\"\nlaunch_prompt = [\"--prompt\", \"{prompt}\"]";

/// The hooks a CLI that leaves a wait registered runs (Claude Code's shape).
const WAIT_FLAGS: &[&str] = &["--context", "--take", "--wait", "--emit"];
/// The hooks a command-woken CLI runs (Codex's shape: the start-up context arrives with
/// the first turn, and every turn start reports the session).
const COMMAND_FLAGS: &[&str] = &[
    "--context-first-turn",
    "--take",
    "--session",
    SESSION,
    "--emit",
];
/// The hooks a CLI with no wake door runs (`wake = "none"`).
const NONE_FLAGS: &[&str] = &["--context-first-turn", "--take", "--emit"];

// --- what reached the CLI ---------------------------------------------------------------

struct Logs {
    dir: PathBuf,
}

impl Logs {
    fn lines(&self, file: &str) -> Vec<serde_json::Value> {
        std::fs::read_to_string(self.dir.join(file))
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    }

    fn events(&self) -> Vec<serde_json::Value> {
        self.lines("events.jsonl")
    }

    fn woken(&self) -> Vec<serde_json::Value> {
        self.lines("woken.jsonl")
    }

    fn count_events(&self, event: &str) -> usize {
        self.events()
            .iter()
            .filter(|line| line["event"] == event)
            .count()
    }

    /// Everything typed into the window, byte for byte.
    fn typed(&self) -> String {
        String::from_utf8_lossy(&std::fs::read(self.dir.join("input.log")).unwrap_or_default())
            .into_owned()
    }

    /// What door `event` printed, on either stream, every run of it together.
    fn door(&self, event: &str) -> String {
        self.events()
            .iter()
            .filter(|line| line["event"] == event)
            .map(|line| {
                format!(
                    "{}{}",
                    line["stdout"].as_str().unwrap_or_default(),
                    line["stderr"].as_str().unwrap_or_default()
                )
            })
            .collect()
    }

    /// The texts the profile's wake command was run with.
    fn woken_texts(&self) -> String {
        self.woken()
            .iter()
            .filter(|line| line["event"] == "woken")
            .map(|line| line["text"].as_str().unwrap_or_default().to_string())
            .collect()
    }

    /// How many times `needle` reached the CLI through any door or the wake command.
    fn received(&self, needle: &str) -> usize {
        ["context", "take", "wait"]
            .iter()
            .map(|door| self.door(door).matches(needle).count())
            .sum::<usize>()
            + self.woken_texts().matches(needle).count()
    }

    fn argv(&self) -> Vec<String> {
        self.events()
            .iter()
            .find(|line| line["event"] == "argv")
            .and_then(|line| line["args"].as_array().cloned())
            .unwrap_or_default()
            .into_iter()
            .filter_map(|arg| arg.as_str().map(str::to_string))
            .collect()
    }

    fn prompt(&self) -> Option<String> {
        self.events()
            .iter()
            .find(|line| line["event"] == "argv")
            .and_then(|line| line["prompt"].as_str().map(str::to_string))
    }

    async fn until(&self, what: &str, mut condition: impl FnMut(&Self) -> bool) {
        let deadline = tokio::time::Instant::now() + yagura_test_support::HANG_GUARD;
        while !condition(self) {
            assert!(
                tokio::time::Instant::now() < deadline,
                "timed out waiting for {what}; events: {:?}; woken: {:?}; typed: {:?}",
                self.events(),
                self.woken(),
                self.typed()
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    async fn until_received(&self, needle: &str) {
        self.until(&format!("{needle:?} to reach the CLI"), |logs| {
            logs.received(needle) > 0
        })
        .await;
    }

    fn assert_never_typed(&self, needle: &str) {
        assert!(
            !self.typed().contains(needle),
            "{needle:?} was typed into the composer: {:?}",
            self.typed()
        );
    }
}

// --- wire round trips -------------------------------------------------------------------

/// Reads the next response, skipping the pushes an approver connection is sent.
async fn read_reply(client: &mut Connection) -> DaemonMessage {
    loop {
        match read_response(client).await {
            DaemonMessage::ChatChanged { .. }
            | DaemonMessage::PlanChanged { .. }
            | DaemonMessage::ApprovalRequested { .. } => continue,
            other => return other,
        }
    }
}

async fn request(client: &mut Connection, message: ClientMessage) -> DaemonMessage {
    send(client, &message).await;
    read_reply(client).await
}

/// Posts `body` to `main` and returns its sequence number and resolved targets.
async fn post(client: &mut Connection, body: &str) -> (u64, Vec<String>) {
    match request(
        client,
        ClientMessage::SendChat {
            room_id: None,
            body: body.to_string(),
        },
    )
    .await
    {
        DaemonMessage::ChatPosted { seq, targets, .. } => (seq, targets),
        other => panic!("expected chat_posted, got {other:?}"),
    }
}

async fn read_room(client: &mut Connection) -> Vec<ChatMessage> {
    match request(
        client,
        ClientMessage::ReadChat {
            room_id: None,
            after_seq: Some(0),
            limit: None,
        },
    )
    .await
    {
        DaemonMessage::ChatMessages { messages, .. } => messages,
        other => panic!("expected chat_messages, got {other:?}"),
    }
}

/// The recorded delivery state of message `seq` to `window_id`, if one is recorded.
async fn delivery_state(client: &mut Connection, seq: u64, window_id: &str) -> Option<String> {
    read_room(client)
        .await
        .into_iter()
        .find(|message| message.seq == seq)
        .and_then(|message| {
            message
                .deliveries
                .into_iter()
                .find(|delivery| delivery.window_id == window_id)
        })
        .map(|delivery| delivery.state)
}

async fn until_state(client: &mut Connection, seq: u64, window_id: &str, state: &str) {
    let deadline = tokio::time::Instant::now() + yagura_test_support::HANG_GUARD;
    loop {
        let seen = delivery_state(client, seq, window_id).await;
        if seen.as_deref() == Some(state) {
            return;
        }
        if let Some(seen) = seen.as_deref() {
            assert!(
                !COMPOSER_STATES.contains(&seen),
                "a window whose profile declares [delivery] reports what its composer did: \
                 {seen:?}"
            );
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for message {seq} to {window_id} to become {state:?} (last \
             seen {seen:?})"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn call_take(client: &mut Connection, window_id: &str) -> String {
    let (output, _, _) = common::expect_delivery(
        common::call_door(
            client,
            &ClientMessage::DoorRead {
                window_id: window_id.to_string(),
                token: VALID_TOKEN.to_string(),
                door: yagura_protocol::Door::Take,
            },
        )
        .await,
    );
    // A door says it printed once it has, which ends what it took.
    common::send(client, &ClientMessage::DeliverPrinted).await;
    output
}

fn handoff_to(window_id: &str, note: &str) -> ClientMessage {
    ClientMessage::SendHandoff {
        window_id: Some(window_id.to_string()),
        profile: None,
        cwd: None,
        name: None,
        model: None,
        effort: None,
        reference: "docs/handoff.md".to_string(),
        note: note.to_string(),
    }
}

/// `HandoffSent`'s `(window_id, opened, delivery)`, the delivery as its wire string.
fn expect_handoff_sent(message: DaemonMessage) -> (String, bool, String) {
    match message {
        DaemonMessage::HandoffSent {
            window_id,
            opened,
            state: delivery,
            ..
        } => (
            window_id,
            opened,
            serde_json::to_value(delivery)
                .expect("a delivery encodes")
                .as_str()
                .expect("a delivery is a string on the wire")
                .to_string(),
        ),
        other => panic!("expected handoff_sent, got {other:?}"),
    }
}

// =========================================================================================
// a chat post reaches the CLI through its doors, once, and is never typed
// =========================================================================================

#[tokio::test]
async fn a_post_to_a_waiting_window_that_waits_for_the_daemon_arrives_through_the_wait() {
    let scene = Scene::start("wait-idle", Opts::default()).await;
    let logs = scene.profile("hc-wait", WAIT_FLAGS, Some(WAIT_DELIVERY));
    let mut user = scene.user().await;
    let id = scene.open_profile(&mut user, "hc-wait").await;
    scene.until_waiting(&id).await;

    let (seq, targets) = post(&mut user, &format!("@{id} R1WAITIDLE")).await;
    assert_eq!(targets, vec![id.clone()]);

    logs.until_received("R1WAITIDLE").await;
    until_state(&mut user, seq, &id, "delivered").await;
    let wait = logs
        .events()
        .into_iter()
        .find(|line| {
            line["event"] == "wait"
                && line["stderr"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("R1WAITIDLE")
        })
        .expect("the wait printed the post on the stream the profile declares");
    assert_eq!(
        wait["code"], 2,
        "the profile's wake exit code ends the wait"
    );
    assert_eq!(logs.received("R1WAITIDLE"), 1);
    logs.assert_never_typed("R1WAITIDLE");
}

#[tokio::test]
async fn a_post_to_a_working_window_that_waits_for_the_daemon_arrives_through_the_wait() {
    let scene = Scene::start("wait-working", Opts::default()).await;
    let logs = scene.profile("hc-wait", WAIT_FLAGS, Some(WAIT_DELIVERY));
    let mut user = scene.user().await;
    let id = scene.open_profile(&mut user, "hc-wait").await;
    scene.start_turn(&mut user, &logs, &id).await;
    scene.until_waiting(&id).await;

    let (seq, _) = post(&mut user, &format!("@{id} R1WAITWORKING")).await;

    logs.until_received("R1WAITWORKING").await;
    until_state(&mut user, seq, &id, "delivered").await;
    assert!(logs.door("wait").contains("R1WAITWORKING"));
    assert_eq!(logs.received("R1WAITWORKING"), 1);
    logs.assert_never_typed("R1WAITWORKING");
}

#[tokio::test]
async fn a_post_to_a_waiting_window_woken_by_command_runs_the_command_with_its_session() {
    let scene = Scene::start("command-idle", Opts::default()).await;
    let logs = scene.profile("hc-command", COMMAND_FLAGS, Some(COMMAND_DELIVERY));
    let mut user = scene.user().await;
    let id = scene.open_profile(&mut user, "hc-command").await;
    // The first turn tells the daemon the session the command needs.
    scene.start_turn(&mut user, &logs, &id).await;
    scene.end_turn(&mut user, &logs, &id).await;
    assert_eq!(scene.entry(&id).session_id().as_deref(), Some(SESSION));

    let (seq, _) = post(&mut user, &format!("@{id} R1COMMANDIDLE")).await;

    logs.until_received("R1COMMANDIDLE").await;
    until_state(&mut user, seq, &id, "delivered").await;
    let woken = logs.woken();
    let run = woken
        .iter()
        .find(|line| line["event"] == "woken")
        .expect("the wake command ran");
    assert_eq!(run["session"], SESSION);
    assert_eq!(logs.received("R1COMMANDIDLE"), 1);
    logs.assert_never_typed("R1COMMANDIDLE");
}

#[tokio::test]
async fn a_post_to_a_working_window_woken_by_command_runs_the_command_at_once() {
    let scene = Scene::start("command-working", Opts::default()).await;
    let logs = scene.profile("hc-command", COMMAND_FLAGS, Some(COMMAND_DELIVERY));
    let mut user = scene.user().await;
    let id = scene.open_profile(&mut user, "hc-command").await;
    scene.start_turn(&mut user, &logs, &id).await;

    let (seq, _) = post(&mut user, &format!("@{id} R1COMMANDWORKING")).await;

    logs.until_received("R1COMMANDWORKING").await;
    until_state(&mut user, seq, &id, "delivered").await;
    assert!(logs.woken_texts().contains("R1COMMANDWORKING"));
    assert_eq!(logs.received("R1COMMANDWORKING"), 1);
    logs.assert_never_typed("R1COMMANDWORKING");
}

#[tokio::test]
async fn a_post_to_a_working_window_with_no_wake_door_arrives_with_the_next_turn_start() {
    let scene = Scene::start("none-working", Opts::default()).await;
    let logs = scene.profile("hc-none", NONE_FLAGS, Some(NONE_DELIVERY));
    let mut user = scene.user().await;
    let id = scene.open_profile(&mut user, "hc-none").await;
    scene.start_turn(&mut user, &logs, &id).await;

    let (seq, _) = post(&mut user, &format!("@{id} R1NONEWORKING")).await;
    until_state(&mut user, seq, &id, "queued").await;
    scene.start_turn(&mut user, &logs, &id).await;

    logs.until_received("R1NONEWORKING").await;
    until_state(&mut user, seq, &id, "delivered").await;
    assert!(logs.door("take").contains("R1NONEWORKING"));
    assert_eq!(logs.received("R1NONEWORKING"), 1);
    logs.assert_never_typed("R1NONEWORKING");
}

#[tokio::test]
async fn a_post_waiting_for_a_session_is_pushed_by_command_once_the_session_is_first_known() {
    let scene = Scene::start("command-session", Opts::default()).await;
    // Reports its session at a turn's start but reads nothing there itself: what arrives
    // can only have come through the command.
    let logs = scene.profile(
        "hc-command-session",
        &["--session", SESSION],
        Some(COMMAND_DELIVERY),
    );
    let mut user = scene.user().await;
    let id = scene.open_profile(&mut user, "hc-command-session").await;
    let (seq, _) = post(&mut user, &format!("@{id} R1SESSIONKNOWN")).await;
    until_state(&mut user, seq, &id, "queued").await;

    scene.type_line(&id, "TURN").await;

    logs.until_received("R1SESSIONKNOWN").await;
    until_state(&mut user, seq, &id, "delivered").await;
    assert!(logs.woken_texts().contains("R1SESSIONKNOWN"));
    assert_eq!(logs.received("R1SESSIONKNOWN"), 1);
    logs.assert_never_typed("R1SESSIONKNOWN");
}

// =========================================================================================
// opening a window types nothing; the start-up door carries the intro
// =========================================================================================

#[tokio::test]
async fn opening_a_window_types_nothing_and_its_start_up_door_carries_the_intro() {
    let scene = Scene::start("intro", Opts::default()).await;
    let logs = scene.profile("hc-wait", WAIT_FLAGS, Some(WAIT_DELIVERY));
    let mut user = scene.user().await;
    let id = scene.open_profile(&mut user, "hc-wait").await;

    logs.until("the start-up door to answer", |logs| {
        logs.count_events("context") > 0
    })
    .await;
    assert!(
        logs.door("context").contains(INTRO),
        "the start-up door carries the intro: {:?}",
        logs.door("context")
    );
    // Anything the daemon queued for the window on its own lands before a later post.
    scene.until_waiting(&id).await;
    let (seq, _) = post(&mut user, &format!("@{id} R2AFTERINTRO")).await;
    logs.until_received("R2AFTERINTRO").await;
    until_state(&mut user, seq, &id, "delivered").await;

    logs.assert_never_typed(INTRO);
    assert_eq!(logs.received(INTRO), 1, "the intro reaches the CLI once");
}

// =========================================================================================
// a handoff, a plan approval and a report reach the CLI through its doors
// =========================================================================================

#[tokio::test]
async fn a_handoff_to_an_existing_window_arrives_through_its_wait_and_answers_taken() {
    let scene = Scene::start("handoff", Opts::default()).await;
    let logs = scene.profile("hc-wait", WAIT_FLAGS, Some(WAIT_DELIVERY));
    let mut user = scene.user().await;
    let id = scene.open_profile(&mut user, "hc-wait").await;
    scene.until_waiting(&id).await;

    let (window_id, opened, delivery) =
        expect_handoff_sent(request(&mut user, handoff_to(&id, "R3HANDOFF")).await);

    assert_eq!((window_id.as_str(), opened), (id.as_str(), false));
    assert_eq!(
        delivery, "delivered",
        "answered once the CLI took the envelope"
    );
    // The answer comes once the door is handed the envelope; the CLI logs it after.
    logs.until_received("R3HANDOFF").await;
    assert!(logs.door("wait").contains("R3HANDOFF"));
    assert_eq!(logs.received("R3HANDOFF"), 1);
    logs.assert_never_typed("R3HANDOFF");
}

#[tokio::test]
async fn a_plan_approval_reaches_its_owner_through_the_owners_wait() {
    let scene = Scene::start(
        "plan",
        Opts {
            plan_review: true,
            ..Opts::default()
        },
    )
    .await;
    let logs = scene.profile("hc-wait", WAIT_FLAGS, Some(WAIT_DELIVERY));
    let mut user = scene.user().await;
    let owner_id = scene.open_profile(&mut user, "hc-wait").await;
    scene.until_waiting(&owner_id).await;
    let mut owner = scene.connect().await;
    identify(&mut owner, &owner_id).await;
    let plan = create_plan(&mut owner, vec![task("t1", "hc-wait", None)]).await;

    match request(
        &mut user,
        ClientMessage::ApprovePlan {
            plan_id: plan.plan_id.clone(),
            version: plan.version,
        },
    )
    .await
    {
        DaemonMessage::PlanDetail { .. } => {}
        other => panic!("expected plan_detail, got {other:?}"),
    }

    let notice = expected_plan_notice(&plan.plan_id);
    logs.until_received(&notice).await;
    assert!(logs.door("wait").contains(&notice));
    assert_eq!(logs.received(&notice), 1);
    logs.assert_never_typed("[yagura plan]");
}

#[tokio::test]
async fn a_report_reaches_its_parent_through_the_parents_wait() {
    let scene = Scene::start("report", Opts::default()).await;
    let logs = scene.profile("hc-wait", WAIT_FLAGS, Some(WAIT_DELIVERY));
    let mut user = scene.user().await;
    let parent_id = scene.open_profile(&mut user, "hc-wait").await;
    scene.until_waiting(&parent_id).await;
    let mut parent = scene.connect().await;
    identify(&mut parent, &parent_id).await;
    let child_id = common::call_open_window(&mut parent, echo_child_command()).await;
    let mut child = scene.connect().await;
    identify(&mut child, &child_id).await;

    match request(
        &mut child,
        ClientMessage::SendReport {
            outcome: ReportOutcome::Succeeded,
            summary: "R3REPORTSUMMARY".to_string(),
            path: None,
        },
    )
    .await
    {
        DaemonMessage::ReportSent { .. } => {}
        other => panic!("expected report_sent, got {other:?}"),
    }

    let notice = expected_report_notice(1);
    logs.until_received(&notice).await;
    assert_eq!(logs.received(&notice), 1);
    assert_eq!(
        logs.received("R3REPORTSUMMARY"),
        0,
        "the notice carries the count only"
    );
    logs.assert_never_typed("[yagura report]");
}

// =========================================================================================
// a task window's notice is its launch prompt, kept out of what is stored
// =========================================================================================

fn task(task_id: &str, profile: &str, instruction: Option<&str>) -> PlanTaskSpec {
    PlanTaskSpec {
        task_id: task_id.to_string(),
        title: "do the thing".to_string(),
        role: None,
        profile: Some(profile.to_string()),
        model: None,
        effort: None,
        depends_on: Vec::new(),
        writes: vec![format!("{task_id}.txt")],
        instruction: instruction.map(str::to_string),
    }
}

async fn create_plan(owner: &mut Connection, tasks: Vec<PlanTaskSpec>) -> PlanInfo {
    match request(
        owner,
        ClientMessage::CreatePlan {
            title: "plan".to_string(),
            tasks,
            ticket: None,
        },
    )
    .await
    {
        DaemonMessage::PlanDetail { plan } => plan,
        other => panic!("expected plan_detail, got {other:?}"),
    }
}

async fn open_task(
    owner: &mut Connection,
    plan_id: &str,
    task_id: &str,
    name: &str,
) -> DaemonMessage {
    request(
        owner,
        ClientMessage::OpenTaskWindow {
            plan_id: plan_id.to_string(),
            task_id: task_id.to_string(),
            name: Some(name.to_string()),
        },
    )
    .await
}

/// An owner: a plain `echo_child` window, and a connection identified as it.
async fn plan_owner(scene: &Scene) -> Connection {
    let owner_id = scene
        .registry
        .open(OpenWindowRequest {
            command: echo_child_command(),
            cwd: None,
            name: None,
            vendor: None,
            detection: None,
            on_name_taken: Default::default(),
        })
        .await
        .expect("open the owner")
        .window_id;
    let mut owner = scene.connect().await;
    identify(&mut owner, &owner_id).await;
    owner
}

/// The stored command of the window named `name`, once the store holds it.
async fn stored_command(scene: &Scene, name: &str) -> Vec<String> {
    let db = scene.root.join("yagura.db");
    let deadline = tokio::time::Instant::now() + yagura_test_support::HANG_GUARD;
    loop {
        if let Some(rows) = crate::common::store_fixtures::window_rows(&db)
            && let Some(row) = rows.into_iter().find(|row| row.name == name)
        {
            return row.command;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for the store to hold the window named {name:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn a_task_windows_notice_is_its_launch_prompt_and_nothing_is_typed() {
    let scene = Scene::start("task", Opts::default()).await;
    let logs = scene.profile("hc-task", NONE_FLAGS, Some(NONE_DELIVERY));
    let mut owner = plan_owner(&scene).await;
    let plan = create_plan(
        &mut owner,
        vec![task("t1", "hc-task", Some("R4INSTRUCTIONBODY"))],
    )
    .await;

    let window_id = match open_task(&mut owner, &plan.plan_id, "t1", "task-window").await {
        DaemonMessage::TaskWindowOpened { window_id, .. } => window_id,
        other => panic!("expected task_window_opened, got {other:?}"),
    };

    let notice = expected_task_notice(&plan.plan_id, "t1");
    logs.until("the window to log its argv", |logs| !logs.argv().is_empty())
        .await;
    assert_eq!(logs.prompt().as_deref(), Some(notice.as_str()));
    assert_eq!(
        logs.argv().last().map(String::as_str),
        Some(notice.as_str()),
        "the launch prompt comes after everything else on the command line"
    );
    assert!(
        !logs
            .argv()
            .iter()
            .any(|arg| arg.contains("R4INSTRUCTIONBODY")),
        "the instruction's own body is read through get_plan, never handed over"
    );
    let detail = match request(
        &mut owner,
        ClientMessage::GetPlan {
            plan_id: plan.plan_id.clone(),
        },
    )
    .await
    {
        DaemonMessage::PlanDetail { plan } => plan,
        other => panic!("expected plan_detail, got {other:?}"),
    };
    let state = detail
        .tasks
        .iter()
        .find(|task| task.task_id == "t1")
        .and_then(|task| task.instruction_notice.as_ref())
        .map(|notice| notice.state);
    assert_eq!(
        state,
        Some(InstructionNoticeState::Delivered),
        "a notice handed over at spawn is delivered once the window is open"
    );
    let mut probe = scene.connect().await;
    wait_until_status(&mut probe, &window_id, WindowStatus::WaitingInput).await;
    logs.assert_never_typed("[yagura task]");
}

#[tokio::test]
async fn a_task_windows_launch_prompt_is_never_part_of_the_command_kept_for_a_restore() {
    let scene = Scene::start("task-restore", Opts::default()).await;
    let logs = scene.profile("hc-task", NONE_FLAGS, Some(NONE_DELIVERY));
    let mut owner = plan_owner(&scene).await;
    let plan = create_plan(&mut owner, vec![task("t1", "hc-task", Some("do it"))]).await;
    match open_task(&mut owner, &plan.plan_id, "t1", "task-window").await {
        DaemonMessage::TaskWindowOpened { .. } => {}
        other => panic!("expected task_window_opened, got {other:?}"),
    }
    let notice = expected_task_notice(&plan.plan_id, "t1");
    logs.until("the window to log its argv", |logs| !logs.argv().is_empty())
        .await;
    assert_eq!(
        logs.prompt().as_deref(),
        Some(notice.as_str()),
        "the child was started with the notice"
    );

    let stored = stored_command(&scene, "task-window").await;
    assert!(
        !stored.iter().any(|arg| arg.contains("[yagura task]")),
        "a window started again from what was stored must not be handed the task again: \
         {stored:?}"
    );
    assert!(!stored.iter().any(|arg| arg == "--prompt"), "{stored:?}");
}

// =========================================================================================
// a launch prompt is never handed over through a Windows `.cmd` / `.bat`
// =========================================================================================

#[cfg(windows)]
const SHIM_GUIDANCE_TEXT: &str = "SHIMROUTE use the WSL profile.";

/// The refusal a launch through a shim gets before anything starts: `unsupported`, with the
/// profile's own guidance in its reason.
#[cfg(windows)]
fn assert_refused_with_the_shim_guidance(reply: &DaemonMessage) {
    match reply {
        DaemonMessage::Error { code, message, .. } => {
            assert_eq!(code, "unsupported", "{reply:?}");
            assert!(
                message.contains(SHIM_GUIDANCE_TEXT),
                "the reason says where the request can be handed over: {reply:?}"
            );
        }
        other => panic!("expected an unsupported refusal, got {other:?}"),
    }
}

#[cfg(windows)]
fn write_cmd_profile(scene: &Scene, id: &str) {
    let shim = scene.root.join("hc-shim.cmd");
    std::fs::write(&shim, "@echo off\r\n").expect("write the shim");
    write_profile(
        &scene.root.join("profiles"),
        id,
        &format!(
            "schema = 1\ndisplay_name = \"{id}\"\ndetection = \"{HOOK_DETECTION}\"\n\
             shim_guidance = {{ text = \"{SHIM_GUIDANCE_TEXT}\" }}\n\n\
             [command]\ndefault = [{}]\n\n[delivery]\n{NONE_DELIVERY}\n",
            toml_path(&shim)
        ),
    );
}

#[cfg(windows)]
#[tokio::test]
async fn an_instructed_task_window_whose_program_is_a_cmd_shim_is_refused_and_nothing_opens() {
    let scene = Scene::start("task", Opts::default()).await;
    write_cmd_profile(&scene, "hc-shim");
    let mut owner = plan_owner(&scene).await;
    let before = call_list_windows(&mut owner).await.len();
    let plan = create_plan(
        &mut owner,
        vec![task("t1", "hc-shim", Some("R4BINSTRUCTION"))],
    )
    .await;

    let reply = open_task(&mut owner, &plan.plan_id, "t1", "window").await;

    // The prompt would be dropped on the way through cmd.exe, so the window is refused.
    assert_refused_with_the_shim_guidance(&reply);
    assert_eq!(
        call_list_windows(&mut owner).await.len(),
        before,
        "nothing was started"
    );
}

#[cfg(windows)]
#[tokio::test]
async fn a_handoff_that_opens_a_window_through_a_cmd_shim_is_refused_and_nothing_opens() {
    let scene = Scene::start("handoff", Opts::default()).await;
    write_cmd_profile(&scene, "hc-shim");
    let mut user = scene.user().await;
    let before = call_list_windows(&mut user).await.len();

    let reply = request(
        &mut user,
        ClientMessage::SendHandoff {
            window_id: None,
            profile: Some("hc-shim".to_string()),
            cwd: None,
            name: None,
            model: None,
            effort: None,
            reference: "docs/handoff.md".to_string(),
            note: "R4BHANDOFF".to_string(),
        },
    )
    .await;

    // The envelope would be dropped on the way through cmd.exe.
    assert_refused_with_the_shim_guidance(&reply);
    assert_eq!(
        call_list_windows(&mut user).await.len(),
        before,
        "nothing was started"
    );
}

// =========================================================================================
// before the first turn, what is waiting stays in the inbox for the first turn's door
// =========================================================================================

#[tokio::test]
async fn a_post_before_the_first_turn_waits_for_it_and_never_runs_the_wake_command() {
    let scene = Scene::start("first-turn", Opts::default()).await;
    let logs = scene.profile("hc-command", COMMAND_FLAGS, Some(COMMAND_DELIVERY));
    let mut user = scene.user().await;
    let id = scene.open_profile(&mut user, "hc-command").await;
    assert_eq!(scene.entry(&id).session_id(), None, "no turn has run yet");

    let (seq, _) = post(&mut user, &format!("@{id} R5BEFOREFIRSTTURN")).await;
    until_state(&mut user, seq, &id, "queued").await;
    scene.start_turn(&mut user, &logs, &id).await;

    logs.until_received("R5BEFOREFIRSTTURN").await;
    until_state(&mut user, seq, &id, "delivered").await;
    assert!(
        logs.door("context").contains("R5BEFOREFIRSTTURN")
            || logs.door("take").contains("R5BEFOREFIRSTTURN"),
        "it arrived with the first turn"
    );
    assert!(
        logs.woken().is_empty(),
        "the wake command needs a session and was never run: {:?}",
        logs.woken()
    );
    assert_eq!(logs.received("R5BEFOREFIRSTTURN"), 1);
    logs.assert_never_typed("R5BEFOREFIRSTTURN");
}

// =========================================================================================
// pending while in the inbox, taken once the CLI has it
// =========================================================================================

#[tokio::test]
async fn read_chat_reports_pending_until_the_cli_takes_the_post_and_taken_after() {
    let scene = Scene::start("states", Opts::default()).await;
    let logs = scene.profile("hc-none", NONE_FLAGS, Some(NONE_DELIVERY));
    let mut user = scene.user().await;
    let id = scene.open_profile(&mut user, "hc-none").await;
    scene.start_turn(&mut user, &logs, &id).await;
    let (seq, _) = post(&mut user, &format!("@{id} R7STATES")).await;

    until_state(&mut user, seq, &id, "queued").await;
    assert_eq!(
        scene.entry(&id).inbox().pending_count(),
        1,
        "the post waits in the inbox"
    );
    scene.start_turn(&mut user, &logs, &id).await;

    until_state(&mut user, seq, &id, "delivered").await;
    assert_eq!(scene.entry(&id).inbox().pending_count(), 0);
}

// =========================================================================================
// send_message is still typed
// =========================================================================================

// =========================================================================================
// the daemon pushes nothing to a window in Unknown; the CLI's own doors still answer
// =========================================================================================

/// A profile without status hooks, so that a status line no rule matches leaves the window
/// `Unknown` with nothing to overrule it.
const WAIT_FLAGS_QUIET: &[&str] = &["--context", "--take", "--wait"];

async fn lose(scene: &Scene, client: &mut Connection, window_id: &str) {
    scene.type_line(window_id, "LOSE").await;
    wait_until_status(client, window_id, WindowStatus::Unknown).await;
}

#[tokio::test]
async fn a_wait_in_an_unknown_window_is_not_answered_but_the_take_door_returns_the_post() {
    let scene = Scene::start("wait", Opts::default()).await;
    let logs = scene.profile("hc-wait", WAIT_FLAGS_QUIET, Some(WAIT_DELIVERY));
    let mut user = scene.user().await;
    let id = scene.open_profile(&mut user, "hc-wait").await;
    scene.until_waiting(&id).await;
    lose(&scene, &mut user, &id).await;

    let (seq, _) = post(&mut user, &format!("@{id} R9WAIT")).await;
    until_state(&mut user, seq, &id, "queued").await;
    let mut door = scene.connect().await;
    let taken = call_take(&mut door, &id).await;

    assert!(
        taken.contains("R9WAIT"),
        "the CLI asking for it gets it: {taken:?}"
    );
    assert_eq!(logs.received("R9WAIT"), 0, "the wait was never answered");
    assert!(
        scene.entry(&id).inbox().has_waiter(),
        "the wait is still registered"
    );
    logs.assert_never_typed("R9WAIT");
}

#[tokio::test]
async fn a_post_held_while_unknown_is_pushed_once_the_status_is_known_again() {
    let scene = Scene::start("wait-recovers", Opts::default()).await;
    let logs = scene.profile("hc-wait", WAIT_FLAGS_QUIET, Some(WAIT_DELIVERY));
    let mut user = scene.user().await;
    let id = scene.open_profile(&mut user, "hc-wait").await;
    scene.until_waiting(&id).await;
    lose(&scene, &mut user, &id).await;
    let (seq, _) = post(&mut user, &format!("@{id} R9RECOVERS")).await;
    until_state(&mut user, seq, &id, "queued").await;

    scene.type_line(&id, "FIND").await;

    logs.until_received("R9RECOVERS").await;
    until_state(&mut user, seq, &id, "delivered").await;
    assert!(logs.door("wait").contains("R9RECOVERS"));
    assert_eq!(logs.received("R9RECOVERS"), 1);
}

#[tokio::test]
async fn the_wake_command_never_runs_for_a_window_in_unknown() {
    let scene = Scene::start("command", Opts::default()).await;
    let logs = scene.profile(
        "hc-command",
        &["--take", "--session", SESSION],
        Some(COMMAND_DELIVERY),
    );
    let mut user = scene.user().await;
    let id = scene.open_profile(&mut user, "hc-command").await;
    scene.type_line(&id, "TURN").await;
    logs.until("the session to be reported", |logs| {
        logs.count_events("session") > 0
    })
    .await;
    scene
        .until("the session to be known", || {
            scene.entry(&id).session_id().is_some()
        })
        .await;
    lose(&scene, &mut user, &id).await;

    let (seq, _) = post(&mut user, &format!("@{id} R9COMMAND")).await;
    until_state(&mut user, seq, &id, "queued").await;
    let mut door = scene.connect().await;
    let taken = call_take(&mut door, &id).await;

    assert!(taken.contains("R9COMMAND"), "{taken:?}");
    assert!(logs.woken().is_empty(), "{:?}", logs.woken());
    logs.assert_never_typed("R9COMMAND");
}

#[tokio::test]
async fn a_window_with_no_wake_door_is_never_typed_into_while_unknown() {
    let scene = Scene::start("none", Opts::default()).await;
    let logs = scene.profile("hc-none", &["--take"], Some(NONE_DELIVERY));
    let mut user = scene.user().await;
    let id = scene.open_profile(&mut user, "hc-none").await;
    lose(&scene, &mut user, &id).await;

    let (seq, _) = post(&mut user, &format!("@{id} R9NONE")).await;
    until_state(&mut user, seq, &id, "queued").await;
    let mut door = scene.connect().await;
    let taken = call_take(&mut door, &id).await;

    assert!(taken.contains("R9NONE"), "{taken:?}");
    logs.assert_never_typed("R9NONE");
}

// =========================================================================================
// a window that declares no delivery, or whose hooks were left out, takes no part
// =========================================================================================

/// Checks that `window_id` is no participant: a post naming it and a broadcast both leave
/// it out, nothing the daemon sends of its own accord -- the intro included -- reaches it,
/// and a handoff or a message to it is undelivered for want of a way to deliver it.
async fn assert_takes_no_part(scene: &Scene, user: &mut Connection, logs: &Logs, window_id: &str) {
    let (_, targets) = post(user, &format!("@{window_id} R10MENTION")).await;
    assert!(
        !targets.contains(&window_id.to_string()),
        "a mention of a window without delivery resolves to nobody: {targets:?}"
    );
    match request(
        user,
        ClientMessage::BroadcastChat {
            room_id: None,
            body: "R10BROADCAST".to_string(),
        },
    )
    .await
    {
        DaemonMessage::ChatPosted { targets, .. } => assert!(
            !targets.contains(&window_id.to_string()),
            "a broadcast leaves it out: {targets:?}"
        ),
        other => panic!("expected chat_posted, got {other:?}"),
    }

    let (_, _, delivery) =
        expect_handoff_sent(request(user, handoff_to(window_id, "R10HANDOFF")).await);
    assert_eq!(delivery, "undelivered_no_delivery");

    match request(
        user,
        ClientMessage::SendText {
            window_id: window_id.to_string(),
            text: "R10MESSAGE".to_string(),
        },
    )
    .await
    {
        DaemonMessage::TextSent { state, .. } => assert_eq!(
            serde_json::to_value(state).expect("a state encodes"),
            "undelivered_no_delivery"
        ),
        other => panic!("expected text_sent, got {other:?}"),
    }
    for needle in [
        INTRO,
        "R10MENTION",
        "R10BROADCAST",
        "R10HANDOFF",
        "R10MESSAGE",
    ] {
        logs.assert_never_typed(needle);
        assert_eq!(logs.received(needle), 0, "{needle}");
    }
    let _ = scene;
}

#[tokio::test]
async fn a_window_opened_from_a_raw_argv_takes_no_part_in_chat_or_notices() {
    let scene = Scene::start("argv", Opts::default()).await;
    let mut user = scene.user().await;
    let logs = Logs {
        dir: scene.root.join("logs").join("argv"),
    };
    let id = scene.open_argv(&mut user, &logs.dir, WAIT_FLAGS).await;

    assert_takes_no_part(&scene, &mut user, &logs, &id).await;
}

#[tokio::test]
async fn a_window_whose_profile_declares_no_delivery_takes_no_part_in_chat_or_notices() {
    let scene = Scene::start("no-table", Opts::default()).await;
    let logs = scene.profile("hc-plain", WAIT_FLAGS, None);
    let mut user = scene.user().await;
    let id = scene.open_profile(&mut user, "hc-plain").await;

    assert_takes_no_part(&scene, &mut user, &logs, &id).await;
}

#[tokio::test]
async fn a_window_whose_hooks_were_left_out_takes_no_part_in_chat_or_notices() {
    let scene = Scene::start("hooks-left-out", Opts::default()).await;
    // The hooks this profile's CLI runs live in a settings file the daemon checks at
    // launch; that file was never written, so the launch goes without them.
    let logs = scene.profile_with(
        "hc-registered",
        "0",
        WAIT_FLAGS,
        Some(WAIT_DELIVERY),
        "\n[mcp_registration]\nadd = [\"mcp\", \"add\", \"{server}\"]\n\
         remove = [\"mcp\", \"remove\", \"{server}\"]\n\n\
         [mcp_registration.status]\nfile = \"hc/mcp.json\"\nformat = \"json\"\n\
         command = \"/mcpServers/{server}/command\"\n\n\
         [mcp_registration.json_entry]\nfile = \"hc/hooks.json\"\n\n\
         [mcp_registration.json_entry.entries.\"{server}\"]\n\
         SessionStart = [{ type = \"command\", command = \"{daemon} --deliver-context\", \
         timeout = 10 }]\n",
    );
    let mut user = scene.user().await;
    let id = scene.open_profile(&mut user, "hc-registered").await;

    assert_takes_no_part(&scene, &mut user, &logs, &id).await;
}

// =========================================================================================
// a handoff answers once the CLI took it, or once it is known it never will
// =========================================================================================

#[tokio::test]
async fn a_handoff_that_opens_a_window_hands_the_envelope_over_as_its_launch_prompt() {
    let scene = Scene::start("open", Opts::default()).await;
    let logs = scene.profile("hc-wait", WAIT_FLAGS, Some(WAIT_DELIVERY));
    let mut user = scene.user().await;

    let (window_id, opened, delivery) = expect_handoff_sent(
        request(
            &mut user,
            ClientMessage::SendHandoff {
                window_id: None,
                profile: Some("hc-wait".to_string()),
                cwd: None,
                name: Some("opened".to_string()),
                model: None,
                effort: None,
                reference: "docs/handoff.md".to_string(),
                note: "R11OPENED".to_string(),
            },
        )
        .await,
    );

    assert!(opened);
    assert_eq!(
        delivery, "delivered",
        "taken once the window started with it"
    );
    logs.until("the window to log its argv", |logs| !logs.argv().is_empty())
        .await;
    assert!(
        logs.prompt().unwrap_or_default().contains("R11OPENED"),
        "the envelope is the launch prompt: {:?}",
        logs.argv()
    );
    wait_until_status(&mut user, &window_id, WindowStatus::WaitingInput).await;
    logs.assert_never_typed("R11OPENED");
    assert_eq!(logs.received("R11OPENED"), 0, "handed over once, at launch");

    let stored = stored_command(&scene, "opened").await;
    assert!(
        !stored.iter().any(|arg| arg.contains("R11OPENED")),
        "the envelope is not kept for a restore: {stored:?}"
    );
}

// =========================================================================================
// a push under way is not handed out again; a failed push goes back to the inbox
// =========================================================================================

#[tokio::test]
async fn a_post_being_pushed_by_command_is_not_returned_by_a_turn_start_door() {
    let scene = Scene::start("command-held", Opts::default()).await;
    let hold = scene.root.join("command-hold");
    std::fs::write(&hold, b"").expect("create the hold file");
    let hold_arg = hold.display().to_string();
    let logs = scene.profile(
        "hc-command",
        &["--session", SESSION, "--woken-hold", &hold_arg],
        Some(COMMAND_DELIVERY),
    );
    let mut user = scene.user().await;
    let id = scene.open_profile(&mut user, "hc-command").await;
    scene.type_line(&id, "TURN").await;
    scene
        .until("the session to be known", || {
            scene.entry(&id).session_id().is_some()
        })
        .await;

    let (seq, _) = post(&mut user, &format!("@{id} R13HELD")).await;
    logs.until("the wake command to start", |logs| {
        logs.woken()
            .iter()
            .any(|line| line["event"] == "woken_started")
    })
    .await;
    let mut door = scene.connect().await;
    let taken = call_take(&mut door, &id).await;
    std::fs::remove_file(&hold).expect("release the wake command");

    assert!(
        !taken.contains("R13HELD"),
        "a post being pushed is not handed out again"
    );
    logs.until_received("R13HELD").await;
    until_state(&mut user, seq, &id, "delivered").await;
    assert_eq!(logs.received("R13HELD"), 1);
}

#[tokio::test]
async fn a_failed_command_push_puts_the_post_back_for_the_next_turn() {
    let scene = Scene::start("command-failed", Opts::default()).await;
    let logs = scene.profile(
        "hc-command",
        &["--session", SESSION, "--woken-fail"],
        Some(COMMAND_DELIVERY),
    );
    let mut user = scene.user().await;
    let id = scene.open_profile(&mut user, "hc-command").await;
    scene.type_line(&id, "TURN").await;
    scene
        .until("the session to be known", || {
            scene.entry(&id).session_id().is_some()
        })
        .await;

    let (seq, _) = post(&mut user, &format!("@{id} R13FAILED")).await;
    logs.until("the wake command to fail", |logs| {
        logs.woken()
            .iter()
            .any(|line| line["event"] == "woken_failed")
    })
    .await;
    scene
        .until("the post to be back in the inbox", || {
            scene.entry(&id).inbox().pending_count() == 1
        })
        .await;
    let mut door = scene.connect().await;
    let taken = call_take(&mut door, &id).await;

    assert!(taken.contains("R13FAILED"), "{taken:?}");
    until_state(&mut user, seq, &id, "delivered").await;
}

// =========================================================================================
// a task window's report stands until a new instruction reaches the window
// =========================================================================================

/// A one-task plan, its task window, the plan's owner, a connection speaking as the task
/// window (its reports) and a plain one that observes.
struct BoundTask {
    id: String,
    owner: Connection,
    reporter: Connection,
    probe: Connection,
}

impl BoundTask {
    /// Opens the task window from `profile` once its screen waits for input. Nothing waits
    /// in its inbox, so no delivery of the start-up can be taken for one that follows a
    /// report.
    async fn open(scene: &Scene, profile: &str) -> Self {
        let mut owner = plan_owner(scene).await;
        let plan = create_plan(&mut owner, vec![task("t1", profile, None)]).await;
        let id = match open_task(&mut owner, &plan.plan_id, "t1", "task-window").await {
            DaemonMessage::TaskWindowOpened { window_id, .. } => window_id,
            other => panic!("expected task_window_opened, got {other:?}"),
        };
        let mut probe = scene.connect().await;
        wait_until_status(&mut probe, &id, WindowStatus::WaitingInput).await;
        let mut reporter = scene.connect().await;
        identify(&mut reporter, &id).await;
        assert_eq!(
            scene.entry(&id).inbox().pending_count(),
            0,
            "a task window starts with nothing waiting in its inbox"
        );
        Self {
            id,
            owner,
            reporter,
            probe,
        }
    }

    /// Reports to the plan's owner and reads the report back from its inbox, so the owner's
    /// inbox is empty again.
    async fn report(&mut self, summary: &str) {
        match request(
            &mut self.reporter,
            ClientMessage::SendReport {
                outcome: ReportOutcome::Succeeded,
                summary: summary.to_string(),
                path: None,
            },
        )
        .await
        {
            DaemonMessage::ReportSent { .. } => {}
            other => panic!("expected report_sent, got {other:?}"),
        }
        match request(
            &mut self.owner,
            ClientMessage::WaitForReport { timeout_sec: 0 },
        )
        .await
        {
            DaemonMessage::Reports { reports, .. } => assert_eq!(reports.len(), 1),
            other => panic!("expected reports, got {other:?}"),
        }
    }

    /// The `task_report` the window list carries for the task window.
    async fn mark(&mut self) -> Option<TaskReportMark> {
        match request(&mut self.probe, ClientMessage::ListWindows).await {
            DaemonMessage::WindowList { windows } => {
                windows
                    .into_iter()
                    .find(|window| window.window_id == self.id)
                    .expect("the task window is listed")
                    .task_report
            }
            other => panic!("expected window_list, got {other:?}"),
        }
    }

    /// Waits for the list to carry `expected`: the event that releases a report is seen
    /// before the list is read.
    async fn until_mark(&mut self, expected: Option<TaskReportMark>) {
        let deadline = tokio::time::Instant::now() + yagura_test_support::HANG_GUARD;
        loop {
            let seen = self.mark().await;
            if seen == expected {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "timed out waiting for the task window's report to read {expected:?} (last \
                 seen {seen:?})"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

fn mark(seq: u64, standing: bool) -> Option<TaskReportMark> {
    Some(TaskReportMark { seq, standing })
}

/// Types `text` into `window_id` the way its user does, through an attached view, and
/// returns once the daemon took the write.
async fn type_as_the_user(scene: &Scene, window_id: &str, text: &str) {
    let mut typist = scene.connect().await;
    common::attach(&mut typist, window_id).await;
    common::write_interactive_input(&mut typist, text).await;
    send(&mut typist, &ClientMessage::Ping).await;
    match read_response(&mut typist).await {
        DaemonMessage::Pong => {}
        other => panic!("expected pong, got {other:?}"),
    }
}

#[tokio::test]
async fn a_report_stands_through_state_swings_and_ends_when_a_door_hands_the_window_a_post() {
    let scene = Scene::start("task-report-doors", Opts::default()).await;
    let logs = scene.profile("hc-wait", WAIT_FLAGS, Some(WAIT_DELIVERY));
    let mut user = scene.user().await;
    let mut task = BoundTask::open(&scene, "hc-wait").await;
    let id = task.id.clone();
    scene.until_waiting(&id).await;

    task.report("first").await;
    assert_eq!(task.mark().await, mark(1, true));

    // A post the wait door hands over is a new instruction (a parcel with a delivery record).
    let (seq, _) = post(&mut user, &format!("@{id} R6WAITPOST")).await;
    logs.until_received("R6WAITPOST").await;
    until_state(&mut user, seq, &id, "delivered").await;
    task.until_mark(mark(1, false)).await;

    // So is a handoff (a parcel with no delivery record).
    scene.until_waiting(&id).await;
    wait_until_status(&mut task.probe, &id, WindowStatus::WaitingInput).await;
    task.report("second").await;
    assert_eq!(task.mark().await, mark(2, true));
    let (_, _, delivery) =
        expect_handoff_sent(request(&mut user, handoff_to(&id, "R6HANDOFF")).await);
    assert_eq!(delivery, "delivered");
    logs.until_received("R6HANDOFF").await;
    task.until_mark(mark(2, false)).await;
}

#[tokio::test]
async fn a_report_survives_a_post_that_only_reached_the_inbox_and_the_post_that_starts_a_turn() {
    let scene = Scene::start("task-report-none", Opts::default()).await;
    let logs = scene.profile("hc-none", NONE_FLAGS, Some(NONE_DELIVERY));
    let mut user = scene.user().await;
    let mut task = BoundTask::open(&scene, "hc-none").await;
    let id = task.id.clone();

    // The post that starts a turn is taken while the window already shows `Running`: it is
    // the turn's own instruction, not one that follows the report sent in that turn.
    let (seq, _) = post(&mut user, &format!("@{id} R6STARTS")).await;
    until_state(&mut user, seq, &id, "queued").await;
    scene.start_turn(&mut task.probe, &logs, &id).await;
    until_state(&mut user, seq, &id, "delivered").await;
    task.report("first").await;
    assert_eq!(task.mark().await, mark(1, true));
    scene.end_turn(&mut task.probe, &logs, &id).await;
    assert_eq!(task.mark().await, mark(1, true));

    // A status the detection cannot read, and the way back, do not release it either.
    lose(&scene, &mut task.probe, &id).await;
    scene.type_line(&id, "FIND").await;
    wait_until_status(&mut task.probe, &id, WindowStatus::WaitingInput).await;
    assert_eq!(task.mark().await, mark(1, true));

    // A turn that takes nothing is no new instruction.
    scene.start_turn(&mut task.probe, &logs, &id).await;
    assert_eq!(task.mark().await, mark(1, true));

    // A post that only reached the inbox is not one yet; the door that hands it over is.
    task.report("second").await;
    assert_eq!(task.mark().await, mark(2, true));
    let (seq, _) = post(&mut user, &format!("@{id} R6QUEUED")).await;
    until_state(&mut user, seq, &id, "queued").await;
    assert_eq!(task.mark().await, mark(2, true));
    scene.end_turn(&mut task.probe, &logs, &id).await;
    assert_eq!(task.mark().await, mark(2, true));
    scene.start_turn(&mut task.probe, &logs, &id).await;
    until_state(&mut user, seq, &id, "delivered").await;
    task.until_mark(mark(2, false)).await;

    // What the user types ahead of a turn's end is the next instruction, not the end of
    // the one this report answers: a report sent while the window shows `Running` stays
    // released once the window waits.
    scene.end_turn(&mut task.probe, &logs, &id).await;
    scene.start_turn(&mut task.probe, &logs, &id).await;
    type_as_the_user(&scene, &id, "keep going please\r").await;
    task.report("third").await;
    assert_eq!(task.mark().await, mark(3, false));
    scene.end_turn(&mut task.probe, &logs, &id).await;
    assert_eq!(task.mark().await, mark(3, false));
}

#[tokio::test]
async fn a_report_ends_when_the_wake_command_hands_the_window_a_post_even_ahead_of_its_turn_end() {
    let scene = Scene::start("task-report-command", Opts::default()).await;
    let logs = scene.profile("hc-command", COMMAND_FLAGS, Some(COMMAND_DELIVERY));
    let mut user = scene.user().await;
    let mut task = BoundTask::open(&scene, "hc-command").await;
    let id = task.id.clone();
    // The first turn tells the daemon the session the wake command needs.
    scene.start_turn(&mut task.probe, &logs, &id).await;
    scene.end_turn(&mut task.probe, &logs, &id).await;
    assert_eq!(scene.entry(&id).session_id().as_deref(), Some(SESSION));

    task.report("first").await;
    assert_eq!(task.mark().await, mark(1, true));
    let (seq, _) = post(&mut user, &format!("@{id} R6PUSHED")).await;
    logs.until_received("R6PUSHED").await;
    until_state(&mut user, seq, &id, "delivered").await;
    task.until_mark(mark(1, false)).await;

    // A post pushed while the window shows `Running` is an instruction that reached the
    // CLI ahead of the turn's end: a report sent in the rest of that turn is released
    // from the start, and stays so once the window waits.
    scene.start_turn(&mut task.probe, &logs, &id).await;
    let (seq, _) = post(&mut user, &format!("@{id} R6PUSHEDRUNNING")).await;
    logs.until_received("R6PUSHEDRUNNING").await;
    until_state(&mut user, seq, &id, "delivered").await;
    task.report("second").await;
    assert_eq!(task.mark().await, mark(2, false));
    scene.end_turn(&mut task.probe, &logs, &id).await;
    assert_eq!(task.mark().await, mark(2, false));
}
