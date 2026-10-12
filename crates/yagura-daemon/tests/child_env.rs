#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: the child's base environment (`ChildBaseEnv`) -- the
//! pure decision, parsing and composition rules a login-shell environment
//! read feeds into, and the two places that must consume the composed
//! result the same way (the PTY spawn path and executable resolution).

mod common;

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::time::Duration;

use yagura_daemon::child_env::{
    ChildBaseEnv, LOGIN_ENV_END_MARKER, LOGIN_ENV_START_MARKER, LoginShellSource,
    compose_child_base_env, env_for_login_shell, filter_login_env, is_reserved_env_name,
    locale_to_supply, login_env_decision, login_shell_argv, parse_login_env, select_login_shell,
};
use yagura_daemon::config::inherits_terminal;
use yagura_daemon::profile::ProfileOs;
use yagura_daemon::{DaemonConfig, PtySession};
use yagura_os::program::ProgramLookup;

fn inherited_from(pairs: Vec<(&str, &str)>) -> impl Fn(&OsStr) -> Option<OsString> {
    let owned: Vec<(OsString, OsString)> = pairs
        .into_iter()
        .map(|(key, value)| (OsString::from(key), OsString::from(value)))
        .collect();
    move |name: &OsStr| {
        owned
            .iter()
            .find(|(key, _)| key.as_os_str() == name)
            .map(|(_, value)| value.clone())
    }
}

fn no_inherited(_name: &OsStr) -> Option<OsString> {
    None
}

fn exists_among(names: Vec<&str>) -> impl Fn(&str) -> bool {
    let owned: Vec<String> = names.into_iter().map(str::to_string).collect();
    move |name: &str| owned.iter().any(|candidate| candidate == name)
}

fn exists_paths_among(paths: Vec<&str>) -> impl Fn(&Path) -> bool {
    let owned: Vec<PathBuf> = paths.into_iter().map(PathBuf::from).collect();
    move |candidate: &Path| owned.iter().any(|known| known == candidate)
}

/// Appends one `env -0`-style item (`NAME=VALUE` followed by a NUL) to `buf`.
fn push_item(buf: &mut Vec<u8>, name: &str, value: &str) {
    buf.extend_from_slice(name.as_bytes());
    buf.push(b'=');
    buf.extend_from_slice(value.as_bytes());
    buf.push(0);
}

// ---------------------------------------------------------------------
// Whether the daemon's own TERM counts as inherited from a real terminal.
// ---------------------------------------------------------------------

#[test]
fn inherits_terminal_is_false_for_a_missing_or_blank_term() {
    assert!(!inherits_terminal(None));
    assert!(!inherits_terminal(Some(OsStr::new(""))));
}

#[test]
fn inherits_terminal_is_true_for_a_non_empty_term() {
    assert!(inherits_terminal(Some(OsStr::new("xterm-256color"))));
}

// ---------------------------------------------------------------------
// Whether the login-shell environment read runs at all.
// ---------------------------------------------------------------------

#[test]
fn login_env_decision_skips_windows_regardless_of_term() {
    for term in [
        None,
        Some(OsStr::new("")),
        Some(OsStr::new("xterm-256color")),
    ] {
        let decision = login_env_decision(ProfileOs::Windows, term);
        assert!(
            decision.skip_reason() == Some("windows"),
            "term={term:?}: {decision:?}"
        );
    }
}

#[test]
fn login_env_decision_reads_on_unix_hosts_with_no_inherited_term() {
    for host_os in [ProfileOs::MacOs, ProfileOs::Linux] {
        for term in [None, Some(OsStr::new(""))] {
            let decision = login_env_decision(host_os, term);
            assert!(
                decision.skip_reason().is_none(),
                "host_os={host_os:?} term={term:?}: {decision:?}"
            );
        }
    }
}

#[test]
fn login_env_decision_skips_terminal_launches_on_unix_hosts() {
    for host_os in [ProfileOs::MacOs, ProfileOs::Linux] {
        let decision = login_env_decision(host_os, Some(OsStr::new("xterm-256color")));
        assert!(
            decision.skip_reason() == Some("terminal"),
            "host_os={host_os:?}: {decision:?}"
        );
    }
}

// ---------------------------------------------------------------------
// The argv the login-shell read spawns, by shell family.
// ---------------------------------------------------------------------

const LOGIN_INTERACTIVE_SHELLS: [&str; 4] = ["zsh", "bash", "sh", "fish"];
const INTERACTIVE_ONLY_SHELLS: [&str; 2] = ["csh", "tcsh"];

#[test]
fn login_shell_argv_uses_login_interactive_flags_for_common_shells_by_basename() {
    let mut previous_cmd: Option<String> = None;
    for shell in LOGIN_INTERACTIVE_SHELLS {
        for shell_path in [shell.to_string(), format!("/usr/local/bin/{shell}")] {
            let argv = login_shell_argv(&shell_path);
            assert_eq!(argv.len(), 5, "shell={shell_path}: {argv:?}");
            assert_eq!(argv[0], shell_path);
            assert_eq!(argv[1], "-l");
            assert_eq!(argv[2], "-i");
            assert_eq!(argv[3], "-c");
            let cmd = argv[4].clone();
            assert!(cmd.contains("/usr/bin/printf"), "{cmd:?}");
            assert!(cmd.contains("/usr/bin/env -0"), "{cmd:?}");
            if let Some(prev) = &previous_cmd {
                assert_eq!(
                    &cmd, prev,
                    "the CMD text must not depend on which login-interactive shell runs it"
                );
            }
            previous_cmd = Some(cmd);
        }
    }
}

#[test]
fn login_shell_argv_uses_interactive_only_flag_for_csh_family_by_basename() {
    let mut previous_cmd: Option<String> = None;
    for shell in INTERACTIVE_ONLY_SHELLS {
        for shell_path in [shell.to_string(), format!("/usr/bin/{shell}")] {
            let argv = login_shell_argv(&shell_path);
            assert_eq!(argv.len(), 3, "shell={shell_path}: {argv:?}");
            assert_eq!(argv[0], shell_path);
            assert_eq!(argv[1], "-ic");
            let cmd = argv[2].clone();
            assert!(cmd.contains("/usr/bin/printf"), "{cmd:?}");
            assert!(cmd.contains("/usr/bin/env -0"), "{cmd:?}");
            if let Some(prev) = &previous_cmd {
                assert_eq!(
                    &cmd, prev,
                    "the CMD text must not depend on which csh-family shell runs it"
                );
            }
            previous_cmd = Some(cmd);
        }
    }
}

#[test]
fn login_shell_argv_cmd_text_is_identical_across_shell_families() {
    let login_interactive = login_shell_argv("bash");
    let interactive_only = login_shell_argv("csh");
    assert_eq!(
        login_interactive.last(),
        interactive_only.last(),
        "the CMD text must not depend on the shell's own quirks"
    );
}

// ---------------------------------------------------------------------
// Extracting the login shell's reported environment from its raw stdout.
// ---------------------------------------------------------------------

#[test]
fn parse_login_env_extracts_entries_between_markers_ignoring_lookalike_noise() {
    let start_marker_text =
        std::str::from_utf8(LOGIN_ENV_START_MARKER).expect("the start marker is fixed ASCII");
    let end_marker_text =
        std::str::from_utf8(LOGIN_ENV_END_MARKER).expect("the end marker is fixed ASCII");

    let mut raw = Vec::new();
    raw.extend_from_slice(b"rc-noise-before-that-could-fool-a-naive-scan");
    raw.extend_from_slice(start_marker_text.as_bytes());
    push_item(&mut raw, "PLAIN", "value");
    push_item(&mut raw, "MULTI", "line one\nline two=three");
    push_item(
        &mut raw,
        "SNEAKY_VALUE",
        &format!("pre-{end_marker_text}-post"),
    );
    push_item(
        &mut raw,
        &format!("{end_marker_text}_LOOKALIKE"),
        "still-a-value",
    );
    raw.extend_from_slice(b"NO_EQUALS_SIGN_AT_ALL");
    raw.push(0);
    push_item(&mut raw, "", "orphan-empty-name");
    raw.extend_from_slice(end_marker_text.as_bytes());
    raw.extend_from_slice(b"noise-after-that-also-looks-like-a-marker");

    let parsed: Vec<(OsString, OsString)> =
        parse_login_env(&raw).expect("well-formed input with both markers must parse");

    let find = |name: &str| -> Option<OsString> {
        parsed
            .iter()
            .find(|(entry_name, _): &&(OsString, OsString)| entry_name == OsStr::new(name))
            .map(|(_, value): &(OsString, OsString)| value.clone())
    };

    assert_eq!(find("PLAIN"), Some(OsString::from("value")));
    assert_eq!(
        find("MULTI"),
        Some(OsString::from("line one\nline two=three")),
        "a value containing a newline and an '=' must survive whole"
    );
    assert_eq!(
        find("SNEAKY_VALUE"),
        Some(OsString::from(format!("pre-{end_marker_text}-post"))),
        "a value merely containing the end marker's text must not truncate parsing"
    );
    assert_eq!(
        find(&format!("{end_marker_text}_LOOKALIKE")),
        Some(OsString::from("still-a-value")),
        "a name merely starting with the end marker's text must not be read as the terminator"
    );
    assert_eq!(
        find(""),
        None,
        "an entry with an empty name must be dropped"
    );
    assert!(
        !parsed
            .iter()
            .any(|(entry_name, _): &(OsString, OsString)| entry_name
                .to_string_lossy()
                .starts_with("NO_EQUALS_SIGN")),
        "an entry with no '=' at all must be dropped: {parsed:?}"
    );
    assert_eq!(
        parsed.len(),
        4,
        "exactly the four well-formed entries must survive: {parsed:?}"
    );
}

#[test]
fn parse_login_env_is_none_when_the_start_marker_is_missing() {
    let end_marker_text =
        std::str::from_utf8(LOGIN_ENV_END_MARKER).expect("the end marker is fixed ASCII");
    let mut raw = Vec::new();
    push_item(&mut raw, "PLAIN", "value");
    raw.extend_from_slice(end_marker_text.as_bytes());
    assert_eq!(parse_login_env(&raw), None);
}

#[test]
fn parse_login_env_is_none_when_the_end_marker_is_missing() {
    let start_marker_text =
        std::str::from_utf8(LOGIN_ENV_START_MARKER).expect("the start marker is fixed ASCII");
    let mut raw = Vec::new();
    raw.extend_from_slice(start_marker_text.as_bytes());
    push_item(&mut raw, "PLAIN", "value");
    assert_eq!(parse_login_env(&raw), None);
}

#[test]
fn parse_login_env_reads_an_empty_environment_as_empty() {
    let mut raw = LOGIN_ENV_START_MARKER.to_vec();
    raw.extend_from_slice(LOGIN_ENV_END_MARKER);
    assert_eq!(parse_login_env(&raw), Some(Vec::new()));
}

#[test]
fn parse_login_env_ignores_items_printed_after_the_end_marker() {
    let mut raw = LOGIN_ENV_START_MARKER.to_vec();
    push_item(&mut raw, "KEPT", "1");
    raw.extend_from_slice(LOGIN_ENV_END_MARKER);
    // Logout output after the marker: a NUL-led item, then text with no marker in it.
    raw.push(0);
    push_item(&mut raw, "AFTER_THE_END", "2");
    raw.extend_from_slice(b"logout-noise-longer-than-the-end-marker");

    assert_eq!(
        parse_login_env(&raw),
        Some(vec![(OsString::from("KEPT"), OsString::from("1"))])
    );
}

#[cfg(unix)]
#[test]
fn parse_login_env_preserves_a_non_utf8_value_on_unix() {
    use std::os::unix::ffi::OsStrExt;

    let start_marker_text =
        std::str::from_utf8(LOGIN_ENV_START_MARKER).expect("the start marker is fixed ASCII");
    let end_marker_text =
        std::str::from_utf8(LOGIN_ENV_END_MARKER).expect("the end marker is fixed ASCII");

    let mut raw = Vec::new();
    raw.extend_from_slice(start_marker_text.as_bytes());
    raw.extend_from_slice(b"NON_UTF8=");
    raw.extend_from_slice(&[b'x', 0xFF, 0xFE, b'y']);
    raw.push(0);
    raw.extend_from_slice(end_marker_text.as_bytes());

    let parsed = parse_login_env(&raw).expect("well-formed input with both markers must parse");
    let value = parsed
        .iter()
        .find(|(name, _)| name == OsStr::new("NON_UTF8"))
        .map(|(_, value)| value.clone())
        .expect("NON_UTF8 must be present");
    assert_eq!(
        value,
        OsStr::from_bytes(&[b'x', 0xFF, 0xFE, b'y']).to_os_string()
    );
}

// ---------------------------------------------------------------------
// Which read-back variables the daemon's own injection reserves.
// ---------------------------------------------------------------------

#[test]
fn is_reserved_env_name_matches_the_yagura_prefix_case_insensitively() {
    assert!(is_reserved_env_name(OsStr::new("YAGURA_INSTANCE")));
    assert!(is_reserved_env_name(OsStr::new("yagura_status_token")));
    assert!(is_reserved_env_name(OsStr::new("Yagura_Window_Id")));
    assert!(!is_reserved_env_name(OsStr::new("PATH")));
    assert!(!is_reserved_env_name(OsStr::new("YAGUR")));
}

#[cfg(unix)]
#[test]
fn is_reserved_env_name_does_not_panic_on_a_non_utf8_name() {
    use std::os::unix::ffi::OsStrExt;

    let non_utf8 = OsStr::from_bytes(&[0xFF, 0xFE, b'_']);
    assert!(!is_reserved_env_name(non_utf8));
}

#[test]
fn filter_login_env_drops_every_excluded_variable_and_keeps_the_rest() {
    let vars = vec![
        (OsString::from("SHLVL"), OsString::from("2")),
        (OsString::from("PWD"), OsString::from("/tmp")),
        (OsString::from("OLDPWD"), OsString::from("/")),
        (OsString::from("_"), OsString::from("/usr/bin/env")),
        (OsString::from("TERM"), OsString::from("xterm")),
        (OsString::from("COLUMNS"), OsString::from("80")),
        (OsString::from("LINES"), OsString::from("24")),
        (
            OsString::from("YAGURA_INSTANCE"),
            OsString::from("attacker"),
        ),
        (
            OsString::from("yagura_status_token"),
            OsString::from("attacker-lower"),
        ),
        (OsString::from("PATH"), OsString::from("/usr/bin")),
        (OsString::from("LANG"), OsString::from("en_US.UTF-8")),
    ];
    let filtered: Vec<(OsString, OsString)> = filter_login_env(vars);
    let names: Vec<String> = filtered
        .iter()
        .map(|(name, _): &(OsString, OsString)| name.to_string_lossy().into_owned())
        .collect();
    for dropped in [
        "SHLVL",
        "PWD",
        "OLDPWD",
        "_",
        "TERM",
        "COLUMNS",
        "LINES",
        "YAGURA_INSTANCE",
        "yagura_status_token",
    ] {
        assert!(
            !names.contains(&dropped.to_string()),
            "{dropped} must be dropped: {names:?}"
        );
    }
    assert!(names.contains(&"PATH".to_string()));
    assert!(names.contains(&"LANG".to_string()));
}

// ---------------------------------------------------------------------
// What is handed to the login shell itself.
// ---------------------------------------------------------------------

#[test]
fn env_for_login_shell_drops_only_the_yagura_prefixed_names_and_keeps_order() {
    let inherited = vec![
        (OsString::from("HOME"), OsString::from("/home/user")),
        (OsString::from("YAGURA_INSTANCE"), OsString::from("abc")),
        (OsString::from("PATH"), OsString::from("/usr/bin")),
        (OsString::from("Yagura_Something"), OsString::from("def")),
        (OsString::from("SHLVL"), OsString::from("2")),
    ];
    let result = env_for_login_shell(inherited);
    assert_eq!(
        result,
        vec![
            (OsString::from("HOME"), OsString::from("/home/user")),
            (OsString::from("PATH"), OsString::from("/usr/bin")),
            (OsString::from("SHLVL"), OsString::from("2")),
        ],
        "only the YAGURA_ prefix is dropped here; SHLVL etc. are excluded later, from the \
         shell's read-back result, not from what is handed to it"
    );
}

// ---------------------------------------------------------------------
// Which shell reads the login environment.
// ---------------------------------------------------------------------

#[test]
fn select_login_shell_prefers_an_existing_absolute_shell_env_value() {
    let exists = exists_paths_among(vec!["/usr/bin/zsh", "/usr/bin/bash", "/bin/sh"]);
    assert_eq!(
        select_login_shell(
            Some(Path::new("/usr/bin/zsh")),
            Some(Path::new("/usr/bin/bash")),
            &exists,
        ),
        Some((PathBuf::from("/usr/bin/zsh"), LoginShellSource::Env))
    );
}

#[test]
fn select_login_shell_skips_a_relative_shell_env_value() {
    let exists = exists_paths_among(vec!["/usr/bin/bash", "/bin/sh"]);
    assert_eq!(
        select_login_shell(
            Some(Path::new("zsh")),
            Some(Path::new("/usr/bin/bash")),
            &exists
        ),
        Some((PathBuf::from("/usr/bin/bash"), LoginShellSource::Passwd))
    );
}

#[test]
fn select_login_shell_skips_an_empty_shell_env_value() {
    let exists = exists_paths_among(vec!["/usr/bin/bash", "/bin/sh"]);
    assert_eq!(
        select_login_shell(
            Some(Path::new("")),
            Some(Path::new("/usr/bin/bash")),
            &exists
        ),
        Some((PathBuf::from("/usr/bin/bash"), LoginShellSource::Passwd))
    );
}

#[test]
fn select_login_shell_skips_a_shell_env_value_that_does_not_exist() {
    let exists = exists_paths_among(vec!["/usr/bin/bash", "/bin/sh"]);
    assert_eq!(
        select_login_shell(
            Some(Path::new("/usr/bin/zsh")),
            Some(Path::new("/usr/bin/bash")),
            &exists,
        ),
        Some((PathBuf::from("/usr/bin/bash"), LoginShellSource::Passwd))
    );
}

#[test]
fn select_login_shell_falls_back_to_bin_sh_when_passwd_shell_is_relative() {
    let exists = exists_paths_among(vec!["/bin/sh"]);
    assert_eq!(
        select_login_shell(None, Some(Path::new("bash")), &exists),
        Some((PathBuf::from("/bin/sh"), LoginShellSource::Fallback))
    );
}

#[test]
fn select_login_shell_falls_back_to_bin_sh_when_passwd_shell_does_not_exist() {
    let exists = exists_paths_among(vec!["/bin/sh"]);
    assert_eq!(
        select_login_shell(None, Some(Path::new("/usr/bin/bash")), &exists),
        Some((PathBuf::from("/bin/sh"), LoginShellSource::Fallback))
    );
}

#[test]
fn select_login_shell_is_none_when_nothing_resolves() {
    let exists = exists_paths_among(vec![]);
    assert_eq!(select_login_shell(None, None, &exists), None);
}

#[test]
fn login_shell_source_as_str_matches_the_logged_words() {
    assert_eq!(LoginShellSource::Env.as_str(), "env");
    assert_eq!(LoginShellSource::Passwd.as_str(), "passwd");
    assert_eq!(LoginShellSource::Fallback.as_str(), "fallback");
}

// ---------------------------------------------------------------------
// Whether a locale needs to be supplied, and which one.
// ---------------------------------------------------------------------

#[test]
fn locale_to_supply_does_not_supply_when_base_has_a_non_empty_lc_all() {
    let base = vec![(OsString::from("LC_ALL"), OsString::from("en_US.UTF-8"))];
    let exists = exists_among(vec!["en_US.UTF-8", "C.UTF-8"]);
    assert_eq!(
        locale_to_supply(&base, &no_inherited, Some("ja_JP.UTF-8"), &exists),
        None
    );
}

#[test]
fn locale_to_supply_does_not_supply_when_base_has_a_non_empty_lc_ctype() {
    let base = vec![(OsString::from("LC_CTYPE"), OsString::from("en_US.UTF-8"))];
    let exists = exists_among(vec!["en_US.UTF-8"]);
    assert_eq!(locale_to_supply(&base, &no_inherited, None, &exists), None);
}

#[test]
fn locale_to_supply_does_not_supply_when_base_has_a_non_empty_lang() {
    let base = vec![(OsString::from("LANG"), OsString::from("en_US.UTF-8"))];
    let exists = exists_among(vec!["en_US.UTF-8"]);
    assert_eq!(locale_to_supply(&base, &no_inherited, None, &exists), None);
}

#[test]
fn locale_to_supply_treats_an_empty_base_value_as_absent() {
    let base = vec![(OsString::from("LANG"), OsString::from(""))];
    let exists = exists_among(vec!["C.UTF-8"]);
    assert_eq!(
        locale_to_supply(&base, &no_inherited, None, &exists),
        Some(OsString::from("C.UTF-8")),
        "an empty LANG in the base environment must not count as already set"
    );
}

#[test]
fn locale_to_supply_is_not_satisfied_by_a_non_locale_base_variable() {
    let base = vec![(OsString::from("PATH"), OsString::from("/usr/bin"))];
    let exists = exists_among(vec!["C.UTF-8"]);
    assert_eq!(
        locale_to_supply(&base, &no_inherited, None, &exists),
        Some(OsString::from("C.UTF-8"))
    );
}

#[test]
fn locale_to_supply_checks_the_base_environment_before_the_inherited_one() {
    let base = vec![(OsString::from("LC_ALL"), OsString::from("en_US.UTF-8"))];
    let inherited = inherited_from(vec![("LC_ALL", "should-not-matter")]);
    let exists = exists_among(vec!["en_US.UTF-8"]);
    assert_eq!(locale_to_supply(&base, &inherited, None, &exists), None);
}

#[test]
fn locale_to_supply_falls_back_to_the_inherited_environment_when_the_base_is_absent() {
    let base: Vec<(OsString, OsString)> = Vec::new();
    let inherited = inherited_from(vec![("LANG", "en_US.UTF-8")]);
    let exists = exists_among(vec!["en_US.UTF-8"]);
    assert_eq!(locale_to_supply(&base, &inherited, None, &exists), None);
}

#[test]
fn locale_to_supply_prefers_the_preferred_locale_when_it_exists() {
    let base: Vec<(OsString, OsString)> = Vec::new();
    let exists = exists_among(vec!["ja_JP.UTF-8", "en_US.UTF-8", "C.UTF-8"]);
    assert_eq!(
        locale_to_supply(&base, &no_inherited, Some("ja_JP.UTF-8"), &exists),
        Some(OsString::from("ja_JP.UTF-8"))
    );
}

#[test]
fn locale_to_supply_falls_back_to_en_us_utf8_when_the_preferred_locale_does_not_exist() {
    let base: Vec<(OsString, OsString)> = Vec::new();
    let exists = exists_among(vec!["en_US.UTF-8", "C.UTF-8"]);
    assert_eq!(
        locale_to_supply(&base, &no_inherited, Some("ja_JP.UTF-8"), &exists),
        Some(OsString::from("en_US.UTF-8"))
    );
}

#[test]
fn locale_to_supply_falls_back_to_c_utf8_when_neither_preferred_nor_en_us_exist() {
    let base: Vec<(OsString, OsString)> = Vec::new();
    let exists = exists_among(vec!["C.UTF-8"]);
    assert_eq!(
        locale_to_supply(&base, &no_inherited, Some("ja_JP.UTF-8"), &exists),
        Some(OsString::from("C.UTF-8"))
    );
}

#[test]
fn locale_to_supply_skips_the_preferred_candidate_when_there_is_none() {
    let base: Vec<(OsString, OsString)> = Vec::new();
    let exists = exists_among(vec!["C.UTF-8"]);
    assert_eq!(
        locale_to_supply(&base, &no_inherited, None, &exists),
        Some(OsString::from("C.UTF-8")),
        "a missing preferred locale (Linux) must fall through to en_US.UTF-8, then C.UTF-8"
    );
}

#[test]
fn locale_to_supply_is_none_when_no_candidate_exists() {
    let base: Vec<(OsString, OsString)> = Vec::new();
    let exists = exists_among(vec![]);
    assert_eq!(
        locale_to_supply(&base, &no_inherited, Some("ja_JP.UTF-8"), &exists),
        None
    );
}

// ---------------------------------------------------------------------
// Composing the base environment from the read-back and the locale rule.
// ---------------------------------------------------------------------

#[test]
fn compose_child_base_env_is_always_empty_on_windows() {
    let with_vars = compose_child_base_env(
        ProfileOs::Windows,
        Some(vec![(OsString::from("PATH"), OsString::from("/usr/bin"))]),
        &no_inherited,
        Some("ja_JP.UTF-8"),
        &exists_among(vec!["ja_JP.UTF-8", "en_US.UTF-8", "C.UTF-8"]),
    );
    assert!(
        with_vars.is_empty(),
        "Windows must never carry a child base environment"
    );

    let without_vars = compose_child_base_env(
        ProfileOs::Windows,
        None,
        &no_inherited,
        Some("ja_JP.UTF-8"),
        &exists_among(vec!["ja_JP.UTF-8"]),
    );
    assert!(without_vars.is_empty());
}

#[test]
fn compose_child_base_env_applies_the_exclusion_rule_on_unix() {
    let login_vars = vec![
        (OsString::from("SHLVL"), OsString::from("2")),
        (
            OsString::from("YAGURA_INSTANCE"),
            OsString::from("attacker"),
        ),
        (OsString::from("PATH"), OsString::from("/usr/bin")),
        (OsString::from("LANG"), OsString::from("en_US.UTF-8")),
    ];
    let composed = compose_child_base_env(
        ProfileOs::Linux,
        Some(login_vars),
        &no_inherited,
        None,
        &exists_among(vec!["C.UTF-8"]),
    );
    assert_eq!(composed.get(OsStr::new("SHLVL")), None);
    assert_eq!(composed.get(OsStr::new("YAGURA_INSTANCE")), None);
    assert_eq!(
        composed.get(OsStr::new("PATH")),
        Some(OsStr::new("/usr/bin"))
    );
    assert_eq!(
        composed.get(OsStr::new("LANG")),
        Some(OsStr::new("en_US.UTF-8"))
    );
}

#[test]
fn compose_child_base_env_supplies_lang_when_nothing_declares_a_locale() {
    let composed = compose_child_base_env(
        ProfileOs::Linux,
        Some(Vec::new()),
        &no_inherited,
        None,
        &exists_among(vec!["C.UTF-8"]),
    );
    assert_eq!(
        composed.get(OsStr::new("LANG")),
        Some(OsStr::new("C.UTF-8"))
    );
}

#[test]
fn compose_child_base_env_supplies_lang_even_when_the_login_shell_read_failed() {
    let composed = compose_child_base_env(
        ProfileOs::MacOs,
        None,
        &no_inherited,
        None,
        &exists_among(vec!["C.UTF-8"]),
    );
    assert_eq!(
        composed.get(OsStr::new("LANG")),
        Some(OsStr::new("C.UTF-8"))
    );
}

#[test]
fn compose_child_base_env_does_not_supply_lang_when_login_vars_already_set_it() {
    let login_vars = vec![(OsString::from("LANG"), OsString::from("ja_JP.UTF-8"))];
    let composed = compose_child_base_env(
        ProfileOs::Linux,
        Some(login_vars),
        &no_inherited,
        None,
        &exists_among(vec!["C.UTF-8"]),
    );
    assert_eq!(
        composed.get(OsStr::new("LANG")),
        Some(OsStr::new("ja_JP.UTF-8"))
    );
}

// ---------------------------------------------------------------------
// The ChildBaseEnv container itself: lookup, and hiding secrets from Debug.
// ---------------------------------------------------------------------

#[test]
fn child_base_env_default_is_empty() {
    let base = ChildBaseEnv::default();
    assert!(base.is_empty());
    assert_eq!(base.len(), 0);
    assert_eq!(base.get(OsStr::new("ANYTHING")), None);
    assert_eq!(base.iter().count(), 0);
}

#[test]
fn child_base_env_len_and_iter_reflect_the_constructed_pairs() {
    let base = ChildBaseEnv::from_pairs(vec![
        (OsString::from("A"), OsString::from("1")),
        (OsString::from("B"), OsString::from("2")),
    ]);
    assert!(!base.is_empty());
    assert_eq!(base.len(), 2);
    let collected: Vec<(&OsStr, &OsStr)> = base.iter().collect();
    assert_eq!(collected.len(), 2);
    assert!(collected.contains(&(OsStr::new("A"), OsStr::new("1"))));
    assert!(collected.contains(&(OsStr::new("B"), OsStr::new("2"))));
}

#[test]
fn child_base_env_get_returns_the_last_value_for_a_duplicated_name() {
    let base = ChildBaseEnv::from_pairs(vec![
        (OsString::from("DUP"), OsString::from("first")),
        (OsString::from("DUP"), OsString::from("second")),
    ]);
    assert_eq!(base.get(OsStr::new("DUP")), Some(OsStr::new("second")));
}

#[test]
fn child_base_env_debug_does_not_reveal_names_or_values() {
    let base = ChildBaseEnv::from_pairs(vec![(
        OsString::from("SENTINEL_SECRET_NAME"),
        OsString::from("sentinel-secret-value"),
    )]);
    let rendered = format!("{base:?}");
    assert!(!rendered.contains("SENTINEL_SECRET_NAME"));
    assert!(!rendered.contains("sentinel-secret-value"));
}

#[test]
fn daemon_config_debug_does_not_reveal_child_base_env_names_or_values() {
    let config = DaemonConfig {
        child_base_env: ChildBaseEnv::from_pairs(vec![(
            OsString::from("SENTINEL_SECRET_NAME"),
            OsString::from("sentinel-secret-value"),
        )]),
        ..DaemonConfig::default()
    };
    let rendered = format!("{config:?}");
    assert!(!rendered.contains("SENTINEL_SECRET_NAME"));
    assert!(!rendered.contains("sentinel-secret-value"));
}

// ---------------------------------------------------------------------
// Executable resolution built from a ChildBaseEnv.
// ---------------------------------------------------------------------

#[test]
fn child_base_env_default_program_lookup_matches_from_env() {
    let looked_up = ChildBaseEnv::default().program_lookup();
    let from_env = ProgramLookup::from_env();
    assert_eq!(looked_up.path_dirs, from_env.path_dirs);
    assert_eq!(looked_up.extra_bin_dirs, from_env.extra_bin_dirs);
    assert_eq!(looked_up.windows_comspec, from_env.windows_comspec);
}

#[test]
fn child_base_env_program_lookup_splits_its_own_path_value() {
    let dir_a = PathBuf::from("base-env-dir-a");
    let dir_b = PathBuf::from("base-env-dir-b");
    let joined =
        std::env::join_paths([&dir_a, &dir_b]).expect("join_paths for constructed base-env dirs");
    let base = ChildBaseEnv::from_pairs(vec![(OsString::from("PATH"), joined)]);
    let looked_up = base.program_lookup();
    assert_eq!(looked_up.path_dirs, vec![dir_a, dir_b]);
}

// ---------------------------------------------------------------------
// PTY wiring: the base environment reaches a spawned child, and combines
// with `extra_env` and the declared terminal in the documented order.
// ---------------------------------------------------------------------

async fn probe_env(
    config: &DaemonConfig,
    extra_env: &[(&str, &str)],
    var_name: &str,
    expected: &str,
) {
    let probe = common::env_probe_path();
    let probe_str = probe.to_str().expect("env_probe path must be valid UTF-8");
    let session = PtySession::spawn_with_command_in_env(
        &[probe_str, var_name],
        None::<&Path>,
        extra_env,
        config,
    )
    .await
    .expect("spawn_with_command_in_env should start env_probe");

    let text =
        common::wait_for_scrollback_containing(&session, expected, Duration::from_secs(10)).await;
    assert!(
        text.contains(expected),
        "env_probe must report {var_name} as part of {expected:?}; got {text:?}"
    );
    session.terminate();
}

#[tokio::test]
async fn child_base_env_variables_reach_the_spawned_child() {
    let config = DaemonConfig {
        child_base_env: ChildBaseEnv::from_pairs(vec![(
            OsString::from("CHILD_ENV_PROBE_VAR"),
            OsString::from("from-base-env"),
        )]),
        ..DaemonConfig::default()
    };
    probe_env(
        &config,
        &[],
        "CHILD_ENV_PROBE_VAR",
        "<<CHILD_ENV_PROBE_VAR=from-base-env>>",
    )
    .await;
}

#[tokio::test]
async fn extra_env_overrides_a_same_named_child_base_env_variable() {
    let config = DaemonConfig {
        child_base_env: ChildBaseEnv::from_pairs(vec![(
            OsString::from("CHILD_ENV_PROBE_VAR"),
            OsString::from("from-base-env"),
        )]),
        ..DaemonConfig::default()
    };
    probe_env(
        &config,
        &[("CHILD_ENV_PROBE_VAR", "from-extra-env")],
        "CHILD_ENV_PROBE_VAR",
        "<<CHILD_ENV_PROBE_VAR=from-extra-env>>",
    )
    .await;
}

#[tokio::test]
async fn the_declared_term_wins_over_a_same_named_child_base_env_variable() {
    let config = DaemonConfig {
        child_base_env: ChildBaseEnv::from_pairs(vec![(
            OsString::from("TERM"),
            OsString::from("from-base-env-term"),
        )]),
        ..DaemonConfig::default()
    };
    probe_env(&config, &[], "TERM", "<<TERM=xterm-256color>>").await;
}

#[tokio::test]
async fn a_launching_terminals_variable_in_the_child_base_env_does_not_reach_the_child() {
    let config = DaemonConfig {
        child_base_env: ChildBaseEnv::from_pairs(vec![(
            OsString::from("TERM_PROGRAM"),
            OsString::from("from-login-shell"),
        )]),
        ..DaemonConfig::default()
    };
    probe_env(&config, &[], "TERM_PROGRAM", "<<TERM_PROGRAM unset>>").await;
}

#[tokio::test]
async fn an_empty_child_base_env_injects_nothing_extra() {
    let config = DaemonConfig::default();
    probe_env(
        &config,
        &[],
        "YAGURA_CHILD_ENV_TEST_UNSET_SENTINEL",
        "<<YAGURA_CHILD_ENV_TEST_UNSET_SENTINEL unset>>",
    )
    .await;
}
