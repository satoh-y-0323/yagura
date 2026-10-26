#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// Reads tickets.toml directly; the daemon's bounded reader is not under test here.
#![allow(clippy::disallowed_methods)]

//! Integration test: the ticket rules a user configures in `tickets.toml`
//! (states, fields, the board guide), the required-field check a state
//! imposes, where the file is looked up, and the bounds on ticket text.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;

use yagura_daemon::cli::{CliCommand, parse_args, resolve_tickets_path};
use yagura_daemon::ticket::{
    read_ticket_config, validate_milestone_description, validate_milestone_name,
    validate_milestone_title, validate_ticket_body, validate_ticket_field_value,
    validate_ticket_result, validate_ticket_title,
};
use yagura_os::InstanceNamespace;
use yagura_test_support::paths::TempDir;

const MAX_CONFIG_BYTES: usize = 64 * 1024;

type NamedCheck = (&'static str, fn(&str) -> bool);
const MAX_STATES: usize = 32;
const MAX_FIELDS: usize = 32;

fn args(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

fn default_namespace() -> InstanceNamespace {
    yagura_os::resolve_instance_from(None).expect("the default instance always resolves")
}

fn dev_namespace() -> InstanceNamespace {
    yagura_os::resolve_instance_from(Some(std::ffi::OsStr::new("dev")))
        .expect("\"dev\" is a valid identifier and must resolve")
}

/// Renders a `tickets.toml` with the given states (name, requires) and field
/// names, in that order.
fn config_text(states: &[(&str, &[&str])], fields: &[&str]) -> String {
    let mut text = String::from("schema = 1\nguide = \"Keep one purpose per ticket.\"\n");
    for (name, requires) in states {
        let requires = requires
            .iter()
            .map(|field| format!("\"{field}\""))
            .collect::<Vec<_>>()
            .join(", ");
        text.push_str(&format!(
            "\n[[state]]\nname = \"{name}\"\ndescription = \"State {name}.\"\nrequires = [{requires}]\n"
        ));
    }
    for name in fields {
        text.push_str(&format!(
            "\n[[field]]\nname = \"{name}\"\ndescription = \"Field {name}.\"\n"
        ));
    }
    text
}

fn write_config(tag: &str, text: &str) -> (TempDir, PathBuf) {
    let tree = TempDir::new("yagura-ticket-rules", tag);
    let path = tree.write_file("tickets.toml", text);
    (tree, path)
}

fn assert_invalid(tag: &str, text: &str, why: &str) {
    let (_tree, path) = write_config(tag, text);
    let result = read_ticket_config(&path);
    assert!(
        result.is_err(),
        "{why}: the file must read as invalid, got {result:?}"
    );
}

fn fields(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect()
}

// --- reading the file ---------------------------------------------------

#[test]
fn reads_states_and_fields_in_file_order_and_the_first_state_is_the_default() {
    let text = config_text(
        &[("waiting", &[]), ("active", &["owner"]), ("archived", &[])],
        &["owner"],
    );
    let (_tree, path) = write_config("reads-in-order", &text);

    let config = read_ticket_config(&path)
        .expect("a well-formed file must read")
        .expect("an existing file must read as configured");

    let names: Vec<&str> = config.states.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["waiting", "active", "archived"]);
    assert_eq!(config.states[1].requires, vec!["owner".to_string()]);
    assert_eq!(config.states[1].description, "State active.");
    let field_names: Vec<&str> = config.fields.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(field_names, ["owner"]);
    assert_eq!(config.guide, "Keep one purpose per ticket.");
    assert_eq!(
        config.default_state(),
        "waiting",
        "the first [[state]] is the state a new ticket starts in, not an alphabetical or \
         hard-coded one"
    );
}

#[test]
fn a_missing_file_reads_as_not_configured_and_a_broken_file_as_invalid() {
    let tree = TempDir::new("yagura-ticket-rules", "missing-vs-broken");
    let missing = tree.path().join("tickets.toml");
    let result = read_ticket_config(&missing);
    assert!(
        matches!(result, Ok(None)),
        "an absent tickets.toml is \"not configured\", not an error, got {result:?}"
    );

    let broken = tree.write_file("broken.toml", "schema = 1\n[[state]\n");
    let result = read_ticket_config(&broken);
    assert!(
        result.is_err(),
        "a file that does not parse is invalid, distinct from absent, got {result:?}"
    );
}

#[test]
fn the_reason_for_an_invalid_file_never_echoes_its_content() {
    let marker = "SECRETMARKER7731";
    let unparsable = format!("schema = 1\nguide = \"{marker}\n");
    let bad_name = config_text(&[(&format!("{marker} Upper"), &[])], &[]);
    let bad_requires = config_text(&[("todo", &[marker])], &[]);

    for (tag, text) in [
        ("echo-unparsable", unparsable),
        ("echo-bad-name", bad_name),
        ("echo-bad-requires", bad_requires),
    ] {
        let (_tree, path) = write_config(tag, &text);
        let err = read_ticket_config(&path).expect_err("the file must read as invalid");
        let reason = err.to_string();
        assert!(
            !reason.is_empty(),
            "{tag}: an invalid file must carry a reason"
        );
        assert!(
            !reason.contains(marker),
            "{tag}: the reason must not echo the file's content, got {reason:?}"
        );
    }
}

#[test]
fn rejects_an_unsupported_schema() {
    let text = config_text(&[("todo", &[])], &[]).replace("schema = 1", "schema = 2");
    assert_invalid("schema-2", &text, "schema other than 1");
}

#[test]
fn rejects_state_and_field_names_that_are_not_tokens() {
    assert_invalid(
        "bad-state-name",
        &config_text(&[("In Progress", &[])], &[]),
        "a state name outside is_valid_token",
    );
    assert_invalid(
        "bad-field-name",
        &config_text(&[("todo", &[])], &["Reason"]),
        "a field name outside is_valid_token",
    );
}

#[test]
fn rejects_duplicated_state_or_field_names() {
    assert_invalid(
        "dup-state",
        &config_text(&[("todo", &[]), ("todo", &[])], &[]),
        "two states with the same name",
    );
    assert_invalid(
        "dup-field",
        &config_text(&[("todo", &[])], &["reason", "reason"]),
        "two fields with the same name",
    );
}

#[test]
fn rejects_requires_that_names_an_undefined_field() {
    assert_invalid(
        "undefined-requires",
        &config_text(&[("on-hold", &["resume_trigger"])], &["reason"]),
        "requires naming a field no [[field]] defines",
    );
}

#[test]
fn rejects_zero_states() {
    assert_invalid(
        "zero-states",
        &config_text(&[], &["reason"]),
        "a file with no [[state]]",
    );
}

#[test]
fn accepts_thirty_two_states_and_rejects_thirty_three() {
    let names: Vec<String> = (0..=MAX_STATES).map(|i| format!("s{i}")).collect();
    let states = |count: usize| -> Vec<(&str, &[&str])> {
        names[..count]
            .iter()
            .map(|n| (n.as_str(), &[] as &[&str]))
            .collect()
    };

    let (_tree, path) = write_config("states-32", &config_text(&states(MAX_STATES), &[]));
    let result = read_ticket_config(&path);
    assert!(
        matches!(result, Ok(Some(_))),
        "{MAX_STATES} states is within the limit, got {result:?}"
    );

    assert_invalid(
        "states-33",
        &config_text(&states(MAX_STATES + 1), &[]),
        "33 states",
    );
}

#[test]
fn accepts_thirty_two_fields_and_rejects_thirty_three() {
    let names: Vec<String> = (0..=MAX_FIELDS).map(|i| format!("f{i}")).collect();
    let fields =
        |count: usize| -> Vec<&str> { names[..count].iter().map(String::as_str).collect() };

    let (_tree, path) = write_config(
        "fields-32",
        &config_text(&[("todo", &[])], &fields(MAX_FIELDS)),
    );
    let result = read_ticket_config(&path);
    assert!(
        matches!(result, Ok(Some(_))),
        "{MAX_FIELDS} fields is within the limit, got {result:?}"
    );

    assert_invalid(
        "fields-33",
        &config_text(&[("todo", &[])], &fields(MAX_FIELDS + 1)),
        "33 fields",
    );
}

/// A valid config padded with a trailing comment to exactly `total` bytes.
fn padded_config(total: usize) -> String {
    let mut text = config_text(&[("todo", &[])], &[]);
    let padding = total - text.len() - 2;
    text.push('#');
    text.push_str(&"x".repeat(padding));
    text.push('\n');
    assert_eq!(text.len(), total);
    text
}

#[test]
fn accepts_a_file_of_exactly_sixty_four_kib_and_rejects_one_byte_more() {
    let (_tree, path) = write_config("size-64k", &padded_config(MAX_CONFIG_BYTES));
    let result = read_ticket_config(&path);
    assert!(
        matches!(result, Ok(Some(_))),
        "a file of exactly 64 KiB is within the limit, got {result:?}"
    );

    assert_invalid(
        "size-64k-plus-1",
        &padded_config(MAX_CONFIG_BYTES + 1),
        "a file over 64 KiB",
    );
}

// --- the required-field check -------------------------------------------

#[test]
fn reports_the_required_fields_that_are_absent_empty_or_blank_in_config_order() {
    let text = config_text(
        &[("todo", &[]), ("blocked", &["zeta", "alpha", "mid"])],
        &["zeta", "alpha", "mid"],
    );
    let (_tree, path) = write_config("missing-required", &text);
    let config = read_ticket_config(&path).unwrap().unwrap();

    assert_eq!(
        config.missing_required("blocked", &fields(&[])),
        vec!["zeta".to_string(), "alpha".to_string(), "mid".to_string()],
        "every required field is missing, reported in the order the config lists them"
    );
    assert_eq!(
        config.missing_required(
            "blocked",
            &fields(&[("zeta", ""), ("alpha", " \t\n "), ("mid", "x")])
        ),
        vec!["zeta".to_string(), "alpha".to_string()],
        "an empty value and a whitespace-only value both count as missing"
    );
    assert!(
        config
            .missing_required(
                "blocked",
                &fields(&[("zeta", "a"), ("alpha", "b"), ("mid", "c")])
            )
            .is_empty(),
        "with every required field filled, the check passes"
    );
    assert!(
        config.missing_required("todo", &fields(&[])).is_empty(),
        "a state with no requires passes with no fields"
    );
}

// --- the bundled reference config ---------------------------------------

#[test]
fn the_bundled_reference_config_reads_and_carries_the_documented_states() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tickets.toml");
    let config = read_ticket_config(&path)
        .expect("the bundled tickets.toml must not be invalid")
        .expect("the bundled tickets.toml must exist at the repository root");

    let states: Vec<(&str, Vec<&str>)> = config
        .states
        .iter()
        .map(|s| {
            (
                s.name.as_str(),
                s.requires.iter().map(String::as_str).collect(),
            )
        })
        .collect();
    assert_eq!(
        states,
        vec![
            ("todo", vec![]),
            ("in-progress", vec![]),
            ("done", vec![]),
            ("considering", vec!["start_condition"]),
            ("on-hold", vec!["resume_trigger"]),
            ("cancelled", vec!["reason"]),
        ]
    );
    assert!(
        !config.guide.trim().is_empty(),
        "the guide must not be empty"
    );
}

// --- where the file is looked up ----------------------------------------

#[test]
fn parse_args_tickets_flag_resolves_tickets_arg() {
    let path = if cfg!(windows) {
        "C:\\some\\tickets.toml"
    } else {
        "/some/tickets.toml"
    };
    match parse_args(&args(&["--tickets", path])).expect("--tickets should parse") {
        CliCommand::Run { tickets_arg, .. } => {
            assert_eq!(tickets_arg, Some(PathBuf::from(path)));
        }
        other => panic!("expected CliCommand::Run, got {other:?}"),
    }
    match parse_args(&args(&[])).expect("empty args should parse") {
        CliCommand::Run { tickets_arg, .. } => assert_eq!(tickets_arg, None),
        other => panic!("expected CliCommand::Run, got {other:?}"),
    }
}

#[test]
fn parse_args_rejects_tickets_without_a_value_or_with_an_empty_one() {
    let result = parse_args(&args(&["--tickets"]));
    assert!(
        result.is_err(),
        "--tickets with no value must be rejected, got {result:?}"
    );
    let result = parse_args(&args(&["--tickets", ""]));
    assert!(
        result.is_err(),
        "an empty --tickets must be rejected, got {result:?}"
    );
}

#[test]
fn resolve_tickets_path_prefers_arg_then_env_then_the_config_root() {
    let tree = TempDir::new("yagura-ticket-rules", "resolve-order");
    let arg_path = tree.write_file("arg-tickets.toml", "");
    let env_path = tree.write_file("env-tickets.toml", "");
    let base = tree.make_dir("config-base");
    let ns = default_namespace();

    assert_eq!(
        resolve_tickets_path(
            Some(arg_path.clone()),
            Some(OsString::from(&env_path)),
            Some(base.clone()),
            &ns
        ),
        Some(arg_path)
    );
    assert_eq!(
        resolve_tickets_path(
            None,
            Some(OsString::from(&env_path)),
            Some(base.clone()),
            &ns
        ),
        Some(env_path)
    );
    assert_eq!(
        resolve_tickets_path(None, Some(OsString::new()), Some(base.clone()), &ns),
        Some(yagura_os::instance_dir(&base, &ns).join("tickets.toml")),
        "an empty YAGURA_TICKETS counts as unset"
    );
    assert_eq!(
        resolve_tickets_path(None, None, Some(base.clone()), &ns),
        Some(yagura_os::instance_dir(&base, &ns).join("tickets.toml")),
        "the config-root path is returned whether or not the file exists"
    );
    assert_eq!(resolve_tickets_path(None, None, None, &ns), None);
    assert_ne!(
        resolve_tickets_path(None, None, Some(base.clone()), &ns),
        resolve_tickets_path(None, None, Some(base), &dev_namespace()),
        "a named instance has its own tickets.toml"
    );
}

// --- text bounds --------------------------------------------------------

fn assert_bound(name: &str, validate: fn(&str) -> bool, max: usize) {
    assert!(validate(&"a".repeat(max)), "{name}: {max} bytes must pass");
    assert!(
        !validate(&"a".repeat(max + 1)),
        "{name}: {} bytes must be rejected",
        max + 1
    );
}

#[test]
fn ticket_and_milestone_text_is_bounded_at_the_designed_limits() {
    assert_bound("title", |v| validate_ticket_title(v).is_ok(), 128);
    assert_bound("body", |v| validate_ticket_body(v).is_ok(), 16 * 1024);
    assert_bound(
        "field value",
        |v| validate_ticket_field_value(v).is_ok(),
        2 * 1024,
    );
    assert_bound("result", |v| validate_ticket_result(v).is_ok(), 4 * 1024);
    assert_bound("milestone name", |v| validate_milestone_name(v).is_ok(), 64);
    assert_bound(
        "milestone title",
        |v| validate_milestone_title(v).is_ok(),
        128,
    );
    assert_bound(
        "milestone description",
        |v| validate_milestone_description(v).is_ok(),
        2 * 1024,
    );
}

#[test]
fn multiline_text_keeps_newlines_and_tabs_and_rejects_other_controls() {
    let multiline: [NamedCheck; 4] = [
        ("body", |v| validate_ticket_body(v).is_ok()),
        ("field value", |v| validate_ticket_field_value(v).is_ok()),
        ("result", |v| validate_ticket_result(v).is_ok()),
        ("milestone description", |v| {
            validate_milestone_description(v).is_ok()
        }),
    ];
    for (name, validate) in multiline {
        assert!(
            validate("first line\n\tsecond line"),
            "{name}: \\n and \\t are kept"
        );
        assert!(!validate("a\u{1b}[31mb"), "{name}: ESC is rejected");
        assert!(!validate("a\rb"), "{name}: a bare CR is rejected");
        assert!(
            !validate("a\u{202e}b"),
            "{name}: a bidi control is rejected"
        );
    }

    assert!(validate_ticket_title("Ship the board").is_ok());
    assert!(
        validate_ticket_title("two\nlines").is_err(),
        "a title is one line"
    );
    assert!(
        validate_milestone_title("two\nlines").is_err(),
        "a milestone title is one line"
    );
    assert!(validate_milestone_name("v1-launch").is_ok());
    assert!(
        validate_milestone_name("V1 Launch").is_err(),
        "a milestone name is a token"
    );
}
