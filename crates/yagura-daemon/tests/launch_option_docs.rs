#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: the bundled launch profiles this repository ships
//! (`profiles/*.toml`) carry the `[model]` / `[effort]` launch-option tables
//! with the documented aliases, and applying a documented launch option to
//! them resolves the alias and appends the flag.
//!
//! Split from `profile_load.rs` (which measures `profile.rs`'s own parsing
//! and resolution rules against synthetic fixtures) because this file's
//! subject is the specific profiles the repository ships rather than the
//! parser's general behaviour.

mod common;

use common::docs::read_doc_verbatim;
use yagura_daemon::profile::{
    LaunchOptionAxis, LaunchOptionSpec, LaunchOptionValues, ProfileLaunchError,
    apply_launch_options, parse_profile, resolve_option_value,
};

/// Reads and parses one of the repository's own shipped profiles by id
/// (`profiles/<id>.toml`, relative to the repository root).
///
/// # Panics
///
/// Panics if the file cannot be read or does not parse as a valid profile: a
/// shipped profile that fails either is a contract violation this test must
/// surface loudly, not one it should silently skip.
fn parse_repo_profile(id: &str) -> yagura_daemon::profile::Profile {
    let rel = format!("../../profiles/{id}.toml");
    let source = read_doc_verbatim(&rel);
    parse_profile(id, &source)
        .unwrap_or_else(|_| panic!("bundled profile {id} must parse as a valid profile"))
}

/// The alias keys of `spec`, in the file's own order.
fn alias_keys(spec: &LaunchOptionSpec) -> Vec<&str> {
    spec.aliases.iter().map(|(key, _)| key.as_str()).collect()
}

// --- the bundled profiles ship the `[model]` / `[effort]` contract ---------

#[test]
fn claude_code_family_profiles_ship_model_and_effort_tables_with_the_documented_aliases() {
    for id in ["claude-code", "wsl-claude-code"] {
        let profile = parse_repo_profile(id);
        let model = profile
            .options
            .model
            .as_ref()
            .unwrap_or_else(|| panic!("{id} must ship a [model] table"));
        assert_eq!(
            alias_keys(model),
            ["fast", "default", "strong", "fable"],
            "{id}'s [model] table must declare exactly these aliases, in this order"
        );
        assert!(
            model.args.iter().any(|arg| arg.contains("{value}")),
            "{id}'s [model] args must contain the {{value}} placeholder, found {:?}",
            model.args
        );

        let effort = profile
            .options
            .effort
            .as_ref()
            .unwrap_or_else(|| panic!("{id} must ship an [effort] table"));
        assert!(
            alias_keys(effort).is_empty(),
            "{id}'s [effort] table must declare no aliases, found {:?}",
            alias_keys(effort)
        );
        assert!(
            effort.args.iter().any(|arg| arg.contains("{value}")),
            "{id}'s [effort] args must contain the {{value}} placeholder, found {:?}",
            effort.args
        );
    }
}

#[test]
fn codex_cli_family_profiles_ship_model_and_effort_tables_with_the_documented_aliases() {
    for id in ["codex-cli", "wsl-codex-cli"] {
        let profile = parse_repo_profile(id);
        let model = profile
            .options
            .model
            .as_ref()
            .unwrap_or_else(|| panic!("{id} must ship a [model] table"));
        assert_eq!(
            alias_keys(model),
            ["fast", "default", "strong"],
            "{id}'s [model] table must declare exactly these aliases, in this order"
        );
        assert!(
            model.args.iter().any(|arg| arg.contains("{value}")),
            "{id}'s [model] args must contain the {{value}} placeholder, found {:?}",
            model.args
        );

        let effort = profile
            .options
            .effort
            .as_ref()
            .unwrap_or_else(|| panic!("{id} must ship an [effort] table"));
        assert!(
            alias_keys(effort).is_empty(),
            "{id}'s [effort] table must declare no aliases, found {:?}",
            alias_keys(effort)
        );
        assert!(
            effort.args.iter().any(|arg| arg.contains("{value}")),
            "{id}'s [effort] args must contain the {{value}} placeholder, found {:?}",
            effort.args
        );
    }
}

#[test]
fn shell_profile_maps_neither_launch_option_axis() {
    let profile = parse_repo_profile("shell");
    assert!(
        profile.options.model.is_none(),
        "shell must not map the model axis"
    );
    assert!(
        profile.options.effort.is_none(),
        "shell must not map the effort axis"
    );
}

#[test]
fn antigravity_cli_family_profiles_select_the_antigravity_detection_and_ship_only_a_model_table() {
    for id in ["antigravity-cli", "wsl-antigravity-cli"] {
        let profile = parse_repo_profile(id);
        assert_eq!(
            profile.detection.as_deref(),
            Some("antigravity-cli"),
            "{id} must select the antigravity-cli detection definition"
        );

        let model = profile
            .options
            .model
            .as_ref()
            .unwrap_or_else(|| panic!("{id} must ship a [model] table"));
        assert_eq!(
            alias_keys(model),
            ["fast", "default", "strong"],
            "{id}'s [model] table must declare exactly these aliases, in this order"
        );
        assert!(
            model.args.iter().any(|arg| arg.contains("{value}")),
            "{id}'s [model] args must contain the {{value}} placeholder, found {:?}",
            model.args
        );

        assert!(
            profile.options.effort.is_none(),
            "{id} must ship no [effort] table at all: agy has no independent effort flag, \
             so effort is selected only through a [model] alias"
        );
    }
}

// --- the bundled profiles' mapping (pure functions) -------------------------

#[test]
fn applying_documented_launch_options_to_the_bundled_claude_code_profile_resolves_aliases_and_appends_them_in_order()
 {
    let profile = parse_repo_profile("claude-code");
    let model_spec = profile
        .options
        .model
        .as_ref()
        .expect("claude-code must ship a [model] table for this test to be meaningful");
    let effort_spec = profile
        .options
        .effort
        .as_ref()
        .expect("claude-code must ship an [effort] table for this test to be meaningful");

    let strong_value = resolve_option_value(model_spec, "strong");
    assert_ne!(
        strong_value, "strong",
        "the strong alias must resolve to a different, vendor-specific value"
    );
    let high_value = resolve_option_value(effort_spec, "high");
    assert_eq!(
        high_value, "high",
        "effort has no aliases, so its value must pass through unchanged"
    );

    let mut argv = vec!["claude".to_string()];
    apply_launch_options(
        &mut argv,
        &profile.options,
        &LaunchOptionValues {
            model: Some("strong".to_string()),
            effort: Some("high".to_string()),
        },
    )
    .expect("claude-code must map both model and effort");

    let tail = &argv[argv.len() - 4..];
    assert_eq!(tail, ["--model", strong_value, "--effort", "high"]);
}

#[test]
fn applying_a_documented_model_alias_to_the_bundled_codex_cli_profile_resolves_it_and_appends_it() {
    let profile = parse_repo_profile("codex-cli");
    let model_spec = profile
        .options
        .model
        .as_ref()
        .expect("codex-cli must ship a [model] table for this test to be meaningful");

    let fast_value = resolve_option_value(model_spec, "fast");
    assert_ne!(
        fast_value, "fast",
        "the fast alias must resolve to a different, vendor-specific value"
    );

    let mut argv = vec!["codex".to_string()];
    apply_launch_options(
        &mut argv,
        &profile.options,
        &LaunchOptionValues {
            model: Some("fast".to_string()),
            effort: None,
        },
    )
    .expect("codex-cli must map model");

    let tail = &argv[argv.len() - 2..];
    assert_eq!(tail, ["-m", fast_value]);
}

#[test]
fn applying_an_effort_launch_option_to_the_bundled_antigravity_cli_profile_is_refused() {
    let profile = parse_repo_profile("antigravity-cli");
    let mut argv = vec!["agy".to_string()];

    let err = apply_launch_options(
        &mut argv,
        &profile.options,
        &LaunchOptionValues {
            model: None,
            effort: Some("high".to_string()),
        },
    )
    .expect_err("antigravity-cli ships no [effort] table, so an effort value must be refused");

    assert!(
        matches!(
            err,
            ProfileLaunchError::UnsupportedLaunchOption(LaunchOptionAxis::Effort)
        ),
        "the refusal must be the same UnsupportedLaunchOption(Effort) error every other \
         effort-less profile produces, got {err:?}"
    );
    assert_eq!(
        argv,
        vec!["agy".to_string()],
        "a refused launch option must not leave a partially built argv behind"
    );
}

// --- self-tests for the `common::docs` helpers ------------------------------

#[test]
fn read_doc_verbatim_reads_the_real_mcp_setup_doc() {
    let doc = read_doc_verbatim("../../docs/mcp-setup.md");
    assert!(
        doc.lines().any(|line| line == "### send_handoff"),
        "the real docs/mcp-setup.md must have a \"### send_handoff\" heading"
    );
}
