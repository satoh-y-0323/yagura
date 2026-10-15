#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The two ways into a window's input: the plain one prefixes a registered
//! take-back before its own bytes, and the one that checks the write ledger
//! writes nothing once the ledger has moved.
//!
//! The child is `hex_echo`, which prints each byte it receives as two hex
//! digits, so the order and count of the bytes it got are read off the screen.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use yagura_daemon::input::InputProfile;
use yagura_daemon::pty::{GuardedWrite, TakeBackPrefix, TakeBackRegistration};
use yagura_daemon::{DaemonConfig, PtySession};
use yagura_test_support::process::hex_echo_path;

const CLEAR: u8 = 0x15;
const READY: &str = "CTRL-READY";
const RECEIVE_TIMEOUT: Duration = Duration::from_secs(20);
/// Written last, with no prefix, so that everything the child got before it is
/// known to have arrived and nothing extra can hide behind a comparison that
/// stops early.
const SENTINEL: &[u8] = b"Z";

struct Fixture {
    session: PtySession,
}

impl Fixture {
    async fn start() -> Self {
        let child = hex_echo_path();
        let session = PtySession::spawn_with_command(
            &[child.to_str().expect("hex_echo path must be UTF-8")],
            &DaemonConfig::default(),
        )
        .await
        .expect("spawn hex_echo");
        let fixture = Self { session };
        fixture.wait_until_ready().await;
        fixture
    }

    async fn wait_until_ready(&self) {
        let deadline = tokio::time::Instant::now() + RECEIVE_TIMEOUT;
        loop {
            let (text, _) = self.session.text_tail(200, 64 * 1024).await.unwrap();
            if text.contains(READY) {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "hex_echo never printed its ready marker"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Every byte the child received so far, as hex, read off the screen.
    async fn received_hex(&self) -> String {
        let (text, _) = self.session.text_tail(2000, 256 * 1024).await.unwrap();
        let after = text.split_once(READY).map_or("", |(_, rest)| rest);
        after
            .chars()
            .filter(|c| matches!(c, '0'..='9' | 'a'..='f'))
            .collect()
    }

    /// Writes the sentinel with no prefix, waits until the child has echoed
    /// everything up to it, and compares the whole stream with `expected`.
    async fn assert_child_received(&self, expected: &[&[u8]]) {
        self.session
            .write_input_with(SENTINEL, TakeBackPrefix::Withhold)
            .await
            .expect("write the sentinel");
        let want: String = expected
            .iter()
            .copied()
            .chain(std::iter::once(SENTINEL))
            .flat_map(|bytes| bytes.iter().map(|b| format!("{b:02x}")))
            .collect();
        let deadline = tokio::time::Instant::now() + RECEIVE_TIMEOUT;
        loop {
            let got = self.received_hex().await;
            if got.len() >= want.len() {
                assert_eq!(got, want, "the child received other bytes than expected");
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "the child never received everything: wanted {want}, has {got}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Registers `text` with the `clear` key through the ledger-checking
    /// write, which also writes `text`.
    async fn register(&self, text: &str) {
        let call = self.session.begin_take_back_call();
        let written = self
            .session
            .write_input_if_unmoved(
                text.as_bytes(),
                self.session.writes_recorded(),
                TakeBackRegistration::Set {
                    call,
                    text: text.to_string(),
                    clear: vec![CLEAR],
                },
                || {},
            )
            .await
            .expect("register the take-back");
        assert_eq!(written, GuardedWrite::Written);
    }

    /// The `clear` keys the profile's own planner asks for at the window's
    /// current width.
    fn clear_keys(&self, text: &str) -> Vec<u8> {
        let profile = InputProfile {
            clear: vec![CLEAR],
            ..InputProfile::default()
        };
        let cols = self.session.input_state().cols;
        let plan = profile.plan_take_back(text, cols);
        assert_eq!(
            plan.steps.len(),
            1,
            "a profile with a clear key plans one step"
        );
        plan.steps[0].bytes.clone()
    }
}

#[tokio::test]
async fn a_plain_write_after_a_registration_gets_the_take_back_in_front_of_its_bytes() {
    let fixture = Fixture::start().await;
    let text = "hello world";
    fixture.register(text).await;
    let keys = fixture.clear_keys(text);
    assert!(keys.len() > 1, "the take-back is more than one key");

    fixture.session.write_input(b"hi").await.unwrap();

    fixture
        .assert_child_received(&[text.as_bytes(), &keys, b"hi"])
        .await;
}

#[tokio::test]
async fn a_checked_write_with_a_stale_expectation_writes_nothing_and_counts_nothing() {
    let fixture = Fixture::start().await;
    let stale = fixture.session.writes_recorded();
    let text = "hello world";
    fixture.register(text).await;
    let recorded = fixture.session.writes_recorded();
    let called = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&called);

    let outcome = fixture
        .session
        .write_input_if_unmoved(b"late", stale, TakeBackRegistration::Keep, move || {
            flag.store(true, Ordering::SeqCst)
        })
        .await
        .unwrap();

    assert_eq!(outcome, GuardedWrite::Moved);
    assert_eq!(fixture.session.writes_recorded(), recorded);
    assert!(!called.load(Ordering::SeqCst), "before_write must not run");
    fixture.assert_child_received(&[text.as_bytes()]).await;
}

#[tokio::test]
async fn the_take_back_is_counted_at_the_width_the_window_has_when_the_write_happens() {
    let fixture = Fixture::start().await;
    let text = "x".repeat(200);
    fixture.register(&text).await;
    let wide = fixture.clear_keys(&text);
    fixture.session.resize(30, 24).await.unwrap();
    let narrow = fixture.clear_keys(&text);
    assert!(
        narrow.len() > wide.len(),
        "the width must change the count or this test measures nothing"
    );

    fixture.session.write_input(b"hi").await.unwrap();

    fixture
        .assert_child_received(&[text.as_bytes(), &narrow, b"hi"])
        .await;
}

#[tokio::test]
async fn a_plain_write_uses_the_registration_up() {
    let fixture = Fixture::start().await;
    let text = "hello world";
    fixture.register(text).await;
    let keys = fixture.clear_keys(text);

    fixture.session.write_input(b"one").await.unwrap();
    fixture.session.write_input(b"two").await.unwrap();

    fixture
        .assert_child_received(&[text.as_bytes(), &keys, b"one", b"two"])
        .await;
}

#[tokio::test]
async fn a_withheld_write_neither_prefixes_nor_leaves_a_registration_behind() {
    let fixture = Fixture::start().await;
    let text = "hello world";
    fixture.register(text).await;

    fixture
        .session
        .write_input_with(b"one", TakeBackPrefix::Withhold)
        .await
        .unwrap();
    fixture.session.write_input(b"two").await.unwrap();

    fixture
        .assert_child_received(&[text.as_bytes(), b"one", b"two"])
        .await;
}

#[tokio::test]
async fn a_registration_from_a_finished_call_is_not_placed() {
    let fixture = Fixture::start().await;
    let text = "hello world";
    let call = fixture.session.begin_take_back_call();
    fixture.session.end_take_back_call(call);

    let outcome = fixture
        .session
        .write_input_if_unmoved(
            text.as_bytes(),
            fixture.session.writes_recorded(),
            TakeBackRegistration::Set {
                call,
                text: text.to_string(),
                clear: vec![CLEAR],
            },
            || {},
        )
        .await
        .unwrap();
    assert_eq!(outcome, GuardedWrite::Written);
    fixture.session.write_input(b"hi").await.unwrap();

    fixture
        .assert_child_received(&[text.as_bytes(), b"hi"])
        .await;
}

#[tokio::test]
async fn ending_one_call_leaves_the_registration_of_another() {
    let fixture = Fixture::start().await;
    let text = "hello world";
    let finished = fixture.session.begin_take_back_call();
    fixture.register(text).await;
    let keys = fixture.clear_keys(text);

    fixture.session.end_take_back_call(finished);
    fixture.session.write_input(b"hi").await.unwrap();

    fixture
        .assert_child_received(&[text.as_bytes(), &keys, b"hi"])
        .await;
}

#[tokio::test]
async fn an_empty_registration_update_removes_the_take_back() {
    let fixture = Fixture::start().await;
    let text = "hello world";
    fixture.register(text).await;
    let outcome = fixture
        .session
        .write_input_if_unmoved(
            b"mid",
            fixture.session.writes_recorded(),
            TakeBackRegistration::Empty,
            || {},
        )
        .await
        .unwrap();
    assert_eq!(outcome, GuardedWrite::Written);

    fixture.session.write_input(b"hi").await.unwrap();

    fixture
        .assert_child_received(&[text.as_bytes(), b"mid", b"hi"])
        .await;
}

#[tokio::test]
async fn a_keep_registration_update_leaves_the_previous_take_back_in_place() {
    let fixture = Fixture::start().await;
    let text = "hello world";
    fixture.register(text).await;
    let keys = fixture.clear_keys(text);
    let outcome = fixture
        .session
        .write_input_if_unmoved(
            b"mid",
            fixture.session.writes_recorded(),
            TakeBackRegistration::Keep,
            || {},
        )
        .await
        .unwrap();
    assert_eq!(outcome, GuardedWrite::Written);

    fixture.session.write_input(b"hi").await.unwrap();

    fixture
        .assert_child_received(&[text.as_bytes(), b"mid", &keys, b"hi"])
        .await;
}
