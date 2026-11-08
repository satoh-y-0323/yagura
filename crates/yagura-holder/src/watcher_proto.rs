//! The frames between a window's holder and its watcher, the process the
//! holder starts to start the window's child and to end the child's tree once
//! the holder is gone.
//!
//! The watcher's binary reads this module as its only definition of the
//! wire, so it uses `std` alone: no serde, no JSON, nothing of the holder.
//!
//! A frame is a `u32` LE length of what follows, a `u16` LE type and a body.
//! In a body, a `u32` and an `i32` are 4 bytes LE, a string is a `u32` LE
//! byte length and its UTF-8 bytes, a list is a `u32` LE count and its items,
//! and an `Option` is one byte, 0 for `None` or 1 followed by the value.
//! Descriptors (the terminal's slave, the endpoint's lock, the scope's
//! members) travel beside a frame in `SCM_RIGHTS`, never in a body.
//!
//! [`WatcherFrame::Hello`] is the watcher's first frame and keeps its type and
//! body in every version, so a holder can always read the version of the
//! watcher it started; [`WATCHER_PROTOCOL`] goes up with any other change.

use std::fmt;

/// The holder-watcher wire version this build speaks. The holder and the
/// watcher are built from this one constant.
pub const WATCHER_PROTOCOL: u32 = 2;

/// The most processes one request to the user manager names: the session
/// bus's default `max_message_unix_fds` (dbus-daemon), past which the bus
/// drops the connection rather than answer. A tree's members past it are
/// left out of the child's scope.
pub const MOST_FDS_PER_SCOPE_REQUEST: usize = 16;

/// The largest `length` a frame may declare. A `Spawn` carries a part of the
/// holder's launch, which is at most 1 MiB; this leaves room for the paths
/// and the lengths the binary form adds.
const MAX_FRAME_LEN: u32 = 2 * 1024 * 1024;

const LENGTH_LEN: usize = 4;
const TYPE_LEN: usize = 2;

const HELLO: u16 = 1;
const SPAWN: u16 = 2;
const LEADER_STARTED: u16 = 3;
const SCOPE_MEMBERS: u16 = 4;
const SCOPE_READY: u16 = 5;
const LEADER_EXITED: u16 = 6;
const SWEEP_DONE: u16 = 7;
const TERMINATE: u16 = 8;
const SCOPE_ASK: u16 = 9;
const SCOPE_MADE: u16 = 10;

/// A frame between a holder and its watcher.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatcherFrame {
    /// Watcher to holder, first: the version the watcher speaks.
    Hello {
        /// The watcher's [`WATCHER_PROTOCOL`].
        protocol: u32,
    },
    /// Holder to watcher: start the child on the terminal's slave, sent with
    /// the slave and the endpoint's lock.
    Spawn {
        /// The child's command line.
        argv: Vec<String>,
        /// The child's working directory.
        cwd: String,
        /// The variables set in the child's environment over the watcher's.
        env: Vec<(String, String)>,
        /// The holder's record, removed by the watcher once the holder is gone.
        record_path: String,
        /// The holder's endpoint, removed as the record is.
        socket_path: String,
        /// The endpoint's lock file, removed last.
        lock_path: String,
    },
    /// Watcher to holder: the child (the tree's leader) started.
    LeaderStarted {
        /// The leader's process id.
        pid: u32,
        /// The leader's start (`yagura_os::ProcessStart` as a string), `None`
        /// when it could not be read.
        start: Option<String>,
    },
    /// Watcher to holder, answering [`WatcherFrame::ScopeAsk`]: the members
    /// of the tree to be moved into the child's scope, the leader first, as
    /// the descriptors sent beside this frame. None when they could not be
    /// named.
    ScopeMembers {
        /// How many descriptors come with the frame, at most
        /// [`MOST_FDS_PER_SCOPE_REQUEST`].
        count: u32,
    },
    /// Holder to watcher: open the tree's members for the child's scope.
    ScopeAsk,
    /// Holder to watcher: the child's scope is made.
    ScopeMade,
    /// Watcher to holder, answering [`WatcherFrame::ScopeMade`]: where the
    /// child's scope is, empty when it could not be read.
    ScopeReady {
        /// The scope's cgroup.
        cgroup_path: String,
        /// The tree's members outside it.
        outside_members: u32,
    },
    /// Watcher to holder: the leader exited.
    LeaderExited {
        /// Its exit code, `None` when it ended by a signal.
        code: Option<i32>,
    },
    /// Watcher to holder: the tree's sweep is over.
    SweepDone {
        /// The members it could not end or could not wait for.
        remaining: u32,
    },
    /// Holder to watcher: end the tree.
    Terminate,
}

/// Why bytes are not a frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatcherCodecError {
    /// The frame declares a length over the limit.
    TooLong(u32),
    /// The frame's type is none this version knows.
    UnknownType(u16),
    /// The body does not match its type.
    Malformed(u16),
    /// The watcher speaks another version.
    OtherVersion(u32),
}

impl fmt::Display for WatcherCodecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLong(length) => write!(f, "a watcher frame declares {length} bytes"),
            Self::UnknownType(kind) => write!(f, "a watcher frame of unknown type {kind}"),
            Self::Malformed(kind) => write!(f, "a malformed watcher frame of type {kind}"),
            Self::OtherVersion(protocol) => write!(
                f,
                "the watcher speaks version {protocol}, not {WATCHER_PROTOCOL}"
            ),
        }
    }
}

impl std::error::Error for WatcherCodecError {}

/// Encodes `frame`.
#[must_use]
pub fn encode_watcher_frame(frame: &WatcherFrame) -> Vec<u8> {
    let mut body = Body::default();
    let kind = match frame {
        WatcherFrame::Hello { protocol } => {
            body.u32(*protocol);
            HELLO
        }
        WatcherFrame::Spawn {
            argv,
            cwd,
            env,
            record_path,
            socket_path,
            lock_path,
        } => {
            body.len(argv.len());
            for arg in argv {
                body.str(arg);
            }
            body.str(cwd);
            body.len(env.len());
            for (name, value) in env {
                body.str(name);
                body.str(value);
            }
            body.str(record_path);
            body.str(socket_path);
            body.str(lock_path);
            SPAWN
        }
        WatcherFrame::LeaderStarted { pid, start } => {
            body.u32(*pid);
            match start {
                Some(start) => {
                    body.0.push(1);
                    body.str(start);
                }
                None => body.0.push(0),
            }
            LEADER_STARTED
        }
        WatcherFrame::ScopeMembers { count } => {
            body.u32(*count);
            SCOPE_MEMBERS
        }
        WatcherFrame::ScopeReady {
            cgroup_path,
            outside_members,
        } => {
            body.str(cgroup_path);
            body.u32(*outside_members);
            SCOPE_READY
        }
        WatcherFrame::LeaderExited { code } => {
            match code {
                Some(code) => {
                    body.0.push(1);
                    body.0.extend_from_slice(&code.to_le_bytes());
                }
                None => body.0.push(0),
            }
            LEADER_EXITED
        }
        WatcherFrame::SweepDone { remaining } => {
            body.u32(*remaining);
            SWEEP_DONE
        }
        WatcherFrame::Terminate => TERMINATE,
        WatcherFrame::ScopeAsk => SCOPE_ASK,
        WatcherFrame::ScopeMade => SCOPE_MADE,
    };
    let length = u32::try_from(TYPE_LEN + body.0.len()).unwrap_or(u32::MAX);
    let mut out = Vec::with_capacity(LENGTH_LEN + TYPE_LEN + body.0.len());
    out.extend_from_slice(&length.to_le_bytes());
    out.extend_from_slice(&kind.to_le_bytes());
    out.extend_from_slice(&body.0);
    out
}

/// Decodes the first frame of `input`: `Ok(None)` while `input` holds only
/// part of it, else the frame and how many bytes it took. Bytes past the
/// frame are left for the next call.
///
/// # Errors
///
/// [`WatcherCodecError::OtherVersion`] for a [`WatcherFrame::Hello`] of
/// another version, and the other variants for bytes that are not a frame;
/// the stream cannot be read past either.
pub fn decode_watcher_frame(
    input: &[u8],
) -> Result<Option<(WatcherFrame, usize)>, WatcherCodecError> {
    let Some(length) = input.first_chunk::<LENGTH_LEN>() else {
        return Ok(None);
    };
    let length = u32::from_le_bytes(*length);
    if length > MAX_FRAME_LEN {
        return Err(WatcherCodecError::TooLong(length));
    }
    let end = LENGTH_LEN + length as usize;
    let Some(frame) = input.get(LENGTH_LEN..end) else {
        return Ok(None);
    };
    let Some((kind, body)) = frame.split_first_chunk::<TYPE_LEN>() else {
        return Err(WatcherCodecError::Malformed(0));
    };
    let kind = u16::from_le_bytes(*kind);
    let frame = decode_body(kind, body)?;
    Ok(Some((frame, end)))
}

fn decode_body(kind: u16, body: &[u8]) -> Result<WatcherFrame, WatcherCodecError> {
    let mut reader = Reader(body);
    let malformed = || WatcherCodecError::Malformed(kind);
    let frame = match kind {
        HELLO => {
            let protocol = reader.u32().ok_or_else(malformed)?;
            if protocol != WATCHER_PROTOCOL {
                return Err(WatcherCodecError::OtherVersion(protocol));
            }
            WatcherFrame::Hello { protocol }
        }
        SPAWN => {
            let argv = reader.list(Reader::string).ok_or_else(malformed)?;
            let cwd = reader.string().ok_or_else(malformed)?;
            let env = reader
                .list(|reader| Some((reader.string()?, reader.string()?)))
                .ok_or_else(malformed)?;
            WatcherFrame::Spawn {
                argv,
                cwd,
                env,
                record_path: reader.string().ok_or_else(malformed)?,
                socket_path: reader.string().ok_or_else(malformed)?,
                lock_path: reader.string().ok_or_else(malformed)?,
            }
        }
        LEADER_STARTED => WatcherFrame::LeaderStarted {
            pid: reader.u32().ok_or_else(malformed)?,
            start: reader.option(Reader::string).ok_or_else(malformed)?,
        },
        SCOPE_MEMBERS => WatcherFrame::ScopeMembers {
            count: reader.u32().ok_or_else(malformed)?,
        },
        SCOPE_READY => WatcherFrame::ScopeReady {
            cgroup_path: reader.string().ok_or_else(malformed)?,
            outside_members: reader.u32().ok_or_else(malformed)?,
        },
        LEADER_EXITED => WatcherFrame::LeaderExited {
            code: reader.option(Reader::i32).ok_or_else(malformed)?,
        },
        SWEEP_DONE => WatcherFrame::SweepDone {
            remaining: reader.u32().ok_or_else(malformed)?,
        },
        TERMINATE => WatcherFrame::Terminate,
        SCOPE_ASK => WatcherFrame::ScopeAsk,
        SCOPE_MADE => WatcherFrame::ScopeMade,
        other => return Err(WatcherCodecError::UnknownType(other)),
    };
    if reader.0.is_empty() {
        Ok(frame)
    } else {
        Err(malformed())
    }
}

#[derive(Default)]
struct Body(Vec<u8>);

impl Body {
    fn u32(&mut self, value: u32) {
        self.0.extend_from_slice(&value.to_le_bytes());
    }

    // A count or a string no frame under `MAX_FRAME_LEN` could hold makes a
    // frame the reader refuses as too long, so saturating loses nothing.
    fn len(&mut self, len: usize) {
        self.u32(u32::try_from(len).unwrap_or(u32::MAX));
    }

    fn str(&mut self, value: &str) {
        self.len(value.len());
        self.0.extend_from_slice(value.as_bytes());
    }
}

struct Reader<'a>(&'a [u8]);

impl Reader<'_> {
    fn take(&mut self, len: usize) -> Option<&[u8]> {
        let (head, rest) = self.0.split_at_checked(len)?;
        self.0 = rest;
        Some(head)
    }

    fn word(&mut self) -> Option<[u8; 4]> {
        let (head, rest) = self.0.split_first_chunk::<4>()?;
        self.0 = rest;
        Some(*head)
    }

    fn u32(&mut self) -> Option<u32> {
        self.word().map(u32::from_le_bytes)
    }

    fn i32(&mut self) -> Option<i32> {
        self.word().map(i32::from_le_bytes)
    }

    fn string(&mut self) -> Option<String> {
        let len = self.u32()? as usize;
        String::from_utf8(self.take(len)?.to_vec()).ok()
    }

    fn option<T>(&mut self, read: impl FnOnce(&mut Self) -> Option<T>) -> Option<Option<T>> {
        match self.take(1)? {
            [0] => Some(None),
            [1] => read(self).map(Some),
            _ => None,
        }
    }

    // Each item takes at least one byte, so a count past the bytes left is
    // refused before anything is reserved for it.
    fn list<T>(&mut self, mut read: impl FnMut(&mut Self) -> Option<T>) -> Option<Vec<T>> {
        let count = self.u32()? as usize;
        if count > self.0.len() {
            return None;
        }
        let mut items = Vec::with_capacity(count);
        for _ in 0..count {
            items.push(read(self)?);
        }
        Some(items)
    }
}
