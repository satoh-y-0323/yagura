//! The environment a window's child is started with, as changes its holder
//! applies.

use std::ffi::OsString;

use yagura_protocol::holder::HolderEnvOp;

use crate::child_env::ChildBaseEnv;
use crate::terminal_identity;

/// The window's environment changes, applied by its holder on top of its
/// own environment (the daemon's, plus the registry's variables on Windows):
/// [`DaemonConfig::child_base_env`], then the launching terminal's own
/// variables taken away and [`terminal_identity::DECLARED_TERMINAL`] set,
/// then `extra_env` (a name given more than once keeps its last value), then
/// each of `removed_env` (compared without regard to case) taken out last.
pub(super) fn window_env(
    child_base_env: &ChildBaseEnv,
    extra_env: &[(&str, &str)],
    removed_env: &[&str],
) -> Vec<HolderEnvOp> {
    let set = |name: OsString, value: OsString| HolderEnvOp::Set { name, value };
    let mut env: Vec<HolderEnvOp> = child_base_env
        .iter()
        .map(|(name, value)| set(name.to_os_string(), value.to_os_string()))
        .collect();
    let inherited: Vec<OsString> = std::env::vars_os().map(|(name, _)| name).collect();
    let launcher_names = terminal_identity::launcher_terminal_names(
        inherited
            .iter()
            .map(OsString::as_os_str)
            .chain(child_base_env.iter().map(|(name, _)| name)),
    );
    env.extend(launcher_names.iter().map(|name| HolderEnvOp::Remove {
        name: name.to_os_string(),
    }));
    env.extend(
        terminal_identity::DECLARED_TERMINAL
            .iter()
            .map(|(name, value)| set(OsString::from(name), OsString::from(value))),
    );
    env.extend(
        extra_env
            .iter()
            .map(|(key, value)| set(OsString::from(key), OsString::from(value))),
    );
    let is_removed = |name: &std::ffi::OsStr| {
        removed_env
            .iter()
            .any(|removed| name.eq_ignore_ascii_case(removed))
    };
    let mut taken_out: Vec<OsString> = removed_env.iter().map(OsString::from).collect();
    for name in inherited
        .iter()
        .map(OsString::as_os_str)
        .chain(child_base_env.iter().map(|(name, _)| name))
    {
        if is_removed(name) && !taken_out.iter().any(|seen| seen == name) {
            taken_out.push(name.to_os_string());
        }
    }
    env.extend(
        taken_out
            .into_iter()
            .map(|name| HolderEnvOp::Remove { name }),
    );
    env
}
