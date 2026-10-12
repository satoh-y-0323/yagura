#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Playbook resolution (`resolve_playbook` in `cli.rs`) and the bundled default playbook.
//!
//! Playbooks come only from explicit sources, in order: the `--playbook` argument, then
//! `YAGURA_PLAYBOOK` (an empty value counts as unset), otherwise `None`. There is no
//! filesystem inference, so resolution is fail-closed until a path is passed explicitly.

use std::ffi::OsString;
use std::path::PathBuf;

use yagura_daemon::cli::resolve_playbook;
use yagura_daemon::config::APPROVAL_TIMEOUT_DEFAULT_SECS;
use yagura_daemon::control::{ApprovalPolicy, DestructiveTool, parse_policy};

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
    for tool in [
        DestructiveTool::CloseWindow,
        DestructiveTool::InterruptWindow,
        DestructiveTool::SendMessage,
        DestructiveTool::SendKeys,
        DestructiveTool::SendHandoff,
        DestructiveTool::BroadcastChat,
    ] {
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
// [limits] handoff_ready_timeout_secs: read through the same parse_policy
// path resolve_playbook feeds into, independent of the control_gate.rs
// coverage of the same key (a second, playbook-shaped view of the contract).
// =============================================================================

#[test]
fn handoff_ready_timeout_secs_is_read_from_a_parsed_playbook() {
    let policy = parse_policy("[limits]\nhandoff_ready_timeout_secs = 90\n")
        .expect("a [limits] handoff_ready_timeout_secs within range must parse");
    assert_eq!(
        policy
            .limits()
            .expect("[limits] is present")
            .handoff_ready_timeout_secs,
        Some(90)
    );
}

#[test]
fn handoff_ready_timeout_secs_out_of_range_fails_the_whole_playbook_load_naming_the_key() {
    for seconds in [0u64, 601] {
        let source = format!("[limits]\nhandoff_ready_timeout_secs = {seconds}\n");
        let result = parse_policy(&source);
        // `err.to_string()` also carries a TOML source-line snippet that
        // echoes the raw key from the input, so matching on the key name
        // alone is a vacuous check (it passes even if the custom message
        // itself dropped the key). Match the custom message text that
        // `check_timeout_range` (control.rs) builds instead, so this test
        // fails if that message stops naming its key.
        let expected =
            format!("handoff_ready_timeout_secs must be between 1 and 600, got {seconds}");
        match result {
            Err(err) => {
                let message = err.to_string();
                assert!(
                    message.contains(&expected),
                    "{seconds} is outside 1..=600 and must fail the whole playbook load \
                     with a message naming the offending key, expected to find {expected:?} \
                     in {message:?}"
                );
            }
            Ok(_) => panic!("{seconds} is outside 1..=600 and must fail the whole load"),
        }
    }
}

#[test]
fn handoff_ready_timeout_secs_defaults_to_none_when_the_playbook_omits_it() {
    let policy = parse_policy("[limits]\ninterrupt_bytes = [3]\n")
        .expect("a [limits] section without handoff_ready_timeout_secs must parse");
    assert_eq!(
        policy
            .limits()
            .expect("[limits] is present")
            .handoff_ready_timeout_secs,
        None,
        "an omitted handoff_ready_timeout_secs must stay None so the daemon's built-in \
         default applies"
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
