//! The output a holder keeps: the latest checkpoint and every byte the child
//! wrote after it. Nothing in it is dropped for want of room; the holder stops
//! reading instead (see [`OutputState::read_permit`]).

use std::io::{self, Read};
use std::num::NonZeroUsize;

use yagura_protocol::MAX_WINDOW_DIMENSION;
use yagura_protocol::holder::HolderCheckpoint;

/// Bytes kept per cell of the largest screen after the checkpoint. A
/// placeholder until checkpoint sizes and repaint costs have been measured.
const BYTES_PER_CELL: usize = 8;

/// The cells of the largest screen; repainting it takes at least a byte each.
const LARGEST_SCREEN_CELLS: usize = MAX_WINDOW_DIMENSION as usize * MAX_WINDOW_DIMENSION as usize;

/// How many bytes after the checkpoint the holder keeps before it stops
/// reading: at least one repaint of the largest screen.
pub(crate) const MAX_PENDING: usize = LARGEST_SCREEN_CELLS * BYTES_PER_CELL;

const _: () = assert!(
    MAX_PENDING >= LARGEST_SCREEN_CELLS,
    "the kept output must hold one repaint of the largest screen"
);

/// The capacity kept however little is buffered, so a quiet window does not
/// reallocate on every read.
const MIN_CAPACITY: usize = 64 * 1024;

/// How far reading may run ahead of the connected daemon's `Ack`, in bytes;
/// the holder names it to the daemon in its `Welcome`.
pub const ACK_WINDOW: u64 = 256 * 1024;

/// Bytes the reader takes from the terminal at a time, at most.
const READ_CHUNK: usize = 8192;

/// The checkpoint and the bytes after it.
#[derive(Default)]
pub(crate) struct OutputBuffer {
    checkpoint: Option<HolderCheckpoint>,
    /// The position of `bytes[0]`; equal to the checkpoint's `seq` when there
    /// is one.
    start: u64,
    bytes: Vec<u8>,
}

impl OutputBuffer {
    pub(crate) fn start(&self) -> u64 {
        self.start
    }

    pub(crate) fn end(&self) -> u64 {
        self.start + self.bytes.len() as u64
    }

    pub(crate) fn pending(&self) -> usize {
        self.bytes.len()
    }

    pub(crate) fn checkpoint(&self) -> Option<&HolderCheckpoint> {
        self.checkpoint.as_ref()
    }

    pub(crate) fn append(&mut self, bytes: &[u8]) {
        self.bytes.extend_from_slice(bytes);
    }

    /// Keeps `bytes` as the checkpoint at `seq` and lets go of the output
    /// before it. A `seq` outside the kept output, or older than the kept
    /// checkpoint, is ignored: the bytes it would describe are not all here.
    pub(crate) fn store_checkpoint(&mut self, seq: u64, bytes: Vec<u8>) {
        if seq < self.start || seq > self.end() {
            return;
        }
        self.drop_before(seq);
        self.checkpoint = Some(HolderCheckpoint { seq, bytes });
    }

    /// Lets go of the output before `seq` with no checkpoint for it, so a
    /// replay starts at `seq`. Provisional: only the end-child-on-disconnect
    /// launch, whose daemon never comes back for it, frees output this way.
    pub(crate) fn release(&mut self, seq: u64) {
        if seq <= self.start || seq > self.end() {
            return;
        }
        self.drop_before(seq);
        self.checkpoint = None;
    }

    fn drop_before(&mut self, seq: u64) {
        let drop_len = usize::try_from(seq - self.start).unwrap_or(self.bytes.len());
        self.bytes.drain(..drop_len.min(self.bytes.len()));
        // Give back what a burst grew, so a window holds memory for what it keeps.
        let keep = self.bytes.len().max(MIN_CAPACITY);
        if self.bytes.capacity() > keep.saturating_mul(2) {
            self.bytes.shrink_to(keep);
        }
        self.start = seq;
    }

    /// The kept output from `from` to `to`, both clamped to what is kept.
    pub(crate) fn range(&self, from: u64, to: u64) -> &[u8] {
        let offset = |seq: u64| {
            usize::try_from(seq.clamp(self.start, self.end()) - self.start)
                .unwrap_or(self.bytes.len())
        };
        let (from, to) = (offset(from), offset(to));
        self.bytes.get(from..to.max(from)).unwrap_or(&[])
    }
}

/// What the PTY reader, the exit monitor and the holder's core share.
#[derive(Default)]
pub(crate) struct OutputState {
    pub(crate) buffer: OutputBuffer,
    /// The connected daemon's acknowledged position; `None` while no daemon
    /// is connected.
    pub(crate) ack_floor: Option<u64>,
    /// The child's exit, once observed: its code when known.
    pub(crate) exit: Option<Option<i32>>,
    /// The PTY's output has ended.
    pub(crate) reader_done: bool,
}

impl OutputState {
    /// How much the reader may read now: no more than keeps the output within
    /// the connected daemon's window and within [`MAX_PENDING`], or `None`
    /// when it is to wait. Once the child has exited everything left is read.
    pub(crate) fn read_permit(&self) -> Option<ReadPermit> {
        let room = if self.exit.is_some() {
            READ_CHUNK
        } else {
            let kept_room = MAX_PENDING.saturating_sub(self.buffer.pending());
            let window_room = self.ack_floor.map_or(usize::MAX, |floor| {
                let ahead = self.buffer.end().saturating_sub(floor);
                usize::try_from(ACK_WINDOW.saturating_sub(ahead)).unwrap_or(usize::MAX)
            });
            READ_CHUNK.min(kept_room).min(window_room)
        };
        NonZeroUsize::new(room).map(|len| ReadPermit { len })
    }

    /// Moves the acknowledged position forward, never past what was read.
    pub(crate) fn acknowledge(&mut self, seq: u64) {
        if let Some(floor) = self.ack_floor.as_mut() {
            *floor = (*floor).max(seq.min(self.buffer.end()));
        }
    }
}

/// Leave to read up to [`ReadPermit::len`] bytes from the terminal; only
/// [`OutputState::read_permit`] makes one.
pub(crate) struct ReadPermit {
    len: NonZeroUsize,
}

impl ReadPermit {
    /// The bytes that may be read, at least one.
    pub(crate) fn len(&self) -> usize {
        self.len.get()
    }
}

/// The terminal's output, read only with a [`ReadPermit`], so no read can
/// skip the decision on how much to take.
pub(crate) struct PtyReader {
    reader: Box<dyn Read + Send>,
    buf: Vec<u8>,
}

impl PtyReader {
    pub(crate) fn new(reader: Box<dyn Read + Send>) -> Self {
        Self {
            reader,
            buf: vec![0u8; READ_CHUNK],
        }
    }

    /// Reads at most what `permit` allows; an empty slice is the end of output.
    pub(crate) fn read(&mut self, permit: ReadPermit) -> io::Result<&[u8]> {
        let len = permit.len().min(self.buf.len());
        let read = self.reader.read(&mut self.buf[..len])?;
        Ok(&self.buf[..read.min(len)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(state: &mut OutputState, len: usize) {
        state.buffer.append(&vec![b'x'; len]);
    }

    fn window_bytes() -> usize {
        usize::try_from(ACK_WINDOW).unwrap_or(usize::MAX)
    }

    /// How much the reader may read now; `0` when it must wait.
    fn allowed(state: &OutputState) -> usize {
        state.read_permit().map_or(0, |permit| permit.len())
    }

    #[test]
    fn a_connected_reader_may_read_up_to_the_window_past_the_ack_and_no_further() {
        let mut state = OutputState {
            ack_floor: Some(0),
            ..OutputState::default()
        };
        let fresh = allowed(&state);
        assert!(
            (1..=window_bytes()).contains(&fresh),
            "an empty window allows {fresh} bytes"
        );

        read(&mut state, window_bytes() - 1);
        assert_eq!(allowed(&state), 1, "the last byte of the window");

        read(&mut state, 1);
        assert!(state.read_permit().is_none(), "read on past the window");

        state.acknowledge(1);
        assert_eq!(allowed(&state), 1, "an ack of one byte allows one byte");
    }

    #[test]
    fn a_reader_with_no_daemon_may_read_up_to_the_kept_output_bound_and_no_further() {
        let mut state = OutputState::default();
        read(&mut state, window_bytes());
        assert!(
            allowed(&state) >= 1,
            "the ack window held a reader with no daemon connected"
        );

        read(&mut state, MAX_PENDING - 1 - window_bytes());
        assert_eq!(allowed(&state), 1, "the last byte before MAX_PENDING");

        read(&mut state, 1);
        assert!(state.read_permit().is_none(), "read on past MAX_PENDING");
    }

    #[test]
    fn after_the_child_exited_the_reader_reads_everything_left() {
        let mut state = OutputState {
            ack_floor: Some(0),
            ..OutputState::default()
        };
        read(&mut state, MAX_PENDING);
        assert!(
            state.read_permit().is_none(),
            "the full state did not stop it"
        );

        state.exit = Some(None);
        assert!(
            allowed(&state) >= 1,
            "a reader was held after the child exited"
        );
    }
}
