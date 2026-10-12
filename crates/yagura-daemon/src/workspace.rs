//! A window's working directory reduced to a chat room key: the `.git` walk that turns a `cwd` into a
//! [`WorkspaceKey`], the id and display name derived from that key, and the
//! collision-avoidance rule applied to a room listing.
//!
//! The walk never shells out to `git`: it reads at most one `.git` marker per
//! ancestor directory through [`FsProbe`], the same three primitives
//! [`StdFsProbe`] backs with `std::fs`. A key is always produced -- there is
//! no "could not determine a workspace" outcome -- so a window never fails to
//! open because its room could not be resolved.

use std::path::{Component, Path, PathBuf};

use sha2::{Digest, Sha256};

/// How a [`WorkspaceKey`] was determined: found by walking up to a `.git`
/// marker, or fallen back to the working directory itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceKind {
    /// A `.git` marker (directory or worktree/submodule file) was found.
    Git,
    /// No `.git` marker was found anywhere above the working directory.
    Directory,
}

/// The stable identity of a window's working directory, from which a chat
/// room's id and display name are both derived. Two windows opened against
/// the same repository (however they reached it -- a worktree checkout, a
/// subdirectory) resolve to the same key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceKey {
    /// Whether a `.git` marker was found.
    pub kind: WorkspaceKind,
    /// The resolved top-level path, canonicalized when possible.
    pub path: String,
}

/// The filesystem primitives [`resolve_key`] needs, isolated behind a trait
/// so the walk itself can be tested without touching a real filesystem.
pub trait FsProbe {
    /// Whether `path` exists and is a directory.
    fn is_dir(&self, path: &Path) -> bool;
    /// The contents of `path` as UTF-8 text, or `None` if it does not exist,
    /// is not a regular file, cannot be decoded as UTF-8, or exceeds the
    /// probe's own size bound. [`resolve_key`] treats every such case alike:
    /// the marker at this level is unreadable, so the walk continues upward.
    fn read_small(&self, path: &Path) -> Option<String>;
    /// The canonical form of `path`, or `None` if it cannot be resolved.
    fn canonicalize(&self, path: &Path) -> Option<PathBuf>;
}

/// The production [`FsProbe`]: `std::fs` plus the daemon's own
/// Windows extended-length-prefix stripping.
pub struct StdFsProbe;

/// [`StdFsProbe::read_small`]'s size bound: large enough for any real
/// `.git` file or `commondir` pointer, small enough that a malicious or
/// corrupt file cannot make one window's open read an unbounded amount of
/// data.
const READ_SMALL_MAX_BYTES: u64 = 4096;

impl FsProbe for StdFsProbe {
    fn is_dir(&self, path: &Path) -> bool {
        path.is_dir()
    }

    fn read_small(&self, path: &Path) -> Option<String> {
        let metadata = std::fs::metadata(path).ok()?;
        if !metadata.is_file() || metadata.len() > READ_SMALL_MAX_BYTES {
            return None;
        }
        std::fs::read_to_string(path).ok()
    }

    fn canonicalize(&self, path: &Path) -> Option<PathBuf> {
        let canonical = std::fs::canonicalize(path).ok()?;
        Some(crate::strip_extended_length_prefix(&canonical))
    }
}

/// The number of ancestor directories [`resolve_key`] will inspect (starting
/// at `cwd` itself) before giving up and falling back to a [`WorkspaceKind::Directory`]
/// key. Bounds the walk's own I/O rather than relying on the filesystem root
/// to stop it, since a crafted or unusual path tree might not have one within
/// a reasonable distance.
const MAX_ANCESTOR_DEPTH: usize = 64;

/// Resolves `cwd` to its [`WorkspaceKey`] by walking up to
/// [`MAX_ANCESTOR_DEPTH`] ancestors looking for a `.git` marker.
///
/// A directory marker (`<d>/.git/`) makes `d` the key directly. A file
/// marker (`<d>/.git`, worktree or submodule form) is read for a `gitdir:`
/// line; when that target has a readable `commondir`, the parent of the
/// resolved common git directory is the key (the primary worktree's top),
/// otherwise `d` itself is the key (a submodule's own worktree). A marker
/// that cannot be read, or a file marker without a `gitdir:` line, is
/// ignored and the walk continues upward. Finding nothing anywhere falls
/// back to a [`WorkspaceKind::Directory`] key at `cwd` -- a key is always
/// produced.
///
/// The result's `path` is canonicalized through `probe`; a canonicalization
/// failure leaves the unnormalized path in place rather than widening this
/// into a fallible function.
pub fn resolve_key(cwd: &Path, probe: &impl FsProbe) -> WorkspaceKey {
    for candidate in cwd.ancestors().take(MAX_ANCESTOR_DEPTH) {
        let git_marker = candidate.join(".git");
        if probe.is_dir(&git_marker) {
            return finalize(WorkspaceKind::Git, candidate, probe);
        }
        if let Some(contents) = probe.read_small(&git_marker)
            && let Some(top) = resolve_git_file(candidate, &contents, probe)
        {
            return finalize(WorkspaceKind::Git, &top, probe);
        }
    }
    finalize(WorkspaceKind::Directory, cwd, probe)
}

/// [`resolve_key`]'s blocking-task wrapper: the walk performs synchronous
/// I/O, so it always runs on a blocking thread rather than an async
/// executor's own.
///
/// If the blocking task cannot complete (the runtime is shutting down), the
/// same walk is run synchronously on the calling task instead of leaving the
/// window without a key: this is a fallback to the identical algorithm, not
/// a degraded one, so it never produces a different key than the task would
/// have.
// The one caller of the synchronous walk: it runs the walk on the blocking pool.
#[allow(clippy::disallowed_methods)]
pub async fn resolve_key_blocking(cwd: PathBuf) -> WorkspaceKey {
    let retry_cwd = cwd.clone();
    match tokio::task::spawn_blocking(move || resolve_key(&cwd, &StdFsProbe)).await {
        Ok(key) => key,
        Err(join_error) => {
            tracing::warn!(
                cancelled = join_error.is_cancelled(),
                "workspace key resolution's blocking task did not finish; \
                 resolving synchronously instead"
            );
            resolve_key(&retry_cwd, &StdFsProbe)
        }
    }
}

/// Interprets a `.git` file's `contents` found at `d`: reads its `gitdir:`
/// line and, from there, the marker's `commondir` (if any). Returns the
/// resolved top directory, or `None` if `contents` has no `gitdir:` line at
/// all (a caller should then ignore this level and keep walking upward).
fn resolve_git_file(d: &Path, contents: &str, probe: &impl FsProbe) -> Option<PathBuf> {
    let first_line = contents.lines().next()?;
    let target = first_line.strip_prefix("gitdir:")?.trim();
    if target.is_empty() {
        return None;
    }
    let gitdir_target = resolve_reference(d, target);

    let commondir_marker = gitdir_target.join("commondir");
    let Some(commondir_contents) = probe.read_small(&commondir_marker) else {
        // No readable `commondir` next to the gitdir target: a submodule,
        // whose own worktree root is `d`.
        return Some(d.to_path_buf());
    };
    let commondir_line = commondir_contents.lines().next().unwrap_or("").trim();
    if commondir_line.is_empty() {
        return Some(d.to_path_buf());
    }
    let common_git_dir = resolve_reference(&gitdir_target, commondir_line);
    Some(
        common_git_dir
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or(common_git_dir),
    )
}

/// Resolves a path reference (`gitdir:`'s target, or a `commondir` file's
/// contents) found inside a file located at `base`. A reference that opens
/// with `..` or `.` is climbed from `base` and normalized lexically (`..`
/// popped, `.` dropped) without touching the filesystem -- mirroring how git
/// itself writes these files (always relative-from-here when not absolute),
/// since the parent it climbs to need not exist as a real directory
/// `canonicalize` could reach. Anything else -- a rooted path on any
/// platform, or already the full reference git wrote -- is used as-is.
fn resolve_reference(base: &Path, reference: &str) -> PathBuf {
    let candidate = Path::new(reference);
    let starts_relative = matches!(
        candidate.components().next(),
        Some(Component::ParentDir | Component::CurDir)
    );
    if !starts_relative {
        return candidate.to_path_buf();
    }
    let mut components: Vec<Component<'_>> = base.components().collect();
    for part in candidate.components() {
        match part {
            Component::ParentDir => {
                if matches!(components.last(), Some(Component::Normal(_))) {
                    components.pop();
                }
            }
            Component::CurDir => {}
            other => components.push(other),
        }
    }
    components.iter().collect()
}

/// Builds the final [`WorkspaceKey`] once a top-level directory has been
/// chosen: canonicalizes it through `probe`, falling back to the
/// unnormalized string on failure, so a key is always produced.
fn finalize(kind: WorkspaceKind, top: &Path, probe: &impl FsProbe) -> WorkspaceKey {
    let path = match probe.canonicalize(top) {
        Some(canonical) => canonical.to_string_lossy().into_owned(),
        None => top.to_string_lossy().into_owned(),
    };
    WorkspaceKey { kind, path }
}

/// Derives a chat room id from a workspace key: `"r-"`
/// followed by the first 16 lowercase hex characters of
/// `sha256(b"git:" | b"dir:" + path)`. Stable for a given key and distinct
/// across `kind` for the same path, so a bare directory and its own `.git`
/// repository (were both ever keyed) never collide.
#[must_use]
pub fn room_id_for(key: &WorkspaceKey) -> String {
    let prefix: &[u8] = match key.kind {
        WorkspaceKind::Git => b"git:",
        WorkspaceKind::Directory => b"dir:",
    };
    let mut hasher = Sha256::new();
    hasher.update(prefix);
    hasher.update(key.path.as_bytes());
    let digest = hasher.finalize();
    let hex: String = digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("r-{hex}")
}

/// The un-disambiguated display name for a workspace key: the
/// key path's final component, ignoring a trailing separator. Falls back to
/// the whole path when it has no final component to take (the filesystem
/// root itself).
#[must_use]
pub fn display_name_for(key: &WorkspaceKey) -> String {
    let trimmed = key.path.trim_end_matches(std::path::MAIN_SEPARATOR);
    Path::new(trimmed)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| key.path.clone())
}

/// Splits a key path into its normal (named) path components, discarding any
/// root/prefix, for [`disambiguate`]'s growing-suffix comparison.
fn normal_components(path: &str) -> Vec<String> {
    Path::new(path)
        .components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect()
}

/// Joins the last `level` entries of `components` (all of them, if `level`
/// exceeds the count) with the platform's path separator.
fn name_at_level(components: &[String], level: usize) -> String {
    let level = level.clamp(1, components.len().max(1));
    let start = components.len().saturating_sub(level);
    components[start..].join(std::path::MAIN_SEPARATOR_STR)
}

/// What one [`disambiguate`] entry's name is drawn from.
enum NameSource {
    /// The built-in `main` room, which never changes.
    Main,
    /// A path with no named component (a filesystem root): the path itself
    /// is the only non-empty name it has, so it never grows.
    WholePath(String),
    /// The workspace path's named components, joined from the end.
    Components(Vec<String>),
}

/// One [`disambiguate`] entry's growable name at its current growth level.
struct Candidate {
    source: NameSource,
    level: usize,
}

impl Candidate {
    fn for_key(key: Option<&WorkspaceKey>) -> Self {
        let source = match key {
            None => NameSource::Main,
            Some(key) => {
                let components = normal_components(&key.path);
                if components.is_empty() {
                    NameSource::WholePath(key.path.clone())
                } else {
                    NameSource::Components(components)
                }
            }
        };
        Self { source, level: 1 }
    }

    fn name(&self) -> String {
        match &self.source {
            NameSource::Main => "main".to_string(),
            NameSource::WholePath(path) => path.clone(),
            NameSource::Components(components) => name_at_level(components, self.level),
        }
    }

    fn can_grow(&self) -> bool {
        match &self.source {
            NameSource::Main | NameSource::WholePath(_) => false,
            NameSource::Components(components) => self.level < components.len(),
        }
    }
}

/// Assigns a display name to every room in `rooms`: the
/// workspace key's final path component, with an ancestor segment prepended,
/// one at a time, to every member of a colliding set until the whole set is
/// unique (or each member has run out of ancestors to add). The built-in
/// `main` room (`None`) always displays as `"main"` and never grows, but
/// still participates in collision detection against a workspace whose own
/// final component happens to be `"main"`. A key with no named component (a
/// filesystem root) displays as its whole path and never grows, so no name
/// is ever empty.
///
/// Input order is preserved; this is a pure read-side transform over
/// whatever room listing the caller already has, so it never itself decides
/// which rooms exist.
#[must_use]
pub fn disambiguate(rooms: &[(String, Option<WorkspaceKey>)]) -> Vec<String> {
    let mut candidates: Vec<Candidate> = rooms
        .iter()
        .map(|(_, key)| Candidate::for_key(key.as_ref()))
        .collect();

    loop {
        let names: Vec<String> = candidates.iter().map(Candidate::name).collect();
        let mut counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
        for name in &names {
            *counts.entry(name.as_str()).or_insert(0) += 1;
        }

        let mut grew = false;
        for (candidate, name) in candidates.iter_mut().zip(names.iter()) {
            let collides = counts.get(name.as_str()).copied().unwrap_or(0) > 1;
            if collides && candidate.can_grow() {
                candidate.level += 1;
                grew = true;
            }
        }

        if !grew {
            return names;
        }
    }
}
