#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// The peers captured below are real processes this binary starts as its own
// fixtures, so they are spawned directly.
#![allow(clippy::disallowed_methods)]

//! Integration test: what the daemon captures of the process at the other end
//! of an accepted connection (its id, start and executable) and the startup
//! check that the capture is available.
//!
//! cfg-free by design: `PlatformIpc` resolves to the platform under
//! compilation, so this file exercises whichever implementation that is
//! without a `#[cfg(...)]` split of its own. The accept/connect shape is
//! the same one `os_unix.rs::peer_id_resolves_the_connecting_uid` uses;
//! `os_security.rs` is not a usable template here because it is
//! `#![cfg(windows)]` and never establishes a connection.

#[cfg(unix)]
use std::io::{BufRead as _, BufReader};
use std::process::{Child, Command, Stdio};

use yagura_os::{IpcTransport, PeerAuth, PlatformIpc, process_start};
use yagura_test_support::ipc::unique_endpoint_name;
use yagura_test_support::paths::TempDir;
use yagura_test_support::process::{install_exe, libtest_name, own_canonical_exe};

/// Endpoint-name prefix for this file, kept distinct from the daemon's own so
/// an endpoint created here can never be mistaken for a resident daemon's.
const ENDPOINT_PREFIX: &str = "yagura-test-peer-executable";

// What is captured of the peer. Names under `yagura_os::peer_capture` and the
// `capture_peer` method are spelled inside each test, where they are used.

const CHILD: &str = "peer_capture_child";
const ENDPOINT_ENV: &str = "YAGURA_OS_TEST_PEER_CAPTURE_ENDPOINT";
const MODE_ENV: &str = "YAGURA_OS_TEST_PEER_CAPTURE_MODE";
#[cfg(unix)]
const FD_ENV: &str = "YAGURA_OS_TEST_PEER_CAPTURE_FD";
/// Printed by a child once it has done what its mode says.
const READY: &str = "@peer-capture ready";

/// Ends a child when the test ends, passed or failed.
struct Peer(Child);

impl Drop for Peer {
    fn drop(&mut self) {
        drop(self.0.stdin.take());
        let _ = self.0.wait();
    }
}

/// The child of the tests below. By mode: `hold` connects and lives until its
/// stdin ends; `exit` connects and ends; on Unix `inherit-keep` and
/// `inherit-close` connect the socket the parent made (descriptor `FD_ENV`)
/// and then keep or close it, and live until stdin ends.
#[tokio::test]
#[ignore = "the child of the peer capture tests"]
async fn peer_capture_child() {
    let (Some(endpoint), Some(mode)) = (std::env::var_os(ENDPOINT_ENV), std::env::var_os(MODE_ENV))
    else {
        return;
    };
    let endpoint = endpoint.into_string().unwrap();
    let mode = mode.into_string().unwrap();
    let ipc = PlatformIpc::default();
    let _connection = match mode.as_str() {
        "hold" | "exit" => Some(ipc.connect(&endpoint).await.unwrap()),
        #[cfg(unix)]
        "inherit-keep" | "inherit-close" => {
            let fd: std::os::fd::RawFd = std::env::var(FD_ENV).unwrap().parse().unwrap();
            connect_inherited(fd, &endpoint);
            if mode == "inherit-close" {
                // SAFETY: `fd` is the inherited socket, closed once here.
                assert_eq!(unsafe { libc::close(fd) }, 0);
            }
            None
        }
        other => panic!("unknown mode {other}"),
    };
    println!("{READY}");
    if mode != "exit" {
        std::io::copy(&mut std::io::stdin().lock(), &mut std::io::sink()).unwrap();
    }
}

#[cfg(unix)]
fn connect_inherited(fd: std::os::fd::RawFd, path: &str) {
    // SAFETY: an all-zero `sockaddr_un` is a valid value to fill in.
    let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
    let bytes = path.as_bytes();
    assert!(bytes.len() < addr.sun_path.len());
    for (slot, byte) in addr.sun_path.iter_mut().zip(bytes) {
        *slot = *byte as libc::c_char;
    }
    // SAFETY: `fd` is a socket and `addr` a filled `sockaddr_un` of the
    // size passed.
    let status = unsafe {
        libc::connect(
            fd,
            std::ptr::addr_of!(addr).cast(),
            std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t,
        )
    };
    assert_eq!(status, 0, "{}", std::io::Error::last_os_error());
}

/// A copy of this test binary, so the peer's executable differs from this
/// process's.
fn binary_copy(dir: &TempDir) -> std::path::PathBuf {
    let copy = dir.path().join(if cfg!(windows) {
        "peer-copy.exe"
    } else {
        "peer-copy"
    });
    install_exe(&std::env::current_exe().unwrap(), &copy);
    copy
}

fn start_child(copy: &std::path::Path, endpoint: &str, mode: &str) -> Command {
    let mut command = Command::new(copy);
    command
        .args([
            libtest_name(module_path!(), CHILD).as_str(),
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(ENDPOINT_ENV, endpoint)
        .env(MODE_ENV, mode)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    command
}

/// Blocks until the child has printed `READY`.
#[cfg(unix)]
fn wait_ready(child: &mut Child) {
    let stdout = child.stdout.as_mut().unwrap();
    for line in BufReader::new(stdout).lines() {
        if line.unwrap().contains(READY) {
            return;
        }
    }
    panic!("the child ended before it was ready");
}

async fn bind(tag: &str) -> (PlatformIpc, <PlatformIpc as IpcTransport>::Listener, String) {
    let ipc = PlatformIpc::default();
    let security = ipc.endpoint_security().unwrap();
    let endpoint = unique_endpoint_name(ENDPOINT_PREFIX, tag);
    let listener = ipc.bind(&endpoint, security).await.unwrap();
    (ipc, listener, endpoint)
}

#[tokio::test]
async fn a_connection_from_this_process_is_captured_as_this_process() {
    let (ipc, mut listener, endpoint) = bind("capture-self").await;
    let (accepted, connected) = tokio::join!(ipc.accept(&mut listener), ipc.connect(&endpoint));
    let accepted = accepted.unwrap();
    let _connected = connected.unwrap();

    let captured = ipc
        .capture_peer(&accepted)
        .expect("a live peer of this process is captured");
    assert_eq!(captured.pid, std::process::id());
    assert_eq!(
        Some(captured.start),
        process_start(std::process::id()).unwrap()
    );
    assert_eq!(
        captured.executable.canonicalize().unwrap(),
        own_canonical_exe()
    );
}

#[tokio::test]
async fn a_connection_from_a_child_is_captured_as_the_child_and_its_executable() {
    let (ipc, mut listener, endpoint) = bind("capture-child").await;
    let dir = TempDir::new("yagura-peer-capture", "child");
    let copy = binary_copy(&dir);
    let child = Peer(start_child(&copy, &endpoint, "hold").spawn().unwrap());
    let accepted = ipc.accept(&mut listener).await.unwrap();

    let captured = ipc
        .capture_peer(&accepted)
        .expect("a live child's connection is captured");
    assert_eq!(captured.pid, child.0.id());
    assert_eq!(
        Some(captured.start),
        process_start(child.0.id()).unwrap(),
        "the start is the child's"
    );
    assert_eq!(
        captured.executable.canonicalize().unwrap(),
        copy.canonicalize().unwrap(),
        "the executable is the copy the child runs, not this test binary"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_connection_whose_peer_has_ended_and_been_reaped_is_not_captured() {
    let (ipc, mut listener, endpoint) = bind("capture-reaped").await;
    let dir = TempDir::new("yagura-peer-capture", "reaped");
    let copy = binary_copy(&dir);
    let mut child = start_child(&copy, &endpoint, "exit").spawn().unwrap();
    let accepted = ipc.accept(&mut listener).await.unwrap();
    assert!(child.wait().unwrap().success());

    assert!(
        ipc.capture_peer(&accepted).is_none(),
        "a peer that has ended is not captured"
    );
}

/// Makes the socket the child connects, in this process, so that both hold
/// it; returns its descriptor.
#[cfg(unix)]
fn inheritable_socket() -> std::os::fd::RawFd {
    // SAFETY: a plain `socket(2)` call; the descriptor is returned to the
    // caller, which closes it.
    let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
    assert!(fd >= 0, "{}", std::io::Error::last_os_error());
    fd
}

/// The captured pid (0 when none) of a connection the child made over a
/// socket this process made and the child inherited, once the child has kept
/// or closed its end as `mode` says. A captured peer is the child, with the
/// child's start.
#[cfg(unix)]
async fn capture_after_inherited_connect(mode: &str) -> u32 {
    let (ipc, mut listener, endpoint) = bind(mode).await;
    let dir = TempDir::new("yagura-peer-capture", mode);
    let copy = binary_copy(&dir);
    let fd = inheritable_socket();
    let mut command = start_child(&copy, &endpoint, mode);
    command.env(FD_ENV, fd.to_string());
    let mut child = Peer(command.spawn().unwrap());
    wait_ready(&mut child.0);
    let accepted = ipc.accept(&mut listener).await.unwrap();
    let captured = ipc.capture_peer(&accepted);
    // SAFETY: `fd` is this process's own socket, closed once here.
    unsafe { libc::close(fd) };
    captured.map_or(0, |peer| {
        assert_eq!(peer.pid, child.0.id());
        assert_eq!(Some(peer.start), process_start(peer.pid).unwrap());
        peer.pid
    })
}

/// macOS reads the peer as the last process that operated on the socket, so
/// a peer is captured only when its descriptors hold the other end.
#[cfg(unix)]
#[cfg_attr(
    not(target_os = "macos"),
    ignore = "the peer is captured by descriptor on macOS"
)]
#[tokio::test]
async fn a_peer_that_closed_its_end_and_lives_on_is_not_captured() {
    let pid = capture_after_inherited_connect("inherit-close").await;
    assert_eq!(pid, 0, "a live process that holds no end was captured");
}

#[cfg(unix)]
#[cfg_attr(
    not(target_os = "macos"),
    ignore = "the peer is captured by descriptor on macOS"
)]
#[tokio::test]
async fn a_peer_that_holds_its_end_is_captured() {
    let pid = capture_after_inherited_connect("inherit-keep").await;
    assert_ne!(pid, 0, "a process that holds its end was not captured");
}

/// The startup check is what daemon and GUI run before serving: here, on a
/// machine the tests run on, the capture is available.
#[test]
fn the_startup_check_passes_where_the_tests_run() {
    let checked = yagura_os::peer_capture::check_peer_capture_available();
    assert!(checked.is_ok(), "{checked:?}");
}
