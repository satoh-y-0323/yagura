#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: `yagura_os::replace_existing_file`, the
//! MCP-registration write-back counterpart to `replace_config_file` --
//! narrower on purpose: it never creates the target, and it does not widen
//! or narrow permissions at all (a registration config file such as
//! `~/.claude.json` may already carry secrets in another table, so this must
//! not touch its mode).
//!
//! Signature under test: `replace_existing_file(path: &Path, content: &str)
//! -> std::io::Result<()>`.

use std::path::Path;

use yagura_os::replace_existing_file;
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
// replaces the content of an existing file.
// ---------------------------------------------------------------------

#[test]
fn replace_existing_file_replaces_the_content_of_an_existing_file() {
    let tmp = TempDir::new("yagura-existing-file-replace", "existing");
    let path = tmp.write_file("config.toml", "schema = 1\n# old\n");

    let new_content = "schema = 1\n\n[mcp_servers.yagura]\ncommand = \"yagura-mcp\"\n";
    let result = replace_existing_file(&path, new_content);
    assert!(
        result.is_ok(),
        "replacing an existing file must succeed: {result:?}"
    );

    let on_disk = std::fs::read_to_string(&path).expect("replaced file must exist");
    assert_eq!(
        on_disk, new_content,
        "the file's bytes must become exactly `content`, replacing the old bytes entirely"
    );
}

// ---------------------------------------------------------------------
// a missing file is an error, and nothing is created.
// ---------------------------------------------------------------------

#[test]
fn replace_existing_file_returns_err_and_creates_nothing_for_a_missing_file() {
    let tmp = TempDir::new("yagura-existing-file-replace", "missing");
    let path = tmp.path().join("config.toml");

    let result = replace_existing_file(&path, "schema = 1\n");
    assert!(
        result.is_err(),
        "a missing target must be an error rather than being created, got {result:?}"
    );
    assert_eq!(
        entry_names(tmp.path()),
        Vec::<String>::new(),
        "a missing target must not be created, and no temp file must be left behind"
    );
}

// ---------------------------------------------------------------------
// no leftover temp file after a successful replace.
// ---------------------------------------------------------------------

#[test]
fn replace_existing_file_leaves_no_temp_file_in_the_directory_after_success() {
    let tmp = TempDir::new("yagura-existing-file-replace", "no-leftover");
    let path = tmp.write_file("config.toml", "schema = 1\n");

    replace_existing_file(&path, "schema = 2\n")
        .expect("replace should succeed against a writable, existing file");

    assert_eq!(
        entry_names(tmp.path()),
        vec!["config.toml".to_string()],
        "the directory must contain only the target file after a successful replace, with no \
         same-directory temp file left behind"
    );
}

// ---------------------------------------------------------------------
// a failed replace leaves the original content in place, and no temp file
// behind. Windows: a read-only target makes the final rename fail, the same
// way a config file another process holds open would. Unix: rename does not
// consult the target file's own mode, so the directory itself is made
// unwritable instead, which blocks creating the same-directory temp file in
// the first place.
// ---------------------------------------------------------------------

#[cfg(windows)]
#[test]
fn replace_existing_file_leaves_the_original_content_when_the_target_cannot_be_replaced() {
    let tmp = TempDir::new("yagura-existing-file-replace", "unwritable-target");
    let path = tmp.write_file("config.toml", "schema = 1\n# original\n");

    let mut perms = std::fs::metadata(&path)
        .expect("target must exist")
        .permissions();
    perms.set_readonly(true);
    std::fs::set_permissions(&path, perms).expect("mark the target read-only");

    let result = replace_existing_file(&path, "schema = 2\n# should not land\n");
    assert!(
        result.is_err(),
        "a read-only target must not be replaceable, got {result:?}"
    );

    let on_disk = std::fs::read_to_string(&path).expect("original file must still be readable");
    assert_eq!(
        on_disk, "schema = 1\n# original\n",
        "a failed replace must leave the original content untouched"
    );
    assert_eq!(
        entry_names(tmp.path()),
        vec!["config.toml".to_string()],
        "a failed replace must not leave a same-directory temp file behind"
    );
    // `std::fs::remove_dir_all` (the TempDir Drop) clears the read-only
    // attribute itself on Windows, so no manual restore is needed here.
}

#[cfg(unix)]
#[test]
fn replace_existing_file_leaves_the_original_content_when_the_directory_is_unwritable() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = TempDir::new("yagura-existing-file-replace", "unwritable-dir");
    let path = tmp.write_file("config.toml", "schema = 1\n# original\n");

    std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o500))
        .expect("chmod the directory read-and-execute-only");

    let result = replace_existing_file(&path, "schema = 2\n# should not land\n");

    // Restored before any assertion below reads the directory or lets
    // TempDir's Drop remove it, regardless of what `result` turned out to
    // be.
    std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o700))
        .expect("restore the directory to writable for cleanup");

    assert!(
        result.is_err(),
        "a same-directory temp file must not be creatable in an unwritable directory, got \
         {result:?}"
    );
    let on_disk = std::fs::read_to_string(&path).expect("original file must still be readable");
    assert_eq!(
        on_disk, "schema = 1\n# original\n",
        "a failed replace must leave the original content untouched"
    );
    assert_eq!(
        entry_names(tmp.path()),
        vec!["config.toml".to_string()],
        "a failed replace must not leave a same-directory temp file behind"
    );
}

// ---------------------------------------------------------------------
// Unix only: a symlinked target is followed -- the real file's content
// changes, the link itself is left as a link, the real file's own mode is
// preserved, and the parent directory's mode is never touched (unlike
// replace_config_file, which hardens the directory through
// secure_owned_dir).
// ---------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn replace_existing_file_follows_a_symlink_and_preserves_its_target_mode() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = TempDir::new("yagura-existing-file-replace", "symlink");
    let real_dir = tmp.path().join("real");
    std::fs::create_dir_all(&real_dir).expect("create the real directory");
    let real_path = real_dir.join("config.toml");
    std::fs::write(&real_path, "schema = 1\n# original\n").expect("write the real file");
    std::fs::set_permissions(&real_path, std::fs::Permissions::from_mode(0o600))
        .expect("chmod the real file to owner-only");

    let link_path = tmp.path().join("config.toml");
    std::os::unix::fs::symlink(&real_path, &link_path).expect("create the symlink");

    let dir_mode_before = std::fs::metadata(tmp.path())
        .expect("directory must exist")
        .permissions()
        .mode()
        & 0o777;

    let result = replace_existing_file(&link_path, "schema = 2\n# replaced through the link\n");
    assert!(
        result.is_ok(),
        "replacing through a symlink must succeed: {result:?}"
    );

    assert!(
        std::fs::symlink_metadata(&link_path)
            .expect("link must still exist")
            .file_type()
            .is_symlink(),
        "the link itself must remain a symlink, not be replaced by a regular file"
    );
    let on_disk = std::fs::read_to_string(&real_path).expect("real file must exist");
    assert_eq!(
        on_disk, "schema = 2\n# replaced through the link\n",
        "the real file the link points at must carry the new content"
    );

    let real_mode_after = std::fs::metadata(&real_path)
        .expect("real file must exist")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(
        real_mode_after, 0o600,
        "the real file's own mode must be preserved, not widened or narrowed"
    );

    let dir_mode_after = std::fs::metadata(tmp.path())
        .expect("directory must exist")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(
        dir_mode_after, dir_mode_before,
        "unlike replace_config_file, the parent directory's mode must never change"
    );

    assert_eq!(
        entry_names(&real_dir),
        vec!["config.toml".to_string()],
        "the real file's own directory must contain only the target, with no leftover temp file"
    );
}
