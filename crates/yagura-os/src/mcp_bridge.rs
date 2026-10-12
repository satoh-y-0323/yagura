//! Building blocks the MCP bridge channel needs from the operating
//! system: resolving the bridge executable next to another one, turning a
//! canonicalized path back into a form safe to hand to a child process, and
//! extending the `WSLENV` value a window's environment carries.
//!
//! WSL translation of the path itself is not one of these functions: WSL
//! does that at the boundary through the `/p` flag on a forwarded `WSLENV`
//! name, so nothing here spawns a process or rewrites a path for it.

use std::path::{Path, PathBuf};

use crate::program::Platform;

/// Resolves the executable named `name` sitting next to `dir`, applying the
/// platform's own naming convention: `{name}.exe` on [`Platform::Windows`],
/// the exact name unchanged on [`Platform::Unix`].
///
/// Both `dir` and the candidate are canonicalized, and the candidate is
/// required to still be a direct child of the canonicalized `dir` and a
/// regular file -- the containment check the GUI's own daemon resolution
/// already applied, now shared with the daemon's bridge resolution --
/// so neither a `..` component nor a symlink whose target
/// lives elsewhere can redirect a caller to an executable outside `dir`.
///
/// Returns `None` when `dir` cannot be canonicalized, the candidate does not
/// exist, is not a regular file, or resolves outside `dir`.
#[must_use]
pub fn resolve_sibling_executable(dir: &Path, name: &str, platform: Platform) -> Option<PathBuf> {
    let directory = dir.canonicalize().ok()?;
    let candidate_name = match platform {
        Platform::Windows => format!("{name}.exe"),
        Platform::Unix => name.to_string(),
    };
    let candidate = directory.join(candidate_name).canonicalize().ok()?;
    if candidate.parent() != Some(directory.as_path()) || !candidate.is_file() {
        return None;
    }
    Some(candidate)
}

/// Turns a canonicalized path back into a form safe to hand to a child
/// process: the Windows extended-length prefix (`\\?\`) that
/// [`Path::canonicalize`] adds ahead of a drive letter is stripped.
///
/// An extended-length UNC path (`\\?\UNC\server\share\...`) has no
/// un-prefixed equivalent a child can be handed unchanged, so this returns
/// `None` rather than guess a translation -- the caller is expected to stop
/// injecting the value instead of passing on something a child cannot use.
///
/// A path carrying no extended-length prefix is returned unchanged.
#[must_use]
pub fn to_plain_path(path: &Path) -> Option<PathBuf> {
    let raw = path.to_string_lossy();
    if raw.starts_with(r"\\?\UNC\") {
        return None;
    }
    match raw.strip_prefix(r"\\?\") {
        Some(rest) => Some(PathBuf::from(rest)),
        None => Some(path.to_path_buf()),
    }
}

/// Builds the value a child's `WSLENV` should carry: `base`'s own items
/// (colon-separated, in order, empty items dropped), followed by every name
/// in `names` that is not already present.
///
/// Presence is judged ignoring ASCII case and any trailing `/flags` suffix
/// (`WSLENV`'s own syntax), so `A/p` and `a` count as the same name; whichever
/// spelling appeared first is the one kept, never rewritten.
#[must_use]
pub fn merge_wslenv(base: Option<&str>, names: &[&str]) -> String {
    let mut items: Vec<String> = Vec::new();
    let mut seen: Vec<String> = Vec::new();

    if let Some(base) = base {
        for item in base.split(':').filter(|item| !item.is_empty()) {
            seen.push(canonical_wslenv_name(item));
            items.push(item.to_string());
        }
    }

    for name in names {
        let canonical = canonical_wslenv_name(name);
        if seen.contains(&canonical) {
            continue;
        }
        seen.push(canonical);
        items.push((*name).to_string());
    }

    items.join(":")
}

/// Name a `WSLENV` item is compared under: its own trailing `/flags` dropped,
/// folded to ASCII uppercase.
fn canonical_wslenv_name(item: &str) -> String {
    item.split('/').next().unwrap_or(item).to_ascii_uppercase()
}
