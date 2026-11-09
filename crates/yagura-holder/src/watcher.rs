//! A window's watcher (Unix): the process a holder starts to start the
//! window's child on the terminal's slave and to end the child's tree.
//!
//! The watcher is the child's parent and keeps it unreaped until its sweep is
//! over, so the child's id keeps naming its process group and session; on
//! Linux it is the tree's subreaper too. It holds a copy of the terminal's
//! master, so the terminal does not hang up when the holder ends before the
//! watcher has read the tree.
//!
//! It ends the tree once, on the first of four events: the holder's
//! `Terminate`, the end of the holder's socket (the holder died), `SIGTERM`,
//! or the child's own exit. Whatever the event, it reads the tree's members
//! first, then hangs the terminal up (a `SIGHUP` to the child when the holder
//! asked, and closing its copy of the master), gives the child a grace period
//! when it hung the terminal up, and sweeps with the members it read. It then
//! tells the holder the sweep is over and ends, removing the holder's files
//! first when the holder is gone; it holds the endpoint's lock for as long as
//! it lives.
//!
//! It runs on one thread, starts no async runtime and reads the frames of
//! [`crate::watcher_proto`] alone.

use std::collections::VecDeque;
use std::io;
use std::os::fd::{AsFd as _, BorrowedFd, OwnedFd};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};

use yagura_os::ReapGate;
use yagura_os::platform_unix as os;

#[cfg(target_os = "linux")]
use crate::watcher_proto::MOST_FDS_PER_SCOPE_REQUEST;
use crate::watcher_proto::{
    WATCHER_PROTOCOL, WatcherFrame, decode_watcher_frame, encode_watcher_frame,
};

/// How long a child asked to end by a hangup has before its tree is swept:
/// the time a shell gets to run its hangup trap.
const HANGUP_GRACE: Duration = Duration::from_millis(250);

/// The descriptors the watcher holds while it sweeps, which the sweep's
/// waits may not use: its three standard streams, the holder's socket, the
/// endpoint's lock, and its wait's two (Linux: the leader's pidfd and a
/// signalfd) and the sweep's two (a signalfd and `cgroup.events`). The copy
/// of the terminal's master is not one: it is closed before the sweep.
const ALWAYS_HELD: usize = 9;

/// Descriptors kept aside for the reads a sweep makes as it goes.
const DESCRIPTOR_MARGIN: usize = 32;

/// `ScopeReady::outside_members` when the members outside could not be
/// counted.
pub(crate) const UNCOUNTED: u32 = u32::MAX;

/// The descriptors one read of the socket took, and the bytes of the stream
/// that read covered: they came with the first byte of the frame that starts
/// among those bytes and carries descriptors.
struct Batch {
    start: usize,
    end: usize,
    fds: Vec<OwnedFd>,
}

/// The frames on one end of a holder's socket, with the descriptors that
/// came beside them.
pub(crate) struct FrameStream {
    socket: Arc<OwnedFd>,
    buffer: Vec<u8>,
    /// Where in the stream the first byte of `buffer` is.
    taken: usize,
    batches: VecDeque<Batch>,
}

fn wrong_descriptors() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "a frame came with other descriptors than it carries",
    )
}

impl FrameStream {
    pub(crate) fn new(socket: Arc<OwnedFd>) -> Self {
        Self {
            socket,
            buffer: Vec::new(),
            taken: 0,
            batches: VecDeque::new(),
        }
    }

    /// Reads what the socket has, waiting for some; `false` at its end.
    ///
    /// # Errors
    ///
    /// The error reading the socket fails with.
    pub(crate) fn fill(&mut self) -> io::Result<bool> {
        // A small stack array: a heap one zeroed per read leaves dirty pages
        // behind in a process that waits most of its life.
        let mut chunk = [0u8; 4096];
        let mut fds = Vec::new();
        let read = os::receive_with_descriptors(self.socket.as_fd(), &mut chunk, &mut fds)?;
        let start = self.taken.saturating_add(self.buffer.len());
        if !fds.is_empty() {
            self.batches.push_back(Batch {
                start,
                end: start.saturating_add(read),
                fds,
            });
        }
        self.buffer.extend_from_slice(&chunk[..read]);
        Ok(read > 0)
    }

    /// The next frame already read, with the descriptors it carries.
    ///
    /// # Errors
    ///
    /// [`io::ErrorKind::InvalidData`] for bytes that are not a frame, or a
    /// frame that came with more or fewer descriptors than it carries; the
    /// descriptors that came with it are closed.
    pub(crate) fn take(&mut self) -> io::Result<Option<(WatcherFrame, Vec<OwnedFd>)>> {
        let Some((frame, used)) = decode_watcher_frame(&self.buffer)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?
        else {
            return Ok(None);
        };
        let begin = self.taken;
        self.buffer.drain(..used);
        self.taken = self.taken.saturating_add(used);
        if self.buffer.is_empty() {
            // Gives back the room the largest frame so far took.
            self.buffer = Vec::new();
        }
        let fds = self.descriptors_of(begin, frame.descriptors())?;
        Ok(Some((frame, fds)))
    }

    /// The descriptors of the frame whose first byte is at `begin`, exactly
    /// `carried` of them. Descriptors no frame can carry any more (every
    /// frame that starts among the bytes they came with has been taken) are
    /// closed, and the frame is refused with them.
    fn descriptors_of(&mut self, begin: usize, carried: usize) -> io::Result<Vec<OwnedFd>> {
        let mut fds = Vec::new();
        if carried > 0
            && self
                .batches
                .front()
                .is_some_and(|batch| batch.start <= begin && begin < batch.end)
            && let Some(batch) = self.batches.pop_front()
        {
            fds = batch.fds;
        }
        let mut unclaimed = false;
        while self
            .batches
            .front()
            .is_some_and(|batch| batch.end <= self.taken)
        {
            self.batches.pop_front();
            unclaimed = true;
        }
        if unclaimed || fds.len() != carried {
            return Err(wrong_descriptors());
        }
        Ok(fds)
    }

    /// The next frame, waiting for it; `None` at the socket's end.
    ///
    /// # Errors
    ///
    /// As [`Self::fill`] and [`Self::take`].
    pub(crate) fn next(&mut self) -> io::Result<Option<(WatcherFrame, Vec<OwnedFd>)>> {
        loop {
            if let Some(frame) = self.take()? {
                return Ok(Some(frame));
            }
            if !self.fill()? {
                return Ok(None);
            }
        }
    }

    /// Sends `frame` with `fds` beside it.
    ///
    /// # Errors
    ///
    /// The error writing the socket fails with, such as once the peer is gone.
    pub(crate) fn send(&self, frame: &WatcherFrame, fds: &[BorrowedFd<'_>]) -> io::Result<()> {
        send_frame(self.socket.as_fd(), frame, fds)
    }

    /// The socket the frames come on.
    pub(crate) fn socket(&self) -> BorrowedFd<'_> {
        self.socket.as_fd()
    }
}

/// Sends `frame` on `socket` with `fds` beside it.
///
/// # Errors
///
/// The error writing the socket fails with, such as once the peer is gone.
pub(crate) fn send_frame(
    socket: BorrowedFd<'_>,
    frame: &WatcherFrame,
    fds: &[BorrowedFd<'_>],
) -> io::Result<()> {
    os::send_with_descriptors(socket, &encode_watcher_frame(frame), fds)
}

/// Runs the watcher as its holder started it: `yagura-watcher <fd>`, the
/// holder's socket as the descriptor `fd`. Ends with `0` once its sweep is
/// over, and with `2` when it could not start the window's child.
#[must_use]
pub fn run() -> ExitCode {
    match start() {
        Ok(watcher) => {
            watcher.watch();
            ExitCode::SUCCESS
        }
        Err(_) => ExitCode::from(2),
    }
}

/// The holder's files, removed by the watcher once the holder is gone.
struct HolderFiles {
    record: PathBuf,
    socket: PathBuf,
    lock: PathBuf,
}

/// What started the sweep.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cause {
    /// The holder asked: the child is hung up on first.
    Terminate,
    /// The holder's socket ended or broke.
    HolderGone,
    /// `SIGTERM`, or frames the watcher cannot read.
    Ended,
    /// The child exited.
    LeaderExited,
}

struct Watcher {
    stream: FrameStream,
    leader: u32,
    gate: Arc<ReapGate>,
    files: HolderFiles,
    /// Holds the endpoint's lock for as long as the watcher lives.
    _lock: OwnedFd,
    /// The copy of the terminal's master, neither read nor written, held
    /// until the tree is read: closing it hangs the terminal up once the
    /// holder is gone.
    master: Option<OwnedFd>,
    slots: usize,
    leader_told: bool,
    holder_gone: bool,
    #[cfg(target_os = "linux")]
    cgroup: Option<PathBuf>,
}

/// Says hello, takes the child to start and starts it.
fn start() -> io::Result<Watcher> {
    os::block_watcher_signals()?;
    let fd = std::env::args()
        .nth(1)
        .and_then(|fd| fd.parse().ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "usage: yagura-watcher <fd>"))?;
    let socket = Arc::new(os::take_inherited_socket(fd)?);
    #[cfg(target_os = "linux")]
    os::set_child_subreaper()?;
    let mut stream = FrameStream::new(socket);
    stream.send(
        &WatcherFrame::Hello {
            protocol: WATCHER_PROTOCOL,
        },
        &[],
    )?;
    let Some((
        WatcherFrame::Spawn {
            argv,
            cwd,
            env,
            record_path,
            socket_path,
            lock_path,
        },
        fds,
    )) = stream.next()?
    else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the holder sent no child to start",
        ));
    };
    let [slave, lock, master] = <[OwnedFd; 3]>::try_from(fds).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "the child came without its terminal, lock and master",
        )
    })?;
    let command = yagura_os::pty::LeaderCommand { argv, cwd, env };
    let leader = yagura_os::pty::start_leader(&command, slave)?;
    let start = yagura_os::process_start(leader)
        .ok()
        .flatten()
        .map(|start| start.to_string());
    stream.send(&WatcherFrame::LeaderStarted { pid: leader, start }, &[])?;
    // Raised only now: the child keeps the limit it started with.
    let soft = os::raise_descriptor_limit().unwrap_or(0);
    let slots = usize::try_from(soft)
        .unwrap_or(usize::MAX)
        .saturating_sub(ALWAYS_HELD + DESCRIPTOR_MARGIN);
    Ok(Watcher {
        stream,
        leader,
        gate: Arc::new(ReapGate::new()),
        files: HolderFiles {
            record: PathBuf::from(record_path),
            socket: PathBuf::from(socket_path),
            lock: PathBuf::from(lock_path),
        },
        _lock: lock,
        master: Some(master),
        slots,
        leader_told: false,
        holder_gone: false,
        #[cfg(target_os = "linux")]
        cgroup: None,
    })
}

/// Sends nothing but lets every send through.
struct Plain;

impl os::TreeEndHooks for Plain {}

impl Watcher {
    /// Waits for the first event; reads the tree, hangs the terminal up,
    /// gives the child its grace and sweeps; tells the holder and, with the
    /// holder gone, removes its files.
    fn watch(mut self) {
        // Without its wait the watcher cannot learn of an event: it sweeps now.
        let mut wait = os::WatcherWait::new(self.leader, self.stream.socket()).ok();
        let cause = match wait.as_mut() {
            Some(wait) => self.wait_for_cause(wait),
            None => Cause::Ended,
        };
        // Read before the hangup can end a member's parent: a child the
        // parent put in a session of its own is found only while it runs.
        let prior = os::read_prior_members(self.leader, &self.gate).unwrap_or_default();
        if self.hang_up(cause)
            && let Some(wait) = wait.as_mut()
        {
            self.grace(wait);
        }
        drop(wait);
        let rest = self.sweep(prior);
        self.finish(&rest);
    }

    fn wait_for_cause(&mut self, wait: &mut os::WatcherWait) -> Cause {
        loop {
            let Ok(events) = wait.wait(None) else {
                return Cause::Ended;
            };
            if events.leader_exited {
                self.tell_leader_exit();
                return Cause::LeaderExited;
            }
            #[cfg(target_os = "linux")]
            if events.children {
                let _ = os::reap_orphans(self.leader, &self.gate);
            }
            if events.socket
                && let Some(cause) = self.read_holder()
            {
                return cause;
            }
            if events.terminated {
                return Cause::Ended;
            }
        }
    }

    /// Takes what the holder sent; the cause of a sweep when it asks for one
    /// or is gone.
    fn read_holder(&mut self) -> Option<Cause> {
        match self.stream.fill() {
            Ok(true) => {}
            Ok(false) | Err(_) => {
                self.holder_gone = true;
                return Some(Cause::HolderGone);
            }
        }
        loop {
            match self.stream.take() {
                Ok(None) => return None,
                Ok(Some((WatcherFrame::Terminate, _))) => return Some(Cause::Terminate),
                Ok(Some((WatcherFrame::ScopeAsk, _))) => self.send_scope_members(),
                Ok(Some((WatcherFrame::ScopeMade, _))) => self.send_scope_ready(),
                Ok(Some(_)) => {}
                Err(_) => return Some(Cause::Ended),
            }
        }
    }

    /// Hangs the child up as closing a terminal would: with `SIGHUP` when
    /// the holder asked, and by closing the copy of the master, which hangs
    /// the terminal up when the holder is gone. Whether it hung either up.
    fn hang_up(&mut self, cause: Cause) -> bool {
        let asked = cause == Cause::Terminate;
        if asked {
            let _ = os::hang_up_child(self.leader, &self.gate);
        }
        drop(self.master.take());
        if !asked && !self.holder_gone && os::peer_has_closed(self.stream.socket()) {
            self.holder_gone = true;
        }
        asked || self.holder_gone
    }

    /// Gives a child just hung up [`HANGUP_GRACE`] to exit by itself before
    /// the sweep, as a shell runs its hangup trap: waits for the child's exit
    /// alone once the holder's socket has nothing more for it.
    fn grace(&mut self, wait: &mut os::WatcherWait) {
        if self.holder_gone && wait.ignore_socket().is_err() {
            return;
        }
        let deadline = Instant::now() + HANGUP_GRACE;
        while !self.leader_told {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return;
            }
            let Ok(events) = wait.wait(Some(left)) else {
                return;
            };
            if events.leader_exited {
                self.tell_leader_exit();
            }
            #[cfg(target_os = "linux")]
            if events.children {
                let _ = os::reap_orphans(self.leader, &self.gate);
            }
            // A holder gone, or sending what cannot be read, has nothing
            // more for the grace.
            if events.socket && self.read_holder().is_some() && wait.ignore_socket().is_err() {
                return;
            }
        }
    }

    fn tell_leader_exit(&mut self) {
        if self.leader_told {
            return;
        }
        if let Ok(Some(code)) = os::unreaped_exit(self.leader) {
            self.leader_told = true;
            let _ = self.stream.send(&WatcherFrame::LeaderExited { code }, &[]);
        }
    }

    fn sweep(&self, prior: os::PriorMembers) -> os::TreeEndRest {
        let sweep = os::TreeEnd::owned(self.leader, Arc::clone(&self.gate))
            .prior_members(prior)
            .exit_watch_slots(self.slots);
        #[cfg(target_os = "linux")]
        let sweep = match &self.cgroup {
            Some(dir) => sweep.cgroup(dir.clone()),
            None => sweep,
        };
        sweep.run(&mut Plain).unwrap_or_else(|_| os::TreeEndRest {
            unsent: vec![self.leader],
            ..os::TreeEndRest::default()
        })
    }

    /// Reaps what ended, tells the holder the sweep is over and, with the
    /// holder gone, removes its files under the lock the watcher holds.
    fn finish(mut self, rest: &os::TreeEndRest) {
        self.tell_leader_exit();
        if self.leader_told {
            let _ = os::reap_exited_child(self.leader, &self.gate);
        }
        #[cfg(target_os = "linux")]
        let _ = os::reap_orphans(self.leader, &self.gate);
        let remaining = rest.unsent.len() + rest.unended.len() + rest.unwaited;
        let told = self.stream.send(
            &WatcherFrame::SweepDone {
                remaining: u32::try_from(remaining).unwrap_or(u32::MAX),
            },
            &[],
        );
        if told.is_err() || os::peer_has_closed(self.stream.socket()) {
            self.holder_gone = true;
        }
        if self.holder_gone {
            let _ = os::remove_endpoint_files_under_lock(
                &self.files.record,
                &self.files.socket,
                &self.files.lock,
            );
        }
    }

    /// Answers [`WatcherFrame::ScopeAsk`] with the leader's pidfd and those
    /// of the watcher's other children (orphans of the tree), opened under
    /// the reap gate so no id is reused first, as many as one request to the
    /// user manager carries ([`MOST_FDS_PER_SCOPE_REQUEST`]); they fit one
    /// message to the holder too.
    #[cfg(target_os = "linux")]
    fn send_scope_members(&mut self) {
        let opened = self
            .gate
            .signal_unless_reaped(|| os::pidfd_open(self.leader))
            .unwrap_or_else(|| Err(io::Error::from(io::ErrorKind::NotFound)))
            .and_then(|leader| {
                let mut members = vec![leader];
                members.extend(
                    os::orphan_pidfds(self.leader, &self.gate)?
                        .into_iter()
                        .take(MOST_FDS_PER_SCOPE_REQUEST - 1),
                );
                Ok(members)
            });
        let members = opened.unwrap_or_default();
        let borrowed: Vec<BorrowedFd<'_>> = members.iter().map(|fd| fd.as_fd()).collect();
        let _ = self.stream.send(
            &WatcherFrame::ScopeMembers {
                count: u32::try_from(borrowed.len()).unwrap_or(0),
            },
            &borrowed,
        );
    }

    /// Answers [`WatcherFrame::ScopeMade`] with the cgroup the leader is in,
    /// which the sweep then kills and waits for, and how many of the tree's
    /// processes are outside it.
    #[cfg(target_os = "linux")]
    fn send_scope_ready(&mut self) {
        let frame = match self
            .gate
            .signal_unless_reaped(|| os::cgroup_dir_of(self.leader))
        {
            Some(Ok(Some(dir))) => {
                let outside = os::descendants_outside(&dir).map_or(UNCOUNTED, |outside| {
                    u32::try_from(outside).unwrap_or(UNCOUNTED)
                });
                let frame = WatcherFrame::ScopeReady {
                    cgroup_path: dir.to_string_lossy().into_owned(),
                    outside_members: outside,
                };
                self.cgroup = Some(dir);
                frame
            }
            _ => WatcherFrame::ScopeReady {
                cgroup_path: String::new(),
                outside_members: UNCOUNTED,
            },
        };
        let _ = self.stream.send(&frame, &[]);
    }

    #[cfg(not(target_os = "linux"))]
    fn send_scope_members(&mut self) {
        let _ = self
            .stream
            .send(&WatcherFrame::ScopeMembers { count: 0 }, &[]);
    }

    #[cfg(not(target_os = "linux"))]
    fn send_scope_ready(&mut self) {
        let _ = self.stream.send(
            &WatcherFrame::ScopeReady {
                cgroup_path: String::new(),
                outside_members: UNCOUNTED,
            },
            &[],
        );
    }
}
