#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// The processes here are fixtures the tests start, move and end themselves.
#![allow(clippy::disallowed_methods)]

//! Integration test (Linux): the names of a window's systemd user scopes and
//! the requests that move a window's processes into them, by pidfd, through
//! the user's systemd user manager.
//!
//! The tests that need a running user manager are ignored, as whether a
//! machine runs one is not known in advance; run them with
//! `--include-ignored` where one runs.

use std::ffi::OsStr;
use std::io::Write as _;
use std::os::fd::{AsFd as _, FromRawFd as _, OwnedFd};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use yagura_os::{Liveness, ProcessHandle};
use yagura_test_support::HANG_GUARD;
use yagura_test_support::paths::short_tmp_dir;

const POLL: Duration = Duration::from_millis(20);

/// Whether `name` is a unit name of the form a window's scope must have.
fn is_scope_name(name: &str) -> bool {
    name.strip_suffix(".scope").is_some_and(|stem| {
        !stem.is_empty()
            && stem
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    })
}

fn pidfd_of(pid: u32) -> OwnedFd {
    let pid = libc::pid_t::try_from(pid).unwrap();
    // SAFETY: pidfd_open takes a pid and flags and returns a new descriptor or -1.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
    assert!(
        fd >= 0,
        "pidfd_open({pid}): {}",
        std::io::Error::last_os_error()
    );
    // SAFETY: the descriptor was just returned to this function alone.
    unsafe { OwnedFd::from_raw_fd(libc::c_int::try_from(fd).unwrap()) }
}

/// The cgroup path `/proc/<pid>/cgroup` names; empty once the process is gone.
fn cgroup_of(pid: u32) -> String {
    std::fs::read_to_string(format!("/proc/{pid}/cgroup"))
        .unwrap_or_default()
        .lines()
        .find_map(|line| line.strip_prefix("0::"))
        .unwrap_or_default()
        .to_owned()
}

fn in_scope(pid: u32, scope: &str) -> bool {
    cgroup_of(pid).ends_with(&format!("/{scope}"))
}

/// The children of every thread of the process `/proc/<pid>` names (`self`
/// for this one).
fn children_of(pid: &str) -> Vec<u32> {
    let mut children = Vec::new();
    for task in std::fs::read_dir(format!("/proc/{pid}/task"))
        .unwrap()
        .flatten()
    {
        let text = std::fs::read_to_string(task.path().join("children")).unwrap_or_default();
        children.extend(text.split_whitespace().map(|id| id.parse::<u32>().unwrap()));
    }
    children.sort_unstable();
    children
}

/// Stops, as a test ends, the scopes it may have made.
struct StopScopes(Vec<String>);

impl Drop for StopScopes {
    fn drop(&mut self) {
        for name in &self.0 {
            let _ = Command::new("systemctl")
                .args(["--user", "stop", "--quiet", name])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
}

/// Kills, as a test ends, the processes it left running.
#[derive(Default)]
struct EndOnDrop(Vec<ProcessHandle>);

impl Drop for EndOnDrop {
    fn drop(&mut self) {
        for process in &self.0 {
            let _ = process.kill();
        }
    }
}

fn handle_of(pid: u32) -> ProcessHandle {
    let start = yagura_os::process_start(pid)
        .unwrap()
        .expect("the process runs");
    ProcessHandle::open(pid, &start)
        .unwrap()
        .expect("the process runs")
}

/// Waits on `process`'s own descriptor until it has exited.
async fn wait_ended(process: &ProcessHandle, what: &str) {
    let watch = process.watch_exit().unwrap();
    tokio::time::timeout(
        HANG_GUARD,
        tokio::task::spawn_blocking(move || watch.wait()),
    )
    .await
    .unwrap_or_else(|_| panic!("{what} never ended"))
    .unwrap()
    .unwrap();
    assert_eq!(process.liveness(), Liveness::Dead, "{what} still runs");
}

fn wait_exit(child: &mut Child, what: &str) -> std::process::ExitStatus {
    let deadline = std::time::Instant::now() + HANG_GUARD;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("{what} never ended");
        }
        std::thread::sleep(POLL);
    }
}

#[test]
fn scope_names_are_unit_names_of_letters_digits_and_underscores_and_new_for_each_holder_start() {
    for instance in [None, Some("dev-2")] {
        let namespace = yagura_os::resolve_instance_from(instance.map(OsStr::new)).unwrap();
        let first = yagura_os::platform_unix::scope_names(&namespace, "w0123abcd")
            .expect("make a window's scope names");
        let second = yagura_os::platform_unix::scope_names(&namespace, "w0123abcd")
            .expect("make a window's scope names");
        for name in [&first.holder, &first.child, &second.holder, &second.child] {
            assert!(
                is_scope_name(name),
                "{name:?} (instance {instance:?}) is not [A-Za-z0-9_] followed by .scope"
            );
        }
        assert_ne!(
            first.holder, first.child,
            "the holder's scope and the child's scope share a name"
        );
        assert_ne!(
            first.holder, second.holder,
            "a holder's scope name is used again for the same window's next holder"
        );
        assert_ne!(
            first.child, second.child,
            "a child's scope name is used again for the same window's next holder"
        );
    }
}

const BUS_PROBE_ENV: &str = "YAGURA_OS_TEST_BUS_PROBE";
const NO_BUS_TEST: &str = "a_request_without_a_reachable_user_bus_fails_and_starts_no_process";
/// The probe's exit codes.
const PROBE_REFUSED: i32 = 0;
const PROBE_ACCEPTED: i32 = 3;
const PROBE_SPAWNED: i32 = 4;

/// Percent-encodes `value` for a D-Bus address.
fn dbus_escaped(value: &str) -> String {
    value
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || b"-_/.*".contains(&byte) {
                char::from(byte).to_string()
            } else {
                format!("%{byte:02x}")
            }
        })
        .collect()
}

/// In a process of its own, whose environment names no reachable user bus:
/// the request fails and no process is started for it. The environment is the
/// probe process's, never this test process's.
#[test]
fn a_request_without_a_reachable_user_bus_fails_and_starts_no_process() {
    if std::env::var_os(BUS_PROBE_ENV).is_some() {
        let before = children_of("self");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let own = pidfd_of(std::process::id());
        let outcome = runtime.block_on(async {
            let manager = yagura_os::platform_unix::UserManager::connect()
                .await
                .map_err(|error| error.to_string())?;
            manager
                .start_holder_scope("yagura_test_no_bus_holder.scope", &[own.as_fd()])
                .await
                .map_err(|error| error.to_string())
        });
        let after = children_of("self");
        println!("PROBE outcome={outcome:?} children before={before:?} after={after:?}");
        let code = if outcome.is_ok() {
            PROBE_ACCEPTED
        } else if after != before {
            PROBE_SPAWNED
        } else {
            PROBE_REFUSED
        };
        std::process::exit(code);
    }

    let dir = short_tmp_dir("um", "nobus");
    let run_dir = dir.path().join("run");
    std::fs::create_dir(&run_dir).unwrap();
    let marker = dir.path().join("spawned");
    let unixexec = format!(
        "unixexec:path=/bin/sh,argv1=-c,argv2={}",
        dbus_escaped(&format!("touch {}", marker.display()))
    );
    let scenes: [(&str, Option<&str>); 3] = [
        ("no bus address and an empty runtime directory", None),
        ("an address that would start a process", Some(&unixexec)),
        (
            "an address asking for an autolaunched bus",
            Some("autolaunch:"),
        ),
    ];
    let exe = std::env::current_exe().unwrap();
    let test = yagura_test_support::process::libtest_name(module_path!(), NO_BUS_TEST);
    for (scene, address) in scenes {
        let mut command = Command::new(&exe);
        command
            .args(["--exact", &test, "--nocapture", "--test-threads=1"])
            .env(BUS_PROBE_ENV, "1")
            .env("XDG_RUNTIME_DIR", &run_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        match address {
            Some(address) => command.env("DBUS_SESSION_BUS_ADDRESS", address),
            None => command.env_remove("DBUS_SESSION_BUS_ADDRESS"),
        };
        let mut probe = command.spawn().unwrap();
        let status = wait_exit(&mut probe, "the probe");
        let mut said = String::new();
        if let Some(mut out) = probe.stdout.take() {
            let _ = std::io::Read::read_to_string(&mut out, &mut said);
        }
        assert_eq!(
            status.code(),
            Some(PROBE_REFUSED),
            "{scene}: the request did not fail without starting a process \
             ({PROBE_ACCEPTED} accepted, {PROBE_SPAWNED} started a process): {said}"
        );
        assert!(
            !marker.exists(),
            "{scene}: the bus address made the request start a process"
        );
    }
}

/// Starts `/bin/sh -c <script> sh <args>...` with its stdin kept open.
fn shell(script: &str, args: &[&Path]) -> Child {
    Command::new("/bin/sh")
        .arg("-c")
        .arg(script)
        .arg("sh")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap()
}

/// A leader that, once told on its stdin, starts a grandchild in a session of
/// its own and a member of its group, all three ignoring SIGTERM and SIGHUP,
/// and writes the two ids to its first argument.
const LEADER: &str = r#"
trap '' TERM HUP
read go
setsid /bin/sh -c 'exec tail -f /dev/null' </dev/null >/dev/null 2>&1 &
echo $! > "$1.staged"
/bin/sh -c 'exec tail -f /dev/null' </dev/null >/dev/null 2>&1 &
echo $! >> "$1.staged"
mv "$1.staged" "$1"
read rest
"#;

async fn read_pids(path: &PathBuf) -> Vec<u32> {
    let deadline = tokio::time::Instant::now() + HANG_GUARD;
    loop {
        if let Ok(text) = std::fs::read_to_string(path) {
            return text
                .split_whitespace()
                .map(|id| id.parse().unwrap())
                .collect();
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the leader never started its tree"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// A holder's scope holds the processes its pidfds name (the holder and its
/// watcher) once the request's job is done, and a child's scope bound to it
/// holds the leader; a grandchild in a session of its own and a member
/// ignoring SIGTERM, started inside, end with the rest of the child's scope
/// once the holder and the watcher are killed. A pidfd of a process already
/// reaped is refused and makes no unit.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the user's systemd user manager; run with --include-ignored where one runs"]
async fn a_childs_scope_bound_to_its_holders_scope_ends_whole_when_the_holder_is_killed() {
    let namespace = yagura_os::resolve_instance_from(None).unwrap();
    let names = yagura_os::platform_unix::scope_names(&namespace, "w0scope01").unwrap();
    let refused = yagura_os::platform_unix::scope_names(&namespace, "w0scope02").unwrap();
    let _scopes = StopScopes(vec![
        names.child.clone(),
        names.holder.clone(),
        refused.holder.clone(),
    ]);
    let dir = short_tmp_dir("um", "scope");
    let pid_file = dir.path().join("tree");
    let mut left = EndOnDrop::default();

    let mut holder = shell("read line", &[]);
    let mut watcher = shell("read line", &[]);
    let mut leader = shell(LEADER, &[&pid_file]);
    left.0.push(handle_of(holder.id()));
    left.0.push(handle_of(watcher.id()));
    left.0.push(handle_of(leader.id()));
    let holder_fd = pidfd_of(holder.id());
    let watcher_fd = pidfd_of(watcher.id());
    let leader_fd = pidfd_of(leader.id());

    let manager = yagura_os::platform_unix::UserManager::connect()
        .await
        .expect("reach the user manager");
    manager
        .start_holder_scope(&names.holder, &[holder_fd.as_fd(), watcher_fd.as_fd()])
        .await
        .expect("the holder's scope");
    for (what, pid) in [("the holder", holder.id()), ("the watcher", watcher.id())] {
        assert!(
            in_scope(pid, &names.holder),
            "{what} is not in the holder's scope once the job is done: {}",
            cgroup_of(pid)
        );
    }
    manager
        .start_child_scope(&names.child, &names.holder, &[leader_fd.as_fd()])
        .await
        .expect("the child's scope");
    assert!(
        in_scope(leader.id(), &names.child),
        "the leader is not in the child's scope once the job is done: {}",
        cgroup_of(leader.id())
    );

    leader.stdin.as_mut().unwrap().write_all(b"go\n").unwrap();
    let pids = read_pids(&pid_file).await;
    let [escaped, member] = pids[..] else {
        panic!("unexpected tree {pids:?}");
    };
    let members = [
        ("the leader", handle_of(leader.id())),
        ("the grandchild in a session of its own", handle_of(escaped)),
        ("the member ignoring SIGTERM", handle_of(member)),
    ];
    left.0.push(handle_of(escaped));
    left.0.push(handle_of(member));
    for pid in [escaped, member] {
        assert!(
            in_scope(pid, &names.child),
            "a process the leader started is not in the child's scope: {}",
            cgroup_of(pid)
        );
    }

    for process in [&mut holder, &mut watcher] {
        process.kill().unwrap();
        process.wait().unwrap();
    }
    for (what, process) in &members {
        wait_ended(process, what).await;
    }
    let _ = leader.wait();

    let mut gone = shell("read line", &[]);
    let gone_fd = pidfd_of(gone.id());
    drop(gone.stdin.take());
    gone.wait().unwrap();
    let answer = manager
        .start_holder_scope(&refused.holder, &[gone_fd.as_fd()])
        .await;
    assert!(
        answer.is_err(),
        "a request naming a reaped process by its pidfd was accepted"
    );
    let load = Command::new("systemctl")
        .args([
            "--user",
            "show",
            "--property=LoadState",
            "--value",
            &refused.holder,
        ])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&load.stdout).trim(),
        "not-found",
        "a refused request left a unit"
    );
}
