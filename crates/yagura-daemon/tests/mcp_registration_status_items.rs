#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: a JSON `status` that picks the registration out of an
//! array by name (`items` / `name`), the way `list` already does, for a CLI
//! whose status output is one array of every registered server.

use serde_json::json;
use yagura_daemon::mcp_registration::{
    DerivedStatus, McpRegistrationSpec, RegistrationState, RegistrationTarget, StatusObservation,
    derive_status,
};
use yagura_daemon::profile::{ProfileParseError, parse_profile};
use yagura_os::program::Platform;

const BRIDGE: &str = "/opt/yagura/yagura-mcp";

fn profile_source(status_keys: &str) -> String {
    format!(
        "schema = 1\ndisplay_name = \"Neutral Cli\"\n\n[command]\ndefault = [\"neutral-cli\"]\n\n\
         [mcp_registration]\n\
         add = [\"mcp\", \"add\", \"{{server}}\", \"--\", \"{{bridge}}\"]\n\
         remove = [\"mcp\", \"remove\", \"{{server}}\"]\n\n\
         [mcp_registration.status]\n\
         args = [\"mcp\", \"list\", \"--json\"]\n\
         format = \"json\"\n\
         {status_keys}\n"
    )
}

fn spec(status_keys: &str) -> McpRegistrationSpec {
    parse_profile("neutral-cli", &profile_source(status_keys))
        .unwrap_or_else(|err| panic!("the fixture profile must parse: {err:?}"))
        .mcp_registration
        .expect("the fixture profile carries a registration table")
}

fn derive(
    spec: &McpRegistrationSpec,
    instance: Option<&str>,
    output: &serde_json::Value,
) -> DerivedStatus {
    derive_status(
        spec,
        &StatusObservation::Exited {
            code: Some(0),
            stdout: output.to_string().into_bytes(),
            stderr: Vec::new(),
        },
        &RegistrationTarget::for_instance(instance),
        BRIDGE,
        Platform::Unix,
    )
}

const FLAT: &str =
    "items = \"\"\nname = \"/name\"\ncommand = \"/command\"\nargs_pointer = \"/args\"";

#[test]
fn the_element_named_for_the_server_decides_registered_or_differs() {
    let spec = spec(FLAT);
    let other = json!({ "name": "other", "command": "/somewhere/else", "args": [] });

    let same = json!([other, { "name": "yagura", "command": BRIDGE, "args": [] }]);
    assert_eq!(
        derive(&spec, None, &same).state,
        RegistrationState::Registered
    );

    let moved = json!([{ "name": "yagura", "command": "/old/yagura-mcp", "args": [] }, other]);
    let derived = derive(&spec, None, &moved);
    assert_eq!(derived.state, RegistrationState::Differs, "{derived:?}");
    assert_eq!(derived.differs_in, vec!["command".to_string()]);

    // A named instance is matched under its own server name, not `yagura`.
    let two = json!([
        { "name": "yagura", "command": "/old/yagura-mcp", "args": [] },
        { "name": "yagura-dev", "command": BRIDGE, "args": [] },
    ]);
    assert_eq!(
        derive(&spec, Some("dev"), &two).state,
        RegistrationState::Registered
    );
    assert_eq!(derive(&spec, None, &two).state, RegistrationState::Differs);
}

#[test]
fn no_element_with_the_servers_name_is_not_registered_whatever_the_others_run() {
    let spec = spec(FLAT);
    let others_run_the_bridge = json!([
        { "name": "yagura-dev", "command": BRIDGE, "args": [] },
        { "name": "other", "command": BRIDGE, "args": [] },
    ]);
    let derived = derive(&spec, None, &others_run_the_bridge);
    assert_eq!(
        derived.state,
        RegistrationState::NotRegistered,
        "{derived:?}"
    );

    let empty = json!([]);
    assert_eq!(
        derive(&spec, None, &empty).state,
        RegistrationState::NotRegistered
    );
}

#[test]
fn command_and_args_pointers_are_read_from_the_element() {
    let spec = spec(
        "items = \"/servers\"\nname = \"/id\"\ncommand = \"/spec/command\"\nargs_pointer = \"/spec/args\"",
    );
    let bare = json!({ "servers": [
        { "id": "yagura", "spec": { "command": BRIDGE, "args": [] } },
    ], "command": "/decoy", "spec": { "command": "/decoy" } });
    assert_eq!(
        derive(&spec, None, &bare).state,
        RegistrationState::Registered
    );

    let leftover = json!({ "servers": [
        { "id": "yagura", "spec": { "command": BRIDGE, "args": ["--flag"] } },
    ]});
    let derived = derive(&spec, None, &leftover);
    assert_eq!(derived.state, RegistrationState::Differs, "{derived:?}");
}

#[test]
fn items_and_name_must_come_together_and_only_for_json() {
    // Positive control: the pair is accepted, so the refusals below come from
    // the pairing rule and not from an unknown key.
    parse_profile("neutral-cli", &profile_source(FLAT)).expect("items with name is accepted");

    for keys in [
        "items = \"\"
command = \"/command\"",
        "name = \"/name\"
command = \"/command\"",
    ] {
        let result = parse_profile("neutral-cli", &profile_source(keys));
        assert!(
            matches!(result, Err(ProfileParseError::InvalidMcpRegistration(_))),
            "{keys:?} must be an invalid registration table, got {result:?}"
        );
    }

    let lines = profile_source(
        "items = \"\"
name = \"/name\"
command = \"Command:\"",
    )
    .replace("format = \"json\"", "format = \"lines\"");
    let result = parse_profile("neutral-cli", &lines);
    assert!(
        matches!(result, Err(ProfileParseError::InvalidMcpRegistration(_))),
        "a lines status may not take items, got {result:?}"
    );
}
