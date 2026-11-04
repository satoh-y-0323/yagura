//! The output a holder keeps: the latest checkpoint and every byte the child
//! wrote after it. Nothing in it is dropped for want of room; the holder stops
//! reading instead (see [`OutputState::reader_must_wait`]).

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

/// How far reading may run ahead of the connected daemon's `Ack`.
pub(crate) const ACK_WINDOW: u64 = 256 * 1024;

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
    /// Whether the reader is to stop reading the PTY for now. Once the child
    /// has exited everything left is read.
    pub(crate) fn reader_must_wait(&self) -> bool {
        if self.exit.is_some() {
            return false;
        }
        let window_full = self
            .ack_floor
            .is_some_and(|floor| self.buffer.end().saturating_sub(floor) >= ACK_WINDOW);
        window_full || self.buffer.pending() >= MAX_PENDING
    }

    /// Moves the acknowledged position forward, never past what was read.
    pub(crate) fn acknowledge(&mut self, seq: u64) {
        if let Some(floor) = self.ack_floor.as_mut() {
            *floor = (*floor).max(seq.min(self.buffer.end()));
        }
    }
}
