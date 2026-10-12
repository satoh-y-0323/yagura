#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: launch-profile directory resolution (`resolve_profiles_dir` in
//! `cli.rs`).
//!
//! Resolution has three stages, in order: the `--profiles-dir` argument, then
//! `YAGURA_PROFILES_DIR` (an empty value counts as unset), then the profiles directory
//! under the platform's configuration base. There is no executable-adjacent or ancestor
//! search.
//!
//! The two explicit stages are taken as given and answer alone: a directory that does
//! not exist, or a path that turns out to be a file, is still what resolution returns,
//! and the mismatch is left for the enumeration to report. The third stage is laid over
//! the shipped profiles, so it is returned whether or not it exists yet: a user's first
//! change creates it.

use std::ffi::OsString;
use std::path::PathBuf;

use yagura_daemon::cli::{ProfilesDirResolution, resolve_profiles_dir};
use yagura_os::InstanceNamespace;

/// The default instance's namespace, for the cases below that are not about
/// instance naming at all.
fn default_namespace() -> InstanceNamespace {
    yagura_os::resolve_instance_from(None).expect("the default instance always resolves")
}

fn base() -> PathBuf {
    PathBuf::from("config-base")
}

fn config_root_profiles() -> PathBuf {
    let namespace =
        yagura_os::resolve_instance_from(None).expect("the default instance always resolves");
    yagura_os::instance_dir(&base(), &namespace).join("profiles")
}

#[test]
fn the_argument_wins_over_the_environment_variable_and_answers_alone() {
    let result = resolve_profiles_dir(
        Some(PathBuf::from("arg-profiles")),
        Some(OsString::from("env-profiles")),
        Some(base()),
        &default_namespace(),
    );
    assert_eq!(
        result,
        ProfilesDirResolution::Explicit(PathBuf::from("arg-profiles"))
    );
}

#[test]
fn the_environment_variable_answers_alone_when_no_argument_is_given() {
    let result = resolve_profiles_dir(
        None,
        Some(OsString::from("env-profiles")),
        Some(base()),
        &default_namespace(),
    );
    assert_eq!(
        result,
        ProfilesDirResolution::Explicit(PathBuf::from("env-profiles"))
    );
}

#[test]
fn an_empty_environment_value_counts_as_unset() {
    let result = resolve_profiles_dir(
        None,
        Some(OsString::from("")),
        Some(base()),
        &default_namespace(),
    );
    assert_eq!(
        result,
        ProfilesDirResolution::ConfigRoot(config_root_profiles())
    );
}

#[test]
fn the_config_root_stage_is_returned_whether_or_not_it_exists() {
    let result = resolve_profiles_dir(None, None, Some(base()), &default_namespace());
    assert_eq!(
        result,
        ProfilesDirResolution::ConfigRoot(config_root_profiles()),
        "the config root's profiles directory holds only the user's changes, so it need \
         not exist before the first one"
    );
}

#[test]
fn no_stage_resolving_leaves_the_shipped_profiles_alone() {
    let result = resolve_profiles_dir(None, None, None, &default_namespace());
    assert_eq!(result, ProfilesDirResolution::Unresolved);
}
