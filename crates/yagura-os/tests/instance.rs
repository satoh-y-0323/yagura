#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: `InstanceNamespace` resolution.
//!
//! Covers the default-name freeze, the residual-name absence for a
//! non-default instance, fail-closed identifier validation, the
//! composite startup resolver's store-override rejection, and the
//! rejected-value-free error `Display`.
//!
//! `InstanceNamespace` has no public constructor and no `as_str()`: every
//! namespace value used here, default or not, is obtained through
//! `resolve_instance_from`, exactly as production code must.

use std::ffi::OsStr;

/// The `user_id` fed to every endpoint-composition case below: a fixed
/// literal, not a real uid/pipe-owner, so the expected strings are written
/// by hand against the current source rather than derived from the
/// implementation under test.
const FIXED_USER_ID: &str = "U";

fn fixed_base() -> std::path::PathBuf {
    std::path::PathBuf::from("/fixed-base")
}

fn default_namespace() -> yagura_os::InstanceNamespace {
    yagura_os::resolve_instance_from(None).expect("None must resolve to the default instance")
}

fn dev_namespace() -> yagura_os::InstanceNamespace {
    yagura_os::resolve_instance_from(Some(OsStr::new("dev")))
        .expect("\"dev\" is a valid identifier and must resolve")
}

// ---------------------------------------------------------------------
// Fail-closed identifier validation against the identifier allow-list.
// ---------------------------------------------------------------------

/// Every input `resolve_instance_from` must reject, paired with a short
/// label naming the rule it violates. Each candidate violates exactly one
/// rule, so a mutation that drops a single guard shows up as a distinct
/// failure rather than being masked by another guard rejecting the same
/// input for a different reason.
fn rejected_instance_ids() -> Vec<(&'static str, &'static str)> {
    vec![
        ("", "empty string"),
        (".", "dot"),
        ("..", "dot-dot"),
        ("a/b", "contains a forward slash"),
        ("a\\b", "contains a backslash"),
        ("a\0b", "contains NUL"),
        ("a b", "contains whitespace"),
        ("a:b", "contains a colon"),
        ("a*b", "contains an asterisk"),
        ("a?b", "contains a question mark"),
        ("a\"b", "contains a double quote"),
        ("a<b", "contains a less-than sign"),
        ("a>b", "contains a greater-than sign"),
        ("a|b", "contains a pipe"),
        ("Dev", "starts with an uppercase letter"),
        ("dEv", "contains an uppercase letter (mixed case)"),
        ("d\u{e9}v", "contains a non-ASCII character"),
        ("-dev", "starts with a hyphen"),
        ("dev-", "ends with a hyphen"),
        ("abcdefghijklmnopq", "17 bytes, over the 16-byte limit"),
    ]
}

#[test]
fn resolve_instance_from_rejects_every_invalid_identifier() {
    for (candidate, label) in rejected_instance_ids() {
        let result = yagura_os::resolve_instance_from(Some(OsStr::new(candidate)));
        assert!(
            result.is_err(),
            "resolve_instance_from must reject {candidate:?} ({label}); it must not fall back \
             to the default instance"
        );
    }
}

#[test]
fn resolve_instance_from_accepts_a_minimal_valid_identifier() {
    // Counter-proof against a rubber-stamp allow-list: a single lowercase
    // ASCII letter is the shortest input every identifier rule admits.
    let namespace = yagura_os::resolve_instance_from(Some(OsStr::new("a")))
        .expect("a single lowercase ASCII letter must be accepted");
    assert_eq!(namespace.id(), Some("a"));
}

#[test]
fn resolve_instance_from_accepts_the_longest_valid_identifier() {
    // 16 bytes is the documented upper bound; 17 is rejected
    // above.
    let longest = "abcdefghijklmnop";
    assert_eq!(longest.len(), 16, "fixture precondition: exactly 16 bytes");
    let namespace = yagura_os::resolve_instance_from(Some(OsStr::new(longest)))
        .expect("a 16-byte identifier must be accepted");
    assert_eq!(namespace.id(), Some(longest));
}

// ---------------------------------------------------------------------
// The default name is frozen byte-for-byte, and every one of the
// six composed names differs for a non-default instance.
//
// Each case here calls `instance_dir` / the endpoint seams with a real base
// and a real namespace obtained from `resolve_instance_from`, so a build
// that stops threading the namespace through goes red here rather than only
// failing to compile.
// ---------------------------------------------------------------------

/// The default instance's directory name. On Windows it is the bundle
/// identifier, so it never shares `%LOCALAPPDATA%\yagura` with the installed
/// program and is the directory the uninstaller's "delete app data" removes.
#[cfg(windows)]
const DEFAULT_DIR: &str = "io.github.satoh-y-0323.yagura";
#[cfg(not(windows))]
const DEFAULT_DIR: &str = "yagura";

#[test]
fn instance_dir_is_frozen_for_the_default_instance() {
    let dir = yagura_os::instance_dir(&fixed_base(), &default_namespace());
    assert_eq!(
        dir,
        fixed_base().join(DEFAULT_DIR),
        "the default instance's directory must stay <base>/{DEFAULT_DIR} verbatim"
    );
}

#[test]
fn the_app_identifier_names_the_windows_directory() {
    assert_eq!(yagura_os::APP_IDENTIFIER, "io.github.satoh-y-0323.yagura");
}

#[test]
fn instance_dir_differs_for_a_non_default_instance() {
    let default_dir = yagura_os::instance_dir(&fixed_base(), &default_namespace());
    let dev_dir = yagura_os::instance_dir(&fixed_base(), &dev_namespace());
    assert_ne!(
        default_dir, dev_dir,
        "a non-default instance's directory must differ from the default's"
    );
    assert_eq!(
        dev_dir,
        fixed_base().join(format!("{DEFAULT_DIR}-dev")),
        "a non-default instance's directory must be <base>/{DEFAULT_DIR}-<id>"
    );
}

#[cfg(windows)]
mod windows_pipe_name {
    use super::{FIXED_USER_ID, default_namespace, dev_namespace};

    #[test]
    fn default_endpoint_is_frozen_for_the_default_instance() {
        assert_eq!(
            yagura_os::default_endpoint(FIXED_USER_ID, &default_namespace()),
            format!(r"\\.\pipe\yagura-{FIXED_USER_ID}"),
            "the default instance's pipe name must stay \\\\.\\pipe\\yagura-{{user_id}} \
             verbatim"
        );
    }

    #[test]
    fn default_endpoint_differs_for_a_non_default_instance() {
        let default_name = yagura_os::default_endpoint(FIXED_USER_ID, &default_namespace());
        let dev_name = yagura_os::default_endpoint(FIXED_USER_ID, &dev_namespace());
        assert_ne!(
            default_name, dev_name,
            "a non-default instance's pipe name must differ from the default's"
        );
        assert_eq!(
            dev_name,
            format!(r"\\.\pipe\yagura-dev-{FIXED_USER_ID}"),
            "a non-default instance's pipe name must be \\\\.\\pipe\\yagura-<id>-{{user_id}}"
        );
    }
}

#[cfg(unix)]
mod unix_stage_names {
    use std::ffi::OsStr;
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

    use yagura_os::platform_unix::{EndpointCreation, resolve_endpoint_with};
    use yagura_test_support::paths::{TempDir, short_tmp_dir};

    use super::{FIXED_USER_ID, default_namespace, dev_namespace};

    /// Built on `short_tmp_dir`'s bounded `/tmp` origin so a macOS-length
    /// `$TMPDIR` cannot push a resolved socket path over
    /// `sockaddr_un.sun_path`.
    fn secure_dir() -> TempDir {
        let dir = short_tmp_dir("instance", "sec");
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))
            .expect("chmod fixture dir to 0700");
        dir
    }

    #[test]
    fn stage1_socket_name_is_frozen_for_the_default_instance() {
        let dir = secure_dir();
        let unused_tmp_root = secure_dir();
        let endpoint = resolve_endpoint_with(
            FIXED_USER_ID,
            Some(OsStr::new(dir.path().to_str().expect("utf8 path"))),
            unused_tmp_root.path(),
            EndpointCreation::ReadOnly,
            &default_namespace(),
        )
        .expect("stage 1 must succeed for a valid, owned, 0700 XDG_RUNTIME_DIR");
        assert_eq!(
            endpoint,
            format!("{}/yagura.sock", dir.path().to_str().expect("utf8 path")),
            "the default instance's stage-1 socket name must stay yagura.sock verbatim"
        );
    }

    #[test]
    fn stage1_socket_name_differs_for_a_non_default_instance() {
        let dir = secure_dir();
        let unused_tmp_root = secure_dir();
        let endpoint = resolve_endpoint_with(
            FIXED_USER_ID,
            Some(OsStr::new(dir.path().to_str().expect("utf8 path"))),
            unused_tmp_root.path(),
            EndpointCreation::ReadOnly,
            &dev_namespace(),
        )
        .expect("stage 1 must succeed for a valid, owned, 0700 XDG_RUNTIME_DIR");
        assert_eq!(
            endpoint,
            format!(
                "{}/yagura-dev.sock",
                dir.path().to_str().expect("utf8 path")
            ),
            "a non-default instance's stage-1 socket name must be \
             yagura-<id>.sock"
        );
    }

    #[test]
    fn stage3_dir_and_socket_names_are_frozen_for_the_default_instance() {
        let root = secure_dir();
        let endpoint = resolve_endpoint_with(
            FIXED_USER_ID,
            None,
            root.path(),
            EndpointCreation::CreateIfMissing,
            &default_namespace(),
        )
        .expect("stage 3 must succeed when EndpointCreation::CreateIfMissing is passed");
        let expected_dir = root.path().join(format!("yagura-{FIXED_USER_ID}"));
        assert_eq!(
            endpoint,
            format!("{}/ipc.sock", expected_dir.to_str().expect("utf8 path")),
            "the default instance's stage-3 directory and socket names must stay \
             yagura-{{user_id}}/ipc.sock verbatim"
        );
    }

    #[test]
    fn stage3_dir_and_socket_names_differ_for_a_non_default_instance() {
        let root = secure_dir();
        let endpoint = resolve_endpoint_with(
            FIXED_USER_ID,
            None,
            root.path(),
            EndpointCreation::CreateIfMissing,
            &dev_namespace(),
        )
        .expect("stage 3 must succeed when EndpointCreation::CreateIfMissing is passed");
        let expected_dir = root.path().join(format!("yagura-dev-{FIXED_USER_ID}"));
        assert_eq!(
            endpoint,
            format!("{}/ipc.sock", expected_dir.to_str().expect("utf8 path")),
            "a non-default instance's stage-3 directory must be \
             yagura-<id>-{{user_id}}, holding the same ipc.sock"
        );
    }

    /// macOS puts `TMPDIR` at `/var/folders/xx/<28 chars>/T/`: 49 bytes with
    /// its trailing slash. The longest identifier the instance rule accepts
    /// must still resolve a stage-3 endpoint under a root that long.
    #[test]
    fn stage3_fits_the_longest_identifier_under_a_macos_length_tmpdir() {
        const MACOS_TMPDIR_LEN: usize = 48;
        let base = secure_dir();
        let base_len = base.path().to_str().expect("utf8 path").len();
        assert!(
            base_len < MACOS_TMPDIR_LEN,
            "the fixture base must leave room"
        );
        let root = base
            .path()
            .join("t".repeat(MACOS_TMPDIR_LEN - base_len - 1));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .expect("create the TMPDIR-length root");
        assert_eq!(root.to_str().expect("utf8 path").len(), MACOS_TMPDIR_LEN);

        let longest = yagura_os::resolve_instance_from(Some(OsStr::new("abcdefghijklmnop")))
            .expect("a 16-byte identifier is valid");
        let endpoint = resolve_endpoint_with(
            "501",
            None,
            &root,
            EndpointCreation::CreateIfMissing,
            &longest,
        )
        .expect("the longest identifier must resolve under a macOS-length TMPDIR");
        assert!(endpoint.ends_with("/yagura-abcdefghijklmnop-501/ipc.sock"));
    }
}

// ---------------------------------------------------------------------
// `resolve_startup_env` rejects a named instance combined with a
// store-path override; the identifier-only resolver has no such parameter
// at all.
// ---------------------------------------------------------------------

#[test]
fn resolve_startup_env_rejects_a_named_instance_with_a_store_override() {
    let result = yagura_os::resolve_startup_env(
        Some(OsStr::new("dev")),
        Some(OsStr::new("/somewhere/else.db")),
    );
    assert!(
        matches!(
            result,
            Err(yagura_os::StartupEnvError::StoreOverrideWithNamedInstance)
        ),
        "a named instance combined with a store-path override must be rejected; got {result:?}"
    );
}

#[test]
fn resolve_startup_env_treats_an_empty_override_as_configured() {
    // Not "unset": store::resolve_db_path already adopts an empty
    // YAGURA_STORE_DB as configured, and the judgment must match that
    // adoption rule exactly or the two sides can disagree.
    let result = yagura_os::resolve_startup_env(Some(OsStr::new("dev")), Some(OsStr::new("")));
    assert!(
        matches!(
            result,
            Err(yagura_os::StartupEnvError::StoreOverrideWithNamedInstance)
        ),
        "an empty store-path override must count as \"configured\"; got {result:?}"
    );
}

#[cfg(unix)]
#[test]
fn resolve_startup_env_treats_a_non_utf8_override_as_configured() {
    use std::os::unix::ffi::OsStrExt;
    let non_utf8 = OsStr::from_bytes(&[0xff, 0xfe]);
    let result = yagura_os::resolve_startup_env(Some(OsStr::new("dev")), Some(non_utf8));
    assert!(
        matches!(
            result,
            Err(yagura_os::StartupEnvError::StoreOverrideWithNamedInstance)
        ),
        "a non-UTF-8 store-path override must count as \"configured\" too; got {result:?}"
    );
}

#[cfg(windows)]
#[test]
fn resolve_startup_env_treats_a_non_utf8_override_as_configured() {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;
    // An unpaired UTF-16 surrogate: not representable as UTF-8.
    let non_utf8 = OsString::from_wide(&[0xD800]);
    let result =
        yagura_os::resolve_startup_env(Some(OsStr::new("dev")), Some(non_utf8.as_os_str()));
    assert!(
        matches!(
            result,
            Err(yagura_os::StartupEnvError::StoreOverrideWithNamedInstance)
        ),
        "a non-UTF-8 store-path override must count as \"configured\" too; got {result:?}"
    );
}

#[test]
fn resolve_startup_env_lets_the_override_win_for_the_default_instance() {
    let result = yagura_os::resolve_startup_env(None, Some(OsStr::new("/somewhere/else.db")));
    let namespace = result.expect(
        "the default instance keeps today's behavior unchanged: the store-path override wins",
    );
    assert!(
        namespace.is_default(),
        "resolve_startup_env(None, Some(_)) must resolve to the default instance"
    );
}

#[test]
fn resolve_instance_from_ignores_any_store_override_entirely() {
    // The identifier-only resolver has no store-path-override parameter at
    // all, so a non-default identifier always succeeds through it -- the
    // seam `--shutdown` / `--emit-status` use so they are never blocked by
    // the named-instance store-override rejection.
    let namespace = yagura_os::resolve_instance_from(Some(OsStr::new("dev")))
        .expect("resolve_instance_from must accept a valid identifier regardless of any store-path override, since it takes none");
    assert_eq!(namespace.id(), Some("dev"));
}

// ---------------------------------------------------------------------
// `InstanceIdError` / `StartupEnvError`'s `Display` never includes the
// rejected value.
// ---------------------------------------------------------------------

#[test]
fn instance_id_error_display_never_echoes_any_rejected_candidate() {
    for (candidate, label) in rejected_instance_ids() {
        if candidate.is_empty() {
            // An empty needle would trivially "match" any message.
            continue;
        }
        let error = yagura_os::resolve_instance_from(Some(OsStr::new(candidate)))
            .unwrap_err_or_panic(candidate, label);
        let rendered = error.to_string();
        assert!(
            !rendered.contains(candidate),
            "InstanceIdError's Display must not echo the rejected candidate {candidate:?} \
             ({label}); got {rendered:?}"
        );
    }
}

/// Local extension so the loop above reads as one assertion per candidate
/// instead of a `match` that would swallow which candidate failed to be
/// rejected.
trait ExpectErrForCandidate<T, E> {
    fn unwrap_err_or_panic(self, candidate: &str, label: &str) -> E;
}

impl<T: std::fmt::Debug, E> ExpectErrForCandidate<T, E> for Result<T, E> {
    fn unwrap_err_or_panic(self, candidate: &str, label: &str) -> E {
        match self {
            Err(err) => err,
            Ok(value) => panic!(
                "{candidate:?} ({label}) must be rejected by resolve_instance_from, got Ok({value:?})"
            ),
        }
    }
}

#[test]
fn startup_env_error_display_never_contains_the_rejected_identifier() {
    let marker = "super-secret-rejected-value-marker";
    let candidate = format!("{marker}!"); // "!" makes it invalid (not in the allow-list)
    let error = yagura_os::resolve_startup_env(Some(OsStr::new(&candidate)), None)
        .expect_err("the candidate must be rejected (contains '!')");
    let rendered = error.to_string();
    assert!(
        !rendered.contains(marker),
        "StartupEnvError's Display must never include the rejected input value; got \
         {rendered:?}"
    );
}
