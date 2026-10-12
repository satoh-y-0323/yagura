#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// Integration-test target: clippy's `allow-unwrap-in-tests` /
// `allow-expect-in-tests` cover only `#[cfg(test)]` code, so a `tests/` file
// needs this header explicitly.

//! Contract tests for `ViewClient::sync` and `ViewClient::assert_no_frame`
//! against a scripted peer. Against a correct daemon these helpers only ever
//! see the quiet case, so their failing branches are measured here.

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use yagura_os::{IpcTransport, PeerAuth, PlatformIpc};
use yagura_protocol::{
    ClientMessage, CursorShape, CursorState, DaemonMessage, Frame, ViewState,
    decode_client_message, encode_daemon_message, frame_decode, frame_encode,
};
use yagura_test_support::ipc;
use yagura_test_support::view::ViewClient;

type ServerConnection = <PlatformIpc as PeerAuth>::Connection;

const GRACE: Duration = Duration::from_millis(300);

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

    /// Reads messages until one that is not a `FrameAck` arrives.
    async fn recv_skipping_acks(&mut self, acks: &mut Vec<u64>) -> ClientMessage {
        loop {
            match self.recv().await {
                ClientMessage::FrameAck { seq } => acks.push(seq),
                other => return other,
            }
        }
    }
}

/// Attaches a `ViewClient` to a peer that answers the attach with frame 1.
async fn attached(tag: &str) -> (ViewClient, Peer) {
    let name = ipc::unique_endpoint_name("yagura-view-contract", tag);
    let transport = PlatformIpc::default();
    let security = transport.endpoint_security().expect("endpoint security");
    let mut listener = transport.bind(&name, security).await.expect("bind");
    let conn = ipc::connect_with_retry(&name, Duration::from_secs(5)).await;
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
    tokio::time::timeout(Duration::from_secs(5), serve)
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

    view.assert_no_frame("nothing may change", GRACE).await;
}

#[tokio::test]
#[should_panic(expected = "arrived after the pong but nothing may change")]
async fn assert_no_frame_fails_when_a_frame_follows_the_pong() {
    let (mut view, mut peer) = attached("after-pong").await;
    let _serve = tokio::spawn(async move {
        assert_eq!(peer.recv().await, ClientMessage::Ping);
        peer.send(&DaemonMessage::Pong).await;
        peer.send(&frame(2, false)).await;
        peer
    });

    view.assert_no_frame("nothing may change", GRACE).await;
}

#[tokio::test]
async fn assert_no_frame_accepts_a_pushed_exit_after_the_pong() {
    let (mut view, mut peer) = attached("exit").await;
    let serve = tokio::spawn(async move {
        let mut acks = Vec::new();
        assert_eq!(
            peer.recv_skipping_acks(&mut acks).await,
            ClientMessage::Ping
        );
        peer.send(&DaemonMessage::Pong).await;
        peer.send(&DaemonMessage::Exit { exit_code: 0 }).await;
        peer
    });

    view.assert_no_frame("the child only exited", GRACE).await;

    assert_eq!(view.exited, Some(0), "the pushed exit must be recorded");
    drop(serve.await.expect("the exit script"));
}
