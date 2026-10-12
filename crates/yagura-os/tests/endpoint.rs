#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: the pure string-representation contract of the
//! IPC endpoint / runtime-directory seams (`default_endpoint`,
//! `default_endpoint_owning`, `data_base_dir_from`, `state_base_dir_from`,
//! `config_base_dir_from`). The
//! environment-reading `default_*` wrappers are not covered here.
//!
//! Split from `tests/os_unix.rs` by what each file needs to compile on: the
//! top level here is libc-free and OS-agnostic, while the `#[cfg(windows)]` /
//! `#[cfg(unix)]` modules below hold each platform's exact expected shape and
//! identifiers. `use` stays inside each `mod` so a Windows-only or Unix-only
//! import never has to compile on the other target.

// ---------------------------------------------------------------------
// Determinism: platform-agnostic, no cfg needed.
//
// Varying with `user_id` is not: on Unix, stage 1's identifier never embeds
// it, and `default_endpoint` picks stage 1 whenever the session has a valid
// `XDG_RUNTIME_DIR`. That property is therefore pinned on Windows through
// `default_endpoint`, and on Unix directly against the seam's stage 3.
// ---------------------------------------------------------------------

/// The default instance, obtained the same way production code must
/// (`InstanceNamespace` has no public constructor).
fn default_namespace() -> yagura_os::InstanceNamespace {
    yagura_os::resolve_instance_from(None).expect("None must resolve to the default instance")
}

#[test]
fn default_endpoint_is_pure_and_deterministic() {
    let user_id = "yagura-endpoint-repr-test-uid";
    let namespace = default_namespace();
    assert_eq!(
        yagura_os::default_endpoint(user_id, &namespace),
        yagura_os::default_endpoint(user_id, &namespace),
        "default_endpoint must be referentially transparent"
    );
}

#[test]
fn data_base_dir_from_is_pure_and_deterministic() {
    let primary = std::ffi::OsStr::new("primary-value");
    let home = std::ffi::OsStr::new("home-value");
    assert_eq!(
        yagura_os::data_base_dir_from(Some(primary), Some(home)),
        yagura_os::data_base_dir_from(Some(primary), Some(home)),
        "data_base_dir_from must be referentially transparent"
    );
}

#[test]
fn state_base_dir_from_is_pure_and_deterministic() {
    let primary = std::ffi::OsStr::new("primary-value");
    let home = std::ffi::OsStr::new("home-value");
    assert_eq!(
        yagura_os::state_base_dir_from(Some(primary), Some(home)),
        yagura_os::state_base_dir_from(Some(primary), Some(home)),
        "state_base_dir_from must be referentially transparent"
    );
}

// ---------------------------------------------------------------------
// macOS: `macos_base_dir_from`'s pure `home ->
// {home}/Library/Application Support` mapping. Compiled on every OS (no cfg,
// unlike every other test in this file) so the mapping stays unit-testable
// off-macOS; the per-seam delegation is exercised in `unix_base_dir`.
// ---------------------------------------------------------------------

#[test]
fn macos_base_dir_from_appends_library_application_support_when_home_is_present() {
    let home = std::ffi::OsStr::new("/Users/example");
    assert_eq!(
        yagura_os::macos_base_dir_from(Some(home)),
        Some(std::path::PathBuf::from(
            "/Users/example/Library/Application Support"
        )),
        "macos_base_dir_from must map home to {{home}}/Library/Application Support"
    );
}

#[test]
fn macos_base_dir_from_returns_none_when_home_is_absent() {
    assert_eq!(
        yagura_os::macos_base_dir_from(None),
        None,
        "macos_base_dir_from must return None when home is absent"
    );
}

// ---------------------------------------------------------------------
// Windows: verbatim representation + identifiers.
// ---------------------------------------------------------------------

#[cfg(windows)]
mod windows_repr {
    use super::default_namespace;

    #[test]
    fn default_endpoint_embeds_the_given_sid_in_the_current_pipe_name() {
        let sid = "S-1-5-21-1111111111-2222222222-3333333333-1001";
        let name = yagura_os::default_endpoint(sid, &default_namespace());
        assert_eq!(name, format!(r"\\.\pipe\yagura-{sid}"));
    }

    /// The pipe name always embeds `user_id` (the SID) verbatim, so this holds
    /// regardless of the calling session's environment -- unlike the Unix side
    /// (`unix_repr::stage3_identifier_differs_for_different_user_ids`).
    #[test]
    fn default_endpoint_differs_for_different_user_ids() {
        let namespace = default_namespace();
        assert_ne!(
            yagura_os::default_endpoint("yagura-endpoint-repr-test-uid-1", &namespace),
            yagura_os::default_endpoint("yagura-endpoint-repr-test-uid-2", &namespace),
            "default_endpoint must vary with user_id"
        );
    }
}

#[cfg(windows)]
mod windows_base_dir {
    use std::ffi::OsStr;
    use std::path::PathBuf;

    #[test]
    fn primary_is_returned_unchanged_when_present() {
        let primary = OsStr::new(r"C:\Users\example\AppData\Local");
        assert_eq!(
            yagura_os::data_base_dir_from(Some(primary), None),
            Some(PathBuf::from(primary)),
            "Windows: primary (LOCALAPPDATA) is returned as-is, home is unused"
        );
        assert_eq!(
            yagura_os::state_base_dir_from(Some(primary), Some(OsStr::new("ignored-home"))),
            Some(PathBuf::from(primary)),
            "Windows: the state seam ignores home just like the data seam does"
        );
    }

    #[test]
    fn none_is_returned_when_primary_is_absent() {
        assert_eq!(
            yagura_os::data_base_dir_from(None, Some(OsStr::new("ignored-home"))),
            None,
            "Windows: an absent primary (LOCALAPPDATA) yields None even when home is given"
        );
    }

    /// The config seam's Windows branch has the same shape as the data and
    /// state seams'. Driven through the seam with synthesized
    /// arguments, so it never depends on the test process's environment.
    #[test]
    fn config_primary_is_returned_unchanged_when_present() {
        let primary = OsStr::new(r"C:\Users\example\AppData\Local");
        assert_eq!(
            yagura_os::config_base_dir_from(Some(primary), Some(OsStr::new("ignored-home"))),
            Some(PathBuf::from(primary)),
            "Windows: the config seam returns primary (LOCALAPPDATA) as-is and ignores home, \
             exactly as the data and state seams do"
        );
    }

    #[test]
    fn config_none_is_returned_when_primary_is_absent() {
        assert_eq!(
            yagura_os::config_base_dir_from(None, Some(OsStr::new("ignored-home"))),
            None,
            "Windows: an absent primary (LOCALAPPDATA) yields None for the config seam too, \
             even when home is given"
        );
    }
}

// ---------------------------------------------------------------------
// Unix: identifier shape (stage 1 / stage 3) + base-dir XDG -> home fallback.
// Uses `platform_unix::resolve_endpoint_with` directly rather than the env
// wrapper, since only the seam's arguments are fully controllable (stage 2's
// fixed `/run/user/{user_id}` path is not).
// ---------------------------------------------------------------------

#[cfg(unix)]
mod unix_repr {
    use std::ffi::OsStr;
    use std::os::unix::fs::PermissionsExt;

    use yagura_os::platform_unix::{EndpointCreation, resolve_endpoint_with};
    use yagura_test_support::paths::{TempDir, short_tmp_dir};

    use super::default_namespace;

    /// Built on `short_tmp_dir`'s bounded `/tmp` origin so a macOS-length
    /// `$TMPDIR` cannot push a resolved socket path over `sockaddr_un.sun_path`.
    /// Its budget check covers only the directory it returns, so
    /// `prefix`/`label` stay short to leave room for the
    /// `yagura-{user_id}/ipc.sock` suffix the stage-3 cases append.
    fn secure_dir() -> TempDir {
        let dir = short_tmp_dir("ep-repr", "sec");
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))
            .expect("chmod fixture dir to 0700");
        dir
    }

    /// Stage 1's identifier is `{xdg_runtime_dir}/yagura.sock` verbatim.
    #[test]
    fn stage1_identifier_is_xdg_runtime_dir_slash_yagura_sock() {
        let dir = secure_dir();
        let endpoint = resolve_endpoint_with(
            "1000",
            Some(OsStr::new(dir.path().to_str().expect("utf8 path"))),
            dir.path(),
            EndpointCreation::ReadOnly,
            &default_namespace(),
        )
        .expect("stage 1 must succeed for a valid XDG_RUNTIME_DIR");
        assert_eq!(
            endpoint,
            format!("{}/yagura.sock", dir.path().to_str().expect("utf8 path"))
        );
    }

    /// Stage 3's identifier is `{tmp_root}/yagura-{user_id}/ipc.sock`: the
    /// `yagura-{user_id}` directory name is what distinguishes it from stage
    /// 1's, which reuses the given directory as-is.
    #[test]
    fn stage3_identifier_embeds_yagura_dash_user_id_and_ends_with_ipc_sock() {
        let root = secure_dir();
        let user_id = "yagura-fake-uid";
        let endpoint = resolve_endpoint_with(
            user_id,
            None,
            root.path(),
            EndpointCreation::CreateIfMissing,
            &default_namespace(),
        )
        .expect("stage 3 must succeed when EndpointCreation::CreateIfMissing is passed");
        let expected_dir = root.path().join(format!("yagura-{user_id}"));
        assert_eq!(
            endpoint,
            format!("{}/ipc.sock", expected_dir.to_str().expect("utf8 path"))
        );
    }

    /// Stage 3's identifier varies with `user_id`, unlike stage 1's. Driven
    /// against the seam with a synthesized `tmp_root` and no
    /// `xdg_runtime_dir`, so an `XDG_RUNTIME_DIR` set by the calling session
    /// cannot select stage 1 and hide the property.
    #[test]
    fn stage3_identifier_differs_for_different_user_ids() {
        let root = secure_dir();
        let namespace = default_namespace();
        let endpoint_1 = resolve_endpoint_with(
            "yagura-fake-uid-1",
            None,
            root.path(),
            EndpointCreation::CreateIfMissing,
            &namespace,
        )
        .expect("stage 3 must succeed when EndpointCreation::CreateIfMissing is passed");
        let endpoint_2 = resolve_endpoint_with(
            "yagura-fake-uid-2",
            None,
            root.path(),
            EndpointCreation::CreateIfMissing,
            &namespace,
        )
        .expect("stage 3 must succeed when EndpointCreation::CreateIfMissing is passed");
        assert_ne!(
            endpoint_1, endpoint_2,
            "resolve_endpoint_with's stage 3 identifier must vary with user_id"
        );
    }
}

#[cfg(unix)]
mod unix_base_dir {
    use std::ffi::OsStr;
    use std::path::PathBuf;

    /// Scoped to non-macOS Unix: the macOS seams ignore `primary` entirely and
    /// have their own delegation cases below.
    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn primary_is_returned_unchanged_when_present() {
        let primary = OsStr::new("/run/user/1000");
        assert_eq!(
            yagura_os::data_base_dir_from(Some(primary), None),
            Some(PathBuf::from(primary)),
            "Unix: an explicit primary (XDG_DATA_HOME) is returned as-is"
        );
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn home_fallback_appends_dot_local_share_for_data() {
        let home = OsStr::new("/home/example");
        assert_eq!(
            yagura_os::data_base_dir_from(None, Some(home)),
            Some(PathBuf::from("/home/example/.local/share")),
            "Unix: an absent primary (XDG_DATA_HOME) falls back to {{home}}/.local/share"
        );
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn home_fallback_appends_dot_local_state_for_state() {
        let home = OsStr::new("/home/example");
        assert_eq!(
            yagura_os::state_base_dir_from(None, Some(home)),
            Some(PathBuf::from("/home/example/.local/state")),
            "Unix: an absent primary (XDG_STATE_HOME) falls back to {{home}}/.local/state"
        );
    }

    /// Not scoped away from macOS: `data_base_dir_from` returns `None` there
    /// too when `home` is absent, so the message stays OS-neutral rather than
    /// naming the Linux-only `XDG_DATA_HOME`.
    #[test]
    fn none_is_returned_when_both_primary_and_home_are_absent() {
        assert_eq!(
            yagura_os::data_base_dir_from(None, None),
            None,
            "Unix: with neither an explicit primary nor home available, the seam returns None"
        );
    }

    /// The config seam prefers an explicit `primary` (`XDG_CONFIG_HOME`) over
    /// `home`, so a relocated configuration root is honored rather than
    /// silently sent back to `{home}/.config`.
    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn config_primary_is_returned_unchanged_when_present() {
        let primary = OsStr::new("/home/example/somewhere-else");
        assert_eq!(
            yagura_os::config_base_dir_from(Some(primary), Some(OsStr::new("/home/example"))),
            Some(PathBuf::from(primary)),
            "Unix: an explicit primary (XDG_CONFIG_HOME) is returned as-is and wins over home"
        );
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn config_home_fallback_appends_dot_config() {
        let home = OsStr::new("/home/example");
        assert_eq!(
            yagura_os::config_base_dir_from(None, Some(home)),
            Some(PathBuf::from("/home/example/.config")),
            "Unix: an absent primary (XDG_CONFIG_HOME) falls back to {{home}}/.config -- the \
             root the GUI's playbook and keymap live under on Linux"
        );
    }

    /// The config-seam counterpart of
    /// `none_is_returned_when_both_primary_and_home_are_absent`: not scoped
    /// away from macOS, so the message stays OS-neutral.
    #[test]
    fn config_none_is_returned_when_both_primary_and_home_are_absent() {
        assert_eq!(
            yagura_os::config_base_dir_from(None, None),
            None,
            "Unix: with neither an explicit primary nor home available, the config seam \
             returns None"
        );
    }

    /// The XDG-style home-fallback suffixes must stay distinct, so a suffix
    /// constant copy-pasted in either direction is caught. Scoped
    /// to non-macOS Unix, where the seams instead share one value
    /// (`data_state_and_config_base_dir_from_agree_on_macos` below).
    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn config_home_fallback_suffix_differs_from_the_data_and_state_seams() {
        let home = OsStr::new("/home/example");
        let config = yagura_os::config_base_dir_from(None, Some(home));
        assert_ne!(
            config,
            yagura_os::data_base_dir_from(None, Some(home)),
            "XDG-style Unix invariant (not macOS, which shares one value instead): \
             the config seam's home fallback (.config) must not collide with the data seam's \
             (.local/share); a collision would place configuration under the data root"
        );
        assert_ne!(
            config,
            yagura_os::state_base_dir_from(None, Some(home)),
            "XDG-style Unix invariant (not macOS, which shares one value instead): \
             the config seam's home fallback (.config) must not collide with the state seam's \
             (.local/state); a collision would place configuration under the log root"
        );
    }

    // -------------------------------------------------------------------
    // macOS delegation: `data/state/config_base_dir_from` ignore
    // `primary` on macOS and delegate to `macos_base_dir_from(home)`. Runs on
    // macOS only; the mapping itself is pinned off-macOS by the top-level
    // `macos_base_dir_from_*` cases above.
    // -------------------------------------------------------------------

    #[cfg(target_os = "macos")]
    #[test]
    fn data_base_dir_from_ignores_primary_and_delegates_to_macos_base_dir_from_on_macos() {
        let primary = OsStr::new("/run/user/1000");
        let home = OsStr::new("/Users/example");
        assert_eq!(
            yagura_os::data_base_dir_from(Some(primary), Some(home)),
            yagura_os::macos_base_dir_from(Some(home)),
            "macOS: data_base_dir_from must ignore primary (there is no XDG-style override) \
             and delegate to macos_base_dir_from"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn state_base_dir_from_ignores_primary_and_delegates_to_macos_base_dir_from_on_macos() {
        let primary = OsStr::new("/run/user/1000");
        let home = OsStr::new("/Users/example");
        assert_eq!(
            yagura_os::state_base_dir_from(Some(primary), Some(home)),
            yagura_os::macos_base_dir_from(Some(home)),
            "macOS: state_base_dir_from must ignore primary and delegate to \
             macos_base_dir_from, exactly as the data seam does"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn config_base_dir_from_ignores_primary_and_delegates_to_macos_base_dir_from_on_macos() {
        let primary = OsStr::new("/run/user/1000");
        let home = OsStr::new("/Users/example");
        assert_eq!(
            yagura_os::config_base_dir_from(Some(primary), Some(home)),
            yagura_os::macos_base_dir_from(Some(home)),
            "macOS: config_base_dir_from must ignore primary and delegate to \
             macos_base_dir_from, exactly as the data and state seams do"
        );
    }

    /// On macOS the data, state and config seams share one base directory,
    /// the opposite of the XDG-style invariant pinned by
    /// `config_home_fallback_suffix_differs_from_the_data_and_state_seams`.
    #[cfg(target_os = "macos")]
    #[test]
    fn data_state_and_config_base_dir_from_agree_on_macos() {
        // Explicit `Option<PathBuf>` annotations keep the `PathBuf` import
        // used on a macOS build, where every other consumer is cfg'd away.
        let home = OsStr::new("/Users/example");
        let data: Option<PathBuf> = yagura_os::data_base_dir_from(None, Some(home));
        let state: Option<PathBuf> = yagura_os::state_base_dir_from(None, Some(home));
        let config: Option<PathBuf> = yagura_os::config_base_dir_from(None, Some(home));
        assert_eq!(
            data, state,
            "macOS: the data and state seams must agree (both are \
             {{home}}/Library/Application Support)"
        );
        assert_eq!(
            data, config,
            "macOS: the data and config seams must agree (both are \
             {{home}}/Library/Application Support)"
        );
    }
}
