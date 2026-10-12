use yagura_protocol::{
    ClientMessage, ControlKey, DaemonMessage, SEND_KEYS_MAX_KEYS, SendKeysError,
    decode_client_message, required_protocol_revision, validate_control_keys,
};

#[test]
fn key_sequence_validation_rejects_empty_unknown_and_overlong_input() {
    assert_eq!(validate_control_keys(&[]), Err(SendKeysError::Empty));
    assert_eq!(
        validate_control_keys(&[ControlKey::Unknown]),
        Err(SendKeysError::Unknown)
    );
    assert_eq!(
        validate_control_keys(&[ControlKey::Enter; 17]),
        Err(SendKeysError::TooMany)
    );
}

#[test]
fn a_sequence_of_exactly_the_key_limit_is_accepted() {
    assert_eq!(SEND_KEYS_MAX_KEYS, 16);
    assert_eq!(
        validate_control_keys(&[ControlKey::Enter; SEND_KEYS_MAX_KEYS]),
        Ok(())
    );
}

#[test]
fn every_control_key_wire_name_is_its_serde_spelling() {
    // The daemon writes wire_name into approval text and error replies, so it
    // must name a key exactly as send_keys carries it.
    for key in [
        ControlKey::Escape,
        ControlKey::Backspace,
        ControlKey::CtrlU,
        ControlKey::Enter,
        ControlKey::Up,
        ControlKey::Down,
        ControlKey::Unknown,
    ] {
        let wire = serde_json::json!(key.wire_name());
        assert_eq!(serde_json::to_value(key).expect("key serializes"), wire);
        assert_eq!(
            serde_json::from_value::<ControlKey>(wire).expect("key decodes"),
            key
        );
    }
}

#[test]
fn escape_is_allowed_only_as_the_last_key() {
    assert_eq!(
        validate_control_keys(&[ControlKey::Escape, ControlKey::Enter]),
        Err(SendKeysError::EscapeMustBeLast)
    );
    assert_eq!(
        validate_control_keys(&[ControlKey::Enter, ControlKey::Escape]),
        Ok(())
    );
}

#[test]
fn decode_send_keys_maps_an_unknown_wire_name_to_the_unknown_key() {
    let decoded = decode_client_message(br#"{"type":"send_keys","window_id":"w1","keys":["tab"]}"#)
        .expect("an unknown key name must remain a decodable send_keys message");
    assert_eq!(
        decoded,
        ClientMessage::SendKeys {
            window_id: "w1".to_string(),
            keys: vec![ControlKey::Unknown],
        }
    );
}

#[test]
fn send_keys_wire_round_trip_and_revision_are_stable() {
    let message = ClientMessage::SendKeys {
        window_id: "w1".to_string(),
        keys: vec![ControlKey::Escape, ControlKey::CtrlU],
    };
    let encoded = serde_json::to_value(&message).expect("message serializes");
    assert_eq!(encoded["type"], "send_keys");
    assert_eq!(encoded["keys"], serde_json::json!(["escape", "ctrl_u"]));
    assert_eq!(required_protocol_revision(&message), 6);
    assert_eq!(
        serde_json::from_value::<ClientMessage>(encoded).expect("message decodes"),
        message
    );

    let response = serde_json::to_value(DaemonMessage::KeysSent {
        window_id: "w1".to_string(),
    })
    .expect("response serializes");
    assert_eq!(response["type"], "keys_sent");
}
