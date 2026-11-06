//! The window's terminal, kept in the holder: one thread owns the model and
//! does all its work in the order things arrived (the child's output, the
//! daemon's requests), and one thread writes everything the child reads.
//!
//! No lock guards the model: it is the terminal thread's alone. A request
//! waits behind at most `MAX_WAITING_OUTPUTS` chunks of output (`buffer.rs`),
//! so a child that floods never starves the copies, the frames and the reads.

use std::collections::VecDeque;
use std::io::Write;
use std::sync::atomic::{AtomicU16, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

use tokio::sync::{mpsc, oneshot};
use yagura_os::pty::PtyControl;
use yagura_protocol::holder::{
    HolderFault, HolderFrame, HolderViewNote, HolderViewOp, MAX_TAIL_BYTES, encode_holder_frame,
};
use yagura_protocol::{InputModes, Palette};
use yagura_term::{FocusState, VtScreen, screen_view};

use crate::buffer::{Intake, Item};
use crate::push::{Push, Stamp};
use crate::session::Event;
use crate::view::Attached;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A request for the terminal thread, taken in the order it arrived among
/// the child's output.
pub(crate) enum Control {
    /// The connection `conn` was welcomed: its first copy goes whole, now.
    Connected {
        conn: u64,
    },
    PushAck {
        number: u64,
    },
    ScreenResync,
    Resize {
        cols: u16,
        rows: u16,
    },
    SetPalette(Palette),
    Attach {
        generation: u64,
    },
    Detach {
        generation: u64,
    },
    ViewNote {
        generation: u64,
        note: HolderViewNote,
    },
    ViewOp {
        id: u64,
        op: HolderViewOp,
    },
    ReadTail {
        id: u64,
        max_lines: usize,
        max_bytes: usize,
    },
    Settle {
        id: u64,
    },
    /// A write was counted, which may release a view a synchronized update
    /// holds.
    Wrote,
    /// The child has exited and its output up to its end was taken in: the
    /// copy holding all of it goes out, and no copy after it. Answered on
    /// `done` once it is sent.
    Final {
        done: oneshot::Sender<()>,
    },
}

/// Where the terminal thread's frames go: the welcomed connection. The core
/// opens and closes it; the terminal thread sends only to the connection it
/// was last told of, so a frame meant for a connection that was replaced is
/// dropped.
#[derive(Default)]
pub(crate) struct Outlet(Mutex<Option<(u64, mpsc::UnboundedSender<Vec<u8>>)>>);

impl Outlet {
    pub(crate) fn open(&self, conn: u64, out: mpsc::UnboundedSender<Vec<u8>>) {
        *lock(&self.0) = Some((conn, out));
    }

    /// Lets go of `conn`'s sender, so its connection can close.
    pub(crate) fn close(&self, conn: u64) {
        let mut outlet = lock(&self.0);
        if outlet.as_ref().is_some_and(|(open, _)| *open == conn) {
            *outlet = None;
        }
    }

    fn send(&self, conn: Option<u64>, frames: &[HolderFrame]) {
        if frames.is_empty() {
            return;
        }
        // Encoded before the lock: a view frame can take megabytes.
        let encoded: Vec<Vec<u8>> = frames.iter().map(encode_holder_frame).collect();
        let outlet = lock(&self.0);
        if let Some((open, out)) = outlet.as_ref()
            && Some(*open) == conn
        {
            for bytes in encoded {
                let _ = out.send(bytes);
            }
        }
    }
}

/// What the terminal thread publishes for the rest of the holder, read
/// without waiting on it.
pub(crate) struct Published {
    outputs_fed: AtomicU64,
    modes: AtomicU16,
    size: AtomicU32,
    /// Writes counted as the user's input; a synchronized update holds a
    /// view only until the next one.
    writes: AtomicU64,
}

impl Published {
    fn of(screen: &VtScreen) -> Self {
        let published = Self {
            outputs_fed: AtomicU64::new(0),
            modes: AtomicU16::new(0),
            size: AtomicU32::new(0),
            writes: AtomicU64::new(0),
        };
        published.update(screen);
        published
    }

    fn update(&self, screen: &VtScreen) {
        self.modes
            .store(screen.input_modes().to_bits(), Ordering::SeqCst);
        let (cols, rows) = screen.size();
        self.size
            .store(u32::from(cols) << 16 | u32::from(rows), Ordering::SeqCst);
    }

    fn outputs_fed(&self) -> u64 {
        self.outputs_fed.load(Ordering::SeqCst)
    }

    fn modes(&self) -> InputModes {
        InputModes::from_bits(self.modes.load(Ordering::SeqCst))
    }

    /// The terminal's size as the model has it.
    pub(crate) fn size(&self) -> (u16, u16) {
        let size = self.size.load(Ordering::SeqCst);
        // Both halves were stored from u16 values.
        (
            u16::try_from(size >> 16).unwrap_or(u16::MAX),
            u16::try_from(size & 0xffff).unwrap_or(u16::MAX),
        )
    }
}

/// The holder's side of the terminal: what the core hands requests and
/// writes to.
pub(crate) struct Terminal {
    pub(crate) intake: Arc<Intake>,
    pub(crate) input: Arc<InputWriter>,
    pub(crate) outlet: Arc<Outlet>,
    pub(crate) published: Arc<Published>,
}

impl Drop for Terminal {
    fn drop(&mut self) {
        self.intake.close();
        self.input.close();
    }
}

/// What the terminal thread starts with.
pub(crate) struct TerminalStart {
    pub(crate) cols: u16,
    pub(crate) rows: u16,
    pub(crate) history_rows: usize,
    pub(crate) palette: Option<Palette>,
    pub(crate) fault: Option<HolderFault>,
}

/// The threads of a window's terminal that are started with the child.
pub(crate) struct TerminalThreads {
    pub(crate) model: Box<dyn FnOnce() + Send>,
    pub(crate) writer: Box<dyn FnOnce() + Send>,
}

/// Builds the window's terminal for the child behind `control` and
/// `writer`, and the bodies of its two threads.
pub(crate) fn terminal(
    start: TerminalStart,
    control: Arc<PtyControl>,
    writer: Box<dyn Write + Send>,
    events: mpsc::UnboundedSender<Event>,
) -> (Terminal, TerminalThreads) {
    let host = yagura_term::PtyHost {
        requests_win32_input: yagura_os::program::HostOs::host().pty_host_requests_win32_input(),
    };
    let mut screen = VtScreen::with_history(start.cols, start.rows, start.history_rows, host);
    if let Some(palette) = start.palette {
        screen.set_palette(palette);
    }
    let published = Arc::new(Published::of(&screen));
    screen.follow_writes({
        let published = Arc::clone(&published);
        move || published.writes.load(Ordering::SeqCst)
    });
    let intake = Arc::new(Intake::default());
    let input = Arc::new(InputWriter {
        hold_answers: start.fault == Some(HolderFault::HoldWriteAnswer),
        ..InputWriter::default()
    });
    let outlet = Arc::new(Outlet::default());
    let model = Model {
        screen,
        control,
        intake: Arc::clone(&intake),
        outlet: Arc::clone(&outlet),
        published: Arc::clone(&published),
        input: Arc::clone(&input),
        push: Push::default(),
        attached: Attached::default(),
        conn: None,
        exited: false,
        fault: start.fault,
        changes: 0,
    };
    let threads = TerminalThreads {
        model: Box::new(move || model.run()),
        writer: Box::new({
            let input = Arc::clone(&input);
            let published = Arc::clone(&published);
            let intake = Arc::clone(&intake);
            move || input.write_all_to(writer, &published, &intake, &events)
        }),
    };
    let terminal = Terminal {
        intake,
        input,
        outlet,
        published,
    };
    (terminal, threads)
}

/// The terminal thread's state: the model and everything sent from it.
struct Model {
    screen: VtScreen,
    control: Arc<PtyControl>,
    intake: Arc<Intake>,
    outlet: Arc<Outlet>,
    published: Arc<Published>,
    input: Arc<InputWriter>,
    push: Push,
    attached: Attached,
    /// The connection frames go to.
    conn: Option<u64>,
    /// The child has exited and its last copy was sent.
    exited: bool,
    /// A fault still to act out (the fault-injecting holder only).
    fault: Option<HolderFault>,
    /// Output chunks taken in and resizes made, so far.
    changes: u64,
}

impl Model {
    fn run(mut self) {
        while let Some(item) = self.intake.next() {
            match item {
                // The model stays as the child's end left it.
                Item::Output(_) if self.exited => {}
                Item::Output(bytes) => self.take_in(&bytes),
                Item::Control(control) => self.handle(control),
            }
        }
    }

    fn send(&self, frames: &[HolderFrame]) {
        self.outlet.send(self.conn, frames);
    }

    fn stamp(&self) -> Stamp {
        Stamp {
            changes: self.changes,
            generation: self.screen.generation(),
        }
    }

    fn take_in(&mut self, bytes: &[u8]) {
        self.screen.feed(bytes);
        let fed = self.screen.note_output();
        let (replies, dropped) = self.screen.take_replies();
        self.published.update(&self.screen);
        self.published.outputs_fed.store(fed, Ordering::SeqCst);
        self.input.push_replies(replies, dropped);
        self.input.note_output();
        self.changes = self.changes.wrapping_add(1);
        self.attached.note_change();
        self.send_due();
    }

    /// The ordinary copy, if one is due, and the view's frame, if one is due.
    fn send_due(&mut self) {
        if !self.exited && self.push.due(self.stamp()) {
            self.send_copy();
        }
        self.send_view();
    }

    fn send_view(&mut self) {
        let frames = self.attached.frames(&mut self.screen, self.exited);
        self.send(&frames);
    }

    /// Sends a copy of the screen as it stands, whatever is awaited.
    fn send_copy(&mut self) {
        // A read that panics rebuilds the model blank; the blank one is read.
        let copy = self
            .screen
            .read_guarded(VtScreen::screen_copy)
            .or_else(|| self.screen.read_guarded(VtScreen::screen_copy));
        let Some(copy) = copy else {
            return;
        };
        if let Some(frames) = self.push.frames(copy, self.stamp()) {
            self.send(&frames);
        }
    }

    fn handle(&mut self, control: Control) {
        match control {
            Control::Connected { conn } => self.connected(conn),
            Control::PushAck { number } => {
                self.push.acknowledge(number);
                self.send_due();
            }
            Control::ScreenResync => {
                self.push.resync();
                self.send_due();
            }
            Control::Resize { cols, rows } => self.resize(cols, rows),
            Control::SetPalette(palette) => self.screen.set_palette(palette),
            Control::Attach { generation } => {
                let frames = self
                    .attached
                    .attach(generation, &mut self.screen, self.exited);
                self.send(&frames);
            }
            Control::Detach { generation } => self.attached.detach(generation),
            Control::ViewNote { generation, note } => {
                self.attached.note(generation, note);
                self.send_view();
            }
            Control::ViewOp { id, op } => self.view_op(id, op),
            Control::ReadTail {
                id,
                max_lines,
                max_bytes,
            } => {
                let (text, truncated) = self
                    .screen
                    .text_tail(max_lines, max_bytes.min(MAX_TAIL_BYTES));
                self.send(&[HolderFrame::Tail {
                    id,
                    text,
                    truncated,
                }]);
            }
            Control::Settle { id } => {
                // Everything read before the settle was taken in before it.
                self.send_view();
                self.send(&[HolderFrame::Settled { id }]);
            }
            Control::Wrote => {
                self.attached.note_write();
                self.send_view();
            }
            Control::Final { done } => {
                if !self.exited && !self.push.holds(self.stamp()) {
                    self.send_copy();
                }
                self.exited = true;
                self.attached.note_change();
                self.send_view();
                let _ = done.send(());
            }
        }
    }

    fn connected(&mut self, conn: u64) {
        self.conn = Some(conn);
        self.push.reset();
        self.attached.clear();
        if !self.exited {
            self.send_copy();
        }
        match self.fault.take() {
            Some(HolderFault::StallModel) => self.stall(),
            Some(HolderFault::PoisonModel) => self.screen.poison_after_calls(0),
            Some(HolderFault::HoldWriteAnswer) | None => {}
        }
    }

    /// Acts out [`HolderFault::StallModel`]: takes nothing in until the
    /// child's end, which is answered with nothing sent before it.
    fn stall(&mut self) {
        self.intake.drop_output();
        if let Some(Control::Final { done }) = self
            .intake
            .wait_for(|control| matches!(control, Control::Final { .. }))
        {
            self.exited = true;
            let _ = done.send(());
        }
    }

    /// The model first, then the terminal, so the child's redraw for the new
    /// size lands on a model that already has it; a size the terminal
    /// refuses puts the model back. The copy of the new size goes before
    /// `Resized`, whatever is awaited.
    fn resize(&mut self, cols: u16, rows: u16) {
        let previous = self.screen.size();
        self.screen.resize(cols, rows);
        let resized = self.control.resize(cols, rows);
        if resized.is_err() {
            self.screen.resize(previous.0, previous.1);
        }
        self.published.update(&self.screen);
        self.changes = self.changes.wrapping_add(1);
        self.attached.note_change();
        let answer = match resized {
            Ok(()) => {
                if !self.exited {
                    self.send_copy();
                }
                HolderFrame::Resized { cols, rows }
            }
            Err(error) => {
                if !self.exited && self.push.due(self.stamp()) {
                    self.send_copy();
                }
                HolderFrame::ResizeFailed {
                    cols,
                    rows,
                    reason: format!("the terminal refused the size: {error}"),
                }
            }
        };
        self.send_view();
        self.send(&[answer]);
    }

    fn view_op(&mut self, id: u64, op: HolderViewOp) {
        let screen = &mut self.screen;
        let (text, moved) = match op {
            HolderViewOp::Scroll { lines } => {
                screen_view::scroll_view(screen, lines);
                (None, false)
            }
            HolderViewOp::Page { up } => {
                screen_view::page_view(screen, up);
                (None, false)
            }
            HolderViewOp::Select { pointer } => {
                screen_view::select(screen, &pointer);
                (None, false)
            }
            HolderViewOp::ClearSelection => {
                screen_view::clear_selection(screen);
                (None, false)
            }
            HolderViewOp::SelectedText => (screen_view::selected_text(screen), false),
            HolderViewOp::ToBottom => (None, screen_view::scroll_to_bottom(screen)),
        };
        self.attached.note_change();
        self.send_view();
        self.send(&[HolderFrame::ViewOpDone { id, text, moved }]);
    }
}

/// Reply and report bytes waiting for the writer; a batch that would go past
/// this is dropped.
const MAX_QUEUED_REPLY_BYTES: usize = 64 * 1024;

/// Bytes of a write handed to the terminal at a time, so letting go of the
/// input stops a write the terminal no longer takes within one piece.
const WRITE_PIECE: usize = 4096;

fn write_whole(writer: &mut dyn Write, bytes: &[u8]) -> bool {
    bytes.is_empty()
        || writer
            .write_all(bytes)
            .and_then(|()| writer.flush())
            .is_ok()
}

/// One thing for the writer, written in the order it was queued.
enum WriteJob {
    /// The daemon's write: counted as the user's input, answered.
    Write {
        conn: u64,
        id: u64,
        bytes: Vec<u8>,
    },
    /// Replies to the child's queries, or a report the child turned on: not
    /// counted, not answered.
    Unanswered(Vec<u8>),
    ViewFocus(bool),
    Interval {
        open: bool,
    },
    /// Output arrived while a focus-out waited for it.
    CheckFocus,
}

#[derive(Default)]
struct WriterState {
    jobs: VecDeque<WriteJob>,
    /// Bytes of [`WriteJob::Unanswered`] in `jobs`.
    unanswered_bytes: usize,
    /// Set while replies are being dropped, so a flood logs once.
    dropping: bool,
    closed: bool,
    focus: FocusState,
}

/// The one writer of the child's input: the daemon's writes, the replies to
/// the child's queries, the daemon's reports and the focus reports go
/// through it one at a time, so none lands inside another. The writer thread
/// alone waits on the child, so a child that stops reading its input holds
/// up neither the terminal thread nor the core.
#[derive(Default)]
pub(crate) struct InputWriter {
    state: Mutex<WriterState>,
    ready: Condvar,
    /// Acts out [`HolderFault::HoldWriteAnswer`].
    hold_answers: bool,
}

impl InputWriter {
    fn state(&self) -> MutexGuard<'_, WriterState> {
        lock(&self.state)
    }

    fn queue(&self, state: &mut WriterState, job: WriteJob) {
        if !state.closed {
            state.jobs.push_back(job);
            self.ready.notify_one();
        }
    }

    /// Queues the daemon's write `id` on `conn`; `false` once the input is
    /// let go of.
    pub(crate) fn push_write(&self, conn: u64, id: u64, bytes: Vec<u8>) -> bool {
        let mut state = self.state();
        if state.closed {
            return false;
        }
        self.queue(&mut state, WriteJob::Write { conn, id, bytes });
        true
    }

    /// Queues `bytes` whole, or drops them when the queue has no room;
    /// `dropped` replies were already dropped before reaching it.
    pub(crate) fn push_replies(&self, bytes: Vec<u8>, dropped: usize) {
        if bytes.is_empty() && dropped == 0 {
            return;
        }
        let mut state = self.state();
        if state.closed {
            return;
        }
        let fits = state.unanswered_bytes + bytes.len() <= MAX_QUEUED_REPLY_BYTES;
        if fits && dropped == 0 {
            state.dropping = false;
        } else if !state.dropping {
            state.dropping = true;
            crate::log(format_args!(
                "dropping replies to the child's terminal queries: the child is not reading its input"
            ));
        }
        if fits && !bytes.is_empty() {
            state.unanswered_bytes += bytes.len();
            self.queue(&mut state, WriteJob::Unanswered(bytes));
        }
    }

    /// Queues a report the daemon sends the child, as replies go.
    pub(crate) fn push_report(&self, bytes: Vec<u8>) {
        self.push_replies(bytes, 0);
    }

    pub(crate) fn set_view_focus(&self, focused: bool) {
        let mut state = self.state();
        self.queue(&mut state, WriteJob::ViewFocus(focused));
    }

    pub(crate) fn focus_interval(&self, open: bool) {
        let mut state = self.state();
        self.queue(&mut state, WriteJob::Interval { open });
    }

    /// Output arrived: a focus-out waiting for it may go now.
    fn note_output(&self) {
        let mut state = self.state();
        if state.focus.out_deferred() {
            self.queue(&mut state, WriteJob::CheckFocus);
        }
    }

    /// Lets go of the input: a write in hand stops at its next piece, and
    /// nothing still queued is written. Each of those writes is answered as
    /// failed, and then the writer thread ends with [`Event::WriterDone`].
    pub(crate) fn close(&self) {
        let mut state = self.state();
        state.closed = true;
        state
            .jobs
            .retain(|job| matches!(job, WriteJob::Write { .. }));
        state.unanswered_bytes = 0;
        self.ready.notify_one();
    }

    /// The next job, and whether the input was let go of (so it is not to be
    /// written); `None` once it was and nothing is left to answer.
    fn next(&self) -> Option<(WriteJob, bool)> {
        let mut state = self.state();
        loop {
            if state.closed {
                return state.jobs.pop_front().map(|job| (job, true));
            }
            if let Some(job) = state.jobs.pop_front() {
                if let WriteJob::Unanswered(bytes) = &job {
                    state.unanswered_bytes -= bytes.len();
                }
                return Some((job, false));
            }
            state = self
                .ready
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    fn wait_closed(&self) {
        let mut state = self.state();
        while !state.closed {
            state = self
                .ready
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// Writes `bytes` a piece at a time; `false` when a piece failed, or the
    /// input was let go of before the last piece.
    fn write_answered(&self, writer: &mut dyn Write, bytes: &[u8]) -> bool {
        for (index, piece) in bytes.chunks(WRITE_PIECE).enumerate() {
            if index > 0 && self.state().closed {
                return false;
            }
            if writer.write_all(piece).is_err() {
                return false;
            }
        }
        writer.flush().is_ok()
    }

    /// The writer thread's body. Every write it is handed is answered here,
    /// and [`Event::WriterDone`] follows the last answer, once the child's
    /// input is let go of.
    fn write_all_to(
        &self,
        mut writer: Box<dyn Write + Send>,
        published: &Published,
        intake: &Intake,
        events: &mpsc::UnboundedSender<Event>,
    ) {
        self.write_jobs(writer.as_mut(), published, intake, events);
        drop(writer);
        let _ = events.send(Event::WriterDone);
    }

    fn write_jobs(
        &self,
        writer: &mut dyn Write,
        published: &Published,
        intake: &Intake,
        events: &mpsc::UnboundedSender<Event>,
    ) {
        while let Some((job, closed)) = self.next() {
            match job {
                WriteJob::Write { conn, id, .. } if closed => {
                    let _ = events.send(Event::WriteDone {
                        conn,
                        id,
                        written: false,
                    });
                }
                _ if closed => {}
                WriteJob::Write { conn, id, bytes } => {
                    // Counted before it is written, so the view it releases
                    // is never read as still held after the child saw it.
                    published.writes.fetch_add(1, Ordering::SeqCst);
                    intake.push(Control::Wrote);
                    let mut lead = self
                        .state()
                        .focus
                        .before_input(published.modes(), published.outputs_fed());
                    lead.extend_from_slice(&bytes);
                    let written = self.write_answered(writer, &lead);
                    if self.hold_answers {
                        self.wait_closed();
                    }
                    let _ = events.send(Event::WriteDone { conn, id, written });
                    let report = {
                        let mut state = self.state();
                        let outputs = published.outputs_fed();
                        if state.focus.input_ended(outputs) {
                            state.focus.reconcile(published.modes(), outputs)
                        } else {
                            Vec::new()
                        }
                    };
                    write_whole(writer, &report);
                }
                WriteJob::Unanswered(bytes) => {
                    write_whole(writer, &bytes);
                }
                WriteJob::ViewFocus(focused) => {
                    let report = {
                        let mut state = self.state();
                        state.focus.set_view(focused);
                        state
                            .focus
                            .reconcile(published.modes(), published.outputs_fed())
                    };
                    write_whole(writer, &report);
                }
                WriteJob::Interval { open } => {
                    let report = {
                        let mut state = self.state();
                        let outputs = published.outputs_fed();
                        let due = if open {
                            state.focus.open_interval();
                            true
                        } else {
                            state.focus.close_interval(outputs)
                        };
                        if due {
                            state.focus.reconcile(published.modes(), outputs)
                        } else {
                            Vec::new()
                        }
                    };
                    write_whole(writer, &report);
                }
                WriteJob::CheckFocus => {
                    let report = self
                        .state()
                        .focus
                        .reconcile(published.modes(), published.outputs_fed());
                    write_whole(writer, &report);
                }
            }
        }
    }
}
