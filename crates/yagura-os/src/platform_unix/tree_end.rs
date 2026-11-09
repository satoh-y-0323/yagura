//! The sweep that ends a window's process tree.
//!
//! Two entries share one set of rules. The owner's entry is the tree
//! leader's unreaping parent's: it signals by plain id what its holding the
//! leader keeps from being reused, and waits, within a limit, for the
//! processes it signalled. The non-parent entry opens every process it
//! signals by its id and start first, and waits for none of them.
//!
//! A sweep sends `SIGTERM` once, gives the tree a grace period, then sends
//! `SIGKILL` round by round: each round reads the tree again and signals the
//! processes it has not signalled yet. A process read as a member once, in
//! any round or before the sweep began ([`PriorMembers`]), stays a member for
//! the whole sweep while it runs as the process read, even once the edge it
//! was read by is gone (its parent ended). It closes on a round with nothing
//! new to signal (once the owner's wait is over), or after a bounded number
//! of rounds that each did signal something new. A refused send is recorded
//! and the rest go on.
//!
//! On Linux the owner's entry keeps no such set: an orphaned member comes to
//! the owner, a subreaper, and is read as its child.

use std::collections::HashSet;
use std::io;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::ReapGate;
use crate::process_identity::{ProcessHandle, ProcessStart, process_facts};

/// The `SIGKILL` rounds that signal something new a sweep runs at most: what
/// stops a tree that keeps growing as fast as it is ended.
const KILL_ROUNDS: usize = 64;

/// What a sweep signals.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SignalTarget {
    /// One process, by its id.
    Process(u32),
    /// The process group with this id.
    Group(u32),
    /// Every process in the cgroup at this path.
    #[cfg(target_os = "linux")]
    Cgroup(std::path::PathBuf),
}

/// Where a sweep's sends pass through and what it tells of its waiting.
pub trait TreeEndHooks {
    /// Sends `signal` to `target` by calling `deliver`, which does the
    /// sending. An error is taken as the send refused: the target is
    /// recorded as unsent and the sweep goes on.
    ///
    /// # Errors
    ///
    /// Returns `deliver`'s error.
    fn send(
        &mut self,
        _target: SignalTarget,
        _signal: libc::c_int,
        deliver: &mut dyn FnMut() -> io::Result<()>,
    ) -> io::Result<()> {
        deliver()
    }

    /// The processes whose ends the owner is now watching for, told once the
    /// watches are made and before the round's `SIGKILL`s go out.
    fn watching(&mut self, _pids: &[u32]) {}

    /// A process the owner was waiting for has ended.
    fn ended(&mut self, _pid: u32) {}
}

/// What a sweep could not finish.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TreeEndRest {
    /// Processes a send to failed. A process group or cgroup whose send
    /// failed is listed by the leader's id.
    pub unsent: Vec<u32>,
    /// Processes signalled with `SIGKILL` that had not ended when the
    /// owner's wait reached its limit.
    pub unended: Vec<u32>,
    /// Processes signalled with `SIGKILL` whose end the owner could not wait
    /// for.
    pub unwaited: usize,
}

/// One process as read for a sweep: the edges its window membership is told
/// by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessRow {
    pub pid: u32,
    pub ppid: u32,
    pub sid: u32,
}

/// The processes in `rows` that belong to `leader`'s window: those in its
/// session, and the children of a member. An edge counts only while its far
/// end is still the process the rows were read from: `still_same(pid)` is
/// asked, after the reading, for the leader and for every parent an edge
/// goes through, so a child read under an id that has since passed to
/// another process is not taken for a member. With the leader no longer the
/// same, nothing is.
pub fn window_members(
    rows: &[ProcessRow],
    leader: u32,
    still_same: impl Fn(u32) -> bool,
) -> Vec<u32> {
    if !still_same(leader) {
        return Vec::new();
    }
    let mut members: Vec<u32> = rows
        .iter()
        .filter(|row| row.pid != leader && row.sid == leader)
        .map(|row| row.pid)
        .collect();
    let mut seen: HashSet<u32> = members.iter().copied().chain([leader]).collect();
    let mut parents: Vec<u32> = members.iter().copied().chain([leader]).collect();
    while let Some(parent) = parents.pop() {
        let children: Vec<u32> = rows
            .iter()
            .filter(|row| row.ppid == parent && !seen.contains(&row.pid))
            .map(|row| row.pid)
            .collect();
        if children.is_empty() || (parent != leader && !still_same(parent)) {
            continue;
        }
        for child in children {
            seen.insert(child);
            members.push(child);
            parents.push(child);
        }
    }
    members
}

/// The members of a window read before its sweep began, by id and start:
/// what [`read_prior_members`] read, handed to [`TreeEnd::prior_members`].
#[derive(Debug, Default, Clone)]
pub struct PriorMembers(Vec<Member>);

/// Reads the members of the window `leader` leads as the owner's sweep would,
/// for the sweep to keep as members ([`TreeEnd::prior_members`]): taken
/// before the window is hung up, it finds the children a member put in a
/// session of their own while their parent still runs. `leader` is a child of
/// this process that `gate` reaps it under, still unreaped.
///
/// On Linux nothing is read: a member orphaned meanwhile comes to the owner,
/// a subreaper, and is read as its child.
///
/// # Errors
///
/// Returns the error reading the processes fails with.
#[cfg(target_os = "macos")]
pub fn read_prior_members(leader: u32, gate: &ReapGate) -> io::Result<PriorMembers> {
    read_members(leader, || !gate.is_reaped()).map(PriorMembers)
}

/// Reads the members of the window `leader` leads as the owner's sweep would,
/// for the sweep to keep as members ([`TreeEnd::prior_members`]).
///
/// On Linux nothing is read: a member orphaned meanwhile comes to the owner,
/// a subreaper, and is read as its child.
///
/// # Errors
///
/// None on Linux.
#[cfg(target_os = "linux")]
pub fn read_prior_members(_leader: u32, _gate: &ReapGate) -> io::Result<PriorMembers> {
    Ok(PriorMembers::default())
}

/// The sweep of one window's tree, built from one of its two entries and
/// then [`run`](Self::run).
#[derive(Debug)]
pub struct TreeEnd {
    leader: u32,
    entry: Entry,
    prior: PriorMembers,
    slots: usize,
    rounds: usize,
    limit: Duration,
    grace: Duration,
    #[cfg(target_os = "linux")]
    cgroup: Option<std::path::PathBuf>,
}

#[derive(Debug)]
enum Entry {
    Owned(Arc<ReapGate>),
    NotParent(ProcessStart),
}

impl TreeEnd {
    /// The owner's sweep of the tree `leader` leads: the leader is a child of
    /// this process that `gate` reaps it under, unreaped until the sweep
    /// returns. On Linux this process is a subreaper, so the tree's orphans
    /// come to it, and `SIGCHLD` is blocked in every thread but the one that
    /// runs the sweep, which blocks it for the run and takes it through a
    /// `signalfd`.
    ///
    /// On macOS a member read once stays one for the whole sweep, the
    /// [`prior_members`](Self::prior_members) first; on Linux the tree's
    /// orphans are read as this process's children instead.
    #[must_use]
    pub fn owned(leader: u32, gate: Arc<ReapGate>) -> Self {
        Self::new(leader, Entry::Owned(gate))
    }

    /// The sweep, from outside the tree, of the tree whose leader is the
    /// process `leader` that started at `start`. Nothing is signalled when
    /// the leader has already ended as the sweep begins; once it began, a
    /// member read in any round stays one after the leader has ended.
    #[must_use]
    pub fn not_parent(leader: u32, start: ProcessStart) -> Self {
        Self::new(leader, Entry::NotParent(start))
    }

    fn new(leader: u32, entry: Entry) -> Self {
        Self {
            leader,
            entry,
            prior: PriorMembers::default(),
            slots: 0,
            rounds: KILL_ROUNDS,
            limit: crate::TREE_END_LIMIT,
            grace: crate::PROCESS_GROUP_TERM_GRACE,
            #[cfg(target_os = "linux")]
            cgroup: None,
        }
    }

    /// Members read before the sweep began ([`read_prior_members`]), kept as
    /// members for the whole sweep while each runs as the process read.
    #[must_use]
    pub fn prior_members(mut self, members: PriorMembers) -> Self {
        self.prior = members;
        self
    }

    /// How many descriptors the owner may hold at once to wait for the ends
    /// of signalled processes that are not its children (Linux): the
    /// caller's to size from its descriptor limit. None by default.
    #[must_use]
    pub fn exit_watch_slots(mut self, slots: usize) -> Self {
        self.slots = slots;
        self
    }

    /// The `SIGKILL` rounds that signal something new the sweep runs at most.
    #[must_use]
    pub fn kill_rounds(mut self, rounds: usize) -> Self {
        self.rounds = rounds;
        self
    }

    /// How long the owner waits for the processes it signalled, from its
    /// first `SIGKILL`: [`crate::TREE_END_LIMIT`] by default. A limit too far
    /// off to be a point in time is none.
    #[must_use]
    pub fn limit(mut self, limit: Duration) -> Self {
        self.limit = limit;
        self
    }

    /// The grace period between `SIGTERM` and `SIGKILL`:
    /// [`crate::PROCESS_GROUP_TERM_GRACE`] by default.
    #[must_use]
    pub fn grace(mut self, grace: Duration) -> Self {
        self.grace = grace;
        self
    }

    /// The cgroup that holds the tree, signalled and waited for whole by the
    /// owner.
    #[cfg(target_os = "linux")]
    #[must_use]
    pub fn cgroup(mut self, dir: std::path::PathBuf) -> Self {
        self.cgroup = Some(dir);
        self
    }

    /// Runs the sweep, every send passing through `hooks`, and returns what
    /// it could not finish.
    ///
    /// # Errors
    ///
    /// Returns the error reading the tree or setting up the owner's wait
    /// fails with. A refused send is not an error.
    pub fn run<H: TreeEndHooks + ?Sized>(self, hooks: &mut H) -> io::Result<TreeEndRest> {
        match &self.entry {
            Entry::Owned(gate) => owned::run(&self, gate, hooks),
            Entry::NotParent(start) => run_not_parent(&self, start, hooks),
        }
    }
}

/// A process the sweep has read: its id and its start.
type Member = (u32, ProcessStart);

fn note_unsent(rest: &mut TreeEndRest, pid: u32) {
    if !rest.unsent.contains(&pid) {
        rest.unsent.push(pid);
    }
}

/// How a send to one process turned out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sent {
    Out,
    /// The process had already ended.
    Gone,
    /// Recorded as unsent.
    Refused,
}

/// Opens `member` by its id and start and sends it `signal` through `hooks`.
/// One that cannot be opened or whose send fails is recorded as unsent.
fn send_identified<H: TreeEndHooks + ?Sized>(
    hooks: &mut H,
    rest: &mut TreeEndRest,
    member: &Member,
    signal: libc::c_int,
) -> Sent {
    let (pid, start) = member;
    match ProcessHandle::open(*pid, start) {
        Ok(None) => Sent::Gone,
        Ok(Some(handle)) => {
            let sent = hooks.send(SignalTarget::Process(*pid), signal, &mut || {
                handle.signal(signal)
            });
            if sent.is_ok() {
                Sent::Out
            } else {
                note_unsent(rest, *pid);
                Sent::Refused
            }
        }
        Err(_) => {
            note_unsent(rest, *pid);
            Sent::Refused
        }
    }
}

/// The ids of every process.
#[cfg(target_os = "linux")]
fn all_pids() -> io::Result<Vec<u32>> {
    let mut pids = Vec::new();
    for entry in std::fs::read_dir("/proc")? {
        if let Some(pid) = entry?
            .file_name()
            .to_str()
            .and_then(|name| name.parse().ok())
        {
            pids.push(pid);
        }
    }
    Ok(pids)
}

#[cfg(target_os = "macos")]
fn all_pids() -> io::Result<Vec<u32>> {
    super::process_group::all_pids()
}

/// Every running process that can be read, with its start.
fn read_rows() -> io::Result<Vec<(ProcessRow, ProcessStart)>> {
    let mut rows = Vec::new();
    for pid in all_pids()? {
        // A process gone or closed to this reading is no member to signal.
        let Ok(Some(facts)) = process_facts(pid) else {
            continue;
        };
        if facts.exited {
            continue;
        }
        let row = ProcessRow {
            pid,
            ppid: facts.ppid,
            sid: facts.sid,
        };
        rows.push((row, facts.start));
    }
    Ok(rows)
}

/// Whether `pid` still runs as the process that started at `start`.
fn still_runs(pid: u32, start: &ProcessStart) -> bool {
    matches!(process_facts(pid), Ok(Some(facts)) if !facts.exited && facts.start == *start)
}

/// The members of `leader`'s window other than the leader, read from every
/// process's edges; `leader_same` is asked after the reading.
fn read_members(leader: u32, leader_same: impl Fn() -> bool) -> io::Result<Vec<Member>> {
    let read = read_rows()?;
    let rows: Vec<ProcessRow> = read.iter().map(|(row, _)| *row).collect();
    let start_of = |pid: u32| read.iter().find(|(row, _)| row.pid == pid).map(|(_, s)| s);
    let members = window_members(&rows, leader, |pid| {
        if pid == leader {
            leader_same()
        } else {
            start_of(pid).is_some_and(|start| still_runs(pid, start))
        }
    });
    Ok(members
        .into_iter()
        .filter_map(|pid| start_of(pid).map(|start| (pid, start.clone())))
        .collect())
}

/// Every member a sweep has read, by id and start, for the whole sweep. The
/// set holds values only, no descriptor, so it costs no descriptor per
/// member; a member it names is sent to by its id and start, which
/// [`send_identified`] checks again as it sends.
#[derive(Debug, Default)]
struct Known(HashSet<Member>);

impl Known {
    fn from_prior(prior: &PriorMembers) -> Self {
        Self(prior.0.iter().cloned().collect())
    }

    /// `read`, and every member read before that `read` lacks and that still
    /// runs as the process read; `read` is kept for later rounds.
    fn with(&mut self, read: Vec<Member>) -> Vec<Member> {
        let read_now: HashSet<&Member> = read.iter().collect();
        let earlier: Vec<Member> = self
            .0
            .iter()
            .filter(|member| !read_now.contains(member) && still_runs(member.0, &member.1))
            .cloned()
            .collect();
        self.0.extend(read.iter().cloned());
        let mut members = read;
        members.extend(earlier);
        members
    }
}

fn run_not_parent<H: TreeEndHooks + ?Sized>(
    sweep: &TreeEnd,
    start: &ProcessStart,
    hooks: &mut H,
) -> io::Result<TreeEndRest> {
    let mut rest = TreeEndRest::default();
    let Some(leader) = ProcessHandle::open(sweep.leader, start)? else {
        return Ok(rest);
    };
    let leader_member = (sweep.leader, start.clone());
    let leader_same = || leader.liveness() == crate::Liveness::Alive;
    // A member read while the leader ran is still sent to once the leader
    // has ended: its edges are gone then, so a reading finds nothing.
    let mut known = Known::from_prior(&sweep.prior);

    let mut sent = false;
    for member in known.with(read_members(sweep.leader, leader_same)?) {
        sent |= send_identified(hooks, &mut rest, &member, libc::SIGTERM) == Sent::Out;
    }
    sent |= send_identified(hooks, &mut rest, &leader_member, libc::SIGTERM) == Sent::Out;
    if sent && !sweep.grace.is_zero() {
        std::thread::sleep(sweep.grace);
    }

    // The leader goes last: while it runs, its session and children are
    // read again each round.
    let mut killed: HashSet<Member> = HashSet::new();
    let mut rounds = 0;
    while rounds < sweep.rounds {
        let fresh: Vec<Member> = known
            .with(read_members(sweep.leader, leader_same)?)
            .into_iter()
            .filter(|member| killed.insert(member.clone()))
            .collect();
        if fresh.is_empty() {
            break;
        }
        for member in &fresh {
            send_identified(hooks, &mut rest, member, libc::SIGKILL);
        }
        rounds += 1;
    }
    send_identified(hooks, &mut rest, &leader_member, libc::SIGKILL);
    Ok(rest)
}

/// How the owner's wait turned out.
enum Waited {
    /// Some processes ended.
    Ended,
    /// Nothing is left that can be waited for.
    Nothing,
    /// The limit has passed.
    TimedOut,
}

/// The owner's side of a sweep, as each platform waits.
trait OwnerRounds {
    /// Sends `SIGTERM`; whether a send went out.
    fn term_round(&mut self) -> io::Result<bool>;
    /// Reads the tree, watches and signals with `SIGKILL` what it has not
    /// signalled yet, and returns how many new processes it signalled.
    fn kill_round(&mut self) -> io::Result<usize>;
    fn wait(&mut self, deadline: Option<Instant>) -> io::Result<Waited>;
    /// Records what the owner still waits for as not ended.
    fn leave_pending_unended(&mut self);
    /// Records what is left pending as not waited for.
    fn leave_pending_unwaited(&mut self);
}

/// The owner's rounds: after `SIGTERM` and the grace period, signals until
/// a round finds nothing new, waits for an end and reads again; closes once
/// nothing is left to wait for, at the limit, or after `rounds` rounds that
/// signalled something new.
fn run_owner(owner: &mut impl OwnerRounds, sweep: &TreeEnd) -> io::Result<()> {
    if owner.term_round()? && !sweep.grace.is_zero() {
        std::thread::sleep(sweep.grace);
    }
    let deadline = Instant::now().checked_add(sweep.limit);
    let mut rounds = 0;
    loop {
        if owner.kill_round()? > 0 {
            rounds += 1;
            if rounds >= sweep.rounds {
                owner.leave_pending_unwaited();
                return Ok(());
            }
            continue;
        }
        match owner.wait(deadline)? {
            Waited::Ended => {}
            Waited::Nothing => {
                owner.leave_pending_unwaited();
                return Ok(());
            }
            Waited::TimedOut => {
                owner.leave_pending_unended();
                owner.kill_round()?;
                owner.leave_pending_unwaited();
                return Ok(());
            }
        }
    }
}

/// The leader's process group id, unless signalling it would reach this
/// process itself.
fn leader_group(leader: u32) -> Option<libc::pid_t> {
    // SAFETY: getpgrp takes no arguments and only reads this process's group.
    let own = unsafe { libc::getpgrp() };
    libc::pid_t::try_from(leader)
        .ok()
        .filter(|&pgid| pgid > 0 && pgid != own)
}

fn group_error(error: crate::OsError) -> io::Error {
    io::Error::other(error)
}

#[cfg(target_os = "linux")]
mod owned {
    use std::collections::HashSet;
    use std::io;
    use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
    use std::path::{Path, PathBuf};
    use std::time::Instant;

    use super::{
        Member, SignalTarget, TreeEnd, TreeEndHooks, TreeEndRest, Waited, group_error,
        leader_group, note_unsent,
    };
    use crate::ReapGate;
    use crate::platform_unix::exit_watch::{
        child_has_exited, reap_orphans, signal_child, with_orphans,
    };
    use crate::platform_unix::process_group::send_signal;
    use crate::platform_unix::user_manager::{kill_cgroup, pidfd_open};
    use crate::process_identity::{process_facts, process_start};

    /// `deadline`'s distance as a poll timeout: `-1` for none, rounded up to
    /// whole milliseconds; `None` once it has passed.
    fn poll_timeout(deadline: Option<Instant>) -> Option<libc::c_int> {
        let Some(deadline) = deadline else {
            return Some(-1);
        };
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return None;
        }
        let millis = left.as_nanos().div_ceil(1_000_000);
        Some(libc::c_int::try_from(millis).unwrap_or(libc::c_int::MAX))
    }

    /// How the end of a signalled process is learnt.
    enum Watch {
        /// A child of this process: from `SIGCHLD`.
        Child,
        /// From its pidfd.
        Slot(OwnedFd),
        /// Not yet: no slot was free or could be opened.
        Unwatched,
    }

    struct Pending {
        member: Member,
        watch: Watch,
    }

    /// `SIGCHLD` blocked in this thread for as long as it lives.
    struct BlockedChildSignal {
        previous: libc::sigset_t,
    }

    impl BlockedChildSignal {
        fn new() -> io::Result<(Self, OwnedFd)> {
            // SAFETY: zeroed is a valid `sigset_t`, set below through the
            // libc calls.
            let mut set: libc::sigset_t = unsafe { std::mem::zeroed() };
            // SAFETY: as above.
            let mut previous: libc::sigset_t = unsafe { std::mem::zeroed() };
            // SAFETY: `set` and `previous` are live locals the calls write.
            let blocked = unsafe {
                libc::sigemptyset(&mut set);
                libc::sigaddset(&mut set, libc::SIGCHLD);
                libc::pthread_sigmask(libc::SIG_BLOCK, &set, &mut previous)
            };
            if blocked != 0 {
                return Err(io::Error::from_raw_os_error(blocked));
            }
            let guard = Self { previous };
            // SAFETY: `set` is a live local; -1 asks for a new descriptor.
            let fd = unsafe { libc::signalfd(-1, &set, libc::SFD_NONBLOCK | libc::SFD_CLOEXEC) };
            if fd == -1 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: the descriptor was just returned to this function alone.
            Ok((guard, unsafe { OwnedFd::from_raw_fd(fd) }))
        }
    }

    impl Drop for BlockedChildSignal {
        fn drop(&mut self) {
            // SAFETY: `previous` is the mask this thread had before.
            unsafe {
                libc::pthread_sigmask(libc::SIG_SETMASK, &self.previous, std::ptr::null_mut());
            }
        }
    }

    /// Reads every signal queued on `signals`, so it polls readable again
    /// only for a new one.
    fn drain(signals: &OwnedFd) {
        let mut buffer = [0u8; 8 * std::mem::size_of::<libc::signalfd_siginfo>()];
        loop {
            // SAFETY: `buffer` is a live local of the length passed.
            let read = unsafe {
                libc::read(
                    signals.as_raw_fd(),
                    buffer.as_mut_ptr().cast(),
                    buffer.len(),
                )
            };
            if read <= 0 {
                let interrupted =
                    read == -1 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted;
                if !interrupted {
                    return;
                }
            }
        }
    }

    /// The `populated` field of a cgroup's `cgroup.events`, which polls with
    /// `POLLPRI` each time it changes.
    struct CgroupEvents {
        dir: PathBuf,
        file: std::fs::File,
    }

    impl CgroupEvents {
        fn open(dir: &Path) -> io::Result<Self> {
            Ok(Self {
                dir: dir.to_owned(),
                file: std::fs::File::open(dir.join("cgroup.events"))?,
            })
        }

        fn populated(&self) -> io::Result<bool> {
            let mut buffer = [0u8; 256];
            let read = std::os::unix::fs::FileExt::read_at(&self.file, &mut buffer, 0)?;
            let text = String::from_utf8_lossy(&buffer[..read]);
            Ok(!text.lines().any(|line| line.trim() == "populated 0"))
        }

        /// The processes the cgroup holds now.
        fn pids(&self) -> Vec<u32> {
            std::fs::read_to_string(self.dir.join("cgroup.procs"))
                .map(|procs| procs.lines().filter_map(|pid| pid.parse().ok()).collect())
                .unwrap_or_default()
        }
    }

    struct Owner<'a, H: TreeEndHooks + ?Sized> {
        leader: u32,
        gate: &'a ReapGate,
        hooks: &'a mut H,
        cgroup: Option<&'a Path>,
        rest: TreeEndRest,
        free_slots: usize,
        termed: HashSet<Member>,
        killed: HashSet<Member>,
        pending: Vec<Pending>,
        leader_signalled: bool,
        cgroup_events: Option<CgroupEvents>,
        signals: OwnedFd,
    }

    pub(super) fn run<H: TreeEndHooks + ?Sized>(
        sweep: &TreeEnd,
        gate: &ReapGate,
        hooks: &mut H,
    ) -> io::Result<TreeEndRest> {
        let (_blocked, signals) = BlockedChildSignal::new()?;
        let mut owner = Owner {
            leader: sweep.leader,
            gate,
            hooks,
            cgroup: sweep.cgroup.as_deref(),
            rest: TreeEndRest::default(),
            free_slots: sweep.slots,
            termed: HashSet::new(),
            killed: HashSet::new(),
            pending: Vec::new(),
            leader_signalled: false,
            cgroup_events: None,
            signals,
        };
        super::run_owner(&mut owner, sweep)?;
        Ok(owner.rest)
    }

    impl<H: TreeEndHooks + ?Sized> super::OwnerRounds for Owner<'_, H> {
        /// Whether a send went out.
        fn term_round(&mut self) -> io::Result<bool> {
            let leader = self.leader;
            let gate = self.gate;
            let leader_ended = self.leader_ended();
            let Self {
                hooks,
                rest,
                termed,
                ..
            } = self;
            with_orphans(leader, gate, |orphans, leader_reaped| {
                let mut sent = false;
                for &pid in orphans {
                    let Some(start) = process_start(pid)? else {
                        continue;
                    };
                    if termed.insert((pid, start)) {
                        sent |= send_child(*hooks, rest, pid, libc::SIGTERM);
                    }
                }
                if !leader_reaped {
                    sent |= send_group(*hooks, rest, leader, libc::SIGTERM);
                    if !leader_ended {
                        sent |= send_child(*hooks, rest, leader, libc::SIGTERM);
                    }
                }
                Ok(sent)
            })
        }

        /// Reads the tree, watches and signals with `SIGKILL` what it has not
        /// signalled yet, and returns how many new processes it signalled.
        fn kill_round(&mut self) -> io::Result<usize> {
            let leader = self.leader;
            let fresh_group = if self.gate.is_reaped() {
                Vec::new()
            } else {
                self.group_members()?
                    .into_iter()
                    .filter(|member| self.killed.insert(member.clone()))
                    .collect()
            };
            let mut fresh = fresh_group.len();
            self.pending
                .extend(fresh_group.into_iter().map(|member| Pending {
                    member,
                    watch: Watch::Unwatched,
                }));
            self.fill_slots();
            if self.cgroup_events.is_none()
                && let Some(dir) = self.cgroup
            {
                // A cgroup gone or empty already has nothing to wait for.
                self.cgroup_events = CgroupEvents::open(dir)
                    .ok()
                    .filter(|events| events.populated().unwrap_or(false));
            }
            let leader_ended = self.leader_ended();
            let gate = self.gate;

            let Self {
                hooks,
                rest,
                killed,
                pending,
                leader_signalled,
                cgroup,
                ..
            } = self;
            with_orphans(leader, gate, |orphans, leader_reaped| {
                let mut children = Vec::new();
                for &pid in orphans {
                    let Some(start) = process_start(pid)? else {
                        continue;
                    };
                    let member = (pid, start);
                    if killed.insert(member.clone()) {
                        children.push(pid);
                        pending.push(Pending {
                            member,
                            watch: Watch::Child,
                        });
                    }
                }
                fresh += children.len();
                let signal_leader = !leader_reaped && !*leader_signalled;
                if signal_leader
                    && !leader_ended
                    && let Ok(Some(start)) = process_start(leader)
                {
                    pending.push(Pending {
                        member: (leader, start),
                        watch: Watch::Child,
                    });
                }
                let watched: Vec<u32> = pending.iter().map(|p| p.member.0).collect();
                hooks.watching(&watched);

                for pid in children {
                    if !send_child(*hooks, rest, pid, libc::SIGKILL) {
                        pending.retain(|p| p.member.0 != pid);
                    }
                }
                if !leader_reaped {
                    send_group(*hooks, rest, leader, libc::SIGKILL);
                }
                if let Some(dir) = cgroup {
                    let target = SignalTarget::Cgroup(dir.to_path_buf());
                    let sent =
                        hooks.send(target, libc::SIGKILL, &mut || kill_cgroup(dir).map(drop));
                    if sent.is_err() {
                        note_unsent(rest, leader);
                    }
                }
                if signal_leader {
                    *leader_signalled = true;
                    if !send_child(*hooks, rest, leader, libc::SIGKILL) {
                        pending.retain(|p| p.member.0 != leader);
                    }
                }
                Ok(fresh)
            })
        }

        /// Takes in every end that has come, then, when none has, waits for
        /// one until `deadline`.
        fn wait(&mut self, deadline: Option<Instant>) -> io::Result<Waited> {
            loop {
                if self.take_ends()? {
                    return Ok(Waited::Ended);
                }
                let waitable = self
                    .pending
                    .iter()
                    .any(|p| !matches!(p.watch, Watch::Unwatched));
                if !waitable && self.cgroup_events.is_none() {
                    return Ok(Waited::Nothing);
                }
                let Some(timeout) = poll_timeout(deadline) else {
                    return Ok(Waited::TimedOut);
                };
                let mut fds = vec![libc::pollfd {
                    fd: self.signals.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                }];
                for pending in &self.pending {
                    if let Watch::Slot(fd) = &pending.watch {
                        fds.push(libc::pollfd {
                            fd: fd.as_raw_fd(),
                            events: libc::POLLIN,
                            revents: 0,
                        });
                    }
                }
                if let Some(events) = &self.cgroup_events {
                    fds.push(libc::pollfd {
                        fd: events.file.as_raw_fd(),
                        events: libc::POLLPRI,
                        revents: 0,
                    });
                }
                let count = libc::nfds_t::try_from(fds.len())
                    .map_err(|_| io::Error::other("too many descriptors to poll"))?;
                // SAFETY: `fds` is a live vector of `count` entries.
                if unsafe { libc::poll(fds.as_mut_ptr(), count, timeout) } == -1 {
                    let error = io::Error::last_os_error();
                    if error.kind() != io::ErrorKind::Interrupted {
                        return Err(error);
                    }
                }
            }
        }

        /// Records what the owner still waits for as not ended.
        fn leave_pending_unended(&mut self) {
            for pending in self.pending.drain(..) {
                if matches!(pending.watch, Watch::Unwatched) {
                    self.rest.unwaited += 1;
                } else if !self.rest.unended.contains(&pending.member.0) {
                    self.rest.unended.push(pending.member.0);
                }
            }
            if let Some(events) = self.cgroup_events.take() {
                for pid in events.pids() {
                    if !self.rest.unended.contains(&pid) {
                        self.rest.unended.push(pid);
                    }
                }
            }
        }

        /// Records what is left pending as not waited for.
        fn leave_pending_unwaited(&mut self) {
            self.rest.unwaited += self.pending.len();
            self.pending.clear();
            self.cgroup_events = None;
        }
    }

    impl<H: TreeEndHooks + ?Sized> Owner<'_, H> {
        fn leader_ended(&self) -> bool {
            // A leader that is no longer this process's child is not one it
            // can signal or wait for.
            child_has_exited(self.leader).unwrap_or(true)
        }

        /// The running members of the leader's group that are not this
        /// process's children: those are read as children.
        fn group_members(&self) -> io::Result<Vec<Member>> {
            let own = std::process::id();
            let mut members = Vec::new();
            for pid in super::all_pids()? {
                if pid == self.leader {
                    continue;
                }
                let Ok(Some(facts)) = process_facts(pid) else {
                    continue;
                };
                if facts.pgid == self.leader && facts.ppid != own && !facts.exited {
                    members.push((pid, facts.start));
                }
            }
            Ok(members)
        }

        /// Opens pidfds for unwatched members while slots are free. A pidfd
        /// is kept only if, once opened, its process is still the member
        /// read and still in the leader's group or session.
        fn fill_slots(&mut self) {
            let mut index = 0;
            while index < self.pending.len() && self.free_slots > 0 {
                if !matches!(self.pending[index].watch, Watch::Unwatched) {
                    index += 1;
                    continue;
                }
                let (pid, start) = self.pending[index].member.clone();
                let fd = match pidfd_open(pid) {
                    Ok(fd) => fd,
                    Err(error) if error.raw_os_error() == Some(libc::ESRCH) => {
                        self.end(index);
                        continue;
                    }
                    // Out of descriptors: the rest wait for a later round.
                    Err(_) => return,
                };
                match process_facts(pid) {
                    Ok(Some(facts)) if facts.start == start && !facts.exited => {
                        if facts.pgid == self.leader || facts.sid == self.leader {
                            self.pending[index].watch = Watch::Slot(fd);
                            self.free_slots -= 1;
                            index += 1;
                        } else {
                            self.pending.remove(index);
                            self.rest.unwaited += 1;
                        }
                    }
                    // Exited, or its id now names another process.
                    _ => self.end(index),
                }
            }
        }

        fn end(&mut self, index: usize) {
            let ended = self.pending.remove(index);
            if matches!(ended.watch, Watch::Slot(_)) {
                self.free_slots += 1;
            }
            self.hooks.ended(ended.member.0);
        }

        /// Takes in every end that has come without waiting; whether any had.
        fn take_ends(&mut self) -> io::Result<bool> {
            drain(&self.signals);
            let mut ended = false;
            let reaped = reap_orphans(self.leader, self.gate)?;
            let leader_ended = self.leader_ended();
            let mut index = 0;
            while index < self.pending.len() {
                let pid = self.pending[index].member.0;
                let done = match &self.pending[index].watch {
                    Watch::Child if pid == self.leader => leader_ended,
                    Watch::Child => reaped.contains(&pid),
                    Watch::Slot(fd) => readable(fd),
                    Watch::Unwatched => false,
                };
                if done {
                    self.end(index);
                    ended = true;
                } else {
                    index += 1;
                }
            }
            if let Some(events) = &self.cgroup_events
                && !events.populated()?
            {
                self.cgroup_events = None;
                ended = true;
            }
            Ok(ended)
        }
    }

    fn readable(fd: &OwnedFd) -> bool {
        let mut entry = libc::pollfd {
            fd: fd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: `entry` is a live stack local, the one entry passed.
        unsafe { libc::poll(&mut entry, 1, 0) > 0 }
    }

    /// Sends `signal` to this process's unreaped child `pid` through
    /// `hooks`; whether it went out.
    fn send_child<H: TreeEndHooks + ?Sized>(
        hooks: &mut H,
        rest: &mut TreeEndRest,
        pid: u32,
        signal: libc::c_int,
    ) -> bool {
        let sent = hooks.send(SignalTarget::Process(pid), signal, &mut || {
            signal_child(pid, signal)
        });
        if sent.is_err() {
            note_unsent(rest, pid);
        }
        sent.is_ok()
    }

    /// Sends `signal` to the leader's group through `hooks`, while the
    /// leader is unreaped; whether it went out.
    fn send_group<H: TreeEndHooks + ?Sized>(
        hooks: &mut H,
        rest: &mut TreeEndRest,
        leader: u32,
        signal: libc::c_int,
    ) -> bool {
        let Some(pgid) = leader_group(leader) else {
            return false;
        };
        let sent = hooks.send(SignalTarget::Group(leader), signal, &mut || {
            send_signal(pgid, signal).map(drop).map_err(group_error)
        });
        if sent.is_err() {
            note_unsent(rest, leader);
        }
        sent.is_ok()
    }
}

#[cfg(target_os = "macos")]
mod owned {
    use std::collections::HashSet;
    use std::io;
    use std::time::Instant;

    use super::{
        Known, Member, Sent, SignalTarget, TreeEnd, TreeEndHooks, TreeEndRest, Waited, group_error,
        leader_group, note_unsent, read_members, send_identified,
    };
    use crate::ReapGate;
    use crate::platform_unix::exit_watch::{ExitQueue, has_exited, signal_child};
    use crate::platform_unix::process_group::send_signal;
    use crate::process_identity::process_facts;

    struct Owner<'a, H: TreeEndHooks + ?Sized> {
        leader: u32,
        gate: &'a ReapGate,
        hooks: &'a mut H,
        rest: TreeEndRest,
        queue: ExitQueue,
        killed: HashSet<Member>,
        /// Every member read, the prior ones first: one whose parent ended
        /// went to launchd, and no edge leads back to it from the window.
        known: Known,
        /// Signalled processes not known to have ended, and whether their
        /// exits are watched.
        pending: Vec<(u32, bool)>,
        leader_signalled: bool,
    }

    pub(super) fn run<H: TreeEndHooks + ?Sized>(
        sweep: &TreeEnd,
        gate: &ReapGate,
        hooks: &mut H,
    ) -> io::Result<TreeEndRest> {
        let mut owner = Owner {
            leader: sweep.leader,
            gate,
            hooks,
            rest: TreeEndRest::default(),
            queue: ExitQueue::new()?,
            killed: HashSet::new(),
            known: Known::from_prior(&sweep.prior),
            pending: Vec::new(),
            leader_signalled: false,
        };
        super::run_owner(&mut owner, sweep)?;
        Ok(owner.rest)
    }

    impl<H: TreeEndHooks + ?Sized> super::OwnerRounds for Owner<'_, H> {
        /// Whether a send went out.
        fn term_round(&mut self) -> io::Result<bool> {
            let mut sent = false;
            for member in self.members()? {
                sent |= send_identified(self.hooks, &mut self.rest, &member, libc::SIGTERM)
                    == Sent::Out;
            }
            sent |= self.send_to_leader(libc::SIGTERM, true);
            Ok(sent)
        }

        /// Reads the tree, watches and signals with `SIGKILL` what it has not
        /// signalled yet, and returns how many new processes it signalled.
        fn kill_round(&mut self) -> io::Result<usize> {
            let fresh: Vec<Member> = self
                .members()?
                .into_iter()
                .filter(|member| self.killed.insert(member.clone()))
                .collect();
            let mut to_send = Vec::new();
            for (pid, start) in &fresh {
                match self.queue.watch(*pid) {
                    // A watch made on an id that has since passed to another
                    // process is not the member's.
                    Ok(true) => {
                        if matches!(process_facts(*pid), Ok(Some(f)) if f.start == *start) {
                            self.pending.push((*pid, true));
                            to_send.push((*pid, start.clone()));
                        }
                    }
                    Ok(false) => {}
                    Err(_) => {
                        self.pending.push((*pid, false));
                        to_send.push((*pid, start.clone()));
                    }
                }
            }
            let signal_leader = !self.leader_signalled && !self.gate.is_reaped();
            if signal_leader && !self.leader_ended() {
                let watched = self.queue.watch(self.leader).unwrap_or(false);
                if watched {
                    self.pending.push((self.leader, true));
                }
            }
            let watched: Vec<u32> = self.pending.iter().map(|&(pid, _)| pid).collect();
            self.hooks.watching(&watched);

            for member in &to_send {
                // A member that ended meanwhile is still watched: its exit
                // is reported.
                let sent = send_identified(self.hooks, &mut self.rest, member, libc::SIGKILL);
                if sent == Sent::Refused {
                    self.pending.retain(|&(pid, _)| pid != member.0);
                }
            }
            if signal_leader {
                self.leader_signalled = true;
            }
            self.send_to_leader(libc::SIGKILL, signal_leader);
            Ok(fresh.len())
        }

        /// Waits for an end until `deadline`, then takes in every end that
        /// has come.
        fn wait(&mut self, deadline: Option<Instant>) -> io::Result<Waited> {
            loop {
                if !self.pending.iter().any(|&(_, watched)| watched) {
                    return Ok(Waited::Nothing);
                }
                let Some(pid) = self.queue.next_exit(deadline)? else {
                    return Ok(Waited::TimedOut);
                };
                let mut ended = self.end(pid);
                while let Some(pid) = self.queue.reported_exit()? {
                    ended |= self.end(pid);
                }
                if ended {
                    return Ok(Waited::Ended);
                }
            }
        }

        /// Records what the owner still waits for as not ended.
        fn leave_pending_unended(&mut self) {
            for (pid, watched) in self.pending.drain(..) {
                if !watched {
                    self.rest.unwaited += 1;
                } else if !self.rest.unended.contains(&pid) {
                    self.rest.unended.push(pid);
                }
            }
        }

        /// Records what is left pending as not waited for.
        fn leave_pending_unwaited(&mut self) {
            self.rest.unwaited += self.pending.len();
            self.pending.clear();
        }
    }

    impl<H: TreeEndHooks + ?Sized> Owner<'_, H> {
        fn leader_ended(&self) -> bool {
            libc::pid_t::try_from(self.leader).map_or(true, has_exited)
        }

        /// The members other than the leader, read through the edges the
        /// leader's being held unreaped keeps from being reused, and those
        /// read before that still run.
        fn members(&mut self) -> io::Result<Vec<Member>> {
            let gate = self.gate;
            let read = read_members(self.leader, || !gate.is_reaped())?;
            Ok(self.known.with(read))
        }

        /// Sends `signal` to the leader's group and then, with `alone`, to the
        /// leader, while it is unreaped; whether a send went out.
        fn send_to_leader(&mut self, signal: libc::c_int, alone: bool) -> bool {
            let leader = self.leader;
            let gate = self.gate;
            let alone = alone && !self.leader_ended();
            let Self { hooks, rest, .. } = self;
            gate.signal_unless_reaped(|| {
                let mut sent = false;
                if let Some(pgid) = leader_group(leader) {
                    let group = hooks.send(SignalTarget::Group(leader), signal, &mut || {
                        send_signal(pgid, signal).map(drop).map_err(group_error)
                    });
                    if group.is_err() {
                        note_unsent(rest, leader);
                    }
                    sent |= group.is_ok();
                }
                if alone {
                    let to_leader = hooks.send(SignalTarget::Process(leader), signal, &mut || {
                        signal_child(leader, signal)
                    });
                    if to_leader.is_err() {
                        note_unsent(rest, leader);
                    }
                    sent |= to_leader.is_ok();
                }
                sent
            })
            .unwrap_or(false)
        }

        fn end(&mut self, pid: u32) -> bool {
            let before = self.pending.len();
            self.pending.retain(|&(member, _)| member != pid);
            let ended = self.pending.len() < before;
            if ended {
                self.hooks.ended(pid);
            }
            ended
        }
    }
}
