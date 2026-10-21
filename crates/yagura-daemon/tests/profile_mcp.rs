#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: a launch profile's `[mcp]` table -- the data that tells
//! the daemon how to hand a spawned window's CLI the argv elements and
//! settings-file content it needs to reach the daemon's own MCP bridge.
//!
//! Split from `bundled_profiles.rs` (which measures the specific profiles
//! this repository ships) because this file's subject is the parser and
//! renderer's general rules, exercised against inline TOML fixtures rather
//! than files under `tests/fixtures/profiles`, following `profile_load.rs`'s
//! own convention for a property no shared fixture file needs to carry.
//!
//! Three placeholders a rendered `args` element may spell are covered here:
//! `{bridge_toml}` (the bridge executable's path, as a TOML literal string),
//! `{env_names_toml}` (the fixed list of environment variable names the
//! bridge reads, as a TOML literal-string array) and `{mcp_config_file}`
//! (a settings file's path, substituted verbatim).

use std::path::Path;

use yagura_daemon::profile::{
    InjectedPaths, InjectionSpecError, ProfileParseError, parse_profile, render_mcp_args,
};

/// Decodes a single rendered TOML-literal-string element (`{bridge_toml}`'s
/// output) by wrapping it as `probe = <value>` and parsing that as TOML,
/// recovering the plain string a reader of the settings would see.
fn decode_toml_string(rendered: &str) -> String {
    #[derive(serde::Deserialize)]
    struct Probe {
        probe: String,
    }
    let source = format!("probe = {rendered}\n");
    let probe: Probe = toml::from_str(&source).unwrap_or_else(|err| {
        panic!("a rendered value must be valid TOML: {err}, source: {source:?}")
    });
    probe.probe
}

/// Decodes a rendered TOML-literal-string-array element (`{env_names_toml}`'s
/// output) the same way `decode_toml_string` does for a single string.
fn decode_toml_string_array(rendered: &str) -> Vec<String> {
    #[derive(serde::Deserialize)]
    struct Probe {
        probe: Vec<String>,
    }
    let source = format!("probe = {rendered}\n");
    let probe: Probe = toml::from_str(&source).unwrap_or_else(|err| {
        panic!("a rendered value must be valid TOML: {err}, source: {source:?}")
    });
    probe.probe
}

// ---------------------------------------------------------------------
// Reading and validating the [mcp] table
// ---------------------------------------------------------------------

#[test]
fn an_mcp_table_with_args_containing_no_placeholder_is_accepted() {
    let profile = parse_profile(
        "mcp-no-placeholder",
        "schema = 1\ndisplay_name = \"Mcp No Placeholder\"\n\n[mcp]\nargs = [\"--flag\", \
         \"value\"]\n",
    )
    .expect("args with no placeholder at all must be accepted");
    let spec = profile
        .mcp
        .expect("mcp-no-placeholder must ship an [mcp] table");

    assert_eq!(spec.args, vec!["--flag".to_string(), "value".to_string()]);
    assert_eq!(spec.config_file, None);
}

#[test]
fn an_mcp_table_using_all_three_known_placeholders_with_a_config_file_is_accepted() {
    let profile = parse_profile(
        "mcp-all-placeholders",
        "schema = 1\ndisplay_name = \"Mcp All Placeholders\"\n\n[mcp]\nconfig_file = \
         \"content\"\nargs = [\"{mcp_config_file}\", \"{bridge_toml}\", \
         \"{env_names_toml}\"]\n",
    )
    .expect("all three known placeholders together with a config_file must be accepted");
    let spec = profile
        .mcp
        .expect("mcp-all-placeholders must ship an [mcp] table");

    assert_eq!(spec.config_file, Some("content".to_string()));
    assert_eq!(spec.args.len(), 3);
}

#[test]
fn an_mcp_table_with_no_args_key_is_rejected() {
    let result = parse_profile(
        "mcp-no-args",
        "schema = 1\ndisplay_name = \"Mcp No Args\"\n\n[mcp]\nconfig_file = \"content\"\n",
    );

    assert!(
        matches!(
            result,
            Err(ProfileParseError::InvalidMcp(
                InjectionSpecError::MissingArgs
            ))
        ),
        "an [mcp] table with no args key must be rejected, got {result:?}"
    );
}

#[test]
fn an_arg_carrying_any_placeholder_other_than_the_three_known_ones_is_rejected() {
    let cases: [(&str, &str); 4] = [
        ("mcp-foo-placeholder", "args = [\"{foo}\"]"),
        ("mcp-value-placeholder", "args = [\"{value}\"]"),
        ("mcp-wrong-case-placeholder", "args = [\"{bridge_Toml}\"]"),
        (
            "mcp-json-embedded-placeholder",
            "args = [\"--x\", '{\"mcpServers\":{\"yagura\":{\"command\":\"{oops}\"}}}']",
        ),
    ];
    for (id, mcp_table) in cases {
        let source = format!("schema = 1\ndisplay_name = \"Mcp Reject\"\n\n[mcp]\n{mcp_table}\n");
        let result = parse_profile(id, &source);
        assert!(
            matches!(
                result,
                Err(ProfileParseError::InvalidMcp(
                    InjectionSpecError::UnknownPlaceholder
                ))
            ),
            "{id} must be rejected as carrying an unknown placeholder once the three \
             known ones are removed, got {result:?}"
        );
    }
}

#[test]
fn an_mcp_config_file_placeholder_without_a_config_file_key_is_rejected() {
    let result = parse_profile(
        "mcp-missing-config-file",
        "schema = 1\ndisplay_name = \"Mcp Missing Config File\"\n\n[mcp]\nargs = \
         [\"--mcp-config\", \"{mcp_config_file}\"]\n",
    );

    assert!(
        matches!(
            result,
            Err(ProfileParseError::InvalidMcp(
                InjectionSpecError::ConfigFileRequiredForPlaceholder
            ))
        ),
        "{{mcp_config_file}} used with no config_file key must be rejected, got \
         {result:?}"
    );
}

#[test]
fn an_mcp_table_with_an_unknown_key_is_rejected() {
    let result = parse_profile(
        "mcp-unknown-key",
        "schema = 1\ndisplay_name = \"Mcp Unknown Key\"\n\n[mcp]\nargs = [\"--x\"]\nbogus = \
         \"nope\"\n",
    );

    assert!(
        matches!(result, Err(ProfileParseError::Toml(_))),
        "an unknown key inside [mcp] must be a structural decode failure, matching how \
         an unknown key inside [model]/[effort] is rejected, got {result:?}"
    );
}

// ---------------------------------------------------------------------
// Rendering the three known placeholders
// ---------------------------------------------------------------------

#[test]
fn render_mcp_args_round_trips_the_bridge_path_through_a_toml_literal_string() {
    let profile = parse_profile(
        "mcp-bridge-toml",
        "schema = 1\ndisplay_name = \"Mcp Bridge Toml\"\n\n[mcp]\nargs = [\"--bridge\", \
         \"{bridge_toml}\"]\n",
    )
    .expect("a bare {bridge_toml} placeholder with no config_file must be accepted");
    let spec = profile
        .mcp
        .as_ref()
        .expect("mcp-bridge-toml must ship an [mcp] table");

    let bridge_path = Path::new(r"C:\Program Files\yagura\yagura-mcp.exe");
    let rendered = render_mcp_args(spec, "yagura", bridge_path, InjectedPaths::default())
        .expect("a bridge path with no quote or newline must render");

    assert_eq!(rendered[0], "--bridge");
    assert_eq!(
        decode_toml_string(&rendered[1]),
        bridge_path.to_string_lossy(),
        "decoding the rendered {{bridge_toml}} value must recover the exact bridge path"
    );
    assert!(
        !rendered[1].contains('"'),
        "a path containing backslashes and spaces must render with no double quote in \
         the element, got {:?}",
        rendered[1]
    );
}

#[test]
fn render_mcp_args_refuses_a_bridge_path_containing_a_single_quote_or_newline() {
    let profile = parse_profile(
        "mcp-bridge-toml-unsafe",
        "schema = 1\ndisplay_name = \"Mcp Bridge Toml Unsafe\"\n\n[mcp]\nargs = \
         [\"{bridge_toml}\"]\n",
    )
    .expect("a bare {bridge_toml} placeholder with no config_file must be accepted");
    let spec = profile
        .mcp
        .as_ref()
        .expect("mcp-bridge-toml-unsafe must ship an [mcp] table");

    for unsafe_path in [
        Path::new("C:\\it's\\yagura-mcp.exe"),
        Path::new("C:\\line\nbreak\\yagura-mcp.exe"),
    ] {
        let rendered = render_mcp_args(spec, "yagura", unsafe_path, InjectedPaths::default());
        assert_eq!(
            rendered, None,
            "a bridge path containing a single quote or newline cannot become a TOML \
             literal string, so rendering must refuse rather than emit a broken one: \
             {unsafe_path:?}"
        );
    }
}

#[test]
fn render_mcp_args_renders_env_names_toml_as_the_four_channel_names_in_order() {
    let profile = parse_profile(
        "mcp-env-names",
        "schema = 1\ndisplay_name = \"Mcp Env Names\"\n\n[mcp]\nargs = \
         [\"{env_names_toml}\"]\n",
    )
    .expect("a bare {env_names_toml} placeholder must be accepted");
    let spec = profile
        .mcp
        .as_ref()
        .expect("mcp-env-names must ship an [mcp] table");

    let bridge_path = Path::new("/opt/yagura/yagura-mcp");
    let rendered = render_mcp_args(spec, "yagura", bridge_path, InjectedPaths::default())
        .expect("nothing unsafe here to refuse");

    let mut expected: Vec<String> = yagura_protocol::MCP_CHANNEL_ENV_VARS
        .iter()
        .map(|name| (*name).to_string())
        .collect();
    expected.push(yagura_protocol::MCP_BRIDGE_ENV.to_string());

    assert_eq!(
        decode_toml_string_array(&rendered[0]),
        expected,
        "{{env_names_toml}} must decode to the window id, token, instance and bridge \
         environment variable names, in that order"
    );
}

#[test]
fn render_mcp_args_substitutes_the_mcp_config_file_placeholder_with_the_given_path() {
    let profile = parse_profile(
        "mcp-config-file",
        "schema = 1\ndisplay_name = \"Mcp Config File\"\n\n[mcp]\nconfig_file = \"irrelevant \
         content\"\nargs = [\"--mcp-config\", \"{mcp_config_file}\"]\n",
    )
    .expect("{mcp_config_file} together with a config_file must be accepted");
    let spec = profile
        .mcp
        .as_ref()
        .expect("mcp-config-file must ship an [mcp] table");

    let bridge_path = Path::new("/opt/yagura/yagura-mcp");
    let config_path = Path::new("/home/yagura/.config/yagura/mcp-abc123.json");
    let rendered = render_mcp_args(
        spec,
        "yagura",
        bridge_path,
        InjectedPaths {
            config_file: Some(config_path),
            ..InjectedPaths::default()
        },
    )
    .expect("nothing unsafe here to refuse");

    assert_eq!(
        rendered,
        vec![
            "--mcp-config".to_string(),
            config_path.to_string_lossy().into_owned(),
        ],
        "{{mcp_config_file}} must be substituted verbatim with the given path"
    );
}

#[test]
fn render_mcp_args_passes_through_an_arg_with_no_placeholder_unchanged() {
    let profile = parse_profile(
        "mcp-no-placeholder-render",
        "schema = 1\ndisplay_name = \"Mcp No Placeholder Render\"\n\n[mcp]\nargs = \
         [\"--flag\", \"literal-value\"]\n",
    )
    .expect("args with no placeholder must be accepted");
    let spec = profile
        .mcp
        .as_ref()
        .expect("mcp-no-placeholder-render must ship an [mcp] table");

    let rendered = render_mcp_args(
        spec,
        "yagura",
        Path::new("/opt/yagura/yagura-mcp"),
        InjectedPaths::default(),
    )
    .expect("nothing unsafe here to refuse");

    assert_eq!(
        rendered,
        vec!["--flag".to_string(), "literal-value".to_string()],
        "an arg with no placeholder must render unchanged"
    );
}

// ---------------------------------------------------------------------
// A profile without [mcp] has no injection
// ---------------------------------------------------------------------

#[test]
fn a_profile_without_an_mcp_table_has_no_injection() {
    let profile = parse_profile("mcp-absent", "schema = 1\ndisplay_name = \"Mcp Absent\"\n")
        .expect("a profile with no [mcp] table is still a valid document");

    assert!(
        profile.mcp.is_none(),
        "the absence of [mcp] means no window opened from this profile carries an \
         injection"
    );
}
