//! A request that waits a day of virtual time for its answer still ends
//! with that answer, not with a failure. The connection is in memory so
//! the paused clock moves in one order everywhere.

use std::time::Duration;

use tokio::sync::Notify;
use yagura_protocol::DaemonMessage;

use super::*;
use crate::host::fake::{Reaction, ScriptedDaemon};
use crate::pool_outcomes::{DecideApprovalOutcome, map_decide_approval_result};

// The daemon holds its answer back for this long of virtual time.
const A_DAY: Duration = Duration::from_secs(24 * 60 * 60);

#[tokio::test(start_paused = true)]
async fn a_decision_the_daemon_answers_after_a_day_settles_with_that_answer() {
    let reached = Arc::new(Notify::new());
    let gate = Arc::new(Notify::new());
    let daemon = ScriptedDaemon::new({
        let reached = Arc::clone(&reached);
        let gate = Arc::clone(&gate);
        move |request| match request {
            ClientMessage::ApprovalDecision { request_id, .. } => Reaction::replies_after(
                Arc::clone(&reached),
                Arc::clone(&gate),
                vec![DaemonMessage::ApprovalResolved {
                    request_id: request_id.clone(),
                }],
            ),
            _ => None,
        }
    });
    let (near, far) = tokio::io::duplex(1 << 16);
    daemon.serve_stream(far);
    let (generation, _reader) = Generation::wired(near);
    let decision = ClientMessage::ApprovalDecision {
        request_id: "r1".to_owned(),
        approved: true,
    };

    let (answered, ()) = tokio::join!(
        generation.request_step(&decision, PendingKind::DecideApproval, None),
        async {
            yagura_test_support::ipc::reply_within("the request", reached.notified()).await;
            tokio::time::sleep(A_DAY).await;
            gate.notify_one();
        }
    );

    let decided = map_decide_approval_result(answered.map(|answer| {
        answer.and_then(|reply| match reply {
            Reply::Done => Ok(()),
            _ => Err(IpcError::UnexpectedReply),
        })
    }));
    assert!(
        matches!(decided, Ok(DecideApprovalOutcome::Decided)),
        "got {decided:?}"
    );
}

// A daemon holds an open until a human permits it and answers other requests
// meanwhile; a decision written then is answered without waiting for it.
#[tokio::test(start_paused = true)]
async fn a_decision_is_answered_while_an_open_waits_for_its_permit() {
    let reached = Arc::new(Notify::new());
    let gate = Arc::new(Notify::new());
    let daemon = ScriptedDaemon::new({
        let reached = Arc::clone(&reached);
        let gate = Arc::clone(&gate);
        move |request| match request {
            ClientMessage::OpenWindow { .. } => Reaction::replies_aside(
                Arc::clone(&reached),
                Arc::clone(&gate),
                vec![DaemonMessage::WindowOpened {
                    window_id: "w1".to_owned(),
                    detection_warning: None,
                    name: None,
                }],
            ),
            ClientMessage::ApprovalDecision { request_id, .. } => {
                Reaction::reply(DaemonMessage::ApprovalResolved {
                    request_id: request_id.clone(),
                })
            }
            _ => None,
        }
    });
    let (near, far) = tokio::io::duplex(1 << 16);
    daemon.serve_stream(far);
    let (generation, _reader) = Generation::wired(near);
    let open = ClientMessage::OpenWindow {
        command: vec!["sh".to_owned()],
        cwd: None,
        name: None,
        vendor: None,
        detection: None,
        on_name_taken: Default::default(),
    };

    let opening = tokio::spawn({
        let generation = Arc::clone(&generation);
        async move { generation.request(&open, PendingKind::OpenWindow).await }
    });
    yagura_test_support::ipc::reply_within("the held open", reached.notified()).await;
    let deciding = tokio::spawn({
        let generation = Arc::clone(&generation);
        async move { crate::ipc_client::request_decide_approval(Some(&generation), "r1", true).await }
    });
    daemon.recorded_at_least(2).await;
    let decided = map_decide_approval_result(
        yagura_test_support::ipc::reply_within("the decision", deciding)
            .await
            .expect("the decision task ran"),
    );
    assert!(
        matches!(decided, Ok(DecideApprovalOutcome::Decided)),
        "got {decided:?}"
    );
    assert!(!opening.is_finished(), "the open is still held");

    gate.notify_one();
    let opened = yagura_test_support::ipc::reply_within("the open", opening)
        .await
        .expect("the open task ran");
    assert!(
        matches!(&opened, Ok(Reply::WindowOpened(reply)) if reply.window_id == "w1"),
        "got {opened:?}"
    );
}

fn decision(request_id: &str) -> ClientMessage {
    ClientMessage::ApprovalDecision {
        request_id: request_id.to_owned(),
        approved: true,
    }
}

/// Polls `future` once with a waker nothing wakes: what it does before it
/// would wait is done when this returns.
fn poll_now<F: std::future::Future + ?Sized>(future: Pin<&mut F>) -> std::task::Poll<F::Output> {
    future.poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
}

/// The next request written to `peer`, with the correlation id it carries.
async fn next_request(peer: &mut tokio::io::DuplexStream) -> (Option<u64>, ClientMessage) {
    use tokio::io::AsyncReadExt;
    let mut length = [0u8; 4];
    peer.read_exact(&mut length).await.expect("frame length");
    let mut body = vec![0u8; u32::from_le_bytes(length) as usize];
    peer.read_exact(&mut body).await.expect("frame body");
    yagura_protocol::decode_correlated_client_message(&body).expect("a client frame")
}

// The request's key is fresh, so nothing written later would overwrite the
// slot a cancelled request left: only its own owner can clear it.
#[tokio::test]
async fn a_request_dropped_while_it_waits_for_its_answer_leaves_no_slot_behind() {
    let (near, mut far) = tokio::io::duplex(1 << 16);
    let (generation, _reader) = Generation::wired(near);
    let request = decision("r1");

    let key = {
        let mut waiting =
            std::pin::pin!(generation.request_step(&request, PendingKind::DecideApproval, None));
        assert!(
            poll_now(waiting.as_mut()).is_pending(),
            "the request waits for its answer"
        );
        let (key, written) =
            yagura_test_support::ipc::reply_within("the request's frame", next_request(&mut far))
                .await;
        assert_eq!(written, request);
        let key = key.expect("the request carries a key");
        assert!(
            generation.awaiting(Some(key)).is_some(),
            "the request waits under its key before it is dropped"
        );
        key
    };

    assert_eq!(
        generation.awaiting(Some(key)),
        None,
        "a dropped request must not stay in the table of requests waiting"
    );
}

// A frame cut short leaves the daemon reading the next bytes as the rest of
// it, so a write dropped midway must end writing on that connection, as a
// failed write does.
#[tokio::test]
async fn a_request_dropped_midway_through_its_frame_has_nothing_written_after_the_cut() {
    use tokio::io::AsyncReadExt;
    const ROOM: usize = 16;
    let (near, mut far) = tokio::io::duplex(ROOM);
    let (generation, _reader) = Generation::wired(near);

    {
        let first = decision("r1");
        let mut writing =
            std::pin::pin!(generation.request_step(&first, PendingKind::DecideApproval, None));
        assert!(
            poll_now(writing.as_mut()).is_pending(),
            "the frame waits for room on the connection"
        );
    }
    let mut cut = [0u8; ROOM];
    yagura_test_support::ipc::reply_within("the cut frame", far.read_exact(&mut cut))
        .await
        .expect("the bytes written before the drop");
    let length = u32::from_le_bytes([cut[0], cut[1], cut[2], cut[3]]) as usize;
    assert!(4 + length > ROOM, "the dropped frame was cut short");

    let second = decision("r2");
    let mut next =
        std::pin::pin!(generation.request_step(&second, PendingKind::DecideApproval, None));
    let mut after_cut = [0u8; 64];
    let read = yagura_test_support::ipc::reply_within("the next request or its bytes", async {
        tokio::select! {
            biased;
            _ = next.as_mut() => None,
            read = far.read(&mut after_cut) => Some(read),
        }
    })
    .await;
    let written = match read {
        Some(read) => read.expect("reading the connection"),
        None => {
            let pending = std::pin::pin!(tokio::task::unconstrained(far.read(&mut after_cut)));
            match poll_now(pending) {
                std::task::Poll::Ready(read) => read.expect("reading the connection"),
                std::task::Poll::Pending => 0,
            }
        }
    };
    assert_eq!(
        written,
        0,
        "nothing may be written on a connection after a frame cut short; got {:?}",
        &after_cut[..written]
    );
}
