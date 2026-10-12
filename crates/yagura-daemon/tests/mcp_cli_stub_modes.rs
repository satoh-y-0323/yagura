#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Self-check for the `mcp_cli_stub` test fixture (`src/bin/mcp_cli_stub.rs`):
//! every mode the fixture documents is exercised here, each paired with a
//! contrast that shows the alternative behavior. This file does not test
//! any product code -- the fixture is the thing under test.
//!
//! Spawned through `yagura_os::spawn_captured`, the same one-shot capture
//! path the daemon's own registration mechanism (a later layer) drives the
//! fixture through.

mod common;

use std::ffi::OsString;
use std::path::Path;
use std::time::{Duration, Instant};

use yagura_os::{CaptureEnv, CaptureLimits, CaptureOutcome};

fn stub_argv(mode_args: &[&str]) -> Vec<String> {
    let mut argv = vec![
        common::mcp_cli_stub_path()
            .to_str()
            .expect("mcp_cli_stub path must be valid UTF-8")
            .to_string(),
    ];
    argv.extend(mode_args.iter().map(|s| (*s).to_string()));
    argv
}

// The fixture is exercised without the daemon, so its environment layering does not apply.
#[allow(clippy::disallowed_methods)]
fn run(argv: &[String], vars: &[(&str, &str)], stdin: Option<Vec<u8>>) -> CaptureOutcome {
    let env_pairs: Vec<(OsString, OsString)> = vars
        .iter()
        .map(|(key, value)| (OsString::from(key), OsString::from(value)))
        .collect();
    let child = yagura_os::spawn_captured(argv, None, CaptureEnv::Inherit(&env_pairs), stdin)
        .expect("spawn should succeed");
    child.collect(CaptureLimits {
        timeout: Duration::from_secs(10),
        max_output_bytes: 1 << 20,
    })
}

fn assert_exit(outcome: &CaptureOutcome, expected_code: i32) {
    match outcome {
        CaptureOutcome::Exited { code, .. } => assert_eq!(*code, Some(expected_code)),
        other => panic!("expected Exited(code = Some({expected_code})), got {other:?}"),
    }
}

fn exited_stdout(outcome: CaptureOutcome, expected_code: i32) -> String {
    match outcome {
        CaptureOutcome::Exited { code, stdout, .. } => {
            assert_eq!(code, Some(expected_code));
            String::from_utf8(stdout).expect("stdout must be UTF-8")
        }
        other => panic!("expected Exited(code = Some({expected_code})), got {other:?}"),
    }
}

fn exited_stdout_bytes(outcome: CaptureOutcome, expected_code: i32) -> Vec<u8> {
    match outcome {
        CaptureOutcome::Exited { code, stdout, .. } => {
            assert_eq!(code, Some(expected_code));
            stdout
        }
        other => panic!("expected Exited(code = Some({expected_code})), got {other:?}"),
    }
}

fn state_var(dir: &Path) -> (&'static str, &str) {
    (
        "MCP_CLI_STUB_STATE_DIR",
        dir.to_str().expect("state dir path must be valid UTF-8"),
    )
}

fn unique_state_dir(tag: &str) -> std::path::PathBuf {
    common::unique_temp_dir(tag)
}

fn add(state: &Path, name: &str, command: &str, extra_vars: &[(&str, &str)]) {
    let mut vars = vec![state_var(state)];
    vars.extend_from_slice(extra_vars);
    let outcome = run(
        &stub_argv(&["mcp", "add", name, "--", command]),
        &vars,
        None,
    );
    assert_exit(&outcome, 0);
}

fn get_json(state: &Path, name: &str, extra_vars: &[(&str, &str)]) -> serde_json::Value {
    let mut vars = vec![state_var(state)];
    vars.extend_from_slice(extra_vars);
    let outcome = run(&stub_argv(&["mcp", "get", name, "--json"]), &vars, None);
    let stdout = exited_stdout(outcome, 0);
    serde_json::from_str(&stdout).expect("get --json must print JSON")
}

// ---------------------------------------------------------------------
// mcp add / mcp get: a normal registration is read back, an unregistered
// name is refused.
// ---------------------------------------------------------------------

#[test]
fn add_then_get_json_reports_the_registered_command_and_args() {
    let state = unique_state_dir("add-get-json");
    let outcome = run(
        &stub_argv(&["mcp", "add", "yagura", "--", "prog", "arg1", "arg2"]),
        &[state_var(&state)],
        None,
    );
    assert_exit(&outcome, 0);

    let body = get_json(&state, "yagura", &[]);
    assert_eq!(body["transport"]["command"].as_str(), Some("prog"));
    assert_eq!(
        body["transport"]["args"].clone(),
        serde_json::json!(["arg1", "arg2"])
    );
    assert!(
        body.get("tool_timeout_sec").is_none(),
        "no TOML table path was configured, so no tool_timeout_sec should appear"
    );
}

#[test]
fn get_for_an_unregistered_name_writes_nothing_to_stdout_and_a_fixed_stderr_line() {
    let state = unique_state_dir("get-unregistered");
    let outcome = run(
        &stub_argv(&["mcp", "get", "yagura", "--json"]),
        &[state_var(&state)],
        None,
    );
    match outcome {
        CaptureOutcome::Exited {
            code,
            stdout,
            stderr,
        } => {
            assert_eq!(code, Some(1));
            assert!(stdout.is_empty());
            assert_eq!(
                String::from_utf8_lossy(&stderr),
                "No MCP server named 'yagura' found.\n"
            );
        }
        other => panic!("expected Exited, got {other:?}"),
    }
}

#[test]
fn get_without_json_writes_the_line_form() {
    let state = unique_state_dir("get-lines");
    add(&state, "yagura", "prog", &[]);
    let outcome = run(
        &stub_argv(&["mcp", "get", "yagura"]),
        &[state_var(&state)],
        None,
    );
    let stdout = exited_stdout(outcome, 0);
    assert!(stdout.contains("  Scope: User config (available in all your projects)\n"));
    assert!(stdout.contains("  Command: prog\n"));
}

// ---------------------------------------------------------------------
// mcp add: a duplicate name is refused by default, and only overwritten
// when MCP_CLI_STUB_ON_DUPLICATE=overwrite is set.
// ---------------------------------------------------------------------

#[test]
fn add_refuses_a_duplicate_name_by_default_and_leaves_the_registration_untouched() {
    let state = unique_state_dir("dup-refuse");
    add(&state, "yagura", "first", &[]);

    let outcome = run(
        &stub_argv(&["mcp", "add", "yagura", "--", "second"]),
        &[state_var(&state)],
        None,
    );
    match outcome {
        CaptureOutcome::Exited {
            code,
            stdout,
            stderr,
        } => {
            assert_eq!(code, Some(1));
            assert!(stdout.is_empty());
            assert!(String::from_utf8_lossy(&stderr).contains("already exists"));
        }
        other => panic!("expected Exited, got {other:?}"),
    }

    let body = get_json(&state, "yagura", &[]);
    assert_eq!(
        body["transport"]["command"].as_str(),
        Some("first"),
        "a refused duplicate must not change the existing registration"
    );
}

#[test]
fn add_overwrites_a_duplicate_name_when_on_duplicate_is_overwrite() {
    let state = unique_state_dir("dup-overwrite");
    add(&state, "yagura", "first", &[]);

    let outcome = run(
        &stub_argv(&["mcp", "add", "yagura", "--", "second"]),
        &[
            state_var(&state),
            ("MCP_CLI_STUB_ON_DUPLICATE", "overwrite"),
        ],
        None,
    );
    assert_exit(&outcome, 0);

    let body = get_json(&state, "yagura", &[]);
    assert_eq!(body["transport"]["command"].as_str(), Some("second"));
}

// ---------------------------------------------------------------------
// mcp remove: deletes the registration, and refuses an unregistered name.
// ---------------------------------------------------------------------

#[test]
fn remove_unregisters_a_present_name() {
    let state = unique_state_dir("remove-present");
    add(&state, "yagura", "prog", &[]);
    let outcome = run(
        &stub_argv(&["mcp", "remove", "yagura"]),
        &[state_var(&state)],
        None,
    );
    assert_exit(&outcome, 0);

    let outcome = run(
        &stub_argv(&["mcp", "get", "yagura", "--json"]),
        &[state_var(&state)],
        None,
    );
    assert_exit(&outcome, 1);
}

#[test]
fn remove_for_an_unregistered_name_fails_without_touching_state() {
    let state = unique_state_dir("remove-unregistered");
    let outcome = run(
        &stub_argv(&["mcp", "remove", "yagura"]),
        &[state_var(&state)],
        None,
    );
    match outcome {
        CaptureOutcome::Exited {
            code,
            stdout,
            stderr,
        } => {
            assert_eq!(code, Some(1));
            assert!(stdout.is_empty());
            assert_eq!(
                String::from_utf8_lossy(&stderr),
                "No MCP server named 'yagura' found.\n"
            );
        }
        other => panic!("expected Exited, got {other:?}"),
    }
    assert!(
        !state.join("registry.json").exists(),
        "a failed remove must not create a registry file"
    );
}

// ---------------------------------------------------------------------
// mcp add with MCP_CLI_STUB_TOML_TABLE_PATH: recreates the server table
// from scratch, dropping a hand-written
// sibling key. Left unset, add never creates a TOML file at all.
// ---------------------------------------------------------------------

#[test]
fn add_with_a_toml_table_path_recreates_the_server_table_and_drops_hand_written_keys() {
    let dir = common::unique_temp_dir("toml-recreate");
    let toml_path = dir.join("config.toml");
    std::fs::write(
        &toml_path,
        "[mcp_servers.yagura]\ncommand = \"old\"\ntool_timeout_sec = 42\n",
    )
    .expect("write the pre-existing settings file");
    let state = unique_state_dir("toml-recreate-state");

    let outcome = run(
        &stub_argv(&["mcp", "add", "yagura", "--", "bridge"]),
        &[
            state_var(&state),
            ("MCP_CLI_STUB_TOML_TABLE_PATH", toml_path.to_str().unwrap()),
        ],
        None,
    );
    assert_exit(&outcome, 0);

    let source = std::fs::read_to_string(&toml_path).expect("read the rewritten settings file");
    assert!(source.contains("command = \"bridge\""));
    assert!(
        !source.contains("tool_timeout_sec"),
        "add must recreate the table from scratch, dropping the hand-written key; got: {source}"
    );
}

#[test]
fn add_without_a_toml_table_path_never_creates_a_settings_file() {
    let dir = common::unique_temp_dir("toml-absent");
    let toml_path = dir.join("config.toml");
    let state = unique_state_dir("toml-absent-state");

    let outcome = run(
        &stub_argv(&["mcp", "add", "yagura", "--", "bridge"]),
        &[state_var(&state)],
        None,
    );
    assert_exit(&outcome, 0);
    assert!(
        !toml_path.exists(),
        "add must not touch a TOML file when MCP_CLI_STUB_TOML_TABLE_PATH is unset"
    );
}

#[test]
fn remove_with_a_toml_table_path_deletes_the_whole_server_table() {
    let dir = common::unique_temp_dir("toml-remove");
    let toml_path = dir.join("config.toml");
    let state = unique_state_dir("toml-remove-state");
    let toml_var = ("MCP_CLI_STUB_TOML_TABLE_PATH", toml_path.to_str().unwrap());
    add(&state, "yagura", "bridge", &[toml_var]);
    assert!(
        std::fs::read_to_string(&toml_path)
            .unwrap()
            .contains("yagura")
    );

    let outcome = run(
        &stub_argv(&["mcp", "remove", "yagura"]),
        &[state_var(&state), toml_var],
        None,
    );
    assert_exit(&outcome, 0);
    let source = std::fs::read_to_string(&toml_path).expect("read the settings file");
    assert!(
        !source.contains("yagura"),
        "remove must delete the whole [mcp_servers.yagura] table; got: {source}"
    );
}

#[test]
fn get_json_reports_tool_timeout_sec_from_the_toml_table_when_present() {
    let dir = common::unique_temp_dir("toml-timeout");
    let toml_path = dir.join("config.toml");
    let state = unique_state_dir("toml-timeout-state");
    // Register through the registry alone (no TOML table path on `add`), then
    // hand-write the settings file the way a daemon's own `toml_set` step
    // would after a successful add -- isolating "reads it back" from add's
    // own recreate-from-scratch behavior.
    add(&state, "yagura", "bridge", &[]);
    std::fs::write(
        &toml_path,
        "[mcp_servers.yagura]\ncommand = \"bridge\"\ntool_timeout_sec = 86400\n",
    )
    .expect("write the settings file");

    let body = get_json(
        &state,
        "yagura",
        &[("MCP_CLI_STUB_TOML_TABLE_PATH", toml_path.to_str().unwrap())],
    );
    assert_eq!(body["tool_timeout_sec"].as_f64(), Some(86400.0));
}

// ---------------------------------------------------------------------
// mcp add / remove with MCP_CLI_STUB_JSON_CONFIG_PATH: writes and prunes a
// JSON settings file directly, for a profile whose status is read from a
// file rather than through this binary.
// ---------------------------------------------------------------------

#[test]
fn add_with_a_json_config_path_writes_the_mcp_servers_entry() {
    let dir = common::unique_temp_dir("json-config-add");
    let json_path = dir.join("mcp_config.json");
    let state = unique_state_dir("json-config-add-state");

    let outcome = run(
        &stub_argv(&["mcp", "add", "yagura", "--", "prog", "arg"]),
        &[
            state_var(&state),
            ("MCP_CLI_STUB_JSON_CONFIG_PATH", json_path.to_str().unwrap()),
        ],
        None,
    );
    assert_exit(&outcome, 0);

    let body: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&json_path).expect("read the JSON config"))
            .expect("the JSON config must parse");
    assert_eq!(
        body["mcpServers"]["yagura"]["command"].as_str(),
        Some("prog")
    );
}

#[test]
fn remove_with_a_json_config_path_deletes_the_entry() {
    let dir = common::unique_temp_dir("json-config-remove");
    let json_path = dir.join("mcp_config.json");
    let state = unique_state_dir("json-config-remove-state");
    let json_var = ("MCP_CLI_STUB_JSON_CONFIG_PATH", json_path.to_str().unwrap());
    add(&state, "yagura", "prog", &[json_var]);

    let outcome = run(
        &stub_argv(&["mcp", "remove", "yagura"]),
        &[state_var(&state), json_var],
        None,
    );
    assert_exit(&outcome, 0);

    let body: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&json_path).expect("read the JSON config"))
            .expect("the JSON config must parse");
    assert!(
        body["mcpServers"].get("yagura").is_none(),
        "remove must prune the JSON config's own entry"
    );
}

// ---------------------------------------------------------------------
// mcp add: a K=V token ahead of `--` is captured as an environment entry,
// no matter what its own flag is spelled.
// ---------------------------------------------------------------------

#[test]
fn add_captures_k_equals_v_tokens_before_the_separator_as_environment_entries() {
    let state = unique_state_dir("env-capture");
    let outcome = run(
        &stub_argv(&[
            "mcp",
            "add",
            "yagura",
            "--env",
            "YAGURA_INSTANCE=dev",
            "--",
            "prog",
        ]),
        &[state_var(&state)],
        None,
    );
    assert_exit(&outcome, 0);

    let registry: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(state.join("registry.json")).expect("read the registry"),
    )
    .expect("the registry must parse");
    assert_eq!(
        registry["yagura"]["env"]["YAGURA_INSTANCE"].as_str(),
        Some("dev")
    );
}

// ---------------------------------------------------------------------
// probe: the fixed fixture transform, overridable for a broken-output case.
// ---------------------------------------------------------------------

#[test]
fn probe_writes_the_fixed_fixture_transform_of_its_argument() {
    let outcome = run(
        &stub_argv(&["probe", r"C:\Tools\yagura\yagura-mcp.exe"]),
        &[],
        None,
    );
    let stdout = exited_stdout(outcome, 0);
    assert_eq!(stdout.trim_end(), "/probedC:/Tools/yagura/yagura-mcp.exe");
}

#[test]
fn probe_output_and_exit_code_can_be_overridden() {
    let outcome = run(
        &stub_argv(&["probe", "whatever"]),
        &[
            ("MCP_CLI_STUB_PROBE_OUTPUT", "broken output"),
            ("MCP_CLI_STUB_PROBE_EXIT_CODE", "5"),
        ],
        None,
    );
    match outcome {
        CaptureOutcome::Exited { code, stdout, .. } => {
            assert_eq!(code, Some(5));
            assert_eq!(String::from_utf8_lossy(&stdout).trim_end(), "broken output");
        }
        other => panic!("expected Exited, got {other:?}"),
    }
}

// ---------------------------------------------------------------------
// file-read: content when present, exit 3 when absent.
// ---------------------------------------------------------------------

#[test]
fn file_read_returns_the_files_content_when_it_exists() {
    let dir = common::unique_temp_dir("file-read-present");
    let path = dir.join("data.txt");
    std::fs::write(&path, b"hello file").expect("write the fixture file");
    let outcome = run(
        &stub_argv(&["file-read", path.to_str().unwrap()]),
        &[],
        None,
    );
    assert_eq!(exited_stdout_bytes(outcome, 0), b"hello file");
}

#[test]
fn file_read_exits_3_when_the_file_is_missing() {
    let dir = common::unique_temp_dir("file-read-missing");
    let path = dir.join("nope.txt");
    let outcome = run(
        &stub_argv(&["file-read", path.to_str().unwrap()]),
        &[],
        None,
    );
    assert_exit(&outcome, 3);
}

// ---------------------------------------------------------------------
// file-replace: writes stdin, creating the file and its parent directory
// when absent, and overwrites existing content.
// ---------------------------------------------------------------------

#[test]
fn file_replace_writes_stdin_to_the_path_creating_it_and_its_parent() {
    let dir = common::unique_temp_dir("file-replace-create");
    let path = dir.join("nested").join("data.txt");
    let outcome = run(
        &stub_argv(&["file-replace", path.to_str().unwrap()]),
        &[],
        Some(b"new content".to_vec()),
    );
    assert_exit(&outcome, 0);
    assert_eq!(
        std::fs::read(&path).expect("read the replaced file"),
        b"new content"
    );
}

#[test]
fn file_replace_overwrites_existing_content() {
    let dir = common::unique_temp_dir("file-replace-overwrite");
    let path = dir.join("data.txt");
    std::fs::write(&path, b"old").expect("write the pre-existing file");
    let outcome = run(
        &stub_argv(&["file-replace", path.to_str().unwrap()]),
        &[],
        Some(b"new".to_vec()),
    );
    assert_exit(&outcome, 0);
    assert_eq!(
        std::fs::read(&path).expect("read the replaced file"),
        b"new"
    );
}

// ---------------------------------------------------------------------
// Failure injection: replaces a named subcommand's own behavior and
// touches no state; a call for a different subcommand runs normally.
// ---------------------------------------------------------------------

#[test]
fn a_failure_injection_replaces_the_subcommands_own_behavior_and_touches_no_state() {
    let state = unique_state_dir("fail-inject");
    let outcome = run(
        &stub_argv(&["mcp", "add", "yagura", "--", "prog"]),
        &[
            state_var(&state),
            ("MCP_CLI_STUB_FAIL_SUBCOMMAND", "add"),
            ("MCP_CLI_STUB_FAIL_EXIT_CODE", "17"),
            ("MCP_CLI_STUB_FAIL_STDOUT", "\u{1b}[31mred\u{1b}[0m"),
            ("MCP_CLI_STUB_FAIL_STDERR", "\u{202e}bidi\u{202c}"),
        ],
        None,
    );
    match outcome {
        CaptureOutcome::Exited {
            code,
            stdout,
            stderr,
        } => {
            assert_eq!(code, Some(17));
            assert_eq!(String::from_utf8_lossy(&stdout), "\u{1b}[31mred\u{1b}[0m");
            assert_eq!(String::from_utf8_lossy(&stderr), "\u{202e}bidi\u{202c}");
        }
        other => panic!("expected Exited, got {other:?}"),
    }
    assert!(
        !state.join("registry.json").exists(),
        "an injected failure must not run the subcommand's own state changes"
    );
}

#[test]
fn without_a_matching_fail_subcommand_the_call_runs_normally() {
    let state = unique_state_dir("fail-inject-mismatch");
    let outcome = run(
        &stub_argv(&["mcp", "add", "yagura", "--", "prog"]),
        &[
            state_var(&state),
            ("MCP_CLI_STUB_FAIL_SUBCOMMAND", "remove"),
        ],
        None,
    );
    assert_exit(&outcome, 0);
    assert!(state.join("registry.json").exists());
}

// ---------------------------------------------------------------------
// Delay and call-log injection: the log's own timestamps bracket the
// configured sleep, and record the call's argv.
// ---------------------------------------------------------------------

#[test]
fn a_delay_and_the_call_log_bracket_the_configured_sleep() {
    let dir = common::unique_temp_dir("delay-log");
    let log_path = dir.join("log.jsonl");
    let started = Instant::now();
    let outcome = run(
        &stub_argv(&["probe", "x"]),
        &[
            ("MCP_CLI_STUB_DELAY_MS", "300"),
            ("MCP_CLI_STUB_LOG", log_path.to_str().unwrap()),
        ],
        None,
    );
    let elapsed = started.elapsed();
    assert_exit(&outcome, 0);
    assert!(
        elapsed >= Duration::from_millis(300),
        "the spawn must have waited out the configured delay, took {elapsed:?}"
    );

    let log_text = std::fs::read_to_string(&log_path).expect("read the call log");
    let record: serde_json::Value =
        serde_json::from_str(log_text.lines().next().expect("one log line")).expect("valid JSON");
    let started_at = record["started_at_ms"].as_u64().expect("started_at_ms");
    let finished_at = record["finished_at_ms"].as_u64().expect("finished_at_ms");
    assert!(
        finished_at - started_at >= 300,
        "the log's own timestamps must bracket the delay too: {record}"
    );
    assert_eq!(record["argv"].clone(), serde_json::json!(["probe", "x"]));
}

// ---------------------------------------------------------------------
// Sanity check on the added profile-document builder
// (`common::mcp_cli_stub_profile_source`): it parses, and renders `add`
// and the `toml_set` table pointing at the fixture.
// ---------------------------------------------------------------------

#[test]
fn the_stub_profile_source_parses_and_renders_add_and_toml_set_against_the_fixture() {
    let dir = common::unique_temp_dir("stub-profile-source");
    let state = dir.join("state");
    let fixture = common::McpCliStubFixture {
        state_dir: &state,
        toml_table_path: Some(".neutral/config.toml"),
        on_duplicate: None,
        log_path: None,
    };
    let source = common::mcp_cli_stub_profile_source("Stub Cli", &fixture);
    let profile = yagura_daemon::profile::parse_profile("stub", &source).expect("must parse");
    let spec = profile
        .mcp_registration
        .expect("the source must carry a registration table");
    let target = yagura_daemon::mcp_registration::RegistrationTarget::for_instance(None);
    let add = spec
        .render_add(&target, "BRIDGE")
        .expect("the default instance must render");
    assert_eq!(add.first().map(String::as_str), Some("mcp"));
    let toml_set = spec.toml_set().expect("the toml_set table must be present");
    assert_eq!(toml_set.file(), ".neutral/config.toml");
}

// ---------------------------------------------------------------------
// Hold: a call stays in flight until the release file exists.
// ---------------------------------------------------------------------

#[test]
fn a_hold_until_path_keeps_the_call_in_flight_until_the_file_exists() {
    let dir = common::unique_temp_dir("hold-until");
    let release = dir.join("release");
    let release_var = release.to_str().unwrap().to_string();
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let outcome = run(
            &stub_argv(&["probe", "x"]),
            &[("MCP_CLI_STUB_HOLD_UNTIL", release_var.as_str())],
            None,
        );
        let _ = sender.send(outcome);
    });

    // Negative window: a probe call that is not held finishes within milliseconds.
    assert!(
        matches!(
            receiver.recv_timeout(Duration::from_millis(1500)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ),
        "the call must stay in flight while the release file is absent"
    );
    std::fs::write(&release, b"").expect("release the held call");
    let outcome = receiver
        .recv_timeout(Duration::from_secs(10))
        .expect("the call must finish once the release file exists");
    assert_exit(&outcome, 0);
}

// ---------------------------------------------------------------------
// A read that fails for a reason other than "absent" is an error, never an
// empty or absent file; an absent settings file is fine for `remove`/`get`.
// ---------------------------------------------------------------------

fn expect_failed_with_code_1(outcome: &CaptureOutcome, what: &str) {
    match outcome {
        CaptureOutcome::Exited { code, stderr, .. } => {
            assert_eq!(*code, Some(1), "{what}: stderr was {stderr:?}");
        }
        other => panic!("{what}: expected Exited, got {other:?}"),
    }
}

#[test]
fn file_read_of_a_directory_is_an_error_not_the_absent_exit_code() {
    let dir = common::unique_temp_dir("file-read-directory");
    let outcome = run(&stub_argv(&["file-read", dir.to_str().unwrap()]), &[], None);
    expect_failed_with_code_1(&outcome, "file-read of a directory");
}

#[test]
fn an_unreadable_registry_is_an_error_not_an_empty_registry() {
    let state = unique_state_dir("registry-unreadable");
    std::fs::create_dir_all(state.join("registry.json")).expect("make registry.json a directory");
    let outcome = run(
        &stub_argv(&["mcp", "get", "yagura", "--json"]),
        &[state_var(&state)],
        None,
    );
    match outcome {
        CaptureOutcome::Exited { code, stderr, .. } => {
            assert_eq!(code, Some(1));
            assert!(
                !String::from_utf8_lossy(&stderr).contains("No MCP server named"),
                "an unreadable registry must not read as an empty one"
            );
        }
        other => panic!("expected Exited, got {other:?}"),
    }
}

#[test]
fn add_refuses_a_toml_settings_file_it_cannot_read_and_leaves_it_untouched() {
    let dir = common::unique_temp_dir("toml-unreadable-add");
    let toml_path = dir.join("config.toml");
    // Not UTF-8: reading it fails, which is not the same as the file being absent.
    std::fs::write(&toml_path, [0xff_u8, 0xfe, 0xfd]).expect("write the settings file");
    let state = unique_state_dir("toml-unreadable-add-state");

    let outcome = run(
        &stub_argv(&["mcp", "add", "yagura", "--", "prog"]),
        &[
            state_var(&state),
            ("MCP_CLI_STUB_TOML_TABLE_PATH", toml_path.to_str().unwrap()),
        ],
        None,
    );
    expect_failed_with_code_1(&outcome, "add over an unreadable TOML file");
    assert_eq!(
        std::fs::read(&toml_path).expect("read the settings file"),
        [0xff_u8, 0xfe, 0xfd]
    );
}

#[test]
fn remove_and_get_json_fail_on_an_unreadable_toml_settings_path() {
    let dir = common::unique_temp_dir("toml-unreadable");
    let state = unique_state_dir("toml-unreadable-state");
    add(&state, "yagura", "bridge", &[]);
    let toml_var = ("MCP_CLI_STUB_TOML_TABLE_PATH", dir.to_str().unwrap());

    let get = run(
        &stub_argv(&["mcp", "get", "yagura", "--json"]),
        &[state_var(&state), toml_var],
        None,
    );
    expect_failed_with_code_1(&get, "get --json over an unreadable TOML path");

    let remove = run(
        &stub_argv(&["mcp", "remove", "yagura"]),
        &[state_var(&state), toml_var],
        None,
    );
    expect_failed_with_code_1(&remove, "remove over an unreadable TOML path");
}

#[test]
fn remove_fails_on_an_unreadable_json_config_path() {
    let dir = common::unique_temp_dir("json-unreadable");
    let state = unique_state_dir("json-unreadable-state");
    add(&state, "yagura", "bridge", &[]);

    let remove = run(
        &stub_argv(&["mcp", "remove", "yagura"]),
        &[
            state_var(&state),
            ("MCP_CLI_STUB_JSON_CONFIG_PATH", dir.to_str().unwrap()),
        ],
        None,
    );
    expect_failed_with_code_1(&remove, "remove over an unreadable JSON path");
}

#[test]
fn remove_and_get_json_succeed_when_the_settings_files_do_not_exist() {
    let dir = common::unique_temp_dir("settings-absent");
    let toml_path = dir.join("absent.toml");
    let json_path = dir.join("absent.json");
    let state = unique_state_dir("settings-absent-state");
    add(&state, "yagura", "bridge", &[]);
    let toml_var = ("MCP_CLI_STUB_TOML_TABLE_PATH", toml_path.to_str().unwrap());
    let json_var = ("MCP_CLI_STUB_JSON_CONFIG_PATH", json_path.to_str().unwrap());

    let body = get_json(&state, "yagura", &[toml_var]);
    assert!(body.get("tool_timeout_sec").is_none());

    let outcome = run(
        &stub_argv(&["mcp", "remove", "yagura"]),
        &[state_var(&state), toml_var, json_var],
        None,
    );
    assert_exit(&outcome, 0);
    assert!(
        !toml_path.exists() && !json_path.exists(),
        "remove must not create a settings file that was absent"
    );
}
