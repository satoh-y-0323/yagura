#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: the operating-system queries behind the child's base
//! environment -- whether a locale exists, the user's preferred locale and
//! the login shell in the password database.

use yagura_os::locale::{PreferredLocale, locale_exists, preferred_locale};
use yagura_os::login_shell::passwd_login_shell;

#[cfg(unix)]
#[test]
fn the_c_locale_exists_on_unix() {
    // POSIX requires the C locale, so this is the positive control for the
    // negative cases below.
    assert!(locale_exists("C"));
}

#[test]
fn a_locale_no_system_ships_does_not_exist() {
    assert!(!locale_exists("yagura_NO.SUCH-LOCALE"));
}

#[test]
fn a_locale_name_with_an_interior_nul_does_not_exist() {
    // Cut at the NUL, the name would read as "C", which does exist on Unix.
    assert!(!locale_exists("C\0junk"));
}

#[cfg(windows)]
#[test]
fn no_locale_counts_as_existing_off_unix() {
    // Windows programs do not read LANG, so nothing is ever supplied there.
    assert!(!locale_exists("C"));
    assert!(!locale_exists("en_US.UTF-8"));
}

#[test]
fn a_preferred_locale_with_a_country_names_its_utf8_locale() {
    let preferred: PreferredLocale = PreferredLocale {
        language: "ja".to_string(),
        country: Some("JP".to_string()),
    };
    let name: Option<String> = preferred.utf8_name();
    assert_eq!(name.as_deref(), Some("ja_JP.UTF-8"));
}

#[test]
fn a_preferred_locale_without_a_country_names_no_locale() {
    // A language alone does not make a `language_COUNTRY` locale name.
    let preferred: PreferredLocale = PreferredLocale {
        language: "ja".to_string(),
        country: None,
    };
    let name: Option<String> = preferred.utf8_name();
    assert_eq!(name, None);
}

#[cfg(not(target_os = "macos"))]
#[test]
fn only_macos_has_a_preferred_locale_to_ask_for() {
    let preferred: Option<PreferredLocale> = preferred_locale();
    assert!(preferred.is_none());
}

#[cfg(target_os = "macos")]
#[test]
fn macos_reports_a_preferred_locale_with_a_language() {
    let preferred: Option<PreferredLocale> = preferred_locale();
    let preferred = preferred.expect("macOS always has a current locale");
    assert!(!preferred.language.is_empty());
}

#[cfg(unix)]
#[test]
fn the_password_database_names_an_absolute_login_shell_on_unix() {
    let shell: Option<std::path::PathBuf> = passwd_login_shell();
    let shell = shell.expect("the test user has a login shell in the password database");
    assert!(
        shell.is_absolute(),
        "a login shell from the password database is an absolute path"
    );
}

#[cfg(windows)]
#[test]
fn there_is_no_password_database_login_shell_on_windows() {
    let shell: Option<std::path::PathBuf> = passwd_login_shell();
    assert!(shell.is_none());
}
