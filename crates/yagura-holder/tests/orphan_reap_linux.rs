#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// Starts the fixture as a process the test owns and reaps itself.
#![allow(clippy::disallowed_methods)]

//! A child subreaper reaps the orphans handed to it as each one ends, and
//! never the child it runs as its leader. The subreaper is `holder_child`
//! in a process of its own: the attribute is per process, and this process
//! runs other tests alongside.

use std::collections::BTreeSet;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::mpsc;

use yagura_test_support::HANG_GUARD;

const CHILD: &str = env!("CARGO_BIN_EXE_holder_child");

/// `(pid, state)` of every process whose parent is `parent`, from `ps`.
fn children_of(parent: u32) -> Vec<(u32, String)> {
    let output = Command::new("ps")
        .args(["-e", "-o", "pid=,ppid=,stat="])
        .output()
        .expect("run ps");
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let pid = fields.next()?.parse().ok()?;
            let ppid: u32 = fields.next()?.parse().ok()?;
            let state = fields.next()?.to_owned();
            (ppid == parent).then_some((pid, state))
        })
        .collect()
}

fn pid_after(line: &str, word: &str) -> Option<u32> {
    line.strip_prefix(word)?.trim().parse().ok()
}

#[test]
fn orphans_are_reaped_as_they_end_and_the_leader_is_left_unreaped() {
    let mut fixture = Command::new(CHILD)
        .arg("orphans")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("start the subreaper fixture");
    let stdout = BufReader::new(fixture.stdout.take().unwrap());
    let (tx, lines) = mpsc::channel();
    std::thread::spawn(move || {
        for line in stdout.lines() {
            let Ok(line) = line else { return };
            if tx.send(line).is_err() {
                return;
            }
        }
    });

    let mut leader = None;
    let mut orphans = BTreeSet::new();
    let mut reaped = BTreeSet::new();
    let mut seen = Vec::new();
    loop {
        let line = lines
            .recv_timeout(HANG_GUARD)
            .unwrap_or_else(|error| panic!("the fixture stopped short ({error}): {seen:?}"));
        if line == "ORPHANS-DONE" {
            break;
        }
        if let Some(pid) = pid_after(&line, "LEADER") {
            leader = Some(pid);
        } else if let Some(pid) = pid_after(&line, "ORPHAN") {
            orphans.insert(pid);
        } else if let Some(pid) = pid_after(&line, "REAPED") {
            reaped.insert(pid);
        }
        seen.push(line);
    }
    let leader = leader.expect("the fixture named no leader");
    let left = children_of(fixture.id());

    drop(fixture.stdin.take());
    let _ = fixture.wait();
    assert_eq!(
        orphans.len(),
        10,
        "the leader did not start every orphan: {seen:?}"
    );
    assert!(
        !reaped.contains(&leader),
        "the leader {leader} was reaped with the orphans: {seen:?}"
    );
    assert_eq!(reaped, orphans, "not every orphan was reaped: {seen:?}");
    assert!(
        matches!(left.as_slice(), [(pid, state)] if *pid == leader && state.starts_with('Z')),
        "the subreaper's children are not just its unreaped leader {leader}: {left:?}"
    );
}
