//! Test utility: a child that never reads its stdin and leaves a grandchild holding the
//! PTY open.
//!
//! Usage: `hold_pty_child <pid_file>`. It starts a grandchild (itself, with
//! `--grandchild`) that inherits the terminal and idles, writes the grandchild's process
//! id to `pid_file`, writes [`QUERY_COUNT`] DA1 queries, prints [`FLOODED_MARKER`], then
//! idles without ever reading.
//!
//! A terminal that answers the queries fills the child's input; with the grandchild
//! alive, the PTY never reports end of input to the writer.

// Test-only fixture binary, not the daemon's production spawn path: it spawns its own
// descendant to hold the PTY open.
#![allow(clippy::disallowed_methods)]

use std::io::{self, Write};
use std::process::{Command, Stdio};

const FLOODED_MARKER: &[u8] = b"\r\nHOLD-FLOODED\r\n";
const DA1: &[u8] = b"\x1b[c";
/// Far beyond what a PTY's input buffers hold, even counting the replies the daemon's
/// bounded queue keeps.
const QUERY_COUNT: usize = 300_000;

fn idle() -> ! {
    loop {
        std::thread::park();
    }
}

fn main() -> io::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--grandchild") {
        idle();
    }
    if let Err(error) = yagura_os::enter_raw_console_input() {
        eprintln!("hold_pty_child: cannot enter raw console input: {error}");
        std::process::exit(2);
    }

    let pid_file = args.first().map_or("", String::as_str);

    let grandchild = Command::new(std::env::current_exe()?)
        .arg("--grandchild")
        .stdin(Stdio::null())
        .spawn()?;
    std::fs::write(pid_file, grandchild.id().to_string())?;

    let mut stdout = io::stdout();
    for _ in 0..QUERY_COUNT {
        stdout.write_all(DA1)?;
    }
    stdout.write_all(FLOODED_MARKER)?;
    stdout.flush()?;
    idle()
}
