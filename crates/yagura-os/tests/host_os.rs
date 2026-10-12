#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: `yagura_os::program`'s host-operating-system selection
//! -- `HostOs`, `HostOs::platform`, and `select_for_os`, the one rule set
//! the daemon and the GUI both read.

use yagura_os::program::{HostOs, Platform, select_for_os};

const ALL_HOST_OSES: [HostOs; 3] = [HostOs::Windows, HostOs::MacOs, HostOs::Linux];

/// Places `own` under the argument that belongs to `os`, leaving the other
/// two `Option`s empty.
fn args_with_only_own_key(os: HostOs, own: &str) -> (Option<&str>, Option<&str>, Option<&str>) {
    match os {
        HostOs::Windows => (Some(own), None, None),
        HostOs::MacOs => (None, Some(own), None),
        HostOs::Linux => (None, None, Some(own)),
    }
}

/// Places `other` under the two arguments that do *not* belong to `os`,
/// leaving `os`'s own argument empty.
fn args_with_only_other_keys(
    os: HostOs,
    other: &str,
) -> (Option<&str>, Option<&str>, Option<&str>) {
    match os {
        HostOs::Windows => (None, Some(other), Some(other)),
        HostOs::MacOs => (Some(other), None, Some(other)),
        HostOs::Linux => (Some(other), Some(other), None),
    }
}

// ---------------------------------------------------------------------
// select_for_os
// ---------------------------------------------------------------------

#[test]
fn select_for_os_each_os_prefers_its_own_key_over_default() {
    for os in ALL_HOST_OSES {
        let (windows, macos, linux) = args_with_only_own_key(os, "own");
        let picked = select_for_os(os, windows, macos, linux, Some("default"));
        assert_eq!(
            picked,
            Some("own"),
            "{os:?} must prefer its own key over default"
        );
    }
}

#[test]
fn select_for_os_each_os_falls_back_to_default_when_its_own_key_is_absent() {
    for os in ALL_HOST_OSES {
        let picked = select_for_os(os, None, None, None, Some("default"));
        assert_eq!(
            picked,
            Some("default"),
            "{os:?} must fall back to default when its own key is absent"
        );
    }
}

#[test]
fn select_for_os_each_os_is_none_when_neither_its_own_key_nor_default_is_present() {
    for os in ALL_HOST_OSES {
        let picked = select_for_os::<str>(os, None, None, None, None);
        assert_eq!(
            picked, None,
            "{os:?} must select nothing when its own key and default are both absent"
        );
    }
}

#[test]
fn select_for_os_never_selects_another_os_s_key() {
    for os in ALL_HOST_OSES {
        let (windows, macos, linux) = args_with_only_other_keys(os, "other");
        let picked = select_for_os(os, windows, macos, linux, None);
        assert_eq!(
            picked, None,
            "{os:?} must not select a key that belongs to a different operating system"
        );
    }
}

// ---------------------------------------------------------------------
// HostOs::host / HostOs::platform
// ---------------------------------------------------------------------

#[test]
fn host_os_host_matches_this_binary_s_operating_system() {
    let expected = if cfg!(windows) {
        HostOs::Windows
    } else if cfg!(target_os = "macos") {
        HostOs::MacOs
    } else {
        HostOs::Linux
    };
    assert_eq!(HostOs::host(), expected);
}

#[test]
fn host_os_platform_maps_windows_to_the_windows_platform() {
    assert_eq!(HostOs::Windows.platform(), Platform::Windows);
}

#[test]
fn host_os_platform_maps_macos_to_the_unix_platform() {
    assert_eq!(HostOs::MacOs.platform(), Platform::Unix);
}

#[test]
fn host_os_platform_maps_linux_to_the_unix_platform() {
    assert_eq!(HostOs::Linux.platform(), Platform::Unix);
}

// ---------------------------------------------------------------------
// Platform::host and HostOs::host agree
// ---------------------------------------------------------------------

#[test]
fn platform_host_agrees_with_host_os_host_s_platform() {
    assert_eq!(Platform::host(), HostOs::host().platform());
}
