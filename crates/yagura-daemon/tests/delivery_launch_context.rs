#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: `[delivery] launch_context` hands the room's introduction to a CLI
//! that has no start-up door, as part of the argv it is spawned with.
//!
//! The introduction goes only to a window that will take part in chat, which is decided
//! before the spawn from the same facts the start-up door's intro is decided from after
//! it: a `[delivery]` table, hooks that reach the CLI, a detection definition not excluded
//! from chat, and a chat that came up. Every window is a `hook_child`, which logs the argv
//! it was started with before it paints its waiting line, so a window seen waiting has
//! already logged it.

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use common::{
    CONTROL_INPUT_TABLE, approver_base_config, composer_section_toml, detection_config,
    empty_lookup, read_response, register_approver, registration_cwd_in, send, unique_temp_dir,
    wait_until_status, write_profile,
};
use yagura_daemon::profile::ProfileOs;
use yagura_daemon::{IpcServer, WindowRegistry};
use yagura_protocol::{ClientMessage, DaemonMessage, ProfileCapability, ProfileInfo, WindowStatus};
use yagura_test_support::ipc::Connection;
use yagura_test_support::process::{
    COMPOSER_CONTINUATION_PREFIX, COMPOSER_MARKER, COMPOSER_SEPARATOR,
};

const HOOK_DETECTION: &str = "hook-child";

/// The playbook's `[chat] intro`.
const INTRO: &str = "INTRO-6L-LAUNCH-CONTEXT";

const EVENT_TIMEOUT: Duration = Duration::from_secs(20);

/// The launch-context template; kept out of `format!` so `{context}` stays literal.
const LAUNCH_CONTEXT: &str = "launch_context = [\"--rules\", \"{context}\"]";

const LAUNCH_PROMPT: &str = "launch_prompt = [\"--prompt\", \"{prompt}\"]";

/// Where the registration's hooks entry lives, under the daemon's home directory.
const HOOKS_FILE: &str = "hc/hooks.json";

/// A registration whose only owned entry is a hooks file: the CLI runs the hooks it holds,
/// so a launch counts on them only while that file holds the entry.
const REGISTRATION: &str = "\n[mcp_registration]\nadd = [\"mcp\", \"add\", \"{server}\"]\n\
     remove = [\"mcp\", \"remove\", \"{server}\"]\n\n\
     [mcp_registration.status]\nfile = \"hc/mcp.json\"\nformat = \"json\"\n\
     command = \"/mcpServers/{server}/command\"\n\n\
     [mcp_registration.json_entry]\nfile = \"hc/hooks.json\"\n\n\
     [mcp_registration.json_entry.entries]\n\
     \"/hooks/{server}\" = { Stop = [{ type = \"command\", command = \"run-hook\" }] }\n";

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn hook_child_exe() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_hook_child"))
}

#[derive(Clone, Copy)]
struct Opts<'a> {
    /// `None` leaves `intro` out of the playbook, so the built-in text applies.
    intro: Option<&'a str>,
    exclude_hook_detection: bool,
    /// `false` gives the daemon no store, so chat does not come up.
    chat_store: bool,
}

impl Default for Opts<'_> {
    fn default() -> Self {
        Self {
            intro: Some(INTRO),
            exclude_hook_detection: false,
            chat_store: true,
        }
    }
}

fn playbook_toml(opts: &Opts<'_>) -> String {
    let mut chat = String::new();
    if let Some(intro) = opts.intro {
        chat.push_str(&format!("intro = \"{intro}\"\n"));
    }
    if opts.exclude_hook_detection {
        chat.push_str(&format!("exclude_detections = [\"{HOOK_DETECTION}\"]\n"));
    }
    format!(
        "[approval]\nauto_approve = [\"send_chat\", \"send_handoff\", \"open_window\", \
         \"close_window\"]\n\n[chat]\n{chat}"
    )
}

/// A daemon built the way `main.rs` builds one, on an endpoint of this test's own.
struct Scene {
    registry: Arc<WindowRegistry>,
    endpoint: String,
    root: PathBuf,
    home: PathBuf,
    chat_intro: String,
    _server: tokio::task::JoinHandle<()>,
}

impl Scene {
    async fn start(tag: &str, opts: Opts<'_>) -> Self {
        let root = unique_temp_dir(&format!("launch-context-{tag}"));
        let instance = format!(
            "lc{:x}{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        );
        let home = root.join("home");
        std::fs::create_dir_all(root.join("profiles")).expect("create the profiles directory");
        std::fs::create_dir_all(&home).expect("create the home directory");

        let source = playbook_toml(&opts);
        let policy = yagura_daemon::control::parse_policy(&source)
            .unwrap_or_else(|err| panic!("the playbook must load ({err}):\n{source}"));
        let mut config = detection_config(&root, approver_base_config());
        yagura_daemon::control::apply_limits(&mut config, policy.limits());
        yagura_daemon::control::apply_chat_section(&mut config, policy.chat.as_ref());
        config.approval_policy = Some(policy);
        config.playbook = Some(root.join("playbook.toml"));
        config.store_db_path_resolved = opts.chat_store.then(|| root.join("yagura.db"));
        config.profiles_dir = Some(root.join("profiles"));
        config.home_dir = Some(home.clone());
        config.registration_cwd = registration_cwd_in(&root);
        config.host_os = ProfileOs::host();
        // No PATH of the machine's, but its own command interpreter, which a `.cmd` needs.
        config.program_lookup = yagura_os::program::ProgramLookup {
            windows_comspec: yagura_os::program::ProgramLookup::from_env().windows_comspec,
            ..empty_lookup()
        };
        config.program_is_executable = |path| path.is_file();
        config.default_shell = None;
        write_hook_definition(&root);
        let chat_intro = config.chat_intro.clone();

        let user = yagura_os::PeerAuth::current_user_id(&yagura_os::PlatformIpc::default())
            .expect("the current user id must resolve");
        let namespace = yagura_os::resolve_instance_from(Some(std::ffi::OsStr::new(&instance)))
            .expect("the instance identifier must be valid");
        let endpoint = yagura_os::default_endpoint_owning(&user, &namespace);

        let registry = Arc::new(WindowRegistry::new(config.clone()));
        if opts.chat_store {
            yagura_daemon::store::wire_into(&registry, Some(root.join("yagura.db")));
        }
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
        let server = Arc::new(IpcServer::new_with_chat(
            registry.clone(),
            endpoint.clone(),
            config,
            chat,
        ));
        let handle = tokio::spawn(async move {
            let _ = server.run().await;
        });
        Self {
            registry,
            endpoint,
            root,
            home,
            chat_intro,
            _server: handle,
        }
    }

    async fn user(&self) -> Connection {
        let mut client =
            yagura_test_support::ipc::connect_with_retry(&self.endpoint, Duration::from_secs(5))
                .await;
        register_approver(&mut client).await;
        client
    }

    fn delivers(&self, window_id: &str) -> bool {
        self.registry
            .get(window_id)
            .expect("the window exists")
            .delivers()
    }

    /// Writes the hooks entry the registration owns, as a registration would.
    fn write_registered_hooks(&self) {
        let path = self.home.join(HOOKS_FILE);
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("create hc/");
        let entry = serde_json::json!({
            "hooks": { "yagura": { "Stop": [{ "type": "command", "command": "run-hook" }] } }
        });
        std::fs::write(&path, entry.to_string()).expect("write the hooks entry");
    }

    fn logs(&self, id: &str) -> PathBuf {
        let logs = self.root.join("logs").join(id);
        std::fs::create_dir_all(&logs).expect("create the log directory");
        logs
    }

    /// Writes profile `id`: a `hook_child` whose `[delivery]` table is `delivery`, followed
    /// by `extra_tables`. Returns where its window logs.
    fn profile(&self, id: &str, delivery: &str, extra_tables: &str) -> PathBuf {
        let logs = self.logs(id);
        let command = [
            hook_child_exe().display().to_string(),
            COMPOSER_MARKER.to_string(),
            COMPOSER_SEPARATOR.to_string(),
            COMPOSER_CONTINUATION_PREFIX.to_string(),
            "0".to_string(),
            "-".to_string(),
            "--log".to_string(),
            logs.display().to_string(),
        ]
        .iter()
        .map(|arg| format!("'{arg}'"))
        .collect::<Vec<_>>()
        .join(", ");
        write_profile(
            &self.root.join("profiles"),
            id,
            &format!(
                "schema = 1\ndisplay_name = \"{id}\"\ndetection = \"{HOOK_DETECTION}\"\n\n\
                 [command]\ndefault = [{command}]\n\n[delivery]\n{delivery}\n{extra_tables}"
            ),
        );
        logs
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
        wait_until_status(client, &id, WindowStatus::WaitingInput, EVENT_TIMEOUT).await;
        id
    }
}

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

fn delivery_with_context() -> String {
    format!("wake = \"none\"\n{LAUNCH_CONTEXT}\n{LAUNCH_PROMPT}")
}

/// The arguments after the program `hook_child` logged it was started with.
fn logged_argv(logs: &Path) -> Vec<String> {
    std::fs::read_to_string(logs.join("events.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find(|line| line["event"] == "argv")
        .and_then(|line| line["args"].as_array().cloned())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|arg| arg.as_str().map(str::to_string))
        .collect()
}

fn position(argv: &[String], element: &str) -> Option<usize> {
    argv.iter().position(|arg| arg == element)
}

/// Asserts the argv carries nothing of a launch context: no `--rules`, and no element
/// holding the introduction.
fn assert_no_launch_context(argv: &[String], intro: &str, what: &str) {
    assert!(!argv.is_empty(), "{what}: the window logged its argv");
    assert!(
        !argv
            .iter()
            .any(|arg| arg == "--rules" || arg.contains(intro)),
        "{what}: nothing of the launch context is added: {argv:?}"
    );
}

/// Reads the next response, skipping the pushes an approver connection is sent.
async fn read_reply(client: &mut Connection) -> DaemonMessage {
    loop {
        match read_response(client, EVENT_TIMEOUT).await {
            DaemonMessage::ChatChanged { .. }
            | DaemonMessage::ChatParticipantsChanged
            | DaemonMessage::PlanChanged { .. }
            | DaemonMessage::ApprovalRequested { .. } => continue,
            other => return other,
        }
    }
}

async fn list_profiles(client: &mut Connection) -> (Vec<ProfileInfo>, Vec<String>) {
    send(client, &ClientMessage::ListProfiles).await;
    match read_reply(client).await {
        DaemonMessage::ProfileList { profiles, warnings } => (profiles, warnings),
        other => panic!("expected profile_list, got {other:?}"),
    }
}

// =========================================================================================
// a participant is started with the introduction, before the launch prompt
// =========================================================================================

#[tokio::test]
async fn a_participant_opened_with_a_prompt_gets_the_intro_as_its_launch_context_before_the_prompt()
{
    let scene = Scene::start("participant", Opts::default()).await;
    scene.write_registered_hooks();
    let logs = scene.profile("hc-context", &delivery_with_context(), REGISTRATION);
    let mut user = scene.user().await;

    send(
        &mut user,
        &ClientMessage::SendHandoff {
            window_id: None,
            profile: Some("hc-context".to_string()),
            cwd: None,
            name: None,
            model: None,
            effort: None,
            reference: "docs/handoff.md".to_string(),
            note: "R6HANDOFFNOTE".to_string(),
        },
    )
    .await;
    let window_id = match read_reply(&mut user).await {
        DaemonMessage::HandoffSent {
            window_id, opened, ..
        } => {
            assert!(opened, "the handoff opened a window");
            window_id
        }
        other => panic!("expected handoff_sent, got {other:?}"),
    };
    wait_until_status(
        &mut user,
        &window_id,
        WindowStatus::WaitingInput,
        EVENT_TIMEOUT,
    )
    .await;
    assert!(
        scene.delivers(&window_id),
        "the registered hooks held, so the window takes part in chat"
    );

    let argv = logged_argv(&logs);
    let rules = position(&argv, "--rules")
        .unwrap_or_else(|| panic!("the intro is handed over at launch: {argv:?}"));
    assert_eq!(
        argv.get(rules + 1).map(String::as_str),
        Some(INTRO),
        "the launch context carries the configured intro and nothing else: {argv:?}"
    );
    assert_eq!(
        argv.iter().filter(|arg| *arg == "--rules").count(),
        1,
        "{argv:?}"
    );
    let prompt = position(&argv, "--prompt")
        .unwrap_or_else(|| panic!("the envelope is the launch prompt: {argv:?}"));
    assert!(
        prompt > rules + 1,
        "the launch prompt stays after the launch context: {argv:?}"
    );
    assert!(
        argv.get(prompt + 1)
            .is_some_and(|envelope| envelope.contains("R6HANDOFFNOTE")),
        "{argv:?}"
    );
}

#[tokio::test]
async fn a_participant_under_a_playbook_without_an_intro_gets_the_built_in_one() {
    let scene = Scene::start(
        "default-intro",
        Opts {
            intro: None,
            ..Opts::default()
        },
    )
    .await;
    assert!(!scene.chat_intro.is_empty(), "the built-in intro has text");
    scene.write_registered_hooks();
    let logs = scene.profile("hc-context", &delivery_with_context(), REGISTRATION);
    let mut user = scene.user().await;

    let id = scene.open_profile(&mut user, "hc-context").await;
    assert!(scene.delivers(&id), "the registered hooks held");

    let argv = logged_argv(&logs);
    let rules = position(&argv, "--rules")
        .unwrap_or_else(|| panic!("the intro is handed over at launch: {argv:?}"));
    assert_eq!(
        argv.get(rules + 1),
        Some(&scene.chat_intro),
        "the built-in intro passes through unchanged: {argv:?}"
    );
    assert_eq!(position(&argv, "--prompt"), None, "no prompt was asked for");
}

// =========================================================================================
// a window that will not take part is started with nothing added
// =========================================================================================

#[tokio::test]
async fn a_window_that_will_not_take_part_in_chat_is_started_without_a_launch_context() {
    // Its hooks entry was never written, so the launch goes without the hooks.
    let scene = Scene::start("left-out", Opts::default()).await;
    let logs = scene.profile("hc-context", &delivery_with_context(), REGISTRATION);
    let mut user = scene.user().await;
    let id = scene.open_profile(&mut user, "hc-context").await;
    assert!(!scene.delivers(&id), "the hooks were left out");
    assert_no_launch_context(&logged_argv(&logs), INTRO, "hooks left out");

    // Its detection definition is excluded from chat.
    let scene = Scene::start(
        "excluded",
        Opts {
            exclude_hook_detection: true,
            ..Opts::default()
        },
    )
    .await;
    scene.write_registered_hooks();
    let logs = scene.profile("hc-context", &delivery_with_context(), REGISTRATION);
    let mut user = scene.user().await;
    let id = scene.open_profile(&mut user, "hc-context").await;
    assert!(scene.delivers(&id), "the registered hooks held");
    assert_no_launch_context(&logged_argv(&logs), INTRO, "excluded detection");

    // Chat did not come up.
    let scene = Scene::start(
        "no-chat",
        Opts {
            chat_store: false,
            ..Opts::default()
        },
    )
    .await;
    scene.write_registered_hooks();
    let logs = scene.profile("hc-context", &delivery_with_context(), REGISTRATION);
    let mut user = scene.user().await;
    let id = scene.open_profile(&mut user, "hc-context").await;
    assert!(scene.delivers(&id), "the registered hooks held");
    assert_no_launch_context(&logged_argv(&logs), INTRO, "chat disabled");
}

// =========================================================================================
// the template must name the introduction
// =========================================================================================

#[tokio::test]
async fn a_launch_context_without_the_context_placeholder_takes_its_profile_out_of_the_listing() {
    let scene = Scene::start("template", Opts::default()).await;
    scene.profile("hc-context", &delivery_with_context(), "");
    scene.profile(
        "hc-no-context",
        &format!("wake = \"none\"\nlaunch_context = [\"--rules\"]\n{LAUNCH_PROMPT}"),
        "",
    );
    let mut user = scene.user().await;

    let (profiles, warnings) = list_profiles(&mut user).await;
    let ids: Vec<&str> = profiles.iter().map(|profile| profile.id.as_str()).collect();
    assert!(
        ids.contains(&"hc-context"),
        "a template naming {{context}} is accepted: {ids:?}; {warnings:?}"
    );
    assert!(
        !ids.contains(&"hc-no-context"),
        "a template without {{context}} is refused: {ids:?}"
    );
    assert!(
        warnings.iter().any(|line| line.contains("hc-no-context")),
        "the refusal is reported: {warnings:?}"
    );
}

// =========================================================================================
// a program that is a `.cmd` shim is opened without the introduction, and says so
// =========================================================================================

#[cfg(windows)]
#[tokio::test]
async fn a_participant_whose_program_is_a_cmd_shim_opens_without_a_launch_context_and_lists_the_limit()
 {
    let scene = Scene::start("shim", Opts::default()).await;
    scene.write_registered_hooks();
    let logs = scene.logs("hc-shim");
    // The shim's own line carries the fixture's arguments, so only what the daemon adds
    // passes through the command interpreter.
    let shim = scene.root.join("hc-shim.cmd");
    std::fs::write(
        &shim,
        format!(
            "@\"{}\" {COMPOSER_MARKER} \"{COMPOSER_SEPARATOR}\" {COMPOSER_CONTINUATION_PREFIX} \
             0 - --log \"{}\" %*\r\n",
            hook_child_exe().display(),
            logs.display()
        ),
    )
    .expect("write the shim");
    write_profile(
        &scene.root.join("profiles"),
        "hc-shim",
        &format!(
            "schema = 1\ndisplay_name = \"hc-shim\"\ndetection = \"{HOOK_DETECTION}\"\n\n\
             [command]\ndefault = ['{}']\n\n[delivery]\nwake = \"none\"\n{LAUNCH_CONTEXT}\n\
             {REGISTRATION}",
            shim.display()
        ),
    );
    let mut user = scene.user().await;

    let id = scene.open_profile(&mut user, "hc-shim").await;
    assert!(scene.delivers(&id), "the registered hooks held");
    assert_no_launch_context(&logged_argv(&logs), INTRO, "through a shim");

    let (profiles, warnings) = list_profiles(&mut user).await;
    let listed = profiles
        .iter()
        .find(|profile| profile.id == "hc-shim")
        .unwrap_or_else(|| panic!("the shim profile is listed: {warnings:?}"));
    assert!(listed.available, "{listed:?}");
    assert!(
        listed
            .limits
            .iter()
            .any(|limit| limit.capability == ProfileCapability::LaunchContext),
        "the listing says the intro cannot be handed over at launch: {:?}",
        listed.limits
    );
}
