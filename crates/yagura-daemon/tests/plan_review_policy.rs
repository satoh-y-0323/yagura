#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The playbook's `[plans]` table: whether a new plan waits for the operator
//! to approve it before any of its task windows can open.
//!
//! Every way the setting can be absent (the key, the table, the playbook
//! itself) must leave approval required, so a daemon never silently skips
//! the operator's review because an older playbook predates the table.

use std::path::PathBuf;

use yagura_daemon::DaemonConfig;
use yagura_daemon::control::{PlansSection, apply_plans_section, parse_policy};

fn plans_section_of(source: &str) -> Option<PlansSection> {
    parse_policy(source)
        .unwrap_or_else(|err| panic!("the playbook {source:?} must load: {err}"))
        .plans
}

fn required_after_applying(start: bool, source: &str) -> bool {
    let section = plans_section_of(source);
    let mut config = DaemonConfig {
        plan_approval_required: start,
        ..DaemonConfig::default()
    };
    apply_plans_section(&mut config, section.as_ref());
    config.plan_approval_required
}

#[test]
fn a_daemon_without_a_playbook_requires_plan_approval() {
    assert!(
        DaemonConfig::default().plan_approval_required,
        "the built-in default must require approval: a missing playbook is the safe side"
    );
}

#[test]
fn require_approval_false_turns_the_gate_off() {
    assert!(!required_after_applying(
        true,
        "[plans]\nrequire_approval = false\n"
    ));
}

#[test]
fn require_approval_true_turns_the_gate_on() {
    assert!(required_after_applying(
        false,
        "[plans]\nrequire_approval = true\n"
    ));
}

#[test]
fn an_omitted_require_approval_key_keeps_the_gate_on() {
    assert!(required_after_applying(
        DaemonConfig::default().plan_approval_required,
        "[plans]\n"
    ));
}

#[test]
fn an_omitted_plans_table_keeps_the_gate_on() {
    assert!(required_after_applying(
        DaemonConfig::default().plan_approval_required,
        "[approval]\nauto_approve = []\n"
    ));
}

#[test]
fn an_unknown_key_in_the_plans_table_fails_the_whole_playbook() {
    assert!(
        parse_policy("[plans]\nrequire_approval = false\n").is_ok(),
        "the table itself must be known before its unknown key can be the reason for a failure"
    );
    assert!(
        parse_policy("[plans]\nrequire_approval = false\nrequire_aproval = true\n").is_err(),
        "a mistyped key must fail the load rather than be ignored"
    );
}

#[test]
fn the_bundled_repository_playbook_requires_plan_approval() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("playbook.toml");
    let source = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("{} must be readable: {err}", path.display()));
    let section = plans_section_of(&source);
    assert_eq!(
        section.and_then(|plans| plans.require_approval),
        Some(true),
        "the bundled playbook must state require_approval = true in an uncommented [plans] table"
    );
}
