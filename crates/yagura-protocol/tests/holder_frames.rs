#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: the frames between the daemon and a window's holder.
//! A frame is a `u32` LE length of what follows, a `u16` LE type and a body;
//! the body of a byte-carrying frame is a `u64` LE sequence or id followed by
//! the raw bytes, and every other body is JSON.

use yagura_protocol::holder::{HolderFrame, decode_holder_frame, encode_holder_frame};

fn decode_whole(bytes: &[u8]) -> HolderFrame {
    let (frame, consumed) = decode_holder_frame(bytes)
        .expect("a well-formed frame must decode")
        .expect("a complete frame must be available");
    assert_eq!(consumed, bytes.len(), "the whole frame must be consumed");
    frame
}

fn framed(frame_type: u16, body: &[u8]) -> Vec<u8> {
    let len = u32::try_from(2 + body.len()).unwrap();
    let mut out = len.to_le_bytes().to_vec();
    out.extend_from_slice(&frame_type.to_le_bytes());
    out.extend_from_slice(body);
    out
}

#[test]
fn every_frame_survives_an_encode_decode_round_trip() {
    let frames = [
        HolderFrame::Hello {
            protocol: 3,
            daemon_boot: "boot-1".to_owned(),
        },
        HolderFrame::Replay,
        HolderFrame::Checkpoint {
            seq: 10,
            bytes: b"\x1b[2J\x00\xff".to_vec(),
        },
        HolderFrame::Output {
            seq: u64::MAX,
            bytes: vec![0, 1, 2, 0xff],
        },
        HolderFrame::Ack { seq: 42 },
        HolderFrame::StoreCheckpoint {
            seq: 7,
            bytes: Vec::new(),
        },
        HolderFrame::Write {
            id: 5,
            bytes: b"ab\r".to_vec(),
        },
        HolderFrame::Written { id: 5 },
        HolderFrame::WriteFailed {
            id: 6,
            reason: "child exited".to_owned(),
        },
        HolderFrame::Resize {
            cols: 120,
            rows: 40,
        },
        HolderFrame::Terminate { tree: true },
        HolderFrame::Exited { code: Some(-1) },
        HolderFrame::Exited { code: None },
        HolderFrame::Displaced,
        HolderFrame::VersionRejected { protocol: 2 },
    ];
    for frame in frames {
        let mut stream = encode_holder_frame(&frame);
        let first_len = stream.len();
        // A second frame behind the first must be left for the next call.
        stream.extend_from_slice(&encode_holder_frame(&HolderFrame::Ack { seq: 1 }));
        let (decoded, consumed) = decode_holder_frame(&stream)
            .expect("decode")
            .expect("complete frame");
        assert_eq!(decoded, frame);
        assert_eq!(consumed, first_len, "{frame:?} consumed past its own end");
    }
}

#[test]
fn a_byte_frame_carries_its_sequence_and_raw_bytes_outside_json() {
    let bytes = vec![0x00, 0xff, b'"', b'\\', 0x1b];
    let encoded = encode_holder_frame(&HolderFrame::Output {
        seq: 0x0102_0304_0506_0708,
        bytes: bytes.clone(),
    });
    let declared = u32::from_le_bytes(encoded[0..4].try_into().unwrap());
    assert_eq!(declared as usize, encoded.len() - 4);
    assert_eq!(&encoded[6..14], &0x0102_0304_0506_0708_u64.to_le_bytes());
    assert_eq!(&encoded[14..], bytes.as_slice());
}

/// The fixed frames keep their type numbers and body shapes in every version,
/// so a holder and a daemon of any two versions can still close and learn of
/// an exit.
#[test]
fn the_fixed_frames_have_pinned_type_numbers_and_bodies() {
    let pinned: [(HolderFrame, u16, &[u8]); 5] = [
        (
            HolderFrame::Hello {
                protocol: 7,
                daemon_boot: "b1".to_owned(),
            },
            1,
            br#"{"protocol":7,"daemon_boot":"b1"}"#,
        ),
        (
            HolderFrame::Terminate { tree: true },
            2,
            br#"{"tree":true}"#,
        ),
        (HolderFrame::Exited { code: Some(7) }, 3, br#"{"code":7}"#),
        (HolderFrame::Displaced, 4, b"{}"),
        (
            HolderFrame::VersionRejected { protocol: 9 },
            5,
            br#"{"protocol":9}"#,
        ),
    ];
    for (frame, frame_type, body) in pinned {
        let expected = framed(frame_type, body);
        assert_eq!(encode_holder_frame(&frame), expected, "{frame:?}");
        assert_eq!(decode_whole(&expected), frame);
    }

    // A later version may add fields to a JSON body; an older reader keeps
    // decoding the fields it knows.
    let newer_hello = framed(1, br#"{"protocol":8,"daemon_boot":"b2","added_later":[1]}"#);
    assert_eq!(
        decode_whole(&newer_hello),
        HolderFrame::Hello {
            protocol: 8,
            daemon_boot: "b2".to_owned(),
        }
    );
}

#[test]
fn incomplete_input_waits_and_broken_frames_are_errors() {
    let whole = encode_holder_frame(&HolderFrame::Written { id: 3 });
    for cut in 0..whole.len() {
        assert!(
            decode_holder_frame(&whole[..cut])
                .expect("a prefix is not an error")
                .is_none(),
            "a {cut}-byte prefix must wait for more input"
        );
    }

    // A byte frame whose body is too short to hold its sequence.
    let encoded = encode_holder_frame(&HolderFrame::Output {
        seq: 1,
        bytes: Vec::new(),
    });
    let output_type = u16::from_le_bytes(encoded[4..6].try_into().unwrap());
    assert!(decode_holder_frame(&framed(output_type, &[1, 2, 3])).is_err());

    // A JSON body that is not JSON, and one missing a required field.
    assert!(decode_holder_frame(&framed(1, b"{not json")).is_err());
    assert!(decode_holder_frame(&framed(1, br#"{"protocol":1}"#)).is_err());

    // A length too short to hold the type.
    assert!(decode_holder_frame(&[1, 0, 0, 0, 1]).is_err());
}

#[test]
fn the_frames_and_launch_fields_the_daemon_link_added_round_trip() {
    use std::path::PathBuf;
    use yagura_protocol::holder::{
        HolderLaunch, HolderSpawnFailure, decode_holder_launch, encode_holder_launch,
    };

    let frames = [
        HolderFrame::Resized {
            cols: 100,
            rows: 30,
        },
        HolderFrame::ResizeFailed {
            cols: 1001,
            rows: 30,
            reason: "out of range".to_owned(),
        },
        HolderFrame::SpawnFailed {
            reason: HolderSpawnFailure::BundledConptyUnavailable,
        },
        HolderFrame::SpawnFailed {
            reason: HolderSpawnFailure::Spawn,
        },
    ];
    for frame in frames {
        assert_eq!(decode_whole(&encode_holder_frame(&frame)), frame);
    }

    let launch = HolderLaunch {
        argv: vec!["cli".to_owned()],
        cwd: None,
        env: Vec::new(),
        cols: 80,
        rows: 24,
        record: "record".to_owned(),
        hello_deadline_ms: 1000,
        holders_dir: PathBuf::from("state").join("holders"),
        daemon_endpoint: "daemon-endpoint".to_owned(),
        end_child_on_disconnect: true,
    };
    assert_eq!(
        decode_holder_launch(&encode_holder_launch(&launch)).unwrap(),
        launch
    );
}
