//! `yagura-daemon`: the process that owns the windows (each a PTY with its
//! own child process) and serves the IPC protocol to the GUI over the
//! platform's local transport ([`yagura_os`]).
//!
//! Execution order is fixed: argument parsing (`--shutdown`,
//! `--emit-status`, the `--deliver-*` doors, `--report-session`) -> instance identifier resolution -> the children's base
//! environment -> multi-instance prevention -> connection acceptance. The
//! daemon starts with no window: every window is opened by a client, so none
//! is spawned before the endpoint is protected and a losing multi-instance
//! race, or a client-side invocation, never orphans a child process. The base
//! environment's login-shell read comes earlier, but it is a one-shot capture
//! collected with its whole process tree before the endpoint is bound, not a
//! window's shell.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use yagura_os::{IpcTransport, PeerAuth, PlatformIpc};

use yagura_daemon::child_env::{
    ChildBaseEnvInputs, LOGIN_ENV_MAX_OUTPUT_BYTES, resolve_child_base_env,
};
use yagura_daemon::cli::{self, CliCommand};
use yagura_daemon::config::{
    StartupParts, log_dir, prepare_report_dir, report_dir, resolve_close_settle_ms,
    resolve_default_window_cwd, resolve_input_echo_window_ms, resolve_max_windows,
    resolve_resize_redraw_window_ms, resolve_status_token_override,
};
use yagura_daemon::control::{self, ApprovalPolicy};
use yagura_daemon::detector::definition::bundled_definition_names;
use yagura_daemon::detector::layers::DetectionLayers;
use yagura_daemon::inbox::{is_session_id, session_at_pointer};
use yagura_daemon::ipc::{chat, plan};
use yagura_daemon::profile::ProfileOs;
use yagura_daemon::store;
use yagura_daemon::{DaemonConfig, IpcServer, WindowRegistry, WindowsShellSearch, resolve_shell};
use yagura_protocol::{
    ClientMessage, DaemonMessage, DeliveryStream, STATUS_TOKEN_ENV, WINDOW_ID_ENV, WindowStatus,
    decode_daemon_message, encode_client_message, frame_encode,
};

/// A client-side connection to the daemon's endpoint, named through the OS
/// boundary.
type ClientConnection = <PlatformIpc as IpcTransport>::ClientConnection;

/// How long to wait for a `pong` reply during multi-instance-prevention /
/// `--shutdown` connectivity checks.
const PING_TIMEOUT: Duration = Duration::from_millis(500);

/// Stands in for a startup-log field the OS would not tell us.
const UNKNOWN_LOG_FIELD: &str = "unknown";

/// How long a hook waits for the daemon to answer a door that is not a wait.
const DOOR_ANSWER_TIMEOUT: Duration = Duration::from_secs(10);

/// How much of a hook's stdin is looked at for the session it names.
const HOOK_STDIN_PARSED_BYTES: usize = 64 * 1024;

fn main() -> Result<ExitCode> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = cli::parse_args(&args).context("invalid command line")?;
    // A CLI started by hand outside any window still runs its hooks; decided
    // before logging is set up so such a hook stays silent and leaves no trace.
    if command.is_hook_command() && !runs_inside_window() {
        if matches!(command, CliCommand::ReportSession { .. }) {
            // The CLI writes its input to the hook; a hook that leaves it
            // unread would make that write fail.
            read_hook_stdin();
        }
        return Ok(ExitCode::SUCCESS);
    }

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("failed to build the tokio runtime")?;

    runtime.block_on(async_main(command))
}

async fn async_main(command: CliCommand) -> Result<ExitCode> {
    // Resolved before anything else that derives from it (the log directory,
    // the endpoint name): every command needs a namespace, and the logger is
    // not yet installed, so a rejection here reaches stderr through this
    // function's own `Result`.
    let namespace = cli::resolve_instance_id(std::env::var_os("YAGURA_INSTANCE"))
        .context("failed to resolve YAGURA_INSTANCE")?;

    init_logging(&namespace);

    let ipc = PlatformIpc::default();
    // Fail-closed on a platform whose identity support is not implemented:
    // without the daemon's own identifier neither the endpoint protection nor
    // the peer check can be built, so startup stops here.
    let daemon_sid = ipc
        .current_user_id()
        .context("failed to resolve the current user identifier")?;
    // Owning: the daemon's bind path, which may create the Unix runtime
    // directory. Clients (gui / mcp) use the non-owning
    // `yagura_os::default_endpoint` instead.
    let endpoint = yagura_os::default_endpoint_owning(&daemon_sid, &namespace);

    // Every client-side command is resolved before anything else: neither may
    // enter the multi-instance-prevention sequence below, where an
    // already-running daemon would make this process exit successfully.
    let (
        detection_name,
        detection_dir_arg,
        playbook_arg,
        profiles_dir_arg,
        approver_exe_arg,
        roles_arg,
    ) = match command {
        CliCommand::Shutdown => return send_shutdown(&ipc, &endpoint).await.map(succeeded),
        CliCommand::EmitStatus { status, source } => {
            return send_status_event(&ipc, &endpoint, status, source)
                .await
                .map(succeeded);
        }
        door @ (CliCommand::DeliverContext
        | CliCommand::DeliverTake
        | CliCommand::DeliverWait
        | CliCommand::ReportSession { .. }) => return run_hook_door(&ipc, &endpoint, door).await,
        // `--detection` / `--detection-dir` / `--playbook` /
        // `--profiles-dir` / `--approver-exe` / `--roles` configure the
        // daemon this process is about to start, and are meaningless for
        // the client-side commands above.
        CliCommand::Run {
            detection_name,
            detection_dir_arg,
            playbook_arg,
            profiles_dir_arg,
            approver_exe_arg,
            roles_arg,
        } => (
            detection_name,
            detection_dir_arg,
            playbook_arg,
            profiles_dir_arg,
            approver_exe_arg,
            roles_arg,
        ),
    };

    // This check sits here, inside the `Run` continuation and before the
    // endpoint is bound, because it guards a resource (the session store) only `Run` opens;
    // `--shutdown` / `--emit-status` already returned above and never reach it.
    let env_store_db = std::env::var_os("YAGURA_STORE_DB");
    if yagura_os::store_override_conflicts(&namespace, env_store_db.as_deref()) {
        return Err(
            anyhow::Error::new(yagura_os::StartupEnvError::StoreOverrideWithNamedInstance)
                .context("YAGURA_STORE_DB is set; unset it to start this named instance"),
        );
    }

    // Whether the daemon inherited a terminal decides whether the login shell
    // is read.
    let term = std::env::var_os("TERM");
    let shell_env = std::env::var_os("SHELL").map(PathBuf::from);
    // Before the endpoint is bound: a client that can connect must find the
    // children's environment already decided (the GUI waits out the read).
    let child_base_env = resolve_child_base_env(ChildBaseEnvInputs {
        host_os: ProfileOs::host(),
        term,
        shell_env: shell_env.clone(),
        passwd_shell: tokio::task::spawn_blocking(yagura_os::login_shell::passwd_login_shell)
            .await
            .ok()
            .flatten(),
        shell_exists: yagura_os::program::default_is_executable,
        inherited: std::env::vars_os().collect(),
        preferred_locale: yagura_os::locale::preferred_locale()
            .and_then(|preferred| preferred.utf8_name()),
        locale_exists: yagura_os::locale::locale_exists,
        limits: yagura_os::CaptureLimits {
            timeout: yagura_os::LOGIN_ENV_READ_TIMEOUT,
            max_output_bytes: LOGIN_ENV_MAX_OUTPUT_BYTES,
        },
    })
    .await;

    // Multi-instance prevention, create-first: the endpoint name is claimed
    // by binding it and that very listener is kept for serving, so no
    // squatter can slip into a probe/drop/recreate window.
    let security = ipc
        .endpoint_security()
        .context("failed to build the ipc endpoint security")?;
    let listener = match ipc.bind(&endpoint, security).await {
        Ok(listener) => listener,
        Err(_) => {
            if try_ping(&ipc, &endpoint).await {
                tracing::info!("an existing yagura-daemon is already running; exiting");
                return Ok(ExitCode::SUCCESS);
            }
            tracing::error!(
                "failed to claim the ipc endpoint and no daemon responded to ping; \
                 a non-daemon process may be occupying the endpoint name"
            );
            std::process::exit(1);
        }
    };

    let defaults = DaemonConfig::default();
    let max_windows = resolve_max_windows(
        std::env::var("YAGURA_MAX_WINDOWS").ok().as_deref(),
        defaults.max_windows,
    );
    let close_settle_ms = resolve_close_settle_ms(
        std::env::var("YAGURA_CLOSE_SETTLE_MS").ok().as_deref(),
        defaults.close_settle_ms,
    );
    let input_echo_window_ms = resolve_input_echo_window_ms(
        std::env::var("YAGURA_INPUT_ECHO_WINDOW_MS").ok().as_deref(),
        defaults.input_echo_window_ms,
    );
    let resize_redraw_window_ms = resolve_resize_redraw_window_ms(
        std::env::var("YAGURA_RESIZE_REDRAW_WINDOW_MS")
            .ok()
            .as_deref(),
        defaults.resize_redraw_window_ms,
    );
    // The playbook is read exactly once, here. A path that cannot be read or
    // parsed leaves the policy at `None`, so a broken playbook degrades to
    // "nothing is auto-approved" instead of taking the daemon down.
    let playbook = cli::resolve_playbook(playbook_arg, std::env::var_os("YAGURA_PLAYBOOK"));
    // Logged verbatim, with no `canonicalize()`: an explicitly given path may
    // be relative, and Windows's `canonicalize` would add a `\\?\` prefix
    // that would not match what an operator typed or sees on disk.
    match playbook.as_deref() {
        Some(path) => tracing::info!("resolved playbook: {}", path.display()),
        None => tracing::info!("no playbook resolved"),
    }
    let approval_policy = playbook
        .as_deref()
        .and_then(|path| match control::load_policy(path) {
            Ok(policy) => Some(policy),
            Err(err) => {
                tracing::warn!(
                    "failed to load the playbook {}: {err}; \
                     every destructive operation stays denied",
                    path.display()
                );
                None
            }
        });

    // Canonicalized once, here, so that `register_approver` compares two
    // already-normalized paths without touching the filesystem per frame.
    let approver_exe = match approver_exe_arg {
        Some(path) => match path.canonicalize() {
            Ok(resolved) => Some(resolved),
            Err(err) => {
                tracing::warn!(
                    "approver registration disabled: --approver-exe could not be resolved: {err}"
                );
                None
            }
        },
        None => {
            tracing::warn!("approver registration disabled: --approver-exe was not given");
            None
        }
    };

    let config_base = yagura_os::default_config_base_dir();
    let detection_layers = DetectionLayers::from_resolution(cli::resolve_detection_dir(
        detection_dir_arg,
        std::env::var_os("YAGURA_DETECTION_DIR"),
        config_base.clone(),
        &namespace,
    ));
    log_startup_identity();
    log_detection_layers(&detection_layers);

    // Built once, here, from the same base environment every child starts
    // with, so resolution searches the `PATH` the resolved program will see;
    // carried forward on `DaemonConfig` for every resolution thereafter.
    let program_lookup = child_base_env.program_lookup();
    // Logged the same way as the playbook / detection-dir resolution above.
    let (profiles_dir, bundled_profiles) = match cli::resolve_profiles_dir(
        profiles_dir_arg,
        std::env::var_os("YAGURA_PROFILES_DIR"),
        config_base.clone(),
        &namespace,
    ) {
        cli::ProfilesDirResolution::Explicit(path) => {
            tracing::info!(
                "resolved profiles dir: {} (explicit, alone)",
                path.display()
            );
            (Some(path), false)
        }
        cli::ProfilesDirResolution::ConfigRoot(path) => {
            tracing::info!(
                "resolved profiles dir: {} (over the shipped profiles)",
                path.display()
            );
            (Some(path), true)
        }
        cli::ProfilesDirResolution::Unresolved => {
            tracing::info!("no profiles dir resolved: the shipped profiles alone");
            (None, true)
        }
    };
    let roles_path = cli::resolve_roles_path(
        roles_arg,
        std::env::var_os("YAGURA_ROLES"),
        config_base,
        &namespace,
    );
    match roles_path.as_deref() {
        Some(path) => tracing::info!("resolved roles path: {}", path.display()),
        None => tracing::info!("no roles path resolved"),
    }

    // One home directory serves both the default window cwd and
    // `config.home_dir`. The existence check runs off the async workers: a
    // home on a network mount can stall.
    let home_dir = std::env::home_dir();
    let default_window_cwd = {
        let home_dir = home_dir.clone();
        tokio::task::spawn_blocking(move || {
            resolve_default_window_cwd(home_dir.as_deref(), &|path: &Path| path.is_dir())
        })
        .await
        .ok()
        .flatten()
    };
    match default_window_cwd.as_deref() {
        Some(path) => tracing::info!("default window cwd: {}", path.display()),
        None => match home_dir.as_deref() {
            Some(path) => tracing::warn!(
                "the home directory {} is not an existing directory; a window opened \
                 without a cwd starts in the daemon's current directory",
                path.display()
            ),
            None => tracing::warn!(
                "no home directory resolved; a window opened without a cwd starts in \
                 the daemon's current directory"
            ),
        },
    }

    let mut config = DaemonConfig::from_startup(StartupParts {
        detection_dir: detection_layers.directory,
        bundled_definitions: detection_layers.bundled,
        detection_name,
        max_windows,
        close_settle_ms,
        input_echo_window_ms,
        resize_redraw_window_ms,
        playbook,
        approval_policy: approval_policy.clone(),
        approver_exe,
        // Test / development-only escape hatch. Production leaves this env
        // var unset, so the registry generates its token from the OS RNG.
        status_token_override: resolve_status_token_override(
            std::env::var("YAGURA_STATUS_TOKEN_OVERRIDE")
                .ok()
                .as_deref(),
        ),
        profiles_dir,
        bundled_profiles,
        roles_path,
        program_lookup,
        child_base_env,
        default_window_cwd,
    });
    control::apply_limits(
        &mut config,
        approval_policy.as_ref().and_then(ApprovalPolicy::limits),
    );
    control::apply_chat_section(
        &mut config,
        approval_policy
            .as_ref()
            .and_then(|policy| policy.chat.as_ref()),
    );
    control::apply_plans_section(
        &mut config,
        approval_policy
            .as_ref()
            .and_then(|policy| policy.plans.as_ref()),
    );
    let configured_shell = std::env::var_os("YAGURA_SHELL").map(PathBuf::from);
    // Unix only: the user's login shell is a better fallback than a hardcoded
    // literal, and `$SHELL` is read above rather than inside the pure
    // `resolve_shell`, keeping that function free of environment access.
    // `WindowsShellSearch::from_env` is likewise read once, here, rather than
    // inside `resolve_shell`.
    let windows_shell_search = WindowsShellSearch::from_env();
    // A profile that omits `[command]` uses exactly this shell, resolved once
    // here rather than re-read from the environment by every launch. Without
    // one the daemon still serves: only such a profile is refused.
    config.default_shell = match resolve_shell(
        configured_shell.as_deref(),
        shell_env.as_deref(),
        &windows_shell_search,
    ) {
        // A window's argv is UTF-8, so a shell path that is not cannot be launched.
        Ok(shell) if shell.to_str().is_some() => Some(shell),
        Ok(shell) => {
            tracing::warn!(
                "the resolved shell path {} is not valid UTF-8; a profile without \
                 [command] cannot open a window",
                shell.display()
            );
            None
        }
        Err(err) => {
            tracing::warn!(
                "failed to resolve a shell executable: {err}; a profile without \
                 [command] cannot open a window"
            );
            None
        }
    };
    config.mcp_bridge = resolve_mcp_bridge(config.host_os.platform());
    config.mcp_config_dir = yagura_os::default_state_base_dir()
        .map(|base| yagura_os::instance_dir(&base, &namespace).join(MCP_CONFIG_DIR_NAME));
    config.report_dir = tokio::task::spawn_blocking({
        let namespace = namespace.clone();
        move || resolve_report_dir(&namespace)
    })
    .await
    .ok()
    .flatten();
    // A named instance's registration must carry `YAGURA_INSTANCE` so
    // it never overwrites, or reads as, the default instance's own.
    config.instance_id = namespace.id().map(str::to_string);
    config.home_dir = home_dir;

    let registry = Arc::new(WindowRegistry::new(config.clone()));

    // The store is wired before any window can be opened, so start-up
    // rotation runs before anything writes to `current`.
    let explicit_db_path = config.store_db_path.clone();
    let data_base_dir = yagura_os::default_data_base_dir();
    let store_db_path = store::resolve_db_path(
        explicit_db_path.clone(),
        env_store_db.clone(),
        data_base_dir.clone().map(PathBuf::into_os_string),
        &namespace,
    );
    // The IPC layer answers restore candidates from the config, so the
    // resolved path must reach the config before the store is wired; the two
    // can then never name different files.
    config.store_db_path_resolved = store_db_path.clone();
    // Only the default `<data_base>/<namespace>` layout is narrowed to an
    // owner-only directory; forcing a caller-chosen path to 0700 could break
    // other legitimate access to it. `env_store_db` is always `None` here for
    // a named instance (the `YAGURA_STORE_DB` check above already refused the
    // combination), so this stays reachable for every instance rather than
    // just the default one.
    if explicit_db_path.is_none()
        && env_store_db.is_none()
        && let Some(base) = &data_base_dir
        && let Err(reason) =
            yagura_os::secure_owned_dir(base, &yagura_os::instance_dir(base, &namespace))
    {
        tracing::warn!("{reason}; the session store directory may not be owner-restricted");
    }
    store::wire_into(&registry, store_db_path);

    // Opening a database is blocking I/O, so it runs on a blocking thread
    // rather than this async one. Installed before any window can be opened,
    // so every participant window gets the intro.
    let chat = Arc::new(
        tokio::task::spawn_blocking({
            let config = config.clone();
            move || chat::ChatRuntime::from_config(&config)
        })
        .await
        .context("chat runtime initialization panicked")?,
    );
    chat::install_intro_hook(&registry, chat.clone());

    // Opened as a new boot (clearing every plan's owner and task window
    // binding, since no window id survives a restart), on a blocking thread
    // for the same reason the chat store above is.
    let ledger = Arc::new(
        tokio::task::spawn_blocking({
            let config = config.clone();
            move || plan::PlanRuntime::from_config(&config)
        })
        .await
        .context("plan ledger initialization panicked")?,
    );

    let server =
        Arc::new(IpcServer::new_with_chat(registry, endpoint, config, chat).with_ledger(ledger));
    server
        .run_with_listener(listener, daemon_sid)
        .await
        .context("IPC server loop failed")?;
    Ok(ExitCode::SUCCESS)
}

fn succeeded((): ()) -> ExitCode {
    ExitCode::SUCCESS
}

/// Directory, under the instance's state directory, holding the MCP
/// settings files profile launches ask for.
const MCP_CONFIG_DIR_NAME: &str = "mcp";

/// File stem of the MCP bridge executable shipped next to the daemon.
const MCP_BRIDGE_EXE_STEM: &str = "yagura-mcp";

/// Resolves the MCP bridge shipped next to this executable, in the form a
/// child process can be handed. `None` (with a warning) when it is absent or
/// its path has no such form: windows then open without MCP injection.
fn resolve_mcp_bridge(platform: yagura_os::program::Platform) -> Option<PathBuf> {
    let dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf));
    let bridge = dir
        .and_then(|dir| yagura_os::resolve_sibling_executable(&dir, MCP_BRIDGE_EXE_STEM, platform))
        .and_then(|path| yagura_os::to_plain_path(&path))
        .filter(|path| path.to_str().is_some());
    match bridge.as_deref() {
        Some(path) => tracing::info!("resolved mcp bridge: {}", path.display()),
        None => tracing::warn!(
            "no usable mcp bridge next to the daemon; windows open without MCP injection"
        ),
    }
    bridge
}

/// Blocking: prepares the report directory once per start (see
/// [`prepare_report_dir`]). `None`, with one warning, when it cannot be
/// prepared: windows then open with their `PATH` unchanged.
fn resolve_report_dir(namespace: &yagura_os::InstanceNamespace) -> Option<PathBuf> {
    let prepared = yagura_os::default_report_base_dir()
        .ok_or_else(|| "failed to resolve the report directory's base".to_string())
        .and_then(|base| {
            let exe = std::env::current_exe()
                .map_err(|err| format!("failed to resolve the daemon executable: {err}"))?;
            let dir = report_dir(&base, namespace);
            prepare_report_dir(&base, &dir, &exe).map(|()| dir)
        });
    match prepared {
        Ok(dir) => {
            tracing::info!("report dir: {}", dir.display());
            Some(dir)
        }
        Err(reason) => {
            tracing::warn!(
                "{reason}; windows open without the report dir in PATH, so CLI hooks cannot \
                 reach the daemon"
            );
            None
        }
    }
}

/// Logs which build is running: the package version compiled in, the
/// executable's path and the time that file was last modified (unix seconds),
/// so a log can be tied back to the binary that produced it. A field the OS
/// does not answer for is reported as `unknown` rather than taken as a
/// startup failure.
fn log_startup_identity() {
    let exe = std::env::current_exe().ok();
    let exe_display = exe.as_ref().map_or_else(
        || UNKNOWN_LOG_FIELD.to_string(),
        |path| path.display().to_string(),
    );
    let exe_modified = exe
        .as_ref()
        .and_then(|path| std::fs::metadata(path).ok())
        .and_then(|metadata| metadata.modified().ok())
        .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or_else(
            || UNKNOWN_LOG_FIELD.to_string(),
            |since_epoch| since_epoch.as_secs().to_string(),
        );
    tracing::info!(
        "daemon starting: version={} exe={} exe_modified={}",
        env!("CARGO_PKG_VERSION"),
        exe_display,
        exe_modified
    );
}

/// Logs the resolved definition layers: the directory layer's path (or
/// `none`), which combination of layers that makes, and how many bundled
/// definitions this daemon can reach.
///
/// The directory is logged verbatim, with no `canonicalize()`, for the same
/// reason the playbook path above is: an explicitly given path may be
/// relative, and Windows's `canonicalize` would add a `\\?\` prefix that
/// matches neither what an operator typed nor what is on disk.
fn log_detection_layers(layers: &DetectionLayers) {
    let directory = layers
        .directory
        .as_deref()
        .map_or_else(|| "none".to_string(), |path| path.display().to_string());
    let mode = match (layers.directory.is_some(), layers.bundled) {
        (true, false) => "exclusive",
        (true, true) => "overlay",
        (false, true) => "bundled-only",
        (false, false) => "disabled",
    };
    let bundled = if layers.bundled {
        bundled_definition_names().len()
    } else {
        0
    };
    tracing::info!(
        "detection layers: directory={} mode={} bundled={}",
        directory,
        mode,
        bundled
    );
}

/// Connects to `endpoint`, sends `ping`, and returns whether `pong` was
/// received within [`PING_TIMEOUT`]. Any connection or protocol failure is
/// treated as "no daemon there" (`false`).
async fn try_ping(ipc: &PlatformIpc, endpoint: &str) -> bool {
    let Ok(mut client) = ipc.connect(endpoint).await else {
        return false;
    };
    let frame = frame_encode(&encode_client_message(&ClientMessage::Ping));
    if client.write_all(&frame).await.is_err() {
        return false;
    }
    matches!(
        tokio::time::timeout(PING_TIMEOUT, read_one_daemon_message(&mut client)).await,
        Ok(Some(DaemonMessage::Pong))
    )
}

/// Connects to `endpoint` and sends `shutdown` (the `--shutdown` CLI entry
/// point). No reply is expected.
async fn send_shutdown(ipc: &PlatformIpc, endpoint: &str) -> Result<()> {
    let mut client = ipc.connect(endpoint).await.with_context(|| {
        format!("failed to connect to the daemon endpoint {endpoint} for --shutdown")
    })?;
    let frame = frame_encode(&encode_client_message(&ClientMessage::Shutdown));
    client
        .write_all(&frame)
        .await
        .context("failed to send the shutdown message")?;
    Ok(())
}

/// Whether this process runs inside a window this daemon spawned, i.e. both
/// variables a window's child inherits from `WindowRegistry::open` are set.
fn runs_inside_window() -> bool {
    [STATUS_TOKEN_ENV, WINDOW_ID_ENV]
        .iter()
        .all(|name| std::env::var_os(name).is_some())
}

/// Connects to `endpoint` and sends one `StatusEvent` (the `--emit-status`
/// helper). No reply is expected; failing to reach a daemon is an error, so a
/// hook that reports into nothing does not look successful.
///
/// Only reached from inside a window (see [`runs_inside_window`]): the token
/// and window id are the ones the window's child inherited, so a child CLI's
/// hooks authenticate as that window.
async fn send_status_event(
    ipc: &PlatformIpc,
    endpoint: &str,
    status: WindowStatus,
    source: String,
) -> Result<()> {
    let token = std::env::var(STATUS_TOKEN_ENV)
        .with_context(|| format!("{STATUS_TOKEN_ENV} is not valid Unicode"))?;
    let window_id = cli::resolve_emit_window_id(std::env::var_os(WINDOW_ID_ENV));
    let mut client = ipc.connect(endpoint).await.with_context(|| {
        format!("failed to connect to the daemon endpoint {endpoint} for --emit-status")
    })?;
    let frame = frame_encode(&encode_client_message(&ClientMessage::StatusEvent {
        source,
        status,
        window_id,
        token,
    }));
    client
        .write_all(&frame)
        .await
        .context("failed to send the status event")?;
    client
        .flush()
        .await
        .context("failed to flush the status event")?;
    Ok(())
}

/// Runs one of the commands a CLI's hook uses to reach its window's inbox,
/// only reached from inside a window (see [`runs_inside_window`]): the token and
/// window id are the ones the window's child inherited.
///
/// What a hook prints and how it ends is the daemon's answer, so a `wait` ends
/// with the exit code the window's profile declares. A refusal prints nothing
/// to stdout and ends with 1, which is never a code a hook reads as a wake.
async fn run_hook_door(ipc: &PlatformIpc, endpoint: &str, command: CliCommand) -> Result<ExitCode> {
    let token = std::env::var(STATUS_TOKEN_ENV)
        .with_context(|| format!("{STATUS_TOKEN_ENV} is not valid Unicode"))?;
    let window_id = std::env::var(WINDOW_ID_ENV)
        .with_context(|| format!("{WINDOW_ID_ENV} is not valid Unicode"))?;
    let waits = matches!(command, CliCommand::DeliverWait);
    let message = match command {
        CliCommand::DeliverContext => ClientMessage::DeliverContext { window_id, token },
        CliCommand::DeliverTake => ClientMessage::DeliverTake { window_id, token },
        CliCommand::DeliverWait => ClientMessage::DeliverWait { window_id, token },
        CliCommand::ReportSession { pointer } => {
            let input = tokio::task::spawn_blocking(read_hook_stdin)
                .await
                .context("reading the hook input panicked")?;
            let Some(session) = session_at_pointer(&input, &pointer).filter(|s| is_session_id(s))
            else {
                return Ok(ExitCode::SUCCESS);
            };
            ClientMessage::ReportSession {
                window_id,
                token,
                session,
            }
        }
        other => anyhow::bail!("{other:?} is not a hook door"),
    };

    let mut client = ipc.connect(endpoint).await.with_context(|| {
        format!("failed to connect to the daemon endpoint {endpoint} for a hook door")
    })?;
    client
        .write_all(&frame_encode(&encode_client_message(&message)))
        .await
        .context("failed to send the request")?;
    client
        .flush()
        .await
        .context("failed to flush the request")?;

    let answer = if waits {
        read_one_daemon_message(&mut client).await
    } else {
        tokio::time::timeout(DOOR_ANSWER_TIMEOUT, read_one_daemon_message(&mut client))
            .await
            .context("the daemon did not answer")?
    };
    match answer {
        Some(DaemonMessage::Delivery {
            output,
            stream,
            exit_code,
        }) => {
            write_hook_output(stream, &output)?;
            Ok(ExitCode::from(exit_code))
        }
        Some(DaemonMessage::SessionRecorded { .. }) => Ok(ExitCode::SUCCESS),
        Some(DaemonMessage::Error { code, .. }) => {
            anyhow::bail!("the daemon refused the request: {code}")
        }
        // The daemon went away while a wait was registered: nothing to wake for.
        None if waits => Ok(ExitCode::SUCCESS),
        _ => anyhow::bail!("the daemon sent no usable answer"),
    }
}

/// Prints `output` on `stream`. Nothing is printed for an empty text.
fn write_hook_output(stream: DeliveryStream, output: &str) -> Result<()> {
    if output.is_empty() {
        return Ok(());
    }
    match stream {
        DeliveryStream::Stdout => {
            let mut out = std::io::stdout().lock();
            out.write_all(output.as_bytes())?;
            out.flush()?;
        }
        DeliveryStream::Stderr => {
            let mut err = std::io::stderr().lock();
            err.write_all(output.as_bytes())?;
            err.flush()?;
        }
    }
    Ok(())
}

/// Reads the whole of stdin, so the CLI that wrote it never sees its write
/// fail, and keeps only the first [`HOOK_STDIN_PARSED_BYTES`] of it.
fn read_hook_stdin() -> Vec<u8> {
    let mut kept = Vec::new();
    let mut chunk = [0u8; 8192];
    let mut stdin = std::io::stdin().lock();
    loop {
        match stdin.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let room = HOOK_STDIN_PARSED_BYTES.saturating_sub(kept.len());
                kept.extend_from_slice(&chunk[..n.min(room)]);
            }
        }
    }
    kept
}

/// Reads a single length-prefixed frame from `client` and decodes it as a
/// [`DaemonMessage`]. Returns `None` on any I/O or decode failure.
async fn read_one_daemon_message(client: &mut ClientConnection) -> Option<DaemonMessage> {
    let mut len_buf = [0u8; 4];
    client.read_exact(&mut len_buf).await.ok()?;
    let len = u32::from_le_bytes(len_buf) as usize;
    let mut body = vec![0u8; len];
    client.read_exact(&mut body).await.ok()?;
    decode_daemon_message(&body).ok()
}

/// Sets up `tracing` with a file appender under `<state_base>/<namespace>/
/// logs/daemon.log` (`state_base` = [`yagura_os::default_state_base_dir`];
/// Windows: `%LOCALAPPDATA%`, macOS: `HOME/Library/Application Support`,
/// other Unix: `XDG_STATE_HOME` falling back to `HOME/.local/state`). Falls
/// back to stderr if the base directory cannot be resolved or the log file
/// cannot be created, warning once through the stderr subscriber right after
/// it is installed so the fallback is never silent. The level defaults to
/// `info` and can be overridden with the `YAGURA_LOG` environment variable.
///
/// On Unix the log directory and file are narrowed to owner-only
/// (`0700`/`0600`). A directory failure is fatal to the file appender (it
/// falls back to stderr); a file *restriction* failure is not, since the file
/// stays usable, so it is surfaced as a warning through the file-backed
/// subscriber once installed.
fn init_logging(namespace: &yagura_os::InstanceNamespace) {
    let level = std::env::var("YAGURA_LOG")
        .ok()
        .and_then(|value| parse_level(&value))
        .unwrap_or(tracing::Level::INFO);

    match open_log_file(namespace) {
        Ok((file, restrict_warning)) => {
            let _ = tracing_subscriber::fmt()
                .with_max_level(level)
                .with_ansi(false)
                .with_writer(std::sync::Mutex::new(file))
                .try_init();
            if let Some(reason) = restrict_warning {
                tracing::warn!("{reason}; the log file may not be owner-restricted");
            }
        }
        Err(reason) => {
            let _ = tracing_subscriber::fmt().with_max_level(level).try_init();
            tracing::warn!("{reason}; daemon logs are only visible on stderr for this run");
        }
    }
}

fn parse_level(value: &str) -> Option<tracing::Level> {
    match value.to_ascii_lowercase().as_str() {
        "trace" => Some(tracing::Level::TRACE),
        "debug" => Some(tracing::Level::DEBUG),
        "info" => Some(tracing::Level::INFO),
        "warn" | "warning" => Some(tracing::Level::WARN),
        "error" => Some(tracing::Level::ERROR),
        _ => None,
    }
}

/// Opens the daemon's log file (see [`init_logging`] for the resolved path).
///
/// Returns the opened file together with an optional restriction-failure
/// reason: unlike a directory or file-open failure (`Err`, fatal --
/// [`init_logging`] falls back to stderr), a failure to narrow the
/// already-open file's permissions still leaves a usable log file, so it is
/// reported as data alongside `Ok` for [`init_logging`] to warn about once
/// its subscriber is installed.
///
/// A symlinked log-directory component makes `secure_owned_dir` return `Err`
/// here, so a pre-planted symlink degrades the daemon to stderr-only logging
/// rather than letting it write into the link's target.
fn open_log_file(
    namespace: &yagura_os::InstanceNamespace,
) -> Result<(std::fs::File, Option<String>), String> {
    let base = yagura_os::default_state_base_dir()
        .ok_or_else(|| "failed to resolve the log directory".to_string())?;
    let dir = log_dir(&base, namespace);
    let path = dir.join("daemon.log");
    // Unlike the session database, the log directory has no explicit-path
    // override, so it is narrowed to owner-only unconditionally.
    yagura_os::secure_owned_dir(&base, &dir).map_err(|reason| {
        format!(
            "failed to prepare the log directory {}: {reason}",
            dir.display()
        )
    })?;
    // `secure_owned_dir` narrows an existing directory but only creates one
    // on Unix, so Windows needs this to reach a first-run log.
    std::fs::create_dir_all(&dir).map_err(|err| {
        format!(
            "failed to create the log directory {}: {err}",
            dir.display()
        )
    })?;

    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options
        .open(&path)
        .map_err(|err| format!("failed to open the log file {}: {err}", path.display()))?;

    // Idempotent for a file that predates this restriction: `OpenOptions`'s
    // `mode` only governs the file's permissions at creation time, not for
    // one that already existed.
    let restrict_warning = yagura_os::secure_owned_file(&path).err();
    Ok((file, restrict_warning))
}
