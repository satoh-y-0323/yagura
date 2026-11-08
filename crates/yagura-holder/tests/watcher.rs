#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// Starts holders, shells and `ps` as processes each test owns and ends itself.
#![allow(clippy::disallowed_methods)]

//! Integration test (Unix): a window's watcher, the process its holder starts
//! to start the window's child on the terminal and to end the child's tree once
//! the holder is gone, once the child ends or once the holder asks. Every
//! process is watched on its own descriptor from while it is known to be
//! alive, so its end is an event on that process rather than on a reused pid.

use std::path::Path;
use std::process::{Command, Stdio};

use yagura_os::{Liveness, ProcessHandle};
use yagura_protocol::holder::{
    HOLDER_PROTOCOL, HolderFrame, HolderLaunch, HolderRecord, HolderSpawnFailure,
    encode_holder_launch,
};
use yagura_test_support::HANG_GUARD;

use crate::holder::{
    CHILD, DaemonEndpoint, Event, HOLDER, Holder, NO_DEADLINE_MS, Peer, Rig, files_left, has_ended,
    hello, hold_daemon_endpoint, is_copy, path_arg, until_exited,
};

/// The watcher's name, read only to say what a process turned out to be.
const WATCHER_NAME: &str = "yagura-watcher";

const TREE_READY: &str = "WATCHED-TREE-READY";

/// The window's tree, run by `/bin/sh` with the fixture as `$0`: a member of
/// the leader's process group, a tool started by `nohup` in a job of its own
/// and, given a second argument, an orphan in a session of its own whose
/// parent has exited. Every one of them, the leader included, ignores
/// hangups. Writes `<member> <tool>` to its first argument, prints
/// `WATCHED-TREE-READY` and idles as the leader.
const TREE: &str = r#"trap '' HUP
"$0" --grandchild </dev/null >/dev/null 2>&1 &
member=$!
set -m
nohup "$0" --grandchild </dev/null >/dev/null 2>&1 &
tool=$!
set +m
if [ -n "$2" ]; then "$0" --orphan-forker "$2" 1 stay || exit 3; fi
echo "$member $tool" > "$1.staged" && mv "$1.staged" "$1"
echo WATCHED-TREE-READY
exec "$0" --grandchild
"#;

/// A shell that starts a tool by `nohup` in a job of its own, writes its id to
/// its first argument, prints `SHELL-READY` and exits with 0 on a line of
/// input, leaving the tool running.
const SHELL: &str = r#"set -m
nohup "$0" --grandchild </dev/null >/dev/null 2>&1 &
echo $! > "$1.staged" && mv "$1.staged" "$1"
set +m
echo SHELL-READY
read line
exit 0
"#;

/// Kills, as a test ends, the processes it watched; a process already ended
/// is left alone.
pub(crate) struct Ends(pub(crate) Vec<(String, ProcessHandle)>);

impl Drop for Ends {
    fn drop(&mut self) {
        for (_, process) in &self.0 {
            let _ = process.kill();
        }
    }
}

impl Ends {
    async fn all_end(&self, why: &str) {
        for (what, process) in &self.0 {
            ended(process, &format!("{what} ({why})")).await;
        }
    }
}

/// A window running [`TREE`]. Its fields drop in order: the holder is killed
/// before the processes left, and the directory goes last.
struct Window {
    holder: Holder,
    tree: Ends,
    watcher: Ends,
    peer: Peer,
    _daemon: DaemonEndpoint,
    record: HolderRecord,
    rig: Rig,
}

impl Window {
    fn watcher(&self) -> &ProcessHandle {
        &self.watcher.0[0].1
    }

    fn leader(&self) -> &ProcessHandle {
        &self.tree.0[0].1
    }
}

fn shell_launch(rig: &Rig, script: &str, args: &[&str]) -> HolderLaunch {
    let mut argv = vec![
        "/bin/sh".to_owned(),
        "-c".to_owned(),
        script.to_owned(),
        CHILD.to_owned(),
    ];
    argv.extend(args.iter().map(|arg| (*arg).to_owned()));
    HolderLaunch {
        argv,
        ..rig.launch(&[], NO_DEADLINE_MS)
    }
}

/// Opens a window running [`TREE`], with `edit` applied to its launch.
async fn open_tree(rig: Rig, edit: impl FnOnce(&mut HolderLaunch)) -> Window {
    let daemon = hold_daemon_endpoint(&rig).await;
    let info = rig.file("tree");
    let orphan_file = rig.file("orphan");
    let orphan_arg = if cfg!(target_os = "linux") {
        path_arg(&orphan_file).to_owned()
    } else {
        String::new()
    };
    let mut launch = shell_launch(&rig, TREE, &[path_arg(&info), &orphan_arg]);
    edit(&mut launch);
    let holder = rig.start(&launch);
    let (mut peer, _) = Peer::welcomed(&rig.record().await.endpoint).await;
    peer.text_until(TREE_READY).await;
    let record = rig.record_where(|record| record.child_pid.is_some()).await;
    let [member, tool] = read_pids(&info)[..] else {
        panic!("the tree named no member and tool");
    };
    let mut tree = vec![
        ("the leader".to_owned(), process(record.child_pid.unwrap())),
        (
            "the member of the leader's group".to_owned(),
            process(member),
        ),
        ("the tool started by nohup".to_owned(), process(tool)),
    ];
    if cfg!(target_os = "linux") {
        let [orphan] = read_pids(&orphan_file)[..] else {
            panic!("the tree named no orphan");
        };
        tree.push((
            "the orphan in a session of its own".to_owned(),
            process(orphan),
        ));
    }
    let tree = Ends(tree);
    let (_, watcher) = watched(&record);
    Window {
        holder,
        tree,
        watcher: Ends(vec![("the watcher".to_owned(), watcher)]),
        peer,
        _daemon: daemon,
        record,
        rig,
    }
}

/// The process ids written to `path`, which is complete once it exists.
pub(crate) fn read_pids(path: &Path) -> Vec<u32> {
    std::fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("{} was not written: {error}", path.display()))
        .split_whitespace()
        .map(|id| id.parse().unwrap())
        .collect()
}

fn process(pid: u32) -> ProcessHandle {
    let start = yagura_os::process_start(pid)
        .unwrap()
        .unwrap_or_else(|| panic!("the process {pid} is not running"));
    ProcessHandle::open(pid, &start)
        .unwrap()
        .unwrap_or_else(|| panic!("the process {pid} is not running"))
}

/// Waits on `process`'s own descriptor until it has exited.
pub(crate) async fn ended(process: &ProcessHandle, what: &str) {
    let watch = process.watch_exit().unwrap();
    tokio::time::timeout(
        HANG_GUARD,
        tokio::task::spawn_blocking(move || watch.wait()),
    )
    .await
    .unwrap_or_else(|_| panic!("{what} never ended"))
    .unwrap()
    .unwrap();
}

fn ps_field(pid: u32, field: &str) -> String {
    let output = Command::new("ps")
        .args(["-o", &format!("{field}="), "-p", &pid.to_string()])
        .output()
        .expect("run ps");
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn ppid_of(pid: u32) -> Option<u32> {
    ps_field(pid, "ppid").parse().ok()
}

/// The processes whose parent is `parent`, from one `ps` of every process.
pub(crate) fn children_by_ps(parent: u32) -> Vec<u32> {
    let output = Command::new("ps")
        .args(["-A", "-o", "pid=,ppid="])
        .output()
        .expect("run ps");
    let mut children: Vec<u32> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let pid = fields.next()?.parse().ok()?;
            let ppid: u32 = fields.next()?.parse().ok()?;
            (ppid == parent).then_some(pid)
        })
        .collect();
    children.sort_unstable();
    children
}

/// The watcher `record` names, after checking where it stands: the holder's
/// only child, the parent of the window's child, and recorded with its start.
pub(crate) fn watched(record: &HolderRecord) -> (u32, ProcessHandle) {
    let watcher = record
        .watcher_pid
        .expect("the holder's record names no watcher");
    let recorded = record
        .watcher_start
        .as_deref()
        .expect("the holder's record names no start of its watcher");
    let start = yagura_os::process_start(watcher)
        .unwrap()
        .expect("the recorded watcher is not running");
    assert_eq!(
        recorded,
        start.to_string(),
        "the record does not carry the watcher's start as yagura-os reads it"
    );
    let leader = record
        .child_pid
        .expect("the holder's record names no child");
    assert_eq!(
        children_by_ps(record.holder_pid),
        vec![watcher],
        "the holder {} runs a process beside its watcher {watcher}",
        record.holder_pid
    );
    assert_eq!(
        ppid_of(leader),
        Some(watcher),
        "the window's child {leader} does not run under the watcher {watcher}"
    );
    assert_eq!(
        ppid_of(watcher),
        Some(record.holder_pid),
        "the watcher {watcher} does not run under the holder {}",
        record.holder_pid
    );
    let name = ps_field(watcher, "comm");
    assert!(
        name.ends_with(WATCHER_NAME),
        "the watcher {watcher} runs {name:?}"
    );
    let handle = ProcessHandle::open(watcher, &start)
        .unwrap()
        .expect("the recorded watcher is not running");
    (watcher, handle)
}

fn signal(pid: u32, signal: libc::c_int) {
    let pid = libc::pid_t::try_from(pid).unwrap();
    // SAFETY: plain values; the process was opened by its start just before.
    assert_eq!(
        unsafe { libc::kill(pid, signal) },
        0,
        "kill({pid}, {signal})"
    );
}

/// Kills the holder: the watcher ends the tree and removes the holder's files
/// (its record, socket and lock), then ends itself.
async fn the_holder_is_killed(mut window: Window) {
    window.holder.child.kill().unwrap();
    window.holder.child.wait().unwrap();
    window.tree.all_end("its holder was killed").await;
    ended(window.watcher(), "the watcher of the killed holder").await;
    let left = files_left(&window.rig);
    assert!(
        left.is_empty(),
        "the holder's files are left once its watcher has ended: {left:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_killed_holders_watcher_ends_the_tree_and_removes_the_holders_files() {
    let window = open_tree(Rig::new("hkill"), |_| {}).await;
    the_holder_is_killed(window).await;
}

/// The holder ends the window when its watcher is killed: the leader still
/// running, the members of its group and of its session, and (Linux) an
/// orphan that was the watcher's child.
#[tokio::test(flavor = "multi_thread")]
async fn a_killed_watchers_holder_ends_the_window() {
    let mut window = open_tree(Rig::new("wkill"), |launch| {
        launch.awaits_successor = false;
    })
    .await;
    window.watcher().kill().unwrap();
    ended(window.watcher(), "the killed watcher").await;

    until_exited(&mut window.peer).await;
    window.tree.all_end("its watcher was killed").await;
    drop(window.peer);
    window.holder.wait_exit().await;
}

/// A shell that exits ends its tree, a tool it left running by `nohup`
/// included, and only then is the window's end told.
#[tokio::test(flavor = "multi_thread")]
async fn a_shell_exiting_ends_its_tree_before_the_end_is_told() {
    let rig = Rig::new("shexit");
    let _daemon = hold_daemon_endpoint(&rig).await;
    let tool_file = rig.file("tool");
    let mut holder = rig.start(&shell_launch(&rig, SHELL, &[path_arg(&tool_file)]));
    let (mut peer, _) = Peer::welcomed(&rig.record().await.endpoint).await;
    peer.text_until("SHELL-READY").await;
    let record = rig.record_where(|record| record.child_pid.is_some()).await;
    let [tool] = read_pids(&tool_file)[..] else {
        panic!("the shell named no tool");
    };
    let _tool = Ends(vec![("the tool".to_owned(), process(tool))]);
    let (_, watcher) = watched(&record);
    let _watcher = Ends(vec![("the watcher".to_owned(), watcher)]);

    peer.send(HolderFrame::Write {
        id: 1,
        bytes: b"x\r".to_vec(),
    })
    .await;
    let code = loop {
        match peer.frame().await {
            HolderFrame::Exited { code } => break code,
            HolderFrame::Written { id: 1 } | HolderFrame::WriteFailed { id: 1, .. } => {}
            frame if is_copy(&frame) => {}
            other => panic!("unexpected frame on the way to Exited: {other:?}"),
        }
    };
    assert!(
        has_ended(tool),
        "the tool the shell left running was running when Exited arrived"
    );
    assert_eq!(code, Some(0), "the shell's exit was not told");
    ended(&_watcher.0[0].1, "the watcher of the ended tree").await;

    peer.send(HolderFrame::Terminate { tree: true }).await;
    holder.wait_exit().await;
}

/// Without the watcher's binary beside the holder's, the window does not open:
/// the holder answers the first `Hello` with the reason a child could not
/// start, then `Exited`, and never with `Welcome`.
#[tokio::test(flavor = "multi_thread")]
async fn without_the_watchers_binary_the_window_does_not_open() {
    let rig = Rig::new("nowatch");
    let _daemon = hold_daemon_endpoint(&rig).await;
    let lonely = rig.file("lonely");
    std::fs::create_dir(&lonely).unwrap();
    let holder_copy = lonely.join("yagura-holder");
    yagura_test_support::process::install_exe(Path::new(HOLDER), &holder_copy);
    let mut holder = rig.start_exe_with(
        path_arg(&holder_copy),
        &encode_holder_launch(&rig.launch(&["hex"], NO_DEADLINE_MS)),
        Stdio::null(),
        |_| {},
    );
    let mut peer = Peer::connect(&rig.record().await.endpoint).await;
    peer.send(hello(HOLDER_PROTOCOL)).await;

    let mut frames = Vec::new();
    loop {
        match peer.next().await {
            Event::Frame(frame @ HolderFrame::Exited { .. }) => {
                frames.push(frame);
                break;
            }
            Event::Frame(frame @ HolderFrame::Welcome { .. }) => {
                panic!("the window opened without its watcher's binary: {frame:?} after {frames:?}")
            }
            Event::Frame(frame) => frames.push(frame),
            Event::Closed => panic!("closed without Exited after {frames:?}"),
            Event::Broken(error) => panic!("the holder sent a broken frame: {error}"),
        }
    }
    assert!(
        matches!(
            frames.as_slice(),
            [
                HolderFrame::SpawnFailed {
                    reason: HolderSpawnFailure::Spawn,
                },
                HolderFrame::Exited { .. },
            ]
        ),
        "expected the reason, then Exited, and no Welcome: {frames:?}"
    );
    drop(peer);
    holder.wait_exit().await;
}

/// The `(device, inode)` of the file `path` names.
#[cfg(target_os = "linux")]
fn file_id(path: &Path) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt as _;
    let meta = std::fs::metadata(path).unwrap();
    (meta.dev(), meta.ino())
}

/// No descriptor of the process `pid` names the file `lock`.
#[cfg(target_os = "linux")]
fn assert_holds_no_descriptor_on(pid: u32, lock: (u64, u64), what: &str) {
    use std::os::unix::fs::MetadataExt as _;
    for entry in std::fs::read_dir(format!("/proc/{pid}/fd"))
        .unwrap()
        .flatten()
    {
        if let Ok(meta) = std::fs::metadata(entry.path()) {
            assert_ne!(
                (meta.dev(), meta.ino()),
                lock,
                "{what} {pid} holds the endpoint's lock file as {}",
                entry.path().display()
            );
        }
    }
}

/// Whether this process can take the lock on `path` without waiting.
fn lock_is_free(path: &Path) -> bool {
    use std::os::fd::AsRawFd as _;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap();
    // SAFETY: `file` owns the descriptor for the whole call; closing it below
    // lets the lock go.
    unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) == 0 }
}

/// The endpoint's lock never reaches the window's child: once the holder and
/// the watcher have both ended without cleaning up, while a leader ignoring
/// hangups still runs, the lock is free.
#[tokio::test(flavor = "multi_thread")]
async fn the_endpoint_lock_never_reaches_the_windows_child() {
    let window = open_tree(Rig::new("locklk"), |_| {}).await;
    let lock_path = format!("{}.lock", window.record.endpoint);
    #[cfg(target_os = "linux")]
    let assert_child_holds_no_lock = {
        let lock = file_id(Path::new(&lock_path));
        let leader = window.record.child_pid.unwrap();
        move || assert_holds_no_descriptor_on(leader, lock, "the window's child")
    };
    #[cfg(target_os = "linux")]
    assert_child_holds_no_lock();

    let ended_holder = yagura_test_support::process::end_holder_and_watcher(&window.record);
    assert_eq!(
        ended_holder,
        Some(window.record.holder_pid),
        "the holder was not running"
    );
    assert_eq!(window.watcher().liveness(), Liveness::Dead);
    assert_eq!(
        window.leader().liveness(),
        Liveness::Alive,
        "the window's child ignoring hangups must outlive its holder and watcher"
    );
    #[cfg(target_os = "linux")]
    assert_child_holds_no_lock();
    assert!(
        lock_is_free(Path::new(&lock_path)),
        "the endpoint's lock is held once the holder and the watcher have ended"
    );
}

/// A watcher stopped from outside while its holder runs does not keep a
/// window from closing: the holder resumes it, and the close completes.
#[tokio::test(flavor = "multi_thread")]
async fn a_watcher_stopped_from_outside_is_resumed_and_the_window_closes() {
    let mut window = open_tree(Rig::new("wstop"), |_| {}).await;
    signal(window.record.watcher_pid.unwrap(), libc::SIGSTOP);

    window
        .peer
        .send(HolderFrame::Terminate { tree: true })
        .await;
    until_exited(&mut window.peer).await;
    window.tree.all_end("the window was closed").await;
    let status = window.holder.wait_exit().await;
    assert!(
        status.success(),
        "the holder did not end as a window closed: {status}"
    );
    ended(window.watcher(), "the watcher of the closed window").await;
}

#[cfg(target_os = "linux")]
mod scoped {
    //! With the user's user manager: the holder and its watcher are in the
    //! holder's scope, and the window's child and the tree it grew before the
    //! scopes were made are in the child's.

    use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd, RawFd};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Condvar, Mutex};

    use yagura_holder::watcher_proto::MOST_FDS_PER_SCOPE_REQUEST;
    use yagura_os::ProcessHandle;
    use yagura_protocol::holder::{HolderFrame, HolderLaunch, HolderScopes};
    use yagura_test_support::HANG_GUARD;

    use super::{
        Ends, children_by_ps, ended, open_tree, process, read_pids, the_holder_is_killed, watched,
    };
    use crate::holder::{
        NO_DEADLINE_MS, Peer, Rig, StopScopes, cgroup_of, hold_daemon_endpoint, path_arg,
        scopes_of, until_exited,
    };

    fn in_scope(pid: u32, scope: &str) -> bool {
        cgroup_of(pid).ends_with(&format!("/{scope}"))
    }

    /// Waits until `misplaced` finds nothing out of place.
    async fn settle(what: &str, misplaced: impl Fn() -> Vec<String>) {
        let deadline = tokio::time::Instant::now() + HANG_GUARD;
        loop {
            let problems = misplaced();
            if problems.is_empty() {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "{what}:\n- {}",
                problems.join("\n- ")
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }

    /// Whether the cgroup `dir` still holds a live process.
    fn populated(dir: &Path) -> bool {
        std::fs::read_to_string(dir.join("cgroup.events"))
            .unwrap_or_default()
            .lines()
            .any(|line| line == "populated 1")
    }

    /// With the holder killed, the watcher stays in the holder's scope, so the
    /// scopes stay up while it ends the tree and removes the holder's files.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "needs the user's systemd user manager; run with --include-ignored where one runs"]
    async fn with_a_user_manager_a_killed_holders_watcher_still_ends_the_tree() {
        let rig = Rig::new("hkillsc");
        let scopes = scopes_of(&rig);
        let _stop = StopScopes(scopes.clone());
        let window = open_tree(rig, |launch| launch.scopes = Some(scopes.clone())).await;
        let watcher = window.record.watcher_pid.unwrap();
        let leader = window.record.child_pid.unwrap();
        settle("the watcher and the child are not in their scopes", || {
            let mut problems = Vec::new();
            if !in_scope(watcher, &scopes.holder) {
                problems.push(format!("the watcher is in {}", cgroup_of(watcher)));
            }
            if !in_scope(leader, &scopes.child) {
                problems.push(format!("the child is in {}", cgroup_of(leader)));
            }
            problems
        })
        .await;
        the_holder_is_killed(window).await;
    }

    /// The real user bus, as `systemd-run --user` finds it.
    fn real_bus() -> PathBuf {
        if let Some(address) = std::env::var_os("DBUS_SESSION_BUS_ADDRESS") {
            let address = address.to_string_lossy().into_owned();
            return address
                .split(';')
                .filter_map(|part| part.strip_prefix("unix:"))
                .flat_map(|part| part.split(','))
                .find_map(|pair| pair.strip_prefix("path="))
                .map(PathBuf::from)
                .unwrap_or_else(|| panic!("the user bus {address} has no unix path"));
        }
        let runtime = std::env::var_os("XDG_RUNTIME_DIR").map_or_else(
            // SAFETY: getuid takes no arguments and cannot fail.
            || PathBuf::from(format!("/run/user/{}", unsafe { libc::getuid() })),
            PathBuf::from,
        );
        runtime.join("bus")
    }

    /// A user bus address whose connections reach the real bus only once
    /// [`BusGate::open`] is called, so a test decides what exists before the
    /// holder's first request. Bytes and descriptors pass through unchanged.
    struct BusGate {
        path: PathBuf,
        open: Arc<(Mutex<bool>, Condvar)>,
        closing: Arc<AtomicBool>,
    }

    impl BusGate {
        fn new(path: &Path) -> Self {
            let real = real_bus();
            let listener = UnixListener::bind(path).unwrap();
            let open = Arc::new((Mutex::new(false), Condvar::new()));
            let closing = Arc::new(AtomicBool::new(false));
            let (gate, stop) = (Arc::clone(&open), Arc::clone(&closing));
            std::thread::spawn(move || {
                for client in listener.incoming() {
                    if stop.load(Ordering::SeqCst) {
                        return;
                    }
                    let Ok(client) = client else { continue };
                    let (gate, real) = (Arc::clone(&gate), real.clone());
                    std::thread::spawn(move || {
                        let (lock, opened) = &*gate;
                        let mut is_open = lock.lock().unwrap();
                        while !*is_open {
                            is_open = opened.wait(is_open).unwrap();
                        }
                        drop(is_open);
                        let Ok(bus) = UnixStream::connect(&real) else {
                            return;
                        };
                        let (client_in, bus_in) =
                            (client.try_clone().unwrap(), bus.try_clone().unwrap());
                        std::thread::spawn(move || pass(&client_in, &bus));
                        pass(&bus_in, &client);
                    });
                }
            });
            Self {
                path: path.to_path_buf(),
                open,
                closing,
            }
        }

        fn address(&self) -> String {
            format!("unix:path={}", self.path.display())
        }

        fn open(&self) {
            let (lock, opened) = &*self.open;
            *lock.lock().unwrap() = true;
            opened.notify_all();
        }
    }

    impl Drop for BusGate {
        fn drop(&mut self) {
            self.closing.store(true, Ordering::SeqCst);
            self.open();
            let _ = UnixStream::connect(&self.path);
        }
    }

    /// The most descriptors one received message carries here.
    const MOST_FDS: usize = 512;

    fn control_space(fds: usize) -> usize {
        let bytes = u32::try_from(fds * std::mem::size_of::<RawFd>()).unwrap();
        // SAFETY: CMSG_SPACE only computes a size.
        usize::try_from(unsafe { libc::CMSG_SPACE(bytes) }).unwrap()
    }

    /// Copies what `from` receives, descriptors included, to `to` until `from`
    /// ends.
    fn pass(from: &UnixStream, to: &UnixStream) {
        let mut bytes = vec![0u8; 64 * 1024];
        let mut control = vec![0u64; control_space(MOST_FDS).div_ceil(8)];
        loop {
            let mut iov = libc::iovec {
                iov_base: bytes.as_mut_ptr().cast(),
                iov_len: bytes.len(),
            };
            // SAFETY: an all-zero msghdr is an empty header, filled in below.
            let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
            msg.msg_iov = &mut iov;
            msg.msg_iovlen = 1;
            msg.msg_control = control.as_mut_ptr().cast();
            msg.msg_controllen = control.len() * 8;
            // SAFETY: `msg` points at buffers that outlive the call.
            let got = unsafe { libc::recvmsg(from.as_raw_fd(), &mut msg, libc::MSG_CMSG_CLOEXEC) };
            if got < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
            {
                continue;
            }
            let Ok(got) = usize::try_from(got) else { break };
            if got == 0 {
                break;
            }
            let fds = received(&msg);
            if !send(to, &bytes[..got], &fds) {
                break;
            }
        }
        let _ = to.shutdown(std::net::Shutdown::Write);
    }

    fn received(msg: &libc::msghdr) -> Vec<OwnedFd> {
        let mut fds = Vec::new();
        // SAFETY: `msg` was filled in by recvmsg; the walk stays within its
        // control buffer, and each descriptor read is owned from here on.
        unsafe {
            let mut header = libc::CMSG_FIRSTHDR(msg);
            while !header.is_null() {
                if (*header).cmsg_level == libc::SOL_SOCKET
                    && (*header).cmsg_type == libc::SCM_RIGHTS
                {
                    let data = libc::CMSG_DATA(header).cast::<RawFd>();
                    let len = (*header).cmsg_len - libc::CMSG_LEN(0) as usize;
                    for index in 0..len / std::mem::size_of::<RawFd>() {
                        fds.push(OwnedFd::from_raw_fd(data.add(index).read_unaligned()));
                    }
                }
                header = libc::CMSG_NXTHDR(msg, header);
            }
        }
        fds
    }

    /// Sends `bytes` to `to`, with `fds` beside the first part sent.
    fn send(to: &UnixStream, mut bytes: &[u8], fds: &[OwnedFd]) -> bool {
        let raw: Vec<RawFd> = fds.iter().map(|fd| fd.as_raw_fd()).collect();
        let mut control = vec![0u64; control_space(raw.len()).div_ceil(8)];
        let mut first = true;
        while !bytes.is_empty() {
            let mut iov = libc::iovec {
                iov_base: bytes.as_ptr().cast_mut().cast(),
                iov_len: bytes.len(),
            };
            // SAFETY: an all-zero msghdr is an empty header, filled in below.
            let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
            msg.msg_iov = &mut iov;
            msg.msg_iovlen = 1;
            if first && !raw.is_empty() {
                msg.msg_control = control.as_mut_ptr().cast();
                msg.msg_controllen = control_space(raw.len());
                // SAFETY: the control buffer is sized for one header with
                // `raw` after it.
                unsafe {
                    let header = libc::CMSG_FIRSTHDR(&msg);
                    (*header).cmsg_level = libc::SOL_SOCKET;
                    (*header).cmsg_type = libc::SCM_RIGHTS;
                    (*header).cmsg_len = libc::CMSG_LEN(
                        u32::try_from(raw.len() * std::mem::size_of::<RawFd>()).unwrap(),
                    ) as usize;
                    std::ptr::copy_nonoverlapping(
                        raw.as_ptr(),
                        libc::CMSG_DATA(header).cast::<RawFd>(),
                        raw.len(),
                    );
                }
            }
            // SAFETY: `msg` points at buffers that outlive the call.
            let sent = unsafe { libc::sendmsg(to.as_raw_fd(), &msg, libc::MSG_NOSIGNAL) };
            if sent < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
            {
                continue;
            }
            let Ok(sent) = usize::try_from(sent) else {
                return false;
            };
            first = false;
            bytes = &bytes[sent..];
        }
        true
    }

    /// Orphans of the watcher that one request carries with the child.
    const FEW_ORPHANS: usize = 10;

    /// The tree the window's child grew before its holder could ask for the
    /// scopes -- orphans of the watcher -- ends up in the child's scope, the watcher stays in the holder's, and a close
    /// ends with the child's scope empty: the watcher's sweep is told over only
    /// once the scope holds no process, while the watcher, outside it, lives
    /// to tell it and the holder ends as a window closes.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "needs the user's systemd user manager; run with --include-ignored where one runs"]
    async fn with_a_user_manager_the_watchers_orphans_join_the_childs_scope_and_it_stays_out() {
        let rig = Rig::new("wscope");
        let _daemon = hold_daemon_endpoint(&rig).await;
        let scopes: HolderScopes = scopes_of(&rig);
        let _stop = StopScopes(scopes.clone());
        let bus = BusGate::new(&rig.file("bus"));
        let pid_file = rig.file("orphans");
        let count = FEW_ORPHANS.to_string();
        let launch = HolderLaunch {
            scopes: Some(scopes.clone()),
            ..rig.launch(
                &["orphans", path_arg(&pid_file), &count, "stay"],
                NO_DEADLINE_MS,
            )
        };
        let mut holder = rig.start_in(&launch, |command| {
            command.env("DBUS_SESSION_BUS_ADDRESS", bus.address());
        });
        let (mut peer, _) = Peer::welcomed(&rig.record().await.endpoint).await;
        peer.text_until("HOLDER-CHILD-ORPHANS").await;
        let orphans = read_pids(&pid_file);
        let _orphans = Ends(
            orphans
                .iter()
                .map(|&pid| ("an orphan".to_owned(), process(pid)))
                .collect(),
        );
        let record = rig.record_where(|record| record.child_pid.is_some()).await;
        let leader = record.child_pid.unwrap();
        let (watcher, watcher_handle): (u32, ProcessHandle) = watched(&record);
        let _watcher = Ends(vec![("the watcher".to_owned(), watcher_handle)]);
        let mut expected: Vec<u32> = orphans.iter().copied().chain([leader]).collect();
        expected.sort_unstable();
        assert_eq!(
            children_by_ps(watcher),
            expected,
            "the tree grown before the scopes is not the leader and the watcher's orphans"
        );

        bus.open();
        settle("the window's processes are not in their scopes", || {
            let mut problems = Vec::new();
            for (what, pid, scope) in [
                ("the holder", record.holder_pid, &scopes.holder),
                ("the watcher", watcher, &scopes.holder),
                ("the window's child", leader, &scopes.child),
            ] {
                if !in_scope(pid, scope) {
                    problems.push(format!("{what} is in {}", cgroup_of(pid)));
                }
            }
            let outside = orphans
                .iter()
                .filter(|&&pid| !in_scope(pid, &scopes.child))
                .count();
            if outside > 0 {
                problems.push(format!(
                    "{outside} of {} orphans are outside the child's scope",
                    orphans.len()
                ));
            }
            problems
        })
        .await;
        let child_cgroup =
            Path::new("/sys/fs/cgroup").join(cgroup_of(leader).trim_start_matches('/'));

        peer.send(HolderFrame::Terminate { tree: true }).await;
        until_exited(&mut peer).await;
        assert!(
            !populated(&child_cgroup),
            "the end was told while the child's scope still held a process"
        );
        let status = holder.wait_exit().await;
        assert!(
            status.success(),
            "the holder did not end as a window closed: {status}"
        );
        ended(&_watcher.0[0].1, "the watcher of the closed window").await;
    }

    /// The number the holder logs for the members left outside the child's
    /// scope, once it logs the scope as made.
    fn logged_outside(log: &Path) -> Option<usize> {
        std::fs::read_to_string(log)
            .unwrap_or_default()
            .lines()
            .find(|line| line.contains("scope is made"))?
            .split(';')
            .nth(1)?
            .split_whitespace()
            .next()?
            .parse()
            .ok()
    }

    /// More of the tree than one request to the user manager carries: the
    /// leader and orphans of the watcher born before the scopes. As many as
    /// one request carries join the child's scope; the rest stay outside and
    /// are counted as outside; and once the holder is killed, the watcher's
    /// sweep ends every one of them, those outside included.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "needs the user's systemd user manager; run with --include-ignored where one runs"]
    async fn with_a_user_manager_a_tree_over_one_requests_limit_joins_it_in_part_and_still_ends() {
        let rig = Rig::new("wscopeb");
        let _daemon = hold_daemon_endpoint(&rig).await;
        let scopes: HolderScopes = scopes_of(&rig);
        let _stop = StopScopes(scopes.clone());
        let bus = BusGate::new(&rig.file("bus"));
        let log = rig.file("holder.log");
        std::fs::write(&log, b"").unwrap();
        let pid_file = rig.file("orphans");
        let count = (MOST_FDS_PER_SCOPE_REQUEST + 1).to_string();
        let launch = HolderLaunch {
            scopes: Some(scopes.clone()),
            log_file: Some(log.clone()),
            ..rig.launch(
                &["orphans", path_arg(&pid_file), &count, "stay"],
                NO_DEADLINE_MS,
            )
        };
        let mut holder = rig.start_in(&launch, |command| {
            command.env("DBUS_SESSION_BUS_ADDRESS", bus.address());
        });
        let (mut peer, _) = Peer::welcomed(&rig.record().await.endpoint).await;
        peer.text_until("HOLDER-CHILD-ORPHANS").await;
        let record = rig.record_where(|record| record.child_pid.is_some()).await;
        let leader = record.child_pid.unwrap();
        let mut tree = read_pids(&pid_file);
        tree.push(leader);
        let members = Ends(
            tree.iter()
                .map(|&pid| (format!("the tree's process {pid}"), process(pid)))
                .collect(),
        );
        let (watcher, watcher_handle): (u32, ProcessHandle) = watched(&record);
        let _watcher = Ends(vec![("the watcher".to_owned(), watcher_handle)]);
        assert!(
            tree.len() > MOST_FDS_PER_SCOPE_REQUEST,
            "the tree fits one request"
        );

        bus.open();
        settle("the holder never logged the child's scope as made", || {
            let mut problems = Vec::new();
            if logged_outside(&log).is_none() {
                problems.push(std::fs::read_to_string(&log).unwrap_or_default());
            }
            if !in_scope(watcher, &scopes.holder) {
                problems.push(format!("the watcher is in {}", cgroup_of(watcher)));
            }
            problems
        })
        .await;
        let inside = tree
            .iter()
            .filter(|&&pid| in_scope(pid, &scopes.child))
            .count();
        assert_eq!(
            inside,
            MOST_FDS_PER_SCOPE_REQUEST,
            "not as many of the tree's {} processes as one request carries joined the child's scope",
            tree.len()
        );
        assert_eq!(
            logged_outside(&log),
            Some(tree.len() - MOST_FDS_PER_SCOPE_REQUEST),
            "the members outside the child's scope were not counted as outside"
        );

        holder.child.kill().unwrap();
        holder.child.wait().unwrap();
        members.all_end("the holder was killed").await;
        ended(&_watcher.0[0].1, "the watcher of the killed holder").await;
        drop(peer);
    }
}
