#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// The trees swept here are real processes this binary starts as its own
// fixtures, so they are spawned directly.
#![allow(clippy::disallowed_methods)]

//! The sweep that ends a window's process tree, from its two entries: the
//! owner's, which waits (within its limit) for the members it signalled, and
//! a non-parent's, which does not. Members are told from processes outside
//! the window by the edges the sweep reads, and no process outside the window
//! is signalled.
//!
//! An owner runs in a fixture process, as the window's watcher would: a
//! subreaper on Linux, with `SIGCHLD` blocked in every thread. The sends and
//! the observed ends pass through hooks, so a refused send, a member that a
//! `SIGKILL` does not end, and members ending one at a time are made by the
//! test rather than by timing.

use std::io::{BufRead as _, BufReader, Read as _};
use std::os::unix::process::CommandExt as _;
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use yagura_test_support::HANG_GUARD;
use yagura_test_support::process::{
    libtest_name, process_is_running, wait_processes_gone_blocking,
};

const FIXTURE: &str = "tree_end_fixture";
const ROLE_ENV: &str = "YAGURA_OS_TREE_END_ROLE";
const SCENE_ENV: &str = "YAGURA_OS_TREE_END_SCENE";
const PLACEMENT_ENV: &str = "YAGURA_OS_TREE_END_PLACEMENT";
const COUNT_ENV: &str = "YAGURA_OS_TREE_END_COUNT";
const THEN_ENV: &str = "YAGURA_OS_TREE_END_THEN";
/// Prefix of the lines the fixtures report on, apart from libtest's own.
const TAG: &str = "@tree ";

/// A member that only `SIGKILL` ends.
const SLEEPER: &str = "trap '' TERM HUP; exec sleep 600";

/// The descriptors an owner keeps free for its own reading, as the watcher
/// does when it sizes its exit-watch slots from its descriptor limit.
#[cfg(target_os = "linux")]
const DESCRIPTOR_MARGIN: usize = 32;
/// The exit-watch slots left once the margin is kept free.
#[cfg(target_os = "linux")]
const SLOTS: usize = 2;
/// Members in the leader's group, more than the slots.
#[cfg(target_os = "linux")]
const GROUP_MEMBERS: usize = 6;

/// The roles this binary is started in by the tests below.
#[test]
#[ignore = "the processes of the tree-end tests"]
fn tree_end_fixture() {
    match std::env::var(ROLE_ENV).as_deref() {
        Ok("owner") => owner(),
        Ok("leader") => leader(),
        Ok("middle") => middle(),
        _ => {}
    }
}

fn say(line: &str) {
    println!("{TAG}{line}");
}

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set"))
}

/// This binary, started again in `role`.
fn fixture(role: &str) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            libtest_name(module_path!(), FIXTURE).as_str(),
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(ROLE_ENV, role)
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    command
}

fn sleeper() -> Command {
    let mut command = Command::new("/bin/sh");
    command
        .args(["-c", SLEEPER])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}

/// Starts `command` as the leader of a session of its own, with `SIGCHLD`
/// unblocked as a window's command starts.
fn as_session_leader(command: &mut Command) -> &mut Command {
    // SAFETY: setsid, sigemptyset, sigaddset and pthread_sigmask are
    // async-signal-safe and touch only the child being started.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            let mut set: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut set);
            libc::sigaddset(&mut set, libc::SIGCHLD);
            libc::pthread_sigmask(libc::SIG_UNBLOCK, &set, std::ptr::null_mut());
            Ok(())
        })
    }
}

/// The tagged lines `out` holds, read until `last` or to its end.
fn tagged_lines(out: ChildStdout, last: Option<&str>) -> Vec<String> {
    let mut lines = Vec::new();
    for line in BufReader::new(out).lines() {
        let Ok(line) = line else { break };
        // libtest may print the test's name on the same line first.
        let Some(at) = line.find(TAG) else {
            continue;
        };
        let line = &line[at + TAG.len()..];
        let done = Some(line) == last;
        lines.push(line.to_owned());
        if done {
            break;
        }
    }
    lines
}

fn pid_after(line: &str, word: &str) -> Option<u32> {
    line.strip_prefix(word)?.trim().parse().ok()
}

/// A window's command: starts `COUNT_ENV` members placed as `PLACEMENT_ENV`
/// says, reports them, then exits or stays as `THEN_ENV` says. Only `SIGKILL`
/// ends it.
fn leader() {
    // SAFETY: ignoring two signals changes only this fixture process.
    unsafe {
        libc::signal(libc::SIGTERM, libc::SIG_IGN);
        libc::signal(libc::SIGHUP, libc::SIG_IGN);
    }
    let count: usize = env(COUNT_ENV).parse().unwrap();
    let placement = env(PLACEMENT_ENV);
    for _ in 0..count {
        let pid = match placement.as_str() {
            "group" => sleeper().spawn().unwrap().id(),
            "own-group" => sleeper().process_group(0).spawn().unwrap().id(),
            "own-session" => {
                let mut middle = fixture("middle").stdout(Stdio::piped()).spawn().unwrap();
                let lines = tagged_lines(middle.stdout.take().unwrap(), None);
                middle.wait().unwrap();
                lines
                    .iter()
                    .find_map(|line| pid_after(line, "member "))
                    .expect("the middle process reported no member")
            }
            other => panic!("no placement {other}"),
        };
        say(&format!("member {pid}"));
    }
    say("ready");
    if env(THEN_ENV) == "stay" {
        // As long as its members live, so a failed test leaves nothing for good.
        std::thread::sleep(Duration::from_secs(600));
    }
}

/// Starts one member in a session of its own and exits, so the member is
/// orphaned as a double-forked server is.
fn middle() {
    let mut member = sleeper();
    // SAFETY: setsid is async-signal-safe and touches only the child.
    unsafe {
        member.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    say(&format!("member {}", member.spawn().unwrap().id()));
}

/// The descriptors this process has open now.
#[cfg(target_os = "linux")]
fn open_descriptors() -> usize {
    std::fs::read_dir("/proc/self/fd").unwrap().count()
}

/// Lowers this process's descriptor limit so `SLOTS` exit watches fit once
/// the margin is kept free, and returns the slots computed from it as the
/// watcher computes them.
#[cfg(target_os = "linux")]
fn lower_descriptor_limit() -> usize {
    let open = open_descriptors();
    let soft = open + DESCRIPTOR_MARGIN + SLOTS;
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: `limit` is a live local the calls read and write.
    unsafe {
        assert_eq!(libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit), 0);
        limit.rlim_cur = libc::rlim_t::try_from(soft).unwrap();
        assert_eq!(libc::setrlimit(libc::RLIMIT_NOFILE, &limit), 0);
    }
    soft - open - DESCRIPTOR_MARGIN
}

/// The descriptor-limit scenes are Linux's: macOS waits on exits without a
/// descriptor per member.
#[cfg(not(target_os = "linux"))]
fn lower_descriptor_limit() -> usize {
    panic!("the exit-watch slots are a Linux owner's")
}

/// The owner of a window's tree, as its watcher: starts the leader, then
/// sweeps the tree from the owner's entry for `SCENE_ENV`, reporting the
/// sends, the ends it was told of and what the sweep left.
fn owner() {
    use std::sync::Arc;
    use yagura_os::platform_unix::{SignalTarget, TreeEnd, TreeEndHooks};

    #[derive(Default)]
    struct Hooks {
        leader: u32,
        refuse_first: bool,
        refused: Option<u32>,
        hold_all: bool,
        hold: Option<u32>,
        chain: Vec<u32>,
        next: usize,
    }

    impl Hooks {
        fn end_next(&mut self) {
            if let Some(&pid) = self.chain.get(self.next) {
                self.next += 1;
                // SAFETY: SIGKILL to a member this fixture's tree started.
                unsafe { libc::kill(libc::pid_t::try_from(pid).unwrap(), libc::SIGKILL) };
            }
        }
    }

    impl TreeEndHooks for Hooks {
        fn send(
            &mut self,
            target: SignalTarget,
            signal: libc::c_int,
            deliver: &mut dyn FnMut() -> std::io::Result<()>,
        ) -> std::io::Result<()> {
            match target {
                SignalTarget::Process(pid) => {
                    say(&format!("target P {pid} {signal}"));
                    if self.refuse_first && pid != self.leader {
                        let first = *self.refused.get_or_insert(pid);
                        if first == pid {
                            say(&format!("refused {pid}"));
                            return Err(std::io::Error::from_raw_os_error(libc::EPERM));
                        }
                    }
                    if self.hold == Some(pid) {
                        return Ok(());
                    }
                }
                SignalTarget::Group(pgid) => say(&format!("target G {pgid} {signal}")),
                _ => say(&format!("target C 0 {signal}")),
            }
            if self.hold_all {
                return Ok(());
            }
            deliver()
        }

        fn watching(&mut self, _pids: &[u32]) {
            if self.next == 0 {
                self.end_next();
            }
        }

        fn ended(&mut self, pid: u32) {
            say(&format!("ended {pid}"));
            if self.next > 0 && self.chain.get(self.next - 1) == Some(&pid) {
                self.end_next();
            }
        }
    }

    let scene = env(SCENE_ENV);
    #[cfg(target_os = "linux")]
    yagura_os::platform_unix::set_child_subreaper().unwrap();
    let (placement, count, then) = match scene.as_str() {
        "refused" => ("own-session", 3, "stay"),
        "slots-real" | "slots-stuck" => ("group", 6, "stay"),
        "session" => ("own-group", 3, "exit"),
        "limit" => ("own-group", 3, "exit"),
        "one-by-one" => ("own-group", 5, "exit"),
        other => panic!("no scene {other}"),
    };
    let mut command = fixture("leader");
    command
        .env(PLACEMENT_ENV, placement)
        .env(COUNT_ENV, count.to_string())
        .env(THEN_ENV, then)
        .stdout(Stdio::piped());
    let mut leader = as_session_leader(&mut command).spawn().unwrap();
    let leader_pid = leader.id();
    let out = leader.stdout.take().unwrap();
    let lines = tagged_lines(out, (then == "stay").then_some("ready"));
    let members: Vec<u32> = lines
        .iter()
        .filter_map(|line| pid_after(line, "member "))
        .collect();
    assert_eq!(members.len(), count, "the leader reported {lines:?}");
    say(&format!("leader {leader_pid}"));
    for member in &members {
        say(&format!("member {member}"));
    }
    if then == "exit" {
        yagura_os::platform_unix::wait_for_exit_without_reaping(leader_pid).unwrap();
    }

    let mut hooks = Hooks {
        leader: leader_pid,
        ..Hooks::default()
    };
    let mut slots = 64;
    let mut limit = HANG_GUARD;
    let mut rounds = None;
    match scene.as_str() {
        "refused" => hooks.refuse_first = true,
        "slots-real" | "slots-stuck" => {
            slots = lower_descriptor_limit();
            if scene == "slots-stuck" {
                hooks.hold_all = true;
                limit = Duration::from_millis(300);
            }
        }
        "limit" => {
            hooks.hold = Some(members[0]);
            limit = Duration::from_millis(300);
        }
        "one-by-one" => {
            hooks.hold_all = true;
            hooks.chain.clone_from(&members);
            rounds = Some(2);
        }
        _ => {}
    }

    let gate = Arc::new(yagura_os::ReapGate::new());
    let mut sweep = TreeEnd::owned(leader_pid, gate)
        .exit_watch_slots(slots)
        .limit(limit);
    if let Some(rounds) = rounds {
        sweep = sweep.kill_rounds(rounds);
    }
    match sweep.run(&mut hooks) {
        Ok(rest) => say(&format!(
            "rest {};{};{}",
            csv(&rest.unsent),
            csv(&rest.unended),
            rest.unwaited
        )),
        Err(error) => say(&format!("error {error}")),
    }
    // The leader is left unreaped: the sweep's owner holds it as the watcher does.
    std::mem::forget(leader);
}

fn csv(pids: &[u32]) -> String {
    pids.iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

fn parse_csv(text: &str) -> Vec<u32> {
    text.split(',')
        .filter(|item| !item.is_empty())
        .map(|item| item.parse().unwrap())
        .collect()
}

/// What a sweep left, as reported.
#[derive(Debug)]
struct Rest {
    unsent: Vec<u32>,
    unended: Vec<u32>,
    unwaited: usize,
}

/// What an owner fixture reported.
#[derive(Debug)]
struct Report {
    leader: u32,
    members: Vec<u32>,
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    refused: Option<u32>,
    targets: Vec<(char, u32)>,
    ended: Vec<u32>,
    rest: Result<Rest, String>,
}

/// Ends the listed processes still running when dropped, so a failed test
/// leaves none of its tree behind.
struct Leftovers(Vec<u32>);

impl Drop for Leftovers {
    fn drop(&mut self) {
        for &pid in &self.0 {
            if pid != 0 && process_is_running(pid) {
                // SAFETY: SIGKILL to a process of this test's own tree.
                unsafe { libc::kill(libc::pid_t::try_from(pid).unwrap(), libc::SIGKILL) };
            }
        }
    }
}

/// Runs an owner fixture for `scene` and returns its report, with the tree's
/// processes registered in `leftovers`.
fn run_owner(scene: &str, leftovers: &mut Leftovers) -> Report {
    let mut command = fixture("owner");
    command
        .env(SCENE_ENV, scene)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // SAFETY: sigemptyset, sigaddset and pthread_sigmask are async-signal-safe
    // and set only the mask the fixture starts every thread with.
    unsafe {
        command.pre_exec(|| {
            let mut set: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut set);
            libc::sigaddset(&mut set, libc::SIGCHLD);
            libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
            Ok(())
        });
    }
    let mut owner = command.spawn().unwrap();
    let owner_pid = owner.id();
    let out = owner.stdout.take().unwrap();
    let mut err = owner.stderr.take().unwrap();
    let (done, finished) = mpsc::channel();
    std::thread::spawn(move || {
        let lines = tagged_lines(out, None);
        let mut said = String::new();
        let _ = err.read_to_string(&mut said);
        let _ = done.send((lines, said, owner.wait()));
    });
    let Ok((lines, said, status)) = finished.recv_timeout(HANG_GUARD) else {
        // SAFETY: SIGKILL to the fixture this test started.
        unsafe { libc::kill(libc::pid_t::try_from(owner_pid).unwrap(), libc::SIGKILL) };
        panic!("the {scene} sweep did not end within {HANG_GUARD:?}");
    };
    let mut report = Report {
        leader: 0,
        members: Vec::new(),
        refused: None,
        targets: Vec::new(),
        ended: Vec::new(),
        rest: Err(format!("no rest reported; status {status:?}; {said}")),
    };
    for line in &lines {
        if let Some(pid) = pid_after(line, "leader ") {
            report.leader = pid;
        } else if let Some(pid) = pid_after(line, "member ") {
            report.members.push(pid);
        } else if let Some(pid) = pid_after(line, "refused ") {
            report.refused = Some(pid);
        } else if let Some(pid) = pid_after(line, "ended ") {
            report.ended.push(pid);
        } else if let Some(target) = line.strip_prefix("target ") {
            let mut fields = target.split(' ');
            let kind = fields.next().unwrap().chars().next().unwrap();
            report
                .targets
                .push((kind, fields.next().unwrap().parse().unwrap()));
        } else if let Some(rest) = line.strip_prefix("rest ") {
            let fields: Vec<&str> = rest.split(';').collect();
            report.rest = Ok(Rest {
                unsent: parse_csv(fields[0]),
                unended: parse_csv(fields[1]),
                unwaited: fields[2].parse().unwrap(),
            });
        } else if let Some(error) = line.strip_prefix("error ") {
            report.rest = Err(error.to_owned());
        }
    }
    leftovers.0.push(report.leader);
    leftovers.0.extend(&report.members);
    assert!(
        report.leader != 0 && !report.members.is_empty(),
        "the {scene} owner reported no tree: {lines:?} {said}"
    );
    report
}

/// Processes outside every window under test: one in a session of its own,
/// and another window's leader with its child.
struct Outsiders {
    children: Vec<Child>,
    pids: Vec<u32>,
    groups: Vec<u32>,
}

impl Outsiders {
    fn start() -> Self {
        let lone = as_session_leader(&mut sleeper()).spawn().unwrap();
        let mut other = Command::new("/bin/sh");
        other
            .args([
                "-c",
                "trap '' TERM HUP; sleep 600 & echo \"$!\"; exec sleep 600",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut other = as_session_leader(&mut other).spawn().unwrap();
        let mut line = String::new();
        BufReader::new(other.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let other_child: u32 = line.trim().parse().unwrap();
        Self {
            pids: vec![lone.id(), other.id(), other_child],
            groups: vec![lone.id(), other.id()],
            children: vec![lone, other],
        }
    }

    /// No send of the sweep named an outsider, and every outsider still runs.
    fn assert_untouched(&self, targets: &[(char, u32)]) {
        for &(kind, id) in targets {
            assert!(
                !(kind == 'P' && self.pids.contains(&id)),
                "the sweep signalled the outside process {id}"
            );
            assert!(
                !(kind == 'G' && self.groups.contains(&id)),
                "the sweep signalled the outside group {id}"
            );
        }
        for &pid in &self.pids {
            assert!(
                runs_unended(pid),
                "the outside process {pid} did not survive the sweep"
            );
        }
    }
}

impl Drop for Outsiders {
    fn drop(&mut self) {
        for &pid in self.pids.iter().rev() {
            // SAFETY: SIGKILL to a process this test started.
            unsafe { libc::kill(libc::pid_t::try_from(pid).unwrap(), libc::SIGKILL) };
        }
        for child in &mut self.children {
            let _ = child.wait();
        }
    }
}

/// Whether `pid` runs and is not a zombie.
fn runs_unended(pid: u32) -> bool {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
            stat.rsplit_once(')')
                .is_some_and(|(_, after)| !after.trim_start().starts_with('Z'))
        })
    }
    #[cfg(target_os = "macos")]
    {
        // SAFETY: zeroed is a valid `proc_bsdinfo`, which proc_pidinfo only writes.
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let size = libc::c_int::try_from(std::mem::size_of::<libc::proc_bsdinfo>()).unwrap();
        // SAFETY: `info` is a live local of `size` bytes.
        let written = unsafe {
            libc::proc_pidinfo(
                libc::c_int::try_from(pid).unwrap(),
                libc::PROC_PIDTBSDINFO,
                0,
                std::ptr::addr_of_mut!(info).cast(),
                size,
            )
        };
        written == size && info.pbi_status != libc::SZOMB
    }
}

fn expect_rest(report: &Report) -> &Rest {
    report
        .rest
        .as_ref()
        .unwrap_or_else(|error| panic!("the sweep failed: {error}"))
}

/// The owner ends orphans that left for sessions of their own, and a send
/// refused with `EPERM` does not stop the sends after it.
#[cfg(target_os = "linux")]
#[test]
fn an_owned_sweep_ends_orphans_in_sessions_of_their_own_past_a_refused_send() {
    let outsiders = Outsiders::start();
    let mut leftovers = Leftovers(Vec::new());
    let report = run_owner("refused", &mut leftovers);
    let rest = expect_rest(&report);
    let refused = report.refused.expect("no send was made to a member");
    assert!(
        report.members.contains(&refused),
        "the refused send was to {refused}, not a member: {report:?}"
    );
    assert!(
        rest.unsent.contains(&refused),
        "the member whose send was refused is not reported unsent: {report:?}"
    );
    let rest_of_tree: Vec<u32> = report
        .members
        .iter()
        .copied()
        .filter(|&pid| pid != refused)
        .chain([report.leader])
        .collect();
    wait_processes_gone_blocking(&rest_of_tree, |left| {
        format!("the sweep stopped at the refused send and left {left:?}: {report:?}")
    });
    assert!(
        runs_unended(refused),
        "the member whose send was refused was ended anyway: {report:?}"
    );
    outsiders.assert_untouched(&report.targets);
}

/// Short of exit-watch slots, an owner still ends the whole tree; members it
/// had no slot for are counted as not waited for.
#[cfg(target_os = "linux")]
#[test]
fn an_owned_sweep_short_of_exit_watch_slots_ends_the_tree_and_counts_who_it_could_not_wait_for() {
    let mut leftovers = Leftovers(Vec::new());

    let report = run_owner("slots-real", &mut leftovers);
    expect_rest(&report);
    let tree: Vec<u32> = report
        .members
        .iter()
        .copied()
        .chain([report.leader])
        .collect();
    wait_processes_gone_blocking(&tree, |left| {
        format!("short of slots, the sweep left {left:?}: {report:?}")
    });

    let report = run_owner("slots-stuck", &mut leftovers);
    let rest = expect_rest(&report);
    assert_eq!(report.members.len(), GROUP_MEMBERS);
    assert_eq!(
        rest.unwaited,
        GROUP_MEMBERS - SLOTS,
        "every member beyond the slots is one not waited for: {report:?}"
    );
}

/// On macOS the owner reaches the members of the dead leader's session while
/// it holds the leader's zombie.
#[cfg(target_os = "macos")]
#[test]
fn an_owned_sweep_ends_the_dead_leaders_session_while_its_zombie_is_held() {
    let outsiders = Outsiders::start();
    let mut leftovers = Leftovers(Vec::new());
    let report = run_owner("session", &mut leftovers);
    expect_rest(&report);
    wait_processes_gone_blocking(&report.members, |left| {
        format!("the session's members {left:?} outlived the sweep: {report:?}")
    });
    outsiders.assert_untouched(&report.targets);
}

/// A member that a `SIGKILL` does not end is waited for no longer than the
/// limit, and is returned as not ended.
#[test]
fn an_owned_sweep_closes_at_its_limit_and_returns_the_member_that_did_not_end() {
    let mut leftovers = Leftovers(Vec::new());
    let report = run_owner("limit", &mut leftovers);
    let rest = expect_rest(&report);
    let stuck = report.members[0];
    assert!(
        rest.unsent.is_empty(),
        "every send went through: {report:?}"
    );
    assert_eq!(
        rest.unended,
        vec![stuck],
        "the member the kill did not end is the one left: {report:?}"
    );
    wait_processes_gone_blocking(&report.members[1..], |left| {
        format!("the members {left:?} outlived the sweep: {report:?}")
    });
}

/// Members ending one at a time, more of them than the rounds allowed, are
/// all waited for: a round with nothing new to send counts against nothing.
#[test]
fn an_owned_sweep_waits_out_members_ending_one_at_a_time_past_its_rounds() {
    let mut leftovers = Leftovers(Vec::new());
    let report = run_owner("one-by-one", &mut leftovers);
    let rest = expect_rest(&report);
    for member in &report.members {
        assert!(
            report.ended.contains(member),
            "the sweep closed before the end of member {member}: {report:?}"
        );
    }
    assert!(rest.unended.is_empty(), "every member ended: {report:?}");
    assert_eq!(rest.unwaited, 0, "every member was waited for: {report:?}");
}

/// A sweep from outside the tree returns without waiting for the members it
/// signalled, and signals nothing outside the window.
#[test]
fn a_non_parent_sweep_returns_without_waiting_for_the_members_it_signalled() {
    use yagura_os::platform_unix::{SignalTarget, TreeEnd, TreeEndHooks};

    struct Hooks {
        hold: u32,
        targets: Vec<(char, u32)>,
    }

    impl TreeEndHooks for Hooks {
        fn send(
            &mut self,
            target: SignalTarget,
            _signal: libc::c_int,
            deliver: &mut dyn FnMut() -> std::io::Result<()>,
        ) -> std::io::Result<()> {
            match target {
                SignalTarget::Process(pid) => {
                    self.targets.push(('P', pid));
                    if pid == self.hold {
                        return Ok(());
                    }
                }
                SignalTarget::Group(pgid) => self.targets.push(('G', pgid)),
                _ => self.targets.push(('C', 0)),
            }
            deliver()
        }
    }

    let outsiders = Outsiders::start();
    let mut command = fixture("leader");
    command
        .env(PLACEMENT_ENV, "own-group")
        .env(COUNT_ENV, "3")
        .env(THEN_ENV, "stay")
        .stdout(Stdio::piped());
    let mut leader = as_session_leader(&mut command).spawn().unwrap();
    let leader_pid = leader.id();
    let lines = tagged_lines(leader.stdout.take().unwrap(), Some("ready"));
    let members: Vec<u32> = lines
        .iter()
        .filter_map(|line| pid_after(line, "member "))
        .collect();
    let _leftovers = Leftovers(members.iter().copied().chain([leader_pid]).collect());
    let stuck = members[0];
    let start = yagura_os::process_start(leader_pid).unwrap().unwrap();

    let (done, swept) = mpsc::channel();
    std::thread::spawn(move || {
        let mut hooks = Hooks {
            hold: stuck,
            targets: Vec::new(),
        };
        let result = TreeEnd::not_parent(leader_pid, start)
            .limit(Duration::MAX)
            .run(&mut hooks);
        let _ = done.send((result, hooks.targets));
    });
    let (result, targets) = swept
        .recv_timeout(HANG_GUARD)
        .expect("the non-parent sweep waited for a member it signalled");
    let rest = result.expect("the non-parent sweep failed");
    assert!(
        targets.contains(&('P', stuck)),
        "the sweep never signalled the member {stuck}: {targets:?}"
    );
    assert!(
        runs_unended(stuck),
        "the member whose signal was held back has ended"
    );
    assert!(
        rest.unended.is_empty(),
        "a non-parent sweep judged members unended, so it waited: {:?}",
        rest.unended
    );
    wait_processes_gone_blocking(&members[1..], |left| {
        format!("the members {left:?} outlived the non-parent sweep: {targets:?}")
    });
    let (reaped, reaping) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = reaped.send(leader.wait());
    });
    reaping
        .recv_timeout(HANG_GUARD)
        .expect("the leader outlived the non-parent sweep")
        .unwrap();
    outsiders.assert_untouched(&targets);
}

/// The edges a sweep counts members by: a child read under a parent counts
/// only while that parent is still the process read, and a session counts
/// only while its leader is; with the leader gone, neither edge counts.
#[test]
fn window_members_count_an_edge_only_while_its_end_is_still_the_process_read() {
    use yagura_os::platform_unix::{ProcessRow, window_members};

    let row = |pid, ppid, sid| ProcessRow { pid, ppid, sid };
    let leader = 100;
    let rows = [
        row(leader, 1, leader),
        // The leader's child.
        row(101, leader, leader),
        // A child of 101 in a session of its own: reached only through 101.
        row(102, 101, 102),
        // A child of 101 in the leader's session.
        row(103, 101, leader),
        // Handed to init, still in the leader's session.
        row(104, 1, leader),
        // Another window.
        row(300, 1, 300),
        row(301, 300, 300),
    ];
    let outside = [300, 301];

    let all = window_members(&rows, leader, |_| true);
    for pid in [101, 102, 103, 104] {
        assert!(all.contains(&pid), "{pid} is the window's: {all:?}");
    }
    for pid in outside {
        assert!(!all.contains(&pid), "{pid} is not the window's: {all:?}");
    }

    let parent_replaced = window_members(&rows, leader, |pid| pid != 101);
    assert!(
        !parent_replaced.contains(&102),
        "102 was counted through a parent that is no longer the process read: \
         {parent_replaced:?}"
    );
    assert!(
        parent_replaced.contains(&103),
        "103 is in the leader's session: {parent_replaced:?}"
    );

    let leader_gone = window_members(&rows, leader, |pid| pid != leader);
    for pid in [101, 102, 103, 104, 300, 301] {
        assert!(
            !leader_gone.contains(&pid),
            "{pid} was counted though the leader is gone: {leader_gone:?}"
        );
    }
}
