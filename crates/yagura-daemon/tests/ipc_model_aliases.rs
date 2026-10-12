#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: the daemon IPC surface for a profile's model choices --
//! `list_model_choices` and `save_model_aliases`.
//!
//! Every listing command spawns the `capture_child` fixture binary (see
//! `src/bin/capture_child.rs`), never a real vendor CLI or a shell.

mod common;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use common::{
    Harness, approver_base_config, expect_error, read_response, register_approver, send,
    unique_temp_dir, write_profile,
};
use tracing_test::traced_test;
use yagura_daemon::DaemonConfig;
use yagura_daemon::child_env::ChildBaseEnv;
use yagura_os::program::ProgramLookup;
use yagura_protocol::{
    ClientMessage, DaemonMessage, LaunchOptionAlias, ModelChoice, ModelChoiceSource, ProfileInfo,
    validate_launch_option_value,
};
use yagura_test_support::ipc::Connection;

// --- the capture_child fixture binary ---------------------------------------

/// Path to the `capture_child` fixture binary next to this test binary. See
/// `tests/one_shot_capture.rs` for the same resolution, duplicated here
/// because integration-test binaries cannot import each other's items.
fn capture_child_path() -> PathBuf {
    let mut dir =
        std::env::current_exe().expect("current_exe should resolve for a running test binary");
    dir.pop();
    if dir.file_name().and_then(|n| n.to_str()) == Some("deps") {
        dir.pop();
    }
    let name = if cfg!(windows) {
        "capture_child.exe"
    } else {
        "capture_child"
    };
    dir.push(name);
    assert!(
        dir.exists(),
        "the capture_child fixture binary is missing at {}: run `cargo build -p yagura-daemon \
         --bin capture_child` first",
        dir.display()
    );
    dir
}

// --- configuration ------------------------------------------------------------

/// A name no real executable will ever have, used where a listing command
/// must never actually be reached.
const NONEXISTENT_PROGRAM: &str = "yagura-model-choices-test-nonexistent-program";

fn base_config(profiles_dir: PathBuf, extra_bin_dirs: Vec<PathBuf>) -> DaemonConfig {
    DaemonConfig {
        profiles_dir: Some(profiles_dir),
        program_lookup: ProgramLookup {
            path_dirs: Vec::new(),
            extra_bin_dirs,
            windows_comspec: if cfg!(windows) {
                Some(PathBuf::from("cmd.exe"))
            } else {
                None
            },
        },
        model_choices_max_concurrent: 2,
        model_choices_timeout: Duration::from_secs(10),
        model_choices_max_output_bytes: 1 << 20,
        ..approver_base_config()
    }
}

// --- profile documents ----------------------------------------------------------

fn values_profile(display_name: &str, values: &[&str]) -> String {
    let list = values
        .iter()
        .map(|v| format!("{v:?}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "schema = 1\ndisplay_name = {display_name:?}\n\n[command]\ndefault = \
         [{NONEXISTENT_PROGRAM:?}]\n\n[model]\nargs = [\"--tier\", \"{{value}}\"]\n\n\
         [model.choices]\nvalues = [{list}]\n"
    )
}

fn no_choices_profile(display_name: &str) -> String {
    format!(
        "schema = 1\ndisplay_name = {display_name:?}\n\n[command]\ndefault = \
         [{NONEXISTENT_PROGRAM:?}]\n\n[model]\nargs = [\"--tier\", \"{{value}}\"]\n"
    )
}

fn unavailable_profile(display_name: &str) -> String {
    format!(
        "schema = 1\ndisplay_name = {display_name:?}\n\n[command]\ndefault = \
         [{NONEXISTENT_PROGRAM:?}]\n\n[model]\nargs = [\"--tier\", \"{{value}}\"]\n\n\
         [model.choices]\nargs = [\"ignored\"]\nformat = \"lines\"\nseparator = \"\\t\"\n"
    )
}

/// A profile whose `[model.choices]` runs `capture_child` (found at
/// `command_path`) via `choices_args`, read as `format_body` declares.
fn command_profile(
    display_name: &str,
    command_path: &Path,
    choices_args: &[&str],
    format_body: &str,
) -> String {
    let args_list = choices_args
        .iter()
        .map(|a| format!("{a:?}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "schema = 1\ndisplay_name = {display_name:?}\n\n[command]\ndefault = ['{}']\n\n\
         [model]\nargs = [\"--tier\", \"{{value}}\"]\n\n[model.choices]\nargs = \
         [{args_list}]\n{format_body}\n",
        command_path.display(),
    )
}

/// Same shape as [`command_profile`], naming the executable by a bare name
/// resolved through `DaemonConfig::program_lookup` instead of an absolute
/// path.
#[cfg(windows)]
fn command_profile_by_name(
    display_name: &str,
    program_name: &str,
    choices_args: &[&str],
    format_body: &str,
) -> String {
    let args_list = choices_args
        .iter()
        .map(|a| format!("{a:?}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "schema = 1\ndisplay_name = {display_name:?}\n\n[command]\ndefault = \
         [{program_name:?}]\n\n[model]\nargs = [\"--tier\", \"{{value}}\"]\n\n\
         [model.choices]\nargs = [{args_list}]\n{format_body}\n"
    )
}

/// A profile whose `[model.choices]` command writes `marker_path` at
/// startup, before exiting -- proof that a listing command actually started.
fn touch_profile(display_name: &str, command_path: &Path, marker_path: &Path) -> String {
    format!(
        "schema = 1\ndisplay_name = {display_name:?}\n\n[command]\ndefault = ['{}']\n\n\
         [model]\nargs = [\"--tier\", \"{{value}}\"]\n\n[model.choices]\nargs = [\"touch\", \
         '{}', \"exit\", \"0\"]\nformat = \"lines\"\nseparator = \"\\t\"\n",
        command_path.display(),
        marker_path.display(),
    )
}

fn alias_profile(display_name: &str, aliases: &[(&str, &str)]) -> String {
    let alias_lines: String = aliases
        .iter()
        .map(|(alias, value)| format!("{alias} = {value:?}\n"))
        .collect();
    format!(
        "schema = 1\ndisplay_name = {display_name:?}\n\n[command]\ndefault = \
         [{NONEXISTENT_PROGRAM:?}]\n\n[model]\nargs = [\"--tier\", \"{{value}}\"]\n\n\
         [model.aliases]\n{alias_lines}"
    )
}

// --- fixture / harness helpers -------------------------------------------------

fn fresh_dirs(tag: &str) -> (PathBuf, PathBuf) {
    let root = unique_temp_dir(tag);
    let profiles_dir = root.join("profiles");
    std::fs::create_dir_all(&profiles_dir).expect("create profiles dir");
    (root, profiles_dir)
}

async fn approver_connection(h: &Harness) -> Connection {
    let mut client = h.connect().await;
    register_approver(&mut client).await;
    client
}

async fn wait_until_marker_appears(marker: &Path, timeout: Duration) {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if marker.exists() {
            return;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!(
                "timed out waiting for the marker file to appear at {}",
                marker.display()
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn assert_marker_absent_after(marker: &Path, observation: Duration) {
    tokio::time::sleep(observation).await;
    assert!(
        !marker.exists(),
        "no listing command must have started: marker file exists at {}",
        marker.display()
    );
}

/// How long a case that must observe "nothing happened" waits before
/// checking. Long enough for a listing command's spawn (if one wrongly
/// happened) to have already written its marker file.
const NEGATIVE_OBSERVATION: Duration = Duration::from_millis(500);

fn expect_requested(msg: DaemonMessage) -> (u64, String) {
    match msg {
        DaemonMessage::ModelChoicesRequested {
            request_id,
            profile,
        } => (request_id, profile),
        other => panic!("expected model_choices_requested, got {other:?}"),
    }
}

#[allow(clippy::type_complexity)]
fn expect_choices(
    msg: DaemonMessage,
) -> (
    u64,
    String,
    Option<ModelChoiceSource>,
    Vec<ModelChoice>,
    Option<String>,
) {
    match msg {
        DaemonMessage::ModelChoices {
            request_id,
            profile,
            source,
            choices,
            error,
        } => (request_id, profile, source, choices, error),
        other => panic!("expected model_choices, got {other:?}"),
    }
}

fn expect_aliases_saved(msg: DaemonMessage) -> (String, Vec<LaunchOptionAlias>) {
    match msg {
        DaemonMessage::ModelAliasesSaved { profile, aliases } => (profile, aliases),
        other => panic!("expected model_aliases_saved, got {other:?}"),
    }
}

async fn list_profiles(client: &mut Connection) -> Vec<ProfileInfo> {
    send(client, &ClientMessage::ListProfiles).await;
    match read_response(client, Duration::from_secs(5)).await {
        DaemonMessage::ProfileList { profiles, .. } => profiles,
        other => panic!("expected profile_list, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// A non-approver connection is refused on every one of the three
// requests, and no listing command starts.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn list_model_choices_from_a_non_approver_connection_is_refused_and_starts_no_child() {
    let (root, profiles_dir) = fresh_dirs("model-i1-list");
    let marker = root.join("marker.txt");
    write_profile(
        &profiles_dir,
        "listing",
        &touch_profile("Listing", &capture_child_path(), &marker),
    );
    let f = Harness::start("model-i1-list", base_config(profiles_dir, Vec::new())).await;
    let mut external = f.connect().await;

    send(
        &mut external,
        &ClientMessage::ListModelChoices {
            profile: "listing".to_string(),
            request_id: 1,
        },
    )
    .await;
    let msg = read_response(&mut external, Duration::from_secs(5)).await;
    expect_error(msg, "auth_failed");
    assert_marker_absent_after(&marker, NEGATIVE_OBSERVATION).await;

    // Control: the same profile, requested by a registered approver, does
    // start the listing command.
    let mut approver = approver_connection(&f).await;
    send(
        &mut approver,
        &ClientMessage::ListModelChoices {
            profile: "listing".to_string(),
            request_id: 2,
        },
    )
    .await;
    expect_requested(read_response(&mut approver, Duration::from_secs(5)).await);
    wait_until_marker_appears(&marker, Duration::from_secs(5)).await;
}

#[tokio::test]
async fn save_model_aliases_from_a_non_approver_connection_is_refused_and_leaves_the_file_untouched()
 {
    let (_root, profiles_dir) = fresh_dirs("model-i1-save");
    let path = profiles_dir.join("tiers.toml");
    write_profile(
        &profiles_dir,
        "tiers",
        &alias_profile("Tiers", &[("fast", "tier-a"), ("default", "tier-b")]),
    );
    let before = std::fs::read(&path).expect("fixture must be readable");
    let f = Harness::start("model-i1-save", base_config(profiles_dir, Vec::new())).await;
    let mut external = f.connect().await;

    send(
        &mut external,
        &ClientMessage::SaveModelAliases {
            profile: "tiers".to_string(),
            aliases: vec![
                LaunchOptionAlias {
                    alias: "fast".to_string(),
                    value: "tier-a-new".to_string(),
                },
                LaunchOptionAlias {
                    alias: "default".to_string(),
                    value: "tier-b".to_string(),
                },
            ],
        },
    )
    .await;
    let msg = read_response(&mut external, Duration::from_secs(5)).await;
    expect_error(msg, "auth_failed");
    assert_eq!(
        std::fs::read(&path).expect("file must stay readable"),
        before,
        "a refused save must not touch the file"
    );
}

// ---------------------------------------------------------------------------
// An invalid id is invalid_argument; a daemon with no profiles_dir
// answers unsupported.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_invalid_profile_id_is_invalid_argument() {
    let (_root, profiles_dir) = fresh_dirs("model-i2-invalid-id");
    let f = Harness::start("model-i2-invalid-id", base_config(profiles_dir, Vec::new())).await;
    let mut approver = approver_connection(&f).await;

    send(
        &mut approver,
        &ClientMessage::ListModelChoices {
            profile: "Bad Id".to_string(),
            request_id: 1,
        },
    )
    .await;
    let msg = read_response(&mut approver, Duration::from_secs(5)).await;
    expect_error(msg, "invalid_argument");
}

#[tokio::test]
async fn no_profiles_dir_configured_answers_unsupported() {
    let config = DaemonConfig {
        profiles_dir: None,
        model_choices_max_concurrent: 2,
        model_choices_timeout: Duration::from_secs(10),
        model_choices_max_output_bytes: 1 << 20,
        ..approver_base_config()
    };
    let harness = Harness::start("model-i2-no-dir", config).await;
    let mut approver = approver_connection(&harness).await;

    send(
        &mut approver,
        &ClientMessage::ListModelChoices {
            profile: "whatever".to_string(),
            request_id: 1,
        },
    )
    .await;
    let msg = read_response(&mut approver, Duration::from_secs(5)).await;
    expect_error(msg, "unsupported");
}

// ---------------------------------------------------------------------------
// An approver's list_model_choices answers with the same request_id it
// sent, first an immediate acknowledgement and then the list itself.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_values_profile_answers_from_values_and_starts_no_child() {
    let (root, profiles_dir) = fresh_dirs("model-i3-values");
    let marker = root.join("marker.txt");
    write_profile(
        &profiles_dir,
        "fixed",
        &values_profile("Fixed", &["tier-a", "tier-b"]),
    );
    // A profile the daemon must never even look at once `fixed` is chosen.
    write_profile(
        &profiles_dir,
        "unused-touch",
        &touch_profile("Unused", &capture_child_path(), &marker),
    );
    let f = Harness::start("model-i3-values", base_config(profiles_dir, Vec::new())).await;
    let mut approver = approver_connection(&f).await;

    send(
        &mut approver,
        &ClientMessage::ListModelChoices {
            profile: "fixed".to_string(),
            request_id: 4242,
        },
    )
    .await;
    let (request_id, profile) =
        expect_requested(read_response(&mut approver, Duration::from_secs(5)).await);
    assert_eq!(request_id, 4242);
    assert_eq!(profile, "fixed");

    let (request_id, profile, source, choices, error) =
        expect_choices(read_response(&mut approver, Duration::from_secs(5)).await);
    assert_eq!(request_id, 4242);
    assert_eq!(profile, "fixed");
    assert_eq!(source, Some(ModelChoiceSource::Values));
    assert_eq!(
        choices,
        vec![
            ModelChoice {
                id: "tier-a".to_string(),
                label: None
            },
            ModelChoice {
                id: "tier-b".to_string(),
                label: None
            },
        ]
    );
    assert!(error.is_none());
    assert_marker_absent_after(&marker, NEGATIVE_OBSERVATION).await;
}

#[tokio::test]
async fn a_lines_profile_lists_choices_read_from_the_commands_output() {
    let (_root, profiles_dir) = fresh_dirs("model-i3-lines");
    write_profile(
        &profiles_dir,
        "lines",
        &command_profile(
            "Lines",
            &capture_child_path(),
            &["echo", "0", "tier-a\tTier A\ntier-b\tTier B\n"],
            "format = \"lines\"\nseparator = \"\\t\"",
        ),
    );
    let f = Harness::start("model-i3-lines", base_config(profiles_dir, Vec::new())).await;
    let mut approver = approver_connection(&f).await;

    send(
        &mut approver,
        &ClientMessage::ListModelChoices {
            profile: "lines".to_string(),
            request_id: 7,
        },
    )
    .await;
    expect_requested(read_response(&mut approver, Duration::from_secs(5)).await);
    let (_, _, source, choices, error) =
        expect_choices(read_response(&mut approver, Duration::from_secs(10)).await);
    match source {
        Some(ModelChoiceSource::Command { argv }) => {
            assert_eq!(
                argv.last().map(String::as_str),
                Some("tier-a\tTier A\ntier-b\tTier B\n")
            );
        }
        other => panic!("expected a command source, got {other:?}"),
    }
    assert_eq!(
        choices,
        vec![
            ModelChoice {
                id: "tier-a".to_string(),
                label: Some("Tier A".to_string()),
            },
            ModelChoice {
                id: "tier-b".to_string(),
                label: Some("Tier B".to_string()),
            },
        ]
    );
    assert!(error.is_none());
}

#[tokio::test]
async fn a_json_profile_with_a_filter_lists_only_the_matching_choices() {
    let (_root, profiles_dir) = fresh_dirs("model-i3-json");
    let stdout = "{\"tiers\":[{\"slug\":\"tier-a\",\"title\":\"Tier A\",\"shown\":\"yes\"},\
                  {\"slug\":\"tier-b\",\"title\":\"Tier B\",\"shown\":\"no\"}]}";
    write_profile(
        &profiles_dir,
        "json",
        &command_profile(
            "Json",
            &capture_child_path(),
            &["echo", "0", stdout],
            "format = \"json\"\nitems = \"/tiers\"\nid = \"/slug\"\nlabel = \"/title\"\nfilter = \
             { pointer = \"/shown\", equals = \"yes\" }",
        ),
    );
    let f = Harness::start("model-i3-json", base_config(profiles_dir, Vec::new())).await;
    let mut approver = approver_connection(&f).await;

    send(
        &mut approver,
        &ClientMessage::ListModelChoices {
            profile: "json".to_string(),
            request_id: 9,
        },
    )
    .await;
    expect_requested(read_response(&mut approver, Duration::from_secs(5)).await);
    let (_, _, _, choices, error) =
        expect_choices(read_response(&mut approver, Duration::from_secs(10)).await);
    assert_eq!(
        choices,
        vec![ModelChoice {
            id: "tier-a".to_string(),
            label: Some("Tier A".to_string()),
        }],
        "the entry whose shown != yes must be filtered out"
    );
    assert!(error.is_none());
}

#[tokio::test]
async fn a_profile_with_no_choices_table_reports_no_source_no_choices_and_no_error() {
    let (_root, profiles_dir) = fresh_dirs("model-i3-none");
    write_profile(&profiles_dir, "plain", &no_choices_profile("Plain"));
    let f = Harness::start("model-i3-none", base_config(profiles_dir, Vec::new())).await;
    let mut approver = approver_connection(&f).await;

    send(
        &mut approver,
        &ClientMessage::ListModelChoices {
            profile: "plain".to_string(),
            request_id: 1,
        },
    )
    .await;
    expect_requested(read_response(&mut approver, Duration::from_secs(5)).await);
    let (_, _, source, choices, error) =
        expect_choices(read_response(&mut approver, Duration::from_secs(5)).await);
    assert!(source.is_none());
    assert!(choices.is_empty());
    assert!(error.is_none());
}

#[tokio::test]
async fn an_unlisted_profile_id_reports_an_error_in_the_push() {
    let (_root, profiles_dir) = fresh_dirs("model-i3-unknown");
    let f = Harness::start("model-i3-unknown", base_config(profiles_dir, Vec::new())).await;
    let mut approver = approver_connection(&f).await;

    send(
        &mut approver,
        &ClientMessage::ListModelChoices {
            profile: "ghost".to_string(),
            request_id: 1,
        },
    )
    .await;
    expect_requested(read_response(&mut approver, Duration::from_secs(5)).await);
    let (_, _, source, choices, error) =
        expect_choices(read_response(&mut approver, Duration::from_secs(5)).await);
    assert!(source.is_none());
    assert!(choices.is_empty());
    assert!(error.is_some());
}

#[tokio::test]
async fn two_requests_carry_the_callers_own_request_ids_independently() {
    let (_root, profiles_dir) = fresh_dirs("model-i3-two-ids");
    write_profile(&profiles_dir, "a", &values_profile("A", &["tier-a"]));
    write_profile(&profiles_dir, "b", &values_profile("B", &["tier-b"]));
    let f = Harness::start("model-i3-two-ids", base_config(profiles_dir, Vec::new())).await;
    let mut approver = approver_connection(&f).await;

    send(
        &mut approver,
        &ClientMessage::ListModelChoices {
            profile: "a".to_string(),
            request_id: 98765,
        },
    )
    .await;
    send(
        &mut approver,
        &ClientMessage::ListModelChoices {
            profile: "b".to_string(),
            request_id: 13579,
        },
    )
    .await;

    let mut requested_ids = Vec::new();
    let mut choice_ids = Vec::new();
    for _ in 0..2 {
        let (id, _) = expect_requested(read_response(&mut approver, Duration::from_secs(5)).await);
        requested_ids.push(id);
    }
    for _ in 0..2 {
        let (id, ..) = expect_choices(read_response(&mut approver, Duration::from_secs(5)).await);
        choice_ids.push(id);
    }
    requested_ids.sort_unstable();
    choice_ids.sort_unstable();
    assert_eq!(requested_ids, vec![13579, 98765]);
    assert_eq!(choice_ids, vec![13579, 98765]);
}

// ---------------------------------------------------------------------------
// The failure text a push carries.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_command_that_times_out_reports_the_time_limit() {
    let (_root, profiles_dir) = fresh_dirs("model-i4-timeout");
    write_profile(
        &profiles_dir,
        "slow",
        &command_profile(
            "Slow",
            &capture_child_path(),
            &["sleep", "3000"],
            "format = \"lines\"\nseparator = \"\\t\"",
        ),
    );
    let config = DaemonConfig {
        model_choices_timeout: Duration::from_secs(1),
        ..base_config(profiles_dir, Vec::new())
    };
    let f = Harness::start("model-i4-timeout", config).await;
    let mut approver = approver_connection(&f).await;

    send(
        &mut approver,
        &ClientMessage::ListModelChoices {
            profile: "slow".to_string(),
            request_id: 1,
        },
    )
    .await;
    expect_requested(read_response(&mut approver, Duration::from_secs(5)).await);
    let (_, _, _, _, error) =
        expect_choices(read_response(&mut approver, Duration::from_secs(10)).await);
    assert_eq!(error.as_deref(), Some("did not finish within 1 seconds"));
}

#[tokio::test]
async fn output_larger_than_the_configured_limit_is_refused() {
    let (_root, profiles_dir) = fresh_dirs("model-i4-too-large");
    write_profile(
        &profiles_dir,
        "big",
        &command_profile(
            "Big",
            &capture_child_path(),
            &["bytes", "4096"],
            "format = \"lines\"\nseparator = \"\\t\"",
        ),
    );
    let config = DaemonConfig {
        model_choices_max_output_bytes: 64,
        ..base_config(profiles_dir, Vec::new())
    };
    let f = Harness::start("model-i4-too-large", config).await;
    let mut approver = approver_connection(&f).await;

    send(
        &mut approver,
        &ClientMessage::ListModelChoices {
            profile: "big".to_string(),
            request_id: 1,
        },
    )
    .await;
    expect_requested(read_response(&mut approver, Duration::from_secs(5)).await);
    let (_, _, _, _, error) =
        expect_choices(read_response(&mut approver, Duration::from_secs(10)).await);
    assert_eq!(error.as_deref(), Some("output too large"));
}

#[tokio::test]
async fn a_nonzero_exit_status_is_reported() {
    let (_root, profiles_dir) = fresh_dirs("model-i4-exit-status");
    write_profile(
        &profiles_dir,
        "fails",
        &command_profile(
            "Fails",
            &capture_child_path(),
            &["echo", "2", "ignored"],
            "format = \"lines\"\nseparator = \"\\t\"",
        ),
    );
    let f = Harness::start(
        "model-i4-exit-status",
        base_config(profiles_dir, Vec::new()),
    )
    .await;
    let mut approver = approver_connection(&f).await;

    send(
        &mut approver,
        &ClientMessage::ListModelChoices {
            profile: "fails".to_string(),
            request_id: 1,
        },
    )
    .await;
    expect_requested(read_response(&mut approver, Duration::from_secs(5)).await);
    let (_, _, _, _, error) =
        expect_choices(read_response(&mut approver, Duration::from_secs(10)).await);
    assert_eq!(error.as_deref(), Some("exited with status 2"));
}

#[tokio::test]
async fn an_exit_status_127_names_the_program_that_was_not_found() {
    let (_root, profiles_dir) = fresh_dirs("model-i4-127");
    let command_path = capture_child_path();
    write_profile(
        &profiles_dir,
        "missing-127",
        &command_profile(
            "Missing",
            &command_path,
            &["echo", "127", "ignored"],
            "format = \"lines\"\nseparator = \"\\t\"",
        ),
    );
    let f = Harness::start("model-i4-127", base_config(profiles_dir, Vec::new())).await;
    let mut approver = approver_connection(&f).await;

    send(
        &mut approver,
        &ClientMessage::ListModelChoices {
            profile: "missing-127".to_string(),
            request_id: 1,
        },
    )
    .await;
    expect_requested(read_response(&mut approver, Duration::from_secs(5)).await);
    let (_, _, _, _, error) =
        expect_choices(read_response(&mut approver, Duration::from_secs(10)).await);
    let program = command_path
        .file_name()
        .and_then(|n| n.to_str())
        .expect("capture_child path has a file name");
    let expected = format!("{program} was not found");
    assert_eq!(error.as_deref(), Some(expected.as_str()));
}

#[tokio::test]
async fn an_unavailable_profile_is_refused_before_anything_is_started() {
    let (root, profiles_dir) = fresh_dirs("model-i4-unavailable");
    let marker = root.join("marker.txt");
    write_profile(
        &profiles_dir,
        "unavailable",
        &unavailable_profile("Unavailable"),
    );
    let f = Harness::start(
        "model-i4-unavailable",
        base_config(profiles_dir, Vec::new()),
    )
    .await;
    let mut approver = approver_connection(&f).await;

    send(
        &mut approver,
        &ClientMessage::ListModelChoices {
            profile: "unavailable".to_string(),
            request_id: 1,
        },
    )
    .await;
    expect_requested(read_response(&mut approver, Duration::from_secs(5)).await);
    let (_, _, source, choices, error) =
        expect_choices(read_response(&mut approver, Duration::from_secs(5)).await);
    assert!(source.is_none());
    assert!(choices.is_empty());
    assert_eq!(
        error.as_deref(),
        Some("the executable for this profile could not be found")
    );
    assert_marker_absent_after(&marker, NEGATIVE_OBSERVATION).await;
}

#[tokio::test]
async fn output_that_does_not_match_the_profiles_format_is_refused() {
    let (_root, profiles_dir) = fresh_dirs("model-i4-format-mismatch");
    write_profile(
        &profiles_dir,
        "empty",
        &command_profile(
            "Empty",
            &capture_child_path(),
            &["bytes", "0"],
            "format = \"lines\"\nseparator = \"\\t\"",
        ),
    );
    let f = Harness::start(
        "model-i4-format-mismatch",
        base_config(profiles_dir, Vec::new()),
    )
    .await;
    let mut approver = approver_connection(&f).await;

    send(
        &mut approver,
        &ClientMessage::ListModelChoices {
            profile: "empty".to_string(),
            request_id: 1,
        },
    )
    .await;
    expect_requested(read_response(&mut approver, Duration::from_secs(5)).await);
    let (_, _, _, _, error) =
        expect_choices(read_response(&mut approver, Duration::from_secs(10)).await);
    assert_eq!(
        error.as_deref(),
        Some("output did not match the profile's format")
    );
}

// ---------------------------------------------------------------------------
// Standard error never reaches the push or the log.
// ---------------------------------------------------------------------------

#[tokio::test]
#[traced_test]
async fn standard_error_never_reaches_the_push_or_the_log() {
    let (_root, profiles_dir) = fresh_dirs("model-i5-stderr");
    write_profile(
        &profiles_dir,
        "noisy",
        &command_profile(
            "Noisy",
            &capture_child_path(),
            &["echo", "0", "tier-a\tTier A\n", "leak-sentinel-on-stderr"],
            "format = \"lines\"\nseparator = \"\\t\"",
        ),
    );
    let f = Harness::start("model-i5-stderr", base_config(profiles_dir, Vec::new())).await;
    let mut approver = approver_connection(&f).await;

    send(
        &mut approver,
        &ClientMessage::ListModelChoices {
            profile: "noisy".to_string(),
            request_id: 1,
        },
    )
    .await;
    expect_requested(read_response(&mut approver, Duration::from_secs(5)).await);
    let (_, _, _, choices, error) =
        expect_choices(read_response(&mut approver, Duration::from_secs(10)).await);
    // Control: the stdout sentinel is present.
    assert!(choices.iter().any(|c| c.id == "tier-a"));
    assert!(error.is_none());
    assert!(
        !logs_contain("leak-sentinel-on-stderr"),
        "standard error must never reach the daemon's own log"
    );
}

// ---------------------------------------------------------------------------
// A listing beyond the concurrency limit is refused without waiting.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_listing_beyond_the_concurrency_limit_is_refused_without_waiting() {
    let (_root, profiles_dir) = fresh_dirs("model-i6-concurrency");
    write_profile(
        &profiles_dir,
        "slow-a",
        &command_profile(
            "SlowA",
            &capture_child_path(),
            &["sleep", "2000"],
            "format = \"lines\"\nseparator = \"\\t\"",
        ),
    );
    write_profile(
        &profiles_dir,
        "slow-b",
        &command_profile(
            "SlowB",
            &capture_child_path(),
            &["sleep", "2000"],
            "format = \"lines\"\nseparator = \"\\t\"",
        ),
    );
    let config = DaemonConfig {
        model_choices_max_concurrent: 1,
        ..base_config(profiles_dir, Vec::new())
    };
    let f = Harness::start("model-i6-concurrency", config).await;
    let mut approver = approver_connection(&f).await;

    send(
        &mut approver,
        &ClientMessage::ListModelChoices {
            profile: "slow-a".to_string(),
            request_id: 1,
        },
    )
    .await;
    expect_requested(read_response(&mut approver, Duration::from_secs(5)).await);

    send(
        &mut approver,
        &ClientMessage::ListModelChoices {
            profile: "slow-b".to_string(),
            request_id: 2,
        },
    )
    .await;
    // Bounded well below the fixture's own 2s sleep: this must be an
    // immediate refusal, not a wait for the first listing to finish.
    let msg = read_response(&mut approver, Duration::from_millis(500)).await;
    expect_error(msg, "resource_exhausted");

    // The first request still finishes and its push still arrives.
    let (request_id, ..) =
        expect_choices(read_response(&mut approver, Duration::from_secs(10)).await);
    assert_eq!(request_id, 1);
}

// ---------------------------------------------------------------------------
// The same connection can still be served while a listing runs.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn list_windows_answers_immediately_while_a_listing_is_still_running() {
    let (_root, profiles_dir) = fresh_dirs("model-i7-list-windows");
    write_profile(
        &profiles_dir,
        "slow",
        &command_profile(
            "Slow",
            &capture_child_path(),
            &["sleep", "2000"],
            "format = \"lines\"\nseparator = \"\\t\"",
        ),
    );
    let f = Harness::start(
        "model-i7-list-windows",
        base_config(profiles_dir, Vec::new()),
    )
    .await;
    let mut approver = approver_connection(&f).await;

    send(
        &mut approver,
        &ClientMessage::ListModelChoices {
            profile: "slow".to_string(),
            request_id: 1,
        },
    )
    .await;
    expect_requested(read_response(&mut approver, Duration::from_secs(5)).await);

    send(&mut approver, &ClientMessage::ListWindows).await;
    let msg = read_response(&mut approver, Duration::from_millis(500)).await;
    match msg {
        DaemonMessage::WindowList { .. } => {}
        other => {
            panic!("expected an immediate window_list while a listing is in flight, got {other:?}")
        }
    }
}

// ---------------------------------------------------------------------------
// save_model_aliases.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn save_model_aliases_stores_the_values_and_list_profiles_reports_them() {
    let (_root, profiles_dir) = fresh_dirs("model-i8-save");
    write_profile(
        &profiles_dir,
        "tiers",
        &alias_profile("Tiers", &[("fast", "tier-a"), ("default", "tier-b")]),
    );
    let f = Harness::start("model-i8-save", base_config(profiles_dir, Vec::new())).await;
    let mut approver = approver_connection(&f).await;

    let requested = vec![
        LaunchOptionAlias {
            alias: "fast".to_string(),
            value: "tier-a-new".to_string(),
        },
        LaunchOptionAlias {
            alias: "default".to_string(),
            value: "tier-b".to_string(),
        },
    ];
    send(
        &mut approver,
        &ClientMessage::SaveModelAliases {
            profile: "tiers".to_string(),
            aliases: requested.clone(),
        },
    )
    .await;
    let (profile, saved) =
        expect_aliases_saved(read_response(&mut approver, Duration::from_secs(5)).await);
    assert_eq!(profile, "tiers");
    assert_eq!(saved, requested);

    let profiles = list_profiles(&mut approver).await;
    let tiers = profiles
        .iter()
        .find(|p| p.id == "tiers")
        .expect("the saved profile must still be listed");
    let model = tiers
        .model
        .as_ref()
        .expect("the profile still maps the model option");
    assert_eq!(model.aliases, requested);
}

#[tokio::test]
async fn save_model_aliases_with_a_changed_key_set_is_invalid_argument_and_leaves_the_file_untouched()
 {
    let (_root, profiles_dir) = fresh_dirs("model-i8-key-set");
    let path = profiles_dir.join("tiers.toml");
    write_profile(
        &profiles_dir,
        "tiers",
        &alias_profile("Tiers", &[("fast", "tier-a"), ("default", "tier-b")]),
    );
    let before = std::fs::read(&path).expect("fixture must be readable");
    let f = Harness::start("model-i8-key-set", base_config(profiles_dir, Vec::new())).await;
    let mut approver = approver_connection(&f).await;

    send(
        &mut approver,
        &ClientMessage::SaveModelAliases {
            profile: "tiers".to_string(),
            aliases: vec![LaunchOptionAlias {
                alias: "fast".to_string(),
                value: "tier-a-new".to_string(),
            }],
        },
    )
    .await;
    let msg = read_response(&mut approver, Duration::from_secs(5)).await;
    expect_error(msg, "invalid_argument");
    assert_eq!(
        std::fs::read(&path).expect("file must stay readable"),
        before
    );
}

#[tokio::test]
async fn save_model_aliases_with_an_invalid_value_is_invalid_argument_and_leaves_the_file_untouched()
 {
    let (_root, profiles_dir) = fresh_dirs("model-i8-invalid-value");
    let path = profiles_dir.join("tiers.toml");
    write_profile(
        &profiles_dir,
        "tiers",
        &alias_profile("Tiers", &[("fast", "tier-a"), ("default", "tier-b")]),
    );
    let before = std::fs::read(&path).expect("fixture must be readable");
    let f = Harness::start(
        "model-i8-invalid-value",
        base_config(profiles_dir, Vec::new()),
    )
    .await;
    let mut approver = approver_connection(&f).await;

    send(
        &mut approver,
        &ClientMessage::SaveModelAliases {
            profile: "tiers".to_string(),
            aliases: vec![
                LaunchOptionAlias {
                    alias: "fast".to_string(),
                    value: "has a space".to_string(),
                },
                LaunchOptionAlias {
                    alias: "default".to_string(),
                    value: "tier-b".to_string(),
                },
            ],
        },
    )
    .await;
    let msg = read_response(&mut approver, Duration::from_secs(5)).await;
    expect_error(msg, "invalid_argument");
    assert_eq!(
        std::fs::read(&path).expect("file must stay readable"),
        before
    );
}

// ---------------------------------------------------------------------------
// (Windows only) a `.cmd` shim resolved through the same argv
// resolution a window uses.
// ---------------------------------------------------------------------------

#[cfg(windows)]
#[tokio::test]
async fn a_cmd_shim_profile_lists_choices_through_the_resolved_argv() {
    let (root, profiles_dir) = fresh_dirs("model-i10-cmd-shim");
    let bin_dir = root.join("bin");
    std::fs::create_dir_all(&bin_dir).expect("bin dir should be creatable");
    let shim_content = format!(
        "@echo off\r\n\"{}\" echo 0 \"tier-a\tTier A\"\r\n",
        capture_child_path().display(),
    );
    std::fs::write(bin_dir.join("capshim.cmd"), shim_content)
        .expect("shim fixture should be writable");
    write_profile(
        &profiles_dir,
        "shim",
        &command_profile_by_name(
            "Shim",
            "capshim",
            &["ignored"],
            "format = \"lines\"\nseparator = \"\\t\"",
        ),
    );
    let config = base_config(profiles_dir, vec![bin_dir]);
    let f = Harness::start("model-i10-cmd-shim", config).await;
    let mut approver = approver_connection(&f).await;

    send(
        &mut approver,
        &ClientMessage::ListModelChoices {
            profile: "shim".to_string(),
            request_id: 1,
        },
    )
    .await;
    expect_requested(read_response(&mut approver, Duration::from_secs(5)).await);
    let (_, _, _, choices, error) =
        expect_choices(read_response(&mut approver, Duration::from_secs(10)).await);
    assert!(
        error.is_none(),
        "the shim must be resolved and run successfully, got error {error:?}"
    );
    assert_eq!(
        choices,
        vec![ModelChoice {
            id: "tier-a".to_string(),
            label: Some("Tier A".to_string()),
        }]
    );
}

// ---------------------------------------------------------------------------
// A program found only in an extra bin directory (not PATH).
// ---------------------------------------------------------------------------

// Extra bin directories are a Windows-only lookup rule (npm's shim directory).
#[cfg(windows)]
#[tokio::test]
async fn a_program_found_only_in_the_extra_bin_directory_lists_choices() {
    let (root, profiles_dir) = fresh_dirs("model-i11-extra-bin");
    let bin_dir = root.join("bin");
    std::fs::create_dir_all(&bin_dir).expect("bin dir should be creatable");
    let copy_path = bin_dir.join("listing-program.exe");
    std::fs::copy(capture_child_path(), &copy_path).expect("fixture copy should succeed");
    write_profile(
        &profiles_dir,
        "resolved",
        &command_profile_by_name(
            "Resolved",
            "listing-program",
            &["echo", "0", "tier-a\tTier A\n"],
            "format = \"lines\"\nseparator = \"\\t\"",
        ),
    );
    let config = base_config(profiles_dir, vec![bin_dir]);
    let f = Harness::start("model-i11-extra-bin", config).await;
    let mut approver = approver_connection(&f).await;

    send(
        &mut approver,
        &ClientMessage::ListModelChoices {
            profile: "resolved".to_string(),
            request_id: 1,
        },
    )
    .await;
    expect_requested(read_response(&mut approver, Duration::from_secs(5)).await);
    let (_, _, _, choices, error) =
        expect_choices(read_response(&mut approver, Duration::from_secs(10)).await);
    assert!(
        error.is_none(),
        "the program found only through the extra bin directory must be resolved and run, got \
         error {error:?}"
    );
    assert_eq!(
        choices,
        vec![ModelChoice {
            id: "tier-a".to_string(),
            label: Some("Tier A".to_string()),
        }]
    );
}

// ---------------------------------------------------------------------------
// The child base environment reaches a listing command: under the profile's
// own [env], on top of what the daemon itself inherited.
// ---------------------------------------------------------------------------

fn os_pairs(pairs: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
    pairs
        .iter()
        .map(|(name, value)| (OsString::from(name), OsString::from(value)))
        .collect()
}

/// A variable this (in-process) daemon inherited whose value is itself a
/// valid choice id, so a listing that echoes it back reports it unchanged.
/// Picked by name order so every run of a given environment picks the same.
fn inherited_listable_var() -> (String, String) {
    let mut candidates: Vec<(String, String)> = std::env::vars_os()
        .filter_map(|(name, value)| Some((name.into_string().ok()?, value.into_string().ok()?)))
        .filter(|(name, value)| {
            !name.is_empty()
                && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                && !name.to_ascii_uppercase().starts_with("YAGURA_")
                && !name.starts_with("CAPTURE_BASE_PROBE")
                && validate_launch_option_value(value).is_ok()
        })
        .collect();
    candidates.sort();
    candidates.into_iter().next().expect(
        "the test process must inherit at least one variable whose value is a valid choice id",
    )
}

/// Asserts `name` is absent from this process, so a listing that reports a
/// value for it can only have taken that value from the base environment.
fn assert_not_inherited(name: &str) {
    assert!(
        std::env::var_os(name).is_none(),
        "{name} must not be set in the test process for this case to mean anything"
    );
}

/// A profile whose listing command is `capture_child env <names>...`, read
/// as one choice per line, with `env_table` appended verbatim.
fn env_listing_profile(names: &[&str], env_table: &str) -> String {
    let mut args: Vec<&str> = vec!["env"];
    args.extend_from_slice(names);
    let format_body = format!("format = \"lines\"\nseparator = \"\\t\"\n{env_table}");
    command_profile("Env Listing", &capture_child_path(), &args, &format_body)
}

/// Runs one listing of the `"envlist"` profile against `config` and returns
/// the choice ids it reported, failing on any error.
async fn listed_ids(tag: &str, config: DaemonConfig) -> Vec<String> {
    let f = Harness::start(tag, config).await;
    let mut approver = approver_connection(&f).await;
    send(
        &mut approver,
        &ClientMessage::ListModelChoices {
            profile: "envlist".to_string(),
            request_id: 1,
        },
    )
    .await;
    expect_requested(read_response(&mut approver, Duration::from_secs(5)).await);
    let (_, _, _, choices, error) =
        expect_choices(read_response(&mut approver, Duration::from_secs(10)).await);
    assert!(error.is_none(), "the listing failed: {error:?}");
    choices.into_iter().map(|choice| choice.id).collect()
}

#[tokio::test]
async fn a_listing_sees_the_child_base_environment_on_top_of_the_daemons_own() {
    assert_not_inherited("CAPTURE_BASE_PROBE_A");
    let (inherited_name, inherited_value) = inherited_listable_var();
    let (_root, profiles_dir) = fresh_dirs("model-base-env-listing");
    write_profile(
        &profiles_dir,
        "envlist",
        &env_listing_profile(&["CAPTURE_BASE_PROBE_A", &inherited_name], ""),
    );
    let config = DaemonConfig {
        child_base_env: ChildBaseEnv::from_pairs(os_pairs(&[(
            "CAPTURE_BASE_PROBE_A",
            "from-base-a",
        )])),
        ..base_config(profiles_dir, Vec::new())
    };

    let ids: Vec<String> = listed_ids("model-base-env-listing", config).await;
    assert_eq!(
        ids,
        vec!["from-base-a".to_string(), inherited_value],
        "the listing must see the base environment and still inherit {inherited_name}"
    );
}

#[tokio::test]
async fn the_profiles_env_wins_over_the_child_base_environment_in_a_listing() {
    assert_not_inherited("CAPTURE_BASE_PROBE_A");
    assert_not_inherited("CAPTURE_BASE_PROBE_B");
    let (_root, profiles_dir) = fresh_dirs("model-base-env-profile-wins");
    write_profile(
        &profiles_dir,
        "envlist",
        &env_listing_profile(
            &["CAPTURE_BASE_PROBE_A", "CAPTURE_BASE_PROBE_B"],
            "\n[env]\nCAPTURE_BASE_PROBE_A = \"from-profile-a\"\n",
        ),
    );
    let config = DaemonConfig {
        child_base_env: ChildBaseEnv::from_pairs(os_pairs(&[
            ("CAPTURE_BASE_PROBE_A", "from-base-a"),
            ("CAPTURE_BASE_PROBE_B", "from-base-b"),
        ])),
        ..base_config(profiles_dir, Vec::new())
    };

    let ids: Vec<String> = listed_ids("model-base-env-profile-wins", config).await;
    assert_eq!(
        ids,
        vec!["from-profile-a".to_string(), "from-base-b".to_string()]
    );
}

#[tokio::test]
async fn with_an_empty_child_base_environment_a_listing_sees_the_daemons_own_value() {
    let (inherited_name, inherited_value) = inherited_listable_var();
    let (_root, profiles_dir) = fresh_dirs("model-base-env-empty");
    write_profile(
        &profiles_dir,
        "envlist",
        &env_listing_profile(&[&inherited_name], ""),
    );
    let config = DaemonConfig {
        child_base_env: ChildBaseEnv::default(),
        ..base_config(profiles_dir, Vec::new())
    };

    let ids: Vec<String> = listed_ids("model-base-env-empty", config).await;
    assert_eq!(ids, vec![inherited_value]);
}
