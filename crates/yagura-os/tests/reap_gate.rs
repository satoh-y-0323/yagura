#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! `yagura_os::ReapGate`: a signal to a child and the reaping of that child
//! exclude each other, and nothing is sent once the child has been reaped, so
//! a signal can never reach a process that took over a reaped child's id.

use std::cell::Cell;
use std::sync::Arc;
use std::time::Duration;

#[test]
fn nothing_is_sent_once_the_child_has_been_reaped() {
    let gate = yagura_os::ReapGate::new();
    let sent = Cell::new(0_u32);

    gate.reap_with(|| {});
    gate.signal_unless_reaped(|| sent.set(sent.get() + 1));

    assert_eq!(sent.get(), 0, "a signal was sent to a reaped child");
}

#[test]
fn a_signal_tried_before_the_reap_is_sent_once() {
    let gate = yagura_os::ReapGate::new();
    let sent = Cell::new(0_u32);
    let reaped = Cell::new(false);

    gate.signal_unless_reaped(|| sent.set(sent.get() + 1));
    gate.reap_with(|| reaped.set(true));

    assert_eq!(
        sent.get(),
        1,
        "the signal before the reap was not sent once"
    );
    assert!(reaped.get(), "the reap was not run");
}

/// Never run: it only has to compile, which holds the kills of a child to
/// taking the gate they are sent through.
#[test]
fn the_kills_of_a_child_take_its_reap_gate() {
    let _escalate =
        |gate: Arc<yagura_os::ReapGate>| yagura_os::schedule_force_kill(1, Duration::ZERO, gate);
    let _kill =
        |killer: &yagura_os::pty::PtyKiller, gate: &Arc<yagura_os::ReapGate>| killer.kill(gate);
}
