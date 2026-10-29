#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// `profile::list_profiles` is called directly here: it is the unit under test.
#![allow(clippy::disallowed_methods)]

//! Integration test: a profile's `[credentials]` table (how a window's
//! credentials reach its CLI) and the `{credentials_file}` placeholder that
//! only a `launch` profile may spell. Profiles are read through
//! `parse_profile`, `list_profiles` and `resolve_launch`.

use std::path::{Path, PathBuf};

use yagura_daemon::DaemonConfig;
use yagura_daemon::profile::{
    LaunchOptionValues, ProfileOs, list_profiles, parse_profile, resolve_launch,
};
use yagura_os::program::ProgramLookup;
use yagura_test_support::paths::TempDir;

const HEAD: &str =
    "schema = 1\ndisplay_name = \"Cred\"\n\n[command]\ndefault = [\"cred-program\"]\n";

fn file_exists(path: &Path) -> bool {
    path.is_file()
}

fn config_in(profiles_dir: PathBuf, bin: &Path) -> DaemonConfig {
    DaemonConfig {
        profiles_dir: Some(profiles_dir),
        host_os: ProfileOs::host(),
        program_lookup: ProgramLookup {
            path_dirs: vec![bin.to_path_buf()],
            extra_bin_dirs: Vec::new(),
            windows_comspec: Some(PathBuf::from("cmd.exe")),
        },
        program_is_executable: file_exists,
        default_shell: None,
        detection_dir: None,
        ..DaemonConfig::default()
    }
}

/// Every table and key a `{credentials_file}` may be written into.
const PLACEMENTS: &[(&str, &str)] = &[
    (
        "hooks args",
        "[hooks]\nargs = [\"--c\", \"{credentials_file}\"]\n",
    ),
    (
        "hooks config_file",
        "[hooks]\nconfig_file = \"x {credentials_file}\"\nargs = [\"{hooks_config_file}\"]\n",
    ),
    (
        "hooks config_dir",
        "[hooks]\nargs = [\"{hooks_config_dir}\"]\n[hooks.config_dir]\n\"a.json\" = \
         \"x {credentials_file}\"\n",
    ),
    (
        "mcp args",
        "[mcp]\nargs = [\"--c\", \"{credentials_file}\"]\n",
    ),
    (
        "mcp config_file",
        "[mcp]\nconfig_file = \"x {credentials_file}\"\nargs = [\"{mcp_config_file}\"]\n",
    ),
    (
        "mcp config_dir",
        "[mcp]\nargs = [\"{mcp_config_dir}\"]\n[mcp.config_dir]\n\"a.json\" = \
         \"x {credentials_file}\"\n",
    ),
];

#[test]
fn credentials_mode_defaults_to_env_and_accepts_only_env_or_launch() {
    for table in ["", "[credentials]\n", "[credentials]\nmode = \"env\"\n"] {
        parse_profile("cred-env", &format!("{HEAD}\n{table}"))
            .unwrap_or_else(|err| panic!("{table:?} must read as an env profile: {err}"));
    }
    parse_profile(
        "cred-launch",
        &format!("{HEAD}\n[credentials]\nmode = \"launch\"\n"),
    )
    .expect("mode = \"launch\" must be accepted");
}

#[test]
fn a_bad_mode_or_an_unknown_key_leaves_the_profile_out_with_a_warning() {
    let tree = TempDir::new("yagura-cli-test", "profile-cred-excluded");
    let dir = tree.make_dir("profiles");
    let bin = tree.make_dir("bin");
    tree.write_file("bin/cred-program", "");
    tree.write_file("bin/cred-program.exe", "");
    let cases = [
        ("cred-ok", "[credentials]\nmode = \"launch\"\n"),
        ("cred-bad-mode", "[credentials]\nmode = \"both\"\n"),
        ("cred-mode-number", "[credentials]\nmode = 1\n"),
        (
            "cred-unknown-key",
            "[credentials]\nmode = \"env\"\nextra = 1\n",
        ),
    ];
    for (id, table) in cases {
        std::fs::write(dir.join(format!("{id}.toml")), format!("{HEAD}\n{table}")).unwrap();
    }

    let listing = list_profiles(&config_in(dir, &bin));

    let listed: Vec<_> = listing.profiles.iter().map(|p| p.id.as_str()).collect();
    assert_eq!(listed, ["cred-ok"], "warnings: {:?}", listing.warnings);
    for (id, _) in &cases[1..] {
        let naming: Vec<_> = listing.warnings.iter().filter(|w| w.contains(id)).collect();
        assert_eq!(
            naming.len(),
            1,
            "{id} needs exactly one warning: {:?}",
            listing.warnings
        );
        assert!(
            naming[0].to_lowercase().contains("credentials"),
            "the warning must name the credentials table: {naming:?}"
        );
    }
}

#[test]
fn credentials_file_is_accepted_only_where_the_profile_is_launch() {
    for (place, table) in PLACEMENTS {
        let launch = format!("{HEAD}\n[credentials]\nmode = \"launch\"\n\n{table}");
        parse_profile("cred-launch", &launch)
            .unwrap_or_else(|err| panic!("launch must accept it in {place}: {err}"));
        for mode in ["", "[credentials]\nmode = \"env\"\n\n"] {
            let env = format!("{HEAD}\n{mode}{table}");
            assert!(
                parse_profile("cred-env", &env).is_err(),
                "an env profile must be refused when it writes it in {place}"
            );
        }
    }
    let unknown = format!(
        "{HEAD}\n[credentials]\nmode = \"launch\"\n\n[hooks]\nargs = [\"{{credential_file}}\"]\n"
    );
    assert!(
        parse_profile("cred-typo", &unknown).is_err(),
        "any other unknown placeholder stays refused in a launch profile"
    );
}

#[test]
fn launch_args_are_accepted_as_argv_elements_and_kept_out_of_the_window_argv() {
    let ok =
        format!("{HEAD}\n[credentials]\nmode = \"env\"\nlaunch_args = [\"-c\", \"k=\\\"\\\"\"]\n");
    parse_profile("cred-args", &ok).expect("launch_args as an array of elements is accepted");
    for (what, value) in [
        ("an empty element", "[\"-c\", \"\"]"),
        ("a NUL element", "[\"-c\", \"a\\u0000b\"]"),
    ] {
        let bad = format!("{HEAD}\n[credentials]\nlaunch_args = {value}\n");
        assert!(
            parse_profile("cred-args", &bad).is_err(),
            "{what} must be refused"
        );
    }

    let tree = TempDir::new("yagura-cli-test", "profile-cred-launch-args");
    let dir = tree.make_dir("profiles");
    let bin = tree.make_dir("bin");
    tree.write_file("bin/cred-program", "");
    tree.write_file("bin/cred-program.exe", "");
    std::fs::write(dir.join("cred-args.toml"), ok).unwrap();
    let launch = resolve_launch(
        &config_in(dir, &bin),
        "cred-args",
        None,
        None,
        &LaunchOptionValues::default(),
    )
    .expect("a profile with launch_args must launch");
    assert_eq!(
        launch.argv,
        ["cred-program"],
        "launch_args belong to the spawn only, never to the recorded command"
    );
}
