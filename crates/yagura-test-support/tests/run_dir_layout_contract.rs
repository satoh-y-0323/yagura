#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// Integration-test target: clippy's `allow-unwrap-in-tests` /
// `allow-expect-in-tests` cover only `#[cfg(test)]` code, so a `tests/` file
// needs this header explicitly.

//! Contract tests for the names inside the run directories and for the Unix
//! socket fixtures living in them.
//!
//! Every fixture path, and every product message quoting one, carries the
//! run directory and the entry name, so both must stay short.

use std::path::Path;

use yagura_test_support::paths;

fn file_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or_else(|| panic!("path must have a file name: {path:?}"))
        .to_string_lossy()
        .into_owned()
}

#[test]
fn the_run_dir_name_is_the_pid_and_a_time_under_two_to_the_28_in_hex() {
    let dir = paths::unique_temp_dir("yagura-layout", "run-name");
    let run = dir.parent().expect("temp dir must have a parent");

    let name = file_name(run);
    let (pid, time) = name
        .split_once('-')
        .unwrap_or_else(|| panic!("the run dir name must be `{{pid:x}}-{{t:x}}`: {name:?}"));
    assert_eq!(
        pid,
        format!("{:x}", std::process::id()),
        "the run dir name must lead with this process's pid in hex: {name:?}"
    );
    let time = u32::from_str_radix(time, 16)
        .unwrap_or_else(|_| panic!("the run dir name must end in a hex time: {name:?}"));
    // A u32 pid takes at most 8 hex digits and a time under 2^28 at most 7.
    assert!(
        time < 1 << 28,
        "the time in the run dir name must stay under 2^28 to keep the name within 16 bytes: {name:?}"
    );
}

#[test]
fn an_entry_name_is_the_prefix_and_tag_followed_only_by_a_sequence_number() {
    let dir = paths::unique_temp_dir("yagura-layout", "entry-name");

    let name = file_name(&dir);
    let rest = name
        .strip_prefix("yagura-layout-entry-name-")
        .unwrap_or_else(|| panic!("the entry name must lead with prefix and tag: {name:?}"));
    assert!(
        !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()),
        "the run dir already separates processes, so the entry name must end in a bare \
         sequence number: {name:?}"
    );
}

#[cfg(unix)]
mod unix_sockets {
    use std::path::Path;

    use yagura_test_support::{ipc, paths};

    /// Asserts that `dir` sits directly in a run directory: a directory
    /// holding `owner.lock`, directly under a `yagura-tests` directory.
    fn assert_in_a_run_dir(dir: &Path) {
        let run = dir
            .parent()
            .unwrap_or_else(|| panic!("socket dir must have a parent: {dir:?}"));
        assert!(
            run.join("owner.lock").is_file(),
            "the socket dir must sit in a run dir holding owner.lock: {dir:?}"
        );
        let runs = run
            .parent()
            .unwrap_or_else(|| panic!("run dir must have a parent: {run:?}"));
        assert_eq!(
            runs.file_name().and_then(|name| name.to_str()),
            Some("yagura-tests"),
            "the run dir must sit directly under a yagura-tests dir: {dir:?}"
        );
    }

    #[test]
    fn unique_endpoint_name_sits_in_a_run_dir_within_the_endpoint_budget() {
        let name = ipc::unique_endpoint_name("yagura-layout", "endpoint");

        assert!(
            ipc::endpoint_candidate_within_budget(&name),
            "the endpoint must fit the endpoint budget: {name}"
        );
        let socket_dir = Path::new(&name)
            .parent()
            .unwrap_or_else(|| panic!("endpoint must have a parent: {name}"));
        assert!(
            socket_dir.is_dir(),
            "unique_endpoint_name must create the socket's parent dir: {name}"
        );
        assert_in_a_run_dir(socket_dir);
    }

    #[test]
    fn short_tmp_dir_sits_in_a_run_dir_within_the_endpoint_budget() {
        let dir = paths::short_tmp_dir("yagura-layout", "short");
        let text = dir.path().to_str().expect("utf8 path");

        assert!(
            ipc::endpoint_candidate_within_budget(text),
            "short_tmp_dir must fit the endpoint budget: {text}"
        );
        assert_in_a_run_dir(dir.path());
    }
}
