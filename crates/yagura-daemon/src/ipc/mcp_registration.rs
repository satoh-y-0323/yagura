//! IPC handlers for `list_mcp_registrations` (one row per profile that
//! describes how to register the daemon's own MCP bridge with its CLI, plus
//! one [`DaemonMessage::McpRegistrationStatus`] push per row read from the
//! CLI itself) and `run_mcp_registration` (registers, updates or removes one
//! profile's registration, pushing one
//! [`DaemonMessage::McpRegistrationResult`] when it finishes). Every
//! registration child is started through [`pty::spawn_one_shot`],
//! the same one-shot capture a window's model-choices listing uses.

use std::ffi::OsString;
use std::path::Path;
use std::sync::Arc;

use tokio::sync::Semaphore;
use yagura_protocol::{
    DaemonMessage, McpRegistrationAction, McpRegistrationCommand, McpRegistrationHookEntry,
    McpRegistrationRow, McpRegistrationState, McpRegistrationStatusInfo, McpRegistrationStep,
    McpRegistrationTomlSet, McpRegistrationTomlValue,
};

use crate::config::{DaemonConfig, REPORT_EXE_NAME};
use crate::control::RequestOrigin;
use crate::json_entry;
use crate::mcp_registration;
use crate::profile;
use crate::pty;
use crate::toml_set;

use super::plan::{auth_failed_msg, load_role_and_profile_state_blocking, unsupported_msg};
use super::{ConnectionState, Outbound, error_msg};

/// One row's registration table, together with everything a later state read
/// needs, kept apart from the wire row so a background read never re-parses
/// the profile it came from.
struct RowSource {
    profile: String,
    spec: mcp_registration::McpRegistrationSpec,
    target: mcp_registration::RegistrationTarget,
    launch_argv: Vec<String>,
    env: Vec<(OsString, OsString)>,
}

/// What [`build_listing`] (a blocking, filesystem-reading pass) produces: the
/// immediate wire answer, plus what each row's later state read needs.
struct ListingBuilt {
    bridge: Option<String>,
    rows: Vec<McpRegistrationRow>,
    sources: Vec<RowSource>,
}

/// Answers `list_mcp_registrations`: an immediate `mcp_registrations_listed`
/// once a listing slot is admitted, then one `mcp_registration_status` push
/// per row, from a detached task that outlives the request that started it
/// (the same shape as [`super::model::handle_list_model_choices`]).
///
/// The listing slot (`listing_permits`, capacity 1) is held for as long as
/// this listing's own row reads take, so at most one listing runs across the
/// whole daemon at a time; a row read is separately admitted by
/// `row_permits` (sized by [`DaemonConfig::mcp_registration_status_max_concurrent`]).
pub(super) async fn handle_list_mcp_registrations(
    config: &DaemonConfig,
    role_and_profile_reads: &Semaphore,
    listing_permits: &Arc<Semaphore>,
    row_permits: &Arc<Semaphore>,
    outbound: &Arc<Outbound>,
    state: &ConnectionState,
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
    let Ok(permit) = Arc::clone(listing_permits).try_acquire_owned() else {
        outbound.reply(&error_msg(
            "resource_exhausted",
            "another mcp registration listing is already running",
        ));
        return;
    };

    let built = build_listing(config, role_and_profile_reads).await;

    outbound.reply(&DaemonMessage::McpRegistrationsListed {
        request_id,
        bridge: built.bridge.clone(),
        rows: built.rows,
    });

    // No bridge beside this daemon: no state to read, and the permit is
    // dropped at once, freeing the slot immediately rather than holding it
    // for nothing.
    let Some(bridge) = built.bridge else {
        return;
    };

    let config = config.clone();
    let outbound = Arc::clone(outbound);
    let row_permits = Arc::clone(row_permits);
    tokio::spawn(async move {
        // Held until every row of this listing has been read.
        let _permit = permit;
        let mut tasks = tokio::task::JoinSet::new();
        for source in built.sources {
            let row_permits = Arc::clone(&row_permits);
            let config = config.clone();
            let outbound = Arc::clone(&outbound);
            let bridge = bridge.clone();
            tasks.spawn(async move {
                // A row read waits for a slot rather than being dropped: a
                // listing with more rows than the limit still pushes every
                // one of them, just not all at once.
                let Ok(_row_permit) = row_permits.acquire_owned().await else {
                    return;
                };
                let status = compute_status(&config, &source, &bridge).await;
                outbound.reply(&DaemonMessage::McpRegistrationStatus { request_id, status });
            });
        }
        while tasks.join_next().await.is_some() {}
    });
}

/// Reads the profiles directory and builds every row this host can register,
/// in the profile list's own order.
///
/// The profile listing itself is read through
/// [`load_role_and_profile_state_blocking`] -- the one function daemon
/// `src/**` may call `profile::list_profiles` from
/// (`disallowed-methods` in `clippy.toml`) -- admitted by `role_and_profile_reads`; the
/// per-row read that follows (each row's own `[mcp_registration]` table) is a
/// second, unrelated pass of blocking file reads and runs on its own
/// blocking thread.
async fn build_listing(config: &DaemonConfig, role_and_profile_reads: &Semaphore) -> ListingBuilt {
    let bridge = config
        .mcp_bridge
        .as_ref()
        .and_then(|path| path.to_str())
        .map(str::to_string);
    let (_roles, profile_listing) =
        load_role_and_profile_state_blocking(config, role_and_profile_reads).await;

    let config = config.clone();
    let bridge_for_rows = bridge.clone();
    let (rows, sources) = tokio::task::spawn_blocking(move || {
        build_rows(&config, profile_listing, bridge_for_rows.as_deref())
    })
    .await
    // Unreachable in practice (`build_rows` does not panic); an empty
    // listing is reported rather than propagating a join error through a
    // type that carries none.
    .unwrap_or_default();

    ListingBuilt {
        bridge,
        rows,
        sources,
    }
}

/// Builds every row `profile_listing` carries a usable
/// [`mcp_registration::McpRegistrationSpec`] for. Blocking: run this off an
/// async runtime.
fn build_rows(
    config: &DaemonConfig,
    profile_listing: profile::ProfileListing,
    bridge: Option<&str>,
) -> (Vec<McpRegistrationRow>, Vec<RowSource>) {
    let target = mcp_registration::RegistrationTarget::for_instance(config.instance_id.as_deref());

    let mut rows = Vec::new();
    let mut sources = Vec::new();
    for entry in profile_listing.profiles {
        let Some((spec, env)) = row_essentials(config, &entry.id) else {
            continue;
        };

        // A probed row's bridge is not known before its probe runs, so the
        // listing keeps the `{bridge}` placeholder even when this daemon's
        // own bridge is known.
        let bridge_for_listing = if spec.has_bridge_probe() {
            None
        } else {
            bridge
        };
        let (add, remove) = row_commands(&spec, &target, &entry.command, bridge_for_listing);
        let toml_set = spec
            .toml_set()
            .map(|toml_set| to_wire_toml_set(toml_set, &target));
        let hook_entries = hook_entries_preview(config, &spec, &target);

        rows.push(McpRegistrationRow {
            profile: entry.id.clone(),
            display_name: entry.display_name.clone(),
            group: spec.group().map(str::to_string),
            add,
            remove,
            toml_set,
            hook_entries,
        });

        sources.push(RowSource {
            profile: entry.id,
            spec,
            target: target.clone(),
            launch_argv: entry.command,
            env,
        });
    }

    (rows, sources)
}

/// Reads `profile_id`'s file again (the listing pass that produced its
/// [`profile::ProfileEntry`] does not keep the parsed [`profile::Profile`])
/// and resolves its registration table and environment -- the two pieces
/// both [`build_rows`] and [`resolve_row_source`] need beyond what
/// [`profile::list_profiles`] already gave them. `None` when the profile
/// cannot be read again, or resolves to no usable
/// [`mcp_registration::McpRegistrationSpec`].
fn row_essentials(
    config: &DaemonConfig,
    profile_id: &str,
) -> Option<(
    mcp_registration::McpRegistrationSpec,
    Vec<(OsString, OsString)>,
)> {
    let listed = profile::read_listed_profile(config, profile_id)?;
    let spec = listed.profile.mcp_registration.clone()?;
    let env = listed
        .profile
        .env
        .iter()
        .map(|(key, value)| (OsString::from(key), OsString::from(value)))
        .collect();
    Some((spec, env))
}

/// Finds and builds the one row `profile_id` names, the same way each of
/// [`build_rows`]'s rows is. `None` when [`profile::list_profiles`] does not
/// report the id (unknown, disabled by its own character set, or excluded
/// for this host) or the profile resolves to no usable registration table;
/// the caller answers either case alike (`invalid_argument`), rather than
/// letting a caller distinguish "no such profile" from "no registration
/// table" over an otherwise-unauthenticated-feeling channel.
async fn resolve_row_source(
    config: &DaemonConfig,
    role_and_profile_reads: &Semaphore,
    profile_id: &str,
) -> Option<RowSource> {
    let target = mcp_registration::RegistrationTarget::for_instance(config.instance_id.as_deref());
    let (_roles, profile_listing) =
        load_role_and_profile_state_blocking(config, role_and_profile_reads).await;
    let config = config.clone();
    let profile_id = profile_id.to_string();
    tokio::task::spawn_blocking(move || {
        let entry = profile_listing
            .profiles
            .into_iter()
            .find(|entry| entry.id == profile_id)?;
        let (spec, env) = row_essentials(&config, &entry.id)?;
        Some(RowSource {
            profile: entry.id,
            spec,
            target,
            launch_argv: entry.command,
            env,
        })
    })
    .await
    // Unreachable in practice (the closure above does not panic); treated
    // as "no such row" rather than propagating a join error through a type
    // that carries none.
    .unwrap_or(None)
}

/// Renders one row's `add` and `remove` commands, prefixed with
/// `launch_argv` (the CLI's own launch command), the way every registration
/// child is started. `bridge` is `None` while the bridge this row would
/// register is not yet known here -- no configured bridge, or a row whose
/// probe has not run -- which keeps the `{bridge}` placeholder in the
/// rendered `add` rather than guessing at it.
fn row_commands(
    spec: &mcp_registration::McpRegistrationSpec,
    target: &mcp_registration::RegistrationTarget,
    launch_argv: &[String],
    bridge: Option<&str>,
) -> (McpRegistrationCommand, McpRegistrationCommand) {
    let program_at = program_at_for(launch_argv);
    let bridge = bridge.unwrap_or("{bridge}");
    let add = McpRegistrationCommand {
        argv: full_argv(launch_argv, spec.render_add_lossy(target, bridge)),
        program_at,
    };
    let remove = McpRegistrationCommand {
        argv: full_argv(launch_argv, spec.render_remove(target)),
        program_at,
    };
    (add, remove)
}

/// `launch_argv` followed by `rendered`.
fn full_argv(launch_argv: &[String], rendered: Vec<String>) -> Vec<String> {
    let mut argv = launch_argv.to_vec();
    argv.extend(rendered);
    argv
}

/// The index in `launch_argv` at which the CLI's own executable starts, once
/// `launch_argv` (the profile's launch command) precedes a rendered `add` /
/// `remove` argv (the same convention `full_argv` follows).
fn program_at_for(launch_argv: &[String]) -> u32 {
    u32::try_from(launch_argv.len().saturating_sub(1)).unwrap_or(u32::MAX)
}

/// `spec`'s settings-file edit, for display: the values in TOML notation, in
/// key order.
fn to_wire_toml_set(
    spec: &mcp_registration::TomlSetSpec,
    target: &mcp_registration::RegistrationTarget,
) -> McpRegistrationTomlSet {
    let mut values: Vec<McpRegistrationTomlValue> = spec
        .values()
        .iter()
        .map(|(key, value)| McpRegistrationTomlValue {
            key: key.clone(),
            value: value.to_string(),
        })
        .collect();
    values.sort_by(|a, b| a.key.cmp(&b.key));
    McpRegistrationTomlSet {
        file: spec.file().to_string(),
        table: spec.table(target),
        values,
    }
}

/// The entries `spec` owns in its settings files, for display: each as
/// registration would write it on this host. A row whose daemon path is
/// converted by its probe keeps the `{daemon}` placeholder, as its `add`
/// keeps `{bridge}`.
fn hook_entries_preview(
    config: &DaemonConfig,
    spec: &mcp_registration::McpRegistrationSpec,
    target: &mcp_registration::RegistrationTarget,
) -> Vec<McpRegistrationHookEntry> {
    let daemon = if spec.has_bridge_probe() {
        None
    } else {
        hook_daemon_path(config)
    };
    let context = EditContext {
        target: target.clone(),
        os: config.host_os,
        daemon,
    };
    SettingsEdit::of(spec)
        .iter()
        .flat_map(|edit| edit.preview(&context))
        .collect()
}

/// [`mcp_registration::RegistrationState`] as the wire enum.
fn to_wire_state(state: mcp_registration::RegistrationState) -> McpRegistrationState {
    match state {
        mcp_registration::RegistrationState::Registered => McpRegistrationState::Registered,
        mcp_registration::RegistrationState::Differs => McpRegistrationState::Differs,
        mcp_registration::RegistrationState::NotRegistered => McpRegistrationState::NotRegistered,
        mcp_registration::RegistrationState::NotInstalled => McpRegistrationState::NotInstalled,
        mcp_registration::RegistrationState::Unknown => McpRegistrationState::Unknown,
    }
}

/// Reads one row's registration state from its own CLI: a probe first (when
/// the row has one), then the status itself, both under `config`'s limits.
async fn compute_status(
    config: &DaemonConfig,
    source: &RowSource,
    bridge: &str,
) -> McpRegistrationStatusInfo {
    match resolve_probe(config, source, bridge).await {
        ProbeOutcome::NoProbe => {
            let derived = match read_status_observation(config, source).await {
                Ok(observation) => derive(source, &observation, bridge),
                Err(reason) => refused_status(reason),
            };
            let derived = with_entries_status(config, source, derived).await;
            build_status_info(source, derived, bridge)
        }
        ProbeOutcome::Bridge(probed) => {
            let derived = match read_status_observation(config, source).await {
                Ok(observation) => derive(source, &observation, &probed),
                Err(reason) => refused_status(reason),
            };
            let derived = with_entries_status(config, source, derived).await;
            build_status_info(source, derived, &probed)
        }
        ProbeOutcome::Refused(reason) => build_status_info(source, refused_status(reason), bridge),
        ProbeOutcome::NotInstalled => build_status_info(
            source,
            derive(
                source,
                &mcp_registration::StatusObservation::NotInstalled,
                bridge,
            ),
            bridge,
        ),
        ProbeOutcome::Unusable => {
            build_status_info(source, mcp_registration::probe_output_unusable(), bridge)
        }
        ProbeOutcome::TimedOut => build_status_info(
            source,
            derive(
                source,
                &mcp_registration::StatusObservation::TimedOut,
                bridge,
            ),
            bridge,
        ),
        ProbeOutcome::OutputTooLarge => build_status_info(
            source,
            derive(
                source,
                &mcp_registration::StatusObservation::OutputTooLarge,
                bridge,
            ),
            bridge,
        ),
    }
}

/// `derived`, with the entries the row owns (a `toml_set`'s `entries`, a
/// `json_entry`) read back from their settings files: a row the CLI reports
/// as registered (or differing) also differs while one of them is missing or
/// changed, and is unknown when a file cannot be read. Any other state is
/// left as it is.
async fn with_entries_status(
    config: &DaemonConfig,
    source: &RowSource,
    mut derived: mcp_registration::DerivedStatus,
) -> mcp_registration::DerivedStatus {
    if !source.spec.owns_entries() {
        return derived;
    }
    if !matches!(
        derived.state,
        mcp_registration::RegistrationState::Registered
            | mcp_registration::RegistrationState::Differs
    ) {
        return derived;
    }
    match read_entries(config, &source.spec, &source.target, &source.env).await {
        EntriesRead::Hold => {}
        EntriesRead::Differ(labels) => {
            derived.state = mcp_registration::RegistrationState::Differs;
            derived.differs_in.extend(labels);
        }
        EntriesRead::Unreadable => {
            derived = refused_status("the settings file could not be read");
        }
    }
    derived
}

/// Reads the entries `spec` owns from their settings files the way a status
/// reads them. A launch counts on the CLI running yagura's hooks only while
/// they all hold: the entries are either the
/// hooks themselves or what the CLI needs before it runs injected ones.
/// Reading the files -- not the CLI's own status command -- keeps the check
/// to one file read per owned file on the host.
pub(super) async fn read_registration_entries(
    config: &DaemonConfig,
    spec: &mcp_registration::McpRegistrationSpec,
    env: &[(OsString, OsString)],
) -> EntriesRead {
    let target = mcp_registration::RegistrationTarget::for_instance(config.instance_id.as_deref());
    read_entries(config, spec, &target, env).await
}

/// What reading a row's owned entries back found.
pub(super) enum EntriesRead {
    Hold,
    /// A file, or entries of it, are missing or hold other values: the
    /// labels of the edits that differ.
    Differ(Vec<String>),
    Unreadable,
}

async fn read_entries(
    config: &DaemonConfig,
    spec: &mcp_registration::McpRegistrationSpec,
    target: &mcp_registration::RegistrationTarget,
    env: &[(OsString, OsString)],
) -> EntriesRead {
    let edits: Vec<SettingsEdit> = SettingsEdit::of(spec)
        .into_iter()
        .filter(|edit| edit.owns_entries_on(config.host_os))
        .collect();
    if edits.is_empty() {
        return EntriesRead::Hold;
    }
    let daemon = if edits.iter().any(SettingsEdit::names_daemon) {
        match resolve_status_daemon(config, spec, env).await {
            Ok(daemon) => daemon,
            Err(()) => return EntriesRead::Unreadable,
        }
    } else {
        None
    };
    let context = EditContext {
        target: target.clone(),
        os: config.host_os,
        daemon,
    };
    let absent_exit = spec.files_absent_exit();
    let mut differ = Vec::new();
    for edit in &edits {
        let Ok(observation) = read_file_observation(config, spec, env, edit.file()).await else {
            return EntriesRead::Unreadable;
        };
        let holds = match observation {
            mcp_registration::StatusObservation::FileMissing => false,
            mcp_registration::StatusObservation::Exited { code, .. }
                if absent_exit.is_some() && code == absent_exit =>
            {
                false
            }
            mcp_registration::StatusObservation::Exited {
                code: Some(0),
                stdout,
                ..
            } => match std::str::from_utf8(&stdout)
                .ok()
                .map(|text| edit.entries_hold(&context, text))
            {
                Some(Ok(holds)) => holds,
                // No daemon path the entries could name: they cannot hold.
                Some(Err(EditError::NoDaemon)) => false,
                _ => return EntriesRead::Unreadable,
            },
            _ => return EntriesRead::Unreadable,
        };
        if !holds {
            differ.push(edit.entries_label(target).unwrap_or_default());
        }
    }
    if differ.is_empty() {
        EntriesRead::Hold
    } else {
        EntriesRead::Differ(differ)
    }
}

/// The daemon path an owned entry names, as the row's own environment sees
/// it, for reading a status: `Ok(None)` when there is none to name, `Err`
/// when the row's probe could not convert it.
async fn resolve_status_daemon(
    config: &DaemonConfig,
    spec: &mcp_registration::McpRegistrationSpec,
    env: &[(OsString, OsString)],
) -> Result<Option<String>, ()> {
    let Some(daemon) = hook_daemon_path(config) else {
        return Ok(None);
    };
    match probe_path(config, spec, env, &daemon).await {
        ProbeOutcome::NoProbe => Ok(Some(daemon)),
        ProbeOutcome::Bridge(converted) => {
            Ok(usable_in_hook_command(&converted).then_some(converted))
        }
        _ => Err(()),
    }
}

/// The daemon's own copy in the report folder, the one a hook command runs:
/// `None` when there is no report folder, or its path cannot be written into
/// a command unquoted.
fn hook_daemon_path(config: &DaemonConfig) -> Option<String> {
    let path = config.report_dir.as_ref()?.join(REPORT_EXE_NAME);
    path.to_str()
        .filter(|path| usable_in_hook_command(path))
        .map(str::to_string)
}

/// Whether `path` can start a hook command as is. The CLI hands the command
/// to a shell (`sh -c`, `cmd /c`), and how it would quote is not known, so a
/// path that needs quoting is not written at all.
fn usable_in_hook_command(path: &str) -> bool {
    !path.is_empty()
        && !path.contains(char::is_whitespace)
        && !yagura_os::program::contains_cmd_metacharacter(path)
        && !path.contains(['\'', '`', '$'])
}

/// A row whose status command was refused before it started
/// ([`profile::resolve_spawn_argv`]): nothing was read, so the state is
/// unknown, with the refusal's own fixed reason.
fn refused_status(reason: &str) -> mcp_registration::DerivedStatus {
    mcp_registration::DerivedStatus {
        state: mcp_registration::RegistrationState::Unknown,
        differs_in: Vec::new(),
        reason: Some(reason.to_string()),
    }
}

/// [`mcp_registration::derive_status`], filled in with `source`'s own spec
/// and target.
fn derive(
    source: &RowSource,
    observation: &mcp_registration::StatusObservation,
    bridge: &str,
) -> mcp_registration::DerivedStatus {
    mcp_registration::derive_status(
        &source.spec,
        observation,
        &source.target,
        bridge,
        yagura_os::program::Platform::host(),
    )
}

/// The wire reply for `derived`, carrying `add` and `remove` rendered with
/// `observed_bridge` (a probed row's own environment path once its probe has
/// run, otherwise the bridge this daemon would register).
fn build_status_info(
    source: &RowSource,
    derived: mcp_registration::DerivedStatus,
    observed_bridge: &str,
) -> McpRegistrationStatusInfo {
    let (add, remove) = row_commands(
        &source.spec,
        &source.target,
        &source.launch_argv,
        Some(observed_bridge),
    );
    McpRegistrationStatusInfo {
        profile: source.profile.clone(),
        state: to_wire_state(derived.state),
        differs_in: derived.differs_in,
        reason: derived.reason,
        add: Some(add),
        remove: Some(remove),
    }
}

/// What running a row's `bridge_probe` (when it has one) came back with.
enum ProbeOutcome {
    /// The row has no probe; `bridge` is used as given.
    NoProbe,
    /// The probe succeeded: this environment's own path to the bridge.
    Bridge(String),
    /// The probe was refused before it started, with this fixed reason.
    Refused(&'static str),
    /// The probe could not be run, or exited unsuccessfully.
    NotInstalled,
    /// The probe exited successfully, but its output was not a usable path.
    Unusable,
    TimedOut,
    OutputTooLarge,
}

/// Runs `source`'s `bridge_probe` against `bridge`, when it has one.
async fn resolve_probe(config: &DaemonConfig, source: &RowSource, bridge: &str) -> ProbeOutcome {
    probe_path(config, &source.spec, &source.env, bridge).await
}

/// Runs `spec`'s `bridge_probe` against `path`, when it has one: the same
/// conversion serves every daemon-side path a row writes into its CLI's
/// environment.
async fn probe_path(
    config: &DaemonConfig,
    spec: &mcp_registration::McpRegistrationSpec,
    env: &[(OsString, OsString)],
    path: &str,
) -> ProbeOutcome {
    let Some(argv) = spec.render_probe(path) else {
        return ProbeOutcome::NoProbe;
    };
    match run_captured(config, argv, env, None).await {
        SpawnOutcome::Refused(reason) => ProbeOutcome::Refused(reason),
        SpawnOutcome::SpawnFailed => ProbeOutcome::NotInstalled,
        SpawnOutcome::Exited { code, stdout, .. } => {
            if code != Some(0) {
                return ProbeOutcome::NotInstalled;
            }
            match mcp_registration::validate_probe_output(&stdout) {
                Some(path) => ProbeOutcome::Bridge(path),
                None => ProbeOutcome::Unusable,
            }
        }
        SpawnOutcome::TimedOut => ProbeOutcome::TimedOut,
        SpawnOutcome::OutputTooLarge => ProbeOutcome::OutputTooLarge,
    }
}

/// Reads `source`'s status: a command's output, or a settings file's
/// content, whichever [`mcp_registration::McpRegistrationSpec::render_status`]
/// says. `Err` carries the fixed reason a command was refused with before it
/// started.
async fn read_status_observation(
    config: &DaemonConfig,
    source: &RowSource,
) -> Result<mcp_registration::StatusObservation, &'static str> {
    match source.spec.render_status(&source.target) {
        mcp_registration::StatusPlan::Run(args) => {
            let argv = full_argv(&source.launch_argv, args);
            Ok(match run_captured(config, argv, &source.env, None).await {
                SpawnOutcome::Refused(reason) => return Err(reason),
                SpawnOutcome::SpawnFailed => mcp_registration::StatusObservation::NotInstalled,
                SpawnOutcome::Exited {
                    code,
                    stdout,
                    stderr,
                } => mcp_registration::StatusObservation::Exited {
                    code,
                    stdout,
                    stderr,
                },
                SpawnOutcome::TimedOut => mcp_registration::StatusObservation::TimedOut,
                SpawnOutcome::OutputTooLarge => mcp_registration::StatusObservation::OutputTooLarge,
            })
        }
        mcp_registration::StatusPlan::ReadFile { file } => {
            read_file_observation(config, &source.spec, &source.env, &file).await
        }
    }
}

/// Reads a file-form status: through the row's own `files.read` tool when it
/// has one, otherwise directly under the configured home directory. `Err` as
/// [`read_status_observation`]'s.
async fn read_file_observation(
    config: &DaemonConfig,
    spec: &mcp_registration::McpRegistrationSpec,
    env: &[(OsString, OsString)],
    file: &str,
) -> Result<mcp_registration::StatusObservation, &'static str> {
    let Some(argv) = spec.render_file_read(file) else {
        return Ok(read_host_file_observation(config, file).await);
    };
    Ok(match run_captured(config, argv, env, None).await {
        SpawnOutcome::Refused(reason) => return Err(reason),
        SpawnOutcome::SpawnFailed => mcp_registration::StatusObservation::NotInstalled,
        SpawnOutcome::Exited { code, stdout, .. } => mcp_registration::StatusObservation::Exited {
            code,
            stdout,
            stderr: Vec::new(),
        },
        SpawnOutcome::TimedOut => mcp_registration::StatusObservation::TimedOut,
        SpawnOutcome::OutputTooLarge => mcp_registration::StatusObservation::OutputTooLarge,
    })
}

/// Reads a host settings file directly under the configured home directory.
/// `None` (no configured home) reads as an unreadable exit rather than
/// falling back to the daemon's own current directory.
async fn read_host_file_observation(
    config: &DaemonConfig,
    file: &str,
) -> mcp_registration::StatusObservation {
    let Some(home) = config.home_dir.clone() else {
        return unreadable_file();
    };
    let path = home.join(file);
    tokio::task::spawn_blocking(move || read_host_file(&path))
        .await
        // Unreachable in practice (`read_host_file` does not panic).
        .unwrap_or_else(|_join_err| unreadable_file())
}

fn read_host_file(path: &Path) -> mcp_registration::StatusObservation {
    match std::fs::read(path) {
        Ok(bytes) => mcp_registration::StatusObservation::Exited {
            code: Some(0),
            stdout: bytes,
            stderr: Vec::new(),
        },
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            mcp_registration::StatusObservation::FileMissing
        }
        Err(_) => unreadable_file(),
    }
}

/// A host file that exists but could not be read (permission denied, and the
/// like): reads as an exit without a code, which
/// [`mcp_registration::derive_status`] reports as
/// [`mcp_registration::RegistrationState::Unknown`].
fn unreadable_file() -> mcp_registration::StatusObservation {
    mcp_registration::StatusObservation::Exited {
        code: None,
        stdout: Vec::new(),
        stderr: Vec::new(),
    }
}

/// What running one registration command came back with.
enum SpawnOutcome {
    /// Refused before anything started, with [`profile::resolve_spawn_argv`]'s
    /// fixed reason.
    Refused(&'static str),
    SpawnFailed,
    Exited {
        code: Option<i32>,
        stdout: Vec<u8>,
        stderr: Vec<u8>,
    },
    TimedOut,
    OutputTooLarge,
}

/// Runs `argv` as a `[mcp_registration]` command: `config.home_dir` as its
/// working directory, `config.child_base_env` plus `env` as its environment,
/// `stdin` written and closed on its own thread when given (a `files.replace`
/// step's new file content), and `config`'s registration limits. `argv` is
/// resolved the way a window's launch is ([`profile::resolve_spawn_argv`]).
async fn run_captured(
    config: &DaemonConfig,
    argv: Vec<String>,
    env: &[(OsString, OsString)],
    stdin: Option<Vec<u8>>,
) -> SpawnOutcome {
    // Resolution probes the filesystem, so it runs off the async runtime.
    let resolve_config = config.clone();
    let resolved =
        tokio::task::spawn_blocking(move || profile::resolve_spawn_argv(&resolve_config, &argv))
            .await;
    let spawn_argv = match resolved {
        Ok(Ok(spawn_argv)) => spawn_argv,
        Ok(Err(reason)) => return SpawnOutcome::Refused(reason),
        // Unreachable in practice (`resolve_spawn_argv` does not panic).
        Err(_join_err) => return SpawnOutcome::SpawnFailed,
    };
    let captured = match pty::spawn_one_shot(
        spawn_argv,
        config.home_dir.clone(),
        pty::CaptureChildEnv::Based {
            base: config.child_base_env.clone(),
            overlay: env.to_vec(),
        },
        stdin,
    )
    .await
    {
        Ok(captured) => captured,
        Err(_) => return SpawnOutcome::SpawnFailed,
    };
    let limits = yagura_os::CaptureLimits {
        timeout: config.mcp_registration_timeout,
        max_output_bytes: config.mcp_registration_max_output_bytes,
    };
    let outcome = tokio::task::spawn_blocking(move || captured.collect(limits))
        .await
        // Unreachable in practice (`collect` does not panic); treated as a
        // timeout rather than propagating a join error through a type that
        // carries none.
        .unwrap_or(yagura_os::CaptureOutcome::TimedOut);
    match outcome {
        yagura_os::CaptureOutcome::Exited {
            code,
            stdout,
            stderr,
        } => SpawnOutcome::Exited {
            code,
            stdout,
            stderr,
        },
        yagura_os::CaptureOutcome::TimedOut => SpawnOutcome::TimedOut,
        yagura_os::CaptureOutcome::OutputTooLarge => SpawnOutcome::OutputTooLarge,
    }
}

// ---------------------------------------------------------------------
// `run_mcp_registration`: register, update or remove one profile's
// registration.
// ---------------------------------------------------------------------

/// A `run_mcp_registration` request's own fields, grouped so
/// [`handle_run_mcp_registration`] takes one argument for them instead of
/// three.
pub(super) struct RunRequest {
    /// The profile being changed.
    pub(super) profile: String,
    /// The action to run.
    pub(super) action: McpRegistrationAction,
    /// The request this operation answers.
    pub(super) request_id: u64,
}

/// Answers `run_mcp_registration`: registers, updates or removes one
/// profile's registration, running every child through
/// [`pty::spawn_one_shot`]. The immediate
/// `mcp_registration_requested` answer is sent once the change slot
/// (`change_permits`, capacity 1 across the whole daemon) is
/// admitted; the operation itself then runs from a detached task, the same
/// shape [`handle_list_mcp_registrations`] uses for its own listing slot, so
/// a caller reading its own connection is never blocked on another
/// connection's operation. The operation's outcome follows as one
/// [`DaemonMessage::McpRegistrationResult`] push.
pub(super) async fn handle_run_mcp_registration(
    config: &DaemonConfig,
    role_and_profile_reads: &Semaphore,
    change_permits: &Arc<Semaphore>,
    outbound: &Arc<Outbound>,
    state: &ConnectionState,
    request: RunRequest,
) {
    let RunRequest {
        profile,
        action,
        request_id,
    } = request;
    if !matches!(state.origin, RequestOrigin::Approver) {
        outbound.reply(&auth_failed_msg());
        return;
    }
    let Some(source) = resolve_row_source(config, role_and_profile_reads, &profile).await else {
        outbound.reply(&error_msg(
            "invalid_argument",
            "no such profile, or it has no mcp registration table",
        ));
        return;
    };
    let needs_bridge = matches!(
        action,
        McpRegistrationAction::Register | McpRegistrationAction::Update
    );
    if needs_bridge && config.mcp_bridge.is_none() {
        outbound.reply(&error_msg(
            "invalid_state",
            "no bridge is configured beside this daemon",
        ));
        return;
    }
    let Ok(permit) = Arc::clone(change_permits).try_acquire_owned() else {
        outbound.reply(&error_msg(
            "resource_exhausted",
            "another mcp registration change is already running",
        ));
        return;
    };

    outbound.reply(&DaemonMessage::McpRegistrationRequested {
        request_id,
        profile: source.profile.clone(),
        action,
    });

    let config = config.clone();
    let outbound = Arc::clone(outbound);
    tokio::spawn(async move {
        // Held until the operation below, and the status re-read that
        // follows it, both finish.
        let _permit = permit;
        let (steps, error) = run_operation(&config, &source, action).await;
        let bridge = config
            .mcp_bridge
            .as_ref()
            .and_then(|path| path.to_str())
            .unwrap_or_default();
        let status = compute_status(&config, &source, bridge).await;
        let error = error.or_else(|| unconfirmed_toml_set(&source, action, &status));
        outbound.reply(&DaemonMessage::McpRegistrationResult {
            request_id,
            profile: source.profile,
            action,
            steps,
            error,
            status,
        });
    });
}

/// The fixed failure for a Register / Update whose settings-file edit was
/// written, yet the status re-read afterwards still lists one of
/// `toml_set`'s keys in `differs_in`: the CLI does not read the value set.
const REASON_TOML_SET_UNCONFIRMED: &str = "the CLI does not report the value that was set";

/// [`REASON_TOML_SET_UNCONFIRMED`] when `action` wrote `source`'s
/// `toml_set` but `status` (read after it) still differs in one of its keys.
fn unconfirmed_toml_set(
    source: &RowSource,
    action: McpRegistrationAction,
    status: &McpRegistrationStatusInfo,
) -> Option<String> {
    if matches!(action, McpRegistrationAction::Remove) {
        return None;
    }
    let values = source
        .spec
        .toml_set()
        .map(mcp_registration::TomlSetSpec::values);
    let labels: Vec<String> = SettingsEdit::of(&source.spec)
        .iter()
        .filter_map(|edit| edit.entries_label(&source.target))
        .collect();
    status
        .differs_in
        .iter()
        .any(|key| values.is_some_and(|values| values.contains_key(key)) || labels.contains(key))
        .then(|| REASON_TOML_SET_UNCONFIRMED.to_string())
}

/// Runs one operation against `source`:
/// - Register: `[probe]` (when the row has one) → `add` → `[daemon probe]`
///   (when an edit names the daemon and the row has a probe) → each
///   settings-file edit (`toml_set`, then `json_entry`).
/// - Update: the same steps as Remove, then the same steps as Register.
/// - Remove: `remove` → the removal of each edit's owned entries.
///
/// A step that fails stops the operation there -- no later step runs -- and
/// `add` is never undone: a settings-file edit that later fails leaves the
/// row `differs`, fixable by running Update.
async fn run_operation(
    config: &DaemonConfig,
    source: &RowSource,
    action: McpRegistrationAction,
) -> (Vec<McpRegistrationStep>, Option<String>) {
    let mut steps = Vec::new();

    let carries_registration = matches!(
        action,
        McpRegistrationAction::Register | McpRegistrationAction::Update
    );
    // Checked before any step runs (a row with no
    // `env_arg` cannot carry a named instance's environment), with a
    // placeholder bridge -- carrying the instance's environment never
    // depends on the bridge value itself, so this check is valid before the
    // bridge (and, for a probed row, its probe) is resolved below.
    if carries_registration && let Err(err) = source.spec.render_add(&source.target, "") {
        return (steps, Some(err.to_string()));
    }

    if matches!(
        action,
        McpRegistrationAction::Update | McpRegistrationAction::Remove
    ) {
        match run_remove_step(config, source).await {
            Ok(step) => steps.push(step),
            Err((step, reason)) => {
                steps.push(step);
                return (steps, Some(reason));
            }
        }
        let context = EditContext {
            target: source.target.clone(),
            os: config.host_os,
            daemon: None,
        };
        for edit in SettingsEdit::of(&source.spec)
            .iter()
            .filter(|edit| edit.owns_entries())
        {
            match run_edit(config, source, edit, EditChange::RemoveEntries, &context).await {
                EditOutcome::Applied(mut edit_steps) => steps.append(&mut edit_steps),
                EditOutcome::Failed(mut edit_steps, reason) => {
                    steps.append(&mut edit_steps);
                    return (steps, Some(reason));
                }
            }
        }
        if matches!(action, McpRegistrationAction::Remove) {
            return (steps, None);
        }
    }

    // Register, or Update continuing after its own `remove`: `run_operation`
    // was only ever called after the caller (`handle_run_mcp_registration`)
    // confirmed `config.mcp_bridge` is configured for one of these two
    // actions, so its absence here is unreachable in practice.
    let Some(configured_bridge) = config.mcp_bridge.as_ref().and_then(|path| path.to_str()) else {
        return (
            steps,
            Some("no bridge is configured beside this daemon".to_string()),
        );
    };
    let bridge = match resolve_operation_path(config, source, configured_bridge, &mut steps).await {
        Ok(bridge) => bridge,
        Err(reason) => return (steps, Some(reason)),
    };

    match run_add_step(config, source, &bridge).await {
        Ok(step) => steps.push(step),
        Err((step, reason)) => {
            steps.push(step);
            return (steps, Some(reason));
        }
    }

    let edits = SettingsEdit::of(&source.spec);
    let daemon = if edits.iter().any(SettingsEdit::names_daemon) {
        match resolve_operation_daemon(config, source, &mut steps).await {
            Ok(daemon) => daemon,
            Err(reason) => return (steps, Some(reason)),
        }
    } else {
        None
    };
    let context = EditContext {
        target: source.target.clone(),
        os: config.host_os,
        daemon,
    };
    for edit in &edits {
        match run_edit(config, source, edit, EditChange::Set, &context).await {
            EditOutcome::Applied(mut edit_steps) => steps.append(&mut edit_steps),
            EditOutcome::Failed(mut edit_steps, reason) => {
                steps.append(&mut edit_steps);
                return (steps, Some(reason));
            }
        }
    }

    (steps, None)
}

/// The daemon path the row's owned entries name, as its CLI's environment
/// sees it: converted by the row's probe the way the bridge is (its step
/// pushed onto `steps`). `Ok(None)` when there is no path the entries can
/// carry.
async fn resolve_operation_daemon(
    config: &DaemonConfig,
    source: &RowSource,
    steps: &mut Vec<McpRegistrationStep>,
) -> Result<Option<String>, String> {
    let Some(daemon) = hook_daemon_path(config) else {
        return Ok(None);
    };
    let converted = resolve_operation_path(config, source, &daemon, steps).await?;
    Ok(usable_in_hook_command(&converted).then_some(converted))
}

/// Resolves a daemon-side path (the bridge `add` registers, the daemon a
/// hook runs) as the row's environment sees it: `path` directly for a row
/// with no [`mcp_registration::McpRegistrationSpec::render_probe`] tool, or
/// -- for one that has it -- that tool's own converted path, once its step
/// (pushed onto `steps` either way) exits zero and its output passes
/// [`mcp_registration::validate_probe_output`].
async fn resolve_operation_path(
    config: &DaemonConfig,
    source: &RowSource,
    path: &str,
    steps: &mut Vec<McpRegistrationStep>,
) -> Result<String, String> {
    let Some(argv) = source.spec.render_probe(path) else {
        return Ok(path.to_string());
    };
    let run = run_step(config, &source.env, argv, 0, true, None).await;
    let exit_code = run.exit_code;
    let stdout = run.stdout.clone();
    let failure = run.failure_reason("the environment probe failed");
    steps.push(run.step);
    if exit_code != Some(0) {
        return Err(failure);
    }
    match mcp_registration::validate_probe_output(&stdout) {
        Some(path) => Ok(path),
        None => Err("the environment probe returned an unusable path".to_string()),
    }
}

/// Runs the `add` step against `bridge`. `Err` carries the step (still
/// reported) alongside a fixed reason: [`RenderError::InstanceNotCarried`]
/// (unreachable here in practice -- `run_operation` already checked this
/// before any step ran) or a non-zero exit.
async fn run_add_step(
    config: &DaemonConfig,
    source: &RowSource,
    bridge: &str,
) -> Result<McpRegistrationStep, (McpRegistrationStep, String)> {
    let rendered = match source.spec.render_add(&source.target, bridge) {
        Ok(rendered) => rendered,
        Err(err) => {
            return Err((
                McpRegistrationStep {
                    argv: Vec::new(),
                    program_at: 0,
                    exit_code: None,
                    output: String::new(),
                },
                err.to_string(),
            ));
        }
    };
    let argv = full_argv(&source.launch_argv, rendered);
    let program_at = program_at_for(&source.launch_argv);
    let run = run_step(config, &source.env, argv, program_at, true, None).await;
    if run.exit_code == Some(0) {
        Ok(run.step)
    } else {
        let reason = run.failure_reason("the registration command failed");
        Err((run.step, reason))
    }
}

/// Runs the `remove` step. Unlike `add`, any exit the command reports (not
/// only zero) is accepted: removing a registration that is already gone is
/// the goal state, not a failure, and re-reading status afterwards is what
/// tells the caller whether the row ended up `not_registered`. Only a step
/// that could not be run at all (no exit code -- a spawn failure, a timeout,
/// or output over the limit) stops the operation.
async fn run_remove_step(
    config: &DaemonConfig,
    source: &RowSource,
) -> Result<McpRegistrationStep, (McpRegistrationStep, String)> {
    let argv = full_argv(
        &source.launch_argv,
        source.spec.render_remove(&source.target),
    );
    let program_at = program_at_for(&source.launch_argv);
    let run = run_step(config, &source.env, argv, program_at, true, None).await;
    if run.exit_code.is_some() {
        Ok(run.step)
    } else {
        let reason = run.failure_reason("the removal command could not be run");
        Err((run.step, reason))
    }
}

/// What running a settings-file edit came back with: the steps it ran
/// (possibly none, for a direct host edit) either way, paired with success
/// or a fixed failure reason.
enum EditOutcome {
    Applied(Vec<McpRegistrationStep>),
    Failed(Vec<McpRegistrationStep>, String),
}

/// One settings file a row edits beside its CLI's own registration command.
/// The kinds differ only in the file format they parse and write; reading,
/// writing back and deciding whether the owned entries hold are shared.
#[derive(Debug, Clone)]
enum SettingsEdit {
    Toml(mcp_registration::TomlSetSpec),
    Json(mcp_registration::JsonEntrySpec),
}

/// What a [`SettingsEdit`] renders its values with, beyond the file itself.
#[derive(Debug, Clone)]
struct EditContext {
    target: mcp_registration::RegistrationTarget,
    os: yagura_os::program::HostOs,
    /// The path a hook command runs the daemon by, as the CLI's own
    /// environment sees it. `None` when there is none to name.
    daemon: Option<String>,
}

/// Why a settings-file edit could not be made.
#[derive(Debug, Clone, Copy)]
enum EditError {
    Toml(toml_set::TomlSetError),
    Json(json_entry::JsonEntryError),
    /// The entries name the daemon, but there is no path to it they can
    /// carry.
    NoDaemon,
}

/// Which change an edit step makes to its settings file.
#[derive(Debug, Clone, Copy)]
enum EditChange {
    /// Sets the edit's values and adds the entries it owns.
    Set,
    /// Takes out the entries the edit owns.
    RemoveEntries,
}

/// What an edit step does when its settings file is not there.
#[derive(Debug, Clone, Copy)]
enum MissingFile {
    /// Nothing to do: the goal is already met.
    Done,
    /// The file is not the one the CLI reads; nothing is written.
    Fail,
    /// The file is created, starting from this content.
    StartFrom(&'static str),
}

impl SettingsEdit {
    /// Every settings file `spec` edits, in the order registration writes
    /// them.
    fn of(spec: &mcp_registration::McpRegistrationSpec) -> Vec<Self> {
        let mut edits = Vec::new();
        if let Some(toml_set) = spec.toml_set() {
            edits.push(Self::Toml(toml_set.clone()));
        }
        if let Some(json_entry) = spec.json_entry() {
            edits.push(Self::Json(json_entry.clone()));
        }
        edits
    }

    fn file(&self) -> &str {
        match self {
            Self::Toml(toml_set) => toml_set.file(),
            Self::Json(json_entry) => json_entry.file(),
        }
    }

    /// Whether the edit owns entries that removal takes out again.
    fn owns_entries(&self) -> bool {
        match self {
            Self::Toml(toml_set) => toml_set.has_entries(),
            Self::Json(_) => true,
        }
    }

    /// Whether the edit owns entries on `os`.
    fn owns_entries_on(&self, os: yagura_os::program::HostOs) -> bool {
        match self {
            Self::Toml(toml_set) => !toml_set.entry_edits(os).is_empty(),
            Self::Json(_) => true,
        }
    }

    /// Whether the edit's values name the daemon.
    fn names_daemon(&self) -> bool {
        matches!(self, Self::Json(_))
    }

    /// The name [`mcp_registration::DerivedStatus::differs_in`] gives the
    /// owned entries.
    fn entries_label(&self, target: &mcp_registration::RegistrationTarget) -> Option<String> {
        match self {
            Self::Toml(toml_set) => toml_set.entries_label(),
            Self::Json(json_entry) => Some(json_entry.names(target).join(", ")),
        }
    }

    fn json_entries(
        json_entry: &mcp_registration::JsonEntrySpec,
        context: &EditContext,
    ) -> Result<Vec<(String, serde_json::Value)>, EditError> {
        let daemon = context.daemon.as_deref().ok_or(EditError::NoDaemon)?;
        Ok(json_entry.entries(&context.target, daemon))
    }

    /// The entries the edit owns on `context.os`, each named and written in
    /// the file's own notation as [`SettingsEdit::rewrite`] adds it.
    fn preview(&self, context: &EditContext) -> Vec<McpRegistrationHookEntry> {
        let file = self.file().to_string();
        match self {
            Self::Toml(toml_set) => toml_set
                .entry_edits(context.os)
                .into_iter()
                .filter_map(|edit| {
                    Some(McpRegistrationHookEntry {
                        file: file.clone(),
                        name: edit.table.last()?.clone(),
                        content: toml::Value::Table(edit.values.clone()).to_string(),
                    })
                })
                .collect(),
            Self::Json(json_entry) => {
                let daemon = context.daemon.as_deref().unwrap_or("{daemon}");
                json_entry
                    .entries(&context.target, daemon)
                    .into_iter()
                    .map(|(name, value)| McpRegistrationHookEntry {
                        file: file.clone(),
                        name,
                        content: value.to_string(),
                    })
                    .collect()
            }
        }
    }

    /// The rewritten text of `content`, `Ok(None)` when nothing changes.
    fn rewrite(
        &self,
        change: EditChange,
        context: &EditContext,
        content: &str,
    ) -> Result<Option<String>, EditError> {
        let updated = match (self, change) {
            (Self::Toml(toml_set), EditChange::Set) => {
                toml_set::apply_edits(content, &toml_set.edits(&context.target, context.os))
                    .map_err(EditError::Toml)?
            }
            (Self::Toml(toml_set), EditChange::RemoveEntries) => {
                let tables: Vec<Vec<String>> = toml_set
                    .entry_edits(context.os)
                    .into_iter()
                    .map(|edit| edit.table)
                    .collect();
                toml_set::remove_tables(content, &tables).map_err(EditError::Toml)?
            }
            (Self::Json(json_entry), EditChange::Set) => {
                json_entry::set_entries(content, &Self::json_entries(json_entry, context)?)
                    .map_err(EditError::Json)?
            }
            (Self::Json(json_entry), EditChange::RemoveEntries) => {
                json_entry::remove_entries(content, &json_entry.names(&context.target))
                    .map_err(EditError::Json)?
            }
        };
        Ok((updated != content).then_some(updated))
    }

    /// Whether every entry the edit owns is in `content` as it would write
    /// it.
    fn entries_hold(&self, context: &EditContext, content: &str) -> Result<bool, EditError> {
        match self {
            Self::Toml(toml_set) => {
                toml_set::edits_hold(content, &toml_set.entry_edits(context.os))
                    .map_err(EditError::Toml)
            }
            Self::Json(json_entry) => {
                json_entry::entries_hold(content, &Self::json_entries(json_entry, context)?)
                    .map_err(EditError::Json)
            }
        }
    }

    /// A TOML settings file that is not there is not the one the CLI reads;
    /// a JSON one holding only entries the row owns is created.
    fn missing_file(&self, change: EditChange) -> MissingFile {
        match (self, change) {
            (_, EditChange::RemoveEntries) => MissingFile::Done,
            (Self::Toml(_), EditChange::Set) => MissingFile::Fail,
            (Self::Json(_), EditChange::Set) => MissingFile::StartFrom("{}\n"),
        }
    }

    /// The step output naming what changed and where, never the values.
    fn describe(
        &self,
        change: EditChange,
        target: &mcp_registration::RegistrationTarget,
    ) -> String {
        let file = self.file();
        match self {
            Self::Toml(toml_set) => {
                let entries = toml_set.entries_label();
                match change {
                    EditChange::Set => {
                        let keys: Vec<&str> =
                            toml_set.values().keys().map(String::as_str).collect();
                        let table = toml_set.table(target).join(".");
                        let owned = entries
                            .map(|under| format!(" and its entries under [{under}]"))
                            .unwrap_or_default();
                        format!("set {} in [{table}]{owned} of {file}", keys.join(", "))
                    }
                    EditChange::RemoveEntries => format!(
                        "removed its entries under [{}] of {file}",
                        entries.unwrap_or_default()
                    ),
                }
            }
            Self::Json(json_entry) => {
                let names = json_entry.names(target).join(", ");
                match change {
                    EditChange::Set => format!("set its entries {names} in {file}"),
                    EditChange::RemoveEntries => {
                        format!("removed its entries {names} from {file}")
                    }
                }
            }
        }
    }

    /// The fixed reason the edit failed. In every case nothing about the
    /// file is written.
    fn failed(&self, err: Option<EditError>) -> String {
        let file = self.file();
        match (self, err) {
            (_, Some(EditError::Toml(toml_set::TomlSetError::NotATable))) => {
                format!("a value in `{file}` is in the way of a table to write")
            }
            (_, Some(EditError::Json(json_entry::JsonEntryError::Unparsable))) => {
                format!("`{file}` is not valid JSON")
            }
            (_, Some(EditError::Json(json_entry::JsonEntryError::NotAnObject))) => {
                format!("`{file}` does not hold a JSON object")
            }
            (_, Some(EditError::NoDaemon)) => {
                format!("no path to the daemon can be written into `{file}`")
            }
            (Self::Json(_), _) => format!("`{file}` could not be read"),
            (Self::Toml(_), _) => format!("the table was not found in `{file}`"),
        }
    }
}

/// Makes `change` to `edit`'s settings file: through the row's own
/// `files.read` / `files.replace` tools when it has them, directly under the
/// configured home directory otherwise (the same `files`-or-not split
/// [`read_file_observation`] uses for reading status).
async fn run_edit(
    config: &DaemonConfig,
    source: &RowSource,
    edit: &SettingsEdit,
    change: EditChange,
    context: &EditContext,
) -> EditOutcome {
    match source.spec.render_file_read(edit.file()) {
        None => run_edit_host(config, source, edit, change, context).await,
        Some(read_argv) => {
            run_edit_via_files(config, source, edit, change, context, read_argv).await
        }
    }
}

/// A host settings file, read and replaced directly (no `files` tool): one
/// combined step, carrying no argv (design: "a step that reads or writes a
/// file rather than running a command carries an empty `argv`") and no part
/// of the value it wrote.
async fn run_edit_host(
    config: &DaemonConfig,
    source: &RowSource,
    edit: &SettingsEdit,
    change: EditChange,
    context: &EditContext,
) -> EditOutcome {
    let Some(home) = config.home_dir.clone() else {
        return EditOutcome::Failed(Vec::new(), edit.failed(None));
    };
    let path = home.join(edit.file());
    let missing = edit.missing_file(change);
    let owned_edit = edit.clone();
    let owned_context = context.clone();
    let applied = tokio::task::spawn_blocking(move || {
        rewrite_host_file(&path, missing, |content| {
            owned_edit.rewrite(change, &owned_context, content)
        })
    })
    .await
    // Unreachable in practice (`rewrite_host_file` does not panic).
    .unwrap_or(Err(None));
    match applied {
        Ok(()) => EditOutcome::Applied(vec![McpRegistrationStep {
            argv: Vec::new(),
            program_at: 0,
            exit_code: None,
            output: edit.describe(change, &source.target),
        }]),
        Err(err) => EditOutcome::Failed(Vec::new(), edit.failed(err)),
    }
}

/// Reads `path`, rewrites it with `rewrite`, and writes the result back
/// ([`yagura_os::replace_existing_file`], a temporary file renamed over the
/// original). A missing file is handled as `missing` says; a rewrite that
/// changes an existing file in nothing writes nothing. `Err` for any failure
/// along the way -- carrying the edit's own error when it was the edit that
/// failed -- leaves the original file untouched.
fn rewrite_host_file(
    path: &Path,
    missing: MissingFile,
    rewrite: impl FnOnce(&str) -> Result<Option<String>, EditError>,
) -> Result<(), Option<EditError>> {
    let (content, exists) = match std::fs::read_to_string(path) {
        Ok(content) => (content, true),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => match missing {
            MissingFile::Done => return Ok(()),
            MissingFile::Fail => return Err(None),
            MissingFile::StartFrom(seed) => (seed.to_string(), false),
        },
        Err(_) => return Err(None),
    };
    let updated = rewrite(&content).map_err(Some)?;
    match (updated, exists) {
        (Some(updated), true) => yagura_os::replace_existing_file(path, &updated).map_err(|_| None),
        (None, true) => Ok(()),
        (updated, false) => {
            create_host_file(path, updated.as_deref().unwrap_or(&content)).map_err(|_| None)
        }
    }
}

/// Creates the settings file at `path`, and any missing directory above it,
/// holding `content`: written to a temporary file beside it and renamed into
/// place, so the CLI never reads a partly written file.
fn create_host_file(path: &Path, content: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "the settings file has no parent directory or file name",
        ));
    };
    std::fs::create_dir_all(dir)?;
    let mut temp_name = OsString::from(".");
    temp_name.push(name);
    temp_name.push(format!(".tmp-{}", std::process::id()));
    let temp_path = dir.join(temp_name);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    // The file holds commands a CLI will run: owner-only from creation on.
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let written = options.open(&temp_path).and_then(|mut temp| {
        temp.write_all(content.as_bytes())
            .and_then(|()| temp.sync_all())
    });
    let result = written.and_then(|()| std::fs::rename(&temp_path, path));
    if result.is_err() {
        let _ = std::fs::remove_file(&temp_path);
    }
    result
}

/// A foreign-environment settings file, edited through the row's own
/// `files.read` / `files.replace` tools: two steps, each carrying its own
/// argv but never its content (design: a file step's output is never shown).
async fn run_edit_via_files(
    config: &DaemonConfig,
    source: &RowSource,
    edit: &SettingsEdit,
    change: EditChange,
    context: &EditContext,
    read_argv: Vec<String>,
) -> EditOutcome {
    let mut steps = Vec::new();
    let read = run_step(config, &source.env, read_argv, 0, false, None).await;
    let read_code = read.exit_code;
    let read_stdout = read.stdout.clone();
    let read_failure = read.failure_reason(&edit.failed(None));
    steps.push(read.step);
    let absent_exit = source.spec.files_absent_exit();
    let absent = absent_exit.is_some() && read_code == absent_exit;
    let content = if absent {
        match edit.missing_file(change) {
            MissingFile::Done => return EditOutcome::Applied(steps),
            MissingFile::Fail => return EditOutcome::Failed(steps, read_failure),
            MissingFile::StartFrom(seed) => seed.to_string(),
        }
    } else {
        if read_code != Some(0) {
            return EditOutcome::Failed(steps, read_failure);
        }
        let Ok(content) = String::from_utf8(read_stdout) else {
            return EditOutcome::Failed(steps, edit.failed(None));
        };
        content
    };
    let updated = match edit.rewrite(change, context, &content) {
        Ok(Some(updated)) => updated,
        Ok(None) if !absent => return EditOutcome::Applied(steps),
        Ok(None) => content,
        Err(err) => return EditOutcome::Failed(steps, edit.failed(Some(err))),
    };
    let Some(replace_argv) = source.spec.render_file_replace(edit.file()) else {
        return EditOutcome::Failed(steps, edit.failed(None));
    };
    let replace = run_step(
        config,
        &source.env,
        replace_argv,
        0,
        false,
        Some(updated.into_bytes()),
    )
    .await;
    let replace_ok = replace.exit_code == Some(0);
    let replace_failure = replace.failure_reason(&format!("could not write `{}`", edit.file()));
    steps.push(replace.step);
    if !replace_ok {
        return EditOutcome::Failed(steps, replace_failure);
    }
    EditOutcome::Applied(steps)
}

/// One step's own child-process outcome: the wire
/// [`McpRegistrationStep`] for the result push, plus the raw exit code and
/// stdout a multi-step edit (`toml_set` through `files.read`) needs but must
/// never itself put in the step's own `output` (design: a file step never
/// carries its content).
struct StepRun {
    step: McpRegistrationStep,
    exit_code: Option<i32>,
    stdout: Vec<u8>,
    /// The fixed reason the step was refused with before it started.
    refused: Option<&'static str>,
}

impl StepRun {
    /// The reason this step failed: its refusal's own reason when it was
    /// refused before starting, `otherwise` for any other failure.
    fn failure_reason(&self, otherwise: &str) -> String {
        self.refused.unwrap_or(otherwise).to_string()
    }
}

/// Runs one operation step: `argv` as a child, `stdin` written and closed
/// when given. `show_output` decides whether the step's own sanitized,
/// combined stdout and stderr reach [`McpRegistrationStep::output`] (an
/// operation step's own output is shown to the caller who ran it; a
/// `files.read` / `files.replace` tool's is not, so a settings file's
/// content never leaves the daemon through this path).
async fn run_step(
    config: &DaemonConfig,
    env: &[(OsString, OsString)],
    argv: Vec<String>,
    program_at: u32,
    show_output: bool,
    stdin: Option<Vec<u8>>,
) -> StepRun {
    let display_argv = argv.clone();
    let outcome = run_captured(config, argv, env, stdin).await;
    let refused = match outcome {
        SpawnOutcome::Refused(reason) => Some(reason),
        _ => None,
    };
    let (exit_code, stdout, raw_output): (Option<i32>, Vec<u8>, Vec<u8>) = match outcome {
        SpawnOutcome::Refused(reason) => (None, Vec::new(), reason.as_bytes().to_vec()),
        SpawnOutcome::Exited {
            code,
            stdout,
            stderr,
        } => {
            let mut combined = stdout.clone();
            combined.extend_from_slice(&stderr);
            (code, stdout, combined)
        }
        SpawnOutcome::SpawnFailed => (
            None,
            Vec::new(),
            b"the executable could not be started".to_vec(),
        ),
        SpawnOutcome::TimedOut => (
            None,
            Vec::new(),
            b"the command did not finish within its time limit".to_vec(),
        ),
        SpawnOutcome::OutputTooLarge => (
            None,
            Vec::new(),
            b"the command's output was larger than accepted".to_vec(),
        ),
    };
    // A refusal's fixed sentence carries nothing the child produced, so it is
    // shown even where the child's own output is not.
    let output = if show_output || refused.is_some() {
        mcp_registration::sanitize_output(&raw_output, mcp_registration::STEP_OUTPUT_MAX_BYTES)
    } else {
        String::new()
    };
    StepRun {
        step: McpRegistrationStep {
            argv: display_argv,
            program_at,
            exit_code,
            output,
        },
        exit_code,
        stdout,
        refused,
    }
}
