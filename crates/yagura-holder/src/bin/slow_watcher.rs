//! Test utility (Unix): a watcher that tells the holder its child started
//! only after `DELAY`, whatever the child has done meanwhile. It stands in for
//! `yagura-watcher` beside a copy of the holder, to show what the holder does
//! with a child's output while its start report is late. It ends nothing but
//! the report: no sweep, no scope. Never shipped.

#[cfg(unix)]
mod fixture {
    use std::io;
    use std::os::fd::{AsFd as _, OwnedFd};
    use std::process::ExitCode;
    use std::time::Duration;

    use yagura_holder::watcher_proto::{
        WATCHER_PROTOCOL, WatcherFrame, decode_watcher_frame, encode_watcher_frame,
    };
    use yagura_os::ReapGate;
    use yagura_os::platform_unix as os;

    /// Longer than macOS keeps the output of a closed terminal unread.
    const DELAY: Duration = Duration::from_millis(1_000);

    pub fn run() -> ExitCode {
        match serve() {
            Ok(()) => ExitCode::SUCCESS,
            Err(_) => ExitCode::from(2),
        }
    }

    fn invalid(what: &str) -> io::Error {
        io::Error::new(io::ErrorKind::InvalidData, what.to_owned())
    }

    fn send(socket: &OwnedFd, frame: &WatcherFrame) -> io::Result<()> {
        os::send_with_descriptors(socket.as_fd(), &encode_watcher_frame(frame), &[])
    }

    /// The holder's one frame, the `Spawn`, with the descriptors beside it.
    fn receive_spawn(socket: &OwnedFd) -> io::Result<(WatcherFrame, Vec<OwnedFd>)> {
        let mut bytes = Vec::new();
        let mut fds = Vec::new();
        loop {
            let mut chunk = [0u8; 4096];
            let read = os::receive_with_descriptors(socket.as_fd(), &mut chunk, &mut fds)?;
            if read == 0 {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
            bytes.extend_from_slice(&chunk[..read]);
            let decoded = decode_watcher_frame(&bytes)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
            if let Some((frame, _)) = decoded {
                return Ok((frame, fds));
            }
        }
    }

    fn serve() -> io::Result<()> {
        let fd = std::env::args()
            .nth(1)
            .and_then(|fd| fd.parse().ok())
            .ok_or_else(|| invalid("usage: slow_watcher <fd>"))?;
        let socket = os::take_inherited_socket(fd)?;
        send(
            &socket,
            &WatcherFrame::Hello {
                protocol: WATCHER_PROTOCOL,
            },
        )?;
        let (WatcherFrame::Spawn { argv, cwd, env, .. }, fds) = receive_spawn(&socket)? else {
            return Err(invalid("the holder sent no child to start"));
        };
        let [slave, _lock, _master] = <[OwnedFd; 3]>::try_from(fds)
            .map_err(|_| invalid("the child came without its terminal, lock and master"))?;
        let command = yagura_os::pty::LeaderCommand { argv, cwd, env };
        let leader = yagura_os::pty::start_leader(&command, slave)?;
        std::thread::sleep(DELAY);
        let start = yagura_os::process_start(leader)
            .ok()
            .flatten()
            .map(|start| start.to_string());
        send(&socket, &WatcherFrame::LeaderStarted { pid: leader, start })?;
        os::wait_for_exit_without_reaping(leader)?;
        let code = os::unreaped_exit(leader)?.and_then(|code| code);
        os::reap_exited_child(leader, &ReapGate::new())?;
        send(&socket, &WatcherFrame::LeaderExited { code })?;
        send(&socket, &WatcherFrame::SweepDone { remaining: 0 })
    }
}

#[cfg(unix)]
fn main() -> std::process::ExitCode {
    fixture::run()
}

/// A Windows window has no watcher.
#[cfg(not(unix))]
fn main() -> std::process::ExitCode {
    std::process::ExitCode::from(2)
}
