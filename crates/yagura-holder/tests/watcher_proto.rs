#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: the frames between a window's holder and its watcher.
//! The watcher's binary reads the same module without serde, so these tests
//! pin what both sides must agree on: that every frame survives a round trip,
//! that a watcher of another version is refused when it first speaks, and how
//! the watcher's descriptors are split over `SCM_RIGHTS` sends.

/// The most descriptors one `SCM_RIGHTS` message may carry on Linux
/// (`SCM_MAX_FD`); a longer list fails with `EINVAL`.
const MOST_FDS_PER_SEND: usize = 253;

fn sample_frames() -> Vec<yagura_holder::watcher_proto::WatcherFrame> {
    use yagura_holder::watcher_proto::{WATCHER_PROTOCOL, WatcherFrame};

    vec![
        WatcherFrame::Hello {
            protocol: WATCHER_PROTOCOL,
        },
        WatcherFrame::Spawn {
            argv: vec!["sh".to_owned(), "-c".to_owned(), "echo \"hi\"\n".to_owned()],
            cwd: "/work/dir".to_owned(),
            env: vec![
                ("TERM".to_owned(), "xterm-256color".to_owned()),
                ("EMPTY".to_owned(), String::new()),
            ],
            record_path: "/run/holders/w1.json".to_owned(),
            socket_path: "/run/holders/w1.sock".to_owned(),
            lock_path: "/run/holders/w1.lock".to_owned(),
        },
        WatcherFrame::LeaderStarted {
            pid: u32::MAX,
            start: Some("12345 6".to_owned()),
        },
        WatcherFrame::LeaderStarted {
            pid: 7,
            start: None,
        },
        WatcherFrame::ScopeMembers { count: 253 },
        WatcherFrame::ScopeReady {
            cgroup_path: "/user.slice/yagura_w1_child.scope".to_owned(),
            outside_members: 3,
        },
        WatcherFrame::LeaderExited { code: Some(-1) },
        WatcherFrame::LeaderExited { code: None },
        WatcherFrame::SweepDone { remaining: 0 },
        WatcherFrame::SweepDone { remaining: 2 },
        WatcherFrame::Terminate,
    ]
}

#[test]
fn every_watcher_frame_survives_an_encode_decode_round_trip() {
    use yagura_holder::watcher_proto::{WatcherFrame, decode_watcher_frame, encode_watcher_frame};

    for frame in sample_frames() {
        let own = encode_watcher_frame(&frame);
        let mut stream = own.clone();
        // A second frame behind the first must be left for the next call.
        stream.extend_from_slice(&encode_watcher_frame(&WatcherFrame::Terminate));

        let (decoded, consumed) = decode_watcher_frame(&stream)
            .expect("decode")
            .expect("complete frame");
        assert_eq!(decoded, frame);
        assert_eq!(consumed, own.len(), "{frame:?} consumed past its own end");

        for cut in 0..own.len() {
            assert!(
                matches!(decode_watcher_frame(&own[..cut]), Ok(None)),
                "{frame:?} cut at {cut} of {} must wait for more bytes",
                own.len()
            );
        }
    }
}

/// The version guards a holder and a watcher built apart from meeting: a
/// watcher that speaks another version is refused on its first frame, so the
/// window is never opened on a mouth the two read differently.
#[test]
fn a_watchers_first_frame_of_another_version_is_refused() {
    use yagura_holder::watcher_proto::{
        WATCHER_PROTOCOL, WatcherFrame, decode_watcher_frame, encode_watcher_frame,
    };

    let same = encode_watcher_frame(&WatcherFrame::Hello {
        protocol: WATCHER_PROTOCOL,
    });
    assert!(matches!(decode_watcher_frame(&same), Ok(Some(_))));

    for other in [
        WATCHER_PROTOCOL.wrapping_add(1),
        WATCHER_PROTOCOL.wrapping_sub(1),
    ] {
        let bytes = encode_watcher_frame(&WatcherFrame::Hello { protocol: other });
        assert!(
            decode_watcher_frame(&bytes).is_err(),
            "a hello of version {other} must not decode"
        );
    }
}

/// A watcher passes the descriptors of the orphans it found to the holder
/// over `SCM_RIGHTS`; one message carries only so many, so a longer list is
/// sent in several, none over the limit and none lost.
#[test]
fn the_scope_members_are_split_into_sends_within_the_descriptor_limit() {
    for total in [0, 1, 252, 253, 254, 255, 506, 507, 1000, 100_000] {
        let batches = yagura_holder::watcher_proto::scope_member_batches(total);
        assert_eq!(
            batches.iter().sum::<usize>(),
            total,
            "{total} members must all be sent, once"
        );
        assert!(
            batches.iter().all(|&batch| batch <= MOST_FDS_PER_SEND),
            "{total} members split as {batches:?} sends more than {MOST_FDS_PER_SEND} at once"
        );
    }
}
