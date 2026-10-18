#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! A hook command cannot start with a path that holds whitespace, so the
//! daemon's path is written in a whitespace-free form or not at all.

use yagura_os::{SpacelessPathError, spaceless_path, spaceless_path_from};

#[test]
fn a_path_without_whitespace_is_kept_as_it_is() {
    assert_eq!(
        spaceless_path_from(
            r"C:\Users\example\bin\yagura-daemon.exe",
            Some(r"C:\X~1\Y.EXE")
        ),
        Some(r"C:\Users\example\bin\yagura-daemon.exe".to_string()),
        "a long path that needs no replacing must win over its short name"
    );
}

#[test]
fn a_path_with_whitespace_is_replaced_by_its_short_name() {
    assert_eq!(
        spaceless_path_from(
            r"C:\Users\Example User\bin\yagura-daemon.exe",
            Some(r"C:\Users\EXAMPL~1\bin\YAGURA~1.EXE")
        ),
        Some(r"C:\Users\EXAMPL~1\bin\YAGURA~1.EXE".to_string())
    );
}

#[test]
fn a_short_name_that_still_holds_whitespace_is_refused() {
    let long = r"C:\Users\Example User\bin\yagura-daemon.exe";
    assert_eq!(spaceless_path_from(long, Some(long)), None);
    assert_eq!(spaceless_path_from(long, Some("C:\\a b\\c")), None);
}

#[test]
fn a_path_with_whitespace_and_no_short_name_is_refused() {
    assert_eq!(
        spaceless_path_from(
            "/Users/Example User/.local/state/yagura/bin/yagura-daemon",
            None
        ),
        None
    );
    assert_eq!(spaceless_path_from("C:\\a\tb", None), None);
    assert_eq!(spaceless_path_from("", None), None);
}

/// A fresh directory whose name holds a space, with a file in it.
fn spaced_file(tag: &str) -> std::path::PathBuf {
    let dir =
        yagura_test_support::paths::unique_temp_dir("yagura-hook-path", tag).join("with space");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("hook daemon.exe");
    std::fs::write(&file, b"the daemon's image").unwrap();
    file
}

#[test]
fn a_path_without_whitespace_is_returned_unchanged() {
    let dir = yagura_test_support::paths::unique_temp_dir("yagura-hook-path", "plain");
    assert_eq!(spaceless_path(&dir), Ok(dir.to_str().unwrap().to_string()));
}

#[cfg(windows)]
#[test]
fn windows_names_a_spaced_file_by_its_short_name_or_refuses_it() {
    let file = spaced_file("short-name");
    match spaceless_path(&file) {
        Ok(short) => {
            assert!(
                !short.contains(char::is_whitespace),
                "the short name must hold no whitespace: {short:?}"
            );
            assert_eq!(
                std::fs::read(&short).unwrap(),
                b"the daemon's image",
                "the short name {short:?} must name the same file"
            );
            assert_eq!(
                std::fs::canonicalize(&short).unwrap(),
                std::fs::canonicalize(&file).unwrap()
            );
        }
        // A volume with 8.3 names turned off.
        Err(err) => assert_eq!(err, SpacelessPathError::NoShortName),
    }
}

#[cfg(windows)]
#[test]
fn windows_refuses_a_spaced_path_that_does_not_exist() {
    let file = spaced_file("missing").with_file_name("not here.exe");
    assert_eq!(spaceless_path(&file), Err(SpacelessPathError::NoShortName));
}

#[cfg(not(windows))]
#[test]
fn unix_refuses_a_spaced_path() {
    let file = spaced_file("unix");
    assert_eq!(spaceless_path(&file), Err(SpacelessPathError::Whitespace));
}

#[test]
fn the_refusal_says_why_the_path_cannot_be_written() {
    let message = SpacelessPathError::NoShortName.to_string();
    assert!(
        message.contains("whitespace") && message.contains("8.3"),
        "{message}"
    );
    assert!(
        SpacelessPathError::Whitespace
            .to_string()
            .contains("whitespace")
    );
}
