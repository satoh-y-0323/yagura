#![cfg(windows)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! `yagura_os::pty::spawn` on Windows, driven directly: the child it reports
//! is the process it started, killing a child that has already exited is not an
//! error, and its handles say what they are when printed.
//!
//! The bundled pseudoconsole sits beside this test executable (the daemon's
//! build script places it), which `spawn` requires.

mod common;

use std::path::Path;

use yagura_os::pty::{PtyCommand, PtySize, PtySpawn, spawn};
use yagura_test_support::process::{echo_child_command, exit_with_code_command};

fn start(argv: Vec<String>) -> PtySpawn {
    spawn(
        &PtyCommand {
            argv,
            cwd: None,
            env: Vec::new(),
        },
        PtySize { cols: 80, rows: 24 },
    )
    .expect("the program should start on a pseudoconsole")
}

#[test]
fn the_reported_process_id_is_the_started_program_run_by_this_process() {
    let argv = echo_child_command();
    let image = Path::new(&argv[0])
        .file_name()
        .expect("the fixture path has a file name")
        .to_string_lossy()
        .into_owned();
    let started = start(argv);

    let reported = started.child.process_id();

    let row = common::process_table()
        .into_iter()
        .find(|row| row.pid == reported)
        .unwrap_or_else(|| panic!("no process has the reported id {reported}"));
    assert!(
        row.image.eq_ignore_ascii_case(&image),
        "process {reported} runs {}, not {image}",
        row.image
    );
    assert_eq!(row.parent_pid, std::process::id());
}

#[test]
fn killing_a_child_that_has_already_exited_is_not_an_error() {
    let started = start(exit_with_code_command("3"));
    let killer = started.killer.clone();

    let code = started.child.wait().expect("the exit code can be read");

    assert_eq!(code, 3);
    killer
        .kill()
        .expect("a child that has already exited is not an error to kill");
}

#[test]
fn the_started_pieces_say_what_they_are_when_printed() {
    let started = start(echo_child_command());

    let spawn_text = format!("{started:?}");
    let child_text = format!("{:?}", started.child);
    let open_text = format!("{:?}", started.control);
    started.control.close();
    let closed_text = format!("{:?}", started.control);

    assert!(spawn_text.starts_with("PtySpawn"), "got {spawn_text:?}");
    assert!(spawn_text.contains("child"), "got {spawn_text:?}");
    assert!(child_text.starts_with("PtyChild"), "got {child_text:?}");
    assert!(open_text.starts_with("PtyControl"), "got {open_text:?}");
    assert!(open_text.contains("open: true"), "got {open_text:?}");
    assert!(closed_text.contains("open: false"), "got {closed_text:?}");
}
