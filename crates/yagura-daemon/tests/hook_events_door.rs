#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// Starts the compiled `yagura-daemon` binary as a real child process, the way a CLI runs
// its hook: what the CLI sees is whether its write of the hook input succeeded, the
// hook's output and its exit code.
#![allow(clippy::disallowed_methods)]

//! Integration test: the `--hook <profile-id> <Event>` door, which does what the
//! `[hook_events]` table of the named profile says, and a window's door given an argument
//! this copy does not know.
//!
//! The window is opened with a raw argv, so its own profile has no `[hook_events]`: the
//! table must come from the profile the hook names. A window opened from a profile whose
//! hooks the daemon arranged answers only a hook naming that profile. Every spawn sets
//! `YAGURA_INSTANCE` to an identifier of this test's own and points the data, state and
//! config base directories into a temporary directory, so it never reaches a running daemon
//! of the user.

use crate::common;

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

use yagura_daemon::inbox::Parcel;
use yagura_daemon::mcp_registration::{EntryVars, RegistrationTarget};
use yagura_daemon::profile::parse_profile;
use yagura_daemon::window::{HooksRoute, LaunchIdentity, WindowProvenance};
use yagura_os::program::HostOs;
use yagura_protocol::WindowStatus;
use yagura_test_support::paths::TempDir;

use common::{
    Harness, VALID_TOKEN, approver_base_config, detection_config, query_status, unique_temp_dir,
    wait_until_status, write_profile,
};

const WINDOW_ID_VAR: &str = "YAGURA_WINDOW_ID";
const STATUS_TOKEN_VAR: &str = "YAGURA_STATUS_TOKEN";
const PROFILE_ID: &str = "hook-door-fixture";
const EVENT: &str = "PostToolUse";
/// Two more events of [`PROFILE_ID`]'s, which name no `skip_if` of their own.
const APPROVAL_EVENT: &str = "PermissionRequest";
const END_EVENT: &str = "Stop";
const NO_RULES_DETECTION: &str = "hook-door-no-rules";
const NO_RULES_DEFINITION: &str = "schema_version = 1
name = \"hook-door-no-rules\"
rules = []

[debounce]
confirm_ms = 30
";
/// A definition with no rules that confirms a status as soon as it is judged, so every
/// status the window is given is published, in the order it was given.
const NO_RULES_AT_ONCE_DETECTION: &str = "hook-door-no-rules-at-once";
const NO_RULES_AT_ONCE_DEFINITION: &str = "schema_version = 1
name = \"hook-door-no-rules-at-once\"
rules = []

[debounce]
confirm_ms = 0
";
const LARGE_INPUT_BYTES: usize = 1024 * 1024;
const EVENT_FORM_PROFILE_ID: &str = "hook-door-event-form";
/// A screen the definition reads as waiting for input, so a window shows `waiting_input`
/// whenever no reported turn is open, and `running` while one is.
const READY_SCREEN: &str = "STOP-DOOR-READY";
const MARKED_DETECTION: &str = "hook-door-marked";

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn daemon_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_yagura-daemon"))
}

fn unique_instance() -> String {
    format!(
        "hevt{:x}{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

/// A profile with every kind of `[hook_events]` entry in one event, and a `[delivery]`
/// whose output form is told apart from plain text.
fn fixture_profile_source() -> String {
    format!(
        "schema = 1\ndisplay_name = \"Hook Door Fixture\"\n\n\
         [command]\ndefault = ['{}']\n\n\
         [delivery]\noutput = '{{\"fixtureOut\":{{text}}}}'\n\n\
         [hook_events]\n{EVENT} = {{ status = \"running\", deliver = \"take\", \
         session = \"/session_id\", skip_if = \"/subagentType\" }}\n\
         {APPROVAL_EVENT} = {{ status = \"waiting_approval\" }}\n\
         {END_EVENT} = {{ status = \"waiting_input\", deliver = \"take\", \
         session = \"/session_id\" }}\n",
        common::echo_child_path().display()
    )
}

/// A profile whose take form names the event its hook ran for.
fn event_form_profile_source() -> String {
    format!(
        "schema = 1\ndisplay_name = \"Hook Door Event Form\"\n\n\
         [command]\ndefault = ['{}']\n\n\
         [delivery.output]\n\
         take = '{{\"hookSpecificOutput\":{{\"hookEventName\":{{event}},\"additionalContext\":{{text}}}}}}'\n\n\
         [hook_events]\nPostToolUse = {{ deliver = \"take\" }}\nUserPromptSubmit = {{ deliver = \"take\" }}\n",
        common::echo_child_path().display()
    )
}

fn door_command(
    args: &[&str],
    instance: &str,
    bases: &TempDir,
    window: Option<(&str, &str)>,
) -> Command {
    let mut command = Command::new(daemon_bin());
    command
        .args(args)
        .env("YAGURA_INSTANCE", instance)
        .env_remove("YAGURA_STORE_DB")
        .env_remove(WINDOW_ID_VAR)
        .env_remove(STATUS_TOKEN_VAR)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if cfg!(windows) {
        command
            .env("LOCALAPPDATA", bases.make_dir("local"))
            .env("APPDATA", bases.make_dir("roaming"));
    } else {
        command
            .env("XDG_DATA_HOME", bases.make_dir("data"))
            .env("XDG_STATE_HOME", bases.make_dir("state"))
            .env("XDG_CONFIG_HOME", bases.make_dir("config"))
            .env("HOME", bases.make_dir("home"));
    }
    if let Some((window_id, token)) = window {
        command
            .env(WINDOW_ID_VAR, window_id)
            .env(STATUS_TOKEN_VAR, token);
    }
    command
}

/// Runs `command` with `input` written on its stdin from a thread of its own, as a CLI
/// writes the hook input; the returned result is that write's.
fn run_with_input(mut command: Command, input: Vec<u8>) -> (Output, std::io::Result<()>) {
    let mut child = command
        .stdin(Stdio::piped())
        .spawn()
        .expect("spawn the yagura-daemon binary");
    let mut pipe = child.stdin.take().expect("stdin");
    let writer = std::thread::spawn(move || pipe.write_all(&input).and_then(|()| pipe.flush()));
    let output = child.wait_with_output().expect("wait for the child");
    let written = writer.join().expect("writer thread");
    (output, written)
}

fn describe(output: &Output) -> String {
    format!(
        "exit {:?}; stdout: {:?}; stderr: {:?}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// A JSON object of about [`LARGE_INPUT_BYTES`] followed by `tail` (`,"k":"v"` fields or
/// nothing).
fn large_input(tail: &str) -> Vec<u8> {
    format!(r#"{{"pad":"{}"{tail}}}"#, "x".repeat(LARGE_INPUT_BYTES)).into_bytes()
}

struct Setup {
    h: Harness,
    instance: String,
    bases: TempDir,
    window_id: String,
    profiles_dir: PathBuf,
}

/// What a window's screen shows its detection definition.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Screen {
    /// An idle prompt.
    Idle,
    /// Nothing a rule matches, so a reported status is not outranked by a screen rule.
    NoRules,
    /// [`Screen::NoRules`], under a definition that confirms a status at once.
    NoRulesAtOnce,
    /// [`READY_SCREEN`]: `waiting_input` unless a reported turn is open.
    Ready,
}

impl Setup {
    async fn start() -> Self {
        Self::start_with(Screen::Idle, false).await
    }

    /// A window whose detection definition has no rule matching its screen, so a
    /// reported status is not outranked by a screen rule.
    async fn start_without_screen_rules() -> Self {
        Self::start_with(Screen::NoRules, false).await
    }

    /// A window showing [`READY_SCREEN`], whose daemon reads the shipped profiles, so a
    /// hook names a CLI's own profile as that CLI's registration does.
    async fn start_with_shipped_profiles() -> Self {
        Self::start_with(Screen::Ready, true).await
    }

    async fn start_with(screen: Screen, shipped_profiles: bool) -> Self {
        Self::start_with_config(screen, shipped_profiles, |_| {}).await
    }

    /// [`Setup::start_with`], with the daemon's configuration changed by `adjust` first.
    async fn start_with_config(
        screen: Screen,
        shipped_profiles: bool,
        adjust: impl FnOnce(&mut yagura_daemon::DaemonConfig),
    ) -> Self {
        let instance = unique_instance();
        let profiles_dir = unique_temp_dir("hook-events-profiles");
        std::fs::create_dir_all(&profiles_dir).expect("create the profiles directory");
        write_profile(&profiles_dir, PROFILE_ID, &fixture_profile_source());
        write_profile(
            &profiles_dir,
            EVENT_FORM_PROFILE_ID,
            &event_form_profile_source(),
        );
        let detection_dir = unique_temp_dir("hook-events");
        let mut config = detection_config(&detection_dir, approver_base_config());
        config.profiles_dir = Some(profiles_dir.clone());
        config.bundled_profiles = shipped_profiles;
        adjust(&mut config);
        let h = Harness::start_on_instance(&instance, config).await;
        // The window is opened with a raw argv: no profile of its own declares
        // `[hook_events]`.
        let window_id = match screen {
            Screen::NoRules | Screen::NoRulesAtOnce => {
                let (name, definition) = if screen == Screen::NoRules {
                    (NO_RULES_DETECTION, NO_RULES_DEFINITION)
                } else {
                    (NO_RULES_AT_ONCE_DETECTION, NO_RULES_AT_ONCE_DEFINITION)
                };
                std::fs::write(detection_dir.join(format!("{name}.toml")), definition)
                    .expect("write the rule-less detection definition");
                h.open_delivering(
                    yagura_test_support::process::echo_child_command(),
                    Some(name),
                )
                .await
            }
            Screen::Ready => {
                let mut client = h.connect().await;
                common::open_marked_delivering_window(
                    &h,
                    &mut client,
                    &detection_dir,
                    MARKED_DETECTION,
                    READY_SCREEN,
                    None,
                )
                .await
            }
            Screen::Idle => {
                let mut client = h.connect().await;
                h.open_idle_window(&mut client).await
            }
        };
        Self {
            h,
            instance,
            bases: TempDir::new("yagura-door", "bases"),
            window_id,
            profiles_dir,
        }
    }

    fn entry(&self) -> std::sync::Arc<yagura_daemon::WindowEntry> {
        self.h.registry.get(&self.window_id).expect("window exists")
    }

    async fn run(&self, args: &[&str], input: Vec<u8>) -> (Output, std::io::Result<()>) {
        self.run_in(&self.window_id, args, input).await
    }

    /// Runs the door `args` in the window `window_id`, with `input` on its stdin.
    async fn run_in(
        &self,
        window_id: &str,
        args: &[&str],
        input: Vec<u8>,
    ) -> (Output, std::io::Result<()>) {
        let command = door_command(
            args,
            &self.instance,
            &self.bases,
            Some((window_id, VALID_TOKEN)),
        );
        tokio::task::spawn_blocking(move || run_with_input(command, input))
            .await
            .expect("join")
    }

    async fn status(&self) -> WindowStatus {
        let mut client = self.h.connect().await;
        query_status(&mut client, &self.window_id).await
    }

    fn deposit(&self, text: &str) {
        self.entry().inbox().deposit(Parcel::new("a", text));
    }
}

fn hook_args() -> [&'static str; 3] {
    ["--hook", PROFILE_ID, EVENT]
}

// --- the named profile's table decides, not the window's -------------------------------

/// Runs `name` of [`PROFILE_ID`] with `input` and checks that it printed nothing, exited 0,
/// recorded no session and left `pending` parcels in the inbox; returns how many statuses
/// it reported.
async fn run_quiet_event(setup: &Setup, name: &str, input: &[u8], pending: usize) -> u64 {
    let entry = setup.entry();
    let before = entry.status_event_count();
    let (output, written) = setup
        .run(&["--hook", PROFILE_ID, name], input.to_vec())
        .await;
    assert!(written.is_ok(), "{name}: {written:?}");
    assert!(
        output.status.code() == Some(0) && output.stdout.is_empty(),
        "{name}: a subagent's hook prints nothing and exits 0; {}",
        describe(&output)
    );
    assert_eq!(
        entry.inbox().pending_count(),
        pending,
        "{name}: a subagent's hook leaves the parcel in the inbox"
    );
    assert_eq!(
        entry.session_id(),
        None,
        "{name}: a subagent's hook records no session"
    );
    entry.status_event_count() - before
}

/// A subagent's hook is told apart on every event of a profile that names the pointer
/// once, here through an older `skip_if` written on one event alone. Its `running` and
/// `waiting_approval` are the window's (a subagent's work is the window's work); its
/// session, its take and any other status are not, since the session and the inbox are the
/// main session's and a subagent's end is not the end of the window's turn.
#[tokio::test]
async fn a_subagent_s_hook_reports_only_running_or_waiting_for_approval_on_every_event_of_its_profile()
 {
    let setup = Setup::start_without_screen_rules().await;
    let mut client = setup.h.connect().await;
    setup.deposit("PARCEL-FOR-THE-MAIN-SESSION");
    let subagent = br#"{"subagentType":"explore","session_id":"sub-1"}"#;

    let reported = run_quiet_event(&setup, EVENT, subagent, 1).await;
    assert_eq!(reported, 1, "{EVENT}: a subagent's running is reported");
    wait_until_status(&mut client, &setup.window_id, WindowStatus::Running).await;

    let reported = run_quiet_event(&setup, APPROVAL_EVENT, subagent, 1).await;
    assert_eq!(
        reported, 1,
        "{APPROVAL_EVENT}: a subagent's waiting_approval is reported"
    );
    wait_until_status(&mut client, &setup.window_id, WindowStatus::WaitingApproval).await;

    let reported = run_quiet_event(&setup, END_EVENT, subagent, 1).await;
    assert_eq!(
        reported, 0,
        "{END_EVENT}: a subagent's waiting_input is not reported, though the event names no \
         skip_if of its own"
    );

    // The main session's end does all its event says.
    let before = setup.entry().status_event_count();
    let (output, written) = setup
        .run(
            &["--hook", PROFILE_ID, END_EVENT],
            br#"{"session_id":"main-1"}"#.to_vec(),
        )
        .await;
    assert!(written.is_ok(), "{written:?}");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("PARCEL-FOR-THE-MAIN-SESSION"),
        "the main session's hook takes the parcel; {}",
        describe(&output)
    );
    assert_eq!(setup.entry().inbox().pending_count(), 0);
    assert_eq!(setup.entry().session_id().as_deref(), Some("main-1"));
    assert_eq!(setup.entry().status_event_count(), before + 1);
}

/// Every event of a profile that names the subagent pointer asks for its value, so an
/// event whose own entry names none still tells a subagent's hook apart.
#[tokio::test]
async fn every_event_of_a_profile_that_names_the_subagent_pointer_asks_for_its_value() {
    use yagura_protocol::{ClientMessage, DaemonMessage};

    let setup = Setup::start_without_screen_rules().await;
    for name in [EVENT, APPROVAL_EVENT, END_EVENT] {
        let mut door = setup.h.connect().await;
        common::send(
            &mut door,
            &ClientMessage::RunHook {
                window_id: setup.window_id.clone(),
                token: VALID_TOKEN.to_string(),
                profile: PROFILE_ID.to_string(),
                name: name.to_string(),
                input: None,
                held: true,
                guest: None,
            },
        )
        .await;
        match common::read_response(&mut door).await {
            DaemonMessage::HookInputWanted { pointers } => assert!(
                pointers
                    .iter()
                    .any(|wanted| wanted.pointer == "/subagentType"),
                "{name}: the subagent pointer is asked for; asked {pointers:?}"
            ),
            other => panic!("{name}: the event's values are asked for: {other:?}"),
        }
    }
}

/// A Copilot CLI hook is a subagent's when its input has `agentId`, a Grok Build hook when
/// it has `subagentType`, and a Codex CLI hook when it has `agent_id`; a Muse Code hook is
/// always the main session's. A subagent's hook reports only `running` and
/// `waiting_approval`; it takes, hands over and records nothing else, while the same
/// profile's hook of the main session still takes.
#[tokio::test]
async fn a_shipped_profile_s_subagent_hooks_report_only_running_or_waiting_for_approval() {
    const COPILOT_SUB: &str = r#"{"agentId":"sub-1","sessionId":"s-1","stop_hook_active":false}"#;
    const GROK_SUB: &str = r#"{"subagentType":"explore","session_id":"s-1","stop_hook_active":false,"stopHookActive":false}"#;
    const CODEX_SUB: &str = r#"{"agent_id":"sub-1","session_id":"sub-session-1"}"#;
    // Every hook below reports at most one status, and none may be lost to the rate limit.
    let setup = Setup::start_with_config(Screen::NoRules, true, |config| {
        config.status_event_burst = 1_000;
    })
    .await;
    setup.deposit("PARCEL-FOR-THE-MAIN-SESSION");
    let copilot: &[(&str, bool)] = &[
        ("sessionStart", false),
        ("userPromptSubmitted", true),
        ("preToolUse", true),
        ("postToolUse", true),
        ("postToolUseFailure", true),
        ("agentStop", false),
    ];
    let grok: &[(&str, bool)] = &[
        ("UserPromptSubmit", true),
        ("PreToolUse", true),
        ("PostToolUse", true),
        ("PostToolUseFailure", true),
        ("Notification", true),
        ("Stop", false),
        ("StopFailure", false),
        ("StopCancelled", false),
    ];
    let codex: &[(&str, bool)] = &[
        ("SessionStart", false),
        ("UserPromptSubmit", true),
        ("PreToolUse", true),
        ("PostToolUse", true),
        ("PermissionRequest", true),
        ("Stop", false),
        ("Interrupt", false),
    ];
    let copilot_main = ("postToolUse", r#"{"sessionId":"s-1"}"#);
    let grok_main = ("PostToolUse", r#"{"session_id":"s-1"}"#);
    let codex_main = ("UserPromptSubmit", r#"{"session_id":"main-1"}"#);
    // (profile, a subagent's input, its events and whether each reports a status, an event
    // of the main session's that takes and its input)
    let cases = [
        ("copilot-cli", COPILOT_SUB, copilot, copilot_main),
        ("wsl-copilot-cli", COPILOT_SUB, copilot, copilot_main),
        ("grok-build", GROK_SUB, grok, grok_main),
        ("wsl-grok-build", GROK_SUB, grok, grok_main),
        ("codex-cli", CODEX_SUB, codex, codex_main),
        ("wsl-codex-cli", CODEX_SUB, codex, codex_main),
    ];
    // A `wsl-*` profile has a Windows argv only, so no other host lists it.
    let cases = cases
        .into_iter()
        .filter(|(profile, ..)| cfg!(windows) || !profile.starts_with("wsl-"));
    let entry = setup.entry();
    for (profile, subagent, events, (take, main)) in cases {
        for &(name, reports) in events {
            let who = format!("{profile} {name}");
            let before = entry.status_event_count();
            let session = entry.session_id();
            let (output, written) = setup
                .run(&["--hook", profile, name], subagent.as_bytes().to_vec())
                .await;
            assert!(written.is_ok(), "{who}: {written:?}");
            assert!(
                output.status.code() == Some(0) && output.stdout.is_empty(),
                "{who}: a subagent's hook prints nothing and exits 0; {}",
                describe(&output)
            );
            assert_eq!(
                entry.inbox().pending_count(),
                1,
                "{who}: a subagent's hook leaves the parcel in the inbox"
            );
            assert_eq!(
                entry.session_id(),
                session,
                "{who}: a subagent's hook records no session"
            );
            assert_eq!(
                entry.status_event_count() - before,
                u64::from(reports),
                "{who}: a subagent's hook reports its status only when it is running or \
                 waiting for approval"
            );
        }
        let (output, written) = setup
            .run(&["--hook", profile, take], main.as_bytes().to_vec())
            .await;
        assert!(written.is_ok(), "{profile} {take}: {written:?}");
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("PARCEL-FOR-THE-MAIN-SESSION"),
            "{profile} {take}: the main session's hook takes the parcel; {}",
            describe(&output)
        );
        assert_eq!(entry.inbox().pending_count(), 0, "{profile} {take}");
        setup.deposit("PARCEL-FOR-THE-MAIN-SESSION");
    }

    // Muse Code names no subagent pointer: whatever its input holds, its hook is the main
    // session's.
    let (output, written) = setup
        .run(
            &["--hook", "muse-code", "UserPromptSubmit"],
            br#"{"agentId":"x","subagentType":"x","agent_id":"x"}"#.to_vec(),
        )
        .await;
    assert!(written.is_ok(), "{written:?}");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("PARCEL-FOR-THE-MAIN-SESSION"),
        "a profile that names no subagent pointer takes for every hook; {}",
        describe(&output)
    );
}

/// The path of keys at which a profile names the pointer that tells a subagent's hook
/// apart, read from the shipped Copilot CLI profile, which names `/agentId` there once and
/// nowhere else outside `[hook_events]`.
pub(crate) fn subagent_pointer_path() -> Vec<String> {
    fn collect(value: &toml::Value, path: &mut Vec<String>, found: &mut Vec<Vec<String>>) {
        match value {
            toml::Value::String(text) if text == "/agentId" => found.push(path.clone()),
            toml::Value::Table(table) => {
                for (key, value) in table {
                    path.push(key.clone());
                    collect(value, path, found);
                    path.pop();
                }
            }
            _ => {}
        }
    }
    let document: toml::Value = common::docs::read_doc_verbatim("../../profiles/copilot-cli.toml")
        .parse()
        .expect("the shipped Copilot CLI profile is TOML");
    let mut found = Vec::new();
    collect(&document, &mut Vec::new(), &mut found);
    found.retain(|path| path.first().map(String::as_str) != Some("hook_events"));
    assert_eq!(
        found.len(),
        1,
        "the shipped Copilot CLI profile names its subagent pointer once, outside \
         [hook_events]; found it at {found:?}"
    );
    found.remove(0)
}

/// `source` with the profile's subagent pointer set to `pointer`, at the place
/// [`subagent_pointer_path`] reads it from.
pub(crate) fn with_subagent_pointer(source: &str, pointer: &str) -> String {
    let mut document: toml::Table = source.parse().expect("the profile source is TOML");
    let path = subagent_pointer_path();
    let (key, tables) = path.split_last().expect("a path has a key");
    let mut table = &mut document;
    for name in tables {
        table = table
            .entry(name.clone())
            .or_insert_with(|| toml::Value::Table(toml::Table::new()))
            .as_table_mut()
            .expect("the subagent pointer is read from inside tables");
    }
    table.insert(key.clone(), toml::Value::String(pointer.to_string()));
    toml::to_string(&document).expect("the profile encodes as TOML")
}

/// A profile that names the subagent pointer and also keeps an older `skip_if` uses the
/// pointer it names, and reports the older key.
#[tokio::test]
async fn a_profile_naming_the_subagent_pointer_uses_it_over_an_older_skip_if() {
    let setup = Setup::start_without_screen_rules().await;
    let id = "hook-door-both-keys";
    let source = with_subagent_pointer(&fixture_profile_source(), "/agentId");
    let parsed = parse_profile(id, &source).expect("the profile is read");
    assert!(
        !parsed.warnings.is_empty(),
        "the older skip_if beside the pointer is reported"
    );
    write_profile(&setup.profiles_dir, id, &source);
    setup.deposit("PARCEL-FOR-THE-MAIN-SESSION");

    let (output, written) = setup
        .run(
            &["--hook", id, EVENT],
            br#"{"agentId":"sub-1","session_id":"sub-1"}"#.to_vec(),
        )
        .await;
    assert!(written.is_ok(), "{written:?}");
    assert!(
        output.status.code() == Some(0) && output.stdout.is_empty(),
        "a hook with a value at the named pointer is a subagent's; {}",
        describe(&output)
    );
    assert_eq!(setup.entry().inbox().pending_count(), 1);
    assert_eq!(setup.entry().session_id(), None);

    let (output, written) = setup
        .run(
            &["--hook", id, EVENT],
            br#"{"subagentType":"explore","session_id":"main-1"}"#.to_vec(),
        )
        .await;
    assert!(written.is_ok(), "{written:?}");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("PARCEL-FOR-THE-MAIN-SESSION"),
        "a hook with a value only at the older skip_if is the main session's; {}",
        describe(&output)
    );
    assert_eq!(setup.entry().session_id().as_deref(), Some("main-1"));
}

#[tokio::test]
async fn a_hook_without_the_skip_value_records_status_and_session_and_prints_in_the_profiles_form()
{
    let setup = Setup::start_without_screen_rules().await;
    setup.deposit("PARCEL-FOR-THE-MAIN-SESSION");
    let input = large_input(r#","session_id":"sess-7""#);

    let (output, written) = setup.run(&hook_args(), input).await;

    assert!(
        written.is_ok(),
        "the CLI's write of a large hook input must succeed, got {written:?}"
    );
    assert_eq!(output.status.code(), Some(0), "{}", describe(&output));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let printed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|error| {
        panic!(
            "stdout must be the profile's JSON form ({error}); {}",
            describe(&output)
        )
    });
    assert!(
        printed["fixtureOut"]
            .as_str()
            .is_some_and(|text| text.contains("PARCEL-FOR-THE-MAIN-SESSION")),
        "the take must print through the profile's [delivery].output; {}",
        describe(&output)
    );
    assert_eq!(setup.entry().inbox().pending_count(), 0);
    assert_eq!(setup.entry().session_id().as_deref(), Some("sess-7"));
    let mut client = setup.h.connect().await;
    wait_until_status(&mut client, &setup.window_id, WindowStatus::Running).await;
}

#[tokio::test]
async fn a_hook_of_a_profile_the_daemon_does_not_list_prints_nothing_and_exits_zero() {
    let setup = Setup::start().await;
    setup.deposit("PARCEL-FOR-THE-MAIN-SESSION");

    let (output, written) = setup
        .run(
            &["--hook", "no-such-profile", EVENT],
            br#"{"session_id":"s-1"}"#.to_vec(),
        )
        .await;

    assert!(written.is_ok(), "{written:?}");
    assert!(
        output.status.code() == Some(0) && output.stdout.is_empty(),
        "{}",
        describe(&output)
    );
    assert_eq!(setup.entry().inbox().pending_count(), 1);
    assert_eq!(setup.status().await, WindowStatus::Idle);
}

// --- a hook names the profile its window was opened from --------------------------------

/// Opens an `echo_child` window as the profile `profile` opens one, its hooks arranged as
/// `hooks` says, with a `[delivery]` of its own, under the detection definition `detection`.
pub(crate) async fn open_launched(
    h: &Harness,
    profile: &str,
    hooks: HooksRoute,
    detection: Option<&str>,
) -> String {
    h.registry
        .open_with_extra_env(
            yagura_daemon::OpenWindowRequest {
                command: yagura_test_support::process::echo_child_command(),
                cwd: None,
                name: None,
                vendor: None,
                detection: detection.map(str::to_string),
                on_name_taken: Default::default(),
            },
            Vec::new(),
            Vec::new(),
            WindowProvenance {
                launch: Some(LaunchIdentity {
                    profile: profile.to_string(),
                    model: None,
                    effort: None,
                }),
                delivery: Some(common::wake_none_delivery()),
                hooks,
                ..WindowProvenance::default()
            },
        )
        .await
        .expect("the launched window opens")
        .window_id
}

/// In a window opened from a profile whose hooks the daemon arranged, a hook that names
/// another profile can only come from a CLI the window's own CLI started, so it does
/// nothing: it prints nothing, exits 0, reports no status, records no session and takes
/// nothing. A hook naming the window's own profile there, and any hook in a window whose
/// hooks the daemon did not arrange, still does what its table says.
#[tokio::test]
async fn a_hook_naming_another_profile_than_its_windows_does_nothing_where_the_daemon_arranged_hooks()
 {
    let setup = Setup::start_without_screen_rules().await;
    let cases = [
        (
            "another profile, registered hooks",
            EVENT_FORM_PROFILE_ID,
            HooksRoute::RegisteredEntries,
            false,
        ),
        (
            "another profile, hooks left out",
            EVENT_FORM_PROFILE_ID,
            HooksRoute::LeftOut,
            false,
        ),
        (
            "the window's own profile",
            PROFILE_ID,
            HooksRoute::RegisteredEntries,
            true,
        ),
        (
            "another profile, hooks not arranged",
            EVENT_FORM_PROFILE_ID,
            HooksRoute::Unarranged,
            true,
        ),
    ];
    for (what, launched_from, hooks, handled) in cases {
        let window_id =
            open_launched(&setup.h, launched_from, hooks, Some(NO_RULES_DETECTION)).await;
        let entry = setup.h.registry.get(&window_id).expect("window exists");
        entry
            .inbox()
            .deposit(Parcel::new("a", "PARCEL-FOR-THE-WINDOW"));
        let reported_before = entry.status_event_count();

        let (output, written) = setup
            .run_in(
                &window_id,
                &hook_args(),
                br#"{"session_id":"sess-named"}"#.to_vec(),
            )
            .await;

        assert!(written.is_ok(), "{what}: {written:?}");
        assert_eq!(
            output.status.code(),
            Some(0),
            "{what}: {}",
            describe(&output)
        );
        if handled {
            assert_eq!(
                entry.status_event_count(),
                reported_before + 1,
                "{what}: the hook reports its status"
            );
            assert_eq!(entry.session_id().as_deref(), Some("sess-named"), "{what}");
            assert_eq!(
                entry.inbox().pending_count(),
                0,
                "{what}: the hook takes the parcel"
            );
        } else {
            assert!(output.stdout.is_empty(), "{what}: {}", describe(&output));
            assert_eq!(
                entry.status_event_count(),
                reported_before,
                "{what}: the hook reports no status"
            );
            assert_eq!(
                entry.session_id(),
                None,
                "{what}: the hook records no session"
            );
            assert_eq!(
                entry.inbox().pending_count(),
                1,
                "{what}: the parcel stays in the inbox"
            );
        }
    }
}

// --- an argument this copy does not know, inside a window ------------------------------

#[tokio::test]
async fn a_window_door_with_an_unknown_argument_reads_all_of_a_large_input_and_exits_zero() {
    let setup = Setup::start().await;
    setup.deposit("PARCEL-FOR-THE-MAIN-SESSION");

    for args in [
        &["--no-such-door"][..],
        &["--no-such-door", "/x"],
        &["--hook", PROFILE_ID, EVENT, "--no-such-flag"],
    ] {
        let (output, written) = setup.run(args, large_input("")).await;
        assert!(
            written.is_ok(),
            "{args:?}: the CLI's write of the hook input must succeed, got {written:?}"
        );
        assert!(
            output.status.code() == Some(0) && output.stdout.is_empty(),
            "{args:?}: an unknown argument inside a window must print nothing and exit 0; {}",
            describe(&output)
        );
    }
    assert_eq!(setup.entry().inbox().pending_count(), 1);
    assert_eq!(setup.status().await, WindowStatus::Idle);
}

// --- the table's validation --------------------------------------------------------------

fn profile_with_hook_events(table: &str) -> String {
    format!(
        "schema = 1\ndisplay_name = \"Hook Events Case\"\n\n[command]\ndefault = ['x']\n\n\
         [hook_events]\n{table}\n"
    )
}

#[test]
fn a_well_formed_hook_events_table_parses() {
    let source = profile_with_hook_events(
        "Start = { deliver = \"context\" }\n\
         Stop = { status = \"waiting_input\" }\n\
         Submit = { status = \"running\", deliver = \"take\", session = \"/session_id\", \
         skip_if = \"/subagentType\" }",
    );

    let parsed = parse_profile("hook-events-case", &source);

    assert!(
        parsed.is_ok(),
        "a well-formed [hook_events] table must be accepted, got {:?}",
        parsed.err()
    );
}

#[test]
fn a_malformed_hook_events_entry_is_refused_with_the_tables_fixed_reason() {
    let unknown_key =
        profile_with_hook_events(r#"Stop = { status = "waiting_input", bogus_key = "x" }"#);
    let no_action = profile_with_hook_events(r#"Stop = { skip_if = "/subagentType" }"#);
    let mut reasons = Vec::new();

    for (case, source) in [("an unknown key", unknown_key), ("no action", no_action)] {
        let error = parse_profile("hook-events-case", &source)
            .err()
            .unwrap_or_else(|| panic!("{case}: the profile must be refused"));
        let reason = error.to_string();
        assert!(
            reason.contains("hook_events") || reason.contains("hook events"),
            "{case}: the reason must name the [hook_events] table, got {reason:?}"
        );
        assert!(
            !reason.contains("bogus_key") && !reason.contains("Stop"),
            "{case}: the reason is fixed and does not echo the entry, got {reason:?}"
        );
        reasons.push(reason);
    }
    assert_eq!(
        reasons[0], reasons[1],
        "both refusals report the same fixed reason"
    );
}

// --- a door before revision 34, against this daemon -------------------------------------

/// A `run_hook` of a door before revision 34 confirms before it prints, so it takes
/// nothing; what the same request reports (the status and the session) is still handled,
/// so a window whose CLI keeps an older registration does not lose its status.
#[tokio::test]
async fn an_older_hook_request_takes_nothing_but_still_records_the_status_and_the_session() {
    use yagura_protocol::{ClientMessage, DaemonMessage, HookInputValue};

    let setup = Setup::start_without_screen_rules().await;
    setup.deposit("PARCEL-FOR-THE-MAIN-SESSION");
    let request = |input| ClientMessage::RunHook {
        window_id: setup.window_id.clone(),
        token: VALID_TOKEN.to_string(),
        profile: PROFILE_ID.to_string(),
        name: EVENT.to_string(),
        input,
        held: true,
        guest: None,
    };
    let mut door = setup.h.connect().await;
    common::send(&mut door, &request(None)).await;
    let wanted = match common::read_response(&mut door).await {
        DaemonMessage::HookInputWanted { pointers } => pointers,
        other => panic!("the event reads its input, so its values are asked for: {other:?}"),
    };
    let input = wanted
        .iter()
        .map(|wanted| match wanted.pointer.as_str() {
            "/session_id" => HookInputValue::Session("sess-older".to_string()),
            _ => HookInputValue::Absent,
        })
        .collect();
    common::send(&mut door, &request(Some(input))).await;
    match common::read_response(&mut door).await {
        DaemonMessage::Delivery {
            output, exit_code, ..
        } => assert!(
            output.is_empty() && exit_code == 0,
            "an older hook request is answered with nothing to print: {output:?} / {exit_code}"
        ),
        other => panic!("expected a delivery, got {other:?}"),
    }
    assert_eq!(
        setup.entry().inbox().pending_count(),
        1,
        "an older hook request must leave the parcel in the inbox"
    );
    assert_eq!(setup.entry().session_id().as_deref(), Some("sess-older"));
    let mut client = setup.h.connect().await;
    wait_until_status(&mut client, &setup.window_id, WindowStatus::Running).await;
}

// --- the door's own end, against a stand-in daemon ---------------------------------------

/// Reads one frame of a door's as JSON; `None` once the door has closed.
async fn read_json_frame<S: tokio::io::AsyncRead + Unpin>(
    stream: &mut S,
) -> Option<serde_json::Value> {
    use tokio::io::AsyncReadExt;
    let guard = yagura_test_support::HANG_GUARD;
    let mut len = [0u8; 4];
    match tokio::time::timeout(guard, stream.read_exact(&mut len)).await {
        Ok(Ok(_)) => {}
        Ok(Err(_)) => return None,
        Err(_) => panic!("the door neither wrote nor closed within {guard:?}"),
    }
    let mut body = vec![0u8; u32::from_le_bytes(len) as usize];
    tokio::time::timeout(guard, stream.read_exact(&mut body))
        .await
        .expect("the door finishes a frame it began")
        .expect("the door finishes a frame it began");
    Some(serde_json::from_slice(&body).expect("a door writes JSON frames"))
}

/// A stand-in for a daemon at `instance`'s endpoint: it answers the first frame of the one
/// door that connects with `answer`, then records what else the door sends until it closes.
/// Binding claims the endpoint, so the door can reach nothing else.
async fn stand_in_daemon(
    instance: &str,
    answer: serde_json::Value,
) -> tokio::task::JoinHandle<Vec<serde_json::Value>> {
    use tokio::io::AsyncWriteExt;

    let accept = claim_endpoint(instance).await;
    tokio::spawn(async move {
        let mut conn = accept.await;
        let mut frames = Vec::new();
        let Some(first) = read_json_frame(&mut conn).await else {
            return frames;
        };
        frames.push(first);
        let body = serde_json::to_vec(&answer).unwrap();
        conn.write_all(&yagura_protocol::frame_encode(&body))
            .await
            .expect("answer the door");
        conn.flush().await.expect("answer the door");
        while let Some(frame) = read_json_frame(&mut conn).await {
            frames.push(frame);
        }
        frames
    })
}

/// Claims `instance`'s endpoint; the returned future accepts the one door that connects.
async fn claim_endpoint(
    instance: &str,
) -> impl std::future::Future<Output = <yagura_os::PlatformIpc as yagura_os::PeerAuth>::Connection>
+ Send
+ 'static {
    use yagura_os::{IpcTransport, PeerAuth, PlatformIpc};

    let ipc = PlatformIpc::default();
    let user = ipc.current_user_id().expect("the current user id resolves");
    let namespace = yagura_os::resolve_instance_from(Some(std::ffi::OsStr::new(instance)))
        .expect("the test's instance id is valid");
    let endpoint = yagura_os::default_endpoint_owning(&user, &namespace);
    let security = ipc.endpoint_security().expect("endpoint security resolves");
    let mut listener = ipc
        .bind(&endpoint, security)
        .await
        .unwrap_or_else(|err| panic!("the stand-in must claim {endpoint:?}: {err}"));
    async move {
        tokio::time::timeout(yagura_test_support::HANG_GUARD, ipc.accept(&mut listener))
            .await
            .expect("the door connects")
            .expect("the door connects")
    }
}

/// The doors a stand-in daemon is asked by, with the door each one's read must name.
fn stand_in_doors() -> [(Vec<&'static str>, serde_json::Value); 2] {
    [
        (
            vec!["--deliver-take"],
            serde_json::json!({ "kind": "take" }),
        ),
        (
            hook_args().to_vec(),
            serde_json::json!({ "kind": "hook", "profile": PROFILE_ID, "name": EVENT }),
        ),
    ]
}

/// Runs the door `args` against a stand-in daemon answering with `answer`; returns the
/// door's output and every frame it sent.
async fn run_against_stand_in(
    args: &[&str],
    answer: serde_json::Value,
) -> (Output, Vec<serde_json::Value>) {
    let instance = unique_instance();
    let bases = TempDir::new("yagura-door", "stand-in");
    let frames = stand_in_daemon(&instance, answer).await;
    let command = door_command(args, &instance, &bases, Some(("w-stand-in", VALID_TOKEN)));
    let (output, _) = tokio::task::spawn_blocking(move || run_with_input(command, b"{}".to_vec()))
        .await
        .expect("join");
    let frames = frames.await.expect("the stand-in daemon");
    (output, frames)
}

fn frame_types(frames: &[serde_json::Value]) -> Vec<&str> {
    frames
        .iter()
        .map(|frame| frame["type"].as_str().unwrap_or("?"))
        .collect()
}

/// The time a door has is the one its daemon names in the answer: with less of it left
/// than the door keeps for printing and ending, the door prints nothing, ends with 0, and
/// never says it printed, so the daemon puts what it took back.
#[tokio::test]
async fn a_door_left_less_time_than_its_margin_prints_nothing_and_never_says_it_printed() {
    for (args, door) in stand_in_doors() {
        let answer = serde_json::json!({
            "type": "delivery",
            "output": "PARCEL-WITHOUT-TIME",
            "stream": "stdout",
            "exit_code": 0,
            "hook_timeout_sec": 0,
        });
        let (output, frames) = run_against_stand_in(&args, answer).await;
        assert_eq!(
            frames.first().map(|read| (&read["type"], &read["door"])),
            Some((&serde_json::json!("door_read"), &door)),
            "{args:?}: the door reads with one request naming its door; sent {frames:?}"
        );
        assert!(
            output.status.code() == Some(0) && output.stdout.is_empty(),
            "{args:?}: {}",
            describe(&output)
        );
        assert_eq!(
            frame_types(&frames),
            ["door_read"],
            "{args:?}: a door that printed nothing must not say it printed"
        );
    }
}

/// A daemon that names no deadline is taken to give the time the door had before revision
/// 34. The door prints, then says so, and ends at once without an answer to that.
#[tokio::test]
async fn a_door_whose_daemon_names_no_deadline_prints_then_says_so_and_ends_without_an_answer() {
    for (args, _) in stand_in_doors() {
        let answer = serde_json::json!({
            "type": "delivery",
            "output": "PARCEL-WITH-THE-OLD-DEADLINE",
            "stream": "stdout",
            "exit_code": 0,
        });
        let (output, frames) = run_against_stand_in(&args, answer).await;
        assert_eq!(
            output.status.code(),
            Some(0),
            "{args:?}: {}",
            describe(&output)
        );
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("PARCEL-WITH-THE-OLD-DEADLINE"),
            "{args:?}: {}",
            describe(&output)
        );
        assert_eq!(
            frame_types(&frames),
            ["door_read", "deliver_printed"],
            "{args:?}: the door says once that it printed"
        );
    }
}

/// A daemon before revision 34 refuses the door's read by name and takes nothing: the
/// door prints nothing and ends with 0, and the text waits for a newer daemon.
#[tokio::test]
async fn a_door_whose_daemon_refuses_its_read_by_name_prints_nothing_and_exits_zero() {
    for (args, _) in stand_in_doors() {
        let answer = serde_json::json!({
            "type": "error",
            "code": "unknown_type",
            "message": "unknown message type",
        });
        let (output, frames) = run_against_stand_in(&args, answer).await;
        assert!(
            output.status.code() == Some(0) && output.stdout.is_empty(),
            "{args:?}: {}",
            describe(&output)
        );
        assert_eq!(frame_types(&frames), ["door_read"], "{args:?}");
    }
}

// --- the name of the event a form prints -------------------------------------------------

/// `{event}` in a form is the name of the event the hook ran for, whichever of the
/// profile's events that is.
#[tokio::test]
async fn a_form_names_the_event_its_hook_ran_for() {
    let setup = Setup::start().await;
    for event in ["PostToolUse", "UserPromptSubmit"] {
        setup.deposit(&format!("PARCEL-FOR-{event}"));
        let (output, written) = setup
            .run(
                &["--hook", EVENT_FORM_PROFILE_ID, event],
                br#"{"session_id":"s-1"}"#.to_vec(),
            )
            .await;
        assert!(written.is_ok(), "{written:?}");
        assert_eq!(
            output.status.code(),
            Some(0),
            "{event}: {}",
            describe(&output)
        );
        let printed: serde_json::Value = serde_json::from_str(
            String::from_utf8_lossy(&output.stdout).trim(),
        )
        .unwrap_or_else(|error| {
            panic!(
                "{event}: the form must print JSON ({error}); {}",
                describe(&output)
            )
        });
        assert_eq!(
            printed["hookSpecificOutput"]["hookEventName"].as_str(),
            Some(event),
            "{event}: {}",
            describe(&output)
        );
        assert!(
            printed["hookSpecificOutput"]["additionalContext"]
                .as_str()
                .is_some_and(|text| text.contains(&format!("PARCEL-FOR-{event}"))),
            "{event}: {}",
            describe(&output)
        );
    }
}

// --- a stop's door: everything in one block, only when the stop is not a continuation ---

/// A CLI whose stop hook can be made to continue the turn, as its own profile declares
/// it: the event, an input of a stop that is not a continuation, inputs of stops that
/// must not be blocked, and the `decision` its block form prints.
struct StopCase {
    profile: &'static str,
    event: &'static str,
    first_stop: &'static str,
    not_to_block: &'static [(&'static str, &'static str)],
    decision: &'static str,
}

/// The CLIs given a stop's block, with the marks their own stop input carries.
fn stop_cases() -> [StopCase; 4] {
    [
        StopCase {
            profile: "grok-build",
            event: "Stop",
            first_stop: GROK_FIRST_STOP,
            not_to_block: &[
                (
                    "a continuation",
                    r#"{"stop_hook_active":true,"stopHookActive":true}"#,
                ),
                ("no continuation mark", r#"{"session_id":"s-1"}"#),
            ],
            decision: "block",
        },
        StopCase {
            profile: "copilot-cli",
            event: "agentStop",
            first_stop: r#"{"stop_hook_active":false}"#,
            not_to_block: &[
                ("a continuation", r#"{"stop_hook_active":true}"#),
                ("no continuation mark", r#"{"sessionId":"s-1"}"#),
            ],
            decision: "block",
        },
        StopCase {
            profile: "muse-code",
            event: "Stop",
            first_stop: r#"{"stop_hook_active":false}"#,
            not_to_block: &[
                ("a continuation", r#"{"stop_hook_active":true}"#),
                ("no continuation mark", r#"{"session_id":"s-1"}"#),
            ],
            decision: "block",
        },
        StopCase {
            profile: "antigravity-cli",
            event: "Stop",
            first_stop: AGY_FIRST_STOP,
            not_to_block: &[
                (
                    "a continuation",
                    r#"{"terminationReason":"NO_TOOL_CALL","executionNum":1}"#,
                ),
                (
                    "no continuation mark",
                    r#"{"terminationReason":"NO_TOOL_CALL"}"#,
                ),
                (
                    "a stop for another reason",
                    r#"{"terminationReason":"OTHER_REASON","executionNum":0}"#,
                ),
            ],
            decision: "continue",
        },
    ]
}

/// A stop that is not a continuation, as Grok Build's stop input says it.
const GROK_FIRST_STOP: &str = r#"{"stop_hook_active":false,"stopHookActive":false}"#;
/// The first stop of a turn that ended without a tool call, as the Antigravity CLI's stop
/// input says it.
pub(crate) const AGY_FIRST_STOP: &str = r#"{"terminationReason":"NO_TOOL_CALL","executionNum":0}"#;

/// A stop's door hands everything waiting over in one block, and only at a stop that is
/// not a continuation and meets the profile's condition; at any other stop, and with
/// nothing waiting, it prints nothing, keeps what waits, and never waits itself.
#[tokio::test]
async fn a_stop_hands_everything_waiting_in_one_block_only_when_it_is_not_a_continuation() {
    let setup = Setup::start_with_shipped_profiles().await;
    for case in stop_cases() {
        let args = ["--hook", case.profile, case.event];
        let who = format!("{} {}", case.profile, case.event);

        let (output, written) = setup.run(&args, case.first_stop.as_bytes().to_vec()).await;
        assert!(written.is_ok(), "{who}: {written:?}");
        assert!(
            output.status.code() == Some(0) && output.stdout.is_empty(),
            "{who}: a stop with nothing waiting prints nothing and exits 0; {}",
            describe(&output)
        );
        assert!(
            !setup.entry().inbox().has_waiter(),
            "{who}: a stop's door does not wait"
        );

        let (first, second) = (format!("FIRST-FOR-{who}"), format!("SECOND-FOR-{who}"));
        setup.deposit(&first);
        setup.deposit(&second);
        for (what, input) in case.not_to_block {
            let (output, written) = setup.run(&args, input.as_bytes().to_vec()).await;
            assert!(written.is_ok(), "{who}, {what}: {written:?}");
            assert!(
                output.status.code() == Some(0) && output.stdout.is_empty(),
                "{who}, {what}: the stop is not blocked; {}",
                describe(&output)
            );
            assert_eq!(
                setup.entry().inbox().pending_count(),
                2,
                "{who}, {what}: what waits stays in the inbox"
            );
        }

        let (output, written) = setup.run(&args, case.first_stop.as_bytes().to_vec()).await;
        assert!(written.is_ok(), "{who}: {written:?}");
        assert_eq!(
            output.status.code(),
            Some(0),
            "{who}: {}",
            describe(&output)
        );
        let printed: serde_json::Value = serde_json::from_str(
            String::from_utf8_lossy(&output.stdout).trim(),
        )
        .unwrap_or_else(|error| {
            panic!(
                "{who}: a block prints JSON ({error}); {}",
                describe(&output)
            )
        });
        assert_eq!(
            printed["decision"].as_str(),
            Some(case.decision),
            "{who}: {}",
            describe(&output)
        );
        let reason = printed["reason"].as_str().unwrap_or_default();
        assert!(
            reason.contains(&first) && reason.contains(&second),
            "{who}: one block carries everything that waits; {}",
            describe(&output)
        );
        assert_eq!(setup.entry().inbox().pending_count(), 0, "{who}");
    }
}

// --- a stop's status is decided where its door ends --------------------------------------

/// What a relay between a door and its daemon does with the door's notice that it printed.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Printed {
    /// Passes it on, as a door's own connection does.
    Forward,
    /// Drops it: the daemon sees the read end without it once the relay lets go.
    Withhold,
}

/// A door run through a relay to its daemon.
pub(crate) struct Relayed {
    /// What the door printed and how it ended.
    pub(crate) output: Output,
    /// Every frame the door sent, in order.
    pub(crate) sent: Vec<serde_json::Value>,
    /// The relay's connection to the daemon, which stays open until it is dropped.
    pub(crate) upstream: yagura_test_support::ipc::Connection,
}

/// Runs the door `args` in `window_id` with `input` on its stdin, relaying every frame
/// between it and `h`'s daemon over one connection of the relay's own, and dealing with
/// the door's notice that it printed as `printed` says.
pub(crate) async fn relay_door(
    h: &Harness,
    window_id: &str,
    args: &[&str],
    input: Vec<u8>,
    printed: Printed,
) -> Relayed {
    let instance = unique_instance();
    let bases = TempDir::new("yagura-door", "relay");
    let accept = claim_endpoint(&instance).await;
    let mut upstream = h.connect().await;
    let command = door_command(args, &instance, &bases, Some((window_id, VALID_TOKEN)));
    let door = tokio::task::spawn_blocking(move || run_with_input(command, input));
    let mut conn = accept.await;
    let mut sent = Vec::new();
    while let Some(frame) = read_json_frame(&mut conn).await {
        let notice = frame["type"] == "deliver_printed";
        sent.push(frame.clone());
        if notice {
            if printed == Printed::Forward {
                write_json_frame(&mut upstream, &frame).await;
            }
            continue;
        }
        write_json_frame(&mut upstream, &frame).await;
        let answer = loop {
            let answer = read_json_frame(&mut upstream)
                .await
                .expect("the daemon answers the door's read");
            if !answer["type"]
                .as_str()
                .is_some_and(|kind| kind.ends_with("_changed"))
            {
                break answer;
            }
        };
        write_json_frame(&mut conn, &answer).await;
    }
    let (output, _) = door.await.expect("join");
    Relayed {
        output,
        sent,
        upstream,
    }
}

async fn write_json_frame<S: tokio::io::AsyncWrite + Unpin>(
    stream: &mut S,
    frame: &serde_json::Value,
) {
    use tokio::io::AsyncWriteExt;
    let body = serde_json::to_vec(frame).expect("a frame encodes");
    stream
        .write_all(&yagura_protocol::frame_encode(&body))
        .await
        .expect("relay a frame");
    stream.flush().await.expect("relay a frame");
}

/// A stop's door decides the window's status where it ends: a block the door printed
/// leaves the turn running; a read that ends without that notice leaves the window
/// waiting for input.
#[tokio::test]
async fn a_stops_status_is_decided_where_its_door_ends() {
    let setup = Setup::start_with_shipped_profiles().await;
    let args = ["--hook", "grok-build", "Stop"];
    let mut client = setup.h.connect().await;
    let pending = || setup.entry().inbox().pending_count();
    let first_stop = || GROK_FIRST_STOP.as_bytes().to_vec();

    // Printed: the block continues the turn.
    setup.deposit("PARCEL-PRINTED");
    let relayed = relay_door(
        &setup.h,
        &setup.window_id,
        &args,
        first_stop(),
        Printed::Forward,
    )
    .await;
    assert!(
        String::from_utf8_lossy(&relayed.output.stdout).contains("PARCEL-PRINTED"),
        "{}",
        describe(&relayed.output)
    );
    wait_until_status(&mut client, &setup.window_id, WindowStatus::Running).await;
    drop(relayed);

    // No notice and nothing newer: the CLI stops, and what the read took goes back.
    setup.deposit("PARCEL-NOT-PRINTED");
    let relayed = relay_door(
        &setup.h,
        &setup.window_id,
        &args,
        first_stop(),
        Printed::Withhold,
    )
    .await;
    assert!(
        String::from_utf8_lossy(&relayed.output.stdout).contains("PARCEL-NOT-PRINTED"),
        "{}",
        describe(&relayed.output)
    );
    assert_eq!(pending(), 0, "the door's read holds what it took");
    drop(relayed.upstream);
    wait_until_status(&mut client, &setup.window_id, WindowStatus::WaitingInput).await;
    until_pending(&setup, 1).await;
}

/// A stop's block publishes no status before its door ends: the first status published
/// after it began is the `running` of its door's notice. A read that ends without that
/// notice does not overwrite a status that came in after the door began. A definition
/// confirming at once publishes every status the window is given, in order, so a later
/// report is the mark that everything before it was taken; the read's parcel going back is
/// the mark that the daemon has seen the read end.
#[tokio::test]
async fn a_stops_door_publishes_no_status_before_its_end_nor_over_a_newer_one() {
    let setup = Setup::start_with(Screen::NoRulesAtOnce, true).await;
    let mut client = setup.h.connect().await;
    let args = ["--hook", "grok-build", "Stop"];

    common::report_status(&mut client, &setup.window_id, WindowStatus::WaitingApproval).await;
    wait_until_status(&mut client, &setup.window_id, WindowStatus::WaitingApproval).await;
    let mut published = setup.entry().subscribe_status();
    setup.deposit("PARCEL-PRINTED");
    let relayed = relay_door(
        &setup.h,
        &setup.window_id,
        &args,
        GROK_FIRST_STOP.as_bytes().to_vec(),
        Printed::Forward,
    )
    .await;
    assert!(
        String::from_utf8_lossy(&relayed.output.stdout).contains("PARCEL-PRINTED"),
        "{}",
        describe(&relayed.output)
    );
    let before_running = published_until(&mut published, WindowStatus::Running).await;
    assert!(
        before_running.is_empty(),
        "a block publishes no status before its door says it printed; published {before_running:?} first"
    );
    drop(relayed);

    setup.deposit("PARCEL-NOT-PRINTED");
    let relayed = relay_door(
        &setup.h,
        &setup.window_id,
        &args,
        GROK_FIRST_STOP.as_bytes().to_vec(),
        Printed::Withhold,
    )
    .await;
    assert!(
        String::from_utf8_lossy(&relayed.output.stdout).contains("PARCEL-NOT-PRINTED"),
        "{}",
        describe(&relayed.output)
    );
    common::report_status(&mut client, &setup.window_id, WindowStatus::WaitingApproval).await;
    wait_until_status(&mut client, &setup.window_id, WindowStatus::WaitingApproval).await;
    let mut published = setup.entry().subscribe_status();
    drop(relayed.upstream);
    until_pending(&setup, 1).await;
    common::report_status(&mut client, &setup.window_id, WindowStatus::Running).await;
    let before_the_mark = published_until(&mut published, WindowStatus::Running).await;
    assert!(
        before_the_mark.is_empty(),
        "the end of a door that began before the newer status must not overwrite it; published {before_the_mark:?} before the later report"
    );
}

/// The statuses `published` carries before the first `mark`.
async fn published_until(
    published: &mut tokio::sync::broadcast::Receiver<yagura_daemon::waiter::StatusAt>,
    mark: WindowStatus,
) -> Vec<WindowStatus> {
    let mut before = Vec::new();
    loop {
        let at = tokio::time::timeout(yagura_test_support::HANG_GUARD, published.recv())
            .await
            .unwrap_or_else(|_| panic!("{mark:?} is published"))
            .expect("the window's statuses are published");
        if at.status.status() == mark {
            return before;
        }
        before.push(at.status.status());
    }
}

/// Polls the window's inbox until `count` parcels wait in it; the deadline only detects a
/// hang.
async fn until_pending(setup: &Setup, count: usize) {
    let deadline = tokio::time::Instant::now() + yagura_test_support::HANG_GUARD;
    while setup.entry().inbox().pending_count() != count {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {count} parcels to wait in the inbox"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

// --- a window's credentials speak for it only from its own CLI (lineage) ---------------

/// The ignored test a copy of this binary runs to play a window's CLI or a CLI that CLI
/// started.
const LINEAGE_ROLE: &str = "lineage_role";
/// Which part the copy plays: `cli` (the window's command) or `nested`.
const LINEAGE_ROLE_VAR: &str = "B8B_LINEAGE_ROLE";
/// The folder a window's CLI reads its steps from and writes their results to.
const LINEAGE_DIR_VAR: &str = "B8B_LINEAGE_DIR";
/// The step a nested CLI runs, as JSON.
const LINEAGE_TASK_VAR: &str = "B8B_LINEAGE_TASK";
/// What a nested CLI writes ahead of its result, on the line libtest writes to.
const LINEAGE_RESULT_MARK: &str = "B8B-LINEAGE-RESULT:";
/// The `[hook_events]` event the lineage profiles name, and what it does.
pub(crate) const LINEAGE_EVENT: &str = "PostToolUse";

/// The value the shipped Antigravity CLI profile declares as its CLI's process image on
/// this operating system. macOS has none shipped, so its place is read from the Linux one.
fn shipped_image() -> &'static str {
    if cfg!(windows) { "agy.exe" } else { "agy" }
}

/// The path of keys at which a profile declares its CLI's process image for this operating
/// system, read from the shipped Antigravity CLI profile: the one place its value for this
/// operating system sits, apart from the argv `[command]` runs. Whether the value there is
/// one string or a list of them comes with it.
pub(crate) fn image_declaration_path() -> (Vec<String>, bool) {
    fn collect(
        value: &toml::Value,
        image: &str,
        path: &mut Vec<String>,
        found: &mut Vec<(Vec<String>, bool)>,
    ) {
        match value {
            toml::Value::String(text) if text == image => found.push((path.clone(), false)),
            toml::Value::Array(items)
                if items
                    .iter()
                    .any(|item| item.as_str().is_some_and(|text| text == image)) =>
            {
                found.push((path.clone(), true));
            }
            toml::Value::Table(table) => {
                for (key, value) in table {
                    path.push(key.clone());
                    collect(value, image, path, found);
                    path.pop();
                }
            }
            _ => {}
        }
    }
    let document: toml::Value =
        common::docs::read_doc_verbatim("../../profiles/antigravity-cli.toml")
            .parse()
            .expect("the shipped Antigravity CLI profile is TOML");
    let mut found = Vec::new();
    collect(&document, shipped_image(), &mut Vec::new(), &mut found);
    found.retain(|(path, _)| {
        !(path.len() == 2
            && path[0] == "command"
            && ["default", "windows", "linux", "macos"].contains(&path[1].as_str()))
    });
    assert_eq!(
        found.len(),
        1,
        "the shipped Antigravity CLI profile declares {:?} as its CLI's process image in one \
         place outside its [command] argv; found it at {found:?}",
        shipped_image()
    );
    let (mut path, list) = found.remove(0);
    if cfg!(target_os = "macos") {
        for key in &mut path {
            if key == "linux" {
                *key = "macos".to_string();
            }
        }
    }
    (path, list)
}

/// `source` declaring `image` as its CLI's process image for this operating system, at the
/// place [`image_declaration_path`] reads it from.
pub(crate) fn with_declared_image(source: &str, image: &str) -> String {
    let mut document: toml::Table = source.parse().expect("the profile source is TOML");
    let (path, list) = image_declaration_path();
    let (key, tables) = path.split_last().expect("a path has a key");
    let mut table = &mut document;
    for name in tables {
        table = table
            .entry(name.clone())
            .or_insert_with(|| toml::Value::Table(toml::Table::new()))
            .as_table_mut()
            .expect("the image is declared inside tables");
    }
    let value = toml::Value::String(image.to_string());
    table.insert(
        key.clone(),
        if list {
            toml::Value::Array(vec![value])
        } else {
            value
        },
    );
    toml::to_string(&document).expect("the profile encodes as TOML")
}

/// Plays a window's CLI, or a CLI one started, for the lineage cases; does nothing when
/// run any other way.
#[test]
#[ignore = "run by the lineage cases from a copy of this binary"]
fn lineage_role() {
    let Some(role) = std::env::var_os(LINEAGE_ROLE_VAR) else {
        return;
    };
    let dir = PathBuf::from(std::env::var_os(LINEAGE_DIR_VAR).expect("the lineage folder"));
    let config: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(dir.join("config.json")).expect("the lineage config"),
    )
    .expect("the lineage config is JSON");
    match role.to_str() {
        Some("cli") => lineage_cli(&dir, &config),
        Some("nested") => {
            let task: serde_json::Value = serde_json::from_str(
                &std::env::var(LINEAGE_TASK_VAR).expect("the nested CLI's task"),
            )
            .expect("the task is JSON");
            let mut held = Vec::new();
            let result = lineage_task(&task, &config, &mut held);
            let mut stdout = std::io::stdout().lock();
            writeln!(stdout, "{LINEAGE_RESULT_MARK}{result}").expect("write the result");
            stdout.flush().expect("write the result");
            drop(stdout);
            if !held.is_empty() {
                // Held until the CLI that started it lets go of its stdin.
                let _ = std::io::copy(&mut std::io::stdin(), &mut std::io::sink());
                end_held(&mut held);
            }
        }
        other => panic!("unknown lineage role {other:?}"),
    }
}

/// A process a lineage step started and keeps running, with the stdin whose end tells it
/// to finish.
struct Held {
    child: std::process::Child,
    stdin: Option<std::process::ChildStdin>,
}

fn end_held(held: &mut Vec<Held>) {
    for mut process in held.drain(..) {
        drop(process.stdin.take());
        let _ = process.child.wait();
    }
}

/// The window's CLI: runs step `n` once the test has written `go-<n>`, and writes its
/// result to `done-<n>.json`. Ends when told to, or when no step comes within
/// [`yagura_test_support::HANG_GUARD`].
fn lineage_cli(dir: &std::path::Path, config: &serde_json::Value) {
    let mut held = Vec::new();
    for step in 1u32.. {
        let go = dir.join(format!("go-{step}"));
        let deadline = std::time::Instant::now() + yagura_test_support::HANG_GUARD;
        let task: serde_json::Value = loop {
            if let Ok(text) = std::fs::read_to_string(&go) {
                break serde_json::from_str(&text).expect("a step is JSON");
            }
            if std::time::Instant::now() > deadline {
                end_held(&mut held);
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        };
        let exit = task["exit"].as_bool() == Some(true);
        let mut result = if exit {
            serde_json::json!({})
        } else if task["end"].as_bool() == Some(true) {
            end_held(&mut held);
            serde_json::json!({})
        } else if task["by"] == "nested" || task["by"] == "rooted" {
            run_nested(&task, config, &mut held)
        } else {
            lineage_task(&task, config, &mut held)
        };
        result["pid"] = std::process::id().into();
        write_whole(&dir.join(format!("done-{step}.json")), &result.to_string());
        if exit {
            end_held(&mut held);
            return;
        }
    }
}

/// Writes `text` to `path` so a reader sees all of it or nothing.
fn write_whole(path: &std::path::Path, text: &str) {
    let partial = path.with_extension("partial");
    std::fs::write(&partial, text).expect("write a lineage file");
    std::fs::rename(&partial, path).expect("put a lineage file in place");
}

/// Has a CLI of the same image, started by this one, run `task`; returns its result. A
/// `rooted` one is started the way the window's own CLI was, through the launch statement
/// of the guest profile in the config, so it is the root of its own environment.
fn run_nested(
    task: &serde_json::Value,
    config: &serde_json::Value,
    held: &mut Vec<Held>,
) -> serde_json::Value {
    let mut command = if task["by"] == "rooted" {
        let argv: Vec<&str> = config["rooted"]
            .as_array()
            .expect("a guest profile's launch argv")
            .iter()
            .map(|arg| arg.as_str().expect("an argument"))
            .collect();
        let mut command = Command::new(argv[0]);
        command.args(&argv[1..]);
        command
    } else {
        let mut command = Command::new(std::env::current_exe().expect("current_exe"));
        command.args([
            yagura_test_support::process::libtest_name(module_path!(), LINEAGE_ROLE).as_str(),
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ]);
        command
    };
    command
        .env(LINEAGE_ROLE_VAR, "nested")
        .env(LINEAGE_TASK_VAR, task.to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = command.spawn().expect("start the nested CLI");
    let stdin = child.stdin.take();
    let mut reader = std::io::BufReader::new(child.stdout.take().expect("stdout"));
    let result = loop {
        let mut line = String::new();
        let read =
            std::io::BufRead::read_line(&mut reader, &mut line).expect("read the nested CLI");
        assert!(read > 0, "the nested CLI ended without a result");
        if let Some((_, result)) = line.split_once(LINEAGE_RESULT_MARK) {
            break serde_json::from_str::<serde_json::Value>(result.trim())
                .expect("the nested result is JSON");
        }
    };
    std::thread::spawn(move || std::io::copy(&mut reader, &mut std::io::sink()));
    let process = Held { child, stdin };
    if task.get("bridge").is_some() || task.get("server").is_some() {
        held.push(process);
    } else {
        end_held(&mut vec![process]);
    }
    result
}

/// Points what a lineage step starts at the test's daemon and keeps it out of the user's
/// own folders.
fn lineage_env(command: &mut Command, config: &serde_json::Value) {
    let bases = PathBuf::from(config["bases"].as_str().expect("bases"));
    command.env(
        "YAGURA_INSTANCE",
        config["instance"].as_str().expect("instance"),
    );
    if cfg!(windows) {
        command
            .env("LOCALAPPDATA", bases.join("local"))
            .env("APPDATA", bases.join("roaming"));
    } else {
        command
            .env("XDG_DATA_HOME", bases.join("data"))
            .env("XDG_STATE_HOME", bases.join("state"))
            .env("XDG_CONFIG_HOME", bases.join("config"))
            .env("HOME", bases.join("home"));
    }
}

/// Runs a door (`door`: its argv, `input`: its stdin) or starts a bridge (`bridge`: the
/// profile it declares) from this process, which inherits the window's environment.
fn lineage_task(
    task: &serde_json::Value,
    config: &serde_json::Value,
    held: &mut Vec<Held>,
) -> serde_json::Value {
    if let Some(args) = task["door"].as_array() {
        let mut command = Command::new(config["door"].as_str().expect("door"));
        command
            .args(
                args.iter()
                    .map(|arg| arg.as_str().expect("a door argument")),
            )
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        lineage_env(&mut command, config);
        let input = task["input"]
            .as_str()
            .unwrap_or_default()
            .as_bytes()
            .to_vec();
        let (output, _) = run_with_input(command, input);
        return serde_json::json!({
            "code": output.status.code(),
            "stdout": String::from_utf8_lossy(&output.stdout),
        });
    }
    if let Some(argv) = task["run"].as_array() {
        let argv: Vec<&str> = argv
            .iter()
            .map(|arg| arg.as_str().expect("an argument"))
            .collect();
        let mut command = Command::new(argv[0]);
        command
            .args(&argv[1..])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        lineage_env(&mut command, config);
        for name in task["unset"].as_array().into_iter().flatten() {
            command.env_remove(name.as_str().expect("a variable name"));
        }
        let input = task["input"]
            .as_str()
            .unwrap_or_default()
            .as_bytes()
            .to_vec();
        let (output, _) = run_with_input(command, input);
        return serde_json::json!({
            "code": output.status.code(),
            "stdout": String::from_utf8_lossy(&output.stdout),
        });
    }
    let mut command;
    if let Some(argv) = task["server"].as_array() {
        let argv: Vec<&str> = argv
            .iter()
            .map(|arg| arg.as_str().expect("an argument"))
            .collect();
        command = Command::new(argv[0]);
        command.args(&argv[1..]);
    } else {
        let profile = task["bridge"]
            .as_str()
            .expect("a lineage step runs a door, a command or a bridge");
        command = Command::new(config["relay"].as_str().expect("relay"));
        command.args(["--profile", profile]);
    }
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    lineage_env(&mut command, config);
    let mut child = command.spawn().expect("start the bridge");
    let mut stdin = child.stdin.take().expect("stdin");
    writeln!(stdin, "{}", common::relay::initialize_line(1)).expect("write initialize");
    stdin.flush().expect("write initialize");
    let mut reader = std::io::BufReader::new(child.stdout.take().expect("stdout"));
    let mut answer = String::new();
    std::io::BufRead::read_line(&mut reader, &mut answer).expect("read the bridge");
    std::thread::spawn(move || std::io::copy(&mut reader, &mut std::io::sink()));
    held.push(Held {
        child,
        stdin: Some(stdin),
    });
    serde_json::json!({ "answer": answer.trim() })
}

/// Which process runs a lineage step.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum By {
    /// The window's CLI, its PTY child.
    Cli,
    /// A CLI of the same image the window's CLI started.
    Nested,
    /// A CLI of the same image the window's CLI started through the guest profile's launch
    /// statement, as the window's own CLI was: the root of an environment of its own.
    Rooted,
}

impl By {
    fn name(self) -> &'static str {
        match self {
            Self::Cli => "cli",
            Self::Nested => "nested",
            Self::Rooted => "rooted",
        }
    }
}

/// What a door a lineage step ran printed, and how it ended.
#[derive(Debug)]
pub(crate) struct DoorRun {
    pub(crate) code: Option<i64>,
    pub(crate) stdout: String,
}

/// The window's CLI of a lineage profile, driven one step at a time.
pub(crate) struct LineageCli {
    dir: PathBuf,
    step: u32,
}

impl LineageCli {
    async fn run(&mut self, task: serde_json::Value) -> serde_json::Value {
        self.step += 1;
        write_whole(
            &self.dir.join(format!("go-{}", self.step)),
            &task.to_string(),
        );
        let done = self.dir.join(format!("done-{}.json", self.step));
        let deadline = tokio::time::Instant::now() + yagura_test_support::HANG_GUARD;
        loop {
            if let Ok(text) = std::fs::read_to_string(&done) {
                return serde_json::from_str(&text).expect("a result is JSON");
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "the window's CLI never finished {task} ({})",
                self.dir.display()
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }

    /// Runs the door `args` with `input` on its stdin, from `by`.
    pub(crate) async fn door(&mut self, by: By, args: &[&str], input: &str) -> DoorRun {
        let result = self
            .run(serde_json::json!({ "by": by.name(), "door": args, "input": input }))
            .await;
        DoorRun {
            code: result["code"].as_i64(),
            stdout: result["stdout"].as_str().unwrap_or_default().to_string(),
        }
    }

    /// Runs the command `argv` (a program, then its arguments) with `input` on its stdin from
    /// `by`, without the variables named in `unset`.
    pub(crate) async fn run_command(
        &mut self,
        by: By,
        argv: &[String],
        input: &str,
        unset: &[String],
    ) -> DoorRun {
        let result = self
            .run(serde_json::json!({
                "by": by.name(), "run": argv, "input": input, "unset": unset
            }))
            .await;
        DoorRun {
            code: result["code"].as_i64(),
            stdout: result["stdout"].as_str().unwrap_or_default().to_string(),
        }
    }

    /// Starts the bridge the command `argv` is, from `by`, and returns its answer to
    /// `initialize` once it has one; the bridge runs on until [`Self::end_bridges`].
    pub(crate) async fn bridge_command(&mut self, by: By, argv: &[String]) -> String {
        let result = self
            .run(serde_json::json!({ "by": by.name(), "server": argv }))
            .await;
        result["answer"].as_str().unwrap_or_default().to_string()
    }

    /// Starts a bridge declaring `profile` from `by`, and returns its answer to
    /// `initialize` once it has one; the bridge runs on until [`Self::end_bridges`].
    pub(crate) async fn bridge(&mut self, by: By, profile: &str) -> String {
        let result = self
            .run(serde_json::json!({ "by": by.name(), "bridge": profile }))
            .await;
        result["answer"].as_str().unwrap_or_default().to_string()
    }

    /// Ends every bridge the CLI started, and every nested CLI holding one.
    pub(crate) async fn end_bridges(&mut self) {
        self.run(serde_json::json!({ "end": true })).await;
    }

    /// Ends the window's CLI; returns its process id.
    pub(crate) async fn exit(&mut self) -> u32 {
        let result = self.run(serde_json::json!({ "exit": true })).await;
        u32::try_from(result["pid"].as_u64().expect("the CLI's pid")).expect("a pid")
    }
}

/// A copy of this test binary under a name of its own, which the lineage profiles run as
/// their window's CLI and may declare as its process image.
pub(crate) struct LineageImage {
    tree: TempDir,
    exe: PathBuf,
    /// The base directories a door or bridge of the window's CLI finds its daemon through,
    /// when a daemon of its own (a `Sandbox`) sets them; one of this copy's own otherwise.
    bases: Option<PathBuf>,
}

impl LineageImage {
    pub(crate) fn install(tag: &str) -> Self {
        let tree = TempDir::new("yagura-lineage", tag);
        let exe = tree.make_dir("bin").join(format!(
            "b8b-cli-{:x}{}{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed),
            std::env::consts::EXE_SUFFIX
        ));
        yagura_test_support::process::install_exe(
            &std::env::current_exe().expect("current_exe"),
            &exe,
        );
        Self {
            tree,
            exe,
            bases: None,
        }
    }

    /// Has what this copy's windows start reach a daemon whose data, state, config and home
    /// directories are `data`, `state`, `config` and `home` under `bases`.
    pub(crate) fn sharing_bases(mut self, bases: &std::path::Path) -> Self {
        self.bases = Some(bases.to_path_buf());
        self
    }

    fn bases_for(&self, id: &str) -> PathBuf {
        self.bases
            .clone()
            .unwrap_or_else(|| self.tree.make_dir(&format!("{id}-bases")))
    }

    /// Renames the copy aside while its windows' CLIs run, and puts a new copy under its
    /// name, as a CLI updating itself does.
    pub(crate) fn rename_aside_and_replace(&self) {
        let aside = self.exe.with_file_name(format!("{}.old", self.image()));
        std::fs::rename(&self.exe, aside).expect("rename the copy aside");
        yagura_test_support::process::install_exe(
            &std::env::current_exe().expect("current_exe"),
            &self.exe,
        );
    }

    /// The copy's file name: the process image its windows' CLIs run under.
    pub(crate) fn image(&self) -> String {
        self.exe
            .file_name()
            .expect("a file name")
            .to_string_lossy()
            .into_owned()
    }

    /// Writes the profile `id` into `profiles_dir`: its window's CLI is this copy, reaching
    /// the daemon of `instance`, declaring the copy's image when `declared`, with `tables`
    /// after its own. Returns that window's CLI.
    pub(crate) fn write_profile(
        &self,
        profiles_dir: &std::path::Path,
        id: &str,
        instance: &str,
        declared: bool,
        tables: &str,
    ) -> LineageCli {
        let dir = self.tree.make_dir(id);
        let config = serde_json::json!({
            "instance": instance,
            "door": daemon_bin(),
            "relay": yagura_test_support::process::relay_path(),
            "bases": self.bases_for(id),
        });
        std::fs::write(dir.join("config.json"), config.to_string()).expect("write the config");
        let command = [
            self.exe.display().to_string(),
            yagura_test_support::process::libtest_name(module_path!(), LINEAGE_ROLE),
            "--exact".to_string(),
            "--ignored".to_string(),
            "--nocapture".to_string(),
            "--test-threads=1".to_string(),
        ]
        .iter()
        .map(|arg| format!("'{arg}'"))
        .collect::<Vec<_>>()
        .join(", ");
        let source = format!(
            "schema = 1\ndisplay_name = \"{id}\"\n\n[command]\ndefault = [{command}]\n\n\
             [env]\n{LINEAGE_ROLE_VAR} = \"cli\"\n{LINEAGE_DIR_VAR} = '{}'\n\n\
             [delivery]\noutput = '{{\"lineageOut\":{{text}}}}'\n\n\
             [hook_events]\n{LINEAGE_EVENT} = {{ status = \"running\", deliver = \"take\", \
             session = \"/session_id\" }}\n{tables}",
            dir.display()
        );
        let source = if declared {
            with_declared_image(&source, &self.image())
        } else {
            source
        };
        write_profile(profiles_dir, id, &source);
        LineageCli { dir, step: 0 }
    }
}

/// The `[host_paths]` table of a profile whose CLI runs in another environment: a WSL
/// distribution's, as far as the daemon can tell. Its converter only echoes the path.
pub(crate) fn guest_host_paths() -> String {
    format!(
        "\n[host_paths]\nconvert = ['{}', 'echo', '0', '{{path}}']\n",
        yagura_test_support::process::capture_child_path().display()
    )
}

/// A daemon reading its profiles from a folder of its own, with an instance doors reach.
pub(crate) async fn lineage_daemon() -> (Harness, String, PathBuf) {
    let instance = unique_instance();
    let profiles_dir = unique_temp_dir("lineage-profiles");
    std::fs::create_dir_all(&profiles_dir).expect("create the profiles directory");
    let config = yagura_daemon::DaemonConfig {
        profiles_dir: Some(profiles_dir.clone()),
        ..approver_base_config()
    };
    let h = Harness::start_on_instance(&instance, config).await;
    (h, instance, profiles_dir)
}

/// Opens a window from the profile `profile` of `h`'s daemon.
pub(crate) async fn open_from_profile(h: &Harness, profile: &str) -> String {
    let mut client = h.connect().await;
    common::send(
        &mut client,
        &yagura_protocol::ClientMessage::OpenWindowFromProfile {
            profile: profile.to_string(),
            cwd: None,
            name: None,
            model: None,
            effort: None,
            on_name_taken: Default::default(),
        },
    )
    .await;
    match common::read_response(&mut client).await {
        yagura_protocol::DaemonMessage::WindowOpened { window_id, .. } => window_id,
        other => panic!("expected window_opened for {profile}, got {other:?}"),
    }
}

/// In a window whose profile declares its CLI's process image, the window's credentials
/// speak for it only from a process whose ancestors up to the window's PTY child hold that
/// image once. A door the window's CLI runs does what its table says, at a door naming the
/// profile and at one naming none. The same doors run by a CLI of that image the window's
/// CLI started print nothing, exit 0, report no status, record no session and take
/// nothing. A window whose profile runs its CLI in another environment (`[host_paths]`) is
/// not judged by the process ids its requests come from, so the nested CLI's door there is
/// only matched, and does what its table says.
#[tokio::test]
async fn only_the_windows_own_cli_speaks_for_a_window_whose_profile_declares_its_image() {
    const DECLARED: &str = "lineage-declared";
    const GUEST: &str = "lineage-guest";
    let (h, instance, profiles_dir) = lineage_daemon().await;
    let image = LineageImage::install("door");
    let mut cli = image.write_profile(&profiles_dir, DECLARED, &instance, true, "");
    let mut guest = image.write_profile(&profiles_dir, GUEST, &instance, true, &guest_host_paths());
    let window_id = open_from_profile(&h, DECLARED).await;
    let guest_id = open_from_profile(&h, GUEST).await;
    let entry = h.registry.get(&window_id).expect("window exists");
    let guest_entry = h.registry.get(&guest_id).expect("window exists");
    let hook = ["--hook", DECLARED, LINEAGE_EVENT];
    let take = ["--deliver-take"];

    entry
        .inbox()
        .deposit(Parcel::new("a", "PARCEL-FOR-THE-WINDOW"));
    let reported = entry.status_event_count();
    for (door, input) in [
        (&hook[..], r#"{"session_id":"sess-nested"}"#),
        (&take[..], ""),
    ] {
        let nested = cli.door(By::Nested, door, input).await;
        assert_eq!(
            nested.code,
            Some(0),
            "{door:?} of the nested CLI exits 0: {nested:?}"
        );
        assert!(
            nested.stdout.is_empty(),
            "{door:?} of the nested CLI prints nothing: {nested:?}"
        );
    }
    assert_eq!(
        entry.status_event_count(),
        reported,
        "the nested CLI's hook reports no status"
    );
    assert_eq!(
        entry.session_id(),
        None,
        "the nested CLI's hook records no session"
    );
    assert_eq!(
        entry.inbox().pending_count(),
        1,
        "neither of the nested CLI's doors takes the parcel"
    );

    let own = cli.door(By::Cli, &take, "").await;
    assert!(
        own.stdout.contains("PARCEL-FOR-THE-WINDOW"),
        "the window's own door that names no profile takes the parcel: {own:?}"
    );
    entry
        .inbox()
        .deposit(Parcel::new("a", "PARCEL-FOR-THE-HOOK"));
    let own = cli
        .door(By::Cli, &hook, r#"{"session_id":"sess-own"}"#)
        .await;
    assert_eq!(own.code, Some(0), "{own:?}");
    assert!(
        own.stdout.contains("PARCEL-FOR-THE-HOOK"),
        "the window's own hook takes the parcel: {own:?}"
    );
    assert_eq!(entry.session_id().as_deref(), Some("sess-own"));
    assert_eq!(entry.inbox().pending_count(), 0);
    assert_eq!(
        entry.status_event_count(),
        reported + 1,
        "the window's own hook reports its status"
    );

    guest_entry
        .inbox()
        .deposit(Parcel::new("a", "PARCEL-FOR-THE-GUEST"));
    let guest_reported = guest_entry.status_event_count();
    let matched = guest
        .door(
            By::Nested,
            &["--hook", GUEST, LINEAGE_EVENT],
            r#"{"session_id":"sess-guest"}"#,
        )
        .await;
    assert!(
        matched.stdout.contains("PARCEL-FOR-THE-GUEST"),
        "a window whose CLI runs in another environment only matches the hook: {matched:?}"
    );
    assert_eq!(guest_entry.status_event_count(), guest_reported + 1);
    assert_eq!(guest_entry.session_id().as_deref(), Some("sess-guest"));

    cli.exit().await;
    guest.exit().await;
}

// --- a WSL guest's hooks: the shipped commands, run for real under dash ------------------
//
// The guest is this machine when it runs Linux: the shipped WSL profile's launch statement
// and hook command are run as they are, with the profile's `wsl -e` left off, and the
// daemon judges what they collect. Where there is no `/proc` there is nothing to read, and
// these cases return without a run.

/// The id of the shipped profile that runs Antigravity CLI in a WSL distribution.
pub(crate) const GUEST_PROFILE: &str = "wsl-antigravity-cli";
/// A hook event that profile names, which reports a status and takes what waits.
pub(crate) const GUEST_EVENT: &str = "PreInvocation";

fn shipped_guest_text() -> String {
    common::docs::read_doc_verbatim("../../profiles/wsl-antigravity-cli.toml")
}

/// The shell script the shipped WSL profile has `bash -c` run before the CLI.
pub(crate) fn guest_statement() -> String {
    let document: toml::Table = shipped_guest_text()
        .parse()
        .expect("the shipped WSL profile is TOML");
    let argv: Vec<String> = document["command"]["windows"]
        .as_array()
        .expect("the shipped WSL profile has a Windows argv")
        .iter()
        .map(|arg| arg.as_str().expect("an argument").to_string())
        .collect();
    assert_eq!(
        argv.len(),
        6,
        "the shipped WSL profile starts its CLI as `wsl -e bash -c <statement> agy`: {argv:?}"
    );
    assert_eq!(argv[..4], ["wsl", "-e", "bash", "-c"]);
    assert_eq!(argv[5], "agy");
    argv[4].clone()
}

/// The shell a guest's CLI runs a hook command with: `/bin/sh`, which is dash on Ubuntu, or
/// `dash` by name where it is not. `None` where there is no guest to read; a Linux without
/// dash fails rather than leaving the case unrun.
pub(crate) fn guest_shell() -> Option<String> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    let is_dash = |path: &str| {
        std::fs::canonicalize(path)
            .ok()
            .and_then(|real| real.file_name().map(|name| name == "dash"))
            .unwrap_or(false)
    };
    let found = ["/bin/sh", "/usr/bin/dash", "/bin/dash"]
        .into_iter()
        .find(|path| is_dash(path));
    assert!(
        found.is_some(),
        "a guest runs a hook command with dash, and this machine has none (/bin/sh is not \
         dash and there is no dash to run it with): install dash"
    );
    found.map(str::to_string)
}

pub(crate) fn bash_path() -> String {
    ["/bin/bash", "/usr/bin/bash"]
        .into_iter()
        .find(|path| std::path::Path::new(path).exists())
        .expect("bash is installed")
        .to_string()
}

fn collect_commands(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::Array(items) => {
            for item in items {
                collect_commands(item, out);
            }
        }
        serde_json::Value::Object(map) => {
            if let Some(command) = map.get("command").and_then(serde_json::Value::as_str) {
                out.push(command.to_string());
            }
            if let Some(inner) = map.get("hooks") {
                collect_commands(inner, out);
            }
        }
        _ => {}
    }
}

/// The hook command the shipped WSL profile registers for [`GUEST_EVENT`], rendered for
/// `instance` with `daemon` as the daemon, naming `profile_id` where it names its profile.
pub(crate) fn guest_hook_command(instance: &str, profile_id: &str, daemon: &str) -> String {
    let profile = parse_profile(GUEST_PROFILE, &shipped_guest_text())
        .expect("the shipped WSL profile is valid");
    let spec = profile
        .mcp_registration
        .as_ref()
        .and_then(|spec| spec.json_entry())
        .expect("the shipped WSL profile registers its hook entries");
    let target = RegistrationTarget::for_instance(Some(instance));
    let vars = EntryVars {
        target: &target,
        os: HostOs::Linux,
        daemon: Some(daemon),
        bridge: Some("/yagura/yagura-mcp"),
    };
    let mut commands = Vec::new();
    for file in spec.files() {
        for (_, value) in file.entries(&vars).expect("the entries render") {
            if let Some(handlers) = value.get(GUEST_EVENT) {
                collect_commands(handlers, &mut commands);
            }
        }
    }
    assert_eq!(
        commands.len(),
        1,
        "the shipped WSL profile registers one command for {GUEST_EVENT}: {commands:?}"
    );
    commands.remove(0).replace(GUEST_PROFILE, profile_id)
}

/// The argv that runs the shipped hook command as a guest's CLI does: the shell, `-c`, the
/// command, with the test's daemon as `{daemon}`.
pub(crate) fn guest_hook_argv(shell: &str, instance: &str, profile_id: &str) -> Vec<String> {
    vec![
        shell.to_string(),
        "-c".to_string(),
        guest_hook_command(instance, profile_id, &daemon_bin().display().to_string()),
    ]
}

/// The command a guest's CLI runs to start the yagura bridge, as the shipped WSL profile's
/// registration has it written for `instance`: what follows `--` in the registration's add
/// command, with `bridge` as the bridge and `profile_id` where it names its profile.
pub(crate) fn guest_server_argv(instance: &str, profile_id: &str, bridge: &str) -> Vec<String> {
    let profile = parse_profile(GUEST_PROFILE, &shipped_guest_text())
        .expect("the shipped WSL profile is valid");
    let spec = profile
        .mcp_registration
        .as_ref()
        .expect("the shipped WSL profile registers the bridge");
    let target = RegistrationTarget::for_instance(Some(instance));
    let add = spec
        .render_add(&target, bridge)
        .expect("the add command renders");
    let at = add
        .iter()
        .position(|arg| arg == "--")
        .expect("the add command separates the server's command with `--`");
    add[at + 1..]
        .iter()
        .map(|arg| {
            if arg == GUEST_PROFILE {
                profile_id.to_string()
            } else {
                arg.clone()
            }
        })
        .collect()
}

/// The pid a process printed and its environment, started through the shipped launch
/// statement (`statement`) or through the plain login-shell wrapper that statement ends with.
fn started_environment(
    statement: Option<&str>,
) -> (String, std::collections::BTreeMap<String, String>) {
    let tree = TempDir::new("yagura-guest-env", "home");
    let home = tree.make_dir("home");
    let mut command = Command::new(bash_path());
    match statement {
        Some(statement) => command.args(["-c", statement]),
        None => command.args(["-lic", "exec \"$0\" \"$@\""]),
    };
    let output = command
        .args(["/bin/sh", "-c", "echo \"PID:$$\"; exec env"])
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .expect("run the launch statement");
    assert!(
        output.status.success(),
        "the launch statement ({statement:?}) must start the CLI it is given: {output:?}"
    );
    // A login shell may print its distribution's greeting first; what follows the pid line
    // is the environment.
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    let mut lines = text.lines().skip_while(|line| !line.starts_with("PID:"));
    let pid = lines
        .next()
        .and_then(|line| line.strip_prefix("PID:"))
        .unwrap_or_default()
        .trim()
        .to_string();
    let variables = lines
        .filter_map(|line| line.split_once('='))
        .filter(|(name, _)| {
            !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        })
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect();
    (pid, variables)
}

/// What the shipped launch statement prints when it starts `program` with `arguments`, run
/// as bash in a guest runs it with a clean environment.
pub(crate) fn run_guest_statement(program: &std::path::Path, arguments: &[&str]) -> String {
    let tree = TempDir::new("yagura-guest-args", "home");
    let home = tree.make_dir("home");
    let output = Command::new(bash_path())
        .args(["-c", &guest_statement()])
        .arg(program)
        .args(arguments)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .expect("run the launch statement");
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// The variable the shipped launch statement exports for the CLI it starts: the guest's root,
/// `<pid>:<boot_id>:<starttime>`.
pub(crate) const GUEST_ROOT_VAR: &str = "YAGURA_GUEST_ROOT";

/// What `shell -c command` prints, run with a window id and the guest root `root` in its
/// environment as a guest's CLI runs a hook command.
pub(crate) fn run_hook_command(shell: &str, command: &str, root: &str) -> String {
    let output = Command::new(shell)
        .args(["-c", command])
        .env("YAGURA_WINDOW_ID", "w")
        .env(GUEST_ROOT_VAR, root)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .expect("run the hook command");
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// The pid of the CLI the shipped launch statement started, and the variables the statement
/// added to its environment beyond what the plain login-shell wrapper gives: the root a
/// guest's requests are judged against.
pub(crate) fn guest_root_variables() -> (String, Vec<(String, String)>) {
    let (pid, started) = started_environment(Some(&guest_statement()));
    let (_, plain) = started_environment(None);
    let added = started
        .into_iter()
        .filter(|(name, _)| !["_", "SHLVL", "PWD", "OLDPWD"].contains(&name.as_str()))
        .filter(|(name, _)| !plain.contains_key(name))
        .collect();
    (pid, added)
}

impl LineageImage {
    /// Writes the profile `id` into `profiles_dir`: the shipped WSL profile's tables for
    /// hooks and delivery, whose CLI is this copy, started through the shipped launch
    /// statement and declaring the copy's image. A `guest_window` also has the
    /// `[host_paths]` of a CLI in another environment. Returns the window's CLI.
    pub(crate) fn write_guest_profile(
        &self,
        profiles_dir: &std::path::Path,
        id: &str,
        instance: &str,
        guest_window: bool,
    ) -> LineageCli {
        let dir = self.tree.make_dir(id);
        let launch: Vec<String> = vec![
            bash_path(),
            "-c".to_string(),
            guest_statement(),
            self.exe.display().to_string(),
            yagura_test_support::process::libtest_name(module_path!(), LINEAGE_ROLE),
            "--exact".to_string(),
            "--ignored".to_string(),
            "--nocapture".to_string(),
            "--test-threads=1".to_string(),
        ];
        let config = serde_json::json!({
            "instance": instance,
            "door": daemon_bin(),
            "relay": yagura_test_support::process::relay_path(),
            "bases": self.bases_for(id),
            "rooted": launch,
        });
        std::fs::write(dir.join("config.json"), config.to_string()).expect("write the config");
        let shipped: toml::Table = shipped_guest_text()
            .parse()
            .expect("the shipped WSL profile is TOML");
        let mut document = toml::Table::new();
        for key in ["schema", "delivery", "hook_timeout", "hook_events"] {
            document.insert(key.to_string(), shipped[key].clone());
        }
        document.insert("display_name".to_string(), id.to_string().into());
        let mut command = toml::Table::new();
        command.insert(
            "default".to_string(),
            toml::Value::Array(launch.into_iter().map(toml::Value::String).collect()),
        );
        document.insert("command".to_string(), toml::Value::Table(command));
        let mut env = toml::Table::new();
        env.insert(LINEAGE_ROLE_VAR.to_string(), "cli".to_string().into());
        env.insert(
            LINEAGE_DIR_VAR.to_string(),
            dir.display().to_string().into(),
        );
        // A daemon of its own (a `Sandbox`) runs with a short PATH; the launch statement and the
        // hook command need the system's tools.
        env.insert(
            "PATH".to_string(),
            std::env::var("PATH").unwrap_or_default().into(),
        );
        // A login shell reads its home's profile files; the user's own are not wanted here.
        env.insert(
            "HOME".to_string(),
            self.tree
                .make_dir(&format!("{id}-home"))
                .display()
                .to_string()
                .into(),
        );
        document.insert("env".to_string(), toml::Value::Table(env));
        if guest_window {
            let convert: Vec<toml::Value> = [
                yagura_test_support::process::capture_child_path()
                    .display()
                    .to_string(),
                "echo".to_string(),
                "0".to_string(),
                "{path}".to_string(),
            ]
            .into_iter()
            .map(toml::Value::String)
            .collect();
            let mut host_paths = toml::Table::new();
            host_paths.insert("convert".to_string(), toml::Value::Array(convert));
            document.insert("host_paths".to_string(), toml::Value::Table(host_paths));
        }
        let source = with_declared_image(
            &toml::to_string(&document).expect("the profile encodes as TOML"),
            &self.image(),
        );
        write_profile(profiles_dir, id, &source);
        LineageCli { dir, step: 0 }
    }
}

/// What a guest's hook is judged by is what the shipped hook command collects, run by dash
/// (the shell a guest's CLI runs it with), and the root the shipped launch statement put in
/// the CLI's environment. In a window whose CLI runs in another environment (`[host_paths]`),
/// the hook command run by the window's own CLI does what its table says: it takes what
/// waits and reports the status. Run by a CLI of the same image that CLI started, it prints
/// nothing, exits 0, takes nothing and reports nothing. A request that carries no root (a
/// window opened before the launch statement exported one) is only matched, whoever runs it.
/// Once the CLI has renamed its file aside to update itself, its hook is still taken.
#[tokio::test]
async fn a_guest_windows_hook_is_judged_by_what_the_shipped_command_collects_under_dash() {
    let Some(shell) = guest_shell() else {
        return;
    };
    const ID: &str = "lineage-wsl";
    let (h, instance, profiles_dir) = lineage_daemon().await;
    let image = LineageImage::install("guest-dash");
    let mut cli = image.write_guest_profile(&profiles_dir, ID, &instance, true);
    let window_id = open_from_profile(&h, ID).await;
    let entry = h.registry.get(&window_id).expect("window exists");
    let hook = guest_hook_argv(&shell, &instance, ID);
    let (_, root_variables) = guest_root_variables();
    let unset: Vec<String> = root_variables.into_iter().map(|(name, _)| name).collect();
    assert!(
        !unset.is_empty(),
        "the shipped launch statement must put a root in the CLI's environment"
    );

    entry.inbox().deposit(Parcel::new("a", "PARCEL-FOR-NESTED"));
    let reported = entry.status_event_count();
    let nested = cli.run_command(By::Nested, &hook, "{}", &[]).await;
    assert_eq!(nested.code, Some(0), "{nested:?}");
    assert!(
        nested.stdout.is_empty(),
        "a CLI the window's CLI started takes nothing: {nested:?}"
    );
    assert_eq!(entry.status_event_count(), reported);
    assert_eq!(entry.inbox().pending_count(), 1);

    let own = cli.run_command(By::Cli, &hook, "{}", &[]).await;
    assert!(
        own.stdout.contains("PARCEL-FOR-NESTED"),
        "the window's own CLI takes the parcel: {own:?}"
    );
    assert_eq!(entry.status_event_count(), reported + 1);
    assert_eq!(entry.inbox().pending_count(), 0);

    entry
        .inbox()
        .deposit(Parcel::new("a", "PARCEL-WITHOUT-ROOT"));
    let rootless = cli.run_command(By::Nested, &hook, "{}", &unset).await;
    assert!(
        rootless.stdout.contains("PARCEL-WITHOUT-ROOT"),
        "a request that carries no root is only matched: {rootless:?}"
    );
    assert_eq!(entry.status_event_count(), reported + 2);

    image.rename_aside_and_replace();
    entry
        .inbox()
        .deposit(Parcel::new("a", "PARCEL-AFTER-UPDATE"));
    let updated = cli.run_command(By::Cli, &hook, "{}", &[]).await;
    assert!(
        updated.stdout.contains("PARCEL-AFTER-UPDATE"),
        "the window's own CLI takes the parcel after renaming its file aside: {updated:?}"
    );
    assert_eq!(entry.status_event_count(), reported + 3);

    cli.exit().await;
}

/// Which rule judges a window is a fact of the window, not of what a request says. In a
/// window run directly (no `[host_paths]`), a CLI of the same image that CLI started through
/// the launch statement names a root of its own and a chain that counts one image up to it,
/// yet takes nothing, because the process ids show two; the window's own CLI is taken.
#[tokio::test]
async fn a_window_run_directly_is_judged_by_process_ids_whatever_root_a_request_names() {
    let Some(shell) = guest_shell() else {
        return;
    };
    const ID: &str = "lineage-wsl-direct";
    let (h, instance, profiles_dir) = lineage_daemon().await;
    let image = LineageImage::install("guest-direct");
    let mut cli = image.write_guest_profile(&profiles_dir, ID, &instance, false);
    let window_id = open_from_profile(&h, ID).await;
    let entry = h.registry.get(&window_id).expect("window exists");
    let hook = guest_hook_argv(&shell, &instance, ID);

    entry.inbox().deposit(Parcel::new("a", "PARCEL-FOR-ROOTED"));
    let reported = entry.status_event_count();
    let rooted = cli.run_command(By::Rooted, &hook, "{}", &[]).await;
    assert_eq!(rooted.code, Some(0), "{rooted:?}");
    assert!(
        rooted.stdout.is_empty(),
        "a CLI that names a root of its own does not become the window's: {rooted:?}"
    );
    assert_eq!(entry.status_event_count(), reported);
    assert_eq!(entry.inbox().pending_count(), 1);

    let own = cli.run_command(By::Cli, &hook, "{}", &[]).await;
    assert!(
        own.stdout.contains("PARCEL-FOR-ROOTED"),
        "the window's own CLI, whose requests carry the facts too, is taken: {own:?}"
    );
    assert_eq!(entry.status_event_count(), reported + 1);

    cli.exit().await;
}
