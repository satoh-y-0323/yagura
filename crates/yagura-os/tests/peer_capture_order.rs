#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The order in which macOS reads a connection's peer, with the operating
//! system's reads replaced by a script. The peer is captured only when the
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

impl yagura_os::peer_capture::OrderReads for Script {
    fn peer_token(&mut self) -> Result<(u32, u32), i32> {
        self.tokens.pop_front().unwrap_or(Err(ENOTCONN))
    }

    fn start_of(&mut self, _pid: u32) -> Option<ProcessStart> {
        self.starts.pop_front().flatten()
    }

    fn executable_of(&mut self, _pid: u32) -> Option<PathBuf> {
        Some(PathBuf::from("/opt/peer"))
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
fn a_peer_with_no_descriptor_holding_the_other_end_is_not_captured() {
    let mut script = Script::agreeing();
    script.held = None;
    assert!(capture(&mut script).is_none());
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
