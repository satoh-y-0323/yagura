#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// What a child inherits is a property of a real spawn, so each test starts
// this same binary as its own child through the function under test.
#![allow(clippy::disallowed_methods)]

//! A child started by `yagura_os::spawn_captured` or
//! `yagura_os::spawn_daemon_detached` receives its standard handles and
//! nothing else: an fd (Unix) or handle (Windows) the caller holds without
//! naming it for the child must not reach it.
//!
//! Each test opens two objects in the parent -- one the child could inherit
//! (no `FD_CLOEXEC` / an inheritable handle) and one it cannot (`FD_CLOEXEC` /
//! a non-inheritable handle) -- and re-invokes this binary with `--exact` and
//! the test's own name. Every input reaches the child through argv (libtest
//! treats the extra free arguments as filters that match nothing). The child
//! reports, for each object, whether its fd number or handle value is open in
//! the child *and* refers to that same object; a number reused for another
//! object counts as closed. The non-inheritable object must always come back
//! closed, which shows the probe can answer both ways.
//!
//! Tests in this binary run concurrently and each child may also receive the
//! other tests' objects, but a probe judges only the numbers it was given
//! against those objects' own identity, so another test cannot change its
//! verdict.

use std::time::Duration;

/// Free argument that turns a re-invocation of a test into its probe child.
const CHILD_MARKER: &str = "yagura-fd-probe-child";
/// Argument prefixes carrying the two objects and the report destination.
const INHERITABLE_ARG: &str = "inheritable=";
const NON_INHERITABLE_ARG: &str = "non-inheritable=";
const REPORT_ARG: &str = "report=";
/// Only a hang guard: every wait below returns as soon as the child reports.
const HANG_GUARD: Duration = Duration::from_secs(30);

/// Whether one parent object reached the child.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    Closed,
    Leaked,
}

impl Verdict {
    fn as_str(self) -> &'static str {
        match self {
            Self::Closed => "closed",
            Self::Leaked => "leaked",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        match text {
            "closed" => Some(Self::Closed),
            "leaked" => Some(Self::Leaked),
            _ => None,
        }
    }
}

fn is_probe_child() -> bool {
    std::env::args().any(|arg| arg == CHILD_MARKER)
}

fn child_arg(prefix: &str) -> Result<String, String> {
    std::env::args()
        .find_map(|arg| arg.strip_prefix(prefix).map(str::to_owned))
        .ok_or_else(|| format!("missing argument {prefix}"))
}

/// The line a detached child writes: both verdicts, or the probe's error.
fn report_line(verdicts: &Result<(Verdict, Verdict), String>) -> String {
    match verdicts {
        Ok((inheritable, non_inheritable)) => {
            format!("{} {}", inheritable.as_str(), non_inheritable.as_str())
        }
        Err(error) => format!("error {error}"),
    }
}

/// Parses [`report_line`]'s output, panicking with the child's own words on a
/// probe error so it is never mistaken for a verdict.
fn parse_report(line: &str) -> (Verdict, Verdict) {
    let mut words = line.split_whitespace();
    match (
        words.next().and_then(Verdict::parse),
        words.next().and_then(Verdict::parse),
    ) {
        (Some(inheritable), Some(non_inheritable)) => (inheritable, non_inheritable),
        _ => panic!("the probe child failed instead of answering: {line:?}"),
    }
}

fn assert_verdicts(function: &str, inheritable: Verdict, non_inheritable: Verdict) {
    assert_eq!(
        non_inheritable,
        Verdict::Closed,
        "probe sanity: an object the child cannot inherit must come back closed from {function}"
    );
    assert_eq!(
        inheritable,
        Verdict::Closed,
        "a child started by {function} received an object the caller never named for it"
    );
}

#[cfg(unix)]
mod unix {
    use std::os::fd::RawFd;

    use super::{Verdict, child_arg};

    /// A pipe whose read end is the probed object; the write end is always
    /// `FD_CLOEXEC`. Both ends are closed on drop.
    pub struct ProbePipe {
        pub read: RawFd,
        write: RawFd,
    }

    impl ProbePipe {
        pub fn new(read_cloexec: bool) -> Self {
            let mut fds = [0 as RawFd; 2];
            // SAFETY: `fds` is a live two-element array, as pipe(2) requires.
            assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0, "pipe(2)");
            let pipe = Self {
                read: fds[0],
                write: fds[1],
            };
            set_cloexec(pipe.write);
            if read_cloexec {
                set_cloexec(pipe.read);
            }
            pipe
        }

        pub fn arg(&self, prefix: &str) -> String {
            let (dev, ino) = identity(self.read).expect("fstat of a pipe this test just opened");
            format!("{prefix}{}:{dev}:{ino}", self.read)
        }

        pub fn identity(&self) -> (u64, u64) {
            identity(self.read).expect("fstat of a pipe this test just opened")
        }
    }

    impl Drop for ProbePipe {
        fn drop(&mut self) {
            // SAFETY: both fds were returned by pipe(2) and are closed only here.
            unsafe {
                libc::close(self.read);
                libc::close(self.write);
            }
        }
    }

    fn set_cloexec(fd: RawFd) {
        // SAFETY: `fd` is an open fd owned by the caller; F_SETFD only sets its flag.
        assert_eq!(
            unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) },
            0,
            "fcntl(F_SETFD)"
        );
    }

    /// `(st_dev, st_ino)` of `fd`, or `None` when the fd is not open.
    fn identity(fd: RawFd) -> Option<(u64, u64)> {
        // SAFETY: an all-zero `stat` is a valid value for fstat to overwrite.
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: `stat` is a live out-parameter; an fd that is not open only
        // makes fstat return -1.
        if unsafe { libc::fstat(fd, &mut stat) } != 0 {
            return None;
        }
        // The field types differ between Linux and macOS.
        #[allow(clippy::useless_conversion, clippy::unnecessary_cast)]
        let key = (stat.st_dev as u64, u64::from(stat.st_ino));
        Some(key)
    }

    fn verdict(spec: &str) -> Result<Verdict, String> {
        let mut parts = spec.split(':');
        let mut next = || {
            parts
                .next()
                .ok_or_else(|| format!("malformed probe argument {spec:?}"))
        };
        let fd: RawFd = next()?.parse().map_err(|_| format!("bad fd in {spec:?}"))?;
        let dev: u64 = next()?
            .parse()
            .map_err(|_| format!("bad dev in {spec:?}"))?;
        let ino: u64 = next()?
            .parse()
            .map_err(|_| format!("bad ino in {spec:?}"))?;
        Ok(if identity(fd) == Some((dev, ino)) {
            Verdict::Leaked
        } else {
            Verdict::Closed
        })
    }

    /// The child side: both verdicts, for the inheritable and the
    /// `FD_CLOEXEC` pipe in that order.
    pub fn probe() -> Result<(Verdict, Verdict), String> {
        Ok((
            verdict(&child_arg(super::INHERITABLE_ARG)?)?,
            verdict(&child_arg(super::NON_INHERITABLE_ARG)?)?,
        ))
    }

    /// Two pipes open at once must have distinct identities, or the probe
    /// could not tell one object from another.
    pub fn assert_distinct(first: &ProbePipe, second: &ProbePipe) {
        assert_ne!(
            first.identity(),
            second.identity(),
            "two distinct pipes share (st_dev, st_ino); the probe's identity key cannot tell them apart"
        );
    }
}

/// Exit codes of the `spawn_captured` probe child: one per pair of verdicts,
/// and one for a probe that could not answer.
#[cfg(unix)]
const CAPTURED_CODE_BASE: i32 = 40;
#[cfg(unix)]
const CAPTURED_PROBE_ERROR: i32 = 30;

#[cfg(unix)]
fn captured_code(inheritable: Verdict, non_inheritable: Verdict) -> i32 {
    CAPTURED_CODE_BASE
        + 2 * i32::from(inheritable == Verdict::Leaked)
        + i32::from(non_inheritable == Verdict::Leaked)
}

#[cfg(unix)]
fn decode_captured_code(code: i32) -> Option<(Verdict, Verdict)> {
    let of = |leaked: bool| {
        if leaked {
            Verdict::Leaked
        } else {
            Verdict::Closed
        }
    };
    let bits = code.checked_sub(CAPTURED_CODE_BASE)?;
    (0..4)
        .contains(&bits)
        .then(|| (of(bits & 2 != 0), of(bits & 1 != 0)))
}

#[cfg(unix)]
const CAPTURED_TEST: &str = "spawn_captured_child_inherits_no_fd_it_was_not_given";

#[cfg(unix)]
#[test]
fn spawn_captured_child_inherits_no_fd_it_was_not_given() {
    use yagura_os::{CaptureEnv, CaptureLimits, CaptureOutcome};

    if is_probe_child() {
        let code = match unix::probe() {
            Ok((inheritable, non_inheritable)) => captured_code(inheritable, non_inheritable),
            Err(_) => CAPTURED_PROBE_ERROR,
        };
        std::process::exit(code);
    }

    let inheritable = unix::ProbePipe::new(false);
    let non_inheritable = unix::ProbePipe::new(true);
    unix::assert_distinct(&inheritable, &non_inheritable);
    let exe = std::env::current_exe().expect("current_exe of a running test binary");
    let argv = vec![
        exe.to_str().expect("utf8 test binary path").to_owned(),
        "--exact".to_owned(),
        CAPTURED_TEST.to_owned(),
        CHILD_MARKER.to_owned(),
        inheritable.arg(INHERITABLE_ARG),
        non_inheritable.arg(NON_INHERITABLE_ARG),
    ];

    let child = yagura_os::spawn_captured(&argv, None, CaptureEnv::Inherit(&[]), None)
        .expect("spawn_captured should start this test binary");
    let outcome = child.collect(CaptureLimits {
        timeout: HANG_GUARD,
        max_output_bytes: 1 << 20,
    });
    let (code, stdout) = match outcome {
        CaptureOutcome::Exited {
            code: Some(code),
            stdout,
            ..
        } => (code, stdout),
        other => panic!("the probe child did not exit with a code: {other:?}"),
    };
    let Some((inheritable_verdict, non_inheritable_verdict)) = decode_captured_code(code) else {
        panic!(
            "the probe child failed instead of answering (exit code {code}): {}",
            String::from_utf8_lossy(&stdout)
        );
    };
    assert_verdicts(
        "spawn_captured",
        inheritable_verdict,
        non_inheritable_verdict,
    );
}

#[cfg(unix)]
const DETACHED_UNIX_TEST: &str = "spawn_daemon_detached_child_inherits_no_fd_it_was_not_given";

#[cfg(unix)]
#[test]
fn spawn_daemon_detached_child_inherits_no_fd_it_was_not_given() {
    use std::ffi::OsStr;
    use std::sync::mpsc;

    if is_probe_child() {
        let line = report_line(&unix::probe());
        if let Ok(fifo) = child_arg(REPORT_ARG) {
            let _ = std::fs::write(fifo, format!("{line}\n"));
        }
        std::process::exit(0);
    }

    let dir = yagura_test_support::paths::short_tmp_dir("fd-inherit", "detached");
    let fifo = dir.path().join("report");
    let fifo_c = std::ffi::CString::new(fifo.to_str().expect("utf8 path")).expect("no NUL");
    // SAFETY: `fifo_c` is a NUL-terminated path that outlives the call.
    assert_eq!(unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) }, 0, "mkfifo");

    let inheritable = unix::ProbePipe::new(false);
    let non_inheritable = unix::ProbePipe::new(true);
    unix::assert_distinct(&inheritable, &non_inheritable);
    let exe = std::env::current_exe().expect("current_exe of a running test binary");
    let report_arg = format!("{REPORT_ARG}{}", fifo.to_str().expect("utf8 path"));
    let inheritable_arg = inheritable.arg(INHERITABLE_ARG);
    let non_inheritable_arg = non_inheritable.arg(NON_INHERITABLE_ARG);
    let args = [
        OsStr::new("--exact"),
        OsStr::new(DETACHED_UNIX_TEST),
        OsStr::new(CHILD_MARKER),
        OsStr::new(&inheritable_arg),
        OsStr::new(&non_inheritable_arg),
        OsStr::new(&report_arg),
    ];
    yagura_os::spawn_daemon_detached(&exe, &args)
        .expect("spawn_daemon_detached should start this test binary");

    let (tx, rx) = mpsc::channel();
    let fifo_path = fifo.clone();
    std::thread::spawn(move || {
        let _ = tx.send(std::fs::read_to_string(fifo_path));
    });
    // The child exits right after writing, and the library's reaper thread
    // reaps it, so nothing is left to wait for once the report is in.
    let line = rx
        .recv_timeout(HANG_GUARD)
        .expect("the detached probe child never reported")
        .expect("read the report fifo");
    let (inheritable_verdict, non_inheritable_verdict) = parse_report(&line);
    assert_verdicts(
        "spawn_daemon_detached",
        inheritable_verdict,
        non_inheritable_verdict,
    );
}

#[cfg(windows)]
mod windows_probe {
    use windows::Win32::Foundation::{
        CloseHandle, CompareObjectHandles, GetHandleInformation, HANDLE,
    };
    use windows::Win32::Security::SECURITY_ATTRIBUTES;
    use windows::Win32::System::Threading::{
        CreateEventW, OpenEventW, SYNCHRONIZATION_SYNCHRONIZE,
    };
    use windows::core::PCWSTR;

    use super::{Verdict, child_arg};

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// A uniquely named event this test owns; closed on drop.
    pub struct ProbeEvent {
        handle: HANDLE,
        name: String,
    }

    impl ProbeEvent {
        pub fn new(inheritable: bool, tag: &str) -> Self {
            let name = format!(
                "Local\\yagura-fd-probe-{}-{tag}",
                yagura_test_support::paths::unique_suffix()
            );
            let attributes = SECURITY_ATTRIBUTES {
                nLength: u32::try_from(std::mem::size_of::<SECURITY_ATTRIBUTES>())
                    .expect("SECURITY_ATTRIBUTES is small"),
                lpSecurityDescriptor: std::ptr::null_mut(),
                bInheritHandle: inheritable.into(),
            };
            let name_wide = wide(&name);
            // SAFETY: `attributes` and `name_wide` outlive the call; the
            // returned handle is owned by this value and closed once on drop.
            let handle =
                unsafe { CreateEventW(Some(&attributes), true, false, PCWSTR(name_wide.as_ptr())) }
                    .expect("CreateEventW should create a fresh named event");
            Self { handle, name }
        }

        pub fn arg(&self, prefix: &str) -> String {
            format!("{prefix}{}:{}", self.handle.0, self.name)
        }
    }

    impl Drop for ProbeEvent {
        fn drop(&mut self) {
            // SAFETY: the handle came from CreateEventW and is closed only here.
            let _ = unsafe { CloseHandle(self.handle) };
        }
    }

    fn verdict(spec: &str) -> Result<Verdict, String> {
        let (value, name) = spec
            .split_once(':')
            .ok_or_else(|| format!("malformed probe argument {spec:?}"))?;
        let value: isize = value
            .parse()
            .map_err(|_| format!("bad handle value in {spec:?}"))?;
        let given = HANDLE(value);
        let mut flags = 0u32;
        // Checked before anything is opened here, so the value cannot be one
        // this child allocated itself.
        // SAFETY: `flags` is a live out-parameter; an invalid handle value
        // only makes the call fail.
        if unsafe { GetHandleInformation(given, &mut flags) }.is_err() {
            return Ok(Verdict::Closed);
        }
        let name_wide = wide(name);
        // SAFETY: `name_wide` outlives the call; the handle is closed below.
        let opened = unsafe {
            OpenEventW(
                SYNCHRONIZATION_SYNCHRONIZE,
                false,
                PCWSTR(name_wide.as_ptr()),
            )
        }
        .map_err(|error| format!("OpenEventW({name}) failed: {error}"))?;
        // SAFETY: both are handle values of this process; comparing them
        // reads only the kernel objects they refer to.
        let same = unsafe { CompareObjectHandles(given, opened) }.as_bool();
        // SAFETY: `opened` came from OpenEventW above and is closed once.
        let _ = unsafe { CloseHandle(opened) };
        Ok(if same {
            Verdict::Leaked
        } else {
            Verdict::Closed
        })
    }

    /// The child side: both verdicts, for the inheritable and the
    /// non-inheritable event in that order.
    pub fn probe() -> Result<(Verdict, Verdict), String> {
        Ok((
            verdict(&child_arg(super::INHERITABLE_ARG)?)?,
            verdict(&child_arg(super::NON_INHERITABLE_ARG)?)?,
        ))
    }
}

#[cfg(windows)]
const DETACHED_WINDOWS_TEST: &str =
    "spawn_daemon_detached_child_inherits_no_handle_it_was_not_given";

#[cfg(windows)]
#[test]
fn spawn_daemon_detached_child_inherits_no_handle_it_was_not_given() {
    use std::ffi::OsStr;
    use std::time::Instant;

    if is_probe_child() {
        let line = report_line(&windows_probe::probe());
        if let Ok(report) = child_arg(REPORT_ARG) {
            // Written whole, then renamed, so the parent never reads half a line.
            let partial = format!("{report}.partial");
            if std::fs::write(&partial, &line).is_ok() {
                let _ = std::fs::rename(&partial, &report);
            }
        }
        std::process::exit(0);
    }

    let dir = yagura_test_support::paths::TempDir::new("fd-inherit", "detached");
    let report = dir.path().join("report");
    let inheritable = windows_probe::ProbeEvent::new(true, "inheritable");
    let non_inheritable = windows_probe::ProbeEvent::new(false, "non-inheritable");
    let exe = std::env::current_exe().expect("current_exe of a running test binary");
    let report_arg = format!("{REPORT_ARG}{}", report.to_str().expect("utf8 path"));
    let inheritable_arg = inheritable.arg(INHERITABLE_ARG);
    let non_inheritable_arg = non_inheritable.arg(NON_INHERITABLE_ARG);
    let args = [
        OsStr::new("--exact"),
        OsStr::new(DETACHED_WINDOWS_TEST),
        OsStr::new(CHILD_MARKER),
        OsStr::new(&inheritable_arg),
        OsStr::new(&non_inheritable_arg),
        OsStr::new(&report_arg),
    ];
    yagura_os::spawn_daemon_detached(&exe, &args)
        .expect("spawn_daemon_detached should start this test binary");

    // The child exits right after the rename; the deadline is only a hang guard.
    let deadline = Instant::now() + HANG_GUARD;
    let line = loop {
        if let Ok(line) = std::fs::read_to_string(&report) {
            break line;
        }
        assert!(
            Instant::now() < deadline,
            "the detached probe child never reported"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    let (inheritable_verdict, non_inheritable_verdict) = parse_report(&line);
    assert_verdicts(
        "spawn_daemon_detached",
        inheritable_verdict,
        non_inheritable_verdict,
    );
}
