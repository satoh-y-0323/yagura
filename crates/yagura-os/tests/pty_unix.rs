#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! `yagura_os::pty` on Unix, driven directly: the terminal size the child
//! reads from its own terminal, and closing the terminal.

use std::io::{ErrorKind, Read, Write};
use std::sync::mpsc;

use yagura_os::pty::{PtyCommand, PtyControl, PtySize, leader_command, open, start_leader};

/// A shell started on a new terminal, as a window's watcher starts a child.
struct Started {
    control: PtyControl,
    reader: Box<dyn Read + Send>,
    writer: Box<dyn Write + Send>,
    _leader: Leader,
}

/// The shell's process id. Dropped, it kills and reaps the shell, so a test
/// leaves no process behind.
struct Leader(u32);

impl Drop for Leader {
    fn drop(&mut self) {
        let pid = self.0 as libc::pid_t;
        // SAFETY: `pid` is this test's unreaped child, so the id still names it.
        unsafe { libc::kill(pid, libc::SIGKILL) };
        let mut status = 0;
        // SAFETY: `status` is a live stack local; `pid` is this test's child.
        while unsafe { libc::waitpid(pid, &mut status, 0) } == -1
            && std::io::Error::last_os_error().kind() == ErrorKind::Interrupted
        {}
    }
}

fn start_shell(script: &str, size: PtySize) -> Started {
    let pair = open(size).expect("a pseudoterminal should open");
    let command = leader_command(&PtyCommand {
        argv: vec!["/bin/sh".to_string(), "-c".to_string(), script.to_string()],
        cwd: None,
        env: Vec::new(),
    })
    .expect("describe /bin/sh");
    let leader =
        start_leader(&command, pair.slave).expect("/bin/sh should start on a pseudoterminal");
    Started {
        control: pair.control,
        reader: pair.reader,
        writer: pair.writer,
        _leader: Leader(leader),
    }
}

/// The child's output, read on a thread so a test can wait for a line.
struct Output {
    chunks: mpsc::Receiver<Vec<u8>>,
    seen: Vec<u8>,
}

impl Output {
    fn new(mut reader: Box<dyn Read + Send>) -> Self {
        let (tx, chunks) = mpsc::channel();
        std::thread::spawn(move || {
            let mut buffer = [0u8; 1024];
            while let Ok(read @ 1..) = reader.read(&mut buffer) {
                if tx.send(buffer[..read].to_vec()).is_err() {
                    break;
                }
            }
        });
        Self {
            chunks,
            seen: Vec::new(),
        }
    }

    /// Returns once `needle` has been written; fails when the output ends
    /// without it.
    fn wait_for(&mut self, needle: &str) {
        while !String::from_utf8_lossy(&self.seen).contains(needle) {
            // Only a hang guard: the output ending fails the wait at once.
            match self.chunks.recv_timeout(yagura_test_support::HANG_GUARD) {
                Ok(chunk) => self.seen.extend(chunk),
                Err(error) => panic!(
                    "{needle:?} never appeared ({error}); output: {:?}",
                    String::from_utf8_lossy(&self.seen)
                ),
            }
        }
    }
}

// Not 80x24, a common default size.
#[test]
fn the_child_reads_the_size_the_terminal_was_opened_with() {
    let started = start_shell("stty size", PtySize { cols: 90, rows: 33 });
    let mut output = Output::new(started.reader);

    output.wait_for("33 90");
}

#[test]
fn the_child_reads_the_size_a_resize_set() {
    let mut started = start_shell(
        "printf 'ready\\n'; read line; stty size",
        PtySize { cols: 80, rows: 24 },
    );
    let mut output = Output::new(started.reader);
    output.wait_for("ready");

    started
        .control
        .resize(100, 30)
        .expect("an open terminal accepts a resize");
    started.writer.write_all(b"\n").expect("write to the child");

    output.wait_for("30 100");
}

#[test]
fn a_closed_terminal_refuses_a_resize_and_says_it_is_closed() {
    let started = start_shell("exec sleep 30", PtySize { cols: 80, rows: 24 });
    let open_text = format!("{:?}", started.control);

    started.control.close();

    let refused = started.control.resize(100, 30);
    assert_eq!(
        refused.map_err(|error| error.kind()),
        Err(ErrorKind::BrokenPipe)
    );
    assert!(open_text.contains("open: true"), "got {open_text:?}");
    let closed_text = format!("{:?}", started.control);
    assert!(closed_text.contains("open: false"), "got {closed_text:?}");
}
