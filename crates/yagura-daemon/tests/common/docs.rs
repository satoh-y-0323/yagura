//! Repository text reading shared by this crate's tests that measure the
//! files the repository ships (bundled profiles, MCP registration, docs).
//! No `#[test]` lives in this module: it is compiled into every integration
//! test binary through `mod common;`, so a test placed here would run once
//! per binary. `read_doc_verbatim`'s self-test lives in
//! `launch_option_docs.rs`.

use std::path::PathBuf;

/// Reads a repository text file relative to this crate's manifest
/// directory, verbatim -- no comment stripping, since a caller measuring
/// prose or scanning source vocabulary wants the literal file text.
/// `rel_from_manifest` is anchored at `CARGO_MANIFEST_DIR`, e.g.
/// `"../../docs/mcp-setup.md"` or `"src/profile.rs"`.
///
/// # Panics
///
/// Panics if the file cannot be read: a doc-contract test built on a missing
/// file must fail loud rather than silently pass on empty text.
#[must_use]
pub fn read_doc_verbatim(rel_from_manifest: &str) -> String {
    let path: PathBuf =
        yagura_test_support::scan::resolve_from(env!("CARGO_MANIFEST_DIR"), rel_from_manifest);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("contract violation: cannot read {}: {e}", path.display()))
}
