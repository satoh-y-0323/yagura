//! How `reply_for` classifies the model-alias daemon messages.
//!
//! `model_choices` is a push sent to the requesting connection asynchronously
//! rather than a reply the connection is waiting on with a `Pending` slot, so
//! it must complete no request of this client.

use yagura_protocol::{DaemonMessage, LaunchOptionAlias};

use crate::commands::{ModelAliasesSavedReply, ModelChoicesRequestedReply};
use crate::ipc_client::{PendingKind, Reply};

use super::reply_for;

fn sample_alias() -> LaunchOptionAlias {
    LaunchOptionAlias {
        alias: "fast".to_owned(),
        value: "haiku".to_owned(),
    }
}

#[test]
fn reply_for_classifies_model_choices_requested_as_a_list_model_choices_reply() {
    let result = reply_for(DaemonMessage::ModelChoicesRequested {
        request_id: 7,
        profile: "claude-code".to_owned(),
    });
    match result {
        Some((PendingKind::ListModelChoices, Reply::ModelChoicesRequested(got))) => {
            let got: ModelChoicesRequestedReply = got;
            assert_eq!(got.request_id, 7);
            assert_eq!(got.profile, "claude-code");
        }
        other => panic!("expected a ModelChoicesRequested reply, got {other:?}"),
    }
}

#[test]
fn reply_for_classifies_model_aliases_saved_as_a_save_model_aliases_reply() {
    let aliases = vec![sample_alias()];
    let result = reply_for(DaemonMessage::ModelAliasesSaved {
        profile: "claude-code".to_owned(),
        aliases: aliases.clone(),
    });
    match result {
        Some((PendingKind::SaveModelAliases, Reply::ModelAliasesSaved(got))) => {
            let got: ModelAliasesSavedReply = got;
            assert_eq!(got.profile, "claude-code");
            assert_eq!(got.aliases, aliases);
        }
        other => panic!("expected a ModelAliasesSaved reply, got {other:?}"),
    }
}

#[test]
fn reply_for_classifies_model_choices_as_answering_no_request_of_this_client() {
    let result = reply_for(DaemonMessage::ModelChoices {
        request_id: 7,
        profile: "claude-code".to_owned(),
        source: None,
        choices: Vec::new(),
        error: None,
    });
    assert!(result.is_none());
}
