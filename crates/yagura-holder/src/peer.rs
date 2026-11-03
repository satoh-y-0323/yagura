//! The first frame of a connection: only the process that holds the
//! instance's daemon endpoint is let in, and only in this holder's version.

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use yagura_os::{IpcTransport, PeerAuth, PlatformIpc};
use yagura_protocol::holder::{
    HOLDER_PROTOCOL, HolderFrame, HolderFrameBuffer, encode_holder_frame,
};

/// A connection to this holder's endpoint.
pub(crate) type Connection = <PlatformIpc as PeerAuth>::Connection;

/// The most a `Hello` may take, so a peer not yet let in cannot make the
/// holder buffer more.
const MAX_HELLO_BYTES: usize = 4096;

/// How long a connection may take from being accepted to being let in, the
/// check against the daemon endpoint included: bounds what a connection that
/// never completes holds, and is not a synchronisation point.
const HANDSHAKE_LIMIT: Duration = Duration::from_secs(10);

/// A connection whose `Hello` was accepted, with the bytes read after it.
pub(crate) struct Accepted {
    pub(crate) connection: Connection,
    pub(crate) frames: HolderFrameBuffer,
}

/// Reads the connection's `Hello` and decides whether to let it in, within
/// [`HANDSHAKE_LIMIT`]. A refused connection is closed: after
/// `VersionRejected` when it is the daemon speaking another version, without
/// a word otherwise.
pub(crate) async fn handshake(
    connection: Connection,
    daemon_endpoint: &str,
    user_id: &str,
) -> Option<Accepted> {
    tokio::time::timeout(
        HANDSHAKE_LIMIT,
        decide(connection, daemon_endpoint, user_id),
    )
    .await
    .ok()
    .flatten()
}

async fn decide(
    mut connection: Connection,
    daemon_endpoint: &str,
    user_id: &str,
) -> Option<Accepted> {
    let mut frames = HolderFrameBuffer::new();
    let frame = read_first_frame(&mut connection, &mut frames).await?;
    let HolderFrame::Hello { protocol, .. } = frame else {
        return None;
    };
    if !is_daemon(&connection, daemon_endpoint, user_id).await {
        return None;
    }
    if protocol != HOLDER_PROTOCOL {
        let rejected = encode_holder_frame(&HolderFrame::VersionRejected {
            protocol: HOLDER_PROTOCOL,
        });
        let _ = connection.write_all(&rejected).await;
        let _ = connection.flush().await;
        return None;
    }
    Some(Accepted { connection, frames })
}

async fn read_first_frame(
    connection: &mut Connection,
    frames: &mut HolderFrameBuffer,
) -> Option<HolderFrame> {
    let mut chunk = [0u8; 1024];
    loop {
        if let Some(frame) = frames.next_frame().ok()? {
            return Some(frame);
        }
        if frames.buffered() >= MAX_HELLO_BYTES {
            return None;
        }
        let read = connection.read(&mut chunk).await.ok()?;
        if read == 0 {
            return None;
        }
        frames.push(&chunk[..read]);
    }
}

/// Whether the peer of `connection` is the process serving the instance's
/// daemon endpoint right now, as the operating system reports both process
/// ids. Fails closed when either is unknown; a process id of 0 is unknown on
/// every platform (a peer outside this pid namespace, a client the system
/// cannot name), so equal zeros never match.
async fn is_daemon(connection: &Connection, daemon_endpoint: &str, user_id: &str) -> bool {
    let ipc = PlatformIpc::default();
    if !yagura_os::is_authorized_peer(ipc.peer_id(connection).as_deref(), user_id) {
        return false;
    }
    let Some(peer) = ipc.peer_process_id(connection).filter(|&pid| pid != 0) else {
        return false;
    };
    let Ok(daemon) = ipc.connect(daemon_endpoint).await else {
        return false;
    };
    ipc.server_process_id(&daemon) == Some(peer)
}
