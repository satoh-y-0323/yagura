#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// `ps` only reads the process table on Unix, and the library host is this test binary
// run again and killed by the test; neither starts anything a window could adopt.
#![allow(clippy::disallowed_methods)]

//! The daemon keeps every window through a `yagura-holder` process: the window's child
//! runs under the holder, every end of the window (closing it, the daemon's shutdown, a
//! library registry going away unclosed) ends the child's tree and the holder, and none
//! of them leaves the holder's files behind.
//!
//! Each process is watched from while it is known to be alive, so its end is observed
//! on the process itself rather than on a reused pid.

use crate::common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use common::real_daemon::{RunningDaemon, Sandbox};
use yagura_daemon::detector::reading;
use yagura_daemon::{DaemonConfig, OpenWindowRequest, WindowRegistry};
use yagura_protocol::holder::read_holder_record;
use yagura_protocol::{ClientMessage, DaemonMessage};
use yagura_test_support::ipc::{ReadOutcome, read_daemon_frame};
use yagura_test_support::process::{capture_child_path, holder_path};

async fn open_window(command: Vec<String>) -> (Arc<WindowRegistry>, String) {
    let registry = Arc::new(WindowRegistry::new(DaemonConfig::default()));
    let window_id = registry
        .open(OpenWindowRequest {
            command,
            cwd: None,
            name: None,
            vendor: None,
            detection: None,
            on_name_taken: Default::default(),
        })
        .await
        .expect("registry.open should start the window")
        .window_id;
    (registry, window_id)
}

/// The parent of `child_pid`, which must be a `yagura-holder`.
#[cfg(windows)]
fn holder_of(child_pid: u32) -> u32 {
    common::holder_of(child_pid).pid
}

#[cfg(unix)]
fn ps(pid: u32, field: &str) -> String {
    let output = std::process::Command::new("ps")
        .args(["-o", &format!("{field}="), "-p", &pid.to_string()])
        .output()
        .expect("run ps");
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// The holder `child_pid` runs under: the parent of its parent, the watcher. Neither is
/// this process, the daemon; the names only say what the two turned out to be.
#[cfg(unix)]
fn holder_of(child_pid: u32) -> u32 {
    let parent_of = |pid: u32| -> u32 {
        ps(pid, "ppid")
            .parse()
            .unwrap_or_else(|_| panic!("the process {pid} is not running"))
    };
    let watcher = parent_of(child_pid);
    let holder = parent_of(watcher);
    let daemon = std::process::id();
    assert!(
        watcher != daemon && holder != daemon,
        "the window's child {child_pid} runs under the daemon {daemon}: its parent is \
         {watcher}, whose parent is {holder}"
    );
    let (watcher_image, holder_image) = (ps(watcher, "comm"), ps(holder, "comm"));
    assert!(
        watcher_image.ends_with("yagura-watcher") && holder_image.ends_with("yagura-holder"),
        "the window's child {child_pid} must run under a yagura-watcher under a yagura-holder; \
         its parent {watcher} is {watcher_image:?}, whose parent {holder} is {holder_image:?}"
    );
    holder
}

/// A process watched from while it is known to be alive.
struct Watch {
    what: String,
    #[cfg(windows)]
    handle: windows::Win32::Foundation::HANDLE,
    #[cfg(unix)]
    pid: u32,
}

#[cfg(windows)]
impl Watch {
    fn new(what: &str, pid: u32) -> Self {
        use windows::Win32::System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE};
        // SAFETY: OpenProcess takes plain values; the handle is closed in `wait_gone`.
        let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid) }
            .unwrap_or_else(|error| panic!("the {what} ({pid}) must be alive: {error}"));
        Self {
            what: what.to_string(),
            handle,
        }
    }

    async fn wait_gone(self) {
        use windows::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
        use windows::Win32::System::Threading::WaitForSingleObject;
        let handle = self.handle;
        let millis = u32::try_from(yagura_test_support::HANG_GUARD.as_millis()).unwrap();
        // SAFETY: `handle` is a process handle this watch owns until here.
        let waited = tokio::task::spawn_blocking(move || unsafe {
            let waited = WaitForSingleObject(handle, millis);
            let _ = CloseHandle(handle);
            waited
        })
        .await
        .unwrap();
        assert_eq!(
            waited, WAIT_OBJECT_0,
            "the {} outlived the window",
            self.what
        );
    }
}

#[cfg(unix)]
impl Watch {
    fn new(what: &str, pid: u32) -> Self {
        assert!(alive(pid), "the {what} ({pid}) must be alive");
        Self {
            what: what.to_string(),
            pid,
        }
    }

    async fn wait_gone(self) {
        let deadline = tokio::time::Instant::now() + yagura_test_support::HANG_GUARD;
        while alive(self.pid) {
            assert!(
                tokio::time::Instant::now() < deadline,
                "the {} outlived the window",
                self.what
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

/// A zombie has ended; only its parent has not read its exit yet.
#[cfg(unix)]
fn alive(pid: u32) -> bool {
    let state = ps(pid, "stat");
    !state.is_empty() && !state.starts_with('Z')
}

/// A profile whose window's child starts a grandchild first thing, publishes the
/// grandchild's pid in `pid_file` and then stays alive.
fn write_tree_profile(sandbox: &Sandbox, profile: &str, pid_file: &Path) {
    let child = capture_child_path();
    sandbox.write_probe_profile(
        profile,
        &[
            child.to_str().expect("capture_child path must be UTF-8"),
            "grandchild-first",
            pid_file.to_str().expect("pid file path must be UTF-8"),
            "600000",
        ],
        "",
    );
}

async fn published_pid(pid_file: &Path) -> u32 {
    let deadline = tokio::time::Instant::now() + yagura_test_support::HANG_GUARD;
    loop {
        if let Some(pid) = std::fs::read_to_string(pid_file)
            .ok()
            .and_then(|text| text.trim().parse::<u32>().ok())
        {
            return pid;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the grandchild never published its pid at {}",
            pid_file.display()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Opens the `profile` window and watches every process it consists of: the holder
/// its record names, the child, the grandchild, on Unix the watcher its record names
/// and, on Windows, the pseudoconsole hosts under the holder.
async fn open_tree_window(
    daemon: &mut RunningDaemon,
    sandbox: &Sandbox,
    profile: &str,
    pid_file: &Path,
) -> (String, Vec<Watch>) {
    let window_id = daemon.open_profile_window(sandbox, profile).await;
    let record = read_holder_record(&sandbox.holders_dir(), &window_id).unwrap_or_else(|error| {
        panic!(
            "the window's holder must keep its record in the instance's holders folder: \
             {error}; daemon log:\n{}",
            sandbox.daemon_log()
        )
    });
    let child = record
        .child_pid
        .expect("an opened window's record names its child");
    let grandchild = published_pid(pid_file).await;

    let mut watches = vec![
        Watch::new("holder", record.holder_pid),
        Watch::new("child", child),
        Watch::new("grandchild", grandchild),
    ];
    #[cfg(unix)]
    watches.push(Watch::new(
        "watcher",
        record
            .watcher_pid
            .expect("an opened window's record names its watcher"),
    ));
    watches.extend(pseudoconsole_hosts(record.holder_pid));
    (window_id, watches)
}

#[cfg(windows)]
fn pseudoconsole_hosts(holder_pid: u32) -> Vec<Watch> {
    common::children_of(&common::process_table(), holder_pid)
        .into_iter()
        .filter(|row| {
            common::PTY_HOST_IMAGES
                .iter()
                .any(|image| row.image.eq_ignore_ascii_case(image))
        })
        .map(|row| Watch::new("pseudoconsole host", row.pid))
        .collect()
}

#[cfg(unix)]
fn pseudoconsole_hosts(_holder_pid: u32) -> Vec<Watch> {
    Vec::new()
}

async fn wait_all_gone(watches: Vec<Watch>) {
    for watch in watches {
        watch.wait_gone().await;
    }
}

/// No file named for the window is left in the holders directory: its
/// holder's record or any other.
fn assert_no_holder_files(sandbox: &Sandbox, window_id: &str, route: &str) {
    let dir = sandbox.holders_dir();
    let prefix = format!("{window_id}.");
    let left: Vec<_> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .map(|entry| entry.unwrap().file_name())
        .filter(|name| name.to_string_lossy().starts_with(&prefix))
        .collect();
    assert!(left.is_empty(), "{route} left the holder's {left:?} behind");
}

#[tokio::test]
async fn a_windows_child_runs_under_a_holder_and_not_under_the_daemon() {
    let (registry, window_id) = open_window(common::sleepy_command("30")).await;
    let entry = registry.get(&window_id).expect("window must exist");
    let child = entry.session().child_process_id();

    // Fails unless the child runs under a watcher under a holder (Unix) or under a
    // holder (Windows), neither of which is the daemon.
    holder_of(child);
    let _ = yagura_daemon::control::close_window(&entry, true, Duration::from_secs(5)).await;
}

#[tokio::test]
async fn closing_a_window_ends_its_tree_and_holder_and_leaves_no_holder_files() {
    let sandbox = Sandbox::new("holder-close");
    let pids = sandbox.tree.make_dir("pids");
    let routes = [("tree-gentle", false), ("tree-force", true)];
    for (profile, _) in routes {
        write_tree_profile(&sandbox, profile, &pids.join(format!("{profile}.pid")));
    }
    let mut daemon = sandbox.start().await;

    for (profile, force) in routes {
        let pid_file = pids.join(format!("{profile}.pid"));
        let (window_id, watches) =
            open_tree_window(&mut daemon, &sandbox, profile, &pid_file).await;

        common::send(
            &mut daemon.client,
            &ClientMessage::CloseWindow {
                window_id: window_id.clone(),
                force: Some(force),
            },
        )
        .await;
        match common::read_response(&mut daemon.client).await {
            DaemonMessage::WindowClosed { .. } => {}
            other => panic!("expected window_closed, got {other:?}"),
        }

        wait_all_gone(watches).await;
        assert_no_holder_files(
            &sandbox,
            &window_id,
            &format!("close_window (force: {force})"),
        );
    }
}

#[tokio::test]
async fn the_daemons_shutdown_ends_every_tree_and_holder_and_leaves_no_holder_files() {
    let sandbox = Sandbox::new("holder-shutdown");
    let pid_file = sandbox.tree.make_dir("pids").join("tree.pid");
    write_tree_profile(&sandbox, "tree", &pid_file);
    let mut daemon = sandbox.start().await;
    let (window_id, watches) = open_tree_window(&mut daemon, &sandbox, "tree", &pid_file).await;

    common::send(&mut daemon.client, &ClientMessage::Shutdown).await;
    // The daemon closes the connection once every window has been terminated.
    while !matches!(
        read_daemon_frame(&mut daemon.client).await,
        ReadOutcome::Closed
    ) {}

    wait_all_gone(watches).await;
    assert_no_holder_files(&sandbox, &window_id, "the daemon's shutdown");
}

const LIBRARY_HOST: &str = "library_host";
const LIBRARY_HOST_OUT_ENV: &str = "HOLDER_WINDOW_LIBRARY_HOST_OUT";
const LIBRARY_HOST_PID_ENV: &str = "HOLDER_WINDOW_LIBRARY_HOST_PID";

/// A window registry used as a library has no next daemon to come for its
/// windows: the process holding it going away unclosed ends every window's tree
/// and holder, and leaves none of the holder's files.
#[tokio::test]
async fn a_library_registry_that_goes_away_unclosed_ends_every_tree_and_holder() {
    let tmp = yagura_test_support::paths::TempDir::new("yagura-holder-window", "library-gone");
    let out = tmp.path().join("window");
    let pid_file = tmp.path().join("grandchild.pid");
    let process = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            yagura_test_support::process::libtest_name(module_path!(), LIBRARY_HOST).as_str(),
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(LIBRARY_HOST_OUT_ENV, &out)
        .env(LIBRARY_HOST_PID_ENV, &pid_file)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("start the library host");
    let mut host = LibraryHost {
        process,
        holders: None,
    };

    let deadline = tokio::time::Instant::now() + yagura_test_support::HANG_GUARD;
    let window_id = loop {
        if let Ok(text) = std::fs::read_to_string(&out)
            && let Some(window_id) = text.strip_suffix('\n')
        {
            break window_id.to_owned();
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the library host never opened its window"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    let prefix = format!("yagura-holders-{}-", host.process.id());
    let holders = std::fs::read_dir(std::env::temp_dir())
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|dir| {
            dir.file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with(&prefix))
        })
        .expect("the library host's holders folder");
    host.holders = Some(holders.clone());
    let record = read_holder_record(&holders, &window_id).expect("the holder's record");
    let mut watches = vec![
        Watch::new("holder", record.holder_pid),
        Watch::new(
            "child",
            record.child_pid.expect("the record names its child"),
        ),
        Watch::new("grandchild", published_pid(&pid_file).await),
    ];
    watches.extend(pseudoconsole_hosts(record.holder_pid));

    host.process.kill().unwrap();
    host.process.wait().unwrap();

    wait_all_gone(watches).await;
    let prefix = format!("{window_id}.");
    let left: Vec<_> = std::fs::read_dir(&holders)
        .into_iter()
        .flatten()
        .map(|entry| entry.unwrap().file_name())
        .filter(|name| name.to_string_lossy().starts_with(&prefix))
        .collect();
    assert!(
        left.is_empty(),
        "the library registry going away left the holder's {left:?} behind"
    );
}

/// The library host process and its place, which it cannot remove once killed: ends
/// whatever the place's records still name, then removes it.
struct LibraryHost {
    process: std::process::Child,
    holders: Option<PathBuf>,
}

impl Drop for LibraryHost {
    fn drop(&mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
        if let Some(holders) = &self.holders {
            let _ = yagura_test_support::process::end_recorded_holders(holders, None);
            let _ = std::fs::remove_dir_all(holders);
        }
    }
}

#[tokio::test]
#[ignore = "the library host of a_library_registry_that_goes_away_unclosed_ends_every_tree_and_holder"]
async fn library_host() {
    let (Some(out), Some(pid_file)) = (
        std::env::var_os(LIBRARY_HOST_OUT_ENV),
        std::env::var_os(LIBRARY_HOST_PID_ENV),
    ) else {
        return;
    };
    let command =
        yagura_test_support::process::first_grandchild_command(Path::new(&pid_file), 600_000);
    let (registry, window_id) = open_window(command).await;
    std::fs::write(&out, format!("{window_id}\n")).unwrap();
    // Held until the test kills this process.
    let _registry = registry;
    std::future::pending::<()>().await;
}

#[tokio::test]
async fn the_last_output_before_the_exit_is_in_the_model_once_the_exit_is_known() {
    let child = capture_child_path();
    let command = vec![
        child
            .to_str()
            .expect("capture_child path must be UTF-8")
            .to_string(),
        "echo".to_string(),
        "7".to_string(),
        "LAST-WORDS".to_string(),
    ];
    let (registry, window_id) = open_window(command).await;
    let entry = registry.get(&window_id).expect("window must exist");
    let session = entry.session();

    let mut exited = session.exit_notified();
    tokio::time::timeout(
        yagura_test_support::HANG_GUARD,
        exited.wait_for(|&gone| gone),
    )
    .await
    .expect("the child's exit must be announced")
    .expect("the exit channel must stay open while the session lives");

    let (text, _) = session
        .text_tail(24, 4096)
        .await
        .expect("the model must be readable");
    assert!(
        text.contains("LAST-WORDS"),
        "output written before the exit must be in the model once the exit is known; it \
         shows {text:?}"
    );
    let screen = session
        .read_screen(|copy| reading::bottom_lines(copy, usize::from(copy.rows)).join("\n"))
        .await
        .expect("the screen copy must be readable");
    assert!(
        screen.contains("LAST-WORDS"),
        "output written before the exit must be on the screen the daemon reads once the exit \
         is known; it shows {screen:?}"
    );
    assert_eq!(session.exit_status(), Some(7));
}

/// Many more small writes than one screen holds.
const FLOOD_BYTES: u64 = 4 * 1024 * 1024 + 64 * 1024;

/// A child that floods in small writes and then writes a marker, staying alive,
/// has the marker reach the model and the copy the daemon reads: the copies the
/// daemon acknowledges keep coming however many writes there are.
#[tokio::test]
async fn a_flood_in_small_writes_ends_with_its_marker_in_the_model_and_the_copy() {
    let child =
        holder_path().with_file_name(format!("holder_child{}", std::env::consts::EXE_SUFFIX));
    let total = FLOOD_BYTES;
    let tmp = yagura_test_support::paths::TempDir::new("yagura-holder-window", "ack-flood");
    let count_file = tmp.path().join("count");
    let command = vec![
        child
            .to_str()
            .expect("holder_child path must be UTF-8")
            .to_string(),
        "flood".to_string(),
        count_file
            .to_str()
            .expect("count file path must be UTF-8")
            .to_string(),
        total.to_string(),
    ];
    let (registry, window_id) = open_window(command).await;
    let entry = registry.get(&window_id).expect("window must exist");
    let session = entry.session();

    let deadline = tokio::time::Instant::now() + yagura_test_support::HANG_GUARD;
    let mut arrivals = session.output_arrivals();
    loop {
        arrivals.borrow_and_update();
        let (text, _) = session
            .text_tail(24, 8192)
            .await
            .expect("the model must be readable");
        if text.contains("HOLDER-CHILD-FLOODED") {
            break;
        }
        tokio::time::timeout_at(deadline, arrivals.changed())
            .await
            .unwrap_or_else(|_| {
                panic!("the marker after {total} bytes never reached the model; it shows {text:?}")
            })
            .expect("the arrivals channel must stay open while the session lives");
    }
    loop {
        arrivals.borrow_and_update();
        let screen = session
            .read_screen(|copy| reading::bottom_lines(copy, usize::from(copy.rows)).join("\n"))
            .await
            .expect("the screen copy must be readable");
        if screen.contains("HOLDER-CHILD-FLOODED") {
            break;
        }
        tokio::time::timeout_at(deadline, arrivals.changed())
            .await
            .unwrap_or_else(|_| {
                panic!("the marker after {total} bytes never reached the copy; it shows {screen:?}")
            })
            .expect("the arrivals channel must stay open while the session lives");
    }
    assert_eq!(session.exit_status(), None, "the child must still be alive");

    let _ = yagura_daemon::control::close_window(&entry, true, Duration::from_secs(5)).await;
    drop(tmp);
}
