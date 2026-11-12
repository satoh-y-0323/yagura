//! The wire between the daemon and a window's holder, and the record a holder
//! keeps in the `holders` directory its launch names.
//!
//! A frame is a `u32` LE length of what follows, a `u16` LE type and a body.
//! The body of [`HolderFrame::Write`] is a `u64` LE id followed by the raw
//! bytes, and the body of [`HolderFrame::Report`] is the raw bytes alone;
//! every other body is a JSON object.
//!
//! The holder keeps the window's terminal: it feeds the child's output to its
//! model, answers the child's queries, and sends the daemon what the daemon
//! reads (screen copies, [`ScreenRows`] then [`ScreenPush`]) and what a client
//! is shown ([`HolderFrame::ViewFrame`]). Raw output never leaves the holder.
//!
//! # Changing the wire
//!
//! A JSON body, the copies' and the view frames' contents included, only ever
//! gains fields: a later version may add one but never removes one or
//! changes its meaning, and a reader keeps decoding the fields it knows. So a
//! new mark on a copy's cell or a new attribute in a view frame is added
//! without a new version. [`HOLDER_PROTOCOL`] goes up only for a change that
//! adding fields cannot express (a frame removed, a field's meaning changed).
//!
//! The fixed frames ([`HolderFrame::Hello`], [`HolderFrame::Terminate`],
//! [`HolderFrame::Exited`], [`HolderFrame::Displaced`],
//! [`HolderFrame::VersionRejected`]) keep their type numbers and bodies in
//! every version, so a daemon and a holder of any two versions can still
//! close a window and learn of its exit.

use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{Frame, Palette, PointerInput, ScreenPush, ScreenRows};

/// The holder wire version this build speaks.
pub const HOLDER_PROTOCOL: u32 = 2;

/// The largest `length` a frame may declare. A peer declaring more is broken
/// or hostile, and is refused before anything is buffered for it.
pub const MAX_HOLDER_FRAME_LEN: usize = 64 * 1024 * 1024;

/// The longest `hello_deadline_ms` a holder honours: long enough for any
/// daemon start, short enough that an unclaimed holder does not linger for
/// hours. A longer request is cut to this.
pub const MAX_HELLO_DEADLINE_MS: u64 = 10 * 60 * 1000;

/// The most text one [`HolderFrame::Tail`] carries; a larger `max_bytes` is
/// cut to this.
pub const MAX_TAIL_BYTES: usize = 8 * 1024 * 1024;

// JSON escapes a control character as six bytes, so even a tail of nothing
// else fits in one frame.
const _: () = assert!(6 * MAX_TAIL_BYTES + 1024 <= MAX_HOLDER_FRAME_LEN);

const TYPE_HELLO: u16 = 1;
const TYPE_TERMINATE: u16 = 2;
const TYPE_EXITED: u16 = 3;
const TYPE_DISPLACED: u16 = 4;
const TYPE_VERSION_REJECTED: u16 = 5;
const TYPE_WELCOME: u16 = 6;
// 7 to 11 carried the raw output and its checkpoints in version 1.
const TYPE_WRITE: u16 = 12;
const TYPE_WRITTEN: u16 = 13;
const TYPE_WRITE_FAILED: u16 = 14;
const TYPE_RESIZE: u16 = 15;
const TYPE_RESIZED: u16 = 16;
const TYPE_RESIZE_FAILED: u16 = 17;
const TYPE_SPAWN_FAILED: u16 = 18;
const TYPE_SCREEN_ROWS: u16 = 19;
const TYPE_SCREEN_PUSH: u16 = 20;
const TYPE_PUSH_ACK: u16 = 21;
const TYPE_SCREEN_RESYNC: u16 = 22;
const TYPE_REPORT: u16 = 23;
const TYPE_SET_PALETTE: u16 = 24;
const TYPE_SET_VIEW_FOCUS: u16 = 25;
// 26 was a focus interval for a daemon that typed into windows; it is not reused.
const TYPE_ATTACH: u16 = 27;
const TYPE_DETACH: u16 = 28;
const TYPE_VIEW_NOTE: u16 = 29;
const TYPE_VIEW_OP: u16 = 30;
const TYPE_VIEW_OP_DONE: u16 = 31;
const TYPE_VIEW_FRAME: u16 = 32;
const TYPE_READ_TAIL: u16 = 33;
const TYPE_TAIL: u16 = 34;
const TYPE_SETTLE: u16 = 35;
const TYPE_SETTLED: u16 = 36;

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
    /// Every copy of the screen holding the child's output comes before it.
    Exited {
        /// The child's exit code.
        code: Option<i32>,
    },
    /// h -> d, fixed: another daemon has taken the holder over; it is the
    /// last frame on this connection, which is closed next and is not to be
    /// reopened.
    Displaced,
    /// h -> d, fixed: the holder does not speak the `Hello`'s version.
    VersionRejected {
        /// The version the holder speaks.
        protocol: u32,
    },
    /// h -> d: the `Hello` is accepted. A whole copy of the screen follows
    /// before any other frame.
    Welcome {
        /// The holder wire version.
        protocol: u32,
        /// The window's record the daemon handed over at launch, unchanged.
        record: String,
        /// The terminal's columns.
        cols: u16,
        /// The terminal's rows.
        rows: u16,
        /// Whether the child has already exited: [`HolderFrame::Exited`]
        /// follows the whole copy of its last screen.
        exited: bool,
        /// The child's process id. `None` from a holder that does not send
        /// it; the daemon learns the pid here, not from the record, which
        /// the holder may already have removed.
        child_pid: Option<u32>,
    },
    /// d -> h: write `bytes` to the child, after every earlier write. Counts
    /// as the user's input: it ends a synchronized update that holds a view.
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
    /// h -> d: the terminal now has the size a `Resize` asked for. A copy of
    /// the screen at that size comes before it.
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
    /// h -> d: rows of a copy of the screen; see [`ScreenRows`].
    ScreenRows(ScreenRows),
    /// h -> d: the close of a copy of the screen; see [`ScreenPush`]. The
    /// next ordinary copy waits for its [`HolderFrame::PushAck`].
    ScreenPush(ScreenPush),
    /// d -> h: the copy `number` was applied, so the next may come.
    PushAck {
        /// The copy's number.
        number: u64,
    },
    /// d -> h: the last copy was refused; the next goes whole. Stands in for
    /// that copy's [`HolderFrame::PushAck`].
    ScreenResync,
    /// d -> h: write `bytes` to the child, after every earlier write, as a
    /// report the child turned on (not the user's input): it is not counted
    /// as a write and is not answered.
    Report {
        /// The bytes to write.
        bytes: Vec<u8>,
    },
    /// d -> h: the colors the child's color queries are answered from.
    SetPalette {
        /// The colors.
        palette: Palette,
    },
    /// d -> h: whether the window's view has the focus.
    SetViewFocus {
        /// The view's focus.
        focused: bool,
    },
    /// d -> h: a client is shown the window as attach `generation`, which
    /// supersedes the one before; every row goes in its first frame.
    Attach {
        /// The attach.
        generation: u64,
    },
    /// d -> h: attach `generation` is let go of.
    Detach {
        /// The attach.
        generation: u64,
    },
    /// d -> h: what the client of attach `generation` told its view.
    ViewNote {
        /// The attach.
        generation: u64,
        /// What it told.
        note: HolderViewNote,
    },
    /// d -> h: an operation on the view, answered by
    /// [`HolderFrame::ViewOpDone`].
    ViewOp {
        /// Identifies the operation in its answer.
        id: u64,
        /// The operation.
        op: HolderViewOp,
    },
    /// h -> d: the view operation `id` is done.
    ViewOpDone {
        /// The operation.
        id: u64,
        /// The selected text, for [`HolderViewOp::SelectedText`].
        text: Option<String>,
        /// Whether the view moved, for [`HolderViewOp::ToBottom`].
        moved: bool,
    },
    /// h -> d: one part of a frame for the client of attach `generation`.
    ViewFrame {
        /// The attach.
        generation: u64,
        /// The part.
        frame: Frame,
    },
    /// d -> h: the window's text as a terminal shows it, answered by
    /// [`HolderFrame::Tail`].
    ReadTail {
        /// Identifies the read in its answer.
        id: u64,
        /// The most logical lines, counted from the end.
        max_lines: usize,
        /// The most bytes, cut to [`MAX_TAIL_BYTES`].
        max_bytes: usize,
    },
    /// h -> d: the text a `ReadTail` asked for.
    Tail {
        /// The read.
        id: u64,
        /// The text.
        text: String,
        /// Text was left out before it.
        truncated: bool,
    },
    /// d -> h: answered by [`HolderFrame::Settled`] once the output read
    /// before it is taken in and the frames for it are sent.
    Settle {
        /// Identifies the settle in its answer.
        id: u64,
    },
    /// h -> d: the settle `id` is done.
    Settled {
        /// The settle.
        id: u64,
    },
}

/// What a client told its view; see [`HolderFrame::ViewNote`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HolderViewNote {
    /// The client applied frame `seq`.
    Ack {
        /// The frame.
        seq: u64,
    },
    /// The client shows the view or no longer does.
    Visible {
        /// Whether it is shown.
        visible: bool,
    },
}

/// An operation on a view; see [`HolderFrame::ViewOp`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HolderViewOp {
    /// Moves the view `lines` rows into the history (negative: back).
    Scroll {
        /// Rows.
        lines: i32,
    },
    /// Moves the view one screen up into the history or back down.
    Page {
        /// Up (`true`) or down.
        up: bool,
    },
    /// A pointer event that selects.
    Select {
        /// The event.
        pointer: PointerInput,
    },
    /// Drops the selection.
    ClearSelection,
    /// Answers the selected text.
    SelectedText,
    /// Brings the view back to the live screen; answers whether it moved.
    ToBottom,
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

/// A fault a test asks the fault-injecting holder (`holder_fault`) to act
/// out. The shipped holder refuses a launch that names one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HolderFault {
    /// Once the first whole copy after the `Welcome` is sent, the model's
    /// work stops: no copy, frame, tail or settle comes after it until the
    /// child has exited.
    StallModel,
    /// Once the first whole copy after the `Welcome` is sent, the model's
    /// next call panics, so the model is rebuilt blank.
    PoisonModel,
    /// Each write's answer is held back, after the write reached the child,
    /// until the holder lets go of the child's input.
    HoldWriteAnswer,
}

// The window's record carries its token, a write and a report carry what the
// user typed, and the screen, the text and the selection carry what the child
// wrote, so none of them is shown; byte frames show their length only.
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
                child_pid,
            } => f
                .debug_struct("Welcome")
                .field("protocol", protocol)
                .field("record_len", &record.len())
                .field("cols", cols)
                .field("rows", rows)
                .field("exited", exited)
                .field("child_pid", child_pid)
                .finish(),
            Self::Write { id, bytes } => f
                .debug_struct("Write")
                .field("id", id)
                .field("len", &bytes.len())
                .finish(),
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
            Self::ScreenRows(rows) => f
                .debug_struct("ScreenRows")
                .field("number", &rows.number)
                .field("lines", &rows.lines.len())
                .finish(),
            Self::ScreenPush(push) => f
                .debug_struct("ScreenPush")
                .field("number", &push.number)
                .field("full", &push.full)
                .field("cols", &push.cols)
                .field("rows", &push.rows)
                .field("sync_open", &push.sync_open)
                .field("generation", &push.generation)
                .field("outputs_fed", &push.outputs_fed)
                .finish(),
            Self::PushAck { number } => f.debug_struct("PushAck").field("number", number).finish(),
            Self::ScreenResync => f.write_str("ScreenResync"),
            Self::Report { bytes } => f.debug_struct("Report").field("len", &bytes.len()).finish(),
            Self::SetPalette { .. } => f.write_str("SetPalette"),
            Self::SetViewFocus { focused } => f
                .debug_struct("SetViewFocus")
                .field("focused", focused)
                .finish(),
            Self::Attach { generation } => f
                .debug_struct("Attach")
                .field("generation", generation)
                .finish(),
            Self::Detach { generation } => f
                .debug_struct("Detach")
                .field("generation", generation)
                .finish(),
            Self::ViewNote { generation, note } => f
                .debug_struct("ViewNote")
                .field("generation", generation)
                .field("note", note)
                .finish(),
            Self::ViewOp { id, op } => {
                // A selection's pointer carries only a cell, no text.
                f.debug_struct("ViewOp")
                    .field("id", id)
                    .field("op", op)
                    .finish()
            }
            Self::ViewOpDone { id, text, moved } => f
                .debug_struct("ViewOpDone")
                .field("id", id)
                .field("text_len", &text.as_ref().map(String::len))
                .field("moved", moved)
                .finish(),
            Self::ViewFrame { generation, frame } => f
                .debug_struct("ViewFrame")
                .field("generation", generation)
                .field("seq", &frame.seq)
                .field("last", &frame.last)
                .finish(),
            Self::ReadTail {
                id,
                max_lines,
                max_bytes,
            } => f
                .debug_struct("ReadTail")
                .field("id", id)
                .field("max_lines", max_lines)
                .field("max_bytes", max_bytes)
                .finish(),
            Self::Tail {
                id,
                text,
                truncated,
            } => f
                .debug_struct("Tail")
                .field("id", id)
                .field("text_len", &text.len())
                .field("truncated", truncated)
                .finish(),
            Self::Settle { id } => f.debug_struct("Settle").field("id", id).finish(),
            Self::Settled { id } => f.debug_struct("Settled").field("id", id).finish(),
        }
    }
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
    /// A byte-carrying body is too short to hold its id.
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
    #[serde(default)]
    child_pid: Option<u32>,
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

#[derive(Serialize, Deserialize)]
struct NumberBody {
    number: u64,
}

#[derive(Serialize, Deserialize)]
struct PaletteBody {
    palette: Palette,
}

#[derive(Serialize, Deserialize)]
struct FocusedBody {
    focused: bool,
}

#[derive(Serialize, Deserialize)]
struct GenerationBody {
    generation: u64,
}

#[derive(Serialize, Deserialize)]
struct ViewNoteBody {
    generation: u64,
    note: HolderViewNote,
}

#[derive(Serialize, Deserialize)]
struct ViewOpBody {
    id: u64,
    op: HolderViewOp,
}

#[derive(Serialize, Deserialize)]
struct ViewOpDoneBody {
    id: u64,
    text: Option<String>,
    moved: bool,
}

#[derive(Serialize, Deserialize)]
struct ViewFrameBody {
    generation: u64,
    frame: Frame,
}

#[derive(Serialize, Deserialize)]
struct ReadTailBody {
    id: u64,
    max_lines: usize,
    max_bytes: usize,
}

#[derive(Serialize, Deserialize)]
struct TailBody {
    id: u64,
    text: String,
    truncated: bool,
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
    // These bodies are plain structs of numbers, strings, chars and options,
    // which serde_json always serializes; an empty body would be refused by
    // the reader.
    let json = serde_json::to_vec(body).unwrap_or_default();
    frame(frame_type, &[&json])
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
            child_pid,
        } => json_frame(
            TYPE_WELCOME,
            &WelcomeBody {
                protocol: *protocol,
                record: record.clone(),
                cols: *cols,
                rows: *rows,
                exited: *exited,
                child_pid: *child_pid,
            },
        ),
        HolderFrame::Write { id, bytes } => self::frame(TYPE_WRITE, &[&id.to_le_bytes(), bytes]),
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
        HolderFrame::ScreenRows(rows) => json_frame(TYPE_SCREEN_ROWS, rows),
        HolderFrame::ScreenPush(push) => json_frame(TYPE_SCREEN_PUSH, push),
        HolderFrame::PushAck { number } => {
            json_frame(TYPE_PUSH_ACK, &NumberBody { number: *number })
        }
        HolderFrame::ScreenResync => json_frame(TYPE_SCREEN_RESYNC, &EmptyBody {}),
        HolderFrame::Report { bytes } => self::frame(TYPE_REPORT, &[bytes]),
        HolderFrame::SetPalette { palette } => json_frame(
            TYPE_SET_PALETTE,
            &PaletteBody {
                palette: palette.clone(),
            },
        ),
        HolderFrame::SetViewFocus { focused } => {
            json_frame(TYPE_SET_VIEW_FOCUS, &FocusedBody { focused: *focused })
        }
        HolderFrame::Attach { generation } => json_frame(
            TYPE_ATTACH,
            &GenerationBody {
                generation: *generation,
            },
        ),
        HolderFrame::Detach { generation } => json_frame(
            TYPE_DETACH,
            &GenerationBody {
                generation: *generation,
            },
        ),
        HolderFrame::ViewNote { generation, note } => json_frame(
            TYPE_VIEW_NOTE,
            &ViewNoteBody {
                generation: *generation,
                note: *note,
            },
        ),
        HolderFrame::ViewOp { id, op } => {
            json_frame(TYPE_VIEW_OP, &ViewOpBody { id: *id, op: *op })
        }
        HolderFrame::ViewOpDone { id, text, moved } => json_frame(
            TYPE_VIEW_OP_DONE,
            &ViewOpDoneBody {
                id: *id,
                text: text.clone(),
                moved: *moved,
            },
        ),
        HolderFrame::ViewFrame { generation, frame } => json_frame(
            TYPE_VIEW_FRAME,
            &ViewFrameBody {
                generation: *generation,
                frame: frame.clone(),
            },
        ),
        HolderFrame::ReadTail {
            id,
            max_lines,
            max_bytes,
        } => json_frame(
            TYPE_READ_TAIL,
            &ReadTailBody {
                id: *id,
                max_lines: *max_lines,
                max_bytes: *max_bytes,
            },
        ),
        HolderFrame::Tail {
            id,
            text,
            truncated,
        } => json_frame(
            TYPE_TAIL,
            &TailBody {
                id: *id,
                text: text.clone(),
                truncated: *truncated,
            },
        ),
        HolderFrame::Settle { id } => json_frame(TYPE_SETTLE, &IdBody { id: *id }),
        HolderFrame::Settled { id } => json_frame(TYPE_SETTLED, &IdBody { id: *id }),
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
                child_pid,
            } = json(frame_type, body)?;
            HolderFrame::Welcome {
                protocol,
                record,
                cols,
                rows,
                exited,
                child_pid,
            }
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
        TYPE_SCREEN_ROWS => HolderFrame::ScreenRows(json(frame_type, body)?),
        TYPE_SCREEN_PUSH => HolderFrame::ScreenPush(json(frame_type, body)?),
        TYPE_PUSH_ACK => {
            let NumberBody { number } = json(frame_type, body)?;
            HolderFrame::PushAck { number }
        }
        TYPE_SCREEN_RESYNC => {
            let EmptyBody {} = json(frame_type, body)?;
            HolderFrame::ScreenResync
        }
        TYPE_REPORT => HolderFrame::Report {
            bytes: body.to_vec(),
        },
        TYPE_SET_PALETTE => {
            let PaletteBody { palette } = json(frame_type, body)?;
            HolderFrame::SetPalette { palette }
        }
        TYPE_SET_VIEW_FOCUS => {
            let FocusedBody { focused } = json(frame_type, body)?;
            HolderFrame::SetViewFocus { focused }
        }
        TYPE_ATTACH => {
            let GenerationBody { generation } = json(frame_type, body)?;
            HolderFrame::Attach { generation }
        }
        TYPE_DETACH => {
            let GenerationBody { generation } = json(frame_type, body)?;
            HolderFrame::Detach { generation }
        }
        TYPE_VIEW_NOTE => {
            let ViewNoteBody { generation, note } = json(frame_type, body)?;
            HolderFrame::ViewNote { generation, note }
        }
        TYPE_VIEW_OP => {
            let ViewOpBody { id, op } = json(frame_type, body)?;
            HolderFrame::ViewOp { id, op }
        }
        TYPE_VIEW_OP_DONE => {
            let ViewOpDoneBody { id, text, moved } = json(frame_type, body)?;
            HolderFrame::ViewOpDone { id, text, moved }
        }
        TYPE_VIEW_FRAME => {
            let ViewFrameBody { generation, frame } = json(frame_type, body)?;
            HolderFrame::ViewFrame { generation, frame }
        }
        TYPE_READ_TAIL => {
            let ReadTailBody {
                id,
                max_lines,
                max_bytes,
            } = json(frame_type, body)?;
            HolderFrame::ReadTail {
                id,
                max_lines,
                max_bytes,
            }
        }
        TYPE_TAIL => {
            let TailBody {
                id,
                text,
                truncated,
            } = json(frame_type, body)?;
            HolderFrame::Tail {
                id,
                text,
                truncated,
            }
        }
        TYPE_SETTLE => {
            let IdBody { id } = json(frame_type, body)?;
            HolderFrame::Settle { id }
        }
        TYPE_SETTLED => {
            let IdBody { id } = json(frame_type, body)?;
            HolderFrame::Settled { id }
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
    /// The directory the holder keeps its record and, on Unix, its socket in. It must be an absolute path to a directory private to
    /// the user; the holder creates it when only it is missing.
    pub holders_dir: PathBuf,
    /// The endpoint whose server is the daemon: only the process serving it
    /// when a `Hello` arrives is let in.
    pub daemon_endpoint: String,
    /// Whether a next daemon may come for the window. When it may, a lost
    /// connection other than [`HolderFrame::Displaced`] leaves the child
    /// running and the holder serving the next `Hello`; when it may not, it
    /// ends the child's tree and the holder.
    pub awaits_successor: bool,
    /// The log the holder appends its messages to, one line per open; `None`
    /// sends them to its standard error. A missing file is not created, and
    /// the line is lost.
    pub log_file: Option<PathBuf>,
    /// Rows the terminal keeps after they scroll off its top, read back by
    /// [`HolderFrame::ReadTail`] and scrolled to by a view.
    pub history_rows: usize,
    /// The colors the child's first color queries are answered from, until
    /// a [`HolderFrame::SetPalette`].
    pub palette: Option<Palette>,
    /// A fault for the fault-injecting holder to act out; the shipped holder
    /// refuses a launch that names one.
    pub fault: Option<HolderFault>,
    /// The systemd user scopes the holder asks to be moved into once the
    /// child has started (Linux); `None` asks for none.
    pub scopes: Option<HolderScopes>,
}

/// The names of the systemd user scopes of one start of a window's holder:
/// unit names of ASCII letters, digits and `_` followed by `.scope`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HolderScopes {
    /// The scope holding the holder alone.
    pub holder: String,
    /// The scope holding the window's child and its tree, bound to the
    /// holder's.
    pub child: String,
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
            .field("awaits_successor", &self.awaits_successor)
            .field("log_file", &self.log_file)
            .field("history_rows", &self.history_rows)
            .field("fault", &self.fault)
            .field("scopes", &self.scopes)
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
    awaits_successor: bool,
    log_file: Option<&'a OsStr>,
    history_rows: usize,
    palette: Option<&'a Palette>,
    fault: Option<HolderFault>,
    scopes: Option<&'a HolderScopes>,
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
    awaits_successor: bool,
    #[serde(default)]
    log_file: Option<OsString>,
    history_rows: usize,
    #[serde(default)]
    palette: Option<Palette>,
    #[serde(default)]
    fault: Option<HolderFault>,
    #[serde(default)]
    scopes: Option<HolderScopes>,
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
        awaits_successor: launch.awaits_successor,
        log_file: launch.log_file.as_deref().map(Path::as_os_str),
        history_rows: launch.history_rows,
        palette: launch.palette.as_ref(),
        fault: launch.fault,
        scopes: launch.scopes.as_ref(),
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
        awaits_successor,
        log_file,
        history_rows,
        palette,
        fault,
        scopes,
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
        awaits_successor,
        log_file: log_file.map(PathBuf::from),
        history_rows,
        palette,
        fault,
        scopes,
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
    /// The holder's start (`yagura_os::ProcessStart` as a string), so a
    /// holder pid taken over by another process is not taken for it. `None`
    /// in a record written before it was carried, or when it could not be
    /// read.
    #[serde(default)]
    pub holder_start: Option<String>,
    /// The child's start, as for `holder_start`; `None` until the child has
    /// started, and when its start could not be read.
    #[serde(default)]
    pub child_start: Option<String>,
    /// The systemd user scope the child is asked to be moved into, written
    /// before it is asked for, so a holder that died is ended by that name.
    /// `None` when none is asked for, and in a record written before it was
    /// carried.
    #[serde(default)]
    pub child_scope: Option<String>,
    /// The process id of the holder's watcher, the process that starts the
    /// child and ends its tree once the holder is gone. `None` for a holder
    /// that has no watcher, and in a record written before it was carried.
    #[serde(default)]
    pub watcher_pid: Option<u32>,
    /// The watcher's start, as for `holder_start`, so a daemon can tell
    /// whether the watcher still lives. `None` with `watcher_pid`, and when
    /// its start could not be read.
    #[serde(default)]
    pub watcher_start: Option<String>,
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
