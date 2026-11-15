//! The daemon boot count: the [`StatusBoot`] each daemon start of an instance
//! takes, so a reader can order a window's status generations across the
//! daemons that take the window back.
//!
//! The count is a file in the instance's state directory, beside the holders'
//! records the take-back reads, so it lasts exactly as long as the windows it
//! orders.

use std::path::Path;

use yagura_protocol::StatusBoot;

/// The boot count's file name in the instance's state directory.
pub const STATUS_BOOT_FILE: &str = "status-boot.json";

/// The largest `origin` or `seq`: 2^53 − 1, which a JavaScript number holds
/// exactly.
const MAX_BOOT_VALUE: u64 = (1 << 53) - 1;

/// Takes this daemon start's boot and records it in `instance_dir`.
///
/// The recorded boot's `seq` plus one, in the same `origin`, when the file
/// reads as a boot and that `seq` stays within 2^53 − 1. Otherwise (no file,
/// unreadable, another shape, a value out of range) a new series: a random
/// `origin` and `seq` 1. When the boot cannot be recorded, the start takes a
/// random `origin` of its own, never recorded, so no later start repeats its
/// boot.
///
/// Must be called by the instance's only writer, after the daemon holds its
/// endpoint. Blocking file I/O.
///
/// # Returns
///
/// `None` only when the OS gives no randomness for a new `origin`; a warning
/// is logged once for that, and once for a boot that could not be recorded.
pub fn advance(state_base: &Path, instance_dir: &Path) -> Option<StatusBoot> {
    let next = match read_recorded(&instance_dir.join(STATUS_BOOT_FILE)).and_then(following) {
        Some(next) => next,
        None => new_series()?,
    };
    let recorded = serde_json::to_vec(&next)
        .map_err(std::io::Error::other)
        .and_then(|content| {
            yagura_os::replace_config_file(state_base, instance_dir, STATUS_BOOT_FILE, content)
        });
    match recorded {
        Ok(()) => Some(next),
        Err(err) => {
            tracing::warn!(
                "failed to record the daemon boot count in {} ({err}); this daemon's status \
                 generations are counted in a series of their own",
                instance_dir.display()
            );
            new_series()
        }
    }
}

fn read_recorded(path: &Path) -> Option<StatusBoot> {
    let content = std::fs::read(path).ok()?;
    serde_json::from_slice(&content).ok()
}

/// The boot after `recorded` in its series, when both values stay in range.
fn following(recorded: StatusBoot) -> Option<StatusBoot> {
    (recorded.origin <= MAX_BOOT_VALUE && recorded.seq < MAX_BOOT_VALUE).then_some(StatusBoot {
        origin: recorded.origin,
        seq: recorded.seq + 1,
    })
}

/// The first boot of a new series, with a random `origin`.
fn new_series() -> Option<StatusBoot> {
    let mut bytes = [0u8; 8];
    match yagura_os::fill_random(&mut bytes) {
        Ok(()) => Some(StatusBoot {
            origin: u64::from_le_bytes(bytes) & MAX_BOOT_VALUE,
            seq: 1,
        }),
        Err(err) => {
            tracing::warn!(
                "failed to obtain randomness for the daemon boot count ({err}); this daemon \
                 names no boot, so readers do not order its status generations"
            );
            None
        }
    }
}
