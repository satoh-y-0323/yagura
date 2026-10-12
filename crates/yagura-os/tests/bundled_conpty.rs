#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg(windows)]

//! `bundled_conpty::verify_beside`: a directory holds the bundled pseudoconsole
//! (`conpty.dll` and `OpenConsole.exe`) only when both files are there and their
//! contents match the pinned hashes.

use std::error::Error as _;
use std::path::{Path, PathBuf};

use yagura_os::bundled_conpty::verify_beside;
use yagura_test_support::paths::TempDir;

const DLL: &str = "conpty.dll";
const HOST: &str = "OpenConsole.exe";

fn vendored_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../vendor/conpty/x64")
}

fn copy_vendored(into: &Path, name: &str) {
    std::fs::copy(vendored_dir().join(name), into.join(name))
        .unwrap_or_else(|e| panic!("copying vendored {name} should succeed: {e}"));
}

/// Flips one byte in the middle of the file, keeping its length.
fn corrupt(path: &Path) {
    let mut bytes = std::fs::read(path).expect("file to corrupt should be readable");
    let middle = bytes.len() / 2;
    bytes[middle] ^= 0xff;
    std::fs::write(path, bytes).expect("corrupted file should be writable");
}

#[test]
fn a_copy_of_the_vendored_pair_verifies() {
    let dir = TempDir::new("yagura-bundled-conpty", "ok");
    copy_vendored(dir.path(), DLL);
    copy_vendored(dir.path(), HOST);

    let result = verify_beside(dir.path());
    assert!(
        result.is_ok(),
        "a copy of vendor/conpty/x64 must verify: {:?}",
        result.err()
    );
}

#[test]
fn an_empty_directory_is_rejected() {
    let dir = TempDir::new("yagura-bundled-conpty", "empty");

    assert!(verify_beside(dir.path()).is_err());
}

#[test]
fn a_missing_directory_is_rejected() {
    let dir = TempDir::new("yagura-bundled-conpty", "missing");
    let missing = dir.path().join("does-not-exist");

    assert!(verify_beside(&missing).is_err());
}

#[test]
fn the_dll_alone_is_rejected() {
    let dir = TempDir::new("yagura-bundled-conpty", "dll-only");
    copy_vendored(dir.path(), DLL);

    assert!(verify_beside(dir.path()).is_err());
}

#[test]
fn the_host_alone_is_rejected() {
    let dir = TempDir::new("yagura-bundled-conpty", "host-only");
    copy_vendored(dir.path(), HOST);

    assert!(verify_beside(dir.path()).is_err());
}

#[test]
fn a_dll_with_other_contents_is_rejected() {
    let dir = TempDir::new("yagura-bundled-conpty", "dll-mismatch");
    copy_vendored(dir.path(), DLL);
    copy_vendored(dir.path(), HOST);
    corrupt(&dir.path().join(DLL));

    assert!(verify_beside(dir.path()).is_err());
}

#[test]
fn a_host_with_other_contents_is_rejected() {
    let dir = TempDir::new("yagura-bundled-conpty", "host-mismatch");
    copy_vendored(dir.path(), DLL);
    copy_vendored(dir.path(), HOST);
    corrupt(&dir.path().join(HOST));

    assert!(verify_beside(dir.path()).is_err());
}

#[test]
fn the_two_files_swapped_are_rejected() {
    let dir = TempDir::new("yagura-bundled-conpty", "swapped");
    std::fs::copy(vendored_dir().join(DLL), dir.path().join(HOST)).unwrap();
    std::fs::copy(vendored_dir().join(HOST), dir.path().join(DLL)).unwrap();

    assert!(verify_beside(dir.path()).is_err());
}

#[test]
fn the_error_type_is_the_modules_own() {
    let dir = TempDir::new("yagura-bundled-conpty", "error-type");

    let err: yagura_os::bundled_conpty::BundledConptyError =
        verify_beside(dir.path()).expect_err("an empty directory must be rejected");
    assert!(
        !err.to_string().is_empty(),
        "the error must say what is wrong"
    );
}

#[test]
fn a_missing_file_is_named_and_keeps_its_read_error_as_the_cause() {
    let dir = TempDir::new("yagura-bundled-conpty", "report-missing");

    let err = verify_beside(dir.path()).expect_err("an empty directory must be rejected");

    let text = err.to_string();
    assert!(text.contains(DLL), "the message must name the file: {text}");
    assert!(
        text.contains("cannot be read"),
        "the message must say why: {text}"
    );
    let fault = err.source().expect("the error wraps the pin fault");
    assert!(
        fault.source().is_some(),
        "an unreadable file must keep the read error as its cause"
    );
}

#[test]
fn an_altered_file_is_named_with_the_hash_it_has() {
    use sha2::{Digest, Sha256};

    let dir = TempDir::new("yagura-bundled-conpty", "report-altered");
    copy_vendored(dir.path(), DLL);
    copy_vendored(dir.path(), HOST);
    corrupt(&dir.path().join(HOST));
    let actual: String = Sha256::digest(std::fs::read(dir.path().join(HOST)).unwrap())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();

    let err = verify_beside(dir.path()).expect_err("an altered host must be rejected");

    let text = err.to_string();
    assert!(
        text.contains(&format!("{HOST} is not the pinned file (sha256 {actual})")),
        "the message must name the file and the hash it has: {text}"
    );
    let fault = err.source().expect("the error wraps the pin fault");
    assert!(
        fault.source().is_none(),
        "a mismatch has no underlying error"
    );
}
