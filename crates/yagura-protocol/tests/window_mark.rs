#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The window mark contract: the mark the GUI draws beside a window and on the
//! approval dialog is a function of the window ID alone, stable across versions
//! (a window taken back after a daemon update keeps its mark), and the daemon
//! hands out IDs by the distance and fill rules measured here.

use yagura_protocol::{WindowMark, window_mark, window_marks_apart};

/// Window IDs whose marks are pinned: FNV-1a 64 over the ID's UTF-8 bytes, mixed by
/// the splitmix64 finalizer; `cells` is the low 15 bits, `hue` the bits from 16 up
/// modulo 12.
const PINNED: [(&str, u16, u8); 5] = [
    ("w00000000", 12097, 6),
    ("wzzzzzzzz", 25519, 5),
    ("w1a2b3c4d", 13543, 6),
    ("wk3j9x0qp", 20283, 11),
    ("w5t8m2c7r", 6223, 0),
];

fn mark(cells: u16) -> WindowMark {
    WindowMark { cells, hue: 0 }
}

#[test]
fn a_window_id_always_draws_the_same_mark_within_fifteen_cells_and_twelve_hues() {
    for (window_id, cells, hue) in PINNED {
        let drawn = window_mark(window_id);
        assert_eq!(
            (drawn.cells, drawn.hue),
            (cells, hue),
            "the mark of {window_id} is part of the contract and may not change between versions"
        );
    }

    const ALPHABET: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    for index in 0..4096usize {
        let digits: String = (0..8)
            .map(|place| char::from(ALPHABET[(index * 7919 + place * 104_729) % ALPHABET.len()]))
            .collect();
        let window_id = format!("w{digits}");
        let drawn = window_mark(&window_id);
        assert!(drawn.cells < 1 << 15, "{window_id}: cells {}", drawn.cells);
        assert!(drawn.hue < 12, "{window_id}: hue {}", drawn.hue);
    }
}

#[test]
fn marks_are_apart_only_from_the_given_number_of_differing_cells() {
    let base = mark(0b000_0000_0000_0000);
    let two_off = mark(0b000_0000_0000_0011);
    let three_off = mark(0b100_0000_0000_0011);

    assert!(!window_marks_apart(base, two_off, 3), "two cells differ");
    assert!(window_marks_apart(base, three_off, 3), "three cells differ");
    assert!(window_marks_apart(base, two_off, 1), "two cells differ");

    let recoloured = WindowMark {
        cells: base.cells,
        hue: 7,
    };
    assert!(
        !window_marks_apart(base, recoloured, 1),
        "a mark told apart by colour alone is not apart: the shape carries the guarantee"
    );
}

#[test]
fn a_mark_is_drawable_only_with_four_to_eleven_cells_filled() {
    for (cells, drawable) in [
        (0b000_0000_0000_0111, false),
        (0b000_0000_0000_1111, true),
        (0b000_0111_1111_1111, true),
        (0b000_1111_1111_1111, false),
    ] {
        assert_eq!(
            mark(cells).is_drawable(),
            drawable,
            "{} cells filled",
            u16::count_ones(cells)
        );
    }
}
