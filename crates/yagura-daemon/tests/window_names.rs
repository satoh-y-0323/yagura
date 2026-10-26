#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: a window's name is the key a chat mention addresses it by, so the
//! daemon keeps the names of live windows (every window that has not exited, and every
//! window still being opened) from colliding.
//!
//! A name the caller supplied is refused when it collides (`name_taken`, pointing at
//! `name`); a default name (a profile's display name, a command's stem, a task's title)
//! is numbered instead (`<name> 2`, `<name> 3`, the smallest free number). Collision is
//! judged by the mention rule: case and `-` / `_` are not told apart, and `user`,
//! `parent` and `all` are always taken.
//!
//! The open requests are written as raw JSON frames so this file names only the wire
//! keys, and an answer's `name` is read from its wire form.

mod common;

use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;

use common::{
    CONTROL_DETECTION_NAME, ChatPlaybook, Harness, call_list_windows, chat_config, empty_lookup,
    expect_error, identify, launch_prompt_profile_source, read_response, register_approver, send,
    send_handoff_from_profile, unique_temp_dir, wait_until_status, write_profile,
};
use yagura_daemon::DaemonConfig;
use yagura_daemon::model_choices::parse_choices;
use yagura_daemon::profile::{OutputFormat, ProfileOs, ProfileParseError, parse_profile};
use yagura_protocol::{
    ClientMessage, DaemonMessage, PlanTaskSpec, WINDOW_NAME_MAX_BYTES, WindowInfo, WindowNameError,
    WindowStatus,
};
use yagura_test_support::detection::CONTROL_READY_MARKER;
use yagura_test_support::ipc::Connection;
use yagura_test_support::process::{
    echo_child_command, exit_with_code_command, ready_child_command,
};

/// The wire key of an open request saying what to do when its name is taken.
const ON_NAME_TAKEN: &str = "on_name_taken";

/// The profile every case opens its windows from.
const TWIN: &str = "twin";
const TWIN_NAME: &str = "Twin";

const REPLY_TIMEOUT: Duration = Duration::from_secs(20);

const AUTO_APPROVE: &[&str] = &["send_chat", "open_window", "close_window", "send_handoff"];

fn is_file(path: &Path) -> bool {
    path.is_file()
}

/// A chat- and ledger-ready config with a profiles directory under `dir`.
fn names_config(dir: &Path, auto_approve: &[&str]) -> DaemonConfig {
    let profiles_dir = profiles_dir(dir);
    let mut config = chat_config(
        dir,
        &ChatPlaybook {
            auto_approve,
            ..ChatPlaybook::default()
        },
    );
    config.profiles_dir = Some(profiles_dir);
    config.host_os = ProfileOs::host();
    config.program_lookup = empty_lookup();
    config.program_is_executable = is_file;
    config.plan_approval_required = false;
    config
}

async fn start(tag: &str, dir: &Path) -> Harness {
    start_with(tag, names_config(dir, AUTO_APPROVE)).await
}

async fn start_with(tag: &str, config: DaemonConfig) -> Harness {
    let (h, _locks) = Harness::start_with_chat_and_ledger(tag, config).await;
    h
}

fn toml_basic(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

/// A `ready_child` profile that takes part in chat: it declares delivery and runs the
/// control definition.
fn participant_profile_source(display_name: &str) -> String {
    let command = ready_child_command(CONTROL_READY_MARKER)
        .iter()
        .map(|arg| format!("'{arg}'"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "schema = 1\ndisplay_name = \"{}\"\ndetection = \"{CONTROL_DETECTION_NAME}\"\n\n\
         [command]\ndefault = [{command}]\n\n[delivery]\nwake = \"none\"\n",
        toml_basic(display_name)
    )
}

fn profiles_dir(dir: &Path) -> std::path::PathBuf {
    let profiles_dir = dir.join("profiles");
    std::fs::create_dir_all(&profiles_dir).expect("create the profiles directory");
    profiles_dir
}

fn write_participant_profile(dir: &Path, id: &str, display_name: &str) {
    write_profile(
        &profiles_dir(dir),
        id,
        &participant_profile_source(display_name),
    );
}

async fn send_json(client: &mut Connection, request: &Value) {
    let body = serde_json::to_vec(request).expect("a request encodes");
    let frame = yagura_protocol::frame_encode(&body);
    client
        .write_all(&frame)
        .await
        .expect("write_all frame to the endpoint");
}

/// The next frame that answers a request, skipping the pushes a connection may be sent.
async fn read_reply(client: &mut Connection) -> DaemonMessage {
    loop {
        match read_response(client, REPLY_TIMEOUT).await {
            DaemonMessage::ChatChanged { .. } | DaemonMessage::PlanChanged { .. } => continue,
            other => return other,
        }
    }
}

fn with_name(mut request: Value, name: Option<&str>, on_name_taken: Option<&str>) -> Value {
    if let Some(name) = name {
        request["name"] = json!(name);
    }
    if let Some(policy) = on_name_taken {
        request[ON_NAME_TAKEN] = json!(policy);
    }
    request
}

async fn open_profile(
    client: &mut Connection,
    profile: &str,
    cwd: &Path,
    name: Option<&str>,
    on_name_taken: Option<&str>,
) -> DaemonMessage {
    let request = json!({
        "type": "open_window_from_profile",
        "profile": profile,
        "cwd": cwd.display().to_string(),
    });
    send_json(client, &with_name(request, name, on_name_taken)).await;
    read_reply(client).await
}

async fn open_command(
    client: &mut Connection,
    command: Vec<String>,
    cwd: &Path,
    name: Option<&str>,
    on_name_taken: Option<&str>,
) -> DaemonMessage {
    let request = json!({
        "type": "open_window",
        "command": command,
        "cwd": cwd.display().to_string(),
    });
    send_json(client, &with_name(request, name, on_name_taken)).await;
    read_reply(client).await
}

fn expect_opened(msg: DaemonMessage) -> String {
    match msg {
        DaemonMessage::WindowOpened { window_id, .. } => window_id,
        other => panic!("expected window_opened, got {other:?}"),
    }
}

/// The `name` an answer carries on the wire.
fn reported_name(msg: &DaemonMessage) -> Option<String> {
    serde_json::to_value(msg)
        .expect("a daemon message encodes")
        .get("name")
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn expect_name_taken(msg: DaemonMessage) -> String {
    match msg {
        DaemonMessage::Error {
            code,
            message,
            rejected_argument,
        } => {
            assert_eq!(
                code, "name_taken",
                "unexpected error code (message: {message})"
            );
            assert_eq!(rejected_argument.as_deref(), Some("name"));
            message
        }
        other => panic!("expected error(name_taken), got {other:?}"),
    }
}

fn find<'a>(windows: &'a [WindowInfo], window_id: &str) -> &'a WindowInfo {
    windows
        .iter()
        .find(|w| w.window_id == window_id)
        .unwrap_or_else(|| panic!("{window_id} is not listed in {windows:?}"))
}

async fn name_of(client: &mut Connection, window_id: &str) -> String {
    find(&call_list_windows(client).await, window_id)
        .name
        .clone()
}

async fn window_count(client: &mut Connection) -> usize {
    call_list_windows(client).await.len()
}

async fn rename(client: &mut Connection, window_id: &str, name: &str) -> DaemonMessage {
    send(
        client,
        &ClientMessage::RenameWindow {
            window_id: window_id.to_string(),
            name: name.to_string(),
        },
    )
    .await;
    read_reply(client).await
}

fn expect_renamed(msg: DaemonMessage) -> String {
    match msg {
        DaemonMessage::WindowRenamed { name, .. } => name,
        other => panic!("expected window_renamed, got {other:?}"),
    }
}

async fn close_and_wait_exited(client: &mut Connection, window_id: &str) {
    send(
        client,
        &ClientMessage::CloseWindow {
            window_id: window_id.to_string(),
            force: Some(true),
        },
    )
    .await;
    match read_reply(client).await {
        DaemonMessage::WindowClosed { window_id: closed } => assert_eq!(closed, window_id),
        other => panic!("expected window_closed, got {other:?}"),
    }
    wait_until_status(client, window_id, WindowStatus::Exited, REPLY_TIMEOUT).await;
}

/// Opens a window whose child exits at once, and returns once it is `Exited`.
async fn open_exited(client: &mut Connection, cwd: &Path, name: &str) -> String {
    let id = expect_opened(
        open_command(client, exit_with_code_command("0"), cwd, Some(name), None).await,
    );
    wait_until_status(client, &id, WindowStatus::Exited, REPLY_TIMEOUT).await;
    id
}

// --- default names are numbered --------------------------------------------------

#[tokio::test]
async fn windows_opened_from_one_profile_are_numbered_and_an_exited_number_is_reused() {
    let dir = unique_temp_dir("names-numbered");
    write_participant_profile(&dir, TWIN, TWIN_NAME);
    let h = start("names-numbered", &dir).await;
    let mut client = h.connect().await;

    let first = expect_opened(open_profile(&mut client, TWIN, &dir, None, None).await);
    let second = expect_opened(open_profile(&mut client, TWIN, &dir, None, None).await);
    let third = expect_opened(open_profile(&mut client, TWIN, &dir, None, None).await);
    let windows = call_list_windows(&mut client).await;
    assert_eq!(
        [&first, &second, &third].map(|id| find(&windows, id).name.clone()),
        ["Twin", "Twin 2", "Twin 3"].map(str::to_string)
    );

    close_and_wait_exited(&mut client, &second).await;
    let fourth = expect_opened(open_profile(&mut client, TWIN, &dir, None, None).await);
    assert_eq!(
        name_of(&mut client, &fourth).await,
        "Twin 2",
        "an exited window's name is free again, and the smallest free number is used"
    );

    h.registry.terminate_all().await;
}

#[tokio::test]
async fn a_profile_named_like_a_reserved_mention_is_numbered() {
    let dir = unique_temp_dir("names-reserved-default");
    write_participant_profile(&dir, "user-profile", "user");
    let h = start("names-reserved-default", &dir).await;
    let mut client = h.connect().await;

    let id = expect_opened(open_profile(&mut client, "user-profile", &dir, None, None).await);
    assert_eq!(name_of(&mut client, &id).await, "user 2");

    h.registry.terminate_all().await;
}

#[tokio::test]
async fn a_numbered_name_is_cut_at_a_character_boundary_to_stay_within_the_limit() {
    let dir = unique_temp_dir("names-numbered-limit");
    // 127 bytes: "a" and 42 three-byte characters.
    let long = format!("a{}", "\u{3042}".repeat(42));
    assert_eq!(long.len(), WINDOW_NAME_MAX_BYTES - 1);
    write_participant_profile(&dir, "long", &long);
    let h = start("names-numbered-limit", &dir).await;
    let mut client = h.connect().await;

    let first = expect_opened(open_profile(&mut client, "long", &dir, None, None).await);
    let second = expect_opened(open_profile(&mut client, "long", &dir, None, None).await);
    assert_eq!(name_of(&mut client, &first).await, long);
    let numbered = name_of(&mut client, &second).await;
    assert_eq!(
        numbered,
        format!("a{} 2", "\u{3042}".repeat(41)),
        "the longest prefix ending on a character boundary that leaves room for \" 2\""
    );
    assert!(numbered.len() <= WINDOW_NAME_MAX_BYTES);

    h.registry.terminate_all().await;
}

#[tokio::test]
async fn two_windows_opened_at_once_from_one_profile_get_different_names() {
    let dir = unique_temp_dir("names-concurrent");
    write_participant_profile(&dir, TWIN, TWIN_NAME);
    let h = start("names-concurrent", &dir).await;
    let mut a = h.connect().await;
    let mut b = h.connect().await;

    let (reply_a, reply_b) = tokio::join!(
        open_profile(&mut a, TWIN, &dir, None, None),
        open_profile(&mut b, TWIN, &dir, None, None),
    );
    let id_a = expect_opened(reply_a);
    let id_b = expect_opened(reply_b);
    let windows = call_list_windows(&mut a).await;
    let mut names = [
        find(&windows, &id_a).name.clone(),
        find(&windows, &id_b).name.clone(),
    ];
    names.sort();
    assert_eq!(names, ["Twin".to_string(), "Twin 2".to_string()]);

    h.registry.terminate_all().await;
}

// --- a supplied name is refused when taken ---------------------------------------

#[tokio::test]
async fn a_supplied_name_matching_a_live_window_by_the_mention_rule_is_refused() {
    let dir = unique_temp_dir("names-taken");
    write_participant_profile(&dir, TWIN, TWIN_NAME);
    let h = start("names-taken", &dir).await;
    let mut client = h.connect().await;
    expect_opened(
        open_command(
            &mut client,
            echo_child_command(),
            &dir,
            Some("Big-Bird"),
            None,
        )
        .await,
    );
    let before = window_count(&mut client).await;

    for taken in ["Big-Bird", "big-bird", "BIG_BIRD", "big_bird"] {
        let message = expect_name_taken(
            open_command(&mut client, echo_child_command(), &dir, Some(taken), None).await,
        );
        assert!(
            message.starts_with("a window named \u{201c}")
                && message.ends_with("\u{201d} is already open"),
            "unexpected wording: {message:?}"
        );
        expect_name_taken(open_profile(&mut client, TWIN, &dir, Some(taken), None).await);
    }
    assert_eq!(
        window_count(&mut client).await,
        before,
        "no window was opened"
    );

    h.registry.terminate_all().await;
}

#[tokio::test]
async fn a_supplied_reserved_mention_name_is_refused() {
    let dir = unique_temp_dir("names-reserved");
    let h = start("names-reserved", &dir).await;
    let mut client = h.connect().await;

    for reserved in ["User", "parent", "ALL"] {
        let message = expect_name_taken(
            open_command(
                &mut client,
                echo_child_command(),
                &dir,
                Some(reserved),
                None,
            )
            .await,
        );
        assert!(
            message.ends_with("\u{201d} is reserved for chat mentions"),
            "unexpected wording: {message:?}"
        );
    }
    assert_eq!(window_count(&mut client).await, 0, "no window was opened");

    h.registry.terminate_all().await;
}

#[tokio::test]
async fn a_supplied_name_is_numbered_when_the_request_asks_for_it() {
    let dir = unique_temp_dir("names-number-policy");
    write_participant_profile(&dir, TWIN, TWIN_NAME);
    let h = start("names-number-policy", &dir).await;
    let mut client = h.connect().await;
    expect_opened(open_command(&mut client, echo_child_command(), &dir, Some("Saved"), None).await);

    let by_command = expect_opened(
        open_command(
            &mut client,
            echo_child_command(),
            &dir,
            Some("Saved"),
            Some("number"),
        )
        .await,
    );
    let by_profile =
        expect_opened(open_profile(&mut client, TWIN, &dir, Some("saved"), Some("number")).await);
    assert_eq!(name_of(&mut client, &by_command).await, "Saved 2");
    assert_eq!(name_of(&mut client, &by_profile).await, "saved 3");

    let refused = open_command(
        &mut client,
        echo_child_command(),
        &dir,
        Some("Saved"),
        Some("refuse"),
    )
    .await;
    expect_name_taken(refused);

    h.registry.terminate_all().await;
}

#[tokio::test]
async fn a_taken_name_is_refused_before_the_approver_is_asked() {
    let dir = unique_temp_dir("names-before-approval");
    let h = start_with("names-before-approval", names_config(&dir, &["send_chat"])).await;
    let existing = h.open(echo_child_command(), None).await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;
    let mut requester = h.connect().await;
    let taken = name_of(&mut requester, &existing).await;

    let request = with_name(
        json!({
            "type": "open_window",
            "command": echo_child_command(),
            "cwd": dir.display().to_string(),
        }),
        Some(&taken),
        None,
    );
    send_json(&mut requester, &request).await;
    tokio::select! {
        reply = read_reply(&mut requester) => {
            expect_name_taken(reply);
        }
        request = next_approval_request(&mut approver) => {
            panic!("the approver was asked about an open whose name is taken: {request:?}");
        }
    }
    let mut late_approver = h.connect().await;
    assert!(
        register_approver(&mut late_approver).await.is_empty(),
        "no approval request was left pending"
    );

    h.registry.terminate_all().await;
}

async fn next_approval_request(approver: &mut Connection) -> yagura_protocol::ApprovalRequest {
    loop {
        if let DaemonMessage::ApprovalRequested { request } =
            read_response(approver, REPLY_TIMEOUT).await
        {
            return request;
        }
    }
}

// --- rename ----------------------------------------------------------------------

#[tokio::test]
async fn rename_is_refused_onto_a_live_name_and_allowed_onto_its_own_or_an_exited_name() {
    let dir = unique_temp_dir("names-rename");
    let h = start("names-rename", &dir).await;
    let mut client = h.connect().await;
    let alpha = expect_opened(
        open_command(&mut client, echo_child_command(), &dir, Some("Alpha"), None).await,
    );
    let beta = expect_opened(
        open_command(&mut client, echo_child_command(), &dir, Some("Beta"), None).await,
    );
    let gone = open_exited(&mut client, &dir, "Gone").await;

    expect_name_taken(rename(&mut client, &beta, "alpha").await);
    assert_eq!(
        name_of(&mut client, &beta).await,
        "Beta",
        "a refused rename keeps the name"
    );

    assert_eq!(
        expect_renamed(rename(&mut client, &alpha, "Alpha").await),
        "Alpha"
    );
    assert_eq!(
        expect_renamed(rename(&mut client, &beta, "gone").await),
        "gone"
    );

    expect_name_taken(rename(&mut client, &gone, "Alpha").await);
    assert_eq!(name_of(&mut client, &gone).await, "Gone");

    h.registry.terminate_all().await;
}

#[tokio::test]
async fn rename_onto_a_reserved_mention_name_is_refused() {
    let dir = unique_temp_dir("names-rename-reserved");
    let h = start("names-rename-reserved", &dir).await;
    let mut client = h.connect().await;
    let id = expect_opened(
        open_command(&mut client, echo_child_command(), &dir, Some("Alpha"), None).await,
    );

    for reserved in ["user", "Parent", "ALL"] {
        expect_name_taken(rename(&mut client, &id, reserved).await);
    }
    assert_eq!(name_of(&mut client, &id).await, "Alpha");

    h.registry.terminate_all().await;
}

// --- `"` is not part of a name -----------------------------------------------------

#[tokio::test]
async fn a_name_with_a_double_quote_is_refused_on_open_and_rename() {
    let dir = unique_temp_dir("names-quote");
    let h = start("names-quote", &dir).await;
    let mut client = h.connect().await;
    let id = expect_opened(
        open_command(&mut client, echo_child_command(), &dir, Some("Alpha"), None).await,
    );
    let before = window_count(&mut client).await;

    expect_error(
        open_command(
            &mut client,
            echo_child_command(),
            &dir,
            Some("Say \"hi\""),
            None,
        )
        .await,
        "invalid_argument",
    );
    assert_eq!(window_count(&mut client).await, before);

    expect_error(
        rename(&mut client, &id, "Say \"hi\"").await,
        "invalid_argument",
    );
    assert_eq!(name_of(&mut client, &id).await, "Alpha");

    h.registry.terminate_all().await;
}

#[test]
fn a_profile_display_name_with_a_double_quote_is_refused() {
    let result = parse_profile("quoted", &participant_profile_source("Say \"hi\""));
    assert!(
        matches!(
            result,
            Err(ProfileParseError::InvalidDisplayName(
                WindowNameError::ForbiddenChar { ch: '"', .. }
            ))
        ),
        "got {result:?}"
    );
}

#[tokio::test]
async fn a_plan_task_title_with_a_double_quote_is_refused() {
    let dir = unique_temp_dir("names-quote-plan");
    write_participant_profile(&dir, TWIN, TWIN_NAME);
    let h = start("names-quote-plan", &dir).await;
    let owner_window = h.open(echo_child_command(), None).await;
    let mut owner = h.connect().await;
    identify(&mut owner, &owner_window).await;

    send(
        &mut owner,
        &ClientMessage::CreatePlan {
            title: "plan".to_string(),
            tasks: vec![task_titled("t1", "Say \"hi\"", "t1.txt")],
            ticket: None,
        },
    )
    .await;
    expect_error(read_reply(&mut owner).await, "invalid_argument");

    h.registry.terminate_all().await;
}

/// The `[host_paths]` and `[mcp_registration]` tables of a row inside another
/// environment, whose `group` heading is `{group}`.
fn foreign_registration_table(group: &str) -> String {
    format!(
        "[host_paths]\nconvert = [\"wsl\", \"-e\", \"wslpath\", \"-u\", \"{{path}}\"]\n\n\
         [mcp_registration]\ngroup = \"{}\"\n\
         add = [\"mcp\", \"add\", \"{{server}}\", \"{{env_args}}\", \"--\", \"{{bridge}}\"]\n\
         remove = [\"mcp\", \"remove\", \"{{server}}\"]\n\
         env_arg = [\"--env\", \"{{env}}\"]\n\n\
         [mcp_registration.status]\n\
         args = [\"mcp\", \"get\", \"{{server}}\", \"--json\"]\n\
         format = \"json\"\ncommand = \"/transport/command\"\n",
        toml_basic(group)
    )
}

#[test]
fn a_registration_group_and_a_model_label_keep_accepting_a_double_quote() {
    let source = format!(
        "schema = 1\ndisplay_name = \"Neutral Cli\"\n\n[command]\ndefault = [\"neutral-cli\"]\n\n{}",
        foreign_registration_table("WSL \"main\"")
    );
    let profile = parse_profile("quoted-group", &source)
        .unwrap_or_else(|err| panic!("a group heading may hold a double quote: {err:?}"));
    let spec = profile
        .mcp_registration
        .expect("the profile carries a registration table");
    assert_eq!(spec.group(), Some("WSL \"main\""));

    let choices = parse_choices(
        &OutputFormat::Lines {
            separator: "\t".to_string(),
        },
        b"tier-a\tTier \"A\"\n",
    )
    .expect("one well-formed line");
    assert_eq!(choices.len(), 1);
    assert_eq!(choices[0].label.as_deref(), Some("Tier \"A\""));
}

// --- answers carry the name ------------------------------------------------------

#[tokio::test]
async fn window_opened_carries_the_name_the_window_was_given() {
    let dir = unique_temp_dir("names-opened-answer");
    write_participant_profile(&dir, TWIN, TWIN_NAME);
    let h = start("names-opened-answer", &dir).await;
    let mut client = h.connect().await;

    for _ in 0..2 {
        let reply = open_profile(&mut client, TWIN, &dir, None, None).await;
        let named = reported_name(&reply);
        let id = expect_opened(reply);
        assert_eq!(named, Some(name_of(&mut client, &id).await));
    }
    let reply = open_command(&mut client, echo_child_command(), &dir, Some("Given"), None).await;
    assert_eq!(reported_name(&reply).as_deref(), Some("Given"));

    h.registry.terminate_all().await;
}

fn task_titled(task_id: &str, title: &str, writes: &str) -> PlanTaskSpec {
    PlanTaskSpec {
        task_id: task_id.to_string(),
        title: title.to_string(),
        role: None,
        profile: Some(TWIN.to_string()),
        model: None,
        effort: None,
        depends_on: Vec::new(),
        writes: vec![writes.to_string()],
        instruction: None,
    }
}

async fn open_task(
    client: &mut Connection,
    plan_id: &str,
    task_id: &str,
    name: Option<&str>,
) -> DaemonMessage {
    send(
        client,
        &ClientMessage::OpenTaskWindow {
            plan_id: plan_id.to_string(),
            task_id: task_id.to_string(),
            name: name.map(str::to_string),
        },
    )
    .await;
    read_reply(client).await
}

fn expect_task_window(msg: DaemonMessage) -> String {
    match msg {
        DaemonMessage::TaskWindowOpened { window_id, .. } => window_id,
        other => panic!("expected task_window_opened, got {other:?}"),
    }
}

#[tokio::test]
async fn task_windows_number_a_shared_title_and_refuse_a_taken_supplied_name() {
    let dir = unique_temp_dir("names-task");
    write_participant_profile(&dir, TWIN, TWIN_NAME);
    let h = start("names-task", &dir).await;
    let owner_window = h.open(echo_child_command(), None).await;
    let mut owner = h.connect().await;
    identify(&mut owner, &owner_window).await;

    send(
        &mut owner,
        &ClientMessage::CreatePlan {
            title: "plan".to_string(),
            tasks: vec![
                task_titled("t1", "Review", "t1.txt"),
                task_titled("t2", "Review", "t2.txt"),
                task_titled("t3", "Other", "t3.txt"),
            ],
            ticket: None,
        },
    )
    .await;
    let plan_id = match read_reply(&mut owner).await {
        DaemonMessage::PlanDetail { plan } => plan.plan_id,
        other => panic!("expected plan_detail, got {other:?}"),
    };

    let mut names = Vec::new();
    for task_id in ["t1", "t2"] {
        let reply = open_task(&mut owner, &plan_id, task_id, None).await;
        let named = reported_name(&reply);
        let id = expect_task_window(reply);
        let listed = name_of(&mut owner, &id).await;
        assert_eq!(named.as_deref(), Some(listed.as_str()));
        names.push(listed);
    }
    assert_eq!(names, ["Review", "Review 2"]);

    expect_name_taken(open_task(&mut owner, &plan_id, "t3", Some("review")).await);

    h.registry.terminate_all().await;
}

#[tokio::test]
async fn a_handoff_that_opens_a_window_numbers_its_default_name_and_refuses_a_taken_one() {
    let dir = unique_temp_dir("names-handoff");
    write_profile(
        &profiles_dir(&dir),
        "ready",
        &launch_prompt_profile_source("Ready", None, ""),
    );
    let h = start("names-handoff", &dir).await;
    let mut client = h.connect().await;
    expect_opened(open_profile(&mut client, "ready", &dir, None, None).await);

    send_handoff_from_profile(&mut client, "ready", None, None, "src/lib.rs", "look").await;
    let reply = read_reply(&mut client).await;
    let named = reported_name(&reply);
    let id = match reply {
        DaemonMessage::HandoffSent { window_id, .. } => window_id,
        other => panic!("expected handoff_sent, got {other:?}"),
    };
    assert_eq!(name_of(&mut client, &id).await, "Ready 2");
    assert_eq!(named.as_deref(), Some("Ready 2"));

    send_handoff_from_profile(
        &mut client,
        "ready",
        None,
        Some("ready"),
        "src/lib.rs",
        "look",
    )
    .await;
    expect_name_taken(read_reply(&mut client).await);

    h.registry.terminate_all().await;
}

// --- a mention reaches the window it names ---------------------------------------

#[tokio::test]
async fn a_mention_of_the_second_window_opened_from_one_profile_reaches_only_it() {
    let dir = unique_temp_dir("names-mention");
    write_participant_profile(&dir, TWIN, TWIN_NAME);
    let h = start("names-mention", &dir).await;
    let mut client = h.connect().await;
    let first = expect_opened(open_profile(&mut client, TWIN, &dir, None, None).await);
    let second = expect_opened(open_profile(&mut client, TWIN, &dir, None, None).await);
    let second_name = name_of(&mut client, &second).await;

    let poster_window = h
        .open_delivering_in(
            echo_child_command(),
            Some(CONTROL_DETECTION_NAME),
            Some(&dir),
        )
        .await;
    let mut poster = h.connect().await;
    identify(&mut poster, &poster_window).await;
    send(
        &mut poster,
        &ClientMessage::SendChat {
            room_id: None,
            body: format!("please look @\"{second_name}\""),
        },
    )
    .await;
    match read_reply(&mut poster).await {
        DaemonMessage::ChatPosted {
            targets,
            unresolved,
            ..
        } => {
            assert_eq!(
                unresolved,
                Vec::<String>::new(),
                "the second window's name must name exactly one window"
            );
            assert_eq!(targets, vec![second.clone()]);
            assert!(!targets.contains(&first));
        }
        other => panic!("expected chat_posted, got {other:?}"),
    }

    h.registry.terminate_all().await;
}
