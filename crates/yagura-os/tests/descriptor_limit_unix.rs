#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// The fixture is this binary started again as its own child, so it is spawned directly.
#![allow(clippy::disallowed_methods)]

//! A process that raises its own descriptor limit does not hand the raised limit
//! to the children it starts through the OS layer's spawns: each starts with the
//! soft `RLIMIT_NOFILE` the process had before raising it, as a program written
//! against the default limit (one that sizes a table by it, or calls `select`)
//! expects.
//!
//! The limit is changed in a fixture process, never in the test process: the
//! fixture lowers its soft limit, raises it, starts children and reports the
//! limit each child reads for itself. A child is a shell asking for its own soft
//! limit, so what it reports is what it inherited rather than what a runtime may
//! raise at startup.

use std::io::{BufRead as _, BufReader, Read as _};
use std::process::{Command, Stdio};

use yagura_test_support::process::libtest_name;

const FIXTURE: &str = "descriptor_limit_fixture";
const TAG: &str = "@limit ";
/// The soft limit the fixture lowers itself to before raising: below every
/// default a platform ships, and well above what the fixture has open.
const LOWERED: libc::rlim_t = 128;
const SHELL: &str = "/bin/sh";
const ASK_SOFT_LIMIT: &str = "ulimit -S -n";

fn say(key: &str, value: impl std::fmt::Display) {
    println!("{TAG}{key} {value}");
}

fn soft_limit_here() -> (libc::rlim_t, libc::rlim_t) {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: `limit` is a live out-parameter of the type getrlimit(2) writes.
    assert_eq!(
        unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) },
        0
    );
    (limit.rlim_cur, limit.rlim_max)
}

/// The fixture: lowers its soft limit, raises it as a daemon does at startup,
/// then reports the limit its own and each child's view of it.
#[test]
#[ignore = "the process of the descriptor-limit test"]
fn descriptor_limit_fixture() {
    use std::ffi::OsStr;
    use yagura_os::platform_unix::raise_descriptor_limit;
    use yagura_os::{CaptureEnv, CaptureLimits, CaptureOutcome, DetachedStderr};

    if std::env::var_os("YAGURA_OS_DESCRIPTOR_LIMIT_FIXTURE").is_none() {
        return;
    }
    let (_, hard) = soft_limit_here();
    say("hard", hard);
    let lowered = libc::rlimit {
        rlim_cur: LOWERED,
        rlim_max: hard,
    };
    // SAFETY: `lowered` is a live local of the type setrlimit(2) reads.
    assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &lowered) }, 0);
    say("lowered", soft_limit_here().0);
    say("raised", raise_descriptor_limit().unwrap());
    say("own", soft_limit_here().0);

    let mut detached = yagura_os::spawn_detached_process(
        SHELL.as_ref(),
        &[OsStr::new("-c"), OsStr::new(ASK_SOFT_LIMIT)],
        None,
        DetachedStderr::Null,
    )
    .unwrap();
    let mut reported = String::new();
    detached
        .take_stdout()
        .unwrap()
        .read_to_string(&mut reported)
        .unwrap();
    detached.wait().unwrap();
    say("detached-child", reported.trim());

    let argv = [SHELL.to_owned(), "-c".to_owned(), ASK_SOFT_LIMIT.to_owned()];
    let outcome = yagura_os::spawn_captured(&argv, None, CaptureEnv::Inherit(&[]), None)
        .unwrap()
        .collect(CaptureLimits {
            timeout: yagura_test_support::HANG_GUARD,
            max_output_bytes: 4096,
        });
    match outcome {
        CaptureOutcome::Exited { stdout, .. } => {
            say("captured-child", String::from_utf8_lossy(&stdout).trim());
        }
        other => panic!("the captured child did not exit: {other:?}"),
    }
}

fn report() -> std::collections::HashMap<String, String> {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            libtest_name(module_path!(), FIXTURE).as_str(),
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("YAGURA_OS_DESCRIPTOR_LIMIT_FIXTURE", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut fixture = command.spawn().unwrap();
    let out = fixture.stdout.take().unwrap();
    let mut err = fixture.stderr.take().unwrap();
    let said = std::thread::spawn(move || {
        let mut said = String::new();
        let _ = err.read_to_string(&mut said);
        said
    });
    let mut lines = std::collections::HashMap::new();
    for line in BufReader::new(out).lines().map_while(Result::ok) {
        // libtest may print the test's name on the same line first.
        if let Some(at) = line.find(TAG)
            && let Some((key, value)) = line[at + TAG.len()..].split_once(' ')
        {
            lines.insert(key.to_owned(), value.to_owned());
        }
    }
    let status = fixture.wait().unwrap();
    let said = said.join().unwrap();
    assert!(
        status.success(),
        "the fixture failed: {status:?}\n{said}\n{lines:?}"
    );
    lines
}

fn number(lines: &std::collections::HashMap<String, String>, key: &str) -> libc::rlim_t {
    lines
        .get(key)
        .unwrap_or_else(|| panic!("the fixture reported no {key}: {lines:?}"))
        .parse()
        .unwrap_or_else(|_| panic!("the fixture's {key} is not a number: {lines:?}"))
}

/// Children started after the process raised its limit read the limit it had
/// before.
#[test]
fn a_spawned_child_starts_with_the_soft_limit_from_before_the_parent_raised_it() {
    let lines = report();
    assert_eq!(number(&lines, "lowered"), LOWERED, "{lines:?}");
    // The raise must have happened, or the children's value proves nothing.
    assert!(
        number(&lines, "raised") > LOWERED && number(&lines, "own") > LOWERED,
        "the fixture could not raise its soft limit above {LOWERED}: {lines:?}"
    );
    for child in ["detached-child", "captured-child"] {
        assert_eq!(
            number(&lines, child),
            LOWERED,
            "the {child} inherited the raised limit instead of the one from before: {lines:?}"
        );
    }
}
