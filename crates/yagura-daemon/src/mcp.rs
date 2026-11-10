//! The daemon's MCP layer: the tools a CLI calls through its window's relay,
//! served over the relay contract ([`yagura_protocol::relay`]).
//!
//! MCP stays a thin layer over the IPC: a tool builds a [`ClientMessage`] and
//! sends it over a [`DaemonTransport`], and in the daemon that transport is a
//! connection of the relay session's own through the same connection body the
//! IPC endpoint runs ([`relay_session`]).

use std::future::Future;

use yagura_protocol::{ClientMessage, DaemonMessage};

pub mod plan_tools;
pub(crate) mod relay_session;
pub mod report_tools;
pub mod server;
pub mod ticket_tools;
pub mod tools;

/// Failure modes a [`DaemonTransport`] implementation can report.
///
/// The mapping onto MCP tool error codes is fixed: `Unavailable` ->
/// `daemon_unavailable`, `Protocol` -> `internal`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportError {
    /// The request could not be carried to the daemon's connection body, or
    /// the connection ended before it answered.
    Unavailable(String),
    /// The answer could not be decoded as a well-formed [`DaemonMessage`].
    Protocol(String),
}

/// How the daemon settled a connection's identity claim, as reported by
/// [`DaemonTransport::requester_identity`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequesterIdentity {
    /// No claim was made; the connection is an external client.
    External,
    /// The daemon accepted the claim. Carries the window ID it settled on.
    Window(String),
    /// The daemon refused the claim. Carries an English warning for the
    /// caller; the connection still works as an external client.
    Refused(String),
}

/// One request/response round trip against the daemon's connection body. The
/// tools are generic over it, so they can be measured against a double.
pub trait DaemonTransport: Send + Sync {
    /// This connection's identity as the daemon settled it. Synchronous so the
    /// tool layer can call it right after a round trip.
    fn requester_identity(&self) -> RequesterIdentity;

    /// Sends `msg` and returns the daemon's reply.
    fn round_trip(
        &self,
        msg: ClientMessage,
    ) -> impl Future<Output = Result<DaemonMessage, TransportError>> + Send;
}
