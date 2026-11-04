//! The wire between the daemon and a window's holder, and the files a holder
//! leaves in the `holders` directory its launch names.
//!
//! A frame is a `u32` LE length of what follows, a `u16` LE type and a body.
//! The body of a byte-carrying frame ([`HolderFrame::Checkpoint`],
//! [`HolderFrame::Output`], [`HolderFrame::StoreCheckpoint`],
//! [`HolderFrame::Write`]) is a `u64` LE sequence or id followed by the raw
//! bytes; every other body is a JSON object, to which a later version may add
//! fields but never remove one or change its meaning.
//!
//! The fixed frames ([`HolderFrame::Hello`], [`HolderFrame::Terminate`],
//! [`HolderFrame::Exited`], [`HolderFrame::Displaced`],
//! [`HolderFrame::VersionRejected`]) keep their type numbers and bodies in
//! every version, so a daemon and a holder of any two versions can still
//! close a window and learn of its exit.
//!
//! A stream position (`seq`) counts the bytes the child has written since it
//! started. An `Output`'s `seq` is the position just past its bytes; `Ack`,
//! `Checkpoint` and `StoreCheckpoint` name positions the same way.

use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The holder wire version this build speaks.
pub const HOLDER_PROTOCOL: u32 = 1;

/// The largest `length` a frame may declare. A peer declaring more is broken
/// or hostile, and is refused before anything is buffered for it.
pub const MAX_HOLDER_FRAME_LEN: usize = 64 * 1024 * 1024;

/// The longest `hello_deadline_ms` a holder honours: long enough for any
/// daemon start, short enough that an unclaimed holder does not linger for
/// hours. A longer request is cut to this.
pub const MAX_HELLO_DEADLINE_MS: u64 = 10 * 60 * 1000;

const TYPE_HELLO: u16 = 1;
const TYPE_TERMINATE: u16 = 2;
const TYPE_EXITED: u16 = 3;
const TYPE_DISPLACED: u16 = 4;
const TYPE_VERSION_REJECTED: u16 = 5;
const TYPE_WELCOME: u16 = 6;
const TYPE_REPLAY: u16 = 7;
const TYPE_CHECKPOINT: u16 = 8;
const TYPE_OUTPUT: u16 = 9;
const TYPE_ACK: u16 = 10;
const TYPE_STORE_CHECKPOINT: u16 = 11;
const TYPE_WRITE: u16 = 12;
const TYPE_WRITTEN: u16 = 13;
const TYPE_WRITE_FAILED: u16 = 14;
const TYPE_RESIZE: u16 = 15;
const TYPE_RESIZED: u16 = 16;
const TYPE_RESIZE_FAILED: u16 = 17;
const TYPE_SPAWN_FAILED: u16 = 18;

/// One frame between the daemon (`d`) and a holder (`h`).
#[derive(Clone, PartialEq, Eq)]
pub enum HolderFrame {
    /// d -> h, fixed: the daemon names itself. The first frame of a connection.
    Hello {
        /// The holder wire version the daemon speaks.
        protocol: u32,
        /// Identifies the daemon's run.
        daemon_boot: String,
    },
    /// d -> h, fixed: end the child (and with `tree`, everything it started at
    /// once), then the holder ends.
    Terminate {
        /// End the child's whole process tree now rather than after the child.
        tree: bool,
    },
    /// h -> d, fixed: the child has exited, with its exit code when known.
    Exited {
        /// The child's exit code.
        code: Option<i32>,
    },
    /// h -> d, fixed: another daemon has taken the holder over; this
    /// connection is closed next and is not to be reopened.
    Displaced,
    /// h -> d, fixed: the holder does not speak the `Hello`'s version.
    VersionRejected {
        /// The version the holder speaks.
        protocol: u32,
    },
    /// h -> d: the `Hello` is accepted.
    Welcome {
        /// The holder wire version.
        protocol: u32,
        /// The window's record the daemon handed over at launch, unchanged.
        record: String,
        /// The terminal's columns.
        cols: u16,
        /// The terminal's rows.
        rows: u16,
        /// Whether the child has already exited.
        exited: bool,
        /// The position of the checkpoint the holder keeps, if any.
        checkpoint_seq: Option<u64>,
        /// The end of the replay [`HolderFrame::Replay`] asks for: output up
        /// to here was read before this connection.
        replay_end_seq: u64,
    },
    /// d -> h: send the checkpoint, every byte after it, then the live output.
    Replay,
    /// h -> d: the kept checkpoint.
    Checkpoint {
        /// The position the checkpoint describes.
        seq: u64,
        /// The checkpoint, opaque to the holder.
        bytes: Vec<u8>,
    },
    /// h -> d: output of the child.
    Output {
        /// The position just past `bytes`.
        seq: u64,
        /// The bytes, as the terminal produced them.
        bytes: Vec<u8>,
    },
    /// d -> h: output up to `seq` has been taken in.
    Ack {
        /// The position taken in.
        seq: u64,
    },
    /// d -> h: keep `bytes` as the checkpoint at `seq`; output before it is
    /// no longer needed.
    StoreCheckpoint {
        /// The position the checkpoint describes.
        seq: u64,
        /// The checkpoint, opaque to the holder.
        bytes: Vec<u8>,
    },
    /// d -> h: write `bytes` to the child, after every earlier write.
    Write {
        /// Identifies the write in its answer.
        id: u64,
        /// The bytes to write.
        bytes: Vec<u8>,
    },
    /// h -> d: the write `id` reached the child's input.
    Written {
        /// The write.
        id: u64,
    },
    /// h -> d: the write `id` did not reach the child.
    WriteFailed {
        /// The write.
        id: u64,
        /// Why.
        reason: String,
    },
    /// d -> h: change the terminal's size.
    Resize {
        /// Columns.
        cols: u16,
        /// Rows.
        rows: u16,
    },
    /// h -> d: the terminal now has the size a `Resize` asked for.
    Resized {
        /// Columns.
        cols: u16,
        /// Rows.
        rows: u16,
    },
    /// h -> d: the terminal kept its size; the `Resize` asking for this one
    /// was refused.
    ResizeFailed {
        /// The columns asked for.
        cols: u16,
        /// The rows asked for.
        rows: u16,
        /// Why.
        reason: String,
    },
    /// h -> d: the child could not be started. Sent before the `Exited` that
    /// answers the first `Hello` in place of a `Welcome`.
    SpawnFailed {
        /// Why.
        reason: HolderSpawnFailure,
    },
}

/// Why a holder could not start its child.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HolderSpawnFailure {
    /// The pseudoconsole bundled beside the holder is missing, altered, or
    /// cannot be loaded.
    BundledConptyUnavailable,
    /// Any other failure to start the child.
    Spawn,
}

// The window's record carries its token and a write carries what the user
// typed, so neither is shown; byte frames show their length only.
impl std::fmt::Debug for HolderFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Hello {
                protocol,
                daemon_boot,
            } => f
                .debug_struct("Hello")
                .field("protocol", protocol)
                .field("daemon_boot", daemon_boot)
                .finish(),
            Self::Terminate { tree } => f.debug_struct("Terminate").field("tree", tree).finish(),
            Self::Exited { code } => f.debug_struct("Exited").field("code", code).finish(),
            Self::Displaced => f.write_str("Displaced"),
            Self::VersionRejected { protocol } => f
                .debug_struct("VersionRejected")
                .field("protocol", protocol)
                .finish(),
            Self::Welcome {
                protocol,
                record,
                cols,
                rows,
                exited,
                checkpoint_seq,
                replay_end_seq,
            } => f
                .debug_struct("Welcome")
                .field("protocol", protocol)
                .field("record_len", &record.len())
                .field("cols", cols)
                .field("rows", rows)
                .field("exited", exited)
                .field("checkpoint_seq", checkpoint_seq)
                .field("replay_end_seq", replay_end_seq)
                .finish(),
            Self::Replay => f.write_str("Replay"),
            Self::Checkpoint { seq, bytes } => byte_frame(f, "Checkpoint", "seq", *seq, bytes),
            Self::Output { seq, bytes } => byte_frame(f, "Output", "seq", *seq, bytes),
            Self::Ack { seq } => f.debug_struct("Ack").field("seq", seq).finish(),
            Self::StoreCheckpoint { seq, bytes } => {
                byte_frame(f, "StoreCheckpoint", "seq", *seq, bytes)
            }
            Self::Write { id, bytes } => byte_frame(f, "Write", "id", *id, bytes),
            Self::Written { id } => f.debug_struct("Written").field("id", id).finish(),
            Self::WriteFailed { id, reason } => f
                .debug_struct("WriteFailed")
                .field("id", id)
                .field("reason", reason)
                .finish(),
            Self::Resize { cols, rows } => f
                .debug_struct("Resize")
                .field("cols", cols)
                .field("rows", rows)
                .finish(),
            Self::Resized { cols, rows } => f
                .debug_struct("Resized")
                .field("cols", cols)
                .field("rows", rows)
                .finish(),
            Self::ResizeFailed { cols, rows, reason } => f
                .debug_struct("ResizeFailed")
                .field("cols", cols)
                .field("rows", rows)
                .field("reason", reason)
                .finish(),
            Self::SpawnFailed { reason } => f
                .debug_struct("SpawnFailed")
                .field("reason", reason)
                .finish(),
        }
    }
}

fn byte_frame(
    f: &mut std::fmt::Formatter<'_>,
    name: &str,
    key: &str,
    value: u64,
    bytes: &[u8],
) -> std::fmt::Result {
    f.debug_struct(name)
        .field(key, &value)
        .field("len", &bytes.len())
        .finish()
}

/// Why a frame, a launch or a holder's file could not be read.
#[derive(Debug, thiserror::Error)]
pub enum HolderCodecError {
    /// The declared length is too short to hold a type.
    #[error("a holder frame declares {0} bytes, too few for its type")]
    TooShort(usize),
    /// The declared length is above [`MAX_HOLDER_FRAME_LEN`].
    #[error("a holder frame declares {0} bytes, more than a frame may hold")]
    TooLong(usize),
    /// The type number is not one this version knows.
    #[error("unknown holder frame type {0}")]
    UnknownType(u16),
    /// A byte-carrying body is too short to hold its sequence or id.
    #[error("the body of holder frame type {0} is too short")]
    ShortBody(u16),
    /// A JSON body is not valid for its type.
    #[error("the body of holder frame type {frame_type} is invalid: {source}")]
    Json {
        /// The frame's type number.
        frame_type: u16,
        /// The JSON error.
        source: serde_json::Error,
    },
    /// A holder's launch is not valid.
    #[error("the holder launch is invalid: {0}")]
    Launch(serde_json::Error),
    /// A holder's exit file does not hold an exit.
    #[error("the holder exit file is malformed")]
    MalformedExit,
}

#[derive(Serialize, Deserialize)]
struct HelloBody {
    protocol: u32,
    daemon_boot: String,
}

#[derive(Serialize, Deserialize)]
struct TerminateBody {
    tree: bool,
}

#[derive(Serialize, Deserialize)]
struct ExitedBody {
    code: Option<i32>,
}

#[derive(Serialize, Deserialize)]
struct EmptyBody {}

#[derive(Serialize, Deserialize)]
struct VersionBody {
    protocol: u32,
}

#[derive(Serialize, Deserialize)]
struct WelcomeBody {
    protocol: u32,
    record: String,
    cols: u16,
    rows: u16,
    exited: bool,
    checkpoint_seq: Option<u64>,
    replay_end_seq: u64,
}

#[derive(Serialize, Deserialize)]
struct SeqBody {
    seq: u64,
}

#[derive(Serialize, Deserialize)]
struct IdBody {
    id: u64,
}

#[derive(Serialize, Deserialize)]
struct WriteFailedBody {
    id: u64,
    reason: String,
}

#[derive(Serialize, Deserialize)]
struct ResizeBody {
    cols: u16,
    rows: u16,
}

#[derive(Serialize, Deserialize)]
struct ResizeFailedBody {
    cols: u16,
    rows: u16,
    reason: String,
}

#[derive(Serialize, Deserialize)]
struct SpawnFailedBody {
    reason: HolderSpawnFailure,
}

fn frame(frame_type: u16, body: &[&[u8]]) -> Vec<u8> {
    let body_len: usize = body.iter().map(|part| part.len()).sum();
    // A frame over u32::MAX cannot be read back anyway: the reader refuses
    // anything above MAX_HOLDER_FRAME_LEN.
    let len = u32::try_from(2 + body_len).unwrap_or(u32::MAX);
    let mut out = Vec::with_capacity(6 + body_len);
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(&frame_type.to_le_bytes());
    for part in body {
        out.extend_from_slice(part);
    }
    out
}

fn json_frame<T: Serialize>(frame_type: u16, body: &T) -> Vec<u8> {
    // These bodies are plain structs of numbers, strings and options, which
    // serde_json always serializes; an empty body would be refused by the reader.
    let json = serde_json::to_vec(body).unwrap_or_default();
    frame(frame_type, &[&json])
}

fn bytes_frame(frame_type: u16, number: u64, bytes: &[u8]) -> Vec<u8> {
    frame(frame_type, &[&number.to_le_bytes(), bytes])
}

/// Encodes `frame` for the wire.
#[must_use]
pub fn encode_holder_frame(frame: &HolderFrame) -> Vec<u8> {
    match frame {
        HolderFrame::Hello {
            protocol,
            daemon_boot,
        } => json_frame(
            TYPE_HELLO,
            &HelloBody {
                protocol: *protocol,
                daemon_boot: daemon_boot.clone(),
            },
        ),
        HolderFrame::Terminate { tree } => {
            json_frame(TYPE_TERMINATE, &TerminateBody { tree: *tree })
        }
        HolderFrame::Exited { code } => json_frame(TYPE_EXITED, &ExitedBody { code: *code }),
        HolderFrame::Displaced => json_frame(TYPE_DISPLACED, &EmptyBody {}),
        HolderFrame::VersionRejected { protocol } => json_frame(
            TYPE_VERSION_REJECTED,
            &VersionBody {
                protocol: *protocol,
            },
        ),
        HolderFrame::Welcome {
            protocol,
            record,
            cols,
            rows,
            exited,
            checkpoint_seq,
            replay_end_seq,
        } => json_frame(
            TYPE_WELCOME,
            &WelcomeBody {
                protocol: *protocol,
                record: record.clone(),
                cols: *cols,
                rows: *rows,
                exited: *exited,
                checkpoint_seq: *checkpoint_seq,
                replay_end_seq: *replay_end_seq,
            },
        ),
        HolderFrame::Replay => json_frame(TYPE_REPLAY, &EmptyBody {}),
        HolderFrame::Checkpoint { seq, bytes } => bytes_frame(TYPE_CHECKPOINT, *seq, bytes),
        HolderFrame::Output { seq, bytes } => bytes_frame(TYPE_OUTPUT, *seq, bytes),
        HolderFrame::Ack { seq } => json_frame(TYPE_ACK, &SeqBody { seq: *seq }),
        HolderFrame::StoreCheckpoint { seq, bytes } => {
            bytes_frame(TYPE_STORE_CHECKPOINT, *seq, bytes)
        }
        HolderFrame::Write { id, bytes } => bytes_frame(TYPE_WRITE, *id, bytes),
        HolderFrame::Written { id } => json_frame(TYPE_WRITTEN, &IdBody { id: *id }),
        HolderFrame::WriteFailed { id, reason } => json_frame(
            TYPE_WRITE_FAILED,
            &WriteFailedBody {
                id: *id,
                reason: reason.clone(),
            },
        ),
        HolderFrame::Resize { cols, rows } => json_frame(
            TYPE_RESIZE,
            &ResizeBody {
                cols: *cols,
                rows: *rows,
            },
        ),
        HolderFrame::Resized { cols, rows } => json_frame(
            TYPE_RESIZED,
            &ResizeBody {
                cols: *cols,
                rows: *rows,
            },
        ),
        HolderFrame::ResizeFailed { cols, rows, reason } => json_frame(
            TYPE_RESIZE_FAILED,
            &ResizeFailedBody {
                cols: *cols,
                rows: *rows,
                reason: reason.clone(),
            },
        ),
        HolderFrame::SpawnFailed { reason } => {
            json_frame(TYPE_SPAWN_FAILED, &SpawnFailedBody { reason: *reason })
        }
    }
}

/// Decodes the first frame of `input`.
///
/// Returns `Ok(None)` while `input` does not yet hold a whole frame, and
/// otherwise the frame and the number of bytes it took, leaving any bytes
/// after it for the next call.
///
/// # Errors
///
/// Returns [`HolderCodecError`] when the declared length is below the type's
/// two bytes or above [`MAX_HOLDER_FRAME_LEN`], the type is unknown, or the
/// body is not valid for its type.
pub fn decode_holder_frame(input: &[u8]) -> Result<Option<(HolderFrame, usize)>, HolderCodecError> {
    let Some(len_bytes) = input.get(..4) else {
        return Ok(None);
    };
    let mut len_le = [0u8; 4];
    len_le.copy_from_slice(len_bytes);
    let len = usize::try_from(u32::from_le_bytes(len_le)).unwrap_or(usize::MAX);
    if len < 2 {
        return Err(HolderCodecError::TooShort(len));
    }
    if len > MAX_HOLDER_FRAME_LEN {
        return Err(HolderCodecError::TooLong(len));
    }
    let Some(whole) = input.get(4..4 + len) else {
        return Ok(None);
    };
    let frame_type = u16::from_le_bytes([whole[0], whole[1]]);
    let body = &whole[2..];
    let frame = decode_body(frame_type, body)?;
    Ok(Some((frame, 4 + len)))
}

/// The capacity a [`HolderFrameBuffer`] keeps however little it holds.
const MIN_FRAME_BUFFER_CAPACITY: usize = 64 * 1024;

/// Bytes read from a connection, cut into frames as they complete. Holds no
/// I/O: the reader pushes what it read and takes the frames out.
#[derive(Default)]
pub struct HolderFrameBuffer {
    bytes: Vec<u8>,
    /// Where the next frame starts in `bytes`; what is before it was taken.
    start: usize,
}

impl HolderFrameBuffer {
    /// An empty buffer.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends bytes read from the connection.
    pub fn push(&mut self, bytes: &[u8]) {
        if self.start > 0 {
            self.bytes.drain(..self.start);
            self.start = 0;
            // Give back what one large frame grew, so a connection holds
            // memory for what is still buffered.
            let keep = self.bytes.len().max(MIN_FRAME_BUFFER_CAPACITY);
            if self.bytes.capacity() > keep.saturating_mul(2) {
                self.bytes.shrink_to(keep);
            }
        }
        self.bytes.extend_from_slice(bytes);
    }

    /// Takes the next whole frame, or `Ok(None)` until more bytes are pushed.
    ///
    /// # Errors
    ///
    /// As for [`decode_holder_frame`]. The stream is broken from there on and
    /// the connection is to be closed.
    pub fn next_frame(&mut self) -> Result<Option<HolderFrame>, HolderCodecError> {
        let rest = self.bytes.get(self.start..).unwrap_or(&[]);
        let Some((frame, used)) = decode_holder_frame(rest)? else {
            return Ok(None);
        };
        self.start += used;
        Ok(Some(frame))
    }

    /// How many bytes are buffered and not yet taken as frames.
    #[must_use]
    pub fn buffered(&self) -> usize {
        self.bytes.len() - self.start
    }
}

impl std::fmt::Debug for HolderFrameBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HolderFrameBuffer")
            .field("buffered", &self.buffered())
            .finish()
    }
}

fn json<'a, T: Deserialize<'a>>(frame_type: u16, body: &'a [u8]) -> Result<T, HolderCodecError> {
    serde_json::from_slice(body).map_err(|source| HolderCodecError::Json { frame_type, source })
}

fn split_number(frame_type: u16, body: &[u8]) -> Result<(u64, Vec<u8>), HolderCodecError> {
    let (head, rest) = body
        .split_first_chunk::<8>()
        .ok_or(HolderCodecError::ShortBody(frame_type))?;
    Ok((u64::from_le_bytes(*head), rest.to_vec()))
}

fn decode_body(frame_type: u16, body: &[u8]) -> Result<HolderFrame, HolderCodecError> {
    Ok(match frame_type {
        TYPE_HELLO => {
            let HelloBody {
                protocol,
                daemon_boot,
            } = json(frame_type, body)?;
            HolderFrame::Hello {
                protocol,
                daemon_boot,
            }
        }
        TYPE_TERMINATE => {
            let TerminateBody { tree } = json(frame_type, body)?;
            HolderFrame::Terminate { tree }
        }
        TYPE_EXITED => {
            let ExitedBody { code } = json(frame_type, body)?;
            HolderFrame::Exited { code }
        }
        TYPE_DISPLACED => {
            let EmptyBody {} = json(frame_type, body)?;
            HolderFrame::Displaced
        }
        TYPE_VERSION_REJECTED => {
            let VersionBody { protocol } = json(frame_type, body)?;
            HolderFrame::VersionRejected { protocol }
        }
        TYPE_WELCOME => {
            let WelcomeBody {
                protocol,
                record,
                cols,
                rows,
                exited,
                checkpoint_seq,
                replay_end_seq,
            } = json(frame_type, body)?;
            HolderFrame::Welcome {
                protocol,
                record,
                cols,
                rows,
                exited,
                checkpoint_seq,
                replay_end_seq,
            }
        }
        TYPE_REPLAY => {
            let EmptyBody {} = json(frame_type, body)?;
            HolderFrame::Replay
        }
        TYPE_CHECKPOINT => {
            let (seq, bytes) = split_number(frame_type, body)?;
            HolderFrame::Checkpoint { seq, bytes }
        }
        TYPE_OUTPUT => {
            let (seq, bytes) = split_number(frame_type, body)?;
            HolderFrame::Output { seq, bytes }
        }
        TYPE_ACK => {
            let SeqBody { seq } = json(frame_type, body)?;
            HolderFrame::Ack { seq }
        }
        TYPE_STORE_CHECKPOINT => {
            let (seq, bytes) = split_number(frame_type, body)?;
            HolderFrame::StoreCheckpoint { seq, bytes }
        }
        TYPE_WRITE => {
            let (id, bytes) = split_number(frame_type, body)?;
            HolderFrame::Write { id, bytes }
        }
        TYPE_WRITTEN => {
            let IdBody { id } = json(frame_type, body)?;
            HolderFrame::Written { id }
        }
        TYPE_WRITE_FAILED => {
            let WriteFailedBody { id, reason } = json(frame_type, body)?;
            HolderFrame::WriteFailed { id, reason }
        }
        TYPE_RESIZE => {
            let ResizeBody { cols, rows } = json(frame_type, body)?;
            HolderFrame::Resize { cols, rows }
        }
        TYPE_RESIZED => {
            let ResizeBody { cols, rows } = json(frame_type, body)?;
            HolderFrame::Resized { cols, rows }
        }
        TYPE_RESIZE_FAILED => {
            let ResizeFailedBody { cols, rows, reason } = json(frame_type, body)?;
            HolderFrame::ResizeFailed { cols, rows, reason }
        }
        TYPE_SPAWN_FAILED => {
            let SpawnFailedBody { reason } = json(frame_type, body)?;
            HolderFrame::SpawnFailed { reason }
        }
        other => return Err(HolderCodecError::UnknownType(other)),
    })
}

/// A change to the environment the holder's child starts with, applied in
/// order on top of the holder's own environment.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum HolderEnvOp {
    /// Sets a variable.
    Set {
        /// The name.
        name: OsString,
        /// The value.
        value: OsString,
    },
    /// Removes a variable if it is there.
    Remove {
        /// The name.
        name: OsString,
    },
}

impl std::fmt::Debug for HolderEnvOp {
    // An environment routinely carries tokens, so neither name nor value is shown.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Set { .. } => f.write_str("HolderEnvOp::Set(..)"),
            Self::Remove { .. } => f.write_str("HolderEnvOp::Remove(..)"),
        }
    }
}

/// What a holder starts and keeps, handed to it on its standard input (never
/// on its command line, which other processes can read).
#[derive(Clone, PartialEq, Eq)]
pub struct HolderLaunch {
    /// The program followed by its arguments.
    pub argv: Vec<String>,
    /// The working directory; `None` is the user's home directory.
    pub cwd: Option<PathBuf>,
    /// Changes to the child's environment.
    pub env: Vec<HolderEnvOp>,
    /// The terminal's initial columns.
    pub cols: u16,
    /// The terminal's initial rows.
    pub rows: u16,
    /// The window's record, kept in memory only and returned in
    /// [`HolderFrame::Welcome`]. Opaque to the holder.
    pub record: String,
    /// How long, from its start, the holder waits for the first accepted
    /// `Hello` before it ends without starting the child. A value above
    /// [`MAX_HELLO_DEADLINE_MS`] is cut to it.
    pub hello_deadline_ms: u64,
    /// The directory the holder keeps its record, its exit file and, on Unix,
    /// its socket in. It must be an absolute path to a directory private to
    /// the user; the holder creates it when only it is missing.
    pub holders_dir: PathBuf,
    /// The endpoint whose server is the daemon: only the process serving it
    /// when a `Hello` arrives is let in.
    pub daemon_endpoint: String,
    /// Provisional, removed once holders outlive the daemon: a lost
    /// connection other than [`HolderFrame::Displaced`] ends the child's tree
    /// and the holder without leaving any file, and an `Ack` frees the output
    /// it acknowledges.
    pub end_child_on_disconnect: bool,
}

impl std::fmt::Debug for HolderLaunch {
    // The record carries the window's token, and the environment may too.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HolderLaunch")
            .field("argv_len", &self.argv.len())
            .field("cols", &self.cols)
            .field("rows", &self.rows)
            .field("hello_deadline_ms", &self.hello_deadline_ms)
            .field("holders_dir", &self.holders_dir)
            .field("daemon_endpoint", &self.daemon_endpoint)
            .field("end_child_on_disconnect", &self.end_child_on_disconnect)
            .finish_non_exhaustive()
    }
}

#[derive(Serialize)]
struct LaunchOut<'a> {
    argv: &'a [String],
    cwd: Option<&'a OsStr>,
    env: &'a [HolderEnvOp],
    cols: u16,
    rows: u16,
    record: &'a str,
    hello_deadline_ms: u64,
    holders_dir: &'a OsStr,
    daemon_endpoint: &'a str,
    end_child_on_disconnect: bool,
}

#[derive(Deserialize)]
struct LaunchIn {
    argv: Vec<String>,
    cwd: Option<OsString>,
    env: Vec<HolderEnvOp>,
    cols: u16,
    rows: u16,
    record: String,
    hello_deadline_ms: u64,
    holders_dir: OsString,
    daemon_endpoint: String,
    end_child_on_disconnect: bool,
}

/// Encodes `launch` for a holder's standard input. Paths and environment
/// values keep their platform form, so a value that is not Unicode survives.
#[must_use]
pub fn encode_holder_launch(launch: &HolderLaunch) -> Vec<u8> {
    let out = LaunchOut {
        argv: &launch.argv,
        cwd: launch.cwd.as_deref().map(Path::as_os_str),
        env: &launch.env,
        cols: launch.cols,
        rows: launch.rows,
        record: &launch.record,
        hello_deadline_ms: launch.hello_deadline_ms,
        holders_dir: launch.holders_dir.as_os_str(),
        daemon_endpoint: &launch.daemon_endpoint,
        end_child_on_disconnect: launch.end_child_on_disconnect,
    };
    // Only strings, numbers and OS strings, which serde_json always encodes;
    // an empty launch would be refused by the holder.
    serde_json::to_vec(&out).unwrap_or_default()
}

/// Decodes what [`encode_holder_launch`] wrote.
///
/// # Errors
///
/// Returns [`HolderCodecError::Launch`] when `input` is not a launch.
pub fn decode_holder_launch(input: &[u8]) -> Result<HolderLaunch, HolderCodecError> {
    let LaunchIn {
        argv,
        cwd,
        env,
        cols,
        rows,
        record,
        hello_deadline_ms,
        holders_dir,
        daemon_endpoint,
        end_child_on_disconnect,
    } = serde_json::from_slice(input).map_err(HolderCodecError::Launch)?;
    Ok(HolderLaunch {
        argv,
        cwd: cwd.map(PathBuf::from),
        env,
        cols,
        rows,
        record,
        hello_deadline_ms,
        holders_dir: PathBuf::from(holders_dir),
        daemon_endpoint,
        end_child_on_disconnect,
    })
}

/// What a running holder publishes about itself, in
/// `holders/<window_id>.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HolderRecord {
    /// The window.
    pub window_id: String,
    /// The endpoint the holder listens on.
    pub endpoint: String,
    /// The holder's process id.
    pub holder_pid: u32,
    /// The child's process id, once the child has started.
    pub child_pid: Option<u32>,
    /// The holder wire version the holder speaks.
    pub protocol: u32,
}

/// A checkpoint as the holder kept it.
#[derive(Clone, PartialEq, Eq)]
pub struct HolderCheckpoint {
    /// The position it describes.
    pub seq: u64,
    /// The checkpoint, opaque to the holder.
    pub bytes: Vec<u8>,
}

impl std::fmt::Debug for HolderCheckpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HolderCheckpoint")
            .field("seq", &self.seq)
            .field("len", &self.bytes.len())
            .finish()
    }
}

/// What a holder leaves when its child has exited, in
/// `holders/<window_id>.exit`.
#[derive(Clone, PartialEq, Eq)]
pub struct HolderExit {
    /// The child's exit code, when known.
    pub code: Option<i32>,
    /// The checkpoint the holder kept, if any.
    pub checkpoint: Option<HolderCheckpoint>,
    /// The output after the checkpoint (all of it when there is none).
    pub output: Vec<u8>,
    /// The position just past `output`.
    pub end_seq: u64,
}

impl std::fmt::Debug for HolderExit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HolderExit")
            .field("code", &self.code)
            .field("checkpoint", &self.checkpoint)
            .field("output_len", &self.output.len())
            .field("end_seq", &self.end_seq)
            .finish()
    }
}

/// Whether `window_id` may name a holder's files and endpoint: ASCII letters,
/// digits, `-` and `_`, at most 64 bytes.
#[must_use]
pub fn is_plain_window_id(window_id: &str) -> bool {
    !window_id.is_empty()
        && window_id.len() <= 64
        && window_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

fn holder_file(holders_dir: &Path, window_id: &str, extension: &str) -> io::Result<PathBuf> {
    if !is_plain_window_id(window_id) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a window id is not a plain identifier",
        ));
    }
    Ok(holders_dir.join(format!("{window_id}.{extension}")))
}

/// The path of the record of `window_id`'s holder.
///
/// # Errors
///
/// [`io::ErrorKind::InvalidInput`] when `window_id` is not
/// [plain](is_plain_window_id).
pub fn holder_record_path(holders_dir: &Path, window_id: &str) -> io::Result<PathBuf> {
    holder_file(holders_dir, window_id, "json")
}

/// The path of the exit file of `window_id`'s holder.
///
/// # Errors
///
/// As for [`holder_record_path`].
pub fn holder_exit_path(holders_dir: &Path, window_id: &str) -> io::Result<PathBuf> {
    holder_file(holders_dir, window_id, "exit")
}

/// Reads the record of `window_id`'s holder.
///
/// # Errors
///
/// The error of reading the file, or [`io::ErrorKind::InvalidData`] when it
/// is not a record.
pub fn read_holder_record(holders_dir: &Path, window_id: &str) -> io::Result<HolderRecord> {
    let text = std::fs::read(holder_record_path(holders_dir, window_id)?)?;
    serde_json::from_slice(&text).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

/// Encodes `record` as the content of its file.
#[must_use]
pub fn encode_holder_record(record: &HolderRecord) -> Vec<u8> {
    // Strings and numbers only, which serde_json always encodes.
    serde_json::to_vec(record).unwrap_or_default()
}

/// The most output one `Output` of an exit file carries, well under
/// [`MAX_HOLDER_FRAME_LEN`] so any amount of output stays readable.
const EXIT_OUTPUT_FRAME_BYTES: usize = 1024 * 1024;

/// Encodes an exit as the frames of an exit file: `Exited`, the checkpoint if
/// any, then the output after it as consecutive `Output` frames (one empty
/// `Output` when there is none, to carry `end_seq`).
#[must_use]
pub fn encode_holder_exit(exit: &HolderExit) -> Vec<u8> {
    let mut out = encode_holder_frame(&HolderFrame::Exited { code: exit.code });
    if let Some(checkpoint) = &exit.checkpoint {
        out.extend(bytes_frame(
            TYPE_CHECKPOINT,
            checkpoint.seq,
            &checkpoint.bytes,
        ));
    }
    if exit.output.is_empty() {
        out.extend(bytes_frame(TYPE_OUTPUT, exit.end_seq, &[]));
        return out;
    }
    let mut seq = exit.end_seq.saturating_sub(exit.output.len() as u64);
    for chunk in exit.output.chunks(EXIT_OUTPUT_FRAME_BYTES) {
        seq = seq.saturating_add(chunk.len() as u64);
        out.extend(bytes_frame(TYPE_OUTPUT, seq, chunk));
    }
    out
}

/// Decodes what [`encode_holder_exit`] wrote.
///
/// # Errors
///
/// The codec's error for a broken frame, and
/// [`HolderCodecError::MalformedExit`] when the frames are not an exit.
pub fn decode_holder_exit(input: &[u8]) -> Result<HolderExit, HolderCodecError> {
    let mut rest = input;
    let mut next = || -> Result<Option<HolderFrame>, HolderCodecError> {
        match decode_holder_frame(rest)? {
            Some((frame, used)) => {
                rest = &rest[used..];
                Ok(Some(frame))
            }
            None if rest.is_empty() => Ok(None),
            None => Err(HolderCodecError::MalformedExit),
        }
    };
    let Some(HolderFrame::Exited { code }) = next()? else {
        return Err(HolderCodecError::MalformedExit);
    };
    let (checkpoint, first) = match next()? {
        Some(HolderFrame::Checkpoint { seq, bytes }) => {
            (Some(HolderCheckpoint { seq, bytes }), next()?)
        }
        other => (None, other),
    };
    let Some(HolderFrame::Output {
        seq: mut end_seq,
        bytes: mut output,
    }) = first
    else {
        return Err(HolderCodecError::MalformedExit);
    };
    // Each further `Output` continues exactly where the previous one ended.
    while let Some(frame) = next()? {
        let HolderFrame::Output { seq, bytes } = frame else {
            return Err(HolderCodecError::MalformedExit);
        };
        if bytes.is_empty() || end_seq.checked_add(bytes.len() as u64) != Some(seq) {
            return Err(HolderCodecError::MalformedExit);
        }
        output.extend_from_slice(&bytes);
        end_seq = seq;
    }
    Ok(HolderExit {
        code,
        checkpoint,
        output,
        end_seq,
    })
}

/// Reads the exit file of `window_id`'s holder.
///
/// # Errors
///
/// The error of reading the file, or [`io::ErrorKind::InvalidData`] when it
/// is not an exit.
pub fn read_holder_exit(holders_dir: &Path, window_id: &str) -> io::Result<HolderExit> {
    let bytes = std::fs::read(holder_exit_path(holders_dir, window_id)?)?;
    decode_holder_exit(&bytes).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}
