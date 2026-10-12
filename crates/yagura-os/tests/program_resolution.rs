#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: `yagura_os::program`'s executable-resolution contract.
//! Two rules shape most of what follows:
//! `into_spawn_argv` absorbs the cmd.exe `/C` quote-stripping pitfall itself,
//! and Windows extension probing applies only to extensionless `argv[0]`
//! names.
//!
//! The Windows and Unix resolution rules are exercised in this single test
//! binary by *value* (`Platform::Windows` / `Platform::Unix`), not by
//! `#[cfg(windows)]` / `#[cfg(not(windows))]`: every test whose name starts
//! with `windows_` or `unix_` runs on every host OS. Only two tests read a
//! real environment or a real file's permission bits, and only those two
//! carry a `#[cfg]`.
//!
//! The Windows tests below stop at "the resolved spawn argv's elements are
//! what is expected": whether a real cmd.exe delivers a whitespace-containing
//! argument verbatim to the child is exercised daemon-side
//! (`ipc_profile.rs`), not by this crate.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use yagura_os::program::{
    Platform, ProgramLookup, Rejected, Resolution, into_spawn_argv, is_available, resolve_program,
    windows_comspec_from, windows_extra_bin_dirs_from,
};

/// cmd.exe metacharacters a `CmdShim` resolution must reject in the shim path
/// or in any trailing argument (the same set as `program.rs`'s own).
const CMD_METACHARACTERS: [char; 8] = ['%', '&', '|', '<', '>', '^', '"', '\n'];

fn lookup_with(path_dirs: Vec<PathBuf>, extra_bin_dirs: Vec<PathBuf>) -> ProgramLookup {
    ProgramLookup {
        path_dirs,
        extra_bin_dirs,
        windows_comspec: Some(PathBuf::from("comspec-root").join("cmd.exe")),
    }
}

fn predicate_over(known: Vec<PathBuf>) -> impl Fn(&Path) -> bool {
    move |candidate: &Path| known.iter().any(|p| p == candidate)
}

fn always_true(_: &Path) -> bool {
    true
}

fn always_false(_: &Path) -> bool {
    false
}

// ---------------------------------------------------------------------
// Windows rules (Platform::Windows, exercised on every host OS)
// ---------------------------------------------------------------------

#[test]
fn windows_argv0_with_path_separator_is_left_untouched() {
    let lookup = lookup_with(vec![PathBuf::from("dir1")], vec![]);
    for argv0 in [
        PathBuf::from("sub")
            .join("codex")
            .to_string_lossy()
            .into_owned(),
        PathBuf::from(".")
            .join("codex")
            .to_string_lossy()
            .into_owned(),
    ] {
        let argv = vec![argv0.clone()];
        // The stub predicate matches unconditionally: if resolution still
        // searched despite the path separator, this would wrongly resolve.
        let resolved = resolve_program(&argv, Platform::Windows, &lookup, &always_true)
            .expect("a path-separator argv0 must never be rejected");
        assert_eq!(
            resolved,
            Resolution::NotFound,
            "argv0 {argv0:?} contains a path separator; the resolver must do nothing"
        );
    }
}

/// `resolve_program` keeps doing nothing for a path-separator argv0
/// (`NotFound`), but `is_available` judges that same argv0 directly by the
/// injected predicate, independent of `resolve_program`'s result -- an
/// absolute-path profile must not read as unavailable just because
/// resolution never touches it.
#[test]
fn windows_is_available_for_path_separator_argv0_is_independent_of_resolve_program() {
    let lookup = lookup_with(vec![PathBuf::from("dir1")], vec![]);
    let argv0 = PathBuf::from("sub")
        .join("codex")
        .to_string_lossy()
        .into_owned();
    let argv = vec![argv0.clone()];

    let resolved = resolve_program(&argv, Platform::Windows, &lookup, &always_true)
        .expect("a path-separator argv0 must never be rejected");
    assert_eq!(
        resolved,
        Resolution::NotFound,
        "resolve_program must do nothing for a path-separator argv0"
    );

    assert!(
        is_available(&argv, Platform::Windows, &lookup, &always_true),
        "a path-separator argv0 must be available when the predicate reports it executable, \
         even though resolve_program reports NotFound for it"
    );
    assert!(
        !is_available(&argv, Platform::Windows, &lookup, &always_false),
        "a path-separator argv0 must be unavailable when the predicate reports it not \
         executable"
    );
}

/// A relative path-separator argv0 (`.\codex`) is judged by the
/// predicate on the literal path exactly as written, not resolved against
/// any lookup directory or the daemon's cwd.
#[test]
fn windows_is_available_for_relative_path_separator_argv0_forwards_the_raw_path_to_the_predicate() {
    let lookup = lookup_with(vec![PathBuf::from("dir1")], vec![]);
    let argv0 = PathBuf::from(".")
        .join("codex")
        .to_string_lossy()
        .into_owned();
    let argv = vec![argv0.clone()];
    // The predicate only recognizes the literal (unjoined) relative path: if
    // availability tried to resolve it against a lookup directory or a cwd,
    // the predicate would not recognize the candidate and this would
    // wrongly report unavailable.
    let predicate = predicate_over(vec![PathBuf::from(&argv0)]);

    assert!(
        is_available(&argv, Platform::Windows, &lookup, &predicate),
        "a relative path-separator argv0 must be judged by the predicate on the literal path"
    );
}

// Path-form shims: an `argv[0]` that already is a path to a `.cmd` / `.bat`
// goes through the same cmd.exe wrapping and metacharacter check as a bare name.

const PATH_FORM_SHIMS: [&str; 6] = [
    "C:\\tools\\codex.cmd",
    "C:\\tools\\codex.bat",
    "C:\\tools\\CODEX.CMD",
    "C:\\tools\\codex.Bat",
    "C:/tools/codex.cmd",
    "tools\\codex.cmd",
];

#[test]
fn windows_path_form_shim_that_exists_resolves_to_a_cmd_shim() {
    let lookup = lookup_with(vec![PathBuf::from("dir1")], vec![]);
    for argv0 in PATH_FORM_SHIMS {
        let predicate = predicate_over(vec![PathBuf::from(argv0)]);
        let argv = vec![argv0.to_string(), "hello world".to_string()];
        let resolved = resolve_program(&argv, Platform::Windows, &lookup, &predicate)
            .unwrap_or_else(|err| panic!("{argv0:?} must not be rejected: {err}"));
        assert_eq!(
            resolved,
            Resolution::CmdShim {
                comspec: lookup.windows_comspec.clone().expect("comspec fixture"),
                shim: PathBuf::from(argv0),
            },
            "a path-form shim {argv0:?} must be wrapped like a bare-name shim"
        );
    }
}

#[test]
fn windows_path_form_shim_spawn_argv_runs_the_shim_through_the_interpreter() {
    let lookup = lookup_with(vec![], vec![]);
    let argv0 = "C:\\tools\\codex.cmd";
    let predicate = predicate_over(vec![PathBuf::from(argv0)]);
    let argv = vec![argv0.to_string(), "arg-one".to_string()];

    let resolved = resolve_program(&argv, Platform::Windows, &lookup, &predicate)
        .expect("a plain path-form shim must not be rejected");
    let spawn_argv = into_spawn_argv(&resolved, &argv, Platform::Windows);

    assert_eq!(
        spawn_argv,
        vec![
            lookup
                .windows_comspec
                .clone()
                .expect("comspec fixture")
                .to_string_lossy()
                .into_owned(),
            "/d".to_string(),
            "/c".to_string(),
            argv0.to_string(),
            "arg-one".to_string(),
        ]
    );
}

#[test]
fn windows_path_form_shim_is_rejected_when_a_trailing_argument_contains_a_metacharacter() {
    let lookup = lookup_with(vec![], vec![]);
    for argv0 in PATH_FORM_SHIMS {
        let predicate = predicate_over(vec![PathBuf::from(argv0)]);
        for ch in CMD_METACHARACTERS {
            let argv = vec![argv0.to_string(), format!("safe{ch}value")];
            let outcome = resolve_program(&argv, Platform::Windows, &lookup, &predicate);
            assert!(
                matches!(outcome, Err(Rejected(_))),
                "{argv0:?} with an argument containing {ch:?} must be rejected, got {outcome:?}"
            );
        }
    }
}

#[test]
fn windows_path_form_shim_is_rejected_when_its_own_path_contains_a_metacharacter() {
    let lookup = lookup_with(vec![], vec![]);
    for ch in CMD_METACHARACTERS {
        let argv0 = format!("C:\\we{ch}ird\\codex.cmd");
        let predicate = predicate_over(vec![PathBuf::from(&argv0)]);
        let argv = vec![argv0.clone()];
        let outcome = resolve_program(&argv, Platform::Windows, &lookup, &predicate);
        assert!(
            matches!(outcome, Err(Rejected(_))),
            "a shim path containing {ch:?} must be rejected, got {outcome:?}"
        );
    }
}

#[test]
fn windows_path_form_shim_that_does_not_exist_is_not_found() {
    let lookup = lookup_with(vec![], vec![]);
    for argv0 in PATH_FORM_SHIMS {
        let argv = vec![argv0.to_string()];
        let resolved = resolve_program(&argv, Platform::Windows, &lookup, &always_false)
            .expect("a missing path-form shim must not be rejected");
        assert_eq!(resolved, Resolution::NotFound, "{argv0:?} does not exist");
    }
}

#[test]
fn windows_path_form_shim_without_a_comspec_is_not_found() {
    let mut lookup = lookup_with(vec![], vec![]);
    lookup.windows_comspec = None;
    let argv0 = "C:\\tools\\codex.cmd";
    let predicate = predicate_over(vec![PathBuf::from(argv0)]);

    let resolved = resolve_program(&[argv0.to_string()], Platform::Windows, &lookup, &predicate)
        .expect("a missing comspec must fall back to NotFound, not a rejection");
    assert_eq!(resolved, Resolution::NotFound);
}

#[test]
fn windows_path_form_exe_and_extensionless_names_stay_not_found() {
    let lookup = lookup_with(vec![], vec![]);
    for argv0 in [
        "C:\\tools\\codex.exe",
        "C:\\tools\\codex",
        "C:\\tools\\codex.ps1",
    ] {
        let argv = vec![argv0.to_string()];
        let resolved = resolve_program(&argv, Platform::Windows, &lookup, &always_true)
            .expect("a path-form non-shim must not be rejected");
        assert_eq!(
            resolved,
            Resolution::NotFound,
            "{argv0:?} is not a shim, so resolution must do nothing"
        );
    }
}

#[test]
fn unix_path_form_cmd_name_is_never_wrapped() {
    let lookup = lookup_with(vec![], vec![]);
    let argv = vec!["/opt/tools/codex.cmd".to_string(), "a&b".to_string()];
    let resolved = resolve_program(&argv, Platform::Unix, &lookup, &always_true)
        .expect("Unix resolution must never reject");
    assert_eq!(resolved, Resolution::NotFound);
}

#[test]
fn windows_path_dirs_are_searched_before_extra_bin_dirs() {
    let path_dir = PathBuf::from("path-dir");
    let extra_dir = PathBuf::from("extra-dir");
    let lookup = lookup_with(vec![path_dir.clone()], vec![extra_dir.clone()]);
    let predicate = predicate_over(vec![
        path_dir.join("codex.exe"),
        extra_dir.join("codex.exe"),
    ]);

    let argv = vec!["codex".to_string()];
    let resolved = resolve_program(&argv, Platform::Windows, &lookup, &predicate)
        .expect("a Direct match must not be rejected");
    assert_eq!(resolved, Resolution::Direct(path_dir.join("codex.exe")));
}

#[test]
fn windows_falls_back_to_extra_bin_dirs_when_not_found_in_path_dirs() {
    let path_dir = PathBuf::from("path-dir");
    let extra_dir = PathBuf::from("extra-dir");
    let lookup = lookup_with(vec![path_dir], vec![extra_dir.clone()]);
    let predicate = predicate_over(vec![extra_dir.join("codex.exe")]);

    let argv = vec!["codex".to_string()];
    let resolved = resolve_program(&argv, Platform::Windows, &lookup, &predicate)
        .expect("an extra-bin-dir match must not be rejected");
    assert_eq!(resolved, Resolution::Direct(extra_dir.join("codex.exe")));
}

#[test]
fn windows_search_order_is_directory_major_not_extension_major_across_dirs() {
    let dir1 = PathBuf::from("dir1");
    let dir2 = PathBuf::from("dir2");
    let lookup = lookup_with(vec![dir1.clone(), dir2.clone()], vec![]);
    // dir1 only has a .bat match; dir2 has a .exe match. Directory-major
    // order must pick dir1's weaker (.bat) match rather than dir2's .exe.
    let predicate = predicate_over(vec![dir1.join("codex.bat"), dir2.join("codex.exe")]);

    let argv = vec!["codex".to_string()];
    let resolved = resolve_program(&argv, Platform::Windows, &lookup, &predicate)
        .expect("a CmdShim match must not be rejected");
    assert_eq!(
        resolved,
        Resolution::CmdShim {
            comspec: lookup.windows_comspec.clone().expect("comspec fixture"),
            shim: dir1.join("codex.bat"),
        }
    );
}

/// An extensionless `argv[0]` must never resolve to a same-name
/// extensionless file -- only `.exe`, `.cmd` and `.bat` are tried. This
/// replaces the previous "the literal name is tried before any extension
/// is appended" rule, which wrongly treated an extensionless file (an
/// npm-installed POSIX shell shim, on a real Windows host) as directly
/// startable.
#[test]
fn windows_extensionless_argv0_never_matches_a_same_name_extensionless_file() {
    let dir = PathBuf::from("dir");
    let lookup = lookup_with(vec![dir.clone()], vec![]);
    // Both the extensionless literal name and a `.exe`-suffixed variant
    // exist; the extensionless file must be ignored and `.exe` must win.
    let predicate = predicate_over(vec![dir.join("codex"), dir.join("codex.exe")]);

    let argv = vec!["codex".to_string()];
    let resolved = resolve_program(&argv, Platform::Windows, &lookup, &predicate)
        .expect("a Direct match must not be rejected");
    assert_eq!(resolved, Resolution::Direct(dir.join("codex.exe")));
}

/// When only the extensionless file exists (no `.exe`, `.cmd` or `.bat`
/// variant), an extensionless `argv[0]` must resolve to nothing at all --
/// the previous rule wrongly resolved it `Direct`, handing a POSIX shell
/// script straight to `CreateProcess`.
#[test]
fn windows_extensionless_argv0_with_only_the_extensionless_file_returns_not_found() {
    let dir = PathBuf::from("npm-dir");
    let lookup = lookup_with(vec![dir.clone()], vec![]);
    let predicate = predicate_over(vec![dir.join("codex")]);

    let argv = vec!["codex".to_string()];
    let resolved = resolve_program(&argv, Platform::Windows, &lookup, &predicate)
        .expect("resolution itself must never be rejected here");
    assert_eq!(
        resolved,
        Resolution::NotFound,
        "an extensionless argv0 must not resolve to the extensionless file itself"
    );
}

/// The exact npm layout: npm places an extensionless POSIX shell shim, a
/// `.cmd` shim and a `.ps1` script side by side. An extensionless
/// `argv[0]` must resolve through the `.cmd` shim, not the extensionless
/// shell script (and `.ps1` is never a probed extension at all).
#[test]
fn windows_extensionless_argv0_resolves_the_cmd_shim_in_an_npm_style_fixture() {
    let dir = PathBuf::from("npm-dir");
    let lookup = lookup_with(vec![dir.clone()], vec![]);
    let predicate = predicate_over(vec![
        dir.join("codex"),
        dir.join("codex.cmd"),
        dir.join("codex.ps1"),
    ]);

    let argv = vec!["codex".to_string()];
    let resolved = resolve_program(&argv, Platform::Windows, &lookup, &predicate)
        .expect("a CmdShim match must not be rejected");
    assert_eq!(
        resolved,
        Resolution::CmdShim {
            comspec: lookup.windows_comspec.clone().expect("comspec fixture"),
            shim: dir.join("codex.cmd"),
        }
    );
}

/// An `argv[0]` that already carries the `.cmd` extension
/// resolves the same shim directly (the literal name is tried, matches, and
/// no extension probing happens for a name that already has one).
#[test]
fn windows_argv0_with_cmd_extension_resolves_the_cmd_shim_in_an_npm_style_fixture() {
    let dir = PathBuf::from("npm-dir");
    let lookup = lookup_with(vec![dir.clone()], vec![]);
    let predicate = predicate_over(vec![
        dir.join("codex"),
        dir.join("codex.cmd"),
        dir.join("codex.ps1"),
    ]);

    let argv = vec!["codex.cmd".to_string()];
    let resolved = resolve_program(&argv, Platform::Windows, &lookup, &predicate)
        .expect("a CmdShim match must not be rejected");
    assert_eq!(
        resolved,
        Resolution::CmdShim {
            comspec: lookup.windows_comspec.clone().expect("comspec fixture"),
            shim: dir.join("codex.cmd"),
        }
    );
}

/// When a `.exe` file also exists alongside the npm-style
/// fixture, an `argv[0]` that already carries the `.exe` extension resolves
/// it `Direct`.
#[test]
fn windows_argv0_with_exe_extension_resolves_direct_in_an_npm_style_fixture() {
    let dir = PathBuf::from("npm-dir");
    let lookup = lookup_with(vec![dir.clone()], vec![]);
    let predicate = predicate_over(vec![
        dir.join("codex"),
        dir.join("codex.cmd"),
        dir.join("codex.ps1"),
        dir.join("codex.exe"),
    ]);

    let argv = vec!["codex.exe".to_string()];
    let resolved = resolve_program(&argv, Platform::Windows, &lookup, &predicate)
        .expect("a Direct match must not be rejected");
    assert_eq!(resolved, Resolution::Direct(dir.join("codex.exe")));
}

/// An `argv[0]` that already carries an extension only tries
/// that literal name -- it must not additionally probe the other Windows
/// extensions the way an extensionless name does.
#[test]
fn windows_argv0_with_extension_does_not_probe_other_extensions_when_the_literal_file_is_absent() {
    let dir = PathBuf::from("dir");
    let lookup = lookup_with(vec![dir.clone()], vec![]);
    let predicate = predicate_over(vec![dir.join("codex.exe")]);

    let argv = vec!["codex.bat".to_string()];
    let resolved = resolve_program(&argv, Platform::Windows, &lookup, &predicate)
        .expect("resolution itself must never be rejected here");
    assert_eq!(
        resolved,
        Resolution::NotFound,
        "an argv0 already carrying .bat must not fall back to the existing .exe file"
    );
}

/// `is_available` must agree with `resolve_program` on the npm-style
/// fixture at all three points -- true for a `.cmd` shim resolved from an
/// extensionless `argv[0]`, true for a `.exe` file resolved from an
/// already-`.exe` `argv[0]`, and false when only the extensionless file
/// exists.
#[test]
fn windows_is_available_reflects_the_npm_style_fixture() {
    let dir = PathBuf::from("npm-dir");
    let lookup = lookup_with(vec![dir.clone()], vec![]);

    let cmd_shim_predicate = predicate_over(vec![
        dir.join("codex"),
        dir.join("codex.cmd"),
        dir.join("codex.ps1"),
    ]);
    assert!(
        is_available(
            &["codex".to_string()],
            Platform::Windows,
            &lookup,
            &cmd_shim_predicate,
        ),
        "an extensionless argv0 must be available through the .cmd shim"
    );

    let exe_predicate = predicate_over(vec![
        dir.join("codex"),
        dir.join("codex.cmd"),
        dir.join("codex.ps1"),
        dir.join("codex.exe"),
    ]);
    assert!(
        is_available(
            &["codex.exe".to_string()],
            Platform::Windows,
            &lookup,
            &exe_predicate,
        ),
        "an argv0 already carrying .exe must be available when the .exe file exists"
    );

    let extensionless_only_predicate = predicate_over(vec![dir.join("codex")]);
    assert!(
        !is_available(
            &["codex".to_string()],
            Platform::Windows,
            &lookup,
            &extensionless_only_predicate,
        ),
        "an extensionless argv0 must not be available when only the extensionless file exists"
    );
}

#[test]
fn windows_extension_priority_prefers_exe_over_cmd_over_bat_within_one_dir() {
    let dir = PathBuf::from("dir");
    let lookup = lookup_with(vec![dir.clone()], vec![]);
    let predicate = predicate_over(vec![
        dir.join("codex.exe"),
        dir.join("codex.cmd"),
        dir.join("codex.bat"),
    ]);

    let argv = vec!["codex".to_string()];
    let resolved = resolve_program(&argv, Platform::Windows, &lookup, &predicate)
        .expect("a Direct match must not be rejected");
    assert_eq!(resolved, Resolution::Direct(dir.join("codex.exe")));
}

#[test]
fn windows_cmd_extension_is_preferred_over_bat_when_no_exe_matches() {
    let dir = PathBuf::from("dir");
    let lookup = lookup_with(vec![dir.clone()], vec![]);
    let predicate = predicate_over(vec![dir.join("codex.cmd"), dir.join("codex.bat")]);

    let argv = vec!["codex".to_string()];
    let resolved = resolve_program(&argv, Platform::Windows, &lookup, &predicate)
        .expect("a CmdShim match must not be rejected");
    assert_eq!(
        resolved,
        Resolution::CmdShim {
            comspec: lookup.windows_comspec.clone().expect("comspec fixture"),
            shim: dir.join("codex.cmd"),
        }
    );
}

#[test]
fn windows_no_match_anywhere_returns_not_found() {
    let lookup = lookup_with(vec![PathBuf::from("dir1")], vec![PathBuf::from("dir2")]);
    let argv = vec!["codex".to_string()];
    let resolved = resolve_program(&argv, Platform::Windows, &lookup, &always_false)
        .expect("an absent program must never be rejected");
    assert_eq!(resolved, Resolution::NotFound);
}

#[test]
fn windows_missing_comspec_falls_back_to_not_found_when_cmd_shim_would_apply() {
    let dir = PathBuf::from("dir");
    let mut lookup = lookup_with(vec![dir.clone()], vec![]);
    lookup.windows_comspec = None;
    let predicate = predicate_over(vec![dir.join("codex.cmd")]);

    let argv = vec!["codex".to_string()];
    let resolved = resolve_program(&argv, Platform::Windows, &lookup, &predicate)
        .expect("a missing comspec must fall back to NotFound, not a rejection");
    assert_eq!(
        resolved,
        Resolution::NotFound,
        "without a comspec, a .cmd match cannot become CmdShim and must be treated as unresolved"
    );
}

#[test]
fn windows_cmd_shim_rejected_when_the_shim_directory_contains_a_metacharacter() {
    for ch in CMD_METACHARACTERS {
        let dir = PathBuf::from(format!("weird{ch}dir"));
        let lookup = lookup_with(vec![dir.clone()], vec![]);
        let predicate = predicate_over(vec![dir.join("codex.cmd")]);

        let argv = vec!["codex".to_string()];
        let outcome = resolve_program(&argv, Platform::Windows, &lookup, &predicate);
        assert!(
            outcome.is_err(),
            "a shim directory containing {ch:?} must be rejected, got {outcome:?}"
        );
    }
}

#[test]
fn windows_cmd_shim_rejected_when_a_trailing_argument_contains_a_metacharacter() {
    let dir = PathBuf::from("dir");
    let lookup = lookup_with(vec![dir.clone()], vec![]);
    let predicate = predicate_over(vec![dir.join("codex.cmd")]);

    for ch in CMD_METACHARACTERS {
        let argv = vec!["codex".to_string(), format!("safe{ch}value")];
        let outcome = resolve_program(&argv, Platform::Windows, &lookup, &predicate);
        assert!(
            outcome.is_err(),
            "a trailing argument containing {ch:?} must be rejected, got {outcome:?}"
        );
    }
}

/// `comspec` itself -- not just the shim path or a trailing argument --
/// must be rejected when it carries a metacharacter, once a `CmdShim`
/// resolution is reached.
#[test]
fn windows_cmd_shim_rejected_when_the_comspec_path_contains_a_metacharacter() {
    for ch in CMD_METACHARACTERS {
        let dir = PathBuf::from("dir");
        let mut lookup = lookup_with(vec![dir.clone()], vec![]);
        lookup.windows_comspec = Some(PathBuf::from(format!("weird{ch}root")).join("cmd.exe"));
        let predicate = predicate_over(vec![dir.join("codex.cmd")]);

        let argv = vec!["codex".to_string()];
        let outcome = resolve_program(&argv, Platform::Windows, &lookup, &predicate);
        assert!(
            outcome.is_err(),
            "a comspec path containing {ch:?} must be rejected, got {outcome:?}"
        );
    }
}

/// The metacharacter check runs comspec, then the shim, then trailing
/// arguments, in that order: when comspec and the shim both offend, the
/// comspec wording must be the one returned.
#[test]
fn windows_cmd_shim_rejection_reports_the_comspec_wording_when_comspec_and_shim_both_offend() {
    let dir = PathBuf::from("weird&dir");
    let mut lookup = lookup_with(vec![dir.clone()], vec![]);
    lookup.windows_comspec = Some(PathBuf::from("weird&root").join("cmd.exe"));
    let predicate = predicate_over(vec![dir.join("codex.cmd")]);

    let argv = vec!["codex".to_string()];
    let outcome = resolve_program(&argv, Platform::Windows, &lookup, &predicate);
    let Err(Rejected(message)) = outcome else {
        panic!(
            "a comspec and shim that both contain a metacharacter must be rejected, got \
             {outcome:?}"
        );
    };
    assert!(
        message.contains("command interpreter's own"),
        "comspec is checked before the shim, so its wording must win when both offend, got \
         {message:?}"
    );
}

/// When the shim path and a trailing argument both offend, the shim wording
/// must be the one returned, not the trailing-argument wording.
#[test]
fn windows_cmd_shim_rejection_reports_the_shim_wording_when_shim_and_trailing_both_offend() {
    let dir = PathBuf::from("weird&dir");
    let lookup = lookup_with(vec![dir.clone()], vec![]);
    let predicate = predicate_over(vec![dir.join("codex.cmd")]);

    let argv = vec!["codex".to_string(), "safe&value".to_string()];
    let outcome = resolve_program(&argv, Platform::Windows, &lookup, &predicate);
    let Err(Rejected(message)) = outcome else {
        panic!(
            "a shim path and a trailing argument that both contain a metacharacter must be \
             rejected, got {outcome:?}"
        );
    };
    assert!(
        message.contains("resolved script shim's path"),
        "the shim is checked before trailing arguments, so its wording must win when both \
         offend, got {message:?}"
    );
}

#[test]
fn windows_cmd_shim_allows_whitespace_in_shim_path_and_arguments() {
    let dir = PathBuf::from("Program Files").join("npm dir");
    let lookup = lookup_with(vec![dir.clone()], vec![]);
    let predicate = predicate_over(vec![dir.join("codex.cmd")]);

    let argv = vec!["codex".to_string(), "hello world".to_string()];
    let resolved = resolve_program(&argv, Platform::Windows, &lookup, &predicate)
        .expect("whitespace must not trigger the metacharacter rejection");
    assert_eq!(
        resolved,
        Resolution::CmdShim {
            comspec: lookup.windows_comspec.clone().expect("comspec fixture"),
            shim: dir.join("codex.cmd"),
        }
    );
}

#[test]
fn windows_direct_resolution_is_never_rejected_for_metacharacters_in_arguments() {
    let dir = PathBuf::from("dir");
    let lookup = lookup_with(vec![dir.clone()], vec![]);
    let predicate = predicate_over(vec![dir.join("codex.exe")]);

    // `codex.exe` is spawned directly (no cmd.exe reparsing), so a
    // metacharacter in a trailing argument must never trigger the CmdShim
    // rejection.
    let argv = vec!["codex".to_string(), "a&b".to_string()];
    let resolved = resolve_program(&argv, Platform::Windows, &lookup, &predicate)
        .expect("a Direct resolution must never be rejected for argument content");
    assert_eq!(resolved, Resolution::Direct(dir.join("codex.exe")));
}

/// A `.cmd` / `.bat` shim's own quotes survive cmd.exe's `/C` preprocessing
/// (which strips only the very first and very last quote character of the
/// whole remainder when it is not exactly one quoted segment) only when a
/// bare `call` is inserted right before the shim -- but only when *both* the
/// shim path and at least one trailing argument need Windows argv quoting
/// (empty, or containing a space, tab, newline or quote character). These
/// four tests fix the resulting truth table; only the both-need-quoting case
/// changes the expanded shape (`call` inserted), the other three keep the
/// shape `into_spawn_argv` produces without it.
#[test]
fn windows_into_spawn_argv_for_cmd_shim_inserts_call_when_shim_and_an_argument_both_require_quoting()
 {
    let comspec = PathBuf::from("comspec-root").join("cmd.exe");
    let shim = PathBuf::from("Program Files").join("codex.cmd");
    let resolution = Resolution::CmdShim {
        comspec: comspec.clone(),
        shim: shim.clone(),
    };
    let argv = vec![
        "codex".to_string(),
        "arg-one".to_string(),
        "arg two".to_string(),
    ];

    let spawn_argv = into_spawn_argv(&resolution, &argv, Platform::Windows);

    assert_eq!(
        spawn_argv,
        vec![
            comspec.to_string_lossy().into_owned(),
            "/d".to_string(),
            "/c".to_string(),
            "call".to_string(),
            shim.to_string_lossy().into_owned(),
            "arg-one".to_string(),
            "arg two".to_string(),
        ],
        "a whitespace-containing shim path with a whitespace-containing trailing argument \
         must have `call` inserted right before the shim"
    );
}

#[test]
fn windows_into_spawn_argv_for_cmd_shim_omits_call_when_only_the_shim_requires_quoting() {
    let comspec = PathBuf::from("comspec-root").join("cmd.exe");
    let shim = PathBuf::from("Program Files").join("codex.cmd");
    let resolution = Resolution::CmdShim {
        comspec: comspec.clone(),
        shim: shim.clone(),
    };
    let argv = vec!["codex".to_string(), "arg-one".to_string()];

    let spawn_argv = into_spawn_argv(&resolution, &argv, Platform::Windows);

    assert_eq!(
        spawn_argv,
        vec![
            comspec.to_string_lossy().into_owned(),
            "/d".to_string(),
            "/c".to_string(),
            shim.to_string_lossy().into_owned(),
            "arg-one".to_string(),
        ],
        "a whitespace-containing shim path with no whitespace-containing trailing argument \
         must not get `call`"
    );
}

#[test]
fn windows_into_spawn_argv_for_cmd_shim_omits_call_when_only_an_argument_requires_quoting() {
    let comspec = PathBuf::from("comspec-root").join("cmd.exe");
    let shim = PathBuf::from("dir").join("codex.cmd");
    let resolution = Resolution::CmdShim {
        comspec: comspec.clone(),
        shim: shim.clone(),
    };
    let argv = vec![
        "codex".to_string(),
        "arg-one".to_string(),
        "arg two".to_string(),
    ];

    let spawn_argv = into_spawn_argv(&resolution, &argv, Platform::Windows);

    assert_eq!(
        spawn_argv,
        vec![
            comspec.to_string_lossy().into_owned(),
            "/d".to_string(),
            "/c".to_string(),
            shim.to_string_lossy().into_owned(),
            "arg-one".to_string(),
            "arg two".to_string(),
        ],
        "a shim path with no whitespace, alongside a whitespace-containing trailing argument, \
         must not get `call`"
    );
}

#[test]
fn windows_into_spawn_argv_for_cmd_shim_omits_call_when_neither_requires_quoting() {
    let comspec = PathBuf::from("comspec-root").join("cmd.exe");
    let shim = PathBuf::from("dir").join("codex.cmd");
    let resolution = Resolution::CmdShim {
        comspec: comspec.clone(),
        shim: shim.clone(),
    };
    let argv = vec!["codex".to_string(), "arg-one".to_string()];

    let spawn_argv = into_spawn_argv(&resolution, &argv, Platform::Windows);

    assert_eq!(
        spawn_argv,
        vec![
            comspec.to_string_lossy().into_owned(),
            "/d".to_string(),
            "/c".to_string(),
            shim.to_string_lossy().into_owned(),
            "arg-one".to_string(),
        ],
        "when neither the shim path nor any trailing argument requires quoting, `call` must \
         not be inserted"
    );
}

#[test]
fn windows_into_spawn_argv_for_direct_replaces_argv0_with_resolved_path() {
    let resolved_path = PathBuf::from("dir").join("codex.exe");
    let resolution = Resolution::Direct(resolved_path.clone());
    let argv = vec!["codex".to_string(), "arg-one".to_string()];

    let spawn_argv = into_spawn_argv(&resolution, &argv, Platform::Windows);

    assert_eq!(
        spawn_argv,
        vec![
            resolved_path.to_string_lossy().into_owned(),
            "arg-one".to_string(),
        ]
    );
}

#[test]
fn windows_into_spawn_argv_for_not_found_passes_argv_through_unchanged() {
    let argv = vec!["codex".to_string(), "arg-one".to_string()];
    let spawn_argv = into_spawn_argv(&Resolution::NotFound, &argv, Platform::Windows);
    assert_eq!(spawn_argv, argv);
}

#[test]
fn windows_is_available_is_true_only_for_direct_and_cmd_shim() {
    let dir = PathBuf::from("dir");

    let direct_lookup = lookup_with(vec![dir.clone()], vec![]);
    let direct_predicate = predicate_over(vec![dir.join("codex.exe")]);
    assert!(is_available(
        &["codex".to_string()],
        Platform::Windows,
        &direct_lookup,
        &direct_predicate,
    ));

    let cmd_shim_predicate = predicate_over(vec![dir.join("codex.cmd")]);
    assert!(is_available(
        &["codex".to_string()],
        Platform::Windows,
        &direct_lookup,
        &cmd_shim_predicate,
    ));

    assert!(!is_available(
        &["codex".to_string()],
        Platform::Windows,
        &direct_lookup,
        &always_false,
    ));

    // A rejected (metacharacter) resolution is not available either.
    let weird_dir = PathBuf::from("weird&dir");
    let weird_lookup = lookup_with(vec![weird_dir.clone()], vec![]);
    let weird_predicate = predicate_over(vec![weird_dir.join("codex.cmd")]);
    assert!(!is_available(
        &["codex".to_string()],
        Platform::Windows,
        &weird_lookup,
        &weird_predicate,
    ));
}

// ---------------------------------------------------------------------
// Unix rules (Platform::Unix, exercised on every host OS)
// ---------------------------------------------------------------------

/// Mirrors
/// `windows_is_available_for_path_separator_argv0_is_independent_of_resolve_program`
/// for the Unix rule set. `resolve_program` must do nothing (`NotFound`)
/// for a path-separator argv0 on Unix too -- it must not resolve the raw
/// path itself just because the predicate happens to accept it -- while
/// `is_available` still judges that argv0 directly by the predicate.
#[test]
fn unix_is_available_for_path_separator_argv0_is_independent_of_resolve_program() {
    let lookup = lookup_with(vec![PathBuf::from("dir1")], vec![]);
    // Built as a literal string (not `PathBuf::join`) so the separator is a
    // `/` on every host, matching the POSIX rule under test regardless of
    // which OS actually runs this test binary.
    let argv0 = "sub/claude".to_string();
    let argv = vec![argv0.clone()];

    let resolved = resolve_program(&argv, Platform::Unix, &lookup, &always_true)
        .expect("a path-separator argv0 must never be rejected");
    assert_eq!(
        resolved,
        Resolution::NotFound,
        "resolve_program must do nothing for a path-separator argv0, even when the \
         predicate would accept the raw path"
    );

    assert!(
        is_available(&argv, Platform::Unix, &lookup, &always_true),
        "a path-separator argv0 must be available when the predicate reports it executable, \
         even though resolve_program reports NotFound for it"
    );
    assert!(
        !is_available(&argv, Platform::Unix, &lookup, &always_false),
        "a path-separator argv0 must be unavailable when the predicate reports it not \
         executable"
    );
}

/// Mirrors the Windows relative-path forwarding test for Unix.
/// Also pins that `resolve_program` does nothing for this relative form too
/// (not just the non-relative `"sub/claude"` form covered above): the "do
/// nothing" rule applies uniformly to every path-separator argv0.
#[test]
fn unix_is_available_for_relative_path_separator_argv0_forwards_the_raw_path_to_the_predicate() {
    let lookup = lookup_with(vec![PathBuf::from("dir1")], vec![]);
    let argv0 = "./claude".to_string();
    let argv = vec![argv0.clone()];
    let predicate = predicate_over(vec![PathBuf::from(&argv0)]);

    let resolved = resolve_program(&argv, Platform::Unix, &lookup, &predicate)
        .expect("a path-separator argv0 must never be rejected");
    assert_eq!(
        resolved,
        Resolution::NotFound,
        "resolve_program must do nothing for a relative path-separator argv0 too"
    );

    assert!(
        is_available(&argv, Platform::Unix, &lookup, &predicate),
        "a relative path-separator argv0 must be judged by the predicate on the literal path"
    );
}

#[test]
fn unix_extra_bin_dirs_are_never_searched() {
    let path_dir = PathBuf::from("path-dir");
    let extra_dir = PathBuf::from("extra-dir");
    let lookup = lookup_with(vec![path_dir], vec![extra_dir.clone()]);
    let predicate = predicate_over(vec![extra_dir.join("claude")]);

    let argv = vec!["claude".to_string()];
    let resolved = resolve_program(&argv, Platform::Unix, &lookup, &predicate)
        .expect("Unix resolution must never be rejected");
    assert_eq!(
        resolved,
        Resolution::NotFound,
        "a match that only exists under extra_bin_dirs must not resolve on Unix"
    );
}

#[test]
fn unix_resolves_exact_name_without_any_extension_probing() {
    let dir = PathBuf::from("dir");
    let lookup = lookup_with(vec![dir.clone()], vec![]);
    let predicate = predicate_over(vec![dir.join("claude")]);

    let argv = vec!["claude".to_string()];
    let resolved = resolve_program(&argv, Platform::Unix, &lookup, &predicate)
        .expect("Unix resolution must never be rejected");
    assert_eq!(resolved, Resolution::Direct(dir.join("claude")));
}

#[test]
fn unix_never_probes_windows_style_extensions() {
    let dir = PathBuf::from("dir");
    let lookup = lookup_with(vec![dir.clone()], vec![]);
    // Only the `.exe`-suffixed candidate matches; the exact (extensionless)
    // name does not. Unix must not try the `.exe` suffix at all.
    let predicate = predicate_over(vec![dir.join("claude.exe")]);

    let argv = vec!["claude".to_string()];
    let resolved = resolve_program(&argv, Platform::Unix, &lookup, &predicate)
        .expect("Unix resolution must never be rejected");
    assert_eq!(resolved, Resolution::NotFound);
}

#[test]
fn unix_missing_match_returns_not_found() {
    let lookup = lookup_with(vec![PathBuf::from("dir1"), PathBuf::from("dir2")], vec![]);
    let argv = vec!["claude".to_string()];
    let resolved = resolve_program(&argv, Platform::Unix, &lookup, &always_false)
        .expect("Unix resolution must never be rejected");
    assert_eq!(resolved, Resolution::NotFound);
}

#[test]
fn unix_resolve_never_rejects_for_shell_metacharacters_in_arguments() {
    let dir = PathBuf::from("dir");
    let lookup = lookup_with(vec![dir.clone()], vec![]);
    let predicate = predicate_over(vec![dir.join("claude")]);

    for ch in CMD_METACHARACTERS {
        let argv = vec!["claude".to_string(), format!("safe{ch}value")];
        let resolved = resolve_program(&argv, Platform::Unix, &lookup, &predicate)
            .unwrap_or_else(|err| panic!("Unix resolution must never reject on {ch:?}: {err}"));
        assert_eq!(resolved, Resolution::Direct(dir.join("claude")));
    }
}

#[test]
fn unix_into_spawn_argv_never_transforms_argv_even_when_resolution_is_direct() {
    let resolution = Resolution::Direct(PathBuf::from("dir").join("claude"));
    let argv = vec!["claude".to_string(), "arg-one".to_string()];
    let spawn_argv = into_spawn_argv(&resolution, &argv, Platform::Unix);
    assert_eq!(
        spawn_argv, argv,
        "Unix spawn argv must be byte-for-byte identical to the input argv"
    );
}

#[test]
fn unix_is_available_reflects_predicate_result() {
    let dir = PathBuf::from("dir");
    let lookup = lookup_with(vec![dir.clone()], vec![]);
    let predicate = predicate_over(vec![dir.join("claude")]);

    assert!(is_available(
        &["claude".to_string()],
        Platform::Unix,
        &lookup,
        &predicate,
    ));
    assert!(!is_available(
        &["claude".to_string()],
        Platform::Unix,
        &lookup,
        &always_false,
    ));
}

// ---------------------------------------------------------------------
// Pure environment-input seams (no `#[cfg]`: same rule, testable off-Windows)
// ---------------------------------------------------------------------

#[test]
fn windows_comspec_from_joins_system_root_with_system32_cmd_exe() {
    let system_root = OsStr::new("system-root");
    let expected = PathBuf::from("system-root")
        .join("System32")
        .join("cmd.exe");
    assert_eq!(windows_comspec_from(Some(system_root)), Some(expected));
}

#[test]
fn windows_comspec_from_is_none_when_system_root_is_absent() {
    assert_eq!(windows_comspec_from(None), None);
}

#[test]
fn windows_extra_bin_dirs_from_appends_npm_to_app_data() {
    let app_data = OsStr::new("app-data-root");
    let expected = vec![PathBuf::from("app-data-root").join("npm")];
    assert_eq!(windows_extra_bin_dirs_from(Some(app_data)), expected);
}

#[test]
fn windows_extra_bin_dirs_from_is_empty_when_app_data_is_absent() {
    assert_eq!(windows_extra_bin_dirs_from(None), Vec::<PathBuf>::new());
}

// ---------------------------------------------------------------------
// Real-environment / real-filesystem seams (the only two `#[cfg]` tests)
// ---------------------------------------------------------------------

/// `cfg(windows)`: the environment-derived `cmd.exe` really exists under
/// `System32` with native (backslash) separators. Native separators are
/// required because `cmd.exe` parses its own `/d /c` arguments by its own
/// rules, not by a general path-parsing routine, and a forward-slash comspec
/// makes that parse fail.
#[cfg(windows)]
#[test]
fn windows_default_lookup_resolves_a_real_native_separator_comspec_under_system32() {
    let lookup = ProgramLookup::from_env();
    let comspec = lookup
        .windows_comspec
        .expect("a Windows host must resolve a comspec from %SystemRoot%");

    assert!(
        comspec.is_file(),
        "the resolved comspec {comspec:?} must exist as a real file"
    );
    let text = comspec.to_string_lossy();
    assert!(
        !text.contains('/'),
        "comspec must use native separators, got {text:?}"
    );
    assert!(
        text.to_ascii_lowercase().contains("system32"),
        "comspec must be under System32, got {text:?}"
    );
    assert!(
        text.to_ascii_lowercase().ends_with("cmd.exe"),
        "comspec must end with cmd.exe, got {text:?}"
    );
}

/// `cfg(unix)`: [`default_is_executable`]'s real implementation reads the
/// file's actual execute permission bit rather than merely its existence.
#[cfg(unix)]
#[test]
fn unix_default_is_executable_distinguishes_the_execute_bit_on_a_real_file() {
    use std::os::unix::fs::PermissionsExt;

    use yagura_os::program::default_is_executable;

    let dir = yagura_test_support::paths::TempDir::new("yagura-program-resolution", "exec-bit");
    let executable = dir.write_file("run-me", "#!/bin/sh\nexit 0\n");
    let mut perms = std::fs::metadata(&executable)
        .expect("stat fixture file")
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&executable, perms).expect("chmod fixture file executable");

    let not_executable = dir.write_file("data.txt", "not a script");
    let mut perms = std::fs::metadata(&not_executable)
        .expect("stat fixture file")
        .permissions();
    perms.set_mode(0o644);
    std::fs::set_permissions(&not_executable, perms).expect("chmod fixture file non-executable");

    assert!(
        default_is_executable(&executable),
        "a file with the execute bit set must be reported executable"
    );
    assert!(
        !default_is_executable(&not_executable),
        "a file without the execute bit must not be reported executable"
    );
}

// ---------------------------------------------------------------------
// Injectable-reader environment resolution (`ProgramLookup::from_vars`).
// ---------------------------------------------------------------------

#[test]
fn from_vars_splits_path_from_the_injected_reader() {
    let joined = std::env::join_paths([PathBuf::from("vars-dir-a"), PathBuf::from("vars-dir-b")])
        .expect("join_paths for constructed dirs");
    let lookup = ProgramLookup::from_vars(move |name| (name == "PATH").then(|| joined.clone()));
    assert_eq!(
        lookup.path_dirs,
        vec![PathBuf::from("vars-dir-a"), PathBuf::from("vars-dir-b")]
    );
}

#[test]
fn from_vars_path_dirs_is_empty_when_the_reader_has_no_path() {
    let lookup = ProgramLookup::from_vars(|_name| None);
    assert_eq!(lookup.path_dirs, Vec::<PathBuf>::new());
}

/// `cfg(windows)`: the Windows-only fields (`extra_bin_dirs`, `windows_comspec`) come
/// from the injected reader, not from the process environment -- a fabricated
/// `APPDATA` proves that, while a real `SystemRoot` keeps the comspec existence
/// check (mirroring `from_env`) satisfiable.
#[cfg(windows)]
#[test]
fn from_vars_derives_windows_fields_from_the_injected_reader() {
    let real_system_root = std::env::var_os("SystemRoot").expect("Windows must set SystemRoot");
    let fabricated_app_data = PathBuf::from("from-vars-appdata-fixture");
    let lookup = ProgramLookup::from_vars(move |name| match name {
        "APPDATA" => Some(fabricated_app_data.clone().into_os_string()),
        "SystemRoot" => Some(real_system_root.clone()),
        _ => None,
    });
    assert_eq!(
        lookup.extra_bin_dirs,
        vec![PathBuf::from("from-vars-appdata-fixture").join("npm")]
    );
    let comspec = lookup
        .windows_comspec
        .expect("a real SystemRoot must resolve a comspec");
    assert!(comspec.is_file());
}

#[test]
fn from_env_matches_from_vars_fed_the_process_environment() {
    let from_env = ProgramLookup::from_env();
    let from_vars = ProgramLookup::from_vars(std::env::var_os::<&str>);
    assert_eq!(from_vars.path_dirs, from_env.path_dirs);
    assert_eq!(from_vars.extra_bin_dirs, from_env.extra_bin_dirs);
    assert_eq!(from_vars.windows_comspec, from_env.windows_comspec);
}
