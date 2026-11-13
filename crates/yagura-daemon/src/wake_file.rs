//! A window's wake file and the answers its open leaves for its hooks.
//!
//! Whether the daemon can ask a window's CLI to wait again, where it writes
//! to ask and which path the CLI watches are facts of the window's open: the
//! open decides them once ([`WakeRecord`]), and the window keeps them with its
//! row and its holder's launch record. The paths reach the CLI through the
//! answers the open leaves beside the window's credentials file, which the
//! answer door (`--print-answer`) prints without asking the daemon, so the
//! CLI learns them even while no daemon runs.
//!
//! A CLI in a WSL guest watches a file in the guest's own state, in a slot
//! numbered per guest base: the lowest number no living or opening window of
//! this daemon holds. A slot's folder and file are never removed, so a slot
//! is used again rather than cleared.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tokio::sync::Semaphore;
use tokio::time::Instant;

use crate::config::DaemonConfig;
use crate::pty::{self, lock_recover};

/// The file a window's CLI watches to be woken.
pub const WAKE_FILE_NAME: &str = "yagura_wake";

/// The placeholder an answer names for the JSON array of the paths the
/// window's CLI watches.
pub const WAKE_PATHS_PLACEHOLDER: &str = "{wake_paths}";

/// The folder of the instance in a guest's state base that holds the slots.
const HOST_WAKE_DIR: &str = "host-wake";

/// What an answer's file is named after, followed by the hook event name.
const ANSWER_FILE_PREFIX: &str = "answer-";

/// The largest answer the answer door prints, and the open writes. An answer
/// is one hook's output, a few hundred bytes for the paths a CLI watches.
pub const ANSWER_FILE_MAX_BYTES: u64 = 64 * 1024;

/// Where a window's wake file is, as its open decided it: kept with the
/// window for its whole life.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WakeRecord {
    /// The path the daemon writes the wake file at.
    pub write: String,
    /// The path the window's CLI watches, as the CLI's environment names it.
    pub cli: String,
    /// The guest slot the file is in; `None` for a CLI the daemon runs
    /// directly, whose file is in its credentials folder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slot: Option<WakeSlotId>,
}

impl WakeRecord {
    /// The record `text` spells in the window table; `None` for text that is
    /// no record, which its reader takes as not kept.
    pub(crate) fn named(text: &str) -> Option<Self> {
        serde_json::from_str(text).ok()
    }

    /// Its spelling in the window table.
    pub(crate) fn spelled(&self) -> serde_json::Result<String> {
        serde_json::to_string(self)
    }
}

/// One slot under a guest's state base.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WakeSlotId {
    /// The instance's folder in the guest's state base, as the guest names
    /// it: the slots are numbered per base.
    pub base: String,
    /// The slot's number.
    pub number: u32,
}

/// The instance's folder in a guest's state base, as the guest names it and
/// as the daemon reaches it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestBase {
    cli: String,
    daemon: String,
}

/// Where a window's open puts its wake file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WakePlace {
    /// In the window's credentials folder: the CLI runs on this host.
    Folder,
    /// In a slot under a guest's state base.
    Guest(GuestBase),
}

/// What a window's open leaves beside its credentials file for its hooks:
/// the `[hooks]` table's answers and where its wake file goes, `None` when
/// the window has no wake record.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HookAnswers {
    /// Hook event name to the text as the profile wrote it.
    pub texts: BTreeMap<String, String>,
    /// Where the window's wake file goes; `None` when it has none.
    pub wake: Option<WakePlace>,
}

impl HookAnswers {
    /// Whether an answer names [`WAKE_PATHS_PLACEHOLDER`]: the window's CLI
    /// is told of a wake file only through one.
    #[must_use]
    pub fn names_wake_paths(&self) -> bool {
        self.texts
            .values()
            .any(|text| text.contains(WAKE_PATHS_PLACEHOLDER))
    }
}

/// The name of the file the answer for the hook event `name` is left in.
#[must_use]
pub fn answer_file_name(name: &str) -> String {
    format!("{ANSWER_FILE_PREFIX}{name}")
}

/// The answers to leave beside a window's credentials file, as file name and
/// content: `{wake_paths}` replaced in one pass by the JSON array of the
/// path its CLI watches, and an answer naming it left out when the window
/// has no wake record.
#[must_use]
pub(crate) fn rendered_answers(
    texts: &BTreeMap<String, String>,
    record: Option<&WakeRecord>,
) -> Vec<(String, String)> {
    let paths = record.and_then(|record| serde_json::to_string(&[record.cli.as_str()]).ok());
    texts
        .iter()
        .filter_map(|(name, text)| {
            let text = if text.contains(WAKE_PATHS_PLACEHOLDER) {
                text.replace(WAKE_PATHS_PLACEHOLDER, paths.as_deref()?)
            } else {
                text.clone()
            };
            Some((answer_file_name(name), text))
        })
        .collect()
}

/// Blocking: writes `answers` into the credentials folder `dir`.
///
/// # Errors
///
/// An answer larger than [`ANSWER_FILE_MAX_BYTES`], or a write failing.
pub(crate) fn write_answers(dir: &Path, answers: &[(String, String)]) -> std::io::Result<()> {
    for (file_name, text) in answers {
        if u64::try_from(text.len()).unwrap_or(u64::MAX) > ANSWER_FILE_MAX_BYTES {
            return Err(std::io::Error::other("an answer is larger than accepted"));
        }
        std::fs::write(dir.join(file_name), text)?;
    }
    Ok(())
}

/// Blocking: the answer for the hook event `name` beside `credentials_file`,
/// read only when that file reads as a window's credentials file
/// ([`yagura_protocol::window_credentials`]); `None` when there is none, or
/// it is larger than [`ANSWER_FILE_MAX_BYTES`].
#[must_use]
pub fn answer_beside(credentials_file: &Path, name: &str) -> Option<Vec<u8>> {
    use std::io::Read as _;

    if !crate::profile_hook_events::is_hook_event_name(name) {
        return None;
    }
    yagura_protocol::window_credentials(None, None, Some(credentials_file))?;
    let path = credentials_file.parent()?.join(answer_file_name(name));
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(ANSWER_FILE_MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    (u64::try_from(bytes.len()).ok()? <= ANSWER_FILE_MAX_BYTES).then_some(bytes)
}

/// The slots this daemon's windows hold, per guest base: a number with its
/// count of holders.
#[derive(Debug, Clone, Default)]
pub(crate) struct WakeSlots {
    held: Arc<Mutex<BTreeMap<String, BTreeMap<u32, usize>>>>,
}

impl WakeSlots {
    /// The wake record of a window whose open put its wake file at `place`,
    /// `folder` being its credentials folder, with the slot it takes; `None`
    /// when the folder cannot be named for the CLI.
    pub(crate) fn record(
        &self,
        place: &WakePlace,
        folder: &Path,
    ) -> Option<(WakeRecord, Option<WakeSlot>)> {
        match place {
            WakePlace::Folder => {
                let record = WakeRecord {
                    write: folder.join(WAKE_FILE_NAME).to_str()?.to_string(),
                    cli: crate::window::folder_file_argument(folder, WAKE_FILE_NAME)?,
                    slot: None,
                };
                Some((record, None))
            }
            WakePlace::Guest(base) => {
                let slot = self.take(&base.cli);
                let number = slot.number;
                let record = WakeRecord {
                    write: format!(
                        "{}\\{HOST_WAKE_DIR}\\{number}\\{WAKE_FILE_NAME}",
                        base.daemon
                    ),
                    cli: format!("{}/{HOST_WAKE_DIR}/{number}/{WAKE_FILE_NAME}", base.cli),
                    slot: Some(WakeSlotId {
                        base: base.cli.clone(),
                        number,
                    }),
                };
                Some((record, Some(slot)))
            }
        }
    }

    /// The slot `id` a window taken back holds, held again.
    pub(crate) fn hold(&self, id: &WakeSlotId) -> WakeSlot {
        *lock_recover(&self.held)
            .entry(id.base.clone())
            .or_default()
            .entry(id.number)
            .or_default() += 1;
        WakeSlot {
            slots: self.clone(),
            base: id.base.clone(),
            number: id.number,
        }
    }

    /// The lowest slot under `base` no window holds, taken.
    fn take(&self, base: &str) -> WakeSlot {
        let number = {
            let mut held = lock_recover(&self.held);
            let numbers = held.entry(base.to_string()).or_default();
            let number = (0..)
                .find(|number| !numbers.contains_key(number))
                .unwrap_or(u32::MAX);
            *numbers.entry(number).or_default() += 1;
            number
        };
        WakeSlot {
            slots: self.clone(),
            base: base.to_string(),
            number,
        }
    }
}

/// A window's hold on its slot, given back when dropped. Touches no file: a
/// slot's folder stays for the next window that takes it.
#[derive(Debug)]
pub(crate) struct WakeSlot {
    slots: WakeSlots,
    base: String,
    number: u32,
}

impl Drop for WakeSlot {
    fn drop(&mut self) {
        let mut held = lock_recover(&self.slots.held);
        let Some(numbers) = held.get_mut(&self.base) else {
            return;
        };
        if let Some(count) = numbers.get_mut(&self.number) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                numbers.remove(&self.number);
            }
        }
        if numbers.is_empty() {
            held.remove(&self.base);
        }
    }
}

/// Where a window keeps its slot until its credentials folder goes: given
/// back with the folder, also when the slot is known only after the folder
/// is in place (a window taken back without its row).
#[derive(Debug, Clone, Default)]
pub(crate) struct WakeHold(Arc<Mutex<HoldState>>);

#[derive(Debug, Default)]
enum HoldState {
    #[default]
    Empty,
    /// Held only to be dropped with the hold.
    Holding {
        _slot: WakeSlot,
    },
    Released,
}

impl WakeHold {
    /// Keeps `slot` until [`WakeHold::release`]; a hold already released
    /// gives it back at once.
    pub(crate) fn keep(&self, slot: WakeSlot) {
        let mut state = lock_recover(&self.0);
        if matches!(*state, HoldState::Released) {
            drop(state);
            drop(slot);
            return;
        }
        let previous = std::mem::replace(&mut *state, HoldState::Holding { _slot: slot });
        drop(state);
        drop(previous);
    }

    /// Gives the slot back, and every slot kept after this.
    pub(crate) fn release(&self) {
        let previous = std::mem::replace(&mut *lock_recover(&self.0), HoldState::Released);
        drop(previous);
    }
}

/// How a window's open reads a guest's state base: the profile's
/// `[host_paths] state_base` argv and environment, under the launch slots a
/// path conversion takes.
pub(crate) struct StateBaseTool {
    pub(crate) argv: Vec<String>,
    pub(crate) env: Vec<(OsString, OsString)>,
    pub(crate) permits: Arc<Semaphore>,
}

/// The instance's folder in the guest's state base `tool` prints, or `None`
/// (with one warning) when it cannot be had within
/// `config.mcp_listing_timeout`: the window then opens without a wake record.
pub(crate) async fn read_guest_base(
    config: &DaemonConfig,
    profile: &str,
    tool: StateBaseTool,
) -> Option<GuestBase> {
    let read = guest_base(config, tool).await;
    if let Err(reason) = &read {
        tracing::warn!(
            profile,
            reason,
            "the guest's state base could not be had; the window opens without a wake file, so \
             a message for it waits for its CLI's next event"
        );
    }
    read.ok()
}

async fn guest_base(config: &DaemonConfig, tool: StateBaseTool) -> Result<GuestBase, &'static str> {
    let namespace = yagura_os::resolve_instance_from(config.instance_id.as_deref().map(OsStr::new))
        .map_err(|_| "the instance has no name a guest folder can be given")?;
    // The same bound and slots as a conversion: both run the guest's tools
    // as the window opens.
    let deadline = Instant::now() + config.mcp_listing_timeout;
    let Ok(Ok(_slot)) = tokio::time::timeout_at(deadline, tool.permits.acquire()).await else {
        return Err("no slot to run the state base tool in");
    };
    let resolve_config = config.clone();
    let argv = tool.argv;
    let (spawn_argv, cwd) = tokio::task::spawn_blocking(move || {
        let spawn_argv = crate::profile::resolve_spawn_argv(&resolve_config, &argv)?;
        // An empty directory, as a conversion starts in: the guest reads no
        // project's settings from it.
        let cwd: Option<PathBuf> = resolve_config
            .registration_cwd
            .as_ref()
            .filter(|dir| dir.ensure().is_ok())
            .map(|dir| dir.path().to_path_buf());
        Ok::<_, &'static str>((spawn_argv, cwd))
    })
    .await
    .map_err(|_| "the state base tool could not be resolved")??;
    let captured = pty::spawn_one_shot(
        spawn_argv,
        cwd,
        pty::CaptureChildEnv::Based {
            base: config.child_base_env.clone(),
            overlay: tool.env,
        },
        None,
    )
    .await
    .map_err(|_| "the state base tool could not be started")?;
    let limits = yagura_os::CaptureLimits {
        timeout: deadline.saturating_duration_since(Instant::now()),
        max_output_bytes: config.mcp_registration_max_output_bytes,
    };
    // A timeout ends the tool's whole tree before returning.
    let outcome = tokio::task::spawn_blocking(move || captured.collect(limits))
        .await
        .unwrap_or(yagura_os::CaptureOutcome::TimedOut);
    match outcome {
        yagura_os::CaptureOutcome::Exited {
            code: Some(0),
            stdout,
            ..
        } => guest_base_printed(&stdout, &namespace)
            .ok_or("the state base tool printed no usable base"),
        yagura_os::CaptureOutcome::Exited { .. } => Err("the state base tool failed"),
        yagura_os::CaptureOutcome::TimedOut => Err("the state base tool timed out"),
        yagura_os::CaptureOutcome::OutputTooLarge => {
            Err("the state base tool's output was larger than accepted")
        }
    }
}

/// The instance's folder in the state base `stdout` names: its first line the
/// base as the guest names it (absolute), its second the UNC path the host
/// reaches it by, neither holding a control character. A guest whose state
/// lies on a host drive prints a host path there, which is no guest base.
fn guest_base_printed(
    stdout: &[u8],
    namespace: &yagura_os::InstanceNamespace,
) -> Option<GuestBase> {
    let text = std::str::from_utf8(stdout).ok()?;
    let mut lines = text.lines();
    let (Some(linux), Some(unc), None) = (lines.next(), lines.next(), lines.next()) else {
        return None;
    };
    let usable = linux.starts_with('/')
        && (unc.starts_with("\\\\wsl.localhost\\") || unc.starts_with("\\\\wsl$\\"))
        && !linux.chars().chain(unc.chars()).any(char::is_control);
    usable.then(|| GuestBase {
        cli: yagura_os::guest_instance_dir(linux.trim_end_matches('/'), '/', namespace),
        daemon: yagura_os::guest_instance_dir(unc.trim_end_matches('\\'), '\\', namespace),
    })
}
