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

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use common::{
    CONTROL_INPUT_TABLE, VALID_TOKEN, approver_base_config, call_list_windows,
    composer_section_toml, detection_config, empty_lookup, identify, read_response,
    register_approver, send, unique_temp_dir, wait_until_status, write_profile,
};
use yagura_daemon::profile::ProfileOs;
use yagura_daemon::{IpcServer, OpenWindowRequest, WindowRegistry};
use yagura_protocol::{
    ChatMessage, ClientMessage, DaemonMessage, InstructionNoticeState, PlanInfo, PlanTaskSpec,
    ReportOutcome, WindowStatus,
};
use yagura_test_support::ipc::Connection;
use yagura_test_support::process::{
    COMPOSER_CONTINUATION_PREFIX, COMPOSER_MARKER, COMPOSER_SEPARATOR, echo_child_command,
};

/// The detection definition every `hook_child` window is opened under.
const HOOK_DETECTION: &str = "hook-child";

/// The playbook's `[chat] intro`.
const INTRO: &str = "INTRO-7Q-STARTUP-TEXT";

/// How long a case waits for something that must happen.
const EVENT_TIMEOUT: Duration = Duration::from_secs(20);

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
    format!("[yagura report] {pending} pending report(s). Call wait_for_report to read them.")
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
            config.max_open_handoffs_per_conn = bound;
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
                    yagura_daemon::ipc::chat::ChatRuntime::from_config(&config)
                })
                .await
                .expect("ChatRuntime::from_config must not panic"),
            )
        };
        yagura_daemon::ipc::chat::install_intro_hook(&registry, chat.clone());
        let ledger = Arc::new(
            tokio::task::spawn_blocking({
                let config = config.clone();
                move || yagura_daemon::ipc::plan::PlanRuntime::from_config(&config)
            })
            .await
            .expect("PlanRuntime::from_config must not panic"),
        );
        let server = Arc::new(
            IpcServer::new_with_chat(registry.clone(), endpoint.clone(), config, chat)
                .with_ledger(ledger),
        );
        let handle = tokio::spawn(async move {
            let _ = server.run().await;
        });
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
        yagura_test_support::ipc::connect_with_retry(&self.endpoint, Duration::from_secs(5)).await
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
            },
        )
        .await;
        let id = match read_reply(client).await {
            DaemonMessage::WindowOpened { window_id, .. } => window_id,
            other => panic!("expected window_opened for {profile}, got {other:?}"),
        };
        wait_until_status(client, &id, WindowStatus::WaitingInput, EVENT_TIMEOUT).await;
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
            })
            .await
            .expect("open the window")
            .window_id;
        wait_until_status(client, &id, WindowStatus::WaitingInput, EVENT_TIMEOUT).await;
        id
    }

    async fn start_turn(&self, client: &mut Connection, logs: &Logs, window_id: &str) {
        let before = logs.count_events("turn_started");
        self.type_line(window_id, "TURN").await;
        logs.until("the turn to start", |logs| {
            logs.count_events("turn_started") > before
        })
        .await;
        wait_until_status(client, window_id, WindowStatus::Running, EVENT_TIMEOUT).await;
    }

    async fn end_turn(&self, client: &mut Connection, logs: &Logs, window_id: &str) {
        let before = logs.count_events("turn_ended");
        self.type_line(window_id, "END").await;
        logs.until("the turn to end", |logs| {
            logs.count_events("turn_ended") > before
        })
        .await;
        wait_until_status(client, window_id, WindowStatus::WaitingInput, EVENT_TIMEOUT).await;
    }

    async fn until(&self, what: &str, mut condition: impl FnMut() -> bool) {
        let deadline = tokio::time::Instant::now() + EVENT_TIMEOUT;
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
             region = \"bottom\"\n{CONTROL_INPUT_TABLE}{}",
            composer_section_toml()
        ),
    )
    .expect("write the hook_child definition");
}

/// The `[delivery]` table of a profile whose CLI leaves a wait registered.
const WAIT_DELIVERY: &str = "wake = \"wait\"\nlaunch_prompt = [\"--prompt\", \"{prompt}\"]";
/// ... of a profile whose CLI is woken by running its own program.
const COMMAND_DELIVERY: &str = "wake = \"command\"\nwake_args = [\"--woken\", \"{session}\", \
                                \"{text}\"]\nsession_pointer = \"/session_id\"\n\
                                launch_prompt = [\"--prompt\", \"{prompt}\"]";
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
/// The hooks a CLI with no wake door runs (agy's shape).
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
        let deadline = tokio::time::Instant::now() + EVENT_TIMEOUT;
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
        match read_response(client, EVENT_TIMEOUT).await {
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
    let deadline = tokio::time::Instant::now() + EVENT_TIMEOUT;
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
            &ClientMessage::DeliverTake {
                window_id: window_id.to_string(),
                token: VALID_TOKEN.to_string(),
            },
        )
        .await,
    );
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
            delivery,
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
// R1: a chat post reaches the CLI through its doors, once, and is never typed
// =========================================================================================

#[tokio::test]
async fn a_post_to_a_waiting_window_that_waits_for_the_daemon_arrives_through_the_wait() {
    let scene = Scene::start("r1-wait-idle", Opts::default()).await;
    let logs = scene.profile("hc-wait", WAIT_FLAGS, Some(WAIT_DELIVERY));
    let mut user = scene.user().await;
    let id = scene.open_profile(&mut user, "hc-wait").await;
    scene.until_waiting(&id).await;

    let (seq, targets) = post(&mut user, &format!("@{id} R1WAITIDLE")).await;
    assert_eq!(targets, vec![id.clone()]);

    logs.until_received("R1WAITIDLE").await;
    until_state(&mut user, seq, &id, "taken").await;
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
    let scene = Scene::start("r1-wait-working", Opts::default()).await;
    let logs = scene.profile("hc-wait", WAIT_FLAGS, Some(WAIT_DELIVERY));
    let mut user = scene.user().await;
    let id = scene.open_profile(&mut user, "hc-wait").await;
    scene.start_turn(&mut user, &logs, &id).await;
    scene.until_waiting(&id).await;

    let (seq, _) = post(&mut user, &format!("@{id} R1WAITWORKING")).await;

    logs.until_received("R1WAITWORKING").await;
    until_state(&mut user, seq, &id, "taken").await;
    assert!(logs.door("wait").contains("R1WAITWORKING"));
    assert_eq!(logs.received("R1WAITWORKING"), 1);
    logs.assert_never_typed("R1WAITWORKING");
}

#[tokio::test]
async fn a_post_to_a_waiting_window_woken_by_command_runs_the_command_with_its_session() {
    let scene = Scene::start("r1-command-idle", Opts::default()).await;
    let logs = scene.profile("hc-command", COMMAND_FLAGS, Some(COMMAND_DELIVERY));
    let mut user = scene.user().await;
    let id = scene.open_profile(&mut user, "hc-command").await;
    // The first turn tells the daemon the session the command needs.
    scene.start_turn(&mut user, &logs, &id).await;
    scene.end_turn(&mut user, &logs, &id).await;
    assert_eq!(scene.entry(&id).session_id().as_deref(), Some(SESSION));

    let (seq, _) = post(&mut user, &format!("@{id} R1COMMANDIDLE")).await;

    logs.until_received("R1COMMANDIDLE").await;
    until_state(&mut user, seq, &id, "taken").await;
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
    let scene = Scene::start("r1-command-working", Opts::default()).await;
    let logs = scene.profile("hc-command", COMMAND_FLAGS, Some(COMMAND_DELIVERY));
    let mut user = scene.user().await;
    let id = scene.open_profile(&mut user, "hc-command").await;
    scene.start_turn(&mut user, &logs, &id).await;

    let (seq, _) = post(&mut user, &format!("@{id} R1COMMANDWORKING")).await;

    logs.until_received("R1COMMANDWORKING").await;
    until_state(&mut user, seq, &id, "taken").await;
    assert!(logs.woken_texts().contains("R1COMMANDWORKING"));
    assert_eq!(logs.received("R1COMMANDWORKING"), 1);
    logs.assert_never_typed("R1COMMANDWORKING");
}

#[tokio::test]
async fn a_post_to_a_working_window_with_no_wake_door_arrives_with_the_next_turn_start() {
    let scene = Scene::start("r1-none-working", Opts::default()).await;
    let logs = scene.profile("hc-none", NONE_FLAGS, Some(NONE_DELIVERY));
    let mut user = scene.user().await;
    let id = scene.open_profile(&mut user, "hc-none").await;
    scene.start_turn(&mut user, &logs, &id).await;

    let (seq, _) = post(&mut user, &format!("@{id} R1NONEWORKING")).await;
    until_state(&mut user, seq, &id, "pending").await;
    scene.start_turn(&mut user, &logs, &id).await;

    logs.until_received("R1NONEWORKING").await;
    until_state(&mut user, seq, &id, "taken").await;
    assert!(logs.door("take").contains("R1NONEWORKING"));
    assert_eq!(logs.received("R1NONEWORKING"), 1);
    logs.assert_never_typed("R1NONEWORKING");
}

#[tokio::test]
async fn a_post_waiting_for_a_session_is_pushed_by_command_once_the_session_is_first_known() {
    let scene = Scene::start("r1-command-session", Opts::default()).await;
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
    until_state(&mut user, seq, &id, "pending").await;

    scene.type_line(&id, "TURN").await;

    logs.until_received("R1SESSIONKNOWN").await;
    until_state(&mut user, seq, &id, "taken").await;
    assert!(logs.woken_texts().contains("R1SESSIONKNOWN"));
    assert_eq!(logs.received("R1SESSIONKNOWN"), 1);
    logs.assert_never_typed("R1SESSIONKNOWN");
}

// =========================================================================================
// R2: opening a window types nothing; the start-up door carries the intro
// =========================================================================================

#[tokio::test]
async fn opening_a_window_types_nothing_and_its_start_up_door_carries_the_intro() {
    let scene = Scene::start("r2-intro", Opts::default()).await;
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
    until_state(&mut user, seq, &id, "taken").await;

    logs.assert_never_typed(INTRO);
    assert_eq!(logs.received(INTRO), 1, "the intro reaches the CLI once");
}

// =========================================================================================
// R3: a handoff, a plan approval and a report reach the CLI through its doors
// =========================================================================================

#[tokio::test]
async fn a_handoff_to_an_existing_window_arrives_through_its_wait_and_answers_taken() {
    let scene = Scene::start("r3-handoff", Opts::default()).await;
    let logs = scene.profile("hc-wait", WAIT_FLAGS, Some(WAIT_DELIVERY));
    let mut user = scene.user().await;
    let id = scene.open_profile(&mut user, "hc-wait").await;
    scene.until_waiting(&id).await;

    let (window_id, opened, delivery) =
        expect_handoff_sent(request(&mut user, handoff_to(&id, "R3HANDOFF")).await);

    assert_eq!((window_id.as_str(), opened), (id.as_str(), false));
    assert_eq!(delivery, "taken", "answered once the CLI took the envelope");
    assert!(logs.door("wait").contains("R3HANDOFF"));
    assert_eq!(logs.received("R3HANDOFF"), 1);
    logs.assert_never_typed("R3HANDOFF");
}

#[tokio::test]
async fn a_plan_approval_reaches_its_owner_through_the_owners_wait() {
    let scene = Scene::start(
        "r3-plan",
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
    let scene = Scene::start("r3-report", Opts::default()).await;
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
// R4 / R12: a task window's notice is its launch prompt, kept out of what is stored
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
    let deadline = tokio::time::Instant::now() + EVENT_TIMEOUT;
    loop {
        if let Ok(conn) = yagura_daemon::store::open_readonly(&db)
            && let Ok(rows) = yagura_daemon::store::read_snapshot_conn(&conn)
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
    let scene = Scene::start("r4-task", Opts::default()).await;
    let logs = scene.profile("hc-task", NONE_FLAGS, Some(NONE_DELIVERY));
    let mut owner = plan_owner(&scene).await;
    let plan = create_plan(
        &mut owner,
        vec![task("t1", "hc-task", Some("R4INSTRUCTIONBODY"))],
    )
    .await;

    let window_id = match open_task(&mut owner, &plan.plan_id, "t1", "r4-task-window").await {
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
    wait_until_status(
        &mut probe,
        &window_id,
        WindowStatus::WaitingInput,
        EVENT_TIMEOUT,
    )
    .await;
    logs.assert_never_typed("[yagura task]");
}

#[tokio::test]
async fn a_task_windows_launch_prompt_is_never_part_of_the_command_kept_for_a_restore() {
    let scene = Scene::start("r12-task-restore", Opts::default()).await;
    let logs = scene.profile("hc-task", NONE_FLAGS, Some(NONE_DELIVERY));
    let mut owner = plan_owner(&scene).await;
    let plan = create_plan(&mut owner, vec![task("t1", "hc-task", Some("do it"))]).await;
    match open_task(&mut owner, &plan.plan_id, "t1", "r12-task-window").await {
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

    let stored = stored_command(&scene, "r12-task-window").await;
    assert!(
        !stored.iter().any(|arg| arg.contains("[yagura task]")),
        "a window started again from what was stored must not be handed the task again: \
         {stored:?}"
    );
    assert!(!stored.iter().any(|arg| arg == "--prompt"), "{stored:?}");
}

// =========================================================================================
// R4b: a launch prompt is never handed over through a Windows `.cmd` / `.bat`
// =========================================================================================

#[cfg(windows)]
fn write_cmd_profile(scene: &Scene, id: &str) {
    let shim = scene.root.join("hc-shim.cmd");
    std::fs::write(&shim, "@echo off\r\n").expect("write the shim");
    write_profile(
        &scene.root.join("profiles"),
        id,
        &format!(
            "schema = 1\ndisplay_name = \"{id}\"\ndetection = \"{HOOK_DETECTION}\"\n\n\
             [command]\ndefault = [{}]\n\n[delivery]\n{NONE_DELIVERY}\n",
            toml_path(&shim)
        ),
    );
}

#[cfg(windows)]
#[tokio::test]
async fn an_instructed_task_window_whose_program_is_a_cmd_shim_is_refused_and_nothing_opens() {
    let scene = Scene::start("r4b-task", Opts::default()).await;
    write_cmd_profile(&scene, "hc-shim");
    let mut owner = plan_owner(&scene).await;
    let before = call_list_windows(&mut owner).await.len();
    let plan = create_plan(
        &mut owner,
        vec![task("t1", "hc-shim", Some("R4BINSTRUCTION"))],
    )
    .await;

    let reply = open_task(&mut owner, &plan.plan_id, "t1", "r4b-window").await;

    assert!(
        matches!(reply, DaemonMessage::Error { .. }),
        "the prompt would be dropped on the way through cmd.exe, so the window is refused: \
         {reply:?}"
    );
    assert_eq!(
        call_list_windows(&mut owner).await.len(),
        before,
        "nothing was started"
    );
}

#[cfg(windows)]
#[tokio::test]
async fn a_handoff_that_opens_a_window_through_a_cmd_shim_is_refused_and_nothing_opens() {
    let scene = Scene::start("r4b-handoff", Opts::default()).await;
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

    assert!(
        matches!(reply, DaemonMessage::Error { .. }),
        "the envelope would be dropped on the way through cmd.exe: {reply:?}"
    );
    assert_eq!(
        call_list_windows(&mut user).await.len(),
        before,
        "nothing was started"
    );
}

// =========================================================================================
// R5: before the first turn, what is waiting stays in the inbox for the first turn's door
// =========================================================================================

#[tokio::test]
async fn a_post_before_the_first_turn_waits_for_it_and_never_runs_the_wake_command() {
    let scene = Scene::start("r5-first-turn", Opts::default()).await;
    let logs = scene.profile("hc-command", COMMAND_FLAGS, Some(COMMAND_DELIVERY));
    let mut user = scene.user().await;
    let id = scene.open_profile(&mut user, "hc-command").await;
    assert_eq!(scene.entry(&id).session_id(), None, "no turn has run yet");

    let (seq, _) = post(&mut user, &format!("@{id} R5BEFOREFIRSTTURN")).await;
    until_state(&mut user, seq, &id, "pending").await;
    scene.start_turn(&mut user, &logs, &id).await;

    logs.until_received("R5BEFOREFIRSTTURN").await;
    until_state(&mut user, seq, &id, "taken").await;
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
// R6: a profile with no wake door is typed into while it waits, and only then
// =========================================================================================

#[tokio::test]
async fn a_post_to_a_waiting_window_with_no_wake_door_is_typed_once_and_taken() {
    let scene = Scene::start("r6-none-idle", Opts::default()).await;
    let logs = scene.profile("hc-none", NONE_FLAGS, Some(NONE_DELIVERY));
    let mut user = scene.user().await;
    let id = scene.open_profile(&mut user, "hc-none").await;

    let (seq, _) = post(&mut user, &format!("@{id} R6NONEIDLE")).await;

    until_state(&mut user, seq, &id, "taken").await;
    assert_eq!(logs.typed().matches("R6NONEIDLE").count(), 1);
    assert_eq!(logs.received("R6NONEIDLE"), 0);
}

#[tokio::test]
async fn a_post_after_the_last_turn_start_is_typed_once_the_window_waits_again() {
    let scene = Scene::start("r6-none-after-turn", Opts::default()).await;
    let logs = scene.profile("hc-none", NONE_FLAGS, Some(NONE_DELIVERY));
    let mut user = scene.user().await;
    let id = scene.open_profile(&mut user, "hc-none").await;
    scene.start_turn(&mut user, &logs, &id).await;
    let (seq, _) = post(&mut user, &format!("@{id} R6AFTERTURN")).await;
    until_state(&mut user, seq, &id, "pending").await;
    logs.assert_never_typed("R6AFTERTURN");

    scene.end_turn(&mut user, &logs, &id).await;

    until_state(&mut user, seq, &id, "taken").await;
    assert_eq!(logs.typed().matches("R6AFTERTURN").count(), 1);
    assert_eq!(logs.received("R6AFTERTURN"), 0);
}

// =========================================================================================
// R7: pending while in the inbox, taken once the CLI has it
// =========================================================================================

#[tokio::test]
async fn read_chat_reports_pending_until_the_cli_takes_the_post_and_taken_after() {
    let scene = Scene::start("r7-states", Opts::default()).await;
    let logs = scene.profile("hc-none", NONE_FLAGS, Some(NONE_DELIVERY));
    let mut user = scene.user().await;
    let id = scene.open_profile(&mut user, "hc-none").await;
    scene.start_turn(&mut user, &logs, &id).await;
    let (seq, _) = post(&mut user, &format!("@{id} R7STATES")).await;

    until_state(&mut user, seq, &id, "pending").await;
    assert_eq!(
        scene.entry(&id).inbox().pending_count(),
        1,
        "the post waits in the inbox"
    );
    scene.start_turn(&mut user, &logs, &id).await;

    until_state(&mut user, seq, &id, "taken").await;
    assert_eq!(scene.entry(&id).inbox().pending_count(), 0);
}

// =========================================================================================
// R8: send_message is still typed
// =========================================================================================

// Green before and after: `send_message` is the caller's own request to type, and keeps
// doing so into a window that declares `[delivery]`.
#[tokio::test]
async fn send_message_is_still_typed_into_a_window_that_declares_delivery() {
    let scene = Scene::start("r8-send-message", Opts::default()).await;
    let logs = scene.profile("hc-wait", WAIT_FLAGS, Some(WAIT_DELIVERY));
    let mut user = scene.user().await;
    let id = scene.open_profile(&mut user, "hc-wait").await;

    match request(
        &mut user,
        ClientMessage::SendText {
            window_id: id.clone(),
            text: "R8TYPED\r".to_string(),
        },
    )
    .await
    {
        DaemonMessage::TextSent { .. } => {}
        other => panic!("expected text_sent, got {other:?}"),
    }
    logs.until("the message to be typed", |logs| {
        logs.typed().contains("R8TYPED")
    })
    .await;
    assert_eq!(logs.received("R8TYPED"), 0);
}

// =========================================================================================
// R9: the daemon pushes nothing to a window in Unknown; the CLI's own doors still answer
// =========================================================================================

/// A profile without status hooks, so that a status line no rule matches leaves the window
/// `Unknown` with nothing to overrule it.
const WAIT_FLAGS_QUIET: &[&str] = &["--context", "--take", "--wait"];

async fn lose(scene: &Scene, client: &mut Connection, window_id: &str) {
    scene.type_line(window_id, "LOSE").await;
    wait_until_status(client, window_id, WindowStatus::Unknown, EVENT_TIMEOUT).await;
}

#[tokio::test]
async fn a_wait_in_an_unknown_window_is_not_answered_but_the_take_door_returns_the_post() {
    let scene = Scene::start("r9-wait", Opts::default()).await;
    let logs = scene.profile("hc-wait", WAIT_FLAGS_QUIET, Some(WAIT_DELIVERY));
    let mut user = scene.user().await;
    let id = scene.open_profile(&mut user, "hc-wait").await;
    scene.until_waiting(&id).await;
    lose(&scene, &mut user, &id).await;

    let (seq, _) = post(&mut user, &format!("@{id} R9WAIT")).await;
    until_state(&mut user, seq, &id, "pending").await;
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
    let scene = Scene::start("r9-wait-recovers", Opts::default()).await;
    let logs = scene.profile("hc-wait", WAIT_FLAGS_QUIET, Some(WAIT_DELIVERY));
    let mut user = scene.user().await;
    let id = scene.open_profile(&mut user, "hc-wait").await;
    scene.until_waiting(&id).await;
    lose(&scene, &mut user, &id).await;
    let (seq, _) = post(&mut user, &format!("@{id} R9RECOVERS")).await;
    until_state(&mut user, seq, &id, "pending").await;

    scene.type_line(&id, "FIND").await;

    logs.until_received("R9RECOVERS").await;
    until_state(&mut user, seq, &id, "taken").await;
    assert!(logs.door("wait").contains("R9RECOVERS"));
    assert_eq!(logs.received("R9RECOVERS"), 1);
}

#[tokio::test]
async fn the_wake_command_never_runs_for_a_window_in_unknown() {
    let scene = Scene::start("r9-command", Opts::default()).await;
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
    until_state(&mut user, seq, &id, "pending").await;
    let mut door = scene.connect().await;
    let taken = call_take(&mut door, &id).await;

    assert!(taken.contains("R9COMMAND"), "{taken:?}");
    assert!(logs.woken().is_empty(), "{:?}", logs.woken());
    logs.assert_never_typed("R9COMMAND");
}

#[tokio::test]
async fn a_window_with_no_wake_door_is_never_typed_into_while_unknown() {
    let scene = Scene::start("r9-none", Opts::default()).await;
    let logs = scene.profile("hc-none", &["--take"], Some(NONE_DELIVERY));
    let mut user = scene.user().await;
    let id = scene.open_profile(&mut user, "hc-none").await;
    lose(&scene, &mut user, &id).await;

    let (seq, _) = post(&mut user, &format!("@{id} R9NONE")).await;
    until_state(&mut user, seq, &id, "pending").await;
    let mut door = scene.connect().await;
    let taken = call_take(&mut door, &id).await;

    assert!(taken.contains("R9NONE"), "{taken:?}");
    logs.assert_never_typed("R9NONE");
}

// =========================================================================================
// R10: a window that declares no delivery, or whose hooks were left out, takes no part
// =========================================================================================

/// Checks that `window_id` is no participant: a post naming it and a broadcast both leave
/// it out, nothing the daemon sends of its own accord -- the intro included -- is typed
/// into it, and a handoff to it is undelivered for want of a way to deliver it.
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

    // Writes into one window are made one at a time: whatever the daemon had queued for it
    // is typed before this line is.
    match request(
        user,
        ClientMessage::SendText {
            window_id: window_id.to_string(),
            text: "R10SENTINEL\r".to_string(),
        },
    )
    .await
    {
        DaemonMessage::TextSent { .. } => {}
        other => panic!("expected text_sent, got {other:?}"),
    }
    logs.until("the sentinel to be typed", |logs| {
        logs.typed().contains("R10SENTINEL")
    })
    .await;
    for needle in [INTRO, "R10MENTION", "R10BROADCAST", "R10HANDOFF"] {
        logs.assert_never_typed(needle);
        assert_eq!(logs.received(needle), 0, "{needle}");
    }
    let _ = scene;
}

#[tokio::test]
async fn a_window_opened_from_a_raw_argv_takes_no_part_in_chat_or_notices() {
    let scene = Scene::start("r10-argv", Opts::default()).await;
    let mut user = scene.user().await;
    let logs = Logs {
        dir: scene.root.join("logs").join("argv"),
    };
    let id = scene.open_argv(&mut user, &logs.dir, WAIT_FLAGS).await;

    assert_takes_no_part(&scene, &mut user, &logs, &id).await;
}

#[tokio::test]
async fn a_window_whose_profile_declares_no_delivery_takes_no_part_in_chat_or_notices() {
    let scene = Scene::start("r10-no-table", Opts::default()).await;
    let logs = scene.profile("hc-plain", WAIT_FLAGS, None);
    let mut user = scene.user().await;
    let id = scene.open_profile(&mut user, "hc-plain").await;

    assert_takes_no_part(&scene, &mut user, &logs, &id).await;
}

#[tokio::test]
async fn a_window_whose_hooks_were_left_out_takes_no_part_in_chat_or_notices() {
    let scene = Scene::start("r10-hooks-left-out", Opts::default()).await;
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
// R11: a handoff answers once the CLI took it, or once it is known it never will
// =========================================================================================

#[tokio::test]
async fn a_handoff_not_taken_within_the_limit_is_undelivered_and_leaves_the_inbox() {
    let scene = Scene::start(
        "r11-timeout",
        Opts {
            limits: "handoff_delivery_timeout_secs = 1",
            handoffs_per_conn: Some(1),
            ..Opts::default()
        },
    )
    .await;
    let logs = scene.profile("hc-none", NONE_FLAGS, Some(NONE_DELIVERY));
    let mut user = scene.user().await;
    let id = scene.open_profile(&mut user, "hc-none").await;
    // Working, with no wake door: nothing takes the envelope until the next turn starts.
    scene.start_turn(&mut user, &logs, &id).await;

    let (_, _, delivery) =
        expect_handoff_sent(request(&mut user, handoff_to(&id, "R11TIMEOUT")).await);

    assert_eq!(delivery, "undelivered_timeout");
    assert_eq!(
        scene.entry(&id).inbox().pending_count(),
        0,
        "an envelope answered as undelivered is no longer waiting"
    );
    scene.start_turn(&mut user, &logs, &id).await;
    assert_eq!(
        logs.received("R11TIMEOUT"),
        0,
        "it never arrives after the answer"
    );
    logs.assert_never_typed("R11TIMEOUT");

    // The answer returned the handoff's slot, the connection's only one: the next handoff
    // is accepted.
    send(&mut user, &handoff_to(&id, "R11SECOND")).await;
    scene
        .until("the second envelope to wait in the inbox", || {
            scene.entry(&id).inbox().pending_count() == 1
        })
        .await;
    // `user` owes the handoff's reply, so the turn is watched on another connection.
    let mut watcher = scene.user().await;
    scene.start_turn(&mut watcher, &logs, &id).await;
    let (_, _, delivery) = expect_handoff_sent(read_reply(&mut user).await);
    assert_eq!(delivery, "taken");
}

#[tokio::test]
async fn a_handoff_to_a_working_window_answers_taken_once_its_next_turn_starts() {
    let scene = Scene::start("r11-taken-later", Opts::default()).await;
    let logs = scene.profile("hc-none", NONE_FLAGS, Some(NONE_DELIVERY));
    let mut user = scene.user().await;
    let id = scene.open_profile(&mut user, "hc-none").await;
    scene.start_turn(&mut user, &logs, &id).await;

    send(&mut user, &handoff_to(&id, "R11LATER")).await;
    scene
        .until("the envelope to wait in the inbox", || {
            scene.entry(&id).inbox().pending_count() == 1
        })
        .await;
    // `user` owes the handoff's reply, so the turn is watched on another connection.
    let mut watcher = scene.user().await;
    scene.start_turn(&mut watcher, &logs, &id).await;

    let (_, _, delivery) = expect_handoff_sent(read_reply(&mut user).await);
    assert_eq!(delivery, "taken");
    assert!(logs.door("take").contains("R11LATER"));
    assert_eq!(logs.received("R11LATER"), 1);
    logs.assert_never_typed("R11LATER");
}

#[tokio::test]
async fn a_handoff_that_opens_a_window_hands_the_envelope_over_as_its_launch_prompt() {
    let scene = Scene::start("r11-open", Opts::default()).await;
    let logs = scene.profile("hc-wait", WAIT_FLAGS, Some(WAIT_DELIVERY));
    let mut user = scene.user().await;

    let (window_id, opened, delivery) = expect_handoff_sent(
        request(
            &mut user,
            ClientMessage::SendHandoff {
                window_id: None,
                profile: Some("hc-wait".to_string()),
                cwd: None,
                name: Some("r11-opened".to_string()),
                model: None,
                effort: None,
                reference: "docs/handoff.md".to_string(),
                note: "R11OPENED".to_string(),
            },
        )
        .await,
    );

    assert!(opened);
    assert_eq!(delivery, "taken", "taken once the window started with it");
    logs.until("the window to log its argv", |logs| !logs.argv().is_empty())
        .await;
    assert!(
        logs.prompt().unwrap_or_default().contains("R11OPENED"),
        "the envelope is the launch prompt: {:?}",
        logs.argv()
    );
    wait_until_status(
        &mut user,
        &window_id,
        WindowStatus::WaitingInput,
        EVENT_TIMEOUT,
    )
    .await;
    logs.assert_never_typed("R11OPENED");
    assert_eq!(logs.received("R11OPENED"), 0, "handed over once, at launch");

    let stored = stored_command(&scene, "r11-opened").await;
    assert!(
        !stored.iter().any(|arg| arg.contains("R11OPENED")),
        "the envelope is not kept for a restore: {stored:?}"
    );
}

// =========================================================================================
// R13: a push under way is not handed out again; a failed push goes back to the inbox
// =========================================================================================

#[tokio::test]
async fn a_post_being_pushed_by_command_is_not_returned_by_a_turn_start_door() {
    let scene = Scene::start("r13-command-held", Opts::default()).await;
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
    until_state(&mut user, seq, &id, "taken").await;
    assert_eq!(logs.received("R13HELD"), 1);
}

#[tokio::test]
async fn a_failed_command_push_puts_the_post_back_for_the_next_turn() {
    let scene = Scene::start("r13-command-failed", Opts::default()).await;
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
    until_state(&mut user, seq, &id, "taken").await;
}

#[tokio::test]
async fn a_post_being_typed_is_not_returned_by_a_turn_start_door() {
    let scene = Scene::start("r13-typing-held", Opts::default()).await;
    let pause = scene.root.join("paint-pause");
    std::fs::write(&pause, b"").expect("create the pause file");
    let logs = scene.profile_with(
        "hc-none",
        &format!("pause|{}", pause.display()),
        NONE_FLAGS,
        Some(NONE_DELIVERY),
        "",
    );
    let mut user = scene.user().await;
    // The pause holds what is typed off the screen; the status line was painted at start.
    let id = scene.open_profile(&mut user, "hc-none").await;

    let (seq, _) = post(&mut user, &format!("@{id} R13TYPING")).await;
    logs.until("the daemon to start typing", |logs| {
        !logs.typed().is_empty()
    })
    .await;
    let mut door = scene.connect().await;
    let taken = call_take(&mut door, &id).await;
    std::fs::remove_file(&pause).expect("let the composer paint");

    assert!(
        !taken.contains("R13TYPING"),
        "a post being typed is not handed out again"
    );
    until_state(&mut user, seq, &id, "taken").await;
    assert_eq!(logs.typed().matches("R13TYPING").count(), 1);
    assert_eq!(logs.received("R13TYPING"), 0);
}
