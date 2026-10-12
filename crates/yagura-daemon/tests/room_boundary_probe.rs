#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Boundary-value probes for `yagura_daemon::room` that `room.rs` does not
//! already cover: the mention scanner fed nothing but separator characters
//! (a lone `@`, `@@`, a trailing `@`), full-width Japanese punctuation as the
//! boundary that cuts an unquoted mention, mention resolution against an
//! empty roster, and the chat envelope's byte cap crossed by a multi-byte
//! character rather than an ASCII one.

use yagura_daemon::room::{ChatEnvelopeFrom, RosterEntry, compose_chat_envelope, parse_mentions};
use yagura_protocol::HANDOFF_ENVELOPE_MAX_BYTES;

fn roster(entries: &[(&str, &str)]) -> Vec<RosterEntry> {
    entries
        .iter()
        .map(|(window_id, name)| RosterEntry {
            window_id: (*window_id).to_string(),
            name: (*name).to_string(),
        })
        .collect()
}

// =====================================================================
// The scanner fed nothing but `@` separators: must not panic, and must
// find no mention at all (there is no word character for it to attach to).
// =====================================================================

#[test]
fn a_lone_at_sign_with_nothing_after_it_yields_no_mention() {
    let people = roster(&[("w-a", "Alice")]);
    let mentions = parse_mentions("@", &people, "w-z");
    assert!(mentions.targets.is_empty());
    assert!(mentions.unresolved.is_empty());
    assert!(!mentions.user);
}

#[test]
fn two_at_signs_back_to_back_yield_no_mention() {
    let people = roster(&[("w-a", "Alice")]);
    let mentions = parse_mentions("hi @@ there", &people, "w-z");
    assert!(mentions.targets.is_empty());
    assert!(mentions.unresolved.is_empty());
}

#[test]
fn a_trailing_at_sign_at_the_end_of_the_body_yields_no_mention() {
    let people = roster(&[("w-a", "Alice")]);
    let mentions = parse_mentions("ping Alice @", &people, "w-z");
    assert!(mentions.targets.is_empty());
    assert!(mentions.unresolved.is_empty());
}

#[test]
fn an_at_sign_immediately_followed_by_whitespace_yields_no_mention() {
    let people = roster(&[("w-a", "Alice")]);
    let mentions = parse_mentions("hi @ Alice", &people, "w-z");
    assert!(mentions.targets.is_empty());
    assert!(mentions.unresolved.is_empty());
}

// =====================================================================
// Full-width (Japanese) punctuation as the boundary that cuts an unquoted
// mention -- `is_mention_word_char` excludes it exactly like ASCII
// punctuation, but this is only proven by running it, not by reading the
// character-class predicate.
// =====================================================================

#[test]
fn a_full_width_japanese_period_cuts_an_unquoted_mention() {
    let people = roster(&[("w-a", "Alice"), ("w-b", "Bob")]);
    let mentions = parse_mentions("ping @Bob。 done", &people, "w-a");
    assert_eq!(mentions.targets, vec!["w-b".to_string()]);
}

#[test]
fn a_full_width_japanese_comma_cuts_an_unquoted_mention() {
    let people = roster(&[("w-a", "Alice"), ("w-b", "Bob")]);
    let mentions = parse_mentions("ping @Bob、お願い", &people, "w-a");
    assert_eq!(mentions.targets, vec!["w-b".to_string()]);
}

#[test]
fn a_full_width_question_mark_cuts_an_unquoted_mention() {
    let people = roster(&[("w-a", "Alice"), ("w-b", "Bob")]);
    let mentions = parse_mentions("@Bob？", &people, "w-a");
    assert_eq!(mentions.targets, vec!["w-b".to_string()]);
}

#[test]
fn a_full_width_exclamation_mark_cuts_an_unquoted_mention() {
    let people = roster(&[("w-a", "Alice"), ("w-b", "Bob")]);
    let mentions = parse_mentions("@Bob！", &people, "w-a");
    assert_eq!(mentions.targets, vec!["w-b".to_string()]);
}

// =====================================================================
// An empty roster: every mention becomes unresolved, `@user`/`@all` are
// still recognized (they never consult the roster).
// =====================================================================

#[test]
fn every_mention_is_unresolved_against_an_empty_roster() {
    let mentions = parse_mentions("ping @Bob and @Carol", &[], "w-z");
    assert!(mentions.targets.is_empty());
    assert_eq!(
        mentions.unresolved,
        vec!["Bob".to_string(), "Carol".to_string()]
    );
}

#[test]
fn at_user_and_at_all_still_resolve_specially_against_an_empty_roster() {
    let mentions = parse_mentions("@user @all", &[], "w-z");
    assert!(mentions.user);
    assert!(mentions.targets.is_empty());
    assert!(mentions.unresolved.is_empty());
}

// =====================================================================
// The chat envelope's byte cap crossed by a multi-byte character: the body
// itself is ASCII up to the boundary, but the byte that pushes the envelope
// past the cap belongs to a multi-byte UTF-8 character, not a single ASCII
// byte. `compose_chat_envelope` must neither panic nor split the character.
// =====================================================================

#[test]
fn an_envelope_landing_exactly_at_the_byte_cap_via_a_multi_byte_character_is_accepted() {
    let from = ChatEnvelopeFrom::Window {
        window_id: "w-a".to_string(),
        name: "n".to_string(),
    };
    // "あ" is 3 bytes in UTF-8. Find the ASCII filler length that makes the
    // envelope land exactly on the cap once "あ" (3 bytes) is appended.
    let base = compose_chat_envelope(&from, "main", &[], true, "")
        .expect("an empty body must fit under the byte cap");
    let ascii_len = HANDOFF_ENVELOPE_MAX_BYTES - base.len() - "あ".len();
    let body = "x".repeat(ascii_len) + "あ";

    let envelope =
        compose_chat_envelope(&from, "main", &[], true, &body).expect("must fit exactly");
    assert_eq!(envelope.len(), HANDOFF_ENVELOPE_MAX_BYTES);
    // The multi-byte character must survive intact (not truncated mid-codepoint):
    // parsing the envelope's JSON payload back out must recover the exact body.
    let json_text = envelope
        .strip_prefix("[yagura chat] ")
        .expect("the envelope must carry the documented prefix");
    let value: serde_json::Value =
        serde_json::from_str(json_text).expect("the envelope's payload must be valid JSON");
    assert_eq!(value["body"], serde_json::Value::String(body));
}

#[test]
fn an_envelope_one_multi_byte_character_past_the_byte_cap_is_rejected_without_panicking() {
    let from = ChatEnvelopeFrom::Window {
        window_id: "w-a".to_string(),
        name: "n".to_string(),
    };
    let base = compose_chat_envelope(&from, "main", &[], true, "")
        .expect("an empty body must fit under the byte cap");
    // One ASCII byte more than the exact-fit case above pushes the trailing
    // "あ" (3 bytes) one byte past the cap.
    let ascii_len = HANDOFF_ENVELOPE_MAX_BYTES - base.len() - "あ".len() + 1;
    let body = "x".repeat(ascii_len) + "あ";

    let result = compose_chat_envelope(&from, "main", &[], true, &body);
    assert!(
        result.is_err(),
        "a body whose multi-byte tail pushes the envelope past the cap must be rejected"
    );
}
