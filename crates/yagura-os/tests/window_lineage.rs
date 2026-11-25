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

/// What each process id answers when it is opened: its creation ticks and
/// its creator's id, an error, or nothing for an id no process has. Every id
/// opened is recorded in order.
#[derive(Default)]
struct CreatorScript {
    answers: std::collections::HashMap<u32, Result<(u64, u32), ()>>,
    opened: Vec<u32>,
}

impl CreatorScript {
    fn with(mut self, pid: u32, created: u64, creator: u32) -> Self {
        self.answers.insert(pid, Ok((created, creator)));
        self
    }

    fn failing(mut self, pid: u32) -> Self {
        self.answers.insert(pid, Err(()));
        self
    }
}

impl yagura_os::window_lineage::CreatorReads for CreatorScript {
    fn creation_and_creator(&mut self, pid: u32) -> std::io::Result<Option<(u64, u32)>> {
        self.opened.push(pid);
        match self.answers.get(&pid) {
            None => Ok(None),
            Some(Ok(read)) => Ok(Some(*read)),
            Some(Err(())) => Err(std::io::Error::other("no answer for this process")),
        }
    }
}

/// The lineage walked from `peer` through `script`, for the anchor
/// `anchor_pid` and the daemon [`DAEMON`].
fn walk(
    script: &mut CreatorScript,
    peer: u32,
    anchor_pid: u32,
) -> yagura_os::window_lineage::Lineage {
    yagura_os::window_lineage::walk_creators(script, peer, anchor_pid, DAEMON)
}

/// Windows: a snapshot keeps a parent's id after the parent exits, so a
/// "parent" created after the child is another process that took the id.
#[test]
fn a_windows_parent_created_after_its_child_is_not_its_parent() {
    use yagura_os::window_lineage::{LineageEnd, windows_link_holds};
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

    // The walk: a creator created after its child stops it, unstacked.
    let mut later = CreatorScript::default().with(31, 200, 21).with(21, 201, 0);
    let inverted = walk(&mut later, 31, 13);
    assert_eq!(inverted.steps(), [step(31, "200")]);
    assert_eq!(inverted.end(), Some(LineageEnd::Inverted));

    let mut equal = CreatorScript::default().with(31, 200, 21).with(21, 200, 0);
    let through = walk(&mut equal, 31, 13);
    assert_eq!(
        through.steps(),
        [step(31, "200"), step(21, "200")],
        "an equal creation time was not followed"
    );
    assert_eq!(through.end(), Some(LineageEnd::Top));

    // A step's start is its creation ticks in decimal, as `process_start`.
    let mut wide = CreatorScript::default()
        .with(31, high + 1, 21)
        .with(21, high, 0);
    assert_eq!(
        walk(&mut wide, 31, 13).steps(),
        [
            step(31, &(high + 1).to_string()),
            step(21, &high.to_string())
        ]
    );
}

/// Windows: how the walk up the creators ends. Whatever cannot be read ends
/// it unanswered, a creator that is none or no process ends it at the top,
/// and the anchor and the daemon end it without their creators being read.
#[test]
fn the_windows_walk_ends_at_the_top_unanswered_or_where_the_search_ends() {
    use yagura_os::window_lineage::LineageEnd;

    for creator in [0, 31] {
        let mut script = CreatorScript::default().with(31, 200, creator);
        let lineage = walk(&mut script, 31, 13);
        assert_eq!(lineage.steps(), [step(31, "200")], "creator {creator}");
        assert_eq!(lineage.end(), Some(LineageEnd::Top), "creator {creator}");
        assert_eq!(script.opened, [31], "creator {creator} was opened");
    }

    let mut gone = CreatorScript::default().with(31, 200, 21);
    let lineage = walk(&mut gone, 31, 13);
    assert_eq!(lineage.steps(), [step(31, "200")]);
    assert_eq!(
        lineage.end(),
        Some(LineageEnd::Top),
        "a creator id no process has"
    );
    assert_eq!(gone.opened, [31, 21]);

    let mut unread_creator = CreatorScript::default().with(31, 200, 21).failing(21);
    let lineage = walk(&mut unread_creator, 31, 13);
    assert_eq!(lineage.steps(), [step(31, "200")]);
    assert_eq!(
        lineage.end(),
        Some(LineageEnd::Unanswered),
        "a creator that could not be read"
    );

    let mut unread_peer = CreatorScript::default().failing(31);
    let lineage = walk(&mut unread_peer, 31, 13);
    assert!(lineage.steps().is_empty(), "{:?}", lineage.steps());
    assert_eq!(
        lineage.end(),
        Some(LineageEnd::Unanswered),
        "a peer not read"
    );

    let mut no_peer = CreatorScript::default();
    let lineage = walk(&mut no_peer, 31, 13);
    assert!(lineage.steps().is_empty(), "{:?}", lineage.steps());
    assert_eq!(
        lineage.end(),
        Some(LineageEnd::Unanswered),
        "no peer process"
    );

    // The search ends at the anchor and at the daemon: above them nothing is
    // opened.
    let mut to_anchor = CreatorScript::default()
        .with(31, 200, 13)
        .with(13, 150, 40)
        .with(40, 100, 0);
    let lineage = walk(&mut to_anchor, 31, 13);
    assert_eq!(lineage.steps(), [step(31, "200"), step(13, "150")]);
    assert_eq!(lineage.end(), None);
    assert_eq!(to_anchor.opened, [31, 13], "read above the anchor");

    let mut to_daemon = CreatorScript::default()
        .with(31, 200, DAEMON)
        .with(DAEMON, 150, 40)
        .with(40, 100, 0);
    let lineage = walk(&mut to_daemon, 31, 13);
    assert_eq!(lineage.steps(), [step(31, "200"), step(DAEMON, "150")]);
    assert_eq!(lineage.end(), None);
    assert_eq!(to_daemon.opened, [31, DAEMON], "read above the daemon");

    // A chain longer than the steps looked at stops at 33 of them.
    let mut deep = (0..40_u32).fold(CreatorScript::default(), |script, at| {
        script.with(1000 + at, u64::from(10_000 - at), 1001 + at)
    });
    let lineage = walk(&mut deep, 1000, 13);
    assert_eq!(lineage.steps().len(), 33);
    assert_eq!(lineage.end(), None);
    assert_eq!(deep.opened.len(), 33, "opened past the steps looked at");
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
