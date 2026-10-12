//! Per-window `StatusEvent` authentication token.
//!
//! [`StatusToken`] is the single source of truth for the value a child CLI's
//! hooks must present (via the `YAGURA_STATUS_TOKEN` environment variable,
//! injected at spawn time) to have a `ClientMessage::StatusEvent` about *their
//! own window* accepted. One is generated per opened window from the OS RNG
//! ([`yagura_os::fill_random`]) and never derived from, or persisted
//! alongside, anything else the daemon holds. The one exception is
//! [`crate::config::DaemonConfig::status_token_override`], a test / development
//! facility that makes every window adopt one fixed value.

/// Length, in hex characters, of a well-formed token (128 bits).
const HEX_LEN: usize = 32;

/// Number of raw random bytes [`generate`] draws (128 bits, `2 * HEX_LEN`
/// hex characters).
const RANDOM_BYTES: usize = HEX_LEN / 2;

/// A `StatusEvent` authentication token.
///
/// A value is either a well-formed 32-character lowercase-hex token, or the
/// *unverifiable* state (`generate` could not obtain randomness): every
/// [`StatusToken::verify`] call against an unverifiable token returns
/// `false`, and [`StatusToken::as_hex`] returns `None` so callers know not to
/// distribute it to a spawned child (fail-closed).
///
/// `Debug` is implemented by hand so that the hex value can never reach a log
/// line or an error message.
pub struct StatusToken {
    hex: Option<String>,
}

impl StatusToken {
    /// Generates a new token from the OS RNG.
    ///
    /// A failure to obtain randomness is not surfaced as an error: it
    /// produces a token in the unverifiable state instead, so the daemon
    /// keeps running and simply rejects every `StatusEvent` rather than
    /// aborting startup over it.
    #[must_use]
    pub fn generate() -> StatusToken {
        let mut bytes = [0u8; RANDOM_BYTES];
        match yagura_os::fill_random(&mut bytes) {
            Ok(()) => StatusToken {
                hex: Some(to_lowercase_hex(&bytes)),
            },
            Err(err) => {
                tracing::warn!(
                    "failed to obtain randomness for the StatusEvent auth token ({err}); every \
                     StatusEvent will be rejected until the daemon is restarted"
                );
                StatusToken { hex: None }
            }
        }
    }

    /// Adopts `value` as a token, provided it is a well-formed 32-character
    /// hex string. This is the only supported way to hand a test, or a
    /// development setup, a token the daemon already knows the value of.
    #[must_use]
    pub fn from_override(value: &str) -> Option<StatusToken> {
        if Self::is_well_formed(value) {
            Some(StatusToken {
                hex: Some(value.to_string()),
            })
        } else {
            None
        }
    }

    /// Whether `value` has the shape of a token: exactly [`HEX_LEN`] ASCII hex
    /// characters.
    ///
    /// This is a *shape* check only -- it says nothing about which token, if
    /// any, `value` equals. It lets a caller reject a malformed candidate
    /// before it has resolved which window's token to compare it against.
    #[must_use]
    pub fn is_well_formed(value: &str) -> bool {
        value.len() == HEX_LEN && value.bytes().all(|byte| byte.is_ascii_hexdigit())
    }

    /// The token's hex value, or `None` in the unverifiable state.
    ///
    /// Used only to decide whether `YAGURA_STATUS_TOKEN` gets injected into a
    /// spawned child's environment; this is not a general-purpose accessor
    /// and must never be routed into a log line, an error message, or any
    /// other rendering (enforced by `tests/status_token.rs`'s source scan).
    #[must_use]
    pub fn as_hex(&self) -> Option<&str> {
        self.hex.as_deref()
    }

    /// Reports whether `candidate` equals this token's value.
    ///
    /// Always `false` in the unverifiable state, and never panics regardless
    /// of `candidate`'s length or contents (including non-ASCII input). The
    /// comparison of an equal-length candidate accumulates via XOR so it does
    /// not short-circuit on the first differing byte (constant-time compare).
    #[must_use]
    pub fn verify(&self, candidate: &str) -> bool {
        let Some(hex) = self.hex.as_deref() else {
            return false;
        };
        let expected = hex.as_bytes();
        let actual = candidate.as_bytes();
        if expected.len() != actual.len() {
            return false;
        }
        let mut diff: u8 = 0;
        for index in 0..expected.len() {
            diff |= expected[index] ^ actual[index];
        }
        diff == 0
    }
}

impl std::fmt::Debug for StatusToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = if self.hex.is_some() {
            "<redacted>"
        } else {
            "<unverifiable>"
        };
        f.debug_struct("StatusToken")
            .field("state", &state)
            .finish()
    }
}

/// Renders `bytes` as a lowercase hex string, two characters per byte.
fn to_lowercase_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from(DIGITS[usize::from(byte >> 4)]));
        out.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    out
}
