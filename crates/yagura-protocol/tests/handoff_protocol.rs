#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration tests for the `send_handoff` protocol contract: the
//! `ClientMessage::SendHandoff` / `DaemonMessage::HandoffSent` wire messages,
//! the argument-exclusivity and text-content validators every caller shares
//! rather than restating, and the four byte-limit constants those validators
//! are bounded by.

use yagura_protocol::{
    APPROVAL_TEXT_MAX_BYTES, ClientMessage, DaemonMessage, HANDOFF_ENVELOPE_MAX_BYTES,
    HANDOFF_NOTE_MAX_BYTES, HANDOFF_REFERENCE_MAX_BYTES, HandoffArgError, HandoffDelivery,
    HandoffTextError, SendTextError, WindowNameError, decode_client_message, decode_daemon_message,
    encode_client_message, encode_daemon_message, is_valid_token, validate_handoff_target,
    validate_handoff_text, validate_send_text_body,
};

// =====================================================================
// Wire round trip: ClientMessage::SendHandoff / DaemonMessage::HandoffSent
// =====================================================================

#[test]
fn send_handoff_to_an_existing_window_decodes() {
    let body = br#"{"type":"send_handoff","window_id":"w1","profile":null,"cwd":null,"name":null,"reference":"src/foo.rs","note":"please review"}"#;
    let decoded = decode_client_message(body).expect("send_handoff to a window_id must decode");
    match decoded {
        ClientMessage::SendHandoff {
            window_id,
            profile,
            cwd,
            name,
            model,
            effort,
            reference,
            note,
        } => {
            assert_eq!(window_id.as_deref(), Some("w1"));
            assert_eq!(profile, None);
            assert_eq!(cwd, None);
            assert_eq!(name, None);
            assert_eq!(model, None);
            assert_eq!(effort, None);
            assert_eq!(reference, "src/foo.rs");
            assert_eq!(note, "please review");
        }
        other => panic!("expected ClientMessage::SendHandoff, got {other:?}"),
    }
}

#[test]
fn send_handoff_from_a_profile_decodes_with_cwd_and_name_overrides() {
    let body = br#"{"type":"send_handoff","profile":"codex-cli","cwd":"/work/repo","name":"reviewer","reference":"src/foo.rs","note":"take a look"}"#;
    let decoded = decode_client_message(body).expect("send_handoff from a profile must decode");
    match decoded {
        ClientMessage::SendHandoff {
            window_id,
            profile,
            cwd,
            name,
            ..
        } => {
            assert_eq!(window_id, None);
            assert_eq!(profile.as_deref(), Some("codex-cli"));
            assert_eq!(cwd.as_deref(), Some("/work/repo"));
            assert_eq!(name.as_deref(), Some("reviewer"));
        }
        other => panic!("expected ClientMessage::SendHandoff, got {other:?}"),
    }
}

#[test]
fn send_handoff_omitting_the_optional_fields_decodes() {
    let body = br#"{"type":"send_handoff","window_id":"w1","reference":"r","note":"t"}"#;
    let decoded =
        decode_client_message(body).expect("send_handoff must decode without profile/cwd/name");
    match decoded {
        ClientMessage::SendHandoff {
            profile, cwd, name, ..
        } => {
            assert_eq!(profile, None);
            assert_eq!(cwd, None);
            assert_eq!(name, None);
        }
        other => panic!("expected ClientMessage::SendHandoff, got {other:?}"),
    }
}

#[test]
fn send_handoff_round_trips_through_encode_and_decode() {
    let body = br#"{"type":"send_handoff","window_id":"w1","reference":"r","note":"t"}"#;
    let decoded = decode_client_message(body).expect("must decode");
    let encoded = encode_client_message(&decoded);
    let decoded2 = decode_client_message(&encoded).expect("must decode a second time");
    assert_eq!(decoded, decoded2);
}

#[test]
fn handoff_sent_decodes_with_window_id_opened_and_delivery() {
    let body = br#"{"type":"handoff_sent","window_id":"w2","opened":true,"delivery":"confirmed"}"#;
    let decoded = decode_daemon_message(body).expect("handoff_sent must decode");
    match decoded {
        DaemonMessage::HandoffSent {
            window_id,
            opened,
            delivery,
        } => {
            assert_eq!(window_id, "w2");
            assert!(opened);
            assert_eq!(delivery, HandoffDelivery::Confirmed);
        }
        other => panic!("expected DaemonMessage::HandoffSent, got {other:?}"),
    }
}

#[test]
fn handoff_sent_round_trips_through_encode_and_decode() {
    let body =
        br#"{"type":"handoff_sent","window_id":"w2","opened":false,"delivery":"not_observed"}"#;
    let decoded = decode_daemon_message(body).expect("must decode");
    let encoded = encode_daemon_message(&decoded);
    let decoded2 = decode_daemon_message(&encoded).expect("must decode a second time");
    assert_eq!(decoded, decoded2);
}

/// The wire round trip for every [`HandoffDelivery`] value carried inside a
/// real `handoff_sent` message, not just the bare enum (`handoff_delivery_*`
/// below covers that): the field's presence must not change how the other
/// two fields decode, in either direction.
#[test]
fn handoff_sent_round_trips_every_delivery_value_through_the_message() {
    for delivery in [
        HandoffDelivery::Confirmed,
        HandoffDelivery::Unconfirmed,
        HandoffDelivery::NotObserved,
        HandoffDelivery::NotWritten,
    ] {
        let message = DaemonMessage::HandoffSent {
            window_id: "w2".to_string(),
            opened: false,
            delivery,
        };
        let encoded = encode_daemon_message(&message);
        let decoded = decode_daemon_message(&encoded).expect("must decode");
        assert_eq!(decoded, message, "round trip mismatch for {delivery:?}");
    }
}

/// The daemon reports `not_written` when it never wrote the envelope into
/// the target's input at all, distinct from the three existing outcomes
/// (which all presuppose that the envelope was written).
#[test]
fn handoff_sent_decodes_with_delivery_not_written() {
    let body =
        br#"{"type":"handoff_sent","window_id":"w2","opened":false,"delivery":"not_written"}"#;
    let decoded = decode_daemon_message(body).expect("handoff_sent with not_written must decode");
    match decoded {
        DaemonMessage::HandoffSent {
            window_id,
            opened,
            delivery,
        } => {
            assert_eq!(window_id, "w2");
            assert!(!opened);
            assert_eq!(delivery, HandoffDelivery::NotWritten);
        }
        other => panic!("expected DaemonMessage::HandoffSent, got {other:?}"),
    }
}

/// `delivery` carries no `#[serde(default)]`: a response shaped like the
/// earlier wire contract, before this field existed (no `delivery` at all),
/// must not be silently read as some default value -- that would misreport
/// an unobserved delivery as whatever the default happens to be.
#[test]
fn handoff_sent_missing_delivery_is_a_decode_error() {
    let body = br#"{"type":"handoff_sent","window_id":"w2","opened":true}"#;
    let result = decode_daemon_message(body);
    assert!(
        result.is_err(),
        "handoff_sent without a delivery field must fail to decode, got {result:?}"
    );
}

#[test]
fn existing_message_types_still_decode_after_adding_send_handoff_and_handoff_sent() {
    // Adding a variant to each enum's #[serde(tag = "type")] wire format must
    // not disturb any existing message's shape.
    let ping = decode_client_message(br#"{"type":"ping"}"#).expect("ping must still decode");
    assert!(matches!(ping, ClientMessage::Ping));

    let pong = decode_daemon_message(br#"{"type":"pong"}"#).expect("pong must still decode");
    assert!(matches!(pong, DaemonMessage::Pong));

    let attach = decode_client_message(br#"{"type":"attach_view","window_id":"w1"}"#)
        .expect("attach decodes");
    assert!(matches!(attach, ClientMessage::AttachView { .. }));
}

// =====================================================================
// validate_handoff_target: the window_id / profile exclusivity rule
// =====================================================================

#[test]
fn validate_handoff_target_rejects_neither_window_id_nor_profile() {
    assert_eq!(
        validate_handoff_target(None, None, None, None, false),
        Err(HandoffArgError::NeitherProvided)
    );
}

#[test]
fn validate_handoff_target_rejects_both_window_id_and_profile() {
    assert_eq!(
        validate_handoff_target(Some("w1"), Some("codex-cli"), None, None, false),
        Err(HandoffArgError::BothProvided)
    );
}

#[test]
fn validate_handoff_target_rejects_a_cwd_override_alongside_window_id() {
    assert_eq!(
        validate_handoff_target(Some("w1"), None, Some("/work"), None, false),
        Err(HandoffArgError::WindowIdWithCwd)
    );
}

#[test]
fn validate_handoff_target_rejects_a_name_override_alongside_window_id() {
    assert_eq!(
        validate_handoff_target(Some("w1"), None, None, Some("reviewer"), false),
        Err(HandoffArgError::WindowIdWithName)
    );
}

#[test]
fn validate_handoff_target_accepts_window_id_alone() {
    assert_eq!(
        validate_handoff_target(Some("w1"), None, None, None, false),
        Ok(())
    );
}

#[test]
fn validate_handoff_target_accepts_profile_alone() {
    assert_eq!(
        validate_handoff_target(None, Some("codex-cli"), None, None, false),
        Ok(())
    );
}

#[test]
fn validate_handoff_target_accepts_profile_with_cwd_and_name() {
    assert_eq!(
        validate_handoff_target(
            None,
            Some("codex-cli"),
            Some("/work/repo"),
            Some("reviewer"),
            false
        ),
        Ok(())
    );
}

#[test]
fn validate_handoff_target_applies_is_valid_token_to_profile() {
    // "Codex CLI" is not a valid token spelling (uppercase letters, a space):
    // is_valid_token itself must be the arbiter, not a looser inline check
    // restated at this call site.
    assert!(!is_valid_token("Codex CLI"));
    let result = validate_handoff_target(None, Some("Codex CLI"), None, None, false);
    assert!(
        result.is_err(),
        "an invalid profile token must be rejected, got {result:?}"
    );
}

#[test]
fn validate_handoff_target_rejects_a_control_character_in_name() {
    let result = validate_handoff_target(None, Some("codex-cli"), None, Some("a\u{0007}b"), false);
    assert!(
        matches!(
            result,
            Err(HandoffArgError::Name(WindowNameError::ForbiddenChar { .. }))
        ),
        "a control character in name must be rejected through the shared \
         validate_window_name rule, got {result:?}"
    );
}

#[test]
fn validate_handoff_target_accepts_an_ordinary_name() {
    assert_eq!(
        validate_handoff_target(None, Some("codex-cli"), None, Some("reviewer 1"), false),
        Ok(())
    );
}

// =====================================================================
// validate_handoff_text: the shared reference / note content rule
// =====================================================================

#[test]
fn validate_handoff_text_rejects_an_empty_value() {
    assert_eq!(
        validate_handoff_text("", HANDOFF_NOTE_MAX_BYTES),
        Err(HandoffTextError::Empty)
    );
}

#[test]
fn validate_handoff_text_accepts_exactly_the_reference_byte_limit() {
    let value = "a".repeat(HANDOFF_REFERENCE_MAX_BYTES);
    assert_eq!(
        validate_handoff_text(&value, HANDOFF_REFERENCE_MAX_BYTES),
        Ok(())
    );
}

#[test]
fn validate_handoff_text_rejects_one_byte_over_the_reference_limit() {
    let value = "a".repeat(HANDOFF_REFERENCE_MAX_BYTES + 1);
    let result = validate_handoff_text(&value, HANDOFF_REFERENCE_MAX_BYTES);
    match result {
        Err(HandoffTextError::TooLong { max, actual }) => {
            assert_eq!(max, HANDOFF_REFERENCE_MAX_BYTES);
            assert_eq!(actual, HANDOFF_REFERENCE_MAX_BYTES + 1);
        }
        other => panic!("expected Err(HandoffTextError::TooLong {{ .. }}), got {other:?}"),
    }
}

#[test]
fn validate_handoff_text_accepts_exactly_the_note_byte_limit() {
    let value = "a".repeat(HANDOFF_NOTE_MAX_BYTES);
    assert_eq!(
        validate_handoff_text(&value, HANDOFF_NOTE_MAX_BYTES),
        Ok(())
    );
}

#[test]
fn validate_handoff_text_rejects_one_byte_over_the_note_limit() {
    let value = "a".repeat(HANDOFF_NOTE_MAX_BYTES + 1);
    let result = validate_handoff_text(&value, HANDOFF_NOTE_MAX_BYTES);
    match result {
        Err(HandoffTextError::TooLong { max, actual }) => {
            assert_eq!(max, HANDOFF_NOTE_MAX_BYTES);
            assert_eq!(actual, HANDOFF_NOTE_MAX_BYTES + 1);
        }
        other => panic!("expected Err(HandoffTextError::TooLong {{ .. }}), got {other:?}"),
    }
}

/// Every character `validate_handoff_text` must reject: the same character
/// class `is_forbidden_label_char` rejects for a window's display name
/// (C0/C1 controls, DEL, line/paragraph separators, bidi controls and the
/// zero-width/invisible set), spot-checked here rather than repeated in full
/// (the exhaustive enumeration is `window_name.rs`'s job for the label rule
/// this one shares implementation with).
fn forbidden_text_chars() -> Vec<char> {
    let mut chars = Vec::new();
    for cp in 0x0000u32..=0x001F {
        chars.push(char::from_u32(cp).expect("valid C0 control code point"));
    }
    chars.push('\u{007F}'); // DEL
    chars.push('\r');
    chars.push('\n');
    chars.push('\u{202E}'); // RIGHT-TO-LEFT OVERRIDE
    chars.push('\u{2066}'); // LEFT-TO-RIGHT ISOLATE
    chars.push('\u{200B}'); // ZERO WIDTH SPACE
    chars.push('\u{2028}'); // LINE SEPARATOR
    chars.push('\u{2029}'); // PARAGRAPH SEPARATOR
    chars.push('\u{FEFF}'); // ZERO WIDTH NO-BREAK SPACE / BOM
    chars
}

#[test]
fn validate_handoff_text_rejects_every_forbidden_character_with_its_position_and_code_point() {
    for ch in forbidden_text_chars() {
        let value = format!("a{ch}b");
        let result = validate_handoff_text(&value, HANDOFF_NOTE_MAX_BYTES);
        match result {
            Err(HandoffTextError::ForbiddenChar { at, ch: reported }) => {
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
                "expected Err(HandoffTextError::ForbiddenChar {{ .. }}) for U+{:04X}, got \
                 {other:?}",
                ch as u32
            ),
        }
    }
}

#[test]
fn validate_handoff_text_display_reports_the_code_point_and_offset_without_echoing_the_character() {
    let value = "a\u{202E}b";
    let result = validate_handoff_text(value, HANDOFF_NOTE_MAX_BYTES);
    let Err(err) = result else {
        panic!("expected a rejection for an embedded U+202E, got {result:?}");
    };
    let message = err.to_string();
    assert!(
        message.contains("U+202E at byte offset 1"),
        "the message must report the code point and byte offset in the \
         \"U+XXXX at byte offset N\" form, got {message:?}"
    );
    assert!(
        !message.contains('\u{202E}'),
        "the raw forbidden character must never be echoed into the message, got {message:?}"
    );
}

#[test]
fn validate_handoff_text_accepts_ordinary_ascii_symbols_and_multibyte_text() {
    for value in [
        "src/foo.rs:42 | grep -n \"needle\" (line 1)",
        "レビューをお願いします",
        "emoji ZWJ sequence: a\u{200D}b",
    ] {
        assert_eq!(
            validate_handoff_text(value, HANDOFF_NOTE_MAX_BYTES),
            Ok(()),
            "expected {value:?} to be accepted"
        );
    }
}

// =====================================================================
// validate_send_text_body: the same forbidden-character rule as
// validate_handoff_text, except `\n` and `\r` -- a send_message body's own
// submit marker and line boundaries -- are allowed.
// =====================================================================

#[test]
fn validate_send_text_body_rejects_every_forbidden_character_except_newline_and_cr() {
    for ch in forbidden_text_chars() {
        if matches!(ch, '\n' | '\r') {
            continue;
        }
        let value = format!("a{ch}b");
        let result = validate_send_text_body(&value);
        match result {
            Err(SendTextError::ForbiddenChar { at, ch: reported }) => {
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
                "expected Err(SendTextError::ForbiddenChar {{ .. }}) for U+{:04X}, got {other:?}",
                ch as u32
            ),
        }
    }
}

#[test]
fn validate_send_text_body_accepts_a_trailing_newline_and_a_trailing_cr() {
    for value in [
        "submit this line\n",
        "submit this line\r",
        "submit this line\r\n",
    ] {
        assert_eq!(
            validate_send_text_body(value),
            Ok(()),
            "expected {value:?} to be accepted"
        );
    }
}

// =====================================================================
// A body with more than one terminator -- any CR/LF that is not
// the single trailing one -- is not written through unchanged. The daemon
// cannot confirm such a body reached the receiver as more than one message, so it is rejected
// outright; a caller with several lines to deliver sends them one at a
// time, or writes them into a plan task's `instruction`.
// =====================================================================

#[test]
fn validate_send_text_body_rejects_a_middle_line_break() {
    for value in ["a\rb\r", "a\nb", "a\r\nb\r\n"] {
        let result = validate_send_text_body(value);
        assert!(
            matches!(result, Err(SendTextError::EmbeddedLineBreak { .. })),
            "expected {value:?} to be rejected as an embedded line break, got {result:?}"
        );
    }
}

#[test]
fn validate_send_text_body_rejects_a_middle_line_break_reporting_its_byte_offset() {
    let result = validate_send_text_body("first line\rsecond line\r");
    match result {
        Err(SendTextError::EmbeddedLineBreak { at }) => {
            assert_eq!(
                at, 10,
                "must report the byte offset of the first non-trailing terminator"
            );
        }
        other => panic!("expected Err(SendTextError::EmbeddedLineBreak {{ .. }}), got {other:?}"),
    }
}

#[test]
fn validate_send_text_body_still_accepts_no_terminator_terminator_only_and_empty() {
    for value in ["no terminator at all", "\r", "\n", "\r\n", ""] {
        assert_eq!(
            validate_send_text_body(value),
            Ok(()),
            "expected {value:?} to be accepted"
        );
    }
}

#[test]
fn send_text_error_embedded_line_break_display_reports_the_offset_without_a_character() {
    let result = validate_send_text_body("a\rb");
    let Err(err) = result else {
        panic!("expected a rejection for an embedded CR, got {result:?}");
    };
    let message = err.to_string();
    assert!(
        message.contains("byte offset 1"),
        "the message must report the byte offset, got {message:?}"
    );
}

#[test]
fn validate_send_text_body_rejects_escape_regardless_of_position() {
    for value in ["\x1b[2Jerased", "erased\x1b[2J", "era\x1bsed\n"] {
        assert!(
            matches!(
                validate_send_text_body(value),
                Err(SendTextError::ForbiddenChar { .. })
            ),
            "expected {value:?} to be rejected"
        );
    }
}

#[test]
fn validate_send_text_body_display_reports_the_code_point_and_offset_without_echoing_the_character()
{
    let value = "a\u{202E}b";
    let result = validate_send_text_body(value);
    let Err(err) = result else {
        panic!("expected a rejection for an embedded U+202E, got {result:?}");
    };
    let message = err.to_string();
    assert!(
        message.contains("U+202E at byte offset 1"),
        "the message must report the code point and byte offset in the \
         \"U+XXXX at byte offset N\" form, got {message:?}"
    );
    assert!(
        !message.contains('\u{202E}'),
        "the raw forbidden character must never be echoed into the message, got {message:?}"
    );
}

#[test]
fn validate_send_text_body_accepts_ordinary_ascii_symbols_and_multibyte_text() {
    for value in [
        "run the tests\n",
        "レビューをお願いします\n",
        "emoji ZWJ sequence: a\u{200D}b\n",
    ] {
        assert_eq!(
            validate_send_text_body(value),
            Ok(()),
            "expected {value:?} to be accepted"
        );
    }
}

// =====================================================================
// The four shared byte-limit constants
// =====================================================================

#[test]
fn the_four_handoff_byte_limits_have_the_documented_values() {
    assert_eq!(HANDOFF_REFERENCE_MAX_BYTES, 512);
    assert_eq!(HANDOFF_NOTE_MAX_BYTES, 1024);
    assert_eq!(APPROVAL_TEXT_MAX_BYTES, 2048);
    assert_eq!(
        HANDOFF_ENVELOPE_MAX_BYTES, APPROVAL_TEXT_MAX_BYTES,
        "the envelope shares its upper bound with the approval-dialog text limit"
    );
}

// =====================================================================
// HandoffDelivery: the delivery-confirmation enum's own wire contract,
// checked directly through `serde_json` rather than through a message
// envelope (the "wire round trip" section above covers the enum riding
// inside `DaemonMessage::HandoffSent`).
// =====================================================================

#[test]
fn handoff_delivery_round_trips_every_wire_string() {
    // Checked against literal wire text, so a change to the rename mapping
    // itself shows up here, not just a change to the variant list.
    for (delivery, wire) in [
        (HandoffDelivery::Confirmed, "\"confirmed\""),
        (HandoffDelivery::Unconfirmed, "\"unconfirmed\""),
        (HandoffDelivery::NotObserved, "\"not_observed\""),
        (HandoffDelivery::NotWritten, "\"not_written\""),
    ] {
        assert_eq!(
            serde_json::to_string(&delivery).expect("HandoffDelivery must encode"),
            wire
        );
        let decoded: HandoffDelivery =
            serde_json::from_str(wire).unwrap_or_else(|err| panic!("{wire} must decode: {err}"));
        assert_eq!(decoded, delivery, "decoded value mismatch for {wire}");
    }
}

#[test]
fn handoff_delivery_rejects_an_unrecognized_string() {
    let result: Result<HandoffDelivery, _> = serde_json::from_str("\"bogus\"");
    assert!(
        result.is_err(),
        "an unrecognized delivery string must fail to decode, got {result:?}"
    );
}
