//! The connected daemon, and the order it is sent things in: nothing of the
//! output before it asks for the replay, and `Exited` only after all of it.

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use yagura_protocol::holder::{
    HOLDER_PROTOCOL, HolderFrame, HolderSpawnFailure, encode_holder_frame,
};

use crate::buffer::{ACK_WINDOW, OutputState};
use crate::peer::Accepted;
use crate::session::Event;

/// The most output one `Output` frame carries.
const OUTPUT_FRAME_BYTES: u64 = 64 * 1024;

/// What a connection is told when it is let in.
pub(crate) struct Welcome {
    pub(crate) record: String,
    pub(crate) cols: u16,
    pub(crate) rows: u16,
    pub(crate) exited: bool,
    pub(crate) checkpoint_seq: Option<u64>,
    pub(crate) replay_end_seq: u64,
    pub(crate) child_pid: u32,
}

/// The answers a connection may be sent at any stage; output and `Exited`
/// go only through [`Conn::replay`] and [`Replayed`].
pub(crate) enum Reply {
    Written {
        id: u64,
    },
    WriteFailed {
        id: u64,
        reason: String,
    },
    Resized {
        cols: u16,
        rows: u16,
    },
    ResizeFailed {
        cols: u16,
        rows: u16,
        reason: String,
    },
    Displaced,
}

enum Stage {
    /// Welcomed; offered the output up to `replay_end`, sent none of it yet.
    Welcomed { replay_end: u64 },
    /// Sent the output up to `sent`.
    Replayed { sent: u64 },
    /// Sent `Exited` (or never had a child): nothing more of the output.
    Ended,
}

pub(crate) struct Conn {
    id: u64,
    out: mpsc::UnboundedSender<Vec<u8>>,
    writer: JoinHandle<()>,
    reader: JoinHandle<()>,
    stage: Stage,
}

impl Conn {
    /// Lets `accepted` in with `welcome`.
    pub(crate) fn welcome(
        accepted: Accepted,
        id: u64,
        welcome: Welcome,
        events: mpsc::UnboundedSender<Event>,
    ) -> Self {
        let conn = Self::open(
            accepted,
            id,
            Stage::Welcomed {
                replay_end: welcome.replay_end_seq,
            },
            events,
        );
        conn.send_frame(&HolderFrame::Welcome {
            protocol: HOLDER_PROTOCOL,
            record: welcome.record,
            cols: welcome.cols,
            rows: welcome.rows,
            exited: welcome.exited,
            checkpoint_seq: welcome.checkpoint_seq,
            replay_end_seq: welcome.replay_end_seq,
            child_pid: Some(welcome.child_pid),
            // The very window `read_permit` reads up to, so the daemon steps within it.
            ack_window: Some(ACK_WINDOW),
        });
        conn
    }

    /// Answers `accepted` with why the child could not start, then `Exited`,
    /// in place of a `Welcome`: there is no output to come before it.
    pub(crate) fn refuse_spawn(
        accepted: Accepted,
        id: u64,
        reason: HolderSpawnFailure,
        events: mpsc::UnboundedSender<Event>,
    ) -> Self {
        let conn = Self::open(accepted, id, Stage::Ended, events);
        conn.send_frame(&HolderFrame::SpawnFailed { reason });
        conn.send_frame(&HolderFrame::Exited { code: None });
        conn
    }

    fn open(
        accepted: Accepted,
        id: u64,
        stage: Stage,
        events: mpsc::UnboundedSender<Event>,
    ) -> Self {
        let (mut read, mut write) = tokio::io::split(accepted.connection);
        let (out, mut outgoing) = mpsc::unbounded_channel::<Vec<u8>>();
        let writer = tokio::spawn(async move {
            while let Some(bytes) = outgoing.recv().await {
                if write.write_all(&bytes).await.is_err() {
                    return;
                }
            }
            let _ = write.shutdown().await;
        });
        let mut frames = accepted.frames;
        let reader = tokio::spawn(async move {
            let mut chunk = vec![0u8; 64 * 1024];
            loop {
                loop {
                    match frames.next_frame() {
                        Ok(Some(frame)) => {
                            if events.send(Event::Frame { conn: id, frame }).is_err() {
                                return;
                            }
                        }
                        Ok(None) => break,
                        Err(_) => {
                            let _ = events.send(Event::Closed { conn: id });
                            return;
                        }
                    }
                }
                match read.read(&mut chunk).await {
                    Ok(0) | Err(_) => {
                        let _ = events.send(Event::Closed { conn: id });
                        return;
                    }
                    Ok(read) => frames.push(&chunk[..read]),
                }
            }
        });
        Self {
            id,
            out,
            writer,
            reader,
            stage,
        }
    }

    pub(crate) fn id(&self) -> u64 {
        self.id
    }

    pub(crate) fn send(&self, reply: Reply) {
        self.send_frame(&match reply {
            Reply::Written { id } => HolderFrame::Written { id },
            Reply::WriteFailed { id, reason } => HolderFrame::WriteFailed { id, reason },
            Reply::Resized { cols, rows } => HolderFrame::Resized { cols, rows },
            Reply::ResizeFailed { cols, rows, reason } => {
                HolderFrame::ResizeFailed { cols, rows, reason }
            }
            Reply::Displaced => HolderFrame::Displaced,
        });
    }

    fn send_frame(&self, frame: &HolderFrame) {
        let _ = self.out.send(encode_holder_frame(frame));
    }

    /// Sends the checkpoint and the output up to the `Welcome`'s
    /// `replay_end_seq`, then everything read since; `None` when this
    /// connection was already replayed.
    pub(crate) fn replay(&mut self, state: &OutputState) -> Option<Replayed<'_>> {
        let Stage::Welcomed { replay_end } = self.stage else {
            return None;
        };
        if let Some(checkpoint) = state.buffer.checkpoint() {
            self.send_frame(&HolderFrame::Checkpoint {
                seq: checkpoint.seq,
                bytes: checkpoint.bytes.clone(),
            });
        }
        let from = state.buffer.start();
        let to = replay_end.max(from);
        send_output(&self.out, state, from, to);
        self.stage = Stage::Replayed { sent: to };
        let mut replayed = self.replayed()?;
        replayed.flush(state);
        Some(replayed)
    }

    /// The connection once its replay is done, until it is sent `Exited`.
    pub(crate) fn replayed(&mut self) -> Option<Replayed<'_>> {
        let Stage::Replayed { sent } = self.stage else {
            return None;
        };
        Some(Replayed {
            out: &self.out,
            stage: &mut self.stage,
            sent,
        })
    }

    /// Stops reading and closes the connection once what was sent so far is
    /// written; returns the task doing that.
    pub(crate) fn close(self) -> JoinHandle<()> {
        self.reader.abort();
        drop(self.out);
        self.writer
    }
}

/// A connection that has been sent its replay: only here can live output and
/// `Exited` be sent.
pub(crate) struct Replayed<'a> {
    out: &'a mpsc::UnboundedSender<Vec<u8>>,
    stage: &'a mut Stage,
    sent: u64,
}

impl Replayed<'_> {
    /// Sends what was read after what has been sent.
    pub(crate) fn flush(&mut self, state: &OutputState) {
        let from = self.sent.max(state.buffer.start());
        let to = state.buffer.end();
        send_output(self.out, state, from, to);
        self.sent = to.max(from);
        *self.stage = Stage::Replayed { sent: self.sent };
    }

    /// Sends the rest of the output, then `Exited`; nothing follows it.
    pub(crate) fn exited(mut self, state: &OutputState, code: Option<i32>) {
        self.flush(state);
        let _ = self
            .out
            .send(encode_holder_frame(&HolderFrame::Exited { code }));
        *self.stage = Stage::Ended;
    }
}

/// Sends the kept output from `from` to `to` as `Output` frames.
fn send_output(out: &mpsc::UnboundedSender<Vec<u8>>, state: &OutputState, from: u64, to: u64) {
    let mut at = from;
    while at < to {
        let bytes = state
            .buffer
            .range(at, to.min(at.saturating_add(OUTPUT_FRAME_BYTES)));
        if bytes.is_empty() {
            return;
        }
        at += bytes.len() as u64;
        let _ = out.send(encode_holder_frame(&HolderFrame::Output {
            seq: at,
            bytes: bytes.to_vec(),
        }));
    }
}
