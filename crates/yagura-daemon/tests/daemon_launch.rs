#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: how the daemon is started and where it keeps its files.
//!
//! The session store and the user's configuration live under the user's own base
//! directories; one daemon serves an instance and `--shutdown` stops it; windows are
//! handed the MCP bridge installed beside the daemon; on Windows a daemon refuses to open
//! windows beside a bundled pseudoconsole that is not the pinned build, and the GUI's
//! detached start reports truthfully whether the daemon left the caller's job.

mod common;

use std::path::PathBuf;
use std::process::{Child, ExitStatus};
use std::time::Duration;

use common::real_daemon::{RunningDaemon, Sandbox, daemon_bin, shell_path};
use yagura_os::{IpcTransport, PeerAuth, PlatformIpc};
use yagura_protocol::{ClientMessage, DaemonMessage, MCP_BRIDGE_ENV};
use yagura_test_support::ipc::{ReadOutcome, read_daemon_frame};
use yagura_test_support::paths::strip_extended_length_prefix;
use yagura_test_support::process::env_probe_path;

#[cfg(windows)]
const DLL: &str = "conpty.dll";
#[cfg(windows)]
const HOST: &str = "OpenConsole.exe";

/// A copy of the built daemon in a folder of its own; on Windows beside a copy of the
/// vendored pseudoconsole pair, without which it opens no window.
fn install_copy(sandbox: &Sandbox) -> PathBuf {
    let dir = sandbox.tree.make_dir("install");
    #[cfg(windows)]
    {
        let vendored =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../vendor/conpty/x64");
        for name in [DLL, HOST] {
            std::fs::copy(vendored.join(name), dir.join(name)).unwrap();
        }
    }
    let bin = dir.join(format!("yagura-daemon{}", std::env::consts::EXE_SUFFIX));
    std::fs::copy(daemon_bin(), &bin).unwrap();
    bin
}

/// Waits for `child` to exit on its own; one still running after `limit` is killed and
/// fails the test.
async fn exit_status_within(child: &mut Child, limit: Duration, sandbox: &Sandbox) -> ExitStatus {
    let deadline = tokio::time::Instant::now() + limit;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        if tokio::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!(
                "the process was still running after {limit:?}; daemon log:\n{}",
                sandbox.daemon_log()
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// What `env_probe` reported for `name` once its whole report has arrived: `Some(None)`
/// when unset. Line breaks are dropped first, since a long value wraps.
fn reported_var(output: &str, name: &str) -> Option<Option<String>> {
    let flat: String = output
        .chars()
        .filter(|c| !matches!(c, '\r' | '\n'))
        .collect();
    let marker = format!("<<{name}");
    let rest = &flat[flat.rfind(&marker)? + marker.len()..];
    if rest.starts_with(" unset>>") {
        return Some(None);
    }
    let value = rest.strip_prefix('=')?;
    value.find(">>").map(|end| Some(value[..end].to_string()))
}

#[tokio::test]
async fn the_session_store_is_kept_under_the_users_data_directory() {
    let sandbox = Sandbox::new("launch-data-root");
    let _daemon = sandbox.start().await;
    let store = sandbox.data_root().join("yagura.db");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while !store.is_file() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the daemon never created its session store at {}; daemon log:\n{}",
            store.display(),
            sandbox.daemon_log()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test]
async fn profiles_are_read_from_the_users_config_directory() {
    let sandbox = Sandbox::new("launch-config-root");
    let profiles = sandbox.config_root().join("profiles");
    std::fs::create_dir_all(&profiles).unwrap();
    let probe = env_probe_path();
    common::write_profile(
        &profiles,
        "config-root-probe",
        &format!(
            "schema = 1\ndisplay_name = \"config-root-probe\"\n\n[command]\ndefault = ['{}', 'PATH']\n",
            probe.to_str().expect("env_probe path must be UTF-8")
        ),
    );
    let mut daemon = sandbox.start_on_config_root().await;

    daemon
        .open_profile_window(&sandbox, "config-root-probe")
        .await;
}

/// The windows the daemon lists, as `(window_id, cwd)` pairs.
async fn listed_windows(daemon: &mut RunningDaemon) -> Vec<(String, String)> {
    common::send(&mut daemon.client, &ClientMessage::ListWindows).await;
    match common::read_response(&mut daemon.client, Duration::from_secs(15)).await {
        DaemonMessage::WindowList { windows } => windows
            .into_iter()
            .map(|window| (window.window_id, window.cwd))
            .collect(),
        other => panic!("expected window_list, got {other:?}"),
    }
}

#[tokio::test]
async fn the_daemon_starts_with_no_window() {
    let sandbox = Sandbox::new("launch-no-window");
    let mut daemon = sandbox.start().await;

    let windows = listed_windows(&mut daemon).await;

    assert!(
        windows.is_empty(),
        "a freshly started daemon must hold no window, got {windows:?}"
    );
}

#[tokio::test]
async fn a_status_event_without_a_window_id_is_rejected() {
    let sandbox = Sandbox::new("launch-status-no-window-id");
    let mut daemon = sandbox.start().await;

    common::send(
        &mut daemon.client,
        &ClientMessage::StatusEvent {
            source: "cli".to_string(),
            status: yagura_protocol::WindowStatus::Running,
            window_id: None,
            token: "0123456789abcdef0123456789abcdef".to_string(),
        },
    )
    .await;

    match common::read_response(&mut daemon.client, Duration::from_secs(15)).await {
        DaemonMessage::Error { code, .. } => assert_eq!(code, "invalid_argument"),
        other => panic!("expected error(invalid_argument), got {other:?}"),
    }
}

#[tokio::test]
async fn a_window_opened_without_a_cwd_starts_in_the_home_directory() {
    let sandbox = Sandbox::new("launch-home-cwd");
    let home = sandbox.tree.make_dir("home");
    let home_str = home.to_str().expect("the sandbox home must be UTF-8");
    // The sandbox starts the daemon from a terminal (`TERM` set), and the test runner's own
    // current directory is not the home: neither may decide where the window starts.
    let mut daemon = sandbox.start_from(&[("USERPROFILE", home_str)]).await;

    common::send(
        &mut daemon.client,
        &ClientMessage::OpenWindow {
            command: vec![shell_path().to_str().unwrap().to_string()],
            cwd: None,
            name: None,
            vendor: None,
            detection: None,
        },
    )
    .await;
    let window_id = match common::read_response(&mut daemon.client, Duration::from_secs(15)).await {
        DaemonMessage::WindowOpened { window_id, .. } => window_id,
        other => panic!(
            "expected window_opened, got {other:?}; daemon log:\n{}",
            sandbox.daemon_log()
        ),
    };

    let windows = listed_windows(&mut daemon).await;
    let (_, cwd) = windows
        .iter()
        .find(|(id, _)| *id == window_id)
        .expect("the opened window must be listed");
    let expected = strip_extended_length_prefix(&home.canonicalize().unwrap());
    let actual = strip_extended_length_prefix(&std::path::Path::new(cwd).canonicalize().unwrap());
    let same = if cfg!(windows) {
        actual
            .to_string_lossy()
            .eq_ignore_ascii_case(&expected.to_string_lossy())
    } else {
        actual == expected
    };
    assert!(
        same,
        "the window must start in the home directory {}, but its cwd is {cwd}",
        expected.display()
    );
}

#[tokio::test]
async fn a_second_daemon_for_a_served_instance_exits_as_a_success_and_the_first_keeps_serving() {
    let sandbox = Sandbox::new("launch-second");
    let mut first = sandbox.start().await;

    let mut second = sandbox.spawn_bin(&daemon_bin());
    let status = exit_status_within(&mut second, Duration::from_secs(30), &sandbox).await;

    assert!(
        status.success(),
        "a daemon whose instance already answers must leave it to that daemon, got \
         {status:?}; daemon log:\n{}",
        sandbox.daemon_log()
    );
    common::send(&mut first.client, &ClientMessage::Ping).await;
    let reply = common::read_response(&mut first.client, Duration::from_secs(5)).await;
    assert!(matches!(reply, DaemonMessage::Pong), "got {reply:?}");
}

#[tokio::test]
async fn a_daemon_whose_endpoint_is_held_by_a_non_daemon_exits_as_a_failure() {
    let sandbox = Sandbox::new("launch-squatted");
    let ipc = PlatformIpc::default();
    let user = ipc
        .current_user_id()
        .expect("the current user id must resolve");
    let endpoint = yagura_os::default_endpoint_owning(&user, &sandbox.namespace());
    let security = ipc.endpoint_security().expect("endpoint security");
    // Holds the name and never answers a ping, as an unrelated process would.
    let _squatter = ipc
        .bind(&endpoint, security)
        .await
        .expect("claim the instance's endpoint");

    let mut daemon = sandbox.spawn_bin(&daemon_bin());
    let status = exit_status_within(&mut daemon, Duration::from_secs(30), &sandbox).await;

    assert!(
        !status.success(),
        "a daemon that can neither claim its endpoint nor reach a daemon on it must fail; \
         daemon log:\n{}",
        sandbox.daemon_log()
    );
}

#[tokio::test]
async fn shutdown_stops_the_daemon_serving_the_instance() {
    let sandbox = Sandbox::new("launch-shutdown");
    let mut daemon = sandbox.start().await;

    let mut shutdown = sandbox.spawn_with_args(&["--shutdown"]);
    let status = exit_status_within(&mut shutdown, Duration::from_secs(30), &sandbox).await;
    assert!(
        status.success(),
        "--shutdown must succeed against a running daemon, got {status:?}"
    );

    // The daemon's exit closes every connection it served.
    let closed = tokio::time::timeout(Duration::from_secs(30), async {
        while let ReadOutcome::Message(_) =
            read_daemon_frame(&mut daemon.client, Duration::from_secs(60)).await
        {}
    })
    .await;
    assert!(
        closed.is_ok(),
        "the daemon kept serving after --shutdown; daemon log:\n{}",
        sandbox.daemon_log()
    );
}

#[tokio::test]
async fn windows_are_handed_the_mcp_bridge_installed_beside_the_daemon() {
    let sandbox = Sandbox::new("launch-mcp-bridge");
    let bin = install_copy(&sandbox);
    let bridge = bin.with_file_name(format!("yagura-mcp{}", std::env::consts::EXE_SUFFIX));
    std::fs::copy(env_probe_path(), &bridge).unwrap();
    sandbox.write_var_probe_profile("bridge-probe", MCP_BRIDGE_ENV, "");
    let mut daemon = sandbox.start_bin(&bin).await;

    let window_id = daemon.open_profile_window(&sandbox, "bridge-probe").await;
    let text = daemon
        .output_until(&window_id, "the bridge report", |text| {
            reported_var(text, MCP_BRIDGE_ENV).is_some()
        })
        .await;

    let reported = reported_var(&text, MCP_BRIDGE_ENV).expect("the report was just seen");
    assert_eq!(
        reported
            .as_deref()
            .and_then(|value| std::fs::canonicalize(value).ok()),
        Some(std::fs::canonicalize(&bridge).unwrap()),
        "a window must be handed the bridge beside the daemon, got {reported:?}"
    );
}

#[cfg(windows)]
mod bundled_pair {
    use std::path::Path;
    use std::time::Duration;

    use yagura_protocol::{ClientMessage, DaemonMessage, DaemonProblemKind};

    use super::common::{self, real_daemon::Sandbox};
    use super::{HOST, install_copy};

    /// Flips one byte in the middle of `path`, keeping its length.
    fn corrupt(path: &Path) {
        let mut bytes = std::fs::read(path).unwrap();
        let middle = bytes.len() / 2;
        bytes[middle] ^= 0xff;
        std::fs::write(path, bytes).unwrap();
    }

    async fn open_probe(sandbox: &Sandbox, bin: &Path) -> DaemonMessage {
        sandbox.write_var_probe_profile("probe", "PATH", "");
        let mut daemon = sandbox.start_bin(bin).await;
        common::send(
            &mut daemon.client,
            &ClientMessage::OpenWindowFromProfile {
                profile: "probe".to_string(),
                cwd: None,
                name: None,
                model: None,
                effort: None,
            },
        )
        .await;
        common::read_response(&mut daemon.client, Duration::from_secs(15)).await
    }

    #[tokio::test]
    async fn a_daemon_beside_the_intact_pair_opens_windows() {
        let sandbox = Sandbox::new("launch-pair-intact");
        let bin = install_copy(&sandbox);

        let reply = open_probe(&sandbox, &bin).await;

        assert!(
            matches!(reply, DaemonMessage::WindowOpened { .. }),
            "a copy beside the vendored pair must open windows, got {reply:?}; daemon log:\n{}",
            sandbox.daemon_log()
        );
    }

    // A refused pair does not end the daemon, so a client can ask why through
    // `get_daemon_health`.
    #[tokio::test]
    async fn a_daemon_beside_an_altered_pseudoconsole_host_stays_up_and_reports_why() {
        let sandbox = Sandbox::new("launch-pair-altered");
        let bin = install_copy(&sandbox);
        corrupt(&bin.with_file_name(HOST));
        sandbox.write_var_probe_profile("probe", "PATH", "");

        let mut daemon = sandbox.start_bin(&bin).await;
        common::send(&mut daemon.client, &ClientMessage::GetDaemonHealth).await;
        match common::read_response(&mut daemon.client, Duration::from_secs(15)).await {
            DaemonMessage::DaemonHealth { problems } => {
                assert_eq!(problems.len(), 1, "{problems:?}");
                assert_eq!(problems[0].kind, DaemonProblemKind::WindowsUnavailable);
            }
            other => panic!("expected daemon_health, got {other:?}"),
        }

        common::send(
            &mut daemon.client,
            &ClientMessage::OpenWindowFromProfile {
                profile: "probe".to_string(),
                cwd: None,
                name: None,
                model: None,
                effort: None,
            },
        )
        .await;
        let reply = common::read_response(&mut daemon.client, Duration::from_secs(15)).await;
        assert!(
            !matches!(reply, DaemonMessage::WindowOpened { .. }),
            "no window may open beside an altered {HOST}, got {reply:?}"
        );
        let log = sandbox.daemon_log();
        assert!(
            log.contains("bundled ConPTY"),
            "the daemon must say the bundled pseudoconsole is why: {log}"
        );
    }
}

#[cfg(windows)]
mod detached {
    use std::ffi::OsStr;
    use std::time::{Duration, Instant};

    use windows::Win32::Foundation::{BOOL, CloseHandle, HANDLE};
    use windows::Win32::System::JobObjects::IsProcessInJob;
    use windows::Win32::System::Threading::{
        GetCurrentProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
        TerminateProcess,
    };
    use yagura_test_support::process::capture_child_path;

    use super::common;

    fn in_any_job(process: HANDLE) -> bool {
        let mut result = BOOL::default();
        // SAFETY: `process` is a live handle with query access and `result` is a live
        // out-parameter for the call.
        unsafe { IsProcessInJob(process, None, &mut result) }.expect("IsProcessInJob");
        result.as_bool()
    }

    // Only the GUI starts the daemon this way; `used_fallback` is what it tells the user
    // about the daemon dying with it.
    #[test]
    fn a_detached_start_leaves_the_callers_job_unless_it_reports_the_fallback() {
        let program = capture_child_path();
        let image = program.file_name().unwrap().to_string_lossy().to_string();
        let own_pid = std::process::id();

        let outcome =
            yagura_os::spawn_daemon_detached(&program, &[OsStr::new("sleep"), OsStr::new("30000")])
                .expect("the detached start must succeed");

        let deadline = Instant::now() + Duration::from_secs(10);
        let pid = loop {
            if let Some(row) = common::process_table()
                .into_iter()
                .find(|row| row.parent_pid == own_pid && row.image.eq_ignore_ascii_case(&image))
            {
                break row.pid;
            }
            assert!(
                Instant::now() < deadline,
                "the detached program never started"
            );
            std::thread::sleep(Duration::from_millis(50));
        };
        // SAFETY: the handle is closed below, after its last use.
        let process = unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE,
                false,
                pid,
            )
        }
        .expect("the detached program must be openable");
        let child_in_job = in_any_job(process);
        // SAFETY: `process` is the live handle opened above and is closed exactly once.
        unsafe {
            let _ = TerminateProcess(process, 1);
            let _ = CloseHandle(process);
        }
        // SAFETY: the pseudo handle of the current process needs no closing.
        let caller_in_job = in_any_job(unsafe { GetCurrentProcess() });

        if outcome.used_fallback {
            assert!(
                caller_in_job,
                "the fallback is only for a caller whose job refuses the breakaway"
            );
        } else {
            assert!(
                !child_in_job,
                "without the fallback reported, the daemon must not share the caller's job"
            );
        }
    }
}
