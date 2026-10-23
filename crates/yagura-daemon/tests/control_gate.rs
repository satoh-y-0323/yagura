#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The destructive-operation approval gate, the playbook policy loader, the `[limits]`
//! merge, the daemon-wide rate bucket and the line-oriented `read_output` helper.
//!
//! Enum variants are matched with the braced `Variant { .. }` pattern throughout, which
//! is valid for unit, tuple and struct variants alike, so the assertions never pin down
//! whether a variant carries a payload.
//!
//! The `WindowEntry`-operating half of `control.rs` needs a real PTY and is covered by
//! `tests/ipc_control.rs`.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use tracing_test::traced_test;
use yagura_daemon::config::{APPROVAL_TIMEOUT_DEFAULT_SECS, DaemonConfig};
use yagura_daemon::control::{
    ApprovalPolicy, ApprovalSection, ChatSection, ComposerAccess, ComposerWrite, DenyReason,
    DestructiveTool, GateVerdict, PolicyError, PolicyLimits, PolicySource, RateBucket,
    RequestOrigin, apply_chat_section, apply_limits, check_destructive, composer_gate, load_policy,
    parse_policy, policy_gate, status_gate,
};
use yagura_daemon::room::ChatLimits;
use yagura_protocol::{DetectedStatus, UnknownReason};
use yagura_test_support::paths::TempDir;

/// Builds a policy that auto-approves exactly `tools`, without going through TOML.
fn policy_approving(tools: &[&str]) -> ApprovalPolicy {
    ApprovalPolicy {
        approval: Some(ApprovalSection {
            auto_approve: tools.iter().map(|tool| (*tool).to_string()).collect(),
        }),
        limits: None,
        chat: None,
        plans: None,
    }
}

/// A playbook file in a uniquely named temporary directory, removed with the
/// directory on drop.
struct TempFile {
    _dir: TempDir,
    path: PathBuf,
}

impl TempFile {
    fn new(tag: &str, contents: &str) -> Self {
        let dir = TempDir::new("yagura-playbook", tag);
        let path = dir.write_file("playbook.toml", contents);
        Self { _dir: dir, path }
    }
}

/// The window every `check_destructive` case in this file is aimed at, so that
/// `RequestOrigin::Window(TARGET)` reads as "the window asking about itself"
/// and any other window id reads as "somebody else's window".
const TARGET: &str = "w1-abc";

/// The probe a window whose definition names one brings to the gate.
const PROBE: char = 'x';

/// A window that takes a judged write: its definition declares a composer and
/// a probe.
const JUDGED: ComposerAccess = ComposerAccess::Judged { probe: PROBE };

/// Every value a window's composer access can take.
const ALL_ACCESS: [ComposerAccess; 3] = [ComposerAccess::Absent, ComposerAccess::RawOnly, JUDGED];

/// Every shape a write can take at the gate.
const ALL_WRITES: [ComposerWrite; 2] = [ComposerWrite::Judged, ComposerWrite::Raw];

/// The origin an unidentified caller (an MCP or CLI client) has.
fn external() -> RequestOrigin {
    RequestOrigin::External
}

/// The origin of the target window itself.
fn own_window() -> RequestOrigin {
    RequestOrigin::Window(TARGET.to_string())
}

/// The origin of a different window than the target.
fn other_window() -> RequestOrigin {
    RequestOrigin::Window("w2-abc".to_string())
}

#[test]
fn check_destructive_needs_approval_for_interrupt_without_a_policy() {
    // Fail-closed default: with no playbook at all nothing is auto-approved, so
    // an outside caller's interrupt_window has to go past a human.
    let result = check_destructive(
        PolicySource::NotConfigured,
        DestructiveTool::InterruptWindow,
        DetectedStatus::Idle,
        JUDGED,
        ComposerWrite::Judged,
        &external(),
        TARGET,
    );
    assert!(
        matches!(result, Ok(GateVerdict::NeedsApproval)),
        "interrupt_window must not be auto-approved when no playbook is \
         configured (fail-closed), got {result:?}"
    );
}

#[test]
fn check_destructive_needs_approval_for_close_without_a_policy() {
    let result = check_destructive(
        PolicySource::NotConfigured,
        DestructiveTool::CloseWindow,
        DetectedStatus::Idle,
        JUDGED,
        ComposerWrite::Judged,
        &external(),
        TARGET,
    );
    assert!(
        matches!(result, Ok(GateVerdict::NeedsApproval)),
        "close_window must not be auto-approved when no playbook is configured \
         (fail-closed), got {result:?}"
    );
}

#[test]
fn check_destructive_needs_approval_for_send_message_without_a_policy() {
    let result = check_destructive(
        PolicySource::NotConfigured,
        DestructiveTool::SendMessage,
        DetectedStatus::Idle,
        JUDGED,
        ComposerWrite::Judged,
        &external(),
        TARGET,
    );
    assert!(
        matches!(result, Ok(GateVerdict::NeedsApproval)),
        "send_message joins the gate on the same fail-closed terms as the other \
         two tools, got {result:?}"
    );
}

#[test]
fn check_destructive_approves_only_the_tool_listed_in_auto_approve() {
    let policy = policy_approving(&["interrupt_window"]);

    let interrupt = check_destructive(
        PolicySource::Loaded(&policy),
        DestructiveTool::InterruptWindow,
        DetectedStatus::Idle,
        JUDGED,
        ComposerWrite::Judged,
        &external(),
        TARGET,
    );
    assert!(
        matches!(interrupt, Ok(GateVerdict::Allow)),
        "interrupt_window is listed in auto_approve and must be allowed through, \
         got {interrupt:?}"
    );

    let close = check_destructive(
        PolicySource::Loaded(&policy),
        DestructiveTool::CloseWindow,
        DetectedStatus::Idle,
        JUDGED,
        ComposerWrite::Judged,
        &external(),
        TARGET,
    );
    assert!(
        matches!(close, Ok(GateVerdict::NeedsApproval)),
        "close_window is not listed in auto_approve and must still need a human, \
         got {close:?}"
    );
}

#[test]
fn allows_matches_the_tool_names_listed_in_auto_approve() {
    // The string <-> variant mapping is an external contract: "interrupt_window"
    // <-> InterruptWindow, "close_window" <-> CloseWindow, "send_message" <->
    // SendMessage, "send_handoff" <-> SendHandoff.
    let policy = policy_approving(&["interrupt_window"]);
    assert!(policy.allows(DestructiveTool::InterruptWindow));
    assert!(!policy.allows(DestructiveTool::CloseWindow));
    assert!(!policy.allows(DestructiveTool::SendMessage));
    assert!(!policy.allows(DestructiveTool::SendHandoff));

    let all = policy_approving(&[
        "interrupt_window",
        "close_window",
        "send_message",
        "send_handoff",
    ]);
    assert!(all.allows(DestructiveTool::InterruptWindow));
    assert!(all.allows(DestructiveTool::CloseWindow));
    assert!(all.allows(DestructiveTool::SendMessage));
    assert!(all.allows(DestructiveTool::SendHandoff));
}

#[test]
fn check_destructive_denies_interrupt_on_unknown_status_even_when_approved() {
    // The status gate outranks the playbook and cannot be relaxed by it:
    // interrupt is an input injection and must never fire on a window whose
    // state could not be detected.
    let policy = policy_approving(&[
        "interrupt_window",
        "close_window",
        "send_message",
        "send_handoff",
    ]);
    let result = check_destructive(
        PolicySource::Loaded(&policy),
        DestructiveTool::InterruptWindow,
        DetectedStatus::Unknown(UnknownReason::NoRuleMatched),
        JUDGED,
        ComposerWrite::Judged,
        &external(),
        TARGET,
    );
    assert!(
        matches!(result, Err(DenyReason::StatusUnknown(_))),
        "interrupt_window on an Unknown window must be denied with StatusUnknown \
         even when auto_approve lists it (the status gate precedes the playbook gate), got {result:?}"
    );
}

#[test]
fn check_destructive_denies_interrupt_on_unknown_status_before_consulting_the_policy() {
    // The status gate precedes the playbook gate, so a missing policy must
    // still surface as StatusUnknown, not as NeedsApproval.
    let result = check_destructive(
        PolicySource::NotConfigured,
        DestructiveTool::InterruptWindow,
        DetectedStatus::Unknown(UnknownReason::NoRuleMatched),
        JUDGED,
        ComposerWrite::Judged,
        &external(),
        TARGET,
    );
    assert!(
        matches!(result, Err(DenyReason::StatusUnknown(_))),
        "the Unknown status gate is evaluated before the playbook gate, got \
         {result:?}"
    );
}

#[test]
fn check_destructive_lets_close_through_on_unknown_status() {
    // close_window is deliberately outside the status gate: a hung or
    // undetectable window must stay closable, or the daemon would leave an
    // orphan process behind.
    let policy = policy_approving(&["close_window"]);
    let result = check_destructive(
        PolicySource::Loaded(&policy),
        DestructiveTool::CloseWindow,
        DetectedStatus::Unknown(UnknownReason::NoRuleMatched),
        JUDGED,
        ComposerWrite::Judged,
        &external(),
        TARGET,
    );
    assert!(
        matches!(result, Ok(GateVerdict::Allow)),
        "close_window must remain permitted on an Unknown window once the \
         playbook approves it, got {result:?}"
    );
}

#[test]
fn parse_policy_rejects_malformed_toml() {
    let source = "[approval\nauto_approve = [\"interrupt_window\"]\n";
    let result = parse_policy(source);
    assert!(
        matches!(result, Err(PolicyError::Parse { .. })),
        "a syntactically invalid playbook must fail to parse (fail-closed: a broken \
         playbook must never silently approve anything), got {result:?}"
    );
}

#[test]
fn parse_policy_rejects_an_unknown_field() {
    // deny_unknown_fields at every level: a typo'd key must be a loud load failure, never
    // a silently ignored setting.
    let source = "[approval]\nauto_aprove = [\"interrupt_window\"]\n";
    let result = parse_policy(source);
    assert!(
        matches!(result, Err(PolicyError::Parse { .. })),
        "a misspelled key (auto_aprove) must be rejected by deny_unknown_fields, got \
         {result:?}"
    );
}

#[test]
fn parse_policy_rejects_an_unknown_section() {
    let source = "[aproval]\nauto_approve = [\"interrupt_window\"]\n";
    let result = parse_policy(source);
    assert!(
        matches!(result, Err(PolicyError::Parse { .. })),
        "a misspelled section name must be rejected by deny_unknown_fields, got {result:?}"
    );
}

#[test]
fn parse_policy_accepts_an_empty_playbook_and_approves_nothing() {
    let policy = parse_policy("").expect("an empty playbook is valid TOML and must load");
    assert!(
        !policy.allows(DestructiveTool::InterruptWindow),
        "an empty playbook must approve nothing (equivalent to an empty auto_approve)"
    );
    assert!(!policy.allows(DestructiveTool::CloseWindow));
}

#[test]
fn parse_policy_accepts_a_playbook_without_an_approval_section_and_approves_nothing() {
    let source = "[limits]\nwait_timeout_max_secs = 600\n";
    let policy = parse_policy(source).expect("a playbook without [approval] must still load");
    assert!(
        !policy.allows(DestructiveTool::InterruptWindow),
        "a missing [approval] section must approve nothing"
    );
    assert!(!policy.allows(DestructiveTool::CloseWindow));
}

#[test]
fn parse_policy_ignores_an_unknown_tool_name_inside_auto_approve() {
    // Value-level forward compatibility: an unrecognised tool name is warned about and
    // ignored, and must not disturb the known names listed next to it.
    let source = "[approval]\nauto_approve = [\"teleport_window\", \"interrupt_window\"]\n";
    let policy =
        parse_policy(source).expect("an unknown tool name must not fail the whole playbook");
    assert!(
        policy.allows(DestructiveTool::InterruptWindow),
        "a known tool name listed alongside an unknown one must still be approved"
    );
    assert!(
        !policy.allows(DestructiveTool::CloseWindow),
        "an unknown tool name must not be read as a wildcard (auto_approve has no wildcard \
         syntax)"
    );
}

#[test]
fn load_policy_reports_an_io_error_for_a_missing_file() {
    let dir = TempDir::new("yagura-playbook", "missing");
    let missing = dir.path().join("missing.toml");
    let result = load_policy(&missing);
    assert!(
        matches!(result, Err(PolicyError::Io { .. })),
        "a non-existent playbook path must surface as PolicyError::Io (distinct from a \
         parse failure, so the daemon can log which one happened), got {result:?}"
    );
}

#[test]
fn load_policy_reads_a_playbook_from_disk() {
    let file = TempFile::new("ok", "[approval]\nauto_approve = [\"close_window\"]\n");
    let policy = load_policy(&file.path).expect("an existing, valid playbook must load");
    assert!(policy.allows(DestructiveTool::CloseWindow));
    assert!(!policy.allows(DestructiveTool::InterruptWindow));
}

#[test]
fn limits_section_exposes_interrupt_bytes_and_wait_timeout() {
    let source = "[limits]\ninterrupt_bytes = [27]\nwait_timeout_max_secs = 600\n";
    let policy = parse_policy(source).expect("a [limits] section must parse");
    let limits = policy
        .limits()
        .expect("limits() must expose the parsed [limits] table");
    assert_eq!(limits.interrupt_bytes, Some(vec![27u8]));
    assert_eq!(limits.wait_timeout_max_secs, Some(600));
}

#[test]
fn limits_are_none_when_the_section_is_omitted() {
    let policy = parse_policy("[approval]\nauto_approve = []\n")
        .expect("a playbook without [limits] must parse");
    assert!(
        policy.limits().is_none(),
        "an omitted [limits] section must read as None so apply_limits keeps the built-in \
         defaults"
    );
}

#[test]
fn limits_omitted_fields_are_none() {
    let policy = parse_policy("[limits]\ninterrupt_bytes = [3]\n")
        .expect("a partially filled [limits] section must parse");
    let limits = policy.limits().expect("[limits] is present");
    assert_eq!(limits.interrupt_bytes, Some(vec![3u8]));
    assert_eq!(
        limits.wait_timeout_max_secs, None,
        "a field omitted from [limits] must stay None so only the specified side is \
         overridden"
    );
}

#[test]
fn limits_reject_a_value_of_the_wrong_type() {
    let source = "[limits]\nwait_timeout_max_secs = \"600\"\n";
    let result = parse_policy(source);
    assert!(
        matches!(result, Err(PolicyError::Parse { .. })),
        "a [limits] value of the wrong TOML type must fail the load rather than be coerced \
         (fail-closed), got {result:?}"
    );
}

#[test]
fn apply_limits_overrides_every_specified_field() {
    let mut config = DaemonConfig::default();
    let limits = PolicyLimits {
        interrupt_bytes: Some(vec![0x1B]),
        wait_timeout_max_secs: Some(120),
        approval_timeout_secs: Some(90),
        handoff_ready_timeout_secs: Some(120),
        handoff_delivery_timeout_secs: None,
        chat_delivery_max: Some(64),
    };

    apply_limits(&mut config, Some(&limits));

    assert_eq!(
        config.interrupt_bytes,
        vec![0x1Bu8],
        "[limits] interrupt_bytes must replace the built-in default (this is the escape \
         hatch when 0x03 does not interrupt a given CLI)"
    );
    assert_eq!(config.wait_timeout_max_secs, 120);
    assert_eq!(
        config.approval_timeout_secs, 90,
        "[limits] approval_timeout_secs must replace the built-in default"
    );
    assert_eq!(
        config.chat_delivery_max, 64,
        "[limits] chat_delivery_max must replace the built-in default"
    );
}

#[test]
fn apply_limits_keeps_the_defaults_when_no_limits_are_given() {
    let defaults = DaemonConfig::default();
    let mut config = DaemonConfig::default();

    apply_limits(&mut config, None);

    assert_eq!(config.interrupt_bytes, defaults.interrupt_bytes);
    assert_eq!(config.wait_timeout_max_secs, defaults.wait_timeout_max_secs);
    assert_eq!(config.approval_timeout_secs, defaults.approval_timeout_secs);
}

#[test]
fn apply_limits_overrides_only_the_specified_field() {
    let defaults = DaemonConfig::default();
    let mut config = DaemonConfig::default();
    let limits = PolicyLimits {
        interrupt_bytes: None,
        wait_timeout_max_secs: Some(42),
        approval_timeout_secs: None,
        handoff_ready_timeout_secs: None,
        handoff_delivery_timeout_secs: None,
        chat_delivery_max: None,
    };

    apply_limits(&mut config, Some(&limits));

    assert_eq!(
        config.interrupt_bytes, defaults.interrupt_bytes,
        "a field omitted from [limits] must leave the corresponding default untouched"
    );
    assert_eq!(config.wait_timeout_max_secs, 42);
    assert_eq!(
        config.approval_timeout_secs, defaults.approval_timeout_secs,
        "a field omitted from [limits] must leave the corresponding default \
         untouched"
    );
}

#[test]
fn daemon_config_defaults_to_the_ctrl_c_interrupt_byte() {
    // The effectiveness of 0x03 itself is a manual check; that the default is 0x03 is
    // pinned here.
    assert_eq!(DaemonConfig::default().interrupt_bytes, vec![0x03u8]);
}

// `Instant` is injected rather than read from the clock, so none of these tests sleep.
// `tokio::time::pause()` is unusable here: the bucket takes a `std::time::Instant` and
// the virtual clock only affects `tokio::time::Instant`.

#[test]
fn rate_bucket_admits_exactly_the_burst_before_any_refill() {
    let now = Instant::now();
    let mut bucket = RateBucket::new(3, 1);

    for attempt in 1..=3 {
        assert!(
            bucket.try_acquire(now),
            "attempt {attempt} must be admitted: the bucket starts full at burst = 3"
        );
    }
    assert!(
        !bucket.try_acquire(now),
        "the fourth attempt at the same instant exceeds burst = 3 and must be rejected"
    );
}

#[test]
fn rate_bucket_recovers_after_the_refill_interval() {
    let now = Instant::now();
    let mut bucket = RateBucket::new(1, 1);

    assert!(bucket.try_acquire(now));
    assert!(
        !bucket.try_acquire(now),
        "the bucket is empty at the same instant"
    );
    assert!(
        bucket.try_acquire(now + Duration::from_secs(1)),
        "at per_sec = 1 exactly one token must be back one second later (rejection is a \
         throttle, not a permanent penalty)"
    );
}

#[test]
fn rate_bucket_with_zero_burst_never_admits() {
    let now = Instant::now();
    let mut bucket = RateBucket::new(0, 5);

    assert!(!bucket.try_acquire(now));
    assert!(
        !bucket.try_acquire(now + Duration::from_secs(10)),
        "a zero-capacity bucket can never hold a token, no matter how much time passes"
    );
}

#[test]
fn rate_bucket_regains_tokens_in_proportion_to_its_refill_rate() {
    let now = Instant::now();
    let mut bucket = RateBucket::new(10, 4);

    for attempt in 1..=10 {
        assert!(
            bucket.try_acquire(now),
            "attempt {attempt} drains the burst"
        );
    }
    // Half a second at 4 tokens per second is exactly two tokens.
    let later = now + Duration::from_millis(500);
    assert!(bucket.try_acquire(later), "the first regained token");
    assert!(bucket.try_acquire(later), "the second regained token");
    assert!(
        !bucket.try_acquire(later),
        "only two tokens are regained in half a second at 4 per second"
    );
}

#[test]
#[traced_test]
fn parse_policy_warns_about_an_unknown_auto_approve_name_and_keeps_it() {
    let policy = parse_policy("[approval]\nauto_approve = [\"send_keys\", \"bogus_tool\"]\n")
        .expect("an unknown name is warned about, not an error");

    assert!(logs_contain("bogus_tool"));
    assert!(
        !logs_contain("\"send_keys\""),
        "a known tool name must not be warned about"
    );
    assert_eq!(
        policy.approval.expect("approval section").auto_approve,
        vec!["send_keys".to_string(), "bogus_tool".to_string()]
    );
}

// =============================================================================
// The status gate on its own
// =============================================================================

#[test]
fn status_gate_denies_input_injection_on_an_unknown_window() {
    for tool in [
        DestructiveTool::InterruptWindow,
        DestructiveTool::SendMessage,
        DestructiveTool::SendKeys,
        DestructiveTool::SendHandoff,
    ] {
        let result = status_gate(tool, DetectedStatus::Unknown(UnknownReason::NoRuleMatched));
        assert!(
            matches!(result, Err(DenyReason::StatusUnknown(_))),
            "{} writes into the window's input, so it must never fire on a \
             window whose state could not be detected, got {result:?}",
            tool.as_tool_name()
        );
    }
}

#[test]
fn status_gate_lets_close_through_on_an_unknown_window() {
    let result = status_gate(
        DestructiveTool::CloseWindow,
        DetectedStatus::Unknown(UnknownReason::NoRuleMatched),
    );
    assert!(
        result.is_ok(),
        "close_window stays available on an undetectable window, or a hung \
         window could never be cleaned up, got {result:?}"
    );
}

#[test]
fn status_gate_lets_every_tool_through_on_a_detected_status() {
    for tool in [
        DestructiveTool::InterruptWindow,
        DestructiveTool::CloseWindow,
        DestructiveTool::SendMessage,
        DestructiveTool::SendKeys,
        DestructiveTool::SendHandoff,
    ] {
        for status in [
            DetectedStatus::Running,
            DetectedStatus::WaitingInput,
            DetectedStatus::WaitingApproval,
            DetectedStatus::Interrupted,
            DetectedStatus::Idle,
        ] {
            let result = status_gate(tool, status);
            assert!(
                result.is_ok(),
                "the status gate only guards Unknown: {} on {status:?} must pass \
                 it, got {result:?}",
                tool.as_tool_name()
            );
        }
    }
}

#[test]
fn status_gate_lets_every_tool_through_on_an_exited_window() {
    // Exited is not the status gate's business: `send_message` and
    // `interrupt_window` refuse it in the handler's pre-validation, ahead of the
    // gate, so that the caller is told `child_exited` rather than being sent
    // through an approval that could not succeed.
    for tool in [
        DestructiveTool::InterruptWindow,
        DestructiveTool::CloseWindow,
        DestructiveTool::SendMessage,
        DestructiveTool::SendKeys,
        DestructiveTool::SendHandoff,
    ] {
        let result = status_gate(tool, DetectedStatus::Exited);
        assert!(
            result.is_ok(),
            "the status gate must not be the thing that refuses an exited \
             window for {}, got {result:?}",
            tool.as_tool_name()
        );
    }
}

// =============================================================================
// The whole judgement order, over every combination
// =============================================================================

/// The four shapes a policy can reach the gate in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SourceKind {
    /// No playbook was configured at all.
    NotConfigured,
    /// A playbook that lists every tool in `auto_approve`.
    ApprovesEverything,
    /// A playbook that lists none of them.
    ApprovesNothing,
    /// A playbook was configured but could not be loaded.
    Failed,
}

/// The input gate's judgement order, transcribed as an oracle: status gate ->
/// composer gate -> own window -> approver -> playbook -> needs approval. The
/// exhaustive test below compares the implementation against this, so a
/// reordering shows up as a specific failing combination rather than as one
/// hand-picked case.
fn expected_verdict(
    kind: SourceKind,
    tool: DestructiveTool,
    status: DetectedStatus,
    access: ComposerAccess,
    write: ComposerWrite,
    origin: &RequestOrigin,
) -> Result<GateVerdict, DenyReason> {
    // 1. status gate
    if let Some(reason) = status.unknown_reason()
        && matches!(
            tool,
            DestructiveTool::InterruptWindow
                | DestructiveTool::SendMessage
                | DestructiveTool::SendKeys
                | DestructiveTool::SendHandoff
                | DestructiveTool::SendChat
                | DestructiveTool::BroadcastChat
        )
    {
        return Err(DenyReason::StatusUnknown(reason));
    }
    // 2. composer gate: a judged write needs a probe, an unjudged one only a
    //    composer, and a handoff is always judged.
    let judged = matches!(access, ComposerAccess::Judged { .. });
    let refused = match tool {
        DestructiveTool::SendHandoff => !judged,
        DestructiveTool::SendMessage => match write {
            ComposerWrite::Judged => !judged,
            ComposerWrite::Raw => matches!(access, ComposerAccess::Absent),
        },
        DestructiveTool::OpenWindow
        | DestructiveTool::InterruptWindow
        | DestructiveTool::CloseWindow
        | DestructiveTool::SendKeys
        | DestructiveTool::SendChat
        | DestructiveTool::BroadcastChat => false,
    };
    if refused {
        return Err(DenyReason::NoComposer);
    }
    // 3. the target window acting on itself
    if let RequestOrigin::Window(window_id) = origin
        && window_id == TARGET
    {
        return Ok(GateVerdict::Allow);
    }
    // 4. a registered approver
    if matches!(origin, RequestOrigin::Approver) {
        return Ok(GateVerdict::Allow);
    }
    // 5. the playbook, whose reach is exactly what is left: another window and
    //    an unidentified caller.
    match kind {
        SourceKind::ApprovesEverything => Ok(GateVerdict::Allow),
        SourceKind::Failed => Err(DenyReason::PlaybookBroken),
        SourceKind::NotConfigured | SourceKind::ApprovesNothing => Ok(GateVerdict::NeedsApproval),
    }
}

#[test]
fn check_destructive_follows_the_judgement_order_for_every_combination() {
    let everything = policy_approving(&[
        "open_window",
        "interrupt_window",
        "close_window",
        "send_message",
        "send_keys",
        "send_handoff",
        "send_chat",
        "broadcast_chat",
    ]);
    let nothing = policy_approving(&[]);

    let mut cases = 0usize;
    for kind in [
        SourceKind::NotConfigured,
        SourceKind::ApprovesEverything,
        SourceKind::ApprovesNothing,
        SourceKind::Failed,
    ] {
        for tool in [
            DestructiveTool::OpenWindow,
            DestructiveTool::InterruptWindow,
            DestructiveTool::CloseWindow,
            DestructiveTool::SendMessage,
            DestructiveTool::SendKeys,
            DestructiveTool::SendHandoff,
            DestructiveTool::SendChat,
            DestructiveTool::BroadcastChat,
        ] {
            for status in [
                DetectedStatus::Unknown(UnknownReason::NoRuleMatched),
                DetectedStatus::Idle,
            ] {
                for access in ALL_ACCESS {
                    for write in ALL_WRITES {
                        for origin in [
                            external(),
                            own_window(),
                            other_window(),
                            RequestOrigin::Approver,
                        ] {
                            let source = match kind {
                                SourceKind::NotConfigured => PolicySource::NotConfigured,
                                SourceKind::ApprovesEverything => PolicySource::Loaded(&everything),
                                SourceKind::ApprovesNothing => PolicySource::Loaded(&nothing),
                                SourceKind::Failed => PolicySource::Failed,
                            };
                            let actual = check_destructive(
                                source, tool, status, access, write, &origin, TARGET,
                            );
                            let expected =
                                expected_verdict(kind, tool, status, access, write, &origin);
                            assert_eq!(
                                actual,
                                expected,
                                "gate disagreement for policy {kind:?}, tool {}, status \
                                 {status:?}, composer access {access:?}, write {write:?}, \
                                 origin {origin:?}, target {TARGET}",
                                tool.as_tool_name()
                            );
                            cases += 1;
                        }
                    }
                }
            }
        }
    }
    assert_eq!(
        cases, 1536,
        "the combination sweep must cover 4 policy shapes x 8 tools x 2 statuses \
         x 3 composer accesses x 2 write shapes x 4 origins"
    );
}

// =============================================================================
// The individual clauses of the judgement order
// =============================================================================

#[test]
fn input_injection_on_an_unknown_window_is_denied_whoever_asks() {
    // The Unknown gate is not relaxed for the window itself nor for
    // an approver, and the refusal is disclosed to all of them alike.
    let policy = policy_approving(&[
        "interrupt_window",
        "close_window",
        "send_message",
        "send_handoff",
    ]);
    for tool in [
        DestructiveTool::InterruptWindow,
        DestructiveTool::SendMessage,
        DestructiveTool::SendHandoff,
    ] {
        for origin in [
            external(),
            own_window(),
            other_window(),
            RequestOrigin::Approver,
        ] {
            let result = check_destructive(
                PolicySource::Loaded(&policy),
                tool,
                DetectedStatus::Unknown(UnknownReason::NoRuleMatched),
                JUDGED,
                ComposerWrite::Judged,
                &origin,
                TARGET,
            );
            assert!(
                matches!(result, Err(DenyReason::StatusUnknown(_))),
                "{} on an Unknown window must be refused for origin {origin:?}, \
                 got {result:?}",
                tool.as_tool_name()
            );
        }
    }
}

#[test]
fn a_window_acting_on_itself_is_allowed_without_a_playbook() {
    // A window driving its own session is the caller acting on itself, so
    // it does not queue behind a human.
    for tool in [
        DestructiveTool::InterruptWindow,
        DestructiveTool::CloseWindow,
        DestructiveTool::SendMessage,
    ] {
        let result = check_destructive(
            PolicySource::NotConfigured,
            tool,
            DetectedStatus::Idle,
            JUDGED,
            ComposerWrite::Judged,
            &own_window(),
            TARGET,
        );
        assert!(
            matches!(result, Ok(GateVerdict::Allow)),
            "{} from the target window itself must be allowed, got {result:?}",
            tool.as_tool_name()
        );
    }
}

#[test]
fn a_window_acting_on_itself_is_allowed_even_with_a_broken_playbook() {
    // The fail-closed reach of a broken playbook starts after the target
    // window acting on itself: refusing a window's own close would leave it
    // unable to clean itself up because of a typo in somebody else's policy
    // file.
    let result = check_destructive(
        PolicySource::Failed,
        DestructiveTool::CloseWindow,
        DetectedStatus::Idle,
        JUDGED,
        ComposerWrite::Judged,
        &own_window(),
        TARGET,
    );
    assert!(
        matches!(result, Ok(GateVerdict::Allow)),
        "a broken playbook must not reach the target-window-acting-on-itself case, got {result:?}"
    );
}

#[test]
fn a_window_acting_on_another_window_gets_no_r1_relief() {
    let result = check_destructive(
        PolicySource::NotConfigured,
        DestructiveTool::CloseWindow,
        DetectedStatus::Idle,
        JUDGED,
        ComposerWrite::Judged,
        &other_window(),
        TARGET,
    );
    assert!(
        matches!(result, Ok(GateVerdict::NeedsApproval)),
        "the target window acting on itself is about the target window only: another window's request must \
         still need approval, got {result:?}"
    );
}

#[test]
fn an_approver_is_allowed_whatever_the_policy_says() {
    // The connection that answers the dialogs is not
    // gated by them, and that stays true when the playbook is missing, empty or
    // broken.
    for source_label in ["not configured", "approves nothing", "failed"] {
        let nothing = policy_approving(&[]);
        let source = match source_label {
            "not configured" => PolicySource::NotConfigured,
            "approves nothing" => PolicySource::Loaded(&nothing),
            _ => PolicySource::Failed,
        };
        let result = check_destructive(
            source,
            DestructiveTool::CloseWindow,
            DetectedStatus::Idle,
            JUDGED,
            ComposerWrite::Judged,
            &RequestOrigin::Approver,
            TARGET,
        );
        assert!(
            matches!(result, Ok(GateVerdict::Allow)),
            "an approver's own request must be allowed with the policy \
             {source_label}, got {result:?}"
        );
    }
}

#[test]
fn a_broken_playbook_denies_an_outside_caller_rather_than_asking_a_human() {
    // A configured playbook that failed to load is a broken
    // policy, not an absent one. The request is refused with its own reason so
    // the operator is told to fix the file instead of being asked to approve.
    for origin in [external(), other_window()] {
        for tool in [
            DestructiveTool::InterruptWindow,
            DestructiveTool::CloseWindow,
            DestructiveTool::SendMessage,
        ] {
            let result = check_destructive(
                PolicySource::Failed,
                tool,
                DetectedStatus::Idle,
                JUDGED,
                ComposerWrite::Judged,
                &origin,
                TARGET,
            );
            assert!(
                matches!(result, Err(DenyReason::PlaybookBroken)),
                "{} from origin {origin:?} must be refused with PlaybookBroken, \
                 distinctly from ApprovalRequired, got {result:?}",
                tool.as_tool_name()
            );
        }
    }
}

#[test]
fn a_playbook_that_omits_the_tool_needs_approval_rather_than_being_broken() {
    let policy = policy_approving(&["close_window"]);
    let result = check_destructive(
        PolicySource::Loaded(&policy),
        DestructiveTool::SendMessage,
        DetectedStatus::Idle,
        JUDGED,
        ComposerWrite::Judged,
        &external(),
        TARGET,
    );
    assert!(
        matches!(result, Ok(GateVerdict::NeedsApproval)),
        "a loaded playbook that simply does not list the tool must route to a \
         human, not to PlaybookBroken, got {result:?}"
    );
}

#[test]
fn a_playbook_can_auto_approve_send_message_for_an_outside_caller() {
    let policy = policy_approving(&["send_message"]);
    let result = check_destructive(
        PolicySource::Loaded(&policy),
        DestructiveTool::SendMessage,
        DetectedStatus::Idle,
        JUDGED,
        ComposerWrite::Judged,
        &external(),
        TARGET,
    );
    assert!(
        matches!(result, Ok(GateVerdict::Allow)),
        "send_message is relaxable through the playbook like the other two \
         tools, got {result:?}"
    );
}

// =============================================================================
// The composer gate: a window whose detection definition
// declares no `[composer]` table refuses the two tools that write daemon-
// composed text into a PTY, before any of the identity or playbook relief
// clauses run.
// =============================================================================

#[test]
fn composer_gate_denies_only_send_message_and_send_handoff_when_no_composer_is_declared() {
    for tool in DestructiveTool::ALL {
        for write in ALL_WRITES {
            let result = composer_gate(tool, ComposerAccess::Absent, write);
            if matches!(
                tool,
                DestructiveTool::SendMessage | DestructiveTool::SendHandoff
            ) {
                assert!(
                    matches!(result, Err(DenyReason::NoComposer)),
                    "{} ({write:?}) writes daemon-composed text and must be refused with \
                     NoComposer when no composer is declared, got {result:?}",
                    tool.as_tool_name()
                );
            } else {
                assert!(
                    result.is_ok(),
                    "{} does not write daemon-composed text and must pass the composer \
                     gate regardless of whether one is declared, got {result:?}",
                    tool.as_tool_name()
                );
            }
        }
    }
}

#[test]
fn composer_gate_allows_every_tool_when_a_composer_and_a_probe_are_declared() {
    for tool in DestructiveTool::ALL {
        for write in ALL_WRITES {
            let result = composer_gate(tool, JUDGED, write);
            assert!(
                result.is_ok(),
                "{} ({write:?}) must pass the composer gate once a composer and a probe \
                 are declared, got {result:?}",
                tool.as_tool_name()
            );
        }
    }
}

#[test]
fn composer_gate_lets_only_an_unjudged_send_message_into_a_composer_without_a_probe() {
    for tool in DestructiveTool::ALL {
        for write in ALL_WRITES {
            let result = composer_gate(tool, ComposerAccess::RawOnly, write);
            let judged_text = match tool {
                DestructiveTool::SendHandoff => true,
                DestructiveTool::SendMessage => matches!(write, ComposerWrite::Judged),
                DestructiveTool::OpenWindow
                | DestructiveTool::InterruptWindow
                | DestructiveTool::CloseWindow
                | DestructiveTool::SendKeys
                | DestructiveTool::SendChat
                | DestructiveTool::BroadcastChat => false,
            };
            if judged_text {
                assert!(
                    matches!(result, Err(DenyReason::NoComposer)),
                    "{} ({write:?}) needs a probe to judge whether its text showed, so a \
                     composer without one must refuse it with NoComposer, got {result:?}",
                    tool.as_tool_name()
                );
            } else {
                assert!(
                    result.is_ok(),
                    "{} ({write:?}) judges nothing on screen and must pass a composer \
                     without a probe, got {result:?}",
                    tool.as_tool_name()
                );
            }
        }
    }
}

#[test]
fn an_unknown_status_outranks_a_missing_composer() {
    // The status gate precedes the composer gate: the reason reported for an
    // Unknown window must stay StatusUnknown even when the window also
    // declares no composer, so a caller is never told the wrong one is the
    // blocker.
    for tool in [DestructiveTool::SendMessage, DestructiveTool::SendHandoff] {
        let result = check_destructive(
            PolicySource::NotConfigured,
            tool,
            DetectedStatus::Unknown(UnknownReason::NoRuleMatched),
            ComposerAccess::Absent,
            ComposerWrite::Judged,
            &external(),
            TARGET,
        );
        assert!(
            matches!(result, Err(DenyReason::StatusUnknown(_))),
            "{} on an Unknown window must report StatusUnknown even with no \
             composer declared, got {result:?}",
            tool.as_tool_name()
        );
    }
}

#[test]
fn a_missing_composer_outranks_acting_on_ones_own_window_an_approver_and_a_broken_playbook() {
    // The composer gate precedes the own-window and approver clauses and the
    // playbook: none of those relief clauses can put a daemon-composed body
    // into a window that declares no composer.
    for tool in [DestructiveTool::SendMessage, DestructiveTool::SendHandoff] {
        for (label, source, origin) in [
            ("its own window", PolicySource::NotConfigured, own_window()),
            (
                "a registered approver",
                PolicySource::NotConfigured,
                RequestOrigin::Approver,
            ),
            (
                "its own window with a broken playbook",
                PolicySource::Failed,
                own_window(),
            ),
            (
                "an approver with a broken playbook",
                PolicySource::Failed,
                RequestOrigin::Approver,
            ),
        ] {
            let result = check_destructive(
                source,
                tool,
                DetectedStatus::Idle,
                ComposerAccess::Absent,
                ComposerWrite::Judged,
                &origin,
                TARGET,
            );
            assert!(
                matches!(result, Err(DenyReason::NoComposer)),
                "{} via {label} must still be refused with NoComposer when no \
                 composer is declared, got {result:?}",
                tool.as_tool_name()
            );
        }
    }
}

// =============================================================================
// send_message as a gated tool
// =============================================================================

#[test]
fn every_destructive_tool_maps_onto_its_external_name() {
    assert_eq!(
        DestructiveTool::InterruptWindow.as_tool_name(),
        "interrupt_window"
    );
    assert_eq!(DestructiveTool::CloseWindow.as_tool_name(), "close_window");
    assert_eq!(
        DestructiveTool::SendMessage.as_tool_name(),
        "send_message",
        "the playbook spelling of the new tool is the MCP tool name, not the \
         IPC message name (`send_text`)"
    );
    assert_eq!(
        DestructiveTool::SendHandoff.as_tool_name(),
        "send_handoff",
        "the playbook spelling of the fourth tool is the MCP tool name, not the \
         IPC message name (`send_handoff` on the wire too, but this pins the \
         mapping rather than assuming it)"
    );
    assert_eq!(DestructiveTool::SendChat.as_tool_name(), "send_chat");
    assert_eq!(
        DestructiveTool::BroadcastChat.as_tool_name(),
        "broadcast_chat"
    );
}

// =============================================================================
// send_handoff as the fourth gated tool
// =============================================================================

#[test]
fn destructive_tool_all_has_exactly_eight_elements() {
    assert_eq!(
        DestructiveTool::ALL.len(),
        8,
        "ALL must list exactly the eight gated tools: open_window, interrupt_window, \
         close_window, send_message, send_keys, send_handoff, send_chat and broadcast_chat"
    );
    let names: Vec<_> = DestructiveTool::ALL
        .iter()
        .map(|tool| tool.as_tool_name())
        .collect();
    assert_eq!(
        names,
        vec![
            "open_window",
            "interrupt_window",
            "close_window",
            "send_message",
            "send_keys",
            "send_handoff",
            "send_chat",
            "broadcast_chat",
        ]
    );
}

#[test]
fn is_known_tool_name_is_true_for_every_name_in_all_and_false_for_unknown_names() {
    for tool in DestructiveTool::ALL {
        assert!(
            DestructiveTool::is_known_tool_name(tool.as_tool_name()),
            "{} is listed in ALL and must be recognised as a known tool name",
            tool.as_tool_name()
        );
    }
    for name in ["teleport_window", "send_text", "SEND_HANDOFF", ""] {
        assert!(
            !DestructiveTool::is_known_tool_name(name),
            "{name:?} must not be recognised as a tool name"
        );
    }
}

/// `policy_gate` is the policy half of [`check_destructive`], callable before
/// a target window exists (`send_handoff`'s pre-flight check ahead of opening
/// a profile). `check_destructive` against a window id that matches none of
/// the origins used in this sweep stands in for "no target window" here: the
/// acting-on-itself relief can never trigger, exactly as when `target` is
/// literally absent. The composer access is fixed at [`JUDGED`] throughout, so
/// `check_destructive`'s composer gate is a no-op and the comparison isolates
/// the status-and-policy halves `policy_gate` actually covers.
#[test]
fn policy_gate_matches_check_destructive_against_a_window_none_of_the_origins_can_be() {
    let everything = policy_approving(&[
        "open_window",
        "interrupt_window",
        "close_window",
        "send_message",
        "send_keys",
        "send_handoff",
        "send_chat",
        "broadcast_chat",
    ]);
    let nothing = policy_approving(&[]);
    const NO_SUCH_WINDOW: &str = "no-such-window-zzz";

    let mut cases = 0usize;
    for kind in [
        SourceKind::NotConfigured,
        SourceKind::ApprovesEverything,
        SourceKind::ApprovesNothing,
        SourceKind::Failed,
    ] {
        for origin in [
            external(),
            own_window(),
            other_window(),
            RequestOrigin::Approver,
        ] {
            for tool in [
                DestructiveTool::OpenWindow,
                DestructiveTool::InterruptWindow,
                DestructiveTool::CloseWindow,
                DestructiveTool::SendMessage,
                DestructiveTool::SendKeys,
                DestructiveTool::SendHandoff,
                DestructiveTool::SendChat,
                DestructiveTool::BroadcastChat,
            ] {
                let source = match kind {
                    SourceKind::NotConfigured => PolicySource::NotConfigured,
                    SourceKind::ApprovesEverything => PolicySource::Loaded(&everything),
                    SourceKind::ApprovesNothing => PolicySource::Loaded(&nothing),
                    SourceKind::Failed => PolicySource::Failed,
                };
                let pre_flight = policy_gate(source, tool, &origin, None);
                let against_a_stand_in_window = check_destructive(
                    source,
                    tool,
                    DetectedStatus::Idle,
                    JUDGED,
                    ComposerWrite::Judged,
                    &origin,
                    NO_SUCH_WINDOW,
                );
                assert_eq!(
                    pre_flight,
                    against_a_stand_in_window,
                    "policy_gate(.., None) must agree with check_destructive against a \
                     window the origin cannot be identifying as, for policy {kind:?}, \
                     tool {}, origin {origin:?}",
                    tool.as_tool_name()
                );
                cases += 1;
            }
        }
    }
    assert_eq!(cases, 128, "4 policy shapes x 4 origins x 8 tools");
}

// =============================================================================
// [limits] handoff_ready_timeout_secs
// =============================================================================

#[test]
fn handoff_ready_timeout_secs_accepts_the_whole_permitted_range() {
    for seconds in [1u64, 60, 599, 600] {
        let source = format!("[limits]\nhandoff_ready_timeout_secs = {seconds}\n");
        let policy = parse_policy(&source)
            .unwrap_or_else(|err| panic!("{seconds} is inside 1..=600 and must parse: {err}"));
        assert_eq!(
            policy
                .limits()
                .expect("[limits] is present")
                .handoff_ready_timeout_secs,
            Some(seconds)
        );
    }
}

#[test]
fn handoff_ready_timeout_secs_rejects_a_value_outside_the_permitted_range_naming_the_key() {
    for seconds in [0u64, 601] {
        let source = format!("[limits]\nhandoff_ready_timeout_secs = {seconds}\n");
        let result = parse_policy(&source);
        match result {
            Err(err) => {
                let message = err.to_string();
                assert!(
                    message.contains("handoff_ready_timeout_secs"),
                    "{seconds} is outside 1..=600 and the load failure message must name \
                     the offending key, got {message:?}"
                );
            }
            Ok(_) => panic!("{seconds} is outside 1..=600 and must fail the whole load"),
        }
    }
}

#[test]
fn handoff_ready_timeout_secs_is_none_when_omitted_from_the_limits_section() {
    let policy = parse_policy("[limits]\ninterrupt_bytes = [3]\n")
        .expect("a [limits] section without handoff_ready_timeout_secs must parse");
    assert_eq!(
        policy
            .limits()
            .expect("[limits] is present")
            .handoff_ready_timeout_secs,
        None,
        "an omitted handoff_ready_timeout_secs must stay None so apply_limits keeps \
         the built-in default"
    );
}

#[test]
fn apply_limits_overrides_handoff_ready_timeout_secs_when_specified() {
    let mut config = DaemonConfig::default();
    let limits = PolicyLimits {
        handoff_ready_timeout_secs: Some(90),
        ..PolicyLimits::default()
    };

    apply_limits(&mut config, Some(&limits));

    assert_eq!(config.handoff_ready_timeout_secs, 90);
}

#[test]
fn apply_limits_keeps_the_default_handoff_ready_timeout_when_no_limits_are_given() {
    let defaults = DaemonConfig::default();
    let mut config = DaemonConfig::default();

    apply_limits(&mut config, None);

    assert_eq!(
        config.handoff_ready_timeout_secs,
        defaults.handoff_ready_timeout_secs
    );
}

#[test]
fn daemon_config_defaults_to_the_documented_handoff_slot_limits() {
    let defaults = DaemonConfig::default();
    assert_eq!(
        defaults.max_open_handoffs, 32,
        "the daemon-wide cap on open-and-undelivered handoff windows must default to 32"
    );
    assert_eq!(
        defaults.max_open_handoffs_per_conn, 4,
        "the per-connection cap on open-and-undelivered handoff windows must default to 4"
    );
}

#[test]
fn the_known_tool_names_are_exactly_the_three_gated_tools() {
    // `is_known_tool_name` decides which auto_approve entries are warned about,
    // so leaving send_message out would make a correct playbook look mistyped.
    for name in ["interrupt_window", "close_window", "send_message"] {
        assert!(
            DestructiveTool::is_known_tool_name(name),
            "{name} must be recognised as a tool name"
        );
    }
    for name in ["teleport_window", "send_text", "SEND_MESSAGE", ""] {
        assert!(
            !DestructiveTool::is_known_tool_name(name),
            "{name:?} must not be recognised as a tool name"
        );
    }
}

#[test]
fn parse_policy_accepts_send_message_in_auto_approve() {
    let policy = parse_policy("[approval]\nauto_approve = [\"send_message\"]\n")
        .expect("send_message must be a valid auto_approve entry");
    assert!(policy.allows(DestructiveTool::SendMessage));
    assert!(!policy.allows(DestructiveTool::CloseWindow));
}

// =============================================================================
// [limits] approval_timeout_secs
// =============================================================================

#[test]
fn approval_timeout_secs_accepts_the_whole_permitted_range() {
    for seconds in [1u64, 60, 599, 600] {
        let source = format!("[limits]\napproval_timeout_secs = {seconds}\n");
        let policy = parse_policy(&source)
            .unwrap_or_else(|err| panic!("{seconds} is inside 1..=600 and must parse: {err}"));
        assert_eq!(
            policy
                .limits()
                .expect("[limits] is present")
                .approval_timeout_secs,
            Some(seconds)
        );
    }
}

#[test]
fn approval_timeout_secs_rejects_a_value_outside_the_permitted_range() {
    // Validated while deserialising, so an out-of-range value fails the whole
    // load like a value of the wrong type: the gate then sees a broken playbook
    // rather than a silently clamped deadline.
    for seconds in [0u64, 601, 3600] {
        let source = format!("[limits]\napproval_timeout_secs = {seconds}\n");
        let result = parse_policy(&source);
        assert!(
            matches!(result, Err(PolicyError::Parse { .. })),
            "{seconds} is outside 1..=600 and must fail the load rather than be \
             clamped, got {result:?}"
        );
    }
}

#[test]
fn approval_timeout_secs_rejects_a_value_of_the_wrong_type() {
    let result = parse_policy("[limits]\napproval_timeout_secs = \"60\"\n");
    assert!(
        matches!(result, Err(PolicyError::Parse { .. })),
        "a quoted number must fail the load rather than be coerced, got {result:?}"
    );
}

#[test]
fn approval_timeout_secs_is_none_when_omitted() {
    let policy = parse_policy("[limits]\ninterrupt_bytes = [3]\n")
        .expect("a [limits] section without approval_timeout_secs must parse");
    assert_eq!(
        policy
            .limits()
            .expect("[limits] is present")
            .approval_timeout_secs,
        None,
        "an omitted approval_timeout_secs must stay None so apply_limits keeps \
         the built-in default"
    );
}

#[test]
fn a_playbook_can_select_the_approval_timeout() {
    // End to end through the real configuration path: TOML -> parse_policy ->
    // limits() -> apply_limits -> DaemonConfig.
    let policy = parse_policy("[limits]\napproval_timeout_secs = 120\n")
        .expect("a [limits] approval_timeout_secs must parse");
    let mut config = DaemonConfig::default();
    apply_limits(&mut config, policy.limits());
    assert_eq!(
        config.approval_timeout_secs, 120,
        "a deadline set in the playbook must reach the daemon configuration"
    );
}

#[test]
fn daemon_config_defaults_to_the_built_in_approval_timeout() {
    assert_eq!(
        DaemonConfig::default().approval_timeout_secs,
        APPROVAL_TIMEOUT_DEFAULT_SECS,
        "the default must come from the one constant, not from a literal \
         repeated at the call site"
    );
    assert_eq!(
        APPROVAL_TIMEOUT_DEFAULT_SECS, 60,
        "the built-in deadline is 60 seconds"
    );
}

// =============================================================================
// [chat] and [limits] chat_delivery_max (documented in the bundled playbook.toml)
// =============================================================================

/// A `[chat]` section with every key omitted, so a test overrides only the
/// one field it cares about via struct-update syntax.
fn empty_chat_section() -> ChatSection {
    ChatSection {
        repetition_count: None,
        repetition_window: None,
        repetition_distance: None,
        monologue_max: None,
        turn_message_max: None,
        silence_timeout_secs: None,
        history_max: None,
        intro: None,
        exclude_detections: None,
    }
}

#[test]
fn apply_chat_section_writes_every_field_when_all_are_specified() {
    let mut config = DaemonConfig::default();
    let section = ChatSection {
        repetition_count: Some(10),
        repetition_window: Some(20),
        repetition_distance: Some(8),
        monologue_max: Some(5),
        turn_message_max: Some(15),
        silence_timeout_secs: Some(120),
        history_max: Some(1000),
        intro: Some("welcome to the room".to_string()),
        exclude_detections: Some(vec!["generic-shell".to_string(), "codex-cli".to_string()]),
    };

    apply_chat_section(&mut config, Some(&section));

    assert_eq!(config.chat_limits.repetition_count, 10);
    assert_eq!(config.chat_limits.repetition_window, 20);
    assert_eq!(config.chat_limits.repetition_distance, 8);
    assert_eq!(config.chat_limits.monologue_max, 5);
    assert_eq!(config.chat_limits.turn_message_max, 15);
    assert_eq!(config.chat_limits.silence_timeout_secs, 120);
    assert_eq!(config.chat_limits.history_max, 1000);
    assert_eq!(config.chat_intro, "welcome to the room");
    assert_eq!(
        config.chat_exclude_detections,
        vec!["generic-shell".to_string(), "codex-cli".to_string()]
    );
}

#[test]
fn apply_chat_section_keeps_the_built_in_defaults_when_no_section_is_given() {
    let mut config = DaemonConfig::default();
    let before_intro = config.chat_intro.clone();
    let before_exclude = config.chat_exclude_detections.clone();

    apply_chat_section(&mut config, None);

    let defaults = ChatLimits::default();
    assert_eq!(
        config.chat_limits.repetition_count,
        defaults.repetition_count
    );
    assert_eq!(
        config.chat_limits.repetition_window,
        defaults.repetition_window
    );
    assert_eq!(
        config.chat_limits.repetition_distance,
        defaults.repetition_distance
    );
    assert_eq!(config.chat_limits.monologue_max, defaults.monologue_max);
    assert_eq!(
        config.chat_limits.turn_message_max,
        defaults.turn_message_max
    );
    assert_eq!(
        config.chat_limits.silence_timeout_secs,
        defaults.silence_timeout_secs
    );
    assert_eq!(config.chat_limits.history_max, defaults.history_max);
    assert_eq!(config.chat_intro, before_intro);
    assert_eq!(config.chat_exclude_detections, before_exclude);
}

#[test]
fn apply_chat_section_overrides_only_the_specified_field() {
    let mut config = DaemonConfig::default();
    let before_intro = config.chat_intro.clone();
    let section = ChatSection {
        repetition_count: Some(99),
        ..empty_chat_section()
    };

    apply_chat_section(&mut config, Some(&section));

    let defaults = ChatLimits::default();
    assert_eq!(config.chat_limits.repetition_count, 99);
    assert_eq!(
        config.chat_limits.repetition_window,
        defaults.repetition_window
    );
    assert_eq!(
        config.chat_limits.repetition_distance,
        defaults.repetition_distance
    );
    assert_eq!(config.chat_limits.monologue_max, defaults.monologue_max);
    assert_eq!(
        config.chat_limits.turn_message_max,
        defaults.turn_message_max
    );
    assert_eq!(
        config.chat_limits.silence_timeout_secs,
        defaults.silence_timeout_secs
    );
    assert_eq!(config.chat_limits.history_max, defaults.history_max);
    assert_eq!(
        config.chat_intro, before_intro,
        "a section that specifies only repetition_count must leave intro untouched"
    );
}

#[test]
fn apply_chat_section_empty_intro_disables_intro_injection() {
    // The built-in [chat] intro default is non-empty; the
    // synthetic playbook builder every other chat test uses instead sends an
    // explicit empty string so intro delivery never fires by accident.
    let mut config = DaemonConfig::default();
    let section = ChatSection {
        intro: Some(String::new()),
        ..empty_chat_section()
    };

    apply_chat_section(&mut config, Some(&section));

    assert_eq!(config.chat_intro, "");
}

#[test]
fn daemon_config_defaults_to_a_non_empty_built_in_chat_intro() {
    assert!(
        !DaemonConfig::default().chat_intro.is_empty(),
        "the built-in default must be non-empty text so a playbook that omits \
         [chat] entirely still greets participants"
    );
}

// The built-in intro text explains the room concept -- how to
// reach every window ("main") and how to answer in the room a message
// arrived in (the envelope's reply.room_id) -- rather than assuming a
// single shared room. The exact wording is not frozen (module precedent:
// only non-empty is pinned above), so this checks the two facts a newly
// opened participant needs, by keyword.
#[test]
fn daemon_config_default_chat_intro_explains_main_and_the_reply_room_id() {
    let intro = DaemonConfig::default().chat_intro;
    assert!(
        intro.contains("main"),
        "the default intro must name \"main\" as the cross-window room, got: {intro:?}"
    );
    assert!(
        intro.contains("room_id"),
        "the default intro must point participants at the envelope's reply.room_id for \
         answering in the same room, got: {intro:?}"
    );
}

#[test]
fn daemon_config_defaults_to_excluding_generic_shell_from_chat() {
    assert_eq!(
        DaemonConfig::default().chat_exclude_detections,
        vec!["generic-shell".to_string()],
        "the built-in default exclude list is exactly the one entry documented in \
         the bundled playbook.toml"
    );
}

#[test]
fn parse_policy_accepts_a_playbook_without_a_chat_section() {
    let policy = parse_policy("[approval]\nauto_approve = [\"send_chat\"]\n")
        .expect("a playbook without [chat] must still parse");
    assert!(
        policy.chat.is_none(),
        "an omitted [chat] table must parse to None, not a defaulted section"
    );
}

#[test]
fn parse_policy_reads_every_chat_key() {
    let source = "[chat]\nrepetition_count = 6\nrepetition_window = 10\n\
                  repetition_distance = 4\nmonologue_max = 2\nturn_message_max = 8\n\
                  silence_timeout_secs = 60\nhistory_max = 200\nintro = \"\"\n\
                  exclude_detections = [\"generic-shell\"]\n";
    let policy = parse_policy(source).expect("a well-formed [chat] table must parse");
    let chat = policy.chat.expect("[chat] is present");
    assert_eq!(chat.repetition_count, Some(6));
    assert_eq!(chat.repetition_window, Some(10));
    assert_eq!(chat.repetition_distance, Some(4));
    assert_eq!(chat.monologue_max, Some(2));
    assert_eq!(chat.turn_message_max, Some(8));
    assert_eq!(chat.silence_timeout_secs, Some(60));
    assert_eq!(chat.history_max, Some(200));
    assert_eq!(chat.intro, Some(String::new()));
    assert_eq!(
        chat.exclude_detections,
        Some(vec!["generic-shell".to_string()])
    );
}

#[test]
fn parse_policy_rejects_an_unknown_key_inside_chat() {
    let result = parse_policy("[chat]\nbogus_key = 1\n");
    assert!(
        matches!(result, Err(PolicyError::Parse { .. })),
        "an unknown [chat] key must fail the whole load (deny_unknown_fields), got \
         {result:?}"
    );
}

#[test]
fn parse_policy_rejects_a_chat_value_outside_its_range() {
    // repetition_distance's range (0..=64, room::ChatLimits::validate) is
    // enforced through parse_policy rather than re-implemented here: this
    // test only proves the wiring, not the boundary numbers themselves (those
    // are room's own tests, see test-room).
    let result = parse_policy("[chat]\nrepetition_distance = 65\n");
    assert!(
        matches!(result, Err(PolicyError::Parse { .. })),
        "a [chat] value outside room::ChatLimits::validate's range must fail the \
         whole load, got {result:?}"
    );
}

#[test]
fn parse_policy_rejects_a_chat_delivery_max_outside_its_range() {
    for value in [0u64, 257] {
        let source = format!("[limits]\nchat_delivery_max = {value}\n");
        let result = parse_policy(&source);
        assert!(
            matches!(result, Err(PolicyError::Parse { .. })),
            "{value} is outside 1..=256 and must fail the whole load, got {result:?}"
        );
    }
}

#[test]
fn parse_policy_accepts_the_whole_chat_delivery_max_range() {
    for value in [1u64, 32, 256] {
        let source = format!("[limits]\nchat_delivery_max = {value}\n");
        let policy = parse_policy(&source)
            .unwrap_or_else(|err| panic!("{value} is inside 1..=256 and must parse: {err}"));
        assert_eq!(
            policy
                .limits()
                .expect("[limits] is present")
                .chat_delivery_max,
            Some(value)
        );
    }
}

#[test]
fn chat_delivery_max_is_none_when_omitted_from_the_limits_section() {
    let policy = parse_policy("[limits]\ninterrupt_bytes = [3]\n")
        .expect("a [limits] section without chat_delivery_max must parse");
    assert_eq!(
        policy
            .limits()
            .expect("[limits] is present")
            .chat_delivery_max,
        None,
        "an omitted chat_delivery_max must stay None so apply_limits keeps the \
         built-in default"
    );
}

#[test]
fn daemon_config_defaults_to_a_chat_delivery_max_of_32() {
    assert_eq!(
        DaemonConfig::default().chat_delivery_max,
        32,
        "the default documented in the bundled playbook.toml"
    );
}
