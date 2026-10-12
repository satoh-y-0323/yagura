//! IPC handler for `set_profile_enabled`: the approver-only switch of a
//! profile's `enabled` key in its user file.

use std::sync::Arc;

use tokio::sync::Semaphore;
use yagura_protocol::DaemonMessage;

use crate::config::DaemonConfig;
use crate::profile_enabled;

use super::profile_job::{ProfileJobNeeds, profile_write_error_msg, run_profile_job};
use super::{ConnectionState, Outbound};

/// Answers `set_profile_enabled`, approver-only: switches a profile's
/// `enabled` key in its user file.
pub(super) async fn handle_set_profile_enabled(
    config: &DaemonConfig,
    permits: &Semaphore,
    outbound: &Arc<Outbound>,
    state: &ConnectionState,
    profile: String,
    enabled: bool,
) {
    let id = profile.clone();
    run_profile_job(
        config,
        permits,
        outbound,
        state,
        &id,
        ProfileJobNeeds::Directory,
        move |config| match profile_enabled::set_profile_enabled(&config, &profile, enabled) {
            Ok(set) => DaemonMessage::ProfileEnabledSet {
                profile,
                enabled: set.enabled,
            },
            Err(err) => profile_write_error_msg(&err),
        },
    )
    .await;
}
