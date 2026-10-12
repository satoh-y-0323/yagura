#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Wire-protocol contracts for the model-alias messages: two
//! `ClientMessage` requests (`list_model_choices`, `save_model_aliases`),
//! three `DaemonMessage` responses (`model_choices_requested`,
//! `model_choices`, `model_aliases_saved`), and the two wire types they carry
//! (`ModelChoiceSource`, `ModelChoice`).
//!
//! Five properties are measured:
//!
//! - every new request/response round trips through
//!   `encode_*_message` / `decode_*_message` and carries the documented
//!   `snake_case` wire tag.
//! - every `Option` field among the new types (`ModelChoices.source`,
//!   `ModelChoices.error`, `ModelChoice.label`) is absent from the wire --
//!   never `null` -- when `None`, and a frame missing the key still decodes
//!   to `None` rather than failing (the same older-peer contract every other
//!   optional field on this wire follows).
//! - `ModelChoiceSource` has exactly two wire forms, `{"kind":"values"}`
//!   and `{"kind":"command","argv":[...]}`, and carries no catch-all: a third
//!   `kind` fails the whole frame's decode.
//! - `list_model_choices` carries a `request_id` the caller supplies.
//! - a malformed `request_id` (negative, a string, or missing
//!   entirely) fails `list_model_choices`'s decode. The revision these
//!   messages need is pinned in `required_revision.rs`.

use yagura_protocol::{
    ClientMessage, DaemonMessage, LaunchOptionAlias, ModelChoice, ModelChoiceSource,
    decode_client_message, decode_daemon_message, encode_client_message, encode_daemon_message,
};

fn as_json(value: &impl serde::Serialize) -> serde_json::Value {
    serde_json::to_value(value).expect("a model-alias wire type must serialize")
}

fn assert_wire_tag(value: &serde_json::Value, expected: &str) {
    assert_eq!(
        value.get("type").and_then(serde_json::Value::as_str),
        Some(expected),
        "the wire tag must be snake_case, got: {value}"
    );
}

fn sample_model_choice(label: Option<&str>) -> ModelChoice {
    ModelChoice {
        id: "m1".to_string(),
        label: label.map(str::to_string),
    }
}

fn sample_model_choices(
    source: Option<ModelChoiceSource>,
    choices: Vec<ModelChoice>,
    error: Option<String>,
) -> DaemonMessage {
    DaemonMessage::ModelChoices {
        request_id: 7,
        profile: "p".to_string(),
        source,
        choices,
        error,
    }
}

// =============================================================================
// Round trips and wire tags
// =============================================================================

#[test]
fn client_message_list_model_choices_round_trips() {
    let decoded = ClientMessage::ListModelChoices {
        profile: "p".to_string(),
        request_id: 42,
    };
    let value = as_json(&decoded);
    assert_wire_tag(&value, "list_model_choices");
    let encoded = encode_client_message(&decoded);
    assert_eq!(
        decode_client_message(&encoded).expect("list_model_choices must round trip"),
        decoded
    );
}

#[test]
fn client_message_save_model_aliases_round_trips() {
    let decoded = ClientMessage::SaveModelAliases {
        profile: "p".to_string(),
        aliases: vec![
            LaunchOptionAlias {
                alias: "fast".to_string(),
                value: "m1".to_string(),
            },
            LaunchOptionAlias {
                alias: "thorough".to_string(),
                value: "m2".to_string(),
            },
        ],
    };
    let value = as_json(&decoded);
    assert_wire_tag(&value, "save_model_aliases");
    let encoded = encode_client_message(&decoded);
    assert_eq!(
        decode_client_message(&encoded).expect("save_model_aliases must round trip"),
        decoded
    );
}

#[test]
fn daemon_message_model_choices_requested_round_trips() {
    let decoded = DaemonMessage::ModelChoicesRequested {
        request_id: 42,
        profile: "p".to_string(),
    };
    let value = as_json(&decoded);
    assert_wire_tag(&value, "model_choices_requested");
    let encoded = encode_daemon_message(&decoded);
    assert_eq!(
        decode_daemon_message(&encoded).expect("model_choices_requested must round trip"),
        decoded
    );
}

#[test]
fn daemon_message_model_choices_round_trips_with_a_command_source_and_mixed_choices() {
    let decoded = sample_model_choices(
        Some(ModelChoiceSource::Command {
            argv: vec!["cli".to_string(), "models".to_string()],
        }),
        vec![sample_model_choice(Some("Fast")), sample_model_choice(None)],
        Some("did not finish within 20 seconds".to_string()),
    );
    let value = as_json(&decoded);
    assert_wire_tag(&value, "model_choices");
    let encoded = encode_daemon_message(&decoded);
    assert_eq!(
        decode_daemon_message(&encoded).expect("model_choices must round trip"),
        decoded
    );
}

#[test]
fn daemon_message_model_aliases_saved_round_trips() {
    let decoded = DaemonMessage::ModelAliasesSaved {
        profile: "p".to_string(),
        aliases: vec![LaunchOptionAlias {
            alias: "fast".to_string(),
            value: "m1".to_string(),
        }],
    };
    let value = as_json(&decoded);
    assert_wire_tag(&value, "model_aliases_saved");
    let encoded = encode_daemon_message(&decoded);
    assert_eq!(
        decode_daemon_message(&encoded).expect("model_aliases_saved must round trip"),
        decoded
    );
}

// =============================================================================
// Option fields are absent (not null) from the wire when None, and an
// older-peer frame missing the key decodes to None.
// =============================================================================

#[test]
fn model_choices_source_and_error_none_are_omitted_from_the_wire() {
    let value = as_json(&sample_model_choices(None, vec![], None));
    let object = value
        .as_object()
        .expect("model_choices must serialize as an object");
    assert!(
        !object.contains_key("source"),
        "source: None must omit the key entirely, got: {value}"
    );
    assert!(
        !object.contains_key("error"),
        "error: None must omit the key entirely, got: {value}"
    );
}

#[test]
fn model_choices_without_source_or_error_keys_decodes_to_none_for_both() {
    let body = serde_json::json!({
        "type": "model_choices",
        "request_id": 7,
        "profile": "p",
        "choices": []
    });
    let bytes = serde_json::to_vec(&body).expect("json must encode");
    let decoded = decode_daemon_message(&bytes)
        .expect("a model_choices frame missing source/error (an older peer) must still decode");
    match decoded {
        DaemonMessage::ModelChoices { source, error, .. } => {
            assert_eq!(source, None, "a missing source key must decode to None");
            assert_eq!(error, None, "a missing error key must decode to None");
        }
        other => panic!("expected DaemonMessage::ModelChoices, got {other:?}"),
    }
}

#[test]
fn model_choice_label_none_is_omitted_from_the_wire() {
    let value = as_json(&sample_model_choice(None));
    let object = value
        .as_object()
        .expect("ModelChoice must serialize as an object");
    assert!(
        !object.contains_key("label"),
        "label: None must omit the key entirely, got: {value}"
    );
}

#[test]
fn model_choice_without_a_label_key_decodes_to_none() {
    let body = serde_json::json!({"id": "m1"});
    let decoded: ModelChoice = serde_json::from_value(body)
        .expect("a ModelChoice missing label (an older peer) must still decode");
    assert_eq!(decoded.label, None);
}

// =============================================================================
// ModelChoiceSource's two wire forms, and no catch-all.
// =============================================================================

#[test]
fn model_choice_source_values_serializes_with_only_the_kind_key() {
    let value = as_json(&ModelChoiceSource::Values);
    assert_eq!(value, serde_json::json!({"kind": "values"}));
}

#[test]
fn model_choice_source_command_serializes_with_kind_and_argv() {
    let value = as_json(&ModelChoiceSource::Command {
        argv: vec!["cli".to_string(), "models".to_string()],
    });
    assert_eq!(
        value,
        serde_json::json!({"kind": "command", "argv": ["cli", "models"]})
    );
}

#[test]
fn model_choices_with_an_unrecognized_source_kind_fails_to_decode() {
    let body = serde_json::json!({
        "type": "model_choices",
        "request_id": 7,
        "profile": "p",
        "source": {"kind": "bogus"},
        "choices": [],
    });
    let bytes = serde_json::to_vec(&body).expect("json must encode");
    let result = decode_daemon_message(&bytes);
    assert!(
        result.is_err(),
        "an unrecognized ModelChoiceSource kind must fail the whole frame's decode, got {result:?}"
    );
}

// =============================================================================
// list_model_choices carries a caller-supplied request_id.
// =============================================================================

// =============================================================================
// A malformed or missing request_id fails list_model_choices's decode.
// =============================================================================

#[test]
fn list_model_choices_with_a_malformed_or_missing_request_id_fails_to_decode() {
    for request_id in [
        Some(serde_json::json!(-1)),
        Some(serde_json::json!("5")),
        None,
    ] {
        let mut body = serde_json::json!({
            "type": "list_model_choices",
            "profile": "p",
        });
        if let Some(request_id) = &request_id {
            body.as_object_mut()
                .expect("the frame is an object")
                .insert("request_id".to_string(), request_id.clone());
        }
        let bytes = serde_json::to_vec(&body).expect("json must encode");
        let result = decode_client_message(&bytes);
        assert!(
            result.is_err(),
            "request_id {request_id:?} must fail list_model_choices's decode, got {result:?}"
        );
    }
}
