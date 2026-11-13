//! Every window's delivery agent: one task per window that pushes what the
//! window's inbox holds, when the window's profile gives the daemon a way to,
//! and settles whatever is left once the window ends.
//!
//! How it pushes is the profile's `[delivery]` `wake` -- data, never the name
//! of a CLI:
//!
//! - `wait`: answers the wait the CLI left registered, whenever the window can
//!   be pushed to again. With none registered, a window whose open named a
//!   wake file has its CLI asked to register one again: the daemon writes the
//!   file once, and asks again only after the CLI answered (a door of the
//!   window arrived, a wait registered) or the window's status changed.
//! - `command`: runs the profile's own program with the wake arguments, once
//!   the CLI has reported its session. A failed run puts the parcels back and
//!   is not repeated until something changes.
//! - `none`: pushes nothing; what waits is read by the window's doors at the
//!   CLI's next turn.
//!
//! Neither pushes to a window whose status is `Unknown`. A parcel a push
//! carries is out of reach of the window's doors until the push ends, and goes
//! back in front of the inbox if nothing reached the CLI.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{oneshot, watch};
use yagura_protocol::WindowStatus;

use crate::config::DaemonConfig;
use crate::inbox::{Deposited, Parcel, ParcelEnd, Place, PushEnd, fill_wake_argument};
use crate::profile_delivery::WakeMode;
use crate::pty;
use crate::waiter::StatusAt;
use crate::wake_file::WakeWriter;
use crate::window::{WindowEntry, WindowLiveness, WindowRegistry};

use super::report::ReportInboxes;

/// How long a wake command may run before it is given up on; only a hang
/// reaches it.
pub(super) const WAKE_COMMAND_TIMEOUT: Duration = Duration::from_secs(30);

/// The most output a wake command's run is read for.
const WAKE_COMMAND_MAX_OUTPUT_BYTES: usize = 64 * 1024;

/// What every window's delivery agent works with. Held strongly by the IPC
/// server and by each running agent; the registry's launcher holds it
/// weakly, so the registry never keeps it alive.
pub(super) struct NoticeAgents {
    registry: Arc<WindowRegistry>,
    reports: Arc<ReportInboxes>,
    config: DaemonConfig,
    wake_writer: WakeWriter,
}

impl NoticeAgents {
    /// Installs the launcher on `registry`, which also starts an agent for
    /// every window whose inbox already holds something.
    pub(super) fn install(
        registry: &Arc<WindowRegistry>,
        reports: Arc<ReportInboxes>,
    ) -> Arc<Self> {
        let agents = Arc::new(Self {
            registry: Arc::clone(registry),
            reports,
            config: registry.config().clone(),
            wake_writer: WakeWriter::new(),
        });
        let launcher_agents = Arc::downgrade(&agents);
        registry.set_notice_launcher(Box::new(move |entry: &Arc<WindowEntry>| {
            if let Some(agents) = launcher_agents.upgrade() {
                launch(&agents, entry);
            }
        }));
        agents
    }
}

/// Starts `entry`'s agent, or wakes the one already running.
fn launch(agents: &Arc<NoticeAgents>, entry: &Arc<WindowEntry>) {
    // Only unreachable outside a runtime (a drop during shutdown), where
    // nothing could deliver anyway.
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        return;
    };
    if entry.claim_notice_agent() {
        runtime.spawn(run_agent(Arc::clone(agents), Arc::clone(entry)));
    } else {
        entry.wake_notice_agent();
    }
}

/// Why [`deposit`] turned a parcel away at once.
pub(super) enum Refused {
    /// The window has ended, or is ending.
    Ended,
    /// The window's inbox has no room for it.
    Full,
}

/// What [`deposit`] tells its depositor about the parcel it put in.
pub(super) struct Tracked {
    /// Yields how the parcel left the inbox.
    pub(super) end: oneshot::Receiver<ParcelEnd>,
    /// Whether the deposit itself answered the CLI's registered wait with it.
    pub(super) handed_to_wait: bool,
    /// Where the parcel is until it ends.
    pub(super) place: watch::Receiver<Place>,
}

/// Puts `parcel` in `entry`'s inbox and wakes the window's agent. A full
/// inbox turns the new parcel away and never pushes an older one out, except
/// that a notice is always taken in.
///
/// # Errors
///
/// [`Refused`] when the inbox would not take it.
pub(super) fn deposit(
    registry: &WindowRegistry,
    entry: &Arc<WindowEntry>,
    parcel: Parcel,
) -> Result<Tracked, Refused> {
    let mut deposited = entry.inbox().deposit_tracked(parcel);
    let handed_to_wait = deposited.handed_to_wait;
    let place = deposited.place.take();
    let end = after_deposit(registry, entry, deposited)?;
    // A parcel taken in always comes with its place.
    let Some(place) = place else {
        return Err(Refused::Ended);
    };
    Ok(Tracked {
        end,
        handed_to_wait,
        place,
    })
}

/// Whether the window's agent pushes what waits to `entry` by running its
/// CLI's wake command, as soon as it can: the window wakes by command, can be
/// pushed to now, has a program to run and a session to name.
pub(super) fn wakes_by_command_now(entry: &WindowEntry) -> bool {
    matches!(
        entry.delivery().map(|delivery| &delivery.wake),
        Some(WakeMode::Command(_))
    ) && pushable(entry)
        && entry.session_id().is_some()
        && entry.wake_command().is_some()
}

/// What [`deposit`] does once the inbox has answered, for a caller that
/// deposits into the inbox itself: logs what was turned away and wakes the
/// window's agent.
///
/// # Errors
///
/// [`Refused`] when the inbox did not take the parcel.
pub(super) fn after_deposit(
    registry: &WindowRegistry,
    entry: &Arc<WindowEntry>,
    deposited: Deposited,
) -> Result<oneshot::Receiver<ParcelEnd>, Refused> {
    let Deposited {
        turned_away, end, ..
    } = deposited;
    if !turned_away.is_empty() {
        let keys: Vec<&str> = turned_away
            .iter()
            .map(|parcel| parcel.key.as_str())
            .collect();
        tracing::info!(
            window_id = entry.window_id(),
            ?keys,
            "parcel turned away by the inbox"
        );
    }
    let Some(end) = end else {
        return Err(
            if matches!(
                registry.liveness(entry.window_id()),
                WindowLiveness::Alive(_)
            ) {
                Refused::Full
            } else {
                Refused::Ended
            },
        );
    };
    registry.wake_notices(entry);
    Ok(end)
}

async fn run_agent(agents: Arc<NoticeAgents>, entry: Arc<WindowEntry>) {
    let window_id = entry.window_id().to_string();
    let mut status = entry.watch_status();
    // A failed wake command is not run again until something changes.
    let mut command_failed = false;
    let mut woke_by_status = false;
    // A wake file that could not be written is warned of once per window.
    let mut ask_failed_before = false;
    while matches!(
        agents.registry.liveness(&window_id),
        WindowLiveness::Alive(_)
    ) {
        // A status change answers an ask, also one made while the agent was
        // busy, which `idle` never sees.
        if std::mem::take(&mut woke_by_status) | status.has_changed().unwrap_or(false) {
            let _ = entry.inbox().clear_ask();
        }
        status.borrow_and_update();
        let wake = entry.delivery().map(|delivery| delivery.wake.clone());
        match wake {
            Some(WakeMode::Wait) => {
                entry.inbox().wake_waiter();
                ask_to_wait(&agents, &entry, &mut ask_failed_before).await;
            }
            Some(WakeMode::Command(args)) => {
                if !command_failed
                    && pushable(&entry)
                    && entry.session_id().is_some()
                    && entry.inbox().pending_count() > 0
                {
                    command_failed = !push_by_command(&agents, &entry, &args).await;
                    continue;
                }
            }
            // Nothing is pushed to a window whose CLI has no way to be woken,
            // or that declares no delivery; the agent still runs so that its
            // report inbox is discarded once it ends.
            Some(WakeMode::None) | None => {}
        }
        match idle(&entry, &mut status).await {
            Woke::Ended => break,
            Woke::Status => woke_by_status = true,
            Woke::Notice => {}
        }
        command_failed = false;
    }
    finish(&agents, &entry);
}

/// Asks the CLI of `entry`, a window whose CLI waits for the daemon, to
/// register a wait again by writing the wake file its open named, when a
/// parcel can be pushed and no wait is registered
/// ([`crate::inbox::Inbox::ask_due`]). The ask stands until the CLI answers
/// it; one that could not be written is taken back.
async fn ask_to_wait(agents: &NoticeAgents, entry: &WindowEntry, failed_before: &mut bool) {
    let Some(record) = entry.wake_record() else {
        return;
    };
    if !entry.inbox().ask_due() {
        return;
    }
    if let Err(err) = agents.wake_writer.write(record).await {
        let _ = entry.inbox().clear_ask();
        if !std::mem::replace(failed_before, true) {
            tracing::warn!(
                window_id = entry.window_id(),
                error = %err,
                "could not write the window's wake file; what waits arrives at the CLI's next turn"
            );
        }
    }
}

/// Whether the daemon may push to `entry` now.
fn pushable(entry: &WindowEntry) -> bool {
    !matches!(
        entry.current_status(),
        WindowStatus::Unknown | WindowStatus::Exited
    )
}

/// What woke an idle agent.
enum Woke {
    /// The window's status changed.
    Status,
    /// A wake: a new parcel, a session reported, a door arrived.
    Notice,
    /// The window's status stream is gone.
    Ended,
}

/// Waits for something that may give the agent work.
async fn idle(entry: &WindowEntry, status: &mut watch::Receiver<StatusAt>) -> Woke {
    tokio::select! {
        changed = status.changed() => if changed.is_ok() { Woke::Status } else { Woke::Ended },
        () = entry.notice_woken() => Woke::Notice,
    }
}

/// Hands what waits to the window's CLI by running the profile's own program
/// with `args`. Returns whether it did.
async fn push_by_command(agents: &NoticeAgents, entry: &WindowEntry, args: &[String]) -> bool {
    let Some(session) = entry.session_id() else {
        return false;
    };
    let Some(wake) = entry.wake_command().cloned() else {
        tracing::warn!(
            window_id = entry.window_id(),
            "the window's profile wakes by command, but it was opened with no program to run"
        );
        return false;
    };
    let Some(push) = entry.inbox().begin_push() else {
        return true;
    };
    let mut argv = wake.argv;
    argv.extend(
        args.iter()
            .map(|arg| fill_wake_argument(arg, &session, &push.text)),
    );
    let ran = run_wake_command(&agents.config, argv, wake.env, PathBuf::from(entry.cwd())).await;
    tracing::info!(window_id = entry.window_id(), ran, "wake command finished");
    entry
        .inbox()
        .finish_push(push.id, if ran { PushEnd::Taken } else { PushEnd::Failed });
    ran
}

/// Runs `argv` once, with no shell, and returns whether it exited with 0.
/// A program that resolves to a Windows `.cmd` / `.bat` file is never run:
/// the command interpreter would read the text as its own syntax.
async fn run_wake_command(
    config: &DaemonConfig,
    argv: Vec<String>,
    env: Vec<(String, String)>,
    cwd: PathBuf,
) -> bool {
    let platform = config.host_os.platform();
    let predicate = crate::profile::is_executable_predicate(config);
    let resolution = match yagura_os::program::resolve_program(
        &argv,
        platform,
        &config.program_lookup,
        &predicate,
    ) {
        Ok(resolution) => resolution,
        Err(_) => {
            tracing::warn!("the wake command's program could not be resolved");
            return false;
        }
    };
    if matches!(resolution, yagura_os::program::Resolution::CmdShim { .. }) {
        tracing::warn!("the wake command's program is a .cmd / .bat file and is not run");
        return false;
    }
    let spawn_argv = yagura_os::program::into_spawn_argv(&resolution, &argv, platform);
    let overlay = env
        .into_iter()
        .map(|(name, value)| (name.into(), value.into()))
        .collect();
    let child = match pty::spawn_one_shot(
        spawn_argv,
        Some(cwd),
        pty::CaptureChildEnv::Based {
            base: config.child_base_env.clone(),
            overlay,
        },
        None,
    )
    .await
    {
        Ok(child) => child,
        Err(err) => {
            tracing::warn!(error = %err, "the wake command could not be started");
            return false;
        }
    };
    let limits = yagura_os::CaptureLimits {
        timeout: WAKE_COMMAND_TIMEOUT,
        max_output_bytes: WAKE_COMMAND_MAX_OUTPUT_BYTES,
    };
    let outcome = tokio::task::spawn_blocking(move || child.collect(limits)).await;
    matches!(
        outcome,
        Ok(yagura_os::CaptureOutcome::Exited { code: Some(0), .. })
    )
}

/// Settles what the window never got once it has ended, then drops its
/// report inbox: window ids are never reused, so nobody could read it again.
fn finish(agents: &NoticeAgents, entry: &WindowEntry) {
    // A daemon that is stopping leaves its posts for the next one to settle.
    let end = if agents.registry.is_shutting_down() {
        ParcelEnd::Stopped
    } else {
        ParcelEnd::Closed
    };
    entry.inbox().close(end);
    agents.reports.discard(entry.window_id());
    entry.release_notice_agent();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wake_argument_is_filled_in_once_and_what_it_carries_is_not_read_again() {
        assert_eq!(
            fill_wake_argument("--thread={session}", "s-1", "x"),
            "--thread=s-1"
        );
        assert_eq!(
            fill_wake_argument("{text}", "s-1", "say {session} and {text}"),
            "say {session} and {text}"
        );
    }
}
