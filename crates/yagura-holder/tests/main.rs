//! Every integration test of this crate, built as one test binary: each file in
//! this directory is a module listed below, and a file not listed is not built.

mod holder;
#[cfg(target_os = "linux")]
mod orphan_reap_linux;
mod terminal;
#[cfg(unix)]
mod watcher;
mod watcher_proto;
