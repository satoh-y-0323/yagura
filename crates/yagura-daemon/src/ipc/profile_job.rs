//! The one shape every approver-only profile request takes: refuse anyone
//! but the approver, a daemon with nothing to read or write, and an invalid
//! id; then run the blocking job, admitted by the shared permits, and reply
//! with what it answers.

use std::sync::Arc;

use tokio::sync::Semaphore;
use yagura_protocol::DaemonMessage;

use crate::config::DaemonConfig;
use crate::control::RequestOrigin;
use crate::profile::{self, validate_profile_id};
use crate::profile_rewrite::ProfileWriteError;

use super::plan::{auth_failed_msg, internal_msg, unsupported_msg};
use super::{ConnectionState, Outbound, error_msg};

/// What a profile request needs from the daemon's configuration.
#[derive(Debug, Clone, Copy)]
pub(super) enum ProfileJobNeeds {
    /// Any source of profiles: a directory, or the shipped profiles alone.
    AnySource,
    /// A profiles directory to write the user's file into.
    Directory,
}

/// Answers one approver-only request about `profile`: after the checks every
/// such request shares, runs `job` on a blocking thread admitted by
/// `permits`, and replies with the message it returns. A job that panics
/// answers `internal`.
pub(super) async fn run_profile_job(
    config: &DaemonConfig,
    permits: &Semaphore,
    outbound: &Arc<Outbound>,
    state: &ConnectionState,
    profile: &str,
    needs: ProfileJobNeeds,
    job: impl FnOnce(DaemonConfig) -> DaemonMessage + Send + 'static,
) {
    if !matches!(state.origin, RequestOrigin::Approver) {
        outbound.reply(&auth_failed_msg());
        return;
    }
    let has_what_it_needs = match needs {
        ProfileJobNeeds::AnySource => profile::has_profile_source(config),
        ProfileJobNeeds::Directory => config.profiles_dir.is_some(),
    };
    if !has_what_it_needs {
        outbound.reply(&unsupported_msg());
        return;
    }
    if !validate_profile_id(profile) {
        outbound.reply(&error_msg("invalid_argument", "profile id is invalid"));
        return;
    }
    let Ok(_permit) = permits.acquire().await else {
        outbound.reply(&internal_msg());
        return;
    };

    let config = config.clone();
    match tokio::task::spawn_blocking(move || job(config)).await {
        Ok(reply) => outbound.reply(&reply),
        Err(join_err) => {
            tracing::warn!(error = %join_err, "profile request task failed");
            outbound.reply(&internal_msg());
        }
    }
}

/// Every [`ProfileWriteError`] as the fixed wire reply it becomes: the
/// request's own fault (`invalid_argument`) or this daemon's (`internal`,
/// logged with the detail the wire reply never carries).
pub(super) fn profile_write_error_msg<R: std::error::Error + 'static>(
    err: &ProfileWriteError<R>,
) -> DaemonMessage {
    match err {
        ProfileWriteError::UnknownProfile | ProfileWriteError::Refused(_) => {
            error_msg("invalid_argument", &err.to_string())
        }
        ProfileWriteError::RewriteFailed | ProfileWriteError::Io(_) => {
            tracing::warn!(error = %err, "profile file could not be rewritten");
            internal_msg()
        }
    }
}
