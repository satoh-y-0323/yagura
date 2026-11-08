#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// The wrapper below starts the built daemon, and the test reads the process table.
#![allow(clippy::disallowed_methods)]

//! A daemon started with a low soft descriptor limit still opens windows past the
//! number that limit would hold, because it raises its own limit at startup, and its
//! windows' commands still see the low limit it was started with.
//!
//! The daemon keeps a fixed number of descriptors plus a few for every window, so a
//! soft limit of a few dozen is crossed by a dozen windows. The daemon is started
//! through a shell that lowers only the soft limit and then executes it: the test
//! process keeps its own limits.

use crate::common;

use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;

use common::real_daemon::{Sandbox, daemon_bin};

/// The soft limit the daemon is started with: under what its descriptors for
/// [`WINDOWS`] windows come to, over what it needs to start and open several.
const START_SOFT: u32 = 64;
/// Windows opened: each takes several descriptors in the daemon (about five), so
/// the daemon's descriptors for them pass [`START_SOFT`] by a wide margin.
const WINDOWS: usize = 16;
/// The hard limit the daemon needs to have room to raise its soft limit into.
const ROOM: u32 = START_SOFT * 4;

/// A script that lowers the soft limit and executes the built daemon with the
/// arguments it was given. It refuses to start the daemon when the hard limit
/// leaves no room to raise the soft one, so the test cannot pass for lack of it.
fn daemon_with_low_soft_limit(sandbox: &Sandbox) -> PathBuf {
    let script = sandbox.tree.make_dir("wrapper").join("daemon-low-soft");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\nhard=$(ulimit -H -n)\n\
             [ \"$hard\" = unlimited ] || [ \"$hard\" -gt {ROOM} ] || exit 1\n\
             ulimit -S -n {START_SOFT} || exit 1\nexec '{}' \"$@\"\n",
            daemon_bin().display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    script
}

#[tokio::test]
async fn a_daemon_started_with_a_low_soft_limit_opens_more_windows_than_that_limit_holds() {
    let sandbox = Sandbox::new("descriptor-limit");
    let wrapper = daemon_with_low_soft_limit(&sandbox);
    // Each window's command reports the soft limit it starts with, then stays.
    sandbox.write_probe_profile(
        "report-limit",
        &["/bin/sh", "-c", "ulimit -S -n; exec sleep 600"],
        "",
    );
    let mut daemon = sandbox.start_bin(&wrapper).await;

    let first = daemon.open_profile_window(&sandbox, "report-limit").await;
    // The start was really under the low limit: the first window's command was
    // started with it, so the windows below are not opened under a roomy one.
    let start = START_SOFT.to_string();
    daemon
        .output_until(&first, "the window's soft limit", |text| {
            text.lines().any(|line| line.trim() == start)
        })
        .await;

    for number in 2..=WINDOWS {
        println!("opening window {number}");
        daemon.open_profile_window(&sandbox, "report-limit").await;
    }
}
