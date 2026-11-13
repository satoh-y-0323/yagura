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
use yagura_daemon::profile::parse_profile;
use yagura_daemon::window::{HooksRoute, LaunchIdentity, WindowProvenance};
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
         session = \"/session_id\", skip_if = \"/subagentType\" }}\n",
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
        config.profiles_dir = Some(profiles_dir);
        config.bundled_profiles = shipped_profiles;
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

#[tokio::test]
async fn a_hook_whose_input_has_the_skip_pointer_does_nothing() {
    let setup = Setup::start().await;
    setup.deposit("PARCEL-FOR-THE-MAIN-SESSION");
    let input = br#"{"subagentType":"explore","session_id":"sub-1"}"#.to_vec();

    let (output, written) = setup.run(&hook_args(), input).await;

    assert!(written.is_ok(), "{written:?}");
    assert!(
        output.status.code() == Some(0) && output.stdout.is_empty(),
        "a hook whose input has a value at the skip pointer must print nothing and exit 0; {}",
        describe(&output)
    );
    assert_eq!(
        setup.entry().inbox().pending_count(),
        1,
        "a skipped hook must leave the parcel in the inbox"
    );
    assert_eq!(
        setup.status().await,
        WindowStatus::Idle,
        "a skipped hook must not record a status"
    );
    assert_eq!(
        setup.entry().session_id(),
        None,
        "a skipped hook must not record a session"
    );
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
