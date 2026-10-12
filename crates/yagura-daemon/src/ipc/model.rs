//! IPC handlers for a profile's model choices: `list_model_choices` and
//! `save_model_aliases`.
//!
//! Listing never blocks the connection that asked for it: the immediate
//! `model_choices_requested` reply only confirms the request was admitted,
//! and the list itself always arrives later as a `model_choices` push
//! carrying the same `request_id` the caller sent, from a detached task that
//! outlives the request that started it.

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::Semaphore;
use yagura_protocol::{DaemonMessage, LaunchOptionAlias, ModelChoice, ModelChoiceSource};

use crate::config::DaemonConfig;
use crate::control::RequestOrigin;
use crate::model_choices::{self, ChoicesFailure};
use crate::profile::{self, ModelChoicesError, ModelChoicesPlan, OutputFormat};
use crate::profile_aliases;
use crate::pty;

use super::plan::{auth_failed_msg, unsupported_msg};
use super::profile_job::{ProfileJobNeeds, profile_write_error_msg, run_profile_job};
use super::{ConnectionState, Outbound, error_msg};

/// [`ModelChoicesError`] and [`ChoicesFailure`] both name "unknown launch
/// profile", "this profile does not map the model option" and an
/// unavailable-listing reason. This is the single place that turns the
/// former into the latter, so every caller of this module formats the same
/// event through one `Display`, never two.
impl From<ModelChoicesError> for ChoicesFailure {
    fn from(err: ModelChoicesError) -> Self {
        match err {
            ModelChoicesError::UnknownProfile => Self::UnknownProfile,
            ModelChoicesError::NoModelOption => Self::NoModelOption,
            ModelChoicesError::Unavailable(reason) => Self::Unavailable(reason),
        }
    }
}

/// Answers `list_model_choices`: an immediate `model_choices_requested` once
/// a listing slot is admitted, then a `model_choices` push once the
/// profile's plan is resolved and -- for a `Command` plan -- its listing
/// command has run to completion or failed.
///
/// Everything past admission runs detached from this connection's own task
/// (`tokio::spawn`), so one slow listing never delays any other request on
/// the same connection. A connection that closes before the push is ready
/// simply never receives it: [`Outbound::reply`] on a broken connection only
/// marks it so.
pub(super) async fn handle_list_model_choices(
    config: &DaemonConfig,
    listing_permits: &Arc<Semaphore>,
    outbound: &Arc<Outbound>,
    state: &ConnectionState,
    profile: String,
    request_id: u64,
) {
    if !matches!(state.origin, RequestOrigin::Approver) {
        outbound.reply(&auth_failed_msg());
        return;
    }
    if !profile::has_profile_source(config) {
        outbound.reply(&unsupported_msg());
        return;
    }
    if !profile::validate_profile_id(&profile) {
        outbound.reply(&error_msg("invalid_argument", "profile id is invalid"));
        return;
    }
    let Ok(permit) = Arc::clone(listing_permits).try_acquire_owned() else {
        outbound.reply(&error_msg(
            "resource_exhausted",
            "too many model choices listings are already running",
        ));
        return;
    };

    outbound.reply(&DaemonMessage::ModelChoicesRequested {
        request_id,
        profile: profile.clone(),
    });

    let config = config.clone();
    let outbound = Arc::clone(outbound);
    tokio::spawn(async move {
        let _permit = permit;
        let (source, choices, error) = list_choices(&config, &profile).await;
        outbound.reply(&DaemonMessage::ModelChoices {
            request_id,
            profile,
            source,
            choices,
            error: error.map(|err| err.to_string()),
        });
    });
}

/// Resolves `profile`'s model choices plan and, for a `Command` plan, runs
/// its listing command: the whole of what a `model_choices` push reports,
/// short of the `request_id` [`handle_list_model_choices`] wraps it in.
async fn list_choices(
    config: &DaemonConfig,
    profile: &str,
) -> (
    Option<ModelChoiceSource>,
    Vec<ModelChoice>,
    Option<ChoicesFailure>,
) {
    match resolve_plan_blocking(config, profile).await {
        Ok(ModelChoicesPlan::NoChoices) => (None, Vec::new(), None),
        Ok(ModelChoicesPlan::Values(values)) => (
            Some(ModelChoiceSource::Values),
            values
                .into_iter()
                .map(|id| ModelChoice { id, label: None })
                .collect(),
            None,
        ),
        Ok(ModelChoicesPlan::Command {
            argv,
            spawn_argv,
            launch_argv,
            env,
            cwd,
            format,
        }) => {
            let source = Some(ModelChoiceSource::Command { argv: argv.clone() });
            match run_listing_command(
                config,
                profile,
                spawn_argv,
                launch_argv,
                argv,
                env,
                cwd,
                format,
            )
            .await
            {
                Ok(choices) => (source, choices, None),
                Err(failure) => (source, Vec::new(), Some(failure)),
            }
        }
        Err(err) => {
            let failure = ChoicesFailure::from(err);
            tracing::debug!(
                profile = %profile,
                reason = failure_kind(&failure),
                "model choices plan could not be resolved"
            );
            (None, Vec::new(), Some(failure))
        }
    }
}

/// Resolves `profile`'s plan on a blocking thread: reading a profile file is
/// blocking I/O that must never run on a request's own async task.
async fn resolve_plan_blocking(
    config: &DaemonConfig,
    profile: &str,
) -> Result<ModelChoicesPlan, ModelChoicesError> {
    let config = config.clone();
    let profile = profile.to_string();
    tokio::task::spawn_blocking(move || profile::resolve_model_choices(&config, &profile))
        .await
        // Unreachable in practice (the closure above does not panic);
        // reported the same way an unstartable listing is, rather than
        // propagating a join error through a type that carries none.
        .unwrap_or_else(|_join_err| {
            Err(ModelChoicesError::Unavailable(
                "the listing task ended unexpectedly".to_string(),
            ))
        })
}

/// Runs a `Command` plan's listing command ([`pty::spawn_one_shot`]), reads
/// its output within `config`'s limits, and parses it as `format`. `argv` is
/// the plan's unresolved argv (what a spawn failure's reason names);
/// `launch_argv` is that argv without the listing's own arguments (what a 127
/// exit's reason names). `env` is the
/// profile's own `[env]` table, layered over the daemon's environment and
/// `config`'s base environment; unlike a window's child, a listing gets no
/// variables from the Windows registry.
#[allow(clippy::too_many_arguments)]
async fn run_listing_command(
    config: &DaemonConfig,
    profile: &str,
    spawn_argv: Vec<String>,
    launch_argv: Vec<String>,
    argv: Vec<String>,
    env: Vec<(String, String)>,
    cwd: Option<PathBuf>,
    format: OutputFormat,
) -> Result<Vec<ModelChoice>, ChoicesFailure> {
    let overlay: Vec<(OsString, OsString)> = env
        .into_iter()
        .map(|(name, value)| (OsString::from(name), OsString::from(value)))
        .collect();
    let captured = match pty::spawn_one_shot(
        spawn_argv,
        cwd,
        pty::CaptureChildEnv::Based {
            base: config.child_base_env.clone(),
            overlay,
        },
        None,
    )
    .await
    {
        Ok(captured) => captured,
        Err(err) => return Err(model_choices::failure_for_spawn_error(&err, &argv)),
    };
    let limits = yagura_os::CaptureLimits {
        timeout: config.model_choices_timeout,
        max_output_bytes: config.model_choices_max_output_bytes,
    };
    let outcome = tokio::task::spawn_blocking(move || captured.collect(limits))
        .await
        // Unreachable in practice (`collect` does not panic); treated as "no
        // exit code was ever reported" rather than propagating a join error
        // through a type that carries none.
        .unwrap_or(yagura_os::CaptureOutcome::Exited {
            code: None,
            stdout: Vec::new(),
            stderr: Vec::new(),
        });
    match outcome {
        yagura_os::CaptureOutcome::Exited { code, stdout, .. } => {
            tracing::debug!(
                profile = %profile,
                exit_code = ?code,
                stdout_bytes = stdout.len(),
                "model choices command exited"
            );
            match code {
                Some(0) => model_choices::parse_choices(&format, &stdout),
                _ => Err(model_choices::failure_for_exit(code, &launch_argv)),
            }
        }
        yagura_os::CaptureOutcome::TimedOut => Err(ChoicesFailure::TimedOut {
            secs: config.model_choices_timeout.as_secs(),
        }),
        yagura_os::CaptureOutcome::OutputTooLarge => Err(ChoicesFailure::OutputTooLarge),
    }
}

/// A short, path-free label for `failure`'s kind, logged in place of its full
/// text so a wording change can never silently change what the log records.
fn failure_kind(failure: &ChoicesFailure) -> &'static str {
    match failure {
        ChoicesFailure::NotFound { .. } => "not_found",
        ChoicesFailure::Unavailable(_) => "unavailable",
        ChoicesFailure::TimedOut { .. } => "timed_out",
        ChoicesFailure::ExitStatus(_) => "exit_status",
        ChoicesFailure::ExitedWithoutCode => "exited_without_code",
        ChoicesFailure::OutputTooLarge => "output_too_large",
        ChoicesFailure::FormatMismatch => "format_mismatch",
        ChoicesFailure::TooMany => "too_many",
        ChoicesFailure::UnknownProfile => "unknown_profile",
        ChoicesFailure::NoModelOption => "no_model_option",
    }
}

/// Answers `save_model_aliases`, approver-only: replaces a profile's alias
/// values in its user file.
pub(super) async fn handle_save_model_aliases(
    config: &DaemonConfig,
    permits: &Semaphore,
    outbound: &Arc<Outbound>,
    state: &ConnectionState,
    profile: String,
    aliases: Vec<LaunchOptionAlias>,
) {
    let pairs: Vec<(String, String)> = aliases
        .into_iter()
        .map(|alias| (alias.alias, alias.value))
        .collect();
    let id = profile.clone();
    run_profile_job(
        config,
        permits,
        outbound,
        state,
        &id,
        ProfileJobNeeds::Directory,
        move |config| match profile_aliases::save_model_aliases(&config, &profile, &pairs) {
            Ok(saved) => DaemonMessage::ModelAliasesSaved {
                profile,
                aliases: saved
                    .aliases
                    .into_iter()
                    .map(|(alias, value)| LaunchOptionAlias { alias, value })
                    .collect(),
            },
            Err(err) => profile_write_error_msg(&err),
        },
    )
    .await;
}
