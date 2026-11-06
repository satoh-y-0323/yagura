//! What the holder's terminal thread takes in, in the order it arrived: the
//! child's output as it was read and the daemon's requests. Little output
//! waits here: when the terminal thread is behind, the reader stops reading
//! and the child is held up in its write instead.

use std::collections::VecDeque;
use std::io::Read;
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};

use crate::terminal::Control;

/// Bytes the reader takes from the terminal at a time, at most.
const READ_CHUNK: usize = 8192;

/// How many chunks of output may wait for the terminal thread. A request
/// behind them waits for at most this much output to be taken in, so a child
/// that floods never holds a request up for long.
const MAX_WAITING_OUTPUTS: usize = 16;

/// One thing for the terminal thread.
pub(crate) enum Item {
    /// A chunk of the child's output.
    Output(Vec<u8>),
    /// A request.
    Control(Control),
}

#[derive(Default)]
struct IntakeState {
    items: VecDeque<Item>,
    /// How many of `items` are output.
    outputs: usize,
    /// Output is dropped as it is read instead of waiting.
    dropping_output: bool,
    closed: bool,
}

/// The queue between the reader, the holder's core and the terminal thread.
#[derive(Default)]
pub(crate) struct Intake {
    state: Mutex<IntakeState>,
    /// Wakes the terminal thread.
    ready: Condvar,
    /// Wakes the reader when output may wait again.
    room: Condvar,
}

impl Intake {
    fn state(&self) -> MutexGuard<'_, IntakeState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Queues a chunk of output once there is room for it; `false` once the
    /// intake is closed.
    fn push_output(&self, bytes: Vec<u8>) -> bool {
        let mut state = self.state();
        while !state.closed && !state.dropping_output && state.outputs >= MAX_WAITING_OUTPUTS {
            state = self
                .room
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
        if state.closed {
            return false;
        }
        if !state.dropping_output {
            state.items.push_back(Item::Output(bytes));
            state.outputs += 1;
            self.ready.notify_one();
        }
        true
    }

    /// Queues a request; never waits.
    pub(crate) fn push(&self, control: Control) {
        let mut state = self.state();
        if !state.closed {
            state.items.push_back(Item::Control(control));
            self.ready.notify_one();
        }
    }

    /// The next item, waiting for one; `None` once the intake is closed.
    pub(crate) fn next(&self) -> Option<Item> {
        let mut state = self.state();
        loop {
            if state.closed {
                return None;
            }
            if let Some(item) = state.items.pop_front() {
                if matches!(item, Item::Output(_)) {
                    state.outputs -= 1;
                    self.room.notify_one();
                }
                return Some(item);
            }
            state = self
                .ready
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// Stops keeping output: everything waiting and everything read from now
    /// on is dropped, so the reader never waits for a terminal thread that
    /// no longer takes it in.
    pub(crate) fn drop_output(&self) {
        let mut state = self.state();
        state.dropping_output = true;
        state.items.retain(|item| matches!(item, Item::Control(_)));
        state.outputs = 0;
        self.room.notify_all();
    }

    /// Waits for the first request `pick` takes, dropping every item before
    /// it; `None` once the intake is closed.
    pub(crate) fn wait_for(&self, pick: impl Fn(&Control) -> bool) -> Option<Control> {
        let mut state = self.state();
        loop {
            if state.closed {
                return None;
            }
            while let Some(item) = state.items.pop_front() {
                match item {
                    Item::Control(control) if pick(&control) => return Some(control),
                    Item::Output(_) => {
                        state.outputs -= 1;
                        self.room.notify_one();
                    }
                    Item::Control(_) => {}
                }
            }
            state = self
                .ready
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// Ends the terminal thread and lets a waiting reader go.
    pub(crate) fn close(&self) {
        let mut state = self.state();
        state.closed = true;
        state.items.clear();
        state.outputs = 0;
        self.ready.notify_all();
        self.room.notify_all();
    }
}

/// Reads the terminal into `intake` until its output ends or the intake is
/// closed.
pub(crate) fn read_into(mut reader: Box<dyn Read + Send>, intake: &Intake) {
    let mut buf = vec![0u8; READ_CHUNK];
    loop {
        match reader.read(&mut buf) {
            Ok(0) | Err(_) => return,
            Ok(read) => {
                if !intake.push_output(buf[..read].to_vec()) {
                    return;
                }
            }
        }
    }
}
