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
//!
//! A slot's folder is made as the window opens, before its CLI starts: a CLI
//! registers what it watches as it starts and sees nothing made in a folder
//! that was not there then. Asking a CLI to wait writes the file only, never
//! its folder.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

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

/// The most blocking wake I/O the daemon runs at once for one guest base,
/// and for the wake files on this host. One small file or folder each, though
/// one in a WSL guest may wait on the guest's file server.
pub(crate) const WAKE_WRITES_MAX_CONCURRENT: usize = 4;

/// Bounds the blocking I/O on wake files and slot folders. A guest base's
/// I/O waits on that guest's file server, so each base has permits of its
/// own and one stuck guest stalls neither another base nor the files on this
/// host, which share permits apart from every guest's. Waiting for a permit
/// and for the I/O together ends within one bound.
#[derive(Debug, Clone)]
pub(crate) struct WakeIo {
    guests: Arc<Mutex<BTreeMap<String, Arc<Semaphore>>>>,
    local: Arc<Semaphore>,
    bound: Duration,
}

impl WakeIo {
    /// Wake I/O whose every wait ends within `bound`.
    pub(crate) fn new(bound: Duration) -> Self {
        Self {
            guests: Arc::default(),
            local: Arc::new(Semaphore::new(WAKE_WRITES_MAX_CONCURRENT)),
            bound,
        }
    }

    /// The permits the I/O of a record in `slot` takes: its base's, or this
    /// host's for a record in no slot.
    fn permits(&self, slot: Option<&WakeSlotId>) -> Arc<Semaphore> {
        let Some(slot) = slot else {
            return Arc::clone(&self.local);
        };
        let mut guests = lock_recover(&self.guests);
        // A base no I/O holds permits of keeps no entry.
        guests.retain(|_, permits| Arc::strong_count(permits) > 1);
        Arc::clone(
            guests
                .entry(slot.base.clone())
                .or_insert_with(|| Arc::new(Semaphore::new(WAKE_WRITES_MAX_CONCURRENT))),
        )
    }

    /// Runs `io` off the runtime's threads under the permits of `slot`. A
    /// run past the bound keeps its permit until it ends, since blocking I/O
    /// cannot be stopped.
    async fn run<F>(&self, slot: Option<&WakeSlotId>, io: F) -> std::io::Result<()>
    where
        F: FnOnce() -> std::io::Result<()> + Send + 'static,
    {
        let permits = self.permits(slot);
        let ran = tokio::time::timeout(self.bound, async move {
            let permit = permits
                .acquire_owned()
                .await
                .map_err(std::io::Error::other)?;
            tokio::task::spawn_blocking(move || {
                let _permit = permit;
                io()
            })
            .await
            .map_err(std::io::Error::other)?
        })
        .await;
        ran.unwrap_or_else(|_| {
            Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "the wake I/O did not end in time",
            ))
        })
    }

    /// Makes the folder of the wake file `record` names, in its guest slot.
    ///
    /// # Errors
    ///
    /// The path has no folder, making it fails, or it is not made within the
    /// bound.
    pub(crate) async fn make_slot_folder(&self, record: &WakeRecord) -> std::io::Result<()> {
        let folder = Path::new(&record.write)
            .parent()
            .ok_or_else(|| std::io::Error::other("the wake file has no folder"))?
            .to_path_buf();
        self.run(record.slot.as_ref(), move || {
            std::fs::create_dir_all(folder)
        })
        .await
    }
}

/// Writes wake files: each write leaves the file holding text it never held
/// before, also across daemon restarts, since a CLI's watcher may not see a
/// write of the same text as a change.
pub(crate) struct WakeWriter {
    io: WakeIo,
    /// Names this daemon's run in every text it writes.
    run: String,
    written: AtomicU64,
}

impl WakeWriter {
    /// A writer whose I/O counts against `io`, shared with the opens that
    /// make the slot folders.
    pub(crate) fn new(io: WakeIo) -> Self {
        let since_epoch = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_nanos());
        Self {
            io,
            run: format!("{}-{since_epoch}", crate::handoff::new_envelope_id()),
            written: AtomicU64::new(0),
        }
    }

    /// Writes the wake file `record` names, off the runtime's threads. Its
    /// folder is not made: one its window's open did not leave is one its CLI
    /// does not watch.
    ///
    /// # Errors
    ///
    /// The file is a link, writing it fails (also when its folder is gone),
    /// or it is not written within the bound.
    pub(crate) async fn write(&self, record: &WakeRecord) -> std::io::Result<()> {
        let text = format!(
            "{}-{}\n",
            self.run,
            self.written.fetch_add(1, Ordering::Relaxed)
        );
        let path = PathBuf::from(&record.write);
        self.io
            .run(record.slot.as_ref(), move || write_wake_file(&path, &text))
            .await
    }
}

/// Blocking: writes `text` over the wake file at `path`. A link is never
/// written through: the file is the daemon's own.
fn write_wake_file(path: &Path, text: &str) -> std::io::Result<()> {
    if std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink()) {
        return Err(std::io::Error::other("the wake file is a link"));
    }
    std::fs::write(path, text)
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
/// reaches it by (absolute on this host, so no path on a Unix one), neither
/// holding a control character. A guest whose state
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
        && Path::new(unc).is_absolute()
        && !linux.chars().chain(unc.chars()).any(char::is_control);
    usable.then(|| GuestBase {
        cli: yagura_os::guest_instance_dir(linux.trim_end_matches('/'), '/', namespace),
        daemon: yagura_os::guest_instance_dir(unc.trim_end_matches('\\'), '\\', namespace),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn guest(cli: &str, daemon: &str) -> WakePlace {
        WakePlace::Guest(GuestBase {
            cli: cli.to_string(),
            daemon: daemon.to_string(),
        })
    }

    fn take(slots: &WakeSlots, place: &WakePlace) -> (WakeRecord, WakeSlot) {
        let (record, slot) = slots.record(place, Path::new("unused")).unwrap();
        (record, slot.unwrap())
    }

    fn number(slots: &WakeSlots, place: &WakePlace) -> (u32, WakeSlot) {
        let (record, slot) = take(slots, place);
        (record.slot.unwrap().number, slot)
    }

    #[test]
    fn a_guest_record_takes_the_lowest_free_slot_counted_per_base() {
        let slots = WakeSlots::default();
        let one = guest("/home/a/yagura", r"\\wsl.localhost\U\home\a\yagura");
        let other = guest("/home/b/yagura", r"\\wsl.localhost\U\home\b\yagura");

        let (first, _first) = number(&slots, &one);
        let (second, _second) = number(&slots, &one);
        let (elsewhere, _elsewhere) = number(&slots, &other);

        assert_eq!((first, second), (0, 1));
        assert_eq!(elsewhere, 0, "another base counts its slots from 0");
    }

    #[test]
    fn a_dropped_slot_is_given_back_and_taken_again_lowest_first() {
        let slots = WakeSlots::default();
        let place = guest("/home/a/yagura", r"\\wsl.localhost\U\home\a\yagura");
        let (_, zero) = number(&slots, &place);
        let (_, one) = number(&slots, &place);
        let (_, _two) = number(&slots, &place);

        drop(one);
        let (middle, _middle) = number(&slots, &place);
        assert_eq!(middle, 1, "the freed middle slot is the lowest free one");

        drop(zero);
        let (lowest, _lowest) = number(&slots, &place);
        assert_eq!(lowest, 0);
        let (next, _next) = number(&slots, &place);
        assert_eq!(next, 3);
    }

    #[test]
    fn a_held_slot_is_not_taken_and_frees_only_when_every_holder_drops() {
        let slots = WakeSlots::default();
        let base = "/home/a/yagura";
        let place = guest(base, r"\\wsl.localhost\U\home\a\yagura");
        let zero = WakeSlotId {
            base: base.to_string(),
            number: 0,
        };

        let held = slots.hold(&zero);
        let (taken, _taken) = number(&slots, &place);
        assert_eq!(taken, 1, "a slot held again is not taken by a new record");
        drop(held);

        let (recorded, by_record) = number(&slots, &place);
        assert_eq!(recorded, 0);
        let by_hold = slots.hold(&zero);
        drop(by_record);
        let (while_held, _while_held) = number(&slots, &place);
        assert_eq!(
            while_held, 2,
            "slot 0 still has a holder after one of its two drops"
        );

        drop(by_hold);
        let (freed, _freed) = number(&slots, &place);
        assert_eq!(freed, 0, "slot 0 frees once its last holder drops");
    }

    #[test]
    fn a_wake_hold_gives_its_slot_back_on_release_or_on_a_newer_keep() {
        let slots = WakeSlots::default();
        let place = guest("/home/a/yagura", r"\\wsl.localhost\U\home\a\yagura");

        let hold = WakeHold::default();
        let (_, zero) = number(&slots, &place);
        hold.keep(zero);
        let (_, one) = number(&slots, &place);
        assert_eq!(number(&slots, &place).0, 2, "a kept slot stays taken");
        hold.keep(one);
        assert_eq!(
            number(&slots, &place).0,
            0,
            "a newer keep gives the slot kept before back"
        );

        hold.release();
        let (first, _first) = number(&slots, &place);
        let (second, _second) = number(&slots, &place);
        assert_eq!((first, second), (0, 1), "release gives the kept slot back");

        let (late, slot) = number(&slots, &place);
        hold.keep(slot);
        assert_eq!(
            number(&slots, &place).0,
            late,
            "a keep after release gives the slot back at once"
        );
    }

    #[test]
    fn a_guest_record_names_its_slot_paths_keeping_the_base_text_as_it_is() {
        let slots = WakeSlots::default();
        let cli = "/home/a \"b\\c {wake_paths}/yagura";
        let daemon = r#"\\wsl.localhost\U\home\a "b\c {wake_paths}\yagura"#;
        let place = guest(cli, daemon);
        let (_, _zero) = number(&slots, &place);

        let (record, _one) = take(&slots, &place);
        assert_eq!(
            record.write,
            format!("{daemon}\\host-wake\\1\\{WAKE_FILE_NAME}")
        );
        assert_eq!(record.cli, format!("{cli}/host-wake/1/{WAKE_FILE_NAME}"));
        assert_eq!(
            record.slot,
            Some(WakeSlotId {
                base: cli.to_string(),
                number: 1,
            })
        );

        let texts = BTreeMap::from([(
            "SessionStart".to_string(),
            format!("{{\"paths\":{WAKE_PATHS_PLACEHOLDER}}}"),
        )]);
        let answers = rendered_answers(&texts, Some(&record));
        let [(file_name, text)] = answers.as_slice() else {
            panic!("one answer per text: {answers:?}");
        };
        assert_eq!(file_name, &answer_file_name("SessionStart"));
        let read_back: serde_json::Value = serde_json::from_str(text).unwrap();
        assert_eq!(read_back["paths"], serde_json::json!([record.cli]));

        let folder = Path::new(if cfg!(windows) { r"C:\w\f" } else { "/w/f" });
        let (native, native_slot) = slots.record(&WakePlace::Folder, folder).unwrap();
        assert!(native.slot.is_none() && native_slot.is_none());
        assert_eq!(
            number(&slots, &place).0,
            2,
            "a folder record takes no guest slot"
        );
    }

    fn namespace() -> yagura_os::InstanceNamespace {
        yagura_os::resolve_instance_from(None).unwrap()
    }

    #[cfg(windows)]
    #[test]
    fn guest_base_printed_takes_both_wsl_unc_spellings() {
        let namespace = namespace();
        for unc in [
            r"\\wsl.localhost\Ubuntu\home\a\.local\state\",
            r"\\wsl$\Ubuntu\home\a\.local\state",
        ] {
            let stdout = format!("/home/a/.local/state/\n{unc}\n");
            let base = guest_base_printed(stdout.as_bytes(), &namespace);
            assert_eq!(
                base,
                Some(GuestBase {
                    cli: yagura_os::guest_instance_dir("/home/a/.local/state", '/', &namespace),
                    daemon: yagura_os::guest_instance_dir(
                        unc.trim_end_matches('\\'),
                        '\\',
                        &namespace
                    ),
                }),
                "{unc}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn guest_base_printed_refuses_a_wsl_unc_on_a_unix_host() {
        let namespace = namespace();
        for unc in [
            r"\\wsl.localhost\Ubuntu\home\a\.local\state",
            r"\\wsl$\Ubuntu\home\a\.local\state",
        ] {
            let stdout = format!("/home/a/.local/state\n{unc}\n");
            assert_eq!(
                guest_base_printed(stdout.as_bytes(), &namespace),
                None,
                "a WSL UNC path is no path on a Unix host: {unc}"
            );
        }
    }
}
