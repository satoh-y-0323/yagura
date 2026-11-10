#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The relay's frame reassembly: the reader that splits the daemon's byte
//! stream into the relay frames it carries.

use yagura_mcp::relay::FrameReader;
use yagura_protocol::{FrameError, MAX_FRAME_BODY_LEN, frame_encode};

// =====================================================================
// FrameReader -- auxiliary seam unit tests
// =====================================================================

#[test]
fn frame_reader_returns_both_bodies_in_order_when_two_frames_arrive_together() {
    let mut reader = FrameReader::new();
    let mut bytes = frame_encode(b"first");
    bytes.extend_from_slice(&frame_encode(b"second"));

    let bodies = reader.push(&bytes).expect("well-formed frames must decode");

    assert_eq!(bodies, vec![b"first".to_vec(), b"second".to_vec()]);
    assert!(!reader.has_remaining());
}

#[test]
fn frame_reader_returns_a_body_only_once_its_last_byte_has_arrived() {
    let mut reader = FrameReader::new();
    let frame = frame_encode(b"drip");
    let (last, head) = frame.split_last().expect("an encoded frame is never empty");

    for byte in head {
        let bodies = reader
            .push(std::slice::from_ref(byte))
            .expect("a partial frame is not an error");
        assert!(
            bodies.is_empty(),
            "an incomplete frame must not yield a body"
        );
    }

    let bodies = reader
        .push(std::slice::from_ref(last))
        .expect("the completed frame must decode");

    assert_eq!(bodies, vec![b"drip".to_vec()]);
    assert!(!reader.has_remaining());
}

#[test]
fn frame_reader_assembles_a_frame_whose_length_prefix_arrived_split() {
    let mut reader = FrameReader::new();
    let frame = frame_encode(b"split-prefix");

    // Two bytes of the 4-byte little-endian length prefix, then the rest:
    // the reader must not mistake a partial prefix for a zero-length frame.
    let bodies = reader
        .push(&frame[..2])
        .expect("half a length prefix is not an error");
    assert!(bodies.is_empty());
    assert!(reader.has_remaining());

    let bodies = reader
        .push(&frame[2..])
        .expect("the completed frame must decode");

    assert_eq!(bodies, vec![b"split-prefix".to_vec()]);
}

#[test]
fn frame_reader_rejects_a_declared_length_above_the_frame_size_limit() {
    let mut reader = FrameReader::new();
    let declared = (MAX_FRAME_BODY_LEN + 1) as u32;

    let err = reader
        .push(&declared.to_le_bytes())
        .expect_err("an oversized declared length must fail loud");

    assert_eq!(err, FrameError::TooLarge);
}

#[test]
fn frame_reader_reports_remaining_bytes_only_while_a_frame_is_incomplete() {
    let mut reader = FrameReader::new();
    assert!(!reader.has_remaining(), "a fresh reader holds nothing");

    let complete = frame_encode(b"done");
    let trailing = frame_encode(b"still-arriving");
    let mut bytes = complete;
    bytes.extend_from_slice(&trailing[..3]);

    let bodies = reader.push(&bytes).expect("the first frame must decode");

    assert_eq!(bodies, vec![b"done".to_vec()]);
    assert!(
        reader.has_remaining(),
        "the leftover bytes of the next frame must be visible to the caller"
    );

    let bodies = reader
        .push(&trailing[3..])
        .expect("the second frame must decode");

    assert_eq!(bodies, vec![b"still-arriving".to_vec()]);
    assert!(!reader.has_remaining());
}
