#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! A window's watcher is the child subreaper of the window's tree: the orphans
//! handed to it are reaped as each one ends, while the child it runs as the
//! tree's leader stays its child, unreaped.

use yagura_protocol::holder::HolderFrame;

use crate::holder::{
    NO_DEADLINE_MS, Peer, Reaper, Rig, hold_daemon_endpoint, path_arg, ps_state, wait_until,
};
use crate::watcher::{children_by_ps, read_pids, watched};

const ORPHANS: usize = 10;

#[tokio::test(flavor = "multi_thread")]
async fn orphans_are_reaped_as_they_end_and_the_leader_is_left_unreaped() {
    let rig = Rig::new("orphans");
    let _daemon = hold_daemon_endpoint(&rig).await;
    let pid_file = rig.file("orphans");
    let count = ORPHANS.to_string();
    let mut holder = rig.start(&rig.launch(
        &["orphans", path_arg(&pid_file), &count, "leave"],
        NO_DEADLINE_MS,
    ));
    let (mut peer, _) = Peer::welcomed(&rig.record().await.endpoint).await;
    peer.text_until("HOLDER-CHILD-ORPHANS").await;
    let orphans = read_pids(&pid_file);
    let _reapers: Vec<Reaper> = orphans
        .iter()
        .map(|&pid| Reaper {
            pid: libc::pid_t::try_from(pid).unwrap(),
            always: false,
        })
        .collect();
    assert_eq!(orphans.len(), ORPHANS, "not every orphan was forked");
    let record = rig.record_where(|record| record.child_pid.is_some()).await;
    let leader = record.child_pid.unwrap();
    let (watcher, _) = watched(&record);

    wait_until("every orphan is reaped as it ends", || {
        orphans.iter().all(|&pid| ps_state(pid).is_none())
    })
    .await;
    assert_eq!(
        children_by_ps(watcher),
        vec![leader],
        "the watcher's children are not just its leader {leader}"
    );

    peer.send(HolderFrame::Terminate { tree: true }).await;
    holder.wait_exit().await;
}
