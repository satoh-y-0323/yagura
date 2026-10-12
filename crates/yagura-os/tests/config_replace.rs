#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: `yagura_os::replace_config_file`, the
//! atomic-replace counterpart to `seed_config_file` -- the mechanism behind
//! the daemon writing a GUI-edited `roles.toml` back to disk.
//!
//! Signature under test: `replace_config_file(base_dir: &Path, leaf_dir:
//! &Path, file_name: &str, content: &str) -> std::io::Result<()>`, mirroring
//! `secure_owned_dir(base, leaf)`'s own two-path split so it applies the
//! same hardening as `seed_config_file` literally: `secure_owned_dir(base_dir,
//! leaf_dir)` runs first, then `create_dir_all(leaf_dir)`, then the target
//! `leaf_dir.join(file_name)` is replaced through a same-directory temp file.

use std::path::Path;

use yagura_os::replace_config_file;
use yagura_test_support::paths::TempDir;

/// Lists the file names directly inside `dir`, sorted for deterministic
/// assertions.
fn entry_names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .expect("directory must be readable")
        .map(|entry| {
            entry
                .expect("directory entry must be readable")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    names
}

// ---------------------------------------------------------------------
// replaces existing content; creates a missing file (and its directory).
// ---------------------------------------------------------------------

#[test]
fn replace_config_file_replaces_the_content_of_an_existing_file() {
    let base = TempDir::new("yagura-config-replace", "existing");
    let leaf_dir = base.path().join("roles-dir");
    std::fs::create_dir_all(&leaf_dir).expect("pre-create leaf dir");
    std::fs::write(leaf_dir.join("roles.toml"), "schema = 1\n# old\n")
        .expect("pre-write original file");

    let new_content = "schema = 1\n\n[[role]]\nname = \"impl\"\nprofile = \"codex-cli\"\n";
    let result = replace_config_file(base.path(), &leaf_dir, "roles.toml", new_content);
    assert!(
        result.is_ok(),
        "replacing an existing file must succeed: {result:?}"
    );

    let on_disk =
        std::fs::read_to_string(leaf_dir.join("roles.toml")).expect("replaced file must exist");
    assert_eq!(
        on_disk, new_content,
        "the file's bytes must become exactly `content`, replacing the old bytes entirely"
    );
}

#[test]
fn replace_config_file_creates_a_missing_file_and_its_directory() {
    let base = TempDir::new("yagura-config-replace", "missing");
    // `leaf_dir` is never created ahead of time.
    let leaf_dir = base.path().join("roles-dir");
    let content = "schema = 1\n";

    let result = replace_config_file(base.path(), &leaf_dir, "roles.toml", content);
    assert!(
        result.is_ok(),
        "replacing into a directory that does not exist yet must create it, the same way \
         seed_config_file does: {result:?}"
    );

    let on_disk =
        std::fs::read_to_string(leaf_dir.join("roles.toml")).expect("created file must exist");
    assert_eq!(on_disk, content);
}

// ---------------------------------------------------------------------
// no leftover temp file after a successful replace.
// ---------------------------------------------------------------------

#[test]
fn replace_config_file_leaves_no_temp_file_in_the_directory_after_success() {
    let base = TempDir::new("yagura-config-replace", "no-leftover");
    let leaf_dir = base.path().join("roles-dir");
    std::fs::create_dir_all(&leaf_dir).expect("pre-create leaf dir");

    replace_config_file(base.path(), &leaf_dir, "roles.toml", "schema = 1\n")
        .expect("replace should succeed against a writable, empty leaf dir");

    assert_eq!(
        entry_names(&leaf_dir),
        vec!["roles.toml".to_string()],
        "the leaf directory must contain only the target file after a successful replace, \
         with no same-directory temp file left behind"
    );
}

// ---------------------------------------------------------------------
// replace fails when the target cannot be replaced (it is an existing
// directory); the original is left alone and no temp file leaks.
// ---------------------------------------------------------------------

#[test]
fn replace_config_file_fails_and_leaves_everything_untouched_when_the_target_is_a_directory() {
    let base = TempDir::new("yagura-config-replace", "target-is-dir");
    let leaf_dir = base.path().join("roles-dir");
    // The target path exists, but as a directory rather than a file, so it
    // cannot be replaced by a rename.
    let target_as_dir = leaf_dir.join("roles.toml");
    std::fs::create_dir_all(&target_as_dir).expect("pre-create target as a directory");

    let result = replace_config_file(base.path(), &leaf_dir, "roles.toml", "schema = 1\n");
    assert!(
        result.is_err(),
        "replacing a target that is itself a directory must fail, got {result:?}"
    );
    assert!(
        target_as_dir.is_dir(),
        "the pre-existing directory at the target path must be left exactly as it was"
    );
    assert_eq!(
        entry_names(&leaf_dir),
        vec!["roles.toml".to_string()],
        "a failed replace must not leave a same-directory temp file behind; only the \
         pre-existing target directory entry should remain"
    );
}

// ---------------------------------------------------------------------
// Unix only: the written file ends up owner-only, the same restriction
// secure_owned_file applies.
// ---------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn replace_config_file_narrows_the_written_file_to_owner_only() {
    use std::os::unix::fs::PermissionsExt;

    let base = TempDir::new("yagura-config-replace", "owner-only");
    let leaf_dir = base.path().join("roles-dir");

    replace_config_file(base.path(), &leaf_dir, "roles.toml", "schema = 1\n")
        .expect("replace should succeed");

    let mode = std::fs::metadata(leaf_dir.join("roles.toml"))
        .expect("written file must exist")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(
        mode, 0o600,
        "a file written by replace_config_file must end up owner-only (0600), the same mode \
         secure_owned_file applies"
    );
}
