#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: `yagura-daemon` CLI argument resolution (`cli.rs`) and the
//! environment-variable resolvers `main.rs` applies at startup.
//!
//! `args` passed to `parse_args` excludes the program name.
//!
//! `cli.rs` must not hardcode the `"generic-shell"` default: an omitted
//! `--detection` resolves to `Run { detection_name: None, .. }`, and applying the
//! default is `DaemonConfig::default()`'s responsibility.

mod common;

use std::ffi::OsString;
use std::path::PathBuf;

use yagura_daemon::cli::{
    CliCommand, CliError, DetectionDirResolution, parse_args, resolve_detection_dir,
    resolve_playbook, resolve_roles_path,
};
use yagura_daemon::detector::layers::DetectionLayers;
use yagura_os::InstanceNamespace;
use yagura_protocol::WindowStatus;
use yagura_test_support::paths::TempDir;

fn args(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

/// The default instance's namespace, for the `resolve_detection_dir` cases
/// below that are not about instance naming at all: they pass it
/// through unchanged so the `yagura/detection` paths they already assert on
/// stay literal.
fn default_namespace() -> InstanceNamespace {
    yagura_os::resolve_instance_from(None).expect("the default instance always resolves")
}

/// A non-default instance, for the `resolve_roles_path` cases that check a
/// named instance's `roles.toml` path differs from the default's.
fn dev_namespace() -> InstanceNamespace {
    yagura_os::resolve_instance_from(Some(std::ffi::OsStr::new("dev")))
        .expect("\"dev\" is a valid identifier and must resolve")
}

#[test]
fn parse_args_return_type_matches_confirmed_api() {
    let result: Result<CliCommand, CliError> = parse_args(&args(&[]));
    assert!(result.is_ok());
}

#[test]
fn parse_args_with_no_arguments_resolves_to_run_with_no_overrides() {
    let result = parse_args(&args(&[])).expect("empty args should parse successfully");
    match result {
        CliCommand::Run {
            detection_name,
            detection_dir_arg,
            playbook_arg,
            ..
        } => {
            assert_eq!(
                detection_name, None,
                "cli.rs must not hardcode the \"generic-shell\" default; that is \
                 DaemonConfig::default()'s responsibility"
            );
            assert_eq!(detection_dir_arg, None);
            assert_eq!(
                playbook_arg, None,
                "an omitted --playbook must stay None: resolving YAGURA_PLAYBOOK is \
                 resolve_playbook's job, not parse_args's -- this field only carries \
                 stage (1)'s raw argument value"
            );
        }
        other => panic!("expected CliCommand::Run, got {other:?}"),
    }
}

#[test]
fn parse_args_shutdown_flag_resolves_to_shutdown() {
    let result = parse_args(&args(&["--shutdown"])).expect("--shutdown should parse successfully");
    assert!(
        matches!(result, CliCommand::Shutdown),
        "expected CliCommand::Shutdown, got {result:?}"
    );
}

#[test]
fn parse_args_detection_flag_resolves_detection_name() {
    let result = parse_args(&args(&["--detection", "claude-code"]))
        .expect("a valid --detection value should parse");
    match result {
        CliCommand::Run { detection_name, .. } => {
            assert_eq!(detection_name, Some("claude-code".to_string()));
        }
        other => panic!("expected CliCommand::Run, got {other:?}"),
    }
}

#[test]
fn parse_args_detection_dir_flag_resolves_detection_dir_arg() {
    let dir = if cfg!(windows) {
        "C:\\some\\detection\\dir"
    } else {
        "/some/detection/dir"
    };
    let result = parse_args(&args(&["--detection-dir", dir]))
        .expect("--detection-dir should parse successfully");
    match result {
        CliCommand::Run {
            detection_dir_arg, ..
        } => {
            assert_eq!(detection_dir_arg, Some(PathBuf::from(dir)));
        }
        other => panic!("expected CliCommand::Run, got {other:?}"),
    }
}

#[test]
fn parse_args_detection_and_detection_dir_resolve_together() {
    let dir = if cfg!(windows) {
        "C:\\combined\\dir"
    } else {
        "/combined/dir"
    };
    let result = parse_args(&args(&[
        "--detection",
        "generic-shell",
        "--detection-dir",
        dir,
    ]))
    .expect("combined --detection and --detection-dir should parse");
    match result {
        CliCommand::Run {
            detection_name,
            detection_dir_arg,
            playbook_arg,
            ..
        } => {
            assert_eq!(detection_name, Some("generic-shell".to_string()));
            assert_eq!(detection_dir_arg, Some(PathBuf::from(dir)));
            assert_eq!(playbook_arg, None);
        }
        other => panic!("expected CliCommand::Run, got {other:?}"),
    }
}

#[test]
fn parse_args_accepts_a_detection_name_at_the_sixty_four_character_boundary() {
    let name = "a".repeat(64);
    let result = parse_args(&args(&["--detection", &name]))
        .unwrap_or_else(|err| panic!("a 64-char detection name must be accepted, got {err:?}"));
    match result {
        CliCommand::Run { detection_name, .. } => {
            assert_eq!(detection_name, Some(name));
        }
        other => panic!("expected CliCommand::Run, got {other:?}"),
    }
}

#[test]
fn parse_args_rejects_invalid_detection_names() {
    let too_long = "a".repeat(65);
    let invalid_names: [&str; 6] = [
        "a/b",     // path separator (forward slash)
        "a\\b",    // path separator (backslash)
        "..",      // parent-directory traversal
        "Claude",  // uppercase is not in [a-z0-9_-]
        "",        // empty
        &too_long, // exceeds the 64-character limit
    ];
    for name in invalid_names {
        let result = parse_args(&args(&["--detection", name]));
        assert!(
            result.is_err(),
            "expected --detection {name:?} to be rejected as an invalid detection name \
             (is_valid_detection_name), got {result:?}"
        );
    }
}

#[test]
fn parse_args_playbook_flag_resolves_playbook_arg() {
    let path = if cfg!(windows) {
        "C:\\some\\playbook.toml"
    } else {
        "/some/playbook.toml"
    };
    let result =
        parse_args(&args(&["--playbook", path])).expect("--playbook should parse successfully");
    match result {
        CliCommand::Run { playbook_arg, .. } => {
            assert_eq!(
                playbook_arg,
                Some(PathBuf::from(path)),
                "cli.rs keeps the raw argument value; resolution against YAGURA_PLAYBOOK is \
                 resolve_playbook's job (same split as --detection-dir)"
            );
        }
        other => panic!("expected CliCommand::Run, got {other:?}"),
    }
}

#[test]
fn parse_args_rejects_playbook_without_a_value() {
    let result = parse_args(&args(&["--playbook"]));
    assert!(
        result.is_err(),
        "--playbook given as the last argument must be rejected fail-loud (MissingValue), \
         never treated as an omitted flag, got {result:?}"
    );
}

#[test]
fn parse_args_approver_exe_flag_resolves_approver_exe_arg() {
    let path = if cfg!(windows) {
        "C:\\some\\yagura-gui.exe"
    } else {
        "/some/yagura-gui"
    };
    let result = parse_args(&args(&["--approver-exe", path]))
        .expect("--approver-exe should parse successfully");
    match result {
        CliCommand::Run {
            approver_exe_arg, ..
        } => {
            assert_eq!(
                approver_exe_arg,
                Some(PathBuf::from(path)),
                "cli.rs keeps the raw argument value; canonicalizing it is main.rs's job \
                 (same split as --playbook)"
            );
        }
        other => panic!("expected CliCommand::Run, got {other:?}"),
    }
}

#[test]
fn parse_args_rejects_approver_exe_without_a_value() {
    let result = parse_args(&args(&["--approver-exe"]));
    assert!(
        result.is_err(),
        "--approver-exe given as the last argument must be rejected fail-loud (MissingValue), \
         never treated as an omitted flag, got {result:?}"
    );
}

#[test]
fn parse_args_rejects_an_empty_approver_exe_value() {
    let result = parse_args(&args(&["--approver-exe", ""]));
    assert!(
        result.is_err(),
        "an empty --approver-exe value must be rejected rather than passed on as the empty \
         path, the same way --playbook rejects it, got {result:?}"
    );
}

#[test]
fn parse_args_lets_the_last_approver_exe_occurrence_win() {
    let (first, last) = if cfg!(windows) {
        ("C:\\first\\yagura-gui.exe", "C:\\last\\yagura-gui.exe")
    } else {
        ("/first/yagura-gui", "/last/yagura-gui")
    };
    let result = parse_args(&args(&["--approver-exe", first, "--approver-exe", last]))
        .expect("a repeated --approver-exe should parse");
    match result {
        CliCommand::Run {
            approver_exe_arg, ..
        } => {
            assert_eq!(
                approver_exe_arg,
                Some(PathBuf::from(last)),
                "a repeated flag resolves to its last occurrence, the rule parse_args \
                 documents for every value-taking flag"
            );
        }
        other => panic!("expected CliCommand::Run, got {other:?}"),
    }
}

#[test]
fn parse_args_shutdown_combined_with_approver_exe_accepts_and_ignores_it() {
    let path = if cfg!(windows) {
        "C:\\some\\yagura-gui.exe"
    } else {
        "/some/yagura-gui"
    };
    let result = parse_args(&args(&["--shutdown", "--approver-exe", path]))
        .expect("--approver-exe combined with --shutdown must be accepted, not rejected");
    assert!(
        matches!(result, CliCommand::Shutdown),
        "--approver-exe only has meaning for CliCommand::Run and must be accepted and \
         ignored on the --shutdown path, the same as --playbook, got {result:?}"
    );
}

#[test]
fn parse_args_emit_status_combined_with_approver_exe_accepts_and_ignores_it() {
    let path = if cfg!(windows) {
        "C:\\some\\yagura-gui.exe"
    } else {
        "/some/yagura-gui"
    };
    let result = parse_args(&args(&["--emit-status", "idle", "--approver-exe", path]))
        .expect("--approver-exe combined with --emit-status must be accepted, not rejected");
    assert!(
        matches!(result, CliCommand::EmitStatus { .. }),
        "--approver-exe only has meaning for CliCommand::Run and must be accepted and \
         ignored on the --emit-status path, the same as --playbook, got {result:?}"
    );
}

#[test]
fn parse_args_profiles_dir_flag_resolves_profiles_dir_arg() {
    let dir = if cfg!(windows) {
        "C:\\some\\profiles\\dir"
    } else {
        "/some/profiles/dir"
    };
    let result = parse_args(&args(&["--profiles-dir", dir])).expect("--profiles-dir should parse");
    match result {
        CliCommand::Run {
            profiles_dir_arg, ..
        } => {
            assert_eq!(
                profiles_dir_arg,
                Some(PathBuf::from(dir)),
                "cli.rs keeps the raw argument value; resolving it against \
                 YAGURA_PROFILES_DIR and the configuration base is resolve_profiles_dir's \
                 job, the same split --detection-dir and --playbook already use"
            );
        }
        other => panic!("expected CliCommand::Run, got {other:?}"),
    }
}

#[test]
fn parse_args_with_no_arguments_leaves_profiles_dir_arg_unset() {
    let result = parse_args(&args(&[])).expect("empty args should parse successfully");
    match result {
        CliCommand::Run {
            profiles_dir_arg, ..
        } => {
            assert_eq!(
                profiles_dir_arg, None,
                "an omitted --profiles-dir must stay None so that resolve_profiles_dir \
                 decides, rather than parse_args baking in a default location"
            );
        }
        other => panic!("expected CliCommand::Run, got {other:?}"),
    }
}

#[test]
fn parse_args_rejects_profiles_dir_without_a_value() {
    let result = parse_args(&args(&["--profiles-dir"]));
    assert!(
        result.is_err(),
        "--profiles-dir given as the last argument must be rejected fail-loud \
         (MissingValue), never treated as an omitted flag, got {result:?}"
    );
}

#[test]
fn parse_args_rejects_an_empty_profiles_dir_value() {
    let result = parse_args(&args(&["--profiles-dir", ""]));
    assert!(
        result.is_err(),
        "an empty --profiles-dir value must be rejected rather than passed on as the \
         empty path, the same way --detection-dir and --playbook reject it, got {result:?}"
    );
}

#[test]
fn parse_args_lets_the_last_profiles_dir_occurrence_win() {
    let (first, last) = if cfg!(windows) {
        ("C:\\first\\profiles", "C:\\last\\profiles")
    } else {
        ("/first/profiles", "/last/profiles")
    };
    let result = parse_args(&args(&["--profiles-dir", first, "--profiles-dir", last]))
        .expect("a repeated --profiles-dir should parse");
    match result {
        CliCommand::Run {
            profiles_dir_arg, ..
        } => {
            assert_eq!(
                profiles_dir_arg,
                Some(PathBuf::from(last)),
                "a repeated flag resolves to its last occurrence, the rule parse_args \
                 documents for every value-taking flag"
            );
        }
        other => panic!("expected CliCommand::Run, got {other:?}"),
    }
}

// --roles: same shape as --profiles-dir / --playbook -- cli.rs keeps the raw
// argument value; precedence against YAGURA_ROLES and the config base is
// resolve_roles_path's job (tested further below).

#[test]
fn parse_args_roles_flag_resolves_roles_arg() {
    let path = if cfg!(windows) {
        "C:\\some\\roles.toml"
    } else {
        "/some/roles.toml"
    };
    let result = parse_args(&args(&["--roles", path])).expect("--roles should parse");
    match result {
        CliCommand::Run { roles_arg, .. } => {
            assert_eq!(
                roles_arg,
                Some(PathBuf::from(path)),
                "cli.rs keeps the raw argument value; resolving it against YAGURA_ROLES and \
                 the configuration base is resolve_roles_path's job, the same split \
                 --profiles-dir already uses"
            );
        }
        other => panic!("expected CliCommand::Run, got {other:?}"),
    }
}

#[test]
fn parse_args_with_no_arguments_leaves_roles_arg_unset() {
    let result = parse_args(&args(&[])).expect("empty args should parse successfully");
    match result {
        CliCommand::Run { roles_arg, .. } => {
            assert_eq!(
                roles_arg, None,
                "an omitted --roles must stay None so resolve_roles_path decides, rather than \
                 parse_args baking in a default location"
            );
        }
        other => panic!("expected CliCommand::Run, got {other:?}"),
    }
}

#[test]
fn parse_args_rejects_roles_without_a_value() {
    let result = parse_args(&args(&["--roles"]));
    assert!(
        result.is_err(),
        "--roles given as the last argument must be rejected fail-loud (MissingValue), never \
         treated as an omitted flag, got {result:?}"
    );
}

#[test]
fn parse_args_rejects_an_empty_roles_value() {
    let result = parse_args(&args(&["--roles", ""]));
    assert!(
        result.is_err(),
        "an empty --roles value must be rejected rather than passed on as the empty path, the \
         same way --profiles-dir rejects it, got {result:?}"
    );
}

#[test]
fn parse_args_lets_the_last_roles_occurrence_win() {
    let (first, last) = if cfg!(windows) {
        ("C:\\first\\roles.toml", "C:\\last\\roles.toml")
    } else {
        ("/first/roles.toml", "/last/roles.toml")
    };
    let result = parse_args(&args(&["--roles", first, "--roles", last]))
        .expect("a repeated --roles should parse");
    match result {
        CliCommand::Run { roles_arg, .. } => {
            assert_eq!(
                roles_arg,
                Some(PathBuf::from(last)),
                "a repeated flag resolves to its last occurrence, the rule parse_args \
                 documents for every value-taking flag"
            );
        }
        other => panic!("expected CliCommand::Run, got {other:?}"),
    }
}

#[test]
fn parse_args_shutdown_combined_with_roles_accepts_and_ignores_it() {
    let path = if cfg!(windows) {
        "C:\\some\\roles.toml"
    } else {
        "/some/roles.toml"
    };
    let result = parse_args(&args(&["--shutdown", "--roles", path]))
        .expect("--roles combined with --shutdown must be accepted, not rejected");
    assert!(
        matches!(result, CliCommand::Shutdown),
        "--roles only has meaning for CliCommand::Run and must be accepted and ignored on the \
         --shutdown path, the same as --profiles-dir, got {result:?}"
    );
}

#[test]
fn parse_args_emit_status_combined_with_roles_accepts_and_ignores_it() {
    let path = if cfg!(windows) {
        "C:\\some\\roles.toml"
    } else {
        "/some/roles.toml"
    };
    let result = parse_args(&args(&["--emit-status", "idle", "--roles", path]))
        .expect("--roles combined with --emit-status must be accepted, not rejected");
    assert!(
        matches!(result, CliCommand::EmitStatus { .. }),
        "--roles only has meaning for CliCommand::Run and must be accepted and ignored on the \
         --emit-status path, the same as --profiles-dir, got {result:?}"
    );
}

// resolve_roles_path precedence: (1) --roles, (2) YAGURA_ROLES (empty
// counts as unset), (3) <config base>/<namespace>/roles.toml. Unlike
// resolve_detection_dir / resolve_profiles_dir, stage (3) is returned
// even when roles.toml does not exist on disk.

#[test]
fn resolve_roles_path_prefers_arg_over_all_other_sources() {
    let tree = TempDir::new("yagura-cli-test", "roles-arg-wins");
    let arg_path = tree.write_file("arg-roles.toml", "");
    let env_path = tree.write_file("env-roles.toml", "");
    let base = tree.make_dir("config-base");

    let resolution = resolve_roles_path(
        Some(arg_path.clone()),
        Some(OsString::from(&env_path)),
        Some(base),
        &default_namespace(),
    );
    assert_eq!(resolution, Some(arg_path));
}

#[test]
fn resolve_roles_path_prefers_env_when_arg_is_absent() {
    let tree = TempDir::new("yagura-cli-test", "roles-env-wins");
    let env_path = tree.write_file("env-roles.toml", "");
    let base = tree.make_dir("config-base");

    let resolution = resolve_roles_path(
        None,
        Some(OsString::from(&env_path)),
        Some(base),
        &default_namespace(),
    );
    assert_eq!(resolution, Some(env_path));
}

#[test]
fn resolve_roles_path_treats_an_empty_env_value_as_unset() {
    let tree = TempDir::new("yagura-cli-test", "roles-empty-env");
    let base = tree.make_dir("config-base");

    let resolution = resolve_roles_path(
        None,
        Some(OsString::new()),
        Some(base.clone()),
        &default_namespace(),
    );
    assert_eq!(
        resolution,
        Some(yagura_os::instance_dir(&base, &default_namespace()).join("roles.toml")),
        "an empty YAGURA_ROLES must count as unset, so stage (3) must still be adopted \
         rather than an explicit empty path"
    );
}

#[test]
fn resolve_roles_path_returns_the_config_root_path_even_when_roles_toml_does_not_exist() {
    let tree = TempDir::new("yagura-cli-test", "roles-config-root-absent-file");
    // Nothing named roles.toml is ever created under this base.
    let base = tree.make_dir("config-base");

    let resolution = resolve_roles_path(None, None, Some(base.clone()), &default_namespace());
    assert_eq!(
        resolution,
        Some(yagura_os::instance_dir(&base, &default_namespace()).join("roles.toml")),
        "unlike resolve_detection_dir / resolve_profiles_dir, stage (3) must be returned \
         whether or not roles.toml already exists on disk: the caller may be about to \
         create it (e.g. save_roles)"
    );
}

#[test]
fn resolve_roles_path_returns_none_when_no_config_base_is_given() {
    let resolution: Option<PathBuf> = resolve_roles_path(None, None, None, &default_namespace());
    assert_eq!(
        resolution, None,
        "with no --roles, no YAGURA_ROLES and no config base, resolve_roles_path must return \
         None rather than guessing a location"
    );
}

#[test]
fn resolve_roles_path_differs_for_a_non_default_namespace() {
    let tree = TempDir::new("yagura-cli-test", "roles-namespace-differs");
    let base = tree.make_dir("config-base");

    let default_resolution =
        resolve_roles_path(None, None, Some(base.clone()), &default_namespace())
            .expect("stage (3) must resolve for the default namespace");
    let dev_resolution = resolve_roles_path(None, None, Some(base), &dev_namespace())
        .expect("stage (3) must resolve for a non-default namespace too");

    assert_ne!(
        default_resolution, dev_resolution,
        "a non-default instance's roles.toml path must differ from the default instance's, \
         the same per-instance derivation every other config-root path uses"
    );
}

#[test]
fn parse_args_emit_status_with_only_status_resolves_defaults() {
    let result = parse_args(&args(&["--emit-status", "waiting_input"]))
        .expect("a valid --emit-status value should parse");
    match result {
        CliCommand::EmitStatus { status, source, .. } => {
            assert_eq!(status, WindowStatus::WaitingInput);
            assert_eq!(source, "cli", "an omitted --source must default to \"cli\"");
        }
        other => panic!("expected CliCommand::EmitStatus, got {other:?}"),
    }
}

#[test]
fn parse_args_emit_status_with_source_resolves_all_fields() {
    let result = parse_args(&args(&["--emit-status", "idle", "--source", "claude-code"]))
        .expect("--emit-status with --source should parse");
    match result {
        CliCommand::EmitStatus { status, source, .. } => {
            assert_eq!(status, WindowStatus::Idle);
            assert_eq!(source, "claude-code");
        }
        other => panic!("expected CliCommand::EmitStatus, got {other:?}"),
    }
}

#[test]
fn parse_args_emit_status_accepts_all_seven_status_values() {
    let cases: [(&str, WindowStatus); 7] = [
        ("running", WindowStatus::Running),
        ("waiting_input", WindowStatus::WaitingInput),
        ("waiting_approval", WindowStatus::WaitingApproval),
        ("interrupted", WindowStatus::Interrupted),
        ("idle", WindowStatus::Idle),
        ("exited", WindowStatus::Exited),
        ("unknown", WindowStatus::Unknown),
    ];
    for (text, expected) in cases {
        let result = parse_args(&args(&["--emit-status", text]))
            .unwrap_or_else(|err| panic!("--emit-status {text} should be accepted, got {err:?}"));
        match result {
            CliCommand::EmitStatus { status, .. } => assert_eq!(status, expected),
            other => panic!("expected CliCommand::EmitStatus, got {other:?}"),
        }
    }
}

#[test]
fn parse_args_rejects_invalid_emit_status_value() {
    let result = parse_args(&args(&["--emit-status", "not_a_status"]));
    assert!(
        result.is_err(),
        "an unrecognized --emit-status value must be rejected, got {result:?}"
    );
}

#[test]
fn parse_args_rejects_invalid_emit_status_source() {
    let result = parse_args(&args(&[
        "--emit-status",
        "running",
        "--source",
        "Invalid Source!",
    ]));
    assert!(
        result.is_err(),
        "a --source value violating is_valid_source ([a-z0-9_-]{{1,64}}, yagura-protocol \
         SSOT) must be rejected, got {result:?}"
    );
}

#[test]
fn parse_args_rejects_ttl_ms_as_an_unknown_argument() {
    // A report is the start or end of a turn and has no lifetime, so the flag
    // is gone: a hook that still passes it fails loudly instead of having the
    // value silently dropped.
    for value in ["5000", "1", "300001"] {
        let result = parse_args(&args(&["--emit-status", "running", "--ttl-ms", value]));
        assert!(
            matches!(&result, Err(CliError::UnknownArgument(arg)) if arg == "--ttl-ms"),
            "--ttl-ms {value} must be an unknown argument, got {result:?}"
        );
    }
}

// resolve_detection_dir: an explicit argument wins, then a non-empty
// environment value, then the `yagura/detection` directory under the config
// base; anything else stays unresolved.

/// Unwraps an explicit resolution, reporting any other variant instead.
fn explicit_path(resolution: DetectionDirResolution) -> PathBuf {
    match resolution {
        DetectionDirResolution::Explicit(path) => path,
        other => panic!("expected an explicit resolution, got {other:?}"),
    }
}

/// Unwraps a config-root resolution, reporting any other variant instead.
fn config_root_path(resolution: DetectionDirResolution) -> PathBuf {
    match resolution {
        DetectionDirResolution::ConfigRoot(path) => path,
        other => panic!("expected a config-root resolution, got {other:?}"),
    }
}

/// Asserts nothing resolved, printing `context` so each caller's case is
/// identifiable from the failure alone.
fn assert_unresolved(resolution: DetectionDirResolution, context: &str) {
    assert!(
        matches!(resolution, DetectionDirResolution::Unresolved),
        "{context} must leave the directory layer unresolved, got {resolution:?}"
    );
}

/// Creates the default instance's `detection` directory under
/// `<tree>/<relative>` and returns both the config
/// base (`<tree>/<relative>`) and that directory, so a case does not rebuild
/// the join it asserts on.
fn config_base_with_detection_dir(tree: &TempDir, relative: &str) -> (PathBuf, PathBuf) {
    let base = tree.make_dir(relative);
    let detection_dir = yagura_os::instance_dir(&base, &default_namespace()).join("detection");
    std::fs::create_dir_all(&detection_dir).expect("config-root detection dir should be creatable");
    (base, detection_dir)
}

#[test]
fn resolve_detection_dir_signature_matches_confirmed_api() {
    // Nothing is given, so this does not depend on any branch succeeding.
    let resolution: DetectionDirResolution =
        resolve_detection_dir(None, None, None, &default_namespace());
    assert_unresolved(
        resolution,
        "no argument, no environment value and no config base",
    );
}

#[test]
fn resolve_detection_dir_prefers_arg_over_all_other_sources() {
    let tree = TempDir::new("yagura-cli-test", "arg-wins");
    let arg_dir = tree.make_dir("arg-detection");
    let env_dir = tree.make_dir("env-detection");
    // A config-root directory also exists; it must not win.
    let (base, _config_root) = config_base_with_detection_dir(&tree, "config-base");

    let resolution = resolve_detection_dir(
        Some(arg_dir.clone()),
        Some(OsString::from(&env_dir)),
        Some(base),
        &default_namespace(),
    );
    assert_eq!(explicit_path(resolution), arg_dir);
}

#[test]
fn resolve_detection_dir_prefers_env_when_arg_is_absent() {
    let tree = TempDir::new("yagura-cli-test", "env-wins");
    let env_dir = tree.make_dir("env-detection");
    // A config-root directory also exists; it must not win.
    let (base, _config_root) = config_base_with_detection_dir(&tree, "config-base");

    let resolution = resolve_detection_dir(
        None,
        Some(OsString::from(&env_dir)),
        Some(base),
        &default_namespace(),
    );
    assert_eq!(explicit_path(resolution), env_dir);
}

#[test]
fn resolve_detection_dir_keeps_an_explicit_directory_that_does_not_exist() {
    let tree = TempDir::new("yagura-cli-test", "explicit-missing");
    let arg_dir = tree.make_dir("tree-root").join("never-created");

    let resolution = resolve_detection_dir(Some(arg_dir.clone()), None, None, &default_namespace());
    assert_eq!(
        explicit_path(resolution),
        arg_dir,
        "an explicitly given directory is returned as typed; whether it exists is decided \
         when a definition is loaded, not here"
    );
}

#[test]
fn resolve_detection_dir_treats_an_empty_env_value_as_unset() {
    let tree = TempDir::new("yagura-cli-test", "empty-env-falls-through");
    let (base, config_root) = config_base_with_detection_dir(&tree, "config-base");

    let resolution = resolve_detection_dir(
        None,
        Some(OsString::new()),
        Some(base),
        &default_namespace(),
    );
    assert_eq!(
        config_root_path(resolution),
        config_root,
        "an empty environment value counts as unset, so the config-root directory must \
         still be adopted rather than an explicit empty path"
    );
}

#[test]
fn resolve_detection_dir_adopts_the_config_root_directory_even_when_it_is_empty() {
    let tree = TempDir::new("yagura-cli-test", "config-root-empty");
    let (base, config_root) = config_base_with_detection_dir(&tree, "config-base");

    let resolution = resolve_detection_dir(None, None, Some(base), &default_namespace());
    assert_eq!(
        config_root_path(resolution),
        config_root,
        "a config-root detection directory is adopted on existence alone; an empty one is \
         a user who has not placed a definition yet, not a reason to fall back"
    );
}

#[test]
fn resolve_detection_dir_returns_unresolved_when_the_config_root_directory_is_absent() {
    let tree = TempDir::new("yagura-cli-test", "config-root-absent");
    let base = tree.make_dir("config-base");

    assert_unresolved(
        resolve_detection_dir(None, None, Some(base), &default_namespace()),
        "a config base without a yagura/detection directory",
    );
}

#[test]
fn resolve_detection_dir_returns_unresolved_when_the_config_root_path_is_a_file() {
    let tree = TempDir::new("yagura-cli-test", "config-root-is-file");
    let base = tree.make_dir("config-base");
    let root = yagura_os::instance_dir(&base, &default_namespace());
    std::fs::create_dir_all(&root).expect("the instance root should be creatable");
    std::fs::write(root.join("detection"), "")
        .expect("the file standing in for the directory should be writable");

    assert_unresolved(
        resolve_detection_dir(None, None, Some(base), &default_namespace()),
        "a config-root path that is a file rather than a directory",
    );
}

#[test]
fn resolve_detection_dir_returns_unresolved_when_nothing_resolves() {
    let tree = TempDir::new("yagura-cli-test", "nothing-resolves");
    let base = tree.make_dir("config-base");
    // No yagura/detection anywhere under the tree.

    assert_unresolved(
        resolve_detection_dir(None, None, Some(base), &default_namespace()),
        "an empty config base with no argument and no environment value",
    );
}

// DetectionLayers::from_resolution is the single place a resolution becomes the
// pair of layers the daemon carries on its config.

#[test]
fn detection_layers_from_an_explicit_resolution_switch_off_the_bundled_layer() {
    let path = PathBuf::from("explicitly-given-detection-dir");
    let layers = DetectionLayers::from_resolution(DetectionDirResolution::Explicit(path.clone()));

    assert_eq!(layers.directory, Some(path));
    assert!(
        !layers.bundled,
        "an explicitly given directory is exclusive: a name it does not carry must not \
         fall through to a bundled definition"
    );
}

#[test]
fn detection_layers_from_a_config_root_resolution_keep_both_layers() {
    let path = PathBuf::from("config-root-detection-dir");
    let layers = DetectionLayers::from_resolution(DetectionDirResolution::ConfigRoot(path.clone()));

    assert_eq!(layers.directory, Some(path));
    assert!(
        layers.bundled,
        "a config-root directory overlays the bundled layer rather than replacing it"
    );
}

#[test]
fn detection_layers_from_an_unresolved_resolution_keep_only_the_bundled_layer() {
    let layers = DetectionLayers::from_resolution(DetectionDirResolution::Unresolved);

    assert_eq!(layers.directory, None);
    assert!(
        layers.bundled,
        "with no directory layer the bundled definitions are the only source left, so they \
         must stay available"
    );
}

// resolve_playbook: only the arg-vs-env precedence lives here; the full
// precedence / fail-closed contract is in tests/playbook_resolution.rs.

#[test]
fn resolve_playbook_prefers_the_argument_over_the_environment() {
    let tree = TempDir::new("yagura-cli-test", "playbook-arg-wins");
    let arg_path = tree.write_file("arg-playbook.toml", "");
    let env_path = tree.write_file("env-playbook.toml", "");

    let result = resolve_playbook(Some(arg_path.clone()), Some(env_path.into_os_string()));
    assert_eq!(result, Some(arg_path));
}

#[test]
fn resolve_playbook_falls_back_to_the_environment_when_the_argument_is_absent() {
    let tree = TempDir::new("yagura-cli-test", "playbook-env");
    let env_path = tree.write_file("env-playbook.toml", "");

    let result = resolve_playbook(None, Some(env_path.clone().into_os_string()));
    assert_eq!(result, Some(env_path));
}

#[test]
fn resolve_playbook_returns_none_when_nothing_resolves() {
    let result: Option<PathBuf> = resolve_playbook(None, None);
    assert_eq!(
        result, None,
        "when neither --playbook nor YAGURA_PLAYBOOK is set, resolve_playbook must stay \
         None: that is what keeps the approval gate at deny-everything \
         (fail-closed). There is no exe-adjacent or ancestor search to fall back to (see \
         tests/playbook_resolution.rs)"
    );
}

#[test]
fn resolve_max_windows_returns_default_when_env_not_set() {
    let result = yagura_daemon::config::resolve_max_windows(None, 32);
    assert_eq!(result, 32, "unset env var must use the provided default");
}

#[test]
fn resolve_max_windows_returns_default_on_invalid_values() {
    let invalid_values = [
        "0",    // zero is invalid
        "-1",   // negative is invalid
        "abc",  // non-numeric is invalid
        "32.5", // float is invalid
        "",     // empty string is invalid
    ];
    for invalid in &invalid_values {
        let result = yagura_daemon::config::resolve_max_windows(Some(invalid), 32);
        assert_eq!(
            result, 32,
            "invalid value {invalid:?} must return default (fail-closed)"
        );
    }
}

#[test]
fn resolve_max_windows_parses_valid_positive_integer() {
    let test_cases = [("1", 1), ("16", 16), ("100", 100), ("999", 999)];
    for (input, expected) in &test_cases {
        let result = yagura_daemon::config::resolve_max_windows(Some(input), 32);
        assert_eq!(
            result, *expected,
            "valid value {input} must be parsed correctly"
        );
    }
}

// resolve_close_settle_ms: missing / blank / non-numeric / zero all fall back to
// the provided default rather than disabling the wait entirely.

#[test]
fn resolve_close_settle_ms_returns_default_when_env_not_set() {
    let result = yagura_daemon::config::resolve_close_settle_ms(None, 200);
    assert_eq!(result, 200, "unset env var must use the provided default");
}

#[test]
fn resolve_close_settle_ms_returns_default_on_invalid_values() {
    let invalid_values = [
        "0",    // zero is invalid (would disable the wait entirely)
        "-1",   // negative is invalid
        "abc",  // non-numeric is invalid
        "32.5", // float is invalid
        "",     // empty string is invalid
        "   ",  // whitespace-only is invalid (blank, distinct from empty)
    ];
    for invalid in &invalid_values {
        let result = yagura_daemon::config::resolve_close_settle_ms(Some(invalid), 200);
        assert_eq!(
            result, 200,
            "invalid value {invalid:?} must return default (fail-safe)"
        );
    }
}

#[test]
fn resolve_close_settle_ms_parses_valid_positive_integer() {
    let test_cases = [("1", 1), ("200", 200), ("5000", 5000), ("60000", 60000)];
    for (input, expected) in &test_cases {
        let result = yagura_daemon::config::resolve_close_settle_ms(Some(input), 200);
        assert_eq!(
            result, *expected,
            "valid value {input} must be parsed correctly"
        );
    }
}

// resolve_input_echo_window_ms: same fail-safe shape as resolve_close_settle_ms,
// and a valid value surrounded by whitespace still parses.

#[test]
fn resolve_input_echo_window_ms_returns_default_when_env_not_set() {
    let result = yagura_daemon::config::resolve_input_echo_window_ms(None, 150);
    assert_eq!(result, 150, "unset env var must use the provided default");
}

#[test]
fn resolve_input_echo_window_ms_returns_default_on_invalid_values() {
    let invalid_values = [
        "0",    // zero is invalid (would disable echo attribution entirely)
        "-1",   // negative is invalid
        "abc",  // non-numeric is invalid
        "32.5", // float is invalid
        "",     // empty string is invalid
        "   ",  // whitespace-only is invalid (blank, distinct from empty)
    ];
    for invalid in &invalid_values {
        let result = yagura_daemon::config::resolve_input_echo_window_ms(Some(invalid), 150);
        assert_eq!(
            result, 150,
            "invalid value {invalid:?} must return default (fail-safe)"
        );
    }
}

#[test]
fn resolve_input_echo_window_ms_parses_valid_positive_integer() {
    let test_cases = [("1", 1), ("150", 150), ("5000", 5000), ("60000", 60000)];
    for (input, expected) in &test_cases {
        let result = yagura_daemon::config::resolve_input_echo_window_ms(Some(input), 150);
        assert_eq!(
            result, *expected,
            "valid value {input} must be parsed correctly"
        );
    }
}

#[test]
fn resolve_input_echo_window_ms_trims_leading_and_trailing_whitespace() {
    let result = yagura_daemon::config::resolve_input_echo_window_ms(Some("  250  "), 150);
    assert_eq!(
        result, 250,
        "a valid value padded with surrounding whitespace must still parse (same trim \
         behavior as resolve_close_settle_ms)"
    );
}

// resolve_resize_redraw_window_ms: same fail-safe shape, plus a positive check
// that a valid override is never clamped, unlike a bounded resolver.

#[test]
fn resolve_resize_redraw_window_ms_returns_default_when_env_not_set() {
    let result = yagura_daemon::config::resolve_resize_redraw_window_ms(None, 150);
    assert_eq!(result, 150, "unset env var must use the provided default");
}

#[test]
fn resolve_resize_redraw_window_ms_returns_default_on_invalid_values() {
    let invalid_values = [
        "0",    // zero is invalid (would disable resize attribution entirely)
        "-1",   // negative is invalid
        "abc",  // non-numeric is invalid
        "32.5", // float is invalid
        "",     // empty string is invalid
        "   ",  // whitespace-only is invalid (blank, distinct from empty)
    ];
    for invalid in &invalid_values {
        let result = yagura_daemon::config::resolve_resize_redraw_window_ms(Some(invalid), 150);
        assert_eq!(
            result, 150,
            "invalid value {invalid:?} must return default (fail-safe)"
        );
    }
}

#[test]
fn resolve_resize_redraw_window_ms_parses_valid_positive_integer() {
    let test_cases = [("1", 1), ("150", 150), ("5000", 5000), ("60000", 60000)];
    for (input, expected) in &test_cases {
        let result = yagura_daemon::config::resolve_resize_redraw_window_ms(Some(input), 150);
        assert_eq!(
            result, *expected,
            "valid value {input} must be parsed correctly"
        );
    }
}

#[test]
fn resolve_resize_redraw_window_ms_trims_leading_and_trailing_whitespace() {
    let result = yagura_daemon::config::resolve_resize_redraw_window_ms(Some("  250  "), 150);
    assert_eq!(
        result, 250,
        "a valid value padded with surrounding whitespace must still parse (same trim \
         behavior as resolve_close_settle_ms / resolve_input_echo_window_ms)"
    );
}

#[test]
fn resolve_resize_redraw_window_ms_does_not_clamp_a_large_override() {
    // An operator-supplied value far beyond the default must pass through
    // unmodified: a large window keeps output attributed for its whole span,
    // which is an accepted trade-off rather than something to clamp here.
    let result = yagura_daemon::config::resolve_resize_redraw_window_ms(Some("600000"), 150);
    assert_eq!(
        result, 600_000,
        "a large override must pass through unclamped"
    );
}

#[test]
fn parse_args_lets_every_hook_door_name_the_instance_it_was_registered_for() {
    for door in [
        &["--emit-status", "running", "--source", "hook"][..],
        &["--deliver-context"],
        &["--deliver-take", "--skip-if", "/subagentType"],
        &["--deliver-wait"],
        &["--report-session", "/session_id"],
    ] {
        let expected = parse_args(&args(door)).expect("the door alone parses");
        for server in ["yagura", "yagura-dev"] {
            let mut with = vec!["--for-instance", server];
            with.extend(door);
            assert_eq!(parse_args(&args(&with)), Ok(expected.clone()), "{with:?}");
        }
    }
}

#[test]
fn parse_args_rejects_for_instance_off_a_door_with_an_invalid_value_or_given_twice() {
    for command in [&[][..], &["--shutdown"]] {
        let mut with = command.to_vec();
        with.extend(["--for-instance", "yagura"]);
        assert_eq!(
            parse_args(&args(&with)),
            Err(CliError::UnknownArgument("--for-instance".to_string())),
            "{with:?}"
        );
    }
    for value in ["other", "yagura-", "yagura-Dev", ""] {
        assert_eq!(
            parse_args(&args(&["--deliver-take", "--for-instance", value])),
            Err(CliError::InvalidValue {
                flag: "--for-instance".to_string(),
                value: value.to_string(),
            }),
            "{value:?}"
        );
    }
    assert_eq!(
        parse_args(&args(&["--deliver-take", "--for-instance"])),
        Err(CliError::MissingValue {
            flag: "--for-instance".to_string(),
        })
    );
    assert_eq!(
        parse_args(&args(&[
            "--deliver-take",
            "--for-instance",
            "yagura",
            "--for-instance",
            "yagura",
        ])),
        Err(CliError::RepeatedArgument("--for-instance".to_string()))
    );
}

#[test]
fn parse_args_accepts_credentials_beside_a_hook_door_and_rejects_it_elsewhere() {
    for door in ["--deliver-take", "--deliver-context", "--deliver-wait"] {
        assert!(
            parse_args(&args(&["--credentials", "/tmp/c.json", door])).is_ok(),
            "{door} must take --credentials"
        );
    }
    assert!(
        parse_args(&args(&[
            "--credentials",
            "/tmp/c.json",
            "--emit-status",
            "idle"
        ]))
        .is_ok()
    );
    for command in [vec![], vec!["--shutdown"]] {
        let mut with = vec!["--credentials", "/tmp/c.json"];
        with.extend(command);
        assert_eq!(
            parse_args(&args(&with)),
            Err(CliError::UnknownArgument("--credentials".to_string()))
        );
    }
}

#[test]
fn parse_args_rejects_an_empty_missing_or_repeated_credentials_path() {
    assert_eq!(
        parse_args(&args(&["--deliver-take", "--credentials", ""])),
        Err(CliError::InvalidValue {
            flag: "--credentials".to_string(),
            value: String::new(),
        })
    );
    assert_eq!(
        parse_args(&args(&["--deliver-take", "--credentials"])),
        Err(CliError::MissingValue {
            flag: "--credentials".to_string(),
        })
    );
    assert_eq!(
        parse_args(&args(&[
            "--deliver-take",
            "--credentials",
            "a",
            "--credentials",
            "b"
        ])),
        Err(CliError::RepeatedArgument("--credentials".to_string()))
    );
}
