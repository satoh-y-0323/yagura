//! Whether a process belongs to a window's process tree, from what the
//! operating system reads of it.
//!
//! A window's tree is its PTY child (the anchor), named by its id and start,
//! and the processes below it: its descendants, and on Unix the members of
//! its session while it runs. Whatever cannot be read, or reads in a way
//! that does not reach the anchor, is not a member.

use crate::{Liveness, ProcessStart};

/// The most parent links above the peer at which the anchor is looked for.
const MOST_STEPS: usize = 32;

/// One process of a lineage read from the peer up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    /// Its id.
    pub pid: u32,
    /// Its start, which tells it from a later process with the same id.
    pub start: ProcessStart,
    /// Its session on Unix; `None` where there are no sessions (Windows).
    pub sid: Option<u32>,
}

impl Step {
    fn in_session_of(&self, leader: u32) -> bool {
        self.sid.is_some_and(|sid| in_leader_session(sid, leader))
    }
}

/// Why a lineage read from the operating system stops before the anchor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineageEnd {
    /// The last process has no parent left to follow.
    Top,
    /// Windows: the id the last process names as its creator now names a
    /// process created after it.
    Inverted,
    /// Unix: the last process did not read the same before and after its
    /// parent was read.
    Reused,
    /// The operating system did not answer for the next step.
    Unanswered,
}

/// The processes from the peer up, the peer first, each the real parent of
/// the one before it.
#[derive(Debug, Clone)]
pub struct Lineage {
    steps: Vec<Step>,
    end: Option<LineageEnd>,
}

impl Lineage {
    /// A lineage of already checked `steps`, the peer first; `end` is why it
    /// stops, `None` when it stops at a process the search ends at or past
    /// the most steps looked at.
    #[must_use]
    pub fn new(steps: Vec<Step>, end: Option<LineageEnd>) -> Self {
        Self { steps, end }
    }

    /// The steps read, the peer first.
    #[must_use]
    pub fn steps(&self) -> &[Step] {
        &self.steps
    }

    /// Why the read stopped before the anchor; `None` as for [`Lineage::new`].
    #[must_use]
    pub fn end(&self) -> Option<LineageEnd> {
        self.end
    }
}

/// The one read of a process the Windows walk up the creators makes.
pub trait CreatorReads {
    /// The creation time of the process `pid` as raw 100 ns ticks and the id
    /// of the process that created it, both of one process; `None` when no
    /// process has that id.
    ///
    /// # Errors
    ///
    /// Returns an error when the process is there but either cannot be
    /// read.
    fn creation_and_creator(&mut self, pid: u32) -> std::io::Result<Option<(u64, u32)>>;
}

/// Windows: walks from the process `pid` up its creators through `reads`,
/// to the anchor `anchor_pid`, the daemon `daemon_pid` or the most steps
/// looked at, whichever comes first. A creator id that now names a process
/// created after its child ends the walk [`LineageEnd::Inverted`], unstacked.
/// Never fails: whatever cannot be read ends it [`LineageEnd::Unanswered`].
#[must_use]
pub fn walk_creators<R: CreatorReads + ?Sized>(
    reads: &mut R,
    pid: u32,
    anchor_pid: u32,
    daemon_pid: u32,
) -> Lineage {
    let mut steps = Vec::new();
    let Ok(Some((mut child_created, mut creator))) = reads.creation_and_creator(pid) else {
        return Lineage::new(steps, Some(LineageEnd::Unanswered));
    };
    steps.push(creator_step(pid, child_created));
    let mut child = pid;
    while steps.len() <= MOST_STEPS {
        if steps
            .last()
            .is_some_and(|step| ends_search(step, anchor_pid, daemon_pid))
        {
            return Lineage::new(steps, None);
        }
        if creator == 0 || creator == child {
            return Lineage::new(steps, Some(LineageEnd::Top));
        }
        let (parent_created, parent_creator) = match reads.creation_and_creator(creator) {
            Ok(Some(read)) => read,
            Ok(None) => return Lineage::new(steps, Some(LineageEnd::Top)),
            Err(_) => return Lineage::new(steps, Some(LineageEnd::Unanswered)),
        };
        if !windows_link_holds(parent_created, child_created) {
            return Lineage::new(steps, Some(LineageEnd::Inverted));
        }
        steps.push(creator_step(creator, parent_created));
        child = creator;
        child_created = parent_created;
        creator = parent_creator;
    }
    Lineage::new(steps, None)
}

fn creator_step(pid: u32, created: u64) -> Step {
    Step {
        pid,
        start: ProcessStart::from_creation_ticks(created),
        sid: None,
    }
}

/// Why a process is not a member of a window's tree. For the log alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The lineage reached the daemon before the anchor.
    ReachedDaemon,
    /// The lineage ends at a process with no parent before the anchor.
    NoParent,
    /// The anchor is not within the most steps above the peer.
    TooDeep,
    /// The peer is not the process kept when its connection was accepted.
    PeerChanged,
    /// The anchor has ended.
    AnchorEnded,
    /// The operating system did not give a reading to judge.
    Unreadable,
    /// Windows: a creator was created after the process it created.
    Inverted,
}

/// The verdict on whether a process is a member of a window's tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineageVerdict {
    /// It is.
    Member,
    /// It is not, for the reason given.
    NotMember(Refusal),
}

/// Whether a process in the session `sid` is in the session `leader` leads.
/// The same rule names a window's session members for the sweep that ends
/// the window's tree.
pub(crate) fn in_leader_session(sid: u32, leader: u32) -> bool {
    sid == leader
}

/// Whether the search for the anchor ends at `step`: the anchor's id, its
/// session, or the daemon, above which no window's anchor is.
fn ends_search(step: &Step, anchor_pid: u32, daemon_pid: u32) -> bool {
    step.pid == anchor_pid || step.pid == daemon_pid || step.in_session_of(anchor_pid)
}

/// Judges `lineage`, read from the peer up, for the peer `(pid, start)` kept
/// when its connection was accepted, against the anchor `(pid, start)` whose
/// liveness read after the lineage is `anchor_now`. The search stops at the
/// daemon `daemon_pid`.
#[must_use]
pub fn judge_lineage(
    lineage: &Lineage,
    peer: (u32, &ProcessStart),
    anchor: (u32, &ProcessStart),
    anchor_now: Liveness,
    daemon_pid: u32,
) -> LineageVerdict {
    use LineageVerdict::{Member, NotMember};

    match anchor_now {
        Liveness::Alive => {}
        Liveness::Dead => return NotMember(Refusal::AnchorEnded),
        Liveness::Unknown => return NotMember(Refusal::Unreadable),
    }
    let Some(first) = lineage.steps.first() else {
        return NotMember(Refusal::Unreadable);
    };
    if first.pid != peer.0 || first.start != *peer.1 {
        return NotMember(Refusal::PeerChanged);
    }
    for (links, step) in lineage.steps.iter().enumerate() {
        if links > MOST_STEPS {
            return NotMember(Refusal::TooDeep);
        }
        if step.pid == daemon_pid {
            return NotMember(Refusal::ReachedDaemon);
        }
        if step.pid == anchor.0 {
            // The anchor runs with its start, so its id names no other
            // process: another start read under it is a reading from before
            // the anchor took the id.
            return if step.start == *anchor.1 {
                Member
            } else {
                NotMember(Refusal::AnchorEnded)
            };
        }
        if step.in_session_of(anchor.0) {
            return Member;
        }
    }
    NotMember(match lineage.end {
        Some(LineageEnd::Top) => Refusal::NoParent,
        Some(LineageEnd::Inverted) => Refusal::Inverted,
        // Not in the design: a link that did not read the same twice is
        // taken as no reading of it.
        Some(LineageEnd::Reused | LineageEnd::Unanswered) => Refusal::Unreadable,
        None if lineage.steps.len() > MOST_STEPS => Refusal::TooDeep,
        None => Refusal::Unreadable,
    })
}

/// Windows: whether a process created at `parent_created` can be the creator
/// of one created at `child_created` (raw creation ticks). A process keeps
/// its creator's id after the creator exits, so a "creator" created after
/// the child is another process that took the id; an equal time is let
/// through.
#[must_use]
pub fn windows_link_holds(parent_created: u64, child_created: u64) -> bool {
    parent_created <= child_created
}

/// Unix: whether the parent read between two readings of the child, each
/// its `(ppid, start)`, is the child's real parent. The kernel hands a child
/// whose parent exits to another, so an unchanged ppid and start mean the
/// parent did not exit between them. Starts are compared for equality only.
#[must_use]
pub fn unix_link_holds(before: (u32, &ProcessStart), after: (u32, &ProcessStart)) -> bool {
    before.0 == after.0 && before.1 == after.1
}

/// Judges the running peer `(pid, start)` kept when its connection was
/// accepted against the anchor `(pid, start)`, reading the peer's lineage
/// and then the anchor's liveness from the operating system. The search
/// stops at the daemon `daemon_pid`. Whatever cannot be read is judged not
/// a member, never an error. Blocks on the operating system.
#[must_use]
pub fn judge_peer(
    peer: (u32, &ProcessStart),
    anchor: (u32, &ProcessStart),
    daemon_pid: u32,
) -> LineageVerdict {
    let lineage = platform::read_lineage(peer.0, anchor.0, daemon_pid);
    let anchor_now = crate::process_liveness(anchor.0, anchor.1);
    judge_lineage(&lineage, peer, anchor, anchor_now, daemon_pid)
}

#[cfg(windows)]
mod platform {
    use super::{CreatorReads, Lineage, walk_creators};
    use crate::process_identity::process_creation_and_creator;

    /// Each process read through one handle of its own.
    struct OsReads;

    impl CreatorReads for OsReads {
        fn creation_and_creator(&mut self, pid: u32) -> std::io::Result<Option<(u64, u32)>> {
            process_creation_and_creator(pid)
        }
    }

    pub(super) fn read_lineage(pid: u32, anchor_pid: u32, daemon_pid: u32) -> Lineage {
        walk_creators(&mut OsReads, pid, anchor_pid, daemon_pid)
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod platform {
    use super::{Lineage, LineageEnd, MOST_STEPS, Step, ends_search, unix_link_holds};
    use crate::process_identity::{ProcessFacts, process_facts};

    fn step_of(pid: u32, facts: &ProcessFacts) -> Step {
        Step {
            pid,
            start: facts.start.clone(),
            sid: Some(facts.sid),
        }
    }

    /// Reads the child, then its parent, then the child again: the parent
    /// read is the real one only when the child reads the same both times.
    pub(super) fn read_lineage(pid: u32, anchor_pid: u32, daemon_pid: u32) -> Lineage {
        let mut steps = Vec::new();
        let Ok(Some(mut before)) = process_facts(pid) else {
            return Lineage::new(steps, Some(LineageEnd::Unanswered));
        };
        steps.push(step_of(pid, &before));
        let mut child = pid;
        while steps.len() <= MOST_STEPS {
            if steps
                .last()
                .is_some_and(|step| ends_search(step, anchor_pid, daemon_pid))
            {
                return Lineage::new(steps, None);
            }
            // PID 1 takes in what loses its parent, so it is no parent of
            // the child's own.
            if before.ppid <= 1 {
                return Lineage::new(steps, Some(LineageEnd::Top));
            }
            let parent = match process_facts(before.ppid) {
                Ok(Some(facts)) => facts,
                Ok(None) => return Lineage::new(steps, Some(LineageEnd::Top)),
                Err(_) => return Lineage::new(steps, Some(LineageEnd::Unanswered)),
            };
            let Ok(Some(after)) = process_facts(child) else {
                return Lineage::new(steps, Some(LineageEnd::Unanswered));
            };
            if !unix_link_holds((before.ppid, &before.start), (after.ppid, &after.start)) {
                return Lineage::new(steps, Some(LineageEnd::Reused));
            }
            child = before.ppid;
            steps.push(step_of(child, &parent));
            before = parent;
        }
        Lineage::new(steps, None)
    }
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
mod platform {
    use super::{Lineage, LineageEnd};

    pub(super) fn read_lineage(_pid: u32, _anchor_pid: u32, _daemon_pid: u32) -> Lineage {
        Lineage::new(Vec::new(), Some(LineageEnd::Unanswered))
    }
}
