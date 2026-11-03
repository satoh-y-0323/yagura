#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: `holder_endpoint` is the only gate keeping a window id
//! inside its directory (Unix) or pipe namespace (Windows).

fn endpoint_for(window_id: &str) -> Result<String, yagura_os::OsError> {
    let namespace = yagura_os::resolve_instance_from(None).unwrap();
    yagura_os::holder_endpoint(
        "1000",
        &namespace,
        std::path::Path::new("holders-not-created"),
        window_id,
    )
}

#[test]
fn ids_that_are_not_one_plain_path_component_are_rejected() {
    let mut bad = vec!["", ".", "..", "a/b", "/a", "a/", "../a", "a\0b", "\0"];
    // A backslash separates components, and `C:` is a prefix, on Windows only.
    if cfg!(windows) {
        bad.extend([
            r"a\b",
            r"\a",
            "C:",
            "C:a",
            r"C:\a",
            r"\\server\share",
            r"\\?\C:\a",
        ]);
    }
    for id in bad {
        assert!(
            matches!(endpoint_for(id), Err(yagura_os::OsError::Security(_))),
            "{id:?} must be rejected"
        );
    }
}

#[test]
fn ordinary_window_ids_are_accepted() {
    for id in ["w1", "1", "window-7", "a.b", "0123456789abcdef"] {
        assert!(endpoint_for(id).is_ok(), "{id:?} must be accepted");
    }
}
