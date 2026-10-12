#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// `profile::list_profiles` is called directly here: it is the unit under test.
#![allow(clippy::disallowed_methods)]

//! Integration test: a profile's top-level `enabled` key, and a profile whose
//! `[command]` names no argv for the host operating system.
//!
//! The two are judged in different places on purpose. A profile with nothing to
//! launch on this operating system is left out of the listing altogether and is
//! unknown to every lookup by id. A disabled profile stays listed (the settings
//! view needs it to turn it back on) and is refused only when something asks to
//! start it: a window launch, or the command that lists its models.
//!
//! Every profile is written by the test itself into a directory it just made,
//! and the operating system is a value in the configuration, so all three
//! operating systems' rules are measured on whichever host runs this binary.

mod common;

use std::path::{Path, PathBuf};

use yagura_daemon::DaemonConfig;
use yagura_daemon::profile::{
    LaunchOptionValues, ModelChoicesError, ModelChoicesPlan, ProfileLaunch, ProfileLaunchError,
    ProfileListing, ProfileOs, ProfileParseError, list_profiles, parse_profile, resolve_launch,
    resolve_model_choices,
};
use yagura_os::program::ProgramLookup;
use yagura_test_support::paths::TempDir;

// --- documents ----------------------------------------------------------------

const HEAD: &str = "schema = 1\ndisplay_name = \"Toggle\"\n";

/// A document with `top_level` written between the head and the first table
/// header, followed by `tables` verbatim.
fn document(top_level: &str, tables: &str) -> String {
    format!("{HEAD}{top_level}\n{tables}")
}

const DEFAULT_COMMAND: &str = "[command]\ndefault = [\"toggle-program\"]\n";
const WINDOWS_ONLY_COMMAND: &str = "[command]\nwindows = [\"windows-only-program\"]\n";
const MACOS_ONLY_COMMAND: &str = "[command]\nmacos = [\"macos-only-program\"]\n";
const LINUX_ONLY_COMMAND: &str = "[command]\nlinux = [\"linux-only-program\"]\n";

/// A `[model]` table and a `[model.choices]` table that lists models through a
/// command. Kept out of `format!` so its placeholder is never read as a format
/// argument.
const MODEL_WITH_LISTING_COMMAND: &str = "[model]\nargs = [\"--tier\", \"{value}\"]\n\n\
                                          [model.choices]\nargs = [\"list\"]\nformat = \
                                          \"lines\"\nseparator = \",\"\n";

const ALL_OPERATING_SYSTEMS: [ProfileOs; 3] =
    [ProfileOs::Windows, ProfileOs::MacOs, ProfileOs::Linux];

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| (*item).to_string()).collect()
}

fn write_profile(dir: &Path, id: &str, source: &str) {
    std::fs::write(dir.join(format!("{id}.toml")), source).expect("profile should be writable");
}

// --- injected configuration ------------------------------------------------------

fn file_exists(path: &Path) -> bool {
    path.is_file()
}

fn empty_lookup() -> ProgramLookup {
    ProgramLookup {
        path_dirs: Vec::new(),
        extra_bin_dirs: Vec::new(),
        windows_comspec: None,
    }
}

fn config_with(profiles_dir: PathBuf, host_os: ProfileOs, lookup: ProgramLookup) -> DaemonConfig {
    DaemonConfig {
        profiles_dir: Some(profiles_dir),
        host_os,
        program_lookup: lookup,
        program_is_executable: file_exists,
        default_shell: Some(PathBuf::from("resolved-default-shell")),
        detection_dir: None,
        ..DaemonConfig::default()
    }
}

fn listed_ids(listing: &ProfileListing) -> Vec<String> {
    let mut ids: Vec<String> = listing
        .profiles
        .iter()
        .map(|entry| entry.id.clone())
        .collect();
    ids.sort();
    ids
}

fn launch(config: &DaemonConfig, id: &str) -> Result<ProfileLaunch, ProfileLaunchError> {
    resolve_launch(config, id, None, None, &LaunchOptionValues::default())
}

/// The id of the one profile, among the three single-OS ones, whose argv is
/// defined for `os`.
fn own_single_os_id(os: ProfileOs) -> &'static str {
    match os {
        ProfileOs::Windows => "only-windows",
        ProfileOs::MacOs => "only-macos",
        ProfileOs::Linux => "only-linux",
    }
}

// --- reading `enabled` ------------------------------------------------------------

#[test]
fn an_omitted_enabled_key_reads_as_enabled() {
    let profile = parse_profile("toggle", &document("", DEFAULT_COMMAND))
        .expect("a document without the key must be accepted");
    assert!(
        profile.enabled,
        "a profile written before the key existed must stay usable"
    );
}

#[test]
fn an_enabled_key_set_to_false_reads_as_disabled() {
    let profile = parse_profile("toggle", &document("enabled = false", DEFAULT_COMMAND))
        .expect("a boolean key must be accepted");
    assert!(!profile.enabled, "`enabled = false` must read as disabled");
}

#[test]
fn an_enabled_key_set_to_true_reads_as_enabled() {
    let profile = parse_profile("toggle", &document("enabled = true", DEFAULT_COMMAND))
        .expect("a boolean key must be accepted");
    assert!(
        profile.enabled,
        "a hand-written `enabled = true` must read as enabled"
    );
}

#[test]
fn an_enabled_key_that_is_not_a_boolean_excludes_the_profile_under_the_fixed_parse_wording() {
    let tree = TempDir::new("yagura-cli-test", "profile-enabled-mistyped");
    for (label, dir_name, value) in [
        ("a string", "string", "enabled = \"false\""),
        ("a number", "number", "enabled = 0"),
    ] {
        let source = document(value, DEFAULT_COMMAND);
        match parse_profile("toggle", &source) {
            Err(ProfileParseError::Toml(_)) => {}
            other => panic!(
                "{label} under `enabled` must fail to decode like any other mistyped key, \
                 got {other:?}"
            ),
        }

        let dir = tree.make_dir(dir_name);
        write_profile(&dir, "mistyped", &source);
        let listing = list_profiles(&config_with(dir, ProfileOs::Windows, empty_lookup()));
        assert!(
            listing.profiles.is_empty(),
            "{label} under `enabled` must exclude the profile, got {:?}",
            listed_ids(&listing)
        );
        assert_eq!(
            listing.warnings,
            vec!["profile file \"mistyped.toml\" could not be parsed".to_string()],
            "the exclusion reads exactly as any other decode failure and names nothing read \
             from the document ({label})"
        );
    }
}

// --- the listing on each operating system --------------------------------------------

#[test]
fn a_listing_leaves_out_every_profile_with_no_argv_for_the_host_and_keeps_the_rest() {
    let tree = TempDir::new("yagura-cli-test", "profile-enabled-listing-per-os");
    let dir = tree.make_dir("profiles");
    write_profile(&dir, "only-windows", &document("", WINDOWS_ONLY_COMMAND));
    write_profile(&dir, "only-macos", &document("", MACOS_ONLY_COMMAND));
    write_profile(&dir, "only-linux", &document("", LINUX_ONLY_COMMAND));
    write_profile(&dir, "default-argv", &document("", DEFAULT_COMMAND));
    write_profile(&dir, "default-shell", &document("", ""));
    write_profile(
        &dir,
        "switched-off",
        &document("enabled = false", DEFAULT_COMMAND),
    );

    for os in ALL_OPERATING_SYSTEMS {
        let listing = list_profiles(&config_with(dir.clone(), os, empty_lookup()));

        let mut expected = vec![
            "default-argv",
            "default-shell",
            own_single_os_id(os),
            "switched-off",
        ];
        expected.sort_unstable();
        assert_eq!(
            listed_ids(&listing),
            strings(&expected),
            "on {os:?} only the profiles with no argv for {os:?} are left out; one with an \
             argv of its own, one with a `default`, one with no `[command]` (the default \
             shell) and a disabled one all stay"
        );
        assert!(
            listing.warnings.is_empty(),
            "leaving out a profile the host cannot launch is not a problem to report \
             ({os:?}): {:?}",
            listing.warnings
        );
    }
}

#[test]
fn a_listing_reports_each_profile_s_enabled_state() {
    let tree = TempDir::new("yagura-cli-test", "profile-enabled-listing-state");
    let dir = tree.make_dir("profiles");
    write_profile(&dir, "omitted", &document("", DEFAULT_COMMAND));
    write_profile(
        &dir,
        "switched-on",
        &document("enabled = true", DEFAULT_COMMAND),
    );
    write_profile(
        &dir,
        "switched-off",
        &document("enabled = false", DEFAULT_COMMAND),
    );

    let listing = list_profiles(&config_with(dir, ProfileOs::Linux, empty_lookup()));

    let mut states: Vec<(String, bool)> = listing
        .profiles
        .iter()
        .map(|entry| (entry.id.clone(), entry.enabled))
        .collect();
    states.sort();
    assert_eq!(
        states,
        vec![
            ("omitted".to_string(), true),
            ("switched-off".to_string(), false),
            ("switched-on".to_string(), true),
        ],
        "a disabled profile is listed with its state rather than left out"
    );
}

#[test]
fn a_disabled_profile_with_no_argv_for_the_host_is_left_out_like_any_other() {
    let tree = TempDir::new(
        "yagura-cli-test",
        "profile-enabled-listing-disabled-no-argv",
    );
    let dir = tree.make_dir("profiles");
    write_profile(
        &dir,
        "off-and-windows-only",
        &document("enabled = false", WINDOWS_ONLY_COMMAND),
    );
    write_profile(&dir, "default-argv", &document("", DEFAULT_COMMAND));

    let listing = list_profiles(&config_with(dir, ProfileOs::Linux, empty_lookup()));

    assert_eq!(
        listed_ids(&listing),
        strings(&["default-argv"]),
        "a profile the host cannot launch is left out whatever its state"
    );
    assert!(listing.warnings.is_empty(), "{:?}", listing.warnings);
}

// --- launching ---------------------------------------------------------------------

#[test]
fn the_disabled_refusal_reads_as_one_fixed_sentence() {
    assert_eq!(
        ProfileLaunchError::Disabled.to_string(),
        "the launch profile is disabled"
    );
}

#[test]
fn launching_a_disabled_profile_is_refused_as_disabled() {
    let tree = TempDir::new("yagura-cli-test", "profile-enabled-launch-disabled");
    let dir = tree.make_dir("profiles");
    write_profile(
        &dir,
        "switched-off",
        &document("enabled = false", DEFAULT_COMMAND),
    );
    let config = config_with(dir, ProfileOs::Linux, empty_lookup());

    match launch(&config, "switched-off") {
        Err(ProfileLaunchError::Disabled) => {}
        other => panic!("a disabled profile must produce no launch content, got {other:?}"),
    }
}

#[test]
fn launching_an_enabled_profile_carries_the_argv_it_always_did() {
    let tree = TempDir::new("yagura-cli-test", "profile-enabled-launch-enabled");
    let dir = tree.make_dir("profiles");
    write_profile(&dir, "omitted", &document("", DEFAULT_COMMAND));
    write_profile(
        &dir,
        "switched-on",
        &document("enabled = true", DEFAULT_COMMAND),
    );
    let config = config_with(dir, ProfileOs::Linux, empty_lookup());

    for id in ["omitted", "switched-on"] {
        let launched = launch(&config, id)
            .unwrap_or_else(|err| panic!("an enabled profile must launch ({id}), got {err:?}"));
        assert_eq!(launched.argv, strings(&["toggle-program"]), "{id}");
    }
}

#[test]
fn launching_a_profile_with_no_argv_for_the_host_is_refused_as_unknown() {
    let tree = TempDir::new("yagura-cli-test", "profile-enabled-launch-no-argv");
    let dir = tree.make_dir("profiles");
    write_profile(&dir, "only-windows", &document("", WINDOWS_ONLY_COMMAND));
    write_profile(&dir, "only-macos", &document("", MACOS_ONLY_COMMAND));
    write_profile(&dir, "only-linux", &document("", LINUX_ONLY_COMMAND));

    for os in ALL_OPERATING_SYSTEMS {
        let config = config_with(dir.clone(), os, empty_lookup());
        for id in ["only-windows", "only-macos", "only-linux"] {
            let result = launch(&config, id);
            if id == own_single_os_id(os) {
                assert!(
                    result.is_ok(),
                    "{id} names an argv for {os:?} and must launch there, got {result:?}"
                );
            } else {
                match result {
                    Err(ProfileLaunchError::UnknownProfile) => {}
                    other => panic!(
                        "{id} names no argv for {os:?}, so it is unknown there like a \
                         profile that does not exist, got {other:?}"
                    ),
                }
            }
        }
    }
}

#[test]
fn a_disabled_profile_with_no_argv_for_the_host_is_refused_as_unknown_not_as_disabled() {
    let tree = TempDir::new("yagura-cli-test", "profile-enabled-launch-order");
    let dir = tree.make_dir("profiles");
    write_profile(
        &dir,
        "off-and-windows-only",
        &document("enabled = false", WINDOWS_ONLY_COMMAND),
    );

    let on_linux = config_with(dir.clone(), ProfileOs::Linux, empty_lookup());
    match launch(&on_linux, "off-and-windows-only") {
        Err(ProfileLaunchError::UnknownProfile) => {}
        other => panic!(
            "a profile the host cannot launch is unknown before its state is looked at, got \
             {other:?}"
        ),
    }

    let on_windows = config_with(dir, ProfileOs::Windows, empty_lookup());
    match launch(&on_windows, "off-and-windows-only") {
        Err(ProfileLaunchError::Disabled) => {}
        other => panic!(
            "on the operating system it names an argv for, the same profile is refused as \
             disabled, got {other:?}"
        ),
    }
}

// --- the command that lists a profile's models ----------------------------------------

#[test]
fn the_model_listing_command_of_a_disabled_profile_is_refused_with_the_disabled_sentence() {
    let tree = TempDir::new("yagura-cli-test", "profile-enabled-model-choices");
    let dir = tree.make_dir("profiles");
    let bin = tree.make_dir("bin");
    std::fs::write(bin.join("toggle-program.exe"), []).expect("program fixture should be writable");
    let tables = format!("{DEFAULT_COMMAND}\n{MODEL_WITH_LISTING_COMMAND}");
    write_profile(&dir, "switched-off", &document("enabled = false", &tables));
    write_profile(&dir, "switched-on", &document("", &tables));
    let lookup = ProgramLookup {
        path_dirs: vec![bin],
        extra_bin_dirs: Vec::new(),
        windows_comspec: Some(PathBuf::from("cmd.exe")),
    };
    let config = config_with(dir, ProfileOs::Windows, lookup);

    match resolve_model_choices(&config, "switched-on") {
        Ok(ModelChoicesPlan::Command { argv, .. }) => assert_eq!(
            argv,
            strings(&["toggle-program", "list"]),
            "the same document, enabled, plans its listing command"
        ),
        other => panic!("an enabled profile must plan its listing command, got {other:?}"),
    }

    match resolve_model_choices(&config, "switched-off") {
        Err(ModelChoicesError::Unavailable(reason)) => assert_eq!(
            reason,
            ProfileLaunchError::Disabled.to_string(),
            "the refusal reads exactly as a disabled launch does"
        ),
        other => panic!("nothing is started for a disabled profile's model listing, got {other:?}"),
    }
}

#[test]
fn the_model_listing_of_a_profile_with_no_argv_for_the_host_is_refused_as_unknown() {
    let tree = TempDir::new("yagura-cli-test", "profile-enabled-model-choices-no-argv");
    let dir = tree.make_dir("profiles");
    let tables = format!("{WINDOWS_ONLY_COMMAND}\n{MODEL_WITH_LISTING_COMMAND}");
    write_profile(&dir, "only-windows", &document("", &tables));
    let config = config_with(dir, ProfileOs::Linux, empty_lookup());

    match resolve_model_choices(&config, "only-windows") {
        Err(ModelChoicesError::UnknownProfile) => {}
        other => panic!(
            "a profile the host cannot launch is unknown to the model listing too, got \
             {other:?}"
        ),
    }
}
