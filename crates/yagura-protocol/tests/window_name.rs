#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration tests for the window display-label validation contract: the
//! `name` and the `vendor` badge, which share one rule except that only a name
//! may not hold `"` (it ends a quoted chat mention).
//!
//! `validate_window_name` is the single source of truth for what a window's
//! display name may contain. The daemon collapses all rejection reasons into
//! one wire-level `invalid_argument` error, so the GUI must be able to
//! distinguish the reasons *before* sending, which is why the error type is
//! an enum rather than a unit type.

use std::path::Path;

use serde_json::{Value, json};
use yagura_protocol::{
    ChatBodyError, HANDOFF_NOTE_MAX_BYTES, WINDOW_NAME_MAX_BYTES, WindowNameError,
    decode_client_message, decode_daemon_message, encode_client_message, encode_daemon_message,
    validate_chat_body, validate_display_label, validate_handoff_text, validate_message_text,
    validate_task_instruction, validate_window_name, validate_window_vendor,
};

/// Invisible characters that carry no glyph-shaping role and are therefore
/// rejected: two labels differing only by one of these render identically.
const INVISIBLE_FORBIDDEN: [char; 6] = [
    '\u{061C}', // ARABIC LETTER MARK
    '\u{200B}', // ZERO WIDTH SPACE
    '\u{200E}', // LEFT-TO-RIGHT MARK
    '\u{200F}', // RIGHT-TO-LEFT MARK
    '\u{2060}', // WORD JOINER
    '\u{FEFF}', // ZERO WIDTH NO-BREAK SPACE (BOM)
];

/// The two invisible characters that stay allowed because they select which
/// glyph is rendered (Persian / Urdu / Indic orthography, emoji sequences).
const JOINERS_ALLOWED: [char; 2] = [
    '\u{200C}', // ZERO WIDTH NON-JOINER
    '\u{200D}', // ZERO WIDTH JOINER
];

/// Characters that must be rejected as `ForbiddenChar`, grouped by category:
/// C0 controls, DEL, C1 controls, newlines, line/paragraph separators, and
/// bidirectional control characters.
fn forbidden_chars() -> Vec<char> {
    let mut chars = Vec::new();

    // C0 controls: U+0000-U+001F
    for cp in 0x0000u32..=0x001F {
        chars.push(char::from_u32(cp).expect("valid C0 control code point"));
    }

    // DEL: U+007F
    chars.push('\u{007F}');

    // C1 controls: U+0080-U+009F
    for cp in 0x0080u32..=0x009F {
        chars.push(char::from_u32(cp).expect("valid C1 control code point"));
    }

    // Newlines: LF and CR (already covered by the C0 range above, kept
    // explicit so the intent stays visible).
    chars.push('\n');
    chars.push('\r');

    // Line/paragraph separators: U+2028, U+2029
    chars.push('\u{2028}');
    chars.push('\u{2029}');

    // Bidirectional control characters: U+202A-U+202E, U+2066-U+2069
    for cp in 0x202Au32..=0x202E {
        chars.push(char::from_u32(cp).expect("valid bidi control code point"));
    }
    for cp in 0x2066u32..=0x2069 {
        chars.push(char::from_u32(cp).expect("valid bidi isolate code point"));
    }

    // Zero-width / invisible characters: the weaker half of the same
    // display-spoofing threat class as the bidi controls above.
    chars.extend_from_slice(&INVISIBLE_FORBIDDEN);

    chars
}

#[test]
fn empty_name_is_rejected_as_empty() {
    let result = validate_window_name("");

    assert!(
        matches!(result, Err(WindowNameError::Empty)),
        "expected Err(WindowNameError::Empty), got {result:?}"
    );
}

#[test]
fn name_at_exact_max_bytes_is_accepted() {
    // Build an ASCII string of exactly WINDOW_NAME_MAX_BYTES bytes so the
    // byte length and char count coincide, keeping the boundary exact.
    let name: String = "a".repeat(WINDOW_NAME_MAX_BYTES);
    assert_eq!(name.len(), WINDOW_NAME_MAX_BYTES);

    let result = validate_window_name(&name);

    assert!(
        result.is_ok(),
        "expected Ok(()) for a name of exactly WINDOW_NAME_MAX_BYTES bytes, got {result:?}"
    );
}

#[test]
fn name_one_byte_over_max_is_rejected_as_too_long() {
    let name: String = "a".repeat(WINDOW_NAME_MAX_BYTES + 1);
    assert_eq!(name.len(), WINDOW_NAME_MAX_BYTES + 1);

    let result = validate_window_name(&name);

    match result {
        Err(WindowNameError::TooLong { max, actual }) => {
            assert_eq!(max, WINDOW_NAME_MAX_BYTES);
            assert_eq!(actual, WINDOW_NAME_MAX_BYTES + 1);
        }
        other => panic!(
            "expected Err(WindowNameError::TooLong {{ max: {WINDOW_NAME_MAX_BYTES}, actual: {} }}), got {other:?}",
            WINDOW_NAME_MAX_BYTES + 1
        ),
    }
}

#[test]
fn each_forbidden_char_is_rejected_individually() {
    for ch in forbidden_chars() {
        // Wrap the forbidden char in an otherwise-ordinary name so the
        // reported byte offset `at` is meaningful (not just 0).
        let name = format!("a{ch}b");
        let result = validate_window_name(&name);

        match result {
            Err(WindowNameError::ForbiddenChar { at, ch: reported }) => {
                assert_eq!(
                    reported, ch,
                    "reported forbidden char mismatch for U+{:04X}",
                    ch as u32
                );
                assert_eq!(
                    at, 1,
                    "reported byte offset mismatch for U+{:04X} (name = {name:?})",
                    ch as u32
                );
            }
            other => panic!(
                "expected Err(WindowNameError::ForbiddenChar {{ .. }}) for U+{:04X}, got {other:?}",
                ch as u32
            ),
        }
    }
}

#[test]
fn ordinary_japanese_and_ascii_name_is_accepted() {
    let result = validate_window_name("claude-1 セッション A");

    assert!(
        result.is_ok(),
        "expected Ok(()) for an ordinary human-readable name, got {result:?}"
    );
}

#[test]
fn ordinary_ascii_name_with_spaces_is_accepted() {
    let result = validate_window_name("my window 42");

    assert!(
        result.is_ok(),
        "expected Ok(()) for an ordinary ASCII name with spaces, got {result:?}"
    );
}

#[test]
fn each_invisible_character_is_rejected_individually() {
    // Asserted separately from `forbidden_chars` so that the zero-width set
    // is pinned by name: a name differing from another only by one of these
    // is indistinguishable on screen.
    for ch in INVISIBLE_FORBIDDEN {
        let name = format!("a{ch}b");
        let result = validate_window_name(&name);

        match result {
            Err(WindowNameError::ForbiddenChar { at, ch: reported }) => {
                assert_eq!(
                    reported, ch,
                    "reported forbidden char mismatch for U+{:04X}",
                    ch as u32
                );
                assert_eq!(
                    at, 1,
                    "reported byte offset mismatch for U+{:04X}",
                    ch as u32
                );
            }
            other => panic!(
                "expected Err(WindowNameError::ForbiddenChar {{ .. }}) for U+{:04X}, got {other:?}",
                ch as u32
            ),
        }
    }
}

#[test]
fn the_two_glyph_shaping_joiners_stay_accepted() {
    // The counterpart of the test above: ZWNJ and ZWJ are just as invisible
    // but change which glyph is rendered, so forbidding them would reject
    // correctly-spelled Persian, Urdu and Indic labels and emoji sequences.
    for ch in JOINERS_ALLOWED {
        let name = format!("a{ch}b");
        let result = validate_window_name(&name);

        assert!(
            result.is_ok(),
            "expected Ok(()) for U+{:04X}, got {result:?}",
            ch as u32
        );
    }
}

#[test]
fn vendor_is_rejected_for_every_character_a_name_is_rejected_for() {
    // The vendor badge renders beside the name, so it must not be the softer
    // way in for a value the name rule turns away.
    for ch in forbidden_chars() {
        let vendor = format!("a{ch}b");
        let result = validate_window_vendor(&vendor);

        assert!(
            matches!(result, Err(WindowNameError::ForbiddenChar { .. })),
            "expected Err(WindowNameError::ForbiddenChar {{ .. }}) for U+{:04X}, got {result:?}",
            ch as u32
        );
    }
}

#[test]
fn empty_vendor_is_rejected_as_empty() {
    let result = validate_window_vendor("");

    assert!(
        matches!(result, Err(WindowNameError::Empty)),
        "expected Err(WindowNameError::Empty), got {result:?}"
    );
}

#[test]
fn vendor_over_max_bytes_is_rejected_as_too_long() {
    let vendor: String = "a".repeat(WINDOW_NAME_MAX_BYTES + 1);

    let result = validate_window_vendor(&vendor);

    assert!(
        matches!(result, Err(WindowNameError::TooLong { .. })),
        "expected Err(WindowNameError::TooLong {{ .. }}), got {result:?}"
    );
}

#[test]
fn ordinary_vendor_identifier_is_accepted() {
    for vendor in ["claude-code", "codex", "ベンダー A"] {
        let result = validate_window_vendor(vendor);

        assert!(
            result.is_ok(),
            "expected Ok(()) for vendor {vendor:?}, got {result:?}"
        );
    }
}

// --- `"` ends a quoted mention, so a name cannot hold one ---------------------------

#[test]
fn a_double_quote_is_rejected_in_a_window_name() {
    let result = validate_window_name("Say \"hi\"");

    assert_eq!(
        result,
        Err(WindowNameError::ForbiddenChar { at: 4, ch: '"' }),
        "a name holding `\"` could not be written inside `@\"...\"`"
    );
}

#[test]
fn a_double_quote_stays_accepted_everywhere_a_window_name_is_not_written() {
    let quoted = "a \"quoted\" word";

    assert_eq!(validate_window_vendor(quoted), Ok(()));
    assert!(validate_handoff_text(quoted, HANDOFF_NOTE_MAX_BYTES).is_ok());
    assert!(validate_task_instruction(quoted).is_ok());
    assert!(validate_message_text(quoted).is_ok());
    assert!(validate_chat_body(quoted).is_ok());
}

// --- the wire of a name's handling and of the name a window was given ---------------

/// The wire key of an open request saying what to do when its name is taken.
const ON_NAME_TAKEN: &str = "on_name_taken";

fn client_round_trip(frame: &Value) -> Value {
    let body = serde_json::to_vec(frame).expect("a frame encodes");
    let decoded =
        decode_client_message(&body).unwrap_or_else(|err| panic!("{frame} must decode: {err:?}"));
    serde_json::from_slice(&encode_client_message(&decoded)).expect("an encoded frame is JSON")
}

fn daemon_round_trip(frame: &Value) -> Value {
    let body = serde_json::to_vec(frame).expect("a frame encodes");
    let decoded =
        decode_daemon_message(&body).unwrap_or_else(|err| panic!("{frame} must decode: {err:?}"));
    serde_json::from_slice(&encode_daemon_message(&decoded)).expect("an encoded frame is JSON")
}

fn open_requests() -> [Value; 2] {
    [
        json!({
            "type": "open_window",
            "command": ["pwsh"],
            "cwd": null,
            "name": "Twin",
            "vendor": null,
            "detection": null,
        }),
        json!({
            "type": "open_window_from_profile",
            "profile": "twin",
            "cwd": null,
            "name": "Twin",
        }),
    ]
}

#[test]
fn an_open_request_asking_to_number_a_taken_name_keeps_asking_across_the_wire() {
    for mut frame in open_requests() {
        frame[ON_NAME_TAKEN] = json!("number");

        let echoed = client_round_trip(&frame);

        assert_eq!(
            echoed[ON_NAME_TAKEN],
            json!("number"),
            "{frame} became {echoed}"
        );
    }
}

#[test]
fn an_open_request_that_omits_the_handling_or_says_refuse_never_asks_for_numbering() {
    for frame in open_requests() {
        let omitted = client_round_trip(&frame);
        let mut refusing = frame.clone();
        refusing[ON_NAME_TAKEN] = json!("refuse");
        let refused = client_round_trip(&refusing);

        for echoed in [omitted, refused] {
            assert!(
                matches!(echoed.get(ON_NAME_TAKEN), None | Some(Value::Null))
                    || echoed[ON_NAME_TAKEN] == json!("refuse"),
                "an omitted handling means refuse: {echoed}"
            );
        }
    }
}

fn opened_answers() -> [Value; 3] {
    [
        json!({ "type": "window_opened", "window_id": "w2-abc" }),
        json!({
            "type": "task_window_opened",
            "plan_id": "p-1",
            "task_id": "t1",
            "window_id": "w2-abc",
            "assignment": { "source": "unassigned", "assignment": null },
        }),
        json!({
            "type": "handoff_sent",
            "window_id": "w2-abc",
            "opened": true,
            "id": "e-1",
            "state": "delivered",
        }),
    ]
}

#[test]
fn an_answer_that_opened_a_window_carries_its_name_across_the_wire() {
    for mut frame in opened_answers() {
        frame["name"] = json!("Twin 2");

        let echoed = daemon_round_trip(&frame);

        assert_eq!(echoed["name"], json!("Twin 2"), "{frame} became {echoed}");
    }
}

#[test]
fn an_answer_from_a_daemon_that_predates_the_name_still_decodes_and_writes_no_name() {
    for frame in opened_answers() {
        let echoed = daemon_round_trip(&frame);

        assert!(
            echoed.get("name").is_none(),
            "an absent name is left off the wire, not written as null: {echoed}"
        );
    }
}

// --- a name must look like itself: no blank edges, at least one visible character ---

/// The message `validate_window_name` gives a name with no visible character.
const NO_VISIBLE_CHAR: &str = "window name has no visible character";

/// The message `validate_window_name` gives a name that starts or ends with `ch`.
fn surrounding_blank(ch: char, at: usize) -> String {
    format!(
        "window name must not start or end with a blank character (U+{:04X} at byte offset {at})",
        ch as u32
    )
}

/// The characters the GUI draws as a space inside a name while it writes them out
/// inside a place: the TS side of the same contract computes this list from the
/// GUI's own write-out functions, so the two sides read one definition.
fn gui_blank_looking_chars() -> Vec<char> {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/blank-looking-chars.json");
    let source = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("{} must be readable: {err}", path.display()));
    let entries: Vec<String> = serde_json::from_str(&source)
        .unwrap_or_else(|err| panic!("{} must be an array of strings: {err}", path.display()));
    entries
        .iter()
        .map(|entry| {
            let mut chars = entry.chars();
            match (chars.next(), chars.next()) {
                (Some(ch), None) => ch,
                _ => panic!(
                    "every entry of {} is one character: {entry:?}",
                    path.display()
                ),
            }
        })
        .collect()
}

fn message_of(result: Result<(), WindowNameError>) -> Option<String> {
    result.err().map(|err| err.to_string())
}

#[test]
fn every_blank_looking_character_is_refused_alone_or_at_an_edge_and_kept_inside_a_name() {
    let fixture = gui_blank_looking_chars();
    assert!(
        fixture.contains(&'\u{2800}'),
        "the GUI draws U+2800 as a space inside a name: {fixture:?}"
    );
    let mut blank_looking = fixture;
    blank_looking.push(' ');

    for ch in blank_looking {
        let code = ch as u32;
        assert_eq!(
            message_of(validate_window_name(&ch.to_string())),
            Some(NO_VISIBLE_CHAR.to_string()),
            "a name of only U+{code:04X} shows nothing"
        );
        assert_eq!(
            message_of(validate_window_name(&format!("{ch}reviewer"))),
            Some(surrounding_blank(ch, 0)),
            "U+{code:04X} before a name makes it look like the name without it"
        );
        assert_eq!(
            message_of(validate_window_name(&format!("reviewer{ch}"))),
            Some(surrounding_blank(ch, "reviewer".len())),
            "U+{code:04X} after a name makes it look like the name without it"
        );
        assert_eq!(
            validate_window_name(&format!("a{ch}b")),
            Ok(()),
            "U+{code:04X} between visible characters is part of the name"
        );
        assert_eq!(
            validate_chat_body(&ch.to_string()),
            Err(ChatBodyError::Empty),
            "a chat message of only U+{code:04X} carries nothing to read"
        );
    }
}

#[test]
fn a_name_of_blank_characters_and_joiners_only_has_no_visible_character() {
    for name in [
        " \u{200C} ",
        "\u{200C}",
        "\u{200D}",
        "\u{2800}\u{200D}\u{3000}",
    ] {
        assert_eq!(
            message_of(validate_window_name(name)),
            Some(NO_VISIBLE_CHAR.to_string()),
            "{name:?} shows nothing"
        );
    }
}

#[test]
fn a_blank_edge_stays_accepted_in_labels_that_are_not_window_names() {
    for label in [" x", "x ", "\u{2800}x", "x\u{3000}"] {
        assert_eq!(validate_window_vendor(label), Ok(()), "vendor {label:?}");
        assert_eq!(validate_display_label(label), Ok(()), "label {label:?}");
    }
}
