#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// The two refusal cases run the compiled relay with `std::process::Command`
// to read its exit status and both streams; no window spawn of yagura-os
// applies to a CLI's MCP server.
#![allow(clippy::disallowed_methods)]

//! `yagura-mcp`'s side of instance namespacing: which endpoint the relay dials,
//! and that an invalid or empty `YAGURA_INSTANCE` refuses to start rather than
//! falling back to the default instance.
//!
//! `yagura-os` owns identifier validation and namespace composition
//! (`crates/yagura-os/tests/instance.rs`) and `yagura-daemon` its own dispatch
//! (`crates/yagura-daemon/tests/instance_resolution.rs`). The relay's logic is
//! private to its `main.rs`, so every property here is measured from outside
//! the compiled binary.
//!
//! ## Safety: never touching a real daemon
//!
//! An earlier incident terminated a real running `yagura-daemon.exe` by
//! assuming a non-default instance identifier was isolated from the default
//! endpoint before the resolution behind it existed. The cases avoid that
//! structurally:
//!
//! - A connecting case binds the stand-in daemon at the endpoint the instance
//!   under test resolves to before it starts the relay. Binding claims the name
//!   exclusively, so the relay can never reach anything else listening under
//!   it; a failed bind panics instead of falling through.
//! - Named instances use identifiers reserved for this file.
//! - Only [`default_instance_is_reached_when_yagura_instance_is_unset`] binds
//!   the default instance's endpoint, after the same bind-first guarantee.
//! - The two refusal cases bind nothing: the relay must refuse before it
//!   computes an endpoint to dial.

use std::path::PathBuf;
use std::process::Stdio as StdStdio;

use serde_json::json;
use yagura_os::InstanceNamespace;
use yagura_test_support::paths::TempDir;

use crate::common;

use common::{
    FakeDaemon, RelayProcess, initialize_line, spawn_relay, spawn_relay_without_instance,
};

/// Path of the `yagura-mcp` binary cargo built for this test run.
fn mcp_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_yagura-mcp"))
}

/// The default instance's namespace, obtained the only public way.
fn default_namespace() -> InstanceNamespace {
    yagura_os::resolve_instance_from(None).expect("None must resolve to the default instance")
}

/// Reserved, non-default instance identifiers of this file, distinct from the
/// daemon suite's own.
const SCOPED_INSTANCE_ID_NAMED: &str = "mcpnamedtest";
const SCOPED_INSTANCE_ID_OVERRIDE: &str = "mcpovertest";

/// A `YAGURA_INSTANCE` value whose rejection is unambiguous (it holds an
/// uppercase letter) and recognizable, so the "never echoed" assertions have
/// something distinctive to search for.
const REJECTED_INSTANCE_CANARY: &str = "BadCanaryXYZ";

/// Opens the CLI's session through `relay` against the stand-in bound for its
/// instance: the relay's `relay_hello` and the `initialize` it carries reach
/// that endpoint, and the stand-in's answer comes back on stdout.
async fn assert_reaches(daemon: &mut FakeDaemon, relay: &mut RelayProcess) {
    relay.send(&initialize_line(1)).await;
    let mut conn = daemon.accept().await;
    conn.hello().await;
    conn.welcome("tools-1").await;
    let (request, seq) = conn.request().await;
    assert_eq!(request, initialize_line(1));
    let answer = conn.answer(&request, seq, false).await;
    assert_eq!(
        relay.answer(&json!(1)).await,
        answer,
        "the stand-in bound for the instance answers through the relay"
    );
}

// --- Connects to the same endpoint as today when unset, and to a different
//     one when named -----------------------------------------------------

#[tokio::test]
async fn default_instance_is_reached_when_yagura_instance_is_unset() {
    let mut daemon = FakeDaemon::bind_namespace(&default_namespace()).await;
    let mut relay = spawn_relay_without_instance(&[]);
    assert_reaches(&mut daemon, &mut relay).await;
}

#[tokio::test]
async fn named_instance_is_reached_when_yagura_instance_is_a_valid_identifier() {
    let mut daemon = FakeDaemon::bind(SCOPED_INSTANCE_ID_NAMED).await;
    let mut relay = spawn_relay(SCOPED_INSTANCE_ID_NAMED, &[], &[]);
    // yagura-os's own suite proves the named namespace's endpoint differs from
    // the default's, so reaching this one shows YAGURA_INSTANCE chose it.
    assert_reaches(&mut daemon, &mut relay).await;
}

// --- The relay never opens the store, so the daemon's store-override
//     conflict check does not apply to it ------------------------------------

#[tokio::test]
async fn store_override_does_not_block_a_named_instance_from_connecting() {
    let mut daemon = FakeDaemon::bind(SCOPED_INSTANCE_ID_OVERRIDE).await;
    let store_dir = TempDir::new("yagura-mcp-store-override", "test");
    let store_override = store_dir.path().join("store.db");
    let store_override = store_override
        .to_str()
        .expect("temp dir path must be valid UTF-8 on this platform");
    let mut relay = spawn_relay(
        SCOPED_INSTANCE_ID_OVERRIDE,
        &[("YAGURA_STORE_DB", store_override)],
        &[],
    );
    assert_reaches(&mut daemon, &mut relay).await;
}

// --- An invalid or empty YAGURA_INSTANCE refuses to start, never falls
//     back to the default instance, and never echoes the rejected value ---

#[test]
fn invalid_instance_identifier_is_rejected_and_never_echoed() {
    let output = std::process::Command::new(mcp_bin())
        .env_remove("YAGURA_INSTANCE")
        .env("YAGURA_INSTANCE", REJECTED_INSTANCE_CANARY)
        .stdin(StdStdio::null())
        .stdout(StdStdio::piped())
        .stderr(StdStdio::piped())
        .output()
        .expect("failed to spawn the yagura-mcp binary under test");
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        !output.status.success(),
        "an invalid YAGURA_INSTANCE must make yagura-mcp exit non-zero rather than fall back \
         to the default instance; stderr: {stderr}"
    );
    assert!(
        output.stdout.is_empty(),
        "an invalid YAGURA_INSTANCE must be refused before yagura-mcp starts serving MCP \
         stdio, so nothing should reach stdout; got: {:?}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        !stderr.is_empty(),
        "the refusal must explain itself on stderr"
    );
    assert!(
        !stderr.contains(REJECTED_INSTANCE_CANARY),
        "the rejected identifier itself must never be echoed back on stderr; stderr: {stderr}"
    );
}

#[test]
fn empty_instance_identifier_is_rejected_not_rounded_to_the_default_instance() {
    let output = std::process::Command::new(mcp_bin())
        .env_remove("YAGURA_INSTANCE")
        .env("YAGURA_INSTANCE", "")
        .stdin(StdStdio::null())
        .stdout(StdStdio::piped())
        .stderr(StdStdio::piped())
        .output()
        .expect("failed to spawn the yagura-mcp binary under test");
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        !output.status.success(),
        "an empty YAGURA_INSTANCE must be rejected, not rounded to the default instance (the \
         same asymmetry yagura-daemon's cli::resolve_instance_id documents relative to \
         resolve_playbook); stderr: {stderr}"
    );
    assert!(
        output.stdout.is_empty(),
        "an empty YAGURA_INSTANCE must be refused before yagura-mcp starts serving MCP stdio, \
         so nothing should reach stdout; got: {:?}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        !stderr.is_empty(),
        "the refusal must explain itself on stderr"
    );
}
