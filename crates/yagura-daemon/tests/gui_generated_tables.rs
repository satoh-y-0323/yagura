#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Tables the GUI reads that only the daemon can decide: what a key chord writes to a
//! window (`input_encode`) and how many cells a character takes (`unicode-width`, the
//! measure the window's model uses). The GUI keeps no copy of either rule; it reads the
//! JSON files written here.
//!
//! The `regenerate_*` tests write the files and are ignored by default:
//! `cargo test -p yagura-daemon --test gui_generated_tables -- --ignored`.
//! The other tests fail while a file differs from what the daemon computes today.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::PathBuf;

use unicode_width::UnicodeWidthChar;
use yagura_daemon::input_encode::{InputModes, ViewPage, encode_key, view_page};
use yagura_protocol::{Key, KeyInput, Modifiers, NamedKey};

const REGENERATE: &str = "cargo test -p yagura-daemon --test gui_generated_tables -- --ignored";

fn gui_file(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../gui/src")
        .join(relative)
}

fn key_table_path() -> PathBuf {
    gui_file("keymap/terminal-key-bytes.generated.json")
}

fn width_table_path() -> PathBuf {
    gui_file("terminal/char-widths.generated.json")
}

// --- what a chord writes ------------------------------------------------------------

const NAMED_KEYS: &[(&str, NamedKey)] = &[
    ("Enter", NamedKey::Enter),
    ("Tab", NamedKey::Tab),
    ("Backspace", NamedKey::Backspace),
    ("Escape", NamedKey::Escape),
    ("ArrowUp", NamedKey::ArrowUp),
    ("ArrowDown", NamedKey::ArrowDown),
    ("ArrowLeft", NamedKey::ArrowLeft),
    ("ArrowRight", NamedKey::ArrowRight),
    ("Home", NamedKey::Home),
    ("End", NamedKey::End),
    ("PageUp", NamedKey::PageUp),
    ("PageDown", NamedKey::PageDown),
    ("Insert", NamedKey::Insert),
    ("Delete", NamedKey::Delete),
    ("F1", NamedKey::F1),
    ("F2", NamedKey::F2),
    ("F3", NamedKey::F3),
    ("F4", NamedKey::F4),
    ("F5", NamedKey::F5),
    ("F6", NamedKey::F6),
    ("F7", NamedKey::F7),
    ("F8", NamedKey::F8),
    ("F9", NamedKey::F9),
    ("F10", NamedKey::F10),
    ("F11", NamedKey::F11),
    ("F12", NamedKey::F12),
];

/// The characters a chord can name besides letters: digits and the US layout's symbols,
/// each in its unshifted and shifted form. `+` is spelled `Plus` and a space `Space`.
const SYMBOLS: &str = "0123456789)!@#$%^&*(;:=+,<-_.>/?`~[{\\|]}'\"";

fn chord_name(mods: Modifiers, key: &str) -> String {
    let mut name = String::new();
    if mods.ctrl {
        name.push_str("Ctrl+");
    }
    if mods.alt {
        name.push_str("Alt+");
    }
    if mods.shift {
        name.push_str("Shift+");
    }
    name.push_str(key);
    name
}

/// What the daemon does with the chord: the bytes it writes (hex), or a move of the view.
fn outcome_of(input: &KeyInput) -> String {
    match view_page(input) {
        Some(ViewPage::Up) => "view:up".to_string(),
        Some(ViewPage::Down) => "view:down".to_string(),
        None => encode_key(input, InputModes::default())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
    }
}

fn key_table() -> BTreeMap<String, String> {
    let mut table = BTreeMap::new();
    for ctrl in [false, true] {
        for alt in [false, true] {
            for shift in [false, true] {
                let mods = Modifiers { shift, alt, ctrl };
                for (name, named) in NAMED_KEYS {
                    let input = KeyInput {
                        key: Key::Named(*named),
                        mods,
                    };
                    table.insert(chord_name(mods, name), outcome_of(&input));
                }
                let mut characters: Vec<(String, char)> = ('a'..='z')
                    .map(|letter| {
                        let typed = if shift {
                            letter.to_ascii_uppercase()
                        } else {
                            letter
                        };
                        (letter.to_string(), typed)
                    })
                    .collect();
                characters.extend(SYMBOLS.chars().map(|c| {
                    let name = if c == '+' {
                        "Plus".to_string()
                    } else {
                        c.to_string()
                    };
                    (name, c)
                }));
                characters.push(("Space".to_string(), ' '));
                for (name, typed) in characters {
                    let input = KeyInput {
                        key: Key::Char(typed),
                        mods,
                    };
                    table.insert(chord_name(mods, &name), outcome_of(&input));
                }
            }
        }
    }
    table
}

fn render_key_table() -> String {
    let mut out = String::new();
    writeln!(out, "{{").unwrap();
    writeln!(out, "  \"generated_by\": \"{REGENERATE}\",").unwrap();
    writeln!(out, "  \"chords\": {{").unwrap();
    let table = key_table();
    for (index, (chord, outcome)) in table.iter().enumerate() {
        let comma = if index + 1 == table.len() { "" } else { "," };
        let chord = serde_json::to_string(chord).unwrap();
        writeln!(out, "    {chord}: \"{outcome}\"{comma}").unwrap();
    }
    writeln!(out, "  }}").unwrap();
    writeln!(out, "}}").unwrap();
    out
}

// --- how wide a character is --------------------------------------------------------

/// Inclusive code point ranges whose characters have `width` cells; a character with no
/// width of its own (a control) counts as zero.
fn ranges_of_width(width: usize) -> Vec<(u32, u32)> {
    let mut ranges: Vec<(u32, u32)> = Vec::new();
    for code in 0..=0x10FFFFu32 {
        let Some(c) = char::from_u32(code) else {
            continue;
        };
        if c.width().unwrap_or(0) != width {
            continue;
        }
        match ranges.last_mut() {
            Some((_, end)) if *end + 1 == code => *end = code,
            _ => ranges.push((code, code)),
        }
    }
    ranges
}

fn render_ranges(out: &mut String, name: &str, ranges: &[(u32, u32)], last: bool) {
    writeln!(out, "  \"{name}\": [").unwrap();
    for (index, (start, end)) in ranges.iter().enumerate() {
        let comma = if index + 1 == ranges.len() { "" } else { "," };
        writeln!(out, "    [{start}, {end}]{comma}").unwrap();
    }
    writeln!(out, "  ]{}", if last { "" } else { "," }).unwrap();
}

fn render_width_table() -> String {
    let (major, minor, patch) = unicode_width::UNICODE_VERSION;
    let mut out = String::new();
    writeln!(out, "{{").unwrap();
    writeln!(out, "  \"generated_by\": \"{REGENERATE}\",").unwrap();
    writeln!(out, "  \"unicode_version\": \"{major}.{minor}.{patch}\",").unwrap();
    render_ranges(&mut out, "zero", &ranges_of_width(0), false);
    render_ranges(&mut out, "wide", &ranges_of_width(2), true);
    writeln!(out, "}}").unwrap();
    out
}

// --- writing and checking -----------------------------------------------------------

fn assert_current(path: &PathBuf, expected: &str, what: &str) {
    let Ok(found) = std::fs::read_to_string(path) else {
        panic!(
            "{what} is missing at {}; generate it with `{REGENERATE}`",
            path.display()
        );
    };
    // A checkout may have turned the line endings into CRLF.
    let found = found.replace("\r\n", "\n");
    assert!(
        found == expected,
        "{what} at {} differs from what the daemon computes today; regenerate it with \
         `{REGENERATE}`",
        path.display()
    );
}

#[test]
#[ignore = "writes gui/src/keymap/terminal-key-bytes.generated.json"]
fn regenerate_terminal_key_table() {
    std::fs::write(key_table_path(), render_key_table()).unwrap();
}

#[test]
#[ignore = "writes gui/src/terminal/char-widths.generated.json"]
fn regenerate_char_width_table() {
    std::fs::write(width_table_path(), render_width_table()).unwrap();
}

#[test]
fn the_key_table_is_what_the_encoder_writes_today() {
    assert_current(&key_table_path(), &render_key_table(), "the key table");
}

#[test]
fn the_char_width_table_is_what_unicode_width_says_today() {
    assert_current(
        &width_table_path(),
        &render_width_table(),
        "the character width table",
    );
}

#[test]
fn the_key_table_holds_the_chords_the_gui_rules_turn_on() {
    let table = key_table();
    let outcome = |chord: &str| table.get(chord).map(String::as_str);
    assert_eq!(outcome("Enter"), Some("0d"));
    assert_eq!(outcome("Shift+Enter"), Some("1b0d"));
    assert_eq!(outcome("Ctrl+Shift+Enter"), Some("0d"));
    assert_eq!(outcome("Ctrl+ArrowUp"), Some("1b5b313b3541"));
    assert_eq!(outcome("Shift+PageUp"), Some("view:up"));
    assert_eq!(outcome("Ctrl+a"), Some("01"));
    assert_eq!(outcome("Ctrl+Shift+a"), Some(""));
    assert_eq!(outcome("Alt+Plus"), Some("1b2b"));
}

fn covers(ranges: &[(u32, u32)], code: u32) -> bool {
    ranges
        .iter()
        .any(|&(start, end)| start <= code && code <= end)
}

#[test]
fn the_width_table_splits_zero_one_and_two_cell_characters() {
    let zero = ranges_of_width(0);
    let wide = ranges_of_width(2);
    assert!(covers(&zero, 0x200D));
    assert!(covers(&wide, 0x1FAE8));
    assert!(!covers(&zero, 0x41) && !covers(&wide, 0x41));
}
