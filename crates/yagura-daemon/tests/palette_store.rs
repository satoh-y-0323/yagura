#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Contracts for `yagura_daemon::palette_store::PaletteStore`: the default
//! palettes the GUI last sent are kept in `yagura.db`, so a daemon started
//! again answers color queries with them before any GUI is attached.

mod common;

use std::collections::BTreeMap;

use yagura_daemon::palette_store::PaletteStore;
use yagura_protocol::{DefaultPalettes, Palette};

fn palette(seed: u8) -> Palette {
    Palette {
        foreground: [seed, seed.wrapping_add(1), seed.wrapping_add(2)],
        background: [seed.wrapping_add(3), seed.wrapping_add(4), 0x20],
        cursor: [0x12, seed, 0x56],
        ansi: (0..16u8).map(|i| [i * 16, seed, 255 - i]).collect(),
    }
}

fn defaults(app: u8, vendors: &[(&str, u8)]) -> DefaultPalettes {
    DefaultPalettes {
        app: palette(app),
        vendors: vendors
            .iter()
            .map(|(vendor, seed)| ((*vendor).to_string(), palette(*seed)))
            .collect::<BTreeMap<_, _>>(),
    }
}

#[test]
fn a_database_no_default_palettes_were_saved_to_reads_back_none() {
    let db = common::unique_temp_dir("palette-store-empty").join("yagura.db");
    let store = PaletteStore::open(&db).expect("open a fresh store");

    assert_eq!(
        store.load().expect("load from a fresh store"),
        None,
        "a store nothing was saved to must have no default palettes"
    );
}

#[test]
fn the_last_default_palettes_saved_are_read_back_by_a_store_opened_again() {
    let db = common::unique_temp_dir("palette-store-reopen").join("yagura.db");
    let first = defaults(0x10, &[("vendor-a", 0x40), ("vendor-b", 0x70)]);
    let last = defaults(0xa0, &[("vendor-b", 0xc0)]);
    {
        let mut store = PaletteStore::open(&db).expect("open the store");
        store.save(&first).expect("save the first default palettes");
        store.save(&last).expect("save the last default palettes");
    }

    let reopened = PaletteStore::open(&db).expect("open the store again");

    assert_eq!(
        reopened.load().expect("load from the reopened store"),
        Some(last),
        "a store opened again must read back the last default palettes saved, \
         replaced whole rather than merged with the earlier ones"
    );
}
