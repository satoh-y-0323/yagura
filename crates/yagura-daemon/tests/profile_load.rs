#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// `profile::list_profiles` is called directly here: it is the unit under test.
#![allow(clippy::disallowed_methods)]

//! Integration test: launch profiles (`profile.rs`) -- reading, validating, enumerating
//! and turning a profile into the content of a launch.
//!
//! A profile is one TOML file whose name stem is its identifier. Validation splits into
//! two outcomes that must not be confused: a file that is *excluded* from the listing with
//! a warning, and a file that stays usable while a part of it is *ignored* with a warning.
//! One fixture carries each, so a rule that moves from one side to the other fails a named
//! test rather than a count.
//!
//! Availability is decided against a lookup handed in through the configuration, never
//! against the machine running the suite: the fixtures name programs that do not exist
//! anywhere, and a test that wants one to be found puts a file of that name into a
//! directory it just made. The operating system is a value in the configuration too, so
//! the Windows rules and the Unix rules are both measured on whichever host runs this
//! binary.
//!
//! Enumeration reads the directory on every call. That is what lets an operator add, edit
//! or remove a profile without restarting the daemon, so it is asserted directly rather
//! than inferred from the absence of a cache.

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use common::repo_detection_dir;
use common::store_fixtures::capture_warns;

use yagura_daemon::profile::{
    LaunchOptionAxis, LaunchOptionSpec, LaunchOptionValues, LaunchOptions, Profile, ProfileCommand,
    ProfileLaunch, ProfileLaunchError, ProfileListing, ProfileOs, ProfileParseError,
    Unavailability, apply_launch_options, argv_for_os, list_profiles, parse_profile,
    resolve_launch, resolve_option_value, unavailability, unavailable_reason,
};
use yagura_daemon::{DaemonConfig, RequestedCwd};
use yagura_os::program::{Platform, ProgramLookup};
use yagura_protocol::{LaunchOptionAlias, LaunchOptionInfo, ProfileUnavailableKind};
use yagura_test_support::paths::TempDir;

// --- fixtures ---------------------------------------------------------------

/// Directory holding the launch-profile fixture files; see its README for what each
/// file carries.
fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/profiles")
}

/// The identifier a fixture would be enumerated under: its file name stem.
fn fixture_id(file_name: &str) -> String {
    Path::new(file_name)
        .file_stem()
        .expect("a fixture file name has a stem")
        .to_string_lossy()
        .into_owned()
}

fn fixture_source(file_name: &str) -> String {
    let path = fixture_dir().join(file_name);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("failed to read the fixture at {}: {err}", path.display()))
}

/// Parses a fixture under the identifier it would be enumerated with, for the cases where
/// the file is expected to be accepted.
fn fixture_profile(file_name: &str) -> Profile {
    let id = fixture_id(file_name);
    parse_profile(&id, &fixture_source(file_name))
        .unwrap_or_else(|err| panic!("fixture {file_name} should be accepted, got {err:?}"))
}

/// Copies just the named fixtures into `dir`, so a listing case sees exactly the files its
/// property is about and nothing else.
fn copy_fixtures(dir: &Path, file_names: &[&str]) {
    for file_name in file_names {
        let from = fixture_dir().join(file_name);
        let to = dir.join(file_name);
        std::fs::copy(&from, &to).unwrap_or_else(|err| {
            panic!(
                "failed to copy {} to {}: {err}",
                from.display(),
                to.display()
            )
        });
    }
}

/// Writes a profile no fixture can hold, because an absolute working directory is spelled
/// differently on each operating system. The TOML literal string (single quotes) keeps a
/// Windows path's separators out of escape processing.
fn write_absolute_cwd_profile(dir: &Path, id: &str, cwd: &Path) {
    let source = format!(
        "schema = 1\ndisplay_name = \"Absolute Cwd\"\ncwd = '{}'\n\n[command]\ndefault = \
         [\"absolute-cwd-program\"]\n",
        cwd.display()
    );
    std::fs::write(dir.join(format!("{id}.toml")), source).expect("profile should be writable");
}

/// Writes a profile from a source built by the test itself, for properties that need an
/// argv the fixture set deliberately does not carry.
fn write_profile(dir: &Path, id: &str, source: &str) {
    std::fs::write(dir.join(format!("{id}.toml")), source).expect("profile should be writable");
}

/// Writes a program under `dir` this host actually resolves for the bare
/// name `name`: a `.exe` file on Windows (`Resolution::Direct` -- a
/// same-name extensionless file is never probed by `resolve_program`) and
/// an extensionless file with the execute bit set on Unix.
#[cfg(windows)]
fn write_available_program(dir: &Path, name: &str) {
    std::fs::write(dir.join(format!("{name}.exe")), [])
        .expect("program fixture should be writable");
}

#[cfg(not(windows))]
fn write_available_program(dir: &Path, name: &str) {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    std::fs::write(&path, []).expect("program fixture should be writable");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .expect("program fixture should be made executable");
}

// --- injected resolution environment ----------------------------------------

/// The predicate the resolution environment is driven by in these tests: mere existence,
/// so a case can make a program available by creating an empty file, on any host.
fn file_exists(path: &Path) -> bool {
    path.is_file()
}

/// A lookup that finds nothing at all.
fn empty_lookup() -> ProgramLookup {
    ProgramLookup {
        path_dirs: Vec::new(),
        extra_bin_dirs: Vec::new(),
        windows_comspec: None,
    }
}

/// A lookup that searches `dir` only, with an interpreter available for script shims.
fn lookup_in(dir: &Path) -> ProgramLookup {
    ProgramLookup {
        path_dirs: vec![dir.to_path_buf()],
        extra_bin_dirs: Vec::new(),
        windows_comspec: Some(PathBuf::from("cmd.exe")),
    }
}

/// A configuration with every profile-related input injected and nothing read from the
/// process environment. Written as a functional update so the field set stays additive.
fn config_with(
    profiles_dir: Option<PathBuf>,
    lookup: ProgramLookup,
    host_os: ProfileOs,
    default_shell: Option<PathBuf>,
) -> DaemonConfig {
    DaemonConfig {
        profiles_dir,
        host_os,
        program_lookup: lookup,
        program_is_executable: file_exists,
        default_shell,
        detection_dir: None,
        ..DaemonConfig::default()
    }
}

/// The identifiers a listing reported, sorted, so a case never depends on the order the
/// filesystem happened to hand the directory back in.
fn listed_ids(listing: &ProfileListing) -> Vec<String> {
    let mut ids: Vec<String> = listing
        .profiles
        .iter()
        .map(|entry| entry.id.clone())
        .collect();
    ids.sort();
    ids
}

/// The argv selected for `os`, copied out so a case can compare it against a plain vector.
fn selected_argv(command: &ProfileCommand, os: ProfileOs) -> Option<Vec<String>> {
    argv_for_os(command, os).map(<[String]>::to_vec)
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| (*item).to_string()).collect()
}

// --- the host operating system is a value -----------------------------------

#[test]
fn the_default_host_operating_system_is_the_one_this_binary_runs_on() {
    let expected = if cfg!(windows) {
        ProfileOs::Windows
    } else if cfg!(target_os = "macos") {
        ProfileOs::MacOs
    } else {
        ProfileOs::Linux
    };

    assert_eq!(
        ProfileOs::host(),
        expected,
        "the daemon decides which operating system it is running on in one place; the \
         program-resolution module stays free of that choice"
    );
    assert_eq!(
        DaemonConfig::default().host_os,
        expected,
        "the default configuration carries the host's own value, so nothing downstream \
         has to ask the platform again"
    );
}

#[test]
fn a_profile_operating_system_maps_onto_the_program_resolution_platform() {
    assert_eq!(ProfileOs::Windows.platform(), Platform::Windows);
    assert_eq!(
        ProfileOs::MacOs.platform(),
        Platform::Unix,
        "the profile schema distinguishes macOS from Linux for argv selection, while \
         program resolution only distinguishes Windows from Unix"
    );
    assert_eq!(ProfileOs::Linux.platform(), Platform::Unix);
}

// --- reading and validating a single profile --------------------------------

#[test]
fn a_well_formed_profile_parses_into_its_declared_fields() {
    let profile = fixture_profile("valid-default.toml");

    assert_eq!(profile.id, "valid-default");
    assert_eq!(profile.display_name, "Valid Default");
    assert_eq!(profile.detection, Some("generic-shell".to_string()));
    assert_eq!(profile.cwd, None);
    assert!(
        profile.env.is_empty(),
        "an absent [env] table is no environment at all, not an inherited one"
    );
    assert!(
        profile.warnings.is_empty(),
        "a file with nothing to complain about must not produce a warning, got {:?}",
        profile.warnings
    );

    let command = profile
        .command
        .as_ref()
        .expect("valid-default.toml has a [command] table");
    assert_eq!(
        selected_argv(command, ProfileOs::Windows),
        Some(strings(&["codex"])),
        "an argv given only under `default` is used by every operating system"
    );
}

#[test]
fn a_schema_version_the_daemon_does_not_support_is_rejected() {
    let result = parse_profile("invalid-schema", &fixture_source("invalid-schema.toml"));

    assert!(
        result.is_err(),
        "a schema version other than the supported one must be refused rather than read \
         optimistically, got {result:?}"
    );
}

#[test]
fn a_missing_display_name_is_rejected() {
    let result = parse_profile(
        "invalid-no-display-name",
        &fixture_source("invalid-no-display-name.toml"),
    );

    assert!(
        result.is_err(),
        "the display name is required: it becomes the default window name, so there is \
         no value to fall back to, got {result:?}"
    );
}

#[test]
fn a_display_name_that_breaks_the_window_label_rule_is_rejected() {
    let result = parse_profile(
        "invalid-display-name",
        &fixture_source("invalid-display-name.toml"),
    );

    assert!(
        result.is_err(),
        "the display name is validated by the same rule that guards a window name, so a \
         profile can never open a window under a name the daemon would refuse, got \
         {result:?}"
    );
}

#[test]
fn an_argv_containing_an_empty_element_is_rejected() {
    let result = parse_profile(
        "invalid-empty-arg",
        &fixture_source("invalid-empty-arg.toml"),
    );

    assert!(
        result.is_err(),
        "an empty argv element is not an argument; accepting it would hand the spawn a \
         token nobody wrote, got {result:?}"
    );
}

#[test]
fn an_argv_element_containing_a_nul_byte_is_rejected_even_though_toml_accepts_it() {
    // TOML's NUL-byte escape sequence is well-formed and accepted at the parse level,
    // so the rejection has to come from parse_profile's own call to
    // validate_argv_elements, never from the TOML decoder.
    let result = parse_profile(
        "nul-arg",
        "schema = 1\ndisplay_name = \"Nul Arg\"\n\n[command]\ndefault = [\"safe\", \
         \"bad\\u0000arg\"]\n",
    );

    assert!(
        result.is_err(),
        "a NUL byte in an argv element must be rejected exactly like an empty element, got \
         {result:?}"
    );
}

#[test]
fn an_identifier_outside_the_allowed_character_set_is_rejected() {
    let result = parse_profile(&fixture_id("Bad Id.toml"), &fixture_source("Bad Id.toml"));

    assert!(
        result.is_err(),
        "the identifier becomes the vendor badge and the value clients send back, so it \
         is held to the same character rule as a detection name, got {result:?}"
    );
}

#[test]
fn a_relative_working_directory_is_ignored_with_a_warning() {
    let profile = fixture_profile("relative-cwd.toml");

    assert_eq!(
        profile.cwd, None,
        "a relative working directory is dropped rather than resolved: against what it \
         would resolve is exactly the question nobody can answer for the operator"
    );
    assert!(
        !profile.warnings.is_empty(),
        "dropping a value the operator wrote is reported, never silent"
    );
}

#[test]
fn a_tilde_working_directory_is_ignored_with_a_warning() {
    let profile = fixture_profile("tilde-cwd.toml");

    assert_eq!(
        profile.cwd, None,
        "there is no shell in this path, so `~` is not a home directory here; it is \
         dropped rather than expanded"
    );
    assert!(!profile.warnings.is_empty());
}

#[test]
fn a_reserved_environment_key_is_dropped_with_a_warning_and_the_profile_stays_usable() {
    let profile = fixture_profile("reserved-env.toml");

    assert_eq!(
        profile.env.get("YAGURA_WINDOW_ID"),
        None,
        "the YAGURA_ prefix is reserved for what the daemon injects; a profile that \
         could set it could impersonate another window"
    );
    assert_eq!(
        profile.env.get("KEPT"),
        Some(&"kept value".to_string()),
        "only the reserved key is dropped; the rest of the table survives"
    );
    assert!(
        !profile.warnings.is_empty(),
        "the dropped key is reported so the operator learns why it had no effect"
    );
}

#[test]
fn a_reserved_environment_key_is_dropped_case_insensitively() {
    // Windows env-merging is itself case-insensitive, so is_reserved_env_key
    // (`profile.rs`) must be too, or a lowercase/mixed-case key silently
    // survives parsing and later overwrites the daemon's own injection at
    // spawn time.
    let profile = parse_profile(
        "reserved-env-case",
        "schema = 1\ndisplay_name = \"Reserved Env Case\"\n\n[env]\nyagura_status_token = \
         \"attacker\"\nYagura_Window_Id = \"attacker\"\nKEPT = \"kept value\"\n",
    )
    .expect("mixed-case reserved keys do not make the document invalid");

    assert_eq!(
        profile.env.get("yagura_status_token"),
        None,
        "a lowercase YAGURA_ prefix must be dropped exactly like the uppercase one"
    );
    assert_eq!(
        profile.env.get("Yagura_Window_Id"),
        None,
        "a mixed-case YAGURA_ prefix must be dropped exactly like the uppercase one"
    );
    assert_eq!(
        profile.env.get("KEPT"),
        Some(&"kept value".to_string()),
        "a key that does not start with the reserved prefix at all is unaffected"
    );
    assert!(
        !profile.warnings.is_empty(),
        "the dropped keys are reported so the operator learns why they had no effect"
    );
}

#[test]
fn yagura_instance_cannot_be_set_from_a_profile_in_any_case() {
    // A profile's [env] table is reserved-prefix-filtered by
    // is_reserved_env_key, which already matches every YAGURA_-prefixed key regardless of
    // case; YAGURA_INSTANCE is covered by that existing rule with no code change, so this
    // is a named regression test rather than new behavior to implement.
    for (key, source) in [
        (
            "YAGURA_INSTANCE",
            "schema = 1\ndisplay_name = \"Yagura Instance Upper\"\n\n[env]\n\
             YAGURA_INSTANCE = \"attacker\"\nKEPT = \"kept value\"\n",
        ),
        (
            "yagura_instance",
            "schema = 1\ndisplay_name = \"Yagura Instance Lower\"\n\n[env]\n\
             yagura_instance = \"attacker\"\nKEPT = \"kept value\"\n",
        ),
        (
            "Yagura_Instance",
            "schema = 1\ndisplay_name = \"Yagura Instance Mixed\"\n\n[env]\n\
             Yagura_Instance = \"attacker\"\nKEPT = \"kept value\"\n",
        ),
    ] {
        let profile = parse_profile("yagura-instance-case", source).unwrap_or_else(|err| {
            panic!("a reserved key must not invalidate the document: {err:?}")
        });
        assert_eq!(
            profile.env.get(key),
            None,
            "a launch profile must not be able to set {key}: a window's instance is decided \
             at daemon start-up, not by the profile that opens it"
        );
        assert_eq!(
            profile.env.get("KEPT"),
            Some(&"kept value".to_string()),
            "only the reserved key is dropped; the rest of the table survives"
        );
        assert!(
            !profile.warnings.is_empty(),
            "the dropped key is reported so the operator learns why it had no effect"
        );
    }
}

#[test]
fn environment_values_are_taken_verbatim() {
    let profile = fixture_profile("valid-with-env.toml");

    let expected: BTreeMap<String, String> = [
        ("PLAIN", "plain value"),
        ("EMPTY", ""),
        ("NOT_EXPANDED", "$HOME %USERPROFILE% ~"),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_string(), value.to_string()))
    .collect();

    assert_eq!(
        profile.env, expected,
        "values pass through unchanged: nothing here is a shell, so no expansion happens, \
         and an empty string is an empty value rather than a removal"
    );
    assert!(profile.warnings.is_empty());
}

#[test]
fn an_omitted_command_table_leaves_the_profile_without_an_argv_of_its_own() {
    let profile = fixture_profile("valid-shell-omitted.toml");

    assert!(
        profile.command.is_none(),
        "an omitted [command] is distinct from a [command] with no argv for this \
         operating system: the first means the daemon's default shell, the second means \
         not launchable here"
    );
    assert!(profile.warnings.is_empty());
}

// --- selecting the argv for an operating system -----------------------------

#[test]
fn an_operating_system_specific_argv_wins_over_the_default() {
    let profile = fixture_profile("valid-os-override.toml");
    let command = profile
        .command
        .as_ref()
        .expect("valid-os-override.toml has a [command] table");

    assert_eq!(
        selected_argv(command, ProfileOs::Windows),
        Some(strings(&["windows-program"]))
    );
    assert_eq!(
        selected_argv(command, ProfileOs::MacOs),
        Some(strings(&["macos-program"]))
    );
    assert_eq!(
        selected_argv(command, ProfileOs::Linux),
        Some(strings(&["linux-program"])),
        "each operating system takes its own key; the `default` argv is only reached when \
         that key is absent"
    );
}

#[test]
fn an_operating_system_without_a_key_and_without_a_default_selects_no_argv() {
    let profile = fixture_profile("valid-windows-only.toml");
    let command = profile
        .command
        .as_ref()
        .expect("valid-windows-only.toml has a [command] table");

    assert_eq!(
        selected_argv(command, ProfileOs::Windows),
        Some(strings(&["windows-only-program"]))
    );
    assert_eq!(
        selected_argv(command, ProfileOs::MacOs),
        None,
        "with no macos key and no default there is nothing to launch, which is a reason \
         to report rather than a fallback to another operating system's argv"
    );
    assert_eq!(selected_argv(command, ProfileOs::Linux), None);
}

// --- availability against an injected lookup --------------------------------

#[test]
fn a_program_found_in_the_injected_lookup_is_available() {
    let tree = TempDir::new("yagura-cli-test", "profile-available");
    let bin = tree.make_dir("bin");
    write_available_program(&bin, "codex");

    let config = config_with(None, lookup_in(&bin), ProfileOs::host(), None);
    let reason: Option<String> =
        unavailable_reason(&fixture_profile("valid-default.toml"), &config);

    assert_eq!(
        reason, None,
        "the profile's argv[0] is present in the one directory the injected lookup \
         searches, so it is launchable"
    );
}

#[test]
fn a_program_missing_from_the_injected_lookup_is_unavailable_with_a_reason() {
    let config = config_with(None, empty_lookup(), ProfileOs::host(), None);

    let reason: Option<String> =
        unavailable_reason(&fixture_profile("valid-default.toml"), &config);

    let reason = reason.expect("a program that resolves nowhere must carry a reason");
    assert!(
        !reason.is_empty(),
        "the reason is shown to a user as it stands, so it is never the empty string"
    );
}

#[test]
fn an_operating_system_with_no_argv_is_unavailable_with_a_reason() {
    let tree = TempDir::new("yagura-cli-test", "profile-no-argv");
    let bin = tree.make_dir("bin");
    tree.write_file("bin/windows-only-program", "");

    let config = config_with(None, lookup_in(&bin), ProfileOs::Linux, None);
    let reason: Option<String> =
        unavailable_reason(&fixture_profile("valid-windows-only.toml"), &config);

    assert!(
        reason.is_some(),
        "the program exists in the lookup, but this operating system has no argv naming \
         it, so the profile is not launchable here"
    );
}

#[test]
fn an_omitted_command_table_without_a_default_shell_is_unavailable_with_a_reason() {
    let config = config_with(None, empty_lookup(), ProfileOs::host(), None);

    let reason: Option<String> =
        unavailable_reason(&fixture_profile("valid-shell-omitted.toml"), &config);

    assert!(
        reason.is_some(),
        "a profile that means the default shell cannot launch when no shell was resolved \
         at start-up"
    );
}

#[test]
fn an_omitted_command_table_is_available_when_a_default_shell_was_resolved() {
    let tree = TempDir::new("yagura-cli-test", "profile-shell-present");
    tree.make_dir("bin");
    let shell = tree.write_file("bin/some-shell", "");

    let config = config_with(None, empty_lookup(), ProfileOs::host(), Some(shell));
    let reason: Option<String> =
        unavailable_reason(&fixture_profile("valid-shell-omitted.toml"), &config);

    assert_eq!(
        reason, None,
        "the resolved shell is an existing path, so it is launchable even though the \
         lookup finds nothing: a path is judged as written, not searched for"
    );
}

#[test]
fn an_argument_the_command_interpreter_would_reinterpret_makes_a_shim_unavailable() {
    let tree = TempDir::new("yagura-cli-test", "profile-metachar-arg");
    let bin = tree.make_dir("bin");
    tree.write_file("bin/shim-program.cmd", "");

    let profile = parse_profile(
        "metachar-argument",
        "schema = 1\ndisplay_name = \"Metachar Argument\"\n\n[command]\nwindows = \
         [\"shim-program\", \"%PATH%\"]\n",
    )
    .expect("a profile whose argv carries a metacharacter is still a valid file");

    let config = config_with(None, lookup_in(&bin), ProfileOs::Windows, None);
    let reason: Option<String> = unavailable_reason(&profile, &config);

    assert!(
        reason.is_some(),
        "a script shim can only be started through the command interpreter, and this \
         argument would be reinterpreted on the way, so the profile is refused rather \
         than launched as something else"
    );
}

#[test]
fn every_unavailable_reason_is_one_fixed_sentence_per_kind_and_names_no_path() {
    let tree = TempDir::new("yagura-cli-test", "profile-reason-kinds");
    let bin = tree.make_dir("bin");
    tree.write_file("bin/shim-program.cmd", "");
    let meta_bin = tree.make_dir("meta^dir");
    tree.write_file("meta^dir/shim-program.cmd", "");

    let host = ProfileOs::host();

    // Kind: the executable was not found. Two different profiles, so a reason that
    // embedded the program name could not be one sentence.
    let not_found = config_with(None, empty_lookup(), host, None);
    let missing_program: Option<String> =
        unavailable_reason(&fixture_profile("valid-default.toml"), &not_found);
    let other_missing_program: Option<String> =
        unavailable_reason(&fixture_profile("valid-with-env.toml"), &not_found);

    // Kind: the command interpreter would reinterpret the command line. Reached once
    // through a trailing argument and once through the shim's own path, which are two
    // routes inside the resolution and must still surface as the same sentence.
    let metachar_argument = parse_profile(
        "metachar-argument",
        "schema = 1\ndisplay_name = \"Metachar Argument\"\n\n[command]\nwindows = \
         [\"shim-program\", \"%PATH%\"]\n",
    )
    .expect("a metacharacter in an argument is a resolution matter, not a schema one");
    let plain_shim = parse_profile(
        "plain-shim",
        "schema = 1\ndisplay_name = \"Plain Shim\"\n\n[command]\nwindows = \
         [\"shim-program\"]\n",
    )
    .expect("a shim with no trailing arguments is a valid profile");
    let argument_reason: Option<String> = unavailable_reason(
        &metachar_argument,
        &config_with(None, lookup_in(&bin), ProfileOs::Windows, None),
    );
    let shim_path_reason: Option<String> = unavailable_reason(
        &plain_shim,
        &config_with(None, lookup_in(&meta_bin), ProfileOs::Windows, None),
    );

    // Kind: this operating system has no argv.
    let no_argv: Option<String> = unavailable_reason(
        &fixture_profile("valid-windows-only.toml"),
        &config_with(None, empty_lookup(), ProfileOs::Linux, None),
    );

    // Kind: the default shell was never resolved.
    let no_shell: Option<String> = unavailable_reason(
        &fixture_profile("valid-shell-omitted.toml"),
        &config_with(None, empty_lookup(), host, None),
    );

    let missing_program = missing_program.expect("missing program is a reason");
    let other_missing_program = other_missing_program.expect("missing program is a reason");
    let argument_reason = argument_reason.expect("a reinterpreted argument is a reason");
    let shim_path_reason = shim_path_reason.expect("a reinterpreted shim path is a reason");
    let no_argv = no_argv.expect("no argv for this operating system is a reason");
    let no_shell = no_shell.expect("an unresolved default shell is a reason");

    assert_eq!(
        missing_program, other_missing_program,
        "one kind is one sentence: two profiles that fail for the same reason must read \
         identically, which is what keeps a program name out of the text"
    );
    assert_eq!(
        argument_reason, shim_path_reason,
        "a shim path and a trailing argument are two routes to the same kind, so they \
         must not become two sentences a user has to tell apart"
    );

    let searched_directory = bin.to_string_lossy().into_owned();
    let kinds: [(&str, String); 4] = [
        ("executable not found", missing_program),
        ("command interpreter would reinterpret", argument_reason),
        ("no argv for this operating system", no_argv),
        ("default shell unresolved", no_shell),
    ];
    for (index, (label, reason)) in kinds.iter().enumerate() {
        assert!(
            !reason.trim().is_empty(),
            "the reason for {label} is displayed as it stands and must not be blank"
        );
        assert!(
            !reason.contains('/') && !reason.contains('\\'),
            "the reason for {label} must carry no path, and a path separator is the \
             cheapest evidence one leaked in: {reason:?}"
        );
        assert!(
            !reason.contains(&searched_directory),
            "the reason for {label} must not name the directory the lookup searched: \
             {reason:?}"
        );
        for (other_label, other_reason) in kinds.iter().skip(index + 1) {
            assert_ne!(
                reason, other_reason,
                "{label} and {other_label} are different reasons and must not share one \
                 sentence, or a user cannot tell which one to fix"
            );
        }
    }
}

// --- enumerating a directory ------------------------------------------------

#[test]
fn enumeration_reports_the_valid_files_under_their_file_name_stems() {
    let tree = TempDir::new("yagura-cli-test", "profile-list-valid");
    let dir = tree.make_dir("profiles");
    copy_fixtures(&dir, &["valid-default.toml", "valid-with-env.toml"]);

    let config = config_with(Some(dir), empty_lookup(), ProfileOs::host(), None);
    let listing: ProfileListing = list_profiles(&config);

    assert_eq!(
        listed_ids(&listing),
        strings(&["valid-default", "valid-with-env"]),
        "the identifier is the file name stem; nothing inside the file names the profile"
    );
    assert!(
        listing.warnings.is_empty(),
        "a directory of well-formed files produces no warnings, got {:?}",
        listing.warnings
    );

    let entry = listing
        .profiles
        .iter()
        .find(|entry| entry.id == "valid-default")
        .expect("valid-default must be listed");
    assert_eq!(entry.display_name, "Valid Default");
    assert_eq!(entry.detection, Some("generic-shell".to_string()));
    assert_eq!(entry.cwd, None);
}

#[test]
fn enumeration_reports_the_default_working_directory_of_a_profile_that_has_one() {
    let tree = TempDir::new("yagura-cli-test", "profile-list-cwd");
    let dir = tree.make_dir("profiles");
    let work = tree.make_dir("work");
    write_absolute_cwd_profile(&dir, "absolute-cwd", &work);

    let config = config_with(Some(dir), empty_lookup(), ProfileOs::host(), None);
    let listing: ProfileListing = list_profiles(&config);

    let entry = listing
        .profiles
        .iter()
        .find(|entry| entry.id == "absolute-cwd")
        .expect("absolute-cwd must be listed");
    assert_eq!(
        entry.cwd,
        Some(work),
        "an absolute working directory survives; it is what the dialog offers as the \
         initial value"
    );
}

#[test]
fn enumeration_excludes_each_unusable_file_and_warns_about_it_by_name() {
    let tree = TempDir::new("yagura-cli-test", "profile-list-excluded");
    let dir = tree.make_dir("profiles");
    let excluded = [
        "invalid-schema.toml",
        "invalid-no-display-name.toml",
        "invalid-display-name.toml",
        "invalid-empty-arg.toml",
        "Bad Id.toml",
    ];
    copy_fixtures(&dir, &excluded);
    copy_fixtures(&dir, &["valid-default.toml"]);

    let config = config_with(Some(dir), empty_lookup(), ProfileOs::host(), None);
    let listing: ProfileListing = list_profiles(&config);

    assert_eq!(
        listed_ids(&listing),
        strings(&["valid-default"]),
        "one unusable file must not take the rest of the directory down with it"
    );
    for file_name in excluded {
        let mentions: Vec<&String> = listing
            .warnings
            .iter()
            .filter(|warning: &&String| warning.contains(file_name))
            .collect();
        assert_eq!(
            mentions.len(),
            1,
            "each excluded file is reported on a line of its own that names it, so the \
             operator knows which file to open; warnings were {:?}",
            listing.warnings
        );
    }
    assert_eq!(
        listing.warnings.len(),
        excluded.len(),
        "the warnings stand one per excluded file, with nothing else mixed in: {:?}",
        listing.warnings
    );
}

/// The fixed clause `list_profiles` prints for each `ProfileParseError`
/// variant. Matched exhaustively, with no `_` arm, so a new variant fails
/// to compile here instead of silently falling out of this test's coverage
/// of "each" variant.
fn expected_wording(err: &ProfileParseError) -> &'static str {
    match err {
        ProfileParseError::InvalidIdentifier => "invalid identifier",
        ProfileParseError::Toml(_) => "could not be parsed",
        ProfileParseError::UnsupportedSchema(_) => "unsupported schema",
        ProfileParseError::InvalidDisplayName(_) => "invalid display name",
        ProfileParseError::InvalidArgvElement(_) => "invalid argv element",
        ProfileParseError::InvalidLaunchOption(_) => "invalid launch option",
        ProfileParseError::InvalidMcp(_) => "invalid mcp table",
        ProfileParseError::InvalidHooks(_) => "invalid hooks table",
        ProfileParseError::InvalidDelivery(_) => "invalid delivery table",
        ProfileParseError::InvalidMcpRegistration(_) => "invalid mcp registration table",
    }
}

#[test]
fn enumeration_states_a_fixed_reason_for_each_parse_failure_variant_without_the_underlying_detail()
{
    let tree = TempDir::new("yagura-cli-test", "profile-list-fixed-reason");
    let dir = tree.make_dir("profiles");
    let file_names = [
        "invalid-schema.toml",
        "invalid-no-display-name.toml",
        "invalid-display-name.toml",
        "invalid-empty-arg.toml",
        "Bad Id.toml",
        "invalid-launch-option-no-args.toml",
    ];
    copy_fixtures(&dir, &file_names);
    // A `[delivery]` table whose value is outside its domain is the one kind no shared
    // fixture file carries.
    let delivery_source = "schema = 1\ndisplay_name = \"Delivery\"\n\n[delivery]\nmax_chars = 0\n";
    write_profile(&dir, "invalid-delivery", delivery_source);

    let config = config_with(Some(dir), empty_lookup(), ProfileOs::host(), None);
    let listing: ProfileListing = list_profiles(&config);

    let delivery_err = parse_profile("invalid-delivery", delivery_source)
        .expect_err("a delivery table outside its domain must be rejected");
    assert!(
        matches!(delivery_err, ProfileParseError::InvalidDelivery(_)),
        "{delivery_err:?}"
    );
    let delivery_expected = format!(
        "profile file \"invalid-delivery.toml\" {}",
        expected_wording(&delivery_err)
    );
    assert!(
        listing.warnings.contains(&delivery_expected),
        "the warning must read exactly {delivery_expected:?}, got {:?}",
        listing.warnings
    );

    for file_name in file_names {
        let id = fixture_id(file_name);
        let err = parse_profile(&id, &fixture_source(file_name))
            .expect_err(&format!("fixture {file_name} must be rejected"));
        let expected = format!("profile file {file_name:?} {}", expected_wording(&err));
        assert!(
            listing.warnings.contains(&expected),
            "the warning for {file_name} must read exactly {expected:?} -- a fixed reason \
             naming no file content -- got {:?}",
            listing.warnings
        );
    }
}

#[test]
fn an_unsupported_schema_names_its_version_in_the_daemon_s_log_though_the_warning_stays_fixed() {
    let tree = TempDir::new("yagura-cli-test", "profile-list-unsupported-schema-log");
    let dir = tree.make_dir("profiles");
    copy_fixtures(&dir, &["invalid-schema.toml"]);

    let config = config_with(Some(dir), empty_lookup(), ProfileOs::host(), None);
    let (warn_lines, listing) = capture_warns(|| list_profiles(&config));

    assert!(
        listing
            .warnings
            .contains(&"profile file \"invalid-schema.toml\" unsupported schema".to_string()),
        "the client-visible warning must stay the fixed reason alone: got {:?}",
        listing.warnings
    );
    assert!(
        warn_lines
            .iter()
            .any(|line| line.contains("schema version 2")),
        "the daemon's own log must still name the rejected schema version through \
         ProfileParseError::UnsupportedSchema's source() chain, even though the \
         client-visible warning does not: got {warn_lines:?}"
    );
}

#[test]
fn enumeration_never_leaks_a_syntax_error_s_offending_line_or_value_into_a_warning() {
    let tree = TempDir::new("yagura-cli-test", "profile-list-syntax-error-secret");
    let dir = tree.make_dir("profiles");
    copy_fixtures(&dir, &["invalid-syntax-with-secret.toml"]);

    let config = config_with(Some(dir), empty_lookup(), ProfileOs::host(), None);
    let listing: ProfileListing = list_profiles(&config);

    assert!(
        listing.profiles.is_empty(),
        "a file with a syntax error must not be listed: {:?}",
        listing.profiles
    );
    assert_eq!(
        listing.warnings,
        vec!["profile file \"invalid-syntax-with-secret.toml\" could not be parsed".to_string()],
        "the warning must be the fixed reason alone -- no offending line, no field name, no \
         value -- got {:?}",
        listing.warnings
    );
    for warning in &listing.warnings {
        assert!(
            !warning.contains("not-a-real-token-0000"),
            "a parse-failure warning must never carry the file's own content, secret-shaped \
             or not: {warning:?}"
        );
        assert!(
            !warning.to_ascii_uppercase().contains("TOML PARSE ERROR"),
            "a parse-failure warning must never carry the underlying parser's diagnostic \
             text: {warning:?}"
        );
        assert!(
            !warning.contains("API_KEY"),
            "a parse-failure warning must never name a key from inside the file: {warning:?}"
        );
    }
}

#[test]
fn enumeration_never_leaks_the_underlying_io_error_when_a_profile_file_cannot_be_read() {
    let tree = TempDir::new("yagura-cli-test", "profile-list-unreadable-entry");
    let dir = tree.make_dir("profiles");
    // A directory whose name ends in `.toml` is a candidate under `toml_entries`'s
    // extension-only filter but fails `std::fs::read_to_string` on every operating
    // system, exercising the read-failure branch without relying on permission bits,
    // which differ across platforms.
    std::fs::create_dir(dir.join("unreadable-entry.toml"))
        .expect("directory fixture should be creatable");
    copy_fixtures(&dir, &["valid-default.toml"]);

    let config = config_with(Some(dir), empty_lookup(), ProfileOs::host(), None);
    let listing: ProfileListing = list_profiles(&config);

    assert_eq!(
        listed_ids(&listing),
        strings(&["valid-default"]),
        "a file that cannot be read must not take the rest of the directory down with it"
    );
    assert_eq!(
        listing.warnings,
        vec!["profile file \"unreadable-entry.toml\" could not be read".to_string()],
        "the warning must be the fixed reason alone, naming no underlying OS error text: got \
         {:?}",
        listing.warnings
    );
}

#[test]
fn enumeration_without_a_configured_directory_reports_no_profiles_and_says_why() {
    let config = config_with(None, empty_lookup(), ProfileOs::host(), None);

    let listing: ProfileListing = list_profiles(&config);

    assert!(
        listing.profiles.is_empty(),
        "with no directory there is nothing to enumerate"
    );
    assert!(
        !listing.warnings.is_empty(),
        "an empty list with no explanation is indistinguishable from a directory that \
         happens to hold nothing, so the reason is always carried"
    );
}

#[test]
fn enumeration_of_a_directory_that_does_not_exist_reports_no_profiles_and_says_why() {
    let tree = TempDir::new("yagura-cli-test", "profile-list-missing-dir");
    let missing = tree.make_dir("profiles").join("no-such-directory");

    let config = config_with(Some(missing), empty_lookup(), ProfileOs::host(), None);
    let listing: ProfileListing = list_profiles(&config);

    assert!(listing.profiles.is_empty());
    assert!(
        !listing.warnings.is_empty(),
        "the operator pointed at this directory, so its absence is reported here rather \
         than silently turning into another location's contents"
    );
}

#[test]
fn enumeration_of_a_path_that_is_a_file_reports_no_profiles_and_says_why() {
    let tree = TempDir::new("yagura-cli-test", "profile-list-file-dir");
    tree.make_dir("profiles");
    let file = tree.write_file("profiles/not-a-directory", "");

    let config = config_with(Some(file), empty_lookup(), ProfileOs::host(), None);
    let listing: ProfileListing = list_profiles(&config);

    assert!(listing.profiles.is_empty());
    assert!(
        !listing.warnings.is_empty(),
        "a file where a directory was expected is reported, not treated as an empty \
         directory"
    );
}

#[test]
fn enumeration_failure_warning_names_no_path() {
    // The resolved directory path (which, under the real config root, embeds
    // the home directory / username) must never appear in a warning the MCP
    // response and the GUI both surface; it belongs in the daemon's local log
    // only.
    let tree = TempDir::new("yagura-cli-test", "profile-list-file-dir-no-path");
    tree.make_dir("profiles");
    let file = tree.write_file("profiles/not-a-directory", "");

    let config = config_with(Some(file.clone()), empty_lookup(), ProfileOs::host(), None);
    let listing: ProfileListing = list_profiles(&config);

    assert!(!listing.warnings.is_empty());
    let searched_path = file.to_string_lossy().into_owned();
    for warning in &listing.warnings {
        assert!(
            !warning.contains(&searched_path),
            "a directory-enumeration-failure warning must not embed the resolved path: \
             {warning:?}"
        );
        assert!(
            !warning.contains('\\') && !warning.contains('/'),
            "a directory-enumeration-failure warning must carry no path at all, and a path \
             separator is the cheapest evidence one leaked in: {warning:?}"
        );
    }
}

#[test]
fn enumeration_warns_on_an_entry_whose_detection_definition_is_absent() {
    let tree = TempDir::new("yagura-cli-test", "profile-list-detection");
    let dir = tree.make_dir("profiles");
    copy_fixtures(&dir, &["valid-default.toml"]);
    let empty_definitions = tree.make_dir("definitions");

    let present = DaemonConfig {
        detection_dir: Some(repo_detection_dir()),
        ..config_with(Some(dir.clone()), empty_lookup(), ProfileOs::host(), None)
    };
    let absent = DaemonConfig {
        detection_dir: Some(empty_definitions),
        ..config_with(Some(dir), empty_lookup(), ProfileOs::host(), None)
    };

    let with_definition: ProfileListing = list_profiles(&present);
    let without_definition: ProfileListing = list_profiles(&absent);

    let found = with_definition
        .profiles
        .iter()
        .find(|entry| entry.id == "valid-default")
        .expect("valid-default must be listed");
    assert_eq!(
        found.detection_warning, None,
        "the profile names a definition this directory really holds, so there is nothing \
         to warn about"
    );

    let not_found = without_definition
        .profiles
        .iter()
        .find(|entry| entry.id == "valid-default")
        .expect("a profile whose definition is missing is still listed");
    assert!(
        not_found.detection_warning.is_some(),
        "a profile naming a definition that is not there stays selectable -- a window opened \
         from it would stay unknown -- so the gap is carried as a warning next to the entry"
    );
}

#[test]
fn enumeration_decides_availability_with_the_injected_lookup() {
    let tree = TempDir::new("yagura-cli-test", "profile-list-availability");
    let dir = tree.make_dir("profiles");
    copy_fixtures(&dir, &["valid-default.toml"]);
    let bin = tree.make_dir("bin");

    let config = config_with(Some(dir.clone()), lookup_in(&bin), ProfileOs::host(), None);
    let before: ProfileListing = list_profiles(&config);
    let entry = before
        .profiles
        .first()
        .expect("valid-default must be listed");
    assert!(
        !entry.available,
        "the injected lookup searches one empty directory, so nothing is launchable"
    );
    assert!(
        entry.unavailable_reason.is_some(),
        "an entry that cannot be launched carries the reason the dialog shows beside it"
    );

    write_available_program(&bin, "codex");
    let after: ProfileListing = list_profiles(&config);
    let entry = after
        .profiles
        .first()
        .expect("valid-default must be listed");
    assert!(
        entry.available,
        "the program now exists in the searched directory, and availability is decided \
         per request rather than remembered from start-up"
    );
    assert_eq!(
        entry.unavailable_reason, None,
        "an available entry carries no reason: a reason with nothing to explain would be \
         shown next to a selectable profile"
    );
}

#[test]
fn the_availability_a_listing_reports_is_the_one_the_launch_path_would_decide() {
    let tree = TempDir::new("yagura-cli-test", "profile-list-one-judgement");
    let dir = tree.make_dir("profiles");
    copy_fixtures(&dir, &["valid-default.toml", "valid-shell-omitted.toml"]);

    let config = config_with(Some(dir), empty_lookup(), ProfileOs::host(), None);
    let listing: ProfileListing = list_profiles(&config);

    for entry in &listing.profiles {
        let profile = fixture_profile(&format!("{}.toml", entry.id));
        let direct: Option<String> = unavailable_reason(&profile, &config);
        assert_eq!(
            entry.unavailable_reason, direct,
            "the listing and the launch path ask one function, so what a list shows and \
             what a spawn attempts can never disagree ({})",
            entry.id
        );
        assert_eq!(
            entry.available,
            direct.is_none(),
            "the availability flag is exactly the absence of a reason ({})",
            entry.id
        );
    }
}

#[test]
fn each_reason_is_decided_together_with_its_kind() {
    let tree = TempDir::new("yagura-cli-test", "profile-reason-kind-pairs");
    let bin = tree.make_dir("bin");
    tree.write_file("bin/shim-program.cmd", "");
    let meta_bin = tree.make_dir("meta^dir");
    tree.write_file("meta^dir/shim-program.cmd", "");
    let host = ProfileOs::host();

    let metachar_argument = parse_profile(
        "metachar-argument",
        "schema = 1\ndisplay_name = \"Metachar Argument\"\n\n[command]\nwindows = \
         [\"shim-program\", \"%PATH%\"]\n",
    )
    .expect("a valid profile");
    let plain_shim = parse_profile(
        "plain-shim",
        "schema = 1\ndisplay_name = \"Plain Shim\"\n\n[command]\nwindows = \
         [\"shim-program\"]\n",
    )
    .expect("a valid profile");

    let cases: [(&str, Profile, DaemonConfig, ProfileUnavailableKind); 5] = [
        (
            "not found",
            fixture_profile("valid-default.toml"),
            config_with(None, empty_lookup(), host, None),
            ProfileUnavailableKind::NotFound,
        ),
        (
            "metacharacter in an argument",
            metachar_argument,
            config_with(None, lookup_in(&bin), ProfileOs::Windows, None),
            ProfileUnavailableKind::Metacharacter,
        ),
        (
            "metacharacter in the shim path",
            plain_shim,
            config_with(None, lookup_in(&meta_bin), ProfileOs::Windows, None),
            ProfileUnavailableKind::Metacharacter,
        ),
        (
            "no argv for this operating system",
            fixture_profile("valid-windows-only.toml"),
            config_with(None, empty_lookup(), ProfileOs::Linux, None),
            ProfileUnavailableKind::NoCommand,
        ),
        (
            "no default shell",
            fixture_profile("valid-shell-omitted.toml"),
            config_with(None, empty_lookup(), host, None),
            ProfileUnavailableKind::NoDefaultShell,
        ),
    ];

    for (label, profile, config, kind) in cases {
        let found: Unavailability =
            unavailability(&profile, &config).unwrap_or_else(|| panic!("{label}: unavailable"));
        assert_eq!(found.kind, kind, "{label}");
        assert_eq!(
            Some(found.reason),
            unavailable_reason(&profile, &config),
            "{label}: the kind travels with the very sentence the reason gives"
        );
        assert_eq!(
            found.guidance, None,
            "{label}: only a profile's own shim guidance is guidance"
        );
    }

    let shell_tree = TempDir::new("yagura-cli-test", "profile-reason-kind-available");
    shell_tree.make_dir("bin");
    let shell = shell_tree.write_file("bin/some-shell", "");
    assert_eq!(
        unavailability(
            &fixture_profile("valid-shell-omitted.toml"),
            &config_with(None, empty_lookup(), host, Some(shell)),
        ),
        None,
        "a launchable profile has no kind to report"
    );
}

#[test]
fn a_listing_entry_carries_the_kind_beside_its_reason() {
    let tree = TempDir::new("yagura-cli-test", "profile-list-reason-kind");
    let dir = tree.make_dir("profiles");
    copy_fixtures(&dir, &["valid-default.toml", "valid-shell-omitted.toml"]);
    let config = config_with(Some(dir), empty_lookup(), ProfileOs::host(), None);

    let listing: ProfileListing = list_profiles(&config);

    for (id, kind) in [
        ("valid-default", ProfileUnavailableKind::NotFound),
        (
            "valid-shell-omitted",
            ProfileUnavailableKind::NoDefaultShell,
        ),
    ] {
        let entry = listing
            .profiles
            .iter()
            .find(|entry| entry.id == id)
            .unwrap_or_else(|| panic!("{id} is listed"));
        assert!(!entry.available, "{id}");
        assert_eq!(entry.unavailable_kind, Some(kind), "{id}");
        assert_eq!(entry.unavailable_guidance, None, "{id}");
    }

    let tree = TempDir::new("yagura-cli-test", "profile-list-reason-kind-available");
    let dir = tree.make_dir("profiles");
    copy_fixtures(&dir, &["valid-default.toml"]);
    let bin = tree.make_dir("bin");
    write_available_program(&bin, "codex");
    let listing = list_profiles(&config_with(
        Some(dir),
        lookup_in(&bin),
        ProfileOs::host(),
        None,
    ));
    let entry = listing.profiles.first().expect("valid-default is listed");
    assert!(entry.available);
    assert_eq!(
        (&entry.unavailable_kind, &entry.unavailable_guidance),
        (&None, &None),
        "an available entry explains nothing"
    );
}

#[test]
fn enumeration_reads_the_directory_again_on_every_call() {
    let tree = TempDir::new("yagura-cli-test", "profile-list-reread");
    let dir = tree.make_dir("profiles");
    copy_fixtures(&dir, &["valid-default.toml"]);

    let config = config_with(Some(dir.clone()), empty_lookup(), ProfileOs::host(), None);

    let first: ProfileListing = list_profiles(&config);
    assert_eq!(listed_ids(&first), strings(&["valid-default"]));

    copy_fixtures(&dir, &["valid-with-env.toml"]);
    let after_add: ProfileListing = list_profiles(&config);
    assert_eq!(
        listed_ids(&after_add),
        strings(&["valid-default", "valid-with-env"]),
        "a file added between two calls appears in the second one, so an operator does \
         not restart the daemon to install a profile"
    );

    write_profile(
        &dir,
        "valid-default",
        "schema = 1\ndisplay_name = \"Renamed\"\n\n[command]\ndefault = [\"codex\"]\n",
    );
    let after_edit: ProfileListing = list_profiles(&config);
    let renamed = after_edit
        .profiles
        .iter()
        .find(|entry| entry.id == "valid-default")
        .expect("valid-default must still be listed");
    assert_eq!(
        renamed.display_name, "Renamed",
        "an edit between two calls is visible in the second one, not held from the first \
         read"
    );

    std::fs::remove_file(dir.join("valid-with-env.toml"))
        .expect("fixture copy should be removable");
    let after_remove: ProfileListing = list_profiles(&config);
    assert_eq!(
        listed_ids(&after_remove),
        strings(&["valid-default"]),
        "a removed file disappears from the next call rather than lingering in a cache"
    );
}

// --- the argv a listing surfaces as `command` -----------------------------
//
// `ProfileEntry::command` is the same unresolved argv `resolve_launch` would
// hand to spawn, applying the operating-system selection `argv_for_os` does:
// non-empty when a launch of the entry would carry one, empty exactly when an
// omitted `[command]` has no default shell resolved. A profile naming no argv
// for this operating system is not listed at all.

#[test]
fn a_listed_entrys_command_is_the_argv_selected_for_this_operating_system() {
    let tree = TempDir::new("yagura-cli-test", "profile-list-command-default");
    let dir = tree.make_dir("profiles");
    copy_fixtures(&dir, &["valid-default.toml"]);

    let config = config_with(Some(dir), empty_lookup(), ProfileOs::host(), None);
    let listing: ProfileListing = list_profiles(&config);

    let entry = listing
        .profiles
        .iter()
        .find(|entry| entry.id == "valid-default")
        .expect("valid-default must be listed");
    assert_eq!(
        entry.command,
        strings(&["codex"]),
        "the listing carries the same unresolved argv a launch of this profile would carry"
    );
}

#[test]
fn a_listed_entrys_command_prefers_the_operating_system_specific_argv_over_the_default() {
    let tree = TempDir::new("yagura-cli-test", "profile-list-command-os-override");
    let dir = tree.make_dir("profiles");
    copy_fixtures(&dir, &["valid-os-override.toml"]);

    for (host_os, expected) in [
        (ProfileOs::Windows, "windows-program"),
        (ProfileOs::MacOs, "macos-program"),
        (ProfileOs::Linux, "linux-program"),
    ] {
        let config = config_with(Some(dir.clone()), empty_lookup(), host_os, None);
        let listing: ProfileListing = list_profiles(&config);
        let entry = listing
            .profiles
            .iter()
            .find(|entry| entry.id == "valid-os-override")
            .expect("valid-os-override must be listed");
        assert_eq!(
            entry.command,
            strings(&[expected]),
            "the operating system's own key wins over `default`, matching argv_for_os \
             ({host_os:?})"
        );
    }
}

#[test]
fn a_profile_with_no_argv_for_this_operating_system_is_not_listed_and_raises_no_warning() {
    let tree = TempDir::new("yagura-cli-test", "profile-list-command-no-argv");
    let dir = tree.make_dir("profiles");
    copy_fixtures(&dir, &["valid-windows-only.toml", "valid-default.toml"]);

    let config = config_with(Some(dir), empty_lookup(), ProfileOs::Linux, None);
    let listing: ProfileListing = list_profiles(&config);

    assert_eq!(
        listed_ids(&listing),
        strings(&["valid-default"]),
        "a profile naming no argv for this operating system has nothing to launch here, so          it is left out rather than listed with an empty command"
    );
    assert!(
        listing.warnings.is_empty(),
        "leaving out a profile this operating system cannot launch is not a problem to          report: {:?}",
        listing.warnings
    );
}

#[test]
fn a_listed_entrys_command_is_the_resolved_default_shell_when_the_command_table_is_omitted() {
    let tree = TempDir::new("yagura-cli-test", "profile-list-command-shell");
    let dir = tree.make_dir("profiles");
    copy_fixtures(&dir, &["valid-shell-omitted.toml"]);
    tree.make_dir("bin");
    let shell = tree.write_file("bin/some-shell", "");

    let config = config_with(
        Some(dir),
        empty_lookup(),
        ProfileOs::host(),
        Some(shell.clone()),
    );
    let listing: ProfileListing = list_profiles(&config);

    let entry = listing
        .profiles
        .iter()
        .find(|entry| entry.id == "valid-shell-omitted")
        .expect("valid-shell-omitted must be listed");
    assert_eq!(
        entry.command,
        vec![shell.to_string_lossy().into_owned()],
        "an omitted [command] means the daemon's resolved default shell, exactly what a \
         launch of it would carry"
    );
}

#[test]
fn a_listed_entrys_command_is_empty_when_the_command_table_is_omitted_and_no_default_shell_was_resolved()
 {
    let tree = TempDir::new("yagura-cli-test", "profile-list-command-no-shell");
    let dir = tree.make_dir("profiles");
    copy_fixtures(&dir, &["valid-shell-omitted.toml"]);

    let config = config_with(Some(dir), empty_lookup(), ProfileOs::host(), None);
    let listing: ProfileListing = list_profiles(&config);

    let entry = listing
        .profiles
        .iter()
        .find(|entry| entry.id == "valid-shell-omitted")
        .expect("valid-shell-omitted must be listed");
    assert_eq!(
        entry.command,
        Vec::<String>::new(),
        "no default shell was resolved at start-up, so there is nothing to show"
    );
}

// --- turning a profile into the content of a launch -------------------------

#[test]
fn a_launch_carries_the_identifier_as_the_vendor_and_the_display_name_as_the_name() {
    let tree = TempDir::new("yagura-cli-test", "profile-launch-defaults");
    let dir = tree.make_dir("profiles");
    copy_fixtures(&dir, &["valid-default.toml"]);
    let bin = tree.make_dir("bin");
    tree.write_file("bin/codex", "");

    let config = config_with(Some(dir), lookup_in(&bin), ProfileOs::host(), None);
    let launch: ProfileLaunch = resolve_launch(
        &config,
        "valid-default",
        None,
        None,
        &LaunchOptionValues::default(),
    )
    .expect("a listed, available profile must launch");

    assert_eq!(
        launch.vendor, "valid-default",
        "the vendor badge is the profile identifier, which is why the identifier is held \
         to the label rules"
    );
    assert_eq!(
        launch.name, "Valid Default",
        "an unnamed request takes the display name, which validation has already checked \
         against the window-name rule"
    );
    assert_eq!(launch.detection, Some("generic-shell".to_string()));
    assert_eq!(launch.cwd, None);
    assert!(launch.env.is_empty());
}

#[test]
fn a_launch_carries_the_argv_as_written_rather_than_as_resolved() {
    let tree = TempDir::new("yagura-cli-test", "profile-launch-unresolved");
    let dir = tree.make_dir("profiles");
    copy_fixtures(&dir, &["valid-default.toml"]);
    let bin = tree.make_dir("bin");
    let resolved = tree.write_file("bin/codex", "");

    let config = config_with(Some(dir), lookup_in(&bin), ProfileOs::host(), None);
    let launch: ProfileLaunch = resolve_launch(
        &config,
        "valid-default",
        None,
        None,
        &LaunchOptionValues::default(),
    )
    .expect("a listed, available profile must launch");

    assert_eq!(
        launch.argv,
        strings(&["codex"]),
        "what a user wrote is what the window records and reports; substituting the \
         resolved path here would make list_windows show a path nobody typed. Resolution \
         belongs immediately before the spawn"
    );
    assert!(
        !launch
            .argv
            .contains(&resolved.to_string_lossy().into_owned()),
        "the resolved path must not appear in the launch argv"
    );
}

#[test]
fn a_launch_prefers_the_requested_working_directory_and_name_over_the_profile_defaults() {
    let tree = TempDir::new("yagura-cli-test", "profile-launch-overrides");
    let dir = tree.make_dir("profiles");
    let profile_default = tree.make_dir("profile-cwd");
    let requested = tree.make_dir("requested-cwd");
    write_absolute_cwd_profile(&dir, "absolute-cwd", &profile_default);

    let config = config_with(Some(dir), empty_lookup(), ProfileOs::host(), None);

    let without_request: ProfileLaunch = resolve_launch(
        &config,
        "absolute-cwd",
        None,
        None,
        &LaunchOptionValues::default(),
    )
    .expect("a profile with its own working directory must launch");
    assert_eq!(
        without_request.cwd.as_ref().map(RequestedCwd::path),
        Some(profile_default.as_path()),
        "with nothing requested, the profile's own working directory is used"
    );
    assert_eq!(without_request.name, "Absolute Cwd");

    let with_request: ProfileLaunch = resolve_launch(
        &config,
        "absolute-cwd",
        Some(requested.clone()),
        Some("Chosen Name".to_string()),
        &LaunchOptionValues::default(),
    )
    .expect("a profile with its own working directory must launch");
    assert_eq!(
        with_request.cwd.as_ref().map(RequestedCwd::path),
        Some(requested.as_path()),
        "what the request carries wins over the profile's default"
    );
    assert_eq!(with_request.name, "Chosen Name");
}

#[test]
fn a_launch_without_a_command_table_uses_the_default_shell() {
    let tree = TempDir::new("yagura-cli-test", "profile-launch-shell");
    let dir = tree.make_dir("profiles");
    copy_fixtures(&dir, &["valid-shell-omitted.toml"]);
    tree.make_dir("bin");
    let shell = tree.write_file("bin/some-shell", "");

    let config = config_with(
        Some(dir),
        empty_lookup(),
        ProfileOs::host(),
        Some(shell.clone()),
    );
    let launch: ProfileLaunch = resolve_launch(
        &config,
        "valid-shell-omitted",
        None,
        None,
        &LaunchOptionValues::default(),
    )
    .expect("an omitted command table launches the resolved default shell");

    assert_eq!(
        launch.argv,
        vec![shell.to_string_lossy().into_owned()],
        "the shell resolved once at start-up is what an omitted [command] means; the \
         profile path must not resolve a shell of its own from the environment"
    );
}

#[test]
fn a_launch_carries_the_profile_environment_without_the_reserved_keys() {
    let tree = TempDir::new("yagura-cli-test", "profile-launch-env");
    let dir = tree.make_dir("profiles");
    copy_fixtures(&dir, &["valid-with-env.toml", "reserved-env.toml"]);

    let config = config_with(Some(dir), empty_lookup(), ProfileOs::host(), None);

    let with_env: ProfileLaunch = resolve_launch(
        &config,
        "valid-with-env",
        None,
        None,
        &LaunchOptionValues::default(),
    )
    .expect("valid-with-env must launch");
    assert_eq!(
        with_env.env.get("EMPTY"),
        Some(&String::new()),
        "an empty value reaches the launch as an empty value"
    );
    assert_eq!(
        with_env.env.get("NOT_EXPANDED"),
        Some(&"$HOME %USERPROFILE% ~".to_string())
    );

    let reserved: ProfileLaunch = resolve_launch(
        &config,
        "reserved-env",
        None,
        None,
        &LaunchOptionValues::default(),
    )
    .expect("a dropped reserved key does not make the profile unusable");
    assert_eq!(reserved.env.get("YAGURA_WINDOW_ID"), None);
    assert_eq!(
        reserved.env.get("KEPT"),
        Some(&"kept value".to_string()),
        "the surviving keys still reach the launch"
    );
}

#[test]
fn a_launch_of_an_identifier_that_is_not_listed_is_refused() {
    let tree = TempDir::new("yagura-cli-test", "profile-launch-unknown");
    let dir = tree.make_dir("profiles");
    copy_fixtures(&dir, &["valid-default.toml", "invalid-schema.toml"]);
    copy_fixtures(&dir, &["Bad Id.toml"]);

    let config = config_with(Some(dir), empty_lookup(), ProfileOs::host(), None);

    for id in ["no-such-profile", "invalid-schema", "Bad Id"] {
        let result = resolve_launch(&config, id, None, None, &LaunchOptionValues::default());
        assert!(
            result.is_err(),
            "only what enumeration accepted can be launched, so {id:?} must be refused \
             rather than read straight off disk, got {result:?}"
        );
    }
}

#[test]
fn a_launch_never_builds_a_path_out_of_the_identifier() {
    let tree = TempDir::new("yagura-cli-test", "profile-launch-traversal");
    let outside = tree.make_dir("outside");
    copy_fixtures(&outside, &["valid-default.toml"]);
    let dir = tree.make_dir("profiles");
    copy_fixtures(&dir, &["valid-with-env.toml"]);

    let config = config_with(Some(dir), empty_lookup(), ProfileOs::host(), None);

    for id in [
        "../outside/valid-default",
        "..\\outside\\valid-default",
        "../outside/valid-default.toml",
    ] {
        let result = resolve_launch(&config, id, None, None, &LaunchOptionValues::default());
        assert!(
            result.is_err(),
            "the identifier is matched against what was enumerated, never joined onto the \
             directory, so {id:?} cannot reach a file outside it, got {result:?}"
        );
    }
}

#[test]
fn a_launch_reads_the_profile_again_on_every_call() {
    let tree = TempDir::new("yagura-cli-test", "profile-launch-reread");
    let dir = tree.make_dir("profiles");
    copy_fixtures(&dir, &["valid-default.toml"]);

    let config = config_with(Some(dir.clone()), empty_lookup(), ProfileOs::host(), None);

    let before: ProfileLaunch = resolve_launch(
        &config,
        "valid-default",
        None,
        None,
        &LaunchOptionValues::default(),
    )
    .expect("valid-default must launch");
    assert_eq!(before.name, "Valid Default");
    assert_eq!(before.argv, strings(&["codex"]));

    write_profile(
        &dir,
        "valid-default",
        "schema = 1\ndisplay_name = \"Edited\"\n\n[command]\ndefault = [\"edited-program\"]\n",
    );

    let after: ProfileLaunch = resolve_launch(
        &config,
        "valid-default",
        None,
        None,
        &LaunchOptionValues::default(),
    )
    .expect("valid-default must still launch");
    assert_eq!(
        after.name, "Edited",
        "an edit takes effect on the next launch request; only the resolution environment \
         and the default shell are fixed at start-up"
    );
    assert_eq!(after.argv, strings(&["edited-program"]));
}

// --- launch options: what a profile maps -------------------------------------

/// The alias table a parsed axis carries, as pairs, so a case can compare it
/// against a plain vector without restating the tuple type at every call.
fn alias_pairs(spec: &LaunchOptionSpec) -> Vec<(String, String)> {
    spec.aliases.clone()
}

fn pairs(items: &[(&str, &str)]) -> Vec<(String, String)> {
    items
        .iter()
        .map(|(alias, value)| ((*alias).to_string(), (*value).to_string()))
        .collect()
}

fn spec(args: &[&str], aliases: &[(&str, &str)]) -> LaunchOptionSpec {
    LaunchOptionSpec {
        args: strings(args),
        aliases: pairs(aliases),
    }
}

fn values(model: Option<&str>, effort: Option<&str>) -> LaunchOptionValues {
    LaunchOptionValues {
        model: model.map(str::to_string),
        effort: effort.map(str::to_string),
    }
}

#[test]
fn a_profile_that_maps_both_launch_options_parses_into_both_axes() {
    let profile = fixture_profile("valid-launch-options.toml");

    let model = profile
        .options
        .model
        .as_ref()
        .expect("the document maps the model axis");
    assert_eq!(model.args, strings(&["--tier", "{value}"]));
    assert_eq!(
        alias_pairs(model),
        pairs(&[("gamma", "tier-c"), ("alpha", "tier-a"), ("beta", "tier-b")]),
        "the alias table keeps the order its author wrote, which is why the \
         fixture spells it in an order sorting would visibly change"
    );

    let effort = profile
        .options
        .effort
        .as_ref()
        .expect("the document maps the effort axis");
    assert_eq!(effort.args, strings(&["-c", "tier={value}"]));
    assert!(
        effort.aliases.is_empty(),
        "an axis whose table declares no alias maps every value straight \
         through, got {:?}",
        effort.aliases
    );
}

#[test]
fn a_profile_that_maps_one_launch_option_leaves_the_other_axis_unmapped() {
    let profile = fixture_profile("valid-launch-option-single-axis.toml");

    assert!(profile.options.model.is_some());
    assert!(
        profile.options.effort.is_none(),
        "an absent table is the profile saying it accepts nothing for that \
         axis, which is what makes a value supplied for it a refusal rather \
         than a value quietly dropped"
    );
    assert!(
        profile.options.get(LaunchOptionAxis::Effort).is_none(),
        "the axis accessor must agree with the field it reads"
    );
}

#[test]
fn a_mapped_axis_without_an_alias_table_parses_with_an_empty_alias_list() {
    let profile = fixture_profile("valid-launch-option-no-aliases.toml");

    let model = profile
        .options
        .model
        .as_ref()
        .expect("the document maps the model axis");
    assert_eq!(model.args, strings(&["--tier", "{value}"]));
    assert!(model.aliases.is_empty());
    assert!(profile.options.effort.is_none());
}

#[test]
fn a_profile_with_no_launch_option_table_maps_neither_axis() {
    let profile = fixture_profile("valid-default.toml");

    for axis in LaunchOptionAxis::ALL {
        assert!(
            profile.options.get(axis).is_none(),
            "a document written before launch options existed maps none of \
             them, so every such profile keeps behaving as it did: {} was \
             mapped",
            axis.name()
        );
    }
}

// --- launch options: the documents that are refused --------------------------

/// The fixtures a launch-option rule excludes, each paired with nothing but
/// its file name: the property under test is the variant and the fixed
/// reason, never the document's own text.
const LAUNCH_OPTION_REJECTED_BY_THE_OPTION_RULE: [&str; 4] = [
    "invalid-launch-option-no-args.toml",
    "invalid-launch-option-no-placeholder.toml",
    "invalid-launch-option-alias-key.toml",
    "invalid-launch-option-alias-value.toml",
];

#[test]
fn every_document_breaking_a_launch_option_rule_is_excluded_under_one_variant() {
    for file_name in LAUNCH_OPTION_REJECTED_BY_THE_OPTION_RULE {
        let id = fixture_id(file_name);
        let err = parse_profile(&id, &fixture_source(file_name))
            .expect_err(&format!("fixture {file_name} must be rejected"));
        assert!(
            matches!(err, ProfileParseError::InvalidLaunchOption(_)),
            "a launch-option rule breaks the document under one variant, so a \
             test never has to reach into what the variant carries: {file_name} \
             gave {err:?}"
        );
        assert_eq!(
            err.to_string(),
            "invalid launch option",
            "the reason a client is shown is a fixed phrase; the detail stays \
             on the source chain the daemon's log reads"
        );
    }
}

#[test]
fn an_alias_value_outside_the_argv_token_set_is_refused_even_off_the_fixture_set() {
    for value in ["tier a", "$x"] {
        let source = format!(
            "schema = 1\ndisplay_name = \"Alias Value\"\n\n[command]\ndefault = \
             [\"alias-value-program\"]\n\n[model]\nargs = [\"--tier\", \"{{value}}\"]\n\n\
             [model.aliases]\nalpha = \"{value}\"\n"
        );
        let err = parse_profile("alias-value-outside-rule", &source)
            .expect_err(&format!("an alias value of {value:?} must be rejected"));
        assert!(
            matches!(err, ProfileParseError::InvalidLaunchOption(_)),
            "an alias value outside the launch option value rule is excluded under \
             the same variant as the fixture set: {value:?} gave {err:?}"
        );
    }
}

#[test]
fn an_empty_element_in_a_launch_options_args_is_refused_by_the_argv_rule() {
    let file_name = "invalid-launch-option-empty-arg.toml";
    let err = parse_profile(&fixture_id(file_name), &fixture_source(file_name))
        .expect_err("an empty argv element must be rejected");
    assert!(
        matches!(err, ProfileParseError::InvalidArgvElement(_)),
        "the elements a launch option appends are argv elements, so they are \
         held to the argv authoring rule that already exists rather than to a \
         second one written for this table: got {err:?}"
    );
}

#[test]
fn an_unknown_key_in_a_launch_option_table_is_refused_while_the_document_is_read() {
    let file_name = "invalid-launch-option-unknown-key.toml";
    let err = parse_profile(&fixture_id(file_name), &fixture_source(file_name))
        .expect_err("an unknown key must be rejected");
    assert!(
        matches!(err, ProfileParseError::Toml(_)),
        "the table refuses a key it does not know while it is being decoded, \
         the same side of the split the detection definitions put it on: got \
         {err:?}"
    );
    assert_eq!(err.to_string(), "could not be parsed");
}

#[test]
fn a_launch_option_warning_never_carries_a_value_read_from_the_document() {
    let tree = TempDir::new("yagura-cli-test", "profile-list-launch-option-excluded");
    let dir = tree.make_dir("profiles");
    let excluded = [
        "invalid-launch-option-no-args.toml",
        "invalid-launch-option-no-placeholder.toml",
        "invalid-launch-option-alias-key.toml",
        "invalid-launch-option-alias-value.toml",
        "invalid-launch-option-empty-arg.toml",
        "invalid-launch-option-unknown-key.toml",
    ];
    copy_fixtures(&dir, &excluded);

    let config = config_with(Some(dir), empty_lookup(), ProfileOs::host(), None);
    let listing: ProfileListing = list_profiles(&config);

    assert!(
        listing.profiles.is_empty(),
        "every one of these documents is excluded: {:?}",
        listing.profiles
    );
    assert_eq!(listing.warnings.len(), excluded.len());
    for file_name in LAUNCH_OPTION_REJECTED_BY_THE_OPTION_RULE {
        let expected = format!("profile file {file_name:?} invalid launch option");
        assert!(
            listing.warnings.contains(&expected),
            "the warning must read exactly {expected:?}, got {:?}",
            listing.warnings
        );
    }
    for leaked in ["tier-a", "tier-c", "Alpha", "whatever", "--tier"] {
        assert!(
            !listing.warnings.iter().any(|w| w.contains(leaked)),
            "a warning reaches clients, so nothing read out of the document may \
             appear in it; {leaked:?} did: {:?}",
            listing.warnings
        );
    }
}

// --- launch options: the pure resolution functions ---------------------------

#[test]
fn resolve_option_value_maps_a_listed_alias_and_passes_anything_else_through() {
    let mapped = spec(&["--tier", "{value}"], &[("alpha", "tier-a")]);
    assert_eq!(resolve_option_value(&mapped, "alpha"), "tier-a");
    assert_eq!(
        resolve_option_value(&mapped, "tier-c"),
        "tier-c",
        "a value the table does not list is the caller naming something the \
         command itself understands, so it is carried through untouched"
    );
    assert_eq!(
        resolve_option_value(&mapped, "Alpha"),
        "Alpha",
        "the alias table is matched exactly, never case-folded"
    );
}

#[test]
fn resolve_option_value_resolves_an_alias_exactly_once() {
    // `alpha` maps to `beta`, and `beta` is itself an alias. One step only:
    // an alias table that chained would let a profile's author build a loop.
    let chained = spec(
        &["--tier", "{value}"],
        &[("alpha", "beta"), ("beta", "tier-b")],
    );
    assert_eq!(resolve_option_value(&chained, "alpha"), "beta");
}

#[test]
fn apply_launch_options_appends_nothing_when_the_caller_supplied_nothing() {
    let specs = LaunchOptions {
        model: Some(spec(&["--tier", "{value}"], &[("alpha", "tier-a")])),
        effort: Some(spec(&["-c", "tier={value}"], &[])),
        model_choices: None,
    };
    let mut argv = strings(&["some-program", "--flag"]);
    apply_launch_options(&mut argv, &specs, &LaunchOptionValues::default())
        .expect("supplying no value is not an error");
    assert_eq!(
        argv,
        strings(&["some-program", "--flag"]),
        "an axis nobody named is left to the command's own default rather than \
         being given one here"
    );
}

#[test]
fn apply_launch_options_appends_the_model_elements_ahead_of_the_effort_ones() {
    let specs = LaunchOptions {
        model: Some(spec(&["--tier", "{value}"], &[("alpha", "tier-a")])),
        effort: Some(spec(&["-c", "tier={value}"], &[])),
        model_choices: None,
    };
    let mut argv = strings(&["some-program"]);
    apply_launch_options(&mut argv, &specs, &values(Some("alpha"), Some("tier-b")))
        .expect("both axes are mapped");
    assert_eq!(
        argv,
        strings(&["some-program", "--tier", "tier-a", "-c", "tier=tier-b"]),
        "the order is fixed so a restored window replays the argv it was opened \
         with; what the profile already carried stays in front"
    );
}

#[test]
fn apply_launch_options_puts_a_value_spelled_inside_an_element_into_that_one_element() {
    let specs = LaunchOptions {
        model: None,
        effort: Some(spec(&["-c", "tier={value}"], &[])),
        model_choices: None,
    };
    let mut argv = strings(&["some-program"]);
    apply_launch_options(&mut argv, &specs, &values(None, Some("tier-b")))
        .expect("the effort axis is mapped");
    assert_eq!(
        argv,
        strings(&["some-program", "-c", "tier=tier-b"]),
        "substitution happens inside an element, so a value can never split \
         one element into two or add an element of its own"
    );
}

#[test]
fn apply_launch_options_replaces_the_placeholder_everywhere_it_appears() {
    let specs = LaunchOptions {
        model: Some(spec(&["--tier", "{value}", "tier={value}/{value}"], &[])),
        effort: None,
        model_choices: None,
    };
    let mut argv = strings(&["some-program"]);
    apply_launch_options(&mut argv, &specs, &values(Some("tier-a"), None))
        .expect("the model axis is mapped");
    assert_eq!(
        argv,
        strings(&["some-program", "--tier", "tier-a", "tier=tier-a/tier-a"]),
        "an author who spells the placeholder more than once means it every time"
    );
}

#[test]
fn apply_launch_options_leaves_the_argv_untouched_when_an_axis_is_not_mapped() {
    let specs = LaunchOptions {
        model: Some(spec(&["--tier", "{value}"], &[("alpha", "tier-a")])),
        effort: None,
        model_choices: None,
    };
    let before = strings(&["some-program"]);
    let mut argv = before.clone();
    let err = apply_launch_options(&mut argv, &specs, &values(Some("alpha"), Some("tier-b")))
        .expect_err("the effort axis is not mapped here");
    assert!(matches!(
        err,
        ProfileLaunchError::UnsupportedLaunchOption(LaunchOptionAxis::Effort)
    ));
    assert_eq!(
        argv, before,
        "every axis is checked before any element is appended, so a refusal \
         never leaves a half-built argv behind for a caller that ignores the \
         error"
    );
}

#[test]
fn apply_launch_options_names_the_first_unmapped_axis_in_the_fixed_axis_order() {
    let specs = LaunchOptions {
        model: None,
        effort: None,
        model_choices: None,
    };
    let mut argv = strings(&["some-program"]);
    let err = apply_launch_options(&mut argv, &specs, &values(Some("alpha"), Some("tier-b")))
        .expect_err("neither axis is mapped");
    assert!(matches!(
        err,
        ProfileLaunchError::UnsupportedLaunchOption(LaunchOptionAxis::Model)
    ));
}

#[test]
fn the_unsupported_launch_option_error_names_the_axis_it_refused() {
    assert_eq!(
        ProfileLaunchError::UnsupportedLaunchOption(LaunchOptionAxis::Effort).to_string(),
        "this profile does not map the effort option"
    );
    assert_eq!(
        ProfileLaunchError::UnsupportedLaunchOption(LaunchOptionAxis::Model).to_string(),
        "this profile does not map the model option"
    );
}

#[test]
fn the_axis_names_are_the_ones_a_caller_spells_on_the_wire() {
    assert_eq!(LaunchOptionAxis::Model.name(), "model");
    assert_eq!(LaunchOptionAxis::Effort.name(), "effort");
    assert_eq!(
        LaunchOptionAxis::ALL,
        [LaunchOptionAxis::Model, LaunchOptionAxis::Effort],
        "the order the axes are processed in is the order their elements are \
         appended in, so it is fixed here rather than left to a reader"
    );
}

#[test]
fn supplied_values_report_whether_the_caller_named_anything_at_all() {
    assert!(LaunchOptionValues::default().is_empty());
    assert!(!values(Some("alpha"), None).is_empty());
    assert!(!values(None, Some("tier-b")).is_empty());
    assert_eq!(
        values(Some("alpha"), Some("tier-b")).get(LaunchOptionAxis::Model),
        Some("alpha")
    );
    assert_eq!(
        values(Some("alpha"), Some("tier-b")).get(LaunchOptionAxis::Effort),
        Some("tier-b")
    );
    assert_eq!(
        LaunchOptionValues::default().get(LaunchOptionAxis::Model),
        None
    );
}

// --- launch options: turning a profile into the content of a launch ----------

fn launch_option_config(tree: &TempDir, dir_name: &str, file_names: &[&str]) -> DaemonConfig {
    let dir = tree.make_dir(dir_name);
    copy_fixtures(&dir, file_names);
    config_with(Some(dir), empty_lookup(), ProfileOs::host(), None)
}

#[test]
fn a_launch_appends_what_the_supplied_launch_options_map_to() {
    let tree = TempDir::new("yagura-cli-test", "profile-launch-options");
    let config = launch_option_config(&tree, "profiles", &["valid-launch-options.toml"]);

    let launch: ProfileLaunch = resolve_launch(
        &config,
        "valid-launch-options",
        None,
        None,
        &values(Some("alpha"), Some("tier-b")),
    )
    .expect("both axes are mapped by this profile");

    assert_eq!(
        launch.argv,
        strings(&[
            "launch-options-program",
            "--tier",
            "tier-a",
            "-c",
            "tier=tier-b",
        ]),
        "the alias is resolved and the elements are appended in axis order, \
         behind the argv the profile already named"
    );
}

#[test]
fn a_launch_passes_a_value_the_alias_table_does_not_list_straight_through() {
    let tree = TempDir::new("yagura-cli-test", "profile-launch-options-passthrough");
    let config = launch_option_config(&tree, "profiles", &["valid-launch-options.toml"]);

    let launch: ProfileLaunch = resolve_launch(
        &config,
        "valid-launch-options",
        None,
        None,
        &values(Some("something-the-table-never-lists"), None),
    )
    .expect("an unlisted value is a value, not a rejection");

    assert_eq!(
        launch.argv,
        strings(&[
            "launch-options-program",
            "--tier",
            "something-the-table-never-lists",
        ]),
        "the daemon does not judge whether a command will accept the value; \
         the alias table is a convenience, not an allow list"
    );
}

#[test]
fn a_launch_with_no_supplied_launch_option_carries_the_argv_it_always_did() {
    let tree = TempDir::new("yagura-cli-test", "profile-launch-options-absent");
    let config = launch_option_config(&tree, "profiles", &["valid-launch-options.toml"]);

    let launch: ProfileLaunch = resolve_launch(
        &config,
        "valid-launch-options",
        None,
        None,
        &LaunchOptionValues::default(),
    )
    .expect("supplying nothing is the ordinary case");

    assert_eq!(
        launch.argv,
        strings(&["launch-options-program"]),
        "a profile that maps launch options still launches exactly as it did \
         before when nobody supplies one"
    );
}

#[test]
fn a_launch_supplying_an_axis_the_profile_does_not_map_is_refused_with_no_launch_content() {
    let tree = TempDir::new("yagura-cli-test", "profile-launch-options-unsupported");
    let config = launch_option_config(&tree, "profiles", &["valid-launch-option-single-axis.toml"]);

    let err = resolve_launch(
        &config,
        "valid-launch-option-single-axis",
        None,
        None,
        &values(Some("alpha"), Some("tier-b")),
    )
    .expect_err("this profile maps no effort axis");
    assert!(matches!(
        err,
        ProfileLaunchError::UnsupportedLaunchOption(LaunchOptionAxis::Effort)
    ));
    assert_eq!(
        err.to_string(),
        "this profile does not map the effort option",
        "the refusal reaches a caller as this sentence, so it is fixed here"
    );

    let accepted: ProfileLaunch = resolve_launch(
        &config,
        "valid-launch-option-single-axis",
        None,
        None,
        &values(Some("alpha"), None),
    )
    .expect("the axis this profile does map is accepted");
    assert_eq!(
        accepted.argv,
        strings(&["single-axis-program", "--tier", "tier-a"]),
        "refusing one axis must not make the other unusable"
    );
}

// --- launch options: what enumeration reports --------------------------------

fn info(aliases: &[(&str, &str)]) -> LaunchOptionInfo {
    LaunchOptionInfo {
        aliases: aliases
            .iter()
            .map(|(alias, value)| LaunchOptionAlias {
                alias: (*alias).to_string(),
                value: (*value).to_string(),
            })
            .collect(),
    }
}

#[test]
fn enumeration_reports_each_mapped_axis_with_its_aliases_in_the_order_they_were_written() {
    let tree = TempDir::new("yagura-cli-test", "profile-list-launch-options");
    let dir = tree.make_dir("profiles");
    copy_fixtures(
        &dir,
        &[
            "valid-launch-options.toml",
            "valid-launch-option-single-axis.toml",
            "valid-default.toml",
        ],
    );

    let config = config_with(Some(dir), empty_lookup(), ProfileOs::host(), None);
    let listing: ProfileListing = list_profiles(&config);
    assert!(listing.warnings.is_empty(), "{:?}", listing.warnings);

    let both = listing
        .profiles
        .iter()
        .find(|entry| entry.id == "valid-launch-options")
        .expect("the profile mapping both axes is listed");
    assert_eq!(
        both.model,
        Some(info(&[
            ("gamma", "tier-c"),
            ("alpha", "tier-a"),
            ("beta", "tier-b"),
        ])),
        "the listing is what a dialog builds its choices from, so the author's \
         order is what it must show"
    );
    assert_eq!(
        both.effort,
        Some(info(&[])),
        "an axis mapped without aliases is reported as accepting anything, \
         which is a different answer from not accepting the axis at all"
    );
    assert_eq!(
        both.command,
        strings(&["launch-options-program"]),
        "a listing knows no value, so the argv it reports is still the one a \
         launch supplying nothing would carry"
    );

    let single = listing
        .profiles
        .iter()
        .find(|entry| entry.id == "valid-launch-option-single-axis")
        .expect("the single-axis profile is listed");
    assert_eq!(single.model, Some(info(&[("alpha", "tier-a")])));
    assert_eq!(single.effort, None);

    let neither = listing
        .profiles
        .iter()
        .find(|entry| entry.id == "valid-default")
        .expect("the profile mapping nothing is listed");
    assert_eq!(neither.model, None);
    assert_eq!(neither.effort, None);
}

/// Every fixture that existed before launch options did, so the set a
/// listing accepts and the set it excludes can be asserted whole.
const FIXTURES_PREDATING_LAUNCH_OPTIONS: [&str; 14] = [
    "Bad Id.toml",
    "invalid-display-name.toml",
    "invalid-empty-arg.toml",
    "invalid-no-display-name.toml",
    "invalid-schema.toml",
    "invalid-syntax-with-secret.toml",
    "relative-cwd.toml",
    "reserved-env.toml",
    "tilde-cwd.toml",
    "valid-default.toml",
    "valid-os-override.toml",
    "valid-shell-omitted.toml",
    "valid-windows-only.toml",
    "valid-with-env.toml",
];

#[test]
fn enumeration_of_every_document_predating_launch_options_is_unchanged_and_maps_no_axis() {
    let tree = TempDir::new(
        "yagura-cli-test",
        "profile-list-launch-options-compatibility",
    );
    let dir = tree.make_dir("profiles");
    copy_fixtures(&dir, &FIXTURES_PREDATING_LAUNCH_OPTIONS);

    // Fixed to Windows: `valid-windows-only` names an argv for Windows alone.
    let config = config_with(Some(dir), empty_lookup(), ProfileOs::Windows, None);
    let listing: ProfileListing = list_profiles(&config);

    assert_eq!(
        listed_ids(&listing),
        strings(&[
            "relative-cwd",
            "reserved-env",
            "tilde-cwd",
            "valid-default",
            "valid-os-override",
            "valid-shell-omitted",
            "valid-windows-only",
            "valid-with-env",
        ]),
        "adding a launch-option table to the schema must move no existing \
         document from one side of the accepted/excluded split to the other"
    );
    assert_eq!(
        listing.warnings.len(),
        6,
        "the same six documents are excluded as before, one warning each: {:?}",
        listing.warnings
    );
    for entry in &listing.profiles {
        assert_eq!(
            (entry.model.as_ref(), entry.effort.as_ref()),
            (None, None),
            "a document that names no launch-option table accepts neither \
             axis; {} reported one",
            entry.id
        );
    }
}
