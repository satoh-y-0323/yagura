#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg(unix)]
// The process-group and peer-credential contracts under test are about real
// children and real sockets, so both are created directly here.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

//! Integration test: the Unix implementation of the OS abstraction boundary
//! (`UnixIpc` as `PeerAuth` + `IpcTransport`, `UnixProcessGroup`, and the
//! `resolve_endpoint_with` seam).
//!
//! This file owns the behavioral / operational side of the seam and of the two
//! Unix implementations; `tests/endpoint.rs`
//! owns the pure string-representation contract shared with the Windows side.
//!
//! Every process spawned here runs `/bin/sh -c` with a script built by
//! concatenating fixed literal fragments (never a runtime-interpolated
//! string), through the local helpers below.
//! `std::os::unix::process::CommandExt::process_group` gives a spawned child
//! its own, brand-new process group without depending on an external `setsid`
//! binary or that binary's version-dependent fork behavior.

use std::ffi::OsStr;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use yagura_os::platform_unix::{
    EndpointCreation, UnixIpc, UnixProcessGroup, resolve_endpoint_with,
};
use yagura_os::{
    IpcTransport, OsError, PeerAuth, SeedOutcome, instance_dir, resolve_instance_from,
    secure_owned_dir, secure_owned_file, seed_config_file,
};

use yagura_test_support::paths::{TempDir, short_tmp_dir};

// =====================================================================
// Local helpers (test-only; no production code depends on any of these)
// =====================================================================

/// A fixture directory this test owns exclusively, chmod'd to 0700 (owner
/// rwx, no group/other bits): the "compliant parent dir" positive control for
/// every bind / resolve_endpoint_with test below.
///
/// Built on `short_tmp_dir`'s bounded `/tmp` origin so a macOS-length
/// `$TMPDIR` cannot push a resolved socket path over `sockaddr_un.sun_path`.
fn secure_dir() -> TempDir {
    let dir = short_tmp_dir("os-unix", "secure");
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))
        .expect("chmod fixture dir to 0700");
    dir
}

/// Same as [`secure_dir`], but group- and other-readable (0755): the
/// "insecure parent dir" negative control.
fn insecure_dir() -> TempDir {
    let dir = short_tmp_dir("os-unix", "insecure");
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755))
        .expect("chmod fixture dir to 0755");
    dir
}

/// A `user_id` value that cannot correspond to a real logind session, so
/// stage 2 (`/run/user/{user_id}`, not overridable through
/// `resolve_endpoint_with`'s arguments) is reliably unusable. Needed by every
/// test here that must force a fallback to stage 1 or 3 to observe them in
/// isolation.
fn unusable_stage2_user_id() -> String {
    format!("999999999{}", std::process::id())
}

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

/// Spawns `/bin/sh -c script` as a brand-new process group leader (its own
/// pgid, equal to its own pid) via `process_group(0)` -- the fixture for
/// `UnixProcessGroup::led_by`'s leader candidate.
fn spawn_own_group_child(script: &str) -> Child {
    let mut cmd = Command::new("/bin/sh");
    cmd.arg("-c").arg(script);
    cmd.process_group(0);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    cmd.spawn().expect("spawn /bin/sh -c should succeed")
}

/// Spawns `/bin/sh -c script` as a brand-new process group leader, like
/// [`spawn_own_group_child`], but blocks until the child has armed a
/// `trap '' TERM` and reports readiness by writing one byte to stdout --
/// the fixture for every test that sends a signal a plain trap-ignoring
/// child could otherwise race (`spawn` returning before the shell has
/// actually installed the trap).
///
/// `script_after_ready` is concatenated, unmodified, after the fixed literal
/// prefix `trap '' TERM; printf r; exec `; only fixed literal fragments are
/// ever combined here, never a runtime-interpolated string.
fn spawn_own_group_child_ready(script_after_ready: &str) -> Child {
    let script = format!("trap '' TERM; printf r; exec {script_after_ready}");
    let mut cmd = Command::new("/bin/sh");
    cmd.arg("-c").arg(script);
    cmd.process_group(0);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = cmd.spawn().expect("spawn /bin/sh -c should succeed");
    let mut stdout = child.stdout.take().expect("stdout must be piped");

    let (tx, rx) = mpsc::channel();
    // Not joined: once the child `exec`s into the caller's script (e.g.
    // `sleep 30`), it keeps stdout open for the rest of its life, so
    // joining this thread after readiness is reported would block until
    // the child itself exits.
    std::thread::spawn(move || {
        let mut byte = [0u8; 1];
        let _ = tx.send(stdout.read_exact(&mut byte));
    });

    match rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(())) => {}
        Ok(Err(_)) => panic!("the ignore-TERM fixture exited before it armed its trap"),
        Err(_) => panic!("the ignore-TERM fixture did not report readiness within 5s"),
    }

    child
}

/// The lock file a daemon holds next to its socket for as long as it lives.
fn endpoint_lock_path(endpoint: &std::path::Path) -> std::path::PathBuf {
    let mut path = endpoint.as_os_str().to_owned();
    path.push(".lock");
    std::path::PathBuf::from(path)
}

/// Takes the endpoint lock the way a live daemon holds it, standing in for
/// that daemon until the returned file is dropped.
fn hold_endpoint_lock(endpoint: &std::path::Path) -> std::fs::File {
    use std::os::fd::AsRawFd;

    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(endpoint_lock_path(endpoint))
        .expect("open the endpoint lock file");
    // SAFETY: `lock` owns this open file descriptor for the whole call.
    let result = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    assert_eq!(result, 0, "the fixture must take the endpoint lock");
    lock
}

/// This test process's own process group id, queried directly via libc.
fn own_pgrp() -> i32 {
    // SAFETY: getpgrp takes no arguments and only reads this process's own
    // process group id.
    unsafe { libc::getpgrp() }
}

/// Polls `child.try_wait()` (which reaps on success, so a zombie is never
/// mistaken for "still alive") until it reports an exit or `timeout` elapses.
fn wait_until_exited(child: &mut Child, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if child
            .try_wait()
            .expect("try_wait must not error for a child we own")
            .is_some()
        {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Blocks until `pid` has exited but is not yet reaped (a zombie), confirmed
/// via `waitid(P_PID, .., WEXITED | WNOWAIT)`, which observes the exit
/// without consuming it -- unlike [`wait_until_exited`]'s `try_wait`, which
/// would reap the child and hide the exact "group exists, no live member,
/// nothing reaped" state this file's `killpg`/EPERM tests need to construct.
fn wait_until_zombie(pid: u32) {
    loop {
        let mut siginfo: libc::siginfo_t = unsafe { std::mem::zeroed() };
        // SAFETY: `siginfo` is a stack-local sized exactly for the struct
        // waitid(2) fills; `pid` is a process this test spawned and has not
        // yet reaped, and `WNOWAIT` means this call does not consume it.
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                pid as libc::id_t,
                &mut siginfo,
                libc::WEXITED | libc::WNOWAIT,
            )
        };
        if result == 0 {
            return;
        }
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() != Some(libc::EINTR) {
            panic!("waitid(WEXITED | WNOWAIT) failed while confirming the zombie: {err}");
        }
    }
}

// =====================================================================
// resolve_endpoint_with: stage 1 (xdg_runtime_dir, secure-dir check)
// =====================================================================

#[test]
fn stage1_is_used_when_xdg_runtime_dir_is_a_valid_owned_0700_dir() {
    let dir = secure_dir();
    let unused_tmp_root = secure_dir();

    let endpoint = resolve_endpoint_with(
        &unusable_stage2_user_id(),
        Some(OsStr::new(dir.path().to_str().expect("utf8 path"))),
        unused_tmp_root.path(),
        EndpointCreation::ReadOnly,
        &default_namespace(),
    )
    .expect("stage 1 must succeed for a valid, owned, 0700 XDG_RUNTIME_DIR");

    assert_eq!(
        endpoint,
        format!("{}/yagura.sock", dir.path().to_str().expect("utf8 path")),
        "stage 1 must build {{xdg_runtime_dir}}/yagura.sock verbatim"
    );
}

#[test]
fn stage1_is_rejected_when_xdg_runtime_dir_is_group_or_other_accessible() {
    let insecure = insecure_dir();
    let tmp_root = secure_dir();

    let endpoint = resolve_endpoint_with(
        &unusable_stage2_user_id(),
        Some(OsStr::new(insecure.path().to_str().expect("utf8 path"))),
        tmp_root.path(),
        EndpointCreation::CreateIfMissing,
        &default_namespace(),
    )
    .expect("stage 3 (tmp_root) must still succeed once stage 1/2 are unusable");

    assert!(
        !endpoint.starts_with(insecure.path().to_str().expect("utf8 path")),
        "an insecure XDG_RUNTIME_DIR (group/other accessible) must not be used for the \
         endpoint (runtime-dir security check); resolved: {endpoint}"
    );
}

#[test]
fn stage1_is_rejected_when_xdg_runtime_dir_is_a_symlink() {
    let target = secure_dir();
    let parent = TempDir::new("os-unix", "symlink-parent");
    let link_path = parent.path().join("xdg-link");
    std::os::unix::fs::symlink(target.path(), &link_path).expect("symlink creation should succeed");
    let tmp_root = secure_dir();

    let endpoint = resolve_endpoint_with(
        &unusable_stage2_user_id(),
        Some(OsStr::new(link_path.to_str().expect("utf8 path"))),
        tmp_root.path(),
        EndpointCreation::CreateIfMissing,
        &default_namespace(),
    )
    .expect("stage 3 must still succeed once stage 1 is rejected");

    assert!(
        !endpoint.starts_with(link_path.to_str().expect("utf8 path")),
        "a symlinked XDG_RUNTIME_DIR must be rejected (symlink_metadata non-link \
         condition); resolved: {endpoint}"
    );
}

#[test]
fn stage1_is_rejected_when_xdg_runtime_dir_is_an_owner_only_regular_file() {
    let holder = secure_dir();
    let file = holder.path().join("not-a-dir");
    std::fs::write(&file, b"").expect("write the fixture file");
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600))
        .expect("chmod the fixture file to 0600");
    let tmp_root = secure_dir();

    let endpoint = resolve_endpoint_with(
        &unusable_stage2_user_id(),
        Some(file.as_os_str()),
        tmp_root.path(),
        EndpointCreation::CreateIfMissing,
        &default_namespace(),
    )
    .expect("stage 3 must still succeed once stage 1 is rejected");

    assert!(
        endpoint.starts_with(tmp_root.path().to_str().expect("utf8 path")),
        "a regular file is not a runtime directory, however private; resolved: {endpoint}"
    );
}

// =====================================================================
// resolve_endpoint_with: stage 3 (tmp_root, EndpointCreation)
// =====================================================================

#[test]
fn stage3_creates_the_fallback_dir_with_mode_0700_when_endpoint_creation_is_create_if_missing() {
    let root = secure_dir();
    let user_id = unusable_stage2_user_id();

    let endpoint = resolve_endpoint_with(
        &user_id,
        None,
        root.path(),
        EndpointCreation::CreateIfMissing,
        &default_namespace(),
    )
    .expect("stage 3 must create the directory and succeed");

    let expected_dir = root.path().join(format!("yagura-{user_id}"));
    assert_eq!(
        endpoint,
        format!("{}/ipc.sock", expected_dir.to_str().expect("utf8 path"))
    );
    let meta = std::fs::symlink_metadata(&expected_dir).expect("created dir must exist");
    assert!(
        meta.is_dir(),
        "stage 3 must create a real directory, not a symlink"
    );
    assert_eq!(
        meta.permissions().mode() & 0o777,
        0o700,
        "stage 3 must create the fallback dir with mode 0700"
    );
}

#[test]
fn stage3_does_not_create_the_fallback_dir_when_endpoint_creation_is_read_only() {
    let root = secure_dir();
    let user_id = unusable_stage2_user_id();

    let result = resolve_endpoint_with(
        &user_id,
        None,
        root.path(),
        EndpointCreation::ReadOnly,
        &default_namespace(),
    );

    assert!(
        result.is_err(),
        "a client (EndpointCreation::ReadOnly) must not fabricate the fallback dir, so with \
         XDG_RUNTIME_DIR unset and stage 2 unusable, all 3 stages must fail"
    );
    let expected_dir = root.path().join(format!("yagura-{user_id}"));
    assert!(
        !expected_dir.exists(),
        "resolve_endpoint_with(EndpointCreation::ReadOnly) must never create {} (a \
         client never mutates the filesystem while resolving an endpoint)",
        expected_dir.display()
    );
}

#[test]
fn stage3_reuses_an_already_existing_fallback_dir_without_recreating_it() {
    let root = secure_dir();
    let user_id = unusable_stage2_user_id();
    let existing_dir = root.path().join(format!("yagura-{user_id}"));
    std::fs::create_dir(&existing_dir).expect("pre-create fallback dir");
    std::fs::set_permissions(&existing_dir, std::fs::Permissions::from_mode(0o700))
        .expect("chmod pre-created dir to 0700");

    let endpoint = resolve_endpoint_with(
        &user_id,
        None,
        root.path(),
        EndpointCreation::CreateIfMissing,
        &default_namespace(),
    )
    .expect("stage 3 must succeed by reusing the already-existing 0700 dir");

    assert_eq!(
        endpoint,
        format!("{}/ipc.sock", existing_dir.to_str().expect("utf8 path"))
    );
}

#[test]
fn resolve_endpoint_with_rejects_a_candidate_path_exceeding_the_sun_path_limit() {
    let root = secure_dir();
    // `sockaddr_un.sun_path`'s usable length is a small, platform-specific
    // constant; pad the tmp_root well past every platform's value so
    // the stage-3 candidate path overflows on every target.
    let long_component = "a".repeat(200);
    let long_root = root.path().join(long_component);
    std::fs::create_dir_all(&long_root).expect("create long tmp_root");

    let result = resolve_endpoint_with(
        &unusable_stage2_user_id(),
        None,
        &long_root,
        EndpointCreation::CreateIfMissing,
        &default_namespace(),
    );

    assert!(
        result.is_err(),
        "a resolved socket path longer than sockaddr_un.sun_path's platform-specific usable \
         length must be rejected rather than handed to bind() where it would fail with a less \
         diagnosable error; result: {result:?}"
    );
}

// =====================================================================
// Two different namespaces can bind concurrently for the same
// user_id; a second bind of the *same* namespace's endpoint still fails
// (multi-launch protection is per-instance).
// =====================================================================

#[tokio::test]
async fn two_namespaces_bind_concurrently_but_a_second_bind_of_the_same_namespace_fails() {
    let ipc = UnixIpc;
    let dir = secure_dir();
    let unused_tmp_root = secure_dir();
    let user_id = unusable_stage2_user_id();

    let default_endpoint = resolve_endpoint_with(
        &user_id,
        Some(OsStr::new(dir.path().to_str().expect("utf8 path"))),
        unused_tmp_root.path(),
        EndpointCreation::ReadOnly,
        &default_namespace(),
    )
    .expect("stage 1 must resolve the default instance's endpoint name");
    let dev_endpoint = resolve_endpoint_with(
        &user_id,
        Some(OsStr::new(dir.path().to_str().expect("utf8 path"))),
        unused_tmp_root.path(),
        EndpointCreation::ReadOnly,
        &dev_namespace(),
    )
    .expect("stage 1 must resolve the dev instance's endpoint name");
    assert_ne!(
        default_endpoint, dev_endpoint,
        "fixture precondition: the two namespaces must resolve to different endpoint names"
    );

    let security_a = ipc
        .endpoint_security()
        .expect("endpoint_security should succeed");
    let _listener_a = ipc
        .bind(&default_endpoint, security_a)
        .await
        .expect("binding the default instance's endpoint must succeed");

    let security_b = ipc
        .endpoint_security()
        .expect("endpoint_security should succeed");
    let _listener_b = ipc.bind(&dev_endpoint, security_b).await.expect(
        "binding a different namespace's endpoint concurrently must succeed (multi-launch \
         protection is per-instance)",
    );

    let security_c = ipc
        .endpoint_security()
        .expect("endpoint_security should succeed");
    let second_default_bind = ipc.bind(&default_endpoint, security_c).await;
    assert!(
        second_default_bind.is_err(),
        "a second bind of the same namespace's endpoint must still fail while the first is \
         alive"
    );
}

// =====================================================================
// UnixIpc::bind: parent-directory rejection
// =====================================================================

#[tokio::test]
async fn bind_rejects_when_the_parent_dir_is_a_symlink() {
    let ipc = UnixIpc;
    let target = secure_dir();
    let parent_holder = TempDir::new("os-unix", "bind-symlink-parent");
    let link_path = parent_holder.path().join("link-parent");
    std::os::unix::fs::symlink(target.path(), &link_path).expect("symlink creation should succeed");
    let endpoint = link_path.join("e.sock");

    let security = ipc
        .endpoint_security()
        .expect("endpoint_security should succeed");
    let result = ipc
        .bind(endpoint.to_str().expect("utf8 path"), security)
        .await;
    assert!(
        result.is_err(),
        "bind must reject an endpoint whose parent dir is a symlink (parent dir check)"
    );
}

#[tokio::test]
async fn bind_rejects_when_the_parent_dir_is_group_or_other_accessible() {
    let ipc = UnixIpc;
    let dir = insecure_dir();
    let endpoint = dir.path().join("e.sock");

    let security = ipc
        .endpoint_security()
        .expect("endpoint_security should succeed");
    let result = ipc
        .bind(endpoint.to_str().expect("utf8 path"), security)
        .await;
    assert!(
        result.is_err(),
        "bind must reject an endpoint whose parent dir grants group/other access"
    );
}

#[tokio::test]
async fn bind_succeeds_when_the_parent_dir_is_owned_and_0700() {
    let ipc = UnixIpc;
    let dir = secure_dir();
    let endpoint = dir.path().join("e.sock");

    let security = ipc
        .endpoint_security()
        .expect("endpoint_security should succeed");
    let _listener = ipc
        .bind(endpoint.to_str().expect("utf8 path"), security)
        .await
        .expect("bind must succeed for a fresh endpoint under an owned, 0700 parent dir");
}

// =====================================================================
// UnixIpc::bind: leftover socket
// =====================================================================

#[tokio::test]
async fn bind_rejects_a_non_socket_file_at_the_endpoint_path() {
    let ipc = UnixIpc;
    let dir = secure_dir();
    let endpoint = dir.path().join("e.sock");
    std::fs::write(&endpoint, b"not a socket").expect("write plain file");

    let security = ipc
        .endpoint_security()
        .expect("endpoint_security should succeed");
    let result = ipc
        .bind(endpoint.to_str().expect("utf8 path"), security)
        .await;

    assert!(
        result.is_err(),
        "bind must reject a non-socket file occupying the endpoint path (type check)"
    );
    assert_eq!(
        std::fs::read(&endpoint).expect("the rejected file must survive untouched"),
        b"not a socket",
        "a rejected non-socket file must never be unlinked"
    );
}

#[tokio::test]
async fn bind_refuses_a_second_bind_as_held_while_the_first_listener_lives() {
    let ipc = UnixIpc;
    let dir = secure_dir();
    let endpoint = dir.path().join("e.sock");
    let endpoint_str = endpoint.to_str().expect("utf8 path");

    let first_security = ipc
        .endpoint_security()
        .expect("endpoint_security should succeed");
    let _first_listener = ipc
        .bind(endpoint_str, first_security)
        .await
        .expect("first bind must succeed");

    let second_security = ipc
        .endpoint_security()
        .expect("endpoint_security should succeed");
    let second = ipc.bind(endpoint_str, second_security).await;
    match second {
        Err(OsError::EndpointHeld(reason)) => assert!(
            reason.contains("live listener"),
            "the refusal must name its reason (a live listener holds the path): {reason}"
        ),
        other => panic!(
            "a second bind while the first listener is still alive must be refused as held \
             by a live daemon; got: {other:?}"
        ),
    }
}

#[tokio::test]
async fn bind_unlinks_and_rebinds_a_stale_dead_socket() {
    let ipc = UnixIpc;
    let dir = secure_dir();
    let endpoint = dir.path().join("e.sock");
    let endpoint_str = endpoint.to_str().expect("utf8 path");

    let first_security = ipc
        .endpoint_security()
        .expect("endpoint_security should succeed");
    let first_listener = ipc
        .bind(endpoint_str, first_security)
        .await
        .expect("first bind must succeed");
    drop(first_listener);
    assert!(
        std::fs::symlink_metadata(&endpoint).is_ok(),
        "the stale socket file must still be on disk after the listener is dropped"
    );

    // A dropped listener's fd can remain open in a concurrently forking
    // sibling process's inherited fd table until that fork execs, so
    // `connect` against it can transiently succeed even after `drop`.
    // What this test waits for is the OS actually closing that fd, which
    // cannot be advanced with virtual time, so it polls in real time on a
    // real deadline instead.
    let start = Instant::now();
    // Only a hang guard: the fd closes once the forked sibling execs.
    let deadline = start + Duration::from_secs(15);
    loop {
        let observation = match tokio::net::UnixStream::connect(&endpoint).await {
            Err(err) if err.kind() == std::io::ErrorKind::ConnectionRefused => break,
            // Accepted into the inherited fd's backlog, then dropped when the
            // sibling closed it: the same transient state as a success.
            Err(err) if err.kind() == std::io::ErrorKind::ConnectionReset => "connect was reset",
            Err(err) => panic!(
                "waiting for the dropped listener to become unreachable failed with an \
                 unexpected error kind {:?}: {err}",
                err.kind()
            ),
            Ok(_) => "connect succeeded",
        };
        if Instant::now() >= deadline {
            panic!(
                "the dropped listener was still connectable after {:?} (last observation: \
                 {observation})",
                start.elapsed()
            );
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // bind takes `<endpoint>.lock` before it binds the socket, so a sibling
    // forked in between inherits the lock's fd without the listener's: the
    // socket is already refusing while that fork still holds the lock.
    let lock_path = dir.path().join("e.sock.lock");
    loop {
        let lock = std::fs::File::open(&lock_path).expect("the endpoint lock file must exist");
        // SAFETY: `lock` owns this fd for the call; flock only reads it.
        let fd = std::os::fd::AsRawFd::as_raw_fd(&lock);
        if unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            break;
        }
        if Instant::now() >= deadline {
            panic!(
                "the dropped listener's endpoint lock was still taken after {:?}",
                start.elapsed()
            );
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let second_security = ipc
        .endpoint_security()
        .expect("endpoint_security should succeed");
    let _second_listener = ipc.bind(endpoint_str, second_security).await.expect(
        "bind must detect the dead socket (connect fails with ConnectionRefused), unlink it, \
         and rebind once",
    );
}

// A live daemon whose queue of pending connections is full refuses a connect
// (EAGAIN on Linux, ECONNREFUSED on macOS); that is busy, not dead, so its
// socket must stay.
#[tokio::test]
async fn bind_does_not_take_over_a_live_listener_whose_backlog_is_full() {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::MetadataExt;

    let ipc = UnixIpc;
    let dir = secure_dir();
    let endpoint = dir.path().join("e.sock");
    let live = std::os::unix::net::UnixListener::bind(&endpoint).expect("bind the live listener");
    let _live_lock = hold_endpoint_lock(&endpoint);
    // SAFETY: `live` owns this listening socket for the whole call. A backlog
    // of 0 keeps the queue as short as the OS allows (macOS raises it to
    // somaxconn).
    assert_eq!(unsafe { libc::listen(live.as_raw_fd(), 0) }, 0);
    let mut waiting = Vec::new();
    loop {
        match tokio::net::UnixStream::connect(&endpoint).await {
            Ok(connection) => waiting.push(connection),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::ConnectionRefused
                ) =>
            {
                break;
            }
            Err(error) => panic!("filling the backlog failed unexpectedly: {error}"),
        }
        assert!(waiting.len() < 4096, "the backlog never filled");
    }
    let inode = std::fs::symlink_metadata(&endpoint)
        .expect("the live socket file exists")
        .ino();

    let security = ipc
        .endpoint_security()
        .expect("endpoint_security should succeed");
    let result = ipc
        .bind(endpoint.to_str().expect("utf8 path"), security)
        .await;

    assert!(
        result.is_err(),
        "a busy live listener must not be replaced: {result:?}"
    );
    assert_eq!(
        std::fs::symlink_metadata(&endpoint)
            .expect("the live socket file must still exist")
            .ino(),
        inode,
        "the live listener's socket file must not be unlinked and rebound"
    );
}

// Whether a daemon is alive is decided by the lock it holds for its whole
// lifetime, so a socket that refuses connections is still not taken over
// while that lock is held.
#[tokio::test]
async fn bind_does_not_take_over_an_endpoint_whose_lock_is_held() {
    use std::os::unix::fs::MetadataExt;

    let ipc = UnixIpc;
    let dir = secure_dir();
    let endpoint = dir.path().join("e.sock");
    drop(std::os::unix::net::UnixListener::bind(&endpoint).expect("bind the dead listener"));
    let _live_lock = hold_endpoint_lock(&endpoint);
    let inode = std::fs::symlink_metadata(&endpoint)
        .expect("the dead socket file exists")
        .ino();

    let security = ipc
        .endpoint_security()
        .expect("endpoint_security should succeed");
    let result = ipc
        .bind(endpoint.to_str().expect("utf8 path"), security)
        .await;

    match result {
        Err(OsError::EndpointHeld(_)) => {}
        other => panic!(
            "bind must refuse an endpoint whose lock is held by a live daemon, whatever a \
             connect to its socket answers; got: {other:?}"
        ),
    }
    assert_eq!(
        std::fs::symlink_metadata(&endpoint)
            .expect("the socket file must still exist")
            .ino(),
        inode,
        "the socket file of an endpoint whose lock is held must not be unlinked and rebound"
    );
}

// The lock must die with the daemon; a child that inherited it would keep the
// endpoint claimed after the daemon is gone.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_child_started_after_bind_does_not_inherit_the_endpoint_lock() {
    use std::os::unix::fs::MetadataExt;

    let ipc = UnixIpc;
    let dir = secure_dir();
    let endpoint = dir.path().join("e.sock");
    let security = ipc
        .endpoint_security()
        .expect("endpoint_security should succeed");
    let _listener = ipc
        .bind(endpoint.to_str().expect("utf8 path"), security)
        .await
        .expect("bind must succeed for a fresh endpoint");
    let lock_meta = std::fs::metadata(endpoint_lock_path(&endpoint))
        .expect("bind must leave the endpoint's lock file next to the socket");

    let mut child = spawn_own_group_child_ready("sleep 30");
    let fd_dir = format!("/proc/{}/fd", child.id());
    let inherited: Vec<String> = std::fs::read_dir(&fd_dir)
        .expect("the child's fd table is readable")
        .filter_map(|entry| {
            let path = entry.expect("fd entry").path();
            let meta = std::fs::metadata(&path).ok()?;
            (meta.dev() == lock_meta.dev() && meta.ino() == lock_meta.ino())
                .then(|| path.display().to_string())
        })
        .collect();
    child.kill().expect("kill the child");
    child.wait().expect("reap the child");

    assert!(
        inherited.is_empty(),
        "a child must not inherit the endpoint lock: {inherited:?}"
    );
}

// =====================================================================
// PeerAuth::peer_id / endpoint_allows_only
// =====================================================================

#[tokio::test]
async fn peer_id_resolves_the_connecting_uid() {
    let ipc = UnixIpc;
    let dir = secure_dir();
    let endpoint = dir.path().join("e.sock");
    let endpoint_str = endpoint.to_str().expect("utf8 path");
    let security = ipc
        .endpoint_security()
        .expect("endpoint_security should succeed");
    let mut listener = ipc
        .bind(endpoint_str, security)
        .await
        .expect("bind should succeed");

    let (accepted, connected) = tokio::join!(ipc.accept(&mut listener), ipc.connect(endpoint_str));
    let accepted = accepted.expect("accept should succeed");
    let _connected = connected.expect("connect should succeed");

    let expected = ipc
        .current_user_id()
        .expect("current_user_id should resolve");
    assert_eq!(
        ipc.peer_id(&accepted),
        Some(expected),
        "peer_id must resolve the connecting process's uid via the platform's peer-credential \
         API (Linux: SO_PEERCRED; macOS: getpeereid)"
    );
}

#[tokio::test]
async fn endpoint_allows_only_true_for_a_securely_bound_endpoint() {
    let ipc = UnixIpc;
    let dir = secure_dir();
    let endpoint = dir.path().join("e.sock");
    let endpoint_str = endpoint.to_str().expect("utf8 path");
    let security = ipc
        .endpoint_security()
        .expect("endpoint_security should succeed");
    let _listener = ipc
        .bind(endpoint_str, security)
        .await
        .expect("bind should succeed");

    let current = ipc
        .current_user_id()
        .expect("current_user_id should resolve");
    let allows = ipc
        .endpoint_allows_only(endpoint_str, &current)
        .expect("endpoint_allows_only should resolve for a bound endpoint");
    assert!(
        allows,
        "endpoint_allows_only must return true for the current user on an endpoint created \
         through bind()"
    );
}

#[tokio::test]
async fn endpoint_allows_only_false_for_a_permissively_created_socket() {
    // Counter-proof against a rubber-stamp `endpoint_allows_only`: a bare
    // `UnixListener::bind` plus an explicit world-writable chmod produces a
    // socket `UnixIpc::bind` would never create.
    let dir = secure_dir();
    let endpoint = dir.path().join("e.sock");
    let _raw = std::os::unix::net::UnixListener::bind(&endpoint).expect("raw bind should succeed");
    std::fs::set_permissions(&endpoint, std::fs::Permissions::from_mode(0o666))
        .expect("chmod the raw socket to 0666");

    let ipc = UnixIpc;
    let current = ipc
        .current_user_id()
        .expect("current_user_id should resolve");
    let allows = ipc
        .endpoint_allows_only(endpoint.to_str().expect("utf8 path"), &current)
        .expect("endpoint_allows_only should resolve for an existing socket file");
    assert!(
        !allows,
        "endpoint_allows_only must return false for a socket whose mode grants group/other \
         access, even when checked against the current user's own id (counter-proof)"
    );
}

#[test]
fn endpoint_allows_only_false_for_an_owner_only_regular_file_at_the_endpoint() {
    let dir = secure_dir();
    let endpoint = dir.path().join("e.sock");
    std::fs::write(&endpoint, b"not a socket").expect("write plain file");
    std::fs::set_permissions(&endpoint, std::fs::Permissions::from_mode(0o600))
        .expect("chmod the plain file to 0600");

    let ipc = UnixIpc;
    let current = ipc
        .current_user_id()
        .expect("current_user_id should resolve");
    let allows = ipc
        .endpoint_allows_only(endpoint.to_str().expect("utf8 path"), &current)
        .expect("endpoint_allows_only should resolve for an existing file");
    assert!(
        !allows,
        "a regular file is not an endpoint, even when only its owner can reach it"
    );
}

// =====================================================================
// UnixProcessGroup::led_by: leader validation
// =====================================================================

#[test]
fn led_by_rejects_our_own_process_group_and_pid_zero() {
    let own = own_pgrp();
    assert!(own > 0, "fixture precondition: getpgrp() is a positive id");

    // `led_by` compares the pid with getpgrp(), so the leader's pid is passed
    // as our own group id directly (no child shares our group here).
    let result = UnixProcessGroup::led_by(own as u32, Duration::from_millis(50));
    assert!(
        result.is_err(),
        "led_by with a pid equal to our own getpgrp() must be rejected (self-group protection)"
    );

    let zero = UnixProcessGroup::led_by(0, Duration::from_millis(50));
    assert!(
        zero.is_err(),
        "led_by(0, ..) must be rejected (killpg(0, ..) would signal our own group)"
    );
}

// =====================================================================
// UnixProcessGroup::terminate: SIGTERM -> SIGKILL, idempotence
// =====================================================================

#[test]
fn terminate_group_terminates_a_cooperating_child() {
    let mut child = spawn_own_group_child("exec sleep 30");
    let group = UnixProcessGroup::led_by(child.id(), Duration::from_secs(5))
        .expect("led_by should succeed for a group leader");

    group.terminate().expect("terminate_group should succeed");
    assert!(
        wait_until_exited(&mut child, Duration::from_secs(2)),
        "a plain child (default DEFAULT_TERM_GRACE_MS) must be terminated"
    );
}

#[test]
fn terminate_group_escalates_to_sigkill_after_the_injected_grace_period() {
    let mut child = spawn_own_group_child_ready("sleep 30");
    let group = UnixProcessGroup::led_by(child.id(), Duration::from_millis(50))
        .expect("led_by should succeed for a group leader");

    group.terminate().expect("terminate_group should succeed");
    assert!(
        wait_until_exited(&mut child, Duration::from_secs(2)),
        "a child that ignores SIGTERM must still die once the injected grace period elapses \
         and SIGKILL is sent"
    );
}

#[test]
fn terminate_group_with_zero_grace_kills_immediately() {
    let mut child = spawn_own_group_child_ready("sleep 30");
    let group = UnixProcessGroup::led_by(child.id(), Duration::ZERO)
        .expect("led_by should succeed for a group leader");

    let started = Instant::now();
    group.terminate().expect("terminate_group should succeed");
    assert!(
        wait_until_exited(&mut child, Duration::from_millis(500)),
        "a zero grace must skip straight to SIGKILL, even for a child that ignores SIGTERM"
    );
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "ZERO grace must not wait out any grace period before escalating"
    );
}

#[test]
fn terminate_group_is_idempotent_on_a_second_call() {
    let mut child = spawn_own_group_child("exec sleep 5");
    let group = UnixProcessGroup::led_by(child.id(), Duration::from_millis(50))
        .expect("led_by should succeed for a group leader");

    group
        .terminate()
        .expect("first terminate_group call should succeed");
    assert!(wait_until_exited(&mut child, Duration::from_secs(2)));

    group.terminate().expect(
        "a second terminate_group call on an already-terminated group must be a no-op, not \
         an error",
    );
}

/// The group's sole member is adopted while it is still alive, then killed
/// and confirmed as an unreaped zombie before `terminate_group` runs, so
/// `terminate_group` observes exactly the state macOS's `killpg` reports as
/// `EPERM` (a process group that still exists but has no live member) --
/// which must still be reported as success, not a termination error.
#[test]
fn terminate_group_reports_success_when_every_member_has_exited_but_none_is_reaped() {
    let mut child = spawn_own_group_child_ready("sleep 30");
    let group = UnixProcessGroup::led_by(child.id(), Duration::ZERO)
        .expect("led_by should succeed for a group leader");

    child.kill().expect("kill must succeed for a child we own");
    wait_until_zombie(child.id());

    let outcome = group.terminate();
    assert!(
        outcome.is_ok(),
        "a group whose every member has already exited must not be reported as a termination \
         error: {outcome:?}"
    );

    child
        .wait()
        .expect("wait must not error for a child we own");
}

// A window whose child exited on its own is still ended through its group,
// which by then has no member at all (ESRCH).
#[test]
fn terminate_group_reports_success_when_every_member_has_exited_and_been_reaped() {
    let mut child = spawn_own_group_child("exit 0");
    let group = UnixProcessGroup::led_by(child.id(), Duration::ZERO)
        .expect("led_by should succeed for a group leader");
    child
        .wait()
        .expect("wait must not error for a child we own");

    let outcome = group.terminate();

    assert!(
        outcome.is_ok(),
        "a group with no member left must not be reported as a termination error: {outcome:?}"
    );
}

// =====================================================================
// Drop (SIGKILL only when not yet terminated and pgid known)
// =====================================================================

#[test]
fn dropping_an_unterminated_group_kills_its_process() {
    let mut child = spawn_own_group_child("exec sleep 30");
    let group = UnixProcessGroup::led_by(child.id(), Duration::from_millis(50))
        .expect("led_by should succeed for a group leader");

    drop(group);

    assert!(
        wait_until_exited(&mut child, Duration::from_secs(2)),
        "dropping a group that was never explicitly terminated must still kill its process \
         (Drop)"
    );
}

#[test]
fn dropping_a_group_whose_members_have_all_exited_does_not_panic() {
    let mut child = spawn_own_group_child_ready("sleep 30");
    let group = UnixProcessGroup::led_by(child.id(), Duration::ZERO)
        .expect("led_by should succeed for a group leader");

    child.kill().expect("kill must succeed for a child we own");
    wait_until_zombie(child.id());

    drop(group);

    let _ = child.wait();
}

// =====================================================================
// secure_owned_dir / secure_owned_file
// =====================================================================

#[test]
fn secure_owned_dir_and_secure_owned_file_restrict_permissions_idempotently_and_fail_soft() {
    // (a) 0700/0600: every missing component from `base` (exclusive) through
    // `leaf` (inclusive) is created at 0700, while `base` itself is left at
    // whatever `create_dir_all` gave it (plain, no explicit mode) -- it may
    // be a directory shared with other applications.
    let base = TempDir::new("os-unix", "secure-owned-create");
    let base_mode_before = std::fs::metadata(base.path())
        .expect("base must exist")
        .permissions()
        .mode()
        & 0o777;
    let leaf = base.path().join("yagura").join("logs");
    secure_owned_dir(base.path(), &leaf).expect("secure_owned_dir must succeed");
    for dir in [base.path().join("yagura"), leaf.clone()] {
        let meta = std::fs::symlink_metadata(&dir).expect("component must have been created");
        assert!(meta.is_dir(), "{dir:?} must be a real directory");
        assert_eq!(
            meta.permissions().mode() & 0o777,
            0o700,
            "{dir:?} must be created with mode 0700"
        );
    }
    assert_eq!(
        std::fs::metadata(base.path())
            .expect("base must still exist")
            .permissions()
            .mode()
            & 0o777,
        base_mode_before,
        "base itself must never be chmod'd"
    );

    let file = leaf.join("secret.db");
    std::fs::write(&file, b"data").expect("write fixture file");
    secure_owned_file(&file).expect("secure_owned_file must succeed");
    assert_eq!(
        std::fs::metadata(&file)
            .expect("file must exist")
            .permissions()
            .mode()
            & 0o777,
        0o600,
        "secure_owned_file must narrow a file to 0600"
    );

    // (b) idempotent restrict: an already-existing dir/file left at a looser
    // mode (as an older build's would be) is tightened on the next call, and
    // a repeat call leaves an already-restricted one unchanged.
    std::fs::set_permissions(&leaf, std::fs::Permissions::from_mode(0o755))
        .expect("chmod the existing leaf to a looser mode");
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644))
        .expect("chmod the existing file to a looser mode");
    for _ in 0..2 {
        secure_owned_dir(base.path(), &leaf).expect("a repeat secure_owned_dir call must succeed");
        secure_owned_file(&file).expect("a repeat secure_owned_file call must succeed");
    }
    assert_eq!(
        std::fs::metadata(&leaf)
            .expect("leaf must exist")
            .permissions()
            .mode()
            & 0o777,
        0o700,
        "a looser existing leaf must be tightened to 0700, and stay 0700 on repeat calls"
    );
    assert_eq!(
        std::fs::metadata(&file)
            .expect("file must exist")
            .permissions()
            .mode()
            & 0o777,
        0o600,
        "a looser existing file must be tightened to 0600, and stay 0600 on repeat calls"
    );

    // (c) fail soft: a failure is reported through the Result (a
    // human-readable, non-empty reason), never a panic, so a caller can log
    // it and keep running with weaker protection.
    let blocking_file = base.path().join("blocks-a-directory");
    std::fs::write(&blocking_file, b"not a directory").expect("write blocking file fixture");
    let unreachable_leaf = blocking_file.join("logs");
    let dir_failure = secure_owned_dir(base.path(), &unreachable_leaf);
    assert!(
        dir_failure.is_err(),
        "a directory component whose parent is a plain file (not a directory) must fail to \
         create, surfaced through the Result rather than a panic"
    );
    assert!(!dir_failure.unwrap_err().is_empty());

    let missing_file = base.path().join("does-not-exist.db");
    let file_failure = secure_owned_file(&missing_file);
    assert!(
        file_failure.is_err(),
        "restricting a file that does not exist must fail through the Result, not panic"
    );
    assert!(!file_failure.unwrap_err().is_empty());
}

// =====================================================================
// secure_owned_dir: symlink rejection
// =====================================================================

#[test]
fn secure_owned_dir_rejects_a_symlinked_intermediate_component_without_chmod_ing_its_target() {
    let base = TempDir::new("os-unix", "secure-owned-symlink-mid");
    let target = TempDir::new("os-unix", "secure-owned-symlink-mid-target");
    // Set explicitly rather than relying on however the fixture directory
    // was created: under a strict umask, "the target's mode is unchanged"
    // would otherwise hold trivially even if the symlink rejection did not
    // exist.
    std::fs::set_permissions(target.path(), std::fs::Permissions::from_mode(0o755))
        .expect("chmod the symlink target to 0755 before the call");

    let mid = base.path().join("yagura");
    std::os::unix::fs::symlink(target.path(), &mid).expect("symlink creation should succeed");
    let leaf = mid.join("logs");

    let result = secure_owned_dir(base.path(), &leaf);
    assert!(
        result.is_err(),
        "a symlinked intermediate component must be rejected with an Err, not chmod'd or \
         descended into"
    );
    assert_eq!(
        std::fs::metadata(target.path())
            .expect("the symlink target must still exist")
            .permissions()
            .mode()
            & 0o777,
        0o755,
        "the symlink target's mode must be left untouched, not narrowed to 0700"
    );
    assert!(
        std::fs::symlink_metadata(&mid)
            .expect("the intermediate path must still exist")
            .is_symlink(),
        "the intermediate path must remain a symlink, not replaced or resolved"
    );
}

#[test]
fn secure_owned_dir_rejects_a_symlinked_leaf_without_chmod_ing_its_target() {
    let base = TempDir::new("os-unix", "secure-owned-symlink-leaf");
    let target = TempDir::new("os-unix", "secure-owned-symlink-leaf-target");
    std::fs::set_permissions(target.path(), std::fs::Permissions::from_mode(0o755))
        .expect("chmod the symlink target to 0755 before the call");

    let mid = base.path().join("yagura");
    std::fs::create_dir(&mid).expect("create the real intermediate directory");
    let leaf = mid.join("logs");
    std::os::unix::fs::symlink(target.path(), &leaf).expect("symlink creation should succeed");

    let result = secure_owned_dir(base.path(), &leaf);
    assert!(
        result.is_err(),
        "a symlinked leaf component must be rejected with an Err, not chmod'd"
    );
    assert_eq!(
        std::fs::metadata(target.path())
            .expect("the symlink target must still exist")
            .permissions()
            .mode()
            & 0o777,
        0o755,
        "the symlink target's mode must be left untouched, not narrowed to 0700"
    );
    assert!(
        std::fs::symlink_metadata(&leaf)
            .expect("the leaf path must still exist")
            .is_symlink(),
        "the leaf path must remain a symlink, not replaced or resolved"
    );
}

// =====================================================================
// seed_config_file: an owner-only config root
// =====================================================================

// The config root holds the playbook that sets the approval policy, so a
// directory another user could write to would let them loosen it.
#[test]
fn seed_config_file_narrows_a_loose_config_root_to_owner_only_before_writing() {
    let base = TempDir::new("os-unix", "seed-config-owner-only");
    let namespace = resolve_instance_from(None).expect("the default instance always resolves");
    let root = instance_dir(base.path(), &namespace);
    std::fs::create_dir(&root).expect("create the config root");
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o777))
        .expect("leave the config root world-writable, as another process could");

    let outcome = seed_config_file(
        base.path(),
        &namespace,
        Some("profiles"),
        "shell.toml",
        "schema = 1
",
    )
    .expect("seeding under a writable base must succeed");
    assert_eq!(outcome, SeedOutcome::Created);

    let mode = |path: &std::path::Path| {
        std::fs::metadata(path)
            .expect("seeded path must exist")
            .permissions()
            .mode()
            & 0o777
    };
    assert_eq!(
        mode(&root),
        0o700,
        "the config root must be narrowed to 0700"
    );
    assert_eq!(
        mode(&root.join("profiles")),
        0o700,
        "the nested directory must be created at 0700"
    );
    assert_eq!(
        mode(&root.join("profiles").join("shell.toml")),
        0o600,
        "the seeded file must be owner-only"
    );
}

#[test]
fn seed_config_file_writes_nothing_through_a_symlinked_config_directory() {
    let base = TempDir::new("os-unix", "seed-config-symlink");
    let target = TempDir::new("os-unix", "seed-config-symlink-target");
    let namespace = resolve_instance_from(None).expect("the default instance always resolves");
    let root = instance_dir(base.path(), &namespace);
    std::os::unix::fs::symlink(target.path(), &root).expect("symlink creation should succeed");

    let result = seed_config_file(
        base.path(),
        &namespace,
        None,
        "playbook.toml",
        "x = 1
",
    );
    assert!(
        result.is_err(),
        "a symlinked config root must be rejected before anything is written, got {result:?}"
    );
    assert!(
        !target.path().join("playbook.toml").exists(),
        "no file may be written into the symlink's target"
    );
}

// =====================================================================
// schedule_force_kill. The child is spawned through this file's own
// spawn_own_group_child helper: `exec` keeps the pid stable and
// grandchild-free, and no SIGHUP suppression is needed because this function
// only ever sends SIGKILL.
// =====================================================================

#[cfg(unix)]
#[test]
fn schedule_force_kill_sigkills_the_child_after_the_grace_period_when_exit_was_not_observed() {
    let mut child = spawn_own_group_child("exec sleep 3600");
    let pid = child.id();

    yagura_os::schedule_force_kill(
        pid,
        Duration::from_millis(50),
        Arc::new(AtomicBool::new(false)),
    );

    let status = child
        .wait()
        .expect("wait must not error for a child we own");
    assert_eq!(
        status.signal(),
        Some(libc::SIGKILL),
        "schedule_force_kill must SIGKILL the child once the grace period elapses with \
         `exited` still false; status: {status:?}"
    );
}

#[test]
fn schedule_force_kill_does_not_kill_before_the_grace_period_has_passed() {
    const GRACE: Duration = Duration::from_millis(300);
    let mut child = spawn_own_group_child("exec sleep 3600");
    let started = Instant::now();

    yagura_os::schedule_force_kill(child.id(), GRACE, Arc::new(AtomicBool::new(false)));

    let status = child
        .wait()
        .expect("wait must not error for a child we own");
    assert_eq!(status.signal(), Some(libc::SIGKILL), "status: {status:?}");
    assert!(
        started.elapsed() >= GRACE,
        "the child was killed {:?} after the call, before the {GRACE:?} grace period",
        started.elapsed()
    );
}

#[cfg(unix)]
#[test]
fn schedule_force_kill_does_not_kill_when_exited_is_already_true() {
    let mut child = spawn_own_group_child("exec sleep 3600");
    let pid = child.id();

    yagura_os::schedule_force_kill(
        pid,
        Duration::from_millis(50),
        Arc::new(AtomicBool::new(true)),
    );

    std::thread::sleep(Duration::from_millis(200));
    assert!(
        child
            .try_wait()
            .expect("try_wait must not error for a child we own")
            .is_none(),
        "schedule_force_kill must not send SIGKILL once `exited` is already true (the \
         self-observed-exit suppression branch)"
    );

    let _ = child.kill();
    let _ = child.wait();
}

// =====================================================================
// spawn_daemon_detached
// =====================================================================

#[test]
fn spawn_daemon_detached_starts_the_program_as_the_leader_of_a_new_session() {
    let dir = secure_dir();
    let fifo = dir.path().join("pid");
    let fifo_c = std::ffi::CString::new(fifo.to_str().expect("utf8 path")).expect("no NUL");
    // SAFETY: `fifo_c` is a NUL-terminated path that outlives the call.
    assert_eq!(unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) }, 0);

    // The fifo's path is passed as `$0`, never spliced into the script.
    let spawned = yagura_os::spawn_daemon_detached(
        std::path::Path::new("/bin/sh"),
        &[
            OsStr::new("-c"),
            OsStr::new("echo $$ > \"$0\"; exec sleep 30"),
            fifo.as_os_str(),
        ],
    )
    .expect("spawn_daemon_detached should start /bin/sh");
    let (tx, rx) = mpsc::channel();
    let fifo_path = fifo.clone();
    std::thread::spawn(move || {
        let _ = tx.send(std::fs::read_to_string(fifo_path));
    });
    // Only a hang guard: the read returns as soon as the child has written.
    let pid: libc::pid_t = rx
        .recv_timeout(Duration::from_secs(10))
        .expect("the detached child never reported its pid")
        .expect("read the fifo")
        .trim()
        .parse()
        .expect("the child reports a numeric pid");

    // SAFETY: getsid only reads the session id of the pid supplied.
    let (session, own_session) = unsafe { (libc::getsid(pid), libc::getsid(0)) };
    // SAFETY: `pid` is the detached child this test started; its reaper
    // thread has not reaped it, since it is still sleeping.
    unsafe { libc::kill(pid, libc::SIGKILL) };
    assert!(!spawned.used_fallback);
    assert_eq!(session, pid, "the daemon must lead its own session");
    assert_ne!(session, own_session);
}

// =====================================================================
// spawn_captured: a child that exits before its process group is read
// back. `spawn_captured` starts the child with `Command::spawn`, then
// reads its process group back in a later, uncoordinated step
// (`getpgid` on the spawned PID). On macOS `getpgid`
// fails once the child has already become a zombie; on Linux it keeps
// working for a zombie, so this only reproduces on macOS. A single caller
// rarely loses this race (the parent's own instructions between
// `Command::spawn` returning and the `getpgid` call are few), but
// scheduler contention from concurrent callers widens the window; a real
// fixture measurement on the target macOS hardware (16 threads x 300
// attempts of the minimal fork/setsid/getpgid sequence, no production code
// involved) hit it 9 times out of 4800. This test spreads its attempts
// across threads for the same reason.
// =====================================================================

#[test]
fn spawn_captured_concurrently_on_immediately_exiting_children_never_fails_to_start() {
    use yagura_os::{CaptureEnv, CaptureLimits, CaptureOutcome};

    const THREADS: usize = 16;
    const ATTEMPTS_PER_THREAD: usize = 300;

    let limits = CaptureLimits {
        timeout: Duration::from_secs(5),
        max_output_bytes: 4096,
    };

    let (tx, rx) = mpsc::channel::<String>();
    let handles: Vec<_> = (0..THREADS)
        .map(|thread_index| {
            let tx = tx.clone();
            std::thread::spawn(move || {
                let argv = vec!["/usr/bin/true".to_string()];
                for attempt in 0..ATTEMPTS_PER_THREAD {
                    match yagura_os::spawn_captured(&argv, None, CaptureEnv::Inherit(&[]), None) {
                        Err(err) => {
                            let _ = tx.send(format!(
                                "thread {thread_index} attempt {attempt}: spawn_captured failed \
                                 to start: {err}"
                            ));
                        }
                        Ok(child) => match child.collect(limits) {
                            CaptureOutcome::Exited { code: Some(0), .. } => {}
                            other => {
                                let _ = tx.send(format!(
                                    "thread {thread_index} attempt {attempt}: did not exit \
                                     cleanly: {other:?}"
                                ));
                            }
                        },
                    }
                }
            })
        })
        .collect();
    drop(tx);

    for handle in handles {
        handle.join().expect("worker thread must not panic");
    }
    let failures: Vec<String> = rx.into_iter().collect();
    assert!(
        failures.is_empty(),
        "spawn_captured must never fail to start an immediately-exiting child: \
         {failures:#?}"
    );
}

// =====================================================================
// resolve_endpoint_with: the sun_path boundary on Linux and macOS.
// =====================================================================

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod sun_path_limit {
    use yagura_os::platform_unix::{EndpointCreation, resolve_endpoint_with};
    use yagura_test_support::paths::short_tmp_dir;

    use super::default_namespace;

    /// Largest usable length of `sockaddr_un.sun_path`: 108 bytes on Linux
    /// and 104 on macOS, minus 1 for the mandatory NUL terminator. An
    /// independent oracle -- not a reference into `platform_unix`'s private
    /// `SUN_PATH_MAX_LEN` -- so this test is not a tautology of the
    /// implementation it exercises.
    #[cfg(target_os = "linux")]
    const SUN_PATH_MAX_LEN: usize = 107;
    #[cfg(target_os = "macos")]
    const SUN_PATH_MAX_LEN: usize = 103;

    /// The `user_id` length that makes `{tmp_root}/yagura-{user_id}/ipc.sock`
    /// exactly `SUN_PATH_MAX_LEN + over_by` bytes long, given `tmp_root`'s
    /// own length. Fails loud (never skips) when `tmp_root` is already too
    /// long to leave room for a non-empty `user_id`, so an oversized `$TMPDIR`
    /// is relayed as an actionable fact rather than silently swallowed.
    fn user_id_len_for(tmp_root_len: usize, over_by: usize) -> usize {
        let fixed_len = "/yagura-".len() + "/ipc.sock".len();
        let budget = SUN_PATH_MAX_LEN + over_by;
        if budget <= tmp_root_len + fixed_len {
            panic!(
                "$TMPDIR is too long for this fixture's boundary-construction assumptions \
                 (short_tmp_dir's bounded /tmp origin): tmp_root length {tmp_root_len}, budget \
                 {budget}"
            );
        }
        budget - tmp_root_len - fixed_len
    }

    #[test]
    fn resolve_endpoint_with_accepts_a_candidate_path_exactly_at_the_sun_path_limit() {
        let tmp_root = short_tmp_dir("os-unix", "sun-path-ok");
        let tmp_root_len = tmp_root.path().to_str().expect("utf8 path").len();
        let user_id = "u".repeat(user_id_len_for(tmp_root_len, 0));

        let endpoint = resolve_endpoint_with(
            &user_id,
            None,
            tmp_root.path(),
            EndpointCreation::CreateIfMissing,
            &default_namespace(),
        )
        .expect("a candidate exactly at the sun_path limit must be accepted");
        assert_eq!(
            endpoint.len(),
            SUN_PATH_MAX_LEN,
            "fixture check: the constructed candidate must land exactly on the boundary"
        );
        std::os::unix::net::UnixListener::bind(&endpoint)
            .expect("oracle check: the kernel itself accepts a path of this length");
    }

    #[test]
    fn resolve_endpoint_with_rejects_a_candidate_path_one_byte_over_the_sun_path_limit() {
        let tmp_root = short_tmp_dir("os-unix", "sun-path-over");
        let tmp_root_len = tmp_root.path().to_str().expect("utf8 path").len();
        let user_id = "u".repeat(user_id_len_for(tmp_root_len, 1));

        let result = resolve_endpoint_with(
            &user_id,
            None,
            tmp_root.path(),
            EndpointCreation::CreateIfMissing,
            &default_namespace(),
        );
        assert!(
            result.is_err(),
            "a candidate one byte over the sun_path limit must be rejected: {result:?}"
        );
    }
}
