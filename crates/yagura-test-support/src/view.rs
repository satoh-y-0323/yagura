//! A GUI stand-in for tests: attaches to a window over the real IPC endpoint,
//! applies the frames the daemon sends into a [`Grid`], and acknowledges them
//! the way a renderer does.

use std::time::Duration;

use tokio::time::Instant;
use yagura_protocol::{
    ClientMessage, DaemonMessage, Frame, KeyInput, PointerInput, TerminalSize, WheelInput,
};

use crate::frame::Grid;
use crate::ipc::{Connection, send_client_message, try_read_daemon_message};

/// What was received for one completed frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletedFrame {
    /// The frame number.
    pub seq: u64,
    /// How many parts it came in.
    pub parts: usize,
    /// Whether it replaced the whole grid.
    pub reset: bool,
    /// How many rows its parts carried in total.
    pub rows_carried: usize,
}

/// One attached connection with its grid.
pub struct ViewClient {
    /// The connection. Tests may send other messages on it directly.
    pub conn: Connection,
    /// The grid every received part was applied to.
    pub grid: Grid,
    /// Whether the child had exited when the attach was answered.
    pub child_exited: bool,
    /// The exit code the attach was answered with.
    pub exit_code: Option<i32>,
    /// Every completed frame so far, oldest first.
    pub completed: Vec<CompletedFrame>,
    /// The exit code of a pushed `Exit`, once one arrived.
    pub exited: Option<i32>,
    /// Every part of the most recently completed frame, in arrival order.
    pub last_frame_parts: Vec<Frame>,
    open_buffer: Vec<Frame>,
    open_parts: usize,
    open_rows: usize,
    open_reset: bool,
}

const ATTACH_TIMEOUT: Duration = Duration::from_secs(20);

impl ViewClient {
    /// Attaches to `window_id` (at `size` when given), reads the `Attached`
    /// answer and the first full frame, applies it and acknowledges it.
    ///
    /// # Panics
    ///
    /// Panics if the daemon does not answer with `Attached` followed by a
    /// full frame (`reset`), or if the frame breaks a wire rule.
    pub async fn attach(conn: Connection, window_id: &str, size: Option<TerminalSize>) -> Self {
        let mut client = Self::attach_unacknowledged(conn, window_id, size).await;
        let seq = client
            .grid
            .last_seq
            .expect("the first frame must have completed");
        client.ack(seq).await;
        client
    }

    /// [`ViewClient::attach`] without acknowledging the first frame.
    ///
    /// # Panics
    ///
    /// Panics under the same conditions as [`ViewClient::attach`].
    pub async fn attach_unacknowledged(
        conn: Connection,
        window_id: &str,
        size: Option<TerminalSize>,
    ) -> Self {
        let mut client = Self {
            conn,
            grid: Grid::new(),
            child_exited: false,
            exit_code: None,
            completed: Vec::new(),
            exited: None,
            last_frame_parts: Vec::new(),
            open_buffer: Vec::new(),
            open_parts: 0,
            open_rows: 0,
            open_reset: false,
        };
        client.reattach_unacknowledged(window_id, size).await;
        client
    }

    /// Sends `attach_view` again on this connection and reads the answer and
    /// the full frame that follows, without acknowledging it. The grid is
    /// replaced by that frame.
    ///
    /// # Panics
    ///
    /// Panics if the daemon does not answer with `Attached` followed by a
    /// full frame (`reset`), or if the frame breaks a wire rule.
    pub async fn reattach_unacknowledged(&mut self, window_id: &str, size: Option<TerminalSize>) {
        self.open_parts = 0;
        self.open_buffer.clear();
        send_client_message(
            &mut self.conn,
            &ClientMessage::AttachView {
                window_id: window_id.to_string(),
                size,
            },
        )
        .await;
        loop {
            match self.read_non_status(ATTACH_TIMEOUT).await {
                Some(DaemonMessage::Attached {
                    child_exited,
                    exit_code,
                }) => {
                    self.child_exited = child_exited;
                    self.exit_code = exit_code;
                    break;
                }
                // A part of the previous attach's frame that was already in flight.
                Some(DaemonMessage::Frame(_) | DaemonMessage::Exit { .. }) => continue,
                other => panic!("expected `attached` after attach_view, got {other:?}"),
            }
        }
        let parts = self.next_complete_frame(ATTACH_TIMEOUT).await;
        assert!(
            parts.first().is_some_and(|part| part.reset),
            "the frame right after attach must replace the whole grid (reset)"
        );
    }

    /// Reads the next message that is not a `StatusChanged` push; `None` when
    /// nothing arrives within `timeout` or the connection closed. A pushed
    /// `Exit` is recorded and also returned.
    async fn read_non_status(&mut self, timeout: Duration) -> Option<DaemonMessage> {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match try_read_daemon_message(&mut self.conn, remaining).await? {
                DaemonMessage::StatusChanged { .. } => continue,
                DaemonMessage::Exit { exit_code } => {
                    self.exited = Some(exit_code);
                    return Some(DaemonMessage::Exit { exit_code });
                }
                message => return Some(message),
            }
        }
    }

    /// Reads one part of a frame, skipping status pushes and a pushed exit.
    /// `None` when nothing arrives within `timeout`.
    ///
    /// # Panics
    ///
    /// Panics if a message other than a frame, a status push or an exit
    /// arrives.
    async fn read_part(&mut self, timeout: Duration) -> Option<Frame> {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match self.read_non_status(remaining).await? {
                DaemonMessage::Frame(frame) => return Some(frame),
                DaemonMessage::Exit { .. } => continue,
                other => panic!("expected a frame part, got {other:?}"),
            }
        }
    }

    fn apply_part(&mut self, part: &Frame) -> bool {
        if self.open_parts == 0 {
            self.open_reset = part.reset;
            self.open_rows = 0;
        }
        self.open_parts += 1;
        self.open_rows += part.lines.len();
        self.open_buffer.push(part.clone());
        let complete = self
            .grid
            .apply(part)
            .unwrap_or_else(|error| panic!("frame {} breaks a wire rule: {error:?}", part.seq));
        if complete {
            self.completed.push(CompletedFrame {
                seq: part.seq,
                parts: self.open_parts,
                reset: self.open_reset,
                rows_carried: self.open_rows,
            });
            self.open_parts = 0;
            self.last_frame_parts = std::mem::take(&mut self.open_buffer);
        }
        complete
    }

    /// Reads and applies the parts of the next frame up to its last part,
    /// without acknowledging it. Returns the parts.
    ///
    /// # Panics
    ///
    /// Panics if nothing arrives within `timeout`, or a part breaks a wire rule.
    pub async fn next_complete_frame(&mut self, timeout: Duration) -> Vec<Frame> {
        self.try_next_complete_frame(timeout)
            .await
            .unwrap_or_else(|| {
                panic!(
                    "no frame within {timeout:?}; the grid shows\n{}",
                    self.grid.dump()
                )
            })
    }

    /// [`ViewClient::next_complete_frame`], but `None` when the first part
    /// does not arrive within `timeout`.
    ///
    /// # Panics
    ///
    /// Panics if a frame began but its last part did not arrive within
    /// `ATTACH_TIMEOUT`, or a part breaks a wire rule.
    pub async fn try_next_complete_frame(&mut self, timeout: Duration) -> Option<Vec<Frame>> {
        let first = self.read_part(timeout).await?;
        let mut parts = Vec::new();
        let mut complete = self.apply_part(&first);
        parts.push(first);
        while !complete {
            let part = self.read_part(ATTACH_TIMEOUT).await.unwrap_or_else(|| {
                panic!("a frame began but its last part never came");
            });
            complete = self.apply_part(&part);
            parts.push(part);
        }
        Some(parts)
    }

    /// Acknowledges frame `seq`.
    pub async fn ack(&mut self, seq: u64) {
        send_client_message(&mut self.conn, &ClientMessage::FrameAck { seq }).await;
    }

    /// Sends `message` on the connection.
    pub async fn send(&mut self, message: &ClientMessage) {
        send_client_message(&mut self.conn, message).await;
    }

    /// Sends committed text.
    pub async fn send_text(&mut self, text: &str) {
        self.send(&ClientMessage::Text {
            text: text.to_string(),
        })
        .await;
    }

    /// Sends a key press.
    pub async fn send_key(&mut self, key: KeyInput) {
        self.send(&ClientMessage::Key(key)).await;
    }

    /// Sends a pointer event.
    pub async fn send_pointer(&mut self, pointer: PointerInput) {
        self.send(&ClientMessage::Pointer(pointer)).await;
    }

    /// Sends a wheel event.
    pub async fn send_wheel(&mut self, wheel: WheelInput) {
        self.send(&ClientMessage::Wheel(wheel)).await;
    }

    /// Applies and acknowledges frames until `done(grid)` holds. `done` is
    /// also tried on the grid as it stands before any frame is read.
    ///
    /// # Panics
    ///
    /// Panics, with `what` and the grid, if `done` does not hold within
    /// `timeout`.
    pub async fn pump_until(
        &mut self,
        what: &str,
        timeout: Duration,
        mut done: impl FnMut(&Grid) -> bool,
    ) {
        let deadline = Instant::now() + timeout;
        loop {
            if done(&self.grid) {
                return;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            let Some(parts) = self.try_next_complete_frame(remaining).await else {
                panic!(
                    "gave up waiting for {what} after {timeout:?}; the grid shows\n{}",
                    self.grid.dump()
                );
            };
            if let Some(last) = parts.last() {
                self.ack(last.seq).await;
            }
        }
    }

    /// Sends a `Ping` and returns once its `Pong` came back, applying and
    /// acknowledging every frame that arrived before it. Everything the
    /// daemon had ready when it handled the ping has then been received.
    ///
    /// # Panics
    ///
    /// Panics if the pong does not come, or anything unexpected arrives.
    pub async fn sync(&mut self) {
        self.send(&ClientMessage::Ping).await;
        let deadline = Instant::now() + ATTACH_TIMEOUT;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match self.read_non_status(remaining).await {
                Some(DaemonMessage::Pong) => return,
                Some(DaemonMessage::Exit { .. }) => continue,
                Some(DaemonMessage::Frame(frame)) => {
                    let seq = frame.seq;
                    if self.apply_part(&frame) {
                        self.ack(seq).await;
                    }
                }
                other => panic!("expected the pong, got {other:?}"),
            }
        }
    }

    /// Asks for the selected text and returns it (`None` when nothing is
    /// selected), applying and acknowledging the frames that arrive before the
    /// answer.
    ///
    /// # Panics
    ///
    /// Panics if no `selection_text` answer comes, or anything unexpected
    /// arrives.
    pub async fn copy_selection(&mut self) -> Option<String> {
        self.send(&ClientMessage::CopySelection).await;
        let deadline = Instant::now() + ATTACH_TIMEOUT;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match self.read_non_status(remaining).await {
                Some(DaemonMessage::SelectionText { text }) => return text,
                Some(DaemonMessage::Exit { .. }) => continue,
                Some(DaemonMessage::Frame(frame)) => {
                    let seq = frame.seq;
                    if self.apply_part(&frame) {
                        self.ack(seq).await;
                    }
                }
                other => panic!("expected selection_text, got {other:?}"),
            }
        }
    }

    /// Asserts that no frame arrives: a `Ping` is answered (everything the
    /// daemon had ready before it has been sent by then), and nothing more
    /// follows within `grace`.
    ///
    /// # Panics
    ///
    /// Panics if a frame part, or anything but the pong, arrives.
    pub async fn assert_no_frame(&mut self, why: &str, grace: Duration) {
        self.send(&ClientMessage::Ping).await;
        loop {
            match self.read_non_status(ATTACH_TIMEOUT).await {
                Some(DaemonMessage::Pong) => break,
                Some(DaemonMessage::Exit { .. }) => continue,
                Some(DaemonMessage::Frame(frame)) => {
                    panic!("a frame (seq {}) arrived but {why}", frame.seq)
                }
                other => panic!("expected the pong, got {other:?}"),
            }
        }
        if let Some(message) = self.read_non_status(grace).await
            && !matches!(message, DaemonMessage::Exit { .. })
        {
            panic!("{message:?} arrived after the pong but {why}");
        }
    }
}
