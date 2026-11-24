#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// Starts this test binary again as the process whose lineage is read.
#![allow(clippy::disallowed_methods)]

//! A connection is a window's own only when the process at its other end is
//! in the window's tree: the window's PTY child (the anchor) itself, a
//! descendant of it, or on Unix a member of its session while it still runs.
//! Whatever the operating system cannot answer, or answers in a way that
//! does not reach the anchor, is not a member.

use std::process::{Child, Command, Stdio};

use yagura_os::{Liveness, ProcessStart, process_start};

const DAEMON: u32 = 4_000_000_000;

fn start(text: &str) -> ProcessStart {
    text.parse().expect("a start reads back from its string")
}

/// A step read on Windows, which has no session.
fn step(pid: u32, start_text: &str) -> yagura_os::window_lineage::Step {
    yagura_os::window_lineage::Step {
        pid,
        start: start(start_text),
        sid: None,
    }
}

/// A step read on Unix, in the session `sid`.
fn unix_step(pid: u32, start_text: &str, sid: u32) -> yagura_os::window_lineage::Step {
    yagura_os::window_lineage::Step {
        pid,
        start: start(start_text),
        sid: Some(sid),
    }
}

/// The verdict on `steps` (the peer first, then each parent), read up to
/// `end`, for the peer `(pid, start)` kept at accept and the anchor
/// `(pid, start)` whose liveness reads `anchor_now`.
fn judge(
    steps: Vec<yagura_os::window_lineage::Step>,
    end: Option<yagura_os::window_lineage::LineageEnd>,
    peer: (u32, &str),
    anchor: (u32, &str),
    anchor_now: Liveness,
) -> yagura_os::window_lineage::LineageVerdict {
    use yagura_os::window_lineage::{Lineage, judge_lineage};
    let lineage = Lineage::new(steps, end);
    let peer_start = start(peer.1);
    let anchor_start = start(anchor.1);
    judge_lineage(
        &lineage,
        (peer.0, &peer_start),
        (anchor.0, &anchor_start),
        anchor_now,
        DAEMON,
    )
}

fn is_member(verdict: &yagura_os::window_lineage::LineageVerdict) -> bool {
    matches!(verdict, yagura_os::window_lineage::LineageVerdict::Member)
}

fn refusal(
    verdict: &yagura_os::window_lineage::LineageVerdict,
) -> Option<yagura_os::window_lineage::Refusal> {
    match verdict {
        yagura_os::window_lineage::LineageVerdict::Member => None,
        yagura_os::window_lineage::LineageVerdict::NotMember(why) => Some(*why),
    }
}

#[test]
fn the_anchor_itself_and_its_descendants_are_members() {
    use yagura_os::window_lineage::LineageEnd;
    let anchor = (13, "113");
    let itself = judge(
        vec![step(13, "113")],
        Some(LineageEnd::Top),
        anchor,
        anchor,
        Liveness::Alive,
    );
    assert!(is_member(&itself), "the anchor itself: {itself:?}");

    // hook -> hook shell -> the window's CLI (the anchor) -> holder.
    let descendant = judge(
        vec![
            step(31, "131"),
            step(21, "121"),
            step(13, "113"),
            step(40, "140"),
        ],
        Some(LineageEnd::Top),
        (31, "131"),
        anchor,
        Liveness::Alive,
    );
    assert!(is_member(&descendant), "a descendant: {descendant:?}");
}

#[test]
fn a_lineage_that_ends_before_the_anchor_is_not_a_member() {
    use yagura_os::window_lineage::{LineageEnd, Refusal};
    let anchor = (13, "113");
    let outside = || vec![step(31, "131"), step(21, "121")];
    let ended = |end| judge(outside(), Some(end), (31, "131"), anchor, Liveness::Alive);

    assert_eq!(refusal(&ended(LineageEnd::Top)), Some(Refusal::NoParent));
    assert_eq!(
        refusal(&ended(LineageEnd::Unanswered)),
        Some(Refusal::Unreadable)
    );
    assert_eq!(
        refusal(&ended(LineageEnd::Inverted)),
        Some(Refusal::Inverted)
    );
    let reused = ended(LineageEnd::Reused);
    assert!(!is_member(&reused), "a reused parent link: {reused:?}");

    let unread = judge(
        Vec::new(),
        Some(LineageEnd::Unanswered),
        (31, "131"),
        anchor,
        Liveness::Alive,
    );
    assert!(!is_member(&unread), "a peer never read: {unread:?}");
}

#[test]
fn a_peer_whose_start_differs_from_the_one_kept_at_accept_is_not_a_member() {
    use yagura_os::window_lineage::{LineageEnd, Refusal};
    let verdict = judge(
        vec![step(31, "999"), step(21, "121"), step(13, "113")],
        Some(LineageEnd::Top),
        (31, "131"),
        (13, "113"),
        Liveness::Alive,
    );
    assert_eq!(refusal(&verdict), Some(Refusal::PeerChanged));
}

#[test]
fn an_ended_anchor_has_no_members() {
    use yagura_os::window_lineage::{LineageEnd, Refusal};
    let anchor = (13, "113");
    let reaching = || vec![step(31, "131"), step(13, "113")];

    let dead = judge(
        reaching(),
        Some(LineageEnd::Top),
        (31, "131"),
        anchor,
        Liveness::Dead,
    );
    assert_eq!(refusal(&dead), Some(Refusal::AnchorEnded));

    let unknown = judge(
        reaching(),
        Some(LineageEnd::Top),
        (31, "131"),
        anchor,
        Liveness::Unknown,
    );
    assert!(
        !is_member(&unknown),
        "an anchor not answered for: {unknown:?}"
    );

    // The anchor's id now names a later process.
    let taken_over = judge(
        vec![step(31, "131"), step(13, "999"), step(40, "140")],
        Some(LineageEnd::Top),
        (31, "131"),
        anchor,
        Liveness::Alive,
    );
    assert!(
        !is_member(&taken_over),
        "the anchor's id under another start: {taken_over:?}"
    );
}

#[test]
fn the_anchor_is_looked_for_up_to_32_steps_above_the_peer() {
    use yagura_os::window_lineage::{LineageEnd, Refusal};
    // The peer, `links - 1` processes, then the anchor `links` steps up.
    let lineage_of = |links: u32| {
        let mut steps: Vec<_> = (0..links)
            .map(|at| step(1000 + at, &format!("s{at}")))
            .collect();
        steps.push(step(13, "113"));
        steps.push(step(40, "140"));
        steps
    };
    let at_limit = judge(
        lineage_of(32),
        Some(LineageEnd::Top),
        (1000, "s0"),
        (13, "113"),
        Liveness::Alive,
    );
    assert!(is_member(&at_limit), "the anchor 32 steps up: {at_limit:?}");
    let past_limit = judge(
        lineage_of(33),
        Some(LineageEnd::Top),
        (1000, "s0"),
        (13, "113"),
        Liveness::Alive,
    );
    assert_eq!(refusal(&past_limit), Some(Refusal::TooDeep));
}

/// No window's anchor is above the daemon, so reaching it ends the search.
#[test]
fn reaching_the_daemon_before_the_anchor_is_not_a_member() {
    use yagura_os::window_lineage::{LineageEnd, Refusal};
    let verdict = judge(
        vec![
            step(31, "131"),
            step(21, "121"),
            step(DAEMON, "1"),
            step(13, "113"),
        ],
        Some(LineageEnd::Top),
        (31, "131"),
        (13, "113"),
        Liveness::Alive,
    );
    assert_eq!(refusal(&verdict), Some(Refusal::ReachedDaemon));
}

/// Unix: a process in the anchor's session belongs to the window while the
/// anchor runs, even when its parents no longer lead to the anchor.
#[test]
fn a_member_of_the_running_anchors_session_is_a_member() {
    use yagura_os::window_lineage::{LineageEnd, Refusal};
    let anchor = (13, "113");
    // A hook handed to PID 1 after its shell ended, still in the session.
    let reparented = || vec![unix_step(31, "131", 13)];

    let alive = judge(
        reparented(),
        Some(LineageEnd::Top),
        (31, "131"),
        anchor,
        Liveness::Alive,
    );
    assert!(is_member(&alive), "a session member: {alive:?}");

    let dead = judge(
        reparented(),
        Some(LineageEnd::Top),
        (31, "131"),
        anchor,
        Liveness::Dead,
    );
    assert_eq!(refusal(&dead), Some(Refusal::AnchorEnded));

    // A process that started its own session under a session member.
    let below_member = judge(
        vec![unix_step(32, "132", 32), unix_step(31, "131", 13)],
        Some(LineageEnd::Top),
        (32, "132"),
        anchor,
        Liveness::Alive,
    );
    assert!(
        is_member(&below_member),
        "a descendant of a session member: {below_member:?}"
    );

    let other_session = judge(
        vec![unix_step(31, "131", 31)],
        Some(LineageEnd::Top),
        (31, "131"),
        anchor,
        Liveness::Alive,
    );
    assert_eq!(refusal(&other_session), Some(Refusal::NoParent));

    // Windows reads no session, so the same reparented hook is outside.
    let sessionless = judge(
        vec![step(31, "131")],
        Some(LineageEnd::Top),
        (31, "131"),
        anchor,
        Liveness::Alive,
    );
    assert!(!is_member(&sessionless), "no session read: {sessionless:?}");
}

/// Windows: a snapshot keeps a parent's id after the parent exits, so a
/// "parent" created after the child is another process that took the id.
#[test]
fn a_windows_parent_created_after_its_child_is_not_its_parent() {
    use yagura_os::window_lineage::windows_link_holds;
    assert!(windows_link_holds(100, 200));
    assert!(
        windows_link_holds(200, 200),
        "an equal creation time was refused"
    );
    assert!(!windows_link_holds(201, 200));
    // Raw 64-bit values, not their decimal strings: "10" sorts before "9".
    assert!(!windows_link_holds(10, 9));
    assert!(windows_link_holds(9, 10));
    let high = 1_u64 << 60;
    assert!(windows_link_holds(high, high + 1));
    assert!(!windows_link_holds(high + 1, high));
}

/// Unix: the parent read is the child's real parent only when the child's
/// ppid and start read before it and after it are both unchanged.
#[test]
fn a_unix_parent_read_holds_only_when_the_child_reads_the_same_on_both_sides() {
    use yagura_os::window_lineage::unix_link_holds;
    let child = start("boot-a:900");
    assert!(unix_link_holds((40, &child), (40, &child)));
    assert!(
        !unix_link_holds((40, &child), (1, &child)),
        "a child handed to PID 1 between the reads kept its parent"
    );
    let reused = start("boot-a:950");
    assert!(
        !unix_link_holds((40, &child), (40, &reused)),
        "a child id taken over between the reads kept its parent"
    );
    let earlier = start("boot-a:10");
    let later = start("boot-a:9");
    assert!(!unix_link_holds((40, &earlier), (40, &later)));
    assert!(!unix_link_holds((40, &later), (40, &earlier)));
}

const IDLE_CHILD: &str = "lineage_idle_child";
const IDLE_CHILD_ENV: &str = "YAGURA_OS_TEST_LINEAGE_IDLE_CHILD";

/// The child of the test below: lives until its stdin ends.
#[test]
#[ignore = "the child of the window lineage test"]
fn lineage_idle_child() {
    if std::env::var_os(IDLE_CHILD_ENV).is_none() {
        return;
    }
    std::io::copy(&mut std::io::stdin().lock(), &mut std::io::sink()).unwrap();
}

/// Ends the child when the test ends, passed or failed.
struct Idle(Child);

impl Drop for Idle {
    fn drop(&mut self) {
        drop(self.0.stdin.take());
        let _ = self.0.wait();
    }
}

fn spawn_idle_child() -> Idle {
    let child = Command::new(std::env::current_exe().unwrap())
        .args([
            yagura_test_support::process::libtest_name(module_path!(), IDLE_CHILD).as_str(),
            "--exact",
            "--ignored",
            "--test-threads=1",
        ])
        .env(IDLE_CHILD_ENV, "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start the idle child");
    Idle(child)
}

fn identity(pid: u32) -> (u32, ProcessStart) {
    let start = process_start(pid)
        .unwrap()
        .unwrap_or_else(|| panic!("the running process {pid} has a start"));
    (pid, start)
}

/// The operating system's own reading: a child of this test process is in
/// its own tree and in this process's, and this process is not in the
/// child's.
#[test]
fn the_os_reading_finds_a_child_in_its_parents_tree_and_not_the_reverse() {
    use yagura_os::window_lineage::judge_peer;
    let idle = spawn_idle_child();
    let (child, child_start) = identity(idle.0.id());
    let (own, own_start) = identity(std::process::id());

    let itself = judge_peer((child, &child_start), (child, &child_start), DAEMON);
    assert!(
        is_member(&itself),
        "the child as its own anchor: {itself:?}"
    );

    let below = judge_peer((child, &child_start), (own, &own_start), DAEMON);
    assert!(
        is_member(&below),
        "the child under this test process: {below:?}"
    );

    let above = judge_peer((own, &own_start), (child, &child_start), DAEMON);
    assert!(
        !is_member(&above),
        "this test process under its own child: {above:?}"
    );
}
