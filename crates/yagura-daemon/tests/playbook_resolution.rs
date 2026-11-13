#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Playbook resolution (`resolve_playbook` in `cli.rs`) and the bundled default playbook.
//!
//! Playbooks come only from explicit sources, in order: the `--playbook` argument, then
//! `YAGURA_PLAYBOOK` (an empty value counts as unset), otherwise `None`. There is no
//! filesystem inference, so resolution is fail-closed until a path is passed explicitly.

use std::ffi::OsString;
use std::path::PathBuf;

use crate::common;

use common::store_fixtures::capture_warn_events;
use yagura_daemon::DaemonConfig;
use yagura_daemon::cli::resolve_playbook;
use yagura_daemon::config::{
    APPROVAL_TIMEOUT_DEFAULT_SECS, DEFAULT_INBOX_CAPACITY, DEFAULT_MAX_QUEUED_PER_SENDER,
};
use yagura_daemon::control::{ApprovalPolicy, DestructiveTool, apply_limits, parse_policy};

#[test]
fn resolve_playbook_prefers_the_argument_over_the_environment_variable() {
    let arg_path = PathBuf::from("arg-playbook.toml");
    let env_path = OsString::from("env-playbook.toml");

    let result = resolve_playbook(Some(arg_path.clone()), Some(env_path));

    assert_eq!(result, Some(arg_path));
}

#[test]
fn resolve_playbook_falls_back_to_the_environment_variable_when_no_argument_is_given() {
    let env_path = OsString::from("env-playbook.toml");

    let result = resolve_playbook(None, Some(env_path.clone()));

    assert_eq!(result, Some(PathBuf::from(env_path)));
}

#[test]
fn resolve_playbook_treats_an_empty_environment_value_as_unset() {
    let result = resolve_playbook(None, Some(OsString::from("")));

    assert_eq!(
        result, None,
        "an empty YAGURA_PLAYBOOK must not resolve to an empty-string path; it must be \
         treated as unset. With the exe-adjacent/ancestor search withdrawn, there is no \
         further stage to fall through to, so the outcome is None, not a resolved path"
    );
}

#[test]
fn resolve_playbook_returns_none_when_neither_source_is_set() {
    let result = resolve_playbook(None, None);

    assert_eq!(
        result, None,
        "with no --playbook argument and no YAGURA_PLAYBOOK, resolution must fail closed: \
         the daemon denies every destructive operation until the caller (the GUI) passes a \
         path explicitly -- no exe-adjacent or ancestor search exists to fall back to"
    );
}

/// Reads the repository's bundled `playbook.toml` and parses it the way the
/// daemon does, returning its path alongside the policy so a failure can name
/// the file.
fn bundled_playbook() -> (PathBuf, ApprovalPolicy) {
    // CARGO_MANIFEST_DIR = crates/yagura-daemon; the repository root is two levels up.
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir
        .parent()
        .expect("crates/yagura-daemon must have a parent (crates/)")
        .parent()
        .expect("crates/ must have a parent (the repository root)");
    let path = repo_root.join("playbook.toml");

    let source = std::fs::read_to_string(&path).unwrap_or_else(|err| {
        panic!(
            "the bundled playbook at {} must exist and be readable: the GUI embeds it with \
             include_str! and seeds it into the config root on first launch; error: {err}",
            path.display()
        )
    });
    let policy = parse_policy(&source).unwrap_or_else(|err| {
        panic!(
            "the bundled playbook at {} must parse as a valid ApprovalPolicy \
             (ApprovalPolicy uses #[serde(deny_unknown_fields)], so a typo in a key name \
             leaves the daemon denying everything instead of failing loudly here); \
             error: {err}",
            path.display()
        )
    });

    (path, policy)
}

#[test]
fn the_bundled_repository_playbook_auto_approves_exactly_send_chat_and_open_window() {
    // send_chat's destination set is
    // fail-closed to the room's participants and never reaches an Unknown
    // window, so it ships pre-approved. Every other gated tool, including
    // the all-participants broadcast_chat, still reaches a human.
    let (path, policy) = bundled_playbook();

    let Some(section) = policy.approval.as_ref() else {
        panic!(
            "the bundled playbook at {} must keep an [approval] table carrying \
             auto_approve = [\"send_chat\", \"open_window\"], so the one knob an operator edits to relax \
             the gate stays visible in the shipped file",
            path.display()
        )
    };
    assert_eq!(
        section.auto_approve,
        vec!["send_chat".to_string(), "open_window".to_string()],
        "the bundled playbook at {} must auto_approve exactly send_chat and open_window: every other \
         destructive tool a caller asks for is meant to reach a human through the \
         approval flow, and a name listed here silently removes that human. Found {:?}",
        path.display(),
        section.auto_approve
    );

    assert!(
        policy.allows(DestructiveTool::SendChat),
        "the bundled playbook must let send_chat run without a human"
    );
    assert!(
        policy.allows(DestructiveTool::OpenWindow),
        "the bundled playbook must let agents start windows without a human"
    );
    for tool in DestructiveTool::ALL.into_iter().filter(|tool| {
        !matches!(
            tool,
            DestructiveTool::SendChat | DestructiveTool::OpenWindow
        )
    }) {
        assert!(
            !policy.allows(tool),
            "the bundled playbook must not let {} run without a human, whatever shape \
             auto_approve matching takes",
            tool.as_tool_name()
        );
    }
}

#[test]
fn the_bundled_repository_playbook_keeps_the_daemon_approval_deadline() {
    let (path, policy) = bundled_playbook();

    let written = policy
        .limits()
        .and_then(|limits| limits.approval_timeout_secs);

    if let Some(seconds) = written {
        assert_eq!(
            seconds,
            APPROVAL_TIMEOUT_DEFAULT_SECS,
            "the bundled playbook at {} writes an approval deadline of {seconds}s while the \
             daemon's own default is {APPROVAL_TIMEOUT_DEFAULT_SECS}s; shipping a value that \
             differs from the built-in one makes the documented default a lie. Either drop \
             the key and let the daemon default apply, or write the same number",
            path.display()
        );
    }
}

// =============================================================================
// Keys of the typing the daemon no longer does: an old playbook that still
// writes them keeps loading, is told about them, and changes nothing with them.
// =============================================================================

/// The `[limits]` keys that bounded typing into a window and waiting for it to
/// be taken. With nothing typed, a value of theirs bounds nothing.
const DEPRECATED_LIMITS_KEYS: [&str; 2] = [
    "handoff_ready_timeout_secs",
    "handoff_delivery_timeout_secs",
];

/// Whether one of `events` is a WARN naming `needle` in its message or a field.
fn warned_about(events: &[common::store_fixtures::WarnEvent], needle: &str) -> bool {
    events.iter().any(|event| {
        event.message.contains(needle) || event.fields.values().any(|value| value.contains(needle))
    })
}

/// A playbook still writing a deprecated `[limits]` key loads whatever the value, even
/// one the old range refused, and the rest of the playbook takes effect; the load warns
/// naming the key, and merging its limits leaves every setting at its default.
#[test]
fn a_deprecated_limits_key_is_warned_about_and_changes_no_setting_whatever_its_value() {
    for key in DEPRECATED_LIMITS_KEYS {
        for seconds in [0_u64, 90, 601] {
            let source = format!(
                "[approval]\nauto_approve = [\"send_chat\"]\n\n[limits]\n{key} = {seconds}\n"
            );
            let (warns, parsed) = capture_warn_events(|| parse_policy(&source));
            let policy = parsed.unwrap_or_else(|err| {
                panic!(
                    "{key} = {seconds} is deprecated and must not fail the playbook load, \
                     got: {err}"
                )
            });
            assert!(
                policy.allows(DestructiveTool::SendChat),
                "the rest of a playbook carrying {key} = {seconds} must still take effect"
            );
            assert!(
                warned_about(&warns, key),
                "loading {key} = {seconds} must warn naming {key}; warned: {:?}",
                warns.iter().map(|event| &event.message).collect::<Vec<_>>()
            );
            let mut config = DaemonConfig::default();
            apply_limits(&mut config, policy.limits());
            assert_eq!(
                format!("{config:?}"),
                format!("{:?}", DaemonConfig::default()),
                "{key} = {seconds} must change no setting of the daemon"
            );
        }
    }
}

/// `[limits] max_queued_per_sender` sets how many envelopes one sender may leave in one
/// window. A value outside 1 to the inbox's capacity does not fail the load: merging it
/// warns naming the key and leaves the default.
#[test]
fn max_queued_per_sender_sets_the_bound_and_one_out_of_range_is_warned_about_and_keeps_the_default()
{
    for (value, expected, warns) in [
        (3, 3, false),
        (DEFAULT_INBOX_CAPACITY as u64, DEFAULT_INBOX_CAPACITY, false),
        (0, DEFAULT_MAX_QUEUED_PER_SENDER, true),
        (
            DEFAULT_INBOX_CAPACITY as u64 + 1,
            DEFAULT_MAX_QUEUED_PER_SENDER,
            true,
        ),
    ] {
        let policy = parse_policy(&format!("[limits]\nmax_queued_per_sender = {value}\n"))
            .unwrap_or_else(|err| panic!("max_queued_per_sender = {value} must load, got: {err}"));
        let mut config = DaemonConfig::default();
        let (events, ()) = capture_warn_events(|| apply_limits(&mut config, policy.limits()));
        assert_eq!(config.max_queued_per_sender, expected, "{value}");
        assert_eq!(
            warned_about(&events, "max_queued_per_sender"),
            warns,
            "{value}: warned: {:?}",
            events
                .iter()
                .map(|event| &event.message)
                .collect::<Vec<_>>()
        );
    }
}

/// `send_keys` in `auto_approve` names a tool that no longer exists: it is warned about
/// and approves nothing, while the other names in the same list still approve.
#[test]
fn send_keys_in_auto_approve_is_warned_about_and_approves_nothing() {
    let (warns, parsed) = capture_warn_events(|| {
        parse_policy("[approval]\nauto_approve = [\"send_keys\", \"send_chat\"]\n")
    });
    let policy = parsed.expect("send_keys in auto_approve must not fail the playbook load");

    assert!(
        warned_about(&warns, "send_keys"),
        "loading send_keys in auto_approve must warn naming it; warned: {:?}",
        warns.iter().map(|event| &event.message).collect::<Vec<_>>()
    );
    let approved: Vec<&str> = DestructiveTool::ALL
        .into_iter()
        .filter(|tool| policy.allows(*tool))
        .map(DestructiveTool::as_tool_name)
        .collect();
    assert_eq!(
        approved,
        ["send_chat"],
        "send_keys must approve no tool; only send_chat of the list may"
    );
}

#[test]
fn approval_timeout_secs_out_of_range_message_still_names_its_own_key() {
    // Two independently validated [limits] keys sharing one range-check
    // helper must not cross-contaminate their error messages. Match the
    // custom message text itself (not just the key substring, which the
    // TOML source-line snippet embedded in `err.to_string()` would also
    // satisfy vacuously) so a cross-contaminated message is caught.
    let result = parse_policy("[limits]\napproval_timeout_secs = 0\n");
    match result {
        Err(err) => {
            let message = err.to_string();
            let expected = "approval_timeout_secs must be between 1 and 600, got 0";
            assert!(
                message.contains(expected),
                "the error for an out-of-range approval_timeout_secs must still name \
                 approval_timeout_secs, not handoff_ready_timeout_secs, expected to find \
                 {expected:?} in {message:?}"
            );
        }
        Ok(_) => panic!("0 is outside 1..=600 and must fail the whole load"),
    }
}
