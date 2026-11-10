#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// Dev-only fixture crate (`publish = false`): a fixture that cannot establish
// its precondition must fail loud rather than degrade silently.

//! Shared test fixtures for the yagura workspace.
//!
//! Holds the helpers that the integration tests of `yagura-daemon`,
//! `yagura-mcp`, `yagura-os` and `yagura-gui` would otherwise duplicate. No
//! production binary depends on this crate.

use std::time::Duration;

/// The longest any single wait of a test may take before the test is called
/// hung. Reaching it fails the test; nothing is judged by waiting this long.
pub const HANG_GUARD: Duration = Duration::from_secs(60);

pub mod detection;
pub mod frame;
pub mod ipc;
pub mod paths;
pub mod process;
pub mod relay_stdio;
pub mod scan;
pub mod view;
