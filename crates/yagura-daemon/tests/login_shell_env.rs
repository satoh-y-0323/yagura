#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: reading the login shell's environment at start-up and
//! resolving the child's base environment from it.
//!
//! The read runs the real `/bin/sh` against a temporary `HOME` holding a
//! `.profile`, so these cases are Unix-only; `/bin/sh` reads `.profile` as a
//! login shell on every Unix yagura supports. The start-up decisions that
//! never reach a shell (Windows, a terminal, no usable shell) run everywhere,
//! with the shell's existence and the locale's existence injected.

use std::ffi::{OsStr, OsString};
use std::path::PathBuf;
use std::time::Duration;

use tracing_test::traced_test;
use yagura_daemon::child_env::{
    ChildBaseEnv, ChildBaseEnvInputs, LOGIN_ENV_MAX_OUTPUT_BYTES, resolve_child_base_env,
};
use yagura_daemon::profile::ProfileOs;
use yagura_os::CaptureLimits;

fn limits(timeout: Duration) -> CaptureLimits {
    CaptureLimits {
        timeout,
        max_output_bytes: LOGIN_ENV_MAX_OUTPUT_BYTES,
    }
}

/// The whole words of a log line, without its leading timestamp. A word keeps
/// underscores, so the test's own span name (one long snake_case word) and
/// the `child_env` target never read as a short word such as `env`.
fn words(line: &str) -> Vec<&str> {
    let without_timestamp = line
        .trim_start()
        .split_once(char::is_whitespace)
        .map_or("", |(_, rest)| rest);
    without_timestamp
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter(|word| !word.is_empty())
        .collect()
}

fn logged_word(lines: &[&str], word: &str) -> bool {
    lines.iter().any(|line| words(line).contains(&word))
}

fn value<'a>(base: &'a ChildBaseEnv, name: &str) -> Option<&'a OsStr> {
    base.get(OsStr::new(name))
}

// ---------------------------------------------------------------------------
// Start-up decisions that never reach a shell (every host).
// ---------------------------------------------------------------------------

#[tokio::test]
#[traced_test]
async fn a_desktop_started_daemon_on_the_pc_host_reads_nothing_and_supplies_nothing() {
    let inputs: ChildBaseEnvInputs = ChildBaseEnvInputs {
        host_os: ProfileOs::Windows,
        term: None,
        shell_env: Some(PathBuf::from("/bin/sh")),
        passwd_shell: None,
        shell_exists: |_| true,
        inherited: Vec::new(),
        preferred_locale: Some("en_US.UTF-8".to_string()),
        locale_exists: |_| true,
        limits: limits(Duration::from_secs(5)),
    };
    let base: ChildBaseEnv = resolve_child_base_env(inputs).await;

    assert!(
        base.is_empty(),
        "Windows children keep the daemon's own environment: no read-back, no LANG"
    );
    logs_assert(|lines: &[&str]| {
        if !logged_word(lines, "windows") {
            return Err("the skip reason `windows` is logged".to_string());
        }
        if logged_word(lines, "terminal") || logged_word(lines, "no_shell") {
            return Err("only the reason that applied is logged".to_string());
        }
        Ok(())
    });
}

#[tokio::test]
#[traced_test]
async fn a_daemon_started_from_a_terminal_only_gets_a_locale_supplied() {
    let inputs: ChildBaseEnvInputs = ChildBaseEnvInputs {
        host_os: ProfileOs::Linux,
        term: Some(OsString::from("xterm-256color")),
        shell_env: Some(PathBuf::from("/bin/sh")),
        passwd_shell: None,
        shell_exists: |_| true,
        inherited: Vec::new(),
        preferred_locale: Some("xx_YY.UTF-8".to_string()),
        locale_exists: |name| name == "xx_YY.UTF-8",
        limits: limits(Duration::from_secs(5)),
    };
    let base: ChildBaseEnv = resolve_child_base_env(inputs).await;

    // A read of the real /bin/sh would add the shell's whole environment.
    assert_eq!(
        base.len(),
        1,
        "a terminal-started daemon reads no login shell, only a LANG is supplied"
    );
    assert_eq!(value(&base, "LANG"), Some(OsStr::new("xx_YY.UTF-8")));
    logs_assert(|lines: &[&str]| {
        if !logged_word(lines, "terminal") {
            return Err("the skip reason `terminal` is logged".to_string());
        }
        if logged_word(lines, "windows") || logged_word(lines, "no_shell") {
            return Err("only the reason that applied is logged".to_string());
        }
        Ok(())
    });
}

#[tokio::test]
#[traced_test]
async fn with_no_usable_shell_the_read_is_skipped_and_only_a_locale_is_supplied() {
    // Every candidate is named, and the injected check rejects them all --
    // including the /bin/sh fallback that exists on any real Unix host.
    let inputs: ChildBaseEnvInputs = ChildBaseEnvInputs {
        host_os: ProfileOs::Linux,
        term: None,
        shell_env: Some(PathBuf::from("/bin/sh")),
        passwd_shell: Some(PathBuf::from("/bin/zsh")),
        shell_exists: |_| false,
        inherited: Vec::new(),
        preferred_locale: None,
        locale_exists: |name| name == "C.UTF-8",
        limits: limits(Duration::from_secs(5)),
    };
    let base: ChildBaseEnv = resolve_child_base_env(inputs).await;

    assert_eq!(base.len(), 1, "no shell was read, only a LANG is supplied");
    assert_eq!(value(&base, "LANG"), Some(OsStr::new("C.UTF-8")));
    logs_assert(|lines: &[&str]| {
        if !logged_word(lines, "no_shell") {
            return Err("the skip reason `no_shell` is logged".to_string());
        }
        if logged_word(lines, "windows") || logged_word(lines, "terminal") {
            return Err("only the reason that applied is logged".to_string());
        }
        Ok(())
    });
}

#[tokio::test]
async fn a_locale_the_daemon_inherited_is_kept_whatever_other_variables_it_inherited() {
    let inputs: ChildBaseEnvInputs = ChildBaseEnvInputs {
        host_os: ProfileOs::Linux,
        term: Some(OsString::from("xterm-256color")),
        shell_env: None,
        passwd_shell: None,
        shell_exists: |_| false,
        inherited: vec![
            (OsString::from("LANG"), OsString::from("ja_JP.UTF-8")),
            (OsString::from("INHERITED_EMPTY"), OsString::new()),
        ],
        preferred_locale: Some("xx_YY.UTF-8".to_string()),
        locale_exists: |_| true,
        limits: limits(Duration::from_secs(5)),
    };
    let base: ChildBaseEnv = resolve_child_base_env(inputs).await;

    assert!(
        base.is_empty(),
        "an inherited LANG declares the locale, so none is supplied; got {} variables",
        base.len()
    );
}

// ---------------------------------------------------------------------------
// Reading a real login shell (Unix only).
// ---------------------------------------------------------------------------

#[cfg(unix)]
mod unix {
    use std::ffi::{OsStr, OsString};
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    use tracing_test::traced_test;
    use yagura_daemon::child_env::{
        ChildBaseEnv, ChildBaseEnvInputs, LoginEnvError, LoginShellEnv, env_for_login_shell,
        filter_login_env, read_login_shell_env, resolve_child_base_env,
    };
    use yagura_daemon::profile::ProfileOs;
    use yagura_test_support::paths::TempDir;

    use super::{limits, logged_word, value, words};

    const SHELL: &str = "/bin/sh";

    /// How much longer than its own timeout a read may take to return before
    /// the test gives up on it.
    const RETURN_SLACK: Duration = Duration::from_secs(3);

    const READ_TIMEOUT: Duration = Duration::from_secs(10);

    /// Variables that would point the shell at the developer's own rc files
    /// instead of the fixture's.
    const RC_REDIRECTING_NAMES: [&str; 4] = ["HOME", "ENV", "BASH_ENV", "ZDOTDIR"];

    const LOCALE_NAMES: [&str; 3] = ["LANG", "LC_ALL", "LC_CTYPE"];

    fn home_with_profile(label: &str, profile: &str) -> TempDir {
        let home = TempDir::new("yagura-login-shell-env", label);
        home.write_file(".profile", profile);
        home
    }

    /// This process's environment with `home` as `HOME`, without the names in
    /// `dropped`, plus `extra`. Built as a list so the process environment is
    /// never written.
    fn env_with_home(
        home: &Path,
        dropped: &[&str],
        extra: &[(&str, &str)],
    ) -> Vec<(OsString, OsString)> {
        let mut vars: Vec<(OsString, OsString)> = std::env::vars_os()
            .filter(|(name, _)| {
                !RC_REDIRECTING_NAMES
                    .iter()
                    .chain(dropped)
                    .any(|excluded| name == OsStr::new(excluded))
            })
            .collect();
        vars.push((OsString::from("HOME"), home.as_os_str().to_os_string()));
        vars.extend(
            extra
                .iter()
                .map(|(name, value)| (OsString::from(name), OsString::from(value))),
        );
        vars
    }

    fn var<'a>(vars: &'a [(OsString, OsString)], name: &str) -> Option<&'a OsStr> {
        vars.iter()
            .rev()
            .find(|(existing, _)| existing == OsStr::new(name))
            .map(|(_, value)| value.as_os_str())
    }

    /// Runs `run` on its own thread and runtime, failing the test when it has
    /// not returned within `outer`, so a read that never ends cannot hang the
    /// suite.
    fn run_within<T: Send + 'static>(
        outer: Duration,
        run: impl std::future::Future<Output = T> + Send + 'static,
    ) -> (T, Duration) {
        let (tx, rx) = std::sync::mpsc::channel::<T>();
        let started = Instant::now();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("build a runtime for the read");
            let _ = tx.send(runtime.block_on(run));
        });
        match rx.recv_timeout(outer) {
            Ok(value) => (value, started.elapsed()),
            Err(_) => panic!("the read had not returned after {outer:?}"),
        }
    }

    fn read(
        shell: &Path,
        env: Vec<(OsString, OsString)>,
        timeout: Duration,
    ) -> (Result<LoginShellEnv, LoginEnvError>, Duration) {
        let shell: PathBuf = shell.to_path_buf();
        run_within(timeout + RETURN_SLACK, async move {
            read_login_shell_env(&shell, env, limits(timeout)).await
        })
    }

    fn expect_read(result: Result<LoginShellEnv, LoginEnvError>) -> LoginShellEnv {
        match result {
            Ok(read) => read,
            Err(error) => panic!("the login shell read failed with `{}`", error.kind()),
        }
    }

    fn expect_error(result: Result<LoginShellEnv, LoginEnvError>) -> LoginEnvError {
        match result {
            Ok(_) => panic!("the login shell read unexpectedly succeeded"),
            Err(error) => error,
        }
    }

    #[test]
    fn a_variable_the_profile_exports_is_read_back_with_newlines_and_equals_intact() {
        let home = home_with_profile(
            "exported",
            "export LOGIN_PROBE_VALUE='first line\nsecond=line'\n",
        );
        let env = env_with_home(home.path(), &[], &[]);

        let (result, _) = read(Path::new(SHELL), env, READ_TIMEOUT);
        let read = expect_read(result);

        assert_eq!(
            var(&read.vars, "LOGIN_PROBE_VALUE"),
            Some(OsStr::new("first line\nsecond=line"))
        );
    }

    #[test]
    fn the_shell_runs_in_home_and_not_in_another_directory_its_environment_names() {
        let home = home_with_profile("cwd", "export LOGIN_CWD_PROBE=\"$(pwd -P)\"\n");
        // Named last, so a lookup that took any variable but HOME would land here.
        let elsewhere = TempDir::new("yagura-login-shell-env", "cwd-elsewhere");
        let elsewhere_dir = elsewhere.path().to_str().unwrap();
        let env = env_with_home(home.path(), &[], &[("LOGIN_OTHER_DIR", elsewhere_dir)]);

        let (result, _) = read(Path::new(SHELL), env, READ_TIMEOUT);
        let read = expect_read(result);

        let home_dir = std::fs::canonicalize(home.path()).unwrap();
        assert_eq!(
            var(&read.vars, "LOGIN_CWD_PROBE"),
            Some(home_dir.as_os_str())
        );
    }

    #[test]
    fn rc_files_printing_megabytes_around_the_environment_are_still_read() {
        // Two MiB of rc output, more than any environment `exec` accepts.
        let home = home_with_profile(
            "large-output",
            "/bin/dd if=/dev/zero bs=1024 count=2048 2>/dev/null | /usr/bin/tr '\\000' x\n\
             export LOGIN_PROBE_VALUE=after-large-output\n",
        );
        let env = env_with_home(home.path(), &[], &[]);

        let (result, _) = read(Path::new(SHELL), env, READ_TIMEOUT);
        let read = expect_read(result);

        assert_eq!(
            var(&read.vars, "LOGIN_PROBE_VALUE"),
            Some(OsStr::new("after-large-output"))
        );
    }

    #[test]
    fn a_background_process_holding_stdout_does_not_hold_up_the_read() {
        // The sleep outlives the timeout, so a read that waited for stdout to
        // close would time out; it still ends on its own.
        let home = home_with_profile(
            "background",
            "sleep 20 &\nexport LOGIN_PROBE_VALUE=after-background\n",
        );
        let env = env_with_home(home.path(), &[], &[]);

        let (result, took) = read(Path::new(SHELL), env, READ_TIMEOUT);
        let read = expect_read(result);

        assert_eq!(
            var(&read.vars, "LOGIN_PROBE_VALUE"),
            Some(OsStr::new("after-background"))
        );
        assert!(
            took < READ_TIMEOUT / 2,
            "the read completes at the end marker, well before its timeout (took {took:?})"
        );
    }

    #[test]
    fn the_shells_exit_trap_has_run_by_the_time_the_read_returns() {
        // The trap takes a second before writing, so ending the tree as soon
        // as the end marker is read would leave no file behind.
        let home = home_with_profile(
            "exit-trap",
            "trap 'sleep 1; : > \"$HOME/exit-trap-ran\"' EXIT\n",
        );
        let marker_file = home.path().join("exit-trap-ran");
        let env = env_with_home(home.path(), &[], &[]);

        let (result, _) = read(Path::new(SHELL), env, READ_TIMEOUT);
        let read = expect_read(result);

        assert!(
            marker_file.exists(),
            "the shell's own exit handling ran before the read returned"
        );
        assert!(read.shell_exited, "the shell exited on its own");
    }

    #[test]
    fn a_profile_slower_than_the_timeout_times_out_within_its_bound() {
        let timeout = Duration::from_secs(2);
        let home = home_with_profile("slow", "sleep 30\n");
        let env = env_with_home(home.path(), &[], &[]);

        let (result, took) = read(Path::new(SHELL), env, timeout);
        let error = expect_error(result);

        assert!(matches!(error, LoginEnvError::TimedOut));
        assert_eq!(error.kind(), "timed_out");
        assert!(
            took < timeout + RETURN_SLACK,
            "a timed-out read returns shortly after its timeout (took {took:?})"
        );
    }

    #[test]
    fn a_program_that_prints_no_markers_is_reported_as_such() {
        // An existing executable that ignores the arguments and prints nothing.
        let silent = Path::new("/usr/bin/true");
        assert!(silent.exists(), "the fixture program exists");
        let home = home_with_profile("no-markers", "");
        let env = env_with_home(home.path(), &[], &[]);

        let (result, _) = read(silent, env, READ_TIMEOUT);

        assert_eq!(expect_error(result).kind(), "no_markers");
    }

    #[test]
    fn a_shell_that_does_not_exist_is_reported_as_a_spawn_failure() {
        let home = home_with_profile("missing-shell", "");
        let missing = home.path().join("no-such-shell");
        let env = env_with_home(home.path(), &[], &[]);

        let (result, _) = read(&missing, env, READ_TIMEOUT);

        assert_eq!(expect_error(result).kind(), "spawn_failed");
    }

    #[test]
    fn the_rc_files_see_the_inherited_environment_but_no_reserved_name() {
        // The rc copies what it saw into unreserved names so the result can
        // carry it back.
        let home = home_with_profile(
            "reserved",
            "export SEEN_RESERVED_PROBE=\"${YAGURA_LOGIN_PROBE-unset}\"\n\
             export SEEN_PLAIN_PROBE=\"${LOGIN_PLAIN_PROBE-unset}\"\n",
        );
        let inherited = env_with_home(
            home.path(),
            &[],
            &[
                ("YAGURA_LOGIN_PROBE", "reserved-value"),
                ("LOGIN_PLAIN_PROBE", "plain-value"),
            ],
        );
        let inputs: ChildBaseEnvInputs = ChildBaseEnvInputs {
            host_os: ProfileOs::host(),
            term: None,
            shell_env: Some(PathBuf::from(SHELL)),
            passwd_shell: None,
            shell_exists: |path| path.exists(),
            inherited,
            preferred_locale: None,
            locale_exists: |_| false,
            limits: limits(READ_TIMEOUT),
        };

        let (base, _): (ChildBaseEnv, Duration) =
            run_within(READ_TIMEOUT + RETURN_SLACK, resolve_child_base_env(inputs));

        assert_eq!(
            value(&base, "SEEN_PLAIN_PROBE"),
            Some(OsStr::new("plain-value"))
        );
        assert_eq!(
            value(&base, "SEEN_RESERVED_PROBE"),
            Some(OsStr::new("unset"))
        );
    }

    const SENTINEL_NAME: &str = "LOGIN_SENTINEL_NAME_QZ";
    const SENTINEL_VALUE: &str = "login-sentinel-value-qz";

    /// A home whose `.profile` exports the sentinel (then runs `tail`), and a
    /// uniquely named link to `/bin/sh` inside it, so the shell's path is a
    /// string no other log line could contain.
    fn sentinel_home(label: &str, tail: &str) -> (TempDir, PathBuf) {
        let home = home_with_profile(
            label,
            &format!("export {SENTINEL_NAME}={SENTINEL_VALUE}\n{tail}"),
        );
        let shell = home.path().join("probe-login-shell");
        std::os::unix::fs::symlink(SHELL, &shell).expect("link the probe shell");
        (home, shell)
    }

    #[tokio::test]
    #[traced_test]
    async fn a_successful_resolve_carries_the_profile_and_logs_only_the_source_and_count() {
        let (home, shell) = sentinel_home("logged-success", "");
        let inherited = env_with_home(home.path(), &[], &[]);

        // The same read on its own gives the counts a log line may report.
        let independent = read_login_shell_env(
            &shell,
            env_for_login_shell(inherited.clone()),
            limits(READ_TIMEOUT),
        )
        .await;
        let independent = expect_read(independent);

        let inputs: ChildBaseEnvInputs = ChildBaseEnvInputs {
            host_os: ProfileOs::host(),
            term: None,
            shell_env: Some(shell.clone()),
            passwd_shell: None,
            shell_exists: |path| path.exists(),
            inherited,
            preferred_locale: None,
            locale_exists: |_| false,
            limits: limits(READ_TIMEOUT),
        };
        let base: ChildBaseEnv = resolve_child_base_env(inputs).await;

        assert_eq!(
            value(&base, SENTINEL_NAME),
            Some(OsStr::new(SENTINEL_VALUE))
        );
        // rc files may export a variable only on their first run in a fresh
        // HOME, so the two reads can differ in what is kept. What is dropped
        // (the read's own shell depth, working directory, ...) does not, and
        // no LANG is supplied here, so the resolve's read-back is exactly
        // `base` plus that many dropped variables.
        let dropped: usize =
            independent.vars.len() - filter_login_env(independent.vars.clone()).len();
        let counts: Vec<String> = [base.len(), base.len() + dropped]
            .iter()
            .map(ToString::to_string)
            .collect();
        logs_assert(|lines: &[&str]| {
            let source_lines: Vec<&&str> = lines
                .iter()
                .filter(|line| words(line).contains(&"env"))
                .collect();
            if source_lines.is_empty() {
                return Err("the shell's source `env` is logged".to_string());
            }
            if logged_word(lines, "passwd") || logged_word(lines, "fallback") {
                return Err("only the source that was used is logged".to_string());
            }
            let has_count = source_lines.iter().any(|line| {
                words(line)
                    .iter()
                    .any(|word| counts.iter().any(|count| count == word))
            });
            if !has_count {
                return Err(format!(
                    "the success line logs the number of variables (one of {counts:?})"
                ));
            }
            Ok(())
        });
        assert!(!logs_contain(SENTINEL_NAME), "no variable name is logged");
        assert!(!logs_contain(SENTINEL_VALUE), "no variable value is logged");
        assert!(
            !logs_contain("probe-login-shell"),
            "the shell's path is never logged"
        );
    }

    #[tokio::test]
    #[traced_test]
    async fn a_timed_out_resolve_supplies_only_a_locale_and_logs_only_the_kind_and_source() {
        let timeout = Duration::from_secs(2);
        let (home, shell) = sentinel_home("logged-timeout", "sleep 30\n");
        let inherited = env_with_home(home.path(), &LOCALE_NAMES, &[]);
        let inputs: ChildBaseEnvInputs = ChildBaseEnvInputs {
            host_os: ProfileOs::host(),
            term: None,
            shell_env: None,
            passwd_shell: Some(shell.clone()),
            shell_exists: |path| path.exists(),
            inherited,
            preferred_locale: None,
            locale_exists: |name| name == "en_US.UTF-8",
            limits: limits(timeout),
        };

        let base: ChildBaseEnv = resolve_child_base_env(inputs).await;

        assert_eq!(
            base.len(),
            1,
            "a failed read contributes nothing, only a LANG is supplied"
        );
        assert_eq!(value(&base, "LANG"), Some(OsStr::new("en_US.UTF-8")));
        logs_assert(|lines: &[&str]| {
            if !logged_word(lines, "timed_out") {
                return Err("the failure kind `timed_out` is logged".to_string());
            }
            if !logged_word(lines, "passwd") {
                return Err("the shell's source `passwd` is logged".to_string());
            }
            Ok(())
        });
        assert!(!logs_contain(SENTINEL_NAME), "no variable name is logged");
        assert!(!logs_contain(SENTINEL_VALUE), "no variable value is logged");
        assert!(
            !logs_contain("probe-login-shell"),
            "the shell's path is never logged"
        );
    }
}
