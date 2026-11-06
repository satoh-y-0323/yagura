#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Composer-draw mode is reachable from the `yagura-mcp` crate through
//! `yagura-test-support::process` alone.
//!
//! `yagura-mcp`'s own tests cannot reach `yagura-daemon/tests/common/mod.rs`
//! (a test-only module private to that crate's own test binaries), so a
//! real `send_handoff` delivery test in this crate has no route to a
//! composer-drawing window other than the shared `yagura-test-support`
//! launcher. This file is the check that route is enough on its own: it
//! repeats `yagura-daemon/tests/composer_fixture.rs`'s single-write case
//! verbatim, built from this crate instead, using only
//! `yagura-test-support::process` plus `yagura-daemon`'s public library
//! surface (`PtySession`, `detector::reading`, `detector::definition` --
//! never `yagura-daemon/tests/common`, which this crate's `Cargo.toml` does
//! not even see). `submit_child` stands in for all three delivery-receiving
//! fixtures here: `yagura-daemon/tests/composer_fixture.rs` already covers
//! `echo_child` and `ready_child` from the crate that owns them, and this
//! file's only job is proving the shared launcher is reachable at all, not
//! re-covering per-fixture drawing behavior.
//!
//! The composer section is built through [`parse_definition`], not by
//! constructing `ComposerSection` directly: that type's `marker` field is a
//! `regex::Regex`, and this crate does not depend on the `regex` crate
//! (only `yagura-daemon` does), so going through the same TOML loader a real
//! detection definition uses is also the only route available here, not
//! merely the one that happens to avoid an extra dependency.

use std::time::Duration;

use yagura_daemon::detector::definition::{ComposerSection, parse_definition};
use yagura_daemon::detector::reading::{self, ComposerReadout};
use yagura_daemon::{DaemonConfig, PtySession};
use yagura_test_support::{HANG_GUARD, detection, process};

fn composer_section() -> ComposerSection {
    let source = format!(
        "schema_version = 1\nname = \"composer-fixture-mcp\"\nrules = []\n{}",
        detection::composer_section_toml(),
    );
    parse_definition(&source)
        .expect("the shared composer triple must parse as a valid [composer] table")
        .composer
        .expect("a definition with a [composer] table must load one")
}

/// Reads the composer until `accept` holds for a readout.
///
/// # Panics
///
/// Panics with the last readout if `accept` still does not hold after
/// [`HANG_GUARD`].
async fn observe_composer_until(
    session: &PtySession,
    section: &ComposerSection,
    what: &str,
    mut accept: impl FnMut(&ComposerReadout) -> bool,
) {
    let deadline = tokio::time::Instant::now() + HANG_GUARD;
    loop {
        let section = section.clone();
        let readout = session
            .read_screen(move |copy| reading::read_composer(copy, &section))
            .await
            .unwrap();
        if accept(&readout) {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the composer never showed {what}; last read {readout:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn submit_child_launched_from_this_crate_via_the_shared_launcher_draws_a_written_body() {
    let section = composer_section();
    let argv = process::submit_child_command_with_composer("MCP-COMPOSER-READY", 0, 0, 0, 0);
    let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    let session = PtySession::spawn_with_command(
        &refs,
        &DaemonConfig::default(),
        &yagura_daemon::HolderHost::from_config(&DaemonConfig::default()),
    )
    .await
    .expect("spawn_with_command should start the composer-draw-mode fixture");

    observe_composer_until(&session, &section, "its marker", |r| {
        !matches!(r, ComposerReadout::NotObserved)
    })
    .await;

    session
        .write_input(b"McpReachableBody7")
        .await
        .expect("write_input should reach the running fixture");

    // A window opened from this crate through the shared launcher alone must
    // draw the body it was written, the same as when the identical fixture is
    // opened from yagura-daemon's own tests.
    observe_composer_until(
        &session,
        &section,
        "the written body",
        |r| matches!(r, ComposerReadout::Body(text) if text == "McpReachableBody7"),
    )
    .await;
}
