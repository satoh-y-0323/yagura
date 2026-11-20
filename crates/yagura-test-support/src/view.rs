//! A GUI stand-in for tests: attaches to a window over the real IPC endpoint,
//! applies the frames the daemon sends into a [`Grid`], and acknowledges them
//! the way a renderer does.

use tokio::time::Instant;
use yagura_protocol::{
    ClientMessage, DaemonMessage, Frame, KeyInput, PointerInput, TerminalSize, WheelInput,
};

use crate::HANG_GUARD;
use crate::frame::Grid;
use crate::ipc::{Connection, ReadOutcome, read_next_frame, send_client_message};

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
    /// Every part of the most recently completed frame, in arrival order.
    pub last_frame_parts: Vec<Frame>,
    open_buffer: Vec<Frame>,
    open_parts: usize,
    open_rows: usize,
    open_reset: bool,
}

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
        let deadline = Instant::now() + HANG_GUARD;
        loop {
            match self.read_non_status(deadline, "`attached`").await {
                Some(DaemonMessage::Attached {
                    child_exited,
                    exit_code,
                }) => {
                    self.child_exited = child_exited;
                    self.exit_code = exit_code;
                    break;
                }
                // A part of the previous attach's frame that was already in flight.
                Some(DaemonMessage::Frame(_)) => continue,
                other => panic!("expected `attached` after attach_view, got {other:?}"),
            }
        }
        let parts = self.next_complete_frame().await;
        assert!(
            parts.first().is_some_and(|part| part.reset),
            "the frame right after attach must replace the whole grid (reset)"
        );
    }

    /// Reads the next message that is not a `StatusChanged` push; `None` when
    /// the connection closed.
    ///
    /// # Panics
    ///
    /// Panics, naming `waiting_for` and showing the grid, if nothing arrives
    /// before `deadline`.
    async fn read_non_status(
        &mut self,
        deadline: Instant,
        waiting_for: &str,
    ) -> Option<DaemonMessage> {
        loop {
            let outcome = tokio::time::timeout_at(deadline, read_next_frame(&mut self.conn))
                .await
                .unwrap_or_else(|_| {
                    panic!(
                        "gave up waiting for {waiting_for} after {HANG_GUARD:?}; the grid shows\n{}",
                        self.grid.dump()
                    )
                });
            match outcome {
                ReadOutcome::Closed => return None,
                ReadOutcome::Message(DaemonMessage::StatusChanged { .. }) => continue,
                ReadOutcome::Message(message) => return Some(message),
            }
        }
    }

    /// Reads one part of a frame, skipping status pushes.
    ///
    /// # Panics
    ///
    /// Panics if a message other than a frame or a status push arrives, if
    /// the connection closed, or as [`ViewClient::read_non_status`] does.
    async fn read_part(&mut self, deadline: Instant, waiting_for: &str) -> Frame {
        match self.read_non_status(deadline, waiting_for).await {
            Some(DaemonMessage::Frame(frame)) => frame,
            other => panic!("expected a frame part, got {other:?}"),
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
    /// Panics if the frame has not completed within [`HANG_GUARD`], or a part
    /// breaks a wire rule.
    pub async fn next_complete_frame(&mut self) -> Vec<Frame> {
        self.complete_frame_until(Instant::now() + HANG_GUARD, "a frame")
            .await
    }

    async fn complete_frame_until(&mut self, deadline: Instant, waiting_for: &str) -> Vec<Frame> {
        let mut parts = Vec::new();
        loop {
            let part = self.read_part(deadline, waiting_for).await;
            let complete = self.apply_part(&part);
            parts.push(part);
            if complete {
                return parts;
            }
        }
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
    /// [`HANG_GUARD`].
    pub async fn pump_until(&mut self, what: &str, mut done: impl FnMut(&Grid) -> bool) {
        let deadline = Instant::now() + HANG_GUARD;
        loop {
            if done(&self.grid) {
                return;
            }
            let parts = self.complete_frame_until(deadline, what).await;
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
        let deadline = Instant::now() + HANG_GUARD;
        loop {
            match self.read_non_status(deadline, "the pong").await {
                Some(DaemonMessage::Pong) => return,
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
        let deadline = Instant::now() + HANG_GUARD;
        loop {
            match self.read_non_status(deadline, "selection_text").await {
                Some(DaemonMessage::SelectionText { text }) => return text,
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

    /// Asserts that no frame comes before the answer to a `Ping`: the daemon
    /// queues the `pong` after the frames for everything this attached
    /// window's feeder had received when it reached the ping (see
    /// [`ClientMessage::Ping`]), so a frame that is due comes first. What
    /// follows the pong is left unread.
    ///
    /// # Panics
    ///
    /// Panics if a frame part, or anything but the pong, arrives before the
    /// pong.
    pub async fn assert_no_frame(&mut self, why: &str) {
        self.send(&ClientMessage::Ping).await;
        let deadline = Instant::now() + HANG_GUARD;
        match self.read_non_status(deadline, "the pong").await {
            Some(DaemonMessage::Pong) => {}
            Some(DaemonMessage::Frame(frame)) => {
                panic!("a frame (seq {}) arrived but {why}", frame.seq)
            }
            other => panic!("expected the pong, got {other:?}"),
        }
    }
}
