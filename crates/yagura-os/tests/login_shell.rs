#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: the login shell in the password database, behind the
//! child's base environment.

use yagura_os::login_shell::passwd_login_shell;

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
