// `cfg(windows)`: this file exercises `WindowsIpc` (the DACL-secured named
// pipe) directly, bypassing the `PlatformIpc` alias on purpose; the
// equivalent Unix `SocketSecurity` counter-proof lives in `os_unix.rs`.
#![cfg(windows)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// The counter-proof needs an endpoint the secured `bind` would never
// produce, so it is created from the raw listener on purpose.
#![allow(clippy::disallowed_types)]

//! Integration test: yagura-os's DACL-secured named pipe security contract
//! (`WindowsIpc` as `PeerAuth` / `IpcTransport`: `endpoint_security` / `bind` /
//! `endpoint_allows_only` / `current_user_id`), through the public safe API
//! only.
//!
//! Each case creates a fresh, uniquely named pipe: verification consumes the
//! pipe instance, so the same pipe can never be verified twice.

use std::ffi::OsStr;

use yagura_os::IpcTransport;
use yagura_os::PeerAuth;
use yagura_os::platform_windows::WindowsIpc;
use yagura_os::{secure_owned_dir, secure_owned_file};
use yagura_test_support::ipc::unique_endpoint_name;
use yagura_test_support::paths::{TempDir, unique_suffix};

/// Endpoint-name prefix for this file, kept distinct from the daemon's own so
/// a pipe created here can never be mistaken for a resident daemon's.
const PIPE_PREFIX: &str = "yagura-test-security";

/// The default instance, obtained the same way production code must
/// (`InstanceNamespace` has no public constructor).
fn default_namespace() -> yagura_os::InstanceNamespace {
    yagura_os::resolve_instance_from(None).expect("None must resolve to the default instance")
}

/// A non-default instance, likewise obtained through the only public
/// resolver.
fn dev_namespace() -> yagura_os::InstanceNamespace {
    yagura_os::resolve_instance_from(Some(OsStr::new("dev")))
        .expect("\"dev\" is a valid identifier and must resolve")
}

/// A `user_id` that cannot collide with a real SID or with another test's
/// pipe, for the multi-instance cases below that build their own pipe name through
/// `default_endpoint_owning` rather than `unique_endpoint_name`.
fn fake_unique_user_id(tag: &str) -> String {
    format!("{PIPE_PREFIX}-{tag}-{}", unique_suffix())
}

// ---------------------------------------------------------------------
// PeerAuth::endpoint_security
// ---------------------------------------------------------------------

#[test]
fn endpoint_security_succeeds() {
    let ipc = WindowsIpc;
    let _security = ipc
        .endpoint_security()
        .expect("endpoint_security should succeed in a normal Windows test-runner session");
}

// ---------------------------------------------------------------------
// IpcTransport::bind: name-squatting exclusivity (name-squatting defense)
// ---------------------------------------------------------------------

#[tokio::test]
async fn bind_succeeds_on_a_fresh_unique_name() {
    let ipc = WindowsIpc;
    let security = ipc
        .endpoint_security()
        .expect("endpoint_security should succeed");
    let name = unique_endpoint_name(PIPE_PREFIX, "first-instance-ok");

    let _listener = ipc
        .bind(&name, security)
        .await
        .expect("the first bind on a fresh unique name must succeed");
}

#[tokio::test]
async fn a_bound_listener_debug_names_its_endpoint_and_that_an_instance_waits() {
    let ipc = WindowsIpc;
    let security = ipc
        .endpoint_security()
        .expect("endpoint_security should succeed");
    let name = unique_endpoint_name(PIPE_PREFIX, "listener-debug");

    let listener = ipc
        .bind(&name, security)
        .await
        .expect("the first bind on a fresh unique name must succeed");

    let text = format!("{listener:?}");
    assert!(text.starts_with("PipeListener"), "got {text:?}");
    // Debug escapes the name's backslashes, so the distinctive tail is looked for.
    assert!(
        text.contains("yagura-test-security-listener-debug"),
        "got {text:?}; endpoint {name}"
    );
    assert!(text.contains("has_pending: true"), "got {text:?}");
}

#[tokio::test]
async fn bind_rejects_a_second_bind_on_the_same_name() {
    let ipc = WindowsIpc;
    let name = unique_endpoint_name(PIPE_PREFIX, "first-instance-exclusive");

    // First bind must succeed and must be kept alive (not dropped) so the
    // name is still owned when the second bind is attempted below.
    let first_security = ipc
        .endpoint_security()
        .expect("endpoint_security should succeed");
    let _first = ipc
        .bind(&name, first_security)
        .await
        .expect("first bind must succeed");

    // `PipeSecurity` is not `Clone`, so a second bind needs a freshly built value.
    let second_security = ipc
        .endpoint_security()
        .expect("endpoint_security should succeed");
    let second = ipc.bind(&name, second_security).await;
    assert!(
        second.is_err(),
        "a second bind on the same name must fail while the first instance is still alive \
         (name-squatting defense)"
    );
}

// ---------------------------------------------------------------------
// PeerAuth::endpoint_allows_only: DACL verification (unique-pipe-per-case approach)
// ---------------------------------------------------------------------

#[tokio::test]
async fn endpoint_allows_only_current_user_with_secured_pipe() {
    let ipc = WindowsIpc;
    let current_id = ipc
        .current_user_id()
        .expect("current_user_id should resolve");
    let security = ipc
        .endpoint_security()
        .expect("endpoint_security should succeed");
    let name = unique_endpoint_name(PIPE_PREFIX, "dacl-allows-current");

    let _listener = ipc
        .bind(&name, security)
        .await
        .expect("bind should succeed");

    let allows = ipc
        .endpoint_allows_only(&name, &current_id)
        .expect("endpoint_allows_only should resolve for a valid pipe");
    assert!(
        allows,
        "endpoint_allows_only must return true for the current user's id when the pipe was \
         created through endpoint_security"
    );
}

#[tokio::test]
async fn endpoint_allows_only_rejects_different_sid() {
    let ipc = WindowsIpc;
    let security = ipc
        .endpoint_security()
        .expect("endpoint_security should succeed");
    let name = unique_endpoint_name(PIPE_PREFIX, "dacl-rejects-system");

    let _listener = ipc
        .bind(&name, security)
        .await
        .expect("bind should succeed");

    // S-1-5-18 is LocalSystem, guaranteed to differ from interactive test-runner user.
    let allows = ipc
        .endpoint_allows_only(&name, "S-1-5-18")
        .expect("endpoint_allows_only should resolve for a valid pipe");
    assert!(
        !allows,
        "endpoint_allows_only must return false for an unrelated well-known SID (LocalSystem)"
    );
}

#[tokio::test]
async fn endpoint_allows_only_false_for_unsecured_acl() {
    // Counter-proof against a rubber-stamp implementation: a pipe created with
    // the default ACL (which grants principals other than the current user,
    // e.g. SYSTEM) must be rejected even for the current user's own id.
    // The raw listener is what builds that default ACL: the secured `bind`
    // never produces one, so there is no other way to reach this case.
    let ipc = WindowsIpc;
    let current_id = ipc
        .current_user_id()
        .expect("current_user_id should resolve");
    let name = unique_endpoint_name(PIPE_PREFIX, "dacl-default-acl-unsecured");

    let _pipe = tokio::net::windows::named_pipe::ServerOptions::new()
        .first_pipe_instance(true)
        .reject_remote_clients(true)
        .create(&name)
        .expect("default-ACL pipe creation via bare ServerOptions must succeed");

    let allows = ipc
        .endpoint_allows_only(&name, &current_id)
        .expect("endpoint_allows_only should resolve for a valid pipe");
    assert!(
        !allows,
        "endpoint_allows_only must return false for a pipe created with the default \
         (unsecured) ACL, even when checked against the current user's own id"
    );
}

// ---------------------------------------------------------------------
// A non-default instance's endpoint gets the same protection strength as
// the default's: the security functions are shared, only the name fed to
// them differs.
// ---------------------------------------------------------------------

#[tokio::test]
async fn endpoint_allows_only_true_for_a_non_default_instances_endpoint() {
    let ipc = WindowsIpc;
    let namespace = dev_namespace();
    let user_id = fake_unique_user_id("nondefault");
    let name = yagura_os::default_endpoint_owning(&user_id, &namespace);

    let security = ipc
        .endpoint_security()
        .expect("endpoint_security should succeed");
    let _listener = ipc
        .bind(&name, security)
        .await
        .expect("bind should succeed for a non-default instance's pipe name too");

    let current = ipc
        .current_user_id()
        .expect("current_user_id should resolve");
    let allows = ipc
        .endpoint_allows_only(&name, &current)
        .expect("endpoint_allows_only should resolve for a bound endpoint");
    assert!(
        allows,
        "endpoint_allows_only must return true for a non-default instance's endpoint, exactly \
         as it does for the default's"
    );
}

// ---------------------------------------------------------------------
// Two different namespaces can bind concurrently for the same user_id; a
// second bind of the *same* namespace's endpoint still fails, because
// multi-launch protection is per-instance.
// ---------------------------------------------------------------------

#[tokio::test]
async fn two_namespaces_bind_concurrently_but_a_second_bind_of_the_same_namespace_fails() {
    let ipc = WindowsIpc;
    let user_id = fake_unique_user_id("namespaces");
    let default_name = yagura_os::default_endpoint_owning(&user_id, &default_namespace());
    let dev_name = yagura_os::default_endpoint_owning(&user_id, &dev_namespace());
    assert_ne!(
        default_name, dev_name,
        "fixture precondition: the two namespaces must resolve to different pipe names"
    );

    let security_a = ipc
        .endpoint_security()
        .expect("endpoint_security should succeed");
    let _listener_a = ipc
        .bind(&default_name, security_a)
        .await
        .expect("binding the default instance's pipe must succeed");

    let security_b = ipc
        .endpoint_security()
        .expect("endpoint_security should succeed");
    let _listener_b = ipc.bind(&dev_name, security_b).await.expect(
        "binding a different namespace's pipe concurrently must succeed (multi-launch \
         protection is per-instance)",
    );

    let security_c = ipc
        .endpoint_security()
        .expect("endpoint_security should succeed");
    let second_default_bind = ipc.bind(&default_name, security_c).await;
    assert!(
        second_default_bind.is_err(),
        "a second bind of the same namespace's pipe must still fail while the first is alive"
    );
}

// ---------------------------------------------------------------------
// secure_owned_dir / secure_owned_file: Windows no-op contract
// ---------------------------------------------------------------------

#[test]
fn secure_owned_dir_and_secure_owned_file_are_a_harmless_no_op_on_windows() {
    // Windows relies on NTFS ACL inheritance from `%LOCALAPPDATA%`,
    // so both helpers must be `Ok(())` no-ops that never touch
    // either target's own permissions.
    let base = TempDir::new("os-security", "secure-owned-noop-base");
    let leaf = base.path().join("yagura");
    std::fs::create_dir_all(&leaf).expect("create fixture leaf dir");
    let file = leaf.join("secret.db");
    std::fs::write(&file, b"data").expect("write fixture file");

    let dir_readonly_before = std::fs::metadata(&leaf)
        .expect("leaf must exist")
        .permissions()
        .readonly();
    let file_readonly_before = std::fs::metadata(&file)
        .expect("file must exist")
        .permissions()
        .readonly();

    let dir_result = secure_owned_dir(base.path(), &leaf);
    let file_result = secure_owned_file(&file);

    assert!(
        dir_result.is_ok(),
        "secure_owned_dir must return Ok(()) on Windows: {dir_result:?}"
    );
    assert!(
        file_result.is_ok(),
        "secure_owned_file must return Ok(()) on Windows: {file_result:?}"
    );
    assert_eq!(
        std::fs::metadata(&leaf)
            .expect("leaf must still exist")
            .permissions()
            .readonly(),
        dir_readonly_before,
        "the directory's own permissions must be unchanged by the Windows no-op"
    );
    assert_eq!(
        std::fs::metadata(&file)
            .expect("file must still exist")
            .permissions()
            .readonly(),
        file_readonly_before,
        "the file's own permissions must be unchanged by the Windows no-op"
    );
}
