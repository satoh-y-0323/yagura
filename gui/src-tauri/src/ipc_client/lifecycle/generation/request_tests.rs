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
