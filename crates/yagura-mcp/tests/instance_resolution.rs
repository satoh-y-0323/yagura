#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// This file spawns the real `yagura-mcp` binary and drives it through the
// MCP stdio protocol from the client side, and binds a throwaway `IpcServer`
// through `yagura_os::IpcTransport::bind` (never a raw platform socket or
// pipe type -- the disallowed-types ban on those stays intact) to observe
// which endpoint the spawned process dials. Both require creating an OS
// process directly rather than through the PTY path the disallowed-methods
// lint otherwise reserves `Command::new` / `Command::spawn` for.
#![allow(clippy::disallowed_methods)]

//! Integration test: `yagura-mcp`'s side of instance namespacing.
//!
//! `yagura-os` owns identifier validation and namespace composition
//! (`crates/yagura-os/tests/instance.rs`); `yagura-daemon` owns the daemon's
//! own dispatch (`crates/yagura-daemon/tests/instance_resolution.rs`); this
//! file measures the bridge's own single responsibility: which endpoint it
//! dials, and that an invalid or empty `YAGURA_INSTANCE` refuses to start
//! rather than falling back to the default instance.
//!
//! `main.rs` holds all of this logic privately, with no library module
//! exposing it, so none of it is reachable as a Rust function call from here.
//! Every property below is measured by spawning the compiled `yagura-mcp`
//! binary (`CARGO_BIN_EXE_yagura-mcp`, the build cargo produces for this
//! test run) and observing its behavior from the
//! outside, the same way `tests/tool_listing.rs` already does for
//! `tools/list`, and the same way
//! `crates/yagura-daemon/tests/instance_resolution.rs` observes `main.rs`'s
//! store-override rejection.
//!
//! ## Safety: never touching a real daemon
//!
//! An earlier incident terminated a real running
//! `yagura-daemon.exe` by assuming a non-default instance identifier was
//! automatically isolated from the default endpoint -- true only once the
//! resolution behind it was implemented, not before. This file avoids that
//! failure mode structurally rather than by convention:
//!
//! - [`bind_test_daemon`] binds a throwaway `IpcServer` at the exact
//!   endpoint name the instance under test resolves to, through
//!   `yagura_os::IpcTransport::bind` -- the same exclusive claim a real
//!   daemon's own bind makes (`crates/yagura-os/tests/os_security.rs` /
//!   `os_unix.rs` prove a second bind on the same name fails). A caller only
//!   proceeds to spawn a client once that bind has *already* returned `Ok`,
//!   so the spawned `yagura-mcp` process can structurally never reach any
//!   other process listening under that name -- including a real daemon --
//!   whether or not one happens to be running when this suite executes. A
//!   `bind` failure panics loudly instead of falling through.
//! - Every test that exercises a named (non-default) instance uses a fixed
//!   identifier reserved for this file, never one shared with
//!   `crates/yagura-daemon/tests/instance_resolution.rs`'s own reserved id.
//! - Only one test ([`default_instance_is_reached_when_yagura_instance_is_unset`])
//!   ever binds the *default* instance's literal endpoint name, and it does
//!   so only after the bind-first guarantee above; every other connecting
//!   test binds a named instance's endpoint instead.
//! - The two rejection tests never bind anything at all: a `YAGURA_INSTANCE`
//!   that fails validation must make `yagura-mcp` refuse before it ever
//!   computes an endpoint to dial, so there is nothing for them to connect
//!   to in the first place.
//!
//! ## Driving the MCP protocol without the `rmcp` "client" feature
//!
//! The workspace enables `rmcp`'s `"server"`, `"macros"` and
//! `"transport-io"` features only (root `Cargo.toml`), so
//! `rmcp::transport::child_process` and the client-side
//! `serve` / `RunningService` API are not compiled in. This file therefore
//! drives the binary through the hand-written JSON-RPC client in
//! `tests/common`.

use std::ffi::OsStr;
use std::path::PathBuf;
use std::process::Stdio as StdStdio;
use std::sync::Arc;

use serde_json::{Value, json};
use tokio::task::JoinHandle;

use yagura_daemon::{DaemonConfig, IpcServer, WindowRegistry};
use yagura_os::{InstanceNamespace, IpcTransport, PeerAuth, PlatformIpc};
use yagura_test_support::paths::TempDir;

mod common;

use common::{McpChild, tool_reply};

/// Path of the `yagura-mcp` binary cargo built for this test run, never an
/// installed or separately running copy.
fn mcp_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_yagura-mcp"))
}

/// The default instance's namespace, obtained the only public way
/// (`InstanceNamespace` has no public constructor).
fn default_namespace() -> InstanceNamespace {
    yagura_os::resolve_instance_from(None).expect("None must resolve to the default instance")
}

/// A named instance's namespace, for `id`.
fn named_namespace(id: &str) -> InstanceNamespace {
    yagura_os::resolve_instance_from(Some(OsStr::new(id)))
        .unwrap_or_else(|err| panic!("{id:?} must be a valid instance identifier: {err}"))
}

/// Reserved, non-default instance identifier for the "connects to a
/// different endpoint" case. Distinct from
/// `crates/yagura-daemon/tests/instance_resolution.rs`'s own
/// `SCOPED_INSTANCE_ID` so the two files' fixed pipe/socket names can never
/// collide even if both suites happen to run at once.
const SCOPED_INSTANCE_ID_NAMED: &str = "mcpnamedtest";

/// Reserved, non-default instance identifier for the store-override case.
const SCOPED_INSTANCE_ID_OVERRIDE: &str = "mcpovertest";

/// A `YAGURA_INSTANCE` value chosen so its rejection is unambiguous
/// (contains an uppercase letter, which the allow-list forbids) and
/// recognizable, so the "the rejected value is never echoed" assertions
/// below have something distinctive to search for.
const REJECTED_INSTANCE_CANARY: &str = "BadCanaryXYZ";

/// Binds a throwaway `IpcServer` (no windows opened; `list_windows` alone is
/// enough to prove a round trip happened) at the endpoint `namespace`
/// resolves to, and starts serving it.
///
/// Binding happens here, synchronously, *before* any client is spawned by a
/// caller: `IpcTransport::bind` claims the endpoint name exclusively, so a
/// caller that only proceeds once this returns can never have its own
/// spawned client reach any other process listening under that name (see
/// the module doc comment's safety section).
///
/// # Panics
///
/// Panics if the endpoint is already claimed by something else. That is the
/// fail-loud outcome this contract requires: proceeding anyway would let
/// the caller's spawned client reach whatever already owns the name.
async fn bind_test_daemon(
    namespace: &InstanceNamespace,
) -> (JoinHandle<Result<(), yagura_daemon::IpcError>>, String) {
    let ipc = PlatformIpc::default();
    let user_id = ipc
        .current_user_id()
        .expect("current_user_id must resolve in a normal test-runner session");
    let endpoint = yagura_os::default_endpoint_owning(&user_id, namespace);
    let security = ipc
        .endpoint_security()
        .expect("endpoint_security must succeed in a normal test-runner session");
    let listener = ipc.bind(&endpoint, security).await.unwrap_or_else(|err| {
        panic!(
            "cannot claim endpoint {endpoint:?} for this test: {err}. If a real yagura-daemon \
             is already running under this instance, stop it before running this suite -- this \
             test refuses to proceed against an endpoint it does not itself own \
             (see this file's module doc comment)"
        )
    });

    let config = DaemonConfig::default();
    let registry = Arc::new(WindowRegistry::new(config.clone()));
    let server = Arc::new(IpcServer::new(registry, endpoint.clone(), config));
    let server_task = tokio::spawn(server.run_with_listener(listener, user_id));
    (server_task, endpoint)
}

/// Speaks the MCP handshake to `bridge` and calls `list_windows`, returning
/// its JSON-RPC response. `list_windows` is never approval-gated, so a
/// successful answer can only come from a real round trip against whatever
/// daemon the bridge dialed.
async fn round_trip_list_windows(bridge: &mut McpChild) -> Value {
    bridge.initialize().await;
    bridge.call_tool("list_windows", json!({})).await
}

/// Asserts `response` is a genuine, successful `list_windows` result: proof
/// that the round trip reached the daemon this test bound at `endpoint`,
/// not merely that `yagura-mcp` answered *something*.
fn assert_reached_the_bound_daemon(response: &Value, endpoint: &str) {
    let (is_error, text) = tool_reply(response);
    assert!(
        !is_error,
        "list_windows must succeed once yagura-mcp dials the endpoint this test bound \
         ({endpoint}); a failed round trip (e.g. daemon_unavailable) means it dialed the wrong \
         one instead; got: {response}"
    );
    // The bound daemon has no windows, so its answer is an empty list.
    let answer: Value = serde_json::from_str(&text)
        .unwrap_or_else(|err| panic!("list_windows must answer in JSON, got {text:?}: {err}"));
    assert_eq!(
        answer.get("windows"),
        Some(&json!([])),
        "list_windows must carry the bound daemon's (empty) window list, got: {response}"
    );
}

// --- Connects to the same endpoint as today when unset, and to a different
//     one when named -----------------------------------------------------

#[tokio::test]
async fn default_instance_is_reached_when_yagura_instance_is_unset() {
    let namespace = default_namespace();
    let (server_task, endpoint) = bind_test_daemon(&namespace).await;

    let mut bridge = McpChild::spawn(&[]);
    let response = round_trip_list_windows(&mut bridge).await;

    bridge.kill().await;
    server_task.abort();

    assert_reached_the_bound_daemon(&response, &endpoint);
}

#[tokio::test]
async fn named_instance_is_reached_when_yagura_instance_is_a_valid_identifier() {
    let namespace = named_namespace(SCOPED_INSTANCE_ID_NAMED);
    let (server_task, endpoint) = bind_test_daemon(&namespace).await;

    let mut bridge = McpChild::spawn(&[("YAGURA_INSTANCE", SCOPED_INSTANCE_ID_NAMED)]);
    let response = round_trip_list_windows(&mut bridge).await;

    bridge.kill().await;
    server_task.abort();

    // Yagura-os's own suite (`resolve_startup_env`) already proves the
    // named namespace's composed name differs from the default's; reaching
    // *this* endpoint (never the default one, which this test never binds)
    // is therefore sufficient to show YAGURA_INSTANCE changed the dial
    // target.
    assert_reached_the_bound_daemon(&response, &endpoint);
}

// --- yagura-mcp never opens the store, so it must not run the
//     store-override conflict check the daemon's Run command does --------

#[tokio::test]
async fn store_override_does_not_block_a_named_instance_from_connecting() {
    let namespace = named_namespace(SCOPED_INSTANCE_ID_OVERRIDE);
    let (server_task, endpoint) = bind_test_daemon(&namespace).await;
    // Never opened by this test or by yagura-mcp: yagura-mcp reaches the
    // daemon over the endpoint alone and has no reason to touch a store
    // path at all.
    let store_dir = TempDir::new("yagura-mcp-store-override", "test");
    let store_override = store_dir.path().join("store.db");
    let store_override_str = store_override
        .to_str()
        .expect("temp dir path must be valid UTF-8 on this platform");

    let mut bridge = McpChild::spawn(&[
        ("YAGURA_INSTANCE", SCOPED_INSTANCE_ID_OVERRIDE),
        ("YAGURA_STORE_DB", store_override_str),
    ]);
    let response = round_trip_list_windows(&mut bridge).await;

    bridge.kill().await;
    server_task.abort();

    assert_reached_the_bound_daemon(&response, &endpoint);
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
