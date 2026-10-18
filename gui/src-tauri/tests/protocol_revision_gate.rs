#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The pure gate that decides whether a frame may be written to a connection
//! at the wire revision that connection has settled on, and the vocabulary
//! that carries the gate's refusal out to the window-open form.
//!
//! Every test below drives pure items directly ([`protocol_revision_gate`],
//! [`window_open_failure`]). None of them builds an `AppHandle`, a `State` or
//! a live connection, so the decision is measured on its four inputs alone:
//! the revision this connection settled on, the generation that settling
//! belongs to, the generation the connection is on now, and the revision the
//! message being written requires.

use yagura_gui::commands::{WindowOpenFailure, window_open_failure};
use yagura_gui::ipc_client::IpcError;
use yagura_gui::pool_outcomes::{ProtocolRevisionGate, protocol_revision_gate};
use yagura_protocol::{
    ClientMessage, LAUNCH_OPTIONS_MIN_REVISION, PROTOCOL_REVISION_BASELINE,
    client_message_wire_tags, required_protocol_revision,
};

/// The generation a connection is on in every test that is not about a
/// generation mismatch. Any value other than zero, so a gate reading a
/// default-initialized counter instead of the one it was handed cannot meet
/// these expectations by accident.
const CURRENT_GENERATION: u64 = 7;

/// An `open_window_from_profile` request that carries no launch option.
fn plain_request() -> ClientMessage {
    ClientMessage::OpenWindowFromProfile {
        profile: "claude".to_owned(),
        cwd: None,
        name: None,
        model: None,
        effort: None,
        on_name_taken: Default::default(),
    }
}

/// The same request with a `model` launch option, which is what raises the
/// wire revision it requires.
fn launch_option_request() -> ClientMessage {
    ClientMessage::OpenWindowFromProfile {
        profile: "claude".to_owned(),
        cwd: None,
        name: None,
        model: Some("opus".to_owned()),
        effort: None,
        on_name_taken: Default::default(),
    }
}

/// The wire tag of the request the gate refuses below, read back from the
/// protocol crate's own table rather than introduced as a new word here.
fn refused_request_tag() -> &'static str {
    client_message_wire_tags()
        .iter()
        .copied()
        .find(|tag| *tag == "open_window_from_profile")
        .expect("the refused request's tag must be one the protocol already names")
}

// ---------------------------------------------------------------------
// The gate itself: one decision taken from four values.
// ---------------------------------------------------------------------

#[test]
fn the_requirement_the_gate_weighs_comes_from_the_message_being_written() {
    let settled_at_baseline = |message: &ClientMessage| {
        protocol_revision_gate(
            Some(PROTOCOL_REVISION_BASELINE),
            CURRENT_GENERATION,
            CURRENT_GENERATION,
            required_protocol_revision(message),
        )
    };
    assert_eq!(
        settled_at_baseline(&plain_request()),
        ProtocolRevisionGate::Carried,
        "a request that needs nothing beyond the baseline must go out to a baseline peer"
    );
    assert_eq!(
        settled_at_baseline(&launch_option_request()),
        ProtocolRevisionGate::Refused,
        "the same request with a launch option must not go out to a baseline peer"
    );
}

#[test]
fn a_settled_revision_that_meets_the_requirement_lets_the_frame_out() {
    assert_eq!(
        protocol_revision_gate(
            Some(LAUNCH_OPTIONS_MIN_REVISION),
            CURRENT_GENERATION,
            CURRENT_GENERATION,
            LAUNCH_OPTIONS_MIN_REVISION,
        ),
        ProtocolRevisionGate::Carried
    );
}

#[test]
fn a_settled_revision_above_the_requirement_lets_the_frame_out() {
    assert_eq!(
        protocol_revision_gate(
            Some(LAUNCH_OPTIONS_MIN_REVISION + 1),
            CURRENT_GENERATION,
            CURRENT_GENERATION,
            LAUNCH_OPTIONS_MIN_REVISION,
        ),
        ProtocolRevisionGate::Carried
    );
}

#[test]
fn a_settled_revision_below_the_requirement_refuses_the_frame() {
    assert_eq!(
        protocol_revision_gate(
            Some(PROTOCOL_REVISION_BASELINE),
            CURRENT_GENERATION,
            CURRENT_GENERATION,
            LAUNCH_OPTIONS_MIN_REVISION,
        ),
        ProtocolRevisionGate::Refused
    );
}

#[test]
fn an_unsettled_connection_refuses_a_frame_that_needs_a_revision() {
    assert_eq!(
        protocol_revision_gate(
            None,
            CURRENT_GENERATION,
            CURRENT_GENERATION,
            LAUNCH_OPTIONS_MIN_REVISION,
        ),
        ProtocolRevisionGate::Refused,
        "an unsettled connection must be weighed as the weakest peer, not the strongest"
    );
}

#[test]
fn an_unsettled_connection_still_lets_a_frame_with_no_requirement_out() {
    assert_eq!(
        protocol_revision_gate(
            None,
            CURRENT_GENERATION,
            CURRENT_GENERATION,
            PROTOCOL_REVISION_BASELINE,
        ),
        ProtocolRevisionGate::Carried,
        "the round trip that settles the revision asks for nothing beyond the baseline, so \
         refusing here would leave every connection unsettled forever"
    );
}

#[test]
fn a_revision_settled_for_another_generation_refuses_the_frame() {
    assert_eq!(
        protocol_revision_gate(
            Some(LAUNCH_OPTIONS_MIN_REVISION),
            CURRENT_GENERATION - 1,
            CURRENT_GENERATION,
            LAUNCH_OPTIONS_MIN_REVISION,
        ),
        ProtocolRevisionGate::Refused,
        "a revision learned from a connection that has since been replaced is not a fact \
         about the connection being written to"
    );
}

#[test]
fn a_revision_settled_for_another_generation_still_lets_a_frame_with_no_requirement_out() {
    assert_eq!(
        protocol_revision_gate(
            Some(LAUNCH_OPTIONS_MIN_REVISION),
            CURRENT_GENERATION - 1,
            CURRENT_GENERATION,
            PROTOCOL_REVISION_BASELINE,
        ),
        ProtocolRevisionGate::Carried
    );
}

// ---------------------------------------------------------------------
// The vocabulary that carries the refusal out to the window-open form.
// ---------------------------------------------------------------------

#[test]
fn a_gate_refusal_reaches_the_form_as_its_own_kind_of_failure() {
    assert_eq!(
        window_open_failure(IpcError::Unsupported {
            request_type: refused_request_tag().to_owned(),
        }),
        WindowOpenFailure::Unsupported {
            request_type: refused_request_tag().to_owned(),
        }
    );
}

#[test]
fn a_gate_refusal_does_not_fall_through_to_the_generic_local_failure() {
    let failure = window_open_failure(IpcError::Unsupported {
        request_type: refused_request_tag().to_owned(),
    });
    assert!(
        !matches!(failure, WindowOpenFailure::Local { .. }),
        "falling through to the local failure would put this layer's own English text in \
         front of the user: {failure:?}"
    );
}

#[test]
fn a_gate_refusal_carries_the_wire_tag_of_the_request_that_was_refused() {
    let failure = window_open_failure(IpcError::Unsupported {
        request_type: refused_request_tag().to_owned(),
    });
    let WindowOpenFailure::Unsupported { request_type } = failure else {
        panic!("a gate refusal must keep its own kind: {failure:?}");
    };
    assert!(
        client_message_wire_tags().contains(&request_type.as_str()),
        "the refusal must name the request with a tag the protocol already has, not a word \
         invented here: {request_type}"
    );
}

#[test]
fn a_serialized_gate_refusal_carries_exactly_what_it_was_given() {
    let serialized = serde_json::to_value(window_open_failure(IpcError::Unsupported {
        request_type: refused_request_tag().to_owned(),
    }))
    .unwrap();
    assert_eq!(
        serialized,
        serde_json::json!({
            "kind": "unsupported",
            "request_type": "open_window_from_profile",
        })
    );
}
