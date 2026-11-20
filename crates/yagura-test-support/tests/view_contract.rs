#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// Integration-test target: clippy's `allow-unwrap-in-tests` /
// `allow-expect-in-tests` cover only `#[cfg(test)]` code, so a `tests/` file
// needs this header explicitly.

//! Contract tests for `ViewClient::sync` and `ViewClient::assert_no_frame`
//! against a scripted peer. Against a correct daemon these helpers only ever
//! see the quiet case, so their failing branches are measured here.

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use yagura_os::{IpcTransport, PeerAuth, PlatformIpc};
use yagura_protocol::{
    ClientMessage, CursorShape, CursorState, DaemonMessage, Frame, ViewState,
    decode_client_message, encode_daemon_message, frame_decode, frame_encode,
};
use yagura_test_support::ipc;
use yagura_test_support::view::ViewClient;

type ServerConnection = <PlatformIpc as PeerAuth>::Connection;

fn frame(seq: u64, reset: bool) -> DaemonMessage {
    DaemonMessage::Frame(Frame {
        seq,
        cols: 4,
        rows: 2,
        reset,
        last: true,
        attrs: Vec::new(),
        lines: Vec::new(),
        cursor: CursorState {
            row: 0,
            col: 0,
            visible: true,
            shape: CursorShape::Block,
        },
        view: ViewState {
            display_offset: 0,
            history_rows: 0,
            mouse_reporting: false,
            alt_screen: false,
        },
        overrides: Vec::new(),
    })
}

/// The daemon side of one connection.
struct Peer {
    conn: ServerConnection,
    buf: Vec<u8>,
}

impl Peer {
    async fn send(&mut self, msg: &DaemonMessage) {
        let bytes = frame_encode(&encode_daemon_message(msg));
        self.conn
            .write_all(&bytes)
            .await
            .expect("write to the view");
    }

    async fn recv(&mut self) -> ClientMessage {
        loop {
            if let Some((body, consumed)) = frame_decode(&self.buf).expect("a frame within the cap")
            {
                let msg = decode_client_message(body).expect("a client message");
                self.buf.drain(..consumed);
                return msg;
            }
            let mut chunk = [0u8; 4096];
            let n = self
                .conn
                .read(&mut chunk)
                .await
                .expect("read from the view");
            assert!(n > 0, "the view closed the connection");
            self.buf.extend_from_slice(&chunk[..n]);
        }
    }
}

/// Attaches a `ViewClient` to a peer that answers the attach with frame 1.
async fn attached(tag: &str) -> (ViewClient, Peer) {
    let name = ipc::unique_endpoint_name("yagura-view-contract", tag);
    let transport = PlatformIpc::default();
    let security = transport.endpoint_security().expect("endpoint security");
    let mut listener = transport.bind(&name, security).await.expect("bind");
    let conn = ipc::connect_with_retry(&name).await;
    let mut peer = Peer {
        conn: transport.accept(&mut listener).await.expect("accept"),
        buf: Vec::new(),
    };
    let serve = tokio::spawn(async move {
        assert!(matches!(
            peer.recv().await,
            ClientMessage::AttachView { .. }
        ));
        peer.send(&DaemonMessage::Attached {
            child_exited: false,
            exit_code: None,
        })
        .await;
        peer.send(&frame(1, true)).await;
        assert_eq!(peer.recv().await, ClientMessage::FrameAck { seq: 1 });
        peer
    });
    let view = ViewClient::attach(conn, "w", None).await;
    (view, serve.await.expect("the attach script"))
}

#[tokio::test]
async fn sync_applies_and_acknowledges_the_frames_sent_before_the_pong() {
    let (mut view, mut peer) = attached("sync").await;
    let serve = tokio::spawn(async move {
        assert_eq!(peer.recv().await, ClientMessage::Ping);
        peer.send(&frame(2, false)).await;
        peer.send(&DaemonMessage::Pong).await;
        assert_eq!(peer.recv().await, ClientMessage::FrameAck { seq: 2 });
    });

    view.sync().await;

    assert_eq!(
        view.completed.iter().map(|f| f.seq).collect::<Vec<_>>(),
        [1, 2],
        "sync must apply the frame that came before the pong"
    );
    tokio::time::timeout(yagura_test_support::HANG_GUARD, serve)
        .await
        .expect("sync must acknowledge the frame it applied")
        .expect("the sync script");
}

#[tokio::test]
#[should_panic(expected = "a frame (seq 2) arrived but nothing may change")]
async fn assert_no_frame_fails_when_a_frame_comes_before_the_pong() {
    let (mut view, mut peer) = attached("before-pong").await;
    let _serve = tokio::spawn(async move {
        assert_eq!(peer.recv().await, ClientMessage::Ping);
        peer.send(&frame(2, false)).await;
        peer.send(&DaemonMessage::Pong).await;
        peer
    });

    view.assert_no_frame("nothing may change").await;
}

#[tokio::test]
async fn assert_no_frame_returns_at_the_pong_and_leaves_what_follows_unread() {
    let (mut view, mut peer) = attached("after-pong").await;
    let _serve = tokio::spawn(async move {
        assert_eq!(peer.recv().await, ClientMessage::Ping);
        peer.send(&DaemonMessage::Pong).await;
        peer.send(&frame(2, false)).await;
        peer
    });

    view.assert_no_frame("nothing may change").await;

    assert_eq!(
        view.completed.iter().map(|f| f.seq).collect::<Vec<_>>(),
        [1],
        "a frame after the pong is not the helper's to read"
    );
    let parts = view.next_complete_frame().await;
    assert_eq!(
        parts.last().map(|part| part.seq),
        Some(2),
        "the frame after the pong must still be on the connection"
    );
}
