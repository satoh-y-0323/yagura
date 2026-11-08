#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// The children here are real processes the tests start and reap themselves.
#![allow(clippy::disallowed_methods)]

//! The Unix events a window's end is ordered by: a child's exit known without
//! reaping it, and the end mark that closes the terminal's output once every
//! writer has died.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use yagura_os::pty::{PtyCommand, PtySize, leader_command, open, start_leader};
use yagura_test_support::HANG_GUARD;
use yagura_test_support::paths::short_tmp_dir;
use yagura_test_support::process::wait_processes_gone_blocking;

/// The most of the terminal's output kept: the end of it is what is checked.
const KEPT_TAIL: usize = 64 * 1024;

/// Whether `pid` names a process, a zombie included.
fn exists(pid: u32) -> bool {
    // SAFETY: signal 0 delivers nothing; it only checks that `pid` exists.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

#[derive(Debug, PartialEq, Eq)]
enum ChildState {
    Running,
    ExitedUnreaped,
    Reaped,
}

/// The state of this process's child `pid`, observed without reaping it.
fn child_state(pid: u32) -> ChildState {
    // SAFETY: zeroed is a valid `siginfo_t`, which waitid only writes.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    // SAFETY: `info` is a stack local waitid fills; `WNOWAIT` leaves the
    // child as it is and `WNOHANG` returns at once.
    let result = unsafe {
        libc::waitid(
            libc::P_PID,
            pid as libc::id_t,
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    if result != 0 {
        let error = std::io::Error::last_os_error();
        assert_eq!(
            error.raw_os_error(),
            Some(libc::ECHILD),
            "waitid failed: {error}"
        );
        return ChildState::Reaped;
    }
    // SAFETY: waitid filled `info` for a child that changed state, and left
    // it zeroed otherwise.
    let changed = unsafe { info.si_pid() };
    if changed == pid as libc::pid_t {
        ChildState::ExitedUnreaped
    } else {
        ChildState::Running
    }
}

#[test]
fn a_childs_exit_is_known_without_reaping_it_and_the_reap_comes_after() {
    let mut child = Command::new("/bin/sh")
        .args(["-c", "read line; exit 7"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let pid = child.id();
    let (done, waited) = mpsc::channel();
    std::thread::spawn(move || {
        let result = yagura_os::platform_unix::wait_for_exit_without_reaping(pid);
        let _ = done.send(result.map(|()| child_state(pid)));
    });

    drop(child.stdin.take());
    let state = waited
        .recv_timeout(HANG_GUARD)
        .expect("the wait did not return after the child exited")
        .expect("the wait failed");
    assert_eq!(
        state,
        ChildState::ExitedUnreaped,
        "the wait must return once the child has exited, leaving it unreaped"
    );

    let status = child.wait().unwrap();
    assert_eq!(status.code(), Some(7), "the reap did not get the exit");
    assert!(!exists(pid), "the reaped child is still there");
}

/// Reaps this process's child `pid`, waiting for it to exit.
fn reap(pid: u32) {
    loop {
        let mut status = 0;
        // SAFETY: `status` is a live stack local; `pid` is this test's child.
        let result = unsafe { libc::waitpid(pid as libc::pid_t, &mut status, 0) };
        if result == pid as libc::pid_t {
            return;
        }
        let error = std::io::Error::last_os_error();
        assert_eq!(
            error.kind(),
            std::io::ErrorKind::Interrupted,
            "reap the session leader: {error}"
        );
    }
}

/// Reads the terminal until its output ends, keeping only its tail.
fn read_tail(mut reader: Box<dyn Read + Send>) -> mpsc::Receiver<Vec<u8>> {
    let (tx, tail) = mpsc::channel();
    std::thread::spawn(move || {
        let mut kept = Vec::new();
        let mut buffer = [0u8; 8192];
        while let Ok(read @ 1..) = reader.read(&mut buffer) {
            kept.extend_from_slice(&buffer[..read]);
            if kept.len() > 2 * KEPT_TAIL {
                kept.drain(..kept.len() - KEPT_TAIL);
            }
        }
        let _ = tx.send(kept);
    });
    tail
}

fn read_pid(path: &Path) -> u32 {
    let deadline = std::time::Instant::now() + HANG_GUARD;
    loop {
        if let Some(pid) = std::fs::read_to_string(path)
            .ok()
            .and_then(|text| text.trim().parse().ok())
        {
            return pid;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no pid in {}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The terminal's writers, a session leader and a member of its group, write
/// until they are killed while another process keeps the terminal open, and
/// nothing they wrote comes after the end of the terminal's output. On Linux
/// the output goes on with them dead, and the end mark written once both are
/// confirmed dead is the last thing it holds. On macOS the session leader's
/// exit revokes its controlling terminal: the output ends there, and the
/// process still holding the terminal can no longer write to it.
#[test]
fn nothing_the_dead_writers_wrote_comes_after_the_end_mark() {
    use std::os::unix::fs::OpenOptionsExt as _;

    let dir = short_tmp_dir("exitw", "mark");
    let member_file = dir.path().join("member");
    let tty_file = dir.path().join("tty");
    let pair = open(PtySize { cols: 80, rows: 24 }).expect("open a pseudoterminal");
    let command = leader_command(&PtyCommand {
        argv: vec![
            "/bin/sh".to_owned(),
            "-c".to_owned(),
            "tty > \"$2\"; yes aaaa & echo $! > \"$1\"; exec yes bbbb".to_owned(),
            "sh".to_owned(),
            member_file.to_str().unwrap().to_owned(),
            tty_file.to_str().unwrap().to_owned(),
        ],
        cwd: None,
        env: Vec::new(),
    })
    .expect("describe the writers");
    let leader = start_leader(&command, pair.slave).expect("start the writers on a pseudoterminal");
    let group = yagura_os::platform_unix::UnixProcessGroup::led_by(
        leader,
        yagura_os::PROCESS_GROUP_TERM_GRACE,
    )
    .expect("take the writers' group");
    // Read all along: a writer, the end mark's included, stops once the
    // terminal's buffer is full and nobody reads it.
    let tail = read_tail(pair.reader);
    let member = read_pid(&member_file);
    let held = std::fs::OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_NOCTTY)
        .open(std::fs::read_to_string(&tty_file).unwrap().trim())
        .expect("open the terminal by its name");

    group.terminate().expect("end the writers' group");
    reap(leader);
    wait_processes_gone_blocking(&[member], |left| {
        format!("the group member {left:?} outlived the group's end")
    });

    #[cfg(target_os = "macos")]
    {
        use std::io::Write as _;

        tail.recv_timeout(HANG_GUARD)
            .expect("the terminal's output did not end with its session leader");
        let refused = (&held)
            .write_all(b"after the end")
            .expect_err("the revoked terminal still took a write");
        assert_eq!(
            refused.raw_os_error(),
            Some(libc::EIO),
            "the revoked terminal refused the write otherwise: {refused}"
        );
    }
    #[cfg(target_os = "linux")]
    {
        let mark = yagura_os::pty::PtyControl::new_end_mark().expect("make the end mark");
        let writer = pair.control.write_mark(&mark).expect("write the end mark");
        // This reader reads to the end by itself, so the terminal may close now.
        drop(writer);
        drop(held);
        let tail = tail
            .recv_timeout(HANG_GUARD)
            .expect("the terminal's output did not end after the end mark");

        assert!(
            !mark.is_empty() && mark.iter().all(u8::is_ascii_digit),
            "the end mark is not a run of digits: {mark:?}"
        );
        assert!(
            tail.ends_with(&mark),
            "the output does not end with the end mark {:?}: ...{:?}",
            String::from_utf8_lossy(&mark),
            String::from_utf8_lossy(&tail[tail.len().saturating_sub(200)..])
        );
    }
}
