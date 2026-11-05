#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: when the daemon answers a holder's `Output` with an `Ack`.
//! The step is a quarter of the window the holder names in its `Welcome`, so
//! acks cost one per step instead of one per arrival, while the output the
//! holder has sent without an ack never reaches that window and the holder
//! never stops reading for want of one.

use yagura_protocol::holder::AckStep;

/// The window a holder of this version names: 256 KiB.
const NAMED_WINDOW: u64 = 256 * 1024;

/// Feeds outputs of `sizes` bytes, the first starting at position `start`,
/// and returns every ack as (index of the output it answered, seq it names).
fn acks(window: Option<u64>, start: u64, sizes: &[u64]) -> Vec<(usize, u64)> {
    let mut step = AckStep::new(window);
    let mut seq = start;
    let mut acks = Vec::new();
    for (index, size) in sizes.iter().enumerate() {
        seq += size;
        if let Some(acked) = step.on_output(seq) {
            acks.push((index, acked));
        }
    }
    acks
}

/// Output sizes from 1 to `max` bytes, the same for every run.
fn sizes(count: usize, max: u64, mut state: u64) -> Vec<u64> {
    (0..count)
        .map(|_| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (state >> 33) % max + 1
        })
        .collect()
}

#[test]
fn the_first_output_is_acked_whatever_the_window() {
    for window in [
        None,
        Some(0),
        Some(3),
        Some(4),
        Some(NAMED_WINDOW),
        Some(u64::MAX),
    ] {
        for start in [0, 1_000_000] {
            let acks = acks(window, start, &[1, 1]);
            assert_eq!(
                acks.first(),
                Some(&(0, start + 1)),
                "window {window:?} from {start}: the first output must be acked, got {acks:?}"
            );
        }
    }
}

/// No window, or one a quarter of which is zero, gives no step: every
/// output is acked.
#[test]
fn without_a_window_that_makes_a_step_every_output_is_acked() {
    let sizes = [5, 1, 300, 2, 70_000];
    for window in [None, Some(0), Some(3)] {
        assert_eq!(
            acks(window, 0, &sizes),
            vec![(0, 5), (1, 6), (2, 306), (3, 308), (4, 70_308)],
            "window {window:?}"
        );
    }
}

/// A window of 4 is the smallest with a step: one byte.
#[test]
fn a_window_of_four_acks_each_byte_taken_in() {
    assert_eq!(
        acks(Some(4), 10, &[1, 1, 2, 5]),
        vec![(0, 11), (1, 12), (2, 14), (3, 19)]
    );
}

#[test]
fn an_ack_comes_once_a_quarter_of_the_window_was_taken_in_since_the_last() {
    // Step 1024: 100 | 600 1100 1124 | 2124 3148 | 6148 | 6149.
    let sizes = [100, 500, 500, 24, 1000, 1024, 3000, 1];
    assert_eq!(
        acks(Some(4096), 0, &sizes),
        vec![(0, 100), (3, 1124), (5, 3148), (6, 6148)]
    );
    // The first output starts the count, wherever the stream stands.
    assert_eq!(
        acks(Some(4096), 1_000_000, &sizes),
        vec![
            (0, 1_000_100),
            (3, 1_001_124),
            (5, 1_003_148),
            (6, 1_006_148)
        ]
    );
}

#[test]
fn the_named_window_acks_every_64_kib_of_8_kib_outputs() {
    let block = 8 * 1024;
    let acks = acks(Some(NAMED_WINDOW), 0, &[block; 40]);
    assert_eq!(
        acks,
        vec![
            (0, block),
            (8, 9 * block),
            (16, 17 * block),
            (24, 25 * block),
            (32, 33 * block)
        ]
    );
}

/// For any outputs: an ack comes exactly when the output taken in since the
/// last ack reaches the step, names the position just taken in, leaves less
/// than the window unacked, and the acks number at most total / step + 1.
#[test]
fn unacked_output_never_reaches_the_window_and_acks_follow_the_step() {
    let windows = [4, 5, 7, 8, 4096, 65_537, NAMED_WINDOW];
    for window in windows {
        let step = window / 4;
        let runs = [
            sizes(2_000, 1, 1),
            sizes(2_000, step.max(2) - 1, 2),
            sizes(2_000, window * 2, 3),
            sizes(2_000, 8 * 1024, 4),
            vec![1; usize::try_from(NAMED_WINDOW * 2).unwrap()],
        ];
        for (run, sizes) in runs.iter().enumerate() {
            let mut ack_step = AckStep::new(Some(window));
            let mut seq = 0_u64;
            let mut last_ack: Option<u64> = None;
            let mut count = 0_u64;
            for (index, size) in sizes.iter().enumerate() {
                seq += size;
                let due = last_ack.is_none_or(|last| seq - last >= step);
                let answer = ack_step.on_output(seq);
                assert_eq!(
                    answer,
                    due.then_some(seq),
                    "window {window} run {run} output {index} at {seq}: last ack {last_ack:?}"
                );
                if answer.is_some() {
                    last_ack = Some(seq);
                    count += 1;
                }
                let unacked = seq - last_ack.unwrap();
                assert!(
                    unacked < window,
                    "window {window} run {run} output {index}: {unacked} bytes unacked"
                );
            }
            assert!(
                count <= seq / step + 1,
                "window {window} run {run}: {count} acks for {seq} bytes in steps of {step}"
            );
        }
    }
}
