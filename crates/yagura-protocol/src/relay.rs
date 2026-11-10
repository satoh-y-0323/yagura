//! The relay contract: the frames between a window's relay (the `yagura-mcp`
//! process a CLI starts as its MCP server) and the daemon, which serves MCP.
//!
//! A frame is the IPC framing ([`crate::frame_encode`]: a `u32` LE length and
//! a body) whose body is a JSON object carrying a `type`. A connection whose
//! first frame is [`RelayFrame::Hello`] carries relay frames only from then on.
//! The relay carries the CLI's JSON-RPC lines as strings and does not read
//! MCP results.
//!
//! # Changing the contract
//!
//! The contract is versioned by [`RelayFrame::Hello::contract`], not by
//! [`crate::PROTOCOL_REVISION`]: changing the IPC does not change this
//! contract. Frames only ever gain fields: a later version may add one but
//! never removes one or changes its meaning, and a reader ignores fields it
//! does not know. A relay ignores a frame whose `type` it does not know (the
//! IPC framing still delimits it, so it is skipped by its length). A daemon
//! speaks the current contract and the one before it ([`SUPPORTED_CONTRACTS`]).

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// The contract versions this build's daemon speaks, newest last: the current
/// one and the one before it. Contract 1 is the first, so it is the only one.
pub const SUPPORTED_CONTRACTS: [u32; 1] = [1];

/// The JSON-RPC `error.code` of an error the relay writes itself to a request
/// other than `tools/call` (see [`rpc_error_line`]).
///
/// It lies in the server-error range the MCP SDKs leave to implementations,
/// away from the codes they reserve or treat specially (-32000, -32001,
/// -32002, -32042) and from the range the MCP specification assigns.
pub const RELAY_ERROR_CODE: i64 = -32010;

/// The call may or may not have taken effect: the daemon's connection ended
/// while it was in flight.
pub const DAEMON_RESTARTED: &str = "daemon_restarted";
/// No daemon could be reached for the request.
pub const DAEMON_UNAVAILABLE: &str = "daemon_unavailable";
/// The relay already holds as many requests as it may.
pub const RESOURCE_EXHAUSTED: &str = "resource_exhausted";
/// The request cannot be carried (for example, a line too large for a frame).
pub const INVALID_REQUEST: &str = "invalid_request";
/// The daemon refused the relay's session ([`RelayFrame::Refused`]).
pub const RELAY_REFUSED: &str = "relay_refused";

/// The most lines of one kind (requests, or notifications) a side holds while
/// it cannot hand them on: a relay while no daemon carries them, and a daemon
/// while its MCP server has not yet chosen how the session runs.
pub const HELD_LINES_MAX: usize = 16;

/// Whether `line` is one line, as [`RelayFrame::In::line`] and the lines of
/// [`RelayResume::opening`] must be: it holds no `\n` and no `\r`.
///
/// `\r` is refused too so that where one line ends does not rest on how a
/// receiver, or a later version of one, splits lines.
#[must_use]
pub fn is_one_line(line: &str) -> bool {
    !line.contains(['\n', '\r'])
}

/// One frame of the relay contract.
///
/// Each variant's doc names its sender: "relay" is the window's relay, and
/// "daemon" the daemon serving MCP.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum RelayFrame {
    /// Sent by the relay, first on each connection, to open a session.
    #[serde(rename = "relay_hello")]
    Hello {
        /// The contract version the relay speaks.
        contract: u32,
        /// The window the relay runs in; absent for a relay outside a window.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        window_id: Option<String>,
        /// The window's credential; absent for a relay outside a window.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        token: Option<String>,
        /// Present on every connection after the process's first, with or
        /// without a window.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resume: Option<RelayResume>,
    },
    /// Sent by the daemon when it accepts the session.
    #[serde(rename = "relay_welcome")]
    Welcome {
        /// The contract version the session speaks.
        contract: u32,
        /// A summary of the daemon's current tool list; opaque to the relay,
        /// which sends it back in [`RelayResume::tools`].
        tools: String,
    },
    /// Sent by the daemon when the session does not start.
    #[serde(rename = "relay_refused")]
    Refused {
        /// Why the session does not start; the relay acts on this.
        kind: RelayRefusedKind,
        /// The contract versions the daemon speaks.
        contracts: Vec<u32>,
        /// An English sentence for a person.
        reason: String,
    },
    /// Sent by the relay: one JSON-RPC line from the CLI.
    #[serde(rename = "relay_in")]
    In {
        /// The line as the CLI wrote it, without its newline. It is one line
        /// ([`is_one_line`]): a daemon closes a session whose relay sends one
        /// that is not, and a relay does not send a CLI line that is not.
        line: String,
        /// The relay's sequence number, on a request only.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        seq: Option<u64>,
    },
    /// Sent by the daemon: one JSON-RPC line for the CLI.
    #[serde(rename = "relay_out")]
    Out {
        /// The line to write to the CLI, without its newline.
        line: String,
        /// The [`RelayFrame::In::seq`] of the request, on an answer only.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        seq: Option<u64>,
        /// When true, the relay answers with [`RelayFrame::Done`] or
        /// [`RelayFrame::Dropped`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        receipt: Option<bool>,
    },
    /// Sent by the relay: the answer of `seq` asking for a receipt was written
    /// to the CLI.
    #[serde(rename = "relay_done")]
    Done {
        /// The answered request's sequence number.
        seq: u64,
    },
    /// Sent by the relay: the answer of `seq` asking for a receipt was not
    /// written, because the CLI had cancelled the request.
    #[serde(rename = "relay_dropped")]
    Dropped {
        /// The answered request's sequence number.
        seq: u64,
    },
}

/// What a relay sends in [`RelayFrame::Hello::resume`] when it connects again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelayResume {
    /// The JSON-RPC lines the CLI opened with and their answers, in order,
    /// each one line ([`is_one_line`]); absent when the CLI opened without
    /// `initialize`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opening: Option<Vec<String>>,
    /// The previous [`RelayFrame::Welcome::tools`].
    pub tools: String,
}

/// Why a daemon refused a relay's session ([`RelayFrame::Refused::kind`]).
///
/// There is no catch-all value: a refusal of a kind the relay does not know is
/// not one it can act on, so the frame does not decode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelayRefusedKind {
    /// The daemon does not speak the relay's contract.
    Contract,
    /// The daemon already holds as many sessions as it may.
    Limit,
    /// The resumed opening does not match what the daemon answers now.
    SessionMismatch,
}

/// The line a relay writes to the CLI to answer the `tools/call` of `id` with
/// an error it writes itself: a result with `isError: true` whose one text
/// content leads with `word` (one of the word constants of this module),
/// followed by `sentence`.
///
/// The line is one line of compact JSON. This function does not fail.
pub fn tool_call_error_line(id: &Value, word: &str, sentence: &str) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "content": [{"type": "text", "text": error_text(word, sentence)}],
            "isError": true,
        },
    })
    .to_string()
}

/// The line a relay writes to the CLI to answer a request other than
/// `tools/call` of `id` with an error it writes itself: a JSON-RPC error of
/// [`RELAY_ERROR_CODE`] whose message leads with `word` (one of the word
/// constants of this module), followed by `sentence`, and whose `data.code`
/// is `word`.
///
/// The line is one line of compact JSON. This function does not fail.
pub fn rpc_error_line(id: &Value, word: &str, sentence: &str) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": RELAY_ERROR_CODE,
            "message": error_text(word, sentence),
            "data": {"code": word},
        },
    })
    .to_string()
}

fn error_text(word: &str, sentence: &str) -> String {
    format!("{word}: {sentence}")
}

/// What a relay or a daemon reads of one JSON-RPC line: only its `method`,
/// its `id` and, for a notification, `params.requestId` (the request a
/// `notifications/cancelled` cancels). See [`read_line`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Line {
    /// A request: a `method` and an `id` that is not null.
    Request {
        /// The request's id.
        id: Value,
        /// The request's method.
        method: String,
    },
    /// A notification: a `method` and no `id` (or a null one).
    Notification {
        /// The notification's method.
        method: String,
        /// Its `params.requestId`, when it has one.
        cancels: Option<Value>,
    },
    /// An answer: an `id` and no `method`.
    Answer {
        /// The id of the request it answers.
        id: Value,
    },
    /// Neither: not a JSON object, or an object with no `method` and no `id`.
    Other,
}

/// The UTF-8 byte order mark a line may lead with (RFC 8259 §8.1), which the
/// MCP SDKs ignore.
const BYTE_ORDER_MARK: char = '\u{FEFF}';

/// Reads what [`Line`] holds of the JSON-RPC line `line`.
///
/// This function does not fail: a line that is not a JSON object is
/// [`Line::Other`]. It is [`parse_line`] and then [`Line::of`].
#[must_use]
pub fn read_line(line: &str) -> Line {
    parse_line(line).map_or(Line::Other, |value| Line::of(&value))
}

/// The JSON value the line `line` holds, read the way an MCP SDK reads one:
/// one leading byte order mark is ignored. `None` when it holds none.
#[must_use]
pub fn parse_line(line: &str) -> Option<Value> {
    let line = line.strip_prefix(BYTE_ORDER_MARK).unwrap_or(line);
    serde_json::from_str(line).ok()
}

impl Line {
    /// What [`Line`] holds of the JSON-RPC message `value`; a value that is
    /// not a JSON object is [`Line::Other`].
    #[must_use]
    pub fn of(value: &Value) -> Self {
        let Value::Object(object) = value else {
            return Self::Other;
        };
        let method = object
            .get("method")
            .and_then(Value::as_str)
            .map(str::to_string);
        let id = object.get("id").filter(|id| !id.is_null()).cloned();
        match (method, id) {
            (Some(method), Some(id)) => Self::Request { id, method },
            (Some(method), None) => {
                let cancels = object
                    .get("params")
                    .and_then(|params| params.get("requestId"))
                    .cloned();
                Self::Notification { method, cancels }
            }
            (None, Some(id)) => Self::Answer { id },
            (None, None) => Self::Other,
        }
    }
}
