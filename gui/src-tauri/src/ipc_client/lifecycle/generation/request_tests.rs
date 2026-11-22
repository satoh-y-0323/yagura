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
