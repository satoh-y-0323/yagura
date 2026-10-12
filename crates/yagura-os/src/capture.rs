//! One-shot, non-PTY process capture: run a program to completion while
//! reading its stdout, and guarantee its whole descendant tree is torn down
//! by the time the caller is done with it -- whether that is a collected
//! result, a limit being hit, or the value simply being dropped.
//!
//! Every spawn here is placed in a [`ProcessGroup`] of its own from the start,
//! so tearing the whole tree down is just dropping that group -- see [`CapturedChild::collect`],
//! [`CapturedChild::collect_until`] and the `Drop` implementation. That only
//! reaches descendants still inside the group / job: one that breaks away
//! (a background job started by a login shell's rc file, for example) keeps
//! running and keeps its end of the stdout pipe open on its own, which is
//! exactly the case a caller's own [`CaptureLimits::timeout`] -- not this
//! module -- bounds the wait against.
//!
//! stderr is always piped and read on a thread of its own, so a child that
//! writes a lot to it never blocks on a full pipe; stdout alone decides the
//! outcome, and stderr is returned alongside it, capped at
//! [`CaptureLimits::max_output_bytes`].

use std::ffi::OsString;
use std::io;
use std::path::Path;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use crate::{OsError, ProcessGroup};

#[cfg(windows)]
type OsChild = crate::platform_windows::ChildProcess;
#[cfg(windows)]
type OsStdin = std::fs::File;
#[cfg(windows)]
type OsStdout = std::fs::File;
#[cfg(windows)]
type OsStderr = std::fs::File;
#[cfg(not(windows))]
type OsChild = std::process::Child;
#[cfg(not(windows))]
type OsStdin = std::process::ChildStdin;
#[cfg(not(windows))]
type OsStdout = std::process::ChildStdout;
#[cfg(not(windows))]
type OsStderr = std::process::ChildStderr;

/// A captured child's exit code, `None` when a signal ended it (Unix only).
type ExitCode = Option<i32>;

/// What [`CapturedChild`] needs from the platform's own child type.
trait WaitExit {
    fn wait_exit(&mut self) -> io::Result<ExitCode>;
    fn try_wait_exit(&mut self) -> io::Result<Option<ExitCode>>;
}

#[cfg(windows)]
impl WaitExit for OsChild {
    // Windows exit codes are `u32`; they are reported as the `i32` std uses.
    fn wait_exit(&mut self) -> io::Result<ExitCode> {
        self.wait().map(|code| Some(code as i32))
    }

    fn try_wait_exit(&mut self) -> io::Result<Option<ExitCode>> {
        self.try_wait()
            .map(|exit| exit.map(|code| Some(code as i32)))
    }
}

#[cfg(not(windows))]
impl WaitExit for OsChild {
    fn wait_exit(&mut self) -> io::Result<ExitCode> {
        self.wait().map(|status| status.code())
    }

    fn try_wait_exit(&mut self) -> io::Result<Option<ExitCode>> {
        self.try_wait().map(|exit| exit.map(|status| status.code()))
    }
}

/// The parent's ends of a captured child's standard handles.
pub(crate) struct CapturePipes {
    pub(crate) stdout: Option<OsStdout>,
    pub(crate) stderr: Option<OsStderr>,
    /// Present only when the child's stdin is a pipe fed a payload.
    pub(crate) stdin: Option<OsStdin>,
}

/// Chunk size used while draining a captured child's stdout.
const READ_CHUNK_SIZE: usize = 8192;

/// How often [`CapturedChild::collect_until`] re-polls the direct child for
/// its own exit after the end marker was found, since a child's own wait has
/// no bounded form.
const CHILD_EXIT_POLL_INTERVAL: Duration = Duration::from_millis(10);

/// Limits [`CapturedChild::collect`] and [`CapturedChild::collect_until`]
/// enforce while reading their child's stdout.
#[derive(Debug, Clone, Copy)]
pub struct CaptureLimits {
    /// How long to wait for the child to finish (or, for
    /// [`CapturedChild::collect_until`], to reach the end marker and then let
    /// the direct child exit on its own) before giving up.
    pub timeout: Duration,
    /// The largest stdout size accepted before the whole descendant tree is
    /// torn down instead of waiting any further. Also the most stderr kept:
    /// stderr past it is read and discarded, never an overrun.
    pub max_output_bytes: usize,
}

/// The environment a captured child starts with, handed to
/// [`spawn_captured`].
///
/// `Copy` and its `Debug` never shows a name or a value: an environment
/// routinely carries tokens.
#[derive(Clone, Copy)]
pub enum CaptureEnv<'a> {
    /// The calling process's own environment, with `vars` layered on top (a
    /// later entry for a name given more than once wins).
    Inherit(&'a [(OsString, OsString)]),
    /// Only `vars`; nothing inherited from the calling process.
    Replace(&'a [(OsString, OsString)]),
}

impl std::fmt::Debug for CaptureEnv<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Inherit(vars) => f.debug_tuple("Inherit").field(&vars.len()).finish(),
            Self::Replace(vars) => f.debug_tuple("Replace").field(&vars.len()).finish(),
        }
    }
}

/// Result of [`CapturedChild::collect`].
#[derive(Debug)]
pub enum CaptureOutcome {
    /// The child exited on its own, within both limits.
    Exited {
        /// The exit code, or `None` when the process was terminated by a
        /// signal (Unix only).
        code: Option<i32>,
        /// Everything the child wrote to stdout.
        stdout: Vec<u8>,
        /// What the child wrote to stderr, up to
        /// [`CaptureLimits::max_output_bytes`], read until stderr's end of
        /// file or [`CaptureLimits::timeout`], whichever came first.
        stderr: Vec<u8>,
    },
    /// Neither an exit nor the output limit was reached within
    /// [`CaptureLimits::timeout`]; the whole descendant tree was torn down.
    TimedOut,
    /// The child wrote more than [`CaptureLimits::max_output_bytes`] to
    /// stdout before finishing; the whole descendant tree was torn down.
    OutputTooLarge,
}

/// Result of [`CapturedChild::collect_until`].
#[derive(Debug)]
pub enum MarkerOutcome {
    /// The end marker was found in the child's stdout.
    Reached {
        /// Everything read up to and including the end marker; whatever the
        /// child wrote afterward is drained and discarded, never kept here.
        stdout: Vec<u8>,
        /// Whether the direct child went on to exit on its own within the
        /// time remaining after the marker was found.
        child_exited: bool,
        /// The direct child's exit code, when `child_exited` is true.
        code: Option<i32>,
    },
    /// The child exited before writing the end marker.
    Exited {
        /// The exit code, or `None` when terminated by a signal (Unix only).
        code: Option<i32>,
        /// Everything the child wrote to stdout before exiting.
        stdout: Vec<u8>,
        /// What the child wrote to stderr, read the same way
        /// [`CaptureOutcome::Exited`]'s is.
        stderr: Vec<u8>,
    },
    /// Neither the end marker nor an exit was seen within
    /// [`CaptureLimits::timeout`]; the whole descendant tree was torn down.
    TimedOut,
    /// The child wrote more than [`CaptureLimits::max_output_bytes`] to
    /// stdout before the end marker; the whole descendant tree was torn
    /// down.
    OutputTooLarge,
}

/// What the stdout-reading thread of [`CapturedChild::collect`] found, sent
/// back to it.
enum ReadOutcome {
    Eof(Vec<u8>),
    OverLimit,
}

/// What the stdout-reading thread of [`CapturedChild::collect_until`] found,
/// sent back to it.
enum MarkerReadOutcome {
    Marker(Vec<u8>),
    Eof(Vec<u8>),
    OverLimit,
}

/// A spawned one-shot, non-PTY process: stdin either closed or fed a fixed
/// payload, stdout and stderr both piped. Constructed only by the
/// platform-specific `spawn_captured` functions, which place the child in a
/// [`ProcessGroup`] of its own before it runs.
pub struct CapturedChild {
    child: Option<OsChild>,
    stdout: Option<OsStdout>,
    stderr: Option<OsStderr>,
    group: Option<ProcessGroup>,
}

impl std::fmt::Debug for CapturedChild {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CapturedChild").finish_non_exhaustive()
    }
}

impl CapturedChild {
    /// Assembles a [`CapturedChild`] from a just-spawned child already placed
    /// in `group`, and starts feeding it `stdin` when the child's stdin was
    /// piped for one. Only the platform `spawn_captured` functions call this.
    pub(crate) fn new(
        child: OsChild,
        group: ProcessGroup,
        pipes: CapturePipes,
        stdin: Option<Vec<u8>>,
    ) -> Self {
        if let (Some(pipe), Some(payload)) = (pipes.stdin, stdin) {
            write_stdin(pipe, payload);
        }
        Self {
            child: Some(child),
            stdout: pipes.stdout,
            stderr: pipes.stderr,
            group: Some(group),
        }
    }

    /// Reads the child's stdout to completion and waits for it to exit,
    /// enforcing both of `limits`. Blocking: run this off an async runtime
    /// (e.g. inside `spawn_blocking`) when called from async code.
    ///
    /// A timeout or an output overrun ends the whole descendant tree before
    /// returning, exactly like dropping this value early does -- see the
    /// `Drop` implementation. Once the outcome is decided this never waits on
    /// the stdout- or stderr-reading threads: a descendant that broke away
    /// from the group and still holds the write end of either pipe cannot
    /// delay the return, only the tree teardown that follows it.
    ///
    /// After stdout's end of file, stderr's end of file is waited for only
    /// within what is left of [`CaptureLimits::timeout`]; past that, the
    /// outcome is still [`CaptureOutcome::Exited`], carrying whatever stderr
    /// was read so far.
    #[must_use]
    pub fn collect(mut self, limits: CaptureLimits) -> CaptureOutcome {
        let deadline = Instant::now() + limits.timeout;
        let stderr_rx = read_stderr(self.stderr.take(), limits.max_output_bytes);

        let Some(mut child) = self.child.take() else {
            // Unreachable in practice: `new` always sets this. Handled
            // rather than panicking on an internal invariant.
            return CaptureOutcome::Exited {
                code: None,
                stdout: Vec::new(),
                stderr: Vec::new(),
            };
        };
        let Some(mut stdout) = self.stdout.take() else {
            self.end_tree();
            let code = child.wait_exit().ok().flatten();
            return CaptureOutcome::Exited {
                code,
                stdout: Vec::new(),
                stderr: gather_stderr_until(&stderr_rx, deadline),
            };
        };

        let (tx, rx) = mpsc::channel::<ReadOutcome>();
        let max_output_bytes = limits.max_output_bytes;
        thread::spawn(move || {
            use std::io::Read as _;
            let mut buf = Vec::new();
            let mut chunk = [0u8; READ_CHUNK_SIZE];
            loop {
                match stdout.read(&mut chunk) {
                    Ok(0) => {
                        let _ = tx.send(ReadOutcome::Eof(buf));
                        return;
                    }
                    Ok(n) => {
                        buf.extend_from_slice(&chunk[..n]);
                        if buf.len() > max_output_bytes {
                            let _ = tx.send(ReadOutcome::OverLimit);
                            return;
                        }
                    }
                    Err(_) => {
                        let _ = tx.send(ReadOutcome::Eof(buf));
                        return;
                    }
                }
            }
        });

        match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(ReadOutcome::Eof(stdout_bytes)) => {
                let Some(code) = wait_for_child_exit(&mut child, deadline) else {
                    self.end_tree();
                    let _ = child.wait_exit();
                    return CaptureOutcome::TimedOut;
                };
                // Ends a descendant that outlived the child, before waiting on
                // stderr, so one still inside the group cannot hold stderr open
                // past this point.
                self.end_tree();
                CaptureOutcome::Exited {
                    code,
                    stdout: stdout_bytes,
                    stderr: gather_stderr_until(&stderr_rx, deadline),
                }
            }
            Ok(ReadOutcome::OverLimit) => {
                self.end_tree();
                let _ = child.wait_exit();
                CaptureOutcome::OutputTooLarge
            }
            Err(_timeout_or_disconnected) => {
                self.end_tree();
                let _ = child.wait_exit();
                CaptureOutcome::TimedOut
            }
        }
    }

    /// Like [`CapturedChild::collect`], but completes as soon as `end_marker`
    /// appears in the child's stdout instead of waiting for end of file.
    /// Blocking: run this off an async runtime the same way `collect` is.
    ///
    /// Once the marker is found, the direct child (not necessarily every
    /// descendant) is given the remainder of [`CaptureLimits::timeout`] to
    /// exit on its own -- so a login shell's own exit processing (an `EXIT`
    /// trap, `.zlogout`) gets to run -- before the whole descendant tree is
    /// torn down either way. Anything the child writes after the marker is
    /// drained and discarded rather than left to block a full pipe or break
    /// with a broken-pipe error: the read end is never closed while this
    /// call is still waiting on the child.
    ///
    /// Returns [`MarkerOutcome::Exited`] when the child exits before writing
    /// the marker at all; its stderr is then read the way
    /// [`CapturedChild::collect`] reads it. Once the marker is found, stderr
    /// is neither waited for nor returned, so a descendant holding stderr open
    /// cannot delay a caller that is done.
    #[must_use]
    pub fn collect_until(mut self, limits: CaptureLimits, end_marker: &[u8]) -> MarkerOutcome {
        let deadline = Instant::now() + limits.timeout;
        let stderr_rx = read_stderr(self.stderr.take(), limits.max_output_bytes);

        let Some(mut child) = self.child.take() else {
            // Unreachable in practice: `new` always sets this. Handled
            // rather than panicking on an internal invariant.
            return MarkerOutcome::Exited {
                code: None,
                stdout: Vec::new(),
                stderr: Vec::new(),
            };
        };
        let Some(mut stdout) = self.stdout.take() else {
            self.end_tree();
            let code = child.wait_exit().ok().flatten();
            return MarkerOutcome::Exited {
                code,
                stdout: Vec::new(),
                stderr: gather_stderr_until(&stderr_rx, deadline),
            };
        };

        let (tx, rx) = mpsc::channel::<MarkerReadOutcome>();
        let max_output_bytes = limits.max_output_bytes;
        let marker = end_marker.to_vec();
        thread::spawn(move || {
            use std::io::Read as _;
            let mut buf = Vec::new();
            let mut chunk = [0u8; READ_CHUNK_SIZE];
            loop {
                match stdout.read(&mut chunk) {
                    Ok(0) => {
                        let _ = tx.send(MarkerReadOutcome::Eof(buf));
                        return;
                    }
                    Ok(n) => {
                        let scanned_from = buf.len();
                        buf.extend_from_slice(&chunk[..n]);
                        if buf.len() > max_output_bytes {
                            let _ = tx.send(MarkerReadOutcome::OverLimit);
                            return;
                        }
                        // Only the newly appended bytes, plus the overlap a
                        // marker split across two reads needs, are scanned --
                        // not the whole buffer on every read.
                        let scan_start =
                            scanned_from.saturating_sub(marker.len().saturating_sub(1));
                        if let Some(pos) = find_subslice(&buf[scan_start..], &marker) {
                            let marker_end = scan_start + pos + marker.len();
                            let _ = tx.send(MarkerReadOutcome::Marker(buf[..marker_end].to_vec()));
                            // Keep the read end open and draining until end of
                            // file: a child that writes more after the marker
                            // must neither block on a full pipe nor see a
                            // broken one.
                            drain_to_eof(&mut stdout);
                            return;
                        }
                    }
                    Err(_) => {
                        let _ = tx.send(MarkerReadOutcome::Eof(buf));
                        return;
                    }
                }
            }
        });

        let wait_for_marker = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(wait_for_marker) {
            Ok(MarkerReadOutcome::Marker(stdout_bytes)) => {
                // The marker's bytes are already final; this wait's outcome
                // never changes what is returned as `stdout`, only
                // `child_exited` and `code`.
                let exit_status = wait_for_child_exit(&mut child, deadline);
                self.end_tree();
                match exit_status {
                    Some(code) => MarkerOutcome::Reached {
                        stdout: stdout_bytes,
                        child_exited: true,
                        code,
                    },
                    None => {
                        let _ = child.wait_exit();
                        MarkerOutcome::Reached {
                            stdout: stdout_bytes,
                            child_exited: false,
                            code: None,
                        }
                    }
                }
            }
            Ok(MarkerReadOutcome::Eof(stdout_bytes)) => {
                let Some(code) = wait_for_child_exit(&mut child, deadline) else {
                    self.end_tree();
                    let _ = child.wait_exit();
                    return MarkerOutcome::TimedOut;
                };
                self.end_tree();
                MarkerOutcome::Exited {
                    code,
                    stdout: stdout_bytes,
                    stderr: gather_stderr_until(&stderr_rx, deadline),
                }
            }
            Ok(MarkerReadOutcome::OverLimit) => {
                self.end_tree();
                let _ = child.wait_exit();
                MarkerOutcome::OutputTooLarge
            }
            Err(_timeout_or_disconnected) => {
                self.end_tree();
                let _ = child.wait_exit();
                MarkerOutcome::TimedOut
            }
        }
    }

    /// Ends the whole descendant tree by dropping the [`ProcessGroup`] the
    /// child was placed in at spawn time -- a job object's kill-on-close
    /// limit terminates every member on Windows, and the process group's
    /// `Drop` sends `SIGKILL` to it on Unix. A no-op when already called once.
    fn end_tree(&mut self) {
        if let Some(group) = self.group.take() {
            drop(group);
        }
    }
}

impl Drop for CapturedChild {
    fn drop(&mut self) {
        self.end_tree();
        let Some(mut child) = self.child.take() else {
            return;
        };
        if matches!(child.try_wait_exit(), Ok(Some(_))) {
            return;
        }
        // Reaps on a detached thread instead of blocking here: this path only
        // runs when a caller abandoned the value instead of calling
        // `collect`, so nothing here may block them while the kill above
        // lands.
        thread::spawn(move || {
            let _ = child.wait_exit();
        });
    }
}

/// Keeps reading and discarding `pipe` until end of file or an error.
///
/// Used once nothing more from `pipe` is kept (stdout past the end marker,
/// stderr once the caller is done): the read end must stay
/// open for as long as the child may still be writing, or a further write
/// would either block on a full pipe or fail with a broken-pipe error.
fn drain_to_eof(pipe: &mut impl io::Read) {
    let mut chunk = [0u8; READ_CHUNK_SIZE];
    loop {
        match pipe.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
    }
}

/// Starts reading `stderr` on a thread of its own, sending at most
/// `max_bytes` of it in total and discarding the rest, so the child never
/// blocks on a full stderr pipe. The channel disconnects at stderr's end of
/// file (or at once when there is no pipe); the thread is never joined.
fn read_stderr(stderr: Option<OsStderr>, max_bytes: usize) -> mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    let Some(mut stderr) = stderr else {
        return rx;
    };
    thread::spawn(move || {
        use std::io::Read as _;
        let mut kept = 0usize;
        let mut chunk = [0u8; READ_CHUNK_SIZE];
        loop {
            match stderr.read(&mut chunk) {
                Ok(0) | Err(_) => return,
                Ok(n) => {
                    let take = n.min(max_bytes.saturating_sub(kept));
                    if take > 0 {
                        kept += take;
                        if tx.send(chunk[..take].to_vec()).is_err() {
                            // Nobody collects any more; keep draining so
                            // the child is not held up by a full pipe.
                            drain_to_eof(&mut stderr);
                            return;
                        }
                    }
                }
            }
        }
    });
    rx
}

/// Collects stderr chunks until the reading thread reports end of file or
/// `deadline` passes, whichever comes first.
fn gather_stderr_until(rx: &mpsc::Receiver<Vec<u8>>, deadline: Instant) -> Vec<u8> {
    let mut stderr = Vec::new();
    loop {
        match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(chunk) => stderr.extend_from_slice(&chunk),
            Err(_timeout_or_end_of_file) => return stderr,
        }
    }
}

/// Writes `payload` to the child's stdin on a thread of its own, then closes
/// it. A failed write (the child exited or closed stdin without reading it
/// all) is ignored: stdout alone decides the outcome.
fn write_stdin(mut stdin: OsStdin, payload: Vec<u8>) {
    thread::spawn(move || {
        use std::io::Write as _;
        let _ = stdin.write_all(&payload);
    });
}

/// Polls `child` for its own exit until it reports one or `deadline` passes,
/// since a child's own wait has no bounded form.
///
/// End of file on stdout is not that exit: a program may close stdout while
/// it still runs (GNU `cat` does, in its exit handler), and ending the tree
/// before it exits would kill it and lose its exit code.
fn wait_for_child_exit(child: &mut OsChild, deadline: Instant) -> Option<ExitCode> {
    loop {
        if let Ok(Some(code)) = child.try_wait_exit() {
            return Some(code);
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return None;
        }
        thread::sleep(CHILD_EXIT_POLL_INTERVAL.min(remaining));
    }
}

/// Finds the first occurrence of `needle` in `haystack`.
///
/// An empty `needle` matches at position 0 rather than panicking: `[u8]`'s
/// `windows` requires a non-zero window size.
fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    if haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Spawns `argv[0]` with `argv[1..]`, in `cwd` (the calling process's own
/// working directory when `None`) with `env` deciding what the child's
/// environment is: stdout and stderr piped, and stdin `null` when `stdin` is
/// `None`, or piped and fed those bytes (then closed) when it is `Some`. The
/// child is in a [`ProcessGroup`] of its own before it runs any code, so
/// everything it starts belongs to that group too, and
/// [`CapturedChild::collect`] (or [`CapturedChild::collect_until`], or
/// dropping the result early) can tear down the whole tree at once.
///
/// On Windows, a bare `argv[0]` is searched for only in the child's own
/// `PATH` (by [`crate::program::resolve_program`]'s rules), and the child
/// receives its three standard handles and no other handle.
///
/// # Errors
///
/// Returns whatever the platform's process spawn returns -- in particular
/// [`io::ErrorKind::NotFound`] for a program that does not exist (on
/// Windows, also a bare name absent from the child's `PATH`).
/// [`io::ErrorKind::InvalidInput`] for an empty `argv`, a NUL in an argument
/// or the environment, and on Windows for a `.cmd` / `.bat` `argv[0]`, which
/// must be wrapped by [`crate::program::into_spawn_argv`] instead. Also
/// returns an I/O error when the process group / job object cannot be
/// created, or the child cannot be placed in it (on Windows the child is
/// then terminated before it runs; on Unix it is killed and reaped).
pub fn spawn_captured(
    argv: &[String],
    cwd: Option<&Path>,
    env: CaptureEnv<'_>,
    stdin: Option<Vec<u8>>,
) -> io::Result<CapturedChild> {
    if argv.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "argv must not be empty",
        ));
    }
    #[cfg(windows)]
    {
        crate::platform_windows::spawn_captured(argv, cwd, env, stdin)
    }
    #[cfg(not(windows))]
    {
        crate::platform_unix::spawn_captured(argv, cwd, env, stdin)
    }
}

/// Converts an [`OsError`] into an [`io::Error`], for the platform
/// `spawn_captured` functions whose own contract is `io::Result`.
pub(crate) fn os_error_to_io(err: OsError) -> io::Error {
    io::Error::other(err.to_string())
}
