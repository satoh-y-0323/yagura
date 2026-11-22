//! Reaching the daemon's endpoint: one attempt and the retries, with no
//! owner state.

use yagura_os::{IpcTransport, PlatformIpc};

use super::super::{Connection, IpcError};
use crate::constants::{
    CONNECT_RETRY_INTERVAL, CONNECT_RETRY_LIMIT, POST_SPAWN_CONNECT_RETRY_LIMIT,
};

pub(super) async fn connect_once(endpoint: &str) -> Option<Connection> {
    PlatformIpc::default().connect(endpoint).await.ok()
}

/// Connects to a running daemon, retrying a refused endpoint up to `limit`
/// times. The first attempt precedes any wait, so an endpoint that is free
/// costs no delay (waiting first would cost one interval per window).
async fn connect_retrying_with_limit(endpoint: &str, limit: u32) -> Option<Connection> {
    for attempt in 0..limit {
        if attempt > 0 {
            tokio::time::sleep(CONNECT_RETRY_INTERVAL).await;
        }
        if let Some(connection) = connect_once(endpoint).await {
            return Some(connection);
        }
    }
    None
}

/// [`connect_retrying_with_limit`] with the ordinary [`CONNECT_RETRY_LIMIT`]
/// budget.
pub(super) async fn connect_retrying(endpoint: &str) -> Option<Connection> {
    connect_retrying_with_limit(endpoint, CONNECT_RETRY_LIMIT).await
}

/// Connects to a daemon that was just spawned. Unlike [`connect_retrying`]
/// this waits before the first attempt, because the daemon still has to bind
/// its endpoint -- reading the login shell's environment first on Unix, so
/// the budget ([`POST_SPAWN_CONNECT_RETRY_LIMIT`]) is longer than the ordinary one.
pub(super) async fn connect_after_spawn(endpoint: &str) -> Result<Connection, IpcError> {
    tokio::time::sleep(CONNECT_RETRY_INTERVAL).await;
    connect_retrying_with_limit(endpoint, POST_SPAWN_CONNECT_RETRY_LIMIT)
        .await
        .ok_or(IpcError::ConnectTimeout)
}

#[cfg(test)]
mod tests;
