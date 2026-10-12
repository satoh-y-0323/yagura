//! Client-side fail-safe constants.
//!
//! Every GUI-side timeout / retry value lives in exactly one
//! place (they are deliberately *not* part of the daemon's `DaemonConfig`,
//! because the GUI does not depend on the daemon crate).

use std::time::Duration;

/// Interval between connection attempts after the daemon has been spawned
/// (200 ms x 25 ~ 5 s).
pub const CONNECT_RETRY_INTERVAL: Duration = Duration::from_millis(200);

/// Maximum number of connection attempts after the daemon has been spawned.
pub const CONNECT_RETRY_LIMIT: u32 = 25;

/// How long to wait for the reply to a request -- `attach` as much as any
/// window-scoped one -- before giving up.
pub const ATTACH_TIMEOUT: Duration = Duration::from_secs(5);

/// How many times an unexpected disconnect is automatically recovered from
/// (exactly one attempt, and never after `error(superseded)`).
pub const RECONNECT_ATTEMPT_LIMIT: u32 = 1;

/// Lower bounds accepted for a PTY's columns and rows (`resize` schema): the
/// daemon's own limits, taken from the wire contract.
pub use yagura_protocol::{MIN_WINDOW_COLS as MIN_PTY_COLS, MIN_WINDOW_ROWS as MIN_PTY_ROWS};

/// Upper bound accepted for a PTY dimension (`resize` schema): the daemon's
/// own limit, taken from the wire contract.
pub use yagura_protocol::MAX_WINDOW_DIMENSION as MAX_PTY_DIMENSION;

/// Size of the chunk used when reading from the daemon endpoint.
pub const READ_CHUNK_LEN: usize = 8192;

/// Interval at which the GUI polls the daemon for the current window list
/// (multi-window v2).
pub const WINDOW_LIST_POLL_INTERVAL: Duration = Duration::from_secs(2);

/// Number of connection attempts spent right after spawning the daemon
/// (replaces [`CONNECT_RETRY_LIMIT`] for that one wait): on Unix a freshly
/// spawned daemon reads the login shell's environment before it binds its
/// endpoint, so this budget has to outlast that read on top of the ordinary
/// connection budget. Derived (ceiling division on nanoseconds -- `Duration`
/// has no const `Div`/`Mul` operator) rather than a literal, so it tracks
/// either budget if it changes.
pub const POST_SPAWN_CONNECT_RETRY_LIMIT: u32 = {
    let required_nanos = yagura_os::LOGIN_ENV_READ_TIMEOUT.as_nanos()
        + CONNECT_RETRY_INTERVAL.as_nanos() * CONNECT_RETRY_LIMIT as u128;
    let interval_nanos = CONNECT_RETRY_INTERVAL.as_nanos();
    required_nanos.div_ceil(interval_nanos) as u32
};

/// Interval between two observations of the daemon's liveness by the
/// recovery supervisor, and the wait it uses on a tick that made no attempt.
pub const TICK_INTERVAL: Duration = Duration::from_secs(1);

/// Ceiling on the backoff wait between two recovery attempts: the wait
/// saturates here instead of growing without bound.
pub const MAX_RECONNECT_DELAY: Duration = Duration::from_secs(60);
