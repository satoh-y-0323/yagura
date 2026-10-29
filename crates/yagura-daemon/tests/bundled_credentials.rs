#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration tests: how each profile this repository ships hands a window's
//! credentials to its CLI.
//!
//! Which processes can claim to be a window is decided here: a profile in
//! `launch` mode hands the credentials file to its hook doors and its bridge
//! alone, and a profile that keeps the credentials in the environment relies
//! on its CLI to keep them from its tools. The shipped files are read as data.

use std::path::Path;

use yagura_daemon::profile::{
    CredentialsMode, InjectedPaths, InjectionSpec, Profile, ProfileCredentials, parse_profile,
    render_mcp_args,
};
use yagura_protocol::{INSTANCE_ENV, STATUS_TOKEN_ENV, WINDOW_ID_ENV};

/// The placeholder a shipped profile spells for the window's credentials file.
const CREDENTIALS_FILE: &str = "{credentials_file}";

/// The profiles in `launch` mode, with the `yagura-daemon` their hooks run.
const LAUNCH_PROFILES: [(&str, &str); 5] = [
    ("claude-code", "yagura-daemon"),
    ("wsl-claude-code", "yagura-daemon.exe"),
    ("copilot-cli", "yagura-daemon"),
    ("wsl-copilot-cli", "yagura-daemon.exe"),
    ("wsl-muse-code", "yagura-daemon.exe"),
];

fn shipped(id: &str) -> Profile {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../profiles")
        .join(format!("{id}.toml"));
    let source = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("cannot read {}: {err}", path.display()));
    parse_profile(id, &source)
        .unwrap_or_else(|err| panic!("bundled profile {id} must parse: {err}"))
}

/// The JSON objects in `texts`: a whole text, or the value of a `key=value` assignment.
fn json_objects(texts: impl IntoIterator<Item = String>) -> Vec<serde_json::Value> {
    texts
        .into_iter()
        .filter_map(|text| {
            serde_json::from_str::<serde_json::Value>(&text)
                .ok()
                .or_else(|| {
                    let (_, value) = text.split_once('=')?;
                    serde_json::from_str(value).ok()
                })
                .filter(serde_json::Value::is_object)
        })
        .collect()
}

/// Every settings text a table hands its CLI: the file, the directory's files, the args.
fn settings_texts(spec: &InjectionSpec, args: Vec<String>) -> Vec<String> {
    spec.config_file
        .iter()
        .cloned()
        .chain(spec.config_dir.iter().flat_map(|dir| dir.values().cloned()))
        .chain(args)
        .collect()
}

fn walk<'a>(value: &'a serde_json::Value, visit: &mut impl FnMut(&'a serde_json::Value)) {
    visit(value);
    match value {
        serde_json::Value::Object(map) => map.values().for_each(|v| walk(v, visit)),
        serde_json::Value::Array(items) => items.iter().for_each(|v| walk(v, visit)),
        _ => {}
    }
}

#[test]
fn the_launch_profiles_hand_every_door_and_the_bridge_the_credentials_file() {
    for (id, program) in LAUNCH_PROFILES {
        let profile = shipped(id);
        assert_eq!(
            profile.credentials.mode,
            CredentialsMode::Launch,
            "{id} must keep the credentials out of its window's environment"
        );

        let hooks = profile.hooks.as_ref().expect("a [hooks] table");
        let mut doors = Vec::new();
        for document in json_objects(settings_texts(hooks, hooks.args.clone())) {
            walk(&document, &mut |value| {
                if let Some(text) = value.as_str().filter(|text| text.contains("yagura-daemon")) {
                    doors.push(text.to_string());
                }
            });
        }
        assert!(!doors.is_empty(), "{id}: no yagura door found in its hooks");
        let handed = format!("{program} --credentials '{CREDENTIALS_FILE}' ");
        for door in &doors {
            assert!(
                door.starts_with(&handed),
                "{id}: every door must be handed the credentials file: {door}"
            );
        }

        // `{env_refs}` is filled later from the window's mode; any object parses here.
        let mcp = profile.mcp.as_ref().expect("an [mcp] table");
        let rendered =
            render_mcp_args(mcp, "yagura", Path::new("bridge"), InjectedPaths::default())
                .expect("the MCP args render")
                .into_iter()
                .map(|arg| arg.replace("{env_refs}", "{}"))
                .collect();
        let mut bridges = Vec::new();
        for document in json_objects(settings_texts(mcp, rendered)) {
            walk(&document, &mut |value| {
                if value.get("command").is_some() {
                    bridges.push(value.clone());
                }
            });
        }
        assert!(!bridges.is_empty(), "{id}: no MCP server found");
        for bridge in &bridges {
            assert_eq!(
                bridge["args"],
                serde_json::json!(["--credentials", CREDENTIALS_FILE]),
                "{id}: the bridge must be started with the credentials file: {bridge}"
            );
        }
    }
}

#[test]
fn wsl_muse_code_forwards_no_credentials_to_its_hooks() {
    let profile = shipped("wsl-muse-code");
    let hooks = profile.hooks.as_ref().expect("a [hooks] table");
    let forwarded: Vec<String> = hooks
        .args
        .iter()
        .find_map(|arg| arg.strip_prefix("managed_hooks_env_vars="))
        .map(|names| serde_json::from_str(names).expect("a JSON array of names"))
        .expect("the hooks name the environment they forward");
    assert!(
        forwarded.iter().any(|name| name == INSTANCE_ENV),
        "{forwarded:?}"
    );
    for name in [WINDOW_ID_ENV, STATUS_TOKEN_ENV] {
        assert!(
            !forwarded.iter().any(|forwarded| forwarded == name),
            "{name} must not reach the hooks through the environment: {forwarded:?}"
        );
    }
}

/// The value a `-c <key>=<value>` launch argument sets `key` to, read as TOML.
fn toml_assignment(arg: &str) -> (String, toml::Value) {
    let (key, value) = arg.split_once('=').expect("a key=value assignment");
    let mut table: toml::Table = format!("v = {value}").parse().expect("a TOML value");
    (key.to_string(), table.remove("v").unwrap())
}

#[test]
fn the_env_profiles_keep_their_credentials_and_codex_blanks_them_for_its_tools() {
    for id in ["codex-cli", "wsl-codex-cli"] {
        let credentials = shipped(id).credentials;
        assert_eq!(credentials.mode, CredentialsMode::Env, "{id}");
        let pairs: Vec<&[String]> = credentials.launch_args.chunks(2).collect();
        let mut blanked: Vec<(String, toml::Value)> = pairs
            .iter()
            .map(|pair| {
                assert_eq!(pair.len(), 2, "{id}: {:?}", credentials.launch_args);
                assert_eq!(pair[0], "-c", "{id}: {:?}", credentials.launch_args);
                toml_assignment(&pair[1])
            })
            .collect();
        blanked.sort_by(|a, b| a.0.cmp(&b.0));
        let mut expected: Vec<(String, toml::Value)> = [WINDOW_ID_ENV, STATUS_TOKEN_ENV]
            .iter()
            .map(|name| {
                (
                    format!("shell_environment_policy.set.{name}"),
                    toml::Value::String(String::new()),
                )
            })
            .collect();
        expected.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            blanked, expected,
            "{id}: its tools must get both credentials blank, and nothing else"
        );
        // An npm-installed codex is a `.cmd` shim; an argument cmd.exe would
        // interpret is dropped there, which would leave the credentials in the tools.
        for arg in &credentials.launch_args {
            assert!(
                !yagura_os::program::contains_cmd_metacharacter(arg),
                "{id}: a launch arg must survive a cmd shim: {arg}"
            );
        }
    }
    for id in [
        "muse-code",
        "grok-build",
        "wsl-grok-build",
        "antigravity-cli",
        "wsl-antigravity-cli",
        "shell",
    ] {
        assert_eq!(
            shipped(id).credentials,
            ProfileCredentials::default(),
            "{id} keeps the credentials in the environment"
        );
    }
}
