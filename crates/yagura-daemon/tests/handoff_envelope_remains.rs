#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: whether a `profile`-addressed `send_handoff` closes the
//! window it opened. The envelope is handed over as the launch prompt and is
//! taken once the window started, so nothing is ever typed into the window and
//! no later attempt can leave anything behind: the window is closed only when
//! its spawn fails or the envelope is known never to be delivered. A profile
//! that cannot be handed a request at launch is refused before anything starts.

mod common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use common::{
    Harness, call_list_windows, call_send_handoff_from_profile_within, empty_lookup,
    expect_handoff_sent, handoff_profile_config, policy, unique_temp_dir, write_profile,
};
use yagura_daemon::DaemonConfig;
use yagura_protocol::DaemonMessage;
use yagura_test_support::process;

/// Upper bound on a `handoff_sent` reply: a profile launch.
const REPLY_TIMEOUT: Duration = Duration::from_secs(30);

/// A config resolving profiles from `profiles_dir` against `detection_dir`,
/// with `send_handoff` auto-approved so no approver connection is needed.
fn profile_config(profiles_dir: PathBuf, detection_dir: &Path) -> DaemonConfig {
    handoff_profile_config(
        Some(profiles_dir),
        empty_lookup(),
        detection_dir,
        None,
        Some(policy(&["send_handoff"])),
        30,
    )
}

/// The `profiles` / `detection` directory pair a case writes into.
fn profile_dirs(dir: &Path) -> (PathBuf, PathBuf) {
    let profiles_dir = dir.join("profiles");
    let detection_dir = dir.join("detection");
    std::fs::create_dir_all(&profiles_dir).expect("make profiles dir");
    std::fs::create_dir_all(&detection_dir).expect("make detection dir");
    (profiles_dir, detection_dir)
}

/// A `hook_child` profile with a composer that is handed a first request at launch
/// as `--prompt <request>` and logs its argv under `logs`.
fn launch_profile_source(logs: &Path) -> String {
    let command = [
        PathBuf::from(env!("CARGO_BIN_EXE_hook_child"))
            .display()
            .to_string(),
        process::COMPOSER_MARKER.to_string(),
        process::COMPOSER_SEPARATOR.to_string(),
        process::COMPOSER_CONTINUATION_PREFIX.to_string(),
        "0".to_string(),
        "-".to_string(),
        "--log".to_string(),
        logs.display().to_string(),
    ]
    .iter()
    .map(|arg| format!("'{arg}'"))
    .collect::<Vec<_>>()
    .join(", ");
    format!(
        "schema = 1\ndisplay_name = \"Launch\"\n\n[command]\ndefault = [{command}]\n{}",
        "\n[delivery]\nwake = \"none\"\nlaunch_prompt = [\"--prompt\", \"{prompt}\"]\n"
    )
}

#[tokio::test]
async fn a_window_that_took_its_envelope_at_launch_stays_open_after_the_requester_leaves() {
    let dir = unique_temp_dir("envelope-remains-launch");
    let (profiles_dir, detection_dir) = profile_dirs(&dir);
    let logs = dir.join("launch-logs");
    std::fs::create_dir_all(&logs).expect("make the log directory");
    write_profile(&profiles_dir, "launch", &launch_profile_source(&logs));

    let h = Harness::start(
        "envelope-remains-launch",
        profile_config(profiles_dir, &detection_dir),
    )
    .await;
    let mut requester = h.connect().await;
    let reply = call_send_handoff_from_profile_within(
        &mut requester,
        "launch",
        None,
        None,
        "src/foo.rs",
        "look",
        REPLY_TIMEOUT,
    )
    .await;
    let (window_id, opened, delivery) = expect_handoff_sent(reply);
    assert!(opened);
    assert_eq!(
        serde_json::to_value(delivery).expect("a delivery encodes"),
        "taken"
    );
    drop(requester);

    // A later connection's view: the requester's departure tore nothing down.
    let mut observer = h.connect().await;
    let windows = call_list_windows(&mut observer).await;
    let info = windows
        .iter()
        .find(|window| window.window_id == window_id)
        .unwrap_or_else(|| panic!("the opened window is listed: {windows:?}"));
    assert!(
        info.exit_code.is_none(),
        "a window that took its envelope at launch is the requester's to keep: {info:?}"
    );
    assert!(
        h.registry
            .get(&window_id)
            .expect("the window is still registered")
            .session()
            .exit_status()
            .is_none(),
        "its child is still running"
    );

    h.registry.terminate_all().await;
}

#[tokio::test]
async fn a_profile_that_cannot_be_handed_a_request_at_launch_is_refused_before_anything_starts() {
    let dir = unique_temp_dir("envelope-remains-no-launch-prompt");
    let (profiles_dir, detection_dir) = profile_dirs(&dir);
    // It declares a composer, but no launch prompt: the envelope is never typed into
    // a window, so there is no way to hand it over.
    write_profile(
        &profiles_dir,
        "drawn",
        &process::ready_child_profile_source_with_composer("Drawn", 0),
    );

    let h = Harness::start(
        "envelope-remains-no-launch-prompt",
        profile_config(profiles_dir, &detection_dir),
    )
    .await;
    let mut requester = h.connect().await;
    let reply = call_send_handoff_from_profile_within(
        &mut requester,
        "drawn",
        None,
        None,
        "src/foo.rs",
        "look",
        REPLY_TIMEOUT,
    )
    .await;
    assert!(
        matches!(reply, DaemonMessage::Error { .. }),
        "a profile with no launch prompt cannot be handed the envelope, got {reply:?}"
    );
    assert!(
        call_list_windows(&mut requester).await.is_empty(),
        "nothing starts for a handoff that cannot be handed over"
    );
}
