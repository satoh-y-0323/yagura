// Real windows, a real IPC endpoint, the daemon's own server: the properties are what the
// doors a CLI's hook reads from return, and what the daemon keeps.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: a window's inbox and the doors that read it (`deliver_context`,
//! `deliver_take`, `deliver_wait`, `report_session`).
//!
//! The inbox is filled through `WindowEntry::inbox().deposit(..)`; nothing in this layer
//! fills it over IPC. Parcels are read back only through the doors, as a hook does.

use crate::common;

use std::path::Path;
use std::time::Duration;

use tracing_test::traced_test;
use yagura_daemon::fault_point::{FaultPoint, FaultPoints};
use yagura_daemon::inbox::{DeliverySpec, DoorOutputs, OutputForm, Parcel};
use yagura_daemon::profile::parse_profile;
use yagura_daemon::profile_delivery::ProfileDelivery;
use yagura_daemon::store;
use yagura_daemon::wake_file::WAKE_FILE_NAME;
use yagura_daemon::window::WindowProvenance;
use yagura_daemon::{DaemonConfig, OpenWindowRequest};
use yagura_protocol::{ClientMessage, DaemonMessage, DeliveryStream, WindowStatus};
use yagura_test_support::ipc::Connection;

use crate::hooks_injection::{credentials_folder_of, until_gone};
use crate::send_message_lifecycle::{
    open_wake_window, outcome_of, register_wait, send_queued, state_of, wake_file_of, withdraw,
};
use common::{
    CONTROL_DETECTION_NAME, Harness, READY_MARKER, VALID_TOKEN, approver_base_config, call_door,
    detection_config, echo_child_path, expect_delivery, expect_error, query_status,
    unique_temp_dir, wait_until_status,
};

fn config(tag: &str, tweak: impl FnOnce(&mut DaemonConfig)) -> DaemonConfig {
    let dir = unique_temp_dir(tag);
    let mut config = detection_config(&dir, approver_base_config());
    tweak(&mut config);
    config
}

pub(crate) async fn start(
    tag: &str,
    tweak: impl FnOnce(&mut DaemonConfig),
) -> (Harness, Connection) {
    let h = Harness::start(tag, config(tag, tweak)).await;
    let client = h.connect().await;
    (h, client)
}

/// The `[delivery]` table `lines` declare, read as a profile file of its own is read.
pub(crate) fn profile_delivery(lines: &str) -> ProfileDelivery {
    let source = format!(
        "schema = 1\ndisplay_name = \"Bytes\"\n\n[command]\ndefault = [\"some-program\"]\n\n\
         [delivery]\n{lines}\n"
    );
    parse_profile("bytes", &source)
        .unwrap_or_else(|err| panic!("{lines:?} should be accepted, got {err:?}"))
        .delivery
        .unwrap_or_else(|| panic!("{lines:?} declares a [delivery] table"))
}

/// An idle window whose profile declares `delivery`: its doors answer, and its inbox
/// takes parcels in, as that table says.
pub(crate) async fn delivering_window(
    h: &Harness,
    client: &mut Connection,
    delivery: ProfileDelivery,
) -> String {
    let id = h
        .registry
        .open_with_extra_env(
            OpenWindowRequest {
                command: yagura_test_support::process::echo_child_command(),
                cwd: None,
                name: None,
                vendor: None,
                detection: Some(CONTROL_DETECTION_NAME.to_string()),
                on_name_taken: Default::default(),
            },
            Vec::new(),
            Vec::new(),
            WindowProvenance {
                delivery: Some(delivery),
                ..WindowProvenance::default()
            },
        )
        .await
        .expect("the window opens")
        .window_id;
    h.write_input(&id, READY_MARKER).await;
    wait_until_status(client, &id, WindowStatus::Idle).await;
    id
}

/// Asserts the window's inbox takes in an ASCII text `slack` bytes short of `limit` and
/// turns away one of `limit + 1` bytes. `slack` is what the widest form the window prints
/// in adds around the text; zero when every form prints it plain and nothing precedes it.
pub(crate) fn assert_takes_in_up_to(h: &Harness, window_id: &str, limit: usize, slack: usize) {
    assert!(
        deposit(h, window_id, "within", &"a".repeat(limit - slack)).is_empty(),
        "a text of {} bytes fits within {limit}",
        limit - slack
    );
    assert_eq!(
        deposit(h, window_id, "over", &"a".repeat(limit + 1)).len(),
        1,
        "a text of {} bytes is over {limit}",
        limit + 1
    );
}

/// A window whose status stays `Unknown`: its definition is loaded but nothing has
/// matched yet.
async fn unknown_window(h: &Harness, client: &mut Connection) -> String {
    let path = echo_child_path();
    let id = h
        .open(
            vec![path.to_str().unwrap().to_string()],
            Some(CONTROL_DETECTION_NAME),
        )
        .await;
    assert_eq!(query_status(client, &id).await, WindowStatus::Unknown);
    id
}

fn deposit(h: &Harness, window_id: &str, key: &str, text: &str) -> Vec<Parcel> {
    h.registry
        .get(window_id)
        .expect("window exists")
        .inbox()
        .deposit(Parcel::new(key, text))
}

fn pending(h: &Harness, window_id: &str) -> usize {
    h.registry
        .get(window_id)
        .expect("window exists")
        .inbox()
        .pending_count()
}

fn has_waiter(h: &Harness, window_id: &str) -> bool {
    h.registry
        .get(window_id)
        .expect("window exists")
        .inbox()
        .has_waiter()
}

/// Polls an observable daemon-side state; the deadline only detects a hang.
async fn until(what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + yagura_test_support::HANG_GUARD;
    while !condition() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// A door's read of `kind` (`context`, `take` or `wait`) with `token`, in the form a door
/// of revision 34 sends it.
fn door_read(window_id: &str, token: &str, kind: &str) -> ClientMessage {
    let wire = serde_json::json!({
        "type": "door_read",
        "window_id": window_id,
        "token": token,
        "door": { "kind": kind },
    });
    serde_json::from_value(wire.clone())
        .unwrap_or_else(|err| panic!("the wire must know a door's read {wire}: {err}"))
}

/// The notice a door sends once it has printed what its read was answered with.
fn printed() -> ClientMessage {
    serde_json::from_value(serde_json::json!({ "type": "deliver_printed" }))
        .unwrap_or_else(|err| panic!("the wire must know deliver_printed: {err}"))
}

fn take_msg(window_id: &str) -> ClientMessage {
    door_read(window_id, VALID_TOKEN, "take")
}

fn context_msg(window_id: &str) -> ClientMessage {
    door_read(window_id, VALID_TOKEN, "context")
}

fn wait_msg(window_id: &str) -> ClientMessage {
    door_read(window_id, VALID_TOKEN, "wait")
}

/// Reads through a door as a hook does: on a connection of its own, printing what it was
/// answered with and saying so before it ends.
async fn read(h: &Harness, msg: &ClientMessage) -> DaemonMessage {
    let mut door = h.connect().await;
    let answer = call_door(&mut door, msg).await;
    if matches!(answer, DaemonMessage::Delivery { .. }) {
        common::send(&mut door, &printed()).await;
    }
    answer
}

async fn take(h: &Harness, window_id: &str) -> String {
    expect_delivery(read(h, &take_msg(window_id)).await).0
}

fn pos(haystack: &str, needle: &str) -> usize {
    haystack
        .find(needle)
        .unwrap_or_else(|| panic!("{needle:?} missing from {haystack:?}"))
}

// --- take -------------------------------------------------------------------

#[tokio::test]
async fn a_take_returns_the_windows_parcels_in_order_and_takes_them_out() {
    let (h, mut client) = start("inbox-i1", |_| {}).await;
    let first = h.open_idle_window(&mut client).await;
    let second = h.open_idle_window(&mut client).await;
    deposit(&h, &first, "a", "PARCEL-ALPHA");
    deposit(&h, &first, "b", "PARCEL-BETA");
    deposit(&h, &second, "c", "PARCEL-GAMMA");

    let got = take(&h, &first).await;
    assert!(pos(&got, "PARCEL-ALPHA") < pos(&got, "PARCEL-BETA"));
    assert!(!got.contains("PARCEL-GAMMA"), "another window's parcel");
    assert_eq!(pending(&h, &first), 0);

    let again = take(&h, &first).await;
    assert!(again.is_empty(), "a taken parcel returns once: {again:?}");

    let other = take(&h, &second).await;
    assert!(other.contains("PARCEL-GAMMA"));
    assert!(!other.contains("PARCEL-ALPHA"));
}

// --- the take limit ---------------------------------------------------------

#[tokio::test]
async fn a_take_stays_within_the_limit_and_never_cuts_a_parcel() {
    let (h, mut client) = start("inbox-i2-ascii", |config| {
        config.chat_intro = String::new();
    })
    .await;
    let id = delivering_window(&h, &mut client, profile_delivery("max_bytes = 100")).await;
    let (a, b, c) = ("A".repeat(40), "B".repeat(40), "C".repeat(40));
    deposit(&h, &id, "a", &a);
    deposit(&h, &id, "b", &b);
    deposit(&h, &id, "c", &c);

    let first = take(&h, &id).await;
    assert!(first.len() <= 100, "{}", first.len());
    assert!(first.contains(&a), "the oldest parcel comes first, whole");
    assert!(!first.contains(&c), "three parcels of 40 do not fit in 100");
    let second = take(&h, &id).await;
    assert!(second.len() <= 100);
    let all = format!("{first}{second}");
    for parcel in [&a, &b, &c] {
        assert_eq!(all.matches(parcel.as_str()).count(), 1, "exactly once");
    }
    assert!(second.contains(&c));
    assert_eq!(pending(&h, &id), 0);
}

/// The text a JSON answer carries under `t`.
fn text_under_t(output: &str) -> String {
    let value: serde_json::Value =
        serde_json::from_str(output).unwrap_or_else(|err| panic!("not JSON ({err}): {output:?}"));
    value["t"]
        .as_str()
        .unwrap_or_else(|| panic!("the text is a JSON string: {output:?}"))
        .to_string()
}

#[tokio::test]
async fn the_limit_counts_the_bytes_of_the_output_as_printed() {
    let (h, mut client) = start("inbox-i2-bytes", |config| {
        config.chat_intro = String::new();
    })
    .await;

    // 20 characters of 3 bytes each: two fit in 100 characters, not in 100 bytes.
    let plain = delivering_window(&h, &mut client, profile_delivery("max_bytes = 100")).await;
    let (a, b) = ("あ".repeat(20), "い".repeat(20));
    deposit(&h, &plain, "a", &a);
    deposit(&h, &plain, "b", &b);
    let first = take(&h, &plain).await;
    assert!(first.contains(&a) && !first.contains(&b), "{first:?}");
    assert!(first.len() <= 100, "{}", first.len());
    assert!(take(&h, &plain).await.contains(&b), "the rest follows");
    deposit(&h, &plain, "a2", &a);
    deposit(&h, &plain, "b2", &b);
    let (woken, _, _) = expect_delivery(read(&h, &wait_msg(&plain)).await);
    assert!(woken.contains(&a) && !woken.contains(&b), "{woken:?}");
    assert!(
        take(&h, &plain).await.contains(&b),
        "a wait's answer leaves the rest too"
    );

    // A quote is escaped to two bytes in a JSON string: two parcels of 30 quotes are 62
    // characters, but print as 8 + 60 + 4 + 60 bytes.
    let json = delivering_window(
        &h,
        &mut client,
        profile_delivery("max_bytes = 100\noutput = '{\"t\":{text}}'"),
    )
    .await;
    let quotes = "\"".repeat(30);
    deposit(&h, &json, "a", &quotes);
    deposit(&h, &json, "b", &quotes);
    let first = take(&h, &json).await;
    assert!(first.len() <= 100, "{}: {first:?}", first.len());
    assert_eq!(text_under_t(&first), quotes, "one parcel, whole");
    assert_eq!(
        text_under_t(&take(&h, &json).await),
        quotes,
        "the rest follows"
    );
}

#[tokio::test]
async fn the_limit_includes_the_intro_in_the_context() {
    // 10 characters of 3 bytes each: 30 + 2 + 40 + 2 + 40 bytes do not fit in 100, though
    // 94 characters would.
    let intro = "い".repeat(10);
    let intro_for_config = intro.clone();
    let (h, mut client) = start("inbox-i2-intro", move |config| {
        config.chat_intro = intro_for_config;
    })
    .await;
    let id = delivering_window(&h, &mut client, profile_delivery("max_bytes = 100")).await;
    let (a, b) = ("A".repeat(40), "B".repeat(40));
    deposit(&h, &id, "a", &a);
    deposit(&h, &id, "b", &b);

    let (context, _, _) = expect_delivery(read(&h, &context_msg(&id)).await);
    assert!(context.contains(&intro) && context.contains(&a));
    assert!(
        !context.contains(&b),
        "30 + 2 + 40 + 2 + 40 bytes do not fit in 100"
    );
    assert!(context.len() <= 100, "{}", context.len());
    assert!(take(&h, &id).await.contains(&b), "the rest follows");
}

#[tokio::test]
async fn a_push_by_command_carries_what_fits_once_put_into_its_argument() {
    let (h, mut client) = start("inbox-i2-push", |config| {
        config.chat_intro = String::new();
    })
    .await;
    // The argument adds 14 bytes to the text: 14 + 45 + 2 + 45 is over 100, though the
    // texts alone, as the doors print them, fit.
    let id = delivering_window(
        &h,
        &mut client,
        profile_delivery(
            "max_bytes = 100\nwake = \"command\"\n\
             wake_args = [\"queue\", \"--thread\", \"{session}\", \"PREFIX-PREFIX-{text}\"]",
        ),
    )
    .await;
    let (a, b) = ("A".repeat(45), "B".repeat(45));
    assert!(deposit(&h, &id, "a", &a).is_empty());
    assert!(deposit(&h, &id, "b", &b).is_empty());
    let inbox = h.registry.get(&id).expect("window exists").inbox().clone();

    let push = inbox.begin_push().expect("something waits");
    assert_eq!(push.text, a, "the first parcel alone, whole");
    inbox.finish_push(push.id, yagura_daemon::inbox::PushEnd::Taken);
    let next = inbox.begin_push().expect("the rest waits");
    assert_eq!(next.text, b, "the rest follows");
}

#[tokio::test]
async fn a_parcel_over_the_limit_in_any_form_the_window_prints_is_turned_away_when_deposited() {
    let (h, mut client) = start("inbox-i2-widest", |config| {
        config.chat_intro = String::new();
    })
    .await;
    const JSON: &str = "'{\"t\":{text}}'";
    let wake = "wake = \"command\"\n\
                wake_args = [\"queue\", \"--thread\", \"{session}\", \"PREFIX-{text}\"]";
    // (what, the rest of the table, a text that fits, a text that does not)
    let cases: [(&str, String, String, String); 8] = [
        (
            "plain, counted in bytes",
            String::new(),
            "a".repeat(100),
            "あ".repeat(34),
        ),
        (
            "the take door's form",
            format!("output = {{ take = {JSON} }}"),
            "a".repeat(92),
            "a".repeat(93),
        ),
        (
            "the context door's form",
            format!("output = {{ context = {JSON} }}"),
            "a".repeat(92),
            "a".repeat(93),
        ),
        (
            "the wait door's form",
            format!("output = {{ wait = {JSON} }}"),
            "a".repeat(92),
            "a".repeat(93),
        ),
        (
            "a stop's block form",
            format!("output = {{ block = {JSON} }}"),
            "a".repeat(92),
            "a".repeat(93),
        ),
        (
            "escaping in a form",
            format!("output = {JSON}"),
            "\"".repeat(46),
            "\"".repeat(47),
        ),
        (
            "the wake command's argument",
            wake.to_string(),
            "a".repeat(50),
            "a".repeat(94),
        ),
        (
            "escaping in one form of several",
            format!("output = {{ take = \"plain\", wait = {JSON} }}"),
            "\n".repeat(46),
            "\n".repeat(47),
        ),
    ];
    for (what, rest, fits, over) in cases {
        let id = delivering_window(
            &h,
            &mut client,
            profile_delivery(&format!("max_bytes = 100\n{rest}")),
        )
        .await;
        assert!(
            deposit(&h, &id, "fits", &fits).is_empty(),
            "{what}: a parcel every form prints within 100 bytes is taken in"
        );
        assert_eq!(
            deposit(&h, &id, "over", &over).len(),
            1,
            "{what}: a parcel no door could carry whole is turned away"
        );
        assert_eq!(pending(&h, &id), 1, "{what}");
    }

    // The intro goes ahead of the parcels in the context: 30 + 2 + 69 is over 100.
    let (h, mut client) = start("inbox-i2-widest-intro", |config| {
        config.chat_intro = "I".repeat(30);
    })
    .await;
    let id = delivering_window(&h, &mut client, profile_delivery("max_bytes = 100")).await;
    assert!(deposit(&h, &id, "fits", &"a".repeat(68)).is_empty());
    assert_eq!(
        deposit(&h, &id, "over", &"a".repeat(69)).len(),
        1,
        "the context with its intro could not carry it whole"
    );
}

#[tokio::test]
async fn a_message_over_the_windows_limit_is_refused_as_an_invalid_argument() {
    let (h, mut client) = start("inbox-i2-send-message", |config| {
        config.chat_intro = String::new();
        config.approval_policy = Some(common::policy(&["send_message"]));
    })
    .await;
    let id = delivering_window(&h, &mut client, profile_delivery("max_bytes = 1000")).await;
    let send = |text: String| ClientMessage::SendText {
        window_id: id.clone(),
        text,
    };

    // 320 characters of 3 bytes each: the envelope is far below 1000 characters, but over
    // 1000 bytes.
    common::send(&mut client, &send("あ".repeat(320))).await;
    let refused = common::read_response(&mut client).await;
    assert_eq!(
        crate::send_message_lifecycle::state_of(&refused).as_deref(),
        Some("invalid_argument"),
        "{refused:?}"
    );
    assert_eq!(pending(&h, &id), 0, "nothing was deposited");

    common::send(&mut client, &send("a".repeat(320))).await;
    let queued = common::read_response(&mut client).await;
    assert_eq!(
        crate::send_message_lifecycle::state_of(&queued).as_deref(),
        Some("queued"),
        "{queued:?}"
    );
}

// --- context ----------------------------------------------------------------

#[tokio::test]
async fn the_context_is_the_intro_and_the_pending_parcels_and_takes_them() {
    let (h, mut client) = start("inbox-i3", |config| {
        config.chat_intro = "WELCOME-INTRO".into();
    })
    .await;
    let id = h.open_idle_window(&mut client).await;
    deposit(&h, &id, "a", "PARCEL-ALPHA");

    let (context, stream, code) = expect_delivery(read(&h, &context_msg(&id)).await);
    assert!(pos(&context, "WELCOME-INTRO") < pos(&context, "PARCEL-ALPHA"));
    assert_eq!(stream, DeliveryStream::Stdout);
    assert_eq!(code, 0);
    assert_eq!(pending(&h, &id), 0);
    let next = take(&h, &id).await;
    assert!(
        !next.contains("PARCEL-ALPHA"),
        "a parcel the context returned is not returned again"
    );
}

#[tokio::test]
async fn an_empty_intro_leaves_the_context_with_the_parcels_only() {
    let (h, mut client) = start("inbox-i3-empty", |config| {
        config.chat_intro = String::new();
    })
    .await;
    let id = h.open_idle_window(&mut client).await;
    let (nothing, _, code) = expect_delivery(read(&h, &context_msg(&id)).await);
    assert!(nothing.is_empty(), "no intro and no parcel: {nothing:?}");
    assert_eq!(code, 0);
    deposit(&h, &id, "a", "PARCEL-ALPHA");
    let (context, _, _) = expect_delivery(read(&h, &context_msg(&id)).await);
    assert!(context.contains("PARCEL-ALPHA"));
}

// --- an unknown window ------------------------------------------------------

#[tokio::test]
async fn the_doors_a_cli_reads_answer_in_an_unknown_window() {
    let (h, mut client) = start("inbox-i4-reads", |config| {
        config.chat_intro = "WELCOME-INTRO".into();
    })
    .await;
    let id = unknown_window(&h, &mut client).await;
    deposit(&h, &id, "a", "PARCEL-ALPHA");
    let (context, _, _) = expect_delivery(read(&h, &context_msg(&id)).await);
    assert!(context.contains("WELCOME-INTRO") && context.contains("PARCEL-ALPHA"));
    deposit(&h, &id, "b", "PARCEL-BETA");
    assert!(take(&h, &id).await.contains("PARCEL-BETA"));
    assert_eq!(query_status(&mut client, &id).await, WindowStatus::Unknown);
}

#[tokio::test]
async fn a_wait_in_an_unknown_window_is_not_woken_by_a_parcel() {
    let (h, mut client) = start("inbox-i4-wait", |_| {}).await;
    let id = unknown_window(&h, &mut client).await;
    deposit(&h, &id, "a", "PARCEL-ALPHA");

    let mut waiter = h.connect().await;
    common::send(&mut waiter, &wait_msg(&id)).await;
    until("the wait to be registered", || has_waiter(&h, &id)).await;
    assert_eq!(
        pending(&h, &id),
        1,
        "an unknown window is not pushed to: the parcel stays in the inbox"
    );
    deposit(&h, &id, "b", "PARCEL-BETA");
    assert!(has_waiter(&h, &id), "the wait is still registered");

    // Once the window can take a push the held parcels come out.
    h.write_input(&id, READY_MARKER).await;
    wait_until_status(&mut client, &id, WindowStatus::Idle).await;
    deposit(&h, &id, "c", "PARCEL-GAMMA");
    let (output, _, _) = expect_delivery(common::read_response(&mut waiter).await);
    assert!(
        output.contains("PARCEL-ALPHA"),
        "the parcel held while unknown is delivered: {output:?}"
    );
}

#[tokio::test]
async fn a_wrong_or_malformed_token_returns_nothing_and_takes_nothing() {
    let (h, mut client) = start("inbox-i4-auth", |_| {}).await;
    let id = h.open_idle_window(&mut client).await;
    deposit(&h, &id, "a", "PARCEL-ALPHA");
    for token in ["ffffffffffffffffffffffffffffffff", "short", ""] {
        for kind in ["take", "context", "wait"] {
            let msg = door_read(&id, token, kind);
            expect_error(call_door(&mut client, &msg).await, "auth_failed");
        }
    }
    assert_eq!(pending(&h, &id), 1);
    assert!(!has_waiter(&h, &id));
    let unknown = call_door(&mut client, &take_msg("w-does-not-exist")).await;
    expect_error(unknown, "window_not_found");
}

#[tokio::test]
async fn a_token_that_is_not_this_windows_cannot_read_its_inbox() {
    // Each window mints its own token when no override is configured.
    let (h, mut client) = start("inbox-i4-other", |config| {
        config.status_token_override = None;
    })
    .await;
    let id = h
        .open(vec![echo_child_path().to_str().unwrap().into()], None)
        .await;
    deposit(&h, &id, "a", "PARCEL-ALPHA");
    expect_error(call_door(&mut client, &take_msg(&id)).await, "auth_failed");
    assert_eq!(pending(&h, &id), 1);
}

// --- registered waits -------------------------------------------------------

#[tokio::test]
async fn a_registered_wait_returns_when_a_parcel_arrives_in_the_declared_form() {
    let (h, mut client) = start("inbox-i6-arrive", |config| {
        config.delivery = DeliverySpec {
            wake_stream: DeliveryStream::Stdout,
            wake_exit: 7,
            ..DeliverySpec::default()
        };
    })
    .await;
    let id = h.open_idle_window(&mut client).await;
    let mut waiter = h.connect().await;
    common::send(&mut waiter, &wait_msg(&id)).await;
    until("the wait to be registered", || has_waiter(&h, &id)).await;

    deposit(&h, &id, "a", "PARCEL-ALPHA");
    let (output, stream, code) = expect_delivery(common::read_response(&mut waiter).await);
    assert!(output.contains("PARCEL-ALPHA"));
    assert_eq!(stream, DeliveryStream::Stdout);
    assert_eq!(code, 7);
    assert_eq!(pending(&h, &id), 0, "a delivered parcel leaves the inbox");
    until("the finished wait to be dropped", || !has_waiter(&h, &id)).await;
}

#[tokio::test]
async fn the_default_wake_is_a_stderr_text_with_exit_code_two() {
    let (h, mut client) = start("inbox-i6-default", |_| {}).await;
    let id = h.open_idle_window(&mut client).await;
    let mut waiter = h.connect().await;
    common::send(&mut waiter, &wait_msg(&id)).await;
    until("the wait to be registered", || has_waiter(&h, &id)).await;
    deposit(&h, &id, "a", "PARCEL-ALPHA");
    let (_, stream, code) = expect_delivery(common::read_response(&mut waiter).await);
    assert_eq!(stream, DeliveryStream::Stderr);
    assert_eq!(code, 2);
}

#[tokio::test]
async fn a_wait_registered_with_a_pushable_parcel_returns_at_once() {
    let (h, mut client) = start("inbox-i6-now", |config| {
        let dir = config
            .detection_dir
            .as_deref()
            .expect("the configuration names a detection directory");
        common::write_turn_end_key_definition(dir);
    })
    .await;
    let id = h.open_idle_window(&mut client).await;
    deposit(&h, &id, "a", "PARCEL-ALPHA");
    let (output, _, code) = expect_delivery(read(&h, &wait_msg(&id)).await);
    assert!(output.contains("PARCEL-ALPHA"));
    assert_eq!(code, 2);
    assert!(!has_waiter(&h, &id));

    // The parcel wakes the CLI once the door prints it, which ends the hold a press of the
    // turn-end key left, so the next running report opens the turn again.
    let pressed = h
        .open(
            yagura_test_support::process::echo_child_command(),
            Some(common::TURN_END_KEY_DETECTION),
        )
        .await;
    let entry = h.registry.get(&pressed).expect("window exists");
    common::open_turn_after_the_key_was_named(&entry).await;
    common::interrupt_with_the_turn_end_key(&entry).await;
    deposit(&h, &pressed, "a", "PARCEL-BETA");
    let mut published = entry.subscribe_status();
    let mut door = h.connect().await;
    let (output, _, _) = expect_delivery(call_door(&mut door, &wait_msg(&pressed)).await);
    assert!(output.contains("PARCEL-BETA"));
    common::send(&mut door, &printed()).await;
    // On the door's own connection, so the daemon takes it after the notice.
    common::report_status(&mut door, &pressed, WindowStatus::Running).await;
    entry
        .session()
        .write_input(common::TURN_END_KEY_MARK)
        .await
        .expect("write the mark");
    let before_the_mark =
        common::published_until(&mut published, WindowStatus::WaitingApproval).await;
    assert!(
        before_the_mark.contains(&WindowStatus::Running),
        "a wait's door that printed what woke the CLI must end the press's hold, so the \
         running report after it opens the turn; published {before_the_mark:?}"
    );
}

#[tokio::test]
async fn the_newest_wait_wins_and_the_old_one_ends_silently() {
    let (h, mut client) = start("inbox-i6-lww", |_| {}).await;
    let id = h.open_idle_window(&mut client).await;
    let mut old = h.connect().await;
    common::send(&mut old, &wait_msg(&id)).await;
    until("the first wait", || has_waiter(&h, &id)).await;

    let mut newer = h.connect().await;
    common::send(&mut newer, &wait_msg(&id)).await;
    let (output, _, code) = expect_delivery(common::read_response(&mut old).await);
    assert!(output.is_empty(), "the superseded wait prints nothing");
    assert_eq!(code, 0, "and exits 0");
    assert!(has_waiter(&h, &id), "the newer wait stays");

    deposit(&h, &id, "a", "PARCEL-ALPHA");
    let (output, _, code) = expect_delivery(common::read_response(&mut newer).await);
    assert!(output.contains("PARCEL-ALPHA"));
    assert_eq!(code, 2);
}

#[tokio::test]
async fn a_wait_whose_connection_closes_is_dropped() {
    let (h, mut client) = start("inbox-i7", |_| {}).await;
    let id = h.open_idle_window(&mut client).await;
    {
        let mut waiter = h.connect().await;
        common::send(&mut waiter, &wait_msg(&id)).await;
        until("the wait to be registered", || has_waiter(&h, &id)).await;
    }
    until("the closed wait to be dropped", || !has_waiter(&h, &id)).await;
    // A parcel arriving now simply waits in the inbox.
    deposit(&h, &id, "a", "PARCEL-ALPHA");
    assert_eq!(pending(&h, &id), 1);
}

// --- report_session ---------------------------------------------------------

fn report(window_id: &str, token: &str, session: &str) -> ClientMessage {
    ClientMessage::ReportSession {
        window_id: window_id.into(),
        token: token.into(),
        session: session.into(),
    }
}

fn held(msg: DaemonMessage) -> bool {
    match msg {
        DaemonMessage::SessionRecorded { held } => held,
        other => panic!("expected session_recorded, got {other:?}"),
    }
}

#[tokio::test]
async fn the_daemon_keeps_only_a_session_value_of_the_allowed_shape() {
    let (h, mut client) = start("inbox-i8-shape", |_| {}).await;
    let id = h.open_idle_window(&mut client).await;
    let entry = h.registry.get(&id).unwrap();

    let longest = "a".repeat(128);
    for good in [
        "abc-123",
        "A_b.c:d",
        "019f-aaaa-bbbb",
        "x",
        longest.as_str(),
    ] {
        assert!(held(
            call_door(&mut client, &report(&id, VALID_TOKEN, good)).await
        ));
        assert_eq!(entry.session_id().as_deref(), Some(good));
    }
    let too_long = "a".repeat(129);
    for bad in [
        "",
        "-leading-dash",
        "has space",
        "slash/inside",
        "quote\"inside",
        "semi;colon",
        "line\nbreak",
        "日本語",
        too_long.as_str(),
    ] {
        assert!(
            !held(call_door(&mut client, &report(&id, VALID_TOKEN, bad)).await),
            "{bad:?} must not be kept"
        );
        assert_eq!(
            entry.session_id().as_deref(),
            Some(longest.as_str()),
            "a refused value leaves the held one alone ({bad:?})"
        );
    }
}

#[tokio::test]
async fn a_session_report_with_a_wrong_token_is_refused() {
    let (h, mut client) = start("inbox-i8-auth", |config| {
        config.status_token_override = None;
    })
    .await;
    let id = h
        .open(vec![echo_child_path().to_str().unwrap().into()], None)
        .await;
    expect_error(
        call_door(&mut client, &report(&id, VALID_TOKEN, "abc")).await,
        "auth_failed",
    );
    assert_eq!(h.registry.get(&id).unwrap().session_id(), None);
}

#[tokio::test]
#[traced_test]
async fn the_daemon_does_not_log_a_session_value() {
    let (h, mut client) = start("inbox-i8-log", |_| {}).await;
    let id = h.open_idle_window(&mut client).await;
    // The capture works for the daemon's own warnings: a foreign token is logged.
    let foreign = ClientMessage::StatusEvent {
        source: "test-report".into(),
        status: WindowStatus::Idle,
        window_id: Some(id.clone()),
        token: "00000000000000000000000000000000".into(),
    };
    common::send(&mut client, &foreign).await;
    let _ = common::read_response(&mut client).await;
    assert!(logs_contain("rejected a status_event"));

    assert!(held(
        call_door(
            &mut client,
            &report(&id, VALID_TOKEN, "SESSION-SECRET-VALUE")
        )
        .await
    ));
    assert!(!held(
        call_door(
            &mut client,
            &report(&id, VALID_TOKEN, "bad SESSION-SECRET-OTHER")
        )
        .await
    ));
    assert!(!logs_contain("SESSION-SECRET-VALUE"));
    assert!(!logs_contain("SESSION-SECRET-OTHER"));
}

// --- the json form ----------------------------------------------------------

/// A form given by one string: every door prints the same way.
fn every_door(form: OutputForm) -> DoorOutputs {
    DoorOutputs::all(&form)
}

const AGY_SHAPE: &str = r#"{"injectSteps":[{"systemMessage":{"systemMessage":{text}}}]}"#;

#[tokio::test]
async fn a_json_form_embeds_the_text_as_one_escaped_json_string_substituted_once() {
    let (h, mut client) = start("inbox-i9", |config| {
        config.delivery = DeliverySpec {
            output: every_door(OutputForm::Template(AGY_SHAPE.into())),
            ..DeliverySpec::default()
        };
    })
    .await;
    let id = h.open_idle_window(&mut client).await;
    let text = "say \"hi\"\nnext\tline \\ back \u{1} ctrl {text} {session} 日本語";
    deposit(&h, &id, "a", text);

    let output = take(&h, &id).await;
    let value: serde_json::Value =
        serde_json::from_str(&output).unwrap_or_else(|err| panic!("not JSON ({err}): {output:?}"));
    let embedded = value["injectSteps"][0]["systemMessage"]["systemMessage"]
        .as_str()
        .expect("the text is a JSON string");
    assert!(embedded.contains(text), "{embedded:?}");
    assert!(!output.contains('\u{1}'), "control characters are escaped");
    assert!(!output.contains('\n'), "newlines are escaped");
    assert_eq!(
        embedded.matches("{text}").count(),
        1,
        "the value is not scanned for placeholders again"
    );
    assert_eq!(embedded.matches("{session}").count(), 1);
}

#[tokio::test]
async fn a_json_form_prints_nothing_when_there_is_nothing_to_deliver() {
    let (h, mut client) = start("inbox-i9-empty", |config| {
        config.chat_intro = String::new();
        config.delivery = DeliverySpec {
            output: every_door(OutputForm::Template(AGY_SHAPE.into())),
            ..DeliverySpec::default()
        };
    })
    .await;
    let id = h.open_idle_window(&mut client).await;
    assert!(take(&h, &id).await.is_empty());
    let (context, _, _) = expect_delivery(read(&h, &context_msg(&id)).await);
    assert!(context.is_empty());
}

// --- a form for each door ---------------------------------------------------

/// The door a JSON answer names and the text it carries.
fn door_and_text(output: &str) -> (String, String) {
    let value: serde_json::Value =
        serde_json::from_str(output).unwrap_or_else(|err| panic!("not JSON ({err}): {output:?}"));
    let door = value["door"]
        .as_str()
        .expect("the door is named")
        .to_string();
    let text = value["t"]
        .as_str()
        .expect("the text is a JSON string")
        .to_string();
    (door, text)
}

#[tokio::test]
async fn each_door_prints_in_its_own_form_and_prints_nothing_when_nothing_waits() {
    let (h, mut client) = start("inbox-door-forms", |config| {
        config.chat_intro = String::new();
        let mut output = DoorOutputs::all(&OutputForm::Plain);
        output.context = OutputForm::Template(r#"{"door":"context","t":{text}}"#.into());
        output.take = OutputForm::Template(r#"{"door":"take","t":{text}}"#.into());
        output.wait = OutputForm::Template(r#"{"door":"wait","t":{text}}"#.into());
        config.delivery = DeliverySpec {
            output,
            ..DeliverySpec::default()
        };
    })
    .await;
    let id = h.open_idle_window(&mut client).await;

    let (empty_context, _, _) = expect_delivery(read(&h, &context_msg(&id)).await);
    assert!(empty_context.is_empty(), "{empty_context:?}");
    assert!(take(&h, &id).await.is_empty());

    deposit(&h, &id, "a", "PARCEL-CONTEXT");
    let (context, _, _) = expect_delivery(read(&h, &context_msg(&id)).await);
    let (door, text) = door_and_text(&context);
    assert_eq!(door, "context");
    assert!(text.contains("PARCEL-CONTEXT"), "{text:?}");

    deposit(&h, &id, "b", "PARCEL-TAKE");
    let (door, text) = door_and_text(&take(&h, &id).await);
    assert_eq!(door, "take");
    assert!(text.contains("PARCEL-TAKE"), "{text:?}");

    deposit(&h, &id, "c", "PARCEL-WAIT");
    let (woken, _, _) = expect_delivery(read(&h, &wait_msg(&id)).await);
    let (door, text) = door_and_text(&woken);
    assert_eq!(door, "wait");
    assert!(text.contains("PARCEL-WAIT"), "{text:?}");
}

// --- the inbox limit --------------------------------------------------------

#[tokio::test]
async fn an_inbox_over_its_limit_turns_the_new_parcel_away_and_says_which() {
    let (h, mut client) = start("inbox-i10", |config| {
        config.inbox_capacity = 3;
    })
    .await;
    let id = h.open_idle_window(&mut client).await;
    let mut dropped = Vec::new();
    for n in 0..5 {
        for parcel in deposit(&h, &id, &format!("k{n}"), &format!("PARCEL-{n}")) {
            dropped.push(parcel.key);
        }
    }
    assert_eq!(dropped, ["k3", "k4"]);
    assert_eq!(pending(&h, &id), 3);
    let got = take(&h, &id).await;
    assert!(!got.contains("PARCEL-3") && !got.contains("PARCEL-4"));
    assert!(pos(&got, "PARCEL-0") < pos(&got, "PARCEL-1"));
    assert!(pos(&got, "PARCEL-1") < pos(&got, "PARCEL-2"));
}

#[tokio::test]
async fn a_notice_is_not_pushed_out_by_the_parcels_that_fill_the_inbox() {
    let (h, mut client) = start("inbox-i10-notice-kept", |config| {
        config.inbox_capacity = 3;
    })
    .await;
    let id = h.open_idle_window(&mut client).await;
    let inbox = h.registry.get(&id).expect("window exists").inbox().clone();
    assert!(
        inbox
            .deposit(Parcel::notice("report", "NOTICE-REPORT"))
            .is_empty()
    );
    for n in 0..5 {
        deposit(&h, &id, &format!("k{n}"), &format!("PARCEL-{n}"));
    }
    assert!(
        inbox.holds("report"),
        "the notice stays however full the inbox gets"
    );
    let got = take(&h, &id).await;
    assert!(got.contains("NOTICE-REPORT"), "{got:?}");
}

#[tokio::test]
async fn a_notice_is_taken_in_by_an_inbox_full_of_other_parcels() {
    let (h, mut client) = start("inbox-i10-notice-room", |config| {
        config.inbox_capacity = 2;
    })
    .await;
    let id = h.open_idle_window(&mut client).await;
    for n in 0..2 {
        assert!(deposit(&h, &id, &format!("k{n}"), &format!("PARCEL-{n}")).is_empty());
    }
    let inbox = h.registry.get(&id).expect("window exists").inbox().clone();
    assert!(
        inbox
            .deposit(Parcel::notice("plan:p1", "NOTICE-PLAN"))
            .is_empty()
    );
    assert_eq!(pending(&h, &id), 3);
    let got = take(&h, &id).await;
    assert!(got.contains("PARCEL-0") && got.contains("PARCEL-1") && got.contains("NOTICE-PLAN"));
}

#[tokio::test]
async fn a_wait_on_an_inbox_that_has_ended_is_answered_at_once_and_not_registered() {
    let (h, mut client) = start("inbox-closed-wait", |_| {}).await;
    let id = h.open_idle_window(&mut client).await;
    let inbox = h.registry.get(&id).expect("window exists").inbox().clone();
    inbox.close(yagura_daemon::inbox::ParcelEnd::Closed);
    assert!(matches!(
        inbox.wait(),
        yagura_daemon::inbox::Wait::Answered(_)
    ));
    assert!(!inbox.has_waiter());
}

#[tokio::test]
async fn the_default_inbox_holds_sixty_four() {
    assert_eq!(DaemonConfig::default().inbox_capacity, 64);
    let (h, mut client) = start("inbox-i10-default", |_| {}).await;
    let id = h.open_idle_window(&mut client).await;
    let mut dropped = 0;
    for n in 0..65 {
        dropped += deposit(&h, &id, &format!("k{n}"), "x").len();
    }
    assert_eq!(dropped, 1);
    assert_eq!(pending(&h, &id), 64);
}

#[tokio::test]
async fn a_notice_deposited_again_replaces_the_one_waiting_under_its_key() {
    let (h, mut client) = start("inbox-notice-replaced", |_| {}).await;
    let id = h.open_idle_window(&mut client).await;
    let inbox = h.registry.get(&id).expect("window exists").inbox().clone();
    let first = inbox.deposit_tracked(Parcel::notice("report", "NOTICE-OLD"));
    let mut first_end = first.end.expect("the first notice is taken in");
    let second = inbox.deposit_tracked(Parcel::notice("report", "NOTICE-NEW"));
    assert!(second.turned_away.is_empty());
    assert!(second.end.is_some());

    assert_eq!(
        pending(&h, &id),
        1,
        "one notice per key, however often deposited"
    );
    assert!(
        matches!(
            first_end.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Closed)
        ),
        "the replaced notice's owner is left unsettled, as for a withdrawn parcel"
    );
    let got = take(&h, &id).await;
    assert!(
        got.contains("NOTICE-NEW") && !got.contains("NOTICE-OLD"),
        "{got:?}"
    );
}

#[tokio::test]
async fn a_replacing_notice_is_the_one_whose_owner_learns_it_was_taken() {
    let (h, mut client) = start("inbox-notice-replaced-taken", |_| {}).await;
    let id = h.open_idle_window(&mut client).await;
    let inbox = h.registry.get(&id).expect("window exists").inbox().clone();
    let _ = inbox.deposit_tracked(Parcel::notice("plan:p1", "NOTICE-OLD"));
    let end = inbox
        .deposit_tracked(Parcel::notice("plan:p1", "NOTICE-NEW"))
        .end
        .expect("taken in");
    let _ = take(&h, &id).await;
    assert_eq!(end.await, Ok(yagura_daemon::inbox::ParcelEnd::Taken));
}

#[tokio::test]
async fn a_notice_does_not_replace_a_parcel_that_merely_shares_its_key() {
    let (h, mut client) = start("inbox-notice-not-replacing-parcel", |_| {}).await;
    let id = h.open_idle_window(&mut client).await;
    let inbox = h.registry.get(&id).expect("window exists").inbox().clone();
    assert!(deposit(&h, &id, "same", "PARCEL").is_empty());
    assert!(inbox.deposit(Parcel::notice("same", "NOTICE")).is_empty());
    assert_eq!(pending(&h, &id), 2);
}

// --- withdrawing: only what still waits in the inbox ------------------------

fn output_of(msg: DaemonMessage) -> String {
    expect_delivery(msg).0
}

#[tokio::test]
async fn withdrawing_a_waiting_parcel_removes_it_and_nothing_else() {
    use yagura_daemon::inbox::Withdrawal;

    let (h, mut client) = start("inbox-withdraw-waiting", |_| {}).await;
    let id = h.open_idle_window(&mut client).await;
    let inbox = h.registry.get(&id).expect("window exists").inbox().clone();
    deposit(&h, &id, "gone", "PARCEL-GONE");
    deposit(&h, &id, "stays", "PARCEL-STAYS");

    assert!(matches!(inbox.withdraw("gone"), Withdrawal::Removed));
    assert!(
        matches!(inbox.withdraw("gone"), Withdrawal::Gone),
        "a parcel that is no longer waiting is not withdrawn twice"
    );
    assert!(matches!(
        inbox.withdraw("never-deposited"),
        Withdrawal::Gone
    ));

    let got = take(&h, &id).await;
    assert!(!got.contains("PARCEL-GONE"), "withdrawn: {got:?}");
    assert!(got.contains("PARCEL-STAYS"));
}

/// A door's read takes the parcel out of the inbox: withdrawing it then leaves it to the
/// door, whose printing ends it as taken.
#[tokio::test]
async fn withdrawing_a_parcel_a_door_holds_leaves_it_to_the_door_that_prints_it() {
    use yagura_daemon::inbox::{ParcelEnd, Withdrawal};

    let (h, mut client) = start("inbox-withdraw-door-held", |_| {}).await;
    let id = h.open_idle_window(&mut client).await;
    let inbox = h.registry.get(&id).expect("window exists").inbox().clone();
    let deposited = inbox.deposit_tracked(Parcel::new("held", "PARCEL-HELD"));
    let end = deposited.end.expect("the inbox must take the parcel in");
    let read = inbox.read_take(None);
    let (output, _, _) = expect_delivery(read.message.clone());
    assert!(output.contains("PARCEL-HELD"), "{output:?}");

    let withdrawal = inbox.withdraw("held");
    assert!(
        matches!(withdrawal, Withdrawal::UnderWay { .. }),
        "a parcel a door holds is not the inbox's to pull back: {withdrawal:?}"
    );
    let _ = read.hold.confirm().await;
    assert_eq!(
        tokio::time::timeout(yagura_test_support::HANG_GUARD, end)
            .await
            .expect("the depositor must learn how the parcel ended"),
        Ok(ParcelEnd::Taken),
        "the door printed it, so it was taken"
    );
    assert!(!inbox.holds("held"), "a printed parcel does not come back");
}

/// A door that ends without printing hands its parcel back to where it was, ahead of what
/// came after it; from there it can be withdrawn.
#[tokio::test]
async fn a_parcel_a_door_ends_without_printing_goes_back_to_its_place_and_can_then_be_withdrawn() {
    use yagura_daemon::inbox::Withdrawal;

    let (h, mut client) = start("inbox-withdraw-door-back", |_| {}).await;
    let id = h.open_idle_window(&mut client).await;
    let inbox = h.registry.get(&id).expect("window exists").inbox().clone();
    deposit(&h, &id, "first", "PARCEL-FIRST");
    let read = inbox.read_take(None);
    deposit(&h, &id, "second", "PARCEL-SECOND");

    assert!(
        matches!(inbox.withdraw("first"), Withdrawal::UnderWay { .. }),
        "a parcel a door holds is not the inbox's to pull back"
    );
    drop(read);
    assert!(inbox.holds("first"), "an unprinted parcel comes back");
    let again = inbox.read_take(None);
    let both = output_of(again.message.clone());
    assert!(
        pos(&both, "PARCEL-FIRST") < pos(&both, "PARCEL-SECOND"),
        "it comes back ahead of what came after it: {both:?}"
    );
    drop(again);

    assert!(
        matches!(inbox.withdraw("first"), Withdrawal::Removed),
        "a parcel that came back waits, so it can be withdrawn"
    );
    assert!(!inbox.holds("first"));
    assert!(inbox.holds("second"));
}

/// A parcel a wake command carries ends with that command: a command that succeeds took
/// it, and one that fails puts it back in front of the inbox even after a withdrawal,
/// where it can then be withdrawn.
#[tokio::test]
async fn withdrawing_a_parcel_a_wake_command_carries_leaves_it_to_the_command() {
    use yagura_daemon::inbox::{ParcelEnd, PushEnd, Withdrawal};

    let (h, mut client) = start("inbox-withdraw-carried", |_| {}).await;
    let id = h.open_idle_window(&mut client).await;
    let inbox = h.registry.get(&id).expect("window exists").inbox().clone();

    let taken = inbox.deposit_tracked(Parcel::new("taken", "PARCEL-TAKEN"));
    let taken_end = taken.end.expect("the inbox must take the parcel in");
    let push = inbox.begin_push().expect("a push picks the parcel up");
    assert!(matches!(
        inbox.withdraw("taken"),
        Withdrawal::UnderWay { .. }
    ));
    inbox.finish_push(push.id, PushEnd::Taken);
    assert_eq!(taken_end.await, Ok(ParcelEnd::Taken));

    deposit(&h, &id, "failed", "PARCEL-FAILED");
    let push = inbox.begin_push().expect("a push picks the parcel up");
    assert!(matches!(
        inbox.withdraw("failed"),
        Withdrawal::UnderWay { .. }
    ));
    inbox.finish_push(push.id, PushEnd::Failed);
    assert!(
        inbox.holds("failed"),
        "a command that failed puts its parcel back, withdrawn or not"
    );
    assert_eq!(pending(&h, &id), 1);
    assert!(matches!(inbox.withdraw("failed"), Withdrawal::Removed));
    assert_eq!(pending(&h, &id), 0);
}

#[tokio::test]
async fn withdrawing_a_waiting_parcel_does_not_disturb_another_that_a_push_carries() {
    use yagura_daemon::inbox::{PushEnd, Withdrawal};

    let (h, mut client) = start("inbox-withdraw-waiting-sibling", |_| {}).await;
    let id = h.open_idle_window(&mut client).await;
    let inbox = h.registry.get(&id).expect("window exists").inbox().clone();
    deposit(&h, &id, "under-way", "PARCEL-UNDER-WAY");
    let push = inbox
        .begin_push()
        .expect("a push must pick the waiting parcel up");
    deposit(&h, &id, "waiting", "PARCEL-WAITING");

    assert!(matches!(inbox.withdraw("waiting"), Withdrawal::Removed));

    // The push fails: what it carried goes back, because nothing withdrew it.
    inbox.finish_push(push.id, PushEnd::Failed);
    assert_eq!(pending(&h, &id), 1);
    assert!(inbox.holds("under-way"));
}

// --- the reads of doors before revision 34 -------------------------------------

/// A door before revision 34 settles or confirms before it prints, so letting it take
/// would lose what it took. It is answered with nothing to print, takes nothing, and its
/// confirmation is answered as withdrawn, which such a door ends on with 0.
#[tokio::test]
async fn an_older_doors_read_takes_nothing_and_its_confirmation_is_answered_as_withdrawn() {
    let (h, mut client) = start("inbox-older-door", |_| {}).await;
    let id = h.open_idle_window(&mut client).await;
    deposit(&h, &id, "a", "PARCEL-ALPHA");
    let (window_id, token) = (id.clone(), VALID_TOKEN.to_string());
    for held in [false, true] {
        for msg in [
            ClientMessage::DeliverContext {
                window_id: window_id.clone(),
                token: token.clone(),
                held,
            },
            ClientMessage::DeliverTake {
                window_id: window_id.clone(),
                token: token.clone(),
                held,
            },
            ClientMessage::DeliverWait {
                window_id: window_id.clone(),
                token: token.clone(),
                held,
            },
        ] {
            let mut door = h.connect().await;
            let (output, _, code) = expect_delivery(call_door(&mut door, &msg).await);
            assert!(
                output.is_empty() && code == 0,
                "{msg:?} must be answered with nothing to print: {output:?} / {code}"
            );
            if held {
                match call_door(&mut door, &ClientMessage::DeliverDone).await {
                    DaemonMessage::DeliverConfirmed { withdrawn } => assert!(
                        withdrawn,
                        "{msg:?}: its confirmation must say there is nothing to print"
                    ),
                    other => panic!("{msg:?}: expected deliver_confirmed, got {other:?}"),
                }
            }
            assert_eq!(pending(&h, &id), 1, "{msg:?} took the parcel");
            assert!(!has_waiter(&h, &id), "{msg:?} left a wait registered");
        }
    }
    assert!(take(&h, &id).await.contains("PARCEL-ALPHA"));
}

// --- parcels are shown once they are written, in the order they were deposited -----

/// Releases an armed fault when dropped, so a failed case does not leave the parcel
/// writer stopped behind it.
struct ReleaseOnDrop(FaultPoints);

impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        self.0.release();
    }
}

/// A daemon whose kept parcels are written to a database of its own, with `point`
/// armed on the writer, and a window opened after that so its inbox keeps its parcels.
async fn start_keeping(
    tag: &str,
    point: FaultPoint,
) -> (Harness, Connection, String, ReleaseOnDrop) {
    let faults = FaultPoints::arm(point);
    let guard = ReleaseOnDrop(faults.clone());
    let db = unique_temp_dir(tag).join("yagura.db");
    let (h, mut client) = start(tag, |config| config.faults = faults.clone()).await;
    store::wire_into(&h.registry, Some(db));
    let id = h.open_idle_window(&mut client).await;
    (h, client, id, guard)
}

/// Returns once the writer has stopped (or failed) at the armed point.
async fn writer_reached(faults: &FaultPoints) {
    tokio::time::timeout(yagura_test_support::HANG_GUARD, faults.reached())
        .await
        .expect("the parcel writer never reached the armed point");
}

/// Parcels deposited while the parcel writer is stopped are shown to no door until their
/// rows are written, and then in the order they were deposited.
#[tokio::test]
async fn parcels_are_shown_only_once_written_and_in_the_order_deposited() {
    let (h, _client, id, guard) = start_keeping("inbox-r15-paused", FaultPoint::WriterPaused).await;
    deposit(&h, &id, "first", "PARCEL-FIRST");
    deposit(&h, &id, "second", "PARCEL-SECOND");
    writer_reached(&guard.0).await;

    assert_eq!(
        pending(&h, &id),
        0,
        "a parcel whose row is not written yet must not be shown"
    );
    let early = take(&h, &id).await;
    assert!(
        early.is_empty(),
        "nothing is handed out unwritten: {early:?}"
    );

    guard.0.release();
    until("both parcels to be shown", || pending(&h, &id) == 2).await;
    let got = take(&h, &id).await;
    assert!(
        pos(&got, "PARCEL-FIRST") < pos(&got, "PARCEL-SECOND"),
        "parcels are shown in the order they were deposited: {got:?}"
    );
}

/// A parcel whose row could not be written never shows, and does not hold back the parcel
/// deposited after it.
#[tokio::test]
async fn a_parcel_that_could_not_be_written_never_shows_and_holds_nothing_back() {
    let (h, _client, id, guard) = start_keeping("inbox-r15-fails", FaultPoint::WriteFails).await;
    deposit(&h, &id, "doomed", "PARCEL-DOOMED");
    writer_reached(&guard.0).await;

    guard.0.release();
    deposit(&h, &id, "later", "PARCEL-LATER");
    let inbox = h.registry.get(&id).expect("window exists").inbox().clone();
    until("the later parcel to be shown", || inbox.holds("later")).await;

    assert!(
        !inbox.holds("doomed"),
        "a parcel whose row was not written must not be shown"
    );
    let got = take(&h, &id).await;
    assert!(got.contains("PARCEL-LATER"), "{got:?}");
    assert!(!got.contains("PARCEL-DOOMED"), "{got:?}");
}

// --- asking a CLI with no wait to wait again --------------------------------------

/// The definition the wake cases run under: `CTRL-READY` reads as idle, a `REACT` line as
/// running and a `GONE` line as unknown, so a test changes the window's status without any
/// door. Five bottom lines: blank lines push an older mark out of reach.
const WAKE_ASK_DETECTION: &str = "wake-ask";

/// Writes [`WAKE_ASK_DETECTION`] beside the control definition and lets a send through.
fn wake_ask_config(config: &mut DaemonConfig) {
    let dir = config
        .detection_dir
        .clone()
        .expect("the wake cases read definitions from a directory");
    std::fs::write(
        dir.join(format!("{WAKE_ASK_DETECTION}.toml")),
        yagura_test_support::detection::idle_definition_toml(
            WAKE_ASK_DETECTION,
            "bottom_line_count = 5\n",
            "\n[[rules]]\nstatus = \"running\"\npattern = 'REACT'\npriority = 20\nregion = \"bottom\"\n\
             \n[[rules]]\nstatus = \"unknown\"\npattern = 'GONE'\npriority = 30\nregion = \"bottom\"\n",
        ),
    )
    .expect("write the wake definition");
    config.approval_policy = Some(common::policy(&["send_message"]));
}

/// What the wake file at `path` holds; `None` while there is none.
fn wake_text(path: &Path) -> Option<Vec<u8>> {
    std::fs::read(path).ok()
}

/// Waits until the wake file at `path` holds something other than `before` (`None`: until
/// there is one), and returns what it held then. The deadline only detects a hang.
pub(crate) async fn until_rewritten(path: &Path, before: Option<&[u8]>) -> Vec<u8> {
    let deadline = tokio::time::Instant::now() + yagura_test_support::HANG_GUARD;
    loop {
        let now = wake_text(path);
        if now.as_deref() != before
            && let Some(now) = now
        {
            return now;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "{} was not written again; it holds {:?}",
            path.display(),
            now.map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// [`until_rewritten`], then what the file holds once every write the daemon started has
/// ended: the next comparison never sees a write half done.
async fn asked_again(path: &Path, before: Option<&[u8]>) -> Vec<u8> {
    until_rewritten(path, before).await;
    common::let_every_clock_bound_run_out().await;
    let now = wake_text(path).expect("a wake file once written stays");
    assert_ne!(Some(now.as_slice()), before, "{}", path.display());
    now
}

/// Asserts the wake file at `path` still holds `held` once everything the daemon had to do
/// is done.
async fn asked_nothing_more(path: &Path, held: Option<&[u8]>, why: &str) {
    common::let_every_clock_bound_run_out().await;
    assert_eq!(wake_text(path).as_deref(), held, "{why}");
}

/// A `wake = "wait"` window whose CLI has no wait registered is asked to wait again: the
/// daemon writes the wake file its open named (and did not make) once a parcel can be
/// pushed. It asks once and waits for the CLI's answer: a parcel arriving meanwhile asks
/// nothing more, and nothing is asked while a wait is registered. A door that registers no
/// wait, a wait registered, and the window's status changing each answer the ask, after
/// which a parcel that can be pushed asks again, as one arriving after a wait ended
/// unanswered does. Each ask leaves the file holding something else.
#[tokio::test]
async fn a_wait_window_with_no_wait_is_asked_once_until_its_cli_answers() {
    let (h, mut client) = start("inbox-wake-ask", wake_ask_config).await;
    let root = unique_temp_dir("inbox-wake-ask-creds");
    let id = open_wake_window(&h, &root, "asked", WAKE_ASK_DETECTION, true).await;
    let wake = wake_file_of(&root, &id);
    h.write_input(&id, READY_MARKER).await;
    wait_until_status(&mut client, &id, WindowStatus::Idle).await;
    assert_eq!(wake_text(&wake), None, "the open makes no wake file");

    let one = send_queued(&mut client, &id, "ASK-ONE").await;
    let first = asked_again(&wake, None).await;
    let two = send_queued(&mut client, &id, "ASK-TWO").await;
    asked_nothing_more(
        &wake,
        Some(&first),
        "a parcel arriving while the ask stands asks nothing more",
    )
    .await;

    assert!(held(
        call_door(&mut client, &report(&id, VALID_TOKEN, "s-1")).await
    ));
    let second = asked_again(&wake, Some(&first)).await;

    for sent in [one, two] {
        assert_eq!(
            outcome_of(&withdraw(&mut client, &sent).await).as_deref(),
            Some("withdrawn")
        );
    }
    let mut waiter = register_wait(&h, &id).await;
    common::send(
        &mut client,
        &ClientMessage::SendText {
            window_id: id.clone(),
            text: "ASK-THREE".to_string(),
        },
    )
    .await;
    let (output, _, _) = expect_delivery(common::read_response(&mut waiter).await);
    assert!(output.contains("ASK-THREE"), "{output:?}");
    common::send(&mut waiter, &printed()).await;
    let answer = common::read_response(&mut client).await;
    assert_eq!(
        state_of(&answer).as_deref(),
        Some("delivered"),
        "{answer:?}"
    );
    asked_nothing_more(
        &wake,
        Some(&second),
        "a parcel handed to the registered wait asks nothing",
    )
    .await;
    send_queued(&mut client, &id, "ASK-FOUR").await;
    let third = asked_again(&wake, Some(&second)).await;

    assert!(take(&h, &id).await.contains("ASK-FOUR"));
    drop(register_wait(&h, &id).await);
    until("the wait that ended unanswered to be dropped", || {
        !has_waiter(&h, &id)
    })
    .await;
    send_queued(&mut client, &id, "ASK-FIVE").await;
    let fourth = asked_again(&wake, Some(&third)).await;

    h.write_input(&id, b"REACT\r").await;
    wait_until_status(&mut client, &id, WindowStatus::Running).await;
    until_rewritten(&wake, Some(&fourth)).await;
}

/// Nothing is asked of a window whose CLI has a wait registered once it can be pushed to,
/// of one that cannot be pushed to (`Unknown`, `Exited`) though the ask was answered and a
/// parcel waits, or of one with no wake file: what waits there waits for the CLI's next
/// event.
#[tokio::test]
async fn a_window_that_cannot_be_pushed_to_or_has_no_wake_file_is_never_asked() {
    let (h, mut client) = start("inbox-wake-never", wake_ask_config).await;
    let root = unique_temp_dir("inbox-wake-never-creds");
    let id = open_wake_window(&h, &root, "never", WAKE_ASK_DETECTION, true).await;
    let wake = wake_file_of(&root, &id);
    h.write_input(&id, READY_MARKER).await;
    wait_until_status(&mut client, &id, WindowStatus::Idle).await;

    let mut waiter = register_wait(&h, &id).await;
    common::send(
        &mut client,
        &ClientMessage::SendText {
            window_id: id.clone(),
            text: "HANDED-TO-THE-WAIT".to_string(),
        },
    )
    .await;
    let (output, _, _) = expect_delivery(common::read_response(&mut waiter).await);
    assert!(output.contains("HANDED-TO-THE-WAIT"), "{output:?}");
    common::send(&mut waiter, &printed()).await;
    let answer = common::read_response(&mut client).await;
    assert_eq!(
        state_of(&answer).as_deref(),
        Some("delivered"),
        "{answer:?}"
    );
    asked_nothing_more(
        &wake,
        None,
        "a parcel handed to the registered wait asks nothing",
    )
    .await;

    send_queued(&mut client, &id, "LEFT-WAITING").await;
    let first = asked_again(&wake, None).await;
    h.write_input(&id, b"GONE\r").await;
    wait_until_status(&mut client, &id, WindowStatus::Unknown).await;
    asked_nothing_more(
        &wake,
        Some(&first),
        "the status change answered the ask, but nothing is asked of an Unknown window",
    )
    .await;

    let mut waiter = register_wait(&h, &id).await;
    h.write_input(&id, b"\r\n\r\n\r\n\r\n\r\n").await;
    h.write_input(&id, READY_MARKER).await;
    let (output, _, _) = expect_delivery(common::read_response(&mut waiter).await);
    assert!(output.contains("LEFT-WAITING"), "{output:?}");
    common::send(&mut waiter, &printed()).await;
    until("the printed parcel to leave", || pending(&h, &id) == 0).await;
    asked_nothing_more(
        &wake,
        Some(&first),
        "a wait was registered when the window could be pushed to again",
    )
    .await;

    send_queued(&mut client, &id, "LEFT-AT-THE-END").await;
    let folder = credentials_folder_of(&root, &id).expect("a launch window has its folder");
    asked_again(&wake, Some(&first)).await;
    h.registry
        .get(&id)
        .expect("window exists")
        .session()
        .terminate();
    wait_until_status(&mut client, &id, WindowStatus::Exited).await;
    until_gone(&folder).await;
    common::let_every_clock_bound_run_out().await;
    assert!(
        !folder.exists(),
        "nothing is asked of an exited window, which would make its folder again"
    );

    let unasked = open_wake_window(&h, &root, "unasked", WAKE_ASK_DETECTION, false).await;
    let unasked_folder =
        credentials_folder_of(&root, &unasked).expect("a launch window has its folder");
    h.write_input(&unasked, READY_MARKER).await;
    wait_until_status(&mut client, &unasked, WindowStatus::Idle).await;
    send_queued(&mut client, &unasked, "NEVER-UNASKED").await;
    common::let_every_clock_bound_run_out().await;
    assert!(
        !unasked_folder.join(WAKE_FILE_NAME).exists(),
        "a window with no wake file is never asked"
    );
}
