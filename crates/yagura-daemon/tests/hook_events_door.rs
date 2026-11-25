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
use crate::common::bins;
use crate::common::door_relay::{
    Printed, claim_endpoint, door_command, read_json_frame, relay_door, run_door, run_with_input,
    unique_instance,
};

use std::path::PathBuf;
use std::process::Output;

use std::time::Duration;

use yagura_daemon::cli::{HOOK_DOOR_MARGIN, HOOK_STDIN_DRAIN_GRACE, door_drain_wait, door_prints};
use yagura_daemon::inbox::Parcel;
use yagura_daemon::profile::parse_profile;
use yagura_daemon::window::{HooksRoute, LaunchIdentity, WindowProvenance};
use yagura_protocol::{DOOR_ANSWER_TIMEOUT, WindowStatus};
use yagura_test_support::paths::TempDir;

use common::{
    Harness, VALID_TOKEN, approver_base_config, detection_config, query_status, unique_temp_dir,
    wait_until_status, write_profile,
};

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
        common::bins::echo_child_path().display()
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
        common::bins::echo_child_path().display()
    )
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
                h.open_delivering(bins::echo_child_command(), Some(name))
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
        run_door(&self.h, window_id, args, input).await
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
                command: bins::echo_child_command(),
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

/// What a stand-in daemon does with one frame the door sends.
enum Reply {
    /// Answers the frame with this message.
    Answer(serde_json::Value),
    /// Leaves the frame unanswered and reads on.
    Silent,
    /// Closes the connection without answering the frame.
    Close,
}

/// A stand-in for a daemon at `instance`'s endpoint: it meets each frame the one door that
/// connects sends with the next of `replies`, then records what else the door sends until
/// it closes. Binding claims the endpoint, so the door can reach nothing else.
async fn stand_in_daemon(
    instance: &str,
    replies: Vec<Reply>,
) -> tokio::task::JoinHandle<Vec<serde_json::Value>> {
    use tokio::io::AsyncWriteExt;

    let accept = claim_endpoint(instance).await;
    tokio::spawn(async move {
        let mut conn = accept.await;
        let mut frames = Vec::new();
        let mut replies = replies.into_iter();
        while let Some(frame) = read_json_frame(&mut conn).await {
            frames.push(frame);
            match replies.next().unwrap_or(Reply::Silent) {
                Reply::Answer(answer) => {
                    let body = serde_json::to_vec(&answer).unwrap();
                    let sent = conn
                        .write_all(&yagura_protocol::frame_encode(&body))
                        .await
                        .and(conn.flush().await);
                    // A door gone before its answer sent all it will; its frames say so.
                    if sent.is_err() {
                        return frames;
                    }
                }
                Reply::Silent => {}
                Reply::Close => return frames,
            }
        }
        frames
    })
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

/// Runs the door `args` against a stand-in daemon meeting its frames with `replies`; returns
/// the door's output and every frame it sent.
async fn run_against_stand_in(
    args: &[&str],
    replies: Vec<Reply>,
) -> (Output, Vec<serde_json::Value>) {
    let instance = unique_instance();
    let bases = TempDir::new("yagura-door", "stand-in");
    let frames = stand_in_daemon(&instance, replies).await;
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
        let (output, frames) = run_against_stand_in(&args, vec![Reply::Answer(answer)]).await;
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
    let named = Duration::from_secs(5);
    for (elapsed, prints) in [
        (named - HOOK_DOOR_MARGIN, true),
        (named - HOOK_DOOR_MARGIN + Duration::from_millis(1), false),
        (named, false),
    ] {
        assert_eq!(
            door_prints(Some(5), elapsed),
            prints,
            "a door named 5 seconds, {elapsed:?} after it started: it prints only with its \
             margin left"
        );
    }
    for (hook_timeout_sec, elapsed, waits) in [
        (None, Duration::from_secs(60), HOOK_STDIN_DRAIN_GRACE),
        (Some(60), Duration::ZERO, HOOK_STDIN_DRAIN_GRACE),
        (Some(5), Duration::ZERO, HOOK_STDIN_DRAIN_GRACE),
        (Some(5), Duration::from_secs(3), Duration::from_millis(1500)),
        (Some(5), named - HOOK_DOOR_MARGIN, Duration::ZERO),
        (Some(5), named, Duration::ZERO),
        (Some(5), named + Duration::from_secs(1), Duration::ZERO),
    ] {
        assert_eq!(
            door_drain_wait(hook_timeout_sec, elapsed),
            waits,
            "a door named {hook_timeout_sec:?} seconds, {elapsed:?} after it started: it waits              for stdin never into its margin and never past the drain bound"
        );
    }
}

/// A daemon that names no deadline knows no time the window's CLI gives the hook, so the
/// door does not measure one: it prints however long it has run, then says so, and ends at
/// once without an answer to that.
#[tokio::test]
async fn a_door_whose_daemon_names_no_deadline_prints_then_says_so_and_ends_without_an_answer() {
    for elapsed in [Duration::ZERO, DOOR_ANSWER_TIMEOUT + Duration::from_secs(1)] {
        assert!(
            door_prints(None, elapsed),
            "a door whose daemon names no time prints {elapsed:?} after it started"
        );
    }
    for (args, _) in stand_in_doors() {
        let answer = serde_json::json!({
            "type": "delivery",
            "output": "PARCEL-WITH-THE-OLD-DEADLINE",
            "stream": "stdout",
            "exit_code": 0,
        });
        let (output, frames) = run_against_stand_in(&args, vec![Reply::Answer(answer)]).await;
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
        let (output, frames) = run_against_stand_in(&args, vec![Reply::Answer(answer)]).await;
        assert!(
            output.status.code() == Some(0) && output.stdout.is_empty(),
            "{args:?}: {}",
            describe(&output)
        );
        assert_eq!(frame_types(&frames), ["door_read"], "{args:?}");
    }
}

/// A status door reads its daemon's answer up to the `pong` to the `ping` it sends after
/// its event: a refusal before the `pong` is read past, and a daemon that closes before
/// the `pong` ends the door with a failure, whatever it answered before.
#[tokio::test]
async fn a_status_door_ends_with_the_pong_to_its_ping_or_fails_without_it() {
    let refused = || {
        Reply::Answer(serde_json::json!({
            "type": "error",
            "code": "auth_failed",
            "message": "the window's credentials are not valid",
        }))
    };
    let pong = || Reply::Answer(serde_json::json!({ "type": "pong" }));
    let args = ["--emit-status", "waiting_input", "--source", "hook"];
    let mut wrong = Vec::new();
    for (case, replies, ends_with_pong) in [
        (
            "an event met with a pong",
            vec![Reply::Silent, pong()],
            true,
        ),
        (
            "closed before the pong",
            vec![Reply::Silent, Reply::Close],
            false,
        ),
        ("a refusal then a pong", vec![refused(), pong()], true),
        (
            "a refusal then closed",
            vec![refused(), Reply::Close],
            false,
        ),
    ] {
        let (output, frames) = run_against_stand_in(&args, replies).await;
        let sent = frame_types(&frames);
        let succeeded = output.status.code() == Some(0);
        if sent != ["status_event", "ping"] || succeeded != ends_with_pong {
            wrong.push(format!(
                "{case}: sent {sent:?}, must exit 0 only with the pong ({ends_with_pong}); {}",
                describe(&output)
            ));
        }
    }
    assert!(
        wrong.is_empty(),
        "a status door sends its event, then a ping, and ends with 0 only on the pong:\n{}",
        wrong.join("\n")
    );
}

/// The time a window's doors are named is the one its CLI was given at the window's open:
/// the profile rewritten after it, or its `[hook_timeout]` taken out, changes nothing for
/// that window's `--deliver-context` or `--hook` door. A window opened from no profile was
/// given no time the daemon knows, so its doors are named none, whatever profile a hook
/// names.
#[tokio::test]
async fn a_windows_doors_are_named_the_time_its_cli_was_given_at_its_open() {
    const ID: &str = "door-time-at-open";
    let (h, _instance, profiles_dir) = own_profiles_daemon().await;
    let source = |table: &str| {
        format!(
            "schema = 1\ndisplay_name = \"{ID}\"\n\n[command]\ndefault = ['{}']\n\n\
             [delivery]\noutput = '{{\"timeOut\":{{text}}}}'\n\n\
             [hook_events]\n{EVENT} = {{ status = \"running\", deliver = \"take\" }}\n\n{table}",
            common::bins::echo_child_path().display()
        )
    };
    write_profile(
        &profiles_dir,
        ID,
        &source(
            "[hook_timeout]\ndefault = 40\ndoors = { context = 41 }\nevents = { PostToolUse = 42 }\n",
        ),
    );
    let window_id = open_from_profile(&h, ID).await;
    let bare_id = h.open(bins::echo_child_command(), None).await;
    let context = serde_json::json!({ "kind": "context" });
    let hook = serde_json::json!({ "kind": "hook", "profile": ID, "name": EVENT });
    let named = |answer: &serde_json::Value| {
        assert_eq!(answer["type"], "delivery", "the read is answered: {answer}");
        answer.get("hook_timeout_sec").cloned()
    };

    for door in [&context, &hook] {
        let answer = door_answer(&h.endpoint, &bare_id, VALID_TOKEN, door.clone()).await;
        assert_eq!(
            named(&answer),
            None,
            "a window opened from no profile is named no time at {door}: {answer}"
        );
    }
    for (table, change) in [
        (
            "[hook_timeout]\ndefault = 50\ndoors = { context = 51 }\nevents = { PostToolUse = 52 }\n",
            "rewritten",
        ),
        ("", "without its [hook_timeout]"),
    ] {
        write_profile(&profiles_dir, ID, &source(table));
        for (door, opened) in [(&context, 41), (&hook, 42)] {
            let answer = door_answer(&h.endpoint, &window_id, VALID_TOKEN, door.clone()).await;
            assert_eq!(
                named(&answer),
                Some(serde_json::json!(opened)),
                "with the profile {change}, the window is still named the time its CLI was \
                 given at its open at {door}: {answer}"
            );
        }
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
fn stop_cases() -> [StopCase; 3] {
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
    ]
}

/// A stop that is not a continuation, as Grok Build's stop input says it.
const GROK_FIRST_STOP: &str = r#"{"stop_hook_active":false,"stopHookActive":false}"#;

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

/// A stop's door decides the window's status where it ends: a block the door printed
/// leaves the turn running; a read that ends without that notice leaves the window
/// waiting for input.
#[tokio::test]
async fn a_stops_status_is_decided_where_its_door_ends() {
    let setup = Setup::start_with_config(Screen::Ready, true, |config| {
        let dir = config
            .detection_dir
            .as_deref()
            .expect("the configuration names a detection directory");
        common::write_turn_end_key_definition(dir);
    })
    .await;
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

    // Printed after a press of the turn-end key: the CLI goes on with the block, which ends
    // the press's hold before the block's own running is recorded.
    let pressed = setup
        .h
        .open_delivering(
            common::views::script_argv(),
            Some(common::TURN_END_KEY_DETECTION),
        )
        .await;
    let entry = setup.h.registry.get(&pressed).expect("window exists");
    common::open_turn_after_the_key_was_named(&entry).await;
    common::interrupt_with_the_turn_end_key(&entry).await;
    entry.inbox().deposit(Parcel::new("a", "PARCEL-PRESSED"));
    let mut published = entry.subscribe_status();
    let mut relayed = relay_door(&setup.h, &pressed, &args, first_stop(), Printed::Forward).await;
    assert!(
        String::from_utf8_lossy(&relayed.output.stdout).contains("PARCEL-PRESSED"),
        "{}",
        describe(&relayed.output)
    );
    // On the relay's connection, so the daemon has taken the door's notice before the mark.
    query_status(&mut relayed.upstream, &pressed).await;
    // The answer only says the notice is queued for the detector; the block's running is
    // evaluated against the live screen, so the mark waits until that running is published.
    // Not published here means the block did not end the press's hold, which hangs this wait.
    common::published_until(&mut published, WindowStatus::Running).await;
    common::views::entry_script_write(&entry, common::TURN_END_KEY_MARK).await;
    common::published_until(&mut published, WindowStatus::WaitingApproval).await;
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
    let before_running = common::published_until(&mut published, WindowStatus::Running).await;
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
    let before_the_mark = common::published_until(&mut published, WindowStatus::Running).await;
    assert!(
        before_the_mark.is_empty(),
        "the end of a door that began before the newer status must not overwrite it; published {before_the_mark:?} before the later report"
    );
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

// --- a daemon of its own profiles, and a door read in a window's name --------------------

/// A daemon reading its profiles from a folder of its own, with an instance doors reach.
pub(crate) async fn own_profiles_daemon() -> (Harness, String, PathBuf) {
    let instance = unique_instance();
    let profiles_dir = unique_temp_dir("door-profiles");
    std::fs::create_dir_all(&profiles_dir).expect("create the profiles directory");
    let config = yagura_daemon::DaemonConfig {
        profiles_dir: Some(profiles_dir.clone()),
        ..approver_base_config()
    };
    let h = Harness::start_on_instance(&instance, config).await;
    (h, instance, profiles_dir)
}

/// What the daemon at `endpoint` answers a read of `door` (its wire form) in the name of
/// `window_id` with `token`, as its wire form. A hook asked for values of its input reports
/// them all absent.
pub(crate) async fn door_answer(
    endpoint: &str,
    window_id: &str,
    token: &str,
    door: serde_json::Value,
) -> serde_json::Value {
    let read = |input: Option<Vec<serde_json::Value>>| -> yagura_protocol::ClientMessage {
        let mut door = door.clone();
        if let Some(input) = input {
            door["input"] = serde_json::Value::Array(input);
        }
        let wire = serde_json::json!({
            "type": "door_read",
            "window_id": window_id,
            "token": token,
            "door": door,
        });
        serde_json::from_value(wire.clone())
            .unwrap_or_else(|err| panic!("the wire must know a door's read {wire}: {err}"))
    };
    let mut client = yagura_test_support::ipc::connect_with_retry(endpoint).await;
    let mut answer = common::call_door(&mut client, &read(None)).await;
    if let yagura_protocol::DaemonMessage::HookInputWanted { pointers } = &answer {
        let input = vec![serde_json::json!("absent"); pointers.len()];
        answer = common::call_door(&mut client, &read(Some(input))).await;
    }
    serde_json::to_value(&answer).expect("a daemon message serializes")
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

/// The path of bash on this machine.
pub(crate) fn bash_path() -> String {
    ["/bin/bash", "/usr/bin/bash"]
        .into_iter()
        .find(|path| std::path::Path::new(path).exists())
        .expect("bash is installed")
        .to_string()
}

// --- an AI's interrupt: a mark on the window's main turn ---------------------------------

/// What an answer without an `outcome` reads as, so a failure says what was missing.
const NO_OUTCOME: &str = "<no outcome>";
const TURN_SCREEN: &str = "TURN-DOOR-READY";
const TURN_DETECTION: &str = "hook-door-turns";

/// The form of a profile's stop answer for one of its stop events.
#[derive(Clone, Copy, Debug)]
enum StopForm {
    /// `{"continue":false,"stopReason":…}`.
    Continue,
    /// `{"permissionDecision":"deny","permissionDecisionReason":…}`.
    Deny,
    /// [`StopForm::Continue`] with a `PreToolUse` `hookSpecificOutput` that denies the tool.
    ContinueDenyingTool,
}

/// A daemon reading the shipped profiles, letting any caller interrupt a window, with one
/// window opened from a shipped profile whose screen reads `waiting_input` until a hook
/// reports a turn.
struct Turns {
    h: Harness,
    window_id: String,
    profile: &'static str,
}

impl Turns {
    async fn start(profile: &'static str) -> Self {
        Self::start_with(profile, common::wake_none_delivery(), Some(TURN_DETECTION)).await
    }

    /// [`Turns::start`] with `layer` laid over the shipped profile.
    async fn start_layered(profile: &'static str, layer: &str) -> Self {
        Self::start_in(
            profile,
            common::wake_none_delivery(),
            Some(TURN_DETECTION),
            layer,
        )
        .await
    }

    /// [`Turns::start`] with the window's own `[delivery]`, under `detection` (`None`
    /// leaves the window `Unknown`).
    async fn start_with(
        profile: &'static str,
        delivery: yagura_daemon::profile_delivery::ProfileDelivery,
        detection: Option<&str>,
    ) -> Self {
        Self::start_in(profile, delivery, detection, "").await
    }

    async fn start_in(
        profile: &'static str,
        delivery: yagura_daemon::profile_delivery::ProfileDelivery,
        detection: Option<&str>,
        layer: &str,
    ) -> Self {
        let instance = unique_instance();
        let profiles_dir = unique_temp_dir("interrupt-turn-profiles");
        std::fs::create_dir_all(&profiles_dir).expect("create the profiles directory");
        if !layer.is_empty() {
            write_profile(&profiles_dir, profile, layer);
        }
        let detection_dir = unique_temp_dir("interrupt-turns");
        let mut config = detection_config(&detection_dir, approver_base_config());
        config.profiles_dir = Some(profiles_dir);
        config.bundled_profiles = true;
        config.approval_policy = Some(common::policy(&["open_window", "interrupt_window"]));
        std::fs::write(
            detection_dir.join(format!("{TURN_DETECTION}.toml")),
            format!(
                "schema_version = 1\nname = \"{TURN_DETECTION}\"\nbottom_line_count = 5\n\n\
                 [debounce]\nconfirm_ms = 30\n\n[[rules]]\nstatus = \"waiting_input\"\n\
                 pattern = '{TURN_SCREEN}'\npriority = 10\nregion = \"bottom\"\n"
            ),
        )
        .expect("write the turn window's definition");
        let h = Harness::start_on_instance(&instance, config).await;
        let window_id = h
            .registry
            .open_with_extra_env(
                yagura_daemon::OpenWindowRequest {
                    command: bins::echo_child_command(),
                    cwd: None,
                    name: None,
                    vendor: None,
                    detection: Some(
                        detection
                            .unwrap_or(common::CONTROL_DETECTION_NAME)
                            .to_string(),
                    ),
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
                    delivery: Some(delivery),
                    hooks: HooksRoute::Unarranged,
                    ..WindowProvenance::default()
                },
            )
            .await
            .expect("the launched window opens")
            .window_id;
        if detection.is_some() {
            h.write_input(&window_id, format!("{TURN_SCREEN}\r").as_bytes())
                .await;
            let mut client = h.connect().await;
            wait_until_status(&mut client, &window_id, WindowStatus::WaitingInput).await;
        }
        Self {
            h,
            window_id,
            profile,
        }
    }

    fn entry(&self) -> std::sync::Arc<yagura_daemon::WindowEntry> {
        self.h.registry.get(&self.window_id).expect("window exists")
    }

    /// Runs the window's CLI's hook for `event` with `input`; it must take the input and
    /// end with 0.
    async fn hook(&self, event: &str, input: &str) -> Output {
        let (output, written) = run_door(
            &self.h,
            &self.window_id,
            &["--hook", self.profile, event],
            input.as_bytes().to_vec(),
        )
        .await;
        assert!(written.is_ok(), "{} {event}: {written:?}", self.profile);
        assert_eq!(
            output.status.code(),
            Some(0),
            "{} {event}: {}",
            self.profile,
            describe(&output)
        );
        output
    }

    /// Runs `event` and asserts it was answered as nothing but the event: no stop answer.
    async fn hook_unstopped(&self, event: &str, input: &str, why: &str) -> Output {
        let output = self.hook(event, input).await;
        assert!(
            stop_answer(&output).is_none(),
            "{} {event}: {why}, so it gets no stop answer; {}",
            self.profile,
            describe(&output)
        );
        output
    }

    /// Runs `event` and asserts it was answered with the stop answer of `form`.
    async fn hook_stopped(&self, event: &str, input: &str, form: StopForm, why: &str) {
        let output = self.hook(event, input).await;
        assert_stop_form(&output, form, &format!("{} {event}: {why}", self.profile));
    }

    /// `interrupt_window` on the window from a connection that is no window's.
    async fn interrupt(&self) -> Result<String, String> {
        let mut client = self.h.connect().await;
        interrupt_from(&mut client, &self.window_id).await
    }

    /// Asserts `interrupt_window` answered `expected`, as `why` says.
    async fn interrupt_answers(&self, expected: &str, why: &str) {
        assert_eq!(
            self.interrupt().await,
            Ok(expected.to_string()),
            "{}: {why}",
            self.profile
        );
    }

    /// Asserts `interrupt_window` answered that no turn is open: it put no mark and ended
    /// nothing.
    async fn interrupt_finds_no_open_turn(&self, why: &str) {
        self.interrupt_answers("no_open_turn", why).await;
    }

    fn deposit(&self, text: &str) {
        self.entry().inbox().deposit(Parcel::new(text, text));
    }
}

/// Sends `interrupt_window` for `window_id` over `client`: the answer's `outcome`, or the
/// error code it was refused with.
pub(crate) async fn interrupt_from(
    client: &mut yagura_test_support::ipc::Connection,
    window_id: &str,
) -> Result<String, String> {
    common::send(
        client,
        &yagura_protocol::ClientMessage::InterruptWindow {
            window_id: window_id.to_string(),
        },
    )
    .await;
    let answer = common::read_response(client).await;
    match &answer {
        yagura_protocol::DaemonMessage::Error { code, .. } => Err(code.clone()),
        yagura_protocol::DaemonMessage::WindowInterrupted { .. } => {
            let encoded = serde_json::to_value(&answer).expect("an answer encodes");
            assert_eq!(encoded["window_id"].as_str(), Some(window_id), "{encoded}");
            Ok(encoded["outcome"]
                .as_str()
                .unwrap_or(NO_OUTCOME)
                .to_string())
        }
        other => panic!("expected window_interrupted or an error, got {other:?}"),
    }
}

/// What the door printed, when it is a stop answer of any form: a JSON object that ends
/// the turn or denies the tool.
fn stop_answer(output: &Output) -> Option<serde_json::Value> {
    let printed: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).ok()?;
    let stops = printed["continue"] == serde_json::Value::Bool(false)
        || printed["permissionDecision"].as_str() == Some("deny")
        || printed["hookSpecificOutput"]["permissionDecision"].as_str() == Some("deny");
    stops.then_some(printed)
}

fn assert_stop_form(output: &Output, form: StopForm, who: &str) {
    let printed = stop_answer(output)
        .unwrap_or_else(|| panic!("{who}: expected a stop answer; {}", describe(output)));
    let named = |value: &serde_json::Value| value.as_str().is_some_and(|text| !text.is_empty());
    let holds = match form {
        StopForm::Continue => {
            printed["continue"] == serde_json::Value::Bool(false) && named(&printed["stopReason"])
        }
        StopForm::Deny => {
            printed["permissionDecision"].as_str() == Some("deny")
                && named(&printed["permissionDecisionReason"])
        }
        StopForm::ContinueDenyingTool => {
            let specific = &printed["hookSpecificOutput"];
            printed["continue"] == serde_json::Value::Bool(false)
                && named(&printed["stopReason"])
                && specific["hookEventName"].as_str() == Some("PreToolUse")
                && specific["permissionDecision"].as_str() == Some("deny")
                && named(&specific["permissionDecisionReason"])
        }
    };
    assert!(
        holds,
        "{who}: expected the {form:?} stop answer, got {printed}"
    );
}

const GROK_SUBAGENT: &str = r#"{"subagentType":"explore"}"#;

/// A window no main hook has reached yet: its CLI may have no door, so an interrupt is a
/// mark that may never be read, and a subagent's hook does not change that. The next main
/// stop event is stopped all the same; once a main turn starts, its stop events are not.
#[tokio::test]
async fn a_window_no_main_hook_reached_is_marked_unconfirmed_and_a_turn_started_later_is_not_stopped()
 {
    let turns = Turns::start("grok-build").await;
    turns
        .hook_unstopped("PreToolUse", GROK_SUBAGENT, "nothing was interrupted")
        .await;
    turns
        .interrupt_answers(
            "stop_requested_unconfirmed",
            "only a subagent's hook has reached the daemon",
        )
        .await;
    turns
        .hook_stopped(
            "PreToolUse",
            "{}",
            StopForm::Continue,
            "the window was marked while no turn had been seen",
        )
        .await;

    turns
        .hook_unstopped("UserPromptSubmit", "{}", "a turn's start is never stopped")
        .await;
    turns
        .hook_unstopped(
            "PreToolUse",
            "{}",
            "the turn started after the mark was put",
        )
        .await;
    turns
        .interrupt_answers("stop_requested", "a main hook has reached the daemon")
        .await;
}

/// In an open turn, every stop event of the main session and of its subagents is
/// stopped, however often the AI asks, until a main stop answer whose answer ends the turn
/// (Grok Build's `PostToolUse`); the answer leaves the inbox alone. A subagent after it is
/// not stopped, and an interrupt then finds no open turn. A stop event whose profile also
/// has it hand a block is stopped all the same, and what waits stays in the inbox.
#[tokio::test]
async fn an_open_turn_is_stopped_at_every_stop_event_until_a_stop_answer_that_ends_it() {
    let turns = Turns::start("grok-build").await;
    turns
        .hook_unstopped("UserPromptSubmit", "{}", "a turn's start is never stopped")
        .await;
    turns
        .interrupt_answers("stop_requested", "a main hook started the turn")
        .await;
    turns
        .interrupt_answers("stop_requested", "asking again is the same request")
        .await;
    turns
        .hook_stopped(
            "PreToolUse",
            GROK_SUBAGENT,
            StopForm::Continue,
            "a subagent of the open turn",
        )
        .await;
    for round in ["first", "second"] {
        turns
            .hook_stopped(
                "PreToolUse",
                "{}",
                StopForm::Continue,
                &format!(
                    "the {round} main PreToolUse: its answer does not end the turn, so the \
                     mark still applies"
                ),
            )
            .await;
    }

    turns.deposit("PARCEL-KEPT-BY-STOP");
    turns
        .hook_stopped(
            "PostToolUse",
            "{}",
            StopForm::Continue,
            "the main PostToolUse of the marked turn",
        )
        .await;
    assert_eq!(
        turns.entry().inbox().pending_count(),
        1,
        "a stop answer reads nothing from the inbox"
    );

    turns
        .hook_unstopped(
            "PreToolUse",
            GROK_SUBAGENT,
            "the main PostToolUse's stop answer ended the turn",
        )
        .await;
    turns
        .interrupt_finds_no_open_turn("the main PostToolUse's stop answer ended the turn")
        .await;
    let taken = turns
        .hook_unstopped("PostToolUse", "{}", "no turn is open")
        .await;
    assert!(
        String::from_utf8_lossy(&taken.stdout).contains("PARCEL-KEPT-BY-STOP"),
        "the parcel the stop answer left is the next door's; {}",
        describe(&taken)
    );

    // A stop event that also hands a block is answered with the stop in its place.
    let blocking = Turns::start_layered(
        "grok-build",
        "[hook_events]\nPreToolUse = { status = \"running\", deliver = \"block\" }\n",
    )
    .await;
    blocking
        .hook_unstopped("UserPromptSubmit", "{}", "a turn's start is never stopped")
        .await;
    blocking
        .interrupt_answers("stop_requested", "a main hook started the turn")
        .await;
    blocking.deposit("PARCEL-KEPT-BY-BLOCKING-STOP");
    blocking
        .hook_stopped(
            "PreToolUse",
            "{}",
            StopForm::Continue,
            "the main PreToolUse of the marked turn, which would hand a block otherwise",
        )
        .await;
    assert_eq!(
        blocking.entry().inbox().pending_count(),
        1,
        "a stop answer in place of a block reads nothing from the inbox"
    );
}

/// A turn its end event closed takes no mark: a subagent working while the window waits
/// is not stopped. A stop's block hands the next round its text, which starts a turn of
/// its own: a mark put before it does not stop that round.
#[tokio::test]
async fn a_turn_its_end_event_closed_takes_no_mark_and_a_stops_block_starts_the_next() {
    let turns = Turns::start("grok-build").await;
    turns
        .hook_unstopped("UserPromptSubmit", "{}", "a turn's start is never stopped")
        .await;
    turns
        .hook_unstopped("Stop", GROK_FIRST_STOP, "a turn's end is never stopped")
        .await;
    turns
        .interrupt_finds_no_open_turn("the last main hook ended the turn")
        .await;
    turns
        .hook_unstopped(
            "PreToolUse",
            GROK_SUBAGENT,
            "a subagent working while the window waits is outside any turn",
        )
        .await;
    turns
        .hook_unstopped("PreToolUse", "{}", "no mark was put")
        .await;

    turns
        .hook_unstopped("UserPromptSubmit", "{}", "a turn's start is never stopped")
        .await;
    turns
        .interrupt_answers("stop_requested", "a main hook started the turn")
        .await;
    turns.deposit("PARCEL-FOR-THE-BLOCK");
    let block = turns.hook("Stop", GROK_FIRST_STOP).await;
    assert!(
        String::from_utf8_lossy(&block.stdout).contains("PARCEL-FOR-THE-BLOCK"),
        "a stop with something waiting hands it over in a block; {}",
        describe(&block)
    );
    turns
        .hook_unstopped(
            "PreToolUse",
            "{}",
            "the block's round started after the mark was put",
        )
        .await;
    turns
        .interrupt_answers("stop_requested", "the block's round is an open turn")
        .await;
}

/// A turn's end is what its CLI's hooks say, never the window's status: a long tool with
/// no output can leave a `waiting_input` status while the turn is still open.
#[tokio::test]
async fn a_mark_outlives_a_waiting_input_status_no_end_event_reported() {
    let turns = Turns::start("grok-build").await;
    turns
        .hook_unstopped("UserPromptSubmit", "{}", "a turn's start is never stopped")
        .await;
    turns
        .interrupt_answers("stop_requested", "a main hook started the turn")
        .await;
    let mut client = turns.h.connect().await;
    common::send(
        &mut client,
        &yagura_protocol::ClientMessage::StatusEvent {
            source: "interrupt-test".to_string(),
            status: WindowStatus::WaitingInput,
            window_id: Some(turns.window_id.clone()),
            token: VALID_TOKEN.to_string(),
        },
    )
    .await;
    wait_until_status(&mut client, &turns.window_id, WindowStatus::WaitingInput).await;
    turns
        .hook_stopped(
            "PreToolUse",
            "{}",
            StopForm::Continue,
            "no end event has reported the turn's end",
        )
        .await;
}

/// Copilot CLI's deny does not end its turn: every tool of the open turn is denied, main
/// or subagent, until the turn's end event. A stop's block then starts a round that is
/// not denied.
#[tokio::test]
async fn copilot_denies_every_tool_of_an_open_turn_until_its_end_event() {
    const SUBAGENT: &str = r#"{"agentId":"agent-1"}"#;
    const FIRST_STOP: &str = r#"{"stop_hook_active":false}"#;
    let turns = Turns::start("copilot-cli").await;
    turns
        .hook_unstopped(
            "userPromptSubmitted",
            "{}",
            "a turn's start is never stopped",
        )
        .await;
    turns
        .interrupt_answers("stop_requested", "a main hook started the turn")
        .await;
    turns
        .hook_stopped("preToolUse", SUBAGENT, StopForm::Deny, "a subagent's tool")
        .await;
    for round in ["first", "second"] {
        turns
            .hook_stopped(
                "preToolUse",
                "{}",
                StopForm::Deny,
                &format!("the {round} main tool: a deny does not end the turn"),
            )
            .await;
    }
    turns
        .hook_unstopped("agentStop", FIRST_STOP, "a turn's end is never stopped")
        .await;
    turns
        .hook_unstopped("preToolUse", "{}", "the end event closed the turn")
        .await;
    turns
        .interrupt_finds_no_open_turn("the last main hook ended the turn")
        .await;

    turns
        .hook_unstopped(
            "userPromptSubmitted",
            "{}",
            "a turn's start is never stopped",
        )
        .await;
    turns
        .interrupt_answers("stop_requested", "a main hook started the turn")
        .await;
    turns.deposit("PARCEL-FOR-COPILOT-BLOCK");
    let block = turns.hook("agentStop", FIRST_STOP).await;
    assert!(
        String::from_utf8_lossy(&block.stdout).contains("PARCEL-FOR-COPILOT-BLOCK"),
        "{}",
        describe(&block)
    );
    turns
        .hook_unstopped(
            "preToolUse",
            "{}",
            "the block's round started after the mark was put",
        )
        .await;
}

/// Claude Code's `PreToolUse` stop answer denies the tool and ends the turn, so a
/// subagent after it is not stopped; its `PostToolUse` ends the turn too.
#[tokio::test]
async fn claude_codes_stop_answer_denies_the_tool_and_ends_the_turn() {
    const SUBAGENT: &str = r#"{"agent_id":"agent-1"}"#;
    let turns = Turns::start("claude-code").await;
    turns
        .hook_unstopped("UserPromptSubmit", "{}", "a turn's start is never stopped")
        .await;
    turns
        .interrupt_answers("stop_requested", "a main hook started the turn")
        .await;
    turns
        .hook_stopped(
            "PreToolUse",
            "{}",
            StopForm::ContinueDenyingTool,
            "the main tool of the marked turn",
        )
        .await;
    turns
        .hook_unstopped(
            "PreToolUse",
            SUBAGENT,
            "the PreToolUse stop answer ended the turn",
        )
        .await;
    turns
        .interrupt_finds_no_open_turn("the PreToolUse stop answer ended the turn")
        .await;

    turns
        .hook_unstopped("UserPromptSubmit", "{}", "a turn's start is never stopped")
        .await;
    turns
        .interrupt_answers("stop_requested", "a main hook started the turn")
        .await;
    turns
        .hook_stopped(
            "PostToolUse",
            "{}",
            StopForm::Continue,
            "the main tool result of the marked turn",
        )
        .await;
    turns
        .interrupt_finds_no_open_turn("the PostToolUse stop answer ended the turn")
        .await;
}

/// Muse Code tells no subagent apart, so its stop answers never end a turn: every tool
/// result of the open turn is stopped until `Interrupt` (a stopped turn's end) or `Stop`
/// (a turn with no tool) closes it. The two stop events carry nothing else: no status, no
/// session and no take.
#[tokio::test]
async fn muse_code_stops_every_tool_result_of_an_open_turn_until_its_end_event() {
    let turns = Turns::start("muse-code").await;
    turns
        .hook_unstopped("UserPromptSubmit", "{}", "a turn's start is never stopped")
        .await;
    turns
        .interrupt_answers("stop_requested", "a main hook started the turn")
        .await;
    for event in ["PostToolUse", "PostToolUseFailure", "PostToolUse"] {
        turns
            .hook_stopped(
                event,
                "{}",
                StopForm::Continue,
                "no stop answer ends a turn of a CLI that tells no subagent apart",
            )
            .await;
    }
    turns
        .hook_unstopped("Interrupt", "{}", "a turn's end is never stopped")
        .await;
    turns
        .interrupt_finds_no_open_turn("Interrupt ended the stopped turn")
        .await;
    turns
        .hook_unstopped("PostToolUse", "{}", "Interrupt ended the turn")
        .await;

    turns
        .hook_unstopped("UserPromptSubmit", "{}", "a turn's start is never stopped")
        .await;
    turns
        .hook_unstopped(
            "Stop",
            r#"{"stop_hook_active":false}"#,
            "a turn's end is never stopped",
        )
        .await;
    turns
        .interrupt_finds_no_open_turn("Stop ended a turn that called no tool")
        .await;

    turns.deposit("PARCEL-NOT-FOR-A-STOP-EVENT");
    let entry = turns.entry();
    let reported = entry.status_event_count();
    for event in ["PostToolUse", "PostToolUseFailure"] {
        turns
            .hook_unstopped(event, r#"{"session_id":"muse-session"}"#, "no turn is open")
            .await;
    }
    assert_eq!(entry.status_event_count(), reported, "no status");
    assert_eq!(entry.session_id(), None, "no session");
    assert_eq!(entry.inbox().pending_count(), 1, "nothing taken");
}

/// An `Unknown` window is never interrupted by an AI: the refusal puts no mark, so the
/// next main stop event gets its ordinary answer.
#[tokio::test]
async fn an_ais_interrupt_of_an_unknown_window_is_refused_and_marks_nothing() {
    let turns = Turns::start_with("grok-build", common::wake_none_delivery(), None).await;
    let mut client = turns.h.connect().await;
    assert_eq!(
        common::query_status(&mut client, &turns.window_id).await,
        WindowStatus::Unknown,
        "fixture precondition"
    );
    assert_eq!(
        turns.interrupt().await,
        Err("status_unknown".to_string()),
        "an Unknown window is refused before its profile's means is looked at"
    );
    turns
        .hook_unstopped("PreToolUse", "{}", "the refused interrupt put no mark")
        .await;
}

/// The user's own interrupt (the GUI's button, an approver's connection) writes the
/// user's key and puts no mark: the next stop event is not stopped.
#[tokio::test]
async fn an_approvers_interrupt_writes_the_users_key_and_puts_no_mark() {
    let turns = Turns::start("grok-build").await;
    turns
        .hook_unstopped("UserPromptSubmit", "{}", "a turn's start is never stopped")
        .await;
    let mut approver = turns.h.connect().await;
    common::register_approver(&mut approver).await;
    assert_eq!(
        interrupt_from(&mut approver, &turns.window_id).await,
        Ok("interrupted".to_string()),
        "the user's interrupt writes the user's key"
    );
    turns
        .hook_unstopped("PreToolUse", "{}", "the user's interrupt puts no mark")
        .await;
}

// --- the window's own requests end with the mark -----------------------------------------

/// A connection identified as `turns`' window, with a `wait_for_status` sent on it for that
/// window to exit, which nothing but the mark can end before its time runs out.
async fn own_wait(turns: &Turns) -> yagura_test_support::ipc::Connection {
    let mut own = turns.h.connect().await;
    common::identify(&mut own, &turns.window_id).await;
    start_wait_for_exit(&mut own, &turns.window_id).await;
    own
}

/// Sends a `wait_for_status` for `window_id` to exit. Frames on one connection are handled
/// in order: the pong proves the wait is registered.
async fn start_wait_for_exit(client: &mut yagura_test_support::ipc::Connection, window_id: &str) {
    common::send(
        client,
        &yagura_protocol::ClientMessage::WaitForStatus {
            window_id: window_id.to_string(),
            target_status: WindowStatus::Exited,
            timeout_sec: 300,
        },
    )
    .await;
    common::send(client, &yagura_protocol::ClientMessage::Ping).await;
    match common::read_response(client).await {
        yagura_protocol::DaemonMessage::Pong => {}
        other => panic!("expected pong, got {other:?}"),
    }
}

/// Asserts the next answer on `client` is the end of its `wait_for_status` by the mark:
/// neither reached nor timed out, with the status the window shows.
async fn expect_wait_ended_by_the_mark(
    client: &mut yagura_test_support::ipc::Connection,
    status: WindowStatus,
    why: &str,
) {
    match common::read_response(client).await {
        yagura_protocol::DaemonMessage::WaitResult {
            outcome,
            status: shown,
            ..
        } => {
            assert_eq!(
                outcome, "stop_requested",
                "{why}: the wait ends as ended by the mark, told apart from its time running out"
            );
            assert_eq!(
                shown, status,
                "{why}: the answer carries the current status"
            );
        }
        other => panic!("{why}: expected a wait_result, got {other:?}"),
    }
}

/// Lets every clock bound run out, then asserts the next answer on `client` is its
/// `wait_for_status` timing out: nothing else ended it.
async fn expect_wait_outlived_the_mark(
    client: &mut yagura_test_support::ipc::Connection,
    why: &str,
) {
    common::let_every_clock_bound_run_out().await;
    match common::read_response(client).await {
        yagura_protocol::DaemonMessage::WaitResult { outcome, .. } => {
            assert_eq!(outcome, "timeout", "{why}: only its time ended the wait");
        }
        other => panic!("{why}: expected a timed-out wait_result, got {other:?}"),
    }
}

/// The mark ends, at once, what the open turn's window is waiting for, and a request the
/// window starts while the mark still applies; another caller's wait on that window is not
/// the window's own and lives on.
#[tokio::test]
async fn the_mark_on_an_open_turn_ends_the_windows_own_waits_and_those_it_starts_after() {
    let turns = Turns::start("grok-build").await;
    turns
        .hook_unstopped("UserPromptSubmit", "{}", "a turn's start is never stopped")
        .await;
    let mut own = own_wait(&turns).await;
    let mut other = turns.h.connect().await;
    start_wait_for_exit(&mut other, &turns.window_id).await;
    // The hook's Running is confirmed through the detector's debounce, not when the hook returns.
    let mut client = turns.h.connect().await;
    wait_until_status(&mut client, &turns.window_id, WindowStatus::Running).await;
    let status = turns.entry().current_status();

    turns
        .interrupt_answers("stop_requested", "a main hook started the turn")
        .await;
    expect_wait_ended_by_the_mark(&mut own, status, "the window's own wait").await;
    // Ended at once, so its answer comes before any ping sent after it.
    common::send(
        &mut own,
        &yagura_protocol::ClientMessage::WaitForStatus {
            window_id: turns.window_id.clone(),
            target_status: WindowStatus::Exited,
            timeout_sec: 300,
        },
    )
    .await;
    expect_wait_ended_by_the_mark(
        &mut own,
        turns.entry().current_status(),
        "a wait the marked turn started",
    )
    .await;
    expect_wait_outlived_the_mark(&mut other, "another caller's wait on the window").await;
}

/// Waiting outside a turn is the window's own business: an interrupt that finds no open
/// turn ends nothing, now or later.
#[tokio::test]
async fn an_interrupt_finding_no_open_turn_ends_none_of_the_windows_waits() {
    let turns = Turns::start("grok-build").await;
    turns
        .hook_unstopped("UserPromptSubmit", "{}", "a turn's start is never stopped")
        .await;
    turns
        .hook_unstopped("Stop", GROK_FIRST_STOP, "a turn's end is never stopped")
        .await;
    let mut own = own_wait(&turns).await;
    turns
        .interrupt_finds_no_open_turn("the last main hook ended the turn")
        .await;
    let mut later = turns.h.connect().await;
    common::identify(&mut later, &turns.window_id).await;
    start_wait_for_exit(&mut later, &turns.window_id).await;
    expect_wait_outlived_the_mark(&mut own, "a wait from before the interrupt").await;
    expect_wait_outlived_the_mark(&mut later, "a wait started after it").await;
}

/// A window no main hook has reached may have no door, so the mark ends what it is
/// waiting for at once but not what it starts later: those would end at once for as long
/// as the window lives.
#[tokio::test]
async fn an_unconfirmed_mark_ends_the_windows_waits_but_not_those_it_starts_after() {
    let turns = Turns::start("grok-build").await;
    let mut own = own_wait(&turns).await;
    let status = turns.entry().current_status();
    turns
        .interrupt_answers(
            "stop_requested_unconfirmed",
            "no main hook has reached the daemon",
        )
        .await;
    expect_wait_ended_by_the_mark(&mut own, status, "the window's own wait").await;
    start_wait_for_exit(&mut own, &turns.window_id).await;
    expect_wait_outlived_the_mark(&mut own, "a wait the unconfirmed window started after").await;
}

// --- a wake starts a turn where the daemon hands its answer ------------------------------

/// Deposits a parcel into `inbox` once a waiting door has registered there.
async fn deposit_once_waited(inbox: &yagura_daemon::inbox::Inbox, key: &str, text: &str) {
    let deadline = tokio::time::Instant::now() + yagura_test_support::HANG_GUARD;
    while !inbox.has_waiter() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the waiting door registers"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    inbox.deposit(Parcel::new(key, text));
}

/// In a window of a CLI the daemon wakes through a waiting door, handing that door its
/// parcel starts a turn, before the door says it printed. A waiting door's read that ends
/// without saying so puts its parcel back, and the turn it started is closed again.
#[tokio::test]
async fn a_wake_answer_starts_a_turn_before_its_notice_and_one_never_printed_closes_it() {
    let delivery = yagura_daemon::profile_delivery::ProfileDelivery {
        wake: yagura_daemon::profile_delivery::WakeMode::Wait,
        ..common::wake_none_delivery()
    };
    let turns = Turns::start_with("claude-code", delivery, Some(TURN_DETECTION)).await;
    let inbox = turns.entry().inbox().clone();

    turns
        .hook_unstopped("UserPromptSubmit", "{}", "a turn's start is never stopped")
        .await;
    turns
        .hook_unstopped("Stop", "{}", "a turn's end is never stopped")
        .await;
    turns
        .interrupt_finds_no_open_turn("the last main hook ended the turn")
        .await;
    let (relayed, ()) = tokio::join!(
        relay_door(
            &turns.h,
            &turns.window_id,
            &["--deliver-wait"],
            Vec::new(),
            Printed::Withhold,
        ),
        deposit_once_waited(&inbox, "wake", "PARCEL-THAT-WAKES")
    );
    assert!(
        String::from_utf8_lossy(&relayed.output.stderr).contains("PARCEL-THAT-WAKES"),
        "fixture precondition: the waiting door printed the parcel; {}",
        describe(&relayed.output)
    );
    turns
        .interrupt_answers(
            "stop_requested",
            "handing a waiting door its parcel started a turn, before the door's notice",
        )
        .await;

    drop(relayed);
    let deadline = tokio::time::Instant::now() + yagura_test_support::HANG_GUARD;
    while !inbox.holds("wake") {
        assert!(
            tokio::time::Instant::now() < deadline,
            "fixture precondition: a parcel no notice settled comes back"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    turns
        .interrupt_finds_no_open_turn("the parcel that started the turn came back unprinted")
        .await;
}
