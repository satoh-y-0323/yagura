#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Wire-protocol contracts for a plan task's `instruction` (wire revision 5):
//! `PlanTaskSpec.instruction` / `PlanTaskInfo.instruction` and the
//! `validate_task_instruction` validator and its byte limit.
//!
//! Wire revision 7 removed `DaemonMessage::TaskWindowOpened.instruction_delivery`
//! (formerly measured here); `notice_protocol.rs` covers what replaced it.

use yagura_protocol::{
    AssignmentSource, PlanTaskInfo, PlanTaskSpec, ResolvedAssignment, TASK_INSTRUCTION_MAX_BYTES,
    TaskInstructionError, validate_task_instruction,
};

fn as_json(value: &impl serde::Serialize) -> serde_json::Value {
    serde_json::to_value(value).expect("a wire payload type must serialize")
}

fn sample_plan_task_spec(instruction: Option<String>) -> PlanTaskSpec {
    PlanTaskSpec {
        task_id: "t1".to_string(),
        title: "write the tests".to_string(),
        role: None,
        profile: None,
        model: None,
        effort: None,
        depends_on: vec![],
        writes: vec![],
        instruction,
    }
}

fn sample_plan_task_info(instruction: Option<String>) -> PlanTaskInfo {
    PlanTaskInfo {
        task_id: "t1".to_string(),
        title: "write the tests".to_string(),
        role: None,
        depends_on: vec![],
        writes: vec![],
        wave: 0,
        parent: None,
        user: None,
        resolved: ResolvedAssignment {
            source: AssignmentSource::Unassigned,
            assignment: None,
        },
        window_id: None,
        report: None,
        instruction,
        instruction_notice: None,
        review_mark: None,
    }
}

// =============================================================================
// PlanTaskSpec.instruction / PlanTaskInfo.instruction round trip;
// None omits the key; a frame without the key decodes to None.
// =============================================================================

#[test]
fn plan_task_spec_instruction_some_round_trips_through_json() {
    let value = sample_plan_task_spec(Some("do the thing\nand this too".to_string()));
    let json = as_json(&value);
    assert_eq!(
        json.get("instruction").and_then(serde_json::Value::as_str),
        Some("do the thing\nand this too")
    );
    let decoded: PlanTaskSpec = serde_json::from_value(json).expect("PlanTaskSpec must decode");
    assert_eq!(decoded, value);
}

#[test]
fn plan_task_spec_instruction_none_omits_the_key() {
    let value = sample_plan_task_spec(None);
    let json = as_json(&value);
    let object = json
        .as_object()
        .expect("PlanTaskSpec must serialize as an object");
    assert!(
        !object.contains_key("instruction"),
        "a PlanTaskSpec with instruction: None must omit the key entirely, got: {json}"
    );
}

#[test]
fn plan_task_spec_without_an_instruction_key_decodes_to_none() {
    let plan_task_json = serde_json::json!({
        "task_id": "t1",
        "title": "write the tests",
        "role": null,
        "profile": null,
        "model": null,
        "effort": null,
        "depends_on": [],
        "writes": []
    });
    let decoded: PlanTaskSpec = serde_json::from_value(plan_task_json)
        .expect("a PlanTaskSpec without an instruction key must still decode (older-caller frame)");
    assert_eq!(decoded.instruction, None);
}

#[test]
fn plan_task_info_instruction_some_round_trips_through_json() {
    let value = sample_plan_task_info(Some("read this before you start".to_string()));
    let json = as_json(&value);
    assert_eq!(
        json.get("instruction").and_then(serde_json::Value::as_str),
        Some("read this before you start")
    );
    let decoded: PlanTaskInfo = serde_json::from_value(json).expect("PlanTaskInfo must decode");
    assert_eq!(decoded, value);
}

#[test]
fn plan_task_info_instruction_none_omits_the_key() {
    let value = sample_plan_task_info(None);
    let json = as_json(&value);
    let object = json
        .as_object()
        .expect("PlanTaskInfo must serialize as an object");
    assert!(
        !object.contains_key("instruction"),
        "a PlanTaskInfo with instruction: None must omit the key entirely, got: {json}"
    );
}

#[test]
fn plan_task_info_without_an_instruction_key_decodes_to_none() {
    let plan_task_json = serde_json::json!({
        "task_id": "t1",
        "title": "write the tests",
        "role": null,
        "depends_on": [],
        "writes": [],
        "wave": 0,
        "parent": null,
        "user": null,
        "resolved": { "source": "unassigned", "assignment": null },
        "window_id": null
    });
    let decoded: PlanTaskInfo = serde_json::from_value(plan_task_json)
        .expect("a PlanTaskInfo without an instruction key must still decode (older-daemon frame)");
    assert_eq!(decoded.instruction, None);
}

// =============================================================================
// TASK_INSTRUCTION_MAX_BYTES and validate_task_instruction
// =============================================================================

#[test]
fn task_instruction_max_bytes_is_sixteen_kib() {
    assert_eq!(TASK_INSTRUCTION_MAX_BYTES, 16 * 1024);
}

#[test]
fn validate_task_instruction_rejects_empty() {
    assert_eq!(
        validate_task_instruction(""),
        Err(TaskInstructionError::Empty)
    );
}

#[test]
fn validate_task_instruction_accepts_exactly_the_byte_limit() {
    let value = "a".repeat(TASK_INSTRUCTION_MAX_BYTES);
    assert_eq!(validate_task_instruction(&value), Ok(()));
}

#[test]
fn validate_task_instruction_rejects_one_byte_over_the_limit() {
    let value = "a".repeat(TASK_INSTRUCTION_MAX_BYTES + 1);
    assert_eq!(
        validate_task_instruction(&value),
        Err(TaskInstructionError::TooLong {
            len: TASK_INSTRUCTION_MAX_BYTES + 1,
            max: TASK_INSTRUCTION_MAX_BYTES,
        })
    );
}

#[test]
fn validate_task_instruction_accepts_lf_and_tab() {
    assert_eq!(
        validate_task_instruction("first line\n\tsecond line, indented"),
        Ok(())
    );
}

/// Every character `validate_task_instruction` must reject: the same set
/// [`is_forbidden_label_char`] documents (in `src/lib.rs`), except LF and TAB
/// -- the two control characters an instruction's own line structure needs.
/// Unlike [`validate_send_text_body`]'s rule, CR stays forbidden here: a
/// caller normalizes CR to LF before this validator ever sees the value.
fn forbidden_instruction_chars() -> Vec<char> {
    let mut chars = Vec::new();
    for cp in 0x0000u32..=0x001F {
        if cp == 0x0A || cp == 0x09 {
            continue;
        }
        chars.push(char::from_u32(cp).expect("valid C0 control code point"));
    }
    chars.push('\u{007F}'); // DEL
    chars.push('\u{202E}'); // RIGHT-TO-LEFT OVERRIDE
    chars.push('\u{2066}'); // LEFT-TO-RIGHT ISOLATE
    chars.push('\u{200B}'); // ZERO WIDTH SPACE
    chars.push('\u{2028}'); // LINE SEPARATOR
    chars.push('\u{2029}'); // PARAGRAPH SEPARATOR
    chars.push('\u{FEFF}'); // ZERO WIDTH NO-BREAK SPACE / BOM
    chars
}

#[test]
fn validate_task_instruction_rejects_every_forbidden_character() {
    for ch in forbidden_instruction_chars() {
        let value = format!("a{ch}b");
        let result = validate_task_instruction(&value);
        match result {
            Err(TaskInstructionError::ForbiddenChar { at, ch: reported }) => {
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
                "expected Err(TaskInstructionError::ForbiddenChar {{ .. }}) for U+{:04X}, got \
                 {other:?}",
                ch as u32
            ),
        }
    }
}

#[test]
fn task_instruction_error_display_never_echoes_the_forbidden_character() {
    let value = "a\u{202E}b";
    let result = validate_task_instruction(value);
    let Err(err) = result else {
        panic!("expected a rejection for an embedded U+202E, got {result:?}");
    };
    let message = err.to_string();
    assert!(
        message.contains("U+202E"),
        "the message must report the code point, got {message:?}"
    );
    assert!(
        !message.contains('\u{202E}'),
        "the raw forbidden character must never be echoed into the message, got {message:?}"
    );
}

// The `instruction_delivery` field is intentionally not measured: wire
// revision 7 removed it; see this file's module doc and notice_protocol.rs.
