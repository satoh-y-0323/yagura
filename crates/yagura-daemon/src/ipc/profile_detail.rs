//! IPC handlers for `get_profile_detail` and `set_profile_command`: the
//! approver-only view of every value a profile uses, and the host's launch
//! argv in its user file.

use std::sync::Arc;

use tokio::sync::Semaphore;
use yagura_protocol::DaemonMessage;

use crate::config::DaemonConfig;
use crate::profile_command;
use crate::profile_detail::{self, ProfileDetailError};

use super::profile_job::{ProfileJobNeeds, profile_write_error_msg, run_profile_job};
use super::{ConnectionState, Outbound, error_msg};

/// Answers `get_profile_detail`, approver-only since the values include the
/// profile's environment.
pub(super) async fn handle_get_profile_detail(
    config: &DaemonConfig,
    permits: &Semaphore,
    outbound: &Arc<Outbound>,
    state: &ConnectionState,
    profile: String,
) {
    let id = profile.clone();
    run_profile_job(
        config,
        permits,
        outbound,
        state,
        &id,
        ProfileJobNeeds::AnySource,
        move |config| match profile_detail::read_profile_detail(&config, &profile) {
            Ok(detail) => DaemonMessage::ProfileDetail {
                profile,
                shipped_base: detail.shipped_base,
                command: detail.command,
                entries: detail.entries,
            },
            Err(err @ ProfileDetailError::UnknownProfile) => {
                error_msg("invalid_argument", &err.to_string())
            }
        },
    )
    .await;
}

/// Answers `set_profile_command`, approver-only: writes the host's argv into
/// the profile's user file.
pub(super) async fn handle_set_profile_command(
    config: &DaemonConfig,
    permits: &Semaphore,
    outbound: &Arc<Outbound>,
    state: &ConnectionState,
    profile: String,
    argv: Option<Vec<String>>,
) {
    let id = profile.clone();
    run_profile_job(
        config,
        permits,
        outbound,
        state,
        &id,
        ProfileJobNeeds::Directory,
        move |config| match profile_command::set_profile_command(&config, &profile, argv.as_deref())
        {
            Ok(command) => DaemonMessage::ProfileCommandSet { profile, command },
            Err(err) => profile_write_error_msg(&err),
        },
    )
    .await;
}
