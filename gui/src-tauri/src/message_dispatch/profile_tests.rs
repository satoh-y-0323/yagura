//! How `reply_for` classifies the profile-enabled daemon messages.

use yagura_protocol::DaemonMessage;

use crate::ipc_client::{PendingKind, Reply};

use super::reply_for;

#[test]
fn reply_for_classifies_profile_enabled_set_as_a_set_profile_enabled_reply() {
    let result = reply_for(DaemonMessage::ProfileEnabledSet {
        profile: "claude-code".to_owned(),
        enabled: false,
    });
    match result {
        Some((PendingKind::SetProfileEnabled, Reply::ProfileEnabledSet(got))) => {
            assert_eq!(got.profile, "claude-code");
            assert!(!got.enabled, "the disabled state must be carried as sent");
        }
        other => panic!("expected a ProfileEnabledSet reply, got {other:?}"),
    }
}

#[test]
fn reply_for_carries_an_enabled_true_profile_enabled_set_reply() {
    let result = reply_for(DaemonMessage::ProfileEnabledSet {
        profile: "shell".to_owned(),
        enabled: true,
    });
    match result {
        Some((PendingKind::SetProfileEnabled, Reply::ProfileEnabledSet(got))) => {
            assert_eq!(got.profile, "shell");
            assert!(got.enabled, "the enabled state must be carried as sent");
        }
        other => panic!("expected a ProfileEnabledSet reply, got {other:?}"),
    }
}
