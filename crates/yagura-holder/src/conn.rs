//! The connected daemon: one task reads its frames into the core's events,
//! another writes what the holder queues for it, in the order it was queued.

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use yagura_protocol::holder::{
    HOLDER_PROTOCOL, HolderFrame, HolderSpawnFailure, encode_holder_frame,
};

use crate::peer::Accepted;
use crate::session::Event;

/// What a connection is told when it is let in.
pub(crate) struct Welcome {
    pub(crate) record: String,
    pub(crate) cols: u16,
    pub(crate) rows: u16,
    pub(crate) exited: bool,
    pub(crate) child_pid: u32,
}

/// What the core sends a connection; the terminal thread sends the rest
/// (copies, frames, answers to reads) on the same queue.
pub(crate) enum Reply {
    Written {
        id: u64,
    },
    WriteFailed {
        id: u64,
        reason: String,
    },
    ResizeFailed {
        cols: u16,
        rows: u16,
        reason: String,
    },
    Exited {
        code: Option<i32>,
    },
    Displaced,
}

pub(crate) struct Conn {
    id: u64,
    out: mpsc::UnboundedSender<Vec<u8>>,
    writer: JoinHandle<()>,
    reader: JoinHandle<()>,
}

impl Conn {
    /// Lets `accepted` in with `welcome`.
    pub(crate) fn welcome(
        accepted: Accepted,
        id: u64,
        welcome: Welcome,
        events: mpsc::UnboundedSender<Event>,
    ) -> Self {
        let conn = Self::open(accepted, id, events);
        conn.send_frame(&HolderFrame::Welcome {
            protocol: HOLDER_PROTOCOL,
            record: welcome.record,
            cols: welcome.cols,
            rows: welcome.rows,
            exited: welcome.exited,
            child_pid: Some(welcome.child_pid),
        });
        conn
    }

    /// Answers `accepted` with why the child could not start, then `Exited`,
    /// in place of a `Welcome`.
    pub(crate) fn refuse_spawn(
        accepted: Accepted,
        id: u64,
        reason: HolderSpawnFailure,
        events: mpsc::UnboundedSender<Event>,
    ) -> Self {
        let conn = Self::open(accepted, id, events);
        conn.send_frame(&HolderFrame::SpawnFailed { reason });
        conn.send_frame(&HolderFrame::Exited { code: None });
        conn
    }

    fn open(accepted: Accepted, id: u64, events: mpsc::UnboundedSender<Event>) -> Self {
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
        }
    }

    pub(crate) fn id(&self) -> u64 {
        self.id
    }

    /// A sender onto this connection's queue, for the terminal thread. The
    /// connection closes only once every sender is dropped.
    pub(crate) fn sender(&self) -> mpsc::UnboundedSender<Vec<u8>> {
        self.out.clone()
    }

    pub(crate) fn send(&self, reply: Reply) {
        self.send_frame(&match reply {
            Reply::Written { id } => HolderFrame::Written { id },
            Reply::WriteFailed { id, reason } => HolderFrame::WriteFailed { id, reason },
            Reply::ResizeFailed { cols, rows, reason } => {
                HolderFrame::ResizeFailed { cols, rows, reason }
            }
            Reply::Exited { code } => HolderFrame::Exited { code },
            Reply::Displaced => HolderFrame::Displaced,
        });
    }

    fn send_frame(&self, frame: &HolderFrame) {
        let _ = self.out.send(encode_holder_frame(frame));
    }

    /// Stops reading and closes the connection once what was sent so far is
    /// written; returns the task doing that.
    pub(crate) fn close(self) -> JoinHandle<()> {
        self.reader.abort();
        drop(self.out);
        self.writer
    }
}
