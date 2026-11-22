#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! What a client sees of a daemon that wrote and closed its connection: the
//! write error that says the peer closed, the bytes the peer sent before it
//! closed, and the end that follows them. A GUI generation tells a peer that
//! closed from a write that failed for another reason by this, and reads the
//! bytes the peer sent before it decides how the connection ended.

use std::io;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use yagura_os::{IpcTransport, PeerAuth, PlatformIpc};
use yagura_test_support::ipc::{reply_within, unique_endpoint_name};

/// Endpoint-name prefix for this file, kept distinct from the daemon's own so
/// a pipe created here can never be mistaken for a resident daemon's.
const PREFIX: &str = "yagura-test-close";

/// What the daemon writes before it closes.
const SENT: [u8; 64] = [0xAB; 64];

type Accepted = <PlatformIpc as PeerAuth>::Connection;
type Client = <PlatformIpc as IpcTransport>::ClientConnection;

async fn connected(tag: &str) -> (Accepted, Client) {
    let ipc = PlatformIpc::default();
    let security = ipc
        .endpoint_security()
        .expect("endpoint_security should succeed");
    let endpoint = unique_endpoint_name(PREFIX, tag);
    let mut listener = ipc
        .bind(&endpoint, security)
        .await
        .expect("bind should succeed");
    let (accepted, client) = tokio::join!(ipc.accept(&mut listener), ipc.connect(&endpoint));
    (
        accepted.expect("accept should succeed"),
        client.expect("connect should succeed"),
    )
}

/// The daemon writes `SENT` and then lets go of both halves of its
/// connection, as it does when it ends one.
async fn daemon_writes_and_closes(accepted: Accepted) {
    let (reader, mut writer) = tokio::io::split(accepted);
    writer.write_all(&SENT).await.expect("the daemon writes");
    writer.flush().await.expect("the daemon flushes");
    drop(reader);
    drop(writer);
}

/// Writes small chunks until a write fails and returns that failure. On
/// Windows the peer's handle closes a little after the daemon let go of it, so
/// some writes may succeed first.
async fn write_until_it_fails<W: AsyncWrite + Unpin>(write: &mut W) -> io::Error {
    let chunk = [0u8; 4096];
    reply_within("a write to a closed peer", async {
        loop {
            match write.write(&chunk).await {
                Ok(0) => panic!("a write of {} bytes wrote none", chunk.len()),
                Ok(_) => {}
                Err(error) => return error,
            }
        }
    })
    .await
}

/// Reads until the connection ends, by end of file or by a read error, and
/// returns what it read before that.
async fn read_until_it_ends<R: AsyncRead + Unpin>(read: &mut R) -> Vec<u8> {
    reply_within("the end of the connection", async {
        let mut got = Vec::new();
        let mut buffer = [0u8; 4096];
        loop {
            match read.read(&mut buffer).await {
                Ok(0) | Err(_) => return got,
                Ok(count) => got.extend_from_slice(&buffer[..count]),
            }
        }
    })
    .await
}

/// What the delivered bytes count on this platform once `SENT` arrived.
fn delivered_after_sent() -> usize {
    if cfg!(unix) { SENT.len() } else { 0 }
}

/// Takes the delivered bytes in pieces of `piece_len` bytes, until a call
/// returns none or fails, and returns them in order with the failure, if any.
fn read_delivered_in_pieces(
    read: &mut yagura_os::ClientReadHalf,
    piece_len: usize,
) -> (Vec<u8>, Option<io::Error>) {
    let mut got = Vec::new();
    let mut piece = [0u8; 32];
    let piece = &mut piece[..piece_len];
    loop {
        match read.read_delivered(piece) {
            Err(error) => return (got, Some(error)),
            Ok(0) => return (got, None),
            Ok(count) => {
                assert!(
                    count <= piece.len(),
                    "{count} bytes read into {}",
                    piece.len()
                );
                got.extend_from_slice(&piece[..count]);
            }
        }
    }
}

#[tokio::test]
async fn a_closed_peer_shows_in_the_write_error_and_what_it_sent_is_read_before_its_end() {
    let (accepted, client) = connected("closed").await;
    daemon_writes_and_closes(accepted).await;
    let (mut read, mut write): (yagura_os::ClientReadHalf, _) = yagura_os::split_client(client);

    let error = write_until_it_fails(&mut write).await;
    assert!(
        yagura_os::write_error_shows_peer_closed(&error),
        "{error:?} (raw {:?}) is what a write to a closed peer fails with",
        error.raw_os_error()
    );

    // Before any other read: the bytes the operating system already handed to
    // this end. A reader that stops on a write error reads them here; where
    // they cannot be read without waiting, none are returned.
    assert_eq!(
        read.delivered_len()
            .expect("the delivered bytes are counted"),
        delivered_after_sent()
    );
    // The read ends by end of file, or by a reset where another process still
    // held a copy of the daemon's end when it closed; only the bytes matter.
    let (delivered, _ended) = read_delivered_in_pieces(&mut read, 16);
    let rest = read_until_it_ends(&mut read).await;
    if cfg!(unix) {
        assert_eq!(delivered, SENT);
        assert!(rest.is_empty(), "{rest:?} after the delivered bytes");
    } else {
        assert!(delivered.is_empty(), "{delivered:?} delivered");
        assert_eq!(rest, SENT);
    }

    // The other direction of the judgement: a failure that is not the peer's
    // close does not say it.
    assert!(!yagura_os::write_error_shows_peer_closed(&io::Error::from(
        io::ErrorKind::OutOfMemory
    )));

    // A daemon that wrote and has not closed: the delivered bytes are read
    // and the next call returns at once with none, instead of waiting for more.
    let (accepted, client) = connected("open").await;
    let (_daemon_read, mut daemon_write) = tokio::io::split(accepted);
    daemon_write
        .write_all(&SENT)
        .await
        .expect("the daemon writes");
    daemon_write.flush().await.expect("the daemon flushes");
    let (mut read, _write): (yagura_os::ClientReadHalf, _) = yagura_os::split_client(client);
    let (queued, first, second_count) = reply_within(
        "a read of the delivered bytes that must not wait",
        tokio::task::spawn_blocking(move || {
            let queued = read
                .delivered_len()
                .expect("the delivered bytes are counted");
            let (first, ended) = read_delivered_in_pieces(&mut read, 16);
            assert!(ended.is_none(), "{ended:?}");
            let second_count = read
                .read_delivered(&mut [0u8; 16])
                .expect("the next call returns");
            (queued, first, second_count)
        }),
    )
    .await
    .expect("the blocking read ran");
    assert_eq!(queued, delivered_after_sent());
    assert_eq!(second_count, 0);
    if cfg!(unix) {
        assert_eq!(first, SENT);
    } else {
        assert!(first.is_empty(), "{first:?}");
    }

    // A daemon that closes without reading what this end sent. Linux resets
    // the connection then, and the reset follows the bytes it delivered. A
    // piece that does not divide `SENT` makes one call read some of them
    // before the reset is met, and that call must return them.
    let (accepted, client) = connected("unread").await;
    let (mut read, mut write): (yagura_os::ClientReadHalf, _) = yagura_os::split_client(client);
    write.write_all(&[1u8]).await.expect("the client writes");
    write.flush().await.expect("the client flushes");
    daemon_writes_and_closes(accepted).await;
    // Another test's forked child can hold the daemon's socket for a moment
    // after the daemon let go of it, so wait until the close shows.
    let error = write_until_it_fails(&mut write).await;
    assert!(
        yagura_os::write_error_shows_peer_closed(&error),
        "{error:?}"
    );
    let (delivered, ended) = read_delivered_in_pieces(&mut read, 24);
    let rest = read_until_it_ends(&mut read).await;
    let seen = format!(
        "{} delivered bytes, then {:?} (raw {:?}), {} bytes after",
        delivered.len(),
        ended.as_ref().map(io::Error::kind),
        ended.as_ref().and_then(io::Error::raw_os_error),
        rest.len()
    );
    let mut all = delivered;
    all.extend_from_slice(&rest);
    assert_eq!(
        all, SENT,
        "every byte the daemon sent before it closed: {seen}"
    );
    if cfg!(target_os = "linux") {
        let error = ended.expect("the reset follows the delivered bytes");
        assert_eq!(
            error.kind(),
            io::ErrorKind::ConnectionReset,
            "{error:?}: {seen}"
        );
    }
}
