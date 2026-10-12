#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! `yagura_daemon::handoff`: the pure functions behind `send_handoff` --
//! composing the envelope written into the target window's PTY, checking its
//! length against the shared 2048-byte cap, and deciding whether a status
//! counts as "ready" for a handoff to land on.
//!
//! `compose_envelope` and `check_envelope_len` are exercised two ways in this
//! file: against a fixed literal held independently in this test (so the
//! byte-accounting fields cannot simply mirror whatever the implementation
//! computes), and through `serde_json` round trips (so escaping and structure
//! are checked by an independent parser rather than by substring matching).

use std::time::Duration;

use serde_json::Value;
use tokio::time::Instant;

use yagura_daemon::handoff::{
    DELIVERY_REACTION_STATUSES, Envelope, EnvelopeFrom, EnvelopeParts, EnvelopeTooLong,
    READY_STATUSES, ReadyHoldLatch, StartupEvidence, check_envelope_len, compose_envelope,
    hold_is_reachable_within, is_ready, reacts_to_delivery,
};
use yagura_daemon::waiter::StatusAt;
use yagura_protocol::{DetectedStatus, HANDOFF_ENVELOPE_MAX_BYTES, UnknownReason, WindowStatus};

const PREFIX: &str = "[yagura handoff] ";

fn window_from(id: &str, name: &str, cwd: &str) -> EnvelopeFrom {
    EnvelopeFrom::Window {
        id: id.to_string(),
        name: name.to_string(),
        cwd: cwd.to_string(),
    }
}

/// Strips the fixed prefix and parses the remainder as JSON, failing loudly
/// (rather than silently accepting a malformed envelope) if either step does
/// not hold.
fn parse_envelope_json(envelope: &Envelope) -> Value {
    let json_part = envelope.text.strip_prefix(PREFIX).unwrap_or_else(|| {
        panic!(
            "envelope text must start with {PREFIX:?}: {:?}",
            envelope.text
        )
    });
    serde_json::from_str(json_part)
        .unwrap_or_else(|err| panic!("envelope JSON part must parse: {err}, text: {json_part:?}"))
}

// =====================================================================
// compose_envelope: fixed literal cross-check (window requester)
// =====================================================================

/// The base fixture for `compose_envelope` (`handoff.rs`): window_id = "A",
/// name = "n", cwd = "c", reference = "r", note = "t". Held here verbatim, so
/// this test checks the implementation against an independently-written
/// expectation rather than against itself.
const FIXED_LITERAL: &str = "[yagura handoff] {\"from\":{\"window_id\":\"A\",\"name\":\"n\",\"cwd\":\"c\"},\"reference\":\"r\",\"note\":\"t\",\"reply\":{\"tool\":\"send_handoff\",\"window_id\":\"A\"}}";

#[test]
fn compose_envelope_matches_the_fixed_literal_for_the_base_fixture() {
    let envelope = compose_envelope(&window_from("A", "n", "c"), "r", "t");

    assert_eq!(envelope.text, FIXED_LITERAL);
    assert_eq!(
        envelope.parts.overhead,
        FIXED_LITERAL.len() - 4,
        "overhead is everything except the four 1-byte fields (name, cwd, \
         reference, note)"
    );
    assert_eq!(envelope.parts.name, 1);
    assert_eq!(envelope.parts.cwd, 1);
    assert_eq!(envelope.parts.reference, 1);
    assert_eq!(envelope.parts.note, 1);
}

#[test]
fn compose_envelope_base_fixture_is_valid_single_line_json_with_the_documented_key_order() {
    let envelope = compose_envelope(&window_from("A", "n", "c"), "r", "t");
    assert!(!envelope.text.contains('\n'));
    assert!(!envelope.text.contains('\r'));

    let value = parse_envelope_json(&envelope);
    let from = &value["from"];
    assert_eq!(from["window_id"], "A");
    assert_eq!(from["name"], "n");
    assert_eq!(from["cwd"], "c");
    assert_eq!(value["reference"], "r");
    assert_eq!(value["note"], "t");
    assert_eq!(value["reply"]["tool"], "send_handoff");
    assert_eq!(value["reply"]["window_id"], "A");
}

#[test]
fn a_longer_window_id_only_changes_overhead_since_it_appears_twice() {
    let base = compose_envelope(&window_from("A", "n", "c"), "r", "t");
    let longer = compose_envelope(&window_from("ABCD", "n", "c"), "r", "t");

    // "ABCD" is 3 bytes longer than "A", and the requester's window id
    // appears twice in the envelope (from.window_id and reply.window_id), so
    // overhead must grow by 2 * 3 = 6 while the four measured fields stay put.
    assert_eq!(
        longer.parts.overhead,
        base.parts.overhead + 6,
        "the window id is counted as overhead wherever it appears (from and reply)"
    );
    assert_eq!(longer.parts.name, base.parts.name);
    assert_eq!(longer.parts.cwd, base.parts.cwd);
    assert_eq!(longer.parts.reference, base.parts.reference);
    assert_eq!(longer.parts.note, base.parts.note);
}

#[test]
fn a_quote_and_backslash_in_note_are_json_encoded_and_do_not_change_overhead() {
    let base = compose_envelope(&window_from("A", "n", "c"), "r", "t");
    let escaped = compose_envelope(&window_from("A", "n", "c"), "r", "a\"b");

    // `a"b` (3 raw characters) JSON-encodes as `a\"b` (4 bytes: a, backslash,
    // quote, b), so the measured note length is 4, not 3.
    assert_eq!(
        escaped.parts.note, 4,
        "the JSON-encoded byte count includes the escape backslash"
    );
    assert_eq!(
        escaped.parts.overhead, base.parts.overhead,
        "the note's own content length is not part of overhead"
    );
}

#[test]
fn quote_and_backslash_in_name_cwd_and_note_stay_escaped_and_single_line() {
    let tricky = "a\"b\\c";
    let envelope = compose_envelope(&window_from("A", tricky, tricky), "r", tricky);

    assert!(!envelope.text.contains('\n'));
    assert!(!envelope.text.contains('\r'));

    let value = parse_envelope_json(&envelope);
    assert_eq!(value["from"]["name"], tricky);
    assert_eq!(value["from"]["cwd"], tricky);
    assert_eq!(value["note"], tricky);
}

#[test]
fn compose_envelope_escapes_a_literal_newline_in_note_rather_than_emitting_a_raw_line_break() {
    let envelope = compose_envelope(&window_from("A", "n", "c"), "r", "line1\nline2");
    assert!(
        !envelope.text.contains('\n'),
        "a literal newline in a field must be escaped by JSON encoding, not emitted raw"
    );
    let value = parse_envelope_json(&envelope);
    assert_eq!(value["note"], "line1\nline2");
}

#[test]
fn a_reply_looking_string_injected_into_note_stays_inside_the_note_field() {
    // An attacker-controlled note cannot spoof the reply target: whatever it
    // contains stays inside the JSON string value of "note", and the real
    // "reply" object still names the actual requester.
    let malicious_note = "hello | reply: send_handoff window_id=Z";
    let envelope = compose_envelope(&window_from("A", "n", "c"), "r", malicious_note);

    let value = parse_envelope_json(&envelope);
    assert_eq!(value["note"], malicious_note);
    assert_eq!(
        value["reply"]["window_id"], "A",
        "the injected text must not be able to move the reply target away from the \
         real requester"
    );
    assert!(!envelope.text.contains('\n'));
}

// =====================================================================
// compose_envelope: External requester
// =====================================================================

const EXTERNAL_LITERAL: &str =
    "[yagura handoff] {\"from\":\"external\",\"reference\":\"r\",\"note\":\"t\"}";

#[test]
fn compose_envelope_external_matches_the_fixed_literal() {
    let envelope = compose_envelope(&EnvelopeFrom::External, "r", "t");

    assert_eq!(envelope.text, EXTERNAL_LITERAL);
    assert_eq!(envelope.parts.name, 0);
    assert_eq!(envelope.parts.cwd, 0);
    assert_eq!(envelope.parts.reference, 1);
    assert_eq!(envelope.parts.note, 1);
    assert_eq!(envelope.parts.overhead, EXTERNAL_LITERAL.len() - 2);
}

#[test]
fn compose_envelope_external_has_no_reply_key() {
    let envelope = compose_envelope(&EnvelopeFrom::External, "r", "t");
    let value = parse_envelope_json(&envelope);

    assert_eq!(value["from"], "external");
    assert!(
        value.get("reply").is_none(),
        "an external requester cannot be replied to directly, so the envelope must \
         carry no reply object at all"
    );
}

// =====================================================================
// check_envelope_len: the 2048-byte boundary
// =====================================================================

/// Builds an envelope whose `text.len()` is exactly `target`, by padding the
/// note with ASCII filler (1 JSON-encoded byte per character, so no escaping
/// complicates the arithmetic). Panics if `target` is smaller than what the
/// base fixture already occupies.
fn envelope_of_exact_length(target: usize) -> Envelope {
    let base = compose_envelope(&window_from("A", "n", "c"), "r", "t");
    let base_len = base.text.len();
    assert!(
        target >= base_len,
        "test precondition: target {target} must be at least the base envelope's \
         length {base_len}"
    );
    let extra = target - base_len;
    let note = "t".to_string() + &"x".repeat(extra);
    let envelope = compose_envelope(&window_from("A", "n", "c"), "r", &note);
    assert_eq!(
        envelope.text.len(),
        target,
        "test precondition failed: padding the note must reach exactly the target length"
    );
    envelope
}

#[test]
fn check_envelope_len_accepts_exactly_the_2048_byte_boundary() {
    let envelope = envelope_of_exact_length(HANDOFF_ENVELOPE_MAX_BYTES);
    assert_eq!(check_envelope_len(&envelope), Ok(()));
}

#[test]
fn check_envelope_len_rejects_one_byte_over_the_2048_boundary() {
    let envelope = envelope_of_exact_length(HANDOFF_ENVELOPE_MAX_BYTES + 1);
    let result = check_envelope_len(&envelope);
    match result {
        Err(EnvelopeTooLong { len, max, parts }) => {
            assert_eq!(len, HANDOFF_ENVELOPE_MAX_BYTES + 1);
            assert_eq!(max, HANDOFF_ENVELOPE_MAX_BYTES);
            assert_eq!(
                parts.overhead + parts.name + parts.cwd + parts.reference + parts.note,
                len,
                "the reported parts must sum to the reported length"
            );
        }
        other => panic!("expected Err(EnvelopeTooLong {{ .. }}), got {other:?}"),
    }
}

#[test]
fn envelope_too_long_display_matches_the_documented_format() {
    let envelope = envelope_of_exact_length(HANDOFF_ENVELOPE_MAX_BYTES + 1);
    let Err(err) = check_envelope_len(&envelope) else {
        panic!("expected an EnvelopeTooLong rejection at 1 byte over the boundary");
    };
    let EnvelopeTooLong { len, max, parts } = err;
    let expected = format!(
        "envelope is {len} bytes (max {max}): overhead {}, name {}, cwd {}, reference {}, \
         note {} (JSON-encoded sizes)",
        parts.overhead, parts.name, parts.cwd, parts.reference, parts.note
    );
    assert_eq!(err.to_string(), expected);
}

#[test]
fn envelope_parts_fields_always_sum_to_the_text_length() {
    for envelope in [
        compose_envelope(&window_from("A", "n", "c"), "r", "t"),
        compose_envelope(
            &window_from("ABCD", "reviewer", "/work/repo"),
            "src/x.rs",
            "hi",
        ),
        compose_envelope(&EnvelopeFrom::External, "r", "t"),
    ] {
        let EnvelopeParts {
            overhead,
            name,
            cwd,
            reference,
            note,
        } = envelope.parts;
        assert_eq!(
            overhead + name + cwd + reference + note,
            envelope.text.len(),
            "the five parts must always sum to the full envelope length: {:?}",
            envelope.text
        );
    }
}

// =====================================================================
// is_ready / READY_STATUSES
// =====================================================================

#[test]
fn is_ready_is_true_only_for_waiting_input_and_idle() {
    for status in [WindowStatus::WaitingInput, WindowStatus::Idle] {
        assert!(is_ready(status), "{status:?} must be ready");
    }
    for status in [
        WindowStatus::Running,
        WindowStatus::WaitingApproval,
        WindowStatus::Interrupted,
        WindowStatus::Exited,
        WindowStatus::Unknown,
    ] {
        assert!(!is_ready(status), "{status:?} must not be ready");
    }
}

#[test]
fn ready_statuses_constant_is_exactly_waiting_input_and_idle() {
    assert_eq!(
        READY_STATUSES.to_vec(),
        vec![WindowStatus::WaitingInput, WindowStatus::Idle]
    );
}

// =====================================================================
// DELIVERY_REACTION_STATUSES / reacts_to_delivery: the seven statuses split into ready / reacts /
// neither, with Unknown and Exited belonging to neither.
// =====================================================================

const ALL_SEVEN_STATUSES: [WindowStatus; 7] = [
    WindowStatus::Running,
    WindowStatus::WaitingInput,
    WindowStatus::WaitingApproval,
    WindowStatus::Interrupted,
    WindowStatus::Idle,
    WindowStatus::Exited,
    WindowStatus::Unknown,
];

#[test]
fn delivery_reaction_statuses_constant_is_exactly_running_waiting_approval_interrupted() {
    assert_eq!(
        DELIVERY_REACTION_STATUSES.to_vec(),
        vec![
            WindowStatus::Running,
            WindowStatus::WaitingApproval,
            WindowStatus::Interrupted,
        ]
    );
}

#[test]
fn reacts_to_delivery_is_true_only_for_running_waiting_approval_and_interrupted() {
    for status in [
        WindowStatus::Running,
        WindowStatus::WaitingApproval,
        WindowStatus::Interrupted,
    ] {
        assert!(
            reacts_to_delivery(status),
            "{status:?} must react to delivery"
        );
    }
    for status in [
        WindowStatus::WaitingInput,
        WindowStatus::Idle,
        WindowStatus::Exited,
        WindowStatus::Unknown,
    ] {
        assert!(
            !reacts_to_delivery(status),
            "{status:?} must not react to delivery"
        );
    }
}

#[test]
fn is_ready_and_reacts_to_delivery_split_all_seven_statuses_with_no_overlap() {
    let mut ready_count = 0;
    let mut reacts_count = 0;
    for status in ALL_SEVEN_STATUSES {
        let ready = is_ready(status);
        let reacts = reacts_to_delivery(status);
        assert!(
            !(ready && reacts),
            "{status:?} must not be counted as both ready and a delivery reaction"
        );
        if ready {
            ready_count += 1;
        }
        if reacts {
            reacts_count += 1;
        }
    }
    assert_eq!(
        ready_count, 2,
        "READY_STATUSES must hold exactly 2 statuses"
    );
    assert_eq!(
        reacts_count, 3,
        "DELIVERY_REACTION_STATUSES must hold exactly 3 statuses"
    );
}

#[test]
fn unknown_and_exited_belong_to_neither_ready_nor_reacts_to_delivery() {
    for status in [WindowStatus::Unknown, WindowStatus::Exited] {
        assert!(!is_ready(status), "{status:?} must not be ready");
        assert!(
            !reacts_to_delivery(status),
            "{status:?} must not count as a delivery reaction"
        );
    }
}

// =====================================================================
// ReadyHoldLatch / StartupEvidence / hold_is_reachable_within. `observe`
// takes no clock
// of its own: `now` is always supplied by the caller, so every case below
// is deterministic without a real-time sleep.
// =====================================================================

#[test]
fn zero_hold_latch_is_latched_at_construction_and_the_first_observe_reports_left() {
    let latch = ReadyHoldLatch::new(Duration::ZERO);
    assert!(
        latch.is_latched(),
        "a zero hold must be latched from construction, before any observe() call"
    );

    let now = Instant::now();
    let observed = StatusAt {
        status: DetectedStatus::Running,
        changed_at: now,
        evidence_since: now,
        generation: 0,
    };
    assert_eq!(
        latch.observe(observed, now),
        StartupEvidence::Left,
        "a zero-hold latch must report Left even for a non-ready status, since it is \
         already latched"
    );
}

#[test]
fn observe_reports_building_when_the_ready_status_has_not_held_for_the_full_duration_yet() {
    let latch = ReadyHoldLatch::new(Duration::from_millis(1000));
    let now = Instant::now();
    let changed_at = now
        .checked_sub(Duration::from_millis(400))
        .expect("the test clock must support a 400ms offset into the past");
    let observed = StatusAt {
        status: DetectedStatus::WaitingInput,
        changed_at,
        evidence_since: changed_at,
        generation: 0,
    };

    assert_eq!(latch.observe(observed, now), StartupEvidence::Building);
    assert!(!latch.is_latched(), "an unsatisfied hold must not latch");
}

#[test]
fn observe_reports_left_and_latches_once_the_ready_status_has_held_for_the_full_duration() {
    let latch = ReadyHoldLatch::new(Duration::from_millis(1000));
    let now = Instant::now();
    let changed_at = now
        .checked_sub(Duration::from_millis(1500))
        .expect("the test clock must support a 1500ms offset into the past");
    let observed = StatusAt {
        status: DetectedStatus::Idle,
        changed_at,
        evidence_since: changed_at,
        generation: 0,
    };

    assert_eq!(latch.observe(observed, now), StartupEvidence::Left);
    assert!(
        latch.is_latched(),
        "a satisfied hold must latch as a side effect of the observe() that found it"
    );
}

#[test]
fn once_latched_a_later_non_ready_observe_still_reports_left() {
    let latch = ReadyHoldLatch::new(Duration::from_millis(500));
    let now = Instant::now();
    let changed_at = now
        .checked_sub(Duration::from_millis(600))
        .expect("the test clock must support a 600ms offset into the past");
    let satisfied = StatusAt {
        status: DetectedStatus::WaitingInput,
        changed_at,
        evidence_since: changed_at,
        generation: 0,
    };
    assert_eq!(latch.observe(satisfied, now), StartupEvidence::Left);
    assert!(
        latch.is_latched(),
        "test precondition: the latch must be set"
    );

    let later = now
        .checked_add(Duration::from_millis(50))
        .expect("the test clock must support a 50ms offset into the future");
    let non_ready = StatusAt {
        status: DetectedStatus::Running,
        changed_at: later,
        evidence_since: later,
        generation: 0,
    };
    assert_eq!(
        latch.observe(non_ready, later),
        StartupEvidence::Left,
        "latching is monotonic: a non-ready observation after latching must not un-latch it"
    );
}

#[test]
fn observe_reports_absent_for_a_running_status_before_the_latch_is_set() {
    let latch = ReadyHoldLatch::new(Duration::from_millis(500));
    let now = Instant::now();
    let observed = StatusAt {
        status: DetectedStatus::Running,
        changed_at: now,
        evidence_since: now,
        generation: 0,
    };

    assert_eq!(latch.observe(observed, now), StartupEvidence::Absent);
    assert!(!latch.is_latched());
}

#[test]
fn observe_reports_absent_for_unknown_and_exited_before_the_latch_is_set() {
    for status in [
        DetectedStatus::Unknown(UnknownReason::NoRuleMatched),
        DetectedStatus::Exited,
    ] {
        let latch = ReadyHoldLatch::new(Duration::from_millis(500));
        let now = Instant::now();
        let observed = StatusAt {
            status,
            changed_at: now,
            evidence_since: now,
            generation: 0,
        };
        assert_eq!(
            latch.observe(observed, now),
            StartupEvidence::Absent,
            "{status:?} must report Absent before the latch is set"
        );
    }
}

#[test]
fn latch_sets_is_latched_unconditionally() {
    let latch = ReadyHoldLatch::new(Duration::from_millis(500));
    assert!(!latch.is_latched(), "test precondition: not yet latched");

    latch.latch();

    assert!(
        latch.is_latched(),
        "latch() must set is_latched() regardless of any prior observe() outcome"
    );
}

#[test]
fn hold_is_reachable_within_is_true_only_when_hold_is_strictly_less_than_timeout() {
    assert!(
        hold_is_reachable_within(Duration::from_millis(1000), Duration::from_millis(2000)),
        "a hold smaller than the timeout must be reachable"
    );
    assert!(
        !hold_is_reachable_within(Duration::from_millis(2000), Duration::from_millis(2000)),
        "an equal hold and timeout must not count as reachable"
    );
    assert!(
        !hold_is_reachable_within(Duration::from_millis(3000), Duration::from_millis(2000)),
        "a hold larger than the timeout must not be reachable"
    );
}
