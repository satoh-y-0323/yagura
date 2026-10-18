// Real windows, a real IPC endpoint, the daemon's own server: the properties are what the
// doors a CLI's hook reads from return, and what the daemon keeps.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: a window's inbox and the doors that read it (`deliver_context`,
//! `deliver_take`, `deliver_wait`, `report_session`).
//!
//! The inbox is filled through `WindowEntry::inbox().deposit(..)`; nothing in this layer
//! fills it over IPC. Parcels are read back only through the doors, as a hook does.

mod common;

use std::time::Duration;

use tracing_test::traced_test;
use yagura_daemon::DaemonConfig;
use yagura_daemon::inbox::{DeliverySpec, OutputForm, Parcel};
use yagura_protocol::{ClientMessage, DaemonMessage, DeliveryStream, WindowStatus};
use yagura_test_support::ipc::Connection;

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

async fn start(tag: &str, tweak: impl FnOnce(&mut DaemonConfig)) -> (Harness, Connection) {
    let h = Harness::start(tag, config(tag, tweak)).await;
    let client = h.connect().await;
    (h, client)
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
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while !condition() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn take_msg(window_id: &str) -> ClientMessage {
    ClientMessage::DeliverTake {
        window_id: window_id.into(),
        token: VALID_TOKEN.into(),
    }
}

fn context_msg(window_id: &str) -> ClientMessage {
    ClientMessage::DeliverContext {
        window_id: window_id.into(),
        token: VALID_TOKEN.into(),
    }
}

fn wait_msg(window_id: &str) -> ClientMessage {
    ClientMessage::DeliverWait {
        window_id: window_id.into(),
        token: VALID_TOKEN.into(),
    }
}

async fn take(client: &mut Connection, window_id: &str) -> String {
    expect_delivery(call_door(client, &take_msg(window_id)).await).0
}

fn pos(haystack: &str, needle: &str) -> usize {
    haystack
        .find(needle)
        .unwrap_or_else(|| panic!("{needle:?} missing from {haystack:?}"))
}

// --- I1 ---------------------------------------------------------------------

#[tokio::test]
async fn a_take_returns_the_windows_parcels_in_order_and_takes_them_out() {
    let (h, mut client) = start("inbox-i1", |_| {}).await;
    let first = h.open_idle_window(&mut client).await;
    let second = h.open_idle_window(&mut client).await;
    deposit(&h, &first, "a", "PARCEL-ALPHA");
    deposit(&h, &first, "b", "PARCEL-BETA");
    deposit(&h, &second, "c", "PARCEL-GAMMA");

    let got = take(&mut client, &first).await;
    assert!(pos(&got, "PARCEL-ALPHA") < pos(&got, "PARCEL-BETA"));
    assert!(!got.contains("PARCEL-GAMMA"), "another window's parcel");
    assert_eq!(pending(&h, &first), 0);

    let again = take(&mut client, &first).await;
    assert!(again.is_empty(), "a taken parcel returns once: {again:?}");

    let other = take(&mut client, &second).await;
    assert!(other.contains("PARCEL-GAMMA"));
    assert!(!other.contains("PARCEL-ALPHA"));
}

// --- I2 ---------------------------------------------------------------------

#[tokio::test]
async fn a_take_stays_within_the_limit_and_never_cuts_a_parcel() {
    let (h, mut client) = start("inbox-i2-ascii", |config| {
        config.delivery = DeliverySpec {
            max_chars: 100,
            ..DeliverySpec::default()
        };
    })
    .await;
    let id = h.open_idle_window(&mut client).await;
    let (a, b, c) = ("A".repeat(40), "B".repeat(40), "C".repeat(40));
    deposit(&h, &id, "a", &a);
    deposit(&h, &id, "b", &b);
    deposit(&h, &id, "c", &c);

    let first = take(&mut client, &id).await;
    assert!(first.chars().count() <= 100, "{}", first.chars().count());
    assert!(first.contains(&a), "the oldest parcel comes first, whole");
    assert!(!first.contains(&c), "three parcels of 40 do not fit in 100");
    let second = take(&mut client, &id).await;
    assert!(second.chars().count() <= 100);
    let all = format!("{first}{second}");
    for parcel in [&a, &b, &c] {
        assert_eq!(all.matches(parcel.as_str()).count(), 1, "exactly once");
    }
    assert!(second.contains(&c));
    assert_eq!(pending(&h, &id), 0);
}

#[tokio::test]
async fn the_limit_counts_characters_not_bytes() {
    let (h, mut client) = start("inbox-i2-chars", |config| {
        config.delivery = DeliverySpec {
            max_chars: 100,
            ..DeliverySpec::default()
        };
    })
    .await;
    let id = h.open_idle_window(&mut client).await;
    // 40 characters are 120 bytes: two of them fit in 100 characters, not in 100 bytes.
    let (a, b) = ("あ".repeat(40), "い".repeat(40));
    deposit(&h, &id, "a", &a);
    deposit(&h, &id, "b", &b);
    let got = take(&mut client, &id).await;
    assert!(got.contains(&a) && got.contains(&b), "{got:?}");
    assert!(got.chars().count() <= 100);
}

#[tokio::test]
async fn the_limit_includes_the_intro_in_the_context() {
    let intro = "I".repeat(30);
    let intro_for_config = intro.clone();
    let (h, mut client) = start("inbox-i2-intro", move |config| {
        config.chat_intro = intro_for_config;
        config.delivery = DeliverySpec {
            max_chars: 100,
            ..DeliverySpec::default()
        };
    })
    .await;
    let id = h.open_idle_window(&mut client).await;
    let (a, b) = ("A".repeat(40), "B".repeat(40));
    deposit(&h, &id, "a", &a);
    deposit(&h, &id, "b", &b);

    let (context, _, _) = expect_delivery(call_door(&mut client, &context_msg(&id)).await);
    assert!(context.contains(&intro) && context.contains(&a));
    assert!(!context.contains(&b), "30 + 40 + 40 does not fit in 100");
    assert!(context.chars().count() <= 100);
    assert!(
        take(&mut client, &id).await.contains(&b),
        "the rest follows"
    );
}

// --- I3 ---------------------------------------------------------------------

#[tokio::test]
async fn the_context_is_the_intro_and_the_pending_parcels_and_takes_them() {
    let (h, mut client) = start("inbox-i3", |config| {
        config.chat_intro = "WELCOME-INTRO".into();
    })
    .await;
    let id = h.open_idle_window(&mut client).await;
    deposit(&h, &id, "a", "PARCEL-ALPHA");

    let (context, stream, code) = expect_delivery(call_door(&mut client, &context_msg(&id)).await);
    assert!(pos(&context, "WELCOME-INTRO") < pos(&context, "PARCEL-ALPHA"));
    assert_eq!(stream, DeliveryStream::Stdout);
    assert_eq!(code, 0);
    assert_eq!(pending(&h, &id), 0);
    let next = take(&mut client, &id).await;
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
    let (nothing, _, code) = expect_delivery(call_door(&mut client, &context_msg(&id)).await);
    assert!(nothing.is_empty(), "no intro and no parcel: {nothing:?}");
    assert_eq!(code, 0);
    deposit(&h, &id, "a", "PARCEL-ALPHA");
    let (context, _, _) = expect_delivery(call_door(&mut client, &context_msg(&id)).await);
    assert!(context.contains("PARCEL-ALPHA"));
}

// --- I4 ---------------------------------------------------------------------

#[tokio::test]
async fn the_doors_a_cli_reads_answer_in_an_unknown_window() {
    let (h, mut client) = start("inbox-i4-reads", |config| {
        config.chat_intro = "WELCOME-INTRO".into();
    })
    .await;
    let id = unknown_window(&h, &mut client).await;
    deposit(&h, &id, "a", "PARCEL-ALPHA");
    let (context, _, _) = expect_delivery(call_door(&mut client, &context_msg(&id)).await);
    assert!(context.contains("WELCOME-INTRO") && context.contains("PARCEL-ALPHA"));
    deposit(&h, &id, "b", "PARCEL-BETA");
    assert!(take(&mut client, &id).await.contains("PARCEL-BETA"));
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
    wait_until_status(
        &mut client,
        &id,
        WindowStatus::Idle,
        Duration::from_secs(10),
    )
    .await;
    deposit(&h, &id, "c", "PARCEL-GAMMA");
    let (output, _, _) =
        expect_delivery(common::read_response(&mut waiter, Duration::from_secs(10)).await);
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
        for msg in [
            ClientMessage::DeliverTake {
                window_id: id.clone(),
                token: token.into(),
            },
            ClientMessage::DeliverContext {
                window_id: id.clone(),
                token: token.into(),
            },
            ClientMessage::DeliverWait {
                window_id: id.clone(),
                token: token.into(),
            },
        ] {
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

// --- I6 / I7 ----------------------------------------------------------------

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
    let (output, stream, code) =
        expect_delivery(common::read_response(&mut waiter, Duration::from_secs(10)).await);
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
    let (_, stream, code) =
        expect_delivery(common::read_response(&mut waiter, Duration::from_secs(10)).await);
    assert_eq!(stream, DeliveryStream::Stderr);
    assert_eq!(code, 2);
}

#[tokio::test]
async fn a_wait_registered_with_a_pushable_parcel_returns_at_once() {
    let (h, mut client) = start("inbox-i6-now", |_| {}).await;
    let id = h.open_idle_window(&mut client).await;
    deposit(&h, &id, "a", "PARCEL-ALPHA");
    let (output, _, code) = expect_delivery(call_door(&mut client, &wait_msg(&id)).await);
    assert!(output.contains("PARCEL-ALPHA"));
    assert_eq!(code, 2);
    assert!(!has_waiter(&h, &id));
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
    let (output, _, code) =
        expect_delivery(common::read_response(&mut old, Duration::from_secs(10)).await);
    assert!(output.is_empty(), "the superseded wait prints nothing");
    assert_eq!(code, 0, "and exits 0");
    assert!(has_waiter(&h, &id), "the newer wait stays");

    deposit(&h, &id, "a", "PARCEL-ALPHA");
    let (output, _, code) =
        expect_delivery(common::read_response(&mut newer, Duration::from_secs(10)).await);
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

// --- I8 ---------------------------------------------------------------------

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
    let _ = common::read_response(&mut client, Duration::from_secs(5)).await;
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

// --- I9 ---------------------------------------------------------------------

const AGY_SHAPE: &str = r#"{"injectSteps":[{"systemMessage":{"systemMessage":{text}}}]}"#;

#[tokio::test]
async fn a_json_form_embeds_the_text_as_one_escaped_json_string_substituted_once() {
    let (h, mut client) = start("inbox-i9", |config| {
        config.delivery = DeliverySpec {
            output: OutputForm::Template(AGY_SHAPE.into()),
            ..DeliverySpec::default()
        };
    })
    .await;
    let id = h.open_idle_window(&mut client).await;
    let text = "say \"hi\"\nnext\tline \\ back \u{1} ctrl {text} {session} 日本語";
    deposit(&h, &id, "a", text);

    let output = take(&mut client, &id).await;
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
            output: OutputForm::Template(AGY_SHAPE.into()),
            ..DeliverySpec::default()
        };
    })
    .await;
    let id = h.open_idle_window(&mut client).await;
    assert!(take(&mut client, &id).await.is_empty());
    let (context, _, _) = expect_delivery(call_door(&mut client, &context_msg(&id)).await);
    assert!(context.is_empty());
}

// --- I10 --------------------------------------------------------------------

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
    let got = take(&mut client, &id).await;
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
    let got = take(&mut client, &id).await;
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
    let got = take(&mut client, &id).await;
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
