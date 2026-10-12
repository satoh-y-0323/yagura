#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// `workspace::resolve_key` is called directly here: it is the unit under test.
#![allow(clippy::disallowed_methods)]

//! `yagura_daemon::workspace`: the pure key-resolution algorithm behind
//! chat rooms scoped to a working directory -- the `.git` walk that turns a window's `cwd` into a
//! `WorkspaceKey`, its blocking-task wrapper, the id and display-name
//! derived from a key, and the collision-avoidance rule applied to a room
//! listing.
//!
//! `resolve_key` never touches a real filesystem in this file except in
//! the dedicated integration group, which exists solely to establish that
//! a worktree checkout and its primary worktree resolve to one path
//! string. Everywhere else a memory-backed probe stands in for the three
//! filesystem primitives, so the git-walk arithmetic is checked without
//! creating a single directory.
//!
//! This file also fixes contract details the architecture leaves open.
//! Each such choice is called out where it is used; the full list travels
//! separately so the implementation can follow it rather than
//! reverse-engineer it from assertions.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use yagura_daemon::workspace::{
    FsProbe, StdFsProbe, WorkspaceKey, WorkspaceKind, disambiguate, display_name_for, resolve_key,
    resolve_key_blocking, room_id_for,
};
use yagura_protocol::is_valid_token;

// =====================================================================
// Shared fixture helpers
// =====================================================================

/// A relative synthetic path built from `segments`, joined with the host's
/// own separator. Nothing under it is ever created on disk; `resolve_key`
/// only does path arithmetic against what the probe reports.
fn synthetic_path(segments: &[&str]) -> PathBuf {
    let mut buf = PathBuf::new();
    for segment in segments {
        buf.push(segment);
    }
    buf
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn git_key(segments: &[&str]) -> WorkspaceKey {
    WorkspaceKey {
        kind: WorkspaceKind::Git,
        path: path_string(&synthetic_path(segments)),
    }
}

fn dir_key(segments: &[&str]) -> WorkspaceKey {
    WorkspaceKey {
        kind: WorkspaceKind::Directory,
        path: path_string(&synthetic_path(segments)),
    }
}

/// True filesystem root (platform-independent: the last element of
/// [`Path::ancestors`] is always the root), used only by the two boundary
/// tests that need to know how many ancestors are actually available
/// above a given directory.
fn filesystem_root() -> PathBuf {
    std::env::temp_dir()
        .ancestors()
        .last()
        .expect("a path is always its own ancestor at minimum")
        .to_path_buf()
}

/// An in-memory stand-in for the three probe primitives. Nothing in it
/// reads or writes a real path; `is_dir` and `read_small` answer from
/// fixed tables, and `canonicalize` defaults to the identity mapping
/// unless a case configures a mapping or a failure explicitly.
#[derive(Default)]
struct MemoryProbe {
    dirs: HashSet<PathBuf>,
    files: HashMap<PathBuf, String>,
    canonical: HashMap<PathBuf, PathBuf>,
    canonicalize_fails: HashSet<PathBuf>,
}

impl MemoryProbe {
    fn new() -> Self {
        Self::default()
    }

    fn mark_dir(mut self, path: PathBuf) -> Self {
        self.dirs.insert(path);
        self
    }

    fn put_file(mut self, path: PathBuf, contents: impl Into<String>) -> Self {
        self.files.insert(path, contents.into());
        self
    }

    fn map_canonical(mut self, from: PathBuf, to: PathBuf) -> Self {
        self.canonical.insert(from, to);
        self
    }

    fn deny_canonicalize(mut self, path: PathBuf) -> Self {
        self.canonicalize_fails.insert(path);
        self
    }
}

impl FsProbe for MemoryProbe {
    fn is_dir(&self, path: &Path) -> bool {
        self.dirs.contains(path)
    }

    fn read_small(&self, path: &Path) -> Option<String> {
        self.files.get(path).cloned()
    }

    fn canonicalize(&self, path: &Path) -> Option<PathBuf> {
        if self.canonicalize_fails.contains(path) {
            return None;
        }
        Some(
            self.canonical
                .get(path)
                .cloned()
                .unwrap_or_else(|| path.to_path_buf()),
        )
    }
}

// =====================================================================
// resolve_key_blocking: agreement with the synchronous result
// =====================================================================

#[tokio::test]
async fn resolve_key_blocking_agrees_with_the_synchronous_result_for_an_unmanaged_directory() {
    let cwd = synthetic_path(&["blocking-probe", "leaf"]);

    let via_blocking = resolve_key_blocking(cwd.clone()).await;
    let via_sync = resolve_key(&cwd, &StdFsProbe);

    assert_eq!(via_blocking, via_sync);
}

// =====================================================================
// resolve_key: the pure `.git` walk
// =====================================================================

#[test]
fn a_git_directory_directly_under_cwd_makes_cwd_the_key() {
    let cwd = synthetic_path(&["repos", "alpha"]);
    let probe = MemoryProbe::new().mark_dir(cwd.join(".git"));

    let key = resolve_key(&cwd, &probe);

    assert_eq!(
        key,
        WorkspaceKey {
            kind: WorkspaceKind::Git,
            path: path_string(&cwd),
        }
    );
}

#[test]
fn a_git_directory_in_an_ancestor_makes_that_ancestor_the_key() {
    let ancestor = synthetic_path(&["repos", "alpha"]);
    let cwd = ancestor.join("src").join("nested");
    let probe = MemoryProbe::new().mark_dir(ancestor.join(".git"));

    let key = resolve_key(&cwd, &probe);

    assert_eq!(
        key,
        WorkspaceKey {
            kind: WorkspaceKind::Git,
            path: path_string(&ancestor),
        }
    );
}

#[test]
fn an_absolute_gitdir_with_a_relative_commondir_resolves_to_the_primary_top() {
    let main_top = synthetic_path(&["worktree-baseline", "main"]);
    let cwd = synthetic_path(&["worktree-baseline", "checkout"]);
    // Two levels under `main_top`, so "../.." from here lands back on
    // `main_top/.git`: exactly what a real worktree's admin directory
    // looks like at `<main>/.git/worktrees/<name>`.
    let gitdir_target = main_top.join(".git").join("worktrees").join("wt1");
    let probe = MemoryProbe::new()
        .put_file(
            cwd.join(".git"),
            format!("gitdir: {}\n", gitdir_target.display()),
        )
        .put_file(gitdir_target.join("commondir"), "../..\n");

    let key = resolve_key(&cwd, &probe);

    assert_eq!(
        key,
        WorkspaceKey {
            kind: WorkspaceKind::Git,
            path: path_string(&main_top),
        }
    );
}

#[test]
fn a_relative_gitdir_line_is_resolved_against_the_git_files_own_directory() {
    let main_top = synthetic_path(&["worktree-relative", "main"]);
    // `cwd` sits two levels under `main_top`, so a `gitdir:` line spelled
    // relative to `cwd` itself (not to the process cwd) must climb back up
    // through those same two levels to land under `main_top`.
    let cwd = main_top.join("child").join("wt");
    let gitdir_target = main_top.join(".git").join("worktrees").join("wt2");
    let probe = MemoryProbe::new()
        .put_file(cwd.join(".git"), "gitdir: ../../.git/worktrees/wt2\n")
        .put_file(gitdir_target.join("commondir"), "../..\n");

    let key = resolve_key(&cwd, &probe);

    assert_eq!(
        key,
        WorkspaceKey {
            kind: WorkspaceKind::Git,
            path: path_string(&main_top),
        }
    );
}

#[test]
fn an_absolute_commondir_is_used_as_is_without_joining_it_to_the_gitdir_target() {
    let main_top = synthetic_path(&["worktree-abs-commondir", "main"]);
    let cwd = synthetic_path(&["worktree-abs-commondir", "checkout"]);
    let gitdir_target = main_top.join(".git").join("worktrees").join("wt3");
    let common_git_dir = main_top.join(".git");
    let probe = MemoryProbe::new()
        .put_file(
            cwd.join(".git"),
            format!("gitdir: {}\n", gitdir_target.display()),
        )
        .put_file(
            gitdir_target.join("commondir"),
            format!("{}\n", common_git_dir.display()),
        );

    let key = resolve_key(&cwd, &probe);

    assert_eq!(
        key,
        WorkspaceKey {
            kind: WorkspaceKind::Git,
            path: path_string(&main_top),
        }
    );
}

#[test]
fn a_git_file_whose_gitdir_has_no_readable_commondir_keys_on_the_git_files_own_directory() {
    // A submodule's `.git` file points at `<repo>/.git/modules/<name>`,
    // which has no `commondir` next to it -- unlike a worktree's admin
    // directory. The key is the submodule's own worktree root, not
    // whatever `gitdir:` happens to resolve to.
    let cwd = synthetic_path(&["submodule-case", "vendor", "lib"]);
    let probe = MemoryProbe::new().put_file(cwd.join(".git"), "gitdir: ../../.git/modules/lib\n");

    let key = resolve_key(&cwd, &probe);

    assert_eq!(
        key,
        WorkspaceKey {
            kind: WorkspaceKind::Git,
            path: path_string(&cwd),
        }
    );
}

#[test]
fn no_git_marker_anywhere_falls_back_to_a_directory_key_at_cwd() {
    let cwd = synthetic_path(&["plain-directory", "leaf"]);
    let probe = MemoryProbe::new();

    let key = resolve_key(&cwd, &probe);

    assert_eq!(
        key,
        WorkspaceKey {
            kind: WorkspaceKind::Directory,
            path: path_string(&cwd),
        }
    );
}

#[test]
fn a_git_file_without_a_gitdir_line_is_ignored_and_the_walk_continues_upward() {
    let ancestor = synthetic_path(&["malformed-content", "repo"]);
    let cwd = ancestor.join("checkout");
    let probe = MemoryProbe::new()
        .put_file(cwd.join(".git"), "this is not a gitdir pointer\n")
        .mark_dir(ancestor.join(".git"));

    let key = resolve_key(&cwd, &probe);

    assert_eq!(
        key,
        WorkspaceKey {
            kind: WorkspaceKind::Git,
            path: path_string(&ancestor),
        }
    );
}

#[test]
fn the_walk_stops_after_the_ancestor_depth_limit_without_finding_a_git_marker() {
    let mut cwd = filesystem_root();
    for level in 0..80 {
        cwd = cwd.join(format!("depth-limit-{level}"));
    }
    let probe = MemoryProbe::new();

    let key = resolve_key(&cwd, &probe);

    assert_eq!(
        key,
        WorkspaceKey {
            kind: WorkspaceKind::Directory,
            path: path_string(&cwd),
        }
    );
}

#[test]
fn the_walk_stops_at_the_filesystem_root_without_finding_a_git_marker() {
    let cwd = filesystem_root().join("root-boundary-case");
    let probe = MemoryProbe::new();

    let key = resolve_key(&cwd, &probe);

    assert_eq!(
        key,
        WorkspaceKey {
            kind: WorkspaceKind::Directory,
            path: path_string(&cwd),
        }
    );
}

#[test]
fn a_canonicalize_failure_leaves_the_unnormalized_path_as_the_key() {
    let cwd = synthetic_path(&["canonicalize-failure", "leaf"]);
    let probe = MemoryProbe::new().deny_canonicalize(cwd.clone());

    let key = resolve_key(&cwd, &probe);

    assert_eq!(
        key,
        WorkspaceKey {
            kind: WorkspaceKind::Directory,
            path: path_string(&cwd),
        }
    );
}

#[test]
fn a_canonicalize_mapping_is_reflected_in_the_resulting_key() {
    let cwd = synthetic_path(&["canonicalize-mapping", "leaf"]);
    let mapped = synthetic_path(&["canonicalize-mapping", "normalized-leaf"]);
    let probe = MemoryProbe::new().map_canonical(cwd.clone(), mapped.clone());

    let key = resolve_key(&cwd, &probe);

    assert_eq!(
        key,
        WorkspaceKey {
            kind: WorkspaceKind::Directory,
            path: path_string(&mapped),
        }
    );
}

#[test]
fn resolve_key_is_deterministic_for_the_same_cwd_and_probe() {
    let cwd = synthetic_path(&["determinism-case", "leaf"]);
    let probe = MemoryProbe::new().mark_dir(cwd.join(".git"));

    let first = resolve_key(&cwd, &probe);
    let second = resolve_key(&cwd, &probe);

    assert_eq!(first, second);
}

// =====================================================================
// resolve_key: real filesystem integration (same-path hypothesis)
// =====================================================================

#[test]
fn a_subdirectory_and_a_worktree_checkout_resolve_to_the_same_path_as_the_primary_top() {
    let primary = yagura_test_support::paths::TempDir::new("worktree-workspace", "primary");
    let checkout = yagura_test_support::paths::TempDir::new("worktree-workspace", "checkout");

    std::fs::create_dir(primary.path().join(".git")).expect("create primary .git directory");
    let worktree_admin_dir = primary
        .path()
        .join(".git")
        .join("worktrees")
        .join("checkout");
    std::fs::create_dir_all(&worktree_admin_dir).expect("create worktree admin directory");
    std::fs::write(worktree_admin_dir.join("commondir"), "../..\n").expect("write commondir file");
    std::fs::write(
        checkout.path().join(".git"),
        format!("gitdir: {}\n", worktree_admin_dir.display()),
    )
    .expect("write worktree .git file");
    let nested = primary.path().join("src").join("inner");
    std::fs::create_dir_all(&nested).expect("create nested subdirectory");

    let from_primary = resolve_key(primary.path(), &StdFsProbe);
    let from_checkout = resolve_key(checkout.path(), &StdFsProbe);
    let from_subdirectory = resolve_key(&nested, &StdFsProbe);

    assert_eq!(from_primary.kind, WorkspaceKind::Git);
    assert_eq!(from_primary.path, from_checkout.path);
    assert_eq!(from_primary.path, from_subdirectory.path);
}

// =====================================================================
// room_id_for
// =====================================================================

#[test]
fn room_id_for_has_the_r_dash_sixteen_hex_form() {
    let id = room_id_for(&git_key(&["repos", "alpha"]));

    assert!(id.starts_with("r-"), "expected an `r-` prefix, got {id}");
    let hex = &id[2..];
    assert_eq!(hex.len(), 16, "expected 16 hex characters, got {hex}");
    assert!(
        hex.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "expected lowercase hex only, got {hex}"
    );
}

#[test]
fn room_id_for_result_is_a_valid_protocol_token() {
    let id = room_id_for(&git_key(&["repos", "alpha"]));

    assert!(is_valid_token(&id));
}

#[test]
fn room_id_for_is_deterministic_for_the_same_key() {
    let key = git_key(&["repos", "alpha"]);

    assert_eq!(room_id_for(&key), room_id_for(&key));
}

#[test]
fn room_id_for_differs_when_kind_differs_for_the_same_path() {
    let git = git_key(&["repos", "alpha"]);
    let dir = dir_key(&["repos", "alpha"]);

    assert_ne!(room_id_for(&git), room_id_for(&dir));
}

#[test]
fn room_id_for_differs_when_the_path_differs_by_one_character() {
    let a = git_key(&["repos", "alpha"]);
    let b = git_key(&["repos", "alphb"]);

    assert_ne!(room_id_for(&a), room_id_for(&b));
}

// =====================================================================
// display_name_for
// =====================================================================

#[test]
fn display_name_for_returns_the_final_path_component() {
    let key = git_key(&["repos", "org", "alpha"]);

    assert_eq!(display_name_for(&key), "alpha");
}

#[test]
fn display_name_for_strips_a_trailing_separator_before_taking_the_final_component() {
    let mut path = path_string(&synthetic_path(&["repos", "org", "alpha"]));
    path.push(std::path::MAIN_SEPARATOR);
    let key = WorkspaceKey {
        kind: WorkspaceKind::Git,
        path,
    };

    assert_eq!(display_name_for(&key), "alpha");
}

#[test]
fn display_name_for_falls_back_to_the_whole_key_when_the_final_component_is_empty() {
    let path = std::path::MAIN_SEPARATOR.to_string();
    let key = WorkspaceKey {
        kind: WorkspaceKind::Directory,
        path: path.clone(),
    };

    assert_eq!(display_name_for(&key), path);
}

// =====================================================================
// disambiguate
// =====================================================================

#[test]
fn disambiguate_leaves_names_unchanged_when_none_of_them_collide() {
    let rooms = vec![
        ("r-a".to_string(), Some(git_key(&["repos", "alpha"]))),
        ("r-b".to_string(), Some(git_key(&["repos", "beta"]))),
    ];

    assert_eq!(
        disambiguate(&rooms),
        vec!["alpha".to_string(), "beta".to_string()]
    );
}

#[test]
fn disambiguate_preserves_input_order_rather_than_sorting_by_name() {
    let rooms = vec![
        ("r-z".to_string(), Some(git_key(&["repos", "zeta"]))),
        ("r-a".to_string(), Some(git_key(&["repos", "alpha"]))),
    ];

    assert_eq!(
        disambiguate(&rooms),
        vec!["zeta".to_string(), "alpha".to_string()]
    );
}

#[test]
fn disambiguate_adds_one_parent_segment_when_two_names_collide() {
    let key_a = git_key(&["org-a", "repo"]);
    let key_b = git_key(&["org-b", "repo"]);
    let rooms = vec![
        ("r-a".to_string(), Some(key_a.clone())),
        ("r-b".to_string(), Some(key_b.clone())),
    ];

    assert_eq!(disambiguate(&rooms), vec![key_a.path, key_b.path]);
}

#[test]
fn disambiguate_adds_a_second_parent_segment_when_one_is_still_not_enough() {
    // Both keys share the same immediate parent name (`proj`), so the
    // first extension attempt (`proj/repo`) still collides and a second
    // level is required.
    let key_a = git_key(&["group-x", "proj", "repo"]);
    let key_b = git_key(&["group-y", "proj", "repo"]);
    let rooms = vec![
        ("r-a".to_string(), Some(key_a.clone())),
        ("r-b".to_string(), Some(key_b.clone())),
    ];

    assert_eq!(disambiguate(&rooms), vec![key_a.path, key_b.path]);
}

#[test]
fn disambiguate_extends_every_member_of_a_three_way_collision() {
    let key_a = git_key(&["org-a", "repo"]);
    let key_b = git_key(&["org-b", "repo"]);
    let key_c = git_key(&["org-c", "repo"]);
    let rooms = vec![
        ("r-a".to_string(), Some(key_a.clone())),
        ("r-b".to_string(), Some(key_b.clone())),
        ("r-c".to_string(), Some(key_c.clone())),
    ];

    assert_eq!(
        disambiguate(&rooms),
        vec![key_a.path, key_b.path, key_c.path]
    );
}

#[test]
fn disambiguate_leaves_main_untouched_but_still_extends_a_colliding_workspace_name() {
    // A real workspace whose final path element happens to be literally
    // "main" collides, in text, with the built-in room's fixed name. The
    // built-in room (`None`) never changes; the workspace room is the one
    // that gains a parent segment.
    let key_real = git_key(&["workspace", "main"]);
    let rooms = vec![
        ("r-real".to_string(), Some(key_real.clone())),
        ("r-builtin".to_string(), None),
    ];

    assert_eq!(
        disambiguate(&rooms),
        vec![key_real.path, "main".to_string()]
    );
}

// =====================================================================
// disambiguate: a key with no named path component (a filesystem root)
// never yields an empty display name
// =====================================================================

#[test]
fn disambiguate_of_a_lone_root_key_returns_the_path_itself_rather_than_an_empty_name() {
    let path = std::path::MAIN_SEPARATOR.to_string();
    let key = WorkspaceKey {
        kind: WorkspaceKind::Directory,
        path: path.clone(),
    };
    let rooms = vec![("r-root".to_string(), Some(key))];

    assert_eq!(disambiguate(&rooms), vec![path]);
}

#[test]
fn disambiguate_does_not_return_an_empty_name_when_two_root_keys_collide() {
    // Two entries that key on the same rootless path (e.g. a bug upstream
    // handed the same window twice): the pre-fix rule would return `""` for
    // both, since an empty component list can never grow to break the tie.
    // The fixed rule must still not surface an empty name for either.
    let path = std::path::MAIN_SEPARATOR.to_string();
    let key_a = WorkspaceKey {
        kind: WorkspaceKind::Directory,
        path: path.clone(),
    };
    let key_b = key_a.clone();
    let rooms = vec![
        ("r-a".to_string(), Some(key_a)),
        ("r-b".to_string(), Some(key_b)),
    ];

    let names = disambiguate(&rooms);

    assert_eq!(names, vec![path.clone(), path]);
    assert!(
        names.iter().all(|name| !name.is_empty()),
        "no colliding root name may be empty, got {names:?}"
    );
}

#[cfg(windows)]
#[test]
fn disambiguate_of_a_windows_drive_root_key_returns_the_drive_itself() {
    let path = r"C:\".to_string();
    let key = WorkspaceKey {
        kind: WorkspaceKind::Directory,
        path: path.clone(),
    };
    let rooms = vec![("r-drive".to_string(), Some(key))];

    assert_eq!(disambiguate(&rooms), vec![path]);
}

// =====================================================================
// StdFsProbe against a real filesystem
// =====================================================================

#[test]
fn a_cwd_spelled_through_a_parent_segment_resolves_to_the_same_key_as_the_directory_itself() {
    let root = yagura_test_support::paths::TempDir::new("workspace-canonical", "root");
    std::fs::create_dir(root.path().join(".git")).expect("create .git directory");
    root.make_dir("sub");
    let roundabout = root.path().join("sub").join("..");

    let direct = resolve_key(root.path(), &StdFsProbe);
    let via_parent_segment = resolve_key(&roundabout, &StdFsProbe);

    assert_eq!(direct.kind, WorkspaceKind::Git);
    assert_eq!(
        via_parent_segment, direct,
        "two spellings of one directory must land in one room"
    );
}

#[test]
fn std_fs_probe_reads_a_marker_up_to_its_size_bound_and_no_further() {
    let dir = yagura_test_support::paths::TempDir::new("workspace-read-small", "bound");
    let at_bound = dir.path().join("at-bound");
    let past_bound = dir.path().join("past-bound");
    std::fs::write(&at_bound, vec![b'x'; 4096]).expect("write a file at the bound");
    std::fs::write(&past_bound, vec![b'x'; 4097]).expect("write a file past the bound");

    assert_eq!(
        StdFsProbe.read_small(&at_bound).map(|text| text.len()),
        Some(4096)
    );
    assert_eq!(StdFsProbe.read_small(&past_bound), None);
}
