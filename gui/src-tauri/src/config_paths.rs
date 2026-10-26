//! Resolves one instance's yagura configuration root and seeds it with the
//! application's default playbook and reference ticket settings on first
//! launch. Launch profiles are not seeded: the daemon lays the user's changes
//! over the profiles it ships.
//!
//! Every seam here takes the configuration base directory and the resolved
//! [`InstanceNamespace`] as arguments rather than reading the environment or
//! [`crate::instance::StartupContext`] itself: the namespace being a type
//! obtainable only through a passed instance resolution is what keeps a
//! caller from seeding the wrong instance's directory, and [`StartupContext`]
//! is what the `pub(crate)` wrapper below takes instead, since it is reached
//! only from places that already hold one.

use std::path::{Path, PathBuf};

use yagura_os::{InstanceNamespace, instance_dir, seed_config_file};

use crate::instance::StartupContext;

/// The default playbook's file name: the single definition point shared by
/// [`resolve_playbook_path`] and [`ensure_default_playbook_in`].
const PLAYBOOK_FILE_NAME: &str = "playbook.toml";

/// The ticket settings' file name, where the daemon looks for them by default.
const TICKETS_FILE_NAME: &str = "tickets.toml";

/// The root of `namespace`'s user-owned yagura configuration under `base`:
/// `<base>/<namespace>`. `base` is `%LOCALAPPDATA%\` on Windows,
/// `~/Library/Application Support/` on macOS, `$XDG_CONFIG_HOME/` (falling
/// back to `~/.config/`) on other Unix, resolved once by the caller through
/// [`yagura_os::default_config_base_dir`] -- this module never reads a
/// platform environment variable itself.
///
/// Distinct from the daemon's state root (`default_state_base_dir`), which
/// happens to share `base` on Windows and macOS but not on other Unix.
#[must_use]
pub fn config_root(base: &Path, namespace: &InstanceNamespace) -> PathBuf {
    instance_dir(base, namespace)
}

/// Resolves the fixed playbook path the daemon is launched with, for the
/// instance `startup` resolved.
///
/// Returns `None` when [`StartupContext::config_root`] cannot be resolved or
/// the file does not exist there, in which case `--playbook` is simply not
/// passed and the daemon falls back to `YAGURA_PLAYBOOK` / no playbook
/// (fail-closed: every destructive operation stays denied).
pub(crate) fn resolve_playbook_path(startup: &StartupContext) -> Option<PathBuf> {
    let path = startup.config_root()?.join(PLAYBOOK_FILE_NAME);
    path.is_file().then_some(path)
}

/// Seeds `namespace`'s config root under `base` with the repository's
/// default playbook on first launch, embedded with `include_str!` since this
/// is an application crate rather than one published to crates.io.
///
/// Does nothing that fails GUI startup: a write failure goes to stderr and is
/// otherwise left to the daemon's fail-closed behavior.
pub fn ensure_default_playbook_in(base: &Path, namespace: &InstanceNamespace) {
    if let Err(error) = seed_config_file(
        base,
        namespace,
        None,
        PLAYBOOK_FILE_NAME,
        include_str!("../../../playbook.toml"),
    ) {
        eprintln!("failed to seed the default playbook: {error}");
    }
}

/// Seeds `namespace`'s config root under `base` with the repository's
/// reference ticket settings on first launch, leaving an existing file as the
/// user wrote it.
///
/// Does nothing that fails GUI startup: a write failure goes to stderr, and
/// the daemon then reports the board as not configured.
pub fn ensure_default_tickets_in(base: &Path, namespace: &InstanceNamespace) {
    if let Err(error) = seed_config_file(
        base,
        namespace,
        None,
        TICKETS_FILE_NAME,
        include_str!("../../../tickets.toml"),
    ) {
        eprintln!("failed to seed the reference ticket settings: {error}");
    }
}

#[cfg(test)]
mod tests {
    use yagura_test_support::paths::TempDir;

    use super::*;

    fn startup_under(base: &Path) -> StartupContext {
        StartupContext::resolve_from(None, None, Some(base.to_path_buf()))
    }

    #[test]
    fn the_daemon_is_handed_the_playbook_the_config_root_holds() {
        let base = TempDir::new("yagura-test", "playbook-present");
        let startup = startup_under(base.path());
        let root = startup
            .config_root()
            .expect("the default instance has a root");
        std::fs::create_dir_all(&root).expect("create the config root");
        std::fs::write(root.join(PLAYBOOK_FILE_NAME), "").expect("write the playbook");

        assert_eq!(
            resolve_playbook_path(&startup),
            Some(root.join(PLAYBOOK_FILE_NAME))
        );
    }

    #[test]
    fn no_playbook_is_handed_over_when_the_config_root_has_none() {
        let base = TempDir::new("yagura-test", "playbook-absent");

        assert_eq!(resolve_playbook_path(&startup_under(base.path())), None);
    }

    #[test]
    fn a_first_launch_places_the_reference_tickets_settings_in_the_config_root() {
        let base = TempDir::new("yagura-test", "tickets-seeded");
        let startup = startup_under(base.path());
        let namespace = startup.resolution().as_ref().expect("the default instance");

        ensure_default_tickets_in(base.path(), namespace);

        let root = startup
            .config_root()
            .expect("the default instance has a root");
        let placed = std::fs::read_to_string(root.join("tickets.toml")).expect("placed file");
        assert_eq!(placed, include_str!("../../../tickets.toml"));
    }

    #[test]
    fn an_existing_tickets_file_is_not_overwritten() {
        let base = TempDir::new("yagura-test", "tickets-kept");
        let startup = startup_under(base.path());
        let namespace = startup.resolution().as_ref().expect("the default instance");
        let root = startup
            .config_root()
            .expect("the default instance has a root");
        std::fs::create_dir_all(&root).expect("create the config root");
        std::fs::write(
            root.join("tickets.toml"),
            "schema = 1
# mine
",
        )
        .expect("write");

        ensure_default_tickets_in(base.path(), namespace);

        let kept = std::fs::read_to_string(root.join("tickets.toml")).expect("kept file");
        assert_eq!(
            kept,
            "schema = 1
# mine
"
        );
    }
}
