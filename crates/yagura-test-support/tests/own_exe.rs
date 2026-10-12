#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// Integration-test target: clippy's `allow-unwrap-in-tests` /
// `allow-expect-in-tests` cover only `#[cfg(test)]` code, so a `tests/` file
// needs this header explicitly.

//! Contract tests for `yagura_test_support::process::own_canonical_exe`.
//!
//! Kept apart from `support_contract.rs`: while this API is undefined, a
//! reference to it fails the whole binary to compile, and folding these
//! cases into the shared contract binary would take every other fixture
//! test down with it. This file stays after the API lands, so the
//! isolation is a standing choice rather than a temporary scaffold.

use yagura_test_support::process::own_canonical_exe;

#[test]
fn own_canonical_exe_is_an_absolute_path_that_exists() {
    let path = own_canonical_exe();

    assert!(
        path.is_absolute(),
        "own_canonical_exe must return an absolute path: {path:?}"
    );
    assert!(
        path.is_file(),
        "own_canonical_exe must point at a file that exists: {path:?}"
    );
}

#[test]
fn own_canonical_exe_is_unchanged_by_re_canonicalizing_it() {
    // Detection power is OS-specific: on Windows, `canonicalize()` prepends
    // the `\\?\` extended-length prefix, so a caller that forgot to
    // canonicalize would fail this assertion there; on Unix,
    // `current_exe()` already returns a canonical path, so this assertion
    // alone would not catch that same omission on that platform.
    let path = own_canonical_exe();
    let recanonicalized = path
        .canonicalize()
        .expect("a path returned by own_canonical_exe must itself be re-canonicalizable");

    assert_eq!(
        path, recanonicalized,
        "re-canonicalizing an already-canonical path must be a no-op"
    );
}

#[test]
fn own_canonical_exe_matches_current_exe_canonicalized() {
    let expected = std::env::current_exe()
        .expect("current_exe should resolve for a running test binary")
        .canonicalize()
        .expect("current_exe path should be canonicalizable");

    assert_eq!(
        own_canonical_exe(),
        expected,
        "own_canonical_exe must agree with std::env::current_exe().canonicalize()"
    );
}
