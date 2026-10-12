#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: `yagura_os::seed_config_file`'s create / preserve /
//! idempotent contract: the mechanism behind the GUI's default-playbook
//! first-run seeding.
//!
//! Unix-only permission properties (owner-only mode, symlink rejection) are
//! owned by `os_unix.rs` instead.
//!
//! `seed_config_file` takes an `InstanceNamespace` and a
//! `nested` relative subdirectory. Every fixture directory below is computed
//! the same way production code composes it --
//! `instance_dir(base, namespace).join(nested)` -- so the assertions look at
//! the directory production code writes to.

use yagura_os::{
    InstanceNamespace, SeedOutcome, instance_dir, resolve_instance_from, seed_config_file,
};
use yagura_test_support::paths::TempDir;

/// The default instance, obtained the same way production code must
/// (`InstanceNamespace` has no public constructor).
fn default_namespace() -> InstanceNamespace {
    resolve_instance_from(None).expect("None must resolve to the default instance")
}

#[test]
fn seed_config_file_creates_directory_and_file_with_exact_content_when_absent() {
    let base = TempDir::new("yagura-config-seed", "create");
    let namespace = default_namespace();
    let nested = "playbook-dir";
    let file_name = "playbook.toml";
    let content = "[greeting]\nmessage = \"hello\"\n";

    let outcome = seed_config_file(base.path(), &namespace, Some(nested), file_name, content)
        .expect("seed_config_file should succeed against a writable temp base");

    assert_eq!(outcome, SeedOutcome::Created);

    let dir = instance_dir(base.path(), &namespace).join(nested);
    assert!(
        dir.is_dir(),
        "seed_config_file must create instance_dir(base, namespace)/{nested}/ under base_dir"
    );

    let written = std::fs::read(dir.join(file_name)).expect("seeded file must be readable");
    assert_eq!(
        written,
        content.as_bytes(),
        "the seeded file's bytes must match the `content` argument exactly"
    );
}

#[test]
fn seed_config_file_preserves_existing_content_and_reports_already_present() {
    let base = TempDir::new("yagura-config-seed", "preserve");
    let namespace = default_namespace();
    let nested = "playbook-dir";
    let file_name = "playbook.toml";
    let original_content = "[greeting]\nmessage = \"original\"\n";

    let dir = instance_dir(base.path(), &namespace).join(nested);
    std::fs::create_dir_all(&dir).expect("pre-create subdir for the fixture");
    std::fs::write(dir.join(file_name), original_content).expect("pre-write existing file");

    // A different `content` argument than what is already on disk: the
    // existing file must still win.
    let different_content = "[greeting]\nmessage = \"a different string entirely\"\n";
    let outcome = seed_config_file(
        base.path(),
        &namespace,
        Some(nested),
        file_name,
        different_content,
    )
    .expect("seed_config_file should succeed even when the file already exists");

    assert_eq!(outcome, SeedOutcome::AlreadyPresent);

    let on_disk = std::fs::read(dir.join(file_name)).expect("existing file must still be readable");
    assert_eq!(
        on_disk,
        original_content.as_bytes(),
        "seed_config_file must not change a single byte of an existing file"
    );
}

#[test]
fn seed_config_file_is_idempotent_when_subdir_already_exists_as_a_directory() {
    let base = TempDir::new("yagura-config-seed", "idempotent");
    let namespace = default_namespace();
    let nested = "playbook-dir";
    let file_name = "playbook.toml";

    // subdir pre-exists as a directory (created by something else), but has
    // no file in it yet.
    let dir = instance_dir(base.path(), &namespace).join(nested);
    std::fs::create_dir_all(&dir).expect("pre-create subdir as an empty directory");

    let first_content = "[greeting]\nmessage = \"first run\"\n";
    let first_outcome = seed_config_file(
        base.path(),
        &namespace,
        Some(nested),
        file_name,
        first_content,
    )
    .expect("first call against a pre-existing empty subdir should succeed");
    assert_eq!(
        first_outcome,
        SeedOutcome::Created,
        "(a) must still apply: the file itself did not exist yet"
    );
    let after_first =
        std::fs::read(dir.join(file_name)).expect("file must exist after the first call");
    assert_eq!(after_first, first_content.as_bytes());

    // Re-run with different content: the on-disk content must be unchanged.
    let second_content = "[greeting]\nmessage = \"second run should not land\"\n";
    let second_outcome = seed_config_file(
        base.path(),
        &namespace,
        Some(nested),
        file_name,
        second_content,
    )
    .expect("second call (re-run) should succeed");
    assert_eq!(
        second_outcome,
        SeedOutcome::AlreadyPresent,
        "(b) must apply on re-run: the file now exists from the first call"
    );
    let after_second =
        std::fs::read(dir.join(file_name)).expect("file must still exist after the second call");
    assert_eq!(
        after_second,
        first_content.as_bytes(),
        "re-running seed_config_file must be idempotent: the second call's content must not land"
    );
}

#[test]
fn seed_config_file_returns_a_failure_that_is_not_an_existing_file_as_an_error() {
    let base = TempDir::new("yagura-config-seed", "create-fails");
    let namespace = default_namespace();

    // The parent of the file does not exist, so the exclusive create fails for a
    // reason other than the file being there already.
    let error = seed_config_file(
        base.path(),
        &namespace,
        Some("playbook-dir"),
        "absent-subdir/playbook.toml",
        "content",
    )
    .expect_err("a create that fails for another reason must not read as AlreadyPresent");

    assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
}
