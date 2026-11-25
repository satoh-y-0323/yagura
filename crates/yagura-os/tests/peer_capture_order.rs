#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The order in which macOS, Linux and Windows read a connection's peer, with
//! the operating system's reads replaced by a script. The peer is captured only when the
//! token read, the peer's start and the other end of the socket in its
//! descriptors all agree, and the token and start read again afterwards are
//! the same: a process that ended and had its id reused, or that holds no end
//! of the connection, is not captured.
//!
//! Compiled on every OS so the order is measured wherever the tests run. The
//! names under `yagura_os::peer_capture` are spelled inside each test.

use std::collections::VecDeque;
use std::path::PathBuf;

use yagura_os::ProcessStart;

const ENOTCONN: i32 = 57;
const EINVAL: i32 = 22;
const PEER: u32 = 4242;
const PCB: u64 = 0xABCD_0001;

fn start(text: &str) -> ProcessStart {
    text.parse().expect("a start reads back from its string")
}

/// What each read answers, in the order it is asked.
struct Script {
    tokens: VecDeque<Result<(u32, u32), i32>>,
    starts: VecDeque<Option<ProcessStart>>,
    own_pcb: Option<u64>,
    /// The `(pid, pcb)` pairs the peer's descriptors are asked about and
    /// answered yes for.
    held: Option<(u32, u64)>,
    asked_held: Vec<(u32, u64)>,
}

impl Script {
    fn agreeing() -> Self {
        Self {
            tokens: VecDeque::from([Ok((PEER, 7)), Ok((PEER, 7))]),
            starts: VecDeque::from([Some(start("boot:100")), Some(start("boot:100"))]),
            own_pcb: Some(PCB),
            held: Some((PEER, PCB)),
            asked_held: Vec::new(),
        }
    }
}

impl yagura_os::peer_capture::ProcessReads for Script {
    fn start_of(&mut self, _pid: u32) -> Option<ProcessStart> {
        self.starts.pop_front().flatten()
    }

    fn executable_of(&mut self, _pid: u32) -> Option<PathBuf> {
        Some(PathBuf::from("/opt/peer"))
    }
}

impl yagura_os::peer_capture::OrderReads for Script {
    fn peer_token(&mut self) -> Result<(u32, u32), i32> {
        self.tokens.pop_front().unwrap_or(Err(ENOTCONN))
    }

    fn own_peer_pcb(&mut self) -> Option<u64> {
        self.own_pcb
    }

    fn peer_holds(&mut self, pid: u32, pcb: u64) -> bool {
        self.asked_held.push((pid, pcb));
        self.held == Some((pid, pcb))
    }
}

fn capture(script: &mut Script) -> Option<yagura_os::peer_capture::CapturedPeer> {
    yagura_os::peer_capture::capture_in_order(script)
}

#[test]
fn a_peer_whose_reads_all_agree_is_captured() {
    let mut script = Script::agreeing();
    let captured = capture(&mut script).expect("every read agrees");
    assert_eq!(captured.pid, PEER);
    assert_eq!(captured.start, start("boot:100"));
    assert_eq!(captured.executable, PathBuf::from("/opt/peer"));
    assert_eq!(
        script.asked_held,
        vec![(PEER, PCB)],
        "the peer is asked for the other end of this process's own socket"
    );
}

#[test]
fn a_peer_holding_another_end_than_this_connections_is_not_captured() {
    let mut script = Script::agreeing();
    script.held = Some((PEER, PCB + 1));
    assert!(capture(&mut script).is_none());
}

#[test]
fn a_connection_whose_own_socket_names_no_other_end_is_not_captured() {
    let mut script = Script::agreeing();
    script.own_pcb = None;
    script.held = Some((PEER, 0));
    assert!(capture(&mut script).is_none());
}

#[test]
fn a_peer_whose_pidversion_changes_between_the_reads_is_not_captured() {
    let mut script = Script::agreeing();
    script.tokens = VecDeque::from([Ok((PEER, 7)), Ok((PEER, 8))]);
    assert!(capture(&mut script).is_none());
}

#[test]
fn a_peer_whose_pid_changes_between_the_reads_is_not_captured() {
    let mut script = Script::agreeing();
    script.tokens = VecDeque::from([Ok((PEER, 7)), Ok((PEER + 1, 7))]);
    assert!(capture(&mut script).is_none());
}

#[test]
fn a_peer_whose_start_changes_between_the_reads_is_not_captured() {
    let mut script = Script::agreeing();
    script.starts = VecDeque::from([Some(start("boot:100")), Some(start("boot:101"))]);
    assert!(capture(&mut script).is_none());
}

#[test]
fn a_peer_whose_start_cannot_be_read_is_not_captured() {
    let mut script = Script::agreeing();
    script.starts = VecDeque::from([None, None]);
    assert!(capture(&mut script).is_none());
}

#[test]
fn a_token_that_cannot_be_read_captures_no_peer() {
    for errno in [ENOTCONN, EINVAL] {
        let mut script = Script::agreeing();
        script.tokens = VecDeque::from([Err(errno)]);
        assert!(capture(&mut script).is_none(), "errno {errno}");
        assert!(
            script.asked_held.is_empty(),
            "a peer was looked for in descriptors after errno {errno}"
        );
    }
}

#[test]
fn a_token_that_cannot_be_read_again_captures_no_peer() {
    let mut script = Script::agreeing();
    script.tokens = VecDeque::from([Ok((PEER, 7)), Err(ENOTCONN)]);
    assert!(capture(&mut script).is_none());
}

/// The Linux reads, in the order they are asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Read {
    Pidfd,
    Pid,
    Start,
    Executable,
    HasExited,
}

struct LinuxScript {
    pidfd: bool,
    pid: Option<u32>,
    start: Option<ProcessStart>,
    executable: Option<PathBuf>,
    has_exited: Result<bool, i32>,
    asked: Vec<Read>,
}

impl LinuxScript {
    fn alive() -> Self {
        Self {
            pidfd: true,
            pid: Some(PEER),
            start: Some(start("boot:100")),
            executable: Some(PathBuf::from("/opt/peer")),
            has_exited: Ok(false),
            asked: Vec::new(),
        }
    }
}

impl yagura_os::peer_capture::PidfdReads for LinuxScript {
    fn peer_pidfd(&mut self) -> bool {
        self.asked.push(Read::Pidfd);
        self.pidfd
    }

    fn pidfd_has_exited(&mut self) -> Result<bool, i32> {
        self.asked.push(Read::HasExited);
        self.has_exited
    }
}

impl yagura_os::peer_capture::PeerPidReads for LinuxScript {
    fn peer_pid(&mut self) -> Option<u32> {
        self.asked.push(Read::Pid);
        self.pid
    }
}

impl yagura_os::peer_capture::ProcessReads for LinuxScript {
    fn start_of(&mut self, pid: u32) -> Option<ProcessStart> {
        assert_eq!(Some(pid), self.pid, "start is read for the pid just read");
        self.asked.push(Read::Start);
        self.start.clone()
    }

    fn executable_of(&mut self, pid: u32) -> Option<PathBuf> {
        assert_eq!(
            Some(pid),
            self.pid,
            "executable is read for the pid just read"
        );
        self.asked.push(Read::Executable);
        self.executable.clone()
    }
}

fn capture_pidfd(script: &mut LinuxScript) -> Option<yagura_os::peer_capture::CapturedPeer> {
    yagura_os::peer_capture::capture_in_pidfd_order(script)
}

#[test]
fn a_pidfd_peer_running_after_every_read_is_captured_pidfd_first_check_last() {
    let mut script = LinuxScript::alive();
    let captured = capture_pidfd(&mut script).expect("every read agrees and the peer runs");
    assert_eq!(captured.pid, PEER);
    assert_eq!(captured.start, start("boot:100"));
    assert_eq!(captured.executable, PathBuf::from("/opt/peer"));
    assert_eq!(script.asked.first(), Some(&Read::Pidfd));
    assert_eq!(script.asked.last(), Some(&Read::HasExited));
    for read in [Read::Pid, Read::Start, Read::Executable] {
        assert_eq!(
            script.asked.iter().filter(|r| **r == read).count(),
            1,
            "{read:?}"
        );
    }
}

#[test]
fn a_pidfd_peer_that_ended_after_the_other_reads_is_not_captured() {
    let mut script = LinuxScript::alive();
    script.has_exited = Ok(true);
    assert!(capture_pidfd(&mut script).is_none());
    assert_eq!(script.asked.last(), Some(&Read::HasExited));
}

#[test]
fn a_pidfd_peer_whose_liveness_cannot_be_checked_is_not_captured() {
    for errno in [EINVAL, ENOTCONN] {
        let mut script = LinuxScript::alive();
        script.has_exited = Err(errno);
        assert!(capture_pidfd(&mut script).is_none(), "errno {errno}");
    }
}

#[test]
fn a_connection_that_yields_no_pidfd_reads_nothing_else_and_captures_no_peer() {
    let mut script = LinuxScript::alive();
    script.pidfd = false;
    assert!(capture_pidfd(&mut script).is_none());
    assert_eq!(script.asked, vec![Read::Pidfd]);
}

#[test]
fn a_pidfd_peer_with_a_read_that_fails_is_not_captured() {
    let breakers: [fn(&mut LinuxScript); 3] = [
        |s| s.pid = None,
        |s| s.start = None,
        |s| s.executable = None,
    ];
    for (index, break_read) in breakers.into_iter().enumerate() {
        let mut script = LinuxScript::alive();
        break_read(&mut script);
        assert!(capture_pidfd(&mut script).is_none(), "breaker {index}");
    }
}

/// The Windows reads: the pipe's client id, then the start, the executable
/// and the start again of the process it names.
struct PipeScript {
    pid: Option<u32>,
    starts: VecDeque<Option<ProcessStart>>,
    executable: Option<PathBuf>,
    asked: Vec<Read>,
}

impl PipeScript {
    fn steady() -> Self {
        Self {
            pid: Some(PEER),
            starts: VecDeque::from([Some(start("100")), Some(start("100"))]),
            executable: Some(PathBuf::from(r"C:\peer.exe")),
            asked: Vec::new(),
        }
    }
}

impl yagura_os::peer_capture::PeerPidReads for PipeScript {
    fn peer_pid(&mut self) -> Option<u32> {
        self.asked.push(Read::Pid);
        self.pid
    }
}

impl yagura_os::peer_capture::ProcessReads for PipeScript {
    fn start_of(&mut self, pid: u32) -> Option<ProcessStart> {
        assert_eq!(Some(pid), self.pid, "start is read for the pid just read");
        self.asked.push(Read::Start);
        self.starts.pop_front().flatten()
    }

    fn executable_of(&mut self, pid: u32) -> Option<PathBuf> {
        assert_eq!(
            Some(pid),
            self.pid,
            "executable is read for the pid just read"
        );
        self.asked.push(Read::Executable);
        self.executable.clone()
    }
}

fn capture_pipe(script: &mut PipeScript) -> Option<yagura_os::peer_capture::CapturedPeer> {
    yagura_os::peer_capture::capture_in_start_order(script)
}

#[test]
fn a_pipe_peer_whose_start_reads_the_same_after_its_executable_is_captured() {
    let mut script = PipeScript::steady();
    let captured = capture_pipe(&mut script).expect("both starts agree");
    assert_eq!(captured.pid, PEER);
    assert_eq!(captured.start, start("100"));
    assert_eq!(captured.executable, PathBuf::from(r"C:\peer.exe"));
    assert_eq!(
        script.asked,
        [Read::Pid, Read::Start, Read::Executable, Read::Start],
        "the start is read again after the executable"
    );
}

/// The id named another process by the second read: the executable read
/// between may be either one's.
#[test]
fn a_pipe_peer_whose_start_changes_by_the_second_read_is_not_captured() {
    let mut script = PipeScript::steady();
    script.starts = VecDeque::from([Some(start("100")), Some(start("101"))]);
    assert!(capture_pipe(&mut script).is_none());
    assert_eq!(script.asked.last(), Some(&Read::Start));
}

#[test]
fn a_pipe_peer_with_a_read_that_fails_is_not_captured() {
    let breakers: [fn(&mut PipeScript); 4] = [
        |s| s.pid = None,
        |s| s.starts = VecDeque::from([None, Some(start("100"))]),
        |s| s.executable = None,
        |s| s.starts = VecDeque::from([Some(start("100")), None]),
    ];
    for (index, break_read) in breakers.into_iter().enumerate() {
        let mut script = PipeScript::steady();
        break_read(&mut script);
        assert!(capture_pipe(&mut script).is_none(), "breaker {index}");
    }

    let mut no_client = PipeScript::steady();
    no_client.pid = None;
    assert!(capture_pipe(&mut no_client).is_none());
    assert_eq!(no_client.asked, [Read::Pid], "read on after no client id");
}
