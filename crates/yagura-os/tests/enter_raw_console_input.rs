#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// Observing the no-op contract needs a process whose own stdin is a pipe,
// so this test spawns a real child of its own.
#![allow(clippy::disallowed_methods)]

//! Unit test for `yagura_os::enter_raw_console_input`'s no-op contract on a
//! non-console standard input.
//!
//! `enter_raw_console_input` mutates the *calling process's own* stdin
//! (`GetStdHandle(STD_INPUT_HANDLE)` on Windows, `libc::STDIN_FILENO` on
//! Unix), so observing "stdin is not a console -> `Ok(())`, no mode change"
//! requires a process whose own stdin really is not a console. A `cargo
//! test` process's stdin is not reliably that (it may be a real console
//! when the suite is run interactively), so this test spawns its own
//! compiled test binary as a child with a piped stdin (a pipe is never a
//! console) and an exact-name filter selecting the one test below. The
//! child recognizes [`CHILD_MODE_ENV`], calls the function once, and
//! reports the outcome through its exit code alone -- not through captured
//! stdout, so the parent's check does not depend on `--nocapture`.

use std::env;
use std::process::{Command, Stdio};

/// Name of the one test this file re-invokes as its own child (see the
/// module doc). Kept as a constant so the `--exact` filter and the `#[test]`
/// item name cannot drift apart silently.
const TEST_NAME: &str = "enter_raw_console_input_is_a_no_op_ok_when_stdin_is_not_a_console";

/// Guard environment variable that switches this test's own re-invocation
/// from "drive the child" (unset) to "be the child" (set): the child calls
/// `enter_raw_console_input` exactly once and exits without spawning a
/// grandchild.
const CHILD_MODE_ENV: &str = "YAGURA_OS_ENTER_RAW_CONSOLE_INPUT_CHILD";

#[test]
fn enter_raw_console_input_is_a_no_op_ok_when_stdin_is_not_a_console() {
    if env::var(CHILD_MODE_ENV).is_ok() {
        // Running as the spawned child below: stdin was piped by the
        // parent, which is never a console, regardless of how the parent's
        // own stdin was set up.
        let result = yagura_os::enter_raw_console_input();
        std::process::exit(i32::from(result.is_err()));
    }

    let exe = env::current_exe().expect("current_exe should resolve for a running test binary");
    let status = Command::new(exe)
        .arg("--exact")
        .arg(TEST_NAME)
        .env(CHILD_MODE_ENV, "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("spawn the child re-invocation of this same test binary");

    assert!(
        status.success(),
        "enter_raw_console_input must return Ok(()) (a no-op) when stdin is a pipe rather \
         than a console; child exit status: {status:?}"
    );
}

/// Name of the Unix test below, re-invoked with its standard input on a
/// pseudoterminal the parent keeps the other end of.
#[cfg(unix)]
const RAW_TEST_NAME: &str = "raw_mode_clears_the_listed_settings_and_keeps_every_other_one";

#[cfg(unix)]
const RAW_CHILD_MODE_ENV: &str = "YAGURA_OS_ENTER_RAW_CONSOLE_INPUT_ON_A_TERMINAL_CHILD";

#[cfg(unix)]
fn terminal_attributes(fd: std::os::fd::RawFd) -> libc::termios {
    let mut attributes = std::mem::MaybeUninit::<libc::termios>::uninit();
    // SAFETY: `fd` is an open terminal and the pointer is to stack storage
    // sized for the struct tcgetattr fills.
    assert_eq!(unsafe { libc::tcgetattr(fd, attributes.as_mut_ptr()) }, 0);
    // SAFETY: tcgetattr returned success, so it initialized the struct.
    unsafe { attributes.assume_init() }
}

// Every setting the function clears starts out set, and some it must leave
// alone are set too, so both a flag it misses and a flag it touches show.
#[cfg(unix)]
#[test]
fn raw_mode_clears_the_listed_settings_and_keeps_every_other_one() {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

    if env::var(RAW_CHILD_MODE_ENV).is_ok() {
        let result = yagura_os::enter_raw_console_input();
        std::process::exit(i32::from(result.is_err()));
    }

    let (mut master, mut slave) = (0, 0);
    // SAFETY: both out-parameters are live locals, and the name, termios and
    // winsize pointers may be null.
    let opened = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    assert_eq!(opened, 0, "openpty: {}", std::io::Error::last_os_error());
    // SAFETY: openpty returned these two descriptors and nothing else owns them.
    let (_master, slave) = unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };

    let input_cleared = libc::IGNBRK
        | libc::BRKINT
        | libc::PARMRK
        | libc::ISTRIP
        | libc::INLCR
        | libc::IGNCR
        | libc::ICRNL
        | libc::IXON;
    let local_cleared = libc::ECHO | libc::ECHONL | libc::ICANON | libc::ISIG | libc::IEXTEN;
    let mut prepared = terminal_attributes(slave.as_raw_fd());
    prepared.c_iflag |= input_cleared | libc::IXANY;
    prepared.c_oflag |= libc::OPOST;
    prepared.c_lflag |= local_cleared | libc::ECHOK;
    prepared.c_cflag =
        (prepared.c_cflag & !libc::CSIZE) | libc::CS7 | libc::PARENB | libc::CREAD | libc::HUPCL;
    prepared.c_cc[libc::VMIN] = 0;
    prepared.c_cc[libc::VTIME] = 5;
    // SAFETY: `slave` is an open terminal and `prepared` a fully initialized struct.
    assert_eq!(
        unsafe { libc::tcsetattr(slave.as_raw_fd(), libc::TCSANOW, &prepared) },
        0
    );
    let before = terminal_attributes(slave.as_raw_fd());

    let exe = env::current_exe().expect("current_exe should resolve for a running test binary");
    let status = Command::new(exe)
        .arg("--exact")
        .arg(RAW_TEST_NAME)
        .env(RAW_CHILD_MODE_ENV, "1")
        .stdin(Stdio::from(
            slave.try_clone().expect("duplicate the terminal"),
        ))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("spawn the child re-invocation of this same test binary");
    assert!(status.success(), "child exit status: {status:?}");

    let after = terminal_attributes(slave.as_raw_fd());
    assert_eq!(after.c_iflag, before.c_iflag & !input_cleared, "c_iflag");
    assert_eq!(after.c_oflag, before.c_oflag, "c_oflag");
    assert_eq!(after.c_lflag, before.c_lflag & !local_cleared, "c_lflag");
    assert_eq!(
        after.c_cflag,
        (before.c_cflag & !(libc::CSIZE | libc::PARENB)) | libc::CS8,
        "c_cflag"
    );
    assert_eq!(
        (after.c_cc[libc::VMIN], after.c_cc[libc::VTIME]),
        (1, 0),
        "VMIN, VTIME"
    );
}
