#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// `profile::list_profiles` is called directly here: it is the unit under test.
#![allow(clippy::disallowed_methods)]

//! Integration test: the Grok Build profiles this repository ships
//! (`profiles/grok-build.toml`, `profiles/wsl-grok-build.toml`), read from the real
//! files and run through the daemon's own loader, launch resolution and
//! registration rendering. No CLI is started.

mod common;

use std::collections::BTreeMap;

use common::docs::read_doc_verbatim;
use yagura_daemon::DaemonConfig;
use yagura_daemon::mcp_registration::{EntryVars, RegistrationTarget};
use yagura_daemon::profile::{
    LaunchOptionValues, ProfileLaunch, list_profiles, parse_profile, resolve_launch,
};
use yagura_os::program::HostOs;
use yagura_test_support::paths::TempDir;

const IDS: [&str; 2] = ["grok-build", "wsl-grok-build"];

fn shipped_source(id: &str) -> String {
    read_doc_verbatim(&format!("../../profiles/{id}.toml"))
}

/// A config whose profiles directory holds the shipped file for `id` and nothing else, so
/// the launch below runs the file as written rather than a bundled or user copy.
struct Placed {
    _tree: TempDir,
    config: DaemonConfig,
}

fn place(id: &str) -> Placed {
    let tree = TempDir::new("yagura-grok-test", id);
    let dir = tree.make_dir("profiles");
    std::fs::write(dir.join(format!("{id}.toml")), shipped_source(id)).expect("write profile");
    let config = DaemonConfig {
        profiles_dir: Some(dir),
        default_shell: None,
        detection_dir: None,
        ..DaemonConfig::default()
    };
    Placed {
        _tree: tree,
        config,
    }
}

fn launch(placed: &Placed, id: &str, model: Option<&str>, effort: Option<&str>) -> ProfileLaunch {
    let options = LaunchOptionValues {
        model: model.map(str::to_string),
        effort: effort.map(str::to_string),
    };
    resolve_launch(&placed.config, id, None, None, &options)
        .unwrap_or_else(|err| panic!("{id} must resolve a launch: {err}"))
}

#[test]
fn both_shipped_profiles_load_without_being_excluded() {
    for id in IDS {
        let placed = place(id);
        let listing = list_profiles(&placed.config);
        // A WSL profile names a command for Windows only, and a listing leaves out a profile with
        // no command for the host.
        let has_command_here = id == "grok-build" || cfg!(windows);

        assert_eq!(
            listing.profiles.iter().any(|entry| entry.id == id),
            has_command_here,
            "{id} must be listed exactly where it has a command, got {:?}",
            listing
                .profiles
                .iter()
                .map(|entry| &entry.id)
                .collect::<Vec<_>>()
        );
        assert!(
            listing.warnings.iter().all(|line| !line.contains(id)),
            "{id} must not be excluded or warned about: {:?}",
            listing.warnings
        );
    }
}

/// `-m` would write the user's own default model into their config; the profile carries the
/// model through the environment instead, so an alias reaches the CLI as a variable and
/// never as an argument.
#[test]
fn a_model_alias_reaches_grok_through_the_environment_and_not_argv() {
    let id = "grok-build";
    let placed = place(id);

    let plain = launch(&placed, id, None, None);
    let fast = launch(&placed, id, Some("fast"), None);

    assert_eq!(fast.env["GROK_DEFAULT_MODEL"], "grok-4.7-build-fast");
    assert!(
        !plain.env.contains_key("GROK_DEFAULT_MODEL"),
        "without a model the variable must stay unset so grok keeps the user's own default"
    );
    assert_eq!(
        fast.argv, plain.argv,
        "a model must add nothing to argv: grok's -m rewrites the user's config.toml"
    );
    assert_eq!(
        plain.argv,
        vec!["grok".to_string()],
        "a plain launch carries no approval-skip, trust or update flag"
    );
    let compat: BTreeMap<_, _> = plain
        .env
        .iter()
        .filter(|(name, _)| name.starts_with("GROK_"))
        .collect();
    assert!(
        ["CLAUDE", "CURSOR"].iter().all(|vendor| {
            ["HOOKS", "MCPS"]
                .iter()
                .all(|surface| compat[&format!("GROK_{vendor}_{surface}_ENABLED")] == "false")
        }),
        "the Claude and Cursor hook and MCP imports must be switched off: {compat:?}"
    );

    let effort = launch(&placed, id, None, Some("low"));
    assert_eq!(
        effort.argv[effort.argv.len() - 2..],
        ["--reasoning-effort".to_string(), "low".to_string()]
    );
}

#[cfg(windows)]
#[test]
fn the_wsl_profile_forwards_the_model_variable_into_wsl() {
    let id = "wsl-grok-build";
    let placed = place(id);

    let fast = launch(&placed, id, Some("fast"), None);

    assert_eq!(fast.env["GROK_DEFAULT_MODEL"], "grok-4.7-build-fast");
    let wslenv = &fast.env["WSLENV"];
    assert!(
        wslenv.split(':').any(|name| name == "GROK_DEFAULT_MODEL"),
        "the variable only crosses into WSL when WSLENV names it: {wslenv}"
    );
}

/// Every handler runs the daemon directly (no shell) through the `--hook` door of its own
/// profile, one handler per event, and what each event does (its status, the inbox, skipping
/// a sub-agent) is the profile's `[hook_events]` table, not the registered text. Every call
/// also names the instance it was registered for, so another instance's hook file stays
/// silent in this instance's windows.
#[test]
fn the_registered_hooks_call_the_profiles_hook_door_for_the_instance() {
    const DAEMON: &str = r"C:\yagura\yagura-daemon.exe";
    let events = [
        "UserPromptSubmit",
        "PreToolUse",
        "PostToolUse",
        "PostToolUseFailure",
        "Notification",
        "Stop",
        "StopFailure",
        "StopCancelled",
    ];

    for id in IDS {
        let profile = parse_profile(id, &shipped_source(id))
            .unwrap_or_else(|err| panic!("{id} must parse: {err}"));
        let table = profile
            .hook_events
            .as_ref()
            .unwrap_or_else(|| panic!("{id} must carry a [hook_events] table"));
        let spec = profile
            .mcp_registration
            .unwrap_or_else(|| panic!("{id} must carry a [mcp_registration] table"));
        let files = spec.json_entry().expect("a json_entry").files();
        assert_eq!(files.len(), 1, "{id}: one hook file per registration");

        let default_target = RegistrationTarget::for_instance(None);
        let dev_target = RegistrationTarget::for_instance(Some("dev"));
        assert_eq!(files[0].file(&default_target), ".grok/hooks/yagura.json");
        assert_eq!(
            files[0].file(&dev_target),
            ".grok/hooks/yagura-dev.json",
            "{id}: each instance owns a hook file of its own"
        );

        // Each OS renders for a different instance, so the name each door carries is
        // the one its own registration was written for.
        for (os, target) in [
            (HostOs::Windows, &default_target),
            (HostOs::Linux, &dev_target),
        ] {
            let vars = EntryVars {
                target,
                os,
                daemon: Some(DAEMON),
                bridge: None,
            };
            let entries = files[0].entries(&vars).expect("entries must render");
            let (key, hooks) = entries.first().expect("an entry");
            assert_eq!(key, "/hooks");

            let mut seen = BTreeMap::new();
            for (event, groups) in hooks.as_object().expect("hooks is an object") {
                let groups = groups.as_array().expect("an event holds a list");
                assert_eq!(groups.len(), 1, "{id}/{event}: one group");
                let handlers = groups[0]["hooks"].as_array().expect("hooks list");
                assert_eq!(handlers.len(), 1, "{id}/{event}: one handler per event");
                let handler = &handlers[0];
                assert_eq!(handler["command"], DAEMON, "{id}/{event}: no shell");
                let args: Vec<&str> = handler["args"]
                    .as_array()
                    .unwrap_or_else(|| panic!("{id}/{event}: args form"))
                    .iter()
                    .map(|arg| arg.as_str().unwrap())
                    .collect();
                assert_eq!(
                    args,
                    [
                        "--for-instance",
                        target.server(),
                        "--hook",
                        id,
                        event.as_str()
                    ],
                    "{id}/{event}: another instance's hook must stay silent"
                );
                if event == "Notification" {
                    assert_eq!(groups[0]["matcher"], "permission_prompt");
                }
                seen.insert(event.clone(), table.get(event).cloned());
            }
            assert_eq!(
                seen.keys().map(String::as_str).collect::<Vec<_>>(),
                {
                    let mut sorted = events.to_vec();
                    sorted.sort_unstable();
                    sorted
                },
                "{id} on {os:?}: the events grok runs"
            );
            assert!(
                seen.values().all(Option::is_some),
                "{id}: every hooked event has a [hook_events] entry: {seen:?}"
            );
        }
    }
}

#[test]
fn registration_carries_the_instance_blocks_other_instances_and_warns_about_trust() {
    for id in IDS {
        let spec = parse_profile(id, &shipped_source(id))
            .unwrap_or_else(|err| panic!("{id} must parse: {err}"))
            .mcp_registration
            .unwrap_or_else(|| panic!("{id} must carry a [mcp_registration] table"));
        let dev = RegistrationTarget::for_instance(Some("dev"));

        let add = spec.render_add(&dev, "BRIDGE").unwrap();
        assert_eq!(add[..2], ["mcp", "add"], "{id}: {add:?}");
        assert!(
            add.iter().any(|arg| arg == "--env=YAGURA_INSTANCE=dev"),
            "{id}: the instance must reach the bridge as one --env argument: {add:?}"
        );
        assert_eq!(add[add.len() - 3..], ["yagura-dev", "--", "BRIDGE"], "{id}");
        assert_eq!(
            spec.list()
                .expect("a list table")
                .render_disable(&["yagura-dev".to_string()]),
            ["--deny", "MCPTool(yagura-dev__*)"]
        );
        let notice = spec.notice().expect("a notice");
        assert!(
            notice.contains("trust") && notice.contains("hooks, MCP servers and instructions"),
            "{id}: the notice must say what answering yes to the trust question also allows: {notice}"
        );
    }
}
